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
// Submodules, one responsibility each; `docs/server-module-layout.md` has the map.
// Each is glob-imported so a sibling reaches it through `use super::*`.
mod block_actions;
pub(crate) use self::block_actions::*;
mod brewing_stand;
use self::brewing_stand::*;
mod chunk_encoding;
pub use self::chunk_encoding::*;
mod client_commands;
use self::client_commands::*;
mod composter_use;
use self::composter_use::*;
mod connection_driver;
use self::connection_driver::*;
mod container_clicks;
use self::container_clicks::*;
mod end_gateway;
use self::end_gateway::*;
mod entity_streaming;
pub use self::entity_streaming::*;
mod entry_points;
pub use self::entry_points::*;
mod health_sync;
use self::health_sync::*;
mod join_snapshots;
use self::join_snapshots::*;
mod join_trace;
pub(crate) use self::join_trace::*;
mod lighting;
use self::lighting::*;
mod map_tick;
use self::map_tick::*;
mod online_mode;
pub use self::online_mode::*;
mod open_containers;
use self::open_containers::*;
mod pickups;
use self::pickups::*;
mod play_dispatch;
use self::play_dispatch::*;
mod play_loop;
use self::play_loop::*;
mod play_state;
use self::play_state::*;
mod player_actions;
use self::player_actions::*;
mod player_effects;
use self::player_effects::*;
mod player_persistence;
use self::player_persistence::*;
mod query_tags;
use self::query_tags::*;
mod resident_queries;
use self::resident_queries::*;
mod resource_pack;
pub use self::resource_pack::*;
mod source_ref;
pub(crate) use self::source_ref::*;
mod use_item;
pub use self::use_item::*;
mod use_item_on;
pub(crate) use self::use_item_on::*;
mod view_tracker;
use self::view_tracker::*;

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


#[cfg(test)]
mod tests;

