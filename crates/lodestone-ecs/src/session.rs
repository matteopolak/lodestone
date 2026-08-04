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
use lodestone_model::{ClientEvent, Difficulty, DimensionId, DimensionTypeInfo, GameMode};

use crate::ingest::{IngestBatch, IngestQueuePlugin};
use crate::player::{LocalPlayer, SelectedSlot};
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

/// The active *other-players'* block-crack overlays, folded from
/// [`ClientEvent::BlockDestruction`].
///
/// `lodestone_game::mining::BlockDestructionOverlays::apply` was one of the
/// three islands `docs/event-routing.md` found with a fold sitting unwired
/// behind them — unit-tested, and consumed nowhere outside its own file and
/// tests. This is the routing fix: the event now reaches a real fold through
/// the ordinary `NetIngest` path, the same per-session collection shape as
/// [`SessionBossBars`]/[`SessionTabList`] above (it is keyed internally by
/// the breaking entity's id, not by *this* session, but there is exactly one
/// copy of it client-side, same as a boss-bar set).
///
/// **Drawing it is a separate piece of work.** The renderer's single-target
/// `CrackTarget`/`CrackPipeline` (`lodestone_shell::gpu`) only ever draws the
/// local player's own dig; painting *other* players' cracks needs that
/// pipeline to accept more than one target, which is a rendering change, not
/// a routing one. [`stage_at`](lodestone_game::mining::BlockDestructionOverlays::stage_at)
/// is the read side ready for whoever picks that up.
#[derive(Component, Debug, Clone, Default)]
pub struct SessionBlockDestruction(pub lodestone_game::mining::BlockDestructionOverlays);

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
    /// `docs/sky-and-air-bubbles.md`.
    pub air: Option<i32>,
    /// Whether the player entity is burning, or `None` before the first
    /// entity-metadata update naming our own id arrives.
    ///
    /// Session-scoped for exactly the reason `air` above is, and it is worth
    /// spelling out because the generic path *looks* like it should work:
    /// `apply_entity_metadata` does set `EntityFlags` on the local player's own
    /// ECS entity, but `lodestone_client::state::entity_view` requires
    /// `EntityKind`/`Position`/`Rotation`/`HeadYaw` and the local player
    /// deliberately has none of them — that absence is what keeps a self-model
    /// out of `ClientHandle::entities()` and rendering at the camera's own eye.
    /// So `entity_view()`'s early `?` returns before `flags` is ever read, and
    /// the flag can only reach the session through a dedicated fold
    /// ([`crate::ingest::apply_local_player_on_fire`]).
    ///
    /// `None` reads as "not burning" downstream — the safe default, unlike
    /// `air`'s, which reads as full. See `docs/screen-overlays.md`.
    pub on_fire: Option<bool>,
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
///
/// Written on `Login`, `Respawned` **and** `GameModeChanged` — the last of which
/// is how a runtime `/gamemode` reaches us. Without that arm this froze at
/// whatever the player logged in as, which is the same stale-value shape
/// [`ServerDimension`] documents for portal travel.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerGameMode(pub Option<GameMode>);

/// The world difficulty as the server last reported it, plus whether it is
/// locked from further changes in the options UI
/// (`ClientboundChangeDifficultyPacket` / [`ClientEvent::DifficultyChanged`]).
///
/// `None` before the first report — one of the two `HudState`-shaped islands
/// this table found: `HudState::apply` folded this correctly and was
/// unit-tested, but `HudState` has no production caller (see this module's
/// note on the Stage 3 collapse), so the event reached nothing at all until
/// this component. The pre-report state is represented honestly rather than
/// guessed as `Normal`, the same convention [`Vitals`]/[`ServerGameMode`] use.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerDifficulty(pub Option<(Difficulty, bool)>);

/// The local player's server-granted **abilities** — `Abilities.Packed` on the
/// wire, `ClientboundPlayerAbilitiesPacket`.
///
/// # Why this exists at all: it was a complete island
///
/// `ClientEvent::AbilitiesChanged` was decoded correctly at
/// `crates/protocol/v770/src/adapter.rs:3301`, unit-tested at the protocol layer,
/// round-tripped in `lodestone-model`'s own tests — and consumed **nowhere**.
/// `grep -c AbilitiesChanged` returned `0` in both this crate's `ingest.rs` and
/// the shell's `sim.rs`. That is the exact defect class `CLAUDE.md` §1 names, and
/// the routing switch ([`handles_event`]) is its usual factory: without an arm
/// there, `SharedState::apply` never forwards the event and a perfect decode plus
/// a correct system still reaches zero pixels.
///
/// The consequence was **player-visible and not merely missing**: `Flying` (the
/// debug free-fly camera) was a purely local toggle with no relationship to
/// [`Self::may_fly`], so the client would happily free-cam on a server that never
/// granted flight. Whether a player *may* fly is server authority.
///
/// # `flying` is state, `may_fly` is permission
///
/// They are separate wire bits and must not be collapsed. `may_fly` gates the
/// client's double-tap toggle (`LocalPlayer.aiStep`, `LocalPlayer.java:825`);
/// `flying` is whether flight is engaged right now, and the server both reports
/// it and accepts our echo of it (`ServerboundPlayerAbilities`). A client that
/// sets `flying` without `may_fly` desyncs and gets corrected.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Abilities {
    /// `Abilities.invulnerable`. Not read by physics — recorded because it
    /// arrives on the same packet and dropping a field silently is how the next
    /// consumer discovers it is missing.
    pub invulnerable: bool,
    /// `Abilities.flying` — flight engaged **right now**, as the server last
    /// reported (or last accepted from) us. Fed straight to
    /// `lodestone_physics::PlayerState::flying`.
    pub flying: bool,
    /// `Abilities.mayfly` — the server **permits** flight. This is the gate: the
    /// double-tap toggle does nothing without it.
    pub may_fly: bool,
    /// `Abilities.instabuild` (creative-mode instant break).
    pub instabuild: bool,
    /// `Abilities.flyingSpeed`, default `0.05F`. Servers do change it, so it is
    /// carried rather than assumed — see
    /// `lodestone_physics::PlayerState::flying_speed`.
    pub flying_speed: f32,
    /// `Abilities.walkingSpeed`, default `0.1F`.
    ///
    /// **Deliberately not fed to physics.** Walk speed reaches movement through
    /// the `minecraft:movement_speed` *attribute* (`crate::player::player_physics`
    /// reads it from [`crate::entity::Attributes`]), which is where the server
    /// folds Speed, Slowness, Soul Speed and boot enchantments. This field is the
    /// abilities packet's own copy and applying it too would double-count.
    pub walking_speed: f32,
}

impl Default for Abilities {
    /// `Abilities.java`'s field initialisers: everything off, `0.05F` flying and
    /// `0.1F` walking speed.
    ///
    /// **`flying: false` and `may_fly: false` are the load-bearing defaults**: a
    /// client that has not been told it may fly, may not fly. A `true` default
    /// would reintroduce exactly the bug this component closes.
    fn default() -> Self {
        Self {
            invulnerable: false,
            flying: false,
            may_fly: false,
            instabuild: false,
            flying_speed: 0.05,
            walking_speed: 0.1,
        }
    }
}

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

/// The **dimension type** the local player's dimension points at, as the server
/// declared it in the Configuration `registry_data` (issue #288). `None` before
/// login, and `None` on a server whose registry did not resolve.
///
/// # Why this is not derivable from [`ServerDimension`]
///
/// [`ServerDimension`] holds a *level* id; this holds the registry entry that
/// level's geometry and lighting rules come from. Deriving one from the other is
/// the name match issue #34 filed: a data pack can point `mypack:mine` at the
/// vanilla overworld type, or give `minecraft:overworld` a 1024-tall custom type.
/// The two are folded together in [`apply_local_player_state`] off two events
/// the adapter emits back to back, so they can never disagree about *when* they
/// moved.
///
/// **`None` must not be read as "the overworld".** It means the server said
/// nothing usable, and every consumer has to state its own fallback — see
/// `lodestone_shell::mesher::sky_default_for_dimension`, which keeps its
/// pre-#288 name match for exactly this case.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct ServerDimensionType(pub Option<DimensionTypeInfo>);

/// Every biome's `minecraft:visual/sky_color` as the server declared it in the
/// Configuration `registry_data`, **indexed by biome holder id** (issue #96).
///
/// Packed `0x00RR_GGBB` in sRGB bytes; `None` at a holder id whose biome
/// declares no sky colour (the Nether and End biomes) or whose entry did not
/// parse. Empty before login and on any server that sent no biome registry —
/// which reads as "tint nothing", the honest fallback, never as a plausible
/// overworld blue.
///
/// # Why an `Arc`, and why the whole table rather than one colour
///
/// The table is read once per frame by the shell, which resolves the *standing*
/// biome from the chunk section under the camera — a value that changes as the
/// player walks and which nothing on the network announces. So the lookup has to
/// happen at the camera, and the table has to be there for it. `Arc` because
/// `PlayerSnapshot` clones it every frame and this is ~66 entries; the clone is a
/// refcount bump.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct ServerBiomeSkyColors(pub std::sync::Arc<[Option<u32>]>);

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

/// The server entity id of the vehicle the **local player** is riding, or `None`
/// when on foot — vanilla's `Entity.getVehicle()` for our own player.
///
/// # Why this is session state and not just [`crate::entity::Vehicle`]
///
/// `SET_PASSENGERS` is one packet feeding two disjoint facts, and the fork is the
/// one `CLAUDE.md` warns costs work when guessed:
///
/// | fact | home | why |
/// |---|---|---|
/// | which entity rides which | [`crate::entity::Passengers`]/[`crate::entity::Vehicle`], `ingest` | per-entity ECS state, keyed by server id |
/// | **am I riding, and what** | this component, `session` | a local-player scalar that drives the camera, physics and input |
///
/// Both routers therefore claim the event, exactly as both claim `Login`.
///
/// The local player is a real member of `EntityIndex` (see
/// [`crate::ingest::apply_local_player_login`]) so `Vehicle` *is* also written on
/// our own entity — but it cannot substitute for this. The local player carries no
/// [`crate::entity::Position`]/[`crate::entity::EntityKind`] by design, so it is
/// structurally excluded from the entity read-model, and every consumer that needs
/// "are we mounted" ([`crate::player::player_physics`]'s seat pin,
/// `lodestone_shell::sim`'s dismount key, the camera) is a local-player consumer
/// reaching for a scalar, not an id-addressed query. Deriving it would also mean
/// depending on the *reverse-edge* fold, which
/// [`crate::ingest::apply_entity_passengers`] documents as best-effort for an
/// unspawned id; this fold reads the **forward** list and is therefore exact.
///
/// # `None` is "on foot", never "unknown"
///
/// There is no unreported case: a rider is always announced by the packet that
/// seats it, and a dismount is announced as that same packet's list going empty.
/// So the default is a real, correct state and consumers need no fallback.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Riding(pub Option<i32>);

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
/// The caller-side routing switch, for the same reason
/// [`crate::ingest::handles_event`] is: an event routed to the ECS that no system
/// folds vanishes silently.
///
/// # This used to be the list, and `#[non_exhaustive]` made the list a trap
///
/// It was a `matches!` over ~25 variants. Because `ClientEvent` is
/// `#[non_exhaustive]`, no `matches!` outside `lodestone-model` can be exhaustive,
/// so a variant nobody remembered returned `false` here *and* `false` in
/// `crate::ingest::handles_event` and reached nothing at all —
/// `DimensionTypeChanged` and `AbilitiesChanged` each shipped that way with a
/// correct, tested decode behind them.
///
/// The list is now [`lodestone_model::event::route`], where the match is
/// exhaustive and an unrouted variant does not compile. This function is one line
/// so the predicate cannot drift from the table.
///
/// # The fork that has cost work twice
///
/// **Per-entity state is `ingest`, local-player scalars are `session`**, and
/// block/world events are neither — they travel the shell's own stream. The table
/// carries that convention as a comment directly above the match, which is where
/// the decision is actually made.
///
/// The [`SessionMenus`] family is claimed variant by variant rather than delegated
/// to `Menus::apply`, because the route table must answer without a `&mut Menus`
/// to hand it. Keep the two in step: an arm added to `Menus::apply` and forgotten
/// in the table never reaches the ECS at all.
#[must_use]
pub fn handles_event(event: &ClientEvent) -> bool {
    lodestone_model::event::route(event).session
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

/// `IngestSet::Apply`: `BlockDestruction` → [`SessionBlockDestruction`].
pub fn apply_block_destruction(
    batch: Res<IngestBatch>,
    mut overlays: Query<&mut SessionBlockDestruction>,
) {
    for event in batch.events() {
        for mut set in &mut overlays {
            let _ = set.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: the local player's own server-reported state →
/// [`Vitals`], [`Xp`], [`ServerEntityId`], [`ServerGameMode`],
/// [`ServerDimension`], [`ServerDimensionType`], [`ServerBiomeSkyColors`],
/// [`ServerAlive`], [`Abilities`], [`ServerDifficulty`],
/// [`SelectedSlot`](crate::player::SelectedSlot).
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
            &mut ServerDimensionType,
            &mut ServerBiomeSkyColors,
            &mut ServerAlive,
            &mut Abilities,
            &mut Riding,
            &mut ServerDifficulty,
            // `Option`, not required: `crate::player::SelectedSlot` is inserted
            // by `spawn_local_player`, not by `insert_session_components`, so a
            // harness that installs `SessionPlugin` alone (`spawn_session`, with
            // no player-input components at all) must not stop every *other*
            // field in this query from folding — the same reasoning
            // `tick_hud_overlays` documents for its own `Option<&SelectedSlot>`.
            Option<&mut SelectedSlot>,
        ),
        With<LocalPlayer>,
    >,
) {
    for event in batch.events() {
        for (
            mut vitals,
            mut xp,
            mut id,
            mut game_mode,
            mut dimension,
            mut dimension_type,
            mut biome_sky_colors,
            mut alive,
            mut abilities,
            mut riding,
            mut difficulty,
            mut selected_slot,
        ) in &mut players
        {
            match event {
                // Issue #288. Emitted immediately *before* `Login`/`Respawned`
                // by the adapter, off the same packet's dimension-type holder id.
                //
                // Assigned unconditionally, `None` included: an unresolvable
                // dimension must **clear** the previous one, or a portal trip
                // into a custom dimension would keep reporting the overworld's
                // `has_skylight` — the stale-value failure mode that is worse
                // than an honest `None`.
                ClientEvent::DimensionTypeChanged {
                    dimension_type: info,
                    ..
                } => {
                    dimension_type.0 = info.clone();
                }
                // Issue #96. Assigned unconditionally for the same reason the
                // arm above is: an empty table must **clear** the previous one.
                // A server switch that sends a registry set without biomes has
                // to stop tinting, not keep painting the last world's sky.
                ClientEvent::BiomeVisuals { sky_colors } => {
                    biome_sky_colors.0 = sky_colors.as_slice().into();
                }
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
                    // Issue #390. The two entity-metadata-fed fields go back to
                    // "no reading yet", because a respawn is a **brand-new
                    // player entity on both sides**: `PlayerList.respawn` does
                    // `new ServerPlayer(...)` (`PlayerList.java:393`) and
                    // vanilla's *client* likewise builds a fresh `LocalPlayer`
                    // via `gameMode.createPlayer`
                    // (`ClientPacketListener.handleRespawn`, `:1286`) and only
                    // copies the old id onto it. Its synched data therefore
                    // starts at `Entity`'s own defaults —
                    // `entityDataBuilder.define(DATA_AIR_SUPPLY_ID,
                    // getMaxAirSupply())` (`Entity.java:319`, i.e. 300) and
                    // shared flags 0 — so nothing in the dead entity's last
                    // metadata survives. We keep one long-lived entity instead
                    // of respawning ours, which is exactly why the clear has to
                    // be explicit here.
                    //
                    // `None`, not `Some(300)`/`Some(false)`: `None` is the
                    // documented pre-report state and already reads as full air
                    // and not-burning downstream ([`Vitals::air`],
                    // [`Vitals::on_fire`]), so the row stays hidden until the
                    // server actually says otherwise. Writing a literal here
                    // would invent a reading we were never given.
                    //
                    // Drowning drove `air` to `0` and nothing cleared it, so the
                    // bubble row kept drawing an **empty** meter after respawn
                    // until the server's next metadata arrived with 300 — which
                    // the player sees as an instant refill on touching water.
                    // `on_fire` has the same shape and the quieter polarity: a
                    // stale `Some(true)` leaves the fire overlay painted on a
                    // freshly respawned player.
                    //
                    // Ordering note: `crate::ingest::apply_local_player_air_supply`
                    // is the other writer of these fields and is unordered with
                    // respect to this system. Both orderings converge for a
                    // respawn — a same-batch metadata packet carries the new
                    // entity's full 300 — so the ambiguity is benign here, but do
                    // not extend this arm to a field where it would not be.
                    vitals.air = None;
                    vitals.on_fire = None;
                    // Same "fresh entity on both sides" reasoning, one field over:
                    // vanilla's respawned `ServerPlayer` is never a passenger and
                    // `ServerPlayer.restoreFrom` carries no vehicle across, so a
                    // player who died while riding must land on foot. `None` and
                    // not "leave it alone": the server sends no `SET_PASSENGERS`
                    // for a vehicle it destroyed our seat in, so without this the
                    // seat pin in `crate::player::player_physics` would hold a
                    // respawned player at a boat they are no longer in with no
                    // packet left that could free them.
                    riding.0 = None;
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
                // A runtime `/gamemode`. `Login`/`Respawned` above carry a mode
                // too, so all three writers of `ServerGameMode` sit together.
                ClientEvent::GameModeChanged { game_mode: mode } => game_mode.0 = Some(*mode),
                // #191. Assigned as a **whole record**, never field-by-field:
                // vanilla's `Abilities.apply(Packed)` overwrites every field from
                // one packet, so a server that clears `mayfly` clears it here too.
                // Merging fields would let a stale `may_fly: true` outlive the
                // grant that set it — which is the failure this fold exists to
                // prevent, pointing the same direction as the original island.
                ClientEvent::AbilitiesChanged {
                    invulnerable,
                    flying,
                    can_fly,
                    instabuild,
                    flying_speed,
                    walking_speed,
                } => {
                    *abilities = Abilities {
                        invulnerable: *invulnerable,
                        flying: *flying,
                        may_fly: *can_fly,
                        instabuild: *instabuild,
                        flying_speed: *flying_speed,
                        walking_speed: *walking_speed,
                    };
                }
                // Tier 1 item 8, the session half of `SET_PASSENGERS`. See
                // [`Riding`] for why this fact lives here while the per-entity
                // `Passengers`/`Vehicle` pair lives in `crate::ingest`.
                //
                // **The `else if` is load-bearing.** A `SET_PASSENGERS` for some
                // *other* vehicle — a pig two chunks away gaining a rider — must
                // not clear our own ride state, so absence from the list only means
                // "dismounted" when the list belongs to the vehicle we are
                // currently in. Assigning `None` unconditionally on any list that
                // does not contain us would eject the player from a boat every time
                // an unrelated mob was mounted anywhere in view distance.
                //
                // `id.0` is set by the `Login` arm above, and a `SET_PASSENGERS`
                // cannot precede login, so a `None` here means "before login" and
                // correctly matches nothing.
                ClientEvent::EntityPassengersChanged {
                    vehicle_id,
                    passenger_ids,
                } => {
                    if id.0.is_some_and(|own| passenger_ids.contains(&own)) {
                        riding.0 = Some(*vehicle_id);
                    } else if riding.0 == Some(*vehicle_id) {
                        riding.0 = None;
                    }
                }
                // One of the two `HudState`-shaped islands (see
                // [`ServerDifficulty`]). Assigned as a whole record, same reason
                // `AbilitiesChanged` above is: one packet reports both fields
                // together, so there is no way to receive a stale `locked` next
                // to a fresh `difficulty`.
                ClientEvent::DifficultyChanged { difficulty: d, locked } => {
                    difficulty.0 = Some((*d, *locked));
                }
                // The other. `HudState::select_slot`'s clamp, reproduced here:
                // an out-of-range wire value (negative, or `>= 9`) is ignored
                // rather than corrupting the selection with a value the hotbar
                // has no slot for. `selected_slot` is `None` only on a harness
                // that installs `SessionPlugin` without the player-input
                // component set (see the query's own doc) — on the real client
                // `spawn_local_player` always inserts
                // `SelectedSlot`(crate::player::SelectedSlot) first.
                ClientEvent::HeldSlotChanged { slot } => {
                    if let Some(sel) = &mut selected_slot {
                        if let Ok(s) = u8::try_from(*slot) {
                            if s < 9 {
                                sel.0 = s as usize;
                            }
                        }
                    }
                }
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
            ServerDimensionType::default(),
            ServerBiomeSkyColors::default(),
            ServerAlive::default(),
            Abilities::default(),
            // Also the quit-to-title reset path (see this function's docs): a new
            // session must start on foot, never still seated in the last one's
            // boat.
            Riding::default(),
        ));
        // A second `insert` call: `Bundle` tuple impls stop at arity 15 and the
        // set above is already at that ceiling, not a meaningful grouping.
        entity.insert((ServerDifficulty::default(), SessionBlockDestruction::default()));
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

/// Registers the shared-fold half: the session components' `NetIngest`
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
                apply_block_destruction,
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

/// The held-item name highlight (issue #126): vanilla's `Hud.tick`
/// (`Hud.java:1190-1203`) timer for the label that appears above the hotbar
/// when the selected item's *identity* changes. See
/// [`lodestone_game::player_state::HeldItemHighlight`]'s own doc for why this
/// is keyed on item id + hover name rather than slot index — switching
/// between two slots holding the same item must not retrigger it.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct HeldItemOverlay(pub lodestone_game::player_state::HeldItemHighlight);

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
        (
            &mut TitleOverlay,
            &mut ActionBarOverlay,
            &mut HudEffects,
            &mut HeldItemOverlay,
            Option<&SelectedSlot>,
            Option<&SessionMenus>,
        ),
        With<LocalPlayer>,
    >,
) {
    for (mut title, mut action_bar, mut effects, mut held_item, selected_slot, menus) in &mut players
    {
        title.0.tick(1);
        action_bar.0.tick(1);
        effects.0.tick(1);
        // `SelectedSlot`/`SessionMenus` are `Option` here, not required
        // query terms: this module's own docs establish that
        // `SessionHudPlugin` and `SessionPlugin` are separate plugins a
        // harness can install independently (`SessionHudPlugin` alone, as
        // `a_game_tick_run_expires_the_action_bar` does), so a required term
        // would have silently stopped every other overlay in this same
        // system from ageing at all on such a harness — the query simply
        // would not have matched the entity.
        //
        // The selected hotbar stack's identity, resolved through the same
        // `Menus::player_native` this module's own doc names as the
        // "borrow-friendly counterpart... the HUD's held item" — reading the
        // native index directly rather than cloning a whole `Menu`.
        // Translation is `|_| None` here: no language table reaches this
        // crate, matching `styled_hover_name`'s own documented gap. Identity
        // is unaffected — the same untranslated key always resolves the same
        // way, so retrigger detection stays correct even though the drawn
        // text is the best-effort fallback rather than a localised string.
        let selected = selected_slot.zip(menus).and_then(|(slot, menus)| {
            menus
                .0
                .player_native(slot.0)
                .filter(|stack| !stack.is_empty())
        });
        let identity = selected.map(|stack| {
            let name = lodestone_game::item::styled_hover_name(stack, &|_| None);
            (stack.item().clone(), name)
        });
        held_item
            .0
            .tick(identity.as_ref().map(|(item, name)| (item, name.as_str())));
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
            HeldItemOverlay::default(),
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

    /// A `World` carrying **both** halves of the fold — the session scalars and
    /// the per-entity ingest that owns [`Vitals::air`]/[`Vitals::on_fire`].
    ///
    /// [`session_app`] alone cannot express issue #390: `air` and `on_fire` are
    /// written only by `crate::ingest::apply_local_player_air_supply` /
    /// `apply_local_player_on_fire`, which are registered by `IngestPlugin`. A
    /// test that reached the drowned state by assigning `Vitals` directly would
    /// be asserting against a state the fold might never be able to produce —
    /// so both plugins go on, and every value below arrives as an event.
    fn drowning_app() -> (App, bevy_ecs::entity::Entity) {
        let mut app = App::new();
        app.add_plugins(SessionPlugin);
        app.add_plugins(crate::ingest::IngestPlugin);
        let entity = spawn_session(app.world_mut());
        (app, entity)
    }

    /// Drown, die, respawn: `EntityMetadataUpdate` naming our own id.
    fn air_and_fire(entity_id: i32, air: i32, on_fire: bool) -> ClientEvent {
        ClientEvent::EntityMetadataUpdated {
            entity_id,
            metadata: lodestone_model::EntityMetadataUpdate {
                air_supply: Some(air),
                flags: Some(if on_fire { 0x01 } else { 0x00 }),
                ..Default::default()
            },
        }
    }

    /// Issue #390: **a respawn clears the two entity-metadata-fed vitals.**
    ///
    /// Reported from play — after drowning, the bubble row rendered *completely
    /// empty* until the server's next metadata packet arrived with 300, which is
    /// what the player saw as an instant refill on touching water. Nothing wrote
    /// `air` on `Respawned`, so the dead entity's last reading (`0`) survived
    /// into the new life.
    ///
    /// `None`, not `Some(300)`: `None` is the documented "no reading yet" state
    /// and reads as full downstream, so the row stays hidden until the server
    /// says otherwise rather than us inventing a number.
    ///
    /// `on_fire` is checked in the same pass because it has the identical shape
    /// and the *quieter* polarity — its absence reads as `false`, so a stale
    /// `Some(true)` paints the fire overlay on a freshly respawned player with
    /// nothing to make the failure obvious.
    ///
    /// Every intermediate assertion here is a control: the drowned/burning state
    /// is asserted **before** the respawn, so a `None` afterwards is a clearing
    /// and not a value that was never set. See
    /// `a_respawn_does_not_clear_air_and_fire_for_a_still_drowning_player` for
    /// the other direction.
    #[test]
    fn a_respawn_clears_the_drowned_air_supply_and_the_burning_flag() {
        let (mut app, entity) = drowning_app();
        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 7,
                game_mode: GameMode::Survival,
                dimension: dim("overworld"),
            },
        );
        assert_eq!(
            app.world().get::<Vitals>(entity).unwrap().air,
            None,
            "nothing has reported air yet — the join state the row must stay hidden for"
        );

        // Drown to zero, on fire on the way down (a burning player who then
        // drowns is unusual but it is the state that proves both fields move).
        fold(&mut app, air_and_fire(7, 0, true));
        let drowned = *app.world().get::<Vitals>(entity).unwrap();
        assert_eq!(
            drowned.air,
            Some(0),
            "control: the fold must actually reach zero, or the clear below asserts nothing"
        );
        assert_eq!(drowned.on_fire, Some(true), "control: and the flag must be set");

        fold(
            &mut app,
            ClientEvent::Death {
                message: Text::literal("drowned"),
            },
        );
        assert_eq!(
            app.world().get::<Vitals>(entity).unwrap().air,
            Some(0),
            "control: the death packet alone must NOT clear it — that would make the \
             respawn arm below untestable by hiding which packet does the work"
        );

        fold(
            &mut app,
            ClientEvent::Respawned {
                dimension: dim("overworld"),
                game_mode: GameMode::Survival,
                previous_game_mode: Some(GameMode::Survival),
                last_death_location: None,
            },
        );
        let after = *app.world().get::<Vitals>(entity).unwrap();
        assert_eq!(
            after.air, None,
            "#390: a respawn is a brand-new entity on both sides — the drowned \
             reading must not survive it, or the bubble row draws empty until the \
             server's next metadata packet"
        );
        assert_eq!(
            after.on_fire, None,
            "#390's sibling: a stale burning flag paints the fire overlay on a \
             player who just respawned"
        );
    }

    /// **The other control.** The clear must be keyed to `Respawned` and nothing
    /// else: a still-drowning player who has not died keeps their reading, so the
    /// assertion above is not satisfied by a fold that clears `air` on every
    /// event (or by one that never lets it hold a value at all).
    ///
    /// A portal trip *does* clear, and that is correct rather than a false
    /// positive: `Respawned` is the same packet, vanilla builds the same new
    /// `LocalPlayer` for it, and 300-on-arrival is exactly what the server then
    /// reports.
    #[test]
    fn a_respawn_does_not_clear_air_and_fire_for_a_still_drowning_player() {
        let (mut app, entity) = drowning_app();
        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 7,
                game_mode: GameMode::Survival,
                dimension: dim("overworld"),
            },
        );
        fold(&mut app, air_and_fire(7, 120, true));

        // Everything a drowning player's tick actually carries, short of dying.
        fold(
            &mut app,
            ClientEvent::HealthChanged {
                health: 6.0,
                food: 17,
                saturation: 0.0,
            },
        );
        fold(
            &mut app,
            ClientEvent::GameModeChanged {
                game_mode: GameMode::Survival,
            },
        );
        let mid = *app.world().get::<Vitals>(entity).unwrap();
        assert_eq!(
            mid.air,
            Some(120),
            "an un-respawned player must keep their air reading — otherwise the \
             bubble row could never draw at all and #390's assertion is vacuous"
        );
        assert_eq!(mid.on_fire, Some(true));
    }

    /// A dimension type as the adapter builds it from `registry_data`.
    fn dim_type(name: &str, has_skylight: bool) -> DimensionTypeInfo {
        DimensionTypeInfo {
            name: format!("minecraft:{name}").parse().expect("valid key"),
            has_skylight,
            has_ceiling: !has_skylight,
            has_fixed_time: !has_skylight,
            coordinate_scale: if has_skylight { 1.0 } else { 8.0 },
            min_y: if has_skylight { -64 } else { 0 },
            height: if has_skylight { 384 } else { 256 },
            logical_height: if has_skylight { 384 } else { 128 },
            ambient_light: if has_skylight { 0.0 } else { 0.1 },
        }
    }

    /// Issue #288: the registry-driven dimension type must reach
    /// [`ServerDimensionType`] **through the schedule**, and must move on a
    /// portal trip the same way [`ServerDimension`] does.
    ///
    /// The pre-login assertion is the control: it proves the component starts
    /// `None` and is genuinely written, rather than having happened to hold the
    /// expected value all along.
    #[test]
    fn a_net_ingest_run_folds_the_registry_dimension_type_and_a_portal_trip_moves_it() {
        let (mut app, entity) = session_app();
        assert_eq!(
            app.world().get::<ServerDimensionType>(entity).unwrap().0,
            None,
            "pre-login there is no dimension type — not a defaulted overworld"
        );

        fold(
            &mut app,
            ClientEvent::DimensionTypeChanged {
                holder_id: 0,
                dimension_type: Some(dim_type("overworld", true)),
            },
        );
        let folded = app
            .world()
            .get::<ServerDimensionType>(entity)
            .unwrap()
            .0
            .clone()
            .expect("the fold must reach the component");
        assert!(folded.has_skylight);
        assert_eq!(folded.min_y, -64);
        assert_eq!(folded.height, 384);

        // A portal trip: the adapter emits this off `respawn`'s own dimension-type
        // holder id, immediately before `Respawned`.
        fold(
            &mut app,
            ClientEvent::DimensionTypeChanged {
                holder_id: 3,
                dimension_type: Some(dim_type("the_nether", false)),
            },
        );
        let nether = app
            .world()
            .get::<ServerDimensionType>(entity)
            .unwrap()
            .0
            .clone()
            .expect("a portal trip must install the new type");
        assert!(
            !nether.has_skylight,
            "the Nether's has_skylight is the value the mesher's sky default reads"
        );
        assert_eq!(nether.logical_height, 128);

        // An unresolvable dimension **clears** rather than keeping the last one:
        // a stale `has_skylight` renders a dark dimension lit, which is worse
        // than an honest `None` that makes the consumer state its fallback.
        fold(
            &mut app,
            ClientEvent::DimensionTypeChanged {
                holder_id: 99,
                dimension_type: None,
            },
        );
        assert_eq!(
            app.world().get::<ServerDimensionType>(entity).unwrap().0,
            None,
            "an unresolved holder id must clear the previous dimension type"
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
    /// #191's routing control. The decode has been correct since v770 landed and
    /// the event still reached **nothing**, because `SharedState::apply` forwards
    /// only what `ingest::handles_event` or [`handles_event`] lists. This pair —
    /// "someone claims it, and it is the right someone" — is the check that has
    /// caught this exact island four times now.
    #[test]
    fn abilities_changed_is_claimed_by_this_module_and_not_by_ingest() {
        let event = ClientEvent::AbilitiesChanged {
            invulnerable: false,
            flying: true,
            can_fly: true,
            instabuild: true,
            flying_speed: 0.05,
            walking_speed: 0.1,
        };
        assert!(
            handles_event(&event),
            "without this arm the abilities packet is decoded into nothing at all"
        );
        assert!(
            !crate::ingest::handles_event(&event),
            "abilities are a session scalar, not per-entity ingest; two claimants \
             would be two folds of one event"
        );

        // Same pair for `GameModeChanged`, whose absence froze `ServerGameMode` at
        // whatever the player logged in as.
        let mode = ClientEvent::GameModeChanged {
            game_mode: GameMode::Creative,
        };
        assert!(handles_event(&mode));
        assert!(!crate::ingest::handles_event(&mode));
    }

    /// #96's routing control, the fifth instance of the same pair.
    ///
    /// The per-biome sky tint was blocked for two sessions on exactly one missing
    /// link: the decoded colours never crossed the version-free seam. Once they
    /// do, the next place they can vanish is this switch — `SharedState::apply`
    /// forwards only what `ingest::handles_event` or [`handles_event`] lists, and
    /// an event neither claims falls through to the dead legacy scalar fallback.
    /// Registry data is a session-scoped scalar, so `ingest` must **not** claim
    /// it; guessing `ingest` for a session fact has misrouted work twice.
    #[test]
    fn biome_visuals_is_claimed_by_this_module_and_not_by_ingest() {
        let event = ClientEvent::BiomeVisuals {
            sky_colors: vec![Some(0x0078_a7ff), None],
        };
        assert!(
            handles_event(&event),
            "without this arm the biome registry is decoded into nothing at all and the \
             sky tint reaches zero pixels"
        );
        assert!(
            !crate::ingest::handles_event(&event),
            "biome registry data is a session scalar, not per-entity ingest"
        );
    }

    /// Issue #96: the biome sky-colour table must reach
    /// [`ServerBiomeSkyColors`] **through the schedule**, keep its holder-id
    /// indexing, and be *replaced* rather than merged.
    ///
    /// The pre-login assertion is the control: it proves the component starts
    /// empty and is genuinely written, rather than having happened to hold the
    /// expected value all along.
    #[test]
    fn a_net_ingest_run_folds_the_biome_sky_colours_and_a_resend_replaces_them() {
        let (mut app, entity) = session_app();
        assert!(
            app.world()
                .get::<ServerBiomeSkyColors>(entity)
                .unwrap()
                .0
                .is_empty(),
            "pre-login there is no biome table — not a defaulted overworld blue"
        );

        // Real 26.2 values at deliberately non-trivial holder ids: a colourless
        // entry first, so an implementation that skipped `None`s would shift
        // every later colour by one and still look plausible on screen.
        fold(
            &mut app,
            ClientEvent::BiomeVisuals {
                sky_colors: vec![None, Some(0x00b9_b9b9), Some(0x006e_b1ff)],
            },
        );
        let folded = app
            .world()
            .get::<ServerBiomeSkyColors>(entity)
            .unwrap()
            .0
            .clone();
        assert_eq!(
            &*folded,
            [None, Some(0x00b9_b9b9), Some(0x006e_b1ff)],
            "the fold must reach the component with holder ids intact"
        );

        // Re-entering configuration resends the registries. Appending would put
        // the new table's biomes at holder ids 3.. while the stale ones kept
        // answering 0.., which is the same failure `ClientRegistries::apply`
        // guards against one layer down.
        fold(
            &mut app,
            ClientEvent::BiomeVisuals {
                sky_colors: vec![Some(0x0085_9dff)],
            },
        );
        assert_eq!(
            &*app
                .world()
                .get::<ServerBiomeSkyColors>(entity)
                .unwrap()
                .0
                .clone(),
            [Some(0x0085_9dff)],
            "a resent registry replaces the table"
        );

        // And an empty table clears it: a server switch that sends no biome
        // registry has to stop tinting, not keep painting the last world's sky.
        fold(
            &mut app,
            ClientEvent::BiomeVisuals {
                sky_colors: Vec::new(),
            },
        );
        assert!(
            app.world()
                .get::<ServerBiomeSkyColors>(entity)
                .unwrap()
                .0
                .is_empty(),
            "an empty table must clear, not preserve"
        );
    }

    /// The fold itself, **through the schedule** rather than by calling the system
    /// directly — a hermetic call passes whether or not the routing arm above
    /// exists, which is precisely why the island survived review three times.
    #[test]
    fn a_net_ingest_run_folds_abilities_onto_the_component() {
        let (mut app, entity) = session_app();

        // PRECONDITION, asserted rather than assumed: a fresh session must not
        // believe it may fly. If this defaulted to `true` the test below would
        // pass against a fold that did nothing.
        let before = *app.world().get::<Abilities>(entity).expect("Abilities");
        assert_eq!(before, Abilities::default());
        assert!(!before.may_fly, "a fresh session has no flight grant");
        assert!(!before.flying);

        fold(
            &mut app,
            ClientEvent::AbilitiesChanged {
                invulnerable: true,
                flying: true,
                can_fly: true,
                instabuild: true,
                flying_speed: 0.075,
                walking_speed: 0.15,
            },
        );
        let after = *app.world().get::<Abilities>(entity).expect("Abilities");
        assert!(after.flying);
        assert!(after.may_fly);
        assert!(after.invulnerable);
        assert!(after.instabuild);
        assert_eq!(after.flying_speed, 0.075);
        assert_eq!(after.walking_speed, 0.15);
    }

    /// **The server-gating test, and it needs a genuinely negative input.**
    ///
    /// A "flight is server-gated" test whose fixture has `can_fly: true`
    /// throughout proves nothing about the gate — the *world* species of vacuous
    /// test, whose flaw lives in the input data and is invisible in the test
    /// source. So this feeds `can_fly: false` explicitly, and additionally proves
    /// a revocation clears a previously-granted bit.
    #[test]
    fn a_server_that_revokes_flight_clears_both_bits() {
        let (mut app, entity) = session_app();
        let grant = |flying, can_fly| ClientEvent::AbilitiesChanged {
            invulnerable: false,
            flying,
            can_fly,
            instabuild: false,
            flying_speed: 0.05,
            walking_speed: 0.1,
        };

        fold(&mut app, grant(true, true));
        assert!(app.world().get::<Abilities>(entity).unwrap().may_fly);

        // A survival server, or `/gamemode survival` mid-session.
        fold(&mut app, grant(false, false));
        let after = *app.world().get::<Abilities>(entity).unwrap();
        assert!(
            !after.may_fly,
            "the record is replaced wholesale; a stale grant must not survive"
        );
        assert!(!after.flying);
    }

    /// A runtime `/gamemode` must reach `ServerGameMode`, which before #191 was
    /// written only by `Login`/`Respawned`.
    #[test]
    fn a_runtime_game_mode_change_reaches_the_component() {
        let (mut app, entity) = session_app();
        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 1,
                game_mode: GameMode::Survival,
                dimension: dim("overworld"),
            },
        );
        assert_eq!(
            app.world().get::<ServerGameMode>(entity).unwrap().0,
            Some(GameMode::Survival),
            "precondition: login must seed the mode, or the change below is invisible"
        );
        fold(
            &mut app,
            ClientEvent::GameModeChanged {
                game_mode: GameMode::Creative,
            },
        );
        assert_eq!(
            app.world().get::<ServerGameMode>(entity).unwrap().0,
            Some(GameMode::Creative)
        );
    }

    /// The first of the three `HudState`-shaped islands
    /// (`docs/event-routing.md`): `HeldSlotChanged` reached
    /// `lodestone_game::player_state::HudState::select_slot`, which nothing
    /// called in production. Drives the real `NetIngest` schedule, not
    /// `HudState::apply` directly — a closed loop over the fold would have
    /// passed the whole time this was an island.
    #[test]
    fn held_slot_changed_reaches_selected_slot_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        // `SelectedSlot` is inserted by `spawn_local_player`
        // (`lodestone-ecs::player`), not `insert_session_components` — added
        // by hand here to exercise the write path. The real client always
        // carries both on one entity; `held_slot_changed_is_a_no_op_without_
        // selected_slot_present` below is the control for a harness that does
        // not.
        app.world_mut().entity_mut(entity).insert(SelectedSlot(0));

        fold(&mut app, ClientEvent::HeldSlotChanged { slot: 5 });
        assert_eq!(app.world().get::<SelectedSlot>(entity).unwrap().0, 5);

        // Vanilla ignores an out-of-range wire value rather than corrupting
        // the selection — `HudState::select_slot`'s clamp, reproduced here.
        fold(&mut app, ClientEvent::HeldSlotChanged { slot: 9 });
        assert_eq!(
            app.world().get::<SelectedSlot>(entity).unwrap().0,
            5,
            "an out-of-range slot must be ignored, not miscast into a wrong \
             in-range value"
        );
        fold(&mut app, ClientEvent::HeldSlotChanged { slot: -1 });
        assert_eq!(app.world().get::<SelectedSlot>(entity).unwrap().0, 5);
    }

    /// The control for the `Option<&mut SelectedSlot>` term: a harness with no
    /// `SelectedSlot` at all — the real shape `spawn_session` alone
    /// produces — must not panic, and every *other* field in the same query
    /// must still fold (proven by the game-mode/difficulty tests running
    /// against the same `session_app()` with no `SelectedSlot` either).
    #[test]
    fn held_slot_changed_is_a_no_op_without_selected_slot_present() {
        let (mut app, entity) = session_app();
        fold(&mut app, ClientEvent::HeldSlotChanged { slot: 5 });
        assert!(app.world().get::<SelectedSlot>(entity).is_none());
    }

    /// The second `HudState`-shaped island: `DifficultyChanged` reached
    /// `HudState::apply`'s difficulty arm, which nothing called.
    #[test]
    fn difficulty_changed_reaches_server_difficulty_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        assert_eq!(
            app.world().get::<ServerDifficulty>(entity).unwrap().0,
            None,
            "precondition: unreported before the first packet"
        );
        fold(
            &mut app,
            ClientEvent::DifficultyChanged {
                difficulty: Difficulty::Hard,
                locked: true,
            },
        );
        assert_eq!(
            app.world().get::<ServerDifficulty>(entity).unwrap().0,
            Some((Difficulty::Hard, true))
        );
    }

    /// The third: `BlockDestruction` reached
    /// `lodestone_game::mining::BlockDestructionOverlays::apply`, which
    /// nothing called outside its own file and tests.
    #[test]
    fn block_destruction_reaches_the_session_overlay_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        let p = lodestone_model::math::BlockPos::new(3, 64, 3);
        fold(
            &mut app,
            ClientEvent::BlockDestruction {
                entity_id: 7,
                pos: p,
                progress: 4,
            },
        );
        assert_eq!(
            app.world()
                .get::<SessionBlockDestruction>(entity)
                .unwrap()
                .0
                .stage_at(p),
            Some(4)
        );

        // A stage >= 10 clears the overlay, matching vanilla
        // (`LevelRenderer.setBlockBreakProgress`) — proven through the same
        // real schedule run, not by calling `BlockDestructionOverlays::apply`.
        fold(
            &mut app,
            ClientEvent::BlockDestruction {
                entity_id: 7,
                pos: p,
                progress: 10,
            },
        );
        assert_eq!(
            app.world()
                .get::<SessionBlockDestruction>(entity)
                .unwrap()
                .0
                .stage_at(p),
            None
        );
    }

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

    /// The `Riding` fold, through the schedule (Tier 1 item 8): our own id
    /// appearing in a vehicle's passenger list is what mounts us, and the list
    /// going empty is what dismounts us.
    #[test]
    fn our_own_id_in_a_passenger_list_mounts_us_and_an_empty_list_dismounts_us() {
        let (mut app, entity) = session_app();
        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 7,
                game_mode: GameMode::Creative,
                dimension: dim("overworld"),
            },
        );
        assert_eq!(
            app.world().get::<Riding>(entity).copied(),
            Some(Riding(None)),
            "a freshly logged-in player is on foot"
        );
        fold(
            &mut app,
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 42,
                passenger_ids: vec![7],
            },
        );
        assert_eq!(
            app.world().get::<Riding>(entity).copied(),
            Some(Riding(Some(42)))
        );
        fold(
            &mut app,
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 42,
                passenger_ids: Vec::new(),
            },
        );
        assert_eq!(app.world().get::<Riding>(entity).copied(), Some(Riding(None)));
    }

    /// **The `else if` this pins is the difference between riding a boat and being
    /// ejected from it by an unrelated mob.** `SET_PASSENGERS` is broadcast for
    /// every vehicle in view distance, so "our id is not in this list" only means
    /// "we dismounted" when the list belongs to the vehicle we are actually in.
    #[test]
    fn another_vehicles_passenger_list_does_not_dismount_us() {
        let (mut app, entity) = session_app();
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
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 42,
                passenger_ids: vec![7],
            },
        );
        // Precondition, asserted: without a live seat the assertion below cannot
        // distinguish "was not ejected" from "was never aboard".
        assert_eq!(
            app.world().get::<Riding>(entity).copied(),
            Some(Riding(Some(42))),
            "precondition: we must be aboard 42"
        );
        // Some pig two chunks away gains a rider.
        fold(
            &mut app,
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 99,
                passenger_ids: vec![1234],
            },
        );
        assert_eq!(
            app.world().get::<Riding>(entity).copied(),
            Some(Riding(Some(42))),
            "an unrelated vehicle's passenger list must leave our own seat alone"
        );
        // And the negative control for the same detector: a list for *our* vehicle
        // that omits us does dismount, so the assertion above is about the vehicle
        // id being compared and not about the fold being inert.
        fold(
            &mut app,
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 42,
                passenger_ids: vec![1234],
            },
        );
        assert_eq!(
            app.world().get::<Riding>(entity).copied(),
            Some(Riding(None)),
            "our own vehicle's list without us must dismount"
        );
    }

    /// A respawn lands us on foot. Vanilla builds a brand-new `ServerPlayer`
    /// (`PlayerList.respawn`) which is never a passenger, and no
    /// `SET_PASSENGERS` follows — so without the explicit clear the seat pin
    /// would hold a respawned player at a vehicle nothing can free them from.
    #[test]
    fn respawning_clears_the_ride_state() {
        let (mut app, entity) = session_app();
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
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 42,
                passenger_ids: vec![7],
            },
        );
        assert_eq!(
            app.world().get::<Riding>(entity).copied(),
            Some(Riding(Some(42))),
            "precondition: aboard before dying"
        );
        fold(
            &mut app,
            ClientEvent::Respawned {
                dimension: dim("overworld"),
                game_mode: GameMode::Survival,
                previous_game_mode: None,
                last_death_location: None,
            },
        );
        assert_eq!(app.world().get::<Riding>(entity).copied(), Some(Riding(None)));
    }
}
