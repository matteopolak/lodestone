//! Resolves a flat block-state id from this era's protocol (774) to the
//! canonical **26.2** block-state id (`lodestone_data::block_states`) that
//! `lodestone-world`'s `PalettedContainer` consumers (the mesher's atlas,
//! collision) are actually built from.
//!
//! # Why this exists
//!
//! 774 is post-Flattening: a chunk-section palette entry is already a single
//! flat state id. But each release's flat id space is **its own** — the game
//! inserts new blocks into the global palette as they are added, so a given
//! block's numeric id drifts release to release. Storing a wire id straight
//! into `lodestone-world` storage unmapped silently renders as whichever
//! unrelated 26.2 block happens to share that number: no error, no panic, just
//! the wrong terrain. This era's palette holds 29,671 states (counted from the
//! jar's own block report) against 26.2's 32,366, so essentially every id
//! above the first few thousand names a different block in the two.
//!
//! # How it works
//!
//! There is nothing left to compute at runtime: a flat state id has no cases
//! needing block-entity or neighbour context, so the whole
//! `wire id -> 26.2 id` mapping is baked into a flat generated array at
//! regeneration time. [`CanonicalTable::resolve`] is therefore a plain array
//! index — see `tests/canonicalisation.rs` for the generator and its data
//! provenance (the jar's own data generator run against the real
//! `.cache/mc/1.21.11/server.jar`).
//!
//! # What a caller does with an out-of-range id
//!
//! A state id `>= state_count()` names no block in this protocol at all — no
//! real vanilla server of this version sends one — so
//! [`CanonicalTable::resolve_or_air`] substitutes the table's own air id and
//! counts it in a [`FallbackTally`]. The **caller** (`packets/chunk.rs`)
//! decides to substitute air and logs the tally; this module only resolves and
//! counts.

use crate::generated_canonical;

/// One protocol's `wire state id -> canonical 26.2 state id` mapping.
///
/// Held by value in a `static` and handed out by [`table_for`];
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
    /// table rather than looked up at runtime, so this crate carries no
    /// runtime dependency on `lodestone-data` for the mapping —
    /// `cargo xtask check-deletable` stays accurate.
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
    /// The single integration point `packets/chunk.rs` calls per decoded cell.
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

/// The era's block-state table. One protocol, one table — see the module docs
/// for why it cannot be shared with any neighbouring era's.
static TABLE: CanonicalTable = CanonicalTable {
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
        crate::adapter::PROTOCOL_1_21_11 => &TABLE,
        other => panic!(
            "protocol {other} is outside this family's PROTOCOLS ({:?}); callers must test \
             membership before resolving a block-state table",
            crate::PROTOCOLS
        ),
    }
}

/// Count of wire state ids that could not be resolved because they named no
/// real state in the source protocol at all — see the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FallbackTally {
    /// Blocks substituted because the wire state id was `>= state_count()` —
    /// outside the range any real server of that version sends.
    pub out_of_range: u32,
}

impl FallbackTally {
    /// Whether any block in this tally needed a fallback substitution.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.out_of_range == 0
    }
}
