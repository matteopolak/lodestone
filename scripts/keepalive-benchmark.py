#!/usr/bin/env python3
"""Run the deterministic keep-alive loop-stall controls.

This is an external wrapper around the existing integration path. It runs the
silent-client and responsive-client scenarios by exact test name, rejects a
Cargo filter that matched no tests, and reports the number of completed
scenarios. The paused-clock tests make the scenario count deterministic; the
wall time is printed only as an advisory local sample.

Run it from the repository root with:

    python3 scripts/keepalive-benchmark.py
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import time
from dataclasses import dataclass


TEST_TARGET = "serve_play"
CASES = (
    (
        "silent-client-timeout",
        "silent_client_is_disconnected_after_keep_alive_timeout",
    ),
    (
        "responsive-client-survival",
        "responsive_client_survives_multiple_keep_alive_intervals",
    ),
)
RESULT_RE = re.compile(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed;")


@dataclass(frozen=True)
class CaseResult:
    name: str
    test: str
    passed: int
    failed: int
    wall_ms: float


def run_case(case_name: str, test_name: str) -> CaseResult:
    started = time.monotonic()
    process = subprocess.run(
        [
            "cargo",
            "test",
            "-p",
            "lodestone-server",
            "--test",
            TEST_TARGET,
            test_name,
            "--",
            "--exact",
            "--nocapture",
        ],
        capture_output=True,
        text=True,
    )
    wall_ms = (time.monotonic() - started) * 1_000.0
    output = f"{process.stdout}\n{process.stderr}"
    matches = RESULT_RE.findall(output)
    if not matches:
        raise RuntimeError(
            f"{test_name}: Cargo produced no test-result summary (exit {process.returncode})\n"
            f"{output}"
        )
    passed, failed = (int(value) for value in matches[-1])
    if process.returncode != 0 or passed != 1 or failed != 0:
        raise RuntimeError(
            f"{test_name}: expected exactly one passing test, got "
            f"passed={passed} failed={failed} exit={process.returncode}\n{output}"
        )
    return CaseResult(case_name, test_name, passed, failed, wall_ms)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    results: list[CaseResult] = []
    try:
        for name, test_name in CASES:
            result = run_case(name, test_name)
            results.append(result)
            print(f"ok   {name}: passed=1 wall_ms={result.wall_ms:.1f} (advisory)")
    except (OSError, RuntimeError) as error:
        print(f"FAIL keepalive benchmark: {error}", file=sys.stderr)
        return 1

    print(
        "KEEPALIVE_BENCHMARK "
        f"scenarios={len(results)} passed={sum(result.passed for result in results)} "
        f"failed={sum(result.failed for result in results)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
