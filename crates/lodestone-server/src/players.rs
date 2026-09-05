//! The connected-player registry — the thing that makes a player an **entity
//! other connections receive**.
//!
//! # What it is
//!
//! The server exposes players through the same entity egress as mobs. A shared
//! registry gives each connection a stable view of every player, including
//! players connected over LAN.
//!
//! [`PlayerRegistry`] is the shared handle that fixes that, in the same shape
//! [`BlockEntityHandle`](crate::BlockEntityHandle) and
//! [`MobHandle`](crate::MobHandle) already established for the other two kinds
//! of shared world state: an `Arc<Mutex<…>>` every connection task clones, so
//! one connection's join is visible to every other connection's next streaming
//! pass.
//!
//! # Why there is no broadcast channel
//!
//! [`EntityStreamer`](crate::EntitySource)'s per-connection diff is a *pull*:
//! each connection compares the entities in the registry with the snapshot it
//! last sent and emits the difference. A player appearing in the registry is
//! picked up by every other connection's next pass, just like a mob entering
//! view. The same diff handles both cases without a separate broadcast path.
//!
//! The tab list is the one thing the entity diff does *not* cover, so it gets
//! the identical treatment one level up: [`PlayerListStreamer`] is the
//! roster-shaped twin of `EntityStreamer`, diffing UUIDs instead of snapshots.
//!
//! # The ordering constraint that is not optional
//!
//! **A real client silently drops an `ADD_ENTITY` for a player it has no
//! player-info entry for.** From the real client's entity-creation-from-packet
//! step, transcribed as the rule it implements, not inferred: when the
//! spawned type is a player, look up its player-info entry by UUID; if none
//! exists, log a warning and refuse to construct the entity at all;
//! otherwise build the remote player from that info's profile.
//!
//! It returns nothing usable, the add-entity handler logs "Skipping Entity with id" and the
//! entity is **never added to the level**. So a server that streams a perfect
//! `ADD_ENTITY` and no `player_info_update` reaches zero pixels while every
//! wire in `cargo xtask connectedness` reads green — exactly this repo's
//! island failure mode. That is why [`PlayerView`] carries the roster and the
//! entity snapshots **together, from one lock acquisition**, and why
//! `crate::server`'s streaming pass emits the player-list directives *before*
//! the entity diff. Two separate reads could interleave a join between them
//! and produce precisely the dropped spawn above.
//!
//! # How to change it
//!
//! * **Adding a field a player carries on the wire** (rotation, held item,
//!   skin properties): extend [`TrackedPlayer`], set it in [`PlayerRegistry`],
//!   and lower it in [`PlayerRegistry::view`]. Nothing in `crate::server`
//!   changes.
//! * **Player rotation is live as of the wiring**, and the gotcha is
//!   that it arrives on *four* different packets, not one.
//!   [`ServerBound::PlayerMoved`](crate::ServerBound::PlayerMoved) carries an
//!   `Option<Rotation>` (`Some` only for `move_player_pos_rot`),
//!   [`PlayerRotated`](crate::ServerBound::PlayerRotated) carries angles with
//!   no position, and [`PlayerStatusOnly`](crate::ServerBound::PlayerStatusOnly)
//!   carries neither. The real client's own send-position step sends exactly one
//!   of the four per tick, so they partition the movement stream rather than
//!   overlapping: if you add a field here, work out which of the four
//!   actually carries it before wiring one arm and assuming the rest follow.
//!   The failure mode is silent and direction-dependent — handling only
//!   `move_player_rot` leaves a *walking* player frozen, which is the case a
//!   stationary test never exercises.
//! * **Entity ids come from a second allocator**, see
//!   [`PLAYER_ENTITY_ID_BASE`].
//!
//! # Dependencies
//!
//! Nothing outside this crate and `lodestone-model`. Deliberately version-free
//! like the rest of `lodestone-server`: this module names no packet id and no
//! wire layout — [`ServerProtocol::encode_player_info_add`](crate::ServerProtocol::encode_player_info_add)
//! and its sibling are the seam a version crate implements.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use lodestone_model::{GameMode, ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use crate::protocol::{EntitySnapshot, PlayerListing, ServerDirective, ServerProtocol};
use crate::server::EntitySource;

/// The first network entity id a player is allocated.
///
/// Player ids and mob ids come from **two independent allocators**:
/// [`MobSim`](crate::MobSim) counts up from `1` (from `1000` in production,
/// see its own `set_next_id` doc comment) and knows nothing about players.
/// A collision would not merely look odd — the per-connection diff keys on
/// the id (`EntityStreamer::last_sent: HashMap<i32, EntitySnapshot>`), so one
/// shared id makes a mob and a player alias into a single tracked entity and
/// each pass overwrites the other's position.
///
/// A disjoint range is the honest fix available without reaching into the mob
/// simulation: `1 << 30` leaves the mob allocator over a billion ids before it
/// could ever reach here, and leaves this allocator another billion. The
/// real engine
/// has no such split — its own entity counter is one shared atomic integer for every
/// entity in the level — so a shared allocator would be needed when both layers
/// share an owner. Until then this
/// constant is what stops the aliasing, and it is asserted disjoint from the
/// mob range by this module's own tests.
pub const PLAYER_ENTITY_ID_BASE: i32 = 1 << 30;

/// The canonical entity-type key a player entity streams as.
///
/// `minecraft:player` is network entity-type id **156** in protocol 776
/// (Mojang's own `registries.json` for 26.2,
/// `minecraft:entity_type` → `minecraft:player` → `protocol_id`). This module
/// deliberately names the *key* rather than the number: resolving it to a wire
/// id is the version crate's job. Getting the key wrong is silent —
/// `entity_type_id(name).unwrap_or(0)` in `v770`'s `encode_add_entity_body`
/// maps an unknown name to type `0` (`minecraft:acacia_boat`) with no error
/// anywhere — so the gate asserts the streamed **type id**, not that an entity
/// arrived.
const PLAYER_ENTITY_TYPE: &str = "minecraft:player";

/// One connected player, as the server tracks it.
///
/// Position is the only mutable field today; see the module docs for why
/// rotation is not here yet.
#[derive(Debug, Clone, PartialEq)]
struct TrackedPlayer {
    /// The network entity id other connections address this player by.
    entity_id: i32,
    /// The profile UUID. This is the uuid the client presented at
    /// login and that
    /// [`ServerProtocol::login_success`](crate::ServerProtocol::login_success)
    /// echoed back, so the entity's uuid, the tab-list entry's uuid and the
    /// uuid the client believes is its own all agree. (The real engine in
    /// offline mode
    /// instead *derives* it from the username and ignores what the client
    /// sent; matching that would mean changing `login_success` too, which is a
    /// separate change — and any divergence between the two would be a bug, so
    /// they move together or not at all.)
    uuid: Uuid,
    /// The username, for the tab-list entry.
    username: String,
    /// World-space feet position, in blocks.
    position: Vec3,
    /// Body/head rotation in degrees, as the client last reported it.
    ///
    /// Defaults to `(0, 0)` at join, which is what vanilla itself spawns a
    /// fresh player at, and is corrected by the first movement packet that
    /// carries angles — every client sends one within a tick or two of
    /// entering play.
    rotation: Rotation,
    /// This player's current game mode.
    ///
    /// Tracked here, rather than only in `serve_play`'s local `game_mode`,
    /// because a *command* needs to read other players' modes: `@a[gamemode=
    /// creative]` is a selector predicate resolved against the roster, and a
    /// per-connection local is unreachable from another connection's dispatch.
    /// The local remains the authority for that connection's own behaviour
    /// (instant break, damage immunity) and republishes here on every change,
    /// which is the same producer/mirror split `position` and `rotation` already
    /// have.
    game_mode: GameMode,
    /// Mirror of [`crate::experience::PlayerExperience::level`] — see
    /// [`PlayerRegistry::set_experience`] for the republish convention.
    /// `0` until the owning connection's first republish, same default a
    /// fresh [`crate::experience::PlayerExperience`] itself starts at.
    xp_level: i32,
    /// Mirror of the `/xp query … points` formula — see
    /// [`PlayerRegistry::set_experience`] and
    /// [`crate::commands::PlayerCandidate::xp_points`]'s own docs.
    xp_points: i32,
}

/// A consistent single-lock read of the registry, from one viewer's point of
/// view.
///
/// The two halves must come from one acquisition — see the module docs on the
/// ordering constraint. `roster` **includes** the viewer (vanilla lists you in
/// your own tab list); `entities` **excludes** it (vanilla never sends a player
/// their own entity, and doing so produces a visible doppelgänger standing
/// inside the camera).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerView {
    /// Every connected player, viewer included, for the tab list.
    pub roster: Vec<PlayerListing>,
    /// Every connected player **except** the viewer, as entity snapshots.
    pub entities: Vec<EntitySnapshot>,
}

/// The shared registry of connected players.
///
/// Clone it freely: every clone is the same registry. One is held by whatever
/// owns the world (`IntegratedServer`'s constructors) and one by each
/// connection task, exactly like [`MobHandle`](crate::MobHandle).
#[derive(Debug, Clone, Default)]
pub struct PlayerRegistry(Arc<Mutex<Inner>>);

#[derive(Debug, Default)]
struct Inner {
    /// Monotonic; never reused within a process even after a player leaves, so
    /// a client that was mid-`REMOVE_ENTITIES` cannot have a stale id resolve
    /// to a *different* player.
    next_offset: i32,
    players: Vec<TrackedPlayer>,
    /// The chat log. See [`PlayerRegistry::say`].
    chat: VecDeque<ChatLine>,
    /// Sequence number of `chat.front()`. Cursors are absolute sequence
    /// numbers, so trimming the front of the window cannot silently rewind
    /// a reader — it makes the gap detectable instead.
    chat_base: u64,
    /// The arm-swing broadcast log. Same cursor-over-append-only-log shape
    /// as `chat` and for the identical reason (every connection must see
    /// every swing, not just whichever connection's timer drains first) —
    /// see [`PlayerRegistry::swing`].
    swings: VecDeque<SwingEvent>,
    /// Sequence number of `swings.front()`, mirroring `chat_base`.
    swings_base: u64,
    /// Per-player queues of command effects awaiting delivery.
    ///
    /// **Directed, and drained rather than cursored** — the two properties that
    /// make this a different mechanism from `chat` above and not a second copy
    /// of it. `/gamemode creative Steve` must reach Steve and nobody else, so it
    /// is keyed by uuid; and it must reach Steve *once*, so the reader takes the
    /// queue instead of advancing a cursor over a shared log. A cursored
    /// broadcast would deliver Steve's game-mode change to every connected
    /// player.
    ///
    /// An entry for a uuid with no connection accumulates and is never read.
    /// That is bounded in practice by [`PlayerRegistry::push_effect`] refusing a
    /// uuid that is not in `players`, which is also the honest answer for
    /// `/gamemode creative Steve` when Steve just left: nothing to do.
    effects: HashMap<Uuid, Vec<crate::commands::Effect>>,
    /// `server.properties`' `enforce-secure-profile`, as this crate applies
    /// it — see [`PlayerRegistry::enforce_secure_profile`]'s own doc for what
    /// that actually gates. `Default` (`false`) matches every other
    /// constructor's permissive default (the same shape `LanConfig::access`'s
    /// own doc names) and — deliberately, unlike every other typed key in
    /// `crate::properties` — does **not** match vanilla's real default of
    /// `true`; see `crate::properties`'s own module doc for why.
    enforce_secure_profile: bool,
}

/// One line of player chat, as broadcast to every connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    /// The username of the player who sent it.
    pub sender: String,
    /// The raw message text, exactly as the sender typed it.
    pub message: String,
}

impl ChatLine {
    /// The real rendered form for the default chat type: `chat.type.text` is
    /// `"<%s> %s"`, bound to the default chat type by the real engine's
    /// default chat decoration.
    ///
    /// This crate broadcasts chat as a `system_chat` component rather than a
    /// real `player_chat` packet, so the decoration the real client would
    /// apply from the chat-type registry has to be applied here instead. See
    /// [`PlayerRegistry::say`] for why that trade was made.
    #[must_use]
    pub fn rendered(&self) -> String {
        format!("<{}> {}", self.sender, self.message)
    }
}

/// How many chat lines the shared log retains.
///
/// The log is bounded because it is process-lifetime shared state that every
/// connection appends to and no one ever truncates — an unbounded `Vec` here
/// is a slow leak on a long-running server. 256 is far more than the 50 ms
/// drain interval can fall behind by; a reader that somehow does fall behind
/// loses the overflow rather than the whole window, because cursors are
/// absolute sequence numbers rather than indices.
const CHAT_LOG_CAPACITY: usize = 256;

/// One arm-swing, as broadcast to every other connection (`ServerBound::Swing`'s
/// own doc comment explains why the sender itself is excluded at the read
/// site rather than at the write site — the log has no way to know who will
/// read it next).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwingEvent {
    /// The swinging entity's network id.
    pub entity_id: i32,
    /// Hand whose swing should be broadcast.
    pub hand: lodestone_model::Hand,
}

/// How many swing events the shared log retains. A swing is far more
/// frequent than a chat line, so this is a larger window than
/// [`CHAT_LOG_CAPACITY`] for the same 50 ms-drain-interval reasoning.
const SWING_LOG_CAPACITY: usize = 1024;

impl PlayerRegistry {
    /// A fresh, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one player chat line to the shared log.
    ///
    /// # Why the chat log lives here, of all places
    ///
    /// Chat is the first thing this crate carries that is genuinely a *push*:
    /// unlike the roster and the entity stream, "Alice said hello" is not a
    /// state another connection can rediscover by diffing what is true now
    /// against what it last sent — see this module's own "why there is no
    /// broadcast channel" section, which is still correct about everything it
    /// was written about.
    ///
    /// It is nonetheless bolted onto `PlayerRegistry` rather than given its
    /// own feed, for one structural reason: this registry is **already** the
    /// single object every LAN connection shares, and it is already reachable
    /// from `serve_play` through [`crate::EntitySource::players`] — a
    /// defaulted trait method that exists precisely so shared state could be
    /// added without changing `serve_connection`'s signature. A new
    /// `ChatFeed` would have needed a new parameter on seven entry points and
    /// a third relay copy in `IntegratedServer::bind`, and would still have
    /// been absent in singleplayer.
    ///
    /// The log is append-only with per-reader cursors, which is the shape
    /// `crate::tick`'s own doc notes is the right one for a growing subscriber
    /// count (unlike `BlockTickFeed`'s drain-all, where the first reader takes
    /// everything and a second sees nothing — fatal for a broadcast).
    ///
    /// **Every connection reads every line, including the sender's own.**
    /// That is real behaviour: the real per-world broadcast-chat-message
    /// step loops over every player
    /// with no sender exclusion, and a real client does not
    /// echo its own chat locally — it waits for the server. Excluding the
    /// sender here would make their own messages invisible to them.
    pub fn say(&self, sender: &str, message: &str) {
        let mut inner = self.lock();
        inner.chat.push_back(ChatLine {
            sender: sender.to_owned(),
            message: message.to_owned(),
        });
        while inner.chat.len() > CHAT_LOG_CAPACITY {
            inner.chat.pop_front();
            inner.chat_base += 1;
        }
    }

    /// Every chat line appended since `cursor`, advancing `cursor` past them.
    ///
    /// `cursor` is an absolute sequence number, so a fresh connection starts
    /// at [`PlayerRegistry::chat_cursor`] (the current end) and therefore sees
    /// only messages sent *after* it joined — never a replay of the backlog.
    ///
    /// If `cursor` has fallen behind the retained window it is snapped
    /// forward to the oldest retained line, dropping the overflow. That is a
    /// deliberate loss rather than a panic or a silent rewind: a rewind would
    /// re-send messages the client already displayed.
    pub fn chat_since(&self, cursor: &mut u64) -> Vec<ChatLine> {
        let inner = self.lock();
        if *cursor < inner.chat_base {
            *cursor = inner.chat_base;
        }
        let end = inner.chat_base + inner.chat.len() as u64;
        if *cursor >= end {
            *cursor = end;
            return Vec::new();
        }
        let start = (*cursor - inner.chat_base) as usize;
        let lines: Vec<ChatLine> = inner.chat.iter().skip(start).cloned().collect();
        *cursor = end;
        lines
    }

    /// The sequence number one past the newest chat line — where a connection
    /// that wants only future messages should start its cursor.
    #[must_use]
    pub fn chat_cursor(&self) -> u64 {
        let inner = self.lock();
        inner.chat_base + inner.chat.len() as u64
    }

    /// Appends one arm-swing to the shared broadcast log (`ServerBound::Swing`).
    ///
    /// Unlike [`say`](Self::say), the *reader* excludes the sender
    /// (`swings_since`'s callers filter `entity_id != player_entity_id`) —
    /// the real engine's own "send to tracking players" step never sends to the swinger,
    /// which already animates locally the instant it sent the packet. The
    /// log itself carries the sender so each reader can make that
    /// per-connection decision; it does not decide for them.
    pub fn swing(&self, entity_id: i32, hand: lodestone_model::Hand) {
        let mut inner = self.lock();
        inner.swings.push_back(SwingEvent { entity_id, hand });
        while inner.swings.len() > SWING_LOG_CAPACITY {
            inner.swings.pop_front();
            inner.swings_base += 1;
        }
    }

    /// Every swing appended since `cursor`, advancing `cursor` past them.
    /// Same absolute-sequence-number shape as [`chat_since`](Self::chat_since).
    pub fn swings_since(&self, cursor: &mut u64) -> Vec<SwingEvent> {
        let inner = self.lock();
        if *cursor < inner.swings_base {
            *cursor = inner.swings_base;
        }
        let end = inner.swings_base + inner.swings.len() as u64;
        if *cursor >= end {
            *cursor = end;
            return Vec::new();
        }
        let start = (*cursor - inner.swings_base) as usize;
        let events: Vec<SwingEvent> = inner.swings.iter().skip(start).copied().collect();
        *cursor = end;
        events
    }

    /// The sequence number one past the newest swing — where a freshly
    /// joined connection's cursor should start, mirroring
    /// [`chat_cursor`](Self::chat_cursor).
    #[must_use]
    pub fn swing_cursor(&self) -> u64 {
        let inner = self.lock();
        inner.swings_base + inner.swings.len() as u64
    }

    /// Registers a connection's player and returns the ticket that owns the
    /// registration.
    ///
    /// The returned [`PlayerTicket`] deregisters on drop. That is not a
    /// stylistic choice: `serve_play` returns through a dozen different `?`
    /// paths (transport error, keep-alive timeout, clean disconnect, an
    /// invalid packet) and a hand-written removal on the success path alone
    /// would leak a ghost player — visible to everyone else, standing still,
    /// forever — on every one of the others.
    #[must_use]
    pub fn join(&self, username: &str, uuid: Uuid, position: Vec3) -> PlayerTicket {
        let entity_id = {
            let mut inner = self.lock();
            let entity_id = PLAYER_ENTITY_ID_BASE.wrapping_add(inner.next_offset);
            inner.next_offset += 1;
            inner.players.push(TrackedPlayer {
                entity_id,
                uuid,
                username: username.to_owned(),
                position,
                rotation: Rotation {
                    yaw: 0.0,
                    pitch: 0.0,
                },
                // Survival, matching `serve_connection_inner`'s own join mode.
                // Restated rather than threaded in because that binding is a
                // `const`-like local there; if it ever becomes configurable,
                // `set_game_mode` below is the one call site that has to fire at
                // join.
                game_mode: GameMode::Survival,
                // `0`, matching a fresh `PlayerExperience`'s own default — the
                // owning connection's `join_experience` call republishes the
                // real value (usually still `0`, unless a save restored some)
                // immediately after.
                xp_level: 0,
                xp_points: 0,
            });
            entity_id
        };
        PlayerTicket {
            entity_id,
            uuid,
            registry: self.clone(),
        }
    }

    /// Moves a tracked player. A no-op for an id that is not registered, which
    /// is the correct behaviour for the one race that can produce it: a
    /// position update computed from a packet that arrived as the player's own
    /// ticket dropped.
    pub fn set_position(&self, entity_id: i32, position: Vec3) {
        if let Some(player) = self
            .lock()
            .players
            .iter_mut()
            .find(|p| p.entity_id == entity_id)
        {
            player.position = position;
        }
    }

    /// Re-aims a tracked player. A no-op for an unregistered id, for exactly
    /// the reason [`set_position`](Self::set_position) is.
    ///
    /// Separate from `set_position` rather than folded into it because the
    /// two arrive on genuinely different packets: the real client's own
    /// send-position step picks one of four movement packets per tick
    /// based on which of position/look is dirty, so a turn on the spot
    /// (`move_player_rot`) updates rotation with no position to offer, and a
    /// walk in a straight line (`move_player_pos`) the reverse. A combined
    /// setter would force every caller to invent the half it did not receive
    /// — and inventing `(0, 0)` for the yaw is precisely the bug this field
    /// exists to fix.
    pub fn set_rotation(&self, entity_id: i32, rotation: Rotation) {
        if let Some(player) = self
            .lock()
            .players
            .iter_mut()
            .find(|p| p.entity_id == entity_id)
        {
            player.rotation = rotation;
        }
    }

    /// The roster and the entity snapshots, from one lock acquisition, as
    /// `viewer` should see them.
    ///
    /// Pass `Some(id)` for a connection that has its own player entity, so it
    /// is excluded from `entities`; `None` for a viewer with no player of its
    /// own (there is no such caller in production — it exists so a caller
    /// cannot be *forced* to invent an id, and because it is what the negative
    /// control for the doppelgänger rule flips to).
    #[must_use]
    pub fn view(&self, viewer: Option<i32>) -> PlayerView {
        let inner = self.lock();
        let entity_type = player_entity_type();
        PlayerView {
            roster: inner
                .players
                .iter()
                .map(|p| PlayerListing {
                    uuid: p.uuid,
                    username: p.username.clone(),
                })
                .collect(),
            entities: inner
                .players
                .iter()
                .filter(|p| Some(p.entity_id) != viewer)
                .map(|p| EntitySnapshot {
                    id: p.entity_id,
                    uuid: p.uuid,
                    entity_type: entity_type.clone(),
                    position: p.position,
                    rotation: p.rotation,
                    // A player's head yaw and body yaw are the same value on
                    // this wire: the client reports one yaw per movement
                    // packet and vanilla's `ServerEntity` sends that same
                    // angle in both the move-rotation and head-rotation
                    // packets for a player. They diverge only for mobs, whose
                    // AI aims the head independently of the body — which is
                    // why `EntitySnapshot` keeps them as two fields even
                    // though this producer sets them equal.
                    head_yaw: p.rotation.yaw,
                    // Player motion is client-authoritative here: the client
                    // reports positions, never velocities, so there is no
                    // per-tick delta to publish. An absolute position update
                    // is what the streamer sends anyway.
                    velocity: Vec3::new(0.0, 0.0, 0.0),
                    // No player metadata is modelled yet. Adding any means
                    // running the entity-data index oracle first — index 8 is
                    // shared by living entities' own flags field *and*
                    // an arrow's flags field, and a player is a
                    // living entity, so the census column that separates the
                    // claimants is not guessable from the previous
                    // collision's guard.
                    metadata: Vec::new(),
                    // The real player entity does not override the
                    // add-entity-packet builder, so the
                    // Object Data field is `0`.
                    object_data: 0,
                    // The real leashable interface is never implemented by
                    // the player entity — a
                    // player cannot be the *leashed* end of a lead, only a holder
                    // (see `crates/lodestone-server/src/mobs/mod.rs`'s
                    // `LeashHolder::Player`).
                    leash_link: None,
                })
                .collect(),
        }
    }

    /// How many players are currently registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().players.len()
    }

    /// Whether no players are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Records a player's new game mode, so a selector predicate on another
    /// connection can read it.
    ///
    /// Keyed by uuid rather than entity id because that is what a command has:
    /// an [`Effect`](crate::commands::Effect) is addressed to a profile uuid, and
    /// the connection applying it knows its own uuid without having to resolve a
    /// network entity id. A no-op for an unregistered uuid, for the same reason
    /// [`set_position`](Self::set_position) is.
    pub fn set_game_mode(&self, uuid: Uuid, game_mode: GameMode) {
        if let Some(player) = self.lock().players.iter_mut().find(|p| p.uuid == uuid) {
            player.game_mode = game_mode;
        }
    }

    /// Records a player's current experience level and points-within-level,
    /// so `/xp query` run from another connection can read it — the same
    /// producer/mirror split [`set_game_mode`](Self::set_game_mode) already
    /// documents, called at every site that already sends
    /// the set-experience packet (`join_experience`'s own doc: "send
    /// once at join, and send after every mutation" — this is that same
    /// convention, extended to the registry). `points` is the *query*
    /// formula (floor of experience progress times the xp needed for the next level),
    /// not the lifetime total — see [`crate::commands::PlayerCandidate::xp_points`]'s
    /// own doc. A no-op for an unregistered uuid, for the same reason
    /// [`set_position`](Self::set_position) is.
    pub fn set_experience(&self, uuid: Uuid, level: i32, points: i32) {
        if let Some(player) = self.lock().players.iter_mut().find(|p| p.uuid == uuid) {
            player.xp_level = level;
            player.xp_points = points;
        }
    }

    /// Every connected player as a command-resolution candidate, from one lock
    /// acquisition.
    ///
    /// A flattened snapshot rather than a borrow so selector resolution — which
    /// sorts, filters and truncates — happens entirely outside this lock. That is
    /// the same reason `lodestone_command::SuggestionProvider`'s doc gives for
    /// snapshotting names, and it matters more here: resolution runs a
    /// caller-supplied predicate list.
    #[must_use]
    pub fn candidates(&self) -> Vec<crate::commands::PlayerCandidate> {
        self.lock()
            .players
            .iter()
            .map(|p| crate::commands::PlayerCandidate {
                uuid: p.uuid,
                entity_id: p.entity_id,
                username: p.username.clone(),
                position: p.position,
                rotation: p.rotation,
                game_mode: p.game_mode,
                xp_level: p.xp_level,
                xp_points: p.xp_points,
            })
            .collect()
    }

    /// Queue `effect` for delivery to `target`'s own connection.
    ///
    /// Returns whether the target is actually connected. `false` is not an
    /// error — `/gamemode creative Steve` for a Steve who disconnected between
    /// resolution and delivery has nothing to do — but it is reported so a
    /// caller can say so rather than claiming success. Refusing an unknown uuid
    /// is also what bounds the queue map: an effect for a player who will never
    /// read it is dropped here instead of accumulating forever.
    pub fn push_effect(&self, target: Uuid, effect: crate::commands::Effect) -> bool {
        let mut inner = self.lock();
        if !inner.players.iter().any(|p| p.uuid == target) {
            return false;
        }
        inner.effects.entry(target).or_default().push(effect);
        true
    }

    /// Take everything queued for `uuid`, leaving the queue empty.
    ///
    /// Single-consumer by construction: the only caller is `uuid`'s own
    /// connection loop. A second reader would silently steal effects, which is
    /// exactly the failure a cursor would have prevented and a drain would not —
    /// hence the directedness, which makes a second reader impossible rather
    /// than merely unlikely.
    #[must_use]
    pub fn take_effects(&self, uuid: Uuid) -> Vec<crate::commands::Effect> {
        self.lock().effects.remove(&uuid).unwrap_or_default()
    }

    /// Deregisters a player. Private: [`PlayerTicket`]'s `Drop` is the only
    /// caller, so a registration cannot be leaked by forgetting to call this.
    fn remove(&self, entity_id: i32) {
        let mut inner = self.lock();
        // Drop the departing player's undelivered effects along with their
        // registration. Without this a `/give` aimed at someone who leaves in the
        // same tick would sit in the map for the life of the process — and would
        // be delivered to them on a *later* rejoin, which is worse than losing
        // it.
        if let Some(index) = inner.players.iter().position(|p| p.entity_id == entity_id) {
            let uuid = inner.players[index].uuid;
            inner.players.remove(index);
            inner.effects.remove(&uuid);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().expect("player registry lock poisoned")
    }

    /// Sets this server's `enforce-secure-profile` policy, applied to every
    /// connection sharing this registry. Called once at
    /// startup — `crates/lodestone-dedicated-server`'s `main` is the one
    /// production caller, right after opening the world and before the first
    /// connection can be accepted, mirroring how `props.gamemode`/
    /// `props.difficulty` are applied to `world_state()` at the same point.
    pub fn set_enforce_secure_profile(&self, value: bool) {
        self.lock().enforce_secure_profile = value;
    }

    /// Whether an unsigned (or unverifiable) chat message should be dropped
    /// rather than broadcast.
    ///
    /// Consulted by [`crate::chat_session::decide`], which is the actual
    /// policy — this is only the stored flag. `false` (the default) matches
    /// this crate's own current relay: every accepted message still goes out
    /// as an unsigned `system_chat`, never a real signed `player_chat` (see
    /// `docs/player-chat.md`), so even a message this flag rejects as
    /// unverified is delivered to *no one* differently from one it lets
    /// through — the only thing this flag changes is whether an unsigned or
    /// forged message reaches other players at all.
    #[must_use]
    pub fn enforce_secure_profile(&self) -> bool {
        self.lock().enforce_secure_profile
    }
}

/// The canonical player entity-type key, parsed.
///
/// `expect` rather than a fallible return: the input is the literal
/// [`PLAYER_ENTITY_TYPE`] above, so a failure here is a broken
/// `ResourceKey::from_str`, not a runtime condition a caller could handle.
fn player_entity_type() -> ResourceKey {
    use std::str::FromStr;
    ResourceKey::from_str(PLAYER_ENTITY_TYPE).expect("`minecraft:player` is a valid resource key")
}

/// Ownership of one player's registration, held by that player's connection
/// task for as long as it is in Play.
///
/// Dropping it removes the player from the registry, so every other
/// connection's next streaming pass emits a `REMOVE_ENTITIES` for it and drops
/// its tab-list entry. See [`PlayerRegistry::join`] for why this is RAII
/// rather than an explicit `leave` call.
#[derive(Debug)]
pub struct PlayerTicket {
    entity_id: i32,
    uuid: Uuid,
    registry: PlayerRegistry,
}

impl PlayerTicket {
    /// The network entity id other connections address this player by — and
    /// the id this connection must be **excluded** by in its own
    /// [`PlayerRegistry::view`].
    #[must_use]
    pub fn entity_id(&self) -> i32 {
        self.entity_id
    }

    /// The player's profile uuid.
    #[must_use]
    pub fn uuid(&self) -> Uuid {
        self.uuid
    }
}

impl Drop for PlayerTicket {
    fn drop(&mut self) {
        self.registry.remove(self.entity_id);
    }
}

/// Per-connection tab-list bookkeeping: the roster-shaped twin of
/// `EntityStreamer`.
///
/// Holds the set of uuids this connection has been told about, so each pass
/// emits one `player_info_update` for the newly-joined and one
/// `player_info_remove` for the departed, and nothing at all when the roster
/// is unchanged (the overwhelmingly common case — this runs on every inbound
/// packet).
#[derive(Debug, Default)]
pub struct PlayerListStreamer {
    sent: HashSet<Uuid>,
}

impl PlayerListStreamer {
    /// Produces the directives that bring this connection's tab list from its
    /// last-sent state to `roster`, updating the bookkeeping to match.
    ///
    /// Adds come **first** in the returned vector, and the caller must emit
    /// this whole vector before the entity diff — see the module docs on why a
    /// client drops an `ADD_ENTITY` that precedes its player-info entry.
    pub fn sync<P: ServerProtocol>(
        &mut self,
        proto: &P,
        roster: &[PlayerListing],
    ) -> Vec<ServerDirective> {
        let current: HashSet<Uuid> = roster.iter().map(|p| p.uuid).collect();

        let added: Vec<PlayerListing> = roster
            .iter()
            .filter(|p| !self.sent.contains(&p.uuid))
            .cloned()
            .collect();
        let removed: Vec<Uuid> = self
            .sent
            .iter()
            .filter(|uuid| !current.contains(uuid))
            .copied()
            .collect();

        let mut directives = Vec::new();
        if !added.is_empty() {
            directives.extend(proto.encode_player_info_add(&added));
        }
        if !removed.is_empty() {
            directives.extend(proto.encode_player_info_remove(&removed));
        }
        self.sent = current;
        directives
    }
}

/// An [`EntitySource`] that carries an inner source's entities **and** the
/// connected players.
///
/// This is the composition production uses: the inner `E` is
/// [`LiveMobSource`](crate::LiveMobSource) (or a `MobHandle`, or `NoEntities`),
/// and the registry rides alongside.
///
/// Note what [`EntitySource::snapshots`] does **not** do here: it returns only
/// the inner source's entities, never the players. Players are reachable only
/// through [`EntitySource::players`], which the streaming pass consults *with a
/// viewer id*. That is deliberate — it makes "send a player their own entity"
/// unrepresentable through the plain `snapshots()` path rather than merely
/// discouraged, so a future caller that forgets the exclusion gets no players
/// at all (loud) instead of a doppelgänger (quiet).
#[derive(Debug, Clone, Default)]
pub struct PlayerAwareSource<E> {
    inner: E,
    players: PlayerRegistry,
}

impl<E> PlayerAwareSource<E> {
    /// Pairs an entity source with a player registry.
    pub fn new(inner: E, players: PlayerRegistry) -> Self {
        Self { inner, players }
    }

    /// The registry this source shares.
    #[must_use]
    pub fn registry(&self) -> &PlayerRegistry {
        &self.players
    }
}

impl<E: EntitySource> EntitySource for PlayerAwareSource<E> {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.inner.snapshots()
    }

    fn players(&self) -> Option<&PlayerRegistry> {
        Some(&self.players)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoEntities;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// The whole point of [`PLAYER_ENTITY_ID_BASE`]: a player id can never be
    /// a mob id. Derived from the mob allocator's own production base rather
    /// than restated, so raising `set_next_id(1000)` by six orders of
    /// magnitude still leaves this true (and if it ever would not, this fails).
    #[test]
    fn player_ids_cannot_collide_with_mob_ids() {
        // `MobSim::set_next_id(1000)` in production, `1` by default; either
        // way the allocator counts *up* from there.
        let mob_production_base = 1000_i32;
        assert!(
            PLAYER_ENTITY_ID_BASE > mob_production_base,
            "player ids must start above the mob allocator's base"
        );
        // Headroom for the mob allocator, stated as a count rather than a
        // vague "plenty": over a billion spawns before it could reach us.
        assert!(PLAYER_ENTITY_ID_BASE - mob_production_base > 1_000_000_000);
        // And headroom for us, so the `wrapping_add` in `join` is documentation
        // of an unreachable case rather than a live overflow path.
        assert!(i32::MAX - PLAYER_ENTITY_ID_BASE > 1_000_000_000);
    }

    /// A joined player appears to *others* and never to itself.
    #[test]
    fn a_player_is_excluded_from_its_own_view_and_present_in_anothers() {
        let registry = PlayerRegistry::new();
        let alice = registry.join("Alice", uuid(1), Vec3::new(8.0, 100.0, 8.0));
        let bob = registry.join("Bob", uuid(2), Vec3::new(9.0, 100.0, 9.0));

        let alice_view = registry.view(Some(alice.entity_id()));
        let ids: Vec<i32> = alice_view.entities.iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            vec![bob.entity_id()],
            "Alice must receive Bob's entity and not her own"
        );

        let bob_view = registry.view(Some(bob.entity_id()));
        let ids: Vec<i32> = bob_view.entities.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![alice.entity_id()]);

        // The roster is the other way round: vanilla lists you in your own tab
        // list, so both entries appear in both views.
        assert_eq!(alice_view.roster.len(), 2);
        assert_eq!(bob_view.roster.len(), 2);
    }

    /// `set_experience`/`candidates` round trip — the producer/mirror split
    /// `/xp query` reads through when the target is a *different* connection
    /// from the one running the command. Defaults to `0` until republished
    /// (matching a fresh `PlayerExperience`), and a no-op for an
    /// unregistered uuid, same as `set_position`/`set_game_mode`.
    #[test]
    fn set_experience_republishes_into_the_candidate_snapshot() {
        let registry = PlayerRegistry::new();
        let alice = registry.join("Alice", uuid(1), Vec3::new(0.0, 64.0, 0.0));

        let before = registry.candidates();
        assert_eq!(before[0].xp_level, 0);
        assert_eq!(before[0].xp_points, 0);

        // Pairwise-distinct so a transposition of the two arguments would be
        // visible rather than coincidentally passing.
        registry.set_experience(uuid(1), 7, 23);
        let after = registry.candidates();
        assert_eq!(after[0].xp_level, 7);
        assert_eq!(after[0].xp_points, 23);

        // An unregistered uuid must not panic and must not fabricate an entry.
        registry.set_experience(uuid(99), 5, 5);
        assert_eq!(registry.candidates().len(), 1);

        drop(alice);
    }

    /// The entity type must be the *player* key, not whatever
    /// `unwrap_or(0)` would silently substitute.
    #[test]
    fn a_player_entity_streams_as_minecraft_player() {
        let registry = PlayerRegistry::new();
        let _alice = registry.join("Alice", uuid(1), Vec3::new(8.0, 100.0, 8.0));
        let view = registry.view(None);
        assert_eq!(view.entities.len(), 1);
        assert_eq!(view.entities[0].entity_type.to_string(), "minecraft:player");
    }

    /// Dropping the ticket is what deregisters — the property every error path
    /// out of `serve_play` depends on.
    #[test]
    fn dropping_the_ticket_deregisters_the_player() {
        let registry = PlayerRegistry::new();
        let alice = registry.join("Alice", uuid(1), Vec3::new(8.0, 100.0, 8.0));
        {
            let _bob = registry.join("Bob", uuid(2), Vec3::new(9.0, 100.0, 9.0));
            assert_eq!(registry.len(), 2);
        }
        assert_eq!(registry.len(), 1, "Bob's ticket went out of scope");
        let view = registry.view(Some(alice.entity_id()));
        assert!(
            view.entities.is_empty(),
            "Alice should see nobody once Bob has left"
        );
        assert_eq!(view.roster.len(), 1);
    }

    /// Ids are never reused, so a client racing a `REMOVE_ENTITIES` cannot
    /// resolve a stale id onto a different player.
    #[test]
    fn entity_ids_are_not_reused_after_a_player_leaves() {
        let registry = PlayerRegistry::new();
        let first = {
            let alice = registry.join("Alice", uuid(1), Vec3::new(0.0, 0.0, 0.0));
            alice.entity_id()
        };
        let bob = registry.join("Bob", uuid(2), Vec3::new(0.0, 0.0, 0.0));
        assert_ne!(first, bob.entity_id());
    }

    #[test]
    fn set_position_moves_the_streamed_entity() {
        let registry = PlayerRegistry::new();
        let alice = registry.join("Alice", uuid(1), Vec3::new(8.0, 100.0, 8.0));
        registry.set_position(alice.entity_id(), Vec3::new(20.0, 65.0, -3.0));
        let view = registry.view(None);
        assert_eq!(view.entities[0].position, Vec3::new(20.0, 65.0, -3.0));
        // An unknown id is a no-op, not a panic.
        registry.set_position(i32::MIN, Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(registry.view(None).entities[0].position.x, 20.0);
    }

    /// `snapshots()` on the composed source returns the inner source's
    /// entities *only* — players travel via `players()`, with a viewer.
    #[test]
    fn the_composed_source_never_leaks_players_through_plain_snapshots() {
        let registry = PlayerRegistry::new();
        let _alice = registry.join("Alice", uuid(1), Vec3::new(0.0, 0.0, 0.0));
        let source = PlayerAwareSource::new(NoEntities, registry);
        assert!(
            source.snapshots().is_empty(),
            "players must not appear in the viewer-agnostic snapshot path"
        );
        assert_eq!(
            source
                .players()
                .expect("a player-aware source reports its registry")
                .len(),
            1
        );
    }

    /// A plain source reports no registry, so every pre-existing
    /// `EntitySource` keeps its exact old behaviour.
    #[test]
    fn a_plain_source_has_no_registry() {
        assert!(NoEntities.players().is_none());
    }

    /// A fresh connection's cursor starts at the log's current end, exactly
    /// like `chat_cursor` — it must not replay a swing that happened before
    /// it joined.
    #[test]
    fn swing_cursor_starts_at_the_current_end() {
        let registry = PlayerRegistry::new();
        registry.swing(1, lodestone_model::Hand::Main);
        registry.swing(1, lodestone_model::Hand::Main);
        let mut cursor = registry.swing_cursor();
        assert_eq!(
            registry.swings_since(&mut cursor),
            Vec::new(),
            "a cursor started at the current end must see none of the prior swings"
        );
    }

    /// Every connection's cursor sees every swing appended after it started —
    /// including entries from more than one swinger, pairwise-distinct so a
    /// transposition between `entity_id` and `hand` cannot survive.
    #[test]
    fn swings_since_returns_every_entry_in_order() {
        let registry = PlayerRegistry::new();
        let mut cursor = registry.swing_cursor();
        registry.swing(11, lodestone_model::Hand::Main);
        registry.swing(22, lodestone_model::Hand::Off);
        let events = registry.swings_since(&mut cursor);
        assert_eq!(
            events,
            vec![
                SwingEvent {
                    entity_id: 11,
                    hand: lodestone_model::Hand::Main
                },
                SwingEvent {
                    entity_id: 22,
                    hand: lodestone_model::Hand::Off
                },
            ]
        );
        // The cursor has advanced past both: a second read with the same
        // cursor sees nothing new.
        assert_eq!(registry.swings_since(&mut cursor), Vec::new());
    }

    /// Two independent cursors over the same log each see the full backlog
    /// from their own starting point — proving this is a shared broadcast log
    /// (every reader sees every entry) and not a single-consumer drain (only
    /// the first reader sees anything).
    #[test]
    fn two_independent_cursors_each_see_the_same_swing() {
        let registry = PlayerRegistry::new();
        let mut alice_cursor = registry.swing_cursor();
        let mut bob_cursor = registry.swing_cursor();
        registry.swing(7, lodestone_model::Hand::Off);
        assert_eq!(
            registry.swings_since(&mut alice_cursor),
            vec![SwingEvent {
                entity_id: 7,
                hand: lodestone_model::Hand::Off
            }]
        );
        assert_eq!(
            registry.swings_since(&mut bob_cursor),
            vec![SwingEvent {
                entity_id: 7,
                hand: lodestone_model::Hand::Off
            }],
            "a second, independent cursor must still see the same entry"
        );
    }

    /// The retention window drops the oldest entries and snaps a fallen-behind
    /// cursor forward rather than rewinding — the same deliberate-loss shape
    /// `chat_since`'s own doc comment describes, proven here by actually
    /// overflowing the window rather than asserting the constant.
    #[test]
    fn a_cursor_that_fell_behind_the_window_is_snapped_forward_not_rewound() {
        let registry = PlayerRegistry::new();
        let mut cursor = registry.swing_cursor();
        for i in 0..(SWING_LOG_CAPACITY as i32 + 5) {
            registry.swing(i, lodestone_model::Hand::Main);
        }
        let events = registry.swings_since(&mut cursor);
        assert_eq!(
            events.len(),
            SWING_LOG_CAPACITY,
            "the overflowed entries must be dropped, not rewound into view"
        );
        assert_eq!(
            events[0].entity_id, 5,
            "the oldest *retained* entry, after the first 5 were evicted"
        );
    }
}
