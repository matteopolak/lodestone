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
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use lodestone_time::Instant;
use crate::worldgen_progress::{PhaseTimer, WorldgenTimingPhase};

#[path = "connection_travel.rs"]
mod connection_travel;
#[path = "connection_prediction.rs"]
mod connection_prediction;
mod player_persistence;
use self::player_persistence::*;
mod online_mode;
pub use self::online_mode::*;
mod view_tracker;
use self::view_tracker::*;
mod end_gateway;
use self::end_gateway::*;
mod source_ref;
pub(crate) use self::source_ref::*;
mod entity_streaming;
pub use self::entity_streaming::*;
mod join_trace;
pub(crate) use self::join_trace::*;


use lodestone_core::State;
use lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE;
use lodestone_entity::{DamageFlags, ItemLifecycle};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, BundleItemSlot, EntityAttributeSnapshot, GameMode, HotbarSlot, ItemStack, MenuSlot,
    ResourceKey, ResourcePackResponseKind, Rotation, Text, TextContent, Vec3, Vec3f,
    WrittenBookContent,
};
use lodestone_model::command_tree::CommandSuggestionEntry;
use lodestone_data::{
    block::Block,
    block_properties::{BuiltinPropertyValue, Properties, PropertyKey, PropertyValue},
    block_items,
    block_states::StateId,
    item::Item,
    potion::PotionId,
};
use lodestone_net::{Connection, NetError, Transport};
// Encryption half: the server-side RSA keypair/decrypt and the
// verify-token generator. Native-only for the same reason `crate::access` is
// (see that field's own doc comment below) — online-mode auth needs the
// native-only `lodestone-auth` session-server call too, so there is nothing
// for a `wasm32` build to gain by linking these.
#[cfg(not(target_arch = "wasm32"))]
use lodestone_net::{ServerKeyPair, generate_verify_token};

use crate::advancements::AdvancementManager;
use crate::block_breaking::PendingBreak;
use crate::block_entities::{
    BlockEntity, BlockEntityHandle, BlockEntityKind, block_entity_for_item,
};
use crate::border::BorderFeed;
use crate::brewing::{Bottle, BottleKind, is_ingredient};
use crate::composter::{InsertOutcome, compostable_chance};
use crate::command::{CommandCaller, CommandDispatch, CommandSession};
use crate::chunk::{
    ChunkColumn, ChunkGenerationStage, ColumnLightSettlementError, ChunkSource,
    generate_columns_offloaded,
};
#[cfg(not(target_arch = "wasm32"))]
use crate::chunk::generate_columns_parallel;
#[cfg(target_arch = "wasm32")]
use crate::chunk::generate_columns_borrowed;
use crate::fall::{FallSample, FallTracker};
use crate::container_click::{
    Click, MayPickup, MenuKind, MenuLayout, SelectedBundleIndex, SlotKind, Station, do_click_with,
};
use crate::crafting::CraftingState;
use crate::inventory::{HOTBAR_SIZE, OFFHAND_NATIVE, PlayerInventory, window_zero_menu_slot};
use crate::mob_spawn::SpawnRng;
use crate::mobs::{MobHandle, PerceivedPlayer, PlayerIdentity, PlayerPerception};
use crate::neighbor_update::Direction;
use crate::players::{ChatLine, PlayerListStreamer, PlayerRegistry, PlayerTicket};
use crate::plugin_channels::{ClientChannels, PluginChannelRegistry};
use crate::protocol::{
    Abilities, BossBarSnapshot, ChunkEncodeError, EntitySnapshot, MerchantOfferOut,
    ResourcePackPush, ServerBound, ServerDirective, ServerProtocol,
};
use crate::redstone::WorldState;
use crate::redstone_diode::{set_comparator, set_repeater};
use crate::redstone_observer::set_observer;
use crate::scheduled_tick::{ScheduledTick, ScheduledTickKind, ScheduledTickQueue};
use crate::sleep::{SleepEvent, SleepFeed, SleepVote};
use crate::ticket::{PLAYER_SPAWN_RADIUS, PlayerTicketGuard, TicketKind, TicketOwner, TicketStoreHandle};
use crate::tick::{BlockTickFeed, ExplosionFeed};
use crate::weather::WeatherFeed;
use crate::vitals::{EYE_HEIGHT, PlayerVitals};
use crate::world_spawn::{RespawnPoint, is_bed_block, is_legal_bed_respawn};

/// Server-initiated keep-alive interval, and the width of the window in
/// which an echo must arrive before the connection is treated as dead.
///
/// Vanilla's own latency-check-interval and closed-listener-timeout constants
/// are both the literal
/// constant `15000` (milliseconds) — **not** two different numbers.
/// Vanilla's own "keep connection alive" step
/// sends a fresh challenge once `now - keepAliveTime >= 15000`, and
/// disconnects immediately if the *previous* challenge is still pending at
/// that point — so an unanswered challenge is caught within one more
/// interval of being sent (up to ~15s later), not two intervals (~30s).
#[cfg(not(target_arch = "wasm32"))]
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(15_000);

/// MOTD included in the server-list status reply.
///
/// Vanilla's own default is `server.properties`' `motd=A Minecraft Server`
/// (vanilla's own dedicated-server properties reader, and the
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

/// `crate::sleep`: the server-side entity id of the single local
/// player in a singleplayer world — the roster key a connection with no
/// [`PlayerRegistry`] uses when it votes (see the sleep-vote inner state's
/// own sleepers doc
/// comment). Matches `crates/protocol/v770/src/server_protocol.rs`'s
/// `LOCAL_PLAYER_ENTITY_ID`, which is what the v770 encoder believes the local
/// player's id is; keeping the two constants equal is the join, and the
/// reason `crate::sleep`'s module doc names this crate as the source.
pub(crate) const LOCAL_PLAYER_ENTITY_ID: i32 = 1;

/// The disconnect reason for an unanswered keep-alive.
///
/// The disconnect reason is a translatable text component keyed
/// `"disconnect.timeout"`, sent from the keep-alive timeout path, so the key
/// is not ours to choose. The `fallback` is the English string for that key,
/// read from
/// `.cache/mc/26.2/client-src/assets/minecraft/lang/en_us.json:3498`
/// (`"disconnect.timeout": "Timed out"`) — not invented here.
///
/// Carrying a fallback makes the response readable when a client cannot resolve
/// the key. A client with translations shows its localized "Timed out", while a
/// client that renders raw translation keys shows readable English instead of
/// the literal string `disconnect.timeout`.
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
/// Vanilla's own "is valid player name" check:
/// at most 16 characters, and **no** character `<= 32` or `>= 127` — i.e. every
/// char must be printable ASCII, excluding space. Vanilla checks this on the
/// login-phase `hello` packet.
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
/// name by throwing (a validation helper wrapping its own "is valid player
/// name" check),
/// which closes the connection with no
/// translatable reason at all. Rejecting is faithful; explaining is an
/// improvement, so this is a plain literal rather than a translation key we would
/// have had to invent.
fn invalid_username_reason() -> Text {
    Text::literal("Invalid username")
}

/// The disconnect reason for a chunk column the selected protocol cannot encode.
fn chunk_encode_failure_reason() -> Text {
    Text::literal("Failed to encode terrain")
}

/// Vanilla's `multiplayer.disconnect.unverified_username` English text
/// (`assets/minecraft/lang/en_us.json`), sent when the session server's
/// `hasJoined` answers "this client never proved ownership of this
/// username".
#[cfg(not(target_arch = "wasm32"))]
fn unverified_username_reason() -> Text {
    Text::literal("Failed to verify username!")
}

/// Vanilla's `multiplayer.disconnect.authservers_down` English text, sent
/// when the `hasJoined` call itself fails (network error, bad response) —
/// distinct from [`unverified_username_reason`], which is the session
/// server successfully saying "no".
#[cfg(not(target_arch = "wasm32"))]
fn auth_servers_down_reason() -> Text {
    Text::literal("Authentication servers are down. Please try again later. Sorry!")
}

/// Vanilla's disconnect component when a client attempts to replace its chat
/// session with a valid certificate that expires before the installed one.
#[cfg(not(target_arch = "wasm32"))]
fn expired_profile_public_key_reason() -> Text {
    Text::translate("multiplayer.disconnect.expired_public_key", Vec::new())
}

/// Cadence of the periodic time-of-day broadcast.
///
/// Vanilla re-broadcasts the world's monotonic game time every 20 ticks
/// (vanilla's own "force game time synchronization" step,
/// gated on `if (this.tickCount % 20 == 0)`) —
/// carrying an *empty* clock-update map, which is what tells a client to keep
/// its held day/night anchor rather than resetting it (see
/// `packets::time::SetTime::day_clock`'s doc comment in the `v770` crate).
/// This crate has no fixed server tick loop (see the module docs), so a
/// 1-second wall-clock interval stands in for "every 20 ticks" at vanilla's
/// normal 20 TPS.
#[cfg(not(target_arch = "wasm32"))]
const TIME_SYNC_INTERVAL: Duration = Duration::from_millis(1_000);

/// The largest view radius a client may raise itself to mid-session on a path
/// whose memory it owns — the ceiling `IntegratedServer::open_in_memory*` hands
/// [`ViewTracker::max_radius`].
///
/// **Derived, not chosen.** The shell's render-distance slider tops out at
/// `config::MAX_RENDER_DISTANCE = 256` chunks and
/// The integrated client requests two rings beyond its selected render distance:
/// one for neighbor-aware meshing and one to keep that halo ahead of movement.
/// The initial join field is a VarInt and can carry the resulting `258`-chunk
/// stream radius. Live client-information
/// updates are a signed byte in every supported protocol, so they can advertise
/// at most `127`; the shell keeps that wire limit explicit when sending an
/// update rather than allowing a wrapping conversion.
///
/// This is a *sanity* bound rather than a memory policy: the wire field is an
/// `i8`, so without it a malformed packet asking for `127` would try to stream
/// 65,025 columns. Singleplayer is deliberately **not** capped by
/// `chunk_store::MAX_CAPACITY` — see that constant and
/// `chunk_store::integrated_capacity_for_view_radius` for whose memory is being
/// spent, and this module's own note on what the store's capacity does *not*
/// follow.
pub const MAX_CLIENT_VIEW_RADIUS: i32 = 258;

/// Milliseconds per tick at vanilla's normal 20 TPS, used to convert
/// wall-clock elapsed time into the tick-based `game_time`
/// [`ServerProtocol::encode_set_time`] carries, in the absence of a real
/// per-tick server loop.
#[cfg(not(target_arch = "wasm32"))]
const MILLIS_PER_TICK: u128 = 50;

/// The melee knockback bonus for a sprinting, full-strength attack is `0.5`.
/// A bare-handed or non-sprinting attack contributes `0.0`; no weapon or
/// enchantment modifier is modeled here. [`apply_attack`] passes this constant
/// only for sprinting attacks.
const SPRINT_ATTACK_KNOCKBACK_POWER: f64 = 0.5;

/// Cadence of the air-supply/drowning-damage tick ([`crate::vitals`]).
/// Vanilla ticks its own generic per-tick base update's water-breath block once per real
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

/// Cadence of the server-driven entity/player streaming pass ([`stream_pass`]).
///
/// **Why this timer exists at all.** Every other caller of [`stream_pass`] in
/// this file is packet-driven: the join sync, and the `read_packet` arm of
/// [`serve_play`]'s loop. That made a connection's whole view of the world
/// advance only when *it* spoke, which is not what vanilla does — its entity
/// tracker runs from the server tick, independent of any client's input. Two
/// measured consequences of the packet-driven form, both real:
///
/// - A player who joins after you were already online is invisible until your
///   own next outbound packet. A client that has gone quiet (our own
///   `select_move_packet` port only re-sends an idle position every 20 ticks)
///   therefore learns about them up to a second late — and one that sends
///   nothing at all never learns about them.
/// - The same holds for every mob's movement, so standing perfectly still made
///   the rest of the world advance in one-second jumps.
///
/// The value is [`MILLIS_PER_TICK`], the same 20 TPS stand-in
/// [`VITALS_TICK_INTERVAL`] uses and the rate vanilla's tracker runs at. The
/// pass is a diff — [`EntityStreamer`] emits nothing when nothing changed.
/// Tick-published entities are compared once per publication; the player
/// registry is sampled on every pass because connections update it directly.
#[cfg(not(target_arch = "wasm32"))]
const ENTITY_STREAM_INTERVAL: Duration = Duration::from_millis(50);

/// [`VITALS_TICK_INTERVAL`]'s wasm32 counterpart — same value (vanilla's 20
/// TPS, same as every other timer in this file), kept as its own literal
/// rather than sharing the native constant. Independent literals that happen
/// to agree are this crate's own established shape for a per-target cadence
/// (see `tick.rs`'s module doc on why `MILLIS_PER_TICK` is not shared either),
/// and here it is load-bearing: [`VITALS_TICK_INTERVAL`] is only compiled for
/// `not(wasm32)`, so a single shared constant would have to drop its `cfg`
/// entirely, which reintroduces exactly the "second file the
/// `tokio::time::Instant` ban allows" shape `tick.rs` warns against for the
/// neighbouring clock type.
#[cfg(target_arch = "wasm32")]
const WASM_VITALS_TICK_INTERVAL: Duration = Duration::from_millis(50);

/// How many [`VITALS_TICK_INTERVAL`] ticks between periodic player saves
/// The measured cadence is 600 ticks, i.e. 30 s at this crate's 20 TPS stand-in.
///
/// **A tick count, not a `Duration`, and that is deliberate.** This crate links
/// into a wasm32 browser bundle where `std::time::Instant::now()` compiles and
/// then panics at runtime under `panic = "abort"` with no log line — three sites
/// in one day. Hanging the cadence off a counter on a timer that already exists
/// means no clock is read at all.
///
/// 30 s rather than the autosave's default: a player file is a few hundred bytes
/// against a region file's megabytes, and the thing being bounded is *how much of
/// a session an alt-F4 costs*, not disk bandwidth.
#[cfg(not(target_arch = "wasm32"))]
const PLAYER_SAVE_EVERY_VITALS_TICKS: u32 = 600;


/// Mirrors effect-driven base-entity flags into the shared player registry
/// before a stream pass. The registry keeps only remote-visible player state;
/// the connection-local effect set remains the timer and gameplay authority.
fn republish_effect_entity_flags(
    players: Option<&PlayerRegistry>,
    ticket: Option<&PlayerTicket>,
    effects: &crate::mob_effects::ActiveEffects,
) {
    let Some((registry, ticket)) = players.zip(ticket) else {
        return;
    };
    let invisible = effects.amplifier_of("minecraft:invisibility").is_some();
    registry.set_shared_flags(ticket.entity_id(), if invisible { 0x20 } else { 0 });
}

/// Runs the player air rule with the same active-effect store that drives the
/// connection's status-effect packets and gameplay consumers.
///
/// Keeping this at the server boundary means [`PlayerVitals`] stays a pure
/// value type: terrain decides submersion and the effect store decides only
/// the hold/refill mode. Both native and browser timer loops call this helper.
fn tick_player_air_supply(
    vitals: &mut PlayerVitals,
    eye_in_water: bool,
    invulnerable: bool,
    effects: &crate::mob_effects::ActiveEffects,
) -> crate::vitals::VitalsTick {
    vitals.tick_with_underwater_breathing(
        eye_in_water && !invulnerable,
        effects.underwater_breathing(),
    )
}

/// Applies the instant-Saturation part of an effect tick through the
/// authoritative food state. Both timer loops use this seam before deciding
/// whether a new `SetHealth` packet is needed.
fn apply_effect_saturation(vitals: &mut PlayerVitals, food_points: u32) -> bool {
    food_points > 0
        && vitals.apply_saturation_effect(
            i32::try_from(food_points).unwrap_or(i32::MAX),
        )
}


/// Which block-entity registry and tick-scheduling feed a connection should
/// route a live placement or a delayed redstone/fluid request through, given
/// `travelled` — this connection's current sibling-dimension source, `None`
/// when it has not travelled at all.
///
/// Select handles from the connection's current dimension. A furnace, lever,
/// or repeater update therefore reaches the registry and delayed-tick feed
/// that belong to the world being viewed; missing sibling handles fall back to
/// the handles passed at join.
///
/// Each field independently falls back to its join-time handle because
/// `crate::chunk::ChunkSource::world_registries`
/// and `crate::chunk::ChunkSource::block_tick_feed` are two different
/// accessors that can in principle disagree, and collapsing them into one
/// `Option` would make a source with only one handle silently lose that half.
/// A `DimensionalSource` built by
/// `crate::integrated::sibling_chunk_source` answers `Some` for both or
/// `None` for neither — see `crate::dimension::DimensionalSource::alone_with_dimension_handles`'s
/// own doc comment — so this test covers the asymmetric-handle choice.
struct DimensionScopedHandles {
    block_entities: Option<BlockEntityHandle>,
    block_ticks: Option<BlockTickFeed>,
}

fn dimension_scoped_handles(travelled: Option<&Arc<dyn ChunkSource>>) -> DimensionScopedHandles {
    DimensionScopedHandles {
        block_entities: travelled
            .and_then(|other| other.world_registries())
            .map(|registries| registries.block_entities),
        block_ticks: travelled.and_then(|other| other.block_tick_feed()),
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
    /// The protocol could not encode a generated chunk column.
    #[error("chunk encoding failed: {0}")]
    ChunkEncode(#[from] ChunkEncodeError),
    /// Initial population terrain could not be prepared.
    #[error("initial population preparation failed: {0}")]
    InitialSeed(ChunkEncodeError),
    /// The client disconnected before completing login.
    #[error("client closed before login completed")]
    ClosedBeforeLogin,
    /// The client did not echo the server's keep-alive challenge before the
    /// next one was due (a fixed 15-second interval, matching vanilla's
    /// own timeout-disconnect-message path —
    /// its own generic per-connection packet listener). Native-only in
    /// practice: nothing constructs this on `wasm32`, since that build never
    /// starts the keep-alive timer in the first place (see
    /// `serve_play`'s doc comment).
    #[error("keep-alive timeout: client did not echo the server's challenge in time")]
    KeepAliveTimeout,
    /// The connection completed a server-list status exchange and was
    /// terminated. **Not a failure**: the status endpoint closes the channel
    /// with reason `multiplayer.status.request_handled` after answering a ping
    /// or receiving a second status request on one connection.
    ///
    /// It is an `Err` rather than an `Ok` only because [`ServeSummary`] is
    /// shaped around a session that logged in: a status connection has no
    /// username, no chunks, and no inventory, so there is nothing truthful to
    /// put in one. Callers discard the result either way (see
    /// [`crate::IntegratedServer`]'s accept loops).
    #[error("server-list status request handled; connection closed (not an error)")]
    StatusRequestHandled,
    /// The client presented a username rejected by [`is_valid_player_name`]
    /// and received a login-phase disconnect explaining the refusal.
    #[error("login rejected: invalid username")]
    InvalidUsername,
    /// The client was refused by the access lists — banned, IP
    /// banned, not whitelisted, or the server was full — and was sent a
    /// login-phase disconnect carrying the refusal message.
    ///
    /// Native-only in practice: `crate::access` is `cfg`-gated off on `wasm32`,
    /// where there is no filesystem to hold the lists and no remote player to
    /// refuse.
    #[error("login rejected: {0}")]
    AccessDenied(String),
    /// The client's RSA-encrypted verify-token echo did not match the
    /// challenge the server generated — either tampering, or a
    /// client answering a stale `EncryptionRequest` after the server moved
    /// on. The mismatch is a hard protocol error, so the connection is
    /// rejected rather than continuing the handshake.
    ///
    /// Not `cfg`-gated, unlike its online-mode siblings below: it names no
    /// native-only type, so a `wasm32` build keeps it available for the same
    /// reason `ServerBound::EncryptionResponse` itself is not gated — the
    /// variant can in principle be decoded on any target, only the request
    /// that would provoke a legitimate reply cannot be sent there.
    #[error("encryption handshake failed: verify token mismatch")]
    VerifyTokenMismatch,
    /// An `EncryptionResponse` (`key` packet) arrived with no matching
    /// `EncryptionRequest` outstanding on this connection — either this
    /// host is not in online mode (always true on `wasm32`, see
    /// [`VerifyTokenMismatch`](Self::VerifyTokenMismatch)'s doc comment), or
    /// the client already completed one handshake. Vanilla's own validation
    /// helper's own "state equals KEY" check is the same guard.
    #[error("encryption handshake failed: no encryption request was outstanding")]
    UnexpectedEncryptionResponse,
    /// The session server says this client never proved ownership of this
    /// username's shared secret (`hasJoined` returned no profile) —
    /// vanilla's `multiplayer.disconnect.unverified_username`.
    #[cfg(not(target_arch = "wasm32"))]
    #[error("login rejected: unverified username")]
    UnverifiedUsername,
    /// The session-server `hasJoined` call itself failed (network error, bad
    /// JSON, unexpected status) — vanilla's
    /// `multiplayer.disconnect.authservers_down`.
    #[cfg(not(target_arch = "wasm32"))]
    #[error("online-mode authentication service error: {0}")]
    AuthServiceUnavailable(#[from] lodestone_auth::AuthError),
    /// A valid chat-session announcement attempted to roll this connection
    /// back to a profile key with an earlier expiry than the installed key.
    #[cfg(not(target_arch = "wasm32"))]
    #[error("chat-session update rejected: replacement profile key expires earlier than the installed key")]
    ProfilePublicKeyRollback,
}


async fn apply<T: Transport>(
    conn: &mut Connection<T>,
    state: &mut State,
    directive: ServerDirective,
) -> Result<(), ServerError> {
    match directive {
        ServerDirective::Send { packet_id, payload } => {
            conn.write_packet_in(*state, packet_id, &payload).await?;
        }
        ServerDirective::SetState(next) => *state = next,
        ServerDirective::SetCompression(threshold) => conn.set_compression(threshold),
        ServerDirective::EnableEncryption(secret) => conn.enable_encryption(&secret)?,
        ServerDirective::None => {}
    }
    Ok(())
}

/// Ends an already-written chunk batch before reporting an encoding failure.
///
/// Callers that only accumulated directives locally pass `None`, so the client
/// never observes an unmatched batch marker. A batch beginning on the wire must
/// always have its matching end marker before the connection is disconnected.
async fn return_chunk_encode_error<T, P, R>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    written_batch_size: Option<i32>,
    error: ChunkEncodeError,
) -> Result<R, ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    if let Some(batch_size) = written_batch_size {
        apply(conn, state, proto.end_chunk_batch(batch_size)).await?;
    }
    apply(
        conn,
        state,
        proto.encode_disconnect(*state, &chunk_encode_failure_reason()),
    )
    .await?;
    Err(ServerError::ChunkEncode(error))
}

async fn return_initial_seed_error<T, P, R>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    written_batch_size: Option<i32>,
    error: ChunkEncodeError,
) -> Result<R, ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    if let Some(batch_size) = written_batch_size {
        apply(conn, state, proto.end_chunk_batch(batch_size)).await?;
    }
    let reason = Text::literal(format!("Failed to prepare world population: {error}"));
    apply(conn, state, proto.encode_disconnect(*state, &reason)).await?;
    Err(ServerError::InitialSeed(error))
}


/// Encodes one complete chunk through the production source-aware initial-
/// chunk path.
///
/// Protocols that retain initial column light use the source's required
/// settled footprint (the complete three-by-three footprint when cross-column
/// light is enabled) before encoding; protocols without that lifecycle use
/// their ordinary dimension-aware encoder.
/// The supplied `column` is the caller's fallback when the source has no
/// resident centre copy. This is the same seam used by the live join path and
/// by persistence parity harnesses, so callers must provide the source that
/// owns the world lifecycle rather than a detached generator.
pub fn encode_chunk_with_source<P: ServerProtocol>(
    proto: &P,
    source: &dyn ChunkSource,
    cx: i32,
    cz: i32,
    column: &ChunkColumn,
) -> Result<ServerDirective, ChunkEncodeError> {
    encode_chunk_with_source_receipt(proto, source, cx, cz, column)
        .map(|encoded| encoded.directive)
}

fn encode_chunk_with_source_receipt<P: ServerProtocol>(
    proto: &P,
    source: &dyn ChunkSource,
    cx: i32,
    cz: i32,
    column: &ChunkColumn,
) -> Result<EncodedColumn, ChunkEncodeError> {
    let dimension = source
        .dimension()
        .unwrap_or(crate::dimension::Dimension::Overworld);
    if !proto.retains_initial_column_light() {
        let packet_column = column_for_initial_encode(column);
        let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketEncoding, 1);
        return proto.try_encode_chunk_in_dimension(cx, cz, &packet_column, dimension)
            .map(|directive| EncodedColumn {
                directive, stage: Some(packet_column.generation_stage()),
            });
    }
    // The initial packet must be based on a complete, settled 3×3 footprint.
    // Prefer the source's current centre copy because another admission may
    // already have installed a newer retained snapshot than the argument held
    // by the streaming queue. The source's settlement transaction validates
    // that this copy remains current after the potentially expensive light
    // computation.
    let neighbour_offsets = light_neighbour_offsets(proto.uses_cross_column_light());
    let mut fallback = resident_column(source, cx, cz).unwrap_or_else(|| column.clone());
    for attempt in 0..=LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES {
        let mut captured_neighbours = Vec::new();
        let exclusive = attempt == LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES;
        let mut compute = |centre: &ChunkColumn, neighbours: &[(i32, i32, &ChunkColumn)]| {
            {
                let _timing = PhaseTimer::start(WorldgenTimingPhase::SnapshotAssembly, 1);
                captured_neighbours = neighbours
                    .iter()
                    .map(|(dx, dz, neighbour)| (*dx, *dz, (*neighbour).clone()))
                    .collect();
            }
            let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketLighting, 1);
            proto.compute_initial_column_lights_with_neighbours_in_dimension(
                centre,
                neighbours,
                dimension,
            )
        };
        match source.settle_resident_column_lights_with_neighbours(
            cx,
            cz,
            &fallback,
            &neighbour_offsets,
            false,
            false,
            exclusive,
            &mut compute,
        ) {
            Ok(centre) => {
                // A persisted centre may already carry a settled light
                // snapshot, so the source transaction can return before the
                // compute callback captures its neighbours. The packet still
                // needs that complete footprint; load it for encoding without
                // replacing the retained centre snapshot.
                if captured_neighbours.is_empty() && !neighbour_offsets.is_empty() {
                    captured_neighbours = neighbour_offsets
                        .iter()
                        .map(|&(dx, dz)| (dx, dz, source.column(cx + dx, cz + dz)))
                        .collect();
                }
                let packet_column = column_for_initial_encode(&centre);
                let neighbour_refs = borrowed_neighbours(&captured_neighbours);
                let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketEncoding, 1);
                return proto.try_encode_chunk_with_neighbours_in_dimension(
                    cx,
                    cz,
                    &packet_column,
                    &neighbour_refs,
                    dimension,
                ).map(|directive| EncodedColumn {
                    directive, stage: Some(packet_column.generation_stage()),
                });
            }
            Err(ColumnLightSettlementError::NoLight) => {
                if captured_neighbours.is_empty() && !neighbour_offsets.is_empty() {
                    captured_neighbours = neighbour_offsets
                        .iter()
                        .map(|&(dx, dz)| (dx, dz, source.column(cx + dx, cz + dz)))
                        .collect();
                }
                let packet_column = column_for_initial_encode(&fallback);
                let neighbour_refs = borrowed_neighbours(&captured_neighbours);
                let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketEncoding, 1);
                return proto.try_encode_chunk_with_neighbours_in_dimension(
                    cx,
                    cz,
                    &packet_column,
                    &neighbour_refs,
                    dimension,
                ).map(|directive| EncodedColumn {
                    directive, stage: Some(packet_column.generation_stage()),
                });
            }
            Err(ColumnLightSettlementError::MissingFootprint) => {
                return Err(ChunkEncodeError::new(
                    "initial retained-light settlement lost its source footprint",
                ));
            }
            Err(ColumnLightSettlementError::Conflict) if !exclusive => {
                // A dependency write completed after capture. Retry from the
                // current source view so the next light snapshot describes the
                // newer blocks rather than the rejected one.
                fallback = resident_column(source, cx, cz).unwrap_or_else(|| column.clone());
            }
            Err(ColumnLightSettlementError::Conflict) => {
                return Err(ChunkEncodeError::new(
                    "initial retained-light settlement remained unstable after the exclusive retry",
                ));
            }
        }
    }
    unreachable!("the bounded initial-light settlement loop always returns")
}

/// Gives a direct initial-packet encoder only a centre-settled retained light
/// snapshot. The source settlement path is the authority for promoting a
/// dependency-initialized column; a `NoLight` result must not let a direct
/// encoder mistake that intermediate storage for the final centre answer.
fn column_for_initial_encode(column: &ChunkColumn) -> ChunkColumn {
    let _timing = PhaseTimer::start(WorldgenTimingPhase::SnapshotAssembly, 1);
    let mut column = column.clone();
    if column.retained_light().is_some() && column.centre_settled_light().is_none() {
        column.clear_retained_light();
    }
    column
}

#[cfg(test)]
fn detached_initial_packet_columns<P: ServerProtocol>(
    proto: &P,
    column: &ChunkColumn,
    neighbours: &[(i32, i32, &ChunkColumn)],
    dimension: crate::dimension::Dimension,
) -> ChunkColumn {
    let mut column = column_for_initial_encode(column);
    if let Some(settlement) = proto.compute_initial_column_lights_with_neighbours_in_dimension(
        &column,
        neighbours,
        dimension,
    ) {
        column.set_retained_light_with_status(
            settlement.centre_light().clone(),
            crate::chunk::RetainedLightStatus::CentreSettled,
        );
    }
    column
}

fn detached_initial_packet_snapshot_columns<P: ServerProtocol>(
    proto: &P,
    snapshot: &crate::worldgen_session::PacketSnapshot,
    neighbours: &[(i32, i32, &ChunkColumn)],
    dimension: crate::dimension::Dimension,
) -> ChunkColumn {
    let settlement = if let Some(settlement) = snapshot.light_settlement() {
        let _timing = PhaseTimer::start(WorldgenTimingPhase::SnapshotAssembly, 1);
        settlement.clone()
    } else {
        let column = column_for_initial_encode(snapshot.column());
        let settlement = {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketLighting, 1);
            proto.compute_initial_column_lights_with_neighbours_in_dimension(
                &column,
                neighbours,
                dimension,
            )
        };
        let Some(settlement) = settlement else {
            return column_for_initial_encode(snapshot.column());
        };
        let _timing = PhaseTimer::start(WorldgenTimingPhase::SnapshotAssembly, 1);
        match snapshot.install_light_settlement(settlement) {
            Ok(()) => snapshot
                .light_settlement()
                .expect("installed packet light settlement")
                .clone(),
            Err(existing) => existing,
        }
    };
    let mut column = column_for_initial_encode(snapshot.column());
    let _timing = PhaseTimer::start(WorldgenTimingPhase::SnapshotAssembly, 1);
    column.set_retained_light_with_status(
        settlement.centre_light().clone(),
        crate::chunk::RetainedLightStatus::CentreSettled,
    );
    column
}

pub fn encode_packet_snapshot_with_protocol<P: ServerProtocol>(
    proto: &P,
    cx: i32,
    cz: i32,
    snapshot: &crate::worldgen_session::PacketSnapshot,
    dimension: crate::dimension::Dimension,
) -> Result<ServerDirective, ChunkEncodeError> {
    if !proto.retains_initial_column_light() {
        let column = column_for_initial_encode(snapshot.column());
        let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketEncoding, 1);
        return proto.try_encode_chunk_in_dimension(
            cx,
            cz,
            &column,
            dimension,
        );
    }
    let neighbours = {
        let _timing = PhaseTimer::start(WorldgenTimingPhase::SnapshotAssembly, 1);
        snapshot
            .neighbours()
            .iter()
            .map(|neighbour| {
                let coordinate = neighbour.coordinate();
                (coordinate.0 - cx, coordinate.1 - cz, neighbour.column())
            })
            .collect::<Vec<_>>()
    };
    let column = detached_initial_packet_snapshot_columns(proto, snapshot, &neighbours, dimension);
    let _timing = PhaseTimer::start(WorldgenTimingPhase::PacketEncoding, 1);
    proto.try_encode_chunk_with_neighbours_in_dimension(
        cx,
        cz,
        &column,
        &neighbours,
        dimension,
    )
}

fn borrowed_neighbours(
    neighbours: &[(i32, i32, ChunkColumn)],
) -> Vec<(i32, i32, &ChunkColumn)> {
    neighbours
        .iter()
        .map(|(dx, dz, column)| (*dx, *dz, column))
        .collect()
}

const LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES: usize = 3;

fn light_neighbour_offsets(cross_column: bool) -> Vec<(i32, i32)> {
    if !cross_column {
        return Vec::new();
    }
    (-1..=1)
        .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
        .filter(|&(dx, dz)| (dx, dz) != (0, 0))
        .collect()
}

/// Coordinates that a synchronous consumer may touch after admitting one
/// centre. Radius one covers the target column, a face-adjacent placement, and
/// the complete retained-light neighbourhood used by initial packet encoding.
fn column_admission_footprint(cx: i32, cz: i32, radius: i32) -> Vec<(i32, i32)> {
    (-radius..=radius)
        .flat_map(|dz| (-radius..=radius).map(move |dx| (cx + dx, cz + dz)))
        .collect()
}

/// The part of [`column_admission_footprint`] still missing after a streaming
/// worker has already produced the centre column.
///
/// Initial retained-light encoding needs the complete three-by-three footprint,
/// but the join pipeline hands this path a fully generated centre. Asking the
/// source for that centre again is redundant work on sources that do not retain
/// generated columns, and a needless resident-cache lookup on sources that do.
/// Preserve the footprint's row-major ordering while excluding only `(cx, cz)`.
fn column_admission_neighbours(cx: i32, cz: i32, radius: i32) -> Vec<(i32, i32)> {
    column_admission_footprint(cx, cz, radius)
        .into_iter()
        .filter(|&pos| pos != (cx, cz))
        .collect()
}

/// Returns the block position whose column an inbound world action owns.
///
/// The packet branch awaits this broker before entering its existing
/// synchronous handlers. A packet is never discarded when the source is cold;
/// admission only establishes the ordering that lets the handler run without
/// making a generation call on the connection task.
fn action_target(packet: &ServerBound) -> Option<BlockPos> {
    match packet {
        ServerBound::BlockAction { pos, .. }
        | ServerBound::UseItemOn { pos, .. }
        | ServerBound::SetCommandBlock { pos, .. }
        | ServerBound::PickItemFromBlock { pos, .. } => Some(*pos),
        _ => None,
    }
}

/// Admits the owned action footprint before the packet handler reads or
/// mutates terrain. This is intentionally one awaited operation in front of
/// the `match packet`: packet order is therefore preserved even when a cold
/// source takes a worker turn to generate its target and neighbours.
async fn admit_action_footprint<S: ChunkSource + 'static>(
    source: SourceRef<'_, S>,
    packet: &ServerBound,
) -> Result<(), ChunkEncodeError> {
    let Some(pos) = action_target(packet) else {
        return Ok(());
    };
    source
        .admit_columns(column_admission_footprint(
            pos.x.div_euclid(16),
            pos.z.div_euclid(16),
            1,
        ))
        .await
}

async fn encode_column<P: ServerProtocol, S: ChunkSource + 'static>(
    proto: &P,
    source: SourceRef<'_, S>,
    cx: i32,
    cz: i32,
    trace: Option<&JoinTrace>,
    payload: crate::join_scheduler::ColumnPayload,
) -> Result<EncodedColumn, ChunkEncodeError> {
    let payload = match payload {
        crate::join_scheduler::ColumnPayload::Encoded(directive) => {
            crate::join_scheduler::ColumnPayload::Encoded(directive)
        }
        crate::join_scheduler::ColumnPayload::Snapshot(snapshot) => {
            crate::join_scheduler::ColumnPayload::Snapshot(snapshot)
        }
        crate::join_scheduler::ColumnPayload::Column(column) => {
            let column = match source
                .get()
                .packet_generation_stage(column.generation_stage())
            {
                Some(required) if required > column.generation_stage() => {
                    source
                        .generate(vec![(cx, cz)])
                        .await?
                        .into_iter()
                        .next()
                        .ok_or_else(|| ChunkEncodeError::new("packet admission returned no column"))?
                }
                _ => column,
            };
            crate::join_scheduler::ColumnPayload::Column(column)
        }
    };
    if matches!(&payload, crate::join_scheduler::ColumnPayload::Column(_))
        && (proto.uses_cross_column_light() || proto.retains_initial_column_light())
    {
        source
            .admit_columns(column_admission_neighbours(
                cx,
                cz,
                i32::from(proto.uses_cross_column_light()),
            ))
            .await?;
    }
    match payload {
        crate::join_scheduler::ColumnPayload::Encoded(directive) => {
            Ok(EncodedColumn { directive, stage: None })
        }
        crate::join_scheduler::ColumnPayload::Column(column) => {
            let directive = match try_encode_initial_column(
                proto, source.get(), cx, cz, |coordinates| source.generate(coordinates),
            ).await? {
                Some(encoded) => Ok(encoded),
                None => encode_chunk_with_source_receipt(proto, source.get(), cx, cz, &column),
            };
            if directive.is_ok() {
                if let Some(trace) = trace {
                    trace.mark("encoded", cx, cz);
                }
            }
            directive
        }
        crate::join_scheduler::ColumnPayload::Snapshot(snapshot) => {
            let directive = encode_packet_snapshot_with_protocol(
                proto,
                cx,
                cz,
                &snapshot,
                source.dimension(),
            )?;
            if let Some(trace) = trace {
                trace.mark("encoded", cx, cz);
            }
            Ok(EncodedColumn { directive, stage: Some(snapshot.column().generation_stage()) })
        }
    }
}

async fn try_encode_initial_column<P, F, Fut>(
    proto: &P,
    source: &dyn ChunkSource,
    cx: i32,
    cz: i32,
    admit: F,
) -> Result<Option<EncodedColumn>, ChunkEncodeError>
where
    P: ServerProtocol,
    F: Fn(Vec<(i32, i32)>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<ChunkColumn>, ChunkEncodeError>>,
{
    use crate::chunk::ResidentLightTransactionError as Error;

    let Some(prepare) = proto.detached_initial_packet_prepare() else {
        return Ok(None);
    };
    let offsets = light_neighbour_offsets(proto.uses_cross_column_light());
    let dimension = source.dimension().unwrap_or(crate::dimension::Dimension::Overworld);
    loop {
        let mut transaction = match source.try_begin_initial_packet(cx, cz, &offsets) {
            None => return Ok(None),
            Some(Ok(transaction)) => transaction,
            Some(Err(Error::Busy | Error::Conflict)) => {
                defer_initial_packet().await;
                continue;
            }
            Some(Err(Error::MissingFootprint)) => {
                admit(column_admission_footprint(
                    cx, cz, i32::from(proto.uses_cross_column_light()),
                )).await?;
                continue;
            }
            Some(Err(Error::InvalidOutputs)) => {
                return Err(ChunkEncodeError::new("invalid initial packet footprint"));
            }
        };
        let input = {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::SnapshotAssembly, 1);
            let columns = transaction.columns();
            let column = columns.iter().find(|(x, z, _)| (*x, *z) == (cx, cz))
                .ok_or_else(|| ChunkEncodeError::new("initial packet capture omitted its centre"))?
                .2.clone();
            let neighbours = offsets.iter().map(|&(dx, dz)| {
                columns.iter().find(|(x, z, _)| (*x, *z) == (cx + dx, cz + dz))
                    .map(|(_, _, column)| (dx, dz, column.clone()))
                    .ok_or_else(|| ChunkEncodeError::new("initial packet capture omitted a dependency"))
            }).collect::<Result<Vec<_>, _>>()?;
            crate::initial_packet::InitialPacketInput {
                coordinate: (cx, cz), dimension, column, neighbours,
            }
        };
        let prepared = crate::join_scheduler::prepare_owned_initial_packet(prepare.clone(), input).await?;
        // The protocol sizes light to the wire dimension, while retained light
        // must match its column's own storage height. A column whose height
        // differs from its wire dimension (a plugin dimension served under a
        // standard dimension's framing) cannot retain that light, so it takes
        // the ordinary encode instead of failing the join.
        if let Some(settlement) = prepared.settlement.as_ref() {
            let retainable = settlement.iter().all(|(offset, light)| {
                transaction.columns().iter()
                    .find(|(x, z, _)| (*x, *z) == (cx + offset.0, cz + offset.1))
                    .is_some_and(|(_, _, column)| light.light_section_count() == column.section_count() + 2)
            });
            if !retainable {
                return Ok(None);
            }
        }
        loop {
            match transaction.try_commit(prepared.settlement.as_ref()) {
                Ok(()) => return Ok(Some(EncodedColumn {
                    directive: prepared.directive, stage: Some(prepared.stage),
                })),
                Err(Error::Busy) => defer_initial_packet().await,
                Err(Error::Conflict | Error::MissingFootprint) => break,
                Err(Error::InvalidOutputs) => {
                    return Err(ChunkEncodeError::new("invalid initial packet light settlement"));
                }
            }
        }
        defer_initial_packet().await;
    }
}

async fn defer_initial_packet() {
    #[cfg(target_arch = "wasm32")]
    lodestone_time::browser_yield().await;
    #[cfg(not(target_arch = "wasm32"))]
    tokio::task::yield_now().await;
}

async fn encode_column_owned<P: ServerProtocol>(
    proto: &P,
    source: Arc<dyn ChunkSource>,
    cx: i32,
    cz: i32,
    trace: Option<Arc<JoinTrace>>,
    payload: crate::join_scheduler::ColumnPayload,
) -> Result<EncodedColumn, ChunkEncodeError> {
    let payload = match payload {
        crate::join_scheduler::ColumnPayload::Encoded(directive) => {
            crate::join_scheduler::ColumnPayload::Encoded(directive)
        }
        crate::join_scheduler::ColumnPayload::Snapshot(snapshot) => {
            crate::join_scheduler::ColumnPayload::Snapshot(snapshot)
        }
        crate::join_scheduler::ColumnPayload::Column(column) => {
            if let Some(encoded) = try_encode_initial_column(
                proto, &*source, cx, cz,
                |coordinates| crate::join_scheduler::generate_owned_columns(Arc::clone(&source), coordinates),
            ).await? {
                if let Some(trace) = trace.as_ref() { trace.mark("encoded", cx, cz); }
                return Ok(encoded);
            }
            let column = match source.packet_generation_stage(column.generation_stage()) {
                Some(required) if required > column.generation_stage() => crate::join_scheduler::generate_owned_columns(
                    Arc::clone(&source),
                    vec![(cx, cz)],
                )
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| ChunkEncodeError::new("packet admission returned no column"))?,
                _ => column,
            };
            crate::join_scheduler::ColumnPayload::Column(column)
        }
    };
    if matches!(&payload, crate::join_scheduler::ColumnPayload::Column(_))
        && (proto.uses_cross_column_light() || proto.retains_initial_column_light())
    {
        let _ = crate::join_scheduler::generate_owned_columns(
            Arc::clone(&source),
            column_admission_neighbours(
                cx,
                cz,
                i32::from(proto.uses_cross_column_light()),
            ),
        )
        .await?;
    }
    match payload {
        crate::join_scheduler::ColumnPayload::Encoded(directive) => {
            Ok(EncodedColumn { directive, stage: None })
        }
        crate::join_scheduler::ColumnPayload::Column(column) => {
            #[cfg(not(target_arch = "wasm32"))]
            let directive = if let Some(encode) = proto.detached_source_encode() {
                let handle = crate::worldgen_dispatch::spawn(move || {
                    encode(&*source, cx, cz, &column)
                })
                .await;
                handle.await.map_err(|_| {
                    ChunkEncodeError::new("detached source encode worker ended without a result")
                })?.map(|directive| EncodedColumn { directive, stage: None })
            } else {
                encode_chunk_with_source_receipt(proto, &*source, cx, cz, &column)
            };
            #[cfg(target_arch = "wasm32")]
            let directive = encode_chunk_with_source_receipt(proto, &*source, cx, cz, &column);
            if directive.is_ok()
                && let Some(trace) = trace.as_ref()
            {
                trace.mark("encoded", cx, cz);
            }
            directive
        }
        crate::join_scheduler::ColumnPayload::Snapshot(snapshot) => {
            let stage = snapshot.column().generation_stage();
            let dimension = source
                .dimension()
                .unwrap_or(crate::dimension::Dimension::Overworld);
            let directive = if let Some(encode) = proto.detached_packet_encode() {
                crate::join_scheduler::encode_owned_packet_snapshot(
                    encode, cx, cz, snapshot, dimension,
                )
                .await
            } else {
                encode_packet_snapshot_with_protocol(proto, cx, cz, &snapshot, dimension)
            }?;
            if let Some(trace) = trace.as_ref() {
                trace.mark("encoded", cx, cz);
            }
            Ok(EncodedColumn { directive, stage: Some(stage) })
        }
    }
}

/// A shared feed of server-initiated resource pack pushes — the
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePackResponseRecord {
    /// Id of the resource pack this response concerns.
    pub id: uuid::Uuid,
    /// Outcome reported by the client.
    pub response: ResourcePackResponseKind,
}

#[derive(Debug, Clone, Default)]
pub struct ResourcePackPushFeed(
    Arc<Mutex<Vec<ResourcePackPush>>>,
    Arc<Mutex<Vec<ResourcePackResponseRecord>>>,
);

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

    /// Records a response received from a client for host-side policy or
    /// telemetry. Recording does not enforce acceptance or disconnect on any
    /// particular outcome.
    pub fn record_response(&self, response: ResourcePackResponseRecord) {
        self.1
            .lock()
            .expect("resource pack response feed lock poisoned")
            .push(response);
    }

    /// Drains responses received since the last call.
    pub fn drain_responses(&self) -> Vec<ResourcePackResponseRecord> {
        std::mem::take(
            &mut *self
                .1
                .lock()
                .expect("resource pack response feed lock poisoned"),
        )
    }
}

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

/// Shared implementation for the connection wrappers. Feed-carrying wrappers
/// pass live handles; compatibility wrappers pass defaults. [`SourceRef`]
/// selects borrowed or shared chunk generation without changing packet flow.
#[allow(clippy::too_many_arguments)]
async fn serve_connection_inner<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    entities: &E,
    view_radius: i32,
    // The largest radius this connection may request; it is separate from the
    // `view_radius` used for the initial join. Compatibility wrappers use the
    // join radius, while integrated hosts supply their configured ceiling.
    max_view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    // Chunk-ticket state shared with the store that owns the connection's
    // loaded columns. A default handle gives compatibility wrappers isolated
    // ticket state; integrated hosts pass `ChunkStore::tickets()`.
    tickets: &TicketStoreHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    // World-tick weather transitions drained by `serve_play`'s
    // `container_sync_tick` arm. A default feed produces no transitions.
    weather: &WeatherFeed,
    // Night-skip votes recorded by packet dispatch and consumed by the world
    // tick loop. A default vote/feed pair leaves night skipping disabled.
    sleep_vote: &SleepVote,
    // Night-skip notifications drained by `serve_play` into `encode_set_time`.
    sleep_feed: &SleepFeed,
    // Command handlers. `CommandDispatch::none()` provides fail-closed
    // behavior for wrappers without a host command dispatcher.
    commands: &CommandDispatch,
    // World border state used for join snapshots and per-tick border damage.
    // A default feed describes an unconfigured border.
    border: &BorderFeed,
    // Server-initiated resource-pack pushes drained by `serve_play`.
    resource_packs: &ResourcePackPushFeed,
    // Wire-level plugin messaging handlers and the server-to-client broadcast
    // queue, drained by `serve_play` alongside resource-pack pushes.
    plugin_channels: &PluginChannelRegistry,
    // Shared game rules, difficulty, and world clock. Integrated hosts pass the
    // handle updated by their world tick; isolated wrappers use a default.
    world: &crate::world_state::WorldStateHandle,
    // Continuously refreshed player-save mirror published by `serve_play`.
    // Integrated shutdown reads the shared slot; isolated wrappers use a
    // default slot.
    live_save: &crate::live_save::LiveSaveSlot,
    // Ops, whitelist, and ban lists consulted at `LoginStart`. A default access
    // handle admits everyone and grants no operator role; hosts may provide
    // configured lists.
    #[cfg(not(target_arch = "wasm32"))] access: &crate::access::AccessHandle,
    // The remote address this connection came from, for the IP ban list. `None`
    // for an in-memory duplex, which has no address — and an IP ban therefore
    // cannot apply to singleplayer, which is correct rather than a gap.
    #[cfg(not(target_arch = "wasm32"))] peer_ip: Option<std::net::IpAddr>,
    // `None` selects offline mode and sends `login_success` directly. `Some`
    // selects the encryption handshake and sends `login_success` after the
    // session server confirms the client's identity.
    #[cfg(not(target_arch = "wasm32"))] online_mode: Option<&OnlineModeConfig>,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    conn.set_outbound_ids(proto.outbound_id_map());
    let mut state = State::Handshaking;
    let mut username: Option<String> = None;
    // Keep the player entity UUID alongside `username`; it must match the UUID
    // echoed by `login_success` so clients resolve the spawned player correctly.
    let mut login_uuid: Option<uuid::Uuid> = None;
    // A configured online-mode listener is not enough: only this flag, set
    // after `hasJoined` replaces the claimed identity, authorizes profile-key
    // provenance enforcement in Play.
    #[cfg(not(target_arch = "wasm32"))]
    let mut online_authenticated = false;
    // `Some` while an encryption response is outstanding: retain the RSA
    // keypair needed to decrypt it and the verify-token challenge it must echo.
    // The response arm consumes this value, so a second response finds no
    // outstanding challenge and cannot reuse a keypair.
    #[cfg(not(target_arch = "wasm32"))]
    let mut pending_encryption: Option<(ServerKeyPair, [u8; lodestone_net::VERIFY_TOKEN_LEN])> =
        None;
    let mut streamer = EntityStreamer::default();
    let mut player_list = PlayerListStreamer::default();
    // Vanilla's own status packet listener's own "has requested status"
    // field: one status reply per
    // connection, a second request is a disconnect.
    let mut status_requested = false;
    // This connection's declared channel support, populated from
    // its `minecraft:register`/`minecraft:unregister` custom payloads — first
    // during Configuration (the arm below), then in Play via the same
    // `ServerBound::CustomPayload` arm in `dispatch_play_packet`. It is the
    // per-connection filter the broadcast drain in `serve_play` applies.
    let mut client_channels = ClientChannels::default();
    // The mode this connection joins in, absent a saved per-player value
    // below: `WorldStateHandle::default_game_mode`, `/defaultgamemode`'s own
    // read side — Survival until a host changes it. A runtime switch (the
    // `change_game_mode` packet, or `/gamemode`) moves it from there.
    // `serve_play` takes ownership of it at the Play handoff.
    let game_mode = world.default_game_mode();

    // A legacy protocol can finish login without receiving the modern
    // configuration acknowledgements. Queue the same play-transition event
    // used by the wire path so both routes share the complete join sequence.
    let mut pending_event: Option<ServerBound> = None;
    loop {
        let event = if let Some(event) = pending_event.take() {
            event
        } else {
            let Some((packet_id, payload)) = conn.read_packet().await? else {
                break;
            };
            proto.decode(state, packet_id, &payload)
        };

        match event {
            ServerBound::Handshake { next_state } => {
                state = next_state;
            }
            // Status requests are one-shot per connection: answer the first
            // request and disconnect on any subsequent one. Repeating requests
            // would let a peer hold the connection open without useful work.
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
            // Ping requests echo the payload, then close. A ping does not
            // require a preceding status request.
            ServerBound::PingRequest { time } => {
                apply(conn, &mut state, proto.encode_pong_response(time)).await?;
                return Err(ServerError::StatusRequestHandled);
            }
            ServerBound::LoginStart {
                username: name,
                uuid,
            } => {
                // Validate the username at login start and send a reason when
                // it fails. Offline-mode UUIDs derive from the username and
                // player data is persisted under that UUID, so control
                // characters must not reach storage.
                //
                // Not merely cosmetic: an offline-mode server derives the account
                // uuid from the username and persists player data under it, so a
                // name carrying control characters is a name that reaches storage.
                if !is_valid_player_name(&name) {
                    let directive = proto.encode_disconnect(state, &invalid_username_reason());
                    apply(conn, &mut state, directive).await?;
                    return Err(ServerError::InvalidUsername);
                }
                // Apply access-list checks after username validation and before
                // `login_success`, so a refused player never reaches
                // Configuration. The online-player count is `0` because this
                // per-connection loop has no cross-connection registry; the
                // player limit is therefore inert while bans and whitelists
                // remain active.
                #[cfg(not(target_arch = "wasm32"))]
                if let Err(refusal) = access.may_join(uuid, peer_ip, 0) {
                    let reason = Text::literal(refusal.message());
                    let directive = proto.encode_disconnect(state, &reason);
                    apply(conn, &mut state, directive).await?;
                    return Err(ServerError::AccessDenied(refusal.message()));
                }
                username = Some(name.clone());
                login_uuid = Some(uuid);

                // online mode sends an encryption request instead
                // of finishing login now — `login_success` is deferred to the
                // `EncryptionResponse` arm below, once the session server has
                // confirmed the client's identity. `encode_encryption_request`
                // returning `ServerDirective::None` means this protocol has no
                // wire support for it (the default every implementor but
                // `V770ServerProtocol` gets), so that also falls back to an
                // offline login rather than sending a request nothing would
                // answer.
                #[cfg(not(target_arch = "wasm32"))]
                let sent_encryption_request = if online_mode.is_some() {
                    let keypair = ServerKeyPair::generate()?;
                    let verify_token = generate_verify_token();
                    let directive =
                        proto.encode_encryption_request(keypair.public_key_der(), &verify_token);
                    if matches!(directive, ServerDirective::None) {
                        false
                    } else {
                        apply(conn, &mut state, directive).await?;
                        pending_encryption = Some((keypair, verify_token));
                        true
                    }
                } else {
                    false
                };
                #[cfg(target_arch = "wasm32")]
                let sent_encryption_request = false;

                if !sent_encryption_request {
                    for directive in proto.login_success(&name, uuid) {
                        apply(conn, &mut state, directive).await?;
                    }
                    if !proto.has_configuration_phase() {
                        state = State::Configuration;
                        pending_event = Some(ServerBound::ConfigurationFinished);
                    }
                }
            }
            // Handle the client's answer to the encryption challenge sent by
            // `LoginStart`. Everything up to and including this
            // packet travels in the clear; `ServerDirective::EnableEncryption`
            // below must be applied before anything is sent in reply, or the
            // two sides disagree about which layer started where —
            // `ServerDirective::EnableEncryption`'s own doc comment names the
            // same ordering hazard `SetCompression` already documents for
            // itself.
            #[cfg(not(target_arch = "wasm32"))]
            ServerBound::EncryptionResponse {
                shared_secret,
                verify_token,
            } => {
                let Some(cfg) = online_mode else {
                    return Err(ServerError::UnexpectedEncryptionResponse);
                };
                let Some((keypair, expected_token)) = pending_encryption.take() else {
                    return Err(ServerError::UnexpectedEncryptionResponse);
                };
                let decrypted_token = keypair.decrypt(&verify_token)?;
                if decrypted_token != expected_token {
                    return Err(ServerError::VerifyTokenMismatch);
                }
                let secret = keypair.decrypt(&shared_secret)?;
                apply(conn, &mut state, ServerDirective::EnableEncryption(secret.clone())).await?;

                // Vanilla's server-id is always the empty string
                // (its own generic login-phase packet listener's own server-id field); the hash is
                // taken over it, the secret, and the exact public-key DER
                // bytes the client encrypted against.
                let hash = lodestone_auth::server_hash("", &secret, keypair.public_key_der());
                let name = username.clone().unwrap_or_default();
                match (cfg.verify)(cfg.http.clone(), name, hash).await {
                    Ok(Some(profile)) => {
                        login_uuid = Some(profile.id);
                        username = Some(profile.name.clone());
                        online_authenticated = true;
                        for directive in proto.login_success(&profile.name, profile.id) {
                            apply(conn, &mut state, directive).await?;
                        }
                        if !proto.has_configuration_phase() {
                            state = State::Configuration;
                            pending_event = Some(ServerBound::ConfigurationFinished);
                        }
                    }
                    Ok(None) => {
                        let directive =
                            proto.encode_disconnect(state, &unverified_username_reason());
                        apply(conn, &mut state, directive).await?;
                        return Err(ServerError::UnverifiedUsername);
                    }
                    Err(error) => {
                        let directive =
                            proto.encode_disconnect(state, &auth_servers_down_reason());
                        apply(conn, &mut state, directive).await?;
                        return Err(ServerError::AuthServiceUnavailable(error));
                    }
                }
            }
            // Online-mode encryption is native-only (`lodestone-net::crypto`'s
            // and `lodestone-auth`'s own doc comments): a `wasm32` build never
            // sends an `EncryptionRequest` in the first place (`LoginStart`'s
            // `sent_encryption_request` is unconditionally `false` there), so
            // a real client answering one it was never sent is a protocol
            // violation exactly like the native "nothing outstanding" case.
            #[cfg(target_arch = "wasm32")]
            ServerBound::EncryptionResponse { .. } => {
                return Err(ServerError::UnexpectedEncryptionResponse);
            }
            ServerBound::LoginAcknowledged => {
                state = State::Configuration;
                // Send the registries needed to resolve the dimension and
                // world-clock holders — the
                // `dimension_type` ids `login`/`respawn` carry, the
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
                let t_cfg = JoinStopwatch::now();
                // The world spawn point is a *search*, not a fixed local `(8, 8)`
                // in the origin column. `world_spawn::find_initial_spawn` walks
                // a ±5-chunk spiral and selects the first chunk with a valid
                // surface. A plains origin yields `(8, y, 8)`, while an invalid
                // origin moves the spawn to the nearest valid chunk instead of
                // stranding the player under water.
                // **Read the world's own spawn first.** The spiral runs at world
                // creation and persists to `level.dat`; a join reuses that value.
                // A missing value triggers the search, avoiding a repeated
                // 121-column search on every connection and allowing
                // `/setworldspawn` to persist.
                //
                // `None` here means "no search has happened for this world yet",
                // which is exactly a fresh world; the first join resolves it and the
                // next autosave writes it. See
                // `WorldStateHandle::world_spawn`.
                let spawn = match world.world_spawn() {
                    Some(stored)
                        if crate::world_spawn::is_spawn_position_clear(
                            source.get(),
                            crate::world_spawn::player_position_for_spawn_anchor(stored.pos),
                        ) =>
                    {
                        stored
                    }
                    Some(stored) => {
                        tracing::warn!(
                            position = ?stored.pos,
                            "stored world spawn is obstructed; searching for a clear replacement"
                        );
                        let found = source.find_initial_spawn().await;
                        world.set_world_spawn(found);
                        found
                    }
                    None => {
                        world
                            .resolve_world_spawn(|| source.find_initial_spawn())
                            .await
                    }
                };
                // Level data stores the world spawn as a block anchor. The
                // entity must enter at that block's bottom centre, matching
                // the collision probe used while selecting the anchor; using
                // the integer corner lets the 0.6-wide body overlap a
                // neighbouring column and can place it inside terrain.
                let spawn_position =
                    crate::world_spawn::player_position_for_spawn_anchor(spawn.pos);

                let home_tickets = source.get().ticket_store().unwrap_or_else(|| tickets.clone());
                let spawn_chunk = (
                    (spawn.pos.x / 16.0).floor() as i32,
                    (spawn.pos.z / 16.0).floor() as i32,
                );
                home_tickets.set_ticket_with_radius(
                    TicketOwner::Spawn,
                    TicketKind::PlayerSpawn,
                    spawn_chunk,
                    PLAYER_SPAWN_RADIUS,
                );
                source.get().reconcile_ticket_residency();

                // this player's own saved state, if this world has
                // any. Reached through `ChunkSource::world_registries` rather
                // than a new parameter — see `crate::chunk::WorldRegistries`'s
                // `player_data` field for why that routing was chosen over
                // threading a 31st argument through eleven wrappers.
                //
                // An in-memory world answers `None` and every existing caller
                // therefore behaves exactly as before.
                let home_source = source;
                #[cfg(not(target_arch = "wasm32"))]
                let saved_player = player_store(source.get()).and_then(|store| {
                    match store.read(login_uuid.unwrap_or_default()) {
                        Ok(data) => data,
                        // Logged, never swallowed. A save we cannot read is not a
                        // player with no save: joining them empty-handed would
                        // overwrite the file they still own on the first
                        // autosave, which is the one outcome that loses the
                        // inventory irrecoverably.
                        Err(err) => {
                            tracing::error!(
                                "player data for {:?} could not be read and will NOT be \
                                 overwritten this session: {err}",
                                login_uuid,
                            );
                            None
                        }
                    }
                });
                #[cfg(target_arch = "wasm32")]
                let saved_player: Option<()> = None;
                #[cfg(not(target_arch = "wasm32"))]
                let mut native_player = NativePlayerSession::load(
                    source.get(),
                    login_uuid.unwrap_or_default(),
                );
                #[cfg(not(target_arch = "wasm32"))]
                let restored_dimension_source = native_player.as_mut().and_then(|native| {
                    let dimension = native.dimension()?;
                    let protocol_supports_dimension = !proto
                        .encode_dimension_change_with_teleport_id(
                            1,
                            dimension.key(),
                            spawn_position,
                            game_mode,
                        )
                        .is_empty();
                    native.restored_source(source.get(), protocol_supports_dimension)
                });
                #[cfg(not(target_arch = "wasm32"))]
                let source = restored_dimension_source
                    .as_ref()
                    .map_or(source, SourceRef::Dimension);

                // Where the player actually re-enters the world. `spawn.pos`
                // stays the **world** spawn: it is what `serve_play` uses for a
                // respawn, and overwriting it with the player's last position
                // would respawn a dead player back where they died. See
                // `join_position_for_saved_player`'s own doc comment for why a
                // saved position is not always trusted verbatim.
                #[cfg(not(target_arch = "wasm32"))]
                let join_pos = {
                    let anvil_pos =
                        join_position_for_saved_player(saved_player.as_ref(), spawn_position);
                    let candidate = native_player
                        .as_ref()
                        .map_or(anvil_pos, |native| native.join_position(anvil_pos));
                    if saved_player.is_some() || native_player.is_some() {
                        // A restored Overworld position may have been written by
                        // an older session before terrain clearance was enforced.
                        // Re-read the live source before trusting it; a saved
                        // position inside a wall falls back to the verified world
                        // spawn. A locator restored into a sibling dimension is
                        // deliberately skipped because its source is not the home
                        // Overworld terrain.
                        source
                            .admit_columns(column_admission_footprint(
                                (candidate.x.floor() as i32).div_euclid(16),
                                (candidate.z.floor() as i32).div_euclid(16),
                                1,
                            ))
                            .await?;
                        if restored_dimension_source.is_none()
                            && !crate::world_spawn::is_spawn_position_clear(source.get(), candidate)
                        {
                            tracing::warn!(
                                position = ?candidate,
                                fallback = ?spawn_position,
                                "saved player position is obstructed; using the world spawn"
                            );
                            spawn_position
                        } else {
                            candidate
                        }
                    } else {
                        spawn_position
                    }
                };
                #[cfg(target_arch = "wasm32")]
                let join_pos = spawn_position;
                // Vanilla's own player-game-type field, restored — a player who typed
                // `/gamemode survival` and quit comes back in survival. Shadowed
                // rather than assigned so a world with no save keeps the mode the
                // host opened with.
                #[cfg(not(target_arch = "wasm32"))]
                let game_mode = saved_player
                    .as_ref()
                    .and_then(|data| data.game_mode)
                    .or_else(|| {
                        native_player
                            .as_ref()
                            .and_then(NativePlayerSession::game_mode)
                    })
                    .unwrap_or(game_mode);

                state = State::Play;
                #[cfg(not(target_arch = "wasm32"))]
                let restored_other_dimension = restored_dimension_source.is_some();
                #[cfg(target_arch = "wasm32")]
                let restored_other_dimension = false;
                let initial_teleport_id = proto
                    .uses_teleport_acknowledgements()
                    .then_some(i32::from(restored_other_dimension));
                for directive in proto.begin_play_at_with_teleport_id(
                    view_radius,
                    join_pos,
                    game_mode,
                    0,
                ) {
                    apply(conn, &mut state, directive).await?;
                }
                if restored_other_dimension {
                    for directive in proto.encode_dimension_change_with_teleport_id(
                        initial_teleport_id.unwrap_or(0),
                        source.dimension().key(),
                        join_pos,
                        game_mode,
                    ) {
                        apply(conn, &mut state, directive).await?;
                    }
                }
                // Vanilla's own "place new player" step sends the abilities
                // packet right after the login packet, and it is not optional:
                // the login packet's `game_type` tells the client *what* mode it
                // is in, while flight permission and instant build live only
                // here. Sending one without the other is how "creative mode that
                // cannot fly" happens.
                apply(
                    conn,
                    &mut state,
                    proto.encode_player_abilities(Abilities::for_mode(game_mode)),
                )
                .await?;

                // The Brigadier command tree. **Nothing sent this before, so the
                // client had no autocomplete and no command highlighting at all** —
                // `lodestone-shell`'s chat box, its `CommandTreeCell` and the whole
                // suggestion UX were complete and starved of input.
                //
                // Position taken from vanilla's own "place new player" step, which calls
                // its own "send player permission level" step — the method that sends the tree —
                // after the abilities packet and before its own "send level info" step. So it goes
                // here: after abilities above, before the clock sync below, and
                // before any chunk goes out. Appending it after chunk streaming
                // would have been easier (the `CommandSession` that owns the tree
                // is built down there) and it is the wrong place.
                //
                // The tree is pruned to this connection's own permission level, as
                // vanilla's own "send commands" step prunes with
                // its own "fill usable commands" helper: a level-0 player is not sent `/gamemode`'s
                // node, which is what stops the client suggesting a command the
                // server will refuse.
                //
                // `login_uuid` cannot be `None` here: reaching Play requires
                // `ConfigurationFinished`, which follows a successful `LoginStart`
                // (or its online-mode login completion) either through
                // `LoginAcknowledged` for modern protocols or through the queued
                // legacy transition. The `unwrap_or_default` is a total fallback
                // rather than a panic because a nil uuid resolves to no player
                // and therefore no permissions — failing closed, not open. On
                // `wasm32` there is no `AccessHandle` in this signature at all
                // (the whole ops/whitelist/ban feature is native-only). The
                // browser build uses level 4 for its local world so the built-in
                // game-mode command remains available there.
                //
                // All three bindings are consumed again by the `CommandSession`
                // further down; see its own comment for why reuse rather than a
                // second construction.
                let player_uuid = login_uuid.unwrap_or_default();
                #[cfg(not(target_arch = "wasm32"))]
                let permission_level = access.command_permission_level(player_uuid);
                #[cfg(target_arch = "wasm32")]
                let permission_level = 4;
                let builtins = crate::commands::ServerCommands::new();
                apply(
                    conn,
                    &mut state,
                    proto.encode_commands(&builtins.wire_tree_for(permission_level)),
                )
                .await?;

                // Full clock sync at join. Send the **world's** clock, not `(0, 0)`;
                // a world loaded from
                // disk starts at the value returned by
                // `WorldStateHandle::load_level_data`.
                let joined_at = world.time();
                apply(
                    conn,
                    &mut state,
                    proto.encode_set_time(joined_at.game_time, Some(joined_at.day_time)),
                )
                .await?;

                apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                // Generate columns with a bounded worker window. The ring order
                // is a pure function of `view_radius`, so worker completion order
                // cannot change the encoded byte sequence or RNG-derived content.
                // Shared sources perform generation and encoding in blocking
                // workers; the connection task emits the resulting frames.
                //
                // The inner `JOIN_PRESTREAM_RADIUS` rings are sent first so the
                // player's column arrives after one generated column. Remaining
                // columns flow through `JoinChunkStream` while the play loop
                // dispatches packets. One begin/end chunk-batch pair covers the
                // complete stream, preserving client flow-control accounting.
                //
                // `join_view_rings` yields offsets `(dx, dz)`, so add the
                // player's absolute chunk `(join_cx, join_cz)` before encoding.
                // At `view_radius = 9` the square contains 361 columns; at
                // `view_radius = 16` it contains 1,089. The absolute centre keeps
                // the streamed terrain aligned with the view tracker for joins
                // away from the origin.
                let join_cx = (join_pos.x / 16.0).floor() as i32;
                let join_cz = (join_pos.z / 16.0).floor() as i32;
                let player_ticket_guard = {
                    let bits = login_uuid.unwrap_or_else(uuid::Uuid::nil).as_u128();
                    let id = (bits as u64) ^ ((bits >> 64) as u64);
                    let store = connection_travel::ticket_store_for_source(
                        source.get(), home_source.get(), &home_tickets,
                    )?;
                    let guard = store.grant_player_with_simulation_radius(
                        id, (join_cx, join_cz), view_radius,
                        view_radius.clamp(0, crate::chunk_store::CONCURRENT_TICK_RADIUS),
                    ).with_spawn_store(&home_tickets);
                    source.get().reconcile_ticket_residency();
                    guard
                };
                let t_chunks = JoinStopwatch::now();
                let join_trace = JoinTrace::new();
                let mut batch_size = 0;
                let mut prestream_deliveries = Vec::new();
                let window = crate::join_scheduler::generation_window();
                let rings: Vec<Vec<(i32, i32)>> = join_view_rings(view_radius)
                    .into_iter()
                    .map(|ring| {
                        ring.into_iter()
                            .map(|(dx, dz)| (join_cx + dx, join_cz + dz))
                            .collect()
                    })
                    .collect();
                let ring_count = rings.len();
                // How much has to be on the wire before the player may act. See
                // `JOIN_PRESTREAM_RADIUS`.
                let prestream: usize = rings
                    .iter()
                    .take(JOIN_PRESTREAM_RADIUS as usize + 1)
                    .map(Vec::len)
                    .sum();
                let join_stream;
                match &source {
                    SourceRef::Shared(src) => {
                        let coords: Vec<(i32, i32)> = rings.into_iter().flatten().collect();
                        // `prioritised` keys deferred columns by distance from
                        // the player, with an in-frustum bonus; `serve_play`
                        // re-keys the queue when the player moves or turns.
                        // At join, the absent rotation makes this key equal the
                        // `join_view_rings` order.
                        //
                        // The priority centre is the player's own column, not
                        // `(0, 0)`: it is compared against the absolute
                        // coordinates in `coords`, and `serve_play`'s
                        // `reprioritise` compares against the player's absolute
                        // chunk. The origin is not a valid substitute for that
                        // centre when a player joins elsewhere.
                        //
                        // `encoding_with`: protocol encode runs **inside** the
                        // per-column `spawn_blocking` closure, so this task only
                        // writes frames. The measured cost is 62 M instructions /
                        // ≈2.4 ms per column (≈2.6 s for serial encoding); see
                        // `crate::protocol::ChunkEncoder`. Worker completion
                        // cannot change the wire because queue order controls
                        // emission.
                        let pipeline = crate::join_scheduler::ColumnPipeline::prioritised(
                            Arc::clone(src),
                            coords,
                            window,
                            (join_cx, join_cz),
                            None,
                        )
                        .with_generation_band(
                            (join_cx, join_cz),
                            crate::join_scheduler::DEFAULT_FULL_GENERATION_RADIUS,
                        );
                        let mut pipeline = pipeline
                            .with_trace(join_trace.clone())
                            .encoding_with(if proto.uses_cross_column_light()
                                || proto.retains_initial_column_light()
                            {
                                None
                            } else {
                                proto.chunk_encoder()
                            });
                        while batch_size < prestream {
                            let next = match pipeline.next().await {
                                Ok(next) => next,
                                Err(error) => {
                                    return return_chunk_encode_error(
                                        conn,
                                        proto,
                                        &mut state,
                                        Some(i32::try_from(batch_size).unwrap_or(i32::MAX)),
                                        error,
                                    )
                                    .await;
                                }
                            };
                            let Some(((cx, cz), payload)) = next else {
                                break;
                            };
                            let directive = match encode_column(
                                proto,
                                source,
                                cx,
                                cz,
                                join_trace.as_deref(),
                                payload,
                            )
                            .await {
                                Ok(directive) => directive,
                                Err(error) => {
                                    return return_chunk_encode_error(
                                        conn,
                                        proto,
                                        &mut state,
                                        Some(i32::try_from(batch_size).unwrap_or(i32::MAX)),
                                        error,
                                    )
                                    .await;
                                }
                            };
                            let sent = matches!(directive.directive, ServerDirective::Send { .. });
                            let stage = directive.stage;
                            apply(conn, &mut state, directive.directive).await?;
                            if sent {
                                prestream_deliveries.push(((cx, cz), stage));
                            }
                            if let Some(trace) = join_trace.as_ref() {
                                trace.mark("delivered", cx, cz);
                            }
                            batch_size += 1;
                        }
                        join_stream = crate::join_scheduler::JoinChunkStream::windowed(pipeline);
                    }
                    // The `Dimension` arm reaches this path when a native player
                    // locator restores directly into a sibling dimension. Its
                    // erased source cannot inhabit the generic shared pipeline,
                    // so it uses the same ordered ring path as a borrowed source.
                    SourceRef::Borrowed(_) | SourceRef::Dimension(_) => {
                        // A borrowed source is not `'static`, so it cannot be
                        // spawned; each ring on this arm blocks until its
                        // columns finish generating. A window cannot overlap
                        // generation and encoding here. Ring cumulative sizes
                        // are `1 + 4r(r + 1)`, always ≡ 1 (mod 8); with a window
                        // of 8, ring 8's 64 columns therefore form eight serial
                        // batches rather than one.
                        //
                        // Both arms walk the same flattened ring sequence and
                        // emit the player's column first. The ordering checks
                        // `join_streams_the_view_outward_from_the_players_own_column`
                        // and `the_shared_arm_streams_the_view_outward_too` cover
                        // the borrowed and shared paths.
                        //
                        // Rings `0..=JOIN_PRESTREAM_RADIUS` are generated and
                        // encoded here; the remaining rings are handed to
                        // `serve_play` as whole rings. The emitted sequence is
                        // follows the ring order, with one barrier per ring.
                        let mut rings = rings;
                        let deferred = rings.split_off(
                            (JOIN_PRESTREAM_RADIUS as usize + 1).min(rings.len()),
                        );
                        for ring in &rings {
                            let columns = source.generate(ring.clone()).await?;
                            for (&(cx, cz), column) in ring.iter().zip(columns.iter()) {
                                if let Some(trace) = join_trace.as_ref() {
                                    trace.mark("generated", cx, cz);
                                }
                                let directive = match encode_chunk_with_source_receipt(
                                    proto,
                                    source.get(),
                                    cx,
                                    cz,
                                    column,
                                ) {
                                    Ok(directive) => directive,
                                    Err(error) => {
                                        return return_chunk_encode_error(
                                            conn,
                                            proto,
                                            &mut state,
                                            Some(i32::try_from(batch_size).unwrap_or(i32::MAX)),
                                            error,
                                        )
                                        .await;
                                    }
                                };
                                if let Some(trace) = join_trace.as_ref() {
                                    trace.mark("encoded", cx, cz);
                                }
                                let sent = matches!(directive.directive, ServerDirective::Send { .. });
                                let stage = directive.stage;
                                apply(conn, &mut state, directive.directive).await?;
                                if sent {
                                    prestream_deliveries.push(((cx, cz), stage));
                                }
                                if let Some(trace) = join_trace.as_ref() {
                                    trace.mark("delivered", cx, cz);
                                }
                                batch_size += 1;
                            }
                        }
                        join_stream = crate::join_scheduler::JoinChunkStream::ringed(deferred);
                    }
                }
                // `batch_size` is a `usize` because it is compared against
                // `prestream` above; the wire field is an `i32`.
                apply(
                    conn,
                    &mut state,
                    proto.end_chunk_batch(i32::try_from(batch_size).unwrap_or(i32::MAX)),
                )
                .await?;
                world.mark_join_ready();
                let chunk_ms = t_chunks.elapsed().as_millis();
                let chunks_sent = batch_size;
                tracing::info!(
                    "join chunks: {} columns inline in {}ms ({:.0} col/s), {} deferred to the \
                     play loop, {} rings, window {}",
                    chunks_sent,
                    chunk_ms,
                    chunks_sent as f64 / (chunk_ms as f64 / 1000.0),
                    join_stream.remaining(),
                    ring_count,
                    window,
                );

                let t_welcome = JoinStopwatch::now();
                for directive in proto.welcome_message() {
                    apply(conn, &mut state, directive).await?;
                }

                // `ConfigurationFinished` cannot be reached without a successful
                // `LoginStart`/login completion in any correct `ServerProtocol`:
                // modern protocols receive the two acknowledgements, while a
                // legacy protocol queues this transition after login. `username`
                // is therefore always `Some` here; falling back to an empty
                // string rather than panicking keeps a protocol that violates
                // that contract merely wrong, not a crash.
                let username = username.clone().unwrap_or_default();

                let join_entities = ActiveEntities::new(world, entities, source.dimension());
                let entities = &join_entities;

                // Register this connection as a player entity before initial
                // sync. Other connections then see it on their next pass, and
                // this connection can exclude its own entity. The ticket moves
                // into `serve_play`, whose `Drop` implementation deregisters
                // the player on every exit path.
                let (player_ticket, initial_swing_cursor) =
                    entities.players().map_or((None, None), |registry| {
                        let (ticket, cursor) = registry.join_in_dimension_with_swing_cursor(
                            &username,
                            login_uuid.unwrap_or_else(uuid::Uuid::nil),
                            join_pos,
                            source.dimension(),
                        );
                        (Some(ticket), Some(cursor))
                    });

                // Initial entity sync sends tab-list additions and other
                // players' spawns in the order defined by [`stream_pass`].
                for directive in stream_pass(
                    proto,
                    entities,
                    &mut streamer,
                    &mut player_list,
                    player_ticket.as_ref(),
                ) {
                    apply(conn, &mut state, directive).await?;
                }

                // derive view centre from the actual spawn
                // chunk rather than assuming (0, 0). For spawn at (8, ~64,
                // 8) both floor to 0, so the centre does not change today;
                // the derivation is the point — when the spawn column or
                // the X/Z offsets move, this follows automatically.
                // `join_pos`, not `spawn.pos`: a restored player standing 400
                // blocks from world spawn must be sent the chunks under *their*
                // feet. Centring on world spawn instead would stream a square of
                // terrain the player cannot see and leave them suspended over
                // nothing — a total chunk blackout with a perfectly healthy join.
                //
                // Reused from the binding the chunk stream above already derived,
                // rather than recomputed: the tracker's `loaded` set is a claim
                // about the square that stream actually emitted, so a second
                // derivation is a second chance for the two to disagree — and
                // when they did, the columns under the player's feet were marked
                // sent without ever being sent.
                let (spawn_cx, spawn_cz) = (join_cx, join_cz);
                // Keep the join radius and its configured maximum distinct:
                // the square is streamed at the first value, while
                // `ClientInformationChanged` may request any value up to
                // `ViewTracker::max_radius`.
                // Shared progressive sources start with a shaped far band, so
                // the tracker seeds the same stage ledger the join pipeline
                // used. Sources that require a complete packet (or retain the
                // legacy borrowed/ringed path) keep the all-full ledger.
                let mut view = match source {
                    SourceRef::Shared(src)
                        if src
                            .packet_generation_stage(ChunkGenerationStage::Shaped)
                            .is_none() => ViewTracker::new_banded(
                        (spawn_cx, spawn_cz),
                        view_radius,
                        max_view_radius,
                        crate::join_scheduler::DEFAULT_FULL_GENERATION_RADIUS,
                    ),
                    _ => ViewTracker::new((spawn_cx, spawn_cz), view_radius, max_view_radius),
                };
                for (coord, stage) in prestream_deliveries {
                    if let Some(incarnation) = view.incarnation(coord) {
                        view.record_delivery(coord, incarnation, stage);
                    }
                }
                // `player_uuid`, `permission_level` and
                // `builtins` are the bindings the `COMMANDS` send above already
                // derived, reused rather than recomputed — the tree the client was
                // sent and the tree this session dispatches against **must** be the
                // same one, and two constructions are two chances for them to
                // differ. That is the failure mode the whole `WireDescriptor`
                // arrangement exists to prevent, and rebuilding here would reopen
                // it one level up.
                //
                // Their derivation is above, at the send, and stated there: the uuid
                // is the one `login_success` echoed to this client, the level comes
                // from that authenticated uuid and never from a command's text, and
                // `username` is the name that survived `is_valid_player_name`.
                // Nothing the player later *sends* can change either, which is
                // exactly the property the seam needs — see the
                // `ServerBound::ChatCommand` arm in `dispatch_play_packet`.
                let commands = CommandSession {
                    builtins,
                    dispatch: commands.clone(),
                    caller: CommandCaller::with_permission_level(
                        player_uuid,
                        username.clone(),
                        permission_level,
                    ),
                    #[cfg(not(target_arch = "wasm32"))]
                    plugin_access: access.clone(),
                    permission_level,
                };
                // The server-authoritative advancement/statistics
                // store for this connection, created at the Play handoff and
                // carried into `serve_play` so the per-packet flush and the
                // `REQUEST_STATS` reply can reach it. The first packet is sent
                // here, at join, exactly where vanilla's own per-player
                // advancements tracker's own "flush dirty" first-packet path fires:
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
                // `saturating_sub`, not `- 1`: `as_millis()` is `u128`, and over an
                // in-memory or loopback transport the welcome phase completes in
                // under a millisecond, so the plain subtraction underflows. That
                // panicked every integrated-server test in debug and wrapped
                // silently in release, which is why no `cargo check` and no
                // `cargo run --release` could see it.
                let welcome_ms = t_welcome.elapsed().as_millis().saturating_sub(1); // approx, minus advancement encode
                tracing::info!(
                    "Configuration -> Play: {}ms total (chunks={}ms, welcome/entities/advancements={}ms)",
                    total_ms,
                    chunk_ms,
                    welcome_ms,
                );
                // Vanilla validates an announced profile key whenever this
                // connection completed online authentication and the Mojang
                // issuer service is usable. `enforce-secure-profile` is a
                // separate policy: it decides whether a player must sign, not
                // whether an otherwise-valid announcement is adopted. Fetching
                // remains outside the cache lock in `profile_key_issuers`.
                #[cfg(not(target_arch = "wasm32"))]
                let profile_key_issuers = if online_authenticated {
                    online_mode
                        .expect("an online-authenticated connection has OnlineModeConfig")
                        .profile_key_issuers(crate::chat_session::now_millis())
                        .await
                } else {
                    None
                };
                #[cfg(not(target_arch = "wasm32"))]
                let enforce_secure_profile = online_authenticated
                    && entities
                        .players()
                        .is_some_and(PlayerRegistry::enforce_secure_profile)
                    && profile_key_issuers.is_some();
                if let Some(error) = world.initial_seed_error() {
                    return return_initial_seed_error(conn, proto, &mut state, None, error).await;
                }
                let service = crate::connection_service::ConnectionService::new();
                let play = crate::connection_service::pin_future(|| serve_play(
                    &service,
                    conn,
                    proto,
                    source,
                    home_source,
                    entities,
                    state,
                    initial_teleport_id,
                    streamer,
                    player_list,
                    player_ticket,
                    initial_swing_cursor,
                    player_ticket_guard,
                    view,
                    username,
                    spawn_position,
                    chunks_sent,
                    join_stream,
                    join_trace,
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
                    #[cfg(not(target_arch = "wasm32"))]
                    profile_key_issuers,
                    #[cfg(not(target_arch = "wasm32"))]
                    enforce_secure_profile,
                    border,
                    resource_packs,
                    &mut client_channels,
                    plugin_channels,
                    game_mode,
                    world,
                    live_save,
                    #[cfg(not(target_arch = "wasm32"))]
                    native_player,
                ));
                return service.run(play).await;
            }
            // Wire-level plugin messaging, Configuration-phase: a
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
            | ServerBound::ItemDropped { .. }
            | ServerBound::UseItemOn { .. }
            | ServerBound::ChangeGameMode { .. }
            | ServerBound::DifficultyChanged { .. }
            | ServerBound::DifficultyLockChanged { .. }
            | ServerBound::GameRuleChanged { .. }
            | ServerBound::CarriedItemChanged { .. }
            | ServerBound::ContainerClicked { .. }
            | ServerBound::RecipePlaced { .. }
            | ServerBound::RecipeBookSettingsChanged { .. }
            | ServerBound::RecipeBookRecipeSeen { .. }
            | ServerBound::SeenAdvancements { .. }
            | ServerBound::ResourcePackResponse { .. }
            | ServerBound::PlayerLoaded
            | ServerBound::ClientTickEnded
            | ServerBound::TeleportationAccepted { .. }
            | ServerBound::PlayerAbilitiesChanged { .. }
            | ServerBound::BlockEntityTagQuery { .. }
            | ServerBound::EntityTagQuery { .. }
            | ServerBound::ContainerClosed { .. }
            | ServerBound::Attack { .. }
            | ServerBound::InteractEntity { .. }
            | ServerBound::UseItem { .. }
            | ServerBound::ReleaseUseItem
            | ServerBound::SwapItemInHand
            | ServerBound::VehicleMoved { .. }
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
            | ServerBound::ChatSessionAnnounced { .. }
            | ServerBound::PlayerCommand { .. }
            | ServerBound::RenameItem { .. }
            | ServerBound::ContainerButtonClick { .. }
            | ServerBound::ContainerSlotStateChanged { .. }
            | ServerBound::SetCommandBlock { .. }
            | ServerBound::SignUpdate { .. }
            | ServerBound::EditBook { .. }
            | ServerBound::SetBeacon { .. }
            | ServerBound::SelectTrade { .. }
            | ServerBound::SelectBundleItem { .. }
            | ServerBound::PaddleBoat { .. }
            | ServerBound::PickItemFromBlock { .. }
            | ServerBound::PickItemFromEntity { .. }
            | ServerBound::Pong { .. }
            | ServerBound::TeleportToEntity { .. }
            | ServerBound::Swing { .. }
            | ServerBound::SpectatorAction { .. }
            | ServerBound::CommandSuggestion { .. }
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
/// own generic block-position "relative" helper, used below to find the placement cell when
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
    /// Which menu shape this window is, which decides the slot layout and the
    /// quick-move routing [`crate::container_click`] runs.
    ///
    /// A crafting table is [`MenuKind::CraftingTable`] and its `pos` is the
    /// table's block position — used for nothing but the "did the player break the
    /// block under the menu" check, because a crafting table is **not** a block
    /// entity and has no slots at `pos` at all (the virtual-menu step). Its grid lives
    /// on [`PlayerInventory::table_crafting`].
    shape: MenuKind,
    container_size: usize,
    /// Vanilla's own generic container-menu state-id field, wrapping at `32767`
    /// (its own "increment state id" helper). Bumped by every content/
    /// slot send (this struct's own [`next_state_id`](Self::next_state_id)),
    /// never by a `container_set_data` send — vanilla's own "broadcast
    /// changes" step
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

/// Which merchant screen (if any) this connection currently has open — set
/// by [`open_merchant_screen`]'s caller, read by
/// [`ServerBound::SelectTrade`]'s dispatch arm. Not [`OpenContainer`]: a
/// villager is a [`crate::mobs::SimMob`], not a [`crate::block_entities::BlockEntity`],
/// so it has no `BlockPos` for that struct's `pos` field or its slot-sync
/// machinery to key on. Carries no `window_id` — vanilla's own consumer of
/// the one packet this drives (`SelectTrade`) checks only "is a merchant
/// menu open", not which window, and this struct exists for exactly that
/// question.
#[derive(Debug, Clone, Copy)]
struct OpenMerchant {
    /// The villager entity id this screen's offers came from.
    entity_id: i32,
}

/// Per-connection bookkeeping for [`OpenContainer`]'s periodic sync
/// ([`sync_open_container`]): the container slots and menu-data properties
/// last pushed to the client, so a background mutation (a furnace's own
/// tick, not any click) can be diffed and only the changed entries re-sent —
/// the same changed-entry model used by [`EntityStreamer`] for entity spawn,
/// update, and removal.
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
/// furnace's own background tick (`crate::tick::run_tick_loop`, the shared world tick,
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

async fn publish_open_container<T: Transport, P: ServerProtocol>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    block_entities: &BlockEntityHandle,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
) -> Result<(), ServerError> {
    if let Some(open) = open_container.as_mut() {
        let (slots, data) = container_state(block_entities, open.pos);
        for directive in sync_open_container(proto, open, container_sync, slots, data) {
            apply(conn, state, directive).await?;
        }
    }
    Ok(())
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
        "minecraft:generic_9x3" => "Chest",
        "minecraft:anvil" => "Repair & Name",
        "minecraft:grindstone" => "Grindstone",
        "minecraft:smithing" => "Smithing Table",
        "minecraft:loom" => "Loom",
        "minecraft:stonecutter" => "Stonecutter",
        "minecraft:enchantment" => "Enchant",
        "minecraft:merchant" => "Villager",
        "minecraft:beacon" => "Beacon",
        _ => "Container",
    }
}

/// Opens a villager's `minecraft:merchant` trade screen (the merchant-offers
/// packet). Unlike [`open_container_screen`]/`open_crafting_table_screen`,
/// this sends no `container_set_content`/`container_set_data` at all: a
/// merchant window's whole state is the `MERCHANT_OFFERS` packet, sent
/// immediately after `open_screen`.
///
/// Trade selection reads the villager's persistent offer state and commits the
/// purchase only when the buyer can pay. Restock, leveling, demand, and uses
/// are handled by [`crate::mobs::MobSim::villager_offers`] and
/// [`crate::mobs::MobSim::try_villager_trade`].
///
/// `offers` is the priced persistent list from
/// [`crate::mobs::MobSim::villager_offers`], so the displayed `uses` and
/// `demand` values match the state charged by trade selection.
#[allow(clippy::too_many_arguments)]
async fn open_merchant_screen<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    offers: &[crate::villager_trade::OfferState],
    level: i32,
    xp: i32,
    next_window_id: &mut i32,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    *next_window_id = *next_window_id % 100 + 1;
    let window_id = *next_window_id;

    apply(
        conn,
        state,
        proto.encode_open_screen(window_id, "minecraft:merchant", container_title("minecraft:merchant")),
    )
    .await?;

    let wire_offers: Vec<MerchantOfferOut> = offers
        .iter()
        .filter_map(|offer| {
            Some(MerchantOfferOut {
                wants_a: (
                    offer.record.wants_item.parse::<ResourceKey>().ok()?,
                    offer.modified_cost_a_count(),
                ),
                wants_b: None,
                gives: (
                    offer.record.gives_item.parse::<ResourceKey>().ok()?,
                    offer.record.gives_count,
                ),
                max_uses: offer.record.max_uses,
                xp: offer.record.xp,
            })
        })
        .collect();

    apply(
        conn,
        state,
        proto.encode_merchant_offers(window_id, &wire_offers, level, xp, true, true),
    )
    .await
}

/// Executes a merchant purchase directly against the player's held
/// inventory and the [`TradeRecord`](crate::mobs::villager::trades::TradeRecord)
/// selected by [`ServerBound::SelectTrade`].
///
/// # Payment model
///
/// A full merchant menu places items into two payment slots and returns the
/// result through a third. That would require per-connection scratch storage
/// shaped like
/// [`PlayerInventory::workstation`], **except** a villager is not a
/// [`crate::block_entities::BlockEntity`] the way an anvil or a furnace is
/// (it is a [`crate::mobs::SimMob`]), so it has no `BlockPos` for
/// [`OpenContainer`]'s slot-sync machinery to key on — the sync loop that
/// makes every *other* menu in this crate live reads a block entity by
/// position. This helper therefore executes the trade in one step when a row
/// is selected and leaves the block-entity slot-sync path untouched.
///
/// # What that costs
///
/// The cost items are found and consumed from wherever they sit in the
/// standard 36-slot hotbar+main inventory ([`PlayerInventory::consume`]),
/// not from two manually-filled slots, so the player can trade without moving
/// items into dedicated payment slots.
///
/// `offer` is the caller's read-only priced peek at the villager's *live*,
/// persistent [`crate::villager_trade::VillagerTrades`] entry
/// ([`crate::mobs::MobSim::villager_offers`]). This function checks whether the
/// buyer's inventory can afford it; [`crate::mobs::MobSim::try_villager_trade`]
/// commits uses, demand, and experience only when that check succeeds.
///
/// Returns `None` — inventory untouched — when the player lacks the cost
/// items or has no room for the result; the inventory stays unchanged and no
/// excess item is dropped.
fn attempt_villager_trade(
    inventory: &PlayerInventory,
    offer: &crate::villager_trade::OfferState,
) -> Option<PlayerInventory> {
    let trade = &offer.record;
    let mut trial = inventory.clone();
    // `offer.modified_cost_a_count()`, not `trade.wants_count`: the persistent
    // whole remaining scope was that a real reputation/Hero-of-the-Village
    // discount was computed and never reached a price — this is that price.
    trial.consume(
        trade.wants_item,
        u32::try_from(offer.modified_cost_a_count()).ok()?,
    )?;
    if let Some((item, count)) = trade.wants_b {
        trial.consume(item, u32::try_from(count).ok()?)?;
    }
    let gives = ItemStack::new(
        trade.gives_item.parse::<ResourceKey>().ok()?,
        u32::try_from(trade.gives_count).ok()?,
    );
    let (_, leftover) = trial.add(gives);
    if leftover.is_some() {
        return None;
    }
    Some(trial)
}

/// Opens a block-entity's container screen for this connection, mirroring
/// vanilla's own per-player "open menu" step end to end: a fresh container id
/// (its own container-counter field: `1..=100`, wrapping),
/// an `open_screen` send, then an immediate full `container_set_content`
/// plus every `container_set_data` property (its own "init menu" step's own
/// "add slot listener" call
/// triggers its own "broadcast full state" step the instant the menu is constructed).
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

    // A lectern is a one-slot reader, not a generic container with a player
    // inventory tail.  The client-side menu has the same one-slot shape; if
    // the tail were appended the book would be offset and ordinary clicks
    // could reach inventory slots that the reader never displays.
    let mut items = own_slots.clone();
    if menu != "minecraft:lectern" {
        for native in 9..=35 {
            items.push(inventory.native(native).cloned());
        }
        for native in 0..=8 {
            items.push(inventory.native(native).cloned());
        }
    }

    let mut opened = OpenContainer {
        window_id,
        pos,
        // Every menu this function opens is a plain `generic_*` container
        // shape *except* the beacon, whose one payment slot has its own
        // restricted `may_place`/`max_stack_size` (`MenuKind::Beacon`'s own
        // doc) — everything else here keyed on the block entity's own
        // `menu_name()`, matching this function's one caller.
        shape: match menu {
            "minecraft:beacon" => MenuKind::Beacon,
            "minecraft:lectern" => MenuKind::Lectern,
            _ => MenuKind::Container { size: own_slots.len() },
        },
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

/// Opens a crafting table's `minecraft:crafting` menu — the virtual-menu step, the
/// **positionless virtual menu**.
///
/// [`open_container_screen`] structurally cannot do this: it is driven entirely by
/// a [`BlockEntity`] at `pos`, and **a crafting table is not a block entity.** Its
/// slots are scratch space owned by the menu (the menu creates a virtual
/// crafting grid and result slot, then discards both on close), which here is
/// [`PlayerInventory::table_crafting`].
///
/// `pos` is still carried on the [`OpenContainer`] — not to find slots, but so
/// breaking the table closes the window, exactly as it already does for a furnace.
///
/// The 46 slots sent are `CraftingMenu`'s own order: result `0`, the 3×3 grid
/// `1..=9`, main storage `10..=36`, hotbar `37..=45`.
async fn open_crafting_table_screen<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    inventory: &mut PlayerInventory,
    pos: BlockPos,
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

    inventory.open_table_crafting();

    apply(
        conn,
        state,
        proto.encode_open_screen(window_id, "minecraft:crafting", "Crafting"),
    )
    .await?;

    let layout = MenuLayout::crafting_table();
    let items = read_menu(&layout, inventory, inventory.table_crafting(), &[]);

    let mut opened = OpenContainer {
        window_id,
        pos,
        shape: MenuKind::CraftingTable,
        // Result + the nine grid cells: the menu's own section, before the player
        // tail. Only `container_menu_slot`'s legacy callers read this; the click
        // path uses `shape`.
        container_size: 10,
        state_id: 0,
    };
    let state_id = opened.next_state_id();
    apply(
        conn,
        state,
        proto.encode_container_content(
            window_id,
            state_id,
            &items,
            inventory.click_state().carried.as_ref(),
        ),
    )
    .await?;

    *open_container = Some(opened);
    // No background mutation to poll — a crafting grid changes only on a click —
    // so the periodic sync is left with nothing to diff.
    *container_sync = ContainerSync::default();
    Ok(())
}

/// The wire `menu_type` [`Station`] opens — `lodestone_game::menus::build_menu`'s
/// own dispatch table (`(Some("anvil"), 3)` etc.) is the client-side mirror of
/// this exact string.
fn workstation_menu_type(station: Station) -> &'static str {
    match station {
        Station::Anvil => "minecraft:anvil",
        Station::Grindstone => "minecraft:grindstone",
        Station::Smithing => "minecraft:smithing",
        Station::Loom => "minecraft:loom",
        Station::Stonecutter => "minecraft:stonecutter",
    }
}

/// Opens an anvil/grindstone/smithing-table screen (the workstation menu dispatcher) — the
/// same *positionless virtual menu* shape [`open_crafting_table_screen`]
/// established for the crafting table, because none of these three is a block
/// entity either (see [`PlayerInventory::workstation`]'s own doc).
async fn open_workstation_screen<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    inventory: &mut PlayerInventory,
    pos: BlockPos,
    station: Station,
    next_window_id: &mut i32,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    *next_window_id = *next_window_id % 100 + 1;
    let window_id = *next_window_id;
    let layout = MenuLayout::item_combiner(station);
    let inputs = layout.len() - 36 - 1;

    inventory.open_workstation(inputs);

    apply(
        conn,
        state,
        proto.encode_open_screen(window_id, workstation_menu_type(station), container_title(workstation_menu_type(station))),
    )
    .await?;

    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let items = read_workstation_menu(&layout, inventory, &cells, station, false, hooks);

    let mut opened = OpenContainer {
        window_id,
        pos,
        shape: MenuKind::ItemCombiner { inputs, station },
        container_size: inputs + 1,
        state_id: 0,
    };
    let state_id = opened.next_state_id();
    apply(
        conn,
        state,
        proto.encode_container_content(window_id, state_id, &items, inventory.click_state().carried.as_ref()),
    )
    .await?;

    *open_container = Some(opened);
    *container_sync = ContainerSync::default();
    Ok(())
}

/// Opens the enchanting-table screen: the same positionless
/// shape as [`open_workstation_screen`], but with no result slot — the item
/// slot is enchanted in place — so it carries its own [`MenuLayout`] and no
/// `Station`. Costs are computed once here from the empty menu (both slots
/// start empty, so all three costs are `0`) and then kept live by
/// [`apply_workstation_clicked`]... actually by the click path directly, since
/// `MenuKind::Enchanting` has no result to re-derive: see
/// `apply_enchanting_clicked`'s own doc for where the three
/// `container_set_data` costs are actually recomputed and sent.
async fn open_enchanting_screen<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    inventory: &mut PlayerInventory,
    pos: BlockPos,
    next_window_id: &mut i32,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    // Draw an enchantment seed from the connection's `[0, i32::MAX)` random
    // stream. `PlayerInventory::open_workstation` stores the value before the
    // first offer is computed, so menu offers receive a per-session seed.
    enchant_seed_roll: i64,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    *next_window_id = *next_window_id % 100 + 1;
    let window_id = *next_window_id;
    let layout = MenuLayout::enchanting_table();

    inventory.open_workstation(2);
    inventory.set_enchant_seed(enchant_seed_roll);

    apply(
        conn,
        state,
        proto.encode_open_screen(window_id, "minecraft:enchantment", "Enchant"),
    )
    .await?;

    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let items: Vec<Option<ItemStack>> = layout
        .iter()
        .map(|(_, kind)| match kind {
            SlotKind::Player(native) => inventory.native(native).cloned(),
            SlotKind::Grid(cell) => cells.get(cell).cloned().flatten(),
            SlotKind::Container(_) | SlotKind::Result => None,
        })
        .collect();

    let mut opened = OpenContainer {
        window_id,
        pos,
        shape: MenuKind::Enchanting,
        container_size: 2,
        state_id: 0,
    };
    let state_id = opened.next_state_id();
    apply(
        conn,
        state,
        proto.encode_container_content(window_id, state_id, &items, inventory.click_state().carried.as_ref()),
    )
    .await?;
    // Vanilla's own enchantment-menu's own data-slot registrations: three costs, all `0` for an empty
    // menu — its own "get enchantment cost" getter is gated on a non-empty, enchantable item 0.
    for index in 0..3i32 {
        apply(conn, state, proto.encode_container_data(window_id, index, 0)).await?;
    }
    let _ = source; // bookshelf power is read on the first item placement, not at open time — see `apply_enchanting_clicked`.

    *open_container = Some(opened);
    *container_sync = ContainerSync::default();
    Ok(())
}

/// Applies one block-breaking phase for the three destroy-action ordinals.
///
/// This production path **validates** the break rather than trusting it: see
/// [`crate::block_breaking`] for the destroy-progress arithmetic and the
/// tolerance it deliberately carries. Creative mode has a separate start-only
/// path that bypasses timing while retaining target validity and proposal
/// protection. Two survival behaviours follow from the timing computation, and
/// they are opposite ends of the same missing computation:
///
/// * **`StartDestroy` can break the block by itself.** When destroy progress
///   reaches `1.0` on the first tick, a zero-hardness block needs no follow-up
///   action. The server therefore handles instant blocks at `StartDestroy`.
/// * **A `StopDestroy` that arrives too early is *deferred*, not refused.** It
///   records a deferred dig and keeps accruing progress on the server's clock,
///   breaking the block once it is fully earned — see
///   [`crate::block_breaking::PendingBreak::defer`] and `serve_play`'s
///   `vitals_tick` arm. Bedrock and obsidian are still not instant, because an
///   unbreakable block is not deferrable and obsidian's deferred dig is minutes
///   long; but hold-and-release on stone breaks stone, which an outright refusal
///   made impossible.
///
/// `pending_break` is this connection's tracked in-progress dig, including its
/// target and accumulated progress.
/// It is what makes `StartDestroy` + `StopDestroy` break a block while
/// `StartDestroy` + `AbortDestroy` does not, and what makes a `StopDestroy` for a
/// position nobody started is a no-op; only the tracked target may advance.
///
/// **Also removes a broken position's [`BlockEntity`], if any, from the
/// registry**. A screen can remain open at the broken position, so leaving the
/// record would let a later container click mutate a container whose block no
/// longer exists. If [`OpenContainer`] points at the broken position, it is
/// cleared as well; the client receives no synthetic close frame.
///
/// When an integrated world has a proposal owner, the earned break is first
/// submitted as a Paper-shaped `BlockBreak` proposal. A denied or unavailable
/// proposal sends the authoritative state back to this connection and leaves
/// the source, drops, block-entity registry, and block-tick feed untouched.
#[allow(clippy::too_many_arguments)]
async fn apply_block_action<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    mut pending_relights: Option<&mut PendingRelights>,
    pending_break: &mut Option<PendingBreak>,
    block_entities: &BlockEntityHandle,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    // Shared mob handle where block-break loot becomes item entities. The
    // composter arm of `apply_use_item_on` uses the same handle for bone-meal
    // drops, so every connection's streaming pass sees the spawned entity.
    mobs: &MobHandle,
    drops_rng: &mut SpawnRng,
    // The breaker's main-hand stack, `None` for a bare hand. It supplies the
    // loot context and tool-eligibility check for the roll; a borrowed stack is
    // sufficient because the caller already owns the mutable inventory.
    held: Option<&ItemStack>,
    // The breaker's tracked feet position for the interaction-range
    // test, `None` until the client has sent a movement packet — see
    // `block_breaking::within_interaction_range` for why `None` permits the break
    // rather than refusing it.
    player_feet: Option<Vec3>,
    // The world's rules, for the `block_drops` gate below.
    world: &crate::world_state::WorldStateHandle,
    // The server tick this packet is being handled on, for the
    // destroy-progress accounting. `None` on `wasm32`, which has no timer to
    // count ticks with (see `serve_play`'s two definitions); the timing test is
    // then skipped, while the hardness and range tests still apply.
    game_tick: Option<u64>,
    // Where `destroy_block`'s break level event is published, and the player it
    // is published *except* for (this connection's own).
    block_ticks: &BlockTickFeed,
    breaker: uuid::Uuid,
    // Creative mode bypasses the hardness clock and produces no drops.
    creative: bool,
    action: BlockActionKind,
    // `minecraft:mined` counter — see `destroy_block`'s own parameter
    // comment for why it is awarded there rather than here.
    advancements: &mut AdvancementManager,
    // Hunger, for the per-block mining exhaustion `destroy_block` charges. Threaded
    // through rather than read from a wider scope so the creative guard stays at the
    // one place that knows the game mode.
    vitals: &mut PlayerVitals,
    pos: BlockPos,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    tracing::debug!(
        target: "lodestone_block_trace",
        ?action,
        x = pos.x,
        y = pos.y,
        z = pos.z,
        ?game_tick,
        pending = ?pending_break.as_ref().map(|dig| dig.pos),
        state = source.block_state_id(pos.x, pos.y, pos.z).raw(),
        "block action received"
    );
    // Vanilla's very first guard in `handleBlockBreakAction`, ahead of the
    // per-ordinal fork: a break out of reach is dropped whatever phase it is.
    if !crate::block_breaking::within_interaction_range(player_feet, pos) {
        return Ok(());
    }
    match action {
        BlockActionKind::StartDestroy => {
            let target = source.block_state_id(pos.x, pos.y, pos.z);
            let per_tick = (!creative)
                .then(|| crate::block_breaking::progress_per_tick(target, held))
                .flatten();
            if creative && target.block() == Block::Air {
                *pending_break = None;
                return Ok(());
            }
            // `None` is a state neither census knows — our gap, not a cheat, so
            // it is priced as an ordinary progressive dig that the `None`-clock
            // branch of `may_break_at` will accept on any `StopDestroy`.
            if creative || per_tick.is_some_and(|per| per >= 1.0) {
                // Vanilla's `"insta mine"` exit: the block is gone now, and no
                // `StopDestroy` is coming for it. This is the one-shot-block fix.
                // Creative takes the same exit for *every* block, which is what
                // makes a creative dig instant rather than merely fast.
                *pending_break = None;
                if !adjudicate_block_break(
                    conn,
                    proto,
                    source,
                    state,
                    world,
                    pos,
                    breaker,
                )
                .await?
                {
                    return Ok(());
                }
                destroy_block(
                    conn,
                    proto,
                    source,
                    state,
                    pending_relights.as_deref_mut(),
                    block_entities,
                    open_container,
                    container_sync,
                    mobs,
                    drops_rng,
                    held,
                    block_ticks,
                    breaker,
                    !creative && world.block_drops(),
                    world.block_drops(),
                    pos,
                    advancements,
                    (!creative).then_some(vitals),
                )
                .await?;
            } else {
                *pending_break = Some(PendingBreak {
                    pos,
                    progress_per_tick: per_tick.unwrap_or(f32::INFINITY),
                    start_tick: game_tick,
                    // Vanilla's `isDestroyingBlock`, not `hasDelayedDestroy`:
                    // this dig is waiting on a `StopDestroy` packet. A fresh
                    // `StartDestroy` replaces whatever was in the slot, including
                    // a deferred dig on another position — vanilla keeps the two
                    // states side by side and prefers the deferred one, a quirk
                    // not worth a second slot here (the client only ever has one
                    // dig in flight).
                    deferred: false,
                });
            }
        }
        BlockActionKind::AbortDestroy => {
            if pending_break.is_some_and(|dig| dig.pos == pos) {
                *pending_break = None;
            }
        }
        BlockActionKind::StopDestroy => {
            let Some(dig) = pending_break.filter(|dig| dig.pos == pos) else {
                return Ok(());
            };
            *pending_break = None;
            if !dig.may_break_at(game_tick) {
                // **Not a refusal.** A dig whose progress is below the threshold
                // enters the deferred state and continues through the per-player
                // tick until the block is fully mined. A `StopDestroy` arriving
                // on the same tick as `StartDestroy` therefore cannot clear 0.7.
                //
                // A `None` means the dig can never finish (bedrock, or no clock),
                // so the slot is simply left empty and nothing breaks. See
                // `block_breaking::PendingBreak::defer` and `serve_play`'s
                // `vitals_tick` arm, which is what finishes a deferred dig.
                *pending_break = dig.defer();
                return Ok(());
            }
            if !adjudicate_block_break(conn, proto, source, state, world, pos, breaker).await? {
                return Ok(());
            }
            destroy_block(
                conn,
                proto,
                source,
                state,
                pending_relights.as_deref_mut(),
                block_entities,
                open_container,
                container_sync,
                mobs,
                drops_rng,
                held,
                block_ticks,
                breaker,
                !creative && world.block_drops(),
                world.block_drops(),
                pos,
                advancements,
                (!creative).then_some(vitals),
            )
            .await?;
        }
    }
    Ok(())
}

/// Gives the tick-owned Paper event path the final say immediately before a
/// connection-side break writes its replacement. The proposal payload uses a
/// validated state id instead of a raw registry string; an unrecognised state
/// stays on the existing direct path because there is no safe event payload to
/// expose for it.
async fn adjudicate_block_break<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    wire_state: &mut State,
    world: &crate::world_state::WorldStateHandle,
    pos: BlockPos,
    breaker: uuid::Uuid,
) -> Result<bool, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let current = source.block_state_id(pos.x, pos.y, pos.z);
    let block_state = current;
    let Some(proposals) = world.proposal_handle() else {
        tracing::debug!(target: "lodestone_block_trace", x = pos.x, y = pos.y, z = pos.z, "block break has no proposal owner");
        return Ok(true);
    };
    let decision = proposals.block_break(pos, block_state, breaker).await;
    tracing::debug!(target: "lodestone_block_trace", x = pos.x, y = pos.y, z = pos.z, ?decision, "block break proposal resolved");
    let allowed = matches!(
        decision,
        Ok(crate::ecs::ServerProposalAction::BlockBreak {
            pos: resolved_pos,
            state: resolved_state,
            breaker: resolved_breaker,
        }) if resolved_pos == pos && resolved_state == block_state && resolved_breaker == breaker
    );
    if !allowed {
        // A client may have predicted the break while the proposal waited for
        // the next tick. Correct it with the source's current authoritative
        // state, without publishing a world mutation or a synthetic break.
        apply(
            conn,
            wire_state,
            proto.encode_block_update(pos.x, pos.y, pos.z, current),
        )
        .await?;
    }
    Ok(allowed)
}

/// The [`crate::fluid::FluidEnv`] for the column `pos` falls in — the
/// dimension's real vertical extent rather than [`crate::fluid::FluidEnv::OVERWORLD`]'s
/// literal bounds, matching how [`crate::tick::run_tick_loop`] derives one for
/// its own fluid drain. [`crate::fluid::ticks_after_edit`] needs this at every
/// edit site so the seeding it schedules honours the same build-height guard
/// a scheduled fluid tick does.
fn fluid_env_at<S: ChunkSource + ?Sized>(source: &S, pos: BlockPos) -> crate::fluid::FluidEnv {
    let column = source.column(pos.x.div_euclid(16), pos.z.div_euclid(16));
    crate::fluid::FluidEnv::overworld_in(column.min_y, column.height)
}

/// Breaks the block at `pos`: rolls and pops its loot, clears any block entity
/// and open container against it, and tells the client.
///
/// [`apply_block_action`] calls this helper for both instant `StartDestroy` and
/// validated `StopDestroy` requests. Both routes share loot rolling,
/// block-entity cleanup, and the client update.
#[allow(clippy::too_many_arguments)]
async fn destroy_block<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    mut pending_relights: Option<&mut PendingRelights>,
    block_entities: &BlockEntityHandle,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    mobs: &MobHandle,
    drops_rng: &mut SpawnRng,
    held: Option<&ItemStack>,
    // Publish the break effect (sound and particles) to every viewer except
    // `breaker`; the acting client predicts its own effect locally. See
    // `BlockTickFeed::publish_effect_except`.
    block_ticks: &BlockTickFeed,
    breaker: uuid::Uuid,
    // `false` in creative — the direct destroy branch writes no drops, and a
    // creative break consumes no loot-roll RNG draws.
    drop_loot: bool,
    // The `block_drops` game rule **alone**, without the creative fork above —
    // for the support cascade only. The two gates differ because a support
    // cascade has no player context; passing `drop_loot` here would make a
    // creative player mining under a flower delete the flower.
    cascade_drops: bool,
    pos: BlockPos,
    // The statistics store, for the `minecraft:mined` counter. Keyed by the block
    // that was broken, and incremented on **every** break including a creative
    // one. Keep this independent of `drop_loot`, because creative breaks still
    // contribute to the mined count even though they produce no item entities.
    advancements: &mut AdvancementManager,
    // Hunger's mining cost (`0.005` per block).
    // `None` for a break by an invulnerable player, who mines for free. An
    // `Option` rather than a bool beside the vitals keeps the guard
    // cannot be forgotten at a new call site.
    exhaust: Option<&mut PlayerVitals>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let started = JoinStopwatch::now();
    // Read the block before replacement; once `set_block` runs, the original
    // state cannot be recovered. Capture its fluid state before writing the
    // replacement so a waterlogged block leaves its water source rather than
    // unconditional air (see `new_state` below).
    let broken = source.block_state_id(pos.x, pos.y, pos.z);
    // The removal write preserves a cell's *fluid* state. For a dry block
    // `fluid_state_of` is `None` and this is plain air, which is why every
    // existing break gate — all of them dry blocks — could not see the
    // difference. A waterlogged block's fluid state is a water source
    // (`fluid_state_of` reports `amount: 8, falling: false`), so its
    // `block_state()` is `minecraft:water[level=0]`, the source state left
    // behind.
    let new_state = crate::fluid::fluid_state_of_id(broken)
        .map(crate::fluid::FluidState::block_state_id)
        .unwrap_or_else(|| Block::Air.default_state());
    // The base name, not the state string: `minecraft:mined` is keyed by *block*,
    // so `minecraft:oak_log[axis=y]` and `minecraft:oak_log` must be one counter
    // rather than two. Every other per-block table in this crate strips the suffix
    // the same way.
    advancements.award_stat(
        breaker,
        crate::advancements::StatKey::new(
            crate::advancements::StatType::Mined,
            broken.block().name(),
        ),
        1,
    );
    if let Some(vitals) = exhaust {
        vitals.add_exhaustion(crate::food::EXHAUSTION_MINE);
    }
    if let Some(effect) = crate::effects::block_destroyed(pos, broken) {
        block_ticks.publish_effect_except(breaker, effect);
    }
    source.set_block(pos.x, pos.y, pos.z, new_state);
    let edit_elapsed = started.elapsed();
    // Roll the broken block's loot table and pop each resulting
    // stack as a real item entity. `MobSim` already ticks item
    // lifecycle and fall dynamics every server tick
    // (`crate::tick::run_tick_loop`) and already streams items to
    // every connection (`MobSim::snapshots`), so this one call is
    // what connects a 1,551-line loot module that had no production
    // caller to the wire path mobs already proved reaches a client.
    //
    // **Gated on `block_drops`**, the world-state drop rule. The resource-drop
    // path checks the rule before it rolls or emits any item entities.
    //
    // **Tool validation decides whether the table rolls and what context it receives.**
    // `drops_are_allowed` checks the required tool before
    // the loot table is rolled — so a bare hand on stone
    // breaks the block and drops nothing, and the roll's RNG draws
    // never happen either (folding the check into the table would
    // still consume them and shift the next break's stream). `held`
    // then rides into the roll as the tool loot-context parameter, which is
    // what makes `match_tool`, `apply_bonus` and `table_bonus`
    // evaluate against a real item instead of an absent one.
    let popped = if drop_loot && crate::block_drops::drops_are_allowed(broken, held) {
        crate::block_drops::drop_block_loot(
            crate::block_drops::bundled_tables(),
            broken,
            pos,
            held,
            drops_rng,
        )
    } else {
        Vec::new()
    };
    if !popped.is_empty() {
        mobs.with(|sim| {
            for drop in popped {
                // `ItemLifecycle::newly_dropped` already sets the
                // 10-tick delay used for freshly spawned drops, so the breaker
                // cannot re-absorb the drop on the spawning tick.
                let count = u8::try_from(drop.stack.count).unwrap_or(u8::MAX);
                sim.spawn_item(
                    drop.stack.item.clone(),
                    drop.position,
                    drop.velocity,
                    ItemLifecycle::newly_dropped(count, DEFAULT_MAX_STACK_SIZE),
                );
            }
        });
    }
    // The break path awards experience orbs at the **centre** of the broken
    // cell, not at the jittered positions its item drops use.
    //
    // Gated on `drop_loot` for the same reason the loot above is: the world drop
    // rule controls the entire break reward path. It is deliberately **not** gated
    // on `drops_are_allowed` — tool validation controls item drops, while the
    // break reward is evaluated for every destroyed block,
    // so breaking coal ore with a bare hand yields no coal and still yields the XP.
    // This keeps item-drop validation separate from the experience reward.
    //
    // No enchantment is modelled here, so no tool-specific experience modifier
    // is applied.
    if drop_loot {
        let points = crate::experience::block_break_points(broken.block().name(), |bound| {
            drops_rng.next_int(bound)
        });
        if points > 0 {
            let centre = Vec3::new(
                f64::from(pos.x) + 0.5,
                f64::from(pos.y) + 0.5,
                f64::from(pos.z) + 0.5,
            );
            mobs.with(|sim| {
                sim.award_experience(centre, Vec3::new(0.0, 0.0, 0.0), points);
            });
        }
    }
    let drops_elapsed = started.elapsed();
    block_entities.with(|reg| {
        reg.remove(pos);
    });
    if open_container.as_ref().is_some_and(|open| open.pos == pos) {
        *open_container = None;
        *container_sync = ContainerSync::default();
    }
    // Fluid spread's seeding hook (`crate::fluid`). Breaking a block is the
    // single most common way a player starts a fluid moving — mine the floor of
    // an ocean, or the block beside a spring — and it is exactly the
    // neighbor-changed case: the *water* did not change, so only a notification
    // can wake it. `ticks_after_edit` reads this cell and its six neighbours to
    // decide which of them already hold a fluid, and schedules only those.
    //
    // Deliberately **not** folded into `propagate_placement`, whose return value
    // several gates assert on exactly. This is its own request against the same
    // feed, and `run_tick_loop`'s rebase loop routes it to the fluid queue.
    block_ticks.request_fluid_scheduled_ticks(crate::fluid::ticks_after_edit(
        source,
        fluid_env_at(source, pos),
        pos,
    ));
    let directive = proto.encode_block_update(pos.x, pos.y, pos.z, new_state);
    apply(conn, state, directive).await?;
    // Breaking a light source has to darken the column, and the `BLOCK_UPDATE`
    // above carries no light. See `crate::light` for why this is a column resend
    // rather than a `LIGHT_UPDATE`. `new_state` rather than a hardcoded `AIR`
    // for the same reason as the write above: a broken waterlogged block keeps
    // a light-dampening fluid in the cell, not empty air.
    resend_column_for_light(
        conn,
        proto,
        source,
        state,
        pending_relights.as_deref_mut(),
        broken,
        new_state,
        pos,
    ).await?;
    let light_elapsed = started.elapsed();

    // A break runs two neighbour passes: shape recomputation (a torch or rail
    // that loses support turns to air) followed by redstone and gravity
    // reactions. The shape pass precedes the neighbour-notification pass.
    let mut collapsed = collapse_unsupported(source, pos);
    // Portal validation is a *second* shape pass, alongside
    // `block_support`'s survives table
    // `collapse_unsupported` already runs above — a broken frame block must
    // extinguish the portal cells it was holding up, which
    // `collapse_unsupported` cannot see (a portal is not "supported by one
    // specific neighbour"; it is re-validated against its whole frame). See
    // `crate::portal::extinguish_broken_frames`'s own doc comment. Extends
    // `collapsed` (same `(pos, state_before)` shape) rather than a second
    // list, so the `block_update`/relight/fan-out code below needs no new
    // branch to reach it.
    if let Some(dimension) = source.dimension() {
        collapsed.extend(crate::portal::extinguish_broken_frames(source, dimension, pos));
    }
    // The update-or-destroy → destroy-block → drop-resources chain.
    //
    // **Gated on `cascade_drops`, not on `drop_loot`.** The creative no-drop
    // applies only to the block *the player broke*, while a cell that
    // self-destructs has no player context. A creative player mining the dirt
    // under a flower therefore gets the flower; reusing `drop_loot` here would
    // silently eat it.
    //
    // The tool is not consulted either: the update-or-destroy routine reaches the
    // three-argument drop-resources call, which carries no
    // tool loot-context parameter — hence `None` rather than `held`, and no
    // `drops_are_allowed` call.
    if cascade_drops {
        for (cell, was) in &collapsed {
            let popped = crate::block_drops::drop_block_loot(
                crate::block_drops::bundled_tables(),
                *was,
                *cell,
                None,
                drops_rng,
            );
            if popped.is_empty() {
                continue;
            }
            mobs.with(|sim| {
                for drop in popped {
                    let count = u8::try_from(drop.stack.count).unwrap_or(u8::MAX);
                    sim.spawn_item(
                        drop.stack.item.clone(),
                        drop.position,
                        drop.velocity,
                        ItemLifecycle::newly_dropped(count, DEFAULT_MAX_STACK_SIZE),
                    );
                }
            });
        }
    }
    let mut fanned: Vec<(BlockPos, StateId)> = Vec::new();
    // Vanilla's own tripwire-block affect-neighbors-after-removal routine — the "the string just broke"
    // instant pulse. `broken` is `pos`'s own state from *before* this function
    // overwrote it, exactly what `propagate_removal_with_entities` needs; a
    // no-op for every block that is not a tripwire.
    {
        let (mut changed, scheduled) = propagate_removal_with_entities(source, pos, broken);
        block_ticks.request_scheduled_ticks(scheduled);
        fanned.append(&mut changed);
    }
    let mut fan_origins: Vec<BlockPos> = vec![pos];
    fan_origins.extend(collapsed.iter().map(|(cell, _)| *cell));
    for origin in fan_origins {
        let (mut changed, scheduled) = propagate_placement_with_entities(source, origin, Some(block_entities));
        block_ticks.request_scheduled_ticks(scheduled);
        fanned.append(&mut changed);
    }
    // The collapsed cells and then whatever the fan-out rewrote, deduped and with
    // `pos` excluded (it already had its own `block_update` above).
    let mut notify: Vec<BlockPos> = Vec::new();
    for cell in collapsed
        .iter()
        .map(|(cell, _)| *cell)
        .chain(fanned.iter().map(|(cell, _)| *cell))
    {
        if cell != pos && !notify.contains(&cell) {
            notify.push(cell);
        }
    }
    for cell in notify {
        let current = source.block_state_id(cell.x, cell.y, cell.z);
        let directive = proto.encode_block_update(cell.x, cell.y, cell.z, current);
        apply(conn, state, directive).await?;
        block_ticks.request_fluid_scheduled_ticks(crate::fluid::ticks_after_edit(
            source,
            fluid_env_at(source, cell),
            cell,
        ));
    }
    // A popped torch or lantern has to darken its column too. `should_relight`
    // compares the two states' emission and dampening, so a collapsed flower
    // costs nothing here. Re-read rather than assume `AIR`: `collapse_unsupported`
    // may have left a fluid's legacy block behind, which dampens light
    // differently than empty air.
    for (cell, was) in &collapsed {
        let now = source.block_state_id(cell.x, cell.y, cell.z);
        resend_column_for_light(
            conn,
            proto,
            source,
            state,
            pending_relights.as_deref_mut(),
            *was,
            now,
            *cell,
        ).await?;
    }
    let total = started.elapsed();
    if total >= Duration::from_millis(50) {
        tracing::warn!(
            target: "lodestone_server::stall",
            x = pos.x,
            y = pos.y,
            z = pos.z,
            total_ms = total.as_millis(),
            edit_ms = edit_elapsed.as_millis(),
            drops_ms = drops_elapsed.saturating_sub(edit_elapsed).as_millis(),
            light_ms = light_elapsed.saturating_sub(drops_elapsed).as_millis(),
            fanout_ms = total.saturating_sub(light_elapsed).as_millis(),
            "block break delayed the connection loop",
        );
    }
    Ok(())
}

/// Vanilla's own `maxChainedNeighborUpdates` for the support cascade specifically.
///
/// The tallest real chain is a bamboo or sugar-cane column (16 at the very most)
/// or a two-cell door, so this is a runaway guard rather than a behavioural
/// limit — but it has to exist, because [`collapse_unsupported`] re-queues the
/// neighbours of every cell it removes and a data error in
/// [`crate::block_support`] would otherwise walk the world.
const MAX_SUPPORT_COLLAPSE: usize = 64;

/// Runs vanilla's `updateNeighbourShapes` self-destruct pass around `origin`,
/// transitively: every cell whose support [`crate::block_support`] models and
/// whose support cell is now gone becomes air, drops its loot, and has its own
/// neighbours re-examined.
///
/// Returns `(pos, state_before)` for each removed cell, already written to air in
/// `source`, so the caller can send the `block_update`s, roll the loot and
/// relight. The drops are deliberately **not** rolled here: this function needs no
/// `MobHandle` and no RNG, which is what lets `crate::support_collapse_gate` drive
/// the production cascade against a rig world rather than a copy of it.
pub(crate) fn collapse_unsupported<S>(source: &S, origin: BlockPos) -> Vec<(BlockPos, StateId)>
where
    S: ChunkSource + ?Sized,
{
    let mut removed: Vec<(BlockPos, StateId)> = Vec::new();
    let mut queue: VecDeque<BlockPos> = crate::neighbor_update::ALL_DIRECTIONS
        .iter()
        .map(|d| d.relative(origin))
        .collect();
    while let Some(cell) = queue.pop_front() {
        if removed.len() >= MAX_SUPPORT_COLLAPSE {
            tracing::warn!(
                "support collapse from {origin:?} hit its {MAX_SUPPORT_COLLAPSE}-cell bound"
            );
            break;
        }
        if removed.iter().any(|(seen, _)| *seen == cell) {
            continue;
        }
        let was = source.block_state_id(cell.x, cell.y, cell.z);
        if crate::chunk::is_air_or_fluid_id(was) {
            continue;
        }
        if crate::block_support::survives(cell, was, |probe| {
            source.block_state_id(probe.x, probe.y, probe.z)
        }) {
            continue;
        }
        // Removing a block with a fluid state writes that fluid's block state
        // rather than literal air. A waterlogged sign therefore leaves its
        // water source behind when the support block collapses; see
        // `destroy_block`'s `new_state` for the same rule.
        let new_state = crate::fluid::fluid_state_of_id(was)
            .map(crate::fluid::FluidState::block_state_id)
            .unwrap_or_else(|| Block::Air.default_state());
        source.set_block(cell.x, cell.y, cell.z, new_state);
        removed.push((cell, was));
        // The removed cell's own neighbours: this is what makes a stack of sugar
        // cane collapse all the way up, and a door's upper half follow its lower.
        for direction in crate::neighbor_update::ALL_DIRECTIONS {
            queue.push_back(direction.relative(cell));
        }
    }
    removed
}

/// Re-sends the column owning `pos` when an edit changed the light that cell
/// emits, so the client's block light follows a placed or broken torch.
///
/// A no-op unless [`crate::light::should_relight`] fires — read that module's doc
/// comment first: it records what the served-light path was measured to actually
/// compute, why this function uses a whole-column resend rather than the `LIGHT_UPDATE`
/// packet that would be cheaper, and the two gaps this leaves (sky light after an
/// edit, and light crossing a column border).
///
/// `source.column(cx, cz)` reflects the `set_block` the caller already performed
/// That contract means the light is computed over terrain
/// that contains the torch.
///
/// # It is a `light_update`, not a column resend
///
/// The stopgap this replaces re-encoded the **whole column**: ~40 KiB on the wire
/// and 62 M instructions of `encode_chunk`, per placed torch, on the connection
/// task. `ServerProtocol::encode_light_update` is the real packet — a few KiB of
/// nibble arrays — and it needs no chunk batch, because vanilla's
/// `PlayerChunkSender` flow control counts chunk *batches* and `light_update` is
/// not one. Vanilla sends it the same way, ungated, from
/// `ChunkMap`'s light listener.
///
async fn resend_column_for_light<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    pending_relights: Option<&mut PendingRelights>,
    old_state: StateId,
    new_state: StateId,
    pos: BlockPos,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    if !crate::light::should_relight(old_state, new_state) {
        return Ok(());
    }
    if let Some(pending_relights) = pending_relights {
        pending_relights.enqueue_edit(
            pos.x.div_euclid(16),
            pos.z.div_euclid(16),
            i32::from(proto.uses_cross_column_light()),
        );
        return Ok(());
    }
    send_lighting_for_edit(
        conn,
        proto,
        source,
        state,
        pos.x.div_euclid(16),
        pos.z.div_euclid(16),
    )
    .await
}

/// Recompute and send one column's light after a caller has established that an
/// edit changed emission or dampening, or when the prior block state is unknown.
async fn send_column_light<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    cx: i32,
    cz: i32,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let dimension = source
        .dimension()
        .unwrap_or(crate::dimension::Dimension::Overworld);
    let (column, light) = if proto.retains_initial_column_light() {
        let neighbour_offsets = light_neighbour_offsets(proto.uses_cross_column_light());
        let mut fallback = source.column(cx, cz);
        'settle: {
            for attempt in 0..=LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES {
            let exclusive = attempt == LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES;
            let mut compute = |candidate: &ChunkColumn,
                               neighbours: &[(i32, i32, &ChunkColumn)]| {
                if proto.uses_cross_column_light() {
                    proto.compute_column_light_with_neighbours_in_dimension(
                        candidate,
                        neighbours,
                        dimension,
                    )
                } else {
                    proto.compute_column_light_in_dimension(candidate, dimension)
                }
            };
            match source.settle_resident_column_light_with_neighbours(
                cx,
                cz,
                &fallback,
                &neighbour_offsets,
                false,
                true,
                exclusive,
                &mut compute,
            ) {
                Ok(column) => break 'settle (column.clone(), column.centre_settled_light().cloned()),
                Err(ColumnLightSettlementError::NoLight)
                | Err(ColumnLightSettlementError::MissingFootprint) => {
                    break 'settle (fallback, None)
                }
                Err(ColumnLightSettlementError::Conflict) if !exclusive => {
                    fallback = source.column(cx, cz);
                }
                Err(ColumnLightSettlementError::Conflict) => return Ok(()),
            }
            }
            unreachable!("the bounded dynamic-light settlement loop always returns")
        }
    } else {
        let column = source.column(cx, cz);
        // Both halves have to be present for the cheap path: a family that can
        // compute light but not encode the packet (or the reverse) would
        // otherwise silently send nothing, which is the exact island this
        // replaces.
        let light = if proto.uses_cross_column_light() {
            let neighbours = (-1..=1)
                .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
                .filter(|&(dx, dz)| (dx, dz) != (0, 0))
                .map(|(dx, dz)| (dx, dz, source.column(cx + dx, cz + dz)))
                .collect::<Vec<_>>();
            let neighbour_refs = borrowed_neighbours(&neighbours);
            proto.compute_column_light_with_neighbours_in_dimension(
                &column,
                &neighbour_refs,
                dimension,
            )
        } else {
            proto.compute_column_light_in_dimension(&column, dimension)
        };
        (column, light)
    };
    if let Some(light) = light {
        let directive = proto.encode_light_update(cx, cz, &light);
        if !matches!(directive, ServerDirective::None) {
            apply(conn, state, directive).await?;
            return Ok(());
        }
    }
    // Fallback: the whole-column resend, inside the same
    // `begin_chunk_batch`/`end_chunk_batch` pair every other chunk send in this
    // module uses — the batch accounting counts these directives, so a bare
    // `encode_chunk` outside one leaves the client's accounting short.
    apply(conn, state, proto.begin_chunk_batch()).await?;
    let packet_column = column_for_initial_encode(&column);
    let directive = match proto.try_encode_chunk_in_dimension(cx, cz, &packet_column, dimension) {
        Ok(directive) => directive,
        Err(error) => return return_chunk_encode_error(conn, proto, state, Some(0), error).await,
    };
    apply(conn, state, directive).await?;
    apply(conn, state, proto.end_chunk_batch(1)).await?;
    Ok(())
}

/// Clones the exact footprint needed for one live light update without turning
/// an unloaded neighbour into a synchronous generation request.
fn resident_light_neighbourhood<S>(
    source: &S,
    cx: i32,
    cz: i32,
    radius: i32,
) -> Option<(ChunkColumn, Vec<(i32, i32, ChunkColumn)>)>
where
    S: ChunkSource + ?Sized,
{
    let column = resident_column(source, cx, cz)?;
    let mut neighbours = Vec::new();
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            if (dx, dz) != (0, 0) {
                neighbours.push((dx, dz, resident_column(source, cx + dx, cz + dz)?));
            }
        }
    }
    Some((column, neighbours))
}

fn settle_resident_light_snapshot<S: ChunkSource + ?Sized>(
    source: &S,
    cx: i32,
    cz: i32,
    neighbour_offsets: &[(i32, i32)],
    resident_only: bool,
    compute: &mut dyn FnMut(
        &ChunkColumn,
        &[(i32, i32, &ChunkColumn)],
    ) -> Option<lodestone_world::ColumnLight>,
) -> Option<(ChunkColumn, Option<lodestone_world::ColumnLight>)> {
    let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightSettlement, 1);
    let mut fallback = if resident_only {
        resident_column(source, cx, cz)?
    } else {
        source.column(cx, cz)
    };
    for attempt in 0..=LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES {
        let exclusive = attempt == LIGHT_SETTLEMENT_OPTIMISTIC_RETRIES;
        match source.settle_resident_column_light_with_neighbours(
            cx,
            cz,
            &fallback,
            neighbour_offsets,
            resident_only,
            true,
            exclusive,
            compute,
        ) {
            Ok(column) => {
                let light = column.centre_settled_light().cloned();
                return Some((column, light));
            }
            Err(ColumnLightSettlementError::MissingFootprint) => return None,
            Err(ColumnLightSettlementError::NoLight) => return Some((fallback, None)),
            Err(ColumnLightSettlementError::Conflict) if !exclusive => {
                fallback = if resident_only {
                    resident_column(source, cx, cz)?
                } else {
                    source.column(cx, cz)
                };
            }
            Err(ColumnLightSettlementError::Conflict) => return None,
        }
    }
    unreachable!("the bounded resident-light settlement loop always returns")
}

/// Sends one changed column's light from a snapshot that is already resident.
/// Unlike [`send_column_light`], this never generates terrain while running a
/// connection timer. A missing member means the future chunk snapshot, not a
/// live relight, is responsible for carrying the world-tick mutation.
async fn send_resident_column_light<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    cx: i32,
    cz: i32,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let radius = i32::from(proto.uses_cross_column_light());
    let dimension = source
        .dimension()
        .unwrap_or(crate::dimension::Dimension::Overworld);
    let (column, light) = if proto.retains_initial_column_light() {
        let neighbour_offsets = light_neighbour_offsets(proto.uses_cross_column_light());
        let mut compute = |candidate: &ChunkColumn,
                           neighbours: &[(i32, i32, &ChunkColumn)]| {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightCompute, 1);
            if radius != 0 {
                proto.compute_column_light_with_neighbours_in_dimension(
                    candidate,
                    neighbours,
                    dimension,
                )
            } else {
                proto.compute_column_light_in_dimension(candidate, dimension)
            }
        };
        let Some(settled) = settle_resident_light_snapshot(
            source,
            cx,
            cz,
            &neighbour_offsets,
            true,
            &mut compute,
        ) else {
            return Ok(());
        };
        settled
    } else {
        let Some((column, neighbours)) = resident_light_neighbourhood(source, cx, cz, radius) else {
            return Ok(());
        };
        let light = if radius != 0 {
            let neighbour_refs = borrowed_neighbours(&neighbours);
            proto.compute_column_light_with_neighbours_in_dimension(
                &column,
                &neighbour_refs,
                dimension,
            )
        } else {
            proto.compute_column_light_in_dimension(&column, dimension)
        };
        (column, light)
    };
    if let Some(light) = light {
        let directive = {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightEncode, 1);
            proto.encode_light_update(cx, cz, &light)
        };
        if !matches!(directive, ServerDirective::None) {
            apply(conn, state, directive).await?;
            return Ok(());
        }
    }
    // The captured centre keeps the protocol's full-column fallback non-generating.
    apply(conn, state, proto.begin_chunk_batch()).await?;
    let packet_column = column_for_initial_encode(&column);
    let directive = match proto.try_encode_chunk_in_dimension(cx, cz, &packet_column, dimension) {
        Ok(directive) => directive,
        Err(error) => return return_chunk_encode_error(conn, proto, state, Some(0), error).await,
    };
    apply(conn, state, directive).await?;
    apply(conn, state, proto.end_chunk_batch(1)).await?;
    Ok(())
}

/// Sends every world-tick block mutation promptly and queues lighting once per
/// affected delivered column.
///
/// This deliberately has no join-stream gate. A column snapshot that has not
/// been sent yet will supersede an earlier block update, but a snapshot that
/// was already sent will not. Dropping the shared feed while a later join
/// column remains would therefore leave an already-visible column stale.
async fn send_tick_block_updates<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    delivered: &HashSet<(i32, i32)>,
    pending_relights: &mut PendingRelights,
    changes: Vec<crate::tick::TickBlockChange>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    if changes.is_empty() {
        return Ok(());
    }
    let started = lodestone_time::Instant::now();
    let change_count = changes.len();
    let radius = i32::from(proto.uses_cross_column_light());
    for change in &changes {
        let (x, y, z, block_state) = (change.x, change.y, change.z, change.state);
        let column = (x.div_euclid(16), z.div_euclid(16));
        if delivered.contains(&column) {
            apply(conn, state, proto.encode_block_update(x, y, z, block_state)).await?;
        }
    }
    let queued_relight_count = pending_relights.enqueue_batch(tick_relight_targets(
        changes.iter().filter(|change| change.needs_relight).map(|change| {
            (change.x.div_euclid(16), change.z.div_euclid(16))
        }),
        delivered,
        radius,
    ));
    let total = started.elapsed();
    if total >= STALL_REPORT {
        tracing::warn!(
            target: "lodestone_server::stall",
            change_count,
            queued_relight_count,
            pending_relight_count = pending_relights.len(),
            total_millis = total.as_millis() as u64,
            "tick block updates stalled the connection loop",
        );
    }
    Ok(())
}

const TICK_BLOCK_UPDATE_BATCH: usize = 64;

fn queue_tick_block_updates(
    pending: &mut VecDeque<crate::tick::TickBlockChange>,
    delivered: &HashSet<(i32, i32)>,
    changes: Vec<crate::tick::TickBlockChange>,
) {
    pending.extend(changes.into_iter().filter(|change| {
        delivered.contains(&(change.x.div_euclid(16), change.z.div_euclid(16)))
    }));
}

async fn send_pending_tick_block_updates<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    delivered: &HashSet<(i32, i32)>,
    pending_relights: &mut PendingRelights,
    pending: &mut VecDeque<crate::tick::TickBlockChange>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    let count = pending.len().min(TICK_BLOCK_UPDATE_BATCH);
    let changes = pending.drain(..count).collect();
    send_tick_block_updates(conn, proto, state, delivered, pending_relights, changes).await
}

fn tick_relight_targets(
    changed_columns: impl IntoIterator<Item = (i32, i32)>,
    delivered: &HashSet<(i32, i32)>,
    radius: i32,
) -> HashSet<(i32, i32)> {
    let mut targets = HashSet::new();
    for (cx, cz) in changed_columns {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let affected = (cx + dx, cz + dz);
                if delivered.contains(&affected) {
                    targets.insert(affected);
                }
            }
        }
    }
    targets
}

#[derive(Default)]
struct PendingRelights {
    queued: VecDeque<(i32, i32)>,
    queued_set: HashSet<(i32, i32)>,
    generation_required: HashSet<(i32, i32)>,
    retry_at: Option<lodestone_time::Instant>,
    retry_single: bool,
}

struct RelightBatch {
    coordinates: Vec<(i32, i32)>,
    generation_required: Vec<(i32, i32)>,
}

enum RelightBatchOutcome {
    Committed(Vec<((i32, i32), lodestone_world::ColumnLight)>),
    Deferred,
    Unsupported,
    Failed(ChunkEncodeError),
}

impl PendingRelights {
    fn enqueue_batch(&mut self, positions: impl IntoIterator<Item = (i32, i32)>) -> usize {
        let mut positions = positions.into_iter().collect::<Vec<_>>();
        positions.sort_unstable();
        positions.dedup();
        let mut added = 0;
        for position in positions {
            if self.queued_set.insert(position) {
                self.queued.push_back(position);
                added += 1;
            }
        }
        if added != 0 {
            self.retry_at = None;
        }
        added
    }

    fn enqueue_edit(&mut self, cx: i32, cz: i32, radius: i32) {
        self.retry_at = None;
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let coordinate = (cx + dx, cz + dz);
                if self.queued_set.insert(coordinate) {
                    self.queued.push_back(coordinate);
                }
                self.generation_required.insert(coordinate);
            }
        }
    }

    fn pop_front(&mut self) -> Option<(i32, i32)> {
        let position = self.queued.pop_front()?;
        self.queued_set.remove(&position);
        self.generation_required.remove(&position);
        Some(position)
    }

    fn requires_generation(&self, coordinate: (i32, i32)) -> bool {
        self.generation_required.contains(&coordinate)
    }

    fn front(&self) -> Option<(i32, i32)> {
        self.queued.front().copied()
    }

    fn clear(&mut self) {
        self.queued.clear();
        self.queued_set.clear();
        self.generation_required.clear();
        self.retry_at = None;
        self.retry_single = false;
    }

    fn ready(&self) -> bool {
        !self.is_empty() && self.retry_at.is_none_or(|at| lodestone_time::Instant::now() >= at)
    }

    fn batch(&self, delivered: &HashSet<(i32, i32)>, shared: bool) -> RelightBatch {
        let Some(anchor) = self.front() else {
            return RelightBatch { coordinates: Vec::new(), generation_required: Vec::new() };
        };
        let coordinates = self.queued.iter().copied().filter(|&(cx, cz)| {
            delivered.contains(&(cx, cz)) && (if shared && !self.retry_single {
                (i64::from(cx) - i64::from(anchor.0)).abs() <= 1
                    && (i64::from(cz) - i64::from(anchor.1)).abs() <= 1
            } else {
                (cx, cz) == anchor
            })
        }).take(9).collect::<Vec<_>>();
        let generation_required = coordinates.iter().copied()
            .filter(|&coordinate| self.requires_generation(coordinate)).collect();
        RelightBatch { coordinates, generation_required }
    }

    fn admit(&mut self, batch: &RelightBatch) {
        self.retry_single = false;
        self.queued.retain(|coordinate| !batch.coordinates.contains(coordinate));
        for coordinate in &batch.coordinates {
            self.queued_set.remove(coordinate);
            self.generation_required.remove(coordinate);
        }
    }

    fn defer(&mut self, batch: &RelightBatch) {
        for &coordinate in batch.coordinates.iter().cycle().skip(1).take(batch.coordinates.len()) {
            if self.queued_set.insert(coordinate) {
                self.queued.push_back(coordinate);
            }
        }
        self.generation_required.extend(batch.generation_required.iter().copied()
            .filter(|coordinate| batch.coordinates.contains(coordinate)));
        self.retry_at = Some(lodestone_time::Instant::now() + Duration::from_millis(50));
        self.retry_single = true;
    }

    fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    fn len(&self) -> usize {
        self.queued.len()
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct DetachedRelight {
    batch: RelightBatch,
    handle: crate::worldgen_dispatch::DispatchHandle<RelightBatchOutcome>,
}

fn relight_transaction_outcome(error: crate::chunk::ResidentLightTransactionError) -> RelightBatchOutcome {
    use crate::chunk::ResidentLightTransactionError;
    match error {
        ResidentLightTransactionError::Busy | ResidentLightTransactionError::MissingFootprint
        | ResidentLightTransactionError::Conflict => RelightBatchOutcome::Deferred,
        ResidentLightTransactionError::InvalidOutputs => RelightBatchOutcome::Failed(
            ChunkEncodeError::new("resident lighting rejected an invalid output batch"),
        ),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn compute_detached_relight_batch(
    source: &dyn ChunkSource,
    coordinates: &[(i32, i32)],
    dimension: crate::dimension::Dimension,
    compute: crate::protocol::ResidentLightBatchCompute,
) -> RelightBatchOutcome {
    let footprint = match lodestone_world::ResidentLightFootprint::new(coordinates.iter().copied()) {
        Ok(footprint) => footprint,
        Err(error) => return RelightBatchOutcome::Failed(ChunkEncodeError::new(format!("resident light footprint: {error:?}"))),
    };
    let transaction = match source.try_begin_resident_light(coordinates, footprint.inputs()) {
        Some(Ok(transaction)) => transaction,
        Some(Err(error)) => return relight_transaction_outcome(error),
        None => return RelightBatchOutcome::Unsupported,
    };
    let lights = match compute(coordinates, transaction.columns(), dimension) {
        Ok(lights) => lights,
        Err(error) => return RelightBatchOutcome::Failed(ChunkEncodeError::new(format!("resident light solve: {error:?}"))),
    };
    match transaction.commit(lights.clone()) {
        Ok(()) => RelightBatchOutcome::Committed(lights),
        Err(error) => relight_transaction_outcome(error),
    }
}

#[cfg(target_arch = "wasm32")]
struct CooperativeRelight<'a> {
    batch: RelightBatch,
    future: std::pin::Pin<Box<dyn std::future::Future<Output = RelightBatchOutcome> + 'a>>,
}

#[cfg(target_arch = "wasm32")]
async fn compute_cooperative_relight<P: ServerProtocol, S: ChunkSource + ?Sized>(
    proto: &P,
    source: &S,
    coordinates: Vec<(i32, i32)>,
) -> RelightBatchOutcome {
    let footprint = match lodestone_world::ResidentLightFootprint::new(coordinates.iter().copied()) {
        Ok(footprint) => footprint,
        Err(error) => return RelightBatchOutcome::Failed(ChunkEncodeError::new(format!("resident light footprint: {error:?}"))),
    };
    let transaction = {
        let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightSettlement, coordinates.len() as u32);
        match source.try_begin_resident_light(&coordinates, footprint.inputs()) {
            Some(Ok(transaction)) => transaction,
            Some(Err(error)) => return relight_transaction_outcome(error),
            None => return RelightBatchOutcome::Unsupported,
        }
    };
    let dimension = source.dimension().unwrap_or(crate::dimension::Dimension::Overworld);
    let lights = {
        let Some(compute) = proto.compute_resident_light_batch(&coordinates, transaction.columns(), dimension) else {
            return RelightBatchOutcome::Unsupported;
        };
        match compute.await {
            Ok(lights) => lights,
            Err(error) => return RelightBatchOutcome::Failed(ChunkEncodeError::new(format!("resident light solve: {error:?}"))),
        }
    };
    let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightSettlement, 0);
    match transaction.commit(lights.clone()) {
        Ok(()) => RelightBatchOutcome::Committed(lights),
        Err(error) => relight_transaction_outcome(error),
    }
}

async fn send_committed_relights<T: Transport, P: ServerProtocol, S: ChunkSource + ?Sized>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    delivered: &HashSet<(i32, i32)>,
    pending: &mut PendingRelights,
    lights: Vec<((i32, i32), lodestone_world::ColumnLight)>,
) -> Result<(), ServerError> {
    for (coordinate, light) in lights {
        if !delivered.contains(&coordinate) {
            continue;
        }
        let current = match source.try_resident_column(coordinate.0, coordinate.1) {
            Some(crate::chunk_store::TryResident::Present(column)) => Some(column),
            Some(_) => None,
            None => resident_column(source, coordinate.0, coordinate.1),
        };
        if !current.as_ref().is_some_and(|column| column.centre_settled_light() == Some(&light)) {
            pending.enqueue_batch([coordinate]);
            continue;
        }
        let directive = {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightEncode, 1);
            proto.encode_light_update(coordinate.0, coordinate.1, &light)
        };
        if matches!(directive, ServerDirective::None) {
            send_resident_column_light(conn, proto, source, state, coordinate.0, coordinate.1).await?;
        } else {
            apply(conn, state, directive).await?;
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn compute_detached_relight(
    source: &dyn ChunkSource,
    coordinate: (i32, i32),
    dimension: crate::dimension::Dimension,
    cross_column: bool,
    resident_only: bool,
    compute: crate::protocol::DetachedLightCompute,
) -> Option<lodestone_world::ColumnLight> {
    let offsets = light_neighbour_offsets(cross_column);
    let mut calculate = |column: &ChunkColumn, neighbours: &[(i32, i32, &ChunkColumn)]| {
        let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightCompute, 1);
        Some(compute(column, neighbours, dimension))
    };
    settle_resident_light_snapshot(
        source,
        coordinate.0,
        coordinate.1,
        &offsets,
        resident_only,
        &mut calculate,
    )?
    .1
}

async fn send_next_relight<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    delivered: &HashSet<(i32, i32)>,
    pending_relights: &mut PendingRelights,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let Some((cx, cz)) = pending_relights.pop_front() else {
        return Ok(());
    };
    if delivered.contains(&(cx, cz)) {
        crate::worldgen_progress::measure_polls(
            WorldgenTimingPhase::ConnectionRelight,
            std::pin::pin!(send_resident_column_light(conn, proto, source, state, cx, cz)),
        ).await?;
    }
    Ok(())
}

/// Recomputes every column a boundary edit can affect. A light source can cross
/// either seam and a corner, so the correct bounded footprint is the edited
/// column plus all eight neighbours; the light engine's 15-block radius cannot
/// reach beyond that 3×3 footprint.
async fn send_lighting_for_edit<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    cx: i32,
    cz: i32,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    let radius = i32::from(proto.uses_cross_column_light());
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            send_column_light(conn, proto, source, state, cx + dx, cz + dz).await?;
        }
    }
    Ok(())
}

/// Collects every dropped item within this player's pickup volume into their
/// inventory, and returns the native slots that changed (the item-pickup link).
///
/// This is the per-tick item-entity pickup → inventory-add chain, minus the
/// XP-orb branch. See
/// [`crate::block_drops::is_within_pickup_range`] for the volume and
/// [`PlayerInventory::add`] for the destination order — both are behavior that
/// a plausible simplification gets wrong.
///
/// # Why the whole thing happens inside one `mobs.with`
///
/// Query-then-remove across two lock acquisitions is a duplication bug with a
/// player on each side of it: two connections whose volumes overlap the same
/// drop would both see it collectable, both credit it, and one `remove_item`
/// would return `false` while the item had already been banked twice. Deciding
/// and removing under a single lock makes the loser's `remove_item` the thing
/// that fails, and it fails *before* the inventory write, so nothing is
/// duplicated.
///
/// # A full inventory leaves the item in the world
///
/// [`PlayerInventory::add`] reports its leftover, and the entity is removed
/// only when the inventory consumed everything.
/// A partial pickup therefore credits what fitted and puts the unfitted items back as
/// the item's new count — the entity stays, visibly, rather than the surplus
/// vanishing.
/// # Statistics and advancements
///
/// This is also the `minecraft:inventory_changed` seam, so it is where
/// [`AdvancementManager::on_inventory_changed`] and the `minecraft:picked_up`
/// counter are driven from. Both are credited **per item actually banked**, not
/// per entity seen: a pickup that only partly fitted credits what fitted, and one
/// that fitted nothing credits nothing — the same `written`/`leftover` split the
/// slot updates already key off.
/// One item entity a player just took, for [`ServerProtocol::encode_take_item_entity`].
#[derive(Debug, Clone, Copy)]
struct TakenItem {
    item_entity_id: i32,
    /// The entity's stack count **before** the inventory took any of it. Not the
    /// amount banked; see the encoder's own doc.
    amount: i32,
}

/// What one pickup pass produced: the inventory slots to resend, and the takes to
/// announce.
///
/// **The takes are returned rather than sent here because the ordering matters more
/// than the plumbing.** The client keeps the item entity alive to interpolate it and
/// removes it once the animation finishes, so `TAKE_ITEM_ENTITY` has to reach the wire
/// *before* the `REMOVE_ENTITIES` that `stream_pass` derives from this same removal.
/// Returning them puts that ordering in the caller, where `stream_pass` is visible;
/// sending from inside `mobs.with` would also mean awaiting under the sim lock.
#[derive(Debug, Default)]
struct Pickups {
    /// Native inventory slot indices whose contents changed.
    changed: Vec<usize>,
    /// Items taken this pass, in pickup order.
    takes: Vec<TakenItem>,
}

fn collect_nearby_items(
    mobs: &MobHandle,
    inventory: &mut PlayerInventory,
    player_feet: Vec3,
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
    // The world clock in milliseconds — `game_time * 50`. Vanilla stamps a
    // criterion with a real `Instant`; this crate must not call
    // `std::time::Instant::now()` anywhere in `lodestone-server`, because the crate
    // links into a wasm32 bundle where that compiles and then panics at runtime
    // under `panic = "abort"` with no log line. A tick-derived value is monotonic,
    // wasm-safe, and means "ms of world time", which is a more useful stamp for a
    // save file than wall clock anyway.
    obtained_millis: i64,
) -> Pickups {
    let mut changed: Vec<usize> = Vec::new();
    let mut takes: Vec<TakenItem> = Vec::new();
    mobs.with(|sim| {
        for (id, item, count) in sim.items_within_pickup_range(player_feet) {
            let stack = ItemStack::new(item, u32::from(count));
            let picked_up_key = crate::advancements::StatKey::new(
                crate::advancements::StatType::PickedUp,
                stack.item.to_string(),
            );
            let item_id = stack.item.to_string();
            let offered = stack.count;
            let (written, leftover) = inventory.add(stack);
            let banked = offered.saturating_sub(leftover.as_ref().map_or(0, |left| left.count));
            match leftover {
                None => {
                    // Fully banked, so the entity goes. `remove_item` returning
                    // `false` would mean another connection took it between the
                    // query and here — impossible under this one lock, which is
                    // the property the doc comment above is about.
                    sim.remove_item(id);
                }
                Some(remaining) => {
                    // Partial fit (or none at all): the inventory keeps what
                    // fitted and the entity keeps the rest, exactly as vanilla's
                    // in-place `ItemStack` shrink does. Clamped into `u8`
                    // because that is what the lifecycle counts in; `remaining`
                    // can never exceed the `count` we started from, which came
                    // from that same `u8`.
                    let left = u8::try_from(remaining.count).unwrap_or(u8::MAX);
                    if left == 0 {
                        sim.remove_item(id);
                    } else {
                        sim.set_item_count(id, left);
                    }
                }
            }
            // How much actually landed in the inventory. `leftover` is what did
            // not, so the banked amount is the difference — credited rather than
            // the offered count, so a full inventory credits nothing.
            if banked > 0 {
                advancements.award_stat(
                    player_uuid,
                    picked_up_key,
                    i32::try_from(banked).unwrap_or(i32::MAX),
                );
                // Vanilla's `inventory_changed` trigger. Fires once per pickup
                // regardless of stack size, because a criterion is satisfied by
                // *having* the item, not by how many.
                advancements.on_inventory_changed(player_uuid, &item_id, obtained_millis);
            }
            // The pickup *animation* cue. Gated on `banked > 0` because the
            // animation belongs only to a transfer that placed at least one
            // item. A pickup into a full inventory shows nothing, which is right:
            // nothing was taken.
            //
            // `offered`, not `banked`: vanilla passes `orgCount`, captured *before*
            // `add` shrinks the stack in place. The two differ exactly when the
            // pickup is partial, and `orgCount` is what drives the client's sound
            // pitch. See `ServerProtocol::encode_take_item_entity`.
            if banked > 0 {
                takes.push(TakenItem {
                    item_entity_id: id,
                    amount: i32::try_from(offered).unwrap_or(i32::MAX),
                });
            }
            for slot in written {
                if !changed.contains(&slot) {
                    changed.push(slot);
                }
            }
        }
    });
    Pickups { changed, takes }
}

/// Vanilla's own `takeXpDelay` field, the value its own experience-orb player-touch routine resets it to.
///
/// Two ticks, so a player standing in a pile absorbs one orb every other tick rather
/// than all of them at once. It is what makes a big drop *sound* and *look* like a
/// stream of orbs instead of a single silent jump on the bar, and it is the only thing
/// limiting the absorption rate — an orb has no pickup delay of its own.
const TAKE_XP_DELAY_TICKS: i32 = 2;

/// One orb absorption, for the caller to announce.
#[derive(Debug, Clone, Copy)]
struct AbsorbedOrb {
    orb_entity_id: i32,
    /// Points paid out by this absorption — one orb's `value`, not the whole pile's.
    points: i32,
}

/// Absorbs at most one nearby experience orb into `experience` during the pickup
/// sweep.
///
/// # Why at most one
///
/// The **player's** pickup delay rejects every orb while non-zero and resets to `2`
/// on each absorption, so the sweep can take only one orb per two
/// ticks no matter how many are overlapping. Draining every overlapping orb in one pass
/// would bank the same total, which is exactly why it is worth stating: the difference is
/// invisible in the final number and obvious on screen, because the client plays one
/// pickup sound per `TAKE_ITEM_ENTITY` and animates one orb per absorption.
///
/// `delay` is the caller's own copy of the pickup delay, decremented here once per call —
/// this runs on the same movement-driven cadence the item pickup does.
///
/// Returns the absorption to announce, if one happened. The points are already in
/// `experience`; the caller owes the wire a `set_experience`.
fn collect_nearby_orbs(
    mobs: &MobHandle,
    player_feet: Vec3,
    experience: &mut crate::experience::PlayerExperience,
    delay: &mut i32,
) -> Option<AbsorbedOrb> {
    if *delay > 0 {
        *delay -= 1;
        return None;
    }
    mobs.with(|sim| {
        let (orb_entity_id, _) = sim.orbs_within_pickup_range(player_feet).into_iter().next()?;
        let points = sim.take_orb(orb_entity_id)?;
        *delay = TAKE_XP_DELAY_TICKS;
        experience.give_points(points);
        Some(AbsorbedOrb {
            orb_entity_id,
            points,
        })
    })
}

/// Vanilla's own yaw-to-direction conversion restricted to the
/// four horizontal directions, from a player yaw in degrees.
///
/// The 2d-data layout is `south=0, west=1, north=2, east=3`
/// (vanilla's own per-variant direction field table), so `floor(yaw / 90 + 0.5) & 3` maps yaw `0` →
/// south, `90` → west, `±180` → north, `-90` → east — the same "yaw 0 =
/// south, increasing clockwise" convention the shell's `camera_rig`/`hud`
/// use for the yaw this server receives from `move_player_rot`. Implemented
/// as a range match on the wrapped `[0, 360)` value rather than the bit-mask
/// formula, with the 45°/135°/225°/315° midpoints landing exactly as the
/// mask's `floor` does.
///
/// The returned direction is the one the player is **looking**, matching the
/// horizontal component of vanilla's own nearest-looking-direction getter —
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

/// Selects the state for the block a player just placed, or
/// `None` when no convention applies and the caller should keep the census's
/// bare default-state name.
///
/// The per-block table lives in [`crate::block_placement`]; this wrapper exists
/// only to keep the three redstone families ahead of it. They are not a
/// different convention — a repeater uses the opposite horizontal direction
/// like a furnace — but the redstone model reads `delay`/`locked`/`powered`
/// straight off the state *string*, so their placement must name the full
/// property set rather than leaving it to be defaulted downstream.
///
/// The observer is deliberately still yaw-only here; the observer model can
/// resolve horizontal facing but not a vertical facing.
/// `crate::redstone_observer` models horizontal observers only, so a
/// `facing=up` observer would be a state the signal model cannot read.
fn placed_block_state<F>(
    block: Block,
    ctx: &crate::block_placement::PlaceContext,
    block_at: F,
) -> Option<crate::block_placement::Placement>
where
    F: Fn(BlockPos) -> WorldState,
{
    if let Some(yaw) = ctx.yaw {
        let look = horizontal_look_direction(yaw);
        let full = match block {
            Block::Repeater => Some(set_repeater(look.opposite(), 1, false, false)),
            Block::Comparator => Some(set_comparator(look.opposite(), false, false, 0)),
            Block::Observer => Some(set_observer(look, false)),
            _ => None,
        };
        if let Some(state) = full {
            return Some(crate::block_placement::Placement {
                state,
                extra: Vec::new(),
            });
        }
    }
    crate::block_placement::placement(block.name(), ctx, block_at)
}

/// The two packets vanilla's own set-game-mode routine sends: the mode itself, then the
/// abilities it implies.
///
/// One helper because the pair must never be split — a client told it is in
/// creative without the abilities packet is in creative and cannot fly.
fn game_mode_directives<P: ServerProtocol>(proto: &P, mode: GameMode, abilities: &mut Abilities) -> [ServerDirective; 2] {
    abilities.set_game_mode(mode);
    [
        proto.encode_game_mode(mode),
        proto.encode_player_abilities(*abilities),
    ]
}

/// Whether a player standing with feet at `(px, py, pz)` overlaps the swept
/// region of a `moving_piston` cell travelling between `source` and `dest`
/// (the piston entity-push integration). The same box `crate::mobs::piston_shove::mob_aabb`
/// gives a mob (`0.6` wide, `1.8` tall — vanilla's own standing player
/// hitbox), against the same union-of-two-unit-cells region
/// `crate::mobs::piston_shove::swept_cell_aabb` builds for a mob; there is no
/// shared type between this crate's per-connection player state and its
/// `MobSim` world to call the mob version directly, so this is that same
/// arithmetic restated over plain floats rather than a second `Aabb` type
/// dependency.
fn player_overlaps_piston_sweep(px: f64, py: f64, pz: f64, source: BlockPos, dest: BlockPos) -> bool {
    const HALF_WIDTH: f64 = 0.3;
    const HEIGHT: f64 = 1.8;
    let min_x = f64::from(source.x.min(dest.x));
    let max_x = f64::from(source.x.max(dest.x)) + 1.0;
    let min_y = f64::from(source.y.min(dest.y));
    let max_y = f64::from(source.y.max(dest.y)) + 1.0;
    let min_z = f64::from(source.z.min(dest.z));
    let max_z = f64::from(source.z.max(dest.z)) + 1.0;
    (px - HALF_WIDTH) < max_x
        && (px + HALF_WIDTH) > min_x
        && py < max_y
        && (py + HEIGHT) > min_y
        && (pz - HALF_WIDTH) < max_z
        && (pz + HALF_WIDTH) > min_z
}

/// Applies one command [`Effect`](crate::Effect) to **this** connection.
///
/// The counterpart to `PlayerRegistry::push_effect`: an effect aimed at the
/// caller's own connection never goes through the registry at all, because
/// everything it needs — `game_mode`, `inventory`, `proto`, `conn` — is right
/// here and nothing else can reach it. The two paths are the reason
/// [`crate::Effect`] exists; see its module doc.
///
/// The `SetGameMode` arm also republishes to the registry, so another
/// connection's `@a[gamemode=creative]` reads the truth. Forgetting that
/// republish is silent: this connection behaves correctly and every *other*
/// connection's selector is wrong.
#[allow(clippy::too_many_arguments)]
async fn apply_own_effect<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    game_mode: &mut GameMode,
    abilities: &mut Abilities,
    inventory: &mut PlayerInventory,
    players: Option<&PlayerRegistry>,
    player_uuid: uuid::Uuid,
    effect: crate::commands::Effect,
    // `/give` is a `minecraft:inventory_changed` producer, so this arm
    // grants criteria exactly as a floor pickup does — see the `GiveItems` arm.
    advancements: &mut AdvancementManager,
    // For the world-clock timestamp the grant is stamped with, which must be
    // tick-derived rather than `Instant::now()` (this crate links into wasm32).
    world: &crate::world_state::WorldStateHandle,
    // This player's live status effects — the store `/effect give` and
    // `/effect clear` write through.
    effects: &mut crate::mob_effects::ActiveEffects,
    // `/kill`'s health write and the `publish_health` death sequence it
    // triggers.
    vitals: &mut PlayerVitals,
    // `/xp`'s read/write surface.
    experience: &mut crate::experience::PlayerExperience,
    // `publish_health`'s own parameters, for the `Kill` arm — see that
    // function's doc for why they are not derivable from anything else
    // already passed here.
    player_entity_id: i32,
    username: &str,
    // `/tp`'s `Teleport` arm. This connection's own tracked position/rotation
    // — read to preserve facing when the effect carries no `yaw`/`pitch`
    // (`Effect::Teleport`'s own doc explains why that resolution can only
    // happen here, at application time, never at the executor that produced
    // the effect), and written so this connection's own `player_pos`/
    // `player_rot` agree with the teleport it just sent — the same
    // `player_pos`/`player_rot` `dispatch_play_packet`'s movement arms keep in
    // sync, so a later relative move is computed from the post-teleport
    // position rather than a stale pre-teleport one.
    player_pos: &mut Option<(f64, f64, f64)>,
    player_rot: &mut Option<Rotation>,
    teleport_acknowledgements: &mut Option<TeleportAcknowledgements>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    match effect {
        crate::commands::Effect::SetGameMode(mode) => {
            *game_mode = mode;
            if let Some(registry) = players {
                registry.set_game_mode(player_uuid, mode);
            }
            for directive in game_mode_directives(proto, mode, abilities) {
                apply(conn, state, directive).await?;
            }
            // The tab-list entry's own game mode (`UPDATE_GAME_MODE`, action
            // ordinal 2). Without it the player's mode changes and every client's
            // tab list keeps reporting the mode they joined in — including their
            // own, which is what makes a spectator still show as survival there.
            for directive in proto.encode_player_info_game_mode(&[(player_uuid, mode)]) {
                apply(conn, state, directive).await?;
            }
        }
        crate::commands::Effect::GiveItems(stacks) => {
            for stack in stacks {
                // The second `minecraft:inventory_changed` producer, and the one a
                // player can reach deliberately: `/give @s crafting_table` must grant
                // `story/root` exactly as picking one off the floor does. Vanilla's
                // criterion is about *having* the item, not about how it arrived,
                // which is precisely why the trigger lives at the inventory seam.
                let given_id = stack.item.to_string();
                let (written, leftover) = inventory.add(stack);
                if leftover.is_none() || !written.is_empty() {
                    advancements.on_inventory_changed(
                        player_uuid,
                        &given_id,
                        world.time().game_time.saturating_mul(50),
                    );
                }
                for native in written {
                    // Window `0`, `state_id` `0` — matching every other
                    // server-initiated slot write in this file.
                    if let Some(menu_slot) = window_zero_menu_slot(native) {
                        apply(
                            conn,
                            state,
                            proto.encode_container_slot(0, 0, menu_slot, inventory.native(native)),
                        )
                        .await?;
                    }
                }
                if leftover.is_some() {
                    // Unfitted items would normally become an item entity. This
                    // crate has no command-spawned drop path, so the surplus is reported rather
                    // than silently discarded — the player is told, which is
                    // strictly better than an item vanishing.
                    apply(
                        conn,
                        state,
                        proto.encode_system_chat("Your inventory was full — some items were not given"),
                    )
                    .await?;
                }
            }
        }
        crate::commands::Effect::ApplyEffect {
            effect,
            duration,
            amplifier,
        } => {
            // Apply the complete stacking rule, including the hidden-effect
            // chain, so a second application of the same effect behaves
            // correctly rather than overwriting.
            //
            // Read whether the effect is present before calling `apply`; this
            // distinguishes a fresh instance from a refreshed one and supplies
            // the encoded packet's `blend` flag (see
            // `ServerProtocol::encode_update_mob_effect`'s own doc). Without this
            // arm, `/effect give` changed real server state — movement speed,
            // damage taken, hunger drain — with zero client feedback: no icon, no
            // particles, no screen tint.
            let already_present = effects.get(&effect).is_some();
            if effects.apply(&effect, duration, amplifier)
                && let Some(instance) = effects.get(&effect)
            {
                apply(
                    conn,
                    state,
                    proto.encode_update_mob_effect(
                        player_entity_id,
                        &effect,
                        instance.amplifier(),
                        instance.duration(),
                        false,
                        true,
                        true,
                        !already_present,
                    ),
                )
                .await?;
            }
        }
        crate::commands::Effect::ClearEffects { effect } => {
            // The counterpart to `ApplyEffect` above — the single-effect
            // removal and all-effects removal paths each
            // send `ClientboundRemoveMobEffectPacket` per cleared effect, so
            // `/effect clear` must tell the client which icons to drop rather
            // than leaving them stuck on screen.
            match effect {
                Some(id) => {
                    if effects.remove(&id) {
                        apply(conn, state, proto.encode_remove_mob_effect(player_entity_id, &id)).await?;
                    }
                }
                None => {
                    let cleared: Vec<String> =
                        effects.active().into_iter().map(|(id, _)| id.to_owned()).collect();
                    effects.clear();
                    for id in cleared {
                        apply(conn, state, proto.encode_remove_mob_effect(player_entity_id, &id)).await?;
                    }
                }
            }
        }
        crate::commands::Effect::Message(line) => {
            apply(conn, state, proto.encode_system_chat(&line)).await?;
        }
        crate::commands::Effect::Kill => {
            // Kill sets health directly to zero without armour or defenses.
            vitals.kill();
            publish_health(
                conn,
                state,
                proto,
                vitals,
                effects,
                // No sound fires for this call (`hurt` below is
                // `None`, and `publish_health` only plays one on a landed hit),
                // but a position is still owed to the parameter.
                player_pos.map(|(x, y, z)| Vec3::new(x, y, z)).unwrap_or_default(),
                player_entity_id,
                username,
                crate::vitals::DeathCause::GenericKill,
                advancements,
                player_uuid,
                None,
            )
            .await?;
        }
        crate::commands::Effect::GiveExperience { levels, amount } => {
            if levels {
                // `take_levels` is a level *subtraction*; negating the delta is
                // exactly `giveExperienceLevels`'s own addition.
                experience.take_levels(-amount);
            } else {
                experience.give_points(amount);
            }
            republish_experience(players, player_uuid, experience);
            apply(
                conn,
                state,
                proto.encode_set_experience(experience.progress(), experience.level(), experience.total()),
            )
            .await?;
        }
        crate::commands::Effect::SetExperience { levels, amount } => {
            // Zeroed first — see `crate::commands::experience`'s module doc for
            // why this is an approximation of vanilla's absolute setters rather
            // than a byte-exact port of them.
            *experience = crate::experience::PlayerExperience::default();
            if levels {
                experience.take_levels(-amount);
            } else {
                experience.give_points(amount);
            }
            republish_experience(players, player_uuid, experience);
            apply(
                conn,
                state,
                proto.encode_set_experience(experience.progress(), experience.level(), experience.total()),
            )
            .await?;
        }
        crate::commands::Effect::ClearInventory { item, max_count } => {
            let mut remaining = max_count.map(|n| u32::try_from(n).unwrap_or(0));
            let mut cleared: u32 = 0;
            for index in 0..crate::inventory::PLAYER_NATIVE_SIZE {
                if matches!(remaining, Some(0)) {
                    break;
                }
                let Some(stack) = inventory.native(index) else { continue };
                if let Some(filter) = &item {
                    if &stack.item.to_string() != filter {
                        continue;
                    }
                }
                let count = stack.count;
                let take = remaining.map_or(count, |cap| count.min(cap));
                if take == 0 {
                    continue;
                }
                if take >= count {
                    inventory.set_native(index, None);
                } else {
                    let mut left = stack.clone();
                    left.count -= take;
                    inventory.set_native(index, Some(left));
                }
                cleared += take;
                if let Some(cap) = remaining.as_mut() {
                    *cap -= take;
                }
                if let Some(menu_slot) = crate::inventory::window_zero_menu_slot(index) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, inventory.native(index)),
                    )
                    .await?;
                }
            }
            if cleared == 0 {
                apply(conn, state, proto.encode_system_chat("No items were found on the player")).await?;
            }
        }
        // World/broadcast/connection-local effects. Always self-targeted by the
        // executors that produce them (see `crate::commands::Effect`'s own doc)
        // and applied inline by the `ChatCommand` arm *before* it reaches this
        // function — that arm has `chunk_source`/`block_ticks`/the player
        // registry/`respawn`, none of which this function receives. A directed
        // effect of this kind reaching a *different* connection's drain would be
        // a registration bug in whichever executor produced it (every one of
        // them resolves `ctx.source.uuid()`, never a selector target); no-op
        // rather than panic, because a connection task must not go down for it.
        crate::commands::Effect::SetBlock { .. }
        | crate::commands::Effect::Fill { .. }
        | crate::commands::Effect::Broadcast { .. }
        | crate::commands::Effect::SetRespawnPoint { .. } => {}
        // `/tp`/`/teleport`. Unlike the world/broadcast effects above, this one
        // genuinely reaches any connected player, so it is an ordinary
        // per-uuid effect applied right here — for the caller inline, for a
        // directed target by that target's own connection loop. A missing
        // `yaw`/`pitch` means "keep this connection's current facing", which
        // is exactly `player_rot`'s own last-known value; a connection with no
        // facing on record yet (never sent one since join) falls back to
        // `0.0`/`0.0`, matching the join sequence's own default.
        crate::commands::Effect::Teleport { x, y, z, yaw, pitch } => {
            let current = player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 });
            let yaw = yaw.unwrap_or(current.yaw);
            let pitch = pitch.unwrap_or(current.pitch);
            *player_pos = Some((x, y, z));
            *player_rot = Some(Rotation { yaw, pitch });
            let teleport_id = issue_teleport_id(teleport_acknowledgements);
            apply(
                conn,
                state,
                proto.encode_teleport_with_id(teleport_id, x, y, z, yaw, pitch),
            )
            .await?;
        }
    }
    // A command can add, replace, restore, or clear Health Boost. Publish the
    // folded attribute in this same command turn, rather than waiting for the
    // periodic status tick and briefly leaving the client's heart capacity
    // stale.
    sync_effect_max_health(conn, state, proto, vitals, effects).await?;
    Ok(())
}

/// Checks whether the clicked slab can be replaced:
/// `true` when placing `held` onto `clicked` should turn it into a double slab
/// rather than start a new one in the next cell.
///
/// This predicate is asked about the clicked block itself, so the whole rule is
/// "same slab, not already double, and the click was on the side the existing
/// half does not already fill".
#[must_use]
fn slab_doubles(clicked: StateId, held: Block, face: BlockFace, cursor: Vec3f) -> bool {
    if crate::redstone::base_name(clicked) != held {
        return false;
    }
    let above_middle = cursor.y > 0.5;
    let horizontal = !matches!(face, BlockFace::Up | BlockFace::Down);
    match crate::redstone::get_str_property(clicked, PropertyKey::Type) {
        Some(BuiltinPropertyValue::Bottom) => matches!(face, BlockFace::Up) || (above_middle && horizontal),
        Some(BuiltinPropertyValue::Top) => matches!(face, BlockFace::Down) || (!above_middle && horizontal),
        _ => false,
    }
}

/// Which of a brewing stand's five slots a held item routes to — decided by
/// item identity alone: slots 0-2 take potions/bottles, slot 3 takes any
/// registered brewing ingredient, and slot 4 takes the brewing-fuel item tag.
enum BrewingSlot {
    /// Blaze powder — the brewing-fuel item tag's sole member (slot 4).
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
/// `None` there). The outcome distinguishes insertion, a consumed full-slot
/// click, and ordinary placement.
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
    /// through to ordinary placement when no brewing slot accepts it.
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
        "minecraft:water_bottle" => Bottle::from_potion_name(BottleKind::Potion, "minecraft:water"),
        _ => None,
    }
}

/// Routes `item` to the brewing-stand slot it belongs in, or `None` if it
/// belongs nowhere — mirroring vanilla's own brewing-stand-block-entity
/// can-place-item check. Blaze powder is checked first even though it is *also* a
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
                for raw in 0..lodestone_model::BrewingBottleSlot::COUNT {
                    let index = lodestone_model::BrewingBottleSlot::new(raw)
                        .expect("bounded bottle loop");
                    if stand.bottle_at(index).is_none() {
                        stand.set_bottle_at(index, Some(bottle));
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

    // Consume one item from the held stack.
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

/// The seed for the per-connection [`SpawnRng`] that draws bone meal's crop-age
/// and sapling-success values — its own stream rather than the composter's, for
/// the reason that constant's own comment gives about coupling two features
/// through one RNG. `crate::bone_meal`'s draw-count gates hold only against a
/// stream nothing else advances.
const BONE_MEAL_BEHAVIOR_SEED: u64 = 0x5EED_B04E;

/// The seed for the per-connection [`SpawnRng`] that draws
/// the base fire-block's `1..=3` player ramp. Its own stream serves the same
/// isolation purpose as the two constants above.
const BURN_BEHAVIOR_SEED: u64 = 0x5EED_F14E;

/// What a right-click on a composter did, so [`apply_use_item_on`] can decide
/// whether the ordinary placement logic may still run.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ComposterUseOutcome {
    /// `pos` held no composter block entity, or the click left both the
    /// composter and the player's hand untouched. This covers a
    /// non-compostable held item and an empty hand on a composter below the
    /// ready level. The block's item-use result is `PASS`, so the placement
    /// logic below must run.
    NotComposter,
    /// The composter consumed the click but nothing moved — level `7`,
    /// waiting on its scheduled tick; the hand is untouched. No
    /// placement may follow.
    Noop,
    /// One item was consumed from the player's hand. The updated hand contents are sent through
    /// the caller to push as a window-0 slot update; `block_state` is the new
    /// block state to write — `Some` when the fill level advanced, `None` on a
    /// failed roll (the item is still consumed; only the state is unchanged).
    Consumed {
        remainder: Option<ItemStack>,
        block_state: Option<StateId>,
    },
    /// Bone meal was extracted (level `8` -> `0`, vanilla's own `extractProduce`)
    /// — the caller spawns the item entity and
    /// writes `block_state`.
    Extracted { block_state: StateId },
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
    Consumed { block_state: Option<StateId> },
    /// Level 8 reached — extract bone meal, reset to 0.
    Extract,
}

/// The block-state string for a composter at fill `level` — vanilla's
/// `minecraft:composter[level=0..8]`, the string the client's
/// `resolve_state_id` maps to the composter's per-level ids (block 229 in
/// `crates/lodestone-data/src/generated/block_states.rs`).
fn composter_state(level: u8) -> StateId {
    let value = match level {
        0 => BuiltinPropertyValue::Value0,
        1 => BuiltinPropertyValue::Value1,
        2 => BuiltinPropertyValue::Value2,
        3 => BuiltinPropertyValue::Value3,
        4 => BuiltinPropertyValue::Value4,
        5 => BuiltinPropertyValue::Value5,
        6 => BuiltinPropertyValue::Value6,
        7 => BuiltinPropertyValue::Value7,
        8 => BuiltinPropertyValue::Value8,
        _ => unreachable!("composter level is bounded to 0..=8"),
    };
    let properties = Properties::try_from_pairs(&[(
        PropertyKey::Level,
        PropertyValue::builtin(value),
    )])
    .expect("generated composter properties");
    Properties::state_for_block(Block::Composter, &properties).expect("generated composter state")
}

/// Applies one right-click on the composter at `pos` — the wiring that makes
/// `Composter::insert`/`extract`, keeping the seven-tier fill state machine
/// reachable from a player.
///
/// Mirrors the composter interaction order: the held item (if any) is
/// offered to the fill machine first, and an unconsumed click enters the
/// hand-use branch, which extracts at level `8` and otherwise returns `PASS`.
/// Concretely:
///
/// * an empty hand on a ready (level `8`) composter extracts the bone meal;
/// * a compostable item is rolled against its chance, consuming one from the
///   hand either way (a failed roll consumes the item);
/// * a compostable item at level `7` (waiting on its scheduled tick) is
///   consumed as a click but changes nothing;
/// * a *non*-compostable item never reaches `insert`'s level gate, because at
///   level `7` the compostable-item check fails before the fill-level add, so
///   the click falls through to placement while
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
            // Empty hand: run the composter's hand-use branch.
            if composter.extract() {
                return ComposterStep::Extract;
            }
            return ComposterStep::FallThrough;
        };
        let item = held.item.to_string();
        // Non-compostable items enter the hand-use branch (see the doc comment
        // above for why this must be checked before `insert`, not by it).
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
                // Level 7 (waiting, compostable): the interaction returns
                // SUCCESS with the hand untouched. Level 8 (ready): the item
                // offer failed below level 8, so the hand-use half extracts
                // instead.
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
            // Consume one item from the selected hotbar stack, the same shrink
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
            // Extract exactly one bone meal at the block's top, with the hand
            // untouched. The standard horizontal jitter is skipped because
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
/// vanilla's own use-item-on handler's replace-vs-relative
/// choice of placement cell (`BlockPlaceContext`'s constructor: place at the
/// clicked block if it `canBeReplaced`, otherwise at its `face`-neighbour) —
/// simplified per this crate's documented scope (`docs/block-edit.md`): no
/// survival/collision validation beyond "is the target cell currently
/// replaceable" (air or a fluid — see [`is_air_or_fluid`], plus
/// [`slab_doubles`] for the one `canBeReplaced` override a hand placement can
/// hit). Per-block orientation now goes through [`crate::block_placement`],
/// which carries each family's own `getStateForPlacement` convention.
///
/// **Placement honours the held item for every block in the game.**
/// `inventory`'s currently selected item is resolved through
/// [`lodestone_data::block_items::block_placed_by`] — the 26.2 census of
/// vanilla's own block-item block getter, dumped from the real jar — which decides both
/// whether a placement happens and which block it writes.
///
/// The block-item census gates placement and names the block. The
/// [`block_entity_for_item`] lookup then inserts the live
/// [`crate::block_entities::BlockEntity`] for the six ticking block types;
/// ordinary blocks use the census result without that extra record.
///
/// **A non-placeable item places nothing.** A sword, a bucket, a spawn egg or
/// an empty hand leaves the world untouched. The `block_update` for both cells
/// is sent below, so a client
/// that predicted a placement is corrected rather than left desynchronised.
///
/// **Block *state* comes from the block's own convention.** The clicked face,
/// the cursor hit within it and the placing player's yaw/pitch all reach
/// [`crate::block_placement`], so a stair faces the way the player does and is
/// upper or lower depending on where in the face they clicked, a chest and a
/// furnace face the *other* way, an anvil is turned a quarter further, and a
/// torch clicked against a wall becomes a `wall_torch`. Two-cell placements (a
/// door's upper half, a bed's head, a paired chest's partner) travel out as
/// `Placement::extra` and are written and notified with the primary cell.
///
/// Sends [`ServerProtocol::encode_block_update`] for **both** `pos` and its
/// `face`-neighbour unconditionally, matching vanilla's own
/// use-item-on handler, which
/// sends both regardless of whether the placement succeeded — this doubles
/// as the correction for a client that predicted a placement the server
/// rejected.
///
/// **Right-clicking a block that already has an *openable* container opens
/// its screen instead of attempting a placement at all** — the closing half
/// of the block-entity interaction section in `docs/block-entities.md`. The
/// interaction order is:
/// clicked-block hand use (which is what opens a furnace/hopper's menu)
/// **before** any placement logic, and a block
/// that opens a menu never falls through to placement.
///
/// **A brewing stand at `pos` is this "clicked block's own use" step too,
/// but without a menu**: it cannot be opened — `menu_name` answers `None`,
/// because its bottle slots are not real `ItemStack`s — so
/// [`insert_into_brewing_stand`] stands in for the menu with a direct
/// one-item-per-click insert, the same interaction shape used for
/// the composter (which also has no menu). A held item that
/// belongs in a brewing stand is routed into the matching slot and consumed;
/// an unrelated held item still falls through to the placement logic below
/// and leaves unrelated held items to the placement logic.
///
/// Whether writing `state` at `target` would intersect the placer's own
/// bounding box, narrowed to the one entity this server can currently name
/// at a placement site: the placer, from `player_pos`. A full
/// The complete check would test every entity's bounding box in the cell and
/// exclude spectators; this crate has no per-connection entity-bounding-box
/// registry to query the rest of, so another player or a mob standing in the
/// target cell is not yet refused — see `docs/block-edit.md`.
///
/// A state with an **empty** collision shape (a torch, a rail, a pressure
/// plate, redstone dust…) is never obstructed — placing one at your own feet is
/// legal here.
///
/// The placer's box uses player dimensions (`0.6 x 1.8`, centred
/// horizontally on `feet`, `feet.y..feet.y + 1.8` vertically) —
/// the unobstructed check reads the entity's own bounding-box getter at click time, which does
/// not shrink for the sneaking pose (`1.5`), so this does not model pose
/// either.
fn placement_obstructs_placer(target: BlockPos, state: StateId, feet: Vec3) -> bool {
    let boxes = lodestone_data::collision_shapes::collision_boxes(state);
    let (px0, px1) = (feet.x - 0.3, feet.x + 0.3);
    let (py0, py1) = (feet.y, feet.y + 1.8);
    let (pz0, pz1) = (feet.z - 0.3, feet.z + 0.3);
    boxes.iter().any(|b| {
        let bx0 = f64::from(target.x) + f64::from(b.min[0]);
        let bx1 = f64::from(target.x) + f64::from(b.max[0]);
        let by0 = f64::from(target.y) + f64::from(b.min[1]);
        let by1 = f64::from(target.y) + f64::from(b.max[1]);
        let bz0 = f64::from(target.z) + f64::from(b.min[2]);
        let bz1 = f64::from(target.z) + f64::from(b.max[2]);
        // Strict inequalities: two boxes that only share a face are touching,
        // not intersecting — the same convention
        // `lodestone_shell::sim::placement::block_intersects_player` uses for
        // the client's own (coarser, full-cell) prediction of this same rule.
        bx1 > px0 && bx0 < px1 && by1 > py0 && by0 < py1 && bz1 > pz0 && bz0 < pz1
    })
}

/// Resolves the selected stack's built-in item once for a placement attempt.
///
/// Custom registry entries have no built-in [`Item`] value, so they cannot
/// enter the built-in placement census.
fn selected_placement_item(inventory: &PlayerInventory, native_slot: usize) -> Option<Item> {
    let item = &inventory.native(native_slot)?.item;
    (item.namespace() == "minecraft")
        .then(|| Item::from_name(item.path()))
        .flatten()
}

#[allow(clippy::too_many_arguments)]
async fn apply_use_item_on<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    pending_relights: Option<&mut PendingRelights>,
    pos: BlockPos,
    face: BlockFace,
    // The block-local hit position within `pos`. `crate::block_placement` reads
    // its `y` for every `half`/`type`-bearing block (a stair, slab or trapdoor
    // clicked high on a side face is an upper one) and its `x`/`z` for a door's
    // hinge tie-break.
    cursor: Vec3f,
    // The player's world-space position, for the bed-respawn
    // reach test (bed ±3 x/z and ±2 y). `None` until
    // the first `PlayerMoved` packet arrives; a bed click before any move
    // skips the reach test rather than rejecting (see
    // [`is_legal_bed_respawn`]'s doc comment).
    player_pos: Option<Vec3>,
    // The player's per-player respawn point, written when a legal
    // bed is right-clicked (see the bed arm below). `&mut`: the set writes
    // through this slot.
    respawn: &mut Option<RespawnPoint>,
    // The placing player's yaw, so the directional families can
    // derive their `facing` (see [`placed_block_state`]). `None` until the
    // first packet carrying angles arrives; placement then falls back to the
    // block's default state.
    player_yaw: Option<f32>,
    // Pitch, for the direction-sensitive families alone (a dispenser
    // or piston placed while looking down points up). `None` on the same
    // terms as `player_yaw`.
    player_pitch: Option<f32>,
    // Whether this click used the secondary-use (sneak) input. A block item
    // held while sneaking bypasses the clicked container's own use.
    sneaking: bool,
    // The placing player, for the place sound's `except` argument (see the
    // `block_placed` call below).
    placer: uuid::Uuid,
    // `&mut`, not `&`: a brewing-stand insertion consumes one item from the
    // player's selected hotbar stack, and only a mutable
    // inventory can write the remainder back.
    inventory: &mut PlayerInventory,
    block_entities: &BlockEntityHandle,
    next_window_id: &mut i32,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    // The composter interaction: `mobs` so a level-8 extraction
    // can spawn its bone-meal item entity, and `roll` — a fresh `[0.0, 1.0)`
    // draw from the connection's [`SpawnRng`], one per right-click, so the
    // fill machine's per-item chance sees a live sample rather than a constant
    // (the caller-supplied-roll shape `Composter::insert` documents).
    mobs: &MobHandle,
    roll: f64,
    // The delayed half: `propagate_placement` below resolves
    // everything synchronous (dust) against a `ScheduledTickQueue` it then
    // discards; a torch/repeater/comparator/observer instead *schedules*, and
    // only `tick::run_tick_loop` owns a queue those can land in. This asks the
    // loop to redo the fan-out on its next iteration, where the schedule
    // survives. See `BlockTickFeed`'s own doc comment.
    block_ticks: &BlockTickFeed,
    // The night-skip vote, written on a bed click (the bed arm
    // above — `lay_down`), and the key it stores this connection's player
    // under — see `dispatch_play_packet`'s parameter comment.
    sleep_vote: &SleepVote,
    player_entity_id: i32,
    // This connection's bone-meal roll source. A whole `SpawnRng` rather than a
    // pre-drawn value like the composter's `roll` above, because
    // `crate::bone_meal::apply_bone_meal` draws a *variable* number of values —
    // one for a crop, one for a sapling, none for a non-target — and the draw
    // count per use is part of the specification its own tests pin. Pre-drawing
    // would fix the count at one and desynchronise the stream.
    bone_meal_rng: &mut SpawnRng,
    // The world difficulty controls which spawn-egg species are permitted on
    // Peaceful. Passed by value because this function needs only the scalar;
    // taking the whole `WorldStateHandle` would add an unrelated read.
    difficulty: lodestone_model::Difficulty,
    // The acting player's game mode controls item consumption: creative
    // placement writes the block without consuming the held item. See the
    // consumption arm at the end of the placement branch.
    game_mode: GameMode,
    // A fresh `[0, i32::MAX)` draw from `dispatch_play_packet`'s `drops_rng`,
    // the same pre-drawn-value shape the composter `roll` above already
    // uses. Only consumed if this click opens an enchanting table (see
    // `open_enchanting_screen`'s own parameter comment); drawn unconditionally
    // by the caller anyway, matching the composter roll's own "one draw per
    // right-click, whatever block was hit" reasoning.
    enchant_seed_roll: i64,
    // `ServerBound::UseItemOn::hand` (`0` main, `1` off) selects the held
    // slot that `held_item` below reads from. Both hands therefore use the same
    // spawn-egg, flint-and-steel, and block-placement paths.
    hand: u8,
    // Only the narrow crafting-station hook registry, not the
    // whole `WorldStateHandle` — see `difficulty`'s own comment above for why
    // this function takes the scalar/handle it actually needs rather than a
    // handle that would invite a second, unrelated read.
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    // A chest placed by terrain generation (a shipwreck, igloo, or ocean ruin)
    // lives in the column, not the live registry. Hydrate it on the first click
    // so the generated loot opens correctly. Check the block kind first so an
    // ordinary right-click does not pay for the lookup.
    let container_here = block_entities.with(|reg| reg.get(pos).is_some());
    if !container_here {
        let clicked = source.block_state_id(pos.x, pos.y, pos.z);
        let block = clicked.block();
        if let Some(name) = crate::block_entities::container_type_for_block(block) {
            let generated = source
                .block_entity(pos.x, pos.y, pos.z)
                .unwrap_or_else(|| BlockEntity::container(name));
            block_entities.with(|reg| reg.insert(pos, generated));
        }
    }

    // A beacon's pyramid tier is recomputed fresh from the world on every
    // open — see `BeaconData::levels`'s own doc for why nothing refreshes it
    // in the background instead.
    block_entities.with(|reg| {
        if let Some(BlockEntity::Beacon(beacon)) = reg.get_mut(pos) {
            beacon.levels = crate::beacon::beacon_levels(source, pos.x, pos.y, pos.z);
        }
    });

    // Secondary use is what lets a block item pass through a clicked chest (or
    // other block entity with a menu) and place into the adjacent cell. Keep
    // the ordinary empty-hand sneak click as a menu open: only a real block
    // item has the alternate placement meaning.
    let hand_native_for_use = if hand == 1 {
        crate::inventory::OFFHAND_NATIVE
    } else {
        usize::from(inventory.selected_hotbar_slot())
    };
    let holding_block_item = selected_placement_item(inventory, hand_native_for_use)
        .is_some_and(|item| block_items::block_placed_by(item).is_some());
    let existing_menu = block_entities.with(|reg| reg.get(pos).and_then(BlockEntity::menu_name));
    if let Some(menu) = existing_menu.filter(|_| !sneaking || !holding_block_item) {
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

    // A crafting table opens a *virtual* menu. It is not a block
    // entity, so the `existing_menu` branch above structurally cannot reach it —
    // see `open_crafting_table_screen`. Ahead of the placement branch for the same
    // reason the `hand_use` block is: right-clicking a table while holding a block
    // opens the table rather than building.
    if source.block_state_id(pos.x, pos.y, pos.z).block() == Block::CraftingTable {
        return open_crafting_table_screen(
            conn,
            proto,
            state,
            inventory,
            pos,
            next_window_id,
            open_container,
            container_sync,
        )
        .await;
    }

    // Workstation menus use per-menu input slots rather than block-entity
    // storage. The `existing_menu` branch therefore cannot find these stations;
    // dispatch them through their virtual menu implementations below.
    let clicked_block = source.block_state_id(pos.x, pos.y, pos.z).block();
    if let Some(station) = match clicked_block {
        Block::Anvil | Block::ChippedAnvil | Block::DamagedAnvil => Some(Station::Anvil),
        Block::Grindstone => Some(Station::Grindstone),
        Block::SmithingTable => Some(Station::Smithing),
        Block::Loom => Some(Station::Loom),
        Block::Stonecutter => Some(Station::Stonecutter),
        _ => None,
    } {
        return open_workstation_screen(
            conn,
            proto,
            state,
            inventory,
            pos,
            station,
            next_window_id,
            open_container,
            container_sync,
            hooks,
        )
        .await;
    }
    if clicked_block == Block::EnchantingTable {
        return open_enchanting_screen(
            conn,
            proto,
            source,
            state,
            inventory,
            pos,
            next_window_id,
            open_container,
            container_sync,
            enchant_seed_roll,
        )
        .await;
    }

    // A brewing-stand right-click routes the held item into the matching slot
    // (fuel, bottle, or ingredient) and consumes one from the player's hand. See
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

    // A composter right-click feeds the seven-tier fill state machine. See
    // [`apply_composter_use`]'s doc comment for the four outcomes. A handled
    // click returns before placement; only `NotComposter` reaches that branch.
    match apply_composter_use(block_entities, inventory, mobs, pos, roll) {
        ComposterUseOutcome::Consumed {
            remainder,
            block_state,
        } => {
            // Write the new fill level — only when it actually advanced; a
            // failed roll consumed the item but left the state alone.
            if let Some(block_state) = block_state {
                source.set_block(pos.x, pos.y, pos.z, block_state);
                apply(conn, state, proto.encode_block_update(pos.x, pos.y, pos.z, block_state)).await?;
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
            source.set_block(pos.x, pos.y, pos.z, block_state);
            apply(conn, state, proto.encode_block_update(pos.x, pos.y, pos.z, block_state)).await?;
            return Ok(());
        }
        ComposterUseOutcome::Noop => return Ok(()),
        ComposterUseOutcome::NotComposter => {
            // Fall through to the ordinary placement logic below.
        }
    }

    // Bone meal on a growable block — `BoneMealItem::useOn`, the consuming half
    // of [`crate::bone_meal`]'s rule layer. Ahead of the placement branch for
    // the same reason the composter and brewing arms are: bone meal is not a
    // block item, but the *clicked* cell is often air-adjacent and a fall-through
    // would try to place whatever else is in hand.
    //
    // The three outcomes are not two: `ConsumedNoChange` is a real vanilla
    // result, because `BoneMealItem` shrinks the stack *outside* the success
    // branch — a failed sapling roll (55% of them) eats the item for nothing,
    // and treating that as a no-op would make bone meal infinitely efficient.
    // `NotModelled` deliberately consumes nothing: the grass-block and
    // stage-1-sapling paths need a worldgen feature placer this crate does not
    // have, and a partial version would consume a *different* number of RNG
    // draws and desynchronise every later use in the same stream.
    if inventory
        .selected_item()
        .is_some_and(|held| held.item.to_string() == crate::bone_meal::BONE_MEAL)
    {
        let clicked = source.block_state_id(pos.x, pos.y, pos.z);
        // The cell above supplies the light input for growth checks; it is
        // resolved here because `bone_meal` has no world access of its own.
        let above = source.block_state_id(pos.x, pos.y + 1, pos.z);
        let outcome = crate::bone_meal::apply_bone_meal(clicked, above, bone_meal_rng);
        // One helper for both consuming arms, performing the same one-item
        // shrink as the composter's `Consumed` arm.
        let consume = |inventory: &mut PlayerInventory| {
            let native = usize::from(inventory.selected_hotbar_slot());
            let remainder = inventory.native(native).cloned().and_then(|mut stack| {
                stack.count -= 1;
                (stack.count > 0).then_some(stack)
            });
            inventory.set_native(native, remainder.clone());
            remainder
        };
        match outcome {
            crate::bone_meal::BoneMealOutcome::Grew { state: new_state } => {
                source.set_block(pos.x, pos.y, pos.z, new_state);
                apply(
                    conn,
                    state,
                    proto.encode_block_update(pos.x, pos.y, pos.z, new_state),
                )
                .await?;
                let remainder = consume(inventory);
                let hotbar_slot =
                    i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
                apply(
                    conn,
                    state,
                    proto.encode_container_slot(0, 0, hotbar_slot, remainder.as_ref()),
                )
                .await?;
                return Ok(());
            }
            crate::bone_meal::BoneMealOutcome::ConsumedNoChange => {
                let remainder = consume(inventory);
                let hotbar_slot =
                    i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
                apply(
                    conn,
                    state,
                    proto.encode_container_slot(0, 0, hotbar_slot, remainder.as_ref()),
                )
                .await?;
                return Ok(());
            }
            // Not a target, or a family whose growth this crate cannot model:
            // fall through to the ordinary placement logic below, consuming
            // nothing.
            crate::bone_meal::BoneMealOutcome::NotBonemealable
            | crate::bone_meal::BoneMealOutcome::NotModelled { .. } => {}
        }
    }

    // Right-clicking a bed records a per-player respawn point and registers
    // the player for the sleep vote. A bed click is an interaction, not a
    // placement, so it returns before the inventory-placement logic. The
    // legality gate applies the three checks in [`is_legal_bed_respawn`].
    // Notify the client only when the stored point changes; a repeat click on
    // the same bed is silent. This crate has no localization table or action-bar
    // encoder, so the notification uses a plain system-chat line.
    if is_bed_block(source.block_state_id(pos.x, pos.y, pos.z)) {
        // Register the player in the night-skip vote. Bed-entry gates for
        // day/night, nearby monsters, and already-sleeping state are outside
        // this interaction; the 100-tick deep-sleep threshold prevents a
        // single daytime click from advancing the vote. Registration is
        // idempotent, so a repeat click does not double-count.
        sleep_vote.lay_down(player_entity_id);
        let bed_dimension = source.dimension().unwrap_or(crate::dimension::Dimension::Overworld);
        if crate::respawn::bed_works_in(bed_dimension)
            && is_legal_bed_respawn(source, pos, player_pos)
            && !respawn.is_some_and(|existing| existing.same_block(pos, bed_dimension))
        {
            *respawn = Some(RespawnPoint::block(pos, bed_dimension, player_yaw.unwrap_or(0.0)));
            apply(conn, state, proto.encode_system_chat("Respawn point set")).await?;
        }
        return Ok(());
    }

    // Right-clicking a respawn anchor: glowstone charges it, a charged anchor
    // sets the respawn point in the Nether and blasts anywhere else. See
    // [`crate::respawn_anchor`] for the decision table. A click that decides
    // `FallThrough` carries on to the ordinary placement logic.
    if crate::respawn_anchor::is_anchor(source.block_state_id(pos.x, pos.y, pos.z)) {
        let clicked = source.block_state_id(pos.x, pos.y, pos.z);
        let dimension = source.dimension().unwrap_or(crate::dimension::Dimension::Overworld);
        let main_native = usize::from(inventory.selected_hotbar_slot());
        let is_glowstone = |native: usize| {
            selected_placement_item(inventory, native) == Some(Item::Glowstone)
        };
        let holds_anything = |native: usize| inventory.native(native).is_some();
        let hand_native = if hand == 1 { OFFHAND_NATIVE } else { main_native };
        let outcome = crate::respawn_anchor::decide_use(crate::respawn_anchor::UseContext {
            charges: crate::respawn_anchor::charges(clicked),
            works_here: crate::respawn_anchor::works_in(dimension),
            clicking_hand_glowstone: is_glowstone(hand_native),
            main_hand: hand != 1,
            off_hand_glowstone: is_glowstone(OFFHAND_NATIVE),
            sneaking_with_item: sneaking
                && (holds_anything(main_native) || holds_anything(OFFHAND_NATIVE)),
            already_this_point: respawn.is_some_and(|existing| existing.same_block(pos, dimension)),
        });
        use crate::respawn_anchor::AnchorUse;
        match outcome {
            AnchorUse::FallThrough => {}
            AnchorUse::Defer | AnchorUse::AlreadySet => return Ok(()),
            AnchorUse::Charge => {
                let new_state = crate::respawn_anchor::with_charges(
                    crate::respawn_anchor::charges(clicked) + 1,
                );
                source.set_block(pos.x, pos.y, pos.z, new_state);
                apply(conn, state, proto.encode_block_update(pos.x, pos.y, pos.z, new_state)).await?;
                block_ticks.publish_change(pos.x, pos.y, pos.z, clicked, new_state);
                // A comparator beside the anchor follows its charge.
                let (changed, scheduled) = propagate_placement_with_entities(source, pos, Some(block_entities));
                block_ticks.request_scheduled_ticks(scheduled);
                for (at, changed_state) in changed {
                    apply(conn, state, proto.encode_block_update(at.x, at.y, at.z, changed_state)).await?;
                }
                block_ticks.publish_effect(crate::respawn_anchor::block_sound("charge", pos));
                if consume_one(inventory, hand_native, game_mode)
                    && let Some(menu_slot) = crate::inventory::window_zero_menu_slot(hand_native)
                {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, inventory.native(hand_native)),
                    )
                    .await?;
                }
                return Ok(());
            }
            AnchorUse::SetSpawn => {
                *respawn = Some(RespawnPoint::block(pos, dimension, 0.0));
                apply(conn, state, proto.encode_system_chat(crate::respawn_anchor::RESPAWN_SET_MESSAGE)).await?;
                block_ticks.publish_effect(crate::respawn_anchor::block_sound("set_spawn", pos));
                return Ok(());
            }
            AnchorUse::Explode => {
                // The block goes first, so the blast does not shield itself.
                let air = crate::chunk::air_state();
                source.set_block(pos.x, pos.y, pos.z, air);
                apply(conn, state, proto.encode_block_update(pos.x, pos.y, pos.z, air)).await?;
                block_ticks.publish_change(pos.x, pos.y, pos.z, clicked, air);
                let centre = Vec3::new(
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y) + 0.5,
                    f64::from(pos.z) + 0.5,
                );
                let smothered = crate::respawn_anchor::smothered_by_water(source, pos);
                mobs.with(|sim| sim.queue_blast(centre, crate::respawn_anchor::BLAST_POWER, !smothered, !smothered));
                return Ok(());
            }
        }
    }

    // The hand-use branch runs **ahead of the placement branch**: a door or
    // other usable block must handle a right-click before block placement;
    // otherwise the block would build instead of opening it. See `crate::hand_use` for the five
    // families and the rules each comes from.
    //
    // Returns early like the bed arm: a click that operated a block is not also a
    // placement.
    {
        let clicked = source.block_state_id(pos.x, pos.y, pos.z);
        if crate::hand_use::is_hand_usable(clicked) {
            // The door's partner half, read here because `hand_use` has no world
            // access. `None` for every other family, and for a door whose partner
            // is missing (a half-broken door, which vanilla also tolerates).
            let other_half = crate::redstone_openable::other_door_half_pos(pos, clicked)
                .map(|p| (p, source.block_state_id(p.x, p.y, p.z)));
            if let Some(used) = crate::hand_use::hand_use(pos, clicked, other_half, player_yaw) {
                let mut fanout: Vec<BlockPos> = Vec::new();
                for (p, new_state) in &used.changes {
                    source.set_block(p.x, p.y, p.z, *new_state);
                    fanout.push(*p);
                }
                // The placement fan-out notifies neighbouring blocks, so a lever
                // powers the wire beside it rather than merely looking flipped.
                // Without this notification, the redstone model stays correct but
                // is unreachable from a player's hand.
                let mut changed: Vec<(BlockPos, lodestone_data::block_states::StateId)> = Vec::new();
                let mut piston_records: Vec<(BlockPos, lodestone_core::Nbt)> = Vec::new();
                for p in &fanout {
                    let (mut more, scheduled) = propagate_placement_with_entities(source, *p, Some(block_entities));
                    piston_records.extend(moving_piston_records(&scheduled));
                    block_ticks.request_scheduled_ticks(scheduled);
                    changed.append(&mut more);
                }
                // A pressed button releases itself. Scheduled through the same
                // relative-delay feed a placement's delayed families use, so
                // `run_tick_loop` rebases it onto its own counter.
                if let Some(delay) = used.release_after {
                    // Built through a throwaway queue rather than a struct literal
                    // because `ScheduledTick`'s `sub_tick_order` is private — the
                    // same idiom `propagate_placement` uses to produce its own
                    // relative-delay batch, and for the same reason.
                    let mut pending: ScheduledTickQueue<ScheduledTickKind> = ScheduledTickQueue::new();
                    pending.schedule(
                        (pos.x, pos.y, pos.z),
                        crate::hand_use::TICK_BUTTON,
                        delay,
                        crate::scheduled_tick::TickPriority::Normal,
                    );
                    block_ticks.request_scheduled_ticks(pending.drain_due(u64::MAX, usize::MAX));
                }
                // Every cell the click rewrote, then every cell the fan-out did.
                let mut notify: Vec<BlockPos> = fanout;
                for (p, _) in &changed {
                    if !notify.contains(p) {
                        notify.push(*p);
                    }
                }
                for p in notify {
                    let current = source.block_state_id(p.x, p.y, p.z);
                    apply(conn, state, proto.encode_block_update(p.x, p.y, p.z, current)).await?;
                    if let Some((_, nbt)) = piston_records.iter().find(|(pos, _)| *pos == p) {
                        let directive = proto.encode_block_entity_data(
                            p,
                            crate::piston::PISTON_BLOCK_ENTITY,
                            nbt,
                        );
                        apply(conn, state, directive).await?;
                    }
                }
                return Ok(());
            }
            // `hand_use` said no (an iron door, or an already-pressed button).
            // Vanilla returns PASS/CONSUME, and in neither case does it fall
            // through to placement against the clicked cell — an iron door is not
            // replaceable, so the placement branch would do nothing anyway, but
            // returning here says why.
            return Ok(());
        }
    }

    let neighbour = relative(pos, face);
    let clicked = source.block_state_id(pos.x, pos.y, pos.z);
    // Which native slot this click reads from. The spawn-egg, flint-and-steel,
    // and block-placement branches below share this one
    // resolution point via `held_item`, so an item held only in the off hand
    // now reaches them instead of the main hand's slot always winning.
    let hand_native = if hand == 1 {
        crate::inventory::OFFHAND_NATIVE
    } else {
        usize::from(inventory.selected_hotbar_slot())
    };
    let held_item = selected_placement_item(inventory, hand_native);

    // Spawn-egg handling runs between clicked-block hand use and generic block
    // placement: an egg held over air must not place a block, while a lever
    // click must not consume the egg. See
    // `crate::spawn_egg` for the placement rule and `docs/spawn-eggs.md` for why
    // the item-to-entity mapping is a checked derivation rather than a table.
    //
    // A block entity at the clicked position is consulted first. Spawners are
    // not simulated here, so the guard is "there is a spawner here, do
    // nothing"; it prevents the egg from creating an unsupported mob.
    if let Some(item) = held_item {
        let spawner_here = block_entities.with(|reg| {
            reg.get(pos)
                .is_some_and(|entity| entity.kind() == BlockEntityKind::MobSpawner)
        });
        if !spawner_here {
            match crate::spawn_egg::apply_spawn_egg(
                item.name(),
                difficulty,
                pos,
                face,
                &|x, y, z| source.block_state_id(x, y, z),
                mobs,
            ) {
                // Not an egg: fall through to the placement branch below.
                crate::spawn_egg::SpawnEggApplied::NotSpawnEgg => {}
                // Vanilla `FAIL`: no entity, no placement, and the stack is
                // untouched. Returning here rather than falling through is the
                // load-bearing half — a refused egg must not place a block.
                crate::spawn_egg::SpawnEggApplied::Refused => return Ok(()),
                crate::spawn_egg::SpawnEggApplied::Spawned { .. } => {
                    // Consume one item *after* the spawn succeeds — the same
                    // shrink-and-report pair the composter and brewing
                    // arms above perform, including the window-0 hotbar slot
                    // update so the held count visibly drops.
                    //
                    // Routed through `consume_one` so creative players keep their
                    // eggs while survival players lose one.
                    let native = hand_native;
                    if consume_one(inventory, native, game_mode) && game_mode != GameMode::Creative {
                        let remainder = inventory.native(native).cloned();
                        if let Some(menu_slot) = window_zero_menu_slot(native) {
                            apply(
                                conn,
                                state,
                                proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                            )
                            .await?;
                        }
                    }
                    return Ok(());
                }
            }
        }
    }

    // Minecart-item handling is a rail-targeted placement, checked ahead of the
    // generic block-placement branch: a minecart item is not a block, so that
    // branch cannot place one. A non-rail target is refused rather than falling
    // through to anything else.
    if let Some(item) = held_item {
        if let Some(kind) = crate::mobs::minecart::MinecartKind::from_item(item.name()) {
            let clicked = source.block_state_id(pos.x, pos.y, pos.z);
            if crate::mobs::minecart::is_rail_block(clicked) {
                let shape = crate::mobs::minecart::rail_shape(clicked);
                let position = crate::mobs::minecart::placement_position(pos, shape);
                mobs.with(|sim| {
                    sim.spawn_minecart(kind, position);
                });
                let native = hand_native;
                if consume_one(inventory, native, game_mode) && game_mode != GameMode::Creative {
                    let remainder = inventory.native(native).cloned();
                    if let Some(menu_slot) = window_zero_menu_slot(native) {
                        apply(
                            conn,
                            state,
                            proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                        )
                        .await?;
                    }
                }
            }
            return Ok(());
        }
    }

    // Lighting a nether portal. **Ahead of the placement branch**, for the same
    // reason the `hand_use` block above is: `flint_and_steel` is not a block item,
    // so the placement branch below cannot reach it at all.
    //
    // The flint-and-steel route places a fire cell, then runs the frame search **from the
    // cell the fire went in**, not from the block that was clicked — so the search
    // origin is `relative(pos, face)`. Clicking the top face of a frame's bottom
    // obsidian therefore searches from the lowest interior cell, which is what makes
    // the ordinary way of lighting a portal work.
    //
    // The fire itself is deliberately *not* placed when there is no frame: fire
    // spread needs `crate::fire::ticks_after_edit` and a live block-tick queue, and
    // an inert fire block would look like a working one. Flint and steel therefore
    // lights portals and nothing else here, and it takes no durability damage — both
    // gaps, both documented in `docs/nether-portals.md`, neither a regression (this
    // item did nothing at all before).
    if held_item == Some(Item::FlintAndSteel) {
        let dimension = source
            .dimension()
            .unwrap_or(crate::dimension::Dimension::Overworld);
        if let Some(cells) = crate::portal::ignite(source, dimension, neighbour) {
            for (cell, cell_state) in &cells {
                source.set_block(cell.x, cell.y, cell.z, *cell_state);
                apply(
                    conn,
                    state,
                    proto.encode_block_update(cell.x, cell.y, cell.z, *cell_state),
                )
                .await?;
            }
            // Publishing to the index is not bookkeeping — it is what lets the
            // *return* trip find this portal instead of building a second one beside
            // it. See `crate::portal::PortalIndex`.
            if let Some(index) = source.portal_index() {
                index.extend(dimension, cells.iter().map(|(cell, _)| *cell));
            }
            return Ok(());
        }
    }
    // Flint and steel or a fire charge clicked directly on a TNT block primes
    // it and clears the block. Checked
    // against the **clicked** cell (`pos`), not `neighbour` the portal arm
    // above reads: the action belongs to the block that was actually clicked,
    // not the face it was clicked from.
    //
    // No `tnt_explodes` gamerule gate here — this call site has no
    // `WorldStateHandle` in scope, matching the portal arm just above, which
    // takes no durability-damage gate either (this crate's own item stacks
    // carry no durability at all — see that arm's own comment). Both are
    // therefore true unconditionally, which is the default here.
    if matches!(held_item, Some(Item::FlintAndSteel | Item::FireCharge)) {
        let clicked = source.block_state_id(pos.x, pos.y, pos.z);
        if clicked.block() == Block::Tnt {
            let air = Block::Air.default_state();
            source.set_block(pos.x, pos.y, pos.z, air);
            apply(
                conn,
                state,
                proto.encode_block_update(pos.x, pos.y, pos.z, air),
            )
            .await?;
            mobs.with(|sim| {
                sim.spawn_tnt(
                    Vec3::new(f64::from(pos.x) + 0.5, f64::from(pos.y), f64::from(pos.z) + 0.5),
                    crate::mobs::tnt::DEFAULT_FUSE_TIME,
                );
            });
            // A fire charge consumes one stack item. Flint and steel wear is
            // outside this crate's item model, so only the charge is shrunk.
            if held_item == Some(Item::FireCharge)
                && consume_one(inventory, hand_native, game_mode)
                && game_mode != GameMode::Creative
            {
                let remainder = inventory.native(hand_native).cloned();
                if let Some(menu_slot) = window_zero_menu_slot(hand_native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                    )
                    .await?;
                }
            }
            return Ok(());
        }
    }
    // Vanilla's own ender-eye-item use-on routine: an eye of ender placed into an unfired
    // `end_portal_frame`. Also ahead of the placement branch — `ender_eye` is
    // not a block item, so the census below cannot reach it at all.
    //
    // `crate::portal::ignite_end_portal_frame` is the pure decision (its own
    // doc derives the ring's "every rim frame faces the centre" rule from
    // vanilla's own block-pattern engine, rather than porting that generic engine); this
    // call site owns every write, the same split `ignite` above uses. Vanilla
    // always writes `eye=true` and consumes the eye on any unfired frame,
    // whether or not a ring completes; the 3x3 `end_portal` fill only follows
    // when this eye is the twelfth.
    if held_item == Some(Item::EnderEye) {
        if let Some(ignition) = crate::portal::ignite_end_portal_frame(source, pos) {
            let (frame_pos, frame_state) = &ignition.frame;
            source.set_block(frame_pos.x, frame_pos.y, frame_pos.z, *frame_state);
            apply(
                conn,
                state,
                proto.encode_block_update(frame_pos.x, frame_pos.y, frame_pos.z, *frame_state),
            )
            .await?;
            if let Some(fill) = &ignition.portal_fill {
                for (cell, cell_state) in fill {
                    source.set_block(cell.x, cell.y, cell.z, *cell_state);
                    apply(
                        conn,
                        state,
                        proto.encode_block_update(cell.x, cell.y, cell.z, *cell_state),
                    )
                    .await?;
                }
            }
            // Vanilla's own item-stack shrink(1), unconditional in vanilla rather than
            // routed through `consume(1, user)` — but `consume_one`'s
            // creative no-op is still the right behaviour either way.
            if consume_one(inventory, hand_native, game_mode) && game_mode != GameMode::Creative {
                let remainder = inventory.native(hand_native).cloned();
                if let Some(menu_slot) = window_zero_menu_slot(hand_native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                    )
                    .await?;
                }
            }
            return Ok(());
        }
    }
    // The census is the gate: it decides *whether* a placement happens at
    // all and *which* block it writes. `block_entity_for_item` only supplies
    // the live `BlockEntity` for
    // the six items this crate ticks, and is consulted second.
    let placed = held_item
        .and_then(|item| block_items::block_placed_by(item).map(|block| (item, block)));
    // Vanilla's own slab-block can-be-replaced check is the one
    // `canBeReplaced` override a hand placement can hit, and without it a slab
    // clicked onto a matching half-slab lands in the cell *above* instead of
    // doubling. Air, fluids, and tagged replaceable blocks target the clicked cell.
    let doubling_slab = placed.is_some_and(|(_, block)| slab_doubles(clicked, block, face, cursor));
    let clicked_replaceable = crate::chunk::is_air_or_fluid_id(clicked)
        || crate::block_placement::is_replaceable_for_placement(clicked);
    let target = if clicked_replaceable || doubling_slab {
        pos
    } else {
        neighbour
    };
    let target_state = source.block_state_id(target.x, target.y, target.z);
    // Every cell the placement's neighbour fan-out rewrote —
    // empty unless a placement actually happened below.
    let mut changed: Vec<(BlockPos, StateId)> = Vec::new();
    // Every cell a multi-cell attempt claimed before the atomic legality gate.
    // Re-sending these cells on rejection clears any optimistic partner half
    // the client may have shown, just as the primary and clicked cells do.
    let mut attempted_cells: Vec<BlockPos> = Vec::new();
    // Paired with the `block_update` packets in the notify loop below — see
    // `moving_piston_records`.
    let mut piston_records: Vec<(BlockPos, lodestone_core::Nbt)> = Vec::new();
    // The remainder of the held stack after a successful placement consumed one
    // from it, `None` when nothing was placed or the game mode does not consume.
    // Held out here rather than sent inside the placement block because `state` is
    // *shadowed* in there by the placed block state — see the `let (state, extra)`
    // below — so `apply` cannot be reached from inside it.
    let mut placement_remainder: Option<Option<ItemStack>> = None;
    let target_replaceable = crate::chunk::is_air_or_fluid_id(target_state)
        || crate::block_placement::is_replaceable_for_placement(target_state)
        || doubling_slab;
    if target_replaceable {
        if let Some((item, block)) = placed {
            // `placed_block_state` applies the block's own
            // `getStateForPlacement` convention (`crate::block_placement`);
            // a block with no convention keeps the census's bare default
            // state, which `resolve_state_id` resolves faithfully. Resolved
            // ahead of the block-entity registration below (moved up from
            // its original position after it) because the obstruction check
            // needs the real placed *state* — a wall-mounted variant's
            // collision box is not a free-standing one's — and nothing may be
            // registered or written until that check passes.
            let ctx = crate::block_placement::PlaceContext {
                target,
                face,
                cursor,
                yaw: player_yaw,
                pitch: player_pitch,
                sneaking,
            };
            let placement = match placed_block_state(block, &ctx, |p| {
                source.block_state_id(p.x, p.y, p.z)
            }) {
                Some(placement) => placement,
                None => crate::block_placement::Placement {
                    state: block.default_state(),
                    extra: Vec::new(),
                },
            };
            let placement = crate::block_placement::apply_waterlogging(
                placement,
                target,
                |p| source.block_state_id(p.x, p.y, p.z),
            );
            let occupied: Vec<(BlockPos, StateId)> = std::iter::once((target, placement.state))
                .chain(placement.extra.iter().copied())
                .collect();
            for (cell, _) in &occupied {
                if !attempted_cells.contains(cell) {
                    attempted_cells.push(*cell);
                }
            }
            let in_build_height = occupied.iter().all(|(p, _)| {
                source
                    .column(p.x.div_euclid(16), p.z.div_euclid(16))
                    .contains_y(p.y)
            });
            // Vanilla's own block-item can-place → level unobstructed check: a placement that
            // would collide with the placer's own body is refused, not
            // written — see `placement_obstructs_placer`'s own doc for what
            // this does and does not cover yet. `player_pos` is `None` until
            // the first movement packet arrives; skipped rather than refused
            // in that case, the same conservative-elsewhere-but-permissive-
            // here direction `is_legal_bed_respawn` documents for the same
            // gap.
            let obstructed = player_pos.is_some_and(|feet| {
                occupied
                    .iter()
                    .any(|(p, state)| placement_obstructs_placer(*p, *state, feet))
            });
            let placement_legal = in_build_height
                && crate::block_placement::validate_placement(
                    &placement,
                    target,
                    target_replaceable,
                    |p| source.block_state_id(p.x, p.y, p.z),
                )
                && !obstructed;
            if placement_legal {
                let crate::block_placement::Placement { state, extra } = placement;
                let state_id = state;
            if let Some((entity_state, mut entity)) = block_entity_for_item(item.name()) {
                // The two sources must agree on the block name, or we would
                // register a furnace at a position holding some other block.
                // `lodestone-data`'s `the_block_entity_blocks_still_resolve_
                // to_themselves` asserts they do for all six today; this
                // catches a future divergence instead of silently trusting
                // the older table.
                debug_assert_eq!(
                    entity_state.block(), block,
                    "block-entity table and item census disagree on {item:?}"
                );
                // A newly placed sign records the placing player as its editor,
                // allowing the following sign-update packet to pass validation.
                if let crate::block_entities::BlockEntity::Sign(sign) = &mut entity {
                    sign.editor = Some(placer);
                }
                block_entities.with(|registry| registry.insert(target, entity));
            } else if let Some(type_name) = lodestone_data::block_entity_types::block_entity_type(state_id)
                .map(lodestone_data::block_entity_types::block_entity_type_name)
            {
                // State-defined block entities need a registry record even when
                // the item has no specialized constructor. The client renders
                // these positions from the record, so an opaque empty payload
                // keeps the placed state visible and survives save/load handling.
                block_entities.with(|registry| {
                    registry.insert(
                        target,
                        crate::block_entities::BlockEntity::Opaque {
                            id: type_name.to_owned().into(),
                            nbt: lodestone_core::Nbt::End,
                        },
                    );
                });
            }
            source.set_block(target.x, target.y, target.z, state_id);
            // Publish the placement sound to every viewer except the placer.
            // `roll` supplies the per-click seed for choosing the sound variant.
            if let Some(effect) =
                crate::effects::block_placed(target, state_id, roll.to_bits() as i64)
            {
                block_ticks.publish_effect_except(placer, effect);
            }
            // A door's upper half, a bed's head, a chest partner's re-typing:
            // cells the placement owns but the client did not predict, so each
            // needs its own `block_update` below.
            for (p, s) in &extra {
                let extra_id = *s;
                source.set_block(p.x, p.y, p.z, extra_id);
                changed.push((*p, extra_id));
            }
            // A carved pumpkin or jack o'lantern can complete a snow- or
            // iron-golem pattern. The mob simulation reports the consumed
            // pattern cells; this caller clears them to air.
            if matches!(block, Block::CarvedPumpkin | Block::JackOLantern) {
                let construction = mobs.with(|sim| {
                    sim.try_construct_golem(
                        &|x, y, z| source.block_state_id(x, y, z),
                        (target.x, target.y, target.z),
                    )
                });
                if let Some(construction) = construction {
                    for cell in &construction.consumed {
                        let air = Block::Air.default_state();
                        source.set_block(cell.x, cell.y, cell.z, air);
                        changed.push((*cell, air));
                    }
                }
            }
            // A wither skeleton skull or wall skull can complete the
            // soul-sand-and-skull pattern. The mob simulation reports consumed
            // cells; this caller clears them to air.
            if matches!(block, Block::WitherSkeletonSkull | Block::WitherSkeletonWallSkull) {
                let construction = mobs.with(|sim| {
                    sim.try_construct_wither(
                        &|x, y, z| source.block_state_id(x, y, z),
                        (target.x, target.y, target.z),
                    )
                });
                if let Some(construction) = construction {
                    for cell in &construction.consumed {
                        let air = Block::Air.default_state();
                        source.set_block(cell.x, cell.y, cell.z, air);
                        changed.push((*cell, air));
                    }
                }
            }
            // Block placement notifies neighboring cells so redstone state can
            // react immediately. Without this fan-out, dust beside a powered
            // line stays at `power=0`.
            let mut owned_cells = Vec::with_capacity(extra.len() + 1);
            owned_cells.push(target);
            owned_cells.extend(extra.iter().map(|(p, _)| *p));
            for cell in owned_cells {
                let (mut fanout, scheduled) =
                    propagate_placement_with_entities(source, cell, Some(block_entities));
                changed.append(&mut fanout);
                piston_records.extend(moving_piston_records(&scheduled));
                // Delayed reactions are returned through the scheduled-tick queue
                // owned by the world tick loop. Publish that queue even when the
                // synchronous fan-out changed no cells.
                block_ticks.request_scheduled_ticks(scheduled);
                // A fluid at any owned cell needs the same edit notification as
                // the primary cell. The queue deduplicates repeated neighbours,
                // so this remains one logical wake-up per position.
                block_ticks.request_fluid_scheduled_ticks(crate::fluid::ticks_after_edit(
                    source,
                    fluid_env_at(source, cell),
                    cell,
                ));
            }
            // Sand and gravel schedule a gravity check two ticks out. Other
            // placed blocks produce no entry in this feed.
            //
            // The scheduled event makes a sand or gravel block fall when it is
            // placed in air. `state` is used instead of the item name because
            // `gravity_tick::is_gravity_state` matches the resolved block state.
            block_ticks.request_scheduled_ticks(crate::gravity_tick::ticks_after_place_id(target, state_id));
            // A successful placement consumes one held item. Without this update
            // **every placement would be free** — the block would be written,
            // the client would predict its own hotbar and the server would never
            // agree, so the stack would return on the next window sync.
            //
            // Creative placement consumes nothing, so the gate is explicit rather
            // than implied: survival decrements the stack and creative does not.
            //
            // `consume_one` clears the slot outright at a count of one rather than
            // leaving a zero-count stack naming an item, which renders as a block
            // you can place forever.
            if game_mode != GameMode::Creative {
                let native = hand_native;
                if consume_one(inventory, native, game_mode) {
                    placement_remainder = Some(inventory.native(native).cloned());
                }
            }
            } // !obstructed
        }
    }
    // Tell the client's window-0 hotbar slot what the server thinks is left —
    // menu slots `36..=44` map onto native `0..=8` (vanilla's `InventoryMenu`),
    // the same server-initiated slot update the composter, brewing-stand,
    // bone-meal and spawn-egg arms above send after they consume. `state_id` is
    // `0`: this crate applies a container diff verbatim and never validates a
    // stale id (`apply_container_clicked`).
    if let Some(remainder) = placement_remainder
        && let Some(menu_slot) = window_zero_menu_slot(hand_native)
    {
        apply(
            conn,
            state,
            proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
        )
        .await?;
    }
    // `pos`/`neighbour` first (the clicked face and the placed cell), then every
    // cell owned by the attempted placement, then every cell the fan-out
    // actually rewrote. This includes partner cells after rejection, so a
    // client prediction cannot leave a ghost upper half behind. Deduped because
    // `target` is always one of the first two.
    let notify = placement_update_positions(pos, neighbour, &attempted_cells, &changed);
    for p in notify {
        let current = source.block_state_id(p.x, p.y, p.z);
        let directive = proto.encode_block_update(p.x, p.y, p.z, current);
        apply(conn, state, directive).await?;
        if let Some((_, nbt)) = piston_records.iter().find(|(pos, _)| *pos == p) {
            let directive =
                proto.encode_block_entity_data(p, crate::piston::PISTON_BLOCK_ENTITY, nbt);
            apply(conn, state, directive).await?;
        }
    }
    // Placing a torch has to light the column, and the `block_update` packets
    // above carry no light. Read back out of `source` rather than reusing the
    // placed state string, because the fan-out may have rewritten the cell since
    // (and because `placed_block_state`'s own result is shadowed inside the
    // placement block above). `target_state` is the cell as it was *before* the
    // placement, captured before the `set_block`.
    {
        let placed_state = source.block_state_id(target.x, target.y, target.z);
        resend_column_for_light(conn, proto, source, state, pending_relights, target_state, placed_state, target)
            .await?;
    }
    Ok(())
}

/// Return the authoritative block-update positions for one use-on attempt.
///
/// The clicked and adjacent cells are always present, while attempted partner
/// cells are included even when the atomic placement gate rejects the write.
/// Fan-out changes are appended last. Keeping this ordering in one production
/// helper makes the correction contract testable without a socket fixture.
fn placement_update_positions(
    clicked: BlockPos,
    neighbour: BlockPos,
    attempted: &[BlockPos],
    changed: &[(BlockPos, StateId)],
) -> Vec<BlockPos> {
    let mut notify = vec![clicked, neighbour];
    for pos in attempted {
        if !notify.contains(pos) {
            notify.push(*pos);
        }
    }
    for (pos, _) in changed {
        if !notify.contains(pos) {
            notify.push(*pos);
        }
    }
    notify
}

/// Converts scheduled piston ticks into block-entity update payloads.
///
/// A `moving_piston` block update marks an animated cell; the payload from the
/// scheduled completion tick identifies the moving state. Send the payload
/// after the cell's `block_update` so the client has the matching cell record.
fn moving_piston_records(
    scheduled: &[ScheduledTick<ScheduledTickKind>],
) -> Vec<(BlockPos, lodestone_core::Nbt)> {
    scheduled
        .iter()
        .filter(|pending| crate::piston::is_finish_kind(&pending.kind))
        .filter_map(|pending| {
            let entity = crate::piston::parse_finish_kind(&pending.kind)?;
            Some((
                BlockPos::new(pending.pos.0, pending.pos.1, pending.pos.2),
                entity.update_tag(),
            ))
        })
        .collect()
}

/// Runs the neighbor-update fan-out for a block placed at `target`, persists
/// each resulting change through `source`, and returns those changes for the
/// client update path.
///
/// The fan-out handles synchronous redstone reactions inline and returns
/// delayed reactions as scheduled ticks for the world tick loop. This keeps
/// player placement and world-tick updates on the same state-transition path.
///
/// # Delayed reactions
///
/// The local scheduled-tick queue records relative delays. Dust resolves
/// synchronously, with a measured zero-tick reaction against the live 26.2
/// oracle; torches, repeaters, comparators, and observers schedule checks two
/// or more ticks out. The world tick loop owns those delayed entries and drains
/// them from the feed.
///
/// # The delayed half travels out with the return value
///
/// The second element contains every scheduled block tick. `trigger_tick` is a
/// relative delay; the world tick loop rebases it onto its own counter after
/// [`BlockTickFeed`] receives the entries.
///
/// Publish the scheduled entries rather than invoking the fan-out a second
/// time: the first pass consumes the synchronous change, while a second pass
/// sees settled state and misses delayed reactions. A repeater measured at four
/// delay settings confirms the distinction: the inline path finishes
/// `powered=false` with output dust at `0`, while a second fan-out finishes
/// `powered=true` at `15`. The test
/// `redstone_placement_gate::the_split_between_the_synchronous_and_delayed_halves_changes_no_outcome`
/// covers this boundary.
///
/// Changes are sent to this connection through the `encode_block_update` loop;
/// the shared tick feed carries only delayed reactions.
///
/// Test helper for placement fan-out without a block-entity registry. Production
/// callers use [`propagate_placement_with_entities`] when command-block state
/// must participate in neighbor reactions.
#[cfg(test)]
pub(crate) fn propagate_placement<S>(
    source: &S,
    target: BlockPos,
) -> (Vec<(BlockPos, StateId)>, Vec<ScheduledTick<ScheduledTickKind>>)
where
    S: ChunkSource + ?Sized,
{
    propagate_placement_with_entities(source, target, None)
}

/// [`propagate_placement`], with an optional [`BlockEntityHandle`] for
/// command-block state during neighbor reactions. `None` has the same behavior
/// as [`propagate_placement`] itself.
pub(crate) fn propagate_placement_with_entities<S>(
    source: &S,
    target: BlockPos,
    block_entities: Option<&BlockEntityHandle>,
) -> (Vec<(BlockPos, StateId)>, Vec<ScheduledTick<ScheduledTickKind>>)
where
    S: ChunkSource + ?Sized,
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
    let mut block_ticks: ScheduledTickQueue<ScheduledTickKind> = ScheduledTickQueue::new();
    // `react_at_placement`, not `propagate_and_react`: the placed block owes
    // itself a `setPlacedBy` reaction that the neighbour pass structurally
    // cannot deliver. See that function's own doc comment.
    let events = crate::random_tick::react_at_placement_with_entities(
        &mut column,
        min_x,
        min_z,
        // The live world, so the placed block's own reactions and the
        // neighbour fan-out both reach an already-loaded neighbouring
        // column. `&source` rather than `source`: `S` is `?Sized` here (a
        // connection is served a type-erased `dyn ChunkSource`), and `&S`
        // is what unsizes to the `&dyn ChunkSource` this wants — see
        // `chunk`'s borrowed-source forwarding impl.
        &source,
        target.x,
        target.y,
        target.z,
        &mut block_ticks,
        // Zero, so every `trigger_tick` below *is* the delay — see the doc
        // comment above.
        0,
        block_entities,
    );
    // `drain_due`, not `iter`: this queue is a `BinaryHeap` and `iter` yields in
    // unspecified order, while `drain_due` yields `DRAIN_ORDER`. The loop
    // re-`schedule`s each entry and so assigns it a fresh `sub_tick_order`, which
    // makes *this* order the one that decides tie-breaks later — so it has to be
    // deterministic. `u64::MAX` drains everything regardless of delay.
    let scheduled: Vec<ScheduledTick<ScheduledTickKind>> =
        block_ticks.drain_due(u64::MAX, usize::MAX);
    let changed = events
        .into_iter()
        .map(|event| {
            let (ex, ey, ez) = event.pos;
            source.set_block(ex, ey, ez, event.to);
            (BlockPos::new(ex, ey, ez), event.to)
        })
        .collect();
    (changed, scheduled)
}

/// Vanilla's own tripwire-block affect-neighbors-after-removal routine's bridge from a [`ChunkSource`]
/// to [`crate::random_tick::react_at_removal`] — the block-**removal** twin
/// of [`propagate_placement_with_entities`], same column-snapshot shape.
/// `wire_state_before_removal` is the removed block's own state just before
/// the caller overwrote the cell; anything other than a tripwire is a fast
/// no-op via `react_at_removal`'s own guard, so a caller may call this
/// unconditionally on every break.
pub(crate) fn propagate_removal_with_entities<S>(
    source: &S,
    target: BlockPos,
    wire_state_before_removal: StateId,
) -> (Vec<(BlockPos, StateId)>, Vec<ScheduledTick<ScheduledTickKind>>)
where
    S: ChunkSource + ?Sized,
{
    let cx = target.x.div_euclid(16);
    let cz = target.z.div_euclid(16);
    let (min_x, min_z) = (cx * 16, cz * 16);
    // Reflects the removal already applied — same contract
    // `propagate_placement_with_entities` relies on for its own placement.
    let mut column = source.column(cx, cz);
    if target.y < column.min_y || target.y >= column.min_y + column.height {
        return (Vec::new(), Vec::new());
    }
    let mut block_ticks: ScheduledTickQueue<ScheduledTickKind> = ScheduledTickQueue::new();
    let events = crate::random_tick::react_at_removal(
        &mut column,
        min_x,
        min_z,
        // Same live world, same reason as `propagate_placement_with_entities`:
        // a tripwire's controlling hook is up to 41 cells away, so it is
        // usually not in the column holding the cell that was broken.
        &source,
        target.x,
        target.y,
        target.z,
        wire_state_before_removal,
        &mut block_ticks,
        0,
    );
    let scheduled: Vec<ScheduledTick<ScheduledTickKind>> =
        block_ticks.drain_due(u64::MAX, usize::MAX);
    let changed = events
        .into_iter()
        .map(|event| {
            let (ex, ey, ez) = event.pos;
            source.set_block(ex, ey, ez, event.to);
            (BlockPos::new(ex, ey, ez), event.to)
        })
        .collect();
    (changed, scheduled)
}

/// Per-connection difficulty and game-rule session state.
///
/// The world handle stores the shared difficulty and rule values; packet
/// handlers validate requests there and send confirmations through the
/// connection's protocol. Permission checks occur at packet dispatch, while
/// this helper only reads or writes the accepted world state.
/// Applies a difficulty-change request (`ServerBound::DifficultyChanged`).
/// The dispatch layer has already applied the permission gate. This helper
/// reads the shared difficulty and lock state and confirms it to this client.
async fn apply_difficulty_change<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    world: &crate::world_state::WorldStateHandle,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    let (difficulty, locked) = world.difficulty();
    let directive = proto.encode_change_difficulty(difficulty, locked);
    apply(conn, state, directive).await
}

/// Applies a game-rule change request (`ServerBound::GameRuleChanged`).
/// Permission filtering occurs in packet dispatch, so an empty `entries` list
/// produces an empty confirmation. Each key and value is parsed by
/// [`crate::world_state::WorldStateHandle::set_rule`]; unknown keys and invalid
/// values are omitted rather than stored verbatim.
async fn apply_game_rule_changed<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    world: &crate::world_state::WorldStateHandle,
    entries: Vec<(String, String)>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    // Confirm only entries accepted by the world-rule parser, so a rejected key
    // is visibly absent from the reply rather than silently acknowledged.
    let accepted: Vec<(String, String)> = entries
        .iter()
        .filter_map(|(key, value)| {
            world
                .set_rule(key, value)
                .ok()
                .map(|parsed| (key.clone(), parsed.serialize()))
        })
        .collect();
    let directive = proto.encode_game_rule_values(&accepted);
    apply(conn, state, directive).await
}

/// Applies a `client_command` request (`ServerBound::ClientCommand`) for the
/// actions modeled by this server.
///
/// # `action == 1`, `REQUEST_STATS`
///
/// The statistics reply comes from [`AdvancementManager::stats_snapshot`] and
/// is encoded by [`ServerProtocol::encode_award_stats`]. Protocols without a
/// statistics encoder send no frame.
///
/// # `action == 0`, `PERFORM_RESPAWN`
///
/// **The respawn position is the player's bed or charged respawn anchor when it
/// remains usable**, and the world spawn otherwise. [`crate::respawn_anchor::resolve`]
/// re-reads the block at death time, so a broken, uncharged or obstructed one
/// falls back to the world spawn, clears the point and tells the player. An
/// anchor spends one charge and plays its depletion sound to this player only.
/// An anchor respawn in a dimension the connection is already viewing keeps the
/// connection there ([`DimensionReset::in_place`]).
///
/// Respawn resets the modeled player vitals and burn state, sends the
/// authoritative position, and refreshes the health and air displays. A request
/// from a living player is ignored.
///
/// # `action == 2`, `REQUEST_GAMERULE_VALUES`
///
/// Action `2` returns the accepted rule entries when the permission level allows
/// it. Rules that have not been set are absent from the reply.
#[allow(clippy::too_many_arguments)]
async fn apply_client_command<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    vitals: &mut PlayerVitals,
    burn: &mut crate::burning::BurnState,
    // The fall accumulator, reset whenever respawn changes the player's
    // position.
    fall: &mut FallTracker,
    teleport_acknowledgements: &mut Option<TeleportAcknowledgements>,
    // The world spawn resolved during the join sequence. It is the fallback
    // when no usable per-player bed position exists.
    //
    // The fallback for a missing or unusable per-player bed position.
    world_spawn: Vec3,
    // This player's bed or anchor point, if they have set one. Resolved against
    // `source` rather than used directly: see this function's own doc comment for
    // why the block is re-read at death time. `&mut` because an unusable point is
    // cleared.
    respawn: &mut Option<RespawnPoint>,
    // The dimension the connection is viewing now, and the one it joined in. The
    // respawn point is re-read in whichever dimension it names.
    source: &dyn ChunkSource,
    home: &dyn ChunkSource,
    world: &crate::world_state::WorldStateHandle,
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
    action: i32,
    // Permission level for the rule-values request and mutation requests.
    permission_level: u8,
    // The readiness marker must be received again after a respawn before
    // movement-dependent simulation resumes.
    client_loaded: &mut bool,
    // Set when the respawn lands outside the view the connection already has;
    // otherwise remains `None`.
    dimension_reset: &mut Option<connection_travel::DimensionReset>,
    // Records the perform-respawn that answers an End-exit win announcement.
    end_exit: &mut connection_travel::EndExit,
    // The dimension change a respawn that stays in a non-home dimension sends.
    game_mode: GameMode,
    // Where a spent anchor charge is published, and the feed its block change
    // reaches other viewers through.
    block_ticks: &BlockTickFeed,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    match action {
        // The credits were dismissed (or skipped): the connection loop moves
        // the player home, keeping everything they carry.
        0 if end_exit.is_won() => end_exit.request_respawn(),
        0 if vitals.health() <= 0.0 => {
            vitals.respawn();
            burn.reset();
            *client_loaded = !proto.sends_player_loaded();
            // The respawn point is re-read in its own dimension; a missing one
            // falls back to the world spawn. Everything the client needs to
            // follow the player there is sent before health and air so the HUD
            // refreshes for the updated state.
            if let Some(reset) = connection_travel::perform_respawn(
                conn,
                proto,
                state,
                home,
                source,
                respawn,
                world_spawn,
                game_mode,
                teleport_acknowledgements,
                block_ticks,
                false,
            )
            .await?
            {
                *dimension_reset = Some(reset);
            }
            apply(
                conn,
                state,
                proto.encode_set_health(
                    vitals.health(),
                    vitals.food().food_level(),
                    vitals.food().saturation(),
                ),
            )
            .await?;
            apply(conn, state, proto.encode_air_supply_update(vitals.air_supply())).await?;
            // The teleport above is a position snap, so the next `PlayerMoved`
            // sample must not be diffed against the y the player died at — a
            // death at y=70 respawning at y=64 would otherwise bank 6 blocks of
            // phantom fall distance against the next landing.
            fall.reset();
        }
        1 => {
            let snapshot = advancements.stats_snapshot(player_uuid);
            apply(conn, state, proto.encode_award_stats(&snapshot)).await?;
        }
        2 => {
            // A denied request produces no response; an allowed request returns
            // the accepted rule entries.
            if permission_level >= COMMANDS_GAMEMASTER_LEVEL {
                apply(
                    conn,
                    state,
                    proto.encode_game_rule_values(&world.rule_entries()),
                )
                .await?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Applies a `SET_CARRIED_ITEM` request (`ServerBound::CarriedItemChanged`),
/// mirroring vanilla's own carried-item-set handler, which
/// writes straight into its own selected-slot setter and sends **no**
/// confirmation packet back — see that `ServerBound` variant's own doc
/// comment. A no-op if `slot` is already out of range (the protocol decoder
/// only ever constructs this variant with a validated slot, so this guard is
/// a second, defensive layer rather than the primary one — see
/// `PlayerInventory::set_selected_hotbar_slot`'s own doc comment for why it
/// degrades instead of panicking).
fn apply_carried_item_changed(inventory: &mut PlayerInventory, slot: u8) {
    if let Some(slot) = HotbarSlot::new(slot) {
        inventory.select_hotbar_slot(slot);
    }
}

/// Applies a `SET_CREATIVE_MODE_SLOT` write (`ServerBound::CreativeModeSlotSet`).
/// The wire slot uses the same numbering as [`PlayerInventory::apply_menu_slot_change`];
/// unsupported and negative values are ignored. Only creative players may use
/// this packet, because it can write arbitrary inventory contents.
fn apply_creative_mode_slot_set(
    inventory: &mut PlayerInventory,
    slot: i16,
    item: Option<ItemStack>,
    creative: bool,
) {
    if !creative {
        return;
    }
    if let Some(slot) = MenuSlot::from_raw(i32::from(slot)) {
        inventory.apply_menu_slot(slot, item);
    }
}

/// Reads one menu's slots in menu order, from whichever backing stores its
/// [`MenuLayout`] names.
///
/// `own` is the open block entity's own slots (empty for a menu with none), and
/// `grid` is the [`CraftingState`] behind the `Grid`/`Result` kinds.
fn read_menu(
    layout: &MenuLayout,
    inventory: &PlayerInventory,
    grid: Option<&CraftingState>,
    own: &[Option<ItemStack>],
) -> Vec<Option<ItemStack>> {
    layout
        .iter()
        .map(|(_, kind)| match kind {
            SlotKind::Player(native) => inventory.native(native).cloned(),
            SlotKind::Container(index) => own.get(index).cloned().flatten(),
            SlotKind::Grid(cell) => grid.and_then(|g| g.input(cell).cloned()),
            SlotKind::Result => grid.and_then(|g| g.result().cloned()),
        })
        .collect()
}

/// The join snapshot's window counter is **`1`, not `0`**. The initial content
/// frame increments the counter from zero before sending it.
///
/// # Why the other window-`0` sends in this file can keep their constant `0`
///
/// The client accepts the counter on content and slot frames; this server does
/// not validate the echoed value on clicks. Other window-`0` updates therefore
/// retain their constant `0`, while the join snapshot uses the initial counter
/// value required by the opening sequence.
const JOIN_CONTENT_STATE_ID: i32 = 1;

/// Sends the joining player's window-`0` inventory snapshot. The snapshot
/// contains every menu slot and the carried cursor stack, so the client can
/// render the inventory before any click or movement packet arrives.
///
/// The snapshot is sent at the top of [`serve_play`], after the login metadata
/// and before the deferred chunk stream. Window `0` uses
/// `encode_container_content` because the content frame carries a slot list
/// and cursor; a single-slot frame cannot represent the whole inventory.
/// `JOIN_CONTENT_STATE_ID` is `1`, the first counter assigned to this window.
fn join_inventory_snapshot<P: ServerProtocol>(
    proto: &P,
    inventory: &PlayerInventory,
) -> ServerDirective {
    let items = read_menu(
        &MenuLayout::player(),
        inventory,
        Some(inventory.crafting()),
        &[],
    );
    proto.encode_container_content(
        0,
        JOIN_CONTENT_STATE_ID,
        &items,
        inventory.click_state().carried.as_ref(),
    )
}

/// Sends the experience bar snapshot owed to a joining player.
///
/// # Why this exists
///
/// The frame is sent once at join and after every
/// [`crate::experience::PlayerExperience`] mutation, including furnace XP.
/// This keeps the bar populated in every game mode and after both level and
/// progress changes.
///
/// # Argument order
///
/// `(progress, level, total)` is the order required by the protocol encoder.
/// Keep the two integer fields explicit here because swapping adjacent VarInts
/// still produces a valid frame with incorrect values.
fn join_experience<P: ServerProtocol>(
    proto: &P,
    experience: &crate::experience::PlayerExperience,
) -> ServerDirective {
    proto.encode_set_experience(
        experience.progress(),
        experience.level(),
        experience.total(),
    )
}

/// [`PlayerRegistry::set_experience`]'s producer half — call this everywhere
/// [`join_experience`]/`encode_set_experience` is sent to the owning
/// connection. The wrapper keeps the optional registry check in one place.
fn republish_experience(players: Option<&PlayerRegistry>, uuid: uuid::Uuid, experience: &crate::experience::PlayerExperience) {
    if let Some(registry) = players {
        registry.set_experience(uuid, experience.level(), experience.query_points());
    }
}

/// Publishes the connection-owned inventory as an owned host-observation
/// snapshot. The registry never becomes an inventory writer.
fn republish_inventory(players: Option<&PlayerRegistry>, uuid: uuid::Uuid, inventory: &PlayerInventory) {
    if let Some(registry) = players {
        registry.set_inventory(uuid, inventory);
    }
}

/// The local player's combat-relevant attributes as wire-shaped snapshots —
/// [`PlayerInventory::combat_stats`]'s already-folded `AttributeMap`, one
/// snapshot per **named** attribute below, each carrying its final value as
/// `base` and an empty modifier list.
///
/// # Every attribute is named explicitly — this is not `AttributeMap::iter`
///
/// `AttributeMap` is sparse: an attribute only appears in it once *something*
/// has touched it ([`lodestone_entity::equipment::apply_equipment`] calls
/// `get_or_default` only for a piece that is actually equipped). Iterating it
/// therefore **omits** `minecraft:armor` entirely the moment the last piece
/// comes off, rather than including it at `0.0` — and the client's own merge
/// (`lodestone_ecs::ingest::apply_entity_attributes`) treats an attribute
/// absent from a packet as *unchanged*, not as *reset to default*: it only
/// overwrites entries the packet actually names. The reported symptom was
/// exactly this — the bar tracked every equip and every partial removal
/// correctly (a non-zero value was always sent) and then froze on the last
/// piece, because that transition was the one case where the whole attribute
/// stopped being sent rather than being sent as zero. Reading each attribute
/// through [`lodestone_entity::attribute::AttributeMap::value`] instead —
/// which already falls back to the registry default for an attribute the map
/// has no entry for — closes that gap for every attribute named here, not
/// only `armor`.
///
/// # Why empty modifiers
///
/// Rather than re-publishing the per-item ones `apply_equipment` built the
/// fold from: the client's own fold
/// (`instance_from_snapshot`/`AttributeInstance::value`,
/// `crates/lodestone-entity/src/attribute.rs`) is a no-op over a bare base
/// value with no modifiers, and re-deriving the exact same modifier ids and
/// operations at the wire would be a second copy of
/// `lodestone_entity::equipment`'s table to keep in step for no observable
/// difference — the client never inspects an individual modifier, only the
/// folded result (the shell's `Session::armour_value` and the attack-speed
/// and water-efficiency readers documented alongside it).
fn player_attribute_snapshots(inventory: &PlayerInventory) -> Vec<EntityAttributeSnapshot> {
    // Every attribute `lodestone_entity::equipment::item_modifiers` can ever
    // publish a modifier for. Adding a new equipment-driven attribute there
    // means adding its name here too, or it inherits this exact bug for
    // itself.
    const COMBAT_ATTRIBUTES: [&str; 4] = [
        "minecraft:armor",
        "minecraft:armor_toughness",
        "minecraft:knockback_resistance",
        "minecraft:attack_damage",
    ];
    let attrs = inventory.combat_stats().attributes;
    COMBAT_ATTRIBUTES
        .into_iter()
        .filter_map(|name| {
            let attribute: lodestone_model::Identifier = name.parse().ok()?;
            let base = attrs.value(&attribute)?;
            Some(EntityAttributeSnapshot {
                attribute,
                base,
                modifiers: Vec::new(),
            })
        })
        .collect()
}

/// One folded maximum-health snapshot for the local player's active effects.
///
/// This travels through the existing local-player attribute packet rather than
/// inventing a status-effect-specific health wire path. The client already
/// merges that packet into its attribute component, which is also where the
/// HUD obtains the number of heart rows.
fn max_health_snapshot(max_health: f32) -> EntityAttributeSnapshot {
    EntityAttributeSnapshot {
        attribute: "minecraft:max_health"
            .parse()
            .expect("built-in max-health attribute identifier"),
        base: f64::from(max_health),
        modifiers: Vec::new(),
    }
}

/// Publishes the one effect-derived attribute that changes the authoritative
/// health ceiling, plus the current-health packet that must be clamped when an
/// expiring effect lowers that ceiling.
///
/// Calling this after effect application and after the timer's expiry pass
/// makes add, amplifier replacement, hidden-chain restoration, and removal
/// share one transition. A no-op effect tick produces no packets.
async fn sync_effect_max_health<T, P>(
    conn: &mut Connection<T>,
    state: &mut State,
    proto: &P,
    vitals: &mut PlayerVitals,
    effects: &crate::mob_effects::ActiveEffects,
) -> Result<bool, ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    if !vitals.set_max_health(effects.max_health()) {
        return Ok(false);
    }
    let snapshot = max_health_snapshot(vitals.max_health());
    apply(conn, state, proto.encode_update_attributes(std::slice::from_ref(&snapshot))).await?;
    apply(
        conn,
        state,
        proto.encode_set_health(
            vitals.health(),
            vitals.food().food_level(),
            vitals.food().saturation(),
        ),
    )
    .await?;
    Ok(true)
}

/// Sends [`player_attribute_snapshots`] as an `update_attributes` packet —
/// the producer half of the armour bar. The client half
/// (`Session::armour_value`, `lodestone_shell::hud`, the v770 adapter's
/// `UPDATE_ATTRIBUTES` decode) was already complete; this crate had no
/// encoder at all, so the HUD row read a permanent `None` no matter what was
/// equipped.
///
/// Sent once at join (a client that never receives this packet has no armour
/// attribute at all, not a zero one) and
/// again after any player-inventory mutation that can change combat
/// equipment (`ServerBound::ContainerClicked`, the right-click armour swap in
/// [`apply_use_item_on`]).
fn join_attributes<P: ServerProtocol>(proto: &P, inventory: &PlayerInventory) -> ServerDirective {
    proto.encode_update_attributes(&player_attribute_snapshots(inventory))
}

/// Applies a `CONTAINER_CLICK` by **deriving** its result server-side
/// (`ServerBound::ContainerClicked`).
///
/// The click's slot/button/click-type go into [`crate::container_click::do_click`],
/// vanilla's own container-menu do-click routine, run over the menu read out of
/// this connection's real state. The client's `changed_slots`/`carried_item`
/// prediction is **never stored** — it is compared against what was derived, and a
/// disagreement sends a full corrective `container_set_content`. So an honest
/// client sees no extra traffic and a client naming an item it does not own is
/// corrected on the same packet.
///
/// The server derives the full menu result instead of trusting the client's
/// claimed diff, so a client cannot mint an item by naming an arbitrary slot.
/// The comparison covers crafting results as well as ordinary menu slots.
///
/// A click against a non-zero `window_id` that does not match the connection's
/// own tracked [`OpenContainer`] (a stale click for a window since closed or
/// replaced) is dropped rather than misapplied to whatever is open now.
///
/// Three menu shapes are served, and which one this is comes from the tracked
/// window rather than from the packet: window `0` is the player screen, an open
/// crafting table is [`MenuKind::CraftingTable`], anything else is a block-entity
/// container.
///
/// Returns the correcting directive to send (if any) and the stacks that left the
/// menu into the world (a throw, or a click outside the window) for the caller to
/// spawn. A directive rather than a send, so this stays a pure function of the
/// click and the unit tests below drive it with no connection.
#[allow(clippy::too_many_arguments)]
fn apply_container_clicked<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    block_entities: &BlockEntityHandle,
    open_container: Option<&mut OpenContainer>,
    window_id: i32,
    click: Click,
    claimed_slots: &[(i32, Option<ItemStack>)],
    claimed_cursor: Option<&ItemStack>,
    creative: bool,
    xp_level: i32,
    // The narrow crafting-station hook registry — see
    // `apply_use_item_on`'s own `hooks` comment for why this is a targeted
    // handle rather than the whole `WorldStateHandle`.
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> (Option<ServerDirective>, Vec<ItemStack>) {
    // Which menu, and where its non-player slots live.
    let mut open = open_container;

    // Lectern slot zero is a read-only display. It is intentionally handled
    // before the generic click state machine, whose `Container` slot kind is
    // otherwise placeable. A forged click receives the authoritative one-slot
    // content rather than being allowed to write arbitrary items into the
    // block entity.
    if window_id != 0
        && open
            .as_ref()
            .is_some_and(|tracked| tracked.window_id == window_id && tracked.shape == MenuKind::Lectern)
    {
        let tracked = open.as_mut().expect("lectern predicate checked Some");
        let own = block_entities.with(|reg| {
            reg.get(tracked.pos)
                .map(BlockEntity::container_slots)
                .unwrap_or_default()
        });
        let items = read_menu(&MenuLayout::lectern(), inventory, None, &own);
        let state_id = tracked.next_state_id();
        return (
            Some(proto.encode_container_content(
                tracked.window_id,
                state_id,
                &items,
                inventory.click_state().carried.as_ref(),
            )),
            Vec::new(),
        );
    }

    // The workstation economy (anvil/grindstone/smithing) is a
    // second positionless-scratch shape alongside the crafting table, but its
    // cells live in `PlayerInventory::workstation` (a flat cell vector) rather
    // than a `CraftingState`, so it is handled by a dedicated function instead
    // of forcing it through `read_menu`'s `CraftingState`-shaped grid.
    if window_id != 0 {
        let combiner_station = open.as_ref().and_then(|tracked| {
            (tracked.window_id == window_id)
                .then_some(tracked.shape)
                .and_then(|shape| match shape {
                    MenuKind::ItemCombiner { station, .. } => Some(station),
                    _ => None,
                })
        });
        if let Some(station) = combiner_station {
            let tracked = open.expect("checked Some above via combiner_station");
            return apply_workstation_clicked(
                proto,
                inventory,
                tracked,
                click,
                claimed_slots,
                claimed_cursor,
                creative,
                station,
                xp_level,
                hooks,
            );
        }
        let is_enchanting = open
            .as_ref()
            .is_some_and(|tracked| tracked.window_id == window_id && tracked.shape == MenuKind::Enchanting);
        if is_enchanting {
            let tracked = open.expect("checked Some above via is_enchanting");
            return apply_enchanting_clicked(proto, inventory, tracked, click, claimed_slots, claimed_cursor, creative);
        }
    }

    let (layout, pos, uses_table_grid) = if window_id == 0 {
        (MenuLayout::player(), None, false)
    } else {
        let Some(tracked) = open.as_mut() else {
            return (None, Vec::new());
        };
        if tracked.window_id != window_id {
            return (None, Vec::new());
        }
        match tracked.shape {
            MenuKind::CraftingTable => (MenuLayout::crafting_table(), Some(tracked.pos), true),
            MenuKind::Lectern => (MenuLayout::lectern(), Some(tracked.pos), false),
            _ => (
                MenuLayout::container(tracked.container_size),
                Some(tracked.pos),
                false,
            ),
        }
    };

    let own = match (pos, uses_table_grid) {
        (Some(pos), false) => block_entities.with(|reg| {
            reg.get(pos)
                .map(BlockEntity::container_slots)
                .unwrap_or_default()
        }),
        _ => Vec::new(),
    };
    let grid_owner = if uses_table_grid {
        inventory.table_crafting().cloned()
    } else if window_id == 0 {
        Some(inventory.crafting().clone())
    } else {
        None
    };

    let mut slots = read_menu(&layout, inventory, grid_owner.as_ref(), &own);
    // The state the client saw when this menu was last sent is the baseline
    // for the agreement check below. Every disagreement receives a full
    // content packet.
    let before = slots.clone();
    let mut state = inventory.click_state().clone();
    // The open grid's dimensions, so `do_click_with` can re-derive the result slot
    // mid-click (`slotsChanged`) — which is what makes a shift-click on the result
    // craft repeatedly instead of once.
    let (grid_width, grid_height) = grid_owner
        .as_ref()
        .map_or((0, 0), |grid| (grid.width(), grid.height()));
    let recipe = |cells: &[Option<ItemStack>]| {
        crate::crafting::derive_result(grid_width, grid_height, cells)
    };
    // The last nested-item selection for this menu slot. The right-click
    // extraction branch reads it to choose which nested item comes out; the
    // following pickup click performs the extraction.
    let selected_bundle = |slot: usize| {
        MenuSlot::from_index(slot)
            .and_then(|slot| inventory.selected_bundle_item(slot))
            .map(BundleItemSlot::index)
    };
    let selected_bundle: Option<SelectedBundleIndex<'_>> = Some(&selected_bundle);
    let dropped = do_click_with(
        &layout,
        &mut slots,
        &mut state,
        click,
        creative,
        Some(&recipe),
        // `Player`/`Container`/`CraftingTable` layouts have no `mayPickup`
        // override anywhere in vanilla — only `ItemCombinerMenu`'s result
        // slot does, and that shape is handled by `apply_workstation_clicked`
        // above, never reaching here.
        None,
        selected_bundle,
    );
    *inventory.click_state_mut() = state;

    // Write back. Grid cells go last and through `set_input`, so the result slot
    // is re-derived from the grid rather than copied out of `slots` — a stale
    // result is the same defect as a trusted one.
    let mut grid_writes: Vec<(usize, Option<ItemStack>)> = Vec::new();
    let mut own_writes: Vec<(usize, Option<ItemStack>)> = Vec::new();
    for (index, kind) in layout.iter() {
        match kind {
            SlotKind::Player(native) => inventory.set_native(native, slots[index].clone()),
            SlotKind::Container(own_index) => own_writes.push((own_index, slots[index].clone())),
            SlotKind::Grid(cell) => grid_writes.push((cell, slots[index].clone())),
            SlotKind::Result => {}
        }
    }
    if let Some(pos) = pos.filter(|_| !own_writes.is_empty()) {
        block_entities.with(|reg| {
            if let Some(entity) = reg.get_mut(pos) {
                for (index, item) in &own_writes {
                    if let Some(index) = entity.container_slot(*index) {
                        entity.set_container_slot_at(index, item.clone());
                    }
                }
            }
        });
    }
    if !grid_writes.is_empty() {
        let grid = if uses_table_grid {
            inventory.table_crafting_mut()
        } else {
            Some(inventory.crafting_mut())
        };
        if let Some(grid) = grid {
            for (cell, item) in grid_writes {
                grid.set_input(cell, item);
            }
        }
    }

    // Re-read, so the comparison and the correction both carry the *derived*
    // result rather than whatever `do_click` left in the result slot.
    let own = match (pos, uses_table_grid) {
        (Some(pos), false) => block_entities.with(|reg| {
            reg.get(pos)
                .map(BlockEntity::container_slots)
                .unwrap_or_default()
        }),
        _ => Vec::new(),
    };
    let grid_owner = if uses_table_grid {
        inventory.table_crafting().cloned()
    } else if window_id == 0 {
        Some(inventory.crafting().clone())
    } else {
        None
    };
    let derived = read_menu(&layout, inventory, grid_owner.as_ref(), &own);

    // Did the client end up believing what the server derived? The client's belief is
    // **the pre-click state overwritten by the slots it claimed** — it does not claim
    // slots it thinks are unchanged — plus its claimed cursor.
    //
    // # Compare claims with the full derived menu
    //
    // The client can omit slots it cannot predict, especially a derived crafting
    // result. Compare its claimed slots against the full derived menu so the
    // result and any shifted inputs are corrected in the same response.
    //
    // A matching prediction needs no corrective packet; the no-traffic test
    // exercises that no-traffic branch.
    let cursor = inventory.click_state().carried.clone();
    let mut agrees = cursor.as_ref() == claimed_cursor;
    if agrees {
        let mut believed = before;
        for (menu_slot, claimed) in claimed_slots {
            match usize::try_from(*menu_slot).ok().filter(|i| *i < believed.len()) {
                Some(index) => believed[index] = claimed.clone(),
                // A claim naming a slot this menu does not have is itself a
                // disagreement: it cannot be reconciled, so correct the client.
                None => {
                    agrees = false;
                    break;
                }
            }
        }
        agrees = agrees && believed == derived;
    }
    if agrees {
        return (None, dropped);
    }

    let state_id = match open.as_mut() {
        Some(tracked) => tracked.next_state_id(),
        None => 0,
    };
    (
        Some(proto.encode_container_content(window_id, state_id, &derived, cursor.as_ref())),
        dropped,
    )
}

/// Reads one [`MenuKind::ItemCombiner`] menu's full slot vector — the
/// workstation cells, the player tail, and the live result derived from
/// [`workstation_result`] (never stored; always re-derived, the same
/// "recompute rather than cache" choice `crate::crafting`'s recipe closure
/// makes).
fn read_workstation_menu(
    layout: &MenuLayout,
    inventory: &PlayerInventory,
    cells: &[Option<ItemStack>],
    station: Station,
    creative: bool,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Vec<Option<ItemStack>> {
    let result = workstation_result(
        station,
        cells,
        creative,
        inventory.pending_rename(),
        inventory.selected_recipe_index(),
        hooks,
    );
    layout
        .iter()
        .map(|(_, kind)| match kind {
            SlotKind::Player(native) => inventory.native(native).cloned(),
            SlotKind::Container(_) => None,
            SlotKind::Grid(cell) => cells.get(cell).cloned().flatten(),
            SlotKind::Result => result.clone(),
        })
        .collect()
}

/// One station's result from its own input cells — [`crate::anvil::compute`],
/// [`crate::anvil::grindstone_result`], [`crate::smithing::compute`],
/// [`crate::loom::result`] or [`crate::stonecutting::result`]. `rename` is
/// the anvil's pending typed name ([`PlayerInventory::pending_rename`]);
/// `selected` is the loom/stonecutter's chosen offer index
/// ([`PlayerInventory::selected_recipe_index`]) — every other station
/// ignores whichever of the two it does not use, the same "the other
/// stations ignore it" shape `rename` already had before `selected` existed.
///
/// `hooks` is the plugin seam: the result computed above is
/// the *input* to [`CraftingStationHooks::evaluate`], never the final
/// answer, so a plugin can allow, deny or replace it — see
/// `crate::plugin_crafting`'s own module doc for why this single function is
/// the right choke point.
fn workstation_result(
    station: Station,
    cells: &[Option<ItemStack>],
    creative: bool,
    rename: Option<&str>,
    selected: Option<i32>,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Option<ItemStack> {
    let get = |i: usize| cells.get(i).and_then(Option::as_ref);
    let computed = match station {
        Station::Anvil => crate::anvil::compute(get(0), get(1), rename, creative).result,
        Station::Grindstone => crate::anvil::grindstone_result(get(0), get(1)),
        Station::Smithing => crate::smithing::compute(get(0), get(1), get(2)),
        Station::Loom => crate::loom::result(get(0), get(1), get(2), selected),
        Station::Stonecutter => crate::stonecutting::result(get(0), selected),
    };
    if hooks.is_empty() {
        // The common, zero-plugin case: skip building `StationInputs` (which
        // would otherwise clone every input cell on every menu read) at all.
        return computed;
    }
    let inputs = crate::plugin_crafting::StationInputs {
        station,
        cells: cells.to_vec(),
        computed: computed.clone(),
    };
    hooks.evaluate(&inputs, computed)
}

/// [`apply_container_clicked`]'s `MenuKind::ItemCombiner` branch: the anvil,
/// grindstone and smithing table all share this shape (`docs/workstation-economy.md`),
/// differing only in [`workstation_result`] (what the result slot shows) and
/// [`crate::container_click`]'s own per-station `may_place`/take rules. Kept as
/// a separate function rather than folded into `apply_container_clicked`
/// because the grid source is [`PlayerInventory::workstation`] (a flat cell
/// vector) rather than a [`crate::crafting::CraftingState`], so it cannot reuse
/// `read_menu`.
///
/// **XP is charged here**, not in [`crate::container_click`] — that module is
/// deliberately economy-free (see its own module doc). A take is detected the
/// same way the crafting-table path detects a craft: by comparing the result
/// cell before and after the click, since [`crate::container_click::do_click_with`]
/// already ran the whole click (including any take) by the time this reads
/// `slots` back.
fn apply_workstation_clicked<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    tracked: &mut OpenContainer,
    click: Click,
    claimed_slots: &[(i32, Option<ItemStack>)],
    claimed_cursor: Option<&ItemStack>,
    creative: bool,
    station: Station,
    xp_level: i32,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> (Option<ServerDirective>, Vec<ItemStack>) {
    let layout = MenuLayout::item_combiner(station);
    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let rename = inventory.pending_rename().map(str::to_owned);
    let selected_recipe_index = inventory.selected_recipe_index();
    let mut slots = read_workstation_menu(&layout, inventory, &cells, station, creative, hooks);
    let before = slots.clone();
    let mut state = inventory.click_state().clone();
    let recipe = |grid_cells: &[Option<ItemStack>]| {
        workstation_result(station, grid_cells, creative, rename.as_deref(), selected_recipe_index, hooks)
    };
    // The anvil-menu may-pickup gate: `(creative || experience_level >= cost) && cost > 0`.
    // `cost` is `crate::anvil::compute`'s own field, re-derived
    // from the pre-click cells and pending rename — never stored, the same
    // "recompute rather than cache" choice `workstation_result` above already
    // makes. `Grindstone`/`Smithing` pass `None`: neither result slot changes
    // this permission, so both retain the default allow-pickup behavior.
    let anvil_cost = crate::anvil::compute(
        cells.first().and_then(Option::as_ref),
        cells.get(1).and_then(Option::as_ref),
        rename.as_deref(),
        creative,
    )
    .cost;
    let anvil_may_pickup = move |_index: usize, _item: &ItemStack| (creative || xp_level >= anvil_cost) && anvil_cost > 0;
    let may_pickup: Option<MayPickup<'_>> = (station == Station::Anvil).then_some(&anvil_may_pickup as _);
    let dropped = do_click_with(
        &layout, &mut slots, &mut state, click, creative, Some(&recipe), may_pickup, None,
    );
    *inventory.click_state_mut() = state;

    let mut new_cells = cells.clone();
    for (index, kind) in layout.iter() {
        match kind {
            SlotKind::Player(native) => inventory.set_native(native, slots[index].clone()),
            SlotKind::Grid(cell) => {
                if let Some(slot) = new_cells.get_mut(cell) {
                    *slot = slots[index].clone();
                }
            }
            SlotKind::Container(_) | SlotKind::Result => {}
        }
    }
    // `container_click::take_result`'s own anvil branch re-derives the
    // outcome with `item_name: None` (that module is deliberately
    // economy/rename-free) purely to read `only_renaming`/
    // `repair_item_count_cost`, which is safe for every case except a take
    // priced *entirely* by a pending rename: seen with no name, that
    // evaluation returns `price <= 0` and takes the "nothing to combine"
    // early exit, so `only_renaming` comes back `false` and the addition
    // cell is wrongly cleared as if a real combine had consumed it. Correct
    // it here, where the real rename text is available — a no-op unless
    // this exact click just took such a result (cell 0 went from occupied to
    // empty).
    if station == Station::Anvil {
        let had_input = cells.first().cloned().flatten();
        let took_input = new_cells.first().is_some_and(Option::is_none);
        if let (Some(input), true) = (had_input, took_input) {
            let addition = cells.get(1).cloned().flatten();
            if let Some(addition_item) = addition.clone() {
                let outcome = crate::anvil::compute(Some(&input), Some(&addition_item), rename.as_deref(), creative);
                if outcome.result.is_some() && outcome.only_renaming && outcome.repair_item_count_cost == 0 {
                    if let Some(slot) = new_cells.get_mut(1) {
                        *slot = addition;
                    }
                }
            }
        }
    }
    if let Some(ws) = inventory.workstation_mut() {
        *ws = new_cells.clone();
    }

    let derived = read_workstation_menu(&layout, inventory, &new_cells, station, creative, hooks);

    let cursor = inventory.click_state().carried.clone();
    let mut agrees = cursor.as_ref() == claimed_cursor;
    if agrees {
        let mut believed = before;
        for (menu_slot, claimed) in claimed_slots {
            match usize::try_from(*menu_slot).ok().filter(|i| *i < believed.len()) {
                Some(index) => believed[index] = claimed.clone(),
                None => {
                    agrees = false;
                    break;
                }
            }
        }
        agrees = agrees && believed == derived;
    }
    if agrees {
        return (None, dropped);
    }
    let state_id = tracked.next_state_id();
    (
        Some(proto.encode_container_content(tracked.window_id, state_id, &derived, cursor.as_ref())),
        dropped,
    )
}

/// [`apply_container_clicked`]'s `MenuKind::Enchanting` branch. No result slot
/// and no take, so there is no economy to charge here at all — see
/// `crate::enchanting`'s own module doc for why the "choose an offer" action
/// (`ClientAction::ContainerButtonClick`) cannot reach this crate yet. This
/// only has to keep the two cells (item, lapis) in sync with clicks; the three
/// `container_set_data` costs are **not** recomputed live here — see
/// `docs/workstation-economy.md` for that scope note.
fn apply_enchanting_clicked<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    tracked: &mut OpenContainer,
    click: Click,
    claimed_slots: &[(i32, Option<ItemStack>)],
    claimed_cursor: Option<&ItemStack>,
    creative: bool,
) -> (Option<ServerDirective>, Vec<ItemStack>) {
    let layout = MenuLayout::enchanting_table();
    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let read = |inv: &PlayerInventory, cells: &[Option<ItemStack>]| -> Vec<Option<ItemStack>> {
        layout
            .iter()
            .map(|(_, kind)| match kind {
                SlotKind::Player(native) => inv.native(native).cloned(),
                SlotKind::Grid(cell) => cells.get(cell).cloned().flatten(),
                SlotKind::Container(_) | SlotKind::Result => None,
            })
            .collect()
    };
    let mut slots = read(inventory, &cells);
    let before = slots.clone();
    let mut state = inventory.click_state().clone();
    let dropped = do_click_with(&layout, &mut slots, &mut state, click, creative, None, None, None);
    *inventory.click_state_mut() = state;

    let mut new_cells = cells;
    for (index, kind) in layout.iter() {
        match kind {
            SlotKind::Player(native) => inventory.set_native(native, slots[index].clone()),
            SlotKind::Grid(cell) => {
                if let Some(slot) = new_cells.get_mut(cell) {
                    *slot = slots[index].clone();
                }
            }
            SlotKind::Container(_) | SlotKind::Result => {}
        }
    }
    if let Some(ws) = inventory.workstation_mut() {
        *ws = new_cells.clone();
    }
    let derived = read(inventory, &new_cells);

    let cursor = inventory.click_state().carried.clone();
    let mut agrees = cursor.as_ref() == claimed_cursor;
    if agrees {
        let mut believed = before;
        for (menu_slot, claimed) in claimed_slots {
            match usize::try_from(*menu_slot).ok().filter(|i| *i < believed.len()) {
                Some(index) => believed[index] = claimed.clone(),
                None => {
                    agrees = false;
                    break;
                }
            }
        }
        agrees = agrees && believed == derived;
    }
    if agrees {
        return (None, dropped);
    }
    let state_id = tracked.next_state_id();
    (
        Some(proto.encode_container_content(tracked.window_id, state_id, &derived, cursor.as_ref())),
        dropped,
    )
}

/// [`ServerBound::RenameItem`]'s consumer — vanilla's own anvil-menu
/// item-name setter, reached
/// the same way its own rename-item handler gates it:
/// only when an anvil is currently open (no `window_id` on the wire to check
/// further — the real packet does not carry one either).
///
/// Returns the directives to resend (the refreshed content, then the
/// `cost` data slot — vanilla's own anvil-menu single `DataSlot`) once the rename
/// actually changed something; `Vec::new()` for a rejected/no-op rename or
/// when no anvil is open, matching `setItemName`'s own `validatedName !=
/// this.itemName` early return.
fn apply_rename_item<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    tracked: Option<&mut OpenContainer>,
    name: &str,
    creative: bool,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Vec<ServerDirective> {
    let Some(tracked) = tracked else { return Vec::new() };
    if !matches!(tracked.shape, MenuKind::ItemCombiner { station: Station::Anvil, .. }) {
        return Vec::new();
    }
    let Some(validated) = crate::anvil::validate_rename(name) else {
        return Vec::new();
    };
    if inventory.pending_rename() == Some(validated.as_str()) {
        return Vec::new();
    }
    inventory.set_pending_rename(Some(validated));

    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let outcome = crate::anvil::compute(
        cells.first().and_then(Option::as_ref),
        cells.get(1).and_then(Option::as_ref),
        inventory.pending_rename(),
        creative,
    );
    let layout = MenuLayout::item_combiner(Station::Anvil);
    let items = read_workstation_menu(&layout, inventory, &cells, Station::Anvil, creative, hooks);
    let state_id = tracked.next_state_id();
    vec![
        proto.encode_container_content(tracked.window_id, state_id, &items, inventory.click_state().carried.as_ref()),
        // The "see the 1-XP rename cost" half `docs/workstation-economy.md`
        // named as the actually-missing piece.
        proto.encode_container_data(tracked.window_id, 0, outcome.cost),
    ]
}

/// [`ServerBound::EditBook`]'s consumer. Only hotbar and off-hand slots are
/// accepted, and the selected item must be a `minecraft:writable_book` carrying
/// its writable-book content marker. The decoded page and title limits are
/// enforced by the protocol layer.
///
/// Returns the native slot written and the replacement item for a
/// `CONTAINER_SET_SLOT` update. Returns `None` when validation fails.
fn apply_edit_book(
    inventory: &mut PlayerInventory,
    slot: i32,
    pages: Vec<String>,
    title: Option<String>,
    author: &str,
) -> Option<(usize, ItemStack)> {
    let native = usize::try_from(slot).ok()?;
    if !(native < usize::from(HOTBAR_SIZE) || native == OFFHAND_NATIVE) {
        return None;
    }
    let mut item = inventory.native(native)?.clone();
    if item.item.path() != "writable_book" {
        return None;
    }
    match title {
        // A submitted title converts the draft to a written book with
        // generation `0` and resolved text.
        Some(title) => {
            item.item = "minecraft:written_book".parse().ok()?;
            item.components.writable_book_content = None;
            item.components.written_book_content = Some(WrittenBookContent {
                title,
                author: author.to_owned(),
                generation: 0,
                pages: pages.into_iter().map(Text::literal).collect(),
                resolved: true,
            });
        }
        // Without a title, replace the draft pages in place.
        None => {
            item.components.writable_book_content = Some(pages);
        }
    }
    inventory.set_native(native, Some(item.clone()));
    Some((native, item))
}

/// [`ServerBound::SetBeacon`]'s consumer — vanilla's own beacon-menu
/// update-effects routine, reached the same way its own set-beacon-packet handler gates it: only while a
/// beacon is currently open (vanilla's own `containerMenu instanceof
/// BeaconMenu` check).
///
/// `levels` is **not** re-derived here — vanilla's own beacon-menu levels getter reads the
/// block entity's own tracked field, last refreshed when the menu opened
/// (see `BeaconData::levels`'s own doc), the same snapshot vanilla's real
/// `ContainerData` would hold between its own 80-tick background
/// recomputes.
///
/// Returns the directives to resend (the refreshed payment slot, then all
/// three data values) once the submission actually changed something, or
/// `Vec::new()` for a refused one — no payment item, or
/// `crate::beacon::validate_beacon_effects` refuses the pair. Vanilla
/// disconnects the client on a refusal (`handleSetBeaconPacket`'s own
/// `this.disconnect(...)`); this crate instead treats it as a malformed
/// packet whose effect is dropped rather than the connection, the same
/// convention `PlayerInventory::set_selected_hotbar_slot`'s own doc already
/// states for an out-of-range packet field.
fn apply_set_beacon<P: ServerProtocol>(
    proto: &P,
    block_entities: &BlockEntityHandle,
    tracked: Option<&mut OpenContainer>,
    primary: Option<String>,
    secondary: Option<String>,
) -> Vec<ServerDirective> {
    let Some(tracked) = tracked else { return Vec::new() };
    if tracked.shape != MenuKind::Beacon {
        return Vec::new();
    }
    let primary = match primary {
        Some(key) => match crate::beacon::BeaconPower::from_key(&key) {
            Some(power) => Some(power),
            None => return Vec::new(),
        },
        None => None,
    };
    let secondary = match secondary {
        Some(key) => match crate::beacon::BeaconPower::from_key(&key) {
            Some(power) => Some(power),
            None => return Vec::new(),
        },
        None => None,
    };
    let pos = tracked.pos;
    let updated = block_entities.with(|reg| {
        let Some(BlockEntity::Beacon(beacon)) = reg.get_mut(pos) else {
            return None;
        };
        beacon.payment.as_ref()?;
        if !crate::beacon::validate_beacon_effects(primary, secondary, beacon.levels) {
            return None;
        }
        beacon.primary_effect = primary;
        beacon.secondary_effect = secondary;
        // Remove one item from the payment slot.
        let consumed_all = beacon.payment.as_ref().is_some_and(|item| item.count <= 1);
        if consumed_all {
            beacon.payment = None;
        } else if let Some(payment) = &mut beacon.payment {
            payment.count -= 1;
        }
        Some((
            beacon.levels,
            beacon.primary_effect.clone(),
            beacon.secondary_effect.clone(),
            beacon.payment.clone(),
        ))
    });
    let Some((levels, primary, secondary, payment)) = updated else {
        return Vec::new();
    };
    let state_id = tracked.next_state_id();
    vec![
        proto.encode_container_slot(tracked.window_id, state_id, 0, payment.as_ref()),
        proto.encode_container_data(tracked.window_id, 0, i32::from(levels)),
        proto.encode_container_data(
            tracked.window_id,
            1,
            crate::beacon::encode_beacon_effect(primary),
        ),
        proto.encode_container_data(
            tracked.window_id,
            2,
            crate::beacon::encode_beacon_effect(secondary),
        ),
    ]
}

/// Applies one lectern reader button against the block entity that owns the
/// open window. Page changes are data-only updates (the book remains in slot
/// zero); taking the book clears the authoritative slot and resets the page,
/// then adds the exact stack to the player's inventory. A full inventory
/// refuses the take, so the server never loses a book it cannot deliver.
fn apply_lectern_button_click<P: ServerProtocol>(
    proto: &P,
    block_entities: &BlockEntityHandle,
    inventory: &mut PlayerInventory,
    tracked: Option<&mut OpenContainer>,
    window_id: i32,
    button_id: i32,
    can_take: bool,
) -> Vec<ServerDirective> {
    let Some(tracked) = tracked else { return Vec::new() };
    if tracked.window_id != window_id || tracked.shape != MenuKind::Lectern {
        return Vec::new();
    }
    let pos = tracked.pos;
    let Some((book, current_page)) = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Lectern(lectern)) => lectern.book.clone().map(|book| (book, lectern.page)),
        _ => None,
    }) else {
        return Vec::new();
    };
    let page_count = book
        .components
        .written_book_content
        .as_ref()
        .map_or(0, |content| content.pages.len())
        .max(book.components.writable_book_content.as_ref().map_or(0, Vec::len));
    let page_count = page_count.max(1);
    let current_page = current_page
        .max(0)
        .min(i32::try_from(page_count - 1).unwrap_or(i32::MAX));

    let next_page = match button_id {
        1 => current_page.saturating_sub(1),
        2 => current_page.saturating_add(1).min(i32::try_from(page_count - 1).unwrap_or(i32::MAX)),
        id if id >= 100 => (id - 100).max(0).min(i32::try_from(page_count - 1).unwrap_or(i32::MAX)),
        3 => {
            if !can_take {
                return Vec::new();
            }
            // The lectern holds one book. `add` therefore either accepts it
            // in full or returns it untouched; restore the authoritative slot
            // if an inventory cannot accept it.
            let before_inventory = inventory.clone();
            if inventory.add(book.clone()).1.is_some() {
                return Vec::new();
            }
            let changed = block_entities.with(|reg| {
                let Some(BlockEntity::Lectern(lectern)) = reg.get_mut(pos) else {
                    return false;
                };
                lectern.book = None;
                lectern.page = 0;
                true
            });
            if !changed {
                // This should only be reachable if another world operation
                // removed the block entity between the snapshot and write.
                // Put the book back rather than deleting the player's item.
                let _ = inventory.take_matching(|item| item == &book);
                return Vec::new();
            }
            let mut directives = Vec::new();
            // The book can land in any player-storage slot, not necessarily
            // the selected hotbar slot. Publish each changed native slot in
            // window-0 menu coordinates so the visible inventory agrees with
            // the server immediately after the lectern action.
            for native in 0..crate::inventory::PLAYER_NATIVE_SIZE {
                if before_inventory.native(native) != inventory.native(native)
                    && let Some(menu_slot) = window_zero_menu_slot(native)
                {
                    directives.push(proto.encode_container_slot(
                        0,
                        0,
                        menu_slot,
                        inventory.native(native),
                    ));
                }
            }
            let state_id = tracked.next_state_id();
            directives.extend([
                proto.encode_container_slot(tracked.window_id, state_id, 0, None),
                proto.encode_container_data(tracked.window_id, 0, 0),
            ]);
            return directives;
        }
        _ => return Vec::new(),
    };
    if next_page == current_page {
        return Vec::new();
    }
    block_entities.with(|reg| {
        if let Some(BlockEntity::Lectern(lectern)) = reg.get_mut(pos) {
            lectern.page = next_page;
        }
    });
    vec![proto.encode_container_data(tracked.window_id, 0, next_page)]
}

/// [`ServerBound::ContainerButtonClick`]'s consumer —
/// vanilla's own enchantment-menu click-menu-button routine. `slot` (`button_id`, `0..3`) selects
/// which of the three offers; the lapis price is `slot + 1` and the XP price
/// is that slot's own [`crate::enchanting::table_costs`] entry, both
/// re-derived here rather than trusted from the client.
///
/// `fresh_seed` is a pre-drawn `[0, i32::MAX)` roll from the caller's own
/// `SpawnRng` — the same "pre-drawn value" shape `apply_use_item_on`'s
/// composter `roll` already uses — only consumed when the enchant actually
/// succeeds, matching vanilla's own on-enchantment-performed routine's own reroll.
///
/// Returns the directives to send (the XP update, if any levels were spent,
/// then the refreshed menu content) or `Vec::new()` when the click is
/// refused: wrong window, no item, no offer at that cost, insufficient
/// lapis/levels, or a roll that produced no enchantment.
fn apply_container_button_click<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    tracked: Option<&mut OpenContainer>,
    window_id: i32,
    button_id: i32,
    source: &dyn ChunkSource,
    experience: &mut crate::experience::PlayerExperience,
    creative: bool,
    fresh_seed: i64,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Vec<ServerDirective> {
    let Some(tracked) = tracked else { return Vec::new() };
    if tracked.window_id != window_id {
        return Vec::new();
    }
    // Loom and stonecutter share this packet type but use different shapes and
    // pricing from the enchanting table. They select an offer without lapis or
    // experience cost; see `apply_workstation_button_click`.
    if let MenuKind::ItemCombiner { station: station @ (Station::Loom | Station::Stonecutter), .. } = tracked.shape {
        return apply_workstation_button_click(proto, inventory, tracked, station, button_id, creative, hooks);
    }
    if tracked.shape != MenuKind::Enchanting {
        return Vec::new();
    }
    let Some(slot) = usize::try_from(button_id).ok().filter(|&s| s < 3) else {
        return Vec::new();
    };
    let pos = tracked.pos;
    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let Some(item) = cells.first().cloned().flatten() else {
        return Vec::new();
    };
    let lapis = cells.get(1).cloned().flatten();

    let seed = inventory.enchant_seed();
    let bookcases = crate::enchanting::bookshelf_power(source, pos);
    let costs = crate::enchanting::table_costs(seed, bookcases, &item);
    let cost = costs[slot];
    let lapis_cost = i32::try_from(slot).unwrap_or(0) + 1;
    let has_lapis = creative || lapis.as_ref().is_some_and(|l| i32::try_from(l.count).unwrap_or(0) >= lapis_cost);
    let affordable = creative || (experience.level() >= lapis_cost && experience.level() >= cost);
    if cost <= 0 || !has_lapis || !affordable {
        return Vec::new();
    }

    // Vanilla's own enchantment-menu enchantment-list getter: reseeded per slot so each of the
    // three offers is an independent draw off the same base seed.
    let mut rng = SpawnRng::new(seed.wrapping_add(slot as i64) as u64);
    let offers = crate::enchanting::select_enchantments(&mut rng, &item, cost);
    if offers.is_empty() {
        return Vec::new();
    }

    let mut enchanted = item;
    if enchanted.item.to_string() == "minecraft:book" {
        enchanted.item = "minecraft:enchanted_book".parse().expect("valid key");
    }
    for offer in &offers {
        crate::anvil::apply_enchantment(&mut enchanted, offer.key, offer.level);
    }
    if !creative {
        experience.take_levels(cost);
    }
    let new_lapis = if creative {
        lapis
    } else {
        lapis.and_then(|l| {
            let remaining = l.count.saturating_sub(u32::try_from(lapis_cost).unwrap_or(0));
            (remaining > 0).then(|| {
                let mut shrunk = l;
                shrunk.count = remaining;
                shrunk
            })
        })
    };
    if let Some(ws) = inventory.workstation_mut() {
        if let Some(slot0) = ws.get_mut(0) {
            *slot0 = Some(enchanted);
        }
        if let Some(slot1) = ws.get_mut(1) {
            *slot1 = new_lapis;
        }
    }
    inventory.set_enchant_seed(fresh_seed);

    let mut directives = Vec::new();
    if !creative {
        directives.push(proto.encode_set_experience(experience.progress(), experience.level(), experience.total()));
    }
    let layout = MenuLayout::enchanting_table();
    let new_cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let items: Vec<Option<ItemStack>> = layout
        .iter()
        .map(|(_, kind)| match kind {
            SlotKind::Player(native) => inventory.native(native).cloned(),
            SlotKind::Grid(cell) => new_cells.get(cell).cloned().flatten(),
            SlotKind::Container(_) | SlotKind::Result => None,
        })
        .collect();
    let state_id = tracked.next_state_id();
    directives.push(proto.encode_container_content(tracked.window_id, state_id, &items, inventory.click_state().carried.as_ref()));
    directives
}

/// [`apply_container_button_click`]'s loom/stonecutter branch —
/// vanilla's own loom-menu/stonecutter-menu click-menu-button routines. Both just
/// pick which offer [`workstation_result`] shows next; neither has a lapis
/// or XP cost (contrast the enchanting table above), so this only ever needs
/// to validate the index and resend the menu.
///
/// `station == Station::Stonecutter`'s own reselect guard
/// (its own stonecutter-menu click-menu-button routine's `if (selectedRecipeIndex.get() ==
/// buttonId) return false;`) is reproduced; its own loom-menu click-menu-button routine has no
/// such guard and re-applies unconditionally when the index is valid.
fn apply_workstation_button_click<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    tracked: &mut OpenContainer,
    station: Station,
    button_id: i32,
    creative: bool,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Vec<ServerDirective> {
    if station == Station::Stonecutter && inventory.selected_recipe_index() == Some(button_id) {
        return Vec::new();
    }
    let layout = MenuLayout::item_combiner(station);
    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let get = |i: usize| cells.get(i).and_then(Option::as_ref);
    let offer_count = match station {
        Station::Loom => crate::loom::selectable_pattern_count(get(2)),
        Station::Stonecutter => crate::stonecutting::count(get(0)),
        Station::Anvil | Station::Grindstone | Station::Smithing => 0,
    };
    if usize::try_from(button_id).is_ok_and(|index| index < offer_count) {
        inventory.set_selected_recipe_index(Some(button_id));
    }
    let items = read_workstation_menu(&layout, inventory, &cells, station, creative, hooks);
    let state_id = tracked.next_state_id();
    vec![proto.encode_container_content(tracked.window_id, state_id, &items, inventory.click_state().carried.as_ref())]
}

/// Lays a recipe-book recipe out in the open crafting grid (the `PLACE_RECIPE` consumer).
///
/// Which grid depends on the window: `0` is the player screen's 2×2, an open
/// crafting table is its 3×3. A 3×3 recipe asked for on the 2×2 screen has no
/// placement and is refused — [`crate::crafting::place_recipe`] returns `false` and
/// nothing moves, which is vanilla's behaviour too.
///
/// Returns the full `container_set_content` the client needs, because a fill moves
/// items out of arbitrary inventory slots and there is no diff to send.
fn apply_recipe_placed<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    open_container: Option<&mut OpenContainer>,
    window_id: i32,
    recipe_index: i32,
    use_max_items: bool,
) -> Option<ServerDirective> {
    let index = usize::try_from(recipe_index).ok()?;
    let (_, recipe) = crate::crafting::recipe_at_index(index)?;

    let mut open = open_container;
    let (layout, uses_table_grid) = if window_id == 0 {
        (MenuLayout::player(), false)
    } else {
        let tracked = open.as_mut()?;
        if tracked.window_id != window_id || tracked.shape != MenuKind::CraftingTable {
            return None;
        }
        (MenuLayout::crafting_table(), true)
    };

    // The grid is moved out and back so `place_recipe` can hold `&mut` on both it
    // and the inventory — they are two fields of the same struct.
    let mut grid = if uses_table_grid {
        inventory.table_crafting()?.clone()
    } else {
        inventory.crafting().clone()
    };
    if !crate::crafting::place_recipe(inventory, &mut grid, recipe, use_max_items) {
        return None;
    }
    if uses_table_grid {
        *inventory.table_crafting_mut()? = grid;
    } else {
        *inventory.crafting_mut() = grid;
    }

    let grid_owner = if uses_table_grid {
        inventory.table_crafting().cloned()
    } else {
        Some(inventory.crafting().clone())
    };
    let items = read_menu(&layout, inventory, grid_owner.as_ref(), &[]);
    let state_id = match open.as_mut() {
        Some(tracked) => tracked.next_state_id(),
        None => 0,
    };
    Some(proto.encode_container_content(
        window_id,
        state_id,
        &items,
        inventory.click_state().carried.as_ref(),
    ))
}

/// Spawns stacks that left a menu into the world as item entities — vanilla's
/// `player.drop(stack, true)`, which [`crate::container_click::do_click`] has no
/// world to make itself.
///
/// A connection with no tracked position yet drops nothing rather than spawning at
/// the origin, the same "no data yet, don't guess" gate [`apply_attack`] uses.
fn spawn_dropped_stacks(
    mobs: &MobHandle,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    rng: &mut SpawnRng,
    dropped: Vec<ItemStack>,
) {
    if dropped.is_empty() {
        return;
    }
    let Some((x, y, z)) = player_pos else { return };
    // Container throws use the hand position and forward impulse derived from
    // `player_rot`, matching the Q-key drop behavior. The pickup delay keeps a
    // thrown stack from being collected by the player immediately.
    let rotation = player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 });
    let position = Vec3::new(x, y + EYE_HEIGHT - crate::block_drops::THROW_HAND_DROP, z);
    mobs.with(|sim| {
        for stack in dropped {
            let count = u8::try_from(stack.count).unwrap_or(u8::MAX);
            // A fresh draw per stack, as vanilla does: `doClick`'s outside case
            // can throw several stacks in one click and each gets its own spread.
            let velocity =
                crate::block_drops::thrown_item_velocity(rotation.yaw, rotation.pitch, rng);
            sim.spawn_item(
                stack.item.clone(),
                position,
                velocity,
                ItemLifecycle {
                    pickup_delay: crate::block_drops::THROWN_PICKUP_DELAY_TICKS,
                    ..ItemLifecycle::newly_dropped(count, DEFAULT_MAX_STACK_SIZE)
                },
            );
        }
    });
}

/// Throws the selected hotbar stack into the world — `Q` (`whole_stack: false`,
/// one item) or `Ctrl+Q` (`whole_stack: true`, all of it).
///
/// The operation has three steps: remove items from the selected slot, record
/// the slot's *new* contents, and spawn the entity with
/// [`crate::block_drops::thrown_item_velocity`].
///
/// # The slot update
///
/// **The client receives no drop acknowledgement.** The server records the
/// selected slot's contents but does not send that bookkeeping as a packet;
/// no separate slot acknowledgement is required, which **suppresses** the
/// corrective broadcast that would otherwise follow. That
/// works because the client predicts the drop itself (`lodestone-client`'s
/// `drop_selected` does, and its doc records that an unpredicted drop leaves the
/// count permanently wrong — the item really is gone server-side).
///
/// A rejected drop sends one `container_set_slot` carrying the authoritative
/// content, while an accepted drop remains inert in the common case because it
/// equals what the client predicted.
/// A no-op drop returns `None` and sends nothing, because the client predicted no
/// change either.
///
/// Returns the directive to send and the stacks to spawn; the caller owns both
/// because it holds the `Connection` and the [`MobHandle`].
fn apply_item_dropped<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    open_container: Option<&mut OpenContainer>,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    whole_stack: bool,
    rng: &mut SpawnRng,
    mobs: &MobHandle,
) -> Option<ServerDirective> {
    let native = usize::from(inventory.selected_hotbar_slot());
    let held = inventory.native(native)?.clone();
    if held.count <= 0 {
        return None;
    }
    // Remove the whole selected stack for a full-stack throw, or one item.
    let taken = if whole_stack { held.count } else { 1 };
    let mut thrown = held.clone();
    thrown.count = taken;
    let remaining = held.count - taken;
    inventory.set_native(
        native,
        (remaining > 0).then(|| {
            let mut rest = held.clone();
            rest.count = remaining;
            rest
        }),
    );

    // Spawned before the reply is built so a panic here cannot leave the client
    // told about an inventory change that produced no entity.
    if let Some((x, y, z)) = player_pos {
        let rotation = player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 });
        let velocity =
            crate::block_drops::thrown_item_velocity(rotation.yaw, rotation.pitch, rng);
        let position = Vec3::new(
            x,
            y + EYE_HEIGHT - crate::block_drops::THROW_HAND_DROP,
            z,
        );
        let count = u8::try_from(thrown.count).unwrap_or(u8::MAX);
        mobs.with(|sim| {
            sim.spawn_item(
                thrown.item.clone(),
                position,
                velocity,
                ItemLifecycle {
                    // 40, not `newly_dropped`'s 10: a player walking forwards
                    // would otherwise pick their own throw straight back up.
                    pickup_delay: crate::block_drops::THROWN_PICKUP_DELAY_TICKS,
                    ..ItemLifecycle::newly_dropped(count, DEFAULT_MAX_STACK_SIZE)
                },
            );
        });
    }

    // The hotbar's menu slot in whichever window is open. The player screen and
    // the crafting table both put native hotbar slot `n` at menu slot
    // `hotbar_start + n`; asking the layout rather than hardcoding 36 is what
    // keeps this right for a container whose payload half is a different size.
    let (layout, window_id, state_id) = match open_container {
        Some(tracked) => {
            let layout = match tracked.shape {
                MenuKind::CraftingTable => MenuLayout::crafting_table(),
                _ => MenuLayout::container(tracked.container_size),
            };
            let window_id = tracked.window_id;
            (layout, window_id, tracked.next_state_id())
        }
        None => (MenuLayout::player(), 0, 0),
    };
    // Asked of the layout rather than hardcoded as `36 + native`: a container
    // window's own slots come *first*, so the hotbar's menu index depends on the
    // container's size. The player screen is the only layout where it is 36.
    let menu_slot = layout
        .iter()
        .find(|&(_, kind)| kind == SlotKind::Player(native))
        .and_then(|(index, _)| i32::try_from(index).ok())?;
    Some(proto.encode_container_slot(
        window_id,
        state_id,
        menu_slot,
        inventory.native(native),
    ))
}

/// One in-progress bow draw: which tick it started on, and the facing the
/// `USE_ITEM` reported.
///
/// The facing is captured at the *start* and used as a fallback only. Vanilla
/// shoots along the player's facing at **release**, which `player_rot` supplies if
/// the client has ever sent angles — so this field only matters for a connection
/// that draws and releases without having sent a single rotation packet, where the
/// alternative would be firing due south.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BowDraw {
    /// The `MobSim::tick_count` the draw began on.
    started_tick: u64,
    /// The facing the `USE_ITEM` packet carried.
    yaw: f32,
    /// The pitch the `USE_ITEM` packet carried.
    pitch: f32,
}

/// The item a `USE_ITEM` is asking to launch, and how.
///
/// A closed enum rather than a string match at the call site, because the two
/// behaviours are genuinely different shapes: a throwable resolves entirely inside
/// the `USE_ITEM` arm, and a bow resolves in a *later* packet.
#[derive(Debug, Clone, Copy, PartialEq)]
enum LaunchIntent {
    /// Thrown the instant the packet arrives, at
    /// [`THROWABLE_SHOOT_POWER`](lodestone_entity::projectile::THROWABLE_SHOOT_POWER):
    /// snowball, egg, ender pearl. The projectile entity is the item's own name.
    InstantThrow {
        /// The projectile entity type to spawn.
        projectile: &'static str,
        /// Launch speed in blocks per tick.
        power: f64,
        /// Vanilla's `yOffset`, non-zero only for a potion.
        pitch_offset: f64,
    },
    /// Starts a draw the release packet finishes.
    BeginDraw,
}

/// What the item in `path` does on a right-click in mid-air.
///
/// Only the items that actually launch something are listed. Everything else —
/// food, blocks, a bucket — is `None`, and the `USE_ITEM` arm does nothing, which
/// is correct rather than unimplemented for a crate with no eating or placement
/// model on this packet.
fn launch_intent(path: &str) -> Option<LaunchIntent> {
    use lodestone_entity::projectile::{
        POTION_PITCH_OFFSET, POTION_SHOOT_POWER, THROWABLE_SHOOT_POWER,
    };
    let throw = |projectile| {
        Some(LaunchIntent::InstantThrow {
            projectile,
            power: THROWABLE_SHOOT_POWER,
            pitch_offset: 0.0,
        })
    };
    match path {
        "snowball" => throw("snowball"),
        "egg" => throw("egg"),
        "ender_pearl" => throw("ender_pearl"),
        "experience_bottle" => throw("experience_bottle"),
        // `ThrowablePotionItem`: slower, and the only one with a pitch offset.
        "splash_potion" => Some(LaunchIntent::InstantThrow {
            projectile: "splash_potion",
            power: POTION_SHOOT_POWER,
            pitch_offset: POTION_PITCH_OFFSET,
        }),
        "lingering_potion" => Some(LaunchIntent::InstantThrow {
            projectile: "lingering_potion",
            power: POTION_SHOOT_POWER,
            pitch_offset: POTION_PITCH_OFFSET,
        }),
        // A crossbow's charge/hold semantics are genuinely different (it stores a
        // loaded projectile in a component and fires on the *next* use), and there
        // is no charged-projectiles component model here, so it is deliberately not
        // folded in with the bow — a shared arm would fire it like a bow, which is
        // wrong in a way that looks right.
        "bow" => Some(LaunchIntent::BeginDraw),
        _ => None,
    }
}

/// The ammunition a drawn bow consumes, and whether the inventory has any.
///
/// The ammunition search matches the weapon's ammo predicate; this crate models
/// the plain arrow only, which is the ammunition a standard bow finds first.
const BOW_AMMUNITION: &str = "arrow";

/// One consume (eat or drink) in progress on a connection.
///
/// The item-use state records the two facts completion needs: which slot is
/// being eaten from, and when it
/// finishes. `item` is carried so a slot whose contents changed mid-bite (a
/// container click, a hotbar swap) cannot complete as if it were still the food
/// The same "re-check what you recorded" guard `PendingBreak` applies to a dig.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ItemInUse {
    /// Native inventory index the food is in.
    native: usize,
    /// The item that started the use, full registry name.
    item: String,
    /// The `MobSim` tick the use completes on — `started` plus the item's consume-ticks value.
    finish_tick: u64,
    /// The `remaining` value the last periodic consume sound was published for.
    ///
    /// The consumable emit-particles-and-sounds predicate is
    /// `remaining % 4 == 0`, which is correct **only if it is evaluated exactly once
    /// per tick**. The loop that drives it reads `MobSim`'s counter from a 50 ms
    /// timer arm, and the two clocks are not the same object: if the timer fires
    /// twice inside one mob tick, the same `remaining` passes the predicate again and
    /// the eating sound doubles. Latching the value it last fired for makes the
    /// emission idempotent per tick without assuming the clocks agree.
    last_effect_remaining: Option<u32>,
}

/// What a `USE_ITEM` started. This is the subset of item-use outcomes with a
/// consequence here.
#[derive(Debug)]
enum UseItemOutcome {
    /// Nothing this crate models.
    Nothing,
    /// A bow draw opened; the `RELEASE_USE_ITEM` that follows ends it.
    Draw(BowDraw),
    /// A consume opened; the server's own clock ends it.
    Consuming(ItemInUse),
    /// An equip swap already happened; this arm is instantaneous.
    Equipped(crate::item_use::EquipSwap),
}

/// Where an eye of ender is launched, as a fraction of the player's standing
/// height above their feet.
const EYE_LAUNCH_HEIGHT_FRACTION: f64 = 0.5;

/// The standing player's collision height, which the launch height is a
/// fraction of.
const PLAYER_STANDING_HEIGHT: f64 = 1.8;

/// Throws the held eye of ender at the nearest stronghold, if there is one.
///
/// Returns the launch sound when an eye was thrown and `None` when nothing
/// happened, in which case the stack is untouched. Nothing happens in a
/// dimension other than the overworld, when `locate` finds no stronghold, when
/// the player is looking at an end portal frame (that click belongs to the
/// frame-filling arm), or when the stack is empty.
///
/// `sound_roll` is a uniform `[0, 1)` draw that sets the launch sound's pitch
/// between 0.33 and 0.5.
#[allow(clippy::too_many_arguments)]
fn launch_eye_of_ender(
    mobs: &MobHandle,
    inventory: &mut PlayerInventory,
    native: usize,
    game_mode: GameMode,
    feet: Vec3,
    yaw: f32,
    pitch: f32,
    dimension: crate::dimension::Dimension,
    block_state: &dyn Fn(i32, i32, i32) -> StateId,
    locate: &dyn Fn(BlockPos) -> Option<BlockPos>,
    sound_roll: f32,
) -> Option<crate::effects::WorldEffect> {
    if dimension != crate::dimension::Dimension::Overworld
        || inventory
            .native(native)
            .is_none_or(|stack| stack.item.path() != "ender_eye")
    {
        return None;
    }
    let eye = Vec3::new(feet.x, feet.y + EYE_HEIGHT, feet.z);
    let reach = crate::boat::block_interaction_range(game_mode == GameMode::Creative);
    let view = crate::boat::view_direction(yaw, pitch);
    let end = Vec3::new(eye.x + view.x * reach, eye.y + view.y * reach, eye.z + view.z * reach);
    if let Some(hit) = crate::boat::clip(eye, end, block_state)
        && crate::portal::is_end_portal_frame(block_state(hit.cell.x, hit.cell.y, hit.cell.z))
    {
        return None;
    }
    let target = locate(BlockPos::new(
        feet.x.floor() as i32,
        feet.y.floor() as i32,
        feet.z.floor() as i32,
    ))?;
    if !consume_one(inventory, native, game_mode) {
        return None;
    }
    let launch = Vec3::new(
        feet.x,
        feet.y + PLAYER_STANDING_HEIGHT * EYE_LAUNCH_HEIGHT_FRACTION,
        feet.z,
    );
    mobs.with(|sim| {
        sim.spawn_eye_of_ender(
            launch,
            Vec3::new(f64::from(target.x), f64::from(target.y), f64::from(target.z)),
        )
    });
    Some(crate::effects::WorldEffect::Sound {
        sound: "minecraft:entity.ender_eye.launch".to_owned(),
        category: lodestone_model::SoundCategory::Neutral,
        pos: feet,
        volume: 1.0,
        pitch: 0.33 + (0.5 - 0.33) * sound_roll,
        seed: (sound_roll * 1_000_000.0) as i64,
    })
}

/// Applies a `USE_ITEM`: ordered item-use arms, plus projectile items whose
/// specialized behavior replaces the ordinary path.
///
/// The order below is load-bearing — see `crate::item_use`'s module doc. The
/// launch arm sits first because those projectile items use a disjoint path and
/// cannot race the arms below.
///
/// `food_level` and `invulnerable` are the acting player's values for the two
/// non-item can-eat conditions.
#[allow(clippy::too_many_arguments)]
fn apply_use_item(
    mobs: &MobHandle,
    effects: &crate::mob_effects::ActiveEffects,
    inventory: &mut PlayerInventory,
    player_pos: Option<(f64, f64, f64)>,
    client_movement: ClientMovement,
    game_mode: GameMode,
    food_level: i32,
    invulnerable: bool,
    hand: u8,
    yaw: f32,
    pitch: f32,
    // The fishing-rod cast/retrieve dispatch needs the caster's own
    // entity id, both to own the bobber (`MobSim::cast_fishing_bobber`'s
    // `owner`) and to find it again on the next click
    // (`MobSim::player_active_bobber`).
    player_entity_id: i32,
) -> UseItemOutcome {
    let native = if hand == 1 {
        crate::inventory::OFFHAND_NATIVE
    } else {
        usize::from(inventory.selected_hotbar_slot())
    };
    let Some(stack) = inventory.native(native) else {
        return UseItemOutcome::Nothing;
    };
    let held = stack.item.to_string();
    let path = stack.item.path().to_owned();
    // Captured before `consume_one` borrows the inventory mutably, and before
    // the stack this reads is gone. A splash or lingering potion carries its
    // identity here and nowhere else on the launch path, so without this the
    // thrown entity has no potion to apply on impact and the whole splash
    // implementation is unreachable from play.
    let thrown_potion = stack.components.potion.and_then(PotionId::from_registry_id);

    if let Some(intent) = launch_intent(&path) {
        // No tracked position means no launch origin, and guessing the origin
        // would put an arrow at the world origin — the same "no data yet, don't
        // guess" gate `apply_attack` uses for knockback direction. Checked here
        // rather than at the top of the function so a *consume* still works before
        // the first movement packet arrives; it needs no position at all.
        let Some((x, y, z)) = player_pos else {
            return UseItemOutcome::Nothing;
        };
        return match intent {
            LaunchIntent::BeginDraw => {
                // The arrow check happens at *release*, not here: vanilla lets a
                // player draw an empty bow (the animation plays) and simply
                // declines to fire. Refusing the draw would also make the release
                // arm unable to tell "no ammunition" from "never drew".
                UseItemOutcome::Draw(BowDraw {
                    started_tick: mobs.with(|sim| sim.tick_count()),
                    yaw,
                    pitch,
                })
            }
            LaunchIntent::InstantThrow {
                projectile,
                power,
                pitch_offset,
            } => {
                if !consume_one(inventory, native, game_mode) {
                    return UseItemOutcome::Nothing;
                }
                let velocity = client_movement.add_to_launch(
                    lodestone_entity::projectile::launch_velocity(
                        f64::from(yaw),
                        f64::from(pitch),
                        pitch_offset,
                        power,
                    ),
                );
                spawn_player_projectile(
                    mobs,
                    projectile,
                    Vec3::new(x, y + EYE_HEIGHT, z),
                    velocity,
                    thrown_potion,
                );
                UseItemOutcome::Nothing
            }
        };
    }

    // Vanilla's own fishing-rod-item use routine: overrides its own item-use routine entirely, exactly like the
    // launch-intent items above, so it sits ahead of the `Consumable`/
    // `Equippable` arms rather than as one of them. A rod already carrying a
    // live bobber reels it in; otherwise it casts a fresh one.
    if path == "fishing_rod" {
        let Some((x, y, z)) = player_pos else {
            return UseItemOutcome::Nothing;
        };
        if let Some(bobber_id) = mobs.with(|sim| sim.player_active_bobber(player_entity_id)) {
            // Vanilla's own fishing-rod-item use routine's "already fishing" arm — reel it in.
            // `FishingRetrieve::rod_damage` is vanilla's own `hurtAndBreak`
            // tier for the rod; this crate models no item durability at all
            // (see the flint-and-steel precedent in `apply_use_item_on`, whose
            // own comment discloses the same gap), so the catch itself lands
            // for real — loot spawned, xp awarded — and only the durability
            // half is the disclosed no-op.
            // The bobber already carries its rod-derived luck. The player's
            // current Luck/Unluck attribute is sampled when the catch is
            // rolled, so expiry before retrieval is observable and no second
            // effect timer is needed in the fishing simulation.
            mobs.with(|sim| {
                sim.retrieve_fishing_bobber(
                    bobber_id,
                    Vec3::new(x, y, z),
                    effects.luck(),
                )
            });
        } else {
            // Vanilla's own fishing-rod-item use routine's cast arm. `luck`/`lure_speed` are `0, 0`
            // No enchantment model reaches this call site yet (see
            // `MobSim::cast_fishing_bobber`'s own doc).
            mobs.with(|sim| {
                sim.cast_fishing_bobber(
                    player_entity_id,
                    Vec3::new(x, y, z),
                    y + EYE_HEIGHT,
                    yaw,
                    pitch,
                    0,
                    0,
                )
            });
        }
        return UseItemOutcome::Nothing;
    }

    // Arm 1: vanilla's own consumable data component → its own start-consuming routine, whose
    // own `canConsume` is vanilla's own can-eat check. A refusal is vanilla's `FAIL` — no use
    // starts, so a full player's right-click on steak does nothing at all, which
    // is the behaviour whose absence is most visible.
    if let Some(food) = crate::item_use::food_for_item(&held) {
        if !crate::item_use::can_eat(food, food_level, invulnerable) {
            return UseItemOutcome::Nothing;
        }
        let now = mobs.with(|sim| sim.tick_count());
        return UseItemOutcome::Consuming(ItemInUse {
            native,
            item: held,
            finish_tick: now + u64::try_from(food.use_ticks.max(0)).unwrap_or(0),
            last_effect_remaining: None,
        });
    }

    // Arm 2: vanilla's own equippable data component gated on `swappable()`. Instantaneous,
    // and it is behind arm 1 for the reason `crate::item_use`'s doc gives — an
    // item that is both eats rather than equips.
    if let Some(swap) = crate::item_use::swap_with_equipment_slot(
        inventory,
        native,
        game_mode == GameMode::Creative,
    ) {
        return UseItemOutcome::Equipped(swap);
    }

    // Arms 3 and 4 (`BLOCKS_ATTACKS`, `KINETIC_WEAPON`) would only
    // `startUsingItem`, and nothing here consumes a raised shield.
    UseItemOutcome::Nothing
}

/// Finishes a consume whose clock ran out — vanilla's own complete-using-item →
/// finish-using-item → consumable-on-consume → food-properties-on-consume chain,
/// which applies the food value and removes one item from the used stack.
///
/// Returns the slot to report and the stack now in it, or `None` when the use is
/// stale: the slot's contents changed under it (a hotbar swap, a container click)
/// or the food is gone. `Option<Option<..>>` rather than a bool so an emptied slot
/// is reported as an *empty* slot rather than as nothing to report — the
/// zero-count-ghost trap.
fn finish_consuming(
    inventory: &mut PlayerInventory,
    vitals: &mut PlayerVitals,
    use_in_progress: &ItemInUse,
    game_mode: GameMode,
) -> Option<(usize, Option<ItemStack>)> {
    let still_there = inventory
        .native(use_in_progress.native)
        .is_some_and(|stack| stack.item.to_string() == use_in_progress.item);
    if !still_there {
        return None;
    }
    let food = crate::item_use::food_for_item(&use_in_progress.item)?;
    let mut data = vitals.food();
    data.eat(food.nutrition, food.saturation_modifier);
    vitals.set_food(data);
    if !consume_one(inventory, use_in_progress.native, game_mode) {
        return None;
    }
    Some((
        use_in_progress.native,
        inventory.native(use_in_progress.native).cloned(),
    ))
}

/// Vanilla's own ominous-bottle-amplifier on-consume routine: finishing a drink of
/// `minecraft:ominous_bottle` grants `minecraft:bad_omen` for 120000 ticks
/// and consumes the bottle — the raid-trigger producer
/// (`item_use.rs`'s own disclosed "potions" gap, closed for exactly this one
/// item rather than generally).
///
/// A separate function from [`finish_consuming`] rather than a branch inside
/// it: that function's success arm is deliberately food-only — its own call
/// sites play the burp sound and `item_consume_finished` effect specifically
/// *because* the item was food (see those call sites' own comments) — and an
/// ominous bottle is not food and must not burp. Same `still_there`/
/// `consume_one` shape as [`finish_consuming`], reused rather than
/// restated.
///
/// The item data does not retain the per-stack amplifier roll, so every bottle
/// grants amplifier `0`. That value still satisfies
/// `absorb_raid_omen(0, 0) == 1` and starts a genuine raid when Bad Omen
/// converts; it represents the weakest roll rather than a no-op.
fn finish_drinking_ominous_bottle(
    inventory: &mut PlayerInventory,
    effects: &mut crate::mob_effects::ActiveEffects,
    use_in_progress: &ItemInUse,
    game_mode: GameMode,
) -> Option<(usize, Option<ItemStack>)> {
    if use_in_progress.item != "minecraft:ominous_bottle" {
        return None;
    }
    let still_there = inventory
        .native(use_in_progress.native)
        .is_some_and(|stack| stack.item.to_string() == use_in_progress.item);
    if !still_there {
        return None;
    }
    effects.apply("minecraft:bad_omen", 120_000, 0);
    if !consume_one(inventory, use_in_progress.native, game_mode) {
        return None;
    }
    Some((
        use_in_progress.native,
        inventory.native(use_in_progress.native).cloned(),
    ))
}

/// Finishing a drink of `minecraft:potion` applies the complete built-in effect
/// list without scaling. An empty or unsupported potion entry produces no
/// effects.
///
/// Reuses [`crate::mob_effects::potion_splash_effects`] at `scale = 1.0`,
/// `duration_scale = 1.0` rather than re-deriving the list: that function's own
/// `splash_instant_amount`/`splash_timed_duration` are both the identity
/// transform at `scale = 1.0` (`floor(1.0 * x + 0.5) == x` for the non-negative
/// integer `x` every potion table entry is), so direct drinking preserves every
/// amount and duration from the table. `duration_scale` is `1.0` because this
/// build's `ItemComponents` does not model `minecraft:potion_duration_scale`.
///
/// Returns the `(slot, remaining stack)` pair [`finish_consuming`] does, plus
/// the effect list to apply — `None` when the item is not a potion or the use
/// is stale (the slot's contents changed under it), matching every sibling
/// `finish_*` function's `still_there` gate.
fn finish_drinking_potion(
    inventory: &mut PlayerInventory,
    use_in_progress: &ItemInUse,
    game_mode: GameMode,
) -> Option<(usize, Option<ItemStack>, Vec<crate::mob_effects::SplashEffect>)> {
    if use_in_progress.item != "minecraft:potion" {
        return None;
    }
    let stack = inventory.native(use_in_progress.native)?;
    if stack.item.to_string() != use_in_progress.item {
        return None;
    }
    let effects = stack
        .components
        .potion
        .and_then(PotionId::from_registry_id)
        .map(|id| crate::mob_effects::potion_splash_effects(id, 1.0, 1.0))
        .unwrap_or_default();
    if !consume_one(inventory, use_in_progress.native, game_mode) {
        return None;
    }
    Some((
        use_in_progress.native,
        inventory.native(use_in_progress.native).cloned(),
        effects,
    ))
}

/// Vanilla's own consumables table's milk-bucket on-consume entry
/// (its own clear-all-status-effects consume effect) — a drunk milk bucket wipes every active status effect.
///
/// Returns the `(slot, remaining stack)` pair plus the ids that were actually
/// active (and are now gone), so the caller can send one
/// `encode_remove_mob_effect` per id rather than guessing which ones changed.
/// An empty vec is a real answer (a player with nothing active drank milk for
/// nothing, exactly like vanilla), not a "did not run" sentinel — matching the
/// water-bottle-control shape this crate's other consume paths already use.
///
/// **Disclosed narrowing**: vanilla's `MilkBucketItem` additionally converts
/// the stack to `minecraft:bucket` (`usingConvertsTo`) rather than consuming it
/// outright; `item_use`'s own module doc already names `usingConvertsTo` as not
/// modelled (a stew leaving a bowl is the same gap), so this reuses
/// [`consume_one`] like every other drink here and empties the stack instead.
/// The effect-clearing half — this function's actual reason to exist — is
/// complete.
fn finish_drinking_milk(
    inventory: &mut PlayerInventory,
    effects: &mut crate::mob_effects::ActiveEffects,
    use_in_progress: &ItemInUse,
    game_mode: GameMode,
) -> Option<(usize, Option<ItemStack>, Vec<String>)> {
    if use_in_progress.item != "minecraft:milk_bucket" {
        return None;
    }
    let still_there = inventory
        .native(use_in_progress.native)
        .is_some_and(|stack| stack.item.to_string() == use_in_progress.item);
    if !still_there {
        return None;
    }
    let cleared: Vec<String> = effects
        .active()
        .into_iter()
        .map(|(id, _)| id.to_owned())
        .collect();
    effects.clear();
    if !consume_one(inventory, use_in_progress.native, game_mode) {
        return None;
    }
    Some((
        use_in_progress.native,
        inventory.native(use_in_progress.native).cloned(),
        cleared,
    ))
}

/// Applies a `RELEASE_USE_ITEM` that ends a bow draw: computes the charge, refuses
/// a shot too weak or unarmed, and launches the arrow.
///
/// Returns `true` if an arrow was actually fired, so a caller (and a gate) can
/// tell a released-but-declined draw from a shot.
fn apply_release_use_item(
    mobs: &MobHandle,
    inventory: &mut PlayerInventory,
    player_pos: Option<(f64, f64, f64)>,
    client_movement: ClientMovement,
    player_rot: Option<Rotation>,
    game_mode: GameMode,
    draw: BowDraw,
) -> bool {
    use lodestone_entity::projectile::{BOW_ARROW_SPEED, BOW_MIN_POWER, bow_power_for_time};
    let Some((x, y, z)) = player_pos else {
        return false;
    };
    // Ticks, from the server's own 20 TPS counter — never `Instant::now()`, which
    // compiles on wasm32 and then panics at runtime under `panic = "abort"` with no
    // log line. `saturating_sub` because the counter is shared and a draw recorded
    // against a sim that was later reseeded must read as a zero-length draw rather
    // than wrapping to an enormous one.
    let held_ticks = mobs
        .with(|sim| sim.tick_count())
        .saturating_sub(draw.started_tick);
    let power = bow_power_for_time(i32::try_from(held_ticks).unwrap_or(i32::MAX));
    if power < BOW_MIN_POWER {
        return false;
    }
    // Vanilla's own bow-item release-using routine resolves the ammunition *before* checking the power in
    // vanilla; the order is unobservable here because neither has a side effect
    // until both pass.
    let Some(ammo_slot) = find_item_slot(inventory, BOW_AMMUNITION) else {
        return false;
    };
    if !consume_one(inventory, ammo_slot, game_mode) {
        return false;
    }
    let rotation = player_rot.unwrap_or(Rotation {
        yaw: draw.yaw,
        pitch: draw.pitch,
    });
    let velocity = client_movement.add_to_launch(
        lodestone_entity::projectile::launch_velocity(
            f64::from(rotation.yaw),
            f64::from(rotation.pitch),
            0.0,
            power * BOW_ARROW_SPEED,
        ),
    );
    spawn_player_projectile(mobs, "arrow", Vec3::new(x, y + EYE_HEIGHT, z), velocity, None);
    true
}

/// Spawns one player-launched projectile into the live sim, picking the ballistic
/// family from the projectile's own registry path.
///
/// `owner` is `None`: this crate's [`MobSim`] numbers mobs and projectiles in one
/// id space that connected **players** are not part of (their ids come from the
/// `PlayerRegistry`), so there is no mob id to exclude — and players are not
/// impact candidates either, so a player cannot be hit by their own arrow
/// regardless. Passing a player entity id here would silently exclude whichever
/// *mob* happened to share that number, which is worse than passing nothing.
///
/// `potion` is the thrown stack's validated `minecraft:potion` identity, and is what
/// [`MobSim::resolve_potion_splash`] later reads to decide which effects the
/// impact applies. It is `None` for every projectile that is not a splash or
/// lingering potion, and also for a potion stack carrying no potion component —
/// a water bottle, which correctly applies nothing.
fn spawn_player_projectile(
    mobs: &MobHandle,
    projectile: &str,
    origin: Vec3,
    velocity: Vec3,
    potion: Option<PotionId>,
) {
    use lodestone_entity::projectile::Projectile;
    let Ok(key) = lodestone_model::ResourceKey::new("minecraft", projectile) else {
        return;
    };
    // The two families disagree on gravity, drag *and* step order — see
    // `lodestone_entity::projectile`'s module doc. A trident integrates as an
    // arrow despite being thrown.
    let ballistic = match projectile {
        "arrow" | "spectral_arrow" | "trident" => Projectile::arrow(origin, velocity),
        _ => Projectile::throwable(origin, velocity),
    };
    // Only the two potion kinds take the potion-carrying spawn; everything else
    // would record a `potion` nothing reads. Splitting on the projectile name
    // rather than on `potion.is_some()` keeps a mis-set component from turning
    // a snowball into a splash.
    match projectile {
        "splash_potion" | "lingering_potion" => mobs.with(|sim| {
            sim.spawn_potion_projectile_from(key.clone(), ballistic, None, potion);
        }),
        _ => mobs.with(|sim| {
            sim.spawn_projectile_from(key.clone(), ballistic, None);
        }),
    }
}

/// The first native slot holding `path`, if any.
fn find_item_slot(inventory: &PlayerInventory, path: &str) -> Option<usize> {
    (0..crate::inventory::PLAYER_NATIVE_SIZE).find(|&i| {
        inventory
            .native(i)
            .is_some_and(|stack| stack.item.path() == path)
    })
}

/// Removes one item from native slot `native`, clearing the slot when the stack
/// empties. A creative-mode player consumes nothing but still succeeds.
///
/// Returns whether the launch may proceed — `false` only when the slot turned out
/// to be empty, which a caller reads as "no ammunition".
fn consume_one(inventory: &mut PlayerInventory, native: usize, game_mode: GameMode) -> bool {
    let Some(stack) = inventory.native(native) else {
        return false;
    };
    if game_mode == GameMode::Creative {
        return true;
    }
    let mut stack = stack.clone();
    if stack.count <= 1 {
        inventory.set_native(native, None);
    } else {
        stack.count -= 1;
        inventory.set_native(native, Some(stack));
    }
    true
}

/// Resolves a `minecraft:attack` request against the live mob
/// simulation: runs the damage pipeline and, for a sprinting attacker, the
/// melee knockback bonus, through [`MobSim::attack`](crate::MobSim::attack).
///
/// **No reply packet is sent from here.** The attack request has no direct
/// acknowledgement. The entity-streaming pass
/// (`EntityStreamer::sync`, called immediately after
/// [`dispatch_play_packet`] returns, on every inbound packet including this
/// one) to carry the result to every connection tracking the target: a
/// knocked-back mob's new position/velocity, or its removal on a killing
/// blow, both flow through [`MobHandle`]'s [`EntitySource`] implementation.
/// The `mobs` handle is shared with [`crate::tick::run_tick_loop`], so the
/// stream observes the updated snapshot. See [`MobSim::attack`](crate::MobSim::attack)'s own doc
/// comment for why `attacker_pos` (not a tracked player yaw — this crate
/// tracks no player rotation at all) stands in for
/// [`lodestone_physics::knockback::attack_direction`]'s real facing formula.
///
/// A connection with no tracked position (`player_pos` is `None`) still lands the
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
///
/// Routes through [`MobSim::attack_from_player`] so the mob simulation receives
/// the attacking account identity and can record villager reputation events.
/// Uses `LOCAL_PLAYER_ENTITY_ID` for [`PlayerIdentity::entity_id`], matching
/// every other self-facing identity built in this file.
fn apply_attack(
    mobs: &MobHandle,
    player_pos: Option<(f64, f64, f64)>,
    sprinting: bool,
    inventory: &PlayerInventory,
    effects: &crate::mob_effects::ActiveEffects,
    entity_id: i32,
    player_uuid: uuid::Uuid,
) {
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
    // The weapon feed resolves the held item through the `ATTACK_DAMAGE`
    // attribute fold. An empty hand uses the player's attribute base with no
    // modifiers.
    let raw_damage = effects.melee_damage(inventory.combat_stats().attack_damage);
    mobs.with(|sim| {
        sim.attack_from_player(
            entity_id,
            Some(PlayerIdentity {
                uuid: player_uuid,
                entity_id: LOCAL_PLAYER_ENTITY_ID,
            }),
            attacker_pos,
            raw_damage,
            DamageFlags::default(),
            knockback_power,
        )
    });
}

/// Records the main-hand animation implied by a serverbound attack packet.
///
/// Every hosted protocol uses a main-hand attack action. The connection that
/// sent it already animates locally, so only a multiplayer registry needs the
/// event; singleplayer has no remote observer.
fn record_attack_swing(players: Option<&PlayerRegistry>, player_entity_id: i32) {
    if let Some(registry) = players {
        registry.swing(player_entity_id, lodestone_model::Hand::Main);
    }
}

/// Resolves `ServerBound::SpectatorAction`'s target against this crate's two
/// id-keyed entity sources — the mob simulation and the player registry —
/// and returns the entity id to attach the camera to, or `None` when any of
/// vanilla's own gates (spectator mode, a target present, a resolvable
/// position, in range) fail. See `ServerBound::SpectatorAction`'s own doc
/// comment for the narrowing from vanilla's box-aware range/`isPickable`
/// checks down to a plain centre-to-centre distance.
fn apply_spectator_action(
    game_mode: GameMode,
    target_entity_id: Option<i32>,
    player_pos: Option<(f64, f64, f64)>,
    mobs: &MobHandle,
    players: Option<&PlayerRegistry>,
) -> Option<i32> {
    if game_mode != GameMode::Spectator {
        return None;
    }
    let target_id = target_entity_id?;
    let (px, py, pz) = player_pos?;
    let target_pos = mobs.with(|sim| sim.position(target_id)).or_else(|| {
        players
            .map(PlayerRegistry::candidates)
            .unwrap_or_default()
            .into_iter()
            .find(|c| c.entity_id == target_id)
            .map(|c| c.position)
    })?;
    // Vanilla's own is-within-entity-interaction-range check grows the target's actual
    // bounding box by this constant (its own `INTERACTION_RANGE` for
    // spectator-camera checks) before measuring; this crate tracks no
    // per-entity bounding box, so a plain centre-to-centre distance against
    // the same 3-block figure is the disclosed narrowing.
    const SPECTATOR_INTERACTION_RANGE: f64 = 3.0;
    let dx = target_pos.x - px;
    let dy = target_pos.y - py;
    let dz = target_pos.z - pz;
    if dx * dx + dy * dy + dz * dz <= SPECTATOR_INTERACTION_RANGE * SPECTATOR_INTERACTION_RANGE {
        Some(target_id)
    } else {
        None
    }
}

/// The outer option denies unauthorized requests without a response; the inner
/// option reports whether the current dimension contains a block entity.
fn block_entity_query_tag(
    entities: &BlockEntityHandle,
    permission_level: u8,
    pos: BlockPos,
) -> Option<Option<lodestone_core::Nbt>> {
    if permission_level < COMMANDS_GAMEMASTER_LEVEL {
        return None;
    }
    Some(entities.with(|registry| {
        registry.get(pos).map(|entity| {
            let mut tag = crate::chunk_nbt::block_entity_to_nbt(pos, entity);
            if let lodestone_core::Nbt::Compound(fields) = &mut tag {
                fields.retain(|(key, _)| !matches!(key.as_str(), "id" | "x" | "y" | "z" | "keepPacked"));
            }
            tag
        })
    }))
}

/// Native inspection shares the save record's modeled fields. Unsupported
/// entity kinds and unknown ids receive no reply, rather than a false empty tag.
fn entity_query_tag(mobs: &MobHandle, permission_level: u8, entity_id: i32) -> Option<lodestone_core::Nbt> {
    if permission_level < COMMANDS_GAMEMASTER_LEVEL {
        return None;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        mobs.with(|sim| {
            let uuid = sim.snapshots().into_iter().find(|entity| entity.id == entity_id)?.uuid;
            let saved = sim.saved_entities().into_iter().find(|entity| entity.uuid == uuid)?;
            let mut tag = saved.to_nbt();
            if let lodestone_core::Nbt::Compound(fields) = &mut tag {
                fields.retain(|(key, _)| key != "id");
            }
            Some(tag)
        })
    }
    #[cfg(target_arch = "wasm32")]
    {
        // The save-record serializer currently belongs to native persistence.
        let _ = (mobs, entity_id);
        None
    }
}

#[cfg(test)]
mod block_entity_query_tests {
    use super::*;
    use lodestone_core::Nbt;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn entity_query_selects_the_requested_live_mob_and_checks_permission() {
        let mobs = MobHandle::default();
        let entity_id = mobs.with(|sim| {
            sim.spawn_species("minecraft:cow".parse().unwrap(), Vec3::new(1.0, 64.0, 3.0))
                .set_health(19.0);
            sim.spawn_species("minecraft:zombie".parse().unwrap(), Vec3::new(-7.5, 68.0, 2.25))
                .set_health(7.25).id()
        });
        assert_eq!(entity_query_tag(&mobs, 1, entity_id), None);
        assert_eq!(entity_query_tag(&mobs, 2, i32::MAX), None);
        let Nbt::Compound(fields) = entity_query_tag(&mobs, 2, entity_id).unwrap() else {
            panic!("live mob query returns a compound")
        };
        assert!(fields.contains(&("Health".into(), Nbt::Float(7.25))));
        assert!(fields.contains(&("Pos".into(), Nbt::List {
            element_type: lodestone_core::NbtTag::Double,
            elements: vec![Nbt::Double(-7.5), Nbt::Double(68.0), Nbt::Double(2.25)],
        })));
        assert!(!fields.iter().any(|(key, _)| key == "id"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn entity_query_preserves_item_identity_and_lifecycle() {
        let mobs = MobHandle::default();
        let entity_id = mobs.with(|sim| sim.spawn_item(
            "minecraft:diamond".parse().unwrap(), Vec3::new(3.0, 65.0, 9.0),
            Vec3::new(0.0, -0.25, 0.0),
            lodestone_entity::item_entity::ItemLifecycle { age: 73, pickup_delay: 6, count: 5, max_stack_size: 64 },
        ));
        let Nbt::Compound(fields) = entity_query_tag(&mobs, 2, entity_id).unwrap() else {
            panic!("dropped item query returns a compound")
        };
        assert!(fields.contains(&("Item".into(), Nbt::Compound(vec![
            ("id".into(), Nbt::String("minecraft:diamond".into())),
            ("count".into(), Nbt::Int(5)),
        ]))));
        assert!(fields.contains(&("Age".into(), Nbt::Short(73))));
        assert!(fields.contains(&("PickupDelay".into(), Nbt::Short(6))));
        assert!(!fields.iter().any(|(key, _)| key == "id"));
        mobs.with(|sim| { sim.remove_item(entity_id); });
        assert_eq!(entity_query_tag(&mobs, 2, entity_id), None);
    }

    #[test]
    fn block_entity_query_checks_permission_and_strips_only_metadata() {
        let entities = BlockEntityHandle::new();
        let pos = BlockPos::new(-3, -17, 5);
        entities.with(|registry| registry.insert(pos, crate::block_entities::BlockEntity::Opaque {
            id: "minecraft:chest".into(),
            nbt: Nbt::Compound(vec![
                ("id".into(), Nbt::String("minecraft:chest".into())),
                ("x".into(), Nbt::Int(-3)),
                ("y".into(), Nbt::Int(-17)),
                ("z".into(), Nbt::Int(5)),
                ("CustomName".into(), Nbt::String("Supplies".into())),
            ]),
        }));
        assert_eq!(block_entity_query_tag(&entities, 1, pos), None);
        assert_eq!(block_entity_query_tag(&entities, 2, pos), Some(Some(Nbt::Compound(vec![
            ("CustomName".into(), Nbt::String("Supplies".into())),
        ]))));
        assert_eq!(block_entity_query_tag(&entities, 2, BlockPos::new(9, 8, 7)), Some(None));
        entities.with(|registry| {
            let crate::block_entities::BlockEntity::Opaque { nbt: Nbt::Compound(fields), .. } =
                registry.get(pos).unwrap() else { panic!("opaque compound retained") };
            assert_eq!(fields.len(), 5, "query must not mutate saved metadata");
        });
    }

    #[test]
    fn block_entity_query_serializes_the_live_container() {
        let entities = BlockEntityHandle::new();
        let pos = BlockPos::new(7, 64, -9);
        entities.with(|registry| registry.insert(pos, crate::block_entities::BlockEntity::Container {
            id: "minecraft:chest".into(),
            slots: vec![Some(ItemStack::new("minecraft:apple".parse().unwrap(), 5))],
        }));
        assert_eq!(block_entity_query_tag(&entities, 2, pos), Some(Some(Nbt::Compound(vec![
            ("components".into(), Nbt::Compound(vec![])),
            ("Items".into(), Nbt::List {
                element_type: lodestone_core::NbtTag::Compound,
                elements: vec![Nbt::Compound(vec![
                    ("Slot".into(), Nbt::Byte(0)),
                    ("id".into(), Nbt::String("minecraft:apple".into())),
                    ("count".into(), Nbt::Int(5)),
                ])],
            }),
        ]))));
    }
}

/// Maps main/off-hand ordinals to animation action bytes; invalid hands use main.
fn swing_action(hand: lodestone_model::Hand) -> u8 {
    match hand {
        lodestone_model::Hand::Main => 0,
        lodestone_model::Hand::Off => 3,
    }
}

/// Folds one recipe-book acknowledgement and returns the one-entry update that
/// exposes the cleared flag back to the client. The wire id is an opaque
/// position in the server-owned *advertised* entries, not every recipe in the
/// corpus: entries without a display must not manufacture acknowledgement
/// state from a malformed packet.
fn record_recipe_book_seen(
    inventory: &mut PlayerInventory,
    recipe_index: i32,
) -> Option<crate::crafting::RecipeBookEntry> {
    let mut entry = crate::crafting::recipe_book_entries()
        .iter()
        .find(|entry| entry.id == recipe_index)?
        .clone();
    inventory.mark_recipe_book_entry_seen(recipe_index);
    entry.highlight = false;
    Some(entry)
}

/// Makes a connection-specific recipe-book snapshot from the shared immutable
/// corpus. A fresh display id highlights until this connection acknowledges it;
/// the clone keeps that mutable flag out of the shared recipe definitions.
fn recipe_book_snapshot(inventory: &PlayerInventory) -> Vec<crate::crafting::RecipeBookEntry> {
    crate::crafting::recipe_book_entries()
        .iter()
        .cloned()
        .map(|mut entry| {
            entry.highlight = inventory.recipe_book_entry_is_highlighted(entry.id);
            entry
        })
        .collect()
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
/// [`ViewTracker::set_view_radius`]), advances the chunk-batch
/// flow-control gate (see [`send_view_update`]), or applies a hotbar
/// selection/container click/creative-slot write against [`PlayerInventory`]
/// (see
/// [`apply_carried_item_changed`]/[`apply_container_clicked`]/[`apply_creative_mode_slot_set`]).
/// The `PlayerLoaded` marker is folded into the connection's readiness state;
/// fall simulation begins only after that marker, and is re-armed after a
/// respawn. Other unmodeled packets remain [`ServerBound::Ignored`] in
/// `State::Play`.
/// Reads the two cells needed by fall tracking without admitting terrain.
///
/// Movement and status packets arrive on the same task that drives the
/// integrated connection. A missing resident cell therefore means "try again
/// after the view stream catches up", not "ask the source to generate a
/// column now". Production packet/timer paths use this gate.
fn resident_fall_sample<S: ChunkSource + ?Sized>(
    source: &S,
    x: f64,
    y: f64,
    z: f64,
    on_ground: bool,
) -> Option<FallSample> {
    let bx = x.floor() as i32;
    let bz = z.floor() as i32;
    let feet = resident_block_state(source, bx, y.floor() as i32, bz)?;
    let below = resident_block_state(source, bx, (y - 0.2).floor() as i32, bz)?;
    Some(FallSample {
        y,
        on_ground,
        in_water: feet.block() == Block::Water,
        fall_resetting: crate::fall::is_fall_damage_resetting(feet),
        block_damage_modifier: crate::fall::block_damage_modifier(below),
    })
}

/// Reads a state id from a retained column without admitting terrain.
fn resident_block_state<S: ChunkSource + ?Sized>(source: &S, x: i32, y: i32, z: i32) -> Option<StateId> {
    match source.try_resident_block_state_id(x, y, z) {
        Some(crate::chunk_store::TryResident::Present(state)) => Some(state),
        Some(crate::chunk_store::TryResident::Busy)
        | Some(crate::chunk_store::TryResident::Absent) => None,
        None => Some(source.resident_block_state_id(x, y, z).unwrap_or_else(|| source.block_state_id(x, y, z))),
    }
}

/// Captures a resident column through the source's atomic try gate when it has
/// one. `Busy` and `Absent` both defer the caller; only legacy sources that do
/// not expose the gate use the older resident snapshot method.
fn resident_column<S: ChunkSource + ?Sized>(source: &S, cx: i32, cz: i32) -> Option<ChunkColumn> {
    match source.try_resident_column(cx, cz) {
        Some(crate::chunk_store::TryResident::Present(column)) => Some(column),
        Some(crate::chunk_store::TryResident::Busy)
        | Some(crate::chunk_store::TryResident::Absent) => None,
        None => source.resident_column(cx, cz),
    }
}

/// Recomputes a beacon pyramid only from retained cells. A failed layer is a
/// complete answer (`Some(0..=3)`); a missing cell in a layer that otherwise
/// passes defers the periodic effect probe instead of entering the generating
/// beacon helper.
fn resident_beacon_levels<S: ChunkSource + ?Sized>(
    source: &S,
    x: i32,
    y: i32,
    z: i32,
) -> Option<u8> {
    let mut levels = 0u8;
    for step in 1..=4i32 {
        let ly = y - step;
        let mut layer_ok = true;
        'layer: for lx in (x - step)..=(x + step) {
            for lz in (z - step)..=(z + step) {
                let state = resident_block_state(source, lx, ly, lz)?;
                if !crate::beacon::BASE_BLOCKS.contains(&state.block()) {
                    layer_ok = false;
                    break 'layer;
                }
            }
        }
        if !layer_ok {
            break;
        }
        levels = u8::try_from(step).unwrap_or(4);
    }
    Some(levels)
}

/// Checks a beacon beam without allowing a cold column to enter the 20 Hz
/// connection task. `None` means the complete scan is not resident yet;
/// `Some(false)` is a resident obstruction and is therefore a real answer.
fn resident_beam_unobstructed<S: ChunkSource + ?Sized>(
    source: &S,
    x: i32,
    y: i32,
    z: i32,
    scan_height: i32,
) -> Option<bool> {
    let max_y = source
        .dimension()
        .map(crate::dimension::Dimension::max_y)
        .or_else(|| {
            resident_column(source, x.div_euclid(16), z.div_euclid(16))
                .map(|column| column.min_y + column.height - 1)
        })
        .unwrap_or(y.saturating_add(scan_height));
    for dy in 1..=scan_height {
        if y.saturating_add(dy) > max_y {
            break;
        }
        let state = resident_block_state(source, x, y + dy, z)?;
        if state.block() == Block::Bedrock {
            continue;
        }
        let transparent = lodestone_data::light_props::dampening(state) < 15;
        if !transparent {
            return Some(false);
        }
    }
    Some(true)
}

/// Publishes the player's post-damage health, **and the death notification when
/// that damage was the hit that killed them.**
///
/// # Why every damage site must go through here
///
/// Health reaching zero does not by itself produce the death screen, animation,
/// sound, or statistic. This function is the single choke point for all five
/// damage sites, so it adds those cues exactly once.
///
/// # Why no "already announced" latch is needed
///
/// Every [`PlayerVitals`] damage entry point returns `None` once `health <= 0.0`
/// (its own first guard), so the caller only reaches this function on a hit that
/// *landed*, and a landed hit can cross zero exactly once per life. The kill
/// packet therefore fires once, without state to keep. [`PlayerVitals::respawn`]
/// re-arms it by construction. That is a property of the guards rather than of
/// this function, so `death_is_announced_exactly_once_per_life` pins it.
///
/// # The animation and sound cues, and why they are here rather than at each site
///
/// A hit also has to be *seen and heard*, and neither `set_health` nor
/// `player_combat_kill` carries any animation or sound: vanilla plays the camera
/// damage tilt off `hurt_animation`, tips the body over off `entity_event` byte
/// 3, and plays `playHurtSound`/`getDeathSound` alongside — this crate encoded
/// none of the three until this function grew them, so singleplayer damage was
/// silent and a death was a screen with a motionless, silent avatar behind it.
///
/// All three cues belong at this choke point for the same reason the death
/// *count* does — the guards above already make "a hit landed" and "the hit
/// that killed them" exactly-once properties, and re-deriving any of them at
/// fourteen call sites is how one of them ends up sending twice on a tick that
/// both burned and starved, or silent on the one path nobody remembered.
///
/// The hurt/death sound comes from [`crate::effects::mob_vocalisation`] with
/// `"minecraft:player"` — entity-type-generic despite the name, and it already
/// resolves the real registered `minecraft:entity.player.hurt`/`.death` sound
/// events. Pitch and the sound-variant seed are both held constant: this
/// function has no RNG source threaded to it, and neither player sound event has
/// more than one variant to pick between, so a constant seed costs nothing here
/// (contrast [`crate::effects::WorldEffect::Sound`]'s own doc, which explains why
/// a constant seed usually would).
///
/// `hurt` is what distinguishes a **hit** from a mere publish: two of the call
/// sites (the status-effect arm and the food arm) reach here for a *heal* or a
/// bare food-bar change, and flashing the screen red on a regeneration tick is a
/// worse bug than not flashing it at all. `None` there; `Some` only where damage
/// actually landed.
#[allow(clippy::too_many_arguments)]
async fn publish_health<T, P>(
    conn: &mut Connection<T>,
    state: &mut State,
    proto: &P,
    vitals: &PlayerVitals,
    effects: &crate::mob_effects::ActiveEffects,
    // The position the hurt/death sound is centred on (`WorldEffect::Sound`'s
    // wire form quantises it to eighths of a block, so a stale or zeroed
    // position only ever costs spatialisation accuracy, never a dropped
    // packet). Every call site already tracks this player's last reported
    // position for its own damage-source logic; a caller with no reported
    // position yet (joined and never moved) passes `Vec3::default()`.
    pos: Vec3,
    // Every caller passes `LOCAL_PLAYER_ENTITY_ID`, never a `PlayerRegistry`
    // ticket id: every packet built from this reaches `conn` directly, this
    // connection's own socket, and the client only recognises itself under
    // the constant its own login entity-id field (`begin_play_at`) claimed —
    // see the call sites' own comments. Kept as a plain parameter rather than
    // inlining the constant here so a future caller broadcasting to *other*
    // connections is not tempted to reuse this function for that; it never
    // varies today, and that is the point.
    player_entity_id: i32,
    username: &str,
    cause: crate::vitals::DeathCause,
    // The statistics store, for the `minecraft:deaths` custom counter. This is the
    // right site rather than each damage source: the function's own guards already
    // make crossing zero happen exactly once per life (see the doc comment above),
    // which is precisely the property a death *count* needs. Awarding it at each
    // `apply_*` call site would double-count a tick that both drowned and fell.
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
    // `Some(direction)` when this publish follows a hit that landed — see the doc
    // comment's third section. Every production site passes
    // `HurtDirection::PURE_ROLL`, and that is vanilla's own answer rather than a
    // stub: every damage type this crate has is `no_knockback`-tagged, so
    // `indicateDamage` would never see a non-zero offset for any of them.
    hurt: Option<crate::vitals::HurtDirection>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    // Ahead of the health packet, matching vanilla's order: `indicateDamage`
    // fires inside `hurtServer`, while the health value rides vanilla's own
    // per-player tick routine. The client folds this into the view bob's countdown,
    // so it wants to arrive with (or before) the health drop it explains.
    if let Some(direction) = hurt {
        apply(
            conn,
            state,
            proto.encode_hurt_animation(player_entity_id, direction.yaw_degrees()),
        )
        .await?;
        // Vanilla's own `hurtServer`'s `playHurtSound`/`die`'s death sound,
        // folded into this same choke point for the reason this function's doc
        // gives. `died` picks the death sound instead of the hurt one on the
        // killing blow, matching the `encode_entity_event` branch below rather
        // than re-deriving its own health check.
        if let Some(effect) = crate::effects::mob_vocalisation(
            "minecraft:player",
            pos,
            vitals.health() <= 0.0,
            false,
            1.0,
            0,
        ) {
            apply(conn, state, proto.encode_world_effect(&effect)).await?;
        }
    }
    apply(
        conn,
        state,
        proto.encode_set_health(
            vitals.health(),
            vitals.food().food_level(),
            vitals.food().saturation(),
        ),
    )
    .await?;
    if vitals.health() <= 0.0 {
        if matches!(effects.death_trigger(), Some(crate::mob_effects::DeathTrigger::WindCharged)) {
            apply(
                conn,
                state,
                proto.encode_world_effect(&crate::effects::wind_charged_death(pos)),
            )
            .await?;
        }
        advancements.award_stat(
            player_uuid,
            crate::advancements::StatKey::new(
                crate::advancements::StatType::Custom,
                "minecraft:deaths",
            ),
            1,
        );
        let message = cause.death_message(username);
        apply(
            conn,
            state,
            proto.encode_player_combat_kill(player_entity_id, &message),
        )
        .await?;
        // Vanilla's own entity-die routine's own broadcast, which its own
        // level broadcast-entity-event routine
        // sends to the dying player too (its own chunk-map broadcast-and-send routine). It is what
        // starts the client's `deathTime` counter — the fall-over tilt the red
        // overlay persists through. The death *screen* comes from the packet above;
        // this is the body behind it, and without it the avatar stands upright
        // through its own death.
        apply(
            conn,
            state,
            proto.encode_entity_event(player_entity_id, crate::protocol::entity_event::DEATH),
        )
        .await?;
    }
    Ok(())
}

/// Feeds one `on_ground` sample to the [`FallTracker`] from a movement packet
/// that carried **no** y coordinate, reusing the last position associated with
/// this connection.
///
/// Reusing the remembered y is not an approximation: `move_player_rot` and
/// `move_player_status_only` are precisely the two packets vanilla's own
/// client-side send-position routine picks when position did *not* change this tick,
/// so the last reported y is the current y by construction. Feeding it back
/// with the new `on_ground` is therefore the same `(y, on_ground)` pair the
/// tracker would have seen had the client sent a position packet.
///
/// Returns without touching the tracker when no position has been reported
/// yet — a status packet before the first movement packet has no y to pair
/// with, and inventing one (say, the spawn point) would fabricate a fall.
#[allow(clippy::too_many_arguments)]
async fn fall_status_sample<T, P, S>(
    conn: &mut Connection<T>,
    state: &mut State,
    proto: &P,
    // Read the retained terrain at the player's feet for water, climbable, and
    // landing block facts. A missing cell defers this sample until a later
    // packet; it must not turn a movement/status probe into cold generation.
    source: &S,
    player_pos: &Option<(f64, f64, f64)>,
    fall: &mut FallTracker,
    vitals: &mut PlayerVitals,
    effects: &crate::mob_effects::ActiveEffects,
    username: &str,
    on_ground: bool,
    client_loaded: bool,
    // `invulnerable` — creative and spectator. `fall` is not in
    // `#minecraft:bypasses_invulnerability` (only `out_of_world` and
    // `generic_kill` are), so an invulnerable player takes none of it. The
    // *tracker* still samples, so the fall is still tracked; only the hit is
    // skipped, matching the damage-immunity rule.
    invulnerable: bool,
    // `minecraft:deaths` counter, threaded only to reach
    // `publish_health` — see its own parameter comment for why the count belongs
    // there and not at each damage source.
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    if !client_loaded {
        return Ok(());
    }
    let Some((x, y, z)) = *player_pos else {
        return Ok(());
    };
    let Some(sample) = resident_fall_sample(source, x, y, z, on_ground) else {
        return Ok(());
    };
    if let Some(raw) = fall.on_player_moved(sample)
        && !invulnerable
        && vitals.apply_fall_damage(raw as f32).is_some()
    {
        publish_health(
            conn,
            state,
            proto,
            vitals,
            effects,
            Vec3::new(x, y, z),
            // Always `LOCAL_PLAYER_ENTITY_ID`, never the registry ticket's id:
            // this packet goes straight to `conn`, this player's own socket,
            // and vanilla's own login entity-id field (`begin_play_at`) always claims that
            // constant regardless of whether a `PlayerRegistry` exists — see
            // `LOCAL_PLAYER_ENTITY_ID`'s own doc comment. The ticket's real id
            // is for *other* connections' view of this player, never this one.
            LOCAL_PLAYER_ENTITY_ID,
            username,
            crate::vitals::DeathCause::Fall,
            advancements,
            player_uuid,
            // `minecraft:fall` is `no_knockback`-tagged, so vanilla's own
            // `indicateDamage` offset for it is `(0, 0)`.
            Some(crate::vitals::HurtDirection::PURE_ROLL),
        )
        .await?;
    }
    Ok(())
}

/// Administrative serverbound actions use permission level `2`, matching the
/// built-in `/gamemode`, `/gamerule`, and `/difficulty` command gates. This
/// constant covers the dedicated packets that perform the same actions without
/// going through a slash command.
const COMMANDS_GAMEMASTER_LEVEL: u8 = 2;

/// The one outstanding player-position correction for an acknowledgement-aware
/// connection. A newer correction supersedes an older one, so an overdue reply
/// cannot reopen movement after the server has already moved the player again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TeleportAcknowledgements {
    next_id: i32,
    pending_id: Option<i32>,
}

impl TeleportAcknowledgements {
    fn after_initial(initial_id: i32) -> Self {
        Self {
            next_id: initial_id.wrapping_add(1),
            pending_id: Some(initial_id),
        }
    }

    fn issue(&mut self) -> i32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.pending_id = Some(id);
        id
    }

    fn accepts(&mut self, id: i32) -> bool {
        if self.pending_id == Some(id) {
            self.pending_id = None;
            true
        } else {
            false
        }
    }

    fn is_pending(&self) -> bool {
        self.pending_id.is_some()
    }
}

fn issue_teleport_id(teleports: &mut Option<TeleportAcknowledgements>) -> i32 {
    teleports.as_mut().map_or(0, TeleportAcknowledgements::issue)
}

/// The movement sample a client has reported for its current local tick.
///
/// Position packets carry a delta only indirectly: the server derives it from
/// two absolute positions. The empty tick-end marker is the delimiter that
/// tells us when an absent position packet means zero movement rather than
/// "keep the previous sample". This is per connection because another
/// player's movement cannot affect this player's projectile launch.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ClientMovement {
    delta: Vec3,
    on_ground: bool,
    received_this_tick: bool,
}

impl Default for ClientMovement {
    fn default() -> Self {
        Self {
            delta: Vec3::new(0.0, 0.0, 0.0),
            on_ground: true,
            received_this_tick: false,
        }
    }
}

impl ClientMovement {
    /// Records the latest player-position sample in this client tick.
    fn observe(&mut self, delta: Vec3, on_ground: bool) {
        self.delta = delta;
        self.on_ground = on_ground;
        self.received_this_tick = true;
    }

    /// Ends the client's local tick, zeroing only a tick with no movement.
    fn finish_tick(&mut self) {
        if !self.received_this_tick {
            self.delta = Vec3::new(0.0, 0.0, 0.0);
        }
        self.received_this_tick = false;
    }

    /// Adds the source's latest movement to a launched projectile.
    ///
    /// Grounded sources contribute horizontal velocity only. This is the
    /// launch rule the protocol's movement boundary protects: a following
    /// idle tick must not leave a projectile with stale horizontal momentum.
    fn add_to_launch(self, velocity: Vec3) -> Vec3 {
        Vec3::new(
            velocity.x + self.delta.x,
            velocity.y + if self.on_ground { 0.0 } else { self.delta.y },
            velocity.z + self.delta.z,
        )
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_play_packet<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    // The dimension the connection joined in, which a respawn is resolved
    // against and returns to.
    home: SourceRef<'_, S>,
    state: &mut State,
    mut pending_relights: Option<&mut PendingRelights>,
    view: &mut ViewTracker,
    // This connection's chunk-residency guard, so a chunk-boundary
    // crossing or a live view-radius change (the `recenter`/`set_view_radius`
    // arms below) can move the same `PLAYER_LOADING`/`PLAYER_SIMULATION`
    // tickets `serve_play` granted at join, rather than leaving them pinned to
    // the join column for the connection's whole lifetime.
    player_ticket_guard: &PlayerTicketGuard,
    pending_keep_alive: &mut Option<i64>,
    pending_break: &mut Option<PendingBreak>,
    pending_prediction_ack: &mut connection_prediction::PendingPredictionAck,
    // The latest server-issued position correction. A matching
    // `TeleportationAccepted` clears it; movement stays inert while it remains.
    teleport_acknowledgements: &mut Option<TeleportAcknowledgements>,
    player_pos: &mut Option<(f64, f64, f64)>,
    // The latest position delta and its tick boundary. Projectile launches
    // inherit this connection-local motion; see [`ClientMovement`].
    client_movement: &mut ClientMovement,
    // Mirrors `player_pos` exactly — updated here, read back by
    // the caller, republished to the `PlayerRegistry` so *other* connections
    // stream this player's facing. `Option` because "no angles reported yet"
    // is distinct from "facing due south"; the registry keeps its join
    // default until a packet that actually carries angles arrives.
    player_rot: &mut Option<Rotation>,
    fall: &mut FallTracker,
    vitals: &mut PlayerVitals,
    burn: &mut crate::burning::BurnState,
    world: &crate::world_state::WorldStateHandle,
    inventory: &mut PlayerInventory,
    block_entities: &BlockEntityHandle,
    open_container: &mut Option<OpenContainer>,
    // Which merchant screen this connection has open, if any — see
    // [`OpenMerchant`]'s own doc for why it is not folded into
    // `open_container`.
    open_merchant: &mut Option<OpenMerchant>,
    container_sync: &mut ContainerSync,
    next_window_id: &mut i32,
    mobs: &MobHandle,
    sprinting: &mut bool,
    // Retained between input packets because the client sends a new bitset
    // only when movement input changes. Placement reads this secondary-use
    // state to bypass a clicked container while placing a block beside it.
    sneaking: &mut bool,
    awaiting_chunk_batch_ack: &mut bool,
    pending_chunk_batches: &mut VecDeque<PendingChunkBatch>,
    // The connection's live column stream, where the caller has one to lend. A
    // chunk-boundary crossing enqueues its newly-visible strip here instead of
    // generating it inline, so the view update costs this function a set difference
    // rather than a `2r + 1`-column `await` — see [`send_view_update`], which owns
    // the decision and the fallback.
    //
    // `Option` reflects the two streaming modes: native `serve_play` drains the
    // deferred stream from a `select!` branch, while the `wasm32` loop drains
    // its join inline and has no deferred-stream consumer.
    mut join_stream: Option<&mut crate::join_scheduler::JoinChunkStream<S>>,
    // `CommandSession` bundles command dispatch with the caller identity used
    // for command execution.
    commands: &CommandSession,
    // This connection's advancement/statistics store and the player
    // key its progress lives under. Threaded only to reach `apply_client_command`
    //'s `REQUEST_STATS` arm, which answers with the player's current stats —
    // see that function's own doc comment.
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
    // `Some` only after an online-authenticated Play handoff found a usable
    // Mojang issuer-key cache. This validates announcements independently of
    // whether the host requires signed chat. The cfg preserves the browser's
    // no-auth/degraded surface without linking `lodestone-auth` there.
    #[cfg(not(target_arch = "wasm32"))]
    profile_key_issuers: Option<&lodestone_auth::MojangPublicKeys>,
    // The separate vanilla policy gate: authenticated online connection,
    // `enforce-secure-profile`, and a usable issuer cache. An adopted session
    // still requires signatures even when this is false; this flag governs a
    // player that has announced no valid session.
    enforce_secure_profile: bool,
    // Mirrors `player_pos`/`player_rot` exactly — filled here,
    // read back by the caller, republished to the `PlayerRegistry` so *other*
    // connections see it. An out-parameter rather than two more parameters (a
    // registry and this connection's username) because the caller already
    // owns both, and this function already takes 25.
    outgoing_chat: &mut Vec<String>,
    // This connection's announced chat-signing session (if any) and the
    // verification chain position tracked against it — mirrors
    // `pending_keep_alive`/`player_pos`'s shape exactly: connection-scoped
    // state the caller owns and this function mutates in place. See
    // `crate::chat_session`'s own module doc for what it is and is not used
    // for.
    chat_session: &mut Option<crate::chat_session::ServerChatSession>,
    // The shared player registry, for the `ChatCommand` arm alone: a command's
    // entity selectors resolve against the roster, and a command's effects aimed
    // at *another* player are queued on it.
    //
    // A concrete `Option<&PlayerRegistry>` rather than the generic
    // `EntitySource` the caller holds, so this function gains no type parameter —
    // and an `Option` rather than a required handle because singleplayer builds
    // no registry at all (`open_in_memory`). The `ChatCommand` arm synthesises the
    // caller's own candidate in that case, which is what keeps `@s` working
    // there.
    players: Option<&PlayerRegistry>,
    // Threaded through only to reach `apply_use_item_on`, which
    // needs to ask the world tick loop for a neighbour-update fan-out that
    // outlives this packet — see that function's own parameter comment.
    block_ticks: &BlockTickFeed,
    // Responses to server-pushed resource packs are recorded here for the
    // host; policy decisions remain outside the protocol loop.
    resource_packs: &ResourcePackPushFeed,
    // Set by the client's empty readiness marker; fall simulation waits for
    // this signal so the first placement movement cannot create a false fall.
    client_loaded: &mut bool,
    // This connection's composter roll source — seeded once in
    // `serve_play`, advanced once per right-click (see
    // [`apply_composter_use`]'s `roll` parameter).
    composter_rng: &mut SpawnRng,
    // This connection's bone-meal roll source — seeded once in `serve_play`,
    // advanced by a bone-meal right-click on a growable block. Its own stream, so
    // fertilising a crop cannot shift which roll a later composter insert or
    // block drop sees.
    bone_meal_rng: &mut SpawnRng,
    // This connection's experience — level, bar and lifetime total.
    // `&mut` because closing a furnace pays out its banked smelting XP (the
    // `ContainerClosed` arm), which is currently the only production producer.
    experience: &mut crate::experience::PlayerExperience,
    // This connection's live status effects — written by `/effect` and
    // ticked from `serve_play`'s vitals timer.
    effects: &mut crate::mob_effects::ActiveEffects,
    // This connection's block-drop roll source — seeded once in
    // `serve_play`, advanced by every break that rolls a table (see
    // `apply_block_action`'s parameter comment). A second stream rather than
    // sharing the composter's, so a composter click cannot shift which drop a
    // later break rolls; the two features would otherwise be coupled through
    // nothing but draw order.
    drops_rng: &mut SpawnRng,
    // This connection's declared channel support (register/
    // unregister interpretation happens here, in Play) and the shared registry
    // to dispatch ordinary payloads on.
    client_channels: &mut ClientChannels,
    plugin_channels: &PluginChannelRegistry,
    // This connection's current game mode, `&mut` because the
    // `ChangeGameMode` arm and the built-in `/gamemode` both rewrite it — and
    // because the creative consequences below (instant break, damage immunity)
    // read it on later packets.
    game_mode: &mut GameMode,
    // The live ability record preserves client flight across mode changes.
    abilities: &mut Abilities,
    // The player's per-player respawn point, written by the bed
    // arm of `apply_use_item_on` and threaded through `serve_play`'s session
    // state. Read back by no caller yet — the placement half of P2 is the
    // next consumer (see `crate::world_spawn`'s module doc).
    respawn: &mut Option<RespawnPoint>,
    // The night-skip vote, fed by the two arms below — `lay_down`
    // on a bed click (`UseItemOn`), `get_up` on a wake-up (`PlayerCommand`
    // action 0). `player_entity_id` is this connection's roster key, resolved
    // once in `serve_play` (a `PlayerRegistry` ticket id where one exists,
    // `LOCAL_PLAYER_ENTITY_ID` in singleplayer) — see `serve_play`'s own
    // binding and `crate::sleep`'s module doc.
    sleep_vote: &SleepVote,
    // `ChatCommand`'s `CommandWorld` needs this to reach
    // `/worldborder`'s read/write surface — the same `BorderFeed` `serve_play`
    // already carries for the join broadcast and the vitals-tick damage read.
    border: &BorderFeed,
    player_entity_id: i32,
    // This connection's login name, for the death message
    // (`DeathCause::death_message`'s victim argument).
    username: &str,
    // The world spawn resolved at join, for the respawn teleport. See
    // `apply_client_command`'s own parameter comment.
    world_spawn: Vec3,
    // The server tick this packet is handled on, for
    // `apply_block_action`'s destroy-progress accounting. Native callers pass
    // the elapsed tick count; `wasm32` callers pass `None` because the browser
    // timer does not expose that counter. Hardness and range checks still apply
    // on that target.
    game_tick: Option<u64>,
    // This connection's in-progress bow draw, if any: the server tick
    // the `USE_ITEM` arrived on, so the `RELEASE_USE_ITEM` that ends it can turn
    // the interval into vanilla's own bow-item power-for-time routine. `None` whenever nothing
    // chargeable is being held down.
    //
    // Per-connection rather than shared, exactly like `sprinting` and
    // `player_pos`: two players can be mid-draw at once and neither's charge is
    // the other's.
    bow_draw: &mut Option<BowDraw>,
    // This connection's in-progress *consume* — eating or drinking. Held here for
    // the same reason `bow_draw` is, and separately from it because the two end
    // differently: a draw ends on a packet (`RELEASE_USE_ITEM`), while a consume
    // ends on the **server's own clock**. The per-tick arm in `serve_play`
    // counts the remaining duration and completes the action; the client sends
    // nothing when a steak finishes.
    item_in_use: &mut Option<ItemInUse>,
    // Set when a `ClientCommand`'s `PERFORM_RESPAWN` just fired *and* `source`
    // above was a portal-travelled dimension — see `apply_client_command`'s own
    // parameter comment. Both connection loops rebuild the home view before
    // dispatching another packet or publishing another world update.
    dimension_reset: &mut Option<connection_travel::DimensionReset>,
    // Leaving the End through the exit portal: the client's perform-respawn
    // answer to the win announcement is recorded here for the connection loop.
    end_exit: &mut connection_travel::EndExit,
    packet_id: i32,
    payload: &[u8],
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
{
    let packet = proto.decode(*state, packet_id, payload);
    pending_prediction_ack.observe(&packet)?;
    if let ServerBound::TeleportationAccepted { id } = packet {
        if let Some(teleports) = teleport_acknowledgements {
            teleports.accepts(id);
        }
        return Ok(());
    }
    if teleport_acknowledgements
        .as_ref()
        .is_some_and(TeleportAcknowledgements::is_pending)
        && matches!(
            &packet,
            ServerBound::PlayerMoved { .. }
                | ServerBound::PlayerRotated { .. }
                | ServerBound::PlayerStatusOnly { .. }
                | ServerBound::VehicleMoved { .. }
        )
    {
        return Ok(());
    }

    // Admit target, neighbour, and retained-light columns before any action
    // arm below performs its existing synchronous reads/writes. Awaiting here
    // keeps packets ordered and, for the integrated `Shared` source, moves
    // every cold `column()` call off this connection task. A cold admission is
    // never a reason to drop the packet.
    admit_action_footprint(source, &packet).await?;

    match packet {
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
            if abilities.flying {
                fall.reset();
            }
            let previous_pos = *player_pos;
            // Hunger exhaustion for the distance just travelled — vanilla's
            // vanilla's own check-movement-statistics routine, which is driven by the
            // position delta rather than by a per-tick constant. Charged **before**
            // `player_pos` is overwritten, because the delta needs the old value.
            //
            // Vanilla's expression is `0.1F * cm * 0.01F` where
            // `cm = round(sqrt(dx² + dz²) * 100)` — an `int` — so the rounding is
            // reproduced rather than collapsed into `0.1 * blocks`. It matters at
            // small steps: a sub-half-centimetre move rounds to zero centimetres and
            // costs nothing at all, which is what keeps a jittering client from
            // accumulating exhaustion.
            //
            // Only the **sprinting on ground** branch is charged, and that is not a
            // simplification: walking and crouching are literal `0.0F` multiplies in
            // vanilla, so the other on-ground branches genuinely cost nothing. The
            // swimming and eye-underwater branches (`0.01F`) are the real omission —
            // they need `isSwimming`/`isEyeInFluid`, which this arm does not have,
            // and charging sprint's constant for them would be ten times too much.
            if let Some((px, _, pz)) = *player_pos
                && *sprinting
                && on_ground
                && !Abilities::for_mode(*game_mode).invulnerable
            {
                let dx = x - px;
                let dz = z - pz;
                let cm = ((dx * dx + dz * dz).sqrt() as f32 * 100.0).round() as i32;
                if cm > 0 {
                    vitals.add_exhaustion(
                        crate::food::EXHAUSTION_SPRINT_PER_BLOCK * cm as f32 * 0.01,
                    );
                }
            }
            *player_pos = Some((x, y, z));
            let delta = previous_pos.map_or_else(
                || Vec3::new(0.0, 0.0, 0.0),
                |(previous_x, previous_y, previous_z)| {
                    Vec3::new(x - previous_x, y - previous_y, z - previous_z)
                },
            );
            client_movement.observe(delta, on_ground);
            // `move_player_pos_rot` carries angles and
            // `move_player_pos` does not, so this is `if let`, not an
            // assignment — overwriting with `None` on every straight-line
            // step would snap the avatar back to yaw 0 between turns, which
            // is a worse failure than never turning at all because it only
            // shows up while moving.
            if let Some(rotation) = rotation {
                *player_rot = Some(rotation);
            }

            if let Some(registry) = players {
                registry.set_presence(player_entity_id, source.dimension(), Vec3::new(x, y, z));
                if let Some(rotation) = *player_rot {
                    registry.set_rotation(player_entity_id, rotation);
                }
            }
            if world.dimension_runtime(source.dimension()).is_none() {
                let facing = player_rot.unwrap_or_default();
                let yaw = f64::from(facing.yaw).to_radians();
                let pitch = f64::from(facing.pitch).to_radians();
                let perceived = players.map_or_else(|| vec![PerceivedPlayer {
                    identity: Some(PlayerIdentity { uuid: player_uuid, entity_id: player_entity_id }),
                    perception: PlayerPerception {
                        position: Vec3::new(x, y, z),
                        held_item: inventory.selected_item().map(|stack| stack.item.clone()),
                        view_direction: Vec3::new(-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()),
                    },
                }], |registry| registry.perceptions(source.dimension()));
                mobs.with(|sim| { sim.set_players(perceived); });
            }

            // Chunk coordinate = floor(block / 16), not truncating division —
            // `-1.0_f64 / 16.0` must floor to chunk `-1`.
            let cx = (x / 16.0).floor() as i32;
            let cz = (z / 16.0).floor() as i32;
            if world.dimension_runtime(source.dimension()).is_none() {
                world.tick_anchors().publish(players.map_or_else(|| vec![crate::tick_area::TickAnchor {
                    dimension: source.dimension(), cx, cz,
                }], PlayerRegistry::tick_anchors));
            }
            // Read the center before the call, since `recenter` writes
            // `self.center` in place; comparing after would always see the
            // new value and move the ticket pair even on a no-op pass.
            let center_before_recenter = view.center;
            let update = view.recenter(
                proto,
                cx,
                cz,
                // The pose that arrived with this very packet where it carried
                // one, so the newly-visible strip is ordered towards what the
                // player is looking at rather than by `cx` then `cz`.
                player_rot.map(|rotation| rotation.yaw),
            );
            if view.center != center_before_recenter {
                player_ticket_guard.move_to_with_simulation_radius(
                    view.center,
                    view.radius,
                    view.radius.clamp(0, crate::chunk_store::CONCURRENT_TICK_RADIUS),
                );
                source.get().reconcile_ticket_residency();
            }
            send_view_update(
                conn,
                proto,
                source,
                join_stream.as_deref_mut(),
                state,
                view,
                update,
                awaiting_chunk_batch_ack,
                pending_chunk_batches,
            )
            .await?;

            if *client_loaded
                && let Some(sample) = resident_fall_sample(source.get(), x, y, z, on_ground)
                && let Some(raw) = fall.on_player_moved(sample)
                && !Abilities::for_mode(*game_mode).invulnerable
                && vitals.apply_fall_damage(raw as f32).is_some()
            {
                publish_health(
                    conn,
                    state,
                    proto,
                    vitals,
                    effects,
                    Vec3::new(x, y, z),
                    // Self-facing, per `fall_status_sample`'s own call site comment.
                    LOCAL_PLAYER_ENTITY_ID,
                    username,
                    crate::vitals::DeathCause::Fall,
                    advancements,
                    player_uuid,
                    Some(crate::vitals::HurtDirection::PURE_ROLL),
                )
                .await?;
            }
        }
        // A player turning on the spot sends `move_player_rot`
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
            if abilities.flying {
                fall.reset();
            }
            client_movement.observe(Vec3::new(0.0, 0.0, 0.0), on_ground);
            *player_rot = Some(Rotation { yaw, pitch });
            fall_status_sample(
                conn,
                state,
                proto,
                source.get(),
                player_pos,
                fall,
                vitals,
                effects,
                username,
                on_ground,
                *client_loaded,
                Abilities::for_mode(*game_mode).invulnerable,
                advancements,
                player_uuid,
            )
            .await?;
        }
        // Carries only the flags byte, so its whole job is the `on_ground`
        // edge. This records a landing even when the final movement packet
        // carries no position change.
        ServerBound::PlayerStatusOnly { on_ground } => {
            if abilities.flying {
                fall.reset();
            }
            client_movement.observe(Vec3::new(0.0, 0.0, 0.0), on_ground);
            fall_status_sample(
                conn,
                state,
                proto,
                source.get(),
                player_pos,
                fall,
                vitals,
                effects,
                username,
                on_ground,
                *client_loaded,
                Abilities::for_mode(*game_mode).invulnerable,
                advancements,
                player_uuid,
            )
            .await?;
        }
        // `Q` / `Ctrl+Q`. Vanilla refuses in spectator and nowhere else —
        // creative included, where `handleCreativeModeItemDrop` is a no-op on the
        // server and the stack really does leave the inventory.
        ServerBound::ItemDropped { whole_stack } => {
            if !matches!(*game_mode, GameMode::Spectator) {
                let directive = apply_item_dropped(
                    proto,
                    inventory,
                    open_container.as_mut(),
                    *player_pos,
                    *player_rot,
                    whole_stack,
                    drops_rng,
                    mobs,
                );
                if let Some(directive) = directive {
                    apply(conn, state, directive).await?;
                }
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
                // The block write is immediate; its light update may be queued.
                source.get(),
                state,
                pending_relights.as_deref_mut(),
                pending_break,
                block_entities,
                open_container,
                container_sync,
                mobs,
                drops_rng,
                inventory.selected_item(),
                // The breaker's feet for the interaction-range test. Use the
                // tracked `player_pos`; `None` means no movement packet exists.
                player_pos.as_ref().map(|&(x, y, z)| Vec3::new(x, y, z)),
                world,
                game_tick,
                block_ticks,
                player_uuid,
                matches!(*game_mode, GameMode::Creative),
                action,
                advancements,
                vitals,
                pos,
            )
            .await?;
        }
        ServerBound::UseItemOn {
            pos,
            face,
            cursor,
            sequence: _,
            hand,
        } => {
            // Draw one roll per right-click, regardless of the clicked block;
            // the composter branch is the only consumer of this stream.
            let roll = composter_rng.next_f64();
            // Same reasoning, `drops_rng`'s own stream: only an enchanting-table
            // open consumes this, but it is drawn unconditionally so opening one
            // does not depend on which block was clicked last.
            let enchant_seed_roll = i64::from(drops_rng.next_int(i32::MAX));
            apply_use_item_on(
                conn,
                proto,
                // The block write is immediate; its light update may be queued.
                source.get(),
                state,
                pending_relights.as_deref_mut(),
                pos,
                face,
                cursor,
                // The player's position, for the bed reach test —
                // `None` until a `PlayerMoved` packet carries one.
                player_pos.as_ref().map(|&(x, y, z)| Vec3::new(x, y, z)),
                respawn,
                // The placing player's yaw and pitch, so
                // `apply_use_item_on` can give directional blocks their
                // placement facing. `None` until a packet carrying angles
                // arrives — placement then uses the block's default state.
                player_rot.map(|rotation| rotation.yaw),
                player_rot.map(|rotation| rotation.pitch),
                *sneaking,
                player_uuid,
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
                bone_meal_rng,
                world.difficulty().0,
                *game_mode,
                enchant_seed_roll,
                hand,
                world.crafting_hooks(),
            )
            .await?;
        }
        ServerBound::DifficultyChanged { difficulty } => {
            // A difficulty change requires permission level `2`. A locked world
            // rejects the mutation, but the confirmation below is sent either
            // way with the value actually stored, keeping the client's display
            // aligned with the server.
            if commands.permission_level >= COMMANDS_GAMEMASTER_LEVEL {
                world.set_difficulty(difficulty);
            }
            apply_difficulty_change(conn, proto, state, world).await?;
        }
        ServerBound::DifficultyLockChanged { locked } => {
            // Same gate as `DifficultyChanged` above — vanilla's own
            // lock-difficulty handler checks the identical permission.
            if commands.permission_level >= COMMANDS_GAMEMASTER_LEVEL {
                world.set_difficulty_locked(locked);
            }
            apply_difficulty_change(conn, proto, state, world).await?;
        }
        ServerBound::GameRuleChanged { entries } => {
            // Vanilla's own set-game-rule handler's own gate —
            // see `DifficultyChanged`'s own comment above for why
            // `commands.permission_level` is the right check to reuse. A
            // refused request sets nothing, so `apply_game_rule_changed`'s own
            // "confirm with exactly what was set" reply is naturally empty
            // rather than needing a separate no-op branch.
            let entries = if commands.permission_level >= COMMANDS_GAMEMASTER_LEVEL {
                entries
            } else {
                Vec::new()
            };
            apply_game_rule_changed(conn, proto, state, world, entries).await?;
        }
        ServerBound::CarriedItemChanged { slot } => {
            // Switching slots cancels an in-progress bite rather than allowing it
            // to complete against the replacement item. `finish_consuming` also
            // re-checks the item because a container click can change the same
            // slot without this packet.
            *item_in_use = None;
            apply_carried_item_changed(inventory, slot);
        }
        ServerBound::ContainerClicked {
            window_id,
            state_id: _,
            slot,
            button,
            click_type,
            changed_slots,
            carried_item,
        } => {
            // A workstation result charges or refunds experience only when the
            // result is taken. Capture the input cells before dispatch because
            // the click handler mutates them; this arm applies the associated
            // experience change after the result transition is confirmed.
            let workstation_take = open_container.as_ref().and_then(|tracked| {
                let MenuKind::ItemCombiner { inputs, station } = tracked.shape else {
                    return None;
                };
                (tracked.window_id == window_id && usize::try_from(slot).ok() == Some(inputs)).then_some(station)
            });
            let pre_click_cells = workstation_take.map(|_| inventory.workstation().map(<[_]>::to_vec).unwrap_or_default());
            // Compared, not assumed dirty: a container click into the crafting
            // grid or a non-equipment slot must not spam an unchanged
            // `update_attributes`, and this is cheaper than working out from
            // `changed_slots` alone whether one of them was an armour/off-hand
            // native index.
            let attrs_before_click = player_attribute_snapshots(inventory);

            let (correction, dropped) = apply_container_clicked(
                proto,
                inventory,
                block_entities,
                open_container.as_mut(),
                window_id,
                Click {
                    slot,
                    button,
                    click_type,
                },
                &changed_slots,
                carried_item.as_ref(),
                *game_mode == GameMode::Creative,
                experience.level(),
                world.crafting_hooks(),
            );
            spawn_dropped_stacks(mobs, *player_pos, *player_rot, drops_rng, dropped);

            let mut experience_changed = false;
            if let (Some(station), Some(cells)) = (workstation_take, pre_click_cells) {
                let get = |i: usize| cells.get(i).and_then(Option::as_ref);
                // A refused take leaves the result input intact, so the
                // pre-click cells alone cannot justify an experience charge.
                // Clearing input cell 0 is the observable transition that
                // confirms a result was taken.
                let took_result = get(0).is_some()
                    && inventory
                        .workstation()
                        .and_then(<[_]>::first)
                        .is_some_and(Option::is_none);
                match station {
                    Station::Anvil => {
                        if took_result {
                            let outcome = crate::anvil::compute(get(0), get(1), inventory.pending_rename(), *game_mode == GameMode::Creative);
                            if outcome.result.is_some() && *game_mode != GameMode::Creative {
                                experience.take_levels(outcome.cost);
                                experience_changed = true;
                            }
                        }
                    }
                    Station::Grindstone => {
                        // A valid grindstone result is available whenever its
                        // inputs produce one; `took_result` also excludes a
                        // click on an empty result slot.
                        if took_result && crate::anvil::grindstone_result(get(0), get(1)).is_some() {
                            let awarded = crate::anvil::grindstone_xp(get(0), get(1), drops_rng);
                            if awarded > 0 {
                                experience.give_points(i32::try_from(awarded).unwrap_or(i32::MAX));
                                experience_changed = true;
                            }
                        }
                    }
                    // Loom, stonecutter, and smithing results do not change
                    // experience; their cost is represented by consumed inputs.
                    Station::Smithing | Station::Loom | Station::Stonecutter => {}
                }
            }
            if experience_changed {
                republish_experience(players, player_uuid, experience);
                apply(
                    conn,
                    state,
                    proto.encode_set_experience(experience.progress(), experience.level(), experience.total()),
                )
                .await?;
            }

            if let Some(correction) = correction {
                apply(conn, state, correction).await?;
            }
            // The armour bar's own packet — see `join_attributes`. A container
            // click is the other way equipment changes (drag/shift-click into
            // the armour slots, not just the right-click swap
            // `UseItemOutcome::Equipped` covers), so it needs the same resync.
            if player_attribute_snapshots(inventory) != attrs_before_click {
                apply(conn, state, join_attributes(proto, inventory)).await?;
            }
        }
        ServerBound::RecipePlaced {
            window_id,
            recipe_index,
            use_max_items,
        } => {
            if let Some(correction) = apply_recipe_placed(
                proto,
                inventory,
                open_container.as_mut(),
                window_id,
                recipe_index,
                use_max_items,
            ) {
                apply(conn, state, correction).await?;
            }
        }
        ServerBound::RecipeBookSettingsChanged {
            book_type,
            open,
            filtering,
        } => {
            inventory.set_recipe_book_settings(book_type, open, filtering);
        }
        ServerBound::RecipeBookRecipeSeen { recipe_index } => {
            if let Some(entry) = record_recipe_book_seen(inventory, recipe_index) {
                apply(conn, state, proto.encode_recipe_book_add(&[entry], false)).await?;
            }
        }
        ServerBound::SeenAdvancements { tab } => {
            let selected = advancements.select_tab(player_uuid, tab);
            apply(conn, state, proto.encode_select_advancements_tab(selected.as_deref())).await?;
        }
        ServerBound::ResourcePackResponse { id, response } => {
            resource_packs.record_response(ResourcePackResponseRecord { id, response });
        }
        ServerBound::PlayerLoaded => {
            *client_loaded = true;
        }
        ServerBound::ClientTickEnded => {
            client_movement.finish_tick();
        }
        ServerBound::PlayerAbilitiesChanged { flying } => {
            abilities.flying = (*game_mode == GameMode::Spectator || flying) && abilities.may_fly;
            if abilities.flying {
                fall.cancel();
            }
        }
        ServerBound::BlockEntityTagQuery { transaction_id, pos } => {
            if let Some(tag) = block_entity_query_tag(block_entities, commands.permission_level, pos) {
                apply(conn, state, proto.encode_tag_query(transaction_id, tag.as_ref())).await?;
            }
        }
        ServerBound::EntityTagQuery { transaction_id, entity_id } => {
            if let Some(tag) = entity_query_tag(mobs, commands.permission_level, entity_id) {
                apply(conn, state, proto.encode_tag_query(transaction_id, Some(&tag))).await?;
            }
        }
        ServerBound::ContainerClosed { window_id } => {
            // Closing returns carried items and virtual crafting/workstation
            // cells to the player's inventory; overflow becomes a dropped
            // stack so closing a menu cannot delete items.
            let mut returning = inventory.take_table_crafting();
            returning.extend(inventory.take_workstation());
            if let Some(carried) = inventory.click_state_mut().carried.take() {
                returning.push(carried);
            }
            inventory.click_state_mut().reset();
            // Bundle selection belongs to the open menu and is cleared with the
            // other menu-local scratch state.
            inventory.clear_selected_bundle_items();
            let mut spilled = Vec::new();
            let mut changed = Vec::new();
            for stack in returning {
                let (written, leftover) = inventory.add(stack);
                changed.extend(written);
                if let Some(leftover) = leftover {
                    spilled.push(leftover);
                }
            }
            changed.sort_unstable();
            changed.dedup();
            for native in changed {
                if let Some(menu_slot) = window_zero_menu_slot(native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, inventory.native(native)),
                    )
                    .await?;
                }
            }
            // A beacon payment is dropped directly rather than merged into the
            // inventory. It lives on the block entity, outside virtual menu
            // scratch storage, so read the payment field directly.
            if open_container.as_ref().is_some_and(|open| open.window_id == window_id && open.shape == MenuKind::Beacon)
                && let Some(pos) = open_container.as_ref().map(|open| open.pos)
                && let Some(payment) = block_entities.with(|reg| match reg.get_mut(pos) {
                    Some(BlockEntity::Beacon(beacon)) => beacon.payment.take(),
                    _ => None,
                })
            {
                spilled.push(payment);
            }
            spawn_dropped_stacks(mobs, *player_pos, *player_rot, drops_rng, spilled);
            if open_container.as_ref().is_some_and(|open| open.window_id == window_id) {
                // Furnace experience is paid on close from recipes accumulated
                // since the last drain. Experience orbs are not modeled here, so
                // award the points directly to the player's experience bar.
                let pos = open_container.as_ref().map(|open| open.pos);
                if let Some(pos) = pos {
                    let used = block_entities.with(|reg| match reg.get_mut(pos) {
                        Some(BlockEntity::Furnace(furnace)) => furnace.take_recipes_used(),
                        _ => std::collections::HashMap::new(),
                    });
                    if !used.is_empty() {
                        let points = crate::furnace::experience_for_recipes(&used, || {
                            drops_rng.next_f32()
                        });
                        if points > 0 {
                            experience.give_points(i32::try_from(points).unwrap_or(i32::MAX));
                            republish_experience(players, player_uuid, experience);
                            apply(
                                conn,
                                state,
                                proto.encode_set_experience(
                                    experience.progress(),
                                    experience.level(),
                                    experience.total(),
                                ),
                            )
                            .await?;
                        }
                    }
                }
                *open_container = None;
                *container_sync = ContainerSync::default();
            }
            // Any window close ends the active menu, including a merchant menu.
            // `OpenMerchant` has no window id, so clear it unconditionally.
            *open_merchant = None;
        }
        // Merchant trade-row selection. See `attempt_villager_trade`'s own
        // doc for why this executes the trade in one operation rather than through
        // a payment-slot placement flow.
        ServerBound::SelectTrade { index } => {
            if let Some(OpenMerchant { entity_id }) = *open_merchant
                && let Some(index) = usize::try_from(index).ok()
            {
                // Read back from the villager's *persistent*
                // [`crate::villager_trade::VillagerTrades`], so the charged
                // price reflects accumulated demand and this offer's
                // out-of-stock state, and is
                // derived identically to what `open_merchant_screen` sent.
                let reputation = mobs.with(|sim| sim.villager_reputation(entity_id, player_uuid));
                let hero_of_the_village_amplifier =
                    effects.amplifier_of("minecraft:hero_of_the_village");
                // A read-only priced peek, so a buyer who cannot afford it
                // never moves the villager's uses/demand — only the
                // `try_villager_trade` commit below does that, and only
                // after `attempt_villager_trade` confirms the inventory can
                // actually pay.
                let offer = mobs.with(|sim| {
                    sim.villager_offers(entity_id, reputation, hero_of_the_village_amplifier)
                        .get(index)
                        .copied()
                });
                if let Some(offer) = offer
                    && let Some(next) = attempt_villager_trade(inventory, &offer)
                    && mobs
                        .with(|sim| {
                            sim.try_villager_trade(entity_id, index, reputation, hero_of_the_village_amplifier)
                        })
                        .is_some()
                {
                    *inventory = next;
                    // A completed trade records `Trading` gossip through
                    // `record_reputation_event`, matching the villager-hit
                    // path in `MobSim::attack_from_player`.
                    mobs.with(|sim| {
                        sim.record_reputation_event(
                            entity_id,
                            crate::mobs::villager::reputation::ReputationEventType::Trade,
                            player_uuid,
                        );
                    });
                    // A full window-0 resync rather than a per-slot diff:
                    // the cost items can land anywhere across 36 slots and
                    // the given item anywhere `add` found room, so there
                    // is no fixed pair of menu slots to name — the same
                    // reasoning `join_inventory_snapshot` already
                    // documents for why this packet (not a per-slot one)
                    // is the right shape for an arbitrary multi-slot
                    // change.
                    let items = read_menu(
                        &MenuLayout::player(),
                        inventory,
                        Some(inventory.crafting()),
                        &[],
                    );
                    apply(
                        conn,
                        state,
                        proto.encode_container_content(
                            0,
                            0,
                            &items,
                            inventory.click_state().carried.as_ref(),
                        ),
                    )
                    .await?;
                }
            }
        }
        // The anvil-menu item-name setter. See `apply_rename_item`'s own doc
        // for the gate and the state resent to the client.
        ServerBound::RenameItem { name } => {
            let creative = *game_mode == GameMode::Creative;
            for directive in apply_rename_item(proto, inventory, open_container.as_mut(), &name, creative, world.crafting_hooks()) {
                apply(conn, state, directive).await?;
            }
        }
        // Command-block packets update the mode, conditional flag, command,
        // output tracking, and automatic scheduling. A redstone signal is
        // still required for execution; this packet only changes configuration.
        ServerBound::SetCommandBlock { pos, command, mode, track_output, conditional, automatic } => {
            // Creative mode and permission level `2` are both required. The
            // mode-derived ability and `commands.permission_level` are already
            // resolved on this connection, so this gate only combines them.
            let can_use_game_master_blocks =
                *game_mode == GameMode::Creative && commands.permission_level >= COMMANDS_GAMEMASTER_LEVEL;
            let is_command_block = can_use_game_master_blocks
                && block_entities.with(|reg| matches!(reg.get(pos), Some(BlockEntity::CommandBlock(_))));
            if is_command_block {
                let current_state = source.get().block_state_id(pos.x, pos.y, pos.z);
                let facing = crate::command_block::facing(current_state);
                let base = crate::command_block::base_name_for_mode(mode);
                let new_state = crate::command_block::state_with(base, facing, conditional);
                if new_state != current_state {
                    source.get().set_block(pos.x, pos.y, pos.z, new_state);
                    block_ticks.publish(pos.x, pos.y, pos.z, new_state.clone());
                }
                let new_mode = crate::command_block::mode_for_block(new_state);
                // A conditional command block checks the block directly behind
                // its facing. Read that state before taking the registry lock.
                let predecessor_succeeded = conditional.then(|| {
                    let behind = facing.opposite().relative(pos);
                    let behind_state = source.get().block_state_id(behind.x, behind.y, behind.z);
                    crate::command_block::is_command_block_family(behind_state)
                        && block_entities.with(|reg| {
                            matches!(reg.get(behind), Some(BlockEntity::CommandBlock(d)) if d.success_count > 0)
                        })
                });
                let should_schedule = block_entities.with(|reg| {
                    let Some(BlockEntity::CommandBlock(data)) = reg.get_mut(pos) else { return false };
                    data.set_command(command);
                    data.track_output = track_output;
                    if !track_output {
                        data.last_output = None;
                    }
                    let should_schedule =
                        crate::command_block::on_automatic_changed(new_mode, data.auto, automatic, data.powered);
                    data.auto = automatic;
                    if should_schedule {
                        data.condition_met =
                            crate::command_block::mark_condition_met(conditional, predecessor_succeeded);
                    }
                    should_schedule
                });
                if should_schedule {
                    block_ticks.request_scheduled_ticks(crate::command_block::ticks_after_schedule(pos));
                }
            }
        }
        // Sign updates strip legacy formatting codes from every line, then
        // `SignData` checks the wax and editor fields before writing. The editor
        // is assigned at placement (see `crate::block_entities::SignData`), so
        // each sign accepts its authorized edit.
        ServerBound::SignUpdate { pos, is_front_text, lines } => {
            let stripped = lines.map(|line| crate::block_entities::strip_sign_formatting(&line));
            block_entities.with(|registry| {
                if let Some(entity) = registry.get_mut(pos) {
                    crate::block_entities::apply_sign_update(entity, player_uuid, is_front_text, stripped);
                }
            });
        }
        // Book edits use `apply_edit_book`'s gate and resend the changed item
        // through `CONTAINER_SET_SLOT` on window `0` (the player's inventory,
        // independent of any open menu), the same "window 0,
        // state id 0" pattern every other server-initiated inventory-slot
        // write in this function already uses.
        ServerBound::EditBook { slot, pages, title } => {
            if let Some((native, item)) = apply_edit_book(inventory, slot, pages, title, username)
                && let Some(menu_slot) = window_zero_menu_slot(native)
            {
                apply(
                    conn,
                    state,
                    proto.encode_container_slot(0, 0, menu_slot, Some(&item)),
                )
                .await?;
            }
        }
        // A bundle-tooltip highlight claim. Stored, not acted on
        // immediately — `container_click::pickup`'s next right-click-on-empty
        // against this slot is what actually reads it
        // (`selected_bundle_item`); the next empty-slot pickup reads that index
        // and extracts the selected item.
        ServerBound::SelectBundleItem { slot_id, selected_item_index } => {
            let Some(slot) = MenuSlot::from_raw(slot_id) else {
                return Ok(());
            };
            let selected = BundleItemSlot::from_wire(selected_item_index);
            inventory.set_bundle_item_selection(slot, selected);
        }
        // Beacon configuration. See `apply_set_beacon`'s own doc for the gate.
        ServerBound::SetBeacon { primary, secondary } => {
            let directives =
                apply_set_beacon(proto, block_entities, open_container.as_mut(), primary, secondary);
            for directive in directives {
                apply(conn, state, directive).await?;
            }
        }
        // The enchanting table's "choose an offer" button. See
        // `apply_container_button_click`'s own doc for the pricing and
        // refusal rules.
        ServerBound::ContainerButtonClick { window_id, button_id } => {
            let creative = *game_mode == GameMode::Creative;
            let lectern = open_container
                .as_ref()
                .is_some_and(|open| open.window_id == window_id && open.shape == MenuKind::Lectern);
            if let Some(pos) = open_container
                .as_ref()
                .filter(|open| open.window_id == window_id)
                .map(|open| open.pos)
            {
                source
                    .admit_columns(column_admission_footprint(
                        pos.x.div_euclid(16),
                        pos.z.div_euclid(16),
                        1,
                    ))
                    .await?;
            }
            // Drawn unconditionally, whether or not the click succeeds — the
            // same "one draw per attempt" reasoning `apply_use_item_on`'s own
            // composter roll already documents.
            let fresh_seed = i64::from(drops_rng.next_int(i32::MAX));
            let directives = if lectern {
                apply_lectern_button_click(
                    proto,
                    block_entities,
                    inventory,
                    open_container.as_mut(),
                    window_id,
                    button_id,
                    !matches!(*game_mode, GameMode::Adventure | GameMode::Spectator),
                )
            } else {
                apply_container_button_click(
                    proto,
                    inventory,
                    open_container.as_mut(),
                    window_id,
                    button_id,
                    source.get(),
                    experience,
                    creative,
                    fresh_seed,
                    world.crafting_hooks(),
                )
            };
            // A no-op when the click was refused (`experience` untouched, so
            // this resends the same level/points it already holds) — cheaper
            // to call unconditionally than to thread a "did it actually spend
            // levels" flag out of `apply_container_button_click` just for this.
            republish_experience(players, player_uuid, experience);
            for directive in directives {
                apply(conn, state, directive).await?;
            }
            if lectern {
                // Keep the timer-driven diff baseline aligned immediately;
                // otherwise the next 50 ms tick would resend the same book
                // removal/page value after this authoritative action.
                if let Some(open) = open_container.as_ref() {
                    let (slots, data) = container_state(block_entities, open.pos);
                    container_sync.slots = slots;
                    container_sync.data = data;
                }
                republish_inventory(players, player_uuid, inventory);
            }
        }
        // A crafter's per-slot enable/disable toggle.
        // No directive to send back: `container_sync_tick`'s existing
        // `sync_open_container` diff already re-reads `data_properties()`
        // every 50ms and pushes whatever changed, the same path a furnace's
        // own background tick uses — there is nothing crafter-specific to
        // wire on the send side.
        ServerBound::ContainerSlotStateChanged { window_id, slot_id, new_state } => {
            let matching_pos = open_container
                .as_ref()
                .filter(|open| open.window_id == window_id)
                .map(|open| open.pos);
            if let Some(pos) = matching_pos
                && let Some(slot) = u8::try_from(slot_id)
                    .ok()
                    .and_then(lodestone_model::CrafterSlot::new)
            {
                block_entities.with(|reg| {
                    if let Some(entity) = reg.get_mut(pos) {
                        entity.set_crafter_slot_state_at(slot, new_state);
                    }
                });
            }
        }
        ServerBound::Attack { entity_id } => {
            // An attack packet also starts the main-hand swing animation. The
            // local client renders its own arm immediately, but every other
            // connection learns that animation through the shared swing log.
            // Record it even when the target is unknown: the wire action is a
            // swing first, while damage validation is a separate concern.
            record_attack_swing(players, player_entity_id);
            apply_attack(
                mobs,
                *player_pos,
                *sprinting,
                inventory,
                effects,
                entity_id,
                player_uuid,
            );
            // Attack exhaustion is charged on every living-target swing, not
            // only when the damage attempt lands.
            if !Abilities::for_mode(*game_mode).invulnerable {
                vitals.add_exhaustion(crate::food::EXHAUSTION_ATTACK);
            }
        }
        // The right-click interaction path covers taming, feeding, sitting,
        // breeding, and vehicle mounting through `MobSim::interact`.
        ServerBound::InteractEntity {
            entity_id,
            hand,
            using_secondary_action,
        } => {
            // Resolve only the main-hand interaction. A client can send both
            // hand values for one click; running both would roll a tame chance
            // twice.
            if hand == 0 {
                // Board boats before generic mob interaction. Boats are
                // vehicles, not tamable mobs, and require a passenger-list
                // update when boarding succeeds.
                //
                // `using_secondary_action` prevents boarding while the player
                // is sneaking.
                if mobs.with(|sim| sim.vehicle_type(entity_id).is_some()) {
                    let boarded = mobs.with(|sim| {
                        sim.mount_vehicle(entity_id, player_entity_id, using_secondary_action)
                    });
                    if boarded {
                        // Send the vehicle's **whole** passenger list rather than
                        // a delta. Without this packet the client
                        // has no way to know it is aboard and
                        // `lodestone_ecs::vehicle::tick_controlled_vehicle` never
                        // engages, so the boat is placeable and unusable.
                        //
                        // `LOCAL_PLAYER_ENTITY_ID`, not `player_entity_id`: this goes
                        // straight to `conn`, this connection's own socket, and the
                        // client only recognises itself among the passengers under
                        // the constant its own login entity-id field claimed — see
                        // `publish_health`'s call sites for the same rule.
                        // `sim.mount_vehicle` above still records the real
                        // `player_entity_id`, which is what a *future* multi-connection
                        // broadcast of this vehicle's passengers would need.
                        apply(
                            conn,
                            state,
                            proto.encode_set_passengers(entity_id, &[LOCAL_PLAYER_ENTITY_ID]),
                        )
                        .await?;
                    }
                    // Boarding consumes no item, and a refused board must not fall
                    // through to the taming chain — a boat is not tameable and the
                    // fall-through would only cost a wasted roll.
                    return Ok(());
                }
                // Minecarts are vehicles rather than tamable mobs, so handle
                // them before generic interaction.
                if let Some(kind) = mobs.with(|sim| sim.minecart_kind(entity_id)) {
                    if kind.is_furnace() {
                        // Coal and charcoal add fuel; consume one item only on
                        // a successful fuel update.
                        let held = inventory.selected_item().map(|stack| stack.item.to_string());
                        if let Some(item) = held {
                            let interacting_pos = player_pos.map_or_else(
                                || {
                                    mobs.with(|sim| sim.minecart_transform(entity_id))
                                        .map_or(Vec3::new(0.0, 0.0, 0.0), |(p, _)| p)
                                },
                                |(x, y, z)| Vec3::new(x, y, z),
                            );
                            let fuelled = mobs.with(|sim| sim.add_minecart_fuel(entity_id, &item, interacting_pos));
                            if fuelled {
                                let native = usize::from(inventory.selected_hotbar_slot());
                                if consume_one(inventory, native, *game_mode) {
                                    let hotbar_slot = i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
                                    apply(
                                        conn,
                                        state,
                                        proto.encode_container_slot(0, 0, hotbar_slot, inventory.native(native)),
                                    )
                                    .await?;
                                }
                            }
                        }
                    } else if kind.is_rideable() && !using_secondary_action {
                        // Mount the minecart and send the same passenger-list
                        // handoff used by the boat arm above.
                        let boarded = mobs.with(|sim| sim.mount_minecart(entity_id, player_entity_id));
                        if boarded {
                            apply(
                                conn,
                                state,
                                proto.encode_set_passengers(entity_id, &[LOCAL_PLAYER_ENTITY_ID]),
                            )
                            .await?;
                        }
                    }
                    // Chest, hopper, and TNT minecarts have no modeled
                    // interaction; their slots have no menu wired to them.
                    return Ok(());
                }
                let held = inventory.selected_item().map(|stack| stack.item.clone());
                // Leash handling precedes taming, feeding, and breeding. A lead
                // in hand attaches or detaches a leash without rolling another
                // interaction.
                let leash_outcome = mobs.with(|sim| {
                    sim.try_leash(
                        entity_id,
                        player_uuid,
                        held.as_ref().is_some_and(|item| item.to_string() == "minecraft:lead"),
                        *game_mode == GameMode::Creative,
                    )
                });
                let outcome = match leash_outcome {
                    crate::mobs::LeashOutcome::Attached => {
                        // Consume one item through the same `consume_one` and
                        // window-0 synchronization used by other interactions.
                        let native = usize::from(inventory.selected_hotbar_slot());
                        if consume_one(inventory, native, *game_mode) {
                            let hotbar_slot = i32::from(inventory.selected_hotbar_slot())
                                + WINDOW_ZERO_HOTBAR_FIRST;
                            apply(
                                conn,
                                state,
                                proto.encode_container_slot(
                                    0,
                                    0,
                                    hotbar_slot,
                                    inventory.native(native),
                                ),
                            )
                            .await?;
                        }
                        None
                    }
                    // `MobSim::try_leash` spawns a dropped lead when required;
                    // this arm has no additional work.
                    crate::mobs::LeashOutcome::Detached { .. } => None,
                    // A non-leashable, out-of-range, or already-owned target
                    // falls through to the ordinary mob interaction.
                    crate::mobs::LeashOutcome::Refused => Some(mobs.with(|sim| {
                        sim.interact(
                            entity_id,
                            PlayerIdentity {
                                uuid: player_uuid,
                                entity_id: player_entity_id,
                            },
                            held.as_ref(),
                        )
                    })),
                };
                // A villager trade outcome opens its screen before generic
                // item-consumption handling; opening the screen is the visible
                // effect and requires no slot synchronization.
                if let Some(crate::mobs::InteractOutcome::OpenTrade { level, .. }) = outcome {
                    let xp = mobs.with(|sim| sim.villager_xp(entity_id));
                    let reputation = mobs.with(|sim| sim.villager_reputation(entity_id, player_uuid));
                    let hero_of_the_village_amplifier =
                        effects.amplifier_of("minecraft:hero_of_the_village");
                    // The villager's *persistent* offer list. The mob supplies
                    // its live profession and level through
                    // `MobSim::villager_offers`.
                    let offers =
                        mobs.with(|sim| sim.villager_offers(entity_id, reputation, hero_of_the_village_amplifier));
                    open_merchant_screen(conn, proto, state, &offers, level, xp, next_window_id).await?;
                    // Record which villager this connection is trading with.
                    // Each open replaces the connection's active merchant
                    // screen and its associated entity id.
                    *open_merchant = Some(OpenMerchant { entity_id });
                }
                // A successful mount is recorded in `MobSim`; send the complete
                // passenger list so the client learns that it is aboard.
                if outcome == Some(crate::mobs::InteractOutcome::Mounted) {
                    apply(
                        conn,
                        state,
                        proto.encode_set_passengers(entity_id, &[LOCAL_PLAYER_ENTITY_ID]),
                    )
                    .await?;
                }
                // `consume_one` handles creative mode. A sit toggle has no item
                // cost, as encoded by `InteractOutcome::consumes_item`.
                //
                // `consume_one` handles the creative case itself, so the game mode
                // goes to it rather than being checked here — and the
                // `encode_container_slot` **is not optional**: without it the server
                // and client disagree about the stack count, which is a worse bug
                // than not consuming at all (the next click sends a stale count and
                // the item appears to come back).
                if let Some(outcome) = outcome
                    && outcome.consumes_item()
                {
                    let native = usize::from(inventory.selected_hotbar_slot());
                    if consume_one(inventory, native, *game_mode) {
                        let hotbar_slot =
                            i32::from(inventory.selected_hotbar_slot()) + WINDOW_ZERO_HOTBAR_FIRST;
                        apply(
                            conn,
                            state,
                            proto.encode_container_slot(
                                0,
                                0,
                                hotbar_slot,
                                inventory.native(native),
                            ),
                        )
                        .await?;
                    }
                }
            }
        }
        // The player's projectile-launch path. A successful bow use creates the
        // projectile record consumed by the entity stream.
        ServerBound::UseItem { hand, yaw, pitch, .. } => {
            // Handle boat items before the eat/equip chain. A boat is neither
            // food nor equippable, and its raytrace needs the world source.
            //
            // This branch supplies the world source required by the raytrace;
            // `apply_use_item` receives only inventory, position, and game mode.
            // The eye height comes from the tracked feet position; without one,
            // the launch arm refuses to guess.
            let boat_native = if hand == 1 {
                crate::inventory::OFFHAND_NATIVE
            } else {
                usize::from(inventory.selected_hotbar_slot())
            };
            let boat_item = inventory
                .native(boat_native)
                .map(|stack| stack.item.to_string());
            if let (Some(item), Some((px, py, pz))) = (boat_item.as_deref(), *player_pos) {
                let applied = crate::boat::apply_boat_item(
                    item,
                    Vec3::new(px, py + EYE_HEIGHT, pz),
                    yaw,
                    pitch,
                    crate::boat::block_interaction_range(*game_mode == GameMode::Creative),
                    &|x, y, z| source.get().block_state_id(x, y, z),
                    mobs,
                );
                match applied {
                    crate::boat::BoatApplied::NotABoat => {}
                    // The raytrace missed or the hull would not fit. Nothing is
                    // consumed and the item does not fall through to eat/equip.
                    crate::boat::BoatApplied::Refused => return Ok(()),
                    crate::boat::BoatApplied::Placed { .. } => {
                        // Consume one item after the boat is placed. Creative
                        // players keep their boats; survival players lose one.
                        if consume_one(inventory, boat_native, *game_mode)
                            && *game_mode != GameMode::Creative
                        {
                            // Publish the window-0 slot value so the client count
                            // stays synchronized for the next click.
                            if let Some(menu_slot) = window_zero_menu_slot(boat_native) {
                                let remainder = inventory.native(boat_native).cloned();
                                apply(
                                    conn,
                                    state,
                                    proto.encode_container_slot(
                                        0,
                                        0,
                                        menu_slot,
                                        remainder.as_ref(),
                                    ),
                                )
                                .await?;
                            }
                        }
                        // A placement ends any draw or bite in progress.
                        *bow_draw = None;
                        *item_in_use = None;
                        return Ok(());
                    }
                }
            }
            // An eye of ender in the air (not aimed at a frame) flies toward the
            // nearest stronghold. Before `apply_use_item`, which has no world
            // source to locate one with.
            if let Some((px, py, pz)) = *player_pos
                && inventory
                    .native(boat_native)
                    .is_some_and(|stack| stack.item.path() == "ender_eye")
            {
                let sound_roll = drops_rng.next_f32();
                let sound = launch_eye_of_ender(
                    mobs,
                    inventory,
                    boat_native,
                    *game_mode,
                    Vec3::new(px, py, pz),
                    yaw,
                    pitch,
                    source.dimension(),
                    &|x, y, z| source.get().block_state_id(x, y, z),
                    &|from| source.get().locate_stronghold(from),
                    sound_roll,
                );
                if let Some(sound) = sound {
                    block_ticks.publish_effect(sound);
                    if *game_mode != GameMode::Creative
                        && let Some(menu_slot) = window_zero_menu_slot(boat_native)
                    {
                        let remainder = inventory.native(boat_native).cloned();
                        apply(
                            conn,
                            state,
                            proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                        )
                        .await?;
                    }
                }
                *bow_draw = None;
                *item_in_use = None;
                return Ok(());
            }
            let outcome = apply_use_item(
                mobs,
                effects,
                inventory,
                *player_pos,
                *client_movement,
                *game_mode,
                vitals.food().food_level(),
                Abilities::for_mode(*game_mode).invulnerable,
                hand,
                yaw,
                pitch,
                player_entity_id,
            );
            // Both state slots are reset for each `USE_ITEM`: a chargeable item
            // starts a new draw or bite, while another item cancels any active
            // use.
            *bow_draw = None;
            *item_in_use = None;
            match outcome {
                UseItemOutcome::Nothing => {}
                UseItemOutcome::Draw(draw) => *bow_draw = Some(draw),
                UseItemOutcome::Consuming(started) => *item_in_use = Some(started),
                UseItemOutcome::Equipped(swap) => {
                    // Every slot the swap touched, so the client's own prediction
                    // is corrected rather than left to drift. The armour slots are
                    // menu `5..=8` in window 0 (`window_zero_menu_slot`), which is
                    // what makes the piece show up in the player's own inventory
                    // screen and on the player model. It does **not** touch the
                    // armour *bar* — that reads `update_attributes`, sent
                    // separately below.
                    let mut touched = vec![swap.equipment.0, swap.hand.0];
                    touched.extend(swap.inventory.iter().copied());
                    for native in touched {
                        let Some(menu_slot) = window_zero_menu_slot(native) else {
                            continue;
                        };
                        let held = inventory.native(native).cloned();
                        apply(
                            conn,
                            state,
                            proto.encode_container_slot(0, 0, menu_slot, held.as_ref()),
                        )
                        .await?;
                    }
                    // The armour bar's own packet — see `join_attributes`. A
                    // right-click equip is exactly the mutation this resync
                    // exists for: `swap.equipment` is always one of the four
                    // armour slots or the off-hand.
                    apply(conn, state, join_attributes(proto, inventory)).await?;
                    // A full inventory sends the displaced equipment to the
                    // world as a dropped stack.
                    if let Some(spilled) = swap.spilled {
                        spawn_dropped_stacks(
                            mobs,
                            *player_pos,
                            *player_rot,
                            drops_rng,
                            vec![spilled],
                        );
                    }
                }
            }
        }
        ServerBound::ReleaseUseItem => {
            // A release before the consume clock expires cancels the use with
            // no food applied.
            *item_in_use = None;
            if let Some(draw) = bow_draw.take() {
                let fired = apply_release_use_item(
                    mobs,
                    inventory,
                    *player_pos,
                    *client_movement,
                    *player_rot,
                    *game_mode,
                    draw,
                );
                // Bow shots have no exhaustion cost, so this arm charges none.
                let _ = fired;
            }
        }
        // The `F`-key hand swap. See `ServerBound::SwapItemInHand`'s own doc
        // comment for why both directives below are the only place either
        // slot's new contents ever reaches the client — there is no local
        // client prediction to correct, unlike `RenameItem`/`EditBook`'s
        // resends just above.
        ServerBound::SwapItemInHand => {
            let main_native = usize::from(inventory.selected_hotbar_slot());
            let main_item = inventory.native(main_native).cloned();
            let off_item = inventory.native(OFFHAND_NATIVE).cloned();
            inventory.set_native(main_native, off_item.clone());
            inventory.set_native(OFFHAND_NATIVE, main_item.clone());
            if let Some(menu_slot) = window_zero_menu_slot(main_native) {
                apply(
                    conn,
                    state,
                    proto.encode_container_slot(0, 0, menu_slot, off_item.as_ref()),
                )
                .await?;
            }
            if let Some(menu_slot) = window_zero_menu_slot(OFFHAND_NATIVE) {
                apply(
                    conn,
                    state,
                    proto.encode_container_slot(0, 0, menu_slot, main_item.as_ref()),
                )
                .await?;
            }
        }
        // The steering packet is an authoritative report from the client. Store
        // its position and yaw in the boat snapshot so every viewer's
        // `move_entity` diff follows.
        //
        // `apply_vehicle_move` resolves the vehicle from this player rather than
        // an id on the wire, so a connection cannot drag a boat it is not riding.
        ServerBound::VehicleMoved {
            position,
            yaw,
            pitch,
        } => {
            // Pitch is decoded and dropped because vehicle movement stores only
            // position and yaw. Keep the binding named so the decoded field is
            // visible at this call site.
            let _ = pitch;
            // A player occupies at most one vehicle map. Try the mob map only
            // when the vehicle map refused, so the shared movement packet uses
            // the appropriate mounted entity.
            mobs.with(|sim| {
                if sim
                    .apply_vehicle_move(player_entity_id, position, yaw)
                    .is_none()
                {
                    sim.apply_mob_move(player_entity_id, position, yaw);
                }
            });
        }
        // `PADDLE_BOAT` is purely cosmetic (see
        // `MobSim::apply_boat_paddle`'s own doc) so there is no directive to
        // send here; the next `snapshots()` diff carries it to every other
        // connected client via `MetadataField::BoatPaddles`.
        ServerBound::PaddleBoat { left, right } => {
            mobs.with(|sim| {
                sim.apply_boat_paddle(player_entity_id, left, right);
            });
        }
        ServerBound::PlayerInput { sprint, shift, jump } => {
            *sprinting = sprint;
            *sneaking = shift;
            // A jump request starts the camel dash when the mount accepts it.
            if jump {
                mobs.with(|sim| sim.trigger_camel_dash(player_entity_id));
            }
            // The client sends a true shift bit on the input edge. Try the
            // vehicle, minecart, and mob mounts in sequence; only one can carry
            // this player at a time.
            if shift {
                let rotation = player_rot.unwrap_or_default();
                let terrain = source.get();
                let dismounted = mobs.with(|sim| {
                    if let Some(vehicle_id) = sim.vehicle_ridden_by(player_entity_id) {
                        let position = sim.vehicle_dismount_position(
                            vehicle_id,
                            rotation.yaw,
                            &|x, y, z| terrain.block_state_id(x, y, z),
                        );
                        sim.dismount_rider(player_entity_id)
                            .map(|id| (id, position))
                    } else {
                        sim.dismount_minecart_rider(player_entity_id)
                            .or_else(|| sim.dismount_mob(player_entity_id))
                            .map(|id| (id, None))
                    }
                });
                if let Some((vehicle_id, dismount_position)) = dismounted {
                    // Send the vehicle's complete, empty passenger list.
                    apply(conn, state, proto.encode_set_passengers(vehicle_id, &[])).await?;
                    if let Some(position) = dismount_position {
                        // Apply the authoritative dismount location locally and
                        // send it before processing the next movement delta.
                        *player_pos = Some((position.x, position.y, position.z));
                        *player_rot = Some(rotation);
                        apply(
                            conn,
                            state,
                            proto.encode_teleport_with_id(
                                issue_teleport_id(teleport_acknowledgements),
                                position.x,
                                position.y,
                                position.z,
                                rotation.yaw,
                                rotation.pitch,
                            ),
                        )
                        .await?;
                    }
                }
            }
        }
        ServerBound::CreativeModeSlotSet { slot, item } => {
            apply_creative_mode_slot_set(inventory, slot, item, *game_mode == GameMode::Creative);
        }
        ServerBound::ClientCommand { action } => {
            apply_client_command(
                conn,
                proto,
                state,
                vitals,
                burn,
                fall,
                teleport_acknowledgements,
                world_spawn,
                respawn,
                source.get(),
                home.get(),
                world,
                advancements,
                player_uuid,
                action,
                commands.permission_level,
                client_loaded,
                dimension_reset,
                end_exit,
                *game_mode,
                block_ticks,
            )
            .await?;
        }
        ServerBound::ClientInformationChanged { view_distance } => {
            // **No host-side clamp here.** `ViewTracker::set_view_radius` applies
            // the server's configured ceiling, stored in `ViewTracker::max_radius`.
            // The connection's requested distance can therefore shrink or grow
            // within that ceiling during a session.
            // Read the current radius so a changed value can move the player's
            // ticket to the requested view centre.
            let radius_before_resize = view.radius;
            let update = view.set_view_radius(
                proto,
                source,
                i32::from(view_distance),
                player_rot.map(|rotation| rotation.yaw),
            );
            if view.radius != radius_before_resize {
                player_ticket_guard.move_to_with_simulation_radius(
                    view.center,
                    view.radius,
                    view.radius.clamp(0, crate::chunk_store::CONCURRENT_TICK_RADIUS),
                );
                source.get().reconcile_ticket_residency();
            }
            send_view_update(
                conn,
                proto,
                source,
                join_stream.as_deref_mut(),
                state,
                view,
                update,
                awaiting_chunk_batch_ack,
                pending_chunk_batches,
            )
            .await?;
        }
        ServerBound::ChunkBatchAcknowledged { .. } => {
            *awaiting_chunk_batch_ack = false;
            if let Some(next) = pending_chunk_batches.pop_front() {
                *awaiting_chunk_batch_ack = true;
                send_pending_chunk_batch(conn, proto, state, view, next).await?;
            }
        }
        // Chat commands run through the built-in tree, with host dispatch as
        // the fallback. Effects for this connection are applied inline; effects
        // targeting another player enter that player's effect queue. The
        // resolved permission level gates both command execution and completion,
        // while an absent host dispatcher fails closed.
        ServerBound::ChatCommand { command } => {
            // Captured before the `source` binding below shadows the chunk
            // source with the command's own `CommandSource` — `Effect::SetBlock`/
            // `Fill` need the former and nothing else in this arm has a name for
            // it once the shadow takes effect.
            let chunk_source = source;
            // The connection may already be in the Nether or End.  Keep that
            // live source dimension in the command stack so `/execute ... run`
            // passes the actual context on to a host dispatcher rather than
            // silently manufacturing an overworld context.
            let command_dimension = chunk_source
                .dimension()
                .key()
                .parse()
                .expect("server dimensions always have valid resource keys");
            // The roster the command's selectors resolve against.
            //
            // With no registry — singleplayer, where `open_in_memory` builds no
            // `PlayerRegistry` at all — the caller is synthesised as the sole
            // candidate. That is not a courtesy: without it `@s` resolves to
            // nothing and `/gamemode creative` fails in single-player, which is
            // the single most common use of the command.
            let mut candidates = players.map(PlayerRegistry::candidates).unwrap_or_default();
            let position = player_pos
                .map_or(world_spawn, |(x, y, z)| Vec3::new(x, y, z));
            if !candidates.iter().any(|c| c.uuid == player_uuid) {
                candidates.push(crate::commands::PlayerCandidate {
                    uuid: player_uuid,
                    entity_id: player_entity_id,
                    username: username.to_owned(),
                    position,
                    rotation: player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 }),
                    game_mode: *game_mode,
                    // No registry to have republished into — this connection
                    // *is* the one live source, read directly rather than
                    // through the mirror `set_experience` maintains for
                    // everyone else's roster entry.
                    xp_level: experience.level(),
                    xp_points: experience.query_points(),
                });
            }
            let respawn_dimension = source.dimension();
            let source = crate::commands::CommandSource::player(
                player_uuid,
                player_entity_id,
                username,
                position,
                player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 }),
                command_dimension,
                commands.permission_level,
            );
            let command_world = crate::commands::CommandWorld {
                rules: world,
                players: &candidates,
                state: world,
                // `/summon`'s synchronous spawn entry point — the same shared
                // `MobHandle` `dispatch_play_packet` already holds, so a
                // spawned mob is picked up by the tick loop's own next
                // publish (see `crate::commands::summon`'s module doc).
                mobs: Some(mobs),
                // `/worldborder`'s read/write surface — the same
                // shared `BorderFeed` this connection already holds.
                border: Some(border),
                // No access list is attached to packet dispatch, so access
                // management commands return the fail-closed refusal. RCON
                // supplies the access handle when those commands are needed.
                #[cfg(not(target_arch = "wasm32"))]
                access: None,
                // `/execute if`/`unless block`'s read-only surface — the same
                // `chunk_source` captured above `Effect::SetBlock`/`Fill`
                // already reach through this arm's own `apply_own_effect`.
                blocks: Some(chunk_source.get()),
            };
            match commands.builtins.run_with_contextual_dispatch(
                &command_world,
                &source,
                &command,
                &commands.dispatch,
                &commands.caller,
            ) {
                Some(outcome) => {
                    // Command effects are still one ordered packet action, but
                    // their block coordinates are resolved only after the
                    // command tree runs. Admit every owned target and its
                    // light neighbours before applying the effects so a
                    // `/setblock` or `/fill` cannot re-enter cold terrain on
                    // the connection task.
                    let mut effect_columns = HashSet::new();
                    for directed in &outcome.effects {
                        if directed.target != player_uuid {
                            continue;
                        }
                        let mut admit = |x: i32, z: i32| {
                            effect_columns.extend(column_admission_footprint(
                                x.div_euclid(16),
                                z.div_euclid(16),
                                1,
                            ));
                        };
                        match &directed.effect {
                            crate::commands::Effect::SetBlock { pos: (x, _y, z), .. } => {
                                admit(*x, *z);
                            }
                            crate::commands::Effect::Fill { positions, .. } => {
                                for &(x, _y, z) in positions {
                                    admit(x, z);
                                }
                            }
                            _ => {}
                        }
                    }
                    if !effect_columns.is_empty() {
                        chunk_source
                            .admit_columns(effect_columns.into_iter().collect())
                            .await?;
                    }
                    for directed in outcome.effects {
                        if directed.target != player_uuid {
                            if let Some(registry) = players {
                                registry.push_effect(directed.target, directed.effect);
                            }
                            continue;
                        }
                        // World/broadcast effects are always self-targeted for
                        // delivery only (see `crate::commands::Effect`'s own doc)
                        // and applied here, inline, because this is the only
                        // place with `chunk_source`/`block_ticks`/the player
                        // registry/`respawn` all in scope. Everything else is a
                        // genuine per-player effect and goes through
                        // `apply_own_effect`.
                        match directed.effect {
                            crate::commands::Effect::SetBlock { pos: (x, y, z), block } => {
                                chunk_source.get().set_block(x, y, z, block);
                                block_ticks.publish(x, y, z, block);
                            }
                            crate::commands::Effect::Fill { positions, block } => {
                                for (x, y, z) in positions {
                                    chunk_source.get().set_block(x, y, z, block);
                                    block_ticks.publish(x, y, z, block);
                                }
                            }
                            crate::commands::Effect::Broadcast { sender, message } => {
                                if let Some(registry) = players {
                                    registry.say(&sender, &message);
                                } else {
                                    // Singleplayer builds no registry at all —
                                    // the same fallback the `@s`-synthesis above
                                    // uses. Rendered identically to
                                    // `ChatLine::rendered` so a `/say` reads no
                                    // differently than ordinary chat would.
                                    apply(
                                        conn,
                                        state,
                                        proto.encode_system_chat(&format!("<{sender}> {message}")),
                                    )
                                    .await?;
                                }
                            }
                            crate::commands::Effect::SetRespawnPoint { pos } => {
                                *respawn = Some(RespawnPoint::forced(
                                    pos,
                                    respawn_dimension,
                                    0.0,
                                    0.0,
                                ));
                            }
                            other => {
                                apply_own_effect(
                                    conn,
                                    proto,
                                    state,
                                    game_mode,
                                    abilities,
                                    inventory,
                                    players,
                                    player_uuid,
                                    other,
                                    advancements,
                                    world,
                                    effects,
                                    vitals,
                                    experience,
                                    player_entity_id,
                                    username,
                                    player_pos,
                                    player_rot,
                                    teleport_acknowledgements,
                                )
                                .await?;
                            }
                        }
                    }
                    for line in outcome.response.chat_lines() {
                        apply(conn, state, proto.encode_system_chat_component(&line)).await?;
                    }
                }
                // No built-in root matched: delegate the command to the host
                // dispatcher.
                None => {
                    let response = if commands.dispatch.is_installed() {
                        let caller = commands.plugin_caller();
                        commands.dispatch.run(&caller, &command)
                    } else {
                        commands.dispatch.run(&commands.caller, &command)
                    };
                    for line in response.chat_lines() {
                        apply(conn, state, proto.encode_system_chat_component(&line)).await?;
                    }
                }
            }
        }
        // A tab-completion request. See `ServerBound::CommandSuggestion`'s own
        // doc comment for the wire shape and
        // `crate::commands::ServerCommands::suggest_response` for the
        // start/length arithmetic and the `/`-stripping this delegates to it,
        // gated by `commands.permission_level` — the same resolved-once level
        // `ChatCommand` above uses. A host registry is consulted only when the
        // built-in tree has no visible candidate, preserving root precedence
        // while making plugin completions reach the same production path as
        // plugin execution.
        ServerBound::CommandSuggestion { id, command } => {
            let mut response =
                commands.builtins.suggest_response(id, &command, commands.permission_level);
            // Built-ins retain precedence. If they have no visible completion,
            // ask the host's permission-aware registry for the plugin roots and
            // branches that the same authenticated caller may use. The helper
            // above already computed the wire token range, so plugin results
            // cannot disagree with client replacement offsets.
            if response.suggestions.is_empty() && commands.dispatch.is_installed() {
                let caller = commands.plugin_caller();
                response.suggestions = commands
                    .dispatch
                    .suggest(&caller, &command)
                    .into_iter()
                    .map(|text| CommandSuggestionEntry { text, tooltip: None })
                    .collect();
            }
            apply(conn, state, proto.encode_command_suggestions(&response)).await?;
        }
        // A game-mode request is answered with directives for the mode the
        // server accepted. Permission level 2 is required for the change.
        ServerBound::ChangeGameMode { mode } => {
            if commands.permission_level >= COMMANDS_GAMEMASTER_LEVEL {
                *game_mode = mode;
            }
            for directive in game_mode_directives(proto, *game_mode, abilities) {
                apply(conn, state, directive).await?;
            }
        }
        // Spectator teleport resolves connected players only, preserves the
        // requester's facing, and ignores requests outside spectator mode or
        // without a matching player. The resulting position uses the normal
        // teleport effect path.
        ServerBound::TeleportToEntity { uuid } => {
            if *game_mode == GameMode::Spectator
                && let Some(target) = players
                    .map(PlayerRegistry::candidates)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|c| c.uuid == uuid)
            {
                let current = player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 });
                *player_pos = Some((target.position.x, target.position.y, target.position.z));
                *player_rot = Some(current);
                let directive = proto.encode_teleport_with_id(
                    issue_teleport_id(teleport_acknowledgements),
                    target.position.x,
                    target.position.y,
                    target.position.z,
                    current.yaw,
                    current.pitch,
                );
                apply(conn, state, directive).await?;
            }
        }
        // An arm swing. See `ServerBound::Swing`'s own doc comment for why
        // this pushes to the shared broadcast log rather than replying
        // directly (same "every connection reads it on its own drain" shape
        // as `Chat` below), and for why the log's own reader excludes the
        // sender. Singleplayer has no registry and therefore nobody else to
        // tell, so this is silently a no-op there.
        ServerBound::Swing { hand } => {
            if let Some(registry) = players {
                registry.swing(player_entity_id, hand);
            }
        }
        // A spectator can attach its camera to a nearby entity when
        // `apply_spectator_action` accepts the target. Invalid or out-of-range
        // requests are ignored and produce no failure reply.
        ServerBound::SpectatorAction { target_entity_id } => {
            if let Some(target_id) =
                apply_spectator_action(*game_mode, target_entity_id, *player_pos, mobs, players)
            {
                apply(conn, state, proto.encode_set_camera(target_id)).await?;
            }
        }
        // Chat is placed in the shared broadcast queue; each connection drains
        // that queue, including the sender, on its normal outgoing pass.
        //
        // Empty messages are malformed and are dropped rather than broadcast.
        //
        // `crate::chat_session::decide` verifies the message before broadcast.
        // A rejection is sent to the sender and never enters `outgoing_chat`.
        ServerBound::Chat {
            message,
            timestamp_millis,
            salt,
            signature,
        } => {
            if !message.trim().is_empty() {
                let decision = crate::chat_session::decide(
                    chat_session,
                    player_uuid,
                    enforce_secure_profile,
                    signature.as_ref().map(|s| s.as_slice()),
                    &message,
                    timestamp_millis,
                    salt,
                    crate::chat_session::now_millis(),
                );
                match decision {
                    crate::chat_session::ChatDecision::Accept { .. } => {
                        outgoing_chat.push(message);
                    }
                    crate::chat_session::ChatDecision::Reject { reason } => {
                        apply(
                            conn,
                            state,
                            proto.encode_system_chat(&format!("Your message was not sent: {reason}")),
                        )
                        .await?;
                    }
                }
            }
        }
        // A session announcement replaces the connection's session; verification
        // reads the session data supplied by that announcement.
        ServerBound::ChatSessionAnnounced {
            session_id,
            expires_at_millis,
            public_key,
            key_signature,
        } => {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let data = lodestone_auth::ProfilePublicKeyData {
                    // `player_uuid` is the identity the session server returned
                    // after `hasJoined`, never the UUID the client claimed in
                    // LoginStart. Mojang signs this exact UUID into the
                    // certificate payload.
                    profile_id: player_uuid,
                    expires_at_millis,
                    public_key_der: public_key,
                    key_signature,
                };
                if let Some(session) = crate::chat_session::adopt_announced_session(
                    profile_key_issuers,
                    player_uuid,
                    session_id,
                    data,
                ) {
                    if chat_session
                        .as_ref()
                        .is_some_and(|current| session.expires_before(current))
                    {
                        let directive = proto.encode_disconnect(
                            *state,
                            &expired_profile_public_key_reason(),
                        );
                        apply(conn, state, directive).await?;
                        return Err(ServerError::ProfilePublicKeyRollback);
                    }
                    *chat_session = Some(session);
                } else if profile_key_issuers.is_some() {
                    // With an issuer set, an invalid update clears the active
                    // session. Without one, retain the active session.
                    *chat_session = None;
                }
            }
            #[cfg(target_arch = "wasm32")]
            {
                // Browser integrated play has no online-authentication or
                // Mojang issuer service. Match the unavailable-service native
                // path: ignore the untrusted announcement and retain the
                // existing session rather than installing a self-asserted key.
                let _ = (session_id, expires_at_millis, public_key, key_signature);
            }
        }
        // Plugin register/unregister channels update this connection's supported
        // set; other channels go to their registered handler or are dropped.
        ServerBound::CustomPayload { channel, data } => {
            if !client_channels.apply_custom_payload(&channel, &data) {
                plugin_channels.dispatch(&channel, &data);
            }
        }
        // `PlayerCommand` action 0 is `STOP_SLEEPING` — the "wake
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
        // `ServerboundPingRequestPacket` shares one wire struct across Status
        // and Play (see the decode arm's own comment), so `PingRequest` reaches
        // here too, unlike its `Handshake`/`LoginStart`/etc. siblings below.
        // Vanilla's own ping-request handler is exactly "echo the
        // time back" — the same body the Status-state arm above uses, minus the
        // connection close, since a Play-state ping must not end the session.
        ServerBound::PingRequest { time } => {
            apply(conn, state, proto.encode_pong_response(time)).await?;
        }
        // `Pong` is the reply to the server-originated `ping` control packet.
        // The hosted protocol has no ping producer or pending-id state, so a
        // valid reply deliberately produces no packet or state mutation.
        // Keeping it distinct from `Ignored` makes that accepted no-op
        // boundary explicit without inventing acknowledgement bookkeeping.
        ServerBound::Pong { id } => {
            let _ = id;
        }
        // Middle-click selection uses `crate::item_use::try_pick_item` for the
        // inventory destination and slot rules. This arm resolves the clicked
        // block's clone stack and checks interaction range and live block state.
        // `include_data` is ignored because this crate has no consumer that
        // copies block-entity data onto the selected item.
        ServerBound::PickItemFromBlock { pos, include_data: _ } => {
            let feet = player_pos.map(|(x, y, z)| Vec3::new(x, y, z));
            if crate::block_breaking::within_interaction_range(feet, pos) {
                let block_state = source.get().block_state_id(pos.x, pos.y, pos.z);
                if let Some(stack) = crate::item_use::clone_item_stack_for_block(block_state) {
                    let creative = *game_mode == GameMode::Creative;
                    let outcome = crate::item_use::try_pick_item(inventory, stack, creative);
                    apply(conn, state, proto.encode_set_held_slot(outcome.selected)).await?;
                    for native in outcome.changed {
                        if let Some(menu_slot) = window_zero_menu_slot(native) {
                            apply(
                                conn,
                                state,
                                proto.encode_container_slot(0, 0, menu_slot, inventory.native(native)),
                            )
                            .await?;
                        }
                    }
                }
            }
        }
        // The entity-pick request uses the same split, aimed at the entity's
        // derived item result instead of a block's clone stack. Only the
        // `Mob` override (a spawn egg) is modelled; see
        // `crate::item_use::spawn_egg_for_entity_type`'s doc comment for the
        // entities this refuses. `include_data` also gates a game-master
        // avatar-profile debug command in vanilla (`FetchProfileCommand`),
        // which this crate has no command channel for, so it is unread here
        // too.
        ServerBound::PickItemFromEntity { entity_id, include_data: _ } => {
            let target = mobs.with(|sim| {
                sim.get(entity_id).map(|mob| (mob.entity_type().to_string(), mob.position()))
            });
            if let Some((entity_type, entity_pos)) = target {
                let feet = player_pos.map(|(x, y, z)| Vec3::new(x, y, z));
                if crate::item_use::within_entity_pick_range(feet, entity_pos)
                    && let Some(stack) = crate::item_use::spawn_egg_for_entity_type(&entity_type)
                {
                    let creative = *game_mode == GameMode::Creative;
                    let outcome = crate::item_use::try_pick_item(inventory, stack, creative);
                    apply(conn, state, proto.encode_set_held_slot(outcome.selected)).await?;
                    for native in outcome.changed {
                        if let Some(menu_slot) = window_zero_menu_slot(native) {
                            apply(
                                conn,
                                state,
                                proto.encode_container_slot(0, 0, menu_slot, inventory.native(native)),
                            )
                            .await?;
                        }
                    }
                }
            }
        }
        // The pre-Play phase signals, unreachable here by construction: a
        // connection in `State::Play` cannot decode a handshake, a login, or
        // a Status-phase status request, because every `ServerProtocol::decode`
        // arm for those is gated on the state.
        ServerBound::Handshake { .. }
        | ServerBound::LoginStart { .. }
        // `EncryptionResponse` is `State::Login`-only too, same
        // as `LoginStart`/`LoginAcknowledged` beside it.
        | ServerBound::EncryptionResponse { .. }
        | ServerBound::LoginAcknowledged
        | ServerBound::ConfigurationFinished
        | ServerBound::StatusRequest
        | ServerBound::TeleportationAccepted { .. }
        | ServerBound::Ignored => {}
    }
    Ok(())
}

/// Converts wall-clock elapsed time into a tick count at vanilla's normal 20
/// TPS, for the `game_time` the periodic [`ServerProtocol::encode_set_time`]
/// broadcast carries.
#[cfg(not(target_arch = "wasm32"))]
fn ticks_since(start: crate::tick::PlayTimerInstant) -> i64 {
    (start.elapsed().as_millis() / MILLIS_PER_TICK) as i64
}

/// A pass through [`serve_play`]'s `select!` shorter than this is not a stall —
/// one server tick. Passes at or above it are summed into
/// [`LoopStallWatch::unserviced`] and the worst one is remembered.
#[cfg(not(target_arch = "wasm32"))]
const STALL_FLOOR: Duration = Duration::from_millis(MILLIS_PER_TICK as u64);

const STALL_REPORT: Duration = Duration::from_millis(200);

/// How long one pass through [`serve_play`]'s `select!` took, and which arm took
/// it.
///
/// # Why the connection loop needs a watchdog at all
///
/// `select!` services exactly one arm per pass, so for the whole duration of that
/// arm this connection reads nothing and writes nothing — the socket is unserviced
/// even though the task is alive and the runtime is healthy. Every arm here awaits
/// something: the longest is `dispatch_play_packet`, which for a
/// `PlayerMoved` that crosses a chunk boundary awaits
/// `ViewTracker::build_batch` over a strip of `2r + 1` columns before returning
/// anything at all.
///
/// That matters twice over, and the second is why this is a type rather than a
/// `tracing::warn!`:
///
/// * **Diagnosis.** A latency symptom needs the *maximum*, not the mean, and it
///   needs to name *where*. An average over a session cannot see a single
///   multi-second gap, and a duration with no site attached gets attributed to
///   whatever the reader already suspected.
/// * **Correctness of the keep-alive timeout.** Vanilla's `keepConnectionAlive`
///   runs on the server tick while its reads happen on a Netty IO thread that
///   never blocks on world generation, so "15 seconds elapsed" and "15 seconds in
///   which the client could have been heard" are the same number there. Here they
///   are not. Denominating the timeout in wall clock therefore lets this server
///   kick a perfectly healthy client for a reply it never gave itself the chance
///   to read — the kick arrives as `disconnect.timeout`, which reads on the client
///   as the *client's* fault. [`unserviced`](Self::unserviced) is what makes the
///   deadline mean the same thing vanilla's does.
#[cfg(not(target_arch = "wasm32"))]
struct LoopStallWatch {
    /// When the arm body currently running started. `None` between passes.
    ///
    /// **Set at the top of the arm body, not at the bottom of the previous one.**
    /// The interval between two passes is mostly time parked in `select!` waiting
    /// for a timer or a packet — the loop is *idle* there, not stalled, and the
    /// socket is being serviced by definition. Timing pass-to-pass measured
    /// exactly that idle wait: under a `start_paused` runtime, where the clock
    /// jumps straight to the next timer deadline whenever nothing is runnable, it
    /// reported the whole keep-alive interval as a stall and suppressed the
    /// timeout the test was gating. Only the arm body can starve the connection,
    /// so only the arm body is measured.
    arm_start: Option<crate::tick::PlayTimerInstant>,
    /// The longest arm body observed, and the arm that owned it. `""` until one
    /// exceeds [`STALL_FLOOR`].
    worst: Duration,
    worst_arm: &'static str,
    /// Time this loop spent unable to service the socket, summed over every arm
    /// body past [`STALL_FLOOR`]. Reset by
    /// [`clear_unserviced`](Self::clear_unserviced) when a fresh keep-alive
    /// challenge is written, so it always answers "how much of *this* challenge's
    /// window did we eat".
    unserviced: Duration,
}

#[cfg(not(target_arch = "wasm32"))]
impl LoopStallWatch {
    fn new() -> Self {
        Self {
            arm_start: None,
            worst: Duration::ZERO,
            worst_arm: "",
            unserviced: Duration::ZERO,
        }
    }

    /// Opens a pass. Called as the first statement of every `select!` arm body.
    fn enter(&mut self) {
        self.arm_start = Some(crate::tick::PlayTimerInstant::now());
    }

    /// Closes the pass that `arm` serviced. A no-op without a matching
    /// [`enter`](Self::enter), so an arm that returns early simply is not measured
    /// rather than being charged someone else's time.
    fn pass(&mut self, arm: &'static str) {
        let Some(start) = self.arm_start.take() else {
            return;
        };
        let took = start.elapsed();
        if took < STALL_FLOOR {
            return;
        }
        self.unserviced += took;
        if took > self.worst {
            self.worst = took;
            self.worst_arm = arm;
        }
        if took >= STALL_REPORT {
            tracing::warn!(
                target: "lodestone_server::stall",
                arm,
                millis = took.as_millis() as u64,
                "connection loop serviced nothing for one pass",
            );
        }
    }

    fn clear_unserviced(&mut self) {
        self.unserviced = Duration::ZERO;
    }

    /// The worst pass, for a log line on the way out. `None` before any pass has
    /// exceeded [`STALL_FLOOR`].
    fn worst(&self) -> Option<(&'static str, Duration)> {
        (!self.worst_arm.is_empty()).then_some((self.worst_arm, self.worst))
    }
}

/// Serves a connection that has just reached [`State::Play`] until the client
/// disconnects.
///
/// This is where [`serve_connection`] hands off once the join sequence and
/// initial chunk view are out: everything here runs on the connection's own
/// schedule rather than strictly in response to one inbound packet —
/// * a server-initiated keep-alive, matching vanilla's fixed 15-second
///   interval and the same-length disconnect timeout
///   (vanilla's own common packet-listener; see the
///   `KEEP_ALIVE_INTERVAL` doc comment for why that is one interval, not two);
/// * a periodic time-of-day broadcast, matching vanilla's every-20-ticks
///   cadence (its own main server loop; see `TIME_SYNC_INTERVAL`);
/// * view streaming (chunk-cache-center, forget, and send) whenever a
///   [`ServerBound::PlayerMoved`] packet crosses into a new chunk column,
///   recentering the tracked view and sending/removing columns as needed;
///
/// all layered over the same entity-streaming pass used by the join sequence;
/// the pass runs on every inbound packet.
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
/// Vitals ticks a joining client may take to report it has loaded before the
/// server treats it as loaded anyway: 60 ticks, three seconds, the same bound
/// a vanilla server applies after a join or respawn.
const CLIENT_LOADED_TIMEOUT_TICKS: u32 = 60;

/// Advances the player-loaded timeout by one vitals tick. A client that never
/// sends its loaded report (an older client, a bot, a stalled join) must not
/// hold the world's initial ticks forever. Any return to "not loaded" after a
/// respawn or dimension change restarts the wait, because the counter clears
/// whenever the client is loaded.
fn tick_client_load_timeout(client_loaded: &mut bool, waited: &mut u32) {
    if *client_loaded {
        *waited = 0;
        return;
    }
    *waited += 1;
    if *waited >= CLIENT_LOADED_TIMEOUT_TICKS {
        *client_loaded = true;
        *waited = 0;
    }
}

fn player_tick_ready(world: &crate::world_state::WorldStateHandle, client_loaded: bool) -> bool {
    if client_loaded {
        world.resume_initial_ticks();
    }
    client_loaded
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
async fn serve_play<T, P, S, E>(
    service: &crate::connection_service::ConnectionService,
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    // The primary source the connection joined from.  `source` may already
    // be a restored Nether/End sibling; portal return must still resolve back
    // through the primary world's sibling graph.
    home_source: SourceRef<'_, S>,
    entities: &E,
    mut state: State,
    initial_teleport_id: Option<i32>,
    mut streamer: EntityStreamer,
    mut player_list: PlayerListStreamer,
    // Keep the ticket guard owned by the connection task. Its `Drop` performs
    // player deregistration on disconnect, error, and cancellation; borrowing
    // it could let the guard outlive the task that owns the connection.
    player_ticket: Option<PlayerTicket>,
    // Captured with player registration so join-time swings remain visible.
    initial_swing_cursor: Option<u64>,
    // The guard withdraws this connection's `PLAYER_LOADING` and
    // `PLAYER_SIMULATION` tickets when the task exits. Move it with each
    // tracked-view recenter or radius change so residency follows the player.
    mut player_ticket_guard: PlayerTicketGuard,
    mut view: ViewTracker,
    username: String,
    // World spawn for death-screen respawn. It is computed during join and
    // reused here; `find_initial_spawn` may inspect up to 121 columns, so a
    // respawn does not repeat that search. Read by `apply_client_command`'s
    // `PERFORM_RESPAWN` arm.
    world_spawn: Vec3,
    mut chunks_sent: usize,
    // The deferred portion of the join view (`JOIN_PRESTREAM_RADIUS`) belongs
    // to this connection and is drained alongside socket reads and timers.
    mut join_stream: crate::join_scheduler::JoinChunkStream<S>,
    // Optional per-stage join timing shared with the generation workers.
    join_trace: Option<Arc<JoinTrace>>,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    // Weather transitions published by the world tick loop's
    // `WeatherState`, drained on this same timer — see that arm's comment.
    weather: &WeatherFeed,
    // The night-skip vote (see `serve_connection_inner`'s
    // parameter comment). `dispatch_play_packet` records this connection's
    // player `lay_down`/`get_up` on it, and the `container_sync_tick` arm
    // feeds it the voter count from the shared `PlayerRegistry`.
    sleep_vote: &SleepVote,
    // Where this connection learns a night skip happened — drained
    // on `container_sync_tick` into a real `encode_set_time`, same timer as
    // the weather drain (see that arm's comment).
    sleep_feed: &SleepFeed,
    // Command dispatch and the authenticated caller identity for this
    // connection.
    commands: CommandSession,
    // The connection's server-authoritative advancement/statistics
    // store, built in `serve_connection_inner` (which already sent its
    // first-packet `update_advancements` at join). Mutable because both the
    // per-packet flush below and the `REQUEST_STATS` reply in
    // `dispatch_play_packet` award into / read from it.
    mut advancements: AdvancementManager,
    // The player key this connection's advancement/statistic
    // progress is stored under — the same `login_uuid` that built
    // `CommandSession`'s caller, resolved the same way (a nil uuid fails
    // closed: the connection tracks nothing).
    player_uuid: uuid::Uuid,
    // A host-shared snapshot fetched after this connection has completed
    // online authentication. `Some` permits announcement validation;
    // `None` deliberately preserves vanilla's service-unavailable degradation.
    profile_key_issuers: Option<lodestone_auth::MojangPublicKeys>,
    // Separate from issuer availability: whether the host requires a player
    // with no adopted session to sign chat.
    enforce_secure_profile: bool,
    // Shared world-border state read by the vitals timer when calculating
    // damage. The default feed represents the full-size static border; see
    // `serve_connection_inner`'s parameter comment.
    border: &BorderFeed,
    // Server-initiated resource pack pushes, drained on
    // `container_sync_tick` — same timer as the block-tick/explosion/weather
    // drains below, for the same reason: a push is published by the host (not
    // by an inbound packet) and needs this connection's own timer to notice.
    resource_packs: &ResourcePackPushFeed,
    // The connection's declared channel support (the filter the
    // broadcast drain below applies) and the shared wire-level registry whose
    // broadcast queue that drain reads. `client_channels` is owned, not
    // borrowed: it was created here for this connection and dies with it.
    client_channels: &mut ClientChannels,
    plugin_channels: &PluginChannelRegistry,
    // The mode this connection joined in (`serve_connection_inner`'s own), owned
    // because the `change_game_mode` and `/gamemode` arms mutate it and nothing
    // outside this loop reads it.
    mut game_mode: GameMode,
    // The world's shared game rules, difficulty, and clock — the handle
    // `run_tick_loop` updates so every connection observes one state.
    world: &crate::world_state::WorldStateHandle,
    // Published once per iteration
    // of this function's own `select!` loop below — see
    // `crate::live_save::LiveSaveSlot`'s own doc comment for why a
    // continuously-refreshed mirror exists at all: `IntegratedServer::
    // shutdown`'s connection-task race drops this whole function's future
    // mid-`.await` on an ordinary singleplayer quit, so the disconnect-save
    // arm below (the `conn.read_packet()` returning `Ok(None)` branch) is
    // structurally unreachable on that path, and only this mirror survives
    // the cancellation to be read back afterwards.
    live_save: &crate::live_save::LiveSaveSlot,
    // The selected native backend's bounded locator session. Full player state
    // continues through the Anvil store above; this value only supplies the
    // native join/read and cancellation-safe locator save sidecar.
    native_player: Option<NativePlayerSession>,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    let mut pending_keep_alive: Option<i64> = None;
    let mut pending_break: Option<PendingBreak> = None;
    let mut pending_prediction_ack = connection_prediction::PendingPredictionAck::default();
    let mut pending_relights = PendingRelights::default();
    let mut pending_tick_updates = VecDeque::new();
    let mut detached_relight: Option<DetachedRelight> = None;
    let mut teleport_acknowledgements = initial_teleport_id.map(TeleportAcknowledgements::after_initial);
    let mut player_pos: Option<(f64, f64, f64)> = None;
    let mut client_movement = ClientMovement::default();
    // A protocol with no player-loaded packet is loaded from the start.
    let mut client_loaded = !proto.sends_player_loaded();
    let mut client_load_wait = 0;
    player_tick_ready(world, client_loaded);
    let mut abilities = Abilities::for_mode(game_mode);
    // The rotation is stored alongside `player_pos` — see `dispatch_play_packet`'s own
    // parameter comment. Restore the native locator's bounded rotation when
    // present; the complete Anvil player record remains authoritative for all
    // other state.
    let native_player = native_player.as_ref();
    let mut player_rot: Option<Rotation> = native_player
        .and_then(NativePlayerSession::initial_rotation);
    // Resolve the per-player store once from the source. `player_uuid` is the
    // key for the stored file, and the loop reuses this handle for its saves.
    let player_store = player_store(source.get());
    let saved_player = player_store
        .as_ref()
        .and_then(|store| store.read(player_uuid).ok().flatten());
    let native_runtime = native_player.and_then(NativePlayerSession::runtime);
    // Preserve fields `crate::player_data` does not model—hunger, experience,
    // the ender chest, and the recipe book—in every save. This keeps a full
    // load/modify/save cycle lossless; see `PlayerData::preserved`.
    let mut preserved_player_fields: Vec<(String, lodestone_core::Nbt)> =
        saved_player.as_ref().map(|d| d.preserved.clone()).unwrap_or_default();
    let mut vitals = saved_player
        .as_ref()
        .map(|data| PlayerVitals::restored(data.health, data.air_supply))
        .or_else(|| {
            native_runtime.map(|runtime| {
                PlayerVitals::restored(runtime.health, runtime.air_supply)
            })
        })
        .unwrap_or_default();
    let mut fall = FallTracker::default();
    let mut inventory = saved_player
        .as_ref()
        .map(crate::player_data::PlayerData::to_inventory)
        .or_else(|| native_player.and_then(NativePlayerSession::inventory))
        .unwrap_or_default();
    republish_inventory(entities.players(), player_uuid, &inventory);
    // Send the book only after this connection's inventory exists: its
    // highlight flags are a per-connection acknowledgement state, while the
    // corpus itself is shared. The client uses the ids for `PLACE_RECIPE` too.
    apply(
        conn,
        &mut state,
        proto.encode_recipe_book_add(&recipe_book_snapshot(&inventory), true),
    )
    .await?;
    let mut open_container: Option<OpenContainer> = None;
    let mut open_merchant: Option<OpenMerchant> = None;
    let mut container_sync = ContainerSync::default();
    // This connection's last-known `ServerBound::PlayerInput` sprint flag —
    // see `apply_attack`'s own doc comment for the one thing it feeds
    // (the melee knockback sprint bonus).
    let mut sprinting = false;
    // This connection's last-known secondary-use (sneak) input. Block
    // placement uses it to bypass a clicked container, so shift-right-clicking
    // a chest can place a block beside it instead of opening the chest.
    let mut sneaking = false;
    let mut player_environment = crate::player_environment::PlayerEnvironment::default();
    // This connection's in-progress bow draw — see this parameter's
    // own comment on `dispatch_play_packet`.
    let mut bow_draw: Option<BowDraw> = None;
    // This connection's in-progress eat or drink — see `item_in_use` on
    // `dispatch_play_packet`. Finished by the per-tick arm below, not by a packet.
    let mut item_in_use: Option<ItemInUse> = None;
    // Window identifiers start at `0`; each open increments before use and
    // wraps with [`open_container_screen`]'s `% 100 + 1` rule.
    let mut next_window_id: i32 = 0;
    // This connection's composter roll stream — see
    // `COMPOSTER_BEHAVIOR_SEED` and `dispatch_play_packet`'s parameter comment.
    let mut composter_rng = SpawnRng::new(COMPOSTER_BEHAVIOR_SEED);
    let mut bone_meal_rng = SpawnRng::new(BONE_MEAL_BEHAVIOR_SEED);
    // Restore experience from the player file alongside `vitals` and
    // `inventory`. `PlayerData::preserved` carries fields not modeled by this
    // crate, while this value keeps the live session and subsequent saves
    // consistent with the stored experience.
    let mut experience = saved_player
        .as_ref()
        .map(|data| data.experience)
        .or_else(|| native_runtime.map(|runtime| runtime.experience))
        .unwrap_or_default();
    // Experience-orb pickup starts with no delay, so the first nearby orb is
    // absorbed immediately; `collect_nearby_orbs` decrements this delay after
    // each pickup.
    let mut take_xp_delay: i32 = 0;
    let mut effects = crate::mob_effects::ActiveEffects::new();
    let mut burn = crate::burning::BurnState::new();
    // The fire-contact ramp draws one value from the inclusive range `1..=3`.
    // Keep that draw on its own stream so standing in fire cannot shift which
    // roll a later block drop or composter insert sees.
    let mut burn_rng = SpawnRng::new(BURN_BEHAVIOR_SEED);
    // This connection's block-drop roll stream — see
    // `block_drops::BLOCK_DROPS_BEHAVIOR_SEED` and `dispatch_play_packet`'s
    // parameter comment for why it is separate from the composter's.
    let mut drops_rng = SpawnRng::new(crate::block_drops::BLOCK_DROPS_BEHAVIOR_SEED);
    // This connection's per-player respawn point, written by the bed
    // interaction handler. `apply_client_command` reads it when resolving a
    // death-screen respawn.
    // Restored from the player file, so a bed or anchor set in an earlier
    // session still applies.
    let mut respawn: Option<RespawnPoint> = crate::respawn::load(&preserved_player_fields);
    // Block position recorded when Bad Omen converts to Raid Omen. The
    // `vitals_tick` arm reads and clears it on the final omen tick.
    let mut raid_omen_position: Option<BlockPos> = None;
    // This connection's server-side entity id is the key the
    // night-skip vote stores this player under. A `PlayerRegistry` ticket
    // carries it where a registry exists (LAN, and every `serve_play` gate);
    // singleplayer has no registry, and `LOCAL_PLAYER_ENTITY_ID` is the same
    // constant the v770 encoder uses for the local player — see that const's
    // doc comment.
    let player_entity_id =
        player_ticket.as_ref().map_or(LOCAL_PLAYER_ENTITY_ID, |t| t.entity_id());
    if let (Some(registry), Some(rotation)) = (entities.players(), player_rot) {
        registry.set_rotation(player_entity_id, rotation);
    }
    // Chunk-batch flow-control gate (`ServerBound::ChunkBatchAcknowledged`,
    // see `send_view_update`'s own doc comment). It begins `true` for the
    // outstanding initial join batch; the first acknowledgement this loop
    // receives therefore clears that batch, while later acknowledgements cover
    // `recenter` or `set_view_radius` batches.
    //
    // The deferred join stream is finite and required for the loading screen,
    // so it is not gated on acknowledgements. The gate applies to reactive
    // streams that can emit a batch for every chunk boundary indefinitely.
    // Gating the join stream on a reply can stall fixtures whose protocol
    // implementation does not answer the acknowledgement.
    let mut awaiting_chunk_batch_ack = true;
    let mut pending_chunk_batches: VecDeque<PendingChunkBatch> = VecDeque::new();
    // Packet dispatch fills this queue; the loop drains it immediately after
    // the call returns to publish the message.
    let mut outgoing_chat: Vec<String> = Vec::new();
    // This connection's announced chat-signing session, if any —
    // `None` until a `chat_session_update` arrives, exactly like every other
    // per-connection `Option` this loop threads (`pending_keep_alive`,
    // `player_pos`'s rotation half). See `crate::chat_session`'s own doc.
    let mut chat_session: Option<crate::chat_session::ServerChatSession> = None;
    // This connection's read position in the shared chat log. Initialize it at
    // the log's *current end* so a joining player receives only messages
    // published during this session.
    let mut chat_cursor = entities.players().map_or(0, PlayerRegistry::chat_cursor);
    // The registration-time snapshot excludes older swings while retaining
    // events appended before this loop reaches its first drain.
    let mut swing_cursor = initial_swing_cursor.unwrap_or(0);
    // This connection's read position in the shared plugin-channel
    // broadcast queue. Started at 0 — unlike chat, a *broadcast* is
    // host-published state a new connection legitimately receives: a client
    // that announces `minecraft:brand` support at join is owed the brand
    // payload that was queued before it arrived.
    let mut plugin_channel_cursor: u64 = 0;
    let mut keep_alive_tick = tokio::time::interval_at(
        crate::tick::PlayTimerInstant::now() + KEEP_ALIVE_INTERVAL,
        KEEP_ALIVE_INTERVAL,
    );
    // **`Delay`, not tokio's default `Burst`, and this is the difference between a
    // stall and a disconnect.** `Burst` makes up for missed ticks by firing them
    // back to back with no delay in between, so a pass that overran two intervals
    // resolves `tick()` twice in immediate succession: the first fires and finds no
    // challenge pending, writes one, and the second fires *in the same instant* and
    // finds it unanswered. The client is given literally zero time to reply and is
    // kicked with `disconnect.timeout` for a stall that was entirely ours. `Delay`
    // collapses the backlog into one tick and restarts the period from it, which is
    // what "check the client every 15 seconds" was always supposed to mean.
    keep_alive_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // When the outstanding challenge was written. The timeout is measured from
    // here plus `LoopStallWatch::unserviced` rather than from the interval's own
    // cadence — see that type's doc comment.
    let mut keep_alive_sent_at = crate::tick::PlayTimerInstant::now();
    let mut watch = LoopStallWatch::new();
    // `interval_at`, not the bare `interval` constructor: `Interval::tick`'s
    // *first* call resolves immediately for an interval built with
    // `tokio::time::interval`, which would otherwise fire a redundant
    // game-time-only broadcast in the same instant as the join-time full
    // sync `serve_connection` just sent. Anchoring the first tick a full
    // `TIME_SYNC_INTERVAL` out avoids that, and mirrors `keep_alive_tick`
    // above for the same reason.
    let mut time_sync_tick = tokio::time::interval_at(
        crate::tick::PlayTimerInstant::now() + TIME_SYNC_INTERVAL,
        TIME_SYNC_INTERVAL,
    );
    // Same reasoning as `time_sync_tick`: anchored one interval out so the
    // first vitals tick does not fire in the same instant as join.
    let mut vitals_tick = tokio::time::interval_at(
        crate::tick::PlayTimerInstant::now() + VITALS_TICK_INTERVAL,
        VITALS_TICK_INTERVAL,
    );
    // Same reasoning again: anchored one interval out so the first sync
    // does not fire in the same instant as join (there is nothing open yet
    // at join, so this is cosmetic here, but consistent with every other
    // timer in this function).
    let mut container_sync_tick = tokio::time::interval_at(
        crate::tick::PlayTimerInstant::now() + CONTAINER_SYNC_INTERVAL,
        CONTAINER_SYNC_INTERVAL,
    );
    // Anchor the first entity-stream tick one interval after the join state is
    // sent. An immediate tick would duplicate a diff over unchanged state.
    let mut entity_stream_tick = tokio::time::interval_at(
        crate::tick::PlayTimerInstant::now() + ENTITY_STREAM_INTERVAL,
        ENTITY_STREAM_INTERVAL,
    );
    // Delay missed intervals so an overrun (a chunk strip or container click)
    // produces one streaming pass at a time. Bursting would issue back-to-back
    // diffs over state that cannot have changed between those passes.
    entity_stream_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let play_start = crate::tick::PlayTimerInstant::now();
    let mut next_keep_alive_id: i64 = 0;
    // The deferred join stream's own batch bookkeeping — see
    // `JOIN_STREAM_BATCH_COLUMNS`. `open` is whether a `begin_chunk_batch` has
    // been sent whose `end_chunk_batch` has not; `size` is how many columns are
    // inside it.
    let mut join_batch_open = false;
    let mut join_batch_size: i32 = 0;
    // This countdown is decremented on `vitals_tick` — see that arm.
    let mut player_save_countdown = PLAYER_SAVE_EVERY_VITALS_TICKS;

    // Send the restored inventory once so a rejoining player's items are
    // visible before any slot interaction; see `join_inventory_snapshot`.
    apply(conn, &mut state, join_inventory_snapshot(proto, &inventory)).await?;
    // Send the initial experience snapshot so the client's bar reflects the
    // restored values; see `join_experience`.
    apply(conn, &mut state, join_experience(proto, &experience)).await?;
    republish_experience(entities.players(), player_uuid, &experience);
    // Send the initial attribute snapshot so the client's derived armor display
    // reflects the restored inventory; see `join_attributes`.
    apply(conn, &mut state, join_attributes(proto, &inventory)).await?;

    let mut travel = connection_travel::TravelController::new(source);
    let mut dimension_reset: Option<connection_travel::DimensionReset> = None;
    let mut end_exit = connection_travel::EndExit::new(
        connection_travel::credits_seen_in(&preserved_player_fields),
    );
    let mut pending_join_encodes = PendingJoinEncodes::new();

    loop {
        service.admit_pass().await;
        if join_stream.is_done() && pending_join_encodes.is_empty() {
            if join_batch_open {
                apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                join_batch_open = false;
                join_batch_size = 0;
            }
            world.mark_initial_view_drained();
        }
        travel.promote();
        let home = home_source;
        let active_source = travel.source();
        let source = match active_source.as_ref() {
            Some(other) => SourceRef::Dimension(other),
            None => home,
        };
        let dimension_handles = dimension_scoped_handles(active_source.as_ref());
        let block_entities = dimension_handles.block_entities.as_ref().unwrap_or(block_entities);
        let block_ticks = dimension_handles.block_ticks.as_ref().unwrap_or(block_ticks);
        let active_entities = ActiveEntities::new(world, entities, source.dimension());
        let mobs = active_entities.runtime.as_ref().map_or(mobs, |runtime| runtime.mobs());
        let entities = &active_entities;
        let mut synchronous_relight = true;
        if let Some(coordinate) = pending_relights.front().filter(|_| pending_relights.ready()) {
            if let (Some(shared), Some(compute)) = (
                source.shared_arc(),
                proto.detached_light_compute()
                    .filter(|_| proto.retains_initial_column_light()),
            )
            {
                synchronous_relight = false;
                if detached_relight.is_none() {
                    if !view.delivered.contains(&coordinate) {
                        pending_relights.pop_front();
                    } else {
                        let dimension = source.dimension();
                        let cross_column = proto.uses_cross_column_light();
                        let batch_compute = proto.detached_resident_light_compute();
                        let batch = pending_relights.batch(&view.delivered, batch_compute.is_some());
                        let coordinates = batch.coordinates.clone();
                        let resident_only = !pending_relights.requires_generation(coordinate);
                        let work = move || {
                            if coordinates.len() > 1 {
                                if let Some(batch_compute) = batch_compute {
                                    let result = compute_detached_relight_batch(
                                        shared.as_ref(), &coordinates, dimension, batch_compute,
                                    );
                                    if !matches!(result, RelightBatchOutcome::Unsupported | RelightBatchOutcome::Deferred) {
                                        return result;
                                    }
                                }
                            }
                            match compute_detached_relight(
                                shared.as_ref(),
                                coordinate,
                                dimension,
                                cross_column,
                                resident_only,
                                compute,
                            ) {
                                Some(light) => RelightBatchOutcome::Committed(vec![(coordinate, light)]),
                                None => RelightBatchOutcome::Deferred,
                            }
                        };
                        if let Ok(handle) = crate::worldgen_dispatch::try_spawn(work) {
                            pending_relights.admit(&batch);
                            detached_relight = Some(DetachedRelight {
                                batch,
                                handle,
                            });
                        }
                    }
                }
            }
        }
        if let Some(error) = world.initial_seed_error() {
            return return_initial_seed_error(
                conn, proto, &mut state,
                if join_batch_open { Some(join_batch_size) } else { None }, error,
            ).await;
        }
        tokio::select! {
            error = world.wait_initial_seed_failure() => {
                return return_initial_seed_error(
                    conn, proto, &mut state,
                    if join_batch_open { Some(join_batch_size) } else { None }, error,
                ).await;
            }
            prepared = std::future::poll_fn(|cx| travel.poll_prepared(cx, player_pos)), if travel.is_preparing() => {
                watch.enter();
                let Some(prepared) = prepared? else {
                    watch.pass("travel_declined");
                    continue;
                };
                let dimension_changed = matches!(&prepared, connection_travel::PreparedTravel::Dimension { .. });
                if dimension_changed {
                    detached_relight = None;
                    if join_batch_open {
                        apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                        join_batch_open = false;
                        join_batch_size = 0;
                    }
                }
                if let Some(arrival) = connection_travel::commit(
                    &mut travel, prepared, conn, proto, source, home, &mut state, &mut view,
                    &mut join_stream, &mut pending_join_encodes, &mut pending_chunk_batches,
                    &mut awaiting_chunk_batch_ack, &mut pending_relights, &mut pending_tick_updates,
                    &mut teleport_acknowledgements, &mut player_pos, player_rot,
                    &mut client_movement, &mut fall, &mut client_loaded, game_mode, world,
                    &mut player_ticket_guard, &mut streamer, entities.players(), player_entity_id,
                ).await? {
                    if dimension_changed {
                        pending_break = None;
                        bow_draw = None;
                        item_in_use = None;
                        open_container = None;
                        open_merchant = None;
                    }
                    live_publish_player(
                        live_save, player_store.as_ref(), player_uuid, player_pos, player_rot,
                        world_spawn, &vitals, game_mode, &inventory, &experience,
                        &preserved_player_fields, arrival.dimension,
                    );
                    publish_native_player(
                        native_player, live_save, player_pos, player_rot, world_spawn,
                        arrival.dimension, game_mode, &vitals, &experience, &inventory,
                    );
                }
                watch.pass("travel_commit");
                continue;
            }
            _ = std::future::ready(()), if !pending_tick_updates.is_empty() => {
                watch.enter();
                send_pending_tick_block_updates(
                    conn,
                    proto,
                    &mut state,
                    &view.delivered,
                    &mut pending_relights,
                    &mut pending_tick_updates,
                )
                .await?;
                watch.pass("tick_block_updates");
            }
            _ = std::future::ready(()), if synchronous_relight && pending_relights.ready() => {
                watch.enter();
                send_next_relight(
                    conn,
                    proto,
                    source.get(),
                    &mut state,
                    &view.delivered,
                    &mut pending_relights,
                )
                .await?;
                watch.pass("tick_relight");
            }
            result = std::future::poll_fn(|cx| match detached_relight.as_mut() {
                Some(job) => std::future::Future::poll(std::pin::Pin::new(&mut job.handle), cx),
                None => std::task::Poll::Pending,
            }), if detached_relight.is_some() => {
                watch.enter();
                let batch = detached_relight
                    .take()
                    .expect("the completed relight has an owned handle")
                    .batch;
                match result {
                    Ok(RelightBatchOutcome::Committed(lights)) => {
                        let completed = lights.iter().map(|(coordinate, _)| *coordinate).collect::<Vec<_>>();
                        let remainder = RelightBatch {
                            coordinates: batch.coordinates.into_iter().filter(|coordinate| !completed.contains(coordinate)).collect(),
                            generation_required: batch.generation_required,
                        };
                        if !remainder.coordinates.is_empty() {
                            pending_relights.defer(&remainder);
                        }
                        send_committed_relights(
                            conn, proto, source.get(), &mut state, &view.delivered,
                            &mut pending_relights, lights,
                        ).await?;
                    }
                    Ok(RelightBatchOutcome::Failed(error)) => return Err(error.into()),
                    _ => pending_relights.defer(&batch),
                }
                watch.pass("tick_relight");
            }
            // Finish the source-aware encode that was started by the join arm.
            // The future owns only the source reference and the payload, so it
            // remains safe to leave it pending while packet/timer arms win the
            // race. Keeping this as a separate branch is what prevents a cold
            // neighbour admission from becoming an unserviceable connection
            // pass.
            encoded = std::future::poll_fn(|cx| pending_join_encodes.poll_next(cx)), if !pending_join_encodes.is_empty() => {
                watch.enter();
                let ((cx, cz), (incarnation, directive)) = match encoded {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        return return_chunk_encode_error(
                            conn,
                            proto,
                            &mut state,
                            if join_batch_open { Some(join_batch_size) } else { None },
                            error,
                        )
                        .await;
                    }
                };
                if !view.needs_delivery((cx, cz), incarnation, directive.stage) {
                    watch.pass("join_encode_obsolete");
                    continue;
                }
                if !join_batch_open {
                    apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                    join_batch_open = true;
                    join_batch_size = 0;
                }
                if !send_encoded_column(conn, &mut state, &mut view, (cx, cz), incarnation, directive).await? {
                    continue;
                }
                if let Some(trace) = join_trace.as_ref() {
                    trace.mark("delivered", cx, cz);
                }
                chunks_sent += 1;
                join_batch_size += 1;
                if join_batch_size as usize >= JOIN_STREAM_BATCH_COLUMNS
                    || (join_stream.is_done() && pending_join_encodes.is_empty())
                {
                    apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                    join_batch_open = false;
                }
                watch.pass("join_encode");
            }
            // Stream the deferred join view (`JOIN_PRESTREAM_RADIUS`) while this
            // loop services digs, damage, and container clicks.
            //
            // Disabled once drained, so this is not a branch that returns `None`
            // forever. `select!` polls its branches in a random order, so a ready
            // packet is never starved by a ready column.
            //
            // Both `JoinChunkStream::next` arms are cancel-safe; a canceled
            // column must not silently leave a hole in the client's terrain.
            chunk = tokio::time::timeout(
                crate::join_scheduler::JOIN_STREAM_SERVICE_BUDGET,
                join_stream.next(source),
            ), if pending_join_encodes.can_admit() && !join_stream.is_done() => {
                watch.enter();
                let chunk = match chunk {
                    // A worker can legitimately take hundreds of milliseconds
                    // on a cold terrain column. Dropping only this borrowed
                    // future is safe: `ColumnPipeline::next` leaves the head
                    // handle in place until it has been emitted, so the next
                    // pass observes the same ordered result. Most importantly,
                    // the socket-read arm gets polled at least once per budget.
                    Err(_) => {
                        watch.pass("join_stream");
                        continue;
                    }
                    Ok(Ok(chunk)) => chunk,
                    Ok(Err(error)) => {
                        return return_chunk_encode_error(
                            conn,
                            proto,
                            &mut state,
                            if join_batch_open { Some(join_batch_size) } else { None },
                            error,
                        )
                        .await;
                    }
                };
                if let Some(((cx, cz), payload)) = chunk {
                    let Some(incarnation) = view.incarnation((cx, cz)) else {
                        continue;
                    };
                    let owned_source: Option<Arc<dyn ChunkSource>> = match source {
                        SourceRef::Shared(source) => {
                            let source: Arc<dyn ChunkSource> = source.clone();
                            Some(source)
                        }
                        SourceRef::Dimension(source) => Some(Arc::clone(source)),
                        SourceRef::Borrowed(_) => None,
                    };
                    if let Some(owned_source) = owned_source {
                        // Do not await source admission in this select arm. The
                        // owned future is polled by its own branch on subsequent
                        // passes, while this loop continues accepting packets and
                        // timers.
                        let trace = join_trace.clone();
                        let serial = !matches!(payload, crate::join_scheduler::ColumnPayload::Snapshot(_));
                        pending_join_encodes.push(serial, Box::pin(async move {
                            encode_column_owned(
                                proto,
                                owned_source,
                                cx,
                                cz,
                                trace,
                                payload,
                            )
                            .await
                            .map(|directive| ((cx, cz), (incarnation, directive)))
                        }));
                    } else {
                        // Borrowed sources exist for protocol-level controls and
                        // cannot outlive this loop iteration. They retain the
                        // original inline path; production integrated sources
                        // always take one of the owned arms above.
                        let directive = match encode_column(
                            proto,
                            source,
                            cx,
                            cz,
                            join_trace.as_deref(),
                            payload,
                        )
                        .await {
                            Ok(directive) => directive,
                            Err(error) => {
                                return return_chunk_encode_error(
                                    conn,
                                    proto,
                                    &mut state,
                                    if join_batch_open { Some(join_batch_size) } else { None },
                                    error,
                                )
                                .await;
                            }
                        };
                        if !join_batch_open {
                            apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                            join_batch_open = true;
                            join_batch_size = 0;
                        }
                        if !send_encoded_column(conn, &mut state, &mut view, (cx, cz), incarnation, directive).await? {
                            continue;
                        }
                        if let Some(trace) = join_trace.as_ref() {
                            trace.mark("delivered", cx, cz);
                        }
                        chunks_sent += 1;
                        join_batch_size += 1;
                        if join_batch_size as usize >= JOIN_STREAM_BATCH_COLUMNS
                            || join_stream.is_done()
                        {
                            apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                            join_batch_open = false;
                        }
                    }
                }
                watch.pass("join_stream");
            }
            packet = conn.read_packet() => {
                watch.enter();
                let Some((packet_id, payload)) = packet? else {
                    // Persist the disconnect snapshot while the loop's state is
                    // still intact. The periodic save on `vitals_tick` covers
                    // crashes, cancellation, and propagated errors.
                    persist_player(
                        player_store.as_ref(),
                        player_uuid,
                        player_pos,
                        player_rot,
                        world_spawn,
                        &vitals,
                        game_mode,
                        &inventory,
                        &experience,
                        &preserved_player_fields,
                        source.dimension(),
                    );
                    persist_native_player(
                        native_player,
                        player_pos,
                        player_rot,
                        world_spawn,
                        source.dimension(),
                        game_mode,
                        &vitals,
                        &experience,
                        &inventory,
                    );
                    return Ok(ServeSummary { username, chunks_sent, inventory });
                };
                let pending_keep_alive_before_packet = pending_keep_alive;
                {
                    let dispatch = std::pin::pin!(dispatch_play_packet(
                        conn,
                        proto,
                        source,
                        home,
                        &mut state,
                        (!matches!(source, SourceRef::Borrowed(_))
                            && proto.retains_initial_column_light()
                            && proto.detached_light_compute().is_some())
                            .then_some(&mut pending_relights),
                        &mut view,
                        &player_ticket_guard,
                        &mut pending_keep_alive,
                        &mut pending_break,
                        &mut pending_prediction_ack,
                        &mut teleport_acknowledgements,
                        &mut player_pos,
                        &mut client_movement,
                        &mut player_rot,
                        &mut fall,
                        &mut vitals,
                        &mut burn,
                        world,
                        &mut inventory,
                        block_entities,
                        &mut open_container,
                        &mut open_merchant,
                        &mut container_sync,
                        &mut next_window_id,
                        mobs,
                        &mut sprinting,
                        &mut sneaking,
                        &mut awaiting_chunk_batch_ack,
                        &mut pending_chunk_batches,
                        // The live stream this loop's own `select!` branch drains, lent
                        // for the length of the call so a chunk-boundary crossing can
                        // enqueue into it rather than generate inline. Safe to reborrow
                        // here: `select!` drops every branch future before running the
                        // handler, which is the same property the `reprioritise` call
                        // further down this arm already relies on.
                        Some(&mut join_stream),
                        &commands,
                        &mut advancements,
                        player_uuid,
                        profile_key_issuers.as_ref(),
                        enforce_secure_profile,
                        &mut outgoing_chat,
                        &mut chat_session,
                        entities.players(),
                        block_ticks,
                        resource_packs,
                        &mut client_loaded,
                        &mut composter_rng,
                        &mut bone_meal_rng,
                        &mut experience,
                        &mut effects,
                        &mut drops_rng,
                        client_channels,
                        plugin_channels,
                        &mut game_mode,
                        &mut abilities,
                        &mut respawn,
                        sleep_vote,
                        border,
                        player_entity_id,
                        &username,
                        world_spawn,
                        // This loop counts ticks from `play_start` for the
                        // time-of-day broadcast; the break validator reads that
                        // monotonic clock, so a dig's start and stop use one counter.
                        Some(u64::try_from(ticks_since(play_start)).unwrap_or(0)),
                        &mut bow_draw,
                        &mut item_in_use,
                        &mut dimension_reset,
                        &mut end_exit,
                        packet_id,
                        &payload,
                    ));
                    crate::worldgen_progress::measure_polls(
                        WorldgenTimingPhase::ConnectionDispatch, dispatch,
                    ).await?;
                }
                player_tick_ready(world, client_loaded);
                if let Some(id) = pending_keep_alive_before_packet
                    && pending_keep_alive.is_none()
                {
                    tracing::debug!(
                        target: "lodestone_keepalive",
                        id,
                        round_trip_millis = keep_alive_sent_at.elapsed().as_millis() as u64,
                        "server accepted keep-alive response"
                    );
                }
                republish_inventory(entities.players(), player_uuid, &inventory);
                if end_exit.take_respawn() {
                    dimension_reset = connection_travel::perform_respawn(
                        conn, proto, &mut state, home.get(), source.get(), &mut respawn, world_spawn, game_mode,
                        &mut teleport_acknowledgements, block_ticks, true,
                    ).await?;
                }
                // A bed, anchor or `/spawnpoint` this packet set (or a respawn
                // that cleared the point) reaches the next save through the
                // preserved fields.
                crate::respawn::store(&mut preserved_player_fields, respawn.as_ref());
                if let Some(connection_travel::DimensionReset { target, route }) = dimension_reset.take() {
                    detached_relight = None;
                    // The respawn's own dimension: the home one, the one already
                    // being viewed, or a sibling reached through the world.
                    let (destination, arrival_dimension) = match &route {
                        crate::respawn::Route::Home => (home.get(), home.dimension()),
                        crate::respawn::Route::Current => (source.get(), source.dimension()),
                        crate::respawn::Route::Sibling(other) => (
                            other.as_ref(),
                            other.dimension().unwrap_or(crate::dimension::Dimension::Overworld),
                        ),
                    };
                    let ticket_transfer = connection_travel::prepare_ticket_transfer(
                        &player_ticket_guard, home.get(), destination, target, view.radius,
                    )?;
                    if join_batch_open {
                        apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                        join_batch_open = false;
                        join_batch_size = 0;
                    }
                    connection_travel::reset_stream(
                        conn, proto, &mut state, target, &mut view, &mut join_stream,
                        &mut pending_join_encodes, &mut pending_chunk_batches,
                        &mut awaiting_chunk_batch_ack, &mut pending_relights, &mut pending_tick_updates,
                    ).await?;
                    for directive in streamer.reset_dimension(proto) {
                        apply(conn, &mut state, directive).await?;
                    }
                    connection_travel::finish_ticket_transfer(
                        &mut player_ticket_guard, ticket_transfer, source.get(), destination,
                    );
                    connection_travel::reset_player(
                        target, arrival_dimension, &mut player_pos, &mut client_movement,
                        &mut fall, &mut client_loaded, proto.sends_player_loaded(), world, entities.players(), player_entity_id,
                    );
                    match route {
                        crate::respawn::Route::Home => travel.stage(connection_travel::Destination::Home),
                        crate::respawn::Route::Current => {}
                        crate::respawn::Route::Sibling(other) => {
                            travel.stage(connection_travel::Destination::Dimension(other));
                        }
                    }
                    pending_break = None;
                    bow_draw = None;
                    item_in_use = None;
                    open_container = None;
                    open_merchant = None;
                    live_publish_player(
                        live_save, player_store.as_ref(), player_uuid, player_pos, player_rot,
                        world_spawn, &vitals, game_mode, &inventory, &experience,
                        &preserved_player_fields, arrival_dimension,
                    );
                    publish_native_player(
                        native_player, live_save, player_pos, player_rot, world_spawn,
                        arrival_dimension, game_mode, &vitals, &experience, &inventory,
                    );
                    watch.pass("dimension_reset");
                    continue;
                }
                // Flush advancement changes caused by the packet just granted.
                // Advancement producers are packet-driven, so flushing after
                // dispatch avoids a separate timer. `flush_dirty` returns
                // `None` when nothing changed, keeping the common case packet-free.
                if let Some(update) = advancements.flush_dirty(player_uuid, true) {
                    apply(conn, &mut state, proto.encode_update_advancements(&update)).await?;
                }
                // Republish this player's chat for every connection, including
                // this one. The registry holds the sender and recipient views.
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
                // Republish this player's position for other connections.
                // Read the value updated by packet dispatch so the registry
                // receives movement without another state parameter.
                if let (Some(ticket), Some(registry), Some((x, y, z))) = (
                    player_ticket.as_ref(),
                    entities.players(),
                    player_pos,
                ) {
                    registry.set_position(ticket.entity_id(), Vec3::new(x, y, z));
                    registry.set_on_ground(ticket.entity_id(), client_movement.on_ground);
                }
                // Re-key pending join columns after movement or facing changes:
                // distance from the player's current column comes first, then
                // the view cone (`join_scheduler::priority_key`). Read back the
                // position and rotation updated by packet dispatch; unchanged
                // center and quantized yaw leave the queue untouched.
                if !join_stream.is_done() {
                    if let Some((x, _, z)) = player_pos {
                        join_stream.reprioritise(
                            (
                                (x / 16.0).floor() as i32,
                                (z / 16.0).floor() as i32,
                            ),
                            player_rot.map(|rotation| rotation.yaw),
                        );
                    }
                }
                // Collect drops at the player's current position.
                // Here, and not in `dispatch_play_packet`, for the same reason
                // the position republish above is here: `player_pos` has just
                // been updated by whichever movement packet arrived, and the
                // `stream_pass` below will carry the resulting
                // `REMOVE_ENTITIES` for the collected item in the very same
                // pass — so the item vanishes from the world and appears in the
                // hotbar together rather than a packet apart.
                if let Some((x, y, z)) = player_pos {
                    let pickups = collect_nearby_items(
                        mobs,
                        &mut inventory,
                        Vec3::new(x, y, z),
                        &mut advancements,
                        player_uuid,
                        world.time().game_time.saturating_mul(50),
                    );
                    // **The pickup animation, and it must go out before the
                    // `stream_pass` below.** That pass derives `REMOVE_ENTITIES`
                    // from the removal `collect_nearby_items` just performed, and
                    // the client keeps the item entity alive precisely so it can
                    // interpolate it toward the collector — it removes the entity
                    // itself when the animation completes. Announce the take after
                    // the removal has been broadcast and the client has nothing
                    // left to animate, so the packet is present, correct, and
                    // invisible.
                    for take in &pickups.takes {
                        apply(
                            conn,
                            &mut state,
                            proto.encode_take_item_entity(
                                take.item_entity_id,
                                // Self-facing (sent only to `conn`): the collector
                                // must be `LOCAL_PLAYER_ENTITY_ID`, matching this
                                // rule's other call sites — see `publish_health`'s.
                                LOCAL_PLAYER_ENTITY_ID,
                                take.amount,
                            ),
                        )
                        .await?;
                    }
                    for native in pickups.changed {
                        // Window `0`, menu slot for this native index. `state_id`
                        // `0` matches every other server-initiated slot write in
                        // this file (`apply_container_clicked` applies a click's
                        // diff verbatim and never validates a stale id).
                        if let Some(menu_slot) = window_zero_menu_slot(native) {
                            apply(
                                conn,
                                &mut state,
                                proto.encode_container_slot(
                                    0,
                                    0,
                                    menu_slot,
                                    inventory.native(native),
                                ),
                            )
                            .await?;
                        }
                    }
                    // Experience orbs, on the same movement cadence and for the same
                    // reason: the `stream_pass` below carries the `REMOVE_ENTITIES` for
                    // an orb that was fully absorbed, so it vanishes and the bar moves
                    // together rather than a pass apart.
                    //
                    // `TAKE_ITEM_ENTITY` with amount `1` is vanilla's own
                    // `player.take(this, 1)` — the same packet an item pickup uses, and
                    // what drives the client's absorption animation and pickup sound.
                    // Sent *before* the removal for the item path's reason.
                    if let Some(absorbed) = collect_nearby_orbs(
                        mobs,
                        Vec3::new(x, y, z),
                        &mut experience,
                        &mut take_xp_delay,
                    ) {
                        apply(
                            conn,
                            &mut state,
                            proto.encode_take_item_entity(
                                absorbed.orb_entity_id,
                                // Self-facing, per the item-pickup call site above.
                                LOCAL_PLAYER_ENTITY_ID,
                                1,
                            ),
                        )
                        .await?;
                        // The mutation-then-send rule `join_experience`'s doc states: a
                        // `give_points` with no `set_experience` behind it is the exact
                        // shape that made the XP bar invisible in the first place.
                        debug_assert!(absorbed.points > 0, "an absorbed orb pays out points");
                        republish_experience(entities.players(), player_uuid, &experience);
                        apply(
                            conn,
                            &mut state,
                            proto.encode_set_experience(
                                experience.progress(),
                                experience.level(),
                                experience.total(),
                            ),
                        )
                        .await?;
                    }
                }
                // Republish facing separately because rotation and position
                // arrive on different packets; requiring both values would
                // omit a player who turns without moving.
                if let (Some(ticket), Some(registry), Some(rotation)) =
                    (player_ticket.as_ref(), entities.players(), player_rot)
                {
                    registry.set_rotation(ticket.entity_id(), rotation);
                }
                republish_effect_entity_flags(entities.players(), player_ticket.as_ref(), &effects);
                for directive in stream_pass(
                    proto,
                    entities,
                    &mut streamer,
                    &mut player_list,
                    player_ticket.as_ref(),
                ) {
                    apply(conn, &mut state, directive).await?;
                }
                // The arm that owns the view update, and therefore the longest pass
                // this loop has. A `PlayerMoved` that crosses a chunk boundary
                // awaits a whole strip of columns inside `dispatch_play_packet`.
                watch.pass("read_packet");
            }

            // The server-driven streaming pass. Identical body to the one at
            // the tail of the `read_packet` arm above, which stays where it is:
            // that one has to run *after* the packet it just handled (a move
            // republishes this player's position into the registry, a dig
            // removes an entity), so folding the two into this timer would
            // delay every such consequence by up to a tick. This arm is what
            // covers the other direction — everything that changes while this
            // connection says nothing. See [`ENTITY_STREAM_INTERVAL`].
            _ = entity_stream_tick.tick() => {
                watch.enter();
                republish_effect_entity_flags(entities.players(), player_ticket.as_ref(), &effects);
                for directive in stream_pass(
                    proto,
                    entities,
                    &mut streamer,
                    &mut player_list,
                    player_ticket.as_ref(),
                ) {
                    apply(conn, &mut state, directive).await?;
                }
                watch.pass("entity_stream_tick");
            }

            _ = keep_alive_tick.tick() => {
                watch.enter();
                // **Forgive an unanswered challenge only when this loop had no
                // serviced time at all to hear the answer in.** A reply needs
                // milliseconds of serviced time to be read, so the only honest
                // excuse is a stall that swallowed the *whole* window — hence the
                // comparison against a full interval rather than against any stall
                // at all. Without this the server kicks a client that answered
                // promptly, because the reply sat unread in the receive buffer while
                // an arm body was awaiting terrain; `disconnect.timeout` then reads
                // on the client as the client's own fault. See `LoopStallWatch`.
                //
                // Bounded, which a wall-clock deadline extension would not be: the
                // excuse has to be re-earned in full inside every window, since
                // `clear_unserviced` zeroes the accounting each time a challenge is
                // written. With no stall the behaviour is identical to having no
                // clause here, which is why the timeout gates are untouched.
                if pending_keep_alive.is_some() && watch.unserviced >= KEEP_ALIVE_INTERVAL {
                    tracing::warn!(
                        target: "lodestone_server::stall",
                        unserviced_millis = watch.unserviced.as_millis() as u64,
                        waited_millis = keep_alive_sent_at.elapsed().as_millis() as u64,
                        worst_arm = watch.worst().map(|(arm, _)| arm).unwrap_or(""),
                        worst_millis = watch.worst().map(|(_, d)| d.as_millis() as u64).unwrap_or(0),
                        "keep-alive unanswered, but this loop ate the whole window — not kicking",
                    );
                    keep_alive_sent_at = crate::tick::PlayTimerInstant::now();
                    watch.clear_unserviced();
                    watch.pass("keep_alive_tick");
                    continue;
                }
                if pending_keep_alive.is_some() {
                    // Tell the client why the connection is closing.
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
                    // **Who initiated the disconnect, in the log, at the moment it
                    // happens.** "Timed out" is the *server's* verdict, and this is
                    // the only producer of it in the workspace; a reader who sees the
                    // string on a client has no way to tell it from a client-side
                    // give-up without this line. The stall figures come with it
                    // because they are what distinguishes a client that stopped
                    // answering from a server that stopped listening.
                    tracing::warn!(
                        target: "lodestone_server::stall",
                        waited_millis = keep_alive_sent_at.elapsed().as_millis() as u64,
                        worst_arm = watch.worst().map(|(arm, _)| arm).unwrap_or(""),
                        worst_millis = watch.worst().map(|(_, d)| d.as_millis() as u64).unwrap_or(0),
                        "server is disconnecting this client for an unanswered keep-alive",
                    );
                    let directive = proto.encode_disconnect(state, &timeout_reason());
                    let _ = apply(conn, &mut state, directive).await;
                    return Err(ServerError::KeepAliveTimeout);
                }
                next_keep_alive_id += 1;
                pending_keep_alive = Some(next_keep_alive_id);
                keep_alive_sent_at = crate::tick::PlayTimerInstant::now();
                watch.clear_unserviced();
                tracing::debug!(
                    target: "lodestone_keepalive",
                    id = next_keep_alive_id,
                    "server sent keep-alive challenge"
                );
                apply(conn, &mut state, proto.encode_keep_alive(next_keep_alive_id)).await?;
                // Refresh the world-spawn ticket from the connection's
                // keep-alive timer. Compatibility entry points use a detached
                // ticket handle, so this call is a no-op there.
                player_ticket_guard.refresh_world_spawn();
                watch.pass("keep_alive_tick");
            }

            _ = time_sync_tick.tick() => {
                watch.enter();
                // **Use the shared world clock.** Encode its long `game_time`
                // and optional day-time so every connection observes the same
                // authoritative sky clock. Sending day-time also lets a frozen
                // time rule keep the client's sun anchored.
                let time = world.time();
                apply(
                    conn,
                    &mut state,
                    proto.encode_set_time(time.game_time, Some(time.day_time)),
                )
                .await?;
                watch.pass("time_sync_tick");
            }

            _ = vitals_tick.tick() => {
                watch.enter();
                if let Some(sequence) = pending_prediction_ack.take() {
                    apply(conn, &mut state, proto.encode_block_changed_ack(sequence)).await?;
                }
                tick_client_load_timeout(&mut client_loaded, &mut client_load_wait);
                if !player_tick_ready(world, client_loaded) {
                    watch.pass("vitals_waiting_for_player_loaded");
                    continue;
                }
                // Count periodic saves in 50 ms vitals ticks rather than wall
                // time. This avoids unsupported wall-clock calls in wasm32.
                // Periodic saves cover disconnect, cancellation, and crash paths
                // that cannot run the disconnect cleanup.
                player_save_countdown = player_save_countdown.saturating_sub(1);
                if player_save_countdown == 0 {
                    player_save_countdown = PLAYER_SAVE_EVERY_VITALS_TICKS;
                    persist_player(
                        player_store.as_ref(),
                        player_uuid,
                        player_pos,
                        player_rot,
                        world_spawn,
                        &vitals,
                        game_mode,
                        &inventory,
                        &experience,
                        &preserved_player_fields,
                        source.dimension(),
                    );
                }

                // Advance deferred block breaks on each 50 ms vitals tick. This
                // completes hold-and-release digs whose stop packet leaves
                // progress below `0.7`. Skip the work when the block is air;
                // another world update may have removed it, and re-breaking air
                // would emit duplicate drops.
                if let Some(dig) = pending_break.filter(|dig| {
                    dig.deferred_break_ready(
                        Some(u64::try_from(ticks_since(play_start)).unwrap_or(0)),
                    )
                }) {
                    // A timer probe must not turn a cold deferred break into
                    // synchronous generation. Keep the pending break intact;
                    // the next tick retries after the view stream admits it.
                    if let Some(current) = resident_block_state(
                        source.get(),
                        dig.pos.x,
                        dig.pos.y,
                        dig.pos.z,
                    ) {
                        pending_break = None;
                        if current.block() != Block::Air {
                            let allowed = adjudicate_block_break(
                                conn,
                                proto,
                                source.get(),
                                &mut state,
                                world,
                                dig.pos,
                                player_uuid,
                            )
                            .await?;
                            if allowed {
                                destroy_block(
                                conn,
                                proto,
                                source.get(),
                                &mut state,
                                (!matches!(source, SourceRef::Borrowed(_))
                                    && proto.retains_initial_column_light()
                                    && proto.detached_light_compute().is_some())
                                    .then_some(&mut pending_relights),
                                block_entities,
                                &mut open_container,
                                &mut container_sync,
                                mobs,
                                &mut drops_rng,
                                inventory.selected_item(),
                                block_ticks,
                                player_uuid,
                                !matches!(game_mode, GameMode::Creative) && world.block_drops(),
                                world.block_drops(),
                                dig.pos,
                                &mut advancements,
                                (!matches!(game_mode, GameMode::Creative)).then_some(&mut vitals),
                                )
                                .await?;
                            }
                        }
                    }
                }

                // Vanilla's own update-using-item routine: a consume ends on the server's
                // own clock, not on a packet — the client sends nothing when a
                // steak finishes, so without this arm every bite starts and none
                // ever lands. Read against `MobSim`'s tick counter because that is
                // the clock `apply_use_item` stamped `finish_tick` from; mixing it
                // with this loop's `ticks_since(play_start)` would compare two
                // unrelated counters.
                if let Some(started) = item_in_use.clone() {
                    let now = mobs.with(|sim| sim.tick_count());
                    // The periodic eating/drinking sound —
                    // vanilla's own on-use-tick → emit-particles-and-sounds chain.
                    // **Sound only**: the crumbs are the client's own prediction,
                    // because the server-side particle hook is a no-op, and
                    // the sound is *only* the server's, because
                    // the client-side seeded-sound hook drops the particle-only call.
                    // Splitting one game action across the two sides looks like
                    // an omission on each of them; it is the whole mechanism.
                    if now < started.finish_tick
                        && let Some(pos) = player_pos
                        && let Some(consumable) =
                            lodestone_game::consumable::consumable_for_item(&started.item)
                    {
                        let remaining =
                            u32::try_from(started.finish_tick - now).unwrap_or(u32::MAX);
                        let already = started.last_effect_remaining == Some(remaining);
                        if !already
                            && lodestone_game::consumable::should_emit_consume_effects(
                                consumable.consume_ticks,
                                remaining,
                            )
                        {
                            let seed = i64::from(drops_rng.next_int(i32::MAX));
                            let roll = drops_rng.next_f32();
                            if let Some(effect) = crate::effects::item_consumed_tick(
                                &started.item,
                                Vec3::new(pos.0, pos.1, pos.2),
                                roll,
                                seed,
                            ) {
                                block_ticks.publish_effect_except(player_uuid, effect);
                            }
                        }
                        // Latched whether or not this tick emitted, so the guard
                        // tracks "already looked at this tick" rather than "already
                        // played", which is the property that makes it idempotent.
                        if let Some(live) = item_in_use.as_mut() {
                            live.last_effect_remaining = Some(remaining);
                        }
                    }
                    if now >= started.finish_tick {
                        item_in_use = None;
                        if let Some((native, remainder)) =
                            finish_consuming(&mut inventory, &mut vitals, &started, game_mode)
                        {
                            // Vanilla's own food-properties on-consume routine: the consumable sound again,
                            // louder and on `NEUTRAL`, plus the burp. Both are on the
                            // **food** component, so they are published here — inside
                            // the `finish_consuming` success arm, which already
                            // required a food — rather than beside the periodic sound
                            // above, which is `Consumable`'s and fires for potions too.
                            if let Some(pos) = player_pos {
                                let at = Vec3::new(pos.0, pos.1, pos.2);
                                let seed = i64::from(drops_rng.next_int(i32::MAX));
                                if let Some(effect) = crate::effects::item_consume_finished(
                                    &started.item,
                                    at,
                                    drops_rng.next_f32(),
                                    seed,
                                ) {
                                    block_ticks.publish_effect(effect);
                                }
                                block_ticks.publish_effect(crate::effects::player_burped(
                                    at,
                                    drops_rng.next_f32(),
                                    i64::from(drops_rng.next_int(i32::MAX)),
                                ));
                            }
                            if let Some(menu_slot) = window_zero_menu_slot(native) {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_container_slot(
                                        0,
                                        0,
                                        menu_slot,
                                        remainder.as_ref(),
                                    ),
                                )
                                .await?;
                            }
                            // The food bar frame carries health, food, and
                            // saturation together, so resend all three values
                            // whenever one of them changes.
                            apply(
                                conn,
                                &mut state,
                                proto.encode_set_health(
                                    vitals.health(),
                                    vitals.food().food_level(),
                                    vitals.food().saturation(),
                                ),
                            )
                            .await?;

                            // Apply item-specific effects after the food-bar
                            // frame. These use separate status-effect packets,
                            // not health-bar fields; the supported item set is
                            // defined by `food_consume_effects`.
                            for grant in crate::mob_effects::food_consume_effects(&started.item) {
                                if drops_rng.next_f32() >= grant.probability {
                                    continue;
                                }
                                effects.apply(grant.effect_id, grant.duration, grant.amplifier);
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_update_mob_effect(
                                        LOCAL_PLAYER_ENTITY_ID,
                                        grant.effect_id,
                                        grant.amplifier,
                                        grant.duration,
                                        false,
                                        true,
                                        true,
                                        false,
                                    ),
                                )
                                .await?;
                            }
                            if crate::mob_effects::removes_poison_on_consume(&started.item)
                                && effects.remove("minecraft:poison")
                            {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_remove_mob_effect(
                                        LOCAL_PLAYER_ENTITY_ID,
                                        "minecraft:poison",
                                    ),
                                )
                                .await?;
                            }
                        } else if let Some((native, remainder)) = finish_drinking_ominous_bottle(
                            &mut inventory,
                            &mut effects,
                            &started,
                            game_mode,
                        ) {
                            if let Some(menu_slot) = window_zero_menu_slot(native) {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_container_slot(
                                        0,
                                        0,
                                        menu_slot,
                                        remainder.as_ref(),
                                    ),
                                )
                                .await?;
                            }
                            apply(
                                conn,
                                &mut state,
                                proto.encode_update_mob_effect(
                                    LOCAL_PLAYER_ENTITY_ID,
                                    "minecraft:bad_omen",
                                    0,
                                    120_000,
                                    true,
                                    true,
                                    true,
                                    false,
                                ),
                            )
                            .await?;
                        } else if let Some((native, remainder, potion_effects)) =
                            finish_drinking_potion(&mut inventory, &started, game_mode)
                        {
                            if let Some(menu_slot) = window_zero_menu_slot(native) {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_container_slot(
                                        0,
                                        0,
                                        menu_slot,
                                        remainder.as_ref(),
                                    ),
                                )
                                .await?;
                            }
                            // Vanilla's own potion-contents apply-to-living-entity routine's own split: an
                            // instantaneous effect heals/damages immediately (no
                            // `MobEffectInstance` is ever stored for one, so no
                            // `update_mob_effect` follows — only the health bar
                            // moves), a timed one is stored and announced.
                            let mut health_changed = false;
                            for effect in potion_effects {
                                match effect {
                                    crate::mob_effects::SplashEffect::Instant {
                                        effect_id,
                                        amount,
                                    } => {
                                        match effect_id {
                                            lodestone_data::mob_effects::MobEffectId::INSTANT_HEALTH => {
                                                vitals.heal(amount)
                                            }
                                            lodestone_data::mob_effects::MobEffectId::INSTANT_DAMAGE => {
                                                vitals.apply_effect_damage(amount);
                                            }
                                            _ => continue,
                                        }
                                        health_changed = true;
                                    }
                                    crate::mob_effects::SplashEffect::Timed {
                                        effect_id,
                                        duration,
                                        amplifier,
                                    } => {
                                        effects.apply(effect_id, duration, amplifier);
                                        let effect_name =
                                            lodestone_data::mob_effects::mob_effect_name_for(effect_id);
                                        apply(
                                            conn,
                                            &mut state,
                                            proto.encode_update_mob_effect(
                                                LOCAL_PLAYER_ENTITY_ID,
                                                effect_name,
                                                amplifier,
                                                duration,
                                                false,
                                                true,
                                                true,
                                                false,
                                            ),
                                        )
                                        .await?;
                                    }
                                }
                            }
                            if health_changed {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_set_health(
                                        vitals.health(),
                                        vitals.food().food_level(),
                                        vitals.food().saturation(),
                                    ),
                                )
                                .await?;
                            }
                        } else if let Some((native, remainder, cleared)) = finish_drinking_milk(
                            &mut inventory,
                            &mut effects,
                            &started,
                            game_mode,
                        ) {
                            if let Some(menu_slot) = window_zero_menu_slot(native) {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_container_slot(
                                        0,
                                        0,
                                        menu_slot,
                                        remainder.as_ref(),
                                    ),
                                )
                                .await?;
                            }
                            for effect_id in cleared {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.encode_remove_mob_effect(
                                        LOCAL_PLAYER_ENTITY_ID,
                                        &effect_id,
                                    ),
                                )
                                .await?;
                            }
                        }
                    }
                }

                // No position yet (client has not sent a single move since
                // join): nothing to test submersion against, so skip rather
                // than guess a spawn position this version-free crate does
                // not otherwise track (see `crate::vitals`'s module docs).
                if let Some((x, y, z)) = player_pos {
                    // Apply border damage before the submersion check because
                    // the player timer processes the border branch first.
                    // Read one border snapshot per timer tick and calculate
                    // damage for the tracked position; `apply_border_damage`
                    // returns `Some` only when the hit lands, while a dead
                    // player is a no-op. A player outside the safe zone takes
                    // `max(1, floor(d*0.2))` every tick. With a default
                    // full-size border, `damage_for_position` is always `None`:
                    // nothing is sent, at the cost of one clone and one
                    // distance scan per 50 ms.
                    let border_state = border.get();
                    let invulnerable = Abilities::for_mode(game_mode).invulnerable;
                    if let Some(damage) =
                        border_state.damage_for_position(x, z).filter(|_| !invulnerable)
                    {
                        if vitals.apply_border_damage(damage).is_some() {
                            publish_health(
                                conn,
                                &mut state,
                                proto,
                                &vitals,
                                &effects,
                                Vec3::new(x, y, z),
                                // Self-facing, per `publish_health`'s own call sites.
                                LOCAL_PLAYER_ENTITY_ID,
                                &username,
                                crate::vitals::DeathCause::OutsideBorder,
                                &mut advancements,
                                player_uuid,
                                Some(crate::vitals::HurtDirection::PURE_ROLL),
                            )
                            .await?;
                        }
                    }

                    if let Some(eye_in_water) = player_environment.eye_in_water(
                        lodestone_physics::Vec3d::new(x, y, z),
                        sprinting,
                        sneaking,
                        abilities.flying,
                        &|bx, by, bz| resident_block_state(source.get(), bx, by, bz),
                    ) {
                        // `!invulnerable &&`: a creative player's air bar does not
                        // deplete and they never drown. Suppressed here rather than
                        // at the damage below because `PlayerVitals` is mode-free by
                        // design, and a depleting bar that can never hurt is worse
                        // than no bar at all. A missing resident cell defers the
                        // complete air probe rather than treating it as air.
                        let outcome = tick_player_air_supply(
                            &mut vitals,
                            eye_in_water,
                            invulnerable,
                            &effects,
                        );
                        if let Some(air) = outcome.air_changed {
                            apply(conn, &mut state, proto.encode_air_supply_update(air)).await?;
                        }
                        if outcome.damage.is_some() {
                            publish_health(
                                conn,
                                &mut state,
                                proto,
                                &vitals,
                                &effects,
                                Vec3::new(x, y, z),
                                // Self-facing, per `publish_health`'s own call sites.
                                LOCAL_PLAYER_ENTITY_ID,
                                &username,
                                crate::vitals::DeathCause::Drown,
                                &mut advancements,
                                player_uuid,
                                Some(crate::vitals::HurtDirection::PURE_ROLL),
                            )
                            .await?;
                        }
                    }

                    // hostile-mob melee damage against this
                    // player, drained from `MobSim::take_player_hits`. See
                    // that method's, and `PlayerHit`'s, own doc comments for
                    // how a mob's attack target position resolves to a
                    // player identity and the one disclosed miss (a stale
                    // grudge target). `!invulnerable` matches the border and
                    // drowning arms just above: a creative/spectator player
                    // takes no damage from any source here.
                    //
                    // Filtered to `player_uuid` because the drain empties for
                    // whichever connection reads it first — the same
                    // single-consumer caveat `ExplosionFeed`/`BlockTickFeed`
                    // document — which matches this crate's one
                    // connection-per-mob-tick-loop shape today (singleplayer,
                    // and `bind`'s per-connection LAN wrapper — see those
                    // types' own doc comments).
                    if !invulnerable {
                        for hit in mobs.with(|sim| sim.take_player_hits()) {
                            if hit.identity.uuid != player_uuid {
                                continue;
                            }
                            let flags = lodestone_entity::DamageFlags::for_damage_type_name(
                                "mob_attack",
                            )
                            .expect("mob_attack is a real damage type");
                            if vitals
                                .apply_damage(
                                    hit.raw_damage,
                                    &effects.overlay_defenses(inventory.combat_stats().defenses),
                                    flags,
                                )
                                .is_some()
                            {
                                let direction = crate::vitals::HurtDirection::from_source(
                                    hit.attacker_pos,
                                    Vec3::new(x, y, z),
                                    player_rot.unwrap_or_default().yaw,
                                );
                                publish_health(
                                    conn,
                                    &mut state,
                                    proto,
                                    &vitals,
                                    &effects,
                                    Vec3::new(x, y, z),
                                    // Self-facing, per `publish_health`'s own call sites.
                                    LOCAL_PLAYER_ENTITY_ID,
                                    &username,
                                    crate::vitals::DeathCause::Generic,
                                    &mut advancements,
                                    player_uuid,
                                    Some(direction),
                                )
                                .await?;
                            }
                        }
                    }
                }

                // Drain elder-guardian pulses for this player. The queue uses
                // the same single-consumer, UUID-filtered shape as
                // `take_player_hits`; each matching pulse applies mining
                // fatigue and emits game event kind `10`.
                if matches!(game_mode, GameMode::Survival | GameMode::Adventure) {
                    for aura in mobs.with(|sim| sim.take_mining_fatigue_auras()) {
                        if aura.target.uuid != player_uuid {
                            continue;
                        }
                        effects.apply(
                            "minecraft:mining_fatigue",
                            crate::mobs::ELDER_GUARDIAN_EFFECT_DURATION,
                            crate::mobs::ELDER_GUARDIAN_EFFECT_AMPLIFIER,
                        );
                        apply(
                            conn,
                            &mut state,
                            proto.encode_update_mob_effect(
                                LOCAL_PLAYER_ENTITY_ID,
                                "minecraft:mining_fatigue",
                                crate::mobs::ELDER_GUARDIAN_EFFECT_AMPLIFIER,
                                crate::mobs::ELDER_GUARDIAN_EFFECT_DURATION,
                                true,
                                true,
                                true,
                                false,
                            ),
                        )
                        .await?;
                        apply(conn, &mut state, proto.encode_game_event(10, 1.0)).await?;
                    }
                }

                // Burning. The ignition producer and the burn consumer in one place,
                // because both need the same feet-cell read — vanilla splits them
                // (its own fire-block entity-inside routine ignites, its own base-tick routine consumes)
                // only because the block and the entity are different objects.
                //
                // The **feet** cell, not the eye: `entityInside` fires for any cell the
                // bounding box overlaps, and the feet cell is the one this crate
                // tracks. Reading the eye instead would let a player stand in fire
                // unharmed up to their chin.
                //
                // `!invulnerable`: fire immunity applies to the entity type and
                // `invulnerable` applies inside the damage path; a creative
                // player is the second. Passed as `fire_immune` because the observable
                // is the same — the fire goes out and nothing hurts — and this crate
                // has no per-entity-type immunity table to consult.
                if let Some((x, y, z)) = player_pos {
                    if let Some(feet) = resident_block_state(
                        source.get(),
                        x.floor() as i32,
                        y.floor() as i32,
                        z.floor() as i32,
                    ) {
                        let standing_in = crate::burning::BurnSource::for_block(feet);
                        let creative = Abilities::for_mode(game_mode).invulnerable;
                        // Fire Resistance refuses the damage and leaves the counter
                        // running — see `crate::burning`'s doc for why that is not the
                        // same as putting the fire out.
                        let resistant = effects
                            .amplifier_of("minecraft:fire_resistance")
                            .is_some();
                        if let Some(source_kind) = standing_in
                            && !creative
                        {
                            match source_kind {
                            // Vanilla's own fire-block ignite routine — the player ramp, which is
                            // why running across one fire block can leave you unburnt.
                            // One draw per contact tick, from this connection's own
                            // stream.
                            crate::burning::BurnSource::Fire
                            | crate::burning::BurnSource::SoulFire => {
                                let ramp = 1 + i32::from(burn_rng.next_f32() < 0.5);
                                burn.fire_ignite(true, ramp);
                            }
                            // Vanilla's own entity lava-ignite routine — a flat 15 seconds, no ramp.
                            crate::burning::BurnSource::Lava => {
                                burn.ignite_for_ticks(crate::burning::LAVA_IGNITE_TICKS);
                            }
                        }
                        }
                        let out = burn.tick(standing_in, creative, resistant);
                        if out.damage > 0.0 {
                            vitals.apply_effect_damage(out.damage);
                            publish_health(
                                conn,
                                &mut state,
                                proto,
                                &vitals,
                                &effects,
                                Vec3::new(x, y, z),
                                // Self-facing, per `publish_health`'s own call sites.
                                LOCAL_PLAYER_ENTITY_ID,
                                &username,
                                crate::vitals::DeathCause::OnFire,
                                &mut advancements,
                                player_uuid,
                                Some(crate::vitals::HurtDirection::PURE_ROLL),
                            )
                            .await?;
                        }
                    }
                }

                // Beacon effects use an 80-tick world-time gate and run per
                // connection because `effects` and the wire notification are
                // connection state. The gate is independent of
                // `effects.is_empty()`, allowing a beacon to apply a first
                // effect to a player with no active effects.
                if let Some((px, py, pz)) = player_pos
                    && world.time().game_time % 80 == 0
                {
                    let candidates: Vec<(
                        BlockPos,
                        Option<crate::beacon::BeaconPower>,
                        Option<crate::beacon::BeaconPower>,
                    )> = block_entities
                        .with(|reg| {
                            reg.iter()
                                .filter_map(|(pos, entity)| match entity {
                                    BlockEntity::Beacon(b) if b.primary_effect.is_some() => Some((
                                        *pos,
                                        b.primary_effect.clone(),
                                        b.secondary_effect.clone(),
                                    )),
                                    _ => None,
                                })
                                .collect()
                        });
                    for (pos, primary, secondary) in candidates {
                        // Recomputed live, not read from the block entity's
                        // own (possibly stale — `BeaconData::levels`'s own
                        // doc) stored field: effect application must not
                        // outlive a pyramid the player has since broken.
                        let Some(levels) = resident_beacon_levels(
                            source.get(),
                            pos.x,
                            pos.y,
                            pos.z,
                        ) else {
                            continue;
                        };
                        let Some(beam_unobstructed) = resident_beam_unobstructed(
                            source.get(),
                            pos.x,
                            pos.y,
                            pos.z,
                            384,
                        ) else {
                            continue;
                        };
                        if levels == 0 || !beam_unobstructed {
                            continue;
                        }
                        let (range, application) =
                            crate::beacon::beacon_effects(levels, primary, secondary);
                        // The effect area reaches `range` blocks horizontally,
                        // from `range` below the beacon to the top of the
                        // world. Approximate the unbounded upper edge as
                        // "no lower than `range` below" because
                        // since `ChunkSource` has no height accessor of its
                        // own (`crate::beacon`'s own module doc names the
                        // same gap).
                        let dx = px - f64::from(pos.x);
                        let dz = pz - f64::from(pos.z);
                        let dy = py - f64::from(pos.y);
                        if dx.mul_add(dx, dz * dz) > range * range || dy < -range {
                            continue;
                        }
                        for grant in &application {
                            effects.apply(
                                grant.effect.key(),
                                grant.duration_ticks,
                                grant.amplifier,
                            );
                            apply(
                                conn,
                                &mut state,
                                proto.encode_update_mob_effect(
                                    // Self-facing, per every other
                                    // `encode_update_mob_effect`-adjacent
                                    // call in this loop.
                                    LOCAL_PLAYER_ENTITY_ID,
                                    grant.effect.key(),
                                    grant.amplifier,
                                    grant.duration_ticks,
                                    true,
                                    true,
                                    true,
                                    false,
                                ),
                            )
                            .await?;
                        }
                    }
                }

                // A qualifying player near an occupied village point converts
                // Bad Omen into Raid Omen and records the player's position.
                // When the effect reaches its final tick, that position feeds
                // [`MobSim::create_or_extend_raid`], which queries occupied
                // village points within 64 blocks. Read the duration before
                // decrementing it so the final tick can perform the conversion.
                if let Some((x, y, z)) = player_pos
                    && game_mode != GameMode::Spectator
                {
                    let pos = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
                    let (difficulty, _) = world.difficulty();
                    if difficulty != lodestone_model::Difficulty::Peaceful
                        && let Some(amplifier) = effects.amplifier_of("minecraft:bad_omen")
                        && !mobs.with(|sim| sim.occupied_village_pois_in_range(pos, 64)).is_empty()
                    {
                        effects.remove("minecraft:bad_omen");
                        apply(
                            conn,
                            &mut state,
                            proto.encode_remove_mob_effect(LOCAL_PLAYER_ENTITY_ID, "minecraft:bad_omen"),
                        )
                        .await?;
                        effects.apply("minecraft:raid_omen", 600, amplifier);
                        apply(
                            conn,
                            &mut state,
                            proto.encode_update_mob_effect(
                                LOCAL_PLAYER_ENTITY_ID,
                                "minecraft:raid_omen",
                                amplifier,
                                600,
                                false,
                                true,
                                true,
                                true,
                            ),
                        )
                        .await?;
                        raid_omen_position = Some(pos);
                    }

                    let expiring_raid_omen = effects
                        .get("minecraft:raid_omen")
                        .map(|instance| (instance.duration(), instance.amplifier()));
                    if let Some((1, amplifier)) = expiring_raid_omen
                        && let Some(origin) = raid_omen_position.take()
                    {
                        effects.remove("minecraft:raid_omen");
                        apply(
                            conn,
                            &mut state,
                            proto.encode_remove_mob_effect(LOCAL_PLAYER_ENTITY_ID, "minecraft:raid_omen"),
                        )
                        .await?;
                        mobs.with(|sim| sim.create_or_extend_raid(origin, difficulty, amplifier));
                    }
                }

                // The raid-completion queue carries the effect a player earns for
                // a killing blow. It fires when a raid this player earned a killing
                // blow in reaches `RaidStatus::Victory`. That transition
                // happens inside the shared background sim task, which has no
                // connection's `ActiveEffects` to grant an effect onto —
                // `MobSim::take_hero_of_the_village_grants` is the queue this
                // drains instead (see its own doc). Checked every tick,
                // independent of whether *this* connection's player currently
                // carries Bad Omen or Raid Omen at all: the killing blow that
                // earned the grant may have landed many ticks, and waves,
                // before the raid's last one actually clears.
                for amplifier in mobs.with(|sim| sim.take_hero_of_the_village_grants(player_uuid)) {
                    let amplifier = u32::try_from(amplifier).unwrap_or(0);
                    effects.apply("minecraft:hero_of_the_village", 48_000, amplifier);
                    apply(
                        conn,
                        &mut state,
                        proto.encode_update_mob_effect(
                            LOCAL_PLAYER_ENTITY_ID,
                            "minecraft:hero_of_the_village",
                            amplifier,
                            48_000,
                            true,
                            true,
                            true,
                            false,
                        ),
                    )
                    .await?;
                }

                // The shared mob simulation queues exit-portal geometry and
                // egg placement because it has no connection on which to
                // publish world changes. Resolve the sibling world through
                // `home`, so any connected player's timer can apply the queue.
                for death in mobs.with(|sim| sim.take_dragon_deaths()) {
                    if let Some(destination) = home.get().sibling(crate::dimension::Dimension::End) {
                        // The death queue is drained from this connection's
                        // 20 Hz timer, so its writes must not be the first
                        // access to a cold End column. Admit every target and
                        // its retained-light neighbours before the existing
                        // ordered write sequence below.
                        let mut admission = HashSet::new();
                        let mut admit_position = |pos: BlockPos| {
                            admission.extend(column_admission_footprint(
                                pos.x.div_euclid(16),
                                pos.z.div_euclid(16),
                                1,
                            ));
                        };
                        for (pos, _) in &death.exit_portal_blocks {
                            admit_position(*pos);
                        }
                        for (pos, _) in &death.gateway_blocks {
                            admit_position(*pos);
                        }
                        if death.outcome.place_dragon_egg {
                            admit_position(death.origin);
                        }
                        if !admission.is_empty() {
                            let _ = generate_columns_offloaded(
                                Arc::clone(&destination),
                                admission.into_iter().collect(),
                            )
                            .await;
                        }
                        for (pos, state) in &death.exit_portal_blocks {
                            let state = StateId::from_state_str(state)
                                .expect("dragon exit portal emits a built-in state");
                            destination.set_block(pos.x, pos.y, pos.z, state);
                        }
                        if death.outcome.place_dragon_egg {
                            // Place the egg on the first solid surface above
                            // the portal. Scan down from `origin.y + 33` so a
                            // column that contains player-built blocks uses
                            // its actual top surface.
                            let mut egg_y = death.origin.y + 33;
                            let mut resident_scan = true;
                            while egg_y > death.origin.y {
                                match resident_block_state(
                                    &*destination,
                                    death.origin.x,
                                    egg_y,
                                    death.origin.z,
                                ) {
                                    Some(state) if state.block() == Block::Air => egg_y -= 1,
                                    Some(_) => break,
                                    None => {
                                        resident_scan = false;
                                        break;
                                    }
                                }
                            }
                            if resident_scan {
                                destination.set_block(
                                    death.origin.x,
                                    egg_y + 1,
                                    death.origin.z,
                                    Block::DragonEgg.default_state(),
                                );
                            }
                        }
                        // `death.gateway_blocks` contains the positions from
                        // `outcome.spawn_gateway`'s formula and its shuffled
                        // 20-slice pool. The list is empty when spawning is
                        // disabled or the pool is exhausted. Publish the centre
                        // block's empty destination into the End registry as
                        // well as writing the visible structure; the contact
                        // loop resolves its delayed safe exit on use.
                        for (pos, state) in &death.gateway_blocks {
                            let state = StateId::from_state_str(state)
                                .expect("dragon gateway emits a built-in state");
                            destination.set_block(pos.x, pos.y, pos.z, state);
                            if state.block() == crate::portal::END_GATEWAY_BLOCK
                                && let Some(registries) = destination.world_registries()
                            {
                                registries.block_entities.with(|registry| {
                                    registry.insert(
                                        *pos,
                                        BlockEntity::EndGateway {
                                            exit: None,
                                            exact: false,
                                        },
                                    );
                                });
                            }
                        }
                    }
                }

                // Status effects, ahead of hunger. The order matters for one arm:
                // `hunger`
                // charges exhaustion, so it must land before the exhaustion is spent
                // rather than a tick late.
                //
                // `game_tick` is the entity tick count needed **only** for an
                // infinite effect; a finite one counts against its remaining
                // duration.
                // Apply a newly granted/replaced Health Boost before periodic
                // effects read their regeneration ceiling. The second sync
                // below catches expiry and hidden-effect restoration.
                sync_effect_max_health(conn, &mut state, proto, &mut vitals, &effects).await?;
                if !effects.is_empty() {
                    let out = effects.tick(
                        i32::try_from(world.time().game_time.max(0)).unwrap_or(i32::MAX),
                        vitals.health(),
                        vitals.max_health(),
                    );
                    if out.exhaustion > 0.0 {
                        vitals.add_exhaustion(out.exhaustion);
                    }
                    let saturation_changed = apply_effect_saturation(&mut vitals, out.saturation);
                    // Poison's `health > 1.0` guard is already applied inside the
                    // registry, so this is an unconditional subtraction of an amount
                    // that was only produced when the guard allowed it.
                    let mut moved = saturation_changed;
                    // Tracked separately from `moved`, because regeneration reaches
                    // this publish too and a heal must not flash the screen red or
                    // tilt the camera. This is the one arm where "health changed"
                    // and "a hit landed" genuinely differ.
                    let mut hurt_landed = false;
                    if out.heal > 0.0 {
                        vitals.heal(out.heal);
                        moved = true;
                    }
                    if out.poison_damage > 0.0 {
                        vitals.apply_effect_damage(out.poison_damage);
                        moved = true;
                        hurt_landed = true;
                    }
                    if out.wither_damage > 0.0 {
                        vitals.apply_effect_damage(out.wither_damage);
                        moved = true;
                        hurt_landed = true;
                    }
                    if moved {
                        publish_health(
                            conn,
                            &mut state,
                            proto,
                            &vitals,
                            &effects,
                            // No terrain read backs this arm (status effects tick
                            // regardless of a reported position), so this falls
                            // back to the origin on a connection that has never
                            // moved — see `publish_health`'s own parameter doc.
                            player_pos.map(|(x, y, z)| Vec3::new(x, y, z)).unwrap_or_default(),
                            // Self-facing, per `publish_health`'s own call sites.
                            LOCAL_PLAYER_ENTITY_ID,
                            &username,
                            crate::vitals::DeathCause::Wither,
                            &mut advancements,
                            player_uuid,
                            hurt_landed.then_some(crate::vitals::HurtDirection::PURE_ROLL),
                        )
                        .await?;
                    }
                    sync_effect_max_health(conn, &mut state, proto, &mut vitals, &effects).await?;
                }

                // Hunger, after the air block — vanilla's own order
                // (its own base-tick routine's water-breath block, then
                // the per-player hunger tick. Runs whether or not a
                // position has been reported, unlike drowning: hunger needs no
                // terrain, only the difficulty and a game rule, and a player who
                // has not moved since joining still starves.
                //
                // `!invulnerable`: the exhaustion gate means a
                // creative player accumulates no exhaustion at all and their bar can
                // never move. Skipping the whole tick is equivalent and cheaper —
                // with no exhaustion there is nothing to spend, and the regeneration
                // arms are moot for a player who cannot be hurt.
                if !Abilities::for_mode(game_mode).invulnerable {
                    let (difficulty, _) = world.difficulty();
                    let food_out = vitals.tick_food(difficulty, world.natural_health_regeneration());
                    // A heal or a starve moves health, and a food/saturation change
                    // moves the HUD — either way the client needs the packet, and
                    // `SetHealth` carries all three fields in one.
                    if !food_out.is_empty() {
                        publish_health(
                            conn,
                            &mut state,
                            proto,
                            &vitals,
                            &effects,
                            // Hunger needs no terrain and runs even before the
                            // first movement packet — see the wither arm just
                            // above for the same fallback.
                            player_pos.map(|(x, y, z)| Vec3::new(x, y, z)).unwrap_or_default(),
                            // Self-facing, per `publish_health`'s own call sites.
                            LOCAL_PLAYER_ENTITY_ID,
                            &username,
                            crate::vitals::DeathCause::Starve,
                            &mut advancements,
                            player_uuid,
                            // `food_out.starve`, not `!food_out.is_empty()`: this
                            // publish also carries a pure food/saturation change and
                            // the natural-regeneration heal, neither of which is a
                            // hit.
                            food_out
                                .starve
                                .map(|_| crate::vitals::HurtDirection::PURE_ROLL),
                        )
                        .await?;
                    }
                }

                travel.tick(
                    home, source, block_entities, mobs, player_entity_id,
                    player_pos, game_mode, world,
                );
                if travel.take_end_exit_contact() {
                    connection_travel::begin_end_exit(
                        conn, proto, &mut state, &mut end_exit, &mut preserved_player_fields,
                    ).await?;
                    live_publish_player(
                        live_save, player_store.as_ref(), player_uuid, player_pos, player_rot,
                        world_spawn, &vitals, game_mode, &inventory, &experience,
                        &preserved_player_fields, source.dimension(),
                    );
                }
                republish_inventory(entities.players(), player_uuid, &inventory);
                watch.pass("vitals_tick");
            }

            _ = container_sync_tick.tick() => {
                watch.enter();
                // The piece with no inbound packet driving it at all: the
                // server's unified tick loop (`crate::tick::run_tick_loop`,
                // mutates the registry independently of any
                // connection, so this connection needs its own timer to notice — see
                // `sync_open_container`'s own doc comment.
                publish_open_container(
                    conn, proto, &mut state, block_entities,
                    &mut open_container, &mut container_sync,
                )
                .await?;
                // Keep tick updates for visible columns ordered, but send them
                // outside this timer arm so a burst cannot block socket reads.
                queue_tick_block_updates(
                    &mut pending_tick_updates,
                    &view.delivered,
                    block_ticks.drain_all(),
                );
                // Drain the feed's effect lane: world-tick sounds, particles,
                // and level events. These effects share the feed's single
                // consumer, as described by `BlockTickFeed`.
                for effect in block_ticks.drain_effects_for(player_uuid) {
                    // Handle piston pushes before generic world-effect
                    // encoding. `PistonPlayerPush` has no packet
                    // representation in `encode_world_effect`; it carries the
                    // displacement needed to correct this connection's
                    // tracked position, so intercept it here.
                    if let crate::effects::WorldEffect::PistonPlayerPush { source, dest, push_delta } = effect {
                        if let Some((px, py, pz)) = player_pos
                            && player_overlaps_piston_sweep(px, py, pz, source, dest)
                        {
                            let (nx, ny, nz) =
                                (px + push_delta.x, py + push_delta.y, pz + push_delta.z);
                            let current = player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 });
                            player_pos = Some((nx, ny, nz));
                            apply(
                                conn,
                                &mut state,
                                proto.encode_teleport_with_id(
                                    issue_teleport_id(&mut teleport_acknowledgements),
                                    nx,
                                    ny,
                                    nz,
                                    current.yaw,
                                    current.pitch,
                                ),
                            )
                            .await?;
                        }
                        continue;
                    }
                    apply(conn, &mut state, proto.encode_world_effect(&effect)).await?;
                }
                // Drain explosions emitted by the shared mob simulation. The
                // feed has one consumer for each in-memory world instance.
                for detonation in explosions.drain_all() {
                    apply(
                        conn,
                        &mut state,
                        proto.encode_explode(detonation.centre, detonation.radius),
                    )
                    .await?;
                }
                // The per-entity animation cues for hits the mob sim resolved:
                // vanilla's `broadcastDamageEvent` (which we send as
                // `hurt_animation` — see `ServerProtocol::encode_hurt_animation`
                // for why the route differs and the pixels do not). The entity
                // death event carries the same byte value used by the client.
                //
                // Without this a mob beaten to death never flashed and never tipped
                // over: it simply disappeared when the next entity diff dropped it,
                // which reads as a despawn rather than a kill.
                //
                // **Drained straight off the `MobHandle` rather than through a
                // feed**, unlike the three drains above. The feeds exist to carry
                // world-global events from the *tick task* to a connection; these
                // are already per-entity and the sim is already shared with this
                // task (`apply_attack` mutates it from here), so a feed would add a
                // hop and nothing else. It inherits the same single-consumer
                // caveat: with two connections sharing one sim the first to reach
                // this line takes the queue. The current LAN wiring gives
                // `IntegratedServer::bind`'s LAN worlds get a `MobHandle::default`
                // with no population — and a second player needs per-connection
                // tracking here, not a feed.
                for animation in mobs.with(crate::mobs::MobSim::take_entity_animations) {
                    let directive = match animation {
                        crate::mobs::MobAnimation::Hurt { entity_id } => {
                            // `0.0` is the fixed hurt-animation direction for a
                            // non-player entity; player-specific direction data
                            // is handled by the connection path.
                            proto.encode_hurt_animation(entity_id, 0.0)
                        }
                        crate::mobs::MobAnimation::Died { entity_id } => proto
                            .encode_entity_event(
                                entity_id,
                                crate::protocol::entity_event::DEATH,
                            ),
                    };
                    apply(conn, &mut state, directive).await?;
                }
                // Drain weather transitions published by the world tick loop.
                // The feed has one consumer for each in-memory world instance.
                for event in weather.drain_all() {
                    let (kind, value) = event.wire();
                    apply(conn, &mut state, proto.encode_game_event(kind, value)).await?;
                }
                // Update the sleep vote and deliver night-skip notifications.
                // Both use this connection's regular timer:
                //
                // 1. Feed the voter count. Vanilla excludes spectators
                //    (vanilla's own update-sleeping-players routine); this crate has no
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
                // Drain server-initiated resource-pack pushes. The feed is
                // published by host control paths and has one consumer for
                // each in-memory world instance.
                for push in resource_packs.drain_all() {
                    apply(conn, &mut state, proto.encode_resource_pack_push(&push)).await?;
                }
                // Drain each connection's chat cursor from the shared
                // append-only log, so every connection receives every line.
                if let Some(registry) = entities.players() {
                    for line in registry.chat_since(&mut chat_cursor) {
                        apply(conn, &mut state, proto.encode_system_chat(&line.rendered()))
                            .await?;
                    }
                    // Broadcast arm swings from the same shared log while
                    // excluding the connection that produced each event.
                    for event in registry.swings_since(&mut swing_cursor) {
                        if event.entity_id != player_entity_id {
                            apply(
                                conn,
                                &mut state,
                                proto.encode_animate(event.entity_id, swing_action(event.hand)),
                            )
                            .await?;
                        }
                    }
                    // Command effects another player's command aimed at *this*
                    // connection — `/gamemode creative Steve` typed by someone
                    // else, or `/give @a diamond`.
                    //
                    // A **drain**, not a cursor, and that is the difference from
                    // chat two lines up: the queue is per-uuid and this is its
                    // only reader, so taking it is what makes the delivery
                    // directed. A cursor over a shared log would hand Steve's
                    // game-mode change to everyone.
                    for effect in registry.take_effects(player_uuid) {
                        apply_own_effect(
                            conn,
                            proto,
                            &mut state,
                            &mut game_mode,
                            &mut abilities,
                            &mut inventory,
                            Some(registry),
                            player_uuid,
                            effect,
                            &mut advancements,
                            world,
                            &mut effects,
                            &mut vitals,
                            &mut experience,
                            player_entity_id,
                            &username,
                            &mut player_pos,
                            &mut player_rot,
                            &mut teleport_acknowledgements,
                        )
                        .await?;
                    }
                }
                // Drain host-published plugin-channel broadcasts through each
                // connection's cursor, filtering to channels this client
                // announced. Unsupported channels are skipped.
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
                watch.pass("container_sync_tick");
            }
        }
        // Publish the cancellation-safe snapshot — see `live_publish_player`'s
        // and `crate::live_save::LiveSaveSlot`'s own doc comments. Once per
        // iteration, after whichever arm above completed, so the mirror is at
        // most one packet or timer tick behind whatever the cancellation
        // below would otherwise drop entirely. Placed here rather than inside
        // each arm individually: every arm reaches this same point on a
        // normal completion, and an arm that instead returns via `?` or the
        // disconnect arm's own explicit `return` skips it — correctly, since
        // both of those are real completions with their own save story
        // already (a genuine error, or the `persist_player` call right
        // above).
        live_publish_player(
            live_save,
            player_store.as_ref(),
            player_uuid,
            player_pos,
            player_rot,
            world_spawn,
            &vitals,
            game_mode,
            &inventory,
            &experience,
            &preserved_player_fields,
            source.dimension(),
        );
        // The native locator is deliberately a separate, typed sidecar. It is
        // published in memory only; IntegratedServer::shutdown writes the last
        // snapshot after joining this task, while a real socket disconnect uses
        // the synchronous path above.
        publish_native_player(
            native_player,
            live_save,
            player_pos,
            player_rot,
            world_spawn,
            source.dimension(),
            game_mode,
            &vitals,
            &experience,
            &inventory,
        );
    }
}

/// Advances player vitals for a `wasm32` connection: air supply and drowning,
/// world-border damage, burning, beacon effects, status effects, hunger, and
/// item-use completion. [`crate::browser_timer::BrowserInterval`] supplies the
/// timer boundary, and the beacon sweep reads `block_entities` for eligible
/// effects.
///
/// # Why this function is separate
///
/// The helper keeps the browser timer boundary small while calling the
/// production operations (`PlayerVitals::tick`, `BurnState::tick`,
/// `ActiveEffects::tick`, `finish_consuming`, and `publish_health`).
///
/// # What it excludes
///
/// - **Periodic player save.** `wasm32` has no filesystem or `PlayerDataStore`.
/// - **Deferred block-break continuation.** The wasm32 caller passes `None` for
///   `start_tick`, so `PendingBreak::defer` returns `None` and no deferred break
///   enters this loop.
/// - **Hostile-mob melee damage.** `MobHandle::take_player_hits` has no producer
///   on wasm32, so its queue is empty.
///
/// World-border damage, burning, status effects, and hunger are included because
/// each has a packet-reachable producer: border commands, block reads, or the
/// `item_in_use` arm.
#[allow(clippy::too_many_arguments)]
#[cfg(target_arch = "wasm32")]
async fn wasm_vitals_tick<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    state: &mut State,
    world: &crate::world_state::WorldStateHandle,
    border: &BorderFeed,
    game_mode: GameMode,
    player_uuid: uuid::Uuid,
    username: &str,
    player_pos: Option<(f64, f64, f64)>,
    player_environment: &mut crate::player_environment::PlayerEnvironment,
    sprinting: bool,
    sneaking: bool,
    flying: bool,
    vitals: &mut PlayerVitals,
    inventory: &mut PlayerInventory,
    advancements: &mut AdvancementManager,
    drops_rng: &mut SpawnRng,
    burn: &mut crate::burning::BurnState,
    burn_rng: &mut SpawnRng,
    effects: &mut crate::mob_effects::ActiveEffects,
    item_in_use: &mut Option<ItemInUse>,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    // Read every tracked beacon block entity for the per-connection sweep.
    block_entities: &BlockEntityHandle,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
{
    let invulnerable = Abilities::for_mode(game_mode).invulnerable;

    // Apply periodic eat/drink effects, then finish the item use when
    // `finish_tick` is reached.
    if let Some(started) = item_in_use.clone() {
        let now = mobs.with(|sim| sim.tick_count());
        if now < started.finish_tick
            && let Some(pos) = player_pos
            && let Some(consumable) =
                lodestone_game::consumable::consumable_for_item(&started.item)
        {
            let remaining = u32::try_from(started.finish_tick - now).unwrap_or(u32::MAX);
            let already = started.last_effect_remaining == Some(remaining);
            if !already
                && lodestone_game::consumable::should_emit_consume_effects(
                    consumable.consume_ticks,
                    remaining,
                )
            {
                let seed = i64::from(drops_rng.next_int(i32::MAX));
                let roll = drops_rng.next_f32();
                if let Some(effect) = crate::effects::item_consumed_tick(
                    &started.item,
                    Vec3::new(pos.0, pos.1, pos.2),
                    roll,
                    seed,
                ) {
                    block_ticks.publish_effect_except(player_uuid, effect);
                }
            }
            if let Some(live) = item_in_use.as_mut() {
                live.last_effect_remaining = Some(remaining);
            }
        }
        if now >= started.finish_tick {
            *item_in_use = None;
            if let Some((native, remainder)) =
                finish_consuming(inventory, vitals, &started, game_mode)
            {
                if let Some(pos) = player_pos {
                    let at = Vec3::new(pos.0, pos.1, pos.2);
                    let seed = i64::from(drops_rng.next_int(i32::MAX));
                    if let Some(effect) = crate::effects::item_consume_finished(
                        &started.item,
                        at,
                        drops_rng.next_f32(),
                        seed,
                    ) {
                        block_ticks.publish_effect(effect);
                    }
                    block_ticks.publish_effect(crate::effects::player_burped(
                        at,
                        drops_rng.next_f32(),
                        i64::from(drops_rng.next_int(i32::MAX)),
                    ));
                }
                if let Some(menu_slot) = window_zero_menu_slot(native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                    )
                    .await?;
                }
                apply(
                    conn,
                    state,
                    proto.encode_set_health(
                        vitals.health(),
                        vitals.food().food_level(),
                        vitals.food().saturation(),
                    ),
                )
                .await?;

                // Apply supported food-consumption effects as status-effect
                // frames.
                for grant in crate::mob_effects::food_consume_effects(&started.item) {
                    if drops_rng.next_f32() >= grant.probability {
                        continue;
                    }
                    effects.apply(grant.effect_id, grant.duration, grant.amplifier);
                    apply(
                        conn,
                        state,
                        proto.encode_update_mob_effect(
                            LOCAL_PLAYER_ENTITY_ID,
                            grant.effect_id,
                            grant.amplifier,
                            grant.duration,
                            false,
                            true,
                            true,
                            false,
                        ),
                    )
                    .await?;
                }
                if crate::mob_effects::removes_poison_on_consume(&started.item)
                    && effects.remove("minecraft:poison")
                {
                    apply(
                        conn,
                        state,
                        proto.encode_remove_mob_effect(LOCAL_PLAYER_ENTITY_ID, "minecraft:poison"),
                    )
                    .await?;
                }
            } else if let Some((native, remainder)) =
                finish_drinking_ominous_bottle(inventory, effects, &started, game_mode)
            {
                if let Some(menu_slot) = window_zero_menu_slot(native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                    )
                    .await?;
                }
                apply(
                    conn,
                    state,
                    proto.encode_update_mob_effect(
                        LOCAL_PLAYER_ENTITY_ID,
                        "minecraft:bad_omen",
                        0,
                        120_000,
                        true,
                        true,
                        true,
                        false,
                    ),
                )
                .await?;
            } else if let Some((native, remainder, potion_effects)) =
                finish_drinking_potion(inventory, &started, game_mode)
            {
                if let Some(menu_slot) = window_zero_menu_slot(native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                    )
                    .await?;
                }
                let mut health_changed = false;
                for effect in potion_effects {
                    match effect {
                        crate::mob_effects::SplashEffect::Instant { effect_id, amount } => {
                            match effect_id {
                                lodestone_data::mob_effects::MobEffectId::INSTANT_HEALTH => {
                                    vitals.heal(amount)
                                }
                                lodestone_data::mob_effects::MobEffectId::INSTANT_DAMAGE => {
                                    vitals.apply_effect_damage(amount)
                                }
                                _ => continue,
                            }
                            health_changed = true;
                        }
                        crate::mob_effects::SplashEffect::Timed {
                            effect_id,
                            duration,
                            amplifier,
                        } => {
                            effects.apply(effect_id, duration, amplifier);
                            let effect_name =
                                lodestone_data::mob_effects::mob_effect_name_for(effect_id);
                            apply(
                                conn,
                                state,
                                proto.encode_update_mob_effect(
                                    LOCAL_PLAYER_ENTITY_ID,
                                    effect_name,
                                    amplifier,
                                    duration,
                                    false,
                                    true,
                                    true,
                                    false,
                                ),
                            )
                            .await?;
                        }
                    }
                }
                if health_changed {
                    apply(
                        conn,
                        state,
                        proto.encode_set_health(
                            vitals.health(),
                            vitals.food().food_level(),
                            vitals.food().saturation(),
                        ),
                    )
                    .await?;
                }
            } else if let Some((native, remainder, cleared)) =
                finish_drinking_milk(inventory, effects, &started, game_mode)
            {
                if let Some(menu_slot) = window_zero_menu_slot(native) {
                    apply(
                        conn,
                        state,
                        proto.encode_container_slot(0, 0, menu_slot, remainder.as_ref()),
                    )
                    .await?;
                }
                for effect_id in cleared {
                    apply(
                        conn,
                        state,
                        proto.encode_remove_mob_effect(LOCAL_PLAYER_ENTITY_ID, &effect_id),
                    )
                    .await?;
                }
            }
        }
    }

    // Apply border damage, then process drowning and air supply.
    if let Some((x, y, z)) = player_pos {
        let border_state = border.get();
        if let Some(damage) = border_state.damage_for_position(x, z).filter(|_| !invulnerable) {
            if vitals.apply_border_damage(damage).is_some() {
                publish_health(
                    conn,
                    state,
                    proto,
                    vitals,
                    effects,
                    Vec3::new(x, y, z),
                    LOCAL_PLAYER_ENTITY_ID,
                    username,
                    crate::vitals::DeathCause::OutsideBorder,
                    advancements,
                    player_uuid,
                    Some(crate::vitals::HurtDirection::PURE_ROLL),
                )
                .await?;
            }
        }

        if let Some(eye_in_water) = player_environment.eye_in_water(
            lodestone_physics::Vec3d::new(x, y, z),
            sprinting,
            sneaking,
            flying,
            &|bx, by, bz| resident_block_state(source.get(), bx, by, bz),
        ) {
            // `!invulnerable &&` keeps creative players from depleting air or
            // drowning. A missing resident cell defers this probe until a later
            // timer or movement packet.
            let outcome = tick_player_air_supply(
                vitals,
                eye_in_water,
                invulnerable,
                effects,
            );
            if let Some(air) = outcome.air_changed {
                apply(conn, state, proto.encode_air_supply_update(air)).await?;
            }
            if outcome.damage.is_some() {
                publish_health(
                    conn,
                    state,
                    proto,
                    vitals,
                    effects,
                    Vec3::new(x, y, z),
                    LOCAL_PLAYER_ENTITY_ID,
                    username,
                    crate::vitals::DeathCause::Drown,
                    advancements,
                    player_uuid,
                    Some(crate::vitals::HurtDirection::PURE_ROLL),
                )
                .await?;
            }
        }
    }

    // Burning reads the block at the player's feet, updates burn state, and
    // publishes health when damage applies.
    if let Some((x, y, z)) = player_pos {
        if let Some(feet) = resident_block_state(
            source.get(),
            x.floor() as i32,
            y.floor() as i32,
            z.floor() as i32,
        ) {
            let standing_in = crate::burning::BurnSource::for_block(feet);
            let creative = Abilities::for_mode(game_mode).invulnerable;
            let resistant = effects.amplifier_of("minecraft:fire_resistance").is_some();
            if let Some(source_kind) = standing_in
                && !creative
            {
                match source_kind {
                    crate::burning::BurnSource::Fire | crate::burning::BurnSource::SoulFire => {
                        let ramp = 1 + i32::from(burn_rng.next_f32() < 0.5);
                        burn.fire_ignite(true, ramp);
                    }
                    crate::burning::BurnSource::Lava => {
                        burn.ignite_for_ticks(crate::burning::LAVA_IGNITE_TICKS);
                    }
                }
            }
            let out = burn.tick(standing_in, creative, resistant);
            if out.damage > 0.0 {
                vitals.apply_effect_damage(out.damage);
                publish_health(
                    conn,
                    state,
                    proto,
                    vitals,
                    effects,
                    Vec3::new(x, y, z),
                    LOCAL_PLAYER_ENTITY_ID,
                    username,
                    crate::vitals::DeathCause::OnFire,
                    advancements,
                    player_uuid,
                    Some(crate::vitals::HurtDirection::PURE_ROLL),
                )
                .await?;
            }
        }
    }

    // Beacons run on an 80-tick cadence and feed `effects` before the status
    // effect tick. **Not** gated on `!effects.is_empty()` —
    // a beacon must be able to apply a *first* effect to a player who
    // currently has none.
    if let Some((px, py, pz)) = player_pos
        && world.time().game_time % 80 == 0
    {
        let candidates: Vec<(
            BlockPos,
            Option<crate::beacon::BeaconPower>,
            Option<crate::beacon::BeaconPower>,
        )> = block_entities.with(|reg| {
            reg.iter()
                .filter_map(|(pos, entity)| match entity {
                    BlockEntity::Beacon(b) if b.primary_effect.is_some() => {
                        Some((*pos, b.primary_effect.clone(), b.secondary_effect.clone()))
                    }
                    _ => None,
                })
                .collect()
        });
        for (pos, primary, secondary) in candidates {
            let Some(levels) = resident_beacon_levels(source.get(), pos.x, pos.y, pos.z) else {
                continue;
            };
            let Some(beam_unobstructed) = resident_beam_unobstructed(
                source.get(),
                pos.x,
                pos.y,
                pos.z,
                384,
            ) else {
                continue;
            };
            if levels == 0 || !beam_unobstructed {
                continue;
            }
            let (range, application) =
                crate::beacon::beacon_effects(levels, primary, secondary);
            let dx = px - f64::from(pos.x);
            let dz = pz - f64::from(pos.z);
            let dy = py - f64::from(pos.y);
            if dx.mul_add(dx, dz * dz) > range * range || dy < -range {
                continue;
            }
            for grant in &application {
                effects.apply(
                    grant.effect.key(),
                    grant.duration_ticks,
                    grant.amplifier,
                );
                apply(
                    conn,
                    state,
                    proto.encode_update_mob_effect(
                        LOCAL_PLAYER_ENTITY_ID,
                        grant.effect.key(),
                        grant.amplifier,
                        grant.duration_ticks,
                        true,
                        true,
                        true,
                        false,
                    ),
                )
                .await?;
            }
        }
    }

    // Tick status effects before hunger so their exhaustion is included when
    // hunger consumes exhaustion.
    // Apply a newly granted/replaced Health Boost before periodic effects read
    // their regeneration ceiling. The second sync below catches expiry and
    // hidden-effect restoration.
    sync_effect_max_health(conn, state, proto, vitals, effects).await?;
    if !effects.is_empty() {
        let out = effects.tick(
            i32::try_from(world.time().game_time.max(0)).unwrap_or(i32::MAX),
            vitals.health(),
            vitals.max_health(),
        );
        if out.exhaustion > 0.0 {
            vitals.add_exhaustion(out.exhaustion);
        }
        let mut moved = apply_effect_saturation(vitals, out.saturation);
        let mut hurt_landed = false;
        if out.heal > 0.0 {
            vitals.heal(out.heal);
            moved = true;
        }
        if out.poison_damage > 0.0 {
            vitals.apply_effect_damage(out.poison_damage);
            moved = true;
            hurt_landed = true;
        }
        if out.wither_damage > 0.0 {
            vitals.apply_effect_damage(out.wither_damage);
            moved = true;
            hurt_landed = true;
        }
        if moved {
            publish_health(
                conn,
                state,
                proto,
                vitals,
                effects,
                // Terrain data is not needed for this health publication.
                player_pos.map(|(x, y, z)| Vec3::new(x, y, z)).unwrap_or_default(),
                LOCAL_PLAYER_ENTITY_ID,
                username,
                crate::vitals::DeathCause::Wither,
                advancements,
                player_uuid,
                hurt_landed.then_some(crate::vitals::HurtDirection::PURE_ROLL),
            )
            .await?;
        }
        sync_effect_max_health(conn, state, proto, vitals, effects).await?;
    }

    // Hunger runs after air checks and does not require a reported position.
    if !Abilities::for_mode(game_mode).invulnerable {
        let (difficulty, _) = world.difficulty();
        let food_out = vitals.tick_food(difficulty, world.natural_health_regeneration());
        if !food_out.is_empty() {
            publish_health(
                conn,
                state,
                proto,
                vitals,
                effects,
                player_pos.map(|(x, y, z)| Vec3::new(x, y, z)).unwrap_or_default(),
                LOCAL_PLAYER_ENTITY_ID,
                username,
                crate::vitals::DeathCause::Starve,
                advancements,
                player_uuid,
                food_out.starve.map(|_| crate::vitals::HurtDirection::PURE_ROLL),
            )
            .await?;
        }
    }

    Ok(())
}

/// Drives a `wasm32` play connection with [`dispatch_play_packet`]. A
/// `crate::browser_timer::BrowserInterval` handles status, effects, hunger,
/// and item-consumption work, together with feed drains that have browser
/// producers.
///
/// Browser connections use an in-process `DuplexStream`, so the peer cannot go
/// quiet independently and keep-alive expiration is not applicable. The
/// world-spawn ticket still has a 20-tick countdown driven by
/// `ChunkStore::maybe_tick_tickets`; the `PLAYER_LOADING`/`PLAYER_SIMULATION`
/// pair uses `timeout: 0` and does not expire. A browser world therefore pays
/// only for the world-spawn ring when no refresh reaches the ticket store.
#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
async fn serve_play<T, P, S, E>(
    service: &crate::connection_service::ConnectionService,
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    home_source: SourceRef<'_, S>,
    entities: &E,
    mut state: State,
    initial_teleport_id: Option<i32>,
    mut streamer: EntityStreamer,
    mut player_list: PlayerListStreamer,
    // Keep the ticket guard alive for the entire connection. Movement remains
    // packet-driven through `FallTracker`; the browser timer separately runs
    // the shared entity diff for world changes that occur while idle.
    player_ticket: Option<PlayerTicket>,
    _initial_swing_cursor: Option<u64>,
    // The guard withdraws this connection's `PLAYER_LOADING` and
    // `PLAYER_SIMULATION` tickets when the task exits. Move it with each
    // tracked-view recenter or radius change so residency follows the player.
    mut player_ticket_guard: PlayerTicketGuard,
    mut view: ViewTracker,
    username: String,
    // World spawn for death-screen respawn. The join computation may inspect up
    // to 121 columns, so `apply_client_command` reuses this value when no
    // usable per-player bed point exists.
    world_spawn: Vec3,
    mut chunks_sent: usize,
    mut join_stream: crate::join_scheduler::JoinChunkStream<S>,
    // Optional per-stage join timing. Browser builds normally leave this off.
    join_trace: Option<Arc<JoinTrace>>,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    // Inbound placement packets publish neighbour-update requests here. The
    // browser timer below also drains the world-tick block lane, so changes
    // made while the client is idle reach the wire without waiting for input.
    block_ticks: &BlockTickFeed,
    // The browser timer drains explosions emitted by the shared world tick.
    explosions: &ExplosionFeed,
    // Weather changes are world-tick events. They remain a follow-up for this
    // connection loop because the shared weather feed is currently drained by
    // the native container-sync path.
    _weather: &WeatherFeed,
    // Bed clicks and wake-up packets are handled here. Voter counts and
    // skipped-night notifications require a container-sync timer, which is
    // not present on the browser target.
    sleep_vote: &SleepVote,
    _sleep_feed: &SleepFeed,
    // Commands are packet-driven: a chat-command frame arrives, the sink
    // answers, and system chat is sent back through the same dispatch path.
    commands: CommandSession,
    // Advancements and statistics are packet-driven: inbound packets update
    // criteria or request statistics, and the response uses the same dispatch
    // path.
    mut advancements: AdvancementManager,
    player_uuid: uuid::Uuid,
    // Border damage is produced by `BorderFeed::with`; the browser vitals
    // timer applies it alongside drowning, burning, effects, and hunger.
    border: &BorderFeed,
    // Resource-pack pushes have no browser timer producer, so this feed is not
    // drained by the browser loop.
    _resource_packs: &ResourcePackPushFeed,
    // Channel registration and inbound dispatch are packet-driven here.
    // Broadcast-queue draining requires a container-sync timer, so browser
    // connections do not receive queued broadcasts from this loop.
    client_channels: &mut ClientChannels,
    plugin_channels: &PluginChannelRegistry,
    // The connection's game mode. The `change_game_mode` and `/gamemode` arms
    // mutate it locally.
    mut game_mode: GameMode,
    // The world's shared game rules, difficulty, and clock; `run_tick_loop`
    // updates this handle for every connection.
    world: &crate::world_state::WorldStateHandle,
    // This target has no filesystem-backed player store. The slot remains in
    // the signature so the shared connection setup can pass the same state;
    // no browser code publishes it.
    _live_save: &crate::live_save::LiveSaveSlot,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    let mut pending_keep_alive: Option<i64> = None;
    let mut pending_break: Option<PendingBreak> = None;
    let mut pending_prediction_ack = connection_prediction::PendingPredictionAck::default();
    let mut pending_relights = PendingRelights::default();
    let mut pending_tick_updates = VecDeque::new();
    let mut teleport_acknowledgements = initial_teleport_id.map(TeleportAcknowledgements::after_initial);
    let mut sprinting = false;
    let mut sneaking = false;
    let mut player_environment = crate::player_environment::PlayerEnvironment::default();
    let mut bow_draw: Option<BowDraw> = None;
    // `wasm_vitals_tick` applies item-use completion rules from its browser
    // timer, so a bite started here reaches its completion result.
    let mut item_in_use: Option<ItemInUse> = None;
    // `player_pos` feeds movement and vital checks. The browser timer applies
    // drowning, border damage, burning, status effects, and hunger; fall damage
    // remains driven by inbound `PlayerMoved` packets.
    let mut player_pos: Option<(f64, f64, f64)> = None;
    let mut client_movement = ClientMovement::default();
    // A protocol with no player-loaded packet is loaded from the start.
    let mut client_loaded = !proto.sends_player_loaded();
    let mut client_load_wait = 0;
    let mut abilities = Abilities::for_mode(game_mode);
    // The rotation is stored alongside `player_pos` — see `dispatch_play_packet`'s own
    // parameter comment.
    let mut player_rot: Option<Rotation> = None;
    let mut vitals = PlayerVitals::default();
    let mut fall = FallTracker::default();
    let mut inventory = PlayerInventory::default();
    republish_inventory(entities.players(), player_uuid, &inventory);
    apply(
        conn,
        &mut state,
        proto.encode_recipe_book_add(&recipe_book_snapshot(&inventory), true),
    )
    .await?;
    // Opening and clicking a window are packet-driven here. Background
    // container synchronization requires the native container-sync timer and
    // is not run by the browser loop.
    let mut open_container: Option<OpenContainer> = None;
    let mut open_merchant: Option<OpenMerchant> = None;
    let mut container_sync = ContainerSync::default();
    let mut next_window_id: i32 = 0;
    // Composter rolls have no timer or native-only dependency on this target.
    let mut composter_rng = SpawnRng::new(COMPOSTER_BEHAVIOR_SEED);
    let mut bone_meal_rng = SpawnRng::new(BONE_MEAL_BEHAVIOR_SEED);
    // Browser builds have no filesystem-backed player store, so experience
    // starts at its default values.
    let mut experience = crate::experience::PlayerExperience::default();
    let mut take_xp_delay: i32 = 0;
    let mut effects = crate::mob_effects::ActiveEffects::new();
    let mut burn = crate::burning::BurnState::new();
    // The fire-contact ramp draws one value from the inclusive range `1..=3`.
    // Keep that draw on its own stream so standing in fire cannot shift which
    // roll a later block drop or composter insert sees.
    let mut burn_rng = SpawnRng::new(BURN_BEHAVIOR_SEED);
    // Block-drop rolls have no timer or native-only dependency on this target.
    let mut drops_rng = SpawnRng::new(crate::block_drops::BLOCK_DROPS_BEHAVIOR_SEED);
    // The per-player respawn point has no timer or native-only dependency.
    let mut respawn: Option<RespawnPoint> = None;
    // Leaving the End; this target keeps no player file, so only the in-session
    // credits flag is tracked.
    let mut end_exit = connection_travel::EndExit::new(false);
    let mut end_exit_preserved: Vec<(String, lodestone_core::Nbt)> = Vec::new();
    // The night-skip vote uses the player's roster key. Browser packet handlers
    // can register and wake voters; timer-fed vote counts remain native-only.
    let player_entity_id =
        player_ticket.as_ref().map_or(LOCAL_PLAYER_ENTITY_ID, |t| t.entity_id());
    // The initial join dump is unacknowledged, so this gate begins `true`.
    let mut awaiting_chunk_batch_ack = true;
    let mut pending_chunk_batches: VecDeque<PendingChunkBatch> = VecDeque::new();
    // Outgoing chat waits here until the shared broadcast queue can be drained.
    let mut outgoing_chat: Vec<String> = Vec::new();
    // Session announcements are stored for secure-profile validation.
    let mut chat_session: Option<crate::chat_session::ServerChatSession> = None;

    // Send the window-0 inventory snapshot before draining the inline join
    // stream. Browser inventory has default values because no player store is
    // available; the snapshot establishes the menu state used by later clicks.
    apply(conn, &mut state, join_inventory_snapshot(proto, &inventory)).await?;
    // Send the initial experience snapshot. Browser experience has default
    // values because no filesystem restore is available; explicit zeroes
    // initialize the client's bar.
    apply(conn, &mut state, join_experience(proto, &experience)).await?;
    republish_experience(entities.players(), player_uuid, &experience);
    // Send the armor/attribute snapshot derived from the current inventory so
    // the client can initialize its derived armor display.
    apply(conn, &mut state, join_attributes(proto, &inventory)).await?;

    // The browser timer uses a macrotask via the active page/worker global's
    // `setTimeout`; it drives both player vitals and publication of world
    // changes once per `WASM_VITALS_TICK_INTERVAL` period.
    let mut vitals_interval =
        crate::browser_timer::BrowserInterval::new(WASM_VITALS_TICK_INTERVAL);
    let browser_play_started = lodestone_time::Instant::now();
    let mut browser_vitals_ticks = 0_u64;
    let mut cooperative_relights = proto.detached_resident_light_compute().is_some();
    let mut cooperative_relight: Option<CooperativeRelight<'_>> = None;
    let mut connection_probe = crate::connection_progress::ConnectionProbe::start();
    use crate::connection_progress::ConnectionActivity;
    let mut pending_join_encodes = PendingJoinEncodes::new();
    let mut travel = connection_travel::TravelController::new(source);
    loop {
        service.admit_pass().await;
        travel.promote();
        let home = home_source;
        let active_source = travel.source();
        let source = match active_source.as_ref() {
            Some(other) => SourceRef::Dimension(other),
            None => home,
        };
        let dimension_handles = dimension_scoped_handles(active_source.as_ref());
        let block_entities = dimension_handles.block_entities.as_ref().unwrap_or(block_entities);
        let block_ticks = dimension_handles.block_ticks.as_ref().unwrap_or(block_ticks);
        let active_entities = ActiveEntities::new(world, entities, source.dimension());
        let mobs = active_entities.runtime.as_ref().map_or(mobs, |runtime| runtime.mobs());
        let entities = &active_entities;
        if join_stream.is_done() && pending_join_encodes.is_empty() {
            world.mark_initial_view_drained();
        }
        if let Some(probe) = connection_probe.as_mut() {
            probe.observe(crate::connection_progress::ConnectionProgress {
                running: true,
                elapsed: std::time::Duration::ZERO,
                passes: 0,
                client_loaded,
                center: view.center,
                radius: view.radius,
                owed_columns: view.loaded.len(),
                delivered_columns: view.delivered.len(),
                chunks_sent,
                remaining: join_stream.remaining() + usize::from(!pending_join_encodes.is_empty()),
                activity: ConnectionActivity::Select,
                activity_elapsed: std::time::Duration::ZERO,
                target: None,
                packet_id: None,
            });
        }
        let activity = |phase, target, packet_id| {
            if let Some(probe) = connection_probe.as_ref() {
                probe.activity(phase, target, packet_id);
            }
        };
        if cooperative_relights && cooperative_relight.is_none() && pending_relights.ready() {
            if pending_relights.front().is_some_and(|coordinate| !view.delivered.contains(&coordinate)) {
                pending_relights.pop_front();
            } else {
                let batch = pending_relights.batch(&view.delivered, true);
                let coordinates = batch.coordinates.clone();
                pending_relights.admit(&batch);
                let owned_source = source.shared_arc();
                let borrowed_home = home.get();
                cooperative_relight = Some(CooperativeRelight {
                    batch,
                    future: Box::pin(async move {
                        let source = owned_source.as_deref().unwrap_or(borrowed_home);
                        crate::worldgen_progress::measure_polls(
                            WorldgenTimingPhase::ConnectionRelight,
                            std::pin::pin!(compute_cooperative_relight(proto, source, coordinates)),
                        ).await
                    }),
                });
            }
        }
        if let Some(error) = world.initial_seed_error() {
            return return_initial_seed_error(conn, proto, &mut state, None, error).await;
        }
        let packet = tokio::select! {
            error = world.wait_initial_seed_failure() => {
                return return_initial_seed_error(conn, proto, &mut state, None, error).await;
            }
            prepared = std::future::poll_fn(|cx| travel.poll_prepared(cx, player_pos)), if travel.is_preparing() => {
                activity(ConnectionActivity::Publication, None, None);
                let Some(prepared) = prepared? else { continue; };
                let dimension_changed = matches!(&prepared, connection_travel::PreparedTravel::Dimension { .. });
                if dimension_changed { cooperative_relight = None; }
                if connection_travel::commit(
                    &mut travel, prepared, conn, proto, source, home, &mut state, &mut view,
                    &mut join_stream, &mut pending_join_encodes, &mut pending_chunk_batches,
                    &mut awaiting_chunk_batch_ack, &mut pending_relights, &mut pending_tick_updates,
                    &mut teleport_acknowledgements, &mut player_pos, player_rot,
                    &mut client_movement, &mut fall, &mut client_loaded, game_mode, world,
                    &mut player_ticket_guard, &mut streamer, entities.players(), player_entity_id,
                ).await?.is_some() && dimension_changed {
                    pending_break = None;
                    bow_draw = None;
                    item_in_use = None;
                    open_container = None;
                    open_merchant = None;
                }
                continue;
            }
            _ = std::future::ready(()), if !pending_tick_updates.is_empty() => {
                activity(ConnectionActivity::TickUpdates, None, None);
                send_pending_tick_block_updates(
                    conn,
                    proto,
                    &mut state,
                    &view.delivered,
                    &mut pending_relights,
                    &mut pending_tick_updates,
                )
                .await?;
                None
            }
            result = std::future::poll_fn(|cx| match cooperative_relight.as_mut() {
                Some(job) => job.future.as_mut().poll(cx),
                None => std::task::Poll::Pending,
            }), if cooperative_relight.is_some() => {
                activity(ConnectionActivity::Relight, None, None);
                let batch = cooperative_relight.take().expect("the completed relight owns its batch").batch;
                match result {
                    RelightBatchOutcome::Committed(lights) => send_committed_relights(
                        conn, proto, source.get(), &mut state, &view.delivered,
                        &mut pending_relights, lights,
                    ).await?,
                    RelightBatchOutcome::Deferred => pending_relights.defer(&batch),
                    RelightBatchOutcome::Unsupported => {
                        cooperative_relights = false;
                        pending_relights.defer(&batch);
                    }
                    RelightBatchOutcome::Failed(error) => return Err(error.into()),
                }
                None
            }
            _ = std::future::ready(()), if !cooperative_relights && pending_relights.ready() => {
                activity(ConnectionActivity::Relight, None, None);
                send_next_relight(
                    conn,
                    proto,
                    source.get(),
                    &mut state,
                    &view.delivered,
                    &mut pending_relights,
                )
                .await?;
                None
            }
            packet = conn.read_packet() => {
                match packet? {
                    Some(packet) => Some(packet),
                    // Clean disconnect. Browser connections have no
                    // filesystem-backed player store to persist here.
                    None => return Ok(ServeSummary { username, chunks_sent, inventory }),
                }
            }
            // Cancel-safe timer polling: a ready packet leaves the interval's
            // deadline untouched, so no timer event is lost.
            _ = vitals_interval.tick() => {
                activity(ConnectionActivity::Vitals, None, None);
                if let Some(sequence) = pending_prediction_ack.take() {
                    apply(conn, &mut state, proto.encode_block_changed_ack(sequence)).await?;
                }
                tick_client_load_timeout(&mut client_loaded, &mut client_load_wait);
                if player_tick_ready(world, client_loaded) {
                    wasm_vitals_tick(
                        conn,
                        proto,
                        source,
                        &mut state,
                        world,
                        border,
                        game_mode,
                        player_uuid,
                        &username,
                        player_pos,
                        &mut player_environment,
                        sprinting,
                        sneaking,
                        abilities.flying,
                        &mut vitals,
                        &mut inventory,
                        &mut advancements,
                        &mut drops_rng,
                        &mut burn,
                        &mut burn_rng,
                        &mut effects,
                        &mut item_in_use,
                        mobs,
                        block_ticks,
                        block_entities,
                    )
                    .await?;
                    travel.tick(
                        home, source, block_entities, mobs, player_entity_id,
                        player_pos, game_mode, world,
                    );
                    if travel.take_end_exit_contact() {
                        connection_travel::begin_end_exit(
                            conn, proto, &mut state, &mut end_exit, &mut end_exit_preserved,
                        ).await?;
                    }
                    browser_vitals_ticks = browser_vitals_ticks.saturating_add(1);
                    if browser_vitals_ticks == 1 || browser_vitals_ticks.is_multiple_of(20) {
                        tracing::debug!(
                            elapsed_ms = browser_play_started.elapsed().as_millis(),
                            vitals_ticks = browser_vitals_ticks,
                            world_tick = world.time().game_time,
                            chunks_sent,
                            join_remaining = join_stream.remaining(),
                            "browser play-loop heartbeat",
                        );
                    }
                }
                republish_inventory(entities.players(), player_uuid, &inventory);
                // The world tick can publish changes without inbound packets.
                publish_open_container(
                    conn, proto, &mut state, block_entities,
                    &mut open_container, &mut container_sync,
                )
                .await?;
                queue_tick_block_updates(
                    &mut pending_tick_updates,
                    &view.delivered,
                    block_ticks.drain_all(),
                );
                // Explosions have the same producer/consumer shape as block
                // changes: the shared world tick publishes them independently
                // of client input, so the timer must forward them as well.
                for detonation in explosions.drain_all() {
                    apply(
                        conn,
                        &mut state,
                        proto.encode_explode(detonation.centre, detonation.radius),
                    )
                    .await?;
                }
                // Entity snapshots are the browser's item/mob visibility
                // path. Run the same diff used by the native connection timer
                // so a dropped item appears and falls even when the player
                // sends no movement packets.
                republish_effect_entity_flags(entities.players(), player_ticket.as_ref(), &effects);
                for directive in stream_pass(
                    proto,
                    entities,
                    &mut streamer,
                    &mut player_list,
                    player_ticket.as_ref(),
                ) {
                    apply(conn, &mut state, directive).await?;
                }
                None
            }
            encoded = std::future::poll_fn(|cx| pending_join_encodes.poll_next(cx)), if !pending_join_encodes.is_empty() => {
                let ((cx, cz), (incarnation, directive)) = match encoded {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        return return_chunk_encode_error(conn, proto, &mut state, Some(0), error)
                            .await;
                    }
                };
                if !view.needs_delivery((cx, cz), incarnation, directive.stage) {
                    continue;
                }
                {
                    let _timing = PhaseTimer::start(WorldgenTimingPhase::WireSend, 1);
                    activity(ConnectionActivity::JoinBegin, Some((cx, cz)), None);
                    apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                    activity(ConnectionActivity::JoinColumn, Some((cx, cz)), None);
                    if !send_encoded_column(conn, &mut state, &mut view, (cx, cz), incarnation, directive).await? {
                        apply(conn, &mut state, proto.end_chunk_batch(0)).await?;
                        continue;
                    }
                    activity(ConnectionActivity::JoinEnd, Some((cx, cz)), None);
                    apply(conn, &mut state, proto.end_chunk_batch(1)).await?;
                }
                if let Some(trace) = join_trace.as_ref() {
                    trace.mark("delivered", cx, cz);
                }
                chunks_sent += 1;
                if chunks_sent == 1 || chunks_sent.is_multiple_of(16)
                    || (join_stream.is_done() && pending_join_encodes.is_empty())
                {
                    crate::worldgen_progress::emit(
                        crate::worldgen_progress::WorldgenProgress::wire_delivered(
                            (cx, cz),
                            chunks_sent,
                            join_stream.remaining(),
                        ),
                    );
                    tracing::debug!(
                        elapsed_ms = browser_play_started.elapsed().as_millis(),
                        chunks_sent,
                        join_remaining = join_stream.remaining(),
                        world_tick = world.time().game_time,
                        "browser join stream progress",
                    );
                }
                continue;
            }
            next = async {
                tokio::select! {
                    next = join_stream.next(source) => Some(next),
                    _ = lodestone_time::browser_sleep(
                        crate::join_scheduler::JOIN_STREAM_SERVICE_BUDGET,
                    ) => None,
                }
            }, if pending_join_encodes.can_admit() && !join_stream.is_done() => {
                let Some(next) = next else {
                    continue;
                };
                let Some(((cx, cz), payload)) = (match next {
                    Ok(next) => next,
                    Err(error) => {
                        return return_chunk_encode_error(conn, proto, &mut state, Some(0), error)
                            .await;
                    }
                }) else {
                    continue;
                };
                activity(ConnectionActivity::JoinAdmission, Some((cx, cz)), None);
                let Some(incarnation) = view.incarnation((cx, cz)) else {
                    continue;
                };
                if let Some(owned_source) = source.shared_arc() {
                    let trace = join_trace.clone();
                    pending_join_encodes.push(true, Box::pin(async move {
                        encode_column_owned(proto, owned_source, cx, cz, trace, payload)
                            .await
                            .map(|directive| ((cx, cz), (incarnation, directive)))
                    }));
                    continue;
                }
                let directive = match encode_column(
                    proto,
                    source,
                    cx,
                    cz,
                    join_trace.as_deref(),
                    payload,
                )
                .await {
                    Ok(directive) => directive,
                    Err(error) => {
                        return return_chunk_encode_error(conn, proto, &mut state, Some(0), error)
                            .await;
                    }
                };
                {
                    let _timing = PhaseTimer::start(WorldgenTimingPhase::WireSend, 1);
                    activity(ConnectionActivity::JoinBegin, Some((cx, cz)), None);
                    apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                    activity(ConnectionActivity::JoinColumn, Some((cx, cz)), None);
                    if !send_encoded_column(conn, &mut state, &mut view, (cx, cz), incarnation, directive).await? {
                        apply(conn, &mut state, proto.end_chunk_batch(0)).await?;
                        continue;
                    }
                    activity(ConnectionActivity::JoinEnd, Some((cx, cz)), None);
                    apply(conn, &mut state, proto.end_chunk_batch(1)).await?;
                }
                if let Some(trace) = join_trace.as_ref() {
                    trace.mark("delivered", cx, cz);
                }
                chunks_sent += 1;
                if chunks_sent == 1 || chunks_sent.is_multiple_of(16) || join_stream.is_done() {
                    crate::worldgen_progress::emit(
                        crate::worldgen_progress::WorldgenProgress::wire_delivered(
                            (cx, cz),
                            chunks_sent,
                            join_stream.remaining(),
                        ),
                    );
                    tracing::debug!(
                        elapsed_ms = browser_play_started.elapsed().as_millis(),
                        chunks_sent,
                        join_remaining = join_stream.remaining(),
                        world_tick = world.time().game_time,
                        "browser join stream progress",
                    );
                }
                continue;
            }
        };
        if let Some((packet_id, payload)) = packet {
            activity(ConnectionActivity::Dispatch, None, Some(packet_id));
            let mut dimension_reset = None;
            {
                let dispatch = std::pin::pin!(dispatch_play_packet(
                    conn,
                    proto,
                    source,
                    home,
                    &mut state,
                    proto.retains_initial_column_light().then_some(&mut pending_relights),
                    &mut view,
                    &player_ticket_guard,
                    &mut pending_keep_alive,
                    &mut pending_break,
                    &mut pending_prediction_ack,
                    &mut teleport_acknowledgements,
                    &mut player_pos,
                    &mut client_movement,
                    &mut player_rot,
                    &mut fall,
                    &mut vitals,
                    &mut burn,
                    world,
                    &mut inventory,
                    block_entities,
                    &mut open_container,
                    &mut open_merchant,
                    &mut container_sync,
                    &mut next_window_id,
                    mobs,
                    &mut sprinting,
                    &mut sneaking,
                    &mut awaiting_chunk_batch_ack,
                    &mut pending_chunk_batches,
                    Some(&mut join_stream),
                    &commands,
                    &mut advancements,
                    player_uuid,
                    false,
                    &mut outgoing_chat,
                    &mut chat_session,
                    entities.players(),
                    block_ticks,
                    _resource_packs,
                    &mut client_loaded,
                    &mut composter_rng,
                    &mut bone_meal_rng,
                    &mut experience,
                    &mut effects,
                    &mut drops_rng,
                    client_channels,
                    plugin_channels,
                    &mut game_mode,
                    &mut abilities,
                    &mut respawn,
                    sleep_vote,
                    border,
                    player_entity_id,
                    &username,
                    world_spawn,
                    // `None`: no timer tick counter is available for dig duration.
                    // Hardness and range still validate; only the timing check is
                    // skipped.
                    None,
                    &mut bow_draw,
                    &mut item_in_use,
                    &mut dimension_reset,
                    &mut end_exit,
                    packet_id,
                    &payload,
                ));
                crate::worldgen_progress::measure_polls(
                    WorldgenTimingPhase::ConnectionDispatch, dispatch,
                ).await?;
            }
            activity(ConnectionActivity::Publication, None, Some(packet_id));
            player_tick_ready(world, client_loaded);
            republish_inventory(entities.players(), player_uuid, &inventory);
            if end_exit.take_respawn() {
                dimension_reset = connection_travel::perform_respawn(
                    conn, proto, &mut state, home.get(), source.get(), &mut respawn, world_spawn, game_mode,
                    &mut teleport_acknowledgements, block_ticks, true,
                ).await?;
            }
            if let Some(connection_travel::DimensionReset { target, route }) = dimension_reset.take() {
                cooperative_relight = None;
                let (destination, arrival_dimension) = match &route {
                    crate::respawn::Route::Home => (home.get(), home.dimension()),
                    crate::respawn::Route::Current => (source.get(), source.dimension()),
                    crate::respawn::Route::Sibling(other) => (
                        other.as_ref(),
                        other.dimension().unwrap_or(crate::dimension::Dimension::Overworld),
                    ),
                };
                let ticket_transfer = connection_travel::prepare_ticket_transfer(
                    &player_ticket_guard, home.get(), destination, target, view.radius,
                )?;
                connection_travel::reset_stream(
                    conn, proto, &mut state, target, &mut view, &mut join_stream,
                    &mut pending_join_encodes, &mut pending_chunk_batches,
                    &mut awaiting_chunk_batch_ack, &mut pending_relights, &mut pending_tick_updates,
                ).await?;
                for directive in streamer.reset_dimension(proto) {
                    apply(conn, &mut state, directive).await?;
                }
                connection_travel::finish_ticket_transfer(
                    &mut player_ticket_guard, ticket_transfer, source.get(), destination,
                );
                connection_travel::reset_player(
                    target, arrival_dimension, &mut player_pos, &mut client_movement,
                    &mut fall, &mut client_loaded, proto.sends_player_loaded(), world, entities.players(), player_entity_id,
                );
                match route {
                    crate::respawn::Route::Home => travel.stage(connection_travel::Destination::Home),
                    crate::respawn::Route::Current => {}
                    crate::respawn::Route::Sibling(other) => {
                        travel.stage(connection_travel::Destination::Dimension(other));
                    }
                }
                pending_break = None;
                bow_draw = None;
                item_in_use = None;
                open_container = None;
                open_merchant = None;
                continue;
            }
            // Flush advancement changes caused by the packet just dispatched.
            if let Some(update) = advancements.flush_dirty(player_uuid, true) {
                apply(conn, &mut state, proto.encode_update_advancements(&update)).await?;
            }
            // Publish chat to the shared registry when one exists. Without a
            // registry, echo it directly to this connection.
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
            // Update the player's streamed position from the packet state.
            if let (Some(ticket), Some(registry), Some((x, y, z))) =
                (player_ticket.as_ref(), entities.players(), player_pos)
            {
                registry.set_position(ticket.entity_id(), Vec3::new(x, y, z));
                registry.set_on_ground(ticket.entity_id(), client_movement.on_ground);
            }
            // The pickup sweep is packet-driven, so run it after each dispatched
            // packet while the player's position is available.
            if let Some((x, y, z)) = player_pos {
                let pickups = collect_nearby_items(
                    mobs,
                    &mut inventory,
                    Vec3::new(x, y, z),
                    &mut advancements,
                    player_uuid,
                    world.time().game_time.saturating_mul(50),
                );
                // Send pickup frames before slot updates and entity streaming so
                // the client can animate an item entity that still exists.
                for take in &pickups.takes {
                    apply(
                        conn,
                        &mut state,
                        proto.encode_take_item_entity(
                            take.item_entity_id,
                            // Use the local player entity id for the pickup reply.
                            LOCAL_PLAYER_ENTITY_ID,
                            take.amount,
                        ),
                    )
                    .await?;
                }
                for native in pickups.changed {
                    if let Some(menu_slot) = window_zero_menu_slot(native) {
                        apply(
                            conn,
                            &mut state,
                            proto.encode_container_slot(0, 0, menu_slot, inventory.native(native)),
                        )
                        .await?;
                    }
                }
                // Absorb nearby experience orbs as part of the packet-driven sweep;
                // browser players receive the resulting pickup behavior here.
                if let Some(absorbed) = collect_nearby_orbs(
                    mobs,
                    Vec3::new(x, y, z),
                    &mut experience,
                    &mut take_xp_delay,
                ) {
                    apply(
                        conn,
                        &mut state,
                        proto.encode_take_item_entity(
                            absorbed.orb_entity_id,
                            LOCAL_PLAYER_ENTITY_ID,
                            1,
                        ),
                    )
                    .await?;
                    republish_experience(entities.players(), player_uuid, &experience);
                    apply(
                        conn,
                        &mut state,
                        proto.encode_set_experience(
                            experience.progress(),
                            experience.level(),
                            experience.total(),
                        ),
                    )
                    .await?;
                }
            }
            // Publish the latest player rotation to the entity registry.
            if let (Some(ticket), Some(registry), Some(rotation)) =
                (player_ticket.as_ref(), entities.players(), player_rot)
            {
                registry.set_rotation(ticket.entity_id(), rotation);
            }
            republish_effect_entity_flags(entities.players(), player_ticket.as_ref(), &effects);
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

    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod eye_of_ender_throw_tests {
    use super::*;
    use crate::dimension::Dimension;

    fn eyes_in(stack: u32) -> PlayerInventory {
        let mut inventory = PlayerInventory::new();
        inventory.set_native(0, Some(ItemStack::new("minecraft:ender_eye".parse().unwrap(), stack)));
        inventory
    }

    fn air(_x: i32, _y: i32, _z: i32) -> StateId {
        crate::chunk::air_state()
    }

    /// A locator that always answers with a stronghold 400 blocks east.
    fn east_stronghold(_from: BlockPos) -> Option<BlockPos> {
        Some(BlockPos::new(400, 0, 0))
    }

    const FEET: Vec3 = Vec3::new(0.5, 64.0, 0.5);

    #[test]
    fn a_throw_spawns_an_eye_consumes_one_and_plays_the_launch_sound() {
        let mobs = MobHandle::default();
        let mut inventory = eyes_in(3);
        let sound = launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 0.0,
            Dimension::Overworld, &air, &east_stronghold, 0.5,
        );
        assert_eq!(mobs.with(|sim| sim.eye_count()), 1);
        assert_eq!(inventory.native(0).map(|stack| stack.count), Some(2));
        // The launch pitch is lerp(roll, 0.33, 0.5): 0.33 + 0.17 * 0.5 = 0.415.
        match sound {
            Some(crate::effects::WorldEffect::Sound { sound, pitch, volume, .. }) => {
                assert_eq!(sound, "minecraft:entity.ender_eye.launch");
                assert!((pitch - 0.415).abs() < 1e-6, "{pitch}");
                assert!((volume - 1.0).abs() < f32::EPSILON);
            }
            other => panic!("expected the launch sound, got {other:?}"),
        }
    }

    /// The eye leaves from half the 1.8-block standing height (64.9), and one
    /// tick later it has not moved (the first tick moves by the zero launch
    /// velocity) but has speed 0.0025 * 12 along +x toward the clamped target.
    #[test]
    fn the_eye_launches_from_mid_body_and_heads_for_the_stronghold() {
        let mobs = MobHandle::default();
        let mut inventory = eyes_in(1);
        launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 0.0,
            Dimension::Overworld, &air, &east_stronghold, 0.0,
        )
        .expect("thrown");
        assert_eq!(inventory.native(0), None);
        let snaps = mobs.with(|sim| sim.snapshots());
        let eye = snaps
            .iter()
            .find(|s| s.entity_type.to_string() == "minecraft:eye_of_ender")
            .expect("the eye streams");
        assert!((eye.position.y - 64.9).abs() < 1e-9);
        mobs.with(|sim| sim.tick_eyes());
        let (position, velocity) = mobs.with(|sim| {
            let id = sim.snapshots().iter().find(|s| s.entity_type.to_string() == "minecraft:eye_of_ender").unwrap().id;
            sim.eye_motion(id).unwrap()
        });
        assert!((position.x - 0.5).abs() < 1e-12);
        // Target (400, 0, 0) from (0.5, 64.9, 0.5) is 399.5 east, 0.5 north of
        // the launch: clamped to 12 along that bearing, so vx ~= 0.03.
        assert!((velocity.x - 0.03).abs() < 1e-4, "{velocity:?}");
        assert!((velocity.y - 0.015).abs() < 1e-12);
    }

    #[test]
    fn creative_throws_keep_the_stack() {
        let mobs = MobHandle::default();
        let mut inventory = eyes_in(1);
        let thrown = launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Creative, FEET, 0.0, 0.0,
            Dimension::Overworld, &air, &east_stronghold, 0.0,
        );
        assert!(thrown.is_some());
        assert_eq!(inventory.native(0).map(|stack| stack.count), Some(1));
        assert_eq!(mobs.with(|sim| sim.eye_count()), 1);
    }

    #[test]
    fn no_throw_outside_the_overworld_or_without_a_stronghold() {
        for dimension in [Dimension::Nether, Dimension::End] {
            let mobs = MobHandle::default();
            let mut inventory = eyes_in(1);
            let thrown = launch_eye_of_ender(
                &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 0.0,
                dimension, &air, &east_stronghold, 0.0,
            );
            assert!(thrown.is_none());
            assert_eq!(inventory.native(0).map(|stack| stack.count), Some(1));
            assert_eq!(mobs.with(|sim| sim.eye_count()), 0);
        }
        let mobs = MobHandle::default();
        let mut inventory = eyes_in(1);
        let thrown = launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 0.0,
            Dimension::Overworld, &air, &|_| None, 0.0,
        );
        assert!(thrown.is_none());
        assert_eq!(inventory.native(0).map(|stack| stack.count), Some(1));
        assert_eq!(mobs.with(|sim| sim.eye_count()), 0);
    }

    /// Looking straight down at a portal frame leaves the eye to the
    /// frame-filling arm; the same look at open air throws (the control).
    #[test]
    fn aiming_at_an_end_portal_frame_throws_nothing() {
        let frame = crate::portal::end_portal_frame_state(Direction::North, false);
        let frame_below = move |x: i32, y: i32, z: i32| {
            if (x, y, z) == (0, 63, 0) { frame } else { crate::chunk::air_state() }
        };
        let mobs = MobHandle::default();
        let mut inventory = eyes_in(1);
        let thrown = launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 90.0,
            Dimension::Overworld, &frame_below, &east_stronghold, 0.0,
        );
        assert!(thrown.is_none());
        assert_eq!(inventory.native(0).map(|stack| stack.count), Some(1));
        let control = launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 90.0,
            Dimension::Overworld, &air, &east_stronghold, 0.0,
        );
        assert!(control.is_some(), "the same look at air must throw");
    }

    /// Hand-worked squared distances from (0, 64, 0) to each chunk centre at
    /// y = 32: (10, 0) -> 168^2 + 32^2 + 8^2 = 29312, (-3, 5) -> 40^2 + 32^2 +
    /// 88^2 = 10368, (0, 20) -> 8^2 + 32^2 + 328^2 = 108672. The middle one
    /// wins and is reported at its chunk's minimum corner, y = 0.
    #[test]
    fn the_nearest_ring_start_is_reported_at_its_chunk_corner() {
        let origins = [(10, 0), (-3, 5), (0, 20)];
        let found = crate::chunk::nearest_ring_start(&origins, BlockPos::new(0, 64, 0));
        assert_eq!(found, Some(BlockPos::new(-48, 0, 80)));
        assert_eq!(crate::chunk::nearest_ring_start(&[], BlockPos::new(0, 64, 0)), None);
    }

    #[test]
    fn other_items_are_ignored() {
        let mobs = MobHandle::default();
        let mut inventory = PlayerInventory::new();
        inventory.set_native(0, Some(ItemStack::new("minecraft:ender_pearl".parse().unwrap(), 1)));
        assert!(launch_eye_of_ender(
            &mobs, &mut inventory, 0, GameMode::Survival, FEET, 0.0, 0.0,
            Dimension::Overworld, &air, &east_stronghold, 0.0,
        )
        .is_none());
    }
}
