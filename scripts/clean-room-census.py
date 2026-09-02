#!/usr/bin/env python3
"""Census of vanilla-code citations left in the tree.

Three pattern classes were each discovered by accident, after a sweep had
already reported "zero residual" using a narrower set. They are listed here so
the next census starts from the widest known net rather than rediscovering them:

  1. dotted        `Class.method`, `Class.CONSTANT`, and unbackticked `Foo.bar(`
  2. paths         `.java` filenames, and `net.minecraft` **and** `net/minecraft`
                   (the slash form slipped past every early sweep)
  3. bare names    a backticked identifier with no dot at all -- camelCase
                   (`updateBob`) or PascalCase (`CollectingNeighborUpdater`).
                   A plain regex for these is useless: it matches our own types
                   in their thousands.

Class 3 is why this is a script rather than a grep. The discriminator is
semantic: a name that appears **in comments but never anywhere in code** is
almost certainly foreign, because our own types are used as well as described.
That single rule turns an unusable 238-file regex into a ranked, actionable list.

Known false positives it will still surface, all legitimate keeps: third-party
API names we interoperate with but do not call in the file that mentions them
(wgpu texture formats, web platform interfaces, crypto structure names), and
our own types that only ever appear in prose. Read a hit before acting on it.

  4. mislabeled    a fenced block holding verbatim vanilla source under a tag
                   that is not `java` -- in practice ```text. Sixteen of these
                   were found in one crate *after* a sweep had confirmed zero
                   ```java blocks, so the tag is not trustworthy evidence about
                   the content. Detected structurally instead, by Java-shaped
                   syntax the surrounding languages do not produce.

Also reported: fenced ```java blocks, which contain verbatim vanilla source and
are the most severe class -- and invisible to every identifier pattern above.

Usage:  python3 scripts/clean-room-census.py [path ...]
"""

import collections
import pathlib
import re
import subprocess
import sys

DOTTED = re.compile(r"`[A-Z][A-Za-z0-9]*\.[a-zA-Z][A-Za-z0-9_]*|\b[A-Z][A-Za-z0-9]*\.[a-z][A-Za-z0-9]*\(")
PATHS = re.compile(r"\.java\b|net[./]minecraft")
FENCED = re.compile(r"```java")
FENCE_OPEN = re.compile(r"^\s*(?://[/!]?\s*)?```([A-Za-z0-9_-]*)\s*$")
# Syntax Rust, WGSL and prose do not produce. Any single one of these inside a
# fenced block is already conclusive, so the threshold is one -- the cost of a
# false positive is one file read, and the cost of a miss is verbatim source.
JAVA_SHAPED = [
    re.compile(p)
    for p in (
        r"\bpublic\s+(static\s+)?(final\s+)?[A-Za-z<>\[\]]+\s+\w+\s*\(",
        r"\bprivate\s+(static\s+)?[A-Za-z<>\[\]]+\s+\w+",
        r"@Override",
        r"\bthis\.\w+\s*=",
        r"\bnew\s+[A-Z]\w+\s*\(",
        r"\bvoid\s+\w+\s*\([A-Za-z]",
        r"\binstanceof\b",
    )
]


def mislabeled_fences(text):
    """Fenced blocks whose body looks like Java but whose tag does not say so."""
    found, tag, body = 0, None, []
    for line in text.splitlines():
        m = FENCE_OPEN.match(line)
        if not m:
            if tag is not None:
                body.append(line)
            continue
        if tag is None:
            tag, body = m.group(1) or "", []
            continue
        joined = "\n".join(body)
        if tag.lower() != "java" and any(r.search(joined) for r in JAVA_SHAPED):
            found += 1
        tag = None
    return found
BACKTICKED = re.compile(r"`([A-Za-z][A-Za-z0-9]{3,})`")
IDENT = re.compile(r"\b([A-Za-z][A-Za-z0-9]{3,})\b")


def tracked(roots):
    out = subprocess.run(
        ["git", "ls-files", "*.rs", "*.wgsl", "*.md"], capture_output=True, text=True
    ).stdout.split()
    if not roots:
        return out
    return [f for f in out if any(f.startswith(r.rstrip("/")) for r in roots)]


def main():
    files = tracked(sys.argv[1:])
    dotted, paths, fenced, mislabeled = set(), set(), set(), set()
    in_code = collections.Counter()
    in_comment = collections.defaultdict(set)

    for name in files:
        try:
            text = pathlib.Path(name).read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        if DOTTED.search(text):
            dotted.add(name)
        if PATHS.search(text):
            paths.add(name)
        if FENCED.search(text):
            fenced.add(name)
        if mislabeled_fences(text):
            mislabeled.add(name)
        for line in text.splitlines():
            stripped = line.lstrip()
            comment = stripped.startswith(("//", "///", "//!", "#")) or name.endswith(".md")
            if comment:
                for m in BACKTICKED.finditer(line):
                    in_comment[m.group(1)].add(name)
            else:
                for m in IDENT.finditer(line.split("//")[0]):
                    in_code[m.group(1)] += 1

    bare = {n: f for n, f in in_comment.items() if in_code[n] == 0}
    bare_files = set().union(*bare.values()) if bare else set()

    print(f"scanned {len(files)} tracked files")
    print(f"  fenced java blocks   {len(fenced):5} files   (most severe: verbatim source)")
    print(f"  mislabeled fences    {len(mislabeled):5} files   (verbatim source under a non-java tag)")
    print(f"  .java / net.minecraft{len(paths):5} files")
    print(f"  dotted Class.method  {len(dotted):5} files")
    print(f"  bare-name candidates {len(bare_files):5} files, {len(bare)} distinct names")
    print(f"  UNION                {len(fenced | mislabeled | paths | dotted | bare_files):5} files")

    if mislabeled:
        print("\nmislabeled fenced blocks (severe, and small enough to list):")
        for name in sorted(mislabeled):
            print(f"  {name}")

    if bare:
        print("\ntop bare-name candidates by file spread (read before acting):")
        for n, f in sorted(bare.items(), key=lambda kv: -len(kv[1]))[:20]:
            print(f"  {len(f):3} files  {n}")


if __name__ == "__main__":
    main()
