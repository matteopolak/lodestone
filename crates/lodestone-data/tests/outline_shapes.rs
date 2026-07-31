//! Block outline/interaction-shape tables: hermetic checks over the committed
//! tables, plus an `#[ignore]`d drift guard that regenerates them from the
//! committed JVM dump and asserts byte-for-byte equality — modelled on
//! `hardness.rs` and `collision_shapes.rs`. The generator lives here so the
//! checked-in tables can never silently drift from the game data.
//!
//! # Data provenance
//!
//! `tests/support/outline_shape_jvm.txt` is an authoritative dump produced by
//! booting the real 26.2 server and reading `BlockStateBase.getShape` and
//! `BlockStateBase.getInteractionShape` for every one of the 32,366 registered
//! states (`OutlineShapeOracle.java`, walking `Block.BLOCK_STATE_REGISTRY`).
//!
//! Vanilla shapes are **code**-defined, not property-derived: `blocks.json` has
//! no geometry at all, and `vendor/minecraft-data` has no 26.x data (and was
//! measured 92.29% covered and stale for 26.2 on the neighbouring *collision*
//! table). So "boot the jar and ask it" is the only authoritative source, and the
//! dump is committed as the external anchor — the tables are derived from it, so
//! a misread coordinate or a transposed shape index fails
//! [`committed_tables_match_the_committed_dump`] rather than silently shipping.
//!
//! # Why this is not the collision census
//!
//! Three vanilla getters, three defaults, three different answers:
//!
//! * `getShape` (the outline) defaults to `Shapes.block()`
//!   (`BlockBehaviour.java:323-325`);
//! * `getCollisionShape` defaults to
//!   `hasCollision ? state.getShape(…) : Shapes.empty()`
//!   (`BlockBehaviour.java:327-329`);
//! * `getInteractionShape` defaults to `Shapes.empty()`
//!   (`BlockBehaviour.java:295-297`).
//!
//! [`outline_differs_from_collision_for_half_of_all_states`] measures the
//! divergence against the committed collision table: 16,484 of 32,366 states
//! (50.9%), and 5,282 states with empty collision and a non-empty outline. That
//! is the negative control for "just reuse the collision table".
//!
//! # Dump format
//!
//! De-duplicated **in the dumper** (by exact `Double.doubleToRawLongBits` list
//! identity, computed in the JVM) so the anchor is 422 KB and can be committed
//! rather than living in a gitignored scratch file like `shape_java.txt`:
//!
//! ```text
//! C <stateCount>
//! B <firstStateIdOfBlock> <blockName>
//! S <O|X> <shapeIndex> <boxCount> [minX minY minZ maxX maxY maxZ]...   (raw double bits, hex)
//! P <O|X> <startStateId> <shapeIndex>...                              (256 per line, ascending)
//! ```
//!
//! `B` lines carry only block boundaries, and
//! [`dump_block_boundaries_match_the_block_state_table`] reconciles the whole
//! implied state→block-name mapping against `crate::block_states::block_name`,
//! which is generated from `blocks.json` — a second, independently produced
//! artifact. Neither restates the other.
//!
//! # Refreshing after a version bump
//!
//! 1. Re-dump from the server (keep the `#` header when copying over the
//!    committed dump):
//!
//! ```text
//! CACHE="$(cd .cache/mc/26.2 && pwd)"
//! HERE="$(cd crates/protocol/v770/oracle-java && pwd)"
//! docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
//!   CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
//!   cp /oracle/OutlineShapeOracle.java /work/ && javac -cp "$CP" -d /work /work/OutlineShapeOracle.java
//!   java -cp "/work:$CP" OutlineShapeOracle'
//! ```
//!
//! 2. Regenerate the committed tables:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test outline_shapes \
//!     committed_tables_match_dump -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::{block_states, collision_shapes, outline_shapes};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/outline_shapes.rs")
}

/// The committed JVM dump — an external anchor, not gitignored.
const DUMP: &str = include_str!("support/outline_shape_jvm.txt");

/// One box as six `f64`s: `[minX, minY, minZ, maxX, maxY, maxZ]`, exactly as the
/// game produced them.
type Boxd = [f64; 6];

/// The parsed dump.
struct Dump {
    state_count: usize,
    /// `(first state id, block name)` per block, ascending.
    blocks: Vec<(usize, String)>,
    /// Distinct outline shapes, by shape index.
    outline_shapes: Vec<Vec<Boxd>>,
    /// Distinct interaction shapes, by shape index.
    interaction_shapes: Vec<Vec<Boxd>>,
    /// Per-state outline shape index.
    state_outline: Vec<usize>,
    /// Per-state interaction shape index.
    state_interaction: Vec<usize>,
}

impl Dump {
    /// The block name of every state, expanded from the `B` boundaries.
    fn block_names(&self) -> Vec<&str> {
        let mut names = Vec::with_capacity(self.state_count);
        for (index, (start, name)) in self.blocks.iter().enumerate() {
            let end = self
                .blocks
                .get(index + 1)
                .map_or(self.state_count, |(next, _)| *next);
            assert_eq!(*start, names.len(), "block boundaries are not contiguous");
            for _ in *start..end {
                names.push(name.as_str());
            }
        }
        assert_eq!(names.len(), self.state_count, "block boundaries do not cover all states");
        names
    }

    /// The outline boxes of `state`, as the game's `f64`s.
    fn outline(&self, state: usize) -> &[Boxd] {
        &self.outline_shapes[self.state_outline[state]]
    }

    /// The interaction boxes of `state`, as the game's `f64`s.
    fn interaction(&self, state: usize) -> &[Boxd] {
        &self.interaction_shapes[self.state_interaction[state]]
    }
}

/// Decodes one coordinate token — a hex-encoded IEEE-754 `double` bit pattern
/// (`0` is `0.0`) — into the `f64` the game produced. Lossless; narrowing to
/// `f32` happens once, in [`generate`], and is measured by
/// [`f32_narrowing_is_lossless_except_for_the_lectern`].
fn decode_coord(token: &str) -> f64 {
    let bits = u64::from_str_radix(token, 16).expect("coordinate is a hex u64 bit pattern");
    f64::from_bits(bits)
}

fn parse_dump(text: &str) -> Dump {
    let mut state_count = None;
    let mut blocks = Vec::new();
    let mut outline_shapes = Vec::new();
    let mut interaction_shapes = Vec::new();
    let mut state_outline: Vec<usize> = Vec::new();
    let mut state_interaction: Vec<usize> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        match tok.next().expect("record kind") {
            "C" => {
                assert!(state_count.is_none(), "duplicate C record");
                state_count = Some(
                    tok.next()
                        .expect("state count")
                        .parse::<usize>()
                        .expect("state count is a usize"),
                );
            }
            "B" => {
                let start: usize = tok
                    .next()
                    .expect("block start state id")
                    .parse()
                    .expect("start is a usize");
                let name = tok.next().expect("block name").to_owned();
                blocks.push((start, name));
            }
            "S" => {
                let kind = tok.next().expect("shape family");
                let index: usize = tok
                    .next()
                    .expect("shape index")
                    .parse()
                    .expect("shape index is a usize");
                let boxes: usize = tok
                    .next()
                    .expect("box count")
                    .parse()
                    .expect("box count is a usize");
                let coords: Vec<&str> = tok.collect();
                assert_eq!(
                    coords.len(),
                    boxes * 6,
                    "shape {kind}#{index}: expected {} coords for {boxes} boxes, got {}",
                    boxes * 6,
                    coords.len()
                );
                let shape: Vec<Boxd> = (0..boxes)
                    .map(|b| {
                        let s = &coords[b * 6..b * 6 + 6];
                        [
                            decode_coord(s[0]),
                            decode_coord(s[1]),
                            decode_coord(s[2]),
                            decode_coord(s[3]),
                            decode_coord(s[4]),
                            decode_coord(s[5]),
                        ]
                    })
                    .collect();
                let target = match kind {
                    "O" => &mut outline_shapes,
                    "X" => &mut interaction_shapes,
                    other => panic!("unknown shape family {other:?}"),
                };
                assert_eq!(index, target.len(), "S {kind} records are not in index order");
                target.push(shape);
            }
            "P" => {
                let kind = tok.next().expect("shape family");
                let start: usize = tok
                    .next()
                    .expect("start state id")
                    .parse()
                    .expect("start is a usize");
                let target = match kind {
                    "O" => &mut state_outline,
                    "X" => &mut state_interaction,
                    other => panic!("unknown shape family {other:?}"),
                };
                assert_eq!(start, target.len(), "P {kind} records are not in state order");
                for token in tok {
                    target.push(token.parse().expect("shape index is a usize"));
                }
            }
            other => panic!("unknown record kind {other:?} on {line:?}"),
        }
    }

    let state_count = state_count.expect("dump carries a C record");
    assert_eq!(state_outline.len(), state_count, "outline index count");
    assert_eq!(state_interaction.len(), state_count, "interaction index count");
    for &index in &state_outline {
        assert!(index < outline_shapes.len(), "outline index {index} out of range");
    }
    for &index in &state_interaction {
        assert!(
            index < interaction_shapes.len(),
            "interaction index {index} out of range"
        );
    }

    Dump {
        state_count,
        blocks,
        outline_shapes,
        interaction_shapes,
        state_outline,
        state_interaction,
    }
}

/// Narrows one box to the `f32` the committed table stores.
fn narrow(boxd: &Boxd) -> [f32; 6] {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "deliberate, measured narrowing; see f32_narrowing_is_lossless_except_for_the_lectern"
    )]
    boxd.map(|value| value as f32)
}

/// A dedup key for a shape: the ordered `f32` *bit patterns* of its boxes, so
/// equal shapes collapse deterministically without needing `Ord` on floats.
fn shape_key(boxes: &[Boxd]) -> Vec<[u32; 6]> {
    boxes
        .iter()
        .map(|b| narrow(b).map(f32::to_bits))
        .collect()
}

/// Renders the committed `outline_shapes.rs` source from the parsed dump.
///
/// Both families are re-de-duplicated here on the **narrowed `f32`** key rather
/// than trusting the dump's own `f64` indices: narrowing can in principle merge
/// two shapes the game held apart, and this is the only place that would show up
/// (as a shorter shape table than the dump's). Distinct shapes are numbered in
/// ascending block-state id order, so the tables are deterministic and
/// independent of dump ordering.
fn generate(dump: &Dump) -> String {
    let count = dump.state_count;

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test outline_shapes -- --ignored`\n\
         // from tests/support/outline_shape_jvm.txt (a headless 26.2 server dump of\n\
         // BlockStateBase.getShape()/getInteractionShape(), protocol 776 / Minecraft 26.2).\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated block outline/interaction-shape tables for protocol 776\n\
         //! (Minecraft 26.2), indexed by global block-state id. Consumed by\n\
         //! [`crate::outline_shapes`].\n\n",
    );
    out.push_str("use lodestone_model::BlockAabb;\n\n");

    let _ = writeln!(out, "/// Number of block states (ids are `0..STATE_COUNT`).");
    let _ = writeln!(out, "pub const STATE_COUNT: u32 = {count};\n");

    emit_family(
        &mut out,
        count,
        "OUTLINE",
        "outline (`BlockStateBase.getShape`)",
        (0..count).map(|state| dump.outline(state)),
    );
    emit_family(
        &mut out,
        count,
        "INTERACTION",
        "interaction (`BlockStateBase.getInteractionShape`)",
        (0..count).map(|state| dump.interaction(state)),
    );

    out
}

/// Emits one shape family's `<NAME>_SHAPES` and `STATE_<NAME>` statics.
fn emit_family<'a>(
    out: &mut String,
    count: usize,
    name: &str,
    description: &str,
    shapes: impl Iterator<Item = &'a [Boxd]>,
) {
    let mut index: BTreeMap<Vec<[u32; 6]>, usize> = BTreeMap::new();
    let mut distinct: Vec<Vec<[f32; 6]>> = Vec::new();
    let mut per_state: Vec<usize> = Vec::with_capacity(count);
    for boxes in shapes {
        let key = shape_key(boxes);
        let slot = *index.entry(key).or_insert_with(|| {
            distinct.push(boxes.iter().map(narrow).collect());
            distinct.len() - 1
        });
        per_state.push(slot);
    }
    assert_eq!(per_state.len(), count, "{name}: per-state length");
    assert!(
        distinct.len() <= usize::from(u16::MAX) + 1,
        "{name}: more than u16::MAX distinct shapes"
    );

    let _ = writeln!(
        out,
        "/// De-duplicated distinct {description} shapes ({} of them), indexed by shape index.",
        distinct.len()
    );
    let _ = writeln!(
        out,
        "pub static {name}_SHAPES: [&[BlockAabb]; {}] = [",
        distinct.len()
    );
    for boxes in &distinct {
        if boxes.is_empty() {
            out.push_str("    &[],\n");
            continue;
        }
        out.push_str("    &[");
        for (i, b) in boxes.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            // `{:?}` emits the shortest decimal that parses back to the same
            // f32, so the literal is human-readable *and* bit-exact.
            let _ = write!(
                out,
                "BlockAabb {{ min: [{:?}, {:?}, {:?}], max: [{:?}, {:?}, {:?}] }}",
                b[0], b[1], b[2], b[3], b[4], b[5]
            );
        }
        out.push_str("],\n");
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Per-state shape index into [`{name}_SHAPES`], indexed by block-state id."
    );
    let _ = writeln!(out, "pub static STATE_{name}: [u16; {count}] = [");
    for chunk in per_state.chunks(16) {
        out.push_str("    ");
        for slot in chunk {
            let _ = write!(out, "{slot}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n\n");
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed tables (anchored to the committed dump)
// ---------------------------------------------------------------------------

/// Finds the first state id whose block name matches `name`, via the committed
/// block-state table — robust to id shifts across data bumps.
fn first_id_named(name: &str) -> u32 {
    (0..block_states::STATE_COUNT)
        .find(|&id| block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} present in the block-state table"))
}

/// All state ids whose block name matches `name`.
fn states_named(name: &str) -> impl Iterator<Item = u32> + '_ {
    (0..block_states::STATE_COUNT).filter(move |&id| block_states::block_name(id) == Some(name))
}

/// The strongest check: every box the shipped accessors return equals the
/// narrowed `f32` of the exact `double` the real server produced, for both
/// families, for all 32,366 states. Non-vacuous by construction.
#[test]
fn committed_tables_match_the_committed_dump() {
    let dump = parse_dump(DUMP);
    assert_eq!(
        dump.state_count,
        outline_shapes::STATE_COUNT as usize,
        "dump/table state count mismatch"
    );
    for state in 0..dump.state_count {
        let id = state as u32;
        let expected_outline: Vec<[f32; 6]> = dump.outline(state).iter().map(narrow).collect();
        let actual_outline = outline_shapes::outline_boxes(id)
            .unwrap_or_else(|| panic!("outline for state {id} missing"));
        assert_eq!(
            actual_outline.len(),
            expected_outline.len(),
            "outline box count for state {id} ({:?})",
            block_states::block_name(id)
        );
        for (actual, expected) in actual_outline.iter().zip(&expected_outline) {
            assert_eq!(
                [
                    actual.min[0],
                    actual.min[1],
                    actual.min[2],
                    actual.max[0],
                    actual.max[1],
                    actual.max[2]
                ],
                *expected,
                "outline box for state {id} ({:?})",
                block_states::block_name(id)
            );
        }

        let expected_inter: Vec<[f32; 6]> = dump.interaction(state).iter().map(narrow).collect();
        let actual_inter = outline_shapes::interaction_boxes(id)
            .unwrap_or_else(|| panic!("interaction shape for state {id} missing"));
        assert_eq!(
            actual_inter.len(),
            expected_inter.len(),
            "interaction box count for state {id}"
        );
        for (actual, expected) in actual_inter.iter().zip(&expected_inter) {
            assert_eq!(
                [
                    actual.min[0],
                    actual.min[1],
                    actual.min[2],
                    actual.max[0],
                    actual.max[1],
                    actual.max[2]
                ],
                *expected,
                "interaction box for state {id}"
            );
        }
    }
    assert_eq!(dump.state_count, 32_366, "expected 32,366 block states");
}

/// Reconciles the dump's block boundaries against `block_states::block_name`,
/// which is generated from `blocks.json` — a second, independently produced
/// Mojang artifact. All 32,366 states must agree. This is what makes indexing
/// these tables by block-state id safe, and it is the check the block registry's
/// alphabetical-vs-registration confusion needed and did not have.
#[test]
fn dump_block_boundaries_match_the_block_state_table() {
    let dump = parse_dump(DUMP);
    let names = dump.block_names();
    assert_eq!(dump.blocks.len(), 1_196, "expected 1,196 blocks");
    for (state, name) in names.iter().enumerate() {
        assert_eq!(
            block_states::block_name(state as u32),
            Some(*name),
            "blocks.json and the JVM dump disagree at state {state}"
        );
    }
}

#[test]
fn count_matches_block_state_table() {
    assert_eq!(
        outline_shapes::STATE_COUNT,
        block_states::STATE_COUNT,
        "outline table must cover exactly the block-state id space"
    );
}

#[test]
fn ids_are_contiguous_and_out_of_range_is_none() {
    let count = outline_shapes::STATE_COUNT;
    for id in 0..count {
        assert!(
            outline_shapes::outline_boxes(id).is_some(),
            "outline for id {id} did not resolve"
        );
        assert!(
            outline_shapes::interaction_boxes(id).is_some(),
            "interaction shape for id {id} did not resolve"
        );
    }
    assert!(outline_shapes::outline_boxes(count).is_none());
    assert!(outline_shapes::outline_boxes(u32::MAX).is_none());
    assert!(outline_shapes::interaction_boxes(count).is_none());
    assert!(outline_shapes::interaction_boxes(u32::MAX).is_none());
}

/// The measured cost of storing `f32` instead of the game's `f64`: four
/// coordinate values, used by `minecraft:lectern` and nothing else, are not
/// exactly representable. Everything else round-trips bit-exactly.
///
/// Pinned rather than asserted-away so that a future version introducing a
/// *materially* inexact coordinate fails here instead of quietly losing
/// precision. The bound is deliberately tight.
#[test]
fn f32_narrowing_is_lossless_except_for_the_lectern() {
    let dump = parse_dump(DUMP);
    let names = dump.block_names();
    let mut inexact: BTreeMap<String, usize> = BTreeMap::new();
    let mut worst = 0.0f64;
    for state in 0..dump.state_count {
        for boxd in dump.outline(state).iter().chain(dump.interaction(state)) {
            for &value in boxd {
                let error = (f64::from(value as f32) - value).abs();
                if error != 0.0 {
                    *inexact.entry(names[state].to_owned()).or_default() += 1;
                    worst = worst.max(error);
                }
            }
        }
    }
    assert_eq!(
        inexact.keys().collect::<Vec<_>>(),
        vec!["minecraft:lectern"],
        "a block other than the lectern now has coordinates that are not f32-exact"
    );
    assert!(
        worst < 3e-9,
        "worst f32 narrowing error grew to {worst:e} blocks"
    );
}

/// The negative control for "just use the collision table". If someone deletes
/// this census and points block picking at `collision_shapes`, half the states
/// get the wrong answer — and 5,282 of them (kelp, seagrass, cobweb, torches,
/// redstone wire, fire, every plant) become untargetable entirely.
#[test]
fn outline_differs_from_collision_for_half_of_all_states() {
    let mut differ = 0usize;
    let mut empty_collision_real_outline = 0usize;
    let mut real_collision_empty_outline = 0usize;
    for id in 0..outline_shapes::STATE_COUNT {
        let outline = outline_shapes::outline_boxes(id).expect("outline resolves");
        let collision = collision_shapes::collision_boxes(id).expect("collision resolves");
        let same = outline.len() == collision.len()
            && outline.iter().zip(collision).all(|(a, b)| {
                a.min[0] == b.min[0]
                    && a.min[1] == b.min[1]
                    && a.min[2] == b.min[2]
                    && a.max[0] == b.max[0]
                    && a.max[1] == b.max[1]
                    && a.max[2] == b.max[2]
            });
        if !same {
            differ += 1;
        }
        if collision.is_empty() && !outline.is_empty() {
            empty_collision_real_outline += 1;
        }
        if !collision.is_empty() && outline.is_empty() {
            real_collision_empty_outline += 1;
        }
    }
    assert_eq!(differ, 16_484, "outline/collision divergence changed");
    assert_eq!(
        empty_collision_real_outline, 5_282,
        "states with no collision but a real outline changed"
    );
    assert_eq!(
        real_collision_empty_outline, 0,
        "a state gained collision without an outline, which vanilla's default \
         `hasCollision ? getShape() : empty()` makes impossible"
    );
}

// ---------------------------------------------------------------------------
// The specific shapes the pick ray needs, each hand-checked against the
// decompiled source
// ---------------------------------------------------------------------------

fn only_box(boxes: &[lodestone_model::BlockAabb]) -> [f32; 6] {
    assert_eq!(boxes.len(), 1, "expected exactly one box, got {boxes:?}");
    let b = boxes[0];
    [b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2]]
}

const FULL_CUBE: [f32; 6] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];

/// `AirBlock.getShape` → `Shapes.empty()` (`AirBlock.java:29-32`), and
/// `LiquidBlock.getShape` → `Shapes.empty()` (`LiquidBlock.java:145-147`). This
/// is why block picking cannot be "the cell is not air": *fluids* are empty too,
/// and vanilla runs the pick with `Fluid.NONE`.
#[test]
fn air_and_fluids_have_an_empty_outline() {
    for name in [
        "minecraft:air",
        "minecraft:cave_air",
        "minecraft:void_air",
        "minecraft:water",
        "minecraft:lava",
        "minecraft:bubble_column",
    ] {
        for id in states_named(name) {
            assert!(
                outline_shapes::outline_boxes(id).expect("resolves").is_empty(),
                "{name} (state {id}) must have an empty outline"
            );
        }
    }
}

/// `KelpBlock`'s shape is `Block.column(16.0, 0.0, 9.0)` (`KelpBlock.java:24`)
/// and `Block.column(sizeXZ, minY, maxY)` is
/// `box(8 - sizeXZ/2, minY, 8 - sizeXZ/2, 8 + sizeXZ/2, maxY, 8 + sizeXZ/2)` in
/// sixteenths (`Block.java:176-184`), i.e. `[0, 0, 0, 1, 9/16, 1]`. Non-empty,
/// which is what makes kelp targetable and breakable — and its collision shape is
/// empty, which is why the collision table cannot answer this.
#[test]
fn kelp_outlines_to_nine_sixteenths_and_collides_with_nothing() {
    for id in states_named("minecraft:kelp") {
        assert_eq!(
            only_box(outline_shapes::outline_boxes(id).expect("resolves")),
            [0.0, 0.0, 0.0, 1.0, 0.5625, 1.0],
            "kelp (state {id}) outline"
        );
        assert!(
            collision_shapes::collision_boxes(id)
                .expect("resolves")
                .is_empty(),
            "kelp (state {id}) must have no collision"
        );
    }
}

/// `SeagrassBlock`'s shape is `Block.column(12.0, 0.0, 12.0)`
/// (`SeagrassBlock.java:29`) → `[2/16, 0, 2/16, 14/16, 12/16, 14/16]`.
#[test]
fn seagrass_outlines_to_twelve_sixteenths_inset() {
    let id = first_id_named("minecraft:seagrass");
    assert_eq!(
        only_box(outline_shapes::outline_boxes(id).expect("resolves")),
        [0.125, 0.0, 0.125, 0.875, 0.75, 0.875],
        "seagrass outline"
    );
}

/// `WebBlock` overrides no shape getter at all, so cobweb inherits
/// `getShape`'s `Shapes.block()` default while `noCollission()` empties its
/// collision shape. The cleanest single demonstration that the two censuses are
/// not interchangeable.
#[test]
fn cobweb_outlines_to_a_full_cube_and_collides_with_nothing() {
    let id = first_id_named("minecraft:cobweb");
    assert_eq!(
        only_box(outline_shapes::outline_boxes(id).expect("resolves")),
        FULL_CUBE,
        "cobweb outline"
    );
    assert!(
        collision_shapes::collision_boxes(id)
            .expect("resolves")
            .is_empty(),
        "cobweb must have no collision"
    );
}

/// `SlabBlock`'s shapes are `column(16, 0, 8)` / `column(16, 8, 16)` /
/// `Shapes.block()` for BOTTOM / TOP / DOUBLE (`SlabBlock.java:35-36, 59-65`).
/// This is the shape the current unit-cube selection box gets visibly wrong.
#[test]
fn slabs_outline_to_a_half_block() {
    let mut seen: Vec<[u32; 6]> = Vec::new();
    for id in states_named("minecraft:stone_slab") {
        let key = only_box(outline_shapes::outline_boxes(id).expect("resolves")).map(f32::to_bits);
        if !seen.contains(&key) {
            seen.push(key);
        }
    }
    let expected = [
        [0.0, 0.0, 0.0, 1.0, 0.5, 1.0], // BOTTOM
        [0.0, 0.5, 0.0, 1.0, 1.0, 1.0], // TOP
        FULL_CUBE,                      // DOUBLE
    ];
    assert_eq!(
        seen.len(),
        expected.len(),
        "a slab must have exactly three distinct outlines"
    );
    for want in expected {
        assert!(
            states_named("minecraft:stone_slab")
                .any(|id| only_box(outline_shapes::outline_boxes(id).expect("resolves")) == want),
            "no stone_slab state outlines to {want:?}"
        );
    }
}

/// Walls build their outline with `makeShapes(16.0F, 14.0F)` and their collision
/// with `makeShapes(24.0F, 24.0F)` (`WallBlock.java:66-67`), so a wall's outline
/// tops out at `y = 1.0` while its collision reaches `y = 1.5`. Using the
/// collision shape for selection would draw the box half a block above the wall.
///
/// And the two states per wall with `up=false` and all four sides `NONE` fold to
/// `Shapes.empty()` (`WallBlock.java:75-86`); because that shape function ignores
/// `WATERLOGGED` it is exactly two states, waterlogged and not.
#[test]
fn wall_outlines_stop_at_one_while_collision_reaches_one_and_a_half() {
    let mut outline_top = 0.0f32;
    let mut collision_top = 0.0f32;
    let mut empty_outlines = 0usize;
    for id in states_named("minecraft:cobblestone_wall") {
        let outline = outline_shapes::outline_boxes(id).expect("resolves");
        if outline.is_empty() {
            empty_outlines += 1;
        }
        for b in outline {
            outline_top = outline_top.max(b.max[1]);
        }
        for b in collision_shapes::collision_boxes(id).expect("resolves") {
            collision_top = collision_top.max(b.max[1]);
        }
    }
    assert_eq!(outline_top, 1.0, "wall outline height");
    assert_eq!(collision_top, 1.5, "wall collision height");
    assert_eq!(
        empty_outlines, 2,
        "a wall has exactly two connectionless states, waterlogged and not"
    );
}

/// `LightBlock.getShape` is
/// `context.isHoldingItem(Items.LIGHT) ? Shapes.block() : Shapes.empty()`
/// (`LightBlock.java:66-68`), and the census dumps every shape with
/// `CollisionContext.empty()`. So the table's answer for `minecraft:light` is
/// **empty** — the correct not-holding-a-light answer, and a genuine limit of a
/// context-free table. Pinned so nobody "fixes" it to a cube.
#[test]
fn light_blocks_outline_to_nothing_because_the_census_holds_no_item() {
    for id in states_named("minecraft:light") {
        assert!(
            outline_shapes::outline_boxes(id).expect("resolves").is_empty(),
            "light (state {id}) outlines to nothing without a light item in hand"
        );
    }
    // `barrier`, by contrast, really is a full cube with no context involved —
    // so "invisible" is not the discriminator, `isHoldingItem` is.
    assert_eq!(
        only_box(
            outline_shapes::outline_boxes(first_id_named("minecraft:barrier")).expect("resolves")
        ),
        FULL_CUBE,
        "barrier outline"
    );
    // …and `structure_void` is a small centred cube, not a full one.
    assert_eq!(
        only_box(
            outline_shapes::outline_boxes(first_id_named("minecraft:structure_void"))
                .expect("resolves")
        ),
        [0.3125, 0.3125, 0.3125, 0.6875, 0.6875, 0.6875],
        "structure_void outline"
    );
}

/// `getInteractionShape` defaults to `Shapes.empty()`
/// (`BlockBehaviour.java:295-297`) and only four block families override it in
/// 26.2. Pinned as a completeness statement: if a fifth appears, this fails and
/// the docs' claim about which blocks refine their hit face gets updated.
#[test]
fn only_four_block_families_have_an_interaction_shape() {
    let mut with_shape = std::collections::BTreeSet::new();
    for id in 0..outline_shapes::STATE_COUNT {
        if !outline_shapes::interaction_boxes(id)
            .expect("resolves")
            .is_empty()
        {
            with_shape.insert(block_states::block_name(id).expect("named"));
        }
    }
    assert_eq!(
        with_shape.into_iter().collect::<Vec<_>>(),
        vec![
            "minecraft:cauldron",
            "minecraft:composter",
            "minecraft:hopper",
            "minecraft:lava_cauldron",
            "minecraft:powder_snow_cauldron",
            "minecraft:scaffolding",
            "minecraft:water_cauldron",
        ],
        "the set of blocks with a non-empty interaction shape changed"
    );
}

/// Only a tenth of block states are actually a full unit cube, which is the size
/// of the defect a unit-cube selection box carries.
#[test]
fn most_states_are_not_a_full_cube() {
    let cubes = (0..outline_shapes::STATE_COUNT)
        .filter(|&id| {
            let boxes = outline_shapes::outline_boxes(id).expect("resolves");
            boxes.len() == 1 && only_box(boxes) == FULL_CUBE
        })
        .count();
    assert_eq!(cubes, 3_328, "full-cube outline count changed");
    assert!(
        cubes * 5 < outline_shapes::STATE_COUNT as usize,
        "fewer than a fifth of states should be full cubes"
    );
}

/// Outline boxes are **not** confined to the unit cube: `pitcher_crop` reaches
/// below zero and the census spans `-0.25..=1.25`. A consumer must not clamp.
#[test]
fn outline_boxes_escape_the_unit_cube() {
    let mut lowest = 0.0f32;
    let mut highest = 1.0f32;
    for id in 0..outline_shapes::STATE_COUNT {
        for b in outline_shapes::outline_boxes(id).expect("resolves") {
            for axis in 0..3 {
                lowest = lowest.min(b.min[axis]);
                highest = highest.max(b.max[axis]);
            }
        }
    }
    assert_eq!(lowest, -0.25, "lowest outline coordinate");
    assert_eq!(highest, 1.25, "highest outline coordinate");
}

// ---------------------------------------------------------------------------
// Drift guard (regenerates from the committed dump; `#[ignore]`d for parity
// with the other generated tables, though it needs no external artifact)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "regenerates/verifies the committed tables; run explicitly"]
fn committed_tables_match_dump() {
    let dump = parse_dump(DUMP);
    let generated = generate(&dump);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write committed tables");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed tables present");
    assert_eq!(
        generated, committed,
        "src/generated/outline_shapes.rs is stale vs the JVM dump; regenerate with \
         LODESTONE_REGEN=1"
    );
}
