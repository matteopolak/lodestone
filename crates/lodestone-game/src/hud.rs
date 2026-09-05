//! The HUD read-model: one coherent, version-free snapshot of everything a HUD
//! draws, assembled by borrowing from the individual subsystem states.
//!
//! ## Why a borrowed snapshot
//!
//! A shell/renderer wants *one* type to read each frame, not a dozen internals.
//! But duplicating state into an owned struct every frame is wasteful and
//! invites drift. So [`HudSnapshot`] is a **borrowed view**: [`assemble`]
//! gathers references (and a few cheap derived values) from the authoritative
//! subsystem states the client already owns — [`HudState`], the player
//! [`Menu`], [`ActiveEffects`], [`BossBarSet`], [`Scoreboard`], [`TabList`],
//! [`TitleState`], and [`ActionBar`] — and exposes exactly what a HUD needs.
//! Nothing here reaches into a subsystem's private indices; if the snapshot ever
//! needed something a subsystem didn't expose, that is a seam to widen on the
//! subsystem, not a reason to leak internals.
//!
//! [`assemble`]: HudSnapshot::assemble

use lodestone_model::{Difficulty, GameMode, Text};

use crate::bossbar::{BossBar, BossBarSet};
use crate::effect::{ActiveEffects, StatusEffect};
use crate::item::ItemStack;
use crate::menu::Menu;
use crate::player_state::{ActionBar, HotbarSlot, HudState, TitleState};
use crate::scoreboard::{DisplaySlot, NumberFormat, RenderType, Scoreboard};
use crate::tablist::{PlayerListEntry, TabList};

/// Vanilla renders at most this many rows in the sidebar.
const SIDEBAR_MAX_LINES: usize = 15;

/// The nine hotbar slots plus which one is selected.
#[derive(Debug, Clone)]
pub struct HotbarView<'a> {
    /// Selected hotbar position.
    pub selected: HotbarSlot,
    /// Hotbar contents, index `0..=8` left-to-right.
    pub slots: [Option<&'a ItemStack>; 9],
}

impl HotbarView<'_> {
    /// The stack in the currently selected hotbar slot, if any.
    #[must_use]
    pub fn held(&self) -> Option<&ItemStack> {
        self.slots[self.selected.index()]
    }
}

/// One rendered sidebar row: a fully decorated holder name, its score value, and
/// the number format to apply to that value.
#[derive(Debug, Clone)]
pub struct SidebarLine<'a> {
    /// The holder's display name, already decorated with any team prefix/suffix
    /// and colour (owned, since it is composed on demand).
    pub name: Text,
    /// The score value.
    pub value: i32,
    /// The effective number format (per-score override if set, else the
    /// objective's default).
    pub number_format: &'a NumberFormat,
}

/// The sidebar objective and its rows, highest score first, capped at 15 rows.
#[derive(Debug, Clone)]
pub struct SidebarView<'a> {
    /// The objective's display title.
    pub title: &'a Text,
    /// How scores render (integer or hearts).
    pub render_type: RenderType,
    /// Rows in sidebar order (highest score first), at most 15.
    pub lines: Vec<SidebarLine<'a>>,
}

/// The borrows the HUD read-model is assembled from. The shell holds these
/// authoritative states and passes them in; the snapshot borrows from them.
#[derive(Debug, Clone, Copy)]
pub struct HudInputs<'a> {
    /// Vitals, XP, mode, difficulty, respawn state.
    pub hud: &'a HudState,
    /// The player inventory menu (window 0), source of hotbar contents.
    pub menu: &'a Menu,
    /// Active status effects.
    pub effects: &'a ActiveEffects,
    /// Active boss bars.
    pub boss_bars: &'a BossBarSet,
    /// Scoreboard (for the sidebar).
    pub scoreboard: &'a Scoreboard,
    /// Tab list (header/footer + entries).
    pub tab_list: &'a TabList,
    /// Title/subtitle state.
    pub title: &'a TitleState,
    /// Action-bar state.
    pub action_bar: &'a ActionBar,
}

/// A single coherent, version-free view of everything a HUD draws.
///
/// Produced by [`HudSnapshot::assemble`]; borrows from the subsystem states for
/// the duration `'a`.
#[derive(Debug, Clone)]
pub struct HudSnapshot<'a> {
    // ---- vitals ----
    /// Current health (`20.0` = full).
    pub health: f32,
    /// Food level, `0..=20`.
    pub food: i32,
    /// Food saturation.
    pub saturation: f32,
    /// Remaining air in ticks.
    pub air: i32,
    /// Full air in ticks (draw bubbles only when `air < max_air`).
    pub max_air: i32,
    /// Whether the player is dead (awaiting respawn).
    pub dead: bool,

    // ---- progression ----
    /// Experience level.
    pub xp_level: i32,
    /// Progress to next level, `0.0..=1.0`.
    pub xp_progress: f32,
    /// Total experience points.
    pub xp_total: i32,

    // ---- mode ----
    /// Current game mode.
    pub game_mode: GameMode,
    /// World difficulty.
    pub difficulty: Difficulty,
    /// Whether difficulty is locked.
    pub difficulty_locked: bool,

    // ---- inventory ----
    /// The hotbar and the selected slot.
    pub hotbar: HotbarView<'a>,

    // ---- overlays ----
    /// Active status effects, in insertion order.
    pub effects: Vec<&'a StatusEffect>,
    /// Active boss bars, in insertion order.
    pub boss_bars: Vec<&'a BossBar>,
    /// The sidebar, if an objective is displayed there.
    pub sidebar: Option<SidebarView<'a>>,
    /// Tab-list header, if set.
    pub tab_header: Option<&'a Text>,
    /// Tab-list footer, if set.
    pub tab_footer: Option<&'a Text>,
    /// Tab-list entries in vanilla display order (listed players only).
    pub tab_players: Vec<&'a PlayerListEntry>,

    // ---- transient text ----
    /// The current title text, if any.
    pub title: Option<&'a Text>,
    /// The current subtitle text, if any.
    pub subtitle: Option<&'a Text>,
    /// The title/subtitle fade alpha, `0.0..=1.0` (0 while hidden).
    pub title_alpha: f32,
    /// The current action-bar text, if any.
    pub action_bar: Option<&'a Text>,
    /// The action-bar fade alpha, `0.0..=1.0` (0 while hidden).
    pub action_bar_alpha: f32,
}

impl<'a> HudSnapshot<'a> {
    /// Assembles the read-model by borrowing from the subsystem states.
    #[must_use]
    pub fn assemble(inputs: &HudInputs<'a>) -> Self {
        let hud = inputs.hud;

        let mut slots: [Option<&'a ItemStack>; 9] = [None; 9];
        for (i, slot) in slots.iter_mut().enumerate() {
            *slot = inputs.menu.player_native(i);
        }
        let hotbar = HotbarView {
            selected: hud.selected_slot,
            slots,
        };

        let effects: Vec<&'a StatusEffect> = inputs.effects.iter().collect();
        let boss_bars: Vec<&'a BossBar> = inputs.boss_bars.iter().map(|(_, bar)| bar).collect();
        let sidebar = build_sidebar(inputs.scoreboard);

        let tab_players: Vec<&'a PlayerListEntry> = inputs
            .tab_list
            .ordered()
            .into_iter()
            .filter(|e| e.listed)
            .collect();

        Self {
            health: hud.health,
            food: hud.food,
            saturation: hud.saturation,
            air: hud.air,
            max_air: HudState::MAX_AIR,
            dead: hud.dead || hud.health <= 0.0,
            xp_level: hud.xp_level,
            xp_progress: hud.xp_progress,
            xp_total: hud.xp_total,
            game_mode: hud.game_mode,
            difficulty: hud.difficulty,
            difficulty_locked: hud.difficulty_locked,
            hotbar,
            effects,
            boss_bars,
            sidebar,
            tab_header: inputs.tab_list.header.as_ref(),
            tab_footer: inputs.tab_list.footer.as_ref(),
            tab_players,
            title: inputs.title.title(),
            subtitle: inputs.title.subtitle(),
            title_alpha: inputs.title.alpha(),
            action_bar: inputs.action_bar.text(),
            action_bar_alpha: inputs.action_bar.alpha(),
        }
    }
}

/// Builds the sidebar view from whatever objective occupies the plain sidebar
/// slot. Team-colour-specific sidebars are looked up with
/// [`Scoreboard::sidebar_for_color`] and can be layered by the caller.
fn build_sidebar(scoreboard: &Scoreboard) -> Option<SidebarView<'_>> {
    let objective_name = scoreboard.displayed(DisplaySlot::Sidebar)?;
    let objective = scoreboard.objective(objective_name)?;

    let lines = scoreboard
        .sorted_scores(objective_name)
        .into_iter()
        .take(SIDEBAR_MAX_LINES)
        .map(|(holder, entry)| {
            let number_format = if matches!(entry.number_format, NumberFormat::Default) {
                &objective.number_format
            } else {
                &entry.number_format
            };
            SidebarLine {
                name: scoreboard.display_name_of(holder),
                value: entry.value,
                number_format,
            }
        })
        .collect();

    Some(SidebarView {
        title: &objective.display_name,
        render_type: objective.render_type,
        lines,
    })
}
