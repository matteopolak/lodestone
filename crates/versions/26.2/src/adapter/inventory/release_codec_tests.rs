use super::*;
use crate::dialect::FixedRegistryMappings;
use crate::packets::registry::PackedRegistryEntry;
use lodestone_data::GameDataVersion;
use lodestone_model::{ItemAnimationKind, ItemFloatValue, ItemIntegerValue};

fn decode_fixed(kind: FixedRegistryKind, id: i32) -> Option<i32> {
    if kind != FixedRegistryKind::DataComponent {
        return (id >= 0).then_some(id);
    }
    Some(match id {
        40 => 111,
        41 => 112,
        43 => 113,
        44 => 114,
        58 => 56,
        63 => 61,
        76 => 74,
        84 => 115,
        85 => 116,
        86 => 117,
        87 => 118,
        117 => 119,
        118 => 120,
        119 => 121,
        120 => 122,
        121 => 123,
        0..=39 => id,
        _ => return None,
    })
}

fn unused_encode(_: FixedRegistryKind, _: i32) -> Option<i32> { None }
fn unused_name(_: FixedRegistryKind, _: i32) -> Option<&'static str> { None }

static LATEST_FIXED: FixedRegistryMappings = FixedRegistryMappings {
    decode: decode_fixed, encode: unused_encode, name: unused_name,
};

fn latest(registries: &ClientRegistries) -> StackCodecContext<'_> {
    StackCodecContext::new(
        ProtocolDialect::v26_2().with_game_data_version(GameDataVersion::V26_3)
            .with_fixed_registries(&LATEST_FIXED),
        registries,
    )
}

fn var(out: &mut Vec<u8>, value: u32) {
    let mut value = value;
    while value >= 128 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn identifier(out: &mut Vec<u8>, value: &str) {
    var(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

fn text(out: &mut Vec<u8>, value: &str) {
    out.push(8);
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn registry(registries: &mut ClientRegistries, name: &str, entries: &[&str]) {
    registries.apply(RegistryData {
        registry: name.to_owned(),
        entries: entries.iter().map(|id| PackedRegistryEntry { id: (*id).to_owned(), data: None }).collect(),
    });
}

fn complete(bytes: &[u8], context: &StackCodecContext<'_>) -> ItemStack {
    let mut reader = Reader::new(bytes);
    let DecodedStack::Complete(Some(stack)) = read_item_stack_with(&mut reader, context).expect("complete stack") else {
        panic!("stack was partial or empty");
    };
    reader.ensure_empty().expect("length-exact stack");
    stack
}

#[test]
fn ordinary_and_template_orders_use_selected_item_map() {
    let registries = ClientRegistries::default();
    let context = latest(&registries);
    let mut ordinary = vec![3];
    var(&mut ordinary, 1012);
    ordinary.extend_from_slice(&[0, 0]);
    let stack = complete(&ordinary, &context);
    assert_eq!(stack.item.to_string(), "minecraft:diamond");
    assert_eq!(stack.count, 3);
    let mut template = Vec::new();
    var(&mut template, 1012);
    template.extend_from_slice(&[3, 0, 0]);
    let mut reader = Reader::new(&template);
    let stack = read_item_stack_template_with(&mut reader, &context).expect("template");
    assert_eq!(stack.item.to_string(), "minecraft:diamond");
    assert_eq!(stack.count, 3);
    reader.ensure_empty().expect("template has no tail");
    let base = complete(&[3, 0x9e, 0x07, 0, 0], &StackCodecContext::v26_2());
    assert_eq!(base.item.to_string(), "minecraft:diamond");
    assert_eq!(base.count, 3);
}

#[test]
fn fuel_and_removed_component_ids_translate_before_dispatch() {
    let registries = ClientRegistries::default();
    let context = latest(&registries);
    let mut bytes = vec![1, 1, 2, 1, 85, 1];
    bytes.extend_from_slice(&201_i32.to_be_bytes());
    bytes.push(0);
    identifier(&mut bytes, "example:furnace_speed");
    bytes.extend_from_slice(&[120, 120]);
    let stack = complete(&bytes, &context);
    let fuel = stack.components.cooking_fuel.expect("fuel retained");
    assert_eq!(fuel.amount, ItemIntegerValue::Constant(201));
    assert_eq!(fuel.speed, ItemFloatValue::Provider("example:furnace_speed".parse().unwrap()));
    assert!(!stack.components.waxed);
    assert!(complete(&[1, 1, 1, 0, 40, 2, 9], &context).components.attack_animation.is_some());
    assert!(read_item_stack_with(&mut Reader::new(&[1, 1, 1, 0, 40, 3, 9]), &context).is_err());
}

#[test]
fn remaining_component_values_retain_references_and_process_parameters() {
    let mut registries = ClientRegistries::default();
    registry(&mut registries, "minecraft:block_transformer", &["example:unused", "example:strip"]);
    let context = latest(&registries);
    let mut bytes = vec![1, 1, 8, 0, 41, 1, 14, 43, 1, 44, 11, 84, 0];
    identifier(&mut bytes, "example:compost_layers");
    bytes.extend_from_slice(&[86, 1]);
    bytes.extend_from_slice(&19_i32.to_be_bytes());
    bytes.push(1);
    bytes.extend_from_slice(&1.375_f32.to_be_bytes());
    bytes.extend_from_slice(&[87, 3, 4, 7]);
    bytes.extend_from_slice(&0.375_f32.to_be_bytes());
    bytes.push(119);
    for line in ["back", "two", "three", "four"] { text(&mut bytes, line); }
    bytes.extend_from_slice(&[0, 4, 0, 121, 9]);
    let components = complete(&bytes, &context).components;
    let animation = components.interact_animation.unwrap();
    assert_eq!(animation.kind, ItemAnimationKind::Whack);
    assert_eq!(animation.duration, 14);
    assert_eq!(components.block_transformer.unwrap().to_string(), "example:strip");
    assert_eq!(components.villager_food, Some(11));
    assert_eq!(components.compostable, Some(ItemIntegerValue::Provider("example:compost_layers".parse().unwrap())));
    let fuel = components.brewing_fuel.unwrap();
    assert_eq!(fuel.amount, ItemIntegerValue::Constant(19));
    assert_eq!(fuel.speed, ItemFloatValue::ConstantBits(1.375_f32.to_bits()));
    let visibility = components.mob_visibility.unwrap();
    assert_eq!(visibility.entities, RegistrySet::Ids(vec![4, 7]));
    assert_eq!(visibility.factor_bits, 0.375_f32.to_bits());
    let sign = components.sign_text_back.unwrap();
    assert_eq!(sign.messages[0], Text::literal("back"));
    assert!(sign.filtered_messages.is_none());
    assert_eq!(sign.color, "yellow");
    assert!(!sign.glowing);
    assert_eq!(components.cushion_color.as_deref(), Some("cyan"));
}

#[test]
fn dynamic_holders_follow_received_order_and_missing_entries_fail() {
    let mut registries = ClientRegistries::default();
    registry(&mut registries, "minecraft:trim_material", &["example:zinc", "minecraft:iron"]);
    registry(&mut registries, "minecraft:trim_pattern", &["example:zigzag", "minecraft:sentry"]);
    let context = StackCodecContext::new(ProtocolDialect::v26_2(), &registries);
    let mut reader = Reader::new(&[1, 1]);
    let trim = read_armor_trim(&mut reader, &context).expect("ordered holders");
    assert_eq!(trim.material, "example:zinc");
    assert_eq!(trim.pattern, "example:zigzag");
    reader.ensure_empty().unwrap();
    assert!(read_armor_trim(&mut Reader::new(&[3, 1]), &context).is_err());
    assert!(read_armor_trim(&mut Reader::new(&[1, 1]), &StackCodecContext::v26_2()).is_err());
}

#[test]
fn pot_faces_preserve_optional_templates_and_nested_pattern_keys() {
    let mut registries = ClientRegistries::default();
    registry(&mut registries, "minecraft:decorated_pot_pattern", &["example:spiral"]);
    let context = latest(&registries);
    let mut bytes = vec![1];
    var(&mut bytes, 392);
    bytes.extend_from_slice(&[1, 0, 76, 1]);
    var(&mut bytes, 1598);
    bytes.extend_from_slice(&[2, 1, 0, 117, 0, 0, 0, 1]);
    var(&mut bytes, 1142);
    bytes.extend_from_slice(&[1, 0, 0]);
    let stack = complete(&bytes, &context);
    let faces = stack.components.pot_decoration_stacks.expect("four faces");
    assert_eq!(faces[0].as_ref().unwrap().count, 2);
    assert_eq!(faces[0].as_ref().unwrap().item.to_string(), "minecraft:angler_pottery_sherd");
    assert_eq!(faces[0].as_ref().unwrap().components.provides_pottery_pattern.as_ref().unwrap().to_string(), "example:spiral");
    assert!(faces[1].is_none() && faces[2].is_none());
    assert_eq!(faces[3].as_ref().unwrap().item.to_string(), "minecraft:brick");
    assert!(read_item_stack_with(&mut Reader::new(&bytes[..bytes.len() - 1]), &context).is_err());
}

#[test]
fn inline_trim_palette_and_fixed_sign_lines_keep_their_values() {
    let registries = ClientRegistries::default();
    let context = latest(&registries);
    let mut bytes = vec![0];
    identifier(&mut bytes, "example:trims/zinc");
    text(&mut bytes, "Zinc");
    bytes.push(0);
    identifier(&mut bytes, "example:zigzag");
    text(&mut bytes, "Zigzag");
    bytes.push(1);
    let mut reader = Reader::new(&bytes);
    let trim = read_armor_trim(&mut reader, &context).expect("inline palette");
    assert_eq!(trim.material_palette.unwrap().to_string(), "example:trims/zinc");
    assert!(trim.material_asset_overrides.is_empty());
    assert_eq!(trim.pattern, "example:zigzag");
    reader.ensure_empty().unwrap();
    let mut bytes = vec![1, 1, 1, 0, 118];
    for line in ["A", "b", "C", "d"] { text(&mut bytes, line); }
    bytes.push(1);
    for line in ["1", "2", "3", "4"] { text(&mut bytes, line); }
    bytes.extend_from_slice(&[11, 1]);
    let sign = complete(&bytes, &context).components.sign_text_front.unwrap();
    assert_eq!(sign.messages[1], Text::literal("b"));
    assert_eq!(sign.filtered_messages.unwrap()[2], Text::literal("3"));
    assert_eq!(sign.color, "blue");
    assert!(sign.glowing);
}

#[test]
fn advancement_position_is_present_without_display_and_length_exact() {
    let registries = ClientRegistries::default();
    let context = latest(&registries);
    let mut bytes = vec![0, 1];
    identifier(&mut bytes, "example:hidden");
    bytes.extend_from_slice(&[0, 0, 0, 1]);
    bytes.extend_from_slice(&1.75_f32.to_be_bytes());
    bytes.extend_from_slice(&(-2.25_f32).to_be_bytes());
    bytes.extend_from_slice(&[0, 0, 1]);
    let directives = decode_update_advancements(&bytes, &context).expect("position after holder");
    let Directive::Emit(ClientEvent::AdvancementsUpdated { added, .. }) = &directives[0] else { panic!("event"); };
    assert!(added[0].display.is_none());
    assert_eq!(added[0].position, Some([1.75_f32.to_bits(), (-2.25_f32).to_bits()]));
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(decode_update_advancements(&trailing, &context).is_err());
    assert!(decode_update_advancements(&bytes[..bytes.len() - 1], &context).is_err());
}

#[test]
fn teleport_direction_flag_and_instrument_durability_have_distinct_widths() {
    let registries = ClientRegistries::default();
    let context = latest(&registries);
    let mut bytes = vec![1, 3];
    bytes.extend_from_slice(&17.25_f32.to_be_bytes());
    bytes.extend_from_slice(&[1, 0xa7]);
    let mut reader = Reader::new(&bytes);
    let effects = read_consume_effects(&mut reader, &context).unwrap().unwrap();
    assert_eq!(effects, vec![ConsumeEffect::TeleportRandomly { diameter_bits: 17.25_f32.to_bits(), directional_particles: true }]);
    assert_eq!(reader.u8().unwrap(), 0xa7);
    let mut bytes = vec![1, 1, 1, 0, 63, 0, 0];
    identifier(&mut bytes, "example:horn");
    bytes.push(0);
    bytes.extend_from_slice(&1.75_f32.to_be_bytes());
    bytes.extend_from_slice(&13.5_f32.to_be_bytes());
    bytes.push(7);
    text(&mut bytes, "Horn");
    let instrument = complete(&bytes, &context).components.instrument.unwrap();
    let ItemInstrument::Inline { durability_damage, use_duration_bits, range_bits, .. } = instrument else { panic!("inline"); };
    assert_eq!(durability_damage, 7);
    assert_eq!(use_duration_bits, 1.75_f32.to_bits());
    assert_eq!(range_bits, 13.5_f32.to_bits());
}
