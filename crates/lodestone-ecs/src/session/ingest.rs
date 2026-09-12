use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{Query, Res, With};
use bevy_ecs::schedule::{IntoScheduleConfigs, SystemSet};
use bevy_ecs::world::World;
use lodestone_model::ClientEvent;

use super::components::*;
use crate::ingest::{IngestBatch, IngestQueuePlugin};
use crate::player::{LocalPlayer, SelectedSlot};
use crate::schedules::NetIngest;
use crate::sets::IngestSet;

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

/// `IngestSet::Apply`: the world-border family → [`SessionWorldBorder`].
///
/// # Why this one takes a clock and its siblings do not
///
/// A border resize is defined by wall time, and
/// [`lodestone_game::worldborder::WorldBorder::apply`] deliberately does not take
/// a clock — that would fork the `apply(&ClientEvent) -> bool` convention every
/// other aggregate in `lodestone-game` follows. So the fold records the resize
/// unstamped and this system, which *can* see resources, stamps it.
///
/// [`FrameClock`](crate::FrameClock) is the right clock rather than
/// [`WorldTime`](crate::WorldTime): `WorldTime` is the *server's* clock, which the
/// server can freeze with `advance_time`, and a frozen clock must not freeze a
/// border animation. `FrameClock::secs` is monotonic wall time — the same clock
/// the chat fade-out reads.
///
/// # Why `Option<Res<..>>`
///
/// `FrameClock` is inserted by [`crate::CorePlugin`], and [`SessionPlugin`] is
/// deliberately addable without it (`spawn_session`'s harness installs
/// `SessionPlugin` alone). A required `Res` would panic there. Absent clock means
/// an unstamped resize, which
/// [`size_at`](lodestone_game::worldborder::BorderExtent::size_at) reports as the
/// *old* size — inert, not a guess.
pub fn apply_world_border(
    batch: Res<IngestBatch>,
    clock: Option<Res<crate::FrameClock>>,
    mut borders: Query<&mut SessionWorldBorder>,
) {
    let now = clock.map_or(0.0, |c| c.secs);
    for event in batch.events() {
        for mut border in &mut borders {
            if border.0.apply(event) {
                // `stamp` is idempotent, so a later border packet cannot restart
                // an interpolation that is already running.
                border.0.stamp(now);
            }
        }
    }
}

/// `IngestSet::Apply`: `SpawnPositionChanged` → [`SessionSpawnPoint`].
pub fn apply_spawn_point(batch: Res<IngestBatch>, mut spawns: Query<&mut SessionSpawnPoint>) {
    for event in batch.events() {
        for mut spawn in &mut spawns {
            let _ = spawn.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: `RecipeBookSettingsChanged` → [`SessionRecipeBookSettings`].
pub fn apply_recipe_book_settings(
    batch: Res<IngestBatch>,
    mut settings: Query<&mut SessionRecipeBookSettings>,
) {
    for event in batch.events() {
        for mut books in &mut settings {
            let _ = books.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: `MapItemData` → [`SessionMaps`].
pub fn apply_maps(batch: Res<IngestBatch>, mut maps: Query<&mut SessionMaps>) {
    for event in batch.events() {
        for mut store in &mut maps {
            let _ = store.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: `AdvancementsUpdated` → [`SessionAdvancements`].
pub fn apply_advancements(batch: Res<IngestBatch>, mut trees: Query<&mut SessionAdvancements>) {
    for event in batch.events() {
        for mut store in &mut trees {
            let _ = store.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: `AdvancementsTabSelected` → [`SessionAdvancementTab`].
pub fn apply_advancement_tab(
    batch: Res<IngestBatch>,
    mut selections: Query<&mut SessionAdvancementTab>,
) {
    for event in batch.events() {
        let ClientEvent::AdvancementsTabSelected { tab } = event else {
            continue;
        };
        for mut selection in &mut selections {
            selection.0 = tab.clone();
        }
    }
}

/// `IngestSet::Apply`: `GameRulesChanged` → [`SessionGameRules`].
pub fn apply_game_rules(batch: Res<IngestBatch>, mut rules: Query<&mut SessionGameRules>) {
    for event in batch.events() {
        for mut table in &mut rules {
            let _ = table.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: `StatisticsAwarded` -> [`SessionStatistics`].
pub fn apply_statistics(batch: Res<IngestBatch>, mut stats: Query<&mut SessionStatistics>) {
    for event in batch.events() {
        for mut table in &mut stats {
            let _ = table.0.apply_event(event);
        }
    }
}

/// `IngestSet::Apply`: the recipe-book family -> [`SessionRecipeBook`].
pub fn apply_recipe_book_sync(batch: Res<IngestBatch>, mut books: Query<&mut SessionRecipeBook>) {
    for event in batch.events() {
        for mut store in &mut books {
            let _ = store.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: `MerchantOffersReceived` -> [`SessionTrades`].
pub fn apply_trades(batch: Res<IngestBatch>, mut trades: Query<&mut SessionTrades>) {
    for event in batch.events() {
        for mut store in &mut trades {
            let _ = store.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: the `*RegistryNames` events -> [`SessionRegistryOrder`].
pub fn apply_registry_order(batch: Res<IngestBatch>, mut orders: Query<&mut SessionRegistryOrder>) {
    for event in batch.events() {
        for mut order in &mut orders {
            let _ = order.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: the `debug_*` family -> [`SessionDebugFeeds`].
pub fn apply_debug_feeds(batch: Res<IngestBatch>, mut feeds: Query<&mut SessionDebugFeeds>) {
    for event in batch.events() {
        for mut store in &mut feeds {
            let _ = store.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: the server-metadata family -> [`SessionServerInfo`].
pub fn apply_server_info(batch: Res<IngestBatch>, mut info: Query<&mut SessionServerInfo>) {
    for event in batch.events() {
        for mut store in &mut info {
            let _ = store.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: `ServerDataReceived` -> [`SessionServerData`].
pub fn apply_server_data(batch: Res<IngestBatch>, mut data: Query<&mut SessionServerData>) {
    for event in batch.events() {
        let ClientEvent::ServerDataReceived { motd, icon } = event else {
            continue;
        };
        for mut server_data in &mut data {
            server_data.0 = Some(ServerData {
                motd: motd.clone(),
                icon: icon.clone(),
            });
        }
    }
}

/// `IngestSet::Apply`: `ItemCooldown` -> [`SessionItemCooldowns`].
pub fn apply_item_cooldowns(
    batch: Res<IngestBatch>,
    mut cooldowns: Query<&mut SessionItemCooldowns>,
) {
    for event in batch.events() {
        for mut store in &mut cooldowns {
            let _ = store.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: combat enter/end packets -> [`SessionCombat`].
pub fn apply_combat_session(batch: Res<IngestBatch>, mut combat: Query<&mut SessionCombat>) {
    for event in batch.events() {
        let state = match event {
            ClientEvent::PlayerCombatEntered => CombatSession::Active,
            ClientEvent::PlayerCombatEnded { duration_ticks } => CombatSession::Ended {
                duration_ticks: *duration_ticks,
            },
            _ => continue,
        };
        for mut session in &mut combat {
            session.0 = Some(state);
        }
    }
}

/// `IngestSet::Apply`: `WaypointUpdated` -> [`SessionWaypoints`].
pub fn apply_waypoints(batch: Res<IngestBatch>, mut waypoints: Query<&mut SessionWaypoints>) {
    for event in batch.events() {
        for mut store in &mut waypoints {
            let _ = store.0.apply(event);
        }
    }
}

/// `IngestSet::Apply`: `BossBarUpdate` -> [`SessionBossBars`].
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

/// `IngestSet::Apply`: block-destruction progress and lifecycle resets →
/// [`SessionBlockDestruction`].
pub fn apply_block_destruction(
    batch: Res<IngestBatch>,
    mut overlays: Query<&mut SessionBlockDestruction>,
) {
    for event in batch.events() {
        for mut set in &mut overlays {
            match event {
                // Login and respawn replace the client-level view from the
                // renderer's perspective. No progress packet is required to
                // clear a crack from the old view.
                ClientEvent::Login { .. } | ClientEvent::Respawned { .. } => set.0.clear(),
                // Entity lifecycle packets use the same event batch as the
                // progress packet, but are folded by the entity plugin. Keep
                // this cleanup in the sole overlay writer so arrival order is
                // preserved when a spawn/removal and progress update share a
                // batch.
                ClientEvent::EntitySpawned { entity_id, .. } => {
                    set.0.clear_entity(*entity_id);
                }
                ClientEvent::EntityRemoved { entity_ids } => {
                    for entity_id in entity_ids {
                        set.0.clear_entity(*entity_id);
                    }
                }
                _ => {
                    let _ = set.0.apply(event);
                }
            }
        }
    }
}

/// `IngestSet::Apply`: the local player's own server-reported state →
/// [`Vitals`], [`Xp`], [`ServerEntityId`], [`ServerGameMode`],
/// [`ServerDimension`], [`ServerDimensionType`], [`ServerBiomeSkyColors`],
/// [`ServerAlive`], [`Abilities`], [`ServerDifficulty`],
/// [`ServerSimulationDistance`],
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
            &mut ServerPermissionLevel,
            &mut ServerGameMode,
            &mut ServerDimension,
            &mut ServerDimensionType,
            &mut ServerBiomeSkyColors,
            &mut ServerAlive,
            &mut Abilities,
            &mut Riding,
            &mut ServerDifficulty,
            &mut ServerSimulationDistance,
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
            mut permission_level,
            mut game_mode,
            mut dimension,
            mut dimension_type,
            mut biome_sky_colors,
            mut alive,
            mut abilities,
            mut riding,
            mut difficulty,
            mut simulation_distance,
            mut selected_slot,
        ) in &mut players
        {
            match event {
                // Emitted immediately *before* `Login`/`Respawned`
                // by the adapter, off the same packet's dimension-type holder id.
                //
                // Assigned unconditionally, `None` included: an unresolvable
                // dimension must **clear** the previous one, or a portal trip
                // into a custom dimension would keep reporting the overworld's
                // `has_skylight` — the stale-value failure mode that is worse
                // than an honest `None`.
                ClientEvent::DimensionTypeChanged {
                    dimension_type: info,
                    is_flat,
                    ..
                } => {
                    dimension_type.info = info.clone();
                    dimension_type.is_flat = *is_flat;
                }
                // Assigned unconditionally for the same reason the
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
                    permission_level.0 = 0;
                    game_mode.0 = Some(*mode);
                    dimension.0 = Some(dim.clone());
                    alive.0 = true;
                }
                ClientEvent::EntityStatus { entity_id, status } if id.0 == Some(*entity_id) => {
                    permission_level.0 = match status {
                        24 => 0,
                        25 => 1,
                        26 => 2,
                        27 => 3,
                        28 => 4,
                        _ => permission_level.0,
                    };
                }
                // `Respawned` is *also* how the server reports portal travel, not
                // only death — see [`ServerDimension`].
                ClientEvent::Respawned {
                    dimension: dim,
                    game_mode: mode,
                    ..
                } => {
                    permission_level.0 = 0;
                    dimension.0 = Some(dim.clone());
                    game_mode.0 = Some(*mode);
                    alive.0 = true;
                    // The two entity-metadata-fed fields go back to
                    // "no reading yet", because a respawn is a **brand-new
                    // player entity on both sides**: `PlayerList.respawn` does
                    // `new ServerPlayer(...)` and
                    // vanilla's *client* likewise builds a fresh `LocalPlayer`
                    // via `gameMode.createPlayer`
                    // (in `ClientPacketListener.handleRespawn`) and only
                    // copies the old id onto it. Its synched data therefore
                    // starts at `Entity`'s own defaults —
                    // `entityDataBuilder.define(DATA_AIR_SUPPLY_ID,
                    // getMaxAirSupply())` (i.e. 300) and
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
                // Assigned as a **whole record**, never field-by-field:
                // vanilla's own abilities-apply step overwrites every field from
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
                // Keep the raw server report rather than deriving a distance
                // from chunk traffic: a server may stream farther than it
                // simulates, and the F3 line exists to make that distinction
                // visible.
                ClientEvent::SimulationDistanceChanged { distance } => {
                    simulation_distance.0 = Some(*distance);
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
            ServerPermissionLevel::default(),
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
        entity.insert((
            ServerDifficulty::default(),
            ServerSimulationDistance::default(),
            SessionBlockDestruction::default(),
            // Also the quit-to-title reset path (see this function's docs). A
            // stale world border is the one of these three with a visible
            // failure mode: joining a vanilla server after a bordered one would
            // otherwise keep the old centre and size until the new server's
            // `InitializeBorder` arrived, so the warning overlay could paint on
            // a world that has no border there.
            SessionWorldBorder::default(),
            SessionSpawnPoint::default(),
            SessionGameRules::default(),
            SessionRecipeBookSettings::default(),
            SessionMaps::default(),
            SessionAdvancements::default(),
            SessionAdvancementTab::default(),
        ));
        // A third `insert`: four more stores, again only because the
        // tuple `Bundle` impls stop at arity 15.
        entity.insert((
            SessionStatistics::default(),
            SessionDebugFeeds::default(),
            SessionServerInfo::default(),
            SessionServerData::default(),
            SessionItemCooldowns::default(),
            SessionCombat::default(),
            ServerChunkCacheCenter::default(),
            SessionWaypoints::default(),
            SessionRegistryOrder::default(),
            SessionRecipeBook::default(),
            SessionTrades::default(),
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
            // Bevy implements tuple scheduling only through a bounded arity.
            // Keep the old one-by-one global order, but nest contiguous runs so
            // adding a session fold cannot turn this registration into a
            // method-resolution compile error. The schedule test below proves a
            // later consumer observes this final fold after all three groups.
            (
                (
                    apply_scoreboard,
                    apply_tab_list,
                    apply_boss_bars,
                    apply_menus,
                    apply_block_destruction,
                    apply_world_border,
                    apply_spawn_point,
                )
                    .chain(),
                (
                    apply_game_rules,
                    apply_recipe_book_settings,
                    apply_maps,
                    apply_advancements,
                    apply_advancement_tab,
                    apply_statistics,
                    apply_registry_order,
                )
                    .chain(),
                (
                    apply_recipe_book_sync,
                    apply_trades,
                    apply_debug_feeds,
                    apply_server_info,
                    apply_server_data,
                    apply_item_cooldowns,
                    apply_combat_session,
                    apply_waypoints,
                    apply_local_player_state,
                )
                    .chain(),
            )
                .chain()
                .in_set(SessionSet::Fold)
                .in_set(IngestSet::Apply),
        );
    }
}
