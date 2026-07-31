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
//! # Two halves, and what §4.1(c) did to the split
//!
//! Stage 3 split this module in two because the process held three `World`s and a
//! component in one is invisible to a system in another:
//!
//! | half | lived in which `World` |
//! |---|---|
//! | [`SessionScoreboard`] / [`SessionTabList`] / [`SessionBossBars`] / [`SessionMenus`] | the **net thread's** — both the bot API and the shell HUD read them, and only that `World` was reachable from `ClientHandle` |
//! | [`Phase`] / [`Vitals`] / [`Xp`] / [`TitleOverlay`] / [`ActionBarOverlay`] / [`HudEffects`] / [`RespawnCount`] / [`ServerEntityId`] | the **driver's** — nothing else folds them, and per-tick driver logic reads them |
//!
//! [`Vitals`], [`Xp`] and [`ServerEntityId`] have since moved to the **shared**
//! half (below), because the rule is *the fold lives where the readers are
//! shared* and `ClientHandle::health`/`food`/`experience_*`/`player` must work
//! with no shell attached at all. The driver half is now [`Phase`], the two
//! overlays, [`HudEffects`], [`RespawnCount`] and [`SessionChat`] — everything
//! whose only reader is the driver.
//!
//! **There is one `World` now** (§4.1(c), `docs/world-unification.md`), and in the
//! shell both halves hang off the *same* entity: `Sim::build` calls
//! `spawn_local_player`, then [`insert_hud_components`], then
//! [`insert_session_components`] on one [`LocalPlayer`]. That is not optional —
//! [`spawn_session`] also marks `LocalPlayer`, so two entities in one `World` would
//! give every `With<LocalPlayer>` system two players.
//!
//! The two *plugins* stay separate anyway, and the reason is no longer the `World`
//! boundary: [`SessionPlugin`] is the fold and [`SessionHudPlugin`] is the 20 Hz
//! ageing tick, and a harness that wants one without the other must be able to say
//! so. `lodestone_client::state::SharedState::default` (a bot with no driver) still
//! installs only [`SessionPlugin`], on an entity of its own.
//!
//! # The vitals collapse, and the routing decision it forced
//!
//! `lodestone_client::state::PlayerSnapshot` used to hold its own `health`,
//! `food`, `saturation`, `xp_*`, `entity_id`, `game_mode`, `dimension` and
//! `alive` beside [`Vitals`] / [`Xp`] / [`ServerEntityId`] here. Stages 2 and 3
//! bounded that residue by "the §4.1 `World` unification"; §4.1(c) shipped and it
//! was **still** duplicated, because `SharedState::apply` routes each event to
//! *exactly one* of two folds and `Login`/`HealthChanged`/`Respawned`/`Death`
//! each carry vitals **and** `dimension`/`game_mode`/`alive`. Claiming one of
//! them for a system here would have stopped the scalar fold seeing it and frozen
//! `dimension` — the too-bright-Nether bug, reached by traversal.
//!
//! The resolution keeps the exclusive routing and moves the rest of the fold
//! here: [`ServerGameMode`], [`ServerDimension`] and [`ServerAlive`] join the
//! set, [`apply_local_player_state`] is the single fold, and `PlayerSnapshot`
//! becomes **derived** from these components the way `EntityView` has been
//! derived from the entity set since Stage 1. Weakening the routing to run both
//! folds was the alternative and it was rejected: it would have left one event
//! with two folds writing two copies of `dimension`, which is the defect class
//! this whole module exists to delete. What is left in
//! `lodestone_client::state` is the **local echo** of our own outbound movement
//! (position/rotation/`on_ground`) and nothing else.
//!
//! `alive` and the shell's `Dead` marker are **not** the same fact and did not
//! merge: [`ServerAlive`] also tracks `health > 0.0`, while `Dead` is inserted
//! only on the death packet, removed only on respawn, and gated on a live-gate
//! test switch (`Sim.recover_from_death`). Merging them would quietly delete a
//! negative control. See [`ServerAlive`]'s own docs.
//!
//! See the Stage 3 doc for the third implementation this leaves standing
//! (`lodestone_game::player_state::HudState`) and what blocks adopting it.

use bevy_app::{App, Plugin};
use bevy_ecs::component::Component;
use bevy_ecs::prelude::{Query, Res, With};
use bevy_ecs::schedule::{IntoScheduleConfigs, SystemSet};
use bevy_ecs::world::World;
use lodestone_model::{ClientEvent, DimensionId, GameMode};

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

/// Server-reported vitals: the local player's health, food and saturation.
///
/// `Option` rather than a value with a default, and that is load-bearing:
/// `None` means *the server has not reported this*, which is how the offline
/// fixture world draws no health bar at all rather than a full one, and it is
/// what `lodestone_client::state::PlayerSnapshot::health_known` is derived from.
/// `lodestone_game::player_state::HudState` — the canonical aggregate — has no
/// such bit, which is why this stage does not adopt it (see the Stage 3 doc).
///
/// All three fields arrive on one `set_health` packet and are written together;
/// a half-populated `Vitals` is not a state the fold can produce.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct Vitals {
    /// Health in `0..=20`, or `None` before the first `set_health`.
    pub health: Option<f32>,
    /// Food level in `0..=20`, or `None` before the first `set_health`.
    pub food: Option<i32>,
    /// Food saturation, or `None` before the first `set_health`.
    ///
    /// No reader draws this today — it is here because
    /// `PlayerSnapshot::saturation` is a public bot-API field and dropping it in
    /// the collapse would have been a silent API regression, not a cleanup.
    pub saturation: Option<f32>,
    /// Current air supply in ticks (`0..=300`), or `None` before the first
    /// entity-metadata update naming our own id arrives.
    ///
    /// Unlike `health`/`food`/`saturation`, this does **not** arrive on
    /// `set_health` — it is `Entity.DATA_AIR_SUPPLY_ID`, a per-entity metadata
    /// field broadcast for any entity (not a session-scoped packet), so it is
    /// folded by [`crate::ingest::apply_local_player_air_supply`] off
    /// `ClientEvent::EntityMetadataUpdated` instead of by
    /// [`apply_local_player_state`] alongside the other three. See
    /// `docs/air-supply.md`.
    pub air: Option<i32>,
}

/// Server-reported experience as `(progress, level, total)`, `None` until
/// `set_experience` arrives.
///
/// The HUD must not substitute a locally-derived guess: there is no vanilla
/// levelling curve the client could invert from partial data that is
/// guaranteed to match a (possibly modded) server's own numbers.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct Xp(pub Option<(f32, i32, i32)>);

/// The server-assigned entity id for the local player, `None` before login.
///
/// Entity-scoped updates that must decide "is this us" (mob effects, most
/// obviously) compare against this rather than guessing, so an id the next
/// session reuses cannot be misattributed.
///
/// This is the *scalar* answer to "which id are we". The **index** answer —
/// `crate::entity::EntityIndex` mapping that id to this same entity, so
/// id-addressed ingest (`update_attributes`) can reach the local player's own
/// components — is written by
/// [`crate::ingest::apply_local_player_login`] off the same event. Both are
/// needed and neither derives the other: a `Query` cannot resolve an id without
/// the index, and the index cannot answer "have we logged in yet".
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerEntityId(pub Option<i32>);

/// The local player's game mode as the server last reported it, `None` before
/// login.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerGameMode(pub Option<GameMode>);

/// The dimension the local player is currently in, `None` before login.
///
/// **Updated on `Respawned` as well as `Login`**, because `Respawned` is how the
/// server reports portal travel and not only death. A fold that only handled
/// `Login` froze this at whatever the player logged into, which reintroduced the
/// too-bright-Nether bug by traversal — see
/// `lodestone-client/tests/read_model.rs`'s
/// `respawning_into_another_dimension_updates_the_read_model`, which is that
/// regression's gate and now reads this component through
/// `PlayerSnapshot::dimension`.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct ServerDimension(pub Option<DimensionId>);

/// Whether the **server** considers the local player alive.
///
/// Defaults to `true`: a client that has not been told otherwise is alive, and a
/// `false` default would make every pre-login read report a dead player.
///
/// # Not the same fact as `crate::player::Dead`, and they must not merge
///
/// | | `ServerAlive` | [`Dead`](crate::player::Dead) |
/// |---|---|---|
/// | set false by | `Death`, **and** any `HealthChanged` with `health <= 0` | `Death` only |
/// | set true by | `Login`, `Respawned`, any `HealthChanged` with `health > 0` | removed by `Respawned` only |
/// | gated on a test switch | no | **yes** — `Sim.recover_from_death` |
/// | who reads it | the bot API (`ClientHandle::is_alive`) | the driver: it freezes movement for a tick |
///
/// That last row is why merging them deletes evidence rather than duplication:
/// flipping `recover_from_death` off is the live death gate's **negative
/// control**, reproducing "stranded on the death screen forever". A merged
/// marker has nowhere for that switch to live.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerAlive(pub bool);

impl Default for ServerAlive {
    fn default() -> Self {
        Self(true)
    }
}

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
            // the local player's server-reported state
            | ClientEvent::Login { .. }
            | ClientEvent::Respawned { .. }
            | ClientEvent::HealthChanged { .. }
            | ClientEvent::Death { .. }
            | ClientEvent::ExperienceChanged { .. }
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

/// `IngestSet::Apply`: the local player's own server-reported state →
/// [`Vitals`], [`Xp`], [`ServerEntityId`], [`ServerGameMode`],
/// [`ServerDimension`], [`ServerAlive`].
///
/// # Why this is one system over five event families and not five systems
///
/// `ServerAlive` is written by **four** of them (`Login`, `Respawned`,
/// `HealthChanged`, `Death`) under one rule. Split across systems that rule lives
/// in four places and the next person to touch one of them has no way to see the
/// other three; kept here it is a single readable `match`. The cost is that this
/// system claims six components, which is fine — the invariant
/// `exactly_one_system_writes_each_session_component` asks for one *writer* per
/// component, not one component per writer.
///
/// This replaces `lodestone_client::state::Inner::apply`'s `Login`,
/// `Respawned`, `HealthChanged`, `Death` and `ExperienceChanged` arms, which are
/// **deleted**; `PlayerSnapshot` is derived from these components now. `Inner`
/// keeps only `TeleportPlayer`, which is not a fold of the server's view at all
/// but a local echo of our own outbound movement.
pub fn apply_local_player_state(
    batch: Res<IngestBatch>,
    mut players: Query<
        (
            &mut Vitals,
            &mut Xp,
            &mut ServerEntityId,
            &mut ServerGameMode,
            &mut ServerDimension,
            &mut ServerAlive,
        ),
        With<LocalPlayer>,
    >,
) {
    for event in batch.events() {
        for (mut vitals, mut xp, mut id, mut game_mode, mut dimension, mut alive) in &mut players {
            match event {
                ClientEvent::Login {
                    entity_id,
                    game_mode: mode,
                    dimension: dim,
                } => {
                    id.0 = Some(*entity_id);
                    game_mode.0 = Some(*mode);
                    dimension.0 = Some(dim.clone());
                    alive.0 = true;
                }
                // `Respawned` is *also* how the server reports portal travel, not
                // only death — see [`ServerDimension`].
                ClientEvent::Respawned {
                    dimension: dim,
                    game_mode: mode,
                    ..
                } => {
                    dimension.0 = Some(dim.clone());
                    game_mode.0 = Some(*mode);
                    alive.0 = true;
                }
                ClientEvent::HealthChanged {
                    health,
                    food,
                    saturation,
                } => {
                    vitals.health = Some(*health);
                    vitals.food = Some(*food);
                    vitals.saturation = Some(*saturation);
                    // Health reaching zero is not a session event and does *not*
                    // insert `crate::player::Dead`; see [`ServerAlive`].
                    alive.0 = *health > 0.0;
                }
                ClientEvent::Death { .. } => alive.0 = false,
                ClientEvent::ExperienceChanged {
                    progress,
                    level,
                    total,
                } => xp.0 = Some((*progress, *level, *total)),
                _ => {}
            }
        }
    }
}

/// Insert the shared-fold session component set onto `entity`.
///
/// Every component is inserted eagerly, like [`crate::spawn_local_player`] and
/// unlike the *observed*-entity set: an empty scoreboard is a real state ("the
/// server has sent no objectives"), not an unknown one, so there is no
/// three-state encoding to preserve. The "has the server reported this yet" bit
/// the vitals *do* need lives inside them, as `Option`, rather than as component
/// absence — see [`Vitals`].
///
/// **This is also the reset path**, the same way [`insert_hud_components`] is:
/// `lodestone_shell::sim::Sim::end_session` calls both, so a component added here
/// cannot be missed by a quit-to-title. Before the vitals collapse this function
/// was spawn-only, and the note that said a teardown "need not clear the tab
/// list, scoreboard, boss bars or menus" went stale the moment §4.1(c) merged the
/// two `World`s: the reader is `Sim.local` now, not a `World` that goes away with
/// the connection, so those really did survive a quit-to-title.
pub fn insert_session_components(world: &mut World, entity: bevy_ecs::entity::Entity) {
    if let Ok(mut entity) = world.get_entity_mut(entity) {
        entity.insert((
            SessionScoreboard::default(),
            SessionTabList::default(),
            SessionBossBars::default(),
            SessionMenus::default(),
            Vitals::default(),
            Xp::default(),
            ServerEntityId::default(),
            ServerGameMode::default(),
            ServerDimension::default(),
            ServerAlive::default(),
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
                apply_local_player_state,
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

/// The received chat/system scrollback, with each line's arrival time.
///
/// # Why this arrives in Stage 5 and not Stage 3
///
/// Stage 3 moved every other session aggregate and deferred this one explicitly:
/// every push needs a monotonic client clock and every read needs it again to age
/// the line for the vanilla fade-out, so a component here while the clock stayed
/// a `Sim` field would have put a *second* clock in the process — the exact
/// failure the authority test exists to catch. The clock is now
/// [`crate::FrameClock`] and they moved together.
///
/// The driver's ingest stamps each line with `FrameClock::secs` and the HUD reads
/// `ChatLog::recent_ages(n, FrameClock::secs)`. Nothing in this crate reads the
/// clock for it: which frame a line belongs to is the driver's fact, not a fold's.
#[derive(Component, Debug, Clone, Default)]
pub struct SessionChat(pub lodestone_game::chat::ChatLog);

/// `TickSet::Animate`: age the three self-expiring HUD overlays one tick.
///
/// **Must stay in the fixed 20 Hz schedule.** Every duration here is counted in
/// *ticks* — vanilla's action bar is 60 ticks, a title's fade is `TitleTimes`
/// ticks, an effect's remaining duration is ticks — so ageing them per frame
/// makes each one frame-rate dependent: an action bar would vanish twice as
/// fast at 120 fps as at 60.
pub fn tick_hud_overlays(
    mut players: Query<
        (&mut TitleOverlay, &mut ActionBarOverlay, &mut HudEffects),
        With<LocalPlayer>,
    >,
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
            TitleOverlay::default(),
            ActionBarOverlay::default(),
            HudEffects::default(),
            RespawnCount::default(),
            // Stage 5. Reset with the rest rather than by hand in the driver's
            // teardown: the old `Sim::end_session` cleared `chat_log` on its own
            // line, which is exactly the shape that gets missed when a component
            // is added later.
            SessionChat::default(),
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

    fn dim(path: &str) -> DimensionId {
        format!("minecraft:{path}")
            .parse()
            .expect("valid dimension id")
    }

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
        let before = app
            .world()
            .get::<SessionScoreboard>(entity)
            .unwrap()
            .clone();
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
        assert_eq!(
            app.world().get::<SessionTabList>(entity).unwrap().0.len(),
            1
        );

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
        assert!(
            app.world()
                .get::<ActionBarOverlay>(entity)
                .unwrap()
                .0
                .text()
                .is_some()
        );

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
            world.resource_mut::<crate::ingest::IngestQueue>().push(
                ClientEvent::DisplayObjective {
                    slot: DisplaySlot::Sidebar,
                    objective: Some("kills".into()),
                },
            );
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
            world
                .resource_mut::<crate::ingest::IngestQueue>()
                .push(ClientEvent::EntitySpawned {
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

    // ---- the local player's server-reported state -------------------------

    /// The whole vitals collapse in one test: five event families, six
    /// components, one fold, reached **through the schedule**.
    #[test]
    fn the_local_players_server_state_folds_onto_components() {
        let (mut app, entity) = session_app();

        // Everything is unknown before the server says anything — the state the
        // offline fixture world draws no bars from.
        {
            let world = app.world();
            assert_eq!(world.get::<Vitals>(entity).unwrap().health, None);
            assert_eq!(world.get::<Xp>(entity).unwrap().0, None);
            assert_eq!(world.get::<ServerEntityId>(entity).unwrap().0, None);
            assert_eq!(world.get::<ServerGameMode>(entity).unwrap().0, None);
            assert_eq!(world.get::<ServerDimension>(entity).unwrap().0, None);
            assert!(
                world.get::<ServerAlive>(entity).unwrap().0,
                "a client nobody has told otherwise is alive"
            );
        }

        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 7,
                game_mode: GameMode::Creative,
                dimension: dim("overworld"),
            },
        );
        fold(
            &mut app,
            ClientEvent::HealthChanged {
                health: 18.0,
                food: 15,
                saturation: 2.5,
            },
        );
        fold(
            &mut app,
            ClientEvent::ExperienceChanged {
                progress: 0.375,
                level: 12,
                total: 289,
            },
        );

        let world = app.world();
        let vitals = *world.get::<Vitals>(entity).unwrap();
        assert_eq!(vitals.health, Some(18.0));
        assert_eq!(vitals.food, Some(15));
        assert_eq!(vitals.saturation, Some(2.5));
        assert_eq!(world.get::<Xp>(entity).unwrap().0, Some((0.375, 12, 289)));
        assert_eq!(world.get::<ServerEntityId>(entity).unwrap().0, Some(7));
        assert_eq!(
            world.get::<ServerGameMode>(entity).unwrap().0,
            Some(GameMode::Creative)
        );
        assert_eq!(
            world.get::<ServerDimension>(entity).unwrap().0,
            Some(dim("overworld"))
        );
    }

    /// `Respawned` moves the dimension, because it is how the server reports a
    /// **portal trip** and not only a death. A fold that only handled `Login`
    /// froze `dimension` at whatever the player logged into — the too-bright-Nether
    /// bug reached by traversal.
    ///
    /// The `Login` assertion is the control: it proves the field is genuinely
    /// rewritten rather than having started out as the expected value.
    #[test]
    fn a_respawn_moves_the_dimension_and_revives() {
        let (mut app, entity) = session_app();
        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 7,
                game_mode: GameMode::Survival,
                dimension: dim("overworld"),
            },
        );
        assert_eq!(
            app.world().get::<ServerDimension>(entity).unwrap().0,
            Some(dim("overworld"))
        );

        fold(
            &mut app,
            ClientEvent::Death {
                message: Text::literal("you died"),
            },
        );
        assert!(!app.world().get::<ServerAlive>(entity).unwrap().0);

        fold(
            &mut app,
            ClientEvent::Respawned {
                dimension: dim("the_nether"),
                game_mode: GameMode::Adventure,
                previous_game_mode: Some(GameMode::Survival),
                last_death_location: None,
            },
        );
        let world = app.world();
        assert_eq!(
            world.get::<ServerDimension>(entity).unwrap().0,
            Some(dim("the_nether")),
            "a respawn/portal trip must move the dimension"
        );
        assert_eq!(
            world.get::<ServerGameMode>(entity).unwrap().0,
            Some(GameMode::Adventure)
        );
        assert!(
            world.get::<ServerAlive>(entity).unwrap().0,
            "a respawn is exactly when the player stops being dead"
        );
    }

    /// `ServerAlive` tracks `health > 0.0` as well as the death packet, and that
    /// is the *whole* reason it cannot merge with `crate::player::Dead` — which is
    /// set only by `Death`, and only when the driver's `recover_from_death` switch
    /// allows it. Both directions are asserted here, because a fold that only
    /// handled `Death` would pass a one-sided version of this.
    #[test]
    fn zero_health_kills_and_positive_health_revives_without_a_death_packet() {
        let (mut app, entity) = session_app();
        let health = |app: &mut App, health: f32| {
            fold(
                app,
                ClientEvent::HealthChanged {
                    health,
                    food: 0,
                    saturation: 0.0,
                },
            );
        };

        health(&mut app, 0.0);
        assert!(
            !app.world().get::<ServerAlive>(entity).unwrap().0,
            "health reaching zero must clear liveness even with no Death packet"
        );
        health(&mut app, 20.0);
        assert!(
            app.world().get::<ServerAlive>(entity).unwrap().0,
            "…and positive health must restore it, again with no packet"
        );
        assert!(
            !app.world().entity(entity).contains::<crate::player::Dead>(),
            "and none of that may insert the driver's `Dead` marker — health \
             reaching zero is not a session event"
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
        app.world_mut()
            .schedule_scope(NetIngest, |world, schedule| {
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
