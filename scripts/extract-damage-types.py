#!/usr/bin/env python3
"""Extract vanilla 26.2's damage-type registry + tags VERBATIM from the server jar.

Writes the committed anchor dump for crates/lodestone-data/tests/support/.
No interpretation: each entry is the datapack JSON exactly as it ships, so the
Rust side is what gets tested, not this script's reading of the data.
"""
import sys
import zipfile

JAR = sys.argv[1]
OUT = sys.argv[2]

TYPE_PREFIX = "data/minecraft/damage_type/"
TAG_PREFIX = "data/minecraft/tags/damage_type/"

with zipfile.ZipFile(JAR) as z:
    names = [n for n in z.namelist() if n.endswith(".json")]
    types = sorted(n for n in names if n.startswith(TYPE_PREFIX))
    tags = sorted(n for n in names if n.startswith(TAG_PREFIX))

    out = []
    out.append("# Vanilla Minecraft 26.2 damage-type registry, VERBATIM datapack JSON.")
    out.append("#")
    out.append("# Provenance: extracted with `unzip` from the real server jar's embedded")
    out.append("# vanilla datapack --")
    out.append("#   .cache/mc/26.2/versions/26.2/server-26.2.jar")
    out.append("# (note the OUTER .cache/mc/26.2/server.jar is a *bundler* and contains")
    out.append("# none of these paths -- searching it returns zero hits).")
    out.append("#")
    out.append("# These are the game's own data files, not a program's reading of them, so")
    out.append("# this dump needs no JVM boot: it is a strictly more direct anchor than the")
    out.append("# registry-walking oracles next to it (HardnessOracle and friends exist")
    out.append("# because hardness/collision have NO datapack representation at all).")
    out.append("#")
    out.append("# Regenerate with `just regen-damage-types` (see the recipe for the exact")
    out.append("# unzip invocation). Entry bodies are byte-for-byte as shipped.")
    out.append("#")
    out.append(f"# {len(types)} damage types, {len(tags)} tags.")
    out.append("")

    for n in types + tags:
        body = z.read(n).decode("utf-8")
        rel = n[len("data/minecraft/") :]
        out.append(f">>> {rel}")
        out.append(body.rstrip("\n"))

with open(OUT, "w") as f:
    f.write("\n".join(out) + "\n")

print(f"wrote {OUT}: {len(types)} types, {len(tags)} tags")
