//! The protocol seam between the version-free integrated server and a
//! version-specific packet format.
//!
//! [`ServerProtocol`] is the mirror of the client's `VersionAdapter`: it is the
//! **only** point where wire ids, encodings, NBT and registries enter the
//! server. A version/protocol crate implements it; this crate never names a
//! protocol number. Keeping the coupling behind one trait is what lets the
//! integrated-server loop stay shared while each version supplies its own
//! encoders/decoders (plan §3).

use lodestone_core::State;
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use crate::chunk::ChunkColumn;

/// A version-free description of one entity's wire-relevant state at a moment in
/// time, handed to a [`ServerProtocol`] so it can encode spawn/move/remove
/// packets without ever seeing the server's internal mob representation.
///
/// The server owns the per-connection "last-sent" bookkeeping and passes the
/// previous snapshot alongside the current one to
/// [`encode_entity_update`](ServerProtocol::encode_entity_update); the protocol
/// stays stateless. Units are deliberate: `position` is world-space blocks
/// (f64), rotation/`head_yaw` are degrees, and `velocity` is **blocks per tick**
/// — the unit vanilla's motion packet packs directly.
#[derive(Debug, Clone, PartialEq)]
pub struct EntitySnapshot {
    /// The entity's network id.
    pub id: i32,
    /// The entity's stable UUID (encoded verbatim in the spawn packet).
    pub uuid: Uuid,
    /// The canonical entity-type key (e.g. `minecraft:zombie`); the protocol
    /// maps it to its own numeric type id.
    pub entity_type: ResourceKey,
    /// World-space feet position, in blocks.
    pub position: Vec3,
    /// Body rotation in degrees.
    pub rotation: Rotation,
    /// Head yaw in degrees (may differ from the body yaw).
    pub head_yaw: f32,
    /// Velocity in **blocks per tick**.
    pub velocity: Vec3,
}

/// A server-bound packet, lifted into the version-free vocabulary the server
/// loop understands.
///
/// The variants mirror vanilla's ack-driven state machine: login success does
/// **not** itself move the connection to [`State::Configuration`] — that only
/// happens once the client's own [`LoginAcknowledged`](Self::LoginAcknowledged)
/// arrives — and configuration does not hand off to [`State::Play`] until the
/// client's [`ConfigurationFinished`](Self::ConfigurationFinished) arrives.
/// This is the same handshake vanilla's server performs and the same one the
/// client-side `VersionAdapter` walks from the other side.
// `PartialEq` only, not `Eq`: `PlayerMoved`'s `f64` fields have no total
// order, so `f64: Eq` does not exist and a derived `Eq` cannot be added here.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum ServerBound {
    /// The handshake selected a next connection state (Status or Login).
    Handshake {
        /// The state the client asked to move into.
        next_state: State,
    },
    /// Login start, carrying the requested username and the profile id the
    /// client presented.
    LoginStart {
        /// The username the client presented.
        username: String,
        /// The profile id the client presented (echoed back in the login
        /// success reply; a real auth-mode server would instead resolve this
        /// from the session server).
        uuid: Uuid,
    },
    /// The client acknowledged login success. This is the server-side signal
    /// to move the connection into [`State::Configuration`] and start sending
    /// configuration-phase directives, mirroring
    /// `ServerboundLoginAcknowledgedPacket`.
    LoginAcknowledged,
    /// The client acknowledged the end of configuration. This is the
    /// server-side signal to move the connection into [`State::Play`] and
    /// begin the join sequence, mirroring
    /// `ServerboundFinishConfigurationPacket`.
    ConfigurationFinished,
    /// The client echoed a previously-sent keep-alive challenge (mirrors
    /// `ServerboundKeepAlivePacket`). `id` is the value that was echoed; the
    /// loop compares it against the challenge it is waiting on before
    /// treating the connection as alive again.
    KeepAlive {
        /// The challenge id the client echoed back.
        id: i64,
    },
    /// The client's absolute position changed (`move_player_pos` /
    /// `move_player_pos_rot` — the only two serverbound movement packets that
    /// carry a position; `move_player_rot` and `move_player_status_only`
    /// carry none and stay [`Ignored`](Self::Ignored)). This is what drives
    /// chunk-cache-center and view-streaming updates; the loop needs no
    /// look/on-ground data for that.
    PlayerMoved {
        /// New absolute x position, in blocks.
        x: f64,
        /// New absolute y position (feet), in blocks.
        y: f64,
        /// New absolute z position, in blocks.
        z: f64,
    },
    /// A block-breaking phase (`ServerboundPlayerActionPacket`'s
    /// `START_DESTROY_BLOCK`/`ABORT_DESTROY_BLOCK`/`STOP_DESTROY_BLOCK`
    /// ordinals). The packet's other four ordinals (drop item, drop stack,
    /// release use, swap-with-offhand, stab) share the same wire packet but
    /// carry no terrain edit — item handling is out of this crate's scope
    /// (see `docs/block-edit.md`) — and decode to [`Ignored`](Self::Ignored)
    /// instead.
    BlockAction {
        /// Which phase of the break this is.
        action: BlockActionKind,
        /// Target block position.
        pos: BlockPos,
        /// Face being mined. Decoded for parity with the wire packet; the
        /// current break handling does not use it (no per-face behaviour is
        /// modelled).
        face: BlockFace,
        /// Client block-prediction sequence number. Decoded but not yet
        /// acted on — this crate does not send
        /// `ClientboundBlockChangedAckPacket`; see `docs/block-edit.md`'s
        /// scope note.
        sequence: i32,
    },
    /// Right-click placement against a block face
    /// (`ServerboundUseItemOnPacket`).
    ///
    /// Carries only what full-cube placement needs: the clicked block and
    /// the face that was clicked determine the placement cell (see
    /// `crate::server`'s handling). The packet's hand and cursor-position
    /// fields are decoded off the wire by `ServerProtocol::decode` but not
    /// threaded through here — this crate has no inventory model to pick an
    /// item with, and no per-block placement rules (stairs/slabs/doors) that
    /// would need a precise cursor hit; see `docs/block-edit.md`'s scope note.
    UseItemOn {
        /// The block face the client clicked.
        pos: BlockPos,
        /// Which face of `pos` was clicked.
        face: BlockFace,
        /// Client block-prediction sequence number (see
        /// [`BlockAction::sequence`](Self::BlockAction) for why it is
        /// decoded but not yet acted on).
        sequence: i32,
    },
    /// A packet the loop does not need to act on (chunk-batch
    /// acknowledgements, teleport confirmations, look-only or status-only
    /// movement). The loop ignores these but stays connected.
    Ignored,
}

/// A side effect the [`ServerProtocol`] asks the connection layer to perform,
/// mirroring the client-side `Directive`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerDirective {
    /// Write a client-bound packet with this protocol-specific id and body.
    Send {
        /// Protocol-specific packet id.
        packet_id: i32,
        /// Encoded packet body.
        payload: Vec<u8>,
    },
    /// Move the connection to a new state (applied after preceding sends).
    SetState(State),
    /// Enable or reconfigure zlib compression (negative disables).
    SetCompression(i32),
    /// No side effect — the scalar analog of returning an empty directive list.
    /// Used by the default entity encoders so a protocol without entity support
    /// emits nothing rather than a bogus packet; the connection layer skips it.
    None,
}

/// Implemented by a protocol/version crate to translate packets for the
/// integrated server.
///
/// The server loop calls, in order:
/// 1. [`decode`](ServerProtocol::decode) on every inbound packet;
/// 2. [`login_success`](ServerProtocol::login_success) once a
///    [`ServerBound::LoginStart`] arrives, to emit the login-success reply
///    (this does **not** change state — vanilla's client only moves to
///    Configuration once it has sent its own acknowledgement);
/// 3. [`begin_configuration`](ServerProtocol::begin_configuration) once the
///    resulting [`ServerBound::LoginAcknowledged`] arrives, to emit whatever
///    configuration-phase packets precede the finish signal;
/// 4. [`begin_play`](ServerProtocol::begin_play) once
///    [`ServerBound::ConfigurationFinished`] arrives, to emit the join
///    sequence (join game, spawn position, initial teleport, chunk cache
///    center);
/// 5. [`begin_chunk_batch`](ServerProtocol::begin_chunk_batch),
///    [`encode_chunk`](ServerProtocol::encode_chunk) for each column in the
///    client's initial view, then
///    [`end_chunk_batch`](ServerProtocol::end_chunk_batch) — mirroring
///    vanilla's `PlayerChunkSender` flow control, which allows exactly one
///    unacknowledged batch until the client's first acknowledgement.
///
/// Once in [`State::Play`], the loop additionally drives, on its own
/// schedule rather than in response to any one inbound packet:
/// [`encode_keep_alive`](ServerProtocol::encode_keep_alive) (a fixed
/// interval, disconnecting on an unanswered challenge),
/// [`encode_set_time`](ServerProtocol::encode_set_time) (once at join with a
/// day/night anchor, then periodically with just the game-time broadcast),
/// and [`encode_chunk_cache_center`](ServerProtocol::encode_chunk_cache_center)
/// / [`encode_forget_chunk`](ServerProtocol::encode_forget_chunk) /
/// [`begin_chunk_batch`](ServerProtocol::begin_chunk_batch) /
/// [`encode_chunk`](ServerProtocol::encode_chunk) /
/// [`end_chunk_batch`](ServerProtocol::end_chunk_batch) (whenever a
/// [`ServerBound::PlayerMoved`] crosses into a new chunk column); and
/// [`encode_block_update`](ServerProtocol::encode_block_update) (in reply to
/// a [`ServerBound::BlockAction`] or [`ServerBound::UseItemOn`], confirming a
/// dig or placement back to the acting client). See `crate::server`'s module
/// docs for the scheduling itself, which is version-free and lives entirely
/// outside this trait.
pub trait ServerProtocol: Send + Sync {
    /// Lifts one inbound (server-bound) packet into [`ServerBound`].
    ///
    /// `packet_id` is protocol-specific and must not escape the implementor.
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound;

    /// Emits the login-success reply for a freshly-presented username/uuid.
    /// Does not itself request a state change; the client's own
    /// acknowledgement (lifted to [`ServerBound::LoginAcknowledged`]) is what
    /// drives the transition to [`State::Configuration`].
    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective>;

    /// Emits the directives sent once the connection has moved into
    /// [`State::Configuration`] (in reply to
    /// [`ServerBound::LoginAcknowledged`]), ending with whatever finishes the
    /// configuration phase from the server's side.
    fn begin_configuration(&self) -> Vec<ServerDirective>;

    /// Emits the join sequence once the connection has moved into
    /// [`State::Play`] (in reply to [`ServerBound::ConfigurationFinished`]):
    /// the join-game packet, default spawn position, initial teleport, and
    /// chunk-cache center. Does not send any chunks; the loop calls
    /// [`begin_chunk_batch`](Self::begin_chunk_batch)/
    /// [`encode_chunk`](Self::encode_chunk)/
    /// [`end_chunk_batch`](Self::end_chunk_batch) separately so it can drive
    /// the view radius itself.
    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective>;

    /// Marks the start of a chunk batch (vanilla's `CHUNK_BATCH_START`, an
    /// empty body in every known protocol).
    fn begin_chunk_batch(&self) -> ServerDirective;

    /// Encodes one terrain column into a client-bound packet.
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective;

    /// Marks the end of a chunk batch of `batch_size` columns (vanilla's
    /// `CHUNK_BATCH_FINISHED`).
    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective;

    /// Emits any directives to send right after the initial chunk batch has
    /// gone out (a post-join system chat message, say). Optional: the default
    /// sends nothing, so an implementor that has no such content need not
    /// override it.
    fn welcome_message(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    /// Encodes an entity's initial appearance for a client that has not seen it
    /// (vanilla `ADD_ENTITY`, plus any immediate follow-up the protocol bundles).
    /// The default emits nothing, so a protocol without entity support need not
    /// override it.
    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        let _ = entity;
        ServerDirective::None
    }

    /// Encodes a per-tick update for an entity the client already tracks, given
    /// the previously-sent snapshot (`None` before the first update was sent) so
    /// the protocol can choose an absolute or relative encoding without holding
    /// any per-connection state itself. The default emits nothing.
    fn encode_entity_update(
        &self,
        prev: Option<&EntitySnapshot>,
        current: &EntitySnapshot,
    ) -> Vec<ServerDirective> {
        let _ = (prev, current);
        Vec::new()
    }

    /// Encodes the removal of a batch of entities in one packet (vanilla
    /// `REMOVE_ENTITIES`, a count-prefixed id list). The default emits nothing.
    fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
        let _ = ids;
        ServerDirective::None
    }

    /// Encodes a server-initiated keep-alive challenge (vanilla
    /// `ClientboundKeepAlivePacket`, wire id `keep_alive`). `id` is the
    /// challenge value the loop expects echoed back as
    /// [`ServerBound::KeepAlive`]. The default emits nothing, so a protocol
    /// without keep-alive support need not override it.
    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        let _ = id;
        ServerDirective::None
    }

    /// Encodes a time-of-day update (vanilla `ClientboundSetTimePacket`, wire
    /// id `set_time`).
    ///
    /// `game_time` is the monotonic world age in ticks. `day_time`, when
    /// `Some`, anchors the day/night clock to that many elapsed ticks at the
    /// normal 1:1 rate — sent once at join, mirroring vanilla's full clock
    /// sync (`ServerClockManager::createFullSyncPacket`, sent from
    /// `PlayerList.sendLevelInfo`). `None` sends only the monotonic
    /// game-time broadcast vanilla repeats every 20 ticks
    /// (`MinecraftServer::forceGameTimeSynchronization`) without touching the
    /// client's already-held day/night anchor. The default emits nothing.
    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        let _ = (game_time, day_time);
        ServerDirective::None
    }

    /// Encodes a chunk-cache-center update (vanilla
    /// `ClientboundSetChunkCacheCenterPacket`, wire id
    /// `set_chunk_cache_center`), sent whenever the player's tracked chunk
    /// column changes (`ChunkMap::applyChunkTrackingView`). The default emits
    /// nothing.
    fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
        let _ = (cx, cz);
        ServerDirective::None
    }

    /// Encodes a forget/unload signal for one chunk column leaving view
    /// (vanilla `ClientboundForgetLevelChunkPacket`, wire id
    /// `forget_level_chunk`; `ChunkMap::dropChunk`). The default emits
    /// nothing.
    fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
        let _ = (cx, cz);
        ServerDirective::None
    }

    /// Encodes a single block-state change (vanilla
    /// `ClientboundBlockUpdatePacket`, wire id `block_update`), confirming a
    /// break or placement back to the acting client — mirroring vanilla's
    /// own `ServerPlayerGameMode`/`ServerGamePacketListenerImpl`, which
    /// answer every dig/place with this same packet whether or not the edit
    /// actually took effect (see `crate::server`'s `UseItemOn` handling for
    /// why it sends two of these per placement).
    ///
    /// `state` is the canonical block-state string [`ChunkColumn`] itself
    /// stores (e.g. `"minecraft:air"`, `"minecraft:stone"`); resolving it to
    /// a wire registry id is the implementor's job, the same seam
    /// `encode_chunk` already crosses. The default emits nothing.
    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        let _ = (x, y, z, state);
        ServerDirective::None
    }
}
