//! The chunk-encode boundary's byte-identity gate: the string path
//! [`super::build_world_column`] used to be, kept as a **control** and asserted
//! to produce byte-identical packet payloads.
//!
//! The instructions-retired half of the same unit is
//! `tests/chunk_encode_cycles.rs`, an integration test rather than lines here,
//! because it needs `proc_pid_rusage` and this crate is
//! `#![forbid(unsafe_code)]` — which `#[allow]` cannot override.
//!
//! # Why a verbatim copy of the old code lives here
//!
//! `DESIGN.md` §12.131. Removing the string work from `build_world_column` is a
//! *representational* change: the bytes we send must be identical before and
//! after. The only assertion that can say so is one that runs both
//! implementations over the same real columns and diffs the encoded payloads —
//! so the pre-change body is reproduced here verbatim, including the 32,366-row
//! `resolve_state_id` scan it called.
//!
//! **This is a control, not a second implementation with callers.** `CLAUDE.md`
//! warns that two test helpers had hand-duplicated an *older* `resolve_state_id`
//! fallback and became silent callers when the real one changed — one failing as
//! a 30-second live timeout rather than a mismatch. The difference here is that
//! the duplicate is *asserted equal* to the real path, by
//! [`build_world_column_is_byte_identical_to_the_string_path`]: if
//! `lodestone_data::block_states::state_id`'s semantics ever change, this file
//! fails loudly and immediately instead of quietly disagreeing. Do not call
//! anything in this module from production code, and do not "fix" the control to
//! match a new expectation without deciding, deliberately, that the wire bytes
//! were meant to move.

use std::collections::HashMap;

use lodestone_server::{ChunkSource, overworld_chunk_source};

use super::*;

// ---------------------------------------------------------------------------
// The control: `build_world_column` and `resolve_state_id` as they were
// ---------------------------------------------------------------------------

/// `super::resolve_state_id` as it was before the resolver moved into
/// `lodestone-data`: a linear scan over **all** 32,366 block states with a
/// property-vector compare per row whose `block_name` matches, then the two
/// fallback tiers. `O(STATE_COUNT)` per call, and this is the function
/// [`build_world_column_legacy`] memoized per distinct state string.
///
/// See this module's docs before touching it. Verbatim, on purpose.
fn resolve_state_id_legacy(state: &str) -> u32 {
    let (name, raw_props) = match state.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest)),
        None => (state, ""),
    };
    let mut wanted: Vec<(&str, &str)> = if raw_props.is_empty() {
        Vec::new()
    } else {
        raw_props
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .collect()
    };
    wanted.sort_unstable();

    let mut first_id: Option<u32> = None;
    let mut last_id: Option<u32> = None;
    let mut default_id: Option<u32> = None;
    for id in 0..lodestone_data::block_states::STATE_COUNT {
        if block_name(id) != Some(name) {
            continue;
        }
        first_id.get_or_insert(id);
        last_id = Some(id);
        if lodestone_data::block_states::StateId::new(id)
            .expect("generated state-table index is valid")
            .is_default()
        {
            default_id = Some(id);
        }
        let have_raw = properties(id).unwrap_or(&[]);
        let mut have: Vec<(&str, &str)> = have_raw.to_vec();
        have.sort_unstable();
        if have == wanted {
            return id;
        }
    }

    let Some(base) = default_id.or(first_id) else {
        return air_id();
    };
    if wanted.is_empty() {
        return base;
    }

    let mut merged: Vec<(&str, &str)> = properties(base).unwrap_or(&[]).to_vec();
    let mut overridden = false;
    for &(key, value) in &wanted {
        if let Some(slot) = merged.iter_mut().find(|(have_key, _)| *have_key == key)
            && slot.1 != value
        {
            slot.1 = value;
            overridden = true;
        }
    }
    if !overridden {
        return base;
    }
    merged.sort_unstable();
    let (Some(start), Some(end)) = (first_id, last_id) else {
        return base;
    };
    for id in start..=end {
        let mut have: Vec<(&str, &str)> = properties(id).unwrap_or(&[]).to_vec();
        have.sort_unstable();
        if have == merged {
            return id;
        }
    }
    base
}

/// `super::build_world_column` as it was: 98,304 `&str` reads per column, each
/// probed through a per-column `HashMap<&str, u32>` (std's SipHash), each
/// distinct entry resolved by [`resolve_state_id_legacy`].
///
/// See this module's docs before touching it. Verbatim, on purpose — the biome
/// half is unchanged from the current version because the change did not touch
/// it.
fn build_world_column_legacy(shape: &ChunkShape, source: &ServerChunkColumn) -> WorldChunkColumn {
    let mut column = WorldChunkColumn::new(
        shape.min_y,
        shape.section_count,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );

    let mut seen: HashMap<&str, u32> = HashMap::new();

    let biome_palette_ids: Vec<u32> = source
        .biome_cell_palette()
        .iter()
        .map(|name| super::resolve_biome_id(name))
        .collect();

    for section_index in 0..shape.section_count {
        let base_y = shape.min_y + (section_index * ChunkSection::EDGE) as i32;
        let mut section = ChunkSection::new(
            shape.block_kind,
            shape.biome_kind,
            shape.air_id,
            shape.biome_id,
        );
        for ly in 0..ChunkSection::EDGE {
            let wy = base_y + ly as i32;
            for lz in 0..ChunkSection::EDGE {
                for lx in 0..ChunkSection::EDGE {
                    let state = source.block_state(lx as i32, wy, lz as i32);
                    let id = *seen
                        .entry(state)
                        .or_insert_with(|| resolve_state_id_legacy(state));
                    if id != shape.air_id {
                        section.set_block(lx, ly, lz, id);
                    }
                }
            }
        }
        for qy in 0..4usize {
            let column_qy = section_index * 4 + qy;
            for qz in 0..4usize {
                for qx in 0..4usize {
                    let cell = source.biome_cell_index(qx, column_qy, qz) as usize;
                    section.set_biome(qx, qy, qz, biome_palette_ids[cell]);
                }
            }
        }
        if !section.is_empty(shape.biome_id) {
            column.set_section(section_index, Some(section));
        }
    }

    column
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Seed used by every gate here — the same one `tests/block_edit.rs` and
/// `encode_chunk_carries_real_block_states_including_a_fluid` pin, so the columns
/// are the ones other gates already describe.
const SEED: i64 = 1234;

/// `n` real generated columns from the production [`overworld_chunk_source`],
/// spiralling out from the origin so the set is not one column repeated (a
/// single column's palette is not representative of the *distinct*-entry count
/// the resolver cost scales with).
///
/// Generation is not measured — this runs before any instrument is read.
fn real_columns(n: usize) -> Vec<ServerChunkColumn> {
    let source = overworld_chunk_source(SEED);
    (0..n)
        .map(|i| {
            let cx = (i % 4) as i32;
            let cz = (i / 4) as i32;
            source.column(cx, cz)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Correctness: identical bytes, and ids that agree with the jar's own columns
// ---------------------------------------------------------------------------

/// **The acceptance criterion.** The integer path and the string path must encode
/// the same packet payload, byte for byte, for real generated columns — this is a
/// representational change, so anything else is a regression regardless of how
/// much faster it is.
///
/// Compares the *whole* `level_chunk_with_light` body (through the real
/// `encode_column_body`, including the light payload computed from each world
/// column independently), not just the section blob: a difference in a block id
/// that happened to cancel inside the palette would still move the light.
#[test]
fn build_world_column_is_byte_identical_to_the_string_path() {
    let shape = ChunkShape::overworld_1_21();
    let columns = real_columns(2);
    let mut differing_cells = 0usize;
    for (i, column) in columns.iter().enumerate() {
        let cx = (i % 4) as i32;
        let cz = (i / 4) as i32;

        let integer_path = build_world_column(&shape, column);
        let string_path = build_world_column_legacy(&shape, column);

        // Where, not just whether — per `CLAUDE.md`'s "measure by location".
        for y in shape.min_y..shape.min_y + shape.world_height as i32 {
            for lz in 0..16usize {
                for lx in 0..16usize {
                    let a = integer_path.get_block(lx, y, lz);
                    let b = string_path.get_block(lx, y, lz);
                    if a != b {
                        differing_cells += 1;
                        assert_eq!(
                            a, b,
                            "chunk ({cx}, {cz}) cell ({lx}, {y}, {lz}): integer path says {a}, \
                             string path says {b} (state string {:?})",
                            column.block_state(lx as i32, y, lz as i32)
                        );
                    }
                }
            }
        }

        let integer_payload = encode_column_body(
            cx,
            cz,
            &shape,
            &integer_path,
            &compute_served_light(&integer_path),
            column,
        );
        let string_payload = encode_column_body(
            cx,
            cz,
            &shape,
            &string_path,
            &compute_served_light(&string_path),
            column,
        );
        assert_eq!(
            integer_payload.len(),
            string_payload.len(),
            "chunk ({cx}, {cz}): payload length moved"
        );
        assert_eq!(
            integer_payload, string_payload,
            "chunk ({cx}, {cz}): payload bytes moved"
        );
    }
    assert_eq!(differing_cells, 0);

    // Non-vacuity: the fixture has to contain the structure the change is about
    // — several distinct palette entries, at least one of them propertied, and at
    // least one non-air id actually written. A pair of all-air columns would
    // satisfy every assertion above and prove nothing.
    let palettes: Vec<&[String]> = columns.iter().map(ServerChunkColumn::raw_palette).collect();
    assert!(
        palettes.iter().any(|p| p.len() >= 3),
        "fixture is degenerate: no column has three distinct palette entries ({palettes:?})"
    );
    assert!(
        palettes.iter().any(|p| p.iter().any(|s| s.contains('['))),
        "fixture is degenerate: no column carries a propertied state, which is the case the \
         three-tier resolver exists for ({palettes:?})"
    );
    assert!(
        columns.iter().any(|c| c.solid_count() > 1000),
        "fixture is degenerate: no column has real terrain in it"
    );
}

/// The ids themselves, against an **outside** source: `lodestone-data`'s
/// jar-derived columns, read directly rather than re-run through the resolver.
///
/// `build_world_column_is_byte_identical_to_the_string_path` above compares two
/// of our own implementations, which `CLAUDE.md` rightly calls the weaker half of
/// a `decode(encode(x)) == x` pair — two symmetric misunderstandings satisfy it.
/// This one never calls either resolver: for every palette entry of every real
/// column it takes the id the column resolved and checks it against
/// `block_name`/`properties`/`is_default_state`, which are dumps of
/// `vanilla's own block's own block state registry` and `defaultBlockState()` out of the real 26.2
/// server.
///
/// The three claims, one per resolver tier:
///
/// * the resolved id's **block name** is the requested one;
/// * every property the state string named — and that the block really has — is
///   present on the resolved id with the requested value (tiers 1 and 2);
/// * a **bare** name resolves to the id the jar marks `is_default_state`, which is
///   the claim whose older, wrong version ("lowest id sharing the name") shipped
///   snowy grass, wrong-facing directionals and climbing redstone dust.
#[test]
fn palette_state_ids_agree_with_the_jar_derived_dump() {
    let columns = real_columns(2);
    let mut checked_bare = 0usize;
    let mut checked_propertied = 0usize;
    for column in &columns {
        let palette = column.raw_palette();
        let ids = column.palette_state_ids();
        assert_eq!(palette.len(), ids.len());
        for (state, &id) in palette.iter().zip(ids) {
            let (name, raw_props) = match state.split_once('[') {
                Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest)),
                None => (state.as_str(), ""),
            };
            assert_eq!(
                block_name(id),
                Some(name),
                "{state} resolved to id {id}, whose block name is {:?}",
                block_name(id)
            );
            let have = properties(id).expect("resolved id is in range");
            if raw_props.is_empty() {
                assert!(
                    lodestone_data::block_states::StateId::new(id)
                        .expect("resolved id is in the generated state table")
                        .is_default(),
                    "bare {state} resolved to id {id}, which the jar does not mark as \
                     {name}'s default state (properties {have:?})"
                );
                checked_bare += 1;
            } else {
                for pair in raw_props.split(',') {
                    let (key, value) = pair.split_once('=').expect("well-formed property");
                    // A property no real state of this block carries is
                    // *synthetic* and deliberately dropped, so only assert on
                    // keys the jar says the block has.
                    if have.iter().any(|(k, _)| *k == key) {
                        assert!(
                            have.contains(&(key, value)),
                            "{state} resolved to id {id}, whose {key} is not {value} \
                             (properties {have:?})"
                        );
                    }
                }
                checked_propertied += 1;
            }
        }
    }
    // The control: an empty palette, or one with no propertied entry, would pass
    // every assertion above without exercising either tier.
    assert!(
        checked_bare > 0 && checked_propertied > 0,
        "vacuous: {checked_bare} bare and {checked_propertied} propertied palette entries checked"
    );
}
