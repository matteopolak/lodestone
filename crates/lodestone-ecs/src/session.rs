//! Session and HUD state as components — Stage 3 of `docs/bevy-migration.md`.
//!
//! # The double fold this module exists to delete
//!
//! `docs/bevy-migration.md` §1.1 measured two *different types* named
//! `Scoreboard` folding the same `ClientEvent` stream in two crates, plus two
//! player-list folds. Tracing it out, it was worse than that: for the
//! scoreboard/tab-list/boss-bar family there were **three** implementations —
//! `lodestone_client::scoreboard::Scoreboard` (folded on the net thread, read
//! by the bot API), `lodestone_game::scoreboard::Scoreboard` (folded on the
//! driver thread from `NetUpdate::ScoreboardEvent`, the copy that reached
//! pixels), and `lodestone_game::bossbar::BossBarSet`, which was a complete
//! canonical fold **nothing called at all**.
//!
//! The collapse is: `lodestone-game`'s aggregates *are* the component set, and
//! the [`SessionSet`] systems here are the only fold. `lodestone-client`'s
//! `scoreboard` module is deleted; the shell no longer folds anything, it
//! reads.
//!
//! # Two halves, and why the split is not arbitrary
//!
//! | half | lives in which `World` | why |
//! |---|---|---|
//! | [`SessionScoreboard`] / [`SessionTabList`] / [`SessionBossBars`] / [`SessionMenus`] | the **net thread's** (`lodestone_client::state::SharedState`) | both the bot API *and* the shell HUD read them, and only the net thread's `World` is reachable from `ClientHandle` |
//! | [`Phase`] / [`Vitals`] / [`Xp`] / [`TitleOverlay`] / [`ActionBarOverlay`] / [`HudEffects`] / [`RespawnCount`] / [`ServerEntityId`] | the **driver's** (`lodestone_shell::sim::Sim`) | nothing else folds them, and per-tick driver logic reads them |
//!
//! That rule — *the fold lives where the readers are shared; a fold with a
//! single driver-side reader stays on the driver* — is what keeps this stage
//! from trading one duplicate for another. A component in one `World` cannot be
//! read by a system in the other, and the workspace has three `World`s until
//! `docs/bevy-migration.md` §4.1 unifies them, so "put everything in one place"
//! is not yet an option that exists.
//!
//! # What deliberately did **not** collapse
//!
//! `lodestone_client::state::PlayerSnapshot`'s vitals (`health`, `food`,
//! `saturation`, `xp_*`, `entity_id`, `alive`) still duplicate [`Vitals`] /
//! [`Xp`] / [`ServerEntityId`]. That is the same ruling Stage 2 made for
//! `PlayerSnapshot` as a whole, and it holds for a concrete reason: the
//! driver-side copies are read by *systems and per-tick logic* (`Dead` gates
//! `MovementIntent`; `ServerEntityId` filters which entity's mob effects reach
//! `PlayerState::effects`), so they cannot live in the net thread's `World`
//! without either a per-tick mirror — a second source of truth by definition —
//! or taking the net thread's lock inside the physics tick. See the Stage 3
//! doc for the third implementation this leaves standing
//! (`lodestone_game::player_state::HudState`) and what blocks adopting it.

use bevy_app::{App, Plugin};
use bevy_ecs::component::Component;
use bevy_ecs::prelude::{Query, Res, With};
use bevy_ecs::schedule::{IntoScheduleConfigs, SystemSet};
use bevy_ecs::world::World;
use lodestone_model::ClientEvent;

use crate::ingest::{IngestBatch, IngestQueuePlugin};
use crate::player::LocalPlayer;
use crate::schedules::{GameTick, NetIngest};
use crate::sets::{IngestSet, TickSet};

// ---------------------------------------------------------------------------
// The shared-fold half: components in the net thread's `World`
// ---------------------------------------------------------------------------

/// The folded scoreboard — objectives, scores, the nineteen display slots and
/// teams.
///
/// The **only** copy. `lodestone_client::scoreboard::Scoreboard` (a second
/// type, a second fold, with subtly different semantics for a score that
/// arrives before its objective) is deleted, and
/// `lodestone_shell::sim::Sim::scoreboard` is deleted; both readers now read
/// this.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionScoreboard(pub lodestone_game::scoreboard::Scoreboard);

/// The folded tab list — profiles, latency, game mode, display names, header
/// and footer.
///
/// Replaces both `Inner.players: HashMap<Uuid, PlayerListEntry>` (which had no
/// `PlayerListRemove` arm at all, so a player who left never disappeared) and
/// `Sim.tab_list`.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTabList(pub lodestone_game::tablist::TabList);

/// The active boss bars, in server insertion (render) order.
///
/// `lodestone_game::bossbar::BossBarSet` was a fully implemented, unit-tested
/// fold with **no production caller** — the third implementation of this event
/// family and the island this component closes.
#[derive(Component, Debug, Clone, Default)]
pub struct SessionBossBars(pub lodestone_game::bossbar::BossBarSet);

/// The player inventory plus at most one open container, with its click
/// prediction.
///
/// Not a pure fold: `lodestone_client`'s `menu_click` predicts against this in
/// place, which is why a reader is handed a *clone* and a predictor must reach
/// the component itself.
#[derive(Component, Debug, Clone, Default)]
pub struct SessionMenus(pub lodestone_game::menus::Menus);

/// Ordering label for the session folds, inside [`IngestSet::Apply`].
///
/// A plugin that wants to observe a folded scoreboard orders
/// `.after(SessionSet::Fold)`; one that wants to pre-empt it orders `.before`.
/// A *set* rather than the system functions, per `docs/bevy-migration.md` §2.6.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionSet {
    /// The `ClientEvent` → session-component folds.
    Fold,
}

/// Whether an event is folded by the systems in this module.
///
/// The caller-side routing switch, kept next to the systems for the same
/// reason [`crate::ingest::handles_event`] is: an event routed to the ECS that
/// no system folds vanishes silently.
///
/// The [`SessionMenus`] family is listed explicitly rather than delegated to
/// `Menus::apply`, because `handles_event` must answer without a `&mut Menus`
/// to hand it. Keep the two in step: an arm added to `Menus::apply` and
/// forgotten here never reaches the ECS at all.
#[must_use]
pub fn handles_event(event: &ClientEvent) -> bool {
    matches!(
        event,
        // scoreboard
        ClientEvent::ObjectiveUpdate { .. }
            | ClientEvent::DisplayObjective { .. }
            | ClientEvent::ScoreUpdate { .. }
            | ClientEvent::ScoreReset { .. }
            | ClientEvent::TeamUpdate { .. }
            // tab list
            | ClientEvent::PlayerListUpdate { .. }
            | ClientEvent::PlayerListRemove { .. }
            // boss bars
            | ClientEvent::BossBarUpdate { .. }
            // menus
            | ClientEvent::ScreenOpened { .. }
            | ClientEvent::ScreenClosed { .. }
            | ClientEvent::ContainerContent { .. }
            | ClientEvent::ContainerSlot { .. }
            | ClientEvent::ContainerData { .. }
            | ClientEvent::CursorItemChanged { .. }
            | ClientEvent::InventorySlotChanged { .. }
    )
}

/// `IngestSet::Apply`: the scoreboard family → [`SessionScoreboard`].
pub fn apply_scoreboard(batch: Res<IngestBatch>, mut boards: Query<&mut SessionScoreboard>) {
    for event in batch.events() {
        for mut board in &mut boards {
            let _ = board.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: the player-list family → [`SessionTabList`].
pub fn apply_tab_list(batch: Res<IngestBatch>, mut lists: Query<&mut SessionTabList>) {
    for event in batch.events() {
        for mut list in &mut lists {
            let _ = list.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: `BossBarUpdate` → [`SessionBossBars`].
pub fn apply_boss_bars(batch: Res<IngestBatch>, mut bars: Query<&mut SessionBossBars>) {
    for event in batch.events() {
        for mut set in &mut bars {
            let _ = set.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: the container family → [`SessionMenus`].
pub fn apply_menus(batch: Res<IngestBatch>, mut menus: Query<&mut SessionMenus>) {
    for event in batch.events() {
        for mut session in &mut menus {
            let _ = session.0.apply(event);
        }
    }
}

/// Insert the shared-fold session component set onto `entity`.
///
/// Every component is inserted eagerly, like [`crate::spawn_local_player`] and
/// unlike the *observed*-entity set: an empty scoreboard is a real state ("the
/// server has sent no objectives"), not an unknown one, so there is no
/// three-state encoding to preserve.
pub fn insert_session_components(world: &mut World, entity: bevy_ecs::entity::Entity) {
    if let Ok(mut entity) = world.get_entity_mut(entity) {
        entity.insert((
            SessionScoreboard::default(),
            SessionTabList::default(),
            SessionBossBars::default(),
            SessionMenus::default(),
        ));
    }
}

/// Spawn the net thread's session entity, carrying [`LocalPlayer`] and the
/// shared-fold component set.
///
/// [`LocalPlayer`] because this *is* the client's own player entity in that
/// `World` — the marker is what lets the §4.1 unification merge this entity
/// with the driver's without renaming anything.
pub fn spawn_session(world: &mut World) -> bevy_ecs::entity::Entity {
    let entity = world.spawn(LocalPlayer).id();
    insert_session_components(world, entity);
    entity
}

/// Registers the shared-fold half: the four session components' `NetIngest`
/// systems.
///
/// Deliberately **not** part of [`crate::CorePlugin`], and deliberately not
/// added by [`SessionHudPlugin`]: only the `World` that is *authoritative* over
/// session state gets these, exactly as `IngestPlugin` is only added to the
/// `World` authoritative over entities. Two `World`s folding one event stream
/// is the defect this module deletes.
#[derive(Debug, Default)]
pub struct SessionPlugin;

impl Plugin for SessionPlugin {
    fn build(&self, app: &mut App) {
        // Via `is_plugin_added`, so a `World` carrying `IngestPlugin` too gets
        // exactly **one** `drain_ingest_queue`. Two of them silently blank every
        // batch — see [`IngestQueuePlugin`]'s docs for how that was found.
        if !app.is_plugin_added::<IngestQueuePlugin>() {
            app.add_plugins(IngestQueuePlugin);
        }
        app.add_systems(
            NetIngest,
            (
                apply_scoreboard,
                apply_tab_list,
                apply_boss_bars,
                apply_menus,
            )
                .chain()
                .in_set(SessionSet::Fold)
                .in_set(IngestSet::Apply),
        );
    }
}

// ---------------------------------------------------------------------------
// The driver half: components in the shell's `World`
// ---------------------------------------------------------------------------

/// The coarse phase of the client's session.
///
/// Moved here from `lodestone_shell::sim` (which re-exports it, so `app.rs` and
/// the live gates are unchanged) because it is session state like everything
/// else in this module, and because `crate::player::Egress` is derived from it.
///
/// Purely a read-model: it never affects physics or rendering directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPhase {
    /// No live connection — the offline fixture world.
    LocalOnly,
    /// A live connection is attached and still handshaking / logging in.
    Connecting,
    /// Logged in to the server.
    Connected,
    /// The session ended; carries the human-readable reason (disconnect, net
    /// error, or death). Terminal until a new connection is attached.
    Ended(String),
}

/// The session phase, as a component on the local player.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct Phase(pub SessionPhase);

impl Default for Phase {
    fn default() -> Self {
        Self(SessionPhase::LocalOnly)
    }
}

/// Server-reported vitals for the HUD.
///
/// `Option` rather than a value with a default, and that is load-bearing:
/// `None` means *the server has not reported this*, which is how the offline
/// fixture world draws no health bar at all rather than a full one.
/// `lodestone_game::player_state::HudState` — the canonical aggregate — has no
/// such bit, which is why this stage does not adopt it (see the Stage 3 doc).
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct Vitals {
    /// Health in `0..=20`, or `None` before the first `set_health`.
    pub health: Option<f32>,
    /// Food level in `0..=20`, or `None` before the first `set_health`.
    pub food: Option<i32>,
}

/// Server-reported experience as `(progress, level, total)`, `None` until
/// `set_experience` arrives.
///
/// The HUD must not substitute a locally-derived guess: there is no vanilla
/// levelling curve the client could invert from partial data that is
/// guaranteed to match a (possibly modded) server's own numbers.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct Xp(pub Option<(f32, i32, i32)>);

/// The title/subtitle overlay and its vanilla fade timer.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct TitleOverlay(pub lodestone_game::player_state::TitleState);

/// The action-bar (GameInfo) overlay; self-clears after 60 ticks.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct ActionBarOverlay(pub lodestone_game::player_state::ActionBar);

/// The local player's active status effects for the HUD stack.
///
/// Distinct from `PhysicsState`'s `effects`, which is the *physics* view (only
/// motion-relevant effects, no durations). This is the full display set, and
/// the two are not a duplicate of each other: the physics one is an integrator
/// input, this one is a row of icons with countdowns.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct HudEffects(pub lodestone_game::effect::ActiveEffects);

/// Respawns observed this session — the diagnostic the live death gate reads to
/// confirm the client actually recovered rather than merely never dying.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RespawnCount(pub u64);

/// The server-assigned entity id for the local player, `None` before login.
///
/// Entity-scoped updates that must decide "is this us" (mob effects, most
/// obviously) compare against this rather than guessing, so an id the next
/// session reuses cannot be misattributed.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerEntityId(pub Option<i32>);

/// `TickSet::Animate`: age the three self-expiring HUD overlays one tick.
///
/// **Must stay in the fixed 20 Hz schedule.** Every duration here is counted in
/// *ticks* — vanilla's action bar is 60 ticks, a title's fade is `TitleTimes`
/// ticks, an effect's remaining duration is ticks — so ageing them per frame
/// makes each one frame-rate dependent: an action bar would vanish twice as
/// fast at 120 fps as at 60.
pub fn tick_hud_overlays(
    mut players: Query<(&mut TitleOverlay, &mut ActionBarOverlay, &mut HudEffects), With<LocalPlayer>>,
) {
    for (mut title, mut action_bar, mut effects) in &mut players {
        title.0.tick(1);
        action_bar.0.tick(1);
        effects.0.tick(1);
    }
}

/// Insert the driver-half session/HUD component set onto `entity`.
///
/// Called by both the spawn and the reset path in the driver, so a component
/// added here cannot be missed by a quit-to-title (the failure mode
/// `reset_local_player`'s docs warn about).
pub fn insert_hud_components(world: &mut World, entity: bevy_ecs::entity::Entity) {
    if let Ok(mut entity) = world.get_entity_mut(entity) {
        entity.insert((
            Phase::default(),
            Vitals::default(),
            Xp::default(),
            TitleOverlay::default(),
            ActionBarOverlay::default(),
            HudEffects::default(),
            RespawnCount::default(),
            ServerEntityId::default(),
        ));
    }
}

/// Registers the driver half: [`tick_hud_overlays`] in [`TickSet::Animate`].
///
/// Separate from [`SessionPlugin`] because they belong to different `World`s
/// (see this module's docs). Adding both to one `App` is legal and is what the
/// §4.1 unification will do.
#[derive(Debug, Default)]
pub struct SessionHudPlugin;

impl Plugin for SessionHudPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<crate::CorePlugin>() {
            app.add_plugins(crate::CorePlugin);
        }
        app.add_systems(GameTick, tick_hud_overlays.in_set(TickSet::Animate));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::event::{DisplaySlot, ObjectiveMode};
    use lodestone_model::{BossAction, BossColor, BossOverlay, GameMode, PlayerListEntry, Text};
    use uuid::Uuid;

    /// Build the net-thread shape: `SessionPlugin` plus one session entity.
    fn session_app() -> (App, bevy_ecs::entity::Entity) {
        let mut app = App::new();
        app.add_plugins(SessionPlugin);
        let entity = spawn_session(app.world_mut());
        (app, entity)
    }

    fn fold(app: &mut App, event: ClientEvent) {
        app.world_mut()
            .resource_mut::<crate::ingest::IngestQueue>()
            .push(event);
        app.world_mut().run_schedule(NetIngest);
    }

    /// The fold must be reachable **through the schedule**. A directly-called
    /// `Scoreboard::apply` passes its own unit tests while the schedule
    /// registration is missing, which is the island this migration has found
    /// nine times.
    #[test]
    fn a_net_ingest_run_folds_a_scoreboard_objective_onto_the_component() {
        let (mut app, entity) = session_app();
        fold(
            &mut app,
            ClientEvent::ObjectiveUpdate {
                name: "kills".into(),
                mode: ObjectiveMode::Add,
                display_name: Some(Text::literal("Kills")),
                render_type: None,
                number_format: None,
            },
        );
        fold(
            &mut app,
            ClientEvent::DisplayObjective {
                slot: DisplaySlot::Sidebar,
                objective: Some("kills".into()),
            },
        );
        fold(
            &mut app,
            ClientEvent::ScoreUpdate {
                holder: "Alice".into(),
                objective: "kills".into(),
                value: 7,
                display: None,
                number_format: None,
            },
        );

        let board = &app.world().get::<SessionScoreboard>(entity).unwrap().0;
        assert_eq!(
            board.displayed(lodestone_game::scoreboard::DisplaySlot::Sidebar),
            Some("kills")
        );
        assert_eq!(board.score("kills", "Alice").map(|e| e.value), Some(7));
    }

    /// The negative control for the above: an event no session system claims
    /// must leave every component alone, so "the component changed" is
    /// actually discriminating rather than true of any schedule run.
    #[test]
    fn an_unclaimed_event_changes_nothing() {
        let (mut app, entity) = session_app();
        let before = app.world().get::<SessionScoreboard>(entity).unwrap().clone();
        fold(&mut app, ClientEvent::KeepAlive { id: 7 });
        let after = app.world().get::<SessionScoreboard>(entity).unwrap();
        assert_eq!(&before, after);
        assert!(
            !handles_event(&ClientEvent::KeepAlive { id: 7 }),
            "…and the routing switch must agree, or the event is silently dropped"
        );
    }

    /// `PlayerListRemove` must actually remove. The fold this replaces
    /// (`Inner::apply`'s `PlayerListUpdate` arm) had **no** remove arm at all,
    /// so a player who left the server stayed in the read-model forever.
    #[test]
    fn a_player_who_leaves_is_removed_from_the_tab_list() {
        let (mut app, entity) = session_app();
        let alice = Uuid::from_u128(1);
        fold(
            &mut app,
            ClientEvent::PlayerListUpdate {
                entries: vec![PlayerListEntry {
                    uuid: alice,
                    name: Some("Alice".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(12),
                    display_name: None,
                    listed: Some(true),
                }],
            },
        );
        assert_eq!(app.world().get::<SessionTabList>(entity).unwrap().0.len(), 1);

        fold(
            &mut app,
            ClientEvent::PlayerListRemove {
                profile_ids: vec![alice],
            },
        );
        assert_eq!(
            app.world().get::<SessionTabList>(entity).unwrap().0.len(),
            0,
            "the old fold never removed anyone; this is the regression guard"
        );
    }

    /// Boss bars reach a component at all — `BossBarSet::apply` was complete
    /// and unit-tested with zero production callers before this stage.
    #[test]
    fn a_boss_bar_add_reaches_the_component_in_render_order() {
        let (mut app, entity) = session_app();
        for (n, title) in [(1u128, "First"), (2, "Second")] {
            fold(
                &mut app,
                ClientEvent::BossBarUpdate {
                    id: Uuid::from_u128(n),
                    action: BossAction::Add {
                        title: Box::new(Text::literal(title)),
                        progress: 0.5,
                        color: BossColor::Red,
                        overlay: BossOverlay::Progress,
                        darken: false,
                        music: false,
                        fog: false,
                    },
                },
            );
        }
        let bars = &app.world().get::<SessionBossBars>(entity).unwrap().0;
        let titles: Vec<String> = bars
            .iter()
            .map(|(_, bar)| bar.title.to_plain_string())
            .collect();
        assert_eq!(titles, vec!["First".to_string(), "Second".to_string()]);
    }

    /// The HUD overlays must be aged by the 20 Hz schedule, not by a driver
    /// calling `tick` by hand — otherwise the component set is authoritative
    /// but inert.
    #[test]
    fn a_game_tick_run_expires_the_action_bar() {
        let mut app = App::new();
        app.add_plugins(SessionHudPlugin);
        let entity = app.world_mut().spawn(LocalPlayer).id();
        insert_hud_components(app.world_mut(), entity);

        app.world_mut()
            .get_mut::<ActionBarOverlay>(entity)
            .unwrap()
            .0
            .set(Text::literal("hi"));
        assert!(app.world().get::<ActionBarOverlay>(entity).unwrap().0.text().is_some());

        for _ in 0..lodestone_game::player_state::ActionBar::DISPLAY_TICKS {
            app.world_mut().run_schedule(GameTick);
        }
        assert!(
            app.world()
                .get::<ActionBarOverlay>(entity)
                .unwrap()
                .0
                .text()
                .is_none(),
            "60 GameTick runs must expire a vanilla action bar"
        );
    }

    /// The shape **production** uses: `new_ingest_handle`, i.e. `IngestPlugin`
    /// *and* `SessionPlugin` on one `World`.
    ///
    /// This exists because the session tests above are a closed loop over
    /// `SessionPlugin` alone, and that loop was green while the real
    /// configuration folded **nothing**: both plugins used to register
    /// `drain_ingest_queue`, the second copy cleared the batch the first had
    /// filled, and every `Apply` system saw an empty slice. A crate's own tests
    /// being green while the crate is inert is the defect class `CLAUDE.md`
    /// names, and this is its detector.
    #[test]
    fn the_real_both_plugin_world_still_folds_a_scoreboard() {
        let handle = crate::new_ingest_handle();
        let entity = spawn_session(&mut handle.write());
        {
            let mut world = handle.write();
            world.resource_mut::<crate::ingest::IngestQueue>().push(ClientEvent::DisplayObjective {
                slot: DisplaySlot::Sidebar,
                objective: Some("kills".into()),
            });
            world.run_schedule(NetIngest);
        }
        assert_eq!(
            handle
                .read()
                .get::<SessionScoreboard>(entity)
                .unwrap()
                .0
                .displayed(lodestone_game::scoreboard::DisplaySlot::Sidebar),
            Some("kills"),
            "the entity+session World must fold session events, not swallow them"
        );
    }

    /// …and the same `World` must still fold an *entity* event, so the fix for
    /// the double drain cannot have been "drop one of the plugins' systems".
    #[test]
    fn the_real_both_plugin_world_still_folds_an_entity_spawn() {
        use lodestone_model::{ResourceKey, Vec3};
        use std::str::FromStr;

        let handle = crate::new_ingest_handle();
        {
            let mut world = handle.write();
            world.resource_mut::<crate::ingest::IngestQueue>().push(ClientEvent::EntitySpawned {
                entity_id: 5,
                uuid: None,
                entity_type: ResourceKey::from_str("minecraft:pig").unwrap(),
                pos: Vec3::new(1.0, 2.0, 3.0),
                rotation: lodestone_model::Rotation::default(),
                velocity: None,
            });
            world.run_schedule(NetIngest);
        }
        assert!(
            handle
                .read()
                .resource::<crate::entity::EntityIndex>()
                .get(5)
                .is_some()
        );
    }

    /// **Exactly one system writes each session component.**
    ///
    /// `docs/bevy-migration.md` Stage 3 asks for this specifically, because the
    /// stage exists to delete a duplicate fold and the cheapest way to grow a new
    /// one is two systems writing one component with no ordering between them.
    /// `ambiguity_detection: LogLevel::Error` turns that into a schedule *build
    /// failure* rather than a race whose outcome depends on registration order.
    ///
    /// azalea logs the same check at `Warn` (`AmbiguityLoggerPlugin`,
    /// `azalea-client/src/client.rs:246-262`); an error is right here because the
    /// invariant is the point of the stage, not a diagnostic.
    #[test]
    fn exactly_one_system_writes_each_session_component() {
        assert!(
            !net_ingest_is_ambiguous(false),
            "the shipped NetIngest schedule must have no unordered conflicting pair"
        );
    }

    /// The control that proves the detector above works: add a second writer of
    /// [`SessionScoreboard`] in the same set with no ordering, and the build must
    /// fail. Without this, `exactly_one_system_writes_each_session_component`
    /// would pass just as well against a detector that was switched off.
    #[test]
    fn a_second_unordered_scoreboard_writer_fails_the_ambiguity_check() {
        assert!(
            net_ingest_is_ambiguous(true),
            "a second unordered writer of SessionScoreboard must be reported"
        );
    }

    /// Build `SessionPlugin`'s `NetIngest` with ambiguity detection promoted to
    /// an error, optionally adding a rogue second writer first.
    fn net_ingest_is_ambiguous(with_rogue_writer: bool) -> bool {
        use bevy_ecs::schedule::{LogLevel, ScheduleBuildSettings};

        fn rogue(mut boards: Query<&mut SessionScoreboard>) {
            for mut board in &mut boards {
                board.0.remove_objective("anything");
            }
        }

        let mut app = App::new();
        app.add_plugins(SessionPlugin);
        if with_rogue_writer {
            app.add_systems(NetIngest, rogue.in_set(IngestSet::Apply));
        }
        // Deliberately *not* run first: an already-built schedule is not rebuilt,
        // so `initialize` would return `Ok` without ever consulting the new
        // settings — which is exactly how this assertion would go vacuous.
        app.world_mut().schedule_scope(NetIngest, |world, schedule| {
            schedule.set_build_settings(ScheduleBuildSettings {
                ambiguity_detection: LogLevel::Error,
                ..ScheduleBuildSettings::default()
            });
            schedule.initialize(world).is_err()
        })
    }

    /// The negative control for the tick test: without the schedule the same 60
    /// iterations leave the message up, so the assertion above is pinning the
    /// registration and not merely the passage of loop iterations.
    #[test]
    fn without_the_tick_system_the_action_bar_never_expires() {
        let mut app = App::new();
        app.add_plugins(crate::CorePlugin);
        let entity = app.world_mut().spawn(LocalPlayer).id();
        insert_hud_components(app.world_mut(), entity);
        app.world_mut()
            .get_mut::<ActionBarOverlay>(entity)
            .unwrap()
            .0
            .set(Text::literal("hi"));
        for _ in 0..lodestone_game::player_state::ActionBar::DISPLAY_TICKS {
            app.world_mut().run_schedule(GameTick);
        }
        assert!(
            app.world()
                .get::<ActionBarOverlay>(entity)
                .unwrap()
                .0
                .text()
                .is_some()
        );
    }
}
