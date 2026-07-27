use crate::ids::ResourceKey;

/// A canonical item stack.
///
/// This intentionally carries the stable item key and count only. Modern item
/// component patches are a larger semantic model and should be added here when a
/// consumer needs them rather than leaking any one protocol's component wire
/// representation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ItemStack {
    /// Canonical item key, for example `minecraft:stone`.
    pub item: ResourceKey,
    /// Number of items in the stack.
    pub count: u32,
}
