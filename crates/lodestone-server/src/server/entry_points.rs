//! The public `serve_connection*` entry points: each builds the matching argument set and runs the shared connection driver.

use super::*;

/// Serves one client connection through login, configuration, the play join
/// sequence, and the initial chunk view — then keeps serving until the client
/// disconnects.
///
/// The loop transitions Handshaking → Login → Configuration → Play according to
/// the [`ServerProtocol`] capability. Protocols with a Configuration phase use
/// the acknowledgement-driven choreography; legacy protocols enter Play after
/// login success because their wire has no configuration acknowledgements:
///
/// 1. [`ServerBound::LoginStart`] → [`ServerProtocol::login_success`] (no
///    state change yet).
/// 2. For a protocol with a Configuration phase, [`ServerBound::LoginAcknowledged`] → state becomes
///    [`State::Configuration`], then [`ServerProtocol::encode_registry_data`]
///    (the configuration phase requires registries before the finish signal), then
///    [`ServerProtocol::begin_configuration`].
/// 3. For a legacy protocol, the loop queues the same
///    [`ServerBound::ConfigurationFinished`] transition immediately after
///    [`ServerProtocol::login_success`]. Otherwise, the client's
///    [`ServerBound::ConfigurationFinished`] → state becomes [`State::Play`],
///    then [`ServerProtocol::begin_play`], then every column in
///    `[-view_radius, view_radius]²` (chunk coordinates) from `source` in
///    bounded chunk batches
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
/// permanently-empty [`BlockTickFeed`]. Callers that need world-tick-driven
/// block changes, including [`crate::IntegratedServer::open_in_memory_with_mobs`],
/// use the variant that receives a live feed.
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
/// core thread.
///
/// Preserves packet flow while allowing `Arc<S>` to move into a
/// `spawn_blocking` closure, keeping column generation off the async runtime's
/// core thread. [`SourceRef`] records the borrowed-versus-shared source
/// distinction: `&S` cannot satisfy `spawn_blocking`'s `'static` bound, while
/// the compatibility wrapper accepts a borrowed source.
///
/// `pub(crate)` because `mod server` is private. The public server surface is
/// exposed through [`crate::IntegratedServer`], while this helper serves as an
/// internal source-sharing entry point.
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
    // Forwarded rather than defaulted to `view_radius`, because this
    // is one of the two entry points a caller with its own memory policy uses —
    // `IntegratedServer::open_in_memory*` passes [`MAX_CLIENT_VIEW_RADIUS`] here
    // so the slider can actually be raised mid-session. See
    // `ViewTracker::max_radius`.
    max_view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    // Ticket state from `ChunkStore::tickets()`. Integrated connections must
    // use this shared handle rather than an isolated default.
    tickets: &TicketStoreHandle,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    // Default world/feed handles provide isolated state for this compatibility
    // wrapper; no world-tick consumer is attached here.
    let world = &crate::world_state::WorldStateHandle::default();
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Shared(source),
        entities,
        view_radius,
        max_view_radius,
        block_entities,
        mobs,
        tickets,
        &BlockTickFeed::default(),
        &ExplosionFeed::default(),
        &WeatherFeed::default(),
        // No world-tick consumer is attached to this vote/feed pair.
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
        world,
        &crate::live_save::LiveSaveSlot::default(),
        // The inert default admits everybody and grants no operator role.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Offline mode; `serve_connection_with_online_mode` passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`serve_connection`], plus the host's access lists and this connection's
/// remote address.
///
/// Added *beside* [`serve_connection`] rather than by widening it, for the reason
/// every wrapper in this file exists: `crates/protocol/v770/tests/*` call the
/// narrow ones directly. The production LAN path goes through
/// [`serve_connection_with_mob_events_and_commands_shared`], which carries the
/// same two arguments; this exists so the enforcement is drivable from outside the
/// crate — an access check nothing can call from a test is exactly the island the
/// repo rules are about.
///
/// # Errors
///
/// As [`serve_connection`], plus [`ServerError::AccessDenied`] when the lists
/// refuse the login.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub async fn serve_connection_with_access<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    entities: &E,
    view_radius: i32,
    access: &crate::access::AccessHandle,
    peer_ip: Option<std::net::IpAddr>,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    let world = &crate::world_state::WorldStateHandle::default();
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        view_radius,
        &BlockEntityHandle::default(),
        &MobHandle::default(),
        &TicketStoreHandle::default(),
        &BlockTickFeed::default(),
        &ExplosionFeed::default(),
        &WeatherFeed::default(),
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
        world,
        &crate::live_save::LiveSaveSlot::default(),
        access,
        peer_ip,
        // Offline mode for this compatibility wrapper.
        None,
    )
    .await
}

/// [`serve_connection_with_access`], with the world state and block-entity
/// registry caller-supplied rather than a private default.
///
/// # Why this exists
///
/// Every existing entry point that takes a real [`crate::access::AccessHandle`]
/// builds its `WorldStateHandle`/`BlockEntityHandle` internally and never hands
/// them back, so nothing outside this function can observe what a connection
/// actually did — only that it did not error. That is enough to prove a
/// low-permission caller's `DifficultyChanged`/`DifficultyLockChanged`/
/// `GameRuleChanged`/`SetCommandBlock`/`ChangeGameMode`/
/// `REQUEST_GAMERULE_VALUES` packet was *accepted* on the wire, never that its
/// effect was *refused* — the exact "assertions of an absence need a control
/// proving the detector works" gap this constructor closes.
///
/// # Errors
///
/// As [`serve_connection_with_access`].
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub async fn serve_connection_with_access_and_state<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    entities: &E,
    view_radius: i32,
    access: &crate::access::AccessHandle,
    world: &crate::world_state::WorldStateHandle,
    block_entities: &BlockEntityHandle,
    peer_ip: Option<std::net::IpAddr>,
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
        view_radius,
        block_entities,
        &MobHandle::default(),
        &TicketStoreHandle::default(),
        &BlockTickFeed::default(),
        &ExplosionFeed::default(),
        &WeatherFeed::default(),
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
        world,
        &crate::live_save::LiveSaveSlot::default(),
        access,
        peer_ip,
        // Offline mode for this compatibility wrapper.
        None,
    )
    .await
}

/// The shared mob-event connection path plus a host-installed command
/// dispatcher.
///
/// The singleplayer-shaped counterpart to
/// [`serve_connection_with_commands`]: `_shared` is the off-core-thread chunk
/// path that [`crate::IntegratedServer::open_in_memory_with_mobs`]
/// uses, and that constructor is the **only** production route a real player
/// reaches this crate through. So this is the entry point singleplayer commands
/// have to come in on; the borrowed-source
/// [`serve_connection_with_commands`] cannot serve it.
///
/// It carries the same live view ceiling, sleep, border and save handles as
/// the plain singleplayer wrapper. The local command constructor must not
/// replace those with defaults merely to install a dispatch: doing so would
/// make plugin commands silently change unrelated integrated-world behaviour.
/// LAN callers pass their existing disconnected defaults explicitly and retain
/// their own configured `CommandDispatch` policy.
///
/// # Errors
///
/// As [`serve_connection`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve_connection_with_mob_events_and_commands_shared<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &Arc<S>,
    entities: &E,
    view_radius: i32,
    max_view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    // `IntegratedServer`'s real handle — see
    // `serve_connection_shared`'s own parameter comment. Ungated like the rest
    // of this function's signature, since browser singleplayer reaches the
    // server through this entry point too.
    tickets: &TicketStoreHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    sleep_vote: &SleepVote,
    sleep_feed: &SleepFeed,
    commands: &CommandDispatch,
    border: &BorderFeed,
    // The three host-supplied surfaces every other constructor
    // hardcodes to `::default()`. `IntegratedServer::open_to_lan` is the one
    // caller that can actually carry a configured one, which is why they are
    // parameters here and nowhere else.
    resource_packs: &ResourcePackPushFeed,
    plugin_channels: &PluginChannelRegistry,
    // The world's shared scalars, the *same* handle
    // `run_tick_loop` ticks. See `serve_connection_inner`'s parameter comment.
    world: &crate::world_state::WorldStateHandle,
    live_save: &crate::live_save::LiveSaveSlot,
    // The host's ops/whitelist/ban lists, shared by every accepted connection,
    // plus this connection's own remote address for the IP ban list. Parameters
    // here and nowhere else for the same reason the three above are:
    // `open_to_lan` is the one caller that can carry a configured one.
    //
    // Target-gated to match `serve_connection_inner`'s own two, which they are
    // forwarded straight into. This function is NOT gated — browser
    // singleplayer reaches the server through it — so leaving the parameters
    // ungated named a `cfg(not(wasm32))` module from ungated code and broke the
    // wasm build outright. `open_to_lan`, the only caller that passes a real
    // one, is native-only anyway: remote players and an on-disk ban list are
    // both things a browser world does not have.
    #[cfg(not(target_arch = "wasm32"))] access: &crate::access::AccessHandle,
    #[cfg(not(target_arch = "wasm32"))] peer_ip: Option<std::net::IpAddr>,
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
        max_view_radius,
        block_entities,
        mobs,
        tickets,
        block_ticks,
        explosions,
        &WeatherFeed::default(),
        sleep_vote,
        sleep_feed,
        commands,
        border,
        resource_packs,
        plugin_channels,
        world,
        live_save,
        #[cfg(not(target_arch = "wasm32"))]
        access,
        #[cfg(not(target_arch = "wasm32"))]
        peer_ip,
        // Offline mode; `serve_connection_with_online_mode` passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`serve_connection_with_mob_events_and_commands_shared`], plus online-mode
/// encryption and session-server verification — the
/// `_and_commands_shared`-shaped sibling promised by that function's own
/// `online_mode` argument comment.
///
/// A dedicated entry point keeps the compatibility wrapper's signature stable
/// while adding online-mode authentication. The integrated host can select this
/// function when it supplies [`OnlineModeConfig`]; protocol tests and other
/// callers can invoke it directly.
///
/// # Errors
///
/// As [`serve_connection`], plus [`ServerError::VerifyTokenMismatch`],
/// [`ServerError::UnverifiedUsername`] and
/// [`ServerError::AuthServiceUnavailable`] for the new handshake.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub async fn serve_connection_with_online_mode<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &Arc<S>,
    entities: &E,
    view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    // `IntegratedServer`'s real handle — see
    // `serve_connection_shared`'s own parameter comment. `open_to_lan` reaches
    // this entry point whenever `LanConfig::online_mode` is `Some`.
    tickets: &TicketStoreHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    commands: &CommandDispatch,
    resource_packs: &ResourcePackPushFeed,
    plugin_channels: &PluginChannelRegistry,
    world: &crate::world_state::WorldStateHandle,
    access: &crate::access::AccessHandle,
    peer_ip: Option<std::net::IpAddr>,
    online_mode: &OnlineModeConfig,
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
        view_radius,
        block_entities,
        mobs,
        tickets,
        block_ticks,
        explosions,
        &WeatherFeed::default(),
        &SleepVote::default(),
        &SleepFeed::default(),
        commands,
        &BorderFeed::default(),
        resource_packs,
        plugin_channels,
        world,
        &crate::live_save::LiveSaveSlot::default(),
        access,
        peer_ip,
        Some(online_mode),
    )
    .await
}

/// Like [`serve_connection`], but also forwards every change published on
/// `block_ticks` (the world tick loop's random ticks) to
/// this connection, through the same `container_sync_tick` timer arm inside
/// [`serve_play`] that already forwards block-entity registry changes with
/// no packet driving them — see that arm's own doc comment.
///
/// Forwards to [`serve_connection_inner`] with a fresh, permanently-empty
/// [`ExplosionFeed`], matching [`serve_connection`]'s compatibility behavior.
/// [`crate::IntegratedServer::open_in_memory_with_mobs`] calls
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
    // Default world/feed handles provide isolated state for this wrapper; no
    // world-tick consumer is attached here.
    let world = &crate::world_state::WorldStateHandle::default();
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        // The join radius also serves as this wrapper's maximum; see
        // `ViewTracker::max_radius`.
        view_radius,
        block_entities,
        mobs,
        &TicketStoreHandle::default(),
        block_ticks,
        &ExplosionFeed::default(),
        &WeatherFeed::default(),
        // No world-tick consumer is attached to this vote/feed pair.
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
        world,
        &crate::live_save::LiveSaveSlot::default(),
        // The inert default admits everybody and grants no operator role.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Offline mode; `serve_connection_with_online_mode` passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// Borrowed-source wrapper that forwards block, explosion, and weather feeds to
/// a connection. The `container_sync_tick` arm drains those feeds into
/// protocol packets. The borrowed source keeps its lifetime local, which makes
/// this wrapper useful for focused protocol tests.
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
    // Default world/feed handles provide isolated state for this wrapper; no
    // world-tick consumer is attached here.
    let world = &crate::world_state::WorldStateHandle::default();
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        // The join radius also serves as this wrapper's maximum; see
        // `ViewTracker::max_radius`.
        view_radius,
        block_entities,
        mobs,
        &TicketStoreHandle::default(),
        block_ticks,
        explosions,
        weather,
        // No caller wires a sleep vote through this wrapper; the feed-carrying
        // variant is `serve_connection_with_mob_events_and_commands_shared`.
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
        world,
        &crate::live_save::LiveSaveSlot::default(),
        // The inert default admits everybody and grants no operator role.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Offline mode; `serve_connection_with_online_mode` passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`serve_connection`], plus a host-installed command dispatcher.
///
/// This is the **only** entry point that can make a `/command` from a real
/// player do anything. Every other one above passes
/// [`CommandDispatch::none()`], under which a `chat_command` frame decodes,
/// reaches this crate, and is answered with
/// [`UNKNOWN_COMMAND`](crate::UNKNOWN_COMMAND) — the fail-closed direction.
///
/// This entry point carries command dispatch in addition to block and explosion
/// feeds, while the smaller wrappers retain their compatibility signatures.
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
    // Default world/feed handles provide isolated state for this wrapper; no
    // world-tick consumer is attached here.
    let world = &crate::world_state::WorldStateHandle::default();
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        // The join radius also serves as this wrapper's maximum; see
        // `ViewTracker::max_radius`.
        view_radius,
        block_entities,
        mobs,
        &TicketStoreHandle::default(),
        block_ticks,
        explosions,
        &WeatherFeed::default(),
        // No world-tick consumer is attached to this vote/feed pair.
        &SleepVote::default(),
        &SleepFeed::default(),
        commands,
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
        world,
        &crate::live_save::LiveSaveSlot::default(),
        // The inert default admits everybody and grants no operator role.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Offline mode; `serve_connection_with_online_mode` passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`serve_connection`], plus a host-observable [`ResourcePackPushFeed`]
/// This is the entry point that makes a server-initiated resource pack
/// push reach a player at all.
///
/// A host constructs a [`ResourcePackPushFeed`], passes it here, and publishes
/// [`ResourcePackPush`] values into it. `serve_play` drains the feed into
/// clientbound `resource_pack_push` frames.
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
    // Default world/feed handles provide isolated state for this wrapper; no
    // world-tick consumer is attached here.
    let world = &crate::world_state::WorldStateHandle::default();
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        // The join radius also serves as this wrapper's maximum; see
        // `ViewTracker::max_radius`.
        view_radius,
        block_entities,
        mobs,
        &TicketStoreHandle::default(),
        block_ticks,
        explosions,
        &WeatherFeed::default(),
        // No world-tick consumer is attached to this vote/feed pair.
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        resource_packs,
        &PluginChannelRegistry::default(),
        world,
        &crate::live_save::LiveSaveSlot::default(),
        // The inert default admits everybody and grants no operator role.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Offline mode; `serve_connection_with_online_mode` passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`serve_connection`], plus a live [`PluginChannelRegistry`] —
/// the entry point that makes wire-level plugin messaging reach a player at all.
///
/// A host constructs a [`PluginChannelRegistry`], registers handlers that
/// implement `crate::PluginChannelHandler`, and passes it here. Inbound `custom_payload`
/// packets dispatch to the registered handler, while
/// [`PluginChannelRegistry::broadcast`] values are filtered to the channels
/// each client announced and drained into clientbound frames.
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
    // Default world/feed handles provide isolated state for this wrapper; no
    // world-tick consumer is attached here.
    let world = &crate::world_state::WorldStateHandle::default();
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        // The join radius also serves as this wrapper's maximum; see
        // `ViewTracker::max_radius`.
        view_radius,
        block_entities,
        mobs,
        // Use isolated ticket state; this wrapper does not share integrated
        // world residency.
        &TicketStoreHandle::default(),
        block_ticks,
        explosions,
        &WeatherFeed::default(),
        // No world-tick consumer is attached to this vote/feed pair.
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        plugin_channels,
        world,
        &crate::live_save::LiveSaveSlot::default(),
        // The inert default admits everybody and grants no operator role.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Offline mode; `serve_connection_with_online_mode` passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}
