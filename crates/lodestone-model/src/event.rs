use uuid::Uuid;

use crate::{
    common::{Difficulty, GameMode},
    ids::{DimensionId, Identifier, ResourceKey},
    item::ItemStack,
    math::{BlockPos, ChunkPos, Rotation, SectionPos, Vec3, Vec3f},
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
/// `ClientboundRespawnPacket` (and the game-join packet's equivalent).
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
/// name wrong (issue #34).
///
/// Version adapters fill this in from the Configuration `registry_data` packet;
/// before #288 nothing decoded that packet at all, so every field here was
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
    /// Raw message signature bytes.
    pub signature: Vec<u8>,
    /// Server-global signed-chat index.
    pub global_index: i32,
    /// Whether the message was shown to the user after filtering.
    pub was_shown: bool,
}

/// A packed message signature, from `MessageSignature.Packed`.
///
/// The wire form is either a full 256-byte signature (for a signature the
/// client has not cached yet) or an index into the last-seen signature
/// cache. This adapter does not track that cache, so `Cached` indices are
/// carried as-is rather than resolved.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PackedMessageSignature {
    /// A full 256-byte signature.
    Full(Vec<u8>),
    /// An index into the last-seen signature cache.
    Cached(i32),
}

/// The anchor point used by `ClientboundPlayerLookAtPacket`'s
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

/// A version-free entity animation kind, from `ClientboundAnimatePacket`.
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
    /// and when the entity is known to be a `Mob`. Decode through
    /// `lodestone_entity::metadata::MobFlags` rather than by masking inline.
    ///
    /// # Why this is separate from [`living_flags`](Self::living_flags)
    ///
    /// It is a different byte at a different index, declared by a different
    /// vanilla class, and it is what actually drives a *mob*'s arm pose. Vanilla's
    /// mob renderers (`AbstractSkeletonRenderer`, `DrownedRenderer`,
    /// `AbstractZombieModel`) read `Mob.isAggressive()`; the using-item bit behind
    /// [`living_flags`](Self::living_flags) is the *player* mechanism. A skeleton
    /// drawing on you never sets the using-item bit, so a client that only decodes
    /// index 8 leaves every mob in the rest pose (issue #379).
    ///
    /// # Why this can be absent on a packet that carried the byte
    ///
    /// Same reason as [`living_flags`](Self::living_flags), one notch tighter. The
    /// byte's index is shared with `ArmorStand`'s client-flags byte of the same
    /// serializer, and an armour stand *is* a living entity — so establishing
    /// "living" is not enough and the adapter must establish `Mob`. `None`
    /// therefore means "not known to be mob flags", which a consumer must read as
    /// "not aggressive", never as a cleared bitfield.
    pub mob_flags: Option<u8>,
    /// The custom name. [`Reported::Unreported`] when this packet did not
    /// mention it; [`Reported::Reported(None)`](Reported::Reported) is an
    /// explicit clear; [`Reported::Reported(Some(name))`](Reported::Reported)
    /// is the name it now holds.
    pub custom_name: Reported<String>,
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
    /// Current air supply in ticks, when present (`Entity.DATA_AIR_SUPPLY_ID`).
    /// Feeds the HUD's underwater bubble row (`docs/sky-and-air-bubbles.md`).
    pub air_supply: Option<i32>,
    /// A creeper's fuse direction (`Creeper.DATA_SWELL_DIR`), when present and
    /// the entity is known to be a creeper: `-1` while idle or backing off,
    /// `1` while counting up to detonation. The counter itself
    /// (`Creeper.swell`/`oldSwell`) is never sent — only the direction is, and
    /// a consumer integrates it client-side one tick at a time, exactly as
    /// vanilla's own client does (`Creeper.java:139`). See
    /// `lodestone_render::entity_anim::pose_swelling`'s docs for why the split
    /// between "synced direction" and "locally integrated counter" exists.
    pub creeper_swell_dir: Option<i32>,
    /// Whether a creeper is charged (lightning-struck), when present and the
    /// entity is known to be a creeper (`Creeper.DATA_IS_POWERED`). Doubles the
    /// explosion radius and drops a charged mob's head; set once and never
    /// cleared.
    pub creeper_powered: Option<bool>,
    /// Whether a creeper's fuse has been lit (flint-and-steel or fire charge),
    /// when present and the entity is known to be a creeper
    /// (`Creeper.DATA_IS_IGNITED`). Set once and never cleared — distinct from
    /// [`creeper_swell_dir`](Self::creeper_swell_dir) alone being positive,
    /// which also happens from proximity (`SwellGoal`) without ever igniting.
    pub creeper_ignited: Option<bool>,
}

impl EntityMetadataUpdate {
    /// Whether this update carries no fields at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.flags.is_none()
            && self.living_flags.is_none()
            && self.mob_flags.is_none()
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
    /// Slots in vanilla `EquipmentSlot` declaration order.
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

    /// Returns the slot for a vanilla `EquipmentSlot` ordinal.
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

    /// Returns this slot's canonical vanilla name, as `minecraft:equippable`
    /// spells it.
    ///
    /// Note `Body` and `Saddle` are **not** humanoid armour: vanilla gates
    /// wearable-by-a-player on `EquipmentSlot.Type.HUMANOID_ARMOR`, which covers
    /// only feet/legs/chest/head. A consumer that folds `"body"` into `"chest"`
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

    /// Returns this slot's vanilla `EquipmentSlot` ordinal.
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
    /// Player profile UUID.
    pub uuid: Uuid,
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
    /// Categories in vanilla `SoundSource` declaration order.
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

    /// Returns the category for a vanilla `SoundSource` ordinal.
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

    /// Returns this category's vanilla `SoundSource` ordinal.
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
        /// Signed-chat acknowledgement metadata, when this chat contributes to
        /// the last-seen acknowledgement window.
        ack: Option<ChatAckInfo>,
    },
    /// The server disconnected the client.
    Disconnect {
        /// Disconnect reason.
        reason: Text,
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
        /// Event-specific data.
        data: i32,
        /// Whether the event is global rather than distance-limited.
        global: bool,
    },
    /// Particles should spawn.
    Particles {
        /// Canonical particle type key.
        particle: ResourceKey,
        /// Whether the particles should be visible at long distance.
        long_distance: bool,
        /// Particle origin.
        pos: Vec3,
        /// Randomized offset bounds.
        offset: Vec3f,
        /// Particle speed parameter.
        max_speed: f32,
        /// Number of particles to spawn.
        count: i32,
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
    /// yaw/pitch, from `ClientboundPlayerRotationPacket`.
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
    /// `ClientboundSetCameraPacket`.
    CameraSet {
        /// The entity id the camera now follows. Vanilla sends the local
        /// player's own id to reset the camera to the first-person view.
        entity_id: i32,
    },
    /// A written book screen should open, from `ClientboundOpenBookPacket`.
    BookOpened {
        /// `true` for the main hand, `false` for the off hand.
        main_hand: bool,
    },
    /// A sound (or sounds) should stop playing, from
    /// `ClientboundStopSoundPacket`. Absent fields are wildcards: `sound: None`
    /// stops every sound in `category` (or all sounds if `category` is also
    /// `None`), not "no sound".
    SoundStopped {
        /// Sound to stop, or `None` to match any sound.
        sound: Option<ResourceKey>,
        /// Category to restrict the stop to, or `None` to match any category.
        category: Option<SoundCategory>,
    },
    /// The player list header/footer text changed, from
    /// `ClientboundTabListPacket`.
    TabListChanged {
        /// Header text shown above the player list.
        header: Text,
        /// Footer text shown below the player list.
        footer: Text,
    },
    /// The world border's center moved, from
    /// `ClientboundSetBorderCenterPacket`.
    WorldBorderCenterChanged {
        /// New center X coordinate.
        x: f64,
        /// New center Z coordinate.
        z: f64,
    },
    /// The world border began (or continued) smoothly resizing, from
    /// `ClientboundSetBorderLerpSizePacket`.
    WorldBorderSizeLerping {
        /// Size (diameter, in blocks) the border is resizing from.
        old_size: f64,
        /// Size (diameter, in blocks) the border is resizing to.
        new_size: f64,
        /// Duration of the resize, in milliseconds.
        lerp_time_ms: i64,
    },
    /// The world border's size changed instantly (no interpolation), from
    /// `ClientboundSetBorderSizePacket`.
    WorldBorderSizeChanged {
        /// New size (diameter, in blocks).
        size: f64,
    },
    /// The world border's warning delay changed, from
    /// `ClientboundSetBorderWarningDelayPacket`.
    WorldBorderWarningDelayChanged {
        /// New warning delay, in seconds, before the border starts closing in.
        warning_time: i32,
    },
    /// The world border's warning distance changed, from
    /// `ClientboundSetBorderWarningDistancePacket`.
    WorldBorderWarningDistanceChanged {
        /// New distance, in blocks, at which the warning effect appears.
        warning_blocks: i32,
    },
    /// The world border was fully (re)initialized, from
    /// `ClientboundInitializeBorderPacket` — sent on join/respawn instead of
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
    /// `ClientboundPlayerCombatEnterPacket` (no payload).
    PlayerCombatEntered,
    /// Combat tracking ended for the local player, from
    /// `ClientboundPlayerCombatEndPacket`.
    PlayerCombatEnded {
        /// Duration of the combat encounter, in ticks.
        duration_ticks: i32,
    },
    /// The server opened a sign-editing UI, from
    /// `ClientboundOpenSignEditorPacket`.
    SignEditorOpened {
        /// Block position of the sign.
        pos: BlockPos,
        /// Whether the front (vs. back) text is being edited.
        is_front_text: bool,
    },
    /// The advancements screen should switch to a given tab, from
    /// `ClientboundSelectAdvancementsTabPacket`.
    AdvancementsTabSelected {
        /// Tab identifier, or `None` to close/deselect the tab.
        tab: Option<Identifier>,
    },
    /// A projectile's acceleration power changed (e.g. a charged crossbow
    /// bolt), from `ClientboundProjectilePowerPacket`.
    ProjectilePowerChanged {
        /// Projectile entity id.
        entity_id: i32,
        /// New acceleration power.
        acceleration_power: f64,
    },
    /// A ridden entity's (e.g. horse, llama) inventory screen was opened,
    /// from `ClientboundMountScreenOpenPacket`.
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
    /// `ClientboundGameRuleValuesPacket`.
    GameRulesChanged {
        /// Game rule identifier and its raw string value, in wire order.
        values: Vec<(Identifier, String)>,
    },
    /// The server asked the client to reconnect to a different address, from
    /// `ClientboundTransferPacket`.
    TransferRequested {
        /// Target server host.
        host: String,
        /// Target server port.
        port: i32,
    },
    /// The server requested a previously stored cookie, from
    /// `ClientboundCookieRequestPacket`.
    CookieRequested {
        /// Cookie key.
        key: Identifier,
    },
    /// The server asked the client to persist an opaque cookie, from
    /// `ClientboundStoreCookiePacket`.
    CookieStored {
        /// Cookie key.
        key: Identifier,
        /// Opaque payload (at most 5120 bytes).
        payload: Vec<u8>,
    },
    /// The server offered a resource pack, from
    /// `ClientboundResourcePackPushPacket`.
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
    /// `ClientboundResourcePackPopPacket`.
    ResourcePackPopped {
        /// Pack id to remove, or `None` to remove all packs.
        id: Option<Uuid>,
    },
    /// A plugin (custom payload) message arrived, from
    /// `ClientboundCustomPayloadPacket`.
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
    /// `ClientboundServerDataPacket`.
    ServerDataReceived {
        /// Message of the day.
        motd: Text,
        /// Favicon PNG bytes, if the server sent one.
        icon: Option<Vec<u8>>,
    },
    /// A play-state pong echo, from `ClientboundPongResponsePacket` (distinct
    /// from the keep-alive-like `Ping`/`ClientAction::PongResponse` pair).
    PongReceived {
        /// Echoed time value.
        time: i64,
    },
    /// A previously sent chat message was deleted/withdrawn, from
    /// `ClientboundDeleteChatPacket`.
    ChatMessageDeleted {
        /// The message's identity as carried on the wire.
        signature: PackedMessageSignature,
    },
    /// The local player should look toward a fixed point or another entity,
    /// from `ClientboundPlayerLookAtPacket`.
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
    /// death, from `ClientboundRespawnPacket`.
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
    /// the Configuration `registry_data` (issue #288).
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
    /// explicitly rather than inherit a plausible default, which is the shape
    /// issue #34 got wrong. `holder_id` is always present, so a consumer can log
    /// exactly which id failed to resolve.
    DimensionTypeChanged {
        /// The `minecraft:dimension_type` holder id the server sent.
        holder_id: i32,
        /// The resolved dimension type, or `None` — see above.
        dimension_type: Option<DimensionTypeInfo>,
    },
    /// The per-biome visual attributes the server declared in the Configuration
    /// `registry_data` (issue #96), **indexed by biome holder id**.
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
    /// The shell's arm is **conditional** — a match guard or a literal field
    /// pattern — so some values of this variant legitimately fall through to
    /// `forward`'s catch-all and the `debug_assert!` there must not fire.
    ///
    /// Two variants only, and both are a property of `net.rs` as it stands rather
    /// than of the event: `LevelEvent` (only sub-event `2001` is consumed) and
    /// `EntitySpawned` (only `lightning_bolt`, to count flashes). If either arm
    /// ever becomes unconditional, clear this and the assert gets stricter for
    /// free.
    pub shell_conditional: bool,
    /// Consumed inside `lodestone-client` itself by something that is **not** one
    /// of the three routers, so [`Route::NOWHERE`] can mean "nothing anywhere"
    /// rather than "nothing I happened to check". Exactly three such places:
    ///
    /// * `Driver::emit`'s auto-response switch (keep-alive, chat acknowledgement,
    ///   `player_loaded`, auto-respawn) — a protocol reply, not screen state.
    /// * `LocalEcho::apply`, which is down to `TeleportPlayer` alone.
    /// * `SharedState::apply`'s own `TimeChanged` arm, which writes `WorldTime`
    ///   ahead of consulting either `handles_event`.
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
        | ClientEvent::EntityHurtAnimation { .. }
        | ClientEvent::EntityAnimation { .. } => INGEST,
        // Riding is genuinely both halves — the component pair one side, the local
        // player's own `Riding` scalar the other.
        ClientEvent::EntityPassengersChanged { .. } => Route {
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

        // ---- local-player and session scalars --------------------------------
        ClientEvent::HealthChanged { .. }
        | ClientEvent::ExperienceChanged { .. }
        | ClientEvent::GameModeChanged { .. }
        | ClientEvent::AbilitiesChanged { .. }
        | ClientEvent::DimensionTypeChanged { .. }
        | ClientEvent::BiomeVisuals { .. } => SESSION,
        // scoreboard, tab list, boss bars
        ClientEvent::ObjectiveUpdate { .. }
        | ClientEvent::DisplayObjective { .. }
        | ClientEvent::ScoreUpdate { .. }
        | ClientEvent::ScoreReset { .. }
        | ClientEvent::TeamUpdate { .. }
        | ClientEvent::PlayerListUpdate { .. }
        | ClientEvent::PlayerListRemove { .. }
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
        | ClientEvent::ItemPickup { .. }
        | ClientEvent::WeatherChanged { .. } => SHELL,
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
        // Only sub-event 2001 (block-break effect) is consumed; the rest fall
        // through on purpose, so adding a consumer later is a new arm and not a
        // new packet.
        ClientEvent::LevelEvent { .. } => SHELL_PARTIAL,

        // ---- consumed inside `lodestone-client` ------------------------------
        // Answered by `Driver::emit`, and the tick surrogate that flushes pending
        // chat acknowledgements.
        ClientEvent::KeepAlive { .. } => CLIENT,
        // `SharedState::apply`'s own arm, ahead of both `handles_event` calls:
        // straight into the `WorldTime` resource.
        ClientEvent::TimeChanged { .. } => CLIENT,
        // The adapter has already dropped the column through the `WorldSink`, so
        // the event is a notification with nothing left to do.
        ClientEvent::ChunkUnloaded { .. } => CLIENT,

        // ---- claimed by nothing ---------------------------------------------
        //
        // Decoded and tested, consumed nowhere. Each line here is a candidate
        // island; `docs/event-routing.md` records which ones already have a fold
        // sitting unwired behind them (`BlockDestruction`, `HeldSlotChanged` and
        // `DifficultyChanged` do) versus which are simply ahead of a consumer.
        //
        // Do not "fix" one by flipping a flag: the flag only says who is *asked*,
        // and a router that is asked but has no system for the event drops it just
        // as silently. Write the system, then the flag, in one commit.
        ClientEvent::Ping { .. }
        | ClientEvent::SpawnPositionChanged { .. }
        | ClientEvent::BlockDestruction { .. }
        | ClientEvent::BlockChangedAck { .. }
        | ClientEvent::ChunkCacheCenterChanged { .. }
        | ClientEvent::ChunkCacheRadiusChanged { .. }
        | ClientEvent::SimulationDistanceChanged { .. }
        | ClientEvent::EntityStatus { .. }
        | ClientEvent::EntityLeashed { .. }
        | ClientEvent::VehicleMoved { .. }
        | ClientEvent::HeldSlotChanged { .. }
        | ClientEvent::ItemCooldown { .. }
        | ClientEvent::DifficultyChanged { .. }
        | ClientEvent::PlayerRotationSet { .. }
        | ClientEvent::CameraSet { .. }
        | ClientEvent::BookOpened { .. }
        | ClientEvent::SoundStopped { .. }
        | ClientEvent::TabListChanged { .. }
        | ClientEvent::WorldBorderCenterChanged { .. }
        | ClientEvent::WorldBorderSizeLerping { .. }
        | ClientEvent::WorldBorderSizeChanged { .. }
        | ClientEvent::WorldBorderWarningDelayChanged { .. }
        | ClientEvent::WorldBorderWarningDistanceChanged { .. }
        | ClientEvent::WorldBorderInitialized { .. }
        | ClientEvent::PlayerCombatEntered
        | ClientEvent::PlayerCombatEnded { .. }
        | ClientEvent::SignEditorOpened { .. }
        | ClientEvent::AdvancementsTabSelected { .. }
        | ClientEvent::ProjectilePowerChanged { .. }
        | ClientEvent::MountScreenOpened { .. }
        | ClientEvent::GameRulesChanged { .. }
        | ClientEvent::TransferRequested { .. }
        | ClientEvent::CookieRequested { .. }
        | ClientEvent::CookieStored { .. }
        | ClientEvent::ResourcePackPushed { .. }
        | ClientEvent::ResourcePackPopped { .. }
        | ClientEvent::CustomPayload { .. }
        | ClientEvent::ServerDataReceived { .. }
        | ClientEvent::PongReceived { .. }
        | ClientEvent::ChatMessageDeleted { .. }
        | ClientEvent::PlayerLookAt { .. } => Route::NOWHERE,
    }
}

#[cfg(test)]
mod route_tests {
    use super::{ClientEvent, Route, route};
    use crate::math::BlockPos;

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

    /// `shell_conditional` exists for exactly this: `LevelEvent`'s arm in
    /// `net::forward` matches the literal sub-event `2001`, so every *other*
    /// level event legitimately reaches the terminal `_ =>` — and the
    /// `debug_assert!` there must not fire on it.
    #[test]
    fn a_guarded_shell_arm_is_not_required_to_forward() {
        let level = ClientEvent::LevelEvent {
            event: 1234,
            pos: BlockPos::new(0, 0, 0),
            data: 0,
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

        assert!(
            route(&ClientEvent::PlayerCombatEntered).is_island(),
            "combat-entered really is decoded and consumed nowhere"
        );
        assert_eq!(Route::NOWHERE, Route::default());
    }
}
