//! The Statistics screen, reached from the pause menu's
//! Statistics button.
//!
//! ## What is and is not built
//!
//! The reference screen has three tabs: **General** (77 fixed statistics),
//! **Items** (per-item counters), and **Mobs** (per-entity counters). Only
//! **General** is built here as a real scrollable list.
//! Items and Mobs are present-and-inactive tab buttons, for a reason that is
//! not a scope cut so much as already-correct behaviour:
//! The Items and Mobs tabs are disabled when their lists are empty. Every row
//! in those lists is empty here because this screen has no per-item or
//! per-entity projection, so the disabled state is intentional.
//!
//! ## Where the numbers come from
//!
//! Statistics are folded into the session model and projected onto the fixed
//! General-tab table once per frame. The live path is:
//!
//! ```text
//! award_stats -> lodestone_game::progress::Statistics (SessionStatistics)
//!   -> Sim::statistics()
//!   -> StatsSnapshot::from_statistics        -- projection onto GENERAL_STATS
//!   -> MenuNav::refresh_stats                -- app::session, once per frame
//!   -> dispatch: stats::frame(nav.stats(), nav.stats_snapshot())
//! ```
//!
//! An empty table is still the correct state *outside* a session, and still the
//! correct state for a fresh world where nothing has happened yet — a stat
//! reading zero is honest in a way a settings row showing a fabricated "ON" is
//! not (`docs/settings-screen.md`'s departure 1). What is no longer true is that
//! zero is the *only* state reachable.
//!
//! **The Items and Mobs tabs are present-and-inactive**: they need per-block and
//! per-entity id tables this screen's flat
//! 77-row model does not have, so a `minecraft:mined` counter is deliberately
//! dropped by the projection rather than squeezed onto a General row.
//!
//! Consequently Items and Mobs are empty: their rows require data that this
//! projection deliberately drops, rather than inventing values for a different
//! category.
//!
//! ## Wired vs. decorative
//!
//! - **Wired**: reaching the screen from the pause menu and back
//!   (Escape/Done), the General tab's 77 rows, captions and four format rules
//!   (`DEFAULT`, `DIVIDE_BY_TEN`, `DISTANCE`, `TIME`), tested against known
//!   non-zero inputs and an explicit row-count census.
//! - **Decorative**: Items and Mobs values, because their source counters are
//!   not projected into this screen. General values are live when a session
//!   supplies them and zero outside a session.
//!
//! ## Geometry
//!
//! Flat rows, no header-padding rule — the same departure
//! [`super::key_binds`] and [`super::social`] already make for their own
//! non-`OptionsList` screens, reusing [`super::options`]'s footer primitives
//! rather than a fourth reimplementation of the same arithmetic.

use super::layout;
use super::options::{self, Placement};
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot, TabEntryView};
use super::widget;

/// `gui.stats`.
pub const TITLE: &str = "Statistics";

/// Labels for this screen's three tabs.
pub const TAB_LABELS: [&str; 3] = ["General", "Items", "Mobs"];
/// [`TAB_LABELS`]'s index of the only tab this screen builds a real list for
/// (see the module docs above) — General.
pub const GENERAL_TAB: usize = 0;

/// The pixel rect of tab `index` (into [`TAB_LABELS`]) at canvas `width`, via
/// the shared [`layout::tab_bar_row_rect`]. See
/// [`super::render::TabEntryView::index`]'s own doc on why a `Slot` cannot
/// express this row's *width*, let alone its `x`.
#[must_use]
pub fn tab_row_rect(index: usize, width: f32) -> (f32, f32, f32, f32) {
    layout::tab_bar_row_rect(index, TAB_LABELS.len(), width)
}

/// The row index of Done. It remains first because the tab rows are appended
/// after it (see [`frame`]).
///
/// Named rather than written as a bare `0` at three sites, because "row 0" and
/// "the Done button" being the same number is exactly what made the focus bug
/// in [`StatsNav::focused`] read as harmless.
pub const DONE_ROW: usize = 0;

/// The four display formats used by the General tab. `Default` is a
/// thousands-grouped integer with no decimals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatFormat {
    Default,
    /// Multiplies by `0.1` and emits two decimal places, with no
    /// thousands grouping (`"########0.00"`).
    DivideByTen,
    /// Converts centimetres to `km`, `m` or `cm` using the measured thresholds.
    Distance,
    /// Converts redstone ticks (1/20 s) to the largest unit over `0.5`. The
    /// `< 0.5 min` branch uses the raw seconds value.
    Time,
}

impl StatFormat {
    /// Emits exactly two decimal places, no thousands grouping, and at least
    /// one digit before the point.
    fn decimal(value: f64) -> String {
        format!("{value:.2}")
    }

    /// The JDK's own integer number format for the US locale: thousands-grouped,
    /// integral. `value` is `i64` because [`Self::format`]'s caller widens
    /// before dividing (see its own doc on why `i32::MIN` cannot be negated
    /// in place).
    fn grouped(value: i64) -> String {
        let neg = value < 0;
        let digits = value.unsigned_abs().to_string();
        let mut grouped = String::new();
        for (i, ch) in digits.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                grouped.push(',');
            }
            grouped.push(ch);
        }
        let mut out: String = grouped.chars().rev().collect();
        if neg {
            out.insert(0, '-');
        }
        out
    }

    /// Formats a raw statistic value according to [`Self`].
    #[must_use]
    pub fn format(self, value: i32) -> String {
        match self {
            StatFormat::Default => Self::grouped(i64::from(value)),
            StatFormat::DivideByTen => Self::decimal(f64::from(value) * 0.1),
            StatFormat::Distance => {
                let cm = value;
                let meters = f64::from(cm) / 100.0;
                let kilometers = meters / 1000.0;
                if kilometers > 0.5 {
                    format!("{} km", Self::decimal(kilometers))
                } else if meters > 0.5 {
                    format!("{} m", Self::decimal(meters))
                } else {
                    format!("{cm} cm")
                }
            }
            StatFormat::Time => {
                let seconds = f64::from(value) / 20.0;
                let minutes = seconds / 60.0;
                let hours = minutes / 60.0;
                let days = hours / 24.0;
                let years = days / 365.0;
                if years > 0.5 {
                    format!("{} y", Self::decimal(years))
                } else if days > 0.5 {
                    format!("{} d", Self::decimal(days))
                } else if hours > 0.5 {
                    format!("{} h", Self::decimal(hours))
                } else if minutes > 0.5 {
                    format!("{} min", Self::decimal(minutes))
                } else {
                    // Preserve the raw seconds representation for values below
                    // one minute; whole values keep a visible `.0` suffix.
                    format!("{} s", java_double_to_string(seconds))
                }
            }
        }
    }
}

/// Formats the raw seconds value used below one minute. Unlike Rust's default
/// `f64` display, whole values retain one digit after the point (`29.0`).
/// Every value this is called with is an exact multiple of `1.0 / 20.0`
/// (redstone ticks), so the fractional part (when there is one) is a simple
/// terminating decimal and Rust's own shortest round-trip formatting already
/// agrees with the reference formatter there; the only correction needed is
/// forcing the `.0` suffix Rust omits for a whole number.
fn java_double_to_string(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// The 77 General-tab statistics in source declaration order: id, caption and
/// [`StatFormat`]. Display sorting happens in [`general_rows`], so new entries
/// need not be inserted alphabetically here.
pub static GENERAL_STATS: &[(&str, &str, StatFormat)] = &[
    ("leave_game", "Games Quit", StatFormat::Default),
    ("play_time", "Time Played", StatFormat::Time),
    ("total_world_time", "Time with World Open", StatFormat::Time),
    ("time_since_death", "Time Since Last Death", StatFormat::Time),
    ("time_since_rest", "Time Since Last Rest", StatFormat::Time),
    ("sneak_time", "Sneak Time", StatFormat::Time),
    ("walk_one_cm", "Distance Walked", StatFormat::Distance),
    ("crouch_one_cm", "Distance Crouched", StatFormat::Distance),
    ("sprint_one_cm", "Distance Sprinted", StatFormat::Distance),
    ("walk_on_water_one_cm", "Distance Walked on Water", StatFormat::Distance),
    ("fall_one_cm", "Distance Fallen", StatFormat::Distance),
    ("climb_one_cm", "Distance Climbed", StatFormat::Distance),
    ("fly_one_cm", "Distance Flown", StatFormat::Distance),
    ("walk_under_water_one_cm", "Distance Walked under Water", StatFormat::Distance),
    ("minecart_one_cm", "Distance by Minecart", StatFormat::Distance),
    ("boat_one_cm", "Distance by Boat", StatFormat::Distance),
    ("pig_one_cm", "Distance by Pig", StatFormat::Distance),
    ("happy_ghast_one_cm", "Distance by Happy Ghast", StatFormat::Distance),
    ("horse_one_cm", "Distance by Horse", StatFormat::Distance),
    ("aviate_one_cm", "Distance by Elytra", StatFormat::Distance),
    ("swim_one_cm", "Distance Swum", StatFormat::Distance),
    ("strider_one_cm", "Distance by Strider", StatFormat::Distance),
    ("nautilus_one_cm", "Distance by Nautilus", StatFormat::Distance),
    ("jump", "Jumps", StatFormat::Default),
    ("drop", "Items Dropped", StatFormat::Default),
    ("damage_dealt", "Damage Dealt", StatFormat::DivideByTen),
    ("damage_dealt_absorbed", "Damage Dealt (Absorbed)", StatFormat::DivideByTen),
    ("damage_dealt_resisted", "Damage Dealt (Resisted)", StatFormat::DivideByTen),
    ("damage_taken", "Damage Taken", StatFormat::DivideByTen),
    ("damage_blocked_by_shield", "Damage Blocked by Shield", StatFormat::DivideByTen),
    ("damage_absorbed", "Damage Absorbed", StatFormat::DivideByTen),
    ("damage_resisted", "Damage Resisted", StatFormat::DivideByTen),
    ("deaths", "Number of Deaths", StatFormat::Default),
    ("mob_kills", "Mob Kills", StatFormat::Default),
    ("animals_bred", "Animals Bred", StatFormat::Default),
    ("player_kills", "Player Kills", StatFormat::Default),
    ("fish_caught", "Fish Caught", StatFormat::Default),
    ("talked_to_villager", "Talked to Villagers", StatFormat::Default),
    ("traded_with_villager", "Traded with Villagers", StatFormat::Default),
    ("eat_cake_slice", "Cake Slices Eaten", StatFormat::Default),
    ("fill_cauldron", "Cauldrons Filled", StatFormat::Default),
    ("use_cauldron", "Water Taken from Cauldron", StatFormat::Default),
    ("clean_armor", "Armor Pieces Cleaned", StatFormat::Default),
    ("clean_banner", "Banners Cleaned", StatFormat::Default),
    ("clean_shulker_box", "Shulker Boxes Cleaned", StatFormat::Default),
    ("interact_with_brewingstand", "Interactions with Brewing Stand", StatFormat::Default),
    ("interact_with_beacon", "Interactions with Beacon", StatFormat::Default),
    ("inspect_dropper", "Droppers Searched", StatFormat::Default),
    ("inspect_hopper", "Hoppers Searched", StatFormat::Default),
    ("inspect_dispenser", "Dispensers Searched", StatFormat::Default),
    ("play_noteblock", "Note Blocks Played", StatFormat::Default),
    ("tune_noteblock", "Note Blocks Tuned", StatFormat::Default),
    ("pot_flower", "Plants Potted", StatFormat::Default),
    ("trigger_trapped_chest", "Trapped Chests Triggered", StatFormat::Default),
    ("open_enderchest", "Ender Chests Opened", StatFormat::Default),
    ("enchant_item", "Items Enchanted", StatFormat::Default),
    ("play_record", "Music Discs Played", StatFormat::Default),
    ("interact_with_furnace", "Interactions with Furnace", StatFormat::Default),
    ("interact_with_crafting_table", "Interactions with Crafting Table", StatFormat::Default),
    ("open_chest", "Chests Opened", StatFormat::Default),
    ("sleep_in_bed", "Times Slept in a Bed", StatFormat::Default),
    ("open_shulker_box", "Shulker Boxes Opened", StatFormat::Default),
    ("open_barrel", "Barrels Opened", StatFormat::Default),
    ("interact_with_blast_furnace", "Interactions with Blast Furnace", StatFormat::Default),
    ("interact_with_smoker", "Interactions with Smoker", StatFormat::Default),
    ("interact_with_lectern", "Interactions with Lectern", StatFormat::Default),
    ("interact_with_campfire", "Interactions with Campfire", StatFormat::Default),
    ("interact_with_cartography_table", "Interactions with Cartography Table", StatFormat::Default),
    ("interact_with_loom", "Interactions with Loom", StatFormat::Default),
    ("interact_with_stonecutter", "Interactions with Stonecutter", StatFormat::Default),
    ("bell_ring", "Bells Rung", StatFormat::Default),
    ("raid_trigger", "Raids Triggered", StatFormat::Default),
    ("raid_win", "Raids Won", StatFormat::Default),
    ("interact_with_anvil", "Interactions with Anvil", StatFormat::Default),
    ("interact_with_grindstone", "Interactions with Grindstone", StatFormat::Default),
    ("target_hit", "Targets Hit", StatFormat::Default),
    ("interact_with_smithing_table", "Interactions with Smithing Table", StatFormat::Default),
];

/// The statistic category every id in [`GENERAL_STATS`] lives in.
///
/// The General tab uses the `minecraft:custom` category. Thus
/// `"sleep_in_bed"` on the screen maps to the wire key
/// `StatKey { category: "minecraft:custom", value: "minecraft:sleep_in_bed" }`.
/// The category id is the registry name, not the tab label.
const CUSTOM_STAT_CATEGORY: &str = "minecraft:custom";

/// The live values behind [`GENERAL_STATS`]. A sparse map (not a `[i32; 77]`)
/// so only the ids the server has actually awarded are stored, exactly
/// mirroring the sparse session counter used as its input.
///
/// A populated snapshot comes from decoded `award_stats` folded into
/// `lodestone_ecs::SessionStatistics`, and
/// [`from_statistics`](Self::from_statistics) projects onto this screen's fixed
/// 77 ids; `app::session`'s per-frame reconciliation pushes it through
/// `MenuNav::refresh_stats`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    values: std::collections::HashMap<&'static str, i32>,
}

impl StatsSnapshot {
    #[must_use]
    pub fn get(&self, id: &str) -> i32 {
        self.values.get(id).copied().unwrap_or(0)
    }

    /// Project a folded [`lodestone_game::progress::Statistics`] onto this
    /// screen's fixed id list.
    ///
    /// Driven by [`GENERAL_STATS`] rather than by the store's own keys, for two
    /// reasons. The screen can only display these 77 ids, so a mined-block or
    /// mob-kill counter has nowhere to go (they are the Items and Mobs tabs,
    /// which are present-and-inactive). And the map's keys are `&'static str`,
    /// so they must come from the table, not from a server-supplied string.
    ///
    /// Absent stays absent rather than being stored as `0`: [`Self::get`]
    /// already reads a missing id as zero, and storing zeros would make
    /// "awarded, and the value is 0" indistinguishable from "never awarded" for
    /// any future consumer.
    #[must_use]
    pub fn from_statistics(stats: &lodestone_game::progress::Statistics) -> Self {
        use std::str::FromStr as _;

        let mut values = std::collections::HashMap::new();
        // Parsed once: an invalid category would make every lookup miss, and
        // that would look exactly like "the server awarded nothing".
        let Ok(category) = lodestone_model::Identifier::from_str(CUSTOM_STAT_CATEGORY) else {
            return Self::default();
        };
        for &(id, _, _) in GENERAL_STATS {
            // Every id in the table is a bare path; `Identifier`'s parse
            // defaults the namespace to `minecraft`, so it is not restated.
            let Ok(value_id) = lodestone_model::Identifier::from_str(id) else {
                continue;
            };
            let value = stats.get(&lodestone_game::progress::StatKey::new(
                category.clone(),
                value_id,
            ));
            if value != 0 {
                values.insert(id, value);
            }
        }
        Self { values }
    }

    /// Test/oracle-only setter. Production writes go through
    /// [`from_statistics`](Self::from_statistics).
    #[cfg(test)]
    fn set(&mut self, id: &'static str, value: i32) {
        self.values.insert(id, value);
    }
}

/// One row: caption plus formatted value, in **display** order —
/// sorted by the translated caption, not by
/// [`GENERAL_STATS`]'s declaration order.
#[must_use]
pub fn general_rows(snapshot: &StatsSnapshot) -> Vec<(&'static str, String)> {
    let mut rows: Vec<(&'static str, String)> = GENERAL_STATS
        .iter()
        .map(|&(id, caption, fmt)| (caption, fmt.format(snapshot.get(id))))
        .collect();
    rows.sort_by_key(|(caption, _)| *caption);
    rows
}

// -- geometry: a flat list, same departure key_binds.rs/social.rs make ------

/// Height of a General-tab row. It is 14 px, distinct from the 20 px button
/// height used by [`options::WIDGET_H`]. The inactive tabs retain their own
/// measured row heights for a future per-category projection.
pub const ROW_H: f32 = 14.0;
/// Measured row height reserved for the Items tab.
pub const ITEMS_ROW_H: f32 = 22.0;
/// Measured row height reserved for the Mobs tab: four 9 px text lines.
pub const MOBS_ROW_H: f32 = 9.0 * 4.0;
/// Half the list's column width — the name column runs from
/// [`Origin::ScreenTop`]`- COLUMN_HALF_W + NAME_LEFT_INSET` to centre, the
/// value column from centre to `+ COLUMN_HALF_W - VALUE_RIGHT_MARGIN`.
const COLUMN_HALF_W: f32 = 150.0;
const VALUE_RIGHT_MARGIN: f32 = 10.0;
const NAME_LEFT_INSET: f32 = 4.0;

/// This screen's header height — **not**
/// [`options::SUB_HEADER_HEIGHT`], which is `HeaderAndFooterLayout`'s default
/// 33 px title band and is not what this screen uses. The tab bar occupies
/// [`layout::TAB_BAR_HEIGHT`] (24 px) at the top, so the list begins directly
/// beneath that band. Using a 33 px header would place the separator 9 px too
/// low and collide with the tab labels.
pub const HEADER_HEIGHT: f32 = layout::TAB_BAR_HEIGHT;

pub const LIST_WINDOW_PX: f32 =
    crate::config::MIN_SCALED_HEIGHT as f32 - HEADER_HEIGHT - options::FOOTER_HEIGHT - options::LIST_TOP_INSET;

/// Top of the list band — the y a row at scroll `0.0` starts at.
#[must_use]
pub fn band_top() -> f32 {
    HEADER_HEIGHT + options::LIST_TOP_INSET
}

/// This screen's [`widget::ListSpec`], the one declaration the
/// scrollbar, the wheel and the row placement all read.
///
/// `top` is [`HEADER_HEIGHT`] rather than [`band_top`]: the spec's band is the
/// *window*, and [`widget::ScrollList`] adds [`widget::LIST_CONTENT_PADDING`]
/// itself as `first_entry_y`. Passing the already-inset value would inset
/// twice, which is the one arithmetic slip that can still look right at scroll
/// zero.
#[must_use]
pub fn list_spec(len: usize, scroll: f32) -> widget::ListSpec {
    widget::ListSpec::uniform(
        ROW_H,
        HEADER_HEIGHT,
        options::FOOTER_HEIGHT,
        len,
        COLUMN_HALF_W * 2.0,
    )
    .at(scroll)
}

/// This screen has no per-row control (a stat row is not clickable), so there is
/// nothing to place beyond the row's
/// own text: every row is a pair of [`super::render::MenuLabel`]s at this y,
/// not a [`super::render::MenuRow`]/[`Slot`].
///
/// **The offset is pixels**. `scroll.floor()` applies one
/// truncation before placement, matching [`widget::ScrollList::row_top`].
#[must_use]
pub fn row_label_y(row: u16, scroll: f32) -> f32 {
    band_top() - scroll.floor() + f32::from(row) * ROW_H
}

/// Zebra striping uses `index % 2 == 0 ? -1 : -4539718` — opaque white
/// on an even displayed row, `0xFFBABABA` on an odd one. `index` is the row's
/// position in the **already-sorted** list ([`general_rows`]'s output order),
/// matching the index in [`general_rows`]'s displayed order.
#[must_use]
pub fn general_row_colour(index: usize) -> [f32; 4] {
    if index % 2 == 0 {
        widget::argb_to_rgba(-1)
    } else {
        widget::argb_to_rgba(-4_539_718)
    }
}

/// This screen's own scroll cursor. General rows are not buttons; only Done is
/// a real control.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StatsNav {
    /// Scroll offset in **pixels**, not a row index.
    ///
    /// A row-index offset would always be a multiple of
    /// [`ROW_H`], which is precisely the snap-to-row behaviour the wheel work
    /// exists to remove. See [`widget::ListSpec`]'s own doc.
    scroll: f32,
    /// Whether Done — the screen's only control — currently holds keyboard
    /// focus. It starts false because opening the screen by mouse does not
    /// focus a child, and keyboard traversal reaches the tab bar before the
    /// footer. A click or Tab explicitly grants focus.
    focused: bool,
    /// Whether the mouse is over Done. The tab bar derives its hover directly
    /// from `MenuFrame::cursor`, while Done needs this explicit field so the
    /// renderer can draw its outline.
    hovered: Option<usize>,
}

impl StatsNav {
    pub fn reset(&mut self) {
        self.scroll = 0.0;
        self.focused = false;
        self.hovered = None;
    }

    /// The mouse moved over row `row`. Only [`DONE_ROW`] can record hover —
    /// there is nothing else on this screen `MenuFrame::hovered` drives a
    /// highlight for (the General list has no per-row control, and the tab
    /// bar's own hover is geometry-derived, not this field).
    pub fn hover_row(&mut self, row: usize) {
        self.hovered = (row == DONE_ROW).then_some(DONE_ROW);
    }

    /// The row the mouse is over, for [`super::render::MenuFrame::hovered`].
    #[must_use]
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// The offset, in pixels.
    #[must_use]
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// The live [`widget::ScrollList`] for a given canvas height, or `None`
    /// when there is nothing to scroll.
    #[must_use]
    fn model(&self, canvas_height: f32) -> Option<widget::ScrollList> {
        list_spec(GENERAL_STATS.len(), self.scroll).model(canvas_height)
    }

    /// One mouse-wheel notch, through the primitive. Positive scrolls **up**;
    /// the negation lives in [`widget::ScrollList::mouse_scrolled`] and nowhere
    /// else.
    pub fn scroll_by(&mut self, notches: f32, canvas_height: f32) {
        let Some(mut list) = self.model(canvas_height) else {
            return;
        };
        list.mouse_scrolled(notches);
        self.scroll = list.scroll();
    }

    /// Whether Done holds keyboard focus. See [`Self::focused`]'s own doc for
    /// why this starts `false`.
    #[must_use]
    pub fn focused(&self) -> bool {
        self.focused
    }

    /// Tab traversal has exactly one focusable child on this screen, so
    /// forward Tab focuses Done and remains there rather than toggling it.
    pub fn focus_next(&mut self) {
        self.focused = true;
    }

    /// A click on a widget focuses it *and then* activates it. Hover does not —
    /// see [`Self::focused`].
    pub fn focus_done(&mut self) {
        self.focused = true;
    }

    /// Arrow-key scroll: one row's worth of pixels, clamped by the primitive.
    ///
    /// The census (77) is fixed, so this needs no snapshot — unlike
    /// [`GENERAL_STATS`]'s *values*, its *length* never changes.
    ///
    /// Measured against [`crate::config::MIN_SCALED_HEIGHT`] rather than the
    /// live canvas for the same reason the accounts screen's keyboard path
    /// does: a keypress has no canvas in hand, and the smallest supported
    /// canvas is the conservative choice — it can only over-scroll into a
    /// region a larger canvas also shows.
    pub fn step(&mut self, forward: bool) {
        let Some(mut list) = self.model(crate::config::MIN_SCALED_HEIGHT as f32) else {
            return;
        };
        let delta = if forward { ROW_H } else { -ROW_H };
        list.set_scroll(list.scroll() + delta);
        self.scroll = list.scroll();
    }
}

/// Builds the whole Statistics frame.
///
/// The title is retained for narration, but the frame draws no separate title
/// label: the tab bar is the complete header and the content begins below it.
#[must_use]
pub fn frame(nav: &StatsNav, snapshot: &StatsSnapshot) -> MenuFrame<'static> {
    let stats = general_rows(snapshot);

    // Emit every row and let `render::draw` clip `list_labels` to the band, so
    // a row straddling the bottom contributes its visible half.
    let scroll = nav.scroll();
    let mut list_labels = Vec::with_capacity(stats.len() * 2);
    for (i, (caption, value)) in stats.iter().enumerate() {
        let y = row_label_y(i as u16, scroll);
        // Zebra striping: both labels in a row share one colour computed from
        // the displayed index.
        let colour = general_row_colour(i);
        list_labels.push(MenuLabel {
            text: (*caption).to_string(),
            origin: Origin::ScreenTop,
            dx: -COLUMN_HALF_W + NAME_LEFT_INSET,
            dy: y,
            align: Align::Left,
            colour,
            scale: 1.0,
        });
        list_labels.push(MenuLabel {
            text: value.clone(),
            origin: Origin::ScreenTop,
            dx: COLUMN_HALF_W - VALUE_RIGHT_MARGIN,
            dy: y,
            align: Align::Right,
            colour,
            scale: 1.0,
        });
    }

    let mut rows = vec![MenuRow {
        label: "Done".to_string(),
        enabled: true,
        slot: Some(Slot {
            origin: Origin::Settings(Placement::Footer { index: 0, count: 1 }),
            dx: 0.0,
            dy: 0.0,
            w: options::SMALL_BUTTON_WIDTH,
            h: options::WIDGET_H,
        }),
        ..Default::default()
    }];
    // One [`MenuRow`] per [`TAB_LABELS`] entry rather than a `MenuLabel` each —
    // see [`tab_row_rect`]
    // for why `slot` cannot express its geometry. Only General is `enabled`:
    // Only General is enabled; Items and Mobs have no projected rows, as
    // described in the module docs.
    rows.extend(TAB_LABELS.iter().enumerate().map(|(index, &label)| MenuRow {
        label: label.to_string(),
        enabled: index == GENERAL_TAB,
        tab: Some(TabEntryView {
            index,
            count: TAB_LABELS.len(),
            selected: index == GENERAL_TAB,
        }),
        ..Default::default()
    }));

    MenuFrame {
        rows,
        // `usize::MAX` is `MenuFrame::selected`'s documented "highlights
        // nothing" sentinel, not an out-of-range accident.
        selected: if nav.focused() { DONE_ROW } else { usize::MAX },
        // Carry the explicit Done hover state so the renderer can outline the
        // button when the cursor is over it.
        hovered: nav.hovered(),
        vanilla: true,
        // No `labels`: the tab bar is the complete header.
        list_labels,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- StatFormat, against known reference arithmetic (not just zero) -----

    #[test]
    fn default_format_groups_thousands_like_vanillas_number_format() {
        assert_eq!(StatFormat::Default.format(0), "0");
        assert_eq!(StatFormat::Default.format(999), "999");
        assert_eq!(StatFormat::Default.format(1000), "1,000");
        assert_eq!(StatFormat::Default.format(1_234_567), "1,234,567");
    }

    #[test]
    fn divide_by_ten_always_shows_two_decimals() {
        // Half-damage-point precision: 47 tenths of a heart is 4.70.
        assert_eq!(StatFormat::DivideByTen.format(0), "0.00");
        assert_eq!(StatFormat::DivideByTen.format(47), "4.70");
        assert_eq!(StatFormat::DivideByTen.format(100), "10.00");
    }

    #[test]
    fn distance_picks_cm_m_or_km_at_the_half_unit_threshold() {
        assert_eq!(StatFormat::Distance.format(0), "0 cm");
        assert_eq!(StatFormat::Distance.format(49), "49 cm", "under 0.5 m stays cm");
        assert_eq!(StatFormat::Distance.format(51), "0.51 m", "over 0.5 m promotes");
        assert_eq!(StatFormat::Distance.format(1000), "10.00 m");
        // 0.5 km exactly does not promote (`> 0.5`, not `>=`).
        assert_eq!(StatFormat::Distance.format(50_000), "500.00 m");
        assert_eq!(StatFormat::Distance.format(60_000), "0.60 km");
    }

    #[test]
    fn time_walks_minutes_hours_days_years_at_the_half_unit_threshold() {
        // 20 redstone ticks per second.
        assert_eq!(
            StatFormat::Time.format(0),
            "0.0 s",
            "Java's Double.toString always shows a fractional digit"
        );
        assert_eq!(StatFormat::Time.format(20 * 29), "29.0 s", "under 0.5 min stays seconds");
        assert_eq!(StatFormat::Time.format(20 * 31), "0.52 min", "over 0.5 min promotes");
        assert_eq!(StatFormat::Time.format(20 * 60 * 60), "1.00 h");
        assert_eq!(StatFormat::Time.format(20 * 60 * 60 * 13), "0.54 d");
        // One redstone tick exercises the fractional branch; no `.0` suffix is
        // needed when the value is already fractional.
        assert_eq!(StatFormat::Time.format(1), "0.05 s");
    }

    // -- the census -----------------------------------------------------------

    #[test]
    fn the_general_census_is_stats_javas_own_seventy_seven() {
        assert_eq!(GENERAL_STATS.len(), 77);
        let mut ids: Vec<&str> = GENERAL_STATS.iter().map(|&(id, _, _)| id).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "no id listed twice");
    }

    #[test]
    fn general_rows_are_sorted_by_caption_not_declaration_order() {
        let snapshot = StatsSnapshot::default();
        let rows = general_rows(&snapshot);
        assert_eq!(rows.len(), 77);
        let mut sorted = rows.clone();
        sorted.sort_by_key(|(c, _)| *c);
        assert_eq!(rows, sorted, "must already be in caption order");
        // The control: `GENERAL_STATS`'s own declaration order is not
        // alphabetical (it starts "Games Quit", "Time Played", …), so this
        // assertion is not vacuously true of any order.
        let declared: Vec<&str> = GENERAL_STATS.iter().map(|&(_, c, _)| c).collect();
        assert_ne!(declared, sorted.iter().map(|(c, _)| *c).collect::<Vec<_>>());
    }

    #[test]
    fn a_populated_snapshot_reaches_the_row_the_same_way_a_decoder_eventually_will() {
        // Proves the plumbing end to end with a synthetic value. Use a
        // hand-computed expected value from outside the code under test,
        // rather than relying on a round trip through the same projection.
        let mut snapshot = StatsSnapshot::default();
        snapshot.set("jump", 42);
        let rows = general_rows(&snapshot);
        let jump_row = rows.iter().find(|(c, _)| *c == "Jumps").unwrap();
        assert_eq!(jump_row.1, "42");
    }

    /// The projection reads the folded session store that supplies the frame.
    /// The load-bearing part is the **key shape**: the screen's ids
    /// are bare paths, and the wire key is
    /// `minecraft:custom` / `minecraft:<path>`. Writing the category as the *tab*
    /// name, or leaving the value's namespace off, misses every lookup — and a
    /// total miss is indistinguishable from "the server awarded nothing", which
    /// is the state an empty session produces.
    #[test]
    fn the_projection_reads_the_custom_category_and_the_namespaced_value() {
        use lodestone_game::progress::{StatKey, Statistics};
        use std::str::FromStr as _;

        let key = |ns: &str, path: &str| {
            StatKey::new(
                lodestone_model::Identifier::from_str(ns).unwrap(),
                lodestone_model::Identifier::from_str(path).unwrap(),
            )
        };

        let mut stats = Statistics::new();
        stats.set(key("minecraft:custom", "minecraft:jump"), 42);
        // A stat outside the General tab's 77 — a mined block — which must be
        // ignored rather than mis-projected onto some row.
        stats.set(key("minecraft:mined", "minecraft:stone"), 999);

        let snapshot = StatsSnapshot::from_statistics(&stats);
        assert_eq!(snapshot.get("jump"), 42);
        let rows = general_rows(&snapshot);
        assert_eq!(rows.iter().find(|(c, _)| *c == "Jumps").unwrap().1, "42");
        // …and nothing else moved off zero, so the `minecraft:mined` entry did
        // not leak into a row.
        let non_zero: Vec<&str> = GENERAL_STATS
            .iter()
            .filter(|&&(id, _, _)| snapshot.get(id) != 0)
            .map(|&(id, _, _)| id)
            .collect();
        assert_eq!(non_zero, vec!["jump"]);

        // The wrong-hypothesis control: the plausible key shapes both miss, so
        // the assertion above is measuring the key and not just "any lookup".
        let mut wrong = Statistics::new();
        wrong.set(key("minecraft:custom", "minecraft:jump"), 0);
        wrong.set(key("minecraft:general", "minecraft:jump"), 42);
        wrong.set(key("minecraft:custom", "minecraft:jumps"), 42);
        assert_eq!(
            StatsSnapshot::from_statistics(&wrong).get("jump"),
            0,
            "a wrong category or a wrong value path must not resolve"
        );
    }

    // -- the frame --------------------------------------------------------

    #[test]
    fn the_frame_has_no_title_label_a_done_button_and_every_row_scrolled_into_view() {
        let mut nav = StatsNav::default();
        let snapshot = StatsSnapshot::default();
        let f = frame(&nav, &snapshot);
        assert_eq!(f.rows.len(), 1 + TAB_LABELS.len(), "Done plus three tabs");
        assert_eq!(f.rows[DONE_ROW].label, "Done");
        assert!(f.rows[DONE_ROW].enabled);
        assert!(f.rows[DONE_ROW].tab.is_none(), "Done is not a tab row");
        // No separate "Statistics" heading; the tab bar is the complete header.
        assert!(
            !f.labels.iter().any(|l| l.text == TITLE),
            "vanilla draws no separate title label on this screen"
        );

        // Every row is emitted; the last alphabetical row is present at rest,
        // while the list model determines whether it is visible.
        let last_caption = general_rows(&snapshot).last().unwrap().0;
        assert!(
            f.list_labels.iter().any(|l| l.text == last_caption),
            "every row must be emitted into list_labels, visible or not"
        );
        assert!(
            !f.labels.iter().any(|l| l.text == last_caption),
            "list rows belong in list_labels, which is the clipped vector — \
             leaving them in `labels` paints them over the footer"
        );

        // Scrolling to the end must bring it inside the band.
        for _ in 0..GENERAL_STATS.len() {
            nav.step(true);
        }
        let f = frame(&nav, &snapshot);
        let list = list_spec(GENERAL_STATS.len(), nav.scroll())
            .model(crate::config::MIN_SCALED_HEIGHT as f32)
            .expect("the stats list scrolls");
        assert!(
            list.row_visible(GENERAL_STATS.len() - 1),
            "scrolling to the end must bring the last row into the band"
        );
        assert!(
            f.list_labels.iter().any(|l| l.text == last_caption),
            "the last row must still be emitted"
        );
    }

    // -- the tab widget -----------------------------------------

    #[test]
    fn the_frame_carries_three_real_tab_rows_and_only_general_is_live() {
        let nav = StatsNav::default();
        let snapshot = StatsSnapshot::default();
        let f = frame(&nav, &snapshot);
        for (index, &label) in TAB_LABELS.iter().enumerate() {
            let row = &f.rows[1 + index];
            assert_eq!(row.label, label);
            let view = row.tab.expect("a tab-bar row must carry a TabEntryView");
            assert_eq!(view.index, index);
            assert_eq!(view.count, TAB_LABELS.len(), "{label} carries this bar's own tab count");
            let live = index == GENERAL_TAB;
            assert_eq!(row.enabled, live, "{label} active state");
            assert_eq!(view.selected, live, "{label} selected state");
        }
    }

    // -- Done's hover outline -------------------------------------------------

    /// Hovering Done is carried into the frame so the renderer can draw its
    /// outline; tab-row hover is derived from the frame cursor instead.
    #[test]
    fn hovering_done_reaches_the_frame() {
        let mut nav = StatsNav::default();
        assert_eq!(nav.hovered(), None);
        assert_eq!(frame(&nav, &StatsSnapshot::default()).hovered, None);

        nav.hover_row(DONE_ROW);
        assert_eq!(nav.hovered(), Some(DONE_ROW));
        assert_eq!(
            frame(&nav, &StatsSnapshot::default()).hovered,
            Some(DONE_ROW),
            "MenuFrame::hovered must carry Done so render::draw_widget outlines it"
        );
    }

    /// The control: this screen has nothing else `hover_row` can highlight —
    /// a tab-bar row's hover comes from `MenuFrame::cursor` at draw time (see
    /// `hover_row`'s own doc), not from this bookkeeping.
    #[test]
    fn hovering_a_non_done_row_records_nothing() {
        let mut nav = StatsNav::default();
        for row in 1..=TAB_LABELS.len() {
            nav.hover_row(row);
            assert_eq!(nav.hovered(), None, "row {row} must not record hover");
        }
    }

    #[test]
    fn reset_clears_hover_too() {
        let mut nav = StatsNav::default();
        nav.hover_row(DONE_ROW);
        assert_eq!(nav.hovered(), Some(DONE_ROW));
        nav.reset();
        assert_eq!(nav.hovered(), None);
    }

    #[test]
    fn tab_rows_lay_out_left_to_right_with_no_overlap_and_within_the_canvas() {
        let width = 854.0;
        let mut prev_right = 0.0f32;
        for index in 0..TAB_LABELS.len() {
            let (x, y, w, h) = tab_row_rect(index, width);
            assert_eq!(y, 0.0, "the tab bar sits at the very top");
            assert_eq!(h, layout::TAB_BAR_HEIGHT);
            assert!(x >= prev_right, "tab {index} at x={x} overlaps its neighbour");
            assert!(x + w <= width, "tab {index} overruns the canvas");
            prev_right = x + w;
        }
    }

    #[test]
    fn general_row_colour_alternates_and_the_two_shades_are_vanillas_own_argb() {
        // Expected values originate outside this function: `-1`/`-4539718` are
        // the measured ARGB row colours, unpacked by shared `argb_to_rgba`.
        assert_eq!(general_row_colour(0), widget::argb_to_rgba(-1));
        assert_eq!(general_row_colour(1), widget::argb_to_rgba(-4_539_718));
        assert_eq!(general_row_colour(2), widget::argb_to_rgba(-1), "alternates back");
        // The discriminating control: the two shades must actually differ, and
        // an all-white implementation must not pass —
        // exercised directly on the frame with a discriminating (odd) row.
        assert_ne!(general_row_colour(0), general_row_colour(1));
        let snapshot = StatsSnapshot::default();
        let f = frame(&StatsNav::default(), &snapshot);
        let rows = general_rows(&snapshot);
        // Row 1 (odd) is the discriminating input — row 0 passes under a
        // solid-white implementation too, so asserting only the first row
        // would not distinguish the two row colours.
        let (odd_caption, _) = rows[1];
        let odd_label = f
            .list_labels
            .iter()
            .find(|l| l.text == odd_caption)
            .expect("row 1's caption must be emitted");
        assert_eq!(odd_label.colour, widget::argb_to_rgba(-4_539_718));
        assert_ne!(
            odd_label.colour,
            widget::ACTIVE_LABEL,
            "an odd row must not be plain white — that is the bug this gate catches"
        );
    }

    /// **The magnitude assertion, not the sign.** "It scrolled" is satisfied by
    /// a snap-to-row implementation, so the predicted value is computed from
    /// this screen's own
    /// `ROW_H` and the rival hypotheses are named and excluded.
    #[test]
    fn one_notch_is_half_a_row_in_pixels_and_lands_on_no_row_top() {
        let canvas = crate::config::MIN_SCALED_HEIGHT as f32;
        let mut nav = StatsNav::default();
        nav.scroll_by(-1.0, canvas);

        // `scrollRate` is `defaultEntryHeight / 2` under *integer* division
        //: 14 / 2 = 7.
        let predicted = (ROW_H / 2.0).floor();
        assert_eq!(predicted, 7.0, "derived from this screen's own ROW_H of 14");
        assert_eq!(nav.scroll(), predicted, "one notch must be {predicted} px");
        assert_ne!(nav.scroll(), ROW_H, "the row-index model's answer is excluded");
        assert_ne!(
            nav.scroll(),
            LIST_WINDOW_PX,
            "a page-sized notch is excluded"
        );

        // And the offset coincides with no row top — the property a row index
        // structurally cannot have. Derived from `row_label_y`, the same
        // expression the draw places rows by, not from a restated constant.
        let at_rest = band_top();
        assert!(
            (0..GENERAL_STATS.len()).all(|i| row_label_y(i as u16, nav.scroll()) != at_rest),
            "offset {} coincides with a row top, so it is indistinguishable from a jump",
            nav.scroll()
        );

        // Three notches must land somewhere that is not a whole number of rows.
        let mut three = StatsNav::default();
        three.scroll_by(-3.0, canvas);
        assert_eq!(three.scroll(), 21.0, "3 * predicted (7 px)");
        assert_ne!(
            three.scroll() % ROW_H,
            0.0,
            "21 px must not be expressible as whole rows, or this gate has stopped \
             discriminating against the row-index model"
        );
    }

    #[test]
    fn scrolling_never_goes_negative_or_past_the_last_window() {
        let canvas = crate::config::MIN_SCALED_HEIGHT as f32;
        let mut nav = StatsNav::default();
        nav.step(false);
        assert_eq!(nav.scroll(), 0.0, "cannot scroll above the top");
        nav.scroll_by(5.0, canvas);
        assert_eq!(nav.scroll(), 0.0, "the wheel cannot go negative either");

        for _ in 0..1000 {
            nav.step(true);
        }
        // Compute the clamp independently as content height minus band height,
        // rather than reading it back from the navigation model.
        let content = GENERAL_STATS.len() as f32 * ROW_H + 2.0 * widget::LIST_CONTENT_PADDING;
        let band = canvas - options::FOOTER_HEIGHT - HEADER_HEIGHT;
        assert_eq!(
            nav.scroll(),
            content - band,
            "the clamp must be maxScrollAmount(), not a row count"
        );
        let list = list_spec(GENERAL_STATS.len(), nav.scroll())
            .model(canvas)
            .expect("scrollable");
        assert!(
            list.row_visible(GENERAL_STATS.len() - 1),
            "must be able to see the last row"
        );
    }

    #[test]
    fn reset_returns_to_the_top() {
        let mut nav = StatsNav::default();
        nav.step(true);
        nav.step(true);
        assert!(nav.scroll() > 0.0);
        nav.reset();
        assert_eq!(nav.scroll(), 0.0);
    }
}
