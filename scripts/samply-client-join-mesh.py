#!/usr/bin/env python3
"""Build and capture the bounded client join/mesh integration fixture."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
from datetime import datetime, timezone


ROOT = Path(__file__).resolve().parent.parent
TEST = "client_join_mesh_profile"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "bench-results/profiles")
    parser.add_argument("--run-id")
    parser.add_argument("--radius", type=int)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if shutil.which("samply") is None and not args.dry_run:
        raise RuntimeError("samply is not on PATH; install it and run `samply setup`")

    run_id = args.run_id or datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = args.output_dir.resolve() / f"client-join-mesh-{run_id}.json.gz"
    build = [
        "cargo", "test", "--release", "-p", "lodestone-shell", "--test", TEST,
        "--no-run", "--message-format=json",
    ]
    if args.dry_run:
        print("build=" + " ".join(build))
        print(
            "command=samply record --save-only --unstable-presymbolicate "
            f"-o {output} -- <cargo-reported-test-executable> --ignored "
            f"--nocapture --exact {TEST} --test-threads=1"
        )
        return 0
    result = subprocess.run(build, cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True)
    binary = None
    for line in result.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("reason") != "compiler-artifact":
            continue
        target = event.get("target", {})
        executable = event.get("executable")
        if target.get("name") == TEST and executable:
            binary = executable
    if binary is None:
        raise RuntimeError(f"cargo did not report the {TEST} test executable")

    command = [
        "samply", "record", "--save-only", "--unstable-presymbolicate", "-o", str(output),
        "--", binary, "--ignored", "--nocapture", "--exact", TEST, "--test-threads=1",
    ]
    print("command=" + " ".join(command))
    args.output_dir.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    if args.radius is not None:
        env["LODESTONE_CLIENT_JOIN_RADIUS"] = str(args.radius)
    assets = ROOT / ".cache/mc/26.2"
    if "LODESTONE_ASSETS" not in env and assets.is_dir():
        env["LODESTONE_ASSETS"] = str(assets)
    subprocess.run(command, cwd=ROOT, env=env, check=True)
    if not output.is_file() or output.stat().st_size == 0:
        raise RuntimeError(f"Samply did not produce a nonempty capture: {output}")
    print(f"capture={output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
