use crate::ids::ResourceKey;
use crate::text::Text;

/// A canonical item stack.
///
/// Carries the stable item key, count, and the subset of the item's data
/// components this client models ([`ItemComponents`]). Components a build does
/// not understand are not represented field-by-field; their presence is instead
/// summarised by [`ItemComponents::has_unmodeled`], so an item that carries an
/// unrecognised component still yields a usable stack rather than tearing down
/// the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemStack {
    /// Canonical item key, for example `minecraft:stone`.
    pub item: ResourceKey,
    /// Number of items in the stack.
    pub count: u32,
    /// The modeled subset of the stack's data-component patch.
    pub components: ItemComponents,
}

impl ItemStack {
    /// Creates a stack of `item` × `count` with no components.
    #[must_use]
    pub fn new(item: ResourceKey, count: u32) -> Self {
        Self {
            item,
            count,
            components: ItemComponents::default(),
        }
    }
}

/// The subset of an item stack's data-component patch this client models.
///
/// Modern item stacks carry an open-ended, versioned set of data components.
/// This models only the fields that drive gameplay surfaces — the HUD, tooltips,
/// and tool behaviour — and deliberately does not attempt to represent every
/// component. [`has_unmodeled`](Self::has_unmodeled) records whether the wire
/// carried at least one component this build could not decode, so a consumer can
/// distinguish a genuinely bare stack from one that was only partially understood.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemComponents {
    /// A player- or server-assigned display name overriding the item's default.
    pub custom_name: Option<Text>,
    /// Accumulated durability damage; the item's remaining durability is its
    /// max damage minus this value. `None` when the stack carries no damage
    /// component (either undamaged or not damageable).
    pub damage: Option<u32>,
    /// Enchantments applied to the stack, in wire order.
    pub enchantments: Vec<ItemEnchantment>,
    /// True when the stack's patch carried at least one component this build
    /// does not model, so decoding stopped early and the modeled fields above
    /// may be incomplete. The modeled fields that were decoded remain valid.
    pub has_unmodeled: bool,
}

/// A single enchantment entry on an item stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemEnchantment {
    /// Network `minecraft:enchantment` registry id. The enchantment registry is
    /// data-driven and synced at configuration time, so this id is scoped to the
    /// current session rather than stable across versions.
    pub id: i32,
    /// Enchantment level (for example, 4 for Efficiency IV).
    pub level: u32,
}
