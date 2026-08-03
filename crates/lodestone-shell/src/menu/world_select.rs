//! The singleplayer world-select screen (issue #397), with world **creation**
//! present and disabled.
//!
//! ## What it is
//!
//! Vanilla's `SelectWorldScreen` (`client/gui/screens/worldselection/`): a
//! [`HeaderAndFooterLayout`](super::layout::HeaderAndFooterLayout) with a title
//! and a search [`EditBox`] in the header, an `ObjectSelectionList` of worlds in
//! the content band, and six footer buttons — Play Selected World, Create New
//! World, Edit, Delete, Re-Create, Back.
//!
//! This module owns the screen's *input* half: which widgets exist, which of
//! them are active, where focus is and what a keystroke or a click means. The
//! geometry lives in [`super::render`] beside the other two vanilla screens'
//! (`world_select_slot`, `world_list_row_rect`), and the draw is
//! `render::frame_for`'s `Screen::WorldSelect` arm.
//!
//! ## What is disabled, and why
//!
//! Five of the six footer buttons are inactive, and **`active = false` is the
//! whole mechanism** — see [`super::widget`]. Vanilla disables four of them
//! itself, for our exact reason:
//! `SelectWorldScreen.updateButtonStatus(null)` (`SelectWorldScreen.java:159-166`)
//! turns Play, Edit, Delete and Re-Create off whenever nothing is selected, and
//! nothing can be selected here (below). The **one deviation is Create New
//! World**, which vanilla leaves active: its press calls
//! `CreateWorldScreen.openFresh` (`:87`), and `CreateWorldScreen` (828 lines) plus
//! `WorldCreationUiState` (326) are issue **#190**, not this one. Rendering it
//! greyed rather than omitting it is the point of the issue: a missing row would
//! change the footer grid's shape and read as a *different screen*, where a
//! greyed one reads exactly like vanilla with the feature unavailable.
//!
//! What vanilla does with a **tooltip** on such a button
//! (`TitleScreen.java:196`, `OptionsScreen.java:88-92`) is still deferred, for
//! #393's reason narrowed by #395: nothing in this shell tracks hover *dwell
//! time*, so a `tooltip` field would reach zero pixels. See
//! `docs/menu-focus.md`'s deliberate-gaps list.
//!
//! ## The list has exactly one world, and no storage behind it
//!
//! There is still no `LevelStorageSource`, no save directory and no save format
//! in this client. What #287 added is the *server*: the integrated server can be
//! started in-process over an in-memory duplex, so a world can be **played**
//! without ever being written. That is what the one row is —
//! [`BUNDLED_WORLD`]: a fixed seed handed to `lodestone_server`'s bundled
//! overworld generator, regenerated identically on every launch and never
//! persisted.
//!
//! One row rather than a list because one is the honest count: with no storage
//! there is nothing to enumerate, and a second row would have to be invented.
//! The row's label says so ("generated, not saved") rather than presenting
//! itself as a save.
//!
//! **Vanilla has no empty-list rendering for this screen to copy** — worth
//! keeping even now that the list is not empty, because it is why the one row
//! looks the way it does. `WorldSelectionList.handleNewLevels`
//! (`WorldSelectionList.java:167-183`) switches on the list type, and for
//! `SINGLEPLAYER` an empty result calls `CreateWorldScreen.openFresh` — it
//! *leaves the screen*. `NoWorldsEntry` (`:379-397`) exists but is only
//! reachable from the Realms `UPLOAD_WORLD` branch. So the row this screen draws
//! is **`NoWorldsEntry`'s geometry** — one entry, a `StringWidget` centred in row
//! 0's content box — rather than `WorldListEntry`'s, whose icon + three text
//! lines (`:494-502`) describe a `LevelSummary` we have no source for.
//!
//! The consequence for the list machinery is unchanged: `AbstractSelectionList`'s
//! scrolling and per-entry hit-testing are **not** ported. There is exactly one
//! world and it is therefore always the selection ([`WorldSelectNav::selected`]),
//! so nothing needs clicking to select — which is the one deliberate deviation
//! from vanilla on this screen's *behaviour*, and it is what makes **Play
//! Selected World** active. #396 is the issue that needs a real list for the
//! server screen.
//!
//! ## What consumes it
//!
//! The title screen's Singleplayer button — [`super::nav::MainButton::Singleplayer`]
//! calls [`UiState::open_world_select`](super::UiState::open_world_select), which
//! is vanilla's own wiring (`TitleScreen.java` opens `SelectWorldScreen`; nothing
//! launches a world straight off the title).
//!
//! Play Selected World is what launches: it returns
//! [`WorldSelectOutcome::Play`], `nav.rs` lifts that to
//! `MenuAction::Singleplayer`, and `app.rs`'s arm calls `begin_singleplayer` →
//! `launch_singleplayer`, which resolves a server protocol from
//! `lodestone_registry::server_protocol_for_protocol` and starts the integrated
//! server. That chain is the whole of #287's shell half, and
//! `MenuAction::Singleplayer` had **no producer at all** between #397 and #287 —
//! it was kept as exactly this seam.

use super::edit_box::EditBox;
use super::focus::{FocusChildren, FocusSet, FocusTarget, KeyEvent, KeyOutcome};
use super::nav::MenuKey;
use super::widget::Widget;

/// `selectWorld.title` (`en_us.json`): the header's `StringWidget`.
pub const WORLD_SELECT_TITLE: &str = "Select World";

/// `gui.selectWorld.search` (`en_us.json`), the search box's hint
/// (`SelectWorldScreen.java:62`). Vanilla styles it with
/// `EditBox.SEARCH_HINT_STYLE` — grey **and italic** (`EditBox.java:37`); this
/// shell's font has no italic variant, so only the grey survives, through
/// [`EditBox::hint`]'s draw in [`super::render`].
pub const SEARCH_HINT: &str = "Search...";

/// The search box's narration `Component` (`SelectWorldScreen.java:55` passes
/// the title through as the narration message). Never drawn.
pub const SEARCH_NARRATION: &str = "Select World";

/// The one world this client can play, and everything that is known about it.
///
/// Not a save file and not a `LevelSummary`: there is no world storage here (see
/// the module docs). It is a seed plus a label. Playing it starts
/// `lodestone_server`'s integrated server over that seed, which regenerates the
/// identical terrain every launch — so it behaves like the same world each time
/// without anything being written to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldEntry {
    /// What the list row says. See [`BUNDLED_WORLD`] for the length limit and why
    /// it is not a vanilla string.
    pub label: &'static str,
    /// The world seed, handed to `lodestone_server::overworld_chunk_source`.
    pub seed: i64,
}

/// The bundled world.
///
/// The seed is fixed rather than random **on purpose**: a random seed per launch
/// would make "the world" a different world every time it is opened, which is
/// worse than not persisting it — a player would notice their surroundings
/// changing and reasonably read it as a bug. A fixed seed plus a deterministic
/// generator is the closest thing to persistence available without a save
/// format, and it is honest because the label says the world is generated.
pub const BUNDLED_WORLD: WorldEntry = WorldEntry {
    // **Not a vanilla string.** Vanilla's row would carry a `LevelSummary`'s
    // name, folder and last-played timestamp, none of which exists here; every
    // candidate vanilla key would claim something untrue. This names what the
    // world actually is.
    //
    // Its **length** is a constraint, not a preference: vanilla's `NoWorldsEntry`
    // wraps a `StringWidget` with no `maxWidth`, so nothing clips it, and a
    // longer string would visibly overhang the 266 px row it is centred in — the
    // ceiling is 44 characters at the jar-less fixed advance.
    // `the_world_list_row_label_fits_the_row_it_is_centred_in` pins that.
    label: "New World (generated, not saved)",
    seed: 20_260_731,
};

/// The search box's row index, and its [`FocusSet`] id.
///
/// The ids **are** the row indices `super::render::frame_for` builds and
/// `app.rs`'s hit-test reports, exactly as [`super::nav::NAME_FIELD`]'s are, and
/// `the_world_select_rows_are_in_the_order_click_assumes` asserts the two still
/// agree. Getting this wrong is issue #391's shape: a mouse that acts on a
/// different control from the one under it.
pub const SEARCH_FIELD: usize = 0;

/// The row index of the first footer button. See [`SEARCH_FIELD`].
pub const FIRST_BUTTON_ROW: usize = 1;

/// The screen's six footer buttons, in vanilla's own `RowHelper` order —
/// `SelectWorldScreen.createFooterButtons` (`SelectWorldScreen.java:81-107`).
///
/// The order is load-bearing twice: it is the grid's cell order (so it decides
/// where each button *is*, via `render::world_select_slot`) and it is the tab
/// order, because `layout.visitWidgets` walks header → contents → footer and
/// vanilla registers them in that sequence (`:76`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldSelectButton {
    /// `LevelSummary.PLAY_WORLD` = `selectWorld.select` (`LevelSummary.java:18`).
    /// Two columns wide. **Enabled** (issue #287): vanilla's
    /// `updateButtonStatus` turns this on for a selection whose
    /// `primaryActionActive()` holds (`:163`), and [`BUNDLED_WORLD`] is always
    /// the selection because it is the only world. Pressing it starts the
    /// integrated server — see [`WorldSelectOutcome::Play`] and the module docs'
    /// "what consumes it".
    Play,
    /// `selectWorld.create`. Two columns wide. Disabled — **the one deviation
    /// from vanilla on this screen**, because its press opens
    /// `CreateWorldScreen` (issue #190). See the module docs.
    Create,
    /// `selectWorld.edit`, 71 px. Disabled: vanilla's `summary.canEdit()`
    /// (`:170`), and there is no selection — nor an `EditWorldScreen` to open.
    Edit,
    /// `selectWorld.delete`, 71 px. Disabled: `summary.canDelete()` (`:172`).
    /// Note vanilla's own `LevelSummary.canDelete()` is unconditionally `true`
    /// (`LevelSummary.java:209-211`), so this button is disabled purely by the
    /// no-selection branch — there is nothing to delete.
    Delete,
    /// `selectWorld.recreate`, 71 px. Disabled: `summary.canRecreate()`
    /// (`:171`), and re-creation routes through `CreateWorldScreen` too.
    ReCreate,
    /// `gui.back`, 71 px. **The one active button**: vanilla's press is
    /// `setScreen(this.lastScreen)` (`:106`), i.e. back to the title screen,
    /// which is also what `onClose()` does (`:154-157`) and therefore what
    /// Escape does.
    Back,
}

/// Every footer button, in vanilla's display and tab order. The index into this
/// array plus [`FIRST_BUTTON_ROW`] is the button's row index.
pub const WORLD_SELECT_BUTTONS: [WorldSelectButton; 6] = [
    WorldSelectButton::Play,
    WorldSelectButton::Create,
    WorldSelectButton::Edit,
    WorldSelectButton::Delete,
    WorldSelectButton::ReCreate,
    WorldSelectButton::Back,
];

impl WorldSelectButton {
    /// Vanilla's `en_us.json` strings, verbatim.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            WorldSelectButton::Play => "Play Selected World",
            WorldSelectButton::Create => "Create New World",
            WorldSelectButton::Edit => "Edit",
            WorldSelectButton::Delete => "Delete",
            WorldSelectButton::ReCreate => "Re-Create",
            WorldSelectButton::Back => "Back",
        }
    }

    /// Whether the button can be activated — what draws
    /// `widget/button_disabled` with a `-6250336` label and makes both the
    /// keyboard and the mouse step over it. See each variant's docs for why.
    ///
    /// This is a constant rather than a function of the selection because the
    /// selection is a constant: there is exactly one world and it is always
    /// selected (module docs). If world storage ever lands, Play's answer moves
    /// into [`WorldSelectNav::update_button_status`] — where vanilla computes it
    /// — and this method keeps only the buttons whose availability is a property
    /// of the *client* rather than of a selection.
    #[must_use]
    pub fn enabled(self) -> bool {
        matches!(self, WorldSelectButton::Play | WorldSelectButton::Back)
    }

    /// This button's row index, i.e. its [`FocusSet`] id.
    #[must_use]
    pub fn row(self) -> usize {
        FIRST_BUTTON_ROW
            + WORLD_SELECT_BUTTONS
                .iter()
                .position(|b| *b == self)
                .expect("every variant is in WORLD_SELECT_BUTTONS")
    }

    /// The button at row `row`, or `None` for a row this screen does not have.
    #[must_use]
    pub fn at_row(row: usize) -> Option<Self> {
        row.checked_sub(FIRST_BUTTON_ROW)
            .and_then(|i| WORLD_SELECT_BUTTONS.get(i))
            .copied()
    }
}

/// The canvas the widgets' bounds are seeded at.
///
/// Same role as [`super::nav`]'s `SEED_CANVAS`: the widgets outlive a frame, so
/// they need real bounds *before* any frame exists — arrow navigation is
/// geometric and [`EditBox`]'s `displayPos` scrolls against a width. The bounds
/// come from `render::world_select_search_slot`/`world_select_slot`, the same
/// slots the draw resolves, rather than restated numbers.
///
/// It matters less here than it does for the edit form, because this screen's
/// slots are *canvas-invariant* once resolved through their [`Origin`] — see
/// `render::WORLD_SELECT_REF_CANVAS` and the gate that asserts it. What the seed
/// fixes is only the absolute pair: the search box is in the header and Back is
/// in the footer, so the box is strictly above the button and overlaps it in x at
/// every canvas, which is the premise Up/Down navigation rests on.
const SEED_CANVAS: (f32, f32) = (854.0, 480.0);

/// The widgets this screen owns, in one struct so [`FocusSet`] can borrow them
/// while [`WorldSelectNav`] borrows the set.
///
/// The split is the same load-bearing one [`super::nav::FormFields`] documents:
/// `FocusSet`'s methods take `&mut dyn FocusChildren`, so the set and its
/// children cannot live in the same struct.
///
/// The title `StringWidget` is **not** here. Vanilla registers it (`:76` visits
/// every leaf) but `StringWidget`'s constructor sets `active = false`
/// (`StringWidget.java:24`), so it can never take focus and never receives an
/// event; the only registry it observably belongs to is `narratables`, and
/// nothing in this shell narrates. It is drawn as a [`super::render::MenuLabel`]
/// instead.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldSelectWidgets {
    /// The header's search field. Filters the list — of nothing, today.
    pub search: EditBox,
    /// The footer buttons, in [`WORLD_SELECT_BUTTONS`]' order.
    pub buttons: [Widget; WORLD_SELECT_BUTTONS.len()],
}

impl FocusChildren for WorldSelectWidgets {
    fn get(&self, id: usize) -> Option<&dyn FocusTarget> {
        if id == SEARCH_FIELD {
            return Some(&self.search as &dyn FocusTarget);
        }
        let i = id.checked_sub(FIRST_BUTTON_ROW)?;
        self.buttons.get(i).map(|w| w as &dyn FocusTarget)
    }

    fn get_mut(&mut self, id: usize) -> Option<&mut dyn FocusTarget> {
        if id == SEARCH_FIELD {
            return Some(&mut self.search as &mut dyn FocusTarget);
        }
        let i = id.checked_sub(FIRST_BUTTON_ROW)?;
        self.buttons.get_mut(i).map(|w| w as &mut dyn FocusTarget)
    }
}

/// What one key or click did to the screen, from [`super::nav::MenuNav`]'s point
/// of view. Only [`Self::Close`] needs the screen's cooperation; the same
/// distinction [`super::nav::FormOutcome`] draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldSelectOutcome {
    /// A widget or the focus layer dealt with it.
    Handled,
    /// Escape, or the Back button: leave for the title screen.
    Close,
    /// Play Selected World: launch [`BUNDLED_WORLD`] (issue #287).
    ///
    /// Carries nothing, because there is one world and the launcher reads
    /// [`BUNDLED_WORLD`] directly. When a real list arrives this becomes
    /// `Play(WorldEntry)` and the change is compile-visible at both ends.
    Play,
}

/// The world-select screen's live state: its widgets, its focus, and which row
/// the cursor is over.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldSelectNav {
    /// The widgets. Public so [`super::render`] can clone the search box and ask
    /// it for its own text, caret and selection rather than re-deriving them.
    pub widgets: WorldSelectWidgets,
    /// Which widget has focus, and the Tab/arrow traversal between them.
    focus: FocusSet,
    /// The row the mouse is over, which on this screen is **not** the focused
    /// row.
    ///
    /// Every other screen in this shell has a single cursor that both the
    /// keyboard and [`super::nav::MenuNav::hover`] move, so one flag carried both
    /// facts (see `render::draw_widget`'s note). Here they must be separate:
    /// merging them would make *moving the mouse across the screen* steal
    /// keyboard focus out of the search field, so typing would land nowhere.
    /// Vanilla keeps them separate for the same reason and joins them only where
    /// the sprite is picked — `isHoveredOrFocused()`, which lives in
    /// [`Widget::is_hovered_or_focused`](super::widget::Widget::is_hovered_or_focused).
    hovered: Option<usize>,
}

impl Default for WorldSelectNav {
    fn default() -> Self {
        Self::new()
    }
}

impl WorldSelectNav {
    /// A fresh screen: an empty search field with the keyboard in it, and the
    /// six footer buttons at their vanilla activity.
    #[must_use]
    pub fn new() -> Self {
        let (sx, sy, sw, sh) =
            super::render::world_select_search_slot().resolve(SEED_CANVAS.0, SEED_CANVAS.1);
        let mut search = EditBox::new(sx, sy, sw, sh, SEARCH_NARRATION);
        // `this.searchBox.setHint(...)` (`SelectWorldScreen.java:62`).
        search.hint = Some(SEARCH_HINT.to_string());
        let buttons = WORLD_SELECT_BUTTONS.map(|b| {
            let (x, y, w, h) = super::render::world_select_slot(b).resolve(SEED_CANVAS.0, SEED_CANVAS.1);
            Widget::button(x, y, w, h, b.label())
        });
        let mut widgets = WorldSelectWidgets { search, buttons };
        let mut focus = FocusSet::new();
        // `layout.visitWidgets(this::addRenderableWidget)` (`:76`), in the
        // header → contents → footer order `HeaderAndFooterLayout.visitChildren`
        // walks (`:84-89`) — which is also the tab order, since nothing here
        // overrides `getTabOrderGroup`.
        focus.add_renderable_widget(SEARCH_FIELD);
        for b in WORLD_SELECT_BUTTONS {
            focus.add_renderable_widget(b.row());
        }
        // `setInitialFocus(this.searchBox)` (`:147-152`) — the explicit overload,
        // for `EditForm::adding`'s reason: the no-argument one is gated on a
        // last-input-type this shell does not track, and without it the first
        // keystroke would go nowhere.
        focus.set_initial_focus(&mut widgets, SEARCH_FIELD);
        let mut nav = Self {
            widgets,
            focus,
            hovered: None,
        };
        nav.update_button_status();
        nav
    }

    /// `SelectWorldScreen.updateButtonStatus(summary)` (`:159-184`), collapsed to
    /// a constant because the selection is one.
    ///
    /// Vanilla's non-null branch reads four `LevelSummary` predicates —
    /// `primaryActionActive()`, `canEdit()`, `canRecreate()`, `canDelete()`
    /// (`LevelSummary.java:189-211`, overridden by its `SymlinkLevelSummary` and
    /// `CorruptedLevelSummary` subclasses at `:273-347`) — plus a
    /// `requiresFileFixing()` tooltip. Only the first is ported, and as a
    /// constant: [`BUNDLED_WORLD`] is always the selection and is always
    /// playable, so `primaryActionActive()` is `true`, while the other three ask
    /// about a *file* — there is none, and Edit/Delete/Re-Create additionally
    /// have no screen to open (#190). An enum whose variants nothing constructs
    /// is the island `CLAUDE.md` names as this repo's dominant defect, so the
    /// predicates stay unported rather than modelled and unused; the lines above
    /// are the lookup for whoever adds world storage.
    fn update_button_status(&mut self) {
        for (widget, button) in self.widgets.buttons.iter_mut().zip(WORLD_SELECT_BUTTONS) {
            widget.active = button.enabled();
        }
    }

    /// The search field, for the draw.
    #[must_use]
    pub fn search(&self) -> &EditBox {
        &self.widgets.search
    }

    /// The focused row, or `None` when nothing is focused.
    #[must_use]
    pub fn focused_row(&self) -> Option<usize> {
        self.focus.focused()
    }

    /// The row the cursor is over. See [`Self::hovered`].
    #[must_use]
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// The focused footer button, if focus is on one rather than in the field.
    #[must_use]
    pub fn focused_button(&self) -> Option<WorldSelectButton> {
        WorldSelectButton::at_row(self.focus.focused()?)
    }

    /// `AbstractWidget.isActive()` for the widget at `row`.
    ///
    /// **Read the widget, not [`WorldSelectButton::enabled`].** That method is
    /// the *initial* value [`Self::update_button_status`] writes, exactly as
    /// vanilla's `updateButtonStatus` computes one and assigns it to
    /// `button.active`; the flag on the widget is the live fact, and it is what
    /// the sprite (`WidgetSprites.get(active, …)`) and focus traversal
    /// (`nextFocusPath`) already key on.
    ///
    /// Consulting the enum instead is a second source of truth, and it was a real
    /// defect here rather than a hypothetical one:
    /// `a_click_on_a_disabled_button_does_nothing_at_all`'s control — enable one
    /// button and watch the click land — failed, because `click_row` was asking
    /// the enum while the test (and `updateButtonStatus`) wrote the widget.
    #[must_use]
    pub fn is_active(&self, row: usize) -> bool {
        self.widgets.get(row).is_some_and(|c| c.is_active())
    }

    /// The mouse moved onto row `row`. Records hover only — **never** focus, see
    /// [`Self::hovered`]. A disabled row is still hovered, matching vanilla:
    /// `AbstractWidget.extractRenderState` sets `isHovered` from geometry alone
    /// (`AbstractWidget.java:56-62`) and the disabled sprite wins anyway.
    pub fn hover(&mut self, row: usize) {
        if row == SEARCH_FIELD || WorldSelectButton::at_row(row).is_some() {
            self.hovered = Some(row);
        }
    }

    /// One key, routed through vanilla's `Screen.keyPressed` order: Escape, then
    /// the focused widget, then — only if it declined — Tab and the arrows as
    /// focus navigation, and only then this screen's own meaning for the key.
    ///
    /// The ordering is what makes the search field behave: it consumes
    /// Backspace/Delete and the horizontal arrows, and *declines* Up/Down and Tab
    /// (`EditBox.java:279-284`), which is how they reach focus traversal without
    /// any rule saying so. See `docs/menu-focus.md`.
    pub fn handle_key(&mut self, key: MenuKey) -> WorldSelectOutcome {
        // A printable character is `charTyped`, a different callback.
        if let MenuKey::Char(ch) = key {
            self.focus.char_typed(&mut self.widgets, ch);
            return WorldSelectOutcome::Handled;
        }
        let Some(event) = KeyEvent::from_menu_key(key) else {
            return WorldSelectOutcome::Handled;
        };
        match self.focus.screen_key_pressed(&mut self.widgets, event) {
            KeyOutcome::Close => WorldSelectOutcome::Close,
            KeyOutcome::Consumed | KeyOutcome::FocusMoved => WorldSelectOutcome::Handled,
            // `AbstractButton.keyPressed` presses a focused, *active* button on
            // Enter or Space and returns `true` (`AbstractButton.java:61-71`).
            // Our `Widget` is data with no press callback, so the screen applies
            // that here instead; the observable behaviour is the same, and an
            // inactive button never gets here because it cannot hold focus.
            KeyOutcome::Declined if key == MenuKey::Enter => self.press_focused(),
            KeyOutcome::Declined => WorldSelectOutcome::Handled,
        }
    }

    /// A left-click that landed on row `row`.
    ///
    /// **Its own arm rather than "hover then Enter"**, which is the translation
    /// that caused #391 on the settings screen and, one screen over, made
    /// clicking a `ServerEdit` field submit the form. This screen has no row
    /// cursor at all: a click on the field focuses it and a click on a button
    /// presses it, and neither is the other.
    ///
    /// Mirrors `ContainerEventHandler.mouseClicked` (`:44-52`) by row instead of
    /// by coordinate: the child answers whether it consumed the click
    /// (`AbstractWidget.mouseClicked` returns `false` when inactive,
    /// `AbstractWidget.java:109-125`) and only then does it take focus, gated on
    /// `shouldTakeFocusAfterInteraction()` — `true` for a plain `Button`
    /// (`GuiEventListener.java:60-62`).
    pub fn click_row(&mut self, row: usize) -> WorldSelectOutcome {
        if row == SEARCH_FIELD {
            self.focus.set_focused(&mut self.widgets, Some(SEARCH_FIELD));
            return WorldSelectOutcome::Handled;
        }
        let Some(button) = WorldSelectButton::at_row(row) else {
            return WorldSelectOutcome::Handled;
        };
        if !self.is_active(row) {
            // A click on a present-but-disabled button does nothing at all —
            // including not moving focus. Returning here is what stops it
            // activating whatever *was* focused.
            return WorldSelectOutcome::Handled;
        }
        self.focus.set_focused(&mut self.widgets, Some(row));
        self.press(button)
    }

    /// Press the focused button, if focus is on one.
    fn press_focused(&mut self) -> WorldSelectOutcome {
        match self.focused_button() {
            Some(button) => self.press(button),
            None => WorldSelectOutcome::Handled,
        }
    }

    /// What one button's press means. Every variant is spelled out rather than
    /// `_`-defaulted, so enabling one without giving it an action is a
    /// compile-visible mistake and not a silently dead button.
    fn press(&mut self, button: WorldSelectButton) -> WorldSelectOutcome {
        match button {
            WorldSelectButton::Back => WorldSelectOutcome::Close,
            // Vanilla's `loadSelectedWorld()` (`:117-121`), which is
            // `WorldSelectionList.getSelectedOpt().ifPresent(Entry::joinWorld)`.
            // Ours is issue #287's launch — see this module's "what consumes it".
            WorldSelectButton::Play => WorldSelectOutcome::Play,
            // Disabled above, and unreachable through either press path.
            WorldSelectButton::Create
            | WorldSelectButton::Edit
            | WorldSelectButton::Delete
            | WorldSelectButton::ReCreate => WorldSelectOutcome::Handled,
        }
    }

    /// The selected world — always [`BUNDLED_WORLD`], because it is the only one.
    ///
    /// An `Option` rather than a plain [`WorldEntry`] so the "nothing selected"
    /// state vanilla's `updateButtonStatus(null)` exists for stays expressible;
    /// a real list will return `None` before the player clicks a row, and every
    /// caller already has to handle it.
    #[must_use]
    pub fn selected(&self) -> Option<WorldEntry> {
        Some(BUNDLED_WORLD)
    }

    /// What the one list row draws.
    ///
    /// Derived from [`Self::selected`] rather than reading [`BUNDLED_WORLD`] at
    /// the draw site, so the row the player reads and the world Play launches
    /// cannot become two different things.
    #[must_use]
    pub fn world_row_label(&self) -> &'static str {
        match self.selected() {
            Some(world) => world.label,
            // Unreachable while there is exactly one world. Kept as an answer
            // rather than an `expect` because the day a real list exists this
            // becomes the empty-list state, and it must not panic there.
            None => "No worlds",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every button is present, and exactly two of them are active.
    ///
    /// The count is asserted both ways round on purpose: "four disabled" is what
    /// makes the screen honest about what this client cannot do, and Play being
    /// enabled is #287 — the screen stopped being a dead end that could only be
    /// left again.
    #[test]
    fn four_of_the_six_footer_buttons_are_present_and_disabled() {
        assert_eq!(WORLD_SELECT_BUTTONS.len(), 6, "vanilla has six footer buttons");
        let enabled: Vec<_> = WORLD_SELECT_BUTTONS
            .iter()
            .copied()
            .filter(|b| b.enabled())
            .collect();
        assert_eq!(
            enabled,
            vec![WorldSelectButton::Play, WorldSelectButton::Back],
            "Play launches the bundled world (#287); Back leaves"
        );
        // The headline of #397: Create New World is *there*, and inactive.
        assert!(
            WORLD_SELECT_BUTTONS.contains(&WorldSelectButton::Create),
            "creation must be present, not absent — an omitted row reshapes the footer"
        );
        assert!(!WorldSelectButton::Create.enabled());
        assert_eq!(WorldSelectButton::Create.label(), "Create New World");
    }

    /// The list has a world, and the row the player reads is the world Play
    /// launches.
    ///
    /// The label's length is the load-bearing part: nothing clips a
    /// `NoWorldsEntry` `StringWidget`, so an overlong label overhangs the row
    /// (`render`'s `the_world_list_row_label_fits_the_row_it_is_centred_in`
    /// measures it against the real row width).
    #[test]
    fn the_list_has_one_world_and_it_is_always_the_selection() {
        let nav = WorldSelectNav::new();
        assert_eq!(nav.selected(), Some(BUNDLED_WORLD));
        assert_eq!(nav.world_row_label(), BUNDLED_WORLD.label);
        assert!(
            !BUNDLED_WORLD.label.is_empty(),
            "an empty row is indistinguishable from a list that failed to draw"
        );
        // The selection is what makes Play active, so the two must agree — this
        // is the link that would otherwise let a greyed Play sit above a world.
        assert_eq!(
            nav.is_active(WorldSelectButton::Play.row()),
            nav.selected().is_some()
        );
    }

    /// The row indices the mouse reports are the ids focus dispatches on.
    ///
    /// Same guard shape as `the_settings_rows_are_in_the_order_click_assumes`,
    /// and the same #391 it protects: two files agreeing about what row 3 is.
    #[test]
    fn the_button_rows_are_contiguous_and_start_after_the_search_field() {
        assert_eq!(SEARCH_FIELD, 0);
        assert_eq!(FIRST_BUTTON_ROW, SEARCH_FIELD + 1);
        for (i, b) in WORLD_SELECT_BUTTONS.iter().enumerate() {
            assert_eq!(b.row(), FIRST_BUTTON_ROW + i);
            assert_eq!(WorldSelectButton::at_row(b.row()), Some(*b));
        }
        assert_eq!(WorldSelectButton::at_row(SEARCH_FIELD), None);
        assert_eq!(
            WorldSelectButton::at_row(FIRST_BUTTON_ROW + WORLD_SELECT_BUTTONS.len()),
            None
        );
    }

    /// The widget set is reachable by id in both directions, and the ids are the
    /// rows.
    #[test]
    fn every_widget_is_reachable_through_the_focus_children_seam() {
        let mut nav = WorldSelectNav::new();
        assert!(nav.widgets.get(SEARCH_FIELD).is_some());
        for b in WORLD_SELECT_BUTTONS {
            assert!(nav.widgets.get(b.row()).is_some(), "{b:?} unreachable");
            assert!(nav.widgets.get_mut(b.row()).is_some(), "{b:?} unreachable (mut)");
        }
        // Control: a row this screen does not have must be `None`, or the lookup
        // would be answering with whatever it happened to index.
        assert!(nav.widgets.get(WORLD_SELECT_BUTTONS.len() + 1).is_none());
        // And `update_button_status` must have written the enum's predicate onto
        // every widget: the two are one source of truth with a copy, not two
        // sources — see `WorldSelectNav::is_active`.
        for b in WORLD_SELECT_BUTTONS {
            assert_eq!(
                nav.is_active(b.row()),
                b.enabled(),
                "{b:?} disagrees with its widget"
            );
            assert_eq!(
                nav.widgets.get(b.row()).unwrap().is_active(),
                b.enabled(),
                "{b:?} disagrees with its widget through the focus seam"
            );
        }
        assert!(nav.is_active(SEARCH_FIELD), "the search field is editable");
    }

    /// The keyboard starts in the search field and Tab reaches exactly the three
    /// widgets that are active.
    ///
    /// Four inactive buttons must be *skipped*, not merely un-pressable —
    /// `AbstractWidget.nextFocusPath` returns null for an inactive widget — and
    /// the wrap is vanilla's `clearFocus()`-then-retry, not `(i + 1) % n`.
    ///
    /// The order is **registration order, not geometry**: Tab sorts by
    /// `getTabOrderGroup` (all default) with a stable sort, and this screen
    /// registers header → footer, so Play comes before Back even though Back is
    /// the earlier row visually in neither sense. See `focus.rs`'s module docs.
    #[test]
    fn tab_visits_the_search_field_play_and_back_and_nothing_else() {
        let mut nav = WorldSelectNav::new();
        assert_eq!(nav.focused_row(), Some(SEARCH_FIELD), "setInitialFocus");
        let mut seen = vec![nav.focused_row()];
        for _ in 0..4 {
            nav.handle_key(MenuKey::Tab);
            seen.push(nav.focused_row());
        }
        assert_eq!(
            seen,
            vec![
                Some(SEARCH_FIELD),
                Some(WorldSelectButton::Play.row()),
                Some(WorldSelectButton::Back.row()),
                Some(SEARCH_FIELD),
                Some(WorldSelectButton::Play.row()),
            ],
            "tab must cycle between the only three active widgets"
        );

        // -- control ---------------------------------------------------------
        // The walk has to be able to reach a button it was skipping, or the
        // assertion above is satisfied by a traversal that can only ever find
        // three things.
        let mut nav = WorldSelectNav::new();
        let create = WorldSelectButton::Create.row();
        nav.widgets.buttons[create - FIRST_BUTTON_ROW].active = true;
        nav.handle_key(MenuKey::Tab);
        nav.handle_key(MenuKey::Tab);
        assert_eq!(
            nav.focused_row(),
            Some(create),
            "an enabled Create must be the tab stop after Play, before Back"
        );
    }

    /// Typing goes into the field, and the vertical arrows do not.
    #[test]
    fn the_focused_field_takes_text_and_lets_the_vertical_arrows_out() {
        let mut nav = WorldSelectNav::new();
        for ch in "cave".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        assert_eq!(nav.search().value(), "cave");
        nav.handle_key(MenuKey::Backspace);
        assert_eq!(nav.search().value(), "cav", "backspace edits at the caret");

        // Down must leave the field for the nearest active widget below it —
        // geometric navigation, so this is also the premise assertion that the
        // seeded bounds put the footer below the search box and overlapping it in
        // x. **Play, not Back**: the strict pass sorts by leading edge and Play's
        // row (y 428) is above Back's (y 452).
        nav.handle_key(MenuKey::Down);
        assert_eq!(nav.focused_row(), Some(WorldSelectButton::Play.row()));
        assert_eq!(nav.search().value(), "cav", "the field kept its text");
        // Down again reaches Back through the *vague* pass — nothing active
        // overlaps Play in x below it, so the strict pass finds nothing and the
        // fallback takes the nearest by squared distance.
        nav.handle_key(MenuKey::Down);
        assert_eq!(nav.focused_row(), Some(WorldSelectButton::Back.row()));
        // Arrows do not wrap (`Screen.java:139-143` gates the retry on Tab), so
        // Down off the last active widget stays put.
        nav.handle_key(MenuKey::Down);
        assert_eq!(nav.focused_row(), Some(WorldSelectButton::Back.row()));
        // And Up comes back to the field: Back's x band (510..581) overlaps the
        // search box's (327..527) and nothing else active sits between them.
        nav.handle_key(MenuKey::Up);
        assert_eq!(nav.focused_row(), Some(SEARCH_FIELD));
    }

    /// Escape closes the screen, Enter on Play launches, and Enter closes only
    /// from Back.
    #[test]
    fn escape_closes_and_enter_launches_from_play_and_closes_from_back() {
        let mut nav = WorldSelectNav::new();
        assert_eq!(nav.handle_key(MenuKey::Escape), WorldSelectOutcome::Close);

        let mut nav = WorldSelectNav::new();
        // Enter with the field focused is `EditBox`'s decline plus a screen that
        // has nothing to do with it — it must *not* close, and must not launch.
        assert_eq!(nav.handle_key(MenuKey::Enter), WorldSelectOutcome::Handled);
        nav.handle_key(MenuKey::Tab);
        assert_eq!(nav.focused_row(), Some(WorldSelectButton::Play.row()));
        assert_eq!(nav.handle_key(MenuKey::Enter), WorldSelectOutcome::Play);
        nav.handle_key(MenuKey::Tab);
        assert_eq!(nav.focused_row(), Some(WorldSelectButton::Back.row()));
        assert_eq!(nav.handle_key(MenuKey::Enter), WorldSelectOutcome::Close);
    }

    /// A click on Play launches, and it is the *only* footer button that does.
    ///
    /// The negative half matters as much as the positive one: five of the six
    /// buttons must not start a world, and `press` spells every variant out so a
    /// newly-enabled button cannot inherit Play's action by falling through a
    /// `_` arm.
    #[test]
    fn only_play_launches_the_world() {
        let mut nav = WorldSelectNav::new();
        assert_eq!(
            nav.click_row(WorldSelectButton::Play.row()),
            WorldSelectOutcome::Play
        );

        for button in WORLD_SELECT_BUTTONS
            .iter()
            .copied()
            .filter(|b| *b != WorldSelectButton::Play)
        {
            let mut nav = WorldSelectNav::new();
            // Enabled, so the click is definitely delivered — this is the
            // control shape `a_click_on_a_disabled_button_does_nothing_at_all`
            // uses, inverted: here a *disabled* button would pass vacuously.
            nav.widgets.buttons[button.row() - FIRST_BUTTON_ROW].active = true;
            assert_ne!(
                nav.click_row(button.row()),
                WorldSelectOutcome::Play,
                "{button:?} must not start a world"
            );
        }
    }

    /// A click is not a hover-then-Enter, and this is the assertion that says so.
    #[test]
    fn a_click_focuses_the_field_and_presses_a_button_but_never_both() {
        let mut nav = WorldSelectNav::new();
        // Focus starts in the field; move it away so "the click focused it" is
        // an observable change rather than the initial state.
        nav.handle_key(MenuKey::Tab);
        assert_eq!(nav.focused_row(), Some(WorldSelectButton::Play.row()));
        assert_eq!(
            nav.click_row(SEARCH_FIELD),
            WorldSelectOutcome::Handled,
            "clicking the search field must not activate the screen"
        );
        assert_eq!(nav.focused_row(), Some(SEARCH_FIELD));

        // A click on Back both focuses and presses it.
        assert_eq!(
            nav.click_row(WorldSelectButton::Back.row()),
            WorldSelectOutcome::Close
        );
        assert_eq!(nav.focused_row(), Some(WorldSelectButton::Back.row()));
    }

    /// A click on a disabled button does nothing — and specifically does not
    /// move focus, which is what would let the *next* Enter press it.
    #[test]
    fn a_click_on_a_disabled_button_does_nothing_at_all() {
        for button in WORLD_SELECT_BUTTONS.iter().copied().filter(|b| !b.enabled()) {
            let mut nav = WorldSelectNav::new();
            assert_eq!(
                nav.click_row(button.row()),
                WorldSelectOutcome::Handled,
                "{button:?} is disabled and must do nothing"
            );
            assert_eq!(
                nav.focused_row(),
                Some(SEARCH_FIELD),
                "{button:?} must not take focus"
            );
        }

        // -- control ---------------------------------------------------------
        // The same click on the same row, with the button enabled, must move
        // focus — otherwise the assertions above would pass for a `click_row`
        // that ignores every row.
        let mut nav = WorldSelectNav::new();
        let create = WorldSelectButton::Create;
        nav.widgets.buttons[create.row() - FIRST_BUTTON_ROW].active = true;
        assert_eq!(nav.click_row(create.row()), WorldSelectOutcome::Handled);
        assert_eq!(nav.focused_row(), Some(create.row()));
    }

    /// Hover is not focus. The bug this prevents is concrete: with one flag,
    /// dragging the cursor over the footer would pull the keyboard out of the
    /// search field mid-word.
    #[test]
    fn hovering_a_row_never_moves_focus() {
        let mut nav = WorldSelectNav::new();
        assert_eq!(nav.hovered(), None);
        for b in WORLD_SELECT_BUTTONS {
            nav.hover(b.row());
            assert_eq!(nav.hovered(), Some(b.row()), "{b:?} not hovered");
            assert_eq!(
                nav.focused_row(),
                Some(SEARCH_FIELD),
                "hovering {b:?} moved focus"
            );
        }
        // A row this screen does not have is ignored rather than recorded, so a
        // stale hover cannot highlight a widget that is not there.
        nav.hover(FIRST_BUTTON_ROW + WORLD_SELECT_BUTTONS.len());
        assert_eq!(nav.hovered(), Some(WorldSelectButton::Back.row()));
    }

    /// The search box carries vanilla's hint, and the hint is what draws when
    /// the box is empty and unfocused.
    #[test]
    fn the_search_box_has_vanillas_hint_and_its_own_bounds() {
        let nav = WorldSelectNav::new();
        assert_eq!(nav.search().hint.as_deref(), Some(SEARCH_HINT));
        assert_eq!(nav.search().value(), "", "it opens empty");
        // 200x20 is vanilla's declared size (`SelectWorldScreen.java:55`), and it
        // must survive the trip through the slot the draw uses.
        assert_eq!(nav.search().widget.width, 200.0);
        assert_eq!(nav.search().widget.height, 20.0);
    }
}
