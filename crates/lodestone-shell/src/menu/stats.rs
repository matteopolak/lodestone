//! The Statistics screen (issue #188), reached from the pause menu's
//! Statistics button — vanilla's `StatsScreen`.
//!
//! ## What is and is not built
//!
//! Vanilla's screen has three tabs: **General** (`Stats.CUSTOM`, 77 fixed
//! stats — `StatsScreen.GeneralStatisticsList`), **Items**
//! (`ContainerObjectSelectionList` over every `Item` with a non-zero
//! mined/crafted/used/picked-up/dropped count), **Mobs** (same shape, over
//! `EntityType`). Only **General** is built here as a real scrollable list.
//! Items and Mobs are present-and-inactive tab buttons, for a reason that is
//! not a scope cut so much as already-correct behaviour:
//! `StatsScreen.setTabActiveStateAndTooltip` (`:124-133`) disables a tab
//! itself, with a `"gui.stats.none_found"` tooltip, whenever its list is
//! empty (`!statsTab.list.children().isEmpty()`) — and see the next section
//! for why every Items/Mobs row is unconditionally empty here. So the
//! disabled state these two tabs show is exactly the state vanilla's own
//! screen would show given the same (zero) underlying data, not an
//! approximation of it.
//!
//! ## Where the numbers come from — **this section used to say they were all zero**
//!
//! It was true when written: nothing decoded `award_stats`, so
//! `StatsSnapshot::default()` was not a placeholder standing in for real data,
//! it was *the* data. That changed, and the moment it did the empty literal
//! `menu::render::dispatch` passed in became an island — the counters arrived in
//! `lodestone_ecs::SessionStatistics` and this screen kept drawing zeros.
//!
//! The live path now:
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
//! **The Items and Mobs tabs are still present-and-inactive**, and that part is
//! unchanged: they need per-block and per-entity id tables this screen's flat
//! 77-row model does not have, so a `minecraft:mined` counter is deliberately
//! dropped by the projection rather than squeezed onto a General row.
//!
//! Consequently Items and Mobs are always empty too: both are filtered to
//! non-zero counts (`ItemStatisticsList`'s own constructor loop, `:0` skips
//! anything with a zero count in every one of its five columns), and with no
//! stat ever above zero, an empty list is the *correct* output of that
//! filter — not a stand-in for one this module never wrote.
//!
//! ## Wired vs. decorative
//!
//! - **Wired**: reaching the screen from the pause menu and back
//!   (Escape/Done), the General tab's real vanilla structure (77 stats,
//!   vanilla's own captions, vanilla's own three format rules — `DEFAULT`,
//!   `DIVIDE_BY_TEN`, `DISTANCE`, `TIME`, transcribed from
//!   `StatFormatter.java` and tested against known non-zero inputs, not only
//!   the trivial zero case), and the census (`GENERAL_STATS.len() == 77`,
//!   matching `Stats.java`'s own count of `makeCustomStat` calls).
//! - **Decorative**: every value shown, because nothing decodes the packet
//!   that would populate one — see above. Enabling the pause button
//!   ([`super::nav::PauseButton::Statistics`]) reflects that this screen now
//!   exists and shows the honest (zero) state, per issue #188's own scope,
//!   which asks for exactly that once the screen exists.
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

/// `StatsScreen.GENERAL_BUTTON`/`ITEMS_BUTTON`/`MOBS_BUTTON` — `stat.
/// generalButton`/`stat.itemsButton`/`stat.mobsButton`, verbatim from
/// `en_us.json`. This screen's tab bar (issue #564).
pub const TAB_LABELS: [&str; 3] = ["General", "Items", "Mobs"];
/// [`TAB_LABELS`]'s index of the only tab this screen builds a real list for
/// (see the module docs above) — General.
pub const GENERAL_TAB: usize = 0;

/// The pixel rect of tab `index` (into [`TAB_LABELS`]) at canvas `width` —
/// vanilla's `MenuTabBar.arrangeElements` (`MenuTabBar.java`), via the shared
/// [`layout::tab_bar_row_rect`] (issue #567's generalisation: this screen's own
/// wrapper used to inline the arithmetic directly, which is what
/// [`super::render::row_rect`]'s `MenuRow::tab` arm called into by name —
/// harmless while Statistics was the only consumer, and a hard-coded
/// dependency the moment Create New World became a second one). See
/// [`super::render::TabEntryView::index`]'s own doc on why a `Slot` cannot
/// express this row's *width*, let alone its `x`.
#[must_use]
pub fn tab_row_rect(index: usize, width: f32) -> (f32, f32, f32, f32) {
    layout::tab_bar_row_rect(index, TAB_LABELS.len(), width)
}

/// The row index of Done — vanilla's `layout.addToFooter(Button.builder(
/// GUI_DONE, …))` (`StatsScreen.java`). Was this screen's only [`MenuRow`]
/// before issue #564 gave it three tab rows too; still `0` and still first,
/// since the tabs are appended after it (see [`frame`]).
///
/// Named rather than written as a bare `0` at three sites, because "row 0" and
/// "the Done button" being the same number is exactly what made the focus bug
/// in [`StatsNav::focused`] read as harmless.
pub const DONE_ROW: usize = 0;

/// `StatFormatter.java`'s four formatters, transcribed. `DEFAULT` is vanilla's
/// `NumberFormat.getIntegerInstance(Locale.US)` — thousands-grouped, no
/// decimals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatFormat {
    Default,
    /// `DECIMAL_FORMAT.format(value * 0.1)` — always two decimal places, no
    /// thousands grouping (`"########0.00"`).
    DivideByTen,
    /// Centimetres in, `km`/`m`/`cm` out — `StatFormatter.DISTANCE`.
    Distance,
    /// Redstone ticks (1/20 s) in, the largest whole unit over 0.5 out —
    /// `StatFormatter.TIME`. The `< 0.5 min` branch is `seconds + " s"` on a
    /// raw `f64`, **not** through `DECIMAL_FORMAT` — vanilla's own code, not
    /// an inconsistency introduced here (`StatFormatter.java`).
    Time,
}

impl StatFormat {
    /// `DECIMAL_FORMAT`'s pattern (`"########0.00"`): always exactly two
    /// decimal places, no thousands grouping, at least one digit before the
    /// point.
    fn decimal(value: f64) -> String {
        format!("{value:.2}")
    }

    /// `NumberFormat.getIntegerInstance(Locale.US)`: thousands-grouped,
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

    /// Formats a raw stat value exactly as vanilla's `Stat::format` would.
    #[must_use]
    pub fn format(self, value: i32) -> String {
        match self {
            StatFormat::Default => Self::grouped(i64::from(value)),
            StatFormat::DivideByTen => Self::decimal(f64::from(value) * 0.1),
            StatFormat::Distance => {
                // `StatFormatter.DISTANCE`, `cm` is `int` in vanilla too.
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
                    // Vanilla's own inconsistency, preserved: a raw `f64`
                    // through Java's default `Double` `toString`-via-`+`,
                    // not `DECIMAL_FORMAT` — see `StatFormat::Time`'s doc.
                    format!("{} s", java_double_to_string(seconds))
                }
            }
        }
    }
}

/// Java's `Double.toString` for the one place this module needs it
/// (`StatFormatter.TIME`'s `< 0.5 min` branch, `seconds + " s"` where
/// `seconds` is a raw `double`): unlike Rust's own `f64` `Display`, Java
/// always shows at least one digit after the point — `29.0`, never `29`.
/// Every value this is called with is an exact multiple of `1.0 / 20.0`
/// (redstone ticks), so the fractional part (when there is one) is a simple
/// terminating decimal and Rust's own shortest round-trip formatting already
/// agrees with Java's there; the only correction needed is forcing the `.0`
/// suffix Rust omits for a whole number.
fn java_double_to_string(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// `Stats.java`'s 77 `makeCustomStat` calls, in declaration order — id, the
/// verbatim `en_us.json` caption at `stat.minecraft.<id>`, and its
/// [`StatFormat`]. Declaration order does not matter for display (vanilla
/// sorts by *translated* caption, `StatsScreen.java`,
/// `Comparator.comparing(k -> I18n.get(...))`) — see [`general_rows`], which
/// sorts at call time instead of requiring this table pre-sorted, so adding a
/// stat here never has to also get its alphabetical position right.
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
/// Vanilla's General tab is `Stats.CUSTOM` — a `StatType` whose registry is
/// `minecraft:custom_stat` — so `"sleep_in_bed"` on the screen is the wire's
/// `StatKey { category: "minecraft:custom", value: "minecraft:sleep_in_bed" }`.
/// The category id is the **stat type's** registry name, not the tab's.
const CUSTOM_STAT_CATEGORY: &str = "minecraft:custom";

/// The live values behind [`GENERAL_STATS`]. A sparse map (not a `[i32; 77]`)
/// so only the ids the server has actually awarded are stored, exactly
/// mirroring vanilla's own `StatsCounter`, which is sparse for the same reason.
///
/// **This is no longer always empty.** `award_stats` is decoded and folded into
/// `lodestone_ecs::SessionStatistics`, and
/// [`from_statistics`](Self::from_statistics) is the projection onto this
/// screen's fixed 77 ids; `app::session`'s per-frame reconciliation pushes it in
/// through `MenuNav::refresh_stats`. The module docs above describe the state
/// before that and are corrected there.
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

/// One row: caption plus formatted value, in vanilla's **display** order —
/// sorted by the translated caption (`StatsScreen.java`), not by
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

/// `GeneralStatisticsList`'s own `itemHeight` (`StatsScreen.java`:
/// `super(minecraft, StatsScreen.this.width, StatsScreen.this.layout.
/// getContentHeight(), 33, 14)` — the last constructor argument). **Not**
/// [`options::WIDGET_H`] (20 px) — that was this constant's previous value,
/// reused from every other non-`OptionsList` list in this tree on the
/// assumption that this screen had no real vanilla row height to port; it
/// does, and it is smaller, which issue #564 named directly.
///
/// [`ITEMS_ROW_H`]/[`MOBS_ROW_H`] are the other two tabs' own heights, ported
/// alongside this one even though neither tab has a real list yet (see the
/// module docs) — so a future `ItemStatisticsList`/`MobsStatisticsList`
/// conversion has the right constant already sitting here rather than a
/// second archaeology pass through the jar.
pub const ROW_H: f32 = 14.0;
/// `ItemStatisticsList`'s own `itemHeight` (`StatsScreen.java`: `super(…,
/// 33, 22)`). Not yet consumed — see [`ROW_H`]'s own doc.
pub const ITEMS_ROW_H: f32 = 22.0;
/// `MobsStatisticsList`'s own `itemHeight` (`StatsScreen.java`: `super(…,
/// 33, 9 * 4)` — four lines of the 9 px font, ported as the expression rather
/// than the literal `36` so a font-size change would not silently desync it).
/// Not yet consumed — see [`ROW_H`]'s own doc.
pub const MOBS_ROW_H: f32 = 9.0 * 4.0;
/// Half the list's column width — the name column runs from
/// [`Origin::ScreenTop`]`- COLUMN_HALF_W + NAME_LEFT_INSET` to centre, the
/// value column from centre to `+ COLUMN_HALF_W - VALUE_RIGHT_MARGIN`.
const COLUMN_HALF_W: f32 = 150.0;
const VALUE_RIGHT_MARGIN: f32 = 10.0;
const NAME_LEFT_INSET: f32 = 4.0;

/// This screen's header height (issue #564) — **not**
/// [`options::SUB_HEADER_HEIGHT`], which is `HeaderAndFooterLayout`'s default
/// 33 px title band and is not what this screen uses. `StatsScreen.
/// repositionElements` calls `this.layout.setHeaderHeight(tabAreaTop)`, where
/// `tabAreaTop` is the tab bar's own `getRectangle().bottom()` — a fixed
/// [`layout::TAB_BAR_HEIGHT`] (24 px), since `MenuTabBar` is `y = 0` height
/// `HEIGHT = 24` (`MenuTabBar.java`). Using the *default* 33 px header
/// here is exactly what put the General list's own top separator 9 px below
/// where the tab row's underline sits — close enough to look plausible, far
/// enough to still collide with a tab label drawn at `dy: 28`, which was the
/// owner's reported symptom.
pub const HEADER_HEIGHT: f32 = layout::TAB_BAR_HEIGHT;

pub const LIST_WINDOW_PX: f32 =
    crate::config::MIN_SCALED_HEIGHT as f32 - HEADER_HEIGHT - options::FOOTER_HEIGHT - options::LIST_TOP_INSET;

/// Top of the list band — the y a row at scroll `0.0` starts at.
#[must_use]
pub fn band_top() -> f32 {
    HEADER_HEIGHT + options::LIST_TOP_INSET
}

/// This screen's [`widget::ListSpec`] (issue #445), the one declaration the
/// scrollbar, the wheel and the row placement all read.
///
/// `top` is [`HEADER_HEIGHT`] rather than [`band_top`]: the spec's band is the
/// *window*, and [`widget::ScrollList`] adds [`widget::LIST_CONTENT_PADDING`]
/// itself as `first_entry_y`. Passing the already-inset value would inset
/// twice, which is the one arithmetic slip this conversion can make and still
/// look right at scroll zero.
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

/// This screen has no per-row control (a stat row is not clickable — vanilla
/// itself only narrates it), so there is nothing to place beyond the row's
/// own text: every row is a pair of [`super::render::MenuLabel`]s at this y,
/// not a [`super::render::MenuRow`]/[`Slot`].
///
/// **The offset is pixels** (issue #445). This used to be
/// `band_top + (row - first) * ROW_H` against a `first: usize` row index,
/// which structurally could not express a half-scrolled row. `scroll.floor()`
/// matches [`widget::ScrollList::row_top`]'s single `(int)` truncation —
/// vanilla truncates the offset once, not per entry.
#[must_use]
pub fn row_label_y(row: u16, scroll: f32) -> f32 {
    band_top() - scroll.floor() + f32::from(row) * ROW_H
}

/// `GeneralStatisticsList.Entry.extractContent`'s zebra striping
/// (`StatsScreen.java`): `index % 2 == 0 ? -1 : -4539718` — opaque white
/// on an even displayed row, `0xFFBABABA` on an odd one. `index` is the row's
/// position in the **already-sorted** list ([`general_rows`]'s output order),
/// matching vanilla's `children().indexOf(this)`.
#[must_use]
pub fn general_row_colour(index: usize) -> [f32; 4] {
    if index % 2 == 0 {
        widget::argb_to_rgba(-1)
    } else {
        widget::argb_to_rgba(-4_539_718)
    }
}

/// This screen's own scroll cursor. No selection/activation at all on the
/// General list (vanilla's own rows are not buttons); only Done is a real
/// control.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StatsNav {
    /// Scroll offset in **pixels** (issue #445), not a row index.
    ///
    /// `Eq` came off this derive when the field changed type, and that is the
    /// point rather than a cost: a row-index offset is always a multiple of
    /// [`ROW_H`], which is precisely the snap-to-row behaviour the wheel work
    /// exists to remove. See [`widget::ListSpec`]'s own doc.
    scroll: f32,
    /// Whether Done — the screen's only control — currently holds keyboard
    /// focus. **`false` on open, and that is the whole of a player report**
    /// (2026-08-04, "the Statistics menu always has the 'Done' button focused
    /// for some reason").
    ///
    /// [`frame`] used to hard-code `selected: 0` on a frame whose only row *is*
    /// Done, so the button was drawn focused the instant the screen appeared.
    /// Vanilla focuses nothing here, and the jar is unusually explicit about
    /// it in two independent ways:
    ///
    /// - `Screen.init` calls `setInitialFocus()` (`Screen.java`), whose
    ///   base implementation (`:161-169`) is wrapped entirely in
    ///   `if (this.minecraft.getLastInputType().isKeyboard())`. This screen is
    ///   reached by **clicking** the pause menu's Statistics button, so the
    ///   last input type is a mouse and the whole body is skipped — nothing is
    ///   focused at all. `StatsScreen` does not override `setInitialFocus`
    ///   (grepped: it appears in eight screens, none of them this one).
    /// - Even opened from the keyboard, Done would still not be it. `StatsScreen.init`
    ///   (`:79-98`) adds the `MenuTabBar` **first** and then puts the footer's
    ///   Done in `setTabOrderGroup(1)`, which sorts it *after* every default-group
    ///   widget — so the first tab stop is the General tab, not Done.
    ///
    /// So a focused Done is wrong under both input types, which is why this is
    /// a plain `false` default rather than a modelled `lastInputType` (nothing
    /// in this shell tracks one, and no reachable path would make it keyboard).
    /// Tab is what grants focus — see [`Self::focus_next`] — and a click grants
    /// it too, because `ContainerEventHandler.mouseClicked` focuses the child it
    /// hit before calling its `onClick`.
    ///
    /// Note this is the same shape as, but a *different mechanism* from, the
    /// earlier "hovering should not focus it" report on the server list: that
    /// one was hover writing into selection, and was fixed by splitting
    /// `hovered` from `selected`. This one is the initial value of the
    /// selection itself, which that split did not touch — so the two had to be
    /// found separately.
    focused: bool,
    /// Whether the mouse is over Done — the same "no `Screen::Statistics` arm
    /// in `MenuNav::hover`" gap issue #567 found and fixed on Create New
    /// World, found here while auditing this screen for the same defect
    /// shape: [`frame`] never set `MenuFrame::hovered`, so Done never drew a
    /// hover outline regardless of where the mouse was. The tab bar itself
    /// needs no equivalent field — see [`hover_row`]'s own doc, the same
    /// reasoning `create_world.rs`'s `CreateWorldNav::hover_row` documents:
    /// a `MenuRow::tab` row derives its hover straight from `MenuFrame::cursor`
    /// at draw time.
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

    /// Tab traversal: `Screen.keyPressed`'s `TabNavigation`
    /// (`Screen.java`), which on this screen has exactly one focusable
    /// child to land on. With one child, forward Tab focuses it and stays
    /// there — vanilla's wrap is `clearFocus()`-then-retry, which re-finds the
    /// same child — so this is idempotent rather than a toggle.
    pub fn focus_next(&mut self) {
        self.focused = true;
    }

    /// `ContainerEventHandler.mouseClicked`: a click on a widget focuses it
    /// *and then* activates it. Hover does not — see [`Self::focused`].
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
/// ## No "Statistics" title label — issue #564's second half
///
/// The owner: *"'Statistics' is [not] even supposed to be in the UI at all"*.
/// Vanilla's `TITLE` (`gui.stats`) is passed to `Screen`'s constructor for
/// narration only; nothing in `StatsScreen.extractRenderState`/
/// `extractMenuBackground` ever draws it — the header **is** the tab bar
/// (`extractMenuBackground` blits `CreateWorldScreen.TAB_HEADER_BACKGROUND`
/// behind it, then the content below), and there is no second heading above
/// that. This used to draw `TITLE` as a centred label at `dy: 12`, which the
/// tab row then drew straight through at `dy: 28`, 9 px below where the real
/// header ends at [`HEADER_HEIGHT`] (24) — close enough to look like a
/// heading, and exactly what put every tab label crossing the divider a
/// screen used to draw at `y = 33` (`options::SUB_HEADER_HEIGHT`, this
/// screen's *old*, wrong header height).
#[must_use]
pub fn frame(nav: &StatsNav, snapshot: &StatsSnapshot) -> MenuFrame<'static> {
    let stats = general_rows(snapshot);

    // **Every** row is emitted, not a `[first..end]` window (issue #445). The
    // slice was what made a partially-scrolled row impossible to express: a
    // row either fitted wholly or was absent. `list_labels` is clipped to the
    // band by `render::draw`, so a row straddling the bottom now paints its
    // visible half instead of vanishing.
    let scroll = nav.scroll();
    let mut list_labels = Vec::with_capacity(stats.len() * 2);
    for (i, (caption, value)) in stats.iter().enumerate() {
        let y = row_label_y(i as u16, scroll);
        // Zebra striping (issue #564) — `GeneralStatisticsList.Entry.
        // extractContent` (`StatsScreen.java`): both the name and the
        // value get the *same* `color`, computed once per row from the
        // row's own displayed index (this loop's `i`, matching vanilla's
        // `children().indexOf(this)` in the already-sorted list).
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
    // Vanilla's real tab widget (issue #564), one [`MenuRow`] per
    // [`TAB_LABELS`] entry rather than a `MenuLabel` each — see [`tab_row_rect`]
    // for why `slot` cannot express its geometry. Only General is `enabled`:
    // `StatsScreen.setTabActiveStateAndTooltip` disables a tab whose list is
    // empty (`:124-133`), and Items/Mobs are unconditionally empty here — see
    // the module docs on why that is already-correct behaviour, not a
    // shortcut.
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
        // nothing" sentinel (the same value `command_block`'s frame uses), not
        // an out-of-range accident. It used to be a hard `0`, i.e. Done, which
        // is the player report [`StatsNav::focused`] documents.
        selected: if nav.focused() { DONE_ROW } else { usize::MAX },
        // The same gap issue #567 found and fixed on Create New World: this
        // used to be left at its `..Default::default()` of `None`
        // unconditionally, so Done never drew a hover outline no matter where
        // the cursor was.
        hovered: nav.hovered(),
        vanilla: true,
        // No `labels` — this screen draws no separate heading; see this
        // function's own doc on why vanilla has none either.
        list_labels,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- StatFormat, against known vanilla arithmetic (not just zero) -------

    #[test]
    fn default_format_groups_thousands_like_vanillas_number_format() {
        assert_eq!(StatFormat::Default.format(0), "0");
        assert_eq!(StatFormat::Default.format(999), "999");
        assert_eq!(StatFormat::Default.format(1000), "1,000");
        assert_eq!(StatFormat::Default.format(1_234_567), "1,234,567");
    }

    #[test]
    fn divide_by_ten_always_shows_two_decimals() {
        // `DECIMAL_FORMAT.format(value * 0.1)` — half-damage-point precision,
        // e.g. 47 tenths of a heart is 4.70.
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
        // One redstone tick — the fractional branch of `java_double_to_string`,
        // where Rust's own shortest round-trip formatting already agrees
        // with Java's (no `.0`-forcing needed, unlike the whole-second case).
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
    fn a_fresh_snapshot_reads_zero_for_every_stat() {
        // The state every real session is in today — see the module docs.
        let snapshot = StatsSnapshot::default();
        for &(id, _, fmt) in GENERAL_STATS {
            assert_eq!(fmt.format(snapshot.get(id)), fmt.format(0), "{id} must default to zero");
        }
    }

    #[test]
    fn a_populated_snapshot_reaches_the_row_the_same_way_a_decoder_eventually_will() {
        // Proves the plumbing end to end with a synthetic value, standing in
        // for the decoder this issue does not build — an expected value from
        // outside the code under test (hand-computed, not round-tripped).
        let mut snapshot = StatsSnapshot::default();
        snapshot.set("jump", 42);
        let rows = general_rows(&snapshot);
        let jump_row = rows.iter().find(|(c, _)| *c == "Jumps").unwrap();
        assert_eq!(jump_row.1, "42");
    }

    /// The projection off the real folded store, which is what the screen now
    /// draws from. The load-bearing part is the **key shape**: the screen's ids
    /// are bare paths, and the wire key is
    /// `minecraft:custom` / `minecraft:<path>`. Writing the category as the *tab*
    /// name, or leaving the value's namespace off, misses every lookup — and a
    /// total miss is indistinguishable from "the server awarded nothing", which
    /// is exactly the state this screen was stuck in before.
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
        // No "Statistics" heading — vanilla draws none; see `frame`'s own doc.
        assert!(
            !f.labels.iter().any(|l| l.text == TITLE),
            "vanilla draws no separate title label on this screen"
        );

        // Every row is emitted now, not a window — the slice is what made a
        // half-scrolled row inexpressible. The last alphabetical row is present
        // at rest; whether it is *visible* is the primitive's question, asked
        // below.
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

    // -- the tab widget (issue #564) -----------------------------------------

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

    /// The same defect shape issue #567 found and fixed on Create New World:
    /// `frame` never set `MenuFrame::hovered`, so Done never drew its hover
    /// outline no matter where the cursor was. `hover_row` is `MenuNav::hover`'s
    /// `Screen::Statistics` arm, wired in `nav.rs`.
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
        // `StatsScreen.java`'s own literals, unpacked by the shared
        // `argb_to_rgba` rather than restated as a second pair of floats.
        assert_eq!(general_row_colour(0), widget::argb_to_rgba(-1));
        assert_eq!(general_row_colour(1), widget::argb_to_rgba(-4_539_718));
        assert_eq!(general_row_colour(2), widget::argb_to_rgba(-1), "alternates back");
        // The discriminating control: the two shades must actually differ, and
        // an all-white implementation (the pre-#564 behaviour) must not pass —
        // exercised directly on the frame with a discriminating (odd) row.
        assert_ne!(general_row_colour(0), general_row_colour(1));
        let snapshot = StatsSnapshot::default();
        let f = frame(&StatsNav::default(), &snapshot);
        let rows = general_rows(&snapshot);
        // Row 1 (odd) is the discriminating input — row 0 passes under a
        // solid-white implementation too, so asserting only the first row
        // would not have caught the pre-#564 bug.
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
    /// a snap-to-row implementation, which is exactly what this conversion
    /// removed — so the predicted value is computed from this screen's own
    /// `ROW_H` and the rival hypotheses are named and excluded.
    #[test]
    fn one_notch_is_half_a_row_in_pixels_and_lands_on_no_row_top() {
        let canvas = crate::config::MIN_SCALED_HEIGHT as f32;
        let mut nav = StatsNav::default();
        nav.scroll_by(-1.0, canvas);

        // `scrollRate` is `defaultEntryHeight / 2` under *integer* division
        // (`AbstractSelectionList.java`): 14 / 2 = 7.
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
        // The clamp is vanilla's `maxScrollAmount() = contentHeight() - height`,
        // computed here from the outside rather than read back off the nav.
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
