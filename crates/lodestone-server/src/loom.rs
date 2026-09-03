//! The loom (station half): a real, server-computed banner
//! pattern list and result — `LoomMenu`'s own `getSelectablePatterns` /
//! `setupResultSlot`.
//!
//! # What it is
//!
//! Vanilla's loom offers one of two pattern lists depending on the pattern
//! slot: an empty pattern slot offers the 32 patterns tagged
//! `#minecraft:no_item_required` (the base grid drawn in the UI); a pattern
//! *item* in that slot offers exactly the one pattern its own
//! `minecraft:provides_banner_patterns` component names — and vanilla
//! auto-selects that single option (`LoomMenu.slotsChanged`'s
//! `selectablePatterns.size() == 1` branch) without needing a button click
//! at all, which is why applying a pattern item to a banner+dye pair works
//! today with no `ContainerButtonClick` support built anywhere in this
//! crate: [`result`] reproduces that auto-select branch directly.
//!
//! [`PATTERN_ITEMS`] and [`BASE_PATTERNS`] are transcribed from the real
//! datapack tag files, not a wiki list —
//! `.cache/mc/26.2/src/data/minecraft/tags/banner_pattern/pattern_item/*.json`
//! (ten files, each named after the *pattern* it grants and containing that
//! one pattern id — `bordure_indented.json`'s `values` is
//! `["minecraft:curly_border"]`, which is why [`PATTERN_ITEMS`]'s pair is
//! `("bordure_indented", "curly_border")` rather than the identity mapping
//! nine of the ten happen to be) and
//! `tags/banner_pattern/no_item_required.json`'s `values` list verbatim, in
//! file order — a disclosed transcription of the tag file's listed order,
//! not a verified live-registry iteration order (the same caveat a prior
//! pass on this issue recorded; nobody has built a `Registries.BANNER_PATTERN`
//! JVM oracle dump to pin the exact button order yet).
//!
//! # How it works
//!
//! Banner/dye detection needs no new data: a banner item is `minecraft:*_banner`
//! and a dye item is `minecraft:*_dye` — both already-established path-suffix
//! simplifications this crate already uses elsewhere (`lodestone_game::item::is_bundle`'s
//! own convention). The dye's colour is the suffix itself (`"red_dye"` →
//! `"red"`), matching [`lodestone_model::BannerPatternLayer::color`]'s own
//! convention.
//!
//! # How to change it
//!
//! A future pattern item needs one row in [`PATTERN_ITEMS`], keyed by the
//! *pattern* id its tag file grants — re-derive from the jar's own tag JSON,
//! not by guessing the identity mapping most rows happen to follow.
//!
//! # Dependencies
//!
//! [`lodestone_model::BannerPatternLayer`] for the applied-layer shape;
//! nothing else new.

use lodestone_model::{BannerPatternLayer, ItemStack};

/// A banner's own real cap — `BannerBlockEntity`'s pattern list is capped at
/// six layers (`hasMaxPatterns` in `LoomMenu.slotsChanged`).
const MAX_BANNER_PATTERNS: usize = 6;

/// `tags/banner_pattern/pattern_item/*.json`, one row per file — `(pattern
/// id, item suffix)`. See this module's own doc for why the pair is not an
/// identity mapping for `bordure_indented`/`field_masoned`.
const PATTERN_ITEMS: &[(&str, &str)] = &[
    ("bordure_indented_banner_pattern", "curly_border"),
    ("creeper_banner_pattern", "creeper"),
    ("field_masoned_banner_pattern", "bricks"),
    ("flow_banner_pattern", "flow"),
    ("flower_banner_pattern", "flower"),
    ("globe_banner_pattern", "globe"),
    ("guster_banner_pattern", "guster"),
    ("mojang_banner_pattern", "mojang"),
    ("piglin_banner_pattern", "piglin"),
    ("skull_banner_pattern", "skull"),
];

/// `tags/banner_pattern/no_item_required.json`'s `values`, verbatim, in file
/// order.
const BASE_PATTERNS: &[&str] = &[
    "square_bottom_left",
    "square_bottom_right",
    "square_top_left",
    "square_top_right",
    "stripe_bottom",
    "stripe_top",
    "stripe_left",
    "stripe_right",
    "stripe_center",
    "stripe_middle",
    "stripe_downright",
    "stripe_downleft",
    "small_stripes",
    "cross",
    "straight_cross",
    "triangle_bottom",
    "triangle_top",
    "triangles_bottom",
    "triangles_top",
    "diagonal_left",
    "diagonal_up_right",
    "diagonal_up_left",
    "diagonal_right",
    "circle",
    "rhombus",
    "half_vertical",
    "half_horizontal",
    "half_vertical_right",
    "half_horizontal_bottom",
    "border",
    "gradient",
    "gradient_up",
];

/// `BannerItem` — `LoomMenu`'s `bannerSlot.mayPlace`.
#[must_use]
pub fn is_banner_item(item: &str) -> bool {
    item.strip_prefix("minecraft:").is_some_and(|rest| rest.ends_with("_banner"))
}

/// `LoomMenu.isDyeItem` — `ItemTags.LOOM_DYES` plus a `DYE` component, which
/// in this crate's data is the same `*_dye` suffix convention every dye item
/// already follows.
#[must_use]
pub fn is_dye_item(item: &str) -> bool {
    item.strip_prefix("minecraft:").is_some_and(|rest| rest.ends_with("_dye"))
}

/// `LoomMenu.isPatternItem` — a [`PATTERN_ITEMS`] member.
#[must_use]
pub fn is_pattern_item(item: &str) -> bool {
    let bare = item.strip_prefix("minecraft:").unwrap_or(item);
    PATTERN_ITEMS.iter().any(|(name, _)| *name == bare)
}

/// The dye's colour name — `"minecraft:red_dye"` → `"red"`. Owned, not
/// borrowed: every real caller only ever has the source string as a
/// temporary (`item.to_string()`), so borrowing from it would not outlive
/// the call.
fn dye_color(item: &str) -> Option<String> {
    Some(item.strip_prefix("minecraft:")?.strip_suffix("_dye")?.to_owned())
}

/// `LoomMenu.getSelectablePatterns`: the pattern-item slot's single granted
/// pattern, or the 32-pattern base grid when the slot is empty, or nothing
/// for an item this crate does not recognise as a pattern item (vanilla's
/// own `mayPlace` would already have refused it into the slot). Returns an
/// owned `Vec` rather than a `&'static [&'static str]` slice — the one-item
/// case has no `&'static` home to borrow a single-element slice from.
#[must_use]
pub(crate) fn selectable_patterns(pattern_item: Option<&ItemStack>) -> Vec<&'static str> {
    match pattern_item {
        None => BASE_PATTERNS.to_vec(),
        Some(stack) => {
            let bare = stack.item.to_string();
            let bare = bare.strip_prefix("minecraft:").unwrap_or(&bare).to_owned();
            PATTERN_ITEMS
                .iter()
                .find(|(name, _)| *name == bare)
                .map_or_else(Vec::new, |(_, pattern)| vec![*pattern])
        }
    }
}

/// How many offers [`selectable_patterns`] would return for `pattern_item` —
/// `crate::server`'s `ContainerButtonClick` consumer's own validity check,
/// without needing the whole list.
#[must_use]
pub fn selectable_pattern_count(pattern_item: Option<&ItemStack>) -> usize {
    selectable_patterns(pattern_item).len()
}

/// The loom's result slot: `banner` with one new pattern layer applied, or
/// `None` if the inputs cannot produce one — `LoomMenu.setupResultSlot`
/// folded with `slotsChanged`'s own auto-select branch.
///
/// `selected` is consulted only when [`selectable_patterns`] offers more
/// than one option (the base 32-pattern grid); a specific pattern *item*
/// always yields exactly one option and is applied regardless of `selected`,
/// matching vanilla's own auto-select rather than requiring a
/// `ContainerButtonClick` for the common case.
#[must_use]
pub fn result(
    banner: Option<&ItemStack>,
    dye: Option<&ItemStack>,
    pattern_item: Option<&ItemStack>,
    selected: Option<i32>,
) -> Option<ItemStack> {
    let banner = banner?;
    let dye = dye?;
    if !is_banner_item(&banner.item.to_string()) || !is_dye_item(&dye.item.to_string()) {
        return None;
    }
    let color = dye_color(&dye.item.to_string())?;
    if banner.components.banner_patterns.len() >= MAX_BANNER_PATTERNS {
        return None;
    }
    let patterns = selectable_patterns(pattern_item);
    let pattern = if patterns.len() == 1 {
        patterns[0]
    } else {
        let index = usize::try_from(selected?).ok()?;
        *patterns.get(index)?
    };
    let mut result = banner.clone();
    result.count = 1;
    result.components.banner_patterns.push(BannerPatternLayer {
        pattern_asset_id: pattern.to_string(),
        color: color.to_string(),
    });
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str) -> ItemStack {
        ItemStack::new(item.parse().expect("valid key"), 1)
    }

    /// A specific pattern *item* auto-selects its own one pattern —
    /// `LoomMenu.slotsChanged`'s `selectablePatterns.size() == 1` branch —
    /// with `selected` left `None`, proving the common case needs no
    /// `ContainerButtonClick` at all.
    #[test]
    fn a_pattern_item_auto_selects_its_single_pattern_with_no_button_click() {
        let banner = stack("minecraft:white_banner");
        let dye = stack("minecraft:red_dye");
        let pattern_item = stack("minecraft:creeper_banner_pattern");

        let result = result(Some(&banner), Some(&dye), Some(&pattern_item), None).expect("must apply");
        assert_eq!(result.item.to_string(), "minecraft:white_banner");
        assert_eq!(result.count, 1);
        assert_eq!(
            result.components.banner_patterns,
            vec![BannerPatternLayer { pattern_asset_id: "creeper".to_string(), color: "red".to_string() }],
            "the tag file maps the item to pattern id `creeper`, not the item's own bare name"
        );
    }

    /// The non-identity mapping is the discriminating case: `bordure_indented_banner_pattern`
    /// grants pattern `curly_border`, not `bordure_indented` — the exact
    /// transposition this module's own doc warns a naive reader would guess.
    #[test]
    fn bordure_indented_item_grants_the_curly_border_pattern_not_its_own_name() {
        let banner = stack("minecraft:blue_banner");
        let dye = stack("minecraft:white_dye");
        let pattern_item = stack("minecraft:bordure_indented_banner_pattern");

        let result = result(Some(&banner), Some(&dye), Some(&pattern_item), None).expect("must apply");
        assert_eq!(result.components.banner_patterns[0].pattern_asset_id, "curly_border");
    }

    /// With no pattern item, the base 32-pattern grid is offered and
    /// `selected` picks one of them — the `ContainerButtonClick`-driven path.
    #[test]
    fn no_pattern_item_offers_the_base_grid_and_selected_picks_one() {
        let banner = stack("minecraft:white_banner");
        let dye = stack("minecraft:black_dye");

        assert_eq!(selectable_pattern_count(None), 32);
        let result = result(Some(&banner), Some(&dye), None, Some(0)).expect("must apply");
        assert_eq!(result.components.banner_patterns[0].pattern_asset_id, BASE_PATTERNS[0]);
        let result = result_at(&banner, &dye, 5);
        assert_eq!(result.components.banner_patterns[0].pattern_asset_id, BASE_PATTERNS[5]);
    }

    fn result_at(banner: &ItemStack, dye: &ItemStack, index: i32) -> ItemStack {
        result(Some(banner), Some(dye), None, Some(index)).expect("must apply")
    }

    /// No pattern item and no selection at all: the base grid has 32 options,
    /// so nothing auto-selects — the discriminating control against "any
    /// missing input still produces something."
    #[test]
    fn no_pattern_item_and_no_selection_produces_nothing() {
        let banner = stack("minecraft:white_banner");
        let dye = stack("minecraft:black_dye");
        assert_eq!(result(Some(&banner), Some(&dye), None, None), None);
    }

    /// Missing banner, missing dye, a non-banner/non-dye item, and a full
    /// six-pattern banner must all refuse — `LoomMenu`'s own guards.
    #[test]
    fn missing_or_invalid_inputs_and_a_full_banner_all_refuse() {
        let banner = stack("minecraft:white_banner");
        let dye = stack("minecraft:red_dye");
        let pattern_item = stack("minecraft:creeper_banner_pattern");
        let not_a_banner = stack("minecraft:stone");
        let not_a_dye = stack("minecraft:stone");

        assert_eq!(result(None, Some(&dye), Some(&pattern_item), None), None);
        assert_eq!(result(Some(&banner), None, Some(&pattern_item), None), None);
        assert_eq!(result(Some(&not_a_banner), Some(&dye), Some(&pattern_item), None), None);
        assert_eq!(result(Some(&banner), Some(&not_a_dye), Some(&pattern_item), None), None);

        let mut full = banner.clone();
        for _ in 0..MAX_BANNER_PATTERNS {
            full.components.banner_patterns.push(BannerPatternLayer {
                pattern_asset_id: "cross".to_string(),
                color: "red".to_string(),
            });
        }
        assert_eq!(
            result(Some(&full), Some(&dye), Some(&pattern_item), None),
            None,
            "a banner already at the six-layer cap must refuse a seventh"
        );
    }

    /// Item/dye/pattern-item detection: the path-suffix convention, both
    /// directions.
    #[test]
    fn item_kind_detection_matches_the_suffix_convention() {
        assert!(is_banner_item("minecraft:white_banner"));
        assert!(!is_banner_item("minecraft:white_wool"));
        assert!(is_dye_item("minecraft:lime_dye"));
        assert!(!is_dye_item("minecraft:lime_wool"));
        assert!(is_pattern_item("minecraft:skull_banner_pattern"));
        assert!(is_pattern_item("skull_banner_pattern"), "bare path (no namespace) must also match");
        assert!(!is_pattern_item("minecraft:skull"));
    }
}
