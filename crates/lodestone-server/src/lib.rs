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
//!   [`ServerBound::CarriedItemChanged`] sets the selected hotbar slot, and
//!   [`ServerBound::ContainerClicked`] is **derived** rather than trusted —
//!   [`container_click::do_click`] re-runs vanilla's
//!   `AbstractContainerMenu.doClick` from the click's slot/button/type, and the
//!   client's claimed slot diff is compared against the result and never stored.
//!   (This replaces an earlier scope cut in which the diff was applied verbatim,
//!   which let any client name any item in any slot.)
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

/// Server access control (issue #336): ops, whitelist, player bans and IP bans in
/// vanilla's own four JSON files, enforced at join. Native only — a browser world
/// has no filesystem and no remote players.
#[cfg(not(target_arch = "wasm32"))]
pub mod access;
mod advancements;
mod block_breaking;
mod block_entities;
/// Rolling a broken block's loot table and popping the result as item entities
/// (issue #337's missing consumer) — the join between [`loot`],
/// [`MobSim::spawn_item`] and `server`'s block-break arm.
pub mod block_drops;
mod block_placement;
mod block_support;
/// Bone meal's instant-growth right-click — the rule layer for one item, on top
/// of the growth families [`growth_tick`] already models. Public because the
/// producer is a right-click handler outside the tick loop.
pub mod bone_meal;
/// Placing a boat — `BoatItem.use`'s raytrace and the vehicle it creates. Public
/// because the producer is a `USE_ITEM` handler outside the tick loop, exactly as
/// [`bone_meal`] and [`spawn_egg`] are.
pub mod boat;
mod border;
mod brewing;
mod chunk;
/// Bit-packed per-section block storage for [`chunk::ChunkColumn`] — issue #551,
/// unit U8 of `docs/plans/chunk-lifecycle.md`. Private: the representation is an
/// implementation detail of `ChunkColumn`, which exposes it only as
/// `append_section_cells`/`blocks_heap_bytes`.
mod chunk_blocks;
pub mod chunk_nbt;
mod chunk_store;
mod command;
/// The built-in server command tree (`/gamerule`, …) — issue #48. **Was an
/// orphan file, never declared and therefore never compiled**; see
/// `docs/game-rules.md`.
///
/// Public as of the command-dispatcher unit: the wire-parity gates live in
/// `crates/protocol/v770/tests/` (they need a real `V770Adapter` to decode the
/// captured vanilla tree, which this crate cannot reach), so `ServerCommands` and
/// its projection have to be nameable from outside. It was `mod commands;` while
/// it was an island, which is part of how the island survived.
pub mod commands;
mod composter;
/// The dimensions this server hosts and their geometry (`docs/nether-portals.md`).
/// Public because [`ChunkSource::sibling`] and [`ChunkSource::dimension`] name
/// [`dimension::Dimension`], and because a host building a multi-dimension world
/// constructs [`dimension::DimensionalSource`] itself.
pub mod dimension;
/// Nether portal frame detection, ignition, destination search and the per-player
/// transition counter (`docs/nether-portals.md`). Public for the same reason
/// [`fire`] is: anything that writes a `nether_portal` block owes
/// [`portal::PortalIndex`] an entry, or the return trip builds a duplicate.
pub mod portal;
/// Server-side `doClick` (the container-click state machine): derives the result
/// of a click from the slot/button/click-type the wire carries, rather than
/// applying the client's claimed slot diff.
pub mod container_click;
/// Issue #529: the server-authoritative crafting grid and the bundled recipe
/// corpus it re-derives a result from. Public because a host may want to read
/// the corpus, and because `CraftingState` is named by the container plumbing.
pub mod crafting;
pub mod ecs;
/// The workstation economy (anvil, grindstone, smithing table, enchanting
/// table): the vanilla `minecraft:enchantment` registry census shared by all
/// four, and each station's own cost/result formula.
pub mod anvil;
pub mod enchantment_data;
pub mod enchanting;
pub mod smithing;
/// Issue #530: sounds, particles and level events the server owns. Public
/// because `ServerProtocol`'s three new encoders name [`effects::WorldEffect`].
pub mod effects;
mod fall;
/// Fire spread and burnout on the block-tick queue (`docs/fire-spread.md`).
/// Public for the same reason [`fluid`] is: any code that writes a fire block
/// owes [`fire::ticks_after_edit`] for that position, or the fire is inert.
pub mod fire;
/// Explosion block destruction — the ray-sampled blast on real blast resistance
/// (`docs/explosion-blocks.md`). Public because the detonation producer lives
/// outside the tick loop.
pub mod explosion_blocks;
/// Water and lava spread on the scheduled-tick queue (`docs/fluid-spread.md`).
/// Public because the tick loop is not the only intended caller: any code that
/// edits a block owes [`fluid::ticks_after_edit`] for that position.
pub mod fluid;
/// Hunger: exhaustion, saturation, natural regeneration and starvation
/// (`docs/hunger.md`). Public because the exhaustion *producers* live in
/// `crate::server` and a caller needs the constants to charge the right amount.
pub mod food;
/// Experience: the three-regime level curve, the orb denomination ladder and a
/// player's XP state (`docs/experience.md`). Public because every XP *source*
/// (smelting, breeding, fishing, enchanting) lives outside this module and must not
/// grow a second curve.
pub mod experience;
/// Entity burning: ignition, the fire-tick damage interval, lava-vs-fire duration and
/// fire immunity (`docs/burning.md`). Fire *spread* between blocks is [`fire`]'s;
/// this is the entity-facing half. Public because the ignition producers live in
/// `crate::server`.
pub mod burning;
/// The general server-side status-effect registry (`docs/status-effects.md`):
/// duration countdown, amplifier stacking with vanilla's hidden-effect chain, and the
/// periodic poison/wither/regeneration ticks. Public because it is the *shared store*
/// every consumer should read — `lodestone_physics::effect` is a movement classifier
/// over an id and an amplifier, not a place to keep effect state.
pub mod mob_effects;
mod furnace;
/// The world's typed game-rule registry (issue #327). **Was an orphan file too**
/// — none of its 780 lines, including `game_rule_defaults_match_the_jar`, was in
/// the crate at all.
pub mod game_rules;
mod gravity_tick;
mod growth_tick;
mod hand_use;
mod hopper;
mod integrated;
mod inventory;
/// `Item.use`'s ordered arms — eating and drinking, and equipping armour by
/// right-click. The join between `crate::food`'s arithmetic, the armour slots
/// `crate::inventory` already models, and the `UseItem` packet that reached
/// neither.
mod item_use;
/// The join burst's generation scheduler (`docs/plans/worldgen-rewrite.md`
/// Unit 10): a primed sliding window over the wire order, replacing the per-ring
/// barrier `4307b59` reinstated. `pub` because its gates measure in-flight
/// concurrency through it from `tests/`, which `pub(crate)` cannot reach.
pub mod join_scheduler;
/// Whether a block edit changed the light a cell emits, and therefore whether
/// its column has to be re-sent (`docs/server-block-light-updates.md`). Read that
/// module's doc before touching light anywhere: it records what
/// `compute_served_light` was measured to actually compute, and the one trap in
/// `docs/server-chunk-light.md`'s brokered seam patch.
pub mod light;
/// Loot-table loading and rolling (issue #337): parses Mojang's datapack
/// loot-table JSON from the bundled `assets/loot_table/` set and rolls it with
/// the server's deterministic RNG for the empty loot context.
pub mod loot;
mod mob_spawn;
mod mobs;
/// Natural mob spawning against a live world (issues #221/#222): the per-species
/// `SpawnPlacements` table, a per-column light cache over the real light engine,
/// and the `NaturalSpawner` that runs vanilla's cluster loop over real terrain
/// and biome spawn lists. Driven by `tick::run_tick_loop`.
pub mod natural_spawn;
mod neighbor_update;
/// Pistons (issue #316): the structure resolver, the quasi-connectivity signal
/// rule, and the move. Public because the resolver's order is the behaviour and
/// gates outside this crate assert it.
pub mod piston;
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
/// `docs/plans/redstone-execution-model.md`'s U1: structural counters through
/// the redstone notification/reaction/scheduling path, feature-gated behind
/// `redstone-counters` (default off) — see this module's own doc comment.
/// `pub`, matching `lodestone-worldgen-core::counters`'s own visibility: a
/// future measurement harness (U6's bench, or a `tests/` gate) reads
/// `snapshot()`/`reset()` from outside this crate.
pub mod redstone_counters;
mod redstone_diode;
mod redstone_dispenser;
/// Issue #315/#317's end-to-end gates: repeater delay/locking, comparator
/// modes and observer pulse width, driven through the production entry point
/// against values measured on a real 26.2 server. Test-only.
#[cfg(test)]
mod redstone_diode_oracle_gate;
mod redstone_note_block;
mod redstone_observer;
mod redstone_openable;
/// Issue #314's end-to-end gate: redstone propagation driven through the
/// production entry point against values measured on a real 26.2 server.
/// Test-only — it holds the oracle table and the gates, no production code.
#[cfg(test)]
mod redstone_oracle_gate;
/// `docs/plans/redstone-execution-model.md`'s U0: an order-sensitive oracle
/// corpus (a repeater-locked latch raced against its own scheduled flip),
/// measured on a real 26.2 server — the safety net the plan requires before
/// any execution-model rework. Test-only — it holds the oracle readings and
/// the gates, no production code.
#[cfg(test)]
mod redstone_order_oracle_gate;
/// Issue #465's delayed half: a component a player mutates must flip at the
/// tick the live 26.2 server flipped it, and the flip must reach the wire —
/// driven through the real `tick::run_tick_loop` rather than through
/// `propagate_and_react` directly, because "does anything drain the queue this
/// schedules into" is exactly what the other two gates structurally cannot
/// see. Test-only.
#[cfg(test)]
mod redstone_placement_gate;
mod redstone_rail;
mod redstone_target;
mod redstone_torch;
mod redstone_tripwire;
mod redstone_wire;
/// Per-player `.dat` persistence (issue #302) — inventory, position, health and
/// game mode across a disconnect. Native only, for the same reason
/// `region_source` is: it is a `std::fs` schema over `lodestone-anvil`.
#[cfg(not(target_arch = "wasm32"))]
pub mod player_data;
/// Per-chunk entity persistence (issue #303) — the `entities/` region set that
/// makes a mob and a dropped item survive a restart. Native only, like
/// `player_data` and `region_source`.
#[cfg(not(target_arch = "wasm32"))]
pub mod entity_storage;
/// Per-section point-of-interest persistence (issue #303's second half) — the
/// `poi/` region set. Native only, like `entity_storage`.
#[cfg(not(target_arch = "wasm32"))]
pub mod poi_storage;
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
mod sleep;
mod spawn;
/// Spawn eggs: which entity a `*_spawn_egg` names, and where a right-click puts
/// it. Public because the right-click dispatcher and a future dispenser
/// behaviour are both callers, and because the item→entity derivation is worth
/// asserting from an integration test rather than only from inside the crate.
pub mod spawn_egg;
/// Structure chests (issue #337): the data-marker pass that fills a shipwreck's,
/// igloo's or ocean ruin's chest with a rolled loot table at generation time.
mod structure_loot;
/// Gates for the support-collapse pass — `server::collapse_unsupported` driven
/// against a rig world, one arm per block family shape. `#![cfg(test)]` inside,
/// like the redstone gate modules beside it.
mod support_collapse_gate;
mod tick;
/// Which chunk columns the world tick loop simulates, and how that set **follows
/// the players** rather than sitting on world spawn. Public because a host wants
/// to publish player anchors into it and because its gates assert from outside.
pub mod tick_area;
mod vitals;
mod weather;
/// Lightning: per-chunk strike-target selection during a thunderstorm, the
/// `LightningBolt` life-cycle and its entity-facing effects (`docs/lightning.md`).
/// Public because spawning the bolt as a real entity and applying an effect
/// both happen outside this module, in `crate::mobs` — this module only
/// decides, and hands its decision off through [`lightning::LightningFeed`].
pub mod lightning;
/// Vanilla's regional difficulty (`DifficultyInstance`) — the scalar grown
/// from world difficulty, elapsed game time and moon phase
/// (`docs/regional-difficulty.md`). Public because [`lightning`]'s
/// skeleton-horse-trap roll reads it from outside this crate's tick loop.
pub mod regional_difficulty;
mod world_spawn;
/// One shared, persistable store for the world's scalars — game rules,
/// difficulty and the clock (issues #327, #328, #323). Public because a host and
/// the gates both read it.
pub mod world_state;
mod worldgen_data;

#[cfg(not(target_arch = "wasm32"))]
pub use access::{AccessHandle, AccessLists, BanEntry, JoinRefusal, OpEntry, WhitelistEntry};
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
pub use chunk::{
    ChunkColumn, ChunkSource, NetherChunkSource, OverworldChunkSource, WorldgenChunkSource,
};
// Issue #505: `chunk_store::ChunkStore` itself stays crate-private (its methods
// are `pub(crate)` and `IntegratedServer` is the only thing that should build
// one), but its **capacity policy** is public, for one reason: the policy is a
// claim about what `crate::server` streams, and the gate that joins the two —
// `tests/view_radius_store_capacity.rs` — has to measure the streamed view
// through the public API and then compare it against the policy. A gate that
// could only see one side of that join would be `decode(encode(x)) == x`.
// Prefixed `STORE_`/`store_` because `MAX_CAPACITY` and `DEFAULT_CAPACITY` are
// far too generic for a crate root that already re-exports three other
// `DEFAULT_*` constants.
pub use chunk_store::{
    CONCURRENT_SCAN_COLUMNS as STORE_CONCURRENT_SCAN_COLUMNS,
    DEFAULT_CAPACITY as STORE_CAPACITY_FLOOR,
    FULLY_RESIDENT_VIEW_RADIUS as STORE_FULLY_RESIDENT_VIEW_RADIUS,
    MAX_CAPACITY as STORE_CAPACITY_CEILING, capacity_for_view_radius as store_capacity_for_view_radius,
    integrated_capacity_for_view_radius as integrated_store_capacity_for_view_radius, view_columns,
};
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
#[cfg(not(target_arch = "wasm32"))]
pub use integrated::{LanConfig, LanDiscovery};
pub use crafting::{BUNDLED_CRAFTING_RECIPES, CraftingState, recipe_book};
pub use inventory::{HOTBAR_SIZE, OFFHAND_NATIVE, PLAYER_NATIVE_SIZE, PlayerInventory};
pub use loot::{LootContext, LootTable, LootTableBuilder, LootTableResolver, LootTableSet, roll_loot};
pub use mob_spawn::{
    DespawnOutcome, MAGIC_NUMBER, MobCategory, SpawnCandidate, SpawnCandidateSource, SpawnRng,
    SpawnState, allowed_in_peaceful, check_despawn, resolve_mob_shape,
};
pub use natural_spawn::NaturalSpawner;
pub use mobs::{
    AttackOutcome, ChunkWorld, Detonation, InteractOutcome, LiveMobSource, MobHandle, MobOwner,
    MobSim, PerceivedPlayer, PlayerIdentity, PlayerPerception, SimMob,
};
pub use neighbor_update::{Direction, NeighborPropagator, Notification, UPDATE_ORDER};
pub use players::{
    PLAYER_ENTITY_ID_BASE, ChatLine, PlayerAwareSource, PlayerListStreamer, PlayerRegistry,
    PlayerTicket, PlayerView,
};
pub use commands::{
    CommandOutcome, CommandSource, DirectedEffect, Effect, PlayerCandidate, ServerCommands,
};
pub use plugin_channels::{
    ClientChannels, PluginChannelHandler, PluginChannelRegistry, REGISTER_CHANNEL,
    UNREGISTER_CHANNEL,
};
pub use protocol::{
    Abilities, ChunkEncoder, EntitySnapshot, MerchantOfferOut, MetadataField, PlayerListing,
    ResourcePackPush, ServerBound, ServerDirective, ServerProtocol, WorldgenScope,
};
/// The `EntityEvent` status bytes [`ServerProtocol::encode_entity_event`] carries.
///
/// Re-exported as a module rather than flattened into the list above because the
/// names (`DEATH`, `TAMING_SUCCEEDED`, …) are only unambiguous behind the
/// `entity_event::` prefix — a bare `DEATH` at a call site says nothing about
/// which of vanilla's several death-adjacent packets it belongs to.
pub mod entity_event {
    pub use crate::protocol::entity_event::*;
}
pub use random_tick::{
    DEFAULT_RANDOM_TICK_SPEED, GrassOutcome, RandomTickEvent, RandomTickScheduler,
    can_propagate_onto, grass_random_tick, is_air_variant, is_randomly_ticking,
    next_random_tick_pos,
};
#[cfg(not(target_arch = "wasm32"))]
pub use rcon::{DEFAULT_RCON_PORT, RconConfig};
pub use scheduled_tick::{ScheduledTick, ScheduledTickQueue, TickPriority};
pub use server::{
    // Issue #551's gate in `tests/view_radius_store_capacity.rs` asserts at compile
    // time that the radius it raises the slider to is one `ViewTracker::max_radius`
    // actually permits — a premise it must not restate as a literal.
    MAX_CLIENT_VIEW_RADIUS,
    EntitySource, NoEntities, ResourcePackPushFeed, ServeSummary, ServerError, serve_connection,
    serve_connection_with_commands, serve_connection_with_mob_events,
    serve_connection_with_plugin_channels, serve_connection_with_resource_pack,
};
#[cfg(not(target_arch = "wasm32"))]
pub use server::serve_connection_with_access;
#[cfg(not(target_arch = "wasm32"))]
pub use server::{OnlineModeConfig, serve_connection_with_online_mode};
pub use tick::{BlockTickFeed, ExplosionFeed, TickClock, TickStats};
pub use weather::{WeatherEvent, WeatherFeed, WeatherState};
pub use vitals::{DROWN_DAMAGE, EYE_HEIGHT, MAX_AIR_SUPPLY, MAX_HEALTH, PlayerVitals, VitalsTick};
pub use worldgen_data::{
    bundled_biome_spawners, bundled_worldgen_serves, nether_chunk_source, nether_generator,
    overworld_chunk_source, overworld_chunk_source_of_type, overworld_generator,
    overworld_generator_of_type, BUNDLED_WORLDGEN_SCOPE, WorldType,
};

// Re-exported so a caller (e.g. the shell's local world) can name the generator
// and its output without depending on `lodestone-worldgen` directly.
pub use lodestone_worldgen::overworld::{GeneratedColumn, OverworldGenerator};

/// `Heightmap.Types.MOTION_BLOCKING`'s registry id, re-exported for the same
/// reason (issue #516): `lodestone-worldgen` is only a *dev*-dependency of
/// `lodestone-v770`, so the encoder that writes
/// [`ChunkColumn::motion_blocking`] into the chunk packet cannot name the
/// constant at its source. Re-exported rather than restated so the id is never
/// retyped from memory.
pub use lodestone_worldgen::overworld::MOTION_BLOCKING_HEIGHTMAP_TYPE_ID;
