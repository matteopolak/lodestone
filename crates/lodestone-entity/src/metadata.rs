//! Version-free entity metadata.
//!
//! Entity metadata is one of the most version-divergent parts of the protocol:
//! the **index** each field sits at and the **serializer id** used to encode it
//! move between versions constantly (a 1.8 entity's "flags" byte is at a
//! different index and uses a different type tag than a 1.21 one). Hardcoding
//! any of that here would defeat the version-isolation architecture.
//!
//! So this module holds only the *version-free* pieces:
//!
//! * [`MetadataValue`] — the value *shapes* that survive across versions
//!   (byte, int, float, string, boolean, rotation, block position, …). A
//!   version crate decodes the wire's serializer ids into these.
//! * [`EntityMetadata`] — an index-keyed bag of those values.
//! * [`MetadataSchema`] — the seam a version crate implements to say *which
//!   index* a semantic field lives at. This mirrors the
//!   [`LongArrayFraming`](lodestone_world) knob pattern in `lodestone-world`:
//!   the shared crate carries the semantics, a per-version value selects the
//!   layout.
//!
//! The bit meanings inside the shared entity flags byte (on-fire, crouching,
//! sprinting, …) *are* stable and live here as [`SharedEntityFlags`]; only the
//! byte's index is version-supplied.

use lodestone_model::BlockPos;
use std::collections::BTreeMap;
use uuid::Uuid;

/// A single metadata value, in the version-free shapes that are stable across
/// protocol versions.
///
/// A version crate is responsible for turning the wire's serializer-id-tagged
/// payload into one of these, and back. New shapes are added here as new
/// versions introduce genuinely new value kinds; the enum is `non_exhaustive`
/// so that is not a breaking change for match sites that use a wildcard arm.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MetadataValue {
    /// A signed byte (also used for bitfields like the base entity flags).
    Byte(i8),
    /// A variable-length signed integer.
    Int(i32),
    /// A variable-length signed long.
    Long(i64),
    /// A single-precision float.
    Float(f32),
    /// A UTF-8 string.
    String(String),
    /// A boolean.
    Boolean(bool),
    /// An optional block position (present flag + position).
    OptBlockPos(Option<BlockPos>),
    /// A block position.
    BlockPos(BlockPos),
    /// An optional UUID.
    OptUuid(Option<Uuid>),
    /// Euler rotation as three floats (used by armor-stand poses).
    Rotations([f32; 3]),
    /// A signed integer holding a registry id whose meaning is version-defined
    /// (e.g. a pose, a block-state id, a particle id). Kept opaque here.
    RegistryId(i32),
    /// A raw, version-opaque payload the shared crate does not interpret.
    ///
    /// This is the escape hatch: a value shape a version has that nothing else
    /// does travels as bytes and is understood only by that version's crate.
    Opaque(Vec<u8>),
}

impl MetadataValue {
    /// Interprets this value as a signed byte, if it is one.
    #[must_use]
    pub fn as_byte(&self) -> Option<i8> {
        match self {
            MetadataValue::Byte(b) => Some(*b),
            _ => None,
        }
    }

    /// Interprets this value as an `i32`, widening a byte if necessary.
    #[must_use]
    pub fn as_int(&self) -> Option<i32> {
        match self {
            MetadataValue::Int(v) | MetadataValue::RegistryId(v) => Some(*v),
            MetadataValue::Byte(b) => Some(i32::from(*b)),
            _ => None,
        }
    }

    /// Interprets this value as a float, if it is one.
    #[must_use]
    pub fn as_float(&self) -> Option<f32> {
        match self {
            MetadataValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Interprets this value as a boolean, if it is one.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            MetadataValue::Boolean(b) => Some(*b),
            _ => None,
        }
    }
}

/// An index-keyed bag of metadata values.
///
/// Vanilla transmits metadata as a sparse list of `(index, value)` entries and
/// applies them cumulatively; a `0xFF` index terminates the list. This bag
/// holds the accumulated state. Indices are ordered so iteration is
/// deterministic (helpful for tests and diffing).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntityMetadata {
    entries: BTreeMap<u8, MetadataValue>,
}

impl EntityMetadata {
    /// Creates an empty metadata bag.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies (inserts or overwrites) a value at `index`.
    pub fn set(&mut self, index: u8, value: MetadataValue) {
        self.entries.insert(index, value);
    }

    /// The value at `index`, if present.
    #[must_use]
    pub fn get(&self, index: u8) -> Option<&MetadataValue> {
        self.entries.get(&index)
    }

    /// Removes the value at `index`, returning it if present.
    pub fn remove(&mut self, index: u8) -> Option<MetadataValue> {
        self.entries.remove(&index)
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the bag is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates entries in ascending index order.
    pub fn iter(&self) -> impl Iterator<Item = (u8, &MetadataValue)> {
        self.entries.iter().map(|(k, v)| (*k, v))
    }

    /// The base entity flags byte, read from the version-supplied index, decoded
    /// into [`SharedEntityFlags`]. Returns `None` if absent or not a byte.
    #[must_use]
    pub fn shared_flags(&self, schema: &dyn MetadataSchema) -> Option<SharedEntityFlags> {
        self.get(schema.shared_flags_index())
            .and_then(MetadataValue::as_byte)
            .map(SharedEntityFlags::from_bits)
    }
}

/// The seam by which a version crate maps semantic fields to concrete indices.
///
/// Only the *base* entity fields are modelled here — enough to prove the seam —
/// because those are shared by every living entity. A version crate returns the
/// index its protocol uses. This is deliberately a trait (rather than a struct
/// of `u8`s) so a version can compute an index from an intra-family predicate if
/// it must, exactly like a small `LongArrayFraming`-style knob.
pub trait MetadataSchema {
    /// The index of the shared entity flags byte (`on fire / crouch / sprint /
    /// swim / invisible / glowing / elytra`).
    fn shared_flags_index(&self) -> u8;

    /// The index of the air-supply `VarInt`, if this version exposes it.
    fn air_supply_index(&self) -> Option<u8> {
        None
    }

    /// The index of the optional custom-name `Text`, if exposed.
    fn custom_name_index(&self) -> Option<u8> {
        None
    }

    /// The index of the custom-name-visible boolean, if exposed.
    fn custom_name_visible_index(&self) -> Option<u8> {
        None
    }
}

/// The bit meanings inside the shared entity flags byte. These bit positions are
/// stable across modern versions; only the byte's *index* is version-specific,
/// which is why they live in the shared crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SharedEntityFlags {
    /// The raw byte value.
    pub bits: u8,
}

impl SharedEntityFlags {
    const ON_FIRE: u8 = 0x01;
    const CROUCHING: u8 = 0x02;
    const SPRINTING: u8 = 0x08;
    const SWIMMING: u8 = 0x10;
    const INVISIBLE: u8 = 0x20;
    const GLOWING: u8 = 0x40;
    const FALL_FLYING: u8 = 0x80;

    /// Wraps a raw byte.
    #[must_use]
    pub const fn from_bits(bits: i8) -> Self {
        Self { bits: bits as u8 }
    }

    /// Whether the entity is on fire.
    #[must_use]
    pub const fn on_fire(self) -> bool {
        self.bits & Self::ON_FIRE != 0
    }

    /// Whether the entity is crouching (sneaking).
    #[must_use]
    pub const fn crouching(self) -> bool {
        self.bits & Self::CROUCHING != 0
    }

    /// Whether the entity is sprinting.
    #[must_use]
    pub const fn sprinting(self) -> bool {
        self.bits & Self::SPRINTING != 0
    }

    /// Whether the entity is swimming.
    #[must_use]
    pub const fn swimming(self) -> bool {
        self.bits & Self::SWIMMING != 0
    }

    /// Whether the entity is invisible.
    #[must_use]
    pub const fn invisible(self) -> bool {
        self.bits & Self::INVISIBLE != 0
    }

    /// Whether the entity has the glowing effect.
    #[must_use]
    pub const fn glowing(self) -> bool {
        self.bits & Self::GLOWING != 0
    }

    /// Whether the entity is fall-flying (elytra).
    #[must_use]
    pub const fn fall_flying(self) -> bool {
        self.bits & Self::FALL_FLYING != 0
    }
}

/// Which hand an entity is using an item with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsedHand {
    /// The entity's main hand (right for a right-handed humanoid).
    #[default]
    Main,
    /// The entity's off hand.
    Off,
}

/// The bit meanings inside the **living-entity** flags byte — a different byte
/// from [`SharedEntityFlags`], carried at a different metadata index, and the one
/// that says an entity is *using* an item (drawing a bow, charging a crossbow,
/// eating, blocking).
///
/// # Why this is not folded into [`SharedEntityFlags`]
///
/// They are two distinct synced fields. Vanilla's base entity type defines the
/// shared-flags byte; the living-entity type defines a second byte of its own, and
/// only living entities have it. Merging them would mean inventing an index that
/// does not exist and would make `on_fire()` and `using_item()` readable off the
/// same value, which they are not.
///
/// # The absent field: how far the item is drawn
///
/// Bit 0 says an item is *in use*; it does **not** say for how long, and vanilla
/// **does not sync the remaining use-duration at all**. The client starts its own
/// countdown when the bit flips on — computing the duration itself, client-side
/// only, from the item and entity — and decrements it locally each tick. So a
/// consumer that wants a *draw
/// fraction* must keep its own per-entity tick counter seeded from this bit's
/// rising edge; there is no wire field to read it from, and waiting for one is
/// waiting for a packet that is never sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LivingEntityFlags {
    /// The raw byte value.
    pub bits: u8,
}

impl LivingEntityFlags {
    const USING_ITEM: u8 = 0x01;
    const OFFHAND: u8 = 0x02;
    const SPIN_ATTACK: u8 = 0x04;

    /// Wraps a raw byte.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self { bits }
    }

    /// Whether the entity is currently using (holding down) an item.
    ///
    /// Bit 0, `(flags & 1) > 0`.
    #[must_use]
    pub const fn using_item(self) -> bool {
        self.bits & Self::USING_ITEM != 0
    }

    /// Which hand the item is being used with. Meaningful only while
    /// [`using_item`](Self::using_item); vanilla reads the hand bit
    /// unconditionally, but the hand of an entity that is not using anything is
    /// not a fact about anything.
    #[must_use]
    pub const fn used_hand(self) -> UsedHand {
        if self.bits & Self::OFFHAND != 0 {
            UsedHand::Off
        } else {
            UsedHand::Main
        }
    }

    /// Whether the entity is in a riptide spin attack.
    ///
    /// Bit 2, `(flags & 4) != 0`, and is in this byte rather than the pose enum,
    /// which also has a `SPIN_ATTACK` value. The two are set together by vanilla
    /// but are separate wire fields.
    #[must_use]
    pub const fn spin_attack(self) -> bool {
        self.bits & Self::SPIN_ATTACK != 0
    }
}

/// The bit meanings inside the **mob** flags byte — a *third* distinct byte, at a
/// third metadata index, declared for mob-type entities rather than for the base
/// entity ([`SharedEntityFlags`]) or the living-entity byte ([`LivingEntityFlags`]).
///
/// # Why a client needs this, and why the using-item bit is not a substitute
///
/// [`aggressive`](Self::aggressive) is what makes a **mob** hold a weapon pose.
/// Vanilla's mob renderers read the aggressive bit directly: a skeleton-family
/// renderer picks a bow-and-arrow arm pose when aggressive and holding a bow, a
/// drowned renderer picks a trident-throwing pose on the same test with a
/// trident, and the shared zombie-arm animation takes it as a parameter and
/// lifts the arms further when it is set.
///
/// The using-item bit in [`LivingEntityFlags`] is a *player* mechanism, driven by
/// the item-use state a player enters while holding down use. A skeleton
/// shooting at you never sets it — its ranged-attack goal fires without ever
/// entering that state — so a client that decodes only that bit leaves every mob
/// permanently in the rest pose while looking, at the wire level, entirely
/// correct.
///
/// # This byte's index collides with an armour stand's, and *living* is too weak
///
/// The mob-flags field and an armour stand's client-flags field share metadata
/// index 15, `BYTE`, where the same `0x04` means "show arms". An armour stand is
/// a living entity, so an is-living guard does not separate them; the version
/// adapter must establish the concrete entity is a mob. A `None` mob-flags byte
/// therefore means "not known to be mob flags" and must be read as *not
/// aggressive*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MobFlags {
    /// The raw byte value.
    pub bits: u8,
}

impl MobFlags {
    const NO_AI: u8 = 0x01;
    const LEFT_HANDED: u8 = 0x02;
    const AGGRESSIVE: u8 = 0x04;

    /// Wraps a raw byte.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self { bits }
    }

    /// Whether the mob's AI is disabled.
    ///
    /// Not consumed by rendering, and modelled anyway because the alternative is a
    /// bare `0x04` mask with no name for the bits either side of it.
    #[must_use]
    pub const fn no_ai(self) -> bool {
        self.bits & Self::NO_AI != 0
    }

    /// Whether the mob is left-handed.
    ///
    /// Vanilla treats this as flipping which arm counts as the "main" one, which
    /// flips which arm every arm pose applies to. Consumed end to end: it folds
    /// into `MobState::left_handed` and threads through `EntityDraw::main_arm_left`,
    /// then gets XORed against the held-hand bit in `arm_pose_for`'s two pose
    /// paths and in the `EquipmentSlot`-to-`Arm` mappings the entity and
    /// world-item draw passes use.
    #[must_use]
    pub const fn left_handed(self) -> bool {
        self.bits & Self::LEFT_HANDED != 0
    }

    /// Whether the mob is aggressive — `(flags & 4) != 0`.
    ///
    /// "Aggressive" is vanilla's own name for it and it is set by the ranged and
    /// melee attack goals while a target is being engaged, so it is closer to
    /// "currently attacking" than to a permanent disposition.
    #[must_use]
    pub const fn aggressive(self) -> bool {
        self.bits & Self::AGGRESSIVE != 0
    }
}

/// The bit meanings inside an armour stand's client-flags byte — the byte that
/// shares [`MobFlags`]'s metadata index (15) on 26.2, `BYTE`-for-`BYTE`, with
/// unrelated bit meanings. `0x04` means "show arms" here and "aggressive" in
/// [`MobFlags`]; a version adapter that cannot establish the concrete entity is
/// an armour stand must withhold this byte entirely rather than guess (see
/// [`MobFlags`]'s own doc for why the index collides and why `is_living` cannot
/// resolve it).
///
/// # Why this exists: the "hologram" case
///
/// A server-side "hologram" — invisible, nametagged floating text, the
/// standard plugin trick — is an armour stand with
/// [`SharedEntityFlags::invisible`] set, a custom name, `CustomNameVisible`
/// set, and usually [`marker`](Self::marker) plus
/// [`no_base_plate`](Self::no_base_plate) so nothing about the stand itself is
/// visible or interactable. Every one of those is a *conjunction*: decoding
/// only the shared-flags invisible bit and the custom-name pair (already wired
/// before this type existed) gives an invisible, nametagged stand that still
/// shows its base plate and, if `arms`/`small` were ever wired without this
/// byte, would desync from what the stand actually looked like — the
/// conjunction trap CLAUDE.md's evidence section warns about: implementing one
/// clause looks finished because the sibling clause it does implement is
/// genuinely correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArmorStandFlags {
    /// The raw byte value.
    pub bits: u8,
}

impl ArmorStandFlags {
    const SMALL: u8 = 0x01;
    const SHOW_ARMS: u8 = 0x04;
    const NO_BASEPLATE: u8 = 0x08;
    const MARKER: u8 = 0x10;

    /// Wraps a raw byte.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self { bits }
    }

    /// Whether the stand is small — `(flags & 1) != 0`. Halves the model scale and
    /// the arm/leg pose geometry vanilla applies.
    #[must_use]
    pub const fn small(self) -> bool {
        self.bits & Self::SMALL != 0
    }

    /// Whether the stand shows arms — `(flags & 4) != 0`. Without it a stand
    /// draws no arms at all, which is vanilla's own default (a bare torso/legs);
    /// this is the *other* `0x04` bit at this wire index, unrelated to
    /// [`MobFlags::aggressive`].
    #[must_use]
    pub const fn show_arms(self) -> bool {
        self.bits & Self::SHOW_ARMS != 0
    }

    /// Whether the base plate is hidden — `(flags & 8) != 0`. Named after the
    /// wire bit and the save-format tag that sets it (`"NoBasePlate"`), **not**
    /// after vanilla's own "show base plate" accessor, which reads the same bit
    /// inverted (`(flags & 8) == 0`); this method's polarity matches the bit, not
    /// that accessor's name.
    #[must_use]
    pub const fn no_base_plate(self) -> bool {
        self.bits & Self::NO_BASEPLATE != 0
    }

    /// Whether the stand is a marker — `(flags & 16) != 0`. A marker stand has no
    /// hitbox and ignores piston pushes; most "hologram" setups set this alongside
    /// [`SharedEntityFlags::invisible`] so nothing about the stand can be hit
    /// or collided with, only its floating name tag remains.
    #[must_use]
    pub const fn marker(self) -> bool {
        self.bits & Self::MARKER != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for a version crate's schema, with 26.2-like indices. It lives
    /// in the test, not the shared crate, which is the whole point.
    struct ModernSchema;
    impl MetadataSchema for ModernSchema {
        fn shared_flags_index(&self) -> u8 {
            0
        }
        fn air_supply_index(&self) -> Option<u8> {
            Some(1)
        }
    }

    /// A stand-in for a 1.8-family schema; flags still at 0 there but the point
    /// is the shared crate never assumed an index.
    struct LegacySchema;
    impl MetadataSchema for LegacySchema {
        fn shared_flags_index(&self) -> u8 {
            0
        }
    }

    #[test]
    fn flags_decode_via_schema() {
        let mut md = EntityMetadata::new();
        md.set(0, MetadataValue::Byte((0x08 | 0x01) as i8)); // sprinting + on fire
        let flags = md.shared_flags(&ModernSchema).unwrap();
        assert!(flags.sprinting());
        assert!(flags.on_fire());
        assert!(!flags.crouching());
    }

    #[test]
    fn same_bag_decodes_under_two_schemas() {
        let mut md = EntityMetadata::new();
        md.set(0, MetadataValue::Byte(0x40)); // glowing
        assert!(md.shared_flags(&ModernSchema).unwrap().glowing());
        assert!(md.shared_flags(&LegacySchema).unwrap().glowing());
    }

    #[test]
    fn cumulative_apply_and_override() {
        let mut md = EntityMetadata::new();
        md.set(5, MetadataValue::Int(10));
        md.set(5, MetadataValue::Int(20));
        assert_eq!(md.get(5).and_then(MetadataValue::as_int), Some(20));
        assert_eq!(md.len(), 1);
    }

    #[test]
    fn value_accessors() {
        assert_eq!(MetadataValue::Byte(3).as_int(), Some(3));
        assert_eq!(MetadataValue::Float(1.5).as_float(), Some(1.5));
        assert_eq!(MetadataValue::Boolean(true).as_bool(), Some(true));
        assert_eq!(MetadataValue::String("x".into()).as_int(), None);
    }

    #[test]
    fn living_flags_decode_each_bit_independently() {
        assert!(LivingEntityFlags::from_bits(0x01).using_item());
        assert!(!LivingEntityFlags::from_bits(0x00).using_item());
        // The offhand bit alone does not imply "using": vanilla sets both, and a
        // consumer must gate on `using_item`, not on the hand.
        assert!(!LivingEntityFlags::from_bits(0x02).using_item());
        assert_eq!(LivingEntityFlags::from_bits(0x01).used_hand(), UsedHand::Main);
        assert_eq!(LivingEntityFlags::from_bits(0x03).used_hand(), UsedHand::Off);
        assert!(LivingEntityFlags::from_bits(0x04).spin_attack());
        assert!(!LivingEntityFlags::from_bits(0x03).spin_attack());
    }

    /// The two flag bytes are different fields and must not share an accessor:
    /// bit 0 is `on fire` in one and `using item` in the other, so a byte read
    /// through the wrong type is silently, plausibly wrong.
    #[test]
    fn the_two_flag_bytes_disagree_on_bit_zero() {
        assert!(SharedEntityFlags::from_bits(0x01).on_fire());
        assert!(LivingEntityFlags::from_bits(0x01).using_item());
        // 0x02 is `crouching` in the shared byte and `offhand` in the living one.
        assert!(SharedEntityFlags::from_bits(0x02).crouching());
        assert_eq!(LivingEntityFlags::from_bits(0x02).used_hand(), UsedHand::Off);
    }

    #[test]
    fn mob_flags_decode_each_bit_independently() {
        assert!(MobFlags::from_bits(0x01).no_ai());
        assert!(MobFlags::from_bits(0x02).left_handed());
        assert!(MobFlags::from_bits(0x04).aggressive());
        assert!(!MobFlags::from_bits(0x03).aggressive());
        assert!(!MobFlags::from_bits(0x00).aggressive());
        // All three at once, so no accessor is secretly reading the whole byte.
        let all = MobFlags::from_bits(0x07);
        assert!(all.no_ai() && all.left_handed() && all.aggressive());
    }

    /// The *third* byte's turn at the same trap. `0x04` is `spin_attack` in the
    /// living byte and `aggressive` in the mob byte, and `0x01` means three
    /// different things across the three. A byte read through the wrong type is
    /// plausibly, silently wrong in every direction.
    #[test]
    fn all_three_flag_bytes_disagree_on_the_same_bits() {
        assert!(LivingEntityFlags::from_bits(0x04).spin_attack());
        assert!(MobFlags::from_bits(0x04).aggressive());
        // 0x01: on fire / using item / no AI.
        assert!(SharedEntityFlags::from_bits(0x01).on_fire());
        assert!(LivingEntityFlags::from_bits(0x01).using_item());
        assert!(MobFlags::from_bits(0x01).no_ai());
        // And the mob byte's aggressive bit is *not* the living byte's using-item
        // bit: a drawing skeleton sets
        // the former and never the former's namesake in the other byte.
        assert!(!LivingEntityFlags::from_bits(0x04).using_item());
        assert!(!MobFlags::from_bits(0x01).aggressive());
    }

    /// The fourth byte's turn at the same trap, and the collision that actually
    /// exists on the wire (not merely a hypothetical): `0x04` is `aggressive` in
    /// [`MobFlags`] and `showArms` in [`ArmorStandFlags`], both `BYTE` at
    /// metadata index 15. A decorative armour stand with arms shown must never
    /// read as an aggressive mob, and vice versa.
    #[test]
    fn mob_flags_and_armor_stand_flags_disagree_on_the_same_bit() {
        assert!(MobFlags::from_bits(0x04).aggressive());
        assert!(ArmorStandFlags::from_bits(0x04).show_arms());
        // Neither type's accessor for the *other* meaning fires off this byte —
        // `ArmorStandFlags` has no `aggressive()` and `MobFlags` has no
        // `show_arms()`, so the only way to conflate them is to decode through
        // the wrong type entirely, which is exactly what the version adapter's
        // `class`/`mob` guard exists to prevent.
        assert!(!ArmorStandFlags::from_bits(0x00).show_arms());
        assert!(!MobFlags::from_bits(0x00).aggressive());
    }

    #[test]
    fn armor_stand_flags_decode_each_bit_independently() {
        assert!(ArmorStandFlags::from_bits(0x01).small());
        assert!(ArmorStandFlags::from_bits(0x04).show_arms());
        assert!(ArmorStandFlags::from_bits(0x08).no_base_plate());
        assert!(ArmorStandFlags::from_bits(0x10).marker());
        assert!(!ArmorStandFlags::from_bits(0x00).small());
        assert!(!ArmorStandFlags::from_bits(0x00).show_arms());
        assert!(!ArmorStandFlags::from_bits(0x00).no_base_plate());
        assert!(!ArmorStandFlags::from_bits(0x00).marker());
        // All four at once (0x1D = small | show_arms | no_base_plate | marker,
        // deliberately skipping bit 0x02 which vanilla leaves unused at this
        // index), so no accessor is secretly reading the whole byte or another
        // bit.
        let all = ArmorStandFlags::from_bits(0x1D);
        assert!(all.small() && all.show_arms() && all.no_base_plate() && all.marker());
    }

    /// The typical "hologram" configuration: invisible (a *different* byte,
    /// [`SharedEntityFlags`], covered by its own test) plus marker and
    /// no-base-plate on this byte, arms and small left off. Values are
    /// deliberately pairwise-distinct-shaped (only two of four bits set, not
    /// all-or-nothing) so a transposition with [`MobFlags`]'s bits cannot
    /// survive unnoticed.
    #[test]
    fn a_typical_hologram_stand_sets_marker_and_no_base_plate_only() {
        let hologram = ArmorStandFlags::from_bits(0x18); // marker | no_base_plate
        assert!(hologram.marker());
        assert!(hologram.no_base_plate());
        assert!(!hologram.small());
        assert!(!hologram.show_arms());
    }

    #[test]
    fn iteration_is_index_ordered() {
        let mut md = EntityMetadata::new();
        md.set(9, MetadataValue::Int(9));
        md.set(1, MetadataValue::Int(1));
        md.set(4, MetadataValue::Int(4));
        let idx: Vec<u8> = md.iter().map(|(i, _)| i).collect();
        assert_eq!(idx, vec![1, 4, 9]);
    }
}
