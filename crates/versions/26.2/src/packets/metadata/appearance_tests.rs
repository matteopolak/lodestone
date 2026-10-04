//! Anchors and controls for the appearance table.

use super::appearance::{ROWS, Row, baby_index};
use super::*;
use lodestone_model::MobAppearance;

fn varint(value: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(value);
    w.into_vec()
}

const DUMP: &str = include_str!("../../../tests/support/entity_data_index_jvm.txt");

/// The `(index, serializer)` the jar dump records for `Owner.FIELD`.
fn dumped(accessor: &str) -> Option<(u8, i32)> {
    DUMP.lines().filter(|l| !l.starts_with('#')).find_map(|line| {
        let mut cols = line.split_whitespace();
        let index = cols.next()?.parse().ok()?;
        (cols.next()? == accessor).then(|| (index, cols.next().unwrap().parse().unwrap()))
    })
}

fn check_row(index: u8, accessor: &str, serializer: i32) -> core::result::Result<(), String> {
    match dumped(accessor) {
        None => Err(format!("{accessor} is not in the jar dump")),
        Some((i, s)) if i == index && s == serializer => Ok(()),
        Some((i, s)) => Err(format!("{accessor}: row says {index}/{serializer}, jar says {i}/{s}")),
    }
}

/// Every row's index and serializer are the jar's own, so no row is a hand count.
#[test]
fn every_appearance_row_matches_the_jar_dump() {
    assert!(ROWS.len() > 30, "the table is suspiciously small");
    for row in ROWS {
        check_row(row.index, row.accessor, row.serializer).unwrap();
    }
}

/// Control: the checker rejects a row off by one in index, and one whose
/// serializer is wrong, or the test above proves nothing.
#[test]
fn the_dump_anchor_rejects_a_drifted_row() {
    assert!(check_row(19, "Rabbit.DATA_TYPE_ID", SER_INT).is_err(), "index off by one");
    assert!(check_row(18, "Rabbit.DATA_TYPE_ID", SER_BYTE).is_err(), "wrong serializer");
    assert!(check_row(18, "Rabbit.NO_SUCH_FIELD", SER_INT).is_err(), "unknown accessor");
    assert!(check_row(18, "Rabbit.DATA_TYPE_ID", SER_INT).is_ok(), "control: the real row passes");
}

fn entry(index: u8, serializer: i32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![index];
    bytes.extend(varint(serializer));
    bytes.extend_from_slice(payload);
    bytes.push(EOF_MARKER);
    bytes
}

fn decode(class: Option<MetadataClass>, bytes: &[u8]) -> EntityMetadataUpdate {
    let mut reader = Reader::new(bytes);
    let decoded = read_entity_metadata(
        &mut reader,
        TrackedEntity { class, living: true, mob: true },
    )
    .expect("decode");
    reader.ensure_empty().expect("no trailing bytes");
    decoded.metadata
}

fn payload_for(serializer: i32, scalar: i64) -> Vec<u8> {
    match serializer {
        SER_BYTE => vec![scalar as u8],
        SER_BOOLEAN => vec![u8::from(scalar != 0)],
        SER_LONG => {
            let mut w = Writer::default();
            w.var_i64(scalar);
            w.into_vec()
        }
        _ => varint(scalar as i32),
    }
}

/// Each row fires for its own class and only its own: the same bytes under a class
/// the row does not name raise nothing in the appearance block.
#[test]
fn each_row_fires_for_its_class_and_no_other() {
    let wrong_classes = [None, Some(MetadataClass::Creeper), Some(MetadataClass::Dragon)];
    for row in ROWS {
        let bytes = entry(row.index, row.serializer, &payload_for(row.serializer, 1));
        for &class in row.classes {
            let md = decode(Some(class), &bytes);
            assert!(
                !md.appearance.is_empty(),
                "{} under {class:?} raised nothing",
                row.accessor
            );
            // The raised field is exactly what the row's own setter writes.
            let mut expected = MobAppearance::default();
            (row.raise)(&mut expected, 1);
            assert_eq!(md.appearance, expected, "{}", row.accessor);
        }
        for wrong in wrong_classes {
            if wrong.is_some_and(|c| row.classes.contains(&c)) {
                continue;
            }
            assert!(
                decode(wrong, &bytes).appearance.is_empty(),
                "{} leaked into {wrong:?}",
                row.accessor
            );
        }
    }
}

fn single(class: MetadataClass, index: u8, serializer: i32, payload: &[u8]) -> MobAppearance {
    decode(Some(class), &entry(index, serializer, payload)).appearance
}

/// Literal bytes, written by hand rather than through the table's own payload
/// builder, so a symmetric misreading cannot hide.
#[test]
fn literal_wire_bytes_land_in_the_right_fields() {
    // A killer rabbit: coat id 99 as a VarInt.
    assert_eq!(single(MetadataClass::Rabbit, 18, 1, &[99]).rabbit_type, Some(99));
    // A goat that lost its left horn: boolean false at 19, right horn untouched.
    let goat = single(MetadataClass::Goat, 19, 8, &[0]);
    assert_eq!(goat.goat_left_horn, Some(false));
    assert_eq!(goat.goat_right_horn, None, "an unmentioned horn stays at its default");
    // A shulker dyed blue (dye ordinal 11) as a byte at 18.
    assert_eq!(single(MetadataClass::Shulker, 18, 0, &[11]).shulker_color, Some(11));
    // A wolf's anger end time is a VarLong: 300 = 0xAC 0x02.
    assert_eq!(single(MetadataClass::Wolf, 22, 2, &[0xAC, 0x02]).wolf_anger_end_time, Some(300));
    // An armadillo state ordinal rides serializer 36.
    assert_eq!(single(MetadataClass::Armadillo, 18, 36, &[2]).armadillo_state, Some(2));
    // A tropical fish's packed variant is a plain VarInt (0x00FF_0305 = 16_712_453).
    assert_eq!(
        single(MetadataClass::TropicalFish, 17, 1, &[0x85, 0x86, 0xFC, 0x07]).tropical_fish_variant,
        Some(0x00FF_0305)
    );
}

/// The merge keeps a field a later packet does not mention.
#[test]
fn merging_updates_keeps_unreported_fields() {
    let mut state = MobAppearance { goat_left_horn: Some(false), ..MobAppearance::default() };
    state.merge(&MobAppearance { goat_right_horn: Some(false), ..MobAppearance::default() });
    assert_eq!(state.goat_left_horn, Some(false));
    assert_eq!(state.goat_right_horn, Some(false));
    state.merge(&MobAppearance::default());
    assert_eq!(state.goat_left_horn, Some(false), "an empty update changes nothing");
}

// Types whose class chain reaches the ageable base (index 16), the zombie
// family or zoglin (their own 16), and the piglin (17), read from the decompiled
// hierarchy. `BABYLESS` carries an unrelated boolean at 16.
const AGEABLE: &[&str] = &[
    "armadillo", "axolotl", "bee", "camel", "camel_husk", "cat", "chicken", "cow", "dolphin",
    "donkey", "fox", "frog", "glow_squid", "goat", "happy_ghast", "hoglin", "horse", "llama",
    "magma_cube", "mooshroom", "mule", "nautilus", "ocelot", "panda", "parrot", "pig",
    "polar_bear", "rabbit", "sheep", "skeleton_horse", "slime", "sniffer", "squid", "strider",
    "sulfur_cube", "trader_llama", "turtle", "villager", "wandering_trader", "wolf",
    "zombie_horse", "zombie_nautilus", "zombie", "husk", "drowned", "zombie_villager",
    "zombified_piglin", "zoglin",
];
const BABYLESS: &[&str] = &[
    "cod", "salmon", "pufferfish", "tropical_fish", "tadpole", "piglin_brute", "allay", "ghast",
    "guardian", "elder_guardian", "pillager", "vindicator", "evoker", "illusioner", "ravager",
    "witch", "skeleton", "bogged", "creaking", "creeper", "enderman", "blaze",
];

#[test]
fn every_ageable_type_reads_its_baby_flag_and_no_other_does() {
    for name in AGEABLE {
        let class = metadata_class(&format!("minecraft:{name}"));
        assert_eq!(baby_index(class), Some(16), "{name} must read the baby flag at 16");
        let md = decode(class, &entry(16, SER_BOOLEAN, &[1]));
        assert_eq!(md.baby, Some(true), "{name}");
    }
    for name in BABYLESS {
        let class = metadata_class(&format!("minecraft:{name}"));
        assert_eq!(baby_index(class), None, "{name} has no baby flag");
        assert_eq!(decode(class, &entry(16, SER_BOOLEAN, &[1])).baby, None, "{name}");
    }
}

/// The piglin's baby flag is index 17; its 16 is the zombification immunity that
/// every overworld piglin carries. Reading 16 as age made them all babies.
#[test]
fn a_piglin_is_a_baby_by_index_17_and_not_by_its_immunity_flag() {
    let class = metadata_class("minecraft:piglin");
    assert_eq!(decode(class, &entry(16, SER_BOOLEAN, &[1])).baby, None);
    assert_eq!(decode(class, &entry(17, SER_BOOLEAN, &[1])).baby, Some(true));
    assert_eq!(decode(class, &entry(17, SER_BOOLEAN, &[0])).baby, Some(false));
}
