//! HUD-adjacent player state: vitals, experience, the held slot, game mode,
//! difficulty, respawn state, and the title/subtitle/action-bar system.
//!
//! This is the version-free canonical state the HUD renders from. A protocol
//! adapter lowers `set_health`, `set_experience`, `player_abilities`,
//! `change_difficulty`, the title packets, and friends into mutations here.

use lodestone_model::{ClientEvent, Difficulty, GameMode, Text};

/// Player vitals and progression shown on the HUD.
#[derive(Debug, Clone, PartialEq)]
pub struct HudState {
    /// Health in half-heart units of a point (`20.0` = full). Death at `<= 0`.
    pub health: f32,
    /// Food level, `0..=20`.
    pub food: i32,
    /// Food saturation, `0.0..=20.0` (hidden reserve that drains before food).
    pub saturation: f32,
    /// Remaining air, in ticks (`300` = full, 15 bubbles). Only shown when
    /// under water or otherwise below the maximum. See [`HudState::MAX_AIR`].
    pub air: i32,
    /// Experience level.
    pub xp_level: i32,
    /// Progress toward the next level, `0.0..=1.0`.
    pub xp_progress: f32,
    /// Total experience points collected.
    pub xp_total: i32,
    /// Selected hotbar slot, `0..=8`.
    pub selected_slot: u8,
    /// Current game mode.
    pub game_mode: GameMode,
    /// Previous game mode (vanilla tracks this for the F3+N toggle); `None`
    /// until the mode has changed at least once.
    pub previous_game_mode: Option<GameMode>,
    /// World difficulty.
    pub difficulty: Difficulty,
    /// Whether difficulty is locked.
    pub difficulty_locked: bool,
    /// Whether the player is currently dead (awaiting respawn). Derived from
    /// health on [`set_health`](Self::set_health) but kept explicit so a respawn
    /// packet can clear it independently.
    pub dead: bool,
}

impl Default for HudState {
    fn default() -> Self {
        Self {
            health: 20.0,
            food: 20,
            saturation: 5.0,
            air: HudState::MAX_AIR,
            xp_level: 0,
            xp_progress: 0.0,
            xp_total: 0,
            selected_slot: 0,
            game_mode: GameMode::Survival,
            previous_game_mode: None,
            difficulty: Difficulty::Normal,
            difficulty_locked: false,
            dead: false,
        }
    }
}

impl HudState {
    /// Full air, in ticks: 15 bubbles × 20 ticks. Vanilla's base air supply
    /// (the `minecraft:max_air` attribute default).
    pub const MAX_AIR: i32 = 300;

    /// A fresh full-health survival state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a `set_health` update, marking the player dead at `<= 0`.
    pub fn set_health(&mut self, health: f32, food: i32, saturation: f32) {
        self.health = health;
        self.food = food;
        self.saturation = saturation;
        if health <= 0.0 {
            self.dead = true;
        }
    }

    /// Applies a `set_experience` update.
    pub fn set_experience(&mut self, progress: f32, level: i32, total: i32) {
        self.xp_progress = progress.clamp(0.0, 1.0);
        self.xp_level = level;
        self.xp_total = total;
    }

    /// Sets remaining air in ticks (from entity metadata / `set_entity_data`),
    /// clamped to `0..=MAX_AIR`.
    pub fn set_air(&mut self, air: i32) {
        self.air = air.clamp(0, Self::MAX_AIR);
    }

    /// Changes the game mode, recording the previous one.
    pub fn set_game_mode(&mut self, mode: GameMode) {
        if mode != self.game_mode {
            self.previous_game_mode = Some(self.game_mode);
            self.game_mode = mode;
        }
    }

    /// Selects a hotbar slot; out-of-range values are ignored.
    pub fn select_slot(&mut self, slot: u8) {
        if slot < 9 {
            self.selected_slot = slot;
        }
    }

    /// Resets vitals to defaults on respawn and clears the dead flag. Keeps the
    /// game mode and difficulty, which the respawn packet sets separately.
    pub fn respawn(&mut self) {
        self.health = 20.0;
        self.food = 20;
        self.saturation = 5.0;
        self.air = Self::MAX_AIR;
        self.dead = false;
    }

    /// Whether the player is dead (health depleted).
    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.dead || self.health <= 0.0
    }

    /// Folds a [`ClientEvent`] into the HUD state, returning `true` if the event
    /// was one this state owns. A driver fans each event across the game
    /// aggregates and stops at the first that claims it (the same contract as
    /// [`Scoreboard::apply`](crate::scoreboard::Scoreboard::apply)).
    ///
    /// Owns the vitals / progression / game-mode / difficulty / held-slot
    /// family. `air` is intentionally *not* folded here: it arrives inside the
    /// local player's entity metadata (`EntityMetadataUpdated`), which needs
    /// entity-id resolution the caller must do before calling
    /// [`set_air`](Self::set_air). Respawn is likewise driver-owned — the wire
    /// respawn arrives as a login/respawn packet, not a distinct HUD event.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::HealthChanged {
                health,
                food,
                saturation,
            } => self.set_health(*health, *food, *saturation),
            ClientEvent::ExperienceChanged {
                progress,
                level,
                total,
            } => self.set_experience(*progress, *level, *total),
            ClientEvent::GameModeChanged { game_mode } => self.set_game_mode(*game_mode),
            ClientEvent::HeldSlotChanged { slot } => {
                // The wire value is an i32; `select_slot` drops anything `>= 9`,
                // so mapping out-of-range values (negative, or `> u8::MAX`) to
                // `u8::MAX` lets it reject them without a panicking cast.
                self.select_slot(u8::try_from(*slot).unwrap_or(u8::MAX));
            }
            ClientEvent::DifficultyChanged { difficulty, locked } => {
                self.difficulty = *difficulty;
                self.difficulty_locked = *locked;
            }
            ClientEvent::Death { .. } => {
                self.dead = true;
            }
            _ => return false,
        }
        true
    }
}

/// Fade-in / stay / fade-out durations, in ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitleTimes {
    /// Ticks to fade in.
    pub fade_in: i32,
    /// Ticks held fully visible.
    pub stay: i32,
    /// Ticks to fade out.
    pub fade_out: i32,
}

impl Default for TitleTimes {
    /// Vanilla defaults: 10 / 70 / 20 ticks.
    fn default() -> Self {
        Self {
            fade_in: 10,
            stay: 70,
            fade_out: 20,
        }
    }
}

impl TitleTimes {
    /// Total lifetime in ticks.
    #[must_use]
    pub fn total(self) -> i32 {
        self.fade_in + self.stay + self.fade_out
    }
}

/// The animation phase of a shown title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitlePhase {
    /// Fading in.
    FadeIn,
    /// Fully visible.
    Stay,
    /// Fading out.
    FadeOut,
    /// Finished (no longer shown).
    Done,
}

/// The title / subtitle overlay with its fade timing.
///
/// `elapsed` counts ticks since the title was (re)shown. Vanilla resets the
/// timer whenever the title or subtitle text is set while a title is active, so
/// updating the subtitle mid-display restarts the animation — a subtlety that
/// naive "set text, keep timer" implementations get wrong.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TitleState {
    title: Option<Text>,
    subtitle: Option<Text>,
    times: TitleTimes,
    elapsed: i32,
}

impl TitleState {
    /// A new empty title state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the timing without changing text or restarting.
    pub fn set_times(&mut self, times: TitleTimes) {
        self.times = times;
    }

    /// Sets the main title and (re)starts the animation.
    pub fn set_title(&mut self, title: Text) {
        self.title = Some(title);
        self.elapsed = 0;
    }

    /// Sets the subtitle. If a title is currently shown this restarts the
    /// animation, matching vanilla; if no title is present the subtitle is
    /// stored to accompany the next title.
    pub fn set_subtitle(&mut self, subtitle: Text) {
        self.subtitle = Some(subtitle);
        if self.title.is_some() {
            self.elapsed = 0;
        }
    }

    /// The current title text, if shown.
    #[must_use]
    pub fn title(&self) -> Option<&Text> {
        self.title.as_ref()
    }

    /// The current subtitle text, if any.
    #[must_use]
    pub fn subtitle(&self) -> Option<&Text> {
        self.subtitle.as_ref()
    }

    /// Advances the animation by `ticks`, clearing the title when it finishes.
    pub fn tick(&mut self, ticks: i32) {
        if self.title.is_none() {
            return;
        }
        self.elapsed += ticks;
        if self.elapsed >= self.times.total() {
            self.clear();
        }
    }

    /// Clears the title and subtitle immediately.
    pub fn clear(&mut self) {
        self.title = None;
        self.subtitle = None;
        self.elapsed = 0;
    }

    /// The current animation phase.
    #[must_use]
    pub fn phase(&self) -> TitlePhase {
        if self.title.is_none() {
            return TitlePhase::Done;
        }
        let TitleTimes { fade_in, stay, .. } = self.times;
        if self.elapsed < fade_in {
            TitlePhase::FadeIn
        } else if self.elapsed < fade_in + stay {
            TitlePhase::Stay
        } else if self.elapsed < self.times.total() {
            TitlePhase::FadeOut
        } else {
            TitlePhase::Done
        }
    }

    /// The current opacity, `0.0..=1.0`.
    #[must_use]
    pub fn alpha(&self) -> f32 {
        let TitleTimes {
            fade_in, fade_out, ..
        } = self.times;
        match self.phase() {
            TitlePhase::FadeIn if fade_in > 0 => self.elapsed as f32 / fade_in as f32,
            TitlePhase::FadeIn => 1.0,
            TitlePhase::Stay => 1.0,
            TitlePhase::FadeOut if fade_out > 0 => {
                (self.times.total() - self.elapsed) as f32 / fade_out as f32
            }
            TitlePhase::FadeOut => 0.0,
            TitlePhase::Done => 0.0,
        }
    }

    /// Folds a [`ClientEvent`] into the title overlay, returning `true` if the
    /// event was one this state owns. Same fan-out contract as
    /// [`HudState::apply`].
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::TitleText { text } => self.set_title(text.clone()),
            ClientEvent::SubtitleText { text } => self.set_subtitle(text.clone()),
            ClientEvent::TitlesAnimation {
                fade_in,
                stay,
                fade_out,
            } => self.set_times(TitleTimes {
                fade_in: *fade_in,
                stay: *stay,
                fade_out: *fade_out,
            }),
            ClientEvent::TitlesCleared { reset_times } => {
                self.clear();
                if *reset_times {
                    self.times = TitleTimes::default();
                }
            }
            _ => return false,
        }
        true
    }
}

/// The action-bar overlay message. Vanilla shows it for a fixed 60 ticks and
/// fades it over the final ticks, independently of the title system.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActionBar {
    text: Option<Text>,
    remaining: i32,
}

impl ActionBar {
    /// Ticks an action bar message stays before fading (vanilla: 60).
    pub const DISPLAY_TICKS: i32 = 60;
    /// Ticks over which it fades out.
    pub const FADE_TICKS: i32 = 10;

    /// A new empty action bar.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows a message, resetting its timer.
    pub fn set(&mut self, text: Text) {
        self.text = Some(text);
        self.remaining = Self::DISPLAY_TICKS;
    }

    /// The current message, if shown.
    #[must_use]
    pub fn text(&self) -> Option<&Text> {
        self.text.as_ref()
    }

    /// Advances by `ticks`, clearing the message when it expires.
    pub fn tick(&mut self, ticks: i32) {
        if self.text.is_none() {
            return;
        }
        self.remaining -= ticks;
        if self.remaining <= 0 {
            self.text = None;
            self.remaining = 0;
        }
    }

    /// The current opacity, `0.0..=1.0` (fades over the last [`FADE_TICKS`]).
    ///
    /// [`FADE_TICKS`]: Self::FADE_TICKS
    #[must_use]
    pub fn alpha(&self) -> f32 {
        if self.text.is_none() {
            return 0.0;
        }
        if self.remaining >= Self::FADE_TICKS {
            1.0
        } else {
            self.remaining as f32 / Self::FADE_TICKS as f32
        }
    }
}

#[cfg(test)]
mod fold_tests {
    use super::*;

    #[test]
    fn health_fold_sets_each_field_at_its_own_position() {
        let mut hud = HudState::new();
        // Distinct values so a health/food/saturation transposition is caught.
        assert!(hud.apply(&ClientEvent::HealthChanged {
            health: 6.0,
            food: 15,
            saturation: 3.5,
        }));
        assert_eq!(hud.health, 6.0);
        assert_eq!(hud.food, 15);
        assert_eq!(hud.saturation, 3.5);
        assert!(!hud.is_dead());

        assert!(hud.apply(&ClientEvent::HealthChanged {
            health: 0.0,
            food: 0,
            saturation: 0.0,
        }));
        assert!(hud.is_dead());
    }

    #[test]
    fn experience_fold_keeps_level_and_total_distinct() {
        let mut hud = HudState::new();
        assert!(hud.apply(&ClientEvent::ExperienceChanged {
            progress: 0.5,
            level: 7,
            total: 130,
        }));
        assert_eq!(hud.xp_progress, 0.5);
        assert_eq!(hud.xp_level, 7);
        assert_eq!(hud.xp_total, 130);
    }

    #[test]
    fn game_mode_fold_records_previous() {
        let mut hud = HudState::new();
        assert_eq!(hud.game_mode, GameMode::Survival);
        assert!(hud.apply(&ClientEvent::GameModeChanged {
            game_mode: GameMode::Creative,
        }));
        assert_eq!(hud.game_mode, GameMode::Creative);
        assert_eq!(hud.previous_game_mode, Some(GameMode::Survival));
    }

    #[test]
    fn held_slot_fold_rejects_out_of_range() {
        let mut hud = HudState::new();
        assert!(hud.apply(&ClientEvent::HeldSlotChanged { slot: 3 }));
        assert_eq!(hud.selected_slot, 3);
        // Out-of-range values (>= 9, negative, or > u8::MAX) leave it unchanged.
        for bad in [9_i32, -1, 300, i32::MAX] {
            assert!(hud.apply(&ClientEvent::HeldSlotChanged { slot: bad }));
            assert_eq!(hud.selected_slot, 3);
        }
    }

    #[test]
    fn difficulty_fold_sets_value_and_lock() {
        let mut hud = HudState::new();
        assert!(hud.apply(&ClientEvent::DifficultyChanged {
            difficulty: Difficulty::Hard,
            locked: true,
        }));
        assert_eq!(hud.difficulty, Difficulty::Hard);
        assert!(hud.difficulty_locked);
    }

    #[test]
    fn death_fold_marks_dead() {
        let mut hud = HudState::new();
        assert!(hud.apply(&ClientEvent::Death {
            message: Text::literal("slain"),
        }));
        assert!(hud.is_dead());
    }

    #[test]
    fn hud_apply_ignores_unowned_event() {
        let mut hud = HudState::new();
        let before = hud.clone();
        assert!(!hud.apply(&ClientEvent::TitleText {
            text: Text::literal("hi"),
        }));
        assert_eq!(hud, before);
    }

    #[test]
    fn title_and_subtitle_fold() {
        let mut title = TitleState::new();
        assert!(title.apply(&ClientEvent::TitleText {
            text: Text::literal("Round 1"),
        }));
        assert_eq!(title.title(), Some(&Text::literal("Round 1")));
        assert!(title.apply(&ClientEvent::SubtitleText {
            text: Text::literal("Fight!"),
        }));
        assert_eq!(title.subtitle(), Some(&Text::literal("Fight!")));
    }

    #[test]
    fn titles_animation_fold_sets_times() {
        let mut title = TitleState::new();
        assert!(title.apply(&ClientEvent::TitlesAnimation {
            fade_in: 5,
            stay: 40,
            fade_out: 8,
        }));
        title.apply(&ClientEvent::TitleText {
            text: Text::literal("go"),
        });
        assert_eq!(
            title.times,
            TitleTimes {
                fade_in: 5,
                stay: 40,
                fade_out: 8,
            }
        );
    }

    #[test]
    fn titles_cleared_resets_times_only_when_requested() {
        let mut title = TitleState::new();
        title.apply(&ClientEvent::TitlesAnimation {
            fade_in: 5,
            stay: 40,
            fade_out: 8,
        });
        title.apply(&ClientEvent::TitleText {
            text: Text::literal("go"),
        });

        // reset_times: false clears text but keeps custom timings.
        assert!(title.apply(&ClientEvent::TitlesCleared { reset_times: false }));
        assert_eq!(title.title(), None);
        assert_eq!(title.times.stay, 40);

        // reset_times: true restores the vanilla defaults.
        title.apply(&ClientEvent::TitleText {
            text: Text::literal("go"),
        });
        assert!(title.apply(&ClientEvent::TitlesCleared { reset_times: true }));
        assert_eq!(title.times, TitleTimes::default());
    }

    #[test]
    fn title_apply_ignores_unowned_event() {
        let mut title = TitleState::new();
        assert!(!title.apply(&ClientEvent::HealthChanged {
            health: 1.0,
            food: 1,
            saturation: 1.0,
        }));
    }
}
