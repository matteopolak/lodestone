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

use lodestone_core::{Error, Reader, Result, Writer, read_network_nbt};
use lodestone_model::{
    BlockPos, EntityAttributeModifier, EntityAttributeSnapshot, EntityMetadataUpdate, EntityPose,
    EntityVariant, Identifier, ItemStack, Quat, Reported, Text, Vec3f,
};

use lodestone_data::attribute_types::{attribute_id, attribute_name};
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
// `Entity`'s second `defineId` call (`Entity.java`, right after
// `DATA_SHARED_FLAGS_ID` at :260 and right before `DATA_CUSTOM_NAME` at :269)
// — `SynchedEntityData.defineId` assigns ids by a class-static counter in
// declaration order, so this is index 1, verified against the jar's own
// source rather than trusted from a briefing.
const IDX_AIR_SUPPLY: u8 = 1;
const IDX_CUSTOM_NAME: u8 = 2;
const IDX_CUSTOM_NAME_VISIBLE: u8 = 3;
const IDX_POSE: u8 = 6;
/// `LivingEntity.DATA_LIVING_ENTITY_FLAGS`, the first
/// `defineId` in `LivingEntity` and therefore index 8 — the byte carrying
/// using-item / off-hand / spin-attack.
///
/// **This index is ambiguous and needs the entity's concrete type.** It is also
/// where `AbstractArrow.ID_FLAGS` lands (`Projectile`
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
/// `Mob.DATA_MOB_FLAGS_ID`, `Mob`'s **only** `defineId` and
/// therefore index 15 — the byte carrying no-AI `0x01` / left-handed `0x02` /
/// **aggressive `0x04`** (`Mob.setAggressive`/`Mob.isAggressive`). Aggressive is what makes a
/// skeleton draw its bow: vanilla's mob renderers read `isAggressive()`, *not*
/// the using-item bit at index 8, which is a player mechanism.
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

/// `Creeper.DATA_SWELL_DIR` (`Creeper.java`), `Creeper`'s first `defineId`
/// and therefore index 16 — `Monster` (its superclass) declares none of its
/// own, so the count runs `Entity`(0-7) → `LivingEntity`(8-14) → `Mob`(15) →
/// `Creeper`(16-18) directly, with no `AgeableMob` in between (a creeper is
/// not ageable). Verified against `tests/support/entity_data_index_jvm.txt`,
/// not hand-counted — see that file's own warning about what hand-counting
/// this exact shape (a class with no `Ageable` in its chain) has cost before.
///
/// An `INT`, `-1` or `1`: which way `swell` is currently moving, integrated
/// **client-side** every tick exactly as the server does (`Creeper.java`,
/// `this.swell += swellDir`) — only the direction is synced, never the
/// counter itself. See [`crate::adapter`]'s per-tick fuse integration and
/// `lodestone_render::entity_anim::pose_swelling`'s docs for why that split
/// exists.
const IDX_CREEPER_SWELL_DIR: u8 = 16;
/// `Creeper.DATA_IS_POWERED` (`Creeper.java`), index 17 — a `BOOLEAN`, set
/// once by `thunderHit` (`Creeper.java`) and never cleared. Doubles the
/// explosion radius (`Creeper.java`) and gates the charged-creeper skull
/// drop; not consumed by rendering yet.
const IDX_CREEPER_POWERED: u8 = 17;
/// `Creeper.DATA_IS_IGNITED` (`Creeper.java`), index 18 — a `BOOLEAN`, set
/// once by `ignite()` (flint-and-steel or fire-charge, `Creeper.java`) and
/// never cleared. Distinct from a **non**-ignited swell (the `SwellGoal`
/// proximity case, which moves `swell_dir` without ever setting this):
/// `ignited` alone would miss a creeper that swells because a player got
/// close and then backs off before detonation, since that path only ever
/// touches `DATA_SWELL_DIR`.
const IDX_CREEPER_IGNITED: u8 = 18;

/// `ArmorStand.DATA_HEAD_POSE` (`ArmorStand.java`), index 16 — the first of six
/// consecutive `ROTATIONS` accessors, `DATA_HEAD_POSE` through
/// `DATA_RIGHT_LEG_POSE` at 16-21, each an `(x, y, z)` triple of Euler degrees.
///
/// # Why these six carry no class guard where index 16's `INT` needs two
///
/// Index 16 alone is hopeless — the committed jar dump
/// (`tests/support/entity_data_index_jvm.txt`) lists 29 claimants, which is why
/// [`IDX_CREEPER_SWELL_DIR`] and [`IDX_DRAGON_PHASE`] each need a
/// [`MetadataClass`]. But the *serializer* settles it here: grepping that dump
/// for `ROTATIONS` returns exactly six lines, all six of them `ArmorStand`. So
/// a `(index, Value::Rotations(_))` pair is unambiguous on the value shape
/// alone, the same property that lets `VECTOR3`/`QUATERNION` skip a guard, and
/// the index is only being asked which *part* moved. Adding a class guard would
/// not be harmless: it would silently drop the pose for any stand whose spawn
/// packet the adapter could not resolve a class from, which is the failure this
/// whole chain exists to prevent.
///
/// # Why decoding these is load bearing rather than cosmetic
///
/// `ArmorStandArmorModel.setupAnim` calls the humanoid `super.setupAnim` —
/// walk cycle, idle bob and all — and then **assigns** all six part rotations
/// from these values. Vanilla computes the swing and throws it away. Dropping
/// these six therefore does not make a stand look neutral; it leaves the walk
/// cycle in place, so a stand carried by a moving contraption swings its arms
/// like a running player, and an item in its hand swings with them.
const IDX_ARMOR_STAND_HEAD_POSE: u8 = 16;
/// `ArmorStand.DATA_BODY_POSE`, index 17. See [`IDX_ARMOR_STAND_HEAD_POSE`].
const IDX_ARMOR_STAND_BODY_POSE: u8 = 17;
/// `ArmorStand.DATA_LEFT_ARM_POSE`, index 18. See [`IDX_ARMOR_STAND_HEAD_POSE`].
const IDX_ARMOR_STAND_LEFT_ARM_POSE: u8 = 18;
/// `ArmorStand.DATA_RIGHT_ARM_POSE`, index 19. See [`IDX_ARMOR_STAND_HEAD_POSE`].
const IDX_ARMOR_STAND_RIGHT_ARM_POSE: u8 = 19;
/// `ArmorStand.DATA_LEFT_LEG_POSE`, index 20. See [`IDX_ARMOR_STAND_HEAD_POSE`].
const IDX_ARMOR_STAND_LEFT_LEG_POSE: u8 = 20;
/// `ArmorStand.DATA_RIGHT_LEG_POSE`, index 21. See [`IDX_ARMOR_STAND_HEAD_POSE`].
const IDX_ARMOR_STAND_RIGHT_LEG_POSE: u8 = 21;

/// `EnderDragon.DATA_PHASE` (`EnderDragon.java`), `EnderDragon`'s first
/// `defineId` and therefore index 16 by the same class-hierarchy count as
/// [`IDX_CREEPER_SWELL_DIR`] (`Entity`(0-7) → `LivingEntity`(8-14) →
/// `Mob`(15) → `EnderDragon`(16), no `AgeableMob`). An `INT`: the current
/// vanilla `EnderDragonPhase` id (holding pattern / strafing / sitting /
/// dying / …) — see `tests/support/entity_data_index_jvm.txt` for the five
/// other `INT` claimants at this index this module's own
/// [`MetadataClass::Dragon`] guard exists to exclude.
const IDX_DRAGON_PHASE: u8 = 16;
/// `EndCrystal.DATA_BEAM_TARGET` (`EndCrystal.java`), index 8 — an
/// `OPTIONAL_BLOCK_POS`. Self-identifying **at this index** (no other index-8
/// claimant in the jar dump is `OPTIONAL_BLOCK_POS`), but the serializer is
/// **not** globally self-identifying: the same serializer is also
/// `LivingEntity.SLEEPING_POS_ID` at index 14 and `Creaking.HOME_POS` at
/// index 19, so the decode arm below still keys on this index rather than the
/// bare `Value::OptBlockPos` shape.
const IDX_CRYSTAL_BEAM_TARGET: u8 = 8;
/// `EndCrystal.DATA_SHOW_BOTTOM` (`EndCrystal.java`), index 9 — a `BOOLEAN`,
/// one of three claimants at that index (`AreaEffectCloud.DATA_WAITING`,
/// `FishingHook.DATA_BITING` are the other two), hence the
/// [`MetadataClass::EndCrystal`] guard.
const IDX_CRYSTAL_SHOW_BOTTOM: u8 = 9;
/// `ItemFrame.DATA_ROTATION` (`ItemFrame.java`), index 10 — an `INT` carrying
/// `0..8`, the eighth-turns the framed stack is rotated by.
///
/// **Index 10, three `INT` claimants** in the committed jar dump
/// (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`):
/// `Display.DATA_POS_ROT_INTERPOLATION_DURATION_ID`, `ItemFrame.DATA_ROTATION`
/// and — under `FLOAT`, so not actually a collision — `VehicleEntity
/// .DATA_ID_DAMAGE`. No census column separates a frame from a display entity
/// (neither is living, neither is a mob), so this is gated on
/// [`MetadataClass::ItemFrame`].
///
/// Note the frame's *own* fields start at 9, not 8: `HangingEntity
/// .DATA_DIRECTION` takes index 8, which this decoder consumes for alignment
/// (`SER_DIRECTION`) and does not surface — the direction is recoverable from
/// the yaw/pitch `ItemFrame.setDirection` derives from it and puts on every
/// spawn and move packet.
const IDX_ITEM_FRAME_ROTATION: u8 = 10;

/// `Display.DATA_TRANSLATION_ID` (`Display.java`), index 11 — a `VECTOR3`.
/// Self-identifying: no other claimant at index 11 in the 26.2 jar dump uses
/// that serializer (see `EntityMetadataUpdate::display_translation`'s doc in
/// `lodestone-model` for the full argument), so this needs no class guard.
const IDX_DISPLAY_TRANSLATION: u8 = 11;
/// `Display.DATA_SCALE_ID`, index 12 — a `VECTOR3`, self-identifying for the
/// same reason as [`IDX_DISPLAY_TRANSLATION`].
const IDX_DISPLAY_SCALE: u8 = 12;
/// `Display.DATA_LEFT_ROTATION_ID`, index 13 — a `QUATERNION`,
/// self-identifying: no other claimant at index 13 uses that serializer.
/// Applied **before** scale (`Transformation.compose`).
const IDX_DISPLAY_LEFT_ROTATION: u8 = 13;
/// `Display.DATA_RIGHT_ROTATION_ID`, index 14 — a `QUATERNION`,
/// self-identifying for the same reason as [`IDX_DISPLAY_LEFT_ROTATION`].
/// Applied **after** scale.
const IDX_DISPLAY_RIGHT_ROTATION: u8 = 14;
/// `Display.DATA_BILLBOARD_RENDER_CONSTRAINTS_ID`, index 15 — a `BYTE`.
///
/// **This index is ambiguous and does need a class guard**, unlike the four
/// translation/scale/rotation fields above: it is the same wire index as
/// [`IDX_MOB_FLAGS`] (`Mob.DATA_MOB_FLAGS_ID`) and
/// `ArmorStand.DATA_CLIENT_FLAGS`, all three `BYTE`. Gated on
/// [`is_display_class`] rather than a single `MetadataClass` variant because
/// all three `Display` subtypes carry this exact field at this exact index.
const IDX_DISPLAY_BILLBOARD: u8 = 15;
/// `Display.DATA_BRIGHTNESS_OVERRIDE_ID`, index 16 — an `INT` carrying
/// `Brightness.pack()`'s `block << 4 | sky << 20`, or `-1` for "no override".
///
/// **Ambiguous, and gated on [`is_display_class`].** The committed jar dump
/// lists six `INT` claimants at index 16 — `Creeper.DATA_SWELL_DIR`
/// ([`IDX_CREEPER_SWELL_DIR`]), `EnderDragon.DATA_PHASE`
/// ([`IDX_DRAGON_PHASE`]), `Phantom.ID_SIZE`, `Warden.CLIENT_ANGER_LEVEL`,
/// `WitherBoss.DATA_TARGET_A` and this one. None of the other five is a
/// `Display` subtype, so the *whole-family* guard separates them; a
/// per-subtype guard would need three arms for one field declared once on the
/// base class.
const IDX_DISPLAY_BRIGHTNESS: u8 = 16;
/// The per-variant payload every `Display` subtype carries at index 23:
/// `Display.BlockDisplay.DATA_BLOCK_STATE_ID` (`BLOCK_STATE`),
/// `Display.ItemDisplay.DATA_ITEM_STACK_ID` (`ITEM_STACK`, self-identifying
/// and handled before the index match — see [`read_entity_metadata`]'s early
/// return), or `Display.TextDisplay.DATA_TEXT_ID` (`COMPONENT`).
///
/// **Only the block-state arm needs a class guard.** `Cat.DATA_COLLAR_COLOR`
/// is the other `INT`-shaped claimant at this index (block-state ids decode
/// to a plain integer, same shape as any other `INT`), so an ungated arm
/// would read a cat's dye ordinal as a block-state id. The `COMPONENT` arm
/// carries a guard too, for consistency with every other `Display` field in
/// this module, though the jar dump shows no real collision at this index.
const IDX_DISPLAY_VARIANT_PAYLOAD: u8 = 23;
/// The per-variant *second* payload every `Display` subtype but `BlockDisplay`
/// carries at index 24: `Display.ItemDisplay.DATA_ITEM_DISPLAY_ID` (`BYTE`,
/// the `ItemDisplayContext` ordinal) or `Display.TextDisplay.DATA_LINE_WIDTH_ID`
/// (`INT`, the wrap width in pixels). Self-identifying by value shape (no
/// other claimant at index 24 in the jar dump is a bare `BYTE` or `INT`), but
/// guarded by class anyway for the same consistency reason as
/// [`IDX_DISPLAY_VARIANT_PAYLOAD`]'s `COMPONENT` arm.
const IDX_DISPLAY_VARIANT_EXTRA: u8 = 24;
/// `Display.TextDisplay.DATA_BACKGROUND_COLOR_ID`, index 25 — a packed ARGB
/// `INT`. The sole claimant of this index in the jar dump.
const IDX_TEXT_BACKGROUND_COLOR: u8 = 25;
/// `Display.TextDisplay.DATA_TEXT_OPACITY_ID`, index 26 — a `BYTE`. The sole
/// claimant of this index in the jar dump.
const IDX_TEXT_OPACITY: u8 = 26;
/// `Display.TextDisplay.DATA_STYLE_FLAGS_ID`, index 27 — a `BYTE`. The sole
/// claimant of this index in the jar dump.
const IDX_TEXT_STYLE_FLAGS: u8 = 27;

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
    /// `EnderDragon` — gates index 16's `INT` against the five other unrelated
    /// `INT` claimants at that index. See [`IDX_DRAGON_PHASE`].
    Dragon,
    /// `EndCrystal` — gates index 9's `BOOLEAN` against
    /// `AreaEffectCloud.DATA_WAITING`/`FishingHook.DATA_BITING`, the other two
    /// claimants at that index. See [`IDX_CRYSTAL_SHOW_BOTTOM`]. (Index 8's
    /// `OPTIONAL_BLOCK_POS` beam target does not need this class — see
    /// [`IDX_CRYSTAL_BEAM_TARGET`]'s own doc for why the index alone already
    /// disambiguates it.)
    EndCrystal,
    /// `ArmorStand` — gates index 15's `BYTE` against `Mob.DATA_MOB_FLAGS_ID`,
    /// the other claimant at that index (see [`IDX_MOB_FLAGS`]). Unlike the
    /// `Sheep`/`Horse`/`Tamable` variant classes, this is not a cosmetic
    /// appearance — it is the small/show-arms/no-base-plate/marker byte a
    /// "hologram" (an invisible, nametagged armour stand) needs alongside the
    /// shared-flags invisible bit and the custom-name pair.
    ArmorStand,
    /// `Display.TextDisplay` — gates index 23's `COMPONENT` (the text itself)
    /// and 24-27 (line width, background colour, opacity, style flags), none
    /// of which any other entity type carries at those indices in the 26.2
    /// jar dump. Kept as its own variant rather than a shared `Display`
    /// variant because indices 23/24 decode to a genuinely different shape
    /// per subtype — see [`ItemDisplay`](Self::ItemDisplay)/
    /// [`BlockDisplay`](Self::BlockDisplay).
    TextDisplay,
    /// `Display.ItemDisplay` — gates index 24's `BYTE` (`ItemDisplayContext`
    /// ordinal). Index 23's item stack needs no class guard: it is
    /// self-identifying by the `ITEM_STACK` serializer, handled before the
    /// index match ever runs (see [`read_entity_metadata`]'s early return).
    ItemDisplay,
    /// `Display.BlockDisplay` — gates index 23's `BLOCK_STATE` against
    /// `Cat.DATA_COLLAR_COLOR`, the other `INT`-shaped claimant at that index
    /// (see [`IDX_DISPLAY_VARIANT_PAYLOAD`]'s doc).
    BlockDisplay,
    /// `ItemFrame`/`GlowItemFrame` — gates index 10's `INT`
    /// (`ItemFrame.DATA_ROTATION`) against the other `INT` claimants at that
    /// index. See [`IDX_ITEM_FRAME_ROTATION`].
    ///
    /// A frame's *stack* needs no class: `ItemFrame.DATA_ITEM` is an
    /// `ITEM_STACK` and is therefore self-identifying by serializer, handled
    /// before the index match runs — which is why a chest in a frame already
    /// drew while its rotation did not.
    ItemFrame,
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
        "minecraft:ender_dragon" => Some(MetadataClass::Dragon),
        "minecraft:end_crystal" => Some(MetadataClass::EndCrystal),
        "minecraft:armor_stand" => Some(MetadataClass::ArmorStand),
        "minecraft:text_display" => Some(MetadataClass::TextDisplay),
        "minecraft:item_display" => Some(MetadataClass::ItemDisplay),
        "minecraft:block_display" => Some(MetadataClass::BlockDisplay),
        "minecraft:item_frame" | "minecraft:glow_item_frame" => Some(MetadataClass::ItemFrame),
        _ => None,
    }
}

/// Whether `class` is one of the three `Display` subtypes — the gate for the
/// fields every subtype shares (billboard mode; translation/scale/rotation
/// are self-identifying by value shape and need no such gate, see
/// [`EntityMetadataUpdate::display_translation`](lodestone_model::EntityMetadataUpdate::display_translation)'s
/// doc).
#[must_use]
fn is_display_class(class: Option<MetadataClass>) -> bool {
    matches!(
        class,
        Some(MetadataClass::TextDisplay | MetadataClass::ItemDisplay | MetadataClass::BlockDisplay)
    )
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
/// `EntityDataSerializers.PAINTING_VARIANT`. One claimant in the whole 26.2
/// dump — `Painting.DATA_PAINTING_VARIANT_ID` — which is why the value it
/// produces needs no index or class guard.
const SER_PAINTING_VARIANT: i32 = 34;
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
    ///
    /// Carries the full styled component tree — see
    /// [`EntityMetadataUpdate::custom_name`]'s doc for why this is `Text`
    /// rather than a flattened plain string.
    OptText(Option<Text>),
    /// A pose enum id.
    Pose(u32),
    /// A decoded `OPTIONAL_BLOCK_POS` value — `None` for vanilla's own
    /// "cleared"/absent sentinel. Surfaced for [`IDX_CRYSTAL_BEAM_TARGET`]
    /// alone; the other two claimants of this serializer
    /// (`LivingEntity.SLEEPING_POS_ID`, `Creaking.HOME_POS`) decode to this
    /// same shape but are filtered out by index at the call site, not here —
    /// see that constant's own doc for why the serializer alone cannot do it.
    OptBlockPos(Option<BlockPos>),
    /// A resolved registry-holder appearance variant (cat, cow, wolf, …).
    Keyed(Identifier),
    /// A decoded `ROTATIONS` — an `(x, y, z)` triple of Euler **degrees**,
    /// already reduced by `Rotations`' own compact constructor. The six
    /// `ArmorStand` pose accessors are this serializer's only claimants in the
    /// jar dump, so no class guard gates the arms that read it; see
    /// [`IDX_ARMOR_STAND_HEAD_POSE`].
    Rotations(Vec3f),
    /// A decoded `VECTOR3` — `Display.DATA_TRANSLATION_ID`/`DATA_SCALE_ID`,
    /// the only claimants of that serializer in the 26.2 jar dump. See
    /// [`IDX_DISPLAY_TRANSLATION`]/[`IDX_DISPLAY_SCALE`].
    Vector3(Vec3f),
    /// A decoded `QUATERNION` — `Display.DATA_LEFT_ROTATION_ID`/
    /// `DATA_RIGHT_ROTATION_ID`, the only claimants of that serializer. See
    /// [`IDX_DISPLAY_LEFT_ROTATION`]/[`IDX_DISPLAY_RIGHT_ROTATION`].
    Quaternion(Quat),
    /// A decoded `COMPONENT` (not the `OPTIONAL_COMPONENT` [`OptText`](Self::OptText)
    /// already carries), styled the same way `OptText` is. Surfaced for
    /// `Display.TextDisplay.DATA_TEXT_ID` alone; the other `COMPONENT`
    /// claimant in the jar dump (`MinecartCommandBlock.DATA_ID_LAST_OUTPUT`,
    /// a different index) is filtered out by index at the call site, not
    /// here — same pattern as [`OptBlockPos`](Self::OptBlockPos).
    Text(Text),
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
    /// A `Holder<PaintingVariant>` already resolved to its registry key.
    ///
    /// Carries no index because it needs none: `PAINTING_VARIANT` has exactly
    /// one claimant in the 26.2 entity-data dump, so the serializer alone
    /// identifies the field — the same property that lets [`Value::Item`] be
    /// routed ahead of the index match.
    PaintingVariant(Identifier),
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

/// Unpacks a vanilla `BlockPos.asLong` value into canonical block coordinates:
/// `x` in the high 26 bits, `z` in the middle 26 bits, `y` in the low 12
/// bits, each two's-complement. A local duplicate of `adapter::unpack_block_pos`/
/// `server_protocol::unpack_block_pos` — both are private to their own
/// modules, and this one is small enough that sharing it is not worth a
/// public seam across three call sites.
fn unpack_block_pos(packed: i64) -> BlockPos {
    let x = (packed >> 38) as i32;
    let y = ((packed << 52) >> 52) as i32;
    let z = ((packed << 26) >> 38) as i32;
    BlockPos::new(x, y, z)
}

/// `Rotations`' compact constructor, applied per component: a non-finite value
/// becomes `0.0`, a finite one is reduced modulo 360.
///
/// Vanilla runs this inside the record's own constructor, so **every**
/// `Rotations` the client holds has already been through it — including the ones
/// its stream codec decodes straight off the wire. Reproducing it at the decode
/// site is what keeps a hostile or buggy server from putting a `NaN` into a part
/// rotation, where it would poison every matrix composed from that part rather
/// than merely mispose it.
///
/// The modulo alone is cosmetically inert (a rotation of 720° draws as 0°); it is
/// transcribed anyway because it is half of one expression, and splitting a
/// ported formula on "which half is observable" is how a later reader ends up
/// re-deriving the whole thing.
fn rotations_component(raw: f32) -> f32 {
    if raw.is_finite() { raw % 360.0 } else { 0.0 }
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
        // Surfaced as a styled component for `Display.TextDisplay.DATA_TEXT_ID`
        // — every other `COMPONENT` claimant (`MinecartCommandBlock`'s last
        // output) is filtered out by index at the call site, matching
        // `OPTIONAL_BLOCK_POS`'s pattern. See [`Value::Text`].
        //
        // `Text::from_nbt` rather than `plain_text_from_nbt_component`: the
        // latter flattens colour/bold/italic/underline/strikethrough away at
        // decode time, before anything downstream ever sees them.
        SER_COMPONENT => {
            let component = read_network_nbt(reader)?;
            Value::Text(Text::from_nbt(&component))
        }
        SER_OPTIONAL_COMPONENT => {
            if reader.bool()? {
                let component = read_network_nbt(reader)?;
                Value::OptText(Some(Text::from_nbt(&component)))
            } else {
                Value::OptText(None)
            }
        }
        SER_BOOLEAN => Value::Bool(reader.bool()?),
        // An armour stand's six pose accessors, `ArmorStand.DATA_HEAD_POSE`
        // through `DATA_RIGHT_LEG_POSE` (indices 16-21) — each an `(x, y, z)`
        // triple of Euler **degrees**. They are the *only* claimants of this
        // serializer in the jar dump, so the value shape alone identifies the
        // field family and the call site's arms need no class guard, exactly
        // as for `VECTOR3`/`QUATERNION`; the index still separates the six
        // parts from each other. See [`IDX_ARMOR_STAND_HEAD_POSE`].
        //
        // The canonicalisation mirrors `Rotations`' own compact constructor,
        // which is applied on construction and therefore on every value the
        // client ever holds: a non-finite component becomes `0.0`, and a
        // finite one is reduced modulo 360. The modulo is cosmetically inert
        // for a rotation; the non-finite clamp is not — a `NaN` reaching the
        // rig poisons every matrix composed from it, and this is the single
        // place that can stop it.
        SER_ROTATIONS => {
            let x = rotations_component(reader.f32()?);
            let y = rotations_component(reader.f32()?);
            let z = rotations_component(reader.f32()?);
            Value::Rotations(Vec3f::new(x, y, z))
        }
        SER_BLOCK_POS => {
            reader.i64()?;
            Value::Consumed
        }
        SER_OPTIONAL_BLOCK_POS => {
            if reader.bool()? {
                Value::OptBlockPos(Some(unpack_block_pos(reader.i64()?)))
            } else {
                Value::OptBlockPos(None)
            }
        }
        SER_DIRECTION | SER_OPTIONAL_BLOCK_STATE | SER_OPTIONAL_UNSIGNED_INT | SER_HUMANOID_ARM => {
            reader.var_i32()?;
            Value::Consumed
        }
        // The global block-state id, as a plain `VarInt` — surfaced as
        // `Value::Int` (the same shape any other `INT` field decodes to)
        // rather than a dedicated variant, since nothing about the wire
        // representation is special. Its only surfaced claimant is
        // `Display.BlockDisplay.DATA_BLOCK_STATE_ID` at index 23, gated on
        // [`MetadataClass::BlockDisplay`] in the caller because index 23 also
        // carries `Cat.DATA_COLLAR_COLOR`, an unrelated `INT` — see
        // [`IDX_DISPLAY_VARIANT_PAYLOAD`]. `PrimedTnt.DATA_BLOCK_STATE_ID`
        // (index 9) uses this same serializer and is intentionally left
        // unsurfaced (no arm claims `(9, Value::Int(_))`), matching this
        // module's existing "decoded for alignment but not surfaced" pattern.
        SER_BLOCK_STATE => Value::Int(reader.var_i32()?),
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
        // A `Holder<PaintingVariant>`: wire value is `id + 1`, with 0 meaning
        // an inline direct value vanilla never sends for a painting. An id the
        // table does not cover is a data-pack variant with no size and no
        // texture here, so it stays aligned and raises nothing rather than
        // naming some other painting.
        SER_PAINTING_VARIANT => {
            let id = reader.var_i32()? - 1;
            match entity_variants::painting_variant(id) {
                Some(key) => Value::PaintingVariant(parse_identifier(key)?),
                None => Value::Consumed,
            }
        }
        22 | 24 | 26 | 29 | 31 | 35..=38 => {
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
        // `Display.DATA_TRANSLATION_ID`/`DATA_SCALE_ID` — the only claimants
        // of this serializer in the jar dump, so no index/class filtering is
        // needed here; the caller's `(index, Value::Vector3(_))` match arms
        // still key on the index to tell translation from scale.
        SER_VECTOR3 => {
            let x = reader.f32()?;
            let y = reader.f32()?;
            let z = reader.f32()?;
            Value::Vector3(Vec3f::new(x, y, z))
        }
        // `Display.DATA_LEFT_ROTATION_ID`/`DATA_RIGHT_ROTATION_ID` — the only
        // claimants of this serializer. Wire order is `x, y, z, w`
        // (`FriendlyByteBuf.readQuaternion`: `new Quaternionf(x, y, z, w)`),
        // matching `Quat::new`'s own field order exactly.
        SER_QUATERNION => {
            let x = reader.f32()?;
            let y = reader.f32()?;
            let z = reader.f32()?;
            let w = reader.f32()?;
            Value::Quaternion(Quat::new(x, y, z, w))
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
        // Routed ahead of the index match for the same reason the item stack
        // above is, and it is the same reason: one serializer, one claimant in
        // the jar dump, so the index carries no information. (It is 9 on a
        // painting; nothing here needs to know that, and
        // `painting_variant_is_the_only_claimant_of_its_serializer` asserts
        // both facts against the jar dump, so the index stays checked without
        // being depended on.)
        if let Value::PaintingVariant(key) = value {
            md.painting_variant = Some(key);
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
            // The *other* claimant of index 15: `ArmorStand.DATA_CLIENT_FLAGS`,
            // gated on `class` rather than `mob` because `mob` is exactly what
            // this claimant is not — see `IDX_MOB_FLAGS`'s doc. This is the byte
            // a "hologram" (an invisible, nametagged armour stand) needs for its
            // marker/no-base-plate/show-arms cosmetics; see
            // `lodestone_entity::metadata::ArmorStandFlags`.
            (IDX_MOB_FLAGS, Value::Byte(b)) if class == Some(MetadataClass::ArmorStand) => {
                md.armor_stand_flags = Some(b as u8);
            }
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
            // `EnderDragon.DATA_PHASE`. Guarded on class: index 16 is an `INT`
            // on five other unrelated mobs too — see [`IDX_DRAGON_PHASE`].
            (IDX_DRAGON_PHASE, Value::Int(v)) if class == Some(MetadataClass::Dragon) => {
                md.dragon_phase = Some(v);
            }
            // `EndCrystal.DATA_SHOW_BOTTOM`. Guarded on class: index 9 is a
            // `BOOLEAN` on two other unrelated entities too — see
            // [`IDX_CRYSTAL_SHOW_BOTTOM`].
            (IDX_CRYSTAL_SHOW_BOTTOM, Value::Bool(b)) if class == Some(MetadataClass::EndCrystal) => {
                md.crystal_show_bottom = Some(b);
            }
            // `ItemFrame.DATA_ROTATION`. Guarded on class: index 10's `INT` is
            // also a `Display`'s interpolation duration — see
            // [`IDX_ITEM_FRAME_ROTATION`]. Masked to `0..8` here rather than at
            // the consumer, matching `ItemFrame.setRotation`'s own `% 8`; a
            // negative or out-of-range int is a datapack, not a rotation.
            (IDX_ITEM_FRAME_ROTATION, Value::Int(v)) if class == Some(MetadataClass::ItemFrame) => {
                md.item_frame_rotation = Some((v.rem_euclid(8)) as u8);
            }
            // `ArmorStand`'s six pose accessors. No class guard: `ROTATIONS` has
            // exactly these six claimants in the jar dump, so the value shape
            // alone establishes the family and the index only says which part
            // moved — see [`IDX_ARMOR_STAND_HEAD_POSE`] for why adding a guard
            // here would be a regression rather than belt-and-braces.
            //
            // Six fields rather than one merged pose because a metadata packet
            // mentions only the accessors that changed; the merge onto the
            // previous pose happens where that previous pose exists. See
            // [`lodestone_model::ArmorStandPose`].
            (IDX_ARMOR_STAND_HEAD_POSE, Value::Rotations(r)) => md.armor_stand_pose.head = Some(r),
            (IDX_ARMOR_STAND_BODY_POSE, Value::Rotations(r)) => md.armor_stand_pose.body = Some(r),
            (IDX_ARMOR_STAND_LEFT_ARM_POSE, Value::Rotations(r)) => {
                md.armor_stand_pose.left_arm = Some(r);
            }
            (IDX_ARMOR_STAND_RIGHT_ARM_POSE, Value::Rotations(r)) => {
                md.armor_stand_pose.right_arm = Some(r);
            }
            (IDX_ARMOR_STAND_LEFT_LEG_POSE, Value::Rotations(r)) => {
                md.armor_stand_pose.left_leg = Some(r);
            }
            (IDX_ARMOR_STAND_RIGHT_LEG_POSE, Value::Rotations(r)) => {
                md.armor_stand_pose.right_leg = Some(r);
            }
            // `EndCrystal.DATA_BEAM_TARGET`. No class guard: the index already
            // disambiguates (see [`IDX_CRYSTAL_BEAM_TARGET`]'s own doc for why
            // the bare serializer could not).
            (IDX_CRYSTAL_BEAM_TARGET, Value::OptBlockPos(pos)) => {
                md.crystal_beam_target = Reported::Reported(pos);
            }
            // `Display.DATA_TRANSLATION_ID`/`DATA_SCALE_ID`/
            // `DATA_LEFT_ROTATION_ID`/`DATA_RIGHT_ROTATION_ID`. No class guard:
            // the `VECTOR3`/`QUATERNION` value shape already disambiguates —
            // see [`IDX_DISPLAY_TRANSLATION`]'s doc.
            (IDX_DISPLAY_TRANSLATION, Value::Vector3(v)) => md.display_translation = Some(v),
            (IDX_DISPLAY_SCALE, Value::Vector3(v)) => md.display_scale = Some(v),
            (IDX_DISPLAY_LEFT_ROTATION, Value::Quaternion(q)) => md.display_left_rotation = Some(q),
            (IDX_DISPLAY_RIGHT_ROTATION, Value::Quaternion(q)) => md.display_right_rotation = Some(q),
            // `Display.DATA_BILLBOARD_RENDER_CONSTRAINTS_ID`. Guarded on
            // [`is_display_class`]: index 15's `BYTE` is also `Mob.DATA_MOB_FLAGS_ID`
            // and `ArmorStand.DATA_CLIENT_FLAGS` — see [`IDX_DISPLAY_BILLBOARD`].
            (IDX_DISPLAY_BILLBOARD, Value::Byte(b)) if is_display_class(class) => {
                md.display_billboard = Some(b as u8);
            }
            // `Display.DATA_BRIGHTNESS_OVERRIDE_ID`. Guarded on
            // [`is_display_class`] for the reason [`IDX_DISPLAY_BRIGHTNESS`]
            // gives: five other unrelated entities put an `INT` at index 16.
            // Surfaced packed, sentinel and all — the consumer needs to tell
            // `-1` ("no override") from a real `(block 0, sky 0)`, which packs
            // to `0`.
            (IDX_DISPLAY_BRIGHTNESS, Value::Int(v)) if is_display_class(class) => {
                md.display_brightness_override = Some(v);
            }
            // `Display.BlockDisplay.DATA_BLOCK_STATE_ID`. Guarded on class: index
            // 23 is also `Cat.DATA_COLLAR_COLOR`, an unrelated `INT` — see
            // [`IDX_DISPLAY_VARIANT_PAYLOAD`].
            (IDX_DISPLAY_VARIANT_PAYLOAD, Value::Int(v)) if class == Some(MetadataClass::BlockDisplay) => {
                md.display_block_state = Some(v as u32);
            }
            // `Display.TextDisplay.DATA_TEXT_ID`.
            (IDX_DISPLAY_VARIANT_PAYLOAD, Value::Text(text)) if class == Some(MetadataClass::TextDisplay) => {
                md.display_text = Reported::Reported(Some(text));
            }
            // `Display.ItemDisplay.DATA_ITEM_DISPLAY_ID` — the `ItemDisplayContext`
            // ordinal this item poses in.
            (IDX_DISPLAY_VARIANT_EXTRA, Value::Byte(b)) if class == Some(MetadataClass::ItemDisplay) => {
                md.display_item_context = Some(b as u8);
            }
            // `Display.TextDisplay.DATA_LINE_WIDTH_ID`.
            (IDX_DISPLAY_VARIANT_EXTRA, Value::Int(v)) if class == Some(MetadataClass::TextDisplay) => {
                md.display_line_width = Some(v);
            }
            // `Display.TextDisplay.DATA_BACKGROUND_COLOR_ID`. Sole claimant of
            // this index in the jar dump — see [`IDX_TEXT_BACKGROUND_COLOR`].
            (IDX_TEXT_BACKGROUND_COLOR, Value::Int(v)) if class == Some(MetadataClass::TextDisplay) => {
                md.display_background_color = Some(v);
            }
            // `Display.TextDisplay.DATA_TEXT_OPACITY_ID`.
            (IDX_TEXT_OPACITY, Value::Byte(b)) if class == Some(MetadataClass::TextDisplay) => {
                md.display_text_opacity = Some(b);
            }
            // `Display.TextDisplay.DATA_STYLE_FLAGS_ID`.
            (IDX_TEXT_STYLE_FLAGS, Value::Byte(b)) if class == Some(MetadataClass::TextDisplay) => {
                md.display_text_style_flags = Some(b as u8);
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

/// Encodes an `update_attributes` packet — the inverse of
/// [`read_update_attributes`], and the wire producer that packet never had
/// (the decoder existed for a real vanilla server's own `update_attributes`;
/// nothing in this workspace built the byte string until the integrated
/// server needed to send one for the armour bar).
///
/// Each snapshot's canonical attribute id is resolved back to its network id
/// through [`attribute_id`]; a snapshot naming an attribute this crate's
/// table does not know is skipped rather than failing the whole packet, and
/// the written count reflects what was actually written, not `attributes.len()`.
///
/// A caller that has already folded equipment into one number (as
/// `lodestone-server`'s attribute sync does — see that crate's
/// `player_attribute_snapshots`) passes an empty modifier list per snapshot;
/// the client's own fold (`instance_from_snapshot`/`AttributeInstance::value`)
/// is a no-op over a bare base value with no modifiers, so it lands on the
/// same number the server already computed.
pub fn write_update_attributes(w: &mut Writer, entity_id: i32, attributes: &[EntityAttributeSnapshot]) {
    let resolved: Vec<(i32, &EntityAttributeSnapshot)> = attributes
        .iter()
        .filter_map(|snapshot| attribute_id(&snapshot.attribute.to_string()).map(|id| (id, snapshot)))
        .collect();
    w.var_i32(entity_id);
    w.var_i32(resolved.len() as i32);
    for (attribute_network_id, snapshot) in resolved {
        w.var_i32(attribute_network_id);
        w.f64(snapshot.base);
        w.var_i32(snapshot.modifiers.len() as i32);
        for modifier in &snapshot.modifiers {
            w.string(&modifier.id.to_string());
            w.f64(modifier.amount);
            w.var_i32(i32::from(modifier.operation));
        }
    }
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

    /// A **living non-mob whose class this decoder does not resolve** — a
    /// player or a mannequin, not an armour stand (which has its own fixture,
    /// [`an_armor_stand`], now that its class is [`MetadataClass::ArmorStand`]).
    /// The population index 15's `mob` guard excludes, and the reason that
    /// guard cannot be `living`: a record shaped exactly like this is what an
    /// armour stand's own `TrackedEntity` used to be, before this module could
    /// tell the two apart.
    fn a_living_non_mob() -> TrackedEntity {
        TrackedEntity {
            class: None,
            living: true,
            mob: false,
        }
    }

    /// An armour stand: living, not a `Mob`, and — the fact that lets index 15
    /// resolve to `armor_stand_flags` instead of being dropped — its own
    /// [`MetadataClass::ArmorStand`].
    fn an_armor_stand() -> TrackedEntity {
        TrackedEntity {
            class: Some(MetadataClass::ArmorStand),
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
            Reported::Reported(Some(Text::literal("Hoglet")))
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

    /// The control this module's style fix exists to satisfy: a nested,
    /// styled `text_display` component must survive decode with its colour
    /// and formatting intact, not flattened to plain text.
    ///
    /// Before `SER_COMPONENT`/`SER_OPTIONAL_COMPONENT` were switched from
    /// `plain_text_from_nbt_component` to `Text::from_nbt`, this assertion
    /// failed: `md.display_text` held `Reported(Some(Text::literal("RBL")))`
    /// — every colour and the bold flag silently dropped, exactly the report
    /// this module exists to fix. Watched failing that way before this fix
    /// landed.
    ///
    /// The fixture is deliberately discriminating, not a flat coloured
    /// string: a root styled `red`, one `extra` child that is `bold` and
    /// inherits the root's colour (no colour of its own), and a second
    /// `extra` child that overrides the colour to `blue` and stays
    /// non-bold. A single flat string cannot distinguish "style threaded
    /// through" from "style discarded then re-applied uniformly" — this can.
    #[test]
    fn decodes_styled_nested_text_display_component_with_inheritance() {
        use lodestone_core::{Nbt, NbtTag, write_network_nbt};
        use lodestone_model::{TextColor, TextContent};

        let component = Nbt::Compound(vec![
            ("color".to_owned(), Nbt::String("red".to_owned())),
            ("text".to_owned(), Nbt::String("R".to_owned())),
            (
                "extra".to_owned(),
                Nbt::List {
                    element_type: NbtTag::Compound,
                    elements: vec![
                        Nbt::Compound(vec![
                            ("bold".to_owned(), Nbt::Byte(1)),
                            ("text".to_owned(), Nbt::String("B".to_owned())),
                        ]),
                        Nbt::Compound(vec![
                            ("color".to_owned(), Nbt::String("blue".to_owned())),
                            ("text".to_owned(), Nbt::String("L".to_owned())),
                        ]),
                    ],
                },
            ),
        ]);
        let mut nbt_writer = Writer::default();
        write_network_nbt(&mut nbt_writer, &component).expect("encode succeeds");

        let mut bytes = Vec::new();
        bytes.push(IDX_DISPLAY_VARIANT_PAYLOAD);
        bytes.extend(varint(SER_COMPONENT));
        bytes.extend(nbt_writer.into_vec());
        bytes.push(EOF_MARKER);

        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(
            &mut reader,
            TrackedEntity {
                class: Some(MetadataClass::TextDisplay),
                living: false,
                mob: false,
            },
        )
        .expect("decode")
        .metadata;
        reader.ensure_empty().expect("no trailing bytes");

        let Reported::Reported(Some(text)) = md.display_text else {
            panic!("text_display component was not surfaced: {:?}", md.display_text);
        };

        // The tree itself, not a flattened string: the root carries its own
        // colour and no bold, and the two `extra` children are still two
        // distinct nodes rather than concatenated text.
        assert_eq!(text.content, TextContent::Literal("R".to_owned()));
        assert_eq!(text.style.color, Some(TextColor::Red));
        assert_eq!(text.style.bold, None);
        assert_eq!(text.extra.len(), 2);
        assert_eq!(text.extra[0].content, TextContent::Literal("B".to_owned()));
        assert_eq!(text.extra[0].style.bold, Some(true));
        assert_eq!(
            text.extra[1].content,
            TextContent::Literal("L".to_owned())
        );
        assert_eq!(text.extra[1].style.color, Some(TextColor::Blue));

        // `to_spans` resolves inheritance down the tree: the bold child has
        // no colour of its own and must inherit the root's red, while the
        // second child's own blue overrides it. This is the shape a plain
        // `String` could never have carried.
        let spans = text.to_spans();
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].text, "R");
        assert_eq!(spans[0].style.color, Some(TextColor::Red));
        assert_eq!(spans[1].text, "B");
        assert_eq!(
            spans[1].style.color,
            Some(TextColor::Red),
            "a child with no colour of its own must inherit the parent's"
        );
        assert_eq!(spans[1].style.bold, Some(true));
        assert_eq!(spans[2].text, "L");
        assert_eq!(spans[2].style.color, Some(TextColor::Blue));
        assert_eq!(spans[2].style.bold, None);
    }

    /// An `item_display`'s stack arrives at index **23**, not the index 8 a
    /// dropped item uses, and must still reach `md.item`.
    ///
    /// # What the expected value is, and what it is not
    ///
    /// This does **not** claim to validate the stack codec — that is what the
    /// captured-fixture gates in `tests/item_entity_metadata.rs` are for, and
    /// bytes built here with our own reading of the format could only close
    /// over a shared misunderstanding of it. The claim under test is purely
    /// about **routing**: `read_entity_metadata` handles `SER_ITEM_STACK`
    /// *before* the index match and therefore surfaces it whatever index it
    /// arrives at, which is the only reason an `item_display` has a stack to
    /// draw at all. The item id comes from `lodestone_data::items::item_id`,
    /// which is generated from Mojang's own `registries.json`.
    ///
    /// The discriminating half is the trailing field: index 23's `Int` arm is
    /// gated on `BlockDisplay`, so an `ItemDisplay` subject proves the stack
    /// took the early return rather than falling into the block-state arm, and
    /// the following index-24 byte proves the reader stayed aligned.
    #[test]
    fn an_item_displays_stack_arrives_at_index_23_and_still_reaches_the_item_field() {
        let diamond = lodestone_data::items::item_id("minecraft:diamond")
            .expect("minecraft:diamond is in the generated item registry");

        let mut bytes = Vec::new();
        bytes.push(IDX_DISPLAY_VARIANT_PAYLOAD);
        bytes.extend(varint(SER_ITEM_STACK));
        bytes.extend(varint(1)); // count
        bytes.extend(varint(diamond)); // item registry id
        bytes.extend(varint(0)); // components added
        bytes.extend(varint(0)); // components removed
        // A second field after it, so a mis-sized stack read shows up as a
        // decode failure here rather than as a silently truncated list.
        bytes.push(IDX_DISPLAY_VARIANT_EXTRA);
        bytes.extend(varint(SER_BYTE));
        bytes.push(8); // ItemDisplayContext.FIXED
        bytes.push(EOF_MARKER);

        let mut reader = Reader::new(&bytes);
        let decoded = read_entity_metadata(
            &mut reader,
            TrackedEntity {
                class: Some(MetadataClass::ItemDisplay),
                living: false,
                mob: false,
            },
        )
        .expect("decode");
        reader.ensure_empty().expect("no trailing bytes");
        assert!(decoded.complete);
        let md = decoded.metadata;

        let Reported::Reported(Some(stack)) = &md.item else {
            panic!("an item_display's index-23 stack never reached md.item: {:?}", md.item);
        };
        assert_eq!(stack.item.to_string(), "minecraft:diamond");
        assert_eq!(stack.count, 1);
        assert_eq!(
            md.display_item_context,
            Some(8),
            "the field after the stack must still decode, so the reader stayed aligned"
        );
        assert_eq!(
            md.display_block_state, None,
            "index 23's block-state arm must not also fire for an item_display"
        );
    }

    /// Index 16's `INT` is a display's brightness override **only** for a
    /// `Display` subtype; for anything else it is consumed for alignment and
    /// deliberately not surfaced.
    ///
    /// The three arms are the whole point, and the third is what a "does it
    /// decode" test would miss:
    ///
    /// * an `item_display` surfaces it, and surfaces *only* it;
    /// * a **creeper** — a real index-16 `INT` claimant
    ///   (`Creeper.DATA_SWELL_DIR`) whose premise
    ///   `the_jar_dump_contains_the_collisions_the_guards_exist_for` already
    ///   asserts — surfaces its own field and **not** the brightness, so the
    ///   guard is doing work rather than being decorative;
    ///   without it a creeper mid-swell would report a brightness override.
    /// * `-1` is a value the field really carries (vanilla's own
    ///   `NO_BRIGHTNESS_OVERRIDE`), so it must reach the consumer as
    ///   `Some(-1)` rather than being folded to `None` here — the consumer has
    ///   to tell "explicitly cleared" from "never reported".
    ///
    /// The packed fixture is `Brightness(block 7, sky 12).pack()`, i.e.
    /// `7 << 4 | 12 << 20` = `12583024`. Deliberately **not** a symmetric
    /// `(15, 15)`: the two nibbles differ, so an unpack that swaps them cannot
    /// pass.
    #[test]
    fn index_16_int_is_a_brightness_override_only_for_a_display() {
        const PACKED: i32 = (7 << 4) | (12 << 20);
        let payload = |value: i32| {
            let mut bytes = Vec::new();
            bytes.push(IDX_DISPLAY_BRIGHTNESS);
            bytes.extend(varint(SER_INT));
            bytes.extend(varint(value));
            bytes.push(EOF_MARKER);
            bytes
        };

        let bytes = payload(PACKED);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(
            &mut reader,
            TrackedEntity {
                class: Some(MetadataClass::ItemDisplay),
                living: false,
                mob: false,
            },
        )
        .expect("decode")
        .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.display_brightness_override, Some(PACKED));
        assert_eq!(
            md.creeper_swell_dir, None,
            "the same INT also landed in the creeper field"
        );
        assert_eq!(md.dragon_phase, None);

        // The control, and its premise is a real collision rather than an
        // invented one: `Creeper.DATA_SWELL_DIR` is an `INT` at index 16 in the
        // committed jar dump.
        let mut reader = Reader::new(&bytes);
        let control = read_entity_metadata(&mut reader, a_creeper())
            .expect("a creeper must still decode, not error")
            .metadata;
        reader
            .ensure_empty()
            .expect("the INT must be consumed for alignment even when it is not surfaced");
        assert_eq!(
            control.display_brightness_override, None,
            "a creeper's swell direction was surfaced as a brightness override"
        );
        assert_eq!(
            control.creeper_swell_dir,
            Some(PACKED),
            "the creeper arm must still fire for the same bytes"
        );

        // `-1` survives as a value, not as an absence.
        let cleared = payload(-1);
        let mut reader = Reader::new(&cleared);
        let md = read_entity_metadata(
            &mut reader,
            TrackedEntity {
                class: Some(MetadataClass::BlockDisplay),
                living: false,
                mob: false,
            },
        )
        .expect("decode")
        .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(
            md.display_brightness_override,
            Some(-1),
            "vanilla's own NO_BRIGHTNESS_OVERRIDE sentinel must reach the consumer"
        );
    }

    /// The premise of the arm above, read off the committed jar dump rather
    /// than hand-counted: `Display.DATA_BRIGHTNESS_OVERRIDE_ID` really is index
    /// 16 as an `INT`, and it really does share that index with five unrelated
    /// `INT`s, none of them a `Display`.
    ///
    /// Named individually rather than counted, matching
    /// `the_jar_dump_contains_the_collisions_the_guards_exist_for`: a dump that
    /// dropped one fails here instead of weakening the guard silently.
    #[test]
    fn the_dump_puts_the_brightness_override_at_index_16_beside_five_other_ints() {
        assert_eq!(
            dump_row("Display.DATA_BRIGHTNESS_OVERRIDE_ID"),
            (IDX_DISPLAY_BRIGHTNESS, SER_INT),
            "IDX_DISPLAY_BRIGHTNESS or its serializer disagrees with the jar"
        );
        let at_16 = dump_claimants(16);
        for owner in [
            "Creeper.DATA_SWELL_DIR",
            "EnderDragon.DATA_PHASE",
            "Phantom.ID_SIZE",
            "Warden.CLIENT_ANGER_LEVEL",
            "WitherBoss.DATA_TARGET_A",
        ] {
            assert!(
                at_16.contains(&(owner.to_owned(), SER_INT)),
                "index 16 does not claim {owner} as an INT in the dump; the is_display_class \
                 guard's premise is not what this test thinks it is"
            );
        }
        // And none of those five is a Display subtype, which is what makes the
        // family-wide guard sufficient — a per-subtype guard would need three
        // arms for one field declared once on the base class.
        for (owner, _) in &at_16 {
            assert!(
                !owner.starts_with("Display.") || owner == "Display.DATA_BRIGHTNESS_OVERRIDE_ID",
                "a second Display field claims index 16: {owner}"
            );
        }
    }

    /// Index 1, `INT`, decodes to `air_supply` — the field this seam exists to
    /// close (`docs/sky-and-air-bubbles.md`). Verified against `Entity.java`'s
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
    /// using-item bitfield behind a bow draw. Index verified against
    /// `LivingEntity.DATA_LIVING_ENTITY_FLAGS` being `LivingEntity`'s first
    /// `defineId`, not assumed from a summary.
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
    /// `0x04` is `isAggressive()` and therefore whether a skeleton draws its bow.
    /// The index comes from the jar dump, not a hand count; see
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
    /// (`ArmorStand.java`). An armour stand with arms is the ordinary
    /// decorative case, so a `living`-gated decode would report a large fraction
    /// of all armour stands as aggressive mobs and, holding a bow, draw it.
    ///
    /// Without the `if mob` guard this test fails with `left: Some(4), right:
    /// None` — run and watched.
    ///
    /// The subject here is deliberately **not** an armour stand — it has its
    /// own class now ([`MetadataClass::ArmorStand`], see
    /// `decodes_armor_stand_flags_at_index_15_for_an_armor_stand` below) — but a
    /// living non-mob whose class this decoder does not resolve at all (a
    /// player, a mannequin). The byte must still be consumed for alignment and
    /// still must not surface as `mob_flags`.
    #[test]
    fn index_15_on_an_unresolved_living_non_mob_is_consumed_but_not_surfaced() {
        let mut bytes = Vec::new();
        bytes.push(IDX_MOB_FLAGS);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x04);
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
            "an unresolved living non-mob's byte must not surface as mob flags"
        );
        assert_eq!(
            md.armor_stand_flags, None,
            "and must not surface as armour-stand flags either — we do not know \
             which of the two claimants at this index this byte actually is"
        );
        assert_eq!(md.health, Some(6.5), "the following field must still align");
    }

    /// Index 15, `BYTE`, on an **`ArmorStand`** decodes to `armor_stand_flags` —
    /// the byte a "hologram" needs for its marker/no-base-plate/show-arms
    /// cosmetics, alongside the shared-flags invisible bit and the custom-name
    /// pair. Before this arm existed the byte reached the final `_ => {}` arm
    /// and every armour stand's own cosmetics were silently dropped, even
    /// though the wire carried them end to end (`a_living_non_mob`, this
    /// module's stand-in for "an armour stand" before its class existed,
    /// documents the exact symptom this fixes).
    #[test]
    fn decodes_armor_stand_flags_at_index_15_for_an_armor_stand() {
        let mut bytes = Vec::new();
        bytes.push(IDX_MOB_FLAGS);
        bytes.extend(varint(SER_BYTE));
        bytes.push(0x18); // marker | no_base_plate, arms/small left off
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, an_armor_stand())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.armor_stand_flags, Some(0x18));
        // And it did not land in either of the byte's other claimants.
        assert_eq!(md.mob_flags, None);
        assert_eq!(md.living_flags, None);
    }

    /// All six `ROTATIONS` accessors, indices 16-21, decode to their own pose
    /// field — **not** into one another.
    ///
    /// # Why every one of the eighteen floats is distinct
    ///
    /// Six adjacent fields of one type, each three adjacent floats, is the
    /// worst possible shape for a transposition: swap any two and the wire
    /// stays byte-legal, our own round trip stays byte-perfect, and the only
    /// symptom is a stand whose left arm is where its right leg should be.
    /// Every value below is unique across the whole fixture, so no pair can be
    /// exchanged without an assertion moving — and the signs and magnitudes
    /// differ too, so a mirrored decode cannot pass either.
    ///
    /// The bytes are hand-built here rather than produced by an encoder of
    /// ours: there is no armour-stand pose *encoder* in this crate to round
    /// trip against, which is the honest form of "the expected value comes
    /// from outside the code under test" — the layout is `Rotations`'
    /// stream codec, three big-endian `f32`s, and nothing here can agree with
    /// a mistake in it.
    #[test]
    fn decodes_all_six_armor_stand_pose_accessors_into_distinct_fields() {
        // (index, x, y, z) — eighteen pairwise-distinct values.
        let fields: [(u8, f32, f32, f32); 6] = [
            (IDX_ARMOR_STAND_HEAD_POSE, 11.0, 12.0, 13.0),
            (IDX_ARMOR_STAND_BODY_POSE, 21.0, 22.0, 23.0),
            (IDX_ARMOR_STAND_LEFT_ARM_POSE, 31.0, 32.0, 33.0),
            (IDX_ARMOR_STAND_RIGHT_ARM_POSE, -41.0, -42.0, -43.0),
            (IDX_ARMOR_STAND_LEFT_LEG_POSE, 51.5, 52.5, 53.5),
            (IDX_ARMOR_STAND_RIGHT_LEG_POSE, -61.5, -62.5, -63.5),
        ];
        let mut bytes = Vec::new();
        for (index, x, y, z) in fields {
            bytes.push(index);
            bytes.extend(varint(SER_ROTATIONS));
            bytes.extend(x.to_be_bytes());
            bytes.extend(y.to_be_bytes());
            bytes.extend(z.to_be_bytes());
        }
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, an_armor_stand())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        // Collected rather than asserted one at a time: an `assert_eq!` per
        // part stops at the first mismatch, which for a transposition would
        // report one wrong part and hide the one it was swapped with.
        let mut wrong = Vec::new();
        let decoded = [
            ("head", md.armor_stand_pose.head),
            ("body", md.armor_stand_pose.body),
            ("left_arm", md.armor_stand_pose.left_arm),
            ("right_arm", md.armor_stand_pose.right_arm),
            ("left_leg", md.armor_stand_pose.left_leg),
            ("right_leg", md.armor_stand_pose.right_leg),
        ];
        for ((name, got), (_, x, y, z)) in decoded.iter().zip(fields) {
            let want = Vec3f::new(x, y, z);
            if *got != Some(want) {
                wrong.push(format!("{name}: got {got:?}, want {want:?}"));
            }
        }
        assert!(wrong.is_empty(), "armour-stand pose mismatches:\n{}", wrong.join("\n"));
    }

    /// The negative half of the arms above: those six indices are shared with a
    /// crowd of unrelated accessors, and only the `ROTATIONS` value shape
    /// separates them. A mob's index-16 `INT` must not land in a pose field.
    ///
    /// This is the control for the "no class guard" decision — the arms key on
    /// the value, so something else at the same index has to be shown *not* to
    /// reach them.
    #[test]
    fn an_int_at_a_pose_index_never_surfaces_as_a_pose() {
        let mut bytes = Vec::new();
        bytes.push(IDX_ARMOR_STAND_HEAD_POSE); // 16 — also Creeper.DATA_SWELL_DIR
        bytes.extend(varint(SER_INT));
        bytes.extend(varint(1));
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_creeper())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.creeper_swell_dir, Some(1), "the int must still reach its own field");
        assert_eq!(md.armor_stand_pose.head, None, "and must not reach a pose field");
    }

    /// `Rotations`' compact constructor, reproduced at the decode site: a
    /// non-finite component becomes `0.0` and a finite one is reduced modulo
    /// 360.
    ///
    /// The `NaN` case is the one that matters — a `NaN` rotation reaching the
    /// rig poisons every matrix composed from that part, so it must be stopped
    /// here rather than mispose one joint. Asserted per component, with the
    /// three components carrying *different* hazards, so a fix that handled
    /// only one of them cannot pass.
    #[test]
    fn a_non_finite_rotation_is_clamped_and_a_large_one_is_wrapped() {
        let mut bytes = Vec::new();
        bytes.push(IDX_ARMOR_STAND_BODY_POSE);
        bytes.extend(varint(SER_ROTATIONS));
        bytes.extend(f32::NAN.to_be_bytes());
        bytes.extend(f32::INFINITY.to_be_bytes());
        bytes.extend(450.0f32.to_be_bytes());
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, an_armor_stand())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(
            md.armor_stand_pose.body,
            Some(Vec3f::new(0.0, 0.0, 90.0)),
            "NaN and infinity become 0.0; 450 wraps to 90"
        );
    }

    /// The three-way fork over one fixture byte, so no one path's result is a
    /// lone assertion about a shape that might differ between the others.
    /// Mirrors `the_living_guard_is_the_only_difference_between_the_two_decodes`.
    ///
    /// This is also the control that proves `decodes_armor_stand_flags_…` above
    /// is not vacuous: before `armor_stand_flags` existed, this same test
    /// (`the_mob_guard_is_the_only_difference_between_the_two_decodes`) asserted
    /// `as_stand.is_empty()` — using the armour-stand-shaped byte's *absence*
    /// from the decoded update as its own negative control. That premise is
    /// exactly what this change invalidates (CLAUDE.md: "a gate that uses an
    /// unimplemented thing as its negative stand-in goes vacuous the moment
    /// someone implements it"), so the assertion is now the opposite: an
    /// armour stand's decode is **not** empty either.
    #[test]
    fn the_class_guard_disambiguates_index_15_between_mob_and_armor_stand() {
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
        let as_stand = decode(an_armor_stand());
        let as_unresolved = decode(a_living_non_mob());
        assert_eq!(as_mob.mob_flags, Some(0x04));
        assert_eq!(as_mob.armor_stand_flags, None);
        assert_eq!(as_stand.mob_flags, None);
        assert_eq!(as_stand.armor_stand_flags, Some(0x04));
        assert_eq!(as_unresolved.mob_flags, None);
        assert_eq!(as_unresolved.armor_stand_flags, None);
        assert!(
            !as_mob.is_empty(),
            "a mob's flags byte is a reportable field"
        );
        assert!(
            !as_stand.is_empty(),
            "an armour stand's flags byte is now a reportable field too"
        );
        assert!(
            as_unresolved.is_empty(),
            "with nothing else in the list and no resolvable class, the byte still \
             leaves the update empty — so `handle_set_entity_data` emits no event \
             for a type this decoder cannot classify at all"
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

    /// The idle default: `swell_dir == -1` (`Creeper.java`,
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

    fn a_dragon() -> TrackedEntity {
        TrackedEntity {
            class: Some(MetadataClass::Dragon),
            living: true,
            mob: true,
        }
    }

    fn an_end_crystal() -> TrackedEntity {
        TrackedEntity {
            class: Some(MetadataClass::EndCrystal),
            living: false,
            mob: false,
        }
    }

    /// A dragon's phase (index 16, `INT`) is raised only when the caller says
    /// the entity is a dragon.
    #[test]
    fn dragon_phase_is_raised_for_a_known_dragon() {
        let mut bytes = Vec::new();
        bytes.push(IDX_DRAGON_PHASE);
        bytes.extend(varint(SER_INT));
        bytes.extend(varint(5)); // SittingFlaming
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_dragon())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.dragon_phase, Some(5));
    }

    /// **The control, and it must fail without the class guard.** Index 16's
    /// `INT` is also `Warden.CLIENT_ANGER_LEVEL`/`Creeper.DATA_SWELL_DIR`/etc
    /// — bit-identical serializer, no other signal distinguishes them. A
    /// following field proves alignment survived the unmatched value.
    #[test]
    fn dragon_phase_without_dragon_class_is_consumed_but_not_surfaced() {
        let mut bytes = Vec::new();
        bytes.push(IDX_DRAGON_PHASE);
        bytes.extend(varint(SER_INT));
        bytes.extend(varint(37)); // a plausible warden anger level
        bytes.push(IDX_HEALTH);
        bytes.extend(varint(SER_FLOAT));
        bytes.extend(4.0f32.to_be_bytes());
        bytes.push(EOF_MARKER);
        for tracked in [a_mob(), a_creeper(), a_sheep()] {
            let mut reader = Reader::new(&bytes);
            let md = read_entity_metadata(&mut reader, tracked)
                .expect("decode")
                .metadata;
            reader.ensure_empty().expect("the value must be consumed, staying aligned");
            assert_eq!(
                md.dragon_phase, None,
                "a non-dragon's index-16 INT must not surface as a dragon phase"
            );
            assert_eq!(md.health, Some(4.0), "the following field must still align");
        }
    }

    /// An end crystal's `showsBottom` (index 9, `BOOLEAN`) is raised only when
    /// the caller says the entity is an end crystal — pairwise-distinct from a
    /// neighbouring bool so a transposition cannot survive (see the beam
    /// target test right below, which sets the opposite value at index 8).
    #[test]
    fn crystal_show_bottom_is_raised_for_a_known_crystal() {
        let mut bytes = Vec::new();
        bytes.push(IDX_CRYSTAL_SHOW_BOTTOM);
        bytes.extend(varint(SER_BOOLEAN));
        bytes.push(0); // false — the collision-species neighbours default true-ish, keep it distinct
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, an_end_crystal())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.crystal_show_bottom, Some(false));
    }

    /// **The control**: index 9's `BOOLEAN` is also `AreaEffectCloud.DATA_WAITING`
    /// and `FishingHook.DATA_BITING`. Without the class guard this would
    /// surface as `crystal_show_bottom` for any mob.
    #[test]
    fn crystal_show_bottom_without_end_crystal_class_is_consumed_but_not_surfaced() {
        let mut bytes = Vec::new();
        bytes.push(IDX_CRYSTAL_SHOW_BOTTOM);
        bytes.extend(varint(SER_BOOLEAN));
        bytes.push(1);
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("the value must be consumed, staying aligned");
        assert_eq!(md.crystal_show_bottom, None);
    }

    /// An end crystal's beam target (index 8, `OPTIONAL_BLOCK_POS`) is
    /// self-identifying by `(index, value shape)` and needs **no** class
    /// guard — present case: presence bool `true` then the packed position.
    /// Coordinates are pairwise-distinct so a transposed unpack cannot survive.
    #[test]
    fn crystal_beam_target_present_needs_no_class_guard() {
        let mut bytes = Vec::new();
        bytes.push(IDX_CRYSTAL_BEAM_TARGET);
        bytes.extend(varint(SER_OPTIONAL_BLOCK_POS));
        bytes.push(1); // present
        // The same packing `pack_block_pos` (server_protocol.rs) writes and
        // this module's own `unpack_block_pos` reverses — independently
        // re-derived here rather than calling either, so this assertion
        // cannot pass by construction against a shared bug.
        let packed: i64 = ((11i64 & 0x3FF_FFFF) << 38) | ((4i64 & 0x3FF_FFFF) << 12) | (65i64 & 0xFFF);
        bytes.extend(packed.to_be_bytes());
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(
            md.crystal_beam_target,
            Reported::Reported(Some(BlockPos::new(11, 65, 4)))
        );
    }

    /// Absent case: presence bool `false`, no position bytes at all —
    /// `Reported::Reported(None)`, distinct from `Reported::Unreported`
    /// (the field was not merely absent from the packet; it was present and
    /// explicitly cleared).
    #[test]
    fn crystal_beam_target_absent_is_reported_cleared_not_unreported() {
        let mut bytes = Vec::new();
        bytes.push(IDX_CRYSTAL_BEAM_TARGET);
        bytes.extend(varint(SER_OPTIONAL_BLOCK_POS));
        bytes.push(0); // absent
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, a_mob())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(md.crystal_beam_target, Reported::Reported(None));
        assert!(md.crystal_beam_target.is_reported());
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
        assert_eq!(metadata_class("minecraft:ender_dragon"), Some(MetadataClass::Dragon));
        assert_eq!(metadata_class("minecraft:end_crystal"), Some(MetadataClass::EndCrystal));
        assert_eq!(
            metadata_class("minecraft:armor_stand"),
            Some(MetadataClass::ArmorStand)
        );
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

    /// A known-answer encode: builds the exact byte string by hand (the same
    /// expectation shape as [`decodes_update_attributes`]'s fixture, not a
    /// `decode(encode(x)) == x` round trip, which cannot distinguish a
    /// correct encoder from a decoder and encoder that share one
    /// misunderstanding) and asserts [`write_update_attributes`] reproduces
    /// it byte-for-byte, including the modifier-free `minecraft:armor` case
    /// `lodestone-server`'s attribute sync actually sends.
    #[test]
    fn encodes_update_attributes_to_the_known_bytes() {
        let mut expected = Vec::new();
        expected.extend(varint(1)); // entity id (LOCAL_PLAYER_ENTITY_ID)
        expected.extend(varint(1)); // one attribute
        expected.extend(varint(1)); // `minecraft:armor` registry id
        expected.extend(11.0f64.to_be_bytes()); // folded base, no modifiers
        expected.extend(varint(0)); // zero modifiers

        let snapshot = EntityAttributeSnapshot {
            attribute: "minecraft:armor".parse().expect("valid identifier"),
            base: 11.0,
            modifiers: Vec::new(),
        };
        let mut w = Writer::default();
        write_update_attributes(&mut w, 1, std::slice::from_ref(&snapshot));
        assert_eq!(w.into_vec(), expected);

        // Control: decoding what was just built reaches the same fields the
        // encoder was given, so the two are not both wrong the same way.
        let mut w2 = Writer::default();
        write_update_attributes(&mut w2, 1, std::slice::from_ref(&snapshot));
        let bytes2 = w2.into_vec();
        let mut reader = Reader::new(&bytes2);
        let (entity_id, attrs) = read_update_attributes(&mut reader).expect("decode");
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(entity_id, 1);
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].attribute.to_string(), "minecraft:armor");
        assert!((attrs[0].base - 11.0).abs() < 1e-12);
        assert!(attrs[0].modifiers.is_empty());
    }

    /// An attribute id this crate's table does not know is dropped rather than
    /// failing the whole encode, and the written count reflects that.
    #[test]
    fn encode_skips_an_unresolvable_attribute_and_keeps_the_count_honest() {
        let known = EntityAttributeSnapshot {
            attribute: "minecraft:armor".parse().expect("valid identifier"),
            base: 4.0,
            modifiers: Vec::new(),
        };
        let unknown = EntityAttributeSnapshot {
            attribute: "minecraft:not_a_real_attribute".parse().expect("valid identifier"),
            base: 9.0,
            modifiers: Vec::new(),
        };
        let mut w = Writer::default();
        write_update_attributes(&mut w, 1, &[unknown, known]);
        let bytes = w.into_vec();
        let mut reader = Reader::new(&bytes);
        let (_, attrs) = read_update_attributes(&mut reader).expect("decode");
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(attrs.len(), 1, "the unresolvable attribute must be dropped, not the whole packet");
        assert_eq!(attrs[0].attribute.to_string(), "minecraft:armor");
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
            (
                IDX_MOB_FLAGS,
                "IDX_MOB_FLAGS",
                "ArmorStand.DATA_CLIENT_FLAGS",
                SER_BYTE,
            ),
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
            (
                IDX_ARMOR_STAND_HEAD_POSE,
                "IDX_ARMOR_STAND_HEAD_POSE",
                "ArmorStand.DATA_HEAD_POSE",
                SER_ROTATIONS,
            ),
            (
                IDX_ARMOR_STAND_BODY_POSE,
                "IDX_ARMOR_STAND_BODY_POSE",
                "ArmorStand.DATA_BODY_POSE",
                SER_ROTATIONS,
            ),
            (
                IDX_ARMOR_STAND_LEFT_ARM_POSE,
                "IDX_ARMOR_STAND_LEFT_ARM_POSE",
                "ArmorStand.DATA_LEFT_ARM_POSE",
                SER_ROTATIONS,
            ),
            (
                IDX_ARMOR_STAND_RIGHT_ARM_POSE,
                "IDX_ARMOR_STAND_RIGHT_ARM_POSE",
                "ArmorStand.DATA_RIGHT_ARM_POSE",
                SER_ROTATIONS,
            ),
            (
                IDX_ARMOR_STAND_LEFT_LEG_POSE,
                "IDX_ARMOR_STAND_LEFT_LEG_POSE",
                "ArmorStand.DATA_LEFT_LEG_POSE",
                SER_ROTATIONS,
            ),
            (
                IDX_ARMOR_STAND_RIGHT_LEG_POSE,
                "IDX_ARMOR_STAND_RIGHT_LEG_POSE",
                "ArmorStand.DATA_RIGHT_LEG_POSE",
                SER_ROTATIONS,
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
    /// other — the `living` guard. Index **15** is `Mob`'s flags byte
    /// *and* `ArmorStand`'s client flags, both `BYTE`, with `0x04` meaning
    /// "aggressive" on one and "show arms" on the other — and since `ArmorStand`
    /// *is* a `LivingEntity`, that one needs a narrower guard than index 8's.
    /// `PAINTING_VARIANT` has exactly **one** claimant in the jar, which is the
    /// whole reason `Value::PaintingVariant` is routed ahead of the index match
    /// rather than gated on an index or a class — the same property
    /// `SER_ITEM_STACK` relies on.
    ///
    /// Two claims, both against the dump rather than against this decoder:
    /// serializer 34 is claimed by `Painting.DATA_PAINTING_VARIANT_ID` alone,
    /// and it sits at index **9** — which nothing here depends on, and which is
    /// asserted anyway so the fact stays checked. It would be easy to assume 8,
    /// since that is where `HangingEntity.DATA_DIRECTION` sits and `Painting`
    /// extends it.
    #[test]
    fn painting_variant_is_the_only_claimant_of_its_serializer() {
        let claimants: Vec<(u8, String)> = INDEX_DUMP
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| {
                let mut tok = line.split_whitespace();
                let index: u8 = tok.next()?.parse().ok()?;
                let owner = tok.next()?.to_owned();
                let serializer: i32 = tok.next()?.parse().ok()?;
                (serializer == SER_PAINTING_VARIANT).then_some((index, owner))
            })
            .collect();
        assert_eq!(
            claimants,
            vec![(9u8, "Painting.DATA_PAINTING_VARIANT_ID".to_owned())],
            "serializer {SER_PAINTING_VARIANT}'s claimants in the jar dump are not the single              painting accessor this decoder's index-agnostic routing assumes"
        );
    }

    /// A painting's variant decodes to its registry key, and the holder's
    /// `id + 1` wire encoding is respected.
    ///
    /// `id = 24` is `minecraft:kebab` — deliberately **not** id 0, because 0 is
    /// the value the spawn-time default synthesis already produces, so a decoder
    /// that ignored the wire entirely would still look right at 0. Its wire form
    /// is `25`.
    #[test]
    fn painting_variant_decodes_to_a_registry_key() {
        let mut bytes = Vec::new();
        bytes.push(9u8);
        bytes.extend(varint(SER_PAINTING_VARIANT));
        bytes.extend(varint(25));
        bytes.push(EOF_MARKER);

        let mut reader = Reader::new(&bytes);
        // `TrackedEntity::default()` on purpose: a painting is neither living
        // nor a mob and has no `MetadataClass`, so this is exactly what the
        // adapter passes for one — and the decode must not need any of them.
        let md = read_entity_metadata(&mut reader, TrackedEntity::default())
            .expect("decode")
            .metadata;
        reader.ensure_empty().expect("no trailing bytes");
        assert_eq!(
            md.painting_variant.as_ref().map(ToString::to_string),
            Some("minecraft:kebab".to_owned())
        );

        // The control: an id past the table is a data-pack variant with no size
        // and no sprite here. It must consume its VarInt (so the list stays
        // aligned and the terminator is reached) and surface nothing, rather
        // than naming some other painting.
        let mut bytes = Vec::new();
        bytes.push(9u8);
        bytes.extend(varint(SER_PAINTING_VARIANT));
        bytes.extend(varint(9999));
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader, TrackedEntity::default())
            .expect("an unknown variant must still decode, not error")
            .metadata;
        reader
            .ensure_empty()
            .expect("the holder VarInt must be consumed even when it is not surfaced");
        assert_eq!(md.painting_variant, None);
    }

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
