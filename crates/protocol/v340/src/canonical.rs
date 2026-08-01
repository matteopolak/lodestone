//! Bridges [`crate::flattening`]'s `id:meta` &rarr; modern-state table to a
//! canonical **26.2** block-state id (`lodestone_data::block_states`), the id
//! space `lodestone-world`'s [`lodestone_world::PalettedContainer`] needs
//! downstream: nothing in `lodestone-world` interprets its `u32` entries (see
//! its module docs — "opaque non-negative integer ids"), but every consumer
//! actually wired into a live world today (the mesher's `BlockAtlas`,
//! collision) is built from the 26.2 registry specifically, so a `v340` chunk
//! has to land in that same space to render or collide correctly rather than
//! silently mismatching a registry nothing warned it about.
//!
//! # Two bridging problems, not one
//!
//! [`crate::flattening::lookup`] already solved "which modern block is this
//! legacy `id:meta` pair" against the real 1.13.2 jar's own flattening step.
//! What it does *not* solve — documented as explicit follow-up work in
//! `docs/protocol-340-flattening-table.md` — is that vanilla's own table is
//! only its **first** flattening step: a few names/property keys are the
//! intermediate 18w-snapshot spelling, renamed by later, unrelated fixes
//! chained further down `DataFixerUpper`, and 26.2 itself has continued
//! renaming and adding properties (`waterlogged`) since 1.13.2. So this
//! module does two separate things:
//!
//! 1. [`bridge_name`] — a small, explicit rename table for names this pass
//!    confirmed do not exist in the 26.2 registry as vanilla's flattening
//!    step spelled them (`mob_spawner`&rarr;`spawner`, `*_bark`&rarr;`*_wood`,
//!    …). **Not sourced from tracing the 1.14+/26.2 jar's own rename fixers**
//!    (that is real, unstarted follow-up work) — each entry here was instead
//!    *verified* by confirming the target name exists in the 26.2 registry
//!    with the exact state shape (property count/kind) the legacy meta range
//!    implies. That is much stronger than a guess (a wrong target name would
//!    not have matched a real registry entry with a plausible shape at all),
//!    but it is not the same standard of evidence the original table used —
//!    see [`resolve`]'s module-level test for the exhaustive count this
//!    leaves unresolved (zero, as of this pass; a future flattening-table
//!    regeneration could reintroduce gaps here, which is exactly why that
//!    test exists as a drift guard).
//! 2. [`bridge_properties`] — 26.2 added properties pre-1.13 storage cannot
//!    carry propositionally (`waterlogged` on every waterloggable block,
//!    `powered` on trapdoors) or renamed/repurposed them (`leaves`decayable`
//!    /`check_decay`&rarr;`persistent`/`distance`). Each fixup here is
//!    individually justified in its own doc comment — this module does not
//!    apply a blanket "fill in whatever's missing" default, because most
//!    such defaults would be exactly the plausible-wrong-terrain CLAUDE.md
//!    warns about. The generic step (appending `waterlogged=false`) is safe
//!    for the *specific* reason that pre-1.13 has no waterlogging concept at
//!    all — every legacy block is unambiguously not waterlogged, not "we
//!    don't know so assume false".
//!
//! # What a caller does with a non-`Resolved` outcome
//!
//! This module deliberately mirrors [`crate::flattening::LegacyBlockState`]'s
//! shape rather than collapsing anything to air itself — see
//! [`CanonicalBlockState`]. The **adapter** (`packets/chunk.rs`) makes the
//! substitution decision, not this module; see its module docs for what it
//! chooses and why, and [`FallbackTally`] for how that choice stays visible
//! and counted rather than a silent hole in the terrain.

use std::collections::HashMap;
use std::sync::OnceLock;

use lodestone_data::block_states;

use crate::flattening::{self, LegacyBlockState, ResolvedState};

/// Number of `(old_block_id, meta)` slots this module can classify — the same
/// [`flattening::SLOT_COUNT`] (4095; see that module for the one excluded
/// slot).
pub const SLOT_COUNT: usize = flattening::SLOT_COUNT;

/// The result of resolving one legacy `(old_block_id, meta)` pair all the way
/// to a canonical 26.2 block-state id. Mirrors
/// [`flattening::LegacyBlockState`]'s four-outcome shape plus one more this
/// module's own bridging step can introduce — see the module docs for why
/// each is kept distinct rather than folded into a single "resolved" or a
/// single "failed".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanonicalBlockState {
    /// A canonical 26.2 [`block_states`] id, reachable either directly or
    /// after this module's rename/property bridging.
    Resolved(u32),
    /// [`LegacyBlockState::NoTableEntry`] passed through unchanged: vanilla's
    /// own flattening table never assigned this pair a target.
    NoTableEntry,
    /// [`LegacyBlockState::RequiresAdditionalContext`] passed through
    /// unchanged: identity needs TileEntity/neighbour data this table cannot
    /// carry (flower pots, skulls, double-plant upper halves).
    RequiresAdditionalContext,
    /// [`LegacyBlockState::OutOfBounds`] passed through unchanged: the one
    /// `old_block_id=255, meta=15` slot past the end of vanilla's own array.
    OutOfBounds,
    /// Vanilla's flattening table resolved this pair to a real name/property
    /// set, but even after [`bridge_name`]/[`bridge_properties`] it matches
    /// no canonical 26.2 state. As of this pass this is empirically **never**
    /// produced (see the exhaustive test at the bottom of this module) — it
    /// exists as an explicit, checkable escape hatch for the day a
    /// flattening-table regeneration (a jar update — see
    /// `docs/protocol-340-flattening-table.md`'s "How to change it")
    /// introduces a name/shape this pass's bridging does not yet cover,
    /// rather than that case silently producing a wrong id.
    Unmapped {
        /// The legacy-table's own resolved name (pre-rename), for
        /// diagnostics.
        legacy_name: &'static str,
    },
}

/// Resolves a legacy `(old_block_id, meta)` pair (see
/// [`flattening::lookup`]'s docs for the wire/storage format) to a canonical
/// 26.2 block-state id, applying this module's rename and property bridging
/// on top of the flattening table's own answer.
#[must_use]
pub fn resolve(old_block_id: u8, meta: u8) -> CanonicalBlockState {
    let meta = meta & 0x0F;
    if old_block_id == 255 && meta == 15 {
        return CanonicalBlockState::OutOfBounds;
    }
    let index = (old_block_id as usize) * 16 + (meta as usize);
    slot_table()[index]
}

/// The canonical 26.2 `minecraft:air` state id (looked up once, not
/// hardcoded to `0`, so a future registry regeneration that reorders states
/// cannot silently point this at the wrong block).
#[must_use]
pub fn air_state_id() -> u32 {
    *AIR_ID.get_or_init(|| {
        (0..block_states::STATE_COUNT)
            .find(|&id| {
                block_states::block_name(id) == Some("minecraft:air")
                    && block_states::properties(id) == Some(&[])
            })
            .expect("26.2 registry always defines minecraft:air with no properties")
    })
}

static AIR_ID: OnceLock<u32> = OnceLock::new();

/// Lazily built, indexed exactly like [`flattening::generated_flattening::SLOTS`]:
/// one [`CanonicalBlockState`] per `(old_block_id, meta)` slot except the one
/// structurally out-of-bounds slot ([`resolve`] special-cases that before
/// ever indexing here).
fn slot_table() -> &'static [CanonicalBlockState; SLOT_COUNT] {
    static TABLE: OnceLock<[CanonicalBlockState; SLOT_COUNT]> = OnceLock::new();
    TABLE.get_or_init(build_slot_table)
}

fn build_slot_table() -> [CanonicalBlockState; SLOT_COUNT] {
    let reverse = canonical_reverse_index();
    let mut table = [CanonicalBlockState::NoTableEntry; SLOT_COUNT];
    for old_block_id in 0..=255u8 {
        for meta in 0..16u8 {
            if old_block_id == 255 && meta == 15 {
                continue;
            }
            let index = (old_block_id as usize) * 16 + (meta as usize);
            table[index] = match flattening::lookup(old_block_id, meta) {
                LegacyBlockState::NoTableEntry => CanonicalBlockState::NoTableEntry,
                LegacyBlockState::RequiresAdditionalContext => {
                    CanonicalBlockState::RequiresAdditionalContext
                }
                LegacyBlockState::OutOfBounds => {
                    unreachable!("old_block_id==255 && meta==15 is skipped above")
                }
                LegacyBlockState::Resolved(state) => resolve_canonical(state, &reverse),
            };
        }
    }
    table
}

/// A canonical `(block name, sorted properties)` &rarr; state-id index, built
/// once from [`block_states`]'s static tables. `lodestone-data` ships only
/// the `id -> (name, properties)` direction (see its module docs on why: the
/// mesher/asset baker only ever need forward lookups); this is the reverse
/// index this crate's translation direction needs, built the same way
/// `lodestone-render`'s `BlockAtlas` and `v770`'s `resolve_state_id` build
/// theirs (iterate `0..STATE_COUNT` once).
fn canonical_reverse_index() -> HashMap<(&'static str, Vec<(&'static str, &'static str)>), u32> {
    let mut index = HashMap::with_capacity(block_states::STATE_COUNT as usize);
    for id in 0..block_states::STATE_COUNT {
        let name = block_states::block_name(id).expect("id in 0..STATE_COUNT");
        let properties = block_states::properties(id).expect("id in 0..STATE_COUNT");
        index.insert((name, properties.to_vec()), id);
    }
    index
}

/// Resolves one already-flattened [`ResolvedState`] to a canonical 26.2
/// state id via [`bridge_name`]/[`bridge_properties`], trying (in order) the
/// bridged name/properties exactly, then the same with `waterlogged=false`
/// appended (see the module docs for why that specific, and only that,
/// generic fallback is safe).
fn resolve_canonical(
    state: ResolvedState,
    reverse: &HashMap<(&'static str, Vec<(&'static str, &'static str)>), u32>,
) -> CanonicalBlockState {
    let (name, properties) = bridge(state.name, state.properties);

    if let Some(&id) = reverse.get(&(name, properties.clone())) {
        return CanonicalBlockState::Resolved(id);
    }

    // Pre-1.13 storage has no waterlogging concept at all: every legacy
    // block is unambiguously *not* waterlogged (there is no "unknown" case
    // to default away), so this one extra key is safe to try generically
    // rather than needing a per-block-name entry the way the other fixups
    // do. Only tried as a fallback so it never overrides a block that
    // already matched without it (e.g. one that has no waterlogged state).
    let mut with_waterlogged = properties.clone();
    insert_sorted(&mut with_waterlogged, "waterlogged", "false");
    if let Some(&id) = reverse.get(&(name, with_waterlogged)) {
        return CanonicalBlockState::Resolved(id);
    }

    CanonicalBlockState::Unmapped {
        legacy_name: state.name,
    }
}

/// Renames vanilla's own (intermediate, pre-26.2) flattening-table spelling
/// to the current 26.2 registry name. See the module docs for how each entry
/// was verified (registry existence + shape, not jar-traced) and
/// `docs/protocol-340-flattening-table.md`'s "Not authoritative for" section
/// for the first six, which *are* jar-confirmed disagreements between the
/// 1.13.2 table and 1.13.2's own final registry.
fn bridge_name(name: &'static str) -> &'static str {
    match name {
        // Jar-confirmed intermediate spellings (see the doc's
        // "Not authoritative for" table).
        "minecraft:mob_spawner" => "minecraft:spawner",
        "minecraft:melon_block" => "minecraft:melon",
        "minecraft:portal" => "minecraft:nether_portal",
        "minecraft:oak_bark" => "minecraft:oak_wood",
        "minecraft:spruce_bark" => "minecraft:spruce_wood",
        "minecraft:birch_bark" => "minecraft:birch_wood",
        "minecraft:jungle_bark" => "minecraft:jungle_wood",
        "minecraft:acacia_bark" => "minecraft:acacia_wood",
        "minecraft:dark_oak_bark" => "minecraft:dark_oak_wood",
        // Renamed later still (post-1.13.2), confirmed only by registry
        // existence/shape per the module docs, not jar-traced:
        // - `sign`/`wall_sign` split into per-species blocks at 1.14
        //   (`minecraft-data`'s 1.13 blocks.json still calls it bare `sign`;
        //   26.2 has none of that name, only `oak_sign`/`oak_wall_sign` etc.
        //   — 1.12.2 only ever had one wood type, so `oak_` is unambiguous).
        "minecraft:sign" => "minecraft:oak_sign",
        "minecraft:wall_sign" => "minecraft:oak_wall_sign",
        // - the single-height tallgrass block (not `grass_block`) was
        //   renamed `grass` -> `short_grass` in 1.20.3 to disambiguate from
        //   the ground block.
        "minecraft:grass" => "minecraft:short_grass",
        // - renamed `grass_path` -> `dirt_path` in 1.17.
        "minecraft:grass_path" => "minecraft:dirt_path",
        other => other,
    }
}

/// Bridges one flattening-table result to a `(name, properties)` pair to try
/// against the canonical reverse index, applying [`bridge_name`] plus
/// whatever property fixup that (renamed) block family needs. Two families
/// — cauldron and the two 1.12.2 walls — change *identity* based on a
/// property's *value*, not just its name, so they are handled here rather
/// than split across a name-only and a properties-only pass.
fn bridge(
    name: &'static str,
    properties: &'static [(&'static str, &'static str)],
) -> (&'static str, Vec<(&'static str, &'static str)>) {
    let name = bridge_name(name);
    match name {
        // 1.12.2's single `cauldron` (old id 118) carried water level 0-3 as
        // meta. 26.2 splits level=0 (empty) into its own `cauldron` block
        // and levels 1-3 into a separate `water_cauldron` block with a
        // `level` property — an identity split driven by the property
        // *value*, the same shape as the mixed-id families
        // `docs/protocol-340-flattening-table.md` documents for the
        // flattening table itself (slab material, anvil damage, ...), just
        // one step further down the rename chain. Confirmed against the
        // registry: `water_cauldron` states are exactly `level in 1..=3`,
        // matching 1.12.2's own non-zero meta range.
        "minecraft:cauldron" => {
            let level = properties
                .iter()
                .find(|&&(key, _)| key == "level")
                .map_or("0", |&(_, value)| value);
            if level == "0" {
                ("minecraft:cauldron", Vec::new())
            } else {
                ("minecraft:water_cauldron", vec![("level", level)])
            }
        }
        // 1.12.2 walls (only cobblestone/mossy existed pre-1.13) connect
        // with a plain boolean per direction. 26.2's 1.16 "tall walls"
        // rework replaced the four cardinal booleans with a three-value
        // `none`/`low`/`tall` enum (only `up` stayed boolean); a legacy
        // connection has no way to express "tall", so `true` bridges to the
        // pre-1.16 visual equivalent, `low`, rather than being dropped or
        // guessed as `tall`.
        "minecraft:cobblestone_wall" | "minecraft:mossy_cobblestone_wall" => {
            let mut bridged: Vec<(&'static str, &'static str)> = properties
                .iter()
                .map(|&(key, value)| match key {
                    "north" | "south" | "east" | "west" => {
                        (key, if value == "true" { "low" } else { "none" })
                    }
                    _ => (key, value),
                })
                .collect();
            bridged.sort_by_key(|&(key, _)| key);
            (name, bridged)
        }
        _ => (name, bridge_properties(name, properties)),
    }
}

/// Fills in properties 26.2 added or repurposed that pre-1.13 `id:meta`
/// cannot carry, for the specific block families where a single fixed answer
/// is actually correct (not merely convenient) — see each arm's comment. Any
/// block not listed here keeps its properties unchanged (before the generic
/// `waterlogged` fallback in [`resolve_canonical`] gets a try). Called from
/// [`bridge`] for every family that does not also need an identity change.
fn bridge_properties(
    name: &'static str,
    properties: &'static [(&'static str, &'static str)],
) -> Vec<(&'static str, &'static str)> {
    match name {
        // The "bark on every face" log variant (see the flattening doc's
        // mixed-id case for `log`/`log2`) carries no axis in `id:meta` — it
        // never needed one pre-1.13, since it was a distinct *identity*
        // rather than an axis value. 26.2's `*_wood` keeps an `axis`
        // property for consistency with `*_log`, but every axis renders
        // identically for the all-bark variant (bark texture on all six
        // faces regardless of orientation), so any value is visually
        // correct; `y` is picked as the block's own registry default.
        "minecraft:oak_wood"
        | "minecraft:spruce_wood"
        | "minecraft:birch_wood"
        | "minecraft:jungle_wood"
        | "minecraft:acacia_wood"
        | "minecraft:dark_oak_wood"
            if properties.is_empty() =>
        {
            vec![("axis", "y")]
        }

        // Pre-1.13 note blocks carry no instrument/note/powered in
        // `id:meta` at all — those were (and still are) TileEntity/circuit
        // state, not identity. Unlike flower pots/skulls this is *not*
        // flagged `RequiresAdditionalContext` by the flattening table
        // because block *identity* is never ambiguous (id 25 is always
        // exactly one note block); only its display properties are unknown.
        // None of the three affects the block's mesh (a note block renders
        // identically regardless of instrument/note/power), so a fixed
        // default is visually correct even though it is not data-faithful;
        // `instrument=harp, note=0, powered=false` matches the registry's
        // own default state.
        "minecraft:note_block" if properties.is_empty() => {
            vec![("instrument", "harp"), ("note", "0"), ("powered", "false")]
        }

        // Leaves: `decayable` is exactly the inverse of 26.2's `persistent`
        // (decayable=true means "not player-placed, eligible to decay",
        // i.e. persistent=false) — a real derived value, not a guess.
        // `distance` (1-7, nearest-log flood-fill) has no pre-1.13
        // equivalent at all (`check_decay` was an internal recheck flag, not
        // a distance) and cannot be computed from a single block's meta;
        // `7` (maximum, "not close enough to a log to be protected from
        // decay") is used as an explicit, documented placeholder rather than
        // a claimed-correct value. This is safe for this project's current
        // scope because decay is server-simulated in vanilla and this
        // client never runs leaf-decay logic itself, and `distance` does
        // not affect the leaf block's mesh or collision — but it is real
        // data loss if that ever changes, which is why it is called out
        // here rather than silently baked in.
        "minecraft:oak_leaves"
        | "minecraft:spruce_leaves"
        | "minecraft:birch_leaves"
        | "minecraft:jungle_leaves"
        | "minecraft:acacia_leaves"
        | "minecraft:dark_oak_leaves" => {
            let decayable = properties
                .iter()
                .find(|&&(key, _)| key == "decayable")
                .map(|&(_, value)| value);
            let persistent = if decayable == Some("false") {
                "true"
            } else {
                "false"
            };
            vec![("distance", "7"), ("persistent", persistent)]
        }

        // Trapdoors gained a `powered` property (redstone-circuit state,
        // like note block's fields not stored in pre-1.13 `id:meta`) that
        // does not affect the block's mesh (open/closed is the only
        // geometry-relevant state, already carried by `open`/`half`), so
        // `false` is a safe placeholder rather than a claimed-correct value.
        "minecraft:oak_trapdoor" | "minecraft:iron_trapdoor" => {
            let mut properties = properties.to_vec();
            insert_sorted(&mut properties, "powered", "false");
            properties
        }

        _ => properties.to_vec(),
    }
}

/// Inserts `(key, value)` into `properties`, kept sorted by key to match both
/// [`ResolvedState::properties`]'s and [`block_states::properties`]'s
/// documented sorted-by-key convention.
fn insert_sorted(
    properties: &mut Vec<(&'static str, &'static str)>,
    key: &'static str,
    value: &'static str,
) {
    let position = properties
        .iter()
        .position(|&(existing, _)| existing > key)
        .unwrap_or(properties.len());
    properties.insert(position, (key, value));
}

/// Per-column tally of how many blocks needed each kind of fallback
/// substitution during canonicalisation, so the adapter's air substitution
/// (see `packets/chunk.rs`) is counted and traceable per CLAUDE.md's
/// evidence standards, not an invisible hole in the terrain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FallbackTally {
    /// Blocks substituted because vanilla's own flattening table never
    /// assigned this `(old_block_id, meta)` pair a target.
    pub no_table_entry: u32,
    /// Blocks substituted because identity needs TileEntity/neighbour data
    /// this crate does not decode (flower pots, skulls, double-plant upper
    /// halves — see `crate::packets::chunk`'s module docs for why block
    /// entities are consumed but not retained here).
    pub requires_additional_context: u32,
    /// Blocks substituted because the pair is the one structurally
    /// out-of-bounds slot (`old_block_id=255, meta=15`).
    pub out_of_bounds: u32,
    /// Blocks substituted because [`bridge_name`]/[`bridge_properties`]
    /// could not bridge this pair's resolved name/properties to any 26.2
    /// state — see [`CanonicalBlockState::Unmapped`]. Always `0` as of this
    /// pass (see the exhaustive test below); a nonzero count here means a
    /// flattening-table regeneration introduced a case this module's
    /// bridging does not cover yet.
    pub unmapped: u32,
}

impl FallbackTally {
    /// Whether any block in this tally needed a fallback substitution.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn record(&mut self, outcome: CanonicalBlockState) {
        match outcome {
            CanonicalBlockState::Resolved(_) => {}
            CanonicalBlockState::NoTableEntry => self.no_table_entry += 1,
            CanonicalBlockState::RequiresAdditionalContext => {
                self.requires_additional_context += 1;
            }
            CanonicalBlockState::OutOfBounds => self.out_of_bounds += 1,
            CanonicalBlockState::Unmapped { .. } => self.unmapped += 1,
        }
    }
}

/// Resolves `(old_block_id, meta)` to a canonical 26.2 state id, substituting
/// [`air_state_id`] and recording into `tally` for every non-[`Resolved`]
/// outcome. This is the single integration point `packets/chunk.rs` calls
/// per legacy value — see its module docs for why air (rather than e.g.
/// rejecting the packet) is the chosen substitution, and for how `tally` is
/// surfaced afterwards.
///
/// [`Resolved`]: CanonicalBlockState::Resolved
#[must_use]
pub fn resolve_or_air(old_block_id: u8, meta: u8, tally: &mut FallbackTally) -> u32 {
    let outcome = resolve(old_block_id, meta);
    tally.record(outcome);
    match outcome {
        CanonicalBlockState::Resolved(id) => id,
        CanonicalBlockState::NoTableEntry
        | CanonicalBlockState::RequiresAdditionalContext
        | CanonicalBlockState::OutOfBounds
        | CanonicalBlockState::Unmapped { .. } => air_state_id(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drift guard for this module's rename/property bridging: every one of
    /// the flattening table's 1663 `Resolved` slots must reach a canonical
    /// 26.2 state through [`resolve`] — i.e. [`CanonicalBlockState::Unmapped`]
    /// must never actually occur for the table as it exists today. This is
    /// an *exhaustive* check (all 4095 slots), not a sample, because the
    /// whole point of keeping `Unmapped` distinct from silently defaulting
    /// is that a real occurrence must be visible — including here, before it
    /// ever reaches a live chunk decode.
    #[test]
    fn no_slot_is_unmapped() {
        let mut resolved = 0;
        let mut no_table_entry = 0;
        let mut requires_context = 0;
        let mut unmapped = Vec::new();
        for old_block_id in 0..=255u8 {
            for meta in 0..16u8 {
                match resolve(old_block_id, meta) {
                    CanonicalBlockState::Resolved(_) => resolved += 1,
                    CanonicalBlockState::NoTableEntry => no_table_entry += 1,
                    CanonicalBlockState::RequiresAdditionalContext => requires_context += 1,
                    CanonicalBlockState::OutOfBounds => {}
                    CanonicalBlockState::Unmapped { legacy_name } => {
                        unmapped.push((old_block_id, meta, legacy_name));
                    }
                }
            }
        }
        assert!(
            unmapped.is_empty(),
            "{} slot(s) resolved by the flattening table have no canonical 26.2 mapping: \
             {unmapped:?}",
            unmapped.len()
        );
        // Matches docs/protocol-340-flattening-table.md's own counts exactly
        // (1663 Resolved / 2400 NoTableEntry / 32 RequiresAdditionalContext),
        // as a second drift guard: if the committed flattening table is ever
        // regenerated, this catches a shift in *which* outcome a slot
        // produces, not just whether bridging covers it.
        assert_eq!(resolved, 1663);
        assert_eq!(no_table_entry, 2400);
        assert_eq!(requires_context, 32);
    }

    #[test]
    fn air_state_id_is_air_with_no_properties() {
        let id = air_state_id();
        assert_eq!(block_states::block_name(id), Some("minecraft:air"));
        assert_eq!(block_states::properties(id), Some(&[][..]));
    }

    #[test]
    fn out_of_bounds_slot_is_out_of_bounds() {
        assert_eq!(resolve(255, 15), CanonicalBlockState::OutOfBounds);
    }

    #[test]
    fn resolve_or_air_counts_fallbacks() {
        let mut tally = FallbackTally::default();
        // (253, 0) is one of the two block ids never assigned to any real
        // 1.12.2 block (see the flattening doc) -> NoTableEntry.
        let id = resolve_or_air(253, 0, &mut tally);
        assert_eq!(id, air_state_id());
        assert_eq!(tally.no_table_entry, 1);
        assert!(!tally.is_empty());
    }
}
