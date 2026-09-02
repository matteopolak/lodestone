//! Resolves a flat block-state id from any protocol in this era (498, 578,
//! 754) to the canonical **26.2** block-state id
//! (`lodestone_data::block_states`) that `lodestone-world`'s
//! `PalettedContainer` consumers (the mesher's atlas, collision) are actually
//! built from.
//!
//! # Why this exists
//!
//! Every protocol here is post-Flattening: a chunk-section palette entry is
//! already a single flat state id, not the pre-1.13 `(blockId << 4) | meta`
//! composite `v1-8`/`v1-9` bridge through `lodestone_canonical::canonical`.
//! But each flat id space is **its own** — the game inserts new blocks into
//! the global palette as they are added, so a given block's numeric id drifts
//! release to release. Before this module existed, `packets/chunk.rs`
//! stored a wire id straight into `lodestone-world` storage unmapped, which
//! silently rendered as whichever unrelated 26.2 block happens to share that
//! number today — no error, no panic, just the wrong terrain. See
//! `tests/canonicalisation.rs`'s
//! `discriminating_states_resolve_to_their_26_2_ids_not_their_wire_ids` for
//! measured examples (e.g. wire id 3355, `minecraft:diamond_block` in
//! 1.16.5, decoded unmapped as `minecraft:warped_shelf`).
//!
//! # Why three tables and not one
//!
//! The drift is between the era's own members too, not just between the era
//! and 26.2: the three jar dumps carry 11,271 / 11,337 / 17,112 states, the
//! 498 and 754 tables first disagree at state 72, and wire state 11214 is a
//! lantern at 498, a bell at 578 and a prismarine wall at 754 (and a trapped
//! chest if left unmapped — four different blocks for one number). Sharing one table across the era would be the same
//! wrong-terrain defect with a smaller radius, so [`table_for`] resolves the
//! negotiated protocol to its own table exactly once, at adapter
//! construction.
//!
//! # How it works
//!
//! Unlike the pre-Flattening bridge, there is nothing left to compute at
//! runtime: a flat state id has no cases needing TileEntity/neighbour context
//! (that was an `id:meta` ambiguity problem), so each whole `wire id -> 26.2
//! id` mapping is baked into a flat generated array at regeneration time.
//! [`CanonicalTable::resolve`] is therefore a plain array index — see
//! `tests/canonicalisation.rs` for the generator, the rename/property
//! bridging it applies, and its data provenance (Mojang's own data generator
//! run against each real `.cache/mc/<version>/server.jar`, plus a vanilla
//! world upgrade for the two properties that changed shape rather than name,
//! cross-checked to zero unmapped states against all three corpora).
//!
//! # What a caller does with an out-of-range id
//!
//! A state id `>= state_count()` names no block in that protocol at all — no
//! real vanilla server of that version sends one — so
//! [`CanonicalTable::resolve_or_air`] substitutes the table's own air id and
//! counts it in a [`FallbackTally`], the same visible, counted-not-silent
//! policy `lodestone_canonical::canonical::resolve_or_air` uses for its own
//! out-of-bounds case. The **caller** (`packets/chunk.rs`) decides to
//! substitute air and logs the tally; this module only resolves and counts.

use crate::{generated_canonical, generated_canonical_498, generated_canonical_578};

/// One protocol's `wire state id -> canonical 26.2 state id` mapping.
///
/// Held by value in a `static` per protocol and handed out by [`table_for`];
/// `packets/chunk.rs` carries a `&'static CanonicalTable` inside its
/// `ChunkShape` so a decode can never reach for a different protocol's table
/// than the adapter was constructed for.
#[derive(Debug)]
pub struct CanonicalTable {
    /// `states[wire_id]` is the canonical 26.2 state id.
    states: &'static [u32],
    /// The canonical 26.2 `minecraft:air` state id, baked into the same
    /// generated file as `states`.
    air: u32,
}

impl CanonicalTable {
    /// Number of block states in this protocol's own global palette; valid
    /// wire ids are `0..state_count()`.
    #[must_use]
    pub const fn state_count(&self) -> u32 {
        self.states.len() as u32
    }

    /// The canonical 26.2 `minecraft:air` state id. Baked into the generated
    /// table (see its module docs for why that is no less current than the
    /// rest of the table) rather than looked up at runtime, so this crate
    /// carries no runtime dependency on `lodestone-data` — `cargo xtask
    /// check-deletable` stays accurate.
    #[must_use]
    pub const fn air_state_id(&self) -> u32 {
        self.air
    }

    /// Resolves a flat wire state id to its canonical 26.2 state id, or
    /// [`None`] if `state_id` is not a real state in this protocol
    /// (`state_id >= state_count()`).
    #[must_use]
    pub fn resolve(&self, state_id: u32) -> Option<u32> {
        self.states.get(state_id as usize).copied()
    }

    /// Resolves `state_id` to a canonical 26.2 state id, substituting
    /// [`Self::air_state_id`] and recording into `tally` when out of range.
    /// The single integration point `packets/chunk.rs` calls per decoded
    /// cell.
    #[must_use]
    pub fn resolve_or_air(&self, state_id: u32, tally: &mut FallbackTally) -> u32 {
        match self.resolve(state_id) {
            Some(id) => id,
            None => {
                tally.out_of_range += 1;
                self.air
            }
        }
    }
}

/// Minecraft 1.14.4's table.
static TABLE_498: CanonicalTable = CanonicalTable {
    states: &generated_canonical_498::STATE_TO_CANONICAL,
    air: generated_canonical_498::AIR_STATE_ID,
};
/// Minecraft 1.15.2's table.
static TABLE_578: CanonicalTable = CanonicalTable {
    states: &generated_canonical_578::STATE_TO_CANONICAL,
    air: generated_canonical_578::AIR_STATE_ID,
};
/// Minecraft 1.16.5's table.
static TABLE_754: CanonicalTable = CanonicalTable {
    states: &generated_canonical::STATE_TO_CANONICAL,
    air: generated_canonical::AIR_STATE_ID,
};

/// Resolves a negotiated protocol to its block-state table.
///
/// # Panics
///
/// Panics for a protocol outside [`crate::PROTOCOLS`], for the same reason
/// `crate::adapter`'s `ids_for` does: answering with some other protocol's
/// table is precisely the silent wrong-terrain failure this module exists to
/// prevent, so it must be impossible rather than merely unlikely.
#[must_use]
pub fn table_for(protocol: i32) -> &'static CanonicalTable {
    match protocol {
        crate::adapter::PROTOCOL_1_14_4 => &TABLE_498,
        crate::adapter::PROTOCOL_1_15_2 => &TABLE_578,
        crate::adapter::PROTOCOL_1_16_5 => &TABLE_754,
        other => panic!(
            "protocol {other} is outside this family's PROTOCOLS ({:?}); callers must test \
             membership before resolving a block-state table",
            crate::PROTOCOLS
        ),
    }
}

/// Count of wire state ids that could not be resolved because they named no
/// real state in the source protocol at all — see the module docs. Mirrors
/// `lodestone_canonical::canonical::FallbackTally`'s shape (a per-column
/// tally the consuming family logs), reduced to the one failure mode this
/// crate's mapping can actually produce.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FallbackTally {
    /// Blocks substituted because the wire state id was `>=
    /// state_count()` — outside the range any real server of that version
    /// sends.
    pub out_of_range: u32,
}

impl FallbackTally {
    /// Whether any block in this tally needed a fallback substitution.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.out_of_range == 0
    }
}
