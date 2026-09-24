#!/usr/bin/env python3
"""Capture a bounded integrated or client join with macOS CPU counters.

The workload writes its normal aggregate phase report to the target stdout log;
the Instruments capture supplies retired instructions, cycles, and IPC for the
same process.  Keeping those two records together makes phase wall markers and
counter totals comparable without adding logging to the join loop.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUTPUT = ROOT / "bench-results/profiles/hardware"


def target_directory() -> Path:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        check=True,
    )
    return Path(json.loads(result.stdout)["target_directory"])


def client_executable(output: str) -> Path:
    for line in output.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("reason") != "compiler-artifact":
            continue
        target = event.get("target", {})
        executable = event.get("executable")
        if target.get("name") == "client_join_mesh_profile" and executable:
            return Path(executable)
    raise RuntimeError("cargo did not report the client_join_mesh_profile executable")


def phase_lines(workload: str, log: Path) -> list[str]:
    prefix = "JOIN_PROFILE " if workload == "integrated" else "CLIENT_JOIN_MESH_PROFILE "
    report = None
    for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
        if line.startswith(prefix):
            report = json.loads(line[len(prefix):])
    if report is None:
        return ["phase_report=missing"]

    phase_keys = (
        ("integrated", ("connecting_ms", "joining_phase_ms", "logged_in_phase_ms",
                         "first_chunk_event_ms", "total_ms")),
        ("client", ("joining_phase_ms", "loading_terrain_phase_ms", "overlay_ready_ms",
                     "all_visible_columns_ms", "all_requested_server_columns_ms",
                     "all_visible_meshes_settled_ms", "first_presented_terrain_ms", "elapsed_ms")),
    )
    keys = dict(phase_keys)[workload]
    lines = ["phase_report=present", f"phase_schema={report.get('schema', 'unknown')}"]
    for key in keys:
        value = report.get(key)
        if isinstance(value, (int, float)):
            lines.append(f"phase.{key}={value:.3f} ms")
    return lines


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("workload", choices=("integrated", "client"))
    parser.add_argument("--seed", type=int, default=4242)
    parser.add_argument("--radius", type=int)
    parser.add_argument("--deadline-seconds", type=int, default=240)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--run-id")
    parser.add_argument("--template", default=os.environ.get("LODESTONE_JOIN_XCTRACE_TEMPLATE", "CPU Counters"))
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    radius = args.radius if args.radius is not None else (1 if args.workload == "integrated" else 1)
    if not 0 <= radius <= 32:
        parser.error("--radius must be between 0 and 32")
    if args.deadline_seconds <= 0:
        parser.error("--deadline-seconds must be positive")
    if not args.dry_run:
        if sys.platform != "darwin":
            raise RuntimeError("join hardware counters require macOS Instruments")
        if shutil.which("xcrun") is None:
            raise RuntimeError("xcrun is unavailable")

    run_id = args.run_id or datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    stem = f"{args.workload}-join-hardware-{run_id}"
    output_dir = args.output_dir.resolve()
    trace = output_dir / f"{stem}.trace"
    toc = output_dir / f"{stem}-toc.xml"
    counters = output_dir / f"{stem}-counters.xml"
    summary = output_dir / f"{stem}-summary.txt"
    stdout = output_dir / f"{stem}.log"
    paths = (trace, toc, counters, summary, stdout)
    if any(path.exists() for path in paths):
        raise RuntimeError(f"run id already exists: {run_id}")

    target = target_directory()
    if args.workload == "integrated":
        binary = target / "release/join_profile"
        build = ["cargo", "build", "--release", "-p", "lodestone-shell", "--bin", "join_profile"]
        command = [str(binary), str(args.seed), str(radius), str(args.deadline_seconds)]
        process = "join_profile"
        time_limit = args.deadline_seconds + 60
    else:
        build = ["cargo", "test", "--release", "-p", "lodestone-shell", "--test",
                 "client_join_mesh_profile", "--no-run", "--message-format=json"]
        binary = None
        command = []
        process = "client_join_mesh_profile"
        time_limit = max(args.deadline_seconds, 120) + 60

    env = os.environ.copy()
    if args.workload == "client":
        env["LODESTONE_CLIENT_JOIN_RADIUS"] = str(radius)
        assets = ROOT / ".cache/mc/26.2"
        if "LODESTONE_ASSETS" not in env and assets.is_dir():
            env["LODESTONE_ASSETS"] = str(assets)

    if not args.dry_run:
        build_output = None
        if args.workload == "client":
            build_result = subprocess.run(
                build, cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
            )
            build_output = build_result.stdout
        else:
            subprocess.run(build, cwd=ROOT, check=True)
        if args.workload == "client":
            binary = client_executable(build_output or "")
            command = [str(binary), "--ignored", "--nocapture", "--exact",
                       "client_join_mesh_profile", "--test-threads=1"]
    elif args.workload == "client":
        command = ["<cargo-reported-test-executable>", "--ignored", "--nocapture",
                   "--exact", "client_join_mesh_profile", "--test-threads=1"]

    profiler = [
        "xcrun", "xctrace", "record", "--no-prompt", "--template", args.template,
        "--output", str(trace), "--time-limit", f"{time_limit}s",
        "--target-stdout", str(stdout), "--launch", "--", *command,
    ]
    print(f"target_directory={target}")
    print("build=" + " ".join(build))
    print("command=" + " ".join(profiler))
    print(f"process={process}")
    print("evidence=retired instructions, cycles, and IPC from the CPU counter samples; phase wall markers from the workload report")
    if args.dry_run:
        return 0

    output_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(profiler, cwd=ROOT, env=env, check=True)
    if not trace.is_file() or trace.stat().st_size == 0:
        raise RuntimeError(f"xctrace did not produce a nonempty capture: {trace}")
    subprocess.run(["xcrun", "xctrace", "export", "--input", str(trace), "--toc", "--output", str(toc)], check=True)
    toc_text = toc.read_text(encoding="utf-8", errors="replace")
    has_counters = 'schema="counters-profile"' in toc_text
    has_instruction_counters = 'pmc-events="Cycles Instructions"' in toc_text
    with summary.open("w", encoding="utf-8") as handle:
        handle.write(f"workload={args.workload}\nprocess={process}\ntemplate={args.template}\n")
        handle.write("counter_table=" + ("present\n" if has_counters else "missing\n"))
        handle.write("\n".join(phase_lines(args.workload, stdout)) + "\n\n")
        if has_counters and has_instruction_counters:
            subprocess.run([
                "xcrun", "xctrace", "export", "--input", str(trace),
                "--xpath", "/trace-toc/run[@number=\"1\"]/data/table[@schema=\"counters-profile\"]",
                "--output", str(counters),
            ], check=True)
            result = subprocess.run(
                ["python3", "scripts/summarize-xctrace-counters.py", str(counters),
                 "--process", process], cwd=ROOT, text=True,
                stdout=subprocess.PIPE, check=True,
            )
            handle.write(result.stdout)
        elif not has_counters:
            handle.write("No counters-profile table was emitted by the selected Instruments template.\n")
        else:
            handle.write(
                "The selected template did not expose the required ordered "
                "Cycles/Instructions counter pair.\n"
            )
    print(f"trace={trace}\ntoc={toc}\nsummary={summary}\nstdout={stdout}")
    if counters.is_file():
        print(f"counters={counters}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
