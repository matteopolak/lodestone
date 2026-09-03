//! Resolves a flat block-state id from protocol 404 to the canonical **26.2**
//! block-state id (`lodestone_data::block_states`) that `lodestone-world`'s
//! `PalettedContainer` consumers (the mesher's atlas, collision) are actually
//! built from.
//!
//! # Why this exists
//!
//! 1.13 is where a chunk-section palette entry stopped being the
//! `(blockId << 4) | meta` composite the pre-1.13 families bridge through
//! `lodestone_canonical::canonical` and became a single flat state id. That
//! removes the ambiguity, not the translation: **1.13.2's flat id space is
//! its own**. Vanilla inserts new blocks into the global palette as they are
//! added, so a given block's numeric id drifts release to release, and
//! storing a wire id straight into `lodestone-world` renders as whichever
//! unrelated 26.2 block happens to share that number today — no error, no
//! panic, just the wrong terrain.
//!
//! The drift here is the largest of any era boundary this repo covers,
//! because 1.13's own renumbering *is* the flattening: 1.13.2's palette holds
//! 8,599 states against 26.2's 32k, and even a block that has existed
//! unchanged since 1.8 sits at a different number in each. See
//! `tests/canonicalisation.rs`'s
//! `discriminating_states_resolve_to_their_26_2_ids_not_their_wire_ids` for
//! measured examples.
//!
//! # How it works
//!
//! There is nothing left to compute at runtime: a flat state id has no cases
//! needing TileEntity/neighbour context (that was an `id:meta` ambiguity
//! problem), so the whole `wire id -> 26.2 id` mapping is baked into a flat
//! generated array at regeneration time and [`CanonicalTable::resolve`] is a
//! plain array index. See `tests/canonicalisation.rs` for the generator, the
//! rename/property bridging it applies, and its data provenance (Mojang's own
//! data generator run against the real `.cache/mc/1.13.2/server.jar`, plus a
//! vanilla world upgrade for the properties that changed shape rather than
//! name).
//!
//! # What a caller does with an out-of-range id
//!
//! A state id `>= state_count()` names no block in this protocol at all — no
//! real vanilla 1.13.2 server sends one — so
//! [`CanonicalTable::resolve_or_air`] substitutes the table's own air id and
//! counts it in a [`FallbackTally`], the same visible, counted-not-silent
//! policy `lodestone_canonical::canonical::resolve_or_air` uses for its own
//! out-of-bounds case. The **caller** (`packets/chunk.rs`) decides to
//! substitute air and logs the tally; this module only resolves and counts.
//!
//! # Why this is still a `table_for(protocol)` and not a bare constant
//!
//! This era has one member, so the resolution is trivial today. It is kept as
//! a lookup that *panics* for anything outside [`crate::PROTOCOLS`] because
//! the failure it guards against is the silent one: answering with some other
//! protocol's numbering produces a populated, plausible, wrong world.

use crate::generated_canonical;

/// One protocol's `wire state id -> canonical 26.2 state id` mapping.
///
/// Held by value in a `static` and handed out by [`table_for`];
/// `packets/chunk.rs` carries a `&'static CanonicalTable` inside its
/// `ChunkShape` so a decode can never reach for a table the adapter was not
/// constructed for.
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

/// Minecraft 1.13.2's table.
static TABLE_404: CanonicalTable = CanonicalTable {
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
        crate::adapter::PROTOCOL_1_13_2 => &TABLE_404,
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
    /// state_count()` — outside the range any real 1.13.2 server sends.
    pub out_of_range: u32,
}

impl FallbackTally {
    /// Whether any block in this tally needed a fallback substitution.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.out_of_range == 0
    }
}
