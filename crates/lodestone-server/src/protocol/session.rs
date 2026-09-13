//! Version-free state records carried across the server protocol seam.
//!
//! These records describe session-visible state without encoding packet ids or wire layouts.

use lodestone_model::{BlockPos, GameMode, ResourceKey, Rotation, Text, Vec3};
use uuid::Uuid;

use super::ResourcePackUrl;

/// The local player's movement abilities — the real per-player abilities
/// record, as
/// carried by the player-abilities packet.
///
/// This is the packet that actually grants creative flight and instant build.
/// A client told "you are in creative" through
/// [`ServerProtocol::encode_game_mode`] alone still cannot fly, because
/// permission lives here — which is why the real set-game-mode step sends both.
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
    /// spectator (the real is-block-placing-restricted query).
    pub may_build: bool,
    /// Flight speed multiplier; the real default is `0.05`.
    pub flying_speed: f32,
    /// Walk speed multiplier; the real default is `0.1`.
    pub walking_speed: f32,
}

impl Abilities {
    /// The real abilities record's field defaults.
    pub const DEFAULT_FLYING_SPEED: f32 = 0.05;
    /// See [`DEFAULT_FLYING_SPEED`](Self::DEFAULT_FLYING_SPEED).
    pub const DEFAULT_WALKING_SPEED: f32 = 0.1;

    /// Constructs initial abilities with default speeds. Creative starts
    /// grounded; spectator starts flying. Use [`Self::set_game_mode`] for a
    /// live transition that must retain permitted flight and configured speeds.
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

    /// Applies a mode change without discarding the current flight state or
    /// configured speeds. Creative preserves flight; spectator forces it on;
    /// survival and adventure revoke it.
    pub fn set_game_mode(&mut self, mode: GameMode) {
        let previous = *self;
        *self = Self::for_mode(mode);
        if mode == GameMode::Creative {
            self.flying = previous.flying;
        }
        self.flying_speed = previous.flying_speed;
        self.walking_speed = previous.walking_speed;
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
    /// client to hold — empty for every entity kind that has
    /// none (projectiles, dropped items, and any mob whose fields are all
    /// still at their default). [`crate::server::EntityStreamer::sync`]
    /// diffs this exactly like every other field on this struct: a spawn
    /// with non-empty metadata, or an update where this changed, calls
    /// [`ServerProtocol::encode_set_entity_data`] with the entity's *current*
    /// full field list (not just what changed) — see that method's own doc
    /// comment for why resending the full set is the simpler and cheap
    /// choice here.
    pub metadata: Vec<MetadataField>,
    /// The `ADD_ENTITY` **Object Data** field — the real engine's own name for the
    /// trailing VarInt on the spawn packet, whose meaning is decided entirely by
    /// the entity type.
    ///
    /// `0` for everything that does not override
    /// the real add-entity-packet builder's data argument,
    /// which is every entity kind this server spawns except one:
    /// the real falling-block entity's own override passes
    /// the block-state id of the state it imitates.
    ///
    /// This is a **spawn-only** field and deliberately not part of the update
    /// path: the real engine sends it once, in `ADD_ENTITY`, and has no packet that
    /// revises it. It is still compared by this struct's `PartialEq`, so a value
    /// that somehow changed mid-life would produce a redundant position update
    /// rather than silently disagreeing with what the client holds.
    ///
    /// # Why the block state cannot ride `metadata` instead
    ///
    /// The real falling-block entity's own synced-data definition registers a
    /// start-position field and
    /// nothing else — the imitated block state is **never** in a `SET_ENTITY_DATA`
    /// packet. So a client that is not told this field has no other source, and
    /// draws whatever state id `0` resolves to. That is the same failure shape as
    /// a dropped item with no reported stack: every wire green, the wrong value
    /// travelling it.
    pub object_data: i32,
    /// The wire entity id this entity is leashed to, or `None` when it carries no
    /// lead — the leashable entity's lead-data holder field, resolved to
    /// an id by [`crate::mobs::MobSim::snapshots`] (a player uuid resolves through
    /// the connected-player list; a leashed mob resolves to its own already-wire
    /// id; a fence-knot holder has no entity to resolve to yet and stays `None` —
    /// see `LeashHolder::Fence`'s own doc comment).
    ///
    /// Diffed by [`crate::server::EntityStreamer::sync`] exactly like `metadata`:
    /// a spawn with `Some` emits [`ServerProtocol::encode_set_entity_link`] right
    /// after the `ADD_ENTITY` (and its metadata, if any), and an update where this
    /// changed emits it again. That spawn-time emission is what makes an
    /// already-leashed mob show its rope to a client that joins or re-enters view
    /// range late — not just to whoever witnessed the attach.
    pub leash_link: Option<i32>,
}

/// One boss bar this world wants a client to hold, keyed by [`id`](Self::id) —
/// the version-free input to [`ServerProtocol::encode_boss_event_add`]/
/// [`encode_boss_event_update_progress`](ServerProtocol::encode_boss_event_update_progress)/
/// [`encode_boss_event_remove`](ServerProtocol::encode_boss_event_remove),
/// diffed by [`crate::server::EntityStreamer`] the same way [`EntitySnapshot`]
/// is: a fresh `id` sends ADD, a changed `progress`/`visible` sends
/// UPDATE_PROGRESS (or REMOVE, once `visible` goes `false` — the real
/// boss-event packet has no wire "visible" flag; visibility is
/// spelled by whether the bar is on the client at all, exactly as
/// the real per-world boss-event tracker's own player-set add/remove does), and a vanished `id`
/// sends REMOVE.
///
/// Color (`PINK`) and overlay (`PROGRESS`) are not fields here because the
/// real engine
/// never varies them for the one producer today (the real dragon-fight
/// init step, per `crate::dragon::fight`'s own module doc) — an implementor hardcodes them once,
/// same as that constructor does.
#[derive(Debug, Clone, PartialEq)]
pub struct BossBarSnapshot {
    /// The bar's stable id — **not** necessarily the boss entity's own uuid on
    /// the wire (the real engine mints a separate insecure random UUID for
    /// the dragon fight's bar); [`crate::mobs::MobSim::boss_bars`] reuses the
    /// dragon's entity uuid as a documented simplification, since this crate
    /// tracks one bar per dragon and nothing needs the two identities to
    /// differ.
    pub id: Uuid,
    /// The bar's title, e.g. `Text::translate("entity.minecraft.ender_dragon", vec![])`.
    pub name: Text,
    /// `health / max_health`, clamped to `[0.0, 1.0]` — see
    /// [`crate::dragon::fight::boss_bar_value`].
    pub progress: f32,
    /// Whether the bar should currently be shown at all — `false` once the
    /// boss is dead. See this struct's own doc for why that is spelled as
    /// add/remove on the wire rather than a packet field.
    pub visible: bool,
}

/// One connected player as the tab list carries them — the
/// version-free vocabulary
/// [`ServerProtocol::encode_player_info_add`] takes a slice of.
///
/// Only the two fields `ADD_PLAYER` cannot do without. The real
/// per-player-info-update entry also carries a game mode, a
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

/// A server-initiated resource pack push (the real resource-pack-push
/// packet) in version-free vocabulary — the
/// server-side representation of a push, fed by
/// [`ServerProtocol::encode_resource_pack_push`].
///
/// Mirrors the wire record exactly: a fresh per-push [`Uuid`] the client
/// echoes back verbatim in its accept/decline response, the download [`url`],
/// the pack's SHA-1 [`hash`] (lowercase hex, at most 40 chars — the real
/// packet's own max-hash-length constant), the [`required`]
/// flag that makes declining a disconnect, and an optional [`prompt`] chat
/// component shown on the accept/decline screen. The URL is parsed before it
/// crosses this version-free seam; the version adapter converts it back to the
/// wire string at its packet boundary. `hash` remains an owned `String`
/// because its empty-or-hex shape is a wire compatibility rule rather than a
/// URL-like identity.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourcePackPush {
    /// The push's own uuid — the real engine generates a fresh one per push, and the
    /// client's response echoes it (see
    /// `crate::server`'s decode of the serverbound `RESOURCE_PACK` frame).
    pub id: Uuid,
    /// The pack's download URL.
    pub url: ResourcePackUrl,
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
/// `SET_ENTITY_DATA` — the general vocabulary
/// [`ServerProtocol::encode_set_entity_data`] takes a slice of, replacing
/// the single hardcoded local-player arm (`encode_air_supply_update`) that
/// used to be the only metadata encoder anywhere in this crate. Adding a
/// field for the next mob is a new variant here plus one arm in the
/// implementor's `encode_set_entity_data` — no second mechanism, and no
/// change to [`EntityStreamer::sync`](crate::server) at all, since that
/// diffing loop already treats `EntitySnapshot::metadata` generically.
///
/// Each variant names the real field it mirrors, not the wire index or
/// serializer id — those are the implementor's concern (see
/// `crates/protocol/v770/src/server_protocol.rs`'s own constants, verified
/// against the `EntityDataIndexOracle` dump the same way
/// `crates/protocol/v770/src/packets/metadata.rs`'s decode-side constants
/// already are), matching every other version-free `Server*`/`Client*`
/// vocabulary type in this crate.
/// # Why this enum is deliberately **not** `Copy`
///
/// This enum is not `Copy` because [`Item`](Self::Item) carries an owned
/// [`ResourceKey`]. A version-free
/// vocabulary enum that derives `Copy` silently forbids every future field that
/// carries an owned value, and the cost surfaces only at the first feature that
/// needs one — here, the whole of "a dropped item draws at all". Keep it
/// non-`Copy`: the only cost is that an implementor's `match` is by reference
/// (`match field`, not `match *field`), and that is one character per
/// implementor. See DESIGN.md §12.116.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataField {
    /// The base-entity shared-flags byte. The producer owns its bits: this
    /// generic wire field is valid for any entity type, so an unrelated
    /// feature must not reuse it without preserving every bit it does not
    /// own.
    SharedFlags(u8),
    /// The real creeper's own swell-direction field — which way `swell` is currently moving
    /// (`-1`, `0`, or `1`). See [`crate::mobs::SimMob::snapshot`]'s own doc
    /// comment for why this is always included for a creeper, even at its
    /// `-1` default, unlike the monotonic [`CreeperIgnited`](Self::CreeperIgnited).
    CreeperSwellDir(i32),
    /// The real creeper's own is-ignited field — set once by its ignite step, never cleared.
    CreeperIgnited(bool),
    /// The real item entity's own item field — the stack a dropped item entity is showing,
    /// and the *whole* of its visible identity.
    ///
    /// A client draws nothing for an item entity whose stack it has not been
    /// told: the real item-entity renderer returns early on
    /// an empty item state, and this project's own client does the same (see
    /// `EntityInterpolator::set_item_stack`). So an item entity streamed
    /// without this field spawns, falls, merges and can be picked up — every
    /// one of which is observable — while drawing zero pixels. That is why it
    /// is one field and not an optimisation.
    ///
    /// `count` is the *entity's* stack size (the real item-lifecycle
    /// count), not the number of entities.
    Item {
        /// The item's registry key, e.g. `minecraft:diamond`. **Not** an
        /// entity type — see [`crate::mobs::MobSim::snapshots`] for the
        /// `minecraft:acacia_boat` bug that confusing the two produced.
        item: ResourceKey,
        /// Stack size. `0` is the empty stack, which a client renders as
        /// nothing — the same as sending no field at all.
        count: u8,
    },
    /// The real experience orb's own value field — the points **one** absorption of this orb pays
    /// out, and the whole of what a client is told about an orb.
    ///
    /// It is what selects the sprite: the real orb-icon derivation buckets the value into
    /// eleven frames at the same thresholds as the denomination ladder, so an orb whose
    /// value never arrives draws frame 0 — the smallest — however much it is worth. The
    /// orb's `count` (how many absorptions it holds after merging) is deliberately not
    /// here, because the real engine does not synchronise it and one entity draws one sprite
    /// whatever its count.
    ///
    /// **Index 8, shared with [`Item`](Self::Item) under a different serializer.**
    /// The real orb value field is an `INT` and the real item field an `ITEM_STACK` at the same
    /// index; the encoder can tell them apart only because the field list is built per
    /// entity kind by [`crate::mobs::MobSim::snapshots`], whose orb loop iterates the
    /// orb map. Never push this variant for anything but an experience orb.
    ExperienceOrbValue {
        /// Points per absorption, the real orb's own value query.
        value: i32,
    },
    /// The real tamable-animal's own flags field — the wolf/cat/parrot flag byte at index
    /// **18**, whose `0x01` bit is the sitting pose and `0x04` bit is tameness.
    ///
    /// # Why this is not shared with [`HorseFlags`](Self::HorseFlags)
    ///
    /// Index 18 is the most crowded index in the game — 37 claimants in the
    /// committed jar dump (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`),
    /// of which **four** are the `BYTE` serializer: the tamable animal's own
    /// flags field, the
    /// abstract horse's own flags field, the sheep's own wool-color field and
    /// the shulker's own color field. No census column separates them; the *producer*
    /// has to know the species, which is why the species switch lives in
    /// [`crate::mobs::SimMob::snapshot`] and never in an encoder.
    ///
    /// And the bit differs: tame is `0x04` here and the horse's own tame bit
    /// is `2`. A
    /// single shared variant therefore sets an **unnamed** bit on whichever species
    /// it was not written for — `0x04` is not in the horse's flag set at all
    /// (its own bred bit is `8`), and `0x02` is not in the tamable's — so the animal
    /// reads as *untamed* while the packet looks correct on the wire. That is worse
    /// than a wrong flag, because there is nothing visibly wrong to chase.
    TamableFlags {
        /// `0x04` — the real tamable animal's own is-tame query.
        tame: bool,
        /// `0x01` — the real tamable animal's own is-in-sitting-pose query, the pose
        /// its own sit-when-ordered goal writes, **not** the persisted "ordered to sit" flag.
        sitting: bool,
    },
    /// The real abstract horse's own flags field — the horse family's own flag byte, also at
    /// index 18. See [`TamableFlags`](Self::TamableFlags) for why this is a
    /// separate variant.
    ///
    /// Only the real tame bit (`0x02`) is modelled. The real bred bit (`0x08`), eating bit
    /// (`0x10`), standing bit (`0x20`) and open-mouth bit (`0x40`) have no
    /// server-side state to drive them yet — the values are transcribed from
    /// the real abstract horse's own constants so the next one to be wired does not have to
    /// be looked up again.
    HorseFlags {
        /// `0x02` — the real abstract horse's own is-tamed query.
        tame: bool,
    },
    /// Whether this mob is a baby — the real ageable-mob's own baby field for the
    /// breedable-animal family (cow, sheep, pig, chicken, rabbit, wolf), and
    /// each of the zombie's and zoglin's own baby fields declared
    /// separately on those classes rather than inherited from the ageable
    /// base
    /// (zombie/zoglin extend the monster base, not the ageable one) — all three
    /// land at the same wire index as the same `BOOLEAN` serializer, which is
    /// what lets one variant cover every eligible species; see
    /// [`crate::mobs::SimMob::snapshot`] for the species switch that decides
    /// who gets it. The real living-entity renderer reads it to apply the
    /// age-scale shrink (`0.5` generic, or the species' real
    /// baby-dimensions literal where one is modelled) to the model,
    /// independently of the hitbox — this crate's aging unit already
    /// computes the correct **hitbox** dimensions server-side
    /// (`crate::mobs::species_shape`); this variant is what lets the
    /// *client* apply the same shrink to what it draws.
    Baby(bool),
    /// The real villager's own villager-data field — index **19**, serializer
    /// `VILLAGER_DATA` (`18`): a villager type plus a villager profession
    /// plus a plain level int, which is the *whole* of what a client's
    /// villager-profession texture layer needs to pick a texture. Pushed
    /// unconditionally for every `minecraft:villager` (see
    /// [`crate::mobs::SimMob::snapshot`]'s doc for why — the same "a
    /// transition needs the same treatment as the arrival" reasoning
    /// [`Baby`](Self::Baby) is pushed unconditionally for).
    VillagerData {
        /// `minecraft:villager_type`, e.g. `minecraft:plains`. Always
        /// `minecraft:plains` today — see `crate::mobs::villager`'s module
        /// doc for why biome-derived type is out of scope.
        kind: ResourceKey,
        /// `minecraft:villager_profession`, e.g. `minecraft:farmer`.
        /// `minecraft:none` for an unemployed villager.
        profession: ResourceKey,
        /// The real villager-data record's own level field, `1..=5`.
        level: i32,
    },
    /// The real primed-tnt entity's own fuse field — ticks remaining before detonation.
    ///
    /// **Index 8, a fifth claimant alongside [`Item`](Self::Item) and
    /// [`ExperienceOrbValue`](Self::ExperienceOrbValue).** The committed jar
    /// dump (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`)
    /// lists five `INT`/`ITEM_STACK` claimants at index 8: the real
    /// experience orb's own value field, the real primed-tnt's own fuse field,
    /// the real fishing hook's own hooked-entity field, the real vehicle
    /// entity's own hurt-id field and
    /// a display entity's interpolation-delay field, plus the real item
    /// entity's own item field
    /// under the self-identifying `ITEM_STACK` serializer. Never push this
    /// variant for anything but a `minecraft:tnt` entity — see
    /// [`crate::mobs::MobSim::snapshots`]'s TNT loop, the only producer.
    TntFuse(i32),
    /// The real furnace minecart's own fuel field — whether the furnace minecart is
    /// currently lit (real remaining fuel), the field that drives the smoke
    /// particle client-side.
    ///
    /// **Index 13**, shared with the real command-block minecart's own
    /// command-name field
    /// (a `STRING`) under a different serializer — the committed jar dump
    /// (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`) lists
    /// both. The two can never collide in practice: only
    /// [`crate::mobs::MobSim::snapshots`]'s furnace-minecart arm ever builds
    /// this variant, and it never fires for a command-block minecart (this
    /// crate does not model that entity type).
    MinecartFuel(bool),
    /// The real abstract boat's own left/right paddle fields — purely
    /// cosmetic: whether each paddle is currently animating. This is the
    /// remaining paddle-state data — a second connected player watching a rowed
    /// boat from outside is the only consumer, since the rider's own client
    /// always animates its paddles from local input regardless of what this
    /// crate streams back (`crate::mobs::vehicles::MobSim::apply_boat_paddle`'s
    /// own doc).
    ///
    /// **Indices 11 and 12**, each a two-way `BOOLEAN` collision in the
    /// committed jar dump (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`):
    /// index 11 is the real abstract boat's own left-paddle field and
    /// the real living entity's own effect-ambience field; index 12 is
    /// the real abstract boat's own right-paddle field and the real thrown
    /// trident's own foil field. No
    /// census column separates them, so — the same rule
    /// [`TamableFlags`](Self::TamableFlags) states — the guard is the
    /// producer's own species knowledge: only
    /// [`crate::mobs::MobSim::snapshots`]'s vehicle loop ever builds this
    /// variant, and every entry in that loop is a boat (`TrackedVehicle`
    /// carries no other species), never a `LivingEntity` or a
    /// `ThrownTrident`.
    BoatPaddles {
        /// Index 11.
        left: bool,
        /// Index 12.
        right: bool,
    },
    /// Vanilla's own vehicle-entity hurt-time/hurt-direction/damage synced-data trio — the
    /// rocking triple every boat, raft and minecart carries. Together they are
    /// the whole of the animation a punched hull plays: the client rolls the
    /// model by `sin(time) * time * damage / 10 * dir` about its own X axis.
    ///
    /// **Indices 8, 9 and 10.** Index 8's `INT` has five claimants in the
    /// committed jar dump (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`)
    /// — an experience orb's value, a primed TNT's fuse, a fishing hook's hooked
    /// entity and a display entity's interpolation delay alongside this one —
    /// and index 9's has two. None of them is a `LivingEntity`, so no census
    /// column separates them and the guard is the same one
    /// [`BoatPaddles`](Self::BoatPaddles) states: the *producer*'s own species
    /// knowledge. Only [`crate::mobs::MobSim::snapshots`]'s vehicle loop ever
    /// builds this variant, and every entry in that loop is a boat.
    ///
    /// `dir`'s resting value is **`1`**, not `0` — vanilla's own
    /// vehicle-entity synced-data-definition registered default — and it multiplies the whole
    /// roll, so a `0` here draws a still boat rather than an unhurt one.
    VehicleHurt {
        /// Index 8: ticks remaining, `10` at the moment of the hit.
        time: i32,
        /// Index 9: `+1` or `-1`.
        dir: i32,
        /// Index 10: accumulated damage x 10.
        damage: f32,
    },
    /// Vanilla's own ender-dragon phase synced-data field — the dragon's current
    /// `crate::dragon::phase::Phase` id (`Phase::id`), the wire twin of the
    /// state [`crate::mobs::MobSim::tick_dragons`] already drives for real.
    ///
    /// **Index 16, one of six `INT` claimants** in the committed jar dump
    /// (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`):
    /// the creeper's own swell-direction field, the display entity's own
    /// brightness-override field, the ender dragon's own phase field, the
    /// phantom's own size field, the warden's own client-anger-level field,
    /// and the wither's own target-A field (index 16 also carries the enderman's own carry-state field,
    /// but that one is `OPTIONAL_BLOCK_STATE`, a different serializer). The
    /// serializer alone cannot separate one `INT` claimant from another, so
    /// only the producer — `MobSim::push_dragon_snapshots`, the sole caller —
    /// disambiguates; never push this variant for anything but a
    /// `minecraft:ender_dragon`.
    DragonPhase(i32),
    /// Vanilla's own end-crystal beam-target synced-data field — where the crystal's healing/summoning
    /// beam points, or `None` for no beam (vanilla's own default).
    ///
    /// **Index 8, serializer `OPTIONAL_BLOCK_POS`** — self-identifying at that
    /// index (no other index-8 claimant in the jar dump uses
    /// `OPTIONAL_BLOCK_POS`; the other seventeen are `BYTE`/`FLOAT`/`INT`/
    /// `ITEM_STACK`/`BLOCK_POS`/`DIRECTION`/`BOOLEAN`), but `OPTIONAL_BLOCK_POS`
    /// itself is **not** globally self-identifying — the same serializer is
    /// also the base living-entity's own sleeping-position field at index 14 and the creaking's own home-position field
    /// at index 19, so a decoder must still key on the index, not the
    /// serializer alone.
    ///
    /// **No producer sets `Some` yet.** This crate has no obsidian pillars
    /// anywhere (`crate::dragon::fight`'s module doc) and no respawn sequence
    /// wired to a real crystal (`crate::dragon::fight::tick_respawn`'s
    /// `SetBeamTarget`/`ClearBeamTarget`/`AimAtSpike` events reach no world),
    /// so every live crystal streams `None` today — a real, disclosed gap
    /// (matching [`crate::mobs::MobSim::damage_dragon`]'s own "not yet wired
    /// to a real hit" precedent), not a silent stub.
    CrystalBeamTarget(Option<BlockPos>),
    /// Vanilla's own end-crystal show-bottom synced-data field — whether the crystal draws its bedrock
    /// base, `true` unless it is a caged crystal on an obsidian spike.
    ///
    /// **Index 9, one of three `BOOLEAN` claimants** in the jar dump:
    /// the area-effect-cloud's own waiting field, the end-crystal's own show-bottom field,
    /// and the fishing-hook's own biting field — the serializer does not separate them, so
    /// only the producer (`MobSim::push_end_crystal_snapshots`) disambiguates.
    /// Always `true` in this crate today: there are no obsidian pillars and so
    /// no caged crystal is ever spawned (see [`CrystalBeamTarget`](Self::CrystalBeamTarget)'s
    /// own doc) — a real field, pushed unconditionally, whose value simply
    /// never varies yet.
    CrystalShowBottom(bool),
    /// Vanilla's own base-entity pose synced-data field — index **6**, the one and only `POSE`-serializer
    /// claimant in the jar dump (`entity_data_index_jvm.txt`), so no species
    /// switch is needed to disambiguate it the way index 8 or 18 need one.
    ///
    /// The raw `Pose` ordinal (vanilla's own pose-id getter), not a
    /// version-free enum — this crate has no general per-mob pose model yet
    /// (the warden dig/emerge behavior is the first producer), so the id is
    /// carried through verbatim rather than inventing a vocabulary for the
    /// other seventeen values nothing here produces. `13` (`EMERGING`) and
    /// `14` (`DIGGING`) are the two this crate currently ever sends; `0`
    /// (`STANDING`) is vanilla's own default.
    ///
    /// **Pushed unconditionally for a warden**, not only while non-standard —
    /// the same "the reset needs to reach the client too" reasoning
    /// [`CreeperSwellDir`](Self::CreeperSwellDir)'s own doc gives:
    /// `SET_ENTITY_DATA` is a sparse update, so a snapshot that stops
    /// including this field the tick emerging ends would leave a client that
    /// received the `13` believing the warden is stuck emerging forever.
    Pose(u32),
    /// Vanilla's own wither-boss invulnerable-ticks synced-data field — the invulnerable "emerging" countdown
    /// (`crate::wither::INVULNERABLE_TICKS` down to `0`) that drives the
    /// client-side shield visual while a freshly-summoned wither is still
    /// rising out of the ground.
    ///
    /// **Index 19, one of six `INT` claimants** in the committed jar dump
    /// (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`):
    /// the dolphin's own moistness-level field, the horse's own type-variant field,
    /// the panda's own sneeze-counter field, the sniffer's own drop-seed-at-tick field,
    /// the magma-cube's own max-fuse field, and the wither's own invulnerable-ticks field (index 19 also carries
    /// several non-`INT` claimants such as the armor-stand's own right-arm-pose field
    /// and the tamable-animal's own owner-uuid field, which a different serializer
    /// already separates). The serializer alone cannot separate one `INT`
    /// claimant from another, so only the producer disambiguates, exactly as
    /// [`DragonPhase`](Self::DragonPhase) documents for its own index; never
    /// push this variant for anything but a `minecraft:wither`.
    WitherInvulnerableTicks(i32),
    /// Vanilla's own goat has-left-horn/has-right-horn synced-data fields — the two fields
    /// its own renderer reads to hide a broken horn's cuboid.
    ///
    /// **Indices 19 and 20**, each one of a large `BOOLEAN` claimant set in
    /// the committed jar dump
    /// (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`):
    /// index 19 alone has thirteen non-`BOOLEAN` claimants
    /// (the villager's own villager-data field, the tamable-animal's own owner-uuid field, …)
    /// plus several other `BOOLEAN` ones (the camel's own dash field,
    /// the axolotl's own playing-dead field, the strider's own suffocating field, …); index 20
    /// is the same shape. The serializer alone cannot separate a goat's own
    /// pair from any of those, so only the producer
    /// ([`crate::mobs::SimMob::snapshot`]'s `"goat"` arm, the sole caller)
    /// disambiguates — never push this variant for anything but a
    /// `minecraft:goat`.
    ///
    /// Pushed unconditionally for every goat (both fields `true` is the
    /// common case), matching [`Baby`](Self::Baby)'s own "a transition needs
    /// the same treatment as the arrival" reasoning — though in this crate
    /// today there is no mid-game transition to reach: the value is decided
    /// once at spawn (vanilla's own goat finalize-spawn routine's pre-broken-horn roll,
    /// `crate::mobs::goat_horn_spawn_roll`) and never changes afterward,
    /// since `RamTarget`'s own doc discloses that the block-contact
    /// horn-breaking trigger is not ported (no block-state read on that
    /// seam). A real, disclosed narrowing: a goat can spawn missing a horn,
    /// but cannot yet lose one mid-game here.
    GoatHorns {
        has_left: bool,
        has_right: bool,
    },
    /// Vanilla's own axolotl playing-dead synced-data field — index 19, one of the `BOOLEAN`
    /// claimants [`GoatHorns`](Self::GoatHorns)'s own doc already names at
    /// that index. The producer ([`crate::mobs::SimMob::snapshot`]'s
    /// `"axolotl"` arm, the sole caller) disambiguates it from every other
    /// claimant, the same shape every crowded-index field in this enum
    /// already uses; never push this variant for anything but a
    /// `minecraft:axolotl`.
    ///
    /// Pushed unconditionally for every axolotl, matching
    /// [`GoatHorns`](Self::GoatHorns)'s own "the reset must reach the client
    /// too" reasoning: an axolotl that stops playing dead must send `false`,
    /// not merely stop sending `true`. Backed by
    /// [`crate::mobs::SimMob::axolotl_is_playing_dead`], itself
    /// vanilla's own axolotl hurt-server routine's own trigger collapsed to a plain countdown —
    /// see that method's own doc for the roll and the disclosed narrowings.
    PlayingDead(bool),
    /// Vanilla's own camel dash synced-data field — index 19, one of the `BOOLEAN` claimants
    /// [`GoatHorns`](Self::GoatHorns)'s own doc already names at that index.
    /// The producer ([`crate::mobs::SimMob::snapshot`]'s `"camel"` arm, the
    /// sole caller) disambiguates it from every other claimant, the same
    /// shape every crowded-index field in this enum already uses; never
    /// push this variant for anything but a `minecraft:camel`.
    ///
    /// Pushed unconditionally for every camel, matching
    /// [`PlayingDead`](Self::PlayingDead)'s own "the reset must reach the
    /// client too" reasoning. Backed by
    /// [`crate::mobs::SimMob::camel_is_dashing`] — see that method's own
    /// doc for the disclosed narrowing (no `onGround` signal exists for a
    /// client-authoritative mount, so the reported window is
    /// vanilla's own camel dash-minimum-duration-ticks constant rather than the real
    /// landing-triggered one).
    Dash(bool),
    /// Vanilla's own sniffer state synced-data field — index 18, the crate's own `SNIFFER_STATE`
    /// serializer (id 35), not a reused generic one. The producer
    /// ([`crate::mobs::SimMob::snapshot`]'s `"sniffer"` arm, the sole
    /// caller) is the only thing that ever pushes this; never push it for
    /// anything but a `minecraft:sniffer`.
    ///
    /// Carries [`crate::mobs::sniffer::SnifferState::wire_ordinal`]'s
    /// output directly — the real sniffer-state enum ordinal, not a
    /// crate-local renumbering. Pushed unconditionally for every sniffer,
    /// matching [`Dash`](Self::Dash)'s own "the reset must reach the client
    /// too" reasoning.
    SnifferState(u8),
}

/// One generated trade offer, ready for the wire —
/// [`ServerProtocol::encode_merchant_offers`]'s per-offer payload.
///
/// Items are [`ResourceKey`]s rather than a version's numeric registry id:
/// this type crosses the version-free `lodestone-server` / versioned
/// `crates/protocol/*` seam the same way every other model type here does
/// (`MetadataField::Item`, `ItemStack`), and it is the version crate's own
/// `item_id` table that resolves a key to its wire id at encode time.
#[derive(Debug, Clone, PartialEq)]
pub struct MerchantOfferOut {
    /// First input: item and count.
    pub wants_a: (ResourceKey, i32),
    /// Optional second input.
    pub wants_b: Option<(ResourceKey, i32)>,
    /// What the trade produces.
    pub gives: (ResourceKey, i32),
    /// Uses before the trade locks until restocked.
    pub max_uses: i32,
    /// Villager xp granted per use.
    pub xp: i32,
}
