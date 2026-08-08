//! Per-block-type blast resistance and flammability for protocol 776
//! (Minecraft 26.2) — the four numbers vanilla's explosion and fire code read
//! off a *block* (rather than off a block state).
//!
//! # What it is
//!
//! Four facts per block, all of them fields on `Block`/`BlockBehaviour` that no
//! subclass overrides:
//!
//! | field | vanilla | consumer |
//! |---|---|---|
//! | [`BlockBlast::explosion_resistance`] | `Block.getExplosionResistance()` | `ServerExplosion.calculateExplodedPositions`'s per-step ray cost |
//! | [`BlockBlast::ignite_odds`] | `FireBlock`'s `igniteOdds` map | `FireBlock.getIgniteOdds` — *can this cell catch* |
//! | [`BlockBlast::burn_odds`] | `FireBlock`'s `burnOdds` map | `FireBlock.checkBurnOut` — *can this block be consumed* |
//! | [`BlockBlast::ignited_by_lava`] | `BlockState.ignitedByLava()` | `LavaFluid.isFlammable` — lava starting a fire nearby |
//!
//! **`ignite_odds > 0` and `ignited_by_lava` are different sets and neither
//! contains the other.** Measured on the real dump: 207 blocks are flammable to
//! fire, 312 are `ignitedByLava`, and both differences are non-empty — every
//! *bed* and `note_block` is lava-ignitable but has no ignite odds, while every
//! small *flower*, `hay_block`, `coal_block` and `scaffolding` has ignite odds
//! but is **not** lava-ignitable. Deriving one from the other is therefore wrong
//! in both directions, which is exactly why both columns are dumped.
//!
//! # Why per-block-type, not per-block-state
//!
//! Unlike [`crate::hardness`] (which really is per state — a `type=double` slab
//! differs), all four of these live on the `Block`, so a per-state table would
//! be 32,366 rows of at most 1,196 distinct values. Issue #313's own body calls
//! this out as a trap and it is confirmed by the dump.
//!
//! Two *state*-level rules do exist, and they are the consumer's job because
//! they are cheap string checks that would otherwise force the 32,366-row shape:
//!
//! * `FireBlock.getBurnOdds`/`getIgniteOdds` (`FireBlock.java:213-223`) return
//!   `0` for any state with `waterlogged=true`, regardless of the block's own
//!   odds — see [`ignite_odds_for_state`]/[`burn_odds_for_state`], which apply
//!   it.
//! * `ExplosionDamageCalculator.getBlockExplosionResistance`
//!   (`ExplosionDamageCalculator.java:11-17`) takes
//!   `max(block resistance, fluid resistance)`, and water/lava both carry
//!   `100.0`, so a waterlogged cell resists at `100.0` even when its block is a
//!   fence. See [`explosion_resistance_for_state`].
//!
//! # Data provenance
//!
//! `blocks.json` carries neither `explosionResistance` nor any flammability
//! field, and the fire odds are not even reachable from a block's *properties* —
//! they live in two private `Object2IntMap<Block>`s that `FireBlock.bootStrap()`
//! fills at boot. So this is a JVM dump, generated the same way
//! [`crate::hardness`] and [`crate::collision_shapes`] are: boot the real 26.2
//! server headlessly and ask it. See `tests/block_blast.rs` for the generator
//! and drift guard, `oracle-java/BlastFireOracle.java` for the extraction, and
//! `just oracle-blast-fire` / `just regen-blast-fire` to refresh.
//!
//! **A hand-transcribed flammability table would have been wrong.**
//! `FireBlock.bootStrap()` registers two of its entries through
//! `Blocks.WOOL.forEach` / `Blocks.CARPET.forEach` rather than by name, so
//! reading the decompiled source alone yields a table missing 32 blocks.
//!
//! # Memory design
//!
//! The 1,196 blocks collapse to a few dozen distinct
//! `(resistance, ignite, burn, lava)` tuples, so the table is a de-duplicated
//! `ENTRIES` array plus a name-sorted `(name, entry index)` array. Lookup is one
//! binary search over `&'static str`s and no allocation; the resistance is
//! stored as raw `f32` bits and rebuilt with [`f32::from_bits`], so nothing is
//! lost to float-literal formatting.
//!
//! # Dependencies
//!
//! None beyond [`crate::generated_block_blast`]. Consumers: `lodestone-server`'s
//! `fire` (spread and burnout) and `explosion_blocks` (blast destruction).

use crate::generated_block_blast as table;

pub use table::BLOCK_COUNT;

/// One block's blast/flammability facts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockBlast {
    /// `Block.getExplosionResistance()`. `0.0` for a block that costs a blast
    /// ray nothing beyond the fixed `0.3` step term; `3600000.0` for bedrock
    /// and friends.
    pub explosion_resistance: f32,
    /// `FireBlock`'s `igniteOdds` for this block — `0` means fire can never
    /// start *on* it (`FireBlock.canBurn` is `igniteOdds > 0`). Values in the
    /// real table are exactly `{0, 5, 15, 30, 60}`.
    pub ignite_odds: u8,
    /// `FireBlock`'s `burnOdds` — how readily `checkBurnOut` consumes the block
    /// itself. Values are exactly `{0, 5, 20, 60, 100}`.
    pub burn_odds: u8,
    /// `BlockState.ignitedByLava()`. Read off the default state; the flag is a
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

/// The blast/flammability facts for `block`, which may be a bare block name or
/// a full canonical state string (the `[...]` suffix is ignored).
///
/// `None` for a name not in the 26.2 block registry. Prefer
/// [`blast_or_inert`] when an unknown name should simply behave like air.
#[must_use]
pub fn blast(block: &str) -> Option<BlockBlast> {
    let name = base_name(block);
    let index = table::BY_NAME
        .binary_search_by_key(&name, |(candidate, _)| *candidate)
        .ok()?;
    let entry = table::BY_NAME[index].1;
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

/// `FireBlock.getIgniteOdds(BlockState)` (`FireBlock.java:220-223`) — the
/// block's ignite odds, **or `0` when the state is `waterlogged=true`**.
///
/// The waterlogged override is the whole reason this exists next to
/// [`blast`]: a waterlogged oak fence must not catch fire, and the block-level
/// table says `5`.
#[must_use]
pub fn ignite_odds_for_state(state: &str) -> u8 {
    if property_of(state, "waterlogged") == Some("true") {
        return 0;
    }
    blast_or_inert(state).ignite_odds
}

/// `FireBlock.getBurnOdds(BlockState)` (`FireBlock.java:213-218`) — the block's
/// burn odds, **or `0` when the state is `waterlogged=true`**.
#[must_use]
pub fn burn_odds_for_state(state: &str) -> u8 {
    if property_of(state, "waterlogged") == Some("true") {
        return 0;
    }
    blast_or_inert(state).burn_odds
}

/// The fluid `explosionResistance` both vanilla fluids report
/// (`WaterFluid`/`LavaFluid` inherit `Fluid`'s `100.0F`), reachable through
/// `FluidState.getExplosionResistance` (`FluidState.java:112-114`).
pub const FLUID_EXPLOSION_RESISTANCE: f32 = 100.0;

/// `ExplosionDamageCalculator.getBlockExplosionResistance`
/// (`ExplosionDamageCalculator.java:11-17`) reduced to a state string:
/// `max(block resistance, fluid resistance)`, where the fluid is water for any
/// `waterlogged=true` state and the block's own for `minecraft:water`/`lava`.
///
/// Returns `None` for air with no fluid — vanilla's `Optional.empty()`, which is
/// the case a blast ray pays **nothing** for, not even the `+0.3` term.
#[must_use]
pub fn explosion_resistance_for_state(state: &str) -> Option<f32> {
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

    /// `Blocks.WOOL.forEach`/`Blocks.CARPET.forEach` are the two entries a
    /// source-only transcription of `FireBlock.bootStrap()` would miss, so pin
    /// one member of each: wool is `(30, 60)`, carpet `(60, 20)`.
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

    /// Air with no fluid is `Optional.empty()`; every other case is a real
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

    /// The sorted-name array really is sorted, which is what makes the binary
    /// search above correct — a generator that emitted registry order would
    /// silently return wrong answers for most names rather than fail.
    #[test]
    fn by_name_is_sorted_and_complete() {
        assert_eq!(table::BY_NAME.len(), BLOCK_COUNT as usize);
        assert!(
            table::BY_NAME.windows(2).all(|w| w[0].0 < w[1].0),
            "BY_NAME must be strictly ascending by name"
        );
    }
}
