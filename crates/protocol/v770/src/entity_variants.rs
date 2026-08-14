//! Protocol 776 (Minecraft 26.2) mob cosmetic-variant registry tables.
//!
//! Several mobs carry their appearance as a `Holder<Variant>` in entity
//! metadata: the wire value is a registry id (`holderRegistry` writes `id + 1`,
//! reserving `0` for an inline direct holder mobs never send), and the id must
//! be resolved to a canonical registry key such as `minecraft:temperate`.
//!
//! # Why the tables live here, and why they are static
//!
//! Which registry id maps to which key is 26.2-specific, so it belongs in this
//! version crate (§3.4 — no per-version index escapes). The variant registries
//! themselves are data-driven and are synced to a real client during
//! configuration, but this crate deliberately resolves them from a static table
//! rather than from the `registry_data` those packets carry, for the same reason
//! every other id table here (entity types, items, attributes, sound events) is
//! static: the ids are stable for vanilla, and a static table needs no
//! cross-phase state.
//!
//! Note that the cross-phase state *does* exist now —
//! `crate::packets::registry::ClientRegistries` keeps the ordered entry names of
//! every synchronized registry, these variant registries included — so switching
//! a table here to a registry lookup is now a choice rather than a blocked one.
//! It is still deliberately not taken: a static table is faster and cannot be
//! absent, and the id outside a table resolves to `None` either way.
//! The ordering below is transcribed from each registry's vanilla bootstrap
//! *registration order* in the 26.2 source (`MappedRegistry` assigns ids in
//! registration order and transmits entries in id order), which is the same
//! authority the generated id tables in `generated/` rely on.
//!
//! An id outside a table (a datapack-added variant, or a future entry) resolves
//! to `None`: the caller then raises no variant rather than a wrong guess, so a
//! stale table degrades to "no override" rather than a misattribution.

/// `minecraft:cat_variant`, in registration order (`CatVariants`).
const CAT: &[&str] = &[
    "minecraft:tabby",
    "minecraft:black",
    "minecraft:red",
    "minecraft:siamese",
    "minecraft:british_shorthair",
    "minecraft:calico",
    "minecraft:persian",
    "minecraft:ragdoll",
    "minecraft:white",
    "minecraft:jellie",
    "minecraft:all_black",
];

/// `minecraft:wolf_variant`, in registration order (`WolfVariants`).
const WOLF: &[&str] = &[
    "minecraft:pale",
    "minecraft:spotted",
    "minecraft:snowy",
    "minecraft:black",
    "minecraft:ashen",
    "minecraft:rusty",
    "minecraft:woods",
    "minecraft:chestnut",
    "minecraft:striped",
];

/// The temperature-variant registries — `pig`, `cow`, `chicken`, `frog` — all
/// register `temperate`, `warm`, `cold` in that order.
const TEMPERATURE: &[&str] = &["minecraft:temperate", "minecraft:warm", "minecraft:cold"];

/// `minecraft:zombie_nautilus_variant`: only `temperate`, `warm` in 26.2.
const ZOMBIE_NAUTILUS: &[&str] = &["minecraft:temperate", "minecraft:warm"];

/// `minecraft:villager_type`, in registration order (`VillagerType.bootstrap`).
const VILLAGER_TYPE: &[&str] = &[
    "minecraft:desert",
    "minecraft:jungle",
    "minecraft:plains",
    "minecraft:savanna",
    "minecraft:snow",
    "minecraft:swamp",
    "minecraft:taiga",
];

/// `minecraft:villager_profession`, in registration order
/// (`VillagerProfession.bootstrap`); `none` is id 0.
const VILLAGER_PROFESSION: &[&str] = &[
    "minecraft:none",
    "minecraft:armorer",
    "minecraft:butcher",
    "minecraft:cartographer",
    "minecraft:cleric",
    "minecraft:farmer",
    "minecraft:fisherman",
    "minecraft:fletcher",
    "minecraft:leatherworker",
    "minecraft:librarian",
    "minecraft:mason",
    "minecraft:nitwit",
    "minecraft:shepherd",
    "minecraft:toolsmith",
    "minecraft:weaponsmith",
];

fn lookup(table: &[&'static str], id: i32) -> Option<&'static str> {
    usize::try_from(id).ok().and_then(|i| table.get(i)).copied()
}

/// Resolves an appearance-variant `Holder` to its canonical registry key from
/// the 26.2 entity-data `serializer` id and the decoded registry `id`
/// (holder wire value minus one). Returns `None` for a non-appearance
/// serializer or an id past the vanilla table.
///
/// The serializer ids are the `EntityDataSerializers` registration order:
/// 21 cat, 23 cow, 25 wolf, 27 frog, 28 pig, 30 chicken, 32 zombie-nautilus.
/// The interleaved odd/even neighbours (22/24/26/29/31 sound variants, 34
/// painting, 35..=38 enum states) are not appearance variants and are not
/// mapped here.
pub fn appearance_variant(serializer: i32, id: i32) -> Option<&'static str> {
    let table = match serializer {
        21 => CAT,
        23 | 28 | 30 | 27 => TEMPERATURE, // cow, pig, chicken, frog
        25 => WOLF,
        32 => ZOMBIE_NAUTILUS,
        _ => return None,
    };
    lookup(table, id)
}

/// Resolves a `minecraft:villager_type` id to its key.
pub fn villager_type(id: i32) -> Option<&'static str> {
    lookup(VILLAGER_TYPE, id)
}

/// Resolves a `minecraft:villager_profession` id to its key.
pub fn villager_profession(id: i32) -> Option<&'static str> {
    lookup(VILLAGER_PROFESSION, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_variants_share_one_order() {
        // cow (23), pig (28), chicken (30), frog (27) all map identically.
        for serializer in [23, 27, 28, 30] {
            assert_eq!(
                appearance_variant(serializer, 0),
                Some("minecraft:temperate")
            );
            assert_eq!(appearance_variant(serializer, 1), Some("minecraft:warm"));
            assert_eq!(appearance_variant(serializer, 2), Some("minecraft:cold"));
            assert_eq!(appearance_variant(serializer, 3), None);
        }
    }

    #[test]
    fn cat_and_wolf_boundaries() {
        assert_eq!(appearance_variant(21, 0), Some("minecraft:tabby"));
        assert_eq!(appearance_variant(21, 10), Some("minecraft:all_black"));
        assert_eq!(appearance_variant(21, 11), None);
        assert_eq!(appearance_variant(25, 0), Some("minecraft:pale"));
        assert_eq!(appearance_variant(25, 8), Some("minecraft:striped"));
        assert_eq!(appearance_variant(25, 9), None);
    }

    #[test]
    fn zombie_nautilus_has_two_entries() {
        assert_eq!(appearance_variant(32, 1), Some("minecraft:warm"));
        assert_eq!(appearance_variant(32, 2), None);
    }

    #[test]
    fn non_appearance_serializer_and_negative_id_are_none() {
        assert_eq!(appearance_variant(22, 0), None); // cat sound variant
        assert_eq!(appearance_variant(34, 0), None); // painting variant
        assert_eq!(appearance_variant(21, -1), None);
    }

    #[test]
    fn villager_tables() {
        assert_eq!(villager_type(0), Some("minecraft:desert"));
        assert_eq!(villager_type(2), Some("minecraft:plains"));
        assert_eq!(villager_type(6), Some("minecraft:taiga"));
        assert_eq!(villager_type(7), None);
        assert_eq!(villager_profession(0), Some("minecraft:none"));
        assert_eq!(villager_profession(5), Some("minecraft:farmer"));
        assert_eq!(villager_profession(14), Some("minecraft:weaponsmith"));
        assert_eq!(villager_profession(15), None);
    }
}
