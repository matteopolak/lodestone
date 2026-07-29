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
}

impl EntityMetadataUpdate {
    /// Whether this update carries no fields at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.flags.is_none()
            && !self.custom_name.is_reported()
            && self.custom_name_visible.is_none()
            && self.pose.is_none()
            && self.health.is_none()
            && self.baby.is_none()
            && self.variant.is_none()
            && !self.item.is_reported()
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
}
