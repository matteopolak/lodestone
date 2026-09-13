//! Maps 1.8's index-keyed `EntityMetadata` list onto the version-free
//! [`EntityMetadataUpdate`], gated by the concrete mob type.
//!
//! # Why gating by mob type is mandatory, not a nicety
//!
//! 1.8's index-keyed metadata list reuses indices across unrelated mob
//! families exactly like every later version's keyed metadata registry
//! does — index 12 is the animal family's clamped baby byte (`-1`/`0`/`1`)
//! *and*, completely independently, the undead family's plain boolean baby
//! byte (`0`/`1`). Interpreting index 12 the same way for both would read a
//! zombie's `1` (baby) as an animal-family "adult" (only negative means baby
//! there), and an animal's `-1` would never occur on a zombie at all — the
//! two encodings are different, not just the semantics. A per-mob-type
//! [`MobProfile`] is how this module avoids that trap.
//!
//! # Evidence
//!
//! 1.8 predates Mojang's data generator and `minecraft-data`'s `pc/1.8`
//! bundle carries no per-index metadata schema (checked: `entities.json` has
//! only id/name/category, nothing about the per-index layout). The decompile
//! under `.cache/mc/26.2` is the *wrong version* — 1.8's indices are not
//! 26.2's. The source used here is a deobfuscated 1.8.8 server-side
//! decompile of the base entity class and its living/mob/ageable/tameable
//! subclasses, plus each concrete mob class — a real, class-named decompile
//! of the exact historical build rather than a transcription, whose
//! index-registration call sites name both the index and the field it backs
//! directly.
//!
//! Every index below was then **independently cross-checked against a real
//! vanilla 1.8.9 server**: `/summon` with explicit NBT (`IsBaby:1`,
//! `Color:14,Sheared:1`, `powered:1`, `Profession:3`, `SkeletonType:1`,
//! `Type:1`, `CatType:2`, `Saddle:1`, `PlayerCreated:1`, `RabbitType:3`, …)
//! against the cached `.cache/mc/1.8.9` server jar, joined with the crate's
//! own `V47Adapter` over a real TCP connection, and the resulting
//! `spawn_entity_living` metadata list decoded and printed. Every mapping in
//! this module matched the requested NBT value at the index the source says
//! it should — including the exact sheep bit-packing (`byte & 0x0F` = colour,
//! `byte & 0x10` = sheared) and the zombie-vs-ageable baby encoding
//! difference the module doc above describes. Also confirmed live, and
//! important for *not* replicating a v26-2-only workaround: 1.8's
//! `spawn_entity_living` always carries **every** registered metadata entry,
//! defaults included (the full-list encoder iterates every entry
//! unconditionally, unlike the incremental packet's encoder, which filters
//! to dirty ones only) — a freshly spawned white unsheared sheep really does
//! carry an explicit index-16 `0` on the wire, confirmed live. So unlike
//! 26.2's adapter, **no default-value synthesis is needed here** for spawn.
//!
//! # What is deliberately left out
//!
//! A partial table that is right beats a complete one that is guessed:
//!
//! * **Villager profession** (index 16, `int`, confirmed live as the raw
//!   profession id) has no clean map onto
//!   [`EntityVariant::Villager`](lodestone_model::EntityVariant::Villager),
//!   which expects a biome *kind* and a trade *level* that did not exist as
//!   concepts until 1.14 — inventing them would be a guess, not a mapping.
//! * **Horse type/variant/armour/owner** (indices 16/19/20/21/22, all
//!   confirmed live) are decoded-safe but not raised into
//!   [`EntityVariant::Horse`](lodestone_model::EntityVariant::Horse): the
//!   colour/markings packing formula for 1.8's single `int` was not found in
//!   any source consulted here and guessing the bit split would be exactly
//!   the "predict the plausible round number" mistake this project has paid
//!   for before.
//! * **Ozelot cat type** (index 18, confirmed live) has no modern equivalent
//!   to map onto: 26.2's ocelot has no variant at all (the cosmetic split
//!   became the separate `minecraft:cat` entity in 1.14).
//! * **Skeleton type** (index 13, confirmed live: `1` = wither) and
//!   **guardian elder** (index 16 bit `0x04`) both name a *different mob
//!   type* in 26.2 (`minecraft:wither_skeleton`, `minecraft:elder_guardian`),
//!   not a cosmetic variant — mapping them would require rewriting the
//!   entity's own spawn type, out of scope for a metadata fold.
//! * **Enderman carried block** (index 16, short combined id) and
//!   **wither target ids** (indices 17–19) have no field in
//!   [`EntityMetadataUpdate`] to carry them.
//! * **Wolf collar colour / begging / owner**, **ozelot / wolf / horse owner
//!   UUID**, **rabbit type**, **iron golem "player created"**, **bat
//!   "hanging"**, **blaze's on-fire *visual* flag** (distinct from the real
//!   burning state, confirmed live to exist independently) and **guardian's
//!   "retracting spikes" bit** are all decoded-safe (confirmed present on the
//!   wire where relevant) but have no corresponding field in the shared
//!   model.
//!
//! None of these are *wrong* to decode — the wire codec in
//! [`crate::packets::metadata`] does not care what an index means — they are
//! left out of the *canonical fold* because raising them would mean
//! inventing a mapping this module has no evidence for.

use lodestone_model::{EntityMetadataUpdate, EntityVariant, Reported, Text};

use crate::packets::metadata::{EntityMetadata, MetadataEntry, MetadataValue};

// ---------------------------------------------------------------------------
// Base-entity / living / mob indices.
//
// Every 1.8 mob-spawn type (all 34 entries in `entity_types::MOB_TYPES`)
// extends this exact base-entity -> living -> mob class chain — so these
// indices carry the same meaning for every mob and need no per-type gating.
// Confirmed against the base entity, living-entity and mob class's index
// registrations and live spawn captures.
// ---------------------------------------------------------------------------

/// The base entity class's shared flags byte: on-fire / crouched / (unused) /
/// sprinting / using-item / invisible.
const IDX_ENTITY_FLAGS: u8 = 0;
/// The base entity class's air-supply short.
const IDX_AIR: u8 = 1;
/// The base entity class's custom-name string (empty when unset).
const IDX_CUSTOM_NAME: u8 = 2;
/// The base entity class's custom-name-visible byte.
const IDX_CUSTOM_NAME_VISIBLE: u8 = 3;
/// The living-entity class's health float.
const IDX_HEALTH: u8 = 6;
/// The mob class's No-AI byte (`0` = has AI, non-zero = `/summon
/// {NoAI:1}`).
const IDX_NO_AI: u8 = 15;

/// 1.8 base-entity flags bit: on fire.
const ENTITY_ON_FIRE: u8 = 0x01;
/// 1.8 base-entity flags bit: crouched / sneaking.
const ENTITY_CROUCHED: u8 = 0x02;
/// 1.8 base-entity flags bit: sprinting.
const ENTITY_SPRINTING: u8 = 0x08;
/// 1.8 base-entity flags bit: eating / drinking / blocking (set/cleared
/// around the player class's active-item field) — this is 1.8's using-item
/// bit, but it lives in the *shared* flags byte, unlike modern versions
/// where using-item moved to the living-entity's own byte. Translated below
/// into [`EntityMetadataUpdate::living_flags`], never into
/// [`EntityMetadataUpdate::flags`] — **not** a passthrough, because this bit
/// position (`0x10`) means `SWIMMING` in the canonical shared-flags byte, an
/// entirely different modern concept 1.8 does not have.
const ENTITY_USING_ITEM: u8 = 0x10;
/// 1.8 base-entity flags bit: invisible.
const ENTITY_INVISIBLE: u8 = 0x20;

/// Canonical `SharedEntityFlags` bit: on fire (`lodestone_entity::metadata`).
const SHARED_ON_FIRE: u8 = 0x01;
/// Canonical `SharedEntityFlags` bit: crouching.
const SHARED_CROUCHING: u8 = 0x02;
/// Canonical `SharedEntityFlags` bit: sprinting.
const SHARED_SPRINTING: u8 = 0x08;
/// Canonical `SharedEntityFlags` bit: invisible.
const SHARED_INVISIBLE: u8 = 0x20;

/// Canonical `LivingEntityFlags` bit: using item.
const LIVING_USING_ITEM: u8 = 0x01;

/// Canonical `MobFlags` bit: no AI.
const MOB_NO_AI: u8 = 0x01;
/// Canonical `MobFlags` bit: aggressive.
const MOB_AGGRESSIVE: u8 = 0x04;

// ---------------------------------------------------------------------------
// Class-specific indices. Each constant's doc names the concrete mob family
// it was read from and confirmed on.
// ---------------------------------------------------------------------------

/// The animal-family class's clamped age byte (`-1` = baby, `0`/`1` =
/// adult), shared by every animal-family type plus the villager class
/// (which extends the same ageable class directly rather than the plain
/// animal class).
const IDX_AGEABLE_AGE: u8 = 12;

/// The tameable-animal class's flags byte: bit `0x01` sitting, bit `0x04`
/// tamed. Wolf and ozelot share this index and these two bits exactly.
const IDX_TAMEABLE_FLAGS: u8 = 16;
const TAMEABLE_SITTING: u8 = 0x01;
const TAMEABLE_TAMED: u8 = 0x04;

/// The sheep class's dyed-wool byte: low nibble is the dye ordinal
/// (`0..=15`), bit `0x10` is sheared.
const IDX_SHEEP_WOOL: u8 = 16;
const SHEEP_SHEARED: u8 = 0x10;
const SHEEP_COLOR_MASK: u8 = 0x0F;

/// The zombie class's baby byte — a **plain boolean** (index 12 equal to
/// `1`), unlike the animal-family class's clamped sign byte at the same
/// index. Shared by the zombie-pigman class (extends the zombie class) but
/// **not** the giant class (extends the monster base class directly —
/// confirmed by class declaration, not assumed from the name).
const IDX_ZOMBIE_BABY: u8 = 12;

/// The creeper class's swell-direction byte (`-1` idle, `1` counting up).
const IDX_CREEPER_STATE: u8 = 16;
/// The creeper class's powered byte.
const IDX_CREEPER_POWERED: u8 = 17;
/// The creeper class's ignited byte.
const IDX_CREEPER_IGNITED: u8 = 18;

/// The witch class's aggressive byte.
const IDX_WITCH_AGGRESSIVE: u8 = 21;

/// Which extra, class-specific fields (beyond the universal base-entity /
/// living-entity / mob base) a mob type's metadata list may carry. Selected
/// by [`profile_for`] from the mob's canonical type string, never guessed
/// from the index alone — see the module doc's collision warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MobProfile {
    /// Nothing beyond the universal base (e.g. skeleton, spider, enderman,
    /// guardian, blaze — each does carry its own extra indices on the wire,
    /// but none of them are raised into the canonical fold; see the module
    /// doc's "deliberately left out" section).
    Base,
    /// The animal-family class's age byte only (pig, cow, chicken, mushroom
    /// cow, rabbit, horse, villager).
    Ageable,
    /// Ageable plus the tameable-animal class's tamed/sitting bits (wolf,
    /// ozelot).
    Tameable,
    /// Ageable plus the sheep class's dyed-wool byte.
    Sheep,
    /// The zombie class's boolean baby byte (zombie, zombie pigman).
    ZombieBaby,
    /// The creeper class's state/powered/ignited trio.
    Creeper,
    /// The witch class's aggressive byte.
    Witch,
}

/// Resolves a 1.8 canonical mob-type identifier (as produced by
/// [`crate::entity_types::mob_type_name`]) to the extra metadata fields its
/// class carries, per the historical server-side class hierarchy:
///
/// ```text
/// base entity -> living entity -> mob -> creature
///   -> ageable -> animal -> {pig, cow, chicken, sheep, rabbit, horse, ...}
///                        -> tameable animal -> {wolf, ozelot}
///   -> ageable -> villager
///   -> monster -> {creeper, zombie, witch, skeleton, spider, ...}
/// ```
///
/// A type not named here (including `minecraft:mob`/`minecraft:monster`,
/// which are 1.8's abstract "unknown" ids and never actually spawned by
/// vanilla) gets [`MobProfile::Base`] — the universal fields only, which is
/// always safe because it never reads a class-specific index.
fn profile_for(mob_type: &str) -> MobProfile {
    match mob_type {
        "minecraft:pig"
        | "minecraft:cow"
        | "minecraft:chicken"
        | "minecraft:mushroom_cow"
        | "minecraft:rabbit"
        | "minecraft:entity_horse"
        | "minecraft:villager" => MobProfile::Ageable,
        "minecraft:wolf" | "minecraft:ozelot" => MobProfile::Tameable,
        "minecraft:sheep" => MobProfile::Sheep,
        "minecraft:zombie" | "minecraft:pig_zombie" => MobProfile::ZombieBaby,
        "minecraft:creeper" => MobProfile::Creeper,
        "minecraft:witch" => MobProfile::Witch,
        _ => MobProfile::Base,
    }
}

fn find(entries: &[MetadataEntry], key: u8) -> Option<&MetadataValue> {
    entries.iter().find(|entry| entry.key == key).map(|entry| &entry.value)
}

fn find_byte(entries: &[MetadataEntry], key: u8) -> Option<u8> {
    match find(entries, key) {
        Some(MetadataValue::Byte(v)) => Some(*v as u8),
        _ => None,
    }
}

fn find_signed_byte(entries: &[MetadataEntry], key: u8) -> Option<i8> {
    match find(entries, key) {
        Some(MetadataValue::Byte(v)) => Some(*v),
        _ => None,
    }
}

fn find_short(entries: &[MetadataEntry], key: u8) -> Option<i16> {
    match find(entries, key) {
        Some(MetadataValue::Short(v)) => Some(*v),
        _ => None,
    }
}

fn find_float(entries: &[MetadataEntry], key: u8) -> Option<f32> {
    match find(entries, key) {
        Some(MetadataValue::Float(v)) => Some(*v),
        _ => None,
    }
}

fn find_string<'a>(entries: &'a [MetadataEntry], key: u8) -> Option<&'a str> {
    match find(entries, key) {
        Some(MetadataValue::String(v)) => Some(v.as_str()),
        _ => None,
    }
}

/// Folds a decoded 1.8 [`EntityMetadata`] list into the version-free
/// [`EntityMetadataUpdate`], gated by `mob_type` (a canonical id such as
/// `"minecraft:sheep"`, as produced by
/// [`crate::entity_types::mob_type_name`]).
///
/// `mob_type` should be `None` when the entity's concrete class is not
/// known (e.g. an `entity_metadata` packet for an id this adapter never saw
/// spawn) — the fold then applies only the universal base fields, which are
/// safe to interpret regardless of class.
#[must_use]
pub fn fold(mob_type: Option<&str>, metadata: &EntityMetadata) -> EntityMetadataUpdate {
    let entries = &metadata.0;
    let mut update = EntityMetadataUpdate::default();

    if let Some(raw) = find_byte(entries, IDX_ENTITY_FLAGS) {
        let mut shared = 0u8;
        if raw & ENTITY_ON_FIRE != 0 {
            shared |= SHARED_ON_FIRE;
        }
        if raw & ENTITY_CROUCHED != 0 {
            shared |= SHARED_CROUCHING;
        }
        if raw & ENTITY_SPRINTING != 0 {
            shared |= SHARED_SPRINTING;
        }
        if raw & ENTITY_INVISIBLE != 0 {
            shared |= SHARED_INVISIBLE;
        }
        update.flags = Some(shared);

        let mut living = 0u8;
        if raw & ENTITY_USING_ITEM != 0 {
            living |= LIVING_USING_ITEM;
        }
        update.living_flags = Some(living);
    }

    if let Some(air) = find_short(entries, IDX_AIR) {
        update.air_supply = Some(i32::from(air));
    }

    if let Some(name) = find_string(entries, IDX_CUSTOM_NAME) {
        // `Text::literal` rather than `Text::from_legacy`: this family is out
        // of scope for the styled-nametag fix (`v26-2` is the target; see
        // `EntityMetadataUpdate::custom_name`'s doc), so this keeps the exact
        // prior plain-text behaviour, just wrapped in the new `Text` shape
        // rather than a bare `String`, to keep this crate compiling against
        // that shared field's new type. `from_legacy` would additionally
        // parse `§`-codes 1.8 lets a player type directly into a nametag, but
        // that widens this family's own behaviour and is left for a change
        // that actually owns `v1-8`.
        update.custom_name = Reported::Reported(if name.is_empty() {
            None
        } else {
            Some(Text::literal(name))
        });
    }

    if let Some(visible) = find_byte(entries, IDX_CUSTOM_NAME_VISIBLE) {
        update.custom_name_visible = Some(visible != 0);
    }

    if let Some(health) = find_float(entries, IDX_HEALTH) {
        update.health = Some(health);
    }

    if let Some(no_ai) = find_byte(entries, IDX_NO_AI) {
        let mut mob = update.mob_flags.unwrap_or(0);
        if no_ai != 0 {
            mob |= MOB_NO_AI;
        }
        update.mob_flags = Some(mob);
    }

    match mob_type.map(profile_for).unwrap_or(MobProfile::Base) {
        MobProfile::Base => {}
        MobProfile::Ageable => apply_ageable(&mut update, entries),
        MobProfile::Tameable => {
            apply_ageable(&mut update, entries);
            apply_tameable(&mut update, entries);
        }
        MobProfile::Sheep => {
            apply_ageable(&mut update, entries);
            apply_sheep(&mut update, entries);
        }
        MobProfile::ZombieBaby => apply_zombie_baby(&mut update, entries),
        MobProfile::Creeper => apply_creeper(&mut update, entries),
        MobProfile::Witch => apply_witch(&mut update, entries),
    }

    update
}

fn apply_ageable(update: &mut EntityMetadataUpdate, entries: &[MetadataEntry]) {
    if let Some(age) = find_signed_byte(entries, IDX_AGEABLE_AGE) {
        update.baby = Some(age < 0);
    }
}

fn apply_tameable(update: &mut EntityMetadataUpdate, entries: &[MetadataEntry]) {
    if let Some(raw) = find_byte(entries, IDX_TAMEABLE_FLAGS) {
        update.sitting = Some(raw & TAMEABLE_SITTING != 0);
        update.tamed = Some(raw & TAMEABLE_TAMED != 0);
    }
}

fn apply_sheep(update: &mut EntityMetadataUpdate, entries: &[MetadataEntry]) {
    if let Some(raw) = find_byte(entries, IDX_SHEEP_WOOL) {
        update.variant = Some(EntityVariant::Dyed {
            color: raw & SHEEP_COLOR_MASK,
            sheared: raw & SHEEP_SHEARED != 0,
        });
    }
}

fn apply_zombie_baby(update: &mut EntityMetadataUpdate, entries: &[MetadataEntry]) {
    if let Some(raw) = find_byte(entries, IDX_ZOMBIE_BABY) {
        update.baby = Some(raw == 1);
    }
}

fn apply_creeper(update: &mut EntityMetadataUpdate, entries: &[MetadataEntry]) {
    if let Some(dir) = find_signed_byte(entries, IDX_CREEPER_STATE) {
        update.creeper_swell_dir = Some(i32::from(dir));
    }
    if let Some(powered) = find_byte(entries, IDX_CREEPER_POWERED) {
        update.creeper_powered = Some(powered != 0);
    }
    if let Some(ignited) = find_byte(entries, IDX_CREEPER_IGNITED) {
        update.creeper_ignited = Some(ignited != 0);
    }
}

fn apply_witch(update: &mut EntityMetadataUpdate, entries: &[MetadataEntry]) {
    if let Some(raw) = find_byte(entries, IDX_WITCH_AGGRESSIVE) {
        let mut mob = update.mob_flags.unwrap_or(0);
        if raw != 0 {
            mob |= MOB_AGGRESSIVE;
        }
        update.mob_flags = Some(mob);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packets::metadata::MetadataValue;

    fn entries(pairs: &[(u8, MetadataValue)]) -> EntityMetadata {
        EntityMetadata(
            pairs
                .iter()
                .cloned()
                .map(|(key, value)| MetadataEntry { key, value })
                .collect(),
        )
    }

    #[test]
    fn base_flags_translate_on_fire_and_sprinting_but_not_the_using_item_bit() {
        // Pairwise-distinct bits set: on-fire (0x01) + sprinting (0x08), NOT
        // crouched/using-item/invisible, so a stuck bit cannot hide.
        let md = entries(&[(IDX_ENTITY_FLAGS, MetadataValue::Byte(0x09))]);
        let update = fold(None, &md);
        let flags = update.flags.expect("flags reported");
        assert_eq!(flags & SHARED_ON_FIRE, SHARED_ON_FIRE);
        assert_eq!(flags & SHARED_SPRINTING, SHARED_SPRINTING);
        assert_eq!(flags & SHARED_CROUCHING, 0);
        assert_eq!(flags & SHARED_INVISIBLE, 0);
        // The using-item bit (0x10) must never leak into `flags`: that bit
        // position means SWIMMING in the canonical shared-flags byte.
        assert_eq!(
            flags & 0x10,
            0,
            "1.8's using-item bit must not alias canonical SWIMMING"
        );
    }

    #[test]
    fn using_item_bit_becomes_living_flags_not_shared_flags() {
        let md = entries(&[(IDX_ENTITY_FLAGS, MetadataValue::Byte(ENTITY_USING_ITEM as i8))]);
        let update = fold(None, &md);
        assert_eq!(update.flags, Some(0));
        assert_eq!(update.living_flags, Some(LIVING_USING_ITEM));
    }

    #[test]
    fn sheep_wool_byte_splits_color_and_sheared_independently() {
        // color=14, sheared=true, pairwise distinct from a plain "== 15" trap:
        // byte 0x1E = 0b0001_1110 -> low nibble 14, bit 0x10 set.
        let md = entries(&[
            (IDX_ENTITY_FLAGS, MetadataValue::Byte(0)),
            (IDX_AGEABLE_AGE, MetadataValue::Byte(0)),
            (IDX_SHEEP_WOOL, MetadataValue::Byte(0x1E)),
        ]);
        let update = fold(Some("minecraft:sheep"), &md);
        assert_eq!(
            update.variant,
            Some(EntityVariant::Dyed {
                color: 14,
                sheared: true
            })
        );
        assert_eq!(update.baby, Some(false));
    }

    #[test]
    fn sheep_unsheared_default_color_decodes_cleanly() {
        let md = entries(&[(IDX_SHEEP_WOOL, MetadataValue::Byte(3))]);
        let update = fold(Some("minecraft:sheep"), &md);
        assert_eq!(
            update.variant,
            Some(EntityVariant::Dyed {
                color: 3,
                sheared: false
            })
        );
    }

    #[test]
    fn ageable_negative_age_is_baby_zero_is_adult() {
        let baby = entries(&[(IDX_AGEABLE_AGE, MetadataValue::Byte(-1))]);
        assert_eq!(fold(Some("minecraft:pig"), &baby).baby, Some(true));

        let adult = entries(&[(IDX_AGEABLE_AGE, MetadataValue::Byte(0))]);
        assert_eq!(fold(Some("minecraft:pig"), &adult).baby, Some(false));
    }

    #[test]
    fn zombie_baby_uses_the_boolean_encoding_not_the_ageable_sign() {
        // Same index (12) as the animal-family class, but zombie's own boolean scheme:
        // `1` is baby, and unlike Ageable a negative value never occurs.
        let baby = entries(&[(IDX_ZOMBIE_BABY, MetadataValue::Byte(1))]);
        assert_eq!(fold(Some("minecraft:zombie"), &baby).baby, Some(true));

        let adult = entries(&[(IDX_ZOMBIE_BABY, MetadataValue::Byte(0))]);
        assert_eq!(fold(Some("minecraft:zombie"), &adult).baby, Some(false));

        // pig_zombie shares the zombie class's own baby encoding.
        assert_eq!(
            fold(Some("minecraft:pig_zombie"), &baby).baby,
            Some(true),
            "pig_zombie inherits the zombie class's baby byte"
        );
    }

    #[test]
    fn giant_does_not_inherit_zombie_baby_despite_the_name() {
        // The giant class extends the monster base class directly (verified
        // against the class declaration), NOT the zombie class, so it must not read
        // index 12 as a baby flag even though one is present on the wire.
        let md = entries(&[(IDX_ZOMBIE_BABY, MetadataValue::Byte(1))]);
        assert_eq!(fold(Some("minecraft:giant"), &md).baby, None);
    }

    #[test]
    fn wolf_tameable_bits_are_independent() {
        // sitting (0x01) set, tamed (0x04) clear — pairwise distinct so
        // neither bit can hide behind the other.
        let md = entries(&[(IDX_TAMEABLE_FLAGS, MetadataValue::Byte(0x01))]);
        let update = fold(Some("minecraft:wolf"), &md);
        assert_eq!(update.sitting, Some(true));
        assert_eq!(update.tamed, Some(false));

        let md2 = entries(&[(IDX_TAMEABLE_FLAGS, MetadataValue::Byte(0x04))]);
        let update2 = fold(Some("minecraft:wolf"), &md2);
        assert_eq!(update2.sitting, Some(false));
        assert_eq!(update2.tamed, Some(true));
    }

    #[test]
    fn creeper_state_powered_and_ignited_are_distinct_fields() {
        let md = entries(&[
            (IDX_CREEPER_STATE, MetadataValue::Byte(-1)),
            (IDX_CREEPER_POWERED, MetadataValue::Byte(1)),
            (IDX_CREEPER_IGNITED, MetadataValue::Byte(0)),
        ]);
        let update = fold(Some("minecraft:creeper"), &md);
        assert_eq!(update.creeper_swell_dir, Some(-1));
        assert_eq!(update.creeper_powered, Some(true));
        assert_eq!(update.creeper_ignited, Some(false));
    }

    #[test]
    fn witch_aggressive_folds_into_mob_flags_alongside_no_ai() {
        let md = entries(&[
            (IDX_NO_AI, MetadataValue::Byte(1)),
            (IDX_WITCH_AGGRESSIVE, MetadataValue::Byte(1)),
        ]);
        let update = fold(Some("minecraft:witch"), &md);
        let mob = update.mob_flags.expect("mob_flags reported");
        assert_eq!(mob & MOB_NO_AI, MOB_NO_AI);
        assert_eq!(mob & MOB_AGGRESSIVE, MOB_AGGRESSIVE);
    }

    #[test]
    fn health_and_custom_name_are_universal() {
        let md = entries(&[
            (IDX_HEALTH, MetadataValue::Float(11.5)),
            (IDX_CUSTOM_NAME, MetadataValue::String("Bob".into())),
            (IDX_CUSTOM_NAME_VISIBLE, MetadataValue::Byte(1)),
            (IDX_AIR, MetadataValue::Short(200)),
        ]);
        let update = fold(Some("minecraft:skeleton"), &md);
        assert_eq!(update.health, Some(11.5));
        assert_eq!(
            update.custom_name,
            Reported::Reported(Some(Text::literal("Bob")))
        );
        assert_eq!(update.custom_name_visible, Some(true));
        assert_eq!(update.air_supply, Some(200));
    }

    #[test]
    fn empty_custom_name_reports_an_explicit_clear() {
        let md = entries(&[(IDX_CUSTOM_NAME, MetadataValue::String(String::new()))]);
        let update = fold(Some("minecraft:skeleton"), &md);
        assert_eq!(update.custom_name, Reported::Reported(None));
    }

    #[test]
    fn unknown_mob_type_still_folds_the_universal_base() {
        let md = entries(&[
            (IDX_ENTITY_FLAGS, MetadataValue::Byte(ENTITY_ON_FIRE as i8)),
            (IDX_HEALTH, MetadataValue::Float(20.0)),
        ]);
        let update = fold(None, &md);
        assert_eq!(update.flags, Some(SHARED_ON_FIRE));
        assert_eq!(update.health, Some(20.0));
        // But never a class-specific field it cannot safely gate.
        assert_eq!(update.baby, None);
        assert_eq!(update.variant, None);
    }

    #[test]
    fn empty_metadata_folds_to_nothing() {
        let md = EntityMetadata::default();
        assert!(fold(Some("minecraft:sheep"), &md).is_empty());
    }
}
