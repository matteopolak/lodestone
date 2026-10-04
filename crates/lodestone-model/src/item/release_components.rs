use super::{ConsumeEffect, ItemComponents, ItemStack, RegistrySet, ResourceKey, Text};

/// Typed item payloads retained together by consumers without dedicated component slots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemReleaseComponents {
    pub attack_animation: Option<ItemAnimation>,
    pub interact_animation: Option<ItemAnimation>,
    pub block_transformer: Option<ResourceKey>,
    pub provides_pottery_pattern: Option<ResourceKey>,
    pub villager_food: Option<i32>,
    pub compostable: Option<ItemIntegerValue>,
    pub cooking_fuel: Option<ItemFuel>,
    pub brewing_fuel: Option<ItemFuel>,
    pub mob_visibility: Option<ItemMobVisibility>,
    pub sign_text_front: Option<Box<ItemSignText>>,
    pub sign_text_back: Option<Box<ItemSignText>>,
    pub waxed: bool,
    pub cushion_color: Option<String>,
    pub pot_decoration_stacks: Option<[Option<Box<ItemStack>>; 4]>,
    pub instrument: Option<ItemInstrument>,
    pub consume_effects: Vec<ConsumeEffect>,
    pub death_protection_effects: Vec<ConsumeEffect>,
}

impl ItemReleaseComponents {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

impl From<&ItemComponents> for ItemReleaseComponents {
    fn from(components: &ItemComponents) -> Self {
        Self {
            attack_animation: components.attack_animation.clone(),
            interact_animation: components.interact_animation.clone(),
            block_transformer: components.block_transformer.clone(),
            provides_pottery_pattern: components.provides_pottery_pattern.clone(),
            villager_food: components.villager_food,
            compostable: components.compostable.clone(),
            cooking_fuel: components.cooking_fuel.clone(),
            brewing_fuel: components.brewing_fuel.clone(),
            mob_visibility: components.mob_visibility.clone(),
            sign_text_front: components.sign_text_front.clone(),
            sign_text_back: components.sign_text_back.clone(),
            waxed: components.waxed,
            cushion_color: components.cushion_color.clone(),
            pot_decoration_stacks: components.pot_decoration_stacks.clone(),
            instrument: components.instrument.clone(),
            consume_effects: components.consume_effects.clone(),
            death_protection_effects: components.death_protection_effects.clone(),
        }
    }
}

/// An item swing's visual form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemAnimationKind {
    None,
    Whack,
    Stab,
}

/// Item animation form and duration in ticks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemAnimation {
    pub kind: ItemAnimationKind,
    pub duration: u32,
}

/// A constant integer or a named provider evaluated with gameplay context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemIntegerValue {
    Constant(i32),
    Provider(ResourceKey),
}

/// A constant float or a named provider evaluated with gameplay context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemFloatValue {
    ConstantBits(u32),
    Provider(ResourceKey),
}

/// Fuel burn ticks or brewing uses, followed by its processing speed multiplier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemFuel {
    pub amount: ItemIntegerValue,
    pub speed: ItemFloatValue,
}

/// Targeting entities and the visibility multiplier they apply to this item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemMobVisibility {
    pub entities: RegistrySet,
    pub factor_bits: u32,
}

/// Four sign lines, their optional filtered alternatives, and face formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSignText {
    pub messages: [Text; 4],
    pub filtered_messages: Option<[Text; 4]>,
    pub color: String,
    pub glowing: bool,
}

/// A canonical sound reference or a direct sound definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemSound {
    Registry(i32),
    Inline { name: ResourceKey, fixed_range_bits: Option<u32> },
}

/// A synchronized instrument reference or its complete inline definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemInstrument {
    Reference(ResourceKey),
    Inline {
        sound: ItemSound,
        use_duration_bits: u32,
        range_bits: u32,
        durability_damage: u32,
        description: Text,
    },
}
