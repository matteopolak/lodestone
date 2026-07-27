//! Pure folding of live client state (scoreboard sidebar, boss bars) into flat,
//! version-free view structs the HUD can draw.
//!
//! Kept separate from [`crate::hud`] geometry and from the [`crate::net`] /
//! [`crate::sim`] wiring so the *interpretation* of scoreboard/boss-bar state is
//! unit-testable with no GPU and no server — which is where the "built, tested,
//! wired to nothing" gap (§12.24) actually closes: these functions are the last
//! mile between modelled game state and pixels, and they assert on the exact
//! rows a player would read.
//!
//! The client's [`Scoreboard`] snapshot is read-only (its mutators are
//! crate-private), so the reusable core here folds over the public
//! [`ScoreEntry`] slice the client hands out; [`sidebar_from`] is a thin adapter
//! that pulls that slice from a live snapshot.

use lodestone_client::{BossBar, BossColor, DisplaySlot, Scoreboard, ScoreEntry, Text};

/// Vanilla renders at most 15 sidebar entries below the title.
const MAX_SIDEBAR_LINES: usize = 15;

/// A ready-to-draw scoreboard sidebar: a title plus up to 15 rows, each a label
/// and its score string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sidebar {
    /// The objective's display name, shown centred at the top.
    pub title: String,
    /// The score rows, top-to-bottom in render order.
    pub lines: Vec<SidebarLine>,
}

/// One sidebar row: the holder's label and its score value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarLine {
    /// Left-aligned holder label (per-score display override, else the holder).
    pub label: String,
    /// Right-aligned score (rendered in red, vanilla-style, by the HUD).
    pub score: String,
}

/// Build a [`Sidebar`] from a title and the objective's score entries, which
/// the caller supplies **already sorted** in render order (the client sorts by
/// descending value then holder). This core does the shell's own work: apply
/// the per-score display override, stringify the value, and clamp to the 15
/// lines vanilla shows — and is pure, so it is tested directly.
#[must_use]
pub fn sidebar_view(title: &str, entries: &[ScoreEntry]) -> Sidebar {
    let lines = entries
        .iter()
        .take(MAX_SIDEBAR_LINES)
        .map(|e| SidebarLine {
            label: e
                .display
                .as_ref()
                .map_or_else(|| e.holder.clone(), Text::to_plain_string),
            score: e.value.to_string(),
        })
        .collect();
    Sidebar {
        title: title.to_string(),
        lines,
    }
}

/// Fold a live scoreboard snapshot's sidebar slot into a [`Sidebar`], or `None`
/// when no objective is displayed there.
///
/// The plain `sidebar` slot is used (team-colour sidebars need the player's own
/// team, which the shell does not track), matching what a spectator sees.
#[must_use]
pub fn sidebar_from(sb: &Scoreboard) -> Option<Sidebar> {
    let objective = sb.displayed(DisplaySlot::Sidebar)?;
    let title = sb
        .objective(objective)
        .and_then(|o| o.display_name.as_ref().map(Text::to_plain_string))
        .unwrap_or_else(|| objective.to_string());
    Some(sidebar_view(&title, &sb.scores_in_slot(DisplaySlot::Sidebar)))
}

/// A ready-to-draw boss bar: a plain title, a clamped progress fraction, and an
/// RGB tint derived from the bar colour.
#[derive(Debug, Clone, PartialEq)]
pub struct BossBarView {
    /// Plain-text title.
    pub title: String,
    /// Progress in `0.0..=1.0`.
    pub progress: f32,
    /// Bar tint (RGB in `0..1`).
    pub color: [f32; 3],
}

/// Fold the active boss bars into drawable views, preserving server (render)
/// order. Progress is clamped defensively in case a server sends out of range.
#[must_use]
pub fn boss_bars_from(bars: &[BossBar]) -> Vec<BossBarView> {
    bars.iter()
        .map(|b| BossBarView {
            title: b.title.to_plain_string(),
            progress: b.progress.clamp(0.0, 1.0),
            color: boss_color_rgb(b.color),
        })
        .collect()
}

/// Map a vanilla boss-bar colour to an approximate RGB tint.
fn boss_color_rgb(color: BossColor) -> [f32; 3] {
    match color {
        BossColor::Pink => [0.96, 0.40, 0.71],
        BossColor::Blue => [0.30, 0.55, 0.95],
        BossColor::Red => [0.90, 0.20, 0.20],
        BossColor::Green => [0.35, 0.80, 0.30],
        BossColor::Yellow => [0.95, 0.85, 0.25],
        BossColor::Purple => [0.65, 0.35, 0.90],
        BossColor::White => [0.92, 0.92, 0.92],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_client::{BossOverlay, NumberFormat};

    fn entry(holder: &str, value: i32, display: Option<&str>) -> ScoreEntry {
        ScoreEntry {
            holder: holder.to_string(),
            objective: "obj".to_string(),
            value,
            display: display.map(Text::literal),
            number_format: None::<NumberFormat>,
        }
    }

    fn boss_bar(title: &str, progress: f32, color: BossColor) -> BossBar {
        BossBar {
            id: uuid::Uuid::nil(),
            title: Text::literal(title),
            progress,
            color,
            overlay: BossOverlay::Progress,
            darken: false,
            music: false,
            fog: false,
        }
    }

    #[test]
    fn sidebar_view_formats_rows_and_honours_display_override() {
        // Caller-sorted (desc value) entries; the middle one carries a display
        // override that must win over its holder key.
        let entries = [
            entry("bob", 10, None),
            entry("alice", 5, Some("Alice the Brave")),
            entry("carol", 1, None),
        ];
        let side = sidebar_view("Stats", &entries);
        assert_eq!(side.title, "Stats");
        let rows: Vec<(&str, &str)> = side
            .lines
            .iter()
            .map(|l| (l.label.as_str(), l.score.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![("bob", "10"), ("Alice the Brave", "5"), ("carol", "1")],
            "labels use the display override when present; scores stringify"
        );
    }

    #[test]
    fn sidebar_view_clamps_to_fifteen_lines() {
        // 30 pre-sorted entries (30 down to 1); only the first 15 may survive.
        let entries: Vec<ScoreEntry> = (0..30)
            .map(|i| entry(&format!("p{i:02}"), 30 - i, None))
            .collect();
        let side = sidebar_view("Big", &entries);
        assert_eq!(
            side.lines.len(),
            MAX_SIDEBAR_LINES,
            "vanilla shows at most 15 sidebar rows, not all 30"
        );
        assert_eq!(side.lines[0].score, "30", "the top (first) row is kept");
        assert_eq!(
            side.lines[14].score, "16",
            "the 15th row is kept; the 16th (score 15) is dropped"
        );
    }

    #[test]
    fn boss_bars_fold_title_progress_and_clamp() {
        let a = boss_bar("Ender Dragon", 0.5, BossColor::Purple);
        let b = boss_bar("Overshoot", 2.0, BossColor::Red); // deliberately out of range

        let views = boss_bars_from(&[a, b]);
        assert_eq!(views.len(), 2, "one view per active bar, order preserved");
        assert_eq!(views[0].title, "Ender Dragon");
        assert!((views[0].progress - 0.5).abs() < 1e-6);
        assert_eq!(views[1].title, "Overshoot");
        assert!(
            (views[1].progress - 1.0).abs() < 1e-6,
            "progress must clamp to 1.0, got {}",
            views[1].progress
        );
        assert_ne!(
            views[0].color, views[1].color,
            "purple and red must tint differently"
        );
    }
}
