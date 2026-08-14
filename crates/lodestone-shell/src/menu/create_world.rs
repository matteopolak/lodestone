//! The World Creation screen (issue #190) — vanilla's `CreateWorldScreen`,
//! reached from [`super::world_select`]'s "Create New World" button, which
//! issue #397 deliberately left present-and-disabled for this issue to build.
//!
//! ## What is and is not vanilla geometry
//!
//! `CreateWorldScreen` is 828 lines with three `GridLayoutTab`s (Game/World/
//! More) inside a `MenuTabBar`, `WorldCreationUiState` (326 lines) tracking a
//! world-type preset list, data packs, game rules and a temp save folder on
//! disk. None of that fits this pipeline or this client: there is still no
//! `LevelStorageSource` (`world_select`'s own module docs), no data-pack
//! loader, and no game-rule model. Building the tab/preset machinery to hold
//! a handful of fields that do get real menu-side support (name, seed, game
//! mode, difficulty, structures, bonus chest, cheats) would be geometry in
//! service of nothing — the same call [`super::key_binds`] and
//! [`super::social`] already made for their own non-`OptionsList` screens,
//! extended to layout instead of to widget shape: **one flat list, hand-
//! placed**, not vanilla's tabs. This is the same legitimate move
//! `docs/ui-framework.md` already names — `TitleScreen` itself uses no
//! layout class and hand-centres — extended one step further, to a screen
//! that skips a *sub-*structure rather than layout entirely.
//!
//! ## Wired vs. decorative
//!
//! - **Wired**: reaching the screen (the "Create New World" button is now
//!   live) and back (Escape/Cancel → [`super::Screen::WorldSelect`]), typing
//!   into the Name/Seed fields (real [`EditBox`]es, the same primitive
//!   [`super::world_select`]'s search field and [`super::nav::EditForm`]
//!   already use), cycling Game Mode/Difficulty and toggling Structures/
//!   Bonus Chest/Allow Cheats (real, in-memory [`WorldCreationConfig`]
//!   state), and the Hardcore→Hard difficulty lock (`GameTab.java`'s own
//!   rule: selecting Hardcore forces and disables the difficulty cycle).
//! - **Wired since — the seed.** This section used to say "nothing
//!   downstream reads any field of it yet"; that queued patch landed
//!   (`72cb451`, `d65d593`). `apply_create_world` turns
//!   [`CreateWorldOutcome::Create`] into `MenuAction::Singleplayer(Some(config))`,
//!   and `app.rs`'s `begin_singleplayer` resolves `config.seed` through
//!   `resolve_launch_seed`/`parse_seed` — vanilla's own
//!   `WorldOptions.parseSeed`/`randomSeed` rule (trim, a valid `i64` literal
//!   used verbatim, free text hashed with Java's `String.hashCode`, empty
//!   means fresh random) — into the `i64`
//!   `lodestone_server::worldgen_data::overworld_chunk_source(seed)` wants,
//!   in place of `BUNDLED_WORLD.seed`. Proved end to end by `app.rs`'s
//!   `resolved_seeds_from_different_world_creation_configs_generate_different_terrain`:
//!   two different typed seeds resolved through the *production* path
//!   generate different real terrain at the same coordinate, and the same
//!   seed reproduces byte-identically — not merely different `i64`s, which
//!   would be the isolated-unit species of this gate.
//! - **Wired since — Online Mode.** Not vanilla (no `CreateWorldScreen`
//!   control ties online-mode to a per-world setting in the real game), and
//!   the one field on this struct that is not merely collected: `true` makes
//!   `begin_singleplayer` open the new world to LAN immediately, with the
//!   real RSA/AES handshake and session-server ownership check
//!   (`lodestone_server::OnlineModeConfig`) running on every connection —
//!   see [`WorldCreationConfig::online_mode`]'s own doc for what it does and
//!   does not cover.
//! - **Decorative — game mode, difficulty, structures, bonus chest and
//!   allow-cheats.** Collected in `WorldCreationConfig` and cycled/toggled for
//!   real, but nothing downstream reads any of them: they need deeper
//!   session-setup wiring (server-side initial state) than the seed's
//!   one-parameter threading, and are left as documented follow-up.
//! - **Decorative — the world name and the "will be saved in" folder.**
//!   There is still no `LevelStorageSource` (`world_select`'s own module
//!   docs, unchanged by this issue), so a name is collected and shown but
//!   nothing is ever written to a folder of that name.

use super::edit_box::EditBox;
use super::focus::{FocusChildren, FocusSet, FocusTarget, KeyEvent, KeyOutcome};
use super::nav::MenuKey;
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot};
use super::widget::Widget;

// -- vanilla captions, verbatim from en_us.json --------------------------

/// `selectWorld.enterName`.
pub const NAME_LABEL: &str = "World Name";
/// `selectWorld.newWorld` — the default value, not a hint.
pub const DEFAULT_NAME: &str = "New World";
/// `selectWorld.enterSeed` — the seed field's own visible label, drawn above
/// it exactly like [`NAME_LABEL`] (`CreateWorldScreen.java`, via
/// `CommonLayouts.labeledElement`).
pub const SEED_LABEL: &str = "Seed for the world generator";
/// `selectWorld.seedInfo` — the seed field's `EditBox.hint` ghost text,
/// shown only while the box is empty and unfocused
/// (`CreateWorldScreen.java`). Not a second permanent label; see
/// [`frame`]'s own doc on the notice this constant used to also feed.
pub const SEED_INFO: &str = "Leave blank for a random seed";
/// `selectWorld.gameMode` / `selectWorld.mapFeatures` / `options.difficulty`
/// / `selectWorld.bonusItems` / `selectWorld.allowCommands`.
pub const GAME_MODE_LABEL: &str = "Game Mode";
pub const DIFFICULTY_LABEL: &str = "Difficulty";
pub const STRUCTURES_LABEL: &str = "Generate Structures";
pub const BONUS_CHEST_LABEL: &str = "Bonus Chest";
pub const ALLOW_CHEATS_LABEL: &str = "Allow Cheats";
/// Not a vanilla caption — there is no vanilla `CreateWorldScreen` control for
/// this, because real Minecraft ties online-mode to the account you are
/// signed in with rather than to a per-world creation setting. See
/// [`WorldCreationConfig::online_mode`] for what this actually does.
pub const ONLINE_MODE_LABEL: &str = "Online Mode (Open to LAN)";
/// `selectWorld.create`, reused verbatim for this screen's own submit button
/// — vanilla uses the same string for both (`CreateWorldScreen.java`'s
/// `createButton`, `:145-149`).
pub const CREATE_LABEL: &str = "Create New World";
pub const CANCEL_LABEL: &str = "Cancel";

/// `WorldCreationUiState.SelectedGameMode`, narrowed to the three a player
/// actually picks from this button (`GameTab.java`'s own cycle — `DEBUG`,
/// vanilla's fourth value, is not offered here; `SelectedGameMode.java`'s
/// own caption for it is literally "spectator", which is not a serious
/// creation-time choice and this button does not cycle to it, matching the
/// real client).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorldGameMode {
    #[default]
    Survival,
    Creative,
    Hardcore,
}

impl WorldGameMode {
    #[must_use]
    pub fn caption(self) -> &'static str {
        match self {
            WorldGameMode::Survival => "Survival",
            WorldGameMode::Creative => "Creative",
            WorldGameMode::Hardcore => "Hardcore",
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            WorldGameMode::Survival => WorldGameMode::Creative,
            WorldGameMode::Creative => WorldGameMode::Hardcore,
            WorldGameMode::Hardcore => WorldGameMode::Survival,
        }
    }
}

/// Reuses [`lodestone_model::common::Difficulty`] directly rather than a
/// narrowed local copy (unlike [`WorldGameMode`]) — every vanilla difficulty
/// is a legitimate creation-time choice, so there is nothing to narrow.
pub use lodestone_model::common::Difficulty as WorldDifficulty;

#[must_use]
fn difficulty_caption(d: WorldDifficulty) -> &'static str {
    match d {
        WorldDifficulty::Peaceful => "Peaceful",
        WorldDifficulty::Easy => "Easy",
        WorldDifficulty::Normal => "Normal",
        WorldDifficulty::Hard => "Hard",
    }
}

#[must_use]
fn next_difficulty(d: WorldDifficulty) -> WorldDifficulty {
    match d {
        WorldDifficulty::Peaceful => WorldDifficulty::Easy,
        WorldDifficulty::Easy => WorldDifficulty::Normal,
        WorldDifficulty::Normal => WorldDifficulty::Hard,
        WorldDifficulty::Hard => WorldDifficulty::Peaceful,
    }
}

/// The fields this screen collects — [`CreateWorldOutcome::Create`]'s
/// payload. Not yet consumed by anything downstream; see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldCreationConfig {
    pub name: String,
    /// The typed seed text, verbatim — empty means "random", matching
    /// vanilla's own `WorldOptions.defaultWithRandomSeed()` branch
    /// (`selectWorld.seedInfo`). Parsing an empty/non-numeric seed into an
    /// actual `i64` is the consuming patch's job (see the module docs), not
    /// this screen's: vanilla itself accepts non-numeric seed text and
    /// hashes it (`WorldOptions.java`'s own `parseSeed`), which this menu
    /// layer has no reason to reimplement ahead of a consumer that needs it.
    pub seed: String,
    pub game_mode: WorldGameMode,
    pub difficulty: WorldDifficulty,
    pub generate_structures: bool,
    pub bonus_chest: bool,
    pub allow_cheats: bool,
    /// **Wired, not decorative** (issue #273's shell-side control) — unlike
    /// every other field on this struct: `true` makes `app.rs`'s
    /// `begin_singleplayer` open this world to LAN on an OS-assigned port
    /// *immediately*, with the real RSA/AES handshake and session-server
    /// ownership check running on every connection it accepts (including the
    /// host's own loopback join — see `net.rs`'s `open_lan_world` match arm on
    /// `(None, Some(address))`), instead of the ordinary in-memory
    /// singleplayer session `open_singleplayer` starts for every other world.
    ///
    /// `false` — the default — changes nothing: the world opens exactly as it
    /// always has, with no listener and no network call.
    ///
    /// Only reachable from **Create New World**, not **Play Selected World**:
    /// `SingleplayerLaunch::Open` carries no `WorldCreationConfig` to hold
    /// this on. A world created without it can still be published later
    /// through the pause menu's existing Open to LAN button, just not with
    /// online mode — that path calls `IntegratedServer::publish`, which has
    /// no online-mode parameter.
    pub online_mode: bool,
}

impl Default for WorldCreationConfig {
    fn default() -> Self {
        Self {
            name: DEFAULT_NAME.to_string(),
            seed: String::new(),
            game_mode: WorldGameMode::default(),
            // `Difficulty.NORMAL` — `WorldCreationUiState.java`.
            difficulty: WorldDifficulty::Normal,
            // `WorldOptions`' own defaults — `generateStructures` true,
            // `generateBonusChest` false (`WorldCreationUiState.java`
            // reads these off `settings.options()`, whose defaults are
            // vanilla's `WorldOptions.defaultWithRandomSeed()`).
            generate_structures: true,
            bonus_chest: false,
            allow_cheats: false,
            online_mode: false,
        }
    }
}

// -- focus row ids -----------------------------------------------------------

pub const NAME_FIELD: usize = 0;
pub const SEED_FIELD: usize = 1;
pub const GAME_MODE_ROW: usize = 2;
pub const DIFFICULTY_ROW: usize = 3;
pub const STRUCTURES_ROW: usize = 4;
pub const BONUS_CHEST_ROW: usize = 5;
pub const ALLOW_CHEATS_ROW: usize = 6;
pub const ONLINE_MODE_ROW: usize = 7;
pub const CREATE_ROW: usize = 8;
pub const CANCEL_ROW: usize = 9;
const ROW_COUNT: usize = 10;

const SEED_CANVAS: (f32, f32) = (854.0, 480.0);

/// Every row's rect, hand-placed (see the module docs). Two text fields, five
/// button-shaped rows, a two-button footer — [`row_slot`] is the single
/// definition every one of `Self::adding`'s seeded rects,
/// [`super::render`]'s draw and `app.rs`'s hit-test all read, so they cannot
/// drift apart the way a restated constant could.
#[must_use]
pub fn row_slot(row: usize) -> Slot {
    const FIELD_W: f32 = 200.0;
    const X: f32 = -(FIELD_W / 2.0);
    const TOP: f32 = 32.0;
    const ROW_H: f32 = 22.0;
    match row {
        NAME_FIELD => Slot { origin: Origin::ScreenTop, dx: X, dy: TOP, w: FIELD_W, h: super::render::EDIT_BOX_H },
        SEED_FIELD => Slot { origin: Origin::ScreenTop, dx: X, dy: TOP + ROW_H, w: FIELD_W, h: super::render::EDIT_BOX_H },
        GAME_MODE_ROW => Slot { origin: Origin::ScreenTop, dx: X, dy: TOP + ROW_H * 2.0 + 8.0, w: FIELD_W, h: super::render::EDIT_BOX_H },
        DIFFICULTY_ROW => Slot { origin: Origin::ScreenTop, dx: X, dy: TOP + ROW_H * 3.0 + 8.0, w: FIELD_W, h: super::render::EDIT_BOX_H },
        STRUCTURES_ROW => Slot { origin: Origin::ScreenTop, dx: X, dy: TOP + ROW_H * 4.0 + 8.0, w: FIELD_W, h: super::render::EDIT_BOX_H },
        BONUS_CHEST_ROW => Slot { origin: Origin::ScreenTop, dx: X, dy: TOP + ROW_H * 5.0 + 8.0, w: FIELD_W, h: super::render::EDIT_BOX_H },
        ALLOW_CHEATS_ROW => Slot { origin: Origin::ScreenTop, dx: X, dy: TOP + ROW_H * 6.0 + 8.0, w: FIELD_W, h: super::render::EDIT_BOX_H },
        ONLINE_MODE_ROW => Slot { origin: Origin::ScreenTop, dx: X, dy: TOP + ROW_H * 7.0 + 8.0, w: FIELD_W, h: super::render::EDIT_BOX_H },
        CREATE_ROW => Slot {
            origin: Origin::Settings(super::options::Placement::Footer { index: 0, count: 2 }),
            dx: 0.0,
            dy: 0.0,
            w: super::options::SMALL_BUTTON_WIDTH,
            h: super::options::WIDGET_H,
        },
        _ => Slot {
            origin: Origin::Settings(super::options::Placement::Footer { index: 1, count: 2 }),
            dx: 0.0,
            dy: 0.0,
            w: super::options::SMALL_BUTTON_WIDTH,
            h: super::options::WIDGET_H,
        },
    }
}

/// The two [`EditBox`]es plus the five cycle/toggle/submit [`Widget`]s, one
/// struct so [`FocusSet`] can borrow them while [`CreateWorldNav`] borrows
/// the set — the same split [`super::nav::FormFields`] and
/// [`super::world_select::WorldSelectWidgets`] already use, for the same
/// reason (`FocusSet`'s methods take `&mut dyn FocusChildren`).
#[derive(Debug, Clone, PartialEq)]
pub struct CreateWorldWidgets {
    pub name: EditBox,
    pub seed: EditBox,
    pub game_mode: Widget,
    pub difficulty: Widget,
    pub structures: Widget,
    pub bonus_chest: Widget,
    pub allow_cheats: Widget,
    pub online_mode: Widget,
    pub create: Widget,
    pub cancel: Widget,
}

impl FocusChildren for CreateWorldWidgets {
    fn get(&self, id: usize) -> Option<&dyn FocusTarget> {
        Some(match id {
            NAME_FIELD => &self.name as &dyn FocusTarget,
            SEED_FIELD => &self.seed as &dyn FocusTarget,
            GAME_MODE_ROW => &self.game_mode as &dyn FocusTarget,
            DIFFICULTY_ROW => &self.difficulty as &dyn FocusTarget,
            STRUCTURES_ROW => &self.structures as &dyn FocusTarget,
            BONUS_CHEST_ROW => &self.bonus_chest as &dyn FocusTarget,
            ALLOW_CHEATS_ROW => &self.allow_cheats as &dyn FocusTarget,
            ONLINE_MODE_ROW => &self.online_mode as &dyn FocusTarget,
            CREATE_ROW => &self.create as &dyn FocusTarget,
            CANCEL_ROW => &self.cancel as &dyn FocusTarget,
            _ => return None,
        })
    }

    fn get_mut(&mut self, id: usize) -> Option<&mut dyn FocusTarget> {
        Some(match id {
            NAME_FIELD => &mut self.name as &mut dyn FocusTarget,
            SEED_FIELD => &mut self.seed as &mut dyn FocusTarget,
            GAME_MODE_ROW => &mut self.game_mode as &mut dyn FocusTarget,
            DIFFICULTY_ROW => &mut self.difficulty as &mut dyn FocusTarget,
            STRUCTURES_ROW => &mut self.structures as &mut dyn FocusTarget,
            BONUS_CHEST_ROW => &mut self.bonus_chest as &mut dyn FocusTarget,
            ALLOW_CHEATS_ROW => &mut self.allow_cheats as &mut dyn FocusTarget,
            ONLINE_MODE_ROW => &mut self.online_mode as &mut dyn FocusTarget,
            CREATE_ROW => &mut self.create as &mut dyn FocusTarget,
            CANCEL_ROW => &mut self.cancel as &mut dyn FocusTarget,
            _ => return None,
        })
    }
}

/// What one key or click did to the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateWorldOutcome {
    Handled,
    Cancel,
    /// Pressing Create — carries the [`WorldCreationConfig`] the player
    /// collected, for `menu/nav.rs`'s `apply_create_world` to hand to
    /// `MenuAction::Singleplayer` (issue #190). Not `Copy` any more:
    /// `WorldCreationConfig` carries a `String` (the world name and the
    /// typed seed text), which `Handled`/`Cancel` never needed.
    Create(WorldCreationConfig),
}

/// This screen's live state: its widgets, its focus, and the config they
/// collect.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateWorldNav {
    pub widgets: CreateWorldWidgets,
    focus: FocusSet,
    config: WorldCreationConfig,
    /// Which button row the mouse is over, if any — separate from keyboard
    /// focus for [`super::nav::EditForm::hovered`]'s exact reason (this
    /// screen has the same shape: two [`EditBox`]es plus five button rows).
    ///
    /// **This is the whole of issue #567's reported bug.** `CreateWorld`
    /// already reaches [`super::render::stamp_canvas_facts`] through
    /// `render::frame_for`'s own `frame.map` — every screen's frame does,
    /// unconditionally, unless its arm returns `None` the way in-world
    /// `Settings` deliberately does — so `cursor`/`gui_scale`/
    /// `panorama_speed`/`list` were never the gap here, unlike the two
    /// earlier instances of this defect shape. What was missing is this
    /// field: nothing on this screen ever recorded *which* row the cursor
    /// was over, so [`frame`]'s `MenuFrame::hovered` stayed `None`
    /// unconditionally and `widget.hovered` (`render::draw_widget`) was
    /// `false` for every row, every frame — no outline, ever, regardless of
    /// where the mouse was.
    hovered: Option<usize>,
}

fn button(row: usize, label: impl Into<String>) -> Widget {
    let (x, y, w, h) = row_slot(row).resolve(SEED_CANVAS.0, SEED_CANVAS.1);
    Widget::new(x, y, w, h, label)
}

impl CreateWorldNav {
    /// A fresh screen at vanilla's own defaults — called every time the
    /// screen is opened (`CreateWorldScreen.openFresh`'s own state, not
    /// resumed from a previous visit), matching every other "reset on entry"
    /// screen in this tree.
    #[must_use]
    pub fn new() -> Self {
        let config = WorldCreationConfig::default();
        let (nx, ny, nw, nh) = row_slot(NAME_FIELD).resolve(SEED_CANVAS.0, SEED_CANVAS.1);
        let (sx, sy, sw, sh) = row_slot(SEED_FIELD).resolve(SEED_CANVAS.0, SEED_CANVAS.1);
        let mut name = EditBox::new(nx, ny, nw, nh, NAME_LABEL);
        name.set_value(&config.name);
        let mut seed = EditBox::new(sx, sy, sw, sh, SEED_LABEL);
        seed.hint = Some(SEED_INFO.to_string());
        let mut widgets = CreateWorldWidgets {
            name,
            seed,
            game_mode: button(GAME_MODE_ROW, cycle_label(GAME_MODE_LABEL, config.game_mode.caption())),
            difficulty: button(
                DIFFICULTY_ROW,
                cycle_label(DIFFICULTY_LABEL, difficulty_caption(config.difficulty)),
            ),
            structures: button(STRUCTURES_ROW, toggle_label(STRUCTURES_LABEL, config.generate_structures)),
            bonus_chest: button(BONUS_CHEST_ROW, toggle_label(BONUS_CHEST_LABEL, config.bonus_chest)),
            allow_cheats: button(ALLOW_CHEATS_ROW, toggle_label(ALLOW_CHEATS_LABEL, config.allow_cheats)),
            online_mode: button(ONLINE_MODE_ROW, toggle_label(ONLINE_MODE_LABEL, config.online_mode)),
            create: button(CREATE_ROW, CREATE_LABEL),
            cancel: button(CANCEL_ROW, CANCEL_LABEL),
        };
        let mut focus = FocusSet::new();
        for row in 0..ROW_COUNT {
            focus.add_renderable_widget(row);
        }
        focus.set_initial_focus(&mut widgets, NAME_FIELD);
        Self {
            widgets,
            focus,
            config,
            hovered: None,
        }
    }

    #[must_use]
    pub fn config(&self) -> &WorldCreationConfig {
        &self.config
    }

    #[must_use]
    pub fn focused(&self) -> Option<usize> {
        self.focus.focused()
    }

    /// The mouse moved over row `row` — mirrors
    /// [`super::nav::EditForm::hover_row`] exactly, including its reason: a
    /// field row (`NAME_FIELD`/`SEED_FIELD`) does nothing here, so hovering
    /// the Seed field while typing in Name cannot steal the caret out from
    /// under the player (vanilla's `ContainerEventHandler` moves focus only
    /// from a *click* or Tab traversal, never from hover — `EditBox` itself
    /// has no hover highlight at all). Every other row records only
    /// [`Self::hovered`], which is what lets the mouse travel to Create
    /// without touching whichever field currently has the keyboard.
    pub fn hover_row(&mut self, row: usize) {
        match row {
            NAME_FIELD | SEED_FIELD => {}
            GAME_MODE_ROW | DIFFICULTY_ROW | STRUCTURES_ROW | BONUS_CHEST_ROW
            | ALLOW_CHEATS_ROW | ONLINE_MODE_ROW | CREATE_ROW | CANCEL_ROW => {
                self.hovered = Some(row);
            }
            _ => {}
        }
    }

    /// The row the mouse is over, for [`super::render::MenuFrame::hovered`].
    #[must_use]
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// Difficulty is locked to Hard and its own row inactive while Hardcore
    /// is selected — `GameTab.java`'s own rule (selecting Hardcore forces
    /// and disables the difficulty cycle; every other mode leaves it live).
    fn apply_hardcore_lock(&mut self) {
        let hardcore = self.config.game_mode == WorldGameMode::Hardcore;
        if hardcore {
            self.config.difficulty = WorldDifficulty::Hard;
        }
        self.widgets.difficulty.active = !hardcore;
        self.widgets.difficulty.message =
            cycle_label(DIFFICULTY_LABEL, difficulty_caption(self.config.difficulty));
    }

    fn refresh_labels(&mut self) {
        self.widgets.game_mode.message = cycle_label(GAME_MODE_LABEL, self.config.game_mode.caption());
        self.widgets.structures.message = toggle_label(STRUCTURES_LABEL, self.config.generate_structures);
        self.widgets.bonus_chest.message = toggle_label(BONUS_CHEST_LABEL, self.config.bonus_chest);
        self.widgets.allow_cheats.message = toggle_label(ALLOW_CHEATS_LABEL, self.config.allow_cheats);
        self.widgets.online_mode.message = toggle_label(ONLINE_MODE_LABEL, self.config.online_mode);
        self.apply_hardcore_lock();
    }

    fn activate(&mut self, row: usize) -> CreateWorldOutcome {
        match row {
            GAME_MODE_ROW => {
                self.config.game_mode = self.config.game_mode.next();
                self.refresh_labels();
                CreateWorldOutcome::Handled
            }
            DIFFICULTY_ROW => {
                if self.widgets.difficulty.active {
                    self.config.difficulty = next_difficulty(self.config.difficulty);
                    self.refresh_labels();
                }
                CreateWorldOutcome::Handled
            }
            STRUCTURES_ROW => {
                self.config.generate_structures = !self.config.generate_structures;
                self.refresh_labels();
                CreateWorldOutcome::Handled
            }
            BONUS_CHEST_ROW => {
                self.config.bonus_chest = !self.config.bonus_chest;
                self.refresh_labels();
                CreateWorldOutcome::Handled
            }
            ALLOW_CHEATS_ROW => {
                self.config.allow_cheats = !self.config.allow_cheats;
                self.refresh_labels();
                CreateWorldOutcome::Handled
            }
            ONLINE_MODE_ROW => {
                self.config.online_mode = !self.config.online_mode;
                self.refresh_labels();
                CreateWorldOutcome::Handled
            }
            CREATE_ROW => {
                self.config.name = self.widgets.name.value().to_string();
                self.config.seed = self.widgets.seed.value().to_string();
                CreateWorldOutcome::Create(self.config.clone())
            }
            CANCEL_ROW => CreateWorldOutcome::Cancel,
            _ => CreateWorldOutcome::Handled,
        }
    }

    /// A click on row `row` — mirrors [`super::world_select::WorldSelectNav::click_row`]'s
    /// own reasoning (#391's shape): a click focuses a field or presses a
    /// button, and neither is "hover then Enter".
    pub fn click_row(&mut self, row: usize) -> CreateWorldOutcome {
        if row == NAME_FIELD || row == SEED_FIELD {
            self.focus.set_focused(&mut self.widgets, Some(row));
            return CreateWorldOutcome::Handled;
        }
        let active = self
            .widgets
            .get(row)
            .is_some_and(super::focus::FocusTarget::is_active);
        if !active {
            return CreateWorldOutcome::Handled;
        }
        self.focus.set_focused(&mut self.widgets, Some(row));
        self.activate(row)
    }

    /// One key, routed through the same `Escape` → field → navigation →
    /// screen order [`super::nav::EditForm::handle_key`] already documents
    /// and cites `Screen.keyPressed`'s own order for.
    pub fn handle_key(&mut self, key: MenuKey) -> CreateWorldOutcome {
        if key == MenuKey::Escape {
            return CreateWorldOutcome::Cancel;
        }
        if let MenuKey::Char(ch) = key {
            self.focus.char_typed(&mut self.widgets, ch);
            return CreateWorldOutcome::Handled;
        }
        let Some(event) = KeyEvent::from_menu_key(key) else {
            return CreateWorldOutcome::Handled;
        };
        match self.focus.screen_key_pressed(&mut self.widgets, event) {
            KeyOutcome::Close => CreateWorldOutcome::Cancel,
            KeyOutcome::Consumed | KeyOutcome::FocusMoved => CreateWorldOutcome::Handled,
            KeyOutcome::Declined if key == MenuKey::Enter => {
                match self.focus.focused() {
                    Some(row) => self.activate(row),
                    None => CreateWorldOutcome::Handled,
                }
            }
            KeyOutcome::Declined => CreateWorldOutcome::Handled,
        }
    }
}

impl Default for CreateWorldNav {
    fn default() -> Self {
        Self::new()
    }
}

fn cycle_label(caption: &str, value: &str) -> String {
    format!("{caption}: {value}")
}

fn toggle_label(caption: &str, on: bool) -> String {
    format!("{caption}: {}", if on { "ON" } else { "OFF" })
}

/// Builds the whole World Creation frame.
///
/// ## The Name/Seed fields used to reach no pixels at all
///
/// `rows` used to start at [`GAME_MODE_ROW`] and never mention
/// [`NAME_FIELD`]/[`SEED_FIELD`] — an island of exactly the shape `CLAUDE.md`
/// warns about: [`CreateWorldWidgets::name`]/`seed` are real, focusable,
/// typeable `EditBox`es (every test above this function drives them and
/// passes), but nothing in the render output ever carried one, so the screen
/// drew two floating labels over an otherwise blank strip and the actual
/// boxes — background, typed text, caret, hint — never appeared. Fixed by
/// giving each field its own [`MenuRow`] with a real `edit: Some(..)`, the
/// same shape [`super::Screen::ServerEdit`]'s form already uses.
#[must_use]
pub fn frame(nav: &CreateWorldNav) -> MenuFrame<'static> {
    let focused = nav.focused();
    let widget_row = |w: &Widget, row: usize| MenuRow {
        label: w.message.clone(),
        enabled: w.active,
        slot: Some(row_slot(row)),
        ..Default::default()
    };
    // Mirrors `Screen::ServerEdit`'s own field rows (`render.rs`'s
    // `manage_server_slot` arm): a live `EditBox` clone, `field: true`, and
    // `slot` rather than the generic stack — `draw_edit_box` reads the box's
    // own state (value, caret, selection, hint) and decides nothing here.
    let field_row = |edit: &EditBox, row: usize| MenuRow {
        label: edit.value().to_string(),
        enabled: true,
        field: true,
        edit: Some(edit.clone()),
        slot: Some(row_slot(row)),
        ..Default::default()
    };

    let rows = vec![
        field_row(&nav.widgets.name, NAME_FIELD),
        field_row(&nav.widgets.seed, SEED_FIELD),
        widget_row(&nav.widgets.game_mode, GAME_MODE_ROW),
        widget_row(&nav.widgets.difficulty, DIFFICULTY_ROW),
        widget_row(&nav.widgets.structures, STRUCTURES_ROW),
        widget_row(&nav.widgets.bonus_chest, BONUS_CHEST_ROW),
        widget_row(&nav.widgets.allow_cheats, ALLOW_CHEATS_ROW),
        widget_row(&nav.widgets.online_mode, ONLINE_MODE_ROW),
        widget_row(&nav.widgets.create, CREATE_ROW),
        widget_row(&nav.widgets.cancel, CANCEL_ROW),
    ];
    // Every focus id used above (`NAME_FIELD == 0` through `CANCEL_ROW == 9`,
    // see their own doc comments) is *also* its row's index in `rows`, so the
    // row cursor and the widget cursor are one number — no restated list to
    // drift from it. A field row's own highlight is its caret, drawn by
    // `draw_edit_box` off the box's `focused` flag rather than off
    // `MenuFrame::selected`, so this needs no special case for it the way the
    // pre-fix version did.
    let selected = focused.unwrap_or(usize::MAX);

    MenuFrame {
        rows,
        selected,
        // Issue #567: this used to be left at its `..Default::default()` of
        // `None` unconditionally — nothing on this screen ever recorded which
        // row the mouse was over (see `CreateWorldNav::hovered`'s own doc) —
        // so `render::draw_widget`'s `widget.hovered` was `false` for every
        // row, every frame, and no button ever drew its hover outline.
        hovered: nav.hovered(),
        vanilla: true,
        labels: vec![
            MenuLabel {
                text: "Create New World".to_string(), // selectWorld.title vanilla reuses selectWorld.create as this screen's own heading (`CreateWorldScreen.java`'s `TITLE`, `Component.translatable("selectWorld.create")`).
                origin: Origin::ScreenTop,
                dx: 0.0,
                dy: 12.0,
                align: Align::Centre,
                colour: super::widget::ACTIVE_LABEL,
                scale: 1.0,
            },
            // `CommonLayouts.labeledElement` draws a real, visible label above
            // each field in vanilla (`CreateWorldScreen.java`) — this is
            // that label, not narration. `10.0` above the field's own `dy`
            // mirrors the offset already used for the name field below.
            MenuLabel {
                text: NAME_LABEL.to_string(),
                origin: Origin::ScreenTop,
                dx: -100.0,
                dy: 22.0,
                align: Align::Left,
                colour: super::widget::ACTIVE_LABEL,
                scale: 1.0,
            },
            // The seed field's own visible label — `SEED_LABEL`
            // (`selectWorld.enterSeed`, "Seed for the world generator"). This
            // used to be missing entirely: only the *hint* text
            // (`SEED_INFO`/`selectWorld.seedInfo`) was drawn, and as a
            // permanent notice rather than vanilla's `EditBox.hint` ghost text
            // — see the removed `notice` field below.
            MenuLabel {
                text: SEED_LABEL.to_string(),
                origin: Origin::ScreenTop,
                dx: -100.0,
                dy: 44.0,
                align: Align::Left,
                colour: super::widget::ACTIVE_LABEL,
                scale: 1.0,
            },
        ],
        // No `notice` here any more. Vanilla shows `SEED_INFO` in exactly one
        // place — `seedEdit.setHint(SEED_EMPTY_HINT)`
        // (`CreateWorldScreen.java`), ghost text drawn only while the box
        // is empty and unfocused. `CreateWorldNav::new` already sets
        // `seed.hint`, so a permanent notice here was drawing the same string
        // vanilla only ever shows conditionally — a duplicate, not a second
        // real label.
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_vanillas_own() {
        let config = WorldCreationConfig::default();
        assert_eq!(config.name, "New World");
        assert_eq!(config.seed, "");
        assert_eq!(config.game_mode, WorldGameMode::Survival);
        assert_eq!(config.difficulty, WorldDifficulty::Normal);
        assert!(config.generate_structures);
        assert!(!config.bonus_chest);
        assert!(!config.allow_cheats);
        assert!(!config.online_mode);
    }

    #[test]
    fn a_fresh_nav_starts_focused_on_the_name_field_with_the_default_value() {
        let nav = CreateWorldNav::new();
        assert_eq!(nav.focused(), Some(NAME_FIELD));
        assert_eq!(nav.widgets.name.value(), "New World");
        assert_eq!(nav.widgets.seed.value(), "");
    }

    #[test]
    fn typing_reaches_the_focused_field() {
        let mut nav = CreateWorldNav::new();
        // Clear the default and type a real name.
        for _ in 0.."New World".len() {
            nav.handle_key(MenuKey::Backspace);
        }
        for ch in "My World".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        assert_eq!(nav.widgets.name.value(), "My World");

        nav.handle_key(MenuKey::Tab);
        assert_eq!(nav.focused(), Some(SEED_FIELD));
        for ch in "12345".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        assert_eq!(nav.widgets.seed.value(), "12345");
        // The name field must be untouched by typing into the seed field.
        assert_eq!(nav.widgets.name.value(), "My World");
    }

    #[test]
    fn game_mode_cycles_through_all_three_and_wraps() {
        let mut nav = CreateWorldNav::new();
        assert_eq!(nav.config().game_mode, WorldGameMode::Survival);
        nav.click_row(GAME_MODE_ROW);
        assert_eq!(nav.config().game_mode, WorldGameMode::Creative);
        nav.click_row(GAME_MODE_ROW);
        assert_eq!(nav.config().game_mode, WorldGameMode::Hardcore);
        nav.click_row(GAME_MODE_ROW);
        assert_eq!(nav.config().game_mode, WorldGameMode::Survival, "wraps");
    }

    #[test]
    fn selecting_hardcore_locks_difficulty_to_hard_and_disables_its_row() {
        let mut nav = CreateWorldNav::new();
        // Move difficulty to a value that is *not* Hard first, so the
        // "forced" assertion below is meaningful — it would fail if
        // `apply_hardcore_lock` did nothing, rather than passing by
        // coincidence because the default already happened to be Hard.
        nav.click_row(DIFFICULTY_ROW); // Normal -> Hard
        nav.click_row(DIFFICULTY_ROW); // Hard -> Peaceful (wraps)
        nav.click_row(DIFFICULTY_ROW); // Peaceful -> Easy
        assert_eq!(nav.config().difficulty, WorldDifficulty::Easy);

        nav.click_row(GAME_MODE_ROW); // Survival -> Creative
        nav.click_row(GAME_MODE_ROW); // Creative -> Hardcore
        assert_eq!(nav.config().difficulty, WorldDifficulty::Hard, "forced");
        assert!(!nav.widgets.difficulty.active, "row must be inactive while locked");

        // Clicking a disabled row does nothing — the same rule every other
        // present-and-disabled control in this tree follows.
        nav.click_row(DIFFICULTY_ROW);
        assert_eq!(nav.config().difficulty, WorldDifficulty::Hard, "unchanged");

        // Leaving Hardcore unlocks it again, at whatever it was left on.
        nav.click_row(GAME_MODE_ROW); // Hardcore -> Survival
        assert!(nav.widgets.difficulty.active, "unlocked outside Hardcore");
    }

    #[test]
    fn the_three_toggles_flip_independently() {
        let mut nav = CreateWorldNav::new();
        assert!(nav.config().generate_structures);
        nav.click_row(STRUCTURES_ROW);
        assert!(!nav.config().generate_structures);
        assert!(!nav.config().bonus_chest, "untouched");
        assert!(!nav.config().allow_cheats, "untouched");

        nav.click_row(BONUS_CHEST_ROW);
        assert!(nav.config().bonus_chest);
        nav.click_row(ALLOW_CHEATS_ROW);
        assert!(nav.config().allow_cheats);
        assert!(!nav.config().generate_structures, "still off from the first click");
    }

    /// Same shape as `the_three_toggles_flip_independently`, kept separate
    /// because `online_mode` is wired (see its own doc) rather than
    /// decorative like the other three — this is the pair the toggle's own
    /// gate needs: the default stays off, and clicking flips only this field.
    #[test]
    fn online_mode_defaults_off_and_toggles_independently() {
        let mut nav = CreateWorldNav::new();
        assert!(!nav.config().online_mode);
        nav.click_row(ONLINE_MODE_ROW);
        assert!(nav.config().online_mode);
        assert!(!nav.config().allow_cheats, "neighbour untouched");
        assert!(!nav.config().bonus_chest, "neighbour untouched");
        assert!(nav.config().generate_structures, "neighbour untouched (default on)");

        nav.click_row(ONLINE_MODE_ROW);
        assert!(!nav.config().online_mode, "toggles back off");
    }

    #[test]
    fn create_carries_the_typed_name_and_seed() {
        let mut nav = CreateWorldNav::new();
        for _ in 0.."New World".len() {
            nav.handle_key(MenuKey::Backspace);
        }
        for ch in "Overworld".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        nav.handle_key(MenuKey::Tab);
        for ch in "42".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        let outcome = nav.click_row(CREATE_ROW);
        let CreateWorldOutcome::Create(config) = outcome else {
            panic!("expected CreateWorldOutcome::Create, got {outcome:?}");
        };
        assert_eq!(config.name, "Overworld");
        assert_eq!(config.seed, "42");
        assert_eq!(nav.config().name, "Overworld");
        assert_eq!(nav.config().seed, "42");
    }

    #[test]
    fn cancel_and_escape_both_ask_to_leave() {
        let mut nav = CreateWorldNav::new();
        assert_eq!(nav.click_row(CANCEL_ROW), CreateWorldOutcome::Cancel);

        let mut nav2 = CreateWorldNav::new();
        assert_eq!(nav2.handle_key(MenuKey::Escape), CreateWorldOutcome::Cancel);
    }

    #[test]
    fn a_click_acts_on_the_row_it_landed_on_and_nothing_else() {
        // #391's shape, on this screen too: clicking Structures must not
        // touch Bonus Chest.
        let mut nav = CreateWorldNav::new();
        nav.click_row(STRUCTURES_ROW);
        assert!(!nav.config().generate_structures);
        assert!(!nav.config().bonus_chest, "neighbour untouched");
    }

    #[test]
    fn the_name_and_seed_fields_reach_the_frame_as_real_edit_boxes() {
        // The island this fixes: `frame()` used to build its `rows` starting
        // at `GAME_MODE_ROW`, so neither field ever carried an `edit` and
        // `draw_edit_box` never ran for either of them — the boxes were
        // focusable and typeable in every test above, and invisible on
        // screen. Positive assertion on the fields, negative control on a
        // button row right next to them, per `CLAUDE.md`'s "a gate that only
        // checks the border exists would have passed while this bug shipped"
        // — the control here is the thing that would have caught it.
        let mut nav = CreateWorldNav::new();
        nav.widgets.seed.set_value("1234");
        let f = frame(&nav);
        assert_eq!(f.rows.len(), ROW_COUNT, "one row per focus id, 1:1");
        assert!(f.rows[NAME_FIELD].field, "row 0 is a text field");
        assert!(f.rows[SEED_FIELD].field, "row 1 is a text field");
        let name_edit = f.rows[NAME_FIELD]
            .edit
            .as_ref()
            .expect("the name row must carry its EditBox, or nothing draws");
        assert_eq!(name_edit.value(), "New World");
        let seed_edit = f.rows[SEED_FIELD]
            .edit
            .as_ref()
            .expect("the seed row must carry its EditBox, or nothing draws");
        assert_eq!(seed_edit.value(), "1234");
        // The control: a button row must not spuriously carry one too, or
        // this assertion would be vacuously satisfied by every row.
        assert!(
            f.rows[GAME_MODE_ROW].edit.is_none(),
            "a button row must not carry an EditBox"
        );
        assert!(!f.rows[GAME_MODE_ROW].field);
    }

    #[test]
    fn both_fields_get_their_own_vanilla_label_and_the_seed_hint_is_not_duplicated() {
        // `CreateWorldScreen.java` wraps each field in
        // `CommonLayouts.labeledElement` — a real, drawn label, not
        // narration. Both must be present, in vanilla's own strings.
        let nav = CreateWorldNav::new();
        let f = frame(&nav);
        assert!(
            f.labels.iter().any(|l| l.text == NAME_LABEL),
            "missing the name field's own label"
        );
        assert!(
            f.labels.iter().any(|l| l.text == SEED_LABEL),
            "missing the seed field's own label — this used to be absent \
             entirely, with only the *hint* text drawn"
        );
        // `SEED_INFO` must appear as the box's own hint, and *not* also as a
        // second, permanent label/notice — vanilla shows it in exactly one
        // place (`EditBox.hint`, conditional on empty+unfocused).
        assert_eq!(nav.widgets.seed.hint.as_deref(), Some(SEED_INFO));
        assert!(
            !f.labels.iter().any(|l| l.text == SEED_INFO),
            "the hint text must not also be drawn as a permanent label"
        );
        assert!(
            f.notice.is_none(),
            "no permanent notice either — that was the pre-fix duplicate"
        );
    }

    #[test]
    fn every_row_resolves_on_screen_at_the_smallest_canvas() {
        let (w, h) = (
            crate::config::MIN_SCALED_WIDTH as f32,
            crate::config::MIN_SCALED_HEIGHT as f32,
        );
        for row in 0..ROW_COUNT {
            let (x, y, rw, rh) = row_slot(row).resolve(w, h);
            assert!(
                x >= 0.0 && y >= 0.0 && x + rw <= w && y + rh <= h,
                "row {row} at ({x}, {y}) size {rw}x{rh} on {w}x{h}"
            );
        }
    }

    #[test]
    fn the_footer_buttons_do_not_overlap_the_content_rows() {
        let (w, h) = (854.0, 480.0);
        let (_, content_bottom, _, _) = row_slot(ONLINE_MODE_ROW).resolve(w, h);
        let (_, footer_y, _, _) = row_slot(CREATE_ROW).resolve(w, h);
        assert!(
            footer_y >= content_bottom,
            "footer at {footer_y} must sit at or below the last content row's bottom {content_bottom}"
        );
    }

    // -- hover (issue #567) --------------------------------------------------

    #[test]
    fn a_fresh_screen_has_nothing_hovered_so_the_frame_carries_none() {
        let nav = CreateWorldNav::new();
        assert_eq!(nav.hovered(), None);
        assert_eq!(
            frame(&nav).hovered,
            None,
            "the frame must reflect the nav's own hover state, not invent one"
        );
    }

    #[test]
    fn hovering_a_button_row_reaches_the_frame() {
        let mut nav = CreateWorldNav::new();
        nav.hover_row(CREATE_ROW);
        assert_eq!(nav.hovered(), Some(CREATE_ROW));
        assert_eq!(
            frame(&nav).hovered,
            Some(CREATE_ROW),
            "MenuFrame::hovered must carry the row render::draw_widget outlines"
        );

        // A second hover replaces the first — only one row highlights at once.
        nav.hover_row(CANCEL_ROW);
        assert_eq!(nav.hovered(), Some(CANCEL_ROW));
    }

    /// The control: hovering a **field** must not be recorded as a button
    /// hover, mirroring `EditForm::hover_row`'s exact reason — a text field
    /// has no hover highlight in vanilla, and if a field row were treated as
    /// a hoverable button the mouse passing over Name while Seed is focused
    /// would draw a spurious outline on a row that is not a button at all.
    #[test]
    fn hovering_a_field_row_does_nothing() {
        let mut nav = CreateWorldNav::new();
        nav.hover_row(NAME_FIELD);
        assert_eq!(nav.hovered(), None, "a field row is not a hoverable button");
        nav.hover_row(SEED_FIELD);
        assert_eq!(nav.hovered(), None);

        // And it must not clobber a real hover already recorded, the same
        // property `hover_row`'s own doc argues protects the *focused* field
        // from a stray mouse move — here it is the *hover* state's turn not
        // to be reset by passing over a field.
        nav.hover_row(CREATE_ROW);
        nav.hover_row(NAME_FIELD);
        assert_eq!(
            nav.hovered(),
            Some(CREATE_ROW),
            "moving the mouse back over a field must not clear a button's hover"
        );
    }

    /// Every button row (not the two fields) must be able to report hover —
    /// a gap here is exactly how issue #567 shipped: `hover_row` existed on
    /// `EditForm` and on other screens, but `CreateWorldNav` had no such
    /// method at all, so no row on this screen could ever be hovered.
    #[test]
    fn every_button_row_can_be_hovered() {
        for row in [
            GAME_MODE_ROW,
            DIFFICULTY_ROW,
            STRUCTURES_ROW,
            BONUS_CHEST_ROW,
            ALLOW_CHEATS_ROW,
            ONLINE_MODE_ROW,
            CREATE_ROW,
            CANCEL_ROW,
        ] {
            let mut nav = CreateWorldNav::new();
            nav.hover_row(row);
            assert_eq!(nav.hovered(), Some(row), "row {row} did not record hover");
        }
    }
}
