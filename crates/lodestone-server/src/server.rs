//! The generic integrated-server driver.
//!
//! [`serve_connection`] runs the server side of a single client connection over
//! any [`Transport`]: it reads packets through the shared
//! [`Connection`](lodestone_net::Connection) codec, lifts them with a
//! [`ServerProtocol`], plays the login sequence, and streams the initial view's
//! chunks from a [`ChunkSource`]. The identical loop serves an in-memory
//! [`memory_pair`](lodestone_net::memory_pair) client (singleplayer) or a
//! `TcpStream` client (open-to-LAN).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

use lodestone_core::State;
use lodestone_entity::{DamageFlags, ItemLifecycle};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, Difficulty, ItemStack, Rotation, Text, TextContent, Vec3,
};
use lodestone_data::block_items;
use lodestone_net::{Connection, NetError, Transport};

use crate::advancements::AdvancementManager;
use crate::block_entities::{BlockEntity, BlockEntityHandle, block_entity_for_item};
use crate::border::BorderFeed;
use crate::brewing::{Bottle, BottleKind, is_ingredient};
use crate::composter::{InsertOutcome, compostable_chance};
use crate::command::{CommandCaller, CommandDispatch, CommandSession};
use crate::chunk::{
    AIR, ChunkColumn, ChunkSource, generate_columns_offloaded, generate_columns_parallel,
    is_air_or_fluid, is_water,
};
use crate::fall::FallTracker;
use crate::inventory::{ContainerMenuSlot, PlayerInventory, container_menu_slot};
use crate::mob_spawn::SpawnRng;
use crate::mobs::{MobHandle, PlayerPerception};
use crate::neighbor_update::Direction;
use crate::players::{ChatLine, PlayerListStreamer, PlayerRegistry, PlayerTicket};
use crate::plugin_channels::{ClientChannels, PluginChannelRegistry};
use crate::protocol::{
    EntitySnapshot, ResourcePackPush, ServerBound, ServerDirective, ServerProtocol,
};
use crate::redstone::{COMPARATOR, OBSERVER, REPEATER};
use crate::redstone_diode::{set_comparator, set_repeater};
use crate::redstone_observer::set_observer;
use crate::scheduled_tick::{ScheduledTick, ScheduledTickQueue};
use crate::sleep::{SleepEvent, SleepFeed, SleepVote};
use crate::tick::{BlockTickFeed, ExplosionFeed};
use crate::weather::WeatherFeed;
use crate::vitals::{EYE_HEIGHT, PlayerVitals};
use crate::world_spawn::{RespawnPoint, find_initial_spawn, is_bed_block, is_legal_bed_respawn};

/// Server-initiated keep-alive interval, and the width of the window in
/// which an echo must arrive before the connection is treated as dead.
///
/// Vanilla's `LATENCY_CHECK_INTERVAL` and `CLOSED_LISTENER_TIMEOUT`
/// (`ServerCommonPacketListenerImpl.java:35-36`) are both the literal
/// constant `15000` (milliseconds) — **not** two different numbers.
/// `keepConnectionAlive` (`ServerCommonPacketListenerImpl.java:118-133`)
/// sends a fresh challenge once `now - keepAliveTime >= 15000`, and
/// disconnects immediately if the *previous* challenge is still pending at
/// that point — so an unanswered challenge is caught within one more
/// interval of being sent (up to ~15s later), not two intervals (~30s).
#[cfg(not(target_arch = "wasm32"))]
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(15_000);

/// MOTD reported in the server-list status reply (issue #277).
///
/// Vanilla's own default is `server.properties`' `motd=A Minecraft Server`
/// (`net/minecraft/server/dedicated/DedicatedServerProperties.java`, and the
/// `.cache/mc/26.2/server.properties` this repo's oracles run against). This
/// crate has no properties file, so the equivalent constant names Lodestone
/// instead of impersonating vanilla.
pub const STATUS_MOTD: &str = "A Lodestone Server";

/// Player cap reported in the server-list status reply, matching the
/// `max_players` this crate's join sequence already reports in-game (the
/// `GameLogin` body every `ServerProtocol::begin_play` builds). The two are
/// deliberately the same number: a client that sees `0/20` in its list and then
/// joins a 20-slot server should not see the cap change.
pub const STATUS_MAX_PLAYERS: i32 = 20;

/// Issue #325 / `crate::sleep`: the server-side entity id of the single local
/// player in a singleplayer world — the roster key a connection with no
/// [`PlayerRegistry`] uses when it votes (see `SleepVoteInner.sleepers`'s doc
/// comment). Matches `crates/protocol/v770/src/server_protocol.rs`'s
/// `LOCAL_PLAYER_ENTITY_ID`, which is what the v770 encoder believes the local
/// player's id is; keeping the two constants equal is the join, and the
/// reason `crate::sleep`'s module doc names this crate as the source.
pub(crate) const LOCAL_PLAYER_ENTITY_ID: i32 = 1;

/// The disconnect reason for an unanswered keep-alive (issue #279).
///
/// Vanilla's is exactly `Component.translatable("disconnect.timeout")`
/// (`net/minecraft/server/network/ServerCommonPacketListenerImpl.java:37`, sent
/// at `:86`), so the key is not ours to choose. The `fallback` is vanilla's own
/// English string for that key, read from
/// `.cache/mc/26.2/client-src/assets/minecraft/lang/en_us.json:3498`
/// (`"disconnect.timeout": "Timed out"`) — not invented here.
///
/// Carrying a fallback at all is a deliberate improvement over vanilla's bare
/// `translatable`, and it is a real vanilla feature, not an extension:
/// `TranslatableContents` resolves `currentLanguage.getOrDefault(key, fallback)`
/// (`network/chat/contents/TranslatableContents.java:90`). So a real client shows
/// its own localized "Timed out", while any client that cannot resolve the key —
/// including *our* client today, which renders raw translation keys (issue #68) —
/// shows readable English instead of the literal string `disconnect.timeout`.
fn timeout_reason() -> Text {
    Text {
        content: TextContent::Translate {
            key: "disconnect.timeout".to_owned(),
            with: Vec::new(),
            fallback: Some("Timed out".to_owned()),
        },
        ..Text::default()
    }
}

/// Whether `name` is a username vanilla's own server would accept.
///
/// `StringUtil.isValidPlayerName` (`net/minecraft/util/StringUtil.java:66-68`):
/// at most 16 characters, and **no** character `<= 32` or `>= 127` — i.e. every
/// char must be printable ASCII, excluding space. Vanilla checks this on the
/// login-phase `hello` packet (`ServerLoginPacketListenerImpl.java:120`).
///
/// Note the bound is on `char`s, matching vanilla's `name.chars()` (Java code
/// points) rather than bytes: a name of 16 multi-byte characters is length-16 to
/// vanilla, and every one of those characters is `>= 127` and so already rejected.
fn is_valid_player_name(name: &str) -> bool {
    name.chars().count() <= 16 && name.chars().all(|c| c > ' ' && (c as u32) < 127)
}

/// The disconnect reason for a username our server will not accept.
///
/// Unlike [`timeout_reason`], the *text* here is ours: vanilla rejects an invalid
/// name by throwing (`Validate.validState(StringUtil.isValidPlayerName(...))`,
/// `ServerLoginPacketListenerImpl.java:120`), which closes the connection with no
/// translatable reason at all. Rejecting is faithful; explaining is an
/// improvement, so this is a plain literal rather than a translation key we would
/// have had to invent.
fn invalid_username_reason() -> Text {
    Text::literal("Invalid username")
}

/// Cadence of the periodic time-of-day broadcast.
///
/// Vanilla re-broadcasts the world's monotonic game time every 20 ticks
/// (`MinecraftServer::forceGameTimeSynchronization`,
/// `MinecraftServer.java:1095-1099`: `if (this.tickCount % 20 == 0)`) —
/// carrying an *empty* clock-update map, which is what tells a client to keep
/// its held day/night anchor rather than resetting it (see
/// `packets::time::SetTime::day_clock`'s doc comment in the `v770` crate).
/// This crate has no fixed server tick loop (see the module docs), so a
/// 1-second wall-clock interval stands in for "every 20 ticks" at vanilla's
/// normal 20 TPS.
#[cfg(not(target_arch = "wasm32"))]
const TIME_SYNC_INTERVAL: Duration = Duration::from_millis(1_000);

/// Milliseconds per tick at vanilla's normal 20 TPS, used to convert
/// wall-clock elapsed time into the tick-based `game_time`
/// [`ServerProtocol::encode_set_time`] carries, in the absence of a real
/// per-tick server loop.
#[cfg(not(target_arch = "wasm32"))]
const MILLIS_PER_TICK: u128 = 50;

/// A bare-handed player's raw melee damage — `Player.createAttributes()`'s
/// own `.add(Attributes.ATTACK_DAMAGE, 1.0)` (`.cache/mc/26.2/src/net/
/// minecraft/world/entity/player/Player.java:208`), **not**
/// `LivingEntity`'s generic `RangedAttribute` default of `2.0` a player would
/// otherwise inherit. This crate has no item/weapon-attribute model for the
/// player (`lodestone_entity::damage`'s own module doc already names this gap
/// for #261), so every hit uses this constant regardless of what is in the
/// main hand — the same "no per-item census, no cooldown ticker" scope
/// `docs/combat.md`'s attack-strength section already discloses for the
/// client side. `Player.attack`'s `baseDamageScaleFactor()` (cooldown-scaled
/// damage) is also not modelled here for the identical reason: no
/// server-tracked attack-strength ticker to read, so every hit is treated as
/// full-strength (`damage.rs`'s own module doc: "no attack-cooldown timer...
/// exists server-side").
const PLAYER_BARE_HAND_ATTACK_DAMAGE: f32 = 1.0;

/// The melee knockback-bonus power a **sprinting** attacker's hit applies,
/// matching `Player.attack`'s `knockbackAttack` bonus exactly:
/// `causeExtraKnockback(entity, this.getKnockback(entity, damageSource) +
/// (knockbackAttack ? 0.5F : 0.0F), ...)` (`Player.java:987-988`), where
/// `knockbackAttack = this.isSprinting() && fullStrengthAttack`
/// (`Player.java:963-966`). `getKnockback` itself resolves to the attacker's
/// `minecraft:attack_knockback` attribute (registry default `0.0`,
/// `Attributes.java:18`) — `0.0` for a bare-handed player, since this crate
/// has no weapon/enchantment model to add to it — so a **non-sprinting**
/// attack's total knockback power is exactly `0.0`. That is not a
/// placeholder: it is the literal jar formula for the one case this crate can
/// currently model (no weapon, no server-tracked attack-cooldown ticker so
/// every hit reads as full-strength), and it is why
/// [`apply_attack`] passes `0.0` unconditionally for a non-sprinting attacker
/// and this constant only for a sprinting one.
const SPRINT_ATTACK_KNOCKBACK_POWER: f64 = 0.5;

/// Cadence of the air-supply/drowning-damage tick ([`crate::vitals`]).
/// Vanilla ticks `LivingEntity.baseTick`'s water-breath block once per real
/// server tick (20 TPS); this crate has no fixed tick loop (see the module
/// docs), so — exactly like [`TIME_SYNC_INTERVAL`] standing in for "every 20
/// ticks" — a wall-clock interval of [`MILLIS_PER_TICK`] stands in for "every
/// tick". Getting this cadence right matters more here than for time-of-day:
/// the drowning countdown's exact tick counts (300 to empty, +20 to the first
/// hit, then every 20 thereafter — see `crate::vitals`'s module doc comment)
/// are the whole point, not an approximation, so this must fire at the real
/// 20 TPS rate rather than some coarser stand-in.
#[cfg(not(target_arch = "wasm32"))]
const VITALS_TICK_INTERVAL: Duration = Duration::from_millis(50);

/// A read-only view of the entities in the world right now, supplied by the
/// caller that owns the simulation and its tick.
///
/// [`serve_connection`] reads snapshots each streaming pass and diffs them
/// against what *this* connection was last sent; it never ticks the simulation
/// itself, so one shared world can feed many connections without double-ticking.
pub trait EntitySource: Send + Sync {
    /// The entities that should currently be visible to the client.
    fn snapshots(&self) -> Vec<EntitySnapshot>;

    /// The registry of connected **players**, if this source tracks them
    /// (issue #438).
    ///
    /// This is the one conduit by which a connection reaches the players
    /// sharing its world, and it exists as a defaulted method on this trait
    /// rather than a new parameter for a specific reason:
    /// [`serve_connection`] and its five sibling entry points are called
    /// directly from `crates/protocol/v770/tests/*`, and adding an argument
    /// would have churned every one of those call sites for a feature none of
    /// them uses. Every pre-existing [`EntitySource`] — [`NoEntities`],
    /// [`crate::LiveMobSource`], [`MobHandle`], and every test double —
    /// inherits `None` and keeps its exact previous behaviour.
    ///
    /// Returning `Some` is what turns on player streaming for a connection:
    /// [`crate::players::PlayerAwareSource`] is the composition production
    /// uses, pairing the mob source with the registry. Note that players are
    /// deliberately **not** reachable through [`snapshots`](Self::snapshots) —
    /// that call has no viewer, and a viewer-less player list is exactly how a
    /// connection would be sent its own entity. See
    /// [`crate::players::PlayerRegistry::view`].
    fn players(&self) -> Option<&PlayerRegistry> {
        None
    }
}

/// Runs one full streaming pass for a connection: tab-list diff first, then the
/// entity diff over the mob source **and** every other connected player.
///
/// The order is load-bearing and the reason this is one function rather than
/// two call sites. A client that receives an `ADD_ENTITY` of type
/// `minecraft:player` before it holds a `PlayerInfo` for that uuid **discards
/// the spawn** — `ClientPacketListener.createEntityFromPacket` returns `null`
/// and logs "Server attempted to add player prior to sending player info"
/// (`.cache/mc/26.2/client-src/net/minecraft/client/multiplayer/
/// ClientPacketListener.java:591-604`). So the roster adds must precede the
/// spawn, in the same pass, and both must come from
/// [`crate::players::PlayerRegistry::view`]'s single lock acquisition — two
/// separate reads could interleave a join between them and produce precisely
/// the dropped spawn.
///
/// `ticket` is this connection's own player registration; its id is what gets
/// excluded, so a connection never receives itself.
fn stream_pass<P, E>(
    proto: &P,
    entities: &E,
    streamer: &mut EntityStreamer,
    player_list: &mut PlayerListStreamer,
    ticket: Option<&PlayerTicket>,
) -> Vec<ServerDirective>
where
    P: ServerProtocol,
    E: EntitySource,
{
    let mut snapshots = entities.snapshots();
    let mut directives = Vec::new();
    if let Some(registry) = entities.players() {
        let view = registry.view(ticket.map(PlayerTicket::entity_id));
        directives.extend(player_list.sync(proto, &view.roster));
        snapshots.extend(view.entities);
    }
    directives.extend(streamer.sync(proto, &snapshots));
    directives
}

/// An [`EntitySource`] carrying no entities — for callers that only stream
/// terrain (the existing chunk-only behaviour).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEntities;

impl EntitySource for NoEntities {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        Vec::new()
    }
}

/// Per-connection bookkeeping that turns "the entities right now" into the
/// spawn / update / remove directives this client still needs, given what it was
/// already sent.
///
/// This is the diff the integrated server owns (plan: the server drives the
/// lifecycle; the [`ServerProtocol`] only encodes individual packets). It holds
/// the last snapshot sent per entity id so it can decide, each pass, which ids
/// are new (spawn), which changed (update, handing the protocol the previous
/// snapshot so it may choose a relative encoding), and which vanished (remove,
/// batched into one packet as `REMOVE_ENTITIES` is on the wire).
#[derive(Debug, Default)]
struct EntityStreamer {
    last_sent: HashMap<i32, EntitySnapshot>,
}

impl EntityStreamer {
    /// Produces the directives that bring the client from its last-sent state to
    /// `current`, updating the bookkeeping to match.
    fn sync<P: ServerProtocol>(
        &mut self,
        proto: &P,
        current: &[EntitySnapshot],
    ) -> Vec<ServerDirective> {
        let mut directives = Vec::new();

        // Removals first, batched: any id we sent that is no longer present.
        let live: HashSet<i32> = current.iter().map(|e| e.id).collect();
        let removed: Vec<i32> = self
            .last_sent
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        if !removed.is_empty() {
            for id in &removed {
                self.last_sent.remove(id);
            }
            directives.push(proto.encode_remove_entity(&removed));
        }

        // Spawns and updates, in the source's iteration order.
        for entity in current {
            match self.last_sent.get(&entity.id) {
                None => {
                    directives.push(proto.encode_add_entity(entity));
                    // Issue #425: vanilla sends an entity's initial
                    // non-default metadata as a `SET_ENTITY_DATA` right
                    // after its `ADD_ENTITY` (`ServerEntity`'s own pairing
                    // sync) — `ADD_ENTITY` itself carries no metadata on the
                    // wire. `encode_add_entity` returns exactly one
                    // `ServerDirective`, so this is a second directive
                    // rather than folding into that call.
                    if !entity.metadata.is_empty() {
                        directives.push(proto.encode_set_entity_data(entity.id, &entity.metadata));
                    }
                    self.last_sent.insert(entity.id, entity.clone());
                }
                Some(prev) if prev != entity => {
                    directives.extend(proto.encode_entity_update(Some(prev), entity));
                    // Issue #425: a metadata-only change (e.g. a creeper's
                    // `swell_dir` climbing while it stands still) still
                    // takes this branch — `EntitySnapshot`'s `PartialEq`
                    // covers `metadata` too — so this check is independent
                    // of whether position/rotation also changed this tick.
                    if prev.metadata != entity.metadata {
                        directives.push(proto.encode_set_entity_data(entity.id, &entity.metadata));
                    }
                    self.last_sent.insert(entity.id, entity.clone());
                }
                Some(_) => {}
            }
        }

        directives
    }
}

/// How a connection reaches its terrain, and the whole of issue #293's
/// blocking-vs-offloaded fork in one place.
///
/// Chunk generation is CPU-bound and synchronous, so it has to be moved off
/// the async runtime's core thread — see
/// [`generate_columns_offloaded`](crate::chunk::generate_columns_offloaded)
/// for the measurement and for why `spawn_blocking` rather than
/// `block_in_place`. `spawn_blocking` needs a `'static` closure, which a
/// `&S` cannot provide. That normally forces `serve_connection`'s `source`
/// parameter from `&S` to `Arc<S>` — and *that* would break every
/// `crates/protocol/v770/tests/*` call site, which are off-limits (the same
/// constraint that already produced three differently-named
/// `serve_connection*` wrappers in this file rather than one changed
/// signature).
///
/// This enum is how both shapes share one body instead:
///
/// | arm | generation | who uses it |
/// |---|---|---|
/// | [`Shared`](Self::Shared) | offloaded, never blocks the runtime | every production caller in [`crate::integrated`] |
/// | [`Borrowed`](Self::Borrowed) | blocking, today's behaviour | `&S`-shaped test call sites |
///
/// The `Borrowed` arm is deliberately kept rather than deleted: it is the
/// **permanent negative control** for #293's gate. A test can drive the exact
/// same `serve_connection` body down the blocking path and watch the world
/// tick stall, which is what proves the `Shared` arm's non-stall assertion is
/// measuring something. A control that only exists as a temporary neuter
/// cannot be re-run later.
///
/// `Copy` (hand-written, because `#[derive(Copy)]` would demand `S: Copy`)
/// so it threads through the dispatch chain exactly as cheaply as the `&S`
/// it replaces.
#[derive(Debug)]
pub(crate) enum SourceRef<'a, S> {
    /// A plain borrow. Generation blocks the calling thread.
    Borrowed(&'a S),
    /// A shared handle. Generation is offloaded to the blocking pool.
    Shared(&'a Arc<S>),
}

impl<S> Clone for SourceRef<'_, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S> Copy for SourceRef<'_, S> {}

impl<'a, S: ChunkSource + 'static> SourceRef<'a, S> {
    /// The underlying source, for the read/write paths that never generate a
    /// whole batch (`block_state`, `set_block`) and so have nothing to
    /// offload.
    fn get(self) -> &'a S {
        match self {
            Self::Borrowed(source) => source,
            Self::Shared(source) => &**source,
        }
    }

    /// Generates every column in `coords`, in `coords` order — off the core
    /// thread on the [`Shared`](Self::Shared) arm, on it for
    /// [`Borrowed`](Self::Borrowed).
    ///
    /// The ordering guarantee is the same one
    /// [`generate_columns_parallel`] documents, and it is load-bearing for
    /// the wire: both arms hand back a `Vec` aligned index-for-index with
    /// `coords`, so which arm a caller is on cannot change the emitted byte
    /// sequence.
    async fn generate(self, coords: Vec<(i32, i32)>) -> Vec<ChunkColumn> {
        match self {
            Self::Shared(source) => generate_columns_offloaded(Arc::clone(source), coords).await,
            Self::Borrowed(source) => generate_columns_parallel(source, &coords),
        }
    }
}

/// Per-connection view-streaming bookkeeping: which chunk columns has this
/// connection been sent, and around which chunk column.
///
/// Mirrors vanilla's `ChunkMap`/`ChunkTrackingView`
/// (`ChunkMap.java:1110-1132`'s `updateChunkTracking`/`applyChunkTrackingView`,
/// `ChunkTrackingView.java`'s `difference`), simplified to the same square
/// window `serve_connection`'s own initial view already uses
/// (`[-view_radius, view_radius]²`) rather than vanilla's rounded
/// `ChunkTrackingView.Positioned::contains` (a buffered Euclidean-distance
/// test). Keeping the join-time and move-time shapes identical is what stops
/// a live connection from immediately forgetting chunks it only just
/// finished sending at join; matching vanilla's exact circular shape is not
/// otherwise load-bearing for "the world keeps up as the player walks".
#[derive(Debug)]
struct ViewTracker {
    center: (i32, i32),
    loaded: HashSet<(i32, i32)>,
    /// The connection's *current* effective view radius — starts at the
    /// server's configured cap (`serve_connection`'s own `view_radius`
    /// parameter) and can shrink or grow within that cap via
    /// [`set_view_radius`](Self::set_view_radius) (issue #270's
    /// `ServerBound::ClientInformationChanged`). Stored on `self` rather than
    /// re-passed at every [`recenter`](Self::recenter) call so a client's
    /// requested distance actually sticks across subsequent moves, instead
    /// of being silently overwritten by the original cap on the next
    /// `PlayerMoved`.
    radius: i32,
}

/// The directives produced by one [`ViewTracker`] update, split by whether
/// they are subject to issue #270's chunk-batch flow-control gate
/// (`ServerBound::ChunkBatchAcknowledged`) — see
/// [`send_view_update`]'s own doc comment for how a caller applies this.
#[derive(Debug, Default)]
struct ViewUpdate {
    /// Cache-center update and forgets: sent right away regardless of any
    /// outstanding chunk-batch acknowledgement, matching vanilla's own
    /// `ChunkTrackingView::difference`, which is not gated by
    /// `PlayerChunkSender` at all (only new chunk *sends* are).
    immediate: Vec<ServerDirective>,
    /// The `begin_chunk_batch`/`encode_chunk`*/`end_chunk_batch` sequence for
    /// any newly-visible columns, if any — empty when nothing new entered the
    /// view. Subject to the one-unacknowledged-batch gate.
    batch: Vec<ServerDirective>,
}

impl ViewTracker {
    /// Seeds the tracker with the square already sent for the initial join
    /// view (`center`, `[-view_radius, view_radius]²` around it), so the
    /// first [`recenter`](Self::recenter) diffs against what the client
    /// actually has rather than an empty set.
    fn new(center: (i32, i32), view_radius: i32) -> Self {
        let mut loaded = HashSet::new();
        for dz in -view_radius..=view_radius {
            for dx in -view_radius..=view_radius {
                loaded.insert((center.0 + dx, center.1 + dz));
            }
        }
        Self {
            center,
            loaded,
            radius: view_radius,
        }
    }

    /// The square `[-self.radius, self.radius]²` window around `center`.
    fn window(center: (i32, i32), radius: i32) -> HashSet<(i32, i32)> {
        let mut next = HashSet::new();
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                next.insert((center.0 + dx, center.1 + dz));
            }
        }
        next
    }

    /// The `begin_chunk_batch`/`encode_chunk`*/`end_chunk_batch` sequence for
    /// every column in `next` this tracker has not already sent — empty if
    /// there is nothing new. Shared by [`recenter`](Self::recenter) and
    /// [`set_view_radius`](Self::set_view_radius) so both diff against
    /// `self.loaded` identically.
    async fn build_batch<P, S>(
        &self,
        proto: &P,
        source: SourceRef<'_, S>,
        next: &HashSet<(i32, i32)>,
    ) -> Vec<ServerDirective>
    where
        P: ServerProtocol,
        S: ChunkSource + 'static,
    {
        // Sorted rather than left in `HashSet::difference`'s hash-iteration
        // order: that order already varies run-to-run (`RandomState` reseeds
        // per process), and generating in parallel below means the set of
        // columns can finish in yet another, scheduling-dependent order.
        // Fixing the wire order here is what makes the encoded byte sequence
        // independent of both.
        let mut added: Vec<(i32, i32)> = next.difference(&self.loaded).copied().collect();
        added.sort_unstable();
        if added.is_empty() {
            return Vec::new();
        }
        let mut batch = vec![proto.begin_chunk_batch()];
        let columns = source.generate(added.clone()).await;
        for (&(x, z), column) in added.iter().zip(columns.iter()) {
            batch.push(proto.encode_chunk(x, z, column));
        }
        batch.push(proto.end_chunk_batch(added.len() as i32));
        batch
    }

    /// Recomputes the view for a new player chunk position `(cx, cz)` at the
    /// tracker's current [`radius`](Self::radius), returning the directives
    /// that bring the client's tracked chunks back in sync — and returning
    /// nothing at all if `(cx, cz)` is still the tracked center (the same
    /// "did the 2D chunk position actually change" guard
    /// `ChunkMap::updateChunkTracking` applies before touching the view at
    /// all).
    ///
    /// Order mirrors vanilla's `applyChunkTrackingView`
    /// (`ChunkMap.java:1122-1132`): the cache-center update is sent first
    /// (unconditionally, since by this point the center *did* change —
    /// vanilla additionally guards this send on the center changing, which
    /// is already implied here), then every column that left the window is
    /// forgotten, then every column that entered it is sent as one chunk
    /// batch.
    async fn recenter<P, S>(
        &mut self,
        proto: &P,
        source: SourceRef<'_, S>,
        cx: i32,
        cz: i32,
    ) -> ViewUpdate
    where
        P: ServerProtocol,
        S: ChunkSource + 'static,
    {
        if (cx, cz) == self.center {
            return ViewUpdate::default();
        }

        let next = Self::window((cx, cz), self.radius);

        let mut immediate = vec![proto.encode_chunk_cache_center(cx, cz)];
        for &(x, z) in self.loaded.difference(&next) {
            immediate.push(proto.encode_forget_chunk(x, z));
        }
        let batch = self.build_batch(proto, source, &next).await;

        self.center = (cx, cz);
        self.loaded = next;
        ViewUpdate { immediate, batch }
    }

    /// Resizes the tracked view around the *current* center without the
    /// player having moved at all (issue #270's
    /// `ServerBound::ClientInformationChanged` — a client changing its
    /// render-distance setting mid-session). Unlike
    /// [`recenter`](Self::recenter), there is no cache-center update to send
    /// (the center did not change) and no early-return guard on position —
    /// the guard here is `radius` itself already matching, so a settings
    /// packet that does not actually change the distance is correctly a
    /// no-op.
    async fn set_view_radius<P, S>(
        &mut self,
        proto: &P,
        source: SourceRef<'_, S>,
        radius: i32,
    ) -> ViewUpdate
    where
        P: ServerProtocol,
        S: ChunkSource + 'static,
    {
        if radius == self.radius {
            return ViewUpdate::default();
        }

        let next = Self::window(self.center, radius);
        let mut immediate = Vec::new();
        for &(x, z) in self.loaded.difference(&next) {
            immediate.push(proto.encode_forget_chunk(x, z));
        }
        let batch = self.build_batch(proto, source, &next).await;

        self.radius = radius;
        self.loaded = next;
        ViewUpdate { immediate, batch }
    }
}

/// The join view, split into **Chebyshev rings** ordered outward from the
/// player's own column — issue #453.
///
/// Ring `r` is every column at Chebyshev (chess-king) distance exactly `r` from
/// the centre, so ring 0 is the single column the player is standing in, ring 1
/// is the 8 around it, and ring `r > 0` holds `8r` columns. Flattened, the
/// result is the whole `[-view_radius, view_radius]²` square with **no column
/// repeated and none missing** — the same set `ViewTracker::new` seeds itself
/// with, in a different order.
///
/// # Why rings, and why this is not a re-sort
///
/// Before this, the join enumerated raster-order from `(-view_radius,
/// -view_radius)`, generated **all** 361 columns, and only then encoded any. Two
/// separate consequences, and the fix needs both halves:
///
/// * the player's own column was item **~180 of 361** on the wire, so terrain
///   materialised from the far corner inward;
/// * nothing at all was encoded until the last column finished generating.
///
/// Returning *groups* rather than one flat `Vec` is what fixes the second half:
/// the caller generates and encodes one ring at a time, so the first chunk
/// reaches the client after **one** column of generation instead of 361. That is
/// also why this deliberately does not touch `ViewTracker::build_batch`, whose
/// lexicographic `sort_unstable` exists for byte-reproducibility — proximity
/// belongs at the enumeration/dispatch layer, and the batch's internal
/// determinism is left exactly as it was.
///
/// Vanilla spirals outward for the same reason, and its priority *is* the ticket
/// level (`ChunkTaskDispatcher.java:62-69`), so there is no separate heuristic
/// invented here. This is a slice of issue #289's U4/U5 rather than new design.
///
/// # Determinism
///
/// Order **within** a ring is the same `dz`-outer/`dx`-inner walk the whole
/// square used to use, filtered to the ring. So the emitted byte sequence stays
/// a pure function of `view_radius` — independent of thread scheduling, hash
/// seeds, and which arm of [`SourceRef`] generated it — exactly as before.
///
/// # Cost
///
/// The inner rings are smaller than `available_parallelism`, so rings 0 and 1
/// leave most worker threads idle where one 361-column batch would not. That is
/// a deliberate trade: it costs a fraction of a second of total generation to
/// buy time-to-first-chunk falling from *the whole view* to *one column*, which
/// is the entire reported symptom. Rings 2 and up saturate the fan-out.
fn join_view_rings(view_radius: i32) -> Vec<Vec<(i32, i32)>> {
    // A negative radius yields **no rings**, not ring 0. `view_radius.max(0)`
    // reads as the harmless guard and is not: the raster walk this replaced built
    // `(-r..=r)`, which is an *empty* range for `r < 0`, so a negative radius
    // sent zero chunks. Clamping to 0 would send one — and `ViewTracker::new`
    // would still record an empty loaded set for the same input, so the tracker
    // and the wire would disagree about a column the client actually has.
    // Nothing produces a negative radius today (`dispatch_play_packet` clamps
    // with `view_radius.max(0)` precisely as an invariant against it), which is
    // exactly why the divergence would have gone unnoticed.
    if view_radius < 0 {
        return Vec::new();
    }
    (0..=view_radius)
        .map(|r| {
            let mut ring = Vec::new();
            for dz in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dz.abs()) == r {
                        ring.push((dx, dz));
                    }
                }
            }
            ring
        })
        .collect()
}

/// Applies one [`ViewUpdate`]: the non-batch directives immediately, and the
/// chunk-batch portion (if any) either right away or queued behind
/// `awaiting_chunk_batch_ack` — the flow-control gate issue #270's
/// `ServerBound::ChunkBatchAcknowledged` closes. Vanilla's `PlayerChunkSender`
/// keeps at most one batch in flight; before this, `crate::server` started a
/// fresh batch on every `recenter` regardless of whether the client had
/// acknowledged the last one at all (the issue's own "never reads this reply"
/// gap). Shared by both [`ViewTracker::recenter`] and
/// [`ViewTracker::set_view_radius`] call sites in [`dispatch_play_packet`] so
/// the two update paths cannot drift into different flow-control behaviour.
async fn send_view_update<T: Transport>(
    conn: &mut Connection<T>,
    state: &mut State,
    update: ViewUpdate,
    awaiting_chunk_batch_ack: &mut bool,
    pending_chunk_batches: &mut VecDeque<Vec<ServerDirective>>,
) -> Result<(), ServerError> {
    for directive in update.immediate {
        apply(conn, state, directive).await?;
    }
    if update.batch.is_empty() {
        return Ok(());
    }
    if *awaiting_chunk_batch_ack {
        tracing::info!(
            "server: chunk batch queued ({} directives), awaiting ack — {} pending total",
            update.batch.len(), pending_chunk_batches.len() + 1,
        );
        pending_chunk_batches.push_back(update.batch);
        return Ok(());
    }
    tracing::info!("server: sending chunk batch ({} directives)", update.batch.len());
    *awaiting_chunk_batch_ack = true;
    for directive in update.batch {
        apply(conn, state, directive).await?;
    }
    Ok(())
}

/// Outcome of serving a connection's initial view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeSummary {
    /// The username the client logged in as.
    pub username: String,
    /// Number of chunk columns sent for the initial view.
    pub chunks_sent: usize,
    /// The connection's final server-authoritative inventory state — empty
    /// (`PlayerInventory::default()`) if the client disconnected before ever
    /// reaching [`State::Play`], since [`PlayerInventory`] is only
    /// constructed once `serve_play` starts. Exposed here (rather than only
    /// internally) so a test can drive a real client through
    /// `SET_CARRIED_ITEM`/`CONTAINER_CLICK` and observe the resulting model
    /// state once the connection closes, without threading a new parameter
    /// through [`IntegratedServer`](crate::IntegratedServer)'s public
    /// constructors.
    pub inventory: PlayerInventory,
}

/// Errors from the integrated-server driver.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The underlying transport/codec failed.
    #[error("network error: {0}")]
    Net(#[from] NetError),
    /// The client disconnected before completing login.
    #[error("client closed before login completed")]
    ClosedBeforeLogin,
    /// The client did not echo the server's keep-alive challenge before the
    /// next one was due (a fixed 15-second interval, matching vanilla's
    /// `TIMEOUT_DISCONNECTION_MESSAGE` disconnect path —
    /// `ServerCommonPacketListenerImpl.java:121-129`). Native-only in
    /// practice: nothing constructs this on `wasm32`, since that build never
    /// starts the keep-alive timer in the first place (see
    /// `serve_play`'s doc comment).
    #[error("keep-alive timeout: client did not echo the server's challenge in time")]
    KeepAliveTimeout,
    /// The connection completed a server-list status exchange and was
    /// terminated (issue #277). **Not a failure**: vanilla itself ends a status
    /// connection exactly this way, and calls it a disconnect —
    /// `ServerStatusPacketListenerImpl` closes the channel with reason
    /// `multiplayer.status.request_handled` after answering a ping, and also
    /// after a *second* status request on one connection
    /// (`net/minecraft/server/network/ServerStatusPacketListenerImpl.java:14,
    /// 34-47`).
    ///
    /// It is an `Err` rather than an `Ok` only because [`ServeSummary`] is
    /// shaped around a session that logged in: a status connection has no
    /// username, no chunks, and no inventory, so there is nothing truthful to
    /// put in one. Callers discard the result either way (see
    /// [`crate::IntegratedServer`]'s accept loops).
    #[error("server-list status request handled; connection closed (not an error)")]
    StatusRequestHandled,
    /// The client presented a username vanilla's own server would refuse
    /// (`StringUtil.isValidPlayerName` — see [`is_valid_player_name`]), and was
    /// sent a login-phase disconnect explaining so (issue #279).
    #[error("login rejected: invalid username")]
    InvalidUsername,
}

async fn apply<T: Transport>(
    conn: &mut Connection<T>,
    state: &mut State,
    directive: ServerDirective,
) -> Result<(), ServerError> {
    match directive {
        ServerDirective::Send { packet_id, payload } => {
            conn.write_packet(packet_id, &payload).await?;
        }
        ServerDirective::SetState(next) => *state = next,
        ServerDirective::SetCompression(threshold) => conn.set_compression(threshold),
        ServerDirective::None => {}
    }
    Ok(())
}

/// A shared feed of server-initiated resource pack pushes (issue #334) — the
/// exact idiom [`BlockTickFeed`]/[`ExplosionFeed`]/[`WeatherFeed`] establish
/// for block changes, detonations and weather transitions, applied to a
/// resource pack push instead. A host publishes one [`ResourcePackPush`] per
/// push; `serve_play`'s `container_sync_tick` arm drains it into a real
/// clientbound `resource_pack_push` frame, on the same timer the three feeds
/// above ride.
///
/// Same single-consumer caveat as all three, and the same resolution:
/// singleplayer (`crate::IntegratedServer::open_in_memory_with_mobs`) spawns
/// exactly one connection task per feed instance. A push is broadcast-shaped
/// in vanilla (every connection must receive it), so this is the documented
/// limitation the other single-consumer feeds share, not a new one.
#[derive(Debug, Clone, Default)]
pub struct ResourcePackPushFeed(Arc<Mutex<Vec<ResourcePackPush>>>);

impl ResourcePackPushFeed {
    /// Records one push for every consumer to learn about on their next
    /// [`drain_all`](Self::drain_all).
    pub fn publish(&self, push: ResourcePackPush) {
        self.0
            .lock()
            .expect("resource pack feed lock poisoned")
            .push(push);
    }

    /// Drains and returns every push published since the last call — see the
    /// struct doc comment for why this is safe only for exactly one consumer.
    pub fn drain_all(&self) -> Vec<ResourcePackPush> {
        std::mem::take(&mut *self.0.lock().expect("resource pack feed lock poisoned"))
    }
}

/// Serves one client connection through login, configuration, the play join
/// sequence, and the initial chunk view — then keeps serving until the client
/// disconnects.
///
/// The loop transitions Handshaking → Login → Configuration → Play driven
/// entirely by the [`ServerProtocol`], acknowledgement by acknowledgement,
/// exactly mirroring the client-side `VersionAdapter`'s choreography:
///
/// 1. [`ServerBound::LoginStart`] → [`ServerProtocol::login_success`] (no
///    state change yet).
/// 2. [`ServerBound::LoginAcknowledged`] → state becomes
///    [`State::Configuration`], then [`ServerProtocol::encode_registry_data`]
///    (issue #275: the registries must precede the finish signal), then
///    [`ServerProtocol::begin_configuration`].
/// 3. [`ServerBound::ConfigurationFinished`] → state becomes [`State::Play`],
///    then [`ServerProtocol::begin_play`], then every column in
///    `[-view_radius, view_radius]²` (chunk coordinates) from `source` as a
///    single flow-controlled chunk batch
///    ([`ServerProtocol::begin_chunk_batch`]/
///    [`ServerProtocol::encode_chunk`]/[`ServerProtocol::end_chunk_batch`]),
///    then [`ServerProtocol::welcome_message`] (optional; empty by default).
///
/// Unlike the initial version of this loop, it does not return once the view
/// has been delivered: a real client stays connected past the join sequence
/// (keep-alives, movement, chunk-batch acknowledgements), so the loop keeps
/// reading and lifting packets — dispatching to [`ServerBound::Ignored`] for
/// anything not yet acted on — until the client closes the connection. The
/// summary is only available once that happens.
///
/// Once the connection reaches [`State::Play`], every inbound packet also drives
/// an entity streaming pass: the [`EntityStreamer`] diffs `entities.snapshots()`
/// against what this connection was last sent and emits the necessary spawn /
/// update / remove directives. The client's own traffic (keep-alives, movement)
/// provides the cadence for this MVP; a fixed server-side tick is a later
/// refinement that only changes *when* `sync` is called, not the diff it
/// computes. Pass [`NoEntities`] to keep the chunk-only behaviour.
///
/// [`State::Play`] itself is served by [`serve_play`], which adds the parts
/// that have no place before a client has a world to live in: a
/// server-initiated keep-alive (with vanilla's disconnect-on-timeout),
/// periodic time-of-day, and view streaming as the player's chunk column
/// changes. See that function's doc comment for the scheduling.
///
/// # Errors
///
/// Returns [`ServerError::Net`] on a transport/codec failure,
/// [`ServerError::ClosedBeforeLogin`] if the client hangs up before it ever
/// reaches [`ServerBound::LoginStart`], or whatever [`serve_play`] returns
/// once [`State::Play`] is reached.
///
/// Forwards to [`serve_connection_with_block_ticks`] with a fresh,
/// permanently-empty [`BlockTickFeed`] — this is the compatibility shape
/// kept for every caller outside this crate (`crates/protocol/v770/tests/*`
/// call this directly and are off-limits for this issue's file-ownership
/// split — see issues #307/#308's own task brief), none of which need to
/// observe a world-tick-driven block change. A caller that does (today:
/// only [`crate::IntegratedServer::open_in_memory_with_mobs`]) calls
/// [`serve_connection_with_block_ticks`] instead.
#[allow(clippy::too_many_arguments)]
pub async fn serve_connection<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    entities: &E,
    view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    serve_connection_with_block_ticks(
        conn,
        proto,
        source,
        entities,
        view_radius,
        block_entities,
        mobs,
        &BlockTickFeed::default(),
    )
    .await
}

/// [`serve_connection`], but generating chunks **off** the async runtime's
/// core thread (issue #293).
///
/// Identical behaviour and identical wire bytes; the only difference is that
/// the `Arc<S>` this takes can be moved into a `spawn_blocking` closure, so a
/// join burst or a view recentre no longer stalls every other task in the
/// process — including [`crate::tick::run_tick_loop`], which on the shell's
/// current-thread runtime shares the connection task's one thread. See
/// [`SourceRef`] for why two entry points exist rather than one changed
/// signature: `&S` cannot satisfy `spawn_blocking`'s `'static` bound, and
/// changing [`serve_connection`]'s own signature would break every
/// off-limits `crates/protocol/v770/tests/*` call site.
///
/// `pub(crate)`, not `pub`: `mod server` is private, so this crate's public
/// surface is whatever `lib.rs` re-exports, and this deliberately is not
/// re-exported. Nothing outside the crate needs it — the shell reaches the
/// server through [`crate::IntegratedServer`] — so #293 costs **no public API
/// change at all**, which is why it needed no `lib.rs` patch.
///
/// # Errors
///
/// As [`serve_connection`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve_connection_shared<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &Arc<S>,
    entities: &E,
    view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Shared(source),
        entities,
        view_radius,
        block_entities,
        mobs,
        &BlockTickFeed::default(),
        &ExplosionFeed::default(),
        &WeatherFeed::default(),
        // Issue #325: a fresh vote/feed no tick loop reads — see
        // `serve_connection_inner`'s parameter comments.
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
    )
    .await
}

/// [`serve_connection_with_mob_events`], but generating chunks off the core
/// thread (issue #293) — the [`serve_connection_shared`] treatment applied to
/// the feed-carrying entry point, which is what
/// [`crate::IntegratedServer::open_in_memory_with_mobs`] (singleplayer) uses.
///
/// # Errors
///
/// As [`serve_connection`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve_connection_with_mob_events_shared<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &Arc<S>,
    entities: &E,
    view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    // Issue #325: the singleplayer night-skip vote and its feed — the same
    // inner handles `crate::integrated`'s tick loop reads. This is the
    // **only** feed-carrying entry point that threads a real one; every other
    // `serve_connection_inner` caller passes a fresh default no loop reads.
    // See `serve_connection_inner`'s own parameter comments.
    sleep_vote: &SleepVote,
    sleep_feed: &SleepFeed,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Shared(source),
        entities,
        view_radius,
        block_entities,
        mobs,
        block_ticks,
        explosions,
        &WeatherFeed::default(),
        sleep_vote,
        sleep_feed,
        &CommandDispatch::none(),
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
    )
    .await
}

/// [`serve_connection_with_mob_events_shared`], plus a host-installed command
/// dispatcher (issues #48, #464).
///
/// The singleplayer-shaped counterpart to
/// [`serve_connection_with_commands`]: `_shared` is the off-core-thread chunk
/// path (issue #293) that [`crate::IntegratedServer::open_in_memory_with_mobs`]
/// uses, and that constructor is the **only** production route a real player
/// reaches this crate through. So this is the entry point singleplayer commands
/// have to come in on; the borrowed-source
/// [`serve_connection_with_commands`] cannot serve it.
///
/// Added *beside* `serve_connection_with_mob_events_shared` rather than by
/// giving that function a tenth parameter, deliberately: its one caller lives
/// in `integrated.rs`, a file this issue's ownership split does not cover, and
/// a changed signature would break it from the outside. This way the wiring
/// there is a purely additive constructor whenever its owner lands it.
///
/// # Errors
///
/// As [`serve_connection`].
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve_connection_with_mob_events_and_commands_shared<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &Arc<S>,
    entities: &E,
    view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    commands: &CommandDispatch,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Shared(source),
        entities,
        view_radius,
        block_entities,
        mobs,
        block_ticks,
        explosions,
        &WeatherFeed::default(),
        // Issue #325: a fresh vote/feed no tick loop reads (see
        // `serve_connection_inner`'s parameter comments).
        &SleepVote::default(),
        &SleepFeed::default(),
        commands,
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
    )
    .await
}

/// Like [`serve_connection`], but also forwards every change published on
/// `block_ticks` (issues #307/#308: the world tick loop's random ticks) to
/// this connection, through the same `container_sync_tick` timer arm inside
/// [`serve_play`] that already forwards block-entity registry changes with
/// no packet driving them — see that arm's own doc comment.
///
/// Forwards to [`serve_connection_inner`] with a fresh, permanently-empty
/// [`ExplosionFeed`] — same compatibility shape as [`serve_connection`]'s own
/// forward, and for the same reason: `crates/protocol/v770/tests/*` call
/// this directly and are off-limits for this issue's (#425) file-ownership
/// split. [`crate::IntegratedServer::open_in_memory_with_mobs`] calls
/// [`serve_connection_with_mob_events`] instead, which does observe
/// detonations.
#[allow(clippy::too_many_arguments)]
pub async fn serve_connection_with_block_ticks<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    entities: &E,
    view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        block_entities,
        mobs,
        block_ticks,
        &ExplosionFeed::default(),
        &WeatherFeed::default(),
        // Issue #325: a fresh vote/feed no tick loop reads.
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
    )
    .await
}

/// Like [`serve_connection_with_block_ticks`], but also forwards every
/// detonation published on `explosions` (issue #425: `MobSim::tick` calling
/// `MobSim::explode` the tick a creeper's fuse completes) to this
/// connection, as a real `EXPLODE` packet — through the same
/// `container_sync_tick` timer arm that already forwards `block_ticks`'
/// changes. The one caller today is
/// [`crate::IntegratedServer::open_in_memory_with_mobs`], the only
/// constructor that spawns a [`MobSim`]-driven tick loop capable of
/// producing a detonation in the first place.
///
/// # Currently unused, and deliberately kept
///
/// Issue #293 moved both production callers (`crate::integrated`) to
/// [`serve_connection_with_mob_events_shared`], so nothing calls this today —
/// and because `mod server` is private and `lib.rs` does not re-export it,
/// nothing outside the crate can. It is retained rather than deleted because it
/// is the borrow-shaped twin of the `_shared` entry point: the one way to drive
/// the *feed-carrying* path down [`SourceRef::Borrowed`], i.e. #293's negative
/// control with block ticks and explosions attached. Delete it only together
/// with that control.
///
/// #324: also the only entry point that carries a real [`WeatherFeed`] today —
/// the borrow-shaped twin of whatever feed the `_shared` variant will gain when
/// `crate::integrated` wires it. `tests/serve_play.rs`'s weather gate drives
/// this one precisely because it is borrow-shaped.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub async fn serve_connection_with_mob_events<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    entities: &E,
    view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    weather: &WeatherFeed,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        block_entities,
        mobs,
        block_ticks,
        explosions,
        weather,
        // Issue #325: the borrow-shaped twin stays sleep-free — no caller of
        // this dead-code entry point wires a vote, and the twin of the *feed*
        // wiring lives in `serve_connection_with_mob_events_shared`.
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
    )
    .await
}

/// [`serve_connection`], plus a host-installed command dispatcher (issues #48,
/// #464).
///
/// This is the **only** entry point that can make a `/command` from a real
/// player do anything. Every other one above passes
/// [`CommandDispatch::none()`], under which a `chat_command` frame decodes,
/// reaches this crate, and is answered with
/// [`UNKNOWN_COMMAND`](crate::UNKNOWN_COMMAND) — the fail-closed direction.
///
/// A new entry point rather than a changed signature, for the same reason
/// [`serve_connection_with_block_ticks`] and
/// [`serve_connection_with_mob_events`] are: `crates/protocol/v770/tests/*`
/// call the older ones directly and are off-limits under this issue's
/// file-ownership split, and every added parameter here would break all of
/// them.
///
/// # Errors
///
/// As [`serve_connection`].
#[allow(clippy::too_many_arguments)]
pub async fn serve_connection_with_commands<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    entities: &E,
    view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    commands: &CommandDispatch,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        block_entities,
        mobs,
        block_ticks,
        explosions,
        &WeatherFeed::default(),
        // Issue #325: a fresh vote/feed no tick loop reads.
        &SleepVote::default(),
        &SleepFeed::default(),
        commands,
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
    )
    .await
}

/// [`serve_connection`], plus a host-observable [`ResourcePackPushFeed`]
/// (issue #334) — the entry point that makes a server-initiated resource pack
/// push reach a player at all.
///
/// The compatibility shape this file established for the feed-carrying
/// variants holds here too: `crates/protocol/v770/tests/*` call
/// [`serve_connection`] and [`serve_connection_with_commands`] directly and
/// are off-limits, so a real feed gets a *new* entry point rather than a
/// changed signature on those two. A host that wants to push constructs a
/// [`ResourcePackPushFeed`], passes it here, and publishes [`ResourcePackPush`]es
/// into it; `serve_play`'s `container_sync_tick` arm drains them into real
/// clientbound `resource_pack_push` frames (see that arm's own comment). A
/// future `IntegratedServer` config surface (`open_in_memory_with_mobs` et al.)
/// is a purely additive constructor calling this (or the `_shared` twin of it)
/// with a feed the config parsed.
///
/// # Errors
///
/// As [`serve_connection`].
#[allow(clippy::too_many_arguments)]
pub async fn serve_connection_with_resource_pack<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    entities: &E,
    view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    resource_packs: &ResourcePackPushFeed,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        block_entities,
        mobs,
        block_ticks,
        explosions,
        &WeatherFeed::default(),
        // Issue #325: a fresh vote/feed no tick loop reads.
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        resource_packs,
        &PluginChannelRegistry::default(),
    )
    .await
}

/// [`serve_connection`], plus a live [`PluginChannelRegistry`] (issue #335) —
/// the entry point that makes wire-level plugin messaging reach a player at all.
///
/// The compatibility shape this file established for the feed-carrying variants
/// holds here too: `crates/protocol/v770/tests/*` call [`serve_connection`] and
/// [`serve_connection_with_commands`] directly and are off-limits, so a live
/// registry gets a *new* entry point rather than a changed signature on those
/// two. A host that wants plugin messaging constructs a [`PluginChannelRegistry`],
/// registers its [`PluginChannelHandler`]s on it, and passes it here; inbound
/// `custom_payload` packets on a registered channel are dispatched to the
/// handler, and [`PluginChannelRegistry::broadcast`] payloads are drained into
/// real clientbound `custom_payload` frames by `serve_play`'s
/// `container_sync_tick` arm, filtered to the channels each client announced
/// (see that arm's own comment). The plugin-facing API this will eventually sit
/// under is issue #77; this entry point is the wire-level seam.
///
/// # Errors
///
/// As [`serve_connection`].
#[allow(clippy::too_many_arguments)]
pub async fn serve_connection_with_plugin_channels<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    entities: &E,
    view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    plugin_channels: &PluginChannelRegistry,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        block_entities,
        mobs,
        block_ticks,
        explosions,
        &WeatherFeed::default(),
        // Issue #325: a fresh vote/feed no tick loop reads.
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        plugin_channels,
    )
    .await
}

/// The real body shared by [`serve_connection`], [`serve_connection_with_block_ticks`]
/// and [`serve_connection_with_mob_events`] — see those three thin wrappers'
/// own doc comments for why a fourth, differently-named function exists
/// instead of adding `explosions` directly to
/// [`serve_connection_with_block_ticks`]'s own signature (it would break
/// every off-limits `crates/protocol/v770/tests/*` call site).
///
/// Now also shared by [`serve_connection_shared`] and
/// [`serve_connection_with_mob_events_shared`], which differ from the three
/// above only in the [`SourceRef`] arm they pass — that is the whole reason
/// issue #293's fix needed no second copy of this body.
#[allow(clippy::too_many_arguments)]
async fn serve_connection_inner<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    entities: &E,
    view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    // Issue #324. Same shape as the two feeds above: the world tick loop's
    // weather transitions, drained by `serve_play`'s `container_sync_tick`
    // arm (see that arm's own comment for why a timer, not a packet, drives
    // it). Every pre-existing entry point passes a permanently-empty default
    // feed — the compatibility shape this file established for `block_ticks`
    // and `explosions`, and the reason no off-limits call site broke.
    weather: &WeatherFeed,
    // Issue #325. The night-skip vote, consulted by this connection and by
    // the world tick loop: `dispatch_play_packet` records this connection's
    // player `lay_down`/`get_up` on it (the `UseItemOn` bed arm and the
    // `PlayerCommand` arm), and `serve_play`'s `container_sync_tick` feeds it
    // the voter count. Same compatibility shape as every feed above —
    // pre-existing entry points pass a fresh vote no tick loop reads, which
    // is observably a vote that never passes; the feed-carrying
    // `serve_connection_with_mob_events_shared` (singleplayer) carries the
    // real one alongside the tick loop's, so the two share one inner handle.
    sleep_vote: &SleepVote,
    // Issue #325: where this connection learns a night skip happened. Drained
    // in `serve_play`'s `container_sync_tick` arm into a real
    // `encode_set_time` so the client's day clock jumps to the morning — see
    // that arm's own comment for why a timer, not a packet, drives it.
    sleep_feed: &SleepFeed,
    // Issues #48/#464. `CommandDispatch::none()` — the `Default` — is the
    // inert value every pre-existing entry point passes, so adding this
    // changed no caller's behaviour and no caller's wire bytes.
    commands: &CommandDispatch,
    // Issue #326 B1: the world border the connection snapshots for its join
    // `initialize_border` broadcast and reads every vitals tick for border
    // damage. Same shape as the feeds above — every pre-existing entry point
    // passes a fresh `BorderFeed::default()`, the compatibility shape this
    // file established for `block_ticks`/`explosions`/`weather`. Nothing
    // mutates a default feed today (the tick loop owns a private border — see
    // `crate::border`'s module doc, shape B), so a default feed and a
    // shared one are observably identical until a resize caller exists.
    border: &BorderFeed,
    // Issue #334. Same shape as every feed above: server-initiated resource
    // pack pushes, drained by `serve_play`'s `container_sync_tick` arm. Every
    // pre-existing entry point passes a permanently-empty default feed — the
    // compatibility shape this file established for `block_ticks`/`explosions`
    // /`weather`/`border`, and the reason no off-limits call site broke. A host
    // that wants to push reaches [`serve_connection_with_resource_pack`]
    // instead, which carries a real feed.
    resource_packs: &ResourcePackPushFeed,
    // Issue #335. Same shape as every feed above: the wire-level plugin
    // messaging registry — host-installed channel handlers plus the
    // server→client broadcast queue — drained by `serve_play`'s
    // `container_sync_tick` arm alongside the resource-pack pushes. Every
    // pre-existing entry point passes a permanently-inert default registry,
    // the compatibility shape this file established for the feeds and
    // `commands`, so no off-limits call site broke. A host that wants plugin
    // messaging reaches [`serve_connection_with_plugin_channels`] instead,
    // which carries a live registry.
    plugin_channels: &PluginChannelRegistry,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    let mut state = State::Handshaking;
    let mut username: Option<String> = None;
    // Issue #438: kept alongside `username` because the player entity's uuid
    // must be the *same* uuid `login_success` echoed back to this client — the
    // client resolves a player spawn by looking that uuid up in its own
    // `PlayerInfo` map, so a second, independently derived uuid would produce a
    // spawn every client silently discards.
    let mut login_uuid: Option<uuid::Uuid> = None;
    let mut streamer = EntityStreamer::default();
    let mut player_list = PlayerListStreamer::default();
    // Vanilla's `ServerStatusPacketListenerImpl.hasRequestedStatus`
    // (`ServerStatusPacketListenerImpl.java:17`): one status reply per
    // connection, a second request is a disconnect.
    let mut status_requested = false;
    // Issue #335. This connection's declared channel support, populated from
    // its `minecraft:register`/`minecraft:unregister` custom payloads — first
    // during Configuration (the arm below), then in Play via the same
    // `ServerBound::CustomPayload` arm in `dispatch_play_packet`. It is the
    // per-connection filter the broadcast drain in `serve_play` applies.
    let mut client_channels = ClientChannels::default();

    while let Some((packet_id, payload)) = conn.read_packet().await? {
        match proto.decode(state, packet_id, &payload) {
            ServerBound::Handshake { next_state } => {
                state = next_state;
            }
            // Issue #277. Mirrors `ServerStatusPacketListenerImpl
            // .handleStatusRequest` exactly (`:34-41`): answer the first
            // request, disconnect on any subsequent one. The repeat case is not
            // pedantry — it is what stops a peer holding a connection open
            // forever cheaply re-asking, which is the same class of leak issue
            // #280 closes for Play.
            ServerBound::StatusRequest => {
                if status_requested {
                    return Err(ServerError::StatusRequestHandled);
                }
                status_requested = true;
                // `players_online` is reported as `0` because this crate has no
                // cross-connection player registry to count: a status request
                // arrives on its *own* connection, before and independent of
                // any join, so a per-connection loop cannot see the sessions
                // other connections are serving. Everything else in the row a
                // client renders (MOTD, cap, version, protocol) is real. See
                // `docs/server-status.md` for what a truthful count needs.
                let directive = proto.encode_status_response(
                    STATUS_MOTD,
                    0,
                    STATUS_MAX_PLAYERS,
                    &[],
                    None,
                    false,
                );
                apply(conn, &mut state, directive).await?;
            }
            // `ServerStatusPacketListenerImpl.handlePingRequest` (`:44-47`):
            // echo the payload, then close. Note vanilla does *not* require a
            // preceding status request here, so neither does this.
            ServerBound::PingRequest { time } => {
                apply(conn, &mut state, proto.encode_pong_response(time)).await?;
                return Err(ServerError::StatusRequestHandled);
            }
            ServerBound::LoginStart {
                username: name,
                uuid,
            } => {
                // Issue #279's login-phase producer. Vanilla validates the name on
                // this exact packet (`ServerLoginPacketListenerImpl.java:120`) —
                // but by *throwing*, which closes the socket with no explanation.
                // We reject the same names and say why, which is the whole point
                // of this issue.
                //
                // Not merely cosmetic: an offline-mode server derives the account
                // uuid from the username and persists player data under it, so a
                // name carrying control characters is a name that reaches storage.
                if !is_valid_player_name(&name) {
                    let directive = proto.encode_disconnect(state, &invalid_username_reason());
                    apply(conn, &mut state, directive).await?;
                    return Err(ServerError::InvalidUsername);
                }
                username = Some(name.clone());
                login_uuid = Some(uuid);
                for directive in proto.login_success(&name, uuid) {
                    apply(conn, &mut state, directive).await?;
                }
            }
            ServerBound::LoginAcknowledged => {
                state = State::Configuration;
                // Issue #275: the registries a real client must resolve — the
                // `dimension_type` holder ids `login`/`respawn` carry, the
                // `world_clock` keys `set_time` uses — must arrive **before**
                // the finish signal, or the client cannot make sense of them.
                for directive in proto.encode_registry_data() {
                    apply(conn, &mut state, directive).await?;
                }
                for directive in proto.begin_configuration() {
                    apply(conn, &mut state, directive).await?;
                }
            }
            ServerBound::ConfigurationFinished => {
                // PERF INSTRUMENT: timing the whole configuration→play transition
                let t_cfg = Instant::now();
                // Issue #329: the world spawn point is a *search*, not a
                // fixed local `(8, 8)` in the origin column. Vanilla's
                // `MinecraftServer.setInitialSpawn` walks a ±5-chunk spiral
                // and picks the first chunk with a `getLevelRespawnPos`-valid
                // surface (`world_spawn::find_initial_spawn`). Issue #461 had
                // already replaced the hardcoded Y with terrain, but X/Z
                // stayed fixed, so an ocean origin chunk spawned the player
                // under water and no search ever moved them. The search keeps
                // the same vanilla `getLevelRespawnPos` rule and yields
                // `(8, y, 8)` for a plains origin — the pre-#329 result — so
                // normal terrain is unchanged; the change is that an invalid
                // origin now moves the spawn to the nearest valid chunk
                // instead of stranding the player.
                let spawn = find_initial_spawn(source.get());

                state = State::Play;
                for directive in proto.begin_play_at(view_radius, spawn.pos) {
                    apply(conn, &mut state, directive).await?;
                }

                // Full clock sync at join, mirroring vanilla's
                // `ServerClockManager::createFullSyncPacket`, sent by
                // `PlayerList.sendLevelInfo` before chunk streaming starts
                // (`PlayerList.java:648-651`): anchor a fresh session's
                // day/night clock at tick 0. This crate has no persisted
                // world age, so "tick 0" is the session's own join moment,
                // not a restored save.
                apply(conn, &mut state, proto.encode_set_time(0, Some(0))).await?;

                apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                // Generation is fanned out over scoped OS threads
                // (`generate_columns_parallel`); the wire order is a fixed
                // function of `view_radius` alone (see `join_view_rings`), which
                // is what makes the emitted byte sequence independent of thread
                // scheduling — see `generate_columns_parallel`'s doc comment for
                // why the fan-out cannot desync per-chunk RNG-derived content
                // either.
                //
                // Issue #293: on the `SourceRef::Shared` arm the whole fan-out
                // *also* runs off this runtime's core thread, so this burst —
                // the largest single generation batch a session performs — no
                // longer holds up `run_tick_loop`. See `SourceRef`.
                //
                // Issue #453: **one ring at a time, generated and encoded before
                // the next is asked for.** This loop used to build all
                // `(2r+1)²` coordinates up front, `await` a single `generate`
                // over the lot, and only then start encoding — so at
                // `view_radius = 9` nothing reached the client until all 361
                // columns existed, and raster order put the player's own column
                // at item ~180. Now ring 0 is the player's column, so the first
                // chunk is encoded after exactly one column of generation, and
                // the sequence is non-decreasing in distance from the centre
                // thereafter.
                //
                // Still **one** chunk batch, not one per ring: the batch markers
                // stay outside this loop, so the client's flow-control
                // accounting (issue #270) sees the same single
                // begin/…/end sequence it always did.
                let t_chunks = Instant::now();
                let mut batch_size = 0;
                let mut ring_idx = 0u32;
                // Issue #293: for the join burst, fan each *column* individually
                // into the blocking pool rather than one spawn_blocking per ring.
                // One spawn_blocking per ring started available_parallelism() scoped
                // threads inside it, serialising rings and competing with the mob
                // seed task for the same scoped-thread pool. Individually each
                // column can use all blocking-pool threads at once, so ring 7's 56
                // columns take max(col) not sum(col) — ~130ms instead of ~7300ms.
                let shared_source: Option<Arc<_>> = match &source {
                    SourceRef::Shared(src) => Some(Arc::clone(src)),
                    SourceRef::Borrowed(_) => None,
                };
                for ring in join_view_rings(view_radius) {
                    let t_ring = Instant::now();
                    if let Some(shared) = &shared_source {
                        // Concurrent per-column generation — every column in
                        // this ring races through the blocking pool at once.
                        let handles: Vec<_> = ring.iter().map(|&(cx, cz)| {
                            let src = Arc::clone(shared);
                            ((cx, cz), tokio::task::spawn_blocking(move || src.column(cx, cz)))
                        }).collect();
                        for ((cx, cz), handle) in handles {
                            let column = handle.await.expect("worldgen join burst panicked");
                            apply(conn, &mut state, proto.encode_chunk(cx, cz, &column)).await?;
                            batch_size += 1;
                        }
                    } else {
                        // Borrowed path (tests): keep existing batch-parallel
                        let columns = source.generate(ring.clone()).await;
                        for (&(cx, cz), column) in ring.iter().zip(columns.iter()) {
                            apply(conn, &mut state, proto.encode_chunk(cx, cz, column)).await?;
                            batch_size += 1;
                        }
                    }
                    let gen_ms = t_ring.elapsed().as_millis();
                    let ring_columns = ring.len();
                    tracing::info!(
                        "join ring {}/{}: {} columns, gen+encode={}ms",
                        ring_idx,
                        view_radius,
                        ring_columns,
                        gen_ms,
                    );
                    ring_idx += 1;
                }
                apply(conn, &mut state, proto.end_chunk_batch(batch_size)).await?;
                let chunk_ms = t_chunks.elapsed().as_millis();
                let chunks_sent = batch_size as usize;
                tracing::info!(
                    "join chunks: {} columns in {}ms ({:.0} col/s), {} rings",
                    chunks_sent,
                    chunk_ms,
                    chunks_sent as f64 / (chunk_ms as f64 / 1000.0),
                    ring_idx,
                );

                let t_welcome = Instant::now();
                for directive in proto.welcome_message() {
                    apply(conn, &mut state, directive).await?;
                }

                // `ConfigurationFinished` cannot be reached without an
                // earlier `LoginStart` in any correct `ServerProtocol` (the
                // documented ack-driven state machine above), so `username`
                // is always `Some` here; falling back to an empty string
                // rather than panicking keeps a protocol that violates that
                // contract merely wrong, not a crash.
                let username = username.clone().unwrap_or_default();

                // Issue #438: this connection becomes a player *entity*,
                // before the initial sync below, so (a) every other
                // connection's next pass already sees it and (b) this
                // connection's own initial sync knows which id to exclude.
                // The ticket is moved into `serve_play`; its `Drop` is what
                // deregisters the player on **every** exit path out of that
                // function, of which there are many — see
                // `PlayerRegistry::join`'s own doc comment.
                let player_ticket = entities.players().map(|registry| {
                    registry.join(
                        &username,
                        login_uuid.unwrap_or_else(uuid::Uuid::nil),
                        spawn.pos,
                    )
                });

                // Initial entity sync — the same pass the old single-loop
                // version ran on this same iteration via its trailing
                // `if state == State::Play` check, now made explicit because
                // `serve_play` below takes over the loop entirely. Since #438
                // this also carries the tab-list adds and the other players'
                // spawns, in that order; see [`stream_pass`].
                for directive in stream_pass(
                    proto,
                    entities,
                    &mut streamer,
                    &mut player_list,
                    player_ticket.as_ref(),
                ) {
                    apply(conn, &mut state, directive).await?;
                }

                // Issue #461: derive view centre from the actual spawn
                // chunk rather than assuming (0, 0). For spawn at (8, ~64,
                // 8) both floor to 0, so the centre does not change today;
                // the derivation is the point — when the spawn column or
                // the X/Z offsets move, this follows automatically.
                let spawn_cx = (spawn.pos.x / 16.0).floor() as i32;
                let spawn_cz = (spawn.pos.z / 16.0).floor() as i32;
                let view = ViewTracker::new((spawn_cx, spawn_cz), view_radius);
                // Issues #48/#464. Built here, at the Play handoff, because
                // this is the first point where both halves of a caller's
                // identity are known and settled: `login_uuid` is the uuid
                // `login_success` echoed to this client, and `username` is the
                // name that survived `is_valid_player_name`. Nothing the
                // player later *sends* can change either, which is exactly the
                // property the seam needs — see the `ServerBound::ChatCommand`
                // arm in `dispatch_play_packet`.
                //
                // `login_uuid` cannot be `None` here: reaching Play requires
                // `ConfigurationFinished`, which requires `LoginAcknowledged`,
                // which requires the `LoginStart` arm that sets it. The
                // `unwrap_or_default` is a total fallback rather than a panic
                // because a nil uuid resolves to no player and therefore no
                // permissions — failing closed, not open.
                let player_uuid = login_uuid.unwrap_or_default();
                let commands = CommandSession {
                    dispatch: commands.clone(),
                    caller: CommandCaller::new(player_uuid, username.clone()),
                };
                // Issue #338. The server-authoritative advancement/statistics
                // store for this connection, created at the Play handoff and
                // carried into `serve_play` so the per-packet flush and the
                // `REQUEST_STATS` reply can reach it. The first packet is sent
                // here, at join, exactly where vanilla's
                // `PlayerAdvancements.flushDirty` first-packet path fires:
                // `reset` true, the whole builtin tree as `added`, and every
                // advancement's (currently empty) progress — the client builds
                // its screen from this one packet and nothing after it until a
                // criterion actually flips. A protocol without an
                // `encode_update_advancements` override simply sends nothing
                // (the trait default), so this is a silent no-op rather than a
                // failure on such a version.
                let mut advancements = AdvancementManager::builtin();
                let initial = advancements.initial_update(player_uuid, true);
                apply(conn, &mut state, proto.encode_update_advancements(&initial)).await?;
                let total_ms = t_cfg.elapsed().as_millis();
                let welcome_ms = t_welcome.elapsed().as_millis() - 1; // approx, minus advancement encode
                tracing::info!(
                    "Configuration -> Play: {}ms total (chunks={}ms, welcome/entities/advancements={}ms)",
                    total_ms,
                    chunk_ms,
                    welcome_ms,
                );
                return serve_play(
                    conn,
                    proto,
                    source,
                    entities,
                    view_radius,
                    state,
                    streamer,
                    player_list,
                    player_ticket,
                    view,
                    username,
                    chunks_sent,
                    block_entities,
                    mobs,
                    block_ticks,
                    explosions,
                    weather,
                    sleep_vote,
                    sleep_feed,
                    commands,
                    advancements,
                    player_uuid,
                    border,
                    resource_packs,
                    &mut client_channels,
                    plugin_channels,
                )
                .await;
            }
            // Issue #335. Wire-level plugin messaging, Configuration-phase: a
            // client announces the channels it supports here (via
            // `minecraft:register`) before the Play handoff, so the broadcast
            // drain in `serve_play` filters against them from the first drain
            // onward. Same interpretation as the `dispatch_play_packet` arm:
            // control channels update this connection's supported set, anything
            // else is dispatched to its registered handler (silently dropped
            // when the server registered no interest).
            ServerBound::CustomPayload { channel, data } => {
                if !client_channels.apply_custom_payload(&channel, &data) {
                    plugin_channels.dispatch(&channel, &data);
                }
            }
            ServerBound::KeepAlive { .. }
            | ServerBound::PlayerMoved { .. }
            | ServerBound::PlayerRotated { .. }
            | ServerBound::PlayerStatusOnly { .. }
            | ServerBound::BlockAction { .. }
            | ServerBound::UseItemOn { .. }
            | ServerBound::DifficultyChanged { .. }
            | ServerBound::DifficultyLockChanged { .. }
            | ServerBound::GameRuleChanged { .. }
            | ServerBound::CarriedItemChanged { .. }
            | ServerBound::ContainerClicked { .. }
            | ServerBound::ContainerClosed { .. }
            | ServerBound::Attack { .. }
            | ServerBound::PlayerInput { .. }
            | ServerBound::CreativeModeSlotSet { .. }
            | ServerBound::ClientCommand { .. }
            | ServerBound::ClientInformationChanged { .. }
            | ServerBound::ChunkBatchAcknowledged { .. }
            // Unreachable here by construction, like the Play-phase variants
            // above it: every `ServerProtocol::decode` arm producing this is
            // gated on `State::Play`, and this loop hands off to `serve_play`
            // the moment Play is reached. Listed rather than folded into a
            // wildcard so that adding a variant stays a compile error.
            | ServerBound::ChatCommand { .. }
            | ServerBound::Chat { .. }
            | ServerBound::PlayerCommand { .. }
            | ServerBound::Ignored => {}
        }
    }

    match username {
        Some(username) => Ok(ServeSummary {
            username,
            chunks_sent: 0,
            inventory: PlayerInventory::default(),
        }),
        None => Err(ServerError::ClosedBeforeLogin),
    }
}

/// The neighbour cell one step off `pos` in `face`'s direction — vanilla's
/// `BlockPos.relative(Direction)`, used below to find the placement cell when
/// the directly clicked block cannot be replaced.
fn relative(pos: BlockPos, face: BlockFace) -> BlockPos {
    let (dx, dy, dz) = match face {
        BlockFace::Down => (0, -1, 0),
        BlockFace::Up => (0, 1, 0),
        BlockFace::North => (0, 0, -1),
        BlockFace::South => (0, 0, 1),
        BlockFace::West => (-1, 0, 0),
        BlockFace::East => (1, 0, 0),
    };
    BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
}

/// Cadence of the periodic open-container sync ([`sync_open_container`]) —
/// the piece that answers `docs/block-entities.md`'s own design question,
/// "a furnace mutates its own container without a client click": nothing
/// about that mutation is a response to any inbound packet, so a connection
/// with a window open needs its own timer polling the block entity, exactly
/// like [`VITALS_TICK_INTERVAL`] polls submersion. Matches the background
/// tick loop's own cadence (`block_entities.rs`'s `BLOCK_ENTITY_TICK_INTERVAL`)
/// so a change is visible within one real tick of it happening, not only the
/// next time the client happens to send a packet.
#[cfg(not(target_arch = "wasm32"))]
const CONTAINER_SYNC_INTERVAL: Duration = Duration::from_millis(50);

/// Which block-entity container (if any) this connection currently has open:
/// the container id the client will echo back in every `container_click`/
/// `container_close` for it, the world position it targets, and how many of
/// its own slots ([`BlockEntity::container_slots`]) precede the standard
/// player-inventory tail in that menu's slot numbering (see
/// `crate::inventory::container_menu_slot`, the click-side consumer of this
/// same number).
#[derive(Debug, Clone, Copy)]
struct OpenContainer {
    window_id: i32,
    pos: BlockPos,
    container_size: usize,
    /// Vanilla's `AbstractContainerMenu.stateId`, wrapping at `32767`
    /// (`AbstractContainerMenu::incrementStateId`). Bumped by every content/
    /// slot send (this struct's own [`next_state_id`](Self::next_state_id)),
    /// never by a `container_set_data` send — vanilla's `broadcastChanges`
    /// does not touch `stateId` for a data-only change either. This crate
    /// does not validate a click's echoed value against it (see
    /// `docs/server-inventory.md`'s existing scope note for window `0`,
    /// which applies identically here) — it exists so a real client
    /// observes vanilla's own incrementing behaviour rather than a
    /// suspicious constant.
    state_id: i32,
}

impl OpenContainer {
    /// Bumps and returns the next state id, matching
    /// `AbstractContainerMenu::incrementStateId`'s exact wrap.
    fn next_state_id(&mut self) -> i32 {
        self.state_id = (self.state_id + 1) & 32767;
        self.state_id
    }
}

/// Per-connection bookkeeping for [`OpenContainer`]'s periodic sync
/// ([`sync_open_container`]): the container slots and menu-data properties
/// last pushed to the client, so a background mutation (a furnace's own
/// tick, not any click) can be diffed and only the changed entries re-sent —
/// the same shape [`EntityStreamer`] already established for entity spawn/
/// update/remove.
#[derive(Debug, Default, Clone)]
struct ContainerSync {
    slots: Vec<Option<ItemStack>>,
    data: Vec<i32>,
}

/// Reads `pos`'s container slots and menu-data properties, or a pair of empty
/// vectors if nothing is registered there — the one read [`open_container_screen`]
/// (opening a menu) and the `container_sync_tick` arm of [`serve_play`]
/// (re-reading a background-ticked entity) both need, against the same
/// [`BlockEntityHandle`].
fn container_state(
    block_entities: &BlockEntityHandle,
    pos: BlockPos,
) -> (Vec<Option<ItemStack>>, Vec<i32>) {
    block_entities.with(|reg| match reg.get(pos) {
        Some(entity) => (entity.container_slots(), entity.data_properties()),
        None => (Vec::new(), Vec::new()),
    })
}

/// Diffs `current_slots`/`current_data` (freshly read off the block entity at
/// `open.pos`) against what [`ContainerSync`] last pushed to this
/// connection, returning the directives that bring the client back in sync —
/// only the entries that actually changed, each via
/// [`ServerProtocol::encode_container_slot`]/[`encode_container_data`](ServerProtocol::encode_container_data).
///
/// This is the one piece of Job 1 with no client packet driving it at all:
/// [`open_container_screen`] covers "a player opens a menu" and
/// [`apply_container_clicked`] covers "a player clicks in one," but a
/// furnace's own background tick (`crate::tick::run_tick_loop`, issue #284 —
/// previously `crate::block_entities::run_block_entity_tick_loop`, running
/// independently of any connection) is neither — see
/// `docs/block-entities.md`'s own note on this. A caller (`serve_play`'s
/// `container_sync_tick` arm) is expected to call this on its own timer,
/// passing a fresh read of the entity's current state each time; this
/// function does no I/O and no locking itself; a plain `#[test]` can drive
/// it directly with no `Connection`/tokio runtime at all.
fn sync_open_container<P: ServerProtocol>(
    proto: &P,
    open: &mut OpenContainer,
    sync: &mut ContainerSync,
    current_slots: Vec<Option<ItemStack>>,
    current_data: Vec<i32>,
) -> Vec<ServerDirective> {
    let mut directives = Vec::new();
    for (index, (new, old)) in current_slots.iter().zip(sync.slots.iter()).enumerate() {
        if new != old {
            let state_id = open.next_state_id();
            directives.push(proto.encode_container_slot(
                open.window_id,
                state_id,
                index as i32,
                new.as_ref(),
            ));
        }
    }
    for (index, (new, old)) in current_data.iter().zip(sync.data.iter()).enumerate() {
        if new != old {
            directives.push(proto.encode_container_data(open.window_id, index as i32, *new));
        }
    }
    sync.slots = current_slots;
    sync.data = current_data;
    directives
}

/// Vanilla's own per-menu display name is a translatable component
/// (`container.furnace`, `container.hopper`, resolved client-side from the
/// current language); [`ServerProtocol::encode_open_screen`] only ever
/// writes a **literal** string component (see that trait method's own doc
/// comment for why), so this is the literal English text substituted in its
/// place — cosmetic only, never read by any gameplay logic on either side.
fn container_title(menu: &str) -> &'static str {
    match menu {
        "minecraft:furnace" => "Furnace",
        "minecraft:smoker" => "Smoker",
        "minecraft:blast_furnace" => "Blast Furnace",
        "minecraft:hopper" => "Hopper",
        _ => "Container",
    }
}

/// Opens a block-entity's container screen for this connection, mirroring
/// vanilla's `ServerPlayer::openMenu` end to end: a fresh container id
/// (`nextContainerCounter`: `1..=100`, wrapping — `ServerPlayer.java:1329-1331`),
/// an `open_screen` send, then an immediate full `container_set_content`
/// plus every `container_set_data` property (`initMenu`'s `addSlotListener`
/// triggers `broadcastFullState` the instant the menu is constructed,
/// `ServerPlayer.java:1343-1356`).
///
/// `pos` must already hold a [`BlockEntity`] whose [`BlockEntity::menu_name`]
/// is `Some` — the caller ([`apply_use_item_on`]) checks this before calling
/// in. The `container_set_content` item list is this entity's own
/// [`BlockEntity::container_slots`] followed by the player's standard 27
/// main-storage + 9 hotbar slots (never armour/off-hand — see
/// `crate::inventory::ContainerMenuSlot`'s doc comment), with no
/// cursor/carried stack (this crate's [`PlayerInventory`] tracks no cursor
/// field at all — `docs/server-inventory.md`'s existing scope note, which
/// applies identically to any window).
#[allow(clippy::too_many_arguments)]
async fn open_container_screen<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    block_entities: &BlockEntityHandle,
    inventory: &PlayerInventory,
    pos: BlockPos,
    menu: &'static str,
    next_window_id: &mut i32,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    *next_window_id = *next_window_id % 100 + 1;
    let window_id = *next_window_id;

    let (own_slots, data) = container_state(block_entities, pos);

    apply(
        conn,
        state,
        proto.encode_open_screen(window_id, menu, container_title(menu)),
    )
    .await?;

    let mut items = own_slots.clone();
    for native in 9..=35 {
        items.push(inventory.native(native).cloned());
    }
    for native in 0..=8 {
        items.push(inventory.native(native).cloned());
    }

    let mut opened = OpenContainer {
        window_id,
        pos,
        container_size: own_slots.len(),
        state_id: 0,
    };
    let state_id = opened.next_state_id();
    apply(
        conn,
        state,
        proto.encode_container_content(window_id, state_id, &items, None),
    )
    .await?;

    for (index, value) in data.iter().enumerate() {
        apply(
            conn,
            state,
            proto.encode_container_data(window_id, index as i32, *value),
        )
        .await?;
    }

    *open_container = Some(opened);
    *container_sync = ContainerSync {
        slots: own_slots,
        data,
    };
    Ok(())
}

/// Applies one block-breaking phase, mirroring
/// `ServerPlayerGameMode.handleBlockBreakAction`'s three destroy ordinals —
/// simplified per this crate's documented scope (`docs/block-edit.md`): no
/// hardness/timing validation (the client's own predictor already gates when
/// it sends `StopDestroy` — see `lodestone-shell`'s `drive_mining`), and no
/// interaction-range or spawn-protection checks (this crate does not track
/// player position beyond the view-tracking column).
///
/// `pending_break` is this connection's tracked in-progress dig — the
/// version-free analogue of vanilla's `destroyPos` field. It is what makes
/// `StartDestroy` + `StopDestroy` break a block while `StartDestroy` +
/// `AbortDestroy` does not, and what makes a `StopDestroy` for a position
/// nobody started a no-op, mirroring vanilla's own
/// `pos.equals(this.destroyPos)` guard (`ServerPlayerGameMode.java:217`).
///
/// **Also removes a broken position's [`BlockEntity`], if any, from the
/// registry** — `docs/block-entities.md`'s own note that only placement
/// wrote into the registry ("once block breaking learns to consult this
/// registry" was future work) now matters for correctness, not just
/// tidiness: a real screen can be open against one, and leaving a dangling
/// entry would let a stale `container_click` keep mutating a container
/// backing a block that no longer exists. If the connection's own
/// [`OpenContainer`] pointed at the broken position, it is cleared too —
/// this crate does not send a `container_close` to force the client's UI
/// shut in that case (a real, documented gap, not attempted here; vanilla's
/// own equivalent is `AbstractContainerMenu::stillValid` polling, which this
/// crate does not model).
#[allow(clippy::too_many_arguments)]
async fn apply_block_action<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    pending_break: &mut Option<BlockPos>,
    block_entities: &BlockEntityHandle,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    action: BlockActionKind,
    pos: BlockPos,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
{
    match action {
        BlockActionKind::StartDestroy => {
            *pending_break = Some(pos);
        }
        BlockActionKind::AbortDestroy => {
            if *pending_break == Some(pos) {
                *pending_break = None;
            }
        }
        BlockActionKind::StopDestroy => {
            if *pending_break == Some(pos) {
                *pending_break = None;
                source.set_block(pos.x, pos.y, pos.z, AIR);
                block_entities.with(|reg| {
                    reg.remove(pos);
                });
                if open_container.as_ref().is_some_and(|open| open.pos == pos) {
                    *open_container = None;
                    *container_sync = ContainerSync::default();
                }
                let directive = proto.encode_block_update(pos.x, pos.y, pos.z, AIR);
                apply(conn, state, directive).await?;
            }
        }
    }
    Ok(())
}

/// Vanilla's `Direction.fromYRot` (`Direction.java:291-293`) restricted to the
/// four horizontal directions, from a player yaw in degrees.
///
/// The 2d-data layout is `south=0, west=1, north=2, east=3`
/// (`Direction.java:33-38`), so `floor(yaw / 90 + 0.5) & 3` maps yaw `0` →
/// south, `90` → west, `±180` → north, `-90` → east — the same "yaw 0 =
/// south, increasing clockwise" convention the shell's `camera_rig`/`hud`
/// use for the yaw this server receives from `move_player_rot`. Implemented
/// as a range match on the wrapped `[0, 360)` value rather than the bit-mask
/// formula, with the 45°/135°/225°/315° midpoints landing exactly as the
/// mask's `floor` does.
///
/// The returned direction is the one the player is **looking**, matching the
/// horizontal component of `BlockPlaceContext.getNearestLookingDirection` —
/// a placed diode then applies `.opposite()` so the block faces the player.
#[must_use]
fn horizontal_look_direction(yaw: f32) -> Direction {
    match yaw.rem_euclid(360.0) {
        y if (45.0..135.0).contains(&y) => Direction::West,
        y if (135.0..225.0).contains(&y) => Direction::North,
        y if (225.0..315.0).contains(&y) => Direction::East,
        _ => Direction::South,
    }
}

/// `BlockState.getStateForPlacement` for the blocks this crate places with a
/// yaw-derived orientation, and `None` for every other block — the caller
/// then keeps the census's bare default-state name (stairs/slabs/logs/dust
/// need click-face, cursor and neighbour state this path does not decode,
/// `docs/block-edit.md`).
///
/// `player_yaw` is `None` before the first packet carrying angles arrives;
/// placement then falls back to the block's default state.
///
/// The two `DiodeBlock` families cite `DiodeBlock.getStateForPlacement`
/// (`DiodeBlock.java:155-158`): `FACING = getHorizontalDirection().getOpposite()`
/// — the block faces the player, so the **opposite** of the player's look
/// direction. The observer is the deliberate exception:
/// `ObserverBlock.getStateForPlacement` (`ObserverBlock.java:133-136`) sets
/// `FACING = getNearestLookingDirection().getOpposite().getOpposite()`, a
/// double negation that is the player's **look** direction — the observer
/// watches the block the player is looking at, and its redstone output faces
/// the player. Not opposite: an observer placed while looking north watches
/// north.
#[must_use]
fn placed_block_state(block: &str, player_yaw: Option<f32>) -> Option<String> {
    let look = horizontal_look_direction(player_yaw?);
    match block {
        REPEATER => Some(set_repeater(look.opposite(), 1, false, false)),
        COMPARATOR => Some(set_comparator(look.opposite(), false, false, 0)),
        OBSERVER => Some(set_observer(look, false)),
        _ => None,
    }
}

/// Which of a brewing stand's five slots a held item routes to — decided by
/// item identity alone, mirroring `BrewingStandBlockEntity.canPlaceItem`
/// (`:217-227`: slots 0-2 take potions/bottles, slot 3 takes any
/// `potionBrewing.isIngredient`, slot 4 takes `ItemTags.BREWING_FUEL`).
enum BrewingSlot {
    /// Blaze powder — `ItemTags.BREWING_FUEL`'s sole member (slot 4).
    Fuel,
    /// A bottle this crate can represent — a water bottle, whose potion the
    /// item id fully determines. See [`bottle_from_item`] for why the other
    /// three bottle-shaped items are *not* insertable (slots 0-2).
    Bottle(Bottle),
    /// Any [`is_ingredient`] item (slot 3).
    Ingredient,
}

/// The outcome of one right-click on a brewing stand — this crate's
/// one-item-per-click stand-in for the brewing menu it cannot open (see
/// [`BlockEntity::menu_name`]'s doc comment for why a brewing stand answers
/// `None` there), the same shape `ComposterBlock.useItemOn` establishes for
/// the composter.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BrewingInsertOutcome {
    /// An item moved out of the player's hand into the stand; the player's
    /// selected hotbar slot now holds `selected` (`None` when the last of a
    /// stack was consumed).
    Inserted(Option<ItemStack>),
    /// The right-click was consumed by the stand but nothing moved — a valid
    /// brewing item whose matching slot was already full (or held a different
    /// stack). No placement may follow: this stands in for the menu that
    /// would have consumed the click in vanilla. Distinct from `NotBrewing`
    /// because some potion ingredients (`minecraft:stone`, `slime_block`,
    /// `cobweb`) are themselves placeable blocks, and a full brewing stand
    /// must never silently place one.
    Consumed,
    /// The held item belongs to no brewing-stand slot — the caller falls
    /// through to ordinary placement exactly as before this branch existed.
    NotBrewing,
}

/// The one bottle-shaped item this crate can put in a brewing-stand bottle
/// slot: a water bottle, whose potion is fully determined by its item id.
///
/// A `minecraft:potion`/`splash_potion`/`lingering_potion` stack carries its
/// actual potion in the `minecraft:potion_contents` data component, which
/// `lodestone_model::ItemComponents` does not model (see `brewing.rs`'s
/// module doc: "no potion-contents component anywhere in `ItemComponents`"),
/// so its contents are unknowable here. Inserting one with a guessed potion
/// would let the mix table brew a *wrong* potion from it, so it is rejected
/// rather than guessed — the same declared gap the `Bottle` type itself is.
#[must_use]
fn bottle_from_item(item: &str) -> Option<Bottle> {
    match item {
        "minecraft:water_bottle" => Some(Bottle::new(BottleKind::Potion, "minecraft:water")),
        _ => None,
    }
}

/// Routes `item` to the brewing-stand slot it belongs in, or `None` if it
/// belongs nowhere — mirroring `BrewingStandBlockEntity.canPlaceItem`
/// (`:217-227`). Blaze powder is checked first even though it is *also* a
/// potion ingredient (strength, `brewing.rs`'s `potion_mix`): the fuel slot
/// wins, matching the slot-4 test vanilla applies.
#[must_use]
fn brewing_slot_for(item: &str) -> Option<BrewingSlot> {
    if item == "minecraft:blaze_powder" {
        return Some(BrewingSlot::Fuel);
    }
    if let Some(bottle) = bottle_from_item(item) {
        return Some(BrewingSlot::Bottle(bottle));
    }
    if is_ingredient(item) {
        return Some(BrewingSlot::Ingredient);
    }
    None
}

/// One ingredient/fuel stack's cap before a further right-click is refused —
/// vanilla's default `MAX_STACK_SIZE` (64), the same number `furnace.rs`'s
/// [`MAX_STACK_SIZE`](crate::furnace::MAX_STACK_SIZE) records for output stacks.
const BREWING_STACK_CAP: u32 = 64;

/// The window-0 menu slot of the hotbar's first (native) slot — vanilla's
/// `InventoryMenu`: hotbar menu slots `36..=44` address native hotbar `0..=8`
/// (see `crate::inventory::PlayerInventory`'s own doc table). The window-0
/// `container_set_slot` [`apply_use_item_on`] sends after a brewing insert
/// addresses the selected hotbar slot by this menu index, not its native one.
const WINDOW_ZERO_HOTBAR_FIRST: i32 = 36;

/// Attempts to insert the player's held item into the brewing stand at `pos`,
/// consuming one from the selected hotbar stack when it lands — the wiring
/// that makes `BrewingStand::set_bottle`/`set_ingredient`/`set_fuel_item` (and
/// therefore the whole brew state machine) reachable from a player at all.
/// See [`BrewingInsertOutcome`] for the three outcomes.
///
/// Merging follows the slot's own shape: fuel and ingredient stacks merge into
/// an existing matching stack up to [`BREWING_STACK_CAP`], while a bottle only
/// ever occupies an empty slot (vanilla's `canPlaceItem` empty-slot test,
/// `:225`, and bottles do not stack).
fn insert_into_brewing_stand(
    block_entities: &BlockEntityHandle,
    inventory: &mut PlayerInventory,
    pos: BlockPos,
) -> BrewingInsertOutcome {
    // The registry lookup happens first, so a right-click on any other block
    // is untouched by this branch entirely.
    let is_stand = block_entities.with(|reg| matches!(reg.get(pos), Some(BlockEntity::BrewingStand(_))));
    if !is_stand {
        return BrewingInsertOutcome::NotBrewing;
    }
    let Some(held) = inventory.selected_item().cloned() else {
        return BrewingInsertOutcome::NotBrewing;
    };
    let item = held.item.to_string();
    let Some(slot) = brewing_slot_for(&item) else {
        return BrewingInsertOutcome::NotBrewing;
    };

    // The slot write happens inside the registry lock — nothing else can see
    // a half-inserted item, and the write is validated against the live slot
    // contents in the same critical section.
    let moved = block_entities.with(|reg| {
        let Some(entity) = reg.get_mut(pos) else {
            return false;
        };
        let BlockEntity::BrewingStand(stand) = entity else {
            return false;
        };
        match slot {
            BrewingSlot::Fuel => match stand.fuel_item() {
                Some(("minecraft:blaze_powder", count)) if count < BREWING_STACK_CAP => {
                    stand.set_fuel_item(Some(("minecraft:blaze_powder".into(), count + 1)));
                    true
                }
                None => {
                    stand.set_fuel_item(Some(("minecraft:blaze_powder".into(), 1)));
                    true
                }
                _ => false,
            },
            BrewingSlot::Bottle(bottle) => {
                for index in 0..3 {
                    if stand.bottle(index).is_none() {
                        stand.set_bottle(index, Some(bottle));
                        return true;
                    }
                }
                false
            }
            BrewingSlot::Ingredient => match stand.ingredient() {
                Some((existing, count)) if existing == item.as_str() && count < BREWING_STACK_CAP => {
                    stand.set_ingredient(Some((item.clone(), count + 1)));
                    true
                }
                None => {
                    stand.set_ingredient(Some((item.clone(), 1)));
                    true
                }
                _ => false,
            },
        }
    });
    if !moved {
        return BrewingInsertOutcome::Consumed;
    }

    // Consume one from the held stack — vanilla's `itemStack.consume(1)`.
    let native = usize::from(inventory.selected_hotbar_slot());
    let remainder = match inventory.native(native).cloned() {
        Some(mut stack) => {
            stack.count -= 1;
            if stack.count == 0 {
                None
            } else {
                Some(stack)
            }
        }
        None => None,
    };
    inventory.set_native(native, remainder.clone());
    BrewingInsertOutcome::Inserted(remainder)
}

/// The seed for the per-connection [`SpawnRng`] that draws a composter's
/// insert roll — the same explicit-seed shape `tick::RANDOM_TICK_BEHAVIOR_SEED`
/// uses for crop growth, and for the same reason: this crate takes seeds
/// explicitly rather than drawing them, so a test can replay an exact
/// outcome. Per-*connection*, not per-*level*: two players feeding the same
/// composter draw from different streams, which only changes which roll a
/// given insert sees — the shared fill state lives in the registry regardless.
const COMPOSTER_BEHAVIOR_SEED: u64 = 0x5EED_C011;

/// What a right-click on a composter did, so [`apply_use_item_on`] can decide
/// whether the ordinary placement logic may still run.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ComposterUseOutcome {
    /// `pos` held no composter block entity, or the click left both the
    /// composter and the player's hand untouched (a non-compostable held item,
    /// or an empty hand on a not-yet-ready composter) — vanilla's
    /// `super.useItemOn`/`useWithoutItem` both `PASS`, so the placement logic
    /// below must run.
    NotComposter,
    /// The composter consumed the click but nothing moved — level `7`,
    /// waiting on its scheduled tick (vanilla's `useItemOn` returns `SUCCESS`
    /// there, `ComposterBlock.java:248-270`, and the hand is untouched). No
    /// placement may follow.
    Noop,
    /// One item was consumed from the player's hand (`itemStack.consume(1)`,
    /// `ComposterBlock.java:263`). `remainder` is the hand's new contents for
    /// the caller to push as a window-0 slot update; `block_state` is the new
    /// block state to write — `Some` when the fill level advanced, `None` on a
    /// failed roll (the item is still consumed; only the state is unchanged).
    Consumed {
        remainder: Option<ItemStack>,
        block_state: Option<String>,
    },
    /// Bone meal was extracted (level `8` -> `0`, `extractProduce`,
    /// `ComposterBlock.java:298-309`) — the caller spawns the item entity and
    /// writes `block_state`.
    Extracted { block_state: String },
}

/// The decision `apply_composter_use`'s registry-locked step makes; the caller
/// then applies the world side effects (inventory shrink, bone-meal spawn).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ComposterStep {
    /// Not a composter, or the click leaves everything untouched — the
    /// placement logic below may run.
    FallThrough,
    /// Click consumed, nothing changed (level 7 waiting).
    Noop,
    /// One item consumed; `block_state` is `Some` when the fill level advanced.
    Consumed { block_state: Option<String> },
    /// Level 8 reached — extract bone meal, reset to 0.
    Extract,
}

/// The block-state string for a composter at fill `level` — vanilla's
/// `minecraft:composter[level=0..8]`, the string the client's
/// `resolve_state_id` maps to the composter's per-level ids (block 229 in
/// `crates/lodestone-data/src/generated/block_states.rs`).
fn composter_state(level: u8) -> String {
    format!("minecraft:composter[level={level}]")
}

/// Applies one right-click on the composter at `pos` — the wiring that makes
/// `Composter::insert`/`extract` (and therefore the whole seven-tier fill
/// state machine, issue #249) reachable from a player at all.
///
/// Mirrors `ComposterBlock.useItemOn`'s order: the held item (if any) is
/// offered to the fill machine first, and whatever the item offer does not
/// consume falls through to `useWithoutItem` (`ComposterBlock.java:272-283`),
/// which extracts at level `8` and otherwise `PASS`s. Concretely:
///
/// * an empty hand on a ready (level `8`) composter extracts the bone meal;
/// * a compostable item is rolled against its chance, consuming one from the
///   hand either way (a failed roll still eats the item — vanilla
///   `itemStack.consume(1)`);
/// * a compostable item at level `7` (waiting on its scheduled tick) is
///   consumed as a click but changes nothing;
/// * a *non*-compostable item never reaches `insert`'s level gate, because at
///   level `7` vanilla's `COMPOSTABLES.containsKey` guard fails *before* the
///   `fillLevel < 7` add, so the click falls through to placement there while
///   the same item at level `8` extracts. Checking the chance table up front
///   reproduces that ordering (`insert` alone would answer `NotAccepting` for
///   both a compostable and a non-compostable item at level 7, and nothing
///   could tell them apart).
///
/// `roll` is an injected `[0.0, 1.0)` sample the caller draws once per
/// interaction — the "caller supplies the randomness" shape
/// [`Composter::insert`] documents, so a test can pin an exact outcome.
///
/// The inventory shrink and the bone-meal spawn are **this** function's job,
/// not the block-state writer's: item consumption lives with the caller (the
/// composter never holds the inserted item — see `composter.rs`'s module
/// doc), and `spawn_item` gives the extraction its world-facing item entity
/// (which `MobSim::snapshots` streams to clients). The caller writes the
/// returned `block_state` (if any) and pushes the window-0 slot update.
fn apply_composter_use(
    block_entities: &BlockEntityHandle,
    inventory: &mut PlayerInventory,
    mobs: &MobHandle,
    pos: BlockPos,
    roll: f64,
) -> ComposterUseOutcome {
    // The registry lookup happens first, so a right-click on any other block
    // is untouched by this branch entirely.
    let held = inventory.selected_item().cloned();
    let step = block_entities.with(|reg| {
        let Some(BlockEntity::Composter(composter)) = reg.get_mut(pos) else {
            return ComposterStep::FallThrough;
        };
        let Some(held) = held else {
            // Empty hand: `useWithoutItem` (`ComposterBlock.java:272-283`).
            if composter.extract() {
                return ComposterStep::Extract;
            }
            return ComposterStep::FallThrough;
        };
        let item = held.item.to_string();
        // Non-compostable items fall through to `useWithoutItem` (see the doc
        // comment above for why this must be checked before `insert`, not by
        // it).
        if compostable_chance(&item).is_none() {
            if composter.extract() {
                return ComposterStep::Extract;
            }
            return ComposterStep::FallThrough;
        }
        match composter.insert(&item, roll) {
            InsertOutcome::Consumed { level_increased } => {
                ComposterStep::Consumed {
                    block_state: level_increased.then(|| composter_state(composter.level())),
                }
            }
            InsertOutcome::NotAccepting => {
                // Level 7 (waiting, compostable): vanilla `useItemOn` returns
                // SUCCESS with the hand untouched. Level 8 (ready): the item
                // offer failed `fillLevel < 8`, so the `useWithoutItem` half
                // extracts instead.
                if composter.extract() {
                    ComposterStep::Extract
                } else {
                    ComposterStep::Noop
                }
            }
            InsertOutcome::NotCompostable => unreachable!(
                "compostable_chance() is the same table insert() consults; \
                 the up-front guard above rules this out"
            ),
        }
    });
    match step {
        ComposterStep::FallThrough => ComposterUseOutcome::NotComposter,
        ComposterStep::Noop => ComposterUseOutcome::Noop,
        ComposterStep::Consumed { block_state } => {
            // Consume one from the selected hotbar stack — vanilla
            // `itemStack.consume(1)`, the same shrink
            // `insert_into_brewing_stand` performs for its own consumed insert.
            let native = usize::from(inventory.selected_hotbar_slot());
            let remainder = match inventory.native(native).cloned() {
                Some(mut stack) => {
                    stack.count -= 1;
                    if stack.count == 0 {
                        None
                    } else {
                        Some(stack)
                    }
                }
                None => None,
            };
            inventory.set_native(native, remainder.clone());
            ComposterUseOutcome::Consumed {
                remainder,
                block_state,
            }
        }
        ComposterStep::Extract => {
            // `extractProduce` (`ComposterBlock.java:298-309`): exactly one
            // bone meal at the block's top, with the hand untouched. Vanilla's
            // `offsetRandomXZ(0.7F)` jitter on the velocity is skipped because
            // this crate has no gaussian f64 source; a gentle upward toss is
            // enough to leave the block.
            mobs.with(|sim| {
                sim.spawn_item(
                    "minecraft:bone_meal".parse().expect("bone_meal is a valid item id"),
                    Vec3::new(
                        pos.x as f64 + 0.5,
                        pos.y as f64 + 1.01,
                        pos.z as f64 + 0.5,
                    ),
                    Vec3::new(0.0, 0.2, 0.0),
                    ItemLifecycle::newly_dropped(1, 64),
                );
            });
            ComposterUseOutcome::Extracted {
                block_state: composter_state(0),
            }
        }
    }
}

/// Applies a right-click placement, mirroring
/// `ServerGamePacketListenerImpl.handleUseItemOn`'s replace-vs-relative
/// choice of placement cell (`BlockPlaceContext`'s constructor: place at the
/// clicked block if it `canBeReplaced`, otherwise at its `face`-neighbour) —
/// simplified per this crate's documented scope (`docs/block-edit.md`): no
/// survival/collision validation beyond "is the target cell currently
/// replaceable" (air or a fluid — see [`is_air_or_fluid`]). Per-block
/// orientation is partial: the redstone directional families (repeater,
/// comparator, observer) derive their facing from the placing player's yaw
/// via [`placed_block_state`] (issue #475), while the click-face/cursor-driven
/// families (stairs/slabs/doors) would need a precise cursor hit this crate
/// does not decode and still place with their default state.
///
/// **Placement honours the held item for every block in the game** (#466).
/// `inventory`'s currently selected item is resolved through
/// [`lodestone_data::block_items::block_for_item`] — the 26.2 census of
/// `BlockItem.getBlock()`, dumped from the real jar — which decides both
/// whether a placement happens and which block it writes.
///
/// Before #466 this went through [`block_entity_for_item`] alone, whose
/// `None` arm wrote [`crate::chunk::STONE`]. That table by design resolves only the six
/// block-entity items, so the `None` arm was the path taken by **every
/// ordinary block**: dirt, planks, wool and glass all placed stone. The two
/// are now composed rather than swapped — the census gates the placement and
/// names the block, and `block_entity_for_item` is consulted second, purely
/// to insert the live [`crate::block_entities::BlockEntity`] the six ticking
/// blocks need.
///
/// **A non-placeable item now places nothing.** A sword, a bucket, a spawn
/// egg or an empty hand leaves the world untouched, where it previously
/// substituted stone. That is a deliberate behaviour change and the correct
/// one; the `block_update` for both cells is still sent below, so a client
/// that predicted a placement is corrected rather than left desynchronised.
///
/// **Block *state* is now partial** (`docs/block-edit.md`): the redstone
/// directional families place with a yaw-derived `facing` (issue #475 — the
/// repeater that always faced north is fixed), but the state that depends on
/// the click face, cursor and neighbours — stairs, slabs, logs and redstone
/// dust's connection state — still places with the block's default state.
/// #466 is about placing the *right block*; the right *state* for the
/// remaining families is a separate and larger piece of work.
///
/// Sends [`ServerProtocol::encode_block_update`] for **both** `pos` and its
/// `face`-neighbour unconditionally, matching vanilla's own
/// `handleUseItemOn` (`ServerGamePacketListenerImpl.java:1397-1398`), which
/// sends both regardless of whether the placement succeeded — this doubles
/// as the correction for a client that predicted a placement the server
/// rejected.
///
/// **Right-clicking a block that already has an *openable* container opens
/// its screen instead of attempting a placement at all** — the closing half
/// of `docs/block-entities.md`'s gap 3. Mirrors vanilla's own order:
/// `ServerGamePacketListenerImpl.handleUseItemOn` runs the clicked block's
/// own `useItemOn`/`useWithoutItem` (which is what opens a furnace/hopper's
/// menu) **before** any `BlockPlaceContext` placement logic, and a block
/// that opens a menu never falls through to placement.
///
/// **A brewing stand at `pos` is this "clicked block's own use" step too,
/// but without a menu** (issue #252): it cannot be opened — `menu_name`
/// answers `None`, because its `Bottle` slots are not real `ItemStack`s — so
/// [`insert_into_brewing_stand`] stands in for the menu with a direct
/// one-item-per-click insert, the shape `ComposterBlock.useItemOn` uses for
/// the composter (which is the *other* kind `menu_name` answers `None` for,
/// there because vanilla gives a composter no menu at all). A held item that
/// belongs in a brewing stand is routed into the matching slot and consumed;
/// an unrelated held item still falls through to the placement logic below
/// exactly as before this change.
#[allow(clippy::too_many_arguments)]
async fn apply_use_item_on<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    pos: BlockPos,
    face: BlockFace,
    // Issue #329. The player's world-space position, for the bed-respawn
    // reach test (vanilla's `bedInRange`, bed ±3 x/z and ±2 y). `None` until
    // the first `PlayerMoved` packet arrives; a bed click before any move
    // skips the reach test rather than rejecting (see
    // [`is_legal_bed_respawn`]'s doc comment).
    player_pos: Option<Vec3>,
    // Issue #329. The player's per-player respawn point, written when a legal
    // bed is right-clicked (see the bed arm below). `&mut`: the set writes
    // through this slot.
    respawn: &mut Option<RespawnPoint>,
    // Issue #475. The placing player's yaw, so the redstone directional
    // families can derive their `facing` (see [`placed_block_state`]). `None`
    // until the first packet carrying angles arrives; placement then falls
    // back to the block's default state.
    player_yaw: Option<f32>,
    // `&mut`, not `&`: a brewing-stand insertion consumes one item from the
    // player's selected hotbar stack (issue #252), and only a mutable
    // inventory can write the remainder back.
    inventory: &mut PlayerInventory,
    block_entities: &BlockEntityHandle,
    next_window_id: &mut i32,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    // Issue #249. The composter interaction: `mobs` so a level-8 extraction
    // can spawn its bone-meal item entity, and `roll` — a fresh `[0.0, 1.0)`
    // draw from the connection's [`SpawnRng`], one per right-click, so the
    // fill machine's per-item chance sees a live sample rather than a constant
    // (the caller-supplied-roll shape `Composter::insert` documents).
    mobs: &MobHandle,
    roll: f64,
    // Issue #465, the delayed half. `propagate_placement` below resolves
    // everything synchronous (dust) against a `ScheduledTickQueue` it then
    // discards; a torch/repeater/comparator/observer instead *schedules*, and
    // only `tick::run_tick_loop` owns a queue those can land in. This asks the
    // loop to redo the fan-out on its next iteration, where the schedule
    // survives. See `BlockTickFeed`'s own doc comment.
    block_ticks: &BlockTickFeed,
    // Issue #325. The night-skip vote, written on a bed click (the bed arm
    // above — `lay_down`), and the key it stores this connection's player
    // under — see `dispatch_play_packet`'s parameter comment.
    sleep_vote: &SleepVote,
    player_entity_id: i32,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
{
    let existing_menu = block_entities.with(|reg| reg.get(pos).and_then(BlockEntity::menu_name));
    if let Some(menu) = existing_menu {
        return open_container_screen(
            conn,
            proto,
            state,
            block_entities,
            inventory,
            pos,
            menu,
            next_window_id,
            open_container,
            container_sync,
        )
        .await;
    }

    // Issue #252, the missing-consumer half: a right-click on a brewing stand
    // routes the held item into the matching slot (fuel/bottle/ingredient)
    // and consumes one from the player's hand. See
    // [`insert_into_brewing_stand`]'s doc comment for the three outcomes.
    match insert_into_brewing_stand(block_entities, inventory, pos) {
        BrewingInsertOutcome::Inserted(selected) => {
            // The stand consumed an item. Tell the client's window-0 hotbar
            // slot (menu slots `36..=44` -> native `0..=8`, vanilla's
            // `InventoryMenu`) so the held count visibly drops — the same
            // server-initiated window-0 slot update vanilla broadcasts after
            // a composter click consumes one. `state_id` is `0`: this crate
            // applies a container click's own diff verbatim and never
            // validates a stale id (`apply_container_clicked`), so the
            // client adopting the value is harmless.
            let hotbar_slot = i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
            apply(conn, state, proto.encode_container_slot(0, 0, hotbar_slot, selected.as_ref())).await?;
            return Ok(());
        }
        BrewingInsertOutcome::Consumed => {
            // The stand ate the click but nothing moved (the matching slot
            // was full). No placement may follow — some ingredients are
            // themselves placeable blocks, and a full stand must not place one.
            return Ok(());
        }
        BrewingInsertOutcome::NotBrewing => {
            // Fall through to the ordinary placement logic below.
        }
    }

    // Issue #249, the missing-consumer half: a right-click on a composter
    // feeds the seven-tier fill state machine — see
    // [`apply_composter_use`]'s doc comment for the four outcomes. Anything
    // the composter itself handles returns before the placement logic; only
    // `NotComposter` (no composter, or a click vanilla would `PASS`) reaches
    // it.
    match apply_composter_use(block_entities, inventory, mobs, pos, roll) {
        ComposterUseOutcome::Consumed {
            remainder,
            block_state,
        } => {
            // Write the new fill level — only when it actually advanced; a
            // failed roll consumed the item but left the state alone.
            if let Some(block_state) = block_state {
                source.set_block(pos.x, pos.y, pos.z, &block_state);
                apply(conn, state, proto.encode_block_update(pos.x, pos.y, pos.z, &block_state)).await?;
            }
            // Tell the client's window-0 hotbar slot (menu slots `36..=44` ->
            // native `0..=8`, vanilla's `InventoryMenu`) so the held count
            // visibly drops — the same server-initiated window-0 slot update
            // vanilla broadcasts after a composter click consumes one.
            // `state_id` is `0`, as in the brewing arm above (this crate
            // applies a container diff verbatim and never validates a stale
            // id).
            let hotbar_slot = i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
            apply(conn, state, proto.encode_container_slot(0, 0, hotbar_slot, remainder.as_ref())).await?;
            return Ok(());
        }
        ComposterUseOutcome::Extracted { block_state } => {
            source.set_block(pos.x, pos.y, pos.z, &block_state);
            apply(conn, state, proto.encode_block_update(pos.x, pos.y, pos.z, &block_state)).await?;
            return Ok(());
        }
        ComposterUseOutcome::Noop => return Ok(()),
        ComposterUseOutcome::NotComposter => {
            // Fall through to the ordinary placement logic below.
        }
    }

    // Issue #329: right-clicking a bed sets the player's per-player respawn
    // point (vanilla `BedBlock.useWithoutItem` → `ServerPlayer.startSleepInBed`
    // → `setRespawnPosition`). A bed click is an *interaction*, not a
    // placement — it must not fall through to the inventory-placement logic
    // below (a bed is itself placeable, and the click target's cell may well
    // be air-adjacent). The legality gate is issue #329's own requirement
    // ("beds/anchors validated for a legal respawn spot before being
    // accepted") — see [`is_legal_bed_respawn`] for the three checks and the
    // documented monster-check remainder.
    //
    // The message is sent only when the stored point *changes* — vanilla's
    // `setRespawnPosition` gates its message on the position having moved —
    // so a re-click on the same bed is silent, and the message is a faithful
    // observable proxy for "the tracking state changed" (the client has no
    // "your respawn point is X" packet; the placement half of P2 will be the
    // next consumer). The message itself is a stand-in: vanilla's
    // `SPAWN_SET_MESSAGE` is a translatable component shown in the action bar,
    // and this crate has no localization table or action-bar encoder, so the
    // honest equivalent is a plain system-chat line.
    if is_bed_block(&source.block_state(pos.x, pos.y, pos.z)) {
        // Issue #325: a bed click registers this connection's player in the
        // night-skip vote. Vanilla's `ServerPlayer.startSleepInBed` calls
        // `sleepStatus.setSleeping` (`ServerPlayer.java`) — this arm is this
        // crate's stand-in for that call (see `crate::sleep`'s module doc for
        // the disclosed gap: bed-entry *gates* — day/night, monsters nearby,
        // already-sleeping — are unmodelled, and the 100-tick deep-sleep
        // threshold is what makes an accidental daytime click harmless).
        // Idempotent: a re-click on the same bed does not double-count.
        sleep_vote.lay_down(player_entity_id);
        if is_legal_bed_respawn(source, pos, player_pos)
            && !respawn.is_some_and(|existing| existing.pos == pos)
        {
            *respawn = Some(RespawnPoint { pos });
            apply(conn, state, proto.encode_system_chat("Respawn point set")).await?;
        }
        return Ok(());
    }

    let neighbour = relative(pos, face);
    let clicked = source.block_state(pos.x, pos.y, pos.z);
    let target = if is_air_or_fluid(&clicked) { pos } else { neighbour };
    let target_state = source.block_state(target.x, target.y, target.z);
    // Every cell the placement's neighbour fan-out rewrote (issue #465) —
    // empty unless a placement actually happened below.
    let mut changed: Vec<(BlockPos, String)> = Vec::new();
    if is_air_or_fluid(&target_state) {
        let held_item = inventory.selected_item().map(|stack| stack.item.to_string());
        // The census is the gate: it decides *whether* a placement happens at
        // all and *which* block it writes. `block_entity_for_item` no longer
        // makes that decision — it only supplies the live `BlockEntity` for
        // the six items this crate ticks, and is consulted second.
        let placed = held_item
            .as_deref()
            .and_then(|item| block_items::block_for_item(item).map(|block| (item, block)));
        if let Some((item, block_name)) = placed {
            if let Some((entity_block, entity)) = block_entity_for_item(item) {
                // The two sources must agree on the block name, or we would
                // register a furnace at a position holding some other block.
                // `lodestone-data`'s `the_block_entity_blocks_still_resolve_
                // to_themselves` asserts they do for all six today; this
                // catches a future divergence instead of silently trusting
                // the older table.
                debug_assert_eq!(
                    entity_block, block_name,
                    "block-entity table and item census disagree on {item}"
                );
                block_entities.with(|registry| registry.insert(target, entity));
            }
            // Issue #475. `placed_block_state` overrides the census's bare
            // name with a `facing=`-bearing state for the redstone directional
            // families; everything else keeps the default state.
            let state = placed_block_state(block_name, player_yaw).unwrap_or_else(|| block_name.to_string());
            source.set_block(target.x, target.y, target.z, &state);
            // Issue #465: placing a block is a mutation like any other, so it
            // owes its neighbours the same fan-out a random tick or a drained
            // scheduled tick already performs. Without this the redstone model
            // is correct but unreachable from any player action — dust placed
            // beside a powered line stays at `power=0` forever.
            let scheduled;
            (changed, scheduled) = propagate_placement(source, target);
            // Issue #465: and the delayed half, which `propagate_placement`
            // structurally cannot host — the queue those land in belongs to the
            // world tick loop. Handed over unconditionally rather than only
            // when `changed` is non-empty: the delayed families are exactly the
            // case where the fan-out rewrites *nothing* now and schedules
            // instead, so gating on a synchronous change would drop precisely
            // the placements this exists for.
            block_ticks.request_scheduled_ticks(scheduled);
        }
    }
    // `pos`/`neighbour` first (the clicked face and the placed cell, which the
    // client predicted), then every cell the fan-out actually rewrote. Deduped
    // because `target` is always one of the first two.
    let mut notify: Vec<BlockPos> = vec![pos, neighbour];
    for (p, _) in &changed {
        if !notify.contains(p) {
            notify.push(*p);
        }
    }
    for p in notify {
        let current = source.block_state(p.x, p.y, p.z);
        let directive = proto.encode_block_update(p.x, p.y, p.z, &current);
        apply(conn, state, directive).await?;
    }
    Ok(())
}

/// Runs the neighbour-update fan-out for a block a player just placed at
/// `target`, persists every resulting change back through `source`, and
/// returns them so the caller can forward them to the client (issue #465).
///
/// This is the same [`crate::random_tick::propagate_and_react`] call
/// `tick::run_tick_loop` already makes after a drained scheduled tick and
/// after a random tick mutated a block — the *third* production caller, and
/// the first one a player can trigger. Before it existed, `propagate_and_react`
/// had exactly two callers, both inside the tick loop, so the whole redstone
/// subsystem was reachable only by the accident of a random tick landing next
/// to a circuit; dust and torches are not randomly-ticking blocks, so in
/// practice it was reachable not at all.
///
/// # What this deliberately does not do
///
/// The `ScheduledTickQueue` below is **local and discarded**. Dust is
/// synchronous in vanilla (`setBlock` recomputes wire power inline, measured
/// at 0 ticks against a live 26.2 oracle), so placing dust — and placing any
/// block *beside* dust — resolves completely here. The delayed families do
/// not: a redstone torch, repeater, comparator or observer reacts by
/// *scheduling* a recheck 2 (or `2d`) ticks out, and only the tick loop owns
/// the queue those land in. Placing one of those next to a live circuit
/// therefore still does nothing until `tick.rs` grows a drain fed from here.
/// That half is a separate landing; this one is not blocked on it, and dust —
/// the case #465 is written about — is complete.
///
/// # The delayed half now travels out with the return value (issue #465)
///
/// The second element is every block tick the fan-out scheduled, with
/// `trigger_tick` holding a **relative delay** rather than an absolute tick:
/// this function has no `game_tick` to be absolute against, and the tick loop
/// that will host these entries does. `apply_use_item_on` publishes them on
/// [`BlockTickFeed`] and `tick::run_tick_loop` rebases them onto its own
/// counter.
///
/// **Carrying the schedules out, rather than asking the loop to redo the
/// fan-out at this position, is not a stylistic choice — the redo does not
/// work, and that was measured.** The originally brokered shape had the loop
/// re-run `propagate_and_react` at `target` on its next iteration, on the
/// stated premise that the two runs are idempotent because the fan-out "writes
/// only on change". They are not idempotent, and the quoted reason is exactly
/// why: the *first* run consumes the change. It settles the dust, cascades to
/// the repeater, schedules the repeater's flip into this local queue and
/// returns; the loop's second run then finds a fully-settled circuit, changes
/// nothing, cascades nowhere and never reaches the repeater at all. Measured
/// with a repeater at four delay settings: the arm with this inline call
/// finished `powered=false` and its output dust at 0, the arm without it
/// finished `powered=true` at 15 —
/// `redstone_placement_gate::the_split_between_the_synchronous_and_delayed_halves_changes_no_outcome`
/// is the gate, and it is red under the redo shape.
///
/// Changes are sent to *this* connection only, matching the existing
/// `encode_block_update` loop above rather than publishing on
/// [`BlockTickFeed`]; a second connection would not see them. That gap is
/// pre-existing for placement itself and is not widened here.
pub(crate) fn propagate_placement<S>(
    source: &S,
    target: BlockPos,
) -> (Vec<(BlockPos, String)>, Vec<ScheduledTick<String>>)
where
    S: ChunkSource,
{
    let cx = target.x.div_euclid(16);
    let cz = target.z.div_euclid(16);
    let (min_x, min_z) = (cx * 16, cz * 16);
    // Reflects the `set_block` just performed — `ChunkSource::column`'s own
    // contract is that it includes any edit already applied.
    let mut column = source.column(cx, cz);
    if target.y < column.min_y || target.y >= column.min_y + column.height {
        return (Vec::new(), Vec::new());
    }
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    // `react_at_placement`, not `propagate_and_react`: the placed block owes
    // itself a `setPlacedBy` reaction that the neighbour pass structurally
    // cannot deliver. See that function's own doc comment.
    let events = crate::random_tick::react_at_placement(
        &mut column,
        min_x,
        min_z,
        target.x,
        target.y,
        target.z,
        &mut block_ticks,
        // Zero, so every `trigger_tick` below *is* the delay — see the doc
        // comment above.
        0,
    );
    // `drain_due`, not `iter`: this queue is a `BinaryHeap` and `iter` yields in
    // unspecified order, while `drain_due` yields `DRAIN_ORDER`. The loop
    // re-`schedule`s each entry and so assigns it a fresh `sub_tick_order`, which
    // makes *this* order the one that decides tie-breaks later — so it has to be
    // deterministic. `u64::MAX` drains everything regardless of delay.
    let scheduled: Vec<ScheduledTick<String>> = block_ticks.drain_due(u64::MAX, usize::MAX);
    let changed = events
        .into_iter()
        .map(|event| {
            let (ex, ey, ez) = event.pos;
            source.set_block(ex, ey, ez, &event.to);
            (BlockPos::new(ex, ey, ez), event.to)
        })
        .collect();
    (changed, scheduled)
}

/// Per-connection difficulty + game-rule session state (issue #268).
///
/// This crate has no permission/operator model and no `GameRules` registry —
/// see [`apply_difficulty_change`]/[`apply_game_rule_changed`]'s own doc
/// comments — so this is deliberately the smallest state that lets the round
/// trip (a `ServerBound::DifficultyChanged`/`DifficultyLockChanged`/
/// `GameRuleChanged` request in, a confirmation back out) be real and
/// observable without inventing a full world-rules model. Per-connection
/// rather than shared across connections, matching `player_pos`/`vitals`/
/// `fall`'s existing precedent in [`serve_play`] — a real scope cut for
/// open-to-LAN (two connections would each hold an independent view, and
/// neither would see the other's change), documented rather than silent.
#[derive(Debug)]
struct WorldAdminState {
    difficulty: Difficulty,
    difficulty_locked: bool,
    game_rules: HashMap<String, String>,
}

impl Default for WorldAdminState {
    fn default() -> Self {
        Self {
            // Matches `LevelSettings.DEFAULT`'s difficulty
            // (`.cache/mc/26.2/src/net/minecraft/world/level/levelgen/`
            // `WorldOptions.java`'s sibling `LevelSettings` default) — a
            // fresh session's starting point before any
            // `DifficultyChanged` request rewrites it.
            difficulty: Difficulty::Normal,
            difficulty_locked: false,
            game_rules: HashMap::new(),
        }
    }
}

/// Applies a difficulty-change request (`ServerBound::DifficultyChanged`),
/// mirroring `ServerGamePacketListenerImpl::handleChangeDifficulty`
/// (`.cache/mc/26.2/src/net/minecraft/server/network/ServerGamePacketListenerImpl.java:2088-2099`)
/// minus its permission check: vanilla gates this on
/// `Permissions.COMMANDS_GAMEMASTER` **or** `isSingleplayerOwner()`, and
/// every connection this crate ever serves *is* the singleplayer owner (no
/// accounts/op model exists — the same simplification `docs/singleplayer.md`
/// already documents elsewhere for this integrated server), so the check
/// always passes here. Confirms back to the *same* connection via
/// [`ServerProtocol::encode_change_difficulty`]; vanilla instead broadcasts
/// to every player (`MinecraftServer::setDifficulty` → `PlayerList`), which
/// needs cross-connection state this crate does not share — see
/// [`WorldAdminState`]'s own doc comment.
async fn apply_difficulty_change<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    admin: &mut WorldAdminState,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    let directive = proto.encode_change_difficulty(admin.difficulty, admin.difficulty_locked);
    apply(conn, state, directive).await
}

/// Applies a game-rule change request (`ServerBound::GameRuleChanged`),
/// mirroring `ServerGamePacketListenerImpl::handleSetGameRule`
/// (`.cache/mc/26.2/src/net/minecraft/server/network/ServerGamePacketListenerImpl.java:800-816`)
/// minus its permission check (see [`apply_difficulty_change`]'s doc comment
/// for why) and minus rule-name/value validation: vanilla looks each key up
/// in `BuiltInRegistries.GAME_RULE` and parses `value` through that rule's
/// own type (`GameRule<T>::deserialize`), discarding an unknown key or an
/// unparseable value with a warning log. This crate has no `GameRules`
/// registry (see [`WorldAdminState`]'s own doc comment) — every entry is
/// stored verbatim as `(String, String)`, unvalidated. Confirms back to the
/// same connection with exactly the entries that were just set; vanilla's
/// `broadcastGameRuleChangeToOperators` instead sends one packet per changed
/// rule to every operator.
async fn apply_game_rule_changed<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    admin: &mut WorldAdminState,
    entries: Vec<(String, String)>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    for (key, value) in &entries {
        admin.game_rules.insert(key.clone(), value.clone());
    }
    let directive = proto.encode_game_rule_values(&entries);
    apply(conn, state, directive).await
}

/// Applies a `client_command` request (`ServerBound::ClientCommand`, issue
/// #270), mirroring `ServerGamePacketListenerImpl::handleClientCommand`'s
/// modellable ordinals.
///
/// # `action == 1`, `REQUEST_STATS`
///
/// Issue #338. Vanilla answers with `player.getStats().sendStats(player)`
/// (`ServerGamePacketListenerImpl.java:1910`) — a full `ClientboundAwardStatsPacket`.
/// Here the same reply is built from [`AdvancementManager::stats_snapshot`] of
/// this connection's [`crate::advancements`] store and lowered through
/// [`ServerProtocol::encode_award_stats`] (a no-op default on protocols
/// without a stats encoder). This is the *framework* reply: individual
/// statistic producers (block-break mined counters, etc.) are follow-up wiring
/// of this epic, so a fresh session typically answers an empty batch.
///
/// # `action == 0`, `PERFORM_RESPAWN`
///
/// Vanilla's full respawn (`PlayerList::respawn`) rebuilds the player entity,
/// re-teleports it to its spawn point, and resets per-player state this
/// crate does not track at all (dimension, XP, permissions, `wonGame`). What
/// *is* modelled here — [`PlayerVitals`] — is reset exactly like a fresh
/// connection's own defaults ([`PlayerVitals::respawn`]), and the result is
/// confirmed back to the client via
/// [`ServerProtocol::encode_set_health`]/[`encode_air_supply_update`], the
/// same two directives [`PlayerVitals::tick`]'s own drowning path already
/// sends — so a real client's health/air HUD actually refills on respawn,
/// not just the server's internal value. Vanilla guards respawn on
/// `player.getHealth() <= 0.0` (`return;` otherwise, ignoring `wonGame` since
/// this crate has no such concept); the identical guard is applied here — a
/// respawn request while still alive is a no-op.
///
/// # `action == 2`, `REQUEST_GAMERULE_VALUES`
///
/// Mirrors `sendGameRuleValues` minus its permission check (see
/// [`apply_difficulty_change`]'s own doc comment for why every connection
/// here is treated as the permission-holding singleplayer owner): replies
/// with every rule this connection's own [`WorldAdminState`] has ever had
/// set, via the same [`ServerProtocol::encode_game_rule_values`] confirmation
/// [`apply_game_rule_changed`] already uses. Vanilla instead enumerates the
/// full `GameRules` registry, including every rule at its default — this
/// crate has no such registry (see [`WorldAdminState`]'s own doc comment), so
/// a rule that was never explicitly set is simply absent from the reply
/// rather than reported at a registry default.
async fn apply_client_command<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    vitals: &mut PlayerVitals,
    admin: &WorldAdminState,
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
    action: i32,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    match action {
        0 if vitals.health() <= 0.0 => {
            vitals.respawn();
            apply(conn, state, proto.encode_set_health(vitals.health())).await?;
            apply(conn, state, proto.encode_air_supply_update(vitals.air_supply())).await?;
        }
        1 => {
            let snapshot = advancements.stats_snapshot(player_uuid);
            apply(conn, state, proto.encode_award_stats(&snapshot)).await?;
        }
        2 => {
            let entries: Vec<(String, String)> = admin
                .game_rules
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            apply(conn, state, proto.encode_game_rule_values(&entries)).await?;
        }
        _ => {}
    }
    Ok(())
}

/// Applies a `SET_CARRIED_ITEM` request (`ServerBound::CarriedItemChanged`),
/// mirroring `ServerGamePacketListenerImpl::handleSetCarriedItem`, which
/// writes straight into `Inventory.setSelectedSlot` and sends **no**
/// confirmation packet back — see that `ServerBound` variant's own doc
/// comment. A no-op if `slot` is already out of range (the protocol decoder
/// only ever constructs this variant with a validated slot, so this guard is
/// a second, defensive layer rather than the primary one — see
/// `PlayerInventory::set_selected_hotbar_slot`'s own doc comment for why it
/// degrades instead of panicking).
fn apply_carried_item_changed(inventory: &mut PlayerInventory, slot: u8) {
    inventory.set_selected_hotbar_slot(slot);
}

/// Applies a `SET_CREATIVE_MODE_SLOT` write (`ServerBound::CreativeModeSlotSet`,
/// issue #266) straight into [`PlayerInventory`] via the exact same menu-slot
/// table `CONTAINER_CLICK` against window 0 already uses
/// ([`PlayerInventory::apply_menu_slot_change`]) — `SET_CREATIVE_MODE_SLOT`'s
/// wire `slot` field uses the identical `InventoryMenu` numbering
/// (`ServerGamePacketListenerImpl::handleSetCreativeModeSlot`'s
/// `player.inventoryMenu.getSlot(slotNum)`), so no new mapping is needed.
/// `slot` values that table does not recognise (`0`, the crafting output; any
/// negative value, vanilla's "drop into the world" case) are silent no-ops
/// here, exactly as [`ServerBound::CreativeModeSlotSet`]'s own doc comment
/// documents — this crate has no world-drop model, and no creative/game-mode
/// state to gate on either (see that same doc comment for why vanilla's
/// `hasInfiniteMaterials()` check has nothing to mirror here).
fn apply_creative_mode_slot_set(inventory: &mut PlayerInventory, slot: i16, item: Option<ItemStack>) {
    inventory.apply_menu_slot_change(i32::from(slot), item);
}

/// Applies a `CONTAINER_CLICK` result the client already predicted locally
/// (`ServerBound::ContainerClicked`).
///
/// **Scope, stated plainly**: this does not re-run vanilla's `doClick` state
/// machine server-side. It applies the client's own predicted diff
/// (`changed_slots`) directly, either into [`PlayerInventory`] (`window_id
/// == 0`, the player's own inventory) or into the block entity backing the
/// connection's currently [`OpenContainer`] (any other window, split by
/// `crate::inventory::container_menu_slot` into "the block entity's own
/// slot" vs. "the player's standard inventory tail" — see that function's
/// own doc comment for the layout).
///
/// A click against a non-zero `window_id` that does not match the
/// connection's own tracked [`OpenContainer`] (a stale click for a window
/// that has since closed or been replaced) is decoded but dropped, not
/// misapplied to whatever happens to be open now.
///
/// This is a deliberate scope cut, not an oversight, and it is *exactly*
/// consistent with today's actual desync risk: `docs/container-clicks.md`
/// states plainly that "the client runs exactly this locally to predict the
/// result of a click before the server confirms it" and that prediction
/// already ships with **no server confirmation needed to look correct** —
/// nothing before this landing validated it server-side at all. Applying the
/// client's own diff verbatim cannot introduce a *new* desync relative to
/// that baseline (the server model becomes a mirror of what the client
/// already believes, by construction), where re-deriving `doClick`
/// server-side and getting one of its seven modes or the quick-craft drag
/// machine subtly wrong **would** — a wrong from-scratch reimplementation is
/// strictly worse than an honest passthrough here. A server-authoritative
/// `doClick` (rejecting an impossible client diff, catching a cheating
/// client) is real future work, not done by this landing; see
/// `crate::inventory`'s module doc comment.
fn apply_container_clicked(
    inventory: &mut PlayerInventory,
    block_entities: &BlockEntityHandle,
    open_container: Option<&OpenContainer>,
    window_id: i32,
    changed_slots: Vec<(i32, Option<ItemStack>)>,
) {
    if window_id == 0 {
        for (menu_slot, item) in changed_slots {
            inventory.apply_menu_slot_change(menu_slot, item);
        }
        return;
    }
    let Some(open) = open_container else {
        return;
    };
    if open.window_id != window_id {
        return;
    }
    for (menu_slot, item) in changed_slots {
        match container_menu_slot(open.container_size, menu_slot) {
            Some(ContainerMenuSlot::Own(index)) => {
                block_entities.with(|reg| {
                    if let Some(entity) = reg.get_mut(open.pos) {
                        entity.set_container_slot(index, item.clone());
                    }
                });
            }
            Some(ContainerMenuSlot::Player(native)) => {
                inventory.set_native(native, item);
            }
            None => {}
        }
    }
}

/// Resolves a `minecraft:attack` request (issue #12) against the live mob
/// simulation: runs the damage pipeline and, for a sprinting attacker, the
/// melee knockback bonus, through [`MobSim::attack`](crate::MobSim::attack).
///
/// **No reply packet is sent from here.** This mirrors the real wire shape —
/// vanilla's own `ServerboundAttackPacket` has no clientbound acknowledgement
/// — and relies on the *existing* entity-streaming pass
/// (`EntityStreamer::sync`, called immediately after
/// [`dispatch_play_packet`] returns, on every inbound packet including this
/// one) to carry the result to every connection tracking the target: a
/// knocked-back mob's new position/velocity, or its removal on a killing
/// blow, both already flow through [`MobHandle`]'s [`EntitySource`] impl once
/// `mobs` is the same handle [`crate::tick::run_tick_loop`] (issue #284) ticks
/// and publishes from. See [`MobSim::attack`](crate::MobSim::attack)'s own doc
/// comment for why `attacker_pos` (not a tracked player yaw — this crate
/// tracks no player rotation at all) stands in for
/// [`lodestone_physics::knockback::attack_direction`]'s real facing formula.
///
/// A connection with no tracked position yet (`player_pos` is `None` —
/// hasn't sent a single `move_player_pos` since join) still lands the
/// damage; only the knockback direction needs a position, so it is skipped
/// entirely in that case (`attacker_pos` defaults to the origin and
/// `knockback_power` is forced to `0.0`) rather than guessing one, the same
/// "no data yet, don't guess" gate `vitals_tick`'s own submersion check
/// already uses for a fresh session.
///
/// `sprinting` is this connection's last-known [`ServerBound::PlayerInput`]
/// sprint flag — see [`SPRINT_ATTACK_KNOCKBACK_POWER`]'s own doc comment for
/// why a non-sprinting attack's knockback power is correctly `0.0`, not a
/// bug.
fn apply_attack(mobs: &MobHandle, player_pos: Option<(f64, f64, f64)>, sprinting: bool, entity_id: i32) {
    let (attacker_pos, knockback_power) = match player_pos {
        Some((x, y, z)) => (
            Vec3::new(x, y, z),
            if sprinting {
                SPRINT_ATTACK_KNOCKBACK_POWER
            } else {
                0.0
            },
        ),
        None => (Vec3::new(0.0, 0.0, 0.0), 0.0),
    };
    mobs.with(|sim| {
        sim.attack(
            entity_id,
            attacker_pos,
            PLAYER_BARE_HAND_ATTACK_DAMAGE,
            DamageFlags::default(),
            knockback_power,
        )
    });
}

/// Decodes and applies one inbound packet once the connection is in
/// [`State::Play`]: matches a keep-alive echo against the pending challenge
/// (clearing it, so the next keep-alive tick does not mistake a live client
/// for a dead one), streams the view when the player's chunk column changed,
/// tracks the player's latest position for [`PlayerVitals`]' submersion test,
/// feeds [`FallTracker`] and applies any resulting fall damage, applies a
/// block break/placement (see [`apply_block_action`]/[`apply_use_item_on`]),
/// applies a difficulty/game-rule change (see
/// [`apply_difficulty_change`]/[`apply_game_rule_changed`]), applies a
/// respawn/game-rule-request `client_command` (see [`apply_client_command`]),
/// resizes the streamed view on a settings change (see
/// [`ViewTracker::set_view_radius`]), advances issue #270's chunk-batch
/// flow-control gate (see [`send_view_update`]), or applies a hotbar
/// selection/container click/creative-slot write against [`PlayerInventory`]
/// (see
/// [`apply_carried_item_changed`]/[`apply_container_clicked`]/[`apply_creative_mode_slot_set`]).
/// Every other packet decodes to [`ServerBound::Ignored`] in `State::Play`
/// under the current protocols (no further state transitions are modeled —
/// no dimension change yet) and is a no-op here.
/// Feeds one `on_ground` sample to the [`FallTracker`] from a movement packet
/// that carried **no** y coordinate, reusing the last position this connection
/// reported (issue #262).
///
/// Reusing the remembered y is not an approximation: `move_player_rot` and
/// `move_player_status_only` are precisely the two packets vanilla's
/// `LocalPlayer.sendPosition` picks when position did *not* change this tick,
/// so the last reported y is the current y by construction. Feeding it back
/// with the new `on_ground` is therefore the same `(y, on_ground)` pair the
/// tracker would have seen had the client sent a position packet.
///
/// Returns without touching the tracker when no position has been reported
/// yet — a status packet before the first movement packet has no y to pair
/// with, and inventing one (say, the spawn point) would fabricate a fall.
async fn fall_status_sample<T, P>(
    conn: &mut Connection<T>,
    state: &mut State,
    proto: &P,
    player_pos: &Option<(f64, f64, f64)>,
    fall: &mut FallTracker,
    vitals: &mut PlayerVitals,
    on_ground: bool,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    let Some((_, y, _)) = *player_pos else {
        return Ok(());
    };
    if let Some(raw) = fall.on_player_moved(y, on_ground)
        && vitals.apply_fall_damage(raw as f32).is_some()
    {
        apply(conn, state, proto.encode_set_health(vitals.health())).await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_play_packet<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    view_radius: i32,
    state: &mut State,
    view: &mut ViewTracker,
    pending_keep_alive: &mut Option<i64>,
    pending_break: &mut Option<BlockPos>,
    player_pos: &mut Option<(f64, f64, f64)>,
    // Issue #262. Mirrors `player_pos` exactly — updated here, read back by
    // the caller, republished to the `PlayerRegistry` so *other* connections
    // stream this player's facing. `Option` because "no angles reported yet"
    // is distinct from "facing due south"; the registry keeps its join
    // default until a packet that actually carries angles arrives.
    player_rot: &mut Option<Rotation>,
    fall: &mut FallTracker,
    vitals: &mut PlayerVitals,
    admin: &mut WorldAdminState,
    inventory: &mut PlayerInventory,
    block_entities: &BlockEntityHandle,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    next_window_id: &mut i32,
    mobs: &MobHandle,
    sprinting: &mut bool,
    awaiting_chunk_batch_ack: &mut bool,
    pending_chunk_batches: &mut VecDeque<Vec<ServerDirective>>,
    // Issues #48/#464. One parameter rather than two (a dispatch and an
    // identity) because this function already takes 24; see
    // [`CommandSession`]'s own doc comment.
    commands: &CommandSession,
    // Issue #338. This connection's advancement/statistics store and the player
    // key its progress lives under. Threaded only to reach `apply_client_command`
    //'s `REQUEST_STATS` arm, which answers with the player's current stats —
    // see that function's own doc comment.
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
    // Issue #469. Mirrors `player_pos`/`player_rot` exactly — filled here,
    // read back by the caller, republished to the `PlayerRegistry` so *other*
    // connections see it. An out-parameter rather than two more parameters (a
    // registry and this connection's username) because the caller already
    // owns both, and this function already takes 25.
    outgoing_chat: &mut Vec<String>,
    // Issue #465. Threaded through only to reach `apply_use_item_on`, which
    // needs to ask the world tick loop for a neighbour-update fan-out that
    // outlives this packet — see that function's own parameter comment.
    block_ticks: &BlockTickFeed,
    // Issue #249. This connection's composter roll source — seeded once in
    // `serve_play`, advanced once per right-click (see
    // [`apply_composter_use`]'s `roll` parameter).
    composter_rng: &mut SpawnRng,
    // Issue #335. This connection's declared channel support (register/
    // unregister interpretation happens here, in Play) and the shared registry
    // to dispatch ordinary payloads on.
    client_channels: &mut ClientChannels,
    plugin_channels: &PluginChannelRegistry,
    // Issue #329. The player's per-player respawn point, written by the bed
    // arm of `apply_use_item_on` and threaded through `serve_play`'s session
    // state. Read back by no caller yet — the placement half of P2 is the
    // next consumer (see `crate::world_spawn`'s module doc).
    respawn: &mut Option<RespawnPoint>,
    // Issue #325. The night-skip vote, fed by the two arms below — `lay_down`
    // on a bed click (`UseItemOn`), `get_up` on a wake-up (`PlayerCommand`
    // action 0). `player_entity_id` is this connection's roster key, resolved
    // once in `serve_play` (a `PlayerRegistry` ticket id where one exists,
    // `LOCAL_PLAYER_ENTITY_ID` in singleplayer) — see `serve_play`'s own
    // binding and `crate::sleep`'s module doc.
    sleep_vote: &SleepVote,
    player_entity_id: i32,
    packet_id: i32,
    payload: &[u8],
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
{
    match proto.decode(*state, packet_id, payload) {
        ServerBound::KeepAlive { id } => {
            if *pending_keep_alive == Some(id) {
                *pending_keep_alive = None;
            }
        }
        ServerBound::PlayerMoved {
            x,
            y,
            z,
            rotation,
            on_ground,
        } => {
            *player_pos = Some((x, y, z));
            // Issue #262: `move_player_pos_rot` carries angles and
            // `move_player_pos` does not, so this is `if let`, not an
            // assignment — overwriting with `None` on every straight-line
            // step would snap the avatar back to yaw 0 between turns, which
            // is a worse failure than never turning at all because it only
            // shows up while moving.
            if let Some(rotation) = rotation {
                *player_rot = Some(rotation);
            }

            // Issue #441: feed mob perception the player's position and held
            // item. This is the *producer* for `MobController::nearest_player`
            // and `::temptation` — the last two of the eight perception
            // methods that had no source at all, which is why
            // `LookAtPlayerGoal` and `TemptGoal` had a constant-false
            // `can_use` in the running game even after the seam existed.
            //
            // This arm is the right home for it rather than the tick loop:
            // `run_tick_loop` receives no player position (the gap
            // `run_mob_tick_loop`'s own doc comment already discloses for
            // `despawn_pass`), whereas this scope already holds the new
            // position, the `MobHandle` and the `PlayerInventory` together, so
            // nothing has to be threaded anywhere. `MobSim::tick` then reads it
            // on the next tick.
            //
            // **Single-player shape, stated rather than assumed:**
            // `set_players` replaces the whole list, so with two connections
            // each would clobber the other's entry. That is correct for
            // `open_in_memory_with_mobs`' single player — the only
            // configuration that has a mob tick loop at all — and a real
            // multiplayer server wants per-connection registration instead.
            // Widening it before a second player can exist would be untested
            // generality.
            //
            // Position-driven, so a perfectly stationary player eventually
            // stops refreshing this. Harmless: the value is a position, not a
            // timer, so a stale entry for a motionless player is still the
            // correct answer. The same is true of `held_item` until they move
            // after a hotbar switch.
            mobs.with(|sim| {
                sim.set_players(vec![PlayerPerception {
                    position: Vec3::new(x, y, z),
                    held_item: inventory.selected_item().map(|stack| stack.item.clone()),
                }]);
            });

            // Chunk coordinate = floor(block / 16), not truncating division —
            // `-1.0_f64 / 16.0` must floor to chunk `-1`, matching vanilla's
            // `SectionPos.blockToSectionCoord` (an arithmetic right shift).
            let cx = (x / 16.0).floor() as i32;
            let cz = (z / 16.0).floor() as i32;
            let update = view.recenter(proto, source, cx, cz).await;
            // Every recenter: log, so we can see if the server detects boundary crosses
            tracing::info!(
                "recenter: center=({cx},{cz}) batch_size={} immediate_forgets={}",
                update.batch.len(),
                update.immediate.len(),
            );
            send_view_update(conn, state, update, awaiting_chunk_batch_ack, pending_chunk_batches).await?;

            if let Some(raw) = fall.on_player_moved(y, on_ground)
                && vitals.apply_fall_damage(raw as f32).is_some()
            {
                apply(conn, state, proto.encode_set_health(vitals.health())).await?;
            }
        }
        // Issue #262. A player turning on the spot sends `move_player_rot`
        // and nothing else, so without this arm their avatar only ever
        // re-aimed on ticks where they also happened to walk.
        //
        // No view-streaming recentre here, deliberately: this packet carries
        // no position, so the chunk column cannot have changed and calling
        // `view.recenter` would re-derive the same centre from a stale
        // `player_pos` for no reason.
        ServerBound::PlayerRotated {
            yaw,
            pitch,
            on_ground,
        } => {
            *player_rot = Some(Rotation { yaw, pitch });
            fall_status_sample(conn, state, proto, player_pos, fall, vitals, on_ground).await?;
        }
        // Issue #262. Carries nothing but the flags byte, so its whole job is
        // the `on_ground` edge — which is exactly the landing sample
        // `FallTracker`'s doc comment used to disclose as unobservable,
        // because a fall that ends with no net position change in its final
        // tick reports the touchdown on *this* packet and no other.
        ServerBound::PlayerStatusOnly { on_ground } => {
            fall_status_sample(conn, state, proto, player_pos, fall, vitals, on_ground).await?;
        }
        ServerBound::BlockAction {
            action,
            pos,
            face: _,
            sequence: _,
        } => {
            apply_block_action(
                conn,
                proto,
                // `.get()`: a break/place touches one block through
                // `block_state`/`set_block`, with no batch to offload — see
                // `SourceRef::get`.
                source.get(),
                state,
                pending_break,
                block_entities,
                open_container,
                container_sync,
                action,
                pos,
            )
            .await?;
        }
        ServerBound::UseItemOn {
            pos,
            face,
            sequence: _,
        } => {
            // Issue #249: one roll per right-click, whatever block was hit —
            // vanilla's level RNG advances on plenty of unrelated draws too,
            // and the composter branch is the only consumer of this stream.
            let roll = composter_rng.next_f64();
            apply_use_item_on(
                conn,
                proto,
                // `.get()`: single-block read/write, nothing to offload.
                source.get(),
                state,
                pos,
                face,
                // Issue #329. The player's position, for the bed reach test —
                // `None` until a `PlayerMoved` packet carries one.
                player_pos.as_ref().map(|&(x, y, z)| Vec3::new(x, y, z)),
                respawn,
                // Issue #475. The placing player's yaw, so
                // `apply_use_item_on` can give directional blocks their
                // placement facing. `None` until a packet carrying angles
                // arrives — placement then uses the block's default state.
                player_rot.map(|rotation| rotation.yaw),
                inventory,
                block_entities,
                next_window_id,
                open_container,
                container_sync,
                mobs,
                roll,
                block_ticks,
                sleep_vote,
                player_entity_id,
            )
            .await?;
        }
        ServerBound::DifficultyChanged { difficulty } => {
            admin.difficulty = difficulty;
            apply_difficulty_change(conn, proto, state, admin).await?;
        }
        ServerBound::DifficultyLockChanged { locked } => {
            admin.difficulty_locked = locked;
            apply_difficulty_change(conn, proto, state, admin).await?;
        }
        ServerBound::GameRuleChanged { entries } => {
            apply_game_rule_changed(conn, proto, state, admin, entries).await?;
        }
        ServerBound::CarriedItemChanged { slot } => {
            apply_carried_item_changed(inventory, slot);
        }
        ServerBound::ContainerClicked {
            window_id,
            state_id: _,
            changed_slots,
            carried_item: _,
        } => {
            apply_container_clicked(
                inventory,
                block_entities,
                open_container.as_ref(),
                window_id,
                changed_slots,
            );
        }
        ServerBound::ContainerClosed { window_id } => {
            if open_container.as_ref().is_some_and(|open| open.window_id == window_id) {
                *open_container = None;
                *container_sync = ContainerSync::default();
            }
        }
        ServerBound::Attack { entity_id } => {
            apply_attack(mobs, *player_pos, *sprinting, entity_id);
        }
        ServerBound::PlayerInput { sprint } => {
            *sprinting = sprint;
        }
        ServerBound::CreativeModeSlotSet { slot, item } => {
            apply_creative_mode_slot_set(inventory, slot, item);
        }
        ServerBound::ClientCommand { action } => {
            apply_client_command(conn, proto, state, vitals, admin, advancements, player_uuid, action)
                .await?;
        }
        ServerBound::ClientInformationChanged { view_distance } => {
            // Clamp against the server's own configured cap (`view_radius`,
            // this connection's original `serve_connection` argument) —
            // vanilla's own server likewise never streams more than its
            // configured `view-distance` setting regardless of what a
            // client asks for. The floor is `0`, not vanilla client UI's
            // slider minimum of `2` (`Options::renderDistance`): the server
            // side has no evidence pinning that specific floor, and a floor
            // above the server's own cap would be actively wrong on a
            // connection configured with a smaller `view_radius` than that
            // (several tests in this crate use `view_radius: 0`). `.max(0)`
            // on the upper bound only guards `clamp`'s own `min <= max`
            // invariant against a caller passing a negative `view_radius`.
            let requested = i32::from(view_distance).clamp(0, view_radius.max(0));
            let update = view.set_view_radius(proto, source, requested).await;
            send_view_update(conn, state, update, awaiting_chunk_batch_ack, pending_chunk_batches).await?;
        }
        ServerBound::ChunkBatchAcknowledged { .. } => {
            *awaiting_chunk_batch_ack = false;
            tracing::info!("server: chunk batch acked, {} pending batches", pending_chunk_batches.len());
            if let Some(next) = pending_chunk_batches.pop_front() {
                tracing::info!("server: draining pending chunk batch ({} directives)", next.len());
                *awaiting_chunk_batch_ack = true;
                for directive in next {
                    apply(conn, state, directive).await?;
                }
            }
        }
        // Issues #48/#464: the wire path for commands, and the *whole* of it
        // on this side of the seam.
        //
        // This arm deliberately does no parsing, no permission check and no
        // name lookup. It cannot: the Brigadier tree, the registry and the
        // `Permissions` resource all live in `lodestone-ecs`, which this crate
        // does not depend on (see `crate::command`'s module doc for why, and
        // for the two alternatives that were rejected). What it does is the
        // one thing only it can do — attach the connection's **authenticated**
        // identity, taken from the login this connection actually performed
        // rather than from anything in the command text — and hand both to the
        // host.
        //
        // With no sink installed, `CommandDispatch::run` refuses. That
        // direction is load-bearing and is not an implementation detail: an
        // absent dispatcher must never read as blanket permission, the same
        // property `dispatch_refuses_rather_than_ungates_when_permissions_are_missing`
        // holds one layer in.
        ServerBound::ChatCommand { command } => {
            let response = commands.dispatch.run(&commands.caller, &command);
            for line in response.lines() {
                apply(conn, state, proto.encode_system_chat(line)).await?;
            }
        }
        // Issue #469. Nothing is written to the wire here: the message is
        // handed to the caller, which publishes it to the shared
        // `PlayerRegistry`, and *every* connection — this one included —
        // picks it up on its own next drain. Replying directly here instead
        // would deliver the sender's copy on a different path from everyone
        // else's, which is exactly how a broadcast ends up working for the
        // one connection a test happens to look at.
        //
        // Empty messages are dropped rather than broadcast. Vanilla rejects
        // them upstream of the packet (the client will not send one), so a
        // frame carrying one is malformed rather than meaningful.
        ServerBound::Chat { message } => {
            if !message.trim().is_empty() {
                outgoing_chat.push(message);
            }
        }
        // Issue #335. Wire-level plugin messaging, Play-phase: the
        // register/unregister control channels update this connection's
        // supported set, and any other channel is dispatched to its registered
        // handler (silently dropped when the server registered no interest —
        // vanilla's `DiscardedPayload` fallback).
        ServerBound::CustomPayload { channel, data } => {
            if !client_channels.apply_custom_payload(&channel, &data) {
                plugin_channels.dispatch(&channel, &data);
            }
        }
        // Issue #325: `PlayerCommand` action 0 is `STOP_SLEEPING` — the "wake
        // up" a client sends when the player climbs out of bed or dies. It is
        // the only ordinal the version crates surface (the others decode to
        // `Ignored`; see `ServerBound::PlayerCommand`'s own doc comment), and
        // the packet carries no player identity — the `get_up` roster key is
        // this connection's own `player_entity_id`, resolved once in
        // `serve_play` (see `crate::sleep::SleepVote` for why the wire cannot
        // supply it).
        ServerBound::PlayerCommand { action } => {
            if action == 0 {
                sleep_vote.get_up(player_entity_id);
            }
        }
        // The pre-Play phase signals, unreachable here by construction: a
        // connection in `State::Play` cannot decode a handshake, a login, or
        // (issue #277) a Status-phase status/ping request, because every
        // `ServerProtocol::decode` arm for those is gated on the state.
        ServerBound::Handshake { .. }
        | ServerBound::LoginStart { .. }
        | ServerBound::LoginAcknowledged
        | ServerBound::ConfigurationFinished
        | ServerBound::StatusRequest
        | ServerBound::PingRequest { .. }
        | ServerBound::Ignored => {}
    }
    Ok(())
}

/// Converts wall-clock elapsed time into a tick count at vanilla's normal 20
/// TPS, for the `game_time` the periodic [`ServerProtocol::encode_set_time`]
/// broadcast carries.
#[cfg(not(target_arch = "wasm32"))]
fn ticks_since(start: tokio::time::Instant) -> i64 {
    (start.elapsed().as_millis() / MILLIS_PER_TICK) as i64
}

/// Serves a connection that has just reached [`State::Play`] until the client
/// disconnects.
///
/// This is where [`serve_connection`] hands off once the join sequence and
/// initial chunk view are out: everything here runs on the connection's own
/// schedule rather than strictly in response to one inbound packet —
/// * a server-initiated keep-alive, matching vanilla's fixed 15-second
///   interval and the same-length disconnect timeout
///   (`ServerCommonPacketListenerImpl.java:35-36,118-133`; see the
///   `KEEP_ALIVE_INTERVAL` doc comment for why that is one interval, not two);
/// * a periodic time-of-day broadcast, matching vanilla's every-20-ticks
///   cadence (`MinecraftServer.java:1095-1099`; see `TIME_SYNC_INTERVAL`);
/// * view streaming (chunk-cache-center, forget, and send) whenever a
///   [`ServerBound::PlayerMoved`] packet crosses into a new chunk column
///   (`ChunkMap::move`/`updateChunkTracking`, `ChunkMap.java:1071-1120`);
///
/// all layered over the same entity-streaming pass the join sequence already
/// ran once, now repeated on every inbound packet exactly as the original
/// single-loop version did.
///
/// # Why this is a separate function, and why it forks on `wasm32`
///
/// Only this phase needs a real timer racing against the socket read, via
/// `tokio::select!` — and `tokio::time`'s timer is unavailable on `wasm32`
/// (see [`Connection::read_packet_timeout`](lodestone_net::Connection::read_packet_timeout)'s
/// own doc comment, the existing precedent for this split in this workspace).
/// The `wasm32` build below degrades to the old packet-driven-only loop
/// through the same [`dispatch_play_packet`] helper: it still answers
/// keep-alive echoes and streams the view reactively, it just never
/// *initiates* a keep-alive challenge or a periodic time broadcast, since
/// nothing can wake it when the client goes quiet. This is a real, documented
/// gap on that target, not a silent one.
///
/// # Errors
///
/// Returns [`ServerError::Net`] on a transport/codec failure, or
/// [`ServerError::KeepAliveTimeout`] if the client does not echo a challenge
/// in time (native only — see above).
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
async fn serve_play<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    entities: &E,
    view_radius: i32,
    mut state: State,
    mut streamer: EntityStreamer,
    mut player_list: PlayerListStreamer,
    // Issue #438: owned, not borrowed. This function's `Drop` is the player's
    // deregistration, so the ticket must die with the connection task — on the
    // clean-disconnect return, on every `?`, and on a task cancelled at
    // shutdown alike. Holding it by reference would put that lifetime
    // somewhere else and reintroduce the ghost-player leak the RAII exists to
    // prevent.
    player_ticket: Option<PlayerTicket>,
    mut view: ViewTracker,
    username: String,
    chunks_sent: usize,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    // Issue #324. Weather transitions published by the world tick loop's
    // `WeatherState`, drained on this same timer — see that arm's comment.
    weather: &WeatherFeed,
    // Issue #325. The night-skip vote (see `serve_connection_inner`'s
    // parameter comment). `dispatch_play_packet` records this connection's
    // player `lay_down`/`get_up` on it, and the `container_sync_tick` arm
    // feeds it the voter count from the shared `PlayerRegistry`.
    sleep_vote: &SleepVote,
    // Issue #325. Where this connection learns a night skip happened — drained
    // on `container_sync_tick` into a real `encode_set_time`, same timer as
    // the weather drain (see that arm's comment).
    sleep_feed: &SleepFeed,
    // Issues #48/#464. Owned rather than borrowed: it is built once, here at
    // the Play handoff, from *this* connection's login, and it is cheap
    // (an `Option<Arc>` plus a `Uuid` and a `String`).
    commands: CommandSession,
    // Issue #338. The connection's server-authoritative advancement/statistics
    // store, built in `serve_connection_inner` (which already sent its
    // first-packet `update_advancements` at join). Mutable because both the
    // per-packet flush below and the `REQUEST_STATS` reply in
    // `dispatch_play_packet` award into / read from it.
    mut advancements: AdvancementManager,
    // Issue #338. The player key this connection's advancement/statistic
    // progress is stored under — the same `login_uuid` that built
    // `CommandSession`'s caller, resolved the same way (a nil uuid fails
    // closed: the connection tracks nothing).
    player_uuid: uuid::Uuid,
    // Issue #326 B1: the world border, snapshotted on the vitals timer for
    // border damage (a default feed is the full-size static border today — see
    // `serve_connection_inner`'s parameter comment).
    border: &BorderFeed,
    // Issue #334. Server-initiated resource pack pushes, drained on
    // `container_sync_tick` — same timer as the block-tick/explosion/weather
    // drains below, for the same reason: a push is published by the host (not
    // by an inbound packet) and needs this connection's own timer to notice.
    resource_packs: &ResourcePackPushFeed,
    // Issue #335. The connection's declared channel support (the filter the
    // broadcast drain below applies) and the shared wire-level registry whose
    // broadcast queue that drain reads. `client_channels` is owned, not
    // borrowed: it was created here for this connection and dies with it.
    client_channels: &mut ClientChannels,
    plugin_channels: &PluginChannelRegistry,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    let mut pending_keep_alive: Option<i64> = None;
    let mut pending_break: Option<BlockPos> = None;
    let mut player_pos: Option<(f64, f64, f64)> = None;
    // Issue #262, alongside `player_pos` — see `dispatch_play_packet`'s own
    // parameter comment.
    let mut player_rot: Option<Rotation> = None;
    let mut vitals = PlayerVitals::default();
    let mut fall = FallTracker::default();
    let mut admin = WorldAdminState::default();
    let mut inventory = PlayerInventory::default();
    let mut open_container: Option<OpenContainer> = None;
    let mut container_sync = ContainerSync::default();
    // This connection's last-known `ServerBound::PlayerInput` sprint flag —
    // see `apply_attack`'s own doc comment for the one thing it feeds
    // (the melee knockback sprint bonus).
    let mut sprinting = false;
    // Vanilla's `ServerPlayer::nextContainerCounter` starts at `0` and the
    // very first open bumps it to `1` before use (`ServerPlayer.java:1330,
    // 1343`) — see [`open_container_screen`]'s own `% 100 + 1` wrap.
    let mut next_window_id: i32 = 0;
    // Issue #249. This connection's composter roll stream — see
    // `COMPOSTER_BEHAVIOR_SEED` and `dispatch_play_packet`'s parameter comment.
    let mut composter_rng = SpawnRng::new(COMPOSTER_BEHAVIOR_SEED);
    // Issue #329. This connection's per-player respawn point, written by
    // `apply_use_item_on`'s bed arm. Never read here — the placement half of
    // P2 is the next consumer (see `crate::world_spawn`'s module doc).
    let mut respawn: Option<RespawnPoint> = None;
    // Issue #325: this connection's server-side entity id — the key the
    // night-skip vote stores this player under. A `PlayerRegistry` ticket
    // carries it where a registry exists (LAN, and every `serve_play` gate);
    // singleplayer has no registry, and `LOCAL_PLAYER_ENTITY_ID` is the same
    // constant the v770 encoder uses for the local player — see that const's
    // doc comment.
    let player_entity_id =
        player_ticket.as_ref().map_or(LOCAL_PLAYER_ENTITY_ID, |t| t.entity_id());
    // Issue #270's chunk-batch flow-control gate (`ServerBound::
    // ChunkBatchAcknowledged`, see `send_view_update`'s own doc comment):
    // starts `true` because `serve_connection`'s own initial full-view dump
    // (sent just before this function was called) is itself an outstanding
    // unacknowledged batch — the first ack this loop receives is for *that*
    // batch, not a later `recenter`/`set_view_radius` one.
    let mut awaiting_chunk_batch_ack = true;
    let mut pending_chunk_batches: VecDeque<Vec<ServerDirective>> = VecDeque::new();
    // Issue #469. Filled by `dispatch_play_packet`, drained immediately after
    // it returns — it exists only to carry a message across that call.
    let mut outgoing_chat: Vec<String> = Vec::new();
    // This connection's read position in the shared chat log. Started at the
    // log's *current end* so a joining player is not replayed the backlog of
    // everything said before they arrived.
    let mut chat_cursor = entities.players().map_or(0, PlayerRegistry::chat_cursor);
    // Issue #335. This connection's read position in the shared plugin-channel
    // broadcast queue. Started at 0 — unlike chat, a *broadcast* is
    // host-published state a new connection legitimately receives: a client
    // that announces `minecraft:brand` support at join is owed the brand
    // payload that was queued before it arrived.
    let mut plugin_channel_cursor: u64 = 0;
    let mut keep_alive_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + KEEP_ALIVE_INTERVAL,
        KEEP_ALIVE_INTERVAL,
    );
    // `interval_at`, not the bare `interval` constructor: `Interval::tick`'s
    // *first* call resolves immediately for an interval built with
    // `tokio::time::interval`, which would otherwise fire a redundant
    // game-time-only broadcast in the same instant as the join-time full
    // sync `serve_connection` just sent. Anchoring the first tick a full
    // `TIME_SYNC_INTERVAL` out avoids that, and mirrors `keep_alive_tick`
    // above for the same reason.
    let mut time_sync_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + TIME_SYNC_INTERVAL,
        TIME_SYNC_INTERVAL,
    );
    // Same reasoning as `time_sync_tick`: anchored one interval out so the
    // first vitals tick does not fire in the same instant as join.
    let mut vitals_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + VITALS_TICK_INTERVAL,
        VITALS_TICK_INTERVAL,
    );
    // Same reasoning again: anchored one interval out so the first sync
    // does not fire in the same instant as join (there is nothing open yet
    // at join, so this is cosmetic here, but consistent with every other
    // timer in this function).
    let mut container_sync_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + CONTAINER_SYNC_INTERVAL,
        CONTAINER_SYNC_INTERVAL,
    );
    let play_start = tokio::time::Instant::now();
    let mut next_keep_alive_id: i64 = 0;

    loop {
        tokio::select! {
            packet = conn.read_packet() => {
                let Some((packet_id, payload)) = packet? else {
                    return Ok(ServeSummary { username, chunks_sent, inventory });
                };
                dispatch_play_packet(
                    conn,
                    proto,
                    source,
                    view_radius,
                    &mut state,
                    &mut view,
                    &mut pending_keep_alive,
                    &mut pending_break,
                    &mut player_pos,
                    &mut player_rot,
                    &mut fall,
                    &mut vitals,
                    &mut admin,
                    &mut inventory,
                    block_entities,
                    &mut open_container,
                    &mut container_sync,
                    &mut next_window_id,
                    mobs,
                    &mut sprinting,
                    &mut awaiting_chunk_batch_ack,
                    &mut pending_chunk_batches,
                    &commands,
                    &mut advancements,
                    player_uuid,
                    &mut outgoing_chat,
                    block_ticks,
                    &mut composter_rng,
                    client_channels,
                    plugin_channels,
                    &mut respawn,
                    sleep_vote,
                    player_entity_id,
                    packet_id,
                    &payload,
                )
                .await?;
                // Issue #338: drain the advancement flush for anything the
                // packet just granted. Vanilla flushes every server tick
                // (`ServerPlayer.tick()` → `advancements.flushDirty(player,
                // true)`); every advancement producer in this crate today is
                // packet-driven, so flushing here — immediately after the
                // packet was applied — is equivalent and needs no timer this
                // loop may not own. `flush_dirty` returns `None` on the
                // no-change fast path, so the common case adds one cheap
                // `is_empty` check and no packet.
                if let Some(update) = advancements.flush_dirty(player_uuid, true) {
                    apply(conn, &mut state, proto.encode_update_advancements(&update)).await?;
                }
                // Issue #469: republish this player's chat for *every*
                // connection to pick up, this one included. Same read-back
                // idiom as `player_pos`/`player_rot` below, and for the same
                // reason: the username and the registry both live here.
                //
                // With no registry — singleplayer, where `open_in_memory`
                // builds no `PlayerRegistry` at all — there is nobody else to
                // broadcast to, so the message is echoed straight back to its
                // sender. That still matches vanilla, whose broadcast loop
                // includes the sender; it is the same rule with a roster of
                // one, not a special case.
                for message in outgoing_chat.drain(..) {
                    let line = ChatLine {
                        sender: username.clone(),
                        message,
                    };
                    match entities.players() {
                        Some(registry) => registry.say(&line.sender, &line.message),
                        None => {
                            apply(conn, &mut state, proto.encode_system_chat(&line.rendered()))
                                .await?;
                        }
                    }
                }
                // Issue #438: republish this player's position for *other*
                // connections to stream. Read back from `player_pos` — which
                // `dispatch_play_packet` has just updated if the packet was a
                // `PlayerMoved` — rather than passing the ticket down into
                // that function: same information, and it keeps
                // `dispatch_play_packet`'s already-`too_many_arguments`
                // signature untouched.
                if let (Some(ticket), Some(registry), Some((x, y, z))) = (
                    player_ticket.as_ref(),
                    entities.players(),
                    player_pos,
                ) {
                    registry.set_position(ticket.entity_id(), Vec3::new(x, y, z));
                }
                // Issue #262: the same republish for facing. A separate
                // `if let` rather than a third binding in the tuple above,
                // because rotation and position arrive on different packets
                // — requiring both to be `Some` would mean a player who has
                // turned but not yet moved publishes neither.
                if let (Some(ticket), Some(registry), Some(rotation)) =
                    (player_ticket.as_ref(), entities.players(), player_rot)
                {
                    registry.set_rotation(ticket.entity_id(), rotation);
                }
                for directive in stream_pass(
                    proto,
                    entities,
                    &mut streamer,
                    &mut player_list,
                    player_ticket.as_ref(),
                ) {
                    apply(conn, &mut state, directive).await?;
                }
            }

            _ = keep_alive_tick.tick() => {
                if pending_keep_alive.is_some() {
                    // Issue #279: tell the client *why* before hanging up.
                    // Vanilla sends `Component.translatable("disconnect.timeout")`
                    // on exactly this path (`ServerCommonPacketListenerImpl
                    // .java:37,86`) — up to now we closed the socket silently and
                    // a real client showed a generic "connection lost".
                    //
                    // The write is best-effort: a peer that stopped answering
                    // keep-alives may well be gone, so a failed write must still
                    // produce `KeepAliveTimeout` rather than masking it as a
                    // transport error. That is what `let _ =` buys here, and it is
                    // the one place in this loop where dropping an error is right.
                    // Built before the `&mut state` borrow, not inline in the
                    // call: `apply` takes `&mut state` and `encode_disconnect`
                    // reads it, which the borrow checker rejects as an argument
                    // expression.
                    let directive = proto.encode_disconnect(state, &timeout_reason());
                    let _ = apply(conn, &mut state, directive).await;
                    return Err(ServerError::KeepAliveTimeout);
                }
                next_keep_alive_id += 1;
                pending_keep_alive = Some(next_keep_alive_id);
                apply(conn, &mut state, proto.encode_keep_alive(next_keep_alive_id)).await?;
            }

            _ = time_sync_tick.tick() => {
                let game_time = ticks_since(play_start);
                apply(conn, &mut state, proto.encode_set_time(game_time, None)).await?;
            }

            _ = vitals_tick.tick() => {
                // No position yet (client has not sent a single move since
                // join): nothing to test submersion against, so skip rather
                // than guess a spawn position this version-free crate does
                // not otherwise track (see `crate::vitals`'s module docs).
                if let Some((x, y, z)) = player_pos {
                    // Issue #326 B1: border damage, applied *before* the
                    // submersion test — vanilla's `LivingEntity.baseTick` runs
                    // the border `else if` ahead of the water-breath block
                    // (`LivingEntity.java:425-434`). Snapshot the border once
                    // per timer tick and ask it for the damage the tracked
                    // position attracts; `apply_border_damage` is `Some` only
                    // when the hit landed (a dead player is a no-op), and a
                    // player past the safe zone takes `max(1, floor(d*0.2))`
                    // *every* tick — the plan gate's per-tick cadence. With a
                    // default full-size border the distance is far inside and
                    // `damage_for_position` is always `None`: nothing is sent
                    // and this costs one clone + one distance scan per 50ms.
                    let border_state = border.get();
                    if let Some(damage) = border_state.damage_for_position(x, z) {
                        if vitals.apply_border_damage(damage).is_some() {
                            apply(conn, &mut state, proto.encode_set_health(vitals.health())).await?;
                        }
                    }

                    let eye_state = source.get().block_state(
                        x.floor() as i32,
                        (y + EYE_HEIGHT).floor() as i32,
                        z.floor() as i32,
                    );
                    let outcome = vitals.tick(is_water(&eye_state));
                    if let Some(air) = outcome.air_changed {
                        apply(conn, &mut state, proto.encode_air_supply_update(air)).await?;
                    }
                    if outcome.damage.is_some() {
                        apply(conn, &mut state, proto.encode_set_health(vitals.health())).await?;
                    }
                }
            }

            _ = container_sync_tick.tick() => {
                // The piece with no inbound packet driving it at all: the
                // server's unified tick loop (`crate::tick::run_tick_loop`,
                // issue #284) mutates the registry independently of any
                // connection, so this connection needs its own timer to notice — see
                // `sync_open_container`'s own doc comment.
                if let Some(open) = open_container.as_mut() {
                    let (slots, data) = container_state(block_entities, open.pos);
                    for directive in
                        sync_open_container(proto, open, &mut container_sync, slots, data)
                    {
                        apply(conn, &mut state, directive).await?;
                    }
                }
                // Issues #307/#308: the world tick loop's random ticks (grass
                // ↔ dirt, `crate::random_tick`) mutate the shared `ChunkSource`
                // independently of this connection too — same shape as the
                // block-entity registry above, so it rides the same timer
                // rather than adding a seventh one (CLAUDE.md's own caution
                // about growing the timer table). `BlockTickFeed::drain_all`
                // is single-consumer (see that type's doc comment); this is
                // the one connection that owns it for
                // `open_in_memory_with_mobs`.
                for (x, y, z, block_state) in block_ticks.drain_all() {
                    apply(conn, &mut state, proto.encode_block_update(x, y, z, &block_state)).await?;
                }
                // Issue #425: same shape again, one timer tick later —
                // `MobSim::tick` already calls `MobSim::explode` the tick a
                // creeper's fuse completes; this is what finally turns that
                // into a real `EXPLODE` packet reaching this connection.
                // `ExplosionFeed::drain_all` is single-consumer for the same
                // reason `BlockTickFeed::drain_all` is (see that type's own
                // doc comment) — safe here for the same reason: exactly one
                // connection task per feed instance under
                // `open_in_memory_with_mobs`.
                for detonation in explosions.drain_all() {
                    apply(
                        conn,
                        &mut state,
                        proto.encode_explode(detonation.centre, detonation.radius),
                    )
                    .await?;
                }
                // Issue #324: same shape again — the world tick loop's weather
                // cycle (`crate::weather::WeatherState`, ticked inside
                // `run_tick_loop`) has no packet driving it either, so this
                // connection learns of a rain flip or a level ramp only when
                // this timer drains the feed. `WeatherFeed::drain_all` is
                // single-consumer for the same reason the two drains above
                // are (see that type's own doc comment).
                for event in weather.drain_all() {
                    let (kind, value) = event.wire();
                    apply(conn, &mut state, proto.encode_game_event(kind, value)).await?;
                }
                // Issue #325: same shape again — the world tick loop's
                // night-skip vote has no packet driving it either. Two duties,
                // both here because this is the connection's only regular
                // timer:
                //
                // 1. Feed the voter count. Vanilla excludes spectators
                //    (`SleepStatus.updateSleepingPlayers`); this crate has no
                //    spectator concept, so every player in the shared
                //    `PlayerRegistry` counts. Where no registry exists
                //    (singleplayer), nothing is fed and
                //    `SleepState::sleepers_needed`'s `max(1, …)` floor yields
                //    exactly 1 — the correct single-player vote.
                if let Some(registry) = entities.players() {
                    sleep_vote.set_active(registry.len() as u32);
                }
                // 2. Learn of a skip. `SleepEvent::SkippedNight` re-anchors
                //    this client's day clock to the morning — `encode_set_time`
                //    with a `Some` day-time — exactly the broadcast vanilla's
                //    skip path sends: the world's clock jumped, so every
                //    connection must re-anchor. `SleepFeed::drain_all` is
                //    single-consumer for the same reason the drains above are
                //    (see that type's own doc comment).
                for event in sleep_feed.drain_all() {
                    match event {
                        SleepEvent::SkippedNight { game_time, morning } => {
                            apply(
                                conn,
                                &mut state,
                                proto.encode_set_time(game_time, Some(morning)),
                            )
                            .await?;
                        }
                    }
                }
                // Issue #334: same shape again — a resource pack push is
                // published by the host (a config surface on `IntegratedServer`,
                // or a future command), never by an inbound packet, so this
                // connection learns of it only when this timer drains the feed.
                // `ResourcePackPushFeed::drain_all` is single-consumer for the
                // same reason the three drains above are (see that type's own
                // doc comment).
                for push in resource_packs.drain_all() {
                    apply(conn, &mut state, proto.encode_resource_pack_push(&push)).await?;
                }
                // Issue #469: player chat, riding the same timer as the three
                // above for the same reason. Unlike them this is *not* a
                // drain-all feed — `chat_since` advances this connection's own
                // cursor over a shared append-only log, which is what lets
                // every connection read every line. A `drain_all` here would
                // deliver each message to whichever connection's timer fired
                // first and to nobody else, which is precisely the bug a
                // broadcast must not have.
                if let Some(registry) = entities.players() {
                    for line in registry.chat_since(&mut chat_cursor) {
                        apply(conn, &mut state, proto.encode_system_chat(&line.rendered()))
                            .await?;
                    }
                }
                // Issue #335: same shape as chat above — a broadcast is
                // host-published (a future #77 plugin, a command, a config
                // surface), never an inbound packet, so this connection learns
                // of it only when this timer drains the shared queue. Also like
                // chat, it is *not* a drain-all feed: every connection reads
                // every payload through its own cursor, filtered to the
                // channels this client announced. See `outbound_since`'s doc
                // comment for why a channel this client never registered is a
                // skip, not a block.
                for (channel, data) in plugin_channels.outbound_since(
                    &mut plugin_channel_cursor,
                    client_channels,
                ) {
                    apply(
                        conn,
                        &mut state,
                        proto.encode_custom_payload(&channel, &data),
                    )
                    .await?;
                }
            }
        }
    }
}

/// `wasm32` counterpart of the native [`serve_play`] above — same signature,
/// same [`dispatch_play_packet`] dispatch, but degraded to the old
/// packet-driven-only loop (no `tokio::select!`, no timers). See the native
/// definition's doc comment for why the two forked instead of sharing one
/// body with an internal `cfg`.
#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
async fn serve_play<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    entities: &E,
    view_radius: i32,
    mut state: State,
    mut streamer: EntityStreamer,
    mut player_list: PlayerListStreamer,
    // Issue #438: see the native definition's identical parameter for why the
    // ticket is owned rather than borrowed. Player streaming itself works
    // identically on this target: it is entirely packet-driven, exactly like
    // `FallTracker`, so it needs none of the timers this loop lacks.
    player_ticket: Option<PlayerTicket>,
    mut view: ViewTracker,
    username: String,
    chunks_sent: usize,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    // Same gap as `vitals`/`container_sync` below for the *outbound*
    // direction: forwarding a random tick's block change with no packet
    // driving it needs `container_sync_tick`, a `tokio::time::interval` this
    // target has none of (see this function's own doc comment). Accepted for
    // signature parity with the native definition (`serve_connection` calls
    // whichever compiles for the target) — a real, documented gap, not a
    // silent one.
    //
    // The **inbound** direction (issue #465) has no such gap and is wired: a
    // placement is packet-driven, so `dispatch_play_packet` can publish the
    // neighbour-update request here exactly as it does natively. Whether
    // anything drains it is a property of the host, not of this loop — a
    // browser singleplayer world runs `run_tick_loop` over the same feed.
    block_ticks: &BlockTickFeed,
    // Issue #425: same gap, same reason — a detonation has no packet driving
    // it either, so this target simply never surfaces one.
    _explosions: &ExplosionFeed,
    // Issue #324: same gap as `_explosions`, same reason — a weather flip or
    // level ramp has no packet driving it, so this target (which owns none of
    // `container_sync_tick`, the native loop's drain point) never surfaces
    // one. Accepted for signature parity, exactly like its two neighbours.
    _weather: &WeatherFeed,
    // Issue #325, **inbound half wired** (same as the native definition): the
    // `lay_down`/`get_up` arms in `dispatch_play_packet` are packet-driven, so
    // a bed click and a wake-up vote identically on this target through the
    // shared call below. The two timer-fed halves are gaps like `_weather`:
    // `set_active` (the voter count) and the `SkippedNight` drain both ride
    // the native loop's `container_sync_tick`, which this target owns none
    // of — so the vote's roster never reaches a passing size here, and a skip
    // would be published to a feed nobody drains. Accepted for signature
    // parity with the native definition, exactly like its neighbours.
    sleep_vote: &SleepVote,
    _sleep_feed: &SleepFeed,
    // Issues #48/#464 — **not** a gap on this target. Commands are entirely
    // packet-driven (a `chat_command` frame arrives, the sink answers, system
    // chat goes back), so the missing timers cost nothing here and this loop
    // dispatches commands identically to the native one.
    commands: CommandSession,
    // Issue #338 — **not** a gap on this target. Advancements and statistics
    // are entirely packet-driven (a criterion flips on an inbound packet, the
    // reply rides the same packet), so the missing timers cost nothing here and
    // this loop flushes and answers `REQUEST_STATS` identically to the native
    // one. Same signature and same position as the native definition's pair.
    mut advancements: AdvancementManager,
    player_uuid: uuid::Uuid,
    // Issue #326 B1: same gap as `_weather`/`_explosions`, same reason —
    // border damage is applied on the native loop's `vitals_tick` timer, which
    // this target owns none of, so a browser singleplayer world never deals
    // border damage. Accepted for signature parity with the native definition
    // (and with `serve_connection_inner`'s call, which is target-agnostic).
    _border: &BorderFeed,
    // Issue #334: same gap as `_weather`/`_explosions`, same reason — a
    // resource pack push has no packet driving it either, and the drain point
    // is the native loop's `container_sync_tick`, which this target owns none
    // of, so a browser singleplayer world never surfaces one. Accepted for
    // signature parity with the native definition.
    _resource_packs: &ResourcePackPushFeed,
    // Issue #335. The *inbound* half (register/unregister + dispatch) is
    // packet-driven, so it is wired identically on this target through the
    // shared `dispatch_play_packet` call. The *outbound* half — draining
    // `plugin_channels`'s broadcast queue — is the same gap as `_resource_packs`:
    // it rides the native loop's `container_sync_tick`, which this target owns
    // none of, so a browser singleplayer world never receives a broadcast
    // until an inbound packet happens to flow. `plugin_channels` is passed
    // through for signature parity and so the inbound dispatch reaches the
    // shared registry.
    client_channels: &mut ClientChannels,
    plugin_channels: &PluginChannelRegistry,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    let mut pending_keep_alive: Option<i64> = None;
    let mut pending_break: Option<BlockPos> = None;
    let mut sprinting = false;
    // `player_pos`/`vitals` are tracked for parity with the native loop's
    // `dispatch_play_packet` calls (shared function, shared signature), but
    // `vitals` is only ever *ticked* by the native loop's timer, which
    // `tokio::time` has none of on `wasm32`. Drowning simply does not happen
    // in a `wasm32`-served session today — a real, documented gap, not a
    // silent one (see this function's own doc comment). Fall damage
    // (`FallTracker`) is different: it is driven purely by inbound
    // `PlayerMoved` packets, not a timer, so it works identically here —
    // `vitals` still needs to exist as somewhere for `apply_fall_damage` to
    // carry health, even though nothing else fills it in on this target.
    let mut player_pos: Option<(f64, f64, f64)> = None;
    // Issue #262, alongside `player_pos` — see `dispatch_play_packet`'s own
    // parameter comment.
    let mut player_rot: Option<Rotation> = None;
    let mut vitals = PlayerVitals::default();
    let mut fall = FallTracker::default();
    let mut admin = WorldAdminState::default();
    let mut inventory = PlayerInventory::default();
    // Same gap as `vitals` above, for the same reason: `sync_open_container`
    // (the piece that pushes a furnace's own background-tick mutation to an
    // open window with no packet driving it) only ever runs off
    // `container_sync_tick`, a `tokio::time::interval` the native loop's
    // `serve_play` owns and this target has none of. A window can still be
    // *opened* and *clicked into* here (both packet-driven, and both go
    // through the shared `dispatch_play_packet` call below identically to
    // native) — only the no-click background sync is missing on `wasm32`.
    let mut open_container: Option<OpenContainer> = None;
    let mut container_sync = ContainerSync::default();
    let mut next_window_id: i32 = 0;
    // Issue #249 — see the native `serve_play`'s identical binding: the
    // composter roll stream has no timer and no wasm32 dependency, so it is
    // wired identically on this target.
    let mut composter_rng = SpawnRng::new(COMPOSTER_BEHAVIOR_SEED);
    // Issue #329 — see the native `serve_play`'s identical binding: the
    // per-player respawn point has no timer and no wasm32 dependency, so it
    // is wired identically on this target.
    let mut respawn: Option<RespawnPoint> = None;
    // Issue #325 — see the native `serve_play`'s identical binding: the
    // night-skip vote's roster key has no timer and no wasm32 dependency, so
    // it is wired identically on this target (the vote's inbound arms are the
    // only thing this target can drive).
    let player_entity_id =
        player_ticket.as_ref().map_or(LOCAL_PLAYER_ENTITY_ID, |t| t.entity_id());
    // See the native `serve_play`'s identical field for why this starts
    // `true` (the initial join dump is itself an unacknowledged batch).
    let mut awaiting_chunk_batch_ack = true;
    let mut pending_chunk_batches: VecDeque<Vec<ServerDirective>> = VecDeque::new();
    // Issue #469 — see the native loop's identical binding.
    let mut outgoing_chat: Vec<String> = Vec::new();

    while let Some((packet_id, payload)) = conn.read_packet().await? {
        dispatch_play_packet(
            conn,
            proto,
            source,
            view_radius,
            &mut state,
            &mut view,
            &mut pending_keep_alive,
            &mut pending_break,
            &mut player_pos,
            &mut player_rot,
            &mut fall,
            &mut vitals,
            &mut admin,
            &mut inventory,
            block_entities,
            &mut open_container,
            &mut container_sync,
            &mut next_window_id,
            mobs,
            &mut sprinting,
            &mut awaiting_chunk_batch_ack,
            &mut pending_chunk_batches,
            &commands,
            &mut advancements,
            player_uuid,
            &mut outgoing_chat,
            block_ticks,
            &mut composter_rng,
            client_channels,
            plugin_channels,
            &mut respawn,
            sleep_vote,
            player_entity_id,
            packet_id,
            &payload,
        )
        .await?;
        // Issue #338 — identical to the native loop, and **not** a gap on this
        // target: the advancement flush is packet-driven (a criterion flips on
        // the packet just dispatched, the reply rides this same iteration), so
        // the missing timers cost nothing here. See the native loop's comment.
        if let Some(update) = advancements.flush_dirty(player_uuid, true) {
            apply(conn, &mut state, proto.encode_update_advancements(&update)).await?;
        }
        // Issue #469, identical to the native loop's publish. The *drain*,
        // though, is a real gap on this target: it rides the native loop's
        // `container_sync_tick`, which `tokio::time` gives this target none
        // of — the same documented gap `vitals` and `sync_open_container`
        // already have here. So a `wasm32`-served connection publishes chat
        // that other connections receive, and receives none itself unless it
        // is the sole connection (the no-registry echo below). Named rather
        // than silent, exactly like its two neighbours.
        for message in outgoing_chat.drain(..) {
            let line = ChatLine {
                sender: username.clone(),
                message,
            };
            match entities.players() {
                Some(registry) => registry.say(&line.sender, &line.message),
                None => {
                    apply(conn, &mut state, proto.encode_system_chat(&line.rendered())).await?;
                }
            }
        }
        // Issue #438, identical to the native loop — see its own comment for
        // why this reads `player_pos` back instead of threading the ticket
        // through `dispatch_play_packet`.
        if let (Some(ticket), Some(registry), Some((x, y, z))) =
            (player_ticket.as_ref(), entities.players(), player_pos)
        {
            registry.set_position(ticket.entity_id(), Vec3::new(x, y, z));
        }
        // Issue #262, identical to the native loop — see its own comment.
        if let (Some(ticket), Some(registry), Some(rotation)) =
            (player_ticket.as_ref(), entities.players(), player_rot)
        {
            registry.set_rotation(ticket.entity_id(), rotation);
        }
        for directive in stream_pass(
            proto,
            entities,
            &mut streamer,
            &mut player_list,
            player_ticket.as_ref(),
        ) {
            apply(conn, &mut state, directive).await?;
        }
    }
    Ok(ServeSummary { username, chunks_sent, inventory })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brewing::BrewingStand;
    use crate::composter::{Composter, MAX_FILL_LEVEL, READY_DELAY_TICKS};
    use crate::chunk::ChunkColumn;
    use crate::furnace::{Furnace, FurnaceKind};
    use crate::protocol::MetadataField;
    use lodestone_model::{Rotation, Vec3};
    use uuid::Uuid;

    /// A protocol double whose entity encoders tag each directive with a
    /// distinct packet id and the entity id(s) involved, so a test can read the
    /// streamer's diff *decisions* straight off the returned directives. It does
    /// not implement the chunk/login half — the streamer never calls those.
    struct TagProto;

    const ADD: i32 = 1;
    const UPDATE: i32 = 2;
    const REMOVE: i32 = 3;
    const METADATA: i32 = 4;

    impl ServerProtocol for TagProto {
        fn decode(&self, _s: State, _id: i32, _p: &[u8]) -> ServerBound {
            unimplemented!("streamer never decodes")
        }
        fn login_success(&self, _u: &str, _uuid: Uuid) -> Vec<ServerDirective> {
            unimplemented!()
        }
        fn begin_configuration(&self) -> Vec<ServerDirective> {
            unimplemented!()
        }
        fn begin_play(&self, _r: i32) -> Vec<ServerDirective> {
            unimplemented!()
        }
        fn begin_chunk_batch(&self) -> ServerDirective {
            unimplemented!()
        }
        fn encode_chunk(&self, _cx: i32, _cz: i32, _c: &ChunkColumn) -> ServerDirective {
            unimplemented!()
        }
        fn end_chunk_batch(&self, _n: i32) -> ServerDirective {
            unimplemented!()
        }

        fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
            ServerDirective::Send {
                packet_id: ADD,
                payload: vec![entity.id as u8],
            }
        }
        fn encode_entity_update(
            &self,
            _prev: Option<&EntitySnapshot>,
            current: &EntitySnapshot,
        ) -> Vec<ServerDirective> {
            vec![ServerDirective::Send {
                packet_id: UPDATE,
                payload: vec![current.id as u8],
            }]
        }
        fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
            ServerDirective::Send {
                packet_id: REMOVE,
                payload: ids.iter().map(|id| *id as u8).collect(),
            }
        }

        fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
            ServerDirective::Send {
                packet_id: METADATA,
                payload: std::iter::once(entity_id as u8)
                    .chain(std::iter::once(fields.len() as u8))
                    .collect(),
            }
        }
    }

    fn snap(id: i32, x: f64) -> EntitySnapshot {
        EntitySnapshot {
            id,
            uuid: Uuid::nil(),
            entity_type: "minecraft:zombie".parse().unwrap(),
            position: Vec3::new(x, 0.0, 0.0),
            rotation: Rotation::new(0.0, 0.0),
            head_yaw: 0.0,
            velocity: Vec3::new(0.0, 0.0, 0.0),
            metadata: Vec::new(),
        }
    }

    /// Extracts `(packet_id, payload)` from a `Send` directive for assertions.
    fn sent(d: &ServerDirective) -> (i32, &[u8]) {
        match d {
            ServerDirective::Send { packet_id, payload } => (*packet_id, payload.as_slice()),
            other => panic!("expected Send, got {other:?}"),
        }
    }

    #[test]
    fn first_sync_spawns_every_entity_in_source_order() {
        let mut s = EntityStreamer::default();
        let out = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0)]);
        assert_eq!(out.len(), 2);
        assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
        assert_eq!(sent(&out[1]), (ADD, [20u8].as_slice()));
    }

    #[test]
    fn resync_with_no_change_emits_nothing() {
        let mut s = EntityStreamer::default();
        let world = [snap(10, 0.0), snap(20, 0.0)];
        let _ = s.sync(&TagProto, &world);
        let out = s.sync(&TagProto, &world);
        assert!(out.is_empty(), "unchanged world must not re-send: {out:?}");
    }

    #[test]
    fn moved_entity_emits_a_single_update() {
        let mut s = EntityStreamer::default();
        let _ = s.sync(&TagProto, &[snap(10, 0.0)]);
        let out = s.sync(&TagProto, &[snap(10, 5.0)]);
        assert_eq!(out.len(), 1);
        assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
    }

    #[test]
    fn vanished_entity_is_removed_and_removals_batch() {
        let mut s = EntityStreamer::default();
        let _ = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0), snap(30, 0.0)]);
        let out = s.sync(&TagProto, &[snap(10, 0.0)]);
        // Both 20 and 30 gone -> one batched REMOVE carrying both ids.
        assert_eq!(out.len(), 1);
        let (id, payload) = sent(&out[0]);
        assert_eq!(id, REMOVE);
        let mut ids: Vec<u8> = payload.to_vec();
        ids.sort_unstable();
        assert_eq!(ids, vec![20, 30]);
    }

    #[test]
    fn readding_a_removed_id_spawns_it_again() {
        let mut s = EntityStreamer::default();
        let _ = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0)]);
        let _ = s.sync(&TagProto, &[snap(10, 0.0)]); // 20 removed
        let out = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0)]); // 20 back
        assert_eq!(out.len(), 1);
        assert_eq!(sent(&out[0]), (ADD, [20u8].as_slice()));
    }

    /// [`snap`] with a non-empty `metadata` — issue #425's own field list,
    /// generic to any entity (not creeper-specific: `EntityStreamer::sync`
    /// treats `metadata` uniformly, so a `CreeperSwellDir`/`CreeperIgnited`
    /// pair exercises the same code path the next mob's fields will).
    fn snap_with_metadata(id: i32, x: f64, metadata: Vec<MetadataField>) -> EntitySnapshot {
        EntitySnapshot { metadata, ..snap(id, x) }
    }

    /// A spawn whose snapshot already carries non-empty metadata must send
    /// `ADD` followed by a metadata sync — vanilla's own `ServerEntity`
    /// pairing behaviour (an initial non-default metadata sync right after
    /// `ADD_ENTITY`), and the wiring this issue's report ("no swelling
    /// animation") needed and did not have before.
    #[test]
    fn spawn_with_non_empty_metadata_sends_add_then_metadata() {
        let mut s = EntityStreamer::default();
        let fields = vec![MetadataField::CreeperSwellDir(1)];
        let out = s.sync(&TagProto, &[snap_with_metadata(10, 0.0, fields)]);
        assert_eq!(out.len(), 2);
        assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
        assert_eq!(sent(&out[1]).0, METADATA);
    }

    /// Control: a spawn with *empty* metadata (every existing test's `snap`)
    /// must send only `ADD` — proves the metadata branch above is
    /// conditional, not unconditional padding on every spawn.
    #[test]
    fn spawn_with_empty_metadata_sends_only_add() {
        let mut s = EntityStreamer::default();
        let out = s.sync(&TagProto, &[snap(10, 0.0)]);
        assert_eq!(out.len(), 1);
        assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
    }

    /// A metadata-only change (position/rotation/velocity all unchanged)
    /// must still be caught — `EntitySnapshot`'s derived `PartialEq` covers
    /// `metadata`, so `Some(prev) if prev != entity` fires exactly as it
    /// would for a moved entity, and re-encodes both the (redundant, but
    /// harmless) position/rotation update and the metadata sync.
    #[test]
    fn metadata_only_change_is_caught_even_with_no_motion() {
        let mut s = EntityStreamer::default();
        let _ = s.sync(&TagProto, &[snap_with_metadata(10, 0.0, vec![MetadataField::CreeperSwellDir(-1)])]);
        let out = s.sync(
            &TagProto,
            &[snap_with_metadata(10, 0.0, vec![MetadataField::CreeperSwellDir(1)])],
        );
        assert_eq!(out.len(), 2, "expected UPDATE then METADATA, got {out:?}");
        assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
        assert_eq!(sent(&out[1]).0, METADATA);
    }

    /// Negative control for the test above: re-syncing the exact same
    /// metadata (no change at all) must emit nothing, proving the branch is
    /// a real diff and not "always resend metadata once present."
    #[test]
    fn unchanged_metadata_emits_nothing_on_resync() {
        let mut s = EntityStreamer::default();
        let snapshot = snap_with_metadata(10, 0.0, vec![MetadataField::CreeperIgnited(true)]);
        let _ = s.sync(&TagProto, &[snapshot.clone()]);
        let out = s.sync(&TagProto, &[snapshot]);
        assert!(out.is_empty(), "unchanged metadata must not re-send: {out:?}");
    }

    // -- container screens (Job 1: OPEN_SCREEN/CONTAINER_SET_CONTENT/SLOT/DATA) --

    fn stack(item: &str, count: u32) -> ItemStack {
        ItemStack::new(item.parse().expect("valid resource key"), count)
    }

    const SLOT: i32 = 20;
    const DATA: i32 = 21;

    /// A protocol double whose container encoders tag each directive with a
    /// distinct packet id, `window_id`, and `state_id`/`property` — enough
    /// for [`sync_open_container`]'s tests to read the diff *decisions* back
    /// off the returned directives without needing the real `lodestone-v770`
    /// wire encoding. Every other method is unreachable from these tests.
    struct ContainerTagProto;

    impl ServerProtocol for ContainerTagProto {
        fn decode(&self, _s: State, _id: i32, _p: &[u8]) -> ServerBound {
            unimplemented!()
        }
        fn login_success(&self, _u: &str, _uuid: Uuid) -> Vec<ServerDirective> {
            unimplemented!()
        }
        fn begin_configuration(&self) -> Vec<ServerDirective> {
            unimplemented!()
        }
        fn begin_play(&self, _r: i32) -> Vec<ServerDirective> {
            unimplemented!()
        }
        fn begin_chunk_batch(&self) -> ServerDirective {
            unimplemented!()
        }
        fn encode_chunk(&self, _cx: i32, _cz: i32, _c: &ChunkColumn) -> ServerDirective {
            unimplemented!()
        }
        fn end_chunk_batch(&self, _n: i32) -> ServerDirective {
            unimplemented!()
        }
        fn encode_container_slot(
            &self,
            window_id: i32,
            state_id: i32,
            slot: i32,
            item: Option<&ItemStack>,
        ) -> ServerDirective {
            ServerDirective::Send {
                packet_id: SLOT,
                payload: vec![
                    window_id as u8,
                    state_id as u8,
                    slot as u8,
                    item.map_or(0, |s| s.count as u8),
                ],
            }
        }
        fn encode_container_data(&self, window_id: i32, property: i32, value: i32) -> ServerDirective {
            ServerDirective::Send {
                packet_id: DATA,
                payload: vec![window_id as u8, property as u8, value as u8],
            }
        }
    }

    fn open(pos: BlockPos, container_size: usize) -> OpenContainer {
        OpenContainer {
            window_id: 7,
            pos,
            container_size,
            state_id: 0,
        }
    }

    #[test]
    fn sync_open_container_emits_nothing_when_nothing_changed() {
        let mut o = open(BlockPos::new(0, 0, 0), 3);
        let mut sync = ContainerSync {
            slots: vec![Some(stack("minecraft:coal", 1)), None, None],
            data: vec![10, 20],
        };
        let out = sync_open_container(
            &ContainerTagProto,
            &mut o,
            &mut sync,
            vec![Some(stack("minecraft:coal", 1)), None, None],
            vec![10, 20],
        );
        assert!(out.is_empty(), "unchanged container must not re-send: {out:?}");
    }

    /// The exact scenario this function exists for: a furnace's own
    /// background tick lights it (data property 0 changes) and later
    /// produces an ingot (slot 2 changes) — no click involved at all.
    #[test]
    fn sync_open_container_emits_only_the_changed_slot_and_data_entries() {
        let mut o = open(BlockPos::new(0, 0, 0), 3);
        let mut sync = ContainerSync {
            slots: vec![Some(stack("minecraft:iron_ore", 1)), Some(stack("minecraft:coal", 1)), None],
            data: vec![0, 0, 0, 200],
        };
        let out = sync_open_container(
            &ContainerTagProto,
            &mut o,
            &mut sync,
            vec![None, Some(stack("minecraft:coal", 1)), Some(stack("minecraft:iron_ingot", 1))],
            // Only index 0 (`lit_time_remaining`) changes here — index 1
            // (`lit_total_time`) is deliberately held constant so this
            // fixture isolates "exactly one data property changed" rather
            // than also exercising two simultaneous data changes (a real
            // ignition tick does change both at once, but that is not what
            // this particular test is asserting).
            vec![190, 0, 0, 200],
        );
        // Slot 0 (iron ore consumed) and slot 2 (ingot produced) changed;
        // slot 1 (fuel) did not.
        let ServerDirective::Send { packet_id, payload } = &out[0] else {
            panic!("expected Send");
        };
        assert_eq!(*packet_id, SLOT);
        assert_eq!(payload[2], 0, "slot index 0 changed first");
        let ServerDirective::Send { packet_id, payload } = &out[1] else {
            panic!("expected Send");
        };
        assert_eq!(*packet_id, SLOT);
        assert_eq!(payload[2], 2, "slot index 2 changed second");
        // Data property 0 (lit_time_remaining) changed.
        let ServerDirective::Send { packet_id, payload } = &out[2] else {
            panic!("expected Send");
        };
        assert_eq!(*packet_id, DATA);
        assert_eq!(payload[1], 0, "property index 0 changed");
        assert_eq!(out.len(), 3);
        // The sync's own bookkeeping must now hold the new values, so the
        // *next* call diffs against these, not the stale ones.
        assert_eq!(sync.slots[2], Some(stack("minecraft:iron_ingot", 1)));
        assert_eq!(sync.data[0], 190);
    }

    /// **Control**: every slot/data send must bump `state_id` (vanilla's
    /// `incrementStateId`), and a data-only change must bump it **zero**
    /// times — proving the two encoders are not accidentally sharing one
    /// counter increment.
    #[test]
    fn sync_open_container_bumps_state_id_only_for_slot_sends() {
        let mut o = open(BlockPos::new(0, 0, 0), 1);
        let mut sync = ContainerSync {
            slots: vec![None],
            data: vec![0],
        };
        assert_eq!(o.state_id, 0);
        let _ = sync_open_container(
            &ContainerTagProto,
            &mut o,
            &mut sync,
            vec![None],
            vec![1], // data-only change
        );
        assert_eq!(o.state_id, 0, "a data-only change must not bump state_id");

        let _ = sync_open_container(
            &ContainerTagProto,
            &mut o,
            &mut sync,
            vec![Some(stack("minecraft:coal", 1))], // slot change
            vec![1],
        );
        assert_eq!(o.state_id, 1, "a slot change must bump state_id exactly once");
    }

    #[test]
    fn container_clicked_against_window_zero_applies_to_player_inventory() {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        apply_container_clicked(
            &mut inventory,
            &block_entities,
            None,
            0,
            vec![(9, Some(stack("minecraft:stone", 1)))],
        );
        assert_eq!(inventory.native(9), Some(&stack("minecraft:stone", 1)));
    }

    /// A click against the connection's *open* non-zero window splits by
    /// [`container_menu_slot`]: a low menu index lands in the block entity's
    /// own slot, a higher one lands in the player's standard inventory tail
    /// — both through the *same* click, proving the split is real rather
    /// than one arm being untested.
    #[test]
    fn container_clicked_against_an_open_window_splits_own_slot_from_player_tail() {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        let pos = BlockPos::new(1, 2, 3);
        block_entities.with(|reg| {
            reg.insert(pos, BlockEntity::Furnace(Furnace::new(FurnaceKind::Furnace)));
        });
        let open = open(pos, 3);

        apply_container_clicked(
            &mut inventory,
            &block_entities,
            Some(&open),
            7,
            vec![
                (1, Some(stack("minecraft:coal", 1))),  // furnace's own fuel slot
                (3, Some(stack("minecraft:stone", 1))), // menu slot 3 -> player native 9
            ],
        );

        let furnace_fuel = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::Furnace(f)) => f.fuel().cloned(),
            _ => None,
        });
        assert_eq!(furnace_fuel, Some(stack("minecraft:coal", 1)));
        assert_eq!(inventory.native(9), Some(&stack("minecraft:stone", 1)));
    }

    /// **Control**: a click carrying the *wrong* (stale) window id must not
    /// mutate anything — the guard that stops a click for an already-closed
    /// or already-replaced window from landing on whatever is open now.
    #[test]
    fn container_clicked_against_a_stale_window_id_is_dropped() {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        let pos = BlockPos::new(1, 2, 3);
        block_entities.with(|reg| {
            reg.insert(pos, BlockEntity::Furnace(Furnace::new(FurnaceKind::Furnace)));
        });
        let open = open(pos, 3); // window_id 7

        apply_container_clicked(
            &mut inventory,
            &block_entities,
            Some(&open),
            8, // stale/mismatched window id
            vec![(0, Some(stack("minecraft:coal", 1)))],
        );

        let furnace_input = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::Furnace(f)) => f.input().cloned(),
            _ => None,
        });
        assert_eq!(furnace_input, None, "a stale window id must not mutate the block entity");
    }

    // -- brewing stand interaction (issue #252) --

    /// A brewing stand registered at `pos`, and a player inventory whose
    /// selected hotbar slot (0) holds `held`.
    fn brew_scene(held: Option<ItemStack>) -> (BlockEntityHandle, PlayerInventory, BlockPos) {
        let block_entities = BlockEntityHandle::new();
        let pos = BlockPos::new(4, 64, 4);
        block_entities.with(|reg| reg.insert(pos, BlockEntity::BrewingStand(BrewingStand::new())));
        let mut inventory = PlayerInventory::new();
        inventory.set_native(0, held);
        (block_entities, inventory, pos)
    }

    /// The stand's ingredient slot as owned `(item, count)`, read back through
    /// the registry.
    fn ingredient_of(block_entities: &BlockEntityHandle, pos: BlockPos) -> Option<(String, u32)> {
        block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::BrewingStand(stand)) => {
                stand.ingredient().map(|(item, count)| (item.to_string(), count))
            }
            _ => None,
        })
    }

    /// A water bottle lands in bottle slot 0 and the single held item is fully
    /// consumed — the basic insert that makes `set_bottle` reachable at all.
    #[test]
    fn right_click_puts_a_water_bottle_in_the_first_empty_bottle_slot_and_consumes_it() {
        let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:water_bottle", 1)));

        let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

        assert_eq!(
            outcome,
            BrewingInsertOutcome::Inserted(None),
            "a single bottle is fully consumed"
        );
        assert_eq!(inventory.native(0), None, "the selected slot is empty after the click");
        let bottle = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::BrewingStand(stand)) => stand.bottle(0).cloned(),
            _ => None,
        });
        assert_eq!(
            bottle,
            Some(Bottle::new(BottleKind::Potion, "minecraft:water")),
            "the water bottle must land in bottle slot 0"
        );
    }

    /// Blaze powder lands in the fuel slot and one of a multi-count stack is
    /// consumed, leaving the remainder in hand.
    #[test]
    fn right_click_puts_blaze_powder_in_the_fuel_slot_and_consumes_one() {
        let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:blaze_powder", 3)));

        let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

        assert_eq!(outcome, BrewingInsertOutcome::Inserted(Some(stack("minecraft:blaze_powder", 2))));
        let fuel = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::BrewingStand(stand)) => {
                stand.fuel_item().map(|(item, count)| (item.to_string(), count))
            }
            _ => None,
        });
        assert_eq!(fuel, Some(("minecraft:blaze_powder".to_string(), 1)));
    }

    /// **Control**: blaze powder is *also* a potion ingredient (strength,
    /// `brewing.rs`'s `potion_mix`), so this proves the fuel routing wins over
    /// the ingredient routing — the item lands only in the fuel slot.
    #[test]
    fn blaze_powder_routes_to_fuel_not_ingredient_even_though_it_is_both() {
        let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:blaze_powder", 1)));

        let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

        assert_eq!(outcome, BrewingInsertOutcome::Inserted(None));
        let (fuel, ingredient) = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::BrewingStand(stand)) => {
                (stand.fuel_item().is_some(), stand.ingredient().is_some())
            }
            _ => (false, false),
        });
        assert!(fuel, "blaze powder must land in the fuel slot");
        assert!(!ingredient, "it must not also land in the ingredient slot");
    }

    /// An ingredient lands in the ingredient slot, and a second click with the
    /// same item **merges** into the existing stack rather than starting a new
    /// one — one consumed from the hand each time.
    #[test]
    fn right_click_puts_an_ingredient_in_the_ingredient_slot_and_a_second_click_merges() {
        let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:nether_wart", 2)));

        let first = insert_into_brewing_stand(&block_entities, &mut inventory, pos);
        assert_eq!(first, BrewingInsertOutcome::Inserted(Some(stack("minecraft:nether_wart", 1))));

        let second = insert_into_brewing_stand(&block_entities, &mut inventory, pos);
        assert_eq!(second, BrewingInsertOutcome::Inserted(None), "the second of two is fully consumed");

        assert_eq!(
            ingredient_of(&block_entities, pos),
            Some(("minecraft:nether_wart".to_string(), 2)),
            "both clicks must merge into one stack of two"
        );
    }

    /// A held item that belongs to no brewing-stand slot falls through without
    /// consuming anything and without touching the stand — the caller's cue to
    /// try ordinary placement.
    #[test]
    fn a_non_brewing_item_falls_through_without_touching_anything() {
        let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:diamond", 1)));

        let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

        assert_eq!(outcome, BrewingInsertOutcome::NotBrewing);
        assert_eq!(
            inventory.native(0),
            Some(&stack("minecraft:diamond", 1)),
            "the item must stay in hand"
        );
        let empty_stand = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::BrewingStand(stand)) => {
                stand.bottle(0).is_none() && stand.ingredient().is_none() && stand.fuel_item().is_none()
            }
            _ => false,
        });
        assert!(empty_stand, "nothing may land in the stand");
    }

    /// **Control**: a valid bottle with all three bottle slots full is consumed
    /// without placing anything — the `Consumed` distinction exists because
    /// some ingredients (`minecraft:stone`, `slime_block`, `cobweb`) are
    /// themselves placeable blocks, and a full stand must never fall through to
    /// placement and place one.
    #[test]
    fn a_valid_bottle_with_all_three_slots_full_is_consumed_without_placing() {
        let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:water_bottle", 1)));
        block_entities.with(|reg| {
            if let Some(BlockEntity::BrewingStand(stand)) = reg.get_mut(pos) {
                for slot in 0..3 {
                    stand.set_bottle(slot, Some(Bottle::new(BottleKind::Potion, "minecraft:awkward")));
                }
            }
        });

        let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

        assert_eq!(outcome, BrewingInsertOutcome::Consumed);
        assert_eq!(
            inventory.native(0),
            Some(&stack("minecraft:water_bottle", 1)),
            "nothing may be consumed from a full stand"
        );
    }

    /// A different ingredient already in the ingredient slot is not silently
    /// overwritten — the click is consumed and the original stack survives.
    #[test]
    fn a_different_ingredient_does_not_overwrite_the_one_already_in_the_slot() {
        let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:redstone", 1)));
        block_entities.with(|reg| {
            if let Some(BlockEntity::BrewingStand(stand)) = reg.get_mut(pos) {
                stand.set_ingredient(Some(("minecraft:nether_wart".into(), 1)));
            }
        });

        let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

        assert_eq!(outcome, BrewingInsertOutcome::Consumed);
        assert_eq!(inventory.native(0), Some(&stack("minecraft:redstone", 1)), "nothing may be consumed");
        assert_eq!(
            ingredient_of(&block_entities, pos),
            Some(("minecraft:nether_wart".to_string(), 1)),
            "the original ingredient is untouched"
        );
    }

    /// **Control**: a `minecraft:potion`/`splash_potion`/`lingering_potion`
    /// stack's actual potion lives in an unmodeled `potion_contents` component
    /// (see [`bottle_from_item`]'s doc comment), so it is rejected rather than
    /// guessed — inserting it with a wrong potion would let the mix table brew
    /// the wrong thing from it.
    #[test]
    fn an_unmodelable_potion_item_is_rejected_not_guessed() {
        let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:potion", 1)));

        let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

        assert_eq!(outcome, BrewingInsertOutcome::NotBrewing);
        assert_eq!(inventory.native(0), Some(&stack("minecraft:potion", 1)), "the potion must stay in hand");
        let no_bottle = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::BrewingStand(stand)) => stand.bottle(0).is_none(),
            _ => false,
        });
        assert!(no_bottle, "no bottle may be fabricated from an unmodelable potion");
    }

    /// **Control**: an ingredient stack already at the 64 cap is not grown past
    /// it — the click is consumed without moving anything.
    #[test]
    fn an_ingredient_stack_at_the_cap_is_not_grown_further() {
        let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:nether_wart", 1)));
        block_entities.with(|reg| {
            if let Some(BlockEntity::BrewingStand(stand)) = reg.get_mut(pos) {
                stand.set_ingredient(Some(("minecraft:nether_wart".into(), BREWING_STACK_CAP)));
            }
        });

        let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

        assert_eq!(outcome, BrewingInsertOutcome::Consumed);
        assert_eq!(
            inventory.native(0),
            Some(&stack("minecraft:nether_wart", 1)),
            "nothing may be consumed when the slot is full"
        );
        assert_eq!(
            ingredient_of(&block_entities, pos),
            Some(("minecraft:nether_wart".to_string(), BREWING_STACK_CAP)),
            "the full stack is untouched"
        );
    }

    /// A position holding no brewing stand is not a brewing interaction at all,
    /// regardless of the held item.
    #[test]
    fn a_position_without_a_brewing_stand_is_not_a_brewing_interaction() {
        let block_entities = BlockEntityHandle::new();
        let mut inventory = PlayerInventory::new();
        inventory.set_native(0, Some(stack("minecraft:nether_wart", 1)));

        let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, BlockPos::new(9, 9, 9));

        assert_eq!(outcome, BrewingInsertOutcome::NotBrewing);
        assert_eq!(inventory.native(0), Some(&stack("minecraft:nether_wart", 1)));
    }

    // -- the composter interaction (issue #249) --

    /// A composter at `pos`, and a player inventory whose selected hotbar slot
    /// (0) holds `held`. `MobHandle::default()` is an empty sim, so the first
    /// `spawn_item` in a test is entity id 1 (its `next_id` starts at 1 — see
    /// `MobSim::new`).
    fn composter_scene(
        composter: Composter,
        held: Option<ItemStack>,
    ) -> (BlockEntityHandle, PlayerInventory, BlockPos, MobHandle) {
        let block_entities = BlockEntityHandle::new();
        let pos = BlockPos::new(4, 64, 4);
        block_entities.with(|reg| reg.insert(pos, BlockEntity::Composter(composter)));
        let mut inventory = PlayerInventory::new();
        inventory.set_native(0, held);
        (block_entities, inventory, pos, MobHandle::default())
    }

    /// The composter's fill level, read back through the registry.
    fn composter_level(block_entities: &BlockEntityHandle, pos: BlockPos) -> u8 {
        block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::Composter(composter)) => composter.level(),
            _ => u8::MAX,
        })
    }

    /// A right-click with a compostable item consumes one from the hand and
    /// raises the fill level — the wiring that makes `Composter::insert`
    /// reachable at all.
    #[test]
    fn right_click_consumes_one_compostable_and_raises_the_level() {
        let (block_entities, mut inventory, pos, mobs) =
            composter_scene(Composter::new(), Some(stack("minecraft:oak_leaves", 3)));

        // oak_leaves chance is 0.3; roll 0.0 always beats it (and level 0
        // always advances regardless of roll — the documented special case).
        let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

        assert_eq!(
            outcome,
            ComposterUseOutcome::Consumed {
                remainder: Some(stack("minecraft:oak_leaves", 2)),
                block_state: Some("minecraft:composter[level=1]".to_string()),
            }
        );
        assert_eq!(composter_level(&block_entities, pos), 1);
    }

    /// A single compostable item in hand is fully consumed, emptying the slot.
    #[test]
    fn right_click_fully_consumes_a_single_item() {
        let (block_entities, mut inventory, pos, mobs) =
            composter_scene(Composter::new(), Some(stack("minecraft:wheat", 1)));

        let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

        assert_eq!(
            outcome,
            ComposterUseOutcome::Consumed {
                remainder: None,
                block_state: Some("minecraft:composter[level=1]".to_string()),
            }
        );
        assert_eq!(inventory.native(0), None, "the selected slot is empty after the click");
    }

    /// **Control**: a failed roll still consumes the item (vanilla consumes on
    /// every accepted insert, `ComposterBlock.java:263`) but leaves the level —
    /// and therefore the block state — unchanged.
    #[test]
    fn a_failed_roll_still_consumes_the_item_but_keeps_the_state() {
        let (block_entities, mut inventory, pos, mobs) =
            composter_scene(Composter::restore(1, None), Some(stack("minecraft:oak_leaves", 2)));

        // oak_leaves chance is 0.3; a roll of 0.9 fails away from level 0.
        let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.9);

        assert_eq!(
            outcome,
            ComposterUseOutcome::Consumed {
                remainder: Some(stack("minecraft:oak_leaves", 1)),
                block_state: None,
            }
        );
        assert_eq!(composter_level(&block_entities, pos), 1);
    }

    /// A non-compostable held item falls through without consuming anything or
    /// touching the composter — the caller's cue to try ordinary placement
    /// (vanilla `super.useItemOn`).
    #[test]
    fn a_non_compostable_item_falls_through_without_touching_anything() {
        let (block_entities, mut inventory, pos, mobs) =
            composter_scene(Composter::new(), Some(stack("minecraft:diamond", 1)));

        let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

        assert_eq!(outcome, ComposterUseOutcome::NotComposter);
        assert_eq!(inventory.native(0), Some(&stack("minecraft:diamond", 1)));
        assert_eq!(composter_level(&block_entities, pos), 0);
    }

    /// An empty hand on a not-yet-ready composter falls through too (vanilla
    /// `useWithoutItem` PASSes below level 8) — you can still place a block on
    /// top of a partially filled composter.
    #[test]
    fn an_empty_hand_on_a_not_ready_composter_falls_through() {
        let (block_entities, mut inventory, pos, mobs) =
            composter_scene(Composter::restore(3, None), None);

        let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

        assert_eq!(outcome, ComposterUseOutcome::NotComposter);
        assert_eq!(composter_level(&block_entities, pos), 3);
    }

    /// A full (level 7, waiting on its scheduled tick) composter consumes the
    /// click without touching the hand — vanilla `useItemOn` returns SUCCESS
    /// at `fillLevel == 7` with nothing to add (`ComposterBlock.java:257-259`).
    #[test]
    fn level_seven_consumes_the_click_without_touching_the_hand() {
        let mut composter = Composter::new();
        for _ in 0..MAX_FILL_LEVEL {
            assert!(matches!(
                composter.insert("minecraft:cake", 0.0),
                InsertOutcome::Consumed {
                    level_increased: true
                }
            ));
        }
        let (block_entities, mut inventory, pos, mobs) =
            composter_scene(composter, Some(stack("minecraft:cake", 2)));

        let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

        assert_eq!(outcome, ComposterUseOutcome::Noop);
        assert_eq!(
            inventory.native(0),
            Some(&stack("minecraft:cake", 2)),
            "the hand must be untouched"
        );
        assert_eq!(composter_level(&block_entities, pos), MAX_FILL_LEVEL);
    }

    /// A ready composter (level 8) with an empty hand yields one bone-meal item
    /// entity just above the block and resets to level 0 — the extraction half
    /// of the interaction (`extractProduce`).
    #[test]
    fn extracting_a_ready_composter_spawns_bone_meal_and_resets() {
        let mut composter = Composter::new();
        for _ in 0..MAX_FILL_LEVEL {
            composter.insert("minecraft:cake", 0.0);
        }
        for _ in 0..READY_DELAY_TICKS {
            composter.tick();
        }
        assert!(composter.is_ready());
        let (block_entities, mut inventory, pos, mobs) = composter_scene(composter, None);

        let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

        assert_eq!(
            outcome,
            ComposterUseOutcome::Extracted {
                block_state: "minecraft:composter[level=0]".to_string(),
            }
        );
        assert_eq!(composter_level(&block_entities, pos), 0);
        assert_eq!(
            mobs.with(|sim| sim.item_count()),
            1,
            "exactly one bone-meal item entity must spawn"
        );
        // The first spawn in a fresh `MobSim` is id 1 (its `next_id` starts at
        // 1), and it must land where vanilla's
        // `atLowerCornerWithOffset(pos, 0.5, 1.01, 0.5)` puts it.
        assert_eq!(
            mobs.with(|sim| sim.item_position(1)),
            Some(Vec3::new(4.5, 65.01, 4.5)),
            "the bone meal must spawn just above the composter"
        );
    }

    /// **Control**: extraction reaches the player even with a compostable item
    /// in hand — the item offer fails `fillLevel < 8` (returns `NotAccepting`)
    /// and the `useWithoutItem` half extracts without consuming the hand.
    #[test]
    fn extracting_a_ready_composter_works_even_with_an_item_in_hand() {
        let mut composter = Composter::new();
        for _ in 0..MAX_FILL_LEVEL {
            composter.insert("minecraft:cake", 0.0);
        }
        for _ in 0..READY_DELAY_TICKS {
            composter.tick();
        }
        let (block_entities, mut inventory, pos, mobs) =
            composter_scene(composter, Some(stack("minecraft:cake", 2)));

        let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

        assert_eq!(
            outcome,
            ComposterUseOutcome::Extracted {
                block_state: "minecraft:composter[level=0]".to_string(),
            }
        );
        assert_eq!(
            inventory.native(0),
            Some(&stack("minecraft:cake", 2)),
            "extraction must not consume the hand"
        );
        assert_eq!(mobs.with(|sim| sim.item_count()), 1);
    }

    /// **Control**: a non-compostable item on a *ready* composter also extracts
    /// — vanilla's item offer fails the `COMPOSTABLES.containsKey` guard and
    /// the `useWithoutItem` half runs, without consuming the hand.
    #[test]
    fn extracting_a_ready_composter_works_for_a_non_compostable_item_too() {
        let mut composter = Composter::new();
        for _ in 0..MAX_FILL_LEVEL {
            composter.insert("minecraft:cake", 0.0);
        }
        for _ in 0..READY_DELAY_TICKS {
            composter.tick();
        }
        let (block_entities, mut inventory, pos, mobs) =
            composter_scene(composter, Some(stack("minecraft:diamond", 1)));

        let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

        assert_eq!(
            outcome,
            ComposterUseOutcome::Extracted {
                block_state: "minecraft:composter[level=0]".to_string(),
            }
        );
        assert_eq!(
            inventory.native(0),
            Some(&stack("minecraft:diamond", 1)),
            "the non-compostable item must stay in hand"
        );
        assert_eq!(mobs.with(|sim| sim.item_count()), 1);
    }

    /// A position holding no composter is not a composter interaction at all,
    /// regardless of the held item.
    #[test]
    fn a_position_without_a_composter_is_not_a_composter_interaction() {
        let block_entities = BlockEntityHandle::new();
        let mut inventory = PlayerInventory::new();
        inventory.set_native(0, Some(stack("minecraft:oak_leaves", 1)));

        let outcome = apply_composter_use(
            &block_entities,
            &mut inventory,
            &MobHandle::default(),
            BlockPos::new(9, 9, 9),
            0.0,
        );

        assert_eq!(outcome, ComposterUseOutcome::NotComposter);
        assert_eq!(inventory.native(0), Some(&stack("minecraft:oak_leaves", 1)));
    }

    /// [`join_view_rings`]'s shape, at the three inputs that matter: the shell's
    /// own radius, the degenerate 0, and a negative one.
    ///
    /// Ring sizes are `1, 8, 16, …, 8r` and must sum to `(2r+1)²` with no
    /// coordinate repeated — a ring walk that double-counted a corner or skipped
    /// an edge would still be non-decreasing in distance, so the end-to-end gate
    /// in `tests/serve_play.rs` checks set equality and this checks the counts.
    #[test]
    fn join_view_rings_partitions_the_square_exactly() {
        let rings = join_view_rings(9);
        assert_eq!(rings.len(), 10, "radius 9 has rings 0..=9");
        assert_eq!(rings[0], vec![(0, 0)], "ring 0 is the player's own column");
        for (r, ring) in rings.iter().enumerate() {
            let expected = if r == 0 { 1 } else { 8 * r };
            assert_eq!(ring.len(), expected, "ring {r} must hold {expected} columns");
            for &(dx, dz) in ring {
                assert_eq!(
                    dx.abs().max(dz.abs()) as usize,
                    r,
                    "({dx}, {dz}) is not on ring {r}"
                );
            }
        }
        let flat: Vec<(i32, i32)> = rings.iter().flatten().copied().collect();
        let unique: HashSet<(i32, i32)> = flat.iter().copied().collect();
        assert_eq!(flat.len(), 361, "the rings must sum to (2*9+1)^2");
        assert_eq!(unique.len(), flat.len(), "no column may appear on two rings");
    }

    /// Radius 0 is one ring holding one column — the configuration several tests
    /// in this crate join with.
    #[test]
    fn join_view_rings_at_radius_zero_is_a_single_column() {
        assert_eq!(join_view_rings(0), vec![vec![(0, 0)]]);
    }

    /// **A negative radius must yield no rings at all**, matching the raster walk
    /// this replaced: `(-r..=r)` is an empty range for `r < 0`, so a negative
    /// radius sent zero chunks. `view_radius.max(0)` would send one, and
    /// `ViewTracker::new` would still record an empty loaded set for the same
    /// input — the tracker and the wire disagreeing about a column the client
    /// actually has. Nothing produces a negative radius today, which is why this
    /// needs a test rather than a reading.
    #[test]
    fn join_view_rings_at_a_negative_radius_is_empty() {
        assert!(join_view_rings(-1).is_empty());
        assert!(join_view_rings(i32::MIN).is_empty());
    }

    /// The yaw → horizontal-facing map is vanilla's `Direction.fromYRot`
    /// (`Direction.java:291-293`): yaw 0 = south, 90 = west, ±180 = north,
    /// -90 = east, split at the 45° midpoints (the value at which
    /// `floor(yaw / 90 + 0.5) & 3` rolls over). This is the facing a placed
    /// diode then inverts so the block faces the player (issue #475).
    #[test]
    fn horizontal_look_direction_matches_vanilla_from_y_rot() {
        assert_eq!(horizontal_look_direction(0.0), Direction::South);
        assert_eq!(horizontal_look_direction(90.0), Direction::West);
        assert_eq!(horizontal_look_direction(180.0), Direction::North);
        assert_eq!(horizontal_look_direction(-90.0), Direction::East);
        // The 45°/135°/225°/315° midpoints land exactly as the bit-mask's
        // `floor` does.
        assert_eq!(horizontal_look_direction(44.0), Direction::South);
        assert_eq!(horizontal_look_direction(45.0), Direction::West);
        assert_eq!(horizontal_look_direction(135.0), Direction::North);
        assert_eq!(horizontal_look_direction(225.0), Direction::East);
        assert_eq!(horizontal_look_direction(315.0), Direction::South);
        assert_eq!(horizontal_look_direction(-45.0), Direction::South);
        assert_eq!(horizontal_look_direction(-135.0), Direction::East);
        assert_eq!(horizontal_look_direction(-225.0), Direction::North);
        assert_eq!(horizontal_look_direction(-315.0), Direction::West);
        // Wraps around rather than clamping at ±180.
        assert_eq!(horizontal_look_direction(450.0), Direction::West);
        assert_eq!(horizontal_look_direction(-450.0), Direction::East);
    }

    /// `placed_block_state` gives the three redstone directional families a
    /// yaw-derived facing and leaves every other block alone. The observer is
    /// deliberately **not** inverted: `ObserverBlock.getStateForPlacement`
    /// applies `.getOpposite()` twice (`ObserverBlock.java:133-136`), so it
    /// watches in the player's look direction — unlike the diodes' single
    /// inversion (`DiodeBlock.java:155-158`), which makes them face the player.
    #[test]
    fn placed_block_state_faces_diodes_at_the_player_and_observers_with_the_player() {
        // Looking north (yaw 180): a repeater and comparator face the player —
        // south — while an observer watches north.
        assert_eq!(
            placed_block_state("minecraft:repeater", Some(180.0)),
            Some("minecraft:repeater[facing=south,delay=1,locked=false,powered=false]".to_string())
        );
        assert_eq!(
            placed_block_state("minecraft:comparator", Some(180.0)),
            Some("minecraft:comparator[facing=south,mode=compare,powered=false,output=0]".to_string())
        );
        assert_eq!(
            placed_block_state("minecraft:observer", Some(180.0)),
            Some("minecraft:observer[facing=north,powered=false]".to_string())
        );
        // Looking east (yaw -90): a repeater faces west.
        assert_eq!(
            placed_block_state("minecraft:repeater", Some(-90.0)),
            Some("minecraft:repeater[facing=west,delay=1,locked=false,powered=false]".to_string())
        );
        // Blocks without a yaw-derived orientation keep the bare census name.
        assert_eq!(placed_block_state("minecraft:dirt", Some(0.0)), None);
        // And no yaw reported yet keeps the bare name for the directional
        // families too.
        assert_eq!(placed_block_state("minecraft:repeater", None), None);
    }
}
