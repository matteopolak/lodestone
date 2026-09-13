//! Entity movement, metadata, equipment, and player-list payloads.

use uuid::Uuid;

use crate::*;
use super::*;

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

/// Velocity carried by a player position correction.
///
/// Newer protocols can replace or add to each velocity component independently,
/// and can rotate the current velocity into the corrected look direction first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TeleportVelocity {
    /// Per-axis velocity value sent by the server.
    pub delta: Vec3,
    /// Add `delta.x` to the current X velocity instead of replacing it.
    pub relative_x: bool,
    /// Add `delta.y` to the current Y velocity instead of replacing it.
    pub relative_y: bool,
    /// Add `delta.z` to the current Z velocity instead of replacing it.
    pub relative_z: bool,
    /// Rotate current velocity by the correction's rotation change before applying `delta`.
    pub rotate_delta: bool,
}

impl TeleportVelocity {
    /// Resolve this correction against the current velocity and the look
    /// direction before and after the position correction.
    #[must_use]
    pub fn resolve(self, current: Vec3, before: Rotation, after: Rotation) -> Vec3 {
        let mut current = current;
        if self.rotate_delta {
            let pitch_delta = f64::from(before.pitch - after.pitch).to_radians();
            let (pitch_sin, pitch_cos) = pitch_delta.sin_cos();
            current = Vec3::new(
                current.x,
                current.y * pitch_cos + current.z * pitch_sin,
                current.z * pitch_cos - current.y * pitch_sin,
            );
            let yaw_delta = f64::from(before.yaw - after.yaw).to_radians();
            let (yaw_sin, yaw_cos) = yaw_delta.sin_cos();
            current = Vec3::new(
                current.x * yaw_cos + current.z * yaw_sin,
                current.y,
                current.z * yaw_cos - current.x * yaw_sin,
            );
        }
        Vec3::new(
            if self.relative_x {
                current.x + self.delta.x
            } else {
                self.delta.x
            },
            if self.relative_y {
                current.y + self.delta.y
            } else {
                self.delta.y
            },
            if self.relative_z {
                current.z + self.delta.z
            } else {
                self.delta.z
            },
        )
    }
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

/// Which axes of a display entity follow the camera rather than its own rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BillboardMode {
    /// Keep the entity's own yaw and pitch. Wire ordinal `0`.
    #[default]
    Fixed,
    /// Follow camera yaw while retaining entity pitch. Wire ordinal `1`.
    Vertical,
    /// Retain entity yaw while following camera pitch. Wire ordinal `2`.
    Horizontal,
    /// Follow both camera axes. Wire ordinal `3`.
    Center,
}

impl BillboardMode {
    /// Decodes the wire ordinal, including the protocol's out-of-range fallback
    /// to [`Self::Fixed`].
    #[must_use]
    pub const fn from_wire(raw: u8) -> Self {
        match raw {
            1 => Self::Vertical,
            2 => Self::Horizontal,
            3 => Self::Center,
            _ => Self::Fixed,
        }
    }

    /// The protocol ordinal for this mode.
    #[must_use]
    pub const fn wire_id(self) -> u8 {
        match self {
            Self::Fixed => 0,
            Self::Vertical => 1,
            Self::Horizontal => 2,
            Self::Center => 3,
        }
    }
}

#[cfg(test)]
mod billboard_mode_tests {
    use super::BillboardMode;

    #[test]
    fn every_defined_wire_ordinal_round_trips() {
        for mode in [
            BillboardMode::Fixed,
            BillboardMode::Vertical,
            BillboardMode::Horizontal,
            BillboardMode::Center,
        ] {
            assert_eq!(BillboardMode::from_wire(mode.wire_id()), mode);
        }
    }

    #[test]
    fn unknown_wire_ordinals_use_the_fixed_fallback() {
        assert_eq!(BillboardMode::from_wire(4), BillboardMode::Fixed);
        assert_eq!(BillboardMode::from_wire(u8::MAX), BillboardMode::Fixed);
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
    /// The optional player model-layer visibility byte, when present. Bit
    /// `0x01` enables the cape layer; the remaining bits are other model-layer
    /// toggles that this client does not currently consume.
    pub player_model_customization: Option<u8>,
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
    /// A primed TNT entity's fuse time in ticks, when present and the entity is
    /// known to be primed TNT. The client renders its final-ten-tick swell and
    /// alternating white flash from this countdown.
    ///
    /// This shares an `INT` at index 8 with an orb value, a fishing-hook target,
    /// a vehicle hurt clock and a display interpolation delay. A version adapter
    /// therefore raises it only after it has established the concrete TNT type.
    pub tnt_fuse: Option<i32>,
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
    /// The display entity's billboard constraint, decoded from its wire ordinal
    /// when present and the entity is known to be one of the three display
    /// subtypes (`text_display`/`item_display`/`block_display`).
    ///
    /// # Why this can be absent on a packet that carried the byte
    ///
    /// Same shape as [`mob_flags`](Self::mob_flags): index 15's `BYTE` is also
    /// a mob's flags field and an armor stand's client-flags field. `None`
    /// means "not known to be a display billboard byte", which a consumer
    /// must treat as "no billboard reported yet", never as a cleared value.
    pub display_billboard: Option<BillboardMode>,
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
            && self.player_model_customization.is_none()
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
