#!/usr/bin/env python3
"""Executable control for `scripts/bench-gate.py`.

Stdlib only, no pytest -- the same shape as
`scripts/test-profile-cost-table.py`, and for the same reason: these are
Python scripts and no crate owns them, so `cargo test` cannot reach them.

# Why a control suite exists for a gate at all

An assertion of an absence needs a control proving the detector works. A
regression gate is exactly that species: on a healthy tree it prints "no
regressions" forever, and a gate that *cannot* fail prints the identical thing.
So every check below that expects green is paired with one that plants a
regression on purpose and expects red, and the plants are then mutation-tested:
each broken copy of the gate under test must turn this suite red. A fix nobody
can observe failing is a description, not a control.

The planted regressions here are synthetic fixtures, which proves the
value-to-verdict half of the chain. The bench-to-value half is proved
separately, by running a real bench with a real change and watching this gate
go red on the number it recorded -- see `docs/benchmark-regression-gate.md`.

# Running it

    python3 scripts/test-bench-gate.py

Point it at a copy of the gate to mutation-test that copy:

    BENCH_GATE_PATH=/tmp/broken-gate.py python3 scripts/test-bench-gate.py

Exit 0 = every check passed. Nothing here writes inside the checkout: every
fixture lives in a per-run temporary directory.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

GATE = Path(
    os.environ.get("BENCH_GATE_PATH")
    or Path(__file__).resolve().parent / "bench-gate.py"
)

FAILURES: list[str] = []
PASSES = 0


def check(name: str, condition: bool, detail: str = "") -> None:
    global PASSES
    if condition:
        PASSES += 1
        print(f"  ok   {name}")
    else:
        FAILURES.append(f"{name}: {detail}")
        print(f"  FAIL {name}: {detail}")


def baseline_doc(entries: list[dict], bench: str = "widget") -> dict:
    return {"bench": bench, "entries": entries}


def jsonl(records: list[dict]) -> str:
    return "".join(json.dumps(r) + "\n" for r in records)


def record(
    metric: str,
    value,
    unit: str,
    scene: str = "fixed scene",
    sha: str = "abc123",
    machine: str = "runner",
) -> dict:
    return {
        "timestamp": 1,
        "git_sha": sha,
        "machine": machine,
        "profile": "release",
        "scene": scene,
        "metric": metric,
        "value": value,
        "unit": unit,
    }


def run_gate(workdir: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [
            sys.executable,
            str(GATE),
            "--baseline-dir",
            str(workdir / "bench-baselines"),
            "--results-dir",
            str(workdir / "bench-results"),
            *args,
        ],
        capture_output=True,
        text=True,
    )


def make_case(
    root: Path, name: str, entries: list[dict], records: list[dict]
) -> Path:
    workdir = root / name
    (workdir / "bench-baselines").mkdir(parents=True)
    (workdir / "bench-results").mkdir(parents=True)
    (workdir / "bench-baselines" / "widget.json").write_text(
        json.dumps(baseline_doc(entries), indent=2) + "\n"
    )
    if records is not None:
        (workdir / "bench-results" / "widget.jsonl").write_text(jsonl(records))
    return workdir


# The subject every fixture shares: a per-frame API-call count that must not
# grow with the number of resident sections. This is the shape of the one real
# performance regression this repository has found and fixed -- a render path
# rewriting one uniform per resident section per frame -- reduced to the two
# numbers a gate can actually hold: a per-frame constant, and a per-section
# count that is allowed to track the scene.
BIND_GROUP_ENTRY = {
    "metric": "camera_bind_group_switches",
    "scene": "fixed scene",
    "unit": "calls",
    "value": 1,
    "tolerance_pct": 0,
}
SECTION_ENTRY = {
    "metric": "drawn_sections",
    "scene": "fixed scene",
    "unit": "sections",
    "value": 347,
    "tolerance_pct": 0,
}


def main() -> int:
    print(f"bench-gate under test: {GATE}")
    if not GATE.exists():
        print(f"no such gate script: {GATE}", file=sys.stderr)
        return 2

    root = Path(tempfile.mkdtemp(prefix="bench-gate-control-"))
    try:
        # 1. The control for every planted regression below: the identical
        #    fixture, unplanted, must be green. Without this, a gate that
        #    always fails would satisfy every red-expecting check here.
        work = make_case(
            root,
            "healthy",
            [BIND_GROUP_ENTRY, SECTION_ENTRY],
            [record("camera_bind_group_switches", 1, "calls"),
             record("drawn_sections", 347, "sections")],
        )
        got = run_gate(work, "--min-compared", "2")
        check(
            "healthy tree passes",
            got.returncode == 0,
            f"exit {got.returncode}\n{got.stdout}{got.stderr}",
        )

        # 2. The planted regression: the exact shape of the real one. One
        #    bind-group switch per drawn section instead of one per frame.
        work = make_case(
            root,
            "planted-per-section-uniform",
            [BIND_GROUP_ENTRY, SECTION_ENTRY],
            [record("camera_bind_group_switches", 347, "calls"),
             record("drawn_sections", 347, "sections")],
        )
        got = run_gate(work, "--min-compared", "1")
        check(
            "per-section-uniform regression is caught",
            got.returncode == 1,
            f"exit {got.returncode}\n{got.stdout}",
        )
        check(
            "the failure names the metric that moved",
            "camera_bind_group_switches" in got.stdout
            and "FAILED" in got.stdout,
            got.stdout,
        )
        check(
            "the failure does not implicate the metric that did not move",
            "FAIL" not in got.stdout.split("FAILED:")[0].split("drawn_sections")[-1][:40],
            got.stdout,
        )

        # 3. A gate that only ratchets one way is noise. An unexplained
        #    *improvement* is a changed number too -- most often a bench that
        #    stopped doing the work rather than a real win.
        work = make_case(
            root,
            "unexplained-improvement",
            [SECTION_ENTRY],
            [record("drawn_sections", 12, "sections")],
        )
        got = run_gate(work)
        check(
            "an unexplained improvement fails too (two-way band)",
            got.returncode == 1,
            f"exit {got.returncode}\n{got.stdout}",
        )

        # 4. Tolerance is honoured in both directions.
        work = make_case(
            root,
            "within-tolerance",
            [dict(SECTION_ENTRY, tolerance_pct=10)],
            [record("drawn_sections", 360, "sections")],
        )
        got = run_gate(work)
        check(
            "a move inside the band passes",
            got.returncode == 0,
            f"exit {got.returncode}\n{got.stdout}",
        )
        work = make_case(
            root,
            "outside-tolerance",
            [dict(SECTION_ENTRY, tolerance_pct=10)],
            [record("drawn_sections", 400, "sections")],
        )
        got = run_gate(work)
        check(
            "a move outside the band fails",
            got.returncode == 1,
            f"exit {got.returncode}\n{got.stdout}",
        )

        # 5. Vacuity. No results at all must be status 2, not 0: a gate that
        #    checked nothing is unrun, and the loudest way that happens in
        #    practice is a bench that failed to write its JSONL.
        work = make_case(root, "no-results", [SECTION_ENTRY], [])
        got = run_gate(work, "--min-compared", "1")
        check(
            "an empty results log is 'did not run', not 'passed'",
            got.returncode == 2,
            f"exit {got.returncode}\n{got.stdout}",
        )
        check(
            "and it says so",
            "GATE DID NOT RUN" in got.stdout or "NORUN" in got.stdout,
            got.stdout,
        )
        # The same fixture with --min-compared 0, which switches off the
        # count-based vacuity guard entirely. This separates the two reasons a
        # gate can be unrun: "fewer comparisons than demanded" and "this
        # bench's log does not exist". Without it, a gate that had lost the
        # second check would still look correct here, because the first one
        # happens to fire on the same fixture -- one defect masking another.
        got = run_gate(work, "--min-compared", "0")
        check(
            "a missing log is 'did not run' even with the count guard disabled",
            got.returncode == 2,
            f"exit {got.returncode}\n{got.stdout}",
        )

        # 6. --min-compared is enforced above zero, so a caller gating a known
        #    set learns that half the set vanished.
        work = make_case(
            root,
            "half-the-set",
            [SECTION_ENTRY, dict(BIND_GROUP_ENTRY, required=False)],
            [record("drawn_sections", 347, "sections")],
        )
        got = run_gate(work, "--min-compared", "2")
        check(
            "fewer comparisons than --min-compared is status 2",
            got.returncode == 2,
            f"exit {got.returncode}\n{got.stdout}",
        )
        got = run_gate(work, "--min-compared", "1")
        check(
            "the same fixture passes when only one metric is demanded",
            got.returncode == 0,
            f"exit {got.returncode}\n{got.stdout}",
        )

        # 7. A required metric that stopped being recorded is a broken bench.
        work = make_case(
            root,
            "required-absent",
            [dict(SECTION_ENTRY, required=True)],
            [record("something_else", 1, "calls")],
        )
        got = run_gate(work)
        check(
            "a required metric that vanished fails",
            got.returncode == 1,
            f"exit {got.returncode}\n{got.stdout}",
        )

        # 8. An optional metric that cannot be measured here (no GPU adapter on
        #    the runner) is skipped, and does not count toward --min-compared.
        work = make_case(
            root,
            "optional-absent",
            [
                SECTION_ENTRY,
                dict(
                    BIND_GROUP_ENTRY,
                    required=False,
                    optional_because="needs a GPU adapter",
                ),
            ],
            [record("drawn_sections", 347, "sections")],
        )
        got = run_gate(work, "--min-compared", "1")
        check(
            "an optional absent metric is skipped, not failed",
            got.returncode == 0 and "SKIP" in got.stdout,
            f"exit {got.returncode}\n{got.stdout}",
        )

        # 9. Duplicate keys are not extra observations. Without this guard,
        #     the same recorded metric satisfies --min-compared more than once
        #     and a baseline can silently lose coverage while still looking
        #     complete.
        work = make_case(
            root,
            "duplicate-baseline-key",
            [SECTION_ENTRY, dict(SECTION_ENTRY)],
            [record("drawn_sections", 347, "sections")],
        )
        got = run_gate(work, "--min-compared", "2")
        check(
            "duplicate baseline keys are rejected before coverage is counted",
            got.returncode == 2,
            f"exit {got.returncode}\n{got.stdout}{got.stderr}",
        )
        check(
            "duplicate rejection names the colliding scene and metric",
            "duplicate baseline key" in got.stderr
            and "drawn_sections" in got.stderr
            and "fixed scene" in got.stderr,
            f"{got.stdout}{got.stderr}",
        )
        got = run_gate(work, "--update")
        check(
            "--update rejects duplicate baseline keys too",
            got.returncode == 2,
            f"exit {got.returncode}\n{got.stdout}{got.stderr}",
        )

        # 10. The structural rule: a duration can never enter a baseline. This
        #    is the wall-clock-ceiling trap made unreachable rather than
        #    documented.
        work = make_case(
            root,
            "time-unit-refused",
            [
                {
                    "metric": "frame_median_ms",
                    "scene": "fixed scene",
                    "unit": "ms",
                    "value": 8.19,
                    "tolerance_pct": 25,
                }
            ],
            [record("frame_median_ms", 8.19, "ms")],
        )
        got = run_gate(work)
        check(
            "a baselined duration is refused outright",
            got.returncode == 2,
            f"exit {got.returncode}\n{got.stdout}{got.stderr}",
        )
        check(
            "and the refusal explains the wall-clock-ceiling reason",
            "measures time" in (got.stdout + got.stderr),
            got.stdout + got.stderr,
        )
        got = run_gate(work, "--update")
        check(
            "--update refuses to write one too",
            got.returncode == 2,
            f"exit {got.returncode}\n{got.stdout}{got.stderr}",
        )

        # 11. A metric that changed unit changed meaning.
        work = make_case(
            root,
            "unit-drift",
            [SECTION_ENTRY],
            [record("drawn_sections", 347, "calls")],
        )
        got = run_gate(work)
        check(
            "a unit change is a failure, not a silent comparison",
            got.returncode == 1 and "UNIT" in got.stdout,
            f"exit {got.returncode}\n{got.stdout}",
        )

        # 12. Zero is a real baseline value (a healthy leak probe records
        #     exactly zero bytes of growth) and a ratio against it is
        #     undefined, so the tolerance reads as an absolute allowance.
        zero_entry = {
            "metric": "rss_growth_bytes",
            "scene": "fixed scene",
            "unit": "bytes",
            "value": 0,
            "tolerance_pct": 0,
        }
        work = make_case(
            root, "zero-ok", [zero_entry], [record("rss_growth_bytes", 0, "bytes")]
        )
        got = run_gate(work)
        check(
            "a zero baseline matched by zero passes",
            got.returncode == 0,
            f"exit {got.returncode}\n{got.stdout}",
        )
        work = make_case(
            root,
            "zero-broken",
            [zero_entry],
            [record("rss_growth_bytes", 60555264, "bytes")],
        )
        got = run_gate(work)
        check(
            "a zero baseline broken by real growth fails",
            got.returncode == 1,
            f"exit {got.returncode}\n{got.stdout}",
        )

        # 13. --update is the documented answer to a legitimate change: it
        #     moves the value, preserves the tolerance, and makes the move a
        #     reviewable diff.
        work = make_case(
            root,
            "update-flow",
            [dict(SECTION_ENTRY, tolerance_pct=5)],
            [record("drawn_sections", 512, "sections")],
        )
        before = run_gate(work)
        upd = run_gate(work, "--update")
        after = run_gate(work)
        written = json.loads(
            (work / "bench-baselines" / "widget.json").read_text()
        )["entries"][0]
        check(
            "before --update the changed number is red",
            before.returncode == 1,
            f"exit {before.returncode}",
        )
        check(
            "--update rewrites the value and the gate goes green",
            upd.returncode == 0 and after.returncode == 0,
            f"update exit {upd.returncode}, after exit {after.returncode}\n{upd.stdout}{after.stdout}",
        )
        check(
            "--update preserves the tolerance rather than widening it",
            written["value"] == 512 and written["tolerance_pct"] == 5,
            json.dumps(written),
        )
        check(
            "--update reports the ratio it wrote, so a review can see the size of the move",
            "ratio" in upd.stdout,
            upd.stdout,
        )

        # 14. A recorder that wrote a null value must not crash the gate for
        #     every other metric in the same file. One real bench in this
        #     repository records `null` for a rate whose denominator was zero.
        work = root / "null-value"
        (work / "bench-baselines").mkdir(parents=True)
        (work / "bench-results").mkdir(parents=True)
        (work / "bench-baselines" / "widget.json").write_text(
            json.dumps(baseline_doc([SECTION_ENTRY]), indent=2) + "\n"
        )
        (work / "bench-results" / "widget.jsonl").write_text(
            jsonl(
                [
                    record("ns_per_draw", None, "ns"),
                    record("drawn_sections", 347, "sections"),
                ]
            )
            + "{ this line is not json\n"
        )
        got = run_gate(work)
        check(
            "a null value and a malformed line do not stop the other metrics",
            got.returncode == 0,
            f"exit {got.returncode}\n{got.stdout}{got.stderr}",
        )
    finally:
        shutil.rmtree(root, ignore_errors=True)

    print()
    if FAILURES:
        print(f"{PASSES} passed, {len(FAILURES)} FAILED")
        for line in FAILURES:
            print("  " + line)
        return 1
    print(f"{PASSES} checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
