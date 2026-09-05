use uuid::Uuid;

use crate::{
    command_tree::{CommandSuggestionEntry, CommandTree},
    common::{Difficulty, GameMode},
    ids::{DimensionId, Identifier, ResourceKey},
    item::ItemStack,
    math::{BlockPos, ChunkPos, Quat, Rotation, SectionPos, Vec3, Vec3f},
    text::{Text, TextColor},
};

/// The semantic kind of an incoming chat component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatKind {
    /// Player or signed chat message.
    Chat,
    /// System message.
    System,
    /// Game information, such as action-bar text.
    GameInfo,
}

/// A last-death location, from the optional `GlobalPos` field of
/// the respawn packet (and the game-join packet's equivalent).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeathLocation {
    /// Dimension the death occurred in.
    pub dimension: DimensionId,
    /// Block position of the death.
    pub pos: BlockPos,
}

/// The server-declared properties of the dimension type the local player is in.
///
/// # Why this is a distinct fact from [`DimensionId`]
///
/// A [`DimensionId`] names a *level* (`minecraft:the_nether`, or `mypack:mine`);
/// a dimension **type** is a registry entry that level points at, and it is
/// where the geometry and lighting rules actually live. Two levels can share one
/// type, and a data pack can give a level called `mypack:mine` the vanilla
/// overworld type — which is exactly the case that made matching on the level
/// name wrong.
///
/// Version adapters fill this in from the Configuration `registry_data` packet;
/// before that, nothing decoded that packet at all, so every field here was
/// hardcoded client-side by level-name match.
///
/// # Field selection
///
/// Only the fields a version-free consumer can act on. Vanilla's dimension-type
/// record additionally carries `infiniburn`, monster-spawn settings, a skybox
/// choice, a cardinal-light mode, an environment-attribute map and a timeline
/// set; those are nested registry references with no consumer here. Add one when
/// something reads it.
///
/// Note there is **no `bed_works`**: 26.2 moved that into the dimension type's
/// environment attributes (`minecraft:gameplay/bed_rule`), so it is not a
/// top-level dimension-type field any more and cannot be modelled as a bool.
#[derive(Debug, Clone, PartialEq)]
pub struct DimensionTypeInfo {
    /// The dimension type's own registry id, e.g. `minecraft:overworld`. This is
    /// a `dimension_type` id, **not** the level's [`DimensionId`].
    pub name: ResourceKey,
    /// Whether columns here carry sky light. `false` only in the Nether among
    /// vanilla's four types — the End has sky light exactly like the overworld.
    pub has_skylight: bool,
    /// Whether the dimension has a solid ceiling (the Nether).
    pub has_ceiling: bool,
    /// Whether the time of day is fixed here (the Nether and the End).
    pub has_fixed_time: bool,
    /// Movement scale relative to the overworld — `8.0` in the Nether.
    pub coordinate_scale: f64,
    /// Lowest world-`y` a column stores (`-64` overworld, `0` Nether/End).
    pub min_y: i32,
    /// Total column height in blocks (`384` overworld, `256` Nether/End).
    pub height: i32,
    /// Highest `y` a portal or bed may place the player at (`128` in the Nether,
    /// against a height of `256`).
    pub logical_height: i32,
    /// Baseline light every block receives regardless of sky exposure — `0.0`
    /// overworld, `0.1` Nether, `0.25` End.
    pub ambient_light: f32,
    /// The dimension's ambient-light-color attribute, packed `0xRRGGBB` — the
    /// colour the GPU lightmap seeds its accumulator with before either light
    /// half is added, so an unlit surface is not pure black. **Not** the same
    /// quantity as [`Self::ambient_light`] above (that one only ever blends a
    /// *lerp fraction*; this is the actual seed colour the terrain/entity/fluid
    /// shaders read). Grey in the overworld, warm brown in the Nether, sage in
    /// the End — see `lodestone_render::light`. `None` when the source did not
    /// resolve one; a version-free consumer should fall back to the
    /// overworld's own value rather than invent a brighter one.
    pub ambient_light_color: Option<u32>,
}

impl DimensionTypeInfo {
    /// Number of 16-tall block sections in a column of this dimension.
    #[must_use]
    pub fn section_count(&self) -> usize {
        usize::try_from(self.height.max(0)).unwrap_or(0) / 16
    }
}

/// A signed player-chat acknowledgement input.
///
/// Only signed player chat carries this. System chat, disguised chat, and older
/// protocols should use `None` on [`ClientEvent::Chat`]. A filtered message still
/// carries `Some(Self { was_shown: false, .. })`: it advances the vanilla
/// last-seen window and burns an acknowledgement offset even though it did not
/// render to the user.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatAckInfo {
    /// Raw message signature bytes. Empty means this message carried no
    /// signature at all —
    /// the common case on a server with signed chat disabled, and by itself
    /// enough to treat the message as unverified.
    pub signature: Vec<u8>,
    /// Server-global signed-chat index.
    pub global_index: i32,
    /// Whether the message was shown to the user after filtering.
    pub was_shown: bool,
    /// This message's position in the sender's signing chain, the wire's
    /// `index` field on `PLAYER_CHAT`.
    /// Needed, together with the sender's announced chat-session id
    /// ([`PlayerListEntry::chat_session`]), to reconstruct the exact
    /// signing-chain link `lodestone_auth::verify_signature` hashes —
    /// verification cannot be attempted without it.
    pub message_index: i32,
    /// The signed body's own timestamp, epoch **milliseconds** — the wire
    /// unit. The signature payload
    /// itself is built over epoch **seconds**
    /// (`lodestone_auth::chat_session::build_signature_payload`'s
    /// `timestamp_epoch_seconds` parameter); converting is the verifier's
    /// job, not this struct's — carrying the wire unit verbatim is what
    /// keeps that conversion a single, visible `/ 1000` at the one call site
    /// that needs it, rather than an implicit unit change baked into a field
    /// name.
    pub timestamp_millis: i64,
    /// The signed body's random salt.
    pub salt: i64,
    /// The raw signed message content, verbatim — **not** [`ClientEvent::Chat`]'s
    /// own `text`, which may be the server's *decorated* form
    /// (`unsigned_content`) instead. Verification must hash exactly what the
    /// sender signed, so this is kept alongside the decorated text rather
    /// than reconstructed from it.
    pub raw_content: String,
    /// The resolved last-seen signature chain this message was built over,
    /// already resolved against the
    /// connection's signature cache — see `read_last_seen_packed`), each
    /// entry 256 raw signature bytes.
    pub last_seen: Vec<Vec<u8>>,
    /// Whether this message's signature was checked against the sender's
    /// announced public key and found valid.
    ///
    /// **Populated by the client driver, not by the wire decoder** — the
    /// adapter that builds this struct has no access to the per-player
    /// public-key store, only the driver's read-model does (see
    /// `lodestone_client::driver`'s `emit` handling of `ClientEvent::Chat`).
    /// Every adapter constructs this `false` (fail-closed: unverified until
    /// proven otherwise, never trusted by default), and the driver may raise
    /// it to `true` after a successful `lodestone_auth::verify_signature`
    /// call. An empty `signature` (no signature at all) is left `false` and
    /// is never attempted — the same rule the real client's own trust
    /// evaluation applies for an unsigned message.
    pub verified: bool,
}

/// A packed message signature.
///
/// The wire form is either a full 256-byte signature (for a signature the
/// client has not cached yet) or an index into the last-seen signature
/// cache. The v26-2 adapter resolves `Cached` references against its
/// per-connection signature cache before emitting — dropping the event on a
/// miss — so a `ChatMessageDeleted` normally carries a `Full` signature; the
/// `Cached` variant remains for adapters that pass the wire form through
/// unresolved.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PackedMessageSignature {
    /// A full 256-byte signature.
    Full(Vec<u8>),
    /// An index into the last-seen signature cache.
    Cached(i32),
}

/// The anchor point used by the player look at packet's
/// `EntityAnchorArgument.Anchor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LookAnchor {
    /// Anchor at the entity's feet.
    Feet,
    /// Anchor at the entity's eyes.
    Eyes,
}

/// Entity-target details for [`ClientEvent::PlayerLookAt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerLookAtEntity {
    /// Target entity id.
    pub entity_id: i32,
    /// Anchor point on the target entity.
    pub to_anchor: LookAnchor,
}

/// Relative components of a player teleport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TeleportFlags {
    /// X position is relative to the current position.
    pub relative_x: bool,
    /// Y position is relative to the current position.
    pub relative_y: bool,
    /// Z position is relative to the current position.
    pub relative_z: bool,
    /// Yaw is relative to the current rotation.
    pub relative_yaw: bool,
    /// Pitch is relative to the current rotation.
    pub relative_pitch: bool,
}

/// A semantic entity movement payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EntityMovement {
    /// New absolute position.
    Absolute(Vec3),
    /// Delta from the entity's current position.
    Relative(Vec3),
}

/// A version-free entity pose.
///
/// A version adapter maps its protocol's numeric pose enum onto these stable
/// names. The set is `non_exhaustive` and carries an [`Other`](EntityPose::Other)
/// escape hatch so a pose a version has that this list does not can still travel
/// as its raw id rather than being dropped or misclassified.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityPose {
    /// Standing upright (the default).
    Standing,
    /// Gliding with an elytra.
    FallFlying,
    /// Sleeping in a bed.
    Sleeping,
    /// Swimming (also crawling).
    Swimming,
    /// Riptide spin attack.
    SpinAttack,
    /// Crouching / sneaking.
    Crouching,
    /// Mid long-jump (e.g. a ravager).
    LongJumping,
    /// Dying.
    Dying,
    /// Sitting.
    Sitting,
    /// A pose this version has that the shared set does not name, kept as its
    /// raw protocol id.
    Other(u32),
}

/// A version-free entity animation kind, from the animate packet.
///
/// `non_exhaustive` with an [`Other`](AnimationAction::Other) escape hatch for
/// the same reason as [`EntityPose`]: vanilla's action byte is a small, fixed
/// set of named constants (with one reserved/unused value, `1`, deliberately
/// skipped by Mojang), so an id this table does not recognise still travels
/// intact rather than being dropped.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnimationAction {
    /// Swing the main hand.
    SwingMainHand,
    /// Play the "wake up" animation (leaving a bed).
    WakeUp,
    /// Swing the off hand.
    SwingOffHand,
    /// Show a critical-hit particle burst.
    CriticalHit,
    /// Show a magic-critical-hit particle burst.
    MagicCriticalHit,
    /// An action byte this list does not name, kept as its raw wire value.
    Other(u8),
}

/// Whether an optional metadata field appeared in an update at all, distinct
/// from whether it currently holds a value.
///
/// Several vanilla metadata fields are themselves nullable on the wire (the
/// custom name, the displayed item stack): a packet can be silent about the
/// field (nothing changed), or carry it with a value, or carry it explicitly
/// cleared. Modelling that as `Option<Option<T>>` — as [`EntityMetadataUpdate`]
/// and a few sibling types across the workspace still do — encodes exactly
/// this, but positionally: nothing in the type says *which* `Option` means
/// what, so every read site has to re-derive "outer is presence, inner is
/// value" from a doc comment (or worse, from the surrounding code) rather
/// than from the type itself. This enum names the two states instead.
///
/// `Unreported` must never overwrite a previously known value — a dropped
/// item, for instance, names its item id **once** at spawn and sends
/// item-free metadata forever after, so a consumer that treats "the field is
/// absent from *this* update" the same as "the server cleared it" blanks the
/// item's model one tick after it appears. `Reported(None)` is the actual
/// clear.
///
/// # Where this is applied
///
/// Wired end to end across the crates that touch a dropped item's or a named
/// entity's identity: [`EntityMetadataUpdate::custom_name`] and
/// [`EntityMetadataUpdate::item`] here; `lodestone-client`'s `EntityView`
/// fields of the same names (folded from the above in
/// `lodestone-client/src/state.rs`'s `apply_metadata`); and
/// `lodestone-shell/src/entities.rs`'s `EntitySnapshot::item`, produced from
/// the client view by `lodestone-shell/src/net.rs`'s `entity_snapshot()`. All
/// four converted in the same pass because each is a producer or consumer of
/// the next — retyping any one alone does not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reported<T> {
    /// The field was not present in this update. Must **not** overwrite a
    /// previously known value.
    Unreported,
    /// The field was present. `None` means the server explicitly cleared it;
    /// `Some(value)` is the value it now holds.
    Reported(Option<T>),
}

impl<T> Reported<T> {
    /// Whether the field was present in this update at all (either variant of
    /// [`Reported::Reported`]), as the `Option<Option<T>>` shape's outer
    /// `is_some()` used to answer.
    #[must_use]
    pub const fn is_reported(&self) -> bool {
        matches!(self, Self::Reported(_))
    }

    /// Collapses to "the value right now, if any" — `Unreported` and
    /// `Reported(None)` both become `None`.
    ///
    /// This deliberately discards the distinction the rest of this type
    /// exists to keep: use it only at a call site that genuinely does not
    /// care *why* there is no value (never reported vs. explicitly cleared),
    /// same as it would not have cared with the old `Option<Option<T>>`
    /// shape's `.flatten()`. A call site that needs to tell "never mentioned"
    /// from "explicitly cleared" — e.g. to decide whether to overwrite a
    /// previously known value — must match on the variants directly instead.
    #[must_use]
    pub fn into_value(self) -> Option<T> {
        match self {
            Self::Reported(v) => v,
            Self::Unreported => None,
        }
    }
}

impl<T> Default for Reported<T> {
    /// The natural default for "this update did not mention the field".
    fn default() -> Self {
        Self::Unreported
    }
}

/// An incremental, version-free update to an entity's metadata.
///
/// Vanilla transmits metadata as a sparse `(index, value)` list applied
/// cumulatively, where the *index* of each semantic field and the *serializer*
/// used to encode it are version-specific. A version adapter resolves those
/// per-version details and produces this struct holding only the fields that a
/// given packet actually carried — every field is `Option`, and `None` means
/// "this packet did not mention it", not "cleared".
///
/// The fields that are themselves optional on the wire — the custom name and
/// the displayed item stack — use a nested `Option`: the outer `Option` is "did
/// this packet include the field", the inner is "is a value currently set".
/// That shape is exactly what [`Reported<T>`] exists to name instead; see its
/// docs and the two fields below for why it is not applied here yet.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EntityMetadataUpdate {
    /// The shared entity flags byte (on-fire / crouching / sprinting / swimming
    /// / invisible / glowing / fall-flying), when present. Bit meanings are
    /// stable across modern versions.
    pub flags: Option<u8>,
    /// The **living-entity** flags byte (using-item / which hand / spin attack),
    /// when present and when the entity is known to be a living entity. Decode
    /// through `lodestone_entity::metadata::LivingEntityFlags` (a downstream
    /// crate, hence no intra-doc link) rather than by masking inline.
    ///
    /// # Why this can be absent on a packet that carried the byte
    ///
    /// The byte's index collides with a *non*-living entity's own flags byte of
    /// the same serializer (in 26.2, `AbstractArrow`'s crit/pierce bitfield sits
    /// at the same index as `LivingEntity`'s), so the wire alone cannot say which
    /// one arrived. A version adapter that cannot establish the entity is living
    /// leaves this `None` rather than surfacing a byte that may mean something
    /// else entirely — a critical arrow's crit bit is bit-identical to the
    /// using-item bit. `None` therefore means "not known to be living flags",
    /// which a consumer must treat as "not using an item", never as a cleared
    /// bitfield.
    pub living_flags: Option<u8>,
    /// The **mob** flags byte (no-AI / left-handed / **aggressive**), when present
    /// and when the entity is known to be a mob-type entity. Decode through
    /// `lodestone_entity::metadata::MobFlags` rather than by masking inline.
    ///
    /// # Why this is separate from [`living_flags`](Self::living_flags)
    ///
    /// It is a different byte at a different index, declared for a different
    /// entity category, and it is what actually drives a *mob*'s arm pose. Every
    /// mob renderer whose model draws an aggressive pose reads this
    /// aggressive flag; the using-item bit behind
    /// [`living_flags`](Self::living_flags) is the *player* mechanism. A skeleton
    /// drawing on you never sets the using-item bit, so a client that only decodes
    /// index 8 leaves every mob in the rest pose.
    ///
    /// # Why this can be absent on a packet that carried the byte
    ///
    /// Same reason as [`living_flags`](Self::living_flags), one notch tighter. The
    /// byte's index is shared with the armor stand's client-flags byte of the same
    /// serializer, and an armour stand *is* a living entity — so establishing
    /// "living" is not enough and the adapter must establish "mob". `None`
    /// therefore means "not known to be mob flags", which a consumer must read as
    /// "not aggressive", never as a cleared bitfield.
    pub mob_flags: Option<u8>,
    /// The **armour stand client-flags** byte (small / show-arms / no-base-plate
    /// / marker), when present and when the entity is known to be an
    /// armor stand. Decode through
    /// `lodestone_entity::metadata::ArmorStandFlags` rather than by masking
    /// inline.
    ///
    /// # Why this is separate from [`mob_flags`](Self::mob_flags)
    ///
    /// It is the *other* claimant of the same metadata index (15) with the same
    /// serializer (`BYTE`) — the armor stand's client-flags field rather than
    /// the mob's flags field — and `0x04` means "show arms" here where it means
    /// `aggressive` in [`mob_flags`](Self::mob_flags). Folding them into one
    /// field would make "is this stand's arm visible" and "is this mob
    /// attacking" read off whichever byte the adapter happened to establish
    /// last.
    ///
    /// # Why a client needs this: the "hologram" case
    ///
    /// A server-side "hologram" is an armour stand with
    /// [`flags`](Self::flags)'s invisible bit set, a custom name, and
    /// `custom_name_visible` — but that trio alone still shows the stand's base
    /// plate and, if it were ever built without this byte, a "show arms" toggle
    /// would have no field to read. `marker` (no hitbox, ignores piston pushes)
    /// and `no_base_plate` are what a decorative stand actually turns off; see
    /// `lodestone_entity::metadata::ArmorStandFlags`'s own doc for the full
    /// conjunction.
    ///
    /// # Why this can be absent on a packet that carried the byte
    ///
    /// Same shape as [`mob_flags`](Self::mob_flags), the complementary half: a
    /// version adapter that cannot establish the entity is an armor stand
    /// leaves this `None` rather than surfacing a byte that may mean a mob's
    /// aggressive bit. `None` therefore means "not known to be armour-stand
    /// flags", which a consumer must read as "no armour-stand-specific
    /// cosmetics known", never as a cleared bitfield.
    pub armor_stand_flags: Option<u8>,
    /// An armour stand's six part rotations, as far as *this* packet reported
    /// them — the head-pose through right-leg-pose fields,
    /// indices 16-21, each an `(x, y, z)` triple of Euler **degrees**.
    ///
    /// # Why the six stay individually optional
    ///
    /// A metadata packet carries only the accessors that *changed*, so an
    /// update that moves one arm mentions one index. Collapsing them into a
    /// whole [`ArmorStandPose`] here would force this type to invent values for
    /// the five it was not told about, and a consumer could not tell an
    /// unreported part from one explicitly set back to its default. The merge
    /// into a whole pose belongs where the *previous* pose exists — see
    /// [`ArmorStandPose::merged`].
    ///
    /// # Why a consumer must apply a pose even when every part is `None`
    ///
    /// Vanilla's `ArmorStandArmorModel.setupAnim` calls the humanoid
    /// `super.setupAnim` — walk cycle, idle bob and all — and then **assigns**
    /// all six part rotations from the pose, unconditionally. The swing is
    /// computed and thrown away. A stand that has never reported a pose still
    /// has one: vanilla's own armor-stand metadata-field defaults, which
    /// [`ArmorStandPose::VANILLA_DEFAULT`] carries. Treating "nothing reported"
    /// as "do not overwrite" leaves the walk cycle standing, and a stand carried
    /// along by a moving contraption then swings its arms — with any held item,
    /// posed off that same arm, swinging with it.
    pub armor_stand_pose: ArmorStandPoseUpdate,
    /// The custom name. [`Reported::Unreported`] when this packet did not
    /// mention it; [`Reported::Reported(None)`](Reported::Reported) is an
    /// explicit clear; [`Reported::Reported(Some(name))`](Reported::Reported)
    /// is the name it now holds.
    ///
    /// Carries the full styled component tree (colour, bold, italic,
    /// underline, strikethrough, inheritance down `extra` children) rather
    /// than a flattened plain string — a version adapter decodes the wire's
    /// NBT/JSON component with [`Text::from_nbt`]/[`Text::from_json`] rather
    /// than reducing it to text at decode time. Flattening early is exactly
    /// what used to make every custom name and every player nametag render
    /// white with no formatting: nothing downstream of a plain `String` can
    /// recover a colour that was never carried past this field.
    pub custom_name: Reported<Text>,
    /// Whether the custom name renders above the entity, when present.
    pub custom_name_visible: Option<bool>,
    /// The entity pose, when present.
    pub pose: Option<EntityPose>,
    /// Current health, when present (living entities only).
    pub health: Option<f32>,
    /// Whether the entity is a baby, when present (ageable mobs only).
    pub baby: Option<bool>,
    /// The cosmetic variant (sheep colour, villager profession, horse
    /// colour/markings, biome-specific animal variant, …), when the version
    /// adapter could raise one from this packet. `None` means the packet did
    /// not carry a variant field; a consumer treats that as "the type's vanilla
    /// default", not "unknown".
    pub variant: Option<EntityVariant>,
    /// The item stack an item-carrying entity displays, when the packet carried
    /// the field.
    ///
    /// This is what a dropped item (`minecraft:item`) is *made of*: its entire
    /// visible identity rides this one metadata field. The same field carries
    /// the display item of thrown projectiles (snowball, egg, ender pearl,
    /// splash potion), fireballs, and the eye of ender.
    ///
    /// Like [`custom_name`](Self::custom_name): [`Reported::Unreported`] when
    /// this packet did not mention the field,
    /// [`Reported::Reported(None)`](Reported::Reported) is the empty stack
    /// (which vanilla draws as nothing), and
    /// [`Reported::Reported(Some(stack))`](Reported::Reported) is the stack it
    /// now holds.
    ///
    /// A stack whose wire form carried a data component this build does not
    /// model still arrives here with
    /// [`ItemComponents::has_unmodeled`](crate::ItemComponents::has_unmodeled)
    /// set. The item key and count are decoded *before* any component is, so an
    /// unrecognised component costs detail, never the answer to "which item is
    /// this".
    pub item: Reported<ItemStack>,
    /// Current air supply in ticks, when present.
    /// Feeds the HUD's underwater bubble row (`docs/sky-and-air-bubbles.md`).
    pub air_supply: Option<i32>,
    /// A creeper's fuse direction, when present and
    /// the entity is known to be a creeper: `-1` while idle or backing off,
    /// `1` while counting up to detonation. The counter itself
    /// is never sent — only the direction is, and
    /// a consumer integrates it client-side one tick at a time, exactly as
    /// the real client does. See
    /// `lodestone_render::entity_anim::pose_swelling`'s docs for why the split
    /// between "synced direction" and "locally integrated counter" exists.
    pub creeper_swell_dir: Option<i32>,
    /// Whether a creeper is charged (lightning-struck), when present and the
    /// entity is known to be a creeper. Doubles the
    /// explosion radius and drops a charged mob's head; set once and never
    /// cleared.
    pub creeper_powered: Option<bool>,
    /// Whether a creeper's fuse has been lit (flint-and-steel or fire charge),
    /// when present and the entity is known to be a creeper. Set once and never
    /// cleared — distinct from
    /// [`creeper_swell_dir`](Self::creeper_swell_dir) alone being positive,
    /// which also happens from proximity (a nearby-player AI goal) without ever igniting.
    pub creeper_ignited: Option<bool>,
    /// An experience orb's XP value, when present
    /// and the entity is known to be an orb.
    ///
    /// This is what **one** absorption of the orb pays, not how many absorptions
    /// the entity holds after merging — the game keeps those as two separate
    /// numbers and only the first is synced. The client needs it for exactly one
    /// thing: picking one of eleven sprite cells to draw,
    /// by a **bucketed** comparison ladder rather than a linear map, so two orbs
    /// worth 7 and 16 draw the same cell and one worth 17 draws the next.
    ///
    /// # Why this can be absent on a packet that carried the value
    ///
    /// Same shape as [`living_flags`](Self::living_flags)/[`mob_flags`](Self::mob_flags),
    /// one index over: the value is an `INT` at an index four *other* entity types
    /// also put an unrelated `INT` at (a primed TNT's fuse, a fishing hook's
    /// target, a vehicle's hurt timer, a display entity's interpolation delay), so
    /// a version adapter that cannot establish the entity is an orb leaves this
    /// `None` rather than surfacing a number that means something else.
    ///
    /// `None` therefore means "not known to be an orb value", which a consumer
    /// reads as the real accessor's own default of `0` — the icon for value `0`
    /// is cell 0 — never as a cleared value.
    pub experience_orb_value: Option<i32>,
    /// Whether a tamed-animal-family entity is tamed, when present and the
    /// entity is known to belong to that family.
    ///
    /// Two different bytes feed this one field: one bit for wolf/cat/parrot,
    /// and a *different* bit at the
    /// same wire index for the horse family. A version adapter resolves which family the concrete
    /// entity type belongs to and reads the matching bit; this field is the
    /// version-free result either way, so a consumer never needs to know the bit
    /// differed.
    ///
    /// # Why this can be absent on a packet that carried the byte
    ///
    /// Same shape as [`living_flags`](Self::living_flags): index 18's `BYTE` is
    /// also a sheep's wool-colour field and a shulker's colour field. A version adapter
    /// that cannot establish the entity is a tamable-animal or a horse leaves
    /// this `None` rather than surfacing a byte that may mean a wool colour.
    /// `None` therefore means "not known to be a tameable family", which a
    /// consumer must treat as "draw the untamed/wild appearance", never as a
    /// cleared bitfield.
    pub tamed: Option<bool>,
    /// Whether a tamable animal is sitting, when present
    /// and the entity is known to be one of the wolf/cat/parrot family. The
    /// horse family has no equivalent bit at this index, so this is `None` for
    /// every horse-family entity regardless of pose. Same absence rule as
    /// [`tamed`](Self::tamed): `None` means "not known to be a tamable animal",
    /// not "not sitting".
    pub sitting: Option<bool>,
    /// The ender dragon's current fight phase, when present and the entity is
    /// known to be an ender dragon.
    ///
    /// # Why this can be absent on a packet that carried the value
    ///
    /// Same shape as [`experience_orb_value`](Self::experience_orb_value): the
    /// value is an `INT` at an index five *other* entity types also put an
    /// unrelated `INT` at (a creeper's swell direction, a display entity's
    /// brightness override, a phantom's size, a warden's anger level, a
    /// wither's target). `None` means "not known to be a dragon phase", which
    /// a consumer must treat as "no phase-specific pose", never as a cleared
    /// value.
    pub dragon_phase: Option<i32>,
    /// The end crystal's beam-target field — where the crystal's beam points, when
    /// present. [`Reported::Reported(None)`](Reported::Reported) is "no beam"
    /// (the field's own empty default); [`Reported::Reported(Some(pos))`](Reported::Reported)
    /// is a beam aimed at `pos`. Self-identifying by `(index, serializer)`
    /// pair at the wire — see the version adapter's own decode-side doc for
    /// why the serializer alone is not enough (that same optional-position
    /// serializer is reused at two other indices for unrelated fields).
    pub crystal_beam_target: Reported<BlockPos>,
    /// The end crystal's show-bottom field — whether the crystal draws its bedrock
    /// base, when present and the entity is known to be an end crystal.
    ///
    /// # Why this can be absent on a packet that carried the byte
    ///
    /// Same shape as [`tamed`](Self::tamed): index 9's `BOOLEAN` is also
    /// an area-effect-cloud's waiting flag and a fishing hook's biting flag. `None`
    /// means "not known to be an end crystal", which a consumer must treat as
    /// "draw the base" (the field's own default), never as a cleared flag.
    pub crystal_show_bottom: Option<bool>,
    /// The painting's variant field — which painting is hung, as its
    /// registry key (`minecraft:kebab`), when present.
    ///
    /// # Why this one needs no class guard
    ///
    /// Unlike almost every other field here, it is self-identifying by
    /// **serializer**: `PAINTING_VARIANT` has exactly one claimant in the 26.2
    /// entity-data dump, so a decoder that sees it knows what it is without
    /// establishing the entity type, exactly as [`item`](Self::item) does for
    /// `ITEM_STACK`. The index it arrives at is therefore not load-bearing.
    ///
    /// # Why a key and not the wire's holder id
    ///
    /// The wire carries a `Holder<PaintingVariant>`, i.e. an index into the
    /// server's own `minecraft:painting_variant` registry, and a data pack can
    /// change what that index means. Surfacing the id would push that hazard
    /// onto every consumer; the version adapter resolves it once, against the
    /// registry order it knows, and hands on the key. A consumer that does not
    /// recognise the key must draw nothing rather than substitute a variant —
    /// see `lodestone_render::painting::painting_size`.
    pub painting_variant: Option<Identifier>,
    /// Whether a firework rocket is **attached to a gliding player** —
    /// the attached-to-target field reduced to its presence,
    /// when present.
    ///
    /// Only presence is carried, not the target id, because
    /// an attached rocket is never itself drawn
    /// and nothing downstream would read which entity it rides. Reducing it
    /// here rather than passing the id on is a decision, not a dropped field.
    ///
    /// `None` means "never reported", which a consumer must treat as **not**
    /// attached (the field's own empty default) — i.e. a rocket that draws.
    pub firework_attached: Option<bool>,
    /// Whether a firework rocket was fired from a crossbow —
    /// the shot-at-angle field, when present and the entity
    /// is known to be a firework rocket.
    ///
    /// It is what tips the sprite out of the camera plane onto its flight axis.
    ///
    /// # Why this can be absent on a packet that carried the byte
    ///
    /// Index 10's `BOOLEAN` is also an arrow's in-ground flag and
    /// an interaction entity's response-id field, and none of the three claimants is a
    /// living entity, so the `living`/`mob` census cannot separate them. `None`
    /// therefore means "not known to be a firework's angle bit", which a
    /// consumer must read as "not shot at an angle", never as a cleared flag.
    pub firework_shot_at_angle: Option<bool>,
    /// The item frame's rotation field — which of the eight 45° steps the stack in
    /// an item frame is turned to (`0..8`).
    ///
    /// # Why this can be absent on a packet that carried the int
    ///
    /// Index 10's `INT` is also a display entity's position/rotation
    /// interpolation-duration field and a vehicle's damage field's neighbours in
    /// the jar dump, so an
    /// adapter raises this only for an entity it already knows is an item
    /// frame. `None` is "not known to be a frame's rotation", which a consumer
    /// treats as the field's own default of `0` — an upright item — never as a
    /// cleared value.
    pub item_frame_rotation: Option<u8>,
    /// The vehicle's hurt-time field — the boat/minecart hurt clock, set to
    /// `10` when the vehicle takes damage and counted down one per tick by the
    /// vehicle's own tick. `0` is "not hurt".
    ///
    /// # Why this can be absent on a packet that carried the int
    ///
    /// Index 8's `INT` has five claimants in the jar dump — an experience
    /// orb's value, a primed TNT's fuse, a fishing hook's hooked entity and a
    /// display entity's interpolation delay alongside this — and no census
    /// column separates them (none of the five is living). An adapter raises
    /// this only for an entity it already knows is a vehicle. `None`
    /// therefore means "not known to be a vehicle's hurt clock", never a
    /// cleared value.
    pub vehicle_hurt_time: Option<i32>,
    /// The vehicle's hurt-direction field — which way the hull rocks, `+1` or
    /// `-1`; each hit negates it so consecutive punches tip
    /// the boat alternately. Its default is `1`, not `0`.
    ///
    /// # Why this can be absent on a packet that carried the int
    ///
    /// Index 9's `INT` is also a display entity's transformation-interpolation
    /// duration, so this is entity-type-gated for the same reason
    /// [`vehicle_hurt_time`](Self::vehicle_hurt_time) is.
    pub vehicle_hurt_dir: Option<i32>,
    /// The vehicle's damage field — accumulated damage × 10, decayed by
    /// `1.0` per tick. It scales the rock amplitude, so a heavier hit tips the
    /// hull further; the vehicle is destroyed past `40.0`.
    ///
    /// Index 10's `FLOAT` has this as its only claimant in the jar dump, so
    /// the serializer alone identifies it — but it is entity-type-gated anyway,
    /// beside its two siblings, because the three are one feature and a future
    /// jar could add a second `FLOAT` there.
    pub vehicle_damage: Option<f32>,
    /// The display entity's billboard-constraint field, as its
    /// raw wire ordinal (`0`=fixed, `1`=vertical, `2`=horizontal, `3`=center),
    /// when present and the entity is known to be one of the three display
    /// subtypes (`text_display`/`item_display`/`block_display`).
    ///
    /// Kept as the raw ordinal rather than a decoded enum because this crate
    /// carries no renderer-facing billboard type — see
    /// `lodestone_render::display::BillboardMode::from_wire` for the
    /// downstream conversion, which reproduces the real client's own
    /// out-of-range fallback to `Fixed`.
    ///
    /// # Why this can be absent on a packet that carried the byte
    ///
    /// Same shape as [`mob_flags`](Self::mob_flags): index 15's `BYTE` is also
    /// a mob's flags field and an armor stand's client-flags field. `None`
    /// means "not known to be a display billboard byte", which a consumer
    /// must treat as "no billboard reported yet", never as a cleared value.
    pub display_billboard: Option<u8>,
    /// The display entity's translation field, in blocks — one quarter of the
    /// shared transformation every display subtype carries (see
    /// `lodestone_render::display::DisplayTransformation`).
    ///
    /// Unlike [`display_billboard`](Self::display_billboard), no entity-type
    /// guard is needed to surface this: the wire's `VECTOR3` serializer at this
    /// index is exclusively the translation field in the 26.2 jar
    /// dump (`tests/support/entity_data_index_jvm.txt` in the version crate),
    /// so the *value shape* alone disambiguates it — the same reasoning
    /// [`crystal_beam_target`](Self::crystal_beam_target) already documents
    /// for its own index.
    pub display_translation: Option<Vec3f>,
    /// The display entity's scale field — the second quarter of the shared
    /// transformation, self-identifying by the same `VECTOR3`-at-this-index
    /// argument as [`display_translation`](Self::display_translation).
    pub display_scale: Option<Vec3f>,
    /// The display entity's left-rotation field — applied **before** scale.
    /// Self-identifying: the wire's `QUATERNION`
    /// serializer at this index is exclusively this field in the jar dump.
    pub display_left_rotation: Option<Quat>,
    /// The display entity's right-rotation field — applied **after** scale. Same
    /// self-identifying argument as
    /// [`display_left_rotation`](Self::display_left_rotation), one index over.
    pub display_right_rotation: Option<Quat>,
    /// The text display's text field, decoded to plain text the same way
    /// [`custom_name`](Self::custom_name) is. [`Reported::Unreported`] when
    /// this packet did not mention it; [`Reported::Reported(Some(text))`](Reported::Reported)
    /// carries the current text. Unlike `custom_name`, a version adapter
    /// never reports the inner `None` here — the field's own accessor default
    /// is the empty string, not an absent component — so a consumer only
    /// ever sees `Unreported` or `Reported(Some(_))` in practice, but the
    /// shape stays `Reported<Text>` (not a bespoke wrapper) for the same
    /// "did this packet mention it" contract every other field here uses.
    ///
    /// Styled, like [`custom_name`](Self::custom_name): the wire's `COMPONENT`
    /// serializer decodes through [`Text::from_nbt`] rather than being
    /// flattened to plain text, so colour/bold/italic/underline/strikethrough
    /// (and inheritance from a parent node down to its `extra` children)
    /// survive to whatever draws this `text_display`.
    ///
    /// Present only when the entity is known to be a `text_display` — index
    /// 23's `COMPONENT` serializer is also a command-block minecart's last
    /// command output at index 14 (a different index, no collision), but the
    /// entity-type guard is kept anyway for the same defence-in-depth every
    /// other display field in this struct uses.
    pub display_text: Reported<Text>,
    /// The text display's line-width field, the wrap width in
    /// pixels (default `200`). Present
    /// only for a `text_display` that has reported it.
    pub display_line_width: Option<i32>,
    /// The text display's background-color field, a packed ARGB int
    /// (default `0x40000000`, a translucent-black panel).
    /// Present only for a `text_display` that has reported it.
    pub display_background_color: Option<i32>,
    /// The text display's text-opacity field, a signed byte (
    /// default `-1`, i.e. fully opaque once read as the top byte of an ARGB
    /// colour: `textOpacity << 24 | 0xFFFFFF`). Present only for a
    /// `text_display` that has reported it.
    pub display_text_opacity: Option<i8>,
    /// The text display's style-flags field: bit `0x01` shadow, `0x02`
    /// see-through, `0x04` use-the-viewer's-own-default-background, bits
    /// `0x08`/`0x10` alignment (neither set is
    /// centre, `0x08` left, `0x10` right). Present only for a `text_display`
    /// that has reported it.
    pub display_text_style_flags: Option<u8>,
    /// The block display's block-state field this `block_display` is showing.
    /// [`BlockStateRef::Canonical`] is in the built-in 26.2 numbering;
    /// [`BlockStateRef::ProtocolLocal`] remains opaque until a matching
    /// version-aware or dynamic-registry consumer resolves it.
    ///
    /// # Why this needs an entity-type guard where the transformation fields do not
    ///
    /// Index 23's `BLOCK_STATE` serializer decodes to the same plain integer
    /// shape as several other fields at *other* indices (block state ids are
    /// carried as a `VarInt`, indistinguishable on the wire from any other
    /// `INT`), and — unlike [`display_translation`](Self::display_translation)'s
    /// `VECTOR3` — index 23 has a real second `INT`-shaped claimant: a cat's
    /// collar-color field. Ungated, a cat's dye ordinal (`0..=15`) would
    /// decode as a wildly out-of-range block-state id. Present only for a
    /// `block_display` that has reported it.
    pub display_block_state: Option<BlockStateRef>,
    /// The item display's item-display-context field, the raw
    /// display-context ordinal this `item_display` was told to pose its
    /// item in. The default is `NONE` (`0`), which selects
    /// the identity pose, not "draw nothing" —
    /// so that is what a consumer applies when this is absent. Present only
    /// for an `item_display` that has reported it.
    pub display_item_context: Option<u8>,
    /// The display entity's brightness-override field, in the game's own
    /// packed layout (`block << 4 | sky << 20`), or its own
    /// `-1` no-override sentinel. Carried unpacked so a consumer can tell the
    /// sentinel from a real `(0, 0)` override, which packs to `0`.
    ///
    /// # Why this needs an entity-type guard where the transformation fields do not
    ///
    /// Index 16 has six `INT`-shaped claimants in the jar dump —
    /// a creeper's swell direction, an ender dragon's phase, a phantom's size,
    /// a warden's anger level and a wither's target beside this
    /// one — and none of the other five is a display subtype, so the guard is
    /// "is this any display", the same one
    /// [`display_billboard`](Self::display_billboard) uses, rather than a
    /// per-subtype check.
    pub display_brightness_override: Option<i32>,
}

impl EntityMetadataUpdate {
    /// Whether this update carries no fields at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.flags.is_none()
            && self.living_flags.is_none()
            && self.mob_flags.is_none()
            && self.armor_stand_flags.is_none()
            && self.armor_stand_pose.is_empty()
            && !self.custom_name.is_reported()
            && self.custom_name_visible.is_none()
            && self.pose.is_none()
            && self.health.is_none()
            && self.baby.is_none()
            && self.variant.is_none()
            && !self.item.is_reported()
            && self.air_supply.is_none()
            && self.creeper_swell_dir.is_none()
            && self.creeper_powered.is_none()
            && self.creeper_ignited.is_none()
            && self.experience_orb_value.is_none()
            && self.tamed.is_none()
            && self.sitting.is_none()
            && self.dragon_phase.is_none()
            && !self.crystal_beam_target.is_reported()
            && self.crystal_show_bottom.is_none()
            && self.painting_variant.is_none()
            && self.firework_attached.is_none()
            && self.firework_shot_at_angle.is_none()
            && self.item_frame_rotation.is_none()
            && self.vehicle_hurt_time.is_none()
            && self.vehicle_hurt_dir.is_none()
            && self.vehicle_damage.is_none()
            && self.display_billboard.is_none()
            && self.display_translation.is_none()
            && self.display_scale.is_none()
            && self.display_left_rotation.is_none()
            && self.display_right_rotation.is_none()
            && !self.display_text.is_reported()
            && self.display_line_width.is_none()
            && self.display_background_color.is_none()
            && self.display_text_opacity.is_none()
            && self.display_text_style_flags.is_none()
            && self.display_block_state.is_none()
            && self.display_item_context.is_none()
            && self.display_brightness_override.is_none()
    }

}

/// An armour stand's six part rotations, merged into the whole pose a renderer
/// applies, in **degrees**.
///
/// [`EntityMetadataUpdate`] carries the same six values *individually* and
/// optionally, because a metadata packet mentions only the accessors that
/// changed; this is what a consumer gets after merging one such update onto the
/// pose it already held. [`Self::VANILLA_DEFAULT`] is the starting point, and it
/// is **not** the all-zero pose: the arms and legs carry a small authored
/// splay by default, so a stand nobody has ever posed still
/// has a pose.
///
/// # Why this exists as a value type at all
///
/// The real armour-stand model runs the ordinary humanoid pose setup — head
/// tracking, walk cycle, idle bob — and then **assigns** all
/// six of these over the top. That assignment is the only thing stopping an
/// armour stand animating like a walking humanoid, so this value has to reach
/// the rig; a client that decodes it and drops it draws a stand that swings its
/// arms as it moves, and swings whatever it is holding with them.
///
/// Angles are degrees rather than radians because that is what the wire carries
/// and what a builder types; the single conversion belongs at the rig, next to
/// the other unit choices its model space makes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArmorStandPose {
    /// The head part's rotation.
    pub head: Vec3f,
    /// The body part's rotation. Also drives the visible stand model's three
    /// body sticks — see [`EntityMetadataUpdate::armor_stand_body_pose`].
    pub body: Vec3f,
    /// The left arm part's rotation.
    pub left_arm: Vec3f,
    /// The right arm part's rotation.
    pub right_arm: Vec3f,
    /// The left leg part's rotation.
    pub left_leg: Vec3f,
    /// The right leg part's rotation.
    pub right_leg: Vec3f,
}

impl ArmorStandPose {
    /// The pose every armour stand starts with — the six
    /// default-pose constants each accessor is registered with.
    ///
    /// Head and body are level; the arms and legs carry a small authored splay,
    /// which is why this is a named constant rather than [`Default`]'s zeroes.
    /// A stand that has never sent a pose is in *this* pose, not in a neutral
    /// one, and not in whatever the walk cycle would have produced.
    pub const VANILLA_DEFAULT: Self = Self {
        head: Vec3f::new(0.0, 0.0, 0.0),
        body: Vec3f::new(0.0, 0.0, 0.0),
        left_arm: Vec3f::new(-10.0, 0.0, -10.0),
        right_arm: Vec3f::new(-15.0, 0.0, 10.0),
        left_leg: Vec3f::new(-1.0, 0.0, -1.0),
        right_leg: Vec3f::new(1.0, 0.0, 1.0),
    };

    /// Applies whichever of an update's six parts were reported, leaving the
    /// rest of this pose alone.
    ///
    /// This is the merge the split in [`ArmorStandPoseUpdate`] exists to make
    /// possible: an update that moves one arm must not reset the other five
    /// parts, and the real synced-entity-data mechanism has exactly these
    /// per-accessor-overwrite semantics.
    #[must_use]
    pub fn merged(mut self, update: ArmorStandPoseUpdate) -> Self {
        for (slot, reported) in [
            (&mut self.head, update.head),
            (&mut self.body, update.body),
            (&mut self.left_arm, update.left_arm),
            (&mut self.right_arm, update.right_arm),
            (&mut self.left_leg, update.left_leg),
            (&mut self.right_leg, update.right_leg),
        ] {
            if let Some(value) = reported {
                *slot = value;
            }
        }
        self
    }
}

/// The armour-stand pose fields one metadata packet reported, each part
/// independently present or absent — the wire's shape, as against
/// [`ArmorStandPose`]'s whole-pose shape.
///
/// Kept as a named struct rather than six loose fields on
/// [`EntityMetadataUpdate`] or an array of six because the merge has to be
/// carried across a deferred command boundary, and six same-typed values in a
/// row is the shape a transposition survives unnoticed: swap two and every
/// round trip still agrees while a stand's left arm sits where its right leg
/// should be. Names are the only thing that makes such a swap a compile error.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ArmorStandPoseUpdate {
    /// The head-pose field, index 16, in degrees.
    pub head: Option<Vec3f>,
    /// The body-pose field, index 17, in degrees.
    ///
    /// Poses four parts rather than one: the *visible* stand model, one layer
    /// below the armour model, drives
    /// `right_body_stick`, `left_body_stick` and `shoulder_stick` from this same
    /// value as well as the body itself.
    pub body: Option<Vec3f>,
    /// The left-arm-pose field, index 18, in degrees.
    pub left_arm: Option<Vec3f>,
    /// The right-arm-pose field, index 19, in degrees.
    pub right_arm: Option<Vec3f>,
    /// The left-leg-pose field, index 20, in degrees.
    pub left_leg: Option<Vec3f>,
    /// The right-leg-pose field, index 21, in degrees.
    pub right_leg: Option<Vec3f>,
}

impl ArmorStandPoseUpdate {
    /// Whether this update mentions no part at all, so a fold has nothing to
    /// merge.
    ///
    /// **Not** "this stand has no pose" — every armour stand has one. See
    /// [`EntityMetadataUpdate::armor_stand_pose`] for why the two readings
    /// differ and what applying the second one costs.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.head.is_none()
            && self.body.is_none()
            && self.left_arm.is_none()
            && self.right_arm.is_none()
            && self.left_leg.is_none()
            && self.right_leg.is_none()
    }
}

impl Default for ArmorStandPose {
    /// [`Self::VANILLA_DEFAULT`], **not** the zero pose.
    ///
    /// Deliberate: every caller that reaches for a default here wants "the pose
    /// an unposed stand is in", and that is the one the game registers. A zeroed
    /// default would silently straighten every stand's arms and legs the first
    /// time one appeared without metadata.
    fn default() -> Self {
        Self::VANILLA_DEFAULT
    }
}

/// A version-free description of a mob's cosmetic *variant*, the thing that
/// changes which texture is drawn for an otherwise-identical model (sheep wool
/// colour, villager profession, horse colour/markings, biome-specific pig/cow
/// variants, and so on).
///
/// The *metadata index* and *serializer* that carry a variant are version- and
/// concrete-class-specific, so a version adapter resolves those and raises the
/// decoded payload into one of these arms. The shared model deliberately holds
/// only the version-free semantics: raw ordinals and canonical registry keys,
/// never a per-version index. Per §3.4 the index must not escape the version
/// crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntityVariant {
    /// Sheep-style dyed appearance with a shear flag. `color` is the vanilla
    /// dye/wool ordinal in `0..=15`; `sheared` is the top bit of the wool byte.
    Dyed {
        /// Dye/wool colour ordinal, `0..=15`.
        color: u8,
        /// Whether the sheep has been sheared.
        sheared: bool,
    },
    /// Villager / zombie-villager appearance. The type (biome), profession, and
    /// level are kept as canonical registry keys and a raw level so no
    /// version-specific index leaks out.
    Villager {
        /// Villager biome type, e.g. `minecraft:plains`.
        kind: Identifier,
        /// Villager profession, e.g. `minecraft:farmer`.
        profession: Identifier,
        /// Trade level (`1..=5` in vanilla).
        level: i32,
    },
    /// Horse appearance: colour and markings packed as vanilla ordinals.
    Horse {
        /// Base coat colour ordinal.
        color: u8,
        /// Markings ordinal.
        markings: u8,
    },
    /// Registry-holder variants (pig/cow/chicken/wolf/cat/frog/…): the canonical
    /// variant key, e.g. `minecraft:temperate` / `minecraft:warm` /
    /// `minecraft:cold`.
    Keyed(Identifier),
}

/// A single attribute modifier in an [`EntityAttributeSnapshot`].
///
/// `operation` is the vanilla wire id: `0` = add value, `1` = add multiplied
/// base, `2` = add multiplied total. The shared model deliberately keeps it as a
/// raw id rather than an enum so it carries no application behaviour; the entity
/// layer maps it onto its own `Operation` when folding modifiers.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityAttributeModifier {
    /// Stable modifier identity.
    pub id: Identifier,
    /// Modifier amount, interpreted per `operation`.
    pub amount: f64,
    /// Vanilla operation wire id (`0`/`1`/`2`).
    pub operation: u8,
}

/// A snapshot of one of an entity's attributes: its base value and the modifiers
/// currently applied to it.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityAttributeSnapshot {
    /// The attribute's canonical id (e.g. `minecraft:movement_speed`).
    pub attribute: Identifier,
    /// The base value before modifiers.
    pub base: f64,
    /// The modifiers applied to this attribute.
    pub modifiers: Vec<EntityAttributeModifier>,
}

/// A semantic equipment slot on an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EquipmentSlot {
    /// The entity's main-hand item.
    MainHand,
    /// The entity's off-hand item.
    OffHand,
    /// Boots / feet armor.
    Feet,
    /// Leggings / leg armor.
    Legs,
    /// Chestplate / chest armor.
    Chest,
    /// Helmet / head armor.
    Head,
    /// Animal body armor.
    Body,
    /// Saddle slot.
    Saddle,
}

impl EquipmentSlot {
    /// Slots in the wire's own ordinal order.
    ///
    /// Protocol adapters that decode raw enum ordinals should index through this
    /// table rather than duplicating the order.
    pub const ALL: [Self; 8] = [
        Self::MainHand,
        Self::OffHand,
        Self::Feet,
        Self::Legs,
        Self::Chest,
        Self::Head,
        Self::Body,
        Self::Saddle,
    ];

    /// Returns the slot for its wire ordinal.
    #[must_use]
    pub const fn from_ordinal(ordinal: u8) -> Option<Self> {
        match ordinal {
            0 => Some(Self::MainHand),
            1 => Some(Self::OffHand),
            2 => Some(Self::Feet),
            3 => Some(Self::Legs),
            4 => Some(Self::Chest),
            5 => Some(Self::Head),
            6 => Some(Self::Body),
            7 => Some(Self::Saddle),
            _ => None,
        }
    }

    /// Returns this slot's canonical name, as `minecraft:equippable`
    /// spells it.
    ///
    /// Note `Body` and `Saddle` are **not** humanoid armour: the game gates
    /// wearable-by-a-player armour to feet/legs/chest/head only. A consumer that folds `"body"` into `"chest"`
    /// lets wolf and horse armour into a player's chestplate slot.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::MainHand => "mainhand",
            Self::OffHand => "offhand",
            Self::Feet => "feet",
            Self::Legs => "legs",
            Self::Chest => "chest",
            Self::Head => "head",
            Self::Body => "body",
            Self::Saddle => "saddle",
        }
    }

    /// The slot for its canonical name — the exact inverse of
    /// [`name`](Self::name).
    ///
    /// Added for the game -> model lowering: `lodestone_game`'s opaque
    /// component map stores `minecraft:equippable` as the slot *name* string
    /// (there being no typed slot variant in a `ComponentValue`), so recovering a
    /// typed slot from it needs this direction. `None` for an unrecognised name
    /// rather than a guess — the same default-deny every other unknown in this
    /// module takes.
    ///
    /// `equipment_slot_names_round_trip` pins this against
    /// [`ALL`](Self::ALL), so the two matches cannot drift apart.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "mainhand" => Some(Self::MainHand),
            "offhand" => Some(Self::OffHand),
            "feet" => Some(Self::Feet),
            "legs" => Some(Self::Legs),
            "chest" => Some(Self::Chest),
            "head" => Some(Self::Head),
            "body" => Some(Self::Body),
            "saddle" => Some(Self::Saddle),
            _ => None,
        }
    }

    /// Returns this slot's wire ordinal.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::MainHand => 0,
            Self::OffHand => 1,
            Self::Feet => 2,
            Self::Legs => 3,
            Self::Chest => 4,
            Self::Head => 5,
            Self::Body => 6,
            Self::Saddle => 7,
        }
    }
}

/// One entity equipment slot update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityEquipment {
    /// Updated equipment slot.
    pub slot: EquipmentSlot,
    /// New item in the slot, or `None` when the slot was cleared.
    pub item: Option<ItemStack>,
}

/// One entry in a player list update.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerListEntry {
    /// Player profile UUID, when the protocol carries one.
    ///
    /// Protocol 5 identifies player-list rows only by display name. `None`
    /// preserves that wire-level absence instead of presenting an offline-mode
    /// derivation as the authenticated identity of an online-mode player.
    pub uuid: Option<Uuid>,
    /// Player name when present in the update.
    pub name: Option<String>,
    /// Current game mode when present in the update.
    pub game_mode: Option<GameMode>,
    /// Reported latency in milliseconds when present in the update.
    pub latency: Option<i32>,
    /// Display name when present in the update.
    pub display_name: Option<Text>,
    /// Whether the player should be listed when present in the update.
    pub listed: Option<bool>,
    /// Profile properties from `ADD_PLAYER`, when the update carried it.
    ///
    /// **This is where a remote player's skin comes from**, and it was decoded and
    /// thrown away until now: `v26-2`'s `read_add_player` consumed all three fields
    /// of every property into `let _`, so `minecraft:textures` never left the
    /// version crate and no remote player could have a skin.
    /// `lodestone_game::tablist` had a comment asking for exactly this carrier.
    ///
    /// `None` means the update did not include `ADD_PLAYER`; `Some(vec![])` means
    /// it did and the profile genuinely has no properties (an offline-mode server).
    /// The distinction matters because a tab-list fold merges partial updates — an
    /// absent field must keep the existing value rather than clear it.
    pub properties: Option<Vec<ProfileProperty>>,
    /// This player's announced chat-signing session, from `INITIALIZE_CHAT`.
    /// `None` means the update did not carry that action, exactly like
    /// [`Self::properties`]'s `None` — a fold must keep the existing value,
    /// not clear it.
    ///
    /// This is the receiving half of secure chat: the public
    /// key needed to verify a signed message from this player
    /// (`lodestone_auth::verify_signature`). It used to be decoded and
    /// discarded at the protocol-adapter layer with nowhere to put it —
    /// `PlayerInfoEntry::chat_session` existed in `v26-2` but this canonical
    /// struct had no field to carry it into, so no consumer could ever look
    /// a sender's key up. See `docs/secure-chat.md`.
    pub chat_session: Option<ChatSessionInfo>,
    /// Tab-list sort key from `UPDATE_LIST_ORDER`, when present in the
    /// update. `None` means the update did not carry that action; a fold
    /// must keep the existing value, exactly like [`Self::properties`].
    pub list_order: Option<i32>,
    /// Whether the player's hat (second skin layer) renders in the tab list,
    /// from `UPDATE_HAT`, when present in the update. Same `None`-means-keep
    /// merge rule as the other per-action fields on this struct.
    pub hat_visible: Option<bool>,
}

/// A player's announced chat-signing session (`RemoteChatSession.Data`):
/// their session UUID and Mojang-issued public key, as broadcast by
/// `INITIALIZE_CHAT` and carried per-entry on [`PlayerListEntry`].
///
/// `key_signature` (Mojang's own signature over `public_key`) is
/// deliberately not carried this far — nothing downstream re-verifies it
/// against Mojang's key; only the *server* does, per
/// `crates/versions/26.2/src/packets/player_info.rs`'s
/// `RemoteChatSessionData` doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSessionInfo {
    /// This player's chat-session UUID — half of the `SignedMessageLink`
    /// every one of their signed messages is hashed against.
    pub session_id: uuid::Uuid,
    /// DER-encoded (X.509 `SubjectPublicKeyInfo`) RSA public key, verbatim —
    /// what `lodestone_auth::verify_signature` parses.
    pub public_key: Vec<u8>,
    /// Public-key expiry, epoch milliseconds.
    /// Not enforced by anything here yet — the real client's own chat-trust
    /// evaluation checks this same expiry against the wall clock, which is
    /// the check this field would feed.
    pub expires_at: i64,
}

/// One entry of a player profile's property multimap, as `ADD_PLAYER` carries it.
///
/// The one that matters is `minecraft:textures`, whose `value` is base64 of a JSON
/// blob holding the skin URL and its model declaration. **Two traps live in that
/// blob rather than here**, both recorded because they cost time:
///
/// * the wide player model is spelled **`default`**, not `wide`. Reading it as
///   `wide` resolves *every* skin as wide, including slim ones, and the only
///   symptom is slightly-too-thick arms — no error and no blank texture.
/// * the payload's shape is **not** in the decompiled client; it lives in the
///   authlib jar's constant pool.
///
/// Nothing here parses or validates the value: it is server-supplied, and on an
/// online-mode server it is Mojang-signed, which is what [`Self::signature`] is
/// for. A consumer that trusts the URL should check the signature first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileProperty {
    /// Property name, e.g. `textures`.
    pub name: String,
    /// Property value. Base64 for `textures`.
    pub value: String,
    /// Mojang's signature over the value, present only in online mode.
    pub signature: Option<String>,
}

/// A Minecraft sound source category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoundCategory {
    /// Master volume category.
    Master,
    /// Music category.
    Music,
    /// Record / jukebox category.
    Record,
    /// Weather category.
    Weather,
    /// Block sound category.
    Block,
    /// Hostile entity category.
    Hostile,
    /// Neutral entity category.
    Neutral,
    /// Player sound category.
    Player,
    /// Ambient sound category.
    Ambient,
    /// Voice category.
    Voice,
    /// User-interface sound category.
    Ui,
}

impl SoundCategory {
    /// Categories in the wire's own ordinal order.
    ///
    /// Protocol adapters that decode raw enum ordinals should index through this
    /// table rather than duplicating the order.
    pub const ALL: [Self; 11] = [
        Self::Master,
        Self::Music,
        Self::Record,
        Self::Weather,
        Self::Block,
        Self::Hostile,
        Self::Neutral,
        Self::Player,
        Self::Ambient,
        Self::Voice,
        Self::Ui,
    ];

    /// Returns the category for its wire ordinal.
    #[must_use]
    pub const fn from_ordinal(ordinal: u8) -> Option<Self> {
        match ordinal {
            0 => Some(Self::Master),
            1 => Some(Self::Music),
            2 => Some(Self::Record),
            3 => Some(Self::Weather),
            4 => Some(Self::Block),
            5 => Some(Self::Hostile),
            6 => Some(Self::Neutral),
            7 => Some(Self::Player),
            8 => Some(Self::Ambient),
            9 => Some(Self::Voice),
            10 => Some(Self::Ui),
            _ => None,
        }
    }

    /// Returns this category's wire ordinal.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::Master => 0,
            Self::Music => 1,
            Self::Record => 2,
            Self::Weather => 3,
            Self::Block => 4,
            Self::Hostile => 5,
            Self::Neutral => 6,
            Self::Player => 7,
            Self::Ambient => 8,
            Self::Voice => 9,
            Self::Ui => 10,
        }
    }
}

/// Objective update mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectiveMode {
    /// Add a new objective.
    Add,
    /// Remove an existing objective.
    Remove,
    /// Change an existing objective.
    Change,
}

/// How objective scores should render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectiveRenderType {
    /// Render as a plain integer.
    Integer,
    /// Render as hearts.
    Hearts,
}

/// Optional scoreboard number formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumberFormat {
    /// Use the objective or client default.
    Default,
    /// Render no number.
    Blank,
    /// Render this fixed text instead of the number.
    Fixed(Box<Text>),
    /// Render the number styled with this colour.
    Styled(TextColor),
}

/// The sixteen named team colours.
///
/// These are the named text colours that can be used as team colours and as
/// coloured sidebar display-slot selectors. RGB text colours are intentionally
/// excluded because teams can only use the named set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TeamColor {
    /// Black.
    Black,
    /// Dark blue.
    DarkBlue,
    /// Dark green.
    DarkGreen,
    /// Dark aqua.
    DarkAqua,
    /// Dark red.
    DarkRed,
    /// Dark purple.
    DarkPurple,
    /// Gold.
    Gold,
    /// Gray.
    Gray,
    /// Dark gray.
    DarkGray,
    /// Blue.
    Blue,
    /// Green.
    Green,
    /// Aqua.
    Aqua,
    /// Red.
    Red,
    /// Light purple.
    LightPurple,
    /// Yellow.
    Yellow,
    /// White.
    White,
}

impl TeamColor {
    /// Converts this team colour to the matching text colour.
    #[must_use]
    pub const fn as_text_color(self) -> TextColor {
        match self {
            Self::Black => TextColor::Black,
            Self::DarkBlue => TextColor::DarkBlue,
            Self::DarkGreen => TextColor::DarkGreen,
            Self::DarkAqua => TextColor::DarkAqua,
            Self::DarkRed => TextColor::DarkRed,
            Self::DarkPurple => TextColor::DarkPurple,
            Self::Gold => TextColor::Gold,
            Self::Gray => TextColor::Gray,
            Self::DarkGray => TextColor::DarkGray,
            Self::Blue => TextColor::Blue,
            Self::Green => TextColor::Green,
            Self::Aqua => TextColor::Aqua,
            Self::Red => TextColor::Red,
            Self::LightPurple => TextColor::LightPurple,
            Self::Yellow => TextColor::Yellow,
            Self::White => TextColor::White,
        }
    }
}

/// A scoreboard display slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplaySlot {
    /// Tab-list player slot.
    List,
    /// Plain sidebar.
    Sidebar,
    /// Below-name slot.
    BelowName,
    /// Sidebar shown to members of a team with the given colour.
    TeamSidebar(TeamColor),
}

/// Name-tag visibility rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Visibility {
    /// Always visible.
    Always,
    /// Never visible.
    Never,
    /// Hidden from players on other teams.
    HideForOtherTeams,
    /// Hidden from players on the same team.
    HideForOwnTeam,
}

/// Team collision rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CollisionRule {
    /// Always collide.
    Always,
    /// Never collide.
    Never,
    /// Push only members of other teams.
    PushOtherTeams,
    /// Push only members of the same team.
    PushOwnTeam,
}

/// Shared parameters for team create/update actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamParameters {
    /// Shown team display name.
    pub display_name: Text,
    /// Prefix prepended to member names.
    pub prefix: Text,
    /// Suffix appended to member names.
    pub suffix: Text,
    /// Name-tag visibility rule.
    pub name_tag_visibility: Visibility,
    /// Collision rule.
    pub collision_rule: CollisionRule,
    /// Optional team colour.
    pub color: Option<TeamColor>,
    /// Whether members can damage each other.
    pub friendly_fire: bool,
    /// Whether members can see invisible teammates.
    pub see_friendly_invisibles: bool,
}

/// Team update action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamAction {
    /// Create a team with parameters and initial members.
    Create {
        /// Team parameters.
        params: Box<TeamParameters>,
        /// Initial member holder names.
        members: Vec<String>,
    },
    /// Remove a team.
    Remove,
    /// Update team parameters.
    Update {
        /// New team parameters.
        params: Box<TeamParameters>,
    },
    /// Add members to the team.
    AddMembers(Vec<String>),
    /// Remove members from the team.
    RemoveMembers(Vec<String>),
}

/// Boss bar colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BossColor {
    /// Pink.
    Pink,
    /// Blue.
    Blue,
    /// Red.
    Red,
    /// Green.
    Green,
    /// Yellow.
    Yellow,
    /// Purple.
    Purple,
    /// White.
    White,
}

/// Boss bar overlay/division style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BossOverlay {
    /// Continuous progress bar.
    Progress,
    /// Six notches.
    Notched6,
    /// Ten notches.
    Notched10,
    /// Twelve notches.
    Notched12,
    /// Twenty notches.
    Notched20,
}

/// Boss bar update action.
#[derive(Debug, Clone, PartialEq)]
pub enum BossAction {
    /// Add a boss bar.
    Add {
        /// Displayed title.
        title: Box<Text>,
        /// Current progress, normally `0.0..=1.0`.
        progress: f32,
        /// Bar colour.
        color: BossColor,
        /// Bar overlay/division style.
        overlay: BossOverlay,
        /// Whether the sky should darken.
        darken: bool,
        /// Whether boss music should play.
        music: bool,
        /// Whether world fog should appear.
        fog: bool,
    },
    /// Remove the boss bar.
    Remove,
    /// Update progress.
    UpdateProgress(f32),
    /// Update title.
    UpdateName(Box<Text>),
    /// Update colour and overlay.
    UpdateStyle {
        /// New bar colour.
        color: BossColor,
        /// New overlay/division style.
        overlay: BossOverlay,
    },
    /// Update visual/audio flags.
    UpdateFlags {
        /// Whether the sky should darken.
        darken: bool,
        /// Whether boss music should play.
        music: bool,
        /// Whether world fog should appear.
        fog: bool,
    },
}

/// A raw block-state id whose numbering source is known, but whose built-in
/// census membership has not yet been checked.
///
/// The version-free event model cannot validate a numeric state against the
/// generated 26.2 census: an older protocol family may use a different
/// numbering, and a synchronized extension may own an opaque value that no
/// built-in table can name. The adapter must therefore tag the source rather
/// than calling `lodestone_data::block_states::StateId::new` on every raw
/// value. A consumer that needs generated data validates only
/// [`Self::Canonical`] at its own boundary; it leaves [`Self::ProtocolLocal`]
/// intact until a matching version or dynamic-registry resolver is available.
///
/// This intentionally owns no `StateId`: `lodestone-model` stays independent
/// of the generated-data crate, while `StateId` remains the proof that a value
/// is one of this build's built-in states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockStateRef {
    /// A raw global state id in the canonical 26.2 numbering. It still needs
    /// generated-census validation before an indexed built-in lookup.
    Canonical(u32),
    /// A raw state id whose protocol family or synchronized extension owns the
    /// numbering. This may numerically overlap the canonical range, so it must
    /// never be range-checked as though it were a 26.2 state.
    ProtocolLocal(u32),
}

impl BlockStateRef {
    /// Tags a raw global state id emitted by the canonical 26.2 protocol.
    #[must_use]
    pub const fn canonical(raw: u32) -> Self {
        Self::Canonical(raw)
    }

    /// Tags a raw state id from a protocol-local or dynamic registry.
    #[must_use]
    pub const fn protocol_local(raw: u32) -> Self {
        Self::ProtocolLocal(raw)
    }

    /// The original numeric value, for the source-specific resolver that owns
    /// this reference's numbering.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::Canonical(raw) | Self::ProtocolLocal(raw) => raw,
        }
    }
}

/// A level event's payload, retaining block-state numbering provenance for the
/// one event whose payload names a block state.
///
/// Most level-event payloads are event-specific signed integers and remain
/// [`Self::Raw`]. Event `2001` carries a state id instead; adapters turn that
/// payload into [`Self::BlockState`] while they still know whether the wire
/// numbering is canonical or protocol-local. This prevents a shell consumer
/// from recovering intent by range-checking a bare integer after that source
/// information has already been lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LevelEventData {
    /// An event-specific signed payload with no block-state interpretation.
    Raw(i32),
    /// Event `2001`'s pre-destruction block state.
    BlockState(BlockStateRef),
}

impl LevelEventData {
    /// The original 32 payload bits, for callers that deliberately handle an
    /// event's protocol-specific data rather than a built-in block-state
    /// lookup.
    #[must_use]
    pub const fn raw_i32(self) -> i32 {
        match self {
            Self::Raw(raw) => raw,
            Self::BlockState(state) => state.raw() as i32,
        }
    }
}

/// A `minecraft:particle_type` registry entry's type-specific payload —
/// [`ClientEvent::Particles`]'s `options`.
///
/// Most vanilla particle types are a bare `SimpleParticleType` with no
/// payload at all ([`Self::None`], the common case); a handful carry extra
/// fields read immediately after the registry id (`DustParticleOptions`,
/// `BlockParticleOption`, `ItemParticleOption`, …). Adding a variant here
/// does not by itself decode anything — the adapter's `LEVEL_PARTICLES` arm
/// (`crates/versions/26.2/src/adapter/chunk.rs`) is what parses a payload out
/// of the wire bytes based on the resolved particle name, and only for the
/// names it recognises; every other name still resolves to [`Self::None`],
/// same as before this type existed.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ParticleOptions {
    /// No type-specific payload.
    #[default]
    None,
    /// `minecraft:dust` (`DustParticleOptions`).
    Dust {
        /// Colour, unpacked from the wire's packed RGB24 `i32` to `[0, 1]`
        /// components (`ARGB.vector3fFromRGB24`).
        color: [f32; 3],
        /// Size multiplier (`ScalableParticleOptionsBase::getScale`).
        scale: f32,
    },
    /// `minecraft:dust_color_transition` (`DustColorTransitionOptions`) — the
    /// sculk-to-redstone sibling of [`Self::Dust`] that lerps colour over its
    /// life instead of holding one fixed.
    DustColorTransition {
        /// Starting colour, same unpacking as [`Self::Dust`]'s `color`.
        from_color: [f32; 3],
        /// Ending colour.
        to_color: [f32; 3],
        /// Size multiplier.
        scale: f32,
    },
    /// `minecraft:effect` and `minecraft:instant_effect`
    /// (`SpellParticleOption`) — the potion-effect motes trailing an entity
    /// under a status effect, and a splash potion's instant burst.
    Spell {
        /// Tint, unpacked from the wire's packed RGB24 `i32` the same way
        /// [`Self::Dust`]'s `color` is. Vanilla's own spell-particle option's own accessors
        /// read only the low three bytes (its own red/green/blue channel reads), so
        /// the top byte of the wire word is not an alpha here — that is
        /// [`Self::Color`]'s field, on a different option type.
        color: [f32; 3],
        /// Velocity multiplier (`SpellParticleOption::getPower`, applied by
        /// the provider through `Particle.setPower`). Defaults to `1.0` in the
        /// data codec but is unconditional on the wire.
        power: f32,
    },
    /// `minecraft:entity_effect` (`ColorParticleOption`) — the ambient motes a
    /// mob under a status effect, or a lingering potion's cloud, gives off.
    ///
    /// Distinct from [`Self::Spell`] despite both driving the same
    /// `SpellParticle` class: this one is a **four**-component ARGB word with
    /// no power field, and the two are not interchangeable on the wire (8
    /// bytes against 4).
    Color {
        /// Tint and alpha, unpacked from the wire's packed **ARGB** `i32` —
        /// `[ARGB.red, ARGB.green, ARGB.blue, ARGB.alpha]`, each `/ 255.0`.
        /// The alpha byte is the top one and is genuinely used
        /// (vanilla's own mob-effect spell-particle provider sets alpha with it), so
        /// dropping it makes every ambient effect mote fully opaque.
        color: [f32; 4],
    },
    /// `minecraft:dragon_breath` (`PowerParticleOption`) — a bare velocity
    /// multiplier and nothing else.
    ///
    /// Its own variant rather than a reuse of [`Self::Spell`]'s `power`: this
    /// option class carries no colour at all (`DragonBreathParticle` draws its
    /// purple out of the RNG), so the wire payload is four bytes against
    /// `SpellParticleOption`'s eight and the two are not interchangeable.
    Power {
        /// Velocity multiplier (`PowerParticleOption::getPower`, applied by
        /// the provider through `Particle.setPower`).
        power: f32,
    },
    /// `minecraft:sculk_charge` (`SculkChargeParticleOptions`).
    SculkCharge {
        /// Roll about the view axis, in radians — the one thing that makes a
        /// sculk charge's motes lie along the direction the charge is
        /// spreading rather than all sharing one orientation.
        roll: f32,
    },
    /// The `BlockParticleOption` family — `minecraft:block`,
    /// `minecraft:block_marker`, `minecraft:block_crumble`,
    /// `minecraft:dust_pillar` and `minecraft:falling_dust`.
    ///
    /// One payload type shared by five registry entries whose *providers* have
    /// nothing else in common: three build a `TerrainParticle` (with different
    /// speeds and lifetimes), one builds a physics-free marker quad and one
    /// builds a sheet-textured falling mote tinted from the block. The wire
    /// payload is identical for all five, so they share this variant and the
    /// emitters differ — reading the shared payload as a shared *behaviour* is
    /// what would make a `block_marker` fall and a `falling_dust` wear the
    /// block's own texture.
    BlockState {
        /// The block state, by **block-state** network id — not a block id and
        /// not an item id. [`BlockStateRef::Canonical`] is the 26.2 numbering
        /// that a built-in renderer may validate against
        /// `lodestone_data::block_states`; [`BlockStateRef::ProtocolLocal`]
        /// stays opaque for a version-aware or dynamic-registry consumer.
        state: BlockStateRef,
    },
}

/// Things that happen to the client after a version adapter lifts a packet into
/// the canonical model.
///
/// **Adding a variant here is not enough to make it reach anything.** Every new
/// variant must also be given an arm in [`route`], which is an exhaustive match
/// in this same crate and therefore a *compile error* until you write it. See
/// [`Route`] and `docs/event-routing.md`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    /// The client entered the game world.
    Login {
        /// Local player entity id.
        entity_id: i32,
        /// Current game mode.
        game_mode: GameMode,
        /// Current dimension.
        dimension: DimensionId,
    },
    /// Chat or system text was received.
    Chat {
        /// Message text.
        text: Text,
        /// Message kind.
        kind: ChatKind,
        /// The sender's profile UUID — the filter key. Only a signed
        /// player-chat message carries one on the wire (`PLAYER_CHAT`); system,
        /// disguised, and action-bar messages have none (the server
        /// pre-decorates the display name into the text), and the legacy
        /// protocol families' chat packets carry no sender field at all. A
        /// consumer that filters by hidden senders must treat `None` as "not a
        /// player message" and show it.
        sender: Option<Uuid>,
        /// Signed-chat acknowledgement metadata, when this chat contributes to
        /// the last-seen acknowledgement window.
        ack: Option<ChatAckInfo>,
    },
    /// The server disconnected the client.
    Disconnect {
        /// Disconnect reason.
        reason: Text,
    },
    /// The session ended because of a **client-side** failure: a transport
    /// error, a read timeout, an adapter rejection, an online-mode
    /// authentication failure. The opposite of [`ClientEvent::Disconnect`],
    /// which is the *server* telling us why.
    ///
    /// # Why this is an event and not just a return value
    ///
    /// The driver already returns every one of these as a
    /// `SessionOutcome::Failed(ClientError)`, and that value is unreachable to
    /// a consumer that only holds a shared handle: taking it consumes the
    /// handle by value, so a shell holding an `Arc<ClientHandle>` cannot. What
    /// such a consumer observes instead is the event stream simply *ending* —
    /// indistinguishable from a clean close, which is why a failed join used to
    /// reach the screen as a synthesised "stream closed" while the real cause
    /// went only to the log. Emitting the failure means the terminal reason
    /// travels the same channel every other session event does, and arrives
    /// before the channel closes.
    ///
    /// # The payload is a plain `String`, deliberately
    ///
    /// Unlike [`ClientEvent::Disconnect`]'s [`Text`], nothing here came off the
    /// wire and nothing is translatable: this is *our* error, rendered with its
    /// full `source()` chain, and a consumer that wants a `Text` wraps it in a
    /// literal node (which every translator is a no-op on).
    SessionFailed {
        /// The error and its `source()` chain, joined with `": "`.
        reason: String,
    },
    /// A keep-alive challenge was received.
    KeepAlive {
        /// Keep-alive id.
        id: i64,
    },
    /// A ping challenge was received (distinct from keep-alive; used for
    /// latency measurement outside the tick-driven keep-alive cadence).
    Ping {
        /// Id that must be echoed back via [`crate::ClientAction::PongResponse`].
        id: i32,
    },
    /// The player was teleported.
    TeleportPlayer {
        /// Target position or relative delta indicated by `flags`.
        pos: Vec3,
        /// Target rotation or relative rotation indicated by `flags`.
        rotation: Rotation,
        /// Relative component flags.
        flags: TeleportFlags,
    },
    /// An entity appeared in the world.
    EntitySpawned {
        /// Entity id.
        entity_id: i32,
        /// Entity UUID when known.
        uuid: Option<Uuid>,
        /// Canonical entity type key.
        entity_type: ResourceKey,
        /// Spawn position.
        pos: Vec3,
        /// Spawn rotation.
        rotation: Rotation,
        /// Spawn velocity when known.
        velocity: Option<Vec3>,
    },
    /// A player entity's profile name was supplied with its spawn identity.
    ///
    /// Protocol 5 carries this beside the remote player's UUID in its named
    /// spawn packet. Keeping it separate from [`ClientEvent::PlayerListUpdate`]
    /// preserves the only wire-authored correlation between that UUID-bearing
    /// entity and this era's name-keyed player-list row.
    PlayerProfileNamed {
        /// Server-assigned entity id receiving the profile name.
        entity_id: i32,
        /// Profile name exactly as supplied by the entity spawn packet.
        profile_name: String,
    },
    /// An entity moved or rotated.
    EntityMoved {
        /// Entity id.
        entity_id: i32,
        /// Movement payload.
        movement: EntityMovement,
        /// New rotation when included.
        rotation: Option<Rotation>,
        /// Whether the entity is on the ground.
        on_ground: bool,
    },
    /// An entity's velocity changed.
    EntityVelocity {
        /// Entity id.
        entity_id: i32,
        /// Velocity vector.
        velocity: Vec3,
    },
    /// One or more entities were removed.
    EntityRemoved {
        /// Removed entity ids.
        entity_ids: Vec<i32>,
    },
    /// An entity's metadata changed (spawn-time or incremental).
    ///
    /// The adapter has already resolved the version-specific indices and
    /// serializers into the version-free [`EntityMetadataUpdate`]; only the
    /// fields the packet carried are `Some`.
    EntityMetadataUpdated {
        /// Entity id.
        entity_id: i32,
        /// The fields this packet updated.
        metadata: EntityMetadataUpdate,
    },
    /// A falling block's imitated block state, from its spawn packet's
    /// **Object Data** field.
    ///
    /// # Why this is its own event and not a field on [`EntitySpawned`]
    ///
    /// `ADD_ENTITY`'s trailing VarInt is vanilla's "Object Data": one field whose
    /// meaning is decided entirely by the entity type, and which each type reads in
    /// its own recreate-from-packet override. Vanilla's own falling-block
    /// entity's is
    /// to resolve the packet's Object Data field to a block state by its global
    /// state id and store it. Lowering it as a
    /// per-type event rather than as an opaque integer on the shared spawn event
    /// keeps the *interpretation* in the adapter that has the version's state table,
    /// which is the same reason [`EntityMetadataUpdated`](Self::EntityMetadataUpdated)
    /// carries resolved fields rather than raw indices.
    ///
    /// **This is the only channel by which the state travels.**
    /// Vanilla's own falling-block synced-data registration registers only its
    /// start-position field and nothing
    /// else, so the block state is never in a `SET_ENTITY_DATA` packet. A consumer
    /// that ignores this draws every falling block as whatever state id `0` happens
    /// to be, with nothing logged anywhere.
    ///
    /// Emitted immediately after the entity's own [`EntitySpawned`](Self::EntitySpawned),
    /// so a consumer keyed on the entity id always has the entity first.
    FallingBlockState {
        /// Entity id.
        entity_id: i32,
        /// The block state the entity is imitating. Its source tag is retained
        /// until a version-aware consumer can resolve it safely.
        block_state: BlockStateRef,
    },
    /// A projectile's **owner** entity id, from its spawn packet's
    /// **Object Data** field — the same trailing VarInt
    /// [`FallingBlockState`](Self::FallingBlockState) reads, under the reading
    /// `Projectile.getAddEntityPacket` gives it.
    ///
    /// `Projectile` writes `owner == null ? 0 : owner.getId()` there, and
    /// `FishingHook` overrides that to `owner == null ? this.getId() : owner.getId()`
    /// so the field is never `0` for a hook. Like the falling block's state, this
    /// is the **only** channel it travels on: neither `Projectile` nor
    /// `FishingHook.defineSynchedData` registers an owner accessor, so no
    /// `SET_ENTITY_DATA` packet ever carries it and a consumer that ignores this
    /// event can never learn who cast the rod.
    ///
    /// Emitted immediately after the entity's own [`EntitySpawned`](Self::EntitySpawned),
    /// so a consumer keyed on the entity id always has the entity first.
    ///
    /// Adapters emit this only for the types whose Object Data they have
    /// *established* means an owner id — today that is `minecraft:fishing_bobber`,
    /// the one type with a live consumer (the line drawn back to the caster's
    /// hand). Widening it to every `Projectile` subclass is a decode change, not a
    /// new event.
    ProjectileOwner {
        /// The projectile's entity id.
        entity_id: i32,
        /// The owner's entity id, as the spawn packet's Object Data field
        /// reported it.
        owner_id: i32,
    },
    /// An entity's attributes were (re)published.
    ///
    /// Each snapshot fully replaces the named attribute's base value and modifier
    /// set for that entity; attributes not named are left unchanged.
    EntityAttributesUpdated {
        /// Entity id.
        entity_id: i32,
        /// The attributes carried by this packet.
        attributes: Vec<EntityAttributeSnapshot>,
    },
    /// One or more equipment slots changed on an entity.
    EntityEquipmentUpdated {
        /// Entity id.
        entity_id: i32,
        /// Updated equipment slots.
        equipment: Vec<EntityEquipment>,
    },
    /// Player health, food, or saturation changed.
    HealthChanged {
        /// Current health.
        health: f32,
        /// Current food level.
        food: i32,
        /// Current saturation.
        saturation: f32,
    },
    /// The player died. The server holds a dead player on the death screen and
    /// stops streaming chunks until it receives a respawn request, so a headless
    /// client must react to this (see the client's respawn policy).
    Death {
        /// The death message shown on the death screen.
        message: Text,
    },
    /// World time changed.
    TimeChanged {
        /// Total world age.
        world_age: i64,
        /// Current time of day.
        time_of_day: i64,
    },
    /// Weather state or intensity changed.
    ///
    /// Fields are optional because the server can send one aspect at a time:
    /// start/stop raining, rain level, or thunder level.
    WeatherChanged {
        /// Whether rain is now active, when that changed.
        raining: Option<bool>,
        /// Rain intensity, when that changed.
        rain_level: Option<f32>,
        /// Thunder intensity, when that changed.
        thunder_level: Option<f32>,
    },
    /// The local player's game mode changed.
    GameModeChanged {
        /// New game mode.
        game_mode: GameMode,
    },
    /// The world's default spawn position changed.
    SpawnPositionChanged {
        /// Dimension containing the spawn position.
        dimension: DimensionId,
        /// New default spawn block position.
        pos: BlockPos,
        /// Spawn yaw in degrees.
        angle: f32,
        /// Spawn pitch in degrees.
        pitch: f32,
    },
    /// The local player's ability flags or movement speeds changed.
    AbilitiesChanged {
        /// Whether the player is invulnerable.
        invulnerable: bool,
        /// Whether the player is currently flying.
        flying: bool,
        /// Whether the player may fly.
        can_fly: bool,
        /// Whether the player may instantly build/break.
        instabuild: bool,
        /// Flying speed multiplier.
        flying_speed: f32,
        /// Walking speed multiplier.
        walking_speed: f32,
    },
    /// A positioned sound should play.
    Sound {
        /// Canonical sound event key.
        sound: ResourceKey,
        /// Sound source category.
        category: SoundCategory,
        /// Sound origin.
        pos: Vec3,
        /// Volume multiplier.
        volume: f32,
        /// Pitch multiplier.
        pitch: f32,
        /// Optional fixed audible range overriding the volume-derived default.
        fixed_range: Option<f32>,
        /// Random seed for deterministic sound variant selection.
        seed: i64,
    },
    /// A sound attached to an entity should play.
    EntitySound {
        /// Canonical sound event key.
        sound: ResourceKey,
        /// Sound source category.
        category: SoundCategory,
        /// Entity id the sound follows.
        entity_id: i32,
        /// Volume multiplier.
        volume: f32,
        /// Pitch multiplier.
        pitch: f32,
        /// Optional fixed audible range overriding the volume-derived default.
        fixed_range: Option<f32>,
        /// Random seed for deterministic sound variant selection.
        seed: i64,
    },
    /// A level event occurred at a block position.
    ///
    /// The event code is Mojang's gameplay-level event code. It is not a
    /// registry id for blocks, items, entities, or sounds.
    LevelEvent {
        /// Gameplay event code.
        event: i32,
        /// Event block position.
        pos: BlockPos,
        /// Event-specific data. Event `2001` carries [`LevelEventData::BlockState`]
        /// so its state-id source survives until a version-aware consumer or a
        /// generated-model boundary can resolve it.
        data: LevelEventData,
        /// Whether the event is global rather than distance-limited.
        global: bool,
    },
    /// An explosion occurred.
    ///
    /// One variant carrying two different wire shapes, deliberately: every
    /// client protocol family from v1-8 (1.8.9) through v1-14 (1.16.5) sends
    /// `packet_explosion`'s own list of removed-block offsets on this same
    /// packet, while 26.2's explosion packet dropped that list in
    /// favour of a bare block **count** used only to scale cosmetic particle
    /// spawning — the real removals now arrive as
    /// ordinary block-update events instead. See [`Self::affected_blocks`]'s
    /// own doc for what an empty list means on each family, rather than
    /// giving 26.2 a second variant for what is, model-side, the same event:
    /// a blast at a position with a radius, optionally pushing this client.
    Explosion {
        /// World-space explosion centre.
        pos: Vec3,
        /// Blast radius, in blocks.
        radius: f32,
        /// Blocks the explosion removed, as integer offsets from `pos`
        /// (`pos.floor() + offset` is the removed block's position) —
        /// `packet_explosion`'s own `affected_block_offsets` on every
        /// pre-26.2 family (v1-8/v1-9/v1-14), each of which puts the list
        /// directly on this packet.
        ///
        /// **Always empty on a 26.2 (`v26-2`) connection.**
        /// The explosion packet on that version carries only a block count
        /// there —
        /// no positions at all — because the real removals now arrive as
        /// separate block-update events. A fold reading this field for
        /// "which blocks did this explosion remove" must treat an empty list
        /// as "not given by this packet", not as "the explosion removed
        /// nothing" — the two are indistinguishable from this field alone,
        /// which is why this variant exists rather than pretending 26.2 has
        /// the same fidelity.
        affected_blocks: Vec<[i8; 3]>,
        /// This client's own knockback impulse from the blast, if any — an
        /// additive velocity delta, not an absolute velocity.
        /// `player_motion_x/y/z` on the legacy wire (present unconditionally,
        /// `[0.0; 3]` when this player is outside the blast — map that to
        /// `Some([0.0; 3])` there, since the field genuinely is on the wire);
        /// vanilla's own player-knockback field on 26.2, which is a real `Optional<Vec3>` on the
        /// wire and should map to `None` one-for-one.
        knockback: Option<Vec3>,
    },
    /// Particles should spawn.
    Particles {
        /// Canonical particle type key.
        particle: ResourceKey,
        /// Whether the particles should be visible at long distance.
        long_distance: bool,
        /// Whether the particles survive the **Minimal** particle setting —
        /// the level particles packet's own "always show" flag, which
        /// the client's particle-level calculation turns into a one-in-ten
        /// reprieve rather than an exemption.
        ///
        /// Distinct from `long_distance`, which is the *distance* cutoff, and
        /// the two are independent on the wire. **`false` on every legacy
        /// family**, and honestly so rather than by omission: the field does
        /// not exist on the pre-26.2 particle packets at all (1.12's
        /// particle packet carries only the distance flag and nothing else), so
        /// there is no value to carry and `false` is what the corresponding
        /// unconditional-particle call passes.
        always_show: bool,
        /// Particle origin.
        pos: Vec3,
        /// Randomized offset bounds.
        offset: Vec3f,
        /// Particle speed parameter.
        max_speed: f32,
        /// Number of particles to spawn.
        count: i32,
        /// The particle type's own extra payload, if it carries one. See
        /// [`ParticleOptions`].
        options: ParticleOptions,
    },
    /// A container's full content changed.
    ContainerContent {
        /// Window/container id.
        window_id: i32,
        /// Container synchronization state id.
        state_id: i32,
        /// Slot contents in container order.
        items: Vec<Option<ItemStack>>,
        /// Item carried by the cursor.
        carried_item: Option<ItemStack>,
    },
    /// A single container slot changed.
    ContainerSlot {
        /// Window/container id.
        window_id: i32,
        /// Container synchronization state id.
        state_id: i32,
        /// Slot index.
        slot: i32,
        /// New slot contents.
        item: Option<ItemStack>,
    },
    /// A container/menu property changed.
    ///
    /// These property ids are menu-local channels such as furnace progress,
    /// brewing progress, or enchantment costs. They are not registry ids.
    ContainerData {
        /// Window/container id.
        window_id: i32,
        /// Menu-local property id.
        property: i32,
        /// New property value.
        value: i32,
    },
    /// The server closed a container/menu screen.
    ScreenClosed {
        /// Window/container id.
        window_id: i32,
    },
    /// A container/menu screen opened.
    ScreenOpened {
        /// Window/container id.
        window_id: i32,
        /// Canonical menu type key.
        menu_type: ResourceKey,
        /// Screen title.
        title: Text,
    },
    /// A scoreboard objective was added, removed, or changed.
    ObjectiveUpdate {
        /// Objective name.
        name: String,
        /// Update mode.
        mode: ObjectiveMode,
        /// Display name for add/change; absent for remove.
        display_name: Option<Text>,
        /// Render type for add/change; absent for remove.
        render_type: Option<ObjectiveRenderType>,
        /// Objective default number format for add/change.
        number_format: Option<NumberFormat>,
    },
    /// A scoreboard display slot changed.
    DisplayObjective {
        /// Display slot being assigned.
        slot: DisplaySlot,
        /// Objective name, or `None` to clear the slot.
        objective: Option<String>,
    },
    /// A score was added or changed.
    ScoreUpdate {
        /// Score holder name.
        holder: String,
        /// Objective name.
        objective: String,
        /// Score value.
        value: i32,
        /// Optional display override for the holder.
        display: Option<Text>,
        /// Optional per-score number format.
        number_format: Option<NumberFormat>,
    },
    /// A score was reset.
    ScoreReset {
        /// Score holder name.
        holder: String,
        /// Objective to reset, or `None` to reset all objectives for the holder.
        objective: Option<String>,
    },
    /// A team was created, removed, changed, or had membership changed.
    TeamUpdate {
        /// Team name.
        name: String,
        /// Team action.
        action: TeamAction,
    },
    /// A boss bar was added, removed, or changed.
    BossBarUpdate {
        /// Boss bar id.
        id: Uuid,
        /// Boss bar action.
        action: BossAction,
    },
    /// The player list changed.
    PlayerListUpdate {
        /// Updated player entries.
        entries: Vec<PlayerListEntry>,
    },
    /// A chunk's data at `pos` became available or was replaced.
    ///
    /// This is a lightweight *notification*, not a data carrier. The adapter
    /// applies the fully decoded, version-free chunk (block-state and biome
    /// sections, light, heightmaps, block entities) directly into the
    /// client-owned [`World`](lodestone_world::World) as it decodes the packet;
    /// consumers read that data by querying the world, keyed by `pos`.
    ///
    /// Deliberately carrying only the position keeps this event cheap and, more
    /// importantly, keeps world correctness independent of consumer liveness:
    /// the event travels a bounded channel, so a payload here could be dropped
    /// under backpressure, and a dropped `ChunkLoaded` would be an unrecoverable
    /// hole. As a bare signal it is idempotent and safe to coalesce — treat it
    /// as "the region at `pos` is dirty; re-read or re-mesh it."
    ChunkLoaded {
        /// Chunk position; look the data up in the world by this key.
        pos: ChunkPos,
    },
    /// A chunk became unavailable. The adapter has already removed it from the
    /// client-owned world; this notifies consumers to drop anything derived
    /// from `pos` (a mesh, a collision cache).
    ChunkUnloaded {
        /// Chunk position.
        pos: ChunkPos,
    },
    /// One or more blocks changed inside an already-loaded section, and the
    /// adapter has already applied them to the client-owned
    /// [`World`](lodestone_world::World).
    ///
    /// Like [`ClientEvent::ChunkLoaded`] this is a **dirty-region signal**, not
    /// a data carrier — read the new states from the world. It exists
    /// separately from `ChunkLoaded` because the region is far smaller: a
    /// consumer that re-derives geometry needs to redo one section and only the
    /// neighbours the changed cells actually touch, where a chunk arrival
    /// invalidates a whole column and its horizontal seams. Overloading
    /// `ChunkLoaded` for block updates forces the consumer to conflate the two
    /// and pay the column-sized cost for every redstone tick.
    ///
    /// `section` is in section coordinates (block >> 4 on every axis).
    /// `blocks` lists the section-relative `(x, y, z)` of each changed cell so a
    /// consumer can tell an interior edit — which cannot affect a neighbouring
    /// section — from one on a boundary, which can.
    SectionBlocksChanged {
        /// Section coordinates of the section that changed.
        section: SectionPos,
        /// Section-relative `(x, y, z)`, each `0..16`, of every changed cell.
        blocks: Vec<[u8; 3]>,
    },
    /// A block-triggering "block event" (e.g. a note block playing, a piston
    /// starting to move, a chest lid animating) occurred.
    ///
    /// `b0`/`b1` are opaque per-block-type parameters; their meaning depends on
    /// `block` and is a rendering/audio concern for the consumer, not something
    /// the adapter interprets.
    BlockEvent {
        /// Block position.
        pos: BlockPos,
        /// First event parameter, meaning depends on `block`.
        b0: u8,
        /// Second event parameter, meaning depends on `block`.
        b1: u8,
        /// Canonical block type key.
        block: ResourceKey,
    },
    /// A block's break-progress overlay changed.
    ///
    /// `progress` is the raw wire byte (vanilla uses `0..=9`ish for visible
    /// stages and other values to clear the overlay); the adapter does not
    /// reinterpret it.
    BlockDestruction {
        /// Id of the entity breaking the block (usually a player).
        entity_id: i32,
        /// Block position.
        pos: BlockPos,
        /// Raw break-stage byte.
        progress: u8,
    },
    /// The server acknowledged a client-predicted block change up to
    /// `sequence`; predictions at or before it can be reconciled/discarded.
    BlockChangedAck {
        /// Acknowledged sequence number.
        sequence: i32,
    },
    /// The chunk-loading center moved (usually following the player).
    ChunkCacheCenterChanged {
        /// New center chunk X.
        x: i32,
        /// New center chunk Z.
        z: i32,
    },
    /// The server's view/loading radius changed.
    ChunkCacheRadiusChanged {
        /// New radius, in chunks.
        radius: i32,
    },
    /// The simulation (entity-ticking) distance changed.
    SimulationDistanceChanged {
        /// New simulation distance, in chunks.
        distance: i32,
    },
    /// An entity-specific status/animation code was triggered.
    ///
    /// `status` is Mojang's raw per-entity-type event byte (e.g. spawn
    /// particles, play a sound, alter behavior); its meaning depends on the
    /// entity's type and is a consumer-side concern.
    EntityStatus {
        /// Entity id.
        entity_id: i32,
        /// Raw status/event byte.
        status: u8,
    },
    /// An entity's head yaw changed independently of its body rotation.
    EntityHeadRotation {
        /// Entity id.
        entity_id: i32,
        /// New head yaw, in degrees.
        head_yaw: f32,
    },
    /// An entity's passenger list changed.
    EntityPassengersChanged {
        /// Vehicle entity id.
        vehicle_id: i32,
        /// Passenger entity ids, in mounting order.
        passenger_ids: Vec<i32>,
    },
    /// An entity's leash holder changed.
    EntityLeashed {
        /// Leashed entity id.
        entity_id: i32,
        /// Holder entity id, or `None` if the leash was removed.
        holder_id: Option<i32>,
    },
    /// An item entity was picked up (visually flies to the collector before
    /// despawning).
    ItemPickup {
        /// Item entity id.
        item_entity_id: i32,
        /// Collecting entity id (usually a player).
        player_id: i32,
        /// Stack size collected.
        amount: i32,
    },
    /// An entity took damage.
    ///
    /// `damage_type_id` is the raw `minecraft:damage_type` registry network id.
    /// Unlike other registries this adapter resolves to canonical keys,
    /// `minecraft:damage_type` is a purely data-driven registry with no default
    /// protocol ids: its network id is assigned per-connection by the order the
    /// server's registry-sync configuration packets listed entries, which this
    /// adapter does not currently track. Carrying the raw id here is honest
    /// about that gap rather than guessing a mapping.
    EntityDamaged {
        /// Damaged entity id.
        entity_id: i32,
        /// Raw `minecraft:damage_type` registry network id (unresolved; see above).
        damage_type_id: i32,
        /// Entity id that caused the damage (e.g. an arrow's shooter), when known.
        cause_id: Option<i32>,
        /// Entity id that directly dealt the damage (e.g. the arrow itself), when known.
        direct_id: Option<i32>,
        /// World-space damage source position, when the damage had no direct entity source.
        source_pos: Option<Vec3>,
    },
    /// An entity played its hurt animation without necessarily taking damage
    /// (e.g. a client-side prediction correction).
    EntityHurtAnimation {
        /// Entity id.
        entity_id: i32,
        /// Yaw the hurt animation should play at, in degrees.
        yaw: f32,
    },
    /// An entity played a hand-swing or hit-effect animation.
    EntityAnimation {
        /// Entity id.
        entity_id: i32,
        /// Animation kind.
        action: AnimationAction,
    },
    /// A mob effect (potion effect) was applied to or refreshed on an entity.
    MobEffectApplied {
        /// Entity id.
        entity_id: i32,
        /// Canonical mob effect key.
        effect: ResourceKey,
        /// Effect amplifier (0 = level I).
        amplifier: i32,
        /// Remaining duration, in ticks.
        duration_ticks: i32,
        /// Whether the effect originated from ambient sources (e.g. a beacon).
        ambient: bool,
        /// Whether particles are shown.
        visible: bool,
        /// Whether the effect icon is shown in the HUD.
        show_icon: bool,
        /// Whether the effect blends its particle color with others.
        blend: bool,
    },
    /// A mob effect was removed from an entity.
    MobEffectRemoved {
        /// Entity id.
        entity_id: i32,
        /// Canonical mob effect key.
        effect: ResourceKey,
    },
    /// The vehicle the player is riding moved to an absolute position.
    VehicleMoved {
        /// New absolute position.
        pos: Vec3,
        /// New yaw, in degrees.
        yaw: f32,
        /// New pitch, in degrees.
        pitch: f32,
    },
    /// The local player's selected hotbar slot changed.
    HeldSlotChanged {
        /// New selected hotbar slot (`0..9`).
        slot: i32,
    },
    /// The local player's experience bar or level changed.
    ExperienceChanged {
        /// Progress toward the next level, in `0.0..1.0`.
        progress: f32,
        /// Current experience level.
        level: i32,
        /// Total accumulated experience points.
        total: i32,
    },
    /// The item held by the cursor (dragged item) changed.
    CursorItemChanged {
        /// New cursor item, or `None` if empty.
        item: Option<ItemStack>,
    },
    /// A slot in the local player's own inventory changed outside of an open
    /// container screen.
    InventorySlotChanged {
        /// Inventory slot index.
        slot: i32,
        /// New slot contents.
        item: Option<ItemStack>,
    },
    /// One or more entries were removed from the player list.
    PlayerListRemove {
        /// Removed player profile ids.
        profile_ids: Vec<Uuid>,
    },
    /// One or more name-keyed entries were removed from the player list.
    ///
    /// Protocol 5 has no profile UUID in either its add or remove shape, so its
    /// display name is the only identity available for correlating the pair.
    PlayerListRemoveByName {
        /// Removed player display names, exactly as received on the wire.
        profile_names: Vec<String>,
    },
    /// The main title text changed.
    TitleText {
        /// New title text.
        text: Text,
    },
    /// The subtitle text changed.
    SubtitleText {
        /// New subtitle text.
        text: Text,
    },
    /// Titles were cleared/hidden.
    TitlesCleared {
        /// Whether the fade/stay/fade-out timings should also reset to defaults.
        reset_times: bool,
    },
    /// The title fade-in/stay/fade-out timings changed.
    TitlesAnimation {
        /// Fade-in duration, in ticks.
        fade_in: i32,
        /// Stay duration, in ticks.
        stay: i32,
        /// Fade-out duration, in ticks.
        fade_out: i32,
    },
    /// An item (or shared cooldown group) started its use cooldown.
    ItemCooldown {
        /// Cooldown group identifier (an item id or a shared group name).
        group: ResourceKey,
        /// Cooldown duration, in ticks.
        duration_ticks: i32,
    },
    /// The world's difficulty (and whether it is locked) changed.
    DifficultyChanged {
        /// New difficulty.
        difficulty: Difficulty,
        /// Whether the difficulty is locked from further changes in the UI.
        locked: bool,
    },
    /// The server instructed the local player to rotate to (or by) a specific
    /// yaw/pitch, from the player rotation packet.
    PlayerRotationSet {
        /// New (or delta) body yaw, in degrees.
        y_rot: f32,
        /// Whether `y_rot` is relative to the current yaw rather than absolute.
        relative_y: bool,
        /// New (or delta) pitch, in degrees.
        x_rot: f32,
        /// Whether `x_rot` is relative to the current pitch rather than absolute.
        relative_x: bool,
    },
    /// The client's camera was attached to (or detached from) an entity, from
    /// the set camera packet.
    CameraSet {
        /// The entity id the camera now follows. Vanilla sends the local
        /// player's own id to reset the camera to the first-person view.
        entity_id: i32,
    },
    /// A written book screen should open, from the open book packet.
    BookOpened {
        /// `true` for the main hand, `false` for the off hand.
        main_hand: bool,
    },
    /// A sound (or sounds) should stop playing, from
    /// the stop sound packet. Absent fields are wildcards: `sound: None`
    /// stops every sound in `category` (or all sounds if `category` is also
    /// `None`), not "no sound".
    SoundStopped {
        /// Sound to stop, or `None` to match any sound.
        sound: Option<ResourceKey>,
        /// Category to restrict the stop to, or `None` to match any category.
        category: Option<SoundCategory>,
    },
    /// The player list header/footer text changed, from
    /// the tab list packet.
    TabListChanged {
        /// Header text shown above the player list.
        header: Text,
        /// Footer text shown below the player list.
        footer: Text,
    },
    /// The server's stored per-book recipe-book UI state, from
    /// the recipe-book-settings packet.
    ///
    /// Four books in the game's own fixed order, each carrying two booleans — the
    /// wire form is exactly eight bytes with no length prefix and no discriminator.
    /// Named fields rather than a `Vec`
    /// deliberately: the shape is fixed, so a collection would admit a length this
    /// packet cannot have.
    ///
    /// This is the *inbound* half of a round trip whose outbound half already
    /// existed — [`crate::action::ClientAction::SetRecipeBookSettings`] has been
    /// encoded by the adapters for some time, so the client could tell the server
    /// its book state and could never be told the state back.
    RecipeBookSettingsChanged {
        /// The crafting-table book.
        crafting: RecipeBookTypeSettings,
        /// The furnace book.
        furnace: RecipeBookTypeSettings,
        /// The blast-furnace book.
        blast_furnace: RecipeBookTypeSettings,
        /// The smoker book.
        smoker: RecipeBookTypeSettings,
    },
    /// The world border's center moved, from
    /// the set border center packet.
    WorldBorderCenterChanged {
        /// New center X coordinate.
        x: f64,
        /// New center Z coordinate.
        z: f64,
    },
    /// The world border began (or continued) smoothly resizing, from
    /// the set border lerp size packet.
    WorldBorderSizeLerping {
        /// Size (diameter, in blocks) the border is resizing from.
        old_size: f64,
        /// Size (diameter, in blocks) the border is resizing to.
        new_size: f64,
        /// Duration of the resize, in milliseconds.
        lerp_time_ms: i64,
    },
    /// The world border's size changed instantly (no interpolation), from
    /// the set border size packet.
    WorldBorderSizeChanged {
        /// New size (diameter, in blocks).
        size: f64,
    },
    /// The world border's warning delay changed, from
    /// the set border warning delay packet.
    WorldBorderWarningDelayChanged {
        /// New warning delay, in seconds, before the border starts closing in.
        warning_time: i32,
    },
    /// The world border's warning distance changed, from
    /// the set border warning distance packet.
    WorldBorderWarningDistanceChanged {
        /// New distance, in blocks, at which the warning effect appears.
        warning_blocks: i32,
    },
    /// The world border was fully (re)initialized, from
    /// the initialize border packet — sent on join/respawn instead of
    /// the incremental variants above.
    WorldBorderInitialized {
        /// New center X coordinate.
        x: f64,
        /// New center Z coordinate.
        z: f64,
        /// Size (diameter, in blocks) the border is resizing from.
        old_size: f64,
        /// Size (diameter, in blocks) the border is resizing to.
        new_size: f64,
        /// Duration of the resize, in milliseconds.
        lerp_time_ms: i64,
        /// Absolute maximum size the border can ever reach.
        absolute_max_size: i32,
        /// Distance, in blocks, at which the warning effect appears.
        warning_blocks: i32,
        /// Warning delay, in seconds, before the border starts closing in.
        warning_time: i32,
    },
    /// Combat tracking began for the local player, from
    /// the player combat enter packet (no payload).
    PlayerCombatEntered,
    /// Combat tracking ended for the local player, from
    /// the player combat end packet.
    PlayerCombatEnded {
        /// Duration of the combat encounter, in ticks.
        duration_ticks: i32,
    },
    /// The server opened a sign-editing UI, from
    /// the open sign editor packet.
    SignEditorOpened {
        /// Block position of the sign.
        pos: BlockPos,
        /// Whether the front (vs. back) text is being edited.
        is_front_text: bool,
    },
    /// The advancements screen should switch to a given tab, from
    /// the select advancements tab packet.
    AdvancementsTabSelected {
        /// Tab identifier, or `None` to close/deselect the tab.
        tab: Option<Identifier>,
    },
    /// A direction-accelerating projectile's power changed after a deflection,
    /// from the projectile power packet.
    ProjectilePowerChanged {
        /// Projectile entity id.
        entity_id: i32,
        /// New acceleration power.
        acceleration_power: f64,
    },
    /// A ridden entity's (e.g. horse, llama) inventory screen was opened,
    /// from the mount screen open packet.
    MountScreenOpened {
        /// Window/container id.
        container_id: i32,
        /// Number of inventory columns (varies by the ridden entity's
        /// carrying capacity).
        inventory_columns: i32,
        /// Ridden entity id.
        entity_id: i32,
    },
    /// The server's game rule values, from
    /// the game rule values packet.
    GameRulesChanged {
        /// Game rule identifier and its raw string value, in wire order.
        values: Vec<(Identifier, String)>,
    },
    /// The server asked the client to reconnect to a different address, from
    /// the transfer packet.
    TransferRequested {
        /// Target server host.
        host: String,
        /// Target server port.
        port: i32,
    },
    /// The server requested a previously stored cookie, from
    /// the cookie request packet.
    CookieRequested {
        /// Cookie key.
        key: Identifier,
    },
    /// The server asked the client to persist an opaque cookie, from
    /// the store cookie packet.
    CookieStored {
        /// Cookie key.
        key: Identifier,
        /// Opaque payload (at most 5120 bytes).
        payload: Vec<u8>,
    },
    /// The server offered a resource pack, from
    /// the resource pack push packet.
    ResourcePackPushed {
        /// Pack id, echoed back in the client's accept/decline response.
        id: Uuid,
        /// Download URL.
        url: String,
        /// SHA-1 hash of the pack (hex; may be empty if not provided).
        hash: String,
        /// Whether declining or failing to download disconnects the client.
        required: bool,
        /// Optional prompt message shown to the user.
        prompt: Option<Text>,
    },
    /// The server withdrew a previously pushed resource pack, from
    /// the resource pack pop packet.
    ResourcePackPopped {
        /// Pack id to remove, or `None` to remove all packs.
        id: Option<Uuid>,
    },
    /// A plugin (custom payload) message arrived, from
    /// the custom payload packet.
    ///
    /// `data` is the raw payload bytes for `channel`, undecoded: only
    /// `minecraft:brand` is specially typed by vanilla (as a single UTF-8
    /// string) and every other channel is opaque plugin data, so this
    /// adapter carries the bytes as-is rather than guessing a shape.
    CustomPayload {
        /// Channel identifier.
        channel: Identifier,
        /// Raw payload bytes.
        data: Vec<u8>,
    },
    /// Public server metadata pushed proactively during play, from
    /// the server data packet.
    ServerDataReceived {
        /// Message of the day.
        motd: Text,
        /// Favicon PNG bytes, if the server sent one.
        icon: Option<Vec<u8>>,
    },
    /// A play-state pong echo, from the pong response packet (distinct
    /// from the keep-alive-like `Ping`/`ClientAction::PongResponse` pair).
    PongReceived {
        /// Echoed time value.
        time: i64,
    },
    /// A previously sent chat message was deleted/withdrawn, from
    /// the delete chat packet.
    ChatMessageDeleted {
        /// The message's signature; the adapter resolves wire-level cache
        /// references to the full 256 bytes before emitting, so this is
        /// normally [`PackedMessageSignature::Full`].
        signature: PackedMessageSignature,
    },
    /// The local player should look toward a fixed point or another entity,
    /// from the player look at packet.
    PlayerLookAt {
        /// Anchor point on the local player to rotate from.
        from_anchor: LookAnchor,
        /// Target position (already resolved by the server for the entity
        /// case).
        target: Vec3,
        /// If set, the target was an entity at send time; carries its id and
        /// the anchor point used on it.
        at_entity: Option<PlayerLookAtEntity>,
    },
    /// The local player changed dimension (portal travel) or respawned after
    /// death, from the respawn packet.
    Respawned {
        /// New dimension.
        dimension: DimensionId,
        /// New game mode.
        game_mode: GameMode,
        /// Game mode before this respawn, if the server reported one.
        previous_game_mode: Option<GameMode>,
        /// Last death location, if the server tracks one for this dimension.
        last_death_location: Option<DeathLocation>,
    },
    /// The dimension **type** the local player is in changed, resolved against
    /// the Configuration `registry_data`.
    ///
    /// Emitted alongside [`Self::Login`] and [`Self::Respawned`] — the two
    /// packets that carry a dimension-type holder id — and always *before* them,
    /// so a consumer folding both sees the geometry before the level name that
    /// depends on it.
    ///
    /// # Why `dimension_type` is an `Option`
    ///
    /// It is `None` when the id could not be resolved: no `registry_data` was
    /// received (an older server, or a protocol family that does not send it),
    /// or the entry's contents were elided or malformed. That is deliberately
    /// **not** the same as "the overworld" — a consumer must fall back
    /// explicitly rather than inherit a plausible default, which is a shape
    /// that has been gotten wrong before. `holder_id` is always present, so a consumer can log
    /// exactly which id failed to resolve.
    DimensionTypeChanged {
        /// The `minecraft:dimension_type` holder id the server sent.
        holder_id: i32,
        /// The resolved dimension type, or `None` — see above.
        dimension_type: Option<DimensionTypeInfo>,
        /// Whether the level uses the **flat** world generator — the login and
        /// respawn packets' own `is_flat` boolean.
        ///
        /// # Its provenance is not the other two fields'
        ///
        /// `holder_id` and `dimension_type` come from the registry; this comes
        /// straight off the packet, and there is nothing in the
        /// `minecraft:dimension_type` registry that could supply it. It rides
        /// this event only because this event is emitted from exactly the two
        /// packets that carry it, and because every consumer that wants one
        /// wants the other: vanilla keeps both in its own client-level-data side by
        /// side, where its own void-darkness-onset-range query reads its own
        /// "is flat" flag and
        /// its own min-Y query reads the dimension type.
        ///
        /// It is deliberately **not** a field of [`DimensionTypeInfo`], which
        /// is a decode of one registry entry and must stay so — a struct with
        /// two sources is how a field ends up populated on one path and
        /// defaulted on another.
        ///
        /// `false` when the sending family has no such field (only `v26-2`
        /// emits this event today), which is also the non-flat answer, so a
        /// legacy session behaves exactly as it did.
        is_flat: bool,
    },
    /// The per-biome visual attributes the server declared in the Configuration
    /// `registry_data`, **indexed by biome holder id**.
    ///
    /// Emitted alongside [`Self::Login`], for the same reason and in the same
    /// position as [`Self::DimensionTypeChanged`]: re-entering Configuration
    /// resends the whole registry set and is always followed by a fresh `Login`,
    /// so `Login` is the one point at which the registries are known to be
    /// complete and current.
    ///
    /// # Why this carries colours and not names
    ///
    /// The biome registry is a **data-pack** registry: a pack can reorder it,
    /// rename an entry, or change a colour, so nothing about the mapping can be
    /// hardcoded and every hop has to be resolved off what the server sent. The
    /// obvious shape — ship the ordered names, look the colour up in a table
    /// derived from our jar — is wrong on all three counts *and* needs a table
    /// re-derived every version. Shipping the value at the holder id needs no
    /// table at all, and the consumer indexes it with exactly the integer a
    /// chunk section's biome palette already stores.
    BiomeVisuals {
        /// Each biome's `minecraft:visual/sky_color`, packed `0x00RR_GGBB` in
        /// **sRGB bytes**, at its holder id.
        ///
        /// `None` where the biome declares none — 10 of 26.2's 66, exactly the
        /// Nether and End biomes, whose dimensions draw no sky disc — or where
        /// the entry could not be parsed. A `None` still occupies its index: the
        /// position *is* the holder id, so dropping one would shift every later
        /// biome's colour by a slot.
        sky_colors: Vec<Option<u32>>,
    },
    /// The per-biome **climate** the server declared in the same Configuration
    /// `registry_data` [`Self::BiomeVisuals`] reads, **indexed by biome holder id** exactly as
    /// [`Self::BiomeVisuals::sky_colors`] is.
    ///
    /// A *separate* variant rather than two more fields on [`Self::BiomeVisuals`]
    /// on purpose: [`Self::BiomeVisuals`] already has a non-`ecs` consumer that
    /// destructures it by name with no `..`, so adding fields there is a
    /// breaking change to a file this session could not touch to fix in the
    /// same commit. Emitted at the same point as [`Self::BiomeVisuals`] (see its
    /// doc for why `Login` is the right moment), so the two always agree on
    /// which registry generation they describe.
    BiomeClimates {
        /// Each biome's declared (not height-adjusted) `temperature`, at its
        /// holder id. `None` where the entry could not be parsed — every real
        /// 26.2 biome declares one, so unlike `sky_colors` a `None` here should
        /// only ever mean "malformed or elided", never "this biome has none".
        temperatures: Vec<Option<f32>>,
        /// Each biome's `downfall`, at its holder id. Feeds the grass/foliage
        /// colormap sample alongside `temperature`; not itself part of the
        /// rain/snow decision.
        downfall: Vec<Option<f32>>,
        /// Each biome's `has_precipitation`, at its holder id. `false` means
        /// the biome never rains or snows regardless of temperature (deserts,
        /// most Nether/End biomes).
        has_precipitation: Vec<Option<bool>>,
    },
    /// The ordered entry names of the `minecraft:worldgen/biome` registry the
    /// server declared in the Configuration `registry_data`,
    /// **indexed by holder id** exactly as
    /// [`Self::BiomeVisuals::sky_colors`] and [`Self::BiomeClimates`] are.
    ///
    /// Emitted at the same point (`Login`) and for the same reason as those
    /// two — see [`Self::BiomeVisuals`]'s doc — so all three always describe
    /// the same registry generation.
    ///
    /// # Why this exists as a third variant rather than a name column on the other two
    ///
    /// [`Self::BiomeVisuals`] and [`Self::BiomeClimates`] carry colour/climate
    /// *values*; this carries the *identity* of the biome at each holder id,
    /// which is a different consumer (id → name resolution for a tint lookup,
    /// not a value the mesher shades with directly) and a different lifetime
    /// concern — see the `shell`-side [`Route`] arm and
    /// `crates/lodestone-shell/src/mesher.rs`'s `biome_name_at`.
    ///
    /// # Why this closes a real (not hypothetical) correctness gap
    ///
    /// Before this variant, `crates/lodestone-shell/src/mesher.rs` resolved a
    /// chunk section's biome holder id to a name through a hardcoded,
    /// alphabetical `FALLBACK_BIOME_NAMES` table — correct only against this
    /// project's own server, which derives the same alphabetical order. A
    /// real vanilla server (or one with a data pack that reorders, adds, or
    /// removes a biome) sends its **own** registry order, and nothing told
    /// the mesher what that order was: `ClientRegistries::entry_names` (the
    /// v26-2 adapter's already-correct decode of it) never left the version
    /// crate. Joining a third-party server could therefore paint the wrong
    /// grass/foliage/water colour with no error anywhere — the id was valid,
    /// just resolved through the wrong table.
    BiomeRegistryNames {
        /// Each biome's registry entry name (e.g. `minecraft:swamp`), at its
        /// holder id. Every synchronized registry entry carries a name (it is
        /// the wire key, never optional), unlike [`Self::BiomeVisuals::sky_colors`]
        /// or [`Self::BiomeClimates`]'s per-field `Option`s.
        names: Vec<String>,
    },
    /// The server's `minecraft:enchantment` registry order, emitted at `Login`
    /// alongside [`Self::BiomeRegistryNames`].
    ///
    /// # The latent bug this exists to remove
    ///
    /// Exactly [`Self::BiomeRegistryNames`]'s story, one registry over. The table
    /// was **already decoded** — `ClientRegistries::entry_names` has had it all
    /// along — and was simply never handed past the version crate, so
    /// `Sim::riptide_level` resolved `minecraft:riptide` through a **hardcoded
    /// holder id of 32**, derived from `riptide` being the 33rd of 26.2's 43
    /// built-in enchantments in resource-location-sorted order.
    ///
    /// That id is correct against a vanilla 26.2 server and wrong against any data
    /// pack that adds, removes or reorders an enchantment sorting before
    /// `riptide` — and it is wrong *silently*, because the id is still valid and
    /// still resolves to *an* enchantment. Same failure shape as the mesher's
    /// `FALLBACK_BIOME_NAMES`: the wrong table, not a missing one.
    ///
    /// A second consumer is already waiting on it — the enchantment-level gap in
    /// `crates/lodestone-shell/src/entities.rs`.
    EnchantmentRegistryNames {
        /// Each enchantment's registry entry name (e.g. `minecraft:riptide`), at
        /// its holder id. Empty when the server sent no `minecraft:enchantment`
        /// registry, which a consumer must treat as "fall back", not as "no
        /// enchantments exist".
        names: Vec<String>,
    },
    /// A win condition was signalled by the server: the game-event packet's
    /// `WIN_GAME` event (code `4`), sent when the local player exits the End
    /// through the exit portal after defeating the ender dragon.
    ///
    /// Carries no data: the real handler ignores the packet's `param` for
    /// this event and always opens the credits screen with the "show the poem"
    /// flag set `true`, so
    /// there is nothing version-free left to carry — this is a pure signal,
    /// like [`Self::Respawned`] is for a plain "you are alive again" with no
    /// win-specific payload.
    WinGame,
    /// The server's whole Brigadier command tree (`minecraft:commands`,
    /// clientbound id 16), sent once after login (and again if the tree
    /// changes, e.g. a permission level change). Boxed for the same reason
    /// `TitleText`/`UpdateName` box a [`Text`]: a full tree is the largest
    /// payload any `ClientEvent` carries, and every other variant pays that
    /// size in the enum's own stack footprint if it isn't indirected.
    ///
    /// Same shape as [`Self::BiomeRegistryNames`]: a registry-generation
    /// table with a single, obvious consumer (the chat box's tab completion
    /// and syntax highlighting) and no per-entity or per-session scalar to
    /// fold — see [`route`]'s `SHELL` arm for both.
    CommandTreeUpdated {
        /// The decoded tree.
        tree: Box<CommandTree>,
    },
    /// A reply to a serverbound `command_suggestion` request
    /// (`minecraft:command_suggestions`, clientbound id 15):
    /// `ClientboundCommandSuggestionsPacket(int id, int start, int length,
    /// List<Entry>)`. The transaction id lets the chat box discard a stale
    /// reply to a request it has since superseded, matching vanilla's own
    /// `ClientSuggestionProvider::completeCustomSuggestions` id check.
    CommandSuggestionsReceived {
        /// Transaction id, echoing the request's.
        id: i32,
        /// Start of the input-text byte range these suggestions replace.
        start: i32,
        /// Length of that byte range.
        length: i32,
        /// The suggested replacement strings.
        suggestions: Vec<CommandSuggestionEntry>,
    },
    /// A filled map's contents changed, from the map item data packet.
    ///
    /// Keyed on the **map id**, not on an entity: one map item can be held by
    /// several players and hung in several item frames at once, so this is
    /// session-scoped map state rather than per-entity state.
    ///
    /// Both payload halves are genuinely optional and independently so
    /// (`Optional<List<MapDecoration>>` and `Optional<MapPatch>`): a decoration-only
    /// update carries no pixels, and a pixel-only update carries no icons. `None`
    /// means "unchanged", never "empty" — clearing the decorations is
    /// `Some(vec![])`.
    MapItemData {
        /// The map's id (`MapId`), which is what the `minecraft:map_id` item
        /// component on a filled-map stack points at.
        map_id: i32,
        /// Zoom level, 0 (1 pixel per block) to 4 (16 blocks per pixel).
        scale: i8,
        /// Whether the map has been locked with a cartography table.
        locked: bool,
        /// Icons to draw over the map, replacing the previous set, or `None` if
        /// this update does not touch them.
        decorations: Option<Vec<MapDecoration>>,
        /// The changed sub-rectangle of the 128×128 colour grid, or `None`.
        color_patch: Option<MapPatch>,
    },
    /// The advancement tree and/or the local player's progress on it changed,
    /// from the update advancements packet.
    AdvancementsUpdated {
        /// `true` on the server's first packet: discard all known advancements
        /// and progress and treat `added` as the whole tree.
        reset: bool,
        /// Advancements added or redefined.
        added: Vec<AdvancementEntry>,
        /// Advancement ids that are no longer visible.
        removed: Vec<Identifier>,
        /// Per-advancement criterion progress, as
        /// `(advancement id, [(criterion, obtained epoch-millis)])`. A criterion
        /// present with `None` is known but not obtained.
        progress: Vec<(Identifier, Vec<(String, Option<i64>)>)>,
        /// Vanilla's own "show advancements" flag — whether completions announce.
        show_advancements: bool,
    },

    // ---- the remaining clientbound packets ----------------------------------
    //
    // Every variant below routes to `session`, which is deliberate and is the
    // fork `route`'s doc says has cost work twice. None of them is per-entity
    // state (`DebugEntityValue` is *about* an entity but is a debug feed keyed by
    // subscription, not a component hanging off one) and none is block/world
    // state travelling the shell's own stream — they are session-scoped tables,
    // the same shape as the scoreboard and the tab list. That also means none of
    // them needs an arm in `lodestone_shell::net::forward`, so this whole block
    // lands without a shell edit; a screen reads the session component when
    // someone builds one.
    /// The server awarded or resynchronised statistics
    /// (the award stats packet).
    ///
    /// Sent in full on request (vanilla's `/stats`-equivalent screen opening) and
    /// incrementally as counters move, so a fold must **overwrite per key**
    /// rather than accumulate: the wire value is the absolute count.
    StatisticsAwarded {
        /// One entry per statistic the server reported.
        stats: Vec<StatAward>,
    },
    /// The server changed the extra names offered in chat tab-completion
    /// (the custom chat completions packet).
    ChatCompletionsChanged {
        /// Whether to add to, remove from, or replace the current set.
        action: ChatCompletionsAction,
        /// The names this update concerns.
        entries: Vec<String>,
    },
    /// A per-block debug feed value (the debug block value packet).
    ///
    /// The server sends nothing on any debug feed until the client asks with
    /// [`crate::ClientAction::SubscribeDebug`] — this and its three siblings are
    /// the *response* half of that request, which is why neither half is useful
    /// alone.
    DebugBlockValue {
        /// The block this value is about.
        pos: BlockPos,
        /// The `minecraft:debug_subscription` this value belongs to.
        subscription: Identifier,
        /// The feed's own payload bytes, or `None` when the server is clearing
        /// this key. **Opaque**: the value codec is per-subscription and the
        /// seventeen registered ones share no shape (one has a `null` codec), so
        /// modelling them here would be seventeen decoders for a debug overlay.
        value: Option<Vec<u8>>,
    },
    /// A per-chunk debug feed value (the debug chunk value packet).
    DebugChunkValue {
        /// The chunk this value is about.
        chunk: ChunkPos,
        /// The `minecraft:debug_subscription` this value belongs to.
        subscription: Identifier,
        /// Opaque per-subscription payload; see [`Self::DebugBlockValue::value`].
        value: Option<Vec<u8>>,
    },
    /// A per-entity debug feed value (the debug entity value packet).
    ///
    /// Routed to `session`, not `ingest`, even though it names an entity: it is a
    /// debug overlay keyed by subscription with no lifetime tied to the entity's
    /// ECS row, and folding it as a component would resurrect entities the client
    /// has already forgotten.
    DebugEntityValue {
        /// Network id of the entity this value is about.
        entity_id: i32,
        /// The `minecraft:debug_subscription` this value belongs to.
        subscription: Identifier,
        /// Opaque per-subscription payload; see [`Self::DebugBlockValue::value`].
        value: Option<Vec<u8>>,
    },
    /// A one-shot debug feed event (the debug event packet).
    ///
    /// Unlike the three `*Value` packets this carries the payload **without** an
    /// optional wrapper — an event is always present — so there is no "clear this
    /// key" form.
    DebugEvent {
        /// The `minecraft:debug_subscription` this event belongs to.
        subscription: Identifier,
        /// Opaque per-subscription payload; see [`Self::DebugBlockValue::value`].
        value: Vec<u8>,
    },
    /// A batch of server performance samples (the debug sample packet).
    DebugSample {
        /// The samples, in nanoseconds for the tick-time kind.
        sample: Vec<i64>,
        /// Which sample series this batch belongs to.
        kind: DebugSampleKind,
    },
    /// The server asked the client to highlight a game-test position
    /// (the game test highlight pos packet).
    GameTestHighlightPos {
        /// Absolute world position.
        absolute: BlockPos,
        /// Position relative to the test's own origin.
        relative: BlockPos,
    },
    /// The server is running low on disk space
    /// (the low disk space warning packet).
    ///
    /// A zero-byte packet — `StreamCodec.unit` — so this variant carries nothing,
    /// like [`Self::WinGame`].
    LowDiskSpaceWarning,
    /// The server sent crash/report metadata for the client to attach to a report
    /// (the custom report details packet).
    CustomReportDetails {
        /// `(title, description)` pairs, at most 32 entries.
        details: Vec<(String, String)>,
    },
    /// The server advertised its links (the server links packet).
    ///
    /// Vanilla shows these on the pause and disconnect screens. Every entry is
    /// **untrusted** — the label may be an arbitrary server-authored component
    /// and the URL an arbitrary string — which is why nothing here resolves or
    /// validates either.
    ServerLinksReceived {
        /// The advertised links, in the order sent.
        links: Vec<ServerLink>,
    },
    /// A tracked waypoint was added, updated or removed
    /// (the tracked waypoint packet).
    WaypointUpdated {
        /// Whether this is a track, untrack or update.
        operation: WaypointOperation,
        /// The waypoint.
        waypoint: TrackedWaypoint,
    },
    /// A reply to a serverbound NBT query (the tag query packet).
    ///
    /// The transaction id echoes
    /// [`crate::ClientAction::QueryEntityTag`]/[`crate::ClientAction::QueryBlockEntityTag`],
    /// so a consumer can match a reply to its own request and drop a stale one.
    /// `tag` is `None` when the server had nothing (or refused): the wire carries
    /// a nullable compound, not an error.
    TagQueryResponse {
        /// Transaction id echoed from the request.
        transaction_id: i32,
        /// The queried NBT as raw network-NBT bytes, or `None`.
        tag: Option<Vec<u8>>,
    },
    /// The world's tick rate or freeze state changed
    /// (the ticking state packet) — vanilla's `/tick rate` and
    /// `/tick freeze`.
    TickingStateChanged {
        /// Ticks per second the server is targeting.
        tick_rate: f32,
        /// Whether the world is frozen.
        frozen: bool,
    },
    /// The server is stepping a frozen world forward
    /// (the ticking step packet) — vanilla's `/tick step`.
    TickingStepped {
        /// How many ticks remain to run while frozen.
        tick_steps: i32,
    },
    /// A test instance block reported its status
    /// (the test-instance-block-status packet).
    TestInstanceBlockStatus {
        /// Human-readable status line.
        status: Text,
        /// Detected region size, when the server has one.
        size: Option<(i32, i32, i32)>,
    },
    /// The server asked the client to open a dialog
    /// (the show dialog packet).
    ///
    /// The wire is a `Holder<Dialog>`: either a registry id, or an inline dialog
    /// as a network-NBT blob. `Dialog` is an NBT `Codec` union of six types with
    /// nested body/input/action trees — a *schema*, not a `StreamCodec` — so the
    /// inline form is carried here as raw NBT bytes. A screen that renders
    /// dialogs parses them; nothing before that point needs to.
    DialogShown {
        /// The registry id of a known dialog, when the server referenced one.
        registry_id: Option<i32>,
        /// The inline dialog as raw network-NBT bytes, when the server sent one.
        /// Exactly one of this and `registry_id` is `Some`.
        inline: Option<Vec<u8>>,
    },
    /// The server closed any open dialog (the clear dialog packet).
    ///
    /// Another zero-byte `StreamCodec.unit` packet.
    DialogCleared,

    // ---- the recipe/trade tranche --------------------------------------------
    //
    // These five needed `SlotDisplay` — a *recursive* registry-dispatched union
    // of eleven variants with no length prefix anywhere — so none of them could
    // land before the walker existed and all five landed together.
    //
    // Each carries **result item ids** rather than a modelled display tree. A
    // recipe panel and a toast both key on the result; the ingredient slots are
    // walked only because they must be consumed to reach it. Modelling the whole
    // tree would be a second recipe representation next to
    // `lodestone_game::recipe`, which already has one.
    /// The server unlocked recipes (the recipe book add packet).
    ///
    /// `replace` is the server's first-sync flag: discard the known set and treat
    /// `entries` as the whole book. **It sits after the entry list on the wire**,
    /// which is why the list cannot be carried as opaque trailing bytes.
    RecipeBookAdded {
        /// The unlocked recipes.
        entries: Vec<RecipeBookEntry>,
        /// Whether this replaces the known set rather than adding to it.
        replace: bool,
    },
    /// The server un-learned recipes (the recipe book remove packet),
    /// e.g. after a datapack reload.
    RecipeBookRemoved {
        /// `RecipeDisplayId`s to forget.
        display_ids: Vec<i32>,
    },
    /// The server is showing a ghost recipe in an open crafting grid
    /// (the place ghost recipe packet) — the faded preview after clicking a
    /// recipe in the book.
    GhostRecipeShown {
        /// The container the ghost belongs to.
        window_id: i32,
        /// Item ids the ghost's result slot can display.
        result_items: Vec<i32>,
    },
    /// The server's recipe *property sets* changed
    /// (the update recipes packet).
    ///
    /// Not the recipe corpus: these are the "which items are valid in this slot"
    /// sets vanilla's screens use to grey out an input (fuel, smithing template,
    /// and so on), plus the stonecutter's own input→result list.
    RecipePropertySetsUpdated {
        /// `(property set key, valid item registry ids)`.
        item_sets: Vec<(Identifier, Vec<i32>)>,
        /// One entry per stonecutter recipe: `(input item registry ids, result
        /// item registry ids)`. The input is the ingredient a stonecutter's
        /// input slot must hold for this entry's results to be offered; without
        /// it a consumer cannot compute the subset of results reachable from
        /// whatever the slot currently holds.
        stonecutter_results: Vec<(Vec<i32>, Vec<i32>)>,
    },
    /// A villager or wandering trader opened its trade list
    /// (the merchant offers packet).
    MerchantOffersReceived {
        /// The trade container's window id.
        window_id: i32,
        /// The offers, in the order shown.
        offers: Vec<MerchantOffer>,
        /// The villager's level, 1–5.
        villager_level: i32,
        /// The villager's experience toward its next level.
        villager_xp: i32,
        /// Whether the level/xp bar should be shown.
        show_progress: bool,
        /// Whether this merchant restocks (false for a wandering trader).
        can_restock: bool,
    },
}

/// One unlocked recipe, from the recipe book add packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeBookEntry {
    /// The server's `RecipeDisplayId` — the handle
    /// [`ClientEvent::RecipeBookRemoved`] and
    /// [`crate::ClientAction::PlaceRecipe`] both use. **Not** a recipe
    /// `Identifier`: 26.x replaced the name with a per-session index.
    pub display_id: i32,
    /// Item ids the recipe's result slot can display. Usually one; a display can
    /// legitimately offer several (a `composite`, or a tag-driven slot).
    pub result_items: Vec<i32>,
    /// Item ids the display's trailing crafting-station/furnace slot-display
    /// can show — the small corner icon a recipe-unlock toast draws (a crafting
    /// table, furnace, etc.). Every `RecipeDisplay` variant carries this as its
    /// final `SlotDisplay`. Usually one entry; empty for a display whose station
    /// slot is itself `empty` or unresolved.
    pub station_items: Vec<i32>,
    /// The recipe-book group this entry shares a stacked button with, or `None`
    /// when the entry stands alone.
    ///
    /// A group is what makes the four wood-plank recipes collapse into one
    /// button that cycles. The wire encoding is an optional VarInt where `0`
    /// means absent and a present value `v` is written `v + 1`; the offset is
    /// already removed here, so `Some(0)` is group zero.
    pub group: Option<i32>,
    /// Which recipe-book tab the entry belongs to — the book category index,
    /// not the crafting-book *type*.
    pub category: i32,
    /// The ingredient sets a player must have already unlocked before this
    /// entry is shown, in wire order, or `None` when the entry states no
    /// requirement.
    ///
    /// This is the recipe book's own progressive-reveal gate, not the recipe's
    /// inputs: the display's inputs are what
    /// [`result_items`](Self::result_items) and the display walk cover. A
    /// [`RegistrySet::Tag`](crate::RegistrySet::Tag) arm names a tag whose
    /// membership is not on the wire.
    pub crafting_requirements: Option<Vec<crate::RegistrySet>>,
    /// Whether this unlock should raise a toast (`flags` bit 0).
    pub notification: bool,
    /// Whether its recipe-book tab should highlight (`flags` bit 1).
    pub highlight: bool,
}

/// One villager trade, from the merchant offers packet.
///
/// Note the arithmetic fields are **big-endian `i32`s on the wire, not VarInts** —
/// vanilla's own merchant-offer codec writes a fixed-width int for `uses`, its
/// own max-uses field, `xp`,
/// its own special-price-diff field, and `demand`, which is unusual enough in this protocol that
/// a VarInt-by-default encoder or decoder gets all five wrong at once.
#[derive(Debug, Clone, PartialEq)]
pub struct MerchantOffer {
    /// First input: `(item registry id, count)`.
    pub cost_a: (i32, i32),
    /// Optional second input.
    pub cost_b: Option<(i32, i32)>,
    /// What the trade produces.
    pub result: Option<ItemStack>,
    /// Whether the trade is currently exhausted.
    pub out_of_stock: bool,
    /// Times used since the last restock.
    pub uses: i32,
    /// Uses before it locks.
    pub max_uses: i32,
    /// Villager xp granted.
    pub xp: i32,
    /// Demand/reputation price adjustment, in items.
    pub special_price_diff: i32,
    /// Demand price multiplier.
    pub price_multiplier: f32,
    /// Accumulated demand.
    pub demand: i32,
}

/// One statistic the server reported, from the award-stats packet.
///
/// The wire carries two registry ids — a `stat_type` and a value id whose
/// registry *depends on that type*. The
/// adapter resolves both, and `value` is `None` when the value registry is one
/// this build has no table for. That is not an error: the count is still usable
/// and a screen keyed on `stat_type` alone (the game's "General" tab is entirely
/// `minecraft:custom`) does not need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatAward {
    /// The `minecraft:stat_type`, e.g. `minecraft:custom` or `minecraft:mined`.
    pub stat_type: Identifier,
    /// The statistic's value key, e.g. `minecraft:bell_ring` under
    /// `minecraft:custom` or `minecraft:stone` under `minecraft:mined`.
    pub value: Option<Identifier>,
    /// The absolute count, not a delta.
    pub count: i32,
}

/// What a `custom_chat_completions` update does to the current set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatCompletionsAction {
    /// Add these entries.
    Add,
    /// Remove these entries.
    Remove,
    /// Replace the whole set with these entries.
    Set,
}

/// Which server sample series a `debug_sample` batch belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DebugSampleKind {
    /// Tick-time sampling — the only kind 26.2 defines.
    TickTime,
}

/// One entry of the server-links packet.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerLink {
    /// What kind of link this is.
    pub kind: ServerLinkKind,
    /// The URL, exactly as the server sent it. **Not validated** — see
    /// [`ClientEvent::ServerLinksReceived`].
    pub url: String,
}

/// A server link's label: one of vanilla's known kinds, or a custom component.
///
/// The wire is `ByteBufCodecs.either`, a boolean where `true` means *Left* — and
/// Left is the **known** id, not the custom label. Getting that polarity
/// backwards produces a plausible-looking decode of the wrong half.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerLinkKind {
    /// One of vanilla's ten `KnownLinkType`s, by id.
    Known(i32),
    /// A server-authored label.
    Custom(Text),
}

/// Whether a `waypoint` packet starts tracking, stops tracking, or updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaypointOperation {
    /// Start tracking.
    Track,
    /// Stop tracking.
    Untrack,
    /// Update an already-tracked waypoint.
    Update,
}

/// One tracked waypoint, from the tracked waypoint packet.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackedWaypoint {
    /// The waypoint's identity: a player's UUID, or a free-form string for a
    /// non-entity waypoint. The wire is a boolean discriminant, `true` for UUID.
    pub id: WaypointId,
    /// The icon style, a `minecraft:waypoint_style` key.
    pub style: Identifier,
    /// Packed RGB tint, when the server overrode the style's own colour.
    pub color: Option<u32>,
    /// Where the waypoint is, at whatever precision the server chose to send.
    pub position: WaypointPosition,
}

/// A waypoint's identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WaypointId {
    /// An entity's UUID — vanilla's own locator-bar waypoints.
    Entity(Uuid),
    /// A free-form name.
    Named(String),
}

/// How precisely a waypoint's position is known.
///
/// Vanilla degrades deliberately with distance: a nearby waypoint sends exact
/// coordinates, a distant one only its chunk, and one past the tracking range
/// only a compass bearing. A consumer must render all four — treating
/// [`Self::Empty`] or [`Self::Azimuth`] as "no position" would make the locator
/// bar go blank exactly when it is most useful.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WaypointPosition {
    /// No position at all.
    Empty,
    /// Exact block position.
    Exact(BlockPos),
    /// Chunk position only.
    Chunk(ChunkPos),
    /// Compass bearing in radians only.
    Azimuth(f32),
}

/// One icon drawn over a filled map, from vanilla's `MapDecoration`.
#[derive(Debug, Clone, PartialEq)]
pub struct MapDecoration {
    /// The `minecraft:map_decoration_type` registry key (e.g.
    /// `minecraft:player`, `minecraft:banner_red`), resolved from the wire's
    /// numeric id.
    pub kind: Identifier,
    /// Position across the map, as vanilla's signed byte in the ±127 space that
    /// spans the whole 128-pixel width (so 2 wire units ≈ 1 pixel).
    pub x: i8,
    /// Position down the map, same space as [`Self::x`].
    pub y: i8,
    /// Facing, 0–15 in sixteenths of a turn. Vanilla masks the wire byte with
    /// `& 15`, so this is always in range.
    pub rotation: u8,
    /// Custom label (a named banner), if any.
    pub name: Option<Text>,
}

/// A rectangular sub-region of a map's 128×128 colour grid, from vanilla's
/// `MapItemSavedData.MapPatch`.
///
/// **This is a sub-rectangle, not the whole frame.** Vanilla only ever sends the
/// dirty columns, so a moving player produces a tall 1-or-2-column-wide patch,
/// and treating `colors` as a full 16 384-byte image reads garbage. Index it as
/// `colors[x + y * width]` and offset by `start_x`/`start_y`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapPatch {
    /// Left edge of the patch within the 128-wide grid.
    pub start_x: u8,
    /// Top edge of the patch within the 128-tall grid.
    pub start_y: u8,
    /// Patch width in pixels, always ≥ 1 (a zero width is how the wire spells
    /// "no patch", which decodes to `None` instead).
    pub width: u8,
    /// Patch height in pixels.
    pub height: u8,
    /// `width * height` map-palette colour indices, row-major.
    pub colors: Vec<u8>,
}

/// Which frame vanilla draws around an advancement's icon — the wire ordinal
/// order of `AdvancementType`.
///
/// **The ordinals are `TASK`, `CHALLENGE`, `GOAL`**, which is not the order the
/// three are usually listed in; reading it as task/goal/challenge swaps the two
/// rarest frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdvancementFrame {
    /// Ordinal 0 — the plain square frame.
    Task,
    /// Ordinal 1 — the spiked frame.
    Challenge,
    /// Ordinal 2 — the rounded frame.
    Goal,
}

impl AdvancementFrame {
    /// From the wire ordinal (`FriendlyByteBuf::readEnum`, a VarInt).
    #[must_use]
    pub const fn from_ordinal(ordinal: i32) -> Option<Self> {
        Some(match ordinal {
            0 => Self::Task,
            1 => Self::Challenge,
            2 => Self::Goal,
            _ => return None,
        })
    }
}

/// The presentation half of an advancement, from vanilla's `DisplayInfo`.
///
/// # `x`/`y` exist only here
///
/// 26.2's advancement JSON on disk carries no position — vanilla computes the
/// tidy-tree layout server-side in `TreeNodePosition` and writes the result to
/// the wire. So these two floats are the *only* source of vanilla's own layout,
/// which is what makes this decode load-bearing rather than cosmetic.
///
/// # Field order is not the datapack's
///
/// Vanilla's own display-info network serializer writes title, description,
/// icon, frame, an
/// `int` flag word, the optional background, then x and y. Its own
/// "announce chat" field is
/// **not on the wire at all** (vanilla's reader hardcodes `false`), and the flag
/// word is a raw big-endian `int`, not a byte.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvancementDisplay {
    /// Title component.
    pub title: Text,
    /// Description component.
    pub description: Text,
    /// The icon stack (`ItemStackTemplate`: item, count, components).
    pub icon: ItemStack,
    /// Frame shape.
    pub frame: AdvancementFrame,
    /// Tab background texture, present on root advancements only.
    pub background: Option<Identifier>,
    /// Whether completing it pops a toast.
    pub show_toast: bool,
    /// Whether it is hidden until obtained.
    pub hidden: bool,
    /// Server-computed tree column, in advancement-grid units.
    pub x: f32,
    /// Server-computed tree row, in advancement-grid units.
    pub y: f32,
}

/// One node of the advancement tree, from vanilla's `AdvancementHolder`.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvancementEntry {
    /// The advancement id, e.g. `minecraft:story/mine_stone`.
    pub id: Identifier,
    /// Parent id; `None` makes this a root (a tab).
    pub parent: Option<Identifier>,
    /// Presentation, absent for an advancement vanilla does not draw (recipe
    /// unlocks). A node without display is hidden by vanilla's own screen.
    pub display: Option<AdvancementDisplay>,
    /// AND-of-ORs completion shape: done when every group has one obtained
    /// criterion.
    pub requirements: Vec<Vec<String>>,
    /// Vanilla's own "sends telemetry event" bit, carried because it is on the wire.
    pub sends_telemetry_event: bool,
}

/// Which of the client's event routers claim a [`ClientEvent`].
///
/// # Why this lives in `lodestone-model` and not next to the routers
///
/// [`ClientEvent`] is `#[non_exhaustive]`, which means **no downstream crate can
/// write an exhaustive match over it** — every consumer is *forced* to end in a
/// `_ =>` arm, and a terminal wildcard is indistinguishable from a decision. That
/// attribute is exactly why a new variant used to compile with zero routing arms
/// anywhere and reach nothing. Inside the defining crate the attribute does not
/// bind, so [`route`] can be exhaustive here while the attribute keeps protecting
/// external plugin code. The layering cost is real and accepted: the leaf model
/// crate names its consumers. It buys the one property nothing else can — a
/// **compile error** when a variant is added and not routed.
///
/// # Why booleans and not an enum
///
/// The claims are **not exclusive**, so an enum would force a false choice and
/// the table would begin by losing information:
///
/// * [`ClientEvent::Login`] is folded by `lodestone_ecs::ingest` (the entity id
///   and the `EntityIndex` entry), *and* by `lodestone_ecs::session` (the session
///   scalars), *and* forwarded to the shell as `NetUpdate::LoggedIn`.
/// * [`ClientEvent::EntityPassengersChanged`] is `ingest` (the
///   `Passengers`/`Vehicle` component pair) *and* `session` (the local player's
///   own `Riding` scalar).
///
/// Three disjoint writes off one event is normal here. A double *fold* of the
/// same state is the thing to avoid, and no boolean can tell you that — only
/// reading the two systems can.
///
/// # What each flag is worth
///
/// | flag | enforced by |
/// |---|---|
/// | [`ingest`](Route::ingest) | `lodestone_ecs::ingest::handles_event` *is* this flag — a live derivation |
/// | [`session`](Route::session) | `lodestone_ecs::session::handles_event` *is* this flag — a live derivation |
/// | [`shell`](Route::shell) | a `debug_assert!` on the catch-all of `lodestone_shell::net::forward` |
/// | [`shell_conditional`](Route::shell_conditional) | nothing; it exists only to keep that assert correct |
/// | [`client`](Route::client) | nothing; documentation, so that [`Route::NOWHERE`] means what it says |
/// One recipe book's stored UI state, from `RecipeBookSettings.TypeSettings`.
///
/// Both fields default to `false`, which is vanilla's own default for a book no
/// server has reported: closed, unfiltered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecipeBookTypeSettings {
    /// Whether this book is open.
    pub open: bool,
    /// Whether this book's "only show craftable" filter is active.
    pub filtering: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Route {
    /// `lodestone_ecs::ingest` folds it: **per-entity ECS state** — components
    /// hanging off an entity in the client-owned world.
    pub ingest: bool,
    /// `lodestone_ecs::session` folds it: **local-player and session scalars** —
    /// vitals, xp, abilities, game mode, menus, scoreboard, tab list, boss bars.
    pub session: bool,
    /// `lodestone_shell`'s `net::forward` has an arm for it: **block and world
    /// state**, plus anything the renderer, HUD or audio reads off the shell's own
    /// `NetUpdate` stream. Such events need no `handles_event` arm at all.
    pub shell: bool,
    /// The shell's arm is **conditional or intercepted** — a match guard or a
    /// literal field pattern in `forward`, or a pre-forward shell consumer — so
    /// the `debug_assert!` on `forward`'s catch-all must not demand an
    /// unconditional forwarding arm.
    ///
    /// Three variants currently use this escape hatch, and all are a property
    /// of `net.rs` as it stands rather than of the event: `LevelEvent` (only
    /// sub-event `2001` is consumed), `EntitySpawned` (only `lightning_bolt`, to
    /// count flashes), and `ResourcePackPopped` (the connection loop clears the
    /// live pack before `forward` sees the event). If a guarded arm becomes
    /// unconditional, or the pre-forward consumer moves into `forward`, clear
    /// or adjust this and the assert gets stricter for free.
    pub shell_conditional: bool,
    /// Consumed inside `lodestone-client` itself by something that is **not** one
    /// of the three routers, so [`Route::NOWHERE`] can mean "nothing anywhere"
    /// rather than "nothing I happened to check". Exactly four such places:
    ///
    /// * `Driver::emit`'s auto-response switch (keep-alive, chat acknowledgement,
    ///   `player_loaded`, auto-respawn, cookie response/store, transfer outcome) —
    ///   a protocol reply or session result, not screen state.
    /// * `LocalEcho::apply`, which is down to `TeleportPlayer` alone.
    /// * `SharedState::apply`'s own `TimeChanged` arm, which writes `WorldTime`
    ///   ahead of consulting either `handles_event`.
    /// * `SharedState::apply`'s optional `GameEventBus`, which carries every
    ///   event to installed client plugins; the shipped brand-channel plugin
    ///   consumes `CustomPayload` there.
    ///
    /// Chunk payloads are a fourth path but not a router: the version adapter
    /// writes them straight through the `lodestone_world::WorldSink`, and the
    /// event is only a dirty-region signal.
    pub client: bool,
}

impl Route {
    /// Claimed by nothing. A legal, and sometimes correct, answer — see
    /// [`route`]'s note on what it costs to write it.
    pub const NOWHERE: Self = Self {
        ingest: false,
        session: false,
        shell: false,
        shell_conditional: false,
        client: false,
    };

    /// `true` when `lodestone_shell::net::forward` must have an **unconditional**
    /// arm for this event. The `debug_assert!` on that function's catch-all reads
    /// exactly this.
    #[must_use]
    pub const fn must_forward(self) -> bool {
        self.shell && !self.shell_conditional
    }

    /// `true` when nothing in the tree consumes the event: decoded, tested, and
    /// reaching zero pixels. Not a bug by itself — plenty of packets are decoded
    /// ahead of a consumer — but it is the shape `CLAUDE.md` §1 calls an island,
    /// and it is what `docs/event-routing.md` keeps a list of.
    #[must_use]
    pub const fn is_island(self) -> bool {
        !self.ingest && !self.session && !self.shell && !self.client
    }
}

/// Which routers claim `event`, as a single exhaustive table.
///
/// # The convention, which is the whole decision this match exists to force
///
/// * **per-entity state** → `ingest`. Components on an ECS entity: position,
///   metadata, equipment, hurt animation.
/// * **local-player scalars** → `session`. Anything scoped to *this* session:
///   health, xp, abilities, open menus, the scoreboard.
/// * **block and world state** → `shell`. It travels the shell's own `NetUpdate`
///   stream and needs no `handles_event` arm at all — the chest-lid work needed
///   none.
///
/// Guessing the `ingest`/`session` fork wrong has cost work twice
/// (`DimensionTypeChanged`, `AbilitiesChanged`): both compile, both unit-test
/// green, and neither runs, because `SharedState::apply` forwards only what one of
/// the two predicates lists.
///
/// # The trade
///
/// Adding a `ClientEvent` variant now costs **one mandatory one-line arm here**,
/// and in exchange it **cannot** island silently. Before this table it cost
/// nothing and risked silence. [`Route::NOWHERE`] is still available, but it has
/// to be typed on purpose, with a reason next to it — which is the difference
/// between a decision and the defect.
///
/// # This table describes what the code does, not what it should do
///
/// It was transcribed from the three routers, arm for arm. Where it says
/// [`Route::NOWHERE`] and a fold nevertheless exists somewhere, that is a finding
/// recorded in `docs/event-routing.md`, **not** something this function quietly
/// fixes: changing a route here changes runtime behaviour and belongs in its own
/// reviewable commit.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn route(event: &ClientEvent) -> Route {
    const INGEST: Route = Route {
        ingest: true,
        ..Route::NOWHERE
    };
    const SESSION: Route = Route {
        session: true,
        ..Route::NOWHERE
    };
    const SHELL: Route = Route {
        shell: true,
        ..Route::NOWHERE
    };
    // The shell has an arm but it is guarded; see `Route::shell_conditional`.
    const SHELL_PARTIAL: Route = Route {
        shell: true,
        shell_conditional: true,
        ..Route::NOWHERE
    };
    const CLIENT: Route = Route {
        client: true,
        ..Route::NOWHERE
    };

    match event {
        // ---- the local player's arrival, claimed by everything ----------------
        //
        // `ingest` takes the entity id and the `EntityIndex` entry, `session`
        // takes the game mode / dimension / alive scalars, the shell takes
        // `NetUpdate::LoggedIn`, and the driver arms its `player_loaded` latch.
        // Four disjoint writes, one event.
        ClientEvent::Login { .. } => Route {
            ingest: true,
            session: true,
            shell: true,
            client: true,
            ..Route::NOWHERE
        },
        // `Respawned` is the same shape minus `ingest`: no entity id is reassigned.
        ClientEvent::Respawned { .. } => Route {
            session: true,
            shell: true,
            client: true,
            ..Route::NOWHERE
        },
        // `Death` drives the death screen (shell), the `alive` scalar (session),
        // and the driver's automatic respawn.
        ClientEvent::Death { .. } => Route {
            session: true,
            shell: true,
            client: true,
            ..Route::NOWHERE
        },

        // ---- per-entity ECS state -------------------------------------------
        ClientEvent::EntityMoved { .. }
        | ClientEvent::EntityVelocity { .. }
        | ClientEvent::EntityRemoved { .. }
        | ClientEvent::EntityHeadRotation { .. }
        | ClientEvent::EntityMetadataUpdated { .. }
        | ClientEvent::EntityAttributesUpdated { .. }
        | ClientEvent::EntityEquipmentUpdated { .. }
        | ClientEvent::EntityDamaged { .. }
        // Per-entity, not a shell-stream fact: the block state becomes a component
        // on the ingest entity and the render extract bridges it through
        // `EntityIndex`, exactly as `HurtTime` and `ItemUse` are. Routing it to the
        // shell instead would compile, test green, and never run — `apply` consults
        // both switches and only forwards what each lists.
        | ClientEvent::FallingBlockState { .. }
        // The other reading of the same spawn field, routed the same way and for
        // the same reason: the owner id becomes `ProjectileOwner` on the ingest
        // entity and the render extract bridges it through `EntityIndex`.
        | ClientEvent::ProjectileOwner { .. }
        // Per-entity despite carrying no entity id, and the distinction is worth
        // stating because "no id" reads as a local-player scalar. The server sends
        // the move vehicle packet only to *reject* a position the
        // client-authoritative rider reported, and what it changes is the
        // vehicle's own `Position`/`Rotation` — components `ingest` already owns
        // the sole writer of. The subject comes from `session::Riding`, exactly as
        // the seat pin already resolves its vehicle from that same scalar.
        | ClientEvent::VehicleMoved { .. }
        // The mob's own leash state (`SET_ENTITY_LINK`) — per-entity
        // like every other row in this block, folded by `lodestone_ecs::ingest::
        // apply_entity_leash` into `Leashed`. Used to be unclaimed entirely (see
        // the "claimed by nothing" block below, which is where this line lived
        // until the fold existed).
        | ClientEvent::EntityLeashed { .. }
        | ClientEvent::EntityAnimation { .. } => INGEST,
        // Both halves, and neither supersedes the other: `ingest` turns this into
        // the per-entity `HurtTime` countdown and destructures with `..`,
        // **discarding the yaw**, while the shell's own `forward` reads that yaw to
        // aim the damage camera tilt. Listing it as `INGEST` alone was not a
        // functional gap — `forward`'s `debug_assert` is one-directional, so the
        // wiring worked — but this table is what a reader consults to answer "does
        // anything consume event X", and understating a consumer here is exactly the
        // authoritative-looking-and-quietly-wrong record this repo pays for most.
        ClientEvent::EntityHurtAnimation { .. } => Route {
            ingest: true,
            shell: true,
            ..Route::NOWHERE
        },
        // Riding is genuinely both halves — the component pair one side, the local
        // player's own `Riding` scalar the other.
        ClientEvent::EntityPassengersChanged { .. } => Route {
            ingest: true,
            session: true,
            ..Route::NOWHERE
        },
        // Most statuses remain per-entity `ingest` state: byte 3 starts a
        // `DeathTime` on the entity the packet names. The local player additionally
        // consumes bytes 24..28 as its permission level, so the one queued event
        // also reaches the session fold. The two systems write disjoint components.
        ClientEvent::EntityStatus { .. } => Route {
            ingest: true,
            session: true,
            ..Route::NOWHERE
        },
        // `ingest` spawns the entity; the shell arm is guarded on
        // `lightning_bolt` and only counts flashes, so every other spawn
        // legitimately reaches `forward`'s catch-all.
        ClientEvent::EntitySpawned { .. } => Route {
            ingest: true,
            shell: true,
            shell_conditional: true,
            ..Route::NOWHERE
        },
        ClientEvent::PlayerProfileNamed { .. } => INGEST,

        // ---- local-player and session scalars --------------------------------
        ClientEvent::HealthChanged { .. }
        | ClientEvent::ExperienceChanged { .. }
        | ClientEvent::GameModeChanged { .. }
        | ClientEvent::AbilitiesChanged { .. }
        | ClientEvent::DimensionTypeChanged { .. }
        | ClientEvent::BiomeVisuals { .. } => SESSION,
        // Two of the three `HudState`-shaped islands this table found
        // (`docs/event-routing.md`): a fold existed and was unit-tested, but
        // `lodestone_game::player_state::HudState` itself has no production
        // caller (Stage 3 superseded it with the session components above and
        // never re-homed these two onto one). Both are local-player scalars,
        // so `session` is correct either way — the fix was writing
        // `crate::player::SelectedSlot` / `ServerDifficulty` from
        // `apply_local_player_state`, not reviving `HudState::apply`.
        | ClientEvent::HeldSlotChanged { .. }
        | ClientEvent::DifficultyChanged { .. } => SESSION,
        // The third: `lodestone_game::mining::BlockDestructionOverlays::apply`
        // existed and was unit-tested with no caller anywhere. This is about
        // *other players'* blocks, so it is tempting to read it as "block/world
        // state" and route it `shell` the way the chest-lid `BlockEvent` is —
        // but `BlockDestructionOverlays` is a per-session collection keyed by
        // breaking-entity id (one entity breaks one block at a time), the same
        // shape as `SessionBossBars`/`SessionTabList` just above, not a
        // world-geometry fact the mesher owns. Folded into
        // `SessionBlockDestruction` alongside them.
        ClientEvent::BlockDestruction { .. } => SESSION,
        // scoreboard, tab list, boss bars
        ClientEvent::ObjectiveUpdate { .. }
        | ClientEvent::DisplayObjective { .. }
        | ClientEvent::ScoreUpdate { .. }
        | ClientEvent::ScoreReset { .. }
        | ClientEvent::TeamUpdate { .. }
        | ClientEvent::PlayerListUpdate { .. }
        | ClientEvent::PlayerListRemove { .. }
        | ClientEvent::PlayerListRemoveByName { .. }
        | ClientEvent::BossBarUpdate { .. } => SESSION,
        // menus / containers
        ClientEvent::ScreenOpened { .. }
        | ClientEvent::ScreenClosed { .. }
        | ClientEvent::ContainerContent { .. }
        | ClientEvent::ContainerSlot { .. }
        | ClientEvent::ContainerData { .. }
        | ClientEvent::CursorItemChanged { .. }
        | ClientEvent::InventorySlotChanged { .. } => SESSION,

        // ---- the shell's own stream ------------------------------------------
        ClientEvent::Disconnect { .. }
        // `SessionFailed` is `Disconnect`'s client-side twin and takes the same
        // route, established from the consumer rather than from the shape: the
        // only thing in the tree that ends a session is
        // `SessionPhase::Ended`, and the only writer of that is
        // `lodestone_shell::sim::Sim::set_phase`, called from `poll_net`'s
        // `NetUpdate` arms. Nothing in `lodestone_ecs::session` folds a `Phase`
        // from a `ClientEvent` at all, so a `session` route here would compile,
        // test green, and reach no screen — and a terminal session failure is
        // not per-entity state either, which rules out `ingest`.
        | ClientEvent::SessionFailed { .. }
        | ClientEvent::Particles { .. }
        | ClientEvent::Sound { .. }
        | ClientEvent::EntitySound { .. }
        | ClientEvent::MobEffectApplied { .. }
        | ClientEvent::MobEffectRemoved { .. }
        | ClientEvent::TitleText { .. }
        | ClientEvent::SubtitleText { .. }
        | ClientEvent::TitlesAnimation { .. }
        | ClientEvent::TitlesCleared { .. }
        | ClientEvent::SectionBlocksChanged { .. }
        | ClientEvent::BlockEvent { .. }
        // A blast, same category as the two block-update variants just
        // above: world/block state, not per-entity (no entity owns a
        // world position) and not a local-player scalar (the knockback it
        // may carry is a one-shot impulse, not persisted session state) —
        // see `route`'s own doc for the convention this follows.
        | ClientEvent::Explosion { .. }
        | ClientEvent::ItemPickup { .. }
        | ClientEvent::WeatherChanged { .. }
        | ClientEvent::BiomeClimates { .. }
        // Same shape as `BiomeClimates` just above: a registry-generation
        // table folded into a shell-owned cell (`net::BiomeNameCell`), read by
        // the mesher at mesh time. No `handles_event` arm needed.
        | ClientEvent::BiomeRegistryNames { .. }
        // The credits screen: a pure world/session signal with no
        // per-entity or per-session scalar to fold, forwarded to the shell's
        // own `NetUpdate` stream exactly like `WeatherChanged`.
        | ClientEvent::WinGame
        // Same shape as `BiomeRegistryNames` just above — a
        // registry-generation table with one obvious consumer (the chat
        // box), no per-entity or per-session scalar to fold. Both travel the
        // shell's own stream; no `handles_event` arm needed for either.
        | ClientEvent::CommandTreeUpdated { .. }
        | ClientEvent::CommandSuggestionsReceived { .. }
        // `net.rs`'s `forward` already has an unconditional arm for this
        // (`NetUpdate::SignEditorOpened`, consumed by `sim/net_apply.rs`); this
        // entry used to live in the "claimed by nothing" block below, stale
        // from before that consumer landed.
        | ClientEvent::SignEditorOpened { .. }
        // The book-open signal follows the same shell-only path: `forward`
        // turns it into `NetUpdate::BookOpened`, and `Sim` holds it until the
        // app projects the selected hand into the book screen.
        | ClientEvent::BookOpened { .. } => SHELL,
        // `run_session` consumes this before calling `forward`: it clears the
        // live server pack and any matching prompt. There is no `NetUpdate` to
        // enqueue, so this is a shell interception rather than an unconditional
        // `forward` arm; `SHELL_PARTIAL` keeps the catch-all assertion honest.
        ClientEvent::ResourcePackPopped { .. } => SHELL_PARTIAL,
        // Chat reaches the shell feed *and* the driver's signed-message
        // acknowledgement valve.
        ClientEvent::Chat { .. } => Route {
            shell: true,
            client: true,
            ..Route::NOWHERE
        },
        // The shell camera adopts the authoritative pose; `LocalEcho` keeps the
        // read-model's `position()` honest; the driver consumes its
        // `player_loaded` latch on the first one after entering the world.
        ClientEvent::TeleportPlayer { .. } => Route {
            shell: true,
            client: true,
            ..Route::NOWHERE
        },
        // A dirty-region signal to the shell; the payload was already written
        // through the `WorldSink` by the adapter.
        ClientEvent::ChunkLoaded { .. } => Route {
            shell: true,
            client: true,
            ..Route::NOWHERE
        },
        // The eviction twin, and this entry used to read `CLIENT` with the
        // comment "the adapter has already dropped the column through the
        // `WorldSink`, so the event is a notification with nothing left to do."
        // That was true of the *world* and false of the *renderer*:
        // collision re-reads the store every tick and so tracked the
        // unload for free, while the GPU kept every section the column ever
        // uploaded — for the whole session, unculled, against a fixed-capacity
        // origin arena. Kept as a worked example of the failure mode `CLAUDE.md`
        // §2 warns about: a routing claim that is accurate about one consumer and
        // silently wrong about another, which nothing about it looks stale.
        ClientEvent::ChunkUnloaded { .. } => Route {
            shell: true,
            client: true,
            ..Route::NOWHERE
        },
        // Only sub-event 2001 (block-break effect) is consumed; the rest fall
        // through on purpose, so adding a consumer later is a new arm and not a
        // new packet.
        ClientEvent::LevelEvent { .. } => SHELL_PARTIAL,

        // ---- consumed inside `lodestone-client` ------------------------------
        // Answered by `Driver::emit`, and the tick surrogate that flushes pending
        // chat acknowledgements.
        ClientEvent::KeepAlive { .. } => CLIENT,
        // `Driver::emit` immediately turns the challenge into a pong action
        // before the shell event loop runs, so this is a client-internal
        // consumer rather than an island.
        ClientEvent::Ping { .. } => CLIENT,
        // The client retains the echoed request timestamp in its local
        // read-model; the shell compares it with its portable current clock for
        // the F3 round-trip-time line.
        ClientEvent::PongReceived { .. } => CLIENT,
        // `Driver::emit` answers from its in-memory cookie store immediately;
        // the resulting `CookieResponse` action is sent before the event reaches
        // the shell, so this is a client-internal consumer rather than an island.
        ClientEvent::CookieRequested { .. } => CLIENT,
        // `Driver::emit` writes the received payload into the same in-memory
        // cookie store used by `CookieRequested`, before the event reaches any
        // router. It is therefore consumed by the client even though no action
        // is emitted for the store operation itself.
        ClientEvent::CookieStored { .. } => CLIENT,
        // `Driver::emit` immediately answers the pushed pack before surfacing
        // the event, so configuration cannot stall waiting for the shell.
        ClientEvent::ResourcePackPushed { .. } => CLIENT,
        // `Driver::emit` records the existing `SessionOutcome::Transferred`
        // result before surfacing this event, so a caller can reconnect with
        // the target and the driver's preserved cookie store.
        ClientEvent::TransferRequested { .. } => CLIENT,
        // `Driver::emit` removes the deleted full signature from its pending
        // acknowledgement tracker before surfacing the event, so the server
        // is not acknowledged for a message it withdrew.
        ClientEvent::ChatMessageDeleted { .. } => CLIENT,
        // `SharedState::apply`'s own arm, ahead of both `handles_event` calls:
        // straight into the `WorldTime` resource.
        ClientEvent::TimeChanged { .. } => CLIENT,
        // `SharedState::apply` publishes this through the optional `GameEvent`
        // bus before routing. The shipped app installs a brand-channel plugin,
        // whose typed decoder and state fold consume this channel in production.
        ClientEvent::CustomPayload { .. } => CLIENT,

        // ---- world-level admin state, folded by `session` ---------------------
        //
        // All nine of these were in the island block below. They are `session`
        // rather than `ingest` because none is per-entity: they are scalars scoped
        // to the world this session is connected to, which is the same category
        // `DimensionTypeChanged` and `AbilitiesChanged` fall into — and both of
        // those cost work by being guessed as `ingest` first.
        //
        // `TabListChanged` is the one that needed no new fold at all:
        // `lodestone_game::tablist::TabList::apply` has had a header/footer arm
        // and `session::apply_tab_list` has been registered since before this
        // routing fix, so the event was decoded, folded-capable, and simply never
        // asked for. The other eight got folds in the same commit as this flag,
        // per the instruction in the block below.
        ClientEvent::TabListChanged { .. } => SESSION,
        // `lodestone_game::recipe::RecipeBookSettings` via
        // `apply_recipe_book_settings`. Not a world scalar like its neighbours
        // here — it is per-*player* UI state the server persists — but `session`
        // for exactly that reason, and certainly not `ingest`.
        ClientEvent::RecipeBookSettingsChanged { .. } => SESSION,
        // Keyed on map id, not on an entity: several players and several item
        // frames can show the same map, so this is session-scoped state
        // (`SessionMaps`) and never a component on the holder.
        ClientEvent::MapItemData { .. } => SESSION,
        // The tree and progress are the local player's, so `session`
        // (`SessionAdvancements`). The advancements *screen* reads that
        // component; it needs no `forward` arm.
        ClientEvent::AdvancementsUpdated { .. } => SESSION,
        // The selected advancement tab is separate from advancement progress:
        // the former is a UI cursor while the latter is the criterion tree.
        // Both are local-player session state and reach the screen through
        // their respective `Session*` components.
        ClientEvent::AdvancementsTabSelected { .. } => SESSION,
        // `lodestone_game::worldborder::WorldBorder` via `apply_world_border`.
        // The largest single cluster in `docs/event-routing.md`'s island list.
        ClientEvent::WorldBorderCenterChanged { .. }
        | ClientEvent::WorldBorderSizeLerping { .. }
        | ClientEvent::WorldBorderSizeChanged { .. }
        | ClientEvent::WorldBorderWarningDelayChanged { .. }
        | ClientEvent::WorldBorderWarningDistanceChanged { .. }
        | ClientEvent::WorldBorderInitialized { .. } => SESSION,
        // `lodestone_game::levelstate::SpawnPoint` via `apply_spawn_point` — the
        // compass target every legacy family's packet doc names.
        ClientEvent::SpawnPositionChanged { .. } => SESSION,
        // `lodestone_game::levelstate::GameRuleValues` via `apply_game_rules`.
        // Note this is *not* the typed registry, which is server-side and
        // unbuilt.
        ClientEvent::GameRulesChanged { .. } => SESSION,

        // ---- the remaining clientbound set, all session --------------------
        //
        // Every one of these folds into a `Session*` component in
        // `lodestone_ecs::session`, the same way the scoreboard, the tab list,
        // maps and advancements already do. That is why none of them appears in
        // `net::forward` and why the `debug_assert!` on its catch-all stays
        // quiet: `shell` is false on purpose, not by omission.
        //
        // `DebugEntityValue` is the one worth arguing about. It names an entity,
        // and `route`'s convention says per-entity state is `ingest` — but a
        // debug feed is keyed by *subscription* and outlives the entity's ECS
        // row, so folding it as a component would resurrect rows the client has
        // already dropped. It is session state about an entity, not entity state.
        // The server's own `minecraft:enchantment` order. Routed to `session`
        // rather than `shell` (where `BiomeRegistryNames` goes) on purpose: a
        // `shell` route needs an unconditional arm in `net::forward` or its
        // `debug_assert!` fires, and a session component reaches the same
        // consumer -- `Sim` holds the session `World` -- with no shell edit and
        // no second table. `BiomeRegistryNames` predates the session-fold
        // convention; it is not a precedent to copy.
        // The recipe/trade tranche folds into `SessionRecipeBook` and
        // `SessionTrades`; `MerchantOffersReceived` is a *menu* the way the other
        // container events are, so it is session state and not per-entity state
        // about the villager.
        ClientEvent::RecipeBookAdded { .. }
        | ClientEvent::RecipeBookRemoved { .. }
        | ClientEvent::GhostRecipeShown { .. }
        | ClientEvent::RecipePropertySetsUpdated { .. }
        | ClientEvent::MerchantOffersReceived { .. }
        | ClientEvent::EnchantmentRegistryNames { .. }
        | ClientEvent::StatisticsAwarded { .. }
        | ClientEvent::ChatCompletionsChanged { .. }
        | ClientEvent::DebugBlockValue { .. }
        | ClientEvent::DebugChunkValue { .. }
        | ClientEvent::DebugEntityValue { .. }
        | ClientEvent::DebugEvent { .. }
        | ClientEvent::DebugSample { .. }
        | ClientEvent::GameTestHighlightPos { .. }
        | ClientEvent::LowDiskSpaceWarning
        | ClientEvent::CustomReportDetails { .. }
        | ClientEvent::ServerLinksReceived { .. }
        | ClientEvent::WaypointUpdated { .. }
        | ClientEvent::TagQueryResponse { .. }
        | ClientEvent::TickingStateChanged { .. }
        | ClientEvent::TickingStepped { .. }
        | ClientEvent::TestInstanceBlockStatus { .. }
        | ClientEvent::DialogShown { .. }
        | ClientEvent::DialogCleared => SESSION,

        // ---- claimed by nothing ---------------------------------------------
        //
        // Decoded and tested, consumed nowhere. Each line here is a candidate
        // island; `docs/event-routing.md` records which of these are simply
        // ahead of a consumer. The three that had a fold sitting unwired behind
        // them (`BlockDestruction`, `HeldSlotChanged`, `DifficultyChanged`) were
        // fixed above and are no longer in this list, and neither are the nine
        // world-level scalars in the block immediately above.
        //
        // Do not "fix" one by flipping a flag: the flag only says who is *asked*,
        // and a router that is asked but has no system for the event drops it just
        // as silently. Write the system, then the flag, in one commit.
        // The placement predictor owns the single prediction sequence and its
        // pending snapshot ledger. `net::forward` carries this acknowledgement
        // to `Sim::settle_placement_predictions`, which retires every snapshot
        // the server has processed; leaving the event unclaimed made that ledger
        // grow once for every optimistic placement for the whole session.
        ClientEvent::BlockChangedAck { .. } => SHELL,
        // This is a local-player correction, but it belongs on the shell stream
        // rather than `session`: `PhysicsState` is the camera/raycast/egress pose
        // owner, and `net::forward` gives its frame-thread consumer both the
        // absolute-versus-relative flags. A session scalar would compile and leave
        // the rendered view pointed at the old direction.
        ClientEvent::PlayerRotationSet { .. } => SHELL,
        // The server's stream center is distinct from the local player during
        // the loading hand-off. `net::forward` carries it to the loading-grid
        // producer, whose cell queries must follow the server's center rather
        // than a predicted player position.
        ClientEvent::ChunkCacheCenterChanged { .. } => SHELL,
        ClientEvent::ProjectilePowerChanged { .. } => INGEST,
        ClientEvent::ItemCooldown { .. } => SESSION,
        // This is a server-owned world scalar, but it is only a local client
        // fact: the session fold retains it and the F3 instrument panel reads
        // that one value. It is not the streamed view radius, which remains a
        // shell route because it sizes the loading-grid consumer.
        ClientEvent::SimulationDistanceChanged { .. } => SESSION,
        ClientEvent::ServerDataReceived { .. } => SESSION,
        ClientEvent::MountScreenOpened { .. } => SESSION,
        // Combat tracking is a local-player session fact. The fold retains
        // active versus ended and the exact end duration; the F3 HUD reads it
        // rather than manufacturing a local combat timer.
        ClientEvent::PlayerCombatEntered | ClientEvent::PlayerCombatEnded { .. } => SESSION,
        // A stop packet names a sound/category filter, while the mixer owns
        // live voices by opaque handles. `net::forward` carries the filters to
        // `ShellAudio`, which keeps the packet-created name/category-to-handle
        // index and cancels every matching audible voice.
        ClientEvent::SoundStopped { .. } => SHELL,
        // The target is already server-resolved, so the shell can derive the
        // local view direction from the current feet or eye anchor. `PhysicsState`
        // is the existing camera, raycast, audio-listener, and movement-egress
        // consumer; retaining a second target record would reach none of them.
        ClientEvent::PlayerLookAt { .. } => SHELL,
        // `net::forward` carries the selected entity id to `Sim`, which reads
        // that entity's shared pose every frame to drive the rendered camera.
        ClientEvent::CameraSet { .. } => SHELL,
        // The server's actual streamed radius, not the launcher's request.
        // `net::forward` carries it to `Sim::set_view_radius`, which sets the
        // loading screen's chunk-grid size and progress denominator.
        ClientEvent::ChunkCacheRadiusChanged { .. } => SHELL,
    }
}

#[cfg(test)]
mod equipment_slot_tests {
    use super::EquipmentSlot;

    /// `from_name` is the inverse of `name` for **every** slot, checked against
    /// `ALL` rather than against a list restated here.
    ///
    /// Two hand-written matches that are supposed to be inverses is exactly the
    /// shape that drifts, and a spot-check of two or three slots would not see
    /// it. Iterating `ALL` means adding a variant fails this test until both
    /// matches learn about it.
    #[test]
    fn equipment_slot_names_round_trip() {
        for slot in EquipmentSlot::ALL {
            assert_eq!(
                EquipmentSlot::from_name(slot.name()),
                Some(slot),
                "{slot:?} did not survive name -> from_name"
            );
        }
        // The count is asserted too, so a variant added to the enum but not to
        // `ALL` cannot make the loop above vacuously pass over a short list.
        assert_eq!(EquipmentSlot::ALL.len(), 8);
    }

    /// The control: an unrecognised name is `None`, not a default. If this ever
    /// returns `Some`, the loop above is measuring a function that says yes to
    /// everything.
    #[test]
    fn an_unknown_equipment_slot_name_is_refused() {
        for name in ["", "chestplate", "CHEST", "minecraft:chest", "hand"] {
            assert_eq!(
                EquipmentSlot::from_name(name),
                None,
                "{name:?} must not resolve to a slot"
            );
        }
    }
}

#[cfg(test)]
mod block_state_ref_tests {
    use super::{BlockStateRef, LevelEventData};

    #[test]
    fn canonical_and_protocol_local_state_ids_keep_the_same_raw_value_distinct() {
        // A deliberately small value: accepting a protocol-local state just
        // because it fits a generated census is the boundary error this type
        // prevents.
        const RAW: u32 = 1;
        let canonical = BlockStateRef::canonical(RAW);
        let local = BlockStateRef::protocol_local(RAW);

        assert_eq!(canonical.raw(), RAW);
        assert_eq!(local.raw(), RAW);
        assert_ne!(canonical, local);
        assert!(matches!(canonical, BlockStateRef::Canonical(RAW)));
        assert!(matches!(local, BlockStateRef::ProtocolLocal(RAW)));
    }

    #[test]
    fn protocol_local_state_ids_preserve_the_full_unsigned_domain() {
        let local = BlockStateRef::protocol_local(u32::MAX);
        assert_eq!(local.raw(), u32::MAX);
        assert!(matches!(local, BlockStateRef::ProtocolLocal(u32::MAX)));
    }

    #[test]
    fn level_event_data_keeps_raw_payload_bits_when_it_tags_a_state() {
        let raw = LevelEventData::Raw(-1);
        let tagged = LevelEventData::BlockState(BlockStateRef::protocol_local(u32::MAX));

        assert_eq!(raw.raw_i32(), -1);
        assert_eq!(tagged.raw_i32(), -1);
        assert_ne!(raw, tagged, "a state source must not collapse into raw event data");
    }
}

#[cfg(test)]
mod route_tests {
    use super::{
        ClientEvent, Difficulty, LevelEventData, PackedMessageSignature, Route, Uuid, route,
    };
    use crate::{LookAnchor, Vec3, ids::Identifier, math::BlockPos};

    /// **The guard that protects the guard.**
    ///
    /// [`route`]'s whole value is that a new [`ClientEvent`] variant is a compile
    /// error (`E0004`) until it is routed. The obvious wrong way to silence that
    /// error is the one rustc itself suggests — `_ => todo!()`, or its friendlier
    /// cousin `_ => Route::NOWHERE`. Either one restores the exact wildcard that
    /// `#[non_exhaustive]` forces on every *other* consumer, deletes the guarantee
    /// in one line, and leaves a green tree behind. So the absence of a catch-all
    /// is asserted, not assumed.
    ///
    /// Reads this file's own source, in the spirit of
    /// `lodestone_shell`'s `no_wgsl_is_inlined_in_rust_sources`.
    #[test]
    fn route_has_no_catch_all_arm() {
        let source = include_str!("event.rs");
        let body = source
            .split_once("pub fn route(event: &ClientEvent) -> Route {")
            .expect("route() must exist in this file")
            .1;
        let body = body
            .split_once("\n#[cfg(test)]")
            .map_or(body, |(before, _)| before);

        let found = catch_all_lines(body);
        assert!(
            found.is_empty(),
            "`route` has a catch-all arm ({found:?}), which restores the wildcard \
             `#[non_exhaustive]` forces everywhere else and deletes the compile \
             error that is this function's entire purpose. Write the arm instead — \
             `Route::NOWHERE` is a legal answer, but per variant and on purpose."
        );

        // The control, per `CLAUDE.md`: an assertion of an absence is worth only
        // as much as the evidence the detector fires. These are the two spellings
        // rustc's own `E0004` help text suggests.
        assert_eq!(
            catch_all_lines("        _ => Route::NOWHERE,\n").len(),
            1,
            "the detector must see a bare wildcard arm"
        );
        assert_eq!(
            catch_all_lines("        _ => todo!(),\n").len(),
            1,
            "the detector must see rustc's suggested `todo!()` wildcard"
        );
        // …and must not fire on the `{ .. }` in every ordinary arm, which also
        // contains a `..` before a `=>`.
        assert!(
            catch_all_lines("        ClientEvent::Ping { .. } => Route::NOWHERE,\n").is_empty(),
            "the detector must not read an ordinary struct pattern as a wildcard"
        );
    }

    /// The public routing document quotes both the remaining terminal islands
    /// and the exhaustive variant total. Keep those numbers derived from this
    /// source, not remembered after an otherwise-correct routing edit.
    #[test]
    fn the_island_count_in_the_docs_matches_this_source() {
        let source = include_str!("event.rs");
        let route_body = source
            .split_once("pub fn route(event: &ClientEvent) -> Route {")
            .expect("route() must exist in this file")
            .1
            .split_once("\n#[cfg(test)]")
            .expect("the route tests must follow route()").0;

        let islands = route_body
            .match_indices("=> Route::NOWHERE,")
            .map(|(end, _)| {
                let arm_start = route_body[..end]
                    .rfind("\n        ClientEvent::")
                    .expect("every terminal NOWHERE arm must start with ClientEvent");
                route_body[arm_start..end].matches("ClientEvent::").count()
            })
            .sum::<usize>();

        let enum_body = source
            .split_once("pub enum ClientEvent {")
            .expect("ClientEvent must remain an enum")
            .1
            .split_once("\n}\n")
            .expect("ClientEvent enum must end before the next item").0;
        let variants = enum_body
            .lines()
            .filter(|line| {
                line.strip_prefix("    ").is_some_and(|rest| {
                    !rest.starts_with("    ")
                        && rest
                            .chars()
                            .next()
                            .is_some_and(char::is_uppercase)
                })
            })
            .count();

        let counts = include_str!("../../../docs/event-routing.md")
            .lines()
            .find(|line| line.contains("variants are currently `Route::NOWHERE`"))
            .and_then(|line| line.strip_prefix("**").and_then(|line| line.split_once("**")))
            .map(|(counts, _)| counts)
            .expect("event-routing.md must declare its island count");
        let (documented_islands, documented_variants) = counts
            .split_once(" of ")
            .map(|(islands, variants)| {
                (
                    islands.parse::<usize>().expect("documented island count"),
                    variants.parse::<usize>().expect("documented variant count"),
                )
            })
            .expect("event-routing.md count must read **N of M** variants");

        assert_eq!(documented_islands, islands);
        assert_eq!(documented_variants, variants);
    }



    /// Lines that would make [`route`]'s match exhaustive by accident.
    fn catch_all_lines(body: &str) -> Vec<&str> {
        body.lines()
            .map(str::trim)
            .filter(|line| {
                let stripped = line.strip_prefix('|').unwrap_or(line).trim_start();
                stripped == "_" || stripped.starts_with("_ =>") || stripped.starts_with("_ if")
            })
            .collect()
    }

    /// The reason [`Route`] is four booleans and not an enum, asserted rather than
    /// asserted-in-prose: one event is genuinely claimed by two routers at once.
    ///
    /// `ingest` folds the per-entity `Passengers`/`Vehicle` pair; `session` folds
    /// the local player's own `Riding` scalar. An enum would have forced one of
    /// those two folds to be dropped from the table on day one, and whichever half
    /// lost would have become an island with a green unit test behind it.
    #[test]
    fn one_event_can_be_claimed_by_two_routers_at_once() {
        let riding = ClientEvent::EntityPassengersChanged {
            vehicle_id: 1,
            passenger_ids: vec![2],
        };
        let r = route(&riding);
        assert!(r.ingest, "the component pair is per-entity ECS state");
        assert!(r.session, "the local player's own ride state is a session scalar");
        assert!(!r.is_island());
    }

    /// `Driver::emit` consumes a cookie request by producing the matching
    /// `CookieResponse` action before the event reaches any router. The route
    /// must record that client-internal consumer so this automatically answered
    /// event is not counted as an island.
    #[test]
    fn cookie_request_reaches_the_client_driver() {
        let event = ClientEvent::CookieRequested {
            key: Identifier::new("lodestone", "route-test").unwrap(),
        };
        let r = route(&event);
        assert!(!r.ingest && !r.session && !r.shell);
        assert!(r.client, "the driver answers cookie requests automatically");
        assert!(!r.is_island());
    }

    /// `Driver::emit` stores a cookie before the event reaches any router, so
    /// the route must record this client-internal consumer as well as the
    /// matching request's automatic response.
    #[test]
    fn cookie_store_reaches_the_client_driver() {
        let event = ClientEvent::CookieStored {
            key: Identifier::new("lodestone", "route-test").unwrap(),
            payload: vec![0xAA, 0xBB],
        };
        let r = route(&event);
        assert!(!r.ingest && !r.session && !r.shell);
        assert!(r.client, "the driver stores cookies automatically");
        assert!(!r.is_island());
    }

    /// `Driver::emit` answers a pushed resource pack before the event reaches
    /// any router. The route must record that client-internal response so the
    /// automatically answered event is not counted as an island.
    #[test]
    fn resource_pack_push_reaches_the_client_driver() {
        let event = ClientEvent::ResourcePackPushed {
            id: Uuid::nil(),
            url: "https://example.invalid/pack.zip".into(),
            hash: String::new(),
            required: false,
            prompt: None,
        };
        let r = route(&event);
        assert!(!r.ingest && !r.session && !r.shell);
        assert!(r.client, "the driver answers resource-pack pushes automatically");
        assert!(!r.is_island());
    }

    /// The shell's connection loop clears a pushed pack and its pending prompt
    /// before the event reaches generic `forward`. The route must record that
    /// existing consumer without requiring a `NetUpdate` arm for an event that
    /// has already been handled.
    #[test]
    fn resource_pack_pop_reaches_the_shell_interceptor() {
        let event = ClientEvent::ResourcePackPopped { id: Some(Uuid::nil()) };
        let r = route(&event);
        assert!(r.shell, "the connection loop clears popped server packs");
        assert!(
            r.shell_conditional,
            "the pop is consumed before generic forwarding"
        );
        assert!(!r.must_forward());
        assert!(!r.is_island());
    }

    /// A block-change acknowledgement is not merely protocol bookkeeping: it
    /// releases the placement predictor's pending snapshots after the server has
    /// applied their authoritative block writes. The shell route makes that
    /// lifecycle consumer visible to the exhaustive table.
    #[test]
    fn block_changed_ack_reaches_the_placement_prediction_consumer() {
        let r = route(&ClientEvent::BlockChangedAck { sequence: 7 });
        assert!(r.shell, "the shell owns the placement prediction ledger");
        assert!(r.must_forward(), "the acknowledgement needs a NetUpdate arm");
        assert!(!r.is_island());
    }

    /// A stop packet has no ECS state to fold: only the shell can translate its
    /// name/category filters back into the opaque mixer handles that are making
    /// an already audible server sound play.
    #[test]
    fn sound_stopped_reaches_the_shell_playback_consumer() {
        let r = route(&ClientEvent::SoundStopped {
            sound: None,
            category: None,
        });
        assert!(r.shell, "the shell owns live mixer voices");
        assert!(r.must_forward(), "the filters need the NetUpdate relay");
        assert!(!r.is_island());
    }

    /// The rotation correction travels to the frame-thread pose owner. Its
    /// relative flags are meaningful only against that live pose, so the route
    /// must be the shell stream rather than a passive session record.
    #[test]
    fn player_rotation_set_reaches_the_shell_pose_consumer() {
        let r = route(&ClientEvent::PlayerRotationSet {
            y_rot: 20.0,
            relative_y: true,
            x_rot: -5.0,
            relative_x: false,
        });
        assert!(r.shell, "the shell owns the drawn and egress pose");
        assert!(r.must_forward(), "the correction needs a NetUpdate arm");
        assert!(!r.is_island());
    }

    /// A server-directed look is not session history: it immediately changes
    /// the local pose the camera and outgoing movement read.
    #[test]
    fn player_look_at_reaches_the_shell_pose_consumer() {
        let r = route(&ClientEvent::PlayerLookAt {
            from_anchor: LookAnchor::Eyes,
            target: Vec3::new(4.0, 70.0, -8.0),
            at_entity: None,
        });
        assert!(r.shell, "the shell owns the live camera and movement pose");
        assert!(r.must_forward(), "the look target needs a NetUpdate arm");
        assert!(!r.is_island());
    }

    /// The simulation distance is a server-reported scalar, distinct from the
    /// streamed-view radius. The session component owns it and the F3 panel
    /// reads that component; forwarding it to the loading-grid path would make
    /// that panel report a different server decision.
    #[test]
    fn simulation_distance_reaches_the_session_instrument_panel() {
        let r = route(&ClientEvent::SimulationDistanceChanged { distance: 11 });
        assert!(r.session, "the F3 panel reads the session scalar");
        assert!(!r.ingest && !r.shell && !r.client);
        assert!(!r.is_island());

        let combat = route(&ClientEvent::PlayerCombatEntered);
        assert!(combat.session, "combat enter reaches its session fold");
        assert!(!combat.is_island());
    }

    /// Public server data is a server-owned session fact. The F3 overlay reads
    /// the folded message, while the session retains the optional icon for a
    /// later in-session identity screen.
    #[test]
    fn server_data_reaches_the_session_hud_consumer() {
        let r = route(&ClientEvent::ServerDataReceived {
            motd: crate::Text::literal("Copper Canyon"),
            icon: Some(vec![0x89, 0x50, 0x4e, 0x47]),
        });
        assert!(r.session, "the F3 overlay reads the session record");
        assert!(!r.ingest && !r.shell && !r.client);
        assert!(!r.is_island());

        let combat = route(&ClientEvent::PlayerCombatEnded { duration_ticks: 240 });
        assert!(combat.session, "combat end reaches its session fold");
        assert!(!combat.is_island());
    }

    /// The mount-open packet has no companion `ScreenOpened`: it supplies both
    /// a window id and the inventory's column count itself. The menu session
    /// consumes that directly so the shell's existing open-menu screen can draw
    /// before ordinary container content arrives.
    #[test]
    fn mount_screen_open_reaches_the_session_menu_consumer() {
        let r = route(&ClientEvent::MountScreenOpened {
            container_id: 1,
            inventory_columns: 3,
            entity_id: 7,
        });
        assert!(r.session, "the menu session builds the announced mount screen");
        assert!(!r.ingest && !r.shell && !r.client);
        assert!(!r.is_island());

        assert!(
            route(&ClientEvent::Ping { id: 7 }).client,
            "control: an unrelated ping must stay out of the menu session"
        );
    }

    /// `Driver::emit` withdraws a deleted full signature from its pending
    /// acknowledgement tracker before the event reaches the shell. The route
    /// must record that client-internal consumer so the event is not counted as
    /// an island.
    #[test]
    fn deleted_chat_reaches_the_client_driver() {
        let event = ClientEvent::ChatMessageDeleted {
            signature: PackedMessageSignature::Full(vec![0; 256]),
        };
        let r = route(&event);
        assert!(!r.ingest && !r.session && !r.shell);
        assert!(r.client, "the driver withdraws deleted chat signatures");
        assert!(!r.is_island());
    }

    /// The three `HudState`-shaped islands this table found
    /// (`docs/event-routing.md`) are fixed: each now reaches `session`, and
    /// none is an island any more. This is the routing half only — the
    /// control that the *fold* actually runs lives in
    /// `lodestone_ecs::session`'s own tests.
    #[test]
    fn the_three_hudstate_islands_are_fixed() {
        let held_slot = ClientEvent::HeldSlotChanged { slot: 3 };
        let r = route(&held_slot);
        assert!(r.session, "held-slot is a local-player scalar");
        assert!(!r.is_island());

        let difficulty = ClientEvent::DifficultyChanged {
            difficulty: Difficulty::Hard,
            locked: false,
        };
        let r = route(&difficulty);
        assert!(r.session);
        assert!(!r.is_island());

        let block_destruction = ClientEvent::BlockDestruction {
            entity_id: 7,
            pos: BlockPos::new(1, 2, 3),
            progress: 4,
        };
        let r = route(&block_destruction);
        assert!(
            r.session,
            "a per-session collection keyed by breaking-entity id, the same \
             shape as the scoreboard/tab-list/boss-bar family"
        );
        assert!(!r.is_island());
    }

    /// `shell_conditional` exists for exactly this: `LevelEvent`'s arm in
    /// `net::forward` matches the literal sub-event `2001`, so every *other*
    /// level event legitimately reaches the terminal `_ =>` — and the
    /// `debug_assert!` there must not fire on it.
    #[test]
    fn a_guarded_shell_arm_is_not_required_to_forward() {
        let level = ClientEvent::LevelEvent {
            event: 1234,
            pos: BlockPos::new(0, 0, 0),
            data: LevelEventData::Raw(0),
            global: false,
        };
        let r = route(&level);
        assert!(r.shell, "the shell does consume one sub-event of this variant");
        assert!(r.shell_conditional);
        assert!(
            !r.must_forward(),
            "a guarded arm must not be asserted on, or every non-2001 level event \
             trips the assert in `net::forward`"
        );

        // The contrast that gives the flag meaning: an unconditional shell arm.
        let cleared = ClientEvent::TitlesCleared { reset_times: true };
        assert!(route(&cleared).must_forward());
    }

    /// The cache-radius update has an unconditional shell forward: it changes
    /// the live loading views, so this route must keep the forward assertion on.
    #[test]
    fn cache_radius_update_is_routed_to_the_shell() {
        let route = route(&ClientEvent::ChunkCacheRadiusChanged { radius: 7 });
        assert!(route.shell, "the shell owns the loading-view radius");
        assert!(route.must_forward(), "the radius must cross net::forward");
        assert!(!route.is_island());
    }

    /// The stream center is a separate scalar from the radius: the visible
    /// loading grid must query the columns around the server's center, even
    /// while the local player still names its previous chunk.
    #[test]
    fn cache_center_update_is_routed_to_the_shell() {
        let route = route(&ClientEvent::ChunkCacheCenterChanged { x: -4, z: 9 });
        assert!(route.shell, "the shell owns the loading-grid center");
        assert!(route.must_forward(), "the center must cross net::forward");
        assert!(!route.is_island());
    }

    #[test]
    fn camera_set_is_routed_to_the_rendered_camera_consumer() {
        let route = route(&ClientEvent::CameraSet { entity_id: 99 });
        assert!(route.shell, "the shell resolves the selected camera entity");
        assert!(route.must_forward(), "the id must cross net::forward");
        assert!(!route.is_island());
    }

    /// `Route::NOWHERE` means nothing anywhere — including nothing in
    /// `lodestone-client`, which is what the `client` flag is for. Without that
    /// flag `TimeChanged` would read as an island while in fact
    /// `SharedState::apply` folds it into `WorldTime` ahead of both predicates.
    #[test]
    fn the_client_flag_keeps_nowhere_honest() {
        let time = ClientEvent::TimeChanged {
            world_age: 1,
            time_of_day: 2,
        };
        let r = route(&time);
        assert!(!r.ingest && !r.session && !r.shell, "no router claims it");
        assert!(r.client, "but `SharedState::apply` has its own arm for it");
        assert!(!r.is_island(), "so it is not an island");

        let transfer = ClientEvent::TransferRequested {
            host: "backend.example".into(),
            port: 25565,
        };
        let r = route(&transfer);
        assert!(!r.ingest && !r.session && !r.shell, "no router claims it");
        assert!(r.client, "but `Driver::emit` records the transfer outcome");
        assert!(!r.is_island(), "so it is not an island");

        let custom_payload = ClientEvent::CustomPayload {
            channel: "minecraft:brand".parse().unwrap(),
            data: vec![6, b'r', b'o', b'u', b't', b'e', b'd'],
        };
        let r = route(&custom_payload);
        assert!(!r.ingest && !r.session && !r.shell, "no router claims it");
        assert!(
            r.client,
            "SharedState publishes it to the production plugin event bus"
        );
        assert!(!r.is_island(), "the installed brand channel consumes it");

        let pong = ClientEvent::PongReceived { time: 1_700_000_123_456 };
        let r = route(&pong);
        assert!(!r.ingest && !r.session && !r.shell, "no router claims it");
        assert!(r.client, "the client preserves the echoed ping timestamp");
        assert!(!r.is_island(), "the F3 latency reader consumes it");

        let cooldown = route(&ClientEvent::ItemCooldown {
            group: "minecraft:ender_pearl".parse().unwrap(),
            duration_ticks: 80,
        });
        assert!(cooldown.session, "the hotbar cooldown veil reads the session fold");
        assert!(!cooldown.is_island());

        let combat = route(&ClientEvent::PlayerCombatEnded { duration_ticks: 240 });
        assert!(combat.session, "the combat HUD reads the session fold");
        assert!(!combat.is_island());
        assert_eq!(Route::NOWHERE, Route::default());
    }
}
