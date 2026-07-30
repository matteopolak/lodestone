//! Public entity-type id→name resolution for protocol 776.
//!
//! `add_entity` carries the entity type as a network **registry id** (a
//! varint), not its identifier. That id→name mapping is version-specific data —
//! ids shift between releases as the registry grows — so it lives here in the
//! version crate, generated from Mojang's own `registries.json`, and never in a
//! shared crate. The generated array is the single source of truth; this module
//! is only the thin bounds-checked accessor over it.

use crate::generated_entity_types::ENTITY_TYPE_NAMES;
pub use crate::generated_entity_types::TYPE_COUNT;

/// Resolves a network entity-type id to its canonical `minecraft:*` identifier.
///
/// Returns `None` for ids outside `0..TYPE_COUNT`, so a malformed or
/// future-version id surfaces as an explicit miss rather than a panic or a
/// silently wrong type.
#[must_use]
pub fn entity_type_name(id: i32) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|index| ENTITY_TYPE_NAMES.get(index).copied())
}

/// Resolves a canonical `minecraft:*` identifier back to its network
/// entity-type id — the reverse of [`entity_type_name`], needed by the
/// encode side (`add_entity`) rather than decode. A linear scan over the
/// generated table: entity spawns are rare relative to tick rate, so this
/// need not be a hash map.
#[must_use]
pub fn entity_type_id(name: &str) -> Option<i32> {
    ENTITY_TYPE_NAMES
        .iter()
        .position(|&candidate| candidate == name)
        .map(|index| index as i32)
}

/// Resolves a **split** identifier — namespace and path separately — back to its
/// network entity-type id.
///
/// The same lookup as [`entity_type_id`], for a caller holding a parsed
/// identifier (`lodestone_model::ResourceKey`) rather than a joined string.
/// Exists so the per-tick physics path can key on a `ResourceKey` without
/// `to_string()`-ing one allocation per nearby entity per tick; the generated
/// table stores joined `namespace:path` strings, so the split is done here on the
/// table side instead.
///
/// Returns `None` for any namespace other than `minecraft`, since every entry in
/// the table is `minecraft:*`. A plugin-namespaced type is a genuine miss, not a
/// type to guess at.
#[must_use]
pub fn entity_type_id_parts(namespace: &str, path: &str) -> Option<i32> {
    if namespace != "minecraft" {
        return None;
    }
    ENTITY_TYPE_NAMES
        .iter()
        .position(|candidate| candidate.strip_prefix("minecraft:") == Some(path))
        .map(|index| index as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_type_id_parts_agrees_with_the_joined_lookup() {
        // Every id round-trips through the split form, so the two lookups cannot
        // drift apart. A one-off `minecraft:` prefix bug would fail here.
        for id in 0..i32::try_from(TYPE_COUNT).unwrap() {
            let name = entity_type_name(id).expect("id within TYPE_COUNT");
            let path = name
                .strip_prefix("minecraft:")
                .expect("every generated name is minecraft-namespaced");
            assert_eq!(entity_type_id_parts("minecraft", path), Some(id), "id {id}");
        }
    }

    #[test]
    fn entity_type_id_parts_rejects_other_namespaces_and_unknown_paths() {
        // A plugin namespace must miss even when the path happens to collide with
        // a vanilla one — `mypack:zombie` is not `minecraft:zombie`.
        assert_eq!(entity_type_id_parts("mypack", "zombie"), None);
        assert_eq!(entity_type_id_parts("minecraft", "not_a_real_entity"), None);
        // And a path that already carries the prefix must not match either, or a
        // caller passing an unsplit string would silently resolve.
        assert_eq!(entity_type_id_parts("minecraft", "minecraft:zombie"), None);
    }

    #[test]
    fn entity_type_id_is_the_inverse_of_entity_type_name() {
        assert_eq!(entity_type_name(0).and_then(entity_type_id), Some(0));
        let zombie_id = entity_type_id("minecraft:zombie").expect("zombie is a real entity type");
        assert_eq!(entity_type_name(zombie_id), Some("minecraft:zombie"));
        // Every id in the generated table round-trips through name and back.
        for id in 0..i32::try_from(TYPE_COUNT).unwrap() {
            let name = entity_type_name(id).expect("id within TYPE_COUNT");
            assert_eq!(entity_type_id(name), Some(id), "id {id} ({name})");
        }
    }

    #[test]
    fn entity_type_id_rejects_unknown_names() {
        assert_eq!(
            entity_type_id("minecraft:definitely_not_a_real_entity"),
            None
        );
    }
}
