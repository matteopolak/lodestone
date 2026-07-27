#!/usr/bin/env python3
"""Diff the Java movement oracle's per-tick bit patterns against the checked-in
golden traces (`tests/support/golden_traces.rs`). Reports the exact scenario,
tick, and component of the first divergence, or confirms bit-for-bit agreement.

The golden traces are what the Rust crate is asserted against (golden.rs), so
Java == golden implies Java == Rust: a real-JVM cross-check of the float/double
arithmetic, not a second copy of the Python oracle.
"""
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
GOLDEN = HERE.parent / "tests" / "support" / "golden_traces.rs"
JAVA = HERE / "move_java.txt"

COMPONENTS = ["pos.x", "pos.y", "pos.z", "vel.x", "vel.y", "vel.z"]


def load_golden():
    text = GOLDEN.read_text()
    scenarios = {}
    for m in re.finditer(r"pub static GOLDEN_(\w+):\s*\[GoldenTick;\s*\d+\]\s*=\s*\[(.*?)\];", text, re.S):
        name = m.group(1).lower()
        ticks = []
        for tm in re.finditer(r"pos:\s*\[([^\]]*)\],\s*vel:\s*\[([^\]]*)\]", m.group(2)):
            pos = [int(x, 16) for x in re.findall(r"0x[0-9a-fA-F]+", tm.group(1))]
            vel = [int(x, 16) for x in re.findall(r"0x[0-9a-fA-F]+", tm.group(2))]
            ticks.append(pos + vel)
        scenarios[name] = ticks
    return scenarios


def load_java():
    scenarios = {}
    name = None
    for line in JAVA.read_text().splitlines():
        if line.startswith("SCENARIO"):
            _, name, _count = line.split()
            scenarios[name] = []
        elif name is not None and line.strip():
            scenarios[name].append([int(t) for t in line.split()])
    return scenarios


def main():
    golden = load_golden()
    java = load_java()
    ok = True
    checked = 0
    for name, jticks in java.items():
        if name not in golden:
            print(f"NOTE: scenario '{name}' has no golden trace (Java-only); skipped.")
            continue
        gticks = golden[name]
        if len(gticks) != len(jticks):
            print(f"FAIL {name}: length golden={len(gticks)} java={len(jticks)}")
            ok = False
            continue
        diverged = False
        for t, (g, j) in enumerate(zip(gticks, jticks)):
            for ci, (gv, jv) in enumerate(zip(g, j)):
                if gv != jv:
                    print(f"FAIL {name} tick {t} {COMPONENTS[ci]}: golden={gv} java={jv}")
                    ok = False
                    diverged = True
                    break
            if diverged:
                break
        if not diverged:
            checked += 1
            print(f"PASS {name}: {len(jticks)} ticks bit-identical (JVM == golden == Rust).")
    if not ok:
        sys.exit(1)
    print(f"\nAll {checked} shared scenarios agree bit-for-bit with the real JVM.")


if __name__ == "__main__":
    main()
