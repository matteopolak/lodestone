#!/usr/bin/env python3
"""Capture one bounded Samply profile of the populated chunk-owner tick scene.

The scene drives the ordinary integrated-server tick loop through eight chunk
owners, scheduled block and fluid work, resident furnaces, and ambient mobs.
It is an investigation input, never a duration gate. A direct preflight run
records and verifies the scene's work witnesses before the same finite command
is run under Samply.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import time
from dataclasses import dataclass


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_SERVER = ROOT / "target/release/examples/chunk-owner-tick-profile"
DEFAULT_OUTPUT = ROOT / "bench-results/profiles"
SCENE_NAME = "chunk-owner-mixed-8"
DEFAULT_TICKS = 128
MAX_TICKS = 512
OWNER_COUNT = 8
AMBIENT_MOB_COUNT = 64
MAX_WALL_DEADLINE_SECS = 60
PROFILER_GRACE_SECS = 15
MACOS_DEBUGGER_ENTITLEMENT = "com.apple.security.cs.debugger"
REQUIRED_COUNTERS = (
    "scheduled_block_ticks",
    "scheduled_fluid_ticks",
    "block_entity_batches",
    "entity_effect_batches",
    "entity_effects",
)


@dataclass(frozen=True)
class CapturePaths:
    capture: Path
    symbols: Path
    witness: Path


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, default=DEFAULT_SERVER)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--ticks", type=int, default=DEFAULT_TICKS)
    parser.add_argument("--wall-deadline-secs", type=int, default=30)
    parser.add_argument(
        "--run-id",
        help="stable artifact suffix for automation; defaults to the current UTC timestamp",
    )
    args = parser.parse_args(argv)
    if not 1 <= args.ticks <= MAX_TICKS:
        parser.error(f"--ticks must be 1..{MAX_TICKS}")
    if not 1 <= args.wall_deadline_secs <= MAX_WALL_DEADLINE_SECS:
        parser.error(f"--wall-deadline-secs must be 1..{MAX_WALL_DEADLINE_SECS}")
    return args


def paths_for(output_dir: Path, run_id: str) -> CapturePaths:
    stem = f"chunk-owner-tick-{run_id}"
    capture = output_dir / f"{stem}.json.gz"
    return CapturePaths(
        capture=capture,
        symbols=capture.with_suffix(".syms.json"),
        witness=output_dir / f"{stem}.witness.txt",
    )


def require_nonempty(path: Path, label: str) -> None:
    if not path.is_file() or path.stat().st_size == 0:
        raise RuntimeError(f"Samply did not produce a nonempty {label}: {path}")


def require_macos_samply_setup(samply: str) -> None:
    """Reject an unsigned Samply before it can start a profile run."""
    if sys.platform != "darwin":
        return
    try:
        signature = subprocess.run(
            ["codesign", "-d", "--entitlements", ":-", samply],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    except OSError as error:
        raise RuntimeError(
            "cannot inspect Samply's macOS code signature; install the Xcode command-line tools "
            "and run `samply setup`"
        ) from error
    entitlements = signature.stdout + signature.stderr
    if signature.returncode or MACOS_DEBUGGER_ENTITLEMENT not in entitlements:
        raise RuntimeError(
            "Samply is not enabled for macOS process attachment: its code signature lacks "
            f"{MACOS_DEBUGGER_ENTITLEMENT}. Run `samply setup` interactively once (and again "
            "after updating Samply). It self-signs only the Samply executable; do not use sudo."
        )


def run_bounded(command: list[str], timeout_secs: int) -> subprocess.CompletedProcess[str]:
    """Run one foreground process group and stop it at the declared deadline."""
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


def parse_witness(output: str, ticks: int) -> dict[str, int]:
    """Require the production profile entrypoint to name all work witnesses."""
    lines = [line for line in output.splitlines() if line.startswith("scene=")]
    if len(lines) != 1:
        raise RuntimeError(f"expected one scene witness line, got {len(lines)}: {output!r}")
    values = {
        key: int(value)
        for key, value in re.findall(r"([a-z_]+)=([0-9]+)", lines[0])
    }
    if not lines[0].startswith(f"scene={SCENE_NAME} "):
        raise RuntimeError(f"wrong scene witness: {lines[0]!r}")
    if values.get("ticks") != ticks:
        raise RuntimeError(f"wrong tick witness: expected {ticks}, got {values.get('ticks')!r}")
    phase_values = lines[0].split("phases=", 1)
    if len(phase_values) != 2:
        raise RuntimeError(f"missing phase witnesses: {lines[0]!r}")
    try:
        phase_counts = [int(value) for value in phase_values[1].split(" ", 1)[0].split("/")]
    except ValueError as error:
        raise RuntimeError(f"invalid phase witnesses: {lines[0]!r}") from error
    if phase_counts != [ticks, ticks, ticks]:
        raise RuntimeError(f"phase witness did not drive every tick: {phase_counts!r}")
    if values.get("owners") != OWNER_COUNT or values.get("ambient_mobs") != AMBIENT_MOB_COUNT:
        raise RuntimeError(
            "wrong population envelope: "
            f"owners={values.get('owners')!r}, ambient_mobs={values.get('ambient_mobs')!r}"
        )
    for counter in REQUIRED_COUNTERS:
        if values.get(counter, 0) <= 0:
            raise RuntimeError(f"missing or zero {counter} witness: {lines[0]!r}")
    if values["scheduled_block_ticks"] < OWNER_COUNT:
        raise RuntimeError(f"scheduled block work did not reach every owner: {lines[0]!r}")
    if values["scheduled_fluid_ticks"] < OWNER_COUNT:
        raise RuntimeError(f"scheduled fluid work did not reach every owner: {lines[0]!r}")
    if values["block_entity_batches"] < OWNER_COUNT * ticks:
        raise RuntimeError(f"block-entity work did not reach every owner each tick: {lines[0]!r}")
    if values["entity_effect_batches"] < OWNER_COUNT:
        raise RuntimeError(f"ambient entity work did not reach every owner: {lines[0]!r}")
    if values["entity_effects"] < values["entity_effect_batches"]:
        raise RuntimeError(f"entity batch witness exceeds effects: {lines[0]!r}")
    return values


def capture(args: argparse.Namespace) -> tuple[CapturePaths, dict[str, int]]:
    if not args.server.is_file():
        raise RuntimeError(
            f"release chunk-owner profile is missing: {args.server}; run "
            "cargo build --release -p lodestone-server --features profile-harness "
            "--example chunk-owner-tick-profile"
        )
    samply = shutil.which("samply")
    if samply is None:
        raise RuntimeError("samply is not on PATH; install Samply before capturing")
    require_macos_samply_setup(samply)
    run_id = args.run_id or time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    paths = paths_for(args.output_dir, run_id)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    for path in (paths.capture, paths.symbols, paths.witness):
        if path.exists():
            raise RuntimeError(f"refusing to overwrite an existing profiling artifact: {path}")

    command = [str(args.server), str(args.ticks)]
    direct = run_bounded(command, args.wall_deadline_secs)
    witnesses = parse_witness(direct.stdout, args.ticks)
    paths.witness.write_text(direct.stdout, encoding="utf-8")
    run_bounded(
        [
            "samply", "record", "--save-only", "--unstable-presymbolicate", "-o", str(paths.capture),
            "--", *command,
        ],
        args.wall_deadline_secs + PROFILER_GRACE_SECS,
    )
    require_nonempty(paths.capture, "capture")
    require_nonempty(paths.symbols, "presymbolication sidecar")
    return paths, witnesses


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        paths, witnesses = capture(args)
    except RuntimeError as error:
        print(f"samply chunk-owner capture failed: {error}", file=sys.stderr)
        return 1
    print(f"capture: {paths.capture}")
    print(f"symbols: {paths.symbols}")
    print(f"witness: {paths.witness}")
    print(
        "work: " + " ".join(f"{key}={witnesses[key]}" for key in REQUIRED_COUNTERS)
    )
    print(f"open flamegraph: samply load {paths.capture}")
    print(f"summarize symbols: python3 scripts/profile-cost-table.py {paths.capture}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
