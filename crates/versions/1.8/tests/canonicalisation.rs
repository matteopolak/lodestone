//! The canonicalisation gate for protocol 47 (multi-version support, unit U3).
//!
//! # What this exists to catch
//!
//! 1.8's `map_chunk` carries a 16-bit `(blockId << 4) | meta` **composite** per
//! cell. Until U3 this crate stored that composite straight into the
//! version-free [`PalettedContainer`], and its module header asserted the
//! composite "*is* the natural block-state id". It is not. The container is
//! version-free and accepts any `u32`, so nothing in this crate could see the
//! error — but its consumers (the mesher's atlas, collision) are built from the
//! **canonical 26.2** block-state space, in which the composite for 1.8 bedrock
//! (`7 << 4` = 112) names a completely different block. Every 1.8 world was
//! therefore rendering and colliding as the wrong blocks while every test was
//! green.
//!
//! # Where the expected values come from (none of them from this repo's code)
//!
//! Per CLAUDE.md's evidence standard, an expected value must originate outside
//! the code under test. Three independent outside anchors are used, and the
//! naive alternative is required to give a *different* answer:
//!
//! 1. **`(id, meta)` → 1.13-era name/properties** is re-read at test time from
//!    the committed text dump of the **real 1.13.2 server jar's own
//!    `DataFixerUpper`** (`lodestone-canonical/tests/support/…jvm.txt`, jar
//!    SHA-256 in its header). This test deliberately reaches across crates for
//!    the *dump text* rather than using `lodestone-canonical`'s generated Rust
//!    table: the generated table is downstream of the thing being trusted, so
//!    reading it here would compare our code to itself.
//! 2. **name/properties → a canonical 26.2 state id** is resolved by searching
//!    `lodestone_data::block_states`, the census of the real 26.2 jar. The
//!    search is required to land on **exactly one** state, so the expectation
//!    is a predicted *value*, not a direction or a range.
//! 3. **The block population itself**, for [`real_1_8_9_world_save_column…`],
//!    is a section lifted out of the vanilla 1.8.9 server's **own world save**
//!    (`tests/support/real_1_8_9_section_save.txt`, region-file SHA-256 in its
//!    header). Anvil 1.8 stores exactly the `(id, meta)` pair the wire sends,
//!    in the same YZX order, so real server output can be replayed through the
//!    real decoder with no server running.
//!
//! # The negative control, asserted rather than described
//!
//! Every asserted pair additionally requires that the **naive** value — the raw
//! composite, i.e. precisely what this crate used to store — names a
//! *different* 26.2 block. Without that, a gate could pass on a coincidence.
//! `minecraft:air` is the one pair where naive and canonical genuinely agree
//! (both are state 0), so it is asserted to decode correctly but excluded from
//! the inequality, explicitly rather than silently.
//!
//! # What would make this vacuous
//!
//! Feeding it only the real save. That world is `level-type=FLAT` and contains
//! **four** distinct pairs — `0:0`, `2:0`, `3:0`, `7:0` — every one of them
//! `meta = 0`, so on its own it cannot exercise the `meta` half of the
//! composite at all: the *world* species of vacuous test, which cannot be seen
//! by reading the test. Hence the second arm below, whose pairs are chosen for
//! `meta != 0` (granite `1:1`, polished granite `1:2`, spruce log `17:1`, red
//! wool `35:14`) and which is the arm that actually proves meta survives.

use lodestone_core::{Reader, Writer};
use lodestone_data::block_states;
use lodestone_v1_8::packets::chunk::{ChunkShape, MapChunk};

/// The committed JVM dump of the 1.13.2 jar's own flattening table — anchor 1.
///
/// Reached across crates on purpose; see this module's docs. If this path ever
/// breaks, the fix is to follow the moved dump, **not** to substitute
/// `lodestone_canonical`'s generated table.
const DUMP: &str = include_str!("../../../lodestone-canonical/tests/support/flattening_1_13_2_jvm.txt");

/// The committed real-1.8.9-server section — anchor 3.
const REAL_SECTION: &str = include_str!("support/real_1_8_9_section_save.txt");

const BLOCK_ENTRIES: usize = 4096;

/// Section-local flat index in 1.8's YZX order (`y << 8 | z << 4 | x`).
fn idx(x: usize, y: usize, z: usize) -> usize {
    y << 8 | z << 4 | x
}

/// The wire composite for a legacy pair: `(id << 4) | meta`.
fn composite(id: u32, meta: u32) -> u32 {
    (id << 4) | meta
}

/// One row of the JVM dump: the 1.13-era name and properties vanilla's own
/// `DataFixerUpper` assigns to `(id << 4) | meta`, or `None` for a slot the
/// table leaves unassigned.
fn dump_slot(id: u32, meta: u32) -> Option<(&'static str, Vec<(&'static str, &'static str)>)> {
    let want = composite(id, meta).to_string();
    for line in DUMP.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        if fields.next() != Some(want.as_str()) {
            continue;
        }
        let name = fields.next()?;
        let props = fields.next().map_or_else(Vec::new, |raw| {
            raw.split(',')
                .filter(|kv| !kv.is_empty())
                .map(|kv| {
                    let (k, v) = kv.split_once('=').expect("dump property is k=v");
                    (k, v)
                })
                .collect()
        });
        return Some((name, props));
    }
    None
}

/// Predicts the canonical 26.2 state id for a dump row, using only the 26.2
/// registry census — anchor 2.
///
/// Requires the search to land on exactly one state. That is what makes this a
/// value prediction rather than a range check: if 26.2 grew a property that
/// makes the row ambiguous, this panics naming the candidates instead of
/// quietly accepting whichever the decoder happened to pick.
fn predict_state(name: &str, props: &[(&str, &str)]) -> u32 {
    let candidates: Vec<u32> = (0..block_states::STATE_COUNT)
        .filter(|&id| block_states::block_name(id) == Some(name))
        .filter(|&id| {
            let have = block_states::properties(id).unwrap_or(&[]);
            props
                .iter()
                .all(|(k, v)| have.iter().any(|(hk, hv)| hk == k && hv == v))
        })
        .collect();
    assert_eq!(
        candidates.len(),
        1,
        "26.2 registry must contain exactly one state for {name} with {props:?}, found {candidates:?}",
    );
    candidates[0]
}

/// Describes a 26.2 state id the way a human reads a failure message.
fn describe(id: u32) -> String {
    match block_states::block_name(id) {
        Some(name) => format!("{id} ({name})"),
        None => format!("{id} (not a 26.2 state at all)"),
    }
}

/// Builds one section's 8192 wire bytes (4096 little-endian composites) from a
/// per-cell value function.
fn section_bytes(mut composite_at: impl FnMut(usize, usize, usize) -> u32) -> Vec<u8> {
    let mut out = vec![0u8; BLOCK_ENTRIES * 2];
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                let v = composite_at(x, y, z) as u16;
                let i = idx(x, y, z);
                out[2 * i] = (v & 0xFF) as u8;
                out[2 * i + 1] = (v >> 8) as u8;
            }
        }
    }
    out
}

/// Wraps one section's block bytes into a full ground-up `map_chunk` body.
fn map_chunk_body(blocks: &[u8]) -> Vec<u8> {
    let mut blob = Vec::new();
    blob.extend_from_slice(blocks);
    blob.extend_from_slice(&[0u8; 2048]); // block light
    blob.extend_from_slice(&[0xFFu8; 2048]); // sky light
    blob.extend_from_slice(&[1u8; 256]); // biome footer

    let mut w = Writer::default();
    w.i32(0);
    w.i32(0);
    w.bool(true); // groundUp
    w.u16(0x0001); // section 0 present
    w.var_i32(blob.len() as i32);
    w.bytes(&blob);
    w.into_vec()
}

/// Parses the committed real-save fixture into 4096 `(id, meta)` pairs.
fn real_section_cells() -> Vec<(u32, u32)> {
    let cells: Vec<(u32, u32)> = REAL_SECTION
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .flat_map(str::split_whitespace)
        .map(|tok| {
            let (id, meta) = tok.split_once(':').expect("fixture token is id:meta");
            (
                id.parse().expect("fixture id is a number"),
                meta.parse().expect("fixture meta is a number"),
            )
        })
        .collect();
    assert_eq!(
        cells.len(),
        BLOCK_ENTRIES,
        "the fixture must describe a whole 16^3 section",
    );
    cells
}

/// The adversarial pairs. Chosen so that **five of the nine carry a non-zero
/// meta**, which the real flat world (all `meta = 0`) structurally cannot
/// exercise, and so that each has a stable 1.13→26.2 name — the rename bridge
/// is `lodestone-canonical`'s business and is gated there, not here.
const ADVERSARIAL: &[(u32, u32)] = &[
    (1, 0),   // stone
    (1, 1),   // granite          -- meta
    (1, 2),   // polished_granite -- meta
    (1, 4),   // polished_diorite -- meta
    (2, 0),   // grass_block, snowy=false
    (3, 0),   // dirt
    (7, 0),   // bedrock
    (17, 1),  // spruce_log, axis=y -- meta
    (35, 14), // red_wool           -- meta
];

#[test]
fn adversarial_pairs_decode_to_canonical_states_and_the_naive_value_does_not() {
    // One legacy pair per Y layer, filling the whole 16x16 layer so a
    // transposed decode fails here too.
    let blocks = section_bytes(|_, y, _| {
        ADVERSARIAL
            .get(y)
            .map_or(0, |&(id, meta)| composite(id, meta))
    });
    let body = map_chunk_body(&blocks);

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld()).expect("decode");
    r.ensure_empty().expect("zero trailing bytes");

    assert_eq!(
        chunk.fallback,
        Default::default(),
        "every adversarial pair is a resolvable slot; a fallback here means the \
         table lost an entry, not that the decode is wrong",
    );

    let mut naive_agreements = 0;
    for (y, &(id, meta)) in ADVERSARIAL.iter().enumerate() {
        let (name, props) = dump_slot(id, meta)
            .unwrap_or_else(|| panic!("the 1.13.2 jar dump assigns {id}:{meta} a name"));
        let predicted = predict_state(name, &props);

        let decoded = chunk.column.get_block(0, y as i32, 0);
        assert_eq!(
            decoded,
            predicted,
            "1.8 {id}:{meta} must decode to the 26.2 state the 1.13.2 jar dump \
             names ({name}, {props:?}); got {} instead of {}",
            describe(decoded),
            describe(predicted),
        );

        // The negative control: what this crate used to store.
        let naive = composite(id, meta);
        if block_states::block_name(naive) == block_states::block_name(decoded) {
            naive_agreements += 1;
        }
        assert_ne!(
            naive,
            decoded,
            "the naive composite for {id}:{meta} must name a DIFFERENT 26.2 block, \
             or this pair proves nothing: naive {} vs canonical {}",
            describe(naive),
            describe(decoded),
        );
    }

    assert_eq!(
        naive_agreements, 0,
        "no adversarial pair may coincidentally share a block name with its raw \
         composite, or the control is weaker than it looks",
    );

    // Layers past the pair list are composite 0 -> air, and prove the section
    // is not simply uniform.
    let air = predict_state("minecraft:air", &[]);
    assert_eq!(chunk.column.get_block(0, 15, 0), air);
}

#[test]
fn real_1_8_9_world_save_column_decodes_to_canonical_states() {
    let cells = real_section_cells();
    let blocks = section_bytes(|x, y, z| {
        let (id, meta) = cells[idx(x, y, z)];
        composite(id, meta)
    });
    let body = map_chunk_body(&blocks);

    let mut r = Reader::new(&body);
    let chunk = MapChunk::decode(&mut r, &ChunkShape::overworld()).expect("decode");
    r.ensure_empty().expect("zero trailing bytes");

    assert_eq!(
        chunk.fallback,
        Default::default(),
        "a real vanilla 1.8.9 world must canonicalise with no air substitutions",
    );

    // Every cell of real server output, checked against the jar dump.
    for y in 0..16usize {
        for z in 0..16usize {
            for x in 0..16usize {
                let (id, meta) = cells[idx(x, y, z)];
                let (name, props) = dump_slot(id, meta)
                    .unwrap_or_else(|| panic!("the jar dump assigns {id}:{meta} a name"));
                let predicted = predict_state(name, &props);
                let decoded = chunk.column.get_block(x, y as i32, z);
                assert_eq!(
                    decoded,
                    predicted,
                    "real save cell ({x},{y},{z}) = {id}:{meta} ({name}) decoded as {}",
                    describe(decoded),
                );
            }
        }
    }

    // The FLAT preset's known layering, asserted by NAME rather than by id, so
    // it reads as a claim about the world the server generated. This is
    // knowledge from the server's own `level-type=FLAT` configuration, not from
    // anything this repo computes.
    let name_at = |y: i32| block_states::block_name(chunk.column.get_block(0, y, 0));
    assert_eq!(name_at(0), Some("minecraft:bedrock"));
    assert_eq!(name_at(1), Some("minecraft:dirt"));
    assert_eq!(name_at(2), Some("minecraft:dirt"));
    assert_eq!(name_at(3), Some("minecraft:grass_block"));
    assert_eq!(name_at(4), Some("minecraft:air"));

    // The negative control on real data: the raw composite for bedrock is not
    // bedrock in 26.2. If this ever becomes an equality, the real-save arm has
    // stopped proving anything and only the adversarial arm is load-bearing.
    let bedrock = predict_state("minecraft:bedrock", &[]);
    assert_ne!(
        composite(7, 0),
        bedrock,
        "1.8 bedrock's composite {} must not be 26.2 bedrock {}",
        describe(composite(7, 0)),
        describe(bedrock),
    );
}

#[test]
fn the_real_save_fixture_cannot_exercise_meta_which_is_why_the_other_arm_exists() {
    // A guard on this file's own premise, so the reason the adversarial arm
    // exists is checkable rather than a comment. If the fixture is ever
    // regenerated from a richer world this will fail, and the right response is
    // to delete this test, not to weaken the adversarial arm.
    let cells = real_section_cells();
    assert!(
        cells.iter().all(|&(_, meta)| meta == 0),
        "the committed flat-world fixture is all meta=0",
    );
    assert!(
        ADVERSARIAL.iter().filter(|&&(_, meta)| meta != 0).count() >= 5,
        "the adversarial arm is what covers meta",
    );
}
