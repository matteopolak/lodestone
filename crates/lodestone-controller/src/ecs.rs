//! The controller as `GameTick` systems: held keys → [`MovementIntent`], and
//! the local player's two outbound movement packets.
//!
//! Stage 2 of `docs/bevy-migration.md`. The *components* live in
//! `lodestone-ecs`; these systems live here because this is the crate the
//! browser client shares, so putting the input→intent rule anywhere else would
//! reopen the movement fork this crate exists to close. It is also the only
//! direction the dependency graph permits: `lodestone-controller` →
//! `lodestone-client` → `lodestone-ecs`, so `lodestone-ecs` can never depend on
//! this crate.
//!
//! ```text
//! GameTick
//!   TickSet::Intent    compute_movement_intent → tick_sprint_window
//!   TickSet::Physics   lodestone_ecs::player::player_physics
//!   TickSet::Send      send_move_action → send_player_input
//! ```
//!
//! # Two orderings inside `TickSet::Intent` that are behaviour, not style
//!
//! [`compute_movement_intent`] runs **before** [`tick_sprint_window`]. The
//! pre-Stage-2 driver computed the intent, ran physics, and only then aged the
//! double-tap window, so the tick's intent was read from the *un-aged* input.
//! Swapping these would move the double-tap sprint window by one tick.
//!
//! [`tick_sprint_window`] must be in this fixed 20 Hz schedule and nowhere
//! else. Vanilla's `sprintTriggerTime` is counted in *ticks* (default 7,
//! [`SPRINT_TRIGGER_WINDOW_TICKS`]), so ageing it per frame instead would make
//! the double-tap window frame-rate dependent — wider at 144 fps than at 30.

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{Query, Res, ResMut, With};
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::IntoScheduleConfigs;
use lodestone_client::ClientAction;
use lodestone_ecs::player::{
    ActionQueue, Dead, Egress, ItemUseEffects, LastPlayerInput, LocalPlayer, MovementIntent,
    PhysicsState, SprintKeyHeld, Submersion,
};
use lodestone_ecs::session::{Abilities, Vitals};
use lodestone_ecs::{GameTick, TickSet};
use lodestone_model::PlayerInput;
use lodestone_physics::{MovementInput, UseEffects};

use crate::action::move_action;
use crate::input::{Action, InputState, movement_intent_with_gates};

/// The platform's held keys and accumulated mouse motion.
///
/// A resource rather than a component on the local player: a keyboard is a
/// property of the *process*, not of a player, and a swarm driver running
/// several clients in one `World` would drive them from bot code rather than
/// from one shared keyboard. The platform layer (winit in `lodestone-shell`,
/// web-sys in the browser) writes it; nothing else does.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq)]
pub struct RawInput(pub InputState);

/// [`movement_intent_with_food`] plus the one water exception vanilla's sprint
/// gate makes.
///
/// `movement_intent_with_food` vetoes sprint while sneaking, which is right on
/// land (vanilla's own is-moving-slowly query). Underwater it is wrong:
/// vanilla's own can-start-sprinting check ANDs in
/// "not moving slowly, or underwater"
/// and its own should-stop-swim-sprinting check explicitly *keeps* a swim-sprint alive while
/// shift is held. Shift is how you steer downward
/// while swimming (`goDownInWater`), so vetoing sprint on it means a submerged
/// player cannot swim and descend at the same time — they stop dead.
///
/// Implemented by re-running the gate on a copy of the input with sneak
/// cleared, rather than restating the gate: the two must not be able to drift
/// apart. Only the sprint bit is taken; `sneak` itself stays set, so the sink
/// impulse and the crouch pose still see it.
///
/// `sprint_allowed_by_food` (vanilla's food-level sprint gate) and `using_item`
/// (vanilla's use-item gates — see [`movement_intent_with_gates`]) are threaded
/// through unchanged to both calls — neither depends on submersion, so each is
/// one value for the whole tick, not something the swim recomputation could
/// disagree with.
#[must_use]
pub fn swim_adjusted_intent(
    state: &InputState,
    submerged: bool,
    sprint_allowed_by_food: bool,
    using_item: Option<UseEffects>,
) -> MovementInput {
    let mut intent = movement_intent_with_gates(state, sprint_allowed_by_food, using_item);
    if !intent.sneak || !submerged {
        return intent;
    }
    let mut without_sneak = *state;
    without_sneak.set(Action::Sneak, false);
    intent.sprint =
        movement_intent_with_gates(&without_sneak, sprint_allowed_by_food, using_item).sprint;
    intent
}

/// Write this tick's [`MovementIntent`] and [`SprintKeyHeld`] for every
/// [`LocalPlayer`].
///
/// # What changed observably in Stage 2
///
/// This used to be computed **once per frame**, outside the driver's
/// `while accumulator >= TICK_DT` loop, so a frame long enough to run several
/// catch-up ticks reused one decision for all of them. As a per-tick system:
///
/// * a double-tap sprint window that expires part-way through a multi-tick
///   frame now stops applying on the tick it expires, not at the end of the
///   frame;
/// * the submersion the swim exception reads is the previous *tick*'s, not the
///   previous *frame*'s, so a player who submerges during a catch-up burst
///   gains the swim-sprint exception a tick later rather than a frame later;
/// * at any frame rate at or above 20 fps a frame runs at most one tick, so
///   nothing changes at all. The difference is confined to stalls.
///
/// The one-tick lag on submersion is deliberate and is vanilla's own:
/// `baseTick` computes submersion before `aiStep` reads it.
///
/// # The food gate
///
/// `Vitals`/`Abilities` are read `Option`al because both start absent until
/// [`lodestone_ecs::session::insert_session_components`] runs (spawn, or
/// before a session ever begins) — an absent `Vitals` means "the server has
/// not told us a food level yet", which resolves to *allowed* rather than
/// *gated*, matching the `None` reading used everywhere else `Vitals` is
/// consulted (`docs/`, `Vitals`'s own doc comment): the absence of a report is
/// not evidence of empty food. `Abilities` absent likewise resolves to
/// "flight is not permitted", the same default `player.rs`'s own `Flying`
/// consumer uses.
pub fn compute_movement_intent(
    input: Res<RawInput>,
    mut players: Query<
        (
            &mut MovementIntent,
            &mut SprintKeyHeld,
            &PhysicsState,
            &Submersion,
            Option<&Dead>,
            Option<&Vitals>,
            Option<&Abilities>,
            Option<&ItemUseEffects>,
        ),
        With<LocalPlayer>,
    >,
) {
    for (mut intent, mut sprint_key, state, submersion, dead, vitals, abilities, using_item) in
        &mut players
    {
        sprint_key.0 = input.0.sprint_held();
        intent.0 = if dead.is_some() {
            // A corpse does not walk: ignore held keys while dead so the player
            // holds still on the death screen until the respawn teleport lands.
            MovementInput::NONE
        } else {
            let sprint_allowed_by_food = vitals
                .and_then(|v| v.food)
                .is_none_or(|food| food > crate::input::MIN_FOOD_LEVEL_TO_SPRINT)
                || abilities.is_some_and(|a| a.may_fly);
            swim_adjusted_intent(
                &input.0,
                submersion.0.under_water() || state.0.swimming,
                sprint_allowed_by_food,
                using_item.and_then(|u| u.0),
            )
        };
    }
}

/// Advance the double-tap-to-sprint window by one 20 Hz tick.
///
/// Ordered *after* [`compute_movement_intent`] — see this module's docs.
pub fn tick_sprint_window(mut input: ResMut<RawInput>) {
    input.0.tick();
}

/// Queue the per-tick movement **action**. This is deliberately NOT the wire
/// cadence, and the two must not be conflated.
///
/// # This does not, and must not, mean one packet is sent every tick
///
/// Vanilla's own client-side send-position step is *evaluated* every client
/// tick but *sends* on only a fraction of them: it tracks the position/
/// rotation last actually transmitted and emits `Pos`/`Rot`/`PosRot`/
/// `StatusOnly` only when that state is dirty by more than `(2e-4)²`
/// (position) or at all (rotation), or forces one `Pos` every 20 ticks
/// regardless (`positionReminder`); an idle player with no on-ground/
/// collision transition sends *nothing at all* on the other ~19 ticks out of
/// 20. `crates/protocol/v770/src/adapter/mod.rs`'s `select_move_packet` is a
/// verified, tested port of that exact algorithm
/// (`crates/protocol/v770/tests/movement_selection.rs`), so pushing one
/// `ClientAction::Move` here every tick does **not** put one packet on the
/// wire every tick for v770 — it is what keeps that downstream dirty-tracker
/// correctly clocked, since its own `positionReminder`-style counter only
/// advances when it is invoked, exactly mirroring `sendPosition()` being
/// called every real tick in vanilla. Removing this per-tick push (rather
/// than throttling *inside* the adapter, where the real "last sent" state
/// already lives) would starve that counter and silently break the 20-tick
/// periodic resync, not fix anything.
///
/// The legacy adapters make the same distinction, with deliberately different
/// family rules. v47 tracks the last pose and periodically refreshes it, but
/// still emits its base `flying` packet on every otherwise-idle tick; that is
/// vanilla 1.8 behavior. v340 and v735 throttle idle movement, emitting
/// `flying` only for an on-ground transition and otherwise staying quiet
/// until their periodic position reminder expires. The controller must still
/// invoke every family every tick: each adapter owns and advances the
/// per-connection reminder state that determines its cadence.
///
/// Only once we are actually in the world — before the server places us, a
/// version adapter (correctly) has no Play-state packet for a move, so
/// sending earlier just produces dropped-action noise. While dead the
/// vanilla client sends no movement (it is held on the death screen), so it
/// is withheld until the respawn lands.
pub fn send_move_action(
    egress: Res<Egress>,
    mut queue: ResMut<ActionQueue>,
    players: Query<(&PhysicsState, Option<&Dead>), With<LocalPlayer>>,
) {
    if !egress.in_world {
        return;
    }
    for (state, dead) in &players {
        if dead.is_none() {
            queue.0.push(move_action(&state.0));
        }
    }
}

/// Queue the edge-triggered [`PlayerInput`] packet when it changes.
///
/// Vanilla's player-input packet is the *only* way the server learns we are
/// sneaking — it never infers shift from the movement packet. Without this a
/// sneak-placement is treated as an interaction server-side (re-opening the
/// chest you meant to place against).
///
/// # This now reports the same intent physics used
///
/// Before Stage 2 this recomputed `movement_intent(&input)` for itself, which
/// deliberately vetoes sprint while sneaking — so a *submerged* player holding
/// shift and sprint had physics swim-sprinting (via
/// [`swim_adjusted_intent`]) while the wire said `sprint: false`. Reading the
/// [`MovementIntent`] component instead removes that disagreement: there is one
/// intent per tick and the server is told about the one that moved us.
///
/// [`Egress::live`] gates the latch, not just the send: a system that ran while
/// disconnected would record the current input into [`LastPlayerInput`] as
/// "already sent", and the first real change after connecting would then be
/// suppressed as a redundant resend.
pub fn send_player_input(
    egress: Res<Egress>,
    mut queue: ResMut<ActionQueue>,
    // The player-move veto registry. `Option`, so a client with no plugin installed
    // is unchanged.
    vetoes: Option<Res<lodestone_ecs::veto::ActionVetoes>>,
    mut players: Query<(&MovementIntent, &mut LastPlayerInput), With<LocalPlayer>>,
) {
    if !(egress.in_world && egress.live) {
        return;
    }
    for (intent, mut last) in &mut players {
        let intent = intent.0;
        let next = PlayerInput {
            forward: intent.forward > 0.0,
            backward: intent.forward < 0.0,
            left: intent.strafe > 0.0,
            right: intent.strafe < 0.0,
            jump: intent.jump,
            shift: intent.sneak,
            sprint: intent.sprint,
        };
        if last.0 == Some(next) {
            continue;
        }
        // The player-move veto. Asked only when the input actually
        // CHANGED (after the edge check above), so a plugin freezing a player
        // is asked once per real input change rather than 20 times a second --
        // and, more importantly, so a denial does not latch `LastPlayerInput`.
        // Latching a value that was never sent is the exact bug `Egress`'s own
        // doc comment describes: the first real change after the veto lifts
        // would be suppressed as a redundant resend.
        if let Some(vetoes) = &vetoes
            && vetoes.allows(&lodestone_ecs::veto::VerbContext::PlayerMove {
                moving: next.forward || next.backward || next.left || next.right,
                jumping: next.jump,
                sprinting: next.sprint,
            }) == lodestone_ecs::veto::Verdict::Deny
        {
            continue;
        }
        last.0 = Some(next);
        queue.0.push(ClientAction::SetPlayerInput(next));
    }
}

/// The controller's half of the `GameTick`: [`TickSet::Intent`] and
/// [`TickSet::Send`].
///
/// Pairs with [`lodestone_ecs::player::LocalPlayerPlugin`], which owns
/// `TickSet::Physics` and the components both halves read. Both are needed for
/// a driven, reported player; either alone is deliberately usable on its own
/// (physics-only for a headless movement harness, input-only for a replay).
#[derive(Debug, Default)]
pub struct ControllerPlugin;

impl Plugin for ControllerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RawInput>();
        // `TickSet::Intent` sits between `TickSet::Input` and
        // `TickSet::Physics` in the enum, but the master ordering chain
        // (`CorePlugin`, `Input, Physics, Predict, Animate, Send`) predates
        // that variant and is out of this crate's edit scope, so the edge is
        // added here. `configure_sets` is additive — `LocalPlayerPlugin`
        // declares the identical edge for `apply_look_intent`'s benefit, and
        // the two are redundant, not conflicting.
        app.configure_sets(
            GameTick,
            TickSet::Intent
                .after(TickSet::Input)
                .before(TickSet::Physics),
        );
        // **Ordered against `apply_look_intent`, which shares this set.** Both
        // touch `PhysicsState` — that one takes `&mut` (it commits this tick's
        // yaw/pitch) and this one takes `&` — so leaving them unordered is a real
        // write/read race, and under strict `ambiguity_detection` it fails the
        // schedule build. `exactly_one_system_writes_movement_intent` is the guard
        // that caught it.
        //
        // Look **before** movement is the correct direction, not just a tie-break:
        // `LookIntent` decides which way the player faces this tick and
        // `MovementInput`'s forward/strafe are relative to facing, so a programmatic
        // driver that aims at a block and walks toward it must have its rotation
        // committed before the movement intent is derived from it.
        app.add_systems(
            GameTick,
            (compute_movement_intent, tick_sprint_window)
                .chain()
                .after(lodestone_ecs::player::apply_look_intent)
                .in_set(TickSet::Intent),
        );
        app.add_systems(
            GameTick,
            (send_move_action, send_player_input)
                .chain()
                .in_set(TickSet::Send),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_ecs::player::{Flying, LocalPlayerPlugin, PlayerCollision, SelectedSlot};
    use lodestone_ecs::{CorePlugin, spawn_local_player};
    use lodestone_physics::{FluidState, PlayerState, Vec3d};

    fn app() -> (App, bevy_ecs::entity::Entity) {
        let mut app = App::new();
        app.add_plugins((CorePlugin, LocalPlayerPlugin, ControllerPlugin));
        // Nothing to stand on: these tests are about intent and egress, and a
        // frozen player keeps the physics arithmetic out of the assertions.
        app.insert_resource(PlayerCollision::NoWorld);
        let entity = spawn_local_player(
            app.world_mut(),
            PlayerState::at(Vec3d::new(0.5, 64.0, 0.5), 0.0),
        );
        (app, entity)
    }

    fn press(app: &mut App, action: Action, held: bool) {
        app.world_mut()
            .resource_mut::<RawInput>()
            .0
            .set(action, held);
    }

    fn tick(app: &mut App) {
        app.world_mut().run_schedule(GameTick);
    }

    fn drain(app: &mut App) -> Vec<ClientAction> {
        std::mem::take(&mut app.world_mut().resource_mut::<ActionQueue>().0)
    }

    /// The whole point of the stage: a `GameTick` run must turn held keys into
    /// the intent the physics set reads, through the schedule.
    #[test]
    fn a_game_tick_turns_held_keys_into_the_intent_component() {
        let (mut app, entity) = app();
        press(&mut app, Action::Forward, true);
        tick(&mut app);
        assert_eq!(
            app.world().get::<MovementIntent>(entity).unwrap().0.forward,
            1.0
        );
    }

    /// Negative control for the above: no key held, no intent. Without it,
    /// "forward == 1.0" could be satisfied by a default.
    #[test]
    fn no_key_held_yields_no_intent() {
        let (mut app, entity) = app();
        tick(&mut app);
        assert_eq!(
            app.world().get::<MovementIntent>(entity).unwrap().0,
            MovementInput::NONE
        );
    }

    /// The pre-existing per-*frame* intent meant several catch-up ticks in one
    /// slow frame shared one decision. As a per-tick system, a double-tap
    /// window that expires mid-burst stops applying on the tick it expires.
    #[test]
    fn the_double_tap_window_expires_mid_burst_rather_than_at_frame_end() {
        let (mut app, entity) = app();
        // Arm the window with one fresh press, then release. The window is
        // `SPRINT_TRIGGER_WINDOW_TICKS` long and is aged once per tick by
        // `tick_sprint_window`.
        press(&mut app, Action::Forward, true);
        press(&mut app, Action::Forward, false);
        for _ in 0..crate::input::SPRINT_TRIGGER_WINDOW_TICKS {
            tick(&mut app);
        }
        // The window has now expired inside the loop, so a second press does
        // not latch sprint.
        press(&mut app, Action::Forward, true);
        tick(&mut app);
        assert!(
            !app.world().get::<MovementIntent>(entity).unwrap().0.sprint,
            "a window aged tick-by-tick must be stale by now"
        );
    }

    /// Sneak is how you swim downward, so the land-side "sneak vetoes sprint"
    /// gate must not apply while submerged — otherwise holding shift underwater
    /// stops the swim dead. The land case is the control.
    #[test]
    fn sneak_cancels_sprint_on_land_but_not_under_water() {
        let mut state = InputState::default();
        state.set(Action::Forward, true);
        state.set(Action::Sprint, true);
        state.set(Action::Sneak, true);

        assert!(
            !swim_adjusted_intent(&state, false, true, None).sprint,
            "control: on land, sneaking still vetoes sprint"
        );
        let intent = swim_adjusted_intent(&state, true, true, None);
        assert!(
            intent.sprint,
            "submerged, shift must not cancel a swim-sprint"
        );
        assert!(
            intent.sneak,
            "…and shift itself must survive, or the sink impulse is lost"
        );
    }

    /// The food gate is independent of the swim exception — vanilla ANDs
    /// its own has-enough-food-to-do-exhaustive-manoeuvres query into its own
    /// is-sprinting-possible check
    /// regardless of the shallow-water/underwater branch it also gates,
    /// so "food says no" must win even in the one
    /// case (submerged + sneaking) that would otherwise grant a swim-sprint.
    #[test]
    fn the_food_gate_applies_even_to_a_swim_sprint() {
        let mut state = InputState::default();
        state.set(Action::Forward, true);
        state.set(Action::Sprint, true);
        state.set(Action::Sneak, true);

        // Control: without the food gate (allowed = true), submerged + sneak
        // does swim-sprint, matching the test above.
        assert!(swim_adjusted_intent(&state, true, true, None).sprint);
        // With it denied, the swim exception must not resurrect the sprint bit.
        assert!(
            !swim_adjusted_intent(&state, true, false, None).sprint,
            "low food must veto a swim-sprint too, not just the land case"
        );
    }

    /// The swim exception has to reach the *system*, not just the free
    /// function — the component read is what could silently be wired to the
    /// wrong source.
    #[test]
    fn the_intent_system_reads_submersion_for_the_swim_exception() {
        let (mut app, entity) = app();
        press(&mut app, Action::Forward, true);
        press(&mut app, Action::Sprint, true);
        press(&mut app, Action::Sneak, true);
        tick(&mut app);
        assert!(
            !app.world().get::<MovementIntent>(entity).unwrap().0.sprint,
            "control: dry, sneak vetoes sprint"
        );

        app.world_mut()
            .entity_mut(entity)
            .insert(Submersion(FluidState {
                water_height: 2.0,
                eye_in_water: true,
                ..FluidState::NONE
            }));
        tick(&mut app);
        assert!(
            app.world().get::<MovementIntent>(entity).unwrap().0.sprint,
            "submerged, the same keys must swim-sprint"
        );
    }

    /// Free-fly's speed doubling reads the *raw* sprint key, which the walking
    /// gate would have vetoed here (no forward impulse).
    #[test]
    fn the_raw_sprint_key_survives_the_walking_gate_for_free_fly() {
        let (mut app, entity) = app();
        app.world_mut().entity_mut(entity).insert(Flying(true));
        press(&mut app, Action::Sprint, true);
        tick(&mut app);
        assert!(
            !app.world().get::<MovementIntent>(entity).unwrap().0.sprint,
            "standing still, the gated sprint bit is false"
        );
        assert!(
            app.world().get::<SprintKeyHeld>(entity).unwrap().0,
            "…but free-fly still needs to see the key itself"
        );
    }

    /// The food gate has to reach the system, not just the free functions — `Vitals::food` has to
    /// actually reach `compute_movement_intent` for a low-food player to stop
    /// sprinting. The healthy-food tick is the control: without it, "low food
    /// stops sprint" could pass against a system that vetoes sprint
    /// unconditionally.
    #[test]
    fn low_food_stops_the_intent_system_from_granting_sprint() {
        let (mut app, entity) = app();
        press(&mut app, Action::Forward, true);
        press(&mut app, Action::Sprint, true);

        app.world_mut().entity_mut(entity).insert(Vitals {
            food: Some(20),
            ..Vitals::default()
        });
        tick(&mut app);
        assert!(
            app.world().get::<MovementIntent>(entity).unwrap().0.sprint,
            "control: full food sprints normally"
        );

        app.world_mut().entity_mut(entity).insert(Vitals {
            food: Some(6),
            ..Vitals::default()
        });
        tick(&mut app);
        assert!(
            !app.world().get::<MovementIntent>(entity).unwrap().0.sprint,
            "food level 6 is vanilla's own cutoff (`> 6`, not `>= 6`) and must not sprint"
        );
    }

    /// No `Vitals` component at all (before the first `set_health` packet)
    /// must not be read as "zero food" — that would make sprint impossible for
    /// every tick before the server's first vitals report.
    #[test]
    fn absent_vitals_does_not_block_sprint() {
        let (mut app, entity) = app();
        press(&mut app, Action::Forward, true);
        press(&mut app, Action::Sprint, true);
        tick(&mut app);
        assert!(
            app.world().get::<MovementIntent>(entity).unwrap().0.sprint,
            "no vitals report yet must resolve to allowed, not denied"
        );
    }

    /// Vanilla's `mayfly` ability bypasses the food check entirely
    /// (its own is-sprinting-possible check ORs it in), so creative/spectator sprint must
    /// survive food exhaustion.
    #[test]
    fn may_fly_bypasses_the_food_gate() {
        let (mut app, entity) = app();
        press(&mut app, Action::Forward, true);
        press(&mut app, Action::Sprint, true);
        app.world_mut().entity_mut(entity).insert(Vitals {
            food: Some(0),
            ..Vitals::default()
        });
        tick(&mut app);
        assert!(
            !app.world().get::<MovementIntent>(entity).unwrap().0.sprint,
            "control: zero food alone stops sprint"
        );

        app.world_mut().entity_mut(entity).insert(Abilities {
            may_fly: true,
            ..Abilities::default()
        });
        tick(&mut app);
        assert!(
            app.world().get::<MovementIntent>(entity).unwrap().0.sprint,
            "may_fly must override the food gate, exactly like vanilla's `||`"
        );
    }

    /// The use-item gate has to reach the *system*, not just the free
    /// functions — `ItemUseEffects` has to actually reach
    /// `compute_movement_intent` for a player charging a default-effects item
    /// to stop sprinting, and the resulting scale has to reach
    /// `MovementIntent` so `lodestone-physics` can apply it. The idle tick is
    /// the control: without it, "using an item stops sprint" could pass
    /// against a system that vetoes sprint unconditionally.
    #[test]
    fn item_use_effects_reach_the_intent_system_and_veto_sprint() {
        let (mut app, entity) = app();
        press(&mut app, Action::Forward, true);
        press(&mut app, Action::Sprint, true);
        tick(&mut app);
        assert!(
            app.world().get::<MovementIntent>(entity).unwrap().0.sprint,
            "control: idle (spawn_local_player's ItemUseEffects(None)) sprints normally"
        );
        assert_eq!(
            app.world().get::<MovementIntent>(entity).unwrap().0.using_item,
            None,
            "control: idle carries no use-item scale onto the intent"
        );

        app.world_mut()
            .entity_mut(entity)
            .insert(ItemUseEffects(Some(UseEffects::DEFAULT)));
        tick(&mut app);
        let intent = app.world().get::<MovementIntent>(entity).unwrap().0;
        assert!(
            !intent.sprint,
            "a default-effects use-item must reach the system and veto sprint"
        );
        assert_eq!(
            intent.using_item,
            Some(UseEffects::DEFAULT),
            "the use-item scale must reach MovementIntent for physics to apply"
        );
    }

    /// The spear override must reach the system too, and — unlike the default
    /// case above — must NOT veto sprint once it does.
    #[test]
    fn spear_use_effects_reach_the_intent_system_without_vetoing_sprint() {
        let (mut app, entity) = app();
        press(&mut app, Action::Forward, true);
        press(&mut app, Action::Sprint, true);
        app.world_mut()
            .entity_mut(entity)
            .insert(ItemUseEffects(Some(UseEffects::SPEAR)));
        tick(&mut app);
        let intent = app.world().get::<MovementIntent>(entity).unwrap().0;
        assert!(
            intent.sprint,
            "a spear's can_sprint must reach the system and NOT veto sprint"
        );
        assert_eq!(intent.using_item, Some(UseEffects::SPEAR));
    }

    /// A dead player holds still and is not reported as moving.
    #[test]
    fn a_dead_player_neither_walks_nor_sends_movement() {
        let (mut app, entity) = app();
        app.world_mut().insert_resource(Egress {
            in_world: true,
            live: true,
        });
        press(&mut app, Action::Forward, true);
        app.world_mut().entity_mut(entity).insert(Dead);
        tick(&mut app);
        assert_eq!(
            app.world().get::<MovementIntent>(entity).unwrap().0,
            MovementInput::NONE
        );
        assert!(
            !drain(&mut app)
                .iter()
                .any(|a| matches!(a, ClientAction::Move { .. })),
            "no movement packet from the death screen"
        );
    }

    /// Nothing reaches the queue until the server has placed us — and the
    /// edge-tracker must not latch either, or the first real input after
    /// joining would be suppressed as a redundant resend.
    #[test]
    fn a_closed_session_queues_nothing_and_latches_nothing() {
        let (mut app, entity) = app();
        press(&mut app, Action::Forward, true);
        tick(&mut app);
        assert!(drain(&mut app).is_empty());
        assert_eq!(
            app.world().get::<LastPlayerInput>(entity).unwrap().0,
            None,
            "the edge-tracker must stay unlatched while disconnected"
        );

        // …and once connected, that very same held key is reported.
        app.world_mut().insert_resource(Egress {
            in_world: true,
            live: true,
        });
        tick(&mut app);
        let sent = drain(&mut app);
        assert!(sent.iter().any(|a| matches!(
            a,
            ClientAction::SetPlayerInput(PlayerInput { forward: true, .. })
        )));
    }

    /// One move per tick, and the input packet only on a change — the wire
    /// contract `Sim`'s live gates measure.
    #[test]
    fn one_move_per_tick_and_an_edge_triggered_input_packet() {
        let (mut app, _entity) = app();
        app.world_mut().insert_resource(Egress {
            in_world: true,
            live: true,
        });
        press(&mut app, Action::Forward, true);
        tick(&mut app);
        let first = drain(&mut app);
        assert_eq!(
            first
                .iter()
                .filter(|a| matches!(a, ClientAction::Move { .. }))
                .count(),
            1
        );
        assert_eq!(
            first
                .iter()
                .filter(|a| matches!(a, ClientAction::SetPlayerInput(_)))
                .count(),
            1
        );

        tick(&mut app);
        let second = drain(&mut app);
        assert_eq!(
            second
                .iter()
                .filter(|a| matches!(a, ClientAction::Move { .. }))
                .count(),
            1,
            "movement is unconditional every tick"
        );
        assert!(
            !second
                .iter()
                .any(|a| matches!(a, ClientAction::SetPlayerInput(_))),
            "…but the input packet is edge-triggered"
        );
    }

    /// The move packet is queued *after* everything in `TickSet::Physics` has
    /// run, which is what makes a plugin inserted between the two able to
    /// change what the server is told this tick.
    #[test]
    fn a_plugin_between_physics_and_send_changes_what_is_reported() {
        use bevy_ecs::prelude::Query;

        let (mut app, _entity) = app();
        app.world_mut().insert_resource(Egress {
            in_world: true,
            live: true,
        });
        app.add_systems(
            GameTick,
            (|mut q: Query<&mut PhysicsState>| {
                for mut state in &mut q {
                    state.0.position.y = 1234.0;
                }
            })
            .after(TickSet::Physics)
            .before(TickSet::Send),
        );
        tick(&mut app);
        let moves: Vec<_> = drain(&mut app)
            .into_iter()
            .filter_map(|a| match a {
                ClientAction::Move { pos, .. } => Some(pos.y),
                _ => None,
            })
            .collect();
        assert_eq!(
            moves,
            vec![1234.0],
            "the plugin's write must be what reaches the wire"
        );
    }

    /// The hotbar selection is not part of the movement tick and must survive
    /// one — a cheap guard against `spawn_local_player`'s eager component set
    /// being re-inserted by some system each tick.
    #[test]
    fn the_selected_slot_is_untouched_by_a_movement_tick() {
        let (mut app, entity) = app();
        app.world_mut().entity_mut(entity).insert(SelectedSlot(4));
        tick(&mut app);
        assert_eq!(app.world().get::<SelectedSlot>(entity).unwrap().0, 4);
    }

    /// The reason `TickSet::Intent` exists: **exactly one system writes
    /// `MovementIntent`.** A plugin adding a second, unordered writer in the
    /// same set must fail the schedule build under strict ambiguity
    /// detection — the failure mode `docs/plugin-api.md`'s gap list names
    /// directly (`docs/bevy-migration.md`'s planned
    /// `ambiguity_detection: LogLevel::Error`).
    ///
    /// Mirrors `lodestone_ecs::session`'s
    /// `exactly_one_system_writes_each_session_component` pattern: a positive
    /// case (the shipped `ControllerPlugin` alone, not ambiguous) and its
    /// negative control (with a rogue second writer, ambiguous) — the control
    /// is what proves the detector is actually switched on, rather than the
    /// first assertion passing vacuously against a no-op checker.
    #[test]
    fn exactly_one_system_writes_movement_intent() {
        assert!(
            !game_tick_is_ambiguous(false),
            "the shipped GameTick schedule must have no unordered conflicting pair"
        );
    }

    /// The negative control for the test above.
    #[test]
    fn a_second_unordered_intent_writer_fails_the_ambiguity_check() {
        assert!(
            game_tick_is_ambiguous(true),
            "a second unordered writer of MovementIntent must be reported"
        );
    }

    /// Build `ControllerPlugin`'s `GameTick` with ambiguity detection promoted
    /// to an error, optionally adding a rogue second `MovementIntent` writer
    /// anchored on the same `TickSet::Intent` set with no explicit order
    /// against the shipped writer.
    fn game_tick_is_ambiguous(with_rogue_writer: bool) -> bool {
        use bevy_ecs::schedule::{LogLevel, ScheduleBuildSettings};

        fn rogue(mut intents: Query<&mut MovementIntent>) {
            for mut intent in &mut intents {
                intent.0 = MovementInput::NONE;
            }
        }

        let mut app = App::new();
        app.add_plugins((CorePlugin, LocalPlayerPlugin, ControllerPlugin));
        if with_rogue_writer {
            app.add_systems(GameTick, rogue.in_set(TickSet::Intent));
        }
        // Deliberately *not* run first: an already-built schedule is not
        // rebuilt, so `initialize` would return `Ok` without ever consulting
        // the new settings — which is exactly how this assertion would go
        // vacuous.
        app.world_mut().schedule_scope(GameTick, |world, schedule| {
            schedule.set_build_settings(ScheduleBuildSettings {
                ambiguity_detection: LogLevel::Error,
                ..ScheduleBuildSettings::default()
            });
            schedule.initialize(world).is_err()
        })
    }

}
