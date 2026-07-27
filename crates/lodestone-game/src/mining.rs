//! Block breaking (mining) and the item-pickup / block-destruction feedback
//! that accompanies it.
//!
//! This module is the version-free *game* half of "the player breaks a block":
//!
//! * [`BreakInputs`] + [`BreakInputs::progress_per_tick`] is the **bit-exact
//!   vanilla break-time formula**. It is a pure function of injected inputs —
//!   block hardness, the selected item's mining speed, the correct-tool flag and
//!   the player's current dig modifiers — the way [`crate`]'s physics uses
//!   `CollisionView`: the state machine never hard-codes a hardness or tool-speed
//!   table (which is per-version *data* and exactly the "mint your own fixtures"
//!   trap). Whoever drives this feeds real values from the world/registry.
//! * [`Mining`] is the **dig state machine**: start → accumulate progress per
//!   tick → finish (or abort), mirroring 26.2's client `MultiPlayerGameMode`
//!   (`startDestroyBlock`/`continueDestroyBlock`/`stopDestroyBlock`). It emits the
//!   [`ClientAction`]s a real client sends — `BlockAction` (start/abort/stop)
//!   plus the arm [`ClientAction::SwingArm`] — and predicts completion with
//!   vanilla's `>= 1.0` accumulator rule, including the single-tick instant-break
//!   case.
//! * [`BlockDestructionOverlays`] folds the clientbound
//!   [`ClientEvent::BlockDestruction`] crack-overlay stages for **other** players
//!   into consumable state (rendering the crack is someone else's job).
//! * [`PickupFeed`] folds [`ClientEvent::ItemPickup`] into a transient
//!   pickup-animation signal. **It is deliberately not an inventory.** The actual
//!   inventory delta from a pickup arrives through `set_player_inventory` /
//!   `container_set_content`, which [`crate::menus`] already folds; duplicating it
//!   here would be a second, silently-diverging fold of the same state.
//!
//! # Behavioural reference, not a copy
//!
//! The formulae and sequencing below were derived from the decompiled 26.2
//! client/server *as a behavioural reference only* and re-implemented from
//! scratch; equivalence is proven by the hermetic golden tests in this file and
//! the live dig gate (`tests/live_mining.rs`), not by transliteration.

use lodestone_model::math::BlockPos;
use lodestone_model::{BlockActionKind, BlockFace, ClientAction, ClientEvent, Hand};

use crate::item::ItemStack;

/// Everything the vanilla break-time formula needs about the block being mined
/// and the player mining it. Injected by the driver from real world/registry
/// data — this crate holds no hardness or tool table of its own.
///
/// The defaults describe an empty-handed player standing on the ground in air,
/// so a test or caller only has to set the fields that differ from that.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BreakInputs {
    /// The block's hardness (`BlockState.getDestroySpeed`). `-1.0` marks an
    /// unbreakable block (bedrock, barrier); `0.0` is an instant-break block.
    pub hardness: f32,
    /// Whether the target is air. Air is never broken and never accumulates
    /// progress; it only affects the state machine, not the raw formula.
    pub is_air: bool,
    /// Whether the selected item is the correct tool for drops
    /// (`Player.hasCorrectToolForDrops`). Selects vanilla's `30` vs `100`
    /// speed divider.
    pub correct_tool: bool,
    /// The selected item's mining speed *for this block*
    /// (`ItemStack.getDestroySpeed`; the `minecraft:tool` component's matching
    /// rule speed, or `1.0` bare-handed / for a non-matching material).
    pub tool_speed: f32,
    /// The `minecraft:mining_efficiency` attribute contribution (Efficiency
    /// enchantment: level `L` contributes `L*L + 1`, see
    /// [`efficiency_bonus`]). Applied only when `tool_speed > 1.0`, matching
    /// vanilla. Zero when unenchanted.
    pub mining_efficiency: f32,
    /// Effective dig-speed amplifier from Haste / Conduit Power
    /// (`MobEffectUtil.getDigSpeedAmplification`), or `None` when neither is
    /// active. `Some(0)` is Haste I.
    pub haste_amplifier: Option<u32>,
    /// Mining Fatigue amplifier, or `None` when the effect is absent.
    /// `Some(0)` is Mining Fatigue I.
    pub mining_fatigue: Option<u32>,
    /// The `minecraft:block_break_speed` attribute (default `1.0`).
    pub block_break_speed: f32,
    /// Whether the player's eyes are in water (`isEyeInFluid(WATER)`).
    pub submerged: bool,
    /// The `minecraft:submerged_mining_speed` attribute, applied only when
    /// [`submerged`](Self::submerged). Vanilla default is `0.2` (the classic
    /// "5× slower underwater"); Aqua Affinity raises it to `1.0`.
    pub submerged_mining_speed: f32,
    /// Whether the player is on the ground. Off-ground mining is 5× slower.
    pub on_ground: bool,
}

impl Default for BreakInputs {
    fn default() -> Self {
        Self {
            hardness: 0.0,
            is_air: false,
            correct_tool: false,
            tool_speed: 1.0,
            mining_efficiency: 0.0,
            haste_amplifier: None,
            mining_fatigue: None,
            block_break_speed: 1.0,
            submerged: false,
            submerged_mining_speed: 0.2,
            on_ground: true,
        }
    }
}

/// The Efficiency-enchantment contribution to the `mining_efficiency` attribute:
/// `level*level + 1` for `level >= 1`, else `0`. Provided as a convenience for
/// callers/tests; the formula itself takes the resolved attribute value so this
/// crate stays free of enchantment data.
#[must_use]
pub fn efficiency_bonus(level: u32) -> f32 {
    if level == 0 {
        0.0
    } else {
        (level * level) as f32 + 1.0
    }
}

impl BreakInputs {
    /// The player's effective dig speed for this block
    /// (`Player.getDestroySpeed`), before the hardness/divider step. Kept as a
    /// separate step so it can be asserted independently.
    ///
    /// The operation order is load-bearing and matches vanilla exactly: the
    /// Efficiency bonus is added only to a tool already faster than the hand,
    /// then Haste multiplies, then Mining Fatigue, then the break-speed
    /// attribute, then the submerged factor, then the off-ground division.
    #[must_use]
    pub fn dig_speed(&self) -> f32 {
        let mut speed = self.tool_speed;
        if speed > 1.0 {
            speed += self.mining_efficiency;
        }
        if let Some(amp) = self.haste_amplifier {
            speed *= 1.0 + (amp as f32 + 1.0) * 0.2;
        }
        if let Some(amp) = self.mining_fatigue {
            let scale = match amp {
                0 => 0.3,
                1 => 0.09,
                2 => 0.0027,
                _ => 8.1e-4,
            };
            speed *= scale;
        }
        speed *= self.block_break_speed;
        if self.submerged {
            speed *= self.submerged_mining_speed;
        }
        if !self.on_ground {
            speed /= 5.0;
        }
        speed
    }

    /// Progress accumulated per tick while mining this block
    /// (`BlockBehaviour.getDestroyProgress`): `dig_speed / hardness / divider`,
    /// where the divider is `30` with the correct tool and `100` without.
    ///
    /// An unbreakable block (`hardness == -1.0`) yields `0.0`. A zero-hardness
    /// block yields `+inf`, which the state machine reads as an instant break —
    /// this matches vanilla, where such blocks satisfy `getDestroyProgress >= 1.0`
    /// on the first tick.
    #[must_use]
    pub fn progress_per_tick(&self) -> f32 {
        if self.hardness == -1.0 {
            return 0.0;
        }
        let divider = if self.correct_tool { 30.0 } else { 100.0 };
        self.dig_speed() / self.hardness / divider
    }

    /// Number of *mining ticks* until the block breaks, computed by replaying
    /// vanilla's accumulate-then-compare loop rather than a closed-form division
    /// (so f32 rounding matches the client exactly).
    ///
    /// * `Some(0)` — instant break: `progress_per_tick >= 1.0`, so the block
    ///   breaks on the `START_DESTROY_BLOCK` tick with no accumulation.
    /// * `Some(n)` — `n` `continueDestroyBlock` ticks after the start tick.
    /// * `None` — the block never breaks (unbreakable or non-positive speed).
    #[must_use]
    pub fn ticks_to_break(&self) -> Option<u32> {
        let per = self.progress_per_tick();
        if per.is_nan() || per <= 0.0 {
            return None;
        }
        if per >= 1.0 {
            return Some(0);
        }
        let mut progress = 0.0f32;
        let mut ticks = 0u32;
        while progress < 1.0 {
            progress += per;
            ticks += 1;
            // Guard against a pathologically tiny speed spinning forever; a real
            // break is always well under this bound.
            if ticks >= 1_000_000 {
                break;
            }
        }
        Some(ticks)
    }
}

/// The in-progress dig, when the player is holding the attack button on a block.
#[derive(Debug, Clone)]
struct Active {
    target: BlockPos,
    progress: f32,
    /// The item held when this dig began. Vanilla's `sameDestroyTarget` requires
    /// the held item to be unchanged, so swapping tools mid-dig restarts.
    tool: Option<ItemStack>,
}

/// The version-free dig state machine.
///
/// Drive it from the input loop: [`start`](Self::start) on the tick the attack
/// button goes down, [`continue_`](Self::continue_) every tick it stays down on
/// the same block, and [`stop`](Self::stop) when it releases. Each call returns
/// the [`ClientAction`]s to send this tick, in order. The machine owns the
/// block-prediction `sequence` counter the modern protocol requires.
#[derive(Debug, Default)]
pub struct Mining {
    state: Option<Active>,
    /// Post-break cooldown (`destroyDelay`): after a block breaks, vanilla
    /// ignores dig input for 5 ticks so a held button does not instantly chew
    /// through the block behind it.
    delay: i32,
    /// Monotonic block-change prediction sequence. `START`/`STOP` carry a fresh
    /// value the server echoes when it acks or rolls back; `ABORT` carries `0`,
    /// matching vanilla's 3-argument packet constructor.
    next_sequence: i32,
}

impl Mining {
    /// A fresh machine with nothing being mined.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a block is currently being mined.
    #[must_use]
    pub fn is_destroying(&self) -> bool {
        self.state.is_some()
    }

    /// The block currently being mined, if any.
    #[must_use]
    pub fn target(&self) -> Option<BlockPos> {
        self.state.as_ref().map(|a| a.target)
    }

    /// Accumulated progress in `0.0..=1.0` (0 when idle).
    #[must_use]
    pub fn progress(&self) -> f32 {
        self.state.as_ref().map_or(0.0, |a| a.progress)
    }

    /// The crack-overlay stage of the player's own dig
    /// (`MultiPlayerGameMode.getDestroyStage`): `(progress * 10)` truncated while
    /// mining, or `-1` when idle.
    #[must_use]
    pub fn destroy_stage(&self) -> i32 {
        let p = self.progress();
        if p > 0.0 { (p * 10.0) as i32 } else { -1 }
    }

    fn take_sequence(&mut self) -> i32 {
        // Vanilla pre-increments, so the first prediction is sequence 1.
        self.next_sequence += 1;
        self.next_sequence
    }

    fn same_target(&self, pos: BlockPos, tool: &Option<ItemStack>) -> bool {
        match &self.state {
            Some(a) => a.target == pos && same_item(&a.tool, tool),
            None => false,
        }
    }

    /// Begin (or retarget) a dig, mirroring `startDestroyBlock`'s survival path.
    ///
    /// Sends `START_DESTROY_BLOCK` and swings the arm. If the target changed
    /// while another dig was live, an `ABORT` for the old target is sent first.
    /// A block that satisfies the instant-break condition
    /// (`progress_per_tick >= 1.0`) breaks on this tick and leaves no live dig.
    ///
    /// `tool` is the currently selected stack (used for the same-target check);
    /// pass `None` for an empty hand.
    pub fn start(
        &mut self,
        pos: BlockPos,
        face: BlockFace,
        inputs: &BreakInputs,
        tool: Option<ItemStack>,
    ) -> Vec<ClientAction> {
        let mut out = Vec::new();
        if self.state.is_none() || !self.same_target(pos, &tool) {
            if let Some(old) = &self.state {
                out.push(block_action(BlockActionKind::AbortDestroy, old.target, face, 0));
            }
            let seq = self.take_sequence();
            if !inputs.is_air && inputs.progress_per_tick() >= 1.0 {
                // Instant break: the server breaks the block on START, so no
                // live dig is retained and no STOP is ever sent.
                self.state = None;
                out.push(block_action(BlockActionKind::StartDestroy, pos, face, seq));
            } else {
                self.state = Some(Active {
                    target: pos,
                    progress: 0.0,
                    tool,
                });
                out.push(block_action(BlockActionKind::StartDestroy, pos, face, seq));
            }
        }
        // `startAttack` swings the arm unconditionally after the block branch.
        out.push(swing());
        out
    }

    /// Advance a held dig one tick, mirroring `continueDestroyBlock`.
    ///
    /// Accumulates `progress_per_tick` and, on reaching `>= 1.0`, sends
    /// `STOP_DESTROY_BLOCK` and starts the 5-tick post-break cooldown. During the
    /// cooldown the arm still swings but no block action is sent (vanilla returns
    /// `true`, so `continueAttack` swings). Retargeting to a different block
    /// delegates to [`start`](Self::start); mining air cancels silently with no
    /// swing.
    pub fn continue_(
        &mut self,
        pos: BlockPos,
        face: BlockFace,
        inputs: &BreakInputs,
        tool: Option<ItemStack>,
    ) -> Vec<ClientAction> {
        if self.delay > 0 {
            self.delay -= 1;
            // Cooldown tick: vanilla returns true here, so the arm swings.
            return vec![swing()];
        }
        if self.same_target(pos, &tool) {
            if inputs.is_air {
                // The block already broke/vanished: cancel with no swing
                // (vanilla returns false).
                self.state = None;
                return Vec::new();
            }
            let per = inputs.progress_per_tick();
            let mut out = Vec::new();
            let finished = {
                let active = self.state.as_mut().expect("same_target implies Some");
                active.progress += per;
                active.progress >= 1.0
            };
            if finished {
                let seq = self.take_sequence();
                // STOP carries the block being finished and the face the input
                // loop is aiming at (vanilla passes `continueDestroyBlock`'s
                // `direction`).
                out.push(block_action(BlockActionKind::StopDestroy, pos, face, seq));
                self.state = None;
                self.delay = 5;
            }
            out.push(swing());
            out
        } else {
            self.start(pos, face, inputs, tool)
        }
    }

    /// Release the dig, mirroring `stopDestroyBlock`.
    ///
    /// Sends `ABORT_DESTROY_BLOCK` for the live target (vanilla uses
    /// `Direction.DOWN` for this abort) and clears progress. No arm swing. A
    /// no-op when nothing is being mined.
    pub fn stop(&mut self) -> Vec<ClientAction> {
        if let Some(active) = self.state.take() {
            vec![block_action(
                BlockActionKind::AbortDestroy,
                active.target,
                BlockFace::Down,
                0,
            )]
        } else {
            Vec::new()
        }
    }
}

fn same_item(a: &Option<ItemStack>, b: &Option<ItemStack>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => ItemStack::is_same_item_same_components(x, y),
        _ => false,
    }
}

fn block_action(action: BlockActionKind, pos: BlockPos, face: BlockFace, sequence: i32) -> ClientAction {
    ClientAction::BlockAction {
        action,
        pos,
        face,
        sequence,
    }
}

fn swing() -> ClientAction {
    ClientAction::SwingArm { hand: Hand::Main }
}

/// Crack-overlay stages for blocks *other* players are breaking, folded from
/// clientbound [`ClientEvent::BlockDestruction`].
///
/// Vanilla shows a stage in `0..=9` and clears the overlay for any other value,
/// keyed on the breaking entity: a given entity breaks at most one block at a
/// time, so a new position for an entity supersedes its previous one. This holds
/// the current stage per position for the renderer to consume.
#[derive(Debug, Default)]
pub struct BlockDestructionOverlays {
    entries: Vec<Overlay>,
}

#[derive(Debug, Clone, Copy)]
struct Overlay {
    entity_id: i32,
    pos: BlockPos,
    stage: u8,
}

impl BlockDestructionOverlays {
    /// An empty overlay set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds one [`ClientEvent`]. Returns `true` if it was a
    /// [`ClientEvent::BlockDestruction`] this state consumed.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        if let ClientEvent::BlockDestruction {
            entity_id,
            pos,
            progress,
        } = event
        {
            self.set(*entity_id, *pos, *progress);
            true
        } else {
            false
        }
    }

    fn set(&mut self, entity_id: i32, pos: BlockPos, stage: u8) {
        // Each entity breaks one block at a time; drop any prior block for it.
        self.entries.retain(|o| o.entity_id != entity_id);
        // A stage outside 0..=9 clears the overlay (vanilla
        // `LevelRenderer.setBlockBreakProgress`).
        if stage < 10 {
            self.entries.push(Overlay {
                entity_id,
                pos,
                stage,
            });
        }
    }

    /// The highest crack stage currently shown at `pos`, if any. Multiple
    /// players breaking the same block show the most-broken stage, matching
    /// vanilla's per-position `max`.
    #[must_use]
    pub fn stage_at(&self, pos: BlockPos) -> Option<u8> {
        self.entries
            .iter()
            .filter(|o| o.pos == pos)
            .map(|o| o.stage)
            .max()
    }

    /// Number of blocks currently showing a crack overlay (by entity).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether any crack overlay is shown.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A single item-pickup animation event: an item entity flew into a collector
/// and despawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pickup {
    /// The item entity that was collected (already despawning).
    pub item_entity_id: i32,
    /// The entity that collected it (usually a player).
    pub collector_id: i32,
    /// How many items were in the collected stack.
    pub amount: i32,
}

/// A transient feed of [`ClientEvent::ItemPickup`] animations.
///
/// # This is not an inventory
///
/// `take_item_entity` is purely the *pickup animation* (the item arcs toward the
/// collector before despawning). The resulting **inventory change** arrives
/// separately, as `set_player_inventory` / `container_set_content` /
/// `container_set_slot`, which [`crate::menus::Menus`] already folds. Folding a
/// count from here into an inventory would be a second, silently-diverging
/// source of truth for the same stacks — the exact duplication this crate exists
/// to avoid. So this feed carries only what the animation needs and leaves the
/// canonical inventory to `Menus`.
#[derive(Debug, Default)]
pub struct PickupFeed {
    pending: Vec<Pickup>,
}

impl PickupFeed {
    /// An empty feed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds one [`ClientEvent`]. Returns `true` if it was a
    /// [`ClientEvent::ItemPickup`] this feed consumed.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        if let ClientEvent::ItemPickup {
            item_entity_id,
            player_id,
            amount,
        } = event
        {
            self.pending.push(Pickup {
                item_entity_id: *item_entity_id,
                collector_id: *player_id,
                amount: *amount,
            });
            true
        } else {
            false
        }
    }

    /// Drains the pickups accumulated since the last drain, in arrival order, for
    /// the renderer/sound layer to animate. The feed is empty afterwards.
    pub fn drain(&mut self) -> Vec<Pickup> {
        std::mem::take(&mut self.pending)
    }

    /// Number of un-drained pickups.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether there are no un-drained pickups.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::ids::Identifier;

    fn ident(s: &str) -> Identifier {
        s.parse().unwrap()
    }

    // ---- Break-time formula: known values at known positions ----

    fn stone() -> BreakInputs {
        // Stone hardness is 1.5; a wrong/no tool is not the correct tool for
        // drops on stone.
        BreakInputs {
            hardness: 1.5,
            correct_tool: false,
            tool_speed: 1.0,
            ..BreakInputs::default()
        }
    }

    fn stone_with_diamond_pickaxe() -> BreakInputs {
        // Diamond pickaxe tool speed on stone is 8.0, and it is the correct tool.
        BreakInputs {
            hardness: 1.5,
            correct_tool: true,
            tool_speed: 8.0,
            ..BreakInputs::default()
        }
    }

    #[test]
    fn stone_bare_hand_takes_vanilla_tick_count() {
        // dig_speed = 1.0; per = 1.0 / 1.5 / 100 = 0.0066666668 (f32).
        //
        // Note the deliberate 151, not the "textbook" 150: `1.0 / per` is
        // 149.9999996, so 150 f32 accumulation steps sum to 0.99999… and stay
        // *below* 1.0 — the block needs a 151st tick. Vanilla accumulates
        // `destroyProgress += getDestroyProgress()` in f32 and breaks on `>= 1.0`,
        // so it undershoots the same way; the closed-form `ceil(1/per)` is an
        // approximation. `ticks_to_break` replays the real accumulator, which is
        // exactly why it is a loop and not a division.
        let s = stone();
        assert!((s.dig_speed() - 1.0).abs() < 1e-6);
        assert!((s.progress_per_tick() - (1.0f32 / 1.5 / 100.0)).abs() < 1e-9);
        assert_eq!(s.ticks_to_break(), Some(151));
    }

    #[test]
    fn stone_diamond_pickaxe_takes_vanilla_tick_count() {
        // dig_speed = 8.0; per = 8.0 / 1.5 / 30 = 0.17777...; accumulates to >=1 in 6 ticks.
        let s = stone_with_diamond_pickaxe();
        assert!((s.dig_speed() - 8.0).abs() < 1e-6);
        assert_eq!(s.ticks_to_break(), Some(6));
    }

    #[test]
    fn diamond_pickaxe_is_materially_faster_than_bare_hand() {
        // The property the live timing gate asserts, proven hermetically too.
        assert!(
            stone_with_diamond_pickaxe().ticks_to_break().unwrap()
                < stone().ticks_to_break().unwrap()
        );
    }

    #[test]
    fn unbreakable_block_never_breaks() {
        let s = BreakInputs {
            hardness: -1.0,
            ..BreakInputs::default()
        };
        assert_eq!(s.progress_per_tick(), 0.0);
        assert_eq!(s.ticks_to_break(), None);
    }

    #[test]
    fn zero_hardness_block_breaks_instantly() {
        // Tall grass / flowers: hardness 0 -> +inf progress -> instant break.
        let s = BreakInputs {
            hardness: 0.0,
            ..BreakInputs::default()
        };
        assert!(s.progress_per_tick().is_infinite());
        assert_eq!(s.ticks_to_break(), Some(0));
    }

    #[test]
    fn efficiency_only_helps_a_real_tool() {
        // Efficiency adds to a tool faster than the hand, but not to the bare
        // hand (tool_speed == 1.0 is not > 1.0).
        let hand = BreakInputs {
            tool_speed: 1.0,
            mining_efficiency: efficiency_bonus(5),
            ..BreakInputs::default()
        };
        assert!((hand.dig_speed() - 1.0).abs() < 1e-6);

        let pick = BreakInputs {
            hardness: 1.5,
            correct_tool: true,
            tool_speed: 8.0,
            mining_efficiency: efficiency_bonus(5), // 5*5+1 = 26
            ..BreakInputs::default()
        };
        assert!((pick.dig_speed() - (8.0 + 26.0)).abs() < 1e-6);
    }

    #[test]
    fn haste_multiplies_dig_speed() {
        // Haste II (amplifier 1): speed *= 1 + (1+1)*0.2 = 1.4.
        let s = BreakInputs {
            tool_speed: 8.0,
            haste_amplifier: Some(1),
            ..BreakInputs::default()
        };
        assert!((s.dig_speed() - 8.0 * 1.4).abs() < 1e-5);
    }

    #[test]
    fn mining_fatigue_scales_by_level() {
        for (amp, scale) in [(0u32, 0.3f32), (1, 0.09), (2, 0.0027), (3, 8.1e-4), (9, 8.1e-4)] {
            let s = BreakInputs {
                tool_speed: 8.0,
                mining_fatigue: Some(amp),
                ..BreakInputs::default()
            };
            assert!(
                (s.dig_speed() - 8.0 * scale).abs() < 1e-6,
                "fatigue amp {amp}"
            );
        }
    }

    #[test]
    fn submerged_and_off_ground_each_slow_five_times() {
        let base = BreakInputs {
            tool_speed: 8.0,
            ..BreakInputs::default()
        };
        assert!((base.dig_speed() - 8.0).abs() < 1e-6);

        let submerged = BreakInputs {
            submerged: true,
            ..base
        };
        // default submerged_mining_speed is 0.2 -> /5.
        assert!((submerged.dig_speed() - 8.0 * 0.2).abs() < 1e-6);

        let aqua_affinity = BreakInputs {
            submerged: true,
            submerged_mining_speed: 1.0,
            ..base
        };
        assert!((aqua_affinity.dig_speed() - 8.0).abs() < 1e-6);

        let flying = BreakInputs {
            on_ground: false,
            ..base
        };
        assert!((flying.dig_speed() - 8.0 / 5.0).abs() < 1e-6);
    }

    // ---- Dig state machine: known packet sequences ----

    fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    fn is_start(a: &ClientAction, at: BlockPos) -> bool {
        matches!(a, ClientAction::BlockAction { action: BlockActionKind::StartDestroy, pos, .. } if *pos == at)
    }
    fn is_stop(a: &ClientAction, at: BlockPos) -> bool {
        matches!(a, ClientAction::BlockAction { action: BlockActionKind::StopDestroy, pos, .. } if *pos == at)
    }
    fn is_abort(a: &ClientAction, at: BlockPos) -> bool {
        matches!(a, ClientAction::BlockAction { action: BlockActionKind::AbortDestroy, pos, .. } if *pos == at)
    }
    fn is_swing(a: &ClientAction) -> bool {
        matches!(a, ClientAction::SwingArm { .. })
    }

    #[test]
    fn multi_tick_dig_sends_start_then_stop_after_vanilla_ticks() {
        let mut m = Mining::new();
        let p = pos(10, 64, 10);
        let inputs = stone_with_diamond_pickaxe(); // 6 continue ticks

        let started = m.start(p, BlockFace::Up, &inputs, None);
        assert!(is_start(&started[0], p), "first action is START at target");
        assert!(is_swing(&started[1]), "start always swings");
        assert!(m.is_destroying());

        // Continue until it finishes; STOP must land on exactly the vanilla tick.
        let mut stop_tick = None;
        for tick in 1..=20 {
            let acts = m.continue_(p, BlockFace::Up, &inputs, None);
            if acts.iter().any(|a| is_stop(a, p)) {
                stop_tick = Some(tick);
                break;
            }
        }
        assert_eq!(
            stop_tick,
            Some(6),
            "STOP must be sent on the 6th continue tick, matching ticks_to_break"
        );
        assert!(!m.is_destroying(), "dig ends after STOP");
    }

    #[test]
    fn stop_button_sends_abort_not_stop() {
        let mut m = Mining::new();
        let p = pos(1, 2, 3);
        let inputs = stone(); // slow, so it is still in progress
        m.start(p, BlockFace::North, &inputs, None);
        m.continue_(p, BlockFace::North, &inputs, None);
        assert!(m.is_destroying());

        let released = m.stop();
        assert_eq!(released.len(), 1);
        assert!(is_abort(&released[0], p), "release sends ABORT for the target");
        assert!(!m.is_destroying());
    }

    #[test]
    fn instant_break_sends_only_start_and_leaves_no_dig() {
        let mut m = Mining::new();
        let p = pos(0, 70, 0);
        let inputs = BreakInputs {
            hardness: 0.0,
            ..BreakInputs::default()
        };
        let acts = m.start(p, BlockFace::Up, &inputs, None);
        assert!(is_start(&acts[0], p));
        assert!(is_swing(&acts[1]));
        assert!(
            !m.is_destroying(),
            "an instant-break block leaves no live dig — the server breaks it on START"
        );
    }

    #[test]
    fn retargeting_aborts_the_old_block_first() {
        let mut m = Mining::new();
        let a = pos(1, 64, 1);
        let b = pos(2, 64, 1);
        let inputs = stone();
        m.start(a, BlockFace::Up, &inputs, None);
        assert_eq!(m.target(), Some(a));

        // Aim at a different block: vanilla aborts the old one, then starts the new.
        let acts = m.start(b, BlockFace::Up, &inputs, None);
        assert!(is_abort(&acts[0], a), "old target is aborted first");
        assert!(is_start(&acts[1], b), "then the new target starts");
        assert_eq!(m.target(), Some(b));
    }

    #[test]
    fn switching_tools_mid_dig_restarts() {
        let mut m = Mining::new();
        let p = pos(5, 64, 5);
        let inputs = stone();
        let pick = ItemStack::new(ident("minecraft:wooden_pickaxe"), 1);
        let shovel = ItemStack::new(ident("minecraft:wooden_shovel"), 1);

        m.start(p, BlockFace::Up, &inputs, Some(pick.clone()));
        // Same block, different held item -> not sameDestroyTarget -> restart
        // (START again), not a progress continuation.
        let acts = m.continue_(p, BlockFace::Up, &inputs, Some(shovel));
        assert!(
            acts.iter().any(|a| is_start(a, p)),
            "a tool swap mid-dig restarts the break"
        );
    }

    #[test]
    fn destroy_stage_tracks_progress() {
        let mut m = Mining::new();
        let p = pos(0, 64, 0);
        // A slow block so several distinct stages are visible.
        let inputs = stone();
        assert_eq!(m.destroy_stage(), -1, "idle stage is -1");
        m.start(p, BlockFace::Up, &inputs, None);
        assert_eq!(m.destroy_stage(), -1, "no progress yet after START");
        for _ in 0..80 {
            m.continue_(p, BlockFace::Up, &inputs, None);
        }
        let stage = m.destroy_stage();
        assert!((0..=9).contains(&stage), "mid-dig stage in 0..=9, got {stage}");
    }

    // ---- Clientbound consumers ----

    #[test]
    fn block_destruction_overlay_shows_and_clears() {
        let mut o = BlockDestructionOverlays::new();
        let p = pos(3, 64, 3);
        assert!(o.apply(&ClientEvent::BlockDestruction {
            entity_id: 7,
            pos: p,
            progress: 4,
        }));
        assert_eq!(o.stage_at(p), Some(4));
        assert_eq!(o.len(), 1);

        // A new stage for the same breaker updates in place.
        o.apply(&ClientEvent::BlockDestruction {
            entity_id: 7,
            pos: p,
            progress: 8,
        });
        assert_eq!(o.stage_at(p), Some(8));
        assert_eq!(o.len(), 1, "same entity keeps one overlay");

        // A stage >= 10 clears it (vanilla's out-of-range = remove).
        o.apply(&ClientEvent::BlockDestruction {
            entity_id: 7,
            pos: p,
            progress: 10,
        });
        assert_eq!(o.stage_at(p), None);
        assert!(o.is_empty());
    }

    #[test]
    fn block_destruction_ignores_unrelated_events() {
        let mut o = BlockDestructionOverlays::new();
        assert!(!o.apply(&ClientEvent::ItemPickup {
            item_entity_id: 1,
            player_id: 2,
            amount: 3,
        }));
        assert!(o.is_empty());
    }

    #[test]
    fn pickup_feed_is_animation_only_and_drains() {
        let mut f = PickupFeed::new();
        assert!(f.apply(&ClientEvent::ItemPickup {
            item_entity_id: 11,
            player_id: 100,
            amount: 5,
        }));
        assert!(f.apply(&ClientEvent::ItemPickup {
            item_entity_id: 12,
            player_id: 100,
            amount: 2,
        }));
        assert_eq!(f.len(), 2);
        let drained = f.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].amount, 5);
        assert_eq!(drained[1].item_entity_id, 12);
        assert!(f.is_empty(), "drain empties the feed — it is transient, not an inventory");
    }
}
