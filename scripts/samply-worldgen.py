#!/usr/bin/env python3
"""Run one bounded Samply capture at a world-generation boundary.

The modes deliberately expose different consumers of the same generation
products: the embedded server benchmark, the dimension-separated generator
workload, and the streaming parity consumer.  Use ``--dry-run`` to inspect the
exact command without starting a build, oracle, or profiler.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys
from datetime import datetime, timezone


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUTPUT = ROOT / "bench-results/profiles"
MAX_RADIUS = 32
MAX_GRID_SIDE = 64


def target_directory() -> Path:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        check=True,
    )
    return Path(json.loads(result.stdout)["target_directory"])


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--mode",
        choices=("production", "session", "parity-consumer"),
        default="production",
    )
    parser.add_argument("--seed", type=int, default=3)
    parser.add_argument("--radius", type=int, default=8)
    parser.add_argument("--grid-side", type=int, default=16)
    parser.add_argument("--dimension", choices=("all", "overworld", "nether", "end"), default="all")
    parser.add_argument("--stage", choices=("all", "shaped", "decorated"), default="all")
    parser.add_argument("--cx", nargs=2, type=int, metavar=("LOW", "HIGH"), default=(0, 0))
    parser.add_argument("--cz", nargs=2, type=int, metavar=("LOW", "HIGH"), default=(0, 0))
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--run-id", help="stable artifact suffix; defaults to the current UTC timestamp")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    if not 0 <= args.radius <= MAX_RADIUS:
        parser.error(f"--radius must be 0..{MAX_RADIUS}")
    if not 1 <= args.grid_side <= MAX_GRID_SIDE:
        parser.error(f"--grid-side must be 1..{MAX_GRID_SIDE}")
    for name in ("cx", "cz"):
        bounds = getattr(args, name)
        if bounds[0] > bounds[1]:
            parser.error(f"--{name} bounds must be ordered")
    if args.mode == "parity-consumer" and args.dimension == "all":
        parser.error("--mode parity-consumer requires one --dimension")
    if args.mode != "production" and args.radius != 8:
        parser.error("--radius applies only to --mode production")
    return args


def run_id(args: argparse.Namespace) -> str:
    return args.run_id or datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def capture_path(args: argparse.Namespace) -> Path:
    return args.output_dir / f"worldgen-{args.mode}-{run_id(args)}.json.gz"


def command(args: argparse.Namespace, target: Path) -> list[str]:
    if args.mode == "production":
        binary = target / "release/examples/bench_worldgen"
        return [str(binary), str(args.seed), str(args.radius)]
    if args.mode == "session":
        binary = target / "release/examples/throughput"
        return [str(binary), str(args.seed), str(args.grid_side), args.dimension, args.stage]
    return [
        str(ROOT / "scripts/worldgen-oracle/stream-parity.sh"),
        "--dimension", args.dimension if args.dimension != "all" else "overworld",
        "--cx", str(args.cx[0]), str(args.cx[1]),
        "--cz", str(args.cz[0]), str(args.cz[1]),
    ]


def build_command(args: argparse.Namespace) -> list[str] | None:
    if args.mode == "production":
        return ["cargo", "build", "--release", "-p", "lodestone-server", "--example", "bench_worldgen"]
    if args.mode == "session":
        return ["cargo", "build", "--release", "-p", "lodestone-worldgen", "--example", "throughput"]
    return None


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    args.output_dir = args.output_dir.resolve()
    if shutil.which("samply") is None and not args.dry_run:
        raise RuntimeError("samply is not on PATH; install it and run `samply setup` on macOS")
    target = target_directory()
    output = capture_path(args)
    command_line = command(args, target)
    profiler = [
        "samply", "record", "--save-only", "--unstable-presymbolicate",
        "-o", str(output), "--", *command_line,
    ]
    print(f"mode={args.mode} target_directory={target}")
    build = build_command(args)
    if build is not None:
        print("build=" + " ".join(build))
    print("command=" + " ".join(profiler))
    if args.dry_run:
        return 0
    args.output_dir.mkdir(parents=True, exist_ok=True)
    if build is not None:
        subprocess.run(build, cwd=ROOT, check=True)
    subprocess.run(profiler, cwd=ROOT, check=True)
    if not output.is_file() or output.stat().st_size == 0:
        raise RuntimeError(f"Samply did not produce a nonempty capture: {output}")
    print(f"capture={output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, subprocess.CalledProcessError, RuntimeError, ValueError) as error:
        print(f"samply-worldgen: {error}", file=sys.stderr)
        raise SystemExit(1) from error
