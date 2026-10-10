#!/usr/bin/env python3
"""Extract each entity type's pathfinding-malus overrides from the decompiled reference.

For every spawnable entity type this resolves its class chain, takes the
overrides its constructors set (a subclass overriding its parent's wins), and
writes `crates/lodestone-entity/data/path_malus.json`: species path -> {path
type variant -> cost}. Overrides set later in a method (a mob that changes them
mid-life) are not constructors and are not included.

Regenerate with `just regen-path-malus`.
"""
import json
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from mc_version import ROOT, cache_root  # noqa: E402

SRC = cache_root() / "src"
OUT = ROOT / "crates" / "lodestone-entity" / "data" / "path_malus.json"


def pascal(screaming: str) -> str:
    return "".join(part.capitalize() for part in screaming.split("_"))


classes = {}
for dirpath, _, files in os.walk(SRC / "net" / "minecraft"):
    for name in files:
        if name.endswith(".java"):
            path = Path(dirpath) / name
            text = path.read_text(errors="ignore")
            for m in re.finditer(r"\bclass\s+(\w+)(?:<[^{]*?>)?\s+extends\s+([\w.]+)", text):
                classes.setdefault(m.group(1), (path, m.group(2).split(".")[-1]))


def constructor_overrides(cls: str) -> dict:
    path, _ = classes[cls]
    text = path.read_text(errors="ignore")
    found = {}
    for m in re.finditer(r"\b%s\s*\([^)]*\)\s*\{" % cls, text):
        depth, i = 1, m.end()
        while depth and i < len(text):
            depth += (text[i] == "{") - (text[i] == "}")
            i += 1
        body = text[m.end() : i]
        for kind, cost in re.findall(
            r"setPathfindingMalus\(\s*(?:PathType\.)?(\w+)\s*,\s*(-?[\d.]+)F?\)", body
        ):
            found[pascal(kind)] = float(cost)
    return found


def chain_overrides(cls: str) -> dict:
    chain = []
    while cls in classes:
        chain.append(cls)
        cls = classes[cls][1]
    merged = {}
    for link in reversed(chain):
        merged.update(constructor_overrides(link))
    return merged


registry = (SRC / "net/minecraft/world/entity/EntityTypes.java").read_text()
table = {}
for const, cls in re.findall(
    r"EntityType<\w+>\s+(\w+)\s*=\s*register\(\s*EntityTypeIds\.\w+,\s*EntityType\.Builder\.of\(\s*(\w+)::new",
    registry,
):
    if cls in classes:
        overrides = chain_overrides(cls)
        if overrides:
            table[const.lower()] = dict(sorted(overrides.items()))

OUT.write_text(json.dumps(dict(sorted(table.items())), indent=1) + "\n")
print(f"{len(table)} species -> {OUT}")
