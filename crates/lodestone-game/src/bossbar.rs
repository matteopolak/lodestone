//! Boss bars: the coloured progress bars shown at the top of the screen.
//!
//! Each is keyed by a UUID so the server can add, update, and remove them
//! independently. Besides a title and progress, a boss bar carries a colour, a
//! division/overlay style, and three client-visual flags (darken sky, boss
//! music, world fog).

use std::collections::HashMap;

use lodestone_model::event as m;
use lodestone_model::{ClientEvent, Text};
use uuid::Uuid;

/// Boss bar colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(missing_docs)]
pub enum BossBarColor {
    Pink,
    Blue,
    Red,
    Green,
    Yellow,
    #[default]
    Purple,
    White,
}

impl BossBarColor {
    /// The wire id (matches vanilla enum order).
    #[must_use]
    pub fn id(self) -> u8 {
        match self {
            BossBarColor::Pink => 0,
            BossBarColor::Blue => 1,
            BossBarColor::Red => 2,
            BossBarColor::Green => 3,
            BossBarColor::Yellow => 4,
            BossBarColor::Purple => 5,
            BossBarColor::White => 6,
        }
    }

    /// From a wire id.
    #[must_use]
    pub fn from_id(id: u8) -> Option<Self> {
        Some(match id {
            0 => BossBarColor::Pink,
            1 => BossBarColor::Blue,
            2 => BossBarColor::Red,
            3 => BossBarColor::Green,
            4 => BossBarColor::Yellow,
            5 => BossBarColor::Purple,
            6 => BossBarColor::White,
            _ => return None,
        })
    }

    /// The GUI atlas sprite id for this colour's **background** plate —
    /// vanilla's own background-sprites table.
    /// Each colour is a distinct pre-baked sprite; vanilla's `blitSprite` call
    /// for it passes no tint (`color = -1`, i.e. opaque white), so a renderer
    /// must select the sprite by id rather than tint a shared greyscale one.
    #[must_use]
    pub fn background_sprite_id(self) -> &'static str {
        match self {
            BossBarColor::Pink => "boss_bar/pink_background",
            BossBarColor::Blue => "boss_bar/blue_background",
            BossBarColor::Red => "boss_bar/red_background",
            BossBarColor::Green => "boss_bar/green_background",
            BossBarColor::Yellow => "boss_bar/yellow_background",
            BossBarColor::Purple => "boss_bar/purple_background",
            BossBarColor::White => "boss_bar/white_background",
        }
    }

    /// The GUI atlas sprite id for this colour's **progress** fill —
    /// vanilla's own progress-sprites table. Also untinted; see
    /// [`Self::background_sprite_id`].
    #[must_use]
    pub fn progress_sprite_id(self) -> &'static str {
        match self {
            BossBarColor::Pink => "boss_bar/pink_progress",
            BossBarColor::Blue => "boss_bar/blue_progress",
            BossBarColor::Red => "boss_bar/red_progress",
            BossBarColor::Green => "boss_bar/green_progress",
            BossBarColor::Yellow => "boss_bar/yellow_progress",
            BossBarColor::Purple => "boss_bar/purple_progress",
            BossBarColor::White => "boss_bar/white_progress",
        }
    }
}

/// Boss bar division/overlay style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(missing_docs)]
pub enum BossBarOverlay {
    #[default]
    Progress,
    Notched6,
    Notched10,
    Notched12,
    Notched20,
}

impl BossBarOverlay {
    /// The wire id (matches vanilla enum order).
    #[must_use]
    pub fn id(self) -> u8 {
        match self {
            BossBarOverlay::Progress => 0,
            BossBarOverlay::Notched6 => 1,
            BossBarOverlay::Notched10 => 2,
            BossBarOverlay::Notched12 => 3,
            BossBarOverlay::Notched20 => 4,
        }
    }

    /// From a wire id.
    #[must_use]
    pub fn from_id(id: u8) -> Option<Self> {
        Some(match id {
            0 => BossBarOverlay::Progress,
            1 => BossBarOverlay::Notched6,
            2 => BossBarOverlay::Notched10,
            3 => BossBarOverlay::Notched12,
            4 => BossBarOverlay::Notched20,
            _ => return None,
        })
    }

    /// The GUI atlas sprite id for this overlay's **background** notch art, or
    /// `None` for [`BossBarOverlay::Progress`] — vanilla's
    /// `BossHealthOverlay.extractBar` only blits an overlay sprite when
    /// `event.getOverlay() != BossEvent.BossBarOverlay.PROGRESS`
    /// (`.cache/mc/26.2/client-src`), so the plain progress style draws no
    /// notch layer at all. Drawn on top of the background colour plate, at the
    /// bar's **full** native width (unlike the progress-side twin, which is
    /// clipped to the health fraction).
    #[must_use]
    pub fn background_sprite_id(self) -> Option<&'static str> {
        Some(match self {
            BossBarOverlay::Progress => return None,
            BossBarOverlay::Notched6 => "boss_bar/notched_6_background",
            BossBarOverlay::Notched10 => "boss_bar/notched_10_background",
            BossBarOverlay::Notched12 => "boss_bar/notched_12_background",
            BossBarOverlay::Notched20 => "boss_bar/notched_20_background",
        })
    }

    /// The GUI atlas sprite id for this overlay's **progress** notch art, or
    /// `None` for [`BossBarOverlay::Progress`]. Drawn on top of the progress
    /// colour fill, clipped to the same health-fraction width as that fill.
    #[must_use]
    pub fn progress_sprite_id(self) -> Option<&'static str> {
        Some(match self {
            BossBarOverlay::Progress => return None,
            BossBarOverlay::Notched6 => "boss_bar/notched_6_progress",
            BossBarOverlay::Notched10 => "boss_bar/notched_10_progress",
            BossBarOverlay::Notched12 => "boss_bar/notched_12_progress",
            BossBarOverlay::Notched20 => "boss_bar/notched_20_progress",
        })
    }
}

/// A single boss bar.
#[derive(Debug, Clone, PartialEq)]
pub struct BossBar {
    /// Displayed title.
    pub title: Text,
    /// Progress, clamped to `0.0..=1.0`.
    pub progress: f32,
    /// Bar colour.
    pub color: BossBarColor,
    /// Division/overlay style.
    pub overlay: BossBarOverlay,
    /// Darken the sky while shown.
    pub darken_screen: bool,
    /// Play boss music while shown.
    pub play_music: bool,
    /// Create world fog while shown.
    pub create_fog: bool,
}

impl BossBar {
    /// Creates a full-progress purple/progress boss bar with the given title.
    #[must_use]
    pub fn new(title: Text) -> Self {
        Self {
            title,
            progress: 1.0,
            color: BossBarColor::Purple,
            overlay: BossBarOverlay::Progress,
            darken_screen: false,
            play_music: false,
            create_fog: false,
        }
    }

    /// Sets progress, clamping to `0.0..=1.0`.
    pub fn set_progress(&mut self, progress: f32) {
        self.progress = progress.clamp(0.0, 1.0);
    }
}

/// The set of active boss bars, keyed by UUID and preserving insertion order for
/// rendering.
#[derive(Debug, Clone, Default)]
pub struct BossBarSet {
    order: Vec<Uuid>,
    bars: HashMap<Uuid, BossBar>,
}

impl BossBarSet {
    /// A new empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces a boss bar (the `add` action).
    pub fn add(&mut self, id: Uuid, bar: BossBar) {
        if !self.bars.contains_key(&id) {
            self.order.push(id);
        }
        self.bars.insert(id, bar);
    }

    /// Removes a boss bar.
    pub fn remove(&mut self, id: &Uuid) {
        if self.bars.remove(id).is_some() {
            self.order.retain(|x| x != id);
        }
    }

    /// Mutable access for a partial update action.
    pub fn get_mut(&mut self, id: &Uuid) -> Option<&mut BossBar> {
        self.bars.get_mut(id)
    }

    /// Looks up a boss bar.
    #[must_use]
    pub fn get(&self, id: &Uuid) -> Option<&BossBar> {
        self.bars.get(id)
    }

    /// Number of active bars.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bars.len()
    }

    /// Whether there are no active bars.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    /// The bars in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&Uuid, &BossBar)> {
        self.order.iter().map(|id| (id, &self.bars[id]))
    }

    /// Whether any active bar requests the sky be darkened.
    #[must_use]
    pub fn any_darken_screen(&self) -> bool {
        self.bars.values().any(|b| b.darken_screen)
    }

    /// Whether any active bar requests world fog.
    #[must_use]
    pub fn any_fog(&self) -> bool {
        self.bars.values().any(|b| b.create_fog)
    }
}

// --- ClientEvent fold -------------------------------------------------------
//
// Translates the model's parallel boss-bar enums into this crate's types and
// folds a `BossBarUpdate` into the set. The model type is imported as `m`.

impl From<m::BossColor> for BossBarColor {
    fn from(c: m::BossColor) -> Self {
        match c {
            m::BossColor::Pink => BossBarColor::Pink,
            m::BossColor::Blue => BossBarColor::Blue,
            m::BossColor::Red => BossBarColor::Red,
            m::BossColor::Green => BossBarColor::Green,
            m::BossColor::Yellow => BossBarColor::Yellow,
            m::BossColor::Purple => BossBarColor::Purple,
            m::BossColor::White => BossBarColor::White,
        }
    }
}

impl From<m::BossOverlay> for BossBarOverlay {
    fn from(o: m::BossOverlay) -> Self {
        match o {
            m::BossOverlay::Progress => BossBarOverlay::Progress,
            m::BossOverlay::Notched6 => BossBarOverlay::Notched6,
            m::BossOverlay::Notched10 => BossBarOverlay::Notched10,
            m::BossOverlay::Notched12 => BossBarOverlay::Notched12,
            m::BossOverlay::Notched20 => BossBarOverlay::Notched20,
        }
    }
}

impl BossBarSet {
    /// Folds a [`BossBarUpdate`](ClientEvent::BossBarUpdate) into the set,
    /// returning whether the event was a boss-bar one. `Add` inserts or replaces
    /// the bar in place (preserving its render order); every partial update is a
    /// no-op if no bar with that id is present, matching the server's contract
    /// that updates follow an add.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        let ClientEvent::BossBarUpdate { id, action } = event else {
            return false;
        };
        match action {
            m::BossAction::Add {
                title,
                progress,
                color,
                overlay,
                darken,
                music,
                fog,
            } => {
                self.add(
                    *id,
                    BossBar {
                        title: (**title).clone(),
                        progress: progress.clamp(0.0, 1.0),
                        color: (*color).into(),
                        overlay: (*overlay).into(),
                        darken_screen: *darken,
                        play_music: *music,
                        create_fog: *fog,
                    },
                );
            }
            m::BossAction::Remove => self.remove(id),
            m::BossAction::UpdateProgress(p) => {
                if let Some(bar) = self.get_mut(id) {
                    bar.set_progress(*p);
                }
            }
            m::BossAction::UpdateName(title) => {
                if let Some(bar) = self.get_mut(id) {
                    bar.title = (**title).clone();
                }
            }
            m::BossAction::UpdateStyle { color, overlay } => {
                if let Some(bar) = self.get_mut(id) {
                    bar.color = (*color).into();
                    bar.overlay = (*overlay).into();
                }
            }
            m::BossAction::UpdateFlags { darken, music, fog } => {
                if let Some(bar) = self.get_mut(id) {
                    bar.darken_screen = *darken;
                    bar.play_music = *music;
                    bar.create_fog = *fog;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod sprite_id_tests {
    use super::*;

    /// Every colour must resolve to a **distinct** background sprite id and a
    /// distinct progress sprite id — vanilla's `BAR_BACKGROUND_SPRITES`/
    /// `BAR_PROGRESS_SPRITES` are seven separate PNGs
    /// (`.cache/mc/26.2/client-src/assets/minecraft/textures/gui/sprites/boss_bar/`),
    /// not one greyscale sprite tinted seven ways. Collected into one
    /// assertion rather than seven, so a single colliding pair still reports
    /// which.
    #[test]
    fn every_colour_resolves_to_a_distinct_sprite_pair() {
        let colors = [
            BossBarColor::Pink,
            BossBarColor::Blue,
            BossBarColor::Red,
            BossBarColor::Green,
            BossBarColor::Yellow,
            BossBarColor::Purple,
            BossBarColor::White,
        ];
        let mut wrong = Vec::new();
        for (i, a) in colors.iter().enumerate() {
            for b in &colors[i + 1..] {
                if a.background_sprite_id() == b.background_sprite_id() {
                    wrong.push(format!("{a:?} and {b:?} share a background sprite id"));
                }
                if a.progress_sprite_id() == b.progress_sprite_id() {
                    wrong.push(format!("{a:?} and {b:?} share a progress sprite id"));
                }
            }
            // Every id lives under the `boss_bar/` folder, ends in the right
            // suffix, and a colour's background and progress ids differ from
            // each other.
            if !a.background_sprite_id().starts_with("boss_bar/")
                || !a.background_sprite_id().ends_with("_background")
            {
                wrong.push(format!("{a:?} background id malformed: {}", a.background_sprite_id()));
            }
            if !a.progress_sprite_id().starts_with("boss_bar/") || !a.progress_sprite_id().ends_with("_progress")
            {
                wrong.push(format!("{a:?} progress id malformed: {}", a.progress_sprite_id()));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// The `Progress` overlay style is vanilla's "no notch art" case —
    /// `BossHealthOverlay.extractBar` only blits an overlay sprite when
    /// `event.getOverlay() != BossEvent.BossBarOverlay.PROGRESS`
    /// (`.cache/mc/26.2/client-src`) — and every notched style must resolve to
    /// a distinct pair, one per `OVERLAY_BACKGROUND_SPRITES`/
    /// `OVERLAY_PROGRESS_SPRITES` entry.
    #[test]
    fn progress_overlay_has_no_sprite_and_every_notch_is_distinct() {
        assert_eq!(BossBarOverlay::Progress.background_sprite_id(), None);
        assert_eq!(BossBarOverlay::Progress.progress_sprite_id(), None);

        let notches = [
            BossBarOverlay::Notched6,
            BossBarOverlay::Notched10,
            BossBarOverlay::Notched12,
            BossBarOverlay::Notched20,
        ];
        let mut wrong = Vec::new();
        for (i, a) in notches.iter().enumerate() {
            let a_bg = a.background_sprite_id();
            let a_pg = a.progress_sprite_id();
            if a_bg.is_none() || a_pg.is_none() {
                wrong.push(format!("{a:?} must have both a background and a progress sprite"));
            }
            for b in &notches[i + 1..] {
                if a_bg == b.background_sprite_id() {
                    wrong.push(format!("{a:?} and {b:?} share a background notch sprite id"));
                }
                if a_pg == b.progress_sprite_id() {
                    wrong.push(format!("{a:?} and {b:?} share a progress notch sprite id"));
                }
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }
}

#[cfg(test)]
mod fold_tests {
    use super::*;

    fn id() -> Uuid {
        Uuid::from_u128(0x1234)
    }

    fn add_event(progress: f32) -> ClientEvent {
        ClientEvent::BossBarUpdate {
            id: id(),
            action: m::BossAction::Add {
                title: Box::new(Text::literal("Ender Dragon")),
                progress,
                color: m::BossColor::Red,
                overlay: m::BossOverlay::Notched6,
                darken: true,
                music: false,
                fog: true,
            },
        }
    }

    #[test]
    fn add_is_readable_and_progress_clamped() {
        let mut bars = BossBarSet::new();
        assert!(bars.apply(&add_event(2.0)));
        let bar = bars.get(&id()).expect("bar present after add");
        assert_eq!(bar.title, Text::literal("Ender Dragon"));
        assert_eq!(bar.progress, 1.0); // clamped from 2.0
        assert_eq!(bar.color, BossBarColor::Red);
        assert_eq!(bar.overlay, BossBarOverlay::Notched6);
        assert!(bar.darken_screen);
        assert!(!bar.play_music);
        assert!(bar.create_fog);
    }

    #[test]
    fn partial_updates_mutate_in_place() {
        let mut bars = BossBarSet::new();
        bars.apply(&add_event(1.0));
        bars.apply(&ClientEvent::BossBarUpdate {
            id: id(),
            action: m::BossAction::UpdateProgress(0.25),
        });
        bars.apply(&ClientEvent::BossBarUpdate {
            id: id(),
            action: m::BossAction::UpdateName(Box::new(Text::literal("Wither"))),
        });
        bars.apply(&ClientEvent::BossBarUpdate {
            id: id(),
            action: m::BossAction::UpdateStyle {
                color: m::BossColor::Purple,
                overlay: m::BossOverlay::Notched20,
            },
        });
        bars.apply(&ClientEvent::BossBarUpdate {
            id: id(),
            action: m::BossAction::UpdateFlags {
                darken: false,
                music: true,
                fog: false,
            },
        });
        let bar = bars.get(&id()).expect("bar present");
        assert_eq!(bar.progress, 0.25);
        assert_eq!(bar.title, Text::literal("Wither"));
        assert_eq!(bar.color, BossBarColor::Purple);
        assert_eq!(bar.overlay, BossBarOverlay::Notched20);
        assert!(!bar.darken_screen);
        assert!(bar.play_music);
        assert!(!bar.create_fog);
    }

    #[test]
    fn remove_drops_the_bar() {
        let mut bars = BossBarSet::new();
        bars.apply(&add_event(1.0));
        assert_eq!(bars.len(), 1);
        bars.apply(&ClientEvent::BossBarUpdate {
            id: id(),
            action: m::BossAction::Remove,
        });
        assert!(bars.is_empty());
    }

    #[test]
    fn update_for_absent_bar_is_a_noop() {
        let mut bars = BossBarSet::new();
        assert!(bars.apply(&ClientEvent::BossBarUpdate {
            id: id(),
            action: m::BossAction::UpdateProgress(0.5),
        }));
        assert!(bars.is_empty());
    }

    #[test]
    fn non_bossbar_event_is_not_claimed() {
        let mut bars = BossBarSet::new();
        assert!(!bars.apply(&ClientEvent::PlayerListRemove {
            profile_ids: vec![],
        }));
    }
}
