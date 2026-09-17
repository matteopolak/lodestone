#!/usr/bin/env python3
"""Build and capture one finite integrated-server join with Samply."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import shutil
import subprocess


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUTPUT = ROOT / "bench-results/profiles"


def target_directory() -> Path:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        check=True,
    )
    return Path(json.loads(result.stdout)["target_directory"])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seed", type=int, default=4242)
    parser.add_argument("--radius", type=int, default=1)
    parser.add_argument("--deadline-seconds", type=int, default=240)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--run-id", help="stable artifact suffix")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if not 0 <= args.radius <= 32:
        parser.error("--radius must be between 0 and 32")
    if args.deadline_seconds <= 0:
        parser.error("--deadline-seconds must be positive")
    if shutil.which("samply") is None and not args.dry_run:
        raise RuntimeError("samply is not on PATH; install it and run `samply setup`")

    target = target_directory()
    binary = target / "release/join_profile"
    run_id = args.run_id or datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = args.output_dir.resolve() / f"integrated-join-{run_id}.json.gz"
    command = [str(binary), str(args.seed), str(args.radius), str(args.deadline_seconds)]
    profiler = [
        "samply", "record", "--save-only", "--unstable-presymbolicate",
        "-o", str(output), "--", *command,
    ]
    print(f"target_directory={target}")
    print("build=cargo build --release -p lodestone-shell --bin join_profile")
    print("command=" + " ".join(profiler))
    if args.dry_run:
        return 0
    args.output_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["cargo", "build", "--release", "-p", "lodestone-shell", "--bin", "join_profile"],
        cwd=ROOT,
        check=True,
    )
    subprocess.run(profiler, cwd=ROOT, check=True)
    if not output.is_file() or output.stat().st_size == 0:
        raise RuntimeError(f"Samply did not produce a nonempty capture: {output}")
    print(f"capture={output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
