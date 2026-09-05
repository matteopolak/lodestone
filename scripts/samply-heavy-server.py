#!/usr/bin/env python3
"""Capture a bounded Samply profile of the real heavyweight server harness.

The client benchmark has a separate workflow.  This runner deliberately stays
headless: it emits the immutable heavy-scene plan, then profiles the release
``heavy-scene-server`` example driving an ``IntegratedServer`` over its real
join protocol.  It is a finite local investigation, never a timing gate.

Run after building the release example::

    python3 scripts/samply-heavy-server.py

The default entity scene has exactly 1,024 live entities.  The runner accepts
at most scale 2 (2,048 entities, matching the harness's own runtime cap) and
uses a wall deadline plus a separate profiler-process timeout.  A successful
run prints the capture, Samply symbol sidecar, scene handoff, and JSONL record.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
from dataclasses import dataclass


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_SERVER = ROOT / "target/release/examples/heavy-scene-server"
DEFAULT_OUTPUT = ROOT / "bench-results/profiles"
ENTITY_COUNT_PER_SCALE = 1_024
MAX_ENTITY_SCALE = 2
MAX_WALL_DEADLINE_SECS = 60
SMOKE_WALL_DEADLINE_SECS = 15
PROFILER_GRACE_SECS = 15


@dataclass(frozen=True)
class CapturePaths:
    capture: Path
    symbols: Path
    scene: Path
    runtime: Path


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, default=DEFAULT_SERVER)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--scale", type=int, default=1)
    parser.add_argument("--wall-deadline-secs", type=int, default=30)
    parser.add_argument(
        "--smoke",
        action="store_true",
        help=f"limit the server deadline to {SMOKE_WALL_DEADLINE_SECS} seconds",
    )
    parser.add_argument(
        "--run-id",
        help="stable artifact suffix for automation; defaults to the current UTC timestamp",
    )
    args = parser.parse_args(argv)
    if not 1 <= args.scale <= MAX_ENTITY_SCALE:
        parser.error(
            f"--scale must be 1..{MAX_ENTITY_SCALE}; this runner caps the live population at "
            f"{ENTITY_COUNT_PER_SCALE * MAX_ENTITY_SCALE}"
        )
    if not 1 <= args.wall_deadline_secs <= MAX_WALL_DEADLINE_SECS:
        parser.error(f"--wall-deadline-secs must be 1..{MAX_WALL_DEADLINE_SECS}")
    if args.smoke and args.wall_deadline_secs > SMOKE_WALL_DEADLINE_SECS:
        parser.error(
            f"--smoke requires --wall-deadline-secs <= {SMOKE_WALL_DEADLINE_SECS}"
        )
    return args


def paths_for(output_dir: Path, run_id: str) -> CapturePaths:
    stem = f"heavy-server-entity-{run_id}"
    capture = output_dir / f"{stem}.json.gz"
    # Samply drops only the final .gz suffix when it writes symbols.
    return CapturePaths(
        capture=capture,
        symbols=capture.with_suffix(".syms.json"),
        scene=output_dir / f"{stem}.scene.json",
        runtime=output_dir / f"{stem}.runtime.jsonl",
    )


def require_nonempty(path: Path, label: str) -> None:
    if not path.is_file() or path.stat().st_size == 0:
        raise RuntimeError(f"Samply did not produce a nonempty {label}: {path}")


def run_bounded(command: list[str], timeout_secs: int) -> subprocess.CompletedProcess[str]:
    """Run one owned foreground process group and stop it on the hard deadline."""
    process = subprocess.Popen(
        command,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout_secs)
    except subprocess.TimeoutExpired as error:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            # The process can exit in the small interval after `communicate`
            # reports its timeout; in either case, its final output is still
            # gathered and the capture is rejected below.
            pass
        stdout, stderr = process.communicate()
        raise RuntimeError(
            f"profiling command exceeded its {timeout_secs}-second process deadline: "
            f"{' '.join(command)}\n{stdout}{stderr}"
        ) from error
    result = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    if result.returncode:
        raise RuntimeError(
            f"command exited {result.returncode}: {' '.join(command)}\n{result.stdout}{result.stderr}"
        )
    return result


def emit_scene(server: Path, paths: CapturePaths, seed: int, scale: int, timeout_secs: int) -> dict:
    run_bounded(
        [
            str(server), "--emit-scene", str(paths.scene), "--scenario", "entity",
            "--seed", str(seed), "--scale", str(scale),
        ],
        timeout_secs,
    )
    require_nonempty(paths.scene, "heavy-scene handoff")
    try:
        scene = json.loads(paths.scene.read_text())
    except json.JSONDecodeError as error:
        raise RuntimeError(f"heavy-scene emitter wrote invalid JSON: {paths.scene}: {error}") from error
    if scene.get("schema") != 1 or scene.get("spec") != {
        "scenario": "entity", "seed": seed, "scale": scale,
    }:
        raise RuntimeError(f"heavy-scene emitter wrote the wrong scene identity: {scene.get('spec')!r}")
    return scene


def validate_runtime(paths: CapturePaths, seed: int, scale: int, scene: dict) -> dict:
    require_nonempty(paths.runtime, "server runtime record")
    try:
        records = [json.loads(line) for line in paths.runtime.read_text().splitlines() if line.strip()]
    except json.JSONDecodeError as error:
        raise RuntimeError(f"server runtime record is not JSONL: {paths.runtime}: {error}") from error
    if len(records) != 1:
        raise RuntimeError(f"expected exactly one runtime record, got {len(records)}: {paths.runtime}")
    record = records[0]
    expected_population = ENTITY_COUNT_PER_SCALE * scale
    if (
        record.get("status") != "complete"
        or record.get("scenario") != "entity"
        or record.get("seed") != seed
        or record.get("scale") != scale
        or record.get("scenario_hash") != scene.get("scene_hash")
    ):
        raise RuntimeError(f"server runtime record does not describe this successful scene: {record!r}")
    requested = record.get("requested", {}).get("entities_spawned")
    consumed = record.get("consumed", {}).get("entities_extracted")
    if requested != expected_population or consumed < expected_population:
        raise RuntimeError(
            "server runtime population witness failed: "
            f"requested={requested!r}, consumed={consumed!r}, expected={expected_population}"
        )
    return record


def capture(args: argparse.Namespace) -> tuple[CapturePaths, dict]:
    if not args.server.is_file():
        raise RuntimeError(
            f"release heavy-scene server is missing: {args.server}; run "
            "cargo build --release -p lodestone-server --example heavy-scene-server"
        )
    if shutil.which("samply") is None:
        raise RuntimeError("samply is not on PATH; install Samply before capturing")
    run_id = args.run_id or time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    paths = paths_for(args.output_dir, run_id)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    for path in (paths.capture, paths.symbols, paths.scene, paths.runtime):
        if path.exists():
            raise RuntimeError(f"refusing to overwrite an existing profiling artifact: {path}")

    scene = emit_scene(args.server, paths, args.seed, args.scale, args.wall_deadline_secs)
    run_bounded(
        [
            "samply", "record", "--save-only", "--unstable-presymbolicate", "-o", str(paths.capture),
            "--", str(args.server), "--scenario", "entity", "--seed", str(args.seed),
            "--scale", str(args.scale), "--phase", "ready", "--wall-deadline-secs",
            str(args.wall_deadline_secs), "--output", str(paths.runtime),
        ],
        args.wall_deadline_secs + PROFILER_GRACE_SECS,
    )
    require_nonempty(paths.capture, "capture")
    require_nonempty(paths.symbols, "presymbolication sidecar")
    return paths, validate_runtime(paths, args.seed, args.scale, scene)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        paths, record = capture(args)
    except RuntimeError as error:
        print(f"samply heavy server capture failed: {error}", file=sys.stderr)
        return 1
    print(f"capture: {paths.capture}")
    print(f"symbols: {paths.symbols}")
    print(f"scene: {paths.scene}")
    print(f"runtime: {paths.runtime}")
    print(
        "population: "
        f"requested={record['requested']['entities_spawned']} "
        f"consumed={record['consumed']['entities_extracted']}"
    )
    print(f"open flamegraph: samply load {paths.capture}")
    print(f"summarize symbols: python3 scripts/profile-cost-table.py {paths.capture}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
