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

/// The layers an entity draws with no item at all: the drowned's outer layer, an
/// inflated copy of its body on its own sheet (the baby has its own rig and sheet).
#[must_use]
pub fn intrinsic_layers(entity: &str, baby: bool) -> Vec<GearLayer> {
    match (entity, baby) {
        ("drowned", false) => vec![plain("drowned_outer", "entity/zombie/drowned_outer_layer")],
        ("drowned", true) => {
            vec![plain("drowned_baby_outer", "entity/zombie/drowned_outer_layer_baby")]
        }
        _ => Vec::new(),
    }
}

/// The entity state a gear layer's choice depends on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GearState {
    /// The entity is a baby. Only the happy ghast has baby gear rigs.
    pub baby: bool,
    /// The entity has a passenger (the harness goggles drop over the eyes).
    pub ridden: bool,
    /// Another entity is leashed to this one (the happy ghast's ropes show).
    pub leash_holder: bool,
}

/// The layers `item` (an item path such as `iron_horse_armor`) draws on `entity`
/// (an entity type path) in `slot`. Empty for anything that draws nothing: a
/// saddle slot holding a non-saddle, a baby (every gear rig is adult-only), or an
/// animal with no layer for that slot.
#[must_use]
pub fn gear_layers(entity: &str, slot: GearSlot, item: &str, state: GearState) -> Vec<GearLayer> {
    if state.baby && entity != "happy_ghast" {
        return Vec::new();
    }
    match slot {
        GearSlot::Saddle if item == "saddle" => saddle_layer(entity).into_iter().collect(),
        GearSlot::Saddle => Vec::new(),
        GearSlot::Body => body_layers(entity, item, state),
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
        "nautilus" | "zombie_nautilus" => {
            plain("nautilus_saddle", "entity/equipment/nautilus_saddle/saddle")
        }
        _ => return None,
    })
}

fn body_layers(entity: &str, item: &str, state: GearState) -> Vec<GearLayer> {
    match entity {
        "happy_ghast" => happy_ghast_layers(item, state),
        "horse" | "skeleton_horse" | "zombie_horse" => {
            let model = if entity == "horse" { "horse_armor" } else { "undead_horse_armor" };
            horse_armor_layers(model, item)
        }
        "llama" | "trader_llama" => carpet_sheet(item)
            .map(|sheet| plain("llama_decor", sheet))
            .into_iter()
            .collect(),
        "nautilus" | "zombie_nautilus" => nautilus_sheet(item)
            .map(|sheet| plain("nautilus_armor", sheet))
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

/// The crack overlay a wolf armour draws at `remaining` durability (a fraction of the
/// maximum): low under 0.95, medium under 0.69, high under 0.32, none above.
#[must_use]
pub fn wolf_armor_cracks(remaining: f32) -> Option<GearLayer> {
    let sheet = if remaining < 0.32 {
        "entity/wolf/wolf_armor_crackiness_high"
    } else if remaining < 0.69 {
        "entity/wolf/wolf_armor_crackiness_medium"
    } else if remaining < 0.95 {
        "entity/wolf/wolf_armor_crackiness_low"
    } else {
        return None;
    };
    Some(plain("wolf_armor", sheet))
}

fn happy_ghast_layers(item: &str, state: GearState) -> Vec<GearLayer> {
    let Some(sheet) = harness_sheet(item) else {
        return Vec::new();
    };
    let harness = match (state.baby, state.ridden) {
        (false, true) => "happy_ghast_harness",
        (false, false) => "happy_ghast_harness_idle",
        (true, true) => "happy_ghast_baby_harness",
        (true, false) => "happy_ghast_baby_harness_idle",
    };
    let mut layers = vec![plain(harness, sheet)];
    if state.leash_holder {
        let ropes = if state.baby { "happy_ghast_baby_ropes" } else { "happy_ghast_ropes" };
        layers.push(plain(ropes, "entity/ghast/happy_ghast_ropes"));
    }
    layers
}

fn harness_sheet(item: &str) -> Option<&'static str> {
    Some(match item.strip_suffix("_harness")? {
        "white" => "entity/equipment/happy_ghast_body/white_harness",
        "orange" => "entity/equipment/happy_ghast_body/orange_harness",
        "magenta" => "entity/equipment/happy_ghast_body/magenta_harness",
        "light_blue" => "entity/equipment/happy_ghast_body/light_blue_harness",
        "yellow" => "entity/equipment/happy_ghast_body/yellow_harness",
        "lime" => "entity/equipment/happy_ghast_body/lime_harness",
        "pink" => "entity/equipment/happy_ghast_body/pink_harness",
        "gray" => "entity/equipment/happy_ghast_body/gray_harness",
        "light_gray" => "entity/equipment/happy_ghast_body/light_gray_harness",
        "cyan" => "entity/equipment/happy_ghast_body/cyan_harness",
        "purple" => "entity/equipment/happy_ghast_body/purple_harness",
        "blue" => "entity/equipment/happy_ghast_body/blue_harness",
        "brown" => "entity/equipment/happy_ghast_body/brown_harness",
        "green" => "entity/equipment/happy_ghast_body/green_harness",
        "red" => "entity/equipment/happy_ghast_body/red_harness",
        "black" => "entity/equipment/happy_ghast_body/black_harness",
        _ => return None,
    })
}

/// The per-part scale the client applies while a body item is worn: a harnessed
/// happy ghast squeezes its body (tentacles included) to 0.9375.
#[must_use]
pub fn part_scales(model: &str, has_body_item: bool) -> &'static [(&'static str, f32)] {
    match model {
        "happy_ghast" | "happy_ghast_baby" | "happy_ghast_ropes" | "happy_ghast_baby_ropes"
            if has_body_item =>
        {
            &[("body", 0.9375)]
        }
        _ => &[],
    }
}

fn nautilus_sheet(item: &str) -> Option<&'static str> {
    Some(match item {
        "copper_nautilus_armor" => "entity/equipment/nautilus_body/copper",
        "iron_nautilus_armor" => "entity/equipment/nautilus_body/iron",
        "golden_nautilus_armor" => "entity/equipment/nautilus_body/gold",
        "diamond_nautilus_armor" => "entity/equipment/nautilus_body/diamond",
        "netherite_nautilus_armor" => "entity/equipment/nautilus_body/netherite",
        _ => return None,
    })
}

/// The decor a trader llama wears when no carpet replaces it: its own striped
/// blanket, with a baby variant.
#[must_use]
pub fn trader_llama_decor(baby: bool) -> GearLayer {
    if baby {
        plain("llama_baby_decor", "entity/equipment/llama_body/trader_llama_baby")
    } else {
        plain("llama_decor", "entity/equipment/llama_body/trader_llama")
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

/// The parts of `model` the client hides for this entity, by part name: the chest
/// boxes of a donkey, mule or llama without a chest, and the rein parts of a saddle
/// layer while nobody rides. A collapsed part draws nothing (see the shell's
/// `hide_parts`).
#[must_use]
pub fn hidden_parts(model: &str, chested: bool, ridden: bool) -> &'static [&'static str] {
    match model {
        "donkey" | "mule" | "llama" | "trader_llama" if !chested => &["left_chest", "right_chest"],
        "horse_saddle" | "undead_horse_saddle" | "donkey_saddle" | "mule_saddle" if !ridden => {
            &["left_saddle_line", "right_saddle_line"]
        }
        "camel_saddle" if !ridden => &["reins"],
        _ => &[],
    }
}

/// The sheet directories the gear layers draw from, as `entity/equipment/<dir>/`
/// suffixes, for the loader's extra-sheet walk.
pub const GEAR_SHEET_DIRS: [&str; 18] = [
    "equipment/nautilus_saddle",
    "equipment/nautilus_body",
    "strider",
    "zombie",
    "equipment/happy_ghast_body",
    "ghast",
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
        assert_eq!(gear_layers("pig", GearSlot::Saddle, "saddle", GearState::default()).len(), 1);
        assert!(gear_layers("pig", GearSlot::Saddle, "carrot_on_a_stick", GearState::default()).is_empty());
        assert!(gear_layers("cow", GearSlot::Saddle, "saddle", GearState::default()).is_empty());
    }

    #[test]
    fn leather_horse_armour_is_a_dyeable_base_then_a_plain_overlay() {
        let layers = gear_layers("horse", GearSlot::Body, "leather_horse_armor", GearState::default());
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].tint, GearTint::Dyeable { undyed: Some(LEATHER_UNDYED) });
        assert_eq!(layers[1].tint, GearTint::None);
    }

    #[test]
    fn wolf_armour_cracks_step_at_the_documented_fractions() {
        assert!(wolf_armor_cracks(1.0).is_none());
        assert!(wolf_armor_cracks(0.95).is_none());
        assert!(wolf_armor_cracks(0.949).unwrap().sheet.ends_with("_low"));
        assert!(wolf_armor_cracks(0.689).unwrap().sheet.ends_with("_medium"));
        assert!(wolf_armor_cracks(0.319).unwrap().sheet.ends_with("_high"));
    }

    #[test]
    fn the_happy_ghast_harness_follows_rider_and_leash_and_babies_keep_theirs() {
        let state = |baby, ridden, leash_holder| GearState { baby, ridden, leash_holder };
        let layers = |s| gear_layers("happy_ghast", GearSlot::Body, "red_harness", s);
        assert_eq!(layers(state(false, false, false))[0].model, "happy_ghast_harness_idle");
        assert_eq!(layers(state(false, true, false))[0].model, "happy_ghast_harness");
        assert_eq!(layers(state(true, true, false))[0].model, "happy_ghast_baby_harness");
        assert_eq!(layers(state(false, false, false)).len(), 1, "no ropes without a leash");
        assert_eq!(layers(state(true, false, true))[1].model, "happy_ghast_baby_ropes");
        assert!(gear_layers("happy_ghast", GearSlot::Body, "saddle", state(false, false, true)).is_empty());
        assert!(gear_layers("wolf", GearSlot::Body, "wolf_armor", state(true, false, false)).is_empty());
        assert_eq!(part_scales("happy_ghast", true), [("body", 0.9375)]);
        assert!(part_scales("happy_ghast", false).is_empty());
    }

    #[test]
    fn only_the_drowned_has_an_item_free_layer() {
        assert_eq!(intrinsic_layers("drowned", false)[0].model, "drowned_outer");
        assert_eq!(intrinsic_layers("drowned", true)[0].model, "drowned_baby_outer");
        assert!(intrinsic_layers("zombie", false).is_empty());
    }

    #[test]
    fn nautilus_gear_and_the_trader_llama_blanket_resolve() {
        let s = GearState::default();
        assert_eq!(gear_layers("nautilus", GearSlot::Saddle, "saddle", s)[0].model, "nautilus_saddle");
        assert_eq!(
            gear_layers("zombie_nautilus", GearSlot::Body, "golden_nautilus_armor", s)[0].sheet,
            "entity/equipment/nautilus_body/gold"
        );
        assert!(gear_layers("nautilus", GearSlot::Body, "iron_horse_armor", s).is_empty());
        assert_eq!(trader_llama_decor(false).model, "llama_decor");
        assert_eq!(trader_llama_decor(true).model, "llama_baby_decor");
    }

    #[test]
    fn chests_and_reins_hide_until_the_flag_or_a_rider_shows_them() {
        assert_eq!(hidden_parts("donkey", false, false), ["left_chest", "right_chest"]);
        assert!(hidden_parts("donkey", true, false).is_empty());
        assert_eq!(hidden_parts("camel_saddle", true, false), ["reins"]);
        assert!(hidden_parts("camel_saddle", false, true).is_empty());
        assert!(hidden_parts("pig", false, false).is_empty());
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
            for layer in gear_layers(entity, slot, item, GearState::default()) {
                assert!(
                    GEAR_SHEET_DIRS.iter().any(|d| layer.sheet.starts_with(&format!("entity/{d}/"))),
                    "{}",
                    layer.sheet
                );
            }
        }
    }
}
