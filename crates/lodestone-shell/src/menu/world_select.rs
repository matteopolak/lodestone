//! The singleplayer world-select screen, with world **creation**
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
//! **Two** of the six footer buttons are inactive — three before that fix gave
//! Delete its confirmation screen, four before that fix enabled Create — and
//! **`active = false` is the whole mechanism**, see [`super::widget`]. Vanilla
//! disables them itself, for our exact reason:
//! `SelectWorldScreen.updateButtonStatus(null)` (`SelectWorldScreen.java:159-166`)
//! turns Edit, Delete and Re-Create off whenever nothing is selected, which is a
//! state this screen really reaches (an empty `saves/`, or a filter matching
//! nothing). What is left is the *client-level* ceiling: `Edit` and `Re-Create`
//! have no screen to open at all. Rendering them greyed rather than omitting them
//! is the point of that fix: a missing row would change the footer grid's shape and
//! read as a *different screen*, where a greyed one reads exactly like vanilla
//! with the feature unavailable.
//!
//! What vanilla does with a **tooltip** on such a button
//! (`TitleScreen.java:196`, `OptionsScreen.java:88-92`) is still deferred, for
//! That fix's reason narrowed by that fix: nothing in this shell tracks hover *dwell
//! time*, so a `tooltip` field would reach zero pixels. See
//! `docs/menu-focus.md`'s deliberate-gaps list.
//!
//! ## The list is the real save list now
//!
//! This section used to say "the list has exactly one world, and no storage
//! behind it", and describe a hardcoded [`BUNDLED_WORLD`] row. That was issue
//! That fix's reading (1) and it is **gone**: [`crate::saves`] is this client's
//! `LevelStorageSource`, and the rows are whatever is in `saves/`. Read
//! `saves.rs`'s module doc for the product decision and for the wart that forced
//! it (with one implicit world, Create New World could not create a second one).
//!
//! What that changes here:
//!
//! - the content band holds **N** rows, one per [`crate::saves::WorldSummary`],
//!   drawn with `WorldListEntry`'s geometry (a 32 px icon column and three text
//!   lines — `WorldSelectionList.java:490-502`) rather than `NoWorldsEntry`'s
//!   single centred string. The icon square itself stays empty: this client
//!   writes no `icon.png`, so there is nothing to blit into it, and the column
//!   is reserved anyway because the three text lines' x is measured from it
//!   (`getTextX() = getContentX() + 32 + 3`, `:568-570`);
//! - a row is a **focusable, clickable widget**, so [`Self::selected`] is a real
//!   selection rather than a constant, and Play/Edit/Delete/Re-Create ask it —
//!   which is what [`WorldSelectNav::update_button_status`] is now for;
//! - the search box filters, by `WorldSelectionList.filterAccepts` (`:233-235`):
//!   a case-insensitive substring of the **display name or the folder name**.
//!
//! ## Three deliberate deviations, each with its reason
//!
//! - **The empty list does not leave the screen.** `handleNewLevels`
//!   (`WorldSelectionList.java:167-183`) switches on the list type, and for
//!   `SINGLEPLAYER` an empty result calls `CreateWorldScreen.openFresh` — real
//!   vanilla *replaces* the world list with the creation screen when you have no
//!   worlds. This shell instead draws `NoWorldsEntry` (`:379-397`, which vanilla
//!   only reaches from the Realms `UPLOAD_WORLD` branch) with
//!   [`NO_WORLDS_LABEL`]. Two reasons: opening a different screen from a screen's
//!   *first frame* makes the world list unreachable for a fresh install, and
//!   Escape from that creation screen would return the player to a screen they
//!   never saw. An empty list with a live Create button says the same thing and
//!   is reversible.
//! - **The first row is selected on open.** Vanilla starts with
//!   `updateButtonStatus(null)` and needs a click. `AbstractSelectionList`'s
//!   keyboard selection is not ported (see the next point), so requiring a click
//!   would leave a keyboard-only player unable to play at all; selecting the
//!   most-recently-played world — which is row 0, because
//!   [`crate::saves::WorldSummary::cmp_for_list`] sorts last-played descending —
//!   is both the likely intent and the state this screen already had when it
//!   had one row.
//! - **The list scrolls, and focus scrolls with it**. This bullet
//!   used to say the list did not, and that a player with more worlds than
//!   `max_visible_world_rows()` could not reach the rest. The cap is gone: every
//!   world gets a widget, [`Self::scroll`](WorldSelectNav) is a pixel offset
//!   driven by `MenuNav::active_list`/`scroll_active_list`, and
//!   [`WorldSelectNav::scroll_to_focus`] brings the focused row into the band —
//!   which is what makes removing the cap safe, because a focusable row with no
//!   rect is a trap. `render::world_list_row_visible` is a partial-overlap band
//!   test now, and `draw_world_entry` runs inside `Quads::with_clip`. See
//!   `docs/world-select.md` for the one residue (a keypress has no canvas, so
//!   scroll-into-view is conservative and the offset is re-clamped at draw time).
//!
//! ## What consumes it
//!
//! The title screen's Singleplayer button — [`super::nav::MainButton::Singleplayer`]
//! calls [`UiState::open_world_select`](super::UiState::open_world_select), which
//! is vanilla's own wiring (`TitleScreen.java` opens `SelectWorldScreen`; nothing
//! launches a world straight off the title). That arm also **re-enumerates**:
//! `MenuNav` rebuilds this screen from disk on entry, the way vanilla constructs
//! a fresh `SelectWorldScreen`, so a world created a moment ago is on the list.
//!
//! Play Selected World is what launches: it returns
//! [`WorldSelectOutcome::Play`] carrying the selected world's **folder name**,
//! `nav.rs` resolves that against its own saves root and lifts it to
//! `MenuAction::Singleplayer(SingleplayerLaunch::Open(dir))`, and `app.rs`'s arm
//! calls `begin_singleplayer` → `launch_singleplayer`, which resolves a server
//! protocol from `lodestone_registry::server_protocol_for_protocol` and starts
//! the integrated server against that directory.

use super::edit_box::EditBox;
use super::focus::{FocusChildren, FocusSet, FocusTarget, KeyEvent, KeyOutcome};
use super::nav::MenuKey;
use super::widget::Widget;
use crate::saves::WorldSummary;

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

/// `selectWorld.load_folder_access` is a *failure* string; the one this needs is
/// `mco.upload.select.world.none`, which is what vanilla's `NoWorldsEntry`
/// carries in the only branch that reaches it (`WorldSelectionList.java:176`).
///
/// Reworded rather than transcribed, because vanilla's own string names the
/// Realms upload flow this client does not have ("No worlds available to
/// upload!"). What survives is the *shape*: one centred line in row 0's content
/// box saying the list is empty. See the module docs on why this screen shows it
/// at all rather than opening `CreateWorldScreen` the way vanilla does.
/// The native empty-list line. See [`NO_WORLDS_LABEL`].
///
/// Named separately from the `cfg`-selected alias so the length gate can measure
/// **both** strings on one target — see [`NO_WORLDS_LABEL_BROWSER`].
pub const NO_WORLDS_LABEL_NATIVE: &str = "No worlds yet — press Create New World";

/// The browser's empty-list line.
///
/// **The list is empty here permanently, not yet**, and saying so is the whole
/// difference. A browser has no `saves/` — `read_dir` returns `Err(Unsupported)` — so
/// [`crate::saves::list_worlds`] can only ever be empty, and a player who creates a
/// world, plays it, and comes back to an empty list would reasonably read that as a
/// broken save. It is not broken: the world was in memory, and the tab closing ended it.
///
/// The **flow is deliberately unchanged**, and this label is what makes that
/// defensible. Vanilla's `handleNewLevels` opens `CreateWorldScreen.openFresh` on an
/// empty singleplayer list, and this module's docs record why this shell does not follow
/// it: opening another screen from a screen's *first frame* makes the world list
/// unreachable, and Escape would return the player somewhere they never saw. That
/// argument is **stronger** in a browser, not weaker — the list is empty on every visit,
/// so auto-opening creation would make this screen unreachable *forever* rather than
/// merely on a fresh install. So the screen stays, the Create button stays live, and the
/// line explains itself.
///
/// It is a **separate named constant rather than only the `cfg` alias below**, and that
/// is the point: the 44-character ceiling on this row is pinned by
/// `the_world_list_row_label_fits_the_row_it_is_centred_in`, which runs on the host and
/// would therefore have measured only the native string. The first draft of this line was
/// 53 characters — it would have overhung the row in a browser and no gate would have
/// said so. A guard only covers what it names.
pub const NO_WORLDS_LABEL_BROWSER: &str = "Not saved — press Create New World";

/// The empty-list line for this target: [`NO_WORLDS_LABEL_NATIVE`] or
/// [`NO_WORLDS_LABEL_BROWSER`].
#[cfg(not(target_arch = "wasm32"))]
pub const NO_WORLDS_LABEL: &str = NO_WORLDS_LABEL_NATIVE;

/// The empty-list line for this target. See [`NO_WORLDS_LABEL_BROWSER`].
#[cfg(target_arch = "wasm32")]
pub const NO_WORLDS_LABEL: &str = NO_WORLDS_LABEL_BROWSER;

/// A world seed and a label, kept from that fix's one-hardcoded-row era.
///
/// **Not a list row any more.** The list is [`crate::saves::list_worlds`]; this
/// type survives for exactly one caller, [`BUNDLED_WORLD`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldEntry {
    /// What the row used to say. Read only by the length gate that keeps it
    /// inside the row it would be centred in.
    pub label: &'static str,
    /// The world seed, handed to `lodestone_server::overworld_chunk_source`.
    pub seed: i64,
}

/// The seed a launch with **no typed seed and no existing world** falls back to
/// — `app::launch::resolve_launch_seed(None)`.
///
/// This used to be *the* world: a fixed seed regenerated identically every
/// launch, because there was no save format. There is one now, so its label is
/// no longer drawn anywhere and its seed is reachable in one narrow case only —
/// Play Selected World on a directory whose `world_gen_settings.dat` is missing,
/// which `resolve_world_seed` then creates from this value.
///
/// **Fixed rather than random, still deliberately.** In that one case a random
/// seed would silently generate *unvisited* chunks against a different seed from
/// the visited ones, which is the discontinuity
/// `lodestone_server::region_source::resolve_world_seed`'s own doc describes as
/// worse than either failure alone.
pub const BUNDLED_WORLD: WorldEntry = WorldEntry {
    // Its **length** is a constraint, not a preference: `NoWorldsEntry` wraps a
    // `StringWidget` with no `maxWidth`, so nothing clips it, and a longer string
    // would visibly overhang the 266 px row it is centred in — the ceiling is 44
    // characters at the jar-less fixed advance.
    // `the_world_list_row_label_fits_the_row_it_is_centred_in` pins that, and now
    // measures [`NO_WORLDS_LABEL`], the string that really is drawn there.
    label: "New World (generated, not saved)",
    seed: 20_260_731,
};

/// The search box's row index, and its [`FocusSet`] id.
///
/// The ids **are** the row indices `super::render::frame_for` builds and
/// `app.rs`'s hit-test reports, exactly as [`super::nav::NAME_FIELD`]'s are, and
/// `the_world_select_rows_are_in_the_order_click_assumes` asserts the two still
/// agree. Getting this wrong is that fix's shape: a mouse that acts on a
/// different control from the one under it.
pub const SEARCH_FIELD: usize = 0;

/// The row index of the first footer button. See [`SEARCH_FIELD`].
pub const FIRST_BUTTON_ROW: usize = 1;

/// The row index of the first **world-list** row.
///
/// The world rows sit *after* the footer buttons in the id space even though
/// they are above them on screen and before them in the tab order. That is not
/// an accident and it is not vanilla's own ordering:
///
/// - the ids are indices into `render::frame_for`'s `rows` **and** into
///   `FocusSet`, so they must be stable; putting the worlds between the search
///   field and the buttons would renumber all six buttons every time the world
///   count changed, and `app.rs`'s hit-test would be reading last frame's
///   numbering;
/// - the *tab* order is registration order, not id order (see
///   [`super::focus`]), so [`WorldSelectNav::new`] can still register
///   header → contents → footer exactly as `layout.visitWidgets` walks them
///   (`SelectWorldScreen.java:76`). `tab_visits_the_list_between_the_search_field_and_the_footer`
///   is the gate on that, and it is the one that would fail if these two facts
///   were ever collapsed into one.
pub const FIRST_WORLD_ROW: usize = FIRST_BUTTON_ROW + WORLD_SELECT_BUTTONS.len();

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
    /// Two columns wide. **Enabled**: vanilla's
    /// `updateButtonStatus` turns this on for a selection whose
    /// `primaryActionActive()` holds (`:163`) — which since that fix is a real
    /// [`crate::saves::WorldSummary`] rather than the one hardcoded
    /// [`BUNDLED_WORLD`] this doc used to name. Pressing it starts the integrated
    /// server — see [`WorldSelectOutcome::Play`] and the module docs' "what
    /// consumes it". [`WorldSelectNav::play_selected`] re-checks `can_play()`,
    /// which is not redundant: see its doc, and that fix.
    Play,
    /// `selectWorld.create`. Two columns wide. **Enabled**: its
    /// press opens [`super::Screen::CreateWorld`] — see
    /// [`super::create_world`]'s module docs for what that screen does and
    /// does not do yet. This was **the one deviation from vanilla** on this
    /// screen before that fix; that history is why the module docs still say so.
    Create,
    /// `selectWorld.edit`, 71 px. Disabled: vanilla's `summary.canEdit()`
    /// (`:170`), and there is no selection — nor an `EditWorldScreen` to open.
    Edit,
    /// `selectWorld.delete`, 71 px. **Live** since that fix:
    /// `summary.canDelete()` (`:172`), and vanilla's own
    /// `LevelSummary.canDelete()` is unconditionally `true`
    /// (`LevelSummary.java:209-211`) — so this is off only in the no-selection
    /// branch, where there is nothing to delete. Its press opens
    /// [`super::Screen::Confirm`]; it does not delete anything itself.
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

    /// Whether this button's availability is a property of the **client** rather
    /// than of the selection — i.e. whether it could ever be active at all.
    ///
    /// The split this method's doc used to promise has happened: world storage
    /// landed, so `Play`'s real answer is now
    /// [`WorldSelectNav::update_button_status`]'s, computed from
    /// [`crate::saves::WorldSummary::can_play`] exactly as vanilla's
    /// `updateButtonStatus` computes it from `LevelSummary`. What is left here is
    /// the *ceiling*: `Edit` and `Re-Create` return `false` unconditionally
    /// because there is no `EditWorldScreen` and no re-create flow to open.
    ///
    /// **`Delete` used to be in that list and is not any more**. The
    /// reason it was there is worth keeping, because it is the reason the fix
    /// looks the way it does: deleting a world is irreversible, and a cheap
    /// confirmation — arming this button and treating a second press as the
    /// answer — is deletable-by-double-click. What changed is that
    /// [`super::confirm`] exists, so the affirmative control is a different
    /// control on a different screen whose rect does not overlap this one's. A
    /// press of this button now returns
    /// [`WorldSelectOutcome::DeleteWorld`], which *asks*.
    ///
    /// **Read this for "could it ever be active", never for "is it active".**
    /// [`WorldSelectNav::is_active`] is the live fact, and consulting the enum
    /// instead was a real defect here — see that method.
    #[must_use]
    pub fn enabled(self) -> bool {
        match self {
            WorldSelectButton::Create | WorldSelectButton::Back => true,
            // Selection-dependent; `update_button_status` narrows it.
            //
            // `Delete` joined this arm in that fix, when the confirmation
            // screen it was waiting for landed — see this method's own doc.
            WorldSelectButton::Play | WorldSelectButton::Delete => true,
            WorldSelectButton::Edit | WorldSelectButton::ReCreate => false,
        }
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
    /// The header's search field. Filters the list by
    /// `WorldSelectionList.filterAccepts`.
    pub search: EditBox,
    /// The footer buttons, in [`WORLD_SELECT_BUTTONS`]' order.
    pub buttons: [Widget; WORLD_SELECT_BUTTONS.len()],
    /// One widget per **visible** world row, in the order the rows draw.
    ///
    /// A `Widget` rather than a bare rect because that is what makes a row a
    /// focus target at all — `AbstractSelectionList`'s entries are
    /// `GuiEventListener`s in vanilla too. Its `message` is never drawn (the row
    /// draws three of its own text lines); it carries the display name so a
    /// narration/tooltip layer would have somewhere to read it.
    pub worlds: Vec<Widget>,
}

impl FocusChildren for WorldSelectWidgets {
    fn get(&self, id: usize) -> Option<&dyn FocusTarget> {
        if id == SEARCH_FIELD {
            return Some(&self.search as &dyn FocusTarget);
        }
        if let Some(i) = id.checked_sub(FIRST_WORLD_ROW) {
            return self.worlds.get(i).map(|w| w as &dyn FocusTarget);
        }
        let i = id.checked_sub(FIRST_BUTTON_ROW)?;
        self.buttons.get(i).map(|w| w as &dyn FocusTarget)
    }

    fn get_mut(&mut self, id: usize) -> Option<&mut dyn FocusTarget> {
        if id == SEARCH_FIELD {
            return Some(&mut self.search as &mut dyn FocusTarget);
        }
        if let Some(i) = id.checked_sub(FIRST_WORLD_ROW) {
            return self.worlds.get_mut(i).map(|w| w as &mut dyn FocusTarget);
        }
        let i = id.checked_sub(FIRST_BUTTON_ROW)?;
        self.buttons.get_mut(i).map(|w| w as &mut dyn FocusTarget)
    }
}

/// What one key or click did to the screen, from [`super::nav::MenuNav`]'s point
/// of view. Only [`Self::Close`] needs the screen's cooperation; the same
/// distinction [`super::nav::FormOutcome`] draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldSelectOutcome {
    /// A widget or the focus layer dealt with it.
    Handled,
    /// Escape, or the Back button: leave for the title screen.
    Close,
    /// Play Selected World: open the world whose **folder name** this carries.
    ///
    /// The folder name and not a path, and not a [`crate::saves::WorldSummary`]:
    /// this module holds no root (`MenuNav` does, so that one place decides where
    /// `saves/` is and a test can point it at a temp directory), and the folder
    /// name is the only field of a summary the launcher needs — resolving it goes
    /// through [`crate::saves::world_dir_in`], which is also the containment
    /// check.
    ///
    /// It used to carry nothing at all, because there was one hardcoded world.
    Play(String),
    /// Create New World: open [`super::Screen::CreateWorld`].
    CreateWorld,
    /// Delete: open the **confirmation** for this world.
    ///
    /// Deliberately not `Delete(String)`-and-do-it: this variant is a request to
    /// *ask*, and nothing anywhere in this module can remove a directory. The
    /// folder name is what [`crate::saves::delete_world_in`] resolves (through
    /// [`crate::saves::world_dir_in`], the containment check); the display name
    /// rides along because vanilla's `selectWorld.deleteWarning` interpolates
    /// `LevelSummary.getLevelName()` rather than the folder
    /// (`WorldSelectionList.java:633`), and quoting the wrong one of the two is
    /// exactly how a player confirms the deletion of a different world.
    DeleteWorld {
        /// The folder under the saves root.
        dir_name: String,
        /// The name to quote in the warning.
        display_name: String,
    },
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
    /// Every world on disk, already sorted by
    /// [`crate::saves::WorldSummary::cmp_for_list`].
    ///
    /// The **unfiltered** set. [`Self::shown`] is the filtered view, and the two
    /// are kept apart so typing in the search box does not lose worlds — vanilla
    /// keeps `currentlyDisplayedLevels` for the same reason
    /// (`WorldSelectionList.java:185-191`: `updateFilter` re-fills from the
    /// retained list rather than re-reading the disk).
    worlds: Vec<WorldSummary>,
    /// Indices into [`Self::worlds`] that pass the search filter, in list order.
    ///
    /// A row index is an index into **this**, not into `worlds`: the rows the
    /// player sees are the filtered ones, and a click on visible row 2 must
    /// select the third *visible* world.
    shown: Vec<usize>,
    /// Which visible row is the list's selection — `getSelectedOpt()`.
    ///
    /// An index into [`Self::shown`]. `None` is vanilla's
    /// `updateButtonStatus(null)` state, which this screen really can be in now
    /// (an empty list, or a filter that matches nothing) rather than never.
    selected: Option<usize>,
    /// How far the list is scrolled, **in logical pixels**.
    ///
    /// Pixels rather than a row index for that fix's reason: one wheel notch is
    /// `scrollRate = defaultEntryHeight / 2` = 18 px, and a row-quantised offset
    /// cannot represent that at all. Owned here rather than on
    /// [`super::nav::MenuNav`] — unlike the multiplayer list's — because focus
    /// lives here, and **scroll-into-view has to happen wherever focus moves**:
    /// that is the whole fix for "Tab can focus a row that is not drawn".
    scroll: f32,
    /// A message to show above the footer — a create failure, mainly.
    ///
    /// Set by `MenuNav` rather than by this screen, for the same reason
    /// `MenuNav::save_error` is: the filesystem write happens where the root is
    /// known.
    error: Option<String>,
}

impl Default for WorldSelectNav {
    fn default() -> Self {
        Self::new()
    }
}

impl WorldSelectNav {
    /// A fresh screen with **no worlds**.
    ///
    /// Deliberately does no filesystem work at all, which is what makes it safe
    /// for `MenuNav::new`'s field initialiser and for every unit test in this
    /// tree: a constructor that enumerated the real `saves/` would be an
    /// OS-side-effect-in-a-test defect of exactly the shape `CLAUDE.md` §12.44
    /// records. [`Self::with_worlds`] is how a real list gets in, and
    /// `MenuNav`'s Singleplayer arm is what calls it.
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
        let widgets = WorldSelectWidgets {
            search,
            buttons,
            worlds: Vec::new(),
        };
        let mut nav = Self {
            widgets,
            focus: FocusSet::new(),
            hovered: None,
            worlds: Vec::new(),
            shown: Vec::new(),
            selected: None,
            scroll: 0.0,
            error: None,
        };
        nav.rebuild();
        nav
    }

    /// A fresh screen listing `worlds`, which
    /// [`crate::saves::list_worlds_in`] already sorted.
    ///
    /// This is the constructor `MenuNav` uses on entry to the screen, matching
    /// vanilla constructing a brand-new `SelectWorldScreen` every time
    /// `TitleScreen`'s Singleplayer button is pressed — so the list is re-read
    /// rather than cached, and a world created a moment ago appears.
    #[must_use]
    pub fn with_worlds(worlds: Vec<WorldSummary>) -> Self {
        let mut nav = Self::new();
        nav.worlds = worlds;
        nav.rebuild();
        nav
    }

    /// Re-apply the filter, re-seed the row widgets, re-derive the selection and
    /// re-register focus.
    ///
    /// **One function rather than four**, because the four are not independent:
    /// the row widget count comes from the filter, the selection has to be
    /// clamped to it, focus ids only exist for rows that do, and
    /// [`Self::update_button_status`] reads the selection. Splitting them is how
    /// a filtered list ends up with a focus id pointing past the end of
    /// `widgets.worlds` — which `FocusChildren::get` answers `None` for, so the
    /// symptom would be a dead keyboard rather than a panic.
    fn rebuild(&mut self) {
        let filter = self.widgets.search.value().to_lowercase();
        // Keep the previously selected *world* selected across a filter change
        // where possible, rather than the previously selected row index — the
        // index means a different world after the filter moves.
        let previously = self
            .selected
            .and_then(|row| self.shown.get(row).copied());

        // `WorldSelectionList.filterAccepts` (`:233-235`): a case-insensitive
        // substring of the display name **or** the folder name. Both, not just
        // the name — a player who renamed a world can still find it by folder.
        self.shown = self
            .worlds
            .iter()
            .enumerate()
            .filter(|(_, world)| {
                filter.is_empty()
                    || world.display_name.to_lowercase().contains(&filter)
                    || world.dir_name.to_lowercase().contains(&filter)
            })
            .map(|(i, _)| i)
            .collect();

        // One seeded widget per row — **every** row, not the first
        // `max_visible_world_rows()` of them. The cap was what made a
        // world past the tenth unreachable, and removing it is only safe because
        // focus now scrolls itself into view ([`Self::scroll_to_focus`]): a
        // focusable row with no rect is a trap, and the fix is to give it a rect
        // rather than to refuse it focus.
        //
        // The rects come from `render::world_list_row_rect`, the same expression
        // the draw and `app.rs`'s hit-test read, so arrow navigation is geometric
        // against the real geometry rather than against a restatement of it —
        // seeded at **scroll 0** deliberately: the offset shifts every row by the
        // same amount, so it cannot change which row is above which or whether two
        // overlap in x, and those are the only facts geometric navigation reads.
        // Built into a local first: the closure reads `self` (for `world_at`)
        // while the destination is a field of `self`, which cannot be one
        // expression.
        let rows: Vec<Widget> = (0..self.shown.len())
            .map(|row| {
                let (x, y, w, h) = super::render::world_list_row_rect(row, SEED_CANVAS.0, 0.0);
                let world = self.world_at(row);
                let mut widget = Widget::new(
                    x,
                    y,
                    w,
                    h,
                    world.map_or_else(String::new, |world| world.display_name.clone()),
                );
                // `LevelSummary.primaryActionActive` — a corrupt world is listed
                // but not openable, so its row must not be a tab stop either.
                widget.active = world.is_some_and(WorldSummary::can_play);
                widget
            })
            .collect();
        self.widgets.worlds = rows;

        // The selection: the same world as before if it survived the filter,
        // otherwise row 0 (the most recently played — see the module docs on why
        // this screen selects on open where vanilla waits for a click), or
        // nothing at all when the list is empty.
        self.selected = previously
            .and_then(|world| self.shown.iter().position(|i| *i == world))
            .or(if self.shown.is_empty() { None } else { Some(0) });

        let focused = self.focus.focused();
        let mut focus = FocusSet::new();
        // `layout.visitWidgets(this::addRenderableWidget)` (`:76`), in the
        // header → contents → footer order `HeaderAndFooterLayout.visitChildren`
        // walks (`:84-89`) — which is also the tab order, since nothing here
        // overrides `getTabOrderGroup`. The *ids* are not in that order (see
        // [`FIRST_WORLD_ROW`]); the registration is, and registration is what Tab
        // follows.
        focus.add_renderable_widget(SEARCH_FIELD);
        for row in 0..self.shown.len() {
            focus.add_renderable_widget(FIRST_WORLD_ROW + row);
        }
        for b in WORLD_SELECT_BUTTONS {
            focus.add_renderable_widget(b.row());
        }
        self.focus = focus;
        // `updateFilter` shortened or lengthened the list; the offset survives it
        // (vanilla's `clearEntries` does not reset one) but must be re-clamped.
        self.clamp_scroll();
        match focused.filter(|id| self.widgets.get(*id).is_some()) {
            // A *rebuild* (the player typed into the search box): put the focus
            // back where it was, or on the search box if the widget it was on went
            // away with its row.
            //
            // **`set_focused`, not `set_initial_focus`, and the difference is a
            // real bug rather than a style choice.** `set_initial_focus` offers the
            // widget an `InitialFocus` event and honours `takes_focus()`, which is
            // `is_active() && !is_focused()` — and the widget the fresh `FocusSet`
            // has forgotten still carries `focused = true` from the old one. So the
            // offer is *declined*, nothing is set, and the set and the widget
            // disagree: `focused_row()` answers `None` while the search box draws a
            // caret. That is what the first draft of this function did, and ten
            // tests said so.
            Some(id) => {
                // Drop the stale flag first so `set_focused`'s own `self.focused ==
                // next` early return cannot short-circuit on a fresh set whose
                // `focused` is `None`.
                if let Some(child) = self.widgets.get_mut(id) {
                    child.set_focused(false);
                }
                self.focus.set_focused(&mut self.widgets, Some(id));
            }
            // `setInitialFocus(this.searchBox)` (`:147-152`) — the explicit
            // overload, for `EditForm::adding`'s reason: the no-argument one is
            // gated on a last-input-type this shell does not track, and without it
            // the first keystroke would go nowhere.
            None => {
                self.focus.set_initial_focus(&mut self.widgets, SEARCH_FIELD);
            }
        }
        self.update_button_status();
    }

    /// The world shown at visible row `row`.
    #[must_use]
    pub fn world_at(&self, row: usize) -> Option<&WorldSummary> {
        self.worlds.get(*self.shown.get(row)?)
    }

    /// How many world rows the list is showing, after the filter.
    #[must_use]
    pub fn shown_len(&self) -> usize {
        self.shown.len()
    }

    /// Every world on disk, unfiltered — for a gate that needs to know what was
    /// enumerated as distinct from what is displayed.
    #[must_use]
    pub fn worlds(&self) -> &[WorldSummary] {
        &self.worlds
    }

    /// The message drawn above the footer, if any. See [`Self::error`].
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Record a failure to show the player — `MenuNav`'s create path calls this.
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
    }

    /// `SelectWorldScreen.updateButtonStatus(summary)` (`:159-184`), now with a
    /// real summary to ask.
    ///
    /// Vanilla's non-null branch reads four `LevelSummary` predicates —
    /// `primaryActionActive()`, `canEdit()`, `canRecreate()`, `canDelete()`
    /// (`LevelSummary.java:189-211`) — plus a `requiresFileFixing()` tooltip. All
    /// four are asked here, against
    /// [`crate::saves::WorldSummary`]'s own ports of them, and then `&&`-ed with
    /// [`WorldSelectButton::enabled`]'s client-level ceiling: a world may be
    /// deletable while this client has nowhere to confirm the deletion, and the
    /// button must then be off. The tooltip is still unported (that fix's reason:
    /// nothing tracks hover dwell time).
    ///
    /// The null branch — no selection — turns all four off, which is a state this
    /// screen can really be in now: an empty `saves/`, or a search that matches
    /// nothing.
    fn update_button_status(&mut self) {
        let selected = self.selected.and_then(|row| self.world_at(row)).cloned();
        for (widget, button) in self.widgets.buttons.iter_mut().zip(WORLD_SELECT_BUTTONS) {
            let allowed_by_selection = match button {
                WorldSelectButton::Create | WorldSelectButton::Back => true,
                WorldSelectButton::Play => {
                    selected.as_ref().is_some_and(WorldSummary::can_play)
                }
                WorldSelectButton::Edit => {
                    selected.as_ref().is_some_and(WorldSummary::can_edit)
                }
                WorldSelectButton::ReCreate => {
                    selected.as_ref().is_some_and(WorldSummary::can_recreate)
                }
                WorldSelectButton::Delete => {
                    selected.as_ref().is_some_and(WorldSummary::can_delete)
                }
            };
            widget.active = button.enabled() && allowed_by_selection;
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
        if row == SEARCH_FIELD
            || WorldSelectButton::at_row(row).is_some()
            || self.world_row(row).is_some()
        {
            self.hovered = Some(row);
        }
    }

    /// The visible world-list row `row` names, or `None` when it names something
    /// else (or a world row that is not currently shown).
    ///
    /// The bounds check against [`Self::shown`] is what stops a **stale** row id
    /// — the search box narrowing the list between a hover and the next frame —
    /// selecting whatever now happens to be at that index.
    #[must_use]
    pub fn world_row(&self, row: usize) -> Option<usize> {
        let index = row.checked_sub(FIRST_WORLD_ROW)?;
        (index < self.shown.len()).then_some(index)
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
        // Captured before anything can edit the box, because the search box's
        // responder (below) fires on a *change* and both branches can cause one.
        let before = self.widgets.search.value().to_string();
        // A printable character is `charTyped`, a different callback.
        if let MenuKey::Char(ch) = key {
            self.focus.char_typed(&mut self.widgets, ch);
            if self.widgets.search.value() != before {
                self.rebuild();
            }
            return WorldSelectOutcome::Handled;
        }
        let Some(event) = KeyEvent::from_menu_key(key) else {
            return WorldSelectOutcome::Handled;
        };
        let outcome = match self.focus.screen_key_pressed(&mut self.widgets, event) {
            KeyOutcome::Close => WorldSelectOutcome::Close,
            KeyOutcome::Consumed | KeyOutcome::FocusMoved => {
                // Focus landing on a list row *is* a selection change —
                // `AbstractSelectionList.nextFocusPath` calls `setSelected` on the
                // entry it moves to, which is why arrowing through the list keeps
                // Play pointed at the row that is highlighted rather than at
                // whatever was clicked last.
                self.sync_selection_to_focus();
                WorldSelectOutcome::Handled
            }
            // `AbstractButton.keyPressed` presses a focused, *active* button on
            // Enter or Space and returns `true` (`AbstractButton.java:61-71`).
            // Our `Widget` is data with no press callback, so the screen applies
            // that here instead; the observable behaviour is the same, and an
            // inactive button never gets here because it cannot hold focus.
            //
            // Enter on a focused **list row** is `joinWorld` — vanilla reaches it
            // through `AbstractSelectionList.keyPressed`'s Enter arm on the
            // selected entry, which is the keyboard twin of the double-click.
            KeyOutcome::Declined if key == MenuKey::Enter => match self.focused_row() {
                Some(row) if self.world_row(row).is_some() => self.play_selected(),
                _ => self.press_focused(),
            },
            KeyOutcome::Declined => WorldSelectOutcome::Handled,
        };
        // `this.searchBox.setResponder(list::updateFilter)`
        // (`SelectWorldScreen.java:63`). Gated on the value actually changing so a
        // Backspace on an empty box does not rebuild the whole list — and, more
        // importantly, so a keystroke that only moved focus does not reset the
        // selection through `rebuild`.
        if self.widgets.search.value() != before {
            self.rebuild();
        }
        outcome
    }

    /// Type `ch` into the search box, whatever has focus.
    ///
    /// The filter path a *test* drives without going through `MenuNav`; the
    /// production path is [`Self::handle_key`], and both end at [`Self::rebuild`].
    #[cfg(test)]
    fn type_into_search(&mut self, text: &str) {
        self.focus.set_focused(&mut self.widgets, Some(SEARCH_FIELD));
        for ch in text.chars() {
            self.handle_key(MenuKey::Char(ch));
        }
    }

    /// If focus is on a world row, make that row the selection — and scroll it
    /// into view.
    ///
    /// The scroll is not optional and not cosmetic: it is what stops focus landing
    /// on a row that is not drawn. Vanilla joins the two the same way
    /// — `setSelected` calls `scrollToEntry` whenever the last input was the
    /// keyboard (`AbstractSelectionList.java:53-62`) — and it is called
    /// unconditionally here rather than only when focus moved *onto* the list,
    /// because [`Self::scroll_to_focus`] is a no-op for a focus that is not on a
    /// row.
    fn sync_selection_to_focus(&mut self) {
        if let Some(row) = self.focus.focused()
            && let Some(index) = self.world_row(row)
        {
            self.selected = Some(index);
            self.update_button_status();
        }
        self.scroll_to_focus();
    }

    /// A left-click that landed on row `row`.
    ///
    /// **Its own arm rather than "hover then Enter"**, which is the translation
    /// that caused that fix on the settings screen and, one screen over, made
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
        // A click on a list row **selects** it and does not launch —
        // `WorldSelectionList.WorldListEntry.mouseClicked` (`:571-583`) only
        // joins on a `doubleClick` or on a click inside the 32×32 icon, and a
        // single click elsewhere falls through to `AbstractSelectionList`'s own
        // `setSelected`. Launching on a single click would make Play Selected
        // World unreachable: you could never point at a world without opening it.
        //
        // The **double-click** half is deliberately not wired here: `MenuNav`
        // owns the [`super::focus::DoubleClickTracker`] (it needs a clock, which
        // this pure module has none of), and it is what turns a second click into
        // Play. See `MenuNav::apply_world_select`.
        if let Some(index) = self.world_row(row) {
            // **Selection and activation are two facts, and conflating them made
            // a corrupt world impossible to remove**. This branch
            // used to return early for an inactive row, so clicking the one world
            // whose `level.dat` will not decode left the selection where it was —
            // and Delete acts on the *selection*, so vanilla's
            // "`canDelete()` is unconditionally `true`, including for a corrupt
            // world" could not be reached from the UI at all.
            //
            // Vanilla has no such coupling: `AbstractSelectionList.setSelected`
            // runs for any entry, and `primaryActionActive()` gates only
            // `joinWorld`. So a click selects any row, and the row's own `active`
            // flag — which stays `can_play()` — still decides two separate
            // things: whether it can take **focus** (so a corrupt row is never a
            // tab stop, as before) and whether Play lights up (it does not).
            self.selected = Some(index);
            if self.is_active(row) {
                self.focus.set_focused(&mut self.widgets, Some(row));
                // `setSelected`'s `topClipped || bottomClipped` branch
                // (`AbstractSelectionList.java:55-61`): a click can land on a row
                // that is only half inside the band, and that row then has to come
                // fully in — it is the focused one.
                self.scroll_to_focus();
            }
            self.update_button_status();
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
            WorldSelectButton::Play => self.play_selected(),
            // Opens `Screen::CreateWorld`.
            WorldSelectButton::Create => WorldSelectOutcome::CreateWorld,
            // Vanilla's `list.getSelectedOpt().ifPresent(WorldListEntry::deleteWorld)`
            // (`SelectWorldScreen.java:94-95`), whose `deleteWorld` opens a
            // `ConfirmScreen` and deletes nothing (`WorldSelectionList.java:619-637`).
            // That fix.
            WorldSelectButton::Delete => self.delete_selected(),
            // Edit and Re-Create have no screen to open, so both are inactive and
            // neither press path reaches them — spelled out anyway so enabling one
            // without giving it an action is a compile error rather than a
            // silently dead button.
            WorldSelectButton::Edit | WorldSelectButton::ReCreate => {
                WorldSelectOutcome::Handled
            }
        }
    }

    /// `loadSelectedWorld()`: open the selection, or do nothing when there is
    /// none.
    ///
    /// `Handled` rather than a panic for an absent selection because the button
    /// that reaches this is *already* inactive in that state
    /// ([`Self::update_button_status`]) — so this is the second of two guards,
    /// and the first one being right is what makes it unreachable rather than
    /// what makes it safe.
    ///
    /// **The `can_play` half of the guard is not decoration**. It was
    /// implicit until then — a corrupt world could not be *selected*, so this
    /// could never see one — and making the corrupt row selectable so it could be
    /// deleted removed that implicit protection. The gate is vanilla's own:
    /// `WorldListEntry.joinWorld` opens with `if (this.summary.primaryActionActive())`
    /// (`WorldSelectionList.java:610`), i.e. the check lives in the *action* and
    /// not only in the button's `active` flag.
    /// `a_corrupt_worlds_row_is_selectable_and_deletable_but_never_playable` is
    /// what caught the gap, and it is the gate on it.
    pub fn play_selected(&mut self) -> WorldSelectOutcome {
        match self.selected() {
            Some(world) if world.can_play() => WorldSelectOutcome::Play(world.dir_name.clone()),
            _ => WorldSelectOutcome::Handled,
        }
    }

    /// `WorldListEntry.deleteWorld()`: **ask** about the selection, or do nothing
    /// when there is none.
    ///
    /// The name is `delete_selected` and it deletes nothing, which is deliberate —
    /// it is what vanilla's method of that name does too. The only thing in this
    /// crate that removes a directory is [`crate::saves::delete_world_in`], and
    /// the only caller of *that* is `MenuNav::apply_confirm`'s affirmative arm.
    pub fn delete_selected(&mut self) -> WorldSelectOutcome {
        match self.selected() {
            Some(world) => WorldSelectOutcome::DeleteWorld {
                dir_name: world.dir_name.clone(),
                display_name: world.display_name.clone(),
            },
            None => WorldSelectOutcome::Handled,
        }
    }

    /// The selected world — `WorldSelectionList.getSelectedOpt()`.
    ///
    /// `None` is a state this screen really reaches now (an empty `saves/`, or a
    /// search matching nothing), where it used to be unreachable because there
    /// was exactly one hardcoded world.
    #[must_use]
    pub fn selected(&self) -> Option<&WorldSummary> {
        self.world_at(self.selected?)
    }

    /// Which visible row is selected, for the draw's highlight.
    #[must_use]
    pub fn selected_row(&self) -> Option<usize> {
        self.selected
    }

    /// How far the list is scrolled, in logical pixels.
    ///
    /// The value the frame stamps onto every row and the scrollbar's thumb is
    /// placed from. It may sit past a *tall* canvas's own `maxScrollAmount` —
    /// see [`super::render::world_list_scroll_for`], which is the one place that
    /// re-clamps it, and why the clamp cannot live here.
    #[must_use]
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// One mouse-wheel notch at a `canvas_height`-tall canvas —
    /// `AbstractScrollArea::mouseScrolled`,
    /// `setScrollAmount(scrollAmount() - scrollY * scrollRate())`
    /// (`AbstractScrollArea.java:34`).
    ///
    /// **Delegates to [`super::widget::ScrollList`] rather than reimplementing the
    /// arithmetic**, which is what makes one notch 18 px rather than a whole 36 px
    /// entry: that type owns `scrollRate = defaultEntryHeight / 2` and
    /// `setScrollAmount`'s clamp, both already gated against the jar. `notches` is
    /// winit's `scrollY` verbatim, so positive scrolls **up**; the negation lives
    /// in `mouse_scrolled` so there is exactly one place the sign can be wrong.
    pub fn scroll_by(&mut self, notches: f32, canvas_height: f32) {
        let Some(mut list) =
            super::render::world_scroll_model(self.shown.len(), canvas_height)
        else {
            return;
        };
        list.set_scroll(self.scroll);
        list.mouse_scrolled(notches);
        self.scroll = list.scroll();
    }

    /// Bring the **focused** row into the band —
    /// `AbstractSelectionList.scrollToEntry` (`:251-261`), reached in vanilla
    /// through `setSelected`'s keyboard branch (`:53-62`).
    ///
    /// This is the fix for the one thing that fix called genuinely wrong rather than
    /// merely limited: Tab could focus a row that was not drawn. With the row cap
    /// gone every world has a widget, so instead of refusing focus the list
    /// *follows* it, and `world_list_row_visible` then reports the focused row as
    /// on screen at every step.
    ///
    /// Runs against [`super::render::world_list_window_rows`]' conservative
    /// shortest band, because a keypress has no canvas — the same trade
    /// `scroll_server_to_show` makes. The residue is stated rather than hidden: at
    /// a **taller** canvas this can ask for more scroll than that canvas's own
    /// maximum, so `world_list_scroll_for` re-clamps at draw time and the visible
    /// effect is that arrowing down reaches the bottom of the list slightly
    /// earlier than it strictly had to. Never the other way round — a focused row
    /// is always inside the band.
    fn scroll_to_focus(&mut self) {
        let Some(row) = self.focus.focused().and_then(|id| self.world_row(id)) else {
            return;
        };
        let row_h = super::render::WORLD_LIST_ITEM_H;
        let window_px = super::render::world_list_window_rows() as f32 * row_h;
        let row_top = row as f32 * row_h;
        // Both deltas measured against the *current* offset and applied in order,
        // exactly as `scrollToEntry` does, so this is the minimum move that brings
        // the row fully into the band rather than a whole-window jump.
        if row_top < self.scroll {
            self.scroll = row_top;
        } else if row_top + row_h > self.scroll + window_px {
            self.scroll = row_top + row_h - window_px;
        }
        self.clamp_scroll();
    }

    /// Keep [`Self::scroll`] inside the range the **conservative** window can
    /// justify — vanilla's `refreshScrollAmount`, which `updateSizeAndPosition`
    /// runs after every resize (`AbstractSelectionList.java:191-195`) and which
    /// `updateFilter` needs for the same reason: `clearEntries` does **not** reset
    /// the offset (`:84-87`), so a filter that shortens the list would otherwise
    /// leave it scrolled past the new end.
    ///
    /// Conservative, so it cannot be the only clamp — see
    /// [`super::render::world_list_scroll_for`] for the canvas-aware one.
    fn clamp_scroll(&mut self) {
        let row_h = super::render::WORLD_LIST_ITEM_H;
        let window_px = super::render::world_list_window_rows() as f32 * row_h;
        let max = (self.shown.len() as f32 * row_h - window_px).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// What the empty-list row says, or `None` when the list is not empty.
    ///
    /// `Some` is vanilla's `NoWorldsEntry` (see the module docs on the deviation
    /// this represents), and it is what keeps "no worlds" distinguishable from "a
    /// list that failed to draw" — the two are otherwise the same picture, which
    /// is the absence-needs-a-control rule applied to a screen.
    #[must_use]
    pub fn empty_label(&self) -> Option<&'static str> {
        self.shown.is_empty().then_some(NO_WORLDS_LABEL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`WorldSummary`] with the fields a list gate cares about.
    ///
    /// Built by hand rather than read off disk: this module's job is the
    /// *screen*, and `crate::saves`'s own tests already gate the enumeration
    /// against real `level.dat` files. Nothing here touches a filesystem at all,
    /// which is also what makes every test below safe to run concurrently.
    fn world(dir: &str, name: &str, last_played: i64) -> WorldSummary {
        WorldSummary {
            dir_name: dir.to_string(),
            display_name: name.to_string(),
            last_played,
            game_type: Some(0),
            version_name: Some("26.2".to_string()),
            allow_commands: false,
            hardcore: false,
            readable: true,
        }
    }

    /// **The fixture, stated so the `world` species of vacuous test is visible
    /// from the source.** Three worlds, already in `cmp_for_list` order (most
    /// recently played first), with distinct display and folder names so the
    /// filter can be shown to look at both, plus a **corrupt** one so
    /// `can_play == false` is exercised rather than assumed:
    ///
    /// | row | folder | name | last played | readable |
    /// |---|---|---|---|---|
    /// | 0 | `alpha` | `Alpha World` | 3000 | yes |
    /// | 1 | `bravo` | `Bravo World` | 2000 | yes |
    /// | 2 | `zulu` | `Corrupt` | 1000 | **no** |
    fn fixture() -> Vec<WorldSummary> {
        let mut corrupt = world("zulu", "Corrupt", 1_000);
        corrupt.readable = false;
        vec![
            world("alpha", "Alpha World", 3_000),
            world("bravo", "Bravo World", 2_000),
            corrupt,
        ]
    }

    fn populated() -> WorldSelectNav {
        let nav = WorldSelectNav::with_worlds(fixture());
        // The precondition. Without it every assertion below is verified against
        // whatever `fixture` happens to contain.
        assert_eq!(nav.shown_len(), 3, "premise: three rows");
        assert!(nav.world_at(0).is_some_and(|w| w.readable));
        assert!(
            nav.world_at(2).is_some_and(|w| !w.readable),
            "premise: row 2 is a corrupt world, or `can_play == false` is never \
             exercised and the disabled-row assertions are vacuous"
        );
        nav
    }

    /// Every button is present; **two** of them can never be active in this
    /// client.
    ///
    /// The count is asserted both ways round on purpose: "two that can never be
    /// active" is what makes the screen honest about what this client cannot do
    /// (Edit and Re-Create have no screen to open), and Play/Create/Delete/Back
    /// being reachable is the four closed fixes that built this screen.
    ///
    /// It was **three** until that fix, and the one that moved is the whole of
    /// that issue: Delete's ceiling was `false` because deleting a world needs a
    /// confirmation the player cannot fire by accident, and
    /// [`super::confirm`] is now that confirmation.
    #[test]
    fn two_of_the_six_footer_buttons_can_never_be_active() {
        assert_eq!(WORLD_SELECT_BUTTONS.len(), 6, "vanilla has six footer buttons");
        let enabled: Vec<_> = WORLD_SELECT_BUTTONS
            .iter()
            .copied()
            .filter(|b| b.enabled())
            .collect();
        assert_eq!(
            enabled,
            vec![
                WorldSelectButton::Play,
                WorldSelectButton::Create,
                WorldSelectButton::Delete,
                WorldSelectButton::Back
            ],
            "Play opens the selection (#468); Create opens the new screen (#190); \
             Delete opens the confirmation (#540); Back leaves"
        );
        assert_eq!(WorldSelectButton::Create.label(), "Create New World");
        for b in [WorldSelectButton::Edit, WorldSelectButton::ReCreate] {
            assert!(!b.enabled(), "{b:?} must still be present and inactive");
        }
    }

    /// Pressing Delete **asks**; it does not delete.
    ///
    /// The strongest thing this module can assert about safety, because it is the
    /// whole of what this module is allowed to do: the outcome is a request naming
    /// the world, and there is no variant that removes anything. The second half
    /// is the one that would have caught the wrong world being named — the request
    /// carries the *folder* for the delete and the *display name* for the warning,
    /// which are two different strings on purpose.
    #[test]
    fn pressing_delete_asks_about_the_selected_world_rather_than_deleting_it() {
        let mut nav = populated();
        assert!(nav.is_active(WorldSelectButton::Delete.row()), "live for a selection");
        assert_eq!(
            nav.click_row(WorldSelectButton::Delete.row()),
            WorldSelectOutcome::DeleteWorld {
                dir_name: "alpha".to_string(),
                display_name: "Alpha World".to_string(),
            }
        );
        // It follows the selection, not row 0 — the failure this rules out is a
        // confirmation naming one world and removing another.
        nav.click_row(FIRST_WORLD_ROW + 1);
        assert_eq!(
            nav.click_row(WorldSelectButton::Delete.row()),
            WorldSelectOutcome::DeleteWorld {
                dir_name: "bravo".to_string(),
                display_name: "Bravo World".to_string(),
            }
        );

        // -- control ---------------------------------------------------------
        // With nothing selected the button is inactive and the press does
        // nothing, so the assertions above are about the selection rather than
        // about Delete always answering.
        let mut empty = WorldSelectNav::new();
        assert!(!empty.is_active(WorldSelectButton::Delete.row()));
        assert_eq!(
            empty.click_row(WorldSelectButton::Delete.row()),
            WorldSelectOutcome::Handled
        );
        assert_eq!(empty.delete_selected(), WorldSelectOutcome::Handled);
    }

    /// The empty list — a fresh install, which is the state the owner hits first.
    ///
    /// It must be a list with a message, not a crash and not a blank band: Create
    /// stays live, Play does not, and [`WorldSelectNav::empty_label`] is `Some` so
    /// the draw has something to distinguish "no worlds" from "the list failed".
    #[test]
    fn an_empty_saves_directory_is_a_usable_screen_and_not_a_dead_one() {
        let nav = WorldSelectNav::new();
        assert_eq!(nav.shown_len(), 0);
        assert!(nav.selected().is_none(), "nothing to select");
        assert_eq!(nav.selected_row(), None);
        assert_eq!(nav.empty_label(), Some(NO_WORLDS_LABEL));
        assert!(
            !nav.is_active(WorldSelectButton::Play.row()),
            "Play must be greyed with nothing to play — `updateButtonStatus(null)`"
        );
        assert!(
            nav.is_active(WorldSelectButton::Create.row()),
            "Create must stay live, or an empty list is a dead end"
        );
        assert!(nav.is_active(WorldSelectButton::Back.row()));
        // Pressing the greyed Play does nothing at all rather than panicking on
        // an absent selection.
        let mut nav = nav;
        assert_eq!(
            nav.click_row(WorldSelectButton::Play.row()),
            WorldSelectOutcome::Handled
        );

        // -- control ---------------------------------------------------------
        // The assertions above are only about *emptiness* if the same screen with
        // worlds answers differently. Otherwise they would pass for a screen that
        // never activates Play at all.
        let with_worlds = populated();
        assert_eq!(with_worlds.empty_label(), None, "a populated list has no notice");
        assert!(
            with_worlds.is_active(WorldSelectButton::Play.row()),
            "Play must be live with a playable selection, or the empty-list \
             assertion above measures nothing"
        );
    }

    /// A populated list selects its first row — the most recently played world,
    /// because [`crate::saves::WorldSummary::cmp_for_list`] sorted it there.
    #[test]
    fn a_populated_list_selects_the_most_recently_played_world() {
        let nav = populated();
        assert_eq!(nav.selected_row(), Some(0));
        assert_eq!(
            nav.selected().map(|w| w.dir_name.as_str()),
            Some("alpha"),
            "row 0 is the most recently played (last_played 3000)"
        );
        assert!(nav.is_active(WorldSelectButton::Play.row()));
        // Play carries the **folder** name, which is what `MenuNav` resolves
        // against its saves root.
        let mut nav = nav;
        assert_eq!(
            nav.click_row(WorldSelectButton::Play.row()),
            WorldSelectOutcome::Play("alpha".to_string())
        );
    }

    /// Clicking a row **selects** it and does not launch, and Play then opens
    /// *that* world — the whole point of a list rather than one row.
    #[test]
    fn clicking_a_row_selects_it_and_play_then_opens_that_world() {
        let mut nav = populated();
        assert_eq!(
            nav.click_row(FIRST_WORLD_ROW + 1),
            WorldSelectOutcome::Handled,
            "a single click on a list row must not launch — \
             `WorldListEntry.mouseClicked` only joins on a double-click"
        );
        assert_eq!(nav.selected_row(), Some(1));
        assert_eq!(
            nav.click_row(WorldSelectButton::Play.row()),
            WorldSelectOutcome::Play("bravo".to_string()),
            "Play must open the row that was clicked, not row 0"
        );

        // -- control ---------------------------------------------------------
        // Without this the assertion above passes for a `click_row` that always
        // answers with `bravo` — e.g. one that selected by *index into the
        // unfiltered list* while the caller meant a visible row.
        let mut nav = populated();
        assert_eq!(
            nav.click_row(WorldSelectButton::Play.row()),
            WorldSelectOutcome::Play("alpha".to_string()),
            "with no row click, Play must still open row 0"
        );
    }

    /// A corrupt world is listed, is not selectable and cannot be played —
    /// vanilla's `CorruptedLevelSummary` reaching this screen's own predicates.
    #[test]
    fn a_corrupt_worlds_row_is_selectable_and_deletable_but_never_playable() {
        let mut nav = populated();
        let row = FIRST_WORLD_ROW + 2;
        assert!(nav.world_row(row).is_some(), "it is on the list");
        assert!(!nav.is_active(row), "and its row is inactive");
        assert_eq!(nav.click_row(row), WorldSelectOutcome::Handled);
        // **This used to assert the selection did not move**, and that was the
        // bug that fix surfaced: Delete acts on the selection, so a corrupt
        // world that could not be selected could not be removed — the one world
        // vanilla most insists you can remove (`canDelete()` is unconditionally
        // `true`). Selection and activation are two facts now; see `click_row`.
        assert_eq!(nav.selected_row(), Some(2), "a click selects any row");
        assert_eq!(
            nav.focused_row(),
            Some(SEARCH_FIELD),
            "but an inactive row still does not take focus, so it is never a tab \
             stop and the next Enter cannot press it"
        );
        // Not playable, and Play is greyed for it — the invariant that must
        // survive the change above.
        assert!(!nav.is_active(WorldSelectButton::Play.row()));
        assert_eq!(nav.play_selected(), WorldSelectOutcome::Handled);
        // But deletable, which is the point.
        assert!(nav.is_active(WorldSelectButton::Delete.row()));
        assert_eq!(
            nav.click_row(WorldSelectButton::Delete.row()),
            WorldSelectOutcome::DeleteWorld {
                dir_name: "zulu".to_string(),
                display_name: "Corrupt".to_string(),
            }
        );

        // The control: the *same* click on a readable row selects **and** focuses,
        // so the focus assertion above is about `readable` and not about
        // `click_row` never focusing a list row.
        assert_eq!(nav.click_row(FIRST_WORLD_ROW + 1), WorldSelectOutcome::Handled);
        assert_eq!(nav.selected_row(), Some(1));
        assert_eq!(nav.focused_row(), Some(FIRST_WORLD_ROW + 1));
        assert!(nav.is_active(WorldSelectButton::Play.row()), "and Play comes back");
    }

    /// The row indices the mouse reports are the ids focus dispatches on, and the
    /// three id bands do not overlap.
    ///
    /// Same guard shape as `the_settings_rows_are_in_the_order_click_assumes`,
    /// and the same that fix it protects: two files agreeing about what row 3 is.
    #[test]
    fn the_three_row_bands_are_contiguous_and_do_not_overlap() {
        assert_eq!(SEARCH_FIELD, 0);
        assert_eq!(FIRST_BUTTON_ROW, SEARCH_FIELD + 1);
        assert_eq!(FIRST_WORLD_ROW, FIRST_BUTTON_ROW + WORLD_SELECT_BUTTONS.len());
        for (i, b) in WORLD_SELECT_BUTTONS.iter().enumerate() {
            assert_eq!(b.row(), FIRST_BUTTON_ROW + i);
            assert_eq!(WorldSelectButton::at_row(b.row()), Some(*b));
        }
        assert_eq!(WorldSelectButton::at_row(SEARCH_FIELD), None);
        assert_eq!(WorldSelectButton::at_row(FIRST_WORLD_ROW), None);

        let nav = populated();
        // No button id is a world row and no world row is a button id.
        for b in WORLD_SELECT_BUTTONS {
            assert_eq!(nav.world_row(b.row()), None, "{b:?} is not a list row");
        }
        for row in 0..nav.shown_len() {
            assert_eq!(nav.world_row(FIRST_WORLD_ROW + row), Some(row));
            assert_eq!(WorldSelectButton::at_row(FIRST_WORLD_ROW + row), None);
        }
        // One past the last world is nothing at all, which is the guard that
        // stops a stale row id (a filter narrowed the list) selecting whatever is
        // now at that index.
        assert_eq!(nav.world_row(FIRST_WORLD_ROW + nav.shown_len()), None);
    }

    /// The widget set is reachable by id in both directions, and the ids are the
    /// rows.
    #[test]
    fn every_widget_is_reachable_through_the_focus_children_seam() {
        let mut nav = populated();
        assert!(nav.widgets.get(SEARCH_FIELD).is_some());
        for b in WORLD_SELECT_BUTTONS {
            assert!(nav.widgets.get(b.row()).is_some(), "{b:?} unreachable");
            assert!(nav.widgets.get_mut(b.row()).is_some(), "{b:?} unreachable (mut)");
        }
        for row in 0..nav.shown_len() {
            let id = FIRST_WORLD_ROW + row;
            assert!(nav.widgets.get(id).is_some(), "world row {row} unreachable");
            assert!(
                nav.widgets.get_mut(id).is_some(),
                "world row {row} unreachable (mut)"
            );
        }
        // Control: a row this screen does not have must be `None`, or the lookup
        // would be answering with whatever it happened to index.
        assert!(
            nav.widgets
                .get(FIRST_WORLD_ROW + nav.shown_len())
                .is_none()
        );
        // `update_button_status` must have written onto every widget: the enum and
        // the widget are one source of truth with a copy, not two sources — see
        // `WorldSelectNav::is_active`. With a playable selection the live flag is
        // exactly the enum's ceiling.
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

    /// Tab visits the list **between** the search field and the footer, which is
    /// vanilla's header → contents → footer walk — even though the world rows'
    /// *ids* are numerically after the buttons'.
    ///
    /// This is the gate that fails if the two facts ([`FIRST_WORLD_ROW`]'s id
    /// band and the registration order) are ever collapsed into one.
    #[test]
    fn tab_visits_the_list_between_the_search_field_and_the_footer() {
        let mut nav = populated();
        assert_eq!(nav.focused_row(), Some(SEARCH_FIELD), "setInitialFocus");
        let mut seen = vec![nav.focused_row()];
        for _ in 0..5 {
            nav.handle_key(MenuKey::Tab);
            seen.push(nav.focused_row());
        }
        assert_eq!(
            seen,
            vec![
                Some(SEARCH_FIELD),
                // The two *readable* world rows. Row 2 is corrupt, so its widget
                // is inactive and `nextFocusPath` steps over it.
                Some(FIRST_WORLD_ROW),
                Some(FIRST_WORLD_ROW + 1),
                Some(WorldSelectButton::Play.row()),
                Some(WorldSelectButton::Create.row()),
                // Delete joined the walk in that fix, between Create and Back,
                // because the footer's registration order is vanilla's own
                // `RowHelper` order and Delete is the fourth cell of it.
                Some(WorldSelectButton::Delete.row()),
            ],
            "tab order is registration order: header, then the list, then the footer"
        );
        assert!(
            !seen.contains(&Some(FIRST_WORLD_ROW + 2)),
            "the corrupt world's row is inactive and must never be a tab stop"
        );
        assert!(
            !seen.contains(&Some(WorldSelectButton::Edit.row())),
            "Edit is inactive and must never be a tab stop"
        );
        // One more Tab reaches Back, which is what makes the six above a prefix of
        // the walk rather than the whole of it.
        nav.handle_key(MenuKey::Tab);
        assert_eq!(nav.focused_row(), Some(WorldSelectButton::Back.row()));

        // Focus landing on a list row **is** a selection change —
        // `AbstractSelectionList.nextFocusPath` calls `setSelected`.
        let mut nav = populated();
        nav.handle_key(MenuKey::Tab);
        nav.handle_key(MenuKey::Tab);
        assert_eq!(nav.focused_row(), Some(FIRST_WORLD_ROW + 1));
        assert_eq!(nav.selected_row(), Some(1), "focus moved the selection");
        // And Enter on a focused list row opens it, which is the keyboard twin of
        // the double-click.
        assert_eq!(
            nav.handle_key(MenuKey::Enter),
            WorldSelectOutcome::Play("bravo".to_string())
        );
    }

    /// With an empty list, Tab visits only the three footer buttons — the shape
    /// this test had before the save list existed, kept because it is the control
    /// for the one above: the list rows appearing in the walk has to be caused by
    /// there *being* rows.
    #[test]
    fn tab_visits_only_the_footer_when_there_are_no_worlds() {
        let mut nav = WorldSelectNav::new();
        let mut seen = vec![nav.focused_row()];
        for _ in 0..3 {
            nav.handle_key(MenuKey::Tab);
            seen.push(nav.focused_row());
        }
        assert_eq!(
            seen,
            vec![
                Some(SEARCH_FIELD),
                // **Not** Play: with nothing selected it is inactive.
                Some(WorldSelectButton::Create.row()),
                Some(WorldSelectButton::Back.row()),
                Some(SEARCH_FIELD),
            ]
        );
    }

    /// Typing filters the list, by display name **or** folder name, and the
    /// selection follows the world rather than the row index.
    #[test]
    fn the_search_box_filters_by_display_name_and_by_folder_name() {
        let mut nav = populated();
        nav.type_into_search("bravo");
        assert_eq!(nav.shown_len(), 1, "one match by folder name");
        assert_eq!(nav.world_at(0).map(|w| w.dir_name.as_str()), Some("bravo"));
        assert_eq!(nav.selected_row(), Some(0));

        // Case-insensitive, and by *display* name — "Alpha World" lives in the
        // folder `alpha`, so a match on "world" can only come from the name.
        let mut nav = populated();
        nav.type_into_search("WORLD");
        assert_eq!(
            nav.shown_len(),
            2,
            "`Alpha World` and `Bravo World` match; `Corrupt`/`zulu` does not"
        );

        // A filter that matches nothing is the second "nothing selected" state,
        // and Play must go grey for it exactly as it does for an empty `saves/`.
        let mut nav = populated();
        nav.type_into_search("nosuchworld");
        assert_eq!(nav.shown_len(), 0);
        assert_eq!(nav.selected_row(), None);
        assert!(!nav.is_active(WorldSelectButton::Play.row()));
        assert_eq!(nav.empty_label(), Some(NO_WORLDS_LABEL));
        assert_eq!(
            nav.worlds().len(),
            3,
            "the filter must not lose worlds — `updateFilter` re-fills from the \
             retained list rather than re-reading the disk"
        );

        // Clearing the filter brings them back.
        for _ in 0.."nosuchworld".len() {
            nav.handle_key(MenuKey::Backspace);
        }
        assert_eq!(nav.shown_len(), 3);

        // The selection follows the **world**, not the row index: select `bravo`
        // (row 1), then filter to a set where it is row 0.
        let mut nav = populated();
        nav.click_row(FIRST_WORLD_ROW + 1);
        assert_eq!(nav.selected().map(|w| w.dir_name.clone()), Some("bravo".into()));
        nav.type_into_search("world");
        assert_eq!(
            nav.selected().map(|w| w.dir_name.clone()),
            Some("bravo".into()),
            "the same world stays selected across a filter change"
        );
        assert_eq!(nav.selected_row(), Some(1), "at its new row");
    }

    /// Typing goes into the field, and the vertical arrows do not.
    #[test]
    fn the_focused_field_takes_text_and_lets_the_vertical_arrows_out() {
        // An empty list, so Down out of the field reaches the footer directly and
        // this test keeps measuring what it always measured (the field/navigation
        // split) rather than the list's own geometry.
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
        // x.
        nav.handle_key(MenuKey::Down);
        assert_ne!(nav.focused_row(), Some(SEARCH_FIELD), "Down left the field");
        assert_eq!(nav.search().value(), "cav", "the field kept its text");

        // Repeated Down must reach Back eventually and then stay there —
        // arrows do not wrap (`Screen.java:139-143` gates the retry on Tab).
        let mut steps = 0;
        while nav.focused_row() != Some(WorldSelectButton::Back.row()) {
            nav.handle_key(MenuKey::Down);
            steps += 1;
            assert!(steps <= WORLD_SELECT_BUTTONS.len() + 1, "Down never reached Back");
        }
        nav.handle_key(MenuKey::Down);
        assert_eq!(
            nav.focused_row(),
            Some(WorldSelectButton::Back.row()),
            "Down off the last active widget must stay put"
        );

        let mut steps = 0;
        while nav.focused_row() != Some(SEARCH_FIELD) {
            nav.handle_key(MenuKey::Up);
            steps += 1;
            assert!(steps <= WORLD_SELECT_BUTTONS.len() + 1, "Up never returned to the search field");
        }
        assert_eq!(nav.search().value(), "cav", "the field kept its text throughout");
    }

    /// Escape closes the screen, Enter on Play launches, and Enter closes only
    /// from Back.
    #[test]
    fn escape_closes_and_enter_launches_from_play_and_closes_from_back() {
        let mut nav = populated();
        assert_eq!(nav.handle_key(MenuKey::Escape), WorldSelectOutcome::Close);

        let mut nav = populated();
        // Enter with the field focused is `EditBox`'s decline plus a screen that
        // has nothing to do with it — it must *not* close, and must not launch.
        assert_eq!(nav.handle_key(MenuKey::Enter), WorldSelectOutcome::Handled);
        // Tab past the two readable list rows to reach Play.
        for _ in 0..3 {
            nav.handle_key(MenuKey::Tab);
        }
        assert_eq!(nav.focused_row(), Some(WorldSelectButton::Play.row()));
        assert_eq!(
            nav.handle_key(MenuKey::Enter),
            WorldSelectOutcome::Play("bravo".to_string()),
            "Tabbing through the list left `bravo` selected, and Play opens the \
             selection rather than a fixed world"
        );
        nav.handle_key(MenuKey::Tab);
        assert_eq!(nav.focused_row(), Some(WorldSelectButton::Create.row()));
        nav.handle_key(MenuKey::Tab);
        assert_eq!(
            nav.focused_row(),
            Some(WorldSelectButton::Delete.row()),
            "Delete is live since #540, so it is a tab stop before Back"
        );
        nav.handle_key(MenuKey::Tab);
        assert_eq!(nav.focused_row(), Some(WorldSelectButton::Back.row()));
        assert_eq!(nav.handle_key(MenuKey::Enter), WorldSelectOutcome::Close);
    }

    /// A click on Play opens a world, and it is the *only* footer button that
    /// does.
    ///
    /// The negative half matters as much as the positive one: five of the six
    /// buttons must not start a world, and `press` spells every variant out so a
    /// newly-enabled button cannot inherit Play's action by falling through a
    /// `_` arm.
    #[test]
    fn only_play_opens_a_world() {
        let mut nav = populated();
        assert!(matches!(
            nav.click_row(WorldSelectButton::Play.row()),
            WorldSelectOutcome::Play(_)
        ));

        for button in WORLD_SELECT_BUTTONS
            .iter()
            .copied()
            .filter(|b| *b != WorldSelectButton::Play)
        {
            let mut nav = populated();
            // Enabled, so the click is definitely delivered — this is the
            // control shape `a_click_on_a_disabled_button_does_nothing_at_all`
            // uses, inverted: here a *disabled* button would pass vacuously.
            nav.widgets.buttons[button.row() - FIRST_BUTTON_ROW].active = true;
            assert!(
                !matches!(nav.click_row(button.row()), WorldSelectOutcome::Play(_)),
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
        assert_eq!(nav.focused_row(), Some(WorldSelectButton::Create.row()));
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
            let mut nav = populated();
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
        let mut nav = populated();
        let play = WorldSelectButton::Play;
        assert!(matches!(
            nav.click_row(play.row()),
            WorldSelectOutcome::Play(_)
        ));
        assert_eq!(nav.focused_row(), Some(play.row()));
    }

    /// Hover is not focus. The bug this prevents is concrete: with one flag,
    /// dragging the cursor over the footer would pull the keyboard out of the
    /// search field mid-word.
    #[test]
    fn hovering_a_row_never_moves_focus() {
        let mut nav = populated();
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
        // A list row is hoverable too, including the corrupt one — vanilla sets
        // `isHovered` from geometry alone.
        for row in 0..nav.shown_len() {
            nav.hover(FIRST_WORLD_ROW + row);
            assert_eq!(nav.hovered(), Some(FIRST_WORLD_ROW + row));
            assert_eq!(nav.selected_row(), Some(0), "hover must not select either");
        }
        // A row this screen does not have is ignored rather than recorded, so a
        // stale hover cannot highlight a widget that is not there.
        let last = nav.hovered();
        nav.hover(FIRST_WORLD_ROW + nav.shown_len());
        assert_eq!(nav.hovered(), last);
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

    /// **The list scrolls**, and the fixture is large enough to
    /// prove it.
    ///
    /// The `world` species of vacuous test lives in the input data, and this is
    /// exactly where it would bite: a fixture that *fits* cannot exercise
    /// scrolling at all, and would pass against a list that still capped itself.
    /// So the row count is asserted as a **precondition** against
    /// [`super::render::world_list_visible_rows`] at the reference canvas — the
    /// same expression the draw's band uses — rather than against a literal.
    ///
    /// This replaces `the_list_is_capped_at_the_rows_the_band_can_actually_show`,
    /// whose subject was the cap. Its magnitude reasoning is kept where it still
    /// applies: 10 rows at 854x480 is `floor((480 - 60 - 51) / 36)`, and the
    /// wrong hypothesis — forgetting the footer band — is
    /// `floor((480 - 51) / 36) == 11`.
    #[test]
    fn every_world_gets_a_row_and_the_list_scrolls_to_the_ones_past_the_band() {
        const CANVAS_H: f32 = 480.0;
        let fits = super::super::render::world_list_visible_rows(CANVAS_H);
        assert_eq!(fits, 10, "floor((480 - 60 - 51) / 36)");
        assert_ne!(fits, 11, "11 is the count that ignores the 60 px footer band");

        let many: Vec<WorldSummary> = (0..25)
            .map(|i| world(&format!("w{i:02}"), &format!("World {i}"), 1_000 - i))
            .collect();
        let mut nav = WorldSelectNav::with_worlds(many);
        // The precondition, both halves: everything was enumerated, and there is
        // genuinely more than fits — without the second, nothing below scrolls.
        assert_eq!(nav.worlds().len(), 25, "premise: all 25 enumerated");
        assert_eq!(
            nav.shown_len(),
            25,
            "premise: every world is a row now — the `.take(max_visible_world_rows())` \
             cap is what made worlds past the tenth unreachable"
        );
        assert!(
            nav.shown_len() > fits,
            "premise: {} rows must not fit in a band that shows {fits}, or this test \
             cannot exercise scrolling",
            nav.shown_len()
        );

        // Every row has a widget, so focus can never land on a row with no rect.
        for row in 0..nav.shown_len() {
            assert!(
                nav.widgets.get(FIRST_WORLD_ROW + row).is_some(),
                "row {row} has no widget"
            );
        }
        assert!(nav.widgets.get(FIRST_WORLD_ROW + nav.shown_len()).is_none());

        // At rest the last row is **out of the band**, which is the state that fix
        // described, and the first is in it.
        assert_eq!(nav.scroll(), 0.0, "the list opens at the top");
        let visible = |nav: &WorldSelectNav, row: usize| {
            super::super::render::world_list_row_visible(row, CANVAS_H, nav.scroll())
        };
        assert!(visible(&nav, 0));
        assert!(
            !visible(&nav, 24),
            "premise: the last row starts off-band, or 'scrolling reached it' is \
             satisfied by it having been there all along"
        );

        // The wheel reaches it. `max_scroll` is the **predicted** value, not "more
        // than zero", and both hypotheses are computed from outside constants:
        // 25 rows of 36 is 900, `contentHeight()` adds vanilla's own `+ 4` — the
        // 2 px above the first entry and below the last
        // (`AbstractSelectionList.java:197-205`) — and the band is
        // `480 - 60 - 49` = 371. So the answer is `904 - 371` = **533**, and the
        // wrong hypothesis (forgetting `contentHeight`'s `+ 4`) is 529. The first
        // draft of this assertion predicted 529 and the measurement said 533.
        let model = super::super::render::world_scroll_model(25, CANVAS_H)
            .expect("25 rows in a 371 px band scroll");
        assert_eq!(model.max_scroll(), 904.0 - 371.0);
        assert_ne!(model.max_scroll(), 900.0 - 371.0, "529 forgets contentHeight's +4");
        // One notch is 18 px — `scrollRate = defaultEntryHeight / 2`, **not** a
        // whole 36 px row (that fix's record of getting this wrong with a `usize`).
        nav.scroll_by(-1.0, CANVAS_H);
        assert_eq!(nav.scroll(), 18.0, "one notch is 18 px, not 36");
        // Enough notches to reach the clamp, then the last row is on screen and
        // the first is not.
        for _ in 0..60 {
            nav.scroll_by(-1.0, CANVAS_H);
        }
        assert_eq!(nav.scroll(), model.max_scroll(), "the wheel clamps at the end");
        assert!(visible(&nav, 24), "the last world must be reachable");
        assert!(
            !visible(&nav, 0),
            "and the first must now be scrolled out, or the band did not move"
        );

        // -- the out-of-view control, in both directions ---------------------
        // A row scrolled out of view must not be a tab stop. It cannot be, because
        // focus scrolls itself into view: walk Tab from the top and require the
        // focused row to be visible at every step.
        let mut nav = WorldSelectNav::with_worlds(
            (0..25)
                .map(|i| world(&format!("w{i:02}"), &format!("World {i}"), 1_000 - i))
                .collect(),
        );
        assert_eq!(nav.focused_row(), Some(SEARCH_FIELD));
        let mut seen_rows = 0usize;
        for _ in 0..30 {
            nav.handle_key(MenuKey::Tab);
            if let Some(row) = nav.focused_row().and_then(|id| nav.world_row(id)) {
                seen_rows += 1;
                assert!(
                    super::super::render::world_list_row_visible(row, CANVAS_H, nav.scroll()),
                    "Tab focused world row {row} while it was scrolled out of the \
                     band (scroll {}) — a focusable row with no rect is a trap",
                    nav.scroll()
                );
            }
        }
        assert_eq!(
            seen_rows, 25,
            "premise: the walk really visited every world row, or the assertion \
             inside it measured almost nothing"
        );
        // And the walk moved the list, which is what makes the assertion above
        // about scrolling rather than about a band that happened to hold
        // everything.
        assert!(
            nav.scroll() > 0.0,
            "tabbing through 25 rows never scrolled the list"
        );
    }
}
