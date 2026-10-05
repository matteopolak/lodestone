//! Which worn-gear layers an animal draws: the saddle and body-slot items an
//! entity carries, resolved to a layer rig from the corpus plus an equipment sheet.
//!
//! Each layer re-poses the animal with another rig (same part names, inflated or
//! extended) and draws it with a sheet from `textures/entity/equipment/`. The rigs
//! live in `lodestone_assets::entity_models`; this module is the item-to-asset
//! table the real client keeps in its equipment definitions.

/// The two animal slots a gear layer is drawn for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GearSlot {
    /// The saddle slot.
    Saddle,
    /// The body slot: horse armour, llama carpet, wolf armour.
    Body,
}

/// How a gear layer is tinted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GearTint {
    /// Drawn white.
    None,
    /// Drawn with the stack's dye colour when it has one, otherwise `undyed`
    /// (`None`: not drawn at all while undyed).
    Dyeable {
        /// The colour used while the stack has no dye; `None` draws nothing.
        undyed: Option<[u8; 3]>,
    },
}

/// One gear layer: the rig, the sheet it is textured with and its tint rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GearLayer {
    /// Corpus name of the layer rig.
    pub model: &'static str,
    /// Equipment sheet reference, e.g. `entity/equipment/horse_body/iron`.
    pub sheet: &'static str,
    /// The tint rule.
    pub tint: GearTint,
}

const fn plain(model: &'static str, sheet: &'static str) -> GearLayer {
    GearLayer { model, sheet, tint: GearTint::None }
}

const LEATHER_UNDYED: [u8; 3] = [0xA0, 0x65, 0x40];

/// The layers `item` (an item path such as `iron_horse_armor`) draws on `entity`
/// (an entity type path) in `slot`. Empty for anything that draws nothing: a
/// saddle slot holding a non-saddle, a baby (every gear rig is adult-only), or an
/// animal with no layer for that slot.
#[must_use]
pub fn gear_layers(entity: &str, slot: GearSlot, item: &str) -> Vec<GearLayer> {
    match slot {
        GearSlot::Saddle if item == "saddle" => saddle_layer(entity).into_iter().collect(),
        GearSlot::Saddle => Vec::new(),
        GearSlot::Body => body_layers(entity, item),
    }
}

fn saddle_layer(entity: &str) -> Option<GearLayer> {
    Some(match entity {
        "pig" => plain("pig_saddle", "entity/equipment/pig_saddle/saddle"),
        "horse" => plain("horse_saddle", "entity/equipment/horse_saddle/saddle"),
        "donkey" => plain("donkey_saddle", "entity/equipment/donkey_saddle/saddle"),
        "mule" => plain("mule_saddle", "entity/equipment/mule_saddle/saddle"),
        "skeleton_horse" => plain("undead_horse_saddle", "entity/equipment/skeleton_horse_saddle/saddle"),
        "zombie_horse" => plain("undead_horse_saddle", "entity/equipment/zombie_horse_saddle/saddle"),
        "strider" => plain("strider", "entity/equipment/strider_saddle/saddle"),
        "camel" => plain("camel_saddle", "entity/equipment/camel_saddle/saddle"),
        "camel_husk" => plain("camel_saddle", "entity/equipment/camel_husk_saddle/saddle"),
        _ => return None,
    })
}

fn body_layers(entity: &str, item: &str) -> Vec<GearLayer> {
    match entity {
        "horse" | "skeleton_horse" | "zombie_horse" => {
            let model = if entity == "horse" { "horse_armor" } else { "undead_horse_armor" };
            horse_armor_layers(model, item)
        }
        "llama" | "trader_llama" => carpet_sheet(item)
            .map(|sheet| plain("llama_decor", sheet))
            .into_iter()
            .collect(),
        "wolf" if item == "wolf_armor" => vec![
            plain("wolf_armor", "entity/equipment/wolf_body/armadillo_scute"),
            GearLayer {
                model: "wolf_armor",
                sheet: "entity/equipment/wolf_body/armadillo_scute_overlay",
                tint: GearTint::Dyeable { undyed: None },
            },
        ],
        _ => Vec::new(),
    }
}

fn horse_armor_layers(model: &'static str, item: &str) -> Vec<GearLayer> {
    let sheet = match item {
        "leather_horse_armor" => {
            return vec![
                GearLayer {
                    model,
                    sheet: "entity/equipment/horse_body/leather",
                    tint: GearTint::Dyeable { undyed: Some(LEATHER_UNDYED) },
                },
                plain(model, "entity/equipment/horse_body/leather_overlay"),
            ];
        }
        "copper_horse_armor" => "entity/equipment/horse_body/copper",
        "iron_horse_armor" => "entity/equipment/horse_body/iron",
        "golden_horse_armor" => "entity/equipment/horse_body/gold",
        "diamond_horse_armor" => "entity/equipment/horse_body/diamond",
        "netherite_horse_armor" => "entity/equipment/horse_body/netherite",
        _ => return Vec::new(),
    };
    vec![plain(model, sheet)]
}

fn carpet_sheet(item: &str) -> Option<&'static str> {
    Some(match item.strip_suffix("_carpet")? {
        "white" => "entity/equipment/llama_body/white",
        "orange" => "entity/equipment/llama_body/orange",
        "magenta" => "entity/equipment/llama_body/magenta",
        "light_blue" => "entity/equipment/llama_body/light_blue",
        "yellow" => "entity/equipment/llama_body/yellow",
        "lime" => "entity/equipment/llama_body/lime",
        "pink" => "entity/equipment/llama_body/pink",
        "gray" => "entity/equipment/llama_body/gray",
        "light_gray" => "entity/equipment/llama_body/light_gray",
        "cyan" => "entity/equipment/llama_body/cyan",
        "purple" => "entity/equipment/llama_body/purple",
        "blue" => "entity/equipment/llama_body/blue",
        "brown" => "entity/equipment/llama_body/brown",
        "green" => "entity/equipment/llama_body/green",
        "red" => "entity/equipment/llama_body/red",
        "black" => "entity/equipment/llama_body/black",
        _ => return None,
    })
}

/// The sheet directories the gear layers draw from, as `entity/equipment/<dir>/`
/// suffixes, for the loader's extra-sheet walk.
pub const GEAR_SHEET_DIRS: [&str; 12] = [
    "equipment/pig_saddle",
    "equipment/horse_saddle",
    "equipment/skeleton_horse_saddle",
    "equipment/zombie_horse_saddle",
    "equipment/donkey_saddle",
    "equipment/mule_saddle",
    "equipment/camel_saddle",
    "equipment/camel_husk_saddle",
    "equipment/strider_saddle",
    "equipment/horse_body",
    "equipment/llama_body",
    "equipment/wolf_body",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saddle_resolves_only_for_a_saddle_item() {
        assert_eq!(gear_layers("pig", GearSlot::Saddle, "saddle").len(), 1);
        assert!(gear_layers("pig", GearSlot::Saddle, "carrot_on_a_stick").is_empty());
        assert!(gear_layers("cow", GearSlot::Saddle, "saddle").is_empty());
    }

    #[test]
    fn leather_horse_armour_is_a_dyeable_base_then_a_plain_overlay() {
        let layers = gear_layers("horse", GearSlot::Body, "leather_horse_armor");
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].tint, GearTint::Dyeable { undyed: Some(LEATHER_UNDYED) });
        assert_eq!(layers[1].tint, GearTint::None);
    }

    #[test]
    fn every_sheet_lives_in_a_listed_directory() {
        let items = [
            ("horse", GearSlot::Body, "golden_horse_armor"),
            ("llama", GearSlot::Body, "light_blue_carpet"),
            ("wolf", GearSlot::Body, "wolf_armor"),
            ("camel_husk", GearSlot::Saddle, "saddle"),
        ];
        for (entity, slot, item) in items {
            for layer in gear_layers(entity, slot, item) {
                assert!(
                    GEAR_SHEET_DIRS.iter().any(|d| layer.sheet.starts_with(&format!("entity/{d}/"))),
                    "{}",
                    layer.sheet
                );
            }
        }
    }
}
