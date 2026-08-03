//! The per-block-state **ambient-occlusion occluder** census generated into
//! `src/generated/shade_brightness.rs` and read through
//! [`lodestone_data::shade_brightness`].
//!
//! Modelled on `block_physics.rs` and `hardness.rs`: generate-or-assert with
//! `LODESTONE_REGEN=1`, anchored to a committed JVM dump.
//!
//! # What is being anchored
//!
//! `BlockModelLighter.prepareQuadAmbientOcclusion` darkens a smooth-lit vertex
//! by `cache.getShadeBrightness(state, level, pos)`, i.e.
//! `BlockBehaviour.getShadeBrightness` (`BlockBehaviour.java:315-317`):
//! `state.isCollisionShapeFullBlock(level, pos) ? 0.2F : 1.0F`, with seven class
//! overrides. That is a **collision** predicate, and a renderer naturally has a
//! *culling* one to hand instead — the two agree on stone, slabs, water and
//! glass and disagree on leaves, which is why this table has to exist rather
//! than being approximated.
//!
//! # Data provenance
//!
//! `tests/support/shade_brightness_jvm.txt` is an authoritative dump produced by
//! booting the real 26.2 server (`oracle-java/ShadeBrightnessOracle.java`) and
//! reading, per block state, `BlockStateBase.getShadeBrightness` (column `S`,
//! reduced to `== 0.2F`) and `BlockStateBase.isCollisionShapeFullBlock` (column
//! `F`, the base formula with every override stripped). Neither is in
//! `blocks.json`, which carries no geometry at all, and `vendor/minecraft-data`
//! has no 26.x data.
//!
//! Two columns exist purely so this test's claims are non-vacuous:
//!
//! * `V <floatBits> <count>` — the histogram of every distinct
//!   `getShadeBrightness` return value across all 32,366 states. Two entries
//!   means the one-bit encoding is **lossless**; a third would mean the bitset
//!   is silently wrong ([`shade_brightness_returns_exactly_two_distinct_values`]).
//! * `F` — the base formula alone, so
//!   [`deriving_from_the_collision_shape_alone_gets_39_states_wrong`] can
//!   *measure* how far a derivation from [`lodestone_data::collision_shapes`]
//!   would be instead of asserting that it would be wrong.
//!
//! And `O <block> <class>` is the override census, read by reflection over the
//! class hierarchy: it makes "exactly seven classes override this" a
//! measurement, and it is the reason no block name is hand-typed here.
//! `TransparentBlock` alone covers 26 registered blocks.
//!
//! # Refreshing after a version bump
//!
//! 1. Re-dump from the server (keep the `#` header when copying over the
//!    committed dump). Byte-reproducible: two runs of the command below produced
//!    identical output (md5 `a03cd79dfd71f4753960c129eba88f49`).
//!
//! ```text
//! CACHE="$(cd .cache/mc/26.2 && pwd)"
//! HERE="$(cd crates/lodestone-data/oracle-java && pwd)"
//! docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
//!   CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
//!   cp /oracle/ShadeBrightnessOracle.java /work/ && javac -cp "$CP" -d /work /work/ShadeBrightnessOracle.java
//!   java -cp "/work:$CP" ShadeBrightnessOracle'
//! ```
//!
//! 2. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test shade_brightness \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```
//!
//! Note that [`committed_bits_match_the_dump`] is **not** `#[ignore]`d: it
//! compares the committed table's *bits* against the dump rather than the
//! generated file's bytes, so a reflow of the generated source cannot hide a
//! wrong bit and an ordinary `cargo test --workspace` still catches drift. The
//! byte-exact comparison lives in the ignored generator test, where a
//! regeneration is one command away.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::{block_states, collision_shapes, shade_brightness};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/shade_brightness.rs")
}

/// The committed JVM dump — an external anchor, not gitignored.
const DUMP: &str = include_str!("support/shade_brightness_jvm.txt");

struct Dump {
    state_count: usize,
    block_count: usize,
    /// `O` rows: block name -> the class declaring its `getShadeBrightness`.
    overrides: BTreeMap<String, String>,
    /// `V` rows: raw float bits -> how many states returned that value.
    histogram: BTreeMap<u32, usize>,
    /// `B` rows: first state id of each block, ascending.
    blocks: Vec<(usize, String)>,
    /// `S`: `getShadeBrightness(..) == 0.2F`, per state.
    shade_occludes: Vec<bool>,
    /// `F`: `isCollisionShapeFullBlock(..)`, per state.
    full_collision_cube: Vec<bool>,
}

impl Dump {
    /// Block name owning state `id`, from the `B` ranges.
    fn block_of(&self, id: usize) -> &str {
        let index = self
            .blocks
            .partition_point(|(start, _)| *start <= id)
            .saturating_sub(1);
        &self.blocks[index].1
    }

    /// `[first, last]` state-id range of `name`.
    fn range_of(&self, name: &str) -> (usize, usize) {
        let index = self
            .blocks
            .iter()
            .position(|(_, n)| n == name)
            .unwrap_or_else(|| panic!("{name} present in the dump"));
        let start = self.blocks[index].0;
        let end = self
            .blocks
            .get(index + 1)
            .map_or(self.state_count, |(next, _)| *next);
        (start, end - 1)
    }
}

fn parse_dump(text: &str) -> Dump {
    let mut state_count = None;
    let mut block_count = None;
    let mut overrides = BTreeMap::new();
    let mut histogram = BTreeMap::new();
    let mut blocks: Vec<(usize, String)> = Vec::new();
    let mut shade: BTreeMap<usize, &str> = BTreeMap::new();
    let mut full: BTreeMap<usize, &str> = BTreeMap::new();

    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split(' ');
        let kind = parts.next().expect("non-empty line has a kind");
        match kind {
            "C" => {
                state_count = Some(parts.next().expect("C states").parse().expect("usize"));
                block_count = Some(parts.next().expect("C blocks").parse().expect("usize"));
            }
            "O" => {
                let name = parts.next().expect("O name").to_owned();
                let owner = parts.next().expect("O class").to_owned();
                overrides.insert(name, owner);
            }
            "V" => {
                let bits = u32::from_str_radix(parts.next().expect("V bits"), 16).expect("hex");
                let count = parts.next().expect("V count").parse().expect("usize");
                histogram.insert(bits, count);
            }
            "B" => {
                let start = parts.next().expect("B id").parse().expect("usize");
                let name = parts.next().expect("B name").to_owned();
                blocks.push((start, name));
            }
            "P" => {
                let which = parts.next().expect("P kind");
                let start: usize = parts.next().expect("P start").parse().expect("usize");
                let bitstring = parts.next().expect("P bits");
                let target = match which {
                    "S" => &mut shade,
                    "F" => &mut full,
                    other => panic!("unknown P column {other}"),
                };
                target.insert(start, bitstring);
            }
            other => panic!("unknown dump line kind {other}"),
        }
    }

    let flatten = |chunks: &BTreeMap<usize, &str>| -> Vec<bool> {
        chunks
            .values()
            .flat_map(|s| s.chars().map(|c| c == '1'))
            .collect()
    };
    let state_count = state_count.expect("dump carries a C row");
    let shade_occludes = flatten(&shade);
    let full_collision_cube = flatten(&full);
    assert_eq!(shade_occludes.len(), state_count, "S column covers every state");
    assert_eq!(
        full_collision_cube.len(),
        state_count,
        "F column covers every state"
    );
    assert!(!blocks.is_empty(), "dump carries B rows");

    Dump {
        state_count,
        block_count: block_count.expect("dump carries a C row"),
        overrides,
        histogram,
        blocks,
        shade_occludes,
        full_collision_cube,
    }
}

/// Packs a per-state bool list into a bitset, bit `i` of byte `i / 8` at
/// `1 << (i % 8)` — the layout [`shade_brightness`] reads.
fn pack(bits: &[bool]) -> Vec<u8> {
    let mut out = vec![0u8; bits.len().div_ceil(8)];
    for (index, &set) in bits.iter().enumerate() {
        if set {
            out[index / 8] |= 1u8 << (index % 8);
        }
    }
    out
}

/// Renders the committed `shade_brightness.rs` source from the parsed dump.
fn generate(dump: &Dump) -> String {
    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test shade_brightness -- --ignored`\n\
         // from tests/support/shade_brightness_jvm.txt (a headless 26.2 server dump of\n\
         // BlockStateBase.getShadeBrightness(), protocol 776 / Minecraft 26.2).\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated per-block-state ambient-occlusion occluder bitset for protocol\n\
         //! 776 (Minecraft 26.2), indexed by global block-state id.\n\
         //! Consumed by [`crate::shade_brightness`].\n\n",
    );
    let _ = writeln!(out, "/// Number of block states (ids are `0..STATE_COUNT`).");
    let _ = writeln!(out, "pub const STATE_COUNT: u32 = {};\n", dump.state_count);

    let packed = pack(&dump.shade_occludes);
    let set = dump.shade_occludes.iter().filter(|&&b| b).count();
    let _ = writeln!(
        out,
        "/// Per-state `BlockStateBase.getShadeBrightness(..) == 0.2F` — vanilla's\n\
         /// ambient-occlusion occluder test — packed one bit per state: bit `id % 8` of\n\
         /// byte `id / 8`. {set} of {} states are set.",
        dump.state_count
    );
    let _ = writeln!(out, "pub static SHADE_OCCLUDES: [u8; {}] = [", packed.len());
    for chunk in packed.chunks(16) {
        out.push_str("    ");
        for byte in chunk {
            let _ = write!(out, "0x{byte:02x}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n\n");

    out
}

// ---------------------------------------------------------------------------
// The dump's own self-consistency — what makes the claims below non-vacuous
// ---------------------------------------------------------------------------

/// The one-bit encoding is lossless **because** the game only ever returns two
/// values. If a future version added a third shade level, this fails first and
/// names it, rather than the bitset quietly rounding it to one of the two.
#[test]
fn shade_brightness_returns_exactly_two_distinct_values() {
    let dump = parse_dump(DUMP);
    let occluded = shade_brightness::OCCLUDED_SHADE.to_bits();
    let open = shade_brightness::OPEN_SHADE.to_bits();

    let values: Vec<(u32, usize)> = dump.histogram.iter().map(|(k, v)| (*k, *v)).collect();
    assert_eq!(
        values.len(),
        2,
        "getShadeBrightness returned {} distinct values across {} states: {:?} — the \
         one-bit encoding in src/generated/shade_brightness.rs is lossy",
        values.len(),
        dump.state_count,
        values
            .iter()
            .map(|(bits, n)| (f32::from_bits(*bits), *n))
            .collect::<Vec<_>>()
    );
    assert!(
        dump.histogram.contains_key(&occluded),
        "0.2F (bits {occluded:#x}) is one of the two values; got {:?}",
        dump.histogram.keys().collect::<Vec<_>>()
    );
    assert!(
        dump.histogram.contains_key(&open),
        "1.0F (bits {open:#x}) is one of the two values; got {:?}",
        dump.histogram.keys().collect::<Vec<_>>()
    );
    // The occluding count is the same number the bitset claims, reached two
    // different ways: the JVM's own histogram, and a popcount of the table.
    assert_eq!(
        dump.histogram[&occluded] as u32,
        shade_brightness::occluding_state_count(),
        "the histogram's 0.2F count and the committed bitset's popcount disagree"
    );
}

/// `getShadeBrightness` is `protected`, so the set of *blocks* it affects is a
/// property of the class hierarchy, not of any data file. This asserts the seven
/// classes mechanically and checks the family sizes, so nobody is tempted to
/// hand-type a block list (`TransparentBlock` is 26 blocks wide).
#[test]
fn exactly_seven_classes_override_shade_brightness() {
    let dump = parse_dump(DUMP);
    let mut by_class: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (block, class) in &dump.overrides {
        by_class
            .entry(class.rsplit('.').next().expect("class has a simple name"))
            .or_default()
            .push(block.as_str());
    }
    let classes: BTreeSet<&str> = by_class.keys().copied().collect();
    let expected: BTreeSet<&str> = [
        "BarrierBlock",
        "LightBlock",
        "MudBlock",
        "SnowLayerBlock",
        "SoulSandBlock",
        "StructureVoidBlock",
        "TransparentBlock",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        classes, expected,
        "the getShadeBrightness override set changed; every block under a new or \
         removed class silently changes its ambient occlusion"
    );
    // The whole point of dumping rather than hand-typing: this family is wide,
    // and `WaterloggedTransparentBlock` (the copper grates) is in it.
    assert_eq!(
        by_class["TransparentBlock"].len(),
        26,
        "TransparentBlock family size changed: {:?}",
        by_class["TransparentBlock"]
    );
    for single in ["BarrierBlock", "LightBlock", "MudBlock", "SoulSandBlock"] {
        assert_eq!(by_class[single].len(), 1, "{single} covers one block");
    }
}

/// Measures, rather than asserts, that [`collision_shapes`] cannot answer this
/// question: the seven overrides move 39 states across 30 blocks, in **both**
/// directions.
#[test]
fn deriving_from_the_collision_shape_alone_gets_39_states_wrong() {
    let dump = parse_dump(DUMP);
    let diverging: Vec<usize> = (0..dump.state_count)
        .filter(|&id| dump.shade_occludes[id] != dump.full_collision_cube[id])
        .collect();
    let blocks: BTreeSet<&str> = diverging.iter().map(|&id| dump.block_of(id)).collect();
    let toward_occluding = diverging
        .iter()
        .filter(|&&id| dump.shade_occludes[id])
        .count();

    assert_eq!(
        (diverging.len(), blocks.len()),
        (39, 30),
        "override effect changed: {} states across {} blocks ({:?})",
        diverging.len(),
        blocks.len(),
        blocks
    );
    // Both directions, so no monotone "and also treat X as solid" shortcut works.
    assert_eq!(
        toward_occluding, 3,
        "expected mud, soul_sand and snow[layers=8] to be occluding where the shape \
         is not; got {toward_occluding} such states"
    );
    assert_eq!(
        diverging.len() - toward_occluding,
        36,
        "expected 36 states (glass family, barrier, copper grates) to be open where \
         the shape says full cube"
    );
}

// ---------------------------------------------------------------------------
// The block populations this predicate exists for
// ---------------------------------------------------------------------------

/// Every leaf state darkens an AO corner. This is the divergence the predicate
/// swap is *for*: leaves are a full collision cube with no `getShadeBrightness`
/// override, so vanilla shades the underside of a canopy at `0.2`, while a
/// culling-occlusion predicate (leaves are cutout, so they do not occlude)
/// leaves it full-bright.
#[test]
fn every_leaf_state_occludes_ambient_light() {
    let mut leaf_states = 0usize;
    let mut leaf_blocks: BTreeSet<&str> = BTreeSet::new();
    for id in 0..block_states::STATE_COUNT {
        let Some(name) = block_states::block_name(id) else {
            continue;
        };
        if !name.ends_with("_leaves") {
            continue;
        }
        leaf_states += 1;
        leaf_blocks.insert(name);
        assert_eq!(
            shade_brightness::occludes_ambient_light(id),
            Some(true),
            "{name} (state {id}) must darken an AO corner"
        );
        assert_eq!(
            shade_brightness::shade_brightness(id).to_bits(),
            shade_brightness::OCCLUDED_SHADE.to_bits(),
            "{name} (state {id}) shade brightness"
        );
    }
    // Anti-vacuity: `_leaves` matching nothing would pass the loop above without
    // asserting anything. Measured on 26.2, so a version bump that adds a wood
    // type is expected to fail here and be updated.
    assert_eq!(
        (leaf_states, leaf_blocks.len()),
        (308, 11),
        "leaf population changed: {leaf_states} states across {leaf_blocks:?}"
    );
}

/// The blocks that make this predicate *not* interchangeable with a culling
/// test, in both directions, one row per surprise. Every id is looked up by name
/// so the table survives id renumbering.
#[test]
fn the_divergence_population_matches_vanilla_by_name() {
    // (block, occludes for AO?) — `true` where a culling predicate says "no".
    let cases: &[(&str, bool)] = &[
        // Full collision cube, no override: vanilla darkens, culling does not.
        ("minecraft:oak_leaves", true),
        ("minecraft:slime_block", true),
        ("minecraft:spawner", true),
        ("minecraft:beacon", true),
        // `IceBlock extends HalfTransparentBlock`, which does **not** override
        // getShadeBrightness — only `TransparentBlock` does. So ice darkens.
        ("minecraft:ice", true),
        ("minecraft:packed_ice", true),
        ("minecraft:blue_ice", true),
        // `TransparentBlock` overrides to 1.0, so these agree with culling.
        ("minecraft:glass", false),
        ("minecraft:red_stained_glass", false),
        ("minecraft:tinted_glass", false),
        // Copper grates are `WaterloggedTransparentBlock extends TransparentBlock`
        // — also 1.0, so they agree with culling too.
        ("minecraft:copper_grate", false),
        ("minecraft:waxed_oxidized_copper_grate", false),
        // Honey's collision box is inset, so the base formula already says open.
        ("minecraft:honey_block", false),
        // Overridden to 0.2 despite a non-full collision box (both sink you).
        ("minecraft:mud", true),
        ("minecraft:soul_sand", true),
        // Overridden to 1.0 despite a full collision box.
        ("minecraft:barrier", false),
        ("minecraft:light", false),
        ("minecraft:structure_void", false),
        // Sanity anchors at each end.
        ("minecraft:stone", true),
        ("minecraft:air", false),
        ("minecraft:water", false),
        ("minecraft:oak_stairs", false),
        ("minecraft:powder_snow", false),
    ];
    for &(name, occludes) in cases {
        let ids: Vec<u32> = (0..block_states::STATE_COUNT)
            .filter(|&id| block_states::block_name(id) == Some(name))
            .collect();
        assert!(!ids.is_empty(), "{name} present in the block-state table");
        for id in ids {
            assert_eq!(
                shade_brightness::occludes_ambient_light(id),
                Some(occludes),
                "{name} (state {id})"
            );
        }
    }
}

/// `SnowLayerBlock` is the one override that is per **state**, not per block:
/// only the eight-layer state (a full cube) darkens. A block-keyed table could
/// not express this, which is why the census is state-keyed.
#[test]
fn snow_darkens_only_at_eight_layers() {
    let mut seen = 0;
    for id in 0..block_states::STATE_COUNT {
        if block_states::block_name(id) != Some("minecraft:snow") {
            continue;
        }
        let layers = block_states::properties(id)
            .and_then(|props| props.iter().find(|(k, _)| *k == "layers"))
            .map(|(_, v)| *v)
            .unwrap_or_else(|| panic!("snow state {id} carries a `layers` property"));
        seen += 1;
        assert_eq!(
            shade_brightness::occludes_ambient_light(id),
            Some(layers == "8"),
            "minecraft:snow[layers={layers}] (state {id})"
        );
    }
    assert_eq!(seen, 8, "snow has eight layer states");
}

/// Out-of-range ids report `None` rather than a plausible-looking `false`, and
/// the float accessor falls back to open. Mirrors `block_solidity`'s contract.
#[test]
fn unknown_ids_are_none_and_open() {
    assert_eq!(
        shade_brightness::occludes_ambient_light(shade_brightness::STATE_COUNT),
        None
    );
    assert_eq!(
        shade_brightness::shade_brightness(shade_brightness::STATE_COUNT).to_bits(),
        shade_brightness::OPEN_SHADE.to_bits()
    );
    assert_eq!(shade_brightness::STATE_COUNT, block_states::STATE_COUNT);
    assert_eq!(shade_brightness::STATE_COUNT, collision_shapes::STATE_COUNT);
}

// ---------------------------------------------------------------------------
// Drift guards
// ---------------------------------------------------------------------------

/// Bit-for-bit, the committed table against the dump — **not** `#[ignore]`d, so
/// an ordinary `cargo test --workspace` catches drift. This is the guard that
/// matters: the byte-exact one below can be defeated by a harmless reflow of the
/// generated source, and `CLAUDE.md` records a line-oriented control on a
/// regenerated table reporting `0` differences where the true figure was ~15,000.
#[test]
fn committed_bits_match_the_dump() {
    let dump = parse_dump(DUMP);
    assert_eq!(dump.state_count as u32, shade_brightness::STATE_COUNT);
    assert_eq!(dump.block_count, dump.blocks.len());

    let mut wrong: Vec<(usize, bool, Option<bool>)> = Vec::new();
    for id in 0..dump.state_count {
        let expected = dump.shade_occludes[id];
        let actual = shade_brightness::occludes_ambient_light(id as u32);
        if actual != Some(expected) {
            wrong.push((id, expected, actual));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} states disagree with the JVM dump; first five: {:?} — regenerate \
         src/generated/shade_brightness.rs with LODESTONE_REGEN=1",
        wrong.len(),
        dump.state_count,
        wrong
            .iter()
            .take(5)
            .map(|&(id, expected, actual)| (id, dump.block_of(id), expected, actual))
            .collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "regenerates/verifies the committed table; run explicitly"]
fn committed_table_matches_dump() {
    let dump = parse_dump(DUMP);
    let generated = generate(&dump);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write committed table");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed table present");
    assert_eq!(
        generated, committed,
        "src/generated/shade_brightness.rs is stale vs the JVM dump; regenerate with \
         LODESTONE_REGEN=1"
    );
}

/// The `B` ranges in the dump and the committed block-state table agree on which
/// block owns which id — without this, every by-name assertion above could be
/// reading the right bit of the wrong block.
#[test]
fn dump_block_ranges_agree_with_the_block_state_table() {
    let dump = parse_dump(DUMP);
    for (start, name) in &dump.blocks {
        assert_eq!(
            block_states::block_name(*start as u32),
            Some(name.as_str()),
            "dump says state {start} is the first {name}"
        );
    }
    let (first, last) = dump.range_of("minecraft:snow");
    assert_eq!(last - first + 1, 8, "snow's dump range is its eight states");
}
