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
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use lodestone_time::Instant;

/// The join path's `PERF INSTRUMENT` clock, which exists so those timers do not
/// break the `wasm32` build.
///
/// `std::time::Instant::now()` **panics on `wasm32`** — there is no monotonic
/// clock behind it — so three bare `Instant::now()` calls in the join sequence
/// made the whole crate unbuildable for the browser while the `Instant` import
/// itself was already `cfg`-gated. The compile error named the import, not the
/// call sites, which is why it read as a missing feature rather than three
/// diagnostics that had outlived their debugging session.
///
/// **Do not "fix" this with `tokio::time::Instant`.** rustc's own `help:`
/// suggests it beside `std`'s, and `serve_play` a few hundred lines below
/// already uses it, so it reads as established precedent. It is not: it bottoms
/// out in `std::time::Instant::now()` (tokio 1.53.1, `src/time/clock.rs:16`) and
/// panics identically. That substitution trades a compile error for a runtime
/// crash in a browser, which is strictly worse — the error moves from the one
/// place that reports it to the one place nobody is watching.
///
/// The `wasm32` arm holds no clock and reports `Duration::ZERO`. These are
/// `tracing::info!` lines about join latency; a zero on a target that cannot
/// measure is the honest reading, and it is deliberately *not* a plausible
/// fabricated number for the same reason `menu::options` refuses to print a
/// value for an option it does not honour.
#[derive(Clone, Copy)]
pub(crate) struct JoinStopwatch {
    #[cfg(not(target_arch = "wasm32"))]
    started: Instant,
}

impl JoinStopwatch {
    pub(crate) fn now() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            started: Instant::now(),
        }
    }

    pub(crate) fn elapsed(&self) -> Duration {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.started.elapsed()
        }
        #[cfg(target_arch = "wasm32")]
        {
            Duration::ZERO
        }
    }
}

use lodestone_core::State;
use lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE;
use lodestone_entity::{DamageFlags, ItemLifecycle};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, EntityAttributeSnapshot, GameMode, ItemStack,
    ResourceKey, ResourcePackResponseKind, Rotation, Text, TextContent, Vec3, Vec3f,
    WrittenBookContent,
};
use lodestone_data::{block::Block, block_items, item::Item, potion::PotionId};
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
use crate::block_entities::{BlockEntity, BlockEntityHandle, block_entity_for_item};
use crate::border::BorderFeed;
use crate::brewing::{Bottle, BottleKind, is_ingredient};
use crate::composter::{InsertOutcome, compostable_chance};
use crate::command::{CommandCaller, CommandDispatch, CommandSession};
use crate::chunk::{
    AIR, ChunkColumn, ChunkSource, generate_columns_offloaded, generate_columns_parallel,
    is_air_or_fluid, is_water,
};
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
use crate::redstone::{WorldState, COMPARATOR, OBSERVER, REPEATER};
use crate::redstone_diode::{set_comparator, set_repeater};
use crate::redstone_observer::set_observer;
use crate::scheduled_tick::{ScheduledTick, ScheduledTickQueue};
use crate::sleep::{SleepEvent, SleepFeed, SleepVote};
use crate::ticket::{PLAYER_SPAWN_RADIUS, PlayerTicketGuard, TicketKind, TicketOwner, TicketStoreHandle};
use crate::tick::{BlockTickFeed, ExplosionFeed};
use crate::weather::WeatherFeed;
use crate::vitals::{EYE_HEIGHT, PlayerVitals};
use crate::world_spawn::{RespawnPoint, find_initial_spawn, is_bed_block, is_legal_bed_respawn};

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
/// `config::MAX_RENDER_DISTANCE = 32` chunks and
/// `Session::set_render_distance` sends `render_distance + 1` (the outermost
/// streamed ring can never be meshed, so asking for exactly `render_distance`
/// loses the last visible ring) — so `33` is the largest value a real client on
/// this project can ask for, and vanilla's own client-information view-distance
/// field
/// is documented as `2..=32` on [`ServerBound::ClientInformationChanged`].
///
/// This is a *sanity* bound rather than a memory policy: the wire field is an
/// `i8`, so without it a malformed packet asking for `127` would try to stream
/// 65,025 columns. Singleplayer is deliberately **not** capped by
/// `chunk_store::MAX_CAPACITY` — see that constant and
/// `chunk_store::integrated_capacity_for_view_radius` for whose memory is being
/// spent, and this module's own note on what the store's capacity does *not*
/// follow.
pub const MAX_CLIENT_VIEW_RADIUS: i32 = 33;

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
/// pass is a diff — [`EntityStreamer`] emits nothing when nothing changed — so
/// an idle connection costs one snapshot comparison per tick and no packets.
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
    /// (the player registry is optional).
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

    /// The boss bars a client should currently hold — the dragon fight's own
    /// health bar, and any future producer this trait gains. Defaulted to
    /// empty for the identical reason [`players`](Self::players) is: adding a
    /// method here must not force every existing implementor (including the
    /// version-crate test doubles) to grow one.
    ///
    /// [`crate::mobs::MobSim::boss_bars`] is today's one real producer, reached
    /// through [`crate::mobs::LiveMobSource`]/[`crate::mobs::MobHandle`].
    fn boss_bars(&self) -> Vec<BossBarSnapshot> {
        Vec::new()
    }
}

/// Runs one full streaming pass for a connection: tab-list diff first, then the
/// entity diff over the mob source **and** every other connected player.
///
/// The order is load-bearing and the reason this is one function rather than
/// two call sites. A client that receives an `ADD_ENTITY` of type
/// `minecraft:player` before it holds a `PlayerInfo` for that uuid **discards
/// the spawn** — vanilla's own client-side "create entity from packet" step
/// returns `null`
/// and logs "Server attempted to add player prior to sending player info".
/// So the roster adds must precede the
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
    directives.extend(streamer.sync_boss_bars(proto, &entities.boss_bars()));
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
    /// The boss bars this connection has been sent `ADD` for and not yet
    /// `REMOVE` — the same last-sent-state shape [`last_sent`](Self::last_sent)
    /// keeps for entities, one level simpler (a bar has no spawn/update split
    /// on the wire, only add/update-progress/remove). See
    /// [`sync_boss_bars`](Self::sync_boss_bars).
    boss_bars_sent: HashMap<uuid::Uuid, BossBarSnapshot>,
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
                    // The spawn frame carries no metadata, so send a second
                    // `SET_ENTITY_DATA` directive whenever the snapshot has
                    // non-default values. `encode_add_entity` returns exactly
                    // one `ServerDirective`, so the metadata stays separate.
                    if !entity.metadata.is_empty() {
                        directives.push(proto.encode_set_entity_data(entity.id, &entity.metadata));
                    }
                    // A snapshot with a leash link needs both the spawn and link
                    // frames, so a client whose first visible snapshot contains
                    // `leash_link` renders the rope immediately.
                    if entity.leash_link.is_some() {
                        directives.push(proto.encode_set_entity_link(entity.id, entity.leash_link));
                    }
                    self.last_sent.insert(entity.id, entity.clone());
                }
                Some(prev) if prev != entity => {
                    directives.extend(proto.encode_entity_update(Some(prev), entity));
                    // A metadata-only change (e.g. a creeper's
                    // `swell_dir` climbing while it stands still) still
                    // takes this branch — `EntitySnapshot`'s `PartialEq`
                    // covers `metadata` too — so this check is independent
                    // of whether position/rotation also changed this tick.
                    if prev.metadata != entity.metadata {
                        directives.push(proto.encode_set_entity_data(entity.id, &entity.metadata));
                    }
                    // A leash-link transition covers both attachment (`None` →
                    // `Some`) and detachment (`Some` → `None`); the encoder
                    // chooses the wire representation for an absent holder.
                    if prev.leash_link != entity.leash_link {
                        directives.push(proto.encode_set_entity_link(entity.id, entity.leash_link));
                    }
                    self.last_sent.insert(entity.id, entity.clone());
                }
                Some(_) => {}
            }
        }

        directives
    }

    /// The `BOSS_EVENT` twin of [`sync`](Self::sync) — diffs `current` against
    /// what this connection was last sent and returns the add/update/remove
    /// directives that close the gap.
    ///
    /// Vanilla's `ClientboundBossEventPacket` carries no "visible" bit of its
    /// own (see [`BossBarSnapshot`]'s own doc): a bar this pass reports
    /// `visible: false` is therefore removed (or, if it was never added,
    /// simply never added) rather than sent with a false flag, and a bar
    /// whose id vanished from `current` entirely — the boss despawned — is
    /// removed the same way an entity id vanishing from [`sync`](Self::sync)'s
    /// `current` triggers `REMOVE_ENTITIES`.
    fn sync_boss_bars<P: ServerProtocol>(
        &mut self,
        proto: &P,
        current: &[BossBarSnapshot],
    ) -> Vec<ServerDirective> {
        let mut directives = Vec::new();

        let live: HashSet<uuid::Uuid> = current.iter().map(|b| b.id).collect();
        let vanished: Vec<uuid::Uuid> = self
            .boss_bars_sent
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        for id in vanished {
            self.boss_bars_sent.remove(&id);
            directives.push(proto.encode_boss_event_remove(id));
        }

        for bar in current {
            match self.boss_bars_sent.get(&bar.id) {
                None if bar.visible => {
                    directives.push(proto.encode_boss_event_add(bar.id, &bar.name, bar.progress));
                    self.boss_bars_sent.insert(bar.id, bar.clone());
                }
                // Never added and still not visible: nothing to do, and
                // nothing to remember — matches vanilla never broadcasting an
                // invisible `ServerBossEvent` to a player in the first place.
                None => {}
                Some(_) if !bar.visible => {
                    directives.push(proto.encode_boss_event_remove(bar.id));
                    self.boss_bars_sent.remove(&bar.id);
                }
                Some(prev) if prev.progress != bar.progress => {
                    directives.push(proto.encode_boss_event_update_progress(bar.id, bar.progress));
                    self.boss_bars_sent.insert(bar.id, bar.clone());
                }
                Some(_) => {}
            }
        }

        directives
    }
}

/// How a connection reaches its terrain, including the blocking and offloaded
/// blocking-vs-offloaded fork in one place.
///
/// Chunk generation is CPU-bound and synchronous, so it has to be moved off
/// the async runtime's core thread — see
/// [`generate_columns_offloaded`](crate::chunk::generate_columns_offloaded)
/// for the measurement and for why `spawn_blocking` rather than
/// `block_in_place`. `spawn_blocking` needs a `'static` closure, which a
/// `&S` cannot provide. That normally forces `serve_connection`'s `source`
/// parameter from `&S` to `Arc<S>`. The separate wrappers preserve the borrowed
/// source API while the shared variant can move an `Arc<S>` into a blocking
/// generation task.
///
/// This enum is how both shapes share one body instead:
///
/// | arm | generation | who uses it |
/// |---|---|---|
/// | [`Shared`](Self::Shared) | offloaded, never blocks the runtime | every production caller in [`crate::integrated`] |
/// | [`Borrowed`](Self::Borrowed) | blocking, direct generation | `&S`-shaped test call sites |
///
/// The `Borrowed` arm is deliberately kept rather than deleted: it is the
/// **permanent negative control** for the offloading gate. A test can drive the exact
/// same `serve_connection` body down the blocking path and watch the world
/// tick stall, which is what proves the `Shared` arm's non-stall assertion is
/// measuring something. A control that only exists as a temporary neuter
/// cannot be re-run later.
///
/// `Copy` (hand-written, because `#[derive(Copy)]` would demand `S: Copy`)
/// so it threads through the dispatch chain exactly as cheaply as the `&S`
/// it replaces.
///
/// Portal travel uses the [`Dimension`](Self::Dimension) arm.
///
/// `Debug` is hand-written because:
/// `#[derive(Debug)]` would demand `dyn ChunkSource: Debug`, and making `Debug` a
/// supertrait of `ChunkSource` to satisfy a diagnostic impl is the wrong direction.
pub(crate) enum SourceRef<'a, S> {
    /// A plain borrow. Generation blocks the calling thread.
    Borrowed(&'a S),
    /// A shared handle. Generation is offloaded to the blocking pool.
    Shared(&'a Arc<S>),
    /// **Another dimension's** terrain, reached through
    /// [`ChunkSource::sibling`](crate::ChunkSource::sibling) after a portal trip.
    /// Generation is offloaded exactly as [`Shared`](Self::Shared) is.
    ///
    /// # Why this is not just `Shared`
    ///
    /// The Nether's concrete source type is not the overworld's
    /// (`NetherChunkSource` vs `OverworldChunkSource`, each behind its own
    /// `ChunkStore` and its own `DimensionalSource`), so no single `S` can name
    /// both — `Shared(&'a Arc<S>)` is monomorphic in the connection's `S` by
    /// construction. Erasing to `dyn ChunkSource` here is what lets a connection
    /// change dimension without the whole `serve_play` state machine being generic
    /// over which dimension it is in.
    ///
    /// This is also why [`get`](Self::get) hands back `&dyn ChunkSource` rather
    /// than `&S`: every helper it feeds is `S: ChunkSource + ?Sized`, and the
    /// `?Sized` bounds throughout this file exist for exactly this arm.
    Dimension(&'a Arc<dyn ChunkSource>),
}

impl<S> Clone for SourceRef<'_, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S> Copy for SourceRef<'_, S> {}

impl<S> std::fmt::Debug for SourceRef<'_, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let arm = match self {
            Self::Borrowed(_) => "Borrowed",
            Self::Shared(_) => "Shared",
            Self::Dimension(_) => "Dimension",
        };
        f.debug_tuple("SourceRef").field(&arm).finish()
    }
}

impl<'a, S: ChunkSource + 'static> SourceRef<'a, S> {
    /// The underlying source, for the read/write paths that never generate a
    /// whole batch (`block_state`, `set_block`) and so have nothing to
    /// offload.
    fn get(self) -> &'a dyn ChunkSource {
        match self {
            Self::Borrowed(source) => source,
            Self::Shared(source) => &**source,
            Self::Dimension(source) => &**source,
        }
    }

    /// Which dimension this reference reads, treating an unlabelled source as the
    /// overworld — see [`ChunkSource::dimension`](crate::ChunkSource::dimension)
    /// for why `None` is a distinct answer at the trait but collapses here.
    fn dimension(self) -> crate::dimension::Dimension {
        self.get()
            .dimension()
            .unwrap_or(crate::dimension::Dimension::Overworld)
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
    /// `pub(crate)` rather than private since `crate::join_scheduler`'s
    /// `JoinChunkStream` finishes a join from `serve_play`, and its borrowed arm
    /// generates through exactly this method — the alternative was duplicating the
    /// arm fork, which is the one thing that must not drift.
    pub(crate) async fn generate(self, coords: Vec<(i32, i32)>) -> Vec<ChunkColumn> {
        match self {
            Self::Shared(source) => generate_columns_offloaded(Arc::clone(source), coords).await,
            Self::Borrowed(source) => generate_columns_parallel(source, &coords),
            Self::Dimension(source) => generate_columns_offloaded(Arc::clone(source), coords).await,
        }
    }
}

/// A completed portal trip: where the player now is, and the terrain they are now
/// standing on.
struct PortalTrip {
    /// What the connection's `SourceRef` becomes from the next loop iteration
    /// onward: `None` is "back to the source you joined with", `Some` is a sibling
    /// dimension.
    ///
    /// **A return trip is not a sibling lookup.** The connection still holds the
    /// source it joined with, so coming home is putting that back — which is why
    /// `crate::dimension::DimensionalSource`'s links only point outward and no
    /// reference cycle exists to leak a world through.
    source: Option<Arc<dyn ChunkSource>>,
    /// Where the player arrived.
    position: Vec3,
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

/// Moves a player through a nether portal — the whole server side of a trip.
///
/// Returns `None`, having sent nothing, when the trip cannot happen: the world has
/// no such dimension (a single-dimension world), the destination has no placeable band, or the
/// hosting protocol cannot encode a dimension change. All three are *declines*
/// rather than failures — the player stays where they are, standing in a portal,
/// and nothing is half-applied.
///
/// # The order of the packets is the whole correctness argument
///
/// 1. **Forget every loaded column.** The client keeps chunks in a store with no
///    bulk-clear operation, so `forget_chunk` empties it before the destination
///    dimension supplies terrain and height metadata.
/// 2. **The dimension change pair** (`respawn` + the placement teleport). This is
///    what re-frames the client's chunk window: it resolves the destination
///    `dimension_type` holder id and installs that dimension's `min_y` and section
///    count. Every chunk sent before it would be decoded against the prior window.
/// 3. **The destination cache centre, then the chunks.** Both must follow (2), for the same
///    reason.
///
/// # Why the view tracker is rebuilt rather than recentred
///
/// [`ViewTracker::recenter`] emits a *difference* — the columns that entered and
/// left — which is exactly wrong here: nothing the old dimension sent is reusable,
/// and the new dimension owes the player the entire square. Rebuilding with
/// [`ViewTracker::new`] and handing the whole square to a
/// [`JoinChunkStream`](crate::join_scheduler::JoinChunkStream) uses the same
/// ring order as an initial join, so the ground under the player's feet arrives
/// first.
async fn travel_through_portal<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    // The source this connection **joined** with, i.e. home. Separate from
    // `current` because it is both where a return trip lands and the only thing that
    // knows the world's siblings — see [`PortalTrip::source`].
    home: SourceRef<'_, S>,
    // The dimension the player is in *now*.
    current: SourceRef<'_, S>,
    state: &mut State,
    view: &mut ViewTracker,
    join_stream: &mut crate::join_scheduler::JoinChunkStream<S>,
    teleport_acknowledgements: &mut Option<TeleportAcknowledgements>,
    entry: BlockPos,
    player_pos: (f64, f64, f64),
    game_mode: GameMode,
) -> Result<Option<PortalTrip>, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
{
    let from = current.dimension();
    let to = from.nether_portal_destination();
    // Going home is the `None` arm — the connection's own source, already in hand.
    // Going out asks *home* for the sibling, because that is where the world's links
    // live. Bound to a local first so the `&dyn` below outlives the borrow.
    let sibling: Option<Arc<dyn ChunkSource>> = if to == home.dimension() {
        None
    } else {
        match home.get().sibling(to) {
            Some(sibling) => Some(sibling),
            // A single-dimension world. The correct degradation: a player can light a
            // portal and stand in it, and nothing happens.
            None => return Ok(None),
        }
    };
    let destination: &dyn ChunkSource = match sibling.as_ref() {
        Some(arc) => &**arc,
        None => home.get(),
    };

    // The index is the *world's*, shared across every dimension of it — see
    // `crate::portal::PortalIndex`. Read through the source already in hand rather
    // than the destination's, because both answer with the same store.
    let index = current.get().portal_index().cloned();
    // The exit portal axis comes from the block containing the player. Carry
    // `entry` here so the generated portal keeps that orientation.
    let source_axis =
        crate::portal::Axis::from_state(&current.get().block_state(entry.x, entry.y, entry.z));
    // # Why the outbound leg is offloaded and the return leg is not
    //
    // `resolve_destination` is synchronous CPU work whose *reads* may each generate a
    // whole column, and the outbound leg is the expensive one by construction: the
    // destination is a dimension nothing has ever looked at, so the site search's
    // 33 × 33 footprint means a dozen columns generated from scratch. Left on the core
    // thread that is measured in seconds, which is a keep-alive timeout rather than a
    // slow frame that can exceed the keep-alive interval. Offloading keeps the
    // current-thread runtime responsive while the destination is resolved.
    //
    // The return leg runs inline because it structurally cannot cost that: the
    // dimension is the one the player joined into and has been streaming from, so its
    // columns are resident, and the index almost always answers before any scan. It
    // also *cannot* be offloaded — `home` may be `SourceRef::Borrowed`, which is not
    // `'static` and so cannot cross `spawn_blocking`. Keeping the two arms honest
    // about which is which is better than a fork that pretends both are cheap.
    let resolved = match sibling.clone() {
        #[cfg(not(target_arch = "wasm32"))]
        Some(owned) => {
            let index = index.clone();
            tokio::task::spawn_blocking(move || {
                crate::portal::resolve_destination(
                    &*owned,
                    from,
                    to,
                    index.as_ref(),
                    player_pos,
                    source_axis,
                )
            })
            .await
            .expect("portal destination search panicked")
        }
        #[cfg(target_arch = "wasm32")]
        Some(owned) => crate::portal::resolve_destination(
            &*owned,
            from,
            to,
            index.as_ref(),
            player_pos,
            source_axis,
        ),
        None => crate::portal::resolve_destination(
            destination,
            from,
            to,
            index.as_ref(),
            player_pos,
            source_axis,
        ),
    };
    let Some(resolved) = resolved else {
        return Ok(None);
    };
    let arrival = resolved.position;
    // `resolve_destination` deliberately does not write, so the commit is here — and
    // it happens *before* the client is told anything, so the terrain the chunk
    // stream below carries already contains the portal the player is about to be
    // standing in.
    if let Some(created) = &resolved.created {
        for (pos, block) in &created.blocks {
            destination.set_block(pos.x, pos.y, pos.z, block);
        }
        if let Some(index) = index.as_ref() {
            index.extend(to, created.portal_cells.iter().copied());
        }
    }

    // Built *before* anything is sent, so a protocol that cannot encode a dimension
    // change costs the client nothing at all — rather than emptying its chunk store
    // and then discovering there is no way to tell it where it now is.
    let change = proto.encode_dimension_change_with_teleport_id(
        issue_teleport_id(teleport_acknowledgements),
        to.key(),
        arrival,
        game_mode,
    );
    if change.is_empty() {
        return Ok(None);
    }

    for &(cx, cz) in &view.loaded {
        apply(conn, state, proto.encode_forget_chunk(cx, cz)).await?;
    }
    for directive in change {
        apply(conn, state, directive).await?;
    }

    let centre_cx = (arrival.x / 16.0).floor() as i32;
    let centre_cz = (arrival.z / 16.0).floor() as i32;
    apply(
        conn,
        state,
        proto.encode_chunk_cache_center(centre_cx, centre_cz),
    )
    .await?;

    let radius = view.radius;
    let max_radius = view.max_radius;
    *view = ViewTracker::new((centre_cx, centre_cz), radius, max_radius);
    let rings: Vec<Vec<(i32, i32)>> = join_view_rings(radius)
        .into_iter()
        .map(|ring| {
            ring.into_iter()
                .map(|(dx, dz)| (centre_cx + dx, centre_cz + dz))
                .collect()
        })
        .collect();
    // The `ringed` arm holds coordinates only, so each generation request uses
    // the `SourceRef` for the destination dimension. A source captured while
    // constructing the stream could send terrain from the wrong world.
    *join_stream = crate::join_scheduler::JoinChunkStream::ringed(rings);

    debug_assert_eq!(
        destination.dimension().unwrap_or(crate::dimension::Dimension::Overworld),
        to,
        "the destination source must be the dimension we told the client about"
    );

    Ok(Some(PortalTrip {
        source: sibling,
        position: arrival,
    }))
}

/// Moves a player through an End portal to its fixed arrival platform. This is
/// the End counterpart to [`travel_through_portal`].
///
/// **Deliberately not a generalisation of [`travel_through_portal`].** An End
/// portal has no coordinate scale, no linked-position search and no fresh
/// portal to build at the far end: [`crate::portal::end_portal_arrival`]
/// names a **fixed** point, and [`crate::portal::ensure_end_platform`] builds
/// (or repairs) the obsidian platform there before the chunk stream below can
/// reach it. Reusing the Nether's destination search here would run
/// a linked-position search over the End's terrain and could as easily strand
/// a player over the void as land them on solid ground.
///
/// The packet sequence — forget every loaded column, the dimension-change
/// pair, the destination cache centre, the rebuilt view and join stream — is
/// otherwise identical to [`travel_through_portal`]'s, because it is the same
/// client-side contract regardless of which portal type triggered it; see
/// that function's own doc comment for why each step is ordered the way it
/// is.
///
/// An End portal inside the End has no destination in this world model. The
/// caller returns `None` for that case rather than selecting an invalid target.
async fn travel_through_end_portal<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    // The source this connection **joined** with — where the End sibling is
    // reached from, exactly as in `travel_through_portal`.
    home: SourceRef<'_, S>,
    state: &mut State,
    view: &mut ViewTracker,
    join_stream: &mut crate::join_scheduler::JoinChunkStream<S>,
    teleport_acknowledgements: &mut Option<TeleportAcknowledgements>,
    game_mode: GameMode,
    mobs: &MobHandle,
) -> Result<Option<PortalTrip>, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
{
    let to = crate::dimension::Dimension::End;
    let sibling: Arc<dyn ChunkSource> = match home.get().sibling(to) {
        Some(sibling) => sibling,
        // A single-dimension world (or one built without the End sibling
        // wired): the same correct degradation `travel_through_portal` falls
        // back to — the ring completes and nothing happens.
        None => return Ok(None),
    };
    let destination: &dyn ChunkSource = &*sibling;

    let (platform_origin, arrival) = crate::portal::end_portal_arrival();
    // Written *before* anything is sent, exactly as `travel_through_portal`
    // commits a freshly built Nether portal before telling the client
    // anything — so the chunk stream below already carries the platform the
    // player is about to be standing on.
    crate::portal::ensure_end_platform(destination, platform_origin);

    // The one remaining hop `docs/dragon-fight.md` names: the first
    // connection to reach a fresh End (this session — see
    // `ChunkSource::claim_dragon_fight_start`'s own doc comment for why this
    // is a process-lifetime gate, not a persisted one) spawns the ten
    // seed-derived crystals, the dragon itself, and writes every obsidian
    // spike/podium block the arena needs, exactly as
    // `MobSim::init_end_dragon_fight`'s own doc describes. `claim_...`
    // returning `false` means another connection already did this for the
    // same End sibling, so this one does nothing further.
    if destination.claim_dragon_fight_start() {
        let seed = crate::worldgen_data::active_world_seed();
        let init = mobs.with(|sim| {
            sim.init_end_dragon_fight(seed, Vec3::new(0.0, 64.0, 0.0), to.min_y())
        });
        for write in &init.block_writes {
            destination.set_block(write.x, write.y, write.z, &write.state);
        }
    }

    let change = proto.encode_dimension_change_with_teleport_id(
        issue_teleport_id(teleport_acknowledgements),
        to.key(),
        arrival,
        game_mode,
    );
    if change.is_empty() {
        return Ok(None);
    }

    for &(cx, cz) in &view.loaded {
        apply(conn, state, proto.encode_forget_chunk(cx, cz)).await?;
    }
    for directive in change {
        apply(conn, state, directive).await?;
    }

    let centre_cx = (arrival.x / 16.0).floor() as i32;
    let centre_cz = (arrival.z / 16.0).floor() as i32;
    apply(
        conn,
        state,
        proto.encode_chunk_cache_center(centre_cx, centre_cz),
    )
    .await?;

    let radius = view.radius;
    let max_radius = view.max_radius;
    *view = ViewTracker::new((centre_cx, centre_cz), radius, max_radius);
    let rings: Vec<Vec<(i32, i32)>> = join_view_rings(radius)
        .into_iter()
        .map(|ring| {
            ring.into_iter()
                .map(|(dx, dz)| (centre_cx + dx, centre_cz + dz))
                .collect()
        })
        .collect();
    *join_stream = crate::join_scheduler::JoinChunkStream::ringed(rings);

    debug_assert_eq!(
        destination.dimension().unwrap_or(crate::dimension::Dimension::Overworld),
        to,
        "the destination source must be the dimension we told the client about"
    );

    Ok(Some(PortalTrip {
        source: Some(sibling),
        position: arrival,
    }))
}

/// Per-connection view-streaming bookkeeping: which chunk columns has this
/// connection been sent, and around which chunk column.
///
/// Mirrors vanilla's own chunk-map/chunk-tracking-view machinery
/// (its own "update chunk tracking"/"apply chunk tracking view" steps,
/// its own tracking-view "difference" helper), simplified to the same square
/// window `serve_connection`'s own initial view already uses
/// (`[-view_radius, view_radius]²`) rather than vanilla's rounded
/// positioned-tracking-view "contains" check (a buffered Euclidean-distance
/// test). Keeping the join-time and move-time shapes identical is what stops
/// a live connection from immediately forgetting chunks it only just
/// finished sending at join; matching vanilla's exact circular shape is not
/// otherwise load-bearing for "the world keeps up as the player walks".
#[derive(Debug)]
struct ViewTracker {
    center: (i32, i32),
    loaded: HashSet<(i32, i32)>,
    /// The connection's *current* effective view radius — starts at the radius
    /// the connection joined with (`serve_connection`'s own `view_radius`
    /// parameter) and can shrink or grow within
    /// [`max_radius`](Self::max_radius) via
    /// [`set_view_radius`](Self::set_view_radius) (the view-distance
    /// `ServerBound::ClientInformationChanged`). Stored on `self` rather than
    /// re-passed at every [`recenter`](Self::recenter) call so a client's
    /// requested distance actually sticks across subsequent moves, instead
    /// of being silently overwritten by the original radius on the next
    /// `PlayerMoved`.
    radius: i32,
    /// The largest radius this connection is **permitted** to reach, and the
    /// ceiling [`set_view_radius`](Self::set_view_radius) clamps a client
    /// request to the configured ceiling, keeping the advertised distance
    /// within the server's accepted range.
    ///
    /// **The view-distance ceiling is a second field because it answers a second
    /// question.** `radius` above is where the connection *starts*; this is how
    /// far it may *go*. A client may lower or raise its requested distance within
    /// the configured ceiling, which is a server setting rather than the player's
    /// current view.
    ///
    /// Who supplies it is a per-path memory-policy decision, the same fork
    /// `ChunkStore::for_view_radius` vs `for_integrated_view_radius` already
    /// encodes: singleplayer (`open_in_memory*`) passes
    /// [`MAX_CLIENT_VIEW_RADIUS`] because it is the slider-mover's own memory,
    /// while open-to-LAN (`IntegratedServer::bind`) passes its configured
    /// `view_radius` because it spends an operator's memory on behalf of players
    /// who did not choose the setting. Every other caller passes `view_radius`,
    /// which preserves the compatibility behavior of those callers.
    max_radius: i32,
}

/// The directives produced by one [`ViewTracker`] update, split by whether
/// they are subject to the chunk-batch flow-control gate
/// (`ServerBound::ChunkBatchAcknowledged`) — see
/// [`send_view_update`]'s own doc comment for how a caller applies this.
#[derive(Debug, Default)]
struct ViewUpdate {
    /// Cache-center updates and forgets are sent right away regardless of any
    /// outstanding chunk-batch acknowledgement. Only new chunk sends wait for
    /// that flow-control signal.
    immediate: Vec<ServerDirective>,
    /// The columns that left the view — the same set `immediate`'s
    /// `encode_forget_chunk` directives name, kept as coordinates as well so
    /// [`send_view_update`] can withdraw any of them still queued in the column
    /// stream. See [`crate::join_scheduler::ColumnPipeline::cancel`] for why an owed
    /// column can outlive its own forget.
    forgotten: HashSet<(i32, i32)>,
    /// The newly-visible columns, in wire order — empty when nothing new entered
    /// the view.
    ///
    /// **Coordinates, not directives.** This API returns work for the caller to
    /// schedule rather than a finished
    /// `begin_chunk_batch`/`encode_chunk`*/`end_chunk_batch` sequence, which meant
    /// [`ViewTracker::recenter`] had to `await` the generation *and* the encode of
    /// every column in the strip before returning: one suspension point covering
    /// `2r + 1` columns, 33 at `view_radius = 16`, during which the connection
    /// task read nothing and wrote nothing. Handing back the coordinates makes
    /// both update paths synchronous and lets [`send_view_update`] feed them to
    /// the same streaming pipeline the join uses, one column per pass of the
    /// `select!` loop.
    added: Vec<(i32, i32)>,
}

impl ViewTracker {
    /// Seeds the tracker with the square already sent for the initial join
    /// view (`center`, `[-view_radius, view_radius]²` around it), so the
    /// first [`recenter`](Self::recenter) diffs against what the client
    /// actually has rather than an empty set.
    /// `max_view_radius` is the ceiling for a *later*
    /// [`set_view_radius`](Self::set_view_radius) — see
    /// [`max_radius`](Self::max_radius). It is raised to `view_radius` if a
    /// caller passes something smaller, because the join square has already been
    /// sent and a ceiling under it would make the connection's very first live
    /// settings packet shrink a view nobody asked to shrink.
    fn new(center: (i32, i32), view_radius: i32, max_view_radius: i32) -> Self {
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
            max_radius: max_view_radius.max(view_radius),
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

    /// Every column in `next` this tracker has not sent, in wire order — empty if
    /// there is nothing visible to add. Shared by [`recenter`](Self::recenter) and
    /// [`set_view_radius`](Self::set_view_radius) so both diff against
    /// `self.loaded` identically.
    fn added_columns(
        &self,
        next: &HashSet<(i32, i32)>,
        centre: (i32, i32),
        facing: Option<f32>,
    ) -> Vec<(i32, i32)> {
        // Sorted rather than left in `HashSet::difference`'s hash-iteration
        // order: that order already varies run-to-run (`RandomState` reseeds
        // per process), and generating in parallel below means the set of
        // columns can finish in yet another, scheduling-dependent order.
        // Fixing the wire order here is what makes the encoded byte sequence
        // independent of both.
        //
        // **Ordered nearest-first, not lexicographically.** A bare
        // `sort_unstable()` orders by `cx` then `cz` — a raster walk, so a player
        // walking east would get the visible column strip filled from its
        // northern end regardless of where along it they actually were. The same
        // key the join stream uses (`join_scheduler::view_order_key`: distance
        // from the player's column first, the cone they are looking down second)
        // makes a *move* behave like a join. It is a total order over
        // integers, so the wire order stays a
        // deterministic function of the pose, not of scheduling.
        let mut added: Vec<(i32, i32)> = next.difference(&self.loaded).copied().collect();
        added.sort_unstable_by_key(|&coord| {
            crate::join_scheduler::view_order_key(centre, facing, coord)
        });
        added
    }

    /// Recomputes the view for a new player chunk position `(cx, cz)` at the
    /// tracker's current [`radius`](Self::radius), returning the directives
    /// that bring the client's tracked chunks back in sync — and returning
    /// nothing at all if `(cx, cz)` is still the tracked center (the same
    /// "did the 2D chunk position actually change" guard
    /// vanilla's own "update chunk tracking" step applies before touching the view at
    /// all).
    ///
    /// Order mirrors vanilla's own "apply chunk tracking view" step: the cache-center update is sent first
    /// (unconditionally, since by this point the center *did* change —
    /// vanilla additionally guards this send on the center changing, which
    /// is already implied here), then every column that left the window is
    /// forgotten, then every column that entered it is sent as one chunk
    /// batch.
    ///
    /// `facing` is the player's yaw in degrees where the connection has reported
    /// one, and only orders the added columns (see
    /// [`added_columns`](Self::added_columns)) — never *which* columns they are,
    /// which is the square alone.
    ///
    /// **Synchronous.** It computes a set difference and encodes forgets; nothing
    /// here generates terrain, which is what lets a chunk-boundary crossing return
    /// to the `select!` loop immediately instead of parking it for the length of a
    /// 33-column strip. [`send_view_update`] is where the columns become bytes.
    fn recenter<P>(&mut self, proto: &P, cx: i32, cz: i32, facing: Option<f32>) -> ViewUpdate
    where
        P: ServerProtocol,
    {
        if (cx, cz) == self.center {
            return ViewUpdate::default();
        }

        let next = Self::window((cx, cz), self.radius);

        let mut immediate = vec![proto.encode_chunk_cache_center(cx, cz)];
        let forgotten: HashSet<(i32, i32)> = self.loaded.difference(&next).copied().collect();
        for &(x, z) in &forgotten {
            immediate.push(proto.encode_forget_chunk(x, z));
        }
        let added = self.added_columns(&next, (cx, cz), facing);

        self.center = (cx, cz);
        self.loaded = next;
        ViewUpdate {
            immediate,
            forgotten,
            added,
        }
    }

    /// Resizes the tracked view around the *current* center without the
    /// player having moved at all (the `ClientInformationChanged` view-distance
    /// `ServerBound::ClientInformationChanged` — a client changing its
    /// render-distance setting mid-session). Unlike
    /// [`recenter`](Self::recenter), there is no cache-center update to send
    /// (the center did not change) and no early-return guard on position —
    /// the guard here is `radius` itself already matching, so a settings
    /// packet that does not actually change the distance is correctly a
    /// no-op.
    fn set_view_radius<P, S>(
        &mut self,
        proto: &P,
        source: SourceRef<'_, S>,
        radius: i32,
        facing: Option<f32>,
    ) -> ViewUpdate
    where
        P: ServerProtocol,
        S: ChunkSource + 'static,
    {
        // Clamp against the configured ceiling rather than the connection's
        // current radius. The lower bound is `0`, which represents an empty
        // view and keeps the server-side range valid for small test worlds.
        // `.max(0)` on the ceiling preserves `clamp`'s `min <= max` invariant
        // when a caller supplies a negative configuration value.
        let radius = radius.clamp(0, self.max_radius.max(0));
        if radius == self.radius {
            return ViewUpdate::default();
        }

        // Resize the retention bound *before* streaming the requested view.
        // A larger radius can evict the **innermost** ring while
        // `join_view_rings` streams outward; regenerating the ground under the
        // player's feet costs ~909 ms per column. Doing this first keeps every
        // column in the `added` set resident until generation completes. For a
        // source that retains nothing per view, the call is a no-op; see
        // `ChunkSource::set_retention_radius`.
        source.get().set_retention_radius(radius);

        let next = Self::window(self.center, radius);
        let mut immediate = Vec::new();
        let forgotten: HashSet<(i32, i32)> = self.loaded.difference(&next).copied().collect();
        for &(x, z) in &forgotten {
            immediate.push(proto.encode_forget_chunk(x, z));
        }
        // Centred on the tracker's own centre, which by definition did not move
        // here — a render-distance change is the one view update with no new pose.
        let added = self.added_columns(&next, self.center, facing);

        self.radius = radius;
        self.loaded = next;
        ViewUpdate {
            immediate,
            forgotten,
            added,
        }
    }
}

/// The join view, split into **Chebyshev rings** ordered outward from the
/// player's own column — the join stream.
///
/// Ring `r` is every column at Chebyshev (chess-king) distance exactly `r` from
/// the centre, so ring 0 is the single column the player is standing in, ring 1
/// is the 8 around it, and ring `r > 0` holds `8r` columns. Flattened, the
/// result is the whole `[-view_radius, view_radius]²` square with **no column
/// repeated and none missing**.
///
/// # These are offsets, not chunk coordinates
///
/// Every pair is a `(dx, dz)` **relative to the player's own column**, which is
/// why ring 0 is `(0, 0)` at every view radius. The caller must add the join
/// centre before any of it reaches `encode_chunk` or a chunk source. This
/// produces the same set that `ViewTracker::new` seeds, in a different order;
/// the property belongs to the call site, not this function.
///
/// # Why rings
///
/// The join enumerates Chebyshev rings rather than raster order from
/// `(-view_radius, -view_radius)`. With 361 columns, the player's own column
/// is first, so terrain near the player is encoded promptly. A raster walk
/// places that column around item **~180 of 361**.
///
/// `crate::join_scheduler` flattens these groups and drives a primed sliding
/// window over the result, so the first chunk reaches the client after **one**
/// column of generation while nothing waits on a ring boundary. This function
/// is therefore purely the **wire order**. The grouping states that order
/// clearly, and `join_view_rings_partitions_the_square_exactly` verifies the
/// partition without synchronizing the groups.
///
/// `ViewTracker::build_batch` — the *move*-time counterpart — orders on the
/// same distance-first key rather than a lexicographic `sort_unstable`, so
/// walking into terrain fills nearest-first like joining does. The key is a
/// total order over integers derived from the player's pose, so determinism is
/// preserved.
///
/// The protocol's chunk priority also spirals outward, with ticket level as the
/// priority. This is the measured distance-first slice of that behavior.
///
/// # Determinism
///
/// Order **within** a ring is the same `dz`-outer/`dx`-inner walk the whole
/// square uses, filtered to the ring. So the emitted byte sequence stays
/// a pure function of `view_radius` — independent of thread scheduling, hash
/// seeds, and which arm of [`SourceRef`] generated it.
///
/// # Cost
///
/// One column. Ring 0 is generated alone, which buys the one-column
/// time-to-first-chunk; from the second column onward the in-flight window
/// spans ring boundaries freely. This keeps worker utilization independent
/// of the slowest column in each ring.
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

/// How many rings of the join view are sent **before** the play loop starts.
///
/// `1` — the player's own column plus its eight neighbours, nine columns of the
/// 1,089 a `view_radius = 16` join owes. Everything past this streams from
/// `serve_play` while the player is already able to act.
///
/// # Why not `0`, and why not more
///
/// Vanilla's answer is essentially "the player's own chunk, then keep sending":
/// vanilla's own "place new player" step adds the player to the level and
/// its own player-chunk-sender feeds the rest over subsequent ticks. The extra ring here
/// is the spawn-safety story this crate already paid for once — an earlier defect
/// spawned the player above terrain, let them fall, and reached zero health with
/// no death screen — so the ground the player stands on *and* the eight columns
/// they can step onto exist on the wire before anything they do can matter. Nine
/// columns is ~0.8% of the burst, so it costs essentially none of the latency the
/// split buys.
///
/// Larger values are a straight trade of interaction latency for that margin;
/// smaller ones put the player one step from a column the client has not been
/// sent. Note this is a *wire* bound, not a world bound: the server can already
/// read any block through `ChunkSource::block_state`, which generates on demand,
/// so this is about what the **client** can stand on.
const JOIN_PRESTREAM_RADIUS: i32 = 1;

/// How many columns of the deferred join stream `serve_play` puts in one chunk
/// batch.
///
/// The deferred half is batched because a stream spans ticks and the batch
/// markers cannot remain open across other play-loop traffic. This matches the
/// standard pacing shape, and the client answers each
/// `chunk_batch_finished` with a `chunk_batch_received` carrying its desired rate
/// (`crates/protocol/v770/src/adapter.rs`'s `ChunkBatchState`).
///
/// 16 is a compromise with no measurement behind it and does not need one: a
/// batch marker is two empty-ish packets, so the cost is ~2 packets per 16
/// columns, and the *only* thing the size changes is the granularity of the
/// client's own rate estimate.
const JOIN_STREAM_BATCH_COLUMNS: usize = 16;

/// Applies one [`ViewUpdate`]: the cache-center and forget directives right away,
/// then the newly-visible columns.
///
/// # The two ways the columns get sent, and why the first is the point
///
/// **Preferred: hand them to `stream`.** `serve_play` drains a
/// [`JoinChunkStream`](crate::join_scheduler::JoinChunkStream) from a `select!`
/// branch one column at a time. The strip is generated on the blocking pool with
/// a primed window, re-keyed as the player moves, and interleaved with connection
/// reads and writes.
///
/// **Fallback: build the batch here.** `stream` refuses on its `Ringed` arm (a
/// borrowed, non-`'static` source — protocol tests) and when the caller has no
/// stream at all (`wasm32`, whose loop has no `select!` to drain one from). Those
/// generate, encode, and send under
/// `awaiting_chunk_batch_ack`, the one-batch-in-flight gate
/// `ServerBound::ChunkBatchAcknowledged` closes.
///
/// The streamed path is deliberately **not** subject to that gate, because the join
/// stream it shares is not: batches there are paced by `JOIN_STREAM_BATCH_COLUMNS`
/// and the client's own `chunk_batch_received` rate estimate. Routing a move through
/// the gate *and* the stream would need two flow-control regimes over one ordered
/// queue, which is how the two paths drift.
async fn send_view_update<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    stream: Option<&mut crate::join_scheduler::JoinChunkStream<S>>,
    state: &mut State,
    update: ViewUpdate,
    awaiting_chunk_batch_ack: &mut bool,
    pending_chunk_batches: &mut VecDeque<Vec<ServerDirective>>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
{
    for directive in update.immediate {
        apply(conn, state, directive).await?;
    }
    // Asked, not attempted: a stream that refused *after* taking the coordinates
    // would leave the fallback below with nothing to send. See
    // `JoinChunkStream::accepts_enqueue`.
    let stream = stream.filter(|stream| stream.accepts_enqueue());
    if let Some(stream) = stream {
        // Withdraw before enqueueing, and unconditionally — a shrink forgets columns
        // and adds none, and those still owed must be dropped just the same. See
        // `ColumnPipeline::cancel`.
        stream.cancel(&update.forgotten);
        stream.enqueue(update.added);
        return Ok(());
    }
    if update.added.is_empty() {
        return Ok(());
    }
    let mut batch = vec![proto.begin_chunk_batch()];
    let count = update.added.len() as i32;
    // Only the `Shared` arm can offload (a borrowed source is not `'static`), which
    // is the same fork `SourceRef` already encodes; both arms emit the same bytes in
    // the same order.
    let offloaded = if proto.uses_cross_column_light() {
        None
    } else {
        match source {
            SourceRef::Shared(src) => {
                crate::chunk::generate_and_encode_columns_offloaded(
                    Arc::clone(src),
                    update.added.clone(),
                    proto.chunk_encoder(),
                )
                .await
            }
            SourceRef::Dimension(src) => {
                crate::chunk::generate_and_encode_columns_offloaded(
                    Arc::clone(src),
                    update.added.clone(),
                    proto.chunk_encoder(),
                )
                .await
            }
            SourceRef::Borrowed(_) => None,
        }
    };
    match offloaded {
        Some(Ok(frames)) => batch.extend(frames),
        Some(Err(error)) => {
            return return_chunk_encode_error(conn, proto, state, None, error).await;
        }
        None => {
            let columns = source.generate(update.added.clone()).await;
            for (&(x, z), column) in update.added.iter().zip(columns.iter()) {
                match encode_chunk_with_source(proto, source.get(), x, z, column) {
                    Ok(directive) => batch.push(directive),
                    Err(error) => {
                        return return_chunk_encode_error(conn, proto, state, None, error).await;
                    }
                }
            }
        }
    }
    batch.push(proto.end_chunk_batch(count));
    if *awaiting_chunk_batch_ack {
        pending_chunk_batches.push_back(batch);
        return Ok(());
    }
    *awaiting_chunk_batch_ack = true;
    for directive in batch {
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
    /// The protocol could not encode a generated chunk column.
    #[error("chunk encoding failed: {0}")]
    ChunkEncode(#[from] ChunkEncodeError),
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

/// The session-server check a login performs once encryption is up: given the
/// HTTP client, the username and the server-id hash, answer whether the
/// client really holds the shared secret it claims to.
///
/// Boxed rather than a plain `fn` pointer so [`OnlineModeConfig::for_test`]
/// can close over a fixture instead of a real client — see that
/// constructor's own doc comment for why a substitutable seam exists here at
/// all rather than only [`OnlineModeConfig::new`].
#[cfg(not(target_arch = "wasm32"))]
type SessionVerify = Arc<
    dyn Fn(
            reqwest::Client,
            String,
            String,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = lodestone_auth::Result<Option<lodestone_auth::HasJoinedProfile>>> + Send>,
        > + Send
        + Sync,
>;

/// Configuration for the online-mode encryption + session-server handshake
/// (the online-mode handshake). Pass `Some` to opt a connection into online
/// mode; callers that do not enable authentication pass `None` for offline
/// mode.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct OnlineModeConfig {
    /// The HTTP client the session-server `hasJoined` call uses. Owned by the
    /// caller (rather than constructed fresh per login) so a real host can
    /// share one client's connection pool across every player who joins.
    pub http: reqwest::Client,
    verify: SessionVerify,
    /// Host-shared Mojang issuer keys for profile-key provenance. The mutex
    /// covers only the tiny cache update/read; HTTP always happens after it is
    /// released so one slow services request never stalls another login.
    profile_key_cache: Arc<Mutex<lodestone_auth::MojangPublicKeyCache>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Debug for OnlineModeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `SessionVerify` (a boxed `Fn`) has no `Debug` impl to derive; a
        // one-line placeholder is more useful than a compile error over a
        // lint.
        f.debug_struct("OnlineModeConfig").finish_non_exhaustive()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl OnlineModeConfig {
    /// The real thing: `verify` calls [`lodestone_auth::has_joined`] against
    /// `sessionserver.mojang.com`.
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            verify: Arc::new(|http, username, hash| {
                Box::pin(async move { lodestone_auth::has_joined(&http, &username, &hash).await })
            }),
            profile_key_cache: Arc::new(Mutex::new(
                lodestone_auth::MojangPublicKeyCache::empty(),
            )),
        }
    }

    /// Substitutes a fixture for the real session-server call in integration
    /// tests. This constructor is available in normal builds because those
    /// tests use the versioned protocol crate as a regular dependency.
    ///
    /// The fixture prevents login tests from contacting the external session
    /// service. The crate has no HTTP-mocking dependency, so the verifier is
    /// injected directly at this seam.
    ///
    /// **Not `#[cfg(test)]`**: the login-sequence test that needs it
    /// (`tests/online_mode.rs`) drives the real [`V770ServerProtocol`] from
    /// `lodestone-v26-2`, which has a *normal* dependency on this crate for the
    /// `ServerProtocol` trait. Adding `lodestone-v26-2` as a *dev*-dependency
    /// here (so a `#[cfg(test)] mod tests` unit test could reach it) makes
    /// this crate's own lib-test compilation and the copy `lodestone-v26-2`
    /// links against two different instantiations of the same trait —
    /// measured: `V770ServerProtocol: ServerProtocol is not implemented`
    /// against the crate's own trait. An external `tests/*.rs` binary has no
    /// such self-reference (it depends on this crate exactly once, normally),
    /// so the test using this constructor lives there, and it needs `pub`.
    pub fn for_test(
        verify: impl Fn(String, String) -> lodestone_auth::Result<Option<lodestone_auth::HasJoinedProfile>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        // `reqwest::Client::new()` panics without a crypto provider installed
        // (see `lodestone_auth::install_crypto_provider`'s own doc); this
        // `http` value is never actually used by the fixture `verify` below
        // (it ignores its `_http` parameter), but the field still needs a
        // real, valid `Client` to satisfy the type. Installing twice in one
        // process is not an error.
        lodestone_auth::install_crypto_provider();
        let verify = Arc::new(verify);
        Self {
            http: reqwest::Client::new(),
            verify: Arc::new(move |_http, username, hash| {
                let result = verify(username, hash);
                Box::pin(async move { result })
            }),
            profile_key_cache: Arc::new(Mutex::new(
                lodestone_auth::MojangPublicKeyCache::empty(),
            )),
        }
    }

    /// Returns the latest issuer-key snapshot, refreshing the shared cache
    /// when authlib policy says it is due. A successful response lives for 24
    /// hours; failures retain the last good set and schedule the capped
    /// 5–320-minute backoff. A first-fetch failure returns `None`, which makes
    /// secure-profile enforcement degrade exactly as vanilla does when it
    /// cannot validate profile keys.
    async fn profile_key_issuers(
        &self,
        now_millis: i64,
    ) -> Option<lodestone_auth::MojangPublicKeys> {
        let due = self
            .profile_key_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .needs_refresh(now_millis);
        if due {
            let fetched = lodestone_auth::fetch_mojang_public_keys(&self.http).await;
            let mut cache = self
                .profile_key_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match fetched {
                Ok(keys) => cache.record_success(keys, now_millis),
                Err(error) => {
                    cache.record_failure(now_millis);
                    tracing::warn!(
                        error = %error,
                        "Mojang profile-key issuer refresh failed; announcement validation and secure-profile enforcement are unavailable until a key set is cached"
                    );
                }
            }
        }
        self.profile_key_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
    }
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

/// This world's per-player `.dat` store, if it has one.
///
/// One accessor rather than the same `world_registries().and_then(...)` chain at
/// three call sites, because the failure mode of getting it wrong is invisible:
/// a chain that returns `None` where a store exists produces a server that joins,
/// plays and saves nothing, with no error. Keeping this lookup in one accessor
/// makes the save-store dependency explicit at each call site.
#[cfg(not(target_arch = "wasm32"))]
fn player_store<S: ChunkSource + ?Sized>(source: &S) -> Option<crate::player_data::PlayerDataStore> {
    source
        .world_registries()
        .and_then(|registries| registries.player_data)
}

/// The native half of one live connection's bounded player persistence.
///
/// The complete Anvil [`PlayerData`](crate::player_data::PlayerData) remains
/// the source of truth for inventory, health, game mode and opaque fields.
/// This session only adds the typed locator record, and marks itself blocked
/// after a corrupt read so a later disconnect cannot overwrite evidence needed
/// for recovery.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct NativePlayerSession {
    storage: Arc<crate::world_storage::WorldStorage>,
    uuid: [u8; 16],
    loaded: Option<crate::world_storage::NativePlayerRecord>,
    save_blocked: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativePlayerSession {
    fn load<S: ChunkSource + ?Sized>(source: &S, uuid: uuid::Uuid) -> Option<Self> {
        let storage = source.world_registries()?.native_storage?;
        let uuid = *uuid.as_bytes();
        match storage.load_player(uuid) {
            Ok(loaded) => {
                let save_blocked = loaded.is_some_and(|record| {
                    record.dimension != lodestone_storage_schema::BuiltinDimension::Overworld
                });
                if save_blocked {
                    tracing::warn!(
                        "native player locator for {uuid:02x?} names a non-overworld dimension; it will not be overwritten until dimension-aware join restore exists"
                    );
                }
                Some(Self {
                    storage,
                    uuid,
                    loaded,
                    save_blocked,
                })
            }
            Err(error) => {
                tracing::error!(
                    "native player locator for {uuid:02x?} could not be read and will NOT be overwritten this session: {error}"
                );
                Some(Self {
                    storage,
                    uuid,
                    loaded: None,
                    save_blocked: true,
                })
            }
        }
    }

    fn join_position(&self, fallback: Vec3) -> Vec3 {
        let Some(record) = self.loaded else {
            return fallback;
        };
        if record.dimension != lodestone_storage_schema::BuiltinDimension::Overworld {
            tracing::warn!(
                "native player locator for {:?} names a non-overworld dimension; using the world spawn until dimension-aware join restore exists",
                self.uuid
            );
            return fallback;
        }
        native_position(record)
    }

    fn initial_rotation(&self) -> Option<Rotation> {
        self.loaded
            .filter(|record| {
                record.dimension == lodestone_storage_schema::BuiltinDimension::Overworld
            })
            .map(native_rotation)
    }

    fn snapshot(
        &self,
        player_pos: Option<(f64, f64, f64)>,
        player_rot: Option<Rotation>,
        fallback: Vec3,
        dimension: crate::dimension::Dimension,
    ) -> Option<crate::world_storage::NativePlayerRecord> {
        if self.save_blocked {
            return None;
        }
        let position = player_pos
            .map(|(x, y, z)| Vec3::new(x, y, z))
            .or_else(|| {
                self.loaded
                    .filter(|record| {
                        record.dimension == lodestone_storage_schema::BuiltinDimension::Overworld
                    })
                    .map(native_position)
            })
            .unwrap_or(fallback);
        let rotation = player_rot
            .or_else(|| {
                self.loaded
                    .filter(|record| {
                        record.dimension == lodestone_storage_schema::BuiltinDimension::Overworld
                    })
                    .map(native_rotation)
            })
            .unwrap_or_default();
        Some(crate::world_storage::NativePlayerRecord {
            uuid: self.uuid,
            dimension: native_dimension(dimension),
            x_fixed: native_fixed_coordinate(position.x)?,
            y_fixed: native_fixed_coordinate(position.y)?,
            z_fixed: native_fixed_coordinate(position.z)?,
            yaw_millidegrees: native_fixed_rotation(rotation.yaw)?,
            pitch_millidegrees: native_fixed_rotation(rotation.pitch)?,
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn native_dimension(
    dimension: crate::dimension::Dimension,
) -> lodestone_storage_schema::BuiltinDimension {
    match dimension {
        crate::dimension::Dimension::Overworld => lodestone_storage_schema::BuiltinDimension::Overworld,
        crate::dimension::Dimension::Nether => lodestone_storage_schema::BuiltinDimension::Nether,
        crate::dimension::Dimension::End => lodestone_storage_schema::BuiltinDimension::End,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn native_position(record: crate::world_storage::NativePlayerRecord) -> Vec3 {
    let units = crate::anvil_player_storage::POSITION_UNITS_PER_BLOCK;
    Vec3::new(
        f64::from(record.x_fixed) / units,
        f64::from(record.y_fixed) / units,
        f64::from(record.z_fixed) / units,
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn native_rotation(record: crate::world_storage::NativePlayerRecord) -> Rotation {
    Rotation::new(
        record.yaw_millidegrees as f32 / 1_000.0,
        record.pitch_millidegrees as f32 / 1_000.0,
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn native_fixed_coordinate(value: f64) -> Option<i32> {
    native_fixed(value, crate::anvil_player_storage::POSITION_UNITS_PER_BLOCK)
}

#[cfg(not(target_arch = "wasm32"))]
fn native_fixed_rotation(value: f32) -> Option<i32> {
    native_fixed(f64::from(value), 1_000.0)
}

#[cfg(not(target_arch = "wasm32"))]
fn native_fixed(value: f64, scale: f64) -> Option<i32> {
    let scaled = value * scale;
    if !scaled.is_finite()
        || scaled.round() < f64::from(i32::MIN)
        || scaled.round() > f64::from(i32::MAX)
    {
        return None;
    }
    Some(scaled.round() as i32)
}

#[cfg(not(target_arch = "wasm32"))]
fn persist_native_player(
    session: Option<&NativePlayerSession>,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    fallback: Vec3,
    dimension: crate::dimension::Dimension,
) {
    let Some(session) = session else {
        return;
    };
    let Some(record) = session.snapshot(player_pos, player_rot, fallback, dimension) else {
        tracing::error!(
            "native player locator for {:?} contains a non-finite or out-of-range live value; it was not written",
            session.uuid
        );
        return;
    };
    if let Err(error) = session.storage.write_dirty_player(record) {
        tracing::warn!("could not save native player locator for {:?}: {error}", session.uuid);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn publish_native_player(
    session: Option<&NativePlayerSession>,
    live_save: &crate::live_save::LiveSaveSlot,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    fallback: Vec3,
    dimension: crate::dimension::Dimension,
) {
    let Some(session) = session else {
        return;
    };
    let Some(record) = session.snapshot(player_pos, player_rot, fallback, dimension) else {
        return;
    };
    live_save.publish_native(Some(session.storage.clone()), record);
}

/// Writes this connection's live state to its `.dat` file.
///
/// # Why the position is an `Option`
///
/// `player_pos` is `None` until the client sends its first movement packet, and
/// this really does happen: a client that joins and closes the window
/// immediately never sends one. Persisting a `(0, 0, 0)` in that case would
/// teleport the player into the void on their next join, so `fallback` — the
/// position they joined at — is written instead. Neither value is a guess: both
/// are positions the server itself placed them at.
///
/// A `None` store is a world with nothing to save into, and this is then a no-op
/// rather than an error; that is the in-memory and browser case.
///
/// Blocking (a gzip encode plus two renames, a few hundred bytes) and called
/// from the connection task. Deliberately *not* `spawn_blocking`: it is orders of
/// magnitude smaller than a region write, and the call at the disconnect return
/// has to complete before the task ends — handing it to a pool there would race
/// the runtime shutting the pool down, which loses exactly the save that matters
/// most.
/// Where a returning player should re-enter the world, given their saved
/// state (if any, `saved`) and the world spawn as the fallback (`world_spawn`).
///
/// **Respawn uses a saved position only for the matching dimension.** A saved position
/// is trusted verbatim only when [`PlayerData::dimension`](crate::player_data::PlayerData)
/// identifies the overworld, the dimension used by a fresh connection before
/// any portal trip. Positions captured in another dimension fall back to the
/// world spawn instead of being interpreted in overworld coordinates.
///
/// [`crate::dimension::Dimension::from_key`] returning `None` — an
/// unparseable tag — degrades the same way a genuinely non-overworld tag
/// does: fall back to the world spawn rather than trust a position whose
/// dimension is not actually known. An ambiguous saved position cannot be
/// recovered; falling back to the world spawn avoids trusting a coordinate
/// whose dimension is unknown.
#[cfg(not(target_arch = "wasm32"))]
fn join_position_for_saved_player(
    saved: Option<&crate::player_data::PlayerData>,
    world_spawn: Vec3,
) -> Vec3 {
    saved.map_or(world_spawn, |data| {
        if crate::dimension::Dimension::from_key(&data.dimension)
            == Some(crate::dimension::Dimension::Overworld)
        {
            data.spawn_state().pos
        } else {
            world_spawn
        }
    })
}

/// Builds the [`PlayerData`](crate::player_data::PlayerData) snapshot
/// [`persist_player`] would write and [`live_publish_player`] would mirror,
/// without doing either — the construction half, factored out so both the
/// two deliberate disk-write call sites and `serve_play`'s per-iteration
/// live-publish (see [`crate::live_save::LiveSaveSlot`]) build the
/// identical snapshot from the identical arguments rather than risking two
/// copies drifting.
///
/// See [`persist_player`]'s own doc comment for why `player_pos` is an
/// `Option` and `fallback` exists.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn player_save_snapshot(
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    fallback: Vec3,
    vitals: &PlayerVitals,
    game_mode: GameMode,
    inventory: &PlayerInventory,
    experience: &crate::experience::PlayerExperience,
    preserved: &[(String, lodestone_core::Nbt)],
    // The dimension `player_pos`/`fallback` are expressed in — the caller's
    // own current `SourceRef::dimension()`, not always the overworld. See
    // `PlayerData::capture`'s own doc comment for why this is load-bearing
    // rather than a label: a Nether-relative position with the wrong (or no)
    // dimension tag must not be interpreted in overworld coordinates.
    dimension: crate::dimension::Dimension,
) -> crate::player_data::PlayerData {
    let pos = player_pos.map_or(fallback, |(x, y, z)| Vec3::new(x, y, z));
    crate::player_data::PlayerData::capture(
        pos,
        player_rot.unwrap_or(Rotation::new(0.0, 0.0)),
        vitals.health(),
        vitals.air_supply(),
        game_mode,
        inventory,
        *experience,
        preserved.to_vec(),
        dimension,
    )
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn persist_player(
    store: Option<&crate::player_data::PlayerDataStore>,
    uuid: uuid::Uuid,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    fallback: Vec3,
    vitals: &PlayerVitals,
    game_mode: GameMode,
    inventory: &PlayerInventory,
    // The live level/bar/total. Saved *and* restored, which has to be one change:
    // modelling the three `Xp*` fields without reading them back would write this
    // session's zeroes over the file's real XP on the first save, which is strictly
    // worse than the bug it replaces.
    experience: &crate::experience::PlayerExperience,
    preserved: &[(String, lodestone_core::Nbt)],
    dimension: crate::dimension::Dimension,
) {
    let Some(store) = store else {
        return;
    };
    let data = player_save_snapshot(
        player_pos, player_rot, fallback, vitals, game_mode, inventory, experience, preserved,
        dimension,
    );
    if let Err(err) = store.write(uuid, &data) {
        tracing::warn!("could not save player data for {uuid}: {err}");
    }
}

/// Refreshes `live_save` with the current live state — see
/// [`crate::live_save::LiveSaveSlot`]'s own doc comment. Cheap and
/// in-memory only (no disk I/O), unlike [`persist_player`]: `serve_play`
/// calls this once per iteration of its own `select!` loop, not only at the
/// two deliberate save points.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn live_publish_player(
    live_save: &crate::live_save::LiveSaveSlot,
    store: Option<&crate::player_data::PlayerDataStore>,
    uuid: uuid::Uuid,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    fallback: Vec3,
    vitals: &PlayerVitals,
    game_mode: GameMode,
    inventory: &PlayerInventory,
    experience: &crate::experience::PlayerExperience,
    preserved: &[(String, lodestone_core::Nbt)],
    dimension: crate::dimension::Dimension,
) {
    let data = player_save_snapshot(
        player_pos, player_rot, fallback, vitals, game_mode, inventory, experience, preserved,
        dimension,
    );
    live_save.publish(store.cloned(), uuid, data);
}

/// Turns one [`ColumnPayload`](crate::join_scheduler::ColumnPayload) into the
/// directive to write, whichever arm it is on.
///
/// This is the join path's *only* remaining branch on where encode happened, and
/// it is deliberately a total function rather than two call sites: the
/// [`Encoded`](crate::join_scheduler::ColumnPayload::Encoded) arm carries bytes a
/// blocking worker already produced (the win — see
/// [`crate::protocol::ChunkEncoder`]), while the
/// [`Column`](crate::join_scheduler::ColumnPayload::Column) arm is the
/// pre-existing shape for a protocol with no off-task encoder and for the
/// non-`'static` [`SourceRef::Borrowed`] arm. Both produce the same bytes, so no
/// caller has to know which one it is on.
fn encode_chunk_with_source<P: ServerProtocol>(
    proto: &P,
    source: &dyn ChunkSource,
    cx: i32,
    cz: i32,
    column: &ChunkColumn,
) -> Result<ServerDirective, ChunkEncodeError> {
    if !proto.uses_cross_column_light() {
        return proto.try_encode_chunk(cx, cz, column);
    }
    let neighbours = (-1..=1)
        .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
        .filter(|&(dx, dz)| (dx, dz) != (0, 0))
        .filter_map(|(dx, dz)| {
            source
                .resident_column(cx + dx, cz + dz)
                .map(|column| (dx, dz, column))
        })
        .collect::<Vec<_>>();
    proto.try_encode_chunk_with_neighbours(cx, cz, column, &neighbours)
}

fn encode_column<P: ServerProtocol, S: ChunkSource + 'static>(
    proto: &P,
    source: SourceRef<'_, S>,
    cx: i32,
    cz: i32,
    payload: crate::join_scheduler::ColumnPayload,
) -> Result<ServerDirective, ChunkEncodeError> {
    match payload {
        crate::join_scheduler::ColumnPayload::Encoded(directive) => Ok(directive),
        crate::join_scheduler::ColumnPayload::Column(column) => {
            encode_chunk_with_source(proto, source.get(), cx, cz, &column)
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

/// [`serve_connection_with_mob_events`], but with chunk generation off the
/// core thread.
///
/// This compatibility wrapper has no command dispatcher; integrated
/// singleplayer uses [`serve_connection_with_mob_events_and_commands_shared`].
/// Callers that need only mob event feeds use this entry point.
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
    // The caller supplies the ceiling: in-memory worlds use
    // [`MAX_CLIENT_VIEW_RADIUS`], while LAN hosts use their configured
    // `view_radius`. See `ViewTracker::max_radius`.
    max_view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    block_ticks: &BlockTickFeed,
    explosions: &ExplosionFeed,
    // Night-skip vote state and notifications consumed by the world tick.
    sleep_vote: &SleepVote,
    sleep_feed: &SleepFeed,
    // Shared world-border state updated by the world tick and `/worldborder`
    // commands. Other wrappers use an unconfigured border feed.
    border: &BorderFeed,
    // Shared world rules, difficulty, and clock updated by `run_tick_loop`.
    world: &crate::world_state::WorldStateHandle,
    // `serve_play` publishes the player's live-save mirror each loop iteration.
    // LAN connections use connection-local slots; integrated shutdown reads the
    // slot supplied by the host.
    live_save: &crate::live_save::LiveSaveSlot,
    // `IntegratedServer`'s real handle — see
    // `serve_connection_shared`'s own parameter comment; this is the
    // singleplayer/open-to-LAN sibling that carries it.
    tickets: &TicketStoreHandle,
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
        &CommandDispatch::none(),
        border,
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
        world,
        live_save,
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

/// [`serve_connection_with_mob_events_shared`], plus a host-installed command
/// dispatcher (the host-installed command dispatcher).
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
        // variant is `serve_connection_with_mob_events_shared`.
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
                let sent_encryption_request = if let Some(cfg) = online_mode {
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
                    Some(stored) => stored,
                    None => {
                        let found = find_initial_spawn(source.get());
                        world.set_world_spawn(found);
                        found
                    }
                };

                // keep the world spawn's own chunks loaded
                // independent of where any particular player is standing —
                // vanilla's own spawn-preparation task, radius 3
                // (`ticket::PLAYER_SPAWN_RADIUS`). Re-granting under the same
                // `(TicketOwner::Spawn, TicketKind::PlayerSpawn)` key on every
                // join is a refresh, not a second ticket — see `ticket.rs`'s
                // own doc for why a ticket is keyed by owner+kind rather than
                // position. Every entry point but `IntegratedServer`'s real
                // join paths carries a fresh, disconnected
                // `TicketStoreHandle::default()` here, so this is a no-op
                // nobody reads on those, exactly like every other feed in
                // this file.
                let spawn_chunk = (
                    (spawn.pos.x / 16.0).floor() as i32,
                    (spawn.pos.z / 16.0).floor() as i32,
                );
                tickets.set_ticket_with_radius(
                    TicketOwner::Spawn,
                    TicketKind::PlayerSpawn,
                    spawn_chunk,
                    PLAYER_SPAWN_RADIUS,
                );

                // this player's own saved state, if this world has
                // any. Reached through `ChunkSource::world_registries` rather
                // than a new parameter — see `crate::chunk::WorldRegistries`'s
                // `player_data` field for why that routing was chosen over
                // threading a 31st argument through eleven wrappers.
                //
                // An in-memory world answers `None` and every existing caller
                // therefore behaves exactly as before.
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
                let native_player = NativePlayerSession::load(
                    source.get(),
                    login_uuid.unwrap_or_default(),
                );

                // Where the player actually re-enters the world. `spawn.pos`
                // stays the **world** spawn: it is what `serve_play` uses for a
                // respawn, and overwriting it with the player's last position
                // would respawn a dead player back where they died. See
                // `join_position_for_saved_player`'s own doc comment for why a
                // saved position is not always trusted verbatim.
                #[cfg(not(target_arch = "wasm32"))]
                let join_pos = {
                    let anvil_pos =
                        join_position_for_saved_player(saved_player.as_ref(), spawn.pos);
                    native_player
                        .as_ref()
                        .map_or(anvil_pos, |native| native.join_position(anvil_pos))
                };
                #[cfg(target_arch = "wasm32")]
                let join_pos = spawn.pos;
                // Vanilla's own player-game-type field, restored — a player who typed
                // `/gamemode survival` and quit comes back in survival. Shadowed
                // rather than assigned so a world with no save keeps the mode the
                // host opened with.
                #[cfg(not(target_arch = "wasm32"))]
                let game_mode = saved_player
                    .as_ref()
                    .and_then(|data| data.game_mode)
                    .unwrap_or(game_mode);

                state = State::Play;
                let initial_teleport_id = proto.uses_teleport_acknowledgements().then_some(0);
                for directive in proto.begin_play_at_with_teleport_id(
                    view_radius,
                    join_pos,
                    game_mode,
                    initial_teleport_id.unwrap_or(0),
                ) {
                    apply(conn, &mut state, directive).await?;
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
                let t_chunks = JoinStopwatch::now();
                let mut batch_size = 0;
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
                        let mut pipeline = crate::join_scheduler::ColumnPipeline::prioritised(
                            Arc::clone(src),
                            coords,
                            window,
                            (join_cx, join_cz),
                            None,
                        )
                        .with_generation_band(
                            (join_cx, join_cz),
                            crate::join_scheduler::DEFAULT_FULL_GENERATION_RADIUS,
                        )
                        .encoding_with(if proto.uses_cross_column_light() {
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
                            let directive = match encode_column(proto, source, cx, cz, payload) {
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
                            apply(conn, &mut state, directive).await?;
                            batch_size += 1;
                        }
                        join_stream = crate::join_scheduler::JoinChunkStream::windowed(pipeline);
                    }
                    // The `Dimension` arm rides here rather than with `Shared`
                    // because it is **unreachable at join**: a connection joins in
                    // whichever dimension its own source names, and a portal trip
                    // re-streams through `send_view_update`, not through this block.
                    // Sharing the ring path costs it nothing it can reach and keeps
                    // the offload fork below reading as the two arms it is about.
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
                            let columns = source.generate(ring.clone()).await;
                            for (&(cx, cz), column) in ring.iter().zip(columns.iter()) {
                                let directive = match encode_chunk_with_source(
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
                                apply(conn, &mut state, directive).await?;
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

                // Register this connection as a player entity before initial
                // sync. Other connections then see it on their next pass, and
                // this connection can exclude its own entity. The ticket moves
                // into `serve_play`, whose `Drop` implementation deregisters
                // the player on every exit path.
                let player_ticket = entities.players().map(|registry| {
                    registry.join(
                        &username,
                        login_uuid.unwrap_or_else(uuid::Uuid::nil),
                        join_pos,
                    )
                });

                // The connection's chunk-residency ticket pair
                // (`PLAYER_LOADING` + `PLAYER_SIMULATION`) is keyed by the
                // same login uuid `PlayerRegistry::join` above uses for the
                // entity ticket — XOR-folded to a `u64` since
                // `TicketOwner::Player` only needs per-connection uniqueness,
                // never identity (see `TicketStoreHandle::grant_player`'s own
                // doc). Moved into `serve_play` below; its `Drop` withdraws
                // both tickets on every exit path out of that function,
                // exactly like `player_ticket` just above. A disconnected
                // `TicketStoreHandle::default()` gives non-integrated callers
                // an isolated store, so these operations do not affect shared
                // residency.
                let player_ticket_guard = {
                    let bits = login_uuid.unwrap_or_else(uuid::Uuid::nil).as_u128();
                    let id = (bits as u64) ^ ((bits >> 64) as u64);
                    tickets.grant_player(id, (join_cx, join_cz), view_radius)
                };

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
                let view = ViewTracker::new((spawn_cx, spawn_cz), view_radius, max_view_radius);
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
                return serve_play(
                    conn,
                    proto,
                    source,
                    entities,
                    view_radius,
                    state,
                    initial_teleport_id,
                    streamer,
                    player_list,
                    player_ticket,
                    player_ticket_guard,
                    view,
                    username,
                    spawn.pos,
                    chunks_sent,
                    join_stream,
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
                )
                .await;
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
        // Every menu this function opens is a plain `generic_*` container
        // shape *except* the beacon, whose one payment slot has its own
        // restricted `may_place`/`max_stack_size` (`MenuKind::Beacon`'s own
        // doc) — everything else here keyed on the block entity's own
        // `menu_name()`, matching this function's one caller.
        shape: if menu == "minecraft:beacon" {
            MenuKind::Beacon
        } else {
            MenuKind::Container {
                size: own_slots.len(),
            }
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
/// [`crate::block_breaking`] for the destroy-progress arithmetic, the tolerance
/// it deliberately carries, and what is still not modelled (creative mode and
/// spawn protection). Two behaviours follow from it, and they are opposite ends
/// of the same missing computation:
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
#[allow(clippy::too_many_arguments)]
async fn apply_block_action<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
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
    // Vanilla's very first guard in `handleBlockBreakAction`, ahead of the
    // per-ordinal fork: a break out of reach is dropped whatever phase it is.
    if !crate::block_breaking::within_interaction_range(player_feet, pos) {
        return Ok(());
    }
    match action {
        BlockActionKind::StartDestroy => {
            let target = source.block_state(pos.x, pos.y, pos.z);
            let per_tick = crate::block_breaking::progress_per_tick(&target, held);
            // `None` is a state neither census knows — our gap, not a cheat, so
            // it is priced as an ordinary progressive dig that the `None`-clock
            // branch of `may_break_at` will accept on any `StopDestroy`.
            if creative || per_tick.is_some_and(|per| per >= 1.0) {
                // Vanilla's `"insta mine"` exit: the block is gone now, and no
                // `StopDestroy` is coming for it. This is the one-shot-block fix.
                // Creative takes the same exit for *every* block, which is what
                // makes a creative dig instant rather than merely fast.
                *pending_break = None;
                destroy_block(
                    conn,
                    proto,
                    source,
                    state,
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
            destroy_block(
                conn,
                proto,
                source,
                state,
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
    // Read the block before replacement; once `set_block` runs, the original
    // state cannot be recovered. Capture its fluid state before writing the
    // replacement so a waterlogged block leaves its water source rather than
    // unconditional air (see `new_state` below).
    let broken = source.block_state(pos.x, pos.y, pos.z);
    // The removal write preserves a cell's *fluid* state. For a dry block
    // `fluid_state_of` is `None` and this is plain air, which is why every
    // existing break gate — all of them dry blocks — could not see the
    // difference. A waterlogged block's fluid state is a water source
    // (`fluid_state_of` reports `amount: 8, falling: false`), so its
    // `block_state()` is `minecraft:water[level=0]`, the source state left
    // behind.
    let new_state = crate::fluid::fluid_state_of(&broken)
        .map(crate::fluid::FluidState::block_state)
        .unwrap_or_else(|| AIR.to_owned());
    // The base name, not the state string: `minecraft:mined` is keyed by *block*,
    // so `minecraft:oak_log[axis=y]` and `minecraft:oak_log` must be one counter
    // rather than two. Every other per-block table in this crate strips the suffix
    // the same way.
    advancements.award_stat(
        breaker,
        crate::advancements::StatKey::new(
            crate::advancements::StatType::Mined,
            broken.split('[').next().unwrap_or(&broken),
        ),
        1,
    );
    if let Some(vitals) = exhaust {
        vitals.add_exhaustion(crate::food::EXHAUSTION_MINE);
    }
    if let Some(effect) = crate::effects::block_destroyed(pos, &broken) {
        block_ticks.publish_effect_except(breaker, effect);
    }
    source.set_block(pos.x, pos.y, pos.z, &new_state);
    debug_assert!(
        !broken.is_empty(),
        "`ChunkSource::block_state` returns a state name, never an empty string"
    );
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
    let popped = if drop_loot && crate::block_drops::drops_are_allowed(&broken, held) {
        crate::block_drops::drop_block_loot(
            crate::block_drops::bundled_tables(),
            &broken,
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
        let points = crate::experience::block_break_points(&broken, |bound| {
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
    block_ticks.request_scheduled_ticks(crate::fluid::ticks_after_edit(
        source,
        fluid_env_at(source, pos),
        pos,
    ));
    let directive = proto.encode_block_update(pos.x, pos.y, pos.z, &new_state);
    apply(conn, state, directive).await?;
    // Breaking a light source has to darken the column, and the `BLOCK_UPDATE`
    // above carries no light. See `crate::light` for why this is a column resend
    // rather than a `LIGHT_UPDATE`. `new_state` rather than a hardcoded `AIR`
    // for the same reason as the write above: a broken waterlogged block keeps
    // a light-dampening fluid in the cell, not empty air.
    resend_column_for_light(conn, proto, source, state, &broken, &new_state, pos).await?;

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
                was,
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
    let mut fanned: Vec<(BlockPos, String)> = Vec::new();
    // Vanilla's own tripwire-block affect-neighbors-after-removal routine — the "the string just broke"
    // instant pulse. `broken` is `pos`'s own state from *before* this function
    // overwrote it, exactly what `propagate_removal_with_entities` needs; a
    // no-op for every block that is not a tripwire.
    {
        let (mut changed, scheduled) = propagate_removal_with_entities(source, pos, &broken);
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
        let current = source.block_state(cell.x, cell.y, cell.z);
        let directive = proto.encode_block_update(cell.x, cell.y, cell.z, &current);
        apply(conn, state, directive).await?;
        block_ticks.request_scheduled_ticks(crate::fluid::ticks_after_edit(
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
        let now = source.block_state(cell.x, cell.y, cell.z);
        resend_column_for_light(conn, proto, source, state, was, &now, *cell).await?;
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
pub(crate) fn collapse_unsupported<S>(source: &S, origin: BlockPos) -> Vec<(BlockPos, String)>
where
    S: ChunkSource + ?Sized,
{
    let mut removed: Vec<(BlockPos, String)> = Vec::new();
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
        let was = source.block_state(cell.x, cell.y, cell.z);
        if is_air_or_fluid(&was) {
            continue;
        }
        if crate::block_support::survives(cell, &was, |probe| {
            source.block_state(probe.x, probe.y, probe.z).into()
        }) {
            continue;
        }
        // Removing a block with a fluid state writes that fluid's block state
        // rather than literal air. A waterlogged sign therefore leaves its
        // water source behind when the support block collapses; see
        // `destroy_block`'s `new_state` for the same rule.
        let new_state = crate::fluid::fluid_state_of(&was)
            .map(crate::fluid::FluidState::block_state)
            .unwrap_or_else(|| AIR.to_owned());
        source.set_block(cell.x, cell.y, cell.z, &new_state);
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
/// The column resend survives as the fallback for a family that implements
/// neither method (both default to "nothing"), so adopting the encoder is per
/// family and the old behaviour is still reachable and still correct.
///
/// # What this does *not* fix
///
/// Families that opt into cross-column light receive a fresh 3×3 neighbourhood
/// for each recompute, and this function resends that same 3×3 footprint after
/// a relevant edit. Other families retain the isolated single-column path.
/// `should_relight` compares emission and dampening; see `crate::light` and
/// `docs/server-light.md`.
///
/// The remaining cost on this task is the `source.column(cx, cz)` fetch itself —
/// a retained-column clone warm, a full generation cold — which is why the
/// predicate stays narrow.
async fn resend_column_for_light<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    old_state: &str,
    new_state: &str,
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

/// [`resend_column_for_light`] with the predicate removed — recompute and send one
/// column's light unconditionally.
///
/// # Why this exists separately
///
/// [`crate::light::should_relight`] needs the state the cell held *before* the
/// edit, and there is one real production path that cannot supply it: the world
/// tick loop's changes arrive over [`BlockTickFeed`] as `(x, y, z, new_state)`
/// only, with the old state already overwritten in the shared source by the time
/// this connection drains them.
///
/// World-tick block changes are drained after the source stores the replacement
/// state, so this function resends light unconditionally after each update.
/// Otherwise a fluid tick can remove an underwater torch while the client
/// retains its light. Fire, grass, crops, redstone torches, and falling blocks
/// use the same update feed.
///
/// The absent old state is why this is unconditional rather than predicated. That
/// is the conservative direction and it is affordable: `should_relight` already
/// fires on essentially every placement and break (see [`crate::light`]), so the
/// predicate was not buying much, and the caller **deduplicates by column** —
/// which is what actually bounds the cost, because a fluid cascade can rewrite
/// dozens of cells in one column in a single tick and each flood is a whole-column
/// recompute.
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
    let column = source.column(cx, cz);
    // Both halves have to be present for the cheap path: a family that can
    // compute light but not encode the packet (or the reverse) would otherwise
    // silently send nothing, which is the exact island this replaces.
    let light = if proto.uses_cross_column_light() {
        let neighbours = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
            .filter(|&(dx, dz)| (dx, dz) != (0, 0))
            .map(|(dx, dz)| (dx, dz, source.column(cx + dx, cz + dz)))
            .collect::<Vec<_>>();
        proto.compute_column_light_with_neighbours(&column, &neighbours)
    } else {
        proto.compute_column_light(&column)
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
    // module uses — vanilla's flow control counts batches, so a bare
    // `encode_chunk` outside one leaves the client's accounting short.
    apply(conn, state, proto.begin_chunk_batch()).await?;
    let directive = match proto.try_encode_chunk(cx, cz, &column) {
        Ok(directive) => directive,
        Err(error) => return return_chunk_encode_error(conn, proto, state, Some(0), error).await,
    };
    apply(conn, state, directive).await?;
    apply(conn, state, proto.end_chunk_batch(1)).await?;
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
    block: &str,
    ctx: &crate::block_placement::PlaceContext,
    block_at: F,
) -> Option<crate::block_placement::Placement>
where
    F: Fn(BlockPos) -> WorldState,
{
    if let Some(yaw) = ctx.yaw {
        let look = horizontal_look_direction(yaw);
        let full = match block {
            REPEATER => Some(set_repeater(look.opposite(), 1, false, false)),
            COMPARATOR => Some(set_comparator(look.opposite(), false, false, 0)),
            OBSERVER => Some(set_observer(look, false)),
            _ => None,
        };
        if let Some(state) = full {
            return Some(crate::block_placement::Placement {
                state,
                extra: Vec::new(),
            });
        }
    }
    crate::block_placement::placement(block, ctx, block_at)
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
fn slab_doubles(clicked: &str, held: &str, face: BlockFace, cursor: Vec3f) -> bool {
    if crate::redstone::base_name(clicked) != held {
        return false;
    }
    let above_middle = cursor.y > 0.5;
    let horizontal = !matches!(face, BlockFace::Up | BlockFace::Down);
    match crate::redstone::get_str_property(clicked, "type") {
        Some("bottom") => matches!(face, BlockFace::Up) || (above_middle && horizontal),
        Some("top") => matches!(face, BlockFace::Down) || (!above_middle && horizontal),
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
        "minecraft:water_bottle" => Some(Bottle::new(BottleKind::Potion, "minecraft:water")),
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
        block_state: Option<String>,
    },
    /// Bone meal was extracted (level `8` -> `0`, vanilla's own `extractProduce`)
    /// — the caller spawns the item entity and
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
fn placement_obstructs_placer(target: BlockPos, state: &str, feet: Vec3) -> bool {
    let Some(id) = lodestone_data::block_states::state_id(state) else {
        return false;
    };
    let Some(state) = lodestone_data::block_states::StateId::new(id) else {
        return false;
    };
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
        let clicked = source.block_state(pos.x, pos.y, pos.z);
        let name = clicked.split('[').next().unwrap_or(&clicked);
        if crate::block_entities::container_type_for_block(name).is_some() {
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

    // A crafting table opens a *virtual* menu. It is not a block
    // entity, so the `existing_menu` branch above structurally cannot reach it —
    // see `open_crafting_table_screen`. Ahead of the placement branch for the same
    // reason the `hand_use` block is: right-clicking a table while holding a block
    // opens the table rather than building.
    if source
        .block_state(pos.x, pos.y, pos.z)
        .split('[')
        .next()
        .is_some_and(|name| name == "minecraft:crafting_table")
    {
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
    let clicked_block = source
        .block_state(pos.x, pos.y, pos.z)
        .split('[')
        .next()
        .unwrap_or_default()
        .to_string();
    if let Some(station) = match clicked_block.as_str() {
        "minecraft:anvil" | "minecraft:chipped_anvil" | "minecraft:damaged_anvil" => {
            Some(Station::Anvil)
        }
        "minecraft:grindstone" => Some(Station::Grindstone),
        "minecraft:smithing_table" => Some(Station::Smithing),
        "minecraft:loom" => Some(Station::Loom),
        "minecraft:stonecutter" => Some(Station::Stonecutter),
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
    if clicked_block == "minecraft:enchanting_table" {
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
        let clicked = source.block_state(pos.x, pos.y, pos.z);
        // The cell above supplies the light input for growth checks; it is
        // resolved here because `bone_meal` has no world access of its own.
        let above = source.block_state(pos.x, pos.y + 1, pos.z);
        let outcome = crate::bone_meal::apply_bone_meal(&clicked, &above, bone_meal_rng);
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
                source.set_block(pos.x, pos.y, pos.z, &new_state);
                apply(
                    conn,
                    state,
                    proto.encode_block_update(pos.x, pos.y, pos.z, &new_state),
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
    if is_bed_block(&source.block_state(pos.x, pos.y, pos.z)) {
        // Register the player in the night-skip vote. Bed-entry gates for
        // day/night, nearby monsters, and already-sleeping state are outside
        // this interaction; the 100-tick deep-sleep threshold prevents a
        // single daytime click from advancing the vote. Registration is
        // idempotent, so a repeat click does not double-count.
        sleep_vote.lay_down(player_entity_id);
        if is_legal_bed_respawn(source, pos, player_pos)
            && !respawn.is_some_and(|existing| existing.pos == pos)
        {
            *respawn = Some(RespawnPoint { pos });
            apply(conn, state, proto.encode_system_chat("Respawn point set")).await?;
        }
        return Ok(());
    }

    // The hand-use branch runs **ahead of the placement branch**: a door or
    // other usable block must handle a right-click before block placement;
    // otherwise the block would build instead of opening it. See `crate::hand_use` for the five
    // families and the rules each comes from.
    //
    // Returns early like the bed arm: a click that operated a block is not also a
    // placement.
    {
        let clicked = source.block_state(pos.x, pos.y, pos.z);
        if crate::hand_use::is_hand_usable(&clicked) {
            // The door's partner half, read here because `hand_use` has no world
            // access. `None` for every other family, and for a door whose partner
            // is missing (a half-broken door, which vanilla also tolerates).
            let other_half = crate::redstone_openable::other_door_half_pos(pos, &clicked)
                .map(|p| (p, source.block_state(p.x, p.y, p.z)));
            if let Some(used) = crate::hand_use::hand_use(pos, &clicked, other_half, player_yaw) {
                let mut fanout: Vec<BlockPos> = Vec::new();
                for (p, new_state) in &used.changes {
                    source.set_block(p.x, p.y, p.z, new_state);
                    fanout.push(*p);
                }
                // The placement fan-out notifies neighbouring blocks, so a lever
                // powers the wire beside it rather than merely looking flipped.
                // Without this notification, the redstone model stays correct but
                // is unreachable from a player's hand.
                let mut changed: Vec<(BlockPos, String)> = Vec::new();
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
                    let mut pending: ScheduledTickQueue<String> = ScheduledTickQueue::new();
                    pending.schedule(
                        (pos.x, pos.y, pos.z),
                        crate::hand_use::TICK_BUTTON.to_string(),
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
                    let current = source.block_state(p.x, p.y, p.z);
                    apply(conn, state, proto.encode_block_update(p.x, p.y, p.z, &current)).await?;
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
    let clicked = source.block_state(pos.x, pos.y, pos.z);
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
                .is_some_and(|entity| entity.type_id() == "minecraft:spawner")
        });
        if !spawner_here {
            match crate::spawn_egg::apply_spawn_egg(
                item.name(),
                difficulty,
                pos,
                face,
                &|x, y, z| source.block_state(x, y, z),
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
            let clicked = source.block_state(pos.x, pos.y, pos.z);
            if crate::mobs::minecart::is_rail_block(&clicked) {
                let shape = crate::mobs::minecart::rail_shape(&clicked);
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
                source.set_block(cell.x, cell.y, cell.z, cell_state);
                apply(
                    conn,
                    state,
                    proto.encode_block_update(cell.x, cell.y, cell.z, cell_state),
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
        let clicked = source.block_state(pos.x, pos.y, pos.z);
        let base = clicked
            .split_once('[')
            .map_or(clicked.as_str(), |(base, _)| base);
        if base == "minecraft:tnt" {
            source.set_block(pos.x, pos.y, pos.z, crate::chunk::AIR);
            apply(
                conn,
                state,
                proto.encode_block_update(pos.x, pos.y, pos.z, crate::chunk::AIR),
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
            source.set_block(frame_pos.x, frame_pos.y, frame_pos.z, frame_state);
            apply(
                conn,
                state,
                proto.encode_block_update(frame_pos.x, frame_pos.y, frame_pos.z, frame_state),
            )
            .await?;
            if let Some(fill) = &ignition.portal_fill {
                for (cell, cell_state) in fill {
                    source.set_block(cell.x, cell.y, cell.z, cell_state);
                    apply(
                        conn,
                        state,
                        proto.encode_block_update(cell.x, cell.y, cell.z, cell_state),
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
    // doubling. Every other block reaches the plain air-or-fluid test.
    let doubling_slab = placed.is_some_and(|(_, block)| slab_doubles(&clicked, block.name(), face, cursor));
    let target = if is_air_or_fluid(&clicked) || doubling_slab {
        pos
    } else {
        neighbour
    };
    let target_state = source.block_state(target.x, target.y, target.z);
    // Every cell the placement's neighbour fan-out rewrote —
    // empty unless a placement actually happened below.
    let mut changed: Vec<(BlockPos, String)> = Vec::new();
    // Paired with the `block_update` packets in the notify loop below — see
    // `moving_piston_records`.
    let mut piston_records: Vec<(BlockPos, lodestone_core::Nbt)> = Vec::new();
    // The remainder of the held stack after a successful placement consumed one
    // from it, `None` when nothing was placed or the game mode does not consume.
    // Held out here rather than sent inside the placement block because `state` is
    // *shadowed* in there by the placed block state — see the `let (state, extra)`
    // below — so `apply` cannot be reached from inside it.
    let mut placement_remainder: Option<Option<ItemStack>> = None;
    if is_air_or_fluid(&target_state) || doubling_slab {
        if let Some((item, block)) = placed {
            let block_name = block.name();
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
            };
            let (state, extra) =
                match placed_block_state(block_name, &ctx, |p| source.block_state(p.x, p.y, p.z).into()) {
                    Some(placed) => (placed.state, placed.extra),
                    None => (block_name.to_string(), Vec::new()),
                };
            // Vanilla's own block-item can-place → level unobstructed check: a placement that
            // would collide with the placer's own body is refused, not
            // written — see `placement_obstructs_placer`'s own doc for what
            // this does and does not cover yet. `player_pos` is `None` until
            // the first movement packet arrives; skipped rather than refused
            // in that case, the same conservative-elsewhere-but-permissive-
            // here direction `is_legal_bed_respawn` documents for the same
            // gap.
            let obstructed = player_pos.is_some_and(|feet| placement_obstructs_placer(target, &state, feet));
            if !obstructed {
            if let Some((entity_block, mut entity)) = block_entity_for_item(item.name()) {
                // The two sources must agree on the block name, or we would
                // register a furnace at a position holding some other block.
                // `lodestone-data`'s `the_block_entity_blocks_still_resolve_
                // to_themselves` asserts they do for all six today; this
                // catches a future divergence instead of silently trusting
                // the older table.
                debug_assert_eq!(
                    entity_block, block_name,
                    "block-entity table and item census disagree on {item:?}"
                );
                // A newly placed sign records the placing player as its editor,
                // allowing the following sign-update packet to pass validation.
                if let crate::block_entities::BlockEntity::Sign(sign) = &mut entity {
                    sign.editor = Some(placer);
                }
                block_entities.with(|registry| registry.insert(target, entity));
            } else if let Some(type_name) = lodestone_data::block_states::state_id(&state)
                .and_then(lodestone_data::block_states::StateId::new)
                .and_then(lodestone_data::block_entity_types::block_entity_type)
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
                            id: type_name.to_owned(),
                            nbt: lodestone_core::Nbt::End,
                        },
                    );
                });
            }
            source.set_block(target.x, target.y, target.z, &state);
            // Publish the placement sound to every viewer except the placer.
            // `roll` supplies the per-click seed for choosing the sound variant.
            if let Some(effect) =
                crate::effects::block_placed(target, &state, roll.to_bits() as i64)
            {
                block_ticks.publish_effect_except(placer, effect);
            }
            // A door's upper half, a bed's head, a chest partner's re-typing:
            // cells the placement owns but the client did not predict, so each
            // needs its own `block_update` below.
            for (p, s) in &extra {
                source.set_block(p.x, p.y, p.z, s);
                changed.push((*p, s.clone()));
            }
            // A carved pumpkin or jack o'lantern can complete a snow- or
            // iron-golem pattern. The mob simulation reports the consumed
            // pattern cells; this caller clears them to air.
            if matches!(block, Block::CarvedPumpkin | Block::JackOLantern) {
                let construction = mobs.with(|sim| {
                    sim.try_construct_golem(
                        &|x, y, z| source.block_state(x, y, z).to_owned(),
                        (target.x, target.y, target.z),
                    )
                });
                if let Some(construction) = construction {
                    for cell in &construction.consumed {
                        source.set_block(cell.x, cell.y, cell.z, "minecraft:air");
                        changed.push((*cell, "minecraft:air".to_string()));
                    }
                }
            }
            // A wither skeleton skull or wall skull can complete the
            // soul-sand-and-skull pattern. The mob simulation reports consumed
            // cells; this caller clears them to air.
            if matches!(block, Block::WitherSkeletonSkull | Block::WitherSkeletonWallSkull) {
                let construction = mobs.with(|sim| {
                    sim.try_construct_wither(
                        &|x, y, z| source.block_state(x, y, z).to_owned(),
                        (target.x, target.y, target.z),
                    )
                });
                if let Some(construction) = construction {
                    for cell in &construction.consumed {
                        source.set_block(cell.x, cell.y, cell.z, "minecraft:air");
                        changed.push((*cell, "minecraft:air".to_string()));
                    }
                }
            }
            // Block placement notifies neighboring cells so redstone state can
            // react immediately. Without this fan-out, dust beside a powered
            // line stays at `power=0`.
            let (mut fanout, scheduled) = propagate_placement_with_entities(source, target, Some(block_entities));
            changed.append(&mut fanout);
            piston_records.extend(moving_piston_records(&scheduled));
            // Delayed reactions are returned through the scheduled-tick queue
            // owned by the world tick loop. Publish that queue even when the
            // synchronous fan-out changed no cells.
            block_ticks.request_scheduled_ticks(scheduled);
            // And the same seeding hook `destroy_block` performs, for the same
            // reason: a block placed into a flow, or beside a source, has to
            // start it re-evaluating. See `crate::fluid::ticks_after_edit`.
            block_ticks.request_scheduled_ticks(crate::fluid::ticks_after_edit(
                source,
                fluid_env_at(source, target),
                target,
            ));
            // Sand and gravel schedule a gravity check two ticks out. Other
            // placed blocks produce no entry in this feed.
            //
            // The scheduled event makes a sand or gravel block fall when it is
            // placed in air. `state` is used instead of the item name because
            // `gravity_tick::is_gravity_block` matches the block-state base.
            block_ticks
                .request_scheduled_ticks(crate::gravity_tick::ticks_after_place(target, &state));
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
        let placed_state = source.block_state(target.x, target.y, target.z);
        resend_column_for_light(conn, proto, source, state, &target_state, &placed_state, target)
            .await?;
    }
    Ok(())
}

/// Converts scheduled piston ticks into block-entity update payloads.
///
/// A `moving_piston` block update marks an animated cell; the payload from the
/// scheduled completion tick identifies the moving state. Send the payload
/// after the cell's `block_update` so the client has the matching cell record.
fn moving_piston_records(scheduled: &[ScheduledTick<String>]) -> Vec<(BlockPos, lodestone_core::Nbt)> {
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
) -> (Vec<(BlockPos, String)>, Vec<ScheduledTick<String>>)
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
) -> (Vec<(BlockPos, String)>, Vec<ScheduledTick<String>>)
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
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
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
    wire_state_before_removal: &str,
) -> (Vec<(BlockPos, String)>, Vec<ScheduledTick<String>>)
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
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
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
/// **The respawn position is the player's bed when it remains usable**, and the
/// world spawn otherwise. [`crate::world_spawn::resolve_bed_respawn`] re-reads
/// the bed cell at death time, so a broken or obstructed bed falls back to the
/// world spawn.
///
/// Respawn resets the modeled player vitals, sends the authoritative position,
/// and refreshes the health and air displays. A request from a living player is
/// ignored.
///
/// # `action == 2`, `REQUEST_GAMERULE_VALUES`
///
/// Action `2` returns the accepted rule entries when the permission level allows
/// it. Rules that have not been set are absent from the reply.
#[allow(clippy::too_many_arguments)]
async fn apply_client_command<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    vitals: &mut PlayerVitals,
    // The fall accumulator, reset whenever respawn changes the player's
    // position.
    fall: &mut FallTracker,
    teleport_acknowledgements: &mut Option<TeleportAcknowledgements>,
    // The world spawn resolved during the join sequence. It is the fallback
    // when no usable per-player bed position exists.
    //
    // The fallback for a missing or unusable per-player bed position.
    world_spawn: Vec3,
    // This player's bed point, if they have set one. Resolved against `source`
    // rather than used directly: see this function's own doc comment for why the
    // bed block is re-read at death time.
    respawn: Option<RespawnPoint>,
    // Read-only source for revalidating the bed position.
    source: &S,
    world: &crate::world_state::WorldStateHandle,
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
    action: i32,
    // Permission level for the rule-values request and mutation requests.
    permission_level: u8,
    // Whether the player died in a portal-traveled dimension. The caller uses
    // this flag to rebuild the dimension view after respawn.
    away_from_home: bool,
    // The readiness marker must be received again after a respawn before
    // movement-dependent simulation resumes.
    client_loaded: &mut bool,
    // Set to the resolved respawn position when a cross-dimension reset is
    // required; otherwise remains `None`.
    dimension_reset: &mut Option<Vec3>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    match action {
        0 if vitals.health() <= 0.0 => {
            vitals.respawn();
            *client_loaded = false;
            // Prefer a usable bed position and fall back to the world spawn when
            // the bed is broken or obstructed.
            let target = respawn
                .and_then(|point| crate::world_spawn::resolve_bed_respawn(source, point))
                .unwrap_or(world_spawn);
            // Send the respawn position before health and air so the client
            // refreshes the HUD for the updated player state.
            let teleport_id = issue_teleport_id(teleport_acknowledgements);
            for directive in proto.encode_respawn_with_teleport_id(teleport_id, target) {
                apply(conn, state, directive).await?;
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
            // The protocol respawn frame names the home dimension. If death
            // occurred in another dimension, ask the caller to rebuild the
            // dimension view so terrain follows the respawn position.
            if away_from_home {
                *dimension_reset = Some(target);
            }
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
    inventory.set_selected_hotbar_slot(slot);
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
    inventory.apply_menu_slot_change(i32::from(slot), item);
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
    let selected_bundle = |slot: usize| inventory.selected_bundle_item(slot);
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
                    entity.set_container_slot(*index, item.clone());
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
            mobs.with(|sim| sim.retrieve_fishing_bobber(bobber_id, Vec3::new(x, y, z), 0));
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
    let raw_damage = inventory.combat_stats().attack_damage;
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
fn swing_action(hand: u8) -> u8 {
    if hand == 1 { 3 } else { 0 }
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
/// The three world-derived facts [`FallSample`] needs, read off the terrain the
/// player is standing in.
///
/// # Which cell each one reads, and why
///
/// * `in_water` — the cell at the player's **feet**. The complete fluid-height
///   test covers the whole bounding box; the feet cell is the earliest
///   part of that box to touch a water surface on the way down, which is the
///   moment the cancellation must fire. Reading the *eye* instead (which
///   `crate::vitals` correctly does for drowning, a different question) would
///   delay the cancellation by the player's height and let a shallow-water landing
///   still hurt.
/// * `fall_resetting` — the same feet cell, since a climbable is something the
///   player is *inside*.
/// * `block_damage_modifier` — the cell **below** the feet, at `y - 0.2`, using
///   a `0.2` epsilon below the support boundary.
///   A plain `y - 1` is wrong for a player standing exactly on a block boundary.
///
/// One `ChunkSource::block_state` call per cell, two cells — and `block_state` is
/// the cheap single-cell read `ChunkStore` overrides, not a column regeneration
/// This runs once per movement packet, alongside `view.recenter`.
fn fall_sample<S: ChunkSource + ?Sized>(source: &S, x: f64, y: f64, z: f64, on_ground: bool) -> FallSample {
    let bx = x.floor() as i32;
    let bz = z.floor() as i32;
    let feet = source.block_state(bx, y.floor() as i32, bz);
    let below = source.block_state(bx, (y - 0.2).floor() as i32, bz);
    FallSample {
        y,
        on_ground,
        // `is_water`, deliberately — **not** `is_air_or_fluid`. Lava does not
        // cancel a fall in vanilla; see `crate::fall`'s module doc.
        in_water: is_water(&feet),
        fall_resetting: crate::fall::is_fall_damage_resetting(&feet),
        block_damage_modifier: crate::fall::block_damage_modifier(&below),
    }
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
    // Read the terrain at the player's feet for water, climbable, and landing
    // block facts. `.get()` performs two single-cell
    // reads, not a batch — see `SourceRef::get`.
    source: &S,
    player_pos: &Option<(f64, f64, f64)>,
    fall: &mut FallTracker,
    vitals: &mut PlayerVitals,
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
    if let Some(raw) = fall.on_player_moved(fall_sample(source, x, y, z, on_ground))
        && !invulnerable
        && vitals.apply_fall_damage(raw as f32).is_some()
    {
        publish_health(
            conn,
            state,
            proto,
            vitals,
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
    view_radius: i32,
    state: &mut State,
    view: &mut ViewTracker,
    // This connection's chunk-residency guard, so a chunk-boundary
    // crossing or a live view-radius change (the `recenter`/`set_view_radius`
    // arms below) can move the same `PLAYER_LOADING`/`PLAYER_SIMULATION`
    // tickets `serve_play` granted at join, rather than leaving them pinned to
    // the join column for the connection's whole lifetime.
    player_ticket_guard: &PlayerTicketGuard,
    pending_keep_alive: &mut Option<i64>,
    pending_break: &mut Option<PendingBreak>,
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
    awaiting_chunk_batch_ack: &mut bool,
    pending_chunk_batches: &mut VecDeque<Vec<ServerDirective>>,
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
    // parameter comment. Only the native `serve_play` (the one with `home` and
    // `pending_travel` in scope) reads this back; `wasm32`'s never leaves the
    // dimension it joined in, so `source` is never the `Dimension` arm there and
    // this is left `None` on every call, a pure no-op passthrough.
    dimension_reset: &mut Option<Vec3>,
    packet_id: i32,
    payload: &[u8],
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
{
    let packet = proto.decode(*state, packet_id, payload);
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

            // Publish the player's position, held item, account UUID, and view
            // direction to `MobSim`. This arm has all four values together,
            // while the mob tick loop consumes the snapshot on its next tick.
            // `set_players` replaces the whole list; the mob-enabled in-memory
            // world uses one connection, while a multi-player host must provide
            // additive registration.
            //
            // Position-driven, so a perfectly stationary player eventually
            // stops refreshing this. Harmless: the value is a position, not a
            // timer, so a stale entry for a motionless player is still the
            // correct answer. The same is true of `held_item` until they move
            // after a hotbar switch.
            // The account UUID identifies the mob owner, and the view
            // vector supplies the gaze used by perception checks. A missing
            // rotation uses yaw and pitch `0.0`.
            let facing = player_rot.unwrap_or_default();
            let (yaw_rad, pitch_rad) = (f64::from(facing.yaw).to_radians(), f64::from(facing.pitch).to_radians());
            let view_direction = Vec3::new(
                -yaw_rad.sin() * pitch_rad.cos(),
                -pitch_rad.sin(),
                yaw_rad.cos() * pitch_rad.cos(),
            );
            mobs.with(|sim| {
                sim.set_players(vec![PerceivedPlayer {
                    identity: Some(PlayerIdentity {
                        uuid: player_uuid,
                        entity_id: player_entity_id,
                    }),
                    perception: PlayerPerception {
                        position: Vec3::new(x, y, z),
                        held_item: inventory.selected_item().map(|stack| stack.item.clone()),
                        view_direction,
                    },
                }]);
            });

            // Chunk coordinate = floor(block / 16), not truncating division —
            // `-1.0_f64 / 16.0` must floor to chunk `-1`.
            let cx = (x / 16.0).floor() as i32;
            let cz = (z / 16.0).floor() as i32;
            // **This makes the world tick follow the player.** Publish a
            // 49-column square around the tracked chunk so natural spawning
            // and randomly ticking blocks follow the connection.
            //
            // The dimension is read off `source`, not assumed: a connection's
            // `SourceRef` switches to `SourceRef::Dimension` on portal travel, so
            // this is the one place that already knows which world the player is
            // standing in. Without it a player in the Nether would drag the
            // *overworld's* tick area to the matching overworld coordinates.
            //
            // Position-driven, like the `set_players` call above and with the same
            // consequence: a perfectly motionless player stops republishing, which
            // is harmless because the value is a position rather than a timer.
            world.tick_anchors().publish(vec![crate::tick_area::TickAnchor {
                dimension: source.dimension(),
                cx,
                cz,
            }]);
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
                player_ticket_guard.move_to(view.center, view.radius);
            }
            send_view_update(
                conn,
                proto,
                source,
                join_stream.as_deref_mut(),
                state,
                update,
                awaiting_chunk_batch_ack,
                pending_chunk_batches,
            )
            .await?;

            if *client_loaded && let Some(raw) =
                fall.on_player_moved(fall_sample(source.get(), x, y, z, on_ground))
                && !Abilities::for_mode(*game_mode).invulnerable
                && vitals.apply_fall_damage(raw as f32).is_some()
            {
                publish_health(
                    conn,
                    state,
                    proto,
                    vitals,
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
                // `.get()`: a break/place touches one block through
                // `block_state`/`set_block`, with no batch to offload — see
                // `SourceRef::get`.
                source.get(),
                state,
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
                // `.get()`: single-block read/write, nothing to offload.
                source.get(),
                state,
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
            abilities.flying = flying && abilities.may_fly;
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
            for stack in returning {
                if let (_, Some(leftover)) = inventory.add(stack) {
                    spilled.push(leftover);
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
                let current_state = source.get().block_state(pos.x, pos.y, pos.z);
                let facing = crate::command_block::facing(&current_state);
                let base = crate::command_block::base_name_for_mode_ordinal(mode);
                let new_state = crate::command_block::state_with(base, facing, conditional);
                if new_state != current_state {
                    source.get().set_block(pos.x, pos.y, pos.z, &new_state);
                    block_ticks.publish(pos.x, pos.y, pos.z, new_state.clone());
                }
                let new_mode = crate::command_block::mode_for_block(&new_state);
                // A conditional command block checks the block directly behind
                // its facing. Read that state before taking the registry lock.
                let predecessor_succeeded = conditional.then(|| {
                    let behind = facing.opposite().relative(pos);
                    let behind_state = source.get().block_state(behind.x, behind.y, behind.z);
                    crate::command_block::is_command_block_family(&behind_state)
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
            inventory.set_selected_bundle_item(slot_id, selected_item_index);
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
            // Drawn unconditionally, whether or not the click succeeds — the
            // same "one draw per attempt" reasoning `apply_use_item_on`'s own
            // composter roll already documents.
            let fresh_seed = i64::from(drops_rng.next_int(i32::MAX));
            let directives = apply_container_button_click(
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
            );
            // A no-op when the click was refused (`experience` untouched, so
            // this resends the same level/points it already holds) — cheaper
            // to call unconditionally than to thread a "did it actually spend
            // levels" flag out of `apply_container_button_click` just for this.
            republish_experience(players, player_uuid, experience);
            for directive in directives {
                apply(conn, state, directive).await?;
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
                && let Some(slot) = usize::try_from(slot_id).ok()
            {
                block_entities.with(|reg| {
                    if let Some(entity) = reg.get_mut(pos) {
                        entity.set_crafter_slot_state(slot, new_state);
                    }
                });
            }
        }
        ServerBound::Attack { entity_id } => {
            apply_attack(mobs, *player_pos, *sprinting, inventory, entity_id, player_uuid);
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
        ServerBound::UseItem { hand, yaw, pitch } => {
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
                    &|x, y, z| source.get().block_state(x, y, z),
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
            let outcome = apply_use_item(
                mobs,
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
                            &|x, y, z| terrain.block_state(x, y, z),
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
            // `SourceRef::Dimension` marks a connection currently viewing a
            // sibling dimension, so respawn handling can distinguish it from
            // the dimension used during the join.
            let away_from_home = matches!(source, SourceRef::Dimension(_));
            apply_client_command(
                conn,
                proto,
                state,
                vitals,
                fall,
                teleport_acknowledgements,
                world_spawn,
                *respawn,
                source.get(),
                world,
                advancements,
                player_uuid,
                action,
                commands.permission_level,
                away_from_home,
                client_loaded,
                dimension_reset,
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
                player_ticket_guard.move_to(view.center, view.radius);
            }
            send_view_update(
                conn,
                proto,
                source,
                join_stream.as_deref_mut(),
                state,
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
                for directive in next {
                    apply(conn, state, directive).await?;
                }
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
                                chunk_source.get().set_block(x, y, z, &block);
                                block_ticks.publish(x, y, z, block);
                            }
                            crate::commands::Effect::Fill { positions, block } => {
                                for (x, y, z) in positions {
                                    chunk_source.get().set_block(x, y, z, &block);
                                    block_ticks.publish(x, y, z, block.clone());
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
                                *respawn = Some(RespawnPoint { pos });
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
                    for line in outcome.response.lines() {
                        apply(conn, state, proto.encode_system_chat(line)).await?;
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
                    for line in response.lines() {
                        apply(conn, state, proto.encode_system_chat(line)).await?;
                    }
                }
            }
        }
        // A tab-completion request. See `ServerBound::CommandSuggestion`'s own
        // doc comment for the wire shape and
        // `crate::commands::ServerCommands::suggest_response` for the
        // start/length arithmetic and the `/`-stripping this delegates to it,
        // gated by `commands.permission_level` — the same resolved-once level
        // `ChatCommand` above uses.
        ServerBound::CommandSuggestion { id, command } => {
            let response =
                commands.builtins.suggest_response(id, &command, commands.permission_level);
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
                let block_state = source.get().block_state(pos.x, pos.y, pos.z);
                if let Some(stack) = crate::item_use::clone_item_stack_for_block(&block_state) {
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

/// A stall at or above this is logged the moment it is observed, naming the arm.
///
/// Four times the tick, so an ordinary busy pass is silent and a
/// hundreds-of-milliseconds one is not. There is no threshold at which a stall
/// stops mattering, which is why the *worst* one is reported unconditionally on
/// the timeout path regardless of this.
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
async fn serve_play<T, P, S, E>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    entities: &E,
    view_radius: i32,
    mut state: State,
    initial_teleport_id: Option<i32>,
    mut streamer: EntityStreamer,
    mut player_list: PlayerListStreamer,
    // Keep the ticket guard owned by the connection task. Its `Drop` performs
    // player deregistration on disconnect, error, and cancellation; borrowing
    // it could let the guard outlive the task that owns the connection.
    player_ticket: Option<PlayerTicket>,
    // The guard withdraws this connection's `PLAYER_LOADING` and
    // `PLAYER_SIMULATION` tickets when the task exits. Move it with each
    // tracked-view recenter or radius change so residency follows the player.
    player_ticket_guard: PlayerTicketGuard,
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
    let mut teleport_acknowledgements = initial_teleport_id.map(TeleportAcknowledgements::after_initial);
    let mut player_pos: Option<(f64, f64, f64)> = None;
    let mut client_movement = ClientMovement::default();
    let mut client_loaded = false;
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
    // Preserve fields `crate::player_data` does not model—hunger, experience,
    // the ender chest, and the recipe book—in every save. This keeps a full
    // load/modify/save cycle lossless; see `PlayerData::preserved`.
    let preserved_player_fields: Vec<(String, lodestone_core::Nbt)> =
        saved_player.as_ref().map(|d| d.preserved.clone()).unwrap_or_default();
    let mut vitals = saved_player
        .as_ref()
        .map_or_else(PlayerVitals::default, |data| {
            PlayerVitals::restored(data.health, data.air_supply)
        });
    let mut fall = FallTracker::default();
    let mut inventory = saved_player
        .as_ref()
        .map_or_else(PlayerInventory::default, crate::player_data::PlayerData::to_inventory);
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
        .map_or_else(crate::experience::PlayerExperience::default, |data| data.experience);
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
    let mut respawn: Option<RespawnPoint> = None;
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
    let mut pending_chunk_batches: VecDeque<Vec<ServerDirective>> = VecDeque::new();
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
    // This connection's read position in the shared arm-swing broadcast log —
    // same "start at the current end" reasoning as `chat_cursor`, so a
    // freshly joined connection is not replayed swings that happened before
    // it arrived.
    let mut swing_cursor = entities.players().map_or(0, PlayerRegistry::swing_cursor);
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

    // Portal travel state is per-connection and advances once per loop.
    let mut portal = crate::portal::PortalTracker::new();
    // The dimension the player has travelled to, if any. **Two variables, and that
    // is not redundancy**: `travelled` is borrowed by the shadowed `source` below for
    // the whole of one `select!`, so an arm that discovered a trip cannot write it.
    // `pending_travel` is where the arm parks the new source; the top of the next
    // iteration promotes it.
    let mut travelled: Option<Arc<dyn ChunkSource>> = None;
    // `Some(None)` is a pending trip *home*, `Some(Some(..))` a pending trip out, and
    // `None` no pending trip at all. One variable rather than a flag beside an
    // `Option`, so "no trip" and "a trip back to the overworld" cannot be confused —
    // they differ by one layer, and the return trip is the one that reads as success
    // in a screenshot when it silently does nothing.
    let mut pending_travel: Option<Option<Arc<dyn ChunkSource>>> = None;
    // Out-parameter for `dispatch_play_packet`'s `ClientCommand` arm — see
    // `apply_client_command`'s `dimension_reset` doc. Read and cleared
    // immediately after every `dispatch_play_packet` call in this loop.
    let mut dimension_reset: Option<Vec3> = None;

    loop {
        if let Some(next) = pending_travel.take() {
            travelled = next;
        }
        // Shadowing the `source` parameter is what makes a dimension change reach
        // every arm at once — the view stream, the block reads, the fall sampler and
        // the drowning probe all take `source`, and none of them has to know that
        // portals exist. `home` keeps the original, which is where a return trip
        // lands.
        let home = source;
        let source = match travelled.as_ref() {
            Some(other) => SourceRef::Dimension(other),
            None => home,
        };
        // Route live placement and delayed redstone/fluid requests through the
        // registry and feed belonging to the active dimension; see
        // `dimension_scoped_handles` for the independent handle fallbacks.
        let dimension_handles = dimension_scoped_handles(travelled.as_ref());
        let block_entities = dimension_handles
            .block_entities
            .as_ref()
            .unwrap_or(block_entities);
        let block_ticks = dimension_handles.block_ticks.as_ref().unwrap_or(block_ticks);
        tokio::select! {
            // Stream the deferred join view (`JOIN_PRESTREAM_RADIUS`) while this
            // loop services digs, damage, and container clicks.
            //
            // Disabled once drained, so this is not a branch that returns `None`
            // forever. `select!` polls its branches in a random order, so a ready
            // packet is never starved by a ready column.
            //
            // Both `JoinChunkStream::next` arms are cancel-safe; a canceled
            // column must not silently leave a hole in the client's terrain.
            chunk = join_stream.next(source), if !join_stream.is_done() => {
                watch.enter();
                let chunk = match chunk {
                    Ok(chunk) => chunk,
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
                if let Some(((cx, cz), payload)) = chunk {
                    if !join_batch_open {
                        apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                        join_batch_open = true;
                        join_batch_size = 0;
                    }
                    let directive = match encode_column(proto, source, cx, cz, payload) {
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
                    apply(conn, &mut state, directive).await?;
                    chunks_sent += 1;
                    join_batch_size += 1;
                    // Close on a full batch or on the last column, whichever comes
                    // first — the tail batch is short, exactly like vanilla's.
                    if join_batch_size as usize >= JOIN_STREAM_BATCH_COLUMNS
                        || join_stream.is_done()
                    {
                        apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                        join_batch_open = false;
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
                    );
                    return Ok(ServeSummary { username, chunks_sent, inventory });
                };
                dispatch_play_packet(
                    conn,
                    proto,
                    source,
                    view_radius,
                    &mut state,
                    &mut view,
                    &player_ticket_guard,
                    &mut pending_keep_alive,
                    &mut pending_break,
                    &mut teleport_acknowledgements,
                    &mut player_pos,
                    &mut client_movement,
                    &mut player_rot,
                    &mut fall,
                    &mut vitals,
                    world,
                    &mut inventory,
                    block_entities,
                    &mut open_container,
                    &mut open_merchant,
                    &mut container_sync,
                    &mut next_window_id,
                    mobs,
                    &mut sprinting,
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
                    packet_id,
                    &payload,
                )
                .await?;
                // A death respawn that just sent the player home from a portal
                // trip — see `apply_client_command`'s `dimension_reset` parameter
                // comment for why the client's own dimension label is not
                // enough. Mirrors `travel_through_portal`'s own tail: forget
                // every column this dimension's view believes is loaded,
                // recentre on the respawn position, rebuild the join stream, and
                // park the trip home for the next loop iteration to promote —
                // the same `pending_travel` a portal trip itself uses.
                if let Some(target) = dimension_reset.take() {
                    for &(cx, cz) in &view.loaded {
                        apply(conn, &mut state, proto.encode_forget_chunk(cx, cz)).await?;
                    }
                    let centre_cx = (target.x / 16.0).floor() as i32;
                    let centre_cz = (target.z / 16.0).floor() as i32;
                    apply(
                        conn,
                        &mut state,
                        proto.encode_chunk_cache_center(centre_cx, centre_cz),
                    )
                    .await?;
                    let radius = view.radius;
                    let max_radius = view.max_radius;
                    view = ViewTracker::new((centre_cx, centre_cz), radius, max_radius);
                    let rings: Vec<Vec<(i32, i32)>> = join_view_rings(radius)
                        .into_iter()
                        .map(|ring| {
                            ring.into_iter()
                                .map(|(dx, dz)| (centre_cx + dx, centre_cz + dz))
                                .collect()
                        })
                        .collect();
                    join_stream = crate::join_scheduler::JoinChunkStream::ringed(rings);
                    if join_batch_open {
                        apply(conn, &mut state, proto.end_chunk_batch(join_batch_size)).await?;
                        join_batch_open = false;
                        join_batch_size = 0;
                    }
                    pending_travel = Some(None);
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
                    pending_break = None;
                    let current = source.get().block_state(dig.pos.x, dig.pos.y, dig.pos.z);
                    if !crate::random_tick::is_air_variant(&current) {
                        destroy_block(
                            conn,
                            proto,
                            source.get(),
                            &mut state,
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
                                // No exclusion: vanilla's own entity play-sound routine passes
                                // `null`, so the eater hears it too — and *only*
                                // through this broadcast.
                                block_ticks.publish_effect(effect);
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
                                        match effect_id
                                            .strip_prefix("minecraft:")
                                            .unwrap_or(effect_id.as_str())
                                        {
                                            "instant_health" => vitals.heal(amount),
                                            "instant_damage" => {
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
                                        effects.apply(&effect_id, duration, amplifier);
                                        apply(
                                            conn,
                                            &mut state,
                                            proto.encode_update_mob_effect(
                                                LOCAL_PLAYER_ENTITY_ID,
                                                &effect_id,
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

                    let eye_state = source.get().block_state(
                        x.floor() as i32,
                        (y + EYE_HEIGHT).floor() as i32,
                        z.floor() as i32,
                    );
                    // `!invulnerable &&`: a creative player's air bar does not
                    // deplete and they never drown. Suppressed here rather than
                    // at the damage below because `PlayerVitals` is mode-free by
                    // design, and a depleting bar that can never hurt is worse
                    // than no bar at all.
                    let outcome = vitals.tick(!invulnerable && is_water(&eye_state));
                    if let Some(air) = outcome.air_changed {
                        apply(conn, &mut state, proto.encode_air_supply_update(air)).await?;
                    }
                    if outcome.damage.is_some() {
                        publish_health(
                            conn,
                            &mut state,
                            proto,
                            &vitals,
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
                    let feet = source.get().block_state(
                        x.floor() as i32,
                        y.floor() as i32,
                        z.floor() as i32,
                    );
                    let standing_in = crate::burning::BurnSource::for_block(&feet);
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
                        let levels = crate::beacon::beacon_levels(source.get(), pos.x, pos.y, pos.z);
                        if levels == 0
                            || !crate::beacon::beam_unobstructed(source.get(), pos.x, pos.y, pos.z, 384)
                        {
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
                        for (pos, state) in &death.exit_portal_blocks {
                            destination.set_block(pos.x, pos.y, pos.z, state);
                        }
                        if death.outcome.place_dragon_egg {
                            // Place the egg on the first solid surface above
                            // the portal. Scan down from `origin.y + 33` so a
                            // column that contains player-built blocks uses
                            // its actual top surface.
                            let mut egg_y = death.origin.y + 33;
                            while egg_y > death.origin.y
                                && destination.block_state(death.origin.x, egg_y, death.origin.z) == "minecraft:air"
                            {
                                egg_y -= 1;
                            }
                            destination.set_block(death.origin.x, egg_y + 1, death.origin.z, "minecraft:dragon_egg");
                        }
                        // `death.gateway_blocks` contains the positions from
                        // `outcome.spawn_gateway`'s formula and its shuffled
                        // 20-slice pool. The list is empty when spawning is
                        // disabled or the pool is exhausted. The visible
                        // structure has no teleport behavior here.
                        for (pos, state) in &death.gateway_blocks {
                            destination.set_block(pos.x, pos.y, pos.z, state);
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
                if !effects.is_empty() {
                    let out = effects.tick(
                        i32::try_from(world.time().game_time.max(0)).unwrap_or(i32::MAX),
                        vitals.health(),
                        crate::vitals::MAX_HEALTH,
                    );
                    if out.exhaustion > 0.0 {
                        vitals.add_exhaustion(out.exhaustion);
                    }
                    // Poison's `health > 1.0` guard is already applied inside the
                    // registry, so this is an unconditional subtraction of an amount
                    // that was only produced when the guard allowed it.
                    let mut moved = false;
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

                // Portal travel runs last, after this tick's damage and hunger
                // updates have been applied.
                //
                // Feed the portal counter with the block at the player's feet.
                // A standing player occupies that cell even when the portal is
                // three blocks tall; using the eye cell would miss the bottom row.
                if let Some((x, y, z)) = player_pos {
                    let feet = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
                    let feet_state = source.get().block_state(feet.x, feet.y, feet.z);
                    // End and Nether portals share one counter, so a player
                    // cannot accumulate two transitions simultaneously.
                    let in_end_portal = crate::portal::is_end_portal(&feet_state);
                    let standing_in =
                        (in_end_portal || crate::portal::is_portal(&feet_state)).then_some(feet);
                    // End portals transition on the first tick inside. Nether
                    // transitions use the creative or default delay from the
                    // shared rules, which is read every tick so rule changes
                    // take effect without reconnecting.
                    let rules = world.rules();
                    let transition = if in_end_portal {
                        0
                    } else if Abilities::for_mode(game_mode).invulnerable {
                        rules.players_nether_portal_creative_delay()
                    } else {
                        rules.players_nether_portal_default_delay()
                    }
                    .max(0);
                    if let Some(entry) = portal.tick(standing_in, transition) {
                        let entry_state = source.get().block_state(entry.x, entry.y, entry.z);
                        let trip = if crate::portal::is_end_portal(&entry_state) {
                            // There is no destination for an End portal that
                            // is already inside the End, so leave that case
                            // inert instead of selecting an invalid target.
                            if source.dimension() == crate::dimension::Dimension::End {
                                None
                            } else {
                                travel_through_end_portal(
                                    conn,
                                    proto,
                                    home,
                                    &mut state,
                                    &mut view,
                                    &mut join_stream,
                                    &mut teleport_acknowledgements,
                                    game_mode,
                                    mobs,
                                )
                                .await?
                            }
                        } else if rules.allow_entering_nether_using_portals()
                            || source.dimension() == crate::dimension::Dimension::Nether
                        {
                            // Check the Nether travel rule at the transition
                            // point. The counter continues while travel is
                            // disabled, so re-enabling the rule permits an
                            // already-qualified player to travel immediately.
                            travel_through_portal(
                                conn,
                                proto,
                                home,
                                source,
                                &mut state,
                                &mut view,
                                &mut join_stream,
                                &mut teleport_acknowledgements,
                                entry,
                                (x, y, z),
                                game_mode,
                            )
                            .await?
                        } else {
                            None
                        };
                        if let Some(trip) = trip {
                            player_pos = Some((
                                trip.position.x,
                                trip.position.y,
                                trip.position.z,
                            ));
                            portal.begin_cooldown();
                            pending_travel = Some(trip.source);
                            // The deferred join stream uses a fresh batch, so close
                            // any batch left open by the outgoing dimension.
                            if join_batch_open {
                                apply(
                                    conn,
                                    &mut state,
                                    proto.end_chunk_batch(join_batch_size),
                                )
                                .await?;
                                join_batch_open = false;
                                join_batch_size = 0;
                            }
                        }
                    }
                }
                watch.pass("vitals_tick");
            }

            _ = container_sync_tick.tick() => {
                watch.enter();
                // The piece with no inbound packet driving it at all: the
                // server's unified tick loop (`crate::tick::run_tick_loop`,
                // mutates the registry independently of any
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
                // World random ticks (for example grass-to-dirt changes) mutate
                // the shared `ChunkSource` independently of this connection.
                // Drain their block updates on this timer; the feed has one
                // consumer for each `open_in_memory_with_mobs` world.
                // Include light for each changed column. `encode_block_update`
                // carries no light, so a tick-driven change must be followed by
                // a column-light resend. For example, a fluid tick can remove an
                // underwater torch after placement; this drain updates both the
                // block and its light. Fire, grass, crops, redstone torches, and
                // landing falling blocks use this update flow.
                //
                // Deduplicated by column, and that is what makes it affordable: a
                // fluid cascade rewrites many cells in one column in a single tick,
                // and each relight is a whole-column flood. `send_lighting_for_edit`
                // is used because the feed carries only
                // the replacement state; without a comparison baseline, a
                // predicate cannot gate the resend.
                let mut relight: Vec<(i32, i32)> = Vec::new();
                for (x, y, z, block_state) in block_ticks.drain_all() {
                    apply(conn, &mut state, proto.encode_block_update(x, y, z, &block_state)).await?;
                    let column = (x.div_euclid(16), z.div_euclid(16));
                    if !relight.contains(&column) {
                        relight.push(column);
                    }
                }
                // `source.get()`, like every other non-batch read here: one
                // column at a time has nothing to offload, and it is the same
                // accessor `resend_column_for_light`'s callers already use.
                for (cx, cz) in relight {
                    send_lighting_for_edit(conn, proto, source.get(), &mut state, cx, cz).await?;
                }
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
/// - **Portal travel.** This loop does not mutate the connection's dimension
///   during a portal trip.
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
                    block_ticks.publish_effect(effect);
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
                            match effect_id.strip_prefix("minecraft:").unwrap_or(effect_id.as_str()) {
                                "instant_health" => vitals.heal(amount),
                                "instant_damage" => vitals.apply_effect_damage(amount),
                                _ => continue,
                            }
                            health_changed = true;
                        }
                        crate::mob_effects::SplashEffect::Timed {
                            effect_id,
                            duration,
                            amplifier,
                        } => {
                            effects.apply(&effect_id, duration, amplifier);
                            apply(
                                conn,
                                state,
                                proto.encode_update_mob_effect(
                                    LOCAL_PLAYER_ENTITY_ID,
                                    &effect_id,
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

        let eye_state = source.get().block_state(
            x.floor() as i32,
            (y + EYE_HEIGHT).floor() as i32,
            z.floor() as i32,
        );
        // `!invulnerable &&` keeps creative players from depleting air or
        // drowning.
        let outcome = vitals.tick(!invulnerable && is_water(&eye_state));
        if let Some(air) = outcome.air_changed {
            apply(conn, state, proto.encode_air_supply_update(air)).await?;
        }
        if outcome.damage.is_some() {
            publish_health(
                conn,
                state,
                proto,
                vitals,
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

    // Burning reads the block at the player's feet, updates burn state, and
    // publishes health when damage applies.
    if let Some((x, y, z)) = player_pos {
        let feet = source.get().block_state(x.floor() as i32, y.floor() as i32, z.floor() as i32);
        let standing_in = crate::burning::BurnSource::for_block(&feet);
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
            let levels = crate::beacon::beacon_levels(source.get(), pos.x, pos.y, pos.z);
            if levels == 0 || !crate::beacon::beam_unobstructed(source.get(), pos.x, pos.y, pos.z, 384) {
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
    if !effects.is_empty() {
        let out = effects.tick(
            i32::try_from(world.time().game_time.max(0)).unwrap_or(i32::MAX),
            vitals.health(),
            crate::vitals::MAX_HEALTH,
        );
        if out.exhaustion > 0.0 {
            vitals.add_exhaustion(out.exhaustion);
        }
        let mut moved = false;
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
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    entities: &E,
    view_radius: i32,
    mut state: State,
    initial_teleport_id: Option<i32>,
    mut streamer: EntityStreamer,
    mut player_list: PlayerListStreamer,
    // Keep the ticket guard alive for the entire connection. Player streaming
    // is packet-driven through `FallTracker` and needs no timer here.
    player_ticket: Option<PlayerTicket>,
    // The guard withdraws this connection's `PLAYER_LOADING` and
    // `PLAYER_SIMULATION` tickets when the task exits. Move it with each
    // tracked-view recenter or radius change so residency follows the player.
    player_ticket_guard: PlayerTicketGuard,
    mut view: ViewTracker,
    username: String,
    // World spawn for death-screen respawn. The join computation may inspect up
    // to 121 columns, so `apply_client_command` reuses this value when no
    // usable per-player bed point exists.
    world_spawn: Vec3,
    mut chunks_sent: usize,
    // The browser loop drains the finite join stream inline before packet
    // dispatch. It has no second thread, so generation occupies this loop until
    // the initial burst completes.
    mut join_stream: crate::join_scheduler::JoinChunkStream<S>,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    // Inbound placement packets publish neighbour-update requests here.
    // Outbound random-tick changes require a container-sync timer, which this
    // browser loop does not provide.
    block_ticks: &BlockTickFeed,
    // No packet produces explosion-feed entries on the browser target.
    _explosions: &ExplosionFeed,
    // Weather changes are world-tick events; this browser loop has no timer
    // producer that drains the weather feed.
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
    let mut teleport_acknowledgements = initial_teleport_id.map(TeleportAcknowledgements::after_initial);
    let mut sprinting = false;
    let mut bow_draw: Option<BowDraw> = None;
    // `wasm_vitals_tick` applies item-use completion rules from its browser
    // timer, so a bite started here reaches its completion result.
    let mut item_in_use: Option<ItemInUse> = None;
    // `player_pos` feeds movement and vital checks. The browser timer applies
    // drowning, border damage, burning, status effects, and hunger; fall damage
    // remains driven by inbound `PlayerMoved` packets.
    let mut player_pos: Option<(f64, f64, f64)> = None;
    let mut client_movement = ClientMovement::default();
    let mut client_loaded = false;
    let mut abilities = Abilities::for_mode(game_mode);
    // The rotation is stored alongside `player_pos` — see `dispatch_play_packet`'s own
    // parameter comment.
    let mut player_rot: Option<Rotation> = None;
    let mut vitals = PlayerVitals::default();
    let mut fall = FallTracker::default();
    let mut inventory = PlayerInventory::default();
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
    // The night-skip vote uses the player's roster key. Browser packet handlers
    // can register and wake voters; timer-fed vote counts remain native-only.
    let player_entity_id =
        player_ticket.as_ref().map_or(LOCAL_PLAYER_ENTITY_ID, |t| t.entity_id());
    // The initial join dump is unacknowledged, so this gate begins `true`.
    let mut awaiting_chunk_batch_ack = true;
    let mut pending_chunk_batches: VecDeque<Vec<ServerDirective>> = VecDeque::new();
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

    // Drain the deferred join view inline; this loop has no separate worker
    // branch for streaming it concurrently.
    if !join_stream.is_done() {
        apply(conn, &mut state, proto.begin_chunk_batch()).await?;
        let mut batch_size: i32 = 0;
        loop {
            let next = match join_stream.next(source).await {
                Ok(next) => next,
                Err(error) => {
                    return return_chunk_encode_error(
                        conn,
                        proto,
                        &mut state,
                        Some(batch_size),
                        error,
                    )
                    .await;
                }
            };
            let Some(((cx, cz), payload)) = next else {
                break;
            };
            let directive = match encode_column(proto, source, cx, cz, payload) {
                Ok(directive) => directive,
                Err(error) => {
                    return return_chunk_encode_error(
                        conn,
                        proto,
                        &mut state,
                        Some(batch_size),
                        error,
                    )
                    .await;
                }
            };
            apply(conn, &mut state, directive).await?;
            chunks_sent += 1;
            batch_size += 1;
        }
        apply(conn, &mut state, proto.end_chunk_batch(batch_size)).await?;
    }

    // The browser timer uses a macrotask via `window.setTimeout`; it drives
    // `wasm_vitals_tick` once per `WASM_VITALS_TICK_INTERVAL` period.
    let mut vitals_interval =
        crate::browser_timer::BrowserInterval::new(WASM_VITALS_TICK_INTERVAL);
    loop {
        let (packet_id, payload) = tokio::select! {
            packet = conn.read_packet() => {
                match packet? {
                    Some(p) => p,
                    // Clean disconnect. Browser connections have no
                    // filesystem-backed player store to persist here.
                    None => return Ok(ServeSummary { username, chunks_sent, inventory }),
                }
            }
            // Cancel-safe timer polling: a ready packet leaves the interval's
            // deadline untouched, so no timer event is lost.
            _ = vitals_interval.tick() => {
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
                continue;
            }
        };
        dispatch_play_packet(
            conn,
            proto,
            source,
            view_radius,
            &mut state,
            &mut view,
            &player_ticket_guard,
            &mut pending_keep_alive,
            &mut pending_break,
            &mut teleport_acknowledgements,
            &mut player_pos,
            &mut client_movement,
            &mut player_rot,
            &mut fall,
            &mut vitals,
            world,
            &mut inventory,
            block_entities,
            &mut open_container,
            &mut open_merchant,
            &mut container_sync,
            &mut next_window_id,
            mobs,
            &mut sprinting,
            &mut awaiting_chunk_batch_ack,
            &mut pending_chunk_batches,
            // `None`: this loop drains the join stream inline, so no deferred
            // stream is available to `dispatch_play_packet`.
            None,
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
            // This target has no portal-travel state, so `source` never uses
            // `SourceRef::Dimension` and this out-parameter remains unused.
            &mut None,
            packet_id,
            &payload,
        )
        .await?;
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
            if let Some(absorbed) =
                collect_nearby_orbs(mobs, Vec3::new(x, y, z), &mut experience, &mut take_xp_delay)
            {
                apply(
                    conn,
                    &mut state,
                    proto.encode_take_item_entity(absorbed.orb_entity_id, LOCAL_PLAYER_ENTITY_ID, 1),
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

    #[test]
    fn client_tick_boundary_clears_stale_launch_momentum() {
        let launch = Vec3::new(4.0, 5.0, 6.0);
        let mut movement = ClientMovement::default();
        movement.observe(Vec3::new(1.0, 2.0, 3.0), false);

        assert_eq!(
            movement.add_to_launch(launch),
            Vec3::new(5.0, 7.0, 9.0),
            "an airborne launch inherits this tick's complete movement sample"
        );

        movement.finish_tick();
        assert_eq!(
            movement.add_to_launch(launch),
            Vec3::new(5.0, 7.0, 9.0),
            "the boundary retains movement reported during the tick it closes"
        );

        movement.finish_tick();
        assert_eq!(
            movement.add_to_launch(launch),
            launch,
            "a following tick with no movement must clear stale launch momentum"
        );

        movement.observe(Vec3::new(1.0, 2.0, 3.0), true);
        assert_eq!(
            movement.add_to_launch(launch),
            Vec3::new(5.0, 5.0, 9.0),
            "a grounded launch inherits horizontal movement but not vertical movement"
        );
    }

    #[test]
    fn recipe_book_seen_accepts_only_advertised_display_ids() {
        let mut inventory = PlayerInventory::new();
        let valid = crate::crafting::recipe_book_entries()
            .first()
            .expect("the bundled recipe book has an entry")
            .id;
        assert!(inventory.recipe_book_entry_is_highlighted(valid));
        assert!(
            recipe_book_snapshot(&inventory)
                .into_iter()
                .find(|entry| entry.id == valid)
                .is_some_and(|entry| entry.highlight),
            "the join snapshot must expose an unacknowledged entry as highlighted"
        );

        let update = record_recipe_book_seen(&mut inventory, valid)
            .expect("an advertised display id must fold into a client update");
        assert!(
            !inventory.recipe_book_entry_is_highlighted(valid),
            "the validated packet must fold into the connection state"
        );
        assert!(
            !update.highlight,
            "the response must expose the cleared flag to the client read-model"
        );
        assert!(
            recipe_book_snapshot(&inventory)
                .into_iter()
                .find(|entry| entry.id == valid)
                .is_some_and(|entry| !entry.highlight),
            "the next snapshot must expose the acknowledgement to the client"
        );

        assert!(record_recipe_book_seen(&mut inventory, i32::MAX).is_none());
        assert!(
            inventory.recipe_book_entry_is_highlighted(i32::MAX),
            "an id absent from the advertised corpus must not manufacture seen state"
        );
    }

    struct RefusingChunkProtocol;

    impl ServerProtocol for RefusingChunkProtocol {
        fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
            unreachable!("these tests only write server directives")
        }

        fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
            unreachable!("these tests only write server directives")
        }

        fn begin_configuration(&self) -> Vec<ServerDirective> {
            unreachable!("these tests only write server directives")
        }

        fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
            unreachable!("these tests only write server directives")
        }

        fn begin_chunk_batch(&self) -> ServerDirective {
            ServerDirective::Send {
                packet_id: 40,
                payload: Vec::new(),
            }
        }

        fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
            unreachable!("the checked encoder must be used")
        }

        fn try_encode_chunk(
            &self,
            _cx: i32,
            _cz: i32,
            _column: &ChunkColumn,
        ) -> Result<ServerDirective, ChunkEncodeError> {
            Err(ChunkEncodeError::new("fixture rejected chunk"))
        }

        fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
            ServerDirective::Send {
                packet_id: 41,
                payload: vec![batch_size as u8],
            }
        }

        fn encode_disconnect(&self, _state: State, _reason: &Text) -> ServerDirective {
            ServerDirective::Send {
                packet_id: 42,
                payload: Vec::new(),
            }
        }
    }

    struct OneColumnSource;

    impl ChunkSource for OneColumnSource {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 256)
        }

        fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:air".to_owned()
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_owned()
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
    }

    #[tokio::test]
    async fn a_local_failed_view_batch_writes_no_batch_markers() {
        let (client_end, server_end) = lodestone_net::memory_pair();
        let mut conn = Connection::new(server_end);
        let mut state = State::Play;
        let source = OneColumnSource;
        let mut awaiting_ack = false;
        let mut pending = VecDeque::new();

        let error = send_view_update(
            &mut conn,
            &RefusingChunkProtocol,
            SourceRef::Borrowed(&source),
            None,
            &mut state,
            ViewUpdate {
                immediate: Vec::new(),
                forgotten: HashSet::new(),
                added: vec![(0, 0)],
            },
            &mut awaiting_ack,
            &mut pending,
        )
        .await
        .expect_err("a rejecting protocol must fail the view update");
        assert!(matches!(error, ServerError::ChunkEncode(_)));

        let mut peer = Connection::new(client_end);
        assert_eq!(
            peer.read_packet().await.expect("disconnect frame decodes"),
            Some((42, Vec::new())),
            "the locally accumulated batch must not write a start or end marker"
        );
    }

    #[tokio::test]
    async fn a_written_chunk_batch_ends_before_its_encoding_disconnect() {
        let (client_end, server_end) = lodestone_net::memory_pair();
        let mut conn = Connection::new(server_end);
        let mut state = State::Play;
        let source = OneColumnSource;

        let error = send_column_light(
            &mut conn,
            &RefusingChunkProtocol,
            &source,
            &mut state,
            0,
            0,
        )
        .await
        .expect_err("a rejecting protocol must fail the column resend");
        assert!(matches!(error, ServerError::ChunkEncode(_)));

        let mut peer = Connection::new(client_end);
        let frames = [
            peer.read_packet().await.expect("batch start decodes"),
            peer.read_packet().await.expect("batch end decodes"),
            peer.read_packet().await.expect("disconnect decodes"),
        ];
        assert_eq!(
            frames,
            [Some((40, Vec::new())), Some((41, vec![0])), Some((42, Vec::new()))],
            "an already-written batch must end before the encoding disconnect"
        );
    }

    /// An empty hand must resolve to the player's canonical attribute base,
    /// while the equipment fold can move off that base.
    ///
    /// The control is that the equipment fold *can* move off the base — a diamond
    /// sword resolves to `7.0` — so the equality below is not comparing a value
    /// against a constant that nothing else could change.
    #[test]
    fn bare_hand_damage_is_the_player_attribute_base() {
        let empty = PlayerInventory::new();
        let bare = empty.combat_stats().attack_damage;
        assert!(
            (f64::from(bare) - lodestone_entity::equipment::PLAYER_BASE_ATTACK_DAMAGE).abs()
                < 1e-9,
            "an empty hand must resolve to the player's attribute base, got {bare}"
        );

        let mut armed = PlayerInventory::new();
        armed.set_native(0, Some(ItemStack::new(item_key("diamond_sword"), 1)));
        let with_sword = armed.combat_stats().attack_damage;
        assert!(
            (with_sword - 7.0).abs() < 1e-6,
            "control: a real weapon must move the number off the base, got {with_sword}"
        );
    }

    /// A held sword only counts from the **selected** hotbar slot. The wrong
    /// implementation — reading native slot `0` — passes the test above and fails
    /// this one, which is the reason this is a second case rather than an extra
    /// assertion.
    #[test]
    fn only_the_selected_hotbar_slot_arms_the_player() {
        let mut inv = PlayerInventory::new();
        inv.set_native(3, Some(ItemStack::new(item_key("diamond_sword"), 1)));
        assert!(
            (f64::from(inv.combat_stats().attack_damage)
                - lodestone_entity::equipment::PLAYER_BASE_ATTACK_DAMAGE)
                .abs()
                < 1e-9,
            "a sword in an unselected slot is not in the main hand"
        );
        assert!(inv.set_selected_hotbar_slot(3));
        assert!(
            (inv.combat_stats().attack_damage - 7.0).abs() < 1e-6,
            "selecting the slot holding it must arm the player"
        );
    }

    /// Worn armour reaches [`PlayerVitals::apply_damage`]'s reduction, and the
    /// value is the one a real vanilla 26.2 server produced for the same set and
    /// the same raw hit: `10.0` of `minecraft:mob_attack` against full diamond
    /// measures **3.0**, where an unarmoured player takes the whole `10.0`.
    #[test]
    fn worn_armour_reduces_an_incoming_hit_to_the_live_verified_value() {
        let mut inv = PlayerInventory::new();
        for (native, item) in [
            (crate::inventory::HEAD_NATIVE, "diamond_helmet"),
            (crate::inventory::CHEST_NATIVE, "diamond_chestplate"),
            (crate::inventory::LEGS_NATIVE, "diamond_leggings"),
            (crate::inventory::FEET_NATIVE, "diamond_boots"),
        ] {
            inv.set_native(native, Some(ItemStack::new(item_key(item), 1)));
        }
        let flags = lodestone_entity::DamageFlags::for_damage_type_name("mob_attack")
            .expect("mob_attack is a real damage type");

        let mut armoured = PlayerVitals::default();
        let dealt = armoured
            .apply_damage(10.0, &inv.combat_stats().defenses, flags)
            .expect("the hit lands");
        assert!((dealt - 3.0).abs() < 1e-3, "armoured hit dealt {dealt}");

        let mut bare_player = PlayerVitals::default();
        let bare_dealt = bare_player
            .apply_damage(10.0, &PlayerInventory::new().combat_stats().defenses, flags)
            .expect("the hit lands");
        assert!(
            (bare_dealt - 10.0).abs() < 1e-3,
            "control: an unarmoured player takes the full hit, got {bare_dealt}"
        );
    }

    /// A parsed `minecraft:` item key for the tests above.
    fn item_key(path: &str) -> lodestone_model::ResourceKey {
        lodestone_model::ResourceKey::new("minecraft", path).expect("a static item key parses")
    }

    /// [`apply_edit_book`]'s draft-save path: a writable book in a hotbar
    /// slot gets its pages overwritten in place, no transmute.
    #[test]
    fn edit_book_draft_save_updates_pages_without_transmuting() {
        let mut inv = PlayerInventory::new();
        inv.set_native(0, Some(ItemStack::new(item_key("writable_book"), 1)));
        let result = apply_edit_book(
            &mut inv,
            0,
            vec!["Page one".to_owned()],
            None,
            "Steve",
        );
        let (native, item) = result.expect("a writable book in a hotbar slot must be editable");
        assert_eq!(native, 0);
        assert_eq!(item.item, item_key("writable_book"));
        assert_eq!(
            item.components.writable_book_content,
            Some(vec!["Page one".to_owned()])
        );
        assert_eq!(inv.native(0), Some(&item));
    }

    /// The signing path: a title present transmutes the stack to
    /// `minecraft:written_book` and stamps the signer's name as author —
    /// vanilla's own sign-book handler's own literal `0`/`true` for
    /// generation/resolved.
    #[test]
    fn edit_book_signing_transmutes_to_written_book() {
        let mut inv = PlayerInventory::new();
        inv.set_native(
            crate::inventory::OFFHAND_NATIVE,
            Some(ItemStack::new(item_key("writable_book"), 1)),
        );
        let (native, item) = apply_edit_book(
            &mut inv,
            i32::try_from(crate::inventory::OFFHAND_NATIVE).unwrap(),
            vec!["Once upon a time".to_owned(), "The End".to_owned()],
            Some("My Book".to_owned()),
            "Alex",
        )
        .expect("a writable book in the off-hand must be signable");
        assert_eq!(native, crate::inventory::OFFHAND_NATIVE);
        assert_eq!(item.item, item_key("written_book"));
        assert_eq!(item.components.writable_book_content, None);
        let content = item
            .components
            .written_book_content
            .expect("signing must set written_book_content");
        assert_eq!(content.title, "My Book");
        assert_eq!(content.author, "Alex");
        assert_eq!(content.generation, 0);
        assert!(content.resolved);
        assert_eq!(content.pages.len(), 2);
    }

    /// **Control**: a slot outside the hotbar or off-hand must be refused —
    /// the `hotbar || off-hand` slot gate (`slot == 40` is the off-hand).
    /// Without this, an implementation that skipped the slot check entirely
    /// would still pass the two tests above (both use in-range slots).
    #[test]
    fn edit_book_refuses_a_main_storage_slot() {
        let mut inv = PlayerInventory::new();
        inv.set_native(9, Some(ItemStack::new(item_key("writable_book"), 1)));
        assert_eq!(
            apply_edit_book(&mut inv, 9, vec!["x".to_owned()], None, "Steve"),
            None
        );
    }

    /// **Control**: an item that is not a writable book must be refused —
    /// vanilla's `carried.has(DataComponents.WRITABLE_BOOK_CONTENT)` gate.
    /// Without this, any item in the targeted slot would silently gain book
    /// content.
    #[test]
    fn edit_book_refuses_a_non_book_item() {
        let mut inv = PlayerInventory::new();
        inv.set_native(0, Some(ItemStack::new(item_key("stone"), 1)));
        assert_eq!(
            apply_edit_book(&mut inv, 0, vec!["x".to_owned()], None, "Steve"),
            None
        );
    }

    /// The packet-shaping function itself, not the equipment maths
    /// [`worn_armour_reduces_an_incoming_hit_to_the_live_verified_value`]
    /// already covers: a full diamond set's folded `minecraft:armor` and
    /// `minecraft:armor_toughness` must reach [`player_attribute_snapshots`]'s
    /// output as a bare `base` with **no** modifiers (see that function's own
    /// doc for why empty modifiers are correct, not merely simpler — the
    /// client's fold is a no-op over a bare base). This is a magnitude check
    /// against the same 20.0/8.0 pair the live server produced for the
    /// identical set, not a "some armour value exists" check.
    #[test]
    fn player_attribute_snapshots_carries_the_folded_armor_with_no_modifiers() {
        let mut inv = PlayerInventory::new();
        for (native, item) in [
            (crate::inventory::HEAD_NATIVE, "diamond_helmet"),
            (crate::inventory::CHEST_NATIVE, "diamond_chestplate"),
            (crate::inventory::LEGS_NATIVE, "diamond_leggings"),
            (crate::inventory::FEET_NATIVE, "diamond_boots"),
        ] {
            inv.set_native(native, Some(ItemStack::new(item_key(item), 1)));
        }
        let snapshots = player_attribute_snapshots(&inv);
        let armor = snapshots
            .iter()
            .find(|s| s.attribute.to_string() == "minecraft:armor")
            .expect("a fully-armoured player must publish minecraft:armor");
        assert!((armor.base - 20.0).abs() < 1e-6, "armor {}", armor.base);
        assert!(
            armor.modifiers.is_empty(),
            "the wire snapshot must fold equipment into base, not re-publish per-item modifiers"
        );
        let toughness = snapshots
            .iter()
            .find(|s| s.attribute.to_string() == "minecraft:armor_toughness")
            .expect("a full diamond set must publish armor_toughness");
        assert!((toughness.base - 8.0).abs() < 1e-6, "toughness {}", toughness.base);

        // Control: an unarmoured player still publishes `minecraft:armor`
        // explicitly, at `0.0` — **not** an absent entry. This verifies that
        // removing the last piece resets the HUD value rather than leaving
        // its last non-zero reading, and the reason
        // `player_attribute_snapshots` reads named attributes through
        // `AttributeMap::value` rather than iterating the sparse map: an
        // omitted attribute is "unchanged" to the client's merge
        // (`lodestone_ecs::ingest::apply_entity_attributes`), not "reset".
        let bare = player_attribute_snapshots(&PlayerInventory::new());
        let bare_armor = bare
            .iter()
            .find(|s| s.attribute.to_string() == "minecraft:armor")
            .expect("an unarmoured player must still publish minecraft:armor, explicitly");
        assert!(
            bare_armor.base.abs() < 1e-6,
            "an unarmoured player's armor must be exactly 0.0, got {}",
            bare_armor.base
        );
    }

    /// **The removal sequence, reproduced directly.** Equip a helmet and a
    /// chestplate (distinct per-piece values — `3.0` and `8.0` — so a
    /// transposition or an off-by-one cannot hide), remove them one at a
    /// time, and assert the *sequence* of published armour values: `11.0`
    /// (both), `8.0` (helmet off), then `0.0` (chestplate off too). Collected
    /// into one list and asserted together, so a failure identifies the
    /// published value at the affected removal point.
    #[test]
    fn removing_the_last_piece_of_armor_publishes_an_explicit_zero() {
        let mut inv = PlayerInventory::new();
        inv.set_native(
            crate::inventory::HEAD_NATIVE,
            Some(ItemStack::new(item_key("diamond_helmet"), 1)),
        );
        inv.set_native(
            crate::inventory::CHEST_NATIVE,
            Some(ItemStack::new(item_key("diamond_chestplate"), 1)),
        );

        fn armor_of(inv: &PlayerInventory) -> Option<f64> {
            player_attribute_snapshots(inv)
                .into_iter()
                .find(|s| s.attribute.to_string() == "minecraft:armor")
                .map(|s| s.base)
        }
        fn check(mismatches: &mut Vec<String>, inv: &PlayerInventory, label: &str, expected: f64) {
            let Some(got) = armor_of(inv) else {
                mismatches.push(format!("{label}: minecraft:armor was not published at all"));
                return;
            };
            if (got - expected).abs() > 1e-6 {
                mismatches.push(format!("{label}: expected {expected}, got {got}"));
            }
        }

        let mut mismatches = Vec::new();
        check(&mut mismatches, &inv, "both pieces worn", 11.0);
        inv.set_native(crate::inventory::HEAD_NATIVE, None);
        check(&mut mismatches, &inv, "helmet removed, chestplate still worn", 8.0);
        inv.set_native(crate::inventory::CHEST_NATIVE, None);
        check(&mut mismatches, &inv, "last piece removed", 0.0);

        assert!(
            mismatches.is_empty(),
            "armour publication went stale: {mismatches:?}"
        );
    }

    /// **Control for the explicit zero.** Iterates only attributes present in
    /// the sparse map and verifies that the final removal omits
    /// `minecraft:armor` instead of publishing `0.0`. The assertion requires
    /// the explicit armor entry, so omitting the final publication fails.
    #[test]
    fn the_sparse_iteration_bug_is_caught_by_the_removal_sequence_above() {
        fn buggy_snapshots(inventory: &PlayerInventory) -> Vec<EntityAttributeSnapshot> {
            inventory
                .combat_stats()
                .attributes
                .iter()
                .map(|(id, instance)| EntityAttributeSnapshot {
                    attribute: id.clone(),
                    base: instance.value(),
                    modifiers: Vec::new(),
                })
                .collect()
        }

        let mut inv = PlayerInventory::new();
        inv.set_native(
            crate::inventory::HEAD_NATIVE,
            Some(ItemStack::new(item_key("diamond_helmet"), 1)),
        );
        inv.set_native(
            crate::inventory::CHEST_NATIVE,
            Some(ItemStack::new(item_key("diamond_chestplate"), 1)),
        );
        inv.set_native(crate::inventory::HEAD_NATIVE, None);
        inv.set_native(crate::inventory::CHEST_NATIVE, None);

        let armor = buggy_snapshots(&inv)
            .into_iter()
            .find(|s| s.attribute.to_string() == "minecraft:armor");
        assert!(
            armor.is_none(),
            "control did not reproduce the bug: the sparse-iteration version was expected to \
             omit minecraft:armor entirely once the last piece came off, but it published {armor:?}"
        );
    }

    /// A protocol double whose entity encoders tag each directive with a
    /// distinct packet id and the entity id(s) involved, so a test can read the
    /// streamer's diff *decisions* straight off the returned directives. It does
    /// not implement the chunk/login half — the streamer never calls those.
    struct TagProto;

    const ADD: i32 = 1;
    const UPDATE: i32 = 2;
    const REMOVE: i32 = 3;
    const METADATA: i32 = 4;
    const LINK: i32 = 5;

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
        fn encode_set_entity_link(&self, source_id: i32, target_id: Option<i32>) -> ServerDirective {
            ServerDirective::Send {
                packet_id: LINK,
                // `255` as the "no target" byte: every id this test file uses is
                // small and positive, so it cannot collide with a real target and
                // stays visually distinct from `0`, which is also a plausible id.
                payload: vec![source_id as u8, target_id.map_or(255, |id| id as u8)],
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
            object_data: 0,
            leash_link: None,
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

    /// [`snap`] with a non-empty `metadata` — the metadata field list,
    /// generic to any entity (not creeper-specific: `EntityStreamer::sync`
    /// treats `metadata` uniformly, so a `CreeperSwellDir`/`CreeperIgnited`
    /// pair exercises the same code path the next mob's fields will).
    fn snap_with_metadata(id: i32, x: f64, metadata: Vec<MetadataField>) -> EntitySnapshot {
        EntitySnapshot { metadata, ..snap(id, x) }
    }

    /// A spawn whose snapshot already carries non-empty metadata must send
    /// `ADD` followed by a metadata sync. The separate metadata frame carries
    /// the initial non-default values, including a visible "no swelling
    /// animation" transition.
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

    /// [`snap`] with a `leash_link` already set — the spawn-time link packet.
    fn snap_leashed(id: i32, x: f64, target: i32) -> EntitySnapshot {
        EntitySnapshot { leash_link: Some(target), ..snap(id, x) }
    }

    /// **The discriminating case.** A mob that is *already* leashed by the time
    /// a client first spawns it — a fresh join, or walking back into view range
    /// after the attach happened — must still get the rope: `ADD` followed by
    /// `LINK`. A test that only ever leashes a mob the streamer has already sent
    /// once after spawn would not cover this branch. The snapshot therefore
    /// includes the link at spawn and requires both records.
    #[test]
    fn a_mob_already_leashed_on_spawn_sends_add_then_link() {
        let mut s = EntityStreamer::default();
        // Pairwise-distinct ids (10, 0, 77): a transposition of `source_id` and
        // `target_id` inside the encoder would otherwise be invisible — the
        // wire-shape reason this repo's own CLAUDE.md gives for two adjacent
        // same-typed fields.
        let out = s.sync(&TagProto, &[snap_leashed(10, 0.0, 77)]);
        assert_eq!(out.len(), 2, "expected ADD then LINK, got {out:?}");
        assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
        assert_eq!(sent(&out[1]), (LINK, [10u8, 77u8].as_slice()));
    }

    /// A fresh attach — `None` on the first sync, `Some` on the second — must
    /// send `UPDATE` then `LINK`, with the link payload carrying the real
    /// target rather than the "no holder" sentinel.
    #[test]
    fn attaching_a_leash_after_spawn_sends_update_then_link() {
        let mut s = EntityStreamer::default();
        let _ = s.sync(&TagProto, &[snap(10, 0.0)]);
        let out = s.sync(&TagProto, &[snap_leashed(10, 0.0, 77)]);
        assert_eq!(out.len(), 2, "expected UPDATE then LINK, got {out:?}");
        assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
        assert_eq!(sent(&out[1]), (LINK, [10u8, 77u8].as_slice()));
    }

    /// A detach — `Some` then `None` — must send `UPDATE` then `LINK` again,
    /// this time carrying the "no holder" sentinel (`255` in this test
    /// protocol's own encoding), proving the diff fires on the way down too,
    /// not only on the way up.
    #[test]
    fn detaching_a_leash_sends_update_then_link_with_no_target() {
        let mut s = EntityStreamer::default();
        let _ = s.sync(&TagProto, &[snap_leashed(10, 0.0, 77)]);
        let out = s.sync(&TagProto, &[snap(10, 0.0)]);
        assert_eq!(out.len(), 2, "expected UPDATE then LINK, got {out:?}");
        assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
        assert_eq!(sent(&out[1]), (LINK, [10u8, 255u8].as_slice()));
    }

    /// Negative control: re-syncing the same `leash_link` (still `Some`, same
    /// target) must emit nothing extra beyond position/rotation — proving the
    /// branch is a real diff, matching the metadata family's own control.
    #[test]
    fn unchanged_leash_link_emits_no_extra_link_on_resync() {
        let mut s = EntityStreamer::default();
        let snapshot = snap_leashed(10, 0.0, 77);
        let _ = s.sync(&TagProto, &[snapshot.clone()]);
        let out = s.sync(&TagProto, &[snapshot]);
        assert!(out.is_empty(), "unchanged leash_link must not re-send LINK: {out:?}");
    }

    // -- container screens (Job 1: OPEN_SCREEN/CONTAINER_SET_CONTENT/SLOT/DATA) --

    fn stack(item: &str, count: u32) -> ItemStack {
        ItemStack::new(item.parse().expect("valid resource key"), count)
    }

    #[test]
    fn selected_placement_item_validates_the_held_stack_once() {
        let mut inventory = PlayerInventory::new();
        inventory.set_native(0, Some(stack("minecraft:redstone", 1)));

        assert_eq!(
            selected_placement_item(&inventory, 0),
            Some(Item::Redstone),
            "a built-in held item must enter placement as its registry type"
        );

        inventory.set_native(
            0,
            Some(ItemStack::new(
                ResourceKey::new("example", "custom_block").expect("valid custom key"),
                1,
            )),
        );
        assert_eq!(
            selected_placement_item(&inventory, 0),
            None,
            "an item outside the built-in registry cannot enter the typed placement path"
        );
    }

    const SLOT: i32 = 20;
    const DATA: i32 = 21;
    const CONTENT: i32 = 22;

    /// A protocol double whose container encoders tag each directive with a
    /// distinct packet id, `window_id`, and `state_id`/`property` — enough
    /// for [`sync_open_container`]'s tests to read the diff *decisions* back
    /// off the returned directives without needing the real `lodestone-v26-2`
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
        fn encode_container_content(
            &self,
            window_id: i32,
            state_id: i32,
            items: &[Option<ItemStack>],
            carried: Option<&ItemStack>,
        ) -> ServerDirective {
            ServerDirective::Send {
                packet_id: CONTENT,
                payload: vec![
                    window_id as u8,
                    state_id as u8,
                    items.len() as u8,
                    carried.map_or(0, |s| s.count as u8),
                ],
            }
        }
    }

    fn open(pos: BlockPos, container_size: usize) -> OpenContainer {
        OpenContainer {
            window_id: 7,
            pos,
            shape: MenuKind::Container {
                size: container_size,
            },
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

    /// A real left-click picks a stack up off a native slot and a second one puts
    /// it down elsewhere — the whole thing derived from `(slot, button, type)`,
    /// with no item named anywhere in the input.
    #[test]
    fn container_clicked_against_window_zero_derives_the_move() {
        let mut inventory = PlayerInventory::new();
        inventory.set_native(9, Some(stack("minecraft:stone", 4)));
        let block_entities = BlockEntityHandle::new();

        // Menu slot 9 is native 9. Left-click: whole stack onto the cursor.
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: 9, button: 0, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert_eq!(inventory.native(9), None);
        assert_eq!(
            inventory.click_state().carried.as_ref().map(|s| s.count),
            Some(4)
        );

        // Menu slot 40 is native 4 (hotbar). Left-click: the whole cursor lands.
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: 40, button: 0, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert_eq!(inventory.native(4), Some(&stack("minecraft:stone", 4)));
        assert!(inventory.click_state().carried.is_none());
    }

    /// This runs end to end through the production dispatch path (not just
    /// `container_click`'s own unit tests): a `ServerBound::SelectBundleItem`
    /// packet's consumer (`inventory.set_selected_bundle_item`) is exactly
    /// what `apply_container_clicked`'s later right-click-extract reads.
    /// Without the store-then-read join this proves, a scroll-selected
    /// bundle item would always come out as the front one regardless of
    /// what the player highlighted — the bug the control below actually
    /// caught in `bundle_other_stacked_on_me`'s first draft.
    #[test]
    fn a_select_bundle_item_packet_changes_which_item_a_later_extract_pops() {
        let mut inventory = PlayerInventory::new();
        let mut bundle = stack("minecraft:bundle", 1);
        bundle.components.bundle_contents =
            vec![stack("minecraft:torch", 3), stack("minecraft:oak_planks", 5)];
        // Menu slot 9 is native 9 for window 0 (`MenuLayout::player`'s own
        // storage-first ordering, the same join `container_clicked_against_
        // window_zero_derives_the_move` above already relies on).
        inventory.set_native(9, Some(bundle));
        let block_entities = BlockEntityHandle::new();

        // The dispatch arm's own body: `ServerBound::SelectBundleItem { slot_id:
        // 9, selected_item_index: 1 } => inventory.set_selected_bundle_item(9, 1)`.
        inventory.set_selected_bundle_item(9, 1);

        // Right-click (button 1, PICKUP) on slot 9 with an empty cursor.
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: 9, button: 1, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );

        assert_eq!(
            inventory.click_state().carried.as_ref().map(|s| s.item.to_string()),
            Some("minecraft:oak_planks".to_owned()),
            "the selected index (1) should have been extracted, not the front item (0)"
        );
    }

    /// **The security property.** A client claiming a slot now holds an item it
    /// never had mints nothing: the claim is not stored, and the server answers the
    /// same click with a full `container_set_content` correction.
    ///
    /// Both halves are asserted because they fail independently — a server that
    /// ignored the claim but sent no correction would leave the client believing in
    /// an item that does not exist.
    #[test]
    fn a_claimed_item_is_never_stored_and_the_client_is_corrected() {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();

        // An empty inventory, an empty cursor, a left-click on an empty slot — and
        // a diff claiming a diamond block appeared there.
        let (correction, dropped) = apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: 9, button: 0, click_type: 0 },
            &[(9, Some(stack("minecraft:diamond_block", 64)))],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert_eq!(inventory.native(9), None, "the claim must not be stored");
        assert!(dropped.is_empty());
        assert!(
            matches!(correction, Some(ServerDirective::Send { packet_id, .. }) if packet_id == CONTENT),
            "a disagreeing claim must be corrected, got {correction:?}"
        );

        // And a claim that matches what the server derived sends nothing at all,
        // so an honest client pays no extra traffic — the control that the
        // correction above is a comparison rather than an unconditional resend.
        inventory.set_native(9, Some(stack("minecraft:stone", 1)));
        let (correction, _) = apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: 9, button: 0, click_type: 0 },
            &[(9, None)],
            Some(&stack("minecraft:stone", 1)),
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert_eq!(correction, None, "an honest prediction needs no correction");
    }

    /// Crafting, end to end and server-derived: planks clicked into the 2x2 make
    /// the server derive a crafting table, and taking the result consumes the grid.
    /// The client never names a result.
    #[test]
    fn the_crafting_result_is_derived_and_taking_it_consumes_the_grid() {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        inventory.click_state_mut().carried = Some(stack("minecraft:oak_planks", 4));

        // Right-click each of the four grid cells: one plank each.
        for menu_slot in 1..=4 {
            apply_container_clicked(
                &ContainerTagProto,
                &mut inventory,
                &block_entities,
                None,
                0,
                Click { slot: menu_slot, button: 1, click_type: 0 },
                &[],
                None,
                false,
                i32::MAX,
                &crate::plugin_crafting::CraftingStationHooks::default(),
            );
        }
        assert_eq!(
            inventory.crafting().result().map(|r| r.item.to_string()),
            Some("minecraft:crafting_table".to_string()),
            "the server derived the result from the grid it now holds"
        );

        // Take it. The cursor holds the table and every grid cell is empty again.
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: 0, button: 0, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert_eq!(
            inventory
                .click_state()
                .carried
                .as_ref()
                .map(|s| s.item.to_string()),
            Some("minecraft:crafting_table".to_string())
        );
        assert!(inventory.crafting().is_empty(), "one craft consumed the grid");
        assert!(inventory.crafting().result().is_none());
    }

    /// The anvil end to end through the real click path: place a
    /// damaged pickaxe and a repair material into the two input cells, take the
    /// derived result, and check both the item mutation and the input-slot
    /// consumption `container_click`'s `take_result` special-cases for
    /// `Station::Anvil` — cell 0 always clears, cell 1 shrinks by the repair
    /// material count actually used, not by one and not entirely.
    #[test]
    fn the_anvil_repairs_through_the_real_click_path_and_consumes_the_right_amount() {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        inventory.open_workstation(2);
        let mut input = stack("minecraft:diamond_pickaxe", 1);
        input.components.damage = Some(1200);
        input.components.max_damage = Some(1561);
        if let Some(ws) = inventory.workstation_mut() {
            ws[0] = Some(input);
            ws[1] = Some(stack("minecraft:diamond", 3));
        }
        let mut open = OpenContainer {
            window_id: 7,
            pos: BlockPos::new(0, 0, 0),
            shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
            container_size: 3,
            state_id: 0,
        };

        // Menu slot 2 is the result for a 2-input combiner menu.
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            7,
            Click { slot: 2, button: 0, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );

        let carried = inventory.click_state().carried.as_ref().expect("the repaired pickaxe must be on the cursor");
        assert_eq!(carried.item.to_string(), "minecraft:diamond_pickaxe");
        assert_eq!(carried.components.damage, Some(30), "matches anvil::compute's own repair-with-material test");

        let cells = inventory.workstation().expect("still open");
        assert_eq!(cells[0], None, "the base item is always fully consumed");
        assert_eq!(
            cells[1], None,
            "all 3 diamonds were used by the repair (repair_item_count_cost == addition.count)"
        );
    }

    /// The anvil result cannot be taken by a survival player without enough XP
    /// levels. The end-to-end click leaves the result in place, keeps the cursor
    /// empty, and consumes nothing; exactly enough XP and creative mode both
    /// allow the take.
    #[test]
    fn a_0_xp_survival_player_cannot_take_a_costed_anvil_result_but_creative_and_enough_levels_can() {
        let same_repair_fixture = |inventory: &mut PlayerInventory| {
            inventory.open_workstation(2);
            let mut input = stack("minecraft:diamond_pickaxe", 1);
            input.components.damage = Some(1200);
            input.components.max_damage = Some(1561);
            if let Some(ws) = inventory.workstation_mut() {
                ws[0] = Some(input);
                ws[1] = Some(stack("minecraft:diamond", 3));
            }
        };
        let open_anvil = || OpenContainer {
            window_id: 7,
            pos: BlockPos::new(0, 0, 0),
            shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
            container_size: 3,
            state_id: 0,
        };

        // The real cost this exact fixture prices to — read from `anvil::compute`
        // itself (the single already-tested source of truth for the formula;
        // this test is about the `xp_level`-vs-`cost` *wiring*, not re-deriving
        // the repair-cost arithmetic a second time), not guessed.
        let mut priced_input = stack("minecraft:diamond_pickaxe", 1);
        priced_input.components.damage = Some(1200);
        priced_input.components.max_damage = Some(1561);
        let cost = crate::anvil::compute(
            Some(&priced_input),
            Some(&stack("minecraft:diamond", 3)),
            None,
            false,
        )
        .cost;
        assert!(cost > 0, "the fixture must actually cost XP levels, or this test proves nothing");

        // 0 XP levels, survival: refused. Nothing moves, nothing is consumed.
        {
            let mut inventory = PlayerInventory::new();
            let block_entities = BlockEntityHandle::new();
            same_repair_fixture(&mut inventory);
            let mut open = open_anvil();
            apply_container_clicked(
                &ContainerTagProto,
                &mut inventory,
                &block_entities,
                Some(&mut open),
                7,
                Click { slot: 2, button: 0, click_type: 0 },
                &[],
                None,
                false,
                0,
                &crate::plugin_crafting::CraftingStationHooks::default(),
            );
            assert!(
                inventory.click_state().carried.is_none(),
                "0 XP levels must not take a {cost}-cost anvil result"
            );
            let cells = inventory.workstation().expect("still open");
            assert!(cells[0].is_some(), "the base item must stay put when the take is refused");
            assert!(cells[1].is_some(), "the addition must stay put when the take is refused");
        }

        // Exactly `cost` XP levels, survival: succeeds — the `>=`, not `>`, half
        // of vanilla's own anvil-menu may-pickup gate's comparison.
        {
            let mut inventory = PlayerInventory::new();
            let block_entities = BlockEntityHandle::new();
            same_repair_fixture(&mut inventory);
            let mut open = open_anvil();
            apply_container_clicked(
                &ContainerTagProto,
                &mut inventory,
                &block_entities,
                Some(&mut open),
                7,
                Click { slot: 2, button: 0, click_type: 0 },
                &[],
                None,
                false,
                cost,
                &crate::plugin_crafting::CraftingStationHooks::default(),
            );
            assert!(
                inventory.click_state().carried.is_some(),
                "exactly {cost} XP levels must take the result"
            );
        }

        // Creative, 0 XP levels: succeeds unconditionally because creative
        // bypasses the experience-cost check.
        {
            let mut inventory = PlayerInventory::new();
            let block_entities = BlockEntityHandle::new();
            same_repair_fixture(&mut inventory);
            let mut open = open_anvil();
            apply_container_clicked(
                &ContainerTagProto,
                &mut inventory,
                &block_entities,
                Some(&mut open),
                7,
                Click { slot: 2, button: 0, click_type: 0 },
                &[],
                None,
                true,
                0,
                &crate::plugin_crafting::CraftingStationHooks::default(),
            );
            assert!(
                inventory.click_state().carried.is_some(),
                "creative must take regardless of XP levels"
            );
        }
    }

    /// The anvil's genuinely bespoke take rule (vanilla's own anvil-menu on-take routine): a take
    /// priced *purely* by a pending rename must leave a present-but-not-
    /// consumed addition cell completely untouched, not cleared as if a real
    /// combine had consumed it. `container_click::take_result`'s own internal
    /// re-derivation always evaluates with no rename text (that module is
    /// deliberately rename-free) and so cannot see this by itself — see
    /// `apply_workstation_clicked`'s own correction, which this pins.
    #[test]
    fn a_pure_rename_take_leaves_a_present_but_unconsumed_addition_untouched() {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        inventory.open_workstation(2);
        inventory.set_pending_rename(Some("Excalibur".to_owned()));
        let input = stack("minecraft:diamond_sword", 1);
        let addition = stack("minecraft:diamond_sword", 1);
        if let Some(ws) = inventory.workstation_mut() {
            ws[0] = Some(input);
            ws[1] = Some(addition.clone());
        }
        let mut open = OpenContainer {
            window_id: 7,
            pos: BlockPos::new(0, 0, 0),
            shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
            container_size: 3,
            state_id: 0,
        };

        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            7,
            Click { slot: 2, button: 0, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );

        let carried = inventory.click_state().carried.as_ref().expect("must take the renamed sword");
        assert_eq!(
            carried.components.custom_name.as_ref().map(lodestone_model::text::Text::to_plain_string),
            Some("Excalibur".to_owned())
        );
        let cells = inventory.workstation().expect("still open");
        assert_eq!(cells[0], None, "the base item is always fully consumed");
        assert_eq!(
            cells[1],
            Some(addition),
            "a pure-rename take must leave an unconsumed addition exactly as it was"
        );
    }

    /// The grindstone end to end: a single enchanted item in one
    /// slot strips to curses only, and taking it **fully clears** the input
    /// cell it came from — the grindstone's distinct-from-the-anvil take rule.
    #[test]
    fn the_grindstone_strips_enchantments_through_the_real_click_path() {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        inventory.open_workstation(2);
        let mut sword = stack("minecraft:diamond_sword", 1);
        sword.components.enchantments = vec![lodestone_model::ItemEnchantment {
            id: crate::enchantment_data::id_of("minecraft:sharpness").unwrap(),
            level: 3,
        }];
        if let Some(ws) = inventory.workstation_mut() {
            ws[0] = Some(sword);
        }
        let mut open = OpenContainer {
            window_id: 7,
            pos: BlockPos::new(0, 0, 0),
            shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Grindstone },
            container_size: 3,
            state_id: 0,
        };

        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            7,
            Click { slot: 2, button: 0, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );

        let carried = inventory.click_state().carried.as_ref().expect("must take a plain sword back");
        assert!(carried.components.enchantments.is_empty(), "sharpness is not a curse and must be stripped");
        let cells = inventory.workstation().expect("still open");
        assert_eq!(cells[0], None, "grindstone always fully clears both inputs on take");
        assert_eq!(cells[1], None);
    }

    /// The smithing table end to end: a netherite upgrade through
    /// the real click path, checking the generic shrink-by-1 take behaviour
    /// (shared with the crafting table) applies to all three input cells.
    #[test]
    fn the_smithing_table_upgrades_to_netherite_through_the_real_click_path() {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        inventory.open_workstation(3);
        if let Some(ws) = inventory.workstation_mut() {
            ws[0] = Some(stack("minecraft:netherite_upgrade_smithing_template", 1));
            ws[1] = Some(stack("minecraft:diamond_sword", 1));
            ws[2] = Some(stack("minecraft:netherite_ingot", 1));
        }
        let mut open = OpenContainer {
            window_id: 7,
            pos: BlockPos::new(0, 0, 0),
            shape: MenuKind::ItemCombiner { inputs: 3, station: Station::Smithing },
            container_size: 4,
            state_id: 0,
        };

        // Menu slot 3 is the result for a 3-input combiner menu.
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            7,
            Click { slot: 3, button: 0, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );

        let carried = inventory.click_state().carried.as_ref().expect("must take the upgraded sword");
        assert_eq!(carried.item.to_string(), "minecraft:netherite_sword");
        let cells = inventory.workstation().expect("still open");
        assert!(cells.iter().all(Option::is_none), "each of the three inputs was a stack of one and is now consumed");
    }

    /// The anvil action reaches [`crate::anvil::compute`] (a pure rename costs
    /// exactly 1 XP level) and re-sending the identical name is a no-op.
    #[test]
    fn rename_item_prices_a_pure_rename_at_one_and_is_idempotent() {
        let mut inventory = PlayerInventory::new();
        inventory.open_workstation(2);
        if let Some(ws) = inventory.workstation_mut() {
            ws[0] = Some(stack("minecraft:diamond_sword", 1));
        }
        let mut open = OpenContainer {
            window_id: 7,
            pos: BlockPos::new(0, 0, 0),
            shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
            container_size: 3,
            state_id: 0,
        };

        let directives = apply_rename_item(
            &ContainerTagProto,
            &mut inventory,
            Some(&mut open),
            "Excalibur",
            false,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert_eq!(directives.len(), 2, "the refreshed content, then the cost data slot");
        assert_eq!(inventory.pending_rename(), Some("Excalibur"));
        match &directives[1] {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(*packet_id, DATA);
                assert_eq!(payload[2], 1, "a pure rename costs exactly 1 XP level");
            }
            other => panic!("expected a Send directive, got {other:?}"),
        }

        let again = apply_rename_item(
            &ContainerTagProto,
            &mut inventory,
            Some(&mut open),
            "Excalibur",
            false,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(again.is_empty(), "an unchanged name must not resend anything");
    }

    /// The enchanting-table action reaches the real click path: choosing an offer
    /// enchants the item, spends XP levels, consumes lapis, and rerolls the seed.
    #[test]
    fn container_button_click_enchants_the_item_and_charges_xp_and_lapis() {
        struct AirWorld;
        impl ChunkSource for AirWorld {
            fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
                unimplemented!("not needed: bookshelf_power reads block_state only")
            }
            fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
                "minecraft:air".to_owned()
            }

            fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
                crate::chunk::DEFAULT_BIOME.to_string()
            }
            fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
                unimplemented!("read-only in this test")
            }
        }

        let sword = stack("minecraft:diamond_sword", 1);
        // Slot 0's cost floors at 1 for any enchantable item regardless of the
        // roll (`cost_for_slot`'s `(selected / 3).max(1)`), so only the offer
        // draw itself needs a seed search — deterministic given the
        // production RNG, not flaky: whichever seed is found here always
        // rolls the same offer.
        let seed = (0..64i64)
            .find(|&s| {
                let costs = crate::enchanting::table_costs(s, 0, &sword);
                let mut rng = SpawnRng::new(s.wrapping_add(0) as u64);
                costs[0] > 0 && !crate::enchanting::select_enchantments(&mut rng, &sword, costs[0]).is_empty()
            })
            .expect("at least one of the first 64 seeds must roll a slot-0 offer for a diamond sword");

        let mut inventory = PlayerInventory::new();
        inventory.open_workstation(2);
        inventory.set_enchant_seed(seed);
        if let Some(ws) = inventory.workstation_mut() {
            ws[0] = Some(sword);
            ws[1] = Some(stack("minecraft:lapis_lazuli", 5));
        }
        let mut open = OpenContainer {
            window_id: 7,
            pos: BlockPos::new(0, 0, 0),
            shape: MenuKind::Enchanting,
            container_size: 2,
            state_id: 0,
        };
        let mut experience = crate::experience::PlayerExperience::default();
        experience.give_points(crate::experience::total_points_for_level(30));
        let before_level = experience.level();

        let directives = apply_container_button_click(
            &ContainerTagProto,
            &mut inventory,
            Some(&mut open),
            7,
            0,
            &AirWorld,
            &mut experience,
            false,
            999,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );

        assert!(!directives.is_empty(), "a successful enchant must resend the menu");
        assert!(experience.level() < before_level, "XP levels must be spent");
        let cells = inventory.workstation().expect("still open");
        let enchanted = cells[0].as_ref().expect("the item stays in slot 0");
        assert!(
            !enchanted.components.enchantments.is_empty(),
            "the item must come back enchanted"
        );
        let lapis_left = cells[1].as_ref().map_or(0, |l| l.count);
        assert!(lapis_left < 5, "at least one lapis must be consumed, left {lapis_left}");
        assert_eq!(inventory.enchant_seed(), 999, "a successful enchant rerolls the seed");

        // A second click at the same (now stale) slot-0 cost/seed combination
        // is refused once the seed has moved on — not a hang, not a panic,
        // and not a second free enchant.
        let refused = apply_container_button_click(
            &ContainerTagProto,
            &mut inventory,
            Some(&mut open),
            7,
            5, // out of range: only 0..3 are real slots
            &AirWorld,
            &mut experience,
            false,
            1,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(refused.is_empty(), "an out-of-range button id must be refused");
    }

    /// This runs end to end through the real production dispatch: a
    /// stonecutter menu opens with cobblestone in its input cell, a
    /// `ContainerButtonClick` selects one of the real offers
    /// `crate::stonecutting::matches` computes, and taking the result slot
    /// consumes exactly one cobblestone and leaves the rest — the same
    /// `apply_container_clicked` → `apply_workstation_clicked` →
    /// `container_click::take_result` path every other workstation in this
    /// crate already goes through, not a hand-rolled shortcut.
    #[test]
    fn a_stonecutter_button_click_then_take_produces_the_selected_recipe_and_consumes_one_input() {
        // `apply_workstation_button_click` (the loom/stonecutter branch of
        // `apply_container_button_click`) never reads `source` at all —
        // unlike the enchanting branch's `bookshelf_power` call — so this
        // must never be invoked.
        struct UnusedSource;
        impl ChunkSource for UnusedSource {
            fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
                unimplemented!("the stonecutter button click must never read the world")
            }
            fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
                unimplemented!("the stonecutter button click must never read the world")
            }

            fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
                crate::chunk::DEFAULT_BIOME.to_string()
            }
            fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
                unimplemented!("read-only in this test")
            }
        }

        let mut inventory = PlayerInventory::new();
        inventory.open_workstation(1);
        if let Some(ws) = inventory.workstation_mut() {
            ws[0] = Some(stack("minecraft:cobblestone", 5));
        }
        let mut open = OpenContainer {
            window_id: 9,
            pos: BlockPos::new(0, 0, 0),
            shape: MenuKind::ItemCombiner { inputs: 1, station: Station::Stonecutter },
            container_size: 2,
            state_id: 0,
        };
        let offers = crate::stonecutting::matches(&stack("minecraft:cobblestone", 1));
        assert!(offers.len() >= 2, "need at least two offers to prove a specific one was selected");

        // Select offer index 1 — not the default/first, so a bug that always
        // takes index 0 would fail this.
        let directives = apply_container_button_click(
            &ContainerTagProto,
            &mut inventory,
            Some(&mut open),
            9,
            1,
            &UnusedSource,
            &mut crate::experience::PlayerExperience::default(),
            false,
            0,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(!directives.is_empty(), "a valid selection must resend the menu");
        assert_eq!(inventory.selected_recipe_index(), Some(1));

        let block_entities = BlockEntityHandle::new();
        let (_, _dropped) = apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            9,
            // Slot `inputs` (1) is the result slot — a plain left-click picks
            // it up, which is what triggers `take_result`.
            Click { slot: 1, button: 0, click_type: 0 },
            &[],
            None,
            false,
            0,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );

        let taken = inventory
            .click_state()
            .carried
            .as_ref()
            .expect("the take must put the result on the cursor");
        assert_eq!(taken.item, offers[1].item, "the taken item must be the selected offer, not the first one");

        let cells = inventory.workstation().expect("still open");
        assert_eq!(
            cells[0].as_ref().map(|s| s.count),
            Some(4),
            "exactly one cobblestone must be consumed by the take"
        );
    }

    /// This runs end to end: a loom with a banner, a dye and a specific
    /// pattern *item* auto-selects that item's one pattern — no
    /// `ContainerButtonClick` needed, matching vanilla's own loom-menu slots-changed routine's own
    /// auto-select branch — and taking the result consumes exactly one
    /// banner and one dye while leaving the pattern item untouched, so it
    /// can stamp a second banner.
    #[test]
    fn a_loom_take_with_a_pattern_item_consumes_banner_and_dye_but_not_the_pattern_item() {
        let mut inventory = PlayerInventory::new();
        inventory.open_workstation(3);
        if let Some(ws) = inventory.workstation_mut() {
            ws[0] = Some(stack("minecraft:white_banner", 3));
            ws[1] = Some(stack("minecraft:red_dye", 5));
            ws[2] = Some(stack("minecraft:creeper_banner_pattern", 1));
        }
        let mut open = OpenContainer {
            window_id: 11,
            pos: BlockPos::new(0, 0, 0),
            shape: MenuKind::ItemCombiner { inputs: 3, station: Station::Loom },
            container_size: 4,
            state_id: 0,
        };
        let block_entities = BlockEntityHandle::new();

        let (_, _dropped) = apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            11,
            // Slot `inputs` (3) is the result slot.
            Click { slot: 3, button: 0, click_type: 0 },
            &[],
            None,
            false,
            0,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );

        let taken = inventory.click_state().carried.as_ref().expect("the take must produce a result");
        assert_eq!(taken.item.to_string(), "minecraft:white_banner");
        assert_eq!(
            taken.components.banner_patterns,
            vec![lodestone_model::BannerPatternLayer {
                pattern_asset_id: "creeper".to_string(),
                color: "red".to_string(),
            }]
        );

        let cells = inventory.workstation().expect("still open");
        assert_eq!(cells[0].as_ref().map(|s| s.count), Some(2), "one banner must be consumed");
        assert_eq!(cells[1].as_ref().map(|s| s.count), Some(4), "one dye must be consumed");
        assert_eq!(
            cells[2].as_ref().map(|s| s.count),
            Some(1),
            "the pattern item must survive the take, so it can stamp a second banner"
        );
    }

    /// Two test-local stand-ins reproducing `lodestone-crafting-warden`'s
    /// real `SmithingSwordBan`/`AnvilBlessing` logic exactly, kept local
    /// rather than a dev-dependency on that crate.
    ///
    /// A dev-dependency depending back on this crate would compile this
    /// crate's own `--lib` unit-test binary *twice* — once as the unit
    /// under test, once through the plugin's normal dependency edge — which
    /// produces two incompatible `CraftingStationHooks` types sharing one
    /// name (`error[E0308]: mismatched types … multiple different versions
    /// of crate lodestone_server in the dependency graph`). An integration
    /// test under `tests/*.rs` would avoid that (it links this crate's lib
    /// once, normally), but `apply_container_clicked`/
    /// `apply_workstation_clicked`/`apply_container_button_click`/
    /// `apply_rename_item` are module-private, so a test proving they
    /// consult a registered hook can only live inside this module. The
    /// external crate's own unit tests call `on_prepare` directly to prove
    /// its logic; these two prove the opposite half — that production
    /// actually asks the question — by driving the real dispatch below.
    struct WiringProofDenySwordUpgrade;
    impl crate::plugin_crafting::CraftingStationHook for WiringProofDenySwordUpgrade {
        fn on_prepare(&self, inputs: &crate::plugin_crafting::StationInputs) -> crate::plugin_crafting::StationVerdict {
            if inputs.station != Station::Smithing {
                return crate::plugin_crafting::StationVerdict::Allow;
            }
            let base = inputs.cells.get(1).and_then(Option::as_ref);
            if base.is_some_and(|item| item.item.to_string() == "minecraft:diamond_sword") {
                crate::plugin_crafting::StationVerdict::Deny
            } else {
                crate::plugin_crafting::StationVerdict::Allow
            }
        }
    }

    struct WiringProofBlessAnvilName;
    impl crate::plugin_crafting::CraftingStationHook for WiringProofBlessAnvilName {
        fn on_prepare(&self, inputs: &crate::plugin_crafting::StationInputs) -> crate::plugin_crafting::StationVerdict {
            if inputs.station != Station::Anvil {
                return crate::plugin_crafting::StationVerdict::Allow;
            }
            let Some(computed) = inputs.computed.clone() else {
                return crate::plugin_crafting::StationVerdict::Allow;
            };
            let Some(name) = computed.components.custom_name.clone() else {
                return crate::plugin_crafting::StationVerdict::Allow;
            };
            let plain = name.to_plain_string();
            if plain.starts_with("[Blessed] ") {
                return crate::plugin_crafting::StationVerdict::Allow;
            }
            let mut blessed = computed;
            blessed.components.custom_name = Some(lodestone_model::text::Text::literal(format!("[Blessed] {plain}")));
            crate::plugin_crafting::StationVerdict::Replace(blessed)
        }
    }

    /// Exercises plugin hook registration through the production smithing
    /// click path. `WiringProofDenySwordUpgrade` vetoes one netherite upgrade
    /// when registered, while a sibling upgrade remains allowed, proving that
    /// dispatch consults the hook for the derived menu result.
    #[test]
    fn a_registered_plugin_hook_vetoes_one_smithing_upgrade_and_allows_a_sibling_one() {
        let hooks = crate::plugin_crafting::CraftingStationHooks::new();
        hooks.register(0, std::sync::Arc::new(WiringProofDenySwordUpgrade));
        let block_entities = BlockEntityHandle::new();

        let mut denied = PlayerInventory::new();
        denied.open_workstation(3);
        if let Some(ws) = denied.workstation_mut() {
            ws[0] = Some(stack("minecraft:netherite_upgrade_smithing_template", 1));
            ws[1] = Some(stack("minecraft:diamond_sword", 1));
            ws[2] = Some(stack("minecraft:netherite_ingot", 1));
        }
        let mut open_denied = OpenContainer {
            window_id: 20,
            pos: BlockPos::new(0, 0, 0),
            shape: MenuKind::ItemCombiner { inputs: 3, station: Station::Smithing },
            container_size: 4,
            state_id: 0,
        };
        apply_container_clicked(
            &ContainerTagProto,
            &mut denied,
            &block_entities,
            Some(&mut open_denied),
            20,
            // Slot `inputs` (3) is the result slot.
            Click { slot: 3, button: 0, click_type: 0 },
            &[],
            None,
            false,
            0,
            &hooks,
        );
        assert!(
            denied.click_state().carried.is_none(),
            "the registered SmithingSwordBan hook must veto the sword upgrade, so nothing is taken"
        );
        let denied_cells = denied.workstation().expect("still open");
        assert!(denied_cells[1].is_some(), "a denied take must leave the base item in place");

        // Positive control, the same dispatch with a pickaxe base instead of
        // a sword: this must succeed, proving the veto is scoped to the one
        // named item rather than blocking every smithing take.
        let mut allowed = PlayerInventory::new();
        allowed.open_workstation(3);
        if let Some(ws) = allowed.workstation_mut() {
            ws[0] = Some(stack("minecraft:netherite_upgrade_smithing_template", 1));
            ws[1] = Some(stack("minecraft:diamond_pickaxe", 1));
            ws[2] = Some(stack("minecraft:netherite_ingot", 1));
        }
        let mut open_allowed = OpenContainer {
            window_id: 21,
            pos: BlockPos::new(0, 0, 0),
            shape: MenuKind::ItemCombiner { inputs: 3, station: Station::Smithing },
            container_size: 4,
            state_id: 0,
        };
        apply_container_clicked(
            &ContainerTagProto,
            &mut allowed,
            &block_entities,
            Some(&mut open_allowed),
            21,
            Click { slot: 3, button: 0, click_type: 0 },
            &[],
            None,
            false,
            0,
            &hooks,
        );
        let taken = allowed
            .click_state()
            .carried
            .as_ref()
            .expect("the pickaxe upgrade must be allowed through unchanged");
        assert_eq!(taken.item.to_string(), "minecraft:netherite_pickaxe");
    }

    /// Exercises the plugin replacement branch through the production anvil
    /// click path. `WiringProofBlessAnvilName` adds a `[Blessed]` prefix to a
    /// rename result before the player takes it.
    #[test]
    fn a_registered_plugin_hook_blesses_a_real_anvil_rename_take() {
        let hooks = crate::plugin_crafting::CraftingStationHooks::new();
        hooks.register(0, std::sync::Arc::new(WiringProofBlessAnvilName));

        let mut inventory = PlayerInventory::new();
        inventory.open_workstation(2);
        if let Some(ws) = inventory.workstation_mut() {
            ws[0] = Some(stack("minecraft:diamond_sword", 1));
        }
        let mut open = OpenContainer {
            window_id: 22,
            pos: BlockPos::new(0, 0, 0),
            shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
            container_size: 3,
            state_id: 0,
        };

        apply_rename_item(&ContainerTagProto, &mut inventory, Some(&mut open), "Excalibur", false, &hooks);
        assert_eq!(inventory.pending_rename(), Some("Excalibur"));

        let block_entities = BlockEntityHandle::new();
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            22,
            // Slot `inputs` (2) is the result slot.
            Click { slot: 2, button: 0, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &hooks,
        );
        let taken = inventory
            .click_state()
            .carried
            .as_ref()
            .expect("the rename take must succeed");
        let name = taken.components.custom_name.as_ref().expect("still named");
        assert_eq!(
            name.to_plain_string(),
            "[Blessed] Excalibur",
            "the registered AnvilBlessing hook must have tweaked the real rename result"
        );
    }

    /// A click against the connection's *open* non-zero window reaches both the
    /// block entity's own slots and the player tail, through the same layout.
    #[test]
    fn container_clicked_against_an_open_window_reaches_both_sections() {
        let mut inventory = PlayerInventory::new();
        inventory.set_native(9, Some(stack("minecraft:coal", 1)));
        let block_entities = BlockEntityHandle::new();
        let pos = BlockPos::new(1, 2, 3);
        block_entities.with(|reg| {
            reg.insert(pos, BlockEntity::Furnace(Furnace::new(FurnaceKind::Furnace)));
        });
        let mut open = open(pos, 3);

        // Menu slot 3 is the player tail's first entry (native 9): pick the coal up.
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            7,
            Click { slot: 3, button: 0, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert_eq!(inventory.native(9), None);

        // Menu slot 1 is the furnace's own fuel slot: put it down there.
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            7,
            Click { slot: 1, button: 0, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        let furnace_fuel = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::Furnace(f)) => f.fuel().cloned(),
            _ => None,
        });
        assert_eq!(furnace_fuel, Some(stack("minecraft:coal", 1)));
    }

    /// [`apply_set_beacon`]'s happy path: a level-1 pyramid, a payment item
    /// present, a valid tier-1 primary — the payment is consumed and the
    /// selection lands on the block entity.
    #[test]
    fn set_beacon_consumes_payment_and_stores_a_valid_selection() {
        let block_entities = BlockEntityHandle::new();
        let pos = BlockPos::new(0, 64, 0);
        block_entities.with(|reg| {
            reg.insert(
                pos,
                BlockEntity::Beacon(crate::block_entities::BeaconData {
                    levels: 1,
                    primary_effect: None,
                    secondary_effect: None,
                    payment: Some(stack("minecraft:emerald", 3)),
                }),
            );
        });
        let mut open = OpenContainer {
            window_id: 7,
            pos,
            shape: MenuKind::Beacon,
            container_size: 1,
            state_id: 0,
        };

        let directives = apply_set_beacon(
            &ContainerTagProto,
            &block_entities,
            Some(&mut open),
            Some("minecraft:speed".to_owned()),
            None,
        );
        assert!(!directives.is_empty(), "a successful selection must resend the menu");

        let after = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::Beacon(b)) => b.clone(),
            _ => panic!("beacon must still be there"),
        });
        assert_eq!(
            after.primary_effect,
            Some(
                crate::beacon::BeaconPower::from_key("minecraft:speed")
                    .expect("beacon power")
            )
        );
        assert_eq!(after.secondary_effect, None);
        assert_eq!(after.payment, Some(stack("minecraft:emerald", 2)), "exactly one payment item is spent");
    }

    /// **Control**: no payment item present must refuse the selection
    /// entirely — a payment item is required. Without this,
    /// the happy-path test above (which does have payment) could not tell a
    /// correct gate from one that never checked at all.
    #[test]
    fn set_beacon_refuses_without_a_payment_item() {
        let block_entities = BlockEntityHandle::new();
        let pos = BlockPos::new(0, 64, 0);
        block_entities.with(|reg| {
            reg.insert(
                pos,
                BlockEntity::Beacon(crate::block_entities::BeaconData {
                    levels: 4,
                    primary_effect: None,
                    secondary_effect: None,
                    payment: None,
                }),
            );
        });
        let mut open = OpenContainer {
            window_id: 7,
            pos,
            shape: MenuKind::Beacon,
            container_size: 1,
            state_id: 0,
        };

        let directives = apply_set_beacon(
            &ContainerTagProto,
            &block_entities,
            Some(&mut open),
            Some("minecraft:speed".to_owned()),
            None,
        );
        assert!(directives.is_empty());
        let after = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::Beacon(b)) => b.clone(),
            _ => panic!("beacon must still be there"),
        });
        assert_eq!(after.primary_effect, None, "a refused submission must not write the selection");
    }

    /// **Control**: an invalid pair for the pyramid's own level (here, a
    /// secondary on a level-1 pyramid) must be refused, and the payment must
    /// stay untouched — not spent on a rejected submission.
    #[test]
    fn set_beacon_refuses_an_invalid_pair_and_keeps_the_payment() {
        let block_entities = BlockEntityHandle::new();
        let pos = BlockPos::new(0, 64, 0);
        block_entities.with(|reg| {
            reg.insert(
                pos,
                BlockEntity::Beacon(crate::block_entities::BeaconData {
                    levels: 1,
                    primary_effect: None,
                    secondary_effect: None,
                    payment: Some(stack("minecraft:diamond", 1)),
                }),
            );
        });
        let mut open = OpenContainer {
            window_id: 7,
            pos,
            shape: MenuKind::Beacon,
            container_size: 1,
            state_id: 0,
        };

        let directives = apply_set_beacon(
            &ContainerTagProto,
            &block_entities,
            Some(&mut open),
            Some("minecraft:speed".to_owned()),
            Some("minecraft:regeneration".to_owned()),
        );
        assert!(directives.is_empty());
        let after = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::Beacon(b)) => b.clone(),
            _ => panic!("beacon must still be there"),
        });
        assert_eq!(after.payment, Some(stack("minecraft:diamond", 1)), "a refused submission must not spend payment");
    }

    /// A raw serverbound key crosses into `BeaconPower` before it reaches the
    /// persisted block entity. `poison` is a real mob effect but not a beacon
    /// power, so this distinguishes the closed-domain boundary from merely
    /// rejecting an unknown string.
    #[test]
    fn set_beacon_rejects_a_known_non_power_key_at_the_boundary() {
        let block_entities = BlockEntityHandle::new();
        let pos = BlockPos::new(0, 64, 0);
        block_entities.with(|reg| {
            reg.insert(
                pos,
                BlockEntity::Beacon(crate::block_entities::BeaconData {
                    levels: 4,
                    primary_effect: None,
                    secondary_effect: None,
                    payment: Some(stack("minecraft:diamond", 1)),
                }),
            );
        });
        let mut open = OpenContainer {
            window_id: 7,
            pos,
            shape: MenuKind::Beacon,
            container_size: 1,
            state_id: 0,
        };

        let directives = apply_set_beacon(
            &ContainerTagProto,
            &block_entities,
            Some(&mut open),
            Some("minecraft:poison".to_owned()),
            None,
        );
        assert!(directives.is_empty());
        let after = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::Beacon(b)) => b.clone(),
            _ => panic!("beacon must still be there"),
        });
        assert_eq!(after.primary_effect, None);
        assert_eq!(after.payment, Some(stack("minecraft:diamond", 1)));
    }

    /// The crafting **table**'s 3×3 menu, which has no block entity at all:
    /// clicks reach the table's own grid and the server derives a 3×3
    /// result the 2×2 player screen structurally cannot make.
    #[test]
    fn a_crafting_table_menu_derives_a_3x3_result() {
        let mut inventory = PlayerInventory::new();
        inventory.open_table_crafting();
        let block_entities = BlockEntityHandle::new();
        let mut open = OpenContainer {
            window_id: 3,
            pos: BlockPos::new(0, 64, 0),
            shape: MenuKind::CraftingTable,
            container_size: 10,
            state_id: 0,
        };

        // Eight planks around an empty centre is a chest — a 3x3-only recipe.
        inventory.click_state_mut().carried = Some(stack("minecraft:oak_planks", 8));
        for menu_slot in [1, 2, 3, 4, 6, 7, 8, 9] {
            apply_container_clicked(
                &ContainerTagProto,
                &mut inventory,
                &block_entities,
                Some(&mut open),
                3,
                Click { slot: menu_slot, button: 1, click_type: 0 },
                &[],
                None,
                false,
                i32::MAX,
                &crate::plugin_crafting::CraftingStationHooks::default(),
            );
        }
        assert_eq!(
            inventory
                .table_crafting()
                .and_then(|g| g.result())
                .map(|r| r.item.to_string()),
            Some("minecraft:chest".to_string()),
            "the table's own 3x3 grid derived the result"
        );
        assert!(
            inventory.crafting().is_empty(),
            "the player screen's 2x2 must be untouched — they are separate grids"
        );
    }

    /// **The reported bug**: the result the server derives has to *reach the client*,
    /// and taking it has to work on the same click — no reopen.
    ///
    /// The claims below are the real client's: `lodestone-game`'s `ClientMenu::predict`
    /// diffs its own menu before/after, and its result slot is server-owned, so a grid
    /// click claims the cell and the cursor and **never the result**. Under the old
    /// agreement check — which walked only the claimed slots — that made every
    /// crafting click "agree", so slot 0 was never sent: the screen drew its own dimmed
    /// ghost, clicking it looked dead, and a craft only appeared after close+reopen.
    ///
    /// The control is the second half: a client that *does* claim the right result and
    /// cursor gets no packet, so this is a comparison over the whole menu rather than
    /// an unconditional resend.
    #[test]
    fn a_derived_result_is_pushed_to_the_client_and_an_honest_claim_still_costs_nothing() {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        inventory.click_state_mut().carried = Some(stack("minecraft:oak_planks", 4));

        // Four right-clicks, one plank per cell. **Every one of them changes the derived
        // result** — measured against vanilla's own datapack, one plank alone is
        // `oak_button.json`, two side by side are `oak_pressure_plate.json`, three match
        // nothing, four are `crafting_table.json` — and the client predicts none of
        // them, so each has to be answered.
        for (menu_slot, left_on_cursor) in [(1, Some(3u32)), (2, Some(2)), (3, Some(1)), (4, None)] {
            let (correction, _) = apply_container_clicked(
                &ContainerTagProto,
                &mut inventory,
                &block_entities,
                None,
                0,
                Click { slot: menu_slot, button: 1, click_type: 0 },
                &[(menu_slot, Some(stack("minecraft:oak_planks", 1)))],
                left_on_cursor
                    .map(|count| stack("minecraft:oak_planks", count))
                    .as_ref(),
                false,
                i32::MAX,
                &crate::plugin_crafting::CraftingStationHooks::default(),
            );
            assert!(
                matches!(&correction, Some(ServerDirective::Send { packet_id, .. }) if *packet_id == CONTENT),
                "menu slot {menu_slot} moved the result slot the client cannot derive, got {correction:?}"
            );
        }
        assert_eq!(
            inventory.crafting().result(),
            Some(&stack("minecraft:crafting_table", 1))
        );

        // Now take it. The client's prediction is empty on both counts (its own result
        // slot is still empty), so this is the click that read as dead.
        let (correction, dropped) = apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: 0, button: 0, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(dropped.is_empty());
        assert_eq!(
            inventory.click_state().carried,
            Some(stack("minecraft:crafting_table", 1)),
            "exactly one table, on the cursor"
        );
        assert!(
            inventory.crafting().is_empty(),
            "and one of every input was consumed"
        );
        // `ContainerTagProto` puts the carried count in the last payload byte: the
        // client is told about the cursor on this same packet, which is what "without a
        // reopen" means.
        match &correction {
            Some(ServerDirective::Send { packet_id, payload }) => {
                assert_eq!(*packet_id, CONTENT);
                assert_eq!(payload[0], 0, "window 0");
                assert_eq!(payload[2], 46, "all 46 InventoryMenu slots");
                assert_eq!(payload[3], 1, "carrying one crafting table");
            }
            other => panic!("taking a result must resync the client, got {other:?}"),
        }

        // The control: with the take already applied, a client claiming precisely what
        // the server derived (nothing left in the grid, one table on the cursor) is
        // answered with silence.
        let (correction, _) = apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: 0, button: 0, click_type: 0 },
            &[],
            Some(&stack("minecraft:crafting_table", 1)),
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert_eq!(
            correction, None,
            "an empty result slot and a matching cursor is agreement, not a resend"
        );
    }

    /// Shift-clicking a result crafts **repeatedly** until the grid runs out —
    /// vanilla's `doClick` `QUICK_MOVE` `while` loop over a result slot that
    /// `slotsChanged` refills between rounds.
    ///
    /// Expected value from outside this code: `chest.json` is eight `#minecraft:planks`
    /// around an empty centre, and vanilla's own result-slot on-take routine removes **one** per occupied
    /// cell per craft, so eight planks per cell is exactly eight chests — not one (the
    /// old single-shot behaviour) and not sixty-four.
    #[test]
    fn shift_clicking_the_result_crafts_until_the_grid_runs_out() {
        let mut inventory = PlayerInventory::new();
        inventory.open_table_crafting();
        let block_entities = BlockEntityHandle::new();
        let mut open = OpenContainer {
            window_id: 3,
            pos: BlockPos::new(0, 64, 0),
            shape: MenuKind::CraftingTable,
            container_size: 10,
            state_id: 0,
        };
        for cell in [0, 1, 2, 3, 5, 6, 7, 8] {
            inventory
                .table_crafting_mut()
                .expect("open")
                .set_input(cell, Some(stack("minecraft:oak_planks", 8)));
        }
        assert_eq!(
            inventory.table_crafting().and_then(|g| g.result()),
            Some(&stack("minecraft:chest", 1)),
            "premise: the grid produces one chest per craft"
        );

        let (correction, dropped) = apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            3,
            Click { slot: 0, button: 0, click_type: 1 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(dropped.is_empty(), "36 empty slots have room for 8 chests");
        let chests: u32 = (0..crate::inventory::PLAYER_NATIVE_SIZE)
            .filter_map(|native| inventory.native(native))
            .filter(|s| s.item.to_string() == "minecraft:chest")
            .map(|s| s.count)
            .sum();
        assert_eq!(chests, 8, "eight planks per cell is eight crafts");
        assert!(
            inventory.table_crafting().is_some_and(CraftingState::is_empty),
            "and the grid is empty, not merely one item lighter"
        );
        assert!(
            correction.is_some(),
            "the client cannot predict any of that and must be resynced"
        );
    }

    /// **Control**, and the third reported symptom: shift-clicking an *input* out of
    /// the grid moves that item to the inventory and withdraws the result. It must
    /// never craft — `quickMoveStack`'s grid-cell branch has no `onTake` on the result
    /// container, and vanilla's own result-slot on-take routine is reachable only through slot 0.
    #[test]
    fn shift_clicking_a_grid_input_moves_it_out_without_crafting() {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        for cell in 0..4 {
            inventory
                .crafting_mut()
                .set_input(cell, Some(stack("minecraft:oak_planks", 1)));
        }
        assert!(
            inventory.crafting().result().is_some(),
            "premise: a result is standing when the input is shift-clicked"
        );

        let (_, dropped) = apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: 1, button: 0, click_type: 1 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );

        assert!(dropped.is_empty());
        assert!(inventory.click_state().carried.is_none(), "nothing on the cursor");
        assert!(
            (0..crate::inventory::PLAYER_NATIVE_SIZE)
                .filter_map(|native| inventory.native(native))
                .all(|s| s.item.to_string() == "minecraft:oak_planks"),
            "no crafting table anywhere: taking an input is not a craft"
        );
        let planks: u32 = (0..crate::inventory::PLAYER_NATIVE_SIZE)
            .filter_map(|native| inventory.native(native))
            .map(|s| s.count)
            .sum();
        assert_eq!(planks, 1, "exactly the one plank that left the grid");
        assert_eq!(inventory.crafting().input(0), None, "the cell it came from");
        assert!(
            inventory.crafting().result().is_none(),
            "and the result is withdrawn, not crafted"
        );
    }

    /// **Control**: a click carrying the *wrong* (stale) window id must not
    /// mutate anything — the guard that stops a click for an already-closed
    /// or already-replaced window from landing on whatever is open now.
    #[test]
    fn container_clicked_against_a_stale_window_id_is_dropped() {
        let mut inventory = PlayerInventory::new();
        inventory.click_state_mut().carried = Some(stack("minecraft:coal", 1));
        let block_entities = BlockEntityHandle::new();
        let pos = BlockPos::new(1, 2, 3);
        block_entities.with(|reg| {
            reg.insert(pos, BlockEntity::Furnace(Furnace::new(FurnaceKind::Furnace)));
        });
        let mut open = open(pos, 3); // window_id 7

        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            8, // stale/mismatched window id
            Click { slot: 0, button: 0, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );

        let furnace_input = block_entities.with(|reg| match reg.get(pos) {
            Some(BlockEntity::Furnace(f)) => f.input().cloned(),
            _ => None,
        });
        assert_eq!(furnace_input, None, "a stale window id must not mutate the block entity");
        assert!(
            inventory.click_state().carried.is_some(),
            "and the cursor is untouched"
        );
    }

    // -- brewing stand interaction  --

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

    // -- the composter interaction  --

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
    /// every accepted insert, per its own composter fill routine) but leaves the level —
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
    /// so the ordinary placement path can handle it.
    #[test]
    fn a_non_compostable_item_falls_through_without_touching_anything() {
        let (block_entities, mut inventory, pos, mobs) =
            composter_scene(Composter::new(), Some(stack("minecraft:diamond", 1)));

        let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

        assert_eq!(outcome, ComposterUseOutcome::NotComposter);
        assert_eq!(inventory.native(0), Some(&stack("minecraft:diamond", 1)));
        assert_eq!(composter_level(&block_entities, pos), 0);
    }

    /// An empty hand on a composter below level 8 returns `PASS`, so the
    /// placement logic may place a block on top of the partially filled
    /// composter.
    #[test]
    fn an_empty_hand_on_a_not_ready_composter_falls_through() {
        let (block_entities, mut inventory, pos, mobs) =
            composter_scene(Composter::restore(3, None), None);

        let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

        assert_eq!(outcome, ComposterUseOutcome::NotComposter);
        assert_eq!(composter_level(&block_entities, pos), 3);
    }

    /// A full (level 7, waiting on its scheduled tick) composter consumes the
    /// click without touching the hand at `fillLevel == 7` with nothing to add.
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
        // 1), and it must land at the block's centre with the measured
        // `1.01`-block vertical offset.
        assert_eq!(
            mobs.with(|sim| sim.item_position(1)),
            Some(Vec3::new(4.5, 65.01, 4.5)),
            "the bone meal must spawn just above the composter"
        );
    }

    /// **Control**: extraction reaches the player even with a compostable item
    /// in hand — the item offer fails below level 8 (returns `NotAccepting`)
    /// and the hand-use half extracts without consuming the hand.
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
    /// — the item offer fails the compostability check and the hand-use half
    /// runs without consuming the hand.
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

    /// **The cross-arm invariant the off-centre join violated**, at a centre where
    /// the two hypotheses actually differ.
    ///
    /// Two independent constructions of one square: `join_view_rings` walks
    /// Chebyshev rings and yields offsets, `ViewTracker::new` rasters a
    /// `[-r, r]²` window around an absolute centre. The tracker's set is a *claim
    /// about what the wire sent*, so if the two disagree the tracker suppresses
    /// resends of columns the client never received. The expectation therefore
    /// comes from neither implementation — it is the geometry both are supposed to
    /// be describing.
    ///
    /// `(25, -13)` deliberately: at a centre of `(0, 0)` the offset and absolute
    /// readings coincide exactly, which is why every existing join gate — all of
    /// which spawn at a position flooring to chunk `(0, 0)` — passed throughout.
    #[test]
    fn ring_offsets_plus_the_join_centre_are_the_square_the_view_tracker_seeds() {
        let radius = 9;
        let (cx, cz) = (25, -13);

        let emitted: HashSet<(i32, i32)> = join_view_rings(radius)
            .into_iter()
            .flatten()
            .map(|(dx, dz)| (cx + dx, cz + dz))
            .collect();
        let seeded = ViewTracker::new((cx, cz), radius, radius).loaded;

        assert_eq!(emitted.len(), 361, "radius 9 is 361 columns either way");
        assert_eq!(
            emitted, seeded,
            "the columns the join stream emits must be exactly the ones the tracker \
             records as sent; any difference is a column the client never gets and \
             never gets resent"
        );

        // The control, and it must fail the same assertion: the pre-fix code used
        // the raw offsets as absolute coordinates. Run and observed failing here
        // rather than described, because the *reason* this bug survived is that
        // the difference is invisible at the origin.
        let unshifted: HashSet<(i32, i32)> =
            join_view_rings(radius).into_iter().flatten().collect();
        assert_ne!(
            unshifted, seeded,
            "control failed: raw ring offsets must NOT equal the tracker's square at a \
             non-origin centre — if they do, this test cannot see the defect it exists for"
        );
        // And the reason the control has to be at a non-origin centre at all.
        assert_eq!(
            unshifted,
            ViewTracker::new((0, 0), radius, radius).loaded,
            "at the origin the two readings are identical, which is exactly why every \
             gate that spawns at chunk (0, 0) was blind to this"
        );
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

    /// The yaw → horizontal-facing map is vanilla's own yaw-to-direction conversion
    /// (vanilla's own per-variant direction field table): yaw 0 = south, 90 = west, ±180 = north,
    /// -90 = east, split at the 45° midpoints (the value at which
    /// `floor(yaw / 90 + 0.5) & 3` rolls over). This is the facing a placed
    /// diode then inverts so the block faces the player.
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

    // `gamemode_command_parses_names_aliases_and_ids` was here, testing
    // `parse_gamemode_command` — a hand-rolled string split that has been
    // deleted. `/gamemode` is now a real Brigadier command in
    // `crate::commands::gamemode`, gated against the captured vanilla tree
    // (`crates/protocol/v770/tests/builtin_command_parity.rs`) and driven
    // end-to-end by `tests/builtin_commands.rs`.
    //
    // Worth recording rather than silently dropping: the deleted test asserted
    // that `gamemode c` and `gamemode 1` parse as creative. **26.2 accepts
    // neither.** vanilla's own game-type name lookup is an exact match against the four
    // `getSerializedName` values, so the old parser — and the test that pinned
    // it — were *more* permissive than vanilla. No test could have caught that,
    // because the failure only ever made a command work that should have failed.

    /// The three redstone families keep the full property set the signal model
    /// reads, and everything else falls through to `crate::block_placement`
    /// (whose own tests cover the per-block conventions). The observer is
    /// deliberately **not** inverted: it watches in the player's look direction,
    /// unlike the diodes' single inversion, which makes them face the player.
    #[test]
    fn placed_block_state_faces_diodes_at_the_player_and_observers_with_the_player() {
        let looking = |yaw: Option<f32>| crate::block_placement::PlaceContext {
            target: BlockPos::new(0, 64, 0),
            face: BlockFace::Up,
            cursor: Vec3f {
                x: 0.5,
                y: 0.0,
                z: 0.5,
            },
            yaw,
            pitch: Some(0.0),
        };
        let air = |_: BlockPos| WorldState::from("minecraft:air");
        let state = |block: &str, yaw: Option<f32>| {
            placed_block_state(block, &looking(yaw), air).map(|placed| placed.state)
        };
        // Looking north (yaw 180): a repeater and comparator face the player —
        // south — while an observer watches north.
        assert_eq!(
            state("minecraft:repeater", Some(180.0)),
            Some("minecraft:repeater[facing=south,delay=1,locked=false,powered=false]".to_string())
        );
        assert_eq!(
            state("minecraft:comparator", Some(180.0)),
            Some("minecraft:comparator[facing=south,mode=compare,powered=false,output=0]".to_string())
        );
        assert_eq!(
            state("minecraft:observer", Some(180.0)),
            Some("minecraft:observer[facing=north,powered=false]".to_string())
        );
        // Looking east (yaw -90): a repeater faces west.
        assert_eq!(
            state("minecraft:repeater", Some(-90.0)),
            Some("minecraft:repeater[facing=west,delay=1,locked=false,powered=false]".to_string())
        );
        // Blocks without any orientation keep the bare census name.
        assert_eq!(state("minecraft:dirt", Some(0.0)), None);
        // And no yaw reported yet keeps the bare name for the directional
        // families too.
        assert_eq!(state("minecraft:repeater", None), None);
    }

    // -----------------------------------------------------------------
    // `placement_obstructs_placer` — the server-side obstruction check used by
    // `apply_use_item_on`. A full block cannot be placed through a standing
    // player. These tests exercise the pure geometry directly rather
    // than driving the whole `apply_use_item_on` pipeline, which needs a live
    // `ChunkSource`/`BlockEntityHandle`/`MobHandle` fixture this predicate
    // does not touch.
    // -----------------------------------------------------------------

    /// A full block at the target cell occupied by the player must be refused.
    #[test]
    fn placement_obstructs_placer_refuses_a_full_block_at_the_players_feet() {
        let target = BlockPos::new(0, 64, 0);
        let feet = Vec3::new(0.5, 64.0, 0.5);
        assert!(placement_obstructs_placer(target, "minecraft:stone", feet));
    }

    /// The discriminating arm: a state with an **empty** collision shape must
    /// never be refused, even at the player's own feet — otherwise this is a
    /// blanket "nothing inside the player" rule rather than a real
    /// obstruction test. Empty-shape blocks such as torches, rails, pressure
    /// plates, and redstone dust remain placeable at those coordinates.
    #[test]
    fn placement_obstructs_placer_allows_an_empty_shape_at_the_players_feet() {
        let target = BlockPos::new(0, 64, 0);
        let feet = Vec3::new(0.5, 64.0, 0.5);
        assert!(
            lodestone_data::collision_shapes::collision_boxes(
                lodestone_data::block_states::StateId::new(
                    lodestone_data::block_states::state_id("minecraft:torch").unwrap(),
                ).expect("torch validates")
            )
            .is_empty(),
            "this test's premise: a torch has no collision boxes"
        );
        assert!(!placement_obstructs_placer(target, "minecraft:torch", feet));
    }

    /// Control: the same full block, far from the player, must not be
    /// refused — proves the detector is a real geometric test and not an
    /// unconditional `true`.
    #[test]
    fn placement_obstructs_placer_allows_a_full_block_far_from_the_player() {
        let target = BlockPos::new(50, 64, 50);
        let feet = Vec3::new(0.5, 64.0, 0.5);
        assert!(!placement_obstructs_placer(target, "minecraft:stone", feet));
    }

    /// Boundary control: a full block exactly adjacent to the player (sharing
    /// only a face) must not be refused — two boxes that only touch are not
    /// intersecting, the same strict-inequality convention
    /// `lodestone_shell::sim::placement::block_intersects_player` uses for
    /// the client's own prediction of this rule.
    #[test]
    fn placement_obstructs_placer_allows_a_full_block_touching_but_not_overlapping() {
        let target = BlockPos::new(1, 64, 0);
        // Feet at x=0.5, half-width 0.3: the player's box is x in
        // [0.2, 0.8], which shares the x=1.0 boundary with `target` (x in
        // [1.0, 2.0]) without entering it.
        let feet = Vec3::new(0.5, 64.0, 0.5);
        assert!(!placement_obstructs_placer(target, "minecraft:stone", feet));
    }

    /// The state-shaped case a full-cube approximation gets wrong: a top
    /// slab occupies only the *upper* half of its cell, so a player whose own
    /// box just clears that upper half is not obstructed by it — while an
    /// (otherwise identically positioned) full block still would be. The
    /// slab's real bottom edge is read from the live collision-shape table
    /// rather than assumed, and the player's feet are derived from it
    /// algebraically so the test holds regardless of the shape's exact
    /// height.
    #[test]
    fn placement_obstructs_placer_lets_a_top_slab_clear_the_players_head_where_a_full_block_would_not()
    {
        let target = BlockPos::new(0, 66, 0);
        let top_slab = "minecraft:oak_slab[type=top,waterlogged=false]";
        let id = lodestone_data::block_states::state_id(top_slab)
            .expect("minecraft:oak_slab[type=top,waterlogged=false] is a real 26.2 state");
        let state = lodestone_data::block_states::StateId::new(id).expect("top slab validates");
        let boxes = lodestone_data::collision_shapes::collision_boxes(state);
        assert!(!boxes.is_empty(), "a top slab has real collision geometry");
        let box_min_y = boxes
            .iter()
            .map(|b| b.min[1])
            .fold(f32::INFINITY, f32::min);
        assert!(
            box_min_y > 0.1,
            "a top slab must not fill the bottom half of its cell, got {box_min_y}"
        );
        // Stand so the player's own box top lands just under the slab's real
        // bottom edge (clears the slab) but still inside the target cell
        // (so an equivalently placed full block, whose bottom edge is the
        // cell floor, still hits the player).
        let feet_y = f64::from(target.y) + f64::from(box_min_y) - 1.8 - 0.05;
        let feet = Vec3::new(0.5, feet_y, 0.5);
        assert!(
            !placement_obstructs_placer(target, top_slab, feet),
            "a top slab should clear the player's head here"
        );
        assert!(
            placement_obstructs_placer(target, "minecraft:stone", feet),
            "a full block at the same position should still hit the player's head"
        );
    }

    // -----------------------------------------------------------------
    // `apply_client_command`'s `dimension_reset` out-parameter — the reset for
    // "die in the Nether, respawn to nothing": `encode_respawn` always tells
    // the client `minecraft:overworld` (this crate's one respawn dimension),
    // but nothing reset the *server's* own dimension tracking, so the client
    // was correctly labelled and never sent any terrain for where it landed.
    // -----------------------------------------------------------------

    /// A `ChunkSource` fixture that reports whichever [`crate::dimension::Dimension`]
    /// it was built with — the only thing `apply_client_command` reads off
    /// `source` here (`respawn` is `None`, so the bed branch never runs).
    struct DimensionOnly(crate::dimension::Dimension);

    impl ChunkSource for DimensionOnly {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 256)
        }
        fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:air".to_string()
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_string()
        }
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
        fn dimension(&self) -> Option<crate::dimension::Dimension> {
            Some(self.0)
        }
    }

    /// A [`ChunkSource`] double for [`dimension_scoped_handles`]: answers
    /// `world_registries`/`block_tick_feed` with whatever the test hands it
    /// and nothing else, so the fixture can stand in for a real
    /// `DimensionalSource` sibling without pulling in `crate::integrated`.
    struct HandleStubSource {
        block_entities: Option<BlockEntityHandle>,
        scheduled: crate::scheduled_tick::ScheduledTickHandle,
        block_tick_feed: Option<BlockTickFeed>,
    }

    impl ChunkSource for HandleStubSource {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 256)
        }
        fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:air".to_string()
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_string()
        }
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
        fn world_registries(&self) -> Option<crate::chunk::WorldRegistries> {
            self.block_entities.clone().map(|block_entities| crate::chunk::WorldRegistries {
                block_entities,
                scheduled: self.scheduled.clone(),
                #[cfg(not(target_arch = "wasm32"))]
                player_data: None,
                #[cfg(not(target_arch = "wasm32"))]
                native_storage: None,
            })
        }
        fn block_tick_feed(&self) -> Option<BlockTickFeed> {
            self.block_tick_feed.clone()
        }
    }

    /// No trip at all: both handles fall back to "use the pair you joined
    /// with" — the precondition every arm below builds on.
    #[test]
    fn dimension_scoped_handles_is_none_before_any_trip() {
        let handles = dimension_scoped_handles(None);
        assert!(handles.block_entities.is_none());
        assert!(handles.block_ticks.is_none());
    }

    /// **The discriminating gate for the validated break path.** A sibling with its own
    /// registry and feed must hand back *that exact instance*, not merely
    /// `Some(_)` — proven by writing a marker through the handle this
    /// function returns and reading it back through the sibling source's own
    /// accessor (a second path over the same data, not a restatement).
    #[test]
    fn dimension_scoped_handles_reaches_the_sibling_own_registry_and_feed() {
        let block_entities = BlockEntityHandle::default();
        let scheduled = crate::scheduled_tick::ScheduledTickHandle::default();
        let block_tick_feed = BlockTickFeed::default();
        let sibling: Arc<dyn ChunkSource> = Arc::new(HandleStubSource {
            block_entities: Some(block_entities.clone()),
            scheduled,
            block_tick_feed: Some(block_tick_feed.clone()),
        });

        let handles = dimension_scoped_handles(Some(&sibling));
        let routed_entities = handles
            .block_entities
            .expect("a sibling with its own registry must answer Some");
        let routed_ticks = handles
            .block_ticks
            .expect("a sibling with its own feed must answer Some");

        // Written through the *returned* handle, read back through the
        // sibling's own — if `dimension_scoped_handles` had handed back a
        // fresh default instead of the sibling's real one, this would find
        // nothing.
        let pos = BlockPos::new(11, 60, -4);
        routed_entities.with(|registry| {
            registry.insert(
                pos,
                BlockEntity::Container {
                    id: "minecraft:chest".to_string(),
                    slots: Vec::new(),
                },
            );
        });
        assert!(
            block_entities.with(|registry| registry.get(pos).is_some()),
            "a marker inserted through the routed handle must be visible through the \
             sibling's own — they must be the same instance, not a copy"
        );

        // `ScheduledTick` carries a private `sub_tick_order`, so it is built
        // through a real queue rather than a struct literal — same trick
        // `tick.rs`'s own `one_pending` test helper uses.
        let mut queue: crate::scheduled_tick::ScheduledTickQueue<String> =
            crate::scheduled_tick::ScheduledTickQueue::new();
        queue.schedule(
            (11, 60, -4),
            "minecraft:redstone_wire".to_string(),
            2,
            crate::scheduled_tick::TickPriority::Normal,
        );
        routed_ticks.request_scheduled_ticks(queue.drain_due(u64::MAX, usize::MAX));
        assert_eq!(
            block_tick_feed.drain_scheduled_ticks().len(),
            1,
            "a tick requested through the routed feed must be drained through the \
             sibling's own — same-instance requirement as the registry above"
        );
    }

    /// The negative control proving the positive result above is not
    /// vacuous: a source with **neither** registry nor feed of its own (an
    /// in-memory sibling with no tick loop wired, or the degenerate case) must
    /// fall back to `None` on both — never invent a private default that
    /// silently discards a placement.
    #[test]
    fn dimension_scoped_handles_falls_back_when_the_sibling_has_neither() {
        let sibling: Arc<dyn ChunkSource> = Arc::new(HandleStubSource {
            block_entities: None,
            scheduled: crate::scheduled_tick::ScheduledTickHandle::default(),
            block_tick_feed: None,
        });
        let handles = dimension_scoped_handles(Some(&sibling));
        assert!(handles.block_entities.is_none());
        assert!(handles.block_ticks.is_none());
    }

    /// No saved player at all falls back to the world spawn.
    #[test]
    fn join_position_for_saved_player_is_world_spawn_with_no_save() {
        let spawn = Vec3::new(8.0, 71.0, 8.0);
        assert_eq!(join_position_for_saved_player(None, spawn), spawn);
    }

    /// **The discriminating gate for the "buried in the ground"
    /// report.** The same raw position, saved under two different dimension
    /// tags: the overworld-tagged save is trusted verbatim (predicted to
    /// equal the saved position exactly, not merely "differs from spawn"),
    /// and the Nether-tagged one must fall back to the world spawn rather
    /// than being joined into the overworld as a raw coordinate — which is
    /// exactly the bug a player who died or disconnected in the Nether hit.
    #[test]
    fn join_position_for_saved_player_distrusts_a_non_overworld_position() {
        let spawn = Vec3::new(8.0, 71.0, 8.0);
        // Deliberately not equal to `spawn` on any axis, so a bug that
        // silently returned the wrong constant could not hide.
        let raw = Vec3::new(15.0, 64.0, -3.0);
        let inventory = PlayerInventory::default();

        let overworld_save = crate::player_data::PlayerData::capture(
            raw,
            Rotation::new(0.0, 0.0),
            20.0,
            300,
            GameMode::Survival,
            &inventory,
            crate::experience::PlayerExperience::default(),
            Vec::new(),
            crate::dimension::Dimension::Overworld,
        );
        assert_eq!(
            join_position_for_saved_player(Some(&overworld_save), spawn),
            raw,
            "an overworld-tagged save must be trusted verbatim"
        );

        let nether_save = crate::player_data::PlayerData::capture(
            raw,
            Rotation::new(0.0, 0.0),
            20.0,
            300,
            GameMode::Survival,
            &inventory,
            crate::experience::PlayerExperience::default(),
            Vec::new(),
            crate::dimension::Dimension::Nether,
        );
        assert_eq!(
            join_position_for_saved_player(Some(&nether_save), spawn),
            spawn,
            "a Nether-tagged save must fall back to the world spawn rather than be \
             joined as a raw overworld coordinate"
        );
    }

    /// An unparseable or unknown dimension tag falls back like any other
    /// non-overworld tag instead of being treated as trustworthy.
    #[test]
    fn join_position_for_saved_player_distrusts_an_unparseable_dimension_tag() {
        let spawn = Vec3::new(8.0, 71.0, 8.0);
        let raw = Vec3::new(15.0, 64.0, -3.0);
        let inventory = PlayerInventory::default();
        let mut save = crate::player_data::PlayerData::capture(
            raw,
            Rotation::new(0.0, 0.0),
            20.0,
            300,
            GameMode::Survival,
            &inventory,
            crate::experience::PlayerExperience::default(),
            Vec::new(),
            crate::dimension::Dimension::Overworld,
        );
        save.dimension = "not a real dimension key".to_string();
        assert_eq!(
            join_position_for_saved_player(Some(&save), spawn),
            spawn,
            "an unparseable dimension tag must not be trusted as the overworld"
        );
    }

    /// A protocol double carrying only what `apply_client_command`'s
    /// `action == 0` arm calls — `encode_respawn`, `encode_set_health`,
    /// `encode_air_supply_update` — everything else `unimplemented!()`, so a
    /// call to a method this test does not expect is a panic, not a silent gap.
    struct RespawnOnlyProto;

    impl ServerProtocol for RespawnOnlyProto {
        fn decode(&self, _s: State, _id: i32, _p: &[u8]) -> ServerBound {
            unimplemented!("this test drives the function directly, never through decode")
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
        fn encode_respawn(&self, spawn: Vec3) -> Vec<ServerDirective> {
            let _ = spawn;
            vec![ServerDirective::None]
        }
        fn encode_set_health(&self, _health: f32, _food: i32, _saturation: f32) -> ServerDirective {
            ServerDirective::None
        }
        fn encode_air_supply_update(&self, _air: i32) -> ServerDirective {
            ServerDirective::None
        }
    }

    /// Drives `apply_client_command`'s `PERFORM_RESPAWN` arm directly (it is
    /// private to this module, so an integration test cannot reach it) and
    /// returns what `dimension_reset` ended up holding.
    async fn drive_respawn(away_from_home: bool) -> Option<Vec3> {
        let (client_end, server_end) = lodestone_net::memory_pair();
        let mut conn = Connection::new(server_end);
        let mut state = State::Play;
        let mut vitals = PlayerVitals::default();
        // The precondition every existing respawn test pins too: no health, no
        // air, matching `PlayerVitals::respawn`'s own before-state.
        vitals.kill();
        let mut fall = FallTracker::default();
        let mut teleport_acknowledgements = None;
        let world_spawn = Vec3::new(11.0, 71.0, -4.0);
        let source = DimensionOnly(if away_from_home {
            crate::dimension::Dimension::Nether
        } else {
            crate::dimension::Dimension::Overworld
        });
        let world = crate::world_state::WorldStateHandle::default();
        let mut advancements =
            AdvancementManager::new(Vec::new()).expect("an empty advancement tree is valid");
        let mut client_loaded = true;
        let mut dimension_reset: Option<Vec3> = None;

        apply_client_command(
            &mut conn,
            &RespawnOnlyProto,
            &mut state,
            &mut vitals,
            &mut fall,
            &mut teleport_acknowledgements,
            world_spawn,
            None,
            &source,
            &world,
            &mut advancements,
            Uuid::nil(),
            0, // PERFORM_RESPAWN
            4, // irrelevant here: only the `REQUEST_GAMERULE_VALUES` arm reads it
            away_from_home,
            &mut client_loaded,
            &mut dimension_reset,
        )
        .await
        .expect("the fixture protocol never errors");

        drop(client_end);
        dimension_reset
    }

    /// **Death away from home returns a reset request.** A death away from home must ask the caller to run the same
    /// dimension reset a portal trip home runs — carrying the resolved respawn
    /// position (here the world spawn, since no bed is set), or the connection
    /// loop never re-centres `view`/`join_stream` and the client sits at a
    /// correctly-labelled position with no terrain ever streamed to it.
    #[tokio::test]
    async fn a_death_away_from_home_asks_for_a_dimension_reset() {
        let reset = drive_respawn(true).await;
        assert_eq!(
            reset,
            Some(Vec3::new(11.0, 71.0, -4.0)),
            "a death in the Nether must signal a reset carrying the resolved respawn position"
        );
    }

    /// **The control.** An ordinary death *at* home must not trip the same
    /// signal, or every respawn would pay the forget-chunk/rebuild-join-stream
    /// cost this exists to avoid on the common path. Without the
    /// `away_from_home` gate this assertion fails identically to the one above
    /// passing — proving the gate is load-bearing, not decorative.
    #[tokio::test]
    async fn a_death_at_home_does_not_ask_for_a_dimension_reset() {
        let reset = drive_respawn(false).await;
        assert_eq!(
            reset, None,
            "a same-dimension death must not signal a reset"
        );
    }

    /// `swing_action`'s two real inputs, against vanilla's own
    /// `ClientboundAnimatePacket` constants (`SWING_MAIN_HAND = 0`,
    /// `SWING_OFF_HAND = 3`) rather than the plausible-but-wrong `0`/`1`.
    #[test]
    fn swing_action_maps_hand_to_vanillas_animate_byte() {
        assert_eq!(swing_action(0), 0, "main hand must map to SWING_MAIN_HAND");
        assert_eq!(swing_action(1), 3, "off hand must map to SWING_OFF_HAND, not 1");
    }

    /// The control for the mapping above: malformed input degrades to the
    /// main-hand swing rather than propagating garbage into the wire byte —
    /// the same convention this crate's decode arms already apply.
    #[test]
    fn swing_action_degrades_malformed_input_to_main_hand() {
        assert_eq!(swing_action(2), 0);
        assert_eq!(swing_action(255), 0);
    }

    /// The positive case: a spectator within range of a resolvable player
    /// target gets the camera attached.
    #[test]
    fn spectator_action_resolves_a_nearby_player_target() {
        let registry = PlayerRegistry::new();
        let target = registry.join("Target", Uuid::from_u128(1), Vec3::new(10.0, 64.0, 10.0));
        let result = apply_spectator_action(
            GameMode::Spectator,
            Some(target.entity_id()),
            Some((10.0, 64.0, 11.0)),
            &MobHandle::default(),
            Some(&registry),
        );
        assert_eq!(result, Some(target.entity_id()));
    }

    /// **Control 1.** The identical setup, but not in spectator mode — the
    /// feature is restricted to spectator mode.
    #[test]
    fn spectator_action_does_nothing_outside_spectator_mode() {
        let registry = PlayerRegistry::new();
        let target = registry.join("Target", Uuid::from_u128(1), Vec3::new(10.0, 64.0, 10.0));
        let result = apply_spectator_action(
            GameMode::Survival,
            Some(target.entity_id()),
            Some((10.0, 64.0, 11.0)),
            &MobHandle::default(),
            Some(&registry),
        );
        assert_eq!(result, None, "survival mode must never attach a camera");
    }

    /// **Control 2.** A target far outside the interaction range must not
    /// resolve, proving the range check is load-bearing rather than
    /// decorative (a wrong implementation that ignores distance entirely
    /// would pass every other case here).
    #[test]
    fn spectator_action_rejects_a_target_out_of_range() {
        let registry = PlayerRegistry::new();
        let target = registry.join("Target", Uuid::from_u128(1), Vec3::new(500.0, 64.0, 500.0));
        let result = apply_spectator_action(
            GameMode::Spectator,
            Some(target.entity_id()),
            Some((10.0, 64.0, 11.0)),
            &MobHandle::default(),
            Some(&registry),
        );
        assert_eq!(result, None, "a target 500+ blocks away must not resolve");
    }

    /// **Control 3.** No target on the wire (`OptionalInt` absent) must do
    /// nothing, matching vanilla's own handler, which has no branch for it at
    /// all.
    #[test]
    fn spectator_action_does_nothing_with_no_target() {
        let registry = PlayerRegistry::new();
        let result = apply_spectator_action(
            GameMode::Spectator,
            None,
            Some((10.0, 64.0, 11.0)),
            &MobHandle::default(),
            Some(&registry),
        );
        assert_eq!(result, None);
    }

    /// **Control 4.** An unresolvable id (no mob, no player) must do nothing
    /// rather than attach a camera to a fabricated position.
    #[test]
    fn spectator_action_rejects_an_unresolvable_target() {
        let registry = PlayerRegistry::new();
        let result = apply_spectator_action(
            GameMode::Spectator,
            Some(9999),
            Some((10.0, 64.0, 11.0)),
            &MobHandle::default(),
            Some(&registry),
        );
        assert_eq!(result, None);
    }

    /// Completing a `minecraft:ominous_bottle` use grants
    /// `minecraft:bad_omen` for 120000 ticks at amplifier 0 and consumes the
    /// bottle. The assertion covers both the effect result and the held-item
    /// decrement.
    #[test]
    fn finish_drinking_ominous_bottle_grants_bad_omen_and_consumes_the_bottle() {
        let mut inv = PlayerInventory::new();
        inv.set_native(0, Some(stack("minecraft:ominous_bottle", 1)));
        let mut effects = crate::mob_effects::ActiveEffects::new();
        let started = ItemInUse {
            native: 0,
            item: "minecraft:ominous_bottle".to_owned(),
            finish_tick: 0,
            last_effect_remaining: None,
        };

        assert!(effects.get("minecraft:bad_omen").is_none(), "precondition: no Bad Omen carried yet");
        let result = finish_drinking_ominous_bottle(&mut inv, &mut effects, &started, GameMode::Survival);
        let (native, remainder) = result.expect("a present ominous bottle must finish");
        assert_eq!(native, 0);
        assert!(remainder.is_none(), "the sole stack of 1 must be fully consumed, leaving the slot empty");
        assert_eq!(inv.native(0), None, "the bottle must actually leave the inventory");

        let instance = effects.get("minecraft:bad_omen").expect("Bad Omen must now be carried");
        assert_eq!(instance.amplifier(), 0);
        assert_eq!(instance.duration(), 120_000, "OminousBottleAmplifier.EFFECT_DURATION");
    }

    /// **Control**: any other item (even another drinkable, like a plain
    /// potion) must not grant Bad Omen or be consumed through this path —
    /// this function's whole reason to be separate from [`finish_consuming`]
    /// is that it is a one-item special case, not a general drink handler.
    #[test]
    fn finish_drinking_ominous_bottle_ignores_every_other_item() {
        let mut inv = PlayerInventory::new();
        inv.set_native(0, Some(stack("minecraft:potion", 1)));
        let mut effects = crate::mob_effects::ActiveEffects::new();
        let started = ItemInUse {
            native: 0,
            item: "minecraft:potion".to_owned(),
            finish_tick: 0,
            last_effect_remaining: None,
        };

        let result = finish_drinking_ominous_bottle(&mut inv, &mut effects, &started, GameMode::Survival);
        assert!(result.is_none(), "a plain potion must not be handled by the ominous-bottle path");
        assert!(effects.get("minecraft:bad_omen").is_none(), "no effect must be granted");
        assert_eq!(inv.native(0), Some(&stack("minecraft:potion", 1)), "the potion must stay in hand, untouched");
    }

    fn potion_stack(potion: &str, count: u32) -> ItemStack {
        let mut s = stack("minecraft:potion", count);
        s.components.potion = Some(lodestone_data::potion::potion_id(potion).expect("real potion"));
        s
    }

    /// A real timed-effect potion (Strength II) must
    /// land its full, **unscaled** duration and amplifier on the drinker and
    /// consume the bottle — the whole reason this function exists, since before
    /// it every potion in the game did nothing at all.
    #[test]
    fn finish_drinking_potion_grants_the_full_unscaled_effect() {
        let mut inv = PlayerInventory::new();
        inv.set_native(0, Some(potion_stack("minecraft:strong_strength", 1)));
        let started = ItemInUse {
            native: 0,
            item: "minecraft:potion".to_owned(),
            finish_tick: 0,
            last_effect_remaining: None,
        };

        let (native, remainder, effects) =
            finish_drinking_potion(&mut inv, &started, GameMode::Survival).expect("a real potion must finish");
        assert_eq!(native, 0);
        assert!(remainder.is_none(), "the sole stack of 1 must be fully consumed");
        assert_eq!(
            effects,
            vec![crate::mob_effects::SplashEffect::Timed {
                effect_id: "minecraft:strength".to_owned(),
                duration: 1800,
                amplifier: 1,
            }],
            "Strong Strength: amplifier II, 1:30 — unscaled, not the splash falloff"
        );
    }

    /// An instant potion (Harming) reaches the caller as a full-strength
    /// [`SplashEffect::Instant`] — `6 << amplifier` unscaled, the same
    /// `splash_instant_amount` computation at `scale = 1.0` a direct hit at
    /// point-blank range would produce, proving drinking is not merely "a splash
    /// with the thrower standing on the target".
    #[test]
    fn finish_drinking_potion_carries_an_instant_effect_at_full_strength() {
        let mut inv = PlayerInventory::new();
        inv.set_native(0, Some(potion_stack("minecraft:harming", 1)));
        let started = ItemInUse {
            native: 0,
            item: "minecraft:potion".to_owned(),
            finish_tick: 0,
            last_effect_remaining: None,
        };

        let (_, _, effects) =
            finish_drinking_potion(&mut inv, &started, GameMode::Survival).expect("harming must finish");
        assert_eq!(
            effects,
            vec![crate::mob_effects::SplashEffect::Instant {
                effect_id: "minecraft:instant_damage".to_owned(),
                amount: 6.0,
            }]
        );
    }

    /// **Control**: a water bottle's `minecraft:potion` id resolves (it is a
    /// real potion), but its built-in effect list is empty, so drinking it must
    /// still fully consume the bottle and yield zero grants — not "not handled"
    /// and not a panic on an empty list.
    #[test]
    fn finish_drinking_potion_water_bottle_control() {
        let mut inv = PlayerInventory::new();
        inv.set_native(0, Some(potion_stack("minecraft:water", 1)));
        let started = ItemInUse {
            native: 0,
            item: "minecraft:potion".to_owned(),
            finish_tick: 0,
            last_effect_remaining: None,
        };

        let (_, remainder, effects) =
            finish_drinking_potion(&mut inv, &started, GameMode::Survival).expect("water must still finish");
        assert!(remainder.is_none());
        assert!(effects.is_empty());
    }

    /// An out-of-census component value remains a wire-boundary failure, not
    /// an empty entry in the built-in potion table. The stack still finishes
    /// consuming, but it cannot grant an arbitrary built-in effect.
    #[test]
    fn finish_drinking_potion_rejects_an_unknown_component_id() {
        let mut invalid = stack("minecraft:potion", 1);
        invalid.components.potion = Some(-1);
        let mut inv = PlayerInventory::new();
        inv.set_native(0, Some(invalid));
        let started = ItemInUse {
            native: 0,
            item: "minecraft:potion".to_owned(),
            finish_tick: 0,
            last_effect_remaining: None,
        };

        let (_, remainder, effects) =
            finish_drinking_potion(&mut inv, &started, GameMode::Survival).expect("the stack must finish");
        assert!(remainder.is_none(), "the invalid component does not cancel consumption");
        assert!(effects.is_empty(), "an unknown raw id cannot become a built-in effect");
    }

    /// **Control**: any other item, including food, must not be handled by
    /// this path.
    #[test]
    fn finish_drinking_potion_ignores_every_other_item() {
        let mut inv = PlayerInventory::new();
        inv.set_native(0, Some(stack("minecraft:golden_apple", 1)));
        let started = ItemInUse {
            native: 0,
            item: "minecraft:golden_apple".to_owned(),
            finish_tick: 0,
            last_effect_remaining: None,
        };
        assert!(finish_drinking_potion(&mut inv, &started, GameMode::Survival).is_none());
    }

    /// Drinking milk clears every active effect and
    /// reports exactly the ids that were cleared, and consumes the bucket.
    #[test]
    fn finish_drinking_milk_clears_every_active_effect() {
        let mut inv = PlayerInventory::new();
        inv.set_native(0, Some(stack("minecraft:milk_bucket", 1)));
        let mut effects = crate::mob_effects::ActiveEffects::new();
        effects.apply("minecraft:poison", 100, 0);
        effects.apply("minecraft:speed", 200, 1);
        let started = ItemInUse {
            native: 0,
            item: "minecraft:milk_bucket".to_owned(),
            finish_tick: 0,
            last_effect_remaining: None,
        };

        let (native, remainder, mut cleared) =
            finish_drinking_milk(&mut inv, &mut effects, &started, GameMode::Survival).expect("milk must finish");
        assert_eq!(native, 0);
        assert!(remainder.is_none());
        cleared.sort();
        assert_eq!(cleared, vec!["minecraft:poison".to_owned(), "minecraft:speed".to_owned()]);
        assert!(effects.is_empty(), "every effect must actually be gone");
    }

    /// **Control**: milk drunk with nothing active clears nothing (an empty
    /// `Vec`, not a sentinel) but still consumes the bucket — matching this
    /// crate's own water-bottle-control convention for "ran, and had nothing to
    /// do" versus "did not run".
    #[test]
    fn finish_drinking_milk_with_no_active_effects_control() {
        let mut inv = PlayerInventory::new();
        inv.set_native(0, Some(stack("minecraft:milk_bucket", 1)));
        let mut effects = crate::mob_effects::ActiveEffects::new();
        let started = ItemInUse {
            native: 0,
            item: "minecraft:milk_bucket".to_owned(),
            finish_tick: 0,
            last_effect_remaining: None,
        };

        let (_, remainder, cleared) =
            finish_drinking_milk(&mut inv, &mut effects, &started, GameMode::Survival).expect("milk must finish");
        assert!(remainder.is_none(), "the bucket is still consumed");
        assert!(cleared.is_empty());
    }

    /// `player_overlaps_piston_sweep` verifies the overlap test used for
    /// connection-side piston self-correction. A player standing in either the
    /// source or destination cell must overlap; one standing a full block clear
    /// of both must not.
    #[test]
    fn player_overlaps_piston_sweep_matches_source_and_dest_but_not_clear_ground() {
        let source = BlockPos::new(4, 0, 0);
        let dest = BlockPos::new(5, 0, 0);

        assert!(
            player_overlaps_piston_sweep(5.5, 0.0, 0.5, source, dest),
            "a player standing in the destination cell must overlap"
        );
        assert!(
            player_overlaps_piston_sweep(4.5, 0.0, 0.5, source, dest),
            "a player standing in the source cell must overlap too"
        );
        assert!(
            !player_overlaps_piston_sweep(10.5, 0.0, 0.5, source, dest),
            "control: a player well clear of both cells must not overlap"
        );
    }

    #[test]
    fn only_the_latest_teleport_acknowledgement_releases_movement() {
        let mut acknowledgements = TeleportAcknowledgements::after_initial(41);
        let replacement = acknowledgements.issue();

        assert_eq!(replacement, 42);
        assert!(
            !acknowledgements.accepts(41),
            "a late acknowledgement for the superseded join correction must stay pending"
        );
        assert!(
            acknowledgements.is_pending(),
            "a stale acknowledgement must not clear the newer correction"
        );
        assert!(acknowledgements.accepts(42));
        assert!(
            !acknowledgements.is_pending(),
            "the current acknowledgement must release the movement gate"
        );
        assert!(
            !acknowledgements.accepts(42),
            "a duplicate acknowledgement must not recreate an accepted state"
        );
    }
}
