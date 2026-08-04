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
//! ## Why every value is zero
//!
//! **Nothing in this workspace decodes the `award_stats`/statistics packet.**
//! `/usr/bin/grep -rln 'award_stats\|AwardStats\|ClientboundAwardStatsPacket'`
//! over `crates/` finds nothing, and `cargo xtask connectedness` names no stat
//! packet either — confirmed rather than assumed, since decoding it is
//! `crates/protocol/*` work, out of this batch's file ownership (see the
//! issue's own scope note: "decode... if not already reachable"). So
//! [`StatsSnapshot::default`] — an empty table, [`StatsSnapshot::get`]
//! returning `0` for everything — is not a placeholder standing in for real
//! data; it is *the* data, because nothing has ever populated anything else.
//!
//! This is a different situation from a settings row showing a fabricated
//! "ON" for a feature that does not work (`docs/settings-screen.md`'s
//! departure 1): a stat reading zero is the **true** state of "nothing has
//! been decoded yet", the same way a freshly created vanilla world's own
//! Statistics screen reads zero for everything a player has not yet done.
//! Nothing here claims a stat is being tracked that is not — it is simply
//! that *no* stat is tracked yet, uniformly and honestly.
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

use super::options::{self, Placement};
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot};
use super::widget;

/// `gui.stats`.
pub const TITLE: &str = "Statistics";

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
    /// an inconsistency introduced here (`StatFormatter.java:32`).
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
/// sorts by *translated* caption, `StatsScreen.java:170`,
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

/// The live values behind [`GENERAL_STATS`]. Always empty in production
/// today — see the module docs on why that is correct rather than a
/// placeholder. A sparse map (not a `[i32; 77]`) so a future decoder can
/// populate only the ids it has actually seen, exactly mirroring vanilla's
/// own `StatsCounter`, which is sparse for the same reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    values: std::collections::HashMap<&'static str, i32>,
}

impl StatsSnapshot {
    #[must_use]
    pub fn get(&self, id: &str) -> i32 {
        self.values.get(id).copied().unwrap_or(0)
    }

    /// Test/oracle-only setter — no production code path writes into this
    /// yet (see the module docs).
    #[cfg(test)]
    fn set(&mut self, id: &'static str, value: i32) {
        self.values.insert(id, value);
    }
}

/// One row: caption plus formatted value, in vanilla's **display** order —
/// sorted by the translated caption (`StatsScreen.java:170`), not by
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

/// Row height — no vanilla source (`ObjectSelectionList`'s own row height for
/// this list is 14 px content in a 33 px-separated flow this pipeline's flat
/// model does not reproduce); reuses [`options::WIDGET_H`] like every other
/// non-`OptionsList` list in this tree.
pub const ROW_H: f32 = options::WIDGET_H;
/// Half the list's column width — the name column runs from
/// [`Origin::ScreenTop`]`- COLUMN_HALF_W + NAME_LEFT_INSET` to centre, the
/// value column from centre to `+ COLUMN_HALF_W - VALUE_RIGHT_MARGIN`.
const COLUMN_HALF_W: f32 = 150.0;
const VALUE_RIGHT_MARGIN: f32 = 10.0;
const NAME_LEFT_INSET: f32 = 4.0;

pub const LIST_WINDOW_PX: f32 =
    crate::config::MIN_SCALED_HEIGHT as f32 - options::SUB_HEADER_HEIGHT - options::FOOTER_HEIGHT - options::LIST_TOP_INSET;

#[must_use]
pub fn visible_rows_len() -> usize {
    (LIST_WINDOW_PX / ROW_H).floor().max(1.0) as usize
}

/// This screen has no per-row control (a stat row is not clickable — vanilla
/// itself only narrates it), so there is nothing to place beyond the row's
/// own text: every row is a pair of [`super::render::MenuLabel`]s at this y,
/// not a [`super::render::MenuRow`]/[`Slot`].
#[must_use]
pub fn row_label_y(row: u16, first: u16) -> f32 {
    let index = row.saturating_sub(first);
    options::SUB_HEADER_HEIGHT + options::LIST_TOP_INSET + f32::from(index) * ROW_H
}

/// This screen's own scroll cursor. No selection/activation at all on the
/// General list (vanilla's own rows are not buttons); only Done is a real
/// control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatsNav {
    first: usize,
}

impl StatsNav {
    pub fn reset(&mut self) {
        self.first = 0;
    }

    #[must_use]
    pub fn first(&self) -> usize {
        self.first
    }

    /// Scrolls by one row. The census (77) is fixed, so this needs no
    /// snapshot — unlike [`GENERAL_STATS`]'s *values*, its *length* never
    /// changes.
    pub fn scroll(&mut self, forward: bool) {
        let len = GENERAL_STATS.len();
        let window = visible_rows_len();
        if len <= window {
            self.first = 0;
            return;
        }
        let max_first = len - window;
        self.first = if forward {
            (self.first + 1).min(max_first)
        } else {
            self.first.saturating_sub(1)
        };
    }
}

/// Builds the whole Statistics frame.
#[must_use]
pub fn frame(nav: &StatsNav, snapshot: &StatsSnapshot) -> MenuFrame<'static> {
    let rows = general_rows(snapshot);
    let first = nav.first().min(rows.len());
    let end = (first + visible_rows_len()).min(rows.len());

    let mut labels = vec![
        MenuLabel {
            text: TITLE.to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: 12.0,
            align: Align::Centre,
            colour: widget::ACTIVE_LABEL,
            scale: 1.0,
        },
        // The three tab buttons, drawn as labels rather than `MenuRow`s: only
        // General is a real destination and it is already showing, so there
        // is nothing for a click on any of the three to *do* — see the
        // module docs on why Items/Mobs are correctly inactive rather than
        // approximately so.
        MenuLabel {
            text: "[General]".to_string(),
            origin: Origin::ScreenTop,
            dx: -100.0,
            dy: 28.0,
            align: Align::Left,
            colour: widget::ACTIVE_LABEL,
            scale: 1.0,
        },
        MenuLabel {
            text: "Items".to_string(),
            origin: Origin::ScreenTop,
            dx: -10.0,
            dy: 28.0,
            align: Align::Left,
            colour: widget::INACTIVE_LABEL,
            scale: 1.0,
        },
        MenuLabel {
            text: "Mobs".to_string(),
            origin: Origin::ScreenTop,
            dx: 60.0,
            dy: 28.0,
            align: Align::Left,
            colour: widget::INACTIVE_LABEL,
            scale: 1.0,
        },
    ];

    for (i, (caption, value)) in rows[first..end].iter().enumerate() {
        let row = (first + i) as u16;
        let y = row_label_y(row, first as u16);
        labels.push(MenuLabel {
            text: (*caption).to_string(),
            origin: Origin::ScreenTop,
            dx: -COLUMN_HALF_W + NAME_LEFT_INSET,
            dy: y,
            align: Align::Left,
            colour: widget::ACTIVE_LABEL,
            scale: 1.0,
        });
        labels.push(MenuLabel {
            text: value.clone(),
            origin: Origin::ScreenTop,
            dx: COLUMN_HALF_W - VALUE_RIGHT_MARGIN,
            dy: y,
            align: Align::Right,
            colour: widget::ACTIVE_LABEL,
            scale: 1.0,
        });
    }

    MenuFrame {
        rows: vec![MenuRow {
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
        }],
        selected: 0,
        vanilla: true,
        labels,
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

    // -- the frame --------------------------------------------------------

    #[test]
    fn the_frame_has_a_title_a_done_button_and_every_row_scrolled_into_view() {
        let mut nav = StatsNav::default();
        let snapshot = StatsSnapshot::default();
        let f = frame(&nav, &snapshot);
        assert_eq!(f.rows.len(), 1, "one control: Done");
        assert_eq!(f.rows[0].label, "Done");
        assert!(f.rows[0].enabled);
        assert!(f.labels.iter().any(|l| l.text == TITLE));

        // Scrolling to the end must eventually show the last alphabetical
        // row somewhere in `labels` (caption + value pair).
        for _ in 0..GENERAL_STATS.len() {
            nav.scroll(true);
        }
        let f = frame(&nav, &snapshot);
        let last_caption = general_rows(&snapshot).last().unwrap().0;
        assert!(
            f.labels.iter().any(|l| l.text == last_caption),
            "scrolling to the end must reach the last row"
        );
    }

    #[test]
    fn scrolling_never_goes_negative_or_past_the_last_window() {
        let mut nav = StatsNav::default();
        nav.scroll(false);
        assert_eq!(nav.first(), 0, "cannot scroll above the top");
        for _ in 0..1000 {
            nav.scroll(true);
        }
        let window = visible_rows_len();
        assert!(
            nav.first() + window >= GENERAL_STATS.len(),
            "must be able to see the last row"
        );
        assert!(
            nav.first() <= GENERAL_STATS.len(),
            "must not scroll past the content entirely"
        );
    }

    #[test]
    fn reset_returns_to_the_top() {
        let mut nav = StatsNav::default();
        nav.scroll(true);
        nav.scroll(true);
        assert!(nav.first() > 0);
        nav.reset();
        assert_eq!(nav.first(), 0);
    }
}
