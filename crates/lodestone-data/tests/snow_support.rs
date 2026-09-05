//! The per-block-state `freeze_top_layer` facts generated into
//! `src/generated/snow_support.rs` and read through
//! [`lodestone_data::snow_support`].
//!
//! Modelled on `shade_brightness.rs` and `block_physics.rs`: generate-or-assert
//! with `LODESTONE_REGEN=1`, anchored to a committed JVM dump.
//!
//! # What is being anchored
//!
//! Vanilla's own snow-and-freeze feature's place step — vanilla's
//! whole `TOP_LAYER_MODIFICATION` step — reads exactly four per-state facts, and
//! the world generator cannot answer any of them from `blocks.json`:
//! vanilla's own "is face full" check against the collision shape and up (column `U`),
//! `!getFluidState().isEmpty()` (`L`), `getFluidState().is(Fluids.WATER) &&
//! block instanceof LiquidBlock` (`W`), and
//! `hasProperty(BlockStateProperties.SNOWY)` (`Y`). A fifth column, `D`
//! (`state == block.defaultBlockState()`), is not a predicate but the key the
//! consumer needs — see [`exactly_one_default_state_per_block`].
//!
//! # Data provenance
//!
//! `tests/support/snow_support_jvm.txt` is an authoritative dump produced by
//! booting the real 26.2 server (`oracle-java/SnowSupportOracle.java`) and
//! reading those five expressions per block state. `vendor/minecraft-data` has
//! no 26.x data at all, and the collision census next door was measured 92.29%
//! covered and stale for 26.2 — so "boot the jar and ask it" is again the only
//! authoritative source.
//!
//! Three columns exist purely so this test's claims are non-vacuous:
//!
//! * `K <col> <count>` — the JVM's own popcount per column. Compared against a
//!   popcount of the committed bitset, so a decode bug in the generator cannot
//!   ship a silently-shifted table
//!   ([`committed_bits_match_the_dump`]).
//! * `B <firstStateId> <block>` — the block ranges, so
//!   [`water_source_is_the_only_freezable_state`] and
//!   [`snowy_property_belongs_to_exactly_three_blocks`] can *name* what they
//!   found instead of asserting a bare count.
//! * `N <block>` — the dynamic-shape census, which bounds column `U`'s known
//!   scope (shapes are read with no neighbours) to a checkable set rather than a
//!   claim ([`powder_snow_is_the_only_dynamic_shape_block_worldgen_exposes`]).
//!
//! # Refreshing after a version bump
//!
//! 1. Re-dump from the server (keep the `#` header when copying over the
//!    committed dump). Runtime is Apple `container`, per
//!    `docs/oracle-runtimes.md`; `docker` works identically.
//!
//! ```text
//! CACHE="$(cd .cache/mc/26.2 && pwd)"
//! HERE="$(cd crates/lodestone-data/oracle-java && pwd)"
//! container run --rm --memory 3g -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work \
//!   eclipse-temurin:25-jdk bash -c '
//!   CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
//!   cp /oracle/SnowSupportOracle.java /work/ && javac -cp "$CP" -d /work /work/SnowSupportOracle.java
//!   java -cp "/work:$CP" SnowSupportOracle'
//! ```
//!
//! 2. Regenerate the committed table (`just regen-snow-support`):
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test snow_support \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```
//!
//! [`committed_bits_match_the_dump`] is deliberately **not** `#[ignore]`d: it
//! compares the committed table's *bits* against the dump rather than the
//! generated file's bytes, so a reflow of the generated source cannot hide a
//! wrong bit and an ordinary `cargo test --workspace` still catches drift.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::{
    block_states::{self, StateId},
    collision_shapes, snow_support,
};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/snow_support.rs")
}

/// The committed JVM dump — an external anchor, not gitignored.
const DUMP: &str = include_str!("support/snow_support_jvm.txt");

/// The dump columns, in the order the generated file emits them.
const COLUMNS: [(char, &str); 5] = [
    ('U', "FACE_FULL_UP"),
    ('L', "HAS_FLUID_STATE"),
    ('W', "IS_WATER_SOURCE_LIQUID_BLOCK"),
    ('Y', "HAS_SNOWY_PROPERTY"),
    ('D', "IS_DEFAULT_STATE"),
];

struct Dump {
    state_count: usize,
    block_count: usize,
    /// `B` rows: first state id of each block, ascending.
    blocks: Vec<(usize, String)>,
    /// `N` rows: blocks declaring a dynamic shape.
    dynamic_shape: BTreeSet<String>,
    /// `K` rows: the JVM's own popcount per column.
    counts: BTreeMap<char, usize>,
    /// `P` rows, flattened per column.
    columns: BTreeMap<char, Vec<bool>>,
}

impl Dump {
    fn column(&self, kind: char) -> &[bool] {
        self.columns
            .get(&kind)
            .unwrap_or_else(|| panic!("dump carries a {kind} column"))
    }

    /// Block name owning state `id`, from the `B` ranges.
    fn block_of(&self, id: usize) -> &str {
        let index = self
            .blocks
            .partition_point(|(start, _)| *start <= id)
            .saturating_sub(1);
        &self.blocks[index].1
    }

    /// Every state id whose bit is set in `kind`, grouped by owning block.
    fn set_states_by_block(&self, kind: char) -> BTreeMap<&str, Vec<usize>> {
        let mut out: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (id, &set) in self.column(kind).iter().enumerate() {
            if set {
                out.entry(self.block_of(id)).or_default().push(id);
            }
        }
        out
    }
}

fn parse_dump(text: &str) -> Dump {
    let mut state_count = None;
    let mut block_count = None;
    let mut blocks: Vec<(usize, String)> = Vec::new();
    let mut dynamic_shape = BTreeSet::new();
    let mut counts = BTreeMap::new();
    let mut chunks: BTreeMap<char, BTreeMap<usize, &str>> = BTreeMap::new();

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
            "B" => {
                let start = parts.next().expect("B id").parse().expect("usize");
                let name = parts.next().expect("B name").to_owned();
                blocks.push((start, name));
            }
            "N" => {
                dynamic_shape.insert(parts.next().expect("N name").to_owned());
            }
            "K" => {
                let col = one_char(parts.next().expect("K column"));
                let count: usize = parts.next().expect("K count").parse().expect("usize");
                counts.insert(col, count);
            }
            "P" => {
                let col = one_char(parts.next().expect("P column"));
                let start: usize = parts.next().expect("P start").parse().expect("usize");
                let bitstring = parts.next().expect("P bits");
                chunks.entry(col).or_default().insert(start, bitstring);
            }
            other => panic!("unknown dump line kind {other}"),
        }
    }

    let state_count = state_count.expect("dump carries a C row");
    let mut columns = BTreeMap::new();
    for (col, per_start) in chunks {
        let flat: Vec<bool> = per_start
            .values()
            .flat_map(|s| s.chars().map(|c| c == '1'))
            .collect();
        assert_eq!(
            flat.len(),
            state_count,
            "column {col} covers every state (got {})",
            flat.len()
        );
        columns.insert(col, flat);
    }
    assert!(!blocks.is_empty(), "dump carries B rows");
    for (kind, _) in COLUMNS {
        assert!(columns.contains_key(&kind), "dump carries column {kind}");
        assert!(counts.contains_key(&kind), "dump carries a K row for {kind}");
    }

    Dump {
        state_count,
        block_count: block_count.expect("dump carries a C row"),
        blocks,
        dynamic_shape,
        counts,
        columns,
    }
}

fn one_char(s: &str) -> char {
    let mut chars = s.chars();
    let c = chars.next().expect("column name is one char");
    assert!(chars.next().is_none(), "column name {s} is one char");
    c
}

/// Packs a per-state bool list into a bitset, bit `i % 8` of byte `i / 8` — the
/// layout [`snow_support`] reads.
fn pack(bits: &[bool]) -> Vec<u8> {
    let mut out = vec![0u8; bits.len().div_ceil(8)];
    for (index, &set) in bits.iter().enumerate() {
        if set {
            out[index / 8] |= 1u8 << (index % 8);
        }
    }
    out
}

/// Renders the committed `snow_support.rs` source from the parsed dump.
fn generate(dump: &Dump) -> String {
    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test snow_support -- --ignored`\n\
         // from tests/support/snow_support_jvm.txt (a headless 26.2 server dump of the four\n\
         // per-block-state facts freeze_top_layer needs, protocol 776 / Minecraft 26.2).\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated per-block-state `freeze_top_layer` support bitsets for protocol\n\
         //! 776 (Minecraft 26.2), indexed by global block-state id.\n\
         //! Consumed by [`crate::snow_support`].\n\n",
    );
    let _ = writeln!(out, "/// Number of block states (ids are `0..STATE_COUNT`).");
    let _ = writeln!(out, "pub const STATE_COUNT: u32 = {};\n", dump.state_count);

    for (kind, name) in COLUMNS {
        let bits = dump.column(kind);
        let packed = pack(bits);
        let set = bits.iter().filter(|&&b| b).count();
        let expression = match kind {
            'U' => "vanilla's own is-face-full check against the collision shape and up",
            'L' => "`!state.getFluidState().isEmpty()`",
            'W' => "`state.getFluidState().is(Fluids.WATER) && block instanceof LiquidBlock`",
            'Y' => "`state.hasProperty(BlockStateProperties.SNOWY)`",
            'D' => "`state == state.getBlock().defaultBlockState()`",
            other => panic!("unknown column {other}"),
        };
        let _ = writeln!(
            out,
            "/// Per-state {expression}, packed one bit per\n\
             /// state: bit `id % 8` of byte `id / 8`. {set} of {} states are set.",
            dump.state_count
        );
        let _ = writeln!(out, "pub static {name}: [u8; {}] = [", packed.len());
        for chunk in packed.chunks(16) {
            out.push_str("    ");
            for byte in chunk {
                let _ = write!(out, "0x{byte:02x}, ");
            }
            out.pop();
            out.push('\n');
        }
        out.push_str("];\n\n");
    }

    out
}

// ---------------------------------------------------------------------------
// Drift guards
// ---------------------------------------------------------------------------

/// Every bit of the committed table equals the dump's, both directions, plus the
/// JVM's own popcount per column. Not `#[ignore]`d: this is what an ordinary
/// `cargo test --workspace` runs, and it cannot be satisfied by a reflow.
#[test]
fn committed_bits_match_the_dump() {
    let dump = parse_dump(DUMP);
    assert_eq!(
        snow_support::STATE_COUNT as usize,
        dump.state_count,
        "committed STATE_COUNT disagrees with the dump"
    );

    let readers: [(char, fn(StateId) -> bool); 5] = [
        ('U', snow_support::face_full_up),
        ('L', snow_support::has_fluid_state),
        ('W', snow_support::is_water_source_liquid_block),
        ('Y', snow_support::has_snowy_property),
        ('D', snow_support::is_default_state),
    ];
    for (kind, read) in readers {
        let expected = dump.column(kind);
        let mut popcount = 0usize;
        for (id, &want) in expected.iter().enumerate() {
            let id = StateId::new(id as u32).expect("dump id is in the state census");
            let got = read(id);
            assert_eq!(
                got,
                want,
                "column {kind}, state id {} ({}): committed {got}, dump {want}",
                id.raw(),
                dump.block_of(id.raw() as usize)
            );
            if got {
                popcount += 1;
            }
        }
        assert_eq!(
            popcount, dump.counts[&kind],
            "column {kind}: the committed table's popcount and the JVM's own K row disagree — \
             a shifted or truncated bitset"
        );
    }
}

/// Byte-exact regeneration. `#[ignore]`d because it rewrites a committed source
/// file under `LODESTONE_REGEN=1`.
#[test]
#[ignore = "regenerates a committed source file; run with LODESTONE_REGEN=1"]
fn committed_table_matches_dump() {
    let dump = parse_dump(DUMP);
    let generated = generate(&dump);
    let path = committed_path();
    if std::env::var("LODESTONE_REGEN").is_ok() {
        std::fs::write(&path, &generated).expect("write generated table");
        println!("regenerated {} ({} bytes)", path.display(), generated.len());
        return;
    }
    let committed = std::fs::read_to_string(&path).expect("read committed table");
    assert_eq!(
        committed, generated,
        "src/generated/snow_support.rs is stale; rerun with LODESTONE_REGEN=1"
    );
}

// ---------------------------------------------------------------------------
// What the columns actually say — the claims `crate::snow_support`'s doc makes
// ---------------------------------------------------------------------------

/// Vanilla's own biome "should freeze" check's block condition is true for **exactly one** state,
/// `minecraft:water[level=0]`. This is the fact most likely to be got wrong by
/// hand ("is it water?"): flowing water is the flowing-water fluid and fails
/// the source-fluid identity test, and a waterlogged block is not a liquid block. A port
/// that freezes any water block ices over waterfalls vanilla leaves running.
#[test]
fn water_source_is_the_only_freezable_state() {
    let dump = parse_dump(DUMP);
    let by_block = dump.set_states_by_block('W');
    let names: Vec<&str> = by_block.keys().copied().collect();
    assert_eq!(
        names,
        vec!["minecraft:water"],
        "more than the water block passes shouldFreeze's block condition"
    );
    let states = &by_block["minecraft:water"];
    assert_eq!(
        states.len(),
        1,
        "exactly one water state freezes; got {} of them",
        states.len()
    );
    // Name it, rather than trusting the count: the freezable state must be the
    // one with `level=0`.
    let id = states[0] as u32;
    let props: BTreeMap<&str, &str> = block_states::properties(id)
        .expect("water state has properties")
        .iter()
        .copied()
        .collect();
    assert_eq!(
        props.get("level").copied(),
        Some("0"),
        "the freezable water state is level=0; got {props:?}"
    );

    // Control that this detector would have found a second one: every OTHER
    // water state must be present in the wider `has_fluid_state` column, so the
    // narrowness above is a real property of `W` and not an empty column.
    let (start, _) = {
        let index = dump
            .blocks
            .iter()
            .position(|(_, n)| n == "minecraft:water")
            .expect("water in B rows");
        (
            dump.blocks[index].0,
            dump.blocks.get(index + 1).map_or(dump.state_count, |(n, _)| *n),
        )
    };
    assert!(
        dump.column('L')[start],
        "water[level=0] must also be in the has_fluid_state column"
    );
    assert!(
        dump.column('L')[start + 1],
        "water[level=1] must be in the has_fluid_state column even though it does not freeze — \
         otherwise `W`'s narrowness is measuring an empty column, not a real distinction"
    );
    assert!(
        !dump.column('W')[start + 1],
        "water[level=1] must NOT freeze"
    );
}

/// `snowy` belongs to exactly three blocks, two states each. Named, so the
/// engine's `snowy` flip has a checked list rather than a remembered one.
#[test]
fn snowy_property_belongs_to_exactly_three_blocks() {
    let dump = parse_dump(DUMP);
    let by_block = dump.set_states_by_block('Y');
    let names: Vec<&str> = by_block.keys().copied().collect();
    assert_eq!(
        names,
        vec![
            "minecraft:grass_block",
            "minecraft:mycelium",
            "minecraft:podzol"
        ],
        "the set of blocks carrying the `snowy` property changed"
    );
    for (name, states) in &by_block {
        assert_eq!(states.len(), 2, "{name} has two `snowy` states");
    }
}

/// The `Y` column agrees with a scan of [`block_states::properties`], which is
/// what makes "this column is derivable" a measurement instead of a claim — and
/// what would catch a state-id renumbering that desynchronised the two censuses.
#[test]
fn snowy_column_agrees_with_the_block_state_property_census() {
    let dump = parse_dump(DUMP);
    let column = dump.column('Y');
    let mut agreed = 0usize;
    for (id, &want) in column.iter().enumerate() {
        let derived = block_states::properties(id as u32)
            .expect("every state has a property list")
            .iter()
            .any(|(key, _)| *key == "snowy");
        assert_eq!(
            derived,
            want,
            "state {id} ({}): property scan says {derived}, the JVM says {want}",
            dump.block_of(id)
        );
        if want {
            agreed += 1;
        }
    }
    assert_eq!(agreed, 6, "six states agreed, not {agreed}");
}

/// `face_full_up` is **not** "the collision shape is one unit box", and this
/// measures the disagreement in both directions rather than asserting it. The
/// number is the argument for dumping the column: a derivation from
/// [`collision_shapes`] alone would be wrong on every state counted here.
#[test]
fn face_full_up_disagrees_with_a_unit_box_derivation() {
    let dump = parse_dump(DUMP);
    let column = dump.column('U');
    let is_unit_box = |id: u32| -> bool {
        let state = block_states::StateId::new(id).expect("every census state validates");
        let boxes = collision_shapes::collision_boxes(state);
        boxes.len() == 1 && boxes[0].min == [0.0, 0.0, 0.0] && boxes[0].max == [1.0, 1.0, 1.0]
    };

    let mut full_up_not_unit: BTreeSet<&str> = BTreeSet::new();
    let mut unit_not_full_up: BTreeSet<&str> = BTreeSet::new();
    let mut unit_count = 0usize;
    for (id, &want) in column.iter().enumerate() {
        let unit = is_unit_box(id as u32);
        if unit {
            unit_count += 1;
        }
        if want && !unit {
            full_up_not_unit.insert(dump.block_of(id));
        }
        if unit && !want {
            unit_not_full_up.insert(dump.block_of(id));
        }
    }

    let set = column.iter().filter(|&&b| b).count();
    println!(
        "face_full_up: {set}/{} states; unit-box shapes: {unit_count}; \
         full-up-but-not-unit blocks: {}; unit-but-not-full-up blocks: {}",
        dump.state_count,
        full_up_not_unit.len(),
        unit_not_full_up.len()
    );
    assert_eq!(set, dump.counts[&'U'], "K U matches the column popcount");
    // The load-bearing direction: blocks whose UP face is full while their shape
    // is not a unit box. Snow survives on all of these, and a unit-box
    // derivation would reject every one.
    assert!(
        !full_up_not_unit.is_empty(),
        "if no block has a full UP face without being a unit box, a derivation from \
         collision_shapes would be adequate and this table would not need to exist — \
         that would be new information, not a pass"
    );
    // Both counts printed above are the measurement; this only pins that the
    // two censuses are not accidentally identical.
    assert_ne!(
        set, unit_count,
        "face_full_up and the unit-box derivation returned the same count, which would \
         make the disagreement above impossible to interpret"
    );
}

/// The one dynamic-shape block world generation actually places at a surface
/// top is `powder_snow` (snowy slopes, groves, frozen peaks), and its `U` bit is
/// **false** — so a snow layer does not survive on powder snow.
///
/// That is not a limitation of this dump: vanilla's own snow-layer block's own
/// "can survive" check reads the block-below's collision shape through the
/// two-argument overload of its collision-shape accessor, which supplies an
/// empty collision context. The oracle
/// calls the same overload, so a context-dependent shape resolves the same way
/// here as it does in the feature. The `N` census exists to make that a bounded,
/// named set rather than an assumption — and it corrected two guesses on first
/// run: `chorus_plant` is **not** dynamic-shape, and `powder_snow` is.
#[test]
fn powder_snow_is_the_only_dynamic_shape_block_worldgen_exposes() {
    let dump = parse_dump(DUMP);
    assert!(
        !dump.dynamic_shape.is_empty(),
        "the dynamicShape census is empty, so this test proves nothing"
    );
    // Everything `lodestone-worldgen` can leave exposed at a MOTION_BLOCKING
    // top: terrain, surface materials, ores, and the vegetation blocks that
    // block motion or carry a fluid.
    const WORLDGEN_SURFACE: &[&str] = &[
        "minecraft:stone",
        "minecraft:deepslate",
        "minecraft:dirt",
        "minecraft:coarse_dirt",
        "minecraft:rooted_dirt",
        "minecraft:grass_block",
        "minecraft:podzol",
        "minecraft:mycelium",
        "minecraft:moss_block",
        "minecraft:mud",
        "minecraft:clay",
        "minecraft:sand",
        "minecraft:red_sand",
        "minecraft:gravel",
        "minecraft:sandstone",
        "minecraft:terracotta",
        "minecraft:calcite",
        "minecraft:tuff",
        "minecraft:andesite",
        "minecraft:diorite",
        "minecraft:granite",
        "minecraft:snow_block",
        "minecraft:powder_snow",
        "minecraft:ice",
        "minecraft:packed_ice",
        "minecraft:blue_ice",
        "minecraft:water",
        "minecraft:lava",
        "minecraft:magma_block",
        "minecraft:obsidian",
        "minecraft:oak_log",
        "minecraft:spruce_log",
        "minecraft:birch_log",
        "minecraft:oak_leaves",
        "minecraft:spruce_leaves",
        "minecraft:birch_leaves",
        "minecraft:cactus",
        "minecraft:pumpkin",
        "minecraft:melon",
    ];
    let overlap: Vec<&str> = WORLDGEN_SURFACE
        .iter()
        .copied()
        .filter(|name| dump.dynamic_shape.contains(*name))
        .collect();
    assert_eq!(
        overlap,
        vec!["minecraft:powder_snow"],
        "the set of dynamicShape() blocks world generation can expose at a surface top \
         changed; each new one needs its collision context re-checked against \
         SnowLayerBlock.canSurvive's own two-argument getCollisionShape call"
    );
    // Non-vacuity: the census must contain something we can name independently,
    // or the filter above is comparing against an empty set.
    assert!(
        dump.dynamic_shape.contains("minecraft:scaffolding")
            && dump.dynamic_shape.contains("minecraft:shulker_box"),
        "scaffolding and shulker_box are dynamicShape() in vanilla; their absence means the \
         N census is not measuring what this test assumes. Got: {:?}",
        dump.dynamic_shape
    );
}

/// Four "can survive" inputs that a hand-written table gets wrong, each read
/// straight out of the dump. Every one of these was a wrong guess before the
/// oracle ran, and each produces visibly wrong terrain on its own:
///
/// * **`snow[layers=8]` has `U == false`.** All eight snow states do. That is
///   why vanilla's own snow-layer block's own "can survive" check carries an explicit
///   `|| belowState.is(this) && belowState.getValue(LAYERS) == 8` clause — the
///   geometry alone never satisfies it, because
///   a full snow layer is 14/16 tall.
/// * **`ice` and `packed_ice` have `U == true`** and are in
///   `cannot_support_snow_layer`. So the tag check must run **before** the
///   geometry check, or every frozen ocean gets a snow blanket on its ice.
/// * **`mud`, `honey_block` and `soul_sand` have `U == false`** and are in
///   `support_override_snow_layer` — so that tag is load-bearing too, in the
///   opposite direction.
/// * **`oak_leaves` has `U == true`** — snow sits on leaves, so the
///   `MOTION_BLOCKING` heightmap must include them.
#[test]
fn cansurvive_inputs_that_a_hand_written_table_gets_wrong() {
    let dump = parse_dump(DUMP);
    let column = dump.column('U');
    let states_of = |name: &str| -> std::ops::Range<usize> {
        let index = dump
            .blocks
            .iter()
            .position(|(_, n)| n == name)
            .unwrap_or_else(|| panic!("{name} in B rows"));
        let start = dump.blocks[index].0;
        let end = dump
            .blocks
            .get(index + 1)
            .map_or(dump.state_count, |(n, _)| *n);
        start..end
    };

    let snow = states_of("minecraft:snow");
    assert_eq!(snow.len(), 8, "snow has eight layer states");
    for id in snow {
        assert!(
            !column[id],
            "snow state {id} reports a full UP face; SnowLayerBlock.canSurvive's explicit \
             layers==8 clause would then be dead code, which it is not"
        );
    }

    for name in ["minecraft:ice", "minecraft:packed_ice", "minecraft:barrier"] {
        for id in states_of(name) {
            assert!(
                column[id],
                "{name} state {id} must have a full UP face — it is only kept snow-free by \
                 the cannot_support_snow_layer tag, so the tag check has to run first"
            );
        }
    }

    for name in [
        "minecraft:mud",
        "minecraft:honey_block",
        "minecraft:soul_sand",
    ] {
        for id in states_of(name) {
            assert!(
                !column[id],
                "{name} state {id} must NOT have a full UP face — it only supports snow via \
                 the support_override_snow_layer tag, so that tag is load-bearing"
            );
        }
    }

    for name in ["minecraft:oak_leaves", "minecraft:spruce_leaves"] {
        for id in states_of(name) {
            assert!(column[id], "{name} state {id} supports a snow layer");
        }
    }

    for name in ["minecraft:cactus", "minecraft:powder_snow"] {
        for id in states_of(name) {
            assert!(
                !column[id],
                "{name} state {id} must not support a snow layer"
            );
        }
    }
}

/// Exactly one default state per block, and it is the state a property-less
/// block string denotes. This is what makes the world generator's bare
/// `minecraft:water` resolve to the one freezable water state — the column exists
/// for that lookup and nothing else.
#[test]
fn exactly_one_default_state_per_block() {
    let dump = parse_dump(DUMP);
    let by_block = dump.set_states_by_block('D');
    assert_eq!(
        by_block.len(),
        dump.block_count,
        "one default state per registered block"
    );
    for (name, states) in &by_block {
        assert_eq!(
            states.len(),
            1,
            "{name} has {} default states, not 1",
            states.len()
        );
    }
    // The load-bearing instance, named: water's default is the level=0 state,
    // which is also the only state `W` sets. If those two ever diverge, a
    // property-less `minecraft:water` stops freezing and every ocean stays open.
    let water_default = by_block["minecraft:water"][0];
    assert!(
        dump.column('W')[water_default],
        "water's default state must be the freezable one; default is state \
         {water_default}, which W does not set"
    );
    // And lava's default must NOT be freezable, or lava lakes would ice over.
    let lava_default = by_block["minecraft:lava"][0];
    assert!(!dump.column('W')[lava_default], "lava must not freeze");
    assert!(
        dump.column('L')[lava_default],
        "lava must still count as a fluid for the MOTION_BLOCKING heightmap"
    );
}

/// The dump's own shape: one `B` row per registered block.
#[test]
fn block_ranges_cover_every_block() {
    let dump = parse_dump(DUMP);
    assert_eq!(
        dump.blocks.len(),
        dump.block_count,
        "one B row per registered block"
    );
    assert_eq!(dump.blocks[0].0, 0, "the first B row starts at state 0");
    for pair in dump.blocks.windows(2) {
        assert!(
            pair[0].0 < pair[1].0,
            "B rows are strictly ascending: {pair:?}"
        );
    }
}
