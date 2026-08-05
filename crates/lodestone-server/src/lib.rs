//! Lodestone's integrated server — the singleplayer host.
//!
//! In vanilla, singleplayer *is* an integrated server the client connects to.
//! Lodestone adopts that shape deliberately (plan §8): the integrated server
//! speaks the **same** [`lodestone_net::Connection`] over a [`Transport`], so
//! singleplayer and multiplayer exercise the same code path and open-to-LAN
//! falls out for free. Over an in-memory
//! [`memory_pair`](lodestone_net::memory_pair) duplex the client and server run
//! in one process; swap the transport for a `TcpStream` and the identical loop
//! serves LAN.
//!
//! # What is version-free here, and what is a seam
//!
//! This crate is **version-free**, exactly like [`lodestone_worldgen`]. It owns:
//!
//! * [`ChunkSource`] — how the server obtains terrain for a chunk column.
//!   [`OverworldChunkSource`] (built by [`overworld_chunk_source`]) backs it
//!   with the composed, JVM-verified overworld generator, so a served chunk
//!   carries real vanilla block states; [`WorldgenChunkSource`] is a
//!   solidity-only stand-in kept only for the transport tests.
//! * [`ServerProtocol`] — the **seam** a protocol/version crate must implement
//!   to lower client-bound packets and lift server-bound ones. It is the mirror
//!   of the client's `VersionAdapter`: this crate never names a wire format,
//!   packet id, or NBT layout, so dropping a version drops its adapter, never
//!   this loop.
//! * [`serve_connection`] — the generic driver that runs the handshake → login
//!   → play sequence over any [`Transport`] using a [`ServerProtocol`] and a
//!   [`ChunkSource`].
//! * [`IntegratedServer`] — the reachable lifecycle wrapper a shell holds to
//!   *start* singleplayer (in-memory) or open-to-LAN (TCP), with a clean
//!   shutdown that never leaks the serving task.
//! * [`MobSim`] / [`ChunkWorld`] — the server-side mob simulation. In vanilla
//!   the *server* ticks mob AI and streams positions; the client interpolates
//!   and runs none. So this crate is mob AI's home: [`ChunkWorld`] adapts the
//!   version-free solid/air terrain into `lodestone-entity`'s `PathWorld` seam,
//!   and [`MobSim`] ticks goal-driven `NavigatingMob`s over it. The encoder
//!   half of streaming the result to a client (`ServerProtocol::encode_add_entity`
//!   / `encode_entity_update` / `encode_remove_entity`) is a separate seam a
//!   version crate implements, but as of issue #217 something in this crate
//!   also *drives* it in production: [`IntegratedServer::open_in_memory_with_mobs`]
//!   spawns a background task (`tick::run_tick_loop`, issue #284 — before that,
//!   `mobs::run_mob_tick_loop`) that owns a live
//!   `MobSim` and republishes its snapshots through [`LiveMobSource`], an
//!   [`EntitySource`] the existing `serve_connection` streaming pass already
//!   diffs against every connection reactively. The same loop also ticks
//!   every registered block entity and tracks MSPT/TPS/overrun accounting
//!   (issue #285) — see the `tick` module's own doc comment for the full
//!   before/after picture of every timer this crate had.
//! * [`PlayerVitals`] — the server-authoritative air-supply countdown and
//!   drowning damage (issue #267). Player-only for now (see its own module
//!   doc comment for why `MobSim` does not yet participate); ticked from
//!   `serve_play`'s per-tick timer against a submersion test over
//!   [`ChunkSource::block_state`] at the tracked player position, with the
//!   two new [`ServerProtocol`] methods (`encode_air_supply_update`,
//!   `encode_set_health`) defaulting to emit nothing, exactly like the
//!   keep-alive/time/view-streaming encoders above.
//! * [`FallTracker`] — server-authoritative fall damage (issue #265),
//!   packet-driven rather than timer-driven like [`PlayerVitals`]: it is fed
//!   the `(y, on_ground)` pair every [`ServerBound::PlayerMoved`] now
//!   carries and reports damage the moment a landing crosses vanilla's
//!   safe-fall-distance threshold, applied through the same
//!   [`lodestone_entity::apply_reductions`] pipeline
//!   [`crate::mobs::SimMob::apply_damage`] uses for mobs.
//! * [`PlayerInventory`] — the server-authoritative model for the player's
//!   own 41-slot inventory (hotbar, main storage, armour, off-hand), the
//!   prerequisite the server-side decode of `SET_CARRIED_ITEM` and
//!   `CONTAINER_CLICK` needed and did not have (see that module's own doc
//!   comment for the vanilla `Inventory` citation and the menu-slot →
//!   native-index table it mirrors from `lodestone-game`'s client-side
//!   `Menu`). `crate::server`'s `dispatch_play_packet` is the consumer:
//!   [`ServerBound::CarriedItemChanged`] sets the selected hotbar slot,
//!   [`ServerBound::ContainerClicked`] against window `0` applies the
//!   client's own predicted per-slot diff directly (see that variant's doc
//!   comment for why trusting the diff, rather than re-deriving vanilla's
//!   full `doClick` state machine server-side, is the deliberate scope for
//!   this landing).
//! * [`BlockEntityRegistry`] / [`BlockEntityHandle`] — the `BlockPos`-keyed
//!   home for the four block-entity simulations (`composter`/`furnace`/
//!   `hopper`/`brewing`, `docs/block-entities.md`), closing that doc's first
//!   named gap. [`crate::server`]'s `apply_use_item_on` is the producer
//!   (placing a furnace/composter/hopper/brewing-stand item inserts a fresh
//!   entry instead of always writing stone — the doc's second named gap);
//!   [`IntegratedServer::open_in_memory_with_mobs`] spawns
//!   `tick::run_tick_loop`, which ticks block entities from the same unified
//!   loop as the mob sim (issue #284 — before that, a second, separate
//!   `block_entities::run_block_entity_tick_loop` task), so a furnace placed
//!   in a real singleplayer session actually
//!   ticks. The doc's third named gap (container packets so a client can
//!   *see* inside one) is not closed by this landing; see that doc for the
//!   current state.
//!
//! Wiring a real vanilla client to this server end-to-end requires the version
//! crate to provide client-bound *encoders* (join game, registry data,
//! `level_chunk_with_light`) and server-bound *decoders*. The client stack is
//! decode-only today, so that encoder half is a reported seam (see the crate
//! README notes / task report), not something this version-free crate may
//! implement itself without coupling to a protocol number.
//!
//! [`Transport`]: lodestone_net::Transport

mod advancements;
mod block_entities;
mod border;
mod brewing;
mod chunk;
pub mod chunk_nbt;
mod chunk_store;
mod command;
mod composter;
pub mod ecs;
mod fall;
mod furnace;
mod gravity_tick;
mod growth_tick;
mod hopper;
mod integrated;
mod inventory;
/// Loot-table loading and rolling (issue #337): parses Mojang's datapack
/// loot-table JSON from the bundled `assets/loot_table/` set and rolls it with
/// the server's deterministic RNG for the empty loot context.
pub mod loot;
mod mob_spawn;
mod mobs;
mod neighbor_update;
mod players;
mod plugin_channels;
mod protocol;
/// The GameSpy4 / UT3 server-query protocol (issue #332): a UDP listener
/// answering the challenge-response dance server-list aggregators use, wired
/// into `IntegratedServer::bind` (native targets only — the socket half of the
/// module is `cfg`-gated, the protocol logic compiles everywhere).
pub mod query;
mod random_tick;
mod redstone;
mod redstone_diode;
/// Issue #315/#317's end-to-end gates: repeater delay/locking, comparator
/// modes and observer pulse width, driven through the production entry point
/// against values measured on a real 26.2 server. Test-only.
#[cfg(test)]
mod redstone_diode_oracle_gate;
mod redstone_observer;
/// Issue #314's end-to-end gate: redstone propagation driven through the
/// production entry point against values measured on a real 26.2 server.
/// Test-only — it holds the oracle table and the gates, no production code.
#[cfg(test)]
mod redstone_oracle_gate;
/// Issue #465's delayed half: a component a player mutates must flip at the
/// tick the live 26.2 server flipped it, and the flip must reach the wire —
/// driven through the real `tick::run_tick_loop` rather than through
/// `propagate_and_react` directly, because "does anything drain the queue this
/// schedules into" is exactly what the other two gates structurally cannot
/// see. Test-only.
#[cfg(test)]
mod redstone_placement_gate;
mod redstone_torch;
mod redstone_wire;
/// World persistence (issue #437). Native only: a browser singleplayer world
/// has no filesystem, and `lodestone-anvil` is a `std::fs` crate — see this
/// crate's `Cargo.toml` for the matching target-gated dependency.
#[cfg(not(target_arch = "wasm32"))]
pub mod region_source;
/// The Source RCON listener (issue #331). Native only, like `region_source`:
/// the listener is a `tokio::net::TcpListener`, and a browser singleplayer
/// world has no network listener for an admin console.
#[cfg(not(target_arch = "wasm32"))]
mod rcon;
mod scheduled_tick;
mod server;
mod spawn;
mod tick;
mod vitals;
mod weather;
mod world_spawn;
mod worldgen_data;

pub use advancements::{
    Advancement, AdvancementError, AdvancementManager, AdvancementProgress, AdvancementProgressUpdate,
    AdvancementUpdate, GrantOutcome, PlayerAdvancementState, PlayerProgress, PlayerStatistics, StatKey,
    StatType,
};
pub use block_entities::{BlockEntity, BlockEntityHandle, BlockEntityRegistry, block_entity_for_item};
pub use border::{ABSOLUTE_MAX_SIZE, MAX_CENTER_COORDINATE, MAX_SIZE, BorderFeed, WorldBorder};
pub use brewing::{
    BREW_TIME_TICKS, Bottle, BottleKind, BrewTick, BrewingStand, FUEL_USES, has_mix, is_ingredient,
    mix_bottle,
};
pub use chunk::{ChunkColumn, ChunkSource, OverworldChunkSource, WorldgenChunkSource};
pub use command::{
    CommandCaller, CommandDispatch, CommandResponse, CommandSink, UNKNOWN_COMMAND,
};
pub use composter::{
    Composter, InsertOutcome as ComposterInsertOutcome, MAX_FILL_LEVEL as COMPOSTER_MAX_FILL_LEVEL,
    READY_DELAY_TICKS as COMPOSTER_READY_DELAY_TICKS, READY_LEVEL as COMPOSTER_READY_LEVEL,
    compostable_chance,
};
pub use fall::{FALL_DAMAGE_MULTIPLIER, FallTracker, SAFE_FALL_DISTANCE};
pub use furnace::{
    BURN_COOL_SPEED, CookingRecipe, DEFAULT_COOK_TIME, Furnace, FurnaceKind, FurnaceTick,
    MAX_STACK_SIZE as FURNACE_MAX_STACK_SIZE, base_burn_duration, effective_burn_duration,
    experience_for as furnace_experience_for, recipe_for as furnace_recipe_for,
};
pub use hopper::{
    HOPPER_SIZE, Hopper, HopperTick, MAX_STACK_SIZE as HOPPER_MAX_STACK_SIZE,
    TRANSFER_COOLDOWN_TICKS as HOPPER_TRANSFER_COOLDOWN_TICKS, try_move_one_item,
};
pub use integrated::IntegratedServer;
pub use inventory::{HOTBAR_SIZE, OFFHAND_NATIVE, PLAYER_NATIVE_SIZE, PlayerInventory};
pub use loot::{LootContext, LootTable, LootTableBuilder, LootTableResolver, LootTableSet, roll_loot};
pub use mob_spawn::{
    DespawnOutcome, MAGIC_NUMBER, MobCategory, SpawnCandidate, SpawnCandidateSource, SpawnRng,
    SpawnState, check_despawn, resolve_mob_shape,
};
pub use mobs::{
    AttackOutcome, ChunkWorld, Detonation, LiveMobSource, MobHandle, MobSim, PlayerPerception,
    SimMob,
};
pub use neighbor_update::{Direction, NeighborPropagator, Notification, UPDATE_ORDER};
pub use players::{
    PLAYER_ENTITY_ID_BASE, ChatLine, PlayerAwareSource, PlayerListStreamer, PlayerRegistry,
    PlayerTicket, PlayerView,
};
pub use plugin_channels::{
    ClientChannels, PluginChannelHandler, PluginChannelRegistry, REGISTER_CHANNEL,
    UNREGISTER_CHANNEL,
};
pub use protocol::{
    EntitySnapshot, MetadataField, PlayerListing, ResourcePackPush, ServerBound, ServerDirective,
    ServerProtocol,
};
pub use random_tick::{
    DEFAULT_RANDOM_TICK_SPEED, GrassOutcome, RandomTickEvent, RandomTickScheduler,
    can_propagate_onto, grass_random_tick, is_air_variant, is_randomly_ticking,
    next_random_tick_pos,
};
#[cfg(not(target_arch = "wasm32"))]
pub use rcon::{DEFAULT_RCON_PORT, RconConfig};
pub use scheduled_tick::{ScheduledTick, ScheduledTickQueue, TickPriority};
pub use server::{
    EntitySource, NoEntities, ResourcePackPushFeed, ServeSummary, ServerError, serve_connection,
    serve_connection_with_commands, serve_connection_with_mob_events,
    serve_connection_with_plugin_channels, serve_connection_with_resource_pack,
};
pub use tick::{BlockTickFeed, ExplosionFeed, TickClock, TickStats};
pub use weather::{WeatherEvent, WeatherFeed, WeatherState};
pub use vitals::{DROWN_DAMAGE, EYE_HEIGHT, MAX_AIR_SUPPLY, MAX_HEALTH, PlayerVitals, VitalsTick};
pub use worldgen_data::{overworld_chunk_source, overworld_generator};

// Re-exported so a caller (e.g. the shell's local world) can name the generator
// and its output without depending on `lodestone-worldgen` directly.
pub use lodestone_worldgen::overworld::{GeneratedColumn, OverworldGenerator};
