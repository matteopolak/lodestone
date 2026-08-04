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
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, Difficulty, ItemStack, ResourceKey, Rotation, Vec3,
};
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
    /// carry none and stay [`Ignored`](Self::Ignored)). This drives
    /// chunk-cache-center/view-streaming updates (needs only `x`/`z`) and
    /// [`crate::fall::FallTracker`] (issue #265, needs `y`/`on_ground`) — the
    /// loop still needs no look data, but `on_ground` is no longer dropped:
    /// a landing that happens to arrive via a rotation-only or status-only
    /// packet (no net position change in that sample) is not observed here,
    /// a known gap noted on [`FallTracker`]'s own doc comment.
    PlayerMoved {
        /// New absolute x position, in blocks.
        x: f64,
        /// New absolute y position (feet), in blocks.
        y: f64,
        /// New absolute z position, in blocks.
        z: f64,
        /// Whether the client reports itself as grounded in this sample.
        on_ground: bool,
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
    /// The client requested a difficulty change
    /// (`ServerboundChangeDifficultyPacket`, issue #268). This crate has no
    /// permission/operator model, so `crate::server`'s consumer always
    /// accepts it — see that consumer's own doc comment for the vanilla
    /// permission check this replaces and why.
    DifficultyChanged {
        /// The requested difficulty.
        difficulty: Difficulty,
    },
    /// The client requested locking/unlocking difficulty
    /// (`ServerboundLockDifficultyPacket`, issue #268).
    DifficultyLockChanged {
        /// Whether difficulty should now be locked (further
        /// [`DifficultyChanged`](Self::DifficultyChanged) requests still
        /// decode and update the tracked value — vanilla does not reject a
        /// change while locked at the packet layer either; the lock is a UI
        /// affordance in the vanilla client, not a server-side veto).
        locked: bool,
    },
    /// The client requested one or more game-rule value changes
    /// (`ServerboundSetGameRulePacket`, issue #268). Each entry is `(rule
    /// key, raw string value)`, exactly as sent — this crate has no
    /// `GameRules` registry to validate a key or parse a value's real type
    /// against, so nothing here rejects an unknown key or a malformed value
    /// (vanilla itself just logs a warning and skips the entry; see
    /// `crate::server`'s consumer).
    GameRuleChanged {
        /// `(rule key, raw value)` pairs, in wire order.
        entries: Vec<(String, String)>,
    },
    /// The client selected a new hotbar slot
    /// (`ServerboundSetCarriedItemPacket`). Mirrors vanilla's
    /// `ServerGamePacketListenerImpl::handleSetCarriedItem`, which writes
    /// straight into `ServerPlayer.getInventory().setSelectedSlot(...)` with
    /// **no confirmation packet** — see `crate::inventory::PlayerInventory
    /// ::set_selected_hotbar_slot`'s consumer in `crate::server` for why
    /// nothing is sent back here either.
    CarriedItemChanged {
        /// The newly selected hotbar slot. The protocol decoder validates
        /// `0..HOTBAR_SIZE` before producing this variant (mirroring
        /// vanilla's `Inventory.isHotbarSlot` guard,
        /// `Inventory.java:70-76`); an out-of-range wire value decodes to
        /// [`Ignored`](Self::Ignored) instead.
        slot: u8,
    },
    /// A container click the client has already predicted locally
    /// (`ServerboundContainerClickPacket`). `changed_slots` and
    /// `carried_item` are the client's own **post-click prediction** — the
    /// wire packet's `HashedStack` payloads, not raw button/slot input —
    /// because `lodestone-game`'s `click.rs` already computed the full
    /// `doClick` result before encoding this packet (issue #27,
    /// `docs/container-clicks.md`). See `crate::inventory`'s module doc
    /// comment and `crate::server`'s consumer for why this crate applies
    /// that diff directly against window `0` (the player's own inventory)
    /// rather than re-deriving vanilla's click state machine server-side —
    /// a deliberate, documented scope cut, not an oversight.
    ContainerClicked {
        /// The window the click targeted. Only window `0` (the player's own
        /// inventory) has a server-side model to apply into today; any
        /// other id is decoded but dropped by the consumer (see
        /// `crate::server`'s doc comment on that arm).
        window_id: i32,
        /// The client's menu state id at the time of the click. Decoded for
        /// parity with the wire packet; not yet validated against a
        /// server-tracked state id (this crate sends no
        /// `container_set_slot`/`container_set_content` for the player's own
        /// inventory to resync from, so there is nothing to validate against
        /// yet).
        state_id: i32,
        /// Every menu slot whose contents changed, with the new value —
        /// `None` clears the slot.
        changed_slots: Vec<(i32, Option<ItemStack>)>,
        /// The cursor ("carried") stack after the click. Decoded for parity
        /// with the wire packet but not yet acted on:
        /// [`PlayerInventory`](crate::inventory::PlayerInventory) has no
        /// cursor field, since nothing server-side reads one today (the same
        /// "decoded but not yet acted on" pattern `BlockAction::sequence`
        /// already established here).
        carried_item: Option<ItemStack>,
    },
    /// The client closed a container screen (`ServerboundContainerClosePacket`).
    /// `window_id` is the id the client had open — vanilla's
    /// `ServerPlayer::doCloseContainer` compares this against nothing at all
    /// (it just closes whatever `containerMenu` currently is); this crate's
    /// consumer instead compares it against the connection's own tracked open
    /// window before clearing it, so a stale close for an already-replaced
    /// window cannot clobber a newer one. See `crate::server`'s consumer.
    ContainerClosed {
        /// The window id the client reports closing.
        window_id: i32,
    },
    /// The client attacked an entity with its currently held item
    /// (`ServerboundAttackPacket`, issue #12). 26.2 split this out of the old
    /// combined interact packet — the wire body carries only the target
    /// entity id, no hand/location/secondary-action data (see this variant's
    /// consumer, `crate::server::apply_attack`, for the damage/knockback
    /// pipeline this drives). The generic `minecraft:interact` packet
    /// (`ServerboundInteractPacket`, used for right-click entity interactions
    /// like taming/feeding/mounting) is deliberately *not* given a variant
    /// here: this crate has no interaction model for any of those, so
    /// decoding it into a new `ServerBound` case with nothing to do with it
    /// would be exactly the manufactured decode-only island `CLAUDE.md`
    /// warns against — it stays [`Ignored`](Self::Ignored) via the wildcard
    /// decode arm, the same treatment `BlockAction`'s own doc comment
    /// documents for the item-action ordinals this crate has no inventory
    /// model to act on either.
    Attack {
        /// Target entity id.
        entity_id: i32,
    },
    /// The client's movement-input flags for the current tick
    /// (`ServerboundPlayerInputPacket`, issue #12). Decoded for exactly one
    /// reason: `sprint` is half of vanilla's melee knockback-bonus gate
    /// (`Player.attack`'s `isSprinting() && fullStrengthAttack` — see
    /// `crate::server::apply_attack`'s own doc comment for the other half,
    /// which this crate cannot track). The other six flags
    /// (forward/backward/left/right/jump/shift) are decoded off the wire by
    /// [`ServerProtocol::decode`] but not threaded through here — nothing in
    /// this crate's server-authoritative model needs them yet, the same
    /// "decode what the loop needs, not the whole packet" convention
    /// [`PlayerMoved`](Self::PlayerMoved)'s own doc comment already
    /// establishes for its two fields.
    PlayerInput {
        /// Whether the client reports itself as sprinting this tick.
        sprint: bool,
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

    /// Encodes an air-supply update for the local player (vanilla's entity
    /// metadata `DATA_AIR_SUPPLY_ID`, sent over `SET_ENTITY_DATA` — see
    /// `crates/protocol/v770/src/packets/metadata.rs`'s `IDX_AIR_SUPPLY`,
    /// the decode side this mirrors). `air` is the new value, `-20..=300`
    /// (never sent negative on the wire in practice — `crate::vitals`
    /// resets to `0` the same tick air crosses the drowning threshold, and
    /// [`PlayerVitals::tick`](crate::PlayerVitals::tick) reports that via
    /// [`VitalsTick::air_changed`](crate::VitalsTick::air_changed) alongside
    /// the reset, not the transient negative value). The default emits
    /// nothing, so a protocol without air-supply support need not override
    /// it and drowning simply never reaches that client's HUD.
    fn encode_air_supply_update(&self, air: i32) -> ServerDirective {
        let _ = air;
        ServerDirective::None
    }

    /// Encodes a health update for the local player (vanilla's
    /// `ClientboundSetHealthPacket`, the same packet
    /// [`begin_play`](Self::begin_play) sends once at join with the
    /// fresh-spawn default). Sent whenever
    /// [`PlayerVitals::tick`](crate::PlayerVitals::tick) reports damage —
    /// currently only drowning. This crate tracks no food/hunger, so an implementor that
    /// reuses vanilla's combined health/food/saturation packet must supply
    /// its own constant food/saturation (see
    /// `V770ServerProtocol::encode_set_health` for the value it picks and
    /// why). The default emits nothing.
    fn encode_set_health(&self, health: f32) -> ServerDirective {
        let _ = health;
        ServerDirective::None
    }

    /// Encodes a difficulty confirmation (vanilla
    /// `ClientboundChangeDifficultyPacket`, wire id `change_difficulty`),
    /// sent back to the requesting connection after
    /// [`ServerBound::DifficultyChanged`]/[`DifficultyLockChanged`](ServerBound::DifficultyLockChanged)
    /// (issue #268). `locked` is always the connection's *current* lock
    /// state, not necessarily what this particular request changed — see
    /// `crate::server`'s consumer, which always passes both fields together
    /// regardless of which of the two `ServerBound` variants triggered the
    /// call. The default emits nothing.
    fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
        let _ = (difficulty, locked);
        ServerDirective::None
    }

    /// Encodes a game-rule confirmation (vanilla
    /// `ClientboundGameRuleValuesPacket`, wire id `game_rule_values`) for
    /// exactly the entries a [`ServerBound::GameRuleChanged`] request just
    /// set (issue #268) — not vanilla's full current-rule-table broadcast,
    /// since this crate models no default rule set to broadcast the rest of;
    /// see `crate::server`'s consumer for the full scope note. The default
    /// emits nothing.
    fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
        let _ = entries;
        ServerDirective::None
    }

    /// Opens a container's screen on the client (vanilla
    /// `ClientboundOpenScreenPacket`, sent by `ServerPlayer::openMenu`).
    /// `window_id` is the container id every subsequent `container_click`/
    /// `container_close` for this window will carry (vanilla's
    /// `nextContainerCounter`: `1..=100`, wrapping — see `crate::server`'s
    /// consumer for why this crate mirrors that exact scheme rather than a
    /// plain counter). `menu` is the vanilla `minecraft:*` menu identifier
    /// (e.g. `"minecraft:furnace"`); `title` is the screen's display name
    /// (this crate sends a plain literal string rather than vanilla's
    /// translatable `container.furnace`-style component — cosmetic only, see
    /// `crate::server`'s consumer for the fixed title table). The default
    /// emits nothing, so a protocol without container support need not
    /// override it.
    fn encode_open_screen(&self, window_id: i32, menu: &str, title: &str) -> ServerDirective {
        let _ = (window_id, menu, title);
        ServerDirective::None
    }

    /// Encodes the clientbound `container_set_content` packet: every slot in
    /// `items`, in vanilla menu order (the container's own slots first, then
    /// the player's standard 27-main + 9-hotbar inventory rows every such
    /// menu appends — never armour/off-hand, which only the player's own
    /// window `0` exposes), plus the cursor/carried stack. `state_id` is
    /// vanilla's `AbstractContainerMenu.stateId` at the time of the send —
    /// this crate does not validate a click's echoed value against it (see
    /// `docs/server-inventory.md`'s existing scope note for window `0`,
    /// which now applies identically to any other window). The default
    /// emits nothing.
    fn encode_container_content(
        &self,
        window_id: i32,
        state_id: i32,
        items: &[Option<ItemStack>],
        carried: Option<&ItemStack>,
    ) -> ServerDirective {
        let _ = (window_id, state_id, items, carried);
        ServerDirective::None
    }

    /// Encodes the clientbound `container_set_slot` packet for exactly one
    /// changed slot (vanilla `ClientboundContainerSetSlotPacket`), in the
    /// same menu-slot numbering [`encode_container_content`](Self::encode_container_content)
    /// uses. The default emits nothing.
    fn encode_container_slot(
        &self,
        window_id: i32,
        state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        let _ = (window_id, state_id, slot, item);
        ServerDirective::None
    }

    /// Encodes the clientbound `container_set_data` packet for one changed
    /// menu-local property (vanilla's `ContainerData`, e.g. a furnace's four
    /// burn/cook timers — see `crate::furnace::Furnace::container_data`'s own
    /// doc comment for the index table this feeds). Unlike a slot change,
    /// vanilla does not bump the container's `stateId` for a data change
    /// (`AbstractContainerMenu::broadcastChanges` calls `setData` directly,
    /// never `incrementStateId`), so this carries no `state_id` parameter at
    /// all. The default emits nothing.
    fn encode_container_data(&self, window_id: i32, property: i32, value: i32) -> ServerDirective {
        let _ = (window_id, property, value);
        ServerDirective::None
    }
}

/// Forwards every method to the boxed implementor, so a **trait object** can be
/// handed to the parts of this crate that take `P: ServerProtocol` by value —
/// [`IntegratedServer::open_in_memory`](crate::IntegratedServer::open_in_memory)
/// and [`bind`](crate::IntegratedServer::bind).
///
/// This is what makes the version seam work in the *serverbound* direction. The
/// clientbound side hands out `Box<dyn VersionAdapter>` from
/// `lodestone_registry::adapter_for_protocol`, and its serverbound twin
/// (`lodestone_registry::server_protocol_for_protocol`) has to do the same: a
/// registry that resolves a protocol *number* cannot return a concrete type, so
/// the only thing it can return is a box. [`ServerProtocol`] is object-safe
/// already (every method takes `&self`, none is generic, none mentions `Self`),
/// but `Box<dyn ServerProtocol>` does not implement the trait for free — without
/// this impl the box could not be served, and the only way to start singleplayer
/// would be for the caller to name the concrete version type. Which is precisely
/// what the seam exists to forbid.
///
/// Written over `Box<P>` with `P: ?Sized` rather than over `Box<dyn ServerProtocol>`
/// so it also covers a boxed *concrete* protocol; `Box` is `#[fundamental]`, so
/// the impl is coherent here in the trait's own crate.
///
/// **When you add a method to [`ServerProtocol`], add its forward here.** A
/// defaulted method that is not forwarded is not a compile error — the box would
/// silently answer with the trait's default (usually
/// [`ServerDirective::None`]) instead of asking the real protocol, so a boxed
/// v770 would stop sending, say, keep-alives while a directly-owned one kept
/// working. That asymmetry is invisible to any test that uses one shape only.
impl<P: ServerProtocol + ?Sized> ServerProtocol for Box<P> {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        (**self).decode(state, packet_id, payload)
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        (**self).login_success(username, uuid)
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        (**self).begin_configuration()
    }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        (**self).begin_play(view_radius)
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        (**self).begin_chunk_batch()
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        (**self).encode_chunk(cx, cz, column)
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        (**self).end_chunk_batch(batch_size)
    }

    fn welcome_message(&self) -> Vec<ServerDirective> {
        (**self).welcome_message()
    }

    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        (**self).encode_add_entity(entity)
    }

    fn encode_entity_update(
        &self,
        prev: Option<&EntitySnapshot>,
        current: &EntitySnapshot,
    ) -> Vec<ServerDirective> {
        (**self).encode_entity_update(prev, current)
    }

    fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
        (**self).encode_remove_entity(ids)
    }

    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        (**self).encode_keep_alive(id)
    }

    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        (**self).encode_set_time(game_time, day_time)
    }

    fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
        (**self).encode_chunk_cache_center(cx, cz)
    }

    fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
        (**self).encode_forget_chunk(cx, cz)
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        (**self).encode_block_update(x, y, z, state)
    }

    fn encode_air_supply_update(&self, air: i32) -> ServerDirective {
        (**self).encode_air_supply_update(air)
    }

    fn encode_set_health(&self, health: f32) -> ServerDirective {
        (**self).encode_set_health(health)
    }

    fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
        (**self).encode_change_difficulty(difficulty, locked)
    }

    fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
        (**self).encode_game_rule_values(entries)
    }

    fn encode_open_screen(&self, window_id: i32, menu: &str, title: &str) -> ServerDirective {
        (**self).encode_open_screen(window_id, menu, title)
    }

    fn encode_container_content(
        &self,
        window_id: i32,
        state_id: i32,
        items: &[Option<ItemStack>],
        carried: Option<&ItemStack>,
    ) -> ServerDirective {
        (**self).encode_container_content(window_id, state_id, items, carried)
    }

    fn encode_container_slot(
        &self,
        window_id: i32,
        state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        (**self).encode_container_slot(window_id, state_id, slot, item)
    }

    fn encode_container_data(&self, window_id: i32, property: i32, value: i32) -> ServerDirective {
        (**self).encode_container_data(window_id, property, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A protocol whose every method answers with a *distinct* directive, so
    /// "the box forwarded" and "the box fell back to the trait default" are
    /// distinguishable answers rather than both being `None`/empty.
    #[derive(Debug)]
    struct Numbered;

    /// One directive per method, numbered so a mixed-up forward is visible too.
    fn send(id: i32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: id,
            payload: Vec::new(),
        }
    }

    impl ServerProtocol for Numbered {
        fn decode(&self, _state: State, packet_id: i32, _payload: &[u8]) -> ServerBound {
            // Echoes the id back through a variant that carries one, so the
            // arguments are proven to survive the forward, not just the call.
            ServerBound::KeepAlive {
                id: i64::from(packet_id),
            }
        }
        fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
            vec![send(2)]
        }
        fn begin_configuration(&self) -> Vec<ServerDirective> {
            vec![send(3)]
        }
        fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
            vec![send(100 + view_radius)]
        }
        fn begin_chunk_batch(&self) -> ServerDirective {
            send(5)
        }
        fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
            send(cx * 1000 + cz)
        }
        fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
            send(200 + batch_size)
        }
        fn welcome_message(&self) -> Vec<ServerDirective> {
            vec![send(8)]
        }
        fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
            send(entity.id)
        }
        fn encode_entity_update(
            &self,
            _prev: Option<&EntitySnapshot>,
            current: &EntitySnapshot,
        ) -> Vec<ServerDirective> {
            vec![send(current.id + 1)]
        }
        fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
            send(ids.len() as i32)
        }
        fn encode_keep_alive(&self, id: i64) -> ServerDirective {
            send(id as i32)
        }
        fn encode_set_time(&self, game_time: i64, _day_time: Option<i64>) -> ServerDirective {
            send(game_time as i32)
        }
        fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
            send(cx * 10 + cz)
        }
        fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
            send(cx * 100 + cz)
        }
        fn encode_block_update(&self, x: i32, y: i32, z: i32, _state: &str) -> ServerDirective {
            send(x + y + z)
        }
        fn encode_air_supply_update(&self, air: i32) -> ServerDirective {
            send(air)
        }
        fn encode_set_health(&self, health: f32) -> ServerDirective {
            send(health as i32)
        }
        fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
            send(difficulty as i32 * 10 + i32::from(locked))
        }
        fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
            send(entries.len() as i32)
        }
        fn encode_open_screen(&self, window_id: i32, _menu: &str, _title: &str) -> ServerDirective {
            send(300 + window_id)
        }
        fn encode_container_content(
            &self,
            window_id: i32,
            state_id: i32,
            items: &[Option<ItemStack>],
            _carried: Option<&ItemStack>,
        ) -> ServerDirective {
            send(400 + window_id * 10 + state_id + items.len() as i32)
        }
        fn encode_container_slot(
            &self,
            window_id: i32,
            state_id: i32,
            slot: i32,
            _item: Option<&ItemStack>,
        ) -> ServerDirective {
            send(500 + window_id * 100 + state_id * 10 + slot)
        }
        fn encode_container_data(&self, window_id: i32, property: i32, value: i32) -> ServerDirective {
            send(600 + window_id * 100 + property * 10 + value)
        }
    }

    fn snapshot(id: i32) -> EntitySnapshot {
        EntitySnapshot {
            id,
            uuid: Uuid::nil(),
            entity_type: ResourceKey::new("minecraft", "pig").expect("static key is valid"),
            position: Vec3::new(0.0, 0.0, 0.0),
            rotation: Rotation { yaw: 0.0, pitch: 0.0 },
            head_yaw: 0.0,
            velocity: Vec3::new(0.0, 0.0, 0.0),
        }
    }

    /// Every [`ServerProtocol`] method must answer identically through a
    /// `Box<dyn ServerProtocol>` and through the concrete value.
    ///
    /// This is the control for the forwarding impl above, and the reason it is
    /// worth writing is that **fifteen of the twenty methods have defaults**:
    /// forgetting to forward one is not a compile error, it silently answers
    /// `ServerDirective::None`. That failure only ever shows up in a boxed
    /// server — i.e. only in singleplayer, which is exactly the path with no
    /// live oracle to catch it.
    #[test]
    fn a_boxed_protocol_answers_exactly_as_the_concrete_one_does() {
        let direct = Numbered;
        let boxed: Box<dyn ServerProtocol> = Box::new(Numbered);
        let column = ChunkColumn::new(-64, 384);
        let entity = snapshot(77);

        assert_eq!(
            boxed.decode(State::Play, 42, &[]),
            direct.decode(State::Play, 42, &[])
        );
        assert_eq!(
            boxed.login_success("a", Uuid::nil()),
            direct.login_success("a", Uuid::nil())
        );
        assert_eq!(boxed.begin_configuration(), direct.begin_configuration());
        assert_eq!(boxed.begin_play(7), direct.begin_play(7));
        assert_eq!(boxed.begin_chunk_batch(), direct.begin_chunk_batch());
        assert_eq!(
            boxed.encode_chunk(3, 4, &column),
            direct.encode_chunk(3, 4, &column)
        );
        assert_eq!(boxed.end_chunk_batch(9), direct.end_chunk_batch(9));
        assert_eq!(boxed.welcome_message(), direct.welcome_message());
        assert_eq!(
            boxed.encode_add_entity(&entity),
            direct.encode_add_entity(&entity)
        );
        assert_eq!(
            boxed.encode_entity_update(None, &entity),
            direct.encode_entity_update(None, &entity)
        );
        assert_eq!(
            boxed.encode_remove_entity(&[1, 2, 3]),
            direct.encode_remove_entity(&[1, 2, 3])
        );
        assert_eq!(boxed.encode_keep_alive(11), direct.encode_keep_alive(11));
        assert_eq!(
            boxed.encode_set_time(13, Some(1)),
            direct.encode_set_time(13, Some(1))
        );
        assert_eq!(
            boxed.encode_chunk_cache_center(2, 5),
            direct.encode_chunk_cache_center(2, 5)
        );
        assert_eq!(
            boxed.encode_forget_chunk(2, 5),
            direct.encode_forget_chunk(2, 5)
        );
        assert_eq!(
            boxed.encode_block_update(1, 2, 3, "minecraft:stone"),
            direct.encode_block_update(1, 2, 3, "minecraft:stone")
        );
        assert_eq!(
            boxed.encode_air_supply_update(19),
            direct.encode_air_supply_update(19)
        );
        assert_eq!(boxed.encode_set_health(4.0), direct.encode_set_health(4.0));
        assert_eq!(
            boxed.encode_change_difficulty(Difficulty::Hard, true),
            direct.encode_change_difficulty(Difficulty::Hard, true)
        );
        let rules = [("doDaylightCycle".to_string(), "false".to_string())];
        assert_eq!(
            boxed.encode_game_rule_values(&rules),
            direct.encode_game_rule_values(&rules)
        );
        assert_eq!(
            boxed.encode_open_screen(7, "minecraft:furnace", "Furnace"),
            direct.encode_open_screen(7, "minecraft:furnace", "Furnace")
        );
        let items = [None, Some(ItemStack::new(
            ResourceKey::new("minecraft", "coal").expect("static key is valid"),
            1,
        ))];
        assert_eq!(
            boxed.encode_container_content(7, 1, &items, None),
            direct.encode_container_content(7, 1, &items, None)
        );
        assert_eq!(
            boxed.encode_container_slot(7, 1, 2, items[1].as_ref()),
            direct.encode_container_slot(7, 1, 2, items[1].as_ref())
        );
        assert_eq!(
            boxed.encode_container_data(7, 0, 42),
            direct.encode_container_data(7, 0, 42)
        );

        // -- control ---------------------------------------------------------
        // Every assertion above compares two answers, so it would also pass if
        // *both* sides were the trait default. This is the premise that says
        // they are not: the spy's answers differ from what an unforwarded
        // defaulted method would produce.
        assert_ne!(direct.encode_keep_alive(11), ServerDirective::None);
        assert_ne!(direct.welcome_message(), Vec::<ServerDirective>::new());
        assert_ne!(
            direct.encode_change_difficulty(Difficulty::Hard, true),
            ServerDirective::None
        );
        assert_ne!(direct.encode_game_rule_values(&rules), ServerDirective::None);
        assert_ne!(
            direct.encode_open_screen(7, "minecraft:furnace", "Furnace"),
            ServerDirective::None
        );
        assert_ne!(
            direct.encode_container_content(7, 1, &items, None),
            ServerDirective::None
        );
        assert_ne!(
            direct.encode_container_slot(7, 1, 2, items[1].as_ref()),
            ServerDirective::None
        );
        assert_ne!(
            direct.encode_container_data(7, 0, 42),
            ServerDirective::None
        );
    }
}
