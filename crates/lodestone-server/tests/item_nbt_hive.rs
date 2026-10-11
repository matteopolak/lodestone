//! A hive item's bees and honey level in the save-file item form.
//!
//! The expected compound is written out by hand in the reference shape: a
//! `minecraft:bees` list of `{entity_data, ticks_in_hive, min_ticks_in_hive}`
//! entries and a `minecraft:block_state` compound of string properties, both
//! under `components`.

use lodestone_core::Nbt;
use lodestone_model::BeeOccupant;
use lodestone_server::item_nbt::{custom_data_from_nbt, stack_from_nbt, stack_to_nbt};

fn bee() -> Nbt {
    Nbt::Compound(vec![
        ("id".to_owned(), Nbt::String("minecraft:bee".to_owned())),
        ("HasNectar".to_owned(), Nbt::Byte(1)),
    ])
}

fn saved() -> Nbt {
    let entry = Nbt::Compound(vec![
        ("entity_data".to_owned(), bee()),
        ("ticks_in_hive".to_owned(), Nbt::Int(123)),
        ("min_ticks_in_hive".to_owned(), Nbt::Int(2400)),
    ]);
    Nbt::Compound(vec![
        ("id".to_owned(), Nbt::String("minecraft:bee_nest".to_owned())),
        ("count".to_owned(), Nbt::Int(1)),
        (
            "components".to_owned(),
            Nbt::Compound(vec![
                ("minecraft:bees".to_owned(), Nbt::List { element_type: lodestone_core::NbtTag::Compound, elements: vec![entry] }),
                (
                    "minecraft:block_state".to_owned(),
                    Nbt::Compound(vec![("honey_level".to_owned(), Nbt::String("5".to_owned()))]),
                ),
            ]),
        ),
    ])
}

#[test]
fn a_hive_item_reads_its_bees_and_honey_level() {
    let stack = stack_from_nbt(&saved()).expect("reads");
    assert!(!stack.components.has_unmodeled);
    assert_eq!(
        stack.components.bees,
        vec![BeeOccupant {
            entity_data: custom_data_from_nbt(&bee()).expect("bee compound"),
            ticks_in_hive: 123,
            min_ticks_in_hive: 2400
        }]
    );
    assert_eq!(stack.components.block_state, vec![("honey_level".to_owned(), "5".to_owned())]);
}

#[test]
fn a_hive_item_writes_the_reference_shape_back() {
    let stack = stack_from_nbt(&saved()).expect("reads");
    let persisted = stack_to_nbt(&stack);
    assert!(persisted.complete, "bees and block_state have saved forms");
    let Nbt::Compound(want) = saved() else { unreachable!() };
    let Some((_, Nbt::Compound(want_components))) = want.iter().find(|(k, _)| k == "components") else { unreachable!() };
    let Some((_, Nbt::Compound(got_components))) = persisted.fields.iter().find(|(k, _)| k == "components") else {
        panic!("components written: {:?}", persisted.fields)
    };
    for (name, value) in want_components {
        let got = got_components.iter().find(|(k, _)| k == name).map(|(_, v)| v);
        assert_eq!(got, Some(value), "{name}");
    }
    assert_eq!(got_components.len(), want_components.len());
}
