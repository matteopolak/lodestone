//! Resolves a 1.16.5 (protocol 754) flat block-state id to the canonical
//! **26.2** block-state id (`lodestone_data::block_states`) that
//! `lodestone-world`'s `PalettedContainer` consumers (the mesher's atlas,
//! collision) are actually built from.
//!
//! # Why this exists
//!
//! 1.16.5 is post-Flattening: a chunk-section palette entry is already a
//! single flat state id, not the pre-1.13 `(blockId << 4) | meta` composite
//! `v1-8`/`v1-9` bridge through `lodestone_canonical::canonical`. But 1.16.5's
//! flat id space is **its own** — the game inserts new blocks into the
//! global palette as they are added, so a given block's numeric id drifts
//! release to release. Before this module existed, `packets/chunk.rs`
//! stored a 1.16.5 wire id straight into `lodestone-world` storage
//! unmapped, which silently rendered as whichever unrelated 26.2 block
//! happens to share that number today — no error, no panic, just the wrong
//! terrain. See `tests/canonicalisation.rs`'s
//! `discriminating_states_resolve_to_their_26_2_ids_not_their_wire_ids` for
//! measured examples (e.g. wire id 3355, `minecraft:diamond_block` in
//! 1.16.5, decoded unmapped as `minecraft:warped_shelf`).
//!
//! # How it works
//!
//! Unlike the pre-Flattening bridge, there is nothing left to compute at
//! runtime: 1.16.5 has no cases needing TileEntity/neighbour context (that
//! was an `id:meta` ambiguity problem), so the whole `wire id -> 26.2 id`
//! mapping is baked into a flat generated array,
//! [`crate::generated_canonical::STATE_TO_CANONICAL`], at regeneration time.
//! [`resolve`] is therefore a plain array index — see
//! `tests/canonicalisation.rs` for the generator, the rename/property
//! bridging it applies, and its data provenance (Mojang's own data
//! generator run against the real `.cache/mc/1.16.5/server.jar`, cross-
//! checked to zero unmapped states against the full 17,112-state corpus).
//!
//! # What a caller does with an out-of-range id
//!
//! A state id `>= SOURCE_STATE_COUNT` names no 1.16.5 block at all — no real
//! vanilla 1.16.5 server sends one — so [`resolve_or_air`] substitutes
//! [`air_state_id`] and counts it in a [`FallbackTally`], the same visible,
//! counted-not-silent policy `lodestone_canonical::canonical::resolve_or_air`
//! uses for its own out-of-bounds case. The **caller** (`packets/chunk.rs`)
//! decides to substitute air and logs the tally; this module only resolves
//! and counts.

use crate::generated_canonical;

/// Resolves a 1.16.5 flat wire state id to its canonical 26.2 state id, or
/// [`None`] if `state_id` is not a real 1.16.5 state
/// (`state_id >= SOURCE_STATE_COUNT`).
#[must_use]
pub fn resolve(state_id: u32) -> Option<u32> {
    generated_canonical::STATE_TO_CANONICAL
        .get(state_id as usize)
        .copied()
}

/// The canonical 26.2 `minecraft:air` state id. Baked into the generated
/// table (see its module docs for why that is no less current than the rest
/// of the table) rather than looked up at runtime, so this crate carries no
/// runtime dependency on `lodestone-data` — `cargo xtask check-deletable`
/// stays accurate.
#[must_use]
pub fn air_state_id() -> u32 {
    generated_canonical::AIR_STATE_ID
}

/// Count of wire state ids that could not be resolved because they named no
/// real 1.16.5 state at all — see the module docs. Mirrors
/// `lodestone_canonical::canonical::FallbackTally`'s shape (a per-column
/// tally the consuming family logs), reduced to the one failure mode this
/// crate's mapping can actually produce.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FallbackTally {
    /// Blocks substituted because the wire state id was `>=
    /// SOURCE_STATE_COUNT` — outside the range any real 1.16.5 server sends.
    pub out_of_range: u32,
}

impl FallbackTally {
    /// Whether any block in this tally needed a fallback substitution.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.out_of_range == 0
    }
}

/// Resolves `state_id` to a canonical 26.2 state id, substituting
/// [`air_state_id`] and recording into `tally` when out of range. The single
/// integration point `packets/chunk.rs` calls per decoded cell.
#[must_use]
pub fn resolve_or_air(state_id: u32, tally: &mut FallbackTally) -> u32 {
    match resolve(state_id) {
        Some(id) => id,
        None => {
            tally.out_of_range += 1;
            air_state_id()
        }
    }
}
