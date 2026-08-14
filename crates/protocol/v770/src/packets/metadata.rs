//! The protocol 776 (Minecraft 26.2) entity-metadata and attribute wire formats.
//!
//! # Why this lives in the version crate
//!
//! Entity metadata is the most version-divergent surface in the protocol. Two
//! separate version-specific tables govern it, and both belong here rather than
//! in any shared crate:
//!
//! * **The serializer table.** Each metadata value is tagged with a *serializer
//!   type id* — an index into vanilla's `EntityDataSerializers` registration
//!   order. 26.2 has 43 of them (0..=42); the order and set change every couple
//!   of releases. The wire carries no per-value length, so a decoder must know
//!   each serializer's exact byte shape: a single mis-sized value silently
//!   desyncs the rest of the list. That is exactly why the caller asserts zero
//!   trailing bytes — a misparse leaves the reader misaligned and the trailing
//!   check (or a bogus follow-on index) catches it.
//! * **The index table.** Which *index* a semantic field (health, custom name,
//!   baby, …) sits at is assigned by vanilla's class hierarchy
//!   (`Entity` → `LivingEntity` → `Mob` → `AgeableMob` → …). Those indices are
//!   26.2-specific and are resolved here into the version-free
//!   [`EntityMetadataUpdate`] the rest of the client consumes.
//!
//! # Robustness vs. the desync detector
//!
//! Packet framing bounds a metadata payload, so a misparse cannot corrupt the
//! TCP stream — it is contained to one packet. The decoder therefore returns a
//! hard error on anything it cannot byte-accurately consume (an unknown
//! serializer, a genuinely complex one it does not model, or a truncated value),
//! and the adapter treats that as "emit nothing for this packet" rather than
//! killing the connection. In tests the same error surfaces as a failed decode,
//! which is the misparse detector doing its job.
//!
//! A couple of serializers carry genuinely complex, self-describing payloads
//! (particles, resolvable profiles) that mobs never emit in practice, so they
//! are deliberately *not* modelled: they decode to an explicit error rather than
//! a guess.
//!
//! # The item-stack serializer, and the one place alignment is given up
//!
//! `ITEM_STACK` used to be in that rejected set, on the reasoning that mobs
//! never emit it. True of mobs — and false of the entity whose entire identity
//! it is: a dropped `minecraft:item` carries its stack under this serializer and
//! nothing else. Rejecting it meant every dropped item reached the client with
//! no idea what it was.
//!
//! It is decoded here by delegating to the adapter's existing clientbound
//! item-stack codec ([`crate::adapter::read_item_stack`]), which already models
//! 26.2's `DataComponentPatch` and already degrades correctly on a component it
//! does not model. That degradation is *load-bearing* and interacts with this
//! module's stream shape:
//!
//! * A clientbound component patch length-prefixes neither the patch nor its
//!   individual components, so an unmodeled component cannot be skipped in
//!   place. The item codec therefore stops there, keeps the item key, count, and
//!   whatever components it already read, and reports `complete == false` with
//!   the reader parked mid-payload.
//! * Metadata, unlike the container packets that codec was written for, is a
//!   *stream of indexed fields terminated by a `0xFF` sentinel*. A reader parked
//!   mid-payload cannot resume: every following byte would decode as a plausible
//!   but wrong `(index, serializer, value)` triple — garbage that never fails
//!   loudly. Scanning ahead for the sentinel is no better, since `0xFF` occurs
//!   freely inside payload bytes.
//! * So this decoder **abandons the remainder of the list** at that point and
//!   returns what it has, flagged [`DecodedMetadata::complete`] `== false`. The
//!   caller must then skip its trailing-bytes assertion (there deliberately are
//!   trailing bytes) and still emit the update.
//!
//! Abandoning is safe because metadata is applied *incrementally*: an update
//! carrying a subset of fields is the normal case, not an error case, and every
//! field it does carry was consumed byte-accurately before the abandonment
//! point. The alternative — dropping the packet outright — would throw away the
//! item identity that was already decoded, which is precisely the fail-closed
//! behaviour this seam exists to remove.

use lodestone_core::{Error, Reader, Result, plain_text_from_nbt_component, read_network_nbt};
use lodestone_model::{
    EntityAttributeModifier, EntityAttributeSnapshot, EntityMetadataUpdate, EntityPose,
    EntityVariant, Identifier, ItemStack, Reported,
};

use lodestone_data::attribute_types::attribute_name;
use crate::entity_variants;

/// Sentinel index terminating a metadata list.
const EOF_MARKER: u8 = 255;
/// Vanilla's string length cap.
const MAX_STRING: usize = 32_767;
/// Vanilla caps an `update_attributes` list at 128 entries.
const MAX_ATTRIBUTES: usize = 128;

// --- 26.2 metadata index constants (class-hierarchy assignment order) --------
// Entity: 0 shared-flags, 1 air, 2 custom-name, 3 custom-name-visible,
// 4 silent, 5 no-gravity, 6 pose, 7 ticks-frozen.
// LivingEntity: 8 living-flags, 9 health, 10 effect-particles, 11 effect-
// ambience, 12 arrow-count, 13 stinger-count, 14 sleeping-pos.
// Mob: 15 mob-flags. AgeableMob: 16 baby.
const IDX_SHARED_FLAGS: u8 = 0;
// `Entity`'s second `defineId` call (`Entity.java:268`, right after
// `DATA_SHARED_FLAGS_ID` at :260 and right before `DATA_CUSTOM_NAME` at :269)
// — `SynchedEntityData.defineId` assigns ids by a class-static counter in
// declaration order, so this is index 1, verified against the jar's own
// source rather than trusted from a briefing.
const IDX_AIR_SUPPLY: u8 = 1;
const IDX_CUSTOM_NAME: u8 = 2;
const IDX_CUSTOM_NAME_VISIBLE: u8 = 3;
const IDX_POSE: u8 = 6;
/// `LivingEntity.DATA_LIVING_ENTITY_FLAGS` (`LivingEntity.java:179`), the first
/// `defineId` in `LivingEntity` and therefore index 8 — the byte carrying
/// using-item / off-hand / spin-attack (issue #57).
///
/// **This index is ambiguous and needs the entity's concrete type.** It is also
/// where `AbstractArrow.ID_FLAGS` lands (`AbstractArrow.java:66`; `Projectile`
/// declares no synched data of its own, so the arrow's first field is index 8
/// too), and both are `EntityDataSerializers.BYTE`. So the serializer cannot
/// disambiguate them the way it does for an item stack, and an arrow's crit bit
/// (`0x01`) is bit-identical to the using-item bit. Only surfaced when the caller
/// says the entity is a `LivingEntity`; see `read_entity_metadata`'s `living`
/// parameter. Index 8 is *also* the item stack on a dropped item and on thrown
/// projectiles, but that one does self-identify by serializer and is handled
/// before the index match.
const IDX_LIVING_FLAGS: u8 = 8;
/// `ExperienceOrb.DATA_VALUE`, `ExperienceOrb`'s only `defineId` and therefore
/// index 8 — an `INT`, how much XP *one* absorption of this orb pays. The client
/// needs it for nothing but the sprite: `ExperienceOrbRenderer.extractRenderState`
/// reads `entity.getIcon()`, and `getIcon` is a **bucketed** lookup on this value
/// (see `lodestone_render::entity::experience_orb_icon`).
///
/// # A third claimant on index 8, and the serializer cannot separate it either
///
/// Index 8 already carries two ambiguities this module resolves ([`IDX_LIVING_FLAGS`]'s
/// `BYTE` pair, and the self-identifying `ITEM_STACK`). This is a *third*, and the
/// jar dump (`tests/support/entity_data_index_jvm.txt`) lists five `INT` claimants
/// at index 8: `ExperienceOrb.DATA_VALUE`, `PrimedTnt.DATA_FUSE_ID`,
/// `FishingHook.DATA_HOOKED_ENTITY`, `VehicleEntity.DATA_ID_HURT` and
/// `Display.DATA_TRANSFORMATION_INTERPOLATION_START_DELTA_TICKS_ID`. All five are
/// `EntityDataSerializers.INT`, so — exactly as for the byte pair — the serializer
/// tells you nothing and only the concrete entity type does. Hence the
/// [`MetadataClass::ExperienceOrb`] guard: ungated, a primed TNT's fuse countdown
/// would arrive as an orb value and pick an orb sprite for it.
///
/// `living` is **not** a usable guard here and neither is `mob`: an orb is neither,
/// so both are `false` for it, and every other claimant is non-living too. The
/// class is the only thing that separates them.
const IDX_EXPERIENCE_ORB_VALUE: u8 = 8;
const IDX_HEALTH: u8 = 9;
/// `Mob.DATA_MOB_FLAGS_ID` (`Mob.java:100`), `Mob`'s **only** `defineId` and
/// therefore index 15 — the byte carrying no-AI `0x01` / left-handed `0x02` /
/// **aggressive `0x04`** (`Mob.java:1313-1336`). Aggressive is what makes a
/// skeleton draw its bow: vanilla's mob renderers read `isAggressive()`, *not*
/// the using-item bit at index 8, which is a player mechanism (issue #379).
///
/// # This index is ambiguous too, and `living` is **not** a strong enough guard
///
/// The jar dump (`tests/support/entity_data_index_jvm.txt`) reports three
/// claimants on index 15, all `EntityDataSerializers.BYTE`:
///
/// | owner | field | `0x04` |
/// |---|---|---|
/// | `Mob` | `DATA_MOB_FLAGS_ID` | aggressive |
/// | `ArmorStand` | `DATA_CLIENT_FLAGS` | show arms |
/// | `Display` | `DATA_BILLBOARD_RENDER_CONSTRAINTS_ID` | an enum ordinal |
///
/// Index 8's collision was between a living entity and a non-living one, so
/// `is_living` resolved it. **`ArmorStand` is a `LivingEntity`**, so the same
/// guard would let a decorative armour stand with arms shown report itself as an
/// aggressive mob — and, holding a bow, draw it. This byte is therefore gated on
/// `entity_census::is_mob`, a strictly narrower census column, resolved from the
/// concrete type at `add_entity` exactly as `living` is. See
/// [`TrackedEntity::mob`].
const IDX_MOB_FLAGS: u8 = 15;
const IDX_BABY: u8 = 16;
// Class-specific indices that alias the same numbers across mobs, so they are
// only meaningful once the entity's concrete type is known (see `MetadataClass`).
//
// **Both of these were off by one until the jar was asked.** They were counted
// by hand as "Sheep's first field, so 17" and "AbstractHorse's flags at 17, so
// the variant int is 18", and the count missed `AgeableMob.AGE_LOCKED` — a
// second accessor on `AgeableMob`, at index 17, right after `DATA_BABY_ID` at 16.
// The real values, from `tests/support/entity_data_index_jvm.txt`:
// `Sheep.DATA_WOOL_ID` is **18** and `Horse.DATA_ID_TYPE_VARIANT` is **19**
// (`AbstractHorse.DATA_ID_FLAGS` occupies 18).
//
// Nothing caught it, and the reason is instructive: the encoders in this
// module's tests push the *same constants*, so `decode(encode(x)) == x` held
// perfectly (`CLAUDE.md`: "two symmetric misunderstandings"), and every sheep
// pixel gate builds an `EntityDraw` directly, downstream of the wire. The live
// symptom was silent — a `BOOLEAN` arrives at 17 where a `Byte` arm waits, so no
// arm matches, `variant` stays `None`, and the decode reports a clean parse.
//
// The player-visible result was **no wool at all**, not a wrong colour. This is
// worth stating precisely because the first write-up of this fix said "renders the
// type's default colour", and that is not what the chain does: `variant: None`
// makes `entities::sheep_wool` return `None` (it matches only
// `EntityVariant::Dyed`), so `EntityDraw::wool` is `None` and
// `RenderState::prepare_wool` emits no batch. A bare, wool-less sheep — which is
// how the user reported it, and the reason the misdescription mattered is that
// "wrong colour" would have sent the next person looking at the tint table
// instead of the wire.
// `every_metadata_index_constant_matches_the_jar_dump` is the anchor that now
// makes such a count checkable.
const IDX_SHEEP_WOOL: u8 = 18;
const IDX_HORSE_VARIANT: u8 = 19;
/// Index 18's other `BYTE` claimants (besides [`IDX_SHEEP_WOOL`]'s Sheep and the
/// creeper's `BOOLEAN` at the same index): `TamableAnimal.DATA_FLAGS_ID` and
/// `AbstractHorse.DATA_ID_FLAGS`. The bit differs between the two —
/// `TamableAnimal.isTame()` is `0x04`, `AbstractHorse.isTamed()` is `0x02` —
/// which is why the decode arm below switches on [`MetadataClass::Tamable`]
/// vs. [`MetadataClass::Horse`] rather than reading one shared "tamed" bit;
/// see `crates/lodestone-server/src/protocol.rs`'s `MetadataField::TamableFlags`/
/// `HorseFlags` doc comment for the server-side encode side of the same split.
const IDX_TAMABLE_OR_HORSE_FLAGS: u8 = 18;

/// `Creeper.DATA_SWELL_DIR` (`Creeper.java:46`), `Creeper`'s first `defineId`
/// and therefore index 16 — `Monster` (its superclass) declares none of its
/// own, so the count runs `Entity`(0-7) → `LivingEntity`(8-14) → `Mob`(15) →
/// `Creeper`(16-18) directly, with no `AgeableMob` in between (a creeper is
/// not ageable). Verified against `tests/support/entity_data_index_jvm.txt`,
/// not hand-counted — see that file's own warning about what hand-counting
/// this exact shape (a class with no `Ageable` in its chain) has cost before.
///
/// An `INT`, `-1` or `1`: which way `swell` is currently moving, integrated
/// **client-side** every tick exactly as the server does (`Creeper.java:139`,
/// `this.swell += swellDir`) — only the direction is synced, never the
/// counter itself. See [`crate::adapter`]'s per-tick fuse integration and
/// `lodestone_render::entity_anim::pose_swelling`'s docs for why that split
/// exists.
const IDX_CREEPER_SWELL_DIR: u8 = 16;
/// `Creeper.DATA_IS_POWERED` (`Creeper.java:47`), index 17 — a `BOOLEAN`, set
/// once by `thunderHit` (`Creeper.java:206`) and never cleared. Doubles the
/// explosion radius (`Creeper.java:232`) and gates the charged-creeper skull
/// drop; not consumed by rendering yet.
const IDX_CREEPER_POWERED: u8 = 17;
/// `Creeper.DATA_IS_IGNITED` (`Creeper.java:48`), index 18 — a `BOOLEAN`, set
/// once by `ignite()` (flint-and-steel or fire-charge, `Creeper.java:264`) and
/// never cleared. Distinct from a **non**-ignited swell (the `SwellGoal`
/// proximity case, which moves `swell_dir` without ever setting this):
/// `ignited` alone would miss a creeper that swells because a player got
/// close and then backs off before detonation, since that path only ever
/// touches `DATA_SWELL_DIR`.
const IDX_CREEPER_IGNITED: u8 = 18;
// Both of index 18's claimants share nothing (`BYTE` vs `BOOLEAN`), so the
// serializer alone tells them apart at decode time — but see the module's
// `decode_value` for why the *index* still needs a class guard: a mob this
// seam does not model could in principle also declare a `BOOLEAN` at 18.
//
// This is a standalone note, not documentation of `MetadataClass` below —
// hence `//` rather than `///`, which is what the empty line before that
// enum's own doc comment used to trip clippy's dangling-doc-comment lint on.

/// The mobs whose cosmetic variant sits at an index that other mobs reuse for an
/// unrelated field, so the raiser can only read it when the concrete entity type
/// is known. Registry-holder variants (cat, cow, …) are self-identifying by
/// serializer and need no entry here.
///
/// [`Creeper`](Self::Creeper) is here for the same structural reason, not a
/// cosmetic variant: indices 16-18 are `Creeper`'s own fields, and every one of
/// them is claimed by several *other* mobs' unrelated `INT`/`BOOLEAN` fields at
/// the same index (`Display.DATA_BRIGHTNESS_OVERRIDE_ID`, `EnderDragon.DATA_PHASE`
/// and `Warden.CLIENT_ANGER_LEVEL` are all `INT` at 16; `EnderMan.DATA_CREEPY` and
/// `Witch.DATA_USING_ITEM` are both `BOOLEAN` at 17 — see
/// `tests/support/entity_data_index_jvm.txt`). Without this guard a warden's
/// anger level would decode as a creeper's swell direction on any client that
/// also tracks wardens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataClass {
    Sheep,
    /// Any `AbstractHorse` subclass — `Horse`, `Donkey`, `Mule`, `Llama`,
    /// `TraderLlama`, `SkeletonHorse`, `ZombieHorse`, `Camel` — not just plain
    /// `Horse`. It gates two unrelated `AbstractHorse`-family fields that
    /// happen to sit at different indices:
    ///
    /// * index 19's `INT` (`Horse.DATA_ID_TYPE_VARIANT`, colour + markings) —
    ///   only `Horse` itself ever emits an `INT` there, so the [`Value::Int`]
    ///   pattern on that arm already excludes every other equine even though
    ///   they share this class.
    /// * index 18's `BYTE` (`AbstractHorse.DATA_ID_FLAGS`, `FLAG_TAME = 0x02`)
    ///   — genuinely shared by the whole family, which is why the class covers
    ///   all of it rather than just `Horse`.
    Horse,
    Creeper,
    /// Not a cosmetic variant either: `ExperienceOrb.DATA_VALUE` is an `INT` at
    /// index 8, an index five unrelated `INT` fields also claim. See
    /// [`IDX_EXPERIENCE_ORB_VALUE`].
    ExperienceOrb,
    /// A `TamableAnimal` subclass — `Wolf`, `Cat`, `Parrot` (via
    /// `ShoulderRidingEntity`), `Nautilus`/`ZombieNautilus` (via
    /// `AbstractNautilus`) — the other `BYTE` claimant of index 18
    /// (`TamableAnimal.DATA_FLAGS_ID`: `isTame` is `0x04`, `isInSittingPose`
    /// is `0x01`). A **different** bit from [`Horse`](Self::Horse)'s
    /// `FLAG_TAME = 0x02` at the same index — see [`IDX_TAMABLE_OR_HORSE_FLAGS`]
    /// for why a single shared "tamed" field would misread one family or the
    /// other.
    Tamable,
}

/// Classifies a resolved entity-type identifier into the [`MetadataClass`] whose
/// ambiguous variant index the raiser must disambiguate. Every other type yields
/// `None`; its self-identifying variants (if any) still resolve by serializer.
pub fn metadata_class(entity_type: &str) -> Option<MetadataClass> {
    match entity_type {
        "minecraft:sheep" => Some(MetadataClass::Sheep),
        "minecraft:horse"
        | "minecraft:donkey"
        | "minecraft:mule"
        | "minecraft:llama"
        | "minecraft:trader_llama"
        | "minecraft:skeleton_horse"
        | "minecraft:zombie_horse"
        | "minecraft:camel" => Some(MetadataClass::Horse),
        "minecraft:creeper" => Some(MetadataClass::Creeper),
        "minecraft:experience_orb" => Some(MetadataClass::ExperienceOrb),
        "minecraft:wolf" | "minecraft:cat" | "minecraft:parrot" | "minecraft:nautilus"
        | "minecraft:zombie_nautilus" => Some(MetadataClass::Tamable),
        _ => None,
    }
}

/// What the adapter remembers about a spawned entity so a later
/// `set_entity_data` can resolve its ambiguous metadata indices.
///
/// Two independent disambiguations, deliberately in one record because they are
/// both "facts about the concrete type that the metadata packet does not carry":
///
/// * [`class`](Self::class) — the sheep/horse variant indices (17/18), which other
///   mobs reuse for unrelated fields.
/// * [`living`](Self::living) — whether index 8's byte is
///   `LivingEntity.DATA_LIVING_ENTITY_FLAGS` or `AbstractArrow.ID_FLAGS`.
/// * [`mob`](Self::mob) — whether index 15's byte is `Mob.DATA_MOB_FLAGS_ID` or
///   `ArmorStand.DATA_CLIENT_FLAGS`.
///
/// # Why this does not grow the tracked set to every entity
///
/// [`is_tracked`](Self::is_tracked) is the insert gate, and it is false for a
/// record that carries neither fact — so arrows, dropped items, display entities,
/// boats and every other non-living type with no ambiguous variant stay out of the
/// map exactly as they did when it held bare `MetadataClass`es. The population it
/// adds is the living entities, which is the population whose flags we want.
///
/// # Why the whole record is passed to [`read_entity_metadata`]
///
/// It used to take `class` and `living` as separate positional arguments. Every
/// fact here is of one kind — "something about the concrete type that the
/// metadata packet does not carry" — and adding a third such fact should not
/// re-thread a signature through the adapter, which several agents share. So the
/// decoder takes the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrackedEntity {
    /// The ambiguous-variant class, if this type has one.
    pub class: Option<MetadataClass>,
    /// Whether this type is a vanilla `LivingEntity`.
    pub living: bool,
    /// Whether this type is a vanilla `Mob` — strictly narrower than
    /// [`living`](Self::living), and the guard on index 15. A `Player`, an
    /// `ArmorStand` and a `Mannequin` are living and **not** mobs, and the armour
    /// stand's own index-15 byte is a `BYTE` whose `0x04` means "show arms". See
    /// [`IDX_MOB_FLAGS`].
    pub mob: bool,
}

impl TrackedEntity {
    /// Whether this record says anything, and so is worth an entry in the map.
    ///
    /// `mob` is deliberately **not** a term here: every mob is living, so a
    /// `mob: true, living: false` record cannot occur, and adding the disjunct
    /// would be dead code that reads as a real condition. Asserted by
    /// `a_mob_is_tracked_through_the_living_disjunct`.
    #[must_use]
    pub const fn is_tracked(self) -> bool {
        self.class.is_some() || self.living
    }
}

// --- 26.2 serializer type ids (EntityDataSerializers registration order) -----
const SER_BYTE: i32 = 0;
const SER_INT: i32 = 1;
const SER_LONG: i32 = 2;
const SER_FLOAT: i32 = 3;
const SER_STRING: i32 = 4;
const SER_COMPONENT: i32 = 5;
const SER_OPTIONAL_COMPONENT: i32 = 6;
const SER_ITEM_STACK: i32 = 7;
const SER_BOOLEAN: i32 = 8;
const SER_ROTATIONS: i32 = 9;
const SER_BLOCK_POS: i32 = 10;
const SER_OPTIONAL_BLOCK_POS: i32 = 11;
const SER_DIRECTION: i32 = 12;
const SER_OPTIONAL_LIVING_ENTITY_REFERENCE: i32 = 13;
const SER_BLOCK_STATE: i32 = 14;
const SER_OPTIONAL_BLOCK_STATE: i32 = 15;
const SER_PARTICLE: i32 = 16;
const SER_PARTICLES: i32 = 17;
const SER_VILLAGER_DATA: i32 = 18;
const SER_OPTIONAL_UNSIGNED_INT: i32 = 19;
const SER_POSE: i32 = 20;
const SER_OPTIONAL_GLOBAL_POS: i32 = 33;
const SER_VECTOR3: i32 = 39;
const SER_QUATERNION: i32 = 40;
const SER_RESOLVABLE_PROFILE: i32 = 41;
const SER_HUMANOID_ARM: i32 = 42;

/// A decoded metadata value in the small set of shapes this seam surfaces.
///
/// Serializers that are consumed byte-accurately but carry no field we expose
/// decode to [`Value::Consumed`]; that keeps the list aligned without inventing
/// a version-free representation for every value shape in the game.
enum Value {
    Byte(i8),
    /// A signed VarInt (surfaced for the horse variant packing).
    Int(i32),
    Float(f32),
    Bool(bool),
    /// An optional text component (used by custom name). Inner `None` = cleared.
    OptText(Option<String>),
    /// A pose enum id.
    Pose(u32),
    /// A resolved registry-holder appearance variant (cat, cow, wolf, …).
    Keyed(Identifier),
    /// A resolved villager type/profession/level composite.
    Villager {
        kind: Identifier,
        profession: Identifier,
        level: i32,
    },
    /// A decoded item stack; inner `None` is the empty stack.
    ///
    /// The only value whose decode can legitimately stop part-way: `complete`
    /// is `false` when an unmodeled data component left the reader parked
    /// mid-payload, which ends the whole list (see the module header).
    Item {
        // Boxed: `ItemStack` carries `ItemComponents`, which alone pushes this
        // variant past clippy's large-enum-variant threshold and every other
        // `Value` variant is a handful of bytes. Boxing this one field keeps
        // the enum small without touching the version-free `ItemStack` shape
        // itself.
        stack: Option<Box<ItemStack>>,
        complete: bool,
    },
    /// Consumed correctly but not surfaced.
    Consumed,
}

/// Outcome of decoding one `set_entity_data` metadata list.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedMetadata {
    /// The fields decoded before the list ended.
    pub metadata: EntityMetadataUpdate,
    /// `false` when an unmodeled item data component halted decoding mid-value,
    /// leaving the reader parked inside that payload and the remaining fields
    /// deliberately abandoned.
    ///
    /// A caller must **not** run its trailing-bytes assertion when this is
    /// `false` — there are trailing bytes by construction — and must still emit
    /// the update, which is partial in exactly the way every metadata packet is
    /// allowed to be.
    pub complete: bool,
}

/// Maps a 26.2 `Pose` enum id to the version-free [`EntityPose`]. Ids the shared
/// set does not name travel as [`EntityPose::Other`].
fn pose_from_id(id: u32) -> EntityPose {
    match id {
        0 => EntityPose::Standing,
        1 => EntityPose::FallFlying,
        2 => EntityPose::Sleeping,
        3 => EntityPose::Swimming,
        4 => EntityPose::SpinAttack,
        5 => EntityPose::Crouching,
        6 => EntityPose::LongJumping,
        7 => EntityPose::Dying,
        10 => EntityPose::Sitting,
        other => EntityPose::Other(other),
    }
}

fn unknown_serializer(id: i32) -> Error {
    Error::InvalidEnumVariant {
        name: "v770 entity-data serializer",
        value: id,
    }
}

/// Consumes exactly one metadata value of the given serializer type, returning
/// the semantic [`Value`] when it is one this seam models.
///
/// Every branch reads precisely the bytes vanilla's codec writes; that byte
/// accuracy is what keeps the surrounding list aligned. Complex, self-describing
/// serializers (item stacks, particles, profiles) are rejected explicitly rather
/// than skipped by guesswork.
fn decode_value(reader: &mut Reader<'_>, serializer: i32) -> Result<Value> {
    let value = match serializer {
        SER_BYTE => Value::Byte(reader.i8()?),
        SER_INT => Value::Int(reader.var_i32()?),
        SER_LONG => {
            reader.var_i64()?;
            Value::Consumed
        }
        SER_FLOAT => Value::Float(reader.f32()?),
        SER_STRING => {
            reader.string(MAX_STRING)?;
            Value::Consumed
        }
        SER_COMPONENT => {
            read_network_nbt(reader)?;
            Value::Consumed
        }
        SER_OPTIONAL_COMPONENT => {
            if reader.bool()? {
                let component = read_network_nbt(reader)?;
                Value::OptText(Some(plain_text_from_nbt_component(&component)))
            } else {
                Value::OptText(None)
            }
        }
        SER_BOOLEAN => Value::Bool(reader.bool()?),
        SER_ROTATIONS => {
            reader.f32()?;
            reader.f32()?;
            reader.f32()?;
            Value::Consumed
        }
        SER_BLOCK_POS => {
            reader.i64()?;
            Value::Consumed
        }
        SER_OPTIONAL_BLOCK_POS => {
            if reader.bool()? {
                reader.i64()?;
            }
            Value::Consumed
        }
        SER_DIRECTION
        | SER_BLOCK_STATE
        | SER_OPTIONAL_BLOCK_STATE
        | SER_OPTIONAL_UNSIGNED_INT
        | SER_HUMANOID_ARM => {
            reader.var_i32()?;
            Value::Consumed
        }
        SER_OPTIONAL_LIVING_ENTITY_REFERENCE => {
            if reader.bool()? {
                reader.uuid()?;
            }
            Value::Consumed
        }
        SER_VILLAGER_DATA => {
            // holderRegistry(type) + holderRegistry(profession) + VarInt level.
            // Each holder is a registry id written as `id + 1` (0 = inline direct,
            // which vanilla never sends for villagers).
            let type_id = reader.var_i32()? - 1;
            let profession_id = reader.var_i32()? - 1;
            let level = reader.var_i32()?;
            match (
                entity_variants::villager_type(type_id),
                entity_variants::villager_profession(profession_id),
            ) {
                (Some(kind), Some(profession)) => Value::Villager {
                    kind: parse_identifier(kind)?,
                    profession: parse_identifier(profession)?,
                    level,
                },
                // An unmapped datapack id: stay aligned, raise no variant.
                _ => Value::Consumed,
            }
        }
        SER_POSE => Value::Pose(reader.var_i32()?.max(0) as u32),
        // Appearance variants are `Holder<Variant>` registry references (wire is
        // `id + 1`; 0 = inline direct, never sent for mobs). Resolve the ones that
        // name an appearance to a canonical key; the interleaved sound-variant and
        // enum-state serializers in this range carry no field we surface.
        21 | 23 | 25 | 27 | 28 | 30 | 32 => {
            let id = reader.var_i32()? - 1;
            match entity_variants::appearance_variant(serializer, id) {
                Some(key) => Value::Keyed(parse_identifier(key)?),
                None => Value::Consumed,
            }
        }
        22 | 24 | 26 | 29 | 31 | 34..=38 => {
            reader.var_i32()?;
            Value::Consumed
        }
        SER_OPTIONAL_GLOBAL_POS => {
            if reader.bool()? {
                reader.string(MAX_STRING)?; // dimension resource key
                reader.i64()?; // packed block position
            }
            Value::Consumed
        }
        SER_VECTOR3 => {
            reader.f32()?;
            reader.f32()?;
            reader.f32()?;
            Value::Consumed
        }
        SER_QUATERNION => {
            reader.f32()?;
            reader.f32()?;
            reader.f32()?;
            reader.f32()?;
            Value::Consumed
        }
        // A dropped item's entire identity. Delegated to the adapter's single
        // clientbound item-stack codec — never re-implemented here — so both
        // paths share one reading of the `DataComponentPatch` wire. An
        // unmodeled component yields a partial stack with `complete == false`
        // rather than an error; the caller ends the list there.
        SER_ITEM_STACK => {
            let decoded = crate::adapter::read_item_stack(reader)
                .map_err(|err| Error::Custom(err.to_string()))?;
            match decoded {
                crate::adapter::DecodedStack::Complete(stack) => Value::Item {
                    stack: stack.map(Box::new),
                    complete: true,
                },
                crate::adapter::DecodedStack::Partial(stack) => Value::Item {
                    stack: stack.map(Box::new),
                    complete: false,
                },
            }
        }
        // Genuinely complex, self-describing payloads mobs never emit. Rejected
        // rather than guessed at.
        SER_PARTICLE | SER_PARTICLES | SER_RESOLVABLE_PROFILE => {
            return Err(unknown_serializer(serializer));
        }
        other => return Err(unknown_serializer(other)),
    };
    Ok(value)
}

/// Decodes a `set_entity_data` metadata list into a version-free
/// [`EntityMetadataUpdate`], resolving 26.2's indices and serializers.
///
/// On a complete decode the reader is left positioned immediately after the
/// `0xFF` terminator and the caller asserts the payload is then empty (the
/// misparse detector). When the result reports `complete == false` an unmodeled
/// item component ended the list early, the reader is parked mid-payload, and
/// that assertion must be skipped — see the module header for why resuming is
/// not an option.
pub fn read_entity_metadata(
    reader: &mut Reader<'_>,
    tracked: TrackedEntity,
) -> Result<DecodedMetadata> {
    let TrackedEntity { class, living, mob } = tracked;
    let mut md = EntityMetadataUpdate::default();
    loop {
        let index = reader.u8()?;
        if index == EOF_MARKER {
            break;
        }
        let serializer = reader.var_i32()?;
        let value = decode_value(reader, serializer)?;
        // An item stack identifies itself by serializer, so — like the
        // registry-holder variants below — the index it arrives at is
        // irrelevant. (It is 8 on a dropped item and an item frame, and 8 on
        // thrown projectiles too, but nothing needs to know that.)
        if let Value::Item { stack, complete } = value {
            md.item = Reported::Reported(stack.map(|boxed| *boxed));
            if !complete {
                // The reader is parked inside an unmodeled component's payload.
                // Every following byte would decode as a plausible-but-wrong
                // field, so the rest of the list is abandoned rather than
                // guessed at. What was decoded before this point is exact.
                return Ok(DecodedMetadata {
                    metadata: md,
                    complete: false,
                });
            }
            continue;
        }
        match (index, value) {
            (IDX_SHARED_FLAGS, Value::Byte(b)) => md.flags = Some(b as u8),
            // Gated on `living`, not merely decoded: see `IDX_LIVING_FLAGS`. A
            // non-living entity's index-8 byte is consumed for alignment by the
            // `_ => {}` arm below and deliberately not surfaced, so a critical
            // arrow never reports itself as drawing a bow.
            (IDX_LIVING_FLAGS, Value::Byte(b)) if living => md.living_flags = Some(b as u8),
            // Gated on `mob`, which is narrower than `living` and has to be:
            // `ArmorStand` is a living entity whose own index-15 `BYTE` sets
            // `0x04` for "show arms". See `IDX_MOB_FLAGS`. Ungated, every armour
            // stand with arms would report itself aggressive.
            (IDX_MOB_FLAGS, Value::Byte(b)) if mob => md.mob_flags = Some(b as u8),
            // Gated on the class for the reason [`IDX_EXPERIENCE_ORB_VALUE`] gives:
            // four other entity types put an unrelated `INT` at this index, and a
            // primed TNT's fuse countdown reaching an orb sprite is exactly the
            // silent-wrong-value failure the byte pair above already documents.
            (IDX_EXPERIENCE_ORB_VALUE, Value::Int(v))
                if class == Some(MetadataClass::ExperienceOrb) =>
            {
                md.experience_orb_value = Some(v);
            }
            (IDX_AIR_SUPPLY, Value::Int(v)) => md.air_supply = Some(v),
            (IDX_CUSTOM_NAME, Value::OptText(t)) => md.custom_name = Reported::Reported(t),
            (IDX_CUSTOM_NAME_VISIBLE, Value::Bool(b)) => md.custom_name_visible = Some(b),
            (IDX_POSE, Value::Pose(p)) => md.pose = Some(pose_from_id(p)),
            (IDX_HEALTH, Value::Float(f)) => md.health = Some(f),
            (IDX_BABY, Value::Bool(b)) => md.baby = Some(b),
            // Sheep pack wool colour and the sheared flag into one byte; only a
            // sheep uses index 18 for a byte, hence the class guard.
            (IDX_SHEEP_WOOL, Value::Byte(b)) if class == Some(MetadataClass::Sheep) => {
                md.variant = Some(EntityVariant::Dyed {
                    color: (b as u8) & 0x0F,
                    sheared: (b as u8) & 0x10 != 0,
                });
            }
            // Horse packs colour (low byte) and markings (next byte) into an int.
            (IDX_HORSE_VARIANT, Value::Int(v)) if class == Some(MetadataClass::Horse) => {
                md.variant = Some(EntityVariant::Horse {
                    color: (v & 0xFF) as u8,
                    markings: ((v >> 8) & 0xFF) as u8,
                });
            }
            // A creeper's fuse direction (-1 idle/retreating, 1 counting up to
            // detonation). Guarded on class: index 16 is an `INT` on several other
            // mobs too (`Display`, `EnderDragon`, `Phantom`, `Warden`, `WitherBoss`
            // — see `IDX_CREEPER_SWELL_DIR`'s doc), none of which mean a creeper's
            // swell.
            (IDX_CREEPER_SWELL_DIR, Value::Int(v)) if class == Some(MetadataClass::Creeper) => {
                md.creeper_swell_dir = Some(v);
            }
            // Charged (lightning-struck): doubles the explosion radius and gates
            // the charged-creeper skull drop. Guarded for the same reason as
            // above — index 17 is `BOOLEAN` on several unrelated mobs.
            (IDX_CREEPER_POWERED, Value::Bool(b)) if class == Some(MetadataClass::Creeper) => {
                md.creeper_powered = Some(b);
            }
            // Lit by flint-and-steel/fire charge. Guarded for the same reason —
            // index 18 is `BOOLEAN` on several unrelated mobs, distinct from the
            // sheep's `BYTE` at the same index just above.
            (IDX_CREEPER_IGNITED, Value::Bool(b)) if class == Some(MetadataClass::Creeper) => {
                md.creeper_ignited = Some(b);
            }
            // `TamableAnimal.DATA_FLAGS_ID`: `isTame` is `0x04`, `isInSittingPose`
            // is `0x01`. Guarded on class because index 18's `BYTE` is also the
            // sheep's wool byte and the horse family's own (differently-bitted)
            // flags, just above and below.
            (IDX_TAMABLE_OR_HORSE_FLAGS, Value::Byte(b)) if class == Some(MetadataClass::Tamable) => {
                let byte = b as u8;
                md.tamed = Some(byte & 0x04 != 0);
                md.sitting = Some(byte & 0x01 != 0);
            }
            // `AbstractHorse.DATA_ID_FLAGS`, `FLAG_TAME = 0x02` — a *different*
            // bit from the tamable-animal arm above, at the same index. See
            // [`IDX_TAMABLE_OR_HORSE_FLAGS`].
            (IDX_TAMABLE_OR_HORSE_FLAGS, Value::Byte(b)) if class == Some(MetadataClass::Horse) => {
                md.tamed = Some((b as u8) & 0x02 != 0);
            }
            // Registry-holder variants identify themselves by serializer, so the
            // index is irrelevant and no class context is needed.
            (_, Value::Keyed(key)) => md.variant = Some(EntityVariant::Keyed(key)),
            (
                _,
                Value::Villager {
                    kind,
                    profession,
                    level,
                },
            ) => {
                md.variant = Some(EntityVariant::Villager {
                    kind,
                    profession,
                    level,
                });
            }
            // Any other (index, value) is decoded for alignment but not surfaced.
            _ => {}
        }
    }
    Ok(DecodedMetadata {
        metadata: md,
        complete: true,
    })
}

fn parse_identifier(raw: &str) -> Result<Identifier> {
    raw.parse()
        .map_err(|_| Error::Custom(format!("invalid identifier {raw:?}")))
}

fn checked_count(count: i32, cap: usize, what: &str) -> Result<usize> {
    let count =
        usize::try_from(count).map_err(|_| Error::Custom(format!("negative {what} {count}")))?;
    if count > cap {
        return Err(Error::Custom(format!("{what} {count} exceeds cap {cap}")));
    }
    Ok(count)
}

/// Decodes an `update_attributes` packet: an entity id and a length-prefixed list
/// of attribute snapshots, each carrying a registry-id attribute, an `f64` base,
/// and a list of `(id, amount, operation)` modifiers.
///
/// The attribute registry id is resolved to its canonical identifier through the
/// version-specific [`attribute_name`] table.
pub fn read_update_attributes(
    reader: &mut Reader<'_>,
) -> Result<(i32, Vec<EntityAttributeSnapshot>)> {
    let entity_id = reader.var_i32()?;
    let count = checked_count(reader.var_i32()?, MAX_ATTRIBUTES, "attribute count")?;
    let mut attributes = Vec::with_capacity(count);
    for _ in 0..count {
        let attribute_id = reader.var_i32()?;
        let base = reader.f64()?;
        let modifier_count =
            checked_count(reader.var_i32()?, usize::MAX, "attribute modifier count")?;
        let mut modifiers = Vec::with_capacity(modifier_count.min(64));
        for _ in 0..modifier_count {
            let id = reader.string(MAX_STRING)?;
            let amount = reader.f64()?;
            let operation = reader.var_i32()?;
            let operation = u8::try_from(operation).map_err(|_| {
                Error::Custom(format!("attribute operation {operation} out of range"))
            })?;
            modifiers.push(EntityAttributeModifier {
                id: parse_identifier(&id)?,
                amount,
                operation,
            });
        }
        let name = attribute_name(attribute_id)
            .ok_or_else(|| Error::Custom(format!("unknown attribute id {attribute_id}")))?;
        attributes.push(EntityAttributeSnapshot {
            attribute: parse_identifier(name)?,
            base,
            modifiers,
        });
    }
    Ok((entity_id, attributes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::Writer;

    /// A tracked record for an ordinary hostile/passive mob: living, a `Mob`, no
    /// ambiguous-variant class. The default subject of the index tests below.
    fn a_mob() -> TrackedEntity {
        TrackedEntity {
            class: None,
            living: true,
            mob: true,
        }
    }

    /// A **living non-mob** — a player, an armour stand, a mannequin. The
    /// population index 15's guard exists to exclude, and the reason that guard
    /// cannot be `living`.
    fn a_living_non_mob() -> TrackedEntity {
        TrackedEntity {
            class: None,
            living: true,
            mob: false,
        }
    }

    /// A non-living entity: an arrow, a dropped item, a display entity.
    fn not_living() -> TrackedEntity {
        TrackedEntity::default()
    }

    fn a_sheep() -> TrackedEntity {
        TrackedEntity {
            class: Some(MetadataClass::Sheep),
            living: true,
            mob: true,
        }
    }

    fn a_horse() -> TrackedEntity {
        TrackedEntity {
            class: Some(MetadataClass::Horse),
            living: true,
            mob: true,
        }
    }

    fn a_creeper() -> TrackedEntity {
        TrackedEntity {
            class: Some(MetadataClass::Creeper),
            living: true,
            mob: true,
        }
    }

    fn a_tamable_animal() -> TrackedEntity {
        TrackedEntity {
            class: Some(MetadataClass::Tamable),
            living: true,
            mob: true,
        }
    }

    /// An experience orb: neither living nor a mob, and carrying the class that
    /// unlocks index 8's `INT`.
    fn an_orb() -> TrackedEntity {
        TrackedEntity {
            class: Some(MetadataClass::ExperienceOrb),
            living: false,
            mob: false,
        }
    }

    /// Index 8's `INT` is an orb's XP value **only** when the caller says the
    /// entity is an orb; for anything else it is consumed for alignment and
    /// deliberately not surfaced.
    ///
    /// The control is the second half, and its premise is checked by
    /// `the_jar_dump_contains_the_collisions_the_guards_exist_for`: a primed TNT
    /// puts `DATA_FUSE_ID` at this exact index with this exact serializer, so
    /// without the class guard a lit TNT block would report itself as an orb worth
    /// 80 XP and draw an orb sprite. `a_mob()` stands in for "any tracked entity
    /// that is not an orb" — the guard is on the class, not on `living`, so a
    /// living subject is the harder case, not an easier one.
    #[test]
    fn index_8_int_is_an_orb_value_only_for_an_orb() {
        let payload = |value: i32| {
            let mut bytes = Vec::new();
            bytes.push(IDX_EXPERIENCE_ORB_VALUE);
            bytes.extend(varint(SER_INT));
            bytes.extend(varint(value));
            bytes.push(EOF_MARKER);
            bytes
        };

        // 617 rather than a small number: it is a real orb denomination and it
        // buckets to a different sprite cell than 0, so a decode that silently
        // produced the default would be visible downstream too.
        let bytes = payload(617);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, an_orb())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.experience_orb_value, Some(617));
        // And it did not also land in the two other fields index 8 can carry.
        assert_eq!(md.living_flags, None);
        assert!(!md.item.is_reported());

        // The control. Same bytes, a subject that is not an orb: the VarInt is
        // consumed (so the list stays aligned and the terminator is reached) and
        // nothing is surfaced.
        let mut reader = Reader::new(&bytes);
        let control = read_entity_metadata(&mut reader, a_mob())
            .expect("a non-orb must still decode, not error")
            .metadata;
        reader
            .ensure_empty()
            .expect("the INT must be consumed for alignment even when it is not surfaced");
        assert_eq!(
            control.experience_orb_value, None,
            "a mob's index-8 INT was surfaced as an orb value"
        );
        assert!(
            control.is_empty(),
            "the control surfaced something: {control:?}"
        );
    }

    /// Appends a network-NBT string component (`TAG_String` + modified-utf8) so
    /// tests can build an `OPTIONAL_COMPONENT` payload without a full NBT writer.
    fn push_string_component(bytes: &mut Vec<u8>, text: &str) {
        bytes.push(0x08); // TAG_String root id (network NBT: no name)
        let utf8 = text.as_bytes();
        bytes.extend_from_slice(&(utf8.len() as u16).to_be_bytes());
        bytes.extend_from_slice(utf8);
    }

    fn varint(value: i32) -> Vec<u8> {
        let mut w = Writer::default();
        w.var_i32(value);
        w.into_vec()
    }

    /// A hand-built metadata stream for a named, baby, on-fire pig: exercises the
    /// byte / optional-component / boolean / float / pose serializers and asserts
    /// each field lands at its known index with zero trailing bytes.
    #[test]
    fn decodes_named_baby_pig_metadata() {
        let mut bytes = Vec::new();
        // index 0, BYTE, shared flags = on-fire (0x01)
        bytes.push(IDX_SHARED_FLAGS);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x01);
        // index 2, OPTIONAL_COMPONENT, present, "Hoglet"
        bytes.push(IDX_CUSTOM_NAME);
        bytes.extend(varint(SER_OPTIONAL_COMPONENT));
        bytes.push(1);
        push_string_component(&mut bytes, "Hoglet");
        // index 3, BOOLEAN, custom name visible = true
        bytes.push(IDX_CUSTOM_NAME_VISIBLE);
        bytes.extend(varint(SER_BOOLEAN));
        bytes.push(1);
        // index 6, POSE, crouching (5)
        bytes.push(IDX_POSE);
        bytes.extend(varint(SER_POSE));
        bytes.extend(varint(5));
        // index 9, FLOAT, health = 10.0
        bytes.push(IDX_HEALTH);
        bytes.extend(varint(SER_FLOAT));
        bytes.extend(10.0f32.to_be_bytes());
        // index 16, BOOLEAN, baby = true
        bytes.push(IDX_BABY);
        bytes.extend(varint(SER_BOOLEAN));
        bytes.push(1);
        // index 19 (a pig's variant field), PIG_VARIANT serializer: a registry
        // holder we now resolve to a canonical key. Wire id 3 = registry id 2.
        bytes.push(19);
        bytes.extend(varint(28)); // PIG_VARIANT serializer id
        bytes.extend(varint(3)); // holder wire value (registry id 2)
        bytes.push(EOF_MARKER);

        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");

        assert_eq!(md.flags, Some(0x01));
        assert_eq!(
            md.custom_name,
            Reported::Reported(Some("Hoglet".to_string()))
        );
        assert_eq!(md.custom_name_visible, Some(true));
        assert_eq!(md.pose, Some(EntityPose::Crouching));
        assert_eq!(md.health, Some(10.0));
        assert_eq!(md.baby, Some(true));
        assert_eq!(
            md.variant,
            Some(EntityVariant::Keyed("minecraft:cold".parse().unwrap()))
        );
    }

    /// Index 1, `INT`, decodes to `air_supply` — the field this seam exists to
    /// close (`docs/sky-and-air-bubbles.md`). Verified against `Entity.java:268`'s
    /// `defineId` declaration order, not assumed.
    #[test]
    fn decodes_air_supply_at_index_1() {
        let mut bytes = Vec::new();
        bytes.push(IDX_AIR_SUPPLY);
        bytes.extend(varint(SER_INT));
        bytes.extend(varint(247));
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.air_supply, Some(247));
    }

    /// Index 8, `BYTE`, on a **living** entity decodes to `living_flags` — the
    /// using-item bitfield behind a bow draw (issue #57). Index verified against
    /// `LivingEntity.java:179` being `LivingEntity`'s first `defineId`, not
    /// assumed from a summary.
    #[test]
    fn decodes_living_flags_at_index_8_for_a_living_entity() {
        // Using an item, off hand: `setLivingEntityFlag(1, true)` +
        // `setLivingEntityFlag(2, hand == OFF_HAND)`.
        let mut bytes = Vec::new();
        bytes.push(IDX_LIVING_FLAGS);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x03);
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.living_flags, Some(0x03));
        // And it did not land in the *shared* flags byte, which is a different
        // field at a different index and would read 0x03 as "on fire, crouching".
        assert_eq!(md.flags, None);
    }

    /// **The control for the guard, and it must fail without it.** The identical
    /// bytes on a non-living entity are `AbstractArrow.ID_FLAGS` — bit `0x01` is
    /// the arrow's *crit* flag, not "using an item". The byte is still consumed
    /// (the list stays aligned and the terminator is reached) but must not be
    /// surfaced.
    ///
    /// Without the `if living` guard this test fails: `living_flags` comes back
    /// `Some(0x01)` and every critical arrow in flight reports itself as drawing
    /// a bow. Run it by deleting the guard to watch it fail — it was watched.
    #[test]
    fn index_8_on_a_non_living_entity_is_consumed_but_not_surfaced() {
        let mut bytes = Vec::new();
        bytes.push(IDX_LIVING_FLAGS);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x01); // AbstractArrow's crit bit
        // A second field *after* it, so this also proves the byte was consumed
        // rather than skipped: a misalignment here would make the health decode
        // garbage or error.
        bytes.push(IDX_HEALTH);
        bytes.extend(varint(SER_FLOAT));
        bytes.extend(2.5f32.to_be_bytes());
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, not_living())
            .expect("decode")
            .metadata;
        reader
            .ensure_empty()
            .expect("the byte must be consumed, leaving the list aligned");
        assert_eq!(
            md.living_flags, None,
            "an arrow's flags byte must not surface as living flags"
        );
        assert_eq!(md.health, Some(2.5), "the list stayed aligned past index 8");
    }

    /// The two `living` polarities over one fixture, so neither is a lone
    /// assertion that could pass on a table stuck at one value.
    #[test]
    fn the_living_guard_is_the_only_difference_between_the_two_decodes() {
        let mut bytes = Vec::new();
        bytes.push(IDX_LIVING_FLAGS);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x01);
        bytes.push(EOF_MARKER);
        let decode = |living: bool| {
            let mut reader = Reader::new(&bytes);
            let md = read_entity_metadata(&mut reader, TrackedEntity { class: None, living, mob: false })
                .expect("decode")
                .metadata;
            reader.ensure_empty().expect("aligned");
            md
        };
        let as_living = decode(true);
        let as_arrow = decode(false);
        assert_eq!(as_living.living_flags, Some(0x01));
        assert_eq!(as_arrow.living_flags, None);
        assert!(
            !as_living.is_empty(),
            "a living entity's flags byte is a reportable field"
        );
        assert!(
            as_arrow.is_empty(),
            "with nothing else in the list, an arrow's index-8 byte leaves the \
             update empty — so `handle_set_entity_data` emits no event at all"
        );
    }

    /// `TrackedEntity`'s insert gate: a type with neither an ambiguous variant
    /// class nor living-ness stays out of the adapter's map, which is what keeps
    /// it bounded to mobs rather than every entity in render distance.
    #[test]
    fn only_entities_with_a_fact_worth_remembering_are_tracked() {
        assert!(!TrackedEntity::default().is_tracked());
        assert!(a_living_non_mob().is_tracked());
        assert!(
            TrackedEntity {
                class: Some(MetadataClass::Sheep),
                living: false,
                mob: false,
            }
            .is_tracked()
        );
    }

    /// `is_tracked` has no `mob` disjunct, and this is why that is not a hole:
    /// `Mob extends LivingEntity`, so a mob is always caught by the `living` one.
    /// Stated as a test because "the missing disjunct is unreachable" is exactly
    /// the kind of claim that rots into a real gap.
    #[test]
    fn a_mob_is_tracked_through_the_living_disjunct() {
        assert!(a_mob().is_tracked());
        // The impossible record, asserted anyway: were a future census ever to
        // report `mob && !living`, the insert gate would drop it and the mob flags
        // would silently never arrive. `entity_census`'s own
        // `is_mob_is_strictly_narrower_than_is_living_and_the_gap_is_named` is
        // what rules the input out; this documents the consequence if it did not.
        assert!(
            !TrackedEntity {
                class: None,
                living: false,
                mob: true,
            }
            .is_tracked(),
            "a mob-but-not-living record is untracked, so the census guarantee \
             that it cannot occur is load-bearing, not decorative"
        );
    }

    /// Index 15, `BYTE`, on a **`Mob`** decodes to `mob_flags` — the byte whose
    /// `0x04` is `isAggressive()` and therefore whether a skeleton draws its bow
    /// (issue #379). The index comes from the jar dump, not a hand count; see
    /// `every_metadata_index_constant_matches_the_jar_dump`.
    #[test]
    fn decodes_mob_flags_at_index_15_for_a_mob() {
        let mut bytes = Vec::new();
        bytes.push(IDX_MOB_FLAGS);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x04); // aggressive, nothing else
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.mob_flags, Some(0x04));
        // And it landed in *this* byte, not in either of the other two flag bytes
        // that also live at low indices. 0x04 is `spin_attack` in the living byte
        // and `sprinting` in the shared one, both plausible and both wrong.
        assert_eq!(md.living_flags, None);
        assert_eq!(md.flags, None);
    }

    /// The guard, and the reason it is `mob` rather than `living`.
    ///
    /// An **armour stand** is a `LivingEntity`, so `living` does not exclude it —
    /// and its own index-15 `BYTE` uses `0x04` for `CLIENT_FLAG_SHOW_ARMS`
    /// (`ArmorStand.java:71`). An armour stand with arms is the ordinary
    /// decorative case, so a `living`-gated decode would report a large fraction
    /// of all armour stands as aggressive mobs and, holding a bow, draw it.
    ///
    /// Without the `if mob` guard this test fails with `left: Some(4), right:
    /// None` — run and watched.
    #[test]
    fn index_15_on_a_living_non_mob_is_consumed_but_not_surfaced() {
        let mut bytes = Vec::new();
        bytes.push(IDX_MOB_FLAGS);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x04); // an armour stand showing its arms
        // A following field must still decode cleanly: the unmatched byte is
        // consumed for *alignment*, not skipped.
        bytes.push(IDX_HEALTH);
        bytes.extend(varint(SER_FLOAT));
        bytes.extend(6.5f32.to_be_bytes());
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_living_non_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(
            md.mob_flags, None,
            "an armour stand's show-arms bit must not surface as mob flags"
        );
        assert_eq!(md.health, Some(6.5), "the following field must still align");
    }

    /// The two `mob` polarities over one fixture, so neither is a lone assertion
    /// about a shape that might differ between the two paths. Mirrors
    /// `the_living_guard_is_the_only_difference_between_the_two_decodes`.
    #[test]
    fn the_mob_guard_is_the_only_difference_between_the_two_decodes() {
        let mut bytes = Vec::new();
        bytes.push(IDX_MOB_FLAGS);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x04);
        bytes.push(EOF_MARKER);

        let decode = |tracked: TrackedEntity| {
            let mut reader = Reader::new(&bytes);
            let md = read_entity_metadata(&mut reader, tracked)
                .expect("decode")
                .metadata;
            reader.ensure_empty().expect("no trailing bytes");
            md
        };
        let as_mob = decode(a_mob());
        let as_stand = decode(a_living_non_mob());
        assert_eq!(as_mob.mob_flags, Some(0x04));
        assert_eq!(as_stand.mob_flags, None);
        assert!(
            !as_mob.is_empty(),
            "a mob's flags byte is a reportable field"
        );
        assert!(
            as_stand.is_empty(),
            "with nothing else in the list, an armour stand's index-15 byte leaves \
             the update empty — so `handle_set_entity_data` emits no event at all"
        );
    }

    /// An empty list (just the terminator) decodes to an empty update.
    #[test]
    fn empty_list_is_empty_update() {
        let bytes = [EOF_MARKER];
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert!(md.is_empty());
    }

    /// A cleared custom name (present field, empty optional) surfaces as
    /// `Reported::Reported(None)`, distinct from "field absent"
    /// (`Reported::Unreported`).
    #[test]
    fn cleared_custom_name_is_reported_none() {
        let mut bytes = Vec::new();
        bytes.push(IDX_CUSTOM_NAME);
        bytes.extend(varint(SER_OPTIONAL_COMPONENT));
        bytes.push(0); // absent
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert_eq!(md.custom_name, Reported::Reported(None));
    }

    /// A truncated value (float claims 4 bytes, only 2 present) must error rather
    /// than silently returning a partial decode — the misparse detector.
    #[test]
    fn truncated_value_errors() {
        let mut bytes = Vec::new();
        bytes.push(IDX_HEALTH);
        bytes.extend(varint(SER_FLOAT));
        bytes.extend_from_slice(&[0x41, 0x20]); // 2 of 4 float bytes
        // no terminator
        let mut reader = Reader::new(&bytes);
        assert!(read_entity_metadata(&mut reader, a_mob()).is_err());
    }

    /// The complex serializers that remain unmodelled (particle, particles,
    /// resolvable profile) are still rejected explicitly rather than guessed at.
    /// `ITEM_STACK` is deliberately absent from this list — see
    /// `tests/item_entity_metadata.rs`, which replays the server's own bytes.
    #[test]
    fn complex_serializers_are_rejected() {
        for serializer in [SER_PARTICLE, SER_PARTICLES, SER_RESOLVABLE_PROFILE] {
            let mut bytes = Vec::new();
            bytes.push(5); // arbitrary index
            bytes.extend(varint(serializer));
            bytes.push(0);
            bytes.push(EOF_MARKER);
            let mut reader = Reader::new(&bytes);
            assert!(
                read_entity_metadata(&mut reader, a_mob()).is_err(),
                "serializer {serializer} must not be guessed at"
            );
        }
    }

    /// An empty stack (`count <= 0`) is a *cleared* item field, distinct from the
    /// field being absent — the same `Reported::Reported(None)` shape the custom
    /// name uses. A following field still decodes, because an empty stack
    /// consumes its whole (one-byte) value.
    #[test]
    fn empty_item_stack_clears_the_field_and_stays_aligned() {
        let mut bytes = Vec::new();
        bytes.push(8); // ItemEntity.DATA_ITEM
        bytes.extend(varint(SER_ITEM_STACK));
        bytes.extend(varint(0)); // count 0 = the empty stack
        bytes.push(IDX_HEALTH);
        bytes.extend(varint(SER_FLOAT));
        bytes.extend(7.0f32.to_be_bytes());
        bytes.push(EOF_MARKER);

        let mut reader = Reader::new(&bytes);
        let decoded = read_entity_metadata(&mut reader, a_mob()).expect("decode");
        reader.ensure_empty().expect("no trailing bytes");

        assert!(decoded.complete);
        assert_eq!(decoded.metadata.item, Reported::Reported(None));
        assert_eq!(decoded.metadata.health, Some(7.0));
    }

    /// A sheep's wool byte at index 18 packs colour (low nibble) and the sheared
    /// flag (bit 4). Only raised when the entity is known to be a sheep.
    #[test]
    fn sheep_wool_byte_raises_dyed_variant() {
        let mut bytes = Vec::new();
        bytes.push(IDX_SHEEP_WOOL);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x1E); // colour 14 (red) + sheared bit (0x10)
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_sheep())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert_eq!(
            md.variant,
            Some(EntityVariant::Dyed {
                color: 14,
                sheared: true,
            })
        );
    }

    /// The same byte at index 18 with no sheep context (or a different class) must
    /// NOT be raised — index 18 aliases unrelated byte fields on other mobs
    /// (`AbstractHorse.DATA_ID_FLAGS` occupies it).
    #[test]
    fn wool_index_without_sheep_class_is_not_raised() {
        let mut bytes = Vec::new();
        bytes.push(IDX_SHEEP_WOOL);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x1E);
        bytes.push(EOF_MARKER);

        for class in [None, Some(MetadataClass::Horse)] {
            let mut reader = Reader::new(&bytes);
            let md = read_entity_metadata(&mut reader, TrackedEntity { class, living: true, mob: true })
                .expect("decode")
                .metadata;
            reader.ensure_empty().expect("empty");
            assert_eq!(md.variant, None);
        }
    }

    /// A horse's variant int at index 18 packs colour (low byte) and markings
    /// (next byte). Raised only when the entity is known to be a horse.
    #[test]
    fn horse_variant_int_raises_horse_variant() {
        let mut bytes = Vec::new();
        bytes.push(IDX_HORSE_VARIANT);
        bytes.extend(varint(SER_INT));
        bytes.extend(varint(0x0305)); // markings 3, colour 5
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_horse())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert_eq!(
            md.variant,
            Some(EntityVariant::Horse {
                color: 5,
                markings: 3,
            })
        );
    }

    /// The int at index 18 without horse context must not be raised.
    #[test]
    fn variant_index_without_horse_class_is_not_raised() {
        let mut bytes = Vec::new();
        bytes.push(IDX_HORSE_VARIANT);
        bytes.extend(varint(SER_INT));
        bytes.extend(varint(0x0305));
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_sheep())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert_eq!(md.variant, None);
    }

    /// A primed creeper's three fields: fuse direction (index 16, `INT`),
    /// powered (17, `BOOLEAN`), ignited (18, `BOOLEAN`) — all raised together on
    /// a known creeper. These were undecoded entirely before this seam (issue:
    /// live player report, "no swelling, no white flash"): `swell_dir` existed
    /// on the wire but had nowhere to land, which is why
    /// `lodestone_render::entity_anim::pose_swelling`'s working swell math never
    /// reached a real creeper.
    #[test]
    fn creeper_fields_are_raised_together() {
        let mut bytes = Vec::new();
        bytes.push(IDX_CREEPER_SWELL_DIR);
        bytes.extend(varint(SER_INT));
        bytes.extend(varint(1)); // counting up: ignited or in SwellGoal range
        bytes.push(IDX_CREEPER_POWERED);
        bytes.extend(varint(SER_BOOLEAN));
        bytes.push(1);
        bytes.push(IDX_CREEPER_IGNITED);
        bytes.extend(varint(SER_BOOLEAN));
        bytes.push(1);
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_creeper())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.creeper_swell_dir, Some(1));
        assert_eq!(md.creeper_powered, Some(true));
        assert_eq!(md.creeper_ignited, Some(true));
    }

    /// The idle default: `swell_dir == -1` (`Creeper.java:100`,
    /// `entityData.define(DATA_SWELL_DIR, -1)`), `powered`/`ignited` both
    /// `false`. A real server never puts these on the wire for an ordinary
    /// spawn (`SynchedEntityData` only sends non-default values — the same
    /// mechanism the sheep-wool fix's module docs describe), so this is what
    /// `handle_add_entity`'s synthesized default must produce, not what a real
    /// packet carries.
    #[test]
    fn creeper_idle_defaults_decode_when_present() {
        let mut bytes = Vec::new();
        bytes.push(IDX_CREEPER_SWELL_DIR);
        bytes.extend(varint(SER_INT));
        bytes.extend(varint(-1));
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_creeper())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.creeper_swell_dir, Some(-1));
    }

    /// **The control, and it must fail without the class guard.** Index 16's
    /// `INT` is also `Warden.CLIENT_ANGER_LEVEL` — bit-identical serializer, no
    /// other signal distinguishes them. Without `if class ==
    /// Some(MetadataClass::Creeper)` this test fails: a warden's anger level
    /// would decode as `creeper_swell_dir`, and every warden in render distance
    /// would report a creeper's fuse. Run by deleting the guard to watch it
    /// fail — it was watched.
    #[test]
    fn index_16_without_creeper_class_is_consumed_but_not_surfaced() {
        let mut bytes = Vec::new();
        bytes.push(IDX_CREEPER_SWELL_DIR);
        bytes.extend(varint(SER_INT));
        bytes.extend(varint(37)); // a plausible warden anger level
        // A following field to prove alignment survived the unmatched value.
        bytes.push(IDX_HEALTH);
        bytes.extend(varint(SER_FLOAT));
        bytes.extend(4.0f32.to_be_bytes());
        bytes.push(EOF_MARKER);
        for tracked in [a_mob(), a_sheep(), a_horse()] {
            let mut reader = Reader::new(&bytes);
            let md = read_entity_metadata(&mut reader, tracked)
                .expect("decode")
                .metadata;
            reader.ensure_empty().expect("the value must be consumed, staying aligned");
            assert_eq!(
                md.creeper_swell_dir, None,
                "a non-creeper's index-16 INT must not surface as a creeper's swell direction"
            );
            assert_eq!(md.health, Some(4.0), "the following field must still align");
        }
    }

    /// The same control for the two `BOOLEAN` fields (17 powered, 18 ignited),
    /// which collide with `Witch.DATA_USING_ITEM`/`EnderMan.DATA_CREEPY` (17)
    /// and `Turtle.HAS_EGG`/`Ocelot.DATA_TRUSTING` (18) among others.
    #[test]
    fn indices_17_and_18_without_creeper_class_are_consumed_but_not_surfaced() {
        let mut bytes = Vec::new();
        bytes.push(IDX_CREEPER_POWERED);
        bytes.extend(varint(SER_BOOLEAN));
        bytes.push(1);
        bytes.push(IDX_CREEPER_IGNITED);
        bytes.extend(varint(SER_BOOLEAN));
        bytes.push(1);
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.creeper_powered, None);
        assert_eq!(md.creeper_ignited, None);
    }

    /// A registry-holder appearance variant (here a wolf, serializer 25) is
    /// self-identifying: it raises `Keyed` from the serializer alone, at whatever
    /// index it appears and with no class context. Wire value is `id + 1`.
    #[test]
    fn wolf_variant_holder_raises_keyed() {
        let mut bytes = Vec::new();
        bytes.push(22); // wolf's variant field index (irrelevant to the raise)
        bytes.extend(varint(25)); // WOLF_VARIANT serializer
        bytes.extend(varint(5)); // holder wire value → registry id 4 → ashen
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert_eq!(
            md.variant,
            Some(EntityVariant::Keyed("minecraft:ashen".parse().unwrap()))
        );
    }

    /// A cow variant (serializer 23) resolves through the shared temperature table.
    #[test]
    fn cow_variant_holder_raises_keyed() {
        let mut bytes = Vec::new();
        bytes.push(17);
        bytes.extend(varint(23)); // COW_VARIANT serializer
        bytes.extend(varint(2)); // wire value → registry id 1 → warm
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert_eq!(
            md.variant,
            Some(EntityVariant::Keyed("minecraft:warm".parse().unwrap()))
        );
    }

    /// An unmapped holder id stays byte-aligned and raises no variant, so a
    /// datapack-added variant degrades to "no override" rather than a wrong key.
    #[test]
    fn unmapped_variant_id_raises_nothing_but_stays_aligned() {
        let mut bytes = Vec::new();
        bytes.push(17);
        bytes.extend(varint(23)); // cow variant
        bytes.extend(varint(99)); // wire value → registry id 98 → unmapped
        bytes.push(IDX_HEALTH); // a following field must still decode cleanly
        bytes.extend(varint(SER_FLOAT));
        bytes.extend(5.0f32.to_be_bytes());
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert_eq!(md.variant, None);
        assert_eq!(md.health, Some(5.0));
    }

    /// VillagerData (serializer 18) decodes two registry holders and a level into
    /// the `Villager` variant.
    ///
    /// The index byte here is decorative — `read_entity_metadata`'s value-to-field
    /// mapping matches `Value::Villager` by *serializer* alone, not by index (see
    /// its `(_, Value::Villager { .. })` arm), so this test would pass at any
    /// index. It is still set to the real one rather than an arbitrary value:
    /// `Villager.DATA_VILLAGER_DATA` is index **19** per the committed
    /// `EntityDataIndexOracle` dump (`tests/support/entity_data_index_jvm.txt`),
    /// not 17 — a prior guess this fixture used to encode, now corrected so a
    /// reader copying this test as a template for the server-side encode arm
    /// does not propagate the wrong index.
    #[test]
    fn villager_data_raises_villager_variant() {
        let mut bytes = Vec::new();
        bytes.push(19); // Villager.DATA_VILLAGER_DATA (oracle-verified)
        bytes.extend(varint(SER_VILLAGER_DATA));
        bytes.extend(varint(4)); // type wire → id 3 → savanna
        bytes.extend(varint(6)); // profession wire → id 5 → farmer
        bytes.extend(varint(3)); // level
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert_eq!(
            md.variant,
            Some(EntityVariant::Villager {
                kind: "minecraft:savanna".parse().unwrap(),
                profession: "minecraft:farmer".parse().unwrap(),
                level: 3,
            })
        );
    }

    /// `TamableAnimal.DATA_FLAGS_ID`'s two bits (`0x04` tame, `0x01` sitting) at
    /// index 18, guarded on [`MetadataClass::Tamable`]. Pairwise-distinct byte
    /// (`0x05` = both bits set, not the same as either alone) so a bit-position
    /// transposition between `tamed`/`sitting` cannot survive.
    #[test]
    fn tamable_flags_byte_raises_tamed_and_sitting() {
        let mut bytes = Vec::new();
        bytes.push(18);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x05_i8 as u8); // tame (0x04) + sitting (0x01)
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_tamable_animal())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert_eq!(md.tamed, Some(true));
        assert_eq!(md.sitting, Some(true));
    }

    /// A wolf that is tame but not sitting — the two bits set independently
    /// rather than both true, so `tamed`/`sitting` can't coincidentally agree.
    #[test]
    fn tamable_flags_byte_distinguishes_tame_from_sitting() {
        let mut bytes = Vec::new();
        bytes.push(18);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x04); // tame, not sitting
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_tamable_animal())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert_eq!(md.tamed, Some(true));
        assert_eq!(md.sitting, Some(false));
    }

    /// `AbstractHorse.DATA_ID_FLAGS`'s `FLAG_TAME = 0x02`, guarded on
    /// [`MetadataClass::Horse`] — a **different** bit from the tamable-animal
    /// arm above at the same index. `0x02` set alone (not `0x04`) is exactly
    /// the byte that would read as "untamed" under a shared-bit
    /// implementation, which is the failure this test exists to catch: if the
    /// horse arm ever regresses to checking `0x04` instead of `0x02`, this
    /// fails rather than passing on a coincidence.
    #[test]
    fn horse_flags_byte_uses_the_horse_tame_bit_not_the_tamable_bit() {
        let mut bytes = Vec::new();
        bytes.push(18);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x02); // AbstractHorse.FLAG_TAME
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_horse())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert_eq!(md.tamed, Some(true));
        assert_eq!(md.sitting, None, "the horse family has no sitting bit here");
    }

    /// The same raw byte (`0x02`, horse-tame only) decoded under
    /// [`MetadataClass::Tamable`] must **not** read as tamed — proving the two
    /// arms really do gate on different bits rather than one shared "tamed"
    /// reading that happens to pass the tests above.
    #[test]
    fn horse_tame_bit_does_not_leak_into_a_tamable_animals_reading() {
        let mut bytes = Vec::new();
        bytes.push(18);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x02); // set only the horse's tame bit
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_tamable_animal())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("empty");
        assert_eq!(
            md.tamed,
            Some(false),
            "0x02 is not TamableAnimal's tame bit (0x04)"
        );
        assert_eq!(md.sitting, Some(false));
    }

    #[test]
    fn metadata_class_only_classifies_ambiguous_mobs() {
        assert_eq!(metadata_class("minecraft:sheep"), Some(MetadataClass::Sheep));
        assert_eq!(metadata_class("minecraft:horse"), Some(MetadataClass::Horse));
        assert_eq!(metadata_class("minecraft:donkey"), Some(MetadataClass::Horse));
        assert_eq!(metadata_class("minecraft:mule"), Some(MetadataClass::Horse));
        assert_eq!(metadata_class("minecraft:llama"), Some(MetadataClass::Horse));
        assert_eq!(metadata_class("minecraft:trader_llama"), Some(MetadataClass::Horse));
        assert_eq!(metadata_class("minecraft:skeleton_horse"), Some(MetadataClass::Horse));
        assert_eq!(metadata_class("minecraft:zombie_horse"), Some(MetadataClass::Horse));
        assert_eq!(metadata_class("minecraft:camel"), Some(MetadataClass::Horse));
        assert_eq!(metadata_class("minecraft:wolf"), Some(MetadataClass::Tamable));
        assert_eq!(metadata_class("minecraft:cat"), Some(MetadataClass::Tamable));
        assert_eq!(metadata_class("minecraft:parrot"), Some(MetadataClass::Tamable));
        assert_eq!(metadata_class("minecraft:cow"), None);
        assert_eq!(metadata_class("minecraft:villager"), None);
    }

    /// A known-answer `update_attributes`: one movement-speed attribute with a
    /// base and a single add-value modifier, asserting exact fields and zero
    /// trailing bytes.
    #[test]
    fn decodes_update_attributes() {
        let mut bytes = Vec::new();
        bytes.extend(varint(1471)); // entity id
        bytes.extend(varint(1)); // one attribute
        bytes.extend(varint(26)); // movement_speed registry id
        bytes.extend(0.25f64.to_be_bytes()); // base
        bytes.extend(varint(1)); // one modifier
        let mod_id = "minecraft:test_speed";
        bytes.extend(varint(mod_id.len() as i32));
        bytes.extend_from_slice(mod_id.as_bytes());
        bytes.extend(0.3f64.to_be_bytes()); // amount
        bytes.extend(varint(2)); // ADD_MULTIPLIED_TOTAL

        let mut reader = Reader::new(&bytes);
        let (entity_id, attrs) = read_update_attributes(&mut reader).expect("decode");
        reader.ensure_empty().expect("no trailing bytes");

        assert_eq!(entity_id, 1471);
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].attribute.to_string(), "minecraft:movement_speed");
        assert!((attrs[0].base - 0.25).abs() < 1e-12);
        assert_eq!(attrs[0].modifiers.len(), 1);
        assert_eq!(attrs[0].modifiers[0].id.to_string(), mod_id);
        assert!((attrs[0].modifiers[0].amount - 0.3).abs() < 1e-12);
        assert_eq!(attrs[0].modifiers[0].operation, 2);
    }

    /// An unknown attribute id fails loudly rather than resolving to a wrong name.
    #[test]
    fn unknown_attribute_id_errors() {
        let mut bytes = Vec::new();
        bytes.extend(varint(1)); // entity id
        bytes.extend(varint(1)); // one attribute
        bytes.extend(varint(9999)); // out-of-range attribute id
        bytes.extend(0.0f64.to_be_bytes());
        bytes.extend(varint(0)); // no modifiers
        let mut reader = Reader::new(&bytes);
        assert!(read_update_attributes(&mut reader).is_err());
    }

    // -----------------------------------------------------------------------
    // The `IDX_*` constants, anchored to the jar
    // -----------------------------------------------------------------------

    /// Every `EntityDataAccessor` in 26.2, dumped from a headless server and
    /// sorted by index so collisions are adjacent lines. See
    /// `oracle-java/EntityDataIndexOracle.java`.
    ///
    /// This exists because every `IDX_*` above is a **hand count** over
    /// `SynchedEntityData.defineId`'s per-hierarchy declaration-order counter —
    /// exactly the kind of expected value `CLAUDE.md` requires to come from
    /// outside the code under test. Two of them were wrong (see
    /// [`IDX_SHEEP_WOOL`]).
    const INDEX_DUMP: &str = include_str!("../../tests/support/entity_data_index_jvm.txt");

    /// `(index, serializer_id)` for `Owner.FIELD`, or a panic naming the miss.
    fn dump_row(owner_field: &str) -> (u8, i32) {
        let mut found = None;
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let index: u8 = tok.next().expect("index column").parse().expect("index is a u8");
            let owner = tok.next().expect("owner.FIELD column");
            let serializer: i32 = tok
                .next()
                .expect("serializer column")
                .parse()
                .expect("serializer is an i32");
            if owner == owner_field {
                assert!(
                    found.is_none(),
                    "{owner_field} appears twice in the dump, which cannot happen"
                );
                found = Some((index, serializer));
            }
        }
        found.unwrap_or_else(|| {
            panic!(
                "{owner_field} is not in the jar dump — the field was renamed or removed in this \
                 version; read the dump before changing the constant"
            )
        })
    }

    /// Every `Owner.FIELD` the dump reports at `index`, with its serializer.
    fn dump_claimants(index: u8) -> Vec<(String, i32)> {
        let mut out = Vec::new();
        for line in INDEX_DUMP.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tok = line.split_whitespace();
            let row_index: u8 = tok.next().expect("index column").parse().expect("index is a u8");
            let owner = tok.next().expect("owner.FIELD column").to_owned();
            let serializer: i32 = tok
                .next()
                .expect("serializer column")
                .parse()
                .expect("serializer is an i32");
            if row_index == index {
                out.push((owner, serializer));
            }
        }
        out
    }

    /// Each `IDX_*` constant equals the index the jar assigned the accessor it
    /// claims to name, *and* the serializer this decoder matches on is the one
    /// that accessor uses.
    ///
    /// Both halves matter. A right index with the wrong serializer arm silently
    /// never matches — which is precisely how the sheep-wool defect behaved.
    #[test]
    fn every_metadata_index_constant_matches_the_jar_dump() {
        // (constant, its name, the accessor it claims, the serializer we match on)
        let claims: &[(u8, &str, &str, i32)] = &[
            (IDX_SHARED_FLAGS, "IDX_SHARED_FLAGS", "Entity.DATA_SHARED_FLAGS_ID", SER_BYTE),
            (IDX_AIR_SUPPLY, "IDX_AIR_SUPPLY", "Entity.DATA_AIR_SUPPLY_ID", SER_INT),
            (
                IDX_CUSTOM_NAME,
                "IDX_CUSTOM_NAME",
                "Entity.DATA_CUSTOM_NAME",
                SER_OPTIONAL_COMPONENT,
            ),
            (
                IDX_CUSTOM_NAME_VISIBLE,
                "IDX_CUSTOM_NAME_VISIBLE",
                "Entity.DATA_CUSTOM_NAME_VISIBLE",
                SER_BOOLEAN,
            ),
            (IDX_POSE, "IDX_POSE", "Entity.DATA_POSE", SER_POSE),
            (
                IDX_LIVING_FLAGS,
                "IDX_LIVING_FLAGS",
                "LivingEntity.DATA_LIVING_ENTITY_FLAGS",
                SER_BYTE,
            ),
            (IDX_HEALTH, "IDX_HEALTH", "LivingEntity.DATA_HEALTH_ID", SER_FLOAT),
            (IDX_MOB_FLAGS, "IDX_MOB_FLAGS", "Mob.DATA_MOB_FLAGS_ID", SER_BYTE),
            (IDX_BABY, "IDX_BABY", "AgeableMob.DATA_BABY_ID", SER_BOOLEAN),
            (IDX_SHEEP_WOOL, "IDX_SHEEP_WOOL", "Sheep.DATA_WOOL_ID", SER_BYTE),
            (
                IDX_HORSE_VARIANT,
                "IDX_HORSE_VARIANT",
                "Horse.DATA_ID_TYPE_VARIANT",
                SER_INT,
            ),
            (
                IDX_CREEPER_SWELL_DIR,
                "IDX_CREEPER_SWELL_DIR",
                "Creeper.DATA_SWELL_DIR",
                SER_INT,
            ),
            (
                IDX_CREEPER_POWERED,
                "IDX_CREEPER_POWERED",
                "Creeper.DATA_IS_POWERED",
                SER_BOOLEAN,
            ),
            (
                IDX_CREEPER_IGNITED,
                "IDX_CREEPER_IGNITED",
                "Creeper.DATA_IS_IGNITED",
                SER_BOOLEAN,
            ),
            (
                IDX_EXPERIENCE_ORB_VALUE,
                "IDX_EXPERIENCE_ORB_VALUE",
                "ExperienceOrb.DATA_VALUE",
                SER_INT,
            ),
            (
                IDX_TAMABLE_OR_HORSE_FLAGS,
                "IDX_TAMABLE_OR_HORSE_FLAGS",
                "TamableAnimal.DATA_FLAGS_ID",
                SER_BYTE,
            ),
            (
                IDX_TAMABLE_OR_HORSE_FLAGS,
                "IDX_TAMABLE_OR_HORSE_FLAGS",
                "AbstractHorse.DATA_ID_FLAGS",
                SER_BYTE,
            ),
        ];
        assert!(!claims.is_empty(), "the claim table is empty, so this test proves nothing");
        for &(constant, name, owner_field, serializer) in claims {
            let (dumped_index, dumped_serializer) = dump_row(owner_field);
            assert_eq!(
                constant, dumped_index,
                "{name} says {constant} but the jar puts {owner_field} at {dumped_index}"
            );
            assert_eq!(
                serializer, dumped_serializer,
                "{name} ({owner_field}) is matched on serializer {serializer} but the jar encodes \
                 it with {dumped_serializer}; the arm would never fire"
            );
        }
    }

    /// The dump's own non-vacuity control: it must actually contain the
    /// collisions this decoder's guards exist for, or the test above is checking
    /// a table that could have been written from the same wrong count.
    ///
    /// Index **8** is `LivingEntity`'s flags byte *and* `AbstractArrow`'s, both
    /// `BYTE`, with `0x01` meaning "using item" on one and "critical" on the
    /// other — the `living` guard (issue #57). Index **15** is `Mob`'s flags byte
    /// *and* `ArmorStand`'s client flags, both `BYTE`, with `0x04` meaning
    /// "aggressive" on one and "show arms" on the other — and since `ArmorStand`
    /// *is* a `LivingEntity`, that one needs a narrower guard than index 8's
    /// (issue #379).
    #[test]
    fn the_jar_dump_contains_the_collisions_the_guards_exist_for() {
        let at_8 = dump_claimants(8);
        for owner in ["LivingEntity.DATA_LIVING_ENTITY_FLAGS", "AbstractArrow.ID_FLAGS"] {
            assert!(
                at_8.contains(&(owner.to_owned(), SER_BYTE)),
                "index 8 does not claim {owner} as a BYTE in the dump; the `living` guard's \
                 premise is not what this test thinks it is"
            );
        }

        // Index 8's *third* collision, and the premise of the `ExperienceOrb` class
        // guard: `ExperienceOrb.DATA_VALUE` shares the index with four unrelated
        // `INT`s, so neither the serializer nor the `living`/`mob` census can
        // separate them. Named individually rather than counted, so a dump that
        // dropped one of them fails here rather than weakening the guard silently.
        for owner in [
            "ExperienceOrb.DATA_VALUE",
            "PrimedTnt.DATA_FUSE_ID",
            "FishingHook.DATA_HOOKED_ENTITY",
            "VehicleEntity.DATA_ID_HURT",
        ] {
            assert!(
                at_8.contains(&(owner.to_owned(), SER_INT)),
                "index 8 does not claim {owner} as an INT in the dump; the `ExperienceOrb` class \
                 guard's premise is not what this test thinks it is"
            );
        }

        let at_15 = dump_claimants(15);
        for owner in ["Mob.DATA_MOB_FLAGS_ID", "ArmorStand.DATA_CLIENT_FLAGS"] {
            assert!(
                at_15.contains(&(owner.to_owned(), SER_BYTE)),
                "index 15 does not claim {owner} as a BYTE in the dump"
            );
        }

        // Index 16 is `Creeper.DATA_SWELL_DIR` *and* an unrelated `INT` on at
        // least `Warden.CLIENT_ANGER_LEVEL` — the `Creeper` class guard's premise
        // for `index_16_without_creeper_class_is_consumed_but_not_surfaced`.
        let at_16 = dump_claimants(16);
        for owner in ["Creeper.DATA_SWELL_DIR", "Warden.CLIENT_ANGER_LEVEL"] {
            assert!(
                at_16.contains(&(owner.to_owned(), SER_INT)),
                "index 16 does not claim {owner} as an INT in the dump; the `Creeper` class \
                 guard's premise is not what this test thinks it is"
            );
        }

        // Indices 17 and 18 are `Creeper.DATA_IS_POWERED`/`DATA_IS_IGNITED` *and*
        // unrelated `BOOLEAN`s on other mobs.
        let at_17 = dump_claimants(17);
        for owner in ["Creeper.DATA_IS_POWERED", "Witch.DATA_USING_ITEM"] {
            assert!(
                at_17.contains(&(owner.to_owned(), SER_BOOLEAN)),
                "index 17 does not claim {owner} as a BOOLEAN in the dump"
            );
        }
        let at_18 = dump_claimants(18);
        for owner in ["Creeper.DATA_IS_IGNITED", "Turtle.HAS_EGG"] {
            assert!(
                at_18.contains(&(owner.to_owned(), SER_BOOLEAN)),
                "index 18 does not claim {owner} as a BOOLEAN in the dump"
            );
        }

        // The `Tamable`/`Horse` class guard's own premise: index 18's `BYTE` has
        // *four* claimants (`Sheep.DATA_WOOL_ID` already covered above via the
        // `Sheep` class), and the two the tame-flag arms exist for are
        // `TamableAnimal.DATA_FLAGS_ID` and `AbstractHorse.DATA_ID_FLAGS` — a
        // wolf and a horse never coexist as the same concrete type, so this is
        // the same species-mutual-exclusion shape as the sheep/creeper pair
        // above, not a real ambiguity once the class is known.
        for owner in [
            "Sheep.DATA_WOOL_ID",
            "Shulker.DATA_COLOR_ID",
            "TamableAnimal.DATA_FLAGS_ID",
            "AbstractHorse.DATA_ID_FLAGS",
        ] {
            assert!(
                at_18.contains(&(owner.to_owned(), SER_BYTE)),
                "index 18 does not claim {owner} as a BYTE in the dump; the `Tamable`/`Horse` \
                 class guard's premise is not what this test thinks it is"
            );
        }

        // And the dump is a whole-game dump, not a handful of lines someone
        // pasted: 26.2 has well over a hundred accessors.
        let rows = INDEX_DUMP
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
            .count();
        assert!(rows > 150, "the index dump has only {rows} rows, so it is truncated");
    }
}
