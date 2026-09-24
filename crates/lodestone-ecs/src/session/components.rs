use bevy_ecs::component::Component;
use lodestone_model::{DimensionId, DimensionTypeInfo, Difficulty, GameMode, Identifier, Text};

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

/// The folded world border — centre, size (including a resize in flight),
/// warning distance and warning delay.
///
/// All six `ClientEvent::WorldBorder*` variants were islands: decoded, covered by
/// `crates/versions/26.2/tests/world_border.rs`, and routed
/// `Route::NOWHERE`. They were the largest single cluster in
/// `docs/event-routing.md`'s list. [`lodestone_game::worldborder::WorldBorder`] is
/// the fold; this is the component, and [`apply_world_border`] the system.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct SessionWorldBorder(pub lodestone_game::worldborder::WorldBorder);

/// The world's default spawn point, from `ClientEvent::SpawnPositionChanged`.
///
/// The consumer is the compass: `lodestone_render::item_render` lists
/// `minecraft:compass` among the item-model range properties that are
/// *deliberately unsourced* because the datum "genuinely is not decoded". It is
/// decoded — it just never reached anything. See
/// [`lodestone_game::levelstate::SpawnPoint`].
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct SessionSpawnPoint(pub lodestone_game::levelstate::SpawnPoint);

/// The server's reported game-rule values, from `ClientEvent::GameRulesChanged`.
///
/// **Not** a typed registry — that would be a server-side 59-rule table and
/// is not built. This holds raw wire strings with typed accessors over the top;
/// see [`lodestone_game::levelstate::GameRuleValues`] for why absence is kept
/// distinct from `false` (the packet is request/response, not broadcast, so an
/// unreported rule is the *normal* case).
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionGameRules(pub lodestone_game::levelstate::GameRuleValues);

/// The server's stored per-book recipe-book UI state, from
/// `ClientEvent::RecipeBookSettingsChanged`.
///
/// The packet had **no decode at all** before this — the id was registered, which
/// proves only that the id is known. `ClientAction::SetRecipeBookSettings` was
/// already encoded, so the round trip was half-open: our state could go out and
/// the server's could never come back. See
/// [`lodestone_game::recipe::RecipeBookSettings`].
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionRecipeBookSettings(pub lodestone_game::recipe::RecipeBookSettings);

/// Filled-map contents by map id, from `ClientEvent::MapItemData`.
///
/// Session-scoped rather than per-entity: one map id can be held by several
/// players and hung in several item frames at once. See
/// [`lodestone_game::maps::MapStore`], and note the colour half of the packet is
/// a sub-rectangle, not a frame.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct SessionMaps(pub lodestone_game::maps::MapStore);

/// The advancement tree and the local player's progress, from
/// `ClientEvent::AdvancementsUpdated`.
///
/// The server-computed `x`/`y` inside each node's display are the only source of
/// vanilla's own tree layout — 26.2's advancement JSON on disk carries no
/// position. See [`lodestone_game::advancement::AdvancementStore`].
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct SessionAdvancements(pub lodestone_game::advancement::AdvancementStore);

/// The tab the server selected for the local player's advancement screen.
///
/// This is intentionally independent of [`SessionAdvancements`]: selecting a
/// tab changes no progress and an empty tree can still have a selected tab.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionAdvancementTab(pub Option<Identifier>);

/// The local player's statistics counters, from `ClientEvent::StatisticsAwarded`.
///
/// `award_stats` had no decode at all before this, which is why
/// `lodestone_shell::menu::stats` renders from `StatsSnapshot::default()` — an
/// empty table its own module doc correctly called "not a placeholder, it is the
/// state". This component is where the real numbers now arrive; feeding the
/// screen from it is a shell-side change.
///
/// See [`lodestone_game::progress::Statistics`].
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionStatistics(pub lodestone_game::progress::Statistics);

/// The server's recipe-book sync -- unlocks, the ghost preview, and the property
/// sets -- from `recipe_book_add`/`remove`, `place_ghost_recipe` and
/// `update_recipes`.
///
/// **Not** the recipe corpus (that is `RecipeRegistry`), and note 26.x identifies
/// a recipe by a per-session `RecipeDisplayId` `i32` rather than an `Identifier`,
/// so `lodestone_game::recipe::RecipeUnlockState` -- which keys on `Identifier` --
/// cannot be fed from this packet family. See
/// [`lodestone_game::recipe_sync::RecipeBookSync`].
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct SessionRecipeBook(pub lodestone_game::recipe_sync::RecipeBookSync);

/// The open merchant's trade list, from `ClientEvent::MerchantOffersReceived`.
///
/// Session state rather than per-entity state about the villager: it is a *menu*,
/// the same as every other container event, and it dies with the screen.
/// See [`lodestone_game::trades::TradeOffers`].
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct SessionTrades(pub lodestone_game::trades::TradeOffers);

/// The server's own registry orders by holder id, from the `*RegistryNames`
/// events (the enchantment half).
///
/// `minecraft:enchantment` today. The table was already decoded by
/// `ClientRegistries::entry_names` and never left the version crate, so
/// `Sim::riptide_level` resolved `minecraft:riptide` through a hardcoded holder
/// id of 32 -- correct against vanilla 26.2 and silently wrong against any data
/// pack that reorders. See [`lodestone_game::registry_order::RegistryOrder`].
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRegistryOrder(pub lodestone_game::registry_order::RegistryOrder);

/// Server debug feeds and NBT query replies, from the `debug_*`, `debug_sample`,
/// `game_test_highlight_pos`, `test_instance_block_status` and `tag_query`
/// packets.
///
/// Empty on every ordinary session and that is correct, not a defect: the server
/// sends nothing on a debug feed until the client asks with
/// `ClientAction::SubscribeDebug`. See [`lodestone_game::debug_feeds::DebugFeedStore`].
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct SessionDebugFeeds(pub lodestone_game::debug_feeds::DebugFeedStore);

/// What the server has announced about itself: links, report details, chat
/// completions, tick rate and the open dialog.
///
/// See [`lodestone_game::serverinfo::ServerInfoStore`]. Everything in it is
/// server-authored and untrusted.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct SessionServerInfo(pub lodestone_game::serverinfo::ServerInfoStore);

/// The public name and icon the connected server last announced.
///
/// Unlike [`SessionServerInfo`], whose packets describe interactive server
/// facilities, this is the server-list identity the play connection repeats.
/// [`lodestone_shell::hud::DebugStats`] reads the message of the day from this
/// component, so an adapter decode is visible in the on-screen F3 overlay.
/// `None` means this protocol family has not sent a server-data packet yet;
/// it is not an empty message of the day.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerData {
    /// The server-authored message of the day, retaining its component styling.
    pub motd: Text,
    /// The optional favicon PNG, retained for a future in-session identity UI.
    pub icon: Option<Vec<u8>>,
}

/// The most recent [`ServerData`] packet, if the server has sent one.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct SessionServerData(pub Option<ServerData>);

/// Server-announced item-use cooldown groups, read by the hotbar cooldown veil.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionItemCooldowns(pub lodestone_game::cooldown::ItemCooldowns);

/// The local player's latest server-announced combat-session state.
///
/// `None` means neither combat packet has arrived. Entering combat is distinct
/// from an ended encounter with a zero duration, so the HUD can report only
/// facts the server actually announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombatSession {
    /// The server has announced that combat tracking is active.
    Active,
    /// The server ended tracking and reported the encounter duration in ticks.
    Ended {
        /// The exact signed value supplied by the packet.
        duration_ticks: i32,
    },
}

/// The latest [`CombatSession`] packet state, read by the F3 HUD line.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionCombat(pub Option<CombatSession>);

/// The center of the chunk square the server is currently streaming.
///
/// This is deliberately separate from the local player's position. During a
/// loading transition the server can move its stream center before local
/// movement catches up; the loading grid must then ask about the columns the
/// server is actually sending rather than draw an all-empty square around the
/// old player chunk. `Sim::poll_net` is the sole writer, because this event
/// crosses the shell's bounded update channel instead of the shared ingest
/// fold.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerChunkCacheCenter(pub Option<(i32, i32)>);

/// Tracked waypoints — vanilla's locator bar — from `ClientEvent::WaypointUpdated`.
///
/// See [`lodestone_game::waypoints::WaypointStore`], and note the position is a
/// four-way precision degradation rather than an `Option`.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct SessionWaypoints(pub lodestone_game::waypoints::WaypointStore);

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
/// **Drawing it is done too, and this note used to say otherwise.** The
/// renderer's `CrackPipeline` (`lodestone_shell::gpu`) now accepts any number
/// of targets in one pass: `Sim::crack_targets` walks this component via
/// `crate::gpu::gather_crack_targets` and hands the local dig plus every
/// other player's overlay to `render_with_crack_and_effects` in one `Vec`
/// (`lodestone_shell::app::redraw`). The gather and the pipeline were both
/// proven in isolation before that call site existed, and nothing in
/// production called the gather until it landed. A stale "separate piece of
/// work" claim is exactly the trap this repo's own working rules call out: it
/// was true when written and wrong by the time this was read.
/// [`stage_at`](lodestone_game::mining::BlockDestructionOverlays::stage_at) is
/// the read side that call chain uses.
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

/// The local player's server-granted permission level, `0..=4`.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerPermissionLevel(pub u8);

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
///
/// Nothing in the shell reads this yet — showing it in the F3 overlay / pause
/// menu is tracked separately, since that is a text/HUD change
/// in files outside this routing fix's scope.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerDifficulty(pub Option<(Difficulty, bool)>);

/// The simulation distance most recently reported by the server, in chunks.
///
/// It is deliberately separate from the streamed-view radius: the latter
/// controls which columns the client receives, while this scalar governs which
/// loaded columns the server advances. The client does not simulate server
/// chunks, so retaining it as a session fact and exposing it in the F3 panel is
/// the honest consumer. `None` means this protocol family has not reported a
/// value yet; a visible `0` would falsely claim a report arrived.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServerSimulationDistance(pub Option<i32>);

/// The local player's server-granted **abilities** — `Abilities.Packed` on the
/// wire, `ClientboundPlayerAbilitiesPacket`.
///
/// # Why this exists at all: it was a complete island
///
/// `ClientEvent::AbilitiesChanged` was decoded correctly in
/// `lodestone_v26_2::adapter::V770Adapter::handle_play_player`, unit-tested at the protocol layer,
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
/// client's double-tap toggle (`LocalPlayer.aiStep`'s `abilities.mayfly` check);
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
    /// Vanilla's own abilities record's field initialisers: everything off, `0.05F` flying and
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
/// declared it in the Configuration `registry_data`. `None` before
/// login, and `None` on a server whose registry did not resolve.
///
/// # Why this is not derivable from [`ServerDimension`]
///
/// [`ServerDimension`] holds a *level* id; this holds the registry entry that
/// level's geometry and lighting rules come from. Deriving one from the other is
/// exactly the mistake a name match makes: a data pack can point `mypack:mine` at the
/// vanilla overworld type, or give `minecraft:overworld` a 1024-tall custom type.
/// The two are folded together in [`apply_local_player_state`] off two events
/// the adapter emits back to back, so they can never disagree about *when* they
/// moved.
///
/// **`None` must not be read as "the overworld".** It means the server said
/// nothing usable, and every consumer has to state its own fallback — see
/// `lodestone_shell::mesher::sky_default_for_dimension`, which keeps its
/// original name match for exactly this case.
/// # Why the flat-world flag lives here too
///
/// `is_flat` is not part of the `minecraft:dimension_type` registry entry —
/// it comes straight off the login/respawn packet. It is folded into the same
/// component because it arrives on the same event, changes on exactly the same
/// edges, and every consumer that wants one wants the other: vanilla keeps
/// both in its own client-level-data side by side, where its own
/// void-darkness-onset-range query
/// reads its own "is flat" flag and the void-fog span reads the dimension type's `min_y`.
/// It is deliberately **not** a field of [`DimensionTypeInfo`], which is a
/// decode of one registry entry and must stay one-sourced.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct ServerDimensionType {
    /// The resolved dimension type, or `None` when the holder id did not
    /// resolve. **`None` must not be read as "the overworld"** — see this
    /// type's own doc.
    pub info: Option<DimensionTypeInfo>,
    /// Whether the level uses the flat world generator.
    pub is_flat: bool,
}

/// Every biome's `minecraft:visual/sky_color` as the server declared it in the
/// Configuration `registry_data`, **indexed by biome holder id**.
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
/// when on foot — vanilla's own get-vehicle accessor for our own player.
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
