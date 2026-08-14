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
use lodestone_model::command_tree::CommandTree;
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, Difficulty, GameMode, ItemStack, ResourceKey, Rotation,
    SoundCategory, Text, Vec3, Vec3f,
};
use uuid::Uuid;

use crate::chunk::ChunkColumn;

/// The `EntityEvent` constants this crate sends through
/// [`ServerProtocol::encode_entity_event`], transcribed from
/// `net.minecraft.world.entity.EntityEvent`'s own `public static final byte`
/// declarations.
///
/// A module rather than a Rust `enum`, because the wire field is an arbitrary
/// byte whose meaning is **per entity type**: vanilla reuses values across
/// classes (`ClientboundEntityEventPacket` carries no type tag), so an
/// exhaustive enum would claim a closed set that does not exist. Naming only
/// the values with a producer here keeps the set honest, and the constant name
/// keeps the number out of the call site.
pub mod entity_event {
    /// `EntityEvent.DEATH` — `LivingEntity.die`'s broadcast. The client's
    /// `LivingEntity.handleEntityEvent` `case 3` starts `deathTime`, which is
    /// what tips a mob onto its side and holds the death screen's red overlay.
    pub const DEATH: u8 = 3;

    /// `EntityEvent.TAMING_FAILED` — the smoke puff of a failed tame roll
    /// (`TamableAnimal.spawnTamingParticles(false)`).
    pub const TAMING_FAILED: u8 = 6;

    /// `EntityEvent.TAMING_SUCCEEDED` — the hearts of a successful tame
    /// (`TamableAnimal.spawnTamingParticles(true)`).
    pub const TAMING_SUCCEEDED: u8 = 7;

    /// `EntityEvent.IN_LOVE_HEARTS` — the breeding hearts an `Animal` in love
    /// emits. **Not** `LOVE_HEARTS` (12), which is the villager's.
    pub const IN_LOVE_HEARTS: u8 = 18;
}

/// The local player's movement abilities — vanilla's `Player.Abilities`, as
/// carried by `ClientboundPlayerAbilitiesPacket`.
///
/// This is the packet that actually grants creative flight and instant build.
/// A client told "you are in creative" through
/// [`ServerProtocol::encode_game_mode`] alone still cannot fly, because
/// permission lives here — which is why `ServerPlayer.setGameMode` sends both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Abilities {
    /// Takes no damage.
    pub invulnerable: bool,
    /// Currently flying (as opposed to merely permitted to).
    pub flying: bool,
    /// Permitted to fly.
    pub may_fly: bool,
    /// Breaks blocks instantly and has infinite materials.
    pub instabuild: bool,
    /// Permitted to place and break at all — `false` only in adventure and
    /// spectator (`GameType.isBlockPlacingRestricted`).
    pub may_build: bool,
    /// Flight speed multiplier; vanilla's `Abilities` default is `0.05`.
    pub flying_speed: f32,
    /// Walk speed multiplier; vanilla's `Abilities` default is `0.1`.
    pub walking_speed: f32,
}

impl Abilities {
    /// Vanilla's `Abilities` field defaults (`Abilities.java`).
    pub const DEFAULT_FLYING_SPEED: f32 = 0.05;
    /// See [`DEFAULT_FLYING_SPEED`](Self::DEFAULT_FLYING_SPEED).
    pub const DEFAULT_WALKING_SPEED: f32 = 0.1;

    /// `GameType.updatePlayerAbilities` (`GameType.java:62-80`), verbatim:
    /// creative may fly and instabuilds and is invulnerable; spectator may fly,
    /// *is* flying and is invulnerable but does not instabuild; survival and
    /// adventure get none of it. `may_build` is `!isBlockPlacingRestricted()`,
    /// which is false for adventure and spectator.
    #[must_use]
    pub fn for_mode(mode: GameMode) -> Self {
        let (may_fly, instabuild, invulnerable, flying) = match mode {
            GameMode::Creative => (true, true, true, false),
            GameMode::Spectator => (true, false, true, true),
            GameMode::Survival | GameMode::Adventure => (false, false, false, false),
        };
        Self {
            invulnerable,
            flying,
            may_fly,
            instabuild,
            may_build: matches!(mode, GameMode::Survival | GameMode::Creative),
            flying_speed: Self::DEFAULT_FLYING_SPEED,
            walking_speed: Self::DEFAULT_WALKING_SPEED,
        }
    }
}

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
    /// Per-species entity-metadata fields this entity currently wants a
    /// client to hold (issue #425) — empty for every entity kind that has
    /// none (projectiles, dropped items, and any mob whose fields are all
    /// still at their default). [`crate::server::EntityStreamer::sync`]
    /// diffs this exactly like every other field on this struct: a spawn
    /// with non-empty metadata, or an update where this changed, calls
    /// [`ServerProtocol::encode_set_entity_data`] with the entity's *current*
    /// full field list (not just what changed) — see that method's own doc
    /// comment for why resending the full set is the simpler and cheap
    /// choice here.
    pub metadata: Vec<MetadataField>,
    /// The `ADD_ENTITY` **Object Data** field — vanilla's own name for the
    /// trailing VarInt on the spawn packet, whose meaning is decided entirely by
    /// the entity type.
    ///
    /// `0` for everything that does not override
    /// `Entity.getAddEntityPacket`/`ClientboundAddEntityPacket`'s data argument,
    /// which is every entity kind this server spawns except one:
    /// `FallingBlockEntity.getAddEntityPacket` passes
    /// `Block.getId(this.getBlockState())`.
    ///
    /// This is a **spawn-only** field and deliberately not part of the update
    /// path: vanilla sends it once, in `ADD_ENTITY`, and has no packet that
    /// revises it. It is still compared by this struct's `PartialEq`, so a value
    /// that somehow changed mid-life would produce a redundant position update
    /// rather than silently disagreeing with what the client holds.
    ///
    /// # Why the block state cannot ride `metadata` instead
    ///
    /// `FallingBlockEntity.defineSynchedData` registers `DATA_START_POS` and
    /// nothing else — the imitated block state is **never** in a `SET_ENTITY_DATA`
    /// packet. So a client that is not told this field has no other source, and
    /// draws whatever state id `0` resolves to. That is the same failure shape as
    /// a dropped item with no reported stack: every wire green, the wrong value
    /// travelling it.
    pub object_data: i32,
}

/// One connected player as the tab list carries them (issue #438) — the
/// version-free vocabulary
/// [`ServerProtocol::encode_player_info_add`] takes a slice of.
///
/// Only the two fields `ADD_PLAYER` cannot do without. Vanilla's own
/// `ClientboundPlayerInfoUpdatePacket.Entry` also carries a game mode, a
/// latency, a `listed` flag, a display-name component, a chat session and a
/// list-order — each behind its own action bit. None of those has a
/// server-side source of truth in this crate yet (there is no per-connection
/// game mode, no measured latency, no scoreboard), so rather than invent
/// plausible values here the implementor supplies the defaults vanilla itself
/// uses for a fresh join; see the `v770` implementation's own doc comment for
/// which bits it sets and why.
///
/// Not `Copy`: `username` is owned, because the registry that produces these
/// (`crate::players::PlayerRegistry`) holds the string and a borrow would tie
/// every reader to its lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerListing {
    /// The player's profile uuid — the key the client stores the entry under,
    /// and the same uuid their entity's `ADD_ENTITY` carries. These two
    /// **must** agree: the client resolves the spawn by looking the uuid up in
    /// this map (see [`ServerProtocol::encode_player_info_add`]).
    pub uuid: Uuid,
    /// The player's username.
    pub username: String,
}

/// A server-initiated resource pack push (vanilla
/// `ClientboundResourcePackPushPacket`) in version-free vocabulary — the
/// server side of issue #334, fed by
/// [`ServerProtocol::encode_resource_pack_push`].
///
/// Mirrors the wire record exactly: a fresh per-push [`Uuid`] the client
/// echoes back verbatim in its accept/decline response, the download [`url`],
/// the pack's SHA-1 [`hash`] (lowercase hex, at most 40 chars — vanilla's
/// `ClientboundResourcePackPushPacket.MAX_HASH_LENGTH`), the [`required`]
/// flag that makes declining a disconnect, and an optional [`prompt`] chat
/// component shown on the accept/decline screen. `url` and `hash` are owned
/// `String`s (not borrowed) so a push can outlive its construction site and
/// ride a feed across a task boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourcePackPush {
    /// The push's own uuid — vanilla generates a fresh one per push, and the
    /// client's response echoes it (see
    /// `crate::server`'s decode of the serverbound `RESOURCE_PACK` frame).
    pub id: Uuid,
    /// The pack's download URL.
    pub url: String,
    /// The SHA-1 hash of the pack, lowercase hex, at most 40 characters
    /// (may be empty if the pack does not declare one).
    pub hash: String,
    /// Whether the client must accept the pack to keep playing — a declined
    /// or failed download disconnects it.
    pub required: bool,
    /// An optional prompt component shown on the accept/decline screen.
    pub prompt: Option<Text>,
}

/// One per-species entity-metadata field a [`ServerProtocol`] can push over
/// `SET_ENTITY_DATA` (issue #425) — the general vocabulary
/// [`ServerProtocol::encode_set_entity_data`] takes a slice of, replacing
/// the single hardcoded local-player arm (`encode_air_supply_update`) that
/// used to be the only metadata encoder anywhere in this crate. Adding a
/// field for the next mob is a new variant here plus one arm in the
/// implementor's `encode_set_entity_data` — no second mechanism, and no
/// change to [`EntityStreamer::sync`](crate::server) at all, since that
/// diffing loop already treats `EntitySnapshot::metadata` generically.
///
/// Each variant names the vanilla field it mirrors, not the wire index or
/// serializer id — those are the implementor's concern (see
/// `crates/protocol/v770/src/server_protocol.rs`'s own constants, verified
/// against the `EntityDataIndexOracle` dump the same way
/// `crates/protocol/v770/src/packets/metadata.rs`'s decode-side constants
/// already are), matching every other version-free `Server*`/`Client*`
/// vocabulary type in this crate.
/// # Why this enum is deliberately **not** `Copy`
///
/// It derived `Copy` until issue #537, which is the first field whose value is
/// an owned one ([`Item`](Self::Item) carries a [`ResourceKey`]). A version-free
/// vocabulary enum that derives `Copy` silently forbids every future field that
/// carries an owned value, and the cost surfaces only at the first feature that
/// needs one — here, the whole of "a dropped item draws at all". Keep it
/// non-`Copy`: the only cost is that an implementor's `match` is by reference
/// (`match field`, not `match *field`), and that is one character per
/// implementor. See DESIGN.md §12.116.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataField {
    /// `Creeper.DATA_SWELL_DIR` — which way `swell` is currently moving
    /// (`-1`, `0`, or `1`). See [`crate::mobs::SimMob::snapshot`]'s own doc
    /// comment for why this is always included for a creeper, even at its
    /// `-1` default, unlike the monotonic [`CreeperIgnited`](Self::CreeperIgnited).
    CreeperSwellDir(i32),
    /// `Creeper.DATA_IS_IGNITED` — set once by `ignite()`, never cleared.
    CreeperIgnited(bool),
    /// `ItemEntity.DATA_ITEM` — the stack a dropped item entity is showing,
    /// and the *whole* of its visible identity.
    ///
    /// A client draws nothing for an item entity whose stack it has not been
    /// told: vanilla's `ItemEntityRenderer.submit` returns early on
    /// `state.item.isEmpty()`, and this project's own client does the same (see
    /// `EntityInterpolator::set_item_stack`). So an item entity streamed
    /// without this field spawns, falls, merges and can be picked up — every
    /// one of which is observable — while drawing zero pixels. That is why it
    /// is one field and not an optimisation.
    ///
    /// `count` is the *entity's* stack size (vanilla's
    /// `ItemLifecycle::count`), not the number of entities.
    Item {
        /// The item's registry key, e.g. `minecraft:diamond`. **Not** an
        /// entity type — see [`crate::mobs::MobSim::snapshots`] for the
        /// `minecraft:acacia_boat` bug that confusing the two produced.
        item: ResourceKey,
        /// Stack size. `0` is the empty stack, which a client renders as
        /// nothing — the same as sending no field at all.
        count: u8,
    },
    /// `ExperienceOrb.DATA_VALUE` — the points **one** absorption of this orb pays
    /// out, and the whole of what a client is told about an orb.
    ///
    /// It is what selects the sprite: `ExperienceOrb.getIcon` buckets the value into
    /// eleven frames at the same thresholds as the denomination ladder, so an orb whose
    /// value never arrives draws frame 0 — the smallest — however much it is worth. The
    /// orb's `count` (how many absorptions it holds after merging) is deliberately not
    /// here, because vanilla does not synchronise it and one entity draws one sprite
    /// whatever its count.
    ///
    /// **Index 8, shared with [`Item`](Self::Item) under a different serializer.**
    /// `DATA_VALUE` is an `INT` and `ItemEntity.DATA_ITEM` an `ITEM_STACK` at the same
    /// index; the encoder can tell them apart only because the field list is built per
    /// entity kind by [`crate::mobs::MobSim::snapshots`], whose orb loop iterates the
    /// orb map. Never push this variant for anything but an experience orb.
    ExperienceOrbValue {
        /// Points per absorption, vanilla's `getValue()`.
        value: i32,
    },
    /// `TamableAnimal.DATA_FLAGS_ID` — the wolf/cat/parrot flag byte at index
    /// **18**, whose `0x01` bit is the sitting pose and `0x04` bit is tameness
    /// (`TamableAnimal.isInSittingPose` is `& 1`, `isTame` is `& 4`).
    ///
    /// # Why this is not shared with [`HorseFlags`](Self::HorseFlags)
    ///
    /// Index 18 is the most crowded index in the game — 37 claimants in the
    /// committed jar dump (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`),
    /// of which **four** are the `BYTE` serializer: `TamableAnimal.DATA_FLAGS_ID`,
    /// `AbstractHorse.DATA_ID_FLAGS`, `Sheep.DATA_WOOL_ID` and
    /// `Shulker.DATA_COLOR_ID`. No census column separates them; the *producer*
    /// has to know the species, which is why the species switch lives in
    /// [`crate::mobs::SimMob::snapshot`] and never in an encoder.
    ///
    /// And the bit differs: tame is `0x04` here and `FLAG_TAME = 2` on a horse. A
    /// single shared variant therefore sets an **unnamed** bit on whichever species
    /// it was not written for — `0x04` is not in the horse's flag set at all
    /// (`FLAG_BRED` is `8`), and `0x02` is not in the tamable's — so the animal
    /// reads as *untamed* while the packet looks correct on the wire. That is worse
    /// than a wrong flag, because there is nothing visibly wrong to chase.
    TamableFlags {
        /// `0x04` — `TamableAnimal.isTame()`.
        tame: bool,
        /// `0x01` — `TamableAnimal.isInSittingPose()`, the pose
        /// `SitWhenOrderedToGoal` writes, **not** the persisted `orderedToSit`.
        sitting: bool,
    },
    /// `AbstractHorse.DATA_ID_FLAGS` — the horse family's own flag byte, also at
    /// index 18. See [`TamableFlags`](Self::TamableFlags) for why this is a
    /// separate variant.
    ///
    /// Only `FLAG_TAME` (`0x02`) is modelled. `FLAG_BRED` (`0x08`), `FLAG_EATING`
    /// (`0x10`), `FLAG_STANDING` (`0x20`) and `FLAG_OPEN_MOUTH` (`0x40`) have no
    /// server-side state to drive them yet — the values are transcribed from
    /// `AbstractHorse`'s own constants so the next one to be wired does not have to
    /// be looked up again.
    HorseFlags {
        /// `0x02` — `AbstractHorse.isTamed()`.
        tame: bool,
    },
    /// `AgeableMob.DATA_BABY_ID` — whether this mob is a baby. Vanilla's
    /// `LivingEntityRenderer` reads it to apply the age-scale shrink
    /// (`0.5` generic, or the species' real `BABY_DIMENSIONS` literal where
    /// one is modelled) to the model, independently of the hitbox — this
    /// crate's aging unit already computes the correct **hitbox** dimensions
    /// server-side (`crate::mobs::species_shape`); this variant is what lets
    /// the *client* apply the same shrink to what it draws. `AgeableMob` is
    /// the shared ancestor for the whole zombie family, cow, sheep, pig,
    /// chicken, rabbit and wolf.
    Baby(bool),
}

/// Which worldgen data bundle a [`ServerProtocol`]'s hosting needs (issue
/// #407) — the version gate between the worldgen data this crate embeds and
/// the protocol family being served.
///
/// The only bundle `lodestone-server` embeds is 26.2 (protocol 776): the
/// `assets/worldgen/` table [`crate::worldgen_data`] serves. A family whose
/// worldgen is **not** the embedded 26.2 bundle must say so and supply its own
/// data — per `docs/plans/worldgen-parity.md` §4 that will be a second engine
/// behind [`crate::ChunkSource`], not a second JSON bundle. The
/// [`None`](Self::None) report is what makes "no worldgen for this version"
/// surfaced rather than silently serving the wrong terrain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldgenScope {
    /// 26.2 (protocol 776) worldgen data — the one bundle this crate embeds.
    V26_2,
    /// No worldgen data: the protocol does not host world generation, or has
    /// not declared a bundle this crate can serve.
    None,
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
    /// The client asked for the server-list status (mirrors
    /// `ServerboundStatusRequestPacket`, whose body is *empty* —
    /// `StreamCodec.unit(INSTANCE)`,
    /// `net/minecraft/network/protocol/status/ServerboundStatusRequestPacket.java:10`).
    ///
    /// This is the very first thing a real client sends after a handshake whose
    /// `next_state` was Status, i.e. when a player adds our server to their
    /// multiplayer list. The loop answers with
    /// [`ServerProtocol::encode_status_response`].
    StatusRequest,
    /// The client asked us to echo a clock reading so it can compute latency
    /// (mirrors `ServerboundPingRequestPacket`: a single big-endian `long`,
    /// `net/minecraft/network/protocol/ping/ServerboundPingRequestPacket.java:19`).
    ///
    /// Sent in the Status phase immediately after
    /// [`ServerBound::StatusRequest`]; the loop answers with
    /// [`ServerProtocol::encode_pong_response`] carrying `time` unchanged and
    /// then terminates the connection, exactly as vanilla's own
    /// `ServerStatusPacketListenerImpl.handlePingRequest` does
    /// (`net/minecraft/server/network/ServerStatusPacketListenerImpl.java:44-47`).
    PingRequest {
        /// The client's local clock reading, echoed back verbatim. Vanilla
        /// treats this as opaque — it is the *client* that subtracts it from
        /// its own clock — so the server must not reinterpret or clamp it.
        time: i64,
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
    /// carry a position). This drives chunk-cache-center/view-streaming
    /// updates (needs only `x`/`z`) and [`crate::fall::FallTracker`]
    /// (issue #265, needs `y`/`on_ground`).
    ///
    /// `rotation` is `Some` only for `move_player_pos_rot`, which is the
    /// packet a client sends whenever position *and* look both changed in a
    /// tick — i.e. the overwhelmingly common case of a player walking while
    /// turning. It was decoded and discarded here until issue #262's wiring:
    /// a player who walks and turns never sends `move_player_rot` at all
    /// (vanilla's `LocalPlayer.sendPosition` picks exactly one of the four
    /// movement packets per tick), so handling only the rotation-*only*
    /// sibling would have left the common case frozen at yaw 0. `None` for
    /// `move_player_pos`, whose wire body genuinely has no angles — a
    /// distinction the consumer must keep, since "no angles in this sample"
    /// is not the same as "facing due south".
    PlayerMoved {
        /// New absolute x position, in blocks.
        x: f64,
        /// New absolute y position (feet), in blocks.
        y: f64,
        /// New absolute z position, in blocks.
        z: f64,
        /// New body/head rotation, when this sample carried one.
        rotation: Option<Rotation>,
        /// Whether the client reports itself as grounded in this sample.
        on_ground: bool,
    },
    /// The client's look changed but its position did not
    /// (`move_player_rot`), the packet a player standing still and turning on
    /// the spot sends every tick.
    ///
    /// Decoded-and-dropped until issue #262's wiring. It is the difference
    /// between an avatar that tracks where its player is looking and one that
    /// only ever re-aims when it also happens to be walking, so it is not
    /// redundant with [`PlayerMoved`](Self::PlayerMoved)'s `rotation`.
    PlayerRotated {
        /// New body/head yaw, in degrees.
        yaw: f32,
        /// New pitch, in degrees.
        pitch: f32,
        /// Whether the client reports itself as grounded in this sample.
        on_ground: bool,
    },
    /// Neither position nor look changed enough to be dirty, but the client's
    /// grounded/collision status flipped (`move_player_status_only`).
    ///
    /// Carries no pose data at all — the flags byte is the whole body. Its
    /// one consumer is [`crate::fall::FallTracker`]: this is the packet that
    /// reports the landing of a fall whose final sample had no net position
    /// change, the exact gap that type's own doc comment used to disclose.
    PlayerStatusOnly {
        /// Whether the client reports itself as grounded in this sample.
        on_ground: bool,
    },
    /// The player threw an item out of their hand — `Q` / `Ctrl+Q`, vanilla's
    /// `ServerboundPlayerActionPacket` ordinals `DROP_ITEM` (4) and
    /// `DROP_ALL_ITEMS` (3).
    ///
    /// **These used to decode to [`Ignored`](Self::Ignored)**, and the note on
    /// [`BlockAction`](Self::BlockAction) below said so — item handling was out of
    /// this crate's scope when that note was written. It no longer is (this crate
    /// owns [`PlayerInventory`](crate::PlayerInventory) and spawns item entities
    /// for block drops), so the ordinals now lift to their own variant. Pressing
    /// `Q` did nothing at all before, and no `_ =>` arm was to blame: the
    /// information was thrown away one layer earlier, at the decode.
    ItemDropped {
        /// `true` for `DROP_ALL_ITEMS` (`Ctrl+Q`, the whole selected stack),
        /// `false` for `DROP_ITEM` (one item) — vanilla's `all` argument to
        /// `Inventory.removeFromSelected`.
        whole_stack: bool,
    },
    /// A block-breaking phase (`ServerboundPlayerActionPacket`'s
    /// `START_DESTROY_BLOCK`/`ABORT_DESTROY_BLOCK`/`STOP_DESTROY_BLOCK`
    /// ordinals). The two drop ordinals share the same wire packet and lift to
    /// [`ItemDropped`](Self::ItemDropped); release-use, swap-with-offhand and
    /// stab still decode to [`Ignored`](Self::Ignored).
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
    /// The clicked block and face determine the placement cell (see
    /// `crate::server`'s handling); `cursor` is vanilla's
    /// `BlockHitResult.getLocation()` reduced to block-local coordinates, and
    /// is what decides a stair/slab/trapdoor's `half`.
    UseItemOn {
        /// The block face the client clicked.
        pos: BlockPos,
        /// Which face of `pos` was clicked.
        face: BlockFace,
        /// Block-local hit position within `pos`, each component `0.0`–`1.0`.
        /// `crate::block_placement` reads its `y` for the upper/lower-half
        /// decision every `Half`-bearing block makes.
        cursor: Vec3f,
        /// Client block-prediction sequence number (see
        /// [`BlockAction::sequence`](Self::BlockAction) for why it is
        /// decoded but not yet acted on).
        sequence: i32,
        /// `0` main hand, `1` off hand — vanilla's `InteractionHand.ordinal()`.
        /// `crate::server`'s `apply_use_item_on` reads this to resolve which
        /// native inventory slot the spawn-egg/flint-and-steel/placement
        /// branches act on; same convention as [`UseItem::hand`](Self::UseItem).
        hand: u8,
    },
    /// The client asked to change its own game mode
    /// (`ServerboundChangeGameModePacket` — the F4 switcher a
    /// singleplayer/LAN host with cheats sends).
    ///
    /// The server stays authoritative: this is a *request*, and
    /// `crate::server` answers it by echoing the mode it actually applied
    /// through [`ServerProtocol::encode_game_mode`] plus
    /// [`encode_player_abilities`](ServerProtocol::encode_player_abilities), so
    /// a client that guessed wrong is corrected rather than trusted.
    ChangeGameMode {
        /// The requested mode.
        mode: GameMode,
    },
    /// The client sent a player-command packet
    /// (`ServerboundPlayerCommandPacket`, issue #325). The packet's action
    /// ordinal is carried raw — the same shape `BlockAction` uses for its
    /// consumed ordinals — and only the one this crate has a consumer for,
    /// `STOP_SLEEPING` (`0`, the "wake up" a client sends when the player
    /// climbs out of bed or dies), is surfaced as a variant by the version
    /// crate's decoder; the other ordinals (sprinting/riding/jump states)
    /// decode to [`Ignored`](Self::Ignored).
    ///
    /// Note this packet deliberately carries **no** player identity: the wire
    /// `entityId` is always the sender's own local-player id (`1`), so the
    /// consumer must resolve who is waking up from the connection's own player
    /// id — see `crate::sleep::SleepVote` for why the key cannot come from the
    /// wire.
    PlayerCommand {
        /// The `ServerboundPlayerCommandPacket.Action` ordinal sent by the
        /// client.
        action: i32,
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
    /// A container click (`ServerboundContainerClickPacket`).
    ///
    /// **The button input is what the consumer acts on.** `slot`, `button` and
    /// `click_type` are the raw click; `crate::container_click::do_click`
    /// re-derives the whole menu state from them, exactly as vanilla's
    /// `AbstractContainerMenu.doClick` does. That replaces the earlier scope cut
    /// in which the client's own `changed_slots` prediction was applied verbatim
    /// — a hole through which any client could name any item in any slot.
    ///
    /// `changed_slots` and `carried_item` are still carried, because the wire
    /// packet has them and they are the client's post-click prediction (issue #27,
    /// `docs/container-clicks.md`): the consumer compares them against what it
    /// derived, purely to decide whether a correcting `container_set_content` is
    /// worth sending. Nothing is ever *stored* from them.
    ContainerClicked {
        /// The window the click targeted — `0` for the player's own inventory
        /// screen, otherwise the id the server handed out in `open_screen`.
        window_id: i32,
        /// The client's menu state id at the time of the click. Decoded for
        /// parity with the wire packet; not yet validated against the server's own
        /// (`OpenContainer::state_id`), which would let the server *reject* a click
        /// raced against a correction rather than merely overwrite its result.
        state_id: i32,
        /// The clicked menu slot. `-999`
        /// ([`SLOT_OUTSIDE`](crate::container_click::SLOT_OUTSIDE)) is vanilla's
        /// "outside the window", which drops the cursor into the world.
        slot: i32,
        /// `buttonNum`: the mouse button for a pickup/quick-move, the hotbar index
        /// (or `40` for the off-hand) for a swap, or the drag header/type mask for
        /// a quick-craft.
        button: i8,
        /// `ContainerInput`'s ordinal — `0` pickup, `1` quick-move, `2` swap,
        /// `3` clone, `4` throw, `5` quick-craft, `6` pickup-all.
        click_type: i32,
        /// The client's predicted per-slot result. **Never stored** — see this
        /// variant's own doc comment.
        changed_slots: Vec<(i32, Option<ItemStack>)>,
        /// The client's predicted cursor stack. **Never stored**, for the same
        /// reason; the server tracks its own cursor in
        /// [`ClickState`](crate::container_click::ClickState).
        carried_item: Option<ItemStack>,
    },
    /// The client clicked a recipe in the recipe book, asking the server to lay it
    /// out in the open crafting grid (`ServerboundPlaceRecipePacket`, issue #529
    /// step 4).
    ///
    /// **`recipe_index` is an opaque id the *server* assigns**, not a name: vanilla
    /// sends the whole book with `ClientboundRecipeBookAddPacket` and the client
    /// echoes back a position in that list. See
    /// [`crate::crafting::recipe_at_index`] for the id space this crate defines and
    /// for the consequence — nothing sends this packet until that clientbound half
    /// exists.
    RecipePlaced {
        /// The window the recipe should be laid into.
        window_id: i32,
        /// `RecipeDisplayId.index`.
        recipe_index: i32,
        /// `useMaxItems` — shift-clicking the recipe, which fills as many rounds as
        /// the inventory allows.
        use_max_items: bool,
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
    /// (`ServerboundInteractPacket`) is the *other* half and has its own
    /// variant, [`InteractEntity`](Self::InteractEntity): 26.2 split the old
    /// combined packet in two, and the two halves reach different consumers
    /// here — attack goes to the damage pipeline, interact to
    /// `crate::mobs::MobSim::interact`.
    Attack {
        /// Target entity id.
        entity_id: i32,
    },
    /// A player right-clicked an entity (`ServerboundInteractPacket`) — the
    /// taming, feeding, sitting and breeding trigger.
    ///
    /// `using_secondary_action` is the packet's trailing boolean (the shift
    /// modifier), carried rather than dropped because vanilla's own
    /// `mobInteract` chain consults it — `AbstractHorse.mobInteract`'s
    /// `isTamed() && player.isSecondaryUseActive()` opens the inventory instead
    /// of mounting. Nothing reads it yet; it is on the wire and dropping it
    /// would have to be undone.
    ///
    /// The low-precision `Vec3` location the packet also carries is **not**
    /// here: vanilla uses it only for the `INTERACT_AT` sub-action (clicking a
    /// specific part of an armour stand), which this crate has no model for.
    InteractEntity {
        /// Target entity id.
        entity_id: i32,
        /// `InteractionHand` ordinal: `0` = main hand, `1` = off hand.
        hand: i32,
        /// Whether the client was sneaking.
        using_secondary_action: bool,
    },
    /// The player began using the item in `hand` in mid-air
    /// (`ServerboundUseItemPacket`).
    ///
    /// This is the *start* of a use, not a completed action, and the difference
    /// matters: an instant throwable (snowball, egg, ender pearl) is released by
    /// vanilla's `use` the moment the packet arrives, while a bow starts a draw
    /// whose length the **server** counts and which ends with a separate
    /// [`ReleaseUseItem`](Self::ReleaseUseItem). One packet, two behaviours,
    /// decided by what is in the hand — see `crate::server`'s `apply_use_item`.
    ///
    /// `ServerboundUseItemPacket` also carries the client's yaw/pitch, which is
    /// what makes a launch direction available without this crate tracking
    /// rotation for every connection: a throw needs the facing *at the instant of
    /// the throw*, and the last `PlayerRotated` packet is not necessarily that.
    UseItem {
        /// `0` main hand, `1` off hand.
        hand: u8,
        /// Yaw in degrees, as the client reported it with the use.
        yaw: f32,
        /// Pitch in degrees.
        pitch: f32,
    },
    /// The player let go of a right-click they had been holding
    /// (`ServerboundPlayerActionPacket`'s `RELEASE_USE_ITEM` ordinal, `5`).
    ///
    /// Vanilla's bow fires from here, not from the `USE_ITEM` that started the
    /// draw, and the arrow's power comes from how long the two were apart —
    /// `BowItem.getPowerForTime`. That interval is counted in **server ticks**, so
    /// the consumer reads `MobSim::tick_count` rather than a wall clock: this crate
    /// links into a wasm32 bundle where `Instant::now()` compiles and then panics
    /// at runtime with no log line.
    ReleaseUseItem,
    /// The client reporting where the vehicle it rides has got to
    /// (`ServerboundMoveVehiclePacket`), once per tick while mounted.
    ///
    /// **This is not a request — it is authoritative.**
    /// `Entity.isClientAuthoritative()` delegates to the controlling passenger and
    /// `Player.isClientAuthoritative()` returns `true`, so the server's own
    /// `travelRidden` takes the `setDeltaMovement(Vec3.ZERO)` branch and its only
    /// job is to accept this and relay it. A server that also simulated the boat
    /// would fight the player.
    ///
    /// The packet carries no entity id: vanilla resolves the target as
    /// `player.getRootVehicle()` and rejects the packet outright when that is the
    /// player themselves. [`crate::mobs::MobSim::apply_vehicle_move`] is the
    /// consumer and applies the same rule, which is what stops a connection moving
    /// a boat it is not sitting in.
    ///
    /// The two rejections vanilla *can* answer with (moved too quickly, moved
    /// wrongly — both followed by `vehicle.absSnapTo(old…)` and a clientbound
    /// `MOVE_VEHICLE`) are not implemented, so no correction is ever sent. Stated
    /// because the client already handles one if it arrives
    /// (`lodestone_ecs::vehicle::apply_vehicle_moved`).
    VehicleMoved {
        /// The vehicle's position as the client simulated it.
        position: Vec3,
        /// Its yaw in degrees.
        yaw: f32,
        /// Its pitch in degrees. A boat never changes it; a land mount takes half
        /// its rider's, so it is carried rather than dropped.
        pitch: f32,
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
    /// A creative-mode inventory slot write predicted locally by the client
    /// (`ServerboundSetCreativeModeSlotPacket`, issue #266). Uses the exact
    /// same menu-slot numbering [`ContainerClicked`](Self::ContainerClicked)
    /// does — see
    /// [`PlayerInventory::apply_menu_slot_change`](crate::inventory::PlayerInventory::apply_menu_slot_change)'s
    /// own doc comment for the table — because vanilla's
    /// `handleSetCreativeModeSlot` writes through the identical
    /// `player.inventoryMenu.getSlot(slotNum)` indexing
    /// (`ServerGamePacketListenerImpl.java:2038`). This crate has no
    /// creative-mode/game-mode model to gate on (`hasInfiniteMaterials()` in
    /// vanilla), matching the permission-check omission
    /// [`DifficultyChanged`](Self::DifficultyChanged)'s own doc comment
    /// already documents for this crate's singleplayer-only shape.
    CreativeModeSlotSet {
        /// Wire slot index. Vanilla only ever writes for `1..=45`
        /// (`validSlot`, `ServerGamePacketListenerImpl.java:2035`); `0`
        /// (crafting output) and negative values (vanilla's "drop into the
        /// world" case, `packet.slotNum() < 0`) are decoded but never
        /// recognised by
        /// [`apply_menu_slot_change`](crate::inventory::PlayerInventory::apply_menu_slot_change) —
        /// this crate has no world-drop model, the same scope cut
        /// [`BlockAction`](Self::BlockAction)'s own doc comment already
        /// makes for the item-drop action ordinals.
        slot: i16,
        /// The item now in that slot, or `None` to clear it.
        item: Option<ItemStack>,
    },
    /// The client sent a `client_command`
    /// (`ServerboundClientCommandPacket`, issue #270). `action` is vanilla's
    /// `Action` ordinal, straight off the wire: `0` = perform respawn, `1` =
    /// request stats (no stats model exists in this crate — see
    /// `crate::server`'s consumer), `2` = request current game-rule values
    /// (mirrors `sendGameRuleValues`, answered from the same
    /// [`WorldAdminState`](crate::server) issue #268 already built).
    ClientCommand {
        /// Action ordinal, straight off the wire.
        action: i32,
    },
    /// The client changed a setting after joining
    /// (`ServerboundClientInformationPacket`, issue #270). Most fields are
    /// cosmetic (locale, chat visibility, skin parts, main hand) and this
    /// crate has nothing that reads any of them; `view_distance` is the one
    /// exception — the "server should honour view distance at minimum" case
    /// this issue's own decode-arm comment flags. Matches
    /// [`PlayerInput`](Self::PlayerInput)'s "decode what the loop needs, not
    /// the whole packet" convention.
    ClientInformationChanged {
        /// Requested render distance in chunks. Vanilla only ever sends
        /// `2..=32`; `crate::server`'s consumer clamps against the server's
        /// own configured view radius either way, so an out-of-range value
        /// degrades rather than misbehaves.
        view_distance: i8,
    },
    /// The client acknowledged one chunk batch
    /// (`ServerboundChunkBatchReceivedPacket`, issue #270) — vanilla's
    /// `PlayerChunkSender` flow control, which allows at most one
    /// unacknowledged batch in flight at a time
    /// (`ServerProtocol`'s own trait doc comment already states this
    /// contract for the *initial* join batch; this variant is what lets
    /// `crate::server` honour it for every later view-streaming batch too,
    /// closing the gap this issue's body names: "the server ... never reads
    /// this reply at all").
    ChunkBatchAcknowledged {
        /// The client's requested chunks-per-tick delivery rate. Decoded for
        /// parity with the wire packet but not yet used to pace *within* a
        /// batch — see `crate::server`'s consumer for the one invariant this
        /// crate does enforce (never starting a second batch before the
        /// first is acked) and this field's own future scope.
        desired_chunks_per_tick: f32,
    },
    /// The client ran a command (`ServerboundChatCommandPacket`, issues #48
    /// and #464).
    ///
    /// `command` is the text **without** its leading `/` — that is the wire
    /// format, not a normalisation we apply: vanilla's own packet carries it
    /// stripped (`crates/protocol/v770/src/packets/game.rs`'s `ChatCommand`
    /// documents the same layout from the client-encode side).
    ///
    /// This crate cannot execute it. The Brigadier registry plugins register
    /// into lives in `lodestone-ecs`, which this crate deliberately does not
    /// depend on, so `crate::server` hands this to the host through
    /// [`CommandDispatch`](crate::CommandDispatch) and turns the answer into
    /// [`encode_system_chat`](ServerProtocol::encode_system_chat) directives.
    /// See `crate::command`'s module doc for the whole argument, including
    /// why the two rejected alternatives were rejected.
    ///
    /// Only the *unsigned* `chat_command` produces this. `chat_command_signed`
    /// carries a signature block this crate has no session key to verify and
    /// stays [`Ignored`](Self::Ignored); a client only sends the signed form for
    /// commands whose arguments the server declared **signable**, and this server
    /// declares none — it never handles `chat_session_update`, holds no player
    /// public keys, and reports `enforcesSecureChat = false`. Sending a `COMMANDS`
    /// tree (which [`encode_commands`](ServerProtocol::encode_commands) now does)
    /// does not change that: the tree carries no signability, so every command
    /// from a real client still arrives unsigned.
    ChatCommand {
        /// Command text without the leading `/`.
        command: String,
    },
    /// A player typed an ordinary chat message (`minecraft:chat`, #469).
    ///
    /// The sibling of [`ChatCommand`](Self::ChatCommand), and the half that
    /// was missing entirely: the outbound direction
    /// ([`encode_system_chat`](ServerProtocol::encode_system_chat),
    /// clientbound `system_chat`) has always been complete and well-tested,
    /// so grepping for "chat" found a finished feature and hid the fact that
    /// a player could not say anything to us at all.
    ///
    /// # Only the text survives decoding, deliberately
    ///
    /// The wire packet also carries a timestamp, a salt, an optional 256-byte
    /// signature and a last-seen acknowledgement block
    /// (`ServerboundChatPacket`, 26.2). Those are decoded — the layout has to
    /// be read to find the end of the frame — and then **dropped**, because
    /// this crate has no session-key infrastructure to verify a signature
    /// against: it never handles `chat_session_update`, holds no player public
    /// keys, and reports `enforcesSecureChat = false` in its own status
    /// response. Carrying an unverifiable signature into the server loop would
    /// be strictly worse than not carrying it, because a later reader could
    /// mistake its presence for validation.
    ///
    /// Chat is therefore **broadcast unsigned**, as a `system_chat` component
    /// rendered in vanilla's own `chat.type.text` (`"<%s> %s"`) form, rather
    /// than as a real `player_chat` packet. Verifying signatures and emitting
    /// `player_chat` is a separate, larger piece of work; see
    /// `docs/player-chat.md`.
    ///
    /// The acknowledgement `offset` is dropped for the same reason
    /// `ChatAckInfo` is unreachable from the WASM plugin ABI
    /// (`lodestone-wasm-host`'s `abi.rs`): the sequence counter belongs to
    /// whoever drives the connection, and a second writer forks it.
    Chat {
        /// The message text exactly as the player typed it, capped at 256
        /// characters by the wire format.
        message: String,
    },
    /// A custom plugin-message payload from the client
    /// (`ServerboundCustomPayloadPacket`, issue #335) — the version-free
    /// lowering of the packet, exactly as it crossed the wire: a namespaced
    /// channel identifier plus the channel's raw bytes.
    ///
    /// Two channels are *interpreted* by the loop rather than dispatched —
    /// `minecraft:register` / `minecraft:unregister` update which channels this
    /// connection supports (see [`crate::ClientChannels`]) — and every other
    /// channel is looked up in the server's
    /// [`PluginChannelRegistry`](crate::PluginChannelRegistry) and delivered to
    /// whatever registered interest owns it, or silently dropped when none
    /// does, exactly vanilla's `DiscardedPayload` fallback.
    CustomPayload {
        /// The namespaced channel identifier (e.g. `minecraft:brand`).
        channel: ResourceKey,
        /// The channel-specific payload bytes, verbatim.
        data: Vec<u8>,
    },
    /// The client typed a new name into an open anvil's name field
    /// (`ServerboundRenameItemPacket`). Vanilla's own handler
    /// (`ServerGamePacketListenerImpl.handleRenameItem`) reads this only when
    /// `player.containerMenu instanceof AnvilMenu` — see `crate::server`'s
    /// consumer for that same gate. The text is carried raw; filtering and the
    /// 50-character cap are `AnvilMenu.setItemName`'s own `validateName`,
    /// ported to [`crate::anvil::validate_rename`] rather than done here, so a
    /// rejected rename is indistinguishable from one this crate chose not to
    /// decode.
    RenameItem {
        /// The client-typed text, unfiltered.
        name: String,
    },
    /// The client pressed a data-driven button in an open menu
    /// (`ServerboundContainerButtonClickPacket`). Only `EnchantmentMenu` reads
    /// this in vanilla (`ServerGamePacketListenerImpl.handleContainerButtonClick`
    /// → `AbstractContainerMenu.clickMenuButton`) — every other menu's
    /// `clickMenuButton` override is the default `false`. `button_id` is
    /// `EnchantmentMenu.clickMenuButton`'s **slot index** (`0..3`), not a cost;
    /// the cost is that slot's own entry in the table's three
    /// `container_set_data` properties, re-derived server-side rather than
    /// trusted from the client.
    ContainerButtonClick {
        /// The window the client believes is open — vanilla compares this
        /// against `player.containerMenu.containerId` before doing anything.
        window_id: i32,
        /// Which of the three enchantment offers was chosen.
        button_id: i32,
    },
    /// A packet the loop does not need to act on (teleport confirmations,
    /// look-only or status-only movement, and several other decoded-but-
    /// unmodelled families — see `crates/protocol/v770/src/server_protocol.rs`'s
    /// own arm comments for exactly which). The loop ignores these but stays
    /// connected.
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
/// 3. [`encode_registry_data`](ServerProtocol::encode_registry_data) then
///    [`begin_configuration`](ServerProtocol::begin_configuration) once the
///    resulting [`ServerBound::LoginAcknowledged`] arrives, to emit the
///    Configuration-phase registry stream followed by the finish signal
///    (issue #275: the registries must precede `FINISH_CONFIGURATION`);
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
/// One column encoder, owned rather than borrowed — the whole of what a
/// blocking worker needs to turn a freshly generated column into wire bytes.
///
/// # Why this exists as its own trait
///
/// Protocol encode used to run on the **connection task**: the join pipeline
/// awaited a column off the blocking pool and then called
/// [`ServerProtocol::encode_chunk`] inline, on the same task that owes the player
/// a reply to their block break. Measured end-to-end, `encode_chunk` is
/// **62 M instructions / ≈2.4 ms per column**, so a 1,089-column view is about
/// **2.6 s of serial encode work** interposed between the player's packets and
/// their answers — and `ViewTracker::build_batch` repeats the same shape every
/// time the player walks across a chunk boundary (a 33-column strip at
/// `view_radius = 16`, ≈80 ms).
///
/// The fix is to encode **inside** the `spawn_blocking` closure that generated
/// the column, so the connection task only writes frames. That needs a `'static`
/// encoder, and [`ServerProtocol`] is reached as `&P` everywhere in
/// `crate::server` (widening that to `Arc<P>` would break every `&P`-shaped call
/// site, the same constraint that produced [`crate::server::SourceRef`]). An
/// `Arc<dyn ChunkEncoder>` obtained *from* the `&P` is the seam that avoids it.
///
/// # Ordering is unaffected, and that is why this is safe
///
/// Emission order is fixed by `crate::join_scheduler::ColumnQueue` at **spawn**
/// time, not by completion order: `ColumnPipeline` awaits the front of its
/// in-flight queue by reference. So moving the encode into the worker changes
/// *who* runs it and never *when the bytes go out* — the wire stays a pure
/// function of the queue.
///
/// # Implementing it
///
/// Implement it on the [`ServerProtocol`] type itself where that type is
/// stateless (`V770ServerProtocol` is a unit struct), have `encode_chunk`
/// delegate to it, and return `Some(Arc::new(Self))` from
/// [`ServerProtocol::chunk_encoder`]. One body, so the two cannot drift.
pub trait ChunkEncoder: Send + Sync + 'static {
    /// Encodes one terrain column into a client-bound packet — byte-identical to
    /// [`ServerProtocol::encode_chunk`] for the same arguments.
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective;
}

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

    /// Emits the Configuration-phase `registry_data` packets — one per
    /// synchronized registry, so the client can resolve the bare holder ids
    /// later packets carry (`login`'s `dimension_type` index, `set_time`'s
    /// `world_clock` keys). **Issue #275.** Sent by the server loop **before**
    /// [`begin_configuration`](ServerProtocol::begin_configuration)'s finish
    /// signal; a real client expects the registries to precede
    /// `FINISH_CONFIGURATION`.
    ///
    /// The default emits nothing. A protocol that does not host (every legacy
    /// family — only a family with a `ServerProtocol` implementation hosts),
    /// or a host that has no registry data to declare yet, sends no packets
    /// and behaves exactly as it did before this method existed — the same
    /// additive, version-free seam [`encode_status_response`](ServerProtocol::encode_status_response)
    /// established. This is deliberately a separate call rather than a
    /// prefix inside [`begin_configuration`](ServerProtocol::begin_configuration):
    /// routing the registry stream through its own method makes "registries
    /// before finish" a version-free invariant of the choreography in
    /// `crate::server`'s `serve_connection_inner`, instead of something each
    /// implementor must remember inside its own `begin_configuration`.
    fn encode_registry_data(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    /// Encodes the server-list status reply to a [`ServerBound::StatusRequest`]
    /// (vanilla `ClientboundStatusResponsePacket`, whose whole body is one
    /// length-prefixed JSON document — `ByteBufCodecs.lenientJson(32767)`,
    /// `net/minecraft/network/protocol/status/ClientboundStatusResponsePacket.java:16`).
    ///
    /// The parameters are deliberately scalars rather than a struct: everything
    /// here is version-free, but the two fields vanilla's own `ServerStatus`
    /// also carries — `version.name` and `version.protocol` — are *not*, so the
    /// implementor fills those from its own protocol number, exactly as
    /// vanilla's `ServerStatus.Version.current()` does
    /// (`status/ServerStatus.java:71-74`). This crate must never name a
    /// protocol number, so it cannot pass them in.
    ///
    /// * `description` — the MOTD, serialized as a text component.
    /// * `players_online` / `players_max` — vanilla's `players.online` /
    ///   `players.max` (`status/ServerStatus.java:52-60`).
    /// * `sample` — `players.sample`, a list of `(uuid, name)` pairs
    ///   (`NameAndId`, `server/players/NameAndId.java:11-13`). Empty is legal
    ///   and is what vanilla sends when the sample is disabled.
    /// * `favicon_png` — raw PNG bytes, which the implementor base64-encodes
    ///   behind vanilla's mandatory `data:image/png;base64,` prefix
    ///   (`status/ServerStatus.java:36`). `None` omits the field entirely.
    /// * `enforces_secure_chat` — vanilla's `enforcesSecureChat`, default
    ///   `false` (`status/ServerStatus.java:30`).
    ///
    /// The default emits nothing, so a protocol with no status support behaves
    /// exactly as it did before this method existed.
    fn encode_status_response(
        &self,
        description: &str,
        players_online: i32,
        players_max: i32,
        sample: &[(Uuid, String)],
        favicon_png: Option<&[u8]>,
        enforces_secure_chat: bool,
    ) -> ServerDirective {
        let _ = (
            description,
            players_online,
            players_max,
            sample,
            favicon_png,
            enforces_secure_chat,
        );
        ServerDirective::None
    }

    /// Encodes a disconnect packet carrying `reason`, for the phase the
    /// connection is currently in (issue #279).
    ///
    /// **The packet is phase-specific in both id *and* encoding**, which is the
    /// one thing to get right here:
    ///
    /// | phase | vanilla packet | reason encoded as |
    /// |---|---|---|
    /// | Login | `ClientboundLoginDisconnectPacket` | **JSON string** (`ByteBufCodecs.lenientJson(262144)`, `login/ClientboundLoginDisconnectPacket.java:18`) |
    /// | Configuration | `ClientboundDisconnectPacket` | **NBT** (`TRUSTED_CONTEXT_FREE_STREAM_CODEC`, `common/ClientboundDisconnectPacket.java:11-12`) |
    /// | Play | `ClientboundDisconnectPacket` | **NBT**, same codec |
    ///
    /// Login is the odd one out for historical reasons — the login phase predates
    /// NBT components on the wire — and an implementor that writes NBT there
    /// produces a packet a real client cannot parse. `Status` has no disconnect
    /// packet at all in 26.2 (its clientbound set is `status_response` and
    /// `pong_response` only), so vanilla just closes the channel there; an
    /// implementor should return [`ServerDirective::None`] for it rather than
    /// inventing an id.
    ///
    /// Sending this does **not** close the connection — the caller does that,
    /// after the write, exactly as vanilla's `Connection::disconnect` flushes the
    /// packet before closing.
    ///
    /// The default emits nothing, so a protocol without disconnect support closes
    /// silently, which is how every family behaved before this method existed.
    fn encode_disconnect(&self, state: State, reason: &Text) -> ServerDirective {
        let _ = (state, reason);
        ServerDirective::None
    }

    /// Encodes the reply to a [`ServerBound::PingRequest`] (vanilla
    /// `ClientboundPongResponsePacket`: the same single big-endian `long`,
    /// echoed unchanged —
    /// `net/minecraft/network/protocol/ping/ClientboundPongResponsePacket.java:14-19`).
    ///
    /// The default emits nothing.
    fn encode_pong_response(&self, time: i64) -> ServerDirective {
        let _ = time;
        ServerDirective::None
    }

    /// Encodes one line of server-originated chat to the calling client
    /// (vanilla `ClientboundSystemChatPacket`: a text component plus an
    /// `overlay` flag, where `false` selects the normal chat history and
    /// `true` the action bar).
    ///
    /// This is command feedback's only route back to the player (issues #48,
    /// #464) — a refusal and a success are both delivered through it, which is
    /// why the failure to implement it is silent rather than loud: the command
    /// still *runs*, the player just never learns what happened. A family that
    /// wants commands must implement this.
    ///
    /// `message` is plain text, not a serialized component: this crate must
    /// never name a wire format, so the implementor wraps it. The default
    /// emits nothing, matching every other optional encoder here.
    fn encode_system_chat(&self, message: &str) -> ServerDirective {
        let _ = message;
        ServerDirective::None
    }

    /// Encodes a server→client plugin-message payload (vanilla
    /// `ClientboundCustomPayloadPacket`, wire id `custom_payload`, issue #335).
    /// `channel` is the namespaced channel identifier; `data` is the
    /// channel-specific raw bytes, written verbatim — the same two-field shape
    /// [`ServerBound::CustomPayload`] lifts on the inbound side.
    ///
    /// This is the one wire-level route a server-initiated payload takes to a
    /// connected client. The default emits nothing, so a protocol without
    /// plugin-message support need not override it — the same convention as
    /// every other optional encoder here.
    fn encode_custom_payload(&self, channel: &ResourceKey, data: &[u8]) -> ServerDirective {
        let _ = (channel, data);
        ServerDirective::None
    }

    /// Emits the join sequence once the connection has moved into
    /// [`State::Play`] (in reply to [`ServerBound::ConfigurationFinished`]):
    /// the join-game packet, default spawn position, initial teleport, and
    /// chunk-cache center. Does not send any chunks; the loop calls
    /// [`begin_chunk_batch`](Self::begin_chunk_batch)/
    /// [`encode_chunk`](Self::encode_chunk)/
    /// [`end_chunk_batch`](Self::end_chunk_batch) separately so it can drive
    /// the view radius itself.
    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective>;

    /// Like [`begin_play`](Self::begin_play), but derives the spawn teleport
    /// and default-spawn-position coordinates from `spawn` (world-space, feet
    /// position) rather than from hardcoded version-specific literals — issue
    /// #461: spawn Y is terrain-derived, and the server computes it; the
    /// protocol only needs to encode it. The chunk-cache center is also
    /// derived from `spawn` rather than assumed to be `(0, 0)`.
    ///
    /// The default delegates to [`begin_play`](Self::begin_play), so a family
    /// that has not adopted terrain-derived spawn yet keeps its existing
    /// hardcoded join behaviour unchanged.
    fn begin_play_at(&self, view_radius: i32, spawn: Vec3, mode: GameMode) -> Vec<ServerDirective> {
        let _ = (spawn, mode);
        self.begin_play(view_radius)
    }

    /// Encodes a game-mode change for the local player (vanilla
    /// `ClientboundGameEventPacket` with `CHANGE_GAME_MODE`, whose float
    /// parameter is the `GameType` id).
    ///
    /// This is *only* the mode; the abilities it implies travel in
    /// [`encode_player_abilities`](Self::encode_player_abilities), exactly as
    /// vanilla sends two packets from `ServerPlayer.setGameMode`. The default
    /// emits nothing.
    fn encode_game_mode(&self, mode: GameMode) -> ServerDirective {
        let _ = mode;
        ServerDirective::None
    }

    /// Encodes the local player's movement abilities (vanilla
    /// `ClientboundPlayerAbilitiesPacket`) — what actually grants creative
    /// flight and instant build on the client.
    ///
    /// Sent at join and on every game-mode change. Without it a client told it
    /// is in creative still cannot fly, because flight permission lives in this
    /// packet and not in the mode. The default emits nothing.
    fn encode_player_abilities(&self, abilities: Abilities) -> ServerDirective {
        let _ = abilities;
        ServerDirective::None
    }

    /// Marks the start of a chunk batch (vanilla's `CHUNK_BATCH_START`, an
    /// empty body in every known protocol).
    fn begin_chunk_batch(&self) -> ServerDirective;

    /// Encodes one terrain column into a client-bound packet.
    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective;

    /// The same encoder as [`encode_chunk`](Self::encode_chunk), detached from
    /// `&self` so it can be **moved into the blocking worker that generated the
    /// column** — see [`ChunkEncoder`] for the measurement that made this
    /// necessary and `docs/server-chunk-encode-offload.md` for the shape.
    ///
    /// The default returns `None`, which means "no off-task encoder": every
    /// caller then falls back to calling [`encode_chunk`](Self::encode_chunk) on
    /// its own task, which is exactly what every caller did before this method
    /// existed. So a family that has not adopted it — and every test protocol in
    /// this workspace — keeps byte-identical behaviour.
    ///
    /// # The one invariant an implementor owes
    ///
    /// The returned encoder must produce **byte-identical** output to
    /// [`encode_chunk`](Self::encode_chunk) for the same arguments. The only
    /// safe way to guarantee that is to have one body and make one call the
    /// other; `V770ServerProtocol` implements [`ChunkEncoder`] and its
    /// `encode_chunk` delegates to it, so there is a single implementation and
    /// nothing to keep in sync.
    fn chunk_encoder(&self) -> Option<std::sync::Arc<dyn ChunkEncoder>> {
        None
    }

    /// Marks the end of a chunk batch of `batch_size` columns (vanilla's
    /// `CHUNK_BATCH_FINISHED`).
    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective;

    /// Encodes a **light-only** update for one column (vanilla's
    /// `ClientboundLightUpdatePacket`, wire id `light_update`) — the packet that
    /// makes a placed torch light its column without re-sending the terrain.
    ///
    /// This is the ninth of nine links in the "torches emit no light" chain, and
    /// it was the only missing one: the client decode, `LightPatch`'s three-state
    /// merge and the re-mesh signal all already existed. See [`crate::light`] for
    /// the audit and `docs/server-chunk-light.md` for the wire format.
    ///
    /// # The wire order is not [`ColumnLight`]'s argument order
    ///
    /// The body is `cx`, `cz`, then **exactly** what
    /// [`lodestone_world::ColumnLight::encode`] writes: four section bitsets in
    /// the order sky / block / empty-sky / empty-block, then the two array lists.
    /// That is *not* the order `LightPatch::from_light_masks` takes its arguments
    /// in (it interleaves each layer's mask with its empty mask), and an
    /// implementor that follows the constructor instead produces a packet a real
    /// client mis-merges silently. `ColumnLight::encode` is already the exact
    /// `ClientboundLightUpdatePacketData` shape, so an implementor should call it
    /// rather than reimplement the four bitsets.
    ///
    /// The default emits nothing, so a family without light support falls back to
    /// the whole-column resend [`crate::light`] describes.
    fn encode_light_update(
        &self,
        cx: i32,
        cz: i32,
        light: &lodestone_world::ColumnLight,
    ) -> ServerDirective {
        let _ = (cx, cz, light);
        ServerDirective::None
    }

    /// Computes the light for one column, so
    /// [`encode_light_update`](Self::encode_light_update) has something to send.
    ///
    /// This is version-specific and this crate cannot do it: the light engine
    /// runs over `lodestone_world`'s **state-id** column, and resolving a
    /// canonical state string to a registry id is exactly the seam
    /// [`encode_chunk`](Self::encode_chunk) crosses. So the implementor converts
    /// and floods, and the server only decides *when* to ask.
    ///
    /// The result is the **isolated** compute — light entering from a neighbouring
    /// column is not pulled in. That is the same residual `encode_chunk` already
    /// carries (`docs/server-chunk-light.md` records it as a measured Δ5 sky-light
    /// dark bias at column borders), and closing it needs light computed in the
    /// chunk source where the 3×3 neighbourhood is resident, not a wider signature
    /// here. **If a column ever carries precomputed light, `ChunkColumn::set_block`
    /// and `ChunkStore::set_block` must invalidate it** — both write blocks into a
    /// retained column without touching anything derived from them, and stale
    /// light produces a correct-looking wire, a re-meshed client, and no change on
    /// screen.
    ///
    /// The default answers `None`, which the server reads as "this family cannot
    /// compute light", and it falls back to the column resend.
    fn compute_column_light(&self, column: &ChunkColumn) -> Option<lodestone_world::ColumnLight> {
        let _ = column;
        None
    }

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

    /// Encodes vanilla's `TAKE_ITEM_ENTITY` — the **pickup animation**: the item
    /// entity arcs toward the collector and shrinks. The default emits nothing.
    ///
    /// # This is an animation cue, not the pickup itself
    ///
    /// The inventory write and the entity's removal are separate and already
    /// happen. This packet exists only so the client can *show* the take, and the
    /// client deliberately keeps the item entity alive to interpolate it, removing
    /// it when the animation finishes (`ClientPacketListener.handleTakeItemEntity`;
    /// our own `lodestone-shell`'s `entities.rs` carries the matching lerp).
    ///
    /// **So the ordering is load-bearing and it is easy to get wrong in a way that
    /// looks fixed.** Vanilla's `ItemEntity.playerTouch` calls `player.take(this,
    /// orgCount)` and only *then* `this.discard()`. A server that removes the entity
    /// first — or emits `REMOVE_ENTITIES` in the same pass, before this — leaves the
    /// client with nothing to interpolate and produces no animation at all, with the
    /// packet present and correct on the wire.
    ///
    /// `amount` is the item entity's stack count **before** the inventory took any
    /// of it — vanilla passes `orgCount`, captured ahead of
    /// `player.getInventory().add(itemStack)`, which shrinks the stack in place. It
    /// is *not* the amount that actually fitted, and the two differ exactly when a
    /// pickup is partial. It drives the client's pickup sound pitch, so a hardcoded
    /// `1` is audible rather than merely wrong.
    fn encode_take_item_entity(
        &self,
        item_entity_id: i32,
        collector_entity_id: i32,
        amount: i32,
    ) -> ServerDirective {
        let _ = (item_entity_id, collector_entity_id, amount);
        ServerDirective::None
    }

    /// Encodes the Brigadier command tree (vanilla `ClientboundCommandsPacket`,
    /// wire id `commands`) — the packet that makes tab completion and command
    /// syntax highlighting possible at all.
    ///
    /// `tree` is already the **per-player** projection: pruned to what this
    /// connection's permission level may see, with every child and redirect index
    /// renumbered against the pruned node list
    /// (`crate::commands::wire::project_filtered`). An implementation writes the
    /// nodes out and must not re-derive an index, because the only thing that
    /// makes a flat index graph self-consistent is that one walk assigned all of
    /// it.
    ///
    /// # The default is silence, and that is the right default
    ///
    /// A protocol family with no override sends nothing, and the client simply
    /// has no tree — which is exactly the state every family was in before this
    /// existed. The failure mode of the alternative (a required method) would be
    /// a legacy family forced to grow an encoder for a packet whose id it may
    /// number differently.
    /// Encodes vanilla's `ClientboundHurtAnimationPacket` — **the camera damage
    /// tilt**, and the red hurt flash on a remote entity.
    ///
    /// # Where vanilla sends it
    ///
    /// `LivingEntity.dealDefaultKnockback` calls `indicateDamage(xd, zd)` with the
    /// horizontal offset from the damage source to the victim, and only
    /// `ServerPlayer` overrides it — `LivingEntity.indicateDamage` is empty, and
    /// `LivingEntity.getHurtDir` is a constant `0.0F`. So in vanilla this packet
    /// goes to **one** connection, the hurt player's own, and never for a mob.
    ///
    /// `yaw` is `ServerPlayer.indicateDamage`'s own expression,
    /// `atan2(zd, xd) * 180 / PI - yRot` — degrees, in the victim's frame, so a hit
    /// from straight ahead is `0`. See [`crate::vitals::hurt_dir_degrees`], which
    /// is that formula and the one place it should be computed.
    ///
    /// # The one deliberate widening, and why the screen needs it
    ///
    /// `dealDefaultKnockback` runs only for a source **outside**
    /// `#minecraft:damage_type/no_knockback`, and that tag holds `fall`, `drown`,
    /// `starve`, `lava`, `in_fire`, `cactus`, `freeze`, `magic` — i.e. very nearly
    /// every way a singleplayer world currently hurts anyone. Vanilla still tilts
    /// the camera for those, because `ClientboundDamageEventPacket` also sets
    /// `hurtTime`, and this crate encodes no `damage_event`. This crate therefore
    /// sends `hurt_animation` for a directionless hit too, with `yaw` **exactly
    /// `0.0`** — the pure-roll case, which is what a vanilla client shows for a
    /// player who has not been knocked back since spawning. It is a substitution
    /// on the *route*, not on the pixels; encoding `damage_event` (which needs a
    /// `minecraft:damage_type` registry id per source) is the follow-up that makes
    /// the route vanilla's as well.
    ///
    /// The default emits nothing, so a protocol family without hurt-animation
    /// support need not override it and the tilt simply never fires there.
    fn encode_hurt_animation(&self, entity_id: i32, yaw: f32) -> ServerDirective {
        let _ = (entity_id, yaw);
        ServerDirective::None
    }

    /// Encodes vanilla's `ClientboundEntityEventPacket` — one raw per-entity-type
    /// status byte, `ServerLevel.broadcastEntityEvent`'s whole payload.
    ///
    /// `event` is a `net.minecraft.world.entity.EntityEvent` constant, and the
    /// values are **not** a registry: they are reused across entity types, so the
    /// same byte means different things on different species. The ones this crate
    /// sends today, read off `EntityEvent`:
    ///
    /// | byte | constant | meaning |
    /// |---|---|---|
    /// | 3 | `DEATH` | `LivingEntity.die`'s broadcast — the fall-over animation |
    /// | 6 | `TAMING_FAILED` | smoke puff |
    /// | 7 | `TAMING_SUCCEEDED` | hearts |
    /// | 18 | `IN_LOVE_HEARTS` | breeding hearts |
    ///
    /// **Note the wire shape**: the entity id is a plain big-endian `int`, *not* a
    /// VarInt — `ClientboundEntityEventPacket.write` is `writeInt` then
    /// `writeByte`, one of the few remaining fixed-width ids in play. Porting from
    /// the field list rather than from `write` would give the same two fields in
    /// the same order at the wrong widths, which desynchronises the stream instead
    /// of merely mis-animating.
    ///
    /// The default emits nothing, so a protocol family without entity-event
    /// support need not override it and a dying mob simply pops out of existence.
    fn encode_entity_event(&self, entity_id: i32, event: u8) -> ServerDirective {
        let _ = (entity_id, event);
        ServerDirective::None
    }

    fn encode_commands(&self, tree: &CommandTree) -> ServerDirective {
        let _ = tree;
        ServerDirective::None
    }

    /// Encodes vanilla's `ClientboundSetPassengersPacket` — the packet that makes
    /// a player *be* in a boat.
    ///
    /// `ServerEntity.sendPairingData` sends it on spawn and
    /// `Entity.startRiding`/`stopRiding` re-send it on every change, always as the
    /// vehicle's **whole** passenger list rather than a delta: dismounting is this
    /// packet with an empty list, which is why `passenger_ids` is a slice and not an
    /// `Option`.
    ///
    /// # This is the only channel, and without it riding cannot exist
    ///
    /// A client learns it is a passenger from nothing else. Our own shell folds it
    /// into `session::Riding` (`ClientEvent::EntityPassengersChanged`), and
    /// `lodestone_ecs::vehicle::tick_controlled_vehicle` reads that scalar to decide
    /// which vehicle to simulate — so with no producer here the whole
    /// client-authoritative boat pipeline is unreachable, however complete it is.
    /// That was the state of this tree before boats were placeable: `SET_PASSENGERS`
    /// was decoded by the v770 adapter, routed by `ingest`, consumed by the seat
    /// pin, and **emitted by nobody**.
    ///
    /// The wire shape is a VarInt vehicle id then `writeVarIntArray` — a VarInt
    /// length followed by that many VarInts, *not* the generic collection codec.
    ///
    /// The default emits nothing, so a protocol family with no passenger support
    /// need not override it and riding simply never engages there.
    fn encode_set_passengers(&self, vehicle_id: i32, passenger_ids: &[i32]) -> ServerDirective {
        let _ = (vehicle_id, passenger_ids);
        ServerDirective::None
    }

    /// Encodes a `SET_ENTITY_DATA` metadata update for an arbitrary entity id
    /// (issue #425), given every [`MetadataField`] that entity currently wants
    /// synced — not a hardcoded single field for a hardcoded entity id, the
    /// shape [`encode_air_supply_update`](Self::encode_air_supply_update) is
    /// stuck in for exactly that reason (`LOCAL_PLAYER_ENTITY_ID` only, one
    /// `INT` field only). [`crate::server::EntityStreamer::sync`] is the one
    /// caller: it calls this whenever an entity spawns with non-empty
    /// [`EntitySnapshot::metadata`], or an update changes it, passing the
    /// entity's *current* full field list each time. The default emits
    /// nothing, so a protocol without per-species metadata support need not
    /// override it and a swelling creeper simply never reaches that client's
    /// screen.
    fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
        let _ = (entity_id, fields);
        ServerDirective::None
    }

    /// Encodes the tab-list additions for players this connection has not been
    /// told about yet (issue #438; vanilla `ClientboundPlayerInfoUpdatePacket`
    /// with the `ADD_PLAYER` action, wire id `player_info_update`).
    ///
    /// **This is not cosmetic, and it is not optional for player entities.** A
    /// real client *drops* an `ADD_ENTITY` whose type is `minecraft:player`
    /// when it holds no `PlayerInfo` for that uuid:
    /// `ClientPacketListener.createEntityFromPacket` logs
    /// `"Server attempted to add player prior to sending player info"` and
    /// returns `null`, so the entity is never added to the level
    /// (`.cache/mc/26.2/client-src/net/minecraft/client/multiplayer/
    /// ClientPacketListener.java:591-604`). [`crate::players::PlayerListStreamer`]
    /// is the one caller and `crate::server`'s streaming pass emits its
    /// directives **before** the entity diff for exactly that reason.
    ///
    /// The default emits nothing, like every other encoder here, so a protocol
    /// with no tab-list support need not override it — at the cost that a
    /// player entity will not reach that version's clients at all, which is
    /// the honest consequence rather than a half-sent spawn.
    fn encode_player_info_add(&self, players: &[PlayerListing]) -> Vec<ServerDirective> {
        let _ = players;
        Vec::new()
    }

    /// Encodes the tab-list removals for players that have left (issue #438;
    /// vanilla `ClientboundPlayerInfoRemovePacket`, wire id
    /// `player_info_remove`) — the counterpart to
    /// [`encode_player_info_add`](Self::encode_player_info_add), emitted by the
    /// same [`crate::players::PlayerListStreamer`] pass. Without it a departed
    /// player's `PlayerInfo` lingers, so their name stays in the tab list even
    /// though the entity diff already sent a `REMOVE_ENTITIES` for them. The
    /// default emits nothing, for the same reason as above.
    fn encode_player_info_remove(&self, uuids: &[Uuid]) -> Vec<ServerDirective> {
        let _ = uuids;
        Vec::new()
    }

    /// Updates existing tab-list entries' game modes (vanilla's
    /// `ClientboundPlayerInfoUpdatePacket` carrying **only** the
    /// `UPDATE_GAME_MODE` action, ordinal 2).
    ///
    /// Needed by `/gamemode`. `encode_player_info_add` sends a game mode too, but
    /// only at join and only the mode the player joined in — it has no
    /// per-connection mode to read, and says so in its own doc comment. Without
    /// this, changing mode leaves every client's tab list reporting the join mode
    /// forever, including the player's own.
    ///
    /// A slice of pairs rather than one uuid because the packet is a list and a
    /// command may change several players at once; an empty slice must emit
    /// nothing (a zero-length entry list is a legal but pointless frame).
    ///
    /// The default emits nothing, so a protocol without tab-list support needs no
    /// override and the mode change is simply invisible there rather than a
    /// failure.
    fn encode_player_info_game_mode(&self, entries: &[(Uuid, GameMode)]) -> Vec<ServerDirective> {
        let _ = entries;
        Vec::new()
    }

    /// Encodes a detonation (issue #425; vanilla `ClientboundExplodePacket`,
    /// wire id `explode`), fed from [`crate::mobs::MobSim::take_detonations`]
    /// via [`crate::tick::ExplosionFeed`] — the handoff that finally gives
    /// [`crate::mobs::MobSim::explode`] (issue #213's own exposure/damage
    /// maths) a wire-visible consequence: before this, a creeper's own fuse
    /// completing removed the creeper and landed real damage on nearby mobs,
    /// but no connected client ever saw a particle or heard a sound, because
    /// nothing encoded this packet at all.
    ///
    /// `centre`/`radius` are the blast's own. This crate tracks no block-
    /// destruction model, so an implementor has nothing to report for
    /// vanilla's `blockCount`/`blockParticles`/`playerKnockback` fields
    /// beyond a faithful "none of that happened" — see the v770
    /// implementation's own doc comment for exactly which fields that
    /// leaves stubbed versus real. The default emits nothing, so a protocol
    /// without explosion support need not override it and a detonation
    /// simply stays silent and invisible.
    fn encode_explode(&self, centre: Vec3, radius: f32) -> ServerDirective {
        let _ = (centre, radius);
        ServerDirective::None
    }

    /// Encodes a positioned sound (issue #530; vanilla
    /// `ClientboundSoundPacket`, wire id `sound`).
    ///
    /// `sound` is a `minecraft:sound_event` registry id
    /// ([`crate::effects`] validates every name it derives against the real
    /// registry before it reaches here, so an implementor may send the
    /// registry-reference holder form rather than an inline definition).
    /// `seed` picks between a sound event's variants — vanilla's per-play
    /// `random.nextLong()`.
    ///
    /// The default emits nothing, so a protocol without sound support need not
    /// override it and the world is simply silent for that client, which is
    /// exactly the state every protocol here was in before this method existed.
    fn encode_sound(
        &self,
        sound: &str,
        category: SoundCategory,
        pos: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) -> ServerDirective {
        let _ = (sound, category, pos, volume, pitch, seed);
        ServerDirective::None
    }

    /// Encodes one of vanilla's numbered composite effects (issue #530;
    /// `ClientboundLevelEventPacket`, wire id `level_event`) — see
    /// [`crate::effects::PARTICLES_DESTROY_BLOCK`], which is a sound *and* a
    /// particle burst in one packet. The default emits nothing.
    fn encode_level_event(&self, event: i32, pos: BlockPos, data: i32, global: bool) -> ServerDirective {
        let _ = (event, pos, data, global);
        ServerDirective::None
    }

    /// Encodes a particle burst (issue #530; `ClientboundLevelParticlesPacket`,
    /// wire id `level_particles`).
    ///
    /// `particle` is a `minecraft:particle_type` registry id. Only
    /// argument-less (`SimpleParticleType`) particles are expressible: the
    /// per-type option payload — dust colour, block state, item stack — has no
    /// representation here, the same scope
    /// [`crate::effects::WorldEffect::Particles`] carries and the same one the
    /// v770 *decoder* already declares. The default emits nothing.
    fn encode_level_particles(
        &self,
        particle: &str,
        pos: Vec3,
        offset: Vec3f,
        max_speed: f32,
        count: i32,
        long_distance: bool,
    ) -> ServerDirective {
        let _ = (particle, pos, offset, max_speed, count, long_distance);
        ServerDirective::None
    }

    /// Encodes one block entity's update tag (vanilla
    /// `ClientboundBlockEntityDataPacket`, wire id `block_entity_data`).
    ///
    /// `block_entity_type` is a `minecraft:block_entity_type` registry **key**, not
    /// a numeric id — resolving it is version-specific, so the implementor does it.
    /// A key this version does not have must emit nothing rather than guess.
    ///
    /// This is the mid-play counterpart to the block-entity array a chunk packet
    /// carries at load time: without it, a record that changes while the chunk is
    /// already resident never reaches the client at all. The default emits nothing.
    fn encode_block_entity_data(
        &self,
        pos: BlockPos,
        block_entity_type: &str,
        nbt: &lodestone_core::Nbt,
    ) -> ServerDirective {
        let _ = (pos, block_entity_type, nbt);
        ServerDirective::None
    }

    /// Encodes one [`crate::effects::WorldEffect`] by dispatching to whichever
    /// of the encoders above it names.
    ///
    /// Provided rather than required: it is pure dispatch, so no implementor
    /// should override it, and it is the only method a *publisher* has to know
    /// about — `serve_play`'s drain calls this and nothing else, so adding a
    /// fourth effect kind is a change here plus one encoder, never a change at
    /// every drain site.
    fn encode_world_effect(&self, effect: &crate::effects::WorldEffect) -> ServerDirective {
        match effect {
            crate::effects::WorldEffect::Sound {
                sound,
                category,
                pos,
                volume,
                pitch,
                seed,
            } => self.encode_sound(sound, *category, *pos, *volume, *pitch, *seed),
            crate::effects::WorldEffect::LevelEvent {
                event,
                pos,
                data,
                global,
            } => self.encode_level_event(*event, *pos, *data, *global),
            crate::effects::WorldEffect::Particles {
                particle,
                pos,
                offset,
                max_speed,
                count,
                long_distance,
            } => self.encode_level_particles(particle, *pos, *offset, *max_speed, *count, *long_distance),
            crate::effects::WorldEffect::BlockEntityData {
                pos,
                block_entity_type,
                nbt,
            } => self.encode_block_entity_data(*pos, block_entity_type, nbt),
        }
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

    /// Encodes the local player's experience bar (vanilla
    /// `ClientboundSetExperiencePacket`).
    ///
    /// **The client half already existed**: `V770Adapter::handle_play` decodes
    /// `SET_EXPERIENCE` into `ClientEvent::ExperienceChanged`, complete with the
    /// note that the wire order is *progress, level, total* rather than declaration
    /// order. Nothing produced the packet, which is the island in the serverbound
    /// direction — a decoder with no encoder.
    ///
    /// `progress` is the bar fill in `0.0..1.0`, `level` the number shown on it, and
    /// `total` vanilla's lifetime `totalExperience`, which is **not** derivable from
    /// the other two (see [`crate::experience::PlayerExperience`]).
    fn encode_set_experience(&self, progress: f32, level: i32, total: i32) -> ServerDirective {
        let _ = (progress, level, total);
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
    fn encode_set_health(&self, health: f32, food: i32, saturation: f32) -> ServerDirective {
        let _ = (health, food, saturation);
        ServerDirective::None
    }

    /// Encodes the death notification (vanilla `ClientboundPlayerCombatKillPacket`,
    /// wire id `player_combat_kill`) — **the packet that raises the death screen**.
    ///
    /// # Why this exists, and why nothing else does the job
    ///
    /// Health reaching `0.0` is *not* what opens the death screen, in vanilla or
    /// here. `ClientPacketListener.handleSetHealth`
    /// (`.cache/mc/26.2/client-src/net/minecraft/client/multiplayer/ClientPacketListener.java:1235-1240`)
    /// only calls `hurtTo`/`setFoodLevel`/`setSaturation`; the screen comes from
    /// `handlePlayerCombatKill` at `:1845-1855`:
    ///
    /// ```java
    /// if (this.minecraft.player.shouldShowDeathScreen()) {
    ///    this.minecraft.gui.setScreen(new DeathScreen(packet.message(), …));
    /// } else {
    ///    this.minecraft.player.respawn();
    /// }
    /// ```
    ///
    /// So a server that only sends `set_health(0.0)` leaves a real client — and
    /// this workspace's own client, whose `Screen::Death` and `death_frame` are
    /// fully wired and were reaching zero pixels for exactly this reason — sitting
    /// at zero hearts with no screen, no respawn button and no way out. That reads
    /// as a server hang, which is how it was reported.
    ///
    /// `player_entity_id` is the *victim's* entity id (the client discards it, but
    /// it is on the wire). `message` is the localized death message, vanilla's
    /// `DamageSource.getLocalizedDeathMessage` (`DamageSource.java:71-86`):
    /// `Component.translatable("death.attack." + msgId, victimName)` when nothing
    /// living gets the kill credit.
    ///
    /// The default emits nothing, so a protocol without death support need not
    /// override it — and its client simply never gets a death screen, which is a
    /// gap rather than a wrong packet.
    fn encode_player_combat_kill(&self, player_entity_id: i32, message: &Text) -> ServerDirective {
        let _ = (player_entity_id, message);
        ServerDirective::None
    }

    /// Encodes a post-death respawn (vanilla `ClientboundRespawnPacket` plus the
    /// placement teleport `PlayerList::respawn` sends after it), moving the client
    /// off the death screen and to `spawn`.
    ///
    /// # Why the respawn packet is not optional
    ///
    /// This is the other half of [`encode_player_combat_kill`](Self::encode_player_combat_kill)
    /// and it fails in a nastier way when missing: the client's `Dead` marker is
    /// cleared only by `ClientEvent::Respawned`, which its adapter decodes from
    /// `player_combat_kill`'s counterpart `respawn` — **not** from a
    /// `set_health(20.0)`. A server that answers `client_command(perform_respawn)`
    /// by resetting vitals and sending health alone therefore refills the hearts
    /// and leaves the death screen up forever, with the player's own respawn
    /// button doing nothing. Sending health without this is strictly worse than
    /// sending neither, because it looks like it worked.
    ///
    /// Returns a directive *list* rather than one directive because the respawn is
    /// two packets: the dimension/data-to-keep record, then the position. The
    /// default emits nothing.
    fn encode_respawn(&self, spawn: Vec3) -> Vec<ServerDirective> {
        let _ = spawn;
        Vec::new()
    }

    /// Encodes a **dimension change** — the same `ClientboundRespawnPacket` pair
    /// [`encode_respawn`](Self::encode_respawn) sends, aimed at another level.
    ///
    /// # Why this is not `encode_respawn` with an argument
    ///
    /// The two differ in the one field that decides what the client throws away.
    /// `PlayerList.respawn` passes `KEEP_ALL_DATA` (`KEEP_ATTRIBUTE_MODIFIERS |
    /// KEEP_ENTITY_DATA`) for a dimension change and **zero** for a death, and
    /// `encode_respawn`'s own doc comment explains why zero is right there: it is
    /// what makes the client rebuild its player state. Rebuilding player state is
    /// exactly what a portal trip must *not* do — inventory, XP and health survive
    /// a trip — so folding the two into one encoder with a boolean would leave the
    /// dangerous default (`0`) one forgotten argument away.
    ///
    /// `dimension` is the destination *level key* (`minecraft:the_nether`), not a
    /// holder id: mapping a key to the `dimension_type` index its own
    /// [`encode_registry_data`](Self::encode_registry_data) published is the
    /// protocol family's business, and a version-free caller cannot know it. An
    /// implementation that does not recognise the key must emit **nothing** rather
    /// than guess a holder id, because a wrong id reframes every subsequent chunk
    /// against the wrong build height.
    ///
    /// # The empty return is load-bearing
    ///
    /// The default emits nothing, and `crate::server`'s travel path treats an empty
    /// list as "this protocol cannot change dimension" and **does not move the
    /// player**. That is the difference between a family without Nether support
    /// having no portals and having portals that silently drop players into terrain
    /// their client is still framing as the overworld.
    fn encode_dimension_change(
        &self,
        dimension: &str,
        spawn: Vec3,
        mode: GameMode,
    ) -> Vec<ServerDirective> {
        let _ = (dimension, spawn, mode);
        Vec::new()
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

    /// Encodes a weather transition (vanilla `ClientboundGameEventPacket`,
    /// wire id `game_event` — the same packet the client's adapter decodes
    /// into `ClientEvent::WeatherChanged`).
    /// `kind` is the vanilla event id: 1 = `START_RAINING`, 2 = `STOP_RAINING`,
    /// 7 = `RAIN_LEVEL_CHANGE`, 8 = `THUNDER_LEVEL_CHANGE`. `value` is the
    /// float parameter — 0.0 for the start/stop pair, the level for the
    /// level-change pair — matching `ClientboundGameEventPacket`'s own
    /// `writeByte(event) + writeFloat(param)` layout. The default emits
    /// nothing, so a protocol without weather support simply never rains.
    fn encode_game_event(&self, kind: u8, value: f32) -> ServerDirective {
        let _ = (kind, value);
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

    /// Encodes the clientbound `initialize_border` packet (vanilla
    /// `ClientboundInitializeBorderPacket`, wire id 43 in 26.2) — the border
    /// state a player is told about on join, sent by `PlayerList.sendLevelInfo`
    /// **before** the time sync and spawn-position packets (`PlayerList.java:
    /// 648-663`). The default emits nothing.
    ///
    /// Passed the whole [`WorldBorder`](crate::border::WorldBorder) because
    /// the packet's `old_size`/`new_size`/`lerp_time` triple is exactly the
    /// extent's `size`/`lerp_target`/`lerp_time` readout
    /// (`ClientboundInitializeBorderPacket.java:35-38`), and its
    /// `absolute_max_size`/`warning_blocks`/`warning_time` are flat fields —
    /// the encoder should not have to re-derive that mapping from primitives.
    fn encode_initialize_border(&self, border: &crate::border::WorldBorder) -> ServerDirective {
        let _ = border;
        ServerDirective::None
    }

    /// Encodes the clientbound `set_border_center` packet (vanilla
    /// `ClientboundSetBorderCenterPacket`, wire id 88 in 26.2). The default
    /// emits nothing.
    fn encode_set_border_center(&self, x: f64, z: f64) -> ServerDirective {
        let _ = (x, z);
        ServerDirective::None
    }

    /// Encodes the clientbound `set_border_lerp_size` packet (vanilla
    /// `ClientboundSetBorderLerpSizePacket`, wire id 89 in 26.2) — the *live*
    /// resize delta a border shrink/grow broadcasts, carrying `old_size`,
    /// `new_size` and the lerp time in **milliseconds**. Vanilla writes
    /// `border.getLerpTime()` — remaining server **ticks** — directly
    /// (`ClientboundSetBorderLerpSizePacket.java:20`, no ×50), but this crate's
    /// client decodes the field as `lerp_time_ms` and interpolates on wall-clock
    /// (`lodestone-game::worldborder`'s `BorderExtent::Moving`), so the caller
    /// converts ticks → ms (`* 50`) before calling and this method writes the ms
    /// value verbatim — the same deliberate divergence
    /// [`encode_initialize_border`](Self::encode_initialize_border) documents
    /// for its own lerp-time field. The default emits nothing.
    fn encode_set_border_lerp_size(
        &self,
        old_size: f64,
        new_size: f64,
        lerp_time_ms: i64,
    ) -> ServerDirective {
        let _ = (old_size, new_size, lerp_time_ms);
        ServerDirective::None
    }

    /// Encodes the clientbound `set_border_size` packet (vanilla
    /// `ClientboundSetBorderSizePacket`, wire id 90 in 26.2) — the instant
    /// snap a `set_size` broadcasts. The default emits nothing.
    fn encode_set_border_size(&self, size: f64) -> ServerDirective {
        let _ = size;
        ServerDirective::None
    }

    /// Encodes the clientbound `set_border_warning_delay` packet (vanilla
    /// `ClientboundSetBorderWarningDelayPacket`, wire id 91 in 26.2).
    /// The default emits nothing.
    fn encode_set_border_warning_delay(&self, warning_time: i32) -> ServerDirective {
        let _ = warning_time;
        ServerDirective::None
    }

    /// Encodes the clientbound `set_border_warning_distance` packet (vanilla
    /// `ClientboundSetBorderWarningDistancePacket`, wire id 92 in 26.2).
    /// The default emits nothing.
    fn encode_set_border_warning_distance(&self, warning_blocks: i32) -> ServerDirective {
        let _ = warning_blocks;
        ServerDirective::None
    }

    /// Encodes the clientbound `resource_pack_push` packet (vanilla
    /// `ClientboundResourcePackPushPacket`) — the server-initiated half of the
    /// resource-pack lifecycle (issue #334). The body is the [`ResourcePackPush`]
    /// record verbatim: a raw 16-byte uuid, a VarInt-prefixed UTF-8 url, a
    /// VarInt-prefixed UTF-8 SHA-1 hash capped at 40 characters (vanilla's
    /// `MAX_HASH_LENGTH`), a bool `required` flag, then — only if present — an
    /// NBT chat component prompt. The default emits nothing.
    fn encode_resource_pack_push(&self, push: &ResourcePackPush) -> ServerDirective {
        let _ = push;
        ServerDirective::None
    }

    /// Encodes the full `ClientboundUpdateAdvancementsPacket` (26.2) — the
    /// advancement tree plus per-player progress (issue #338). The payload is
    /// [`crate::advancements::AdvancementUpdate`] verbatim, built by
    /// [`AdvancementManager::initial_update`](crate::advancements::AdvancementManager::initial_update)
    /// on join (`reset` true, the whole tree as `added`) and by
    /// [`flush_dirty`](crate::advancements::AdvancementManager::flush_dirty)
    /// on every tick that something changed (incremental `added`/`removed`
    /// deltas plus the changed `progress`). Vanilla's `AdvancementHolder`
    /// travels as the `added` list and `CriterionProgress` as each
    /// `AdvancementProgressUpdate` entry's epoch-millis. The default emits
    /// nothing, so a protocol without advancement support never shows a tree.
    fn encode_update_advancements(&self, update: &crate::advancements::AdvancementUpdate) -> ServerDirective {
        let _ = update;
        ServerDirective::None
    }

    /// Encodes the `ClientboundAwardStatsPacket` (26.2): a batch of
    /// `(StatKey, count)` pairs, sent in reply to the client's
    /// `ClientCommand(REQUEST_STATS)` (issue #338). Each `StatKey` is the
    /// stat-type registry id (e.g. `minecraft:mined`) plus the value key
    /// (item/block/entity id, or the custom-stat id), exactly vanilla's
    /// `Stat.STREAM_CODEC` dispatch; an implementor maps those to registry
    /// ids and writes the count as a varint. The default emits nothing.
    fn encode_award_stats(&self, stats: &[(crate::advancements::StatKey, i32)]) -> ServerDirective {
        let _ = stats;
        ServerDirective::None
    }

    /// Encodes the `ClientboundRecipeBookAddPacket` (26.2) — the packet that
    /// **hands out `RecipeDisplayId`s** (issue #547).
    ///
    /// Without it `PLACE_RECIPE` is structurally unreachable rather than merely
    /// unimplemented: the id a client echoes back is a position in *this* list, so
    /// no client — ours or a real vanilla 26.2 one — can ever send a valid one
    /// until something encodes it. `crate::crafting::recipe_at_index` and
    /// [`crate::crafting::recipe_book_entries`] walk the same id-sorted corpus
    /// order, so the index space cannot disagree.
    ///
    /// `replace` is vanilla's flag for "this is the whole book" (`true` at join)
    /// versus "add these to what you have". The default emits nothing, so a
    /// protocol without recipe-book support simply leaves the book empty — which
    /// is what every family other than v770 does.
    fn encode_recipe_book_add(
        &self,
        entries: &[crate::crafting::RecipeBookEntry],
        replace: bool,
    ) -> ServerDirective {
        let _ = (entries, replace);
        ServerDirective::None
    }

    /// Encodes the `ClientboundSelectAdvancementsTabPacket` (26.2), sent in
    /// reply to the client's `select_advancements_tab` request (issue #338).
    /// `tab` is the advancement id to open, or `None` to close the screen —
    /// vanilla answers the client's own request with the same id it was given.
    /// The default emits nothing.
    fn encode_select_advancements_tab(&self, tab: Option<&str>) -> ServerDirective {
        let _ = tab;
        ServerDirective::None
    }

    /// Which worldgen data bundle this protocol's hosting needs, for the
    /// [`crate::worldgen_data`] version gate (issue #407).
    ///
    /// The only bundle this crate embeds is 26.2
    /// ([`WorldgenScope::V26_2`]) — the `assets/worldgen/` data
    /// [`crate::overworld_generator`] serves. A hosting family must report
    /// [`WorldgenScope::V26_2`] if and only if that bundle is the terrain it
    /// actually wants to serve. The default reports [`WorldgenScope::None`],
    /// so a protocol that has not adopted the gate — every test double, and
    /// every family whose worldgen is not the embedded 26.2 bundle — is
    /// treated as "no worldgen data", never silently served the wrong
    /// bundle. The one production override is the v770 host (→
    /// [`WorldgenScope::V26_2`]): that is the family the embedded data
    /// belongs to.
    fn worldgen_scope(&self) -> WorldgenScope {
        WorldgenScope::None
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

    fn encode_registry_data(&self) -> Vec<ServerDirective> {
        (**self).encode_registry_data()
    }

    fn encode_status_response(
        &self,
        description: &str,
        players_online: i32,
        players_max: i32,
        sample: &[(Uuid, String)],
        favicon_png: Option<&[u8]>,
        enforces_secure_chat: bool,
    ) -> ServerDirective {
        (**self).encode_status_response(
            description,
            players_online,
            players_max,
            sample,
            favicon_png,
            enforces_secure_chat,
        )
    }

    fn encode_pong_response(&self, time: i64) -> ServerDirective {
        (**self).encode_pong_response(time)
    }

    fn encode_disconnect(&self, state: State, reason: &Text) -> ServerDirective {
        (**self).encode_disconnect(state, reason)
    }

    fn encode_system_chat(&self, message: &str) -> ServerDirective {
        (**self).encode_system_chat(message)
    }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        (**self).begin_play(view_radius)
    }

    // **Issue #329's live bug was the absence of exactly this three-line
    // forward.** `begin_play_at` has a default that discards `spawn` and calls
    // `begin_play`, so without this the box silently took that default: the
    // spiral search ran, `server.rs` passed its answer in, and the boxed
    // protocol threw it away and emitted `V770ServerProtocol::begin_play`'s
    // hardcoded `(8, 100, 8)`. Singleplayer is the *only* path that boxes the
    // protocol, so the symptom was "every join lands at y=100 at (8, 8)" with a
    // fully correct spawn search sitting one call frame away — and no live
    // oracle covers the boxed path, which is what the parity test below is for.
    fn begin_play_at(&self, view_radius: i32, spawn: Vec3, mode: GameMode) -> Vec<ServerDirective> {
        (**self).begin_play_at(view_radius, spawn, mode)
    }

    // Forwarded for the same reason `begin_play_at` is: both have defaults that
    // emit nothing, so a missing forward here would silently mute the game-mode
    // and abilities packets on the singleplayer (boxed) path alone.
    fn encode_game_mode(&self, mode: GameMode) -> ServerDirective {
        (**self).encode_game_mode(mode)
    }

    fn encode_player_abilities(&self, abilities: Abilities) -> ServerDirective {
        (**self).encode_player_abilities(abilities)
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        (**self).begin_chunk_batch()
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        (**self).encode_chunk(cx, cz, column)
    }

    fn chunk_encoder(&self) -> Option<std::sync::Arc<dyn ChunkEncoder>> {
        (**self).chunk_encoder()
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        (**self).end_chunk_batch(batch_size)
    }

    fn encode_light_update(
        &self,
        cx: i32,
        cz: i32,
        light: &lodestone_world::ColumnLight,
    ) -> ServerDirective {
        (**self).encode_light_update(cx, cz, light)
    }

    fn compute_column_light(&self, column: &ChunkColumn) -> Option<lodestone_world::ColumnLight> {
        (**self).compute_column_light(column)
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

    fn encode_take_item_entity(
        &self,
        item_entity_id: i32,
        collector_entity_id: i32,
        amount: i32,
    ) -> ServerDirective {
        (**self).encode_take_item_entity(item_entity_id, collector_entity_id, amount)
    }

    fn encode_hurt_animation(&self, entity_id: i32, yaw: f32) -> ServerDirective {
        (**self).encode_hurt_animation(entity_id, yaw)
    }

    fn encode_entity_event(&self, entity_id: i32, event: u8) -> ServerDirective {
        (**self).encode_entity_event(entity_id, event)
    }

    fn encode_commands(&self, tree: &CommandTree) -> ServerDirective {
        (**self).encode_commands(tree)
    }

    fn encode_set_passengers(&self, vehicle_id: i32, passenger_ids: &[i32]) -> ServerDirective {
        (**self).encode_set_passengers(vehicle_id, passenger_ids)
    }

    fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
        (**self).encode_set_entity_data(entity_id, fields)
    }

    fn encode_explode(&self, centre: Vec3, radius: f32) -> ServerDirective {
        (**self).encode_explode(centre, radius)
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

    fn encode_block_entity_data(
        &self,
        pos: BlockPos,
        block_entity_type: &str,
        nbt: &lodestone_core::Nbt,
    ) -> ServerDirective {
        (**self).encode_block_entity_data(pos, block_entity_type, nbt)
    }

    fn encode_air_supply_update(&self, air: i32) -> ServerDirective {
        (**self).encode_air_supply_update(air)
    }

    fn encode_set_experience(&self, progress: f32, level: i32, total: i32) -> ServerDirective {
        (**self).encode_set_experience(progress, level, total)
    }

    fn encode_set_health(&self, health: f32, food: i32, saturation: f32) -> ServerDirective {
        (**self).encode_set_health(health, food, saturation)
    }

    fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
        (**self).encode_change_difficulty(difficulty, locked)
    }

    fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
        (**self).encode_game_rule_values(entries)
    }

    fn encode_game_event(&self, kind: u8, value: f32) -> ServerDirective {
        (**self).encode_game_event(kind, value)
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

    fn encode_initialize_border(&self, border: &crate::border::WorldBorder) -> ServerDirective {
        (**self).encode_initialize_border(border)
    }

    fn encode_set_border_center(&self, x: f64, z: f64) -> ServerDirective {
        (**self).encode_set_border_center(x, z)
    }

    fn encode_set_border_lerp_size(
        &self,
        old_size: f64,
        new_size: f64,
        lerp_time_ms: i64,
    ) -> ServerDirective {
        (**self).encode_set_border_lerp_size(old_size, new_size, lerp_time_ms)
    }

    fn encode_set_border_size(&self, size: f64) -> ServerDirective {
        (**self).encode_set_border_size(size)
    }

    fn encode_set_border_warning_delay(&self, warning_time: i32) -> ServerDirective {
        (**self).encode_set_border_warning_delay(warning_time)
    }

    fn encode_set_border_warning_distance(&self, warning_blocks: i32) -> ServerDirective {
        (**self).encode_set_border_warning_distance(warning_blocks)
    }

    fn encode_resource_pack_push(&self, push: &ResourcePackPush) -> ServerDirective {
        (**self).encode_resource_pack_push(push)
    }

    fn encode_update_advancements(
        &self,
        update: &crate::advancements::AdvancementUpdate,
    ) -> ServerDirective {
        (**self).encode_update_advancements(update)
    }

    fn encode_award_stats(&self, stats: &[(crate::advancements::StatKey, i32)]) -> ServerDirective {
        (**self).encode_award_stats(stats)
    }

    fn encode_select_advancements_tab(&self, tab: Option<&str>) -> ServerDirective {
        (**self).encode_select_advancements_tab(tab)
    }

    fn encode_custom_payload(&self, channel: &ResourceKey, data: &[u8]) -> ServerDirective {
        (**self).encode_custom_payload(channel, data)
    }

    fn encode_player_info_add(&self, players: &[PlayerListing]) -> Vec<ServerDirective> {
        (**self).encode_player_info_add(players)
    }

    fn encode_player_info_remove(&self, uuids: &[Uuid]) -> Vec<ServerDirective> {
        (**self).encode_player_info_remove(uuids)
    }

    fn encode_player_info_game_mode(&self, entries: &[(Uuid, GameMode)]) -> Vec<ServerDirective> {
        (**self).encode_player_info_game_mode(entries)
    }

    // The three world-effect encoders and their dispatcher. Every one of them had
    // an emit-nothing default and no forward, so a boxed protocol — i.e. every
    // singleplayer session — produced **no sounds, no level events and no
    // particles at all**, silently, while a directly-owned protocol emitted them
    // normally. Same shape as `begin_play_at` above, and the same reason it went
    // unnoticed: the drain site calls `encode_world_effect` and gets a
    // `ServerDirective::None` that is indistinguishable from "nothing happened".
    fn encode_sound(
        &self,
        sound: &str,
        category: SoundCategory,
        pos: Vec3,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) -> ServerDirective {
        (**self).encode_sound(sound, category, pos, volume, pitch, seed)
    }

    fn encode_level_event(&self, event: i32, pos: BlockPos, data: i32, global: bool) -> ServerDirective {
        (**self).encode_level_event(event, pos, data, global)
    }

    fn encode_level_particles(
        &self,
        particle: &str,
        pos: Vec3,
        offset: Vec3f,
        max_speed: f32,
        count: i32,
        long_distance: bool,
    ) -> ServerDirective {
        (**self).encode_level_particles(particle, pos, offset, max_speed, count, long_distance)
    }

    // Forwarded even though the trait's own body is pure dispatch and would
    // already reach the inner protocol through the four forwards above: an
    // implementor that *does* override the dispatcher would otherwise have its
    // override skipped by the box, and the parity guard below requires a forward
    // for every trait method rather than reasoning about which ones are
    // redundant.
    fn encode_world_effect(&self, effect: &crate::effects::WorldEffect) -> ServerDirective {
        (**self).encode_world_effect(effect)
    }

    fn encode_player_combat_kill(&self, player_entity_id: i32, message: &Text) -> ServerDirective {
        (**self).encode_player_combat_kill(player_entity_id, message)
    }

    fn encode_dimension_change(
        &self,
        dimension: &str,
        spawn: Vec3,
        mode: GameMode,
    ) -> Vec<ServerDirective> {
        (**self).encode_dimension_change(dimension, spawn, mode)
    }

    fn encode_respawn(&self, spawn: Vec3) -> Vec<ServerDirective> {
        (**self).encode_respawn(spawn)
    }

    fn encode_recipe_book_add(
        &self,
        entries: &[crate::crafting::RecipeBookEntry],
        replace: bool,
    ) -> ServerDirective {
        (**self).encode_recipe_book_add(entries, replace)
    }

    fn worldgen_scope(&self) -> WorldgenScope {
        (**self).worldgen_scope()
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
        fn encode_registry_data(&self) -> Vec<ServerDirective> {
            vec![send(4)]
        }
        fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
            vec![send(100 + view_radius)]
        }
        /// Encodes the **spawn** into the answer, not just the view radius, so
        /// "the box forwarded `begin_play_at`" and "the box took the default,
        /// which discards `spawn` and calls `begin_play`" are different values.
        /// Without this override both sides would answer `send(100 + radius)` and
        /// the parity assertion would pass with the forward missing — which is
        /// exactly how #329's bug survived this test file.
        fn begin_play_at(&self, view_radius: i32, spawn: Vec3, mode: GameMode) -> Vec<ServerDirective> {
            let mode = match mode {
                GameMode::Survival => 0,
                GameMode::Creative => 1,
                GameMode::Adventure => 2,
                GameMode::Spectator => 3,
            };
            vec![send(
                300 + view_radius + spawn.x as i32 + spawn.y as i32 + spawn.z as i32 + mode,
            )]
        }
        // Overridden for the same reason `begin_play_at` is: both have
        // emit-nothing defaults, so a missing forward on the box would pass a
        // parity assertion built on the defaults.
        fn encode_game_mode(&self, _mode: GameMode) -> ServerDirective {
            send(401)
        }
        fn encode_player_abilities(&self, _abilities: Abilities) -> ServerDirective {
            send(402)
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
        fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
            send(700 + entity_id * 10 + fields.len() as i32)
        }
        fn encode_explode(&self, centre: Vec3, radius: f32) -> ServerDirective {
            send(800 + centre.x as i32 + radius as i32)
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
        fn encode_set_experience(&self, progress: f32, level: i32, total: i32) -> ServerDirective {
            let _ = progress;
            send(level * 10_000 + total)
        }
        fn encode_set_health(&self, health: f32, food: i32, saturation: f32) -> ServerDirective {
            let _ = saturation;
            send(health as i32 * 100 + food)
        }
        fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
            send(difficulty as i32 * 10 + i32::from(locked))
        }
        fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
            send(entries.len() as i32)
        }
        fn encode_game_event(&self, kind: u8, value: f32) -> ServerDirective {
            send(900 + i32::from(kind) * 100 + value as i32)
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
        fn encode_initialize_border(&self, border: &crate::border::WorldBorder) -> ServerDirective {
            send(1000 + border.size() as i32)
        }
        fn encode_set_border_center(&self, x: f64, z: f64) -> ServerDirective {
            send(1100 + x as i32 + z as i32)
        }
        fn encode_set_border_lerp_size(
            &self,
            old_size: f64,
            new_size: f64,
            lerp_time_ms: i64,
        ) -> ServerDirective {
            send(1200 + old_size as i32 + new_size as i32 + lerp_time_ms as i32)
        }
        fn encode_set_border_size(&self, size: f64) -> ServerDirective {
            send(1300 + size as i32)
        }
        fn encode_set_border_warning_delay(&self, warning_time: i32) -> ServerDirective {
            send(1400 + warning_time)
        }
        fn encode_set_border_warning_distance(&self, warning_blocks: i32) -> ServerDirective {
            send(1500 + warning_blocks)
        }
        fn encode_resource_pack_push(&self, push: &ResourcePackPush) -> ServerDirective {
            send(1600 + push.url.len() as i32 + push.hash.len() as i32 + i32::from(push.required))
        }
        fn encode_update_advancements(
            &self,
            update: &crate::advancements::AdvancementUpdate,
        ) -> ServerDirective {
            send(1700 + update.added.len() as i32 + update.removed.len() as i32 + i32::from(update.reset))
        }
        fn encode_award_stats(&self, stats: &[(crate::advancements::StatKey, i32)]) -> ServerDirective {
            send(1800 + stats.len() as i32)
        }
        fn encode_select_advancements_tab(&self, tab: Option<&str>) -> ServerDirective {
            send(1900 + tab.map_or(0, |t| t.len() as i32))
        }
        fn worldgen_scope(&self) -> WorldgenScope {
            // Non-default on purpose: this is the value that proves the box
            // forward works rather than both sides silently using the trait
            // default (the exact failure the `a_boxed_protocol_answers...`
            // control section exists to catch).
            WorldgenScope::V26_2
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
            metadata: Vec::new(),
            object_data: 0,
        }
    }

    /// Every [`ServerProtocol`] method must answer identically through a
    /// `Box<dyn ServerProtocol>` and through the concrete value.
    ///
    /// This is the control for the forwarding impl above, and the reason it is
    /// worth writing is that **most of the methods have defaults**: forgetting
    /// to forward one is not a compile error, it silently answers
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
        assert_eq!(
            boxed.encode_registry_data(),
            direct.encode_registry_data()
        );
        assert_eq!(boxed.begin_play(7), direct.begin_play(7));
        // The one this test was missing, and the reason #329's fix never reached
        // a player: `begin_play_at`'s default discards its `spawn` argument, so
        // an unforwarded box answers with the family's hardcoded literal instead.
        // `Numbered` overrides it, so the two sides differ unless the forward
        // exists.
        let spawn = Vec3::new(-101.0, 71.0, 202.0);
        assert_eq!(
            boxed.begin_play_at(7, spawn, GameMode::Creative),
            direct.begin_play_at(7, spawn, GameMode::Creative)
        );
        assert_eq!(
            boxed.encode_game_mode(GameMode::Creative),
            direct.encode_game_mode(GameMode::Creative)
        );
        let abilities = Abilities::for_mode(GameMode::Creative);
        assert_eq!(
            boxed.encode_player_abilities(abilities),
            direct.encode_player_abilities(abilities)
        );
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
        let fields = [MetadataField::CreeperSwellDir(1), MetadataField::CreeperIgnited(true)];
        assert_eq!(
            boxed.encode_set_entity_data(9, &fields),
            direct.encode_set_entity_data(9, &fields)
        );
        assert_eq!(
            boxed.encode_explode(Vec3::new(1.0, 2.0, 3.0), 3.0),
            direct.encode_explode(Vec3::new(1.0, 2.0, 3.0), 3.0)
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
        assert_eq!(
            boxed.encode_set_health(4.0, 20, 5.0),
            direct.encode_set_health(4.0, 20, 5.0)
        );
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
        assert_eq!(
            boxed.encode_game_event(7, 0.5),
            direct.encode_game_event(7, 0.5)
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
        let border = crate::border::WorldBorder::default();
        assert_eq!(
            boxed.encode_initialize_border(&border),
            direct.encode_initialize_border(&border)
        );
        assert_eq!(
            boxed.encode_set_border_center(1.0, 2.0),
            direct.encode_set_border_center(1.0, 2.0)
        );
        assert_eq!(
            boxed.encode_set_border_lerp_size(1000.0, 100.0, 20000),
            direct.encode_set_border_lerp_size(1000.0, 100.0, 20000)
        );
        assert_eq!(
            boxed.encode_set_border_size(512.0),
            direct.encode_set_border_size(512.0)
        );
        assert_eq!(
            boxed.encode_set_border_warning_delay(15),
            direct.encode_set_border_warning_delay(15)
        );
        assert_eq!(
            boxed.encode_set_border_warning_distance(5),
            direct.encode_set_border_warning_distance(5)
        );
        let push = ResourcePackPush {
            id: Uuid::nil(),
            url: "https://example.com/pack.zip".to_owned(),
            hash: "0123456789abcdef".to_owned(),
            required: true,
            prompt: None,
        };
        assert_eq!(
            boxed.encode_resource_pack_push(&push),
            direct.encode_resource_pack_push(&push)
        );
        let advancement_update = crate::advancements::AdvancementUpdate {
            reset: true,
            added: vec![crate::advancements::Advancement::new(
                "minecraft:story/root",
                vec![vec!["crafting_table".to_string()]],
                true,
            )],
            removed: vec!["minecraft:story/removed".to_string()],
            progress: Vec::new(),
            show_advancements: true,
        };
        assert_eq!(
            boxed.encode_update_advancements(&advancement_update),
            direct.encode_update_advancements(&advancement_update)
        );
        let stats = [(
            crate::advancements::StatKey::new(crate::advancements::StatType::Mined, "minecraft:stone"),
            3,
        )];
        assert_eq!(
            boxed.encode_award_stats(&stats),
            direct.encode_award_stats(&stats)
        );
        assert_eq!(
            boxed.encode_select_advancements_tab(Some("minecraft:story/root")),
            direct.encode_select_advancements_tab(Some("minecraft:story/root"))
        );
        assert_eq!(
            boxed.encode_select_advancements_tab(None),
            direct.encode_select_advancements_tab(None)
        );
        assert_eq!(
            boxed.worldgen_scope(),
            direct.worldgen_scope(),
            "the box must forward worldgen_scope, not answer with the trait default"
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
        assert_ne!(direct.encode_game_event(7, 0.5), ServerDirective::None);
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
        assert_ne!(direct.encode_set_entity_data(9, &fields), ServerDirective::None);
        assert_ne!(
            direct.encode_explode(Vec3::new(1.0, 2.0, 3.0), 3.0),
            ServerDirective::None
        );
        assert_ne!(direct.encode_initialize_border(&border), ServerDirective::None);
        assert_ne!(
            direct.encode_set_border_center(1.0, 2.0),
            ServerDirective::None
        );
        assert_ne!(
            direct.encode_set_border_lerp_size(1000.0, 100.0, 20000),
            ServerDirective::None
        );
        assert_ne!(direct.encode_set_border_size(512.0), ServerDirective::None);
        assert_ne!(
            direct.encode_set_border_warning_delay(15),
            ServerDirective::None
        );
        assert_ne!(
            direct.encode_set_border_warning_distance(5),
            ServerDirective::None
        );
        assert_ne!(direct.encode_resource_pack_push(&push), ServerDirective::None);
        assert_ne!(
            direct.encode_update_advancements(&advancement_update),
            ServerDirective::None
        );
        assert_ne!(direct.encode_award_stats(&stats), ServerDirective::None);
        assert_ne!(
            direct.encode_select_advancements_tab(Some("minecraft:story/root")),
            ServerDirective::None
        );
        assert_ne!(direct.encode_select_advancements_tab(None), ServerDirective::None);
        assert_ne!(
            direct.worldgen_scope(),
            WorldgenScope::None,
            "worldgen_scope answered with the trait default through the box, so the \
             forward is missing and a boxed v770 would silently report 'no worldgen'"
        );
    }

    /// The names of every item-level `fn` inside the top-level item whose
    /// declaration line starts with `anchor`.
    ///
    /// Deliberately crude, and deliberately not a Rust lexer — this repo has
    /// already paid for one of those being wrong about lifetimes. Two facts make
    /// line-shape matching sufficient here, and both are properties of this file
    /// rather than of Rust: a top-level item's closing brace is the only `}` that
    /// ever appears alone at column 0, and a direct member of that item is the
    /// only `fn` that ever appears at exactly four spaces of indent. A `}` inside
    /// a doc comment (this file has fenced Java in one) is prefixed by `///`, and
    /// a nested `fn` inside a default body is indented further.
    fn item_level_fn_names(source: &str, anchor: &str) -> Vec<String> {
        let mut lines = source.lines().skip_while(|l| !l.starts_with(anchor));
        assert!(
            lines.next().is_some(),
            "anchor {anchor:?} matched no line — the parser found nothing, which is a \
             failure to run and not a pass"
        );
        lines
            .take_while(|l| *l != "}")
            .filter_map(|l| {
                let rest = l.strip_prefix("    ")?;
                if rest.starts_with(' ') {
                    return None;
                }
                let rest = rest.strip_prefix("pub ").unwrap_or(rest);
                let name = rest.strip_prefix("fn ")?;
                let end = name.find(|c: char| !c.is_ascii_alphanumeric() && c != '_')?;
                Some(name[..end].to_owned())
            })
            .collect()
    }

    /// **Every [`ServerProtocol`] method has a forward in the `Box<P>` impl.**
    ///
    /// The impl's own doc comment has asked for this in prose since it was
    /// written, and prose is not a rule: at the time this guard was added the box
    /// was missing **eleven** forwards, including all three world-effect encoders
    /// — so every singleplayer session emitted no sounds, no level events and no
    /// particles, silently, because an unforwarded defaulted method answers
    /// `ServerDirective::None` and that is indistinguishable from "nothing
    /// happened".
    ///
    /// `a_boxed_protocol_answers_exactly_as_the_concrete_one_does` above cannot
    /// see this class of gap: it compares two things by a hand-written list, so a
    /// *third* thing — a method nobody added to the list — is invisible to it.
    /// This one enumerates instead of listing.
    #[test]
    fn every_server_protocol_method_is_forwarded_by_the_box_impl() {
        let source = include_str!("protocol.rs");
        let trait_fns = item_level_fn_names(source, "pub trait ServerProtocol");
        let box_fns =
            item_level_fn_names(source, "impl<P: ServerProtocol + ?Sized> ServerProtocol for Box<P>");

        // The floor is a measurement, not a guess: the trait had 64 methods when
        // this guard landed. Without it, an anchor that stopped matching would
        // compare two empty sets and report green — the vacuous-precondition
        // species, and the one this whole test exists to rule out.
        assert!(
            trait_fns.len() >= 60,
            "parsed only {} trait methods (64 when this guard landed) — the anchor or \
             the region scan has drifted, so this gate is measuring nothing",
            trait_fns.len()
        );
        assert!(
            box_fns.len() >= 60,
            "parsed only {} forwards in the Box impl — see above",
            box_fns.len()
        );

        // Collected rather than asserted in the loop: an `assert!` inside the
        // iteration reports one missing forward and leaves the rest as arguments,
        // so a neuter would demonstrate a single arm instead of all eleven.
        let missing: Vec<&String> = trait_fns.iter().filter(|f| !box_fns.contains(f)).collect();
        assert!(
            missing.is_empty(),
            "{} ServerProtocol method(s) have no forward in `impl ServerProtocol for Box<P>`: \
             {missing:?}. Each one silently answers the trait's default (usually \
             ServerDirective::None) for every boxed protocol, i.e. for every singleplayer \
             session, while a directly-owned protocol keeps working.",
            missing.len()
        );

        let stray: Vec<&String> = box_fns.iter().filter(|f| !trait_fns.contains(f)).collect();
        assert!(
            stray.is_empty(),
            "the Box impl forwards {stray:?}, which the trait does not declare — either a \
             rename left a stale arm behind or the region scan is reading the wrong item"
        );
    }
}
