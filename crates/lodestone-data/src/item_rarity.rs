//! Typed built-in item rarity data used by client-facing item labels.
//!
//! ## What it is
//!
//! [`ItemRarity`] is the four-level display classification attached to a
//! built-in item definition. The table is keyed by the generated [`Item`]
//! enum, so callers do not repeatedly compare registry strings while drawing
//! HUD and container surfaces.
//!
//! ## How it works
//!
//! Common is the zero-cost default. The sparse match below records only the
//! non-common definitions and returns the corresponding RGB colour used by
//! text and tooltip title consumers. Enchanted stacks promote common and
//! uncommon to rare, and rare to epic, while epic remains epic.
//!
//! ## How to change it
//!
//! Update the generated item census first when a registry entry changes. Then
//! adjust [`ItemRarity::for_item`] and its table test. Keep this table about
//! built-in definitions only; dynamic item components belong to the protocol
//! adapter rather than being guessed here.
//!
//! ## Configuration
//!
//! There is no runtime configuration. The table is compiled into read-only
//! data and is selected by the canonical item registry id.
//!
//! ## Dependencies
//!
//! This module depends only on the generated [`crate::item::Item`] registry.

use crate::item::Item;

/// The display rarity attached to an item name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemRarity {
    /// The default white label.
    Common,
    /// Yellow label.
    Uncommon,
    /// Aqua label.
    Rare,
    /// Light-purple label.
    Epic,
}

impl ItemRarity {
    /// Returns the built-in rarity for `item` without allocating or parsing.
    #[must_use]
    pub const fn for_item(item: Item) -> Self {
        use Item::*;

        match item {
            HeavyCore | DragonEgg | CommandBlock | Barrier | Light | RepeatingCommandBlock
            | ChainCommandBlock | StructureVoid | Elytra | StructureBlock | Jigsaw | TestBlock
            | TestInstanceBlock | Mace | EnchantedBook | KnowledgeBook | DebugStick
            | CommandBlockMinecart | SilenceArmorTrimSmithingTemplate => Self::Epic,
            Beacon | EnchantedGoldenApple | WitherSkeletonSkull | NetherStar | MusicDiscCreator
            | MusicDiscLavaChicken | MusicDiscOtherside | MusicDiscPigstep | Trident
            | SkullBannerPattern | MojangBannerPattern | FlowBannerPattern | GusterBannerPattern
            | WardArmorTrimSmithingTemplate | EyeArmorTrimSmithingTemplate
            | VexArmorTrimSmithingTemplate | SpireArmorTrimSmithingTemplate => Self::Rare,
            SnifferEgg | Conduit | ChainmailHelmet | ChainmailChestplate | ChainmailLeggings
            | ChainmailBoots | RecoveryCompass | ExperienceBottle | SkeletonSkull | PlayerHead
            | ZombieHead | CreeperHead | PiglinHead | DragonBreath | TotemOfUndying
            | MusicDisc13 | MusicDiscCat | MusicDiscBlocks | MusicDiscBounce | MusicDiscChirp
            | MusicDiscCreatorMusicBox | MusicDiscFar | MusicDiscMall | MusicDiscMellohi
            | MusicDiscStal | MusicDiscStrad | MusicDiscWard | MusicDisc11 | MusicDiscWait
            | MusicDiscRelic | MusicDisc5 | MusicDiscPrecipice | MusicDiscTears | DiscFragment5
            | NautilusShell | HeartOfTheSea | CreeperBannerPattern | PiglinBannerPattern
            | GoatHorn | EchoShard | NetheriteUpgradeSmithingTemplate
            | SentryArmorTrimSmithingTemplate | DuneArmorTrimSmithingTemplate
            | CoastArmorTrimSmithingTemplate | WildArmorTrimSmithingTemplate
            | TideArmorTrimSmithingTemplate | SnoutArmorTrimSmithingTemplate
            | RibArmorTrimSmithingTemplate | WayfinderArmorTrimSmithingTemplate
            | ShaperArmorTrimSmithingTemplate | RaiserArmorTrimSmithingTemplate
            | HostArmorTrimSmithingTemplate | FlowArmorTrimSmithingTemplate
            | BoltArmorTrimSmithingTemplate | AnglerPotterySherd | ArcherPotterySherd
            | ArmsUpPotterySherd | BladePotterySherd | BrewerPotterySherd | BurnPotterySherd
            | DangerPotterySherd | ExplorerPotterySherd | FlowPotterySherd | FriendPotterySherd
            | GusterPotterySherd | HeartPotterySherd | HeartbreakPotterySherd | HowlPotterySherd
            | MinerPotterySherd | MournerPotterySherd | PlentyPotterySherd | PrizePotterySherd
            | ScrapePotterySherd | SheafPotterySherd | ShelterPotterySherd | SkullPotterySherd
            | SnortPotterySherd | OminousBottle => Self::Uncommon,
            _ => Self::Common,
        }
    }

    /// Returns the gamma-space RGBA colour used for a label or tooltip title.
    #[must_use]
    pub const fn rgba(self) -> [f32; 4] {
        match self {
            Self::Common => [1.0, 1.0, 1.0, 1.0],
            Self::Uncommon => [1.0, 1.0, 85.0 / 255.0, 1.0],
            Self::Rare => [85.0 / 255.0, 1.0, 1.0, 1.0],
            Self::Epic => [1.0, 85.0 / 255.0, 1.0, 1.0],
        }
    }

    /// Returns the effective rarity after the item has a glint.
    #[must_use]
    pub const fn with_enchantment(self, enchanted: bool) -> Self {
        if !enchanted {
            return self;
        }
        match self {
            Self::Common | Self::Uncommon => Self::Rare,
            Self::Rare | Self::Epic => Self::Epic,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Item, ItemRarity};

    #[test]
    fn built_in_rarity_table_has_positive_and_common_controls() {
        assert_eq!(ItemRarity::for_item(Item::Stone), ItemRarity::Common);
        assert_eq!(ItemRarity::for_item(Item::MusicDiscPigstep), ItemRarity::Rare);
        assert_eq!(ItemRarity::for_item(Item::Mace), ItemRarity::Epic);
        assert_eq!(ItemRarity::for_item(Item::AnglerPotterySherd), ItemRarity::Uncommon);
    }

    #[test]
    fn enchantment_promotion_does_not_change_epic_or_unenchanted_items() {
        assert_eq!(
            ItemRarity::for_item(Item::Stone).with_enchantment(true),
            ItemRarity::Rare
        );
        assert_eq!(
            ItemRarity::for_item(Item::Mace).with_enchantment(true),
            ItemRarity::Epic
        );
        assert_eq!(
            ItemRarity::for_item(Item::Mace).with_enchantment(false),
            ItemRarity::Epic
        );
    }
}
