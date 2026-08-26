#!/usr/bin/env python3
"""Run and summarize deterministic Java-backed Lodestone frame benchmarks.

The runner stays in the foreground for its entire child process lifetime. It
uses only the Python standard library so a fresh checkout needs no virtualenv.
"""

from __future__ import annotations

import argparse
import contextlib
import csv
import json
import math
import os
import pathlib
import platform
import re
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from typing import Iterable, Mapping


ROOT = pathlib.Path(__file__).resolve().parents[1]
RESULTS = ROOT / "bench-results" / "live_frame_profile.jsonl"
SCENE = ROOT / "scripts" / "benchmark-scenes" / "showcase.txt"
RCON_PASSWORD = "lodestone"
RENDER_DISTANCE = 24
SERVER_VIEW_DISTANCE = RENDER_DISTANCE + 1
METADATA_COLUMNS = {"frame", "frame_interval_ms", "segment"}
COUNT_COLUMNS = {
    "world.packed_sections_visited",
    "world.model_sections_visited",
    "hud.chat_lines",
    "hud.debug_lines",
    "hud.menu_overlays_drawn",
}
GPU_SAMPLE_RE = re.compile(
    r"gpu: world_total=(?P<world_total>\d+(?:\.\d+)?)ms, "
    r"world=(?P<world>\d+(?:\.\d+)?)ms, "
    r"first_person=(?P<first_person>\d+(?:\.\d+)?)ms, "
    r"hud_total=(?P<hud_total>\d+(?:\.\d+)?)ms"
)

ORACLES = {
    "terrain": {
        "script": ROOT / "scripts" / "live-oracles" / "terrain.sh",
        "world": ROOT / ".cache" / "mc" / "terrain",
        "game_port": 25580,
        "rcon_port": 25581,
    },
    "showcase": {
        "script": ROOT / "scripts" / "live-oracles" / "creative.sh",
        "world": ROOT / ".cache" / "mc" / "creative",
        "game_port": 25570,
        "rcon_port": 25571,
    },
    "megaworld": {
        "script": ROOT / "scripts" / "live-oracles" / "megaworld.sh",
        "world": ROOT / ".cache" / "mc" / "megaworld",
        "game_port": 25590,
        "rcon_port": 25591,
    },
    "lovelier": {
        "script": ROOT / "scripts" / "live-oracles" / "lovelier.sh",
        "world": ROOT / ".cache" / "mc" / "lovelier",
        "game_port": 25600,
        "rcon_port": 25601,
    },
}


def nearest_rank(values: Iterable[float], quantile: float) -> float:
    """Return the observed nearest-rank percentile, never an interpolation."""
    ordered = sorted(values)
    if not ordered:
        raise ValueError("cannot take a percentile of no values")
    if not 0.0 < quantile <= 1.0:
        raise ValueError(f"quantile must be in (0, 1], got {quantile}")
    rank = max(1, math.ceil(quantile * len(ordered)))
    return ordered[rank - 1]


def _numbers(rows: Iterable[Mapping[str, str]], column: str) -> list[float]:
    values = []
    for row in rows:
        text = row.get(column, "").strip()
        if text:
            values.append(float(text))
    return values


def summarize_rows(rows: list[Mapping[str, str]]) -> dict:
    """Reduce raw CSV rows without treating skipped empty phases as zero."""
    intervals = _numbers(rows, "frame_interval_ms")
    if len(intervals) != len(rows):
        raise ValueError("every measured row must carry frame_interval_ms")
    phase_names = sorted(
        {
            name
            for row in rows
            for name in row
            if name not in METADATA_COLUMNS and name not in COUNT_COLUMNS
        }
    )
    phases = {}
    for name in phase_names:
        values = _numbers(rows, name)
        if values:
            phases[name] = statistics.fmean(values)
    count_summary = {}
    for name in sorted(COUNT_COLUMNS):
        values = _numbers(rows, name)
        if values:
            count_summary[name] = {
                "median": statistics.median(values),
                "p95": nearest_rank(values, 0.95),
                "max": max(values),
            }
    return {
        "frames": len(rows),
        "p50_ms": nearest_rank(intervals, 0.50),
        "p95_ms": nearest_rank(intervals, 0.95),
        "p99_ms": nearest_rank(intervals, 0.99),
        "mean_ms": statistics.fmean(intervals),
        "over_16_67": sum(ms > 16.67 for ms in intervals),
        "over_33_3": sum(ms > 33.3 for ms in intervals),
        "phases_ms": phases,
        "workload_counts": count_summary,
    }


def summarize_gpu_log(log_text: str) -> dict:
    """Summarize asynchronous one-second GPU timestamp snapshots.

    Pass values stay separate. In particular, the diagnostic total spans are
    not added to the real-pass readings or presented as bounds.
    """
    samples = [
        {name: float(value) for name, value in match.groupdict().items()}
        for match in GPU_SAMPLE_RE.finditer(log_text)
    ]
    summary = {"samples": len(samples)}
    for name in ("world_total", "world", "first_person", "hud_total"):
        values = [sample[name] for sample in samples]
        if values:
            summary[name] = {
                "median_ms": statistics.median(values),
                "p95_ms": nearest_rank(values, 0.95),
            }
    return summary


def validate_run(
    rows: list[Mapping[str, str]], log_text: str, workload: str
) -> tuple[int, int]:
    """Reject incomplete or mislabeled runs before they enter the history."""
    if "benchmark complete" not in log_text:
        raise ValueError("completion marker missing from client log")
    if "selected hardware built-in display for fullscreen benchmark" not in log_text:
        raise ValueError("client log does not confirm hardware built-in display selection")
    ready = re.search(
        r"benchmark window ready[^\n]*framebuffer_width=(\d+)"
        r"[^\n]*framebuffer_height=(\d+)[^\n]*fullscreen=(true|false)",
        log_text,
    )
    if ready is None:
        raise ValueError("client log does not contain parseable framebuffer/fullscreen metadata")
    if ready.group(3) != "true":
        raise ValueError("client log does not confirm fullscreen presentation")
    framebuffer = (int(ready.group(1)), int(ready.group(2)))
    if framebuffer[0] <= 0 or framebuffer[1] <= 0:
        raise ValueError(f"invalid physical framebuffer size: {framebuffer}")

    measured = {
        row.get("segment", "")
        for row in rows
        if row.get("segment", "").endswith((".stationary", ".moving"))
    }
    wrong = sorted(segment for segment in measured if not segment.startswith(f"{workload}."))
    if wrong:
        raise ValueError(f"workload metadata mismatch: expected {workload}, saw {wrong}")
    for suffix in ("stationary", "moving"):
        label = f"{workload}.{suffix}"
        if not any(row.get("segment") == label for row in rows):
            raise ValueError(f"{suffix} segment has no frame rows")
    return framebuffer


def _build_frame(request_id: int, packet_type: int, payload: str) -> bytes:
    body = (
        request_id.to_bytes(4, "little", signed=True)
        + packet_type.to_bytes(4, "little", signed=True)
        + payload.encode("utf-8")
        + b"\x00\x00"
    )
    return len(body).to_bytes(4, "little", signed=True) + body


def _recv_exact(sock: socket.socket, size: int) -> bytes:
    chunks = []
    remaining = size
    while remaining:
        chunk = sock.recv(remaining)
        if not chunk:
            raise ConnectionError("RCON connection closed before a full frame arrived")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def _read_response(sock: socket.socket) -> tuple[int, str]:
    length = int.from_bytes(_recv_exact(sock, 4), "little", signed=True)
    if length < 10:
        raise ValueError(f"short RCON response (length={length})")
    body = _recv_exact(sock, length)
    return (
        int.from_bytes(body[:4], "little", signed=True),
        body[8:-2].decode("utf-8", errors="replace"),
    )


class RconClient:
    """Small persistent Source-RCON client for one local oracle."""

    def __init__(self, port: int):
        self.port = port
        self.sock: socket.socket | None = None
        self.request_id = 10

    def __enter__(self) -> "RconClient":
        self.sock = socket.create_connection(("127.0.0.1", self.port), timeout=15)
        self.sock.settimeout(15)
        self.sock.sendall(_build_frame(1, 3, RCON_PASSWORD))
        response_id, _ = _read_response(self.sock)
        if response_id != 1:
            raise RuntimeError("RCON authentication failed")
        return self

    def __exit__(self, *_exc) -> None:
        if self.sock is not None:
            self.sock.close()
            self.sock = None

    def command(self, command: str) -> str:
        if self.sock is None:
            raise RuntimeError("RCON client is not connected")
        self.request_id += 1
        self.sock.sendall(_build_frame(self.request_id, 2, command))
        response_id, payload = _read_response(self.sock)
        if response_id != self.request_id:
            raise RuntimeError(
                f"RCON response id mismatch for {command!r}: got {response_id}"
            )
        return payload.strip()


@contextlib.contextmanager
def _server_view_distance(world: pathlib.Path, distance: int):
    """Raise Java's cap for this launch, then restore the persistent file."""
    properties = world / "server.properties"
    original = properties.read_text(encoding="utf-8")
    replacement, count = re.subn(
        r"(?m)^view-distance=.*$", f"view-distance={distance}", original
    )
    if count != 1:
        raise RuntimeError(f"{properties} must contain exactly one view-distance entry")
    properties.write_text(replacement, encoding="utf-8")
    try:
        yield
    finally:
        properties.write_text(original, encoding="utf-8")


def start_oracle(workload: str) -> dict:
    oracle = ORACLES[workload]
    print(
        f"starting {workload} Java oracle with server view distance "
        f"{SERVER_VIEW_DISTANCE}...",
        flush=True,
    )
    with _server_view_distance(oracle["world"], SERVER_VIEW_DISTANCE):
        subprocess.run([str(oracle["script"])], cwd=ROOT, check=True)
    with RconClient(oracle["rcon_port"]) as rcon:
        rcon.command("defaultgamemode creative")
        rcon.command("difficulty peaceful")
        rcon.command("gamerule advance_time false")
        rcon.command("time set noon")
    return oracle


def _is_scene_error(command: str, reply: str) -> bool:
    if command.startswith("kill @e[tag=lodestone_benchmark]") and "No entity" in reply:
        return False
    error_markers = (
        "Unknown or incomplete command",
        "Incorrect argument",
        "Expected ",
        "No entity was found",
        "That position is not loaded",
        "Cannot place",
        "Too many blocks",
        "Unable to summon",
        "Could not set the block",
    )
    return any(marker in reply for marker in error_markers)


def prepare_showcase(rcon_port: int) -> None:
    commands = []
    for raw in SCENE.read_text(encoding="utf-8").splitlines():
        command = raw.strip()
        if not command or command.startswith("#"):
            continue
        # These target the benchmark's unique player and are replayed by
        # `configure_joined_player` once that player exists. Command-block NBT
        # containing `@a` is intentionally not filtered.
        if command.startswith(("gamemode creative @a", "effect clear @a", "tp @a")):
            continue
        commands.append(command)
    print(f"installing {len(commands)} showcase RCON commands...", flush=True)
    with RconClient(rcon_port) as rcon:
        for index, command in enumerate(commands, 1):
            reply = rcon.command(command)
            if _is_scene_error(command, reply):
                raise RuntimeError(
                    f"showcase command {index}/{len(commands)} failed:\n"
                    f"  {command}\n  {reply or '(empty response)'}"
                )
    print("showcase scene accepted by Java", flush=True)


def joined_player_commands(workload: str, username: str) -> list[str]:
    commands = [
        f"gamemode creative {username}",
        f"effect clear {username}",
    ]
    if workload == "terrain":
        commands.append(f"tp {username} 0 140 0 0 10")
    elif workload == "showcase":
        commands.append(f"tp {username} 0 65 0 0 0")
    elif workload == "lovelier":
        commands.append(f"tp {username} 0 180 0 0 35")
    return commands


def configure_joined_player(workload: str, rcon_port: int, username: str) -> None:
    commands = joined_player_commands(workload, username)
    with RconClient(rcon_port) as rcon:
        for command in commands:
            reply = rcon.command(command)
            if "No player was found" in reply or "No entity was found" in reply:
                raise RuntimeError(f"post-join command failed: {command}: {reply}")


def process_rss_bytes(pid: int) -> int | None:
    result = subprocess.run(
        ["ps", "-o", "rss=", "-p", str(pid)],
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0 or not result.stdout.strip():
        return None
    return int(result.stdout.strip().splitlines()[0]) * 1024


def _read_csv(path: pathlib.Path) -> list[dict[str, str]]:
    with path.open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


def _client_command(
    binary: pathlib.Path,
    workload: str,
    game_port: int,
    durations: tuple[int, int, int],
    debug_overlay: str,
) -> list[str]:
    warmup, stationary, moving = durations
    return [
        str(binary),
        "--benchmark",
        workload,
        "--benchmark-warmup",
        str(warmup),
        "--benchmark-stationary",
        str(stationary),
        "--benchmark-moving",
        str(moving),
        "--benchmark-debug-overlay",
        debug_overlay,
        "--host",
        "127.0.0.1",
        "--port",
        str(game_port),
        "--protocol",
        "776",
        "--render-distance",
        str(RENDER_DISTANCE),
    ]


def _samply_command(client: list[str], artifact: pathlib.Path) -> list[str]:
    return [
        "samply",
        "record",
        "--save-only",
        "--unstable-presymbolicate",
        "--output",
        str(artifact),
        "--",
        *client,
    ]


def _unique_username(trial: int) -> str:
    suffix = int(time.time() * 1000) % 100_000
    return f"LdB{os.getpid():x}{trial}{suffix}"[:16]


def run_trial(
    workload: str,
    trial: int,
    binary: pathlib.Path,
    oracle: dict,
    durations: tuple[int, int, int],
    debug_overlay: str,
    samply_artifact: pathlib.Path | None = None,
) -> dict:
    with tempfile.TemporaryDirectory(prefix=f"lodestone-{workload}-bench-") as temp_name:
        temp = pathlib.Path(temp_name)
        csv_path = temp / "frames.csv"
        log_path = temp / "client.log"
        data_dir = temp / "data"
        data_dir.mkdir()
        username = _unique_username(trial)
        (data_dir / "offline.json").write_text(
            json.dumps({"username": username}), encoding="utf-8"
        )

        client = _client_command(
            binary, workload, oracle["game_port"], durations, debug_overlay
        )
        if samply_artifact is not None:
            command = _samply_command(client, samply_artifact)
        else:
            command = client
        env = os.environ.copy()
        env.update(
            {
                "LODESTONE_DATA_DIR": str(data_dir),
                "LODESTONE_FRAME_PROFILE_DUMP": str(csv_path),
                "RUST_LOG": "frame_profile=info,frame_benchmark=info,warn",
            }
        )

        total = sum(durations)
        deadline = time.monotonic() + total + 120
        rss_samples = []
        joined_configured = False
        timed_out = False
        print(
            f"trial {trial}: {workload}, debug_overlay={debug_overlay}, "
            f"user={username}, durations={durations}",
            flush=True,
        )
        with log_path.open("w", encoding="utf-8") as log_handle:
            process = subprocess.Popen(
                command,
                cwd=ROOT,
                env=env,
                stdout=log_handle,
                stderr=subprocess.STDOUT,
            )
            while process.poll() is None:
                rss = process_rss_bytes(process.pid)
                if rss is not None:
                    rss_samples.append(rss)
                if not joined_configured:
                    log_text = log_path.read_text(encoding="utf-8", errors="replace")
                    if f'segment="{workload}.warmup"' in log_text or f"segment={workload}.warmup" in log_text:
                        configure_joined_player(
                            workload, oracle["rcon_port"], username
                        )
                        joined_configured = True
                if time.monotonic() >= deadline:
                    timed_out = True
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                    break
                time.sleep(0.05 if not joined_configured else 0.25)
            return_code = process.wait()

        log_text = log_path.read_text(encoding="utf-8", errors="replace")
        if timed_out:
            raise RuntimeError(f"benchmark exceeded its {total + 120}s deadline; log: {log_path}")
        if return_code != 0:
            tail = "\n".join(log_text.splitlines()[-40:])
            raise RuntimeError(f"client exited {return_code}:\n{tail}")
        if not joined_configured:
            raise RuntimeError("client never reached a configurable joined warmup")
        if not csv_path.exists():
            raise RuntimeError("client exited without writing its frame CSV")

        rows = _read_csv(csv_path)
        framebuffer = validate_run(rows, log_text, workload)
        segments = {}
        for suffix in ("stationary", "moving"):
            label = f"{workload}.{suffix}"
            segments[suffix] = summarize_rows(
                [row for row in rows if row.get("segment") == label]
            )
        rss = {
            "start": rss_samples[0] if rss_samples else 0,
            "peak": max(rss_samples, default=0),
            "end": rss_samples[-1] if rss_samples else 0,
        }
        return {
            "trial": trial,
            "username": username,
            "debug_overlay": debug_overlay,
            "segments": segments,
            "rss_bytes": rss,
            "framebuffer": framebuffer,
            "gpu_timestamp_ms": summarize_gpu_log(log_text),
            "log": log_text,
        }


def _git_sha() -> str:
    return subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=True,
    ).stdout.strip()


def _print_trial(workload: str, result: dict) -> None:
    rss = result["rss_bytes"]
    print(
        f"trial {result['trial']} debug_overlay={result['debug_overlay']} "
        f"RSS MiB start/peak/end: "
        f"{rss['start']/1048576:.1f}/{rss['peak']/1048576:.1f}/{rss['end']/1048576:.1f}"
    )
    for suffix, summary in result["segments"].items():
        misses_60 = summary["over_16_67"]
        misses_30 = summary["over_33_3"]
        print(
            f"  {workload}.{suffix}: n={summary['frames']} "
            f"p50/p95/p99={summary['p50_ms']:.2f}/"
            f"{summary['p95_ms']:.2f}/{summary['p99_ms']:.2f} ms "
            f">16.67={misses_60} >33.3={misses_30}"
        )
        phase_line = " ".join(
            f"{name}={value:.2f}"
            for name, value in sorted(summary["phases_ms"].items())
        )
        print(f"    phase means ms: {phase_line}")
        if summary["workload_counts"]:
            count_line = " ".join(
                f"{name}=median/p95/max "
                f"{values['median']:.0f}/{values['p95']:.0f}/{values['max']:.0f}"
                for name, values in sorted(summary["workload_counts"].items())
            )
            print(f"    workload counts: {count_line}")
    gpu = result["gpu_timestamp_ms"]
    if gpu["samples"]:
        print(f"  GPU timestamp snapshots: n={gpu['samples']}")
        for name in ("world", "first_person", "world_total", "hud_total"):
            values = gpu[name]
            qualifier = " (diagnostic span)" if name.endswith("_total") else ""
            print(
                f"    {name}{qualifier}: median/p95="
                f"{values['median_ms']:.2f}/{values['p95_ms']:.2f} ms"
            )


def _append_records(
    workload: str,
    result: dict,
    durations: tuple[int, int, int],
    binary: pathlib.Path,
) -> None:
    RESULTS.parent.mkdir(parents=True, exist_ok=True)
    common = {
        "git_sha": _git_sha(),
        "machine": platform.platform(),
        "arch": platform.machine(),
        "profile": "release",
        "scene": workload,
        "debug_overlay": result["debug_overlay"],
        "trial": result["trial"],
        "binary": str(binary),
        "render_distance": RENDER_DISTANCE,
        "framebuffer": list(result["framebuffer"]),
        "durations_seconds": {
            "warmup": durations[0],
            "stationary": durations[1],
            "moving": durations[2],
        },
        "rss_bytes": result["rss_bytes"],
        "gpu_timestamp_ms": result["gpu_timestamp_ms"],
    }
    with RESULTS.open("a", encoding="utf-8") as handle:
        for suffix, summary in result["segments"].items():
            record = {**common, "segment": f"{workload}.{suffix}", **summary}
            handle.write(json.dumps(record, separators=(",", ":"), sort_keys=True) + "\n")


def _print_spread(workload: str, debug_overlay: str, results: list[dict]) -> None:
    if len(results) < 2:
        return
    print(f"trial spread (p50 frame interval, debug_overlay={debug_overlay}):")
    for suffix in ("stationary", "moving"):
        values = [result["segments"][suffix]["p50_ms"] for result in results]
        print(
            f"  {workload}.{suffix}: min/median/max="
            f"{min(values):.2f}/{statistics.median(values):.2f}/{max(values):.2f} ms"
        )


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workload", choices=sorted(ORACLES), required=True)
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--smoke", action="store_true")
    parser.add_argument("--samply", action="store_true")
    parser.add_argument("--debug-overlay", choices=("closed", "open", "both"))
    parser.add_argument(
        "--binary", type=pathlib.Path, default=ROOT / "target" / "release" / "lodestone"
    )
    args = parser.parse_args(argv)
    if args.trials < 1:
        parser.error("--trials must be at least 1")
    return args


def overlay_arms(workload: str, requested: str | None) -> list[str]:
    policy = requested or ("both" if workload == "megaworld" else "closed")
    return ["closed", "open"] if policy == "both" else [policy]


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    binary = args.binary.resolve()
    if not binary.is_file():
        raise SystemExit(f"release client not found: {binary}; build it before benchmarking")
    if args.samply and shutil.which("samply") is None:
        raise SystemExit("--samply requested but samply is not on PATH")

    durations = (2, 2, 3) if args.smoke else (
        (45, 30, 60) if args.workload in ("megaworld", "lovelier") else (20, 30, 60)
    )
    trial_count = 1 if args.smoke or args.samply else args.trials
    arms = overlay_arms(args.workload, args.debug_overlay)
    if args.samply and len(arms) != 1:
        raise SystemExit(
            "--samply with megaworld requires --debug-overlay closed or open"
        )
    oracle = start_oracle(args.workload)
    if args.workload == "showcase":
        prepare_showcase(oracle["rcon_port"])

    profile_artifact = None
    if args.samply:
        profiles = ROOT / "bench-results" / "profiles"
        profiles.mkdir(parents=True, exist_ok=True)
        stamp = time.strftime("%Y%m%d-%H%M%S")
        profile_artifact = profiles / f"{args.workload}-{arms[0]}-{stamp}.json.gz"

    for debug_overlay in arms:
        results = []
        for trial in range(1, trial_count + 1):
            result = run_trial(
                args.workload,
                trial,
                binary,
                oracle,
                durations,
                debug_overlay,
                samply_artifact=profile_artifact,
            )
            _print_trial(args.workload, result)
            if not args.samply:
                _append_records(args.workload, result, durations, binary)
                results.append(result)
        _print_spread(args.workload, debug_overlay, results)
    if profile_artifact is not None:
        print(f"samply profile: {profile_artifact}")
    else:
        print(f"appended comparable records to {RESULTS}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as exc:
        print(f"benchmark failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
