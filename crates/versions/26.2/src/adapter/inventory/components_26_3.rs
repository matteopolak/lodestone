use super::*;
use lodestone_model::{
    ItemAnimation, ItemAnimationKind, ItemFloatValue, ItemFuel, ItemIntegerValue, ItemMobVisibility,
    ItemSignText,
};

pub(super) fn read_added(
    name: Option<&str>,
    reader: &mut Reader<'_>,
    components: &mut ItemComponents,
    depth: Depth,
    context: &StackCodecContext<'_>,
) -> Result<bool, AdapterError> {
    match name {
        Some("minecraft:attack_animation") => {
            components.attack_animation = Some(read_animation(reader)?);
        }
        Some("minecraft:interact_animation") => {
            components.interact_animation = Some(read_animation(reader)?);
        }
        Some("minecraft:block_transformer") => {
            let holder = reader.var_i32().map_err(dec_err)?;
            let name = context.dynamic("minecraft:block_transformer", holder)?;
            components.block_transformer = Some(parse_key(&name, "block transformer")?);
        }
        Some("minecraft:villager_food") => {
            components.villager_food = Some(reader.var_i32().map_err(dec_err)?);
        }
        Some("minecraft:compostable") => {
            components.compostable = Some(read_integer_value(reader)?);
        }
        Some("minecraft:cooking_fuel") => {
            components.cooking_fuel = Some(read_fuel(reader)?);
        }
        Some("minecraft:brewing_fuel") => {
            components.brewing_fuel = Some(read_fuel(reader)?);
        }
        Some("minecraft:mob_visibility") => {
            let mut entities = read_registry_set(reader)?;
            if let RegistrySet::Ids(ids) = &mut entities {
                for id in ids {
                    *id = context.fixed(FixedRegistryKind::Entity, *id)?;
                }
            }
            components.mob_visibility = Some(ItemMobVisibility {
                entities,
                factor_bits: reader.f32().map_err(dec_err)?.to_bits(),
            });
        }
        Some("minecraft:provides_pottery_pattern") => {
            let holder = reader.var_i32().map_err(dec_err)?;
            let name = context.dynamic("minecraft:decorated_pot_pattern", holder)?;
            components.provides_pottery_pattern = Some(parse_key(&name, "pottery pattern")?);
        }
        Some("minecraft:sign_text_front") => {
            components.sign_text_front = Some(Box::new(read_sign_text(reader)?));
        }
        Some("minecraft:sign_text_back") => {
            components.sign_text_back = Some(Box::new(read_sign_text(reader)?));
        }
        Some("minecraft:waxed") => components.waxed = true,
        Some("minecraft:cushion/color") => {
            components.cushion_color = Some(read_dye(reader)?);
        }
        Some("minecraft:pot_decorations") => {
            let mut stacks: [Option<Box<ItemStack>>; 4] = [None, None, None, None];
            let mut names: [Option<ResourceKey>; 4] = [None, None, None, None];
            for (stack, name) in stacks.iter_mut().zip(&mut names) {
                if reader.bool().map_err(dec_err)? {
                    let decoded = read_item_stack_template(reader, depth, context)?;
                    if decoded.item.namespace() != "minecraft" || decoded.item.path() != "brick" {
                        *name = Some(decoded.item.clone());
                    }
                    *stack = Some(Box::new(decoded));
                }
            }
            let [back, left, right, front] = names;
            components.pot_decorations = Some(PotDecorations { back, left, right, front });
            components.pot_decoration_stacks = Some(stacks);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

pub(super) fn remove(name: Option<&str>, components: &mut ItemComponents) -> bool {
    match name {
        Some("minecraft:attack_animation") => components.attack_animation = None,
        Some("minecraft:interact_animation") => components.interact_animation = None,
        Some("minecraft:block_transformer") => components.block_transformer = None,
        Some("minecraft:villager_food") => components.villager_food = None,
        Some("minecraft:compostable") => components.compostable = None,
        Some("minecraft:cooking_fuel") => components.cooking_fuel = None,
        Some("minecraft:brewing_fuel") => components.brewing_fuel = None,
        Some("minecraft:mob_visibility") => components.mob_visibility = None,
        Some("minecraft:provides_pottery_pattern") => components.provides_pottery_pattern = None,
        Some("minecraft:sign_text_front") => components.sign_text_front = None,
        Some("minecraft:sign_text_back") => components.sign_text_back = None,
        Some("minecraft:waxed") => components.waxed = false,
        Some("minecraft:cushion/color") => components.cushion_color = None,
        Some("minecraft:pot_decorations") => {
            components.pot_decorations = None;
            components.pot_decoration_stacks = None;
        }
        _ => return false,
    }
    true
}

fn read_animation(reader: &mut Reader<'_>) -> Result<ItemAnimation, AdapterError> {
    let raw = reader.var_i32().map_err(dec_err)?;
    let kind = match raw {
        0 => ItemAnimationKind::None,
        1 => ItemAnimationKind::Whack,
        2 => ItemAnimationKind::Stab,
        _ => return Err(AdapterError::Decode(format!("unknown item animation kind {raw}"))),
    };
    let duration = reader.var_i32().map_err(dec_err)?;
    let duration = u32::try_from(duration)
        .map_err(|_| AdapterError::Decode("negative item animation duration".to_owned()))?;
    Ok(ItemAnimation { kind, duration })
}

fn read_integer_value(reader: &mut Reader<'_>) -> Result<ItemIntegerValue, AdapterError> {
    Ok(if reader.bool().map_err(dec_err)? {
        ItemIntegerValue::Constant(reader.i32().map_err(dec_err)?)
    } else {
        let name = reader.string(32767).map_err(dec_err)?;
        ItemIntegerValue::Provider(parse_key(&name, "integer context provider")?)
    })
}

fn read_float_value(reader: &mut Reader<'_>) -> Result<ItemFloatValue, AdapterError> {
    Ok(if reader.bool().map_err(dec_err)? {
        ItemFloatValue::ConstantBits(reader.f32().map_err(dec_err)?.to_bits())
    } else {
        let name = reader.string(32767).map_err(dec_err)?;
        ItemFloatValue::Provider(parse_key(&name, "float context provider")?)
    })
}

fn read_fuel(reader: &mut Reader<'_>) -> Result<ItemFuel, AdapterError> {
    Ok(ItemFuel { amount: read_integer_value(reader)?, speed: read_float_value(reader)? })
}

fn read_dye(reader: &mut Reader<'_>) -> Result<String, AdapterError> {
    let id = reader.var_i32().map_err(dec_err)?;
    usize::try_from(id).ok().and_then(|id| DYE_COLOR_NAMES.get(id))
        .map(|name| (*name).to_owned())
        .ok_or_else(|| AdapterError::Decode(format!("invalid dye color {id}")))
}

fn read_sign_lines(reader: &mut Reader<'_>) -> Result<[Text; 4], AdapterError> {
    Ok([
        Text::from_nbt(&read_network_nbt(reader).map_err(dec_err)?),
        Text::from_nbt(&read_network_nbt(reader).map_err(dec_err)?),
        Text::from_nbt(&read_network_nbt(reader).map_err(dec_err)?),
        Text::from_nbt(&read_network_nbt(reader).map_err(dec_err)?),
    ])
}

fn read_sign_text(reader: &mut Reader<'_>) -> Result<ItemSignText, AdapterError> {
    let messages = read_sign_lines(reader)?;
    let filtered_messages = if reader.bool().map_err(dec_err)? {
        Some(read_sign_lines(reader)?)
    } else {
        None
    };
    Ok(ItemSignText {
        messages, filtered_messages, color: read_dye(reader)?,
        glowing: reader.bool().map_err(dec_err)?,
    })
}
