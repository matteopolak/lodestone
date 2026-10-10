#!/usr/bin/env python3
"""Regenerates xtask/vanilla-class-names.txt from the reference sources.

Lists every class, interface, enum and record name in the decompiled reference
release (`mc-version`) that has at least two capitalised words, which is what
keeps ordinary prose words out. `cargo xtask check-comment-voice` fails on any
of these in a comment or doc unless this workspace defines a type of the same
name itself.
"""
import os
import re
import sys

root = os.path.join(os.path.dirname(__file__), "..")
version = open(os.path.join(root, "mc-version")).read().strip()
names = set()
for sub in ("src", "client-src"):
    base = os.path.join(root, ".cache", "mc", version, sub)
    for dirpath, _, files in os.walk(base):
        for f in files:
            if not f.endswith(".java"):
                continue
            names.add(f[:-5])
            text = open(os.path.join(dirpath, f), errors="ignore").read()
            names.update(re.findall(r"\b(?:class|interface|enum|record)\s+([A-Z]\w+)", text))
camel = re.compile(r"^[A-Z][a-z0-9]+(?:[A-Z][a-z0-9]*)+$")
# Wire-format and file-format vocabulary that is also a reference class name.
vocabulary = {"VarInt", "VarLong", "DataVersion", "GameType"}
kept = sorted(n for n in names if camel.match(n) and len(n) >= 6 and n not in vocabulary)
if not kept:
    sys.exit("no reference sources found; run the cache setup first")
with open(os.path.join(root, "xtask", "vanilla-class-names.txt"), "w") as out:
    out.write("\n".join(kept) + "\n")
print(f"wrote {len(kept)} names")
