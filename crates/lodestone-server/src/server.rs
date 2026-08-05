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
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use lodestone_core::State;
use lodestone_entity::DamageFlags;
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, Difficulty, ItemStack, Rotation, Text, TextContent, Vec3,
};
use lodestone_data::block_items;
use lodestone_net::{Connection, NetError, Transport};

use crate::block_entities::{BlockEntity, BlockEntityHandle, block_entity_for_item};
use crate::command::{CommandCaller, CommandDispatch, CommandSession};
use crate::chunk::{
    AIR, ChunkColumn, ChunkSource, generate_columns_offloaded, generate_columns_parallel,
    is_air_or_fluid, is_water,
};
use crate::fall::FallTracker;
use crate::inventory::{ContainerMenuSlot, PlayerInventory, container_menu_slot};
use crate::mobs::{MobHandle, PlayerPerception};
use crate::players::{ChatLine, PlayerListStreamer, PlayerRegistry, PlayerTicket};
use crate::protocol::{EntitySnapshot, ServerBound, ServerDirective, ServerProtocol};
use crate::tick::{BlockTickFeed, ExplosionFeed};
use crate::vitals::{EYE_HEIGHT, PlayerVitals};

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

/// Where a joining player's entity is placed until its client sends a first
/// movement packet (issue #438).
///
/// This must agree with the position the version crate's
/// [`ServerProtocol::begin_play`] teleports the client to, or every other
/// connection sees the newcomer standing somewhere they are not for the
/// fraction of a second before the first `PlayerMoved` arrives. `v770` uses
/// `(8, 100, 8)` (`crates/protocol/v770/src/server_protocol.rs`'s
/// `spawn_x`/`spawn_y`/`spawn_z`), inside chunk `(0, 0)` — the same column
/// `ViewTracker::new((0, 0), view_radius)` centres this join on.
///
/// It is a constant here rather than a value read back from the protocol
/// because [`ServerProtocol`] has no "where is spawn" query and inventing one
/// for this would be a wider seam change than the problem warrants; when a real
/// spawn position exists this, `ViewTracker::new((0, 0), …)` and `begin_play`'s
/// own literals all move together, exactly as that call site's existing comment
/// already says.
const JOIN_SPAWN_POSITION: Vec3 = Vec3::new(8.0, 100.0, 8.0);

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
        pending_chunk_batches.push_back(update.batch);
        return Ok(());
    }
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
///    [`State::Configuration`], then [`ServerProtocol::begin_configuration`].
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
        &CommandDispatch::none(),
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
        &CommandDispatch::none(),
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
        commands,
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
        &CommandDispatch::none(),
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
        &CommandDispatch::none(),
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
        commands,
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
    // Issues #48/#464. `CommandDispatch::none()` — the `Default` — is the
    // inert value every pre-existing entry point passes, so adding this
    // changed no caller's behaviour and no caller's wire bytes.
    commands: &CommandDispatch,
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
                for directive in proto.begin_configuration() {
                    apply(conn, &mut state, directive).await?;
                }
            }
            ServerBound::ConfigurationFinished => {
                state = State::Play;
                for directive in proto.begin_play(view_radius) {
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
                // The centre is `(0, 0)` because that is where this join places
                // the player (`ViewTracker::new((0, 0), view_radius)` below, and
                // `begin_play`'s own spawn point); when a real spawn position
                // arrives this and that line move together.
                //
                // Still **one** chunk batch, not one per ring: the batch markers
                // stay outside this loop, so the client's flow-control
                // accounting (issue #270) sees the same single
                // begin/…/end sequence it always did.
                let mut batch_size = 0;
                for ring in join_view_rings(view_radius) {
                    let columns = source.generate(ring.clone()).await;
                    for (&(cx, cz), column) in ring.iter().zip(columns.iter()) {
                        apply(conn, &mut state, proto.encode_chunk(cx, cz, column)).await?;
                        batch_size += 1;
                    }
                }
                apply(conn, &mut state, proto.end_chunk_batch(batch_size)).await?;
                let chunks_sent = batch_size as usize;

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
                        JOIN_SPAWN_POSITION,
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

                let view = ViewTracker::new((0, 0), view_radius);
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
                let commands = CommandSession {
                    dispatch: commands.clone(),
                    caller: CommandCaller::new(
                        login_uuid.unwrap_or_default(),
                        username.clone(),
                    ),
                };
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
                    commands,
                )
                .await;
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

/// Applies a right-click placement, mirroring
/// `ServerGamePacketListenerImpl.handleUseItemOn`'s replace-vs-relative
/// choice of placement cell (`BlockPlaceContext`'s constructor: place at the
/// clicked block if it `canBeReplaced`, otherwise at its `face`-neighbour) —
/// simplified per this crate's documented scope (`docs/block-edit.md`): no
/// survival/collision validation beyond "is the target cell currently
/// replaceable" (air or a fluid — see [`is_air_or_fluid`]), and no per-block
/// orientation (stairs/slabs/doors would need a precise cursor hit this
/// crate does not decode).
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
/// **Block *state* remains out of scope** (`docs/block-edit.md`): stairs,
/// slabs, logs and redstone dust place with orientation or connection state
/// derived from the click face, cursor and neighbours, and this path still
/// writes each block's default state. #466 is about placing the *right
/// block*; the right *state* is a separate and larger piece of work.
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
/// that opens a menu never falls through to placement. See
/// [`BlockEntity::menu_name`]'s own doc comment for why a composter or
/// brewing stand at `pos` does *not* take this branch (each for a different
/// reason) and instead falls through to the placement logic below exactly
/// as before this change.
#[allow(clippy::too_many_arguments)]
async fn apply_use_item_on<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    pos: BlockPos,
    face: BlockFace,
    inventory: &PlayerInventory,
    block_entities: &BlockEntityHandle,
    next_window_id: &mut i32,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
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

    let neighbour = relative(pos, face);
    let clicked = source.block_state(pos.x, pos.y, pos.z);
    let target = if is_air_or_fluid(&clicked) { pos } else { neighbour };
    let target_state = source.block_state(target.x, target.y, target.z);
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
            source.set_block(target.x, target.y, target.z, block_name);
        }
    }
    for p in [pos, neighbour] {
        let current = source.block_state(p.x, p.y, p.z);
        let directive = proto.encode_block_update(p.x, p.y, p.z, &current);
        apply(conn, state, directive).await?;
    }
    Ok(())
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
/// #270), mirroring `ServerGamePacketListenerImpl::handleClientCommand`'s two
/// modellable ordinals — `action == 1` (`REQUEST_STATS`) has no stats model
/// in this crate and is a documented no-op, matching every other
/// "decoded, no model to act on yet" gap this crate already discloses
/// elsewhere (e.g. `PLAYER_ACTION`'s item-action ordinals).
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
    // Issue #469. Mirrors `player_pos`/`player_rot` exactly — filled here,
    // read back by the caller, republished to the `PlayerRegistry` so *other*
    // connections see it. An out-parameter rather than two more parameters (a
    // registry and this connection's username) because the caller already
    // owns both, and this function already takes 25.
    outgoing_chat: &mut Vec<String>,
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
            apply_use_item_on(
                conn,
                proto,
                // `.get()`: single-block read/write, nothing to offload.
                source.get(),
                state,
                pos,
                face,
                inventory,
                block_entities,
                next_window_id,
                open_container,
                container_sync,
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
            apply_client_command(conn, proto, state, vitals, admin, action).await?;
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
            if let Some(next) = pending_chunk_batches.pop_front() {
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
    // Issues #48/#464. Owned rather than borrowed: it is built once, here at
    // the Play handoff, from *this* connection's login, and it is cheap
    // (an `Option<Arc>` plus a `Uuid` and a `String`).
    commands: CommandSession,
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
                    &mut outgoing_chat,
                    packet_id,
                    &payload,
                )
                .await?;
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
    // Same gap as `vitals`/`container_sync` below: forwarding a random tick's
    // block change with no packet driving it needs `container_sync_tick`, a
    // `tokio::time::interval` this target has none of (see this function's
    // own doc comment). Accepted for signature parity with the native
    // definition (`serve_connection` calls whichever compiles for the
    // target) and never read here — a real, documented gap, not a silent one.
    _block_ticks: &BlockTickFeed,
    // Issue #425: same gap, same reason — a detonation has no packet driving
    // it either, so this target simply never surfaces one.
    _explosions: &ExplosionFeed,
    // Issues #48/#464 — **not** a gap on this target. Commands are entirely
    // packet-driven (a `chat_command` frame arrives, the sink answers, system
    // chat goes back), so the missing timers cost nothing here and this loop
    // dispatches commands identically to the native one.
    commands: CommandSession,
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
            &mut outgoing_chat,
            packet_id,
            &payload,
        )
        .await?;
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
}
