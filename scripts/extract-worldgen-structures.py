#!/usr/bin/env python3
"""Extract vanilla 26.2's structure corpus VERBATIM from the server jar.

Writes the bundled asset tree under `crates/lodestone-server/assets/` plus the
jar-derived SHA-256 manifest that is the drift gate's external anchor.

No interpretation happens here. Every file is a byte copy out of the zip -- the
JSON is never parsed and re-serialized, so what lands in the bundle is what
Mojang shipped, and the manifest hashes are hashes of the *jar's* bytes rather
than of anything this script composed. That is the whole point: five hand-written
tables in this repo have been wrong, so the only acceptable origin is data
source #1.

Two jars exist and only one has this data: the outer `.cache/mc/26.2/server.jar`
is a *bundler* wrapper and contains none of these paths, so searching it looks
exactly like "this version ships no structure data". Always point at
`.cache/mc/26.2/versions/26.2/server-26.2.jar`.

Usage:
    python3 scripts/extract-worldgen-structures.py [JAR] [ASSETS_DIR] [MANIFEST]

Defaults resolve to the cached 26.2 jar, `crates/lodestone-server/assets/` and
`crates/lodestone-server/tests/support/worldgen_structure_corpus.txt`.
"""

import hashlib
import pathlib
import sys
import zipfile

REPO = pathlib.Path(__file__).resolve().parent.parent

JAR = pathlib.Path(
    sys.argv[1] if len(sys.argv) > 1 else REPO / ".cache/mc/26.2/versions/26.2/server-26.2.jar"
)
ASSETS = pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else REPO / "crates/lodestone-server/assets")
MANIFEST = pathlib.Path(
    sys.argv[3]
    if len(sys.argv) > 3
    else REPO / "crates/lodestone-server/tests/support/worldgen_structure_corpus.txt"
)

# Whole `data/minecraft/worldgen/<name>/` registries taken in full. Each maps to
# `assets/worldgen/<name>/`, which `build.rs` already walks for `*.json`.
FULL_REGISTRIES = [
    "structure",
    "structure_set",
    "template_pool",
    "processor_list",
    "world_preset",
    "flat_level_generator_preset",
]

# `noise_settings` is SHARED with the concurrent Nether/End unit, which owns
# `nether.json` and `end.json`. This phase takes only the four that are neither:
# amplified and large_biomes are pure-data overworld presets over the existing
# engine, caves and floating_islands are the two the superflat/void and
# custom-preset paths reference. Listing them explicitly (rather than "all but
# two") means adding a dimension upstream does not silently pull a sibling's file
# into this phase's commit.
NOISE_SETTINGS = ["amplified", "caves", "floating_islands", "large_biomes"]

# The worldgen TAG tree, `data/minecraft/tags/worldgen/` -> `assets/worldgen/tags/worldgen/`.
#
# The doubled "worldgen" is deliberate: the bundle already keys block tags as
# `tags/block/<name>` (i.e. the jar path with `data/minecraft/` stripped), and
# `build.rs` turns the path under `assets/worldgen/` straight into the lookup id.
# Mirroring the jar is worth more than a prettier path.
#
# These are NOT optional trim, and the plan's inventory table does not name them.
# Every one of the 34 structures states its `biomes` field as a tag reference
# (`"#minecraft:has_structure/village_plains"`) -- zero inline biome lists -- so
# without `tags/worldgen/biome/has_structure/*` no structure's biome filter can
# resolve and placement stays blocked on data. The 20 `tags/worldgen/structure/*`
# entries are what `structure_overrides` in the world and flat presets point at.
# 92 files, 15,188 bytes.
TAG_PREFIX = "data/minecraft/tags/worldgen/"

# The NBT structure templates, from `data/minecraft/structure/` -> `assets/structure/`.
# NOT under `assets/worldgen/`: they mirror the jar's own layout, which puts them
# in a sibling registry, and `build.rs`'s worldgen table is JSON-only anyway.
#
# The full set is required. Template pools reference 989 distinct templates but
# 1212 exist: `end_city/*`, `ancient_city/city_center/*` and others are named
# from Java code (`EndCityPieces`, etc.), never from a pool. Deriving the set
# from pool references would omit 224 files and the omission would only surface
# when a structure failed to place.
NBT_PREFIX = "data/minecraft/structure/"


def main() -> int:
    if not JAR.is_file():
        print(f"error: jar not found: {JAR}", file=sys.stderr)
        print("  (the outer server.jar is a bundler -- use versions/26.2/server-26.2.jar)", file=sys.stderr)
        return 1

    # (asset-relative path, jar entry name)
    plan: list[tuple[str, str]] = []
    with zipfile.ZipFile(JAR) as z:
        names = set(z.namelist())

        for reg in FULL_REGISTRIES:
            prefix = f"data/minecraft/worldgen/{reg}/"
            found = sorted(n for n in names if n.startswith(prefix) and n.endswith(".json"))
            if not found:
                print(f"error: no entries under {prefix}", file=sys.stderr)
                return 1
            for n in found:
                plan.append((f"worldgen/{reg}/{n[len(prefix):]}", n))

        for stem in NOISE_SETTINGS:
            n = f"data/minecraft/worldgen/noise_settings/{stem}.json"
            if n not in names:
                print(f"error: missing {n}", file=sys.stderr)
                return 1
            plan.append((f"worldgen/noise_settings/{stem}.json", n))

        tags = sorted(n for n in names if n.startswith(TAG_PREFIX) and n.endswith(".json"))
        if not tags:
            print(f"error: no entries under {TAG_PREFIX}", file=sys.stderr)
            return 1
        for n in tags:
            plan.append((f"worldgen/tags/worldgen/{n[len(TAG_PREFIX):]}", n))

        nbt = sorted(n for n in names if n.startswith(NBT_PREFIX) and n.endswith(".nbt"))
        if not nbt:
            print(f"error: no entries under {NBT_PREFIX}", file=sys.stderr)
            return 1
        for n in nbt:
            plan.append((f"structure/{n[len(NBT_PREFIX):]}", n))

        rows: list[tuple[str, str, int]] = []
        total = 0
        for rel, entry in plan:
            body = z.read(entry)
            dest = ASSETS / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(body)
            rows.append((rel, hashlib.sha256(body).hexdigest(), len(body)))
            total += len(body)

    rows.sort()
    counts: dict[str, int] = {}
    for rel, _, _ in rows:
        parts = rel.split("/")
        if parts[0] == "worldgen" and parts[1] == "tags":
            # One bucket per tagged registry, so the enumeration stays checkable
            # per-registry rather than as one opaque "92 tags".
            key = "/".join(parts[:4])
        elif parts[0] == "worldgen":
            key = f"{parts[0]}/{parts[1]}"
        else:
            key = parts[0]
        counts[key] = counts.get(key, 0) + 1

    out = [
        "# Vanilla Minecraft 26.2 structure corpus -- jar-derived SHA-256 manifest.",
        "#",
        "# Provenance: scripts/extract-worldgen-structures.py, reading",
        "#   .cache/mc/26.2/versions/26.2/server-26.2.jar",
        "# (the OUTER .cache/mc/26.2/server.jar is a *bundler* and holds none of",
        "# these paths -- searching it returns zero hits).",
        "#",
        "# Every hash below is the SHA-256 of the JAR entry's bytes, not of the",
        "# bundled file, and regenerating requires the jar. So this manifest is an",
        "# external anchor: an asset edited by hand fails the drift gate, and the",
        "# manifest cannot be re-derived from the assets to hide the edit.",
        "#",
        "# Regenerate with `just regen-worldgen-structures`.",
        "#",
        "# Paths are relative to crates/lodestone-server/assets/.",
        "# Format: <path> <sha256> <bytes>",
        "#",
    ]
    for key in sorted(counts):
        out.append(f"# counts {key} {counts[key]}")
    out.append(f"# counts TOTAL {len(rows)} files {total} bytes")
    out.append("")
    for rel, digest, size in rows:
        out.append(f"{rel} {digest} {size}")

    MANIFEST.parent.mkdir(parents=True, exist_ok=True)
    MANIFEST.write_text("\n".join(out) + "\n", encoding="utf-8")

    for key in sorted(counts):
        print(f"{key:46s} {counts[key]:5d}")
    print(f"{'TOTAL':46s} {len(rows):5d} files, {total} bytes ({total / 1048576:.2f} MiB)")
    print(f"manifest -> {MANIFEST}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
