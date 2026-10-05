//! Whether a block can stand at a position, for the blocks decoration places.
//!
//! Every block is classified once into a [`Kind`]; blocks not classified are `Unported` and
//! panic when asked, so a feature that places one never silently matches or mismatches the
//! reference. [`Kinds::supported`] lets callers detect that ahead of time.

use crate::blocks::{BlockId, BlockTable, FluidKind, State};
use crate::level::Level;
use crate::tags::BlockTags;

/// How a block decides whether it can stand where it is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// No placement rule.
    Always,
    /// The block below must be in the vegetation support tag (saplings, grass, flowers, bushes).
    Vegetation,
    /// A two-tall plant: the lower half needs vegetation support; the upper half needs the
    /// lower half beneath it.
    DoubleVegetation,
    /// The block below must be in the dry-vegetation support tag.
    DryVegetation,
    /// The block below must be in the azalea support tag.
    Azalea,
    /// Mushrooms: a tagged light-exempt block below, or a solid block below and low light.
    Mushroom,
    SugarCane,
    Cactus,
    CactusFlower,
    LilyPad,
    Seagrass,
    TallSeagrass,
    Kelp,
    KelpPlant,
    /// A full-sturdy up face below.
    SturdyBelow,
    Unported,
}

#[derive(Debug)]
pub struct Kinds {
    by_block: Vec<Kind>,
}

fn classify(name: &str) -> Kind {
    let n = name.strip_prefix("minecraft:").unwrap_or(name);
    match n {
        "short_grass" | "fern" | "bush" | "red_shrub" | "firefly_bush" | "sweet_berry_bush" | "pink_petals" | "wildflowers" | "dandelion"
        | "golden_dandelion" | "poppy" | "blue_orchid" | "allium" | "azure_bluet" | "red_tulip" | "orange_tulip" | "white_tulip"
        | "pink_tulip" | "oxeye_daisy" | "cornflower" | "lily_of_the_valley" | "torchflower" | "closed_eyeblossom" | "open_eyeblossom" => {
            Kind::Vegetation
        }
        n if n.ends_with("_sapling") => Kind::Vegetation,
        "tall_grass" | "large_fern" | "sunflower" | "lilac" | "rose_bush" | "peony" => Kind::DoubleVegetation,
        "dead_bush" | "short_dry_grass" | "tall_dry_grass" => Kind::DryVegetation,
        "azalea" | "flowering_azalea" => Kind::Azalea,
        "brown_mushroom" | "red_mushroom" => Kind::Mushroom,
        "sugar_cane" => Kind::SugarCane,
        "cactus" => Kind::Cactus,
        "cactus_flower" => Kind::CactusFlower,
        "lily_pad" => Kind::LilyPad,
        "seagrass" => Kind::Seagrass,
        "tall_seagrass" => Kind::TallSeagrass,
        "kelp" => Kind::Kelp,
        "kelp_plant" => Kind::KelpPlant,
        "leaf_litter" => Kind::SturdyBelow,
        "pumpkin" | "melon" => Kind::Always,
        _ => Kind::Unported,
    }
}

impl Kinds {
    #[must_use]
    pub fn build(table: &BlockTable) -> Self {
        Self { by_block: table.blocks.iter().map(|b| classify(&b.name)).collect() }
    }

    #[must_use]
    pub fn kind(&self, b: BlockId) -> Kind {
        self.by_block[b as usize]
    }

    /// Whether `can_survive` can answer for this block.
    #[must_use]
    pub fn supported(&self, b: BlockId) -> bool {
        self.kind(b) != Kind::Unported
    }
}

fn in_tag(tags: &BlockTags, name: &str, b: BlockId) -> bool {
    tags.get(name).unwrap_or_else(|| panic!("unknown block tag {name}")).contains(b)
}

/// Whether `state` could stand at the position (reads the surrounding blocks).
///
/// Light-gated blocks read the brightness of a column the light engine has no data for
/// ([`crate::climate::RAW_BRIGHTNESS`]).
///
/// # Panics
/// For a block whose rule is not yet ported; callers screen with [`Kinds::supported`].
#[must_use]
pub fn can_survive(level: &Level<'_>, state: State, x: i32, y: i32, z: i32) -> bool {
    let env = level.env;
    let blocks = &env.blocks;
    let block = blocks.block_of(state);
    let kind = env.survive.kind(block);
    let below = level.get(x, y - 1, z);
    let below_block = blocks.block_of(below);
    let below_in = |tag: &str| in_tag(&env.tags, tag, below_block);
    match kind {
        Kind::Always => true,
        Kind::Vegetation => below_in("supports_vegetation"),
        Kind::DoubleVegetation => {
            if blocks.get(state, "half") == Some("upper") {
                below_block == block && blocks.get(below, "half") == Some("lower")
            } else {
                below_in("supports_vegetation")
            }
        }
        Kind::DryVegetation => below_in("supports_dry_vegetation"),
        Kind::Azalea => below_in("supports_azalea"),
        Kind::Mushroom => below_in("overrides_mushroom_light_requirement") || (crate::climate::RAW_BRIGHTNESS < 13 && blocks.solid_render(below)),
        Kind::SugarCane => {
            if below_block == block {
                return true;
            }
            if !below_in("supports_sugar_cane") {
                return false;
            }
            [(-1, 0), (1, 0), (0, -1), (0, 1)].iter().any(|&(dx, dz)| {
                let side = level.get(x + dx, y - 1, z + dz);
                blocks.fluid(side) == FluidKind::Water || in_tag(&env.tags, "supports_sugar_cane_adjacently", blocks.block_of(side))
            })
        }
        Kind::Cactus => {
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let side = level.get(x + dx, y, z + dz);
                if blocks.solid(side) || blocks.fluid(side) == FluidKind::Lava {
                    return false;
                }
            }
            (below_block == block || below_in("supports_cactus")) && !blocks.liquid(level.get(x, y + 1, z))
        }
        Kind::CactusFlower => below_in("support_override_cactus_flower") || blocks.face_sturdy(below, crate::blocks::Dir::Up, crate::blocks::Support::Center),
        Kind::LilyPad => {
            let here = level.get(x, y - 1, z);
            let above_fluid = level.get(x, y, z);
            let source_water = blocks.fluid(here) == FluidKind::Water && blocks.fluid_is_source(here);
            (source_water || in_tag(&env.tags, "supports_lily_pad", blocks.block_of(here))) && blocks.fluid(above_fluid) == FluidKind::Empty
        }
        Kind::Seagrass => blocks.face_sturdy(below, crate::blocks::Dir::Up, crate::blocks::Support::Full) && !below_in("cannot_support_seagrass"),
        Kind::TallSeagrass => {
            if blocks.get(state, "half") == Some("upper") {
                return below_block == block && blocks.get(below, "half") == Some("lower");
            }
            let here = level.get(x, y, z);
            blocks.face_sturdy(below, crate::blocks::Dir::Up, crate::blocks::Support::Full)
                && !below_in("cannot_support_seagrass")
                && blocks.fluid(here) == FluidKind::Water
                && blocks.fluid_amount(here) == 8
        }
        Kind::Kelp | Kind::KelpPlant => {
            let head = blocks.block_by_name("kelp").expect("kelp");
            let body = blocks.block_by_name("kelp_plant").expect("kelp_plant");
            if in_tag(&env.tags, "cannot_support_kelp", below_block) {
                return false;
            }
            below_block == head || below_block == body || blocks.face_sturdy(below, crate::blocks::Dir::Up, crate::blocks::Support::Full)
        }
        Kind::SturdyBelow => blocks.face_sturdy(below, crate::blocks::Dir::Up, crate::blocks::Support::Full),
        Kind::Unported => panic!("canSurvive is not ported for {}", blocks.block_name(block)),
    }
}
