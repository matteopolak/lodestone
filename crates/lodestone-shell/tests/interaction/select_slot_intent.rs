//! `SelectSlotIntent`, hermetically: a plugin's wish to change the selected
//! hotbar slot, driven through the real [`lodestone::interact::drive_select_slot`]
//! system — never a hand-called function.
//!
//! # What this proves, and what it does not
//!
//! Per `CLAUDE.md`'s "nothing is done until something on screen changes": a
//! test that only constructs a `SelectSlotIntent` and reads the component back
//! would prove nothing about the seam this file exists for. Every test here
//! runs a real `GameTick` [`Schedule`] holding the production system, so the
//! gate is the **echo** — the [`ClientAction::SetCarriedItem`] that reaches
//! [`lodestone_ecs::ActionQueue`] exactly when the selection actually changes —
//! actually firing (or, in the control, *not* firing) as a consequence of a
//! component write on the ECS side. Reading the echo off the queue, never off
//! any shell method, is what keeps the gate honest about the system under test
//! rather than about a path a plugin could not reach.
//!
//! # The harness is deliberately lighter than `place_intent.rs`'s
//!
//! `drive_select_slot` reads nothing but the player's [`SelectedSlot`] and the
//! [`SelectSlotIntent`] component — no version adapter, no chunk store, no
//! session — so this file stands up one [`EcsHandle`] and one spawned local
//! player and nothing else. That is itself a claim about the system's shape: a
//! `drive_select_slot` that grew a world read it does not need would fail to
//! fit in this harness.

use lodestone_ecs::ecs::entity::Entity;
use lodestone_ecs::ecs::schedule::Schedule;
use lodestone_ecs::player::{ActionQueue, SelectSlotIntent, SelectedSlot};
use lodestone_ecs::{EcsHandle, GameTick};
use lodestone_model::ClientAction;

struct Harness {
    ecs: EcsHandle,
    entity: Entity,
}

impl Harness {
    /// A local player spawned at `(0.5, 4.0, 0.5)`, a fresh [`ActionQueue`],
    /// and a `GameTick` [`Schedule`] holding [`drive_select_slot`] and nothing
    /// else.
    fn build() -> Self {
        let ecs = lodestone_ecs::new_handle();
        let entity = {
            let mut world = ecs.write();
            let state = lodestone_physics::PlayerState::at(
                lodestone_physics::Vec3d::new(0.5, 4.0, 0.5),
                0.0,
            );
            let entity = lodestone_ecs::spawn_local_player(&mut world, state);
            world.insert_resource(ActionQueue::default());
            let mut schedule = Schedule::new(GameTick);
            schedule.add_systems(lodestone::interact::drive_select_slot);
            world.add_schedule(schedule);
            entity
        };
        Self { ecs, entity }
    }

    /// Run one `GameTick` and return everything queued this tick, draining
    /// [`ActionQueue`] exactly as the real driver does between ticks.
    fn tick(&self) -> Vec<ClientAction> {
        let mut world = self.ecs.write();
        world.run_schedule(GameTick);
        world.resource_mut::<ActionQueue>().0.drain(..).collect()
    }

    /// The local player's current [`SelectedSlot`], straight off the component.
    fn selected(&self) -> usize {
        let world = self.ecs.write();
        world.get::<SelectedSlot>(self.entity).unwrap().0
    }

    fn set_intent(&self, intent: SelectSlotIntent) {
        let mut world = self.ecs.write();
        world.entity_mut(self.entity).insert(intent);
    }

    fn has_intent(&self) -> bool {
        let world = self.ecs.write();
        world.get::<SelectSlotIntent>(self.entity).is_some()
    }
}

// ---------------------------------------------------------------------------
// The gate: a real selection change, through the real system
// ---------------------------------------------------------------------------

/// **The gate.** A [`SelectSlotIntent`] on the ECS side must move the local
/// [`SelectedSlot`] and echo exactly one [`ClientAction::SetCarriedItem`] —
/// the same write-plus-echo [`lodestone::sim::Sim::select_slot`] performs for a
/// human's number key — then consume the intent.
#[test]
fn a_select_slot_intent_moves_the_local_selection_and_echoes_one_set_carried_item() {
    let harness = Harness::build();
    assert_eq!(harness.selected(), 0, "precondition: spawn starts at slot 0");

    harness.set_intent(SelectSlotIntent(4));
    let actions = harness.tick();

    assert_eq!(
        harness.selected(),
        4,
        "a valid intent must move the local selection"
    );
    assert_eq!(
        actions,
        vec![ClientAction::SetCarriedItem { slot: 4 }],
        "a real change must echo exactly one SetCarriedItem, no more, no less: {actions:?}"
    );
    assert!(
        !harness.has_intent(),
        "one insertion is one attempt: the shell must consume the intent"
    );
}

// ---------------------------------------------------------------------------
// The controls: the two cases `Sim::select_slot`'s own gate ignores
// ---------------------------------------------------------------------------

/// **The control.** A no-op (the slot already selected) and an out-of-range
/// value must both be consumed without moving the selection and without any
/// echo. Without this control the gate above would prove nothing about *when*
/// an echo is legitimate: a `drive_select_slot` that echoed on every intent
/// would pass it identically.
#[test]
fn a_noop_or_out_of_range_intent_is_consumed_without_an_echo() {
    let harness = Harness::build();

    // No-op: slot 0 is already selected.
    harness.set_intent(SelectSlotIntent(0));
    let actions = harness.tick();
    assert!(
        actions.is_empty(),
        "a no-op selection must not echo: {actions:?}"
    );
    assert_eq!(harness.selected(), 0, "a no-op must not move the selection");
    assert!(
        !harness.has_intent(),
        "a no-op intent is still an attempt, and is consumed"
    );

    // Out of range: vanilla's hotbar is `0..=8` (`HOTBAR_SLOTS` = 9).
    harness.set_intent(SelectSlotIntent(9));
    let actions = harness.tick();
    assert!(
        actions.is_empty(),
        "an out-of-range slot must not echo: {actions:?}"
    );
    assert_eq!(harness.selected(), 0, "an out-of-range slot must be ignored");
    assert!(
        !harness.has_intent(),
        "an out-of-range intent is still an attempt, and is consumed"
    );
}
