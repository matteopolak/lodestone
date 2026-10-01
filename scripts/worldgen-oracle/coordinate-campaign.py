#!/usr/bin/env python3
"""Bounded coordinate search over the existing production stream comparator."""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
FORMAT_VERSION = 1
GENERATOR_VERSION = 1
MAX_JSON_BYTES = 64 * 1024
MAX_LOG_BYTES = 64 * 1024
ORIGINS = ((-1, -1), (0, 0), (-33, 31), (31, -33), (32, 32),
           (-32, -32), (1, -1), (-1, 1))
SIGNATURE_FIELDS = ("target", "component", "cell", "expected", "actual")


class CampaignError(Exception):
    pass


def integer(value):
    try:
        return int(value, 16 if value.lower().startswith("0x") else 10)
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected decimal or 0x integer") from error


def mix(value):
    mask = (1 << 64) - 1
    value = (value + 0x9E3779B97F4A7C15) & mask
    value = ((value ^ (value >> 30)) * 0xBF58476D1CE4E5B9) & mask
    value = ((value ^ (value >> 27)) * 0x94D049BB133111EB) & mask
    return value ^ (value >> 31)


def rectangle(seed, index):
    x, z = ORIGINS[(index + seed % len(ORIGINS)) % len(ORIGINS)]
    bits = mix(seed ^ index)
    return [x, x + (bits & 1), z, z + ((bits >> 1) & 1)]


def validate_rectangle(rect):
    if not isinstance(rect, list) or len(rect) != 4 or any(type(x) is not int for x in rect):
        raise CampaignError("rectangle must contain four integers")
    x0, x1, z0, z1 = rect
    if not (-33 <= x0 <= x1 <= 33 and -33 <= z0 <= z1 <= 33
            and x1 - x0 <= 1 and z1 - z0 <= 1):
        raise CampaignError("rectangle is outside the bounded coordinate domain")


def signature(report):
    divergence = report.get("divergence")
    if not isinstance(divergence, dict) or any(key not in divergence for key in SIGNATURE_FIELDS):
        raise CampaignError("missing first-divergence signature")
    return {key: divergence[key] for key in SIGNATURE_FIELDS}


def validate_report(report, rect):
    validate_rectangle(rect)
    if not isinstance(report, dict) or (report.get("format_version"), report.get("protocol"),
            report.get("world_seed"), report.get("stream_format"), report.get("rectangle")) != (1, 776, 42, 7, rect):
        raise CampaignError("comparator report provenance differs from the requested case")
    for key in ("header_sha256", "oracle_jar_sha256", "oracle_source_sha256"):
        digest = report.get(key)
        if not isinstance(digest, str) or len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
            raise CampaignError("comparator report has no complete oracle fingerprints")
    count = (rect[1] - rect[0] + 1) * (rect[3] - rect[2] + 1)
    if report.get("status") == "agreed":
        if report.get("compared") != count or report.get("divergence") is not None:
            raise CampaignError("agreement report did not compare the whole rectangle")
    elif report.get("status") == "diverged":
        sig = signature(report)
        frame = report["divergence"].get("frame")
        if type(frame) is not int or not 0 <= frame < count or report.get("compared") != frame + 1:
            raise CampaignError("invalid first frame in comparator report")
        target = [rect[0] + frame % (rect[1] - rect[0] + 1), rect[2] + frame // (rect[1] - rect[0] + 1)]
        if sig["target"] != target or sig["component"] not in ("terrain", "biome", "heightmap", "block-entity", "packet"):
            raise CampaignError("invalid first target or component")
        if not isinstance(sig["cell"], dict) or not all(isinstance(sig[key], str) and sig[key] for key in ("expected", "actual")):
            raise CampaignError("invalid first cell or identities")
        cell = sig["cell"]
        kind = cell.get("kind")
        if kind == "record-byte":
            valid_cell = type(cell.get("offset")) is int and 0 <= cell["offset"] <= 16 * 1024 * 1024
        elif kind in ("block", "biome-quart", "heightmap"):
            width = 4 if kind == "biome-quart" else 16
            valid_cell = all(type(cell.get(axis)) is int and -1 <= cell[axis] < width for axis in ("x", "z"))
            if kind == "heightmap":
                valid_cell = valid_cell and type(cell.get("map")) is int and 0 <= cell["map"] <= 0xFFFFFFFF
            else:
                valid_cell = valid_cell and type(cell.get("y")) is int and -64 <= cell["y"] < 320
        else:
            valid_cell = False
        if not valid_cell:
            raise CampaignError("first cell is outside its typed coordinate domain")
    else:
        raise CampaignError("unknown comparator outcome")
    return report


def read_json(path):
    with path.open("rb") as source:
        data = source.read(MAX_JSON_BYTES + 1)
    if len(data) > MAX_JSON_BYTES:
        raise CampaignError("JSON artifact exceeds 64 KiB")
    return json.loads(data)


def atomic_json(path, value):
    data = (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()
    if len(data) > MAX_JSON_BYTES:
        raise CampaignError("JSON artifact exceeds 64 KiB")
    pending = path.with_suffix(".pending")
    with pending.open("wb") as output:
        output.write(data)
        output.flush()
        os.fsync(output.fileno())
    os.replace(pending, path)
    descriptor = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def stop_group(process):
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=2)


def bounded_process(command, env, deadline):
    tail = bytearray()
    process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, start_new_session=True)
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise CampaignError("oracle/comparator subprocess deadline exceeded")
                for key, _ in selector.select(min(remaining, 0.2)):
                    chunk = os.read(key.fileobj.fileno(), 8192)
                    if not chunk:
                        selector.unregister(key.fileobj)
                    else:
                        tail.extend(chunk)
                        del tail[:-MAX_LOG_BYTES]
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise CampaignError("oracle/comparator subprocess deadline exceeded")
            return process.wait(timeout=remaining), bytes(tail)
    finally:
        stop_group(process)
        process.stdout.close()


def evaluate(rect, config, campaign_deadline):
    validate_rectangle(rect)
    if config["oracle_jar_sha256"] is None:
        raise CampaignError("26.2 oracle cache has no server binary")
    deadline = min(campaign_deadline, time.monotonic() + config["subprocess_seconds"])
    if deadline <= time.monotonic():
        raise CampaignError("total campaign deadline exceeded")
    with tempfile.TemporaryDirectory(prefix="lodestone-coordinate-") as temporary:
        report_path = Path(temporary) / "report.json"
        env = {key: value for key, value in os.environ.items() if not key.startswith("LODESTONE_")}
        env.update(LODESTONE_COORDINATE_REPORT=str(report_path),
                   LODESTONE_MC_CACHE=str(ROOT / ".cache/mc/26.2"),
                   LODESTONE_LARGE_PARITY_STREAM_BATCH_SIZE="1")
        command = ["bash", str(ROOT / "scripts/worldgen-oracle/stream-parity.sh"),
                   "--dimension", "overworld", "--cx", str(rect[0]), str(rect[1]),
                   "--cz", str(rect[2]), str(rect[3])]
        status, log = bounded_process(command, env, deadline)
        if not report_path.is_file():
            raise CampaignError(f"oracle/comparator failed without a report (exit {status}): "
                                + log[-2048:].decode(errors="replace"))
        report = validate_report(read_json(report_path), rect)
        if report["oracle_jar_sha256"] != config["oracle_jar_sha256"] or report["oracle_source_sha256"] != config["source_sha256"]["scripts/worldgen-oracle/LargeParityOracle.java"]:
            raise CampaignError("observed oracle binary or source differs from campaign provenance")
        if (status == 0) != (report["status"] == "agreed"):
            raise CampaignError("subprocess exit and comparator report disagree")
        return report


def shrink_rectangles(rect, target):
    x0, x1, z0, z1 = rect
    x, z = target
    candidates = ([x, x, z0, z1], [x0, x1, z, z], [x, x, z, z])
    return [candidate for index, candidate in enumerate(candidates)
            if candidate != rect and candidate not in candidates[:index]]


def settings(args):
    for value, maximum, name in ((args.cases, 8, "cases"), (args.run_cases, 8, "run cases"),
            (args.shrink_attempts, 8, "shrink attempts"), (args.subprocess_seconds, 600, "subprocess seconds"),
            (args.total_seconds, 3600, "total seconds")):
        if not 1 <= value <= maximum:
            raise CampaignError(f"{name} must be in 1..{maximum}")
    if not 0 <= args.seed < 1 << 64:
        raise CampaignError("generator seed must be an unsigned 64-bit integer")
    files = ("scripts/worldgen-oracle/coordinate-campaign.py", "scripts/worldgen-oracle/stream-parity.sh",
             "scripts/worldgen-oracle/run.sh", "scripts/worldgen-oracle/LargeParityOracle.java",
             "crates/versions/26.2/tests/streaming_worldgen_parity.rs",
             "crates/versions/26.2/tests/support/large_parity_manifest.rs")
    jar = ROOT / ".cache/mc/26.2/versions/26.2/server-26.2.jar"
    jar_digest = hashlib.sha256()
    if jar.is_file():
        with jar.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                jar_digest.update(chunk)
    return {"format_version": FORMAT_VERSION, "generator_version": GENERATOR_VERSION,
            "generator_seed": args.seed, "world_seed": 42, "protocol": 776,
            "dimension": "overworld", "stream_format": 7,
            "stream_domain": "lodestone.worldgen.streaming-parity/v2/light-free",
            "record_domain": "lodestone.worldgen.large-parity.chunk/v7/light-free",
            "order": "z-major/x-fastest", "batch_size": 1, "defer_heightmaps": False,
            "origins": [list(origin) for origin in ORIGINS], "max_rectangle": [2, 2],
            "cases": args.cases, "shrink_attempts": args.shrink_attempts,
            "subprocess_seconds": args.subprocess_seconds, "total_seconds": args.total_seconds,
            "oracle_jar_sha256": jar_digest.hexdigest() if jar.is_file() else None,
            "source_sha256": {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest() for name in files}}


def validate_replay(replay, config):
    if not isinstance(replay, dict) or replay.get("config") != config:
        raise CampaignError("replay configuration or source fingerprint changed")
    validate_rectangle(replay["rectangle"])
    validate_report(replay["report"], replay["rectangle"])
    if replay["report"]["status"] != "diverged":
        raise CampaignError("replay must contain a divergence")
    index = replay.get("case_index")
    if type(index) is not int or not 0 <= index < config["cases"]:
        raise CampaignError("replay case index outside campaign")
    original = rectangle(config["generator_seed"], index)
    if replay.get("original_rectangle") != original:
        raise CampaignError("replay original rectangle differs from generator")
    rect = replay["rectangle"]
    if not (original[0] <= rect[0] <= rect[1] <= original[1]
            and original[2] <= rect[2] <= rect[3] <= original[3]):
        raise CampaignError("replay rectangle is not a reduction of its generated case")
    tried = replay.get("tried", [])
    if not isinstance(tried, list) or len(tried) > config["shrink_attempts"]:
        raise CampaignError("replay shrink history exceeds its budget")
    for candidate in tried:
        validate_rectangle(candidate)
        if not (original[0] <= candidate[0] <= candidate[1] <= original[1]
                and original[2] <= candidate[2] <= candidate[3] <= original[3]):
            raise CampaignError("shrink history leaves the generated rectangle")


def campaign(args, config, evaluator=evaluate):
    deadline = time.monotonic() + config["total_seconds"]
    if args.replay:
        replay = read_json(args.replay)
        validate_replay(replay, config)
        observed = evaluator(replay["rectangle"], config, deadline)
        if observed["status"] != "diverged" or signature(observed) != signature(replay["report"]):
            raise CampaignError("replay did not preserve the recorded divergence")
        print("replay confirmed")
        return 0
    args.output.mkdir(parents=True, exist_ok=True)
    with (args.output / "campaign.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise CampaignError("output campaign is already running") from error
        checkpoint_path = args.output / "checkpoint.json"
        if args.resume:
            state = read_json(checkpoint_path)
            if state.get("config") != config:
                raise CampaignError("checkpoint configuration or source fingerprint changed")
        else:
            if any(path.name != "campaign.lock" for path in args.output.iterdir()):
                raise CampaignError("new campaign output must be empty")
            state = {"config": config, "next_case": 0, "status": "ready", "finding": None,
                     "evaluations": {"generated": 0, "shrink": 0, "replay": 0}, "last_failure": None}
            atomic_json(checkpoint_path, state)
        if type(state.get("next_case")) is not int or not 0 <= state["next_case"] <= config["cases"]:
            raise CampaignError("checkpoint case index outside campaign")
        if state.get("status") not in ("ready", "complete", "confirmed"):
            raise CampaignError("unknown checkpoint status")
        counters = state.get("evaluations")
        if not isinstance(counters, dict) or set(counters) != {"generated", "shrink", "replay"} or any(type(value) is not int or value < 0 for value in counters.values()):
            raise CampaignError("invalid checkpoint evaluation counters")
        if state["finding"] is not None:
            validate_replay(state["finding"], config)
        if state["status"] == "confirmed":
            if state["finding"] is None:
                raise CampaignError("confirmed checkpoint has no finding")
            return 1
        phase = "generated"
        try:
            stop = min(config["cases"], state["next_case"] + args.run_cases)
            while state["finding"] is not None or state["next_case"] < stop:
                finding = state["finding"]
                if finding is None:
                    phase = "generated"
                    rect = rectangle(config["generator_seed"], state["next_case"])
                    report = validate_report(evaluator(rect, config, deadline), rect)
                    state["evaluations"][phase] += 1
                    if report["status"] == "agreed":
                        state["next_case"] += 1
                    else:
                        state["finding"] = {"config": config, "case_index": state["next_case"],
                                            "original_rectangle": rect, "rectangle": rect,
                                            "report": report, "tried": []}
                    state["last_failure"] = None
                    atomic_json(checkpoint_path, state)
                    continue
                validate_replay(finding, config)
                phase = "shrink"
                candidates = [rect for rect in shrink_rectangles(finding["rectangle"], signature(finding["report"])["target"])
                              if rect not in finding["tried"]]
                if candidates and len(finding["tried"]) < config["shrink_attempts"]:
                    rect = candidates[0]
                    report = validate_report(evaluator(rect, config, deadline), rect)
                    state["evaluations"][phase] += 1
                    finding["tried"].append(rect)
                    if report["status"] == "diverged" and signature(report) == signature(finding["report"]):
                        finding["rectangle"], finding["report"] = rect, report
                    state["last_failure"] = None
                    atomic_json(checkpoint_path, state)
                    continue
                phase = "replay"
                atomic_json(args.output / "replay.json", finding)
                report = validate_report(evaluator(finding["rectangle"], config, deadline), finding["rectangle"])
                if report["status"] != "diverged" or signature(report) != signature(finding["report"]):
                    raise CampaignError("fresh replay did not preserve the minimized divergence")
                state["evaluations"][phase] += 1
                state["status"] = "confirmed"
                state["next_case"] += 1
                state["last_failure"] = None
                atomic_json(checkpoint_path, state)
                print(json.dumps({"status": "confirmed", "rectangle": finding["rectangle"],
                                  "divergence": finding["report"]["divergence"]}, sort_keys=True))
                return 1
            state["status"] = "complete" if state["next_case"] == config["cases"] else "ready"
            atomic_json(checkpoint_path, state)
            print(json.dumps({"status": state["status"], "next_case": state["next_case"]}))
            return 0
        except CampaignError as error:
            state["last_failure"] = {"phase": phase, "message": str(error)[-4096:]}
            atomic_json(checkpoint_path, state)
            raise


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    destination = result.add_mutually_exclusive_group(required=True)
    destination.add_argument("--output", type=Path)
    destination.add_argument("--replay", type=Path)
    result.add_argument("--resume", action="store_true")
    result.add_argument("--seed", type=integer, default=0x26C00D)
    result.add_argument("--cases", type=integer, default=8)
    result.add_argument("--run-cases", type=integer, default=8)
    result.add_argument("--shrink-attempts", type=integer, default=8)
    result.add_argument("--subprocess-seconds", type=integer, default=240)
    result.add_argument("--total-seconds", type=integer, default=1800)
    return result


def main():
    args = parser().parse_args()
    def interrupted(_signal, _frame):
        raise CampaignError("campaign interrupted")

    previous_handler = signal.signal(signal.SIGTERM, interrupted)
    try:
        if args.resume and args.replay:
            raise CampaignError("resume requires an output checkpoint")
        return campaign(args, settings(args))
    except (CampaignError, OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        print(f"coordinate campaign: {error}", file=sys.stderr)
        return 2
    finally:
        signal.signal(signal.SIGTERM, previous_handler)


if __name__ == "__main__":
    sys.exit(main())
