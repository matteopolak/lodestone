//! Hoes and shovels transforming the clicked block, and the off-hand shield that
//! takes the click instead.
//!
//! A hoe tills grass, path and dirt into farmland, coarse dirt into dirt, and
//! rooted dirt into dirt plus a hanging-roots drop; a shovel flattens grass,
//! dirt, podzol, coarse dirt, mycelium and rooted dirt into a path. Both need air
//! above the block and refuse the bottom face (rooted dirt tills from any face).
//! Both releases share these tables.
//!
//! A tool used from the main hand does nothing, and the click falls through to
//! raising an off-hand shield, when the player is not sneaking. Which tools defer
//! differs by release; [`GameDataVersion::tool_yields_to_offhand_shield`] holds
//! the rule and [`yields_to_offhand_shield`] applies it to a click.
//!
//! [`transform`] and [`yields_to_offhand_shield`] are pure; the use-item-on
//! handler applies the result to the world, the stack and the other players.

use lodestone_data::{GameDataVersion, block::Block, block_states::StateId};
use lodestone_model::BlockFace;

/// A tool that transforms the block it is used on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Hoe,
    Shovel,
}

impl Tool {
    /// The tool an item id names, with or without namespace.
    #[must_use]
    pub fn of(item: &str) -> Option<Self> {
        let path = item.rsplit_once(':').map_or(item, |(_, path)| path);
        if path.ends_with("_hoe") {
            Some(Self::Hoe)
        } else if path.ends_with("_shovel") {
            Some(Self::Shovel)
        } else {
            None
        }
    }
}

/// What a tool does to its clicked block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transform {
    /// The state written over the clicked block.
    pub to: StateId,
    /// The sound played at the block.
    pub sound: &'static str,
    /// Whether one hanging roots pops out of the clicked face.
    pub drops_hanging_roots: bool,
}

/// The transformation `tool` makes on `clicked` struck on `face`, or `None` when
/// it does nothing there. `air_above` is whether the cell over the block is air.
#[must_use]
pub fn transform(tool: Tool, clicked: Block, face: BlockFace, air_above: bool) -> Option<Transform> {
    let open_top = air_above && face != BlockFace::Down;
    match tool {
        Tool::Shovel => {
            let flattens = matches!(
                clicked,
                Block::GrassBlock | Block::Dirt | Block::Podzol | Block::CoarseDirt | Block::Mycelium | Block::RootedDirt
            );
            (flattens && open_top).then(|| Transform {
                to: Block::DirtPath.default_state(),
                sound: "minecraft:item.shovel.flatten",
                drops_hanging_roots: false,
            })
        }
        Tool::Hoe => {
            let (to, drops_hanging_roots) = match clicked {
                Block::GrassBlock | Block::DirtPath | Block::Dirt if open_top => (Block::Farmland, false),
                Block::CoarseDirt if open_top => (Block::Dirt, false),
                Block::RootedDirt => (Block::Dirt, true),
                _ => return None,
            };
            Some(Transform { to: to.default_state(), sound: "minecraft:item.hoe.till", drops_hanging_roots })
        }
    }
}

/// Whether a click with `tool_item` does nothing so the off-hand shield can be
/// raised by the use that follows it.
///
/// Only a main-hand click defers, only to a shield in the off hand, and never
/// while sneaking.
#[must_use]
pub fn yields_to_offhand_shield(
    release: GameDataVersion,
    main_hand: bool,
    tool_item: &str,
    offhand_item: Option<&str>,
    sneaking: bool,
) -> bool {
    main_hand
        && !sneaking
        && offhand_item.is_some_and(|item| item == crate::shield::SHIELD_ITEM)
        && release.tool_yields_to_offhand_shield(tool_item)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_on(tool: Tool, clicked: Block) -> Option<Block> {
        transform(tool, clicked, BlockFace::Up, true).map(|t| t.to.block())
    }

    /// The shovel's six targets and the hoe's three tilling targets come from the
    /// `turns_into_dirt_path` and `turns_into_farmland` block tags.
    #[test]
    fn each_tool_transforms_exactly_the_blocks_its_tag_lists() {
        for block in [Block::GrassBlock, Block::Dirt, Block::Podzol, Block::CoarseDirt, Block::Mycelium, Block::RootedDirt] {
            assert_eq!(tool_on(Tool::Shovel, block), Some(Block::DirtPath), "{block:?}");
        }
        for block in [Block::GrassBlock, Block::DirtPath, Block::Dirt] {
            assert_eq!(tool_on(Tool::Hoe, block), Some(Block::Farmland), "{block:?}");
        }
        assert_eq!(tool_on(Tool::Hoe, Block::CoarseDirt), Some(Block::Dirt));
        assert_eq!(tool_on(Tool::Hoe, Block::RootedDirt), Some(Block::Dirt));
        for untouched in [Block::Stone, Block::Sand, Block::Farmland, Block::DirtPath] {
            assert_eq!(tool_on(Tool::Shovel, untouched), None, "shovel {untouched:?}");
        }
        for untouched in [Block::Stone, Block::Podzol, Block::Mycelium, Block::Farmland] {
            assert_eq!(tool_on(Tool::Hoe, untouched), None, "hoe {untouched:?}");
        }
    }

    #[test]
    fn a_covered_block_or_the_bottom_face_stops_everything_but_tilling_rooted_dirt() {
        for tool in [Tool::Hoe, Tool::Shovel] {
            assert!(transform(tool, Block::Dirt, BlockFace::Up, false).is_none(), "{tool:?} covered");
            assert!(transform(tool, Block::Dirt, BlockFace::Down, true).is_none(), "{tool:?} from below");
            assert!(transform(tool, Block::Dirt, BlockFace::North, true).is_some(), "{tool:?} from the side");
        }
        let rooted = transform(Tool::Hoe, Block::RootedDirt, BlockFace::Down, false).expect("rooted dirt tills anywhere");
        assert!(rooted.drops_hanging_roots);
        assert!(transform(Tool::Shovel, Block::RootedDirt, BlockFace::Down, true).is_none());
    }

    #[test]
    fn only_tilling_rooted_dirt_drops_and_each_tool_has_its_sound() {
        let till = transform(Tool::Hoe, Block::Dirt, BlockFace::Up, true).unwrap();
        assert!(!till.drops_hanging_roots);
        assert_eq!(till.sound, "minecraft:item.hoe.till");
        let path = transform(Tool::Shovel, Block::Dirt, BlockFace::Up, true).unwrap();
        assert_eq!(path.sound, "minecraft:item.shovel.flatten");
    }

    #[test]
    fn tool_of_reads_the_item_suffix() {
        assert_eq!(Tool::of("minecraft:netherite_hoe"), Some(Tool::Hoe));
        assert_eq!(Tool::of("copper_shovel"), Some(Tool::Shovel));
        assert_eq!(Tool::of("minecraft:iron_axe"), None);
        assert_eq!(Tool::of("minecraft:shield"), None);
    }

    /// 26.2 deferred only axes, so a hoe or shovel with an off-hand shield still
    /// transformed the block; 26.3 defers all three. Sneaking, a shield-less off
    /// hand and an off-hand click each keep the transformation in both.
    #[test]
    fn the_shield_takes_a_hoe_or_shovel_click_only_in_26_3() {
        let shield = Some("minecraft:shield");
        for (tool, in_26_2, in_26_3) in [
            ("minecraft:iron_hoe", false, true),
            ("minecraft:iron_shovel", false, true),
            ("minecraft:iron_axe", true, true),
        ] {
            let click = |release, main, offhand, sneaking| yields_to_offhand_shield(release, main, tool, offhand, sneaking);
            assert_eq!(click(GameDataVersion::V26_2, true, shield, false), in_26_2, "26.2 {tool}");
            assert_eq!(click(GameDataVersion::V26_3, true, shield, false), in_26_3, "26.3 {tool}");
            for release in [GameDataVersion::V26_2, GameDataVersion::V26_3] {
                assert!(!click(release, true, shield, true), "sneaking {tool}");
                assert!(!click(release, true, Some("minecraft:shears"), false), "no shield {tool}");
                assert!(!click(release, true, None, false), "empty off hand {tool}");
                assert!(!click(release, false, shield, false), "off-hand click {tool}");
            }
        }
    }
}
