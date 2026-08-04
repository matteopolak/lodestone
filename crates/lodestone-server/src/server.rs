//! The generic integrated-server driver.
//!
//! [`serve_connection`] runs the server side of a single client connection over
//! any [`Transport`]: it reads packets through the shared
//! [`Connection`](lodestone_net::Connection) codec, lifts them with a
//! [`ServerProtocol`], plays the login sequence, and streams the initial view's
//! chunks from a [`ChunkSource`]. The identical loop serves an in-memory
//! [`memory_pair`](lodestone_net::memory_pair) client (singleplayer) or a
//! `TcpStream` client (open-to-LAN).

use std::collections::{HashMap, HashSet};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use lodestone_core::State;
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, Difficulty, ItemStack};
use lodestone_net::{Connection, NetError, Transport};

use crate::block_entities::{BlockEntity, BlockEntityHandle, block_entity_for_item};
use crate::chunk::{AIR, ChunkSource, STONE, is_air_or_fluid, is_water};
use crate::fall::FallTracker;
use crate::inventory::{ContainerMenuSlot, PlayerInventory, container_menu_slot};
use crate::protocol::{EntitySnapshot, ServerBound, ServerDirective, ServerProtocol};
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
                    self.last_sent.insert(entity.id, entity.clone());
                }
                Some(prev) if prev != entity => {
                    directives.extend(proto.encode_entity_update(Some(prev), entity));
                    self.last_sent.insert(entity.id, entity.clone());
                }
                Some(_) => {}
            }
        }

        directives
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
        Self { center, loaded }
    }

    /// Recomputes the view for a new player chunk position `(cx, cz)`,
    /// returning the directives that bring the client's tracked chunks back
    /// in sync — and returning nothing at all if `(cx, cz)` is still the
    /// tracked center (the same "did the 2D chunk position actually change"
    /// guard `ChunkMap::updateChunkTracking` applies before touching the
    /// view at all).
    ///
    /// Order mirrors vanilla's `applyChunkTrackingView`
    /// (`ChunkMap.java:1122-1132`): the cache-center update is sent first
    /// (unconditionally, since by this point the center *did* change —
    /// vanilla additionally guards this send on the center changing, which
    /// is already implied here), then every column that left the window is
    /// forgotten, then every column that entered it is sent as one chunk
    /// batch.
    fn recenter<P, S>(
        &mut self,
        proto: &P,
        source: &S,
        cx: i32,
        cz: i32,
        view_radius: i32,
    ) -> Vec<ServerDirective>
    where
        P: ServerProtocol,
        S: ChunkSource,
    {
        if (cx, cz) == self.center {
            return Vec::new();
        }

        let mut next = HashSet::new();
        for dz in -view_radius..=view_radius {
            for dx in -view_radius..=view_radius {
                next.insert((cx + dx, cz + dz));
            }
        }

        let mut directives = vec![proto.encode_chunk_cache_center(cx, cz)];

        for &(x, z) in self.loaded.difference(&next) {
            directives.push(proto.encode_forget_chunk(x, z));
        }

        let added: Vec<(i32, i32)> = next.difference(&self.loaded).copied().collect();
        if !added.is_empty() {
            directives.push(proto.begin_chunk_batch());
            for &(x, z) in &added {
                let column = source.column(x, z);
                directives.push(proto.encode_chunk(x, z, &column));
            }
            directives.push(proto.end_chunk_batch(added.len() as i32));
        }

        self.center = (cx, cz);
        self.loaded = next;
        directives
    }
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
pub async fn serve_connection<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    entities: &E,
    view_radius: i32,
    block_entities: &BlockEntityHandle,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
    E: EntitySource,
{
    let mut state = State::Handshaking;
    let mut username: Option<String> = None;
    let mut streamer = EntityStreamer::default();

    while let Some((packet_id, payload)) = conn.read_packet().await? {
        match proto.decode(state, packet_id, &payload) {
            ServerBound::Handshake { next_state } => {
                state = next_state;
            }
            ServerBound::LoginStart {
                username: name,
                uuid,
            } => {
                username = Some(name.clone());
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
                let mut batch_size = 0;
                for cz in -view_radius..=view_radius {
                    for cx in -view_radius..=view_radius {
                        let column = source.column(cx, cz);
                        apply(conn, &mut state, proto.encode_chunk(cx, cz, &column)).await?;
                        batch_size += 1;
                    }
                }
                apply(conn, &mut state, proto.end_chunk_batch(batch_size)).await?;
                let chunks_sent = batch_size as usize;

                for directive in proto.welcome_message() {
                    apply(conn, &mut state, directive).await?;
                }

                // Initial entity sync — the same pass the old single-loop
                // version ran on this same iteration via its trailing
                // `if state == State::Play` check, now made explicit because
                // `serve_play` below takes over the loop entirely.
                for directive in streamer.sync(proto, &entities.snapshots()) {
                    apply(conn, &mut state, directive).await?;
                }

                // `ConfigurationFinished` cannot be reached without an
                // earlier `LoginStart` in any correct `ServerProtocol` (the
                // documented ack-driven state machine above), so `username`
                // is always `Some` here; falling back to an empty string
                // rather than panicking keeps a protocol that violates that
                // contract merely wrong, not a crash.
                let username = username.clone().unwrap_or_default();
                let view = ViewTracker::new((0, 0), view_radius);
                return serve_play(
                    conn,
                    proto,
                    source,
                    entities,
                    view_radius,
                    state,
                    streamer,
                    view,
                    username,
                    chunks_sent,
                    block_entities,
                )
                .await;
            }
            ServerBound::KeepAlive { .. }
            | ServerBound::PlayerMoved { .. }
            | ServerBound::BlockAction { .. }
            | ServerBound::UseItemOn { .. }
            | ServerBound::DifficultyChanged { .. }
            | ServerBound::DifficultyLockChanged { .. }
            | ServerBound::GameRuleChanged { .. }
            | ServerBound::CarriedItemChanged { .. }
            | ServerBound::ContainerClicked { .. }
            | ServerBound::ContainerClosed { .. }
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

/// Diffs `current_slots`/`current_data` (freshly read off the block entity at
/// `open.pos`) against what [`ContainerSync`] last pushed to this
/// connection, returning the directives that bring the client back in sync —
/// only the entries that actually changed, each via
/// [`ServerProtocol::encode_container_slot`]/[`encode_container_data`](ServerProtocol::encode_container_data).
///
/// This is the one piece of Job 1 with no client packet driving it at all:
/// [`open_container_screen`] covers "a player opens a menu" and
/// [`apply_container_clicked`] covers "a player clicks in one," but a
/// furnace's own background tick (`crate::block_entities::run_block_entity_tick_loop`,
/// running independently of any connection) is neither — see
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

    let (own_slots, data) = block_entities.with(|reg| match reg.get(pos) {
        Some(entity) => (entity.container_slots(), entity.data_properties()),
        None => (Vec::new(), Vec::new()),
    });

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
/// **Placement now honours the held item for the four block-entity blocks**
/// (issue: `docs/block-entities.md`'s second named gap). `inventory`'s
/// currently selected item is looked up through
/// [`block_entity_for_item`]: a furnace/smoker/blast-furnace/composter/
/// hopper/brewing-stand item writes its own block and inserts a fresh
/// [`crate::block_entities::BlockEntity`] into `block_entities` at the
/// target position; anything else (including an empty hand — this crate has
/// no "consume from an empty hand" concept to reject) still falls back to
/// [`STONE`], exactly as before this change. This is a deliberately narrow
/// extension of `docs/block-edit.md`'s existing scope cut, not a general
/// per-item placement system — see that doc for why a wider one (a real
/// `BlockItem` registry) is not attempted here.
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
        let resolved = held_item
            .as_deref()
            .and_then(block_entity_for_item);
        match resolved {
            Some((block_name, entity)) => {
                block_entities.with(|registry| registry.insert(target, entity));
                source.set_block(target.x, target.y, target.z, block_name);
            }
            None => {
                source.set_block(target.x, target.y, target.z, STONE);
            }
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

/// Decodes and applies one inbound packet once the connection is in
/// [`State::Play`]: matches a keep-alive echo against the pending challenge
/// (clearing it, so the next keep-alive tick does not mistake a live client
/// for a dead one), streams the view when the player's chunk column changed,
/// tracks the player's latest position for [`PlayerVitals`]' submersion test,
/// feeds [`FallTracker`] and applies any resulting fall damage, applies a
/// block break/placement (see [`apply_block_action`]/[`apply_use_item_on`]),
/// applies a difficulty/game-rule change (see
/// [`apply_difficulty_change`]/[`apply_game_rule_changed`]), or applies a
/// hotbar selection/container click against [`PlayerInventory`] (see
/// [`apply_carried_item_changed`]/[`apply_container_clicked`]).
/// Every other packet decodes to [`ServerBound::Ignored`] in `State::Play`
/// under the current protocols (no further state transitions are modeled —
/// no respawn/dimension change yet) and is a no-op here.
#[allow(clippy::too_many_arguments)]
async fn dispatch_play_packet<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    view_radius: i32,
    state: &mut State,
    view: &mut ViewTracker,
    pending_keep_alive: &mut Option<i64>,
    pending_break: &mut Option<BlockPos>,
    player_pos: &mut Option<(f64, f64, f64)>,
    fall: &mut FallTracker,
    vitals: &mut PlayerVitals,
    admin: &mut WorldAdminState,
    inventory: &mut PlayerInventory,
    block_entities: &BlockEntityHandle,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    next_window_id: &mut i32,
    packet_id: i32,
    payload: &[u8],
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
{
    match proto.decode(*state, packet_id, payload) {
        ServerBound::KeepAlive { id } => {
            if *pending_keep_alive == Some(id) {
                *pending_keep_alive = None;
            }
        }
        ServerBound::PlayerMoved { x, y, z, on_ground } => {
            *player_pos = Some((x, y, z));

            // Chunk coordinate = floor(block / 16), not truncating division —
            // `-1.0_f64 / 16.0` must floor to chunk `-1`, matching vanilla's
            // `SectionPos.blockToSectionCoord` (an arithmetic right shift).
            let cx = (x / 16.0).floor() as i32;
            let cz = (z / 16.0).floor() as i32;
            for directive in view.recenter(proto, source, cx, cz, view_radius) {
                apply(conn, state, directive).await?;
            }

            if let Some(raw) = fall.on_player_moved(y, on_ground)
                && vitals.apply_fall_damage(raw as f32).is_some()
            {
                apply(conn, state, proto.encode_set_health(vitals.health())).await?;
            }
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
                source,
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
                source,
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
        ServerBound::Handshake { .. }
        | ServerBound::LoginStart { .. }
        | ServerBound::LoginAcknowledged
        | ServerBound::ConfigurationFinished
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
    source: &S,
    entities: &E,
    view_radius: i32,
    mut state: State,
    mut streamer: EntityStreamer,
    mut view: ViewTracker,
    username: String,
    chunks_sent: usize,
    block_entities: &BlockEntityHandle,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
    E: EntitySource,
{
    let mut pending_keep_alive: Option<i64> = None;
    let mut pending_break: Option<BlockPos> = None;
    let mut player_pos: Option<(f64, f64, f64)> = None;
    let mut vitals = PlayerVitals::default();
    let mut fall = FallTracker::default();
    let mut admin = WorldAdminState::default();
    let mut inventory = PlayerInventory::default();
    let mut open_container: Option<OpenContainer> = None;
    let mut container_sync = ContainerSync::default();
    // Vanilla's `ServerPlayer::nextContainerCounter` starts at `0` and the
    // very first open bumps it to `1` before use (`ServerPlayer.java:1330,
    // 1343`) — see [`open_container_screen`]'s own `% 100 + 1` wrap.
    let mut next_window_id: i32 = 0;
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
                    &mut fall,
                    &mut vitals,
                    &mut admin,
                    &mut inventory,
                    block_entities,
                    &mut open_container,
                    &mut container_sync,
                    &mut next_window_id,
                    packet_id,
                    &payload,
                )
                .await?;
                for directive in streamer.sync(proto, &entities.snapshots()) {
                    apply(conn, &mut state, directive).await?;
                }
            }

            _ = keep_alive_tick.tick() => {
                if pending_keep_alive.is_some() {
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
                    let eye_state = source.block_state(
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
                // The piece with no inbound packet driving it at all: a
                // furnace's own background tick loop
                // (`crate::block_entities::run_block_entity_tick_loop`) mutates
                // the registry independently of any connection, so this
                // connection needs its own timer to notice — see
                // `sync_open_container`'s own doc comment.
                if let Some(open) = open_container.as_mut() {
                    let (slots, data) = block_entities.with(|reg| match reg.get(open.pos) {
                        Some(entity) => (entity.container_slots(), entity.data_properties()),
                        None => (Vec::new(), Vec::new()),
                    });
                    for directive in
                        sync_open_container(proto, open, &mut container_sync, slots, data)
                    {
                        apply(conn, &mut state, directive).await?;
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
    source: &S,
    entities: &E,
    view_radius: i32,
    mut state: State,
    mut streamer: EntityStreamer,
    mut view: ViewTracker,
    username: String,
    chunks_sent: usize,
    block_entities: &BlockEntityHandle,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource,
    E: EntitySource,
{
    let mut pending_keep_alive: Option<i64> = None;
    let mut pending_break: Option<BlockPos> = None;
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
            &mut fall,
            &mut vitals,
            &mut admin,
            &mut inventory,
            block_entities,
            &mut open_container,
            &mut container_sync,
            &mut next_window_id,
            packet_id,
            &payload,
        )
        .await?;
        for directive in streamer.sync(proto, &entities.snapshots()) {
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
}
