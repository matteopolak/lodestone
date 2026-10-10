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
//! The appearance-variant tables are in **sorted key order**, the order a real
//! server transmits these data-driven registries (measured from the captured
//! `registry_data` payloads, and pinned by a test); bootstrap registration order
//! is wrong for them.
//!
//! An id outside a table (a datapack-added variant, or a future entry) resolves
//! to `None`: the caller then raises no variant rather than a wrong guess, so a
//! stale table degrades to "no override" rather than a misattribution.

/// `minecraft:cat_variant` in registry order. These variant registries are data
/// driven, so a server sends them sorted by key and the holder id indexes that
/// order; `tests::tables_match_the_captured_registry_data` pins every table
/// below to the captured `registry_data` payloads of both 26.2 and 26.3.
const CAT: &[&str] = &[
    "minecraft:all_black",
    "minecraft:black",
    "minecraft:british_shorthair",
    "minecraft:calico",
    "minecraft:jellie",
    "minecraft:persian",
    "minecraft:ragdoll",
    "minecraft:red",
    "minecraft:siamese",
    "minecraft:tabby",
    "minecraft:white",
];

/// `minecraft:wolf_variant` in registry (sorted) order.
const WOLF: &[&str] = &[
    "minecraft:ashen",
    "minecraft:black",
    "minecraft:chestnut",
    "minecraft:pale",
    "minecraft:rusty",
    "minecraft:snowy",
    "minecraft:spotted",
    "minecraft:striped",
    "minecraft:woods",
];

/// The temperature-variant registries (`pig`, `cow`, `chicken`, `frog`) all
/// hold `cold`, `temperate`, `warm` in that sorted order.
const TEMPERATURE: &[&str] = &["minecraft:cold", "minecraft:temperate", "minecraft:warm"];

/// `minecraft:zombie_nautilus_variant`: only `temperate`, `warm`.
const ZOMBIE_NAUTILUS: &[&str] = &["minecraft:temperate", "minecraft:warm"];

/// `minecraft:villager_type`, in registration order (the villager type bootstrap).
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
/// (the villager profession bootstrap); `none` is id 0.
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

/// `minecraft:painting_variant`'s 51 entries **in registry order**, which is
/// the order a holder id indexes.
///
/// # The order is alphabetical, and that is measured rather than assumed
///
/// It would be natural to transcribe the painting variants bootstrap's
/// registration order, and that is **wrong**: painting variants are a data-pack
/// registry loaded from `data/minecraft/painting_variant/*.json` through the
/// resource manager, which lists keys sorted. Decoding the repo's own captured
/// `registry_data` payload for this registry
/// (`tests/fixtures/registry_data_painting_variant.hex`, taken from a real
/// vanilla 26.2 server) gives id 0 = `minecraft:alban`, not the bootstrap
/// class's first entry `kebab`, and all 51 names in exactly `sorted()` order.
/// The fixture is the outside source here; the bootstrap class is not.
///
/// # What a data pack breaks
///
/// A pack that adds or removes a variant shifts every id after it, and this
/// table would then name the wrong painting — silently, since every id still
/// resolves. The authoritative per-server answer is
/// [`ClientRegistries::entry_names`](crate::packets::registry::ClientRegistries::entry_names),
/// which already holds this registry's names as sent; wiring it here needs a
/// registry handle threaded into `read_entity_metadata`, whose signature has
/// more than fifty call sites. This table matches the house pattern the
/// appearance-variant tables above already set, and carries the same hazard.
const PAINTING: &[&str] = &[
    "minecraft:alban",
    "minecraft:aztec",
    "minecraft:aztec2",
    "minecraft:backyard",
    "minecraft:baroque",
    "minecraft:bomb",
    "minecraft:bouquet",
    "minecraft:burning_skull",
    "minecraft:bust",
    "minecraft:cavebird",
    "minecraft:changing",
    "minecraft:cotan",
    "minecraft:courbet",
    "minecraft:creebet",
    "minecraft:dennis",
    "minecraft:donkey_kong",
    "minecraft:earth",
    "minecraft:endboss",
    "minecraft:fern",
    "minecraft:fighters",
    "minecraft:finding",
    "minecraft:fire",
    "minecraft:graham",
    "minecraft:humble",
    "minecraft:kebab",
    "minecraft:lowmist",
    "minecraft:match",
    "minecraft:meditative",
    "minecraft:orb",
    "minecraft:owlemons",
    "minecraft:passage",
    "minecraft:pigscene",
    "minecraft:plant",
    "minecraft:pointer",
    "minecraft:pond",
    "minecraft:pool",
    "minecraft:prairie_ride",
    "minecraft:sea",
    "minecraft:skeleton",
    "minecraft:skull_and_roses",
    "minecraft:stage",
    "minecraft:sunflowers",
    "minecraft:sunset",
    "minecraft:tides",
    "minecraft:unpacked",
    "minecraft:void",
    "minecraft:wanderer",
    "minecraft:wasteland",
    "minecraft:water",
    "minecraft:wind",
    "minecraft:wither",
];

fn lookup(table: &[&'static str], id: i32) -> Option<&'static str> {
    usize::try_from(id).ok().and_then(|i| table.get(i)).copied()
}

/// Resolves an appearance-variant `Holder` to its canonical registry key from
/// the 26.2 entity-data `serializer` id and the decoded registry `id`
/// (holder wire value minus one). Returns `None` for a non-appearance
/// serializer or an id past the vanilla table.
///
/// The serializer ids are the entity-data serializer registration order:
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

/// Resolves a `minecraft:painting_variant` holder id to its registry key.
///
/// The id is the wire value **minus one** (a `Holder` sends `id + 1`, with 0
/// reserved for an inline direct value that vanilla never sends for a
/// painting), exactly as [`appearance_variant`]'s is. `None` for an id past
/// the vanilla table, which is a data-pack-added variant this build has no
/// size or texture for.
pub fn painting_variant(id: i32) -> Option<&'static str> {
    lookup(PAINTING, id)
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
            assert_eq!(appearance_variant(serializer, 0), Some("minecraft:cold"));
            assert_eq!(
                appearance_variant(serializer, 1),
                Some("minecraft:temperate")
            );
            assert_eq!(appearance_variant(serializer, 2), Some("minecraft:warm"));
            assert_eq!(appearance_variant(serializer, 3), None);
        }
    }

    #[test]
    fn cat_and_wolf_boundaries() {
        assert_eq!(appearance_variant(21, 0), Some("minecraft:all_black"));
        assert_eq!(appearance_variant(21, 10), Some("minecraft:white"));
        assert_eq!(appearance_variant(21, 11), None);
        assert_eq!(appearance_variant(25, 0), Some("minecraft:ashen"));
        assert_eq!(appearance_variant(25, 8), Some("minecraft:woods"));
        assert_eq!(appearance_variant(25, 9), None);
    }

    /// Every appearance table equals the entry order of the `registry_data`
    /// payload a real server sent, for both protocols that share these tables.
    #[test]
    fn tables_match_the_captured_registry_data() {
        use lodestone_core::{Ctx, Decode, Reader};

        use crate::packets::registry::{ClientRegistries, RegistryData};

        let dirs = [
            (776, concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")),
            (777, concat!(env!("CARGO_MANIFEST_DIR"), "/../26.3/fixtures/server-config")),
        ];
        for (version, dir) in dirs {
            for (registry, serializer) in [
                ("cat_variant", 21),
                ("cow_variant", 23),
                ("wolf_variant", 25),
                ("frog_variant", 27),
                ("pig_variant", 28),
                ("chicken_variant", 30),
                ("zombie_nautilus_variant", 32),
            ] {
                let path = format!("{dir}/registry_data_{registry}.hex");
                let text = std::fs::read_to_string(&path).expect(&path);
                let bytes: Vec<u8> = text
                    .lines()
                    .filter(|l| !l.trim_start().starts_with('#'))
                    .flat_map(str::split_whitespace)
                    .map(|t| u8::from_str_radix(t, 16).unwrap())
                    .collect();
                let data = RegistryData::decode(&mut Reader::new(&bytes), Ctx { version })
                    .expect("captured registry_data decodes");
                let mut registries = ClientRegistries::default();
                registries.apply(data);
                let sent = registries
                    .entry_names(&format!("minecraft:{registry}"))
                    .expect("registry present");
                for (id, name) in sent.iter().enumerate() {
                    assert_eq!(
                        appearance_variant(serializer, id as i32),
                        Some(name.as_str()),
                        "{version} {registry} id {id}"
                    );
                }
                assert_eq!(appearance_variant(serializer, sent.len() as i32), None, "{registry} length");
            }
        }
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
