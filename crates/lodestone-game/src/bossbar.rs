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
