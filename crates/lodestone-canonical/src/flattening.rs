//! Pre-Flattening `id:meta` (protocol 340 / Minecraft 1.12.2) &rarr; modern
//! block-state lookup: the table multi-version support asked for as the
//! forcing function, built and verified against the real 1.13.2
//! server jar's own world-upgrade flattening step (the same conversion
//! vanilla itself runs to upgrade a pre-1.13 world) rather than written by
//! hand or trusted blindly from a community dataset.
//!
//! # Why this table exists and why it is shaped the way it is
//!
//! Below 1.13, a block on the wire and in storage is a numeric
//! `(old_block_id, meta)` pair (the exact global id
//! `(old_block_id << 4) | meta` this crate's own `packets/chunk.rs` already
//! extracts per paletted-chunk-section entry). From 1.13 on, a block is a
//! namespaced block state, and this project's entire data layer downstream of
//! `lodestone-world` is state-shaped. Every pre-Flattening version crate
//! needs a real translation from the former to the latter.
//!
//! The mapping is **not** a formula and **not** one-to-one, and CLAUDE.md is
//! explicit that a table which silently resolves the ambiguous cases is worse
//! than no table (it produces plausible wrong terrain nobody can trace). So
//! [`LegacyBlockState`] has **four** variants, not one — [`Self::Resolved`]
//! plus three distinct, explicit ways a lookup can fail to be a single clean
//! answer, each with a different root cause a caller may need to react to
//! differently:
//!
//! - [`LegacyBlockState::NoTableEntry`] — this exact pair was never assigned
//!   a target by vanilla's own flattening table (1663 of the 4095 valid
//!   `(id, meta)` combinations *are* assigned; 2400 are not — see
//!   `tests/flattening.rs`'s generator for the exact count derivation). Live
//!   vanilla's own accessor silently substitutes air for every one of these;
//!   this table does not, on purpose.
//! - [`LegacyBlockState::RequiresAdditionalContext`] — vanilla's own
//!   flattening fix could not resolve identity from `id:meta` alone. Two
//!   confirmed families: flower pots (old id 140 — the contained plant is a
//!   block-entity field, not a meta value; all 16 metas collapse to the same
//!   placeholder `potted_cactus` in vanilla's own table) and skulls (old id
//!   144 — type/rotation are block-entity fields; vanilla's own table literally
//!   leaves its internal placeholder string `%%FILTER_ME%%` in the Name
//!   field for every skull meta). A third family — the upper half of double
//!   plants (old id 175, metas 8-11; species is read from the paired lower
//!   half at conversion time, not stored in the upper half's own meta) — is
//!   *not* separately flagged here: vanilla's table returns a plausible-looking
//!   but wrong single species (`peony`) for all four upper-half metas rather
//!   than a sentinel, so there is no mechanical signal to detect it by; it is
//!   recorded in `docs/protocol-340-flattening-table.md`'s ambiguous-case
//!   enumeration instead, and any caller resolving id 175 metas 8-11 must
//!   consult the paired lower-half block, not this table.
//! - [`LegacyBlockState::OutOfBounds`] — `old_block_id == 255 && meta == 15`
//!   lands one past the end of vanilla's own 4095-entry lookup array (a
//!   genuine off-by-one in vanilla itself: `256 * 16 == 4096`, the array is
//!   4095 long). Distinguished from `NoTableEntry` because the reason is
//!   different and worth surfacing separately if anyone goes looking.
//!
//! # What this table is *not* authoritative for
//!
//! This is vanilla's own **first** flattening step (an early schema step in
//! its world-upgrade pipeline), not the final word on 1.13.2 spelling. A handful of resolved
//! names/property keys are the **intermediate** snapshot-era spelling that
//! this one schema step produces, later renamed by separate, unrelated fixes
//! chained further down the same pipeline — confirmed, not guessed: the
//! literal strings `"persistent"` and `"distance"` (1.13.2's actual final
//! leaf-block property names) do not appear anywhere in the 1.13.2 server
//! jar's bytes, while `"decayable"`/`"check_decay"` (what this table
//! resolves leaves to) do. Similarly `mob_spawner`/`melon_block`/`portal`/
//! `oak_bark` (etc.) are this table's output where final 1.13.2 uses
//! `spawner`/`melon`/`nether_portal`/`oak_wood`. See
//! `docs/protocol-340-flattening-table.md` for the full list and the
//! `minecraft-data` cross-check that surfaced it. **A block's resolved
//! `name` here should not be assumed to already match `lodestone-v26-2`'s
//! (26.2) naming without going through the same rename layer vanilla does —
//! wiring this table to the canonical censuses is exactly the follow-up work
//! this task does not attempt (see the doc's wiring section).**

use crate::generated_flattening as table;

/// Number of `(old_block_id, meta)` slots vanilla's own array covers
/// (`old_block_id in 0..=255`, `meta in 0..16`, minus the one structurally
/// out-of-bounds slot — see [`LegacyBlockState::OutOfBounds`]).
pub const SLOT_COUNT: usize = table::SLOT_COUNT;

/// A single resolved modern block state: a namespaced block name plus its
/// state properties (empty for a block with no meta-derived state), exactly
/// as vanilla's own flattening fix produced it — see the module docs for why
/// that is not automatically 1.13.2's *final* spelling for every block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedState {
    /// Namespaced block name, e.g. `"minecraft:oak_stairs"`.
    pub name: &'static str,
    /// `(key, value)` state properties, sorted by key. Empty for blocks with
    /// no properties (most single-state blocks).
    pub properties: &'static [(&'static str, &'static str)],
}

/// The result of resolving one legacy `(old_block_id, meta)` pair. See the
/// module docs for why this has four variants instead of one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyBlockState {
    /// Vanilla's own flattening fix resolved this pair to exactly one modern
    /// block state.
    Resolved(ResolvedState),
    /// This `(old_block_id, meta)` pair has no entry in vanilla's own table.
    /// Live vanilla itself silently treats this as air; this table refuses
    /// to make that substitution for you.
    NoTableEntry,
    /// Vanilla's own flattening fix could not determine identity from
    /// `id:meta` alone (block-entity-dependent — flower pot contents or skull
    /// type/rotation). See the module docs for the two families this crate
    /// currently detects.
    RequiresAdditionalContext,
    /// `old_block_id == 255 && meta == 15`, one past the end of vanilla's own
    /// lookup array.
    OutOfBounds,
}

/// Resolves a legacy `(old_block_id, meta)` pair (pre-Flattening protocol
/// 340 wire/storage format: `(old_block_id << 4) | meta`) against vanilla's
/// own 1.13.2 flattening table.
///
/// `meta` values outside `0..16` are truncated to their low 4 bits before
/// lookup (the wire format never carries more than 4 meta bits; callers
/// passing a value already masked get an identical result).
#[must_use]
pub fn lookup(old_block_id: u8, meta: u8) -> LegacyBlockState {
    let meta = meta & 0x0F;
    if old_block_id == 255 && meta == 15 {
        return LegacyBlockState::OutOfBounds;
    }
    let index = (old_block_id as usize) * 16 + (meta as usize);
    match table::SLOTS[index] {
        table::Slot::NoTableEntry => LegacyBlockState::NoTableEntry,
        table::Slot::RequiresContext => LegacyBlockState::RequiresAdditionalContext,
        table::Slot::Resolved(entry_index) => {
            let entry = &table::RESOLVED[entry_index as usize];
            let start = entry.properties_start as usize;
            let len = entry.properties_len as usize;
            LegacyBlockState::Resolved(ResolvedState {
                name: entry.name,
                properties: &table::PROPERTIES[start..start + len],
            })
        }
    }
}
