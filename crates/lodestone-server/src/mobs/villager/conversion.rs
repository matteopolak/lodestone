//! Zombie villager curing (issue #247): the conversion timer state machine —
//! `ZombieVillager`'s `startConverting`/`tick`/`getConversionProgress` port
//! (`.cache/mc/26.2/src/net/minecraft/world/entity/monster/zombie/ZombieVillager.java`).
//!
//! # What it is
//!
//! The randomised, block-accelerated countdown between "a golden apple was
//! used on a weakened zombie villager" and "it is a villager again", plus the
//! nearby-block scan (iron bars / beds) that can speed it up. This is the
//! delay vanilla's own issue-body warning names directly: modelling the cure
//! as instant silently drops the whole mechanic (the shaking, the sound cue,
//! the window where the zombie can still be re-hurt) down to a single
//! click-and-done.
//!
//! # How it works
//!
//! [`roll_conversion_ticks`] is `random.nextInt(2401) + 3600` —
//! `VILLAGER_CONVERSION_WAIT_MIN`/`MAX` (3600–6000 ticks, 3–5 minutes) — the
//! starting countdown [`ConversionState`] carries alongside the curing
//! player's uuid (`conversionStarter`, `None` when the offline-mode/no-uuid
//! caller has none, matching this crate's own convention elsewhere).
//!
//! [`conversion_progress`] is `getConversionProgress`: normally `1` tick of
//! progress per game tick, but with a flat 1% roll per call to instead scan
//! nearby iron bars and beds (up to 14 of them, `MAX_SPECIAL_BLOCKS_COUNT`)
//! and roll 30% per block found to add **one more** tick of progress for
//! that block — so a zombie villager cured next to a full jail cell of iron
//! bars converts meaningfully faster on average, without the countdown ever
//! *decreasing*. The nearby-block scan itself
//! ([`count_nearby_special_blocks`]) needs a live [`ChunkWorld`] and is kept
//! separate from the roll so the roll's own arithmetic is testable with no
//! world at all.
//!
//! # How to change it, and the gotchas
//!
//! - **The countdown only ever advances by `conversion_progress`'s return
//!   value, never by a flat `1`.** A caller that subtracts `1` per tick
//!   unconditionally silently drops the iron-bars/bed acceleration —
//!   exactly the "modelled as instant" collapse in the opposite direction
//!   from dropping the delay outright.
//! - **The nearby-block scan is capped at 14 *blocks found*, not 14 cells
//!   scanned** — vanilla's own loop keeps widening `x`/`y`/`z` until either
//!   the whole `8×8×8` neighbourhood is exhausted or 14 qualifying blocks
//!   have been seen, whichever comes first; a caller that stops at the
//!   14th *cell* regardless of what is in it undercounts.
//! - **`conversion_progress`'s two rolls are independent draws** — the 1%
//!   gate and each block's 30% chance are separate `nextFloat()` calls in
//!   vanilla, so a caller sharing one roll between them changes the
//!   distribution.
//! - **This module never flips a zombie villager to a villager itself.**
//!   [`ConversionState::remaining_ticks` reaching `<= 0`] is the signal a
//!   caller (`MobSim`'s tick loop, which owns `SimMob::entity_type` and
//!   `SimMob::gossip`) acts on — see `crate::mobs::MobSim::tick_with_terrain`'s
//!   own conversion-completion arm for what happens next: profession/level/
//!   xp are already generic `SimMob` fields, so they need no special
//!   carry-over code at all; gossip is seeded via
//!   [`super::reputation::apply_reputation_event`] with
//!   [`super::reputation::ReputationEventType::ZombieVillagerCured`].
//!
//! # What is not built, named rather than silent
//!
//! - **No zombie-villager *spawning* is added here** — natural spawn odds
//!   (`Zombie`'s own villager-variant roll on hard difficulty),
//!   `natural_spawn.rs`/`mob_spawn.rs`/`spawn_egg.rs`, are off limits for
//!   this change. `minecraft:zombie_villager` is already a registered
//!   [`lodestone_data`] entity type and [`crate::mobs::MobSim::spawn_species`]
//!   can already produce one generically (as a plain hostile zombie, with no
//!   conversion behaviour) — only the golden-apple/weakness interaction and
//!   the tick-driven timer this file and `MobSim`'s own wiring add are new.
//! - **No initial-profession roll for a naturally-spawned zombie villager**
//!   (`initializeZombieVillagerData`'s random profession pick) — the
//!   `SimMob::profession` a converted villager ends up with is whatever the
//!   zombie villager already carried, and nothing in this crate currently
//!   assigns one at zombie-villager spawn time. A disclosed gap in the same
//!   shape as the one above: this file's job is the conversion timer, not
//!   spawn-time data.

use uuid::Uuid;

use crate::mobs::ChunkWorld;
use lodestone_model::Vec3;

/// `ZombieVillager.VILLAGER_CONVERSION_WAIT_MIN`.
pub const CONVERSION_WAIT_MIN: i32 = 3600;
/// `ZombieVillager.VILLAGER_CONVERSION_WAIT_MAX` — `MIN + 2400`, the
/// `nextInt(2401)` span's inclusive top.
pub const CONVERSION_WAIT_MAX: i32 = 6000;
/// `ZombieVillager.MAX_SPECIAL_BLOCKS_COUNT`.
pub const MAX_SPECIAL_BLOCKS_COUNT: u32 = 14;
/// `ZombieVillager.SPECIAL_BLOCK_RADIUS`.
pub const SPECIAL_BLOCK_RADIUS: i32 = 4;

/// A converting zombie villager's live timer — `villagerConversionTime` plus
/// `conversionStarter`, vanilla's own pair (`ZombieVillager`'s two private
/// fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversionState {
    /// Who used the golden apple, if known — `None` when the curing actor
    /// carries no resolvable identity, mirroring
    /// `PlayerIdentity`'s own "an unidentified player can still act, but
    /// owns nothing" convention elsewhere in this crate.
    pub starter: Option<Uuid>,
    /// Ticks of progress remaining. Conversion completes the tick this
    /// drops to `0` or below.
    pub remaining_ticks: i32,
}

/// `random.nextInt(2401) + 3600` — vanilla's own conversion-time roll.
/// `next_int` takes the same `[0, bound)` contract as
/// `super::gossip::GossipContainer::transfer_from`'s RNG parameter.
#[must_use]
pub fn roll_conversion_ticks(next_int: impl FnOnce(i32) -> i32) -> i32 {
    next_int(2401) + CONVERSION_WAIT_MIN
}

/// Starts a conversion: `ZombieVillager.startConverting`'s timer half (the
/// effect swap — removing Weakness, adding Strength — and the entity-event
/// broadcast are the caller's job, since they touch `SimMob`/`WorldEffect`
/// this pure module has no access to).
#[must_use]
pub fn start_converting(starter: Option<Uuid>, next_int: impl FnOnce(i32) -> i32) -> ConversionState {
    ConversionState {
        starter,
        remaining_ticks: roll_conversion_ticks(next_int),
    }
}

/// `ZombieVillager.getConversionProgress`: normally `1`, occasionally more.
///
/// `next_f32` is **one** shared `nextFloat()`-shaped stream (`[0.0, 1.0)`),
/// used for both the initial 1% gate and every block roll after it — matching
/// vanilla exactly: every draw here is `this.random.nextFloat()` on the
/// entity's own single `RandomSource`, not two independent streams. (An
/// earlier version of this function took two separate `FnMut` closures for
/// "clarity"; that does not compile at the call site once both need to share
/// one real RNG — two closures each capturing the same `&mut` source
/// simultaneously is a double-mutable-borrow. One stream is both the correct
/// port and the only signature that is actually callable.)
///
/// `nearby_special_blocks` is a **lazy** count ([`count_nearby_special_blocks`]'s
/// result, already capped at [`MAX_SPECIAL_BLOCKS_COUNT`]) evaluated only
/// when the 1% gate hits — matching vanilla's own control flow, where the
/// block scan lives *inside* the `if (random.nextFloat() < 0.01F)` branch and
/// so never runs on an ordinary tick. A caller passing an eagerly-scanned
/// count would pay a world scan every tick a conversion is active, ~99% of it
/// for nothing.
#[must_use]
pub fn conversion_progress(
    mut next_f32: impl FnMut() -> f32,
    nearby_special_blocks: impl FnOnce() -> u32,
) -> i32 {
    let mut amount = 1;
    if next_f32() < 0.01 {
        for _ in 0..nearby_special_blocks().min(MAX_SPECIAL_BLOCKS_COUNT) {
            if next_f32() < 0.3 {
                amount += 1;
            }
        }
    }
    amount
}

/// Whether a block id (bare, no namespace/state — the same form
/// [`super::bare_block_id`] produces) is one of `getConversionProgress`'s
/// two accelerants: `minecraft:iron_bars`, or any bed (`BlockStateBase
/// instanceof BedBlock`, ported here as "id ends in `_bed`" — every vanilla
/// bed's registry name has that suffix and nothing else does).
fn is_special_conversion_block(bare_id: &str) -> bool {
    bare_id == "iron_bars" || bare_id.ends_with("_bed")
}

/// `getConversionProgress`'s block scan: counts iron-bars/bed blocks within
/// [`SPECIAL_BLOCK_RADIUS`] of `pos` (an `8×8×8` neighbourhood, vanilla's
/// `(int)x - 4 .. (int)x + 4` on each axis — asymmetric around the floored
/// coordinate, not a `2*radius + 1` cube), stopping early once
/// [`MAX_SPECIAL_BLOCKS_COUNT`] is reached exactly as vanilla's own loop
/// does. Callers roll this only behind [`conversion_progress`]'s own 1%
/// gate — vanilla scans unconditionally *inside* that gate, so calling this
/// every tick regardless would waste work `getConversionProgress` itself
/// never does, though it would not change the final count.
#[must_use]
pub fn count_nearby_special_blocks(world: &ChunkWorld, pos: Vec3) -> u32 {
    let base_x = pos.x.floor() as i32 - SPECIAL_BLOCK_RADIUS;
    let base_y = pos.y.floor() as i32 - SPECIAL_BLOCK_RADIUS;
    let base_z = pos.z.floor() as i32 - SPECIAL_BLOCK_RADIUS;
    let span = 2 * SPECIAL_BLOCK_RADIUS;
    let mut found = 0u32;
    'scan: for dx in 0..span {
        for dy in 0..span {
            for dz in 0..span {
                let state = world.block_state(base_x + dx, base_y + dy, base_z + dz);
                let bare = super::bare_block_id(state);
                if is_special_conversion_block(bare) {
                    found += 1;
                    if found >= MAX_SPECIAL_BLOCKS_COUNT {
                        break 'scan;
                    }
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roll_conversion_ticks_uses_the_predicted_range() {
        assert_eq!(roll_conversion_ticks(|bound| { assert_eq!(bound, 2401); 0 }), 3600);
        assert_eq!(roll_conversion_ticks(|_| 2400), 6000);
    }

    #[test]
    fn start_converting_carries_the_starter_uuid_through() {
        let starter = Uuid::from_bytes([7; 16]);
        let state = start_converting(Some(starter), |_| 0);
        assert_eq!(state.starter, Some(starter));
        assert_eq!(state.remaining_ticks, CONVERSION_WAIT_MIN);
    }

    #[test]
    fn an_unresolved_starter_is_carried_as_none() {
        let state = start_converting(None, |_| 0);
        assert_eq!(state.starter, None);
    }

    /// The common case: the 1% roll misses, so progress is exactly `1`
    /// regardless of how many special blocks are nearby — `nearby_special_blocks`
    /// must never even be evaluated.
    #[test]
    fn progress_is_one_when_the_one_percent_roll_misses() {
        let amount = conversion_progress(
            || 0.5,
            || panic!("nearby_special_blocks must not be evaluated when the roll misses"),
        );
        assert_eq!(amount, 1);
    }

    /// The accelerated case: the first draw clears the 1% gate, and every one
    /// of the (three) nearby blocks' own draw lands under 0.3 — progress must
    /// be base `1` plus one per successful block roll, `1 + 3 = 4`. All four
    /// draws come off the **same** queue, matching the single shared
    /// `nextFloat()` stream `conversion_progress`'s own doc names.
    #[test]
    fn progress_adds_one_per_successful_block_roll() {
        let mut rolls = [0.005_f32, 0.1, 0.29, 0.0].into_iter();
        let amount = conversion_progress(|| rolls.next().unwrap(), || 3);
        assert_eq!(amount, 4);
    }

    /// A block roll landing at or above `0.3` does not add progress — the
    /// boundary case a "less than or equal" off-by-one would get wrong.
    #[test]
    fn a_block_roll_at_or_above_the_threshold_adds_nothing() {
        let mut rolls = [0.0_f32, 0.3, 0.9].into_iter();
        let amount = conversion_progress(|| rolls.next().unwrap(), || 2);
        assert_eq!(amount, 1, "neither block roll (0.3, 0.9) clears the < 0.3 gate");
    }

    /// More than 14 nearby blocks must still only ever roll 14 times — the
    /// cap this module's own doc names.
    #[test]
    fn nearby_special_blocks_is_capped_at_fourteen_rolls() {
        let mut rolls = 0;
        let amount = conversion_progress(
            || {
                rolls += 1;
                0.0
            },
            || 100,
        );
        // One draw for the 1% gate itself, plus one per block roll (capped at 14).
        assert_eq!(rolls, 1 + 14, "must cap at MAX_SPECIAL_BLOCKS_COUNT block rolls, not roll all 100");
        assert_eq!(amount, 1 + 14);
    }

    /// `is_special_conversion_block` must recognise every bed colour, not
    /// just one hardcoded name — every vanilla bed id ends in `_bed`.
    #[test]
    fn every_bed_colour_counts_as_a_special_block() {
        assert!(is_special_conversion_block("red_bed"));
        assert!(is_special_conversion_block("white_bed"));
        assert!(is_special_conversion_block("iron_bars"));
    }

    /// Control: an ordinary block (not iron bars, not a bed) must not
    /// count — otherwise the predicate above would be trivially true and
    /// every one of the tests exercising it would pass for the wrong
    /// reason.
    #[test]
    fn an_unrelated_block_is_not_a_special_conversion_block() {
        assert!(!is_special_conversion_block("stone"));
        assert!(!is_special_conversion_block("bedrock"));
    }
}
