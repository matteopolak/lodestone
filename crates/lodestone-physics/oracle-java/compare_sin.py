#!/usr/bin/env python3
"""Element-wise diff of the Java-produced sin table vs the checked-in Rust table.

Stronger than any hash: reports the exact index of the first divergence, or
confirms all 65,536 entries are bit-identical.
"""
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUST = HERE.parent / "src" / "sin_table.rs"
JAVA = HERE / "sin_java.txt"


def load_rust():
    text = RUST.read_text()
    # Take only the array body between `[` after SIN_TABLE_BITS and the closing `]`.
    start = text.index("SIN_TABLE_BITS")
    array_start = text.index("= [", start) + len("= [")
    body = text[array_start : text.index("];", array_start)]
    return [int(tok) for tok in re.findall(r"\d+", body)]


def load_java():
    return [int(line) for line in JAVA.read_text().split()]


def main():
    rust = load_rust()
    java = load_java()
    if len(rust) != 65536:
        sys.exit(f"FAIL: Rust table has {len(rust)} entries, expected 65536")
    if len(java) != 65536:
        sys.exit(f"FAIL: Java table has {len(java)} entries, expected 65536")
    mismatches = [(i, r, j) for i, (r, j) in enumerate(zip(rust, java)) if r != j]
    if not mismatches:
        print("PASS: all 65536 float bit patterns identical (Java JVM == Rust f32).")
        return
    print(f"FAIL: {len(mismatches)} mismatching entries. First 5:")
    for i, r, j in mismatches[:5]:
        print(f"  index {i}: rust={r} java={j}")
    sys.exit(1)


if __name__ == "__main__":
    main()
