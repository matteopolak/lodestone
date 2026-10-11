//! Suspicious sand and gravel: the brushable block entity and its brush state machine.
//!
//! The connection-side half (the held-use loop, the brushing raycast, durability, the item
//! drop) lives in `server::brushing`; the falling-block half in `gravity_tick`. Both are
//! described in `docs/archaeology.md`.

use lodestone_data::block_states::StateId;
use lodestone_model::{BlockFace, ItemStack};

/// Ticks between two brush strokes the block accepts.
pub const BRUSH_COOLDOWN_TICKS: u64 = 10;
/// Ticks without a stroke after which the dust starts to settle back.
pub const BRUSH_RESET_TICKS: u64 = 40;
/// Ticks between two settle steps once the dust has started to settle.
pub const RETRACTION_TICKS: u64 = 4;
/// Strokes that finish a block.
pub const REQUIRED_BRUSHES: u32 = 10;
/// Strokes a settle step takes back.
pub const RETRACTION_STROKES: u32 = 2;

/// The one-byte direction id the update tag carries: down, up, north, south, west, east.
#[must_use]
pub fn legacy_direction_id(face: BlockFace) -> i8 {
    match face {
        BlockFace::Down => 0,
        BlockFace::Up => 1,
        BlockFace::North => 2,
        BlockFace::South => 3,
        BlockFace::West => 4,
        BlockFace::East => 5,
    }
}

/// Inverse of [`legacy_direction_id`].
#[must_use]
pub fn face_from_legacy_id(id: i32) -> Option<BlockFace> {
    Some(match id {
        0 => BlockFace::Down,
        1 => BlockFace::Up,
        2 => BlockFace::North,
        3 => BlockFace::South,
        4 => BlockFace::West,
        5 => BlockFace::East,
        _ => return None,
    })
}

/// A loot table and the seed its roll will use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingLoot {
    /// The loot table id, e.g. `minecraft:archaeology/desert_pyramid`.
    pub table: String,
    /// The roll seed; `0` is saved as absent.
    pub seed: i64,
}

/// What a stroke did to the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stroke {
    /// Inside the cooldown: nothing counted.
    Cooldown,
    /// Counted. `dusted` is the new `dusted` property when it changed.
    Counted {
        /// The new `dusted` value, if the stroke moved it.
        dusted: Option<u8>,
    },
    /// The tenth stroke: the block gives up its item and becomes its base block.
    Finished,
}

/// What a settle check did to the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settle {
    /// The new `dusted` value, if it moved.
    pub dusted: Option<u8>,
    /// Whether another check must be scheduled.
    pub again: bool,
}

/// The block entity of suspicious sand and suspicious gravel.
///
/// Persisted state is the unrolled loot or the rolled item and the hit face; the stroke
/// counters are transient, exactly as they are for the reference.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Brushable {
    loot: Option<PendingLoot>,
    item: Option<ItemStack>,
    hit_direction: Option<BlockFace>,
    brush_count: u32,
    resets_at: u64,
    cooldown_ends_at: u64,
}

impl Brushable {
    /// An entity whose first stroke will roll `table` with `seed`.
    #[must_use]
    pub fn with_loot(table: impl Into<String>, seed: i64) -> Self {
        Self { loot: Some(PendingLoot { table: table.into(), seed }), ..Self::default() }
    }

    /// Restores a saved entity. A saved loot table wins over a saved item, as in the save format.
    #[must_use]
    pub fn restore(loot: Option<PendingLoot>, item: Option<ItemStack>, hit_direction: Option<BlockFace>) -> Self {
        let item = if loot.is_some() { None } else { item };
        Self { loot, item, hit_direction, ..Self::default() }
    }

    /// The loot still to be rolled.
    #[must_use]
    pub fn loot(&self) -> Option<&PendingLoot> {
        self.loot.as_ref()
    }

    /// The rolled item the block holds.
    #[must_use]
    pub fn item(&self) -> Option<&ItemStack> {
        self.item.as_ref()
    }

    /// The face the first stroke of the current brushing hit.
    #[must_use]
    pub fn hit_direction(&self) -> Option<BlockFace> {
        self.hit_direction
    }

    /// The `dusted` property the stroke count maps to.
    #[must_use]
    pub fn completion_state(&self) -> u8 {
        match self.brush_count {
            0 => 0,
            1..=2 => 1,
            3..=5 => 2,
            _ => 3,
        }
    }

    /// One brush stroke at `game_time` on `face`.
    ///
    /// `roll` turns the pending loot into the block's item; it is called once, on the first
    /// stroke past the cooldown, and an empty roll leaves the block without an item.
    pub fn brush(&mut self, game_time: u64, face: BlockFace, roll: impl FnOnce(&PendingLoot) -> Option<ItemStack>) -> Stroke {
        if self.hit_direction.is_none() {
            self.hit_direction = Some(face);
        }
        self.resets_at = game_time + BRUSH_RESET_TICKS;
        if game_time < self.cooldown_ends_at {
            return Stroke::Cooldown;
        }
        self.cooldown_ends_at = game_time + BRUSH_COOLDOWN_TICKS;
        self.unpack(roll);
        let before = self.completion_state();
        self.brush_count += 1;
        if self.brush_count >= REQUIRED_BRUSHES {
            return Stroke::Finished;
        }
        let after = self.completion_state();
        Stroke::Counted { dusted: (before != after).then_some(after) }
    }

    /// Rolls the pending loot, if any, into the item.
    pub fn unpack(&mut self, roll: impl FnOnce(&PendingLoot) -> Option<ItemStack>) {
        if let Some(loot) = self.loot.take() {
            self.item = roll(&loot);
        }
    }

    /// The item a finished block drops, at most `amount` of it, leaving the rest in the block.
    pub fn take_item(&mut self, amount: u32) -> Option<ItemStack> {
        let stack = self.item.as_mut()?;
        let taken = amount.min(stack.count);
        let mut out = stack.clone();
        out.count = taken;
        stack.count -= taken;
        if stack.count == 0 {
            self.item = None;
        }
        (taken > 0).then_some(out)
    }

    /// The scheduled settle check: after [`BRUSH_RESET_TICKS`] without a stroke the dust loses
    /// [`RETRACTION_STROKES`] strokes every [`RETRACTION_TICKS`].
    pub fn settle(&mut self, game_time: u64) -> Settle {
        let mut dusted = None;
        if self.brush_count != 0 && game_time >= self.resets_at {
            let before = self.completion_state();
            self.brush_count = self.brush_count.saturating_sub(RETRACTION_STROKES);
            let after = self.completion_state();
            if before != after {
                dusted = Some(after);
            }
            self.resets_at = game_time + RETRACTION_TICKS;
        }
        if self.brush_count == 0 {
            self.hit_direction = None;
            self.resets_at = 0;
            self.cooldown_ends_at = 0;
            Settle { dusted, again: false }
        } else {
            Settle { dusted, again: true }
        }
    }
}

/// `state` with its `dusted` property set to `dusted`.
#[must_use]
pub fn with_dusted(state: StateId, dusted: u8) -> Option<StateId> {
    let base = state.name();
    StateId::from_state_str(&format!("{base}[dusted={dusted}]"))
}

/// The block a finished brushable turns into.
#[must_use]
pub fn base_block(state: StateId) -> Option<StateId> {
    let name = match state.name() {
        "minecraft:suspicious_sand" => "minecraft:sand",
        "minecraft:suspicious_gravel" => "minecraft:gravel",
        _ => return None,
    };
    StateId::from_state_str(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sherd() -> ItemStack {
        ItemStack::new("minecraft:arms_up_pottery_sherd".parse().unwrap(), 1)
    }

    fn stroke_at(entity: &mut Brushable, time: u64) -> Stroke {
        entity.brush(time, BlockFace::Up, |_| Some(sherd()))
    }

    /// Strokes ten ticks apart: the dust steps 0 -> 1 on the first, 2 on the third, 3 on the
    /// sixth, and the tenth finishes. The thresholds are the counts 1, 3, 6 and 10.
    #[test]
    fn ten_strokes_step_the_dust_one_two_three_then_finish() {
        let mut entity = Brushable::with_loot("minecraft:archaeology/desert_well", 7);
        let mut steps = Vec::new();
        for n in 0..10u64 {
            steps.push(stroke_at(&mut entity, 100 + n * 10));
        }
        let want = [
            Stroke::Counted { dusted: Some(1) },
            Stroke::Counted { dusted: None },
            Stroke::Counted { dusted: Some(2) },
            Stroke::Counted { dusted: None },
            Stroke::Counted { dusted: None },
            Stroke::Counted { dusted: Some(3) },
            Stroke::Counted { dusted: None },
            Stroke::Counted { dusted: None },
            Stroke::Counted { dusted: None },
            Stroke::Finished,
        ];
        assert_eq!(steps, want);
    }

    /// A stroke inside the ten-tick cooldown counts nothing but still pushes the settle
    /// deadline out and records the first face.
    #[test]
    fn a_stroke_inside_the_cooldown_counts_nothing() {
        let mut entity = Brushable::with_loot("t", 1);
        assert_eq!(entity.brush(100, BlockFace::North, |_| None), Stroke::Counted { dusted: Some(1) });
        assert_eq!(entity.brush(109, BlockFace::East, |_| unreachable!("rolled twice")), Stroke::Cooldown);
        assert_eq!(entity.hit_direction(), Some(BlockFace::North), "the first face sticks");
        assert_eq!(entity.brush(110, BlockFace::East, |_| None), Stroke::Counted { dusted: None });
    }

    /// The loot rolls once, on the first counted stroke.
    #[test]
    fn the_loot_rolls_once() {
        let mut entity = Brushable::with_loot("t", 1);
        let mut rolls = 0;
        for n in 0..4u64 {
            entity.brush(100 + n * 10, BlockFace::Up, |_| {
                rolls += 1;
                Some(sherd())
            });
        }
        assert_eq!(rolls, 1);
        assert_eq!(entity.item(), Some(&sherd()));
        assert!(entity.loot().is_none());
    }

    /// Without further strokes the dust holds for 40 ticks, then loses two strokes every four.
    #[test]
    fn the_dust_settles_back_after_forty_idle_ticks() {
        let mut entity = Brushable::with_loot("t", 1);
        for n in 0..7u64 {
            stroke_at(&mut entity, 100 + n * 10);
        }
        // Seven strokes at t = 100..160; the deadline is 160 + 40.
        assert_eq!(entity.completion_state(), 3);
        assert_eq!(entity.settle(199), Settle { dusted: None, again: true });
        // 7 -> 5: state 3 -> 2.
        assert_eq!(entity.settle(200), Settle { dusted: Some(2), again: true });
        assert_eq!(entity.settle(203), Settle { dusted: None, again: true }, "four ticks must pass first");
        // 5 -> 3 keeps state 2; 3 -> 1 reaches state 1; 1 -> 0 reaches state 0 and forgets the face.
        assert_eq!(entity.settle(204), Settle { dusted: None, again: true });
        assert_eq!(entity.settle(208), Settle { dusted: Some(1), again: true });
        assert_eq!(entity.settle(212), Settle { dusted: Some(0), again: false });
        assert_eq!(entity.hit_direction(), None);
    }

    /// Taking the item splits off at most the requested amount.
    #[test]
    fn taking_the_item_leaves_the_remainder() {
        let mut stack = sherd();
        stack.count = 40;
        let mut entity = Brushable::restore(None, Some(stack), Some(BlockFace::Up));
        assert_eq!(entity.take_item(25).map(|s| s.count), Some(25));
        assert_eq!(entity.item().map(|s| s.count), Some(15));
        assert_eq!(entity.take_item(25).map(|s| s.count), Some(15));
        assert!(entity.item().is_none());
        assert!(entity.take_item(5).is_none());
    }

    /// A saved loot table wins over a saved item.
    #[test]
    fn saved_loot_hides_a_saved_item() {
        let entity = Brushable::restore(Some(PendingLoot { table: "t".into(), seed: 3 }), Some(sherd()), None);
        assert!(entity.item().is_none());
        assert!(entity.loot().is_some());
    }
}
