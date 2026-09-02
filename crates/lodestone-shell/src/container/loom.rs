//! The loom's 32-pattern grid (`ClientAction::ContainerButtonClick`'s
//! remainder for this screen — see [`super::stonecutter`]'s module doc for
//! the stonecutter's own producer, this module's own precedent, and
//! [`super::enchant`]'s for the original shape both follow).
//!
//! ## What it is
//!
//! `LoomScreen`/`LoomMenu` (`26.2`): a 4×4 grid of up to sixteen visible
//! pattern buttons, scrollable when more than sixteen patterns are offered.
//! Vanilla offers one of two lists depending on the pattern-item slot
//! (`LoomMenu.getSelectablePatterns`): a specific pattern *item* (e.g.
//! `minecraft:creeper_banner_pattern`) grants exactly one pattern and
//! auto-selects it with **no button click at all**
//! (`LoomMenu.slotsChanged`'s `selectablePatterns.size() == 1` branch,
//! reproduced server-side in `lodestone_server::loom::result`); an *empty*
//! pattern slot offers the 32-pattern base grid this module's [`grid_rect`]/
//! [`hit_test_local`] exist for.
//!
//! [`is_banner_item`]/[`is_dye_item`]/[`is_pattern_item`]/
//! [`selectable_pattern_count`] are the client-side mirror of
//! `lodestone-server`'s own `crate::loom` module — the server half landed
//! first and computes this authoritatively for real; this is the *client's*
//! copy of the identical tag-derived data, which is what lets a click be
//! pre-validated (bounded to the real offer count) before it is even sent,
//! exactly as vanilla's own client-side `LoomMenu` mirror does before
//! `clickMenuButton` ever reaches the network.
//!
//! ## How it works
//!
//! [`grid_rect`] is `LoomScreen`'s own real layout constants (`PATTERNS_X =
//! 60`, `PATTERNS_Y = 13`, a 14×14 cell, 4 columns, 4 visible rows) —
//! [`hit_test_local`] mirrors `LoomScreen.mouseClicked`'s exact arithmetic,
//! `start_row`-relative (vanilla's own local variable name — the loom scrolls
//! by **row**, not by absolute index the way the stonecutter's `start_index`
//! does, since `index = (row + startRow) * 4 + column` is computed fresh
//! inside the click loop rather than carried as one pre-multiplied number),
//! and [`button_hit_test`] adds the panel origin/scale resolution every click
//! surface in this crate goes through.
//!
//! ## How to change it
//!
//! [`PATTERN_ITEMS`]/[`BASE_PATTERNS`] are transcribed from the same real
//! datapack tag files `lodestone_server::loom`'s own doc names
//! (`tags/banner_pattern/pattern_item/*.json`,
//! `tags/banner_pattern/no_item_required.json`) — re-derive both from the
//! jar's own tag JSON if a future pattern item needs a row, not by guessing
//! the identity mapping most rows happen to follow (`bordure_indented`
//! grants `curly_border`, not its own name — the discriminating case this
//! module's own tests pin). No visual grid (icons, selected/highlighted
//! sprites) is drawn — matching [`super::stonecutter`]'s own disclosed scope
//! cut, which likewise draws nothing beyond what the existing panel
//! background already provides.
//!
//! Scrolling ([`start_row_for_scroll`]) is wired to the mouse wheel (see
//! `WindowApp::scroll_loom`) the same way [`super::stonecutter`]'s own
//! `start_index_for_scroll` is; **the scrollbar thumb drag is not** —
//! `LoomScreen.mouseDragged`'s own click-track/drag-track offset
//! inconsistency (`yo = topPos + 9` to *start* a drag,
//! `yscr = topPos + 13` to *continue* one) is real vanilla behaviour, not a
//! typo, but reproducing a drag surface for it was judged not worth the
//! plumbing when the wheel alone already reaches every offer past sixteen.
//!
//! ## Dependencies
//!
//! [`lodestone_game::item::ItemStack`] for the input-slot item kind checks,
//! [`super::layout`] for the panel origin/scale seam every other click
//! surface in this crate resolves a cursor through.

use lodestone_game::item::ItemStack;

use super::layout::Rect;

/// `LoomMenu`'s own slot indices — `Menu::loom`'s doc: banner (`0`), dye
/// (`1`), pattern item (`2`), result (`3`).
pub const BANNER_SLOT: usize = 0;
pub const DYE_SLOT: usize = 1;
pub const PATTERN_SLOT: usize = 2;

/// A banner's own real cap — `BannerBlockEntity`'s pattern list is capped at
/// six layers (`hasMaxPatterns` in `LoomMenu.slotsChanged`), mirroring
/// `lodestone_server::loom::MAX_BANNER_PATTERNS`.
pub const MAX_BANNER_PATTERNS: usize = 6;

/// `LoomScreen.PATTERNS_X`/`PATTERNS_Y`/cell size/column count.
const GRID_X: f32 = 60.0;
const GRID_Y: f32 = 13.0;
const CELL: f32 = 14.0;
const COLUMNS: i32 = 4;
/// `LoomScreen`'s four visible rows (sixteen visible buttons at once).
const VISIBLE_ROWS: i32 = 4;

/// `tags/banner_pattern/pattern_item/*.json`, one row per file — `(pattern
/// id, item suffix)`. See this module's own doc for why the pair is not an
/// identity mapping for `bordure_indented`/`field_masoned` — the exact table
/// `lodestone_server::loom::PATTERN_ITEMS` carries, transcribed independently
/// as this crate's own client-side mirror.
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
/// order — the same transcription `lodestone_server::loom::BASE_PATTERNS`
/// carries.
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

/// `LoomMenu.isDyeItem` — the same `*_dye` suffix convention every dye item
/// already follows in this crate.
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

/// `LoomMenu.getSelectablePatterns`'s own count: the pattern-item slot's
/// single granted pattern, the 32-pattern base grid when the slot is empty,
/// or zero for an item this crate does not recognise as a pattern item
/// (vanilla's own `mayPlace` would already have refused it into the slot).
#[must_use]
pub fn selectable_pattern_count(pattern_item: Option<&ItemStack>) -> usize {
    match pattern_item {
        None => BASE_PATTERNS.len(),
        Some(stack) => {
            let bare = stack.item().to_string();
            let bare = bare.strip_prefix("minecraft:").unwrap_or(&bare).to_owned();
            usize::from(PATTERN_ITEMS.iter().any(|(name, _)| *name == bare))
        }
    }
}

/// `LoomScreen`'s own `displayPatterns` gate (`containerChanged`): a banner
/// and a dye both present, the banner not already at its six-layer cap, and
/// at least one pattern offered.
#[must_use]
pub fn display_patterns(
    banner: Option<&ItemStack>,
    dye: Option<&ItemStack>,
    pattern_item: Option<&ItemStack>,
) -> bool {
    let Some(banner) = banner else { return false };
    if dye.is_none() {
        return false;
    }
    if banner.banner_patterns().len() >= MAX_BANNER_PATTERNS {
        return false;
    }
    selectable_pattern_count(pattern_item) > 0
}

/// One pattern button's local-widget-pixel rect, `index`-relative to
/// `start_row` — `LoomScreen.extractBackground`'s own `posX`/`posY`
/// (`x + column * 14`, `y + row * 14`), where `row`/`column` are recovered
/// from the absolute `index` the same way `LoomScreen.mouseClicked`'s loop
/// derives `index = (row + startRow) * 4 + column`.
#[must_use]
#[allow(clippy::cast_precision_loss)] // index/start_row are always small
pub fn grid_rect(index: i32, start_row: i32) -> Option<Rect> {
    let column = index.rem_euclid(COLUMNS);
    let actual_row = index.div_euclid(COLUMNS);
    let pos_row = actual_row - start_row;
    if !(0..VISIBLE_ROWS).contains(&pos_row) {
        return None;
    }
    Some(Rect {
        x: GRID_X + column as f32 * CELL,
        y: GRID_Y + pos_row as f32 * CELL,
        w: CELL,
        h: CELL,
    })
}

fn hit(x: f32, y: f32, r: Rect) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

/// Resolves a **local widget-pixel** point to the pattern index it hits, if
/// any — vanilla's own loom-screen click handling's own nested loop, `start_row`-relative,
/// bounded by `pattern_count` (vanilla's own selectable-patterns count):
/// a partially-filled last row must not accept a click past the real pattern
/// count even though its cell rect exists, the same guard
/// [`super::stonecutter::hit_test_local`] carries.
#[must_use]
pub fn hit_test_local(pattern_count: usize, start_row: i32, x: f32, y: f32) -> Option<i32> {
    for row in 0..VISIBLE_ROWS {
        for column in 0..COLUMNS {
            let actual_row = row + start_row;
            let index = actual_row * COLUMNS + column;
            if index >= 0
                && (index as usize) < pattern_count
                && let Some(r) = grid_rect(index, start_row)
                && hit(x, y, r)
            {
                return Some(index);
            }
        }
    }
    None
}

/// `LoomScreen.totalRowCount`: `ceil(pattern_count / 4)`, minus the four
/// visible rows, floored at `0` — the same shape
/// [`super::stonecutter::offscreen_rows`] uses for its own three-row window.
#[must_use]
fn offscreen_rows(pattern_count: usize) -> i32 {
    let rows = (pattern_count as i32 + COLUMNS - 1) / COLUMNS - VISIBLE_ROWS;
    rows.max(0)
}

/// `LoomScreen.mouseDragged`/`mouseScrolled`'s shared tail:
/// `startRow = (scrollOffs * offscreenRows + 0.5) as i32`, `scroll_offset`
/// clamped to `0.0..=1.0` first exactly as vanilla clamps it before either
/// call site uses it. **Not pre-multiplied by the column count** — unlike
/// [`super::stonecutter::start_index_for_scroll`]'s `start_index`, this is a
/// row count, matching `LoomScreen`'s own `startRow` field.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // matches vanilla's own (int) cast
pub fn start_row_for_scroll(scroll_offset: f32, pattern_count: usize) -> i32 {
    let clamped = scroll_offset.clamp(0.0, 1.0);
    let rows = offscreen_rows(pattern_count) as f32;
    (clamped * rows + 0.5) as i32
}

/// `LoomScreen.mouseScrolled`'s own step: `scrollOffs = clamp(scrollOffs -
/// scrollY / offscreenRows, 0, 1)` — a no-op (returns `current` unchanged)
/// when nothing is offscreen, matching vanilla's own `offscreenRows > 0`
/// guard rather than dividing by zero.
#[must_use]
pub fn scroll_offset_after_wheel(current: f32, notches: f32, pattern_count: usize) -> f32 {
    let rows = offscreen_rows(pattern_count);
    if rows <= 0 {
        return 0.0;
    }
    (current - notches / rows as f32).clamp(0.0, 1.0)
}

/// [`hit_test_local`] plus the panel-origin/scale resolution every other
/// click surface in this crate does — the same shape as
/// [`super::stonecutter::button_hit_test`]. `None` off any non-loom screen.
#[must_use]
pub fn button_hit_test(
    menu: &lodestone_game::menu::Menu,
    gui_scale: u32,
    width: u32,
    height: u32,
    x: f32,
    y: f32,
    pattern_count: usize,
    start_row: i32,
) -> Option<i32> {
    if menu.special_layout() != Some(lodestone_game::menu::SpecialLayout::Loom) {
        return None;
    }
    let layout = super::layout::slot_layout(menu);
    let (px, py) = super::layout::panel_origin_with_scale(&layout, gui_scale, width, height);
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    hit_test_local(pattern_count, start_row, x / scale - px, y / scale - py)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str) -> ItemStack {
        ItemStack::new(item.parse().expect("valid key"), 1)
    }

    #[test]
    fn item_kind_detection_matches_the_suffix_convention() {
        assert!(is_banner_item("minecraft:white_banner"));
        assert!(!is_banner_item("minecraft:white_wool"));
        assert!(is_dye_item("minecraft:lime_dye"));
        assert!(!is_dye_item("minecraft:lime_wool"));
        assert!(is_pattern_item("minecraft:skull_banner_pattern"));
        assert!(!is_pattern_item("minecraft:skull"));
    }

    #[test]
    fn selectable_pattern_count_matches_the_transcribed_tag_tables() {
        assert_eq!(selectable_pattern_count(None), 32, "empty pattern slot offers the base grid");
        let creeper = stack("minecraft:creeper_banner_pattern");
        assert_eq!(selectable_pattern_count(Some(&creeper)), 1, "a pattern item auto-selects its own single pattern");
        let not_a_pattern = stack("minecraft:stone");
        assert_eq!(selectable_pattern_count(Some(&not_a_pattern)), 0, "an unrecognised item offers nothing");
    }

    #[test]
    fn display_patterns_needs_a_banner_and_a_dye_and_room_and_an_offer() {
        let banner = stack("minecraft:white_banner");
        let dye = stack("minecraft:red_dye");
        assert!(display_patterns(Some(&banner), Some(&dye), None));
        assert!(!display_patterns(None, Some(&dye), None), "no banner: nothing to show");
        assert!(!display_patterns(Some(&banner), None, None), "no dye: nothing to show");

        let mut full = banner.clone();
        full.set_banner_patterns(
            (0..MAX_BANNER_PATTERNS)
                .map(|_| lodestone_model::BannerPatternLayer {
                    pattern_asset_id: "cross".to_string(),
                    color: "red".to_string(),
                })
                .collect(),
        );
        assert!(!display_patterns(Some(&full), Some(&dye), None), "a full banner must refuse, matching hasMaxPatterns");

        let not_a_pattern = stack("minecraft:stone");
        assert!(
            !display_patterns(Some(&banner), Some(&dye), Some(&not_a_pattern)),
            "an item this crate does not recognise as a pattern item offers zero patterns"
        );
    }

    #[test]
    fn grid_rect_matches_the_transcribed_loom_screen_arithmetic() {
        assert_eq!(grid_rect(0, 0), Some(Rect { x: 60.0, y: 13.0, w: 14.0, h: 14.0 }));
        assert_eq!(grid_rect(3, 0), Some(Rect { x: 102.0, y: 13.0, w: 14.0, h: 14.0 }));
        assert_eq!(grid_rect(4, 0), Some(Rect { x: 60.0, y: 27.0, w: 14.0, h: 14.0 }));
        // Scrolled: index 20 with start_row 2 is actual_row 5, pos_row 3, col 0.
        assert_eq!(grid_rect(20, 2), Some(Rect { x: 60.0, y: 13.0 + 3.0 * 14.0, w: 14.0, h: 14.0 }));
        // Out of the visible 4x4 window relative to start_row: no rect.
        assert_eq!(grid_rect(20, 0), None);
    }

    #[test]
    fn hit_test_finds_the_index_a_point_falls_in() {
        let r = grid_rect(6, 0).unwrap();
        assert_eq!(hit_test_local(32, 0, r.x + 1.0, r.y + 1.0), Some(6));
        assert_eq!(hit_test_local(32, 0, -5.0, -5.0), None);
    }

    #[test]
    fn hit_test_refuses_a_cell_past_the_real_pattern_count() {
        // Only 5 real patterns (a hypothetical partial-row case): cell index 5
        // exists geometrically but must not be clickable.
        let r = grid_rect(5, 0).unwrap();
        assert_eq!(hit_test_local(5, 0, r.x + 1.0, r.y + 1.0), None);
        assert_eq!(hit_test_local(6, 0, r.x + 1.0, r.y + 1.0), Some(5));
    }

    #[test]
    fn offscreen_rows_and_scroll_start_row_match_the_transcribed_formula() {
        // 16 or fewer patterns: nothing to scroll.
        assert_eq!(offscreen_rows(16), 0);
        assert_eq!(start_row_for_scroll(1.0, 16), 0);
        // The real base grid: 32 patterns -> 8 rows total -> 4 offscreen rows.
        assert_eq!(offscreen_rows(32), 4);
        assert_eq!(start_row_for_scroll(0.0, 32), 0);
        // (1.0 * 4 + 0.5) as i32 = 4.
        assert_eq!(start_row_for_scroll(1.0, 32), 4);
        // scroll_offset outside 0..=1 is clamped first.
        assert_eq!(start_row_for_scroll(5.0, 32), start_row_for_scroll(1.0, 32));
        assert_eq!(start_row_for_scroll(-5.0, 32), start_row_for_scroll(0.0, 32));
    }

    #[test]
    fn wheel_scroll_matches_the_transcribed_formula_and_is_a_no_op_with_nothing_offscreen() {
        // 32 patterns -> 4 offscreen rows. One notch moves 1/4 of the range.
        assert_eq!(scroll_offset_after_wheel(0.0, -1.0, 32), 0.25);
        assert_eq!(scroll_offset_after_wheel(0.25, -1.0, 32), 0.5);
        // Clamped at the top and bottom.
        assert_eq!(scroll_offset_after_wheel(0.0, 5.0, 32), 0.0);
        assert_eq!(scroll_offset_after_wheel(1.0, -5.0, 32), 1.0);
        // Nothing offscreen: always pinned at 0, never divides by zero.
        assert_eq!(scroll_offset_after_wheel(0.5, -1.0, 16), 0.0);
    }
}
