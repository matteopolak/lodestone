//! Composter block entity: the seven-tier fill state machine.
//!
//! # Where the truth comes from
//!
//! The real composter block's own tick and item-insertion rules.
//! There is no separate block-entity type for the composter — the level
//! (`0..=8`) lives directly on the block state, so
//! [`Composter`] here models that
//! block-state integer plus the one piece of transient timing state the
//! real block keeps as a scheduled block tick rather than persisted NBT.
//!
//! ## The state machine
//!
//! * `MIN_LEVEL = 0`, `MAX_LEVEL = 7`, `READY = 8`.
//! * [`insert`](Composter::insert) is only ever called with
//!   `fillLevel < 7` (both call sites — the right-click insert path,
//!   and the shared insert path hoppers/
//!   dispensers use — guard on `fillLevel < 7` before calling
//!   it), and the real container lookup hands out an empty, insert-rejecting
//!   container for level `7`, so an insert at level `7` *or* `8` is
//!   always a no-op. This is [`Composter::insert`]'s `level >= MAX_FILL_LEVEL`
//!   guard.
//! * The per-item chance roll: look up the item's compost chance; if the
//!   fill level is nonzero, or that chance is not positive, and a random
//!   draw does not fall under the chance, nothing changes.
//!   Every compostable item has `chance > 0.0` (a non-compostable item
//!   never reaches this code at all — both insertion paths gate on the
//!   compost table containing the item first), so at `fillLevel == 0` the left
//!   disjunct `fillLevel != 0` is false and `chance > 0.0F` is always true,
//!   making the whole `&&` condition false: **the first item into an empty
//!   composter always raises the level, regardless of its own chance value**.
//!   At any other level the roll is the ordinary `roll < chance` test. Either
//!   way the item is consumed whether or not the
//!   level actually increased — a failed roll is not a rejected insert.
//! * Reaching level `7` schedules a tick **20 game ticks** later, and the tick
//!   handler unconditionally advances `7 -> 8` — deterministic, no further roll.
//! * Extraction (reached via the empty-hand-use path
//!   when `fillLevel == 8`) resets the level to `0` and yields
//!   exactly one `minecraft:bone_meal`.
//!
//! ## What this module does not model
//!
//! Item consumption from the *caller's* stack (the composter itself never
//! holds the inserted item — it is shrunk by the caller and only the fill
//! level state carries over) and the world-facing side effects
//! (level events/particles/sound) are for a wiring
//! layer to add; [`Composter`] is the pure value type, matching this crate's
//! established shape for tick-driven mechanics (see
//! [`crate::vitals::PlayerVitals`], [`crate::fall::FallTracker`]).

/// The real ready-level constant: the level at
/// which the composter holds bone meal instead of accepting compost.
pub const READY_LEVEL: u8 = 8;

/// The real max-level constant: the
/// highest level an insert can still land on (`7` itself never accepts an
/// insert — see [`Composter::insert`]'s doc comment).
pub const MAX_FILL_LEVEL: u8 = 7;

/// The fixed delay between reaching level `7` and flipping to [`READY_LEVEL`]
/// (the real scheduled-tick delay set on reaching level 7).
pub const READY_DELAY_TICKS: u8 = 20;

/// The outcome of one [`Composter::insert`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    /// `item` is not in the real compost table at all — the caller's
    /// stack must not be touched (mirrors both insertion paths'
    /// "is this item compostable" guard rejecting the interaction outright).
    NotCompostable,
    /// The composter is at level `7` (full, waiting on the scheduled tick)
    /// or [`READY_LEVEL`] (holding bone meal) — no insert is possible until
    /// it advances or is emptied. The caller's stack must not be touched
    /// (this is the "genuinely no-op" case, not a failed roll).
    NotAccepting,
    /// The item was compostable and the composter was accepting inserts —
    /// the caller must shrink its stack by exactly one regardless of
    /// `level_increased` (the real rule consumes on every accepted insert,
    /// roll outcome notwithstanding).
    Consumed {
        /// Whether the fill level actually advanced this call.
        level_increased: bool,
    },
}

/// A composter's fill-level state machine plus the one piece of transient
/// timing state the real block schedules as a block tick (see the module
/// doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Composter {
    level: u8,
    /// `Some(n)` while counting down the `20`-tick delay after reaching
    /// level `7`; `n` is ticks remaining *after* the current call.
    ticks_until_ready: Option<u8>,
}

impl Composter {
    /// A freshly placed, empty composter (level `0`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds a composter from persisted state — see
    /// [`crate::furnace::Furnace::restore`] for why this is one total
    /// constructor rather than a setter per field.
    ///
    /// **The real composter has no block entity at all**: its fill level is a
    /// block-state property (`minecraft:composter[level=0..8]`) and the
    /// 20-tick ready delay is a scheduled block tick. This crate models it as
    /// a block entity instead (`crate::block_entities`), so
    /// [`ticks_until_ready`](Self::ticks_until_ready) has no real field to
    /// live in and [`crate::chunk_nbt`] writes the whole thing under a
    /// namespaced id. See that module for what a real server does
    /// with it.
    #[must_use]
    pub fn restore(level: u8, ticks_until_ready: Option<u8>) -> Self {
        Self {
            level,
            ticks_until_ready,
        }
    }

    /// Ticks left on the level-7 → level-8 delay, for persistence. `None`
    /// when no delay is running.
    #[must_use]
    pub fn ticks_until_ready(&self) -> Option<u8> {
        self.ticks_until_ready
    }

    /// The current fill level, `0..=8`.
    #[must_use]
    pub fn level(&self) -> u8 {
        self.level
    }

    /// Whether the composter currently holds bone meal (level `8`).
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.level == READY_LEVEL
    }

    /// Attempts to insert one `item` (a full `minecraft:...` id, matching
    /// [`lodestone_model::ItemStack::item`]'s `Display`), given `roll`, an
    /// injected `[0.0, 1.0)` uniform sample standing in for the real
    /// random draw — the same "caller supplies the
    /// randomness" shape [`crate::vitals::PlayerVitals::tick`] and
    /// [`crate::fall::FallTracker`] already use, so a test can pin an exact
    /// outcome rather than looping until one appears.
    ///
    /// See [`InsertOutcome`] for what each variant means and the module doc
    /// comment for the exact real formula this implements.
    pub fn insert(&mut self, item: &str, roll: f64) -> InsertOutcome {
        if self.level >= MAX_FILL_LEVEL {
            return InsertOutcome::NotAccepting;
        }
        let Some(chance) = compostable_chance(item) else {
            return InsertOutcome::NotCompostable;
        };

        // `(fillLevel != 0 || !(chance > 0.0F)) && !(roll < chance)` bails
        // out unchanged; every compostable chance is > 0.0, so at level 0
        // the left side is always false and the roll is skipped entirely.
        let level_increased = self.level == 0 || roll < f64::from(chance);
        if level_increased {
            self.level += 1;
            if self.level == MAX_FILL_LEVEL {
                self.ticks_until_ready = Some(READY_DELAY_TICKS);
            }
        }
        InsertOutcome::Consumed { level_increased }
    }

    /// Advances by exactly one server tick. Returns `true` on the tick the
    /// composter flips from `7` to [`READY_LEVEL`] (exactly
    /// [`READY_DELAY_TICKS`] calls after the insert that reached level `7`),
    /// `false` otherwise — including every tick before and after that one.
    pub fn tick(&mut self) -> bool {
        match self.ticks_until_ready {
            Some(0) | None => false,
            Some(1) => {
                self.level = READY_LEVEL;
                self.ticks_until_ready = None;
                true
            }
            Some(n) => {
                self.ticks_until_ready = Some(n - 1);
                false
            }
        }
    }

    /// Extracts the bone meal, resetting to level `0`. Returns `true` if
    /// bone meal was actually produced (level was [`READY_LEVEL`]), `false`
    /// as a no-op otherwise (mirrors the real empty-hand-use rule's
    /// `fillLevel == 8` guard).
    pub fn extract(&mut self) -> bool {
        if self.level == READY_LEVEL {
            self.level = 0;
            true
        } else {
            false
        }
    }
}

/// The per-item compost chance, restated from the real compost-table
/// bootstrap — every registered chance/item pair,
/// lowered to its registry id. `None` for anything not in the real
/// compost table (which defaults every unlisted item to `-1.0`,
/// i.e. "rejected").
#[must_use]
pub fn compostable_chance(item: &str) -> Option<f32> {
    Some(match item {
        "minecraft:jungle_leaves"
        | "minecraft:oak_leaves"
        | "minecraft:spruce_leaves"
        | "minecraft:dark_oak_leaves"
        | "minecraft:pale_oak_leaves"
        | "minecraft:acacia_leaves"
        | "minecraft:cherry_leaves"
        | "minecraft:birch_leaves"
        | "minecraft:azalea_leaves"
        | "minecraft:mangrove_leaves"
        | "minecraft:oak_sapling"
        | "minecraft:spruce_sapling"
        | "minecraft:birch_sapling"
        | "minecraft:jungle_sapling"
        | "minecraft:acacia_sapling"
        | "minecraft:cherry_sapling"
        | "minecraft:dark_oak_sapling"
        | "minecraft:pale_oak_sapling"
        | "minecraft:mangrove_propagule"
        | "minecraft:beetroot_seeds"
        | "minecraft:dried_kelp"
        | "minecraft:short_grass"
        | "minecraft:kelp"
        | "minecraft:melon_seeds"
        | "minecraft:pumpkin_seeds"
        | "minecraft:seagrass"
        | "minecraft:sweet_berries"
        | "minecraft:glow_berries"
        | "minecraft:wheat_seeds"
        | "minecraft:moss_carpet"
        | "minecraft:pale_moss_carpet"
        | "minecraft:pale_hanging_moss"
        | "minecraft:pink_petals"
        | "minecraft:wildflowers"
        | "minecraft:leaf_litter"
        | "minecraft:small_dripleaf"
        | "minecraft:hanging_roots"
        | "minecraft:mangrove_roots"
        | "minecraft:torchflower_seeds"
        | "minecraft:pitcher_pod"
        | "minecraft:firefly_bush"
        | "minecraft:bush"
        | "minecraft:cactus_flower"
        | "minecraft:dry_short_grass"
        | "minecraft:dry_tall_grass" => 0.3,

        "minecraft:dried_kelp_block"
        | "minecraft:tall_grass"
        | "minecraft:flowering_azalea_leaves"
        | "minecraft:cactus"
        | "minecraft:sugar_cane"
        | "minecraft:vine"
        | "minecraft:nether_sprouts"
        | "minecraft:weeping_vines"
        | "minecraft:twisting_vines"
        | "minecraft:melon_slice"
        | "minecraft:glow_lichen" => 0.5,

        "minecraft:sea_pickle"
        | "minecraft:lily_pad"
        | "minecraft:pumpkin"
        | "minecraft:carved_pumpkin"
        | "minecraft:melon"
        | "minecraft:apple"
        | "minecraft:beetroot"
        | "minecraft:carrot"
        | "minecraft:cocoa_beans"
        | "minecraft:potato"
        | "minecraft:wheat"
        | "minecraft:brown_mushroom"
        | "minecraft:red_mushroom"
        | "minecraft:mushroom_stem"
        | "minecraft:crimson_fungus"
        | "minecraft:warped_fungus"
        | "minecraft:nether_wart"
        | "minecraft:crimson_roots"
        | "minecraft:warped_roots"
        | "minecraft:shroomlight"
        | "minecraft:dandelion"
        | "minecraft:poppy"
        | "minecraft:blue_orchid"
        | "minecraft:allium"
        | "minecraft:azure_bluet"
        | "minecraft:red_tulip"
        | "minecraft:orange_tulip"
        | "minecraft:white_tulip"
        | "minecraft:pink_tulip"
        | "minecraft:oxeye_daisy"
        | "minecraft:cornflower"
        | "minecraft:lily_of_the_valley"
        | "minecraft:wither_rose"
        | "minecraft:open_eyeblossom"
        | "minecraft:closed_eyeblossom"
        | "minecraft:fern"
        | "minecraft:sunflower"
        | "minecraft:lilac"
        | "minecraft:rose_bush"
        | "minecraft:peony"
        | "minecraft:large_fern"
        | "minecraft:spore_blossom"
        | "minecraft:azalea"
        | "minecraft:moss_block"
        | "minecraft:pale_moss_block"
        | "minecraft:big_dripleaf" => 0.65,

        "minecraft:hay_block"
        | "minecraft:brown_mushroom_block"
        | "minecraft:red_mushroom_block"
        | "minecraft:nether_wart_block"
        | "minecraft:warped_wart_block"
        | "minecraft:flowering_azalea"
        | "minecraft:bread"
        | "minecraft:baked_potato"
        | "minecraft:cookie"
        | "minecraft:torchflower"
        | "minecraft:pitcher_plant" => 0.85,

        "minecraft:cake" | "minecraft:pumpkin_pie" => 1.0,

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_composter_is_level_zero_and_not_ready() {
        let c = Composter::new();
        assert_eq!(c.level(), 0);
        assert!(!c.is_ready());
    }

    #[test]
    fn unknown_item_is_rejected_without_state_change() {
        let mut c = Composter::new();
        let outcome = c.insert("minecraft:stone", 0.0);
        assert_eq!(outcome, InsertOutcome::NotCompostable);
        assert_eq!(c.level(), 0);
    }

    /// The documented special case: level 0 always advances regardless of
    /// the item's own chance and regardless of an adversarial roll (`1.0`,
    /// which would fail almost every other item's chance check).
    #[test]
    fn first_item_into_empty_composter_always_advances() {
        let mut c = Composter::new();
        // wither_rose has chance 0.65; roll 0.999999 would fail an ordinary
        // roll check but must still succeed at level 0.
        let outcome = c.insert("minecraft:wither_rose", 0.999_999);
        assert_eq!(
            outcome,
            InsertOutcome::Consumed {
                level_increased: true
            }
        );
        assert_eq!(c.level(), 1);
    }

    /// **Control**: away from level 0, the same item and the same
    /// adversarial roll must now fail the level-up (proving the level-0
    /// special case is real, not just an always-succeed bug in `insert`).
    #[test]
    fn roll_failure_away_from_level_zero_leaves_level_unchanged() {
        let mut c = Composter::new();
        c.insert("minecraft:wither_rose", 0.0); // -> level 1, guaranteed
        assert_eq!(c.level(), 1);

        // wither_rose chance is 0.65; a roll of 0.99 must fail.
        let outcome = c.insert("minecraft:wither_rose", 0.99);
        assert_eq!(
            outcome,
            InsertOutcome::Consumed {
                level_increased: false
            },
            "item must still be consumed even on a failed roll"
        );
        assert_eq!(c.level(), 1, "level must not advance on a failed roll");
    }

    #[test]
    fn roll_just_under_chance_succeeds_and_just_over_fails() {
        let mut c = Composter::new();
        c.insert("minecraft:jungle_leaves", 0.0); // -> level 1 (chance 0.3)
        assert_eq!(c.level(), 1);

        let mut succeed = c;
        assert_eq!(
            succeed.insert("minecraft:jungle_leaves", 0.299_999),
            InsertOutcome::Consumed {
                level_increased: true
            }
        );
        assert_eq!(succeed.level(), 2);

        let mut fail = c;
        // `chance` is stored as an `f32` (`0.3`) and widened to `f64` for the
        // comparison, exactly like Java's implicit `float` -> `double`
        // promotion in `random.nextDouble() < chance` — the widened value is
        // `0.300000011920929...`, slightly *above* the literal `f64` `0.3`,
        // so the roll here must clear that widened value, not the bare
        // decimal, to land on the "fails" side.
        assert_eq!(
            fail.insert("minecraft:jungle_leaves", 0.300_001),
            InsertOutcome::Consumed {
                level_increased: false
            }
        );
        assert_eq!(fail.level(), 1);
    }

    /// Drives a composter to level 7 with guaranteed rolls (`roll = 0.0`
    /// always beats any positive chance), then asserts the exact 20-tick
    /// schedule to [`READY_LEVEL`] — not "eventually", the precise tick.
    #[test]
    fn reaches_ready_at_exactly_tick_20_after_hitting_level_seven() {
        let mut c = Composter::new();
        for _ in 0..7 {
            let outcome = c.insert("minecraft:cake", 0.0); // chance 1.0
            assert_eq!(
                outcome,
                InsertOutcome::Consumed {
                    level_increased: true
                }
            );
        }
        assert_eq!(c.level(), MAX_FILL_LEVEL);
        assert!(!c.is_ready());

        for t in 1..READY_DELAY_TICKS {
            let became_ready = c.tick();
            assert!(!became_ready, "became ready early at tick {t}");
            assert_eq!(c.level(), MAX_FILL_LEVEL, "level moved early at tick {t}");
        }

        let became_ready = c.tick();
        assert!(became_ready, "expected exactly tick {READY_DELAY_TICKS} to flip to ready");
        assert!(c.is_ready());
        assert_eq!(c.level(), READY_LEVEL);
    }

    /// **Control**: once at level 7, further inserts (even guaranteed-roll
    /// ones) must be flatly rejected and must not disturb the ready
    /// countdown — proves `NotAccepting` really gates, not just happens to
    /// coincide with "nothing left to increase".
    #[test]
    fn level_seven_rejects_inserts_and_ready_schedule_is_undisturbed() {
        let mut c = Composter::new();
        for _ in 0..7 {
            c.insert("minecraft:cake", 0.0);
        }
        assert_eq!(c.level(), MAX_FILL_LEVEL);

        for _ in 0..5 {
            let outcome = c.insert("minecraft:cake", 0.0);
            assert_eq!(outcome, InsertOutcome::NotAccepting);
        }

        // The ready countdown must still fire at exactly tick 20, unaffected
        // by the rejected inserts above.
        for _ in 0..READY_DELAY_TICKS - 1 {
            assert!(!c.tick());
        }
        assert!(c.tick());
        assert!(c.is_ready());
    }

    #[test]
    fn ready_composter_rejects_inserts_until_extracted() {
        let mut c = Composter::new();
        for _ in 0..7 {
            c.insert("minecraft:cake", 0.0);
        }
        for _ in 0..READY_DELAY_TICKS {
            c.tick();
        }
        assert!(c.is_ready());

        assert_eq!(
            c.insert("minecraft:cake", 0.0),
            InsertOutcome::NotAccepting,
            "a ready composter must reject inserts until emptied"
        );
    }

    #[test]
    fn extract_resets_ready_composter_to_zero() {
        let mut c = Composter::new();
        for _ in 0..7 {
            c.insert("minecraft:cake", 0.0);
        }
        for _ in 0..READY_DELAY_TICKS {
            c.tick();
        }
        assert!(c.extract());
        assert_eq!(c.level(), 0);
        assert!(!c.is_ready());
    }

    /// **Control**: extracting a non-ready composter must be a no-op, not
    /// silently "succeed" at some other level.
    #[test]
    fn extract_before_ready_is_a_no_op() {
        let mut c = Composter::new();
        c.insert("minecraft:cake", 0.0);
        assert_eq!(c.level(), 1);
        assert!(!c.extract());
        assert_eq!(c.level(), 1);
    }

    #[test]
    fn every_representative_tier_item_has_the_documented_chance() {
        let cases: &[(&str, f32)] = &[
            ("minecraft:wheat_seeds", 0.3),
            ("minecraft:sugar_cane", 0.5),
            ("minecraft:wheat", 0.65),
            ("minecraft:hay_block", 0.85),
            ("minecraft:cake", 1.0),
            ("minecraft:pumpkin_pie", 1.0),
        ];
        for &(item, chance) in cases {
            assert_eq!(compostable_chance(item), Some(chance), "item: {item}");
        }
        assert_eq!(compostable_chance("minecraft:diamond"), None);
    }
}
