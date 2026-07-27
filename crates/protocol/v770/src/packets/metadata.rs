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
//! A handful of serializers carry genuinely complex, self-describing payloads
//! (item stacks, particles, resolvable profiles). Mobs never emit those in
//! practice, so they are deliberately *not* modelled: they decode to an explicit
//! error rather than a guess.

use lodestone_core::{Error, Reader, Result, plain_text_from_nbt_component, read_network_nbt};
use lodestone_model::{
    EntityAttributeModifier, EntityAttributeSnapshot, EntityMetadataUpdate, EntityPose, Identifier,
};

use crate::attribute_types::attribute_name;

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
const IDX_CUSTOM_NAME: u8 = 2;
const IDX_CUSTOM_NAME_VISIBLE: u8 = 3;
const IDX_POSE: u8 = 6;
const IDX_HEALTH: u8 = 9;
const IDX_BABY: u8 = 16;

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
    Float(f32),
    Bool(bool),
    /// An optional text component (used by custom name). Inner `None` = cleared.
    OptText(Option<String>),
    /// A pose enum id.
    Pose(u32),
    /// Consumed correctly but not surfaced.
    Consumed,
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
        SER_INT => {
            reader.var_i32()?;
            Value::Consumed
        }
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
            // holder type + holder profession + level, all VarInt.
            reader.var_i32()?;
            reader.var_i32()?;
            reader.var_i32()?;
            Value::Consumed
        }
        SER_POSE => Value::Pose(reader.var_i32()?.max(0) as u32),
        // Ids 21..=38 (excluding the global-pos serializer at 33) are all single
        // registry-holder / enum VarInts: cat/cow/wolf/frog/pig/… variants and
        // sound variants, painting variant, sniffer/armadillo/copper-golem/
        // weathering-copper states.
        21..=32 | 34..=38 => {
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
        // Genuinely complex, self-describing payloads mobs never emit. Rejected
        // rather than guessed at.
        SER_ITEM_STACK | SER_PARTICLE | SER_PARTICLES | SER_RESOLVABLE_PROFILE => {
            return Err(unknown_serializer(serializer));
        }
        other => return Err(unknown_serializer(other)),
    };
    Ok(value)
}

/// Decodes a `set_entity_data` metadata list into a version-free
/// [`EntityMetadataUpdate`], resolving 26.2's indices and serializers.
///
/// The reader is left positioned immediately after the `0xFF` terminator; the
/// caller asserts the payload is then empty (the misparse detector).
pub fn read_entity_metadata(reader: &mut Reader<'_>) -> Result<EntityMetadataUpdate> {
    let mut md = EntityMetadataUpdate::default();
    loop {
        let index = reader.u8()?;
        if index == EOF_MARKER {
            break;
        }
        let serializer = reader.var_i32()?;
        let value = decode_value(reader, serializer)?;
        match (index, value) {
            (IDX_SHARED_FLAGS, Value::Byte(b)) => md.flags = Some(b as u8),
            (IDX_CUSTOM_NAME, Value::OptText(t)) => md.custom_name = Some(t),
            (IDX_CUSTOM_NAME_VISIBLE, Value::Bool(b)) => md.custom_name_visible = Some(b),
            (IDX_POSE, Value::Pose(p)) => md.pose = Some(pose_from_id(p)),
            (IDX_HEALTH, Value::Float(f)) => md.health = Some(f),
            (IDX_BABY, Value::Bool(b)) => md.baby = Some(b),
            // Any other (index, value) is decoded for alignment but not surfaced.
            _ => {}
        }
    }
    Ok(md)
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
        // index 19 (pig variant), a holder VarInt we must consume but not surface
        bytes.push(19);
        bytes.extend(varint(28)); // PIG_VARIANT serializer id
        bytes.extend(varint(3)); // some registry id
        bytes.push(EOF_MARKER);

        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader).expect("decode");
        reader.ensure_empty().expect("no trailing bytes");

        assert_eq!(md.flags, Some(0x01));
        assert_eq!(md.custom_name, Some(Some("Hoglet".to_string())));
        assert_eq!(md.custom_name_visible, Some(true));
        assert_eq!(md.pose, Some(EntityPose::Crouching));
        assert_eq!(md.health, Some(10.0));
        assert_eq!(md.baby, Some(true));
    }

    /// An empty list (just the terminator) decodes to an empty update.
    #[test]
    fn empty_list_is_empty_update() {
        let bytes = [EOF_MARKER];
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader).expect("decode");
        reader.ensure_empty().expect("empty");
        assert!(md.is_empty());
    }

    /// A cleared custom name (present field, empty optional) surfaces as
    /// `Some(None)`, distinct from "field absent" (`None`).
    #[test]
    fn cleared_custom_name_is_some_none() {
        let mut bytes = Vec::new();
        bytes.push(IDX_CUSTOM_NAME);
        bytes.extend(varint(SER_OPTIONAL_COMPONENT));
        bytes.push(0); // absent
        bytes.push(EOF_MARKER);
        let mut reader = Reader::new(&bytes);
        let md = read_entity_metadata(&mut reader).expect("decode");
        reader.ensure_empty().expect("empty");
        assert_eq!(md.custom_name, Some(None));
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
        assert!(read_entity_metadata(&mut reader).is_err());
    }

    /// A complex serializer mobs never emit is rejected explicitly, not guessed.
    #[test]
    fn complex_serializer_is_rejected() {
        let mut bytes = Vec::new();
        bytes.push(5); // arbitrary index
        bytes.extend(varint(SER_ITEM_STACK));
        bytes.push(0);
        let mut reader = Reader::new(&bytes);
        assert!(read_entity_metadata(&mut reader).is_err());
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
}
