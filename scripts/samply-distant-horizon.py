#!/usr/bin/env python3
"""Capture one bounded Samply profile of the distant-horizon workload.

The preflight executes the production horizon-profile binary and consumes its
two path-witness lines before Samply records the identical command. This is an
investigation input, never a timing or CI gate.
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
DEFAULT_BINARY = ROOT / "target/release/horizon-profile"
DEFAULT_OUTPUT = ROOT / "bench-results/profiles"
MAX_WALL_DEADLINE_SECS = 90
PROFILER_GRACE_SECS = 15
MACOS_DEBUGGER_ENTITLEMENT = "com.apple.security.cs.debugger"
FAR_COLUMNS = 256
ATLAS_BYTES = 2_654_208
HORIZON_CELLS_PER_TILE = 64 * 64


@dataclass(frozen=True)
class CapturePaths:
    capture: Path
    symbols: Path
    witness: Path


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument(
        "--capture",
        type=Path,
        help="exact capture path; conflicts with --output-dir and --run-id",
    )
    parser.add_argument("--wall-deadline-secs", type=int, default=60)
    parser.add_argument(
        "--run-id",
        help="stable artifact suffix for automation; defaults to the current UTC timestamp",
    )
    args = parser.parse_args(argv)
    if not 1 <= args.wall_deadline_secs <= MAX_WALL_DEADLINE_SECS:
        parser.error(f"--wall-deadline-secs must be 1..{MAX_WALL_DEADLINE_SECS}")
    if args.capture is not None and (
        args.output_dir != DEFAULT_OUTPUT or args.run_id is not None
    ):
        parser.error("--capture cannot be combined with --output-dir or --run-id")
    return args


def paths_for(output_dir: Path, run_id: str) -> CapturePaths:
    stem = f"distant-horizon-{run_id}"
    capture = output_dir / f"{stem}.json.gz"
    return CapturePaths(
        capture=capture,
        symbols=capture.with_suffix(".syms.json"),
        witness=output_dir / f"{stem}.witness.txt",
    )


def paths_for_capture(capture: Path) -> CapturePaths:
    """Derive sidecars beside a caller-selected Samply output path."""
    if not capture.name.endswith(".json.gz"):
        raise RuntimeError(f"capture path must end in .json.gz: {capture}")
    stem = capture.name.removesuffix(".json.gz")
    return CapturePaths(
        capture=capture,
        symbols=capture.with_suffix(".syms.json"),
        witness=capture.with_name(f"{stem}.witness.txt"),
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


def _witness_values(line: str, phase: str) -> dict[str, int]:
    prefix = f"horizon-profile phase={phase} "
    if not line.startswith(prefix):
        raise RuntimeError(f"wrong {phase} witness: {line!r}")
    values = {key: int(value) for key, value in re.findall(r"([a-z_]+)=([0-9]+)", line)}
    if not values:
        raise RuntimeError(f"missing {phase} counters: {line!r}")
    return values


def parse_witness(output: str) -> tuple[dict[str, int], dict[str, int]]:
    """Require both production phases and their finite-work coverage witnesses."""
    lines = [line for line in output.splitlines() if line.startswith("horizon-profile phase=")]
    if len(lines) != 2:
        raise RuntimeError(f"expected two horizon-profile witness lines, got {len(lines)}: {output!r}")
    far = _witness_values(lines[0], "far-columns")
    horizon = _witness_values(lines[1], "horizon")
    if far.get("full") != 0:
        raise RuntimeError(f"far workload fell back to full columns: {lines[0]!r}")
    if far.get("requested") != FAR_COLUMNS or far.get("shaped") != FAR_COLUMNS:
        raise RuntimeError(f"far workload was not exactly {FAR_COLUMNS} shaped columns: {lines[0]!r}")
    for counter in ("solid_blocks", "store_entries"):
        if far.get(counter, 0) <= 0:
            raise RuntimeError(f"missing or zero far {counter} witness: {lines[0]!r}")
    for counter in ("candidates", "tiles_updated", "tiles_skipped", "cells_written"):
        if horizon.get(counter, 0) <= 0:
            raise RuntimeError(f"missing or zero horizon {counter} witness: {lines[1]!r}")
    if horizon["cells_written"] != horizon["tiles_updated"] * HORIZON_CELLS_PER_TILE:
        raise RuntimeError(f"horizon cell count does not match updated tiles: {lines[1]!r}")
    for counter in ("atlas_cpu_bytes", "atlas_gpu_bytes"):
        if horizon.get(counter) != ATLAS_BYTES:
            raise RuntimeError(f"wrong fixed atlas {counter}: {lines[1]!r}")
    return far, horizon


def _remove_created(paths: CapturePaths) -> None:
    for path in (paths.capture, paths.symbols, paths.witness):
        path.unlink(missing_ok=True)


def capture(args: argparse.Namespace) -> tuple[CapturePaths, tuple[dict[str, int], dict[str, int]]]:
    if not args.binary.is_file():
        raise RuntimeError(
            f"release horizon profile is missing: {args.binary}; run "
            "cargo build --release -p lodestone-shell --bin horizon-profile"
        )
    samply = shutil.which("samply")
    if samply is None:
        raise RuntimeError("samply is not on PATH; install Samply before capturing")
    require_macos_samply_setup(samply)
    run_id = args.run_id or time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    paths = paths_for_capture(args.capture) if args.capture else paths_for(args.output_dir, run_id)
    paths.capture.parent.mkdir(parents=True, exist_ok=True)
    for path in (paths.capture, paths.symbols, paths.witness):
        if path.exists():
            raise RuntimeError(f"refusing to overwrite an existing profiling artifact: {path}")

    command = [str(args.binary)]
    try:
        direct = run_bounded(command, args.wall_deadline_secs)
        witnesses = parse_witness(direct.stdout)
        paths.witness.write_text(direct.stdout, encoding="utf-8")
        run_bounded(
            [
                "samply", "record", "--save-only", "--unstable-presymbolicate", "-o",
                str(paths.capture), "--", *command,
            ],
            args.wall_deadline_secs + PROFILER_GRACE_SECS,
        )
        require_nonempty(paths.capture, "capture")
        require_nonempty(paths.symbols, "presymbolication sidecar")
    except Exception:
        _remove_created(paths)
        raise
    return paths, witnesses


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        paths, (far, horizon) = capture(args)
    except RuntimeError as error:
        print(f"samply distant-horizon capture failed: {error}", file=sys.stderr)
        return 1
    print(f"capture: {paths.capture}")
    print(f"symbols: {paths.symbols}")
    print(f"witness: {paths.witness}")
    print(
        "work: "
        f"shaped_columns={far['shaped']} tiles_updated={horizon['tiles_updated']} "
        f"cells_written={horizon['cells_written']}"
    )
    print(f"open flamegraph: samply load {paths.capture}")
    print(f"summarize symbols: python3 scripts/profile-cost-table.py {paths.capture}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
