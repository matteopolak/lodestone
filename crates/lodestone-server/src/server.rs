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
use web_time::Instant;

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
    ResourceKey, Rotation, Text, TextContent, Vec3, Vec3f, WrittenBookContent,
};
use lodestone_data::block_items;
use lodestone_net::{Connection, NetError, Transport};
// Issue #273's encryption half: the server-side RSA keypair/decrypt and the
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
    Abilities, BossBarSnapshot, EntitySnapshot, MerchantOfferOut, ResourcePackPush, ServerBound,
    ServerDirective, ServerProtocol,
};
use crate::redstone::{COMPARATOR, OBSERVER, REPEATER};
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

/// MOTD reported in the server-list status reply (issue #277).
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

/// Issue #325 / `crate::sleep`: the server-side entity id of the single local
/// player in a singleplayer world — the roster key a connection with no
/// [`PlayerRegistry`] uses when it votes (see the sleep-vote inner state's
/// own sleepers doc
/// comment). Matches `crates/protocol/v770/src/server_protocol.rs`'s
/// `LOCAL_PLAYER_ENTITY_ID`, which is what the v770 encoder believes the local
/// player's id is; keeping the two constants equal is the join, and the
/// reason `crate::sleep`'s module doc names this crate as the source.
pub(crate) const LOCAL_PLAYER_ENTITY_ID: i32 = 1;

/// The disconnect reason for an unanswered keep-alive (issue #279).
///
/// Vanilla's is exactly a translatable text component keyed
/// `"disconnect.timeout"`, sent
/// from its own generic "keep connection alive" step, so the key is not ours to choose. The `fallback` is vanilla's own
/// English string for that key, read from
/// `.cache/mc/26.2/client-src/assets/minecraft/lang/en_us.json:3498`
/// (`"disconnect.timeout": "Timed out"`) — not invented here.
///
/// Carrying a fallback at all is a deliberate improvement over vanilla's bare
/// translatable text component, and it is a real vanilla feature, not an extension:
/// vanilla's own translatable-text-content resolver resolves
/// `currentLanguage.getOrDefault(key, fallback)`. So a real client shows
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

/// Vanilla's `multiplayer.disconnect.unverified_username` English text
/// (`assets/minecraft/lang/en_us.json`), sent when the session server's
/// `hasJoined` answers "this client never proved ownership of this
/// username" (issue #273).
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
/// [`ViewTracker::max_radius`] (issue #545).
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

/// A bare-handed player's raw melee damage — vanilla's own player attribute
/// builder's
/// own `.add(Attributes.ATTACK_DAMAGE, 1.0)`, **not** the generic living-entity
/// ranged-attribute default of `2.0` a player would otherwise inherit.
///
/// **No longer what every hit deals.** [`apply_attack`] now resolves the held
/// item through [`lodestone_entity::equipment`]'s real `ATTACK_DAMAGE` modifier
/// fold, so a diamond sword deals `7.0` and this value is what an *empty* hand
/// resolves to — the attribute base with no modifiers on it. It survives as a
/// named constant because the equality "empty hand == this number" is the one
/// thing a gate can check without restating the whole fold, and because
/// [`lodestone_entity::equipment::PLAYER_BASE_ATTACK_DAMAGE`] is the same figure
/// read from the same line of the jar (pinned equal by
/// `bare_hand_damage_is_the_player_attribute_base`).
///
/// Still not modelled: vanilla's own player-attack step's base-damage-scale
/// factor
/// (cooldown-scaled damage) and the critical-hit bonus, because there is no
/// server-tracked attack-strength ticker to read, so every hit is treated as
/// full-strength.
const PLAYER_BARE_HAND_ATTACK_DAMAGE: f32 = 1.0;

/// The melee knockback-bonus power a **sprinting** attacker's hit applies,
/// matching vanilla's own player-attack step's own sprint-knockback bonus exactly:
/// `causeExtraKnockback(entity, this.getKnockback(entity, damageSource) +
/// (knockbackAttack ? 0.5F : 0.0F), ...)`, where
/// `knockbackAttack = this.isSprinting() && fullStrengthAttack`.
/// Vanilla's own "get knockback" getter itself resolves to the attacker's
/// `minecraft:attack_knockback` attribute (registry default `0.0`)
/// — `0.0` for a bare-handed player, since this crate
/// has no weapon/enchantment model to add to it — so a **non-sprinting**
/// attack's total knockback power is exactly `0.0`. That is not a
/// placeholder: it is the literal jar formula for the one case this crate can
/// currently model (no weapon, no server-tracked attack-cooldown ticker so
/// every hit reads as full-strength), and it is why
/// [`apply_attack`] passes `0.0` unconditionally for a non-sprinting attacker
/// and this constant only for a sprinting one.
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
/// (issue #302) — 600, i.e. 30 s at this crate's 20 TPS stand-in.
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
                    // Issue #236: an already-leashed mob must show its rope to a
                    // client that only just spawned it — a client that joined, or
                    // walked back into view range, after the attach — not only to
                    // whoever was connected at attach time. Mirrors the metadata
                    // arm immediately above: sent once here, on spawn, whenever
                    // `leash_link` is already `Some`.
                    if entity.leash_link.is_some() {
                        directives.push(proto.encode_set_entity_link(entity.id, entity.leash_link));
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
                    // Issue #236: covers both a fresh attach (`None` → `Some`) and
                    // a detach (`Some` → `None`) — `encode_set_entity_link` itself
                    // decides how to spell "no holder" on the wire.
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
///
/// A third arm was added for portal travel — see [`Dimension`](Self::Dimension).
///
/// `Debug` is hand-written too, for the same shape of reason `Copy` is:
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
/// **This is the whole fix for the join-dimension routing bug**: before it,
/// `serve_play`'s `block_entities`/`block_ticks` parameters were fixed for the
/// life of the connection, taken from the dimension the player *joined* in —
/// so a furnace lit or a lever flipped while standing in the Nether was
/// recorded into the *overworld's* registry, and a repeater's delayed flip
/// request landed in a feed only the overworld's tick loop ever drained. The
/// caller (`serve_play`'s connection loop, right where `source` itself is
/// already re-derived from `travelled` every iteration) shadows its local
/// `block_entities`/`block_ticks` bindings with these, falling back to the
/// pair it joined with on either field's `None`.
///
/// Each field independently falls back to "use the pair you joined with" —
/// **not** as one combined switch — because `crate::chunk::ChunkSource::world_registries`
/// and `crate::chunk::ChunkSource::block_tick_feed` are two different
/// accessors that can in principle disagree, and collapsing them into one
/// `Option` would make a future asymmetric source (one with a registry but no
/// tick loop, say) silently lose the registry half too. In production today
/// both a `DimensionalSource` built by
/// `crate::integrated::sibling_chunk_source` answers `Some` for both or
/// `None` for neither — see `crate::dimension::DimensionalSource::alone_with_dimension_handles`'s
/// own doc comment — so this test doubles as the discriminator, not just a
/// hypothetical.
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
/// no such dimension (a single-dimension world, which is every world built before
/// `crate::dimension` existed), the destination has no placeable band, or the
/// hosting protocol cannot encode a dimension change. All three are *declines*
/// rather than failures — the player stays where they are, standing in a portal,
/// and nothing is half-applied.
///
/// # The order of the packets is the whole correctness argument
///
/// 1. **Forget every loaded column.** The client keeps chunks in a store nothing
///    else clears — its `Respawned` handler does not, and there is no bulk-clear
///    method on its world sink — so without this the old dimension's columns stay
///    meshed *and* the client's own `world_extent` can go on reporting the old
///    dimension's `min_y`/`height` off an arbitrary leftover column. `forget_chunk`
///    is the one wired path that empties it.
/// 2. **The dimension change pair** (`respawn` + the placement teleport). This is
///    what re-frames the client's chunk window: it resolves the new
///    `dimension_type` holder id and installs that dimension's `min_y` and section
///    count. Every chunk sent before it would be decoded against the old window.
/// 3. **The new cache centre, then the chunks.** Both must follow (2), for the same
///    reason.
///
/// # Why the view tracker is rebuilt rather than recentred
///
/// [`ViewTracker::recenter`] emits a *difference* — the columns that entered and
/// left — which is exactly wrong here: nothing the old dimension sent is reusable,
/// and the new dimension owes the player the entire square. Rebuilding with
/// [`ViewTracker::new`] and handing the whole square to a fresh
/// [`JoinChunkStream`](crate::join_scheduler::JoinChunkStream) is the same shape the
/// join path already uses, and it reuses the same ring order so the ground under the
/// player's feet arrives first.
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
    // The axis a newly built exit portal takes comes from the block the player is
    // standing in — vanilla's `getExitPortal` reads it off `portalEntryPos`, which is
    // why `entry` is threaded this far rather than defaulting to X.
    let source_axis =
        crate::portal::Axis::from_state(&current.get().block_state(entry.x, entry.y, entry.z));
    // # Why the outbound leg is offloaded and the return leg is not
    //
    // `resolve_destination` is synchronous CPU work whose *reads* may each generate a
    // whole column, and the outbound leg is the expensive one by construction: the
    // destination is a dimension nothing has ever looked at, so the site search's
    // 33 × 33 footprint means a dozen columns generated from scratch. Left on the core
    // thread that is measured in seconds, which is a keep-alive timeout rather than a
    // slow frame — the same shape as the join-strip stall (`DESIGN.md` §12.165), and
    // offloading is the fix for the same reason it was there.
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
    let change = proto.encode_dimension_change(to.key(), arrival, game_mode);
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
    // The `ringed` arm rather than `windowed`: it holds coordinates only, so it
    // generates through whichever `SourceRef` the loop hands it — which is the new
    // dimension's from the next iteration onward. A `windowed` pipeline would have
    // captured an `Arc` of the *old* dimension at construction and streamed the
    // wrong world's terrain into the right world's chunk packets.
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

/// Moves a player through an End portal — vanilla's own end-portal-block
/// "get portal destination" step's
/// `fromEnd == false` arm, the End's counterpart to [`travel_through_portal`].
///
/// **Deliberately not a generalisation of [`travel_through_portal`].** An End
/// portal has no coordinate scale, no linked-position search and no fresh
/// portal to build at the far end: [`crate::portal::end_portal_arrival`]
/// names a **fixed** point, and [`crate::portal::ensure_end_platform`] builds
/// (or repairs) the obsidian platform there before the chunk stream below can
/// reach it. Reusing the Nether's destination search here would run
/// `PortalForcer`'s site-scoring logic over the End's own terrain and could
/// as easily strand a player over the void as land them on solid ground.
///
/// The packet sequence — forget every loaded column, the dimension-change
/// pair, the new cache centre, the rebuilt view and join stream — is
/// otherwise identical to [`travel_through_portal`]'s, because it is the same
/// client-side contract regardless of which portal type triggered it; see
/// that function's own doc comment for why each step is ordered the way it
/// is.
///
/// **The return trip (`fromEnd == true`) is not implemented.** It needs the
/// stronghold's exit portal and the dragon fight, neither of which exists in
/// this crate yet. This function's only caller does not reach it while the
/// player is already in the End — the same "correct degradation" this
/// function's own `None` arm below uses for a world with no End sibling
/// wired, rather than a panic or a silently wrong destination.
async fn travel_through_end_portal<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    // The source this connection **joined** with — where the End sibling is
    // reached from, exactly as in `travel_through_portal`.
    home: SourceRef<'_, S>,
    state: &mut State,
    view: &mut ViewTracker,
    join_stream: &mut crate::join_scheduler::JoinChunkStream<S>,
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

    let change = proto.encode_dimension_change(to.key(), arrival, game_mode);
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
    /// [`set_view_radius`](Self::set_view_radius) (issue #270's
    /// `ServerBound::ClientInformationChanged`). Stored on `self` rather than
    /// re-passed at every [`recenter`](Self::recenter) call so a client's
    /// requested distance actually sticks across subsequent moves, instead
    /// of being silently overwritten by the original radius on the next
    /// `PlayerMoved`.
    radius: i32,
    /// The largest radius this connection is **permitted** to reach, and the
    /// ceiling [`set_view_radius`](Self::set_view_radius) clamps a client
    /// request to — vanilla's own chunk-map clamp,
    /// `Mth.clamp(player.requestedViewDistance(), 2, this.serverViewDistance)`.
    ///
    /// **Issue #545: this is a second field precisely because it is a second
    /// question.** `radius` above is where the connection *starts*; this is how
    /// far it may *go*. They were one value, so the ceiling for a live change was
    /// the radius the client happened to join with — lowering render distance
    /// mid-session worked and raising it silently did nothing, which is exactly
    /// the owner's report. Vanilla clamps against `serverViewDistance`, a server
    /// setting, never against the player's current view.
    ///
    /// Who supplies it is a per-path memory-policy decision, the same fork
    /// `ChunkStore::for_view_radius` vs `for_integrated_view_radius` already
    /// encodes: singleplayer (`open_in_memory*`) passes
    /// [`MAX_CLIENT_VIEW_RADIUS`] because it is the slider-mover's own memory,
    /// while open-to-LAN (`IntegratedServer::bind`) passes its configured
    /// `view_radius` because it spends an operator's memory on behalf of players
    /// who did not choose the setting. Every other caller passes `view_radius`,
    /// which is exactly the old behaviour.
    max_radius: i32,
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
    /// The columns that left the view — the same set `immediate`'s
    /// `encode_forget_chunk` directives name, kept as coordinates as well so
    /// [`send_view_update`] can withdraw any of them still queued in the column
    /// stream. See [`crate::join_scheduler::ColumnPipeline::cancel`] for why an owed
    /// column can outlive its own forget.
    forgotten: HashSet<(i32, i32)>,
    /// The newly-visible columns, in wire order — empty when nothing new entered
    /// the view.
    ///
    /// **Coordinates, not directives, and that is the fix rather than a
    /// refactor.** This used to be a finished
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

    /// Every column in `next` this tracker has not already sent, in wire order —
    /// empty if there is nothing new. Shared by [`recenter`](Self::recenter) and
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
        // **Ordered nearest-first, not lexicographically.** It used to be a bare
        // `sort_unstable()`, i.e. by `cx` then `cz` — a raster walk, so a player
        // walking east got the newly-visible column strip filled from its
        // northern end regardless of where along it they actually were. The same
        // key the join stream uses (`join_scheduler::view_order_key`: distance
        // from the player's column first, the cone they are looking down second)
        // makes a *move* behave like a join, which is what the owner asked for —
        // "if the player moves it should properly generate the closer chunks
        // first". Still a total order over integers, so the wire order stays a
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
    /// player having moved at all (issue #270's
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
        // Issue #545: the clamp lives here, against
        // [`max_radius`](Self::max_radius), and **not** at the call site against
        // the connection's current radius — which is the bug. The floor is `0`,
        // not vanilla client UI's slider minimum of `2` (`Options::renderDistance`):
        // the server side has no evidence pinning that specific floor, and a floor
        // above the ceiling would be actively wrong on a connection served with a
        // smaller radius than that (several tests in this crate use
        // `view_radius: 0`). `.max(0)` on the upper bound only guards `clamp`'s own
        // `min <= max` invariant against a negative `max_radius`.
        let radius = radius.clamp(0, self.max_radius.max(0));
        if radius == self.radius {
            return ViewUpdate::default();
        }

        // Issue #551: resize the retention bound *before* streaming the new view,
        // not after. `ChunkStore`'s capacity used to be fixed at construction from
        // the radius the connection joined with, so raising render distance
        // mid-session over-subscribed the cache — and because `join_view_rings`
        // streams outward, the LRU victim is the **innermost** ring, i.e. the
        // ground under the player's feet at ~909 ms a column to regenerate. Doing
        // it first means the columns reported below are never evicted before they
        // are generated. A no-op for every source that retains nothing per view;
        // see `ChunkSource::set_retention_radius`.
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
/// player's own column — issue #453.
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
/// centre before any of it reaches `encode_chunk` or a chunk source. It did not,
/// for a while: the square that went out was centred on chunk `(0, 0)` while
/// `ViewTracker::new` seeded its `loaded` set around the player's actual column,
/// so a player restored away from the origin got terrain in the wrong place,
/// never got the ground under their feet, and had a tracker that believed
/// otherwise. Adding the centre is what makes this "the same set
/// `ViewTracker::new` seeds itself with, in a different order" — a property of
/// the call site, not of this function.
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
/// Fixing the second half needs a *scheduler*, not an order, and since Unit 10
/// that is `crate::join_scheduler`: the caller flattens these groups and drives a
/// primed sliding window over the result, so the first chunk still reaches the
/// client after **one** column of generation instead of 361 while nothing waits
/// on a ring boundary. This function is therefore now purely the **wire order**
/// — the grouping survives because it is the clearest statement of that order and
/// because `join_view_rings_partitions_the_square_exactly` gates it, not because
/// anything synchronises per group.
///
/// `ViewTracker::build_batch` — the *move*-time counterpart — now orders on the
/// same distance-first key rather than its old lexicographic `sort_unstable`, so
/// walking into new terrain fills nearest-first exactly like joining does. That is
/// a change of key, not a loss of determinism: it is still a total order over
/// integers derived from the player's pose.
///
/// Vanilla spirals outward for the same reason, and its priority *is* the ticket
/// level (vanilla's own chunk-task dispatcher), so there is no separate heuristic
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
/// One column. Before Unit 10 the caller waited on every ring boundary, so all
/// the rings smaller than `available_parallelism` — 0 through 2 — left most
/// worker threads idle, and every ring paid its slowest column's tail rather
/// than its mean. `crate::join_scheduler` keeps exactly the first column of that
/// trade (ring 0 is generated alone, which is what buys the one-column
/// time-to-first-chunk) and deletes the rest: from the second column onward the
/// in-flight window spans ring boundaries freely.
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
/// The join used to be a single `begin`/…/`end` pair around the whole view, and
/// that could not survive a stream that spans ticks: the pair would have to stay
/// open across everything else the play loop sends. So the deferred half is
/// batched, which is vanilla's own shape — `ChunkBatchSizeCalculator` exists
/// precisely to pace a stream of batches, and our own client answers each
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
/// **Preferred: hand them to `stream`.** `serve_play` already drains a
/// [`JoinChunkStream`](crate::join_scheduler::JoinChunkStream) from a `select!`
/// branch one column at a time, so enqueueing there means a chunk-boundary crossing
/// costs the connection task one set difference and returns — the strip is generated
/// on the blocking pool with a primed window, re-keyed as the player keeps moving,
/// and interleaved with every read and write this connection owes. The old shape
/// awaited the whole strip inside `ViewTracker::recenter`: one suspension point over
/// `2r + 1` columns during which nothing was read and nothing was written, including
/// the client's keep-alive reply. That is the steady-state half of the owner's
/// latency report, and it is the half the join fix did not cover.
///
/// **Fallback: build the batch here.** `stream` refuses on its `Ringed` arm (a
/// borrowed, non-`'static` source — protocol tests) and when the caller has no
/// stream at all (`wasm32`, whose loop has no `select!` to drain one from). Those
/// take the unchanged path: generate, encode, and send under
/// `awaiting_chunk_batch_ack`, the one-batch-in-flight gate
/// `ServerBound::ChunkBatchAcknowledged` closes — vanilla's `PlayerChunkSender`
/// shape.
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
    let offloaded = match source {
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
    };
    match offloaded {
        Some(frames) => batch.extend(frames),
        None => {
            let columns = source.generate(update.added.clone()).await;
            for (&(x, z), column) in update.added.iter().zip(columns.iter()) {
                batch.push(proto.encode_chunk(x, z, column));
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
    /// terminated (issue #277). **Not a failure**: vanilla itself ends a status
    /// connection exactly this way, and calls it a disconnect —
    /// vanilla's own status packet listener closes the channel with reason
    /// `multiplayer.status.request_handled` after answering a ping, and also
    /// after a *second* status request on one connection.
    ///
    /// It is an `Err` rather than an `Ok` only because [`ServeSummary`] is
    /// shaped around a session that logged in: a status connection has no
    /// username, no chunks, and no inventory, so there is nothing truthful to
    /// put in one. Callers discard the result either way (see
    /// [`crate::IntegratedServer`]'s accept loops).
    #[error("server-list status request handled; connection closed (not an error)")]
    StatusRequestHandled,
    /// The client presented a username vanilla's own server would refuse
    /// (vanilla's own "is valid player name" check — see [`is_valid_player_name`]), and was
    /// sent a login-phase disconnect explaining so (issue #279).
    #[error("login rejected: invalid username")]
    InvalidUsername,
    /// The client was refused by the access lists (issue #336) — banned, IP
    /// banned, not whitelisted, or the server was full — and was sent a
    /// login-phase disconnect carrying vanilla's own translation key.
    ///
    /// Native-only in practice: `crate::access` is `cfg`-gated off on `wasm32`,
    /// where there is no filesystem to hold the lists and no remote player to
    /// refuse.
    #[error("login rejected: {0}")]
    AccessDenied(String),
    /// The client's RSA-encrypted verify-token echo did not match the
    /// challenge the server generated (issue #273) — either tampering, or a
    /// client answering a stale `EncryptionRequest` after the server moved
    /// on. Vanilla's equivalent is its own serverbound key-packet's own
    /// "is challenge valid" check
    /// returning false, treated as a hard protocol error there too.
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
/// (issue #273's remaining half). Passed as `Some` to opt a connection into
/// online mode; every pre-existing entry point in this file continues to
/// pass `None` (offline, this crate's behaviour before this change) so no
/// caller's wire behaviour changes underneath it.
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

    /// Substitutes a fixture for the real session-server call — meant for
    /// tests, though not `#[cfg(test)]`-gated (see below for why).
    ///
    /// This exists because of the exact hazard `CLAUDE.md` records for the
    /// client-side half of this same handshake — a pre-existing unit test
    /// that would otherwise reach a real external service the moment
    /// online-mode auth got wired into a code path tests already call. There
    /// is no way to hermetically test "a real vanilla client joins in online
    /// mode" without either mocking HTTP or making the verification step
    /// substitutable; this crate has no HTTP-mocking dependency, so this is
    /// the injection seam instead.
    ///
    /// **Not `#[cfg(test)]`**, deliberately, even though every current caller
    /// is a test: the login-sequence test that needs it
    /// (`tests/online_mode.rs`) drives the real [`V770ServerProtocol`] from
    /// `lodestone-v770`, which has a *normal* dependency on this crate for the
    /// `ServerProtocol` trait. Adding `lodestone-v770` as a *dev*-dependency
    /// here (so a `#[cfg(test)] mod tests` unit test could reach it) makes
    /// this crate's own lib-test compilation and the copy `lodestone-v770`
    /// links against two different instantiations of the same trait —
    /// measured: `V770ServerProtocol: ServerProtocol is not implemented`
    /// against the crate's own trait. An external `tests/*.rs` binary has no
    /// such self-reference (it depends on this crate exactly once, normally),
    /// so that is where the test using this constructor lives, and it needs
    /// `pub`. Which constructor a *caller* chooses (`new` vs this one) is
    /// still an assertable, compile-time-visible fact — the same property
    /// `CLAUDE.md` asks a `#[cfg(test)]` fork for — it just isn't spelled with
    /// that attribute here.
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

/// This world's per-player `.dat` store, if it has one (issue #302).
///
/// One accessor rather than the same `world_registries().and_then(...)` chain at
/// three call sites, because the failure mode of getting it wrong is invisible:
/// a chain that returns `None` where a store exists produces a server that joins,
/// plays and saves nothing, with no error and no failing test — the island shape
/// this repo's first rule is about.
#[cfg(not(target_arch = "wasm32"))]
fn player_store<S: ChunkSource + ?Sized>(source: &S) -> Option<crate::player_data::PlayerDataStore> {
    source
        .world_registries()
        .and_then(|registries| registries.player_data)
}

/// Writes this connection's live state to its `.dat` file (issue #302).
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
/// **This is the fix for "I respawn buried in the ground."** A saved position
/// is trusted verbatim only when [`PlayerData::dimension`](crate::player_data::PlayerData)
/// says it was actually recorded in the overworld — this crate's one join
/// dimension (a fresh connection always joins there, before any portal
/// trip). Before this existed, a player who died or disconnected in the
/// Nether had their Nether-relative position saved, and the next join used
/// that raw coordinate as an *overworld* one — a semi-arbitrary point that is
/// rarely at overworld surface height, since the Nether's own placeable band
/// and its 8:1 coordinate scale share nothing with overworld terrain shape at
/// the same numbers. See [`crate::player_data::PlayerData::capture`]'s own
/// doc comment for the write-side half of this fix.
///
/// [`crate::dimension::Dimension::from_key`] returning `None` — an
/// unparseable tag, which is every save written before this field was
/// captured accurately — degrades the same way a genuinely non-overworld tag
/// does: fall back to the world spawn rather than trust a position whose
/// dimension is not actually known. This does not retroactively repair an
/// already-corrupted save (the true dimension of an old save's raw position
/// is not recoverable), but it stops the bug for every save written from here
/// on, and it is the same "correct degradation" shape used throughout this
/// crate for a gap rather than inventing a wrong answer.
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
    // rather than a label: a Nether-relative position saved under the wrong
    // (or no) dimension tag is what the next join used to place a player
    // inside overworld terrain at a semi-arbitrary point.
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
fn encode_column<P: ServerProtocol>(
    proto: &P,
    cx: i32,
    cz: i32,
    payload: crate::join_scheduler::ColumnPayload,
) -> ServerDirective {
    match payload {
        crate::join_scheduler::ColumnPayload::Encoded(directive) => directive,
        crate::join_scheduler::ColumnPayload::Column(column) => proto.encode_chunk(cx, cz, &column),
    }
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
    // Issue #545. Forwarded rather than defaulted to `view_radius`, because this
    // is one of the two entry points a caller with its own memory policy uses —
    // `IntegratedServer::open_in_memory*` passes [`MAX_CLIENT_VIEW_RADIUS`] here
    // so the slider can actually be raised mid-session. See
    // `ViewTracker::max_radius`.
    max_view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    // Issue #619. `IntegratedServer`'s real handle, from `ChunkStore::tickets()` —
    // this is one of the entry points a real join reaches, so unlike most
    // parameters in this file it must NOT default to a fresh, disconnected
    // handle. See `serve_connection_inner`'s own parameter comment.
    tickets: &TicketStoreHandle,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    // Issues #327/#328/#323: a fresh, unshared store — the compatibility shape
    // this file uses for every feed. Observably identical to a shared one until
    // something else writes to it, which for this entry point is nothing.
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
        // Issue #325: a fresh vote/feed no tick loop reads — see
        // `serve_connection_inner`'s parameter comments.
        &SleepVote::default(),
        &SleepFeed::default(),
        &CommandDispatch::none(),
        &BorderFeed::default(),
        &ResourcePackPushFeed::default(),
        &PluginChannelRegistry::default(),
        world,
        &crate::live_save::LiveSaveSlot::default(),
        // Issue #336: the inert default — admits everybody, ops nobody.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Issue #273: offline, this wrapper's behaviour before online mode
        // existed. `serve_connection_with_online_mode` is the one entry point
        // that passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`serve_connection_with_mob_events`], but generating chunks off the core
/// thread (issue #293) — the [`serve_connection_shared`] treatment applied to
/// the feed-carrying entry point.
///
/// **Stale as of the command-dispatch landing**: this was
/// [`crate::IntegratedServer::open_in_memory_with_mobs`]'s (singleplayer)
/// entry point when written; that constructor now calls
/// [`serve_connection_with_mob_events_and_commands_shared`] instead (grep
/// confirms zero callers of this function anywhere, prod or test), so every
/// comment elsewhere in this file that still names this function as *the*
/// feed-carrying/singleplayer example is describing this function's shape,
/// not naming today's live call site.
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
    // Issue #545, and this is the entry point where the fork actually *matters*:
    // both `IntegratedServer::open_in_memory_with_mobs*` (uncapped — passes
    // [`MAX_CLIENT_VIEW_RADIUS`]) and `IntegratedServer::bind` (open-to-LAN —
    // passes its configured `view_radius`) come through here, so the ceiling
    // cannot be derived locally. See `ViewTracker::max_radius`.
    max_view_radius: i32,
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
    // Issues #326/#580: the shared world border — the same handle
    // `crate::tick::run_tick_loop_with_weather` now ticks and a `/worldborder`
    // command mutates through `BorderFeed::with`. This is the **only**
    // border-carrying entry point; every other `serve_connection_inner`
    // caller still passes a fresh `BorderFeed::default()` no loop reads, the
    // same compatibility shape `sleep_vote`/`sleep_feed` above already use.
    border: &BorderFeed,
    // Issues #327/#328/#323: the world's shared scalars, the *same* handle
    // `run_tick_loop` ticks. See `serve_connection_inner`'s parameter comment.
    world: &crate::world_state::WorldStateHandle,
    // The player-save mirror `serve_play` publishes to every loop iteration —
    // see `crate::live_save::LiveSaveSlot`'s own doc comment. `bind`'s
    // per-connection LAN wrapper passes a fresh, connection-local
    // `LiveSaveSlot::default()` (a shared one would mix different players'
    // saves under concurrent LAN connections); `open_in_memory_with_mobs`'s
    // singleplayer connection passes the one handle `IntegratedServer::
    // shutdown` reads back, which is the whole point of threading this
    // through rather than defaulting it like every other feed above.
    live_save: &crate::live_save::LiveSaveSlot,
    // Issue #619. `IntegratedServer`'s real handle — see
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
        // Issue #336: the inert default — admits everybody, ops nobody.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Issue #273: offline, this wrapper's behaviour before online mode
        // existed. `serve_connection_with_online_mode` is the one entry point
        // that passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`serve_connection`], plus the host's access lists and this connection's
/// remote address (issue #336).
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
        // Issue #273: this wrapper predates online mode; offline, same as
        // every other pre-existing entry point.
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
        // Issue #273: this wrapper predates online mode; offline, same as
        // every other pre-existing entry point.
        None,
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
    // Issue #619. `IntegratedServer`'s real handle — see
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
    // Issue #535. The three host-supplied surfaces every other constructor
    // hardcodes to `::default()`. `IntegratedServer::open_to_lan` is the one
    // caller that can actually carry a configured one, which is why they are
    // parameters here and nowhere else.
    resource_packs: &ResourcePackPushFeed,
    plugin_channels: &PluginChannelRegistry,
    // Issues #327/#328/#323: the world's shared scalars, the *same* handle
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
        // Issue #273: this function's signature is fixed for the same reason
        // its own doc comment gives for not gaining a tenth parameter earlier
        // — its one caller lives in `integrated.rs`. Offline, same as every
        // pre-existing entry point; `serve_connection_with_online_mode` below
        // is the `_and_commands_shared`-shaped sibling that passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// [`serve_connection_with_mob_events_and_commands_shared`], plus online-mode
/// encryption and session-server verification (issue #273) — the
/// `_and_commands_shared`-shaped sibling promised by that function's own
/// `online_mode` argument comment.
///
/// A **new** entry point rather than a widened
/// [`serve_connection_with_mob_events_and_commands_shared`], for the same
/// reason every other capability in this file gets one: that function's one
/// caller lives in `integrated.rs`, outside this change's reach, and a
/// changed signature would break it from the outside. Wiring a real dedicated
/// host up to online mode is therefore whoever owns `integrated.rs` calling
/// this instead of that — a purely additive swap, not a signature change —
/// once they choose to. Until then this is reachable for testing (see
/// `crates/protocol/v770/tests/`, following the precedent
/// `serve_connection_with_resource_pack`/`serve_connection_with_plugin_channels`
/// already set for a capability with no production wiring yet) and for any
/// other caller that wants online mode directly.
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
    // Issue #619. `IntegratedServer`'s real handle — see
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
    // Issues #327/#328/#323: a fresh, unshared store — the compatibility shape
    // this file uses for every feed. Observably identical to a shared one until
    // something else writes to it, which for this entry point is nothing.
    let world = &crate::world_state::WorldStateHandle::default();
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        // Issue #545: the join radius is also the ceiling here — this wrapper
        // serves no path with its own capacity policy. See `ViewTracker::max_radius`.
        view_radius,
        block_entities,
        mobs,
        &TicketStoreHandle::default(),
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
        world,
        &crate::live_save::LiveSaveSlot::default(),
        // Issue #336: the inert default — admits everybody, ops nobody.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Issue #273: offline, this wrapper's behaviour before online mode
        // existed. `serve_connection_with_online_mode` is the one entry point
        // that passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
    .await
}

/// Like [`serve_connection_with_block_ticks`], but also forwards every
/// detonation published on `explosions` (issue #425: `MobSim::tick` calling
/// `MobSim::explode` the tick a creeper's fuse completes) to this
/// connection, as a real `EXPLODE` packet — through the same
/// `container_sync_tick` timer arm that already forwards `block_ticks`'
/// changes. When production called this directly, the one caller was
/// [`crate::IntegratedServer::open_in_memory_with_mobs`], the only
/// constructor that spawns a [`MobSim`]-driven tick loop capable of
/// producing a detonation in the first place — see the next section for why
/// that is now history rather than the live wiring.
///
/// # Currently unused, and deliberately kept
///
/// Issue #293 moved both production callers (`crate::integrated`) to
/// [`serve_connection_with_mob_events_shared`], so nothing calls this today —
/// and because `mod server` is private and `lib.rs` does not re-export it,
/// nothing outside the crate can. (That `_shared` entry point has itself
/// since been superseded by
/// [`serve_connection_with_mob_events_and_commands_shared`] — see its own
/// "Stale as of the command-dispatch landing" note — but the reasoning below
/// for keeping *this* function around is unaffected by which `_shared`
/// variant production currently calls.) It is retained rather than deleted
/// because it is the borrow-shaped twin of the `_shared` entry point: the one
/// way to drive the *feed-carrying* path down [`SourceRef::Borrowed`], i.e.
/// #293's negative control with block ticks and explosions attached. Delete
/// it only together
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
    // Issues #327/#328/#323: a fresh, unshared store — the compatibility shape
    // this file uses for every feed. Observably identical to a shared one until
    // something else writes to it, which for this entry point is nothing.
    let world = &crate::world_state::WorldStateHandle::default();
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        // Issue #545: the join radius is also the ceiling here — this wrapper
        // serves no path with its own capacity policy. See `ViewTracker::max_radius`.
        view_radius,
        block_entities,
        mobs,
        &TicketStoreHandle::default(),
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
        world,
        &crate::live_save::LiveSaveSlot::default(),
        // Issue #336: the inert default — admits everybody, ops nobody.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Issue #273: offline, this wrapper's behaviour before online mode
        // existed. `serve_connection_with_online_mode` is the one entry point
        // that passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
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
    // Issues #327/#328/#323: a fresh, unshared store — the compatibility shape
    // this file uses for every feed. Observably identical to a shared one until
    // something else writes to it, which for this entry point is nothing.
    let world = &crate::world_state::WorldStateHandle::default();
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        // Issue #545: the join radius is also the ceiling here — this wrapper
        // serves no path with its own capacity policy. See `ViewTracker::max_radius`.
        view_radius,
        block_entities,
        mobs,
        &TicketStoreHandle::default(),
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
        world,
        &crate::live_save::LiveSaveSlot::default(),
        // Issue #336: the inert default — admits everybody, ops nobody.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Issue #273: offline, this wrapper's behaviour before online mode
        // existed. `serve_connection_with_online_mode` is the one entry point
        // that passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
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
    // Issues #327/#328/#323: a fresh, unshared store — the compatibility shape
    // this file uses for every feed. Observably identical to a shared one until
    // something else writes to it, which for this entry point is nothing.
    let world = &crate::world_state::WorldStateHandle::default();
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        // Issue #545: the join radius is also the ceiling here — this wrapper
        // serves no path with its own capacity policy. See `ViewTracker::max_radius`.
        view_radius,
        block_entities,
        mobs,
        &TicketStoreHandle::default(),
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
        world,
        &crate::live_save::LiveSaveSlot::default(),
        // Issue #336: the inert default — admits everybody, ops nobody.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Issue #273: offline, this wrapper's behaviour before online mode
        // existed. `serve_connection_with_online_mode` is the one entry point
        // that passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
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
    // Issues #327/#328/#323: a fresh, unshared store — the compatibility shape
    // this file uses for every feed. Observably identical to a shared one until
    // something else writes to it, which for this entry point is nothing.
    let world = &crate::world_state::WorldStateHandle::default();
    serve_connection_inner(
        conn,
        proto,
        SourceRef::Borrowed(source),
        entities,
        view_radius,
        // Issue #545: the join radius is also the ceiling here — this wrapper
        // serves no path with its own capacity policy. See `ViewTracker::max_radius`.
        view_radius,
        block_entities,
        mobs,
        // Issue #619: a fresh, disconnected store — this wrapper's whole
        // compatibility shape (see every other feed just below), and its own
        // doc names `serve_connection_with_plugin_channels` as a secondary
        // entry point rather than one of `IntegratedServer`'s real join
        // paths.
        &TicketStoreHandle::default(),
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
        world,
        &crate::live_save::LiveSaveSlot::default(),
        // Issue #336: the inert default — admits everybody, ops nobody.
        #[cfg(not(target_arch = "wasm32"))]
        &crate::access::AccessHandle::default(),
        #[cfg(not(target_arch = "wasm32"))]
        None,
        // Issue #273: offline, this wrapper's behaviour before online mode
        // existed. `serve_connection_with_online_mode` is the one entry point
        // that passes `Some`.
        #[cfg(not(target_arch = "wasm32"))]
        None,
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
    // Issue #545. The largest radius this connection may later raise itself to,
    // which is a different question from the `view_radius` it joins with — see
    // `ViewTracker::max_radius` for the per-path policy and why one value could
    // not do both jobs. Every wrapper above except the two `*_shared` ones
    // passes `view_radius` here, which is exactly the pre-#545 behaviour.
    max_view_radius: i32,
    block_entities: &BlockEntityHandle,
    mobs: &MobHandle,
    // Issue #619. The chunk-ticket graph's shared handle — same compatibility
    // shape as every feed below: every pre-existing entry point passes a
    // fresh, disconnected `TicketStoreHandle::default()` (so no off-limits
    // call site's residency behaviour changes), and the production `_shared`
    // wrappers `IntegratedServer` uses carry the *same* handle
    // `ChunkStore::tickets()` returns — the whole point, since a ticket
    // granted into a store nobody else reads means nothing. See
    // `docs/chunk-tickets.md`'s "Open work" for why this parameter, rather
    // than a new `ChunkSource` trait method or a changed public signature,
    // is how the connection reaches it.
    tickets: &TicketStoreHandle,
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
    // Issues #327/#328/#323. The world's shared scalars -- game rules, difficulty
    // and the clock. Same compatibility shape as every feed above: each
    // pre-existing entry point passes a fresh `WorldStateHandle::default()`, so no
    // off-limits call site broke, and the two `_shared` wrappers
    // `IntegratedServer` uses carry the *same* handle `run_tick_loop` ticks. That
    // sharing is the whole point: a per-connection store is the bug both #327 and
    // #328 were reported for.
    world: &crate::world_state::WorldStateHandle,
    // The continuously-refreshed player-save mirror `serve_play` publishes to
    // every loop iteration — see [`crate::live_save::LiveSaveSlot`]'s own
    // doc comment for the shutdown-cancellation race it exists to survive.
    // Same compatibility shape as every feed above: each pre-existing entry
    // point passes a fresh `LiveSaveSlot::default()`, which nothing reads;
    // `serve_connection_with_mob_events_shared` (singleplayer) carries the
    // one `IntegratedServer::shutdown` reads back.
    live_save: &crate::live_save::LiveSaveSlot,
    // Issue #336. Ops, whitelist and the two ban lists, consulted once at
    // `LoginStart`. Same compatibility shape as every feed above: each
    // pre-existing entry point passes a fresh, empty `AccessHandle::default()`,
    // which admits everybody and makes nobody an operator — the singleplayer
    // shape, and the one that cannot lock a player out of their own world. A
    // *host* opts in through `LanConfig::access`.
    #[cfg(not(target_arch = "wasm32"))] access: &crate::access::AccessHandle,
    // The remote address this connection came from, for the IP ban list. `None`
    // for an in-memory duplex, which has no address — and an IP ban therefore
    // cannot apply to singleplayer, which is correct rather than a gap.
    #[cfg(not(target_arch = "wasm32"))] peer_ip: Option<std::net::IpAddr>,
    // Issue #273. `None` (every pre-existing entry point) means offline mode,
    // this crate's behaviour before this parameter existed: `LoginStart` goes
    // straight to `proto.login_success` exactly as it always did. `Some`
    // sends an encryption request instead and only calls `login_success`
    // once the session server confirms the client's identity — see the
    // `LoginStart`/`EncryptionResponse` arms below.
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
    // Issue #438: kept alongside `username` because the player entity's uuid
    // must be the *same* uuid `login_success` echoed back to this client — the
    // client resolves a player spawn by looking that uuid up in its own
    // `PlayerInfo` map, so a second, independently derived uuid would produce a
    // spawn every client silently discards.
    let mut login_uuid: Option<uuid::Uuid> = None;
    // A configured online-mode listener is not enough: only this flag, set
    // after `hasJoined` replaces the claimed identity, authorizes profile-key
    // provenance enforcement in Play.
    #[cfg(not(target_arch = "wasm32"))]
    let mut online_authenticated = false;
    // Issue #273. `Some` between the moment `LoginStart` sends an
    // `EncryptionRequest` and the moment the matching `EncryptionResponse`
    // arrives — the RSA keypair the request's public key came from (needed
    // again to decrypt the response) plus the verify-token challenge the
    // response must echo back correctly. `take()`n by the `EncryptionResponse`
    // arm, so a second response on the same connection (there is nothing
    // outstanding for it to answer) is the `None` branch there, not a reused
    // keypair.
    #[cfg(not(target_arch = "wasm32"))]
    let mut pending_encryption: Option<(ServerKeyPair, [u8; lodestone_net::VERIFY_TOKEN_LEN])> =
        None;
    let mut streamer = EntityStreamer::default();
    let mut player_list = PlayerListStreamer::default();
    // Vanilla's own status packet listener's own "has requested status"
    // field: one status reply per
    // connection, a second request is a disconnect.
    let mut status_requested = false;
    // Issue #335. This connection's declared channel support, populated from
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

    while let Some((packet_id, payload)) = conn.read_packet().await? {
        match proto.decode(state, packet_id, &payload) {
            ServerBound::Handshake { next_state } => {
                state = next_state;
            }
            // Issue #277. Mirrors vanilla's own status packet listener's own
            // "handle status request" step exactly: answer the first
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
            // Vanilla's own status packet listener's own "handle ping
            // request" step:
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
                // this exact packet, in its own generic login-phase packet listener —
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
                // Issue #336: vanilla's own "can player login" check, at vanilla's
                // own point in the sequence — after the name check, before
                // `login_success`, so a refused player never reaches
                // Configuration. `online` is 0 because this crate has no
                // cross-connection player registry to count from; the player
                // *limit* is therefore inert while the ban and whitelist checks
                // are live, which is the honest split rather than a fabricated
                // count.
                #[cfg(not(target_arch = "wasm32"))]
                if let Err(refusal) = access.may_join(uuid, peer_ip, 0) {
                    let reason = Text::literal(refusal.message());
                    let directive = proto.encode_disconnect(state, &reason);
                    apply(conn, &mut state, directive).await?;
                    return Err(ServerError::AccessDenied(refusal.message()));
                }
                username = Some(name.clone());
                login_uuid = Some(uuid);

                // Issue #273: online mode sends an encryption request instead
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
                }
            }
            // Issue #273: the client's answer to the `EncryptionRequest` the
            // `LoginStart` arm above sent. Everything up to and including this
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
                let t_cfg = JoinStopwatch::now();
                // Issue #329: the world spawn point is a *search*, not a
                // fixed local `(8, 8)` in the origin column. Vanilla's
                // own "set initial spawn" step walks a ±5-chunk spiral
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
                // **Read the world's own spawn first.** The spiral is
                // vanilla's own "set initial spawn" step, which vanilla runs **once, at
                // world creation**, and persists to `level.dat`. Running it per
                // connection re-paid a 121-column search on every join and meant
                // the persisted value was written and never read — so a world could
                // not remember where its spawn was, and nothing a future
                // `/setworldspawn` wrote could stick.
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

                // Issue #297/#619: keep the world spawn's own chunks loaded
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

                // Issue #302: this player's own saved state, if this world has
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

                // Where the player actually re-enters the world. `spawn.pos`
                // stays the **world** spawn: it is what `serve_play` uses for a
                // respawn, and overwriting it with the player's last position
                // would respawn a dead player back where they died. See
                // `join_position_for_saved_player`'s own doc comment for why a
                // saved position is not always trusted verbatim.
                #[cfg(not(target_arch = "wasm32"))]
                let join_pos = join_position_for_saved_player(saved_player.as_ref(), spawn.pos);
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
                for directive in proto.begin_play_at(view_radius, join_pos, game_mode) {
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
                // `ConfigurationFinished`, which requires `LoginAcknowledged`, which
                // requires the `LoginStart` arm that sets it. The `unwrap_or_default`
                // is a total fallback rather than a panic because a nil uuid resolves
                // to no player and therefore no permissions — failing closed, not
                // open. On `wasm32` there is no `AccessHandle` in this signature at
                // all (the whole ops/whitelist/ban feature is native-only), and the
                // browser build is the single-player owner's own world, so level 4 is
                // the honest answer there rather than 0 — which would lock the owner
                // out of `/gamemode` in their own game, and now would also delete its
                // node from the tree they are sent.
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

                // Full clock sync at join, mirroring vanilla's
                // own clock-manager's own "create full sync packet" step, sent by
                // its own "send level info" step before chunk streaming starts.
                //
                // Issue #323: the **world's** clock, not `(0, 0)`. A join no longer
                // resets the sky to dawn, and a world loaded off disk starts wherever
                // it left off (`WorldStateHandle::load_level_data`).
                let joined_at = world.time();
                apply(
                    conn,
                    &mut state,
                    proto.encode_set_time(joined_at.game_time, Some(joined_at.day_time)),
                )
                .await?;

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
                // Issue #453: the player's own column is encoded after **one**
                // column of generation. This loop used to build all `(2r+1)²`
                // coordinates up front, `await` a single `generate` over the lot,
                // and only then start encoding — so at `view_radius = 9` nothing
                // reached the client until all 361 columns existed, and raster
                // order put the player's own column at item ~180. [`join_view_rings`]
                // fixed the order and the scheduler below preserves the latency.
                //
                // Still **one** chunk batch, not one per group: the batch markers
                // stay outside this loop, so the client's flow-control
                // accounting (issue #270) sees the same single
                // begin/…/end sequence it always did.
                //
                // Unit 10: **no per-ring barrier.** Until now this walked
                // [`join_view_rings`] and waited for every column of ring `r`
                // before asking for ring `r + 1`, so the rings' slowest-column
                // tails stacked. Its stated rationale was the old FIFO memo
                // caches' warmup order — "ring 0 seeds the cache" — which
                // stopped describing anything when Unit 6's staged store landed
                // (`34202a21`): a stage now computes exactly once regardless of
                // arrival order, measured 441/361 exactly across 3 of 3
                // concurrent 289-column bursts against the old cache's varying
                // 452/452/448. The rings remain as the **wire order** only;
                // `crate::join_scheduler` schedules on a bounded window over
                // that order, whose width comes from `available_parallelism`
                // rather than from the view radius — which is the half of
                // `5104adf` that `4307b59` was right to revert.
                //
                // **The owner's report: "I can't break blocks, take damage, etc.
                // until it finishes."** Everything above was about the *order* of
                // the burst and none of it about its position in the sequence: all
                // `(2r + 1)²` columns — 1,089 at `view_radius = 16` — were
                // generated and encoded here, inline, before control ever reached
                // the loop that dispatches play packets. So a dig, a hurt, a
                // container click and every other interaction queued behind the
                // whole initial generation burst.
                //
                // Now only the innermost [`JOIN_PRESTREAM_RADIUS`] rings go out
                // here; the rest becomes a `JoinChunkStream` that `serve_play`
                // drains from a `select!` branch beside its socket read. Vanilla's
                // shape: its own "place new player" step adds the player to the level
                // and its own player-chunk-sender feeds chunks over subsequent ticks — the
                // client holds its own loading screen until it has what it needs,
                // but the server is not blocked.
                // **The join centre, and it has to be derived here rather than
                // after the stream.** [`join_view_rings`] yields Chebyshev-ring
                // *offsets* `(dx, dz)` in `-r..=r`, not absolute chunk
                // coordinates — the loop below used to hand them straight to
                // `encode_chunk`, so the square that actually went out was
                // always centred on chunk `(0, 0)` no matter where the player
                // joined. For a restored player 400 blocks from world spawn the
                // consequences compounded: `begin_play_at` teleported them to
                // `join_pos` and set the chunk cache centre to their own column,
                // `ViewTracker::new` below seeded its `loaded` set with the
                // square around that column — and none of those columns had
                // been sent. So the terrain the player got was a square around
                // the origin, the ground under their feet never arrived, and the
                // tracker believed it already had, which is why walking did not
                // repair it either.
                //
                // The reason it survived every gate: both `serve_play.rs` join
                // gates assert against `square(0, 0, view_radius)` with a spawn
                // that floors to chunk `(0, 0)`, where offsets and absolute
                // coordinates coincide — the *world* species of vacuous test.
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
                        // `prioritised`, not `with_window`: the pending half of
                        // this pipeline outlives the join now, so it is keyed on
                        // distance-from-the-player with an in-frustum bonus and
                        // re-keyed by `serve_play` when the player moves or turns.
                        // With no rotation known — which is exactly the state at
                        // join — that key *is* the ring walk, so the sequence this
                        // emits is unchanged from `join_view_rings` order.
                        //
                        // The priority centre is the player's own column, not
                        // `(0, 0)`: it is compared against the absolute
                        // coordinates in `coords`, and `serve_play`'s
                        // `reprioritise` re-keys the same queue against the
                        // player's absolute chunk. A hardcoded origin here made
                        // the two disagree for any player away from it.
                        //
                        // `encoding_with`: protocol encode runs **inside** the
                        // per-column `spawn_blocking` closure, so this task only
                        // writes frames. Measured at 62 M instructions / ≈2.4 ms
                        // per column, that was ≈2.6 s of serial work on the task
                        // that owes the player a reply — see
                        // `crate::protocol::ChunkEncoder`. It cannot change the
                        // wire, because the emit order is fixed by the queue at
                        // spawn time and not by which worker finished first.
                        let mut pipeline = crate::join_scheduler::ColumnPipeline::prioritised(
                            Arc::clone(src),
                            coords,
                            window,
                            (join_cx, join_cz),
                            None,
                        )
                        .encoding_with(proto.chunk_encoder());
                        while batch_size < prestream {
                            let Some(((cx, cz), payload)) = pipeline.next().await else {
                                break;
                            };
                            apply(conn, &mut state, encode_column(proto, cx, cz, payload)).await?;
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
                        // **Deliberately still per-ring, and this is not the
                        // divergence `805a1fb` warned about.** A borrowed source
                        // is not `'static`, so it cannot be spawned; every batch
                        // on this arm is a `generate_columns_parallel` call that
                        // blocks until the whole batch is done. There is
                        // therefore no generation for a window to overlap with
                        // the encode — the one thing a window buys — and
                        // splitting a ring into window-sized batches would only
                        // *add* barriers: measured while building
                        // `join_scheduler_gates.rs`, ring cumulative sizes are
                        // `1 + 4r(r + 1)`, always ≡ 1 (mod 8), so at a window of
                        // 8 no batch even straddles a ring boundary and ring 8's
                        // 64 columns become eight serial batches instead of one.
                        //
                        // What has to match across the arms is the **wire
                        // order**, and it does: both walk the same flattened ring
                        // sequence, both encode one column before generating the
                        // second, and both are gated —
                        // `join_streams_the_view_outward_from_the_players_own_column`
                        // here and `the_shared_arm_streams_the_view_outward_too`
                        // over a real loopback socket.
                        //
                        // The pre-stream/defer split lands on a ring boundary on
                        // this arm precisely because a ring is its unit of work:
                        // rings `0..=JOIN_PRESTREAM_RADIUS` are generated and
                        // encoded here, and the rest are handed to `serve_play` as
                        // whole rings. Same emitted sequence as the other arm, one
                        // barrier per ring instead of a window.
                        let mut rings = rings;
                        let deferred = rings.split_off(
                            (JOIN_PRESTREAM_RADIUS as usize + 1).min(rings.len()),
                        );
                        for ring in &rings {
                            let columns = source.generate(ring.clone()).await;
                            for (&(cx, cz), column) in ring.iter().zip(columns.iter()) {
                                apply(conn, &mut state, proto.encode_chunk(cx, cz, column)).await?;
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
                        join_pos,
                    )
                });

                // Issue #619: this connection's own chunk-residency ticket
                // pair (`PLAYER_LOADING` + `PLAYER_SIMULATION`), keyed by the
                // same login uuid `PlayerRegistry::join` above uses for the
                // entity ticket — XOR-folded to a `u64` since
                // `TicketOwner::Player` only needs per-connection uniqueness,
                // never identity (see `TicketStoreHandle::grant_player`'s own
                // doc). Moved into `serve_play` below; its `Drop` withdraws
                // both tickets on every exit path out of that function,
                // exactly like `player_ticket` just above. A disconnected
                // `TicketStoreHandle::default()` (every non-`IntegratedServer`
                // caller) makes the grant, every `move_to`, and the eventual
                // drop all reach a store nobody else reads — observably a
                // no-op, same as every other feed in this file.
                let player_ticket_guard = {
                    let bits = login_uuid.unwrap_or_else(uuid::Uuid::nil).as_u128();
                    let id = (bits as u64) ^ ((bits >> 64) as u64);
                    tickets.grant_player(id, (join_cx, join_cz), view_radius)
                };

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
                // Issue #545: two radii, two roles — the square that was just
                // streamed, and the ceiling a later `ClientInformationChanged`
                // may raise this connection to. See `ViewTracker::max_radius`.
                let view = ViewTracker::new((spawn_cx, spawn_cz), view_radius, max_view_radius);
                // Issues #48/#464. `player_uuid`, `permission_level` and
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
                    caller: CommandCaller::new(player_uuid, username.clone()),
                    permission_level,
                };
                // Issue #338. The server-authoritative advancement/statistics
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
                // Issue #547: the recipe book, `replace: true` — vanilla's own
                // per-player "init menu"/recipe-book-menu join path sends the
                // whole book once. **This is what hands out `RecipeDisplayId`s**,
                // so without it `PLACE_RECIPE` is not merely unimplemented but
                // unreachable: the id a client echoes back is a position in this
                // list. Same trait-default no-op story as the advancements above
                // for a protocol with no override.
                apply(
                    conn,
                    &mut state,
                    proto.encode_recipe_book_add(crate::crafting::recipe_book_entries(), true),
                )
                .await?;
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
            | ServerBound::ItemDropped { .. }
            | ServerBound::UseItemOn { .. }
            | ServerBound::ChangeGameMode { .. }
            | ServerBound::DifficultyChanged { .. }
            | ServerBound::DifficultyLockChanged { .. }
            | ServerBound::GameRuleChanged { .. }
            | ServerBound::CarriedItemChanged { .. }
            | ServerBound::ContainerClicked { .. }
            | ServerBound::RecipePlaced { .. }
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
    /// entity and has no slots at `pos` at all (issue #529 step 2). Its grid lives
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

/// Opens a villager's `minecraft:merchant` trade screen (issue #245's
/// visible half). Unlike [`open_container_screen`]/`open_crafting_table_screen`,
/// this sends no `container_set_content`/`container_set_data` at all: a
/// merchant window's whole state is the `MERCHANT_OFFERS` packet, which
/// vanilla's `ServerPlayer::openMenu` sends in its own right immediately
/// after `open_screen` for a `MerchantMenu`.
///
/// **Trade purchase is wired**, through [`ServerBound::SelectTrade`]'s
/// dispatch arm in `dispatch_play_packet` — issue #245's third piece
/// (restock/leveling/purchase), previously named here as absent, is now a
/// real per-villager economy: see [`crate::mobs::MobSim::villager_offers`]/
/// [`crate::mobs::MobSim::try_villager_trade`].
///
/// `offers` is the caller's already-priced, already-*persistent* list —
/// [`crate::mobs::MobSim::villager_offers`], not a fresh stateless one — so
/// what this sends is read from exactly the same `uses`/`demand` state
/// [`ServerBound::SelectTrade`]'s dispatch arm charges against.
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
/// inventory, given the [`TradeRecord`](crate::mobs::villager::trades::TradeRecord)
/// [`ServerBound::SelectTrade`]'s index resolved to.
///
/// # Why this is not vanilla's payment-slot flow
///
/// Real `MerchantMenu` mechanics need the player to place items into two
/// payment slots and take the result from a third — that is genuinely new
/// per-connection scratch storage the same shape as
/// [`PlayerInventory::workstation`], **except** a villager is not a
/// [`crate::block_entities::BlockEntity`] the way an anvil or a furnace is
/// (it is a [`crate::mobs::SimMob`]), so it has no `BlockPos` for
/// [`OpenContainer`]'s slot-sync machinery to key on — the sync loop that
/// makes every *other* menu in this crate live reads a block entity by
/// position. Standing up a second, parallel slot-storage-and-sync mechanism
/// for exactly one menu shape was judged out of scope for the pass that
/// connected this packet at all (see the issue's own tracking comment); this
/// executes the trade in one step instead, the moment a row is selected.
///
/// # What that costs
///
/// The cost items are found and consumed from wherever they sit in the
/// standard 36-slot hotbar+main inventory ([`PlayerInventory::consume`]),
/// not from two manually-filled slots — so a player can trade without first
/// dragging items into place, a real deviation from vanilla's UX rather than
/// a silent one.
///
/// `offer` is the caller's read-only priced peek at the villager's *live*,
/// persistent [`crate::villager_trade::VillagerTrades`] entry
/// ([`crate::mobs::MobSim::villager_offers`]) — this function only decides
/// whether the buyer's inventory can afford it; committing the trade against
/// that persistent state (uses, demand, xp) is
/// [`crate::mobs::MobSim::try_villager_trade`]'s job, called only after this
/// one succeeds, so a buyer who cannot pay never moves the villager's
/// economics.
///
/// Returns `None` — inventory untouched — when the player lacks the cost
/// items or has no room for the result, mirroring vanilla's own refusal
/// rather than dropping the excess on the floor.
fn attempt_villager_trade(
    inventory: &PlayerInventory,
    offer: &crate::villager_trade::OfferState,
) -> Option<PlayerInventory> {
    let trade = &offer.record;
    let mut trial = inventory.clone();
    // `offer.modified_cost_a_count()`, not `trade.wants_count`: issue #246's
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

/// Opens a crafting table's `minecraft:crafting` menu — issue #529's step 2, the
/// **positionless virtual menu**.
///
/// [`open_container_screen`] structurally cannot do this: it is driven entirely by
/// a [`BlockEntity`] at `pos`, and **a crafting table is not a block entity.** Its
/// slots are scratch space owned by the menu (vanilla's `CraftingMenu` builds a
/// `TransientCraftingContainer` + `ResultContainer` in its constructor and throws
/// them away on close), which here is [`PlayerInventory::table_crafting`].
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

/// Opens an anvil/grindstone/smithing-table screen (issues #253-#255) — the
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

/// Opens the enchanting-table screen (issue #253): the same positionless
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
    // A fresh `[0, i32::MAX)` draw from the connection's own `SpawnRng`
    // (`dispatch_play_packet`'s `drops_rng`, the same "pre-drawn value"
    // shape `apply_use_item_on`'s own composter `roll` already uses),
    // standing in for vanilla's own player-side "get enchantment seed"
    // getter at menu-open —
    // vanilla's own enchantment-menu seed field's initial value.
    // `PlayerInventory::open_workstation` just zeroed it; this replaces that
    // zero with a real roll before the first offer is ever computed.
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

/// Applies one block-breaking phase, mirroring
/// vanilla's own per-player game-mode "handle block break action" step's
/// three destroy ordinals.
///
/// Since issue #531 this **validates** the break rather than trusting it: see
/// [`crate::block_breaking`] for the destroy-progress arithmetic, the tolerance
/// it deliberately carries, and what is still not modelled (creative mode and
/// spawn protection). Two behaviours follow from it, and they are opposite ends
/// of the same missing computation:
///
/// * **`StartDestroy` can break the block by itself.** Vanilla's `"insta mine"`
///   branch fires when destroy progress reaches `1.0` in the first tick, which is
///   every zero-hardness block — and a client that knows the block is instant
///   sends no `StopDestroy` at all. Breaking only on `StopDestroy` therefore made
///   sugar cane, grass and flowers *unbreakable on this server*, which is the bug
///   the owner reported.
/// * **A `StopDestroy` that arrives too early is *deferred*, not refused.** It
///   arms vanilla's `hasDelayedDestroy` and the dig keeps accruing progress on
///   the server's clock, breaking the block once it is fully earned — see
///   [`crate::block_breaking::PendingBreak::defer`] and `serve_play`'s
///   `vitals_tick` arm. Bedrock and obsidian are still not instant, because an
///   unbreakable block is not deferrable and obsidian's deferred dig is minutes
///   long; but hold-and-release on stone breaks stone, which an outright refusal
///   made impossible.
///
/// `pending_break` is this connection's tracked in-progress dig — the
/// version-free analogue of vanilla's own destroy-position + destroy-progress-start pair.
/// It is what makes `StartDestroy` + `StopDestroy` break a block while
/// `StartDestroy` + `AbortDestroy` does not, and what makes a `StopDestroy` for a
/// position nobody started a no-op, mirroring vanilla's own
/// "position equals the tracked destroy position" guard.
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
/// own equivalent is its own generic container-menu "still valid" polling, which this
/// crate does not model).
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
    // Issue #337. Where a broken block's rolled loot is spawned as item
    // entities, and this connection's draw source for the roll plus
    // `popResource`'s placement. `mobs` is the same shared handle the composter
    // arm of `apply_use_item_on` already spawns bone meal into, so drops land in
    // the one `MobSim` every connection's streaming pass reads.
    mobs: &MobHandle,
    drops_rng: &mut SpawnRng,
    // Issue #539. The breaker's main-hand stack, `None` for a bare hand — this
    // connection's `PlayerInventory::selected_item`. It is the loot-context
    // "tool" parameter
    // for the roll *and* the subject of vanilla's own "has correct tool for
    // drops" check, which is
    // consulted before the roll happens at all. Passed as a borrowed stack rather
    // than the whole inventory because that is all either use needs, and because
    // the caller holds `&mut PlayerInventory` for other reasons.
    held: Option<&ItemStack>,
    // Issue #531. The breaker's tracked feet position for the interaction-range
    // test, `None` until the client has sent a movement packet — see
    // `block_breaking::within_interaction_range` for why `None` permits the break
    // rather than refusing it.
    player_feet: Option<Vec3>,
    // Issue #327: the world's rules, for the `block_drops` gate below (vanilla's
    // own gate site, inside its own resource-drop routine).
    world: &crate::world_state::WorldStateHandle,
    // Issue #531. The server tick this packet is being handled on, for the
    // destroy-progress accounting. `None` on `wasm32`, which has no timer to
    // count ticks with (see `serve_play`'s two definitions); the timing test is
    // then skipped, while the hardness and range tests still apply.
    game_tick: Option<u64>,
    // Where `destroy_block`'s break level event is published, and the player it
    // is published *except* for (this connection's own).
    block_ticks: &BlockTickFeed,
    breaker: uuid::Uuid,
    // Issue: creative mode. Vanilla's own block-break-action handler's very
    // first branch is `if (this.isCreative()) { destroyAndAck(...); return; }` —
    // no hardness clock and no drops, whatever the block or the tool.
    creative: bool,
    action: BlockActionKind,
    // Issue #338's `minecraft:mined` counter — see `destroy_block`'s own parameter
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
                // **Not a refusal.** Vanilla's shortfall branch arms
                // `hasDelayedDestroy` and keeps accruing progress in
                // `ServerPlayerGameMode.tick` until the block is fully earned
                // (vanilla's own per-player game-mode tick); it sends no rollback
                // here at all. Refusing instead — which is what this arm did
                // between #531 and this fix — made every non-instant block
                // unbreakable, because a `StopDestroy` arriving on the same tick
                // as its `StartDestroy` (which is what a local integrated server
                // sees) can never clear 0.7.
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

/// Breaks the block at `pos`: rolls and pops its loot, clears any block entity
/// and open container against it, and tells the client.
///
/// Vanilla's own destroy-block funnel. Extracted from
/// [`apply_block_action`] by issue #531 because there are now **two** call sites
/// — the instant break on `StartDestroy` and the validated `StopDestroy` — and
/// vanilla likewise reaches its own destroy-block routine from both.
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
    // The break's own level event (vanilla's own `PARTICLES_DESTROY_BLOCK`, sound
    // *and* particles in one packet), published excluding `breaker` — the
    // acting client predicts its own break sound locally, every other player
    // must hear it. See `BlockTickFeed::publish_effect_except`.
    block_ticks: &BlockTickFeed,
    breaker: uuid::Uuid,
    // `false` in creative — vanilla's own destroy-block routine calls
    // `removeBlock(pos, false)` there, so a creative break drops nothing and
    // rolls no loot at all (which also means it consumes no RNG draws).
    drop_loot: bool,
    // The `block_drops` game rule **alone**, without the creative fork above —
    // for the support cascade only. See [`collapse_unsupported`]'s own doc comment
    // for why the two gates genuinely differ in vanilla; passing `drop_loot` here
    // would make a creative player mining under a flower delete the flower.
    cascade_drops: bool,
    pos: BlockPos,
    // The statistics store, for the `minecraft:mined` counter. Keyed by the block
    // that was broken, and incremented on **every** break including a creative
    // one — vanilla's own destroy-block routine calls
    // `awardStat(Stats.BLOCK_MINED)` before the `isCreative()` fork that decides
    // whether anything drops, so gating this on `drop_loot` would silently stop
    // counting in creative.
    advancements: &mut AdvancementManager,
    // Hunger's mining cost (vanilla's own `EXHAUSTION_MINE` constant, 0.005 per block).
    // `None` for a creative break — vanilla's guard is on `causeFoodExhaustion`
    // (`!abilities.invulnerable`), not on the break, so an invulnerable player mines
    // for free. An `Option` rather than a bool beside the vitals, so the guard
    // cannot be forgotten at a new call site.
    exhaust: Option<&mut PlayerVitals>,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    // Issue #337: read the block *before* it is replaced. This is
    // the whole reason the drop has to happen here rather than in a
    // later tick — once `set_block` has run, what was broken is
    // unrecoverable, and vanilla's own `removeBlock` likewise
    // captures the fluid state first (its own removal routine reads
    // `getFluidState(pos)`, and only then `setBlock(pos,
    // fluidState.createLegacyBlock())` — **not** air unconditionally,
    // see `new_state` below).
    let broken = source.block_state(pos.x, pos.y, pos.z);
    // Vanilla's own removal routine's own write, which is what its own
    // destroy-block routine actually calls (not its own plain block-destroy
    // routine, despite the name): the cell's *fluid* state survives a break. For a dry block
    // `fluid_state_of` is `None` and this is plain air, which is why every
    // existing break gate — all of them dry blocks — could not see the
    // difference. A waterlogged block's fluid state is a water source
    // (`fluid_state_of` reports `amount: 8, falling: false`), so its
    // `block_state()` is `minecraft:water[level=0]` — the source vanilla
    // leaves behind.
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
    // **Gated on `block_drops`** (pre-26.2 `doTileDrops`), which vanilla
    // consults in the same place — its own resource-drop routine wraps
    // `popResource` in `level.getGameRules().get(RULE_DOBLOCKDROPS)`.
    // ~~"this crate has no live game-rule registry to consult"~~ was
    // true when written; the registry is now `world_state`.
    //
    // **Issue #539: the tool decides both whether anything drops at
    // all and what.** `drops_are_allowed` is vanilla's own
    // correct-tool-for-drops check, consulted by `destroyBlock`
    // *before* it calls `dropResources` — so a bare hand on stone
    // breaks the block and drops nothing, and the roll's RNG draws
    // never happen either (folding the check into the table would
    // still consume them and shift the next break's stream). `held`
    // then rides into the roll as vanilla's own tool loot-context parameter, which is
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
                // 10-tick delay `popResource`'s
                // `setDefaultPickUpDelay()` applies, so the breaker
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
    // Vanilla's own spawn-after-break → try-drop-experience → pop-experience chain: an ore pops
    // experience orbs at the **centre** of the broken cell, not at the jittered
    // positions its item drops used.
    //
    // Gated on `drop_loot` for the same reason the loot above is: `popExperience`'s own
    // guard is `level.getGameRules().get(GameRules.BLOCK_DROPS)`, the same rule. It is
    // deliberately **not** gated on `drops_are_allowed` — vanilla's tool check guards
    // `dropResources`, while `spawnAfterBreak` is called by `destroyBlock` either way,
    // so breaking coal ore with a bare hand yields no coal and still yields the XP.
    // That asymmetry looks like a bug until you read which method each guard is on.
    //
    // Silk touch would zero this through `processBlockExperience`; no enchantment
    // exists in this crate, so nothing to apply.
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
    // an ocean, or the block beside a spring — and it is exactly vanilla's
    // `neighborChanged` case: the *water* did not change, so only a notification
    // can wake it. `ticks_after_edit` covers this cell and its six neighbours and
    // reads none of them, so it works across a chunk border; a position holding
    // no fluid is a silent no-op when the tick drains.
    //
    // Deliberately **not** folded into `propagate_placement`, whose return value
    // several gates assert on exactly. This is its own request against the same
    // feed, and `run_tick_loop`'s rebase loop routes it to the fluid queue.
    block_ticks.request_scheduled_ticks(crate::fluid::ticks_after_edit(pos));
    let directive = proto.encode_block_update(pos.x, pos.y, pos.z, &new_state);
    apply(conn, state, directive).await?;
    // Breaking a light source has to darken the column, and the `BLOCK_UPDATE`
    // above carries no light. See `crate::light` for why this is a column resend
    // rather than a `LIGHT_UPDATE`. `new_state` rather than a hardcoded `AIR`
    // for the same reason as the write above: a broken waterlogged block keeps
    // a light-dampening fluid in the cell, not empty air.
    resend_column_for_light(conn, proto, source, state, &broken, &new_state, pos).await?;

    // Vanilla's `setBlock(pos, AIR, UPDATE_ALL)` runs two passes the break above
    // did not: `updateNeighbourShapes` (every neighbour's `updateShape`, which is
    // where a torch or a rail that just lost its support turns to air) and then
    // `updateNeighborsAt` (`neighborChanged`, the redstone/gravity reactions).
    // **Neither of them happened on a break in this crate** — `propagate_placement`
    // had exactly one caller, `apply_use_item_on`, so breaking a block beside dust
    // never recomputed the dust and breaking the block *under* anything never
    // destroyed it. Both are here now, shapes first, matching that order.
    let mut collapsed = collapse_unsupported(source, pos);
    // Vanilla's own nether-portal block's update-shape hook is a *second* member of
    // `updateNeighbourShapes`, alongside `block_support`'s survives table
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
    // Vanilla's own update-or-destroy → destroy-block → drop-resources chain.
    //
    // **Gated on `cascade_drops`, not on `drop_loot`, and the difference is
    // vanilla's**: the creative no-drop is its own destroy-block routine
    // choosing `removeBlock(pos, false)` for the block *the player broke*, while a
    // cell that self-destructs goes through its own update-or-destroy routine, which knows nothing
    // about who caused it. So a creative player mining the dirt under a flower does
    // get the flower, and reusing `drop_loot` here would have silently eaten it.
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
        block_ticks.request_scheduled_ticks(crate::fluid::ticks_after_edit(cell));
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
            source.block_state(probe.x, probe.y, probe.z)
        }) {
            continue;
        }
        // Vanilla's own update-or-destroy routine reaches its own destroy-block routine, which — like
        // `removeBlock` above in this file — writes `fluidState
        // .createLegacyBlock()`, not literal air: a waterlogged sign whose
        // support block collapses leaves its water source behind. See
        // `crate::server::destroy_block`'s `new_state` for the identical rule.
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
/// compute, why the fix is a whole-column resend rather than the `LIGHT_UPDATE`
/// packet that would be cheaper, and the two gaps this leaves (sky light after an
/// edit, and light crossing a column border).
///
/// `source.column(cx, cz)` reflects the `set_block` the caller already performed
/// — `ChunkSource::column`'s own contract — so the light is computed over terrain
/// that contains the torch.
///
/// # It is a `light_update` now, not a column resend
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
/// `compute_column_light` is the **isolated** compute, so light still does not
/// cross a column border and the measured Δ5 sky-light dark bias at borders is
/// unchanged — this is a cheaper carrier for the same values, not a better
/// computation. (The claim that used to sit here, that `should_relight` compares
/// emission only so breaking a roof does not re-send sky light, is no longer true:
/// it compares dampening too. Left recorded rather than deleted because it was the
/// stated reason this predicate was safe to keep narrow.) The border gap needs
/// light computed where the 3×3
/// neighbourhood is resident (the chunk source) and carried on the column; see
/// `crate::light` and `docs/server-chunk-light.md`, including the invalidation
/// trap that makes stale light look like a working fix.
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
    send_column_light(conn, proto, source, state, pos.x.div_euclid(16), pos.z.div_euclid(16)).await
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
/// **That path used to send no light at all**, which is the whole reason this
/// function was split out. `container_sync_tick`'s drain forwarded
/// `encode_block_update` and stopped, so every block change *originating in the
/// tick loop* moved on the client and left the light behind — stale until the
/// player rejoined and the column was re-encoded from scratch. The reported
/// symptom was a torch placed underwater: the placement relights correctly through
/// the predicate above, and then the fluid tick destroys the torch a tick later,
/// on this path, so the block vanished and its light stayed. Fire spreading and
/// dying, grass and crops, a redstone torch flipping `lit`, and a falling block
/// landing all travel the same wire and had the same defect.
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
    if let Some(light) = proto.compute_column_light(&column) {
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
    apply(conn, state, proto.encode_chunk(cx, cz, &column)).await?;
    apply(conn, state, proto.end_chunk_batch(1)).await?;
    Ok(())
}

/// Collects every dropped item within this player's pickup volume into their
/// inventory, and returns the native slots that changed (issue #337's fifth and
/// last link).
///
/// This is vanilla's own per-tick step → item-entity player-touch →
/// inventory-add chain, minus the XP-orb branch. See
/// [`crate::block_drops::is_within_pickup_range`] for the volume and
/// [`PlayerInventory::add`] for the destination order — both are vanilla
/// behaviour that a plausible simplification gets wrong.
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
/// [`PlayerInventory::add`] reports its leftover, and vanilla's `playerTouch`
/// only removes the entity when `getInventory().add(...)` consumed everything.
/// A partial pickup therefore credits what fitted and puts the remainder back as
/// the item's new count — the entity stays, visibly, rather than the surplus
/// vanishing.
/// # Statistics and advancements
///
/// This is also vanilla's `minecraft:inventory_changed` seam, so it is where
/// [`AdvancementManager::on_inventory_changed`] and the `minecraft:picked_up`
/// counter are driven from. Both are credited **per item actually banked**, not
/// per entity seen: a pickup that only partly fitted credits what fitted, and one
/// that fitted nothing credits nothing — the same `written`/`leftover` split the
/// slot updates already key off.
/// One item entity a player just took, for [`ServerProtocol::encode_take_item_entity`].
#[derive(Debug, Clone, Copy)]
struct TakenItem {
    item_entity_id: i32,
    /// The entity's stack count **before** the inventory took any of it — vanilla's
    /// `orgCount`. Not the amount banked; see the encoder's own doc.
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
            // The pickup *animation* cue. Gated on `banked > 0` because that is
            // vanilla's own guard: `playerTouch` only calls `player.take` when
            // `getInventory().add(itemStack)` returned true, i.e. when something
            // actually went in. A pickup into a full inventory shows nothing, which
            // is right — nothing was taken.
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

/// Absorbs at most one nearby experience orb into `experience` — the pickup half of
/// vanilla's own experience-orb player-touch routine.
///
/// # Why at most one
///
/// `playerTouch` refuses outright while the **player's** `takeXpDelay` is non-zero and
/// resets it to 2 on every absorption, so vanilla can only ever take one orb per two
/// ticks no matter how many are overlapping. Draining every overlapping orb in one pass
/// would bank the same total, which is exactly why it is worth stating: the difference is
/// invisible in the final number and obvious on screen, because the client plays one
/// pickup sound per `TAKE_ITEM_ENTITY` and animates one orb per absorption.
///
/// `delay` is the caller's own copy of `takeXpDelay`, decremented here once per call —
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

/// Vanilla's own `Direction.fromYRot` restricted to the
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

/// Vanilla's own `getStateForPlacement` for the block a player just placed, or
/// `None` when no convention applies and the caller should keep the census's
/// bare default-state name.
///
/// The per-block table lives in [`crate::block_placement`]; this wrapper exists
/// only to keep the three redstone families ahead of it. They are not a
/// different convention — a repeater does take `getHorizontalDirection().getOpposite()`
/// like a furnace — but the redstone model reads `delay`/`locked`/`powered`
/// straight off the state *string*, so their placement must name the full
/// property set rather than leaving it to be defaulted downstream.
///
/// The observer is deliberately still yaw-only here, where vanilla's
/// vanilla's own observer-block placement state getter resolves a vertical facing too;
/// `crate::redstone_observer` models horizontal observers only, so a
/// `facing=up` observer would be a state the signal model cannot read.
fn placed_block_state<F>(
    block: &str,
    ctx: &crate::block_placement::PlaceContext,
    block_at: F,
) -> Option<crate::block_placement::Placement>
where
    F: Fn(BlockPos) -> String,
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
/// creative without the abilities packet is in creative and cannot fly, which
/// is the exact defect this batch was reported as.
fn game_mode_directives<P: ServerProtocol>(proto: &P, mode: GameMode) -> [ServerDirective; 2] {
    [
        proto.encode_game_mode(mode),
        proto.encode_player_abilities(Abilities::for_mode(mode)),
    ]
}

/// Whether a player standing with feet at `(px, py, pz)` overlaps the swept
/// region of a `moving_piston` cell travelling between `source` and `dest`
/// (issue #694, item 4). The same box `crate::mobs::piston_shove::mob_aabb`
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
    inventory: &mut PlayerInventory,
    players: Option<&PlayerRegistry>,
    player_uuid: uuid::Uuid,
    effect: crate::commands::Effect,
    // Issue #338. `/give` is a `minecraft:inventory_changed` producer, so this arm
    // grants criteria exactly as a floor pickup does — see the `GiveItems` arm.
    advancements: &mut AdvancementManager,
    // For the world-clock timestamp the grant is stamped with, which must be
    // tick-derived rather than `Instant::now()` (this crate links into wasm32).
    world: &crate::world_state::WorldStateHandle,
    // Issue #259. This player's live status effects — the store `/effect give` and
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
            for directive in game_mode_directives(proto, mode) {
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
                    // Vanilla drops the remainder as an item entity
                    // (`GiveCommand`'s `player.drop(...)`). This crate has no
                    // command-spawned drop path, so the surplus is reported rather
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
            // The producer #259 needed. `ActiveEffects::apply` runs vanilla's whole
            // stacking rule (including the hidden-effect chain), so a second
            // application of the same effect behaves correctly rather than
            // overwriting.
            //
            // Read the "already present" fact *before* calling `apply` — that is
            // exactly the distinction `ServerPlayer.onEffectAdded` (a brand-new
            // instance) vs. `onEffectUpdated` (an existing one refreshed) makes,
            // and it is what the encoded packet's `blend` flag carries (see
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
            // The counterpart to `ApplyEffect` above — `LivingEntity
            // .removeEffectNoUpdate` (single) / `onEffectsRemoved` (all) each
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
            // `Entity.kill()`: straight to zero, no armour/defenses consulted —
            // vanilla's `genericKill` damage type is what `/kill` always deals.
            vitals.kill();
            publish_health(
                conn,
                state,
                proto,
                vitals,
                // No sound fires for this call today regardless (`hurt` below is
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
            apply(conn, state, proto.encode_teleport(x, y, z, yaw, pitch)).await?;
        }
    }
    Ok(())
}

/// Vanilla's own `SlabBlock.canBeReplaced` for the clicked block:
/// `true` when placing `held` onto `clicked` should turn it into a double slab
/// rather than start a new one in the next cell.
///
/// Vanilla's `replacingClickedOnBlock()` branch is the only one reachable here
/// — this is asked about the clicked block itself — so the whole predicate is
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
/// item identity alone, mirroring vanilla's own brewing-stand-block-entity
/// can-place-item check (slots 0-2 take potions/bottles, slot 3 takes any
/// `potionBrewing.isIngredient`, slot 4 takes the brewing-fuel item tag).
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
/// `None` there), the same shape vanilla's own composter-block use-item-on
/// routine establishes for the composter.
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

/// The seed for the per-connection [`SpawnRng`] that draws bone meal's crop-age
/// and sapling-success values — its own stream rather than the composter's, for
/// the reason that constant's own comment gives about coupling two features
/// through one RNG. `crate::bone_meal`'s draw-count gates hold only against a
/// stream nothing else advances.
const BONE_MEAL_BEHAVIOR_SEED: u64 = 0x5EED_B04E;

/// The seed for the per-connection [`SpawnRng`] that draws
/// vanilla's own base-fire-block ignite routine's `nextInt(1, 3)` player ramp. Its own stream for the
/// reason the two constants above give.
const BURN_BEHAVIOR_SEED: u64 = 0x5EED_F14E;

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
    /// there, and the hand is untouched). No
    /// placement may follow.
    Noop,
    /// One item was consumed from the player's hand (`itemStack.consume(1)`,
    /// vanilla's own composter fill routine). `remainder` is the hand's new contents for
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
/// `Composter::insert`/`extract` (and therefore the whole seven-tier fill
/// state machine, issue #249) reachable from a player at all.
///
/// Mirrors vanilla's own composter-block use-item-on routine's order: the held item (if any) is
/// offered to the fill machine first, and whatever the item offer does not
/// consume falls through to vanilla's own `useWithoutItem`,
/// which extracts at level `8` and otherwise `PASS`s. Concretely:
///
/// * an empty hand on a ready (level `8`) composter extracts the bone meal;
/// * a compostable item is rolled against its chance, consuming one from the
///   hand either way (a failed roll still eats the item — vanilla
///   `itemStack.consume(1)`);
/// * a compostable item at level `7` (waiting on its scheduled tick) is
///   consumed as a click but changes nothing;
/// * a *non*-compostable item never reaches `insert`'s level gate, because at
///   level `7` vanilla's own compostables-table containment guard fails *before* the
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
            // Empty hand: vanilla's own `useWithoutItem`.
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
            // Vanilla's own `extractProduce`: exactly one
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
/// **Placement honours the held item for every block in the game** (#466).
/// `inventory`'s currently selected item is resolved through
/// [`lodestone_data::block_items::block_for_item`] — the 26.2 census of
/// vanilla's own block-item block getter, dumped from the real jar — which decides both
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
/// of `docs/block-entities.md`'s gap 3. Mirrors vanilla's own order:
/// its own use-item-on handler runs the clicked block's
/// own `useItemOn`/`useWithoutItem` (which is what opens a furnace/hopper's
/// menu) **before** any `BlockPlaceContext` placement logic, and a block
/// that opens a menu never falls through to placement.
///
/// **A brewing stand at `pos` is this "clicked block's own use" step too,
/// but without a menu** (issue #252): it cannot be opened — `menu_name`
/// answers `None`, because its `Bottle` slots are not real `ItemStack`s — so
/// [`insert_into_brewing_stand`] stands in for the menu with a direct
/// one-item-per-click insert, the shape vanilla's own composter-block
/// use-item-on routine uses for
/// the composter (which is the *other* kind `menu_name` answers `None` for,
/// there because vanilla gives a composter no menu at all). A held item that
/// belongs in a brewing stand is routed into the matching slot and consumed;
/// an unrelated held item still falls through to the placement logic below
/// exactly as before the fix that added this routing.
///
/// Whether writing `state` at `target` would intersect the placer's own
/// bounding box — vanilla's own item-can-place check refusing when
/// `Level.isUnobstructed(state, pos, CollisionContext.empty())` is false,
/// narrowed to the one entity this server can currently name
/// at a placement site: the placer, from `player_pos`. A full
/// `isUnobstructed` tests *every* entity's bounding box in the cell and
/// excludes spectators; this crate has no per-connection entity-bounding-box
/// registry to query the rest of, so another player or a mob standing in the
/// target cell is not yet refused — see `docs/block-edit.md`.
///
/// A state with an **empty** collision shape (a torch, a rail, a pressure
/// plate, redstone dust…) is never obstructed, matching vanilla's own
/// empty collision shape never intersecting anything — placing one at your own
/// feet is legal in vanilla and stays legal here.
///
/// The placer's box is vanilla's own player entity-type dimensions (`0.6 x 1.8`, centred
/// horizontally on `feet`, `feet.y..feet.y + 1.8` vertically) —
/// the unobstructed check reads the entity's own bounding-box getter at click time, which does
/// not shrink for the sneaking pose (`1.5`), so this does not model pose
/// either.
fn placement_obstructs_placer(target: BlockPos, state: &str, feet: Vec3) -> bool {
    let Some(id) = lodestone_data::block_states::state_id(state) else {
        return false;
    };
    let Some(boxes) = lodestone_data::collision_shapes::collision_boxes(id) else {
        return false;
    };
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
    // Issue #475. The placing player's yaw, so the directional families can
    // derive their `facing` (see [`placed_block_state`]). `None` until the
    // first packet carrying angles arrives; placement then falls back to the
    // block's default state.
    player_yaw: Option<f32>,
    // Pitch, for the `getNearestLookingDirection` families alone (a dispenser
    // or piston placed while looking down points up). `None` on the same
    // terms as `player_yaw`.
    player_pitch: Option<f32>,
    // The placing player, for the place sound's `except` argument (see the
    // `block_placed` call below).
    placer: uuid::Uuid,
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
    // This connection's bone-meal roll source. A whole `SpawnRng` rather than a
    // pre-drawn value like the composter's `roll` above, because
    // `crate::bone_meal::apply_bone_meal` draws a *variable* number of values —
    // one for a crop, one for a sapling, none for a non-target — and the draw
    // count per use is part of the specification its own tests pin. Pre-drawing
    // would fix the count at one and desynchronise the stream.
    bone_meal_rng: &mut SpawnRng,
    // The **world** difficulty, for vanilla's own can-spawn entity-type check — a spawn egg on
    // Peaceful fails for any `notInPeaceful` species rather than spawning and
    // being evicted on the next tick. Passed by value rather than as the
    // `WorldStateHandle` because this is the only scalar this function needs and
    // a handle would invite a second, unrelated read. Spelled with its full path
    // rather than added to this module's `lodestone_model` import list, which is
    // edited concurrently by other work.
    difficulty: lodestone_model::Difficulty,
    // The acting player's game mode, for vanilla's own item-stack consume routine's
    // `hasInfiniteMaterials()` gate — a creative placement writes the block and
    // consumes nothing. See the consumption arm at the end of the placement branch.
    game_mode: GameMode,
    // A fresh `[0, i32::MAX)` draw from `dispatch_play_packet`'s `drops_rng`,
    // the same pre-drawn-value shape the composter `roll` above already
    // uses. Only consumed if this click opens an enchanting table (see
    // `open_enchanting_screen`'s own parameter comment); drawn unconditionally
    // by the caller anyway, matching the composter roll's own "one draw per
    // right-click, whatever block was hit" reasoning.
    enchant_seed_roll: i64,
    // `ServerBound::UseItemOn::hand` (`0` main, `1` off) — which native slot
    // `held_item` below reads from. Previously there was no such parameter
    // and every click acted on the selected hotbar slot only, so a shulker
    // box (or anything else) held in the off hand could never be placed.
    hand: u8,
    // Issue #150. Only the narrow crafting-station hook registry, not the
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
    // Issue #337: a chest that generation placed (a shipwreck's, an igloo's, an
    // ocean ruin's) lives in the *column*, not in the live registry — nothing has
    // placed or mutated it. Hydrate it on the first click, so the loot that was
    // rolled at generation is what opens. Gated on the block actually being one of
    // the container blocks, so an ordinary right-click never pays for the lookup.
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

    // Issue #529 step 2: a crafting table opens a *virtual* menu. It is not a block
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

    // Issues #253-#255: the anvil, grindstone, smithing table and enchanting
    // table are, like the crafting table just above, **not** block entities in
    // vanilla — each menu's own input slots are scratch space the menu itself
    // owns and throws away on close (vanilla's own per-menu input-slot
    // containers on the anvil, grindstone, smithing and enchantment menus),
    // so `existing_menu` above structurally
    // cannot reach any of them either.
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
        // The cell above is what `SaplingBlock`/`CropBlock` light checks read;
        // resolved here because `bone_meal` has no world access of its own.
        let above = source.block_state(pos.x, pos.y + 1, pos.z);
        let outcome = crate::bone_meal::apply_bone_meal(&clicked, &above, bone_meal_rng);
        // One helper for both consuming arms — vanilla `itemStack.consume(1)`,
        // the identical shrink the composter's `Consumed` arm performs.
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

    // Issue #329: right-clicking a bed sets the player's per-player respawn
    // point (vanilla's own bed-block empty-hand-use → start-sleep-in-bed
    // → `setRespawnPosition` chain). A bed click is an *interaction*, not a
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
        // night-skip vote. Vanilla's own start-sleep-in-bed routine calls
        // `sleepStatus.setSleeping` — this arm is this
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

    // Issue #532: `useWithoutItem`. **Ahead of the placement branch**, exactly like
    // vanilla, whose own use-item-on routine tries `state.useItemOn`/`useWithoutItem`
    // before its own block-item place routine — otherwise right-clicking a door while holding a
    // block would build instead of opening it. See `crate::hand_use` for the five
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
                // The same neighbour fan-out a placement owes its neighbours
                // (issue #465). This is what makes a lever actually power the
                // wire beside it rather than merely look flipped: without it the
                // redstone model is correct and unreachable from a player's hand,
                // which is precisely the state #314/#315/#319 were left in.
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
    // Which native slot this click reads from. Vanilla's own use-item-on routine
    // uses `player.getItemInHand(hand)` for the spawn-egg, flint-and-steel
    // and block-placement branches below — all three share this one
    // resolution point via `held_item`, so an item held only in the off hand
    // now reaches them instead of the main hand's slot always winning.
    let hand_native = if hand == 1 {
        crate::inventory::OFFHAND_NATIVE
    } else {
        usize::from(inventory.selected_hotbar_slot())
    };
    let held_item = inventory.native(hand_native).map(|stack| stack.item.to_string());

    // Vanilla's own spawn-egg-item use-on routine. Between the block's own `useWithoutItem` above and
    // its own block-item place routine below, which is vanilla's order in its own use-item-on routine —
    // ahead of the placement branch, or an egg held over air would place a block;
    // behind the `hand_use` arm, or a click on a lever would eat the egg. See
    // `crate::spawn_egg` for the placement rule and `docs/spawn-eggs.md` for why
    // the item-to-entity mapping is a checked derivation rather than a table.
    //
    // `block_entities` is consulted first because vanilla's own spawn-egg-item use-on routine tests
    // `getBlockEntity(pos) instanceof Spawner` before anything else, and that
    // branch re-keys the spawner instead of spawning. Nothing is modelled for a
    // spawner yet, so the guard is "there is a spawner here, do nothing" rather
    // than a re-key — the honest behaviour, and it keeps the egg from spawning a
    // mob vanilla would not.
    if let Some(item) = held_item.as_deref() {
        let spawner_here = block_entities.with(|reg| {
            reg.get(pos)
                .is_some_and(|entity| entity.type_id() == "minecraft:spawner")
        });
        if !spawner_here {
            match crate::spawn_egg::apply_spawn_egg(
                item,
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
                    // `itemStack.consume(1, user)`, *after* the spawn succeeded —
                    // the same shrink-and-report pair the composter and brewing
                    // arms above perform, including the window-0 hotbar slot
                    // update so the held count visibly drops.
                    //
                    // Routed through `consume_one` rather than shrinking inline, so
                    // vanilla's own item-stack consume routine's `!hasInfiniteMaterials()` gate
                    // applies: a creative player's eggs are not used up, which is
                    // vanilla and was not true of the inline shrink this replaces.
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

    // Vanilla's own minecart-item use-on routine — a rail-targeted placement, checked ahead of the
    // generic block-placement branch below for the same reason the spawn-egg
    // arm above is: a minecart item is not a `BlockItem`, so that branch could
    // never place one anyway, and vanilla's own `useOn` returns `FAIL` (not
    // `PASS`) on a non-rail target — stop here rather than falling through to
    // anything else, matching that refusal.
    if let Some(item) = held_item.as_deref() {
        if let Some(kind) = crate::mobs::minecart::MinecartKind::from_item(item) {
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
    // Vanilla's route is its own flint-and-steel-item use-on routine → `level.setBlock(fire)` →
    // its own fire-block on-place routine, whose portal branch runs the frame search **from the
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
    if held_item.as_deref() == Some("minecraft:flint_and_steel") {
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
    // `TntBlock::useItemOn` — flint and steel or a fire charge clicked
    // directly on a TNT block primes it and clears the block. Checked
    // against the **clicked** cell (`pos`), not `neighbour` the portal arm
    // above reads: vanilla's `useItemOn` runs on the block that was actually
    // clicked, not the face it was clicked from.
    //
    // No `tnt_explodes` gamerule gate here — this call site has no
    // `WorldStateHandle` in scope, matching the portal arm just above, which
    // takes no durability-damage gate either (this crate's own item stacks
    // carry no durability at all — see that arm's own comment). Both are
    // therefore true unconditionally, which is vanilla's own default.
    if matches!(
        held_item.as_deref(),
        Some("minecraft:flint_and_steel" | "minecraft:fire_charge")
    ) {
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
            // `itemStack.consume(1, player)` for the fire charge;
            // `itemStack.hurtAndBreak(1, ...)` for flint and steel, which
            // this crate does not model (see this arm's own comment) — so
            // only the charge is shrunk.
            if held_item.as_deref() == Some("minecraft:fire_charge")
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
    if held_item.as_deref() == Some("minecraft:ender_eye") {
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
    // all and *which* block it writes. `block_entity_for_item` no longer
    // makes that decision — it only supplies the live `BlockEntity` for
    // the six items this crate ticks, and is consulted second.
    let placed = held_item
        .as_deref()
        .and_then(|item| block_items::block_for_item(item).map(|block| (item, block)));
    // Vanilla's own `SlabBlock.canBeReplaced` is the one
    // `canBeReplaced` override a hand placement can hit, and without it a slab
    // clicked onto a matching half-slab lands in the cell *above* instead of
    // doubling. Every other block reaches the plain air-or-fluid test.
    let doubling_slab = placed.is_some_and(|(_, block)| slab_doubles(&clicked, block, face, cursor));
    let target = if is_air_or_fluid(&clicked) || doubling_slab {
        pos
    } else {
        neighbour
    };
    let target_state = source.block_state(target.x, target.y, target.z);
    // Every cell the placement's neighbour fan-out rewrote (issue #465) —
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
        if let Some((item, block_name)) = placed {
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
                match placed_block_state(block_name, &ctx, |p| source.block_state(p.x, p.y, p.z)) {
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
            if let Some((entity_block, mut entity)) = block_entity_for_item(item) {
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
                // Vanilla's own sign-item use-on → sign-block open-text-edit →
                // `setAllowedPlayerEditor(player.getUUID())`: the placer is
                // granted edit permission on the sign they just placed, the
                // one grant site this crate models (see `SignData::editor`'s
                // own doc for the other vanilla grant site this does not
                // reach yet).
                if let crate::block_entities::BlockEntity::Sign(sign) = &mut entity {
                    sign.editor = Some(placer);
                }
                block_entities.with(|registry| registry.insert(target, entity));
            } else if let Some(type_name) = lodestone_data::block_states::state_id(&state)
                .and_then(lodestone_data::block_entity_types::block_entity_type)
                .and_then(lodestone_data::block_entity_types::block_entity_type_name)
            {
                // Vanilla's own `LevelChunk.setBlockState` creates a block entity
                // from the *state* alone (`state.hasBlockEntity()`), for every
                // block-entity type — not only the dozen `block_entity_for_item`
                // simulates real behaviour for. A skull, banner, jukebox, … placed
                // through this path still needs a registry record: its client-side
                // render is entirely block-entity-driven (see
                // `ChunkColumn::missing_block_entity_states`'s own doc, which
                // repairs the identical gap for a chunk loaded off disk), so with
                // no record here the position is on the wire as a bare state and
                // draws nothing until an unrelated later block update lets the
                // client synthesize an empty record for itself. `Nbt::End` matches
                // what a save/load round trip already reconstructs for a type this
                // crate does not otherwise model (`chunk_nbt::block_entity_from_nbt`'s
                // own catch-all arm).
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
            // Vanilla's own block-item place routine's own `level.playSound(player, …)`
            // — the placer is vanilla's `except` argument,
            // and here it must be, because the shell predicts its own place
            // sound. `roll` stands in for vanilla's per-play `nextLong()`: it is
            // already a live draw from this connection's `SpawnRng`, one per
            // right-click, which is exactly the variant-picking seed's shape.
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
            // Issue #239: a placed carved pumpkin or jack o'lantern may
            // complete a snow- or iron-golem block pattern — vanilla's
            // own carved-pumpkin-block set-placed-by → try-spawn-golem chain.
            // `MobSim::try_construct_golem` is a pure detection query with no
            // block-write authority of its own (see its own doc comment), so
            // this is the caller that owns clearing the consumed pattern
            // cells to air, exactly as vanilla's own carved-pumpkin-block
            // clear-pattern-blocks routine does.
            if block_name == "minecraft:carved_pumpkin" || block_name == "minecraft:jack_o_lantern"
            {
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
            // A placed wither skeleton skull (or wall skull) may complete the
            // soul-sand-and-skull pattern — vanilla `WitherSkullBlock
            // .setPlacedBy` → `checkSpawn`. Same shape as the golem trigger
            // above: `MobSim::try_construct_wither` is a pure detection query
            // with no block-write authority of its own (see its own doc
            // comment), so this is the caller that owns clearing the
            // consumed pattern cells to air.
            if block_name == "minecraft:wither_skeleton_skull"
                || block_name == "minecraft:wither_skeleton_wall_skull"
            {
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
            // Issue #465: placing a block is a mutation like any other, so it
            // owes its neighbours the same fan-out a random tick or a drained
            // scheduled tick already performs. Without this the redstone model
            // is correct but unreachable from any player action — dust placed
            // beside a powered line stays at `power=0` forever.
            let (mut fanout, scheduled) = propagate_placement_with_entities(source, target, Some(block_entities));
            changed.append(&mut fanout);
            piston_records.extend(moving_piston_records(&scheduled));
            // Issue #465: and the delayed half, which `propagate_placement`
            // structurally cannot host — the queue those land in belongs to the
            // world tick loop. Handed over unconditionally rather than only
            // when `changed` is non-empty: the delayed families are exactly the
            // case where the fan-out rewrites *nothing* now and schedules
            // instead, so gating on a synchronous change would drop precisely
            // the placements this exists for.
            block_ticks.request_scheduled_ticks(scheduled);
            // And the same seeding hook `destroy_block` performs, for the same
            // reason: a block placed into a flow, or beside a source, has to
            // start it re-evaluating. See `crate::fluid::ticks_after_edit`.
            block_ticks.request_scheduled_ticks(crate::fluid::ticks_after_edit(target));
            // Vanilla's own falling-block on-place routine: a placed sand or gravel block owes itself a
            // gravity check two ticks out. Same shape and same call site as the
            // fluid seeding above, and empty for every other block, so no guard.
            //
            // **This is what makes placing sand in mid-air fall at all.** Until
            // now the only route to the gravity check was a neighbour update,
            // which is exactly the owner's report — "they don't fall when I place
            // them in the air, they only fall when I place another block beside
            // them". `state` is the placed state rather than the item name,
            // because `gravity_tick::is_gravity_block` matches the base of a real
            // block state.
            block_ticks
                .request_scheduled_ticks(crate::gravity_tick::ticks_after_place(target, &state));
            // Vanilla's own block-item place routine's own tail: `itemStack.consume(1, player)`. Nothing
            // in this crate did it, so **every placement was free** — the block was
            // written, the client predicted its own hotbar and the server never
            // agreed, so the stack came back on the next window sync.
            //
            // Vanilla's own item-stack consume routine is
            // `if (entity == null || !entity.hasInfiniteMaterials()) shrink(count)`,
            // so a creative placement consumes nothing and "placing does not use up
            // the block" is *correct* there. The gate is explicit rather than
            // implied for exactly that reason.
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

/// The moving-piston records a batch of relative-delay scheduled ticks implies.
///
/// A `moving_piston` block update tells a client that a cell is animating and
/// nothing at all about *which* block is travelling through it — the record that
/// says so lives in the pending commit tick (`crate::piston::finish_kind`). The
/// connection paths below send their own `block_update` packets rather than
/// publishing on [`crate::BlockTickFeed`], so unlike the world tick loop (whose
/// equivalent is `crate::tick`'s `publish_moving_piston`) they have to pair the two
/// up themselves. Without this a piston triggered by a lever animates nothing: the
/// client holds a `moving_piston` cell with an empty record for two ticks and then
/// the finished block appears, which is the snap the two-phase move exists to
/// replace.
///
/// The record must be sent **after** the cell's own `block_update`, so the state
/// write has already created the record this fills in.
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
///
/// Test-only now: every real production call site was moved to
/// [`propagate_placement_with_entities`] once the command-block redstone-edge
/// arm needed a live [`BlockEntityHandle`] threaded through, which this
/// `None`-only wrapper cannot supply. Kept for the oracle gates and unit tests
/// that have no registry to hand it.
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

/// [`propagate_placement`], plus a live [`BlockEntityHandle`] threaded into
/// [`crate::random_tick::react_at_placement_with_entities`]'s fan-out — the
/// path a lever flip, a button press or a placed block's own neighbour
/// notification reaches a command block's `neighborChanged` through. `None`
/// behaves exactly like [`propagate_placement`] itself.
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

/// Per-connection difficulty + game-rule session state (issue #268).
///
/// This crate has no `GameRules` registry — see [`apply_game_rule_changed`]'s
/// own doc comment — so this is deliberately the smallest state that lets the
/// round trip (a `ServerBound::DifficultyChanged`/`DifficultyLockChanged`/
/// `GameRuleChanged` request in, a confirmation back out) be real and
/// observable without inventing a full world-rules model. Per-connection
/// rather than shared across connections, matching `player_pos`/`vitals`/
/// `fall`'s existing precedent in [`serve_play`] — a real scope cut for
/// open-to-LAN (two connections would each hold an independent view, and
/// `WorldAdminState` used to live here: a `Difficulty` + lock + a bare
/// `HashMap<String, String>` of game rules, constructed as a **stack local inside
/// `serve_play`**. That is one store per accepted socket, so two LAN players each
/// held a private, divergent view, and nothing anywhere read either. It is now
/// [`crate::world_state::WorldStateHandle`], shared with the tick loop.
///
/// (This crate did have no permission/operator model at all when the
/// paragraph above was first written — that was issue #268's own finding.
/// `crate::access` closed that gap; the permission check itself lives at each
/// `dispatch_play_packet` call site, see `COMMANDS_GAMEMASTER_LEVEL`'s own
/// doc, not in this state struct.)
/// Applies a difficulty-change request (`ServerBound::DifficultyChanged`),
/// mirroring vanilla's own `handleChangeDifficulty`.
/// Vanilla gates this on `Permissions.COMMANDS_GAMEMASTER` **or**
/// `isSingleplayerOwner()` — the caller in `dispatch_play_packet` now performs
/// that same check (`commands.permission_level`, which already resolves an
/// unconfigured/no-ops world to the singleplayer-owner shape) before this
/// function ever runs, so a refused request reaches here having changed
/// nothing. Confirms back to the *same* connection via
/// [`ServerProtocol::encode_change_difficulty`]; vanilla instead broadcasts
/// to every player (`MinecraftServer::setDifficulty` → `PlayerList`), which
/// needs cross-connection state this crate does not share — see
/// [`WorldAdminState`]'s own doc comment.
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

/// Applies a game-rule change request (`ServerBound::GameRuleChanged`),
/// mirroring vanilla's own `handleSetGameRule`.
/// The permission check (see [`apply_difficulty_change`]'s own doc comment)
/// happens before this function is ever called — a refused request arrives
/// with an empty `entries`, so the reply below is naturally an empty
/// confirmation rather than this function needing its own refusal branch.
/// Rule-name/value validation, by contrast, **is** real: vanilla looks each
/// key up in vanilla's own game-rule registry and parses `value` through that
/// rule's own type (`GameRule<T>::deserialize`), and
/// [`crate::world_state::WorldStateHandle::set_rule`] does the same against
/// this crate's own rule set, rejecting an unknown key or an unparseable
/// value rather than storing it verbatim. Confirms back to the same
/// connection with exactly the entries that were actually set; vanilla's
/// `broadcastGameRuleChangeToOperators` instead sends one packet per changed
/// rule to every operator.
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
    // **Validated now**, which is what vanilla does too
    // (its own game-rule registry lookup + `GameRule<T>::deserialize`). The old
    // store kept every `(String, String)` verbatim, so `randomTickSpeed` — the
    // pre-26.2 spelling — was accepted, echoed back, and then never read by
    // anything, because the reader asks for `random_tick_speed`. The player saw
    // their rule confirmed and no behaviour change.
    //
    // Only the entries that were actually *set* are confirmed back, so a rejected
    // key is visibly absent from the reply rather than silently agreed with.
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

/// Applies a `client_command` request (`ServerBound::ClientCommand`, issue
/// #270), mirroring `ServerGamePacketListenerImpl::handleClientCommand`'s
/// modellable ordinals.
///
/// # `action == 1`, `REQUEST_STATS`
///
/// Issue #338. Vanilla answers with `player.getStats().sendStats(player)`
/// — a full `ClientboundAwardStatsPacket`.
/// Here the same reply is built from [`AdvancementManager::stats_snapshot`] of
/// this connection's [`crate::advancements`] store and lowered through
/// [`ServerProtocol::encode_award_stats`] (a no-op default on protocols
/// without a stats encoder). This is the *framework* reply: individual
/// statistic producers (block-break mined counters, etc.) are follow-up wiring
/// of this epic, so a fresh session typically answers an empty batch.
///
/// # `action == 0`, `PERFORM_RESPAWN`
///
/// **The respawn position is the player's bed when they have a usable one**, and
/// the world spawn otherwise — vanilla's own
/// find-respawn-position-and-use-spawn-block routine, resolved through
/// [`crate::world_spawn::resolve_bed_respawn`]. The bed block is **re-read at
/// death time** rather than trusted from when the point was set, so a bed that has
/// since been broken (or walled in) falls back to the world spawn instead of
/// returning the player inside whatever replaced it. That is vanilla's
/// `Optional.empty()` arm, which it answers with `NO_RESPAWN_BLOCK_AVAILABLE`.
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
/// Mirrors `sendGameRuleValues`, including its permission check (see
/// [`apply_difficulty_change`]'s own doc comment for the same
/// `commands.permission_level` reasoning) — a refused request gets nothing
/// back, matching vanilla's own silent `else` branch here rather than the
/// "always echo something" shape the mutating packets above take. On success,
/// replies with every rule this connection's own [`WorldAdminState`] has ever
/// had set, via the same [`ServerProtocol::encode_game_rule_values`]
/// confirmation [`apply_game_rule_changed`] already uses. Vanilla instead
/// enumerates the full `GameRules` registry, including every rule at its
/// default — this crate has no such registry (see [`WorldAdminState`]'s own
/// doc comment), so a rule that was never explicitly set is simply absent
/// from the reply rather than reported at a registry default.
#[allow(clippy::too_many_arguments)]
async fn apply_client_command<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    vitals: &mut PlayerVitals,
    // The fall accumulator, reset by the respawn arm. Vanilla resets
    // `fallDistance` on every position snap (vanilla's own entity position-snap sites) and a
    // respawn is one — `FallTracker::reset`'s own doc comment used to say
    // "nothing calls this yet", and this is the caller it was waiting for.
    fall: &mut FallTracker,
    // The world spawn this connection joined at, resolved once in
    // `serve_connection`'s `ConfigurationFinished` arm. Vanilla's `PlayerList::
    // respawn` re-teleports the rebuilt player; without a position the client
    // would respawn wherever the corpse was, which for a fall death is at the
    // bottom of whatever killed them.
    //
    // The **fallback**, used when this player has no bed point or their bed is no
    // longer usable — which is also exactly what a player with no bed gets in
    // vanilla.
    world_spawn: Vec3,
    // This player's bed point, if they have set one. Resolved against `source`
    // rather than used directly: see this function's own doc comment for why the
    // bed block is re-read at death time.
    respawn: Option<RespawnPoint>,
    // Read-only, and only for the bed re-validation above.
    source: &S,
    world: &crate::world_state::WorldStateHandle,
    advancements: &mut AdvancementManager,
    player_uuid: uuid::Uuid,
    action: i32,
    // `sendGameRuleValues`'s own gate (`action == 2`, `REQUEST_GAMERULE_VALUES`
    // — see this function's own doc comment). `dispatch_play_packet`'s already-
    // resolved `commands.permission_level`, the same value `COMMANDS_GAMEMASTER_LEVEL`
    // gates the mutating `GameRuleChanged`/`DifficultyChanged` packets with.
    permission_level: u8,
    // Whether `source` above is a portal-travelled dimension rather than the
    // connection's home one — `matches!(source_ref, SourceRef::Dimension(_))`,
    // computed by the caller because only it holds the `SourceRef` this `&S` was
    // unwrapped from. `encode_respawn` always tells the client
    // `minecraft:overworld` (this crate's join dimension), so a death away from
    // home leaves the *server's* own `travelled`/`view`/`join_stream` still
    // pointed at the dimension the player died in — no chunk stream is ever
    // re-centred on the respawn position, so the client (correctly told it is
    // home) receives no terrain there at all. `dimension_reset` is how this
    // function asks its caller to run that reset; see its own doc.
    away_from_home: bool,
    // Set to the resolved respawn position when a respawn just happened *and*
    // `away_from_home` was true — the signal `dispatch_play_packet`'s caller
    // (`serve_play`, the only one with `home`/`pending_travel` in scope) uses to
    // run the same forget-chunk/recentre/rebuild-join-stream sequence
    // `travel_through_portal` runs for an outbound trip, then park a trip home.
    // `None` on entry and left `None` for every other action or a same-dimension
    // respawn, so a caller that never checks it loses nothing.
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
            // The bed first, the world spawn as the fallback. `resolve_bed_respawn`
            // answers `None` for a broken or walled-in bed, which is the case this
            // whole indirection exists for.
            let target = respawn
                .and_then(|point| crate::world_spawn::resolve_bed_respawn(source, point))
                .unwrap_or(world_spawn);
            // Order matters and mirrors `PlayerList::respawn`: the respawn record
            // and the placement teleport first, then the vitals the client's HUD
            // reads. Sending health *before* the respawn packet would refill the
            // hearts while the death screen was still up.
            for directive in proto.encode_respawn(target) {
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
            // `encode_respawn` above already told the client it is in
            // `minecraft:overworld` — this crate's one respawn dimension,
            // matching vanilla's no-bed/no-anchor default. If the player died
            // away from home, the *server's* own dimension tracking (`travelled`,
            // `view`, `join_stream`) is still pointed at the dimension they died
            // in, and nothing above touches it — so ask the caller to run the
            // same reset a portal trip home runs, or the client sits at a
            // correctly-labelled position with no terrain ever streamed to it.
            if away_from_home {
                *dimension_reset = Some(target);
            }
        }
        1 => {
            let snapshot = advancements.stats_snapshot(player_uuid);
            apply(conn, state, proto.encode_award_stats(&snapshot)).await?;
        }
        2 => {
            // Vanilla logs a warning and sends nothing on refusal
            // (`sendGameRuleValues`'s own `else` is the only branch that
            // replies) — matched here rather than the "always echo something"
            // shape the mutating packets above take, since there is no prior
            // client-guessed value to correct.
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
/// (vanilla's own creative-mode-slot-set handler's
/// `player.inventoryMenu.getSlot(slotNum)`), so no new mapping is needed.
/// `slot` values that table does not recognise (`0`, the crafting output; any
/// negative value, vanilla's "drop into the world" case) are silent no-ops
/// here — this crate has no world-drop model.
///
/// **Gated on creative mode**, which vanilla also does
/// (`handleSetCreativeModeSlot`'s `player.hasInfiniteMaterials()`). That gate is
/// load-bearing rather than cosmetic: this packet *is* a mint-anything channel by
/// design, so leaving it ungated would reopen — through a different packet — the
/// exact hole `apply_container_clicked` was rewritten to close. A survival client
/// sending it gets nothing.
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

/// The `stateId` the join snapshot carries, and it is **`1`, not `0`**.
///
/// Vanilla's own container-menu synchronizer-setter calls `sendAllDataToRemote`,
/// which hands the list to `ContainerSynchronizer::sendInitialData` — and
/// `ServerPlayer`'s implementation of that sends
/// `new ClientboundContainerSetContentPacket(container.containerId,
/// container.incrementStateId(), …)`. `stateId` starts at `0` and
/// `incrementStateId` is `(stateId + 1) & 32767`, so the *first* content packet a
/// real client ever receives for its own inventory menu carries `1`.
///
/// # Why the other window-`0` sends in this file can keep their constant `0`
///
/// Measured against the 26.2 decompile rather than assumed, because the obvious
/// worry — "a wrong state id makes the client reject the next update" — is
/// **backwards**. No client validates this field:
/// Vanilla's own client-side container-content handler calls
/// `menu.initializeContents(packet.stateId(), …)` unconditionally, and
/// `initializeContents` simply assigns `this.stateId = stateId`. Ditto
/// `AbstractContainerMenu::setItem` for a single-slot update, and this workspace's
/// own client does the same (`lodestone_game::reconcile::MenuPair::sync_state_id`
/// adopts whatever arrived).
///
/// The only consumer anywhere is a **server** checking a click's *echoed* id:
/// vanilla's own container-click handler's
/// `packet.stateId() != this.player.containerMenu.getStateId()` picks the
/// `broadcastFullState()` branch. That is the other direction of travel, and this
/// crate does not perform that check at all (the `ServerBound::ContainerClicked`
/// arm binds `state_id: _`). So a window-`0` id that moves backwards costs nothing
/// observable on either known client — which is why the join snapshot is faithful
/// to the record here without threading a real counter through
/// [`dispatch_play_packet`] for a field nothing reads.
const JOIN_CONTENT_STATE_ID: i32 = 1;

/// The window-`0` inventory snapshot a joining player is owed — vanilla's own
/// per-player inventory-menu initializer.
///
/// # Why this exists
///
/// **Nothing sent it.** The encoder
/// ([`ServerProtocol::encode_container_content`]) and its `V770ServerProtocol`
/// implementation were both complete, and the client decodes the packet and folds
/// it through `lodestone_game::menus::Menus`, but every producer in this file was
/// reactive: [`open_container_screen`] (a menu was opened),
/// [`apply_container_clicked`] (a click disagreed) and [`apply_recipe_placed`].
/// A rejoining player was therefore never told what they were holding, and their
/// screen drew the fresh-`Menu` default — an empty grid — until the first click
/// produced a disagreement and the corrective resync flushed all 46 slots at once.
/// That is the reported symptom exactly: *"my inventory is empty, but if I
/// shift-click something then all the items pop in"*. The items were never lost;
/// `PlayerData::to_inventory` had restored them before this function's first line.
///
/// # Placement, and it is deliberate
///
/// Vanilla's own new-player placement routine calls `initInventoryMenu()` **last** — after the
/// abilities/held-slot/recipe packets, after `sendPlayerPermissionLevel`, after the
/// placement `teleport`, after the player-info adds and after `sendLevelInfo`. So
/// the top of [`serve_play`] is the faithful position: `serve_connection_inner` has
/// already done all of those, and the deferred chunk stream that this loop drains
/// corresponds to vanilla's own per-player chunk sender feeding columns over *subsequent*
/// ticks. Sending it later — say, lazily on the first movement packet — would
/// reintroduce the same class of bug for a player who joins and stands still.
///
/// # The packet is `container_set_content`, not `set_player_inventory`
///
/// Both exist in 26.2 and this workspace's client decodes both, so it is worth
/// recording why only one is correct here.
/// `ClientboundSetPlayerInventoryPacket` is a **single-slot** record — `(int slot,
/// ItemStack contents)` — and vanilla's only producer is
/// vanilla's own inventory-update-packet builder, called from `Inventory.add` to
/// acknowledge one item pickup. It carries no slot list and no cursor, so it cannot
/// express a snapshot. `ClientboundContainerSetContentPacket` is what
/// `sendInitialData` sends, and `handleContainerContent`'s `containerId == 0` arm
/// routes it to `player.inventoryMenu` — which is why window `0` is right.
///
/// The carried (cursor) stack is part of the snapshot rather than an afterthought:
/// `sendAllDataToRemote` reads `getCarried()` and forces `remoteCarried` to it in
/// the same pass. A player who quit mid-drag holds nothing on the server (the
/// `ServerBound::ContainerClosed` arm returns the cursor to the inventory or the
/// floor), so this is `None` in practice today — but reading it from
/// [`ClickState`](crate::container_click::ClickState) rather than hardcoding `None`
/// means it stays right when a disconnect path that preserves a cursor appears.
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

/// The experience bar a joining player is owed — vanilla's first
/// `ClientboundSetExperiencePacket`.
///
/// # Why this exists
///
/// **The XP bar never appeared, in survival as well as creative**, and the creative
/// gate was a red herring (vanilla hides the bar client-side via
/// its own has-experience check, and still sends the packet). Same island shape as
/// [`join_inventory_snapshot`], one step further along: the encoder existed in both
/// the [`ServerProtocol`] trait and `V770ServerProtocol`, the client decodes
/// `SET_EXPERIENCE` into `ClientEvent::ExperienceChanged`, and the HUD draws the bar
/// from it — but the *only* producer in this crate was the furnace-close arm of
/// [`dispatch_play_packet`], which pays out banked smelting XP. So a player who had
/// never closed a furnace was never sent the packet at all, and the bar had no
/// values to draw from.
///
/// # Where vanilla sends it, and why "on join" is the faithful answer
///
/// Not from `placeNewPlayer` — from vanilla's own per-player tick routine, which sends whenever
/// `this.totalExperience != this.lastSentExp`. `lastSentExp` is initialised to
/// `-99999999`, so the comparison is true on the **first tick after any join** even
/// for a player with zero experience, and the packet goes out unconditionally.
/// Every mutator (`setExperiencePoints`, `setExperienceLevels`,
/// `giveExperienceLevels`, `onEnchantmentPerformed`) additionally forces
/// `lastSentExp = -1` so that a change to *progress or level alone* — which leaves
/// `totalExperience` untouched — still resends. The equivalent here is: send once at
/// join, and send after every [`crate::experience::PlayerExperience`] mutation. The
/// furnace arm already does the latter.
///
/// # Argument order
///
/// `(progress, level, total)`, matching the trait and **the wire**, which is not
/// vanilla's constructor order. `ClientboundSetExperiencePacket`'s field
/// declaration and its constructor both read `(progress, total, level)`, while its
/// `write` method emits `writeFloat(progress)`, `writeVarInt(level)`,
/// `writeVarInt(total)`. Reading the constructor call in `doTick` instead of the
/// record's own codec is how the two integers get transposed — and they are adjacent
/// VarInts, so a swap costs nothing at the wire level and silently shows the wrong
/// number on the bar.
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
/// connection, the same "send once at join, and after every mutation"
/// moments [`join_experience`]'s own doc names. A one-line wrapper rather
/// than inlining the `if let Some(registry) = players` at each of those call
/// sites, so a future field on [`crate::commands::PlayerCandidate`]'s
/// experience mirror only has one call to update, not eight.
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
/// Sent once at join (mirroring [`join_experience`]'s "no values at all
/// without an explicit send" reasoning — vanilla's `AttributeMap` is
/// synced on `addEntity`/`ServerPlayer` placement, so a client that never
/// receives this packet has no armour attribute at all, not a zero one) and
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
/// vanilla's own `AbstractContainerMenu.doClick`, run over the menu read out of
/// this connection's real state. The client's `changed_slots`/`carried_item`
/// prediction is **never stored** — it is compared against what was derived, and a
/// disagreement sends a full corrective `container_set_content`. So an honest
/// client sees no extra traffic and a client naming an item it does not own is
/// corrected on the same packet.
///
/// That closes the hole this function used to be: it applied the client's diff
/// verbatim, so any client could mint any item in any slot by claiming it. Issue
/// #529 had closed the crafting *result* alone; the general case is this.
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
    // Issue #150. The narrow crafting-station hook registry — see
    // `apply_use_item_on`'s own `hooks` comment for why this is a targeted
    // handle rather than the whole `WorldStateHandle`.
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> (Option<ServerDirective>, Vec<ItemStack>) {
    // Which menu, and where its non-player slots live.
    let mut open = open_container;

    // The workstation economy (anvil/grindstone/smithing, issues #253-#255) is a
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
    // What the client believed before this click — the baseline the agreement check
    // below rebuilds its prediction on top of. Vanilla keeps this as the menu's
    // `remoteSlots`; here the last thing we sent *was* this state, because every
    // disagreement is answered with a full content packet.
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
    // `ServerboundSelectBundleItemPacket`'s last claim for this menu slot —
    // `container_click::pickup`'s own `tryItemClickBehaviourOverride`
    // right-click-extract branch reads this to pick which nested item comes
    // out, exactly as vanilla's own container-menu selected-bundle-item-index field does.
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
    // # Comparing only the claimed slots was vacuous, and that was the bug
    //
    // This used to walk `claimed_slots` alone, so a client that claimed *nothing*
    // agreed by construction. That is exactly what a client does for every change it
    // cannot predict — above all the **crafting result**, which is server-derived — so
    // the result slot was never sent, the screen drew its own dimmed ghost forever,
    // and a shift-clicked craft only showed up on the next full content packet (i.e.
    // after closing and reopening the table). Vanilla has no such hole: `doClick` is
    // followed unconditionally by `broadcastChanges`, which diffs **every** slot
    // against `remoteSlots`, and `slotChangedCraftingGrid` additionally pushes slot 0
    // on any grid change (vanilla's own crafting-menu slot-changed hook).
    //
    // An honest prediction still costs no traffic — the control for that is
    // `a_claimed_item_is_never_stored_and_the_client_is_corrected`'s second half.
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
/// `hooks` is issue #150's plugin seam: the vanilla result computed above is
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
    // Vanilla's own anvil-menu may-pickup gate: `(player.hasInfiniteMaterials() ||
    // player.experienceLevel >= this.cost.get()) && this.cost.get() > 0`.
    // `this.cost` is `crate::anvil::compute`'s own `cost` field, re-derived
    // from the pre-click cells and pending rename — never stored, the same
    // "recompute rather than cache" choice `workstation_result` above already
    // makes. `Grindstone`/`Smithing` pass `None`: neither's vanilla result
    // slot overrides `mayPickup` (its own grindstone-menu anonymous result `Slot`
    // only overrides `mayPlace`/`onTake`; `SmithingMenu` inherits
    // `ItemCombinerMenu`'s own default-true `mayPickup`).
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

/// [`ServerBound::EditBook`]'s consumer — vanilla's own update-book-contents/
/// sign-book handlers, reached the same way its own edit-book handler
/// gates it: `slot` must be a hotbar index or the off-hand (vanilla's
/// `Inventory.isHotbarSlot(slot) || slot == 40`), and the item sitting there
/// must be a `minecraft:writable_book` — vanilla's `carried.has(DataComponents
/// .WRITABLE_BOOK_CONTENT)` gate, which is always true for that item because
/// vanilla's own item registration for writable books registers the component as a **prototype default**
/// (`WritableBookContent.EMPTY`), not only after a first edit. This crate has
/// no general item-prototype default-component census to reproduce that
/// distinction (see `ItemComponents`'s own doc on *effective* vs. *patch*
/// fields), so the item's own canonical key stands in for it here — every
/// `minecraft:writable_book` this crate can ever produce is editable, exactly
/// as vanilla's default makes it.
///
/// `pages`/`title` reach here already filtered to nothing (this crate runs no
/// chat-filtering service, the same call every other text-carrying consumer
/// makes) and already length/count-capped by the wire decode (`EditBook`'s
/// own `#[mc(max = 100)]` list cap and its `pages`/`title` string caps).
///
/// Returns the native slot written and the item now sitting there, for the
/// caller to resend via `CONTAINER_SET_SLOT` — `None` when the gate refuses
/// (out-of-range slot, no item there, or not a writable book), matching
/// `updateBookContents`/`signBook`'s own silent no-op on a failed `has` check.
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
        // `signBook`: transmute to a written book, dropping the draft
        // component (`writtenBook.remove(DataComponents
        // .WRITABLE_BOOK_CONTENT)`) and setting the written one — generation
        // `0` and `resolved: true`, matching `signBook`'s own literal
        // `new WrittenBookContent(title, author, 0, pages, true)`.
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
        // `updateBookContents`: overwrite the draft pages in place, no
        // transmute.
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
    let pos = tracked.pos;
    let updated = block_entities.with(|reg| {
        let Some(BlockEntity::Beacon(beacon)) = reg.get_mut(pos) else {
            return None;
        };
        beacon.payment.as_ref()?;
        if !crate::beacon::validate_beacon_effects(primary.as_deref(), secondary.as_deref(), beacon.levels) {
            return None;
        }
        beacon.primary_effect = primary;
        beacon.secondary_effect = secondary;
        // `paymentSlot.remove(1)`.
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
            crate::beacon::encode_beacon_effect(primary.as_deref()),
        ),
        proto.encode_container_data(
            tracked.window_id,
            2,
            crate::beacon::encode_beacon_effect(secondary.as_deref()),
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
/// lapis/levels, or — vanilla's own `newEnchantment.isEmpty()` guard — a roll
/// that happened to produce nothing.
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
    // Issue #150: the loom and stonecutter share this same packet
    // (`ContainerButtonClick`) but neither shape nor pricing in common with
    // the enchanting table below — vanilla's own loom-menu/stonecutter-menu click-menu-button routines
    // both just pick an offer, with no lapis or XP cost at all. See
    // `apply_workstation_button_click`'s own doc.
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

/// Lays a recipe-book recipe out in the open crafting grid (issue #529 step 4).
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
    // Vanilla routes a container throw through the *same* `drop(stack, false,
    // true)` the `Q` key uses (its own container-menu doClick outside case →
    // its own player-drop routine), so it gets the same hand position and the same forward
    // impulse. This used to release at the eye with **zero** velocity and a
    // 10-tick pickup delay, with a comment saying facing was not tracked here — it
    // is (`player_rot`), and the effect of the old shape was that a stack thrown
    // out of an open window dropped straight onto the player's feet and was
    // immediately picked back up, which reads as the throw not working.
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
/// Vanilla's own `ServerPlayer.drop(boolean)`, which is
/// three steps: `Inventory.removeFromSelected(all)` takes the items, the menu is
/// told the selected slot's *new* contents, and vanilla's own entity-drop routine spawns the
/// entity with [`crate::block_drops::thrown_item_velocity`].
///
/// # The slot update is a deliberate divergence from vanilla, not a port of it
///
/// **Vanilla sends nothing here.** There is no drop ack, and
/// vanilla's own per-player drop routine's `containerMenu.setRemoteSlot(slotIndex, …)` is *not* a
/// send — it updates the server's record of what the client is believed to hold,
/// which **suppresses** the corrective broadcast that would otherwise follow. That
/// works because the client predicts the drop itself (`lodestone-client`'s
/// `drop_selected` does, and its doc records that an unpredicted drop leaves the
/// count permanently wrong — the item really is gone server-side).
///
/// This crate has no `setRemoteSlot` model, so "send nothing" is not available in
/// the same sense: if the server *rejects* the drop where the client predicted one
/// — a stale selected index, a slot the server reads as empty — nothing ever
/// reconciles it and the client shows a ghost item until the window is reopened.
/// One `container_set_slot` carrying the authoritative content closes that, and it
/// is inert in the common case because it equals what the client predicted.
///
/// If a `setRemoteSlot` equivalent ever lands, this is the send to remove.
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
    // `removeFromSelected(all)`: the whole count, or one item.
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
/// Vanilla's own `getProjectile` searches for anything matching the weapon's
/// `ammoPredicate`; this crate models the plain arrow only, which is the ammunition
/// a vanilla bow finds first anyway.
const BOW_AMMUNITION: &str = "arrow";

/// One consume (eat or drink) in progress on a connection.
///
/// Vanilla's own `useItem`/`useItemRemaining` pair, reduced to the two
/// facts the completion needs: which slot is being eaten from, and when it
/// finishes. `item` is carried so a slot whose contents changed mid-bite (a
/// container click, a hotbar swap) cannot complete as if it were still the food
/// — the same "re-check what you recorded" guard `PendingBreak` applies to a dig.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ItemInUse {
    /// Native inventory index the food is in.
    native: usize,
    /// The item that started the use, full registry name.
    item: String,
    /// The `MobSim` tick the use completes on — `started` plus vanilla's own consumable consume-ticks value.
    finish_tick: u64,
    /// The `remaining` value the last periodic consume sound was published for.
    ///
    /// Vanilla's own consumable emit-particles-and-sounds predicate is on
    /// `remaining % 4 == 0`, which is correct **only if it is evaluated exactly once
    /// per tick**. The loop that drives it reads `MobSim`'s counter from a 50 ms
    /// timer arm, and the two clocks are not the same object: if the timer fires
    /// twice inside one mob tick, the same `remaining` passes the predicate again and
    /// the eating sound doubles. Latching the value it last fired for makes the
    /// emission idempotent per tick without assuming the clocks agree.
    last_effect_remaining: Option<u32>,
}

/// What a `USE_ITEM` started. Vanilla's own item-use routine returns an
/// `InteractionResult`; this is the subset with a consequence here.
#[derive(Debug)]
enum UseItemOutcome {
    /// Nothing this crate models. Vanilla's `PASS`/`FAIL`.
    Nothing,
    /// A bow draw opened; the `RELEASE_USE_ITEM` that follows ends it.
    Draw(BowDraw),
    /// A consume opened; the server's own clock ends it.
    Consuming(ItemInUse),
    /// An equip swap already happened — arm 2 of vanilla's own item-use routine is instantaneous,
    /// unlike arms 1, 3 and 4.
    Equipped(crate::item_use::EquipSwap),
}

/// Applies a `USE_ITEM`: vanilla's own item-use routine's ordered arms, plus the projectile items
/// whose own `use` override replaces it.
///
/// The order below is vanilla's own item-use routine's own, and it is load-bearing — see
/// `crate::item_use`'s module doc. The launch arm sits first because those items
/// (`BowItem`, `SnowballItem`, `ThrowablePotionItem`) *override* vanilla's own item-use routine
/// entirely rather than being an arm of it, so they are a disjoint set and cannot
/// race the arms below.
///
/// `food_level` and `invulnerable` are the acting player's, for
/// vanilla's own can-eat check's two non-item disjuncts.
#[allow(clippy::too_many_arguments)]
fn apply_use_item(
    mobs: &MobHandle,
    inventory: &mut PlayerInventory,
    player_pos: Option<(f64, f64, f64)>,
    game_mode: GameMode,
    food_level: i32,
    invulnerable: bool,
    hand: u8,
    yaw: f32,
    pitch: f32,
    // Vanilla's own fishing-rod-item use routine's cast/retrieve dispatch needs the caster's own
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
    let thrown_potion = stack.components.potion;

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
                let velocity = lodestone_entity::projectile::launch_velocity(
                    f64::from(yaw),
                    f64::from(pitch),
                    pitch_offset,
                    power,
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
            // — no enchantment model reaches this call site yet (see
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

    // Arm 1: `DataComponents.CONSUMABLE` → `Consumable.startConsuming`, whose
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

    // Arm 2: `DataComponents.EQUIPPABLE` gated on `swappable()`. Instantaneous,
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
/// which is `player.getFoodData().eat(this)` plus `stack.consume(1, entity)`.
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
/// and consumes the bottle — issue #241's remaining producer gap
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
/// **Disclosed narrowing**: vanilla rolls a uniform `0..=4` amplifier onto
/// the bottle's own `minecraft:ominous_bottle_amplifier` component when a
/// pillager patrol captain drops it
/// ([`crate::mobs::MobSim::drop_ominous_bottle`]'s own doc has the loot-table
/// citation); every bottle drunk here grants amplifier `0` instead, because
/// persisting the real per-stack roll needs a new
/// `lodestone_model::ItemComponents` field this session's ownership does not
/// reach (`crates/lodestone-model/**`) — see `docs/raids-and-patrols.md` §5
/// for the exact hunk. Amplifier `0` is a real, working value rather than a
/// no-op: `absorb_raid_omen(0, 0) == 1` still starts a genuine raid once Bad
/// Omen converts, so this is "always the weakest roll", not "does nothing".
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

/// Vanilla's own potion-contents on-consume → `applyToLivingEntity(user, durationScale)` chain:
/// finishing a drink of `minecraft:potion` applies its full, **unscaled**
/// built-in effect list — issue #690's central gap, the one that made every
/// potion in the game do nothing at all.
///
/// Reuses [`crate::mob_effects::potion_splash_effects`] at `scale = 1.0`,
/// `duration_scale = 1.0` rather than re-deriving the list: that function's own
/// `splash_instant_amount`/`splash_timed_duration` are both the identity
/// transform at `scale = 1.0` (`floor(1.0 * x + 0.5) == x` for the non-negative
/// integer `x` every potion table entry is), which is exactly
/// `applyInstantaneousEffect`'s own unscaled call and `entity.addEffect`'s own
/// unscaled duration — not an approximation reused for convenience, the same
/// computation vanilla's *other* call site performs. `duration_scale` is `1.0`
/// for the same disclosed `minecraft:potion_duration_scale` gap
/// `resolve_potion_splash` already carries (this build's `ItemComponents` does
/// not model that component).
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
    let velocity = lodestone_entity::projectile::launch_velocity(
        f64::from(rotation.yaw),
        f64::from(rotation.pitch),
        0.0,
        power * BOW_ARROW_SPEED,
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
/// `potion` is the thrown stack's `minecraft:potion` registry id, and is what
/// [`MobSim::resolve_potion_splash`] later reads to decide which effects the
/// impact applies. It is `None` for every projectile that is not a splash or
/// lingering potion, and also for a potion stack carrying no potion component —
/// a water bottle, which correctly applies nothing.
fn spawn_player_projectile(
    mobs: &MobHandle,
    projectile: &str,
    origin: Vec3,
    velocity: Vec3,
    potion: Option<i32>,
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
///
/// Routes through [`MobSim::attack_from_player`] rather than the bare
/// [`MobSim::attack`] it wraps — that method's own doc names this call site as
/// the broker it was built for: with no identified attacker reaching the mob
/// sim, a player-landed hit on a villager wrote no `VillagerHurt`/
/// `VillagerKilled` gossip, so `villager::reputation`'s scoring machinery
/// (tested, correct) never saw a real production event. `LOCAL_PLAYER_ENTITY_ID`
/// for [`PlayerIdentity::entity_id`], matching every other self-facing
/// identity built in this file.
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
    // The weapon feed. This used to pass `PLAYER_BARE_HAND_ATTACK_DAMAGE`
    // unconditionally, so a diamond sword and a fist did the same 1.0 — the
    // armour formula on the receiving end was live-verified against a real
    // vanilla server while the number going into it could never be right.
    // `combat_stats` resolves the held item through the real
    // `ATTACK_DAMAGE` attribute fold, and an empty hand still lands on
    // `PLAYER_BARE_HAND_ATTACK_DAMAGE` because that *is* the player's attribute
    // base with no modifiers.
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

/// Maps a wire hand ordinal (`0` main, `1` off) to
/// `ClientboundAnimatePacket`'s own action byte (`SWING_MAIN_HAND = 0`,
/// `SWING_OFF_HAND = 3`) — see vanilla's own `ClientboundAnimatePacket` constants,
/// which `ServerProtocol::encode_animate`'s own doc comment already names.
/// Anything outside `0..=1` degrades to the main-hand swing, the same
/// "malformed input degrades the effect" convention `ServerBound::Swing`'s
/// own decode arm already applies at the wire boundary.
fn swing_action(hand: u8) -> u8 {
    if hand == 1 { 3 } else { 0 }
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
/// The three world-derived facts [`FallSample`] needs, read off the terrain the
/// player is standing in (issue #534).
///
/// # Which cell each one reads, and why
///
/// * `in_water` — the cell at the player's **feet**. Vanilla's `isInWater()` is a
///   fluid-height test over the whole bounding box; the feet cell is the earliest
///   part of that box to touch a water surface on the way down, which is the
///   moment the cancellation must fire. Reading the *eye* instead (which
///   `crate::vitals` correctly does for drowning, a different question) would
///   delay the cancellation by the player's height and let a shallow-water landing
///   still hurt.
/// * `fall_resetting` — the same feet cell, since a climbable is something the
///   player is *inside*.
/// * `block_damage_modifier` — the cell **below** the feet, at `y - 0.2`, which is
///   vanilla's own `getOnPosLegacy()` offset (its own `getOnPos`'s `0.2` epsilon).
///   A plain `y - 1` is wrong for a player standing exactly on a block boundary.
///
/// One `ChunkSource::block_state` call per cell, two cells — and `block_state` is
/// the cheap single-cell read `ChunkStore` overrides, not a column regeneration
/// (issue #440). This runs once per movement packet, the same cadence
/// `view.recenter` already runs at.
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
/// Before this function the five damage sites each sent `encode_set_health` and
/// nothing else, and `set_health(0.0)` does not raise a death screen — not in
/// vanilla's client (its own client-side set-health handler only calls
/// `hurtTo`/`setFoodLevel`/`setSaturation`) and not in this workspace's, whose
/// `NetUpdate::Death` is decoded from `player_combat_kill` alone. The visible
/// result was a player pinned at zero hearts with no screen and no respawn
/// button, which reads as the server having hung.
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
    // the constant its own `GameLogin.entity_id` (`begin_play_at`) claimed —
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
/// that carried **no** y coordinate, reusing the last position this connection
/// reported (issue #262).
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
    // Issue #534: the terrain the player is standing in, for the water /
    // climbable / landing-block facts. `.get()` because this is two single-cell
    // reads, not a batch — see `SourceRef::get`.
    source: &S,
    player_pos: &Option<(f64, f64, f64)>,
    fall: &mut FallTracker,
    vitals: &mut PlayerVitals,
    username: &str,
    on_ground: bool,
    // `Abilities.invulnerable` — creative and spectator. `fall` is not in
    // `#minecraft:bypasses_invulnerability` (only `out_of_world` and
    // `generic_kill` are), so an invulnerable player takes none of it. The
    // *tracker* still samples, so the fall is still tracked; only the hit is
    // skipped, which is vanilla's own is-invulnerable-to check's own placement.
    invulnerable: bool,
    // Issue #338's `minecraft:deaths` counter, threaded only to reach
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

/// Vanilla's own `PermissionLevel.GAMEMASTERS` — the level `Permissions
/// .COMMANDS_GAMEMASTER` requires, and the gate `handleChangeDifficulty`/
/// `handleLockDifficulty`/`handleSetGameRule`/`handleSetCommandBlock`/
/// `handleChangeGameMode` all share. The built-in `/gamemode`/`/gamerule`/
/// `/difficulty` commands already carry the identical `2` as their own
/// per-command constant (`crate::commands::gamemode::GAMEMODE_LEVEL` and
/// siblings); this one is for the handful of dedicated serverbound packets
/// vanilla uses for the same administrative actions instead of a `/`-command.
const COMMANDS_GAMEMASTER_LEVEL: u8 = 2;

#[allow(clippy::too_many_arguments)]
async fn dispatch_play_packet<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: SourceRef<'_, S>,
    view_radius: i32,
    state: &mut State,
    view: &mut ViewTracker,
    // Issue #619. This connection's chunk-residency guard, so a chunk-boundary
    // crossing or a live view-radius change (the `recenter`/`set_view_radius`
    // arms below) can move the same `PLAYER_LOADING`/`PLAYER_SIMULATION`
    // tickets `serve_play` granted at join, rather than leaving them pinned to
    // the join column for the connection's whole lifetime.
    player_ticket_guard: &PlayerTicketGuard,
    pending_keep_alive: &mut Option<i64>,
    pending_break: &mut Option<PendingBreak>,
    player_pos: &mut Option<(f64, f64, f64)>,
    // Issue #262. Mirrors `player_pos` exactly — updated here, read back by
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
    // `Option` because the two callers genuinely differ rather than for
    // convenience: only the native `serve_play` has a `select!` branch draining
    // this stream. The `wasm32` loop drains its join inline and then never looks at
    // it again, so lending it one would enqueue columns nothing sends — an island,
    // with a green test suite and a hole in the world. It passes `None` and takes
    // the inline path, which is that target's existing documented shape.
    mut join_stream: Option<&mut crate::join_scheduler::JoinChunkStream<S>>,
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
    // Issue #469. Mirrors `player_pos`/`player_rot` exactly — filled here,
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
    // Issue #465. Threaded through only to reach `apply_use_item_on`, which
    // needs to ask the world tick loop for a neighbour-update fan-out that
    // outlives this packet — see that function's own parameter comment.
    block_ticks: &BlockTickFeed,
    // Issue #249. This connection's composter roll source — seeded once in
    // `serve_play`, advanced once per right-click (see
    // [`apply_composter_use`]'s `roll` parameter).
    composter_rng: &mut SpawnRng,
    // This connection's bone-meal roll source — seeded once in `serve_play`,
    // advanced by a bone-meal right-click on a growable block. Its own stream, so
    // fertilising a crop cannot shift which roll a later composter insert or
    // block drop sees.
    bone_meal_rng: &mut SpawnRng,
    // Issue #256. This connection's experience — level, bar and lifetime total.
    // `&mut` because closing a furnace pays out its banked smelting XP (the
    // `ContainerClosed` arm), which is currently the only production producer.
    experience: &mut crate::experience::PlayerExperience,
    // Issue #259. This connection's live status effects — written by `/effect` and
    // ticked from `serve_play`'s vitals timer.
    effects: &mut crate::mob_effects::ActiveEffects,
    // Issue #337. This connection's block-drop roll source — seeded once in
    // `serve_play`, advanced by every break that rolls a table (see
    // `apply_block_action`'s parameter comment). A second stream rather than
    // sharing the composter's, so a composter click cannot shift which drop a
    // later break rolls; the two features would otherwise be coupled through
    // nothing but draw order.
    drops_rng: &mut SpawnRng,
    // Issue #335. This connection's declared channel support (register/
    // unregister interpretation happens here, in Play) and the shared registry
    // to dispatch ordinary payloads on.
    client_channels: &mut ClientChannels,
    plugin_channels: &PluginChannelRegistry,
    // This connection's current game mode, `&mut` because the
    // `ChangeGameMode` arm and the built-in `/gamemode` both rewrite it — and
    // because the creative consequences below (instant break, damage immunity)
    // read it on later packets.
    game_mode: &mut GameMode,
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
    // Issue #580. `ChatCommand`'s `CommandWorld` needs this to reach
    // `/worldborder`'s read/write surface — the same `BorderFeed` `serve_play`
    // already carries for the join broadcast and the vitals-tick damage read.
    border: &BorderFeed,
    player_entity_id: i32,
    // This connection's login name, for the death message
    // (`DeathCause::death_message`'s victim argument — vanilla's
    // `victim.getDisplayName()`).
    username: &str,
    // The world spawn resolved at join, for the respawn teleport. See
    // `apply_client_command`'s own parameter comment.
    world_spawn: Vec3,
    // Issue #531. The server tick this packet is handled on, for
    // `apply_block_action`'s destroy-progress accounting. `Some(ticks_since(
    // play_start))` on native; `None` on `wasm32`, whose `serve_play` has no
    // `tokio::time` to count ticks with — a documented gap of the same shape as
    // that loop's other timer-fed ones, and the only cost is that the break
    // *timing* test is skipped there (hardness and range still apply).
    game_tick: Option<u64>,
    // Issue #260. This connection's in-progress bow draw, if any: the server tick
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
    // ends on the **server's own clock** — vanilla's own
    // update-using-item routine counts `useItemRemaining` down and calls
    // `completeUsingItem` itself, and the client sends nothing at all when a
    // steak finishes. `serve_play`'s per-tick arm is what finishes it here.
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
            // The identity is what makes ownership expressible: a mob's owner is an
            // account uuid (vanilla's own tamable-animal owner-uuid data field), and
            // `MobSim` resolves a tamed pet's owner *position* by looking that uuid
            // up in this list every tick. `set_players` is generic over
            // `Into<PerceivedPlayer>`, so supplying the bare perception compiles fine
            // and silently makes every pet ownerless — the failure is invisible from
            // the call site, which is why this spells the identity out.
            // Issue #458 primitive 2: the gaze feed needs a real view vector,
            // not just a position. `player_rot` holds the latest known angles
            // (just possibly refreshed above, or `Default` — yaw/pitch 0 — for
            // a `MovePlayerPos` packet that carries no rotation at all before
            // the first look). Same formula `block_placement.rs`'s
            // `nearest_look` already uses for the block-placement raycast
            // (vanilla's own entity view-vector calculator).
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
            // `-1.0_f64 / 16.0` must floor to chunk `-1`, matching vanilla's
            // vanilla's own section-position block-to-section-coord conversion (an arithmetic right shift).
            let cx = (x / 16.0).floor() as i32;
            let cz = (z / 16.0).floor() as i32;
            // **This is what makes the world tick follow the player.**
            // `crate::tick::run_tick_loop` used to simulate a 49-column square
            // nailed to chunk (0, 0) — natural spawning and every randomly-ticking
            // block stopped once the player walked out of it. See
            // `crate::tick_area` for the design.
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
            // Issue #619: read before the call, since `recenter` writes
            // `self.center` in place — comparing after would always see the
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

            if let Some(raw) =
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
                Abilities::for_mode(*game_mode).invulnerable,
                advancements,
                player_uuid,
            )
            .await?;
        }
        // Issue #262. Carries nothing but the flags byte, so its whole job is
        // the `on_ground` edge — which is exactly the landing sample
        // `FallTracker`'s doc comment used to disclose as unobservable,
        // because a fall that ends with no net position change in its final
        // tick reports the touchdown on *this* packet and no other.
        ServerBound::PlayerStatusOnly { on_ground } => {
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
                // Issue #531. The breaker's feet for the interaction-range test
                // — the same `player_pos` ticket `apply_use_item_on` already
                // reads for the bed reach check, and `None` for the same reason
                // (no `PlayerMoved` packet has arrived yet).
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
            // Issue #249: one roll per right-click, whatever block was hit —
            // vanilla's level RNG advances on plenty of unrelated draws too,
            // and the composter branch is the only consumer of this stream.
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
                // Issue #329. The player's position, for the bed reach test —
                // `None` until a `PlayerMoved` packet carries one.
                player_pos.as_ref().map(|&(x, y, z)| Vec3::new(x, y, z)),
                respawn,
                // Issue #475. The placing player's yaw and pitch, so
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
            // Vanilla's own change-difficulty handler's own gate:
            // `Permissions.COMMANDS_GAMEMASTER` (level 2) or the singleplayer
            // owner. `commands.permission_level` already *is* that check —
            // `AccessLists::command_permission_level` resolves to the real op
            // level, or to `MAX_PERMISSION_LEVEL` for a world with no operator
            // model configured at all (the singleplayer-owner shape), which is
            // vanilla's `isSingleplayerOwner()` arm restated at the one place
            // this crate already computes it, not a second check. This was a
            // disclosed, deliberate omission before `crate::access` existed
            // (issue #268); it is real now the model does.
            //
            // A **locked** world separately refuses the change, which vanilla
            // enforces in `MinecraftServer.setDifficulty`. The confirmation
            // below is sent either way and carries the value that is actually
            // stored, so a refused request (either reason) corrects the
            // client's own UI rather than leaving it wrong — vanilla itself
            // sends nothing on a permission refusal, but this crate already
            // made that choice for the lock case and there is no reason for
            // the two refusal reasons to behave differently here.
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
            // Vanilla's own set-carried-item handler calls
            // `player.stopUsingItem()` before it moves the selection, so switching
            // off a half-eaten steak cancels the bite rather than letting it
            // complete against whatever is in the slot now. `finish_consuming` also
            // re-checks the item as a second layer, because a *container* click can
            // change the same slot without this packet.
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
            // Issues #253-#255: the anvil charges XP levels and the grindstone
            // refunds them, both **only** on a click that takes the result —
            // read before `apply_container_clicked` runs, because that call is
            // what performs the take (`crate::container_click::take_result`)
            // and overwrites `inventory.workstation()` with the post-take cells.
            // `crate::container_click`/`apply_container_clicked` are
            // deliberately economy-free (see their own module docs), so this is
            // the one place that connects a workstation take to a real
            // `PlayerExperience` — the same split `apply_use_item_on`'s XP-free
            // block-breaking already has from `destroy_block`'s own charge.
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
                // Issue #617: `apply_container_clicked` may have refused the
                // take outright (`container_click`'s `MayPickup` gate, wired
                // for the anvil in `apply_workstation_clicked`), so charging
                // XP off the *pre-click* cells alone — as if a take always
                // succeeds — would zero out a player's levels for a click
                // that left the item sitting in the result slot. A real take
                // always clears the anvil's input cell 0
                // (`container_click::take_result`'s `Station::Anvil` arm, the
                // only place that clears it), so that transition is the
                // signal an actual take happened.
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
                        // The grindstone's own result slot never overrides
                        // `mayPickup` (`GrindstoneMenu`'s anonymous result
                        // `Slot` only overrides `mayPlace`/`onTake`), so a
                        // take here can never be refused — `took_result`
                        // therefore only guards against a click that clicked
                        // the result index without one being present at all.
                        if took_result && crate::anvil::grindstone_result(get(0), get(1)).is_some() {
                            let awarded = crate::anvil::grindstone_xp(get(0), get(1), drops_rng);
                            if awarded > 0 {
                                experience.give_points(i32::try_from(awarded).unwrap_or(i32::MAX));
                                experience_changed = true;
                            }
                        }
                    }
                    // Neither `LoomMenu` nor `StonecutterMenu`'s result slot
                    // charges or refunds XP anywhere in vanilla — the whole
                    // cost of either is the input items `take_result`
                    // already consumes.
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
        ServerBound::ContainerClosed { window_id } => {
            // Vanilla's own do-close-container → container-menu removed chain:
            // the cursor and any crafting grid go back to the player,
            // and what does not fit hits the floor. Dropping them silently would
            // delete items every time a player closed a menu mid-drag.
            //
            // Issues #253-#255: an open anvil/grindstone/smithing/enchanting-table's
            // input cells (`PlayerInventory::workstation`) are exactly the same
            // "menu-owned scratch container, cleared on `removed`" shape as the
            // crafting table's grid (`AnvilMenu`/`GrindstoneMenu`/`SmithingMenu`/
            // `EnchantmentMenu` all clear their own input container in `removed`),
            // so they get the same treatment here.
            let mut returning = inventory.take_table_crafting();
            returning.extend(inventory.take_workstation());
            if let Some(carried) = inventory.click_state_mut().carried.take() {
                returning.push(carried);
            }
            inventory.click_state_mut().reset();
            // Issue #692: the same "menu-owned scratch state, cleared on
            // `removed`" shape as the crafting grid and workstation cells
            // above — vanilla's own container-menu selected-bundle-item-index is a
            // field on the menu instance itself, so it does not survive past
            // the menu that set it.
            inventory.clear_selected_bundle_items();
            let mut spilled = Vec::new();
            for stack in returning {
                if let (_, Some(leftover)) = inventory.add(stack) {
                    spilled.push(leftover);
                }
            }
            // Vanilla's own beacon-menu removed routine: the payment slot is dropped straight to
            // the floor — `player.drop(itemStack, false)` — **not** merged
            // into the inventory the way the crafting-grid/workstation
            // return above is, and unlike those two, a beacon's own slot is
            // not this crate's scratch-container state at all (it lives on
            // the block entity, `BeaconData::payment`), so it needs its own
            // take here rather than falling out of `take_workstation`.
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
                // Furnace XP, paid out **on close** rather than per cook — vanilla's own
                // furnace-block-entity award-used-recipes-and-pop-experience routine,
                // which the player's `stopUsing` reaches. `Furnace::take_recipes_used`
                // has banked the smelts since the last drain and had no caller at
                // all; this is it.
                //
                // Vanilla pops orbs at the furnace and the player absorbs them.
                // There is no orb entity here (see `crate::experience`'s module doc
                // for what one needs), so the points go straight to the player's
                // bar. That is the difference between "no XP exists" and "XP exists
                // without a flying orb", and the second is the honest subset.
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
            // Any window close ends whatever menu was open, merchant or not
            // (`AbstractContainerMenu::removed` runs for every menu type);
            // unconditional because, unlike `open_container`, `OpenMerchant`
            // carries no `window_id` for this arm to compare against.
            *open_merchant = None;
        }
        // Issue #616's remainder: a merchant trade-row selection
        // (`ServerboundSelectTradePacket`). See `attempt_villager_trade`'s own
        // doc for why this executes the trade in one step rather than through
        // a payment-slot placement flow.
        ServerBound::SelectTrade { index } => {
            if let Some(OpenMerchant { entity_id }) = *open_merchant
                && let Some(index) = usize::try_from(index).ok()
            {
                // Read back from the villager's *persistent*
                // [`crate::villager_trade::VillagerTrades`] (issue #245's
                // third piece — no longer recomputed fresh every click), so
                // the charged price already reflects any earlier purchase's
                // demand and this offer's real out-of-stock state, and is
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
                    // Vanilla's own villager notify-trade routine's on-reputation-event-from call: a
                    // completed trade is the one real production producer
                    // of `Trading` gossip, matching
                    // `MobSim::attack_from_player`'s villager-hit case.
                    // `record_reputation_event` (issue #246) already exists
                    // and was, until this call, only reachable from its own
                    // tests.
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
        // Issues #253-#255's last mile: vanilla's own anvil-menu item-name setter. See
        // `apply_rename_item`'s own doc for the gate and what gets resent.
        ServerBound::RenameItem { name } => {
            let creative = *game_mode == GameMode::Creative;
            for directive in apply_rename_item(proto, inventory, open_container.as_mut(), &name, creative, world.crafting_hooks()) {
                apply(conn, state, directive).await?;
            }
        }
        // Issue #48's remainder, wire-decode half. Vanilla's own
        // set-command-block handler: swap the block to the requested mode
        // (preserving `FACING`), write `conditional`, then update the entity's
        // command/track-output/"Always Active" fields. See
        // `crate::command_block`'s own module doc for what still needs a real
        // redstone signal instead of this packet, and for why "Always Active"
        // scheduling here (via `on_automatic_changed`) is faithful to vanilla's
        // own command-block-entity set-automatic routine rather than an addition.
        ServerBound::SetCommandBlock { pos, command, mode, track_output, conditional, automatic } => {
            // Vanilla's own can-use-game-master-blocks check: creative mode (`abilities
            // .instabuild`, which `Abilities::for_mode` sets only for
            // `GameMode::Creative`) **and** `Permissions.COMMANDS_GAMEMASTER`
            // (level 2) — both, not either. This was a disclosed omission
            // before `crate::access` existed; real now the model does, and
            // `commands.permission_level` is the same already-resolved check
            // `DifficultyChanged`'s own comment above explains.
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
                // `markConditionMet`'s predecessor read — the block directly
                // behind this one's own facing. Read before the registry lock
                // below so this never nests a second `.with` inside the first.
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
        // Issue #616's remainder. Vanilla's own sign-update handler:
        // strip legacy formatting codes from every line, then
        // `updateSignText`'s own gate (not waxed, `editor` matches) decides
        // whether the write actually lands. This crate grants `editor` only
        // at placement (see `crate::block_entities::SignData`'s own doc), so
        // in practice this succeeds exactly once per sign, right after it is
        // placed.
        ServerBound::SignUpdate { pos, is_front_text, lines } => {
            let stripped = lines.map(|line| crate::block_entities::strip_sign_formatting(&line));
            block_entities.with(|registry| {
                if let Some(entity) = registry.get_mut(pos) {
                    crate::block_entities::apply_sign_update(entity, player_uuid, is_front_text, stripped);
                }
            });
        }
        // Issue #616's remainder, `EDIT_BOOK`. See `apply_edit_book`'s own
        // doc for the gate; resent via `CONTAINER_SET_SLOT` on window `0`
        // (the player's own inventory screen, not whatever menu happens to
        // be open — vanilla's own edit-book handler operates on `this.player.getInventory()`
        // directly and never checks `containerMenu`), the same "window 0,
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
        // Issue #692: a bundle-tooltip highlight claim. Stored, not acted on
        // immediately — `container_click::pickup`'s next right-click-on-empty
        // against this slot is what actually reads it
        // (`selected_bundle_item`), mirroring vanilla's own two-step
        // select-then-extract (`ServerboundSelectBundleItemPacket` just
        // updates vanilla's own container-menu selected-bundle-item-index; the
        // extraction happens on the following `PICKUP` click).
        ServerBound::SelectBundleItem { slot_id, selected_item_index } => {
            inventory.set_selected_bundle_item(slot_id, selected_item_index);
        }
        // Issue #616's remainder, `SET_BEACON`. See `apply_set_beacon`'s own
        // doc for the gate.
        ServerBound::SetBeacon { primary, secondary } => {
            let directives =
                apply_set_beacon(proto, block_entities, open_container.as_mut(), primary, secondary);
            for directive in directives {
                apply(conn, state, directive).await?;
            }
        }
        // The enchanting table's "choose an offer" button
        // (vanilla's own enchantment-menu click-menu-button routine) — issue #253's other last-mile
        // gap. See `apply_container_button_click`'s own doc for the pricing
        // and refusal rules.
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
        // A crafter's per-slot enable/disable toggle
        // (vanilla's own container-slot-state-changed handler).
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
            // Vanilla's own attack routine's `causeFoodExhaustion(0.1F)`, charged on the swing
            // rather than on a hit that landed — vanilla charges it inside `attack`
            // after the damage call, unconditionally for a living target.
            if !Abilities::for_mode(*game_mode).invulnerable {
                vitals.add_exhaustion(crate::food::EXHAUSTION_ATTACK);
            }
        }
        // The right-click half of the old combined interact packet, and the
        // **production producer `MobSim::interact` did not have**: every taming,
        // feeding, sitting and breeding mechanism was driven only from that type's
        // own gates, so a real client's right-click on a wolf decoded to
        // `ServerBound::Ignored` and nothing in the game could be tamed.
        ServerBound::InteractEntity {
            entity_id,
            hand,
            using_secondary_action,
        } => {
            // Off-hand interactions are dropped rather than duplicated: a vanilla
            // client sends the main hand first, and running both would roll a tame
            // chance twice for one right-click — which is invisible in a gate that
            // drives `interact` directly and only shows up as "taming is suspiciously
            // easy" in the running game.
            if hand == 0 {
                // **Boarding a boat, and it has to be ahead of `MobSim::interact`.**
                // A boat is not a mob: `interact`'s whole chain is
                // vanilla's own tamable-animal/abstract-horse/animal mob-interact routines and has no arm
                // for one, so a right-click on a boat reached the taming code, fell
                // through to `Pass`, and did nothing at all — `SET_PASSENGERS` had no
                // producer anywhere in the tree.
                //
                // `using_secondary_action` is `player.isSecondaryUseActive()`, which
                // vanilla's own abstract-boat interact routine really does consult: sneak-clicking a boat
                // must *not* board it. This is the first reader of that field, whose
                // own doc comment said "nothing reads it yet".
                if mobs.with(|sim| sim.vehicle_type(entity_id).is_some()) {
                    let boarded = mobs.with(|sim| {
                        sim.mount_vehicle(entity_id, player_entity_id, using_secondary_action)
                    });
                    if boarded {
                        // The vehicle's **whole** passenger list, which is how
                        // vanilla always sends it — vanilla's own start-riding routine re-broadcasts
                        // the list rather than a delta. Without this packet the client
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
                // **Minecarts, for the identical reason boats are checked first**:
                // a minecart is not a `Mob` either, so `MobSim::interact`'s taming
                // chain has no arm for one and a right-click would otherwise fall
                // through to `Pass` and do nothing.
                if let Some(kind) = mobs.with(|sim| sim.minecart_kind(entity_id)) {
                    if kind.is_furnace() {
                        // Vanilla's own minecart-furnace interact routine — `addFuel` on coal/charcoal,
                        // `itemStack.consume(1, player)` only on success.
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
                        // Vanilla's own minecart interact routine — `player.startRiding(this)`, the
                        // same `SET_PASSENGERS` handoff the boat arm above uses.
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
                    // Chest/hopper/TNT minecarts (and a sneak-click / already-seated
                    // plain one) reach here with no modelled interaction — see
                    // `crate::mobs::minecart`'s own module doc for why a container
                    // minecart's slots have no menu wired to them yet.
                    return Ok(());
                }
                let held = inventory.selected_item().map(|stack| stack.item.clone());
                // Issue #236: vanilla's own mob interact routine runs
                // `checkAndHandleImportantInteractions` then `super.interact`
                // (vanilla's own entity interact routine's two leash branches — detach-if-mine,
                // attach-if-holding-a-lead) **before** `mobInteract`'s taming
                // chain, and a consuming leash branch short-circuits the rest
                // (`Mob.interact`: `if (superReaction != PASS) return
                // superReaction;`). `MobSim::try_leash` is checked first here
                // for the same reason — a lead in hand must attach rather
                // than roll a taming/feed/breed interaction against it.
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
                        // vanilla `itemStack.shrink(1)`, through the same
                        // `consume_one`/window-0 sync every other consuming
                        // interaction below uses.
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
                    // `MobSim::try_leash` already spawned the dropped lead
                    // item itself when appropriate (mirroring `tick_leashes`'
                    // own snap branch) — nothing left for this arm to do.
                    crate::mobs::LeashOutcome::Detached { .. } => None,
                    // Not leashable, no lead in hand, out of range, or already
                    // someone else's: vanilla's own entity interact routine returns
                    // `PASS` here and falls through to `mobInteract`, so this
                    // does too — the taming chain below, unchanged.
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
                // Issue #245: a villager's trade screen, from
                // `MobSim::interact`'s own villager short-circuit — checked
                // ahead of the generic item-consumption handling below
                // because opening the screen is itself the whole visible
                // effect of this outcome (there is no slot sync to send).
                if let Some(crate::mobs::InteractOutcome::OpenTrade { level, .. }) = outcome {
                    let xp = mobs.with(|sim| sim.villager_xp(entity_id));
                    let reputation = mobs.with(|sim| sim.villager_reputation(entity_id, player_uuid));
                    let hero_of_the_village_amplifier =
                        effects.amplifier_of("minecraft:hero_of_the_village");
                    // The villager's *persistent* offer list (issue #245's
                    // third piece), not a fresh stateless one —
                    // `MobSim::villager_offers` reads the mob's own live
                    // profession/level itself, so `OpenTrade`'s `profession`
                    // is no longer needed here.
                    let offers =
                        mobs.with(|sim| sim.villager_offers(entity_id, reputation, hero_of_the_village_amplifier));
                    open_merchant_screen(conn, proto, state, &offers, level, xp, next_window_id).await?;
                    // `SelectTrade`'s only lookup: which villager this
                    // connection is currently trading with. Set unconditionally
                    // on every open (not merged/kept) — a second interact while
                    // one screen is already open re-sends `open_screen` too,
                    // which vanilla's own client treats as replacing the menu.
                    *open_merchant = Some(OpenMerchant { entity_id });
                }
                // Vanilla's own abstract-horse do-player-ride routine: `interact_horse`'s empty-handed arm
                // on a tamed adult already recorded the mount in `MobSim` (its own
                // doc names this exact send as the caller's cue) — this is the
                // client's only way to learn it is aboard, the same
                // whole-passenger-list handoff the boat/minecart arms above use.
                if outcome == Some(crate::mobs::InteractOutcome::Mounted) {
                    apply(
                        conn,
                        state,
                        proto.encode_set_passengers(entity_id, &[LOCAL_PLAYER_ENTITY_ID]),
                    )
                    .await?;
                }
                // Vanilla consumes through `usePlayerItem`, a no-op in creative
                // (vanilla's own has-infinite-materials check). A sit toggle is
                // `InteractionResult.SUCCESS.withoutItem()` and consumes nothing,
                // which `InteractOutcome::consumes_item` already encodes.
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
        // Issue #260: the player's own launch path. Before this, every projectile
        // in the game came from a mob goal — `ClientAction`-side bow support
        // existed in the protocol crates with no server model behind it, so a
        // player could draw a bow and nothing was ever created.
        ServerBound::UseItem { hand, yaw, pitch } => {
            // **the boat item's own use routine first, because it is an override rather than an
            // arm.** It replaces vanilla's own item-use routine wholesale, exactly as
            // the bow/snowball items' own use routines do, so it belongs on the disjoint-set side of
            // the dispatch alongside `launch_intent` and ahead of the eat/equip
            // chain. A boat is neither food nor equippable, so the order is
            // unobservable today — it is written this way so it stays right if one
            // ever becomes both.
            //
            // Handled here rather than inside `apply_use_item` because the raytrace
            // needs the **world**, which that function is deliberately without: it
            // takes an inventory, a position and a game mode and nothing else. The
            // eye height comes from the tracked feet position — `getEyePosition()`,
            // whose absence is the same "no data yet, don't guess" refusal the
            // launch arm makes.
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
                    // Vanilla `PASS`/`FAIL` — the raytrace missed, or the hull would
                    // not fit. Nothing is consumed and, crucially, nothing falls
                    // through: a boat reaching the eat/equip arms would find no food
                    // component and no equippable one anyway, but returning here says
                    // so rather than relying on it.
                    crate::boat::BoatApplied::Refused => return Ok(()),
                    crate::boat::BoatApplied::Placed { .. } => {
                        // `itemStack.consume(1, player)`, *after* `addFreshEntity`.
                        // Through `consume_one` so `!hasInfiniteMaterials()` applies
                        // and a creative player's boats are not used up — the trap a
                        // previous arm here hit by shrinking unconditionally.
                        if consume_one(inventory, boat_native, *game_mode)
                            && *game_mode != GameMode::Creative
                        {
                            // The remainder on window 0, the same channel every other
                            // consuming arm reports on. Without it the client's count
                            // desyncs and the next click sends a stale one, which
                            // looks like the item coming back.
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
                        // A placement ends any draw or bite in progress, as any other
                        // `USE_ITEM` does.
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
                *game_mode,
                vitals.food().food_level(),
                Abilities::for_mode(*game_mode).invulnerable,
                hand,
                yaw,
                pitch,
                player_entity_id,
            );
            // Both slots are overwritten rather than merged, whatever the outcome:
            // a fresh `USE_ITEM` restarts the charge, and a `USE_ITEM` for
            // something that is not chargeable ends any draw or bite in progress
            // (vanilla's `stopUsingItem` on a new use).
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
                    // `player.drop(swappedToInventory, false)` — the previously worn
                    // piece when the inventory was full.
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
            // A release *before* the consume clock ran out cancels it with no food
            // applied — vanilla's own release-using-item routine, which for a consumable is
            // `stopUsingItem` and nothing else. This is the arm a player hits
            // constantly and the one most easily forgotten.
            *item_in_use = None;
            if let Some(draw) = bow_draw.take() {
                let fired = apply_release_use_item(
                    mobs,
                    inventory,
                    *player_pos,
                    *player_rot,
                    *game_mode,
                    draw,
                );
                // Vanilla's own attack routine's exhaustion is charged on a melee swing; a bow
                // shot has no exhaustion cost in vanilla, so nothing is charged
                // here. Recorded because its absence otherwise reads as an
                // oversight next to the `Attack` arm two branches down.
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
        // The steering half. The client owns the boat it rides
        // (vanilla's own is-client-authoritative check), so this is not a request to be
        // validated — it is the authoritative report, and the server's job is to
        // write it down so the boat's snapshot moves and every other viewer's
        // `move_entity` diff follows.
        //
        // The rider check lives in `apply_vehicle_move`, which resolves the vehicle
        // from *this* player rather than from an id on the wire (the packet carries
        // none) — vanilla's own `getRootVehicle()` rule, and what stops a connection
        // dragging a boat it is not sitting in.
        ServerBound::VehicleMoved {
            position,
            yaw,
            pitch,
        } => {
            // Pitch is decoded and dropped: `AbstractBoat` never writes `xRot`, and
            // a mounted mob only ever reports half its rider's (unmodelled — see
            // `apply_mob_move`). Named rather than `_` so the field's existence is
            // visible at the one place that could use it.
            let _ = pitch;
            // A player can only be aboard one of the two maps at a time
            // (`mount_mob` and `mount_vehicle` are separate "one map's worry"
            // occupancy rules — see `mount_mob`'s own doc), so trying the mob map
            // only when the vehicle map refused is exact rather than a guess:
            // vanilla's own is-client-authoritative check does not distinguish a
            // boat from a horse, and this is the one wire packet both share.
            mobs.with(|sim| {
                if sim
                    .apply_vehicle_move(player_entity_id, position, yaw)
                    .is_none()
                {
                    sim.apply_mob_move(player_entity_id, position, yaw);
                }
            });
        }
        // Issue #262's `PADDLE_BOAT` remainder — purely cosmetic (see
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
            // Vanilla's own camel on-player-jump routine: `isSaddled() && dashCooldown <= 0 &&
            // onGround()`. This crate has no saddle-equip model at all (the
            // whole horse family already boards without one — see
            // `MobSim::mount_mob`'s own doc) and no `onGround` for a
            // client-authoritative mount (nothing here simulates a ridden
            // mob's physics; the client does, per
            // `lodestone_physics::vehicle`'s module doc) — both disclosed
            // narrowings, not silent ones. Reacting to every *received*
            // `jump: true` is the same "a received packet already is the
            // rising edge" reasoning the `shift` dismount two lines below
            // relies on, since `PlayerInput`'s producer only resends on
            // change.
            if jump {
                mobs.with(|sim| sim.trigger_camel_dash(player_entity_id));
            }
            // Vanilla's own player ride-tick routine: `!level.isClientSide && wantsToStopRiding() &&
            // isPassenger()` → `stopRiding()`, where `wantsToStopRiding()` is
            // exactly `isShiftKeyDown()` — tested every tick a passenger is
            // aboard, not on a press-edge. This dismounts on every *received*
            // `shift: true` rather than replaying a stored per-tick flag, which
            // reproduces the same observable behaviour without a stale-state
            // hazard: `lodestone_controller::ecs::send_player_input` only queues
            // this packet when the input actually changes (`LastPlayerInput`'s
            // own edge check — real vanilla resends on change too,
            // vanilla's own local-player tick routine's `!lastSentInput.equals(...)`), so a received
            // `shift: true` already *is* the rising edge here, not a level held
            // across many packets. And even a literal level check would be safe:
            // `mount_vehicle`'s own `using_secondary_action` gate (and the
            // horse-family arm of `MobSim::interact`) reads a *fresh*
            // `using_secondary_action` bit straight off the `INTERACT` packet,
            // not this stored flag, so a sneaking player simply cannot board in
            // the first place — there is no re-board race to lock them out of.
            //
            // A player rides at most one of the three maps at a time ("one map's
            // worry" — see `mount_vehicle`'s own doc), so trying vehicle, then
            // minecart, then mob in sequence finds whichever one (if any) is
            // occupied without risking a double-dismount.
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
                    // Vanilla's own stop-riding routine's own wire consequence — the vehicle's
                    // whole (now empty) passenger list, the same `SET_PASSENGERS`
                    // handoff every mount arm above uses to announce boarding.
                    apply(conn, state, proto.encode_set_passengers(vehicle_id, &[])).await?;
                    if let Some(position) = dismount_position {
                        // Vanilla's own remove-vehicle routine: the server applies the
                        // vehicle's dismount location and the client receives
                        // that authoritative position in the same transition.
                        // Updating the connection-local mirror first keeps the
                        // next movement delta and the player registry on the
                        // same side of the teleport.
                        *player_pos = Some((position.x, position.y, position.z));
                        *player_rot = Some(rotation);
                        apply(
                            conn,
                            state,
                            proto.encode_teleport(
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
            // `source` is this call's own `SourceRef` — the `Dimension` arm is
            // only ever reached via a portal trip (see that variant's own doc),
            // so this is exactly "away from the dimension we joined in".
            let away_from_home = matches!(source, SourceRef::Dimension(_));
            apply_client_command(
                conn,
                proto,
                state,
                vitals,
                fall,
                world_spawn,
                *respawn,
                source.get(),
                world,
                advancements,
                player_uuid,
                action,
                commands.permission_level,
                away_from_home,
                dimension_reset,
            )
            .await?;
        }
        ServerBound::ClientInformationChanged { view_distance } => {
            // **Issue #545: no clamp here.** This arm used to do
            // `clamp(0, view_radius.max(0))` against `view_radius` — *this
            // connection's own `serve_connection` argument*, i.e. the radius it
            // joined with. That made lowering render distance mid-session work
            // and raising it silently do nothing, which is the owner's report.
            // Vanilla clamps against `serverViewDistance`, a server setting
            // (its own server view-distance field), never against the player's current view.
            //
            // The ceiling now lives on the `ViewTracker` as its own field and
            // `set_view_radius` applies it — see `ViewTracker::max_radius` for
            // the per-path policy and why the two roles had to be separated.
            // Issue #619: same "read before the call" reasoning as the
            // `recenter` arm above.
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
        // Issues #48/#464: the wire path for commands, and the *whole* of it
        // on this side of the seam.
        //
        // # The built-in tree is consulted first, and that is the fix
        //
        // This arm used to do no parsing at all beyond a hand-rolled
        // `parse_gamemode_command` string split, and then fall through to the
        // host sink. Since every real constructor passes
        // `CommandDispatch::none()` (issue #535), **that meant `/gamerule` typed
        // by a player did nothing** — the built-in `ServerCommands` tree existed,
        // was tested, and had zero references outside its own module. Its own doc
        // comment claimed this arm consulted it; that claim was stale. This is
        // the call that makes it true, and `rcon.rs` is the other one.
        //
        // # Why the effects come back rather than being applied by the executor
        //
        // `game_mode` and `inventory` are *this function's* parameters, reached
        // through `&mut`. An executor is a shared `Arc` closure inside a
        // process-wide tree and cannot touch either, nor can it reach `proto` or
        // `conn`. So `run` returns typed `Effect`s: the ones aimed at this
        // connection are applied here, inline, exactly as the hand-rolled
        // `/gamemode` arm already did; the rest are queued on the shared
        // `PlayerRegistry` for their own connection's loop to drain.
        //
        // # Permission level
        //
        // `commands.permission_level` was resolved once at the Play handoff from
        // this connection's authenticated uuid. It gates the *tree* — a level-2
        // command is invisible to tab completion and answers `NoPermission` on
        // execution — rather than being checked per command here.
        //
        // With no sink installed, `CommandDispatch::run` refuses. That
        // direction is load-bearing and is not an implementation detail: an
        // absent dispatcher must never read as blanket permission, the same
        // property `dispatch_refuses_rather_than_ungates_when_permissions_are_missing`
        // holds one layer in.
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
                // `/worldborder`'s read/write surface (issue #580) — the same
                // shared `BorderFeed` this connection already holds.
                border: Some(border),
                // `dispatch_play_packet` does not carry an `AccessHandle` in
                // its own signature (adding one would widen this crate's
                // single largest parameter list for one command family) —
                // `/op`/`/deop`/`/whitelist` typed in chat report the same
                // "no access list configured" refusal an offline-world RCON
                // caller would see. RCON (`crate::rcon::run_command_as`) is
                // the one production `Some`; see `access_commands`'s module
                // doc for why that scoping is deliberate, not a gap.
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
                        // `apply_own_effect`, exactly as before.
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
                                )
                                .await?;
                            }
                        }
                    }
                    for line in outcome.response.lines() {
                        apply(conn, state, proto.encode_system_chat(line)).await?;
                    }
                }
                // No built-in root matched: the host's problem, exactly as
                // before.
                None => {
                    let response = commands.dispatch.run(&commands.caller, &command);
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
        // The F4 switcher. A *request*, not an instruction: the two directives
        // below echo the mode this server actually applied, so a client that
        // guessed wrong (including one that guessed a refusal wrong) is
        // corrected either way. `GameModeCommand.PERMISSION_CHECK`
        // (`Permissions.COMMANDS_GAMEMASTER`, level 2) is
        // `handleChangeGameMode`'s own gate — real now `crate::access` exists,
        // see `DifficultyChanged`'s own comment above; this crate's `/gamemode`
        // built-in already carries the identical `GAMEMODE_LEVEL` check on the
        // command path, so this brings the packet path to parity with it
        // rather than introducing a new posture.
        ServerBound::ChangeGameMode { mode } => {
            if commands.permission_level >= COMMANDS_GAMEMASTER_LEVEL {
                *game_mode = mode;
            }
            for directive in game_mode_directives(proto, *game_mode) {
                apply(conn, state, directive).await?;
            }
        }
        // A spectator clicking a player's name in the tab list. See
        // `ServerBound::TeleportToEntity`'s own doc comment for the scoping
        // this narrows vanilla's `handleTeleportToEntityPacket` to (connected
        // players only, current facing kept rather than matched to the
        // target's). Silently does nothing outside spectator mode or for an
        // unresolved uuid — the same "request, not an instruction" posture
        // `ChangeGameMode` above takes, and matches vanilla's own handler,
        // which has no failure reply either. Mirrors `Effect::Teleport`'s own
        // arm (`/tp`) for how position/rotation/fall state are updated —
        // same mechanism, a different resolver for the destination.
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
                let directive = proto.encode_teleport(
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
        // A spectator attaching its camera to a nearby entity. See
        // `ServerBound::SpectatorAction`'s own doc comment for the resolver
        // and the range/`isPickable` narrowing. Silently does nothing when
        // `apply_spectator_action`'s gates are not all met — the same
        // "request, not an instruction" posture `TeleportToEntity` above
        // takes, and matches vanilla's own handler, which has no failure
        // reply either.
        ServerBound::SpectatorAction { target_entity_id } => {
            if let Some(target_id) =
                apply_spectator_action(*game_mode, target_entity_id, *player_pos, mobs, players)
            {
                apply(conn, state, proto.encode_set_camera(target_id)).await?;
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
        //
        // Decode/verification half: `crate::chat_session::decide`
        // is consulted before broadcasting at all. A rejected message is
        // never queued on `outgoing_chat` — it is reported back to the
        // sender alone (mirroring vanilla's `handleMessageDecodeFailure`,
        // which never disconnects for this), not to every connection the way
        // a real broadcast would. See that module's own doc for exactly what
        // "verified" does and does not mean here.
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
        // `crate::chat_session::ServerChatSession::new` replaces
        // whatever session this connection had announced before — see that
        // type's own doc for why a fresh announcement always resets the
        // verification chain rather than trying to reconcile it.
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
                    // A live issuer set makes an invalid update a fail-closed
                    // replacement. Without one vanilla ignores the update and
                    // retains any existing session instead.
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
        // `ServerboundPingRequestPacket` shares one wire struct across Status
        // and Play (see the decode arm's own comment), so `PingRequest` reaches
        // here too, unlike its `Handshake`/`LoginStart`/etc. siblings below.
        // `ServerGamePacketListenerImpl.handlePingRequest` is exactly "echo the
        // time back" — the same body the Status-state arm above uses, minus the
        // connection close, since a Play-state ping must not end the session.
        ServerBound::PingRequest { time } => {
            apply(conn, state, proto.encode_pong_response(time)).await?;
        }
        // `ServerboundPickItemFromBlockPacket` (middle-click pick, issue
        // #558). `crate::item_use::try_pick_item` is the "where it goes"
        // three-way split (`ServerGamePacketListenerImpl::tryPickItem`); this
        // arm resolves the "what" — the clicked block's clone-item-stack —
        // and the one thing only this function can see: the interaction
        // range and the live block state. `include_data` is not read: the
        // gate it feeds (`addBlockDataToItem`, copying a block entity's NBT
        // onto the stack) has no consumer in this crate, the same "not
        // modelled, no completion hook" scope cut `crate::item_use`'s module
        // doc already takes for other `Item.use` arms.
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
        // `ServerboundPickItemFromEntityPacket` — same split, aimed at
        // `entity.getPickResult()` instead of a block's clone stack. Only the
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
        // Issue #273: `EncryptionResponse` is `State::Login`-only too, same
        // as `LoginStart`/`LoginAcknowledged` beside it.
        | ServerBound::EncryptionResponse { .. }
        | ServerBound::LoginAcknowledged
        | ServerBound::ConfigurationFinished
        | ServerBound::StatusRequest
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
///   [`ServerBound::PlayerMoved`] packet crosses into a new chunk column
///   (vanilla's own `ChunkMap::move`/`updateChunkTracking`);
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
    // Issue #619: same ownership shape as `player_ticket` just above, and for
    // the same reason — this function's every exit path (clean disconnect,
    // a propagated `?`, or task cancellation at shutdown) must withdraw this
    // connection's `PLAYER_LOADING`/`PLAYER_SIMULATION` tickets, not just the
    // happy path. `dispatch_play_packet` calls
    // [`PlayerTicketGuard::move_to`] whenever the tracked view actually
    // recentres or its radius changes, so residency follows the player
    // rather than staying pinned to the join column.
    player_ticket_guard: PlayerTicketGuard,
    mut view: ViewTracker,
    username: String,
    // Issue #329 / the death-screen respawn. The world spawn `serve_connection`
    // already resolved for this join, carried forward rather than re-searched:
    // `find_initial_spawn` is a real spiral over the source, and a respawn is not
    // a good moment to pay for up to 121 columns again. Read only by
    // `apply_client_command`'s `PERFORM_RESPAWN` arm — see its own comment for why
    // it is the *world* spawn and not the per-player bed point.
    world_spawn: Vec3,
    mut chunks_sent: usize,
    // The part of the join view that had **not** gone out when the play loop
    // started (`JOIN_PRESTREAM_RADIUS`), drained by the `join_stream` arm of the
    // `select!` below. Owned: it is this connection's view and dies with it.
    //
    // This is the whole of the owner's "I can't break blocks until it finishes"
    // fix. Nothing else in this signature changed, because nothing else had to:
    // the loop that was already racing a socket read against four timers simply
    // races one more thing.
    mut join_stream: crate::join_scheduler::JoinChunkStream<S>,
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
    // A host-shared snapshot fetched after this connection has completed
    // online authentication. `Some` permits announcement validation;
    // `None` deliberately preserves vanilla's service-unavailable degradation.
    profile_key_issuers: Option<lodestone_auth::MojangPublicKeys>,
    // Separate from issuer availability: whether the host requires a player
    // with no adopted session to sign chat.
    enforce_secure_profile: bool,
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
    // The mode this connection joined in (`serve_connection_inner`'s own), owned
    // because the `change_game_mode` and `/gamemode` arms mutate it and nothing
    // outside this loop reads it.
    mut game_mode: GameMode,
    // Issues #327/#328/#323. The world's shared game rules, difficulty and clock —
    // the same handle `run_tick_loop` ticks. Replaced the `WorldAdminState` local
    // that used to be constructed right here, one per accepted socket.
    world: &crate::world_state::WorldStateHandle,
    // Issue #302's shutdown-cancellation gap. Published to once per iteration
    // of this function's own `select!` loop below — see
    // `crate::live_save::LiveSaveSlot`'s own doc comment for why a
    // continuously-refreshed mirror exists at all: `IntegratedServer::
    // shutdown`'s connection-task race drops this whole function's future
    // mid-`.await` on an ordinary singleplayer quit, so the disconnect-save
    // arm below (the `conn.read_packet()` returning `Ok(None)` branch) is
    // structurally unreachable on that path, and only this mirror survives
    // the cancellation to be read back afterwards.
    live_save: &crate::live_save::LiveSaveSlot,
) -> Result<ServeSummary, ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
    E: EntitySource,
{
    let mut pending_keep_alive: Option<i64> = None;
    let mut pending_break: Option<PendingBreak> = None;
    let mut player_pos: Option<(f64, f64, f64)> = None;
    // Issue #262, alongside `player_pos` — see `dispatch_play_packet`'s own
    // parameter comment.
    let mut player_rot: Option<Rotation> = None;
    // Issue #302. Read here rather than passed in from `serve_connection_inner`
    // (which already read it to place the join): that would be a 31st parameter
    // through eleven wrapper call sites, and this is a few-hundred-byte gzip
    // decode once per join. `player_uuid` is the same uuid that file is keyed by.
    let player_store = player_store(source.get());
    let saved_player = player_store
        .as_ref()
        .and_then(|store| store.read(player_uuid).ok().flatten());
    // The fields `crate::player_data` does not model — hunger, experience, the
    // ender chest, the recipe book. Carried from the loaded file into every save
    // this session makes, so a full load/modify/save cycle preserves them rather
    // than deleting them the first time this player quits. See
    // `PlayerData::preserved`.
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
    let mut open_container: Option<OpenContainer> = None;
    let mut open_merchant: Option<OpenMerchant> = None;
    let mut container_sync = ContainerSync::default();
    // This connection's last-known `ServerBound::PlayerInput` sprint flag —
    // see `apply_attack`'s own doc comment for the one thing it feeds
    // (the melee knockback sprint bonus).
    let mut sprinting = false;
    // Issue #260. This connection's in-progress bow draw — see this parameter's
    // own comment on `dispatch_play_packet`.
    let mut bow_draw: Option<BowDraw> = None;
    // This connection's in-progress eat or drink — see `item_in_use` on
    // `dispatch_play_packet`. Finished by the per-tick arm below, not by a packet.
    let mut item_in_use: Option<ItemInUse> = None;
    // Vanilla's `ServerPlayer::nextContainerCounter` starts at `0` and the
    // very first open bumps it to `1` before use (vanilla's own player fields)
    // — see [`open_container_screen`]'s own `% 100 + 1` wrap.
    let mut next_window_id: i32 = 0;
    // Issue #249. This connection's composter roll stream — see
    // `COMPOSTER_BEHAVIOR_SEED` and `dispatch_play_packet`'s parameter comment.
    let mut composter_rng = SpawnRng::new(COMPOSTER_BEHAVIOR_SEED);
    let mut bone_meal_rng = SpawnRng::new(BONE_MEAL_BEHAVIOR_SEED);
    // Restored from the player file, exactly as `vitals` and `inventory` above are.
    // This was `PlayerExperience::default()` unconditionally while the `.dat`
    // faithfully kept `XpLevel`/`XpP`/`XpTotal` through `PlayerData::preserved` — so
    // XP survived the *file* and not the *session*, and the next save wrote the same
    // untouched bytes back while the player played on at level 0. The fix is this
    // read plus modelling the three fields in `crate::player_data`; either half alone
    // is a regression (see `persist_player`'s own parameter comment).
    let mut experience = saved_player
        .as_ref()
        .map_or_else(crate::experience::PlayerExperience::default, |data| data.experience);
    // Vanilla's `Player.takeXpDelay`. Starts at `0`, so the first orb a player walks
    // into is absorbed immediately — see `collect_nearby_orbs`.
    let mut take_xp_delay: i32 = 0;
    let mut effects = crate::mob_effects::ActiveEffects::new();
    let mut burn = crate::burning::BurnState::new();
    // The `nextInt(1, 3)` ramp draw `BaseFireBlock.fireIgnite` makes on a player's
    // contact tick. Its own stream, so standing in fire cannot shift which roll a
    // later block drop or composter insert sees.
    let mut burn_rng = SpawnRng::new(BURN_BEHAVIOR_SEED);
    // Issue #337. This connection's block-drop roll stream — see
    // `block_drops::BLOCK_DROPS_BEHAVIOR_SEED` and `dispatch_play_packet`'s
    // parameter comment for why it is separate from the composter's.
    let mut drops_rng = SpawnRng::new(crate::block_drops::BLOCK_DROPS_BEHAVIOR_SEED);
    // Issue #329. This connection's per-player respawn point, written by
    // `apply_use_item_on`'s bed arm. Never read here — the placement half of
    // P2 is the next consumer (see `crate::world_spawn`'s module doc).
    let mut respawn: Option<RespawnPoint> = None;
    // Issue #241's raid trigger: vanilla's `ServerPlayer.raidOmenPosition`.
    // Set the tick Bad Omen converts to Raid Omen (the block the carrier
    // stood on then), read and cleared on Raid Omen's own last tick — see
    // the `vitals_tick` arm below for both halves.
    let mut raid_omen_position: Option<BlockPos> = None;
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
    // starts `true` because `serve_connection`'s own initial view burst
    // (sent just before this function was called) is itself an outstanding
    // unacknowledged batch — the first ack this loop receives is for *that*
    // batch, not a later `recenter`/`set_view_radius` one.
    //
    // **The deferred join stream is deliberately not gated on this**, and the
    // reason is that it is not the same kind of send. This gate exists so a
    // *reactive* stream — one new batch per chunk boundary the player crosses,
    // unbounded in time — cannot outrun a client. The join stream is a fixed,
    // finite set the client is already owed and is holding its loading screen
    // for; gating it would make delivering the world depend on a reply, and every
    // `ServerProtocol` fixture in this crate's tests answers a batch with silence.
    // That failure mode is the worst shape available: not a mismatch, a hang.
    let mut awaiting_chunk_batch_ack = true;
    let mut pending_chunk_batches: VecDeque<Vec<ServerDirective>> = VecDeque::new();
    // Issue #469. Filled by `dispatch_play_packet`, drained immediately after
    // it returns — it exists only to carry a message across that call.
    let mut outgoing_chat: Vec<String> = Vec::new();
    // This connection's announced chat-signing session, if any —
    // `None` until a `chat_session_update` arrives, exactly like every other
    // per-connection `Option` this loop threads (`pending_keep_alive`,
    // `player_pos`'s rotation half). See `crate::chat_session`'s own doc.
    let mut chat_session: Option<crate::chat_session::ServerChatSession> = None;
    // This connection's read position in the shared chat log. Started at the
    // log's *current end* so a joining player is not replayed the backlog of
    // everything said before they arrived.
    let mut chat_cursor = entities.players().map_or(0, PlayerRegistry::chat_cursor);
    // This connection's read position in the shared arm-swing broadcast log —
    // same "start at the current end" reasoning as `chat_cursor`, so a
    // freshly joined connection is not replayed swings that happened before
    // it arrived.
    let mut swing_cursor = entities.players().map_or(0, PlayerRegistry::swing_cursor);
    // Issue #335. This connection's read position in the shared plugin-channel
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
    // Same anchoring reasoning again: the initial pass `serve_connection` ran
    // just before calling this function is this connection's first, so a tick
    // firing in the same instant would only re-diff a state nothing has
    // changed yet.
    let mut entity_stream_tick = tokio::time::interval_at(
        crate::tick::PlayTimerInstant::now() + ENTITY_STREAM_INTERVAL,
        ENTITY_STREAM_INTERVAL,
    );
    // `Delay`, for the same reason `keep_alive_tick` sets it: tokio's default
    // `Burst` fires every missed tick back to back with no delay, so a pass
    // that overran (a chunk strip, a container click) would be followed by a
    // run of streaming passes in one instant, each re-diffing state that
    // cannot have changed between them.
    entity_stream_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let play_start = crate::tick::PlayTimerInstant::now();
    let mut next_keep_alive_id: i64 = 0;
    // The deferred join stream's own batch bookkeeping — see
    // `JOIN_STREAM_BATCH_COLUMNS`. `open` is whether a `begin_chunk_batch` has
    // been sent whose `end_chunk_batch` has not; `size` is how many columns are
    // inside it.
    let mut join_batch_open = false;
    let mut join_batch_size: i32 = 0;
    // Issue #302, counted down on `vitals_tick` — see that arm.
    let mut player_save_countdown = PLAYER_SAVE_EVERY_VITALS_TICKS;

    // Vanilla's `ServerPlayer::initInventoryMenu`, the last call in
    // `PlayerList.placeNewPlayer`. `inventory` above is already the restored one, so
    // this is the packet that makes a rejoining player's items visible without
    // touching a slot first — see `join_inventory_snapshot` for the whole story.
    apply(conn, &mut state, join_inventory_snapshot(proto, &inventory)).await?;
    // The first `SET_EXPERIENCE`, which `ServerPlayer.doTick` sends on the tick after
    // every join because `lastSentExp` starts at `-99999999`. Without it the bar has
    // no values at all — see `join_experience`.
    apply(conn, &mut state, join_experience(proto, &experience)).await?;
    republish_experience(entities.players(), player_uuid, &experience);
    // The first `update_attributes` — the armour bar's own packet, and the one
    // this crate never sent at all until now (see `join_attributes`'s own doc
    // comment). Without it `Session::armour_value` never leaves `None` and the
    // row never draws, no matter what the player is wearing.
    apply(conn, &mut state, join_attributes(proto, &inventory)).await?;

    // Portal travel state, in the same place as `take_xp_delay` and for the same
    // reason: it is a per-player per-tick counter and this loop is where those live.
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
        // Same shadow, same reason, for the two handles a live placement or a
        // delayed redstone/fluid request has to land in — see
        // `dimension_scoped_handles`'s own doc comment for why this closes
        // the join-dimension routing bug (Nether furnaces/redstone recording
        // into the overworld's registry and feed).
        let dimension_handles = dimension_scoped_handles(travelled.as_ref());
        let block_entities = dimension_handles
            .block_entities
            .as_ref()
            .unwrap_or(block_entities);
        let block_ticks = dimension_handles.block_ticks.as_ref().unwrap_or(block_ticks);
        tokio::select! {
            // The deferred join view (`JOIN_PRESTREAM_RADIUS`), streamed while this
            // loop goes on servicing everything else — which is the point: a dig,
            // a hurt or a container click no longer waits behind the burst.
            //
            // Disabled once drained, so this is not a branch that returns `None`
            // forever. `select!` polls its branches in a random order, so a ready
            // packet is never starved by a ready column.
            //
            // Both `JoinChunkStream::next` arms are cancel-safe (see their doc
            // comments); a column dropped mid-generation here would be a hole in
            // the world that no test in this crate would notice.
            chunk = join_stream.next(source), if !join_stream.is_done() => {
                watch.enter();
                if let Some(((cx, cz), payload)) = chunk {
                    if !join_batch_open {
                        apply(conn, &mut state, proto.begin_chunk_batch()).await?;
                        join_batch_open = true;
                        join_batch_size = 0;
                    }
                    apply(conn, &mut state, encode_column(proto, cx, cz, payload)).await?;
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
                    // Issue #302: the disconnect save. Vanilla's own
                    // `PlayerList.remove` writes the player file here, and this is
                    // the only exit that is reached with the loop's state still
                    // intact — see the periodic save on `vitals_tick` below for
                    // what covers a crash, a cancelled task and every `?`.
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
                    &mut player_pos,
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
                    &mut composter_rng,
                    &mut bone_meal_rng,
                    &mut experience,
                    &mut effects,
                    &mut drops_rng,
                    client_channels,
                    plugin_channels,
                    &mut game_mode,
                    &mut respawn,
                    sleep_vote,
                    border,
                    player_entity_id,
                    &username,
                    world_spawn,
                    // Issue #531. This loop already counts ticks off
                    // `play_start` for the time-of-day broadcast; the break
                    // validator reads the same clock, so a dig's start and stop
                    // are priced on one monotonic counter.
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
                // **"If the player moves it should properly generate the closer
                // chunks first."** The join stream can now still be draining while
                // the player walks and turns, so the columns it has *not yet
                // started* are re-keyed on the pose that packet just delivered:
                // distance from the player's current column first, the cone they
                // are looking down second (`join_scheduler::priority_key`).
                //
                // Read back from `player_pos`/`player_rot` for the same reason the
                // republish above is: `dispatch_play_packet` has just updated
                // whichever of them the packet carried. Cheap on purpose — this
                // runs on *every* inbound packet, and `reprioritise` does nothing
                // at all unless the centre chunk or the quantised yaw actually
                // changed.
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
                // Issue #337: collect any drops this player is now standing in.
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
                    // Issue #279: tell the client *why* before hanging up.
                    // Vanilla sends `Component.translatable("disconnect.timeout")`
                    // on exactly this path (its own common packet-listener)
                    // — up to now we closed the socket silently and
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
                // Issue #297/#619: vanilla's `Ready.keepAlive()`, run from this
                // connection's own keep-alive timer rather than from the world
                // tick loop (`tick.rs` is off-limits to this change, and the
                // ticket graph's own read-driven check-in means there is no
                // world-tick hook to add regardless — see
                // `ChunkStore::maybe_tick_tickets`'s doc). A no-op, reported by
                // its own `bool`, on every non-`IntegratedServer` entry point,
                // whose `tickets` is a disconnected default that never held a
                // spawn ticket to begin with.
                player_ticket_guard.refresh_world_spawn();
                watch.pass("keep_alive_tick");
            }

            _ = time_sync_tick.tick() => {
                watch.enter();
                // **Issue #323's fix, and it is one line's worth of value.** This
                // used to send `ticks_since(play_start)` — wall-clock elapsed since
                // *this connection* joined — with `None` for the day clock. Every
                // link in the chain was green and a connected client's sky really did
                // move, which is exactly why `cargo xtask connectedness` is blind to
                // it: a fully-connected wire carrying the wrong value.
                //
                // The world's own clock is the source now, and the `day_time` is sent
                // rather than left to the client's own extrapolation, because that is
                // the only way `/gamerule advance_time false` can actually freeze the
                // sun: an empty map means "keep the anchor you have", and the client
                // keeps advancing it.
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
                // Issue #302: the periodic player save, on a counter rather than a
                // clock. `PLAYER_SAVE_EVERY_VITALS_TICKS` of these 50 ms ticks, so
                // no `Instant::now()` is involved — this crate links into a wasm32
                // browser bundle where `Instant::now()` compiles and then panics at
                // runtime under `panic = "abort"` with no log line.
                //
                // **This is not redundant with the disconnect save.** That one is
                // reached on exactly one of this function's exit paths; every `?`,
                // a keep-alive timeout, a task cancelled at shutdown and a crash
                // all skip it. A player who alt-F4s (the common case, not the rare
                // one) would otherwise lose the whole session, which is precisely
                // the silent data loss #302 is about.
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

                // Vanilla's `ServerPlayerGameMode.tick` deferred-destroy pass,
                // riding this timer because it is the one that already fires
                // every 50ms — one server tick, exactly the cadence the
                // continuation is counted in. This is what finishes an ordinary
                // hold-and-release dig: `apply_block_action`'s `StopDestroy` arm
                // defers a dig that fell short of 0.7 rather than refusing it,
                // and nothing else in this loop would ever look at it again.
                //
                // `is_air` first, mirroring vanilla's own `blockState.isAir()`
                // guard: something else (a random tick, another player) may have
                // removed the block while the dig was deferred, and re-breaking
                // air would roll a second set of drops.
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

                // `LivingEntity.updateUsingItem`: a consume ends on the server's
                // own clock, not on a packet — the client sends nothing when a
                // steak finishes, so without this arm every bite starts and none
                // ever lands. Read against `MobSim`'s tick counter because that is
                // the clock `apply_use_item` stamped `finish_tick` from; mixing it
                // with this loop's `ticks_since(play_start)` would compare two
                // unrelated counters.
                if let Some(started) = item_in_use.clone() {
                    let now = mobs.with(|sim| sim.tick_count());
                    // The periodic eating/drinking sound —
                    // `ItemStack.onUseTick` → `Consumable.emitParticlesAndSounds`.
                    // **Sound only**: the crumbs are the client's own prediction,
                    // because `ServerLevel.addParticle` is a no-op in vanilla, and
                    // the sound is *only* the server's, because
                    // `ClientLevel.playSeededSound` drops a `playSound(null, …)`.
                    // Splitting a single vanilla call across the two sides looks like
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
                                // No exclusion: vanilla's `Entity.playSound` passes
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
                            // `FoodProperties.onConsume`: the consumable sound again,
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
                            // The food bar itself. `encode_set_health` is vanilla's
                            // `ClientboundSetHealthPacket`, which carries all three
                            // of health, food and saturation in one packet — so the
                            // bar cannot move without re-sending the health beside
                            // it.
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

                            // `Consumable.onConsume`'s `onConsumeEffects.forEach` —
                            // issue #690's golden-apple/pufferfish/rotten-flesh/
                            // spider-eye/poisonous-potato/chicken/honey-bottle gap.
                            // Runs after the food-bar packet above rather than
                            // folding into it: these are `update_mob_effect`/
                            // `remove_mob_effect` packets, not health-bar fields.
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
                            // `PotionContents.applyToLivingEntity`'s own split: an
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
                    // Issue #326 B1: border damage, applied *before* the
                    // submersion test — vanilla's `LivingEntity.baseTick` runs
                    // the border `else if` ahead of the water-breath block.
                    // Snapshot the border once
                    // per timer tick and ask it for the damage the tracked
                    // position attracts; `apply_border_damage` is `Some` only
                    // when the hit landed (a dead player is a no-op), and a
                    // player past the safe zone takes `max(1, floor(d*0.2))`
                    // *every* tick — the plan gate's per-tick cadence. With a
                    // default full-size border the distance is far inside and
                    // `damage_for_position` is always `None`: nothing is sent
                    // and this costs one clone + one distance scan per 50ms.
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

                    // Issue #625: hostile-mob melee damage against this
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

                // Issue #232's disclosed remainder, closed: `MobSim
                // ::take_mining_fatigue_auras` computed a real per-tick elder
                // guardian pulse but had no drain at all, so the mining-fatigue
                // debuff — a core part of the guardian temple fight — never
                // once reached a real player. Same single-consumer/filter-by-
                // uuid shape as `take_player_hits` just above and for the
                // identical reason (the queue empties for whichever connection
                // reads it first). `MiningFatigueAura`'s own doc comment names
                // both obligations this drains: the effect application and the
                // `GUARDIAN_ELDER_EFFECT` game event (kind `10`,
                // `ClientboundGameEventPacket.GUARDIAN_ELDER_EFFECT` — read
                // from the jar, not guessed).
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
                // (`BaseFireBlock.entityInside` ignites, `Entity.baseTick` consumes)
                // only because the block and the entity are different objects.
                //
                // The **feet** cell, not the eye: `entityInside` fires for any cell the
                // bounding box overlaps, and the feet cell is the one this crate
                // tracks. Reading the eye instead would let a player stand in fire
                // unharmed up to their chin.
                //
                // `!invulnerable`: vanilla's guards are `fireImmune()` on the entity
                // type and `abilities.invulnerable` inside the damage path; a creative
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
                            // `BaseFireBlock.fireIgnite` — the player ramp, which is
                            // why running across one fire block can leave you unburnt.
                            // One draw per contact tick, from this connection's own
                            // stream.
                            crate::burning::BurnSource::Fire
                            | crate::burning::BurnSource::SoulFire => {
                                let ramp = 1 + i32::from(burn_rng.next_f32() < 0.5);
                                burn.fire_ignite(true, ramp);
                            }
                            // `Entity.lavaIgnite` — a flat 15 seconds, no ramp.
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

                // Beacons — issue #616's remainder. Vanilla's own 80-tick
                // world-time gate (`BeaconBlockEntity.tick`'s
                // `level.getGameTime() % 80L == 0L`), run per-connection
                // rather than from a global tick loop: `effects`/the wire
                // notify below are per-connection state, the same
                // architecture the `effects.tick()` block right after this
                // one already uses. **Not** gated on `!effects.is_empty()`
                // the way that block is — a beacon must be able to apply a
                // *first* effect to a player who currently has none.
                if let Some((px, py, pz)) = player_pos
                    && world.time().game_time % 80 == 0
                {
                    let candidates: Vec<(BlockPos, Option<String>, Option<String>)> = block_entities
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
                            crate::beacon::beacon_effects(levels, primary.as_deref(), secondary.as_deref());
                        // Vanilla's own box: `range` blocks horizontally,
                        // from `range` below the beacon to the top of the
                        // world above it (`AABB(pos).inflate(range)
                        // .expandTowards(0, height, 0)`) — approximated as
                        // "no lower than `range` below" with no upper bound,
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
                            effects.apply(&grant.effect, grant.duration_ticks, grant.amplifier);
                            apply(
                                conn,
                                &mut state,
                                proto.encode_update_mob_effect(
                                    // Self-facing, per every other
                                    // `encode_update_mob_effect`-adjacent
                                    // call in this loop.
                                    LOCAL_PLAYER_ENTITY_ID,
                                    &grant.effect,
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

                // Issue #241's raid trigger, traced through the decompile:
                // `BadOmenMobEffect.applyEffectTick` converts Bad Omen into Raid
                // Omen the moment its carrier (not a spectator, not on Peaceful)
                // stands near an occupied village POI, remembering *where* that
                // happened (`raid_omen_position`); `RaidOmenMobEffect
                // .applyEffectTick`, on Raid Omen's own last tick, spends that
                // remembered position on `Raids.createOrExtendRaid`
                // (`MobSim::create_or_extend_raid`) — a flat 64-block radius query
                // over occupied `#village` POIs, **not** `isVillage`'s own
                // section-distance tracker, a different subsystem this port does
                // not build (see `crate::mobs::villager`'s own module doc). Ahead
                // of the generic `effects.tick()` block below, and reading
                // `duration()` *before* it decrements — vanilla's own `tickCount`
                // is the pre-decrement remaining duration.
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

                // Issue #246's remaining gap, closed: `Raid.tick`'s own
                // `hero.addEffect(HERO_OF_THE_VILLAGE, 48000, raidOmenLevel - 1)`
                // loop, fired the moment a raid this player earned a killing
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

                // Issue #276's post-kill controller, wired for real:
                // `dragon::MobSim::record_dragon_death` (now reached from a
                // real hit through `MobSim::attack_dragon`, and from the
                // death-flight health-drive clause in `tick_one_dragon`)
                // queues the exit-portal geometry and egg placement a kill
                // needs but cannot apply itself — `MobSim` holds `world`
                // immutably and owns no connection, the same reason
                // `take_hero_of_the_village_grants` above is a queue rather
                // than an inline effect. `home`, not `source`, resolves the
                // sibling: it is "the only thing that knows the world's
                // siblings" regardless of which dimension *this* connection
                // currently occupies (`SourceRef::Dimension`'s own doc), so
                // whichever connected player's tick reaches this first —
                // not necessarily the one standing in the End — can apply it.
                for death in mobs.with(|sim| sim.take_dragon_deaths()) {
                    if let Some(destination) = home.get().sibling(crate::dimension::Dimension::End) {
                        for (pos, state) in &death.exit_portal_blocks {
                            destination.set_block(pos.x, pos.y, pos.z, state);
                        }
                        if death.outcome.place_dragon_egg {
                            // `EnderDragonFight.setDragonKilled`'s own
                            // `level.getHeightmapPos(MOTION_BLOCKING,
                            // EndPodiumFeature.getLocation(origin))` — the
                            // highest solid block in the podium's own
                            // column. Scanned for real (rather than assumed
                            // to be the bedrock pole's own top) by walking
                            // down from just above the portal's domed air
                            // clearing (`exit_portal_blocks`'s own
                            // `origin.y + 32` ceiling), so a column a player
                            // has already built on still gets the real
                            // surface.
                            let mut egg_y = death.origin.y + 33;
                            while egg_y > death.origin.y
                                && destination.block_state(death.origin.x, egg_y, death.origin.z) == "minecraft:air"
                            {
                                egg_y -= 1;
                            }
                            destination.set_block(death.origin.x, egg_y + 1, death.origin.z, "minecraft:dragon_egg");
                        }
                        // Issue #276/#689's remaining gap, closed: `outcome
                        // .spawn_gateway`'s position formula and shuffled
                        // 20-slice pool (`crate::dragon::fight::GatewayPool`)
                        // are real now — `death.gateway_blocks` is already
                        // resolved to real block writes by
                        // `dragon::MobSim::record_dragon_death`, empty only
                        // when `spawn_gateway` was false or the pool was
                        // exhausted. **The teleport mechanic itself is still
                        // not ported** — see `fight::gateway_blocks`'s own
                        // doc for exactly what that means: a real, visible
                        // structure appears, but standing in it does
                        // nothing.
                        for (pos, state) in &death.gateway_blocks {
                            destination.set_block(pos.x, pos.y, pos.z, state);
                        }
                    }
                }

                // Status effects, ahead of hunger — vanilla ticks `activeEffects` in
                // `LivingEntity.aiStep` before `ServerPlayer.tick` reaches
                // `foodData.tick`, and the order matters for one arm: `hunger`
                // charges exhaustion, so it must land before the exhaustion is spent
                // rather than a tick late.
                //
                // `game_tick` is the entity tick count `ActiveEffects::tick` needs
                // **only** for an infinite effect (vanilla's `target.tickCount`); a
                // finite one counts against its own remaining duration.
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
                // (`LivingEntity.baseTick`'s water-breath block, then
                // `ServerPlayer.tick`'s `foodData.tick`). Runs whether or not a
                // position has been reported, unlike drowning: hunger needs no
                // terrain, only the difficulty and a game rule, and a player who
                // has not moved since joining still starves.
                //
                // `!invulnerable`: vanilla's guard is on `causeFoodExhaustion`, so a
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

                // Portal travel, last in the tick so a player who is about to be
                // moved has already taken this tick's damage and hunger — vanilla's
                // own order (`Entity.handlePortal` runs from `Entity.baseTick`, after
                // `LivingEntity.baseTick`'s damage block).
                //
                // The counter is fed "which portal cell am I standing in", read at
                // the player's **feet**, because `NetherPortalBlock.entityInside` is
                // driven by the entity's bounding box and the feet cell is the one a
                // standing player is always inside. Using the eye cell instead means
                // a 3-tall portal only triggers on its middle row.
                if let Some((x, y, z)) = player_pos {
                    let feet = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
                    let feet_state = source.get().block_state(feet.x, feet.y, feet.z);
                    // An end portal shares this same counter — vanilla's
                    // `Entity.portalProcess`/`portalCooldown` are generic across
                    // portal types, not Nether-specific — so a player cannot be
                    // simultaneously ramping up a Nether trip and an End trip.
                    let in_end_portal = crate::portal::is_end_portal(&feet_state);
                    let standing_in =
                        (in_end_portal || crate::portal::is_portal(&feet_state)).then_some(feet);
                    // `getPortalTransitionTime`: `Portal`'s own default is 0 and
                    // `EndPortalBlock` does not override it, so an end portal
                    // fires on the very first tick inside — the Nether's
                    // gamerule-configurable delay is `NetherPortalBlock`'s own
                    // override, not the shared default. Read off the shared
                    // rules, so a `/gamerule` change takes effect on the next
                    // tick rather than at the next join.
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
                            // `EndPortalBlock.getPortalDestination`'s `fromEnd`
                            // branch is not implemented (see
                            // `travel_through_end_portal`'s own doc) — reached
                            // only by standing in an end portal *inside* the
                            // End, which does not happen without the
                            // stronghold's exit portal. Guarding it here rather
                            // than inside the tracker keeps that gap a single,
                            // named "do nothing" rather than a wrong
                            // destination.
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
                                    game_mode,
                                    mobs,
                                )
                                .await?
                            }
                        } else if rules.allow_entering_nether_using_portals()
                            || source.dimension() == crate::dimension::Dimension::Nether
                        {
                            // `allow_entering_nether_using_portals` is checked here
                            // rather than inside the tracker: vanilla passes
                            // `canUsePortal(false)` into `processPortalTeleportation`,
                            // so the counter still climbs while travel is forbidden,
                            // and turning the rule back on lets a player who has been
                            // standing there travel immediately. The End has no such
                            // gamerule gate in vanilla.
                            travel_through_portal(
                                conn,
                                proto,
                                home,
                                source,
                                &mut state,
                                &mut view,
                                &mut join_stream,
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
                            // The deferred join stream this trip installed has to
                            // start a fresh batch, so close any batch the previous
                            // dimension's stream left open.
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
                // **And the light for them, which this drain used to omit
                // entirely.** `encode_block_update` carries no light, so before
                // this every block change originating in the tick loop moved on the
                // client and left its light behind — stale until the player
                // rejoined and the column was re-encoded from scratch. The reported
                // case was a torch placed underwater: `apply_use_item_on`'s own
                // resend lights the column correctly for the placement, then the
                // fluid tick destroys the torch a tick later and arrives *here*, so
                // the torch vanished and its light did not. Fire, grass, crops, a
                // redstone torch flipping `lit` and a landing falling block all ride
                // this same drain.
                //
                // Deduplicated by column, and that is what makes it affordable: a
                // fluid cascade rewrites many cells in one column in a single tick,
                // and each relight is a whole-column flood. `send_column_light`
                // rather than `resend_column_for_light` because the feed carries only
                // the *new* state — see that function's own doc comment for why the
                // missing old state means this cannot be predicated.
                let mut relight: Vec<(i32, i32)> = Vec::new();
                for (x, y, z, block_state) in block_ticks.drain_all() {
                    apply(conn, &mut state, proto.encode_block_update(x, y, z, &block_state)).await?;
                    let column = (x.div_euclid(16), z.div_euclid(16));
                    if !relight.contains(&column) {
                        relight.push(column);
                    }
                }
                // `source.get()`, like every other non-batch read on this task: one
                // column at a time has nothing to offload, and it is the same
                // accessor `resend_column_for_light`'s callers already use.
                for (cx, cz) in relight {
                    send_column_light(conn, proto, source.get(), &mut state, cx, cz).await?;
                }
                // Issue #530: the same feed's effect lane — every sound,
                // particle and level event the world tick produced. This is what
                // finally gives the server a way to say "play this here": before
                // it, `ServerProtocol` had no sound encoder at all, so a mob
                // could be beaten to death in silence and a redstone door opened
                // without a click. Single-consumer for the reason the drain above
                // is; see `BlockTickFeed`'s own doc comment.
                for effect in block_ticks.drain_effects_for(player_uuid) {
                    // Issue #694, item 4. `PistonPlayerPush` is the one
                    // variant `encode_world_effect` never turns into a
                    // packet (see that variant's own doc) — it is this
                    // connection's own signal to maybe correct its own
                    // last-known position, so it is intercepted here rather
                    // than handed to the generic encoder below.
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
                                proto.encode_teleport(nx, ny, nz, current.yaw, current.pitch),
                            )
                            .await?;
                        }
                        continue;
                    }
                    apply(conn, &mut state, proto.encode_world_effect(&effect)).await?;
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
                // The per-entity animation cues for hits the mob sim resolved:
                // vanilla's `broadcastDamageEvent` (which we send as
                // `hurt_animation` — see `ServerProtocol::encode_hurt_animation`
                // for why the route differs and the pixels do not) and
                // `LivingEntity.die`'s `broadcastEntityEvent(this, (byte)3)`.
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
                // this line takes the queue. That is not reachable today —
                // `IntegratedServer::bind`'s LAN worlds get a `MobHandle::default`
                // with no population — and a second player needs per-connection
                // tracking here, not a feed.
                for animation in mobs.with(crate::mobs::MobSim::take_entity_animations) {
                    let directive = match animation {
                        crate::mobs::MobAnimation::Hurt { entity_id } => {
                            // `0.0` is vanilla's own value for a non-player, not a
                            // placeholder: `LivingEntity.getHurtDir` is a constant
                            // and only `ServerPlayer` overrides it.
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
                    // Arm-swing broadcast, riding the same timer and the same
                    // cursor-over-a-shared-log shape as chat just above. The
                    // sender exclusion lives here, at the read site, rather
                    // than at `PlayerRegistry::swing`'s write site — see that
                    // method's own doc comment for why.
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
                        )
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
                watch.pass("container_sync_tick");
            }
        }
        // Issue #302's shutdown-cancellation gap — see `live_publish_player`'s
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
    }
}

/// The player-`vitals` slice of the native loop's `vitals_tick` `select!` arm
/// above — air supply/drowning, world-border damage, burning, the periodic
/// beacon effect sweep, status effects, hunger, and finishing an
/// in-progress eat/drink — ported to run off
/// [`crate::browser_timer::BrowserInterval`] instead of `tokio::time::
/// interval_at`, so the wasm32 [`serve_play`] loop below can drive it too
/// (issue #636). The beacon sweep was the one piece `docs/beacon.md` had
/// long recorded as a disclosed gap here ("the `wasm32` loop's own effects
/// section... does not yet carry the same beacon sweep") — closed by the
/// same block the native arm already runs, `block_entities` added as a
/// parameter for exactly that reason.
///
/// # Why this is a new function rather than a shared extraction
///
/// The native arm is left untouched rather than refactored to call this
/// function: it is a single ~600-line block threading roughly two dozen
/// mutable locals through one `tokio::select!` arm inside this crate's
/// largest and most delicate file, and this crate's own root `CLAUDE.md`
/// names `server.rs` a "known choke point" where a wholesale rewrite has
/// silently dropped another agent's work before. Duplicating the call
/// sequence into a smaller, purpose-built function for one target is the
/// lower-risk trade against a 1387+1732-test native suite this change must
/// not regress. Every call this function makes is to the same production
/// types/methods the native arm calls (`PlayerVitals::tick`,
/// `BurnState::tick`, `ActiveEffects::tick`, `finish_consuming`,
/// `publish_health`, …), not a reimplementation of any of them.
///
/// # What is deliberately excluded, and why
///
/// - **The periodic player save.** There is no `PlayerDataStore` on `wasm32`
///   at all (no filesystem) — see the native `serve_play`'s own comment on
///   `player_store` for this target's reality, and this function's own lack
///   of a `player_store` parameter.
/// - **The deferred-break continuation**
///   (`PendingBreak::deferred_break_ready`). `PendingBreak::defer` already
///   returns `None` unconditionally when `start_tick` is `None`, and
///   `dispatch_play_packet`'s `wasm32` call site passes `None` for that tick
///   counter (see its own comment, "Issue #531") — so a dig can never even
///   enter the deferred state on this target today, timer or not. Wiring the
///   finish side alone would be exactly the "connected but nothing produces
///   the input" island shape this repo's own `CLAUDE.md` warns about; making
///   it reachable needs a second change (feeding a real tick count into
///   `dispatch_play_packet` on this target), left for a follow-up.
/// - **Hostile-mob melee damage against the player**
///   (`MobHandle::take_player_hits`). Its only producer is `MobSim::tick`,
///   which runs inside `crate::tick::run_tick_loop` — itself entirely
///   `#[cfg(not(target_arch = "wasm32"))]` (see `integrated.rs`'s own module
///   doc). No mob AI ticks on this target at all yet, so this queue is always
///   empty here; wiring the drain would be the same island shape as above.
/// - **Portal travel** (`PortalTracker::tick` firing a trip). A real trip
///   mutates `join_stream`/`view`/the connection's own dimension mid-loop —
///   genuine complexity this change does not take on for a mechanic none of
///   the four originally reported symptoms (drops, fall damage, air supply,
///   worldgen-past-spawn) name. A player standing in a portal on `wasm32`
///   today simply never travels — a real, now-documented gap, not a silent
///   one.
///
/// World-border damage, burning, status effects and hunger *are* included
/// even though this target still has no world-tick loop either, because each
/// has a real, independent, packet-reachable producer already: `BorderFeed::
/// with` is a command/config surface, not the tick loop; standing in
/// fire/lava is a block read, not tick-loop state; and a potion/food item
/// finishing its use is this same function's own `item_in_use` arm.
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
    // The beacon sweep's own read of every tracked `Beacon` block entity —
    // see this function's own doc for why it was missing until now.
    block_entities: &BlockEntityHandle,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + 'static,
{
    let invulnerable = Abilities::for_mode(game_mode).invulnerable;

    // `LivingEntity.updateUsingItem` — see the native arm's identical block
    // (`server::serve_play`'s `vitals_tick`) for the full reasoning: the
    // periodic eat/drink sound, then the finish once `finish_tick` is
    // reached.
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

                // Same `onConsumeEffects` producer the native arm wires — see
                // that call site's own comment (issue #690).
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

    // Border damage, then drowning — vanilla's own order, same as the native
    // arm.
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
        // Issue #636's named gap: air supply/drowning had no timer at all on
        // this target. `!invulnerable &&`: a creative player's air bar does
        // not deplete and they never drown, matching the native arm.
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

    // Burning — the ignition producer and the burn consumer in one place,
    // same as the native arm's identical block and for the same reason (see
    // its own comment).
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

    // Beacons — same block as the native arm's identical comment (`server::
    // serve_play`'s `vitals_tick` `select!` arm), ahead of the status-effect
    // block below for the same reason: it feeds `effects`, and the ordering
    // between them is unobservable either way since neither reads the
    // other's output this tick. **Not** gated on `!effects.is_empty()` —
    // a beacon must be able to apply a *first* effect to a player who
    // currently has none.
    if let Some((px, py, pz)) = player_pos
        && world.time().game_time % 80 == 0
    {
        let candidates: Vec<(BlockPos, Option<String>, Option<String>)> = block_entities.with(|reg| {
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
                crate::beacon::beacon_effects(levels, primary.as_deref(), secondary.as_deref());
            let dx = px - f64::from(pos.x);
            let dz = pz - f64::from(pos.z);
            let dy = py - f64::from(pos.y);
            if dx.mul_add(dx, dz * dz) > range * range || dy < -range {
                continue;
            }
            for grant in &application {
                effects.apply(&grant.effect, grant.duration_ticks, grant.amplifier);
                apply(
                    conn,
                    state,
                    proto.encode_update_mob_effect(
                        LOCAL_PLAYER_ENTITY_ID,
                        &grant.effect,
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

    // Status effects, ahead of hunger — same order as the native arm, and
    // for the same reason (see its own comment: `hunger` charges exhaustion,
    // so it must land before the exhaustion is spent).
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
                // No terrain read backs this arm — see the native arm's
                // identical fallback comment.
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

    // Hunger, after the air block — vanilla's own order, same as the native
    // arm. Runs whether or not a position has been reported, unlike
    // drowning/burning above.
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

/// `wasm32` counterpart of the native [`serve_play`] above — same signature,
/// same [`dispatch_play_packet`] dispatch. Used to be degraded to a bare
/// packet-driven-only loop with no timer at all (`tokio::time` is compiled in
/// for this crate's wasm32 target but **hangs on its first poll** rather than
/// firing — see `crate::browser_timer`'s module doc). Issue #636 gave it one
/// real `tokio::select!` arm, racing the socket read against a single
/// `crate::browser_timer::BrowserInterval` at the vitals cadence — see
/// [`wasm_vitals_tick`] for exactly what that arm does and does not cover.
/// See the native definition's doc comment for why the two forked instead of
/// sharing one body with an internal `cfg`; that reasoning (no real
/// `tokio::time` on this target) is why this loop still has only one timer
/// where native has four, not zero.
///
/// Issue #619's world-spawn-ticket refresh (`PlayerTicketGuard::
/// refresh_world_spawn`) is threaded through this target too, but nothing on
/// this loop calls it — the native definition's own `keep_alive_tick` timer
/// arm is the one caller, and this target's own timer (`vitals_interval`
/// below) is not it, deliberately: a real socket on this target is an
/// in-process `DuplexStream` between this same tab's client and server code,
/// not a remote peer that can go quiet independently, so the failure mode
/// `keep_alive_tick` exists to catch cannot occur here — see the loop body's
/// own comment on `vitals_interval`. Real, not a bug this issue introduces:
/// this target's `open_in_memory` join path is the one production
/// `IntegratedServer` entry point where `PLAYER_SPAWN`'s 20-"tick" countdown (a
/// `TicketStore::tick` unit, driven by `ChunkStore::maybe_tick_tickets`'s own
/// read-cadence check-in — see that method's doc) can lapse with nobody
/// refreshing it. The `PLAYER_LOADING`/`PLAYER_SIMULATION` pair this same
/// guard grants is unaffected (`timeout: 0`, i.e. does not expire), so this
/// only ever costs the world-spawn ring specifically, on this one target.
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
    // Issue #619: same ownership shape as `player_ticket` just above, and for
    // the same reason — this function's every exit path (clean disconnect,
    // a propagated `?`, or task cancellation at shutdown) must withdraw this
    // connection's `PLAYER_LOADING`/`PLAYER_SIMULATION` tickets, not just the
    // happy path. `dispatch_play_packet` calls
    // [`PlayerTicketGuard::move_to`] whenever the tracked view actually
    // recentres or its radius changes, so residency follows the player
    // rather than staying pinned to the join column.
    player_ticket_guard: PlayerTicketGuard,
    mut view: ViewTracker,
    username: String,
    // Issue #329 / the death-screen respawn. The world spawn `serve_connection`
    // already resolved for this join, carried forward rather than re-searched:
    // `find_initial_spawn` is a real spiral over the source, and a respawn is not
    // a good moment to pay for up to 121 columns again. Read only by
    // `apply_client_command`'s `PERFORM_RESPAWN` arm — see its own comment for why
    // it is the *world* spawn and not the per-player bed point.
    world_spawn: Vec3,
    mut chunks_sent: usize,
    // The deferred half of the join view (`JOIN_PRESTREAM_RADIUS`). On the native
    // loop this is a `select!` branch racing the socket read; **this target has no
    // `select!` and no second thread**, so there is no concurrency to win and it is
    // drained inline below, before the packet loop — the unchanged pre-split
    // behaviour, one batch later in the sequence. A browser world therefore still
    // pays the whole burst up front; that is the same documented `wasm32` gap as
    // every timer this loop lacks, not a new one.
    mut join_stream: crate::join_scheduler::JoinChunkStream<S>,
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
    // Issue #636: **wired**, unlike this parameter's neighbours below. Border
    // damage needs no world-tick-loop producer — `BorderFeed::with` is a
    // command/config surface, independent of `crate::tick::run_tick_loop` —
    // so once this loop has any timer at all (`wasm_vitals_tick`, driven by
    // `crate::browser_timer::BrowserInterval` below), there is nothing left
    // stopping it. See that function's own doc for what else this loop's new
    // timer does and does not cover.
    border: &BorderFeed,
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
    // The mode this connection joined in (`serve_connection_inner`'s own), owned
    // because the `change_game_mode` and `/gamemode` arms mutate it and nothing
    // outside this loop reads it.
    mut game_mode: GameMode,
    // Issues #327/#328/#323. The world's shared game rules, difficulty and clock —
    // the same handle `run_tick_loop` ticks. Replaced the `WorldAdminState` local
    // that used to be constructed right here, one per accepted socket.
    world: &crate::world_state::WorldStateHandle,
    // Accepted for signature parity with the native definition and with
    // `serve_connection_inner`'s target-agnostic call, but never published to:
    // this target has no `PlayerDataStore` at all — "there is no player store
    // in the browser", per this loop's own experience-restore comment below —
    // so there is nothing for `IntegratedServer::shutdown` to read back
    // regardless. `LiveSaveSlot::publish` is itself native-only (see its own
    // doc comment), so this parameter is unused on this target by
    // construction, not by oversight.
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
    let mut sprinting = false;
    let mut bow_draw: Option<BowDraw> = None;
    // Issue #636: **finished here now**, unlike the doc below used to say —
    // `wasm_vitals_tick`'s item-in-use arm (driven by the new
    // `vitals_interval` below) is exactly the native loop's own finish logic,
    // ported. A bite started on this target now lands.
    let mut item_in_use: Option<ItemInUse> = None;
    // `player_pos`/`vitals` were tracked for parity only, before issue #636:
    // `vitals` used to be ticked exclusively by the native loop's timer,
    // which `tokio::time` has none of on `wasm32`. **That gap is now closed**
    // for drowning/air supply, border damage, burning, status effects and
    // hunger — see `wasm_vitals_tick`'s own doc for the mechanism and for
    // what is *still* excluded (the periodic save, the deferred-break
    // continuation, mob melee damage, portal travel) and why. Fall damage
    // (`FallTracker`) never needed this: it is driven purely by inbound
    // `PlayerMoved` packets, not a timer, so it already worked identically on
    // both targets.
    let mut player_pos: Option<(f64, f64, f64)> = None;
    // Issue #262, alongside `player_pos` — see `dispatch_play_packet`'s own
    // parameter comment.
    let mut player_rot: Option<Rotation> = None;
    let mut vitals = PlayerVitals::default();
    let mut fall = FallTracker::default();
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
    let mut open_merchant: Option<OpenMerchant> = None;
    let mut container_sync = ContainerSync::default();
    let mut next_window_id: i32 = 0;
    // Issue #249 — see the native `serve_play`'s identical binding: the
    // composter roll stream has no timer and no wasm32 dependency, so it is
    // wired identically on this target.
    let mut composter_rng = SpawnRng::new(COMPOSTER_BEHAVIOR_SEED);
    let mut bone_meal_rng = SpawnRng::new(BONE_MEAL_BEHAVIOR_SEED);
    // `default()`, unlike the native loop's restore: there is no `PlayerDataStore` on
    // `wasm32` (no filesystem), so there is no saved player to read XP out of.
    let mut experience = crate::experience::PlayerExperience::default();
    let mut take_xp_delay: i32 = 0;
    let mut effects = crate::mob_effects::ActiveEffects::new();
    let mut burn = crate::burning::BurnState::new();
    // The `nextInt(1, 3)` ramp draw `BaseFireBlock.fireIgnite` makes on a player's
    // contact tick. Its own stream, so standing in fire cannot shift which roll a
    // later block drop or composter insert sees.
    let mut burn_rng = SpawnRng::new(BURN_BEHAVIOR_SEED);
    // Issue #337 — see the native `serve_play`'s identical binding: the
    // block-drop roll stream has no timer and no wasm32 dependency either.
    let mut drops_rng = SpawnRng::new(crate::block_drops::BLOCK_DROPS_BEHAVIOR_SEED);
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
    // See the native loop's identical binding.
    let mut chat_session: Option<crate::chat_session::ServerChatSession> = None;

    // `ServerPlayer::initInventoryMenu` — see the native `serve_play`'s identical
    // send and `join_inventory_snapshot`'s own doc comment. Placed *before* the
    // inline join-stream drain below rather than after it, so the packet's position
    // relative to the deferred chunks matches native: there the send happens before
    // the `select!` loop that drains the stream. This target's `inventory` is always
    // a fresh `PlayerInventory::default()` (there is no player store in the
    // browser), so the snapshot is 46 empty slots today — sent anyway, because the
    // client's `Menus` fold is what establishes the window-`0` menu it will
    // reconcile every later click against, and a target-specific omission here is
    // exactly how the two loops drift apart.
    apply(conn, &mut state, join_inventory_snapshot(proto, &inventory)).await?;
    // The first `SET_EXPERIENCE` — see the native `serve_play`'s identical send and
    // `join_experience`'s own doc comment. `experience` is `default()` on both
    // targets today (nothing restores it from disk), so this carries zeroes; it is
    // sent anyway because the bar is drawn from the *last* values received and a
    // client that is never sent any has nothing to draw.
    apply(conn, &mut state, join_experience(proto, &experience)).await?;
    republish_experience(entities.players(), player_uuid, &experience);
    // The armour bar's own packet — see the native `serve_play`'s identical
    // send and `join_attributes`'s own doc comment. `inventory` is the same
    // fresh-or-restored value the snapshot above already sent.
    apply(conn, &mut state, join_attributes(proto, &inventory)).await?;

    // The deferred join view, inline — see this function's `join_stream`
    // parameter for why this target does not race it against anything.
    if !join_stream.is_done() {
        apply(conn, &mut state, proto.begin_chunk_batch()).await?;
        let mut batch_size: i32 = 0;
        while let Some(((cx, cz), payload)) = join_stream.next(source).await {
            apply(conn, &mut state, encode_column(proto, cx, cz, payload)).await?;
            chunks_sent += 1;
            batch_size += 1;
        }
        apply(conn, &mut state, proto.end_chunk_batch(batch_size)).await?;
    }

    // Issue #636: this loop's one timer — see `crate::browser_timer`'s module
    // doc for the mechanism (a real browser macrotask via `window.
    // setTimeout`, never `tokio::time`, which hangs on this target rather
    // than merely failing) and `wasm_vitals_tick`'s own doc for exactly what
    // rides it and what does not. Anchored one period out, matching every
    // native `interval_at` call in `serve_play` above (`BrowserInterval::
    // new`'s own doc).
    let mut vitals_interval =
        crate::browser_timer::BrowserInterval::new(WASM_VITALS_TICK_INTERVAL);
    loop {
        let (packet_id, payload) = tokio::select! {
            packet = conn.read_packet() => {
                match packet? {
                    Some(p) => p,
                    // Clean disconnect — same exit as the native loop's
                    // `read_packet` arm, minus that arm's player-save call:
                    // this target has no `PlayerDataStore` to save into (see
                    // `inventory`'s own comment above).
                    None => return Ok(ServeSummary { username, chunks_sent, inventory }),
                }
            }
            // Cancel-safe: `BrowserInterval::tick`'s own doc comment — losing
            // this race to a ready packet leaves the interval's deadline
            // untouched, so nothing is lost by not firing this iteration.
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
            &mut player_pos,
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
            // `None`: this target has no `select!` branch draining the stream, so a
            // column enqueued into it would never be sent. See the parameter's own
            // comment on `dispatch_play_packet`.
            None,
            &commands,
            &mut advancements,
            player_uuid,
            false,
            &mut outgoing_chat,
            &mut chat_session,
            entities.players(),
            block_ticks,
            &mut composter_rng,
            &mut bone_meal_rng,
            &mut experience,
            &mut effects,
            &mut drops_rng,
            client_channels,
            plugin_channels,
            &mut game_mode,
            &mut respawn,
            sleep_vote,
            border,
            player_entity_id,
            &username,
            world_spawn,
            // Issue #531. `None`: this target has no `tokio::time`, so there is
            // no tick counter to price a dig's duration against — the same gap
            // as `vitals` and `container_sync` above. Hardness and range still
            // validate; only the timing test is skipped.
            None,
            &mut bow_draw,
            &mut item_in_use,
            // This target has no portal travel (see `serve_play`'s own doc
            // comment: no `select!`, no `home`, no `pending_travel`), so `source`
            // is never `SourceRef::Dimension` here and this out-parameter can
            // never come back `Some` — a throwaway local rather than a signal
            // anything reads.
            &mut None,
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
        // Issue #337, identical to the native loop — the pickup sweep is
        // packet-driven with no timer, so unlike `vitals`/`sync_open_container`
        // this target loses nothing. See the native loop's comment.
        if let Some((x, y, z)) = player_pos {
            let pickups = collect_nearby_items(
                mobs,
                &mut inventory,
                Vec3::new(x, y, z),
                &mut advancements,
                player_uuid,
                world.time().game_time.saturating_mul(50),
            );
            // Before the slot writes and before `stream_pass`, for the reason the
            // native loop's own comment gives: the client needs the item entity to
            // still exist in order to animate it.
            for take in &pickups.takes {
                apply(
                    conn,
                    &mut state,
                    proto.encode_take_item_entity(
                        take.item_entity_id,
                        // Self-facing, per the earlier native-loop call site.
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
            // Orb absorption, identical to the native loop. Wired here too rather than
            // left as a native-only feature: this sweep is packet-driven with no timer,
            // so `wasm32` loses nothing, and a browser player who could see orbs and not
            // absorb them would be the worse failure.
            if let Some(absorbed) =
                collect_nearby_orbs(mobs, Vec3::new(x, y, z), &mut experience, &mut take_xp_delay)
            {
                apply(
                    conn,
                    &mut state,
                    // Self-facing, per the earlier native-loop call site.
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

    /// The two places `Player.createAttributes()`' `add(Attributes.ATTACK_DAMAGE,
    /// 1.0)` is transcribed must not drift: this module's own constant, and the
    /// attribute base [`lodestone_entity::equipment`] folds equipment onto.
    ///
    /// The control is that the equipment fold *can* move off the base — a diamond
    /// sword resolves to `7.0` — so the equality below is not comparing a value
    /// against a constant that nothing else could change.
    #[test]
    fn bare_hand_damage_is_the_player_attribute_base() {
        let empty = PlayerInventory::new();
        let bare = empty.combat_stats().attack_damage;
        assert!(
            (bare - PLAYER_BARE_HAND_ATTACK_DAMAGE).abs() < 1e-6,
            "an empty hand must resolve to the documented bare-hand figure, got {bare}"
        );
        assert!(
            (f64::from(PLAYER_BARE_HAND_ATTACK_DAMAGE)
                - lodestone_entity::equipment::PLAYER_BASE_ATTACK_DAMAGE)
                .abs()
                < 1e-9,
            "the two transcriptions of the same jar line disagree"
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
            (inv.combat_stats().attack_damage - PLAYER_BARE_HAND_ATTACK_DAMAGE).abs() < 1e-6,
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
    /// `ServerGamePacketListenerImpl.signBook`'s own literal `0`/`true` for
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
    /// vanilla's own `Inventory.isHotbarSlot(slot) || slot == 40` gate.
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
        // explicitly, at `0.0` — **not** an absent entry. This is the exact
        // owner-reported bug (removing the last piece left the HUD frozen at
        // its last non-zero reading) and the reason
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

    /// **The owner-reported bug, reproduced directly.** Equip a helmet and a
    /// chestplate (distinct per-piece values — `3.0` and `8.0` — so a
    /// transposition or an off-by-one cannot hide), remove them one at a
    /// time, and assert the *sequence* of published armour values: `11.0`
    /// (both), `8.0` (helmet off — matches "it sort of updates when I remove
    /// armour"), then `0.0` (chestplate off too — the transition the bug
    /// dropped). Collected into one list and asserted on together, per
    /// `CLAUDE.md`'s evidence standard, so a regression names *which* step
    /// went stale rather than only "some step failed".
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

    /// **The control for the assertion of absence above.** Reinstates the
    /// original `AttributeMap::iter()`-only implementation (reading only
    /// entries the map happens to hold, the exact code
    /// `player_attribute_snapshots` had before this fix) and shows it
    /// reproduces the owner-reported symptom at the final step of the same
    /// removal sequence: `minecraft:armor` is omitted entirely once the last
    /// piece comes off, rather than published at `0.0`. Proves the
    /// `.expect("...must still publish minecraft:armor, explicitly")` above
    /// would actually have fired against the old code, rather than the fix
    /// happening to pass vacuously.
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

    /// [`snap`] with a `leash_link` already set — issue #236.
    fn snap_leashed(id: i32, x: f64, target: i32) -> EntitySnapshot {
        EntitySnapshot { leash_link: Some(target), ..snap(id, x) }
    }

    /// **The discriminating case.** A mob that is *already* leashed by the time
    /// a client first spawns it — a fresh join, or walking back into view range
    /// after the attach happened — must still get the rope: `ADD` followed by
    /// `LINK`. A test that only ever leashes a mob the streamer has already sent
    /// once (an `UPDATE`-path attach) would pass under a fix that only patched
    /// the update branch and left the spawn branch blind, which is exactly the
    /// "partial fix that looks complete during testing" this issue's own report
    /// warns about — nobody would notice until a *second* client joined late.
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

    const SLOT: i32 = 20;
    const DATA: i32 = 21;
    const CONTENT: i32 = 22;

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

    /// Issue #692, end to end through the production dispatch path (not just
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

    /// The anvil end to end through the real click path (issue #254): place a
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

    /// Issue #617: the anvil result can be taken for free without enough XP
    /// levels, because `container_click` had no `Slot.mayPickup` gate at all.
    /// End to end through `apply_container_clicked` (not just
    /// `container_click`'s own unit tests): a 0-XP survival player must be
    /// refused outright — item stays in the result slot, cursor stays empty,
    /// nothing consumed — matching `AnvilMenu.mayPickup`'s
    /// `(hasInfiniteMaterials() || experienceLevel >= cost) && cost > 0`.
    ///
    /// Two companion arms make the refusal above meaningful rather than
    /// vacuous: exactly enough levels succeeds, and creative with *zero*
    /// levels succeeds too (the `hasInfiniteMaterials()` clause) — so this is
    /// not merely "every anvil click above now fails".
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
        // of `AnvilMenu.mayPickup`'s comparison.
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

        // Creative, 0 XP levels: succeeds unconditionally —
        // `player.hasInfiniteMaterials()`'s own exemption clause.
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

    /// The anvil's genuinely bespoke take rule (`AnvilMenu.onTake`): a take
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

    /// The grindstone end to end (issue #254): a single enchanted item in one
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

    /// The smithing table end to end (issue #255): a netherite upgrade through
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

    /// Issue #253-#255's last mile, half one: typing an anvil name reaches
    /// [`crate::anvil::compute`] for real (a pure rename costs exactly 1 XP
    /// level — the number `docs/workstation-economy.md` named as the thing a
    /// player could not yet see) and re-sending the identical name is a no-op,
    /// matching `AnvilMenu.setItemName`'s own dedup.
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

    /// Issue #253-#255's last mile, half two: choosing an enchanting-table
    /// offer through the real click path actually enchants the item, spends
    /// XP levels, consumes lapis, and rerolls the seed
    /// (`Player.onEnchantmentPerformed`) — the join
    /// `docs/workstation-economy.md` named as the only thing still missing.
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

    /// Issue #150, end to end through the real production dispatch: a
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

    /// Issue #150, end to end: a loom with a banner, a dye and a specific
    /// pattern *item* auto-selects that item's one pattern — no
    /// `ContainerButtonClick` needed, matching `LoomMenu.slotsChanged`'s own
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

    /// Issue #150's plugin seam, end to end through the real production
    /// dispatch: [`WiringProofDenySwordUpgrade`] — reproducing
    /// `lodestone-crafting-warden`'s real, shipped `SmithingSwordBan` logic,
    /// registered exactly the way a host would register a plugin's hook,
    /// never called directly — vetoes one specific netherite upgrade by
    /// driving the *actual* `apply_container_clicked` →
    /// `apply_workstation_clicked` → `workstation_result` chain a player's
    /// own smithing-table click goes through. A test calling `on_prepare`
    /// directly (as `lodestone-crafting-warden`'s own unit tests do, for its
    /// logic) would be the closed loop `CLAUDE.md` warns about: it would
    /// prove the hook's answer is correct, never that production asks it
    /// the question.
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

    /// Issue #150's plugin seam, the `Replace` half: [`WiringProofBlessAnvilName`]
    /// — reproducing `lodestone-crafting-warden`'s real, shipped
    /// `AnvilBlessing` logic — tweaks a real anvil rename's result, driven
    /// through the actual `apply_rename_item` → `apply_container_clicked` →
    /// `apply_workstation_clicked` → `workstation_result` chain, exactly the
    /// packets a real `RenameItem` then a real take produce, never a
    /// hand-rolled shortcut.
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
        assert_eq!(after.primary_effect.as_deref(), Some("minecraft:speed"));
        assert_eq!(after.secondary_effect, None);
        assert_eq!(after.payment, Some(stack("minecraft:emerald", 2)), "exactly one payment item is spent");
    }

    /// **Control**: no payment item present must refuse the selection
    /// entirely — vanilla's own `paymentSlot.hasItem()` gate. Without this,
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

    /// The crafting **table**'s 3×3 menu, which has no block entity at all (issue
    /// #529 step 2): clicks reach the table's own grid and the server derives a 3×3
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
    /// around an empty centre, and `ResultSlot.onTake` removes **one** per occupied
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
    /// container, and `ResultSlot.onTake` is reachable only through slot 0.
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
    /// at `fillLevel == 7` with nothing to add.
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

    /// The yaw → horizontal-facing map is vanilla's `Direction.fromYRot`
    /// (vanilla's own per-variant direction field table): yaw 0 = south, 90 = west, ±180 = north,
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

    // `gamemode_command_parses_names_aliases_and_ids` was here, testing
    // `parse_gamemode_command` — a hand-rolled string split that has been
    // deleted. `/gamemode` is now a real Brigadier command in
    // `crate::commands::gamemode`, gated against the captured vanilla tree
    // (`crates/protocol/v770/tests/builtin_command_parity.rs`) and driven
    // end-to-end by `tests/builtin_commands.rs`.
    //
    // Worth recording rather than silently dropping: the deleted test asserted
    // that `gamemode c` and `gamemode 1` parse as creative. **26.2 accepts
    // neither.** `GameType.byName` is an exact match against the four
    // `getSerializedName` values, so the old parser — and the test that pinned
    // it — were *more* permissive than vanilla. No test could have caught that,
    // because the failure only ever made a command work that should have failed.

    /// The three redstone families keep the full property set the signal model
    /// reads, and everything else falls through to `crate::block_placement`
    /// (whose own tests cover the per-block conventions). The observer is
    /// deliberately **not** inverted: `ObserverBlock.getStateForPlacement`
    /// applies `.getOpposite()` twice (vanilla's own observer-block placement state),
    /// so it
    /// watches in the player's look direction — unlike the diodes' single
    /// inversion (vanilla's own diode-block placement state), which makes them face the player.
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
        let air = |_: BlockPos| "minecraft:air".to_string();
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
    // `placement_obstructs_placer` — the server-side half of
    // `BlockItem.canPlace` → `Level.isUnobstructed`. `apply_use_item_on` had
    // no obstruction test of any kind before this: the only legality gate was
    // "is the target cell air or a fluid", so a full block could be placed
    // through a standing player. These test the pure geometry directly rather
    // than driving the whole `apply_use_item_on` pipeline, which needs a live
    // `ChunkSource`/`BlockEntityHandle`/`MobHandle` fixture this predicate
    // does not touch.
    // -----------------------------------------------------------------

    /// The reported bug, most directly: a full block at the target cell the
    /// player is standing in must be refused.
    #[test]
    fn placement_obstructs_placer_refuses_a_full_block_at_the_players_feet() {
        let target = BlockPos::new(0, 64, 0);
        let feet = Vec3::new(0.5, 64.0, 0.5);
        assert!(placement_obstructs_placer(target, "minecraft:stone", feet));
    }

    /// The discriminating arm: a state with an **empty** collision shape must
    /// never be refused, even at the player's own feet — otherwise this is a
    /// blanket "nothing inside the player" rule rather than a real
    /// obstruction test, and vanilla legitimately lets you plant a torch (or a
    /// rail, a pressure plate, redstone dust…) where you stand.
    #[test]
    fn placement_obstructs_placer_allows_an_empty_shape_at_the_players_feet() {
        let target = BlockPos::new(0, 64, 0);
        let feet = Vec3::new(0.5, 64.0, 0.5);
        assert!(
            lodestone_data::collision_shapes::collision_boxes(
                lodestone_data::block_states::state_id("minecraft:torch").unwrap()
            )
            .is_some_and(<[_]>::is_empty),
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
        let boxes = lodestone_data::collision_shapes::collision_boxes(id)
            .filter(|b| !b.is_empty())
            .expect("a top slab has real collision geometry");
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
    // `apply_client_command`'s `dimension_reset` out-parameter — the fix for
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

    /// **The discriminating gate for issue #620.** A sibling with its own
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

    /// No saved player at all: falls back to the world spawn, exactly as
    /// before this fix existed.
    #[test]
    fn join_position_for_saved_player_is_world_spawn_with_no_save() {
        let spawn = Vec3::new(8.0, 71.0, 8.0);
        assert_eq!(join_position_for_saved_player(None, spawn), spawn);
    }

    /// **The discriminating gate for issue #614's "buried in the ground"
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

    /// An unparseable/unknown dimension tag — every save written before this
    /// field was captured accurately — must degrade the same way a genuine
    /// non-overworld tag does, not be treated as trustworthy by default.
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
        let world_spawn = Vec3::new(11.0, 71.0, -4.0);
        let source = DimensionOnly(if away_from_home {
            crate::dimension::Dimension::Nether
        } else {
            crate::dimension::Dimension::Overworld
        });
        let world = crate::world_state::WorldStateHandle::default();
        let mut advancements =
            AdvancementManager::new(Vec::new()).expect("an empty advancement tree is valid");
        let mut dimension_reset: Option<Vec3> = None;

        apply_client_command(
            &mut conn,
            &RespawnOnlyProto,
            &mut state,
            &mut vitals,
            &mut fall,
            world_spawn,
            None,
            &source,
            &world,
            &mut advancements,
            Uuid::nil(),
            0, // PERFORM_RESPAWN
            4, // irrelevant here: only the `REQUEST_GAMERULE_VALUES` arm reads it
            away_from_home,
            &mut dimension_reset,
        )
        .await
        .expect("the fixture protocol never errors");

        drop(client_end);
        dimension_reset
    }

    /// **The fix.** A death away from home must ask the caller to run the same
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

    /// **Control 1.** The identical setup, but not in spectator mode — vanilla
    /// gates the whole feature on `player.isSpectator()`.
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

    /// Issue #241's ominous-bottle consumer half: drinking
    /// `minecraft:ominous_bottle` to completion grants `minecraft:bad_omen`
    /// (120000 ticks, amplifier 0 — see [`finish_drinking_ominous_bottle`]'s
    /// own doc for the disclosed fixed-amplifier narrowing) and consumes the
    /// bottle, mirroring [`finish_consuming`]'s `still_there`/`consume_one`
    /// shape exactly.
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

    /// Issue #690's central gap: a real timed-effect potion (Strength II) must
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

    /// Issue #690's milk gap: drinking milk clears every active effect and
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

    /// Issue #694, item 4: `player_overlaps_piston_sweep` is the overlap test
    /// behind a connection's own `PistonPlayerPush` self-correction. A player
    /// standing in either the source or destination cell (matching
    /// `crate::mobs::piston_shove`'s own two positive cases for a mob) must
    /// overlap; one standing a full block clear of both must not.
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
}
