//! Collision-shape table: hermetic checks over the committed table, plus an
//! `#[ignore]`d drift guard that regenerates it from the authoritative oracle
//! dump and asserts byte-for-byte equality (modelled on the block-state table
//! and `xtask gen-packet-ids --check`). The generator lives here so the
//! checked-in table can never silently drift from the game data.
//!
//! The dump (`shape_java.txt`, ~5.7 MB, gitignored like `.cache/mc`) is produced
//! by `impl-physics`'s `ShapeOracle.java`, which boots the real 26.2 server and
//! dumps `getCollisionShape(...).toAabbs()` for every one of the 32,366 states.
//! We own the *data*; physics owns the oracle and the consuming `CollisionView`.
//!
//! Regenerate the committed table after a data bump with:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test collision_shapes \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::block_states;
use lodestone_data::collision_shapes;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The authoritative oracle dump (gitignored local artifact, produced by
/// `impl-physics`'s `ShapeOracle.java`).
fn dump_path() -> PathBuf {
    manifest_dir().join("../lodestone-physics/oracle-java/shape_java.txt")
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/collision_shapes.rs")
}

// ---------------------------------------------------------------------------
// Generator (shared by regen and the drift check)
// ---------------------------------------------------------------------------

/// One collision box as six `f32`s: `[minX, minY, minZ, maxX, maxY, maxZ]`.
type Boxf = [f32; 6];

/// Decodes one coordinate token — a hex-encoded IEEE-754 `double` bit pattern
/// (`0` is `0.0`) — into `f32`. Every value in the dump is exactly representable
/// in `f32` (asserted by the drift check), so this is lossless.
fn decode_coord(token: &str) -> f32 {
    let bits = u64::from_str_radix(token, 16).expect("coordinate is a hex u64 bit pattern");
    f64::from_bits(bits) as f32
}

/// Parses the dump into `boxes_by_id[id]` = that state's boxes, in file order.
/// Robust to line ordering: it indexes by the leading id and asserts density.
fn parse_dump(text: &str) -> Vec<Vec<Boxf>> {
    // First pass: find the max id so we can size the vec.
    let mut rows: Vec<(usize, Vec<Boxf>)> = Vec::new();
    let mut max_id = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut tok = line.split_whitespace();
        let id: usize = tok
            .next()
            .expect("state id")
            .parse()
            .expect("id is an integer");
        let _name = tok.next().expect("block name");
        let n: usize = tok
            .next()
            .expect("box count")
            .parse()
            .expect("count is an integer");
        let coords: Vec<&str> = tok.collect();
        assert_eq!(
            coords.len(),
            n * 6,
            "state {id}: expected {} coords for {n} boxes, got {}",
            n * 6,
            coords.len()
        );
        let boxes: Vec<Boxf> = (0..n)
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
        max_id = max_id.max(id);
        rows.push((id, boxes));
    }

    let count = max_id + 1;
    assert_eq!(
        rows.len(),
        count,
        "dump ids are not dense (count {} vs max+1 {count})",
        rows.len()
    );
    let mut by_id: Vec<Option<Vec<Boxf>>> = (0..count).map(|_| None).collect();
    for (id, boxes) in rows {
        assert!(by_id[id].is_none(), "duplicate id {id} in dump");
        by_id[id] = Some(boxes);
    }
    by_id
        .into_iter()
        .map(|slot| slot.expect("every id in 0..count present in dump"))
        .collect()
}

/// A dedup key for a shape: the ordered `f32` *bit patterns* of its boxes, so
/// equal shapes collapse deterministically without needing `Ord` on `f32`.
fn shape_key(boxes: &[Boxf]) -> Vec<[u32; 6]> {
    boxes.iter().map(|b| b.map(f32::to_bits)).collect()
}

/// Renders the committed `collision_shapes.rs` source from the parsed dump.
///
/// Deterministic: distinct shapes are numbered in ascending block-state id
/// order (independent of dump line order), so the empty shape is index 0 iff
/// state 0 (air) is empty.
fn generate(text: &str) -> String {
    let boxes_by_id = parse_dump(text);
    let count = boxes_by_id.len();

    // Assign distinct-shape indices in ascending id order.
    let mut shape_index: BTreeMap<Vec<[u32; 6]>, usize> = BTreeMap::new();
    let mut distinct: Vec<Vec<Boxf>> = Vec::new();
    let mut state_shape: Vec<usize> = Vec::with_capacity(count);
    for boxes in &boxes_by_id {
        let key = shape_key(boxes);
        let idx = *shape_index.entry(key).or_insert_with(|| {
            distinct.push(boxes.clone());
            distinct.len() - 1
        });
        state_shape.push(idx);
    }

    assert!(
        distinct.len() <= usize::from(u16::MAX) + 1,
        "more than u16::MAX distinct shapes"
    );

    // --- emit -------------------------------------------------------------
    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test collision_shapes -- --ignored`\n\
         // from crates/lodestone-physics/oracle-java/shape_java.txt (real 26.2 server dump,\n\
         // protocol 776). DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the\n\
         // test module docs).\n",
    );
    out.push_str(
        "//! Generated block collision-shape table for protocol 776 (Minecraft 26.2).\n//!\n",
    );
    out.push_str(
        "//! Raw rodata arrays consumed by [`crate::collision_shapes`]. Coordinates are\n\
         //! `f32` (lossless for every value in the dump), so the whole table lives in\n\
         //! rodata with zero heap.\n\n",
    );

    out.push_str("use crate::collision_shapes::Aabb;\n\n");

    let _ = writeln!(
        out,
        "/// Number of block states (ids are `0..STATE_COUNT`)."
    );
    let _ = writeln!(out, "pub const STATE_COUNT: u32 = {count};\n");

    let _ = writeln!(
        out,
        "/// De-duplicated distinct collision shapes ({} of them), indexed by shape index.",
        distinct.len()
    );
    let _ = writeln!(out, "pub static SHAPES: [&[Aabb]; {}] = [", distinct.len());
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
            let _ = write!(
                out,
                "Aabb {{ min: [{:?}, {:?}, {:?}], max: [{:?}, {:?}, {:?}] }}",
                b[0], b[1], b[2], b[3], b[4], b[5]
            );
        }
        out.push_str("],\n");
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Per-state shape index into [`SHAPES`], indexed by block-state id."
    );
    let _ = writeln!(out, "pub static STATE_SHAPE: [u16; {count}] = [");
    for chunk in state_shape.chunks(16) {
        out.push_str("    ");
        for idx in chunk {
            let _ = write!(out, "{idx}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n");

    out
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed table (no dump needed)
// ---------------------------------------------------------------------------

/// Finds the first state id whose block name matches `name`, via the committed
/// block-state table — robust to id shifts across data bumps.
fn first_id_named(name: &str) -> Option<u32> {
    (0..block_states::STATE_COUNT).find(|&id| block_states::block_name(id) == Some(name))
}

/// All state ids whose block name matches `name`.
fn states_named(name: &str) -> impl Iterator<Item = u32> + '_ {
    (0..block_states::STATE_COUNT).filter(move |&id| block_states::block_name(id) == Some(name))
}

#[test]
fn count_matches_block_state_table() {
    assert_eq!(
        collision_shapes::STATE_COUNT,
        block_states::STATE_COUNT,
        "collision table must cover exactly the block-state id space"
    );
}

#[test]
fn validated_collision_lookup_is_total() {
    let lookup: fn(block_states::StateId) -> &'static [collision_shapes::Aabb] =
        collision_shapes::collision_boxes;
    let air = block_states::StateId::new(block_states::air_state_id()).expect("air exists");
    assert!(lookup(air).is_empty());
    let stone = block_states::StateId::new(first_id_named("minecraft:stone").expect("stone exists"))
        .expect("stone validates");
    assert_eq!(lookup(stone), &[collision_shapes::Aabb { min: [0.0; 3], max: [1.0; 3] }]);
}

fn validated(id: u32) -> block_states::StateId {
    block_states::StateId::new(id).expect("known census state")
}

#[test]
fn validated_ids_cover_the_table_and_invalid_raw_ids_stop_at_boundary() {
    let count = collision_shapes::STATE_COUNT;
    for id in 0..count {
        let state = block_states::StateId::new(id).expect("every census id validates");
        let _ = collision_shapes::collision_boxes(state);
    }
    assert!(block_states::StateId::new(count).is_none());
    assert!(block_states::StateId::new(u32::MAX).is_none());
}

#[test]
fn air_and_stone_have_the_expected_shapes() {
    // Air: no collision at all (empty slice, not a zero box).
    assert_eq!(collision_shapes::collision_boxes(validated(0)), &[][..]);
    // Stone: a single full unit cube.
    let stone = collision_shapes::collision_boxes(validated(1));
    assert_eq!(
        stone,
        &[collision_shapes::Aabb {
            min: [0.0, 0.0, 0.0],
            max: [1.0, 1.0, 1.0]
        }]
    );
}

#[test]
fn no_collision_blocks_are_empty() {
    // These are load-bearing: an empty shape means "walk/fall through", and a
    // stray zero box would wrongly block movement.
    for name in [
        "minecraft:water",
        "minecraft:lava",
        "minecraft:cobweb",
        "minecraft:air",
    ] {
        let id = first_id_named(name).unwrap_or_else(|| panic!("{name} present"));
        assert_eq!(
            collision_shapes::collision_boxes(validated(id)),
            &[][..],
            "{name} (id {id}) must have no collision boxes"
        );
    }
}

#[test]
fn fence_and_wall_height_is_1_5_never_1_0() {
    // The single most easily-lost fact: fences and walls are 1.5 tall so the
    // 0.6 auto-step cannot mount them. A regeneration that truncated to 1.0
    // would break navigation subtly, so pin it per-state.
    //
    // Fences are *uniformly* 1.5 (the post is always present). Walls are more
    // subtle — the authoritative dump shows some `up=false` no-connection wall
    // states with *empty* collision (0.0). So the exact, faithful invariant is:
    // whenever a fence/wall state collides at all, it is 1.5 tall, never 1.0.
    let fences_uniform = ["minecraft:oak_fence", "minecraft:nether_brick_fence"];
    let also_may_be_empty = ["minecraft:cobblestone_wall"];

    for name in fences_uniform {
        let mut checked = 0usize;
        for id in states_named(name) {
            let boxes = collision_shapes::collision_boxes(validated(id));
            assert!(
                !boxes.is_empty(),
                "{name} (id {id}) unexpectedly has no collision"
            );
            let max_y = boxes.iter().map(|b| b.max[1]).fold(0.0f32, f32::max);
            assert_eq!(
                max_y, 1.5,
                "{name} (id {id}) height must be 1.5, got {max_y}"
            );
            checked += 1;
        }
        assert!(checked > 0, "no states found for {name}");
    }

    for name in also_may_be_empty {
        let mut reached_1_5 = false;
        let mut checked = 0usize;
        for id in states_named(name) {
            let boxes = collision_shapes::collision_boxes(validated(id));
            let max_y = boxes.iter().map(|b| b.max[1]).fold(0.0f32, f32::max);
            assert!(
                max_y == 0.0 || max_y == 1.5,
                "{name} (id {id}) height must be empty (0.0) or 1.5, never {max_y} — a 1.0 cube would be an auto-step regression"
            );
            reached_1_5 |= max_y == 1.5;
            checked += 1;
        }
        assert!(checked > 0, "no states found for {name}");
        assert!(
            reached_1_5,
            "{name} never reaches 1.5 — the post shape is missing"
        );
    }
}

#[test]
fn soul_sand_is_0_875_tall() {
    let id = first_id_named("minecraft:soul_sand").expect("soul_sand present");
    let boxes = collision_shapes::collision_boxes(validated(id));
    assert_eq!(
        boxes,
        &[collision_shapes::Aabb {
            min: [0.0, 0.0, 0.0],
            max: [1.0, 0.875, 1.0]
        }],
        "soul_sand (id {id}) is a single 0.875-tall box"
    );
}

#[test]
fn every_shape_index_is_in_range() {
    // A corrupt STATE_SHAPE entry would panic at lookup; prove none does.
    for id in 0..collision_shapes::STATE_COUNT {
        let _ = collision_shapes::collision_boxes(validated(id));
    }
}

// ---------------------------------------------------------------------------
// Drift guard + corpus report (requires the oracle dump)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires the physics oracle dump; regenerates and checks the committed table"]
fn committed_table_matches_dump() {
    let text = std::fs::read_to_string(dump_path())
        .expect("shape_java.txt present under crates/lodestone-physics/oracle-java");
    let generated = generate(&text);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write committed table");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed table present");
    assert_eq!(
        generated, committed,
        "src/generated/collision_shapes.rs is stale vs the oracle dump; regenerate with LODESTONE_REGEN=1"
    );

    // --- corpus report ----------------------------------------------------
    let boxes_by_id = parse_dump(&text);
    let dump_states = boxes_by_id.len();

    // f32 exactness: every coordinate must round-trip through f32 losslessly.
    let mut lossy = 0usize;
    let mut distinct_coords = std::collections::BTreeSet::new();
    for line in text.lines() {
        let mut tok = line.split_whitespace();
        let (_id, _name, n) = (tok.next(), tok.next(), tok.next());
        let n: usize = n.unwrap().parse().unwrap();
        let coords: Vec<&str> = tok.collect();
        for token in &coords[..n * 6] {
            let bits = u64::from_str_radix(token, 16).unwrap();
            let d = f64::from_bits(bits);
            distinct_coords.insert(bits);
            if f64::from(d as f32) != d {
                lossy += 1;
            }
        }
    }

    let distinct_shapes = collision_shapes_distinct_count();
    let deduped_boxes: usize = distinct_shape_boxes();
    let total_boxes: usize = boxes_by_id.iter().map(Vec::len).sum();

    // rodata estimate (bytes): STATE_SHAPE (u16 each) + SHAPES slice headers
    // (ptr+len = 16 B each on 64-bit) + deduped Aabb data (6 * f32 = 24 B each).
    let state_shape_bytes = dump_states * 2;
    let headers_bytes = distinct_shapes * 16;
    let aabb_bytes = deduped_boxes * 24;
    let rodata = state_shape_bytes + headers_bytes + aabb_bytes;

    println!("=== COLLISION-SHAPE TABLE REPORT ===");
    println!(
        "states (dump / table)    : {dump_states} / {}",
        collision_shapes::STATE_COUNT
    );
    println!(
        "max id + 1 == count      : {} -> {}",
        dump_states,
        dump_states == collision_shapes::STATE_COUNT as usize
    );
    println!("distinct shapes          : {distinct_shapes}");
    println!("distinct coord values    : {}", distinct_coords.len());
    println!("coords not exact in f32  : {lossy}");
    println!("boxes (total / deduped)  : {total_boxes} / {deduped_boxes}");
    println!(
        "rodata estimate          : {rodata} bytes ({:.1} KiB) = STATE_SHAPE {state_shape_bytes} + headers {headers_bytes} + aabbs {aabb_bytes}",
        rodata as f64 / 1024.0
    );
    println!("====================================");

    assert_eq!(dump_states, collision_shapes::STATE_COUNT as usize);
    assert_eq!(
        lossy, 0,
        "some coordinate is not exactly representable in f32"
    );
}

/// Distinct-shape count read back from the committed table.
fn collision_shapes_distinct_count() -> usize {
    // The committed SHAPES length is the distinct count; recover it via the max
    // index actually used plus one is not enough (unused entries), so read the
    // set of indices present. All indices in STATE_SHAPE point into SHAPES.
    let mut seen = std::collections::BTreeSet::new();
    for id in 0..collision_shapes::STATE_COUNT {
        // Reconstruct the index by pointer identity is awkward; instead count
        // distinct shape *contents*.
        seen.insert(format!(
            "{:?}",
            collision_shapes::collision_boxes(validated(id))
        ));
    }
    seen.len()
}

/// Total de-duplicated boxes across the distinct shapes in the committed table.
fn distinct_shape_boxes() -> usize {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for id in 0..collision_shapes::STATE_COUNT {
        let boxes = collision_shapes::collision_boxes(validated(id));
        seen.entry(format!("{boxes:?}")).or_insert(boxes.len());
    }
    seen.values().sum()
}
