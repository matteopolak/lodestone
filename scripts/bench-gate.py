#!/usr/bin/env python3
"""Compare freshly recorded bench metrics against a committed baseline.

# What this is

`bench-results/*.jsonl` is local, gitignored measurement data: one JSON object
per metric per run, written by each crate's `benches/support.rs` `record()`.
`cargo xtask bench-compare` reads two entries out of one such file and reports a
ratio, which answers "did the run I just took differ from the run before it on
this machine". Neither of those is a baseline: nothing in the repository records
what a metric is *supposed* to be, so nothing notices when it changes on a
machine that has no history, which is every CI runner and every fresh checkout.

This script is that missing half. `bench-baselines/<bench>.json` is committed
repository state; this compares the newest recorded value for each baselined
`(scene, metric)` against it and exits non-zero on drift outside that entry's
tolerance band.

# Why only counts, and never a wall-clock number

A baseline of a duration is a wall-clock ceiling wearing a different hat, and a
wall-clock ceiling is the wrong shape for a gate. The worked example in this
repository: a light test asserted `hood_best < 200.0` while its own comment
named the printed *ratio* as the deliverable. A 3x3 neighbourhood is nine
columns and the measured factor was ~8.7x -- essentially linear and perfectly
healthy -- yet that ceiling silently asserted `single_best < 22.2 ms`, an
undocumented constraint on how fast the machine had to be, and it failed under
load.

So a baselined metric must be *deterministic*: a count of things the program
did, a byte total, or a fraction of a fixed capacity. Those do not move when the
machine is busy, and they do not move between an M-series laptop and a Linux
runner. `ALLOWED_UNITS` below is that rule made structural rather than
documented: `--update` refuses to write an entry whose unit measures time, so a
timing cannot enter a baseline by accident later.

Durations keep the treatment they already have -- recorded to the JSONL,
compared by `bench-compare` against the same machine's own history, advisory,
never asserted.

# Would this shape have caught the frame-time regression that motivated it?

The regression was a render path rewriting one camera uniform per resident
section per frame -- roughly 4000 buffer writes and 4000 bind-group binds per
frame where one of each was needed. Median frame time went 17.05 ms -> 8.19 ms
when it was fixed. That regression is *count-shaped*: the honest gate is
"per-frame bind-group binds do not grow with resident section count", which is
exactly a deterministic count and exactly what this script compares. A timing
gate would have needed a ceiling somewhere between 8.19 and 17.05 ms on one
specific machine, which is the trap above.

# Exit status

    0  every compared metric inside its band, and enough were compared
    1  at least one metric outside its band, or a required metric absent
    2  the gate did not really run (no results, or fewer comparisons than
       --min-compared) -- an audit that prints nothing is a failure to run,
       not a pass

Never read this through a pipeline. `cargo test --workspace | grep ... | tail`
once reported success while cargo returned 101; check the status directly.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import sys
from pathlib import Path

# Units a baseline entry may carry: a count of events, a byte total, a count of
# fixed-size things, or a fraction of a fixed capacity. Every one of these is a
# property of what the program did, not of how fast the machine did it.
ALLOWED_UNITS = frozenset(
    {
        "calls",
        "draws",
        "draw_calls",
        "quads",
        "sections",
        "slots",
        "batches",
        "buffers",
        "sprites",
        "entities",
        "allocs",
        "bytes",
        "px",
        "fraction",
        "count",
        "columns",
    }
)

# Units that make a metric a wall-clock measurement, listed explicitly so the
# refusal message can say *which* rule was hit rather than "not in the allowed
# set". A unit in neither list is still refused -- the allowed set is the
# authority -- but one in this list gets the specific explanation.
TIME_UNITS = frozenset({"ns", "us", "µs", "ms", "s", "ms/s"})

DEFAULT_RESULTS_DIR = "bench-results"
DEFAULT_BASELINE_DIR = "bench-baselines"

# A gate that compared nothing is not green, it is unrun. The default is 1
# rather than 0 so the failure mode of "the bench did not write its JSONL" is a
# red gate instead of a silent pass; a caller gating a known set raises it.
DEFAULT_MIN_COMPARED = 1


class GateError(Exception):
    """A condition that makes the comparison meaningless rather than failed."""


def is_finite_number(value: object) -> bool:
    """Whether a JSON number can take part in an ordinary numeric comparison."""
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        return False
    try:
        return math.isfinite(float(value))
    except OverflowError:
        return False


def repo_root() -> Path:
    """The directory holding this script's parent, i.e. the checkout root."""
    return Path(__file__).resolve().parent.parent


def load_results(path: Path) -> dict[tuple[str, str], dict]:
    """Newest recorded object per `(scene, metric)` in one JSONL file.

    Later lines win: the file is append-only, so the last line for a key is the
    most recent run that recorded it. Lines that do not parse, or that carry a
    null/non-numeric value, are skipped rather than crashing the gate -- a
    recorder that wrote `null` for a metric with no samples should not make an
    unrelated metric's comparison impossible. A numeric non-finite value is
    different: JSON parsers commonly accept `NaN` and `Infinity` extensions,
    but either makes a comparison meaningless, so it rejects the whole gate
    input instead of falling back to an older value for that metric.
    """
    newest: dict[tuple[str, str], dict] = {}
    if not path.exists():
        return newest
    for line_number, line in enumerate(
        path.read_text(encoding="utf-8", errors="replace").splitlines(), start=1
    ):
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        scene = obj.get("scene")
        metric = obj.get("metric")
        value = obj.get("value")
        if not isinstance(scene, str) or not isinstance(metric, str):
            continue
        if not isinstance(value, (int, float)) or isinstance(value, bool):
            continue
        if not is_finite_number(value):
            raise GateError(
                f"{path}:{line_number}: metric {metric!r} has a non-finite "
                f"value {value!r}; a gate cannot compare NaN or infinity"
            )
        newest[(scene, metric)] = obj
    return newest


def load_baseline(path: Path) -> dict:
    with path.open(encoding="utf-8") as handle:
        doc = json.load(handle)
    if not isinstance(doc, dict) or not isinstance(doc.get("entries"), list):
        raise GateError(f"{path}: expected an object with an 'entries' list")
    return doc


def validate_entries(path: Path, entries: list[object]) -> None:
    """Reject malformed, non-finite, or duplicate baseline entries.

    A duplicate `(scene, metric)` is not two observations: `load_results`
    deliberately collapses that key to its newest recorded value. Accepting
    duplicate baseline entries would therefore let one observed metric satisfy
    `--min-compared` more than once and make the coverage count dishonest.
    """
    seen: dict[tuple[str, str], int] = {}
    for index, entry in enumerate(entries):
        if not isinstance(entry, dict):
            raise GateError(f"{path}: baseline entry #{index} must be an object")
        for field in ("metric", "scene", "unit", "value"):
            if field not in entry:
                raise GateError(
                    f"{path}: an entry is missing required field {field!r}"
                )
        metric = entry["metric"]
        scene = entry["scene"]
        if not isinstance(metric, str) or not isinstance(scene, str):
            raise GateError(
                f"{path}: baseline entry #{index} requires string 'metric' and 'scene'"
            )
        unit = entry["unit"]
        if not isinstance(unit, str):
            raise GateError(
                f"{path}: baseline entry #{index} requires string 'unit'"
            )
        value = entry["value"]
        if not is_finite_number(value):
            raise GateError(
                f"{path}: baseline entry #{index} requires a finite numeric 'value'"
            )
        tolerance = entry.get("tolerance_pct", 0.0)
        if not is_finite_number(tolerance) or tolerance < 0:
            raise GateError(
                f"{path}: baseline entry #{index} requires a finite, non-negative "
                "numeric 'tolerance_pct'"
            )
        if "required" in entry and not isinstance(entry["required"], bool):
            raise GateError(
                f"{path}: baseline entry #{index} requires boolean 'required'"
            )
        key = (scene, metric)
        previous = seen.get(key)
        if previous is not None:
            raise GateError(
                f"{path}: duplicate baseline key (scene={scene!r}, metric={metric!r}) "
                f"in entries #{previous} and #{index}; each metric must be listed once"
            )
        seen[key] = index


def load_baseline_set(
    baseline_dir: Path, only: str | None
) -> list[tuple[Path, dict, str]]:
    """Load and structurally validate the selected baselines as one set.

    A baseline file may override its filename with the ``bench`` field. Two
    files naming the same bench would then read the same results log and make
    one observed metric count twice toward ``--min-compared``. Keep that
    identity unique before either the gate or ``--update`` uses the set.
    """
    paths = sorted(baseline_dir.glob("*.json"))
    if only:
        paths = [p for p in paths if p.stem == only]
        if not paths:
            raise GateError(f"no baseline named {only!r} under {baseline_dir}")
    if not paths:
        raise GateError(f"no baseline files under {baseline_dir}")

    loaded: list[tuple[Path, dict, str]] = []
    seen: dict[str, Path] = {}
    for path in paths:
        doc = load_baseline(path)
        validate_entries(path, doc["entries"])
        bench = doc.get("bench", path.stem)
        if not isinstance(bench, str) or not bench:
            raise GateError(
                f"{path}: baseline 'bench' must be a non-empty string"
            )
        previous = seen.get(bench)
        if previous is not None:
            raise GateError(
                f"{path}: duplicate baseline bench {bench!r}; also declared by "
                f"{previous}. Each bench must have one baseline file so one "
                "results log cannot satisfy coverage twice"
            )
        seen[bench] = path
        loaded.append((path, doc, bench))
    return loaded


def check_unit(entry: dict, where: str) -> None:
    unit = entry.get("unit")
    if unit in ALLOWED_UNITS:
        return
    metric = entry.get("metric", "<unnamed>")
    if unit in TIME_UNITS:
        raise GateError(
            f"{where}: metric {metric!r} has unit {unit!r}, which measures time. "
            "A baselined duration is a wall-clock ceiling: it encodes how fast "
            "the machine that recorded it happened to be, and goes red under "
            "load on healthy code. Record it to bench-results/ and compare it "
            "with `cargo xtask bench-compare` against the same machine's own "
            "history instead."
        )
    raise GateError(
        f"{where}: metric {metric!r} has unit {unit!r}, which is not in the "
        f"deterministic set {sorted(ALLOWED_UNITS)}. Only counts, byte totals "
        "and fractions of a fixed capacity are comparable across machines."
    )


def compare_one(entry: dict, observed: dict | None) -> tuple[str, str]:
    """Returns `(status, human-readable line)` for one baseline entry.

    Status is one of `ok`, `fail`, `skip`. `skip` means the metric was not
    recorded at all and the entry does not require it -- the GPU-dependent
    metrics behave this way on a runner with no adapter.
    """
    metric = entry["metric"]
    scene = entry["scene"]
    unit = entry["unit"]
    expected = float(entry["value"])
    tolerance = float(entry.get("tolerance_pct", 0.0))
    required = bool(entry.get("required", True))
    label = f"{metric} [{scene}]"

    if observed is None:
        if required:
            return (
                "fail",
                f"MISSING  {label}: baseline expects {expected:g}{unit} but no "
                "run recorded it. A required metric that stopped being recorded "
                "is a broken bench, not a pass. Mark the entry "
                '"required": false if its measurement genuinely cannot run here.',
            )
        return (
            "skip",
            f"SKIP     {label}: not recorded in this run; entry is optional "
            f"({entry.get('optional_because', 'no reason recorded')}).",
        )

    actual = float(observed["value"])
    observed_unit = observed.get("unit")
    if observed_unit != unit:
        return (
            "fail",
            f"UNIT     {label}: baseline unit {unit!r} but the run recorded "
            f"{observed_unit!r}. The metric changed meaning; re-baseline "
            "deliberately rather than comparing two different quantities.",
        )

    if expected == 0.0:
        # A ratio against zero is undefined, and a zero baseline is a real,
        # meaningful value here (a healthy leak probe records exactly 0 bytes
        # of growth). Compare on the absolute difference against the tolerance
        # read as an absolute allowance in the metric's own unit.
        delta = abs(actual - expected)
        allowance = tolerance
        verdict = "ok" if delta <= allowance else "fail"
        return (
            verdict,
            f"{verdict.upper():8s} {label}: expected {expected:g}{unit}, got "
            f"{actual:g}{unit} (absolute allowance {allowance:g}{unit}) "
            f"@ {observed.get('git_sha', '?')} on {observed.get('machine', '?')}",
        )

    ratio = actual / expected
    lo = 1.0 - tolerance / 100.0
    hi = 1.0 + tolerance / 100.0
    verdict = "ok" if lo <= ratio <= hi else "fail"
    direction = "" if verdict == "ok" else ("  (higher)" if ratio > 1.0 else "  (lower)")
    return (
        verdict,
        f"{verdict.upper():8s} {label}: expected {expected:g}{unit}, got "
        f"{actual:g}{unit}, ratio {ratio:.4f} (band {lo:.4f}..{hi:.4f})"
        f"{direction} @ {observed.get('git_sha', '?')} on "
        f"{observed.get('machine', '?')}",
    )


def run_gate(
    baseline_dir: Path,
    results_dir: Path,
    only: str | None,
    min_compared: int,
    out=sys.stdout,
) -> int:
    baselines = load_baseline_set(baseline_dir, only)

    compared = 0
    failures: list[str] = []
    lines: list[str] = []
    unrun: list[str] = []

    for path, doc, bench in baselines:
        observed = load_results(results_dir / f"{bench}.jsonl")
        lines.append(f"--- {bench} ({len(doc['entries'])} baselined) ---")
        # A bench whose log is absent or carries no usable record did not run
        # at all. That is a different fact from "a metric this bench used to
        # record has disappeared", and collapsing the two would report a
        # never-invoked bench as a regression in every metric it owns --
        # loud, but pointing at the wrong thing.
        if not observed:
            unrun.append(bench)
            lines.append(
                f"  NORUN    {bench}: {results_dir / f'{bench}.jsonl'} is absent "
                "or holds no usable record, so none of its baselined metrics "
                "could be compared. Run the bench before the gate."
            )
            continue
        for entry in doc["entries"]:
            check_unit(entry, str(path))
            status, line = compare_one(
                entry, observed.get((entry["scene"], entry["metric"]))
            )
            lines.append("  " + line)
            if status == "fail":
                failures.append(line)
            elif status == "ok":
                compared += 1

    for line in lines:
        print(line, file=out)

    print(file=out)
    print(
        f"compared {compared} metric(s) inside band, {len(failures)} failing",
        file=out,
    )

    if unrun:
        print(
            f"\nGATE DID NOT RUN for {len(unrun)} bench(es): {', '.join(unrun)}. "
            "Their measurement logs are absent or empty, so their baselined "
            "metrics were not checked at all -- treat this as red, not green.",
            file=out,
        )
        return 2

    if failures:
        print(file=out)
        print("FAILED:", file=out)
        for line in failures:
            print("  " + line, file=out)
        print(file=out)
        print(
            "If a change deliberately moved one of these, re-baseline it in the "
            "same commit: `python3 scripts/bench-gate.py --update` and commit "
            "the bench-baselines/ diff alongside the code. The number moving is "
            "not the problem; the number moving without anyone saying so is.",
            file=out,
        )
        return 1

    if compared < min_compared:
        print(
            f"\nGATE DID NOT RUN: {compared} metric(s) compared, "
            f"--min-compared is {min_compared}. Nothing failed because almost "
            "nothing was checked -- treat this as red, not green. The usual "
            f"cause is that no bench wrote {results_dir}/ before the gate ran.",
            file=out,
        )
        return 2

    return 0


def run_update(
    baseline_dir: Path, results_dir: Path, only: str | None, out=sys.stdout
) -> int:
    """Rewrite each baseline's `value` from the newest recorded run.

    Tolerances, `required` flags and every other annotation are preserved: this
    updates what the number *is*, never how strictly it is checked. Entries with
    no recorded value are left exactly as they are, so running `--update` after
    a partial bench run cannot silently drop coverage.
    """
    baselines = load_baseline_set(baseline_dir, only)

    touched = 0
    for path, doc, bench in baselines:
        observed = load_results(results_dir / f"{bench}.jsonl")
        changed = False
        for entry in doc["entries"]:
            check_unit(entry, str(path))
            found = observed.get((entry["scene"], entry["metric"]))
            if found is None:
                print(
                    f"  keep    {entry['metric']} [{entry['scene']}]: not "
                    "recorded in this run, left unchanged",
                    file=out,
                )
                continue
            old = float(entry["value"])
            new = float(found["value"])
            if old != new:
                entry["value"] = new
                entry["recorded_sha"] = found.get("git_sha", "unknown")
                changed = True
                delta = "" if old == 0 else f", ratio {new / old:.4f}"
                print(
                    f"  update  {entry['metric']} [{entry['scene']}]: "
                    f"{old:g} -> {new:g}{delta}",
                    file=out,
                )
        if changed:
            path.write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
            touched += 1
            print(f"wrote {path}", file=out)
    print(f"\n{touched} baseline file(s) updated", file=out)
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Compare recorded bench metrics against the committed baseline "
            "under bench-baselines/."
        ),
        epilog=(
            "Exit 0 = inside band; 1 = drift or a required metric absent; "
            "2 = the gate did not really run. Check the status directly, never "
            "through a pipe."
        ),
    )
    parser.add_argument(
        "--results-dir",
        default=None,
        help=f"directory of *.jsonl measurement logs (default: {DEFAULT_RESULTS_DIR})",
    )
    parser.add_argument(
        "--baseline-dir",
        default=None,
        help=f"directory of committed *.json baselines (default: {DEFAULT_BASELINE_DIR})",
    )
    parser.add_argument(
        "--only", default=None, help="restrict to one baseline file, by stem"
    )
    parser.add_argument(
        "--min-compared",
        type=int,
        default=DEFAULT_MIN_COMPARED,
        help=(
            "fail with status 2 if fewer than this many metrics were actually "
            f"compared (default {DEFAULT_MIN_COMPARED}); an audit that checks "
            "nothing is unrun, not green"
        ),
    )
    parser.add_argument(
        "--update",
        action="store_true",
        help=(
            "rewrite baseline values from the newest recorded run, preserving "
            "tolerances and flags -- then commit the diff alongside the change "
            "that moved the number"
        ),
    )
    args = parser.parse_args(argv)

    root = repo_root()
    results_dir = Path(
        args.results_dir
        or os.environ.get("LODESTONE_BENCH_RESULTS")
        or root / DEFAULT_RESULTS_DIR
    )
    baseline_dir = Path(
        args.baseline_dir
        or os.environ.get("LODESTONE_BENCH_BASELINES")
        or root / DEFAULT_BASELINE_DIR
    )

    try:
        if args.update:
            return run_update(baseline_dir, results_dir, args.only)
        return run_gate(baseline_dir, results_dir, args.only, args.min_compared)
    except GateError as exc:
        print(f"bench-gate: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
