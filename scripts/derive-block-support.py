#!/usr/bin/env python3
"""Classify every 26.2 block by the *shape* of its `canSurvive` support rule.

Reads the decompiled 26.2 source (an outside source, not this repo's code):

  * `Blocks.java` registrations, for block name -> implementing class
  * every `.java` under the block tree, for `class X extends Y`

then walks each block's ancestor chain until it hits a class in BASE_KIND —
the set of vanilla base classes whose `canSurvive`/`updateShape` pair is a
self-destruct on a *named* support cell. Everything else is `none`.

Generates the `SUPPORT_KINDS` table in
`crates/lodestone-server/src/block_support.rs`. Run from the repo root:

    python3 scripts/derive-block-support.py            # counts + spot checks
    python3 scripts/derive-block-support.py --rust     # the Rust rows, sorted

Paste the `--rust` output between the `static SUPPORT_KINDS` braces verbatim.
**Do not hand-transcribe it** — a hand-typed pass at this table lost 18 rows and
invented 8, which the module's own census check would have caught only for the
invented half.

`FORCE_NONE` is the load-bearing half of the classification: a class that
inherits from a `BASE_KIND` ancestor but declares no `canSurvive` of its own, or
whose rule is not a single named cell, has to be listed there. Every entry was
grepped against the decompile before being added; see
`docs/block-support-and-item-use.md`.
"""
import os
import re
import sys
from collections import defaultdict

ROOT = ".cache/mc/26.2/src/net/minecraft"

# `DyeColor` declaration order, for the `ColorCollection.registerBlocks` helpers
# (beds, carpets, banners, wall banners, shulker boxes, ...): one block per
# colour, named `<colour>_<field>`.
DYE_COLOURS = [
    "white", "orange", "magenta", "light_blue", "yellow", "lime", "pink",
    "gray", "light_gray", "cyan", "purple", "blue", "brown", "green", "red",
    "black",
]

# class -> support-cell shape. Each entry was read out of that class's own
# canSurvive/updateShape in the 26.2 decompile.
BASE_KIND = {
    # `VegetationBlock.canSurvive` -> mayPlaceOn(below). Covers flowers,
    # saplings, crops, stems, mushrooms, tall grass, dead bush, nether wart,
    # seagrass, and DoublePlantBlock's lower half.
    "VegetationBlock": "below",
    "BushBlock": "below",
    "DoublePlantBlock": "double_block",
    # `BaseTorchBlock.canSurvive` -> canSupportCenter(below, UP).
    "BaseTorchBlock": "below",
    "TorchBlock": "below",
    "WallTorchBlock": "attached_facing",
    "RedstoneWallTorchBlock": "attached_facing",
    # `BaseRailBlock.canSurvive` -> canSupportRigidBlock(below).
    "BaseRailBlock": "below",
    "LadderBlock": "attached_facing",
    "DoorBlock": "double_block",
    "BedBlock": "bed_part",
    "SnowLayerBlock": "below",
    "CarpetBlock": "below",
    "BasePressurePlateBlock": "below",
    "RedStoneWireBlock": "below",
    "DiodeBlock": "below",
    "SugarCaneBlock": "below",
    "CactusBlock": "below",
    "BambooStalkBlock": "below",
    "BambooSaplingBlock": "below",
    "SeaPickleBlock": "below",
    "WaterlilyBlock": "below",
    "LilyPadBlock": "below",
    "TripWireHookBlock": "attached_facing",
    "StandingSignBlock": "below",
    "CeilingHangingSignBlock": "hanging",
    "WallHangingSignBlock": "attached_facing",
    "WallSignBlock": "attached_facing",
    "BannerBlock": "below",
    "WallBannerBlock": "attached_facing",
    "FaceAttachedHorizontalDirectionalBlock": "attach_face",
    "LanternBlock": "hanging_or_below",
    "AmethystClusterBlock": "attached_facing",
    # `BaseCoralPlantTypeBlock.canSurvive` -> below is sturdy UP; the wall-fan
    # subclass overrides it with its own facing test.
    "BaseCoralWallFanBlock": "attached_facing",
    "BaseCoralPlantTypeBlock": "below",
    "NetherWartBlock": "below",
    "CropBlock": "below",
    "StemBlock": "below",
    "AttachedStemBlock": "below",
    "MushroomBlock": "below",
    "FlowerBlock": "below",
    "SaplingBlock": "below",
    "TallGrassBlock": "below",
    "DeadBushBlock": "below",
    "SeagrassBlock": "below",
    "DryVegetationBlock": "below",
    "FireflyBushBlock": "below",
    "SweetBerryBushBlock": "below",
    "CocoaBlock": "attached_facing",
    "PinkPetalsBlock": "below",
    "LeafLitterBlock": "below",
    "HangingRootsBlock": "hanging",
    "CaveVinesBlock": "hanging",
    "CaveVinesPlantBlock": "hanging",
    "WeepingVinesPlantBlock": "hanging",
    "TwistingVinesPlantBlock": "below",
    "GrowingPlantHeadBlock": "growing_plant",
    "GrowingPlantBodyBlock": "growing_plant",
    "BigDripleafBlock": "below",
    "SmallDripleafBlock": "below",
    "AzaleaBlock": "below",
    "NetherSproutsBlock": "below",
    "RootsBlock": "below",
    "FungusBlock": "below",
    "WitherRoseBlock": "below",
    "PitcherCropBlock": "double_block",
}

# Classes whose `canSurvive` is genuinely absent or not a support rule, listed
# so a subclass of a "below" base is not misclassified by inheritance alone.
FORCE_NONE = {
    "FireBlock",  # crate::fire owns its own canSurvive
    "SoulFireBlock",
    "BaseFireBlock",
    "ScaffoldingBlock",  # distance-based, its own tick
    "ChorusPlantBlock",
    "ChorusFlowerBlock",
    "PointedDripstoneBlock",
    "VineBlock",  # any of five faces; not a single named cell
    "GlowLichenBlock",
    "MultifaceBlock",
    "SculkVeinBlock",
    "PowderSnowBlock",
    "WebBlock",
    # Supported by WATER, not by a block: `LilyPadBlock.mayPlaceOn` /
    # `FrogspawnBlock.mayPlaceOn` both accept a water fluid below, and this
    # crate's "the support cell went to air or fluid" trigger would destroy
    # them on sight.
    "LilyPadBlock",
    "FrogspawnBlock",
    # Two-cell rules that are not one named support cell:
    # `BigDripleafStemBlock.canSurvive` reads above AND below;
    # `MossyCarpetBlock.canSurvive` has a `bottom`/side-wall variant set.
    "BigDripleafStemBlock",
    "MossyCarpetBlock",
    # No `canSurvive` at all in 26.2 — verified by grep on each class and its
    # ancestors. A floor skull, a turtle egg and a tripwire all stay put when
    # the block under them goes.
    "SkullBlock",
    "AbstractSkullBlock",
    "TripWireBlock",
    "TurtleEggBlock",
    "SnifferEggBlock",
}


def java_files():
    for base, _dirs, files in os.walk(ROOT):
        for name in files:
            if name.endswith(".java"):
                yield os.path.join(base, name)


def build_hierarchy():
    parent = {}
    decl = re.compile(
        r"^\s*(?:public\s+|final\s+|abstract\s+|protected\s+)*class\s+(\w+)"
        r"(?:<[^>]*>)?\s+extends\s+([\w.]+)"
    )
    for path in java_files():
        with open(path, errors="replace") as fh:
            for line in fh:
                m = decl.match(line)
                if m:
                    child, sup = m.group(1), m.group(2).split(".")[-1]
                    parent.setdefault(child, sup)
    return parent


def parse_registrations():
    """field name -> implementing class name, from Blocks.java."""
    src = open(os.path.join(ROOT, "world/level/block/Blocks.java"), errors="replace").read()
    out = {}
    # Each registration starts at `public static final Block FOO = ` and runs
    # to the next one; the class is the first `new X(` or `X::new` inside.
    starts = [(m.start(), m.group(2), m.group(1) is not None) for m in
              re.finditer(r"public static final (ColorCollection<)?Block>? (\w+) =", src)]
    for i, (pos, field, is_colours) in enumerate(starts):
        end = starts[i + 1][0] if i + 1 < len(starts) else len(src)
        body = src[pos:end]
        m = re.search(r"new (\w+)\s*\(", body) or re.search(r"(\w+)::new", body)
        cls = m.group(1) if m else None
        if cls is None:
            # `register(id, props)` with no class -> plain Block, or a
            # registerLegacyStair/registerLeaves style helper.
            helper = re.search(r"=\s*(\w+)\(", body)
            cls = {"registerLegacyStair": "StairBlock",
                   "registerLeaves": "LeavesBlock"}.get(
                helper.group(1) if helper else "", "Block")
        if is_colours:
            for colour in DYE_COLOURS:
                out[f"{colour.upper()}_{field}"] = cls
        else:
            out[field] = cls
    return out


def main():
    parent = build_hierarchy()
    regs = parse_registrations()
    kinds = {}
    unresolved = defaultdict(list)
    for field, cls in regs.items():
        name = field.lower()
        chain = []
        cur = cls
        seen = set()
        while cur and cur not in seen:
            seen.add(cur)
            chain.append(cur)
            cur = parent.get(cur)
        kind = None
        for c in chain:
            if c in FORCE_NONE:
                kind = "none"
                break
            if c in BASE_KIND:
                kind = BASE_KIND[c]
                break
        if kind is None:
            kind = "none"
            unresolved[cls].append(name)
        kinds[name] = kind

    if "--rust" in sys.argv:
        variant = {
            "below": "Below",
            "attached_facing": "AttachedFacing",
            "attach_face": "AttachFace",
            "double_block": "DoubleBlock",
            "bed_part": "BedPart",
            "hanging": "Hanging",
            "hanging_or_below": "HangingOrBelow",
        }
        rows = [(n, k) for n, k in sorted(kinds.items())
                if k in variant]
        for name, kind in rows:
            print(f'    ("minecraft:{name}", SupportKind::{variant[kind]}),')
        print(f"// rows: {len(rows)}", file=sys.stderr)
        return

    counts = defaultdict(int)
    for kind in kinds.values():
        counts[kind] += 1
    for kind, n in sorted(counts.items(), key=lambda kv: -kv[1]):
        print(f"{kind:20s} {n}")
    print(f"\ntotal blocks: {len(kinds)}")
    print("\n-- spot checks --")
    for probe in ["torch", "wall_torch", "poppy", "oak_sapling", "sugar_cane",
                  "rail", "powered_rail", "lever", "stone_button", "ladder",
                  "oak_door", "red_bed", "snow", "white_carpet", "wheat",
                  "stone_pressure_plate", "redstone_wire", "repeater",
                  "comparator", "cactus", "bamboo", "sunflower", "tall_grass",
                  "oak_sign", "oak_wall_sign", "white_banner", "lily_pad",
                  "stone", "dirt", "cobweb", "vine", "chest", "furnace",
                  "sea_pickle", "brown_mushroom", "nether_wart", "kelp",
                  "sculk_vein", "lantern", "soul_lantern", "amethyst_cluster",
                  "tripwire", "tripwire_hook", "melon_stem", "pumpkin_stem",
                  "cocoa", "turtle_egg", "pitcher_crop", "torchflower_crop",
                  "big_dripleaf", "small_dripleaf", "hanging_roots",
                  "weeping_vines", "twisting_vines", "glow_lichen",
                  "scaffolding", "pointed_dripstone", "chorus_flower"]:
        print(f"  {probe:24s} {kinds.get(probe, '<<MISSING>>')}  ({regs.get(probe.upper(), '?')})")
    print("\n-- top unresolved classes (kind=none by fallthrough) --")
    for cls, names in sorted(unresolved.items(), key=lambda kv: -len(kv[1]))[:25]:
        print(f"  {cls:36s} {len(names):4d}  e.g. {names[:3]}")


main()
