//! Per-block blast resistance and flammability for protocol 776 (Minecraft
//! 26.2) — the four numbers vanilla's explosion and fire code read off a
//! *block*, plus a flat per-block-**state** resistance table for the ray-walk
//! hot path.
//!
//! # What it is
//!
//! Four facts per block, all of them fields on vanilla's own block-behaviour
//! base class that no subclass overrides:
//!
//! | field | vanilla | consumer |
//! |---|---|---|
//! | [`BlockBlast::explosion_resistance`] | vanilla's own "get explosion resistance" accessor | vanilla's own explosion "calculate exploded positions" step's per-step ray cost |
//! | [`BlockBlast::ignite_odds`] | vanilla's own fire-block class's own ignite-odds map | vanilla's own fire-block "get ignite odds" accessor — *can fire start on this cell* |
//! | [`BlockBlast::burn_odds`] | vanilla's own fire-block class's own burn-odds map | vanilla's own fire-block "check burn out" step — *can this block be consumed* |
//! | [`BlockBlast::ignited_by_lava`] | vanilla's own block-state base class's own "ignited by lava" field | vanilla's own lava-fluid "is flammable" check — lava starting a fire nearby |
//!
//! **`ignite_odds > 0` and `ignited_by_lava` are different sets and neither
//! contains the other.** Measured on the real dump: 207 blocks are flammable to
//! fire, 312 are lava-ignitable, and both differences are non-empty — every
//! *bed* and `note_block` is lava-ignitable but has no ignite odds, while every
//! small *flower*, `hay_block`, `coal_block` and `scaffolding` has ignite odds
//! but is **not** lava-ignitable. Deriving one from the other is therefore wrong
//! in both directions, which is exactly why both columns are dumped.
//!
//! # Two shapes, deliberately
//!
//! The four-column table is keyed **per block type**, because all four values
//! live on the `Block` — a per-state copy would be 32,366 rows of at most 1,196
//! distinct values, which is the trap the explosion issue's own body calls out.
//!
//! Resistance additionally gets a **flat per-block-state array**
//! ([`explosion_resistance_for_state_id`]), because that lookup is the explosion
//! ray walk's innermost operation: 1,352 rays × up to ~13 steps each, so tens of
//! thousands of lookups per blast. Two things are folded into it at generation
//! time so the hot path is a single bounds-checked index with no string work at
//! all:
//!
//! * vanilla's own fluid-state "get explosion resistance" accessor, via
//!   vanilla's own explosion damage calculator's "get block explosion
//!   resistance" step's `max(block, fluid)` — so a waterlogged fence already
//!   reads `100.0`;
//! * vanilla's own empty-optional result for a cell that is both air and fluid-free,
//!   encoded as [`EMPTY_RESISTANCE`].
//!
//! The neighbouring per-state string helper exists for callers that hold a state
//! string rather than an id and is documented as the slow path.
//!
//! The two *fire* odds columns keep only the by-name form: vanilla's own
//! fire-block tick
//! reads at most a couple of dozen cells, so nothing there is hot, and the one
//! state-level rule (the ignite/burn-odds accessors return `0` for
//! `waterlogged=true`) is a cheap property check.
//!
//! # Data provenance
//!
//! `blocks.json` carries neither an explosion-resistance nor any flammability
//! field, and the fire odds are not even reachable from a block's *properties* —
//! they live in two private maps that vanilla's own fire-block class's own
//! bootstrap step
//! fills at boot. So this is a JVM dump, generated the same way
//! [`crate::hardness`] and [`crate::collision_shapes`] are: boot the real 26.2
//! server headlessly and ask it. See `tests/block_blast.rs` for the generator
//! and drift guard, `oracle-java/BlastFireOracle.java` for the extraction, and
//! `just oracle-blast-fire` / `just regen-blast-fire` to refresh.
//!
//! **A hand-transcribed flammability table would have been wrong.**
//! Vanilla's own fire-block bootstrap step registers two of its entries through
//! a per-wool-colour and a per-carpet-colour loop rather than by name, so
//! reading the decompiled source alone yields a table missing 32 blocks.
//!
//! # Memory design
//!
//! The 1,196 blocks collapse to a few dozen distinct
//! `(resistance, ignite, burn, lava)` tuples, so the by-block table is a
//! de-duplicated `ENTRIES` array plus a name-sorted `(name, entry index)` array;
//! lookup is one binary search over `&'static str`s and no allocation. The
//! per-state resistance array is a `u16` index per state into a small
//! `RESISTANCE_VALUES` table — the same shape [`crate::light_props`] uses, so
//! 32,366 states cost 65 KB of rodata and zero heap. Resistances are stored as
//! raw `f32` bits and rebuilt with [`f32::from_bits`], so nothing is lost to
//! float-literal formatting.
//!
//! # Dependencies
//!
//! [`crate::generated_block_blast`], and [`StateId::from_state_str`] for the
//! string-to-id resolution on the slow path. Consumers: `lodestone-server`'s `fire`
//! (spread and burnout) and `explosion_blocks` (blast destruction).

use crate::block::Block;
use crate::block_states::StateId;
use crate::generated_block_blast as table;

pub use table::{BLOCK_COUNT, EMPTY_RESISTANCE, STATE_COUNT};

/// One block's blast/flammability facts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockBlast {
    /// Vanilla's own "get explosion resistance" accessor. `0.0` for a block that costs a blast ray
    /// nothing beyond the fixed step term; `3600000.0` for bedrock and friends.
    pub explosion_resistance: f32,
    /// Vanilla's own fire-block class's own ignite-odds map for this block — `0` means fire can never start
    /// *on* it (its own "can burn" check is `igniteOdds > 0`). Values in the real
    /// table are exactly `{0, 5, 15, 30, 60}`.
    pub ignite_odds: u8,
    /// Vanilla's own fire-block class's own burn-odds map — how readily its own
    /// "check burn out" step consumes the block
    /// itself. Values are exactly `{0, 5, 20, 60, 100}`.
    pub burn_odds: u8,
    /// Vanilla's own block-state base class's own "ignited by lava" field. Read off the default state; the flag is a
    /// per-block property, identical for every state.
    pub ignited_by_lava: bool,
}

impl BlockBlast {
    /// The all-zero, non-flammable answer for an unknown block name — the same
    /// value `minecraft:air` itself carries, and therefore the safe default: a
    /// blast ray pays only its step cost, and fire neither starts nor spreads.
    pub const INERT: BlockBlast = BlockBlast {
        explosion_resistance: 0.0,
        ignite_odds: 0,
        burn_odds: 0,
        ignited_by_lava: false,
    };
}

/// Strips a `[...]` property suffix, so a canonical state string may be passed
/// to any of the by-name lookups below.
#[must_use]
fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// The value of `state`'s `key=` property. A whole-key match, so `level` cannot
/// be found inside another property's name.
fn property_of<'s>(state: &'s str, key: &str) -> Option<&'s str> {
    let props = state.split_once('[')?.1.strip_suffix(']')?;
    props.split(',').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == key).then_some(v.trim())
    })
}

/// The blast/flammability facts for `block`, which may be a bare block path, a
/// full canonical block name, or a full canonical state string (the `[...]`
/// suffix is ignored).
///
/// `None` for a name not in the 26.2 block registry. Prefer [`blast_or_inert`]
/// when an unknown name should simply behave like air.
#[must_use]
pub fn blast(block: &str) -> Option<BlockBlast> {
    let block = Block::from_name(base_name(block))?;
    let entry = table::ENTRY_BY_REGISTRY_ID[usize::from(block.registry_id())];
    let (bits, ignite_odds, burn_odds, ignited_by_lava) = table::ENTRIES[entry as usize];
    Some(BlockBlast {
        explosion_resistance: f32::from_bits(bits),
        ignite_odds,
        burn_odds,
        ignited_by_lava,
    })
}

/// [`blast`], with [`BlockBlast::INERT`] for an unknown name.
#[must_use]
pub fn blast_or_inert(block: &str) -> BlockBlast {
    blast(block).unwrap_or(BlockBlast::INERT)
}

/// `FireBlock::getIgniteOdds` — the block's ignite odds, **or `0` when the state
/// is `waterlogged=true`**.
///
/// The waterlogged override is the whole reason this exists next to [`blast`]: a
/// waterlogged oak fence must not catch fire, and the block-level table says
/// `5`.
#[must_use]
pub fn ignite_odds_for_state(state: &str) -> u8 {
    if property_of(state, "waterlogged") == Some("true") {
        return 0;
    }
    blast_or_inert(state).ignite_odds
}

/// `FireBlock::getBurnOdds` — the block's burn odds, **or `0` when the state is
/// `waterlogged=true`**.
#[must_use]
pub fn burn_odds_for_state(state: &str) -> u8 {
    if property_of(state, "waterlogged") == Some("true") {
        return 0;
    }
    blast_or_inert(state).burn_odds
}

/// The fluid explosion resistance both vanilla fluids report
/// (the water and lava fluid classes both inherit the base fluid class's
/// `100.0F`), reachable through vanilla's own fluid-state "get explosion
/// resistance" accessor.
pub const FLUID_EXPLOSION_RESISTANCE: f32 = 100.0;

/// **The explosion hot path.** Vanilla's own explosion damage calculator's
/// "get block explosion resistance" step
/// for validated block state `id`, as a single flat array index.
///
/// `None` is vanilla's own empty-optional result — the cell is air *and* holds no fluid,
/// so a blast ray pays no resistance term for it at all.
///
/// The `max(block, fluid)` is already folded in at generation time, so a
/// waterlogged state answers `100.0` with no property parsing here.
#[must_use]
pub fn explosion_resistance_for_state_id(id: StateId) -> Option<f32> {
    let entry = table::STATE_RESISTANCE_ENTRY[id.raw() as usize];
    let bits = table::RESISTANCE_VALUES[entry as usize];
    (bits != EMPTY_RESISTANCE).then(|| f32::from_bits(bits))
}

/// The string-keyed form of [`explosion_resistance_for_state_id`] — **the slow
/// path**, kept for callers holding a state string rather than an id.
///
/// Resolves through [`StateId::from_state_str`] when it can, so the answer is
/// byte-identical to the flat table's, and falls back to name + `waterlogged`
/// parsing for a state string the built-in registry cannot resolve at all.
#[must_use]
pub fn explosion_resistance_for_state(state: &str) -> Option<f32> {
    if let Some(id) = StateId::from_state_str(state) {
        return explosion_resistance_for_state_id(id);
    }
    let name = base_name(state);
    let is_air = matches!(
        name,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    );
    let waterlogged = property_of(state, "waterlogged") == Some("true");
    if is_air && !waterlogged {
        return None;
    }
    let block = blast_or_inert(name).explosion_resistance;
    Some(if waterlogged {
        block.max(FLUID_EXPLOSION_RESISTANCE)
    } else {
        block
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-picked anchors spanning the whole resistance range, each read out of
    /// the committed dump rather than from memory.
    #[test]
    fn known_resistances() {
        assert_eq!(blast("minecraft:stone").unwrap().explosion_resistance, 6.0);
        assert_eq!(blast("minecraft:obsidian").unwrap().explosion_resistance, 1200.0);
        assert_eq!(blast("minecraft:bedrock").unwrap().explosion_resistance, 3_600_000.0);
        assert_eq!(blast("minecraft:dirt").unwrap().explosion_resistance, 0.5);
        assert_eq!(blast("minecraft:water").unwrap().explosion_resistance, 100.0);
        assert_eq!(blast("minecraft:tnt").unwrap().explosion_resistance, 0.0);
    }

    /// The three fire columns for one block from each odds tier.
    #[test]
    fn known_fire_odds() {
        let planks = blast("minecraft:oak_planks").unwrap();
        assert_eq!((planks.ignite_odds, planks.burn_odds), (5, 20));
        let log = blast("minecraft:oak_log").unwrap();
        assert_eq!((log.ignite_odds, log.burn_odds), (5, 5));
        let leaves = blast("minecraft:oak_leaves").unwrap();
        assert_eq!((leaves.ignite_odds, leaves.burn_odds), (30, 60));
        let grass = blast("minecraft:short_grass").unwrap();
        assert_eq!((grass.ignite_odds, grass.burn_odds), (60, 100));
        let stone = blast("minecraft:stone").unwrap();
        assert_eq!((stone.ignite_odds, stone.burn_odds), (0, 0));
    }

    /// The per-wool-colour and per-carpet-colour registration loops are the two entries a
    /// source-only transcription of vanilla's own fire-block bootstrap step would miss, so pin one
    /// member of each: wool is `(30, 60)`, carpet `(60, 20)`.
    #[test]
    fn the_two_list_registered_families_are_present() {
        for colour in ["white", "red", "black", "lime"] {
            let wool = blast(&format!("minecraft:{colour}_wool")).unwrap();
            assert_eq!((wool.ignite_odds, wool.burn_odds), (30, 60), "{colour}_wool");
            let carpet = blast(&format!("minecraft:{colour}_carpet")).unwrap();
            assert_eq!((carpet.ignite_odds, carpet.burn_odds), (60, 20), "{colour}_carpet");
        }
    }

    /// The waterlogged override, plus its control: the *same* block without the
    /// property keeps its real odds, so the test measures the property and not
    /// some unrelated lookup failure.
    #[test]
    fn waterlogged_zeroes_both_odds_but_the_dry_state_does_not() {
        assert_eq!(ignite_odds_for_state("minecraft:oak_fence[waterlogged=true]"), 0);
        assert_eq!(burn_odds_for_state("minecraft:oak_fence[waterlogged=true]"), 0);
        assert_eq!(ignite_odds_for_state("minecraft:oak_fence[waterlogged=false]"), 5);
        assert_eq!(burn_odds_for_state("minecraft:oak_fence[waterlogged=false]"), 20);
        assert_eq!(ignite_odds_for_state("minecraft:oak_fence"), 5);
    }

    /// Air with no fluid is vanilla's own empty-optional result; every other case is a real
    /// number, and a waterlogged slab resists at the fluid's `100.0` rather than
    /// the slab's own `6.0`.
    #[test]
    fn resistance_for_state_matches_the_calculator() {
        assert_eq!(explosion_resistance_for_state("minecraft:air"), None);
        assert_eq!(explosion_resistance_for_state("minecraft:cave_air"), None);
        assert_eq!(explosion_resistance_for_state("minecraft:stone"), Some(6.0));
        assert_eq!(
            explosion_resistance_for_state("minecraft:stone_slab[type=bottom,waterlogged=true]"),
            Some(100.0)
        );
        assert_eq!(
            explosion_resistance_for_state("minecraft:stone_slab[type=bottom,waterlogged=false]"),
            Some(6.0)
        );
        // Obsidian beats the fluid, so the `max` really is a max and not a
        // "waterlogged wins" shortcut.
        assert_eq!(explosion_resistance_for_state("minecraft:obsidian"), Some(1200.0));
    }

    /// The flat per-state array and the string path must agree on every state —
    /// the property that lets the hot path skip the string work entirely. Run
    /// over all 32,366 states, so a single transposed row fails.
    #[test]
    fn the_flat_state_table_agrees_with_the_string_path_on_every_state() {
        let mut empty = 0usize;
        let mut fluid_capped = 0usize;
        for id in 0..STATE_COUNT {
            let state_id = StateId::new(id).expect("generated state id is valid");
            let name = state_id.name();
            let waterlogged = state_id
                .properties()
                .iter()
                .any(|(k, v)| *k == "waterlogged" && *v == "true");
            let flat = explosion_resistance_for_state_id(state_id);
            let block = blast_or_inert(name).explosion_resistance;
            let is_air = matches!(
                name,
                "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
            );
            let expected = if is_air && !waterlogged {
                empty += 1;
                None
            } else if waterlogged {
                fluid_capped += 1;
                Some(block.max(FLUID_EXPLOSION_RESISTANCE))
            } else {
                Some(block)
            };
            assert_eq!(flat, expected, "state id {id} ({name}, waterlogged={waterlogged})");
        }
        // Magnitude, not sign: 26.2 has exactly three air states and a large but
        // finite set of waterloggable ones. A table that answered `None`
        // everywhere would pass an "agrees" loop written the lazy way.
        assert_eq!(empty, 3, "exactly air, cave_air and void_air are Optional.empty()");
        assert!(fluid_capped > 1000, "waterloggable states, got {fluid_capped}");
    }

    #[test]
    fn validated_state_lookup_is_total_and_invalid_raw_ids_stop_at_boundary() {
        let lookup = explosion_resistance_for_state_id
            as fn(crate::block_states::StateId) -> Option<f32>;
        let state = crate::block_states::StateId::new(0).expect("state zero is valid");
        let _ = lookup(state);
        assert!(crate::block_states::StateId::new(STATE_COUNT).is_none());
        assert!(crate::block_states::StateId::new(u32::MAX).is_none());
    }

    /// The two sets that must not be conflated — see this module's own doc
    /// comment. Both directions are asserted, so a "derive one from the other"
    /// regression fails whichever way round it is written.
    #[test]
    fn lava_ignitable_and_fire_flammable_are_different_sets() {
        let bed = blast("minecraft:white_bed").unwrap();
        assert!(bed.ignited_by_lava, "a bed is lava-ignitable");
        assert_eq!(bed.ignite_odds, 0, "a bed has no fire ignite odds");

        let poppy = blast("minecraft:poppy").unwrap();
        assert_eq!(poppy.ignite_odds, 60, "a poppy is flammable to fire");
        assert!(!poppy.ignited_by_lava, "a poppy is not lava-ignitable");

        let planks = blast("minecraft:oak_planks").unwrap();
        assert!(planks.ignited_by_lava && planks.ignite_odds > 0, "planks are both");

        let stone = blast("minecraft:stone").unwrap();
        assert!(!stone.ignited_by_lava && stone.ignite_odds == 0, "stone is neither");
    }

    /// Unknown names fall back to air's own answer rather than panicking.
    #[test]
    fn unknown_names_are_inert() {
        assert_eq!(blast("minecraft:not_a_block"), None);
        assert_eq!(blast_or_inert("minecraft:not_a_block"), BlockBlast::INERT);
        assert_eq!(blast("minecraft:air").unwrap(), BlockBlast::INERT);
    }

    /// An unknown dynamic state stays on the string fallback rather than being
    /// coerced into a built-in state merely to reach the flat table.
    #[test]
    fn unknown_dynamic_states_keep_their_waterlogged_fallback() {
        assert_eq!(
            explosion_resistance_for_state("example:reservoir[waterlogged=true]"),
            Some(FLUID_EXPLOSION_RESISTANCE)
        );
        assert_eq!(
            explosion_resistance_for_state("example:reservoir[waterlogged=false]"),
            Some(0.0)
        );
    }

    #[test]
    fn full_bare_and_state_suffixed_names_resolve_to_the_same_block() {
        let full = blast("minecraft:oak_fence");

        assert_eq!(blast("oak_fence"), full);
        assert_eq!(blast("minecraft:oak_fence[waterlogged=false]"), full);
    }

    /// Every built-in block has one generated entry index, and every index
    /// points into the de-duplicated facts table.
    #[test]
    fn entry_index_is_complete_and_in_bounds() {
        assert_eq!(table::ENTRY_BY_REGISTRY_ID.len(), BLOCK_COUNT as usize);
        assert!(
            table::ENTRY_BY_REGISTRY_ID
                .iter()
                .all(|&entry| usize::from(entry) < table::ENTRIES.len()),
            "every generated registry entry index must address ENTRIES"
        );
    }
}
