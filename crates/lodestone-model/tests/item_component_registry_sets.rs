//! The shape gates for the item-component types that carry a value a decoder
//! reads off the wire and would otherwise have nowhere to put.
//!
//! # Why these are worth a test at all
//!
//! Each type here exists to hold a field that used to be read for byte
//! alignment and dropped. That failure mode is invisible to every instrument
//! this repo has: the packet decodes, the stream stays in sync,
//! `cargo xtask connectedness` scores it fully connected, and a round trip
//! agrees with itself about a field neither half retains. The only thing that
//! can fail is a test that names the field — which is why this file's real job
//! is to **not compile** against a build where the field is absent.
//!
//! What it asserts beyond existence is the two properties a decoder can get
//! wrong without any packet failing:
//!
//! * a registry set's tag arm is not the same value as an empty id list, and
//! * the bit-packed float fields come back out in the order they went in.

use lodestone_model::{
    ArmorTrim, BlocksAttacks, ConsumeEffect, DamageReduction, ItemComponents, MobEffectInstance,
    RegistrySet, Text,
};

/// A tag-form registry set and an explicitly-empty one are **different
/// values**, and neither is a substitute for the other.
///
/// A tag's membership never reaches the client, so a decoder that reduced the
/// tag arm to its (empty) id list would turn "every item the server put in
/// `#minecraft:planks`" into "no item at all" — a total inversion of the
/// component's meaning, with no byte out of place to give it away.
#[test]
fn a_tag_set_is_not_an_empty_id_set() {
    let tag = RegistrySet::Tag("minecraft:planks".to_owned());
    let empty = RegistrySet::Ids(Vec::new());
    assert_ne!(tag, empty);
    assert!(
        tag.explicit_ids().is_empty(),
        "a tag names no ids; a caller that needs membership must match the arm"
    );
    assert_eq!(RegistrySet::Ids(vec![7, 3]).explicit_ids(), &[7, 3]);
}

/// The six fields of a mob-effect instance are independently addressable.
///
/// The three trailing flags are deliberately *not* all the same: an instance
/// built with `ambient == show_particles == show_icon` cannot tell a decoder
/// that transposed two of them from one that read them in order.
#[test]
fn a_mob_effect_instance_keeps_all_six_of_its_fields() {
    let effect = MobEffectInstance {
        effect_id: 10,
        amplifier: 2,
        duration_ticks: 900,
        ambient: false,
        show_particles: true,
        show_icon: false,
    };
    assert_eq!(effect.effect_id, 10);
    assert_eq!(effect.amplifier, 2);
    assert_eq!(effect.duration_ticks, 900);
    assert!(!effect.ambient);
    assert!(effect.show_particles);
    assert!(!effect.show_icon);
}

/// The two consume-effect variants that carry a float read it back through
/// their accessors, and the payload-free variants stay distinguishable.
#[test]
fn consume_effect_floats_survive_the_bit_packing() {
    let apply = ConsumeEffect::ApplyEffects {
        effects: vec![MobEffectInstance {
            effect_id: 4,
            amplifier: 0,
            duration_ticks: 200,
            ambient: true,
            show_particles: false,
            show_icon: true,
        }],
        probability_bits: 0.375_f32.to_bits(),
    };
    assert_eq!(apply.probability(), Some(0.375));
    assert_eq!(apply.teleport_diameter(), None);

    let teleport = ConsumeEffect::TeleportRandomly {
        diameter_bits: 18.5_f32.to_bits(),
    };
    assert_eq!(teleport.teleport_diameter(), Some(18.5));
    assert_eq!(teleport.probability(), None);

    assert_ne!(ConsumeEffect::ClearAllEffects, ConsumeEffect::PlaySound);
    assert_eq!(ConsumeEffect::ClearAllEffects.probability(), None);
}

/// A blocks-attacks component's nine floats come back in the order they went
/// in, and its two registry sets do not swap.
///
/// Every value is distinct and none is `0.0` or `1.0`: nine same-typed fields
/// in a row is exactly the shape where a round-numbered fixture makes a
/// transposition and a correct read agree.
#[test]
fn blocks_attacks_accessors_return_the_fields_in_wire_order() {
    let component = BlocksAttacks::new(
        0.25,
        4.75,
        vec![
            DamageReduction::new(97.5, Some(RegistrySet::Ids(vec![11])), 2.5, 0.125),
            DamageReduction::new(45.5, None, 6.75, 0.375),
        ],
        3.5,
        1.25,
        0.625,
        Some(RegistrySet::Tag("minecraft:bypasses_shield".to_owned())),
    );

    assert_eq!(component.block_delay_seconds(), 0.25);
    assert_eq!(component.disable_cooldown_scale(), 4.75);
    assert_eq!(component.item_damage_threshold(), 3.5);
    assert_eq!(component.item_damage_base(), 1.25);
    assert_eq!(component.item_damage_factor(), 0.625);
    assert_eq!(
        component.bypassed_by,
        Some(RegistrySet::Tag("minecraft:bypasses_shield".to_owned()))
    );

    let [first, second] = &component.damage_reductions[..] else {
        panic!("two reduction rules");
    };
    assert_eq!(first.horizontal_blocking_angle(), 97.5);
    assert_eq!(first.base(), 2.5);
    assert_eq!(first.factor(), 0.125);
    assert_eq!(first.damage_types, Some(RegistrySet::Ids(vec![11])));
    assert_eq!(second.horizontal_blocking_angle(), 45.5);
    assert_eq!(second.base(), 6.75);
    assert_eq!(second.factor(), 0.375);
    assert_eq!(
        second.damage_types, None,
        "a rule with no set applies to every damage type, which is a different \
         statement from a rule whose set is empty"
    );
}

/// A default `ArmorTrim` leaves all four inline-only fields unset.
///
/// That is the state a registry-reference trim must decode to: `Some(false)`
/// for the decal, or an empty-but-present description, would claim the wire
/// said something it never carried.
#[test]
fn a_trims_inline_only_fields_default_to_absent() {
    let trim = ArmorTrim::default();
    assert_eq!(trim.material_description, None);
    assert_eq!(trim.pattern_description, None);
    assert_eq!(trim.pattern_decal, None);
    assert!(trim.material_asset_overrides.is_empty());

    let inline = ArmorTrim {
        material: "obsidian".to_owned(),
        pattern: "eclipse".to_owned(),
        material_description: Some(Text::literal("Obsidian Material")),
        material_asset_overrides: vec![("minecraft:iron".to_owned(), "obsidian_darker".to_owned())],
        pattern_description: Some(Text::literal("Eclipse Armor Trim")),
        pattern_decal: Some(true),
    };
    assert_ne!(
        inline,
        ArmorTrim {
            material: "obsidian".to_owned(),
            pattern: "eclipse".to_owned(),
            ..Default::default()
        },
        "two trims naming the same material and pattern still differ when only \
         one carries an inline definition's own body"
    );
}

/// Every component field added for a value a decoder was reading and dropping
/// defaults to absent, so `None`/empty means "the patch did not mention it"
/// rather than a guessed value.
#[test]
fn the_registry_set_components_default_to_absent() {
    let components = ItemComponents::default();
    assert_eq!(components.repairable_items, None);
    assert_eq!(components.equippable_allowed_entities, None);
    assert_eq!(components.damage_resistant, None);
    assert_eq!(components.blocks_attacks, None);
    assert_eq!(components.provides_banner_patterns, None);
    assert!(components.consume_effects.is_empty());
    assert!(components.death_protection_effects.is_empty());
}
