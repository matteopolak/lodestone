//! Vanilla's **screen input layer** — focus, Tab/arrow traversal, and event
//! dispatch to children: `GuiEventListener`, `ContainerEventHandler`,
//! `ComponentPath`, `FocusNavigationEvent` and the
//! `gui/navigation/Screen{Axis,Direction,Position,Rectangle}` geometry they
//! navigate over.
//!
//! ## What it is
//!
//! The third child of the menu-framework epic (#392/#395). [`super::widget`]
//! landed the leaf, [`super::layout`] landed the containers that place leaves;
//! this is the part that decides **which leaf gets the keystroke**. Its first
//! real consumer is [`super::edit_box::EditBox`], wired into
//! [`super::Screen::ServerEdit`] through [`super::nav::EditForm`].
//!
//! ## The two orderings that are the whole point
//!
//! ### 1. `Screen.keyPressed`: Escape, then the focused child, *then* navigation
//!
//! `Screen.java`, and the order is load-bearing:
//!
//! ```text
//! 1. event.isEscape() && shouldCloseOnEsc()  -> onClose(), return true
//! 2. super.keyPressed(event)                 -> the FOCUSED CHILD ONLY
//! 3. only if that returned false:            -> 258 Tab / 262..265 arrows
//!                                               become focus navigation
//! ```
//!
//! Step 2 is `ContainerEventHandler.keyPressed`, which is
//! `getFocused() != null && getFocused().keyPressed(event)` — it **never
//! iterates children**. So a focused text field swallows Left/Right *before*
//! they can move focus, and that falls out of the ordering rather than from a
//! special case anywhere. [`FocusSet::screen_key_pressed`] is this, transcribed.
//!
//! The corollary is the reason [`super::edit_box::EditBox`] can be dropped into
//! a screen that already navigates with the arrow keys and not fight it:
//! `EditBox.keyPressed` handles 262/263 (Left/Right) and 268/269 (Home/End) and
//! explicitly **declines** 264/265 (Down/Up) — `EditBox.java` lists them
//! in the `default:` group — so vertical arrows fall through to step 3 and move
//! focus, while horizontal ones move the caret.
//!
//! ### 2. Tab does not wrap in `handleTabNavigation` — the wrap is in `Screen`
//!
//! This one contradicts the obvious reading. `ContainerEventHandler`'s tab walk
//! runs off the end of the sorted child list and returns `null`; there is no
//! modular arithmetic anywhere in it. The wrap is a **retry** one layer up
//! (`Screen.java`):
//!
//! ```text
//! ComponentPath focusPath = super.nextFocusPath(navigationEvent);
//! if (focusPath == null && navigationEvent instanceof TabNavigation) {
//!     this.clearFocus();                            // forget where we were
//!     focusPath = super.nextFocusPath(navigationEvent);   // start from the end
//! }
//! ```
//!
//! With no focus, `handleTabNavigation` starts at index `0` when forward and at
//! `size` when backward — so the retry lands on the first (or last) focusable
//! child. Two consequences that a hand-rolled `(i + 1) % n` gets wrong:
//!
//! - **Arrow navigation does not wrap at all.** The retry is gated on
//!   `instanceof TabNavigation`. Arrow off the edge and focus simply stays.
//! - **The wrap clears focus first**, so a widget that refuses focus when it is
//!   already focused (every [`super::widget::Widget`] — `takes_focus` is
//!   `isActive() && !isFocused()`) becomes eligible again. In a one-focusable-
//!   child screen, Tab therefore re-lands on the same child rather than doing
//!   nothing.
//!
//! ## Tab order is insertion order until something says otherwise
//!
//! `handleTabNavigation` sorts `children()` by `getTabOrderGroup()`
//! (`TabOrderedElement.java`, default `0`) with `Collections.sort`, which is
//! **stable** — so an all-default screen tabs in the order widgets were added.
//! [`FocusTarget::tab_order_group`] has the same default and
//! [`FocusSet::next_focus_path`] uses `slice::sort_by_key`, which is Rust's
//! stable sort. Getting either wrong is invisible until a screen mixes groups.
//!
//! ## Arrow navigation is geometric, not ordinal
//!
//! `nextFocusPathInDirection` (`ContainerEventHandler.java`) is two
//! passes, and the second is the one that is easy to forget:
//!
//! 1. **Strict.** Keep children that *overlap the focused rect in the orthogonal
//!    axis* and lie after it along the travel axis; sort by leading edge, then by
//!    the orthogonal negative edge. Column-to-column, row-to-row.
//! 2. **Vague** (`nextFocusPathVaguelyInDirection`, `:235-269`). If nothing
//!    qualified, drop the overlap requirement entirely and take the nearest by
//!    **squared distance** between the focused rect's leading-edge centre and
//!    each candidate's trailing-edge centre.
//!
//! Ship only the strict pass and focus dies at the end of a column instead of
//! hopping to the next one.
//!
//! ## The three registries, and why they are an island factory
//!
//! `Screen` keeps three lists (`Screen.java`) and a widget's membership
//! decides what it can do:
//!
//! | added with | drawn | gets events | narrated |
//! |---|---|---|---|
//! | `addRenderableWidget` | yes | yes | yes |
//! | `addWidget` | **no** | yes | yes |
//! | `addRenderableOnly` | yes | **no** | no |
//!
//! `ContainerEventHandler`'s dispatch reads `children()`, which only
//! `addWidget` appends to. So a widget in the wrong list is unit-testable,
//! correct, registered, and **never clickable** — or invisible — with nothing
//! failing loudly. That is `CLAUDE.md`'s dominant defect class in miniature,
//! and it is why [`Registry`] is an explicit enum rather than three `Vec`
//! pushes: `registries_are_not_interchangeable` asserts a render-only widget
//! receives no click and runs the interactive registration as its control.
//!
//! ## `getChildAt` is first-match, not topmost
//!
//! `ContainerEventHandler.getChildAt` returns the **first** child in
//! `children()` whose `isMouseOver` is true (`:28-36`) — insertion order, no z
//! ordering, no reverse iteration. Two overlapping widgets means the older one
//! wins every click, forever, and vanilla simply does not overlap them.
//!
//! ## How to change it
//!
//! - **Identity is an index here, not a reference.** Vanilla's `ComponentPath`
//!   holds the components themselves and `applyFocus` walks *them*; Rust cannot
//!   hold a back-reference into the screen's own storage while mutating it. So a
//!   caller hands its children to [`FocusSet`] through [`FocusChildren`] (an
//!   `id -> &dyn FocusTarget` lookup) and every path is a path of **ids**. Two
//!   things follow: [`FocusTarget::current_focus_path`] has to be *told* its own
//!   id to build a [`ComponentPath::Leaf`], and `applyFocus`'s recursion lives on
//!   [`FocusSet`]/[`FocusTarget::apply_focus`] rather than on the path.
//! - **[`ComponentPath`] is relative to the container that returned it.** In
//!   vanilla every path returned by a container is wrapped
//!   `ComponentPath.path(this, child)`, so the screen appears at its head; here
//!   the [`FocusSet`] *is* `this`, so the head element of a returned path is
//!   already a child id. Nothing nests yet — [`ComponentPath::Path`] exists for
//!   the scroll list #396 needs, and [`FocusTarget::apply_focus`]'s default is
//!   the leaf behaviour.
//! - **[`KeyEvent`] carries GLFW's raw code, not an abstract key.** That is
//!   deliberate: `EditBox.keyPressed`'s entire body is a `switch` on it, so
//!   porting it as a match on the same integers keeps the port checkable against
//!   the jar line by line. [`super::nav::MenuKey`] is mapped onto it at the one
//!   boundary ([`KeyEvent::from_menu_key`]).
//! - **The edit-shortcut modifier is Cmd on macOS, not Ctrl.**
//!   `InputQuirks.EDIT_SHORTCUT_KEY_MODIFIER` is `8` (SUPER) on OSX and `2`
//!   (CONTROL) everywhere else, and *every* `isCut`/`isCopy`/`isPaste`/
//!   `isSelectAll` goes through it (`InputWithModifiers.java`). A port that
//!   hardcodes Ctrl gives Mac users a client where Cmd+V does nothing — and this
//!   is a Mac. [`EDIT_SHORTCUT_MODIFIER`] is the constant.
//!
//! ## Not here, on purpose
//!
//! - **Narration.** `Screen.addWidget` also appends to `narratables`, and
//!   [`Registry`] records the distinction, but nothing in this shell speaks. A
//!   `NarratableEntry` port would reach zero pixels and zero audio.
//! - **Mouse drag and scroll.** `ContainerEventHandler.mouseDragged`/
//!   `mouseScrolled` need a drag state machine and a scrolling container; both
//!   land with #396's list. [`FocusSet::mouse_clicked`] is here because it is
//!   what *sets* focus, which is the subject.
//! - **`setInitialFocus`.** It is gated on
//!   `minecraft.getLastInputType().isKeyboard()` (`Screen.java`) — a
//!   piece of state this shell does not track. [`super::nav::EditForm`] focuses
//!   its first field explicitly instead, which is what
//!   `setInitialFocus(GuiEventListener)` does.
//!
//! ## Dependencies
//!
//! [`super::widget`] for [`super::widget::Widget`]'s [`FocusTarget`] impl and
//! [`super::layout::ipx`] for the `f32` → integer-pixel boundary. Nothing else.

use super::layout::ipx;
use super::widget::Widget;

// GLFW key codes, spelled as vanilla's `switch` labels spell them so a port can
// be diffed against `EditBox.java` and `Screen.java` directly.
/// `GLFW_KEY_ESCAPE`.
pub const KEY_ESCAPE: i32 = 256;
/// `GLFW_KEY_ENTER`.
pub const KEY_ENTER: i32 = 257;
/// `GLFW_KEY_TAB`.
pub const KEY_TAB: i32 = 258;
/// `GLFW_KEY_BACKSPACE`.
pub const KEY_BACKSPACE: i32 = 259;
/// `GLFW_KEY_INSERT`.
pub const KEY_INSERT: i32 = 260;
/// `GLFW_KEY_DELETE`.
pub const KEY_DELETE: i32 = 261;
/// `GLFW_KEY_RIGHT`.
pub const KEY_RIGHT: i32 = 262;
/// `GLFW_KEY_LEFT`.
pub const KEY_LEFT: i32 = 263;
/// `GLFW_KEY_DOWN`.
pub const KEY_DOWN: i32 = 264;
/// `GLFW_KEY_UP`.
pub const KEY_UP: i32 = 265;
/// `GLFW_KEY_HOME`.
pub const KEY_HOME: i32 = 268;
/// `GLFW_KEY_END`.
pub const KEY_END: i32 = 269;
/// GLFW `GLFW_KEY_F5`, the code `JoinMultiplayerScreen.keyPressed` compares
/// against to refresh the server list (`JoinMultiplayerScreen.java`).
pub const KEY_F5: i32 = 294;
/// `GLFW_KEY_A` — Ctrl/Cmd+A is select-all.
pub const KEY_A: i32 = 65;
/// `GLFW_KEY_C` — Ctrl/Cmd+C is copy.
pub const KEY_C: i32 = 67;
/// `GLFW_KEY_V` — Ctrl/Cmd+V is paste.
pub const KEY_V: i32 = 86;
/// `GLFW_KEY_X` — Ctrl/Cmd+X is cut.
pub const KEY_X: i32 = 88;

/// `GLFW_MOD_SHIFT`.
pub const MOD_SHIFT: i32 = 1;
/// `GLFW_MOD_CONTROL`.
pub const MOD_CONTROL: i32 = 2;
/// `GLFW_MOD_ALT`.
pub const MOD_ALT: i32 = 4;
/// `GLFW_MOD_SUPER` — the Command key on macOS.
pub const MOD_SUPER: i32 = 8;

/// `InputQuirks.EDIT_SHORTCUT_KEY_MODIFIER`: the modifier bit every text-editing
/// shortcut (`isSelectAll`/`isCopy`/`isPaste`/`isCut`) tests, which vanilla
/// swaps to **Super** on macOS and leaves as **Control** elsewhere
/// (`InputQuirks.java`).
///
/// Resolved at compile time from `target_os`, which is the closest thing to
/// `Util.getPlatform()` available without a runtime probe. Hardcoding
/// [`MOD_CONTROL`] would ship a client where Cmd+V silently does nothing on the
/// platform this repo is developed on.
pub const EDIT_SHORTCUT_MODIFIER: i32 = if cfg!(target_os = "macos") {
    MOD_SUPER
} else {
    MOD_CONTROL
};

/// Vanilla's `KeyEvent` record (`client/input/KeyEvent.java`) plus the
/// `InputWithModifiers` predicates the GUI actually asks it for
/// (`InputWithModifiers.java`).
///
/// `scancode` is dropped: nothing in `Screen`, `AbstractWidget` or `EditBox`
/// reads it — only `KeyMapping` does, and key *bindings* are `keybinds.rs`'s
/// problem, not the menu's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyEvent {
    /// The GLFW key code.
    pub key: i32,
    /// The GLFW modifier bitmask.
    pub modifiers: i32,
}

impl KeyEvent {
    /// A keypress with no modifiers held.
    #[must_use]
    pub const fn new(key: i32) -> Self {
        Self { key, modifiers: 0 }
    }

    /// A keypress with an explicit modifier mask.
    #[must_use]
    pub const fn with_modifiers(key: i32, modifiers: i32) -> Self {
        Self { key, modifiers }
    }

    /// `isEscape()`.
    #[must_use]
    pub const fn is_escape(self) -> bool {
        self.key == KEY_ESCAPE
    }

    /// `isCycleFocus()` — Tab, whichever direction Shift makes it.
    #[must_use]
    pub const fn is_cycle_focus(self) -> bool {
        self.key == KEY_TAB
    }

    /// `hasShiftDown()`.
    #[must_use]
    pub const fn has_shift_down(self) -> bool {
        self.modifiers & MOD_SHIFT != 0
    }

    /// `hasAltDown()`.
    #[must_use]
    pub const fn has_alt_down(self) -> bool {
        self.modifiers & MOD_ALT != 0
    }

    /// `hasControlDownWithQuirk()`: [`EDIT_SHORTCUT_MODIFIER`], i.e. Cmd on
    /// macOS and Ctrl elsewhere. This is the one `EditBox` tests for word-wise
    /// cursor motion and whole-word delete.
    #[must_use]
    pub const fn has_control_down_with_quirk(self) -> bool {
        self.modifiers & EDIT_SHORTCUT_MODIFIER != 0
    }

    /// `isSelectAll()`: the quirked modifier and *neither* Shift nor Alt.
    #[must_use]
    pub const fn is_select_all(self) -> bool {
        self.is_edit_shortcut(KEY_A)
    }

    /// `isCopy()`.
    #[must_use]
    pub const fn is_copy(self) -> bool {
        self.is_edit_shortcut(KEY_C)
    }

    /// `isPaste()`.
    #[must_use]
    pub const fn is_paste(self) -> bool {
        self.is_edit_shortcut(KEY_V)
    }

    /// `isCut()`.
    #[must_use]
    pub const fn is_cut(self) -> bool {
        self.is_edit_shortcut(KEY_X)
    }

    const fn is_edit_shortcut(self, key: i32) -> bool {
        self.key == key
            && self.has_control_down_with_quirk()
            && !self.has_shift_down()
            && !self.has_alt_down()
    }

    /// The [`KeyEvent`] a [`super::nav::MenuKey`] stands for, or `None` for
    /// [`super::nav::MenuKey::Char`] — which is vanilla's *`charTyped`* path, a
    /// different callback entirely, and must not be smuggled through this one.
    ///
    /// **This mapping used to be partial, and the mismatch was a shipped bug**
    /// (menu inputs never receiving keyboard modifiers): the doc here used to
    /// claim "no change is needed in `app.rs` or `EditBox`" for
    /// Cmd/Ctrl+A/C/X/V, which was true of *this* function and false of the
    /// producer — `winit`'s modifier state was never tracked anywhere in this
    /// crate, so every real `KeyEvent` app.rs built carried `modifiers: 0` and
    /// Cmd+A was indistinguishable from `a`. `app::lifecycle` now tracks
    /// `ModifiersChanged` and `app::menus::menu_key_for` produces
    /// [`super::nav::MenuKey::SelectAll`]/`Copy`/`Cut`/`Paste` only when the
    /// shortcut modifier is held, which is what makes the four arms below
    /// reachable in production.
    ///
    /// Left/Right/Home/End arrive too now, through
    /// [`super::nav::MenuKey::Edit`] rather than through an abstract variant
    /// of their own — see that variant's doc for why the modifiers have to
    /// travel with the key for exactly these four and not for the others.
    #[must_use]
    pub fn from_menu_key(key: super::nav::MenuKey) -> Option<Self> {
        use super::nav::MenuKey;
        Some(match key {
            MenuKey::Up => Self::new(KEY_UP),
            MenuKey::Down => Self::new(KEY_DOWN),
            MenuKey::Enter => Self::new(KEY_ENTER),
            MenuKey::Escape => Self::new(KEY_ESCAPE),
            MenuKey::Tab => Self::new(KEY_TAB),
            MenuKey::Backspace => Self::new(KEY_BACKSPACE),
            MenuKey::Delete => Self::new(KEY_DELETE),
            // F5 is a real key event, so a focused child is offered it first
            // exactly as vanilla's `Screen.keyPressed` does — `EditBox` declines
            // 294 (it is in `keyPressed`'s `default:` group), which is what lets
            // the multiplayer screen's own `keyPressed` see it.
            MenuKey::Refresh => Self::new(KEY_F5),
            // `EDIT_SHORTCUT_MODIFIER` is Cmd on macOS and Ctrl elsewhere
            // (`InputQuirks.EDIT_SHORTCUT_KEY_MODIFIER`), and these four GLFW
            // key codes plus that one bit are exactly `isSelectAll`/`isCopy`/
            // `isCut`/`isPaste` — see [`Self::is_select_all`] and its
            // siblings, and `EditBox::handle_key`'s `_` arm for what each does.
            MenuKey::SelectAll => Self::with_modifiers(KEY_A, EDIT_SHORTCUT_MODIFIER),
            MenuKey::Copy => Self::with_modifiers(KEY_C, EDIT_SHORTCUT_MODIFIER),
            MenuKey::Cut => Self::with_modifiers(KEY_X, EDIT_SHORTCUT_MODIFIER),
            MenuKey::Paste => Self::with_modifiers(KEY_V, EDIT_SHORTCUT_MODIFIER),
            // Already a real key event — caret motion carries its modifiers
            // rather than being abstracted away, because for these keys the
            // modifiers are the meaning. See [`super::nav::MenuKey::Edit`].
            MenuKey::Edit(event) => event,
            MenuKey::Char(_) => return None,
        })
    }
}

/// `gui/navigation/ScreenAxis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenAxis {
    /// x.
    Horizontal,
    /// y.
    Vertical,
}

impl ScreenAxis {
    /// `orthogonal()`.
    #[must_use]
    pub const fn orthogonal(self) -> Self {
        match self {
            Self::Horizontal => Self::Vertical,
            Self::Vertical => Self::Horizontal,
        }
    }

    /// `getPositive()` — Right for horizontal, Down for vertical.
    #[must_use]
    pub const fn positive(self) -> ScreenDirection {
        match self {
            Self::Horizontal => ScreenDirection::Right,
            Self::Vertical => ScreenDirection::Down,
        }
    }

    /// `getNegative()` — Left for horizontal, Up for vertical.
    #[must_use]
    pub const fn negative(self) -> ScreenDirection {
        match self {
            Self::Horizontal => ScreenDirection::Left,
            Self::Vertical => ScreenDirection::Up,
        }
    }
}

/// `gui/navigation/ScreenDirection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenDirection {
    /// Toward smaller y.
    Up,
    /// Toward larger y.
    Down,
    /// Toward smaller x.
    Left,
    /// Toward larger x.
    Right,
}

impl ScreenDirection {
    /// `getAxis()`.
    #[must_use]
    pub const fn axis(self) -> ScreenAxis {
        match self {
            Self::Up | Self::Down => ScreenAxis::Vertical,
            Self::Left | Self::Right => ScreenAxis::Horizontal,
        }
    }

    /// `getOpposite()`.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Up => Self::Down,
            Self::Down => Self::Up,
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    /// `isPositive()` — Down and Right travel toward larger coordinates.
    #[must_use]
    pub const fn is_positive(self) -> bool {
        matches!(self, Self::Down | Self::Right)
    }

    /// `isAfter(a, b)`: is `a` further along this direction than `b`?
    #[must_use]
    pub const fn is_after(self, a: i32, b: i32) -> bool {
        if self.is_positive() { a > b } else { b > a }
    }

    /// `isBefore(a, b)`.
    #[must_use]
    pub const fn is_before(self, a: i32, b: i32) -> bool {
        if self.is_positive() { a < b } else { b < a }
    }

    /// `coordinateValueComparator()`: orders coordinates so "earlier along this
    /// direction" sorts first, which for Up/Left means *descending*.
    #[must_use]
    pub fn compare_coordinates(self, a: i32, b: i32) -> core::cmp::Ordering {
        if a == b {
            core::cmp::Ordering::Equal
        } else if self.is_before(a, b) {
            core::cmp::Ordering::Less
        } else {
            core::cmp::Ordering::Greater
        }
    }

    /// The [`FocusNavigationEvent`] Screen builds for the arrow key with this
    /// GLFW code, or `None` for any other key
    /// (`Screen.java`).
    #[must_use]
    pub const fn from_key(key: i32) -> Option<Self> {
        match key {
            KEY_RIGHT => Some(Self::Right),
            KEY_LEFT => Some(Self::Left),
            KEY_DOWN => Some(Self::Down),
            KEY_UP => Some(Self::Up),
            _ => None,
        }
    }
}

/// `gui/navigation/ScreenPosition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScreenPosition {
    /// x.
    pub x: i32,
    /// y.
    pub y: i32,
}

impl ScreenPosition {
    /// `of(axis, primary, secondary)`.
    #[must_use]
    pub const fn of(axis: ScreenAxis, primary: i32, secondary: i32) -> Self {
        match axis {
            ScreenAxis::Horizontal => Self { x: primary, y: secondary },
            ScreenAxis::Vertical => Self { x: secondary, y: primary },
        }
    }

    /// `step(direction)`: one pixel along `direction`.
    #[must_use]
    pub const fn step(self, direction: ScreenDirection) -> Self {
        match direction {
            ScreenDirection::Down => Self { x: self.x, y: self.y + 1 },
            ScreenDirection::Up => Self { x: self.x, y: self.y - 1 },
            ScreenDirection::Left => Self { x: self.x - 1, y: self.y },
            ScreenDirection::Right => Self { x: self.x + 1, y: self.y },
        }
    }

    /// `getCoordinate(axis)`.
    #[must_use]
    pub const fn coordinate(self, axis: ScreenAxis) -> i32 {
        match axis {
            ScreenAxis::Horizontal => self.x,
            ScreenAxis::Vertical => self.y,
        }
    }

    /// Squared distance to `other`, as `Vector2i.distanceSquared` — the
    /// tiebreak in `nextFocusPathVaguelyInDirection`. `i64` because vanilla's is
    /// a `long`: two 32-bit coordinate deltas squared overflow an `i32`.
    #[must_use]
    pub const fn distance_squared(self, other: Self) -> i64 {
        let dx = (self.x - other.x) as i64;
        let dy = (self.y - other.y) as i64;
        dx * dx + dy * dy
    }
}

/// `gui/navigation/ScreenRectangle` — integer pixels, as vanilla's whole
/// navigation layer is.
///
/// Note [`Self::bound_in_direction`]'s `- 1` on the positive side: the bound is
/// the **last pixel inside**, not the exclusive edge. Every comparison in
/// `nextFocusPathInDirection` is against that inclusive value, and a port using
/// `x + width` there quietly makes touching rectangles overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScreenRectangle {
    /// Top-left corner.
    pub position: ScreenPosition,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

impl ScreenRectangle {
    /// `new(x, y, width, height)`.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            position: ScreenPosition { x, y },
            width,
            height,
        }
    }

    /// `empty()`, which is also `GuiEventListener.getRectangle()`'s default —
    /// and therefore what a widget that forgets to report its bounds navigates
    /// as.
    #[must_use]
    pub const fn empty() -> Self {
        Self::new(0, 0, 0, 0)
    }

    /// A widget's `f32` rect in integer pixels, through [`ipx`].
    #[must_use]
    pub fn from_rect(rect: (f32, f32, f32, f32)) -> Self {
        Self::new(ipx(rect.0), ipx(rect.1), ipx(rect.2), ipx(rect.3))
    }

    /// `of(primaryAxis, primaryIndex, secondaryIndex, primaryLength, secondaryLength)`.
    #[must_use]
    pub const fn of(
        primary_axis: ScreenAxis,
        primary_index: i32,
        secondary_index: i32,
        primary_length: i32,
        secondary_length: i32,
    ) -> Self {
        match primary_axis {
            ScreenAxis::Horizontal => {
                Self::new(primary_index, secondary_index, primary_length, secondary_length)
            }
            ScreenAxis::Vertical => {
                Self::new(secondary_index, primary_index, secondary_length, primary_length)
            }
        }
    }

    /// `step(direction)`.
    #[must_use]
    pub const fn step(self, direction: ScreenDirection) -> Self {
        Self {
            position: self.position.step(direction),
            width: self.width,
            height: self.height,
        }
    }

    /// `getLength(axis)`.
    #[must_use]
    pub const fn length(self, axis: ScreenAxis) -> i32 {
        match axis {
            ScreenAxis::Horizontal => self.width,
            ScreenAxis::Vertical => self.height,
        }
    }

    /// `getBoundInDirection(direction)`: the **inclusive** far edge along
    /// `direction`.
    #[must_use]
    pub const fn bound_in_direction(self, direction: ScreenDirection) -> i32 {
        let axis = direction.axis();
        if direction.is_positive() {
            self.position.coordinate(axis) + self.length(axis) - 1
        } else {
            self.position.coordinate(axis)
        }
    }

    /// `getBorder(direction)`: the 1 px sliver just *outside* this rect's
    /// `direction` edge — what arrow navigation from an unfocused screen starts
    /// from.
    #[must_use]
    pub const fn border(self, direction: ScreenDirection) -> Self {
        let start_first = self.bound_in_direction(direction);
        let orthogonal = direction.axis().orthogonal();
        let start_second = self.bound_in_direction(orthogonal.negative());
        let length = self.length(orthogonal);
        Self::of(direction.axis(), start_first, start_second, 1, length).step(direction)
    }

    /// `overlapsInAxis(other, axis)`, on inclusive bounds.
    #[must_use]
    pub const fn overlaps_in_axis(self, other: Self, axis: ScreenAxis) -> bool {
        let this_lower = self.bound_in_direction(axis.negative());
        let other_lower = other.bound_in_direction(axis.negative());
        let this_higher = self.bound_in_direction(axis.positive());
        let other_higher = other.bound_in_direction(axis.positive());
        let lower = if this_lower > other_lower { this_lower } else { other_lower };
        let higher = if this_higher < other_higher { this_higher } else { other_higher };
        lower <= higher
    }

    /// `getCenterInAxis(axis)`, on inclusive bounds and truncating like Java's
    /// `int` division.
    #[must_use]
    pub const fn center_in_axis(self, axis: ScreenAxis) -> i32 {
        (self.bound_in_direction(axis.positive()) + self.bound_in_direction(axis.negative())) / 2
    }
}

/// `gui/navigation/FocusNavigationEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusNavigationEvent {
    /// `TabNavigation(forward)`.
    Tab {
        /// Shift+Tab is backward.
        forward: bool,
    },
    /// `ArrowNavigation(direction, previousFocus)`.
    Arrow {
        /// Which way the arrow points.
        direction: ScreenDirection,
        /// The rect focus is travelling *from*, threaded down so a nested
        /// container can navigate as if focus were outside it
        /// (`ArrowNavigation.with`). `None` at the top of a fresh arrow press.
        previous_focus: Option<ScreenRectangle>,
    },
    /// `InitialFocus` — what `setInitialFocus(target)` sends a widget to ask
    /// whether it wants focus at all.
    Initial,
}

impl FocusNavigationEvent {
    /// A fresh arrow press.
    #[must_use]
    pub const fn arrow(direction: ScreenDirection) -> Self {
        Self::Arrow {
            direction,
            previous_focus: None,
        }
    }

}

/// `gui/ComponentPath`, over child **ids** rather than component references —
/// see the module docs on why.
///
/// The path is relative to the container that produced it: its head is a child
/// id in *that* container's id space, and a [`Self::Path`] descends into a child
/// that is itself a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentPath {
    /// `ComponentPath.Leaf` — this child takes the focus.
    Leaf(usize),
    /// `ComponentPath.Path` — this child is a container, and focus lands
    /// somewhere inside it.
    Path(usize, Box<ComponentPath>),
}

impl ComponentPath {
    /// `ComponentPath.leaf(component)`.
    #[must_use]
    pub const fn leaf(id: usize) -> Self {
        Self::Leaf(id)
    }

    /// `ComponentPath.path(container, childPath)`: `null` in, `null` out — which
    /// is the whole reason the static exists, and why every `nextFocusPath`
    /// caller can wrap unconditionally.
    #[must_use]
    pub fn path(id: usize, child: Option<Self>) -> Option<Self> {
        child.map(|c| Self::Path(id, Box::new(c)))
    }

    /// The child id at this level — what a container's `setFocused` takes.
    #[must_use]
    pub const fn head(&self) -> usize {
        match self {
            Self::Leaf(id) | Self::Path(id, _) => *id,
        }
    }

    /// `leafComponent()`: the id at the bottom of the path, in the innermost
    /// container's own id space.
    #[must_use]
    pub fn leaf_id(&self) -> usize {
        match self {
            Self::Leaf(id) => *id,
            Self::Path(_, child) => child.leaf_id(),
        }
    }

    /// How many containers this path descends through. `0` for a leaf.
    #[must_use]
    pub fn depth(&self) -> usize {
        match self {
            Self::Leaf(_) => 0,
            Self::Path(_, child) => 1 + child.depth(),
        }
    }
}

/// Which of `Screen`'s three child lists a widget was added to
/// (`Screen.java`).
///
/// An enum rather than three parallel `Vec`s because the failure mode of getting
/// it wrong is silent — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registry {
    /// `addRenderableWidget`: drawn, receives events, narrated.
    RenderableWidget,
    /// `addWidget`: receives events and narrated, **not drawn**.
    Widget,
    /// `addRenderableOnly`: drawn, **inert** — no events, no narration.
    RenderableOnly,
}

impl Registry {
    /// Whether `Screen.renderables` holds this widget, i.e. whether it is drawn.
    #[must_use]
    pub const fn is_renderable(self) -> bool {
        matches!(self, Self::RenderableWidget | Self::RenderableOnly)
    }

    /// Whether `Screen.children()` holds this widget, i.e. whether it can be
    /// clicked or focused at all. `addRenderableOnly` does **not** append here,
    /// which is the island.
    #[must_use]
    pub const fn is_child(self) -> bool {
        matches!(self, Self::RenderableWidget | Self::Widget)
    }
}

/// `GuiEventListener` + `TabOrderedElement`: what [`FocusSet`] needs of a child.
///
/// [`Self::takes_focus`] is `AbstractWidget.nextFocusPath`'s predicate rather
/// than the method itself, because a leaf cannot build a
/// [`ComponentPath::Leaf`] without knowing its own id — the container does that.
pub trait FocusTarget: core::fmt::Debug {
    /// `getRectangle()`.
    fn rectangle(&self) -> ScreenRectangle;

    /// `isActive()` — `visible && active`.
    fn is_active(&self) -> bool;

    /// `isFocused()`.
    fn is_focused(&self) -> bool;

    /// `setFocused(boolean)`.
    fn set_focused(&mut self, focused: bool);

    /// The predicate inside `AbstractWidget.nextFocusPath`
    /// (`AbstractWidget.java`): a leaf offers itself only when it is
    /// active and not *already* focused.
    fn takes_focus(&self) -> bool {
        self.is_active() && !self.is_focused()
    }

    /// `getTabOrderGroup()` — `0` for everything vanilla ships except the
    /// widgets that deliberately jump the queue.
    fn tab_order_group(&self) -> i32 {
        0
    }

    /// `shouldTakeFocusAfterInteraction()` (`GuiEventListener.java`):
    /// `true` by default, and `false` for a widget that wants a click to *do*
    /// something without keeping the keyboard.
    fn should_take_focus_after_interaction(&self) -> bool {
        true
    }

    /// `isMouseOver(x, y)` — for `AbstractWidget` this is
    /// `isActive() && areCoordinatesInRectangle(..)`, so a disabled widget is
    /// not merely unclickable but *invisible* to `getChildAt`.
    fn is_mouse_over(&self, x: f32, y: f32) -> bool;

    /// `mouseClicked` on the child: did it consume the click?
    /// `AbstractWidget.mouseClicked` returns `false` for an inactive widget or a
    /// click outside its bounds (`AbstractWidget.java`).
    fn mouse_clicked(&mut self, x: f32, y: f32) -> bool {
        self.is_mouse_over(x, y)
    }

    /// `keyPressed(KeyEvent)`. `false` by default, which is what makes an
    /// ordinary button transparent to Tab and the arrows.
    fn key_pressed(&mut self, event: KeyEvent) -> bool {
        let _ = event;
        false
    }

    /// `charTyped(CharacterEvent)`.
    fn char_typed(&mut self, ch: char) -> bool {
        let _ = ch;
        false
    }

    /// `getBorderForArrowNavigation(opposite)`.
    fn border_for_arrow_navigation(&self, opposite: ScreenDirection) -> ScreenRectangle {
        self.rectangle().border(opposite)
    }

    /// `getCurrentFocusPath()`, told its own `id` because a leaf cannot name
    /// itself here. A container child overrides to descend.
    fn current_focus_path(&self, id: usize) -> ComponentPath {
        ComponentPath::Leaf(id)
    }

    /// `ComponentPath.Path.applyFocus`'s recursion into a container child. The
    /// default is `Leaf.applyFocus`, which is correct for every leaf and is why
    /// a screen with no nested container never calls the other arm.
    fn apply_focus(&mut self, path: &ComponentPath, focused: bool) {
        let _ = path;
        self.set_focused(focused);
    }
}

impl FocusTarget for Widget {
    fn rectangle(&self) -> ScreenRectangle {
        ScreenRectangle::from_rect(self.rect())
    }

    fn is_active(&self) -> bool {
        Widget::is_active(self)
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn takes_focus(&self) -> bool {
        Widget::takes_focus(self)
    }

    fn is_mouse_over(&self, x: f32, y: f32) -> bool {
        Widget::is_mouse_over(self, x, y)
    }
}

/// The caller's child storage, addressed by id.
///
/// This is the seam that replaces vanilla's object identity: [`FocusSet`] holds
/// only ids and registry membership, so a screen keeps its widgets in whatever
/// shape suits it (named fields, a `Vec`, a mix of types) and implements this to
/// hand them over. Ids need not be contiguous and need not be indices —
/// `get`/`get_mut` returning `None` for an unknown id is fine and is treated as
/// "this child no longer exists", the same as `Screen.removeWidget`.
pub trait FocusChildren {
    /// The child with this id, or `None`.
    fn get(&self, id: usize) -> Option<&dyn FocusTarget>;
    /// The child with this id, mutably.
    fn get_mut(&mut self, id: usize) -> Option<&mut dyn FocusTarget>;
}

/// What one [`FocusSet::screen_key_pressed`] did, which is more than a `bool`
/// because vanilla's own return value throws information away.
///
/// `Screen.keyPressed` returns `true` only for the Escape branch and `false`
/// for *everything else* — including a keystroke a focused child consumed and
/// including a navigation event that moved focus (`Screen.java`;
/// the final `return false` is after the navigation block). That is fine in
/// vanilla, where the caller only asks "should this fall through to a
/// `KeyMapping`", and useless to a caller that has to decide whether the screen
/// still needs to interpret the key itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    /// The Escape branch fired: `shouldCloseOnEsc()` was true and the screen
    /// should close. Vanilla's only `return true`.
    Close,
    /// The focused child consumed it (`ContainerEventHandler.keyPressed`).
    Consumed,
    /// It was Tab or an arrow and focus moved.
    FocusMoved,
    /// Nobody wanted it. The screen may now apply its own meaning — this is
    /// where Enter-to-save and the shell's own row commands belong.
    Declined,
}

/// `Screen`'s focus bookkeeping and `ContainerEventHandler`'s dispatch, over the
/// ids of children the caller owns.
///
/// Holds the three registries in their *own* insertion orders, because
/// `addRenderableWidget` appends to both `renderables` and `children` while the
/// other two append to one each — so a screen that interleaves them has two
/// different orders, and Tab follows `children`'s while drawing follows
/// `renderables`'.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusSet {
    /// `Screen.children` order — dispatch and tab traversal.
    children: Vec<usize>,
    /// `Screen.renderables` order — draw order.
    renderables: Vec<usize>,
    /// `Screen.narratables` order. Recorded for fidelity; nothing reads it yet
    /// (see the module docs).
    narratables: Vec<usize>,
    /// `AbstractContainerEventHandler.focused`.
    focused: Option<usize>,
    /// `Screen.shouldCloseOnEsc()`. `true` in vanilla's base class
    /// (`Screen.java`); `DeathScreen` is the notable `false`.
    close_on_esc: bool,
}

impl Default for FocusSet {
    /// **Not** `derive`d, for the same reason [`Widget`]'s is not: a derived
    /// `Default` would give `close_on_esc = false`, i.e. every screen built from
    /// `..Default::default()` would silently swallow Escape and trap the player.
    /// Vanilla's `Screen.shouldCloseOnEsc()` returns `true`
    /// (`Screen.java`).
    fn default() -> Self {
        Self::new()
    }
}

impl FocusSet {
    /// An empty screen that closes on Escape, as vanilla's base `Screen` does.
    #[must_use]
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            renderables: Vec::new(),
            narratables: Vec::new(),
            focused: None,
            close_on_esc: true,
        }
    }

    /// `shouldCloseOnEsc() == false` — the death-screen shape, where Escape must
    /// be swallowed entirely rather than routed anywhere.
    #[must_use]
    pub fn without_close_on_esc(mut self) -> Self {
        self.close_on_esc = false;
        self
    }

    /// `Screen.addRenderableWidget`: drawn, interactive, narrated.
    pub fn add_renderable_widget(&mut self, id: usize) {
        self.renderables.push(id);
        self.add_widget(id);
    }

    /// `Screen.addWidget`: interactive and narrated, **not drawn**.
    pub fn add_widget(&mut self, id: usize) {
        self.children.push(id);
        self.narratables.push(id);
    }

    /// `Screen.addRenderableOnly`: drawn and **inert**. It never enters
    /// `children()`, so it cannot be clicked or focused — deliberate for a
    /// decoration, and a silent island for anything else.
    pub fn add_renderable_only(&mut self, id: usize) {
        self.renderables.push(id);
    }

    /// Which registry `id` is in, or `None` if it was never added. A widget in
    /// two registries reports the *most capable* one, which is what
    /// `addRenderableWidget` produces.
    #[must_use]
    pub fn registry(&self, id: usize) -> Option<Registry> {
        let drawn = self.renderables.contains(&id);
        let child = self.children.contains(&id);
        match (drawn, child) {
            (true, true) => Some(Registry::RenderableWidget),
            (false, true) => Some(Registry::Widget),
            (true, false) => Some(Registry::RenderableOnly),
            (false, false) => None,
        }
    }

    /// `Screen.children()` — dispatch and tab order.
    #[must_use]
    pub fn children(&self) -> &[usize] {
        &self.children
    }

    /// `Screen.renderables` — draw order.
    #[must_use]
    pub fn renderables(&self) -> &[usize] {
        &self.renderables
    }

    /// `Screen.narratables`.
    #[must_use]
    pub fn narratables(&self) -> &[usize] {
        &self.narratables
    }

    /// `getFocused()`.
    #[must_use]
    pub fn focused(&self) -> Option<usize> {
        self.focused
    }

    /// `AbstractContainerEventHandler.setFocused(child)`: unfocus the outgoing
    /// child, focus the incoming one, and do neither when nothing changed
    /// (`AbstractContainerEventHandler.java`).
    ///
    /// The no-op-when-equal guard is not an optimisation:
    /// `EditBox.setFocused(true)` resets the caret blink phase
    /// (`EditBox.java`), so re-setting the same focus would restart it.
    pub fn set_focused(&mut self, kids: &mut dyn FocusChildren, next: Option<usize>) {
        if self.focused == next {
            return;
        }
        if let Some(old) = self.focused {
            if let Some(child) = kids.get_mut(old) {
                child.set_focused(false);
            }
        }
        if let Some(new) = next {
            if let Some(child) = kids.get_mut(new) {
                child.set_focused(true);
            }
        }
        self.focused = next;
    }

    /// `getCurrentFocusPath()`, minus the implicit screen at its head.
    #[must_use]
    pub fn current_focus_path(&self, kids: &dyn FocusChildren) -> Option<ComponentPath> {
        let id = self.focused?;
        let child = kids.get(id)?;
        child.is_focused().then(|| child.current_focus_path(id))
    }

    /// `ComponentPath.applyFocus(focused)` for a path this set produced.
    pub fn apply_focus(
        &mut self,
        kids: &mut dyn FocusChildren,
        path: &ComponentPath,
        focused: bool,
    ) {
        let head = path.head();
        // `Path.applyFocus`: the container points at (or forgets) the child…
        self.set_focused(kids, if focused { Some(head) } else { None });
        // …and then the path recurses. For a `Leaf` that recursion *is*
        // `component.setFocused(focused)`, which `set_focused` above has
        // already done for the `true` case but not for `false` when the head
        // was not the currently focused child.
        if let Some(child) = kids.get_mut(head) {
            match path {
                ComponentPath::Leaf(_) => child.set_focused(focused),
                ComponentPath::Path(_, inner) => child.apply_focus(inner, focused),
            }
        }
    }

    /// `Screen.clearFocus()`.
    pub fn clear_focus(&mut self, kids: &mut dyn FocusChildren) {
        if let Some(path) = self.current_focus_path(&*kids) {
            self.apply_focus(kids, &path, false);
        } else {
            // `getCurrentFocusPath()` is null when the child disagrees about
            // being focused; the container's own pointer still has to go.
            self.set_focused(kids, None);
        }
    }

    /// `Screen.changeFocus(path)`: clear, then apply
    /// (`Screen.java`). The clear is why Tab's wrap works — see the
    /// module docs.
    pub fn change_focus(&mut self, kids: &mut dyn FocusChildren, path: &ComponentPath) {
        self.clear_focus(kids);
        self.apply_focus(kids, path, true);
    }

    /// `Screen.setInitialFocus(target)` (`Screen.java`): offer `id` an
    /// `InitialFocus` event and take the focus there if it accepts.
    pub fn set_initial_focus(&mut self, kids: &mut dyn FocusChildren, id: usize) {
        let accepts = kids.get(id).is_some_and(|c| c.takes_focus());
        if accepts {
            let path = ComponentPath::Leaf(id);
            self.change_focus(kids, &path);
        }
    }

    /// `ContainerEventHandler.getChildAt(x, y)`: the **first** child in
    /// `children()` order whose `isMouseOver` is true. Not the topmost — see the
    /// module docs.
    #[must_use]
    pub fn child_at(&self, kids: &dyn FocusChildren, x: f32, y: f32) -> Option<usize> {
        self.children
            .iter()
            .copied()
            .find(|&id| kids.get(id).is_some_and(|c| c.is_mouse_over(x, y)))
    }

    /// `ContainerEventHandler.mouseClicked` (`:38-55`), minus the drag state.
    ///
    /// Returns whether a child was *hit* — which is vanilla's return value, and
    /// is **not** the same as "focus moved": a child that consumes the click but
    /// reports `shouldTakeFocusAfterInteraction() == false` is clicked without
    /// being focused, and one that declines the click is hit without either.
    pub fn mouse_clicked(&mut self, kids: &mut dyn FocusChildren, x: f32, y: f32) -> bool {
        let Some(id) = self.child_at(&*kids, x, y) else {
            return false;
        };
        let (consumed, takes_focus) = match kids.get_mut(id) {
            Some(child) => (
                child.mouse_clicked(x, y),
                child.should_take_focus_after_interaction(),
            ),
            None => (false, false),
        };
        if consumed && takes_focus {
            self.set_focused(kids, Some(id));
        }
        true
    }

    /// `ContainerEventHandler.keyPressed` (`:80-83`): the focused child, and
    /// **only** the focused child. Never iterates.
    pub fn key_pressed(&mut self, kids: &mut dyn FocusChildren, event: KeyEvent) -> bool {
        let Some(id) = self.focused else {
            return false;
        };
        kids.get_mut(id)
            .is_some_and(|child| child.key_pressed(event))
    }

    /// `ContainerEventHandler.charTyped` (`:90-93`).
    pub fn char_typed(&mut self, kids: &mut dyn FocusChildren, ch: char) -> bool {
        let Some(id) = self.focused else {
            return false;
        };
        kids.get_mut(id).is_some_and(|child| child.char_typed(ch))
    }

    /// `Screen.keyPressed` (`Screen.java`) in full, with the ordering
    /// intact: Escape, then the focused child, then — only if it declined — Tab
    /// and the arrows as focus navigation.
    ///
    /// Returns a [`KeyOutcome`] rather than vanilla's `boolean`, because vanilla
    /// reports `false` both for "focus moved" and for "nobody wanted this" and
    /// the caller here has to tell them apart.
    pub fn screen_key_pressed(
        &mut self,
        kids: &mut dyn FocusChildren,
        event: KeyEvent,
    ) -> KeyOutcome {
        if event.is_escape() && self.close_on_esc {
            return KeyOutcome::Close;
        }
        if self.key_pressed(kids, event) {
            return KeyOutcome::Consumed;
        }
        let navigation = if event.is_cycle_focus() {
            Some(FocusNavigationEvent::Tab {
                forward: !event.has_shift_down(),
            })
        } else {
            ScreenDirection::from_key(event.key).map(FocusNavigationEvent::arrow)
        };
        let Some(navigation) = navigation else {
            return KeyOutcome::Declined;
        };
        let mut path = self.next_focus_path(&*kids, navigation);
        // The wrap, and the only place it lives: retry from scratch, for Tab
        // only, after forgetting where focus was.
        if path.is_none() && matches!(navigation, FocusNavigationEvent::Tab { .. }) {
            self.clear_focus(kids);
            path = self.next_focus_path(&*kids, navigation);
        }
        match path {
            Some(path) => {
                self.change_focus(kids, &path);
                KeyOutcome::FocusMoved
            }
            None => KeyOutcome::Declined,
        }
    }

    /// `ContainerEventHandler.nextFocusPath` (`:126-140`): ask the focused child
    /// first — which is how a nested container keeps focus inside itself — then
    /// fall back to this container's own traversal.
    #[must_use]
    pub fn next_focus_path(
        &self,
        kids: &dyn FocusChildren,
        event: FocusNavigationEvent,
    ) -> Option<ComponentPath> {
        if let Some(id) = self.focused {
            if let Some(path) = self.child_focus_path(kids, id, event) {
                // Vanilla wraps in `ComponentPath.path(this, ..)`; `this` is
                // implicit here (module docs), so the child's own path is
                // already relative to this container.
                return Some(path);
            }
        }
        match event {
            FocusNavigationEvent::Tab { forward } => self.tab_navigation(kids, forward),
            FocusNavigationEvent::Arrow { .. } => self.arrow_navigation(kids, event),
            FocusNavigationEvent::Initial => None,
        }
    }

    /// One child's `nextFocusPath`. A leaf's is
    /// `isActive() && !isFocused() ? leaf(this) : null`
    /// (`AbstractWidget.java`), which is [`FocusTarget::takes_focus`].
    fn child_focus_path(
        &self,
        kids: &dyn FocusChildren,
        id: usize,
        _event: FocusNavigationEvent,
    ) -> Option<ComponentPath> {
        let child = kids.get(id)?;
        child.takes_focus().then(|| child.current_focus_path(id))
    }

    /// `ContainerEventHandler.handleTabNavigation` (`:142-169`).
    ///
    /// Two details a rewrite loses: the sort is **stable** so equal
    /// `tabOrderGroup`s keep insertion order, and `newIndex` is
    /// `index + (forward ? 1 : 0)` — a *backward* walk starts at the focused
    /// index and steps `previous()`, so it reaches `index - 1` first. There is
    /// no wrap here.
    fn tab_navigation(&self, kids: &dyn FocusChildren, forward: bool) -> Option<ComponentPath> {
        let mut sorted: Vec<usize> = self.children.clone();
        // `slice::sort_by_key` is stable, like `Collections.sort`.
        sorted.sort_by_key(|&id| kids.get(id).map_or(0, |c| c.tab_order_group()));
        let index = self
            .focused
            .and_then(|f| sorted.iter().position(|&id| id == f));
        let new_index = match index {
            Some(i) => i + usize::from(forward),
            None if forward => 0,
            None => sorted.len(),
        };
        if forward {
            sorted[new_index.min(sorted.len())..]
                .iter()
                .find_map(|&id| self.child_focus_path(kids, id, FocusNavigationEvent::Tab { forward }))
        } else {
            sorted[..new_index.min(sorted.len())]
                .iter()
                .rev()
                .find_map(|&id| self.child_focus_path(kids, id, FocusNavigationEvent::Tab { forward }))
        }
    }

    /// `ContainerEventHandler.handleArrowNavigation` (`:171-184`).
    fn arrow_navigation(
        &self,
        kids: &dyn FocusChildren,
        event: FocusNavigationEvent,
    ) -> Option<ComponentPath> {
        let FocusNavigationEvent::Arrow {
            direction,
            previous_focus,
        } = event
        else {
            return None;
        };
        match self.focused.and_then(|id| kids.get(id).map(|c| (id, c))) {
            Some((id, child)) => {
                let from = child.border_for_arrow_navigation(direction);
                self.focus_path_in_direction(
                    kids,
                    from,
                    direction,
                    Some(id),
                    FocusNavigationEvent::Arrow {
                        direction,
                        previous_focus: Some(from),
                    },
                )
            }
            None => {
                // No focus: start from whatever rect the event carries, or from
                // the 1 px border on the *far* side of this container, so the
                // first child along `direction` is picked up.
                let from = previous_focus
                    .unwrap_or_else(|| self.container_rect(kids).border(direction.opposite()));
                self.focus_path_in_direction(kids, from, direction, None, event)
            }
        }
    }

    /// The union of this container's children, standing in for the screen's own
    /// rect. Vanilla asks `getBorderForArrowNavigation` on the *screen*, whose
    /// `getRectangle()` is the whole viewport; this shell's screens do not carry
    /// one, and the union is the smallest thing that makes
    /// `border(direction.opposite())` land outside every child, which is all the
    /// unfocused arrow case needs.
    fn container_rect(&self, kids: &dyn FocusChildren) -> ScreenRectangle {
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        for &id in &self.children {
            let Some(child) = kids.get(id) else { continue };
            let r = child.rectangle();
            let (l, t, rr, b) = (
                r.position.x,
                r.position.y,
                r.position.x + r.width,
                r.position.y + r.height,
            );
            bounds = Some(match bounds {
                None => (l, t, rr, b),
                Some((cl, ct, cr, cb)) => (cl.min(l), ct.min(t), cr.max(rr), cb.max(b)),
            });
        }
        match bounds {
            Some((l, t, r, b)) => ScreenRectangle::new(l, t, r - l, b - t),
            None => ScreenRectangle::empty(),
        }
    }

    /// `ContainerEventHandler.nextFocusPathInDirection` (`:186-233`) — the
    /// strict pass, falling through to [`Self::focus_path_vaguely_in_direction`].
    fn focus_path_in_direction(
        &self,
        kids: &dyn FocusChildren,
        from: ScreenRectangle,
        direction: ScreenDirection,
        excluded: Option<usize>,
        event: FocusNavigationEvent,
    ) -> Option<ComponentPath> {
        let axis = direction.axis();
        let other_axis = axis.orthogonal();
        let positive_other = other_axis.positive();
        let from_first_bound = from.bound_in_direction(direction.opposite());

        let mut candidates: Vec<usize> = Vec::new();
        for &id in &self.children {
            if Some(id) == excluded {
                continue;
            }
            let Some(child) = kids.get(id) else { continue };
            let rect = child.rectangle();
            if !rect.overlaps_in_axis(from, other_axis) {
                continue;
            }
            let child_first_bound = rect.bound_in_direction(direction.opposite());
            let strictly_after = direction.is_after(child_first_bound, from_first_bound);
            // The equal-leading-edge tiebreak: two widgets starting on the same
            // row still order by their *trailing* edge, so a short one nested
            // inside a tall one is reachable.
            let same_edge_but_longer = child_first_bound == from_first_bound
                && direction.is_after(
                    rect.bound_in_direction(direction),
                    from.bound_in_direction(direction),
                );
            if strictly_after || same_edge_but_longer {
                candidates.push(id);
            }
        }

        let key = |id: usize| -> (i32, i32) {
            let rect = kids
                .get(id)
                .map_or_else(ScreenRectangle::empty, |c| c.rectangle());
            (
                rect.bound_in_direction(direction.opposite()),
                rect.bound_in_direction(positive_other.opposite()),
            )
        };
        candidates.sort_by(|&a, &b| {
            let (a1, a2) = key(a);
            let (b1, b2) = key(b);
            direction
                .compare_coordinates(a1, b1)
                .then_with(|| positive_other.compare_coordinates(a2, b2))
        });

        candidates
            .iter()
            .find_map(|&id| self.child_focus_path(kids, id, event))
            .or_else(|| self.focus_path_vaguely_in_direction(kids, from, direction, excluded, event))
    }

    /// `ContainerEventHandler.nextFocusPathVaguelyInDirection` (`:235-269`): the
    /// overlap requirement dropped, nearest by squared distance between the
    /// focused rect's leading-edge centre and each candidate's trailing-edge
    /// centre.
    ///
    /// **Do not skip this pass.** Without it focus stops dead at the end of a
    /// column instead of hopping to the next one, which reads as "Tab works,
    /// arrows are broken" rather than as a missing fallback.
    fn focus_path_vaguely_in_direction(
        &self,
        kids: &dyn FocusChildren,
        from: ScreenRectangle,
        direction: ScreenDirection,
        excluded: Option<usize>,
        event: FocusNavigationEvent,
    ) -> Option<ComponentPath> {
        let axis = direction.axis();
        let other_axis = axis.orthogonal();
        let from_centre = ScreenPosition::of(
            axis,
            from.bound_in_direction(direction),
            from.center_in_axis(other_axis),
        );

        let mut candidates: Vec<(usize, i64)> = Vec::new();
        for &id in &self.children {
            if Some(id) == excluded {
                continue;
            }
            let Some(child) = kids.get(id) else { continue };
            let rect = child.rectangle();
            let child_centre = ScreenPosition::of(
                axis,
                rect.bound_in_direction(direction.opposite()),
                rect.center_in_axis(other_axis),
            );
            if direction.is_after(
                child_centre.coordinate(axis),
                from_centre.coordinate(axis),
            ) {
                candidates.push((id, from_centre.distance_squared(child_centre)));
            }
        }
        candidates.sort_by_key(|&(_, d)| d);
        candidates
            .iter()
            .find_map(|&(id, _)| self.child_focus_path(kids, id, event))
    }
}

/// `MouseHandler.DOUBLE_CLICK_THRESHOLD_MS` (`MouseHandler.java`): the exact
/// wire vanilla checks with `currentTime - lastClick.time() < 250L` —
/// **strict** less-than, so a click exactly 250 ms after the last one is not a
/// double.
pub const DOUBLE_CLICK_THRESHOLD_MS: u64 = 250;

/// `MouseHandler.onButton`'s double-click detector (`MouseHandler.java`),
/// pulled out as a reusable primitive rather than re-derived per screen.
///
/// ## Why this exists here and not in `container.rs` or `widget.rs`
///
/// Vanilla's own detector is not a widget method at all — `AbstractSelectionList`
/// and `EditBox` both just *receive* an already-computed `doubleClick: bool` on
/// `mouseClicked`/`onClick`; the clock and the 250 ms comparison live one layer
/// up, in `MouseHandler`, and every consumer downstream is a plain `if
/// (doubleClick)`. That is the shape this type copies: one small, clock-fed
/// tracker that any screen's click handler can hold, rather than a per-screen
/// reimplementation of the same subtraction. `container.rs` already has its own
/// double-click handling for slot clicks (see `docs/container-clicks.md`) and is
/// owned by another agent right now — this is the primitive for everywhere
/// *else* a double-click means something (the server list today; a world-select
/// row tomorrow), not a replacement for that one.
///
/// ## What is intentionally simplified
///
/// Vanilla's real predicate is
/// `lastClick != null && currentTime - lastClick.time() < 250 &&
/// lastClick.screen() == screen && lastClickButton == event.button()`
/// (`MouseHandler.java`) — same **screen instance** and same **button**,
/// not just "recently". [`DoubleClickTracker::click`] folds "same screen" and
/// "same button" into a single caller-supplied `target: T` — the row/id being
/// clicked — because every consumer here already only feeds it clicks that
/// landed on a left-clickable row of one screen; a target mismatch (a different
/// row, or a click that missed the list entirely) is exactly what a screen
/// change or a button change would also invalidate in vanilla. If a future
/// caller needs the button distinguished too, fold it into `T` — e.g.
/// `(usize, MouseButton)` — rather than growing this type's arity.
///
/// `lastClick` is only *armed* by vanilla when `screen.mouseClicked` returns
/// `true`, i.e. the click was consumed. This type has no notion of "consumed" —
/// every call to [`Self::click`] arms it — so a caller must only call it for
/// clicks that actually hit something, the same discipline
/// [`FocusSet::mouse_clicked`] already asks of its own caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DoubleClickTracker<T> {
    /// The last click this tracker saw, or `None` before the first one.
    last: Option<(T, u64)>,
}

impl<T: Copy + PartialEq> DoubleClickTracker<T> {
    /// A tracker with no prior click.
    #[must_use]
    pub const fn new() -> Self {
        Self { last: None }
    }

    /// One click on `target` at `now_ms` (a monotonic millisecond clock the
    /// caller owns — see [`super::edit_box::EditBox::show_cursor`] for the same
    /// "caller supplies the clock" shape). Returns whether this click, paired
    /// with the previous one, is a double-click.
    ///
    /// Always records `(target, now_ms)` as the new "last click", double or
    /// not — matching `MouseHandler`'s own unconditional re-arm on every
    /// consumed click. This is what lets a fast triple-click report a double on
    /// clicks 2-and-3 as well as clicks 1-and-2, exactly as vanilla's pairwise
    /// comparison does.
    pub fn click(&mut self, now_ms: u64, target: T) -> bool {
        let is_double = matches!(
            self.last,
            Some((last_target, last_ms))
                if last_target == target && now_ms.saturating_sub(last_ms) < DOUBLE_CLICK_THRESHOLD_MS
        );
        self.last = Some((target, now_ms));
        is_double
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal screen: a `Vec` of widgets addressed by index.
    #[derive(Debug, Default)]
    struct Kids(Vec<Widget>);

    impl FocusChildren for Kids {
        fn get(&self, id: usize) -> Option<&dyn FocusTarget> {
            self.0.get(id).map(|w| w as &dyn FocusTarget)
        }

        fn get_mut(&mut self, id: usize) -> Option<&mut dyn FocusTarget> {
            self.0.get_mut(id).map(|w| w as &mut dyn FocusTarget)
        }
    }

    /// A widget that swallows every key, standing in for a focused text field.
    #[derive(Debug)]
    struct Greedy {
        widget: Widget,
        /// Keys this one declines, so the ordering can be exercised in both
        /// directions with the same type.
        declines: Vec<i32>,
        seen: Vec<i32>,
    }

    impl Greedy {
        fn new(x: f32, y: f32, declines: Vec<i32>) -> Self {
            Self {
                widget: Widget::new(x, y, 100.0, 20.0, "field"),
                declines,
                seen: Vec::new(),
            }
        }
    }

    impl FocusTarget for Greedy {
        fn rectangle(&self) -> ScreenRectangle {
            self.widget.rectangle()
        }
        fn is_active(&self) -> bool {
            self.widget.is_active()
        }
        fn is_focused(&self) -> bool {
            self.widget.focused
        }
        fn set_focused(&mut self, focused: bool) {
            self.widget.focused = focused;
        }
        fn is_mouse_over(&self, x: f32, y: f32) -> bool {
            self.widget.is_mouse_over(x, y)
        }
        fn key_pressed(&mut self, event: KeyEvent) -> bool {
            self.seen.push(event.key);
            !self.declines.contains(&event.key)
        }
    }

    #[derive(Debug)]
    struct Mixed {
        plain: Vec<Widget>,
        greedy: Greedy,
    }

    impl FocusChildren for Mixed {
        fn get(&self, id: usize) -> Option<&dyn FocusTarget> {
            match id {
                0 => Some(&self.greedy as &dyn FocusTarget),
                n => self.plain.get(n - 1).map(|w| w as &dyn FocusTarget),
            }
        }
        fn get_mut(&mut self, id: usize) -> Option<&mut dyn FocusTarget> {
            match id {
                0 => Some(&mut self.greedy as &mut dyn FocusTarget),
                n => self.plain.get_mut(n - 1).map(|w| w as &mut dyn FocusTarget),
            }
        }
    }

    fn column(n: usize) -> Kids {
        Kids(
            (0..n)
                .map(|i| Widget::button(20.0, 20.0 + 30.0 * i as f32, 100.0, 20.0, format!("b{i}")))
                .collect(),
        )
    }

    fn tab(set: &mut FocusSet, kids: &mut Kids, forward: bool) -> Option<usize> {
        let event = KeyEvent::with_modifiers(
            KEY_TAB,
            if forward { 0 } else { MOD_SHIFT },
        );
        set.screen_key_pressed(kids, event);
        set.focused()
    }

    #[test]
    fn tab_visits_children_in_insertion_order_and_wraps_through_clear_focus() {
        // The exact sequence, not a property — an ordering change has to fail
        // here and no `cargo check` can see it. Four buttons, all default
        // `tabOrderGroup`, so this is insertion order.
        let mut kids = column(4);
        let mut set = FocusSet::new();
        for id in 0..4 {
            set.add_renderable_widget(id);
        }
        assert_eq!(set.focused(), None, "a fresh screen has no focus");
        let seen: Vec<Option<usize>> = (0..6).map(|_| tab(&mut set, &mut kids, true)).collect();
        assert_eq!(
            seen,
            vec![Some(0), Some(1), Some(2), Some(3), Some(0), Some(1)],
            "forward Tab is insertion order and wraps 3 -> 0"
        );
        // Backward from 1: 0, then the wrap to the last.
        let back: Vec<Option<usize>> = (0..3).map(|_| tab(&mut set, &mut kids, false)).collect();
        assert_eq!(back, vec![Some(0), Some(3), Some(2)]);
        // And exactly one widget believes it is focused at any time.
        assert_eq!(
            kids.0.iter().filter(|w| w.focused).count(),
            1,
            "changeFocus clears the old leaf before applying the new one"
        );
    }

    #[test]
    fn tab_order_group_overrides_insertion_order_but_ties_keep_it() {
        // `handleTabNavigation` sorts by `getTabOrderGroup()` with a *stable*
        // sort, so group wins and insertion order breaks ties. Vanilla's
        // default group is 0, so this is only observable once something
        // overrides it — which is why the shipped behaviour cannot prove the
        // sort is stable and this test has to.
        #[derive(Debug)]
        struct Grouped(Widget, i32);
        impl FocusTarget for Grouped {
            fn rectangle(&self) -> ScreenRectangle {
                self.0.rectangle()
            }
            fn is_active(&self) -> bool {
                self.0.is_active()
            }
            fn is_focused(&self) -> bool {
                self.0.focused
            }
            fn set_focused(&mut self, focused: bool) {
                self.0.focused = focused;
            }
            fn is_mouse_over(&self, x: f32, y: f32) -> bool {
                self.0.is_mouse_over(x, y)
            }
            fn tab_order_group(&self) -> i32 {
                self.1
            }
        }
        #[derive(Debug)]
        struct G(Vec<Grouped>);
        impl FocusChildren for G {
            fn get(&self, id: usize) -> Option<&dyn FocusTarget> {
                self.0.get(id).map(|w| w as &dyn FocusTarget)
            }
            fn get_mut(&mut self, id: usize) -> Option<&mut dyn FocusTarget> {
                self.0.get_mut(id).map(|w| w as &mut dyn FocusTarget)
            }
        }
        // Insertion order 0,1,2,3 with groups 1,0,1,0 -> tab order 1,3,0,2.
        let mut kids = G(
            [1, 0, 1, 0]
                .iter()
                .enumerate()
                .map(|(i, &g)| {
                    Grouped(
                        Widget::button(0.0, 20.0 * i as f32, 100.0, 20.0, format!("g{i}")),
                        g,
                    )
                })
                .collect(),
        );
        let mut set = FocusSet::new();
        for id in 0..4 {
            set.add_renderable_widget(id);
        }
        let mut seen = Vec::new();
        for _ in 0..4 {
            set.screen_key_pressed(&mut kids, KeyEvent::new(KEY_TAB));
            seen.push(set.focused());
        }
        assert_eq!(seen, vec![Some(1), Some(3), Some(0), Some(2)]);
    }

    #[test]
    fn an_inactive_child_is_skipped_by_tab_but_still_counted_in_the_walk() {
        let mut kids = column(4);
        kids.0[1].active = false;
        let mut set = FocusSet::new();
        for id in 0..4 {
            set.add_renderable_widget(id);
        }
        let seen: Vec<Option<usize>> = (0..4).map(|_| tab(&mut set, &mut kids, true)).collect();
        assert_eq!(
            seen,
            vec![Some(0), Some(2), Some(3), Some(0)],
            "`nextFocusPath` returns null for an inactive widget"
        );
        // The control: re-enable it and the same walk must include it, or this
        // test is asserting an absence with no evidence the detector works.
        kids.0[1].active = true;
        set.clear_focus(&mut kids);
        let seen: Vec<Option<usize>> = (0..4).map(|_| tab(&mut set, &mut kids, true)).collect();
        assert_eq!(seen, vec![Some(0), Some(1), Some(2), Some(3)]);
    }

    #[test]
    fn a_single_focusable_child_re_lands_on_itself_because_the_wrap_clears_focus() {
        // The subtle consequence of the wrap being `clearFocus()` + retry
        // rather than modular arithmetic: `takes_focus` is
        // `isActive() && !isFocused()`, so the only child refuses the first
        // pass and accepts the second.
        let mut kids = column(1);
        let mut set = FocusSet::new();
        set.add_renderable_widget(0);
        assert_eq!(tab(&mut set, &mut kids, true), Some(0));
        assert_eq!(tab(&mut set, &mut kids, true), Some(0));
        assert!(kids.0[0].focused);
    }

    #[test]
    fn arrow_navigation_does_not_wrap_where_tab_does() {
        // `Screen.keyPressed`'s retry is gated on
        // `instanceof TabNavigation`, so falling off the bottom with Down
        // leaves focus where it was.
        let mut kids = column(3);
        let mut set = FocusSet::new();
        for id in 0..3 {
            set.add_renderable_widget(id);
        }
        set.set_initial_focus(&mut kids, 0);
        for expected in [1, 2, 2, 2] {
            set.screen_key_pressed(&mut kids, KeyEvent::new(KEY_DOWN));
            assert_eq!(set.focused(), Some(expected));
        }
        // The control: Tab from the same terminal position *does* move.
        assert_eq!(tab(&mut set, &mut kids, true), Some(0));
    }

    #[test]
    fn arrow_navigation_is_geometric_not_ordinal() {
        // Two columns, added in an order that has nothing to do with geometry:
        // right-top(0), left-bottom(1), left-top(2), right-bottom(3).
        let mut kids = Kids(vec![
            Widget::button(200.0, 20.0, 100.0, 20.0, "right top"),
            Widget::button(20.0, 80.0, 100.0, 20.0, "left bottom"),
            Widget::button(20.0, 20.0, 100.0, 20.0, "left top"),
            Widget::button(200.0, 80.0, 100.0, 20.0, "right bottom"),
        ]);
        let mut set = FocusSet::new();
        for id in 0..4 {
            set.add_renderable_widget(id);
        }
        set.set_initial_focus(&mut kids, 2); // left top
        set.screen_key_pressed(&mut kids, KeyEvent::new(KEY_DOWN));
        assert_eq!(set.focused(), Some(1), "down from left-top is left-bottom");
        set.screen_key_pressed(&mut kids, KeyEvent::new(KEY_RIGHT));
        assert_eq!(
            set.focused(),
            Some(3),
            "right from left-bottom is right-bottom, not the next-added widget"
        );
        set.screen_key_pressed(&mut kids, KeyEvent::new(KEY_UP));
        assert_eq!(set.focused(), Some(0), "up from right-bottom is right-top");
        // The control: Tab from the same place is *insertion* order, which
        // proves the two mechanisms are genuinely different rather than both
        // being the ordinal walk.
        assert_eq!(tab(&mut set, &mut kids, true), Some(1));
    }

    #[test]
    fn the_vague_pass_hops_columns_when_the_strict_pass_finds_nothing() {
        // Left column of two, and one widget far to the right and *below* both.
        // Down from the bottom-left has no orthogonal overlap anywhere, so only
        // `nextFocusPathVaguelyInDirection` can reach the third widget.
        let mut kids = Kids(vec![
            Widget::button(20.0, 20.0, 100.0, 20.0, "a"),
            Widget::button(20.0, 60.0, 100.0, 20.0, "b"),
            Widget::button(400.0, 200.0, 100.0, 20.0, "far"),
        ]);
        let mut set = FocusSet::new();
        for id in 0..3 {
            set.add_renderable_widget(id);
        }
        set.set_initial_focus(&mut kids, 1);
        assert!(
            !kids.0[2]
                .rectangle()
                .overlaps_in_axis(kids.0[1].rectangle(), ScreenAxis::Horizontal),
            "premise: the far widget shares no column with the focused one, so \
             the strict pass cannot be what finds it"
        );
        set.screen_key_pressed(&mut kids, KeyEvent::new(KEY_DOWN));
        assert_eq!(set.focused(), Some(2));
    }

    #[test]
    fn registries_are_not_interchangeable() {
        // The island: `addRenderableOnly` never appends to `children()`, so the
        // widget is drawn and receives nothing.
        let mut kids = column(2);
        let mut set = FocusSet::new();
        set.add_renderable_only(0);
        set.add_widget(1);
        assert_eq!(set.registry(0), Some(Registry::RenderableOnly));
        assert_eq!(set.registry(1), Some(Registry::Widget));
        assert!(set.registry(2).is_none(), "an unregistered id is in nothing");

        // A click squarely inside widget 0 reaches nobody.
        let (x, y) = (70.0, 30.0);
        assert!(kids.0[0].contains(x, y), "premise: the click is inside its rect");
        assert!(!set.mouse_clicked(&mut kids, x, y));
        assert_eq!(set.focused(), None);
        // Tab cannot reach it either.
        assert_eq!(tab(&mut set, &mut kids, true), Some(1));

        // The control, watched failing: register the *same* widget
        // interactively and every assertion above flips.
        let mut set = FocusSet::new();
        set.add_renderable_widget(0);
        set.add_widget(1);
        assert_eq!(set.registry(0), Some(Registry::RenderableWidget));
        assert!(set.mouse_clicked(&mut kids, x, y));
        assert_eq!(set.focused(), Some(0));

        // And the third distinction: `addWidget` is interactive but not drawn.
        assert!(!Registry::Widget.is_renderable());
        assert!(Registry::Widget.is_child());
        assert!(Registry::RenderableOnly.is_renderable());
        assert!(!Registry::RenderableOnly.is_child());
        assert!(Registry::RenderableWidget.is_renderable());
        assert!(Registry::RenderableWidget.is_child());
    }

    #[test]
    fn get_child_at_is_first_match_in_children_order_not_topmost() {
        // Two widgets on the same rect. Vanilla iterates `children()` forward
        // and takes the first hit, so the *earlier* one wins — there is no z
        // order anywhere in `getChildAt`.
        let mut kids = Kids(vec![
            Widget::button(0.0, 0.0, 100.0, 20.0, "under"),
            Widget::button(0.0, 0.0, 100.0, 20.0, "over"),
        ]);
        let mut set = FocusSet::new();
        set.add_renderable_widget(0);
        set.add_renderable_widget(1);
        assert_eq!(set.child_at(&kids, 50.0, 10.0), Some(0));
        set.mouse_clicked(&mut kids, 50.0, 10.0);
        assert_eq!(set.focused(), Some(0));
        // The control: registering them the other way round swaps the winner,
        // which shows the answer comes from list order and not from geometry.
        let mut set = FocusSet::new();
        set.add_renderable_widget(1);
        set.add_renderable_widget(0);
        assert_eq!(set.child_at(&kids, 50.0, 10.0), Some(1));
    }

    #[test]
    fn a_disabled_child_is_invisible_to_get_child_at() {
        // `isMouseOver` is `isActive() && areCoordinatesInRectangle(..)`, so a
        // disabled widget stacked over an enabled one does not eat the click.
        let mut kids = Kids(vec![
            Widget::button(0.0, 0.0, 100.0, 20.0, "disabled"),
            Widget::button(0.0, 0.0, 100.0, 20.0, "enabled"),
        ]);
        kids.0[0].active = false;
        let mut set = FocusSet::new();
        set.add_renderable_widget(0);
        set.add_renderable_widget(1);
        assert_eq!(set.child_at(&kids, 50.0, 10.0), Some(1));
        set.mouse_clicked(&mut kids, 50.0, 10.0);
        assert_eq!(set.focused(), Some(1));
    }

    #[test]
    fn should_take_focus_after_interaction_separates_clicking_from_focusing() {
        #[derive(Debug)]
        struct Transient(Widget);
        impl FocusTarget for Transient {
            fn rectangle(&self) -> ScreenRectangle {
                self.0.rectangle()
            }
            fn is_active(&self) -> bool {
                self.0.is_active()
            }
            fn is_focused(&self) -> bool {
                self.0.focused
            }
            fn set_focused(&mut self, focused: bool) {
                self.0.focused = focused;
            }
            fn is_mouse_over(&self, x: f32, y: f32) -> bool {
                self.0.is_mouse_over(x, y)
            }
            fn should_take_focus_after_interaction(&self) -> bool {
                false
            }
        }
        #[derive(Debug)]
        struct T(Transient);
        impl FocusChildren for T {
            fn get(&self, id: usize) -> Option<&dyn FocusTarget> {
                (id == 0).then_some(&self.0 as &dyn FocusTarget)
            }
            fn get_mut(&mut self, id: usize) -> Option<&mut dyn FocusTarget> {
                (id == 0).then_some(&mut self.0 as &mut dyn FocusTarget)
            }
        }
        let mut kids = T(Transient(Widget::button(0.0, 0.0, 100.0, 20.0, "x")));
        let mut set = FocusSet::new();
        set.add_renderable_widget(0);
        assert!(
            set.mouse_clicked(&mut kids, 50.0, 10.0),
            "the click still lands"
        );
        assert_eq!(set.focused(), None, "but focus does not follow it");
    }

    #[test]
    fn the_focused_child_sees_a_key_before_the_screen_interprets_it() {
        // The whole point of #395, as a sequence. Child 0 swallows everything;
        // children 1 and 2 are plain buttons.
        let mut kids = Mixed {
            greedy: Greedy::new(20.0, 20.0, vec![]),
            plain: vec![
                Widget::button(20.0, 60.0, 100.0, 20.0, "b"),
                Widget::button(20.0, 100.0, 100.0, 20.0, "c"),
            ],
        };
        let mut set = FocusSet::new();
        for id in 0..3 {
            set.add_renderable_widget(id);
        }
        set.set_initial_focus(&mut kids, 0);

        // Down and Tab both reach the field, and the field eats them, so focus
        // does not move — this is a text field swallowing navigation.
        for key in [KEY_DOWN, KEY_TAB, KEY_RIGHT] {
            assert_eq!(
                set.screen_key_pressed(&mut kids, KeyEvent::new(key)),
                KeyOutcome::Consumed
            );
            assert_eq!(set.focused(), Some(0));
        }
        assert_eq!(
            kids.greedy.seen,
            vec![KEY_DOWN, KEY_TAB, KEY_RIGHT],
            "every key was offered to the focused child first"
        );

        // The control: the same field declining Down and Tab. Nothing else
        // changes, and now both move focus — which is exactly how vanilla's
        // `EditBox` lets vertical arrows out while keeping horizontal ones.
        kids.greedy.declines = vec![KEY_DOWN, KEY_TAB];
        kids.greedy.seen.clear();
        assert_eq!(
            set.screen_key_pressed(&mut kids, KeyEvent::new(KEY_DOWN)),
            KeyOutcome::FocusMoved
        );
        assert_eq!(set.focused(), Some(1));
        assert_eq!(
            set.screen_key_pressed(&mut kids, KeyEvent::new(KEY_RIGHT)),
            KeyOutcome::Declined,
            "nothing lies right of the middle button, and arrows do not wrap"
        );
    }

    #[test]
    fn escape_is_answered_before_the_focused_child_and_before_navigation() {
        let mut kids = Mixed {
            greedy: Greedy::new(20.0, 20.0, vec![]),
            plain: vec![Widget::button(20.0, 60.0, 100.0, 20.0, "b")],
        };
        let mut set = FocusSet::new();
        set.add_renderable_widget(0);
        set.add_renderable_widget(1);
        set.set_initial_focus(&mut kids, 0);
        assert_eq!(
            set.screen_key_pressed(&mut kids, KeyEvent::new(KEY_ESCAPE)),
            KeyOutcome::Close
        );
        assert!(
            kids.greedy.seen.is_empty(),
            "the greedy field never sees Escape — it is answered first, which is \
             why a text field cannot lock a player out of the pause menu"
        );
        // `shouldCloseOnEsc() == false` (the death screen) hands it on instead,
        // and then the greedy child *does* see it.
        let mut set = FocusSet::new().without_close_on_esc();
        set.add_renderable_widget(0);
        // `set_focused` rather than `set_initial_focus`: the field is *already*
        // focused from the first half, so `takes_focus()` — which is
        // `isActive() && !isFocused()` — would decline and this half would be
        // testing an unfocused screen instead.
        set.set_focused(&mut kids, Some(0));
        assert_eq!(
            set.screen_key_pressed(&mut kids, KeyEvent::new(KEY_ESCAPE)),
            KeyOutcome::Consumed
        );
        assert_eq!(kids.greedy.seen, vec![KEY_ESCAPE]);
    }

    #[test]
    fn bound_in_direction_is_inclusive_so_touching_rects_do_not_overlap() {
        // A 100x20 rect at (10, 20): right bound is 109, not 110. Every
        // comparison in `nextFocusPathInDirection` uses this, and the off-by-one
        // makes abutting widgets look overlapped.
        let r = ScreenRectangle::new(10, 20, 100, 20);
        assert_eq!(r.bound_in_direction(ScreenDirection::Left), 10);
        assert_eq!(r.bound_in_direction(ScreenDirection::Right), 109);
        assert_eq!(r.bound_in_direction(ScreenDirection::Up), 20);
        assert_eq!(r.bound_in_direction(ScreenDirection::Down), 39);
        // Abutting: the next one starts exactly where this one's exclusive edge
        // is, and they must *not* overlap.
        let next = ScreenRectangle::new(110, 20, 100, 20);
        assert!(!r.overlaps_in_axis(next, ScreenAxis::Horizontal));
        assert!(r.overlaps_in_axis(next, ScreenAxis::Vertical));
        // One pixel of real overlap does register.
        let over = ScreenRectangle::new(109, 20, 100, 20);
        assert!(r.overlaps_in_axis(over, ScreenAxis::Horizontal));
        // `getBorder` is the 1 px sliver *outside* the named edge.
        let border = r.border(ScreenDirection::Right);
        assert_eq!(border, ScreenRectangle::new(110, 20, 1, 20));
        // Centre is on inclusive bounds and truncates: (10 + 109) / 2 = 59.
        assert_eq!(r.center_in_axis(ScreenAxis::Horizontal), 59);
        assert_eq!(r.center_in_axis(ScreenAxis::Vertical), 29);
    }

    #[test]
    fn the_edit_shortcut_modifier_is_cmd_on_macos_and_ctrl_elsewhere() {
        // `InputQuirks.java`. Asserted against `cfg!` rather than restated, so
        // this cannot agree with itself.
        assert_eq!(
            EDIT_SHORTCUT_MODIFIER,
            if cfg!(target_os = "macos") { MOD_SUPER } else { MOD_CONTROL }
        );
        let quirked = KeyEvent::with_modifiers(KEY_V, EDIT_SHORTCUT_MODIFIER);
        assert!(quirked.is_paste());
        assert!(!quirked.is_copy(), "and it keys on the letter too");
        // Shift or Alt disqualifies every one of them
        // (`InputWithModifiers.java`).
        assert!(
            !KeyEvent::with_modifiers(KEY_V, EDIT_SHORTCUT_MODIFIER | MOD_SHIFT).is_paste()
        );
        assert!(!KeyEvent::with_modifiers(KEY_V, EDIT_SHORTCUT_MODIFIER | MOD_ALT).is_paste());
        // The control: the *other* platform's modifier must not work, or the
        // constant is not doing anything.
        let other = if cfg!(target_os = "macos") { MOD_CONTROL } else { MOD_SUPER };
        assert!(!KeyEvent::with_modifiers(KEY_V, other).is_paste());
    }

    #[test]
    fn a_component_path_carries_ids_at_every_level() {
        let leaf = ComponentPath::leaf(7);
        assert_eq!((leaf.head(), leaf.leaf_id(), leaf.depth()), (7, 7, 0));
        let nested = ComponentPath::path(3, Some(ComponentPath::leaf(9))).expect("some in");
        assert_eq!((nested.head(), nested.leaf_id(), nested.depth()), (3, 9, 1));
        assert!(
            ComponentPath::path(3, None).is_none(),
            "`ComponentPath.path` is null-in null-out, which is why every \
             `nextFocusPath` caller can wrap unconditionally"
        );
    }

    #[test]
    fn menu_keys_map_onto_glfw_codes_and_chars_do_not() {
        use super::super::nav::MenuKey;
        assert_eq!(
            KeyEvent::from_menu_key(MenuKey::Tab).map(|e| e.key),
            Some(KEY_TAB)
        );
        assert_eq!(
            KeyEvent::from_menu_key(MenuKey::Backspace).map(|e| e.key),
            Some(KEY_BACKSPACE)
        );
        assert_eq!(
            KeyEvent::from_menu_key(MenuKey::Delete).map(|e| e.key),
            Some(KEY_DELETE)
        );
        assert_eq!(
            KeyEvent::from_menu_key(MenuKey::Up).map(|e| e.key),
            Some(KEY_UP)
        );
        assert_eq!(
            KeyEvent::from_menu_key(MenuKey::Down).map(|e| e.key),
            Some(KEY_DOWN)
        );
        assert!(
            KeyEvent::from_menu_key(MenuKey::Char('a')).is_none(),
            "a printable character is `charTyped`, a different callback — \
             smuggling it through `keyPressed` would make Ctrl+A's key code and \
             the letter 'a' the same event"
        );
    }

    #[test]
    fn the_double_click_threshold_matches_mousehandlers_own_250ms() {
        // `MouseHandler.java`, transcribed rather than picked.
        assert_eq!(DOUBLE_CLICK_THRESHOLD_MS, 250);
    }

    #[test]
    fn two_clicks_on_the_same_row_inside_the_threshold_double() {
        let mut t: DoubleClickTracker<usize> = DoubleClickTracker::new();
        assert!(!t.click(0, 3), "there is no previous click to pair with");
        assert!(
            t.click(100, 3),
            "100 ms after the first click on the same row must double, \
             predicted true (100 < 250)"
        );
    }

    #[test]
    fn two_clicks_slower_than_the_threshold_do_not_double() {
        // The coordinator's requested control: run it, watch it fail to
        // double. 300 ms is on the wrong side of `MouseHandler.java`'s strict
        // `< 250L`, so the predicted value is `false`, not "some smaller
        // truthiness" — a wrong hypothesis here would be `true` if the
        // threshold were mistakenly read as `<=` or padded upward.
        let mut t: DoubleClickTracker<usize> = DoubleClickTracker::new();
        assert!(!t.click(0, 3));
        assert!(
            !t.click(300, 3),
            "300 ms is slower than the 250 ms threshold — predicted false, \
             this must not join"
        );
        // And the *next* click, now only 50 ms after the miss, doubles against
        // that miss rather than staying stuck refusing forever.
        assert!(t.click(350, 3));
    }

    #[test]
    fn the_boundary_is_strictly_less_than_not_less_or_equal() {
        // `currentTime - lastClick.time() < 250L` (`MouseHandler.java`) is
        // strict. A click exactly at the boundary must not double — the wrong
        // hypothesis (`<=`) would report `true` here instead.
        let mut t: DoubleClickTracker<usize> = DoubleClickTracker::new();
        assert!(!t.click(0, 3));
        assert!(
            !t.click(250, 3),
            "250 ms is the threshold itself, not inside it — vanilla's `<` \
             excludes it"
        );
        let mut t2: DoubleClickTracker<usize> = DoubleClickTracker::new();
        assert!(!t2.click(0, 3));
        assert!(t2.click(249, 3), "249 ms is inside the window");
    }

    #[test]
    fn a_different_target_does_not_double_even_inside_the_threshold() {
        // Stands in for vanilla's `lastClick.screen() == screen` /
        // `lastClickButton == event.button()` guards — see the type's own
        // doc on why those are folded into `target` here. The control: the
        // same two clicks *do* double when the target is held fixed, so this
        // failure is really about the target comparison and not some other
        // reason the pair failed to double.
        let mut a: DoubleClickTracker<usize> = DoubleClickTracker::new();
        assert!(!a.click(0, 1));
        assert!(
            !a.click(50, 2),
            "row 2, 50 ms later, must not double against row 1's click"
        );

        let mut control: DoubleClickTracker<usize> = DoubleClickTracker::new();
        assert!(!control.click(0, 1));
        assert!(control.click(50, 1), "the control: same row does double");
    }

    #[test]
    fn a_fast_triple_click_doubles_on_both_adjacent_pairs() {
        // Vanilla re-arms `lastClick` on every consumed click, not just the
        // first of a pair, so clicks (1,2) and (2,3) are each their own
        // comparison — see the type's own doc.
        let mut t: DoubleClickTracker<usize> = DoubleClickTracker::new();
        assert!(!t.click(0, 7));
        assert!(t.click(80, 7), "pair (1,2)");
        assert!(t.click(150, 7), "pair (2,3), 70 ms after the double");
    }
}
