//! Pure folding of live client state (the scoreboard sidebar's *shape*, boss
//! bars) into flat, version-free view structs the HUD can draw.
//!
//! Kept separate from [`crate::hud`] geometry and from the [`crate::net`] /
//! [`crate::sim`] wiring so the *interpretation* of scoreboard/boss-bar state is
//! unit-testable with no GPU and no server — which is where the "built, tested,
//! wired to nothing" gap (§12.24) actually closes: these types are the last mile
//! between modelled game state and pixels, and their tests assert on the exact
//! rows a player would read.
//!
//! ## What Stage 3 removed from here
//!
//! This module used to carry a *second* sidebar projection, `sidebar_from` /
//! `sidebar_view`, over the deleted `lodestone_client::Scoreboard`. It was
//! reachable only through `NetClient::sidebar()`, which **nothing called** — the
//! HUD has drawn [`crate::scoreboard::sidebar_from`] (over `lodestone-game`'s
//! aggregate, with `translate` resolution) for as long as that function has
//! existed. Two projections of one thing, one of them unreachable, was the same
//! defect one layer up from the double fold `docs/bevy-migration.md` §1.1
//! measured. [`Sidebar`] / [`SidebarLine`] stay here because they are the HUD's
//! vocabulary, not the fold's.
//!
//! ## The same defect, found a second time
//!
//! `NetClient::boss_bars()` was the boss-bar twin of exactly that shape, and it
//! survived Stage 3 because nothing about it looked wrong on inspection: a
//! `pub fn` with a plausible doc comment, folding the right component, next to
//! sibling accessors that *are* live. Nothing called it — the HUD reaches
//! [`Sidebar`]'s counterpart through [`crate::sim::Sim::boss_bars`], which reads
//! the same `lodestone_ecs::SessionBossBars` component directly.
//!
//! It was **not** merely redundant. It passed a null translator (`&|_| None`)
//! into [`boss_bars_from`], so anything that had started calling it would have
//! rendered boss-bar titles as raw keys like `entity.minecraft.ender_dragon` —
//! and it only carried that null translator because the *live* path was given a
//! real one while fixing exactly that bug, and this one had no language table to
//! fix it with. A dead projection does not stay merely dead: it drifts, silently,
//! away from the live one, and it is the version a future caller will find first.
//!
//! The lesson for the next one: **two projections of one thing is the defect**,
//! whether or not the dead half currently misbehaves. Grep for a second folder
//! whenever one is touched.

use lodestone_game::bossbar::{BossBarColor, BossBarSet};

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
///
/// Takes the folded [`BossBarSet`] — one of the three implementations of this
/// event family that Stage 3 collapsed to one. `BossBarSet::iter` is what
/// carries insertion order; a `HashMap` iteration would shuffle the stack every
/// frame.
///
/// `translate` is the language table (an `assets::Language` becomes one via
/// `Language::translator`). It is **not** optional decoration: a vanilla boss
/// bar's title is the boss entity's display name, i.e.
/// `translate("entity.minecraft.ender_dragon")` — flattening it without a table
/// puts the raw key on screen. Same shape as [`crate::scoreboard::sidebar_from`]
/// and [`crate::tablist::player_rows`], deliberately.
#[must_use]
pub fn boss_bars_from(
    bars: &BossBarSet,
    translate: &dyn Fn(&str) -> Option<String>,
) -> Vec<BossBarView> {
    bars.iter()
        .map(|(_, b)| BossBarView {
            title: lodestone_game::text::resolve_to_string(&b.title, translate),
            progress: b.progress.clamp(0.0, 1.0),
            color: boss_color_rgb(b.color),
        })
        .collect()
}

/// Map a vanilla boss-bar colour to an approximate RGB tint.
fn boss_color_rgb(color: BossBarColor) -> [f32; 3] {
    match color {
        BossBarColor::Pink => [0.96, 0.40, 0.71],
        BossBarColor::Blue => [0.30, 0.55, 0.95],
        BossBarColor::Red => [0.90, 0.20, 0.20],
        BossBarColor::Green => [0.35, 0.80, 0.30],
        BossBarColor::Yellow => [0.95, 0.85, 0.25],
        BossBarColor::Purple => [0.65, 0.35, 0.90],
        BossBarColor::White => [0.92, 0.92, 0.92],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::bossbar::{BossBar, BossBarOverlay};
    use lodestone_model::Text;

    fn boss_bar(title: &str, progress: f32, color: BossBarColor) -> BossBar {
        BossBar {
            title: Text::literal(title),
            // Deliberately assigned rather than via `set_progress`, which
            // clamps: the clamp under test is this module's own defensive one,
            // and going through the setter would make the assertion vacuous.
            progress,
            color,
            overlay: BossBarOverlay::Progress,
            darken_screen: false,
            play_music: false,
            create_fog: false,
        }
    }

    /// A tiny table so these tests depend on no real asset. Deliberately does
    /// **not** contain the chat/death keys `Text`'s built-in stub table carries,
    /// so a fold that ignored this closure would be visible.
    fn tr(key: &str) -> Option<String> {
        match key {
            "entity.minecraft.ender_dragon" => Some("Ender Dragon".to_owned()),
            _ => None,
        }
    }

    #[test]
    fn titles_resolve_translate_components_through_the_translator() {
        // A vanilla boss bar's title is the boss's display name — a `translate`
        // node, not prose. Issue #52's defect class: flattening it without the
        // language table renders `entity.minecraft.ender_dragon`.
        let mut bars = BossBarSet::new();
        bars.add(
            uuid::Uuid::from_u128(7),
            BossBar {
                title: Text::translate("entity.minecraft.ender_dragon", vec![]),
                progress: 1.0,
                color: BossBarColor::Purple,
                overlay: BossBarOverlay::Progress,
                darken_screen: false,
                play_music: false,
                create_fog: false,
            },
        );

        let views = boss_bars_from(&bars, &tr);
        assert_eq!(views[0].title, "Ender Dragon");

        // -- negative control -------------------------------------------------
        // The same fold against a table that knows nothing must surface the raw
        // key, proving the assertion above is reading the translator and not a
        // built-in table that happens to agree.
        let empty = boss_bars_from(&bars, &|_| None);
        assert_eq!(empty[0].title, "entity.minecraft.ender_dragon");
    }

    #[test]
    fn boss_bars_fold_title_progress_and_clamp_in_insertion_order() {
        let mut bars = BossBarSet::new();
        bars.add(
            uuid::Uuid::from_u128(1),
            boss_bar("Ender Dragon", 0.5, BossBarColor::Purple),
        );
        // Out of range on purpose.
        bars.add(
            uuid::Uuid::from_u128(2),
            boss_bar("Overshoot", 2.0, BossBarColor::Red),
        );

        let views = boss_bars_from(&bars, &tr);
        assert_eq!(views.len(), 2, "one view per active bar");
        assert_eq!(
            views[0].title, "Ender Dragon",
            "insertion order is render order"
        );
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
