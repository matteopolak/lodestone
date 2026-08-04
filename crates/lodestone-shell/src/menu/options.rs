//! Vanilla's **whole options tree** — `OptionsScreen`, `OptionsSubScreen`,
//! `OptionsList` and the `OptionInstance` model — as data plus arithmetic, with
//! every control present and the ones this client does not honour rendered
//! **inactive**.
//!
//! This is issue #55, the settings branch of the menu-framework epic #392. The
//! leaf is #393 ([`super::widget`]), the containers are #394
//! ([`super::layout`]), and the plan of record is `docs/ui-framework.md`.
//! Per-screen detail is in `docs/settings-screen.md`.
//!
//! ## Why this is one mechanism and not thirteen screens
//!
//! `Options.java` declares 94 `OptionInstance` fields with 93 accessors, and
//! every settings sub-screen is the same three lines:
//! `HeaderAndFooterLayout` + an `OptionsList` + `addOptions()`. `OptionsList`
//! offers exactly three shapes — `addBig` (one 310 px control),
//! `addSmall` (two 150 px controls, 160 px apart) and `addHeader` — so a
//! settings screen is a **list of options**, not bespoke geometry. That is why
//! the census here is a table ([`Entry`]/[`Cell`]) rather than 127 hand-placed
//! widgets, and why adding a screen is adding a `static`.
//!
//! ## `active = false` is the entire disabled path
//!
//! There is no disabled widget type in vanilla and none here — see
//! [`super::widget`]'s module docs. [`Cell::is_live`] is what decides it, and
//! it answers `false` for **112 of the 135** controls this module renders (the
//! twenty-three live ones are seven real options — `guiScale`/`bobView` from
//! #55, plus `toggleCrouch`/`toggleSprint`/`invertMouseX`/`invertMouseY`/
//! `mouseWheelSensitivity` from #200/#202/#203 — eight `Done` buttons and
//! eight working nav buttons). That ratio is the point of the issue: a greyed row in
//! vanilla's own position makes the gap between this client and vanilla
//! *visible*, where a missing row silently changes the screen's shape.
//!
//! Vanilla disables its own controls for exactly this reason — the narrator
//! button (`OptionsSubScreen.java:43-46`), the anisotropy slider
//! (`VideoSettingsScreen.java:166-167`), telemetry
//! (`OptionsScreen.java:88-92`) — so this is copying an idiom, not inventing
//! one.
//!
//! ## What is deliberately *not* faithful, and why
//!
//! Four departures, each measured rather than guessed:
//!
//! 1. **An inactive option shows its caption alone**, where vanilla shows
//!    `genericValueLabel(caption, value)` — `"%s: %s"`
//!    (`Options.java:1974-1976`). We hold no value for an option we do not
//!    honour, and printing one would be exactly the fabricated persistence this
//!    issue exists to avoid: a row reading `Entity Shadows: ON` next to a client
//!    that draws no shadows is a lie a screenshot cannot distinguish from a
//!    working feature. The two live options *do* use `genericValueLabel`.
//! 2. **An inactive slider draws its track and no handle.** The handle's
//!    position *is* the value (`AbstractSliderButton.extractWidgetRenderState`),
//!    so drawing one at 0 is the same fabrication as (1) in pixels instead of
//!    text. This is the one place where the absence of a component is the honest
//!    render; it is not "disabled art", which
//!    [`super::widget`] correctly forbids for this widget family.
//! 3. **The scroll snaps to whole entries and the visible window is fixed at
//!    [`LIST_WINDOW_PX`].** `AbstractSelectionList` scrolls continuously and
//!    scissors the band; this menu pipeline has no scissor, so a row that
//!    overran the band would paint over the footer. The window is therefore
//!    derived from the *shortest* content band any `gui_scale` can produce
//!    (`config::MIN_SCALED_HEIGHT`), which makes it correct at every canvas and
//!    conservative at large ones. `super::accounts`' `VISIBLE_ROWS` is the
//!    existing precedent for the same trade.
//! 4. **Up/Down move the cursor over *every* control, including inactive
//!    ones** — where `AbstractWidget.nextFocusPath` skips them
//!    (`AbstractWidget.java:152-158`), as [`super::nav`]'s `step_enabled` does
//!    on the title and pause screens. On a screen whose *content* is the
//!    inactive majority, skipping them would leave 112 of 135 rows unreachable
//!    **and unscrollable**, i.e. invisible — which defeats the whole issue. The
//!    vanilla predicate still governs activation: Enter consults
//!    [`Widget::takes_focus`](super::widget::Widget::takes_focus)'s `is_active`
//!    half, so a cursor on an inactive row does nothing, and
//!    `WidgetSprites::get(false, true)` keeps it drawing `widget/button_disabled`
//!    under the cursor exactly as vanilla does.
//!
//! ## Dependencies
//!
//! - [`super::layout`] — `HeaderAndFooterLayout` is this module's **first
//!   production consumer** (#394 landed it with arithmetic-only gates and a
//!   note saying so). `GridLayout` and `LinearLayout` build
//!   `OptionsScreen.init`'s own tree.
//! - [`super::widget`] — `Widget`, `WidgetSprites`, the grey label.
//! - [`super::render`] — [`Origin::Settings`] resolves a [`Placement`] to a
//!   rect; `draw_widget` draws the row.
//! - [`crate::config`] — the two options that are real.

use super::layout::{self, HeaderAndFooterLayout, LayoutSettings, LinearLayout};
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot};
use super::widget::{self, LayoutElement, Widget};

// -- vanilla's metrics ------------------------------------------------------
//
// Every number below is transcribed from `.cache/mc/26.2/client-src`, with the
// file and line named, in logical GUI pixels. Nothing here is measured off our
// own output.

/// `OptionsList.BIG_BUTTON_WIDTH` (`OptionsList.java:17`) — an `addBig` row, and
/// also the row width `getRowWidth()` returns (`:64-66`).
pub const BIG_BUTTON_WIDTH: f32 = 310.0;
/// The width `OptionInstance.createButton(options)` defaults to
/// (`OptionInstance.java:123-125`), i.e. every `addSmall` control.
/// `Button.DEFAULT_WIDTH` (`Button.java:13`) is the same 150.
pub const SMALL_BUTTON_WIDTH: f32 = widget::DEFAULT_WIDTH;
/// `OptionsList.DEFAULT_ITEM_HEIGHT` (`OptionsList.java:18`), passed as the
/// list's `itemHeight` (`:24`).
pub const DEFAULT_ITEM_HEIGHT: f32 = 25.0;
/// `OptionsList.Entry.X_OFFSET` (`OptionsList.java:113`): the pitch between the
/// two columns of an `addSmall` row. Note it is **not** `SMALL_BUTTON_WIDTH`
/// plus a gap that anything else in the file names — 160 is written down.
pub const COLUMN_PITCH: f32 = 160.0;
/// `OptionsList.Entry.extractContent`'s `this.screen.width / 2 - 155`
/// (`OptionsList.java:150`). Kept as the inset rather than `BIG_BUTTON_WIDTH /
/// 2` because that is how the jar spells it, and because the two would silently
/// stop agreeing if `getRowWidth()` ever changed alone.
pub const ROW_LEFT_INSET: f32 = 155.0;
/// `AbstractSelectionList.getFirstEntryY()`'s `getY() + 2`
/// (`AbstractSelectionList.java:104-106`).
pub const LIST_TOP_INSET: f32 = 2.0;
/// `AbstractSelectionList.Entry.getContentY()`'s `getY() + 2` (`:481-483`) —
/// where a row's widget is placed inside its 25 px entry.
pub const ENTRY_CONTENT_INSET: f32 = 2.0;
/// The `int lineHeight = 9` in `OptionsList.addHeader` (`OptionsList.java:57`),
/// which is also `StringWidget`'s own height (`StringWidget.java:18-20`).
pub const HEADER_LINE_HEIGHT: f32 = 9.0;
/// `OptionsList.addHeader`'s `paddingTop` for every header **after** the first:
/// `lineHeight * 2` (`OptionsList.java:58`). The first header in a list gets
/// `0`, which is the whole reason this is a function of position rather than a
/// constant height.
pub const HEADER_PADDING_TOP: f32 = HEADER_LINE_HEIGHT * 2.0;
/// The `+ 4` in `addHeader`'s `paddingTop + lineHeight + 4` (`:59`).
pub const HEADER_PADDING_BOTTOM: f32 = 4.0;

/// `HeaderAndFooterLayout.DEFAULT_HEADER_AND_FOOTER_HEIGHT` — every
/// `OptionsSubScreen`'s header band, and *every* page's footer band
/// (`OptionsSubScreen.java:19` takes the 1-argument constructor).
pub const SUB_HEADER_HEIGHT: f32 = layout::DEFAULT_HEADER_AND_FOOTER_HEIGHT;
/// The footer band, on every page including the root.
pub const FOOTER_HEIGHT: f32 = layout::DEFAULT_HEADER_AND_FOOTER_HEIGHT;
/// `new HeaderAndFooterLayout(this, 61, 33)` (`OptionsScreen.java:37`) — the
/// root screen is the one page with a taller header, because its header carries
/// the FOV slider and the Online button under the title.
pub const ROOT_HEADER_HEIGHT: f32 = 61.0;
/// `LinearLayout.vertical().spacing(8)` and `LinearLayout.horizontal()…
/// spacing(8)` in `OptionsScreen.init` (`:52,55`), and the accessibility
/// footer's `spacing(8)` (`AccessibilityOptionsScreen.java:78`).
pub const ROOT_SPACING: i32 = 8;
/// `gridLayout.defaultCellSetting().paddingHorizontal(4)`
/// (`OptionsScreen.java:68`).
pub const GRID_PADDING_H: i32 = 4;
/// `…paddingBottom(4)` on the same line.
pub const GRID_PADDING_BOTTOM: i32 = 4;
/// `OptionsScreen.COLUMNS` (`OptionsScreen.java:36`).
pub const GRID_COLUMNS: usize = 2;
/// `Button.builder(GUI_DONE, …).width(200)` (`OptionsSubScreen.java:52`,
/// `OptionsScreen.java:96`).
pub const DONE_WIDTH: f32 = 200.0;
/// Every menu button's height — `Button.DEFAULT_HEIGHT` (`Button.java:15`).
pub const WIDGET_H: f32 = widget::DEFAULT_HEIGHT;

/// How many pixels of list a page may show, measured from
/// `getFirstEntryY()`.
///
/// This is the **shortest** content band any `gui_scale` can produce:
/// `calculate_gui_scale` clamps the logical canvas to at least
/// [`crate::config::MIN_SCALED_HEIGHT`] (vanilla's `Window.java:453`), so a band
/// of `MIN_SCALED_HEIGHT - header - footer` is available at every scale and the
/// window derived from it can never overrun the footer. Deliberately
/// conservative on a tall canvas — see the module docs' departure (3).
pub const LIST_WINDOW_PX: f32 =
    crate::config::MIN_SCALED_HEIGHT as f32 - SUB_HEADER_HEIGHT - FOOTER_HEIGHT - LIST_TOP_INSET;

// -- the option model -------------------------------------------------------

/// Which widget vanilla's `OptionInstance.createButton` builds for an option
/// (`OptionInstance.java:127-135`).
///
/// The dispatch is on the `ValueSet`: a `CycleableValueSet` gets a
/// `CycleButton` (`:232-249`) and a `SliderableValueSet` an
/// `OptionInstanceSliderButton` (`:368`). `SliderableOrCyclableValueSet` asks
/// `createCycleButton()` (`:525-541`), and the one implementor —
/// `ClampingLazyMaxIntRange`, which is `guiScale`'s — answers **`true`**
/// (`:213-216`). So GUI Scale is a cycle button, not a slider, which is why
/// this client's Enter-cycles binding was already the faithful one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionWidget {
    /// A `CycleButton`: booleans, `Enum`, `AltEnum`, `LazyEnum` and
    /// `ClampingLazyMaxIntRange`. Extends `AbstractButton`, so it uses
    /// [`widget::BUTTON_SPRITES`] and has real disabled art.
    Cycle,
    /// An `OptionInstance.OptionInstanceSliderButton`: `IntRange`,
    /// `UnitDouble`, `SliderableEnum`. Extends `AbstractSliderButton`, which
    /// **bypasses `WidgetSprites`** and has no disabled sprite —
    /// [`widget::SLIDER_SPRITES`] is the two-state collapse of what it picks
    /// between by hand.
    Slider,
}

/// A persisted option this client genuinely honours.
///
/// See [`crate::config::Options`], whose fields (besides `keybinds`, not a
/// vanilla `OptionInstance`) this enum enumerates one-for-one. **`render_distance`
/// and `sensitivity` are not here**, and the census in #55 and
/// `docs/ui-framework.md` is wrong to list them: both live on
/// [`crate::config::Config`], which is parsed from argv every run and *never
/// written back* (`config.rs`'s own doc comment says so). A settings row that
/// appeared to set them would be fabricated persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveOption {
    /// `options.guiScale` → [`crate::config::Options::gui_scale`]. Threaded to
    /// every menu and HUD draw through `render::logical_canvas`.
    GuiScale,
    /// `options.viewBobbing` → [`crate::config::Options::view_bobbing`]. Read
    /// per presented frame by `app.rs` and handed to `Sim::set_view_bobbing`;
    /// see `docs/view-bobbing.md`.
    ViewBobbing,
    /// `key.sneak` → [`crate::config::Options::toggle_sneak`] (issue #202).
    /// Fed to `InputState::set_toggle_modes` every tick.
    ToggleSneak,
    /// `key.sprint` → [`crate::config::Options::toggle_sprint`] (issue #202).
    ToggleSprint,
    /// `options.invertMouseX` → [`crate::config::Options::invert_mouse_x`]
    /// (issue #203). Fed to `apply_look_inverted`.
    InvertMouseX,
    /// `options.invertMouseY` → [`crate::config::Options::invert_mouse_y`]
    /// (issue #203).
    InvertMouseY,
    /// `options.mouseWheelSensitivity` →
    /// [`crate::config::Options::mouse_wheel_sensitivity`] (issue #203). Fed
    /// to the hotbar scroll handler.
    MouseWheelSensitivity,
}

/// One vanilla `OptionInstance`, reduced to what a row needs.
///
/// `accessor` is the census key — `Options.java`'s own method name — so a row on
/// screen can be traced back to the field it stands for without guessing from
/// the caption. It is also what `the_census_matches_the_written_one` counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionSpec {
    /// The `Options.java` accessor, e.g. `"renderDistance"`.
    pub accessor: &'static str,
    /// The caption, verbatim from `assets/minecraft/lang/en_us.json`.
    pub caption: &'static str,
    /// Which widget vanilla builds for it.
    pub widget: OptionWidget,
    /// The persisted option it drives, or `None` — the inactive majority.
    pub live: Option<LiveOption>,
}

/// What a settings screen does when a non-option button is activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The footer's `Done`, and Escape's equivalent: leave this page.
    Done,
    /// A control that is present because vanilla has it and inactive because
    /// this client cannot do it — the accessibility guide's external link
    /// (`AccessibilityOptionsScreen.java:79-81`) and Credits & Attribution
    /// (`OptionsScreen.java:94`).
    Unsupported,
}

/// One focusable widget on a settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// An `OptionInstance` widget.
    Option(OptionSpec),
    /// A `Button` that opens another screen. `None` names a vanilla screen this
    /// client does not have, and the row is inactive — [`SettingsPage`]'s docs
    /// say which and why.
    Nav {
        /// The button's label, verbatim from `en_us.json`.
        label: &'static str,
        /// The destination, or `None` for a screen we do not build.
        page: Option<SettingsPage>,
    },
    /// A `Button` that is neither an option nor navigation.
    Act {
        /// The button's label, verbatim from `en_us.json`.
        label: &'static str,
        /// What activating it does.
        act: Action,
    },
}

impl Cell {
    /// The label drawn on the widget.
    ///
    /// An option shows `genericValueLabel(caption, value)` — vanilla's
    /// `"%s: %s"` (`Options.java:1974-1976`) — when we hold a value for it, and
    /// its **caption alone** when we do not. See the module docs' departure (1)
    /// for why that is not an omission.
    #[must_use]
    pub fn label(self, options: &crate::config::Options) -> String {
        match self {
            Cell::Option(spec) => match spec.live {
                Some(live) => generic_value_label(spec.caption, &live_value(live, options)),
                None => spec.caption.to_string(),
            },
            Cell::Nav { label, .. } | Cell::Act { label, .. } => label.to_string(),
        }
    }

    /// Whether this control can be activated, i.e. `AbstractWidget.active`.
    ///
    /// An option is live only if it drives something in
    /// [`crate::config::Options`]; a nav button is live only if its destination
    /// is a page we build; `Done` is always live and
    /// [`Action::Unsupported`] never is.
    #[must_use]
    pub fn is_live(self) -> bool {
        match self {
            Cell::Option(spec) => spec.live.is_some(),
            Cell::Nav { page, .. } => page.is_some(),
            Cell::Act { act, .. } => act == Action::Done,
        }
    }

    /// Whether this control draws as `AbstractSliderButton` rather than a
    /// `Button`.
    #[must_use]
    pub fn is_slider(self) -> bool {
        matches!(
            self,
            Cell::Option(OptionSpec {
                widget: OptionWidget::Slider,
                ..
            })
        )
    }
}

/// `Options.genericValueLabel` (`Options.java:1974-1976`):
/// `Component.translatable("options.generic_value", caption, value)`, whose
/// `en_us.json` pattern is `"%s: %s"`.
#[must_use]
pub fn generic_value_label(caption: &str, value: &str) -> String {
    format!("{caption}: {value}")
}

/// The displayed value of one live option.
///
/// `guiScale`'s stringifier is `value == 0 ? "options.guiScale.auto" :
/// literal(value)` (`Options.java:908`) — note it returns the value **without**
/// the caption, which is why `CycleButton` composes them and this does not.
/// `bobView` is a plain boolean, so `CycleButton.onOffBuilder`'s
/// `options.on`/`options.off` apply: `"ON"`/`"OFF"`, upper case in `en_us.json`.
#[must_use]
pub fn live_value(live: LiveOption, options: &crate::config::Options) -> String {
    match live {
        LiveOption::GuiScale => {
            if options.gui_scale == crate::config::AUTO_GUI_SCALE {
                "Auto".to_string()
            } else {
                options.gui_scale.to_string()
            }
        }
        LiveOption::ViewBobbing => {
            if options.view_bobbing { "ON" } else { "OFF" }.to_string()
        }
        // `ToggleKeyMapping`'s own stringifier is `value ? KEY_TOGGLE :
        // KEY_HOLD` (`ToggleKeyMapping`'s caller in `Options.java:605-609`),
        // i.e. "Toggle"/"Hold" — **not** ON/OFF, unlike every other boolean
        // option on this page. `en_us.json`'s `options.key.toggle`/
        // `options.key.hold`.
        LiveOption::ToggleSneak => {
            if options.toggle_sneak { "Toggle" } else { "Hold" }.to_string()
        }
        LiveOption::ToggleSprint => {
            if options.toggle_sprint { "Toggle" } else { "Hold" }.to_string()
        }
        LiveOption::InvertMouseX => {
            if options.invert_mouse_x { "ON" } else { "OFF" }.to_string()
        }
        LiveOption::InvertMouseY => {
            if options.invert_mouse_y { "ON" } else { "OFF" }.to_string()
        }
        // `String.format(Locale.ROOT, "%.2f", value)` (`Options.java:479`).
        LiveOption::MouseWheelSensitivity => {
            format!("{:.2}", options.mouse_wheel_sensitivity)
        }
    }
}

/// One row of an `OptionsList`, i.e. one `addBig` / `addSmall` / `addHeader`
/// call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// `addHeader(text)`: a `StringWidget`, not a control. Its height is
    /// `paddingTop + 9 + 4` and its `paddingTop` is `0` for the first entry in
    /// the list and `18` otherwise (`OptionsList.java:56-60`) — which is why
    /// [`entry_height`] takes the index.
    Header(&'static str),
    /// `addBig(option)`: one 310 px control on its own row.
    Big(Cell),
    /// `addSmall(a, b)`: two 150 px controls 160 px apart, or one when the
    /// option count is odd (`OptionsList.java:37-42`).
    Small(Cell, Option<Cell>),
}

const fn cycle(accessor: &'static str, caption: &'static str) -> Cell {
    Cell::Option(OptionSpec {
        accessor,
        caption,
        widget: OptionWidget::Cycle,
        live: None,
    })
}

const fn slider(accessor: &'static str, caption: &'static str) -> Cell {
    Cell::Option(OptionSpec {
        accessor,
        caption,
        widget: OptionWidget::Slider,
        live: None,
    })
}

const fn live_cycle(accessor: &'static str, caption: &'static str, live: LiveOption) -> Cell {
    Cell::Option(OptionSpec {
        accessor,
        caption,
        widget: OptionWidget::Cycle,
        live: Some(live),
    })
}

/// As [`live_cycle`], for a slider-widget option — issues #200/#202/#203's
/// `mouseWheelSensitivity` is the first slider to leave the "labels only"
/// set. A click steps it by one increment, the same simplification
/// `guiScale` already uses (`SettingsOutcome::Cycle` has one variant for both
/// widget kinds — see that type's doc).
const fn live_slider(accessor: &'static str, caption: &'static str, live: LiveOption) -> Cell {
    Cell::Option(OptionSpec {
        accessor,
        caption,
        widget: OptionWidget::Slider,
        live: Some(live),
    })
}

const fn nav(label: &'static str, page: SettingsPage) -> Cell {
    Cell::Nav {
        label,
        page: Some(page),
    }
}

/// A nav button to a vanilla screen this client does not build. Present and
/// inactive; [`SettingsPage`]'s docs list every one and the reason.
const fn no_screen(label: &'static str) -> Cell {
    Cell::Nav { label, page: None }
}

const fn done() -> Cell {
    Cell::Act {
        label: "Done",
        act: Action::Done,
    }
}

const fn unsupported(label: &'static str) -> Cell {
    Cell::Act {
        label,
        act: Action::Unsupported,
    }
}

const fn head(text: &'static str) -> Entry {
    Entry::Header(text)
}

const fn big(cell: Cell) -> Entry {
    Entry::Big(cell)
}

const fn pair(a: Cell, b: Cell) -> Entry {
    Entry::Small(a, Some(b))
}

const fn lone(a: Cell) -> Entry {
    Entry::Small(a, None)
}

// -- the census -------------------------------------------------------------

/// `VideoSettingsScreen.addOptions` (`:142-150`), in its own order: the three
/// headers, the inline `fullscreenOption` built at `:108-141`, then
/// `displayOptions` (`:66-77`), `graphicsPreset`, `qualityOptions` (`:45-64`)
/// and `preferenceOptions` (`:79-81`).
///
/// The pairing is `addSmall`'s: it walks the array two at a time
/// (`OptionsList.java:37-42`), so the two columns of a row are consecutive
/// entries of vanilla's array and the last one is alone if the count is odd.
static VIDEO: &[Entry] = &[
    head("Display"),
    big(slider("fullscreenResolution", "Fullscreen Resolution")),
    pair(
        slider("framerateLimit", "Max Framerate"),
        cycle("enableVsync", "VSync"),
    ),
    pair(
        cycle("inactivityFpsLimit", "Reduce FPS when"),
        live_cycle("guiScale", "GUI Scale", LiveOption::GuiScale),
    ),
    pair(
        cycle("fullscreen", "Fullscreen"),
        cycle("exclusiveFullscreen", "Exclusive Fullscreen"),
    ),
    pair(
        slider("gamma", "Brightness"),
        cycle("preferredGraphicsBackend", "Graphics API"),
    ),
    head("Quality & Performance"),
    big(slider("graphicsPreset", "Preset")),
    pair(
        slider("biomeBlendRadius", "Biome Blend"),
        slider("renderDistance", "Render Distance"),
    ),
    pair(
        cycle("prioritizeChunkUpdates", "Chunk Builder"),
        slider("simulationDistance", "Simulation Distance"),
    ),
    pair(
        cycle("ambientOcclusion", "Smooth Lighting"),
        cycle("cloudStatus", "Clouds"),
    ),
    pair(
        cycle("particles", "Particles"),
        slider("mipmapLevels", "Mipmap Levels"),
    ),
    pair(
        cycle("entityShadows", "Entity Shadows"),
        slider("entityDistanceScaling", "Entity Distance"),
    ),
    pair(
        slider("menuBackgroundBlurriness", "Menu Background Blur"),
        slider("cloudRange", "Cloud Distance"),
    ),
    pair(
        cycle("cutoutLeaves", "See-Through Leaves"),
        cycle("improvedTransparency", "Improved Transparency"),
    ),
    pair(
        cycle("textureFiltering", "Texture Filtering"),
        slider("maxAnisotropyBit", "Anisotropic Filtering"),
    ),
    lone(slider("weatherRadius", "Weather Effect Radius")),
    head("Preferences"),
    pair(
        cycle("showAutosaveIndicator", "Autosave Indicator"),
        cycle("vignette", "Show Vignette"),
    ),
    pair(
        cycle("attackIndicator", "Attack Indicator"),
        slider("chunkSectionFadeInTime", "Chunk Fade Time"),
    ),
];

/// `ControlsScreen.addOptions` (`controls/ControlsScreen.java:26-36`).
///
/// The four `toggle*` options are the only ones in the tree whose caption is a
/// **keybind** name rather than an `options.*` key — `key.sneak`, `key.sprint`,
/// `key.attack`, `key.use` (`Options.java:603-629`) — and their values are
/// `options.key.toggle`/`options.key.hold` rather than ON/OFF.
///
/// **Sneak and Sprint are live** (issue #202) — [`crate::config::Options::toggle_sneak`]/
/// `toggle_sprint`, read by `InputState::set` (`lodestone-controller`).
/// Attack/Destroy and Use Item/Place Block stay inactive: #202 is scoped to
/// movement, and this crate's attack/use handling (`interact.rs`) has no
/// toggle concept to hang a mode off yet.
static CONTROLS: &[Entry] = &[
    pair(
        nav("Mouse Settings...", SettingsPage::Mouse),
        no_screen("Key Binds..."),
    ),
    pair(
        live_cycle("toggleCrouch", "Sneak", LiveOption::ToggleSneak),
        live_cycle("toggleSprint", "Sprint", LiveOption::ToggleSprint),
    ),
    pair(
        cycle("toggleAttack", "Attack/Destroy"),
        cycle("toggleUse", "Use Item/Place Block"),
    ),
    pair(
        cycle("autoJump", "Auto-Jump"),
        slider("sprintWindow", "Sprint Window"),
    ),
    lone(cycle("operatorItemsTab", "Operator Items Tab")),
];

/// `MouseSettingsScreen.addOptions` (`:23-29`).
///
/// `rawMouseInput` is included: vanilla appends it only when
/// `InputConstants.isRawMouseInputSupported()`, which is true on every desktop
/// GLFW build, so the seven-control shape is the one a player sees.
///
/// **Scroll Sensitivity and both inverts are live** (issue #203) —
/// [`crate::config::Options::mouse_wheel_sensitivity`]/`invert_mouse_x`/
/// `invert_mouse_y`. **Sensitivity (look) is not, and stays inactive
/// deliberately**: it lives on [`crate::config::Config`], parsed from argv and
/// never written back (see [`LiveOption`]'s doc) — a settings row that
/// appeared to persist it would be fabricated. `discreteMouseScroll`,
/// `allowCursorChanges` and `rawMouseInput` are also still inactive: none of
/// the three has a consumer in this shell yet (there is no discrete-vs-continuous
/// scroll distinction, no OS cursor swap, and no raw-input toggle), so wiring
/// the label without the behaviour would be exactly the fabrication #203
/// exists to fix, one row over.
static MOUSE: &[Entry] = &[
    pair(
        slider("sensitivity", "Sensitivity"),
        live_slider(
            "mouseWheelSensitivity",
            "Scroll Sensitivity",
            LiveOption::MouseWheelSensitivity,
        ),
    ),
    pair(
        cycle("discreteMouseScroll", "Discrete Scrolling"),
        live_cycle("invertMouseX", "Invert Mouse X", LiveOption::InvertMouseX),
    ),
    pair(
        live_cycle("invertMouseY", "Invert Mouse Y", LiveOption::InvertMouseY),
        cycle("allowCursorChanges", "Allow Cursor Changes"),
    ),
    lone(cycle("rawMouseInput", "Raw Input")),
];

/// `SoundOptionsScreen.addOptions` (`:18-24`).
///
/// The eleven volume sliders are `SoundSource.values()` in declaration order
/// (`sounds/SoundSource.java:3-14`) with `MASTER` pulled out into the `addBig`
/// row; their captions are `soundCategory.<name>`.
static SOUND: &[Entry] = &[
    big(slider("soundSource.master", "Master Volume")),
    pair(
        slider("soundSource.music", "Music"),
        slider("soundSource.record", "Jukebox/Note Blocks"),
    ),
    pair(
        slider("soundSource.weather", "Weather"),
        slider("soundSource.block", "Blocks"),
    ),
    pair(
        slider("soundSource.hostile", "Hostile Mobs"),
        slider("soundSource.neutral", "Friendly Mobs"),
    ),
    pair(
        slider("soundSource.player", "Players"),
        slider("soundSource.ambient", "Ambient/Environment"),
    ),
    pair(
        slider("soundSource.voice", "Narrator/Voice"),
        slider("soundSource.ui", "UI"),
    ),
    big(cycle("soundDevice", "Device")),
    pair(
        cycle("showSubtitles", "Closed Captions"),
        cycle("directionalAudio", "Directional Audio"),
    ),
    pair(
        cycle("musicFrequency", "Music Frequency"),
        cycle("musicToast", "Music Toast"),
    ),
];

/// `ChatOptionsScreen.options` (`:11-32`), paired two at a time.
static CHAT: &[Entry] = &[
    pair(
        cycle("chatVisibility", "Chat"),
        cycle("chatColors", "Colors"),
    ),
    pair(
        cycle("chatLinks", "Web Links"),
        cycle("chatLinksPrompt", "Prompt on Links"),
    ),
    pair(
        slider("chatOpacity", "Chat Text Opacity"),
        slider("textBackgroundOpacity", "Text Background Opacity"),
    ),
    pair(
        slider("chatScale", "Chat Text Size"),
        slider("chatLineSpacing", "Line Spacing"),
    ),
    pair(
        slider("chatDelay", "Chat Delay"),
        slider("chatWidth", "Width"),
    ),
    pair(
        slider("chatHeightFocused", "Focused Height"),
        slider("chatHeightUnfocused", "Unfocused Height"),
    ),
    pair(
        cycle("narrator", "Narrator"),
        cycle("autoSuggestions", "Command Suggestions"),
    ),
    pair(
        cycle("hideMatchedNames", "Hide Matched Names"),
        cycle("reducedDebugInfo", "Reduced Debug Info"),
    ),
    pair(
        cycle("onlyShowSecureChat", "Only Show Secure Chat"),
        cycle("saveChatDrafts", "Save Unsent Chats"),
    ),
];

/// `AccessibilityOptionsScreen.addOptions` (`:66-71`).
///
/// The shape is unusual and reproduced rather than tidied: the **narrator** is
/// pulled out of the option array and paired with a link to the Controls screen
/// (`:68-70`), then the remaining 23 options are paired two at a time. So the
/// narrator is the top-left control and the array's own order resumes at
/// `showSubtitles`.
///
/// `bobView` is vanilla's View Bobbing and one of this client's two live
/// options — note it lives *here*, on Accessibility, not on Video, in 26.2.
static ACCESSIBILITY: &[Entry] = &[
    pair(
        cycle("narrator", "Narrator"),
        nav("Controls...", SettingsPage::Controls),
    ),
    pair(
        cycle("showSubtitles", "Closed Captions"),
        cycle("highContrast", "High Contrast"),
    ),
    pair(
        slider("menuBackgroundBlurriness", "Menu Background Blur"),
        slider("textBackgroundOpacity", "Text Background Opacity"),
    ),
    pair(
        cycle("backgroundForChatOnly", "Text Background"),
        slider("chatOpacity", "Chat Text Opacity"),
    ),
    pair(
        slider("chatLineSpacing", "Line Spacing"),
        slider("chatDelay", "Chat Delay"),
    ),
    pair(
        slider("notificationDisplayTime", "Notification Time"),
        live_cycle("bobView", "View Bobbing", LiveOption::ViewBobbing),
    ),
    pair(
        slider("screenEffectScale", "Distortion Effects"),
        slider("fovEffectScale", "FOV Effects"),
    ),
    pair(
        slider("darknessEffectScale", "Darkness Pulsing"),
        slider("damageTiltStrength", "Damage Tilt"),
    ),
    pair(
        slider("glintSpeed", "Glint Speed"),
        slider("glintStrength", "Glint Strength"),
    ),
    pair(
        cycle("hideLightningFlash", "Hide Sky Flashes"),
        cycle("darkMojangStudiosBackground", "Monochrome Logo"),
    ),
    pair(
        slider("panoramaSpeed", "Panorama Scroll Speed"),
        cycle("hideSplashTexts", "Hide Splash Texts"),
    ),
    pair(
        cycle("narratorHotkey", "Narrator Hotkey"),
        cycle("rotateWithMinecart", "Rotate with Minecarts"),
    ),
    lone(cycle(
        "highContrastBlockOutline",
        "High Contrast Block Outlines",
    )),
];

/// `SkinCustomizationScreen.addOptions` (`:20-31`): the seven
/// `PlayerModelPart`s in declaration order
/// (`world/entity/player/PlayerModelPart.java:8-14`) as `onOffBuilder` cycle
/// buttons, then `mainHand`.
///
/// These seven are the only controls in the tree that are **not**
/// `OptionInstance`s at all — they are built inline from
/// `options.isModelPartEnabled(part)` — so the `accessor` names below are
/// vanilla's `PlayerModelPart` ids rather than `Options.java` methods.
static SKIN: &[Entry] = &[
    pair(
        cycle("modelPart.cape", "Cape"),
        cycle("modelPart.jacket", "Jacket"),
    ),
    pair(
        cycle("modelPart.left_sleeve", "Left Sleeve"),
        cycle("modelPart.right_sleeve", "Right Sleeve"),
    ),
    pair(
        cycle("modelPart.left_pants_leg", "Left Pant Leg"),
        cycle("modelPart.right_pants_leg", "Right Pant Leg"),
    ),
    pair(cycle("modelPart.hat", "Hat"), cycle("mainHand", "Main Hand")),
];

/// The root screen's ten nav buttons, in `OptionsScreen.init`'s own
/// `helper.addChild` order (`:70-95`) — which is what fills the 2×5 grid
/// row-major.
static ROOT_GRID: &[Cell] = &[
    nav("Skin Customization...", SettingsPage::Skin),
    nav("Music & Sounds...", SettingsPage::Sound),
    nav("Video Settings...", SettingsPage::Video),
    nav("Controls...", SettingsPage::Controls),
    no_screen("Language..."),
    nav("Chat Settings...", SettingsPage::Chat),
    no_screen("Resource Packs..."),
    nav("Accessibility Settings...", SettingsPage::Accessibility),
    no_screen("Telemetry Data..."),
    unsupported("Credits & Attribution..."),
];

/// One screen of the options tree.
///
/// Eight of vanilla's thirteen. The five that are **not** here are absent
/// because each needs a *different list widget*, not because their options were
/// skipped — and each is reachable as a present-and-inactive nav button, which
/// is what keeps the parent screen's shape honest:
///
/// | vanilla screen | why not built |
/// |---|---|
/// | `KeyBindsScreen` | `KeyBindsList`, not `OptionsList`: `getRowWidth()` 340, two widgets per row (a 150 px bind button plus a 20 px per-row Reset), and a live key-capture mode. That is #15's own piece of work. |
/// | `LanguageSelectScreen` | a scrolling `ObjectSelectionList` of languages, and this client loads exactly one language table (`resources.rs`). `FontOptionsScreen`'s two options hang off it and are unreachable without it. |
/// | `PackSelectionScreen` | two drag-between `ObjectSelectionList`s over a `PackRepository`. |
/// | `TelemetryInfoScreen` | prose and external links, no options at all. |
/// | `OnlineOptionsScreen` | seven Realms/Xbox controls, none of which has anything behind it here. |
///
/// Building a second and third selection-list type in the same change is where
/// this stops being one mechanism; #396 and #397 are landing that shape
/// concurrently for the server and world lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPage {
    /// `OptionsScreen` — the root, and the only page that is **not** an
    /// `OptionsSubScreen`: a taller header carrying the FOV slider, a 2×5
    /// `GridLayout` of nav buttons, and no list at all.
    Root,
    /// `VideoSettingsScreen` — 31 controls, the most of any screen.
    Video,
    /// `controls/ControlsScreen`.
    Controls,
    /// `MouseSettingsScreen`.
    Mouse,
    /// `SoundOptionsScreen`.
    Sound,
    /// `ChatOptionsScreen`.
    Chat,
    /// `AccessibilityOptionsScreen` — where `bobView` lives in 26.2, and the
    /// one page with two footer buttons.
    Accessibility,
    /// `SkinCustomizationScreen`.
    Skin,
}

impl SettingsPage {
    /// The screen title, verbatim from `en_us.json`.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            SettingsPage::Root => "Options",
            SettingsPage::Video => "Video Settings",
            SettingsPage::Controls => "Controls",
            SettingsPage::Mouse => "Mouse Settings",
            SettingsPage::Sound => "Music & Sound Options",
            SettingsPage::Chat => "Chat Settings",
            SettingsPage::Accessibility => "Accessibility Settings",
            SettingsPage::Skin => "Skin Customization",
        }
    }

    /// The header band's height: 61 on the root (`OptionsScreen.java:37`), the
    /// inherited 33 everywhere else (`OptionsSubScreen.java:19`).
    #[must_use]
    pub fn header_height(self) -> f32 {
        if self == SettingsPage::Root {
            ROOT_HEADER_HEIGHT
        } else {
            SUB_HEADER_HEIGHT
        }
    }

    /// The `OptionsList` entries, or `&[]` for the root, which has no list.
    #[must_use]
    pub fn entries(self) -> &'static [Entry] {
        match self {
            SettingsPage::Root => &[],
            SettingsPage::Video => VIDEO,
            SettingsPage::Controls => CONTROLS,
            SettingsPage::Mouse => MOUSE,
            SettingsPage::Sound => SOUND,
            SettingsPage::Chat => CHAT,
            SettingsPage::Accessibility => ACCESSIBILITY,
            SettingsPage::Skin => SKIN,
        }
    }

    /// The footer buttons, left to right.
    ///
    /// Every page but one is `OptionsSubScreen.addFooter`'s single 200 px Done
    /// (`:51-53`); Accessibility overrides it with a `LinearLayout.horizontal()
    /// .spacing(8)` of two default-width buttons — the external accessibility
    /// guide, then Done (`AccessibilityOptionsScreen.java:77-83`).
    #[must_use]
    pub fn footer(self) -> &'static [Cell] {
        static ONE: &[Cell] = &[done()];
        static TWO: &[Cell] = &[unsupported("Accessibility Guide"), done()];
        if self == SettingsPage::Accessibility {
            TWO
        } else {
            ONE
        }
    }
}

// -- flattening a page into controls ---------------------------------------

/// Where one settings widget sits, for [`Origin::Settings`] to resolve into a
/// rect at draw time.
///
/// A [`Slot`] is built before the canvas is known ([`super::render::frame_for`]
/// takes no size), so every canvas-dependent term has to live behind an
/// [`Origin`]. That is why the scroll position is *in here*: a row's y depends
/// on which entry is at the top of the window, and nothing downstream of
/// `frame_for` knows that either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// One widget of `OptionsScreen`'s own arranged layout, by `visitWidgets`
    /// index: `0` the title `StringWidget`, `1` the FOV slider, `2` the
    /// Online/World Options button, `3..=12` the nav grid row-major, `13` Done.
    Root(u8),
    /// One footer button of an `OptionsSubScreen`, `index` within a block of
    /// `count` (see [`SettingsPage::footer`]).
    Footer {
        /// Position in the block, left to right.
        index: u8,
        /// How many buttons the block holds — 1 or 2.
        count: u8,
    },
    /// A control cell of an `OptionsList`.
    ListCell {
        /// Which page's entry heights to walk.
        page: SettingsPage,
        /// The entry's absolute index in [`SettingsPage::entries`].
        entry: u16,
        /// The entry currently at the top of the visible window.
        first: u16,
        /// `0` or `1` — the `addSmall` column.
        column: u8,
    },
    /// A header entry's `StringWidget`.
    ListHeader {
        /// Which page's entry heights to walk.
        page: SettingsPage,
        /// The entry's absolute index.
        entry: u16,
        /// The entry at the top of the visible window.
        first: u16,
    },
}

/// One flattened control on a page: the cell, where it goes, and how big it is.
///
/// The **order of these is the one index space** the keyboard cursor, the mouse
/// hover, `app.rs`'s hit-test and [`SettingsNav::activate`] all share, exactly
/// as `MAIN_BUTTONS`' order is on the title screen.
/// `the_settings_rows_are_in_the_order_click_assumes` is what stops it drifting
/// from the frame `settings_frame` builds — the guard issue #391 exists for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Control {
    /// The widget.
    pub cell: Cell,
    /// Where it sits.
    pub placement: Placement,
    /// Its width — 310 for `addBig`, 150 for `addSmall`, 200 or 150 in a
    /// footer.
    pub width: f32,
}

impl Control {
    /// The [`Slot`] the renderer and the hit-test share.
    #[must_use]
    pub fn slot(self) -> Slot {
        Slot {
            origin: Origin::Settings(self.placement),
            dx: 0.0,
            dy: 0.0,
            w: self.width,
            h: WIDGET_H,
        }
    }
}

/// Every control on `page`, in `visitWidgets` order, with the list scrolled so
/// entry `first` is at the top of the window.
///
/// List cells first (top to bottom, left to right), then the footer — which is
/// `HeaderAndFooterLayout.visitChildren`'s own order, header then contents then
/// footer (`HeaderAndFooterLayout.java:84-89`).
#[must_use]
pub fn controls(page: SettingsPage, first: usize) -> Vec<Control> {
    let mut out = Vec::new();
    if page == SettingsPage::Root {
        // 1 = the FOV slider, 2 = the Online / World Options button; the title
        // at index 0 is a `StringWidget` and not focusable.
        out.push(Control {
            cell: slider("fov", "FOV"),
            placement: Placement::Root(1),
            width: SMALL_BUTTON_WIDTH,
        });
        out.push(Control {
            cell: no_screen("Online..."),
            placement: Placement::Root(2),
            width: SMALL_BUTTON_WIDTH,
        });
        for (i, cell) in ROOT_GRID.iter().enumerate() {
            out.push(Control {
                cell: *cell,
                placement: Placement::Root(3 + i as u8),
                width: SMALL_BUTTON_WIDTH,
            });
        }
        out.push(Control {
            cell: done(),
            placement: Placement::Root(3 + ROOT_GRID.len() as u8),
            width: DONE_WIDTH,
        });
        return out;
    }

    let entries = page.entries();
    for entry in visible_entries(entries, first) {
        let (a, b, width) = match entries[entry] {
            // A header carries no control; it is drawn as a `MenuLabel` by
            // `settings_frame` instead.
            Entry::Header(_) => continue,
            Entry::Big(cell) => (cell, None, BIG_BUTTON_WIDTH),
            Entry::Small(a, b) => (a, b, SMALL_BUTTON_WIDTH),
        };
        for (column, cell) in [Some(a), b].into_iter().enumerate() {
            let Some(cell) = cell else { continue };
            out.push(Control {
                cell,
                placement: Placement::ListCell {
                    page,
                    entry: entry as u16,
                    first: first as u16,
                    column: column as u8,
                },
                width,
            });
        }
    }
    let footer = page.footer();
    let count = footer.len() as u8;
    for (index, cell) in footer.iter().enumerate() {
        out.push(Control {
            cell: *cell,
            placement: Placement::Footer {
                index: index as u8,
                count,
            },
            width: if count == 1 {
                DONE_WIDTH
            } else {
                SMALL_BUTTON_WIDTH
            },
        });
    }
    out
}

/// Every control on `page`, ignoring the scroll — the census, and what the
/// cursor steps through.
#[must_use]
pub fn all_controls(page: SettingsPage) -> Vec<Cell> {
    if page == SettingsPage::Root {
        let mut out = vec![slider("fov", "FOV"), no_screen("Online...")];
        out.extend_from_slice(ROOT_GRID);
        out.push(done());
        return out;
    }
    let mut out: Vec<Cell> = page
        .entries()
        .iter()
        .flat_map(|e| match e {
            Entry::Header(_) => Vec::new(),
            Entry::Big(a) => vec![*a],
            Entry::Small(a, b) => match b {
                Some(b) => vec![*a, *b],
                None => vec![*a],
            },
        })
        .collect();
    out.extend_from_slice(page.footer());
    out
}

/// Which entry a given control index lives in, so the cursor can scroll it into
/// view. `None` for a footer button (always visible) and for the root.
#[must_use]
pub fn entry_of_control(page: SettingsPage, control: usize) -> Option<usize> {
    if page == SettingsPage::Root {
        return None;
    }
    let mut seen = 0usize;
    for (i, entry) in page.entries().iter().enumerate() {
        let cells = match entry {
            Entry::Header(_) => 0,
            Entry::Big(_) => 1,
            Entry::Small(_, b) => {
                if b.is_some() {
                    2
                } else {
                    1
                }
            }
        };
        if control < seen + cells {
            return Some(i);
        }
        seen += cells;
    }
    None
}

// -- `OptionsList` geometry -------------------------------------------------

/// One entry's height.
///
/// `addEntry(entry)` uses the list's `defaultEntryHeight` of 25
/// (`AbstractSelectionList.java:118-120`, `OptionsList.java:24`); `addHeader`
/// passes `paddingTop + lineHeight + 4` explicitly (`OptionsList.java:59`),
/// where `paddingTop` is `0` for the first entry in the list and `18`
/// otherwise (`:58`). That first-header case is the reason this takes an index
/// rather than being a method on [`Entry`].
#[must_use]
pub fn entry_height(entries: &[Entry], index: usize) -> f32 {
    match entries.get(index) {
        Some(Entry::Header(_)) => {
            header_padding_top(index) + HEADER_LINE_HEIGHT + HEADER_PADDING_BOTTOM
        }
        Some(_) => DEFAULT_ITEM_HEIGHT,
        None => 0.0,
    }
}

/// `OptionsList.addHeader`'s `paddingTop`: `0` when the list is still empty,
/// `lineHeight * 2` otherwise (`OptionsList.java:58`).
///
/// "The list is still empty" is `index == 0`, because `addHeader` is called in
/// entry order and every screen that uses a header opens with one.
#[must_use]
pub fn header_padding_top(index: usize) -> f32 {
    if index == 0 {
        0.0
    } else {
        HEADER_PADDING_TOP
    }
}

/// The y of entry `index`, relative to `getFirstEntryY()`, with entry `first` at
/// the top of the window.
///
/// `repositionEntries` accumulates `child.getHeight()` from
/// `getFirstEntryY() - scrollAmount` (`AbstractSelectionList.java:143-150`);
/// snapping the scroll to an entry boundary makes that sum start at `first`.
#[must_use]
pub fn entry_offset(entries: &[Entry], first: usize, index: usize) -> f32 {
    (first..index).map(|k| entry_height(entries, k)).sum()
}

/// The entries visible with `first` at the top: as many as fit
/// [`LIST_WINDOW_PX`], measured to the **bottom of what each entry draws**
/// rather than to the bottom of the entry box.
///
/// The distinction is 5 px on a control row and it is load-bearing: an entry is
/// 25 px tall but paints a 20 px widget inset 2 px, so the trailing 3 px are
/// blank and excluding a row for them would drop a row that fits.
#[must_use]
pub fn visible_entries(entries: &[Entry], first: usize) -> std::ops::Range<usize> {
    let mut used = 0.0f32;
    let mut end = first;
    while end < entries.len() {
        let drawn = match entries[end] {
            Entry::Header(_) => {
                ENTRY_CONTENT_INSET + header_padding_top(end) + HEADER_LINE_HEIGHT
            }
            _ => ENTRY_CONTENT_INSET + WIDGET_H,
        };
        if used + drawn > LIST_WINDOW_PX {
            break;
        }
        used += entry_height(entries, end);
        end += 1;
    }
    // A window that fits nothing would make the page unreachable; show one.
    if end == first && first < entries.len() {
        end = first + 1;
    }
    first..end
}

/// `OptionsList.Entry.extractContent`'s x for column `column`:
/// `this.screen.width / 2 - 155 + column * 160` (`OptionsList.java:149-155`).
///
/// The division is Java integer division on `screen.width`, hence [`layout::ipx`].
#[must_use]
pub fn row_left(width: f32, column: u8) -> f32 {
    (layout::ipx(width) / 2) as f32 - ROW_LEFT_INSET + f32::from(column) * COLUMN_PITCH
}

/// The top-left of the widget in `entry`, column `column`, on `page`.
///
/// `list.updateSize(width, layout)` puts the list at
/// `(0, layout.getHeaderHeight())` sized `(width, layout.getContentHeight())`
/// (`OptionsSubScreen.java:57-60`, `AbstractSelectionList.java:176-180`), so the
/// list's own `getY()` is the header height and everything below is
/// `getFirstEntryY()` + the entry walk + `getContentY()`'s inset.
#[must_use]
pub fn list_cell_origin(
    page: SettingsPage,
    entry: usize,
    first: usize,
    column: u8,
    width: f32,
) -> (f32, f32) {
    let entries = page.entries();
    let y = page.header_height()
        + LIST_TOP_INSET
        + entry_offset(entries, first, entry)
        + ENTRY_CONTENT_INSET;
    (row_left(width, column), y)
}

/// The top-left of a header entry's `StringWidget`:
/// `(screen.width / 2 - 155, getContentY() + paddingTop)`
/// (`OptionsList.java:196-200`).
#[must_use]
pub fn list_header_origin(
    page: SettingsPage,
    entry: usize,
    first: usize,
    width: f32,
) -> (f32, f32) {
    let (x, y) = list_cell_origin(page, entry, first, 0, width);
    (x, y + header_padding_top(entry))
}

// -- the arranged layouts --------------------------------------------------

/// A zero-width, 9 px `StringWidget` stand-in.
///
/// The real one is `font.width(text)` wide (`StringWidget.java:18-20`), which is
/// not known in a layout pass with no font. Zero is safe for **placement** —
/// every block it sits in is wider than the title in `en_us` ("Options" is
/// ~40 px against a 308 px row), so the title never widens its parent — and the
/// title itself is drawn by [`Align::Centre`] about the block's own centre
/// rather than from this width. `the_root_title_is_centred_on_the_header_block`
/// asserts that equivalence.
fn string_widget() -> Box<dyn LayoutElement> {
    Box::new(Widget::new(0.0, 0.0, 0.0, HEADER_LINE_HEIGHT, ""))
}

fn button(w: f32) -> Box<dyn LayoutElement> {
    Box::new(Widget::button(0.0, 0.0, w, WIDGET_H, ""))
}

/// `OptionsScreen.init` (`:50-99`) as a real [`HeaderAndFooterLayout`],
/// arranged for one canvas.
///
/// Returns `visitWidgets` order: title, FOV, Online/World Options, the ten grid
/// cells row-major, Done — which is the index space [`Placement::Root`] uses.
///
/// **This is `HeaderAndFooterLayout`'s first production consumer.** #394 landed
/// it with arithmetic-only gates and a note saying no screen used it yet; this
/// is that screen, and nothing here re-derives the band arithmetic — the layout
/// is built and asked.
///
/// Built per resolution rather than cached like `render::pause_block`, because
/// unlike the pause grid this tree's *content* position depends on the canvas
/// height (`HeaderAndFooterLayout`'s `min(headerHeight + 30, height - footer -
/// contentHeight)` clamp), so there is no canvas-independent arrangement to
/// cache. It is ~15 small boxes per call on a screen with no world behind it.
#[must_use]
pub fn root_widget_rects(width: f32, height: f32) -> Vec<(f32, f32, f32, f32)> {
    let mut layout_root =
        HeaderAndFooterLayout::with_heights(width, height, ROOT_HEADER_HEIGHT, FOOTER_HEIGHT);

    // `LinearLayout header = layout.addToHeader(LinearLayout.vertical().spacing(8))`
    // with the title centred, then a horizontal sub-row of the FOV slider and
    // the Online (or World Options) button, also spacing 8 (`:52-65`).
    let mut header = LinearLayout::vertical().spacing(ROOT_SPACING);
    header.add_child_settings(
        string_widget(),
        LayoutSettings::defaults().align_horizontally_center(),
    );
    let mut sub_header = LinearLayout::horizontal().spacing(ROOT_SPACING);
    sub_header.add_child(button(SMALL_BUTTON_WIDTH));
    sub_header.add_child(button(SMALL_BUTTON_WIDTH));
    header.add_child(Box::new(sub_header));
    layout_root.add_to_header(Box::new(header));

    // `gridLayout.defaultCellSetting().paddingHorizontal(4).paddingBottom(4)
    // .alignHorizontallyCenter()` then ten `helper.addChild` (`:67-95`).
    let mut grid = layout::GridLayout::new();
    {
        let baseline = grid.default_cell_setting();
        *baseline = baseline
            .padding_horizontal(GRID_PADDING_H)
            .padding_bottom(GRID_PADDING_BOTTOM)
            .align_horizontally_center();
    }
    {
        let mut helper = grid.create_row_helper(GRID_COLUMNS);
        for _ in 0..ROOT_GRID.len() {
            helper.add_child(button(SMALL_BUTTON_WIDTH));
        }
    }
    layout_root.add_to_contents(Box::new(grid));

    layout_root.add_to_footer(button(DONE_WIDTH));
    layout_root.arrange_elements();
    layout::widget_rects(&layout_root)
}

/// An `OptionsSubScreen`'s footer buttons, arranged for one canvas.
///
/// `count == 1` is `OptionsSubScreen.addFooter`'s single 200 px Done (`:51-53`);
/// `count == 2` is `AccessibilityOptionsScreen.addFooter`'s
/// `LinearLayout.horizontal().spacing(8)` of two 150 px buttons (`:77-83`).
/// Both go through the same real `HeaderAndFooterLayout` so the band's own
/// centring is asked for rather than restated.
#[must_use]
pub fn footer_rects(width: f32, height: f32, count: u8) -> Vec<(f32, f32, f32, f32)> {
    let mut layout_root =
        HeaderAndFooterLayout::with_heights(width, height, SUB_HEADER_HEIGHT, FOOTER_HEIGHT);
    if count <= 1 {
        layout_root.add_to_footer(button(DONE_WIDTH));
    } else {
        let mut row = LinearLayout::horizontal().spacing(ROOT_SPACING);
        for _ in 0..count {
            row.add_child(button(SMALL_BUTTON_WIDTH));
        }
        layout_root.add_to_footer(Box::new(row));
    }
    layout_root.arrange_elements();
    layout::widget_rects(&layout_root)
}

/// The top-left of the widget a [`Placement`] names, on a `width`×`height`
/// canvas. [`Origin::Settings`]'s whole body.
#[must_use]
pub fn placement_anchor(placement: Placement, width: f32, height: f32) -> (f32, f32) {
    match placement {
        Placement::Root(index) => {
            let rects = root_widget_rects(width, height);
            let (x, y, _, _) = rects
                .get(usize::from(index))
                .copied()
                // A `Placement::Root` index past the arranged tree is a table
                // that no longer describes the screen. Off-canvas rather than a
                // panic in a draw path, and `the_root_placements_all_resolve`
                // is what fails instead.
                .unwrap_or((-1000.0, -1000.0, 0.0, 0.0));
            (x, y)
        }
        Placement::Footer { index, count } => {
            let rects = footer_rects(width, height, count);
            let (x, y, _, _) = rects
                .get(usize::from(index))
                .copied()
                .unwrap_or((-1000.0, -1000.0, 0.0, 0.0));
            (x, y)
        }
        Placement::ListCell {
            page,
            entry,
            first,
            column,
        } => list_cell_origin(page, usize::from(entry), usize::from(first), column, width),
        Placement::ListHeader { page, entry, first } => {
            list_header_origin(page, usize::from(entry), usize::from(first), width)
        }
    }
}

/// The y of a page title's line, inside its header band.
///
/// The band's `FrameLayout` inherits `align(0.5, 0.5)`
/// (`HeaderAndFooterLayout.java:32-33`), so a 9 px `StringWidget` in a 33 px
/// band sits at `Math.round(lerp(0.5, 0, 33 - 9)) = 12`. Asked of a real
/// arranged layout rather than written down, because
/// `AbstractChildWrapper::setY` **rounds** where `setX` truncates and the
/// asymmetry is worth a pixel (see `docs/menu-layout.md`).
#[must_use]
pub fn title_y(page: SettingsPage) -> f32 {
    // The canvas is irrelevant to a *vertical* band offset, so any size does.
    if page == SettingsPage::Root {
        // The root's title is the first child of its header column, so its y is
        // whatever the arranged tree put there.
        return root_widget_rects(640.0, 480.0)
            .first()
            .map_or(0.0, |&(_, y, _, _)| y);
    }
    let mut layout_root =
        HeaderAndFooterLayout::with_heights(640.0, 480.0, SUB_HEADER_HEIGHT, FOOTER_HEIGHT);
    layout_root.add_to_header(string_widget());
    layout_root.arrange_elements();
    layout::widget_rects(&layout_root)
        .first()
        .map_or(0.0, |&(_, y, _, _)| y)
}

// -- navigation ------------------------------------------------------------

/// What [`SettingsNav`] asks the caller to do after a keypress or a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsOutcome {
    /// Handled internally; nothing for the caller to do.
    None,
    /// Leave the settings tree entirely — the root page's Done, or Escape from
    /// it. [`super::UiState::close_settings`] is what that means.
    Close,
    /// Cycle this live option and persist it. [`super::nav::MenuNav`] owns the
    /// [`crate::config::Options`] and the file, so it does the mutation.
    Cycle(LiveOption),
}

/// The settings tree's own cursor: which page, where the cursor is on it, and
/// how far the list is scrolled.
///
/// One per [`super::nav::MenuNav`]. Escape unwinds [`Self::stack`] rather than
/// consulting a `parent()` on [`SettingsPage`], because the tree is a *graph*
/// and not a tree: Accessibility links to Controls, which the root also links
/// to, so "where did I come from" is history and not structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsNav {
    page: SettingsPage,
    stack: Vec<SettingsPage>,
    cursor: usize,
    first: usize,
}

impl Default for SettingsNav {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsNav {
    /// A fresh cursor at the root page.
    #[must_use]
    pub fn new() -> Self {
        Self {
            page: SettingsPage::Root,
            stack: Vec::new(),
            cursor: 0,
            first: 0,
        }
    }

    /// Back to the root with nothing scrolled — called when the screen is
    /// opened, so re-entering Options does not resume three pages deep.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The page being shown.
    #[must_use]
    pub fn page(&self) -> SettingsPage {
        self.page
    }

    /// The cursor's index into [`all_controls`] for the current page.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The entry at the top of the visible window.
    #[must_use]
    pub fn first(&self) -> usize {
        self.first
    }

    /// The controls actually on screen, with their placements — what
    /// [`settings_frame`] draws and what a row index from `app.rs`'s hit-test
    /// indexes into.
    #[must_use]
    pub fn visible(&self) -> Vec<Control> {
        controls(self.page, self.first)
    }

    /// The cursor's position **within [`Self::visible`]**, i.e. the row index
    /// `MenuFrame::selected` wants. `None` when the cursor is off-window, which
    /// [`Self::scroll_to_cursor`] makes impossible in practice.
    #[must_use]
    pub fn selected_row(&self) -> Option<usize> {
        let all = all_controls(self.page);
        let cell = *all.get(self.cursor)?;
        let entry = entry_of_control(self.page, self.cursor);
        self.visible().iter().position(|c| {
            c.cell == cell
                && match (entry, c.placement) {
                    // A list cell must be the one in *that* entry — the same
                    // cell can legitimately appear on two rows of a page.
                    (Some(e), Placement::ListCell { entry: ce, .. }) => usize::from(ce) == e,
                    (Some(_), _) => false,
                    // A footer or root widget is never a list cell, so a cell
                    // match there is the widget itself.
                    (None, Placement::ListCell { .. }) => false,
                    (None, _) => true,
                }
        })
    }

    /// Moves the cursor by one control, wrapping, and scrolls it into view.
    ///
    /// Steps over **nothing** — see the module docs' departure (4) on why an
    /// inactive row is still a cursor stop here when `step_enabled` skips one
    /// on every other screen.
    pub fn step(&mut self, forward: bool) {
        let len = all_controls(self.page).len();
        if len == 0 {
            return;
        }
        self.cursor = if forward {
            (self.cursor + 1) % len
        } else {
            (self.cursor + len - 1) % len
        };
        self.scroll_to_cursor();
    }

    /// `AbstractSelectionList.scrollToEntry`'s job, at entry granularity: bring
    /// the cursor's entry inside the window, moving the window as little as
    /// possible. Modelled on `super::accounts`' `scroll_to_show`.
    fn scroll_to_cursor(&mut self) {
        let Some(entry) = entry_of_control(self.page, self.cursor) else {
            return;
        };
        let entries = self.page.entries();
        if entry < self.first {
            self.first = entry;
            return;
        }
        while !visible_entries(entries, self.first).contains(&entry) {
            if self.first + 1 >= entries.len() {
                break;
            }
            self.first += 1;
        }
    }

    /// Puts the cursor on the control at visible row `row` — the mouse's half.
    ///
    /// A visible row is resolved back to an index into [`all_controls`] by
    /// matching the *cell* **and** its entry: a cell alone is not unique across a
    /// page in principle, and an index that drifted by one is precisely the #391
    /// failure mode one screen over.
    pub fn hover_row(&mut self, row: usize) {
        let page = self.page;
        let visible = controls(page, self.first);
        let Some(control) = visible.get(row).copied() else {
            return;
        };
        let entry = match control.placement {
            Placement::ListCell { entry, .. } => Some(usize::from(entry)),
            _ => None,
        };
        let all = all_controls(page);
        let found = (0..all.len())
            .find(|&i| all[i] == control.cell && entry_of_control(page, i) == entry);
        if let Some(i) = found {
            self.cursor = i;
        }
    }

    /// Activates the control at visible row `row`.
    ///
    /// This is a click, and it must **not** be routed as "hover then Enter":
    /// issue #391 is exactly that translation on this screen, where the shared
    /// `Enter` meaning was applied to whichever row was clicked. Here a click
    /// resolves the row to its own [`Control`] and acts on that one.
    pub fn click_row(&mut self, row: usize) -> SettingsOutcome {
        let visible = self.visible();
        let Some(control) = visible.get(row).copied() else {
            return SettingsOutcome::None;
        };
        self.hover_row(row);
        self.activate(control.cell)
    }

    /// Activates whatever the cursor is on — Enter's half.
    pub fn enter(&mut self) -> SettingsOutcome {
        let all = all_controls(self.page);
        match all.get(self.cursor).copied() {
            Some(cell) => self.activate(cell),
            None => SettingsOutcome::None,
        }
    }

    /// Escape: unwind one page, or ask to leave the tree from the root.
    ///
    /// `Screen.shouldCloseOnEsc` is true for every options screen, and
    /// `OptionsSubScreen.onClose` returns to `lastScreen`
    /// (`OptionsSubScreen.java:69-75`) — which is the page stack here.
    pub fn escape(&mut self) -> SettingsOutcome {
        self.back()
    }

    fn back(&mut self) -> SettingsOutcome {
        match self.stack.pop() {
            Some(page) => {
                self.page = page;
                self.cursor = 0;
                self.first = 0;
                SettingsOutcome::None
            }
            None => SettingsOutcome::Close,
        }
    }

    /// The one place a control's activation is interpreted.
    ///
    /// An **inactive** control does nothing at all, which is
    /// `AbstractWidget.mouseClicked`'s `isActive()` guard
    /// (`AbstractWidget.java:160-163`) and the same shape as
    /// [`super::nav::MenuNav`]'s `key_main` refusing Enter on a disabled title
    /// button.
    fn activate(&mut self, cell: Cell) -> SettingsOutcome {
        if !cell.is_live() {
            return SettingsOutcome::None;
        }
        match cell {
            Cell::Option(spec) => match spec.live {
                Some(live) => SettingsOutcome::Cycle(live),
                None => SettingsOutcome::None,
            },
            Cell::Nav { page: Some(page), .. } => {
                self.stack.push(self.page);
                self.page = page;
                self.cursor = 0;
                self.first = 0;
                SettingsOutcome::None
            }
            Cell::Nav { page: None, .. } => SettingsOutcome::None,
            Cell::Act {
                act: Action::Done, ..
            } => self.back(),
            Cell::Act {
                act: Action::Unsupported,
                ..
            } => SettingsOutcome::None,
        }
    }
}

// -- the frame ------------------------------------------------------------

/// Vanilla's inactive label grey, for a `StringWidget`-shaped header.
///
/// A header is not a widget with an `active` flag; vanilla draws it in the
/// component's own default white. Read from [`widget::ACTIVE_LABEL`] rather
/// than restated.
const HEADER_COLOUR: [f32; 4] = widget::ACTIVE_LABEL;

/// The save-error line's colour: this shell's failure red, the same
/// `render`-level convention every other menu message uses.
const ERROR_COLOUR: [f32; 4] = [0.92, 0.45, 0.42, 1.0];

/// Builds the whole settings frame for whichever page the cursor is on.
///
/// `in_world` picks vanilla's own fork in the root header: `worldOptions` when a
/// level is loaded, `online` otherwise (`OptionsScreen.java:56-66`). Both are
/// inactive here, so only the label differs — reproduced anyway because a
/// screen that shows the wrong one of two mutually exclusive buttons is the
/// kind of quiet infidelity nothing else would catch.
#[must_use]
pub fn settings_frame(
    nav: &SettingsNav,
    options: &crate::config::Options,
    in_world: bool,
    save_error: Option<&str>,
) -> MenuFrame<'static> {
    let page = nav.page();
    let visible = nav.visible();
    let selected = nav.selected_row();

    let mut rows: Vec<MenuRow> = visible
        .iter()
        .map(|control| {
            let label = match (control.placement, in_world) {
                // The root's second header widget is the Online / World Options
                // fork; every other cell's label is its own.
                (Placement::Root(2), true) => "World Options...".to_string(),
                _ => control.cell.label(options),
            };
            MenuRow {
                label,
                enabled: control.cell.is_live(),
                slider: control.cell.is_slider(),
                slot: Some(control.slot()),
                ..Default::default()
            }
        })
        .collect();
    // A frame with no rows would make `selected` meaningless and the screen
    // unreachable; every page has at least a Done button, so this cannot fire —
    // it is here so that a table edit that empties one is visible instead of
    // silent.
    if rows.is_empty() {
        let fallback = Control {
            cell: done(),
            placement: Placement::Footer { index: 0, count: 1 },
            width: DONE_WIDTH,
        };
        rows.push(MenuRow {
            label: "Done".to_string(),
            enabled: true,
            slot: Some(fallback.slot()),
            ..Default::default()
        });
    }

    let mut labels = vec![MenuLabel {
        text: page.title().to_string(),
        origin: if page == SettingsPage::Root {
            Origin::Settings(Placement::Root(0))
        } else {
            Origin::ScreenTop
        },
        dx: 0.0,
        dy: title_y(page),
        align: Align::Centre,
        colour: HEADER_COLOUR,
        scale: 1.0,
    }];
    // `OptionsList.HeaderEntry`'s `StringWidget`s, for the visible entries only.
    let entries = page.entries();
    for entry in visible_entries(entries, nav.first()) {
        if let Entry::Header(text) = entries[entry] {
            labels.push(MenuLabel {
                text: text.to_string(),
                origin: Origin::Settings(Placement::ListHeader {
                    page,
                    entry: entry as u16,
                    first: nav.first() as u16,
                }),
                dx: 0.0,
                dy: 0.0,
                align: Align::Left,
                colour: HEADER_COLOUR,
                scale: 1.0,
            });
        }
    }
    // A failed `options.json` write, surfaced here rather than swallowed — the
    // same rule the server list follows. `vanilla` frames draw no footer and no
    // `message`, so it goes in as a label.
    if let Some(error) = save_error {
        labels.push(MenuLabel {
            text: error.to_string(),
            origin: Origin::ScreenBottom,
            dx: 0.0,
            dy: -(FOOTER_HEIGHT + HEADER_LINE_HEIGHT + 2.0),
            align: Align::Centre,
            colour: ERROR_COLOUR,
            scale: 1.0,
        });
    }

    MenuFrame {
        title: page.title(),
        subtitle: "",
        rows,
        selected: selected.unwrap_or(usize::MAX),
        vanilla: true,
        labels,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every page, so a sweep cannot silently miss one.
    const PAGES: [SettingsPage; 8] = [
        SettingsPage::Root,
        SettingsPage::Video,
        SettingsPage::Controls,
        SettingsPage::Mouse,
        SettingsPage::Sound,
        SettingsPage::Chat,
        SettingsPage::Accessibility,
        SettingsPage::Skin,
    ];

    #[test]
    fn the_per_screen_control_counts_are_the_censused_ones() {
        // The expected values originate **outside** this file: they are the
        // `addBig`/`addSmall` call-site counts in #55's census comment and
        // `docs/ui-framework.md`, which were counted from the jar. A table edit
        // that drops or duplicates a control fails here and names the screen.
        //
        // Each figure counts focusable widgets including the page's own footer,
        // which is how the census counted the root's Done.
        let expected = [
            (SettingsPage::Root, 13),
            (SettingsPage::Video, 32),
            (SettingsPage::Controls, 10),
            (SettingsPage::Mouse, 8),
            (SettingsPage::Sound, 17),
            (SettingsPage::Chat, 19),
            (SettingsPage::Accessibility, 27),
            (SettingsPage::Skin, 9),
        ];
        for (page, count) in expected {
            assert_eq!(
                all_controls(page).len(),
                count,
                "{page:?} should carry {count} controls"
            );
        }
        // 127 across the eight pages. The census's 141-excluding-keybinds
        // covers five more screens this change does not build (see
        // `SettingsPage`'s table), so this is not expected to reach it.
        let total: usize = PAGES.iter().map(|&p| all_controls(p).len()).sum();
        assert_eq!(total, 135, "13+32+10+8+17+19+27+9");
    }

    #[test]
    fn the_disabled_majority_is_the_point_and_it_is_measured() {
        // The whole issue is that most rows are present-and-inactive. This
        // asserts the *ratio*, so a change that quietly enabled a row it does
        // not honour has to say so here.
        let mut live = Vec::new();
        let mut total = 0;
        for page in PAGES {
            for cell in all_controls(page) {
                total += 1;
                if cell.is_live() {
                    live.push((page, cell));
                }
            }
        }
        assert_eq!(total, 135);
        // Seven *options* are live, in page order (`PAGES`) and then
        // declaration order within each page — the persisted fields of
        // `config::Options` besides `keybinds`.
        let live_options: Vec<LiveOption> = live
            .iter()
            .filter_map(|(_, cell)| match cell {
                Cell::Option(spec) => spec.live,
                _ => None,
            })
            .collect();
        assert_eq!(
            live_options,
            vec![
                LiveOption::GuiScale,
                LiveOption::ToggleSneak,
                LiveOption::ToggleSprint,
                LiveOption::MouseWheelSensitivity,
                LiveOption::InvertMouseX,
                LiveOption::InvertMouseY,
                LiveOption::ViewBobbing,
            ],
            "GUI Scale on Video; Sneak/Sprint toggle on Controls (#202); scroll \
             sensitivity and both inverts on Mouse (#203); View Bobbing on \
             Accessibility — and nothing else"
        );
        // The control: an option we do not persist must report itself inactive,
        // and the detector must be able to tell the difference.
        let render_distance = slider("renderDistance", "Render Distance");
        assert!(
            !render_distance.is_live(),
            "renderDistance is a `Config` (argv) field, not a persisted `Options` one"
        );
        assert!(
            live_cycle("guiScale", "GUI Scale", LiveOption::GuiScale).is_live(),
            "and the same predicate must answer true for one that is"
        );
    }

    #[test]
    fn an_inactive_option_shows_its_caption_and_a_live_one_shows_its_value() {
        let mut options = crate::config::Options::default();
        // `genericValueLabel`'s `"%s: %s"`, with vanilla's own value strings.
        options.gui_scale = crate::config::AUTO_GUI_SCALE;
        let scale = live_cycle("guiScale", "GUI Scale", LiveOption::GuiScale);
        assert_eq!(scale.label(&options), "GUI Scale: Auto");
        options.gui_scale = 3;
        assert_eq!(scale.label(&options), "GUI Scale: 3");
        let bob = live_cycle("bobView", "View Bobbing", LiveOption::ViewBobbing);
        options.view_bobbing = true;
        assert_eq!(bob.label(&options), "View Bobbing: ON");
        options.view_bobbing = false;
        assert_eq!(bob.label(&options), "View Bobbing: OFF");
        // An option we hold no value for shows the caption alone — the module
        // docs' departure (1). The control is the live row above, which does
        // carry a value, so this is not simply "labels never have colons".
        assert_eq!(
            cycle("entityShadows", "Entity Shadows").label(&options),
            "Entity Shadows"
        );
    }

    #[test]
    fn only_slider_backed_options_are_sliders() {
        // `OptionInstance.createButton` dispatches on the `ValueSet`, and
        // `ClampingLazyMaxIntRange.createCycleButton()` is `true`
        // (`OptionInstance.java:213-216`) — so GUI Scale, an int range, is a
        // **cycle** button. Getting this backwards would draw a slider track
        // under the one option on the page that works.
        assert!(!live_cycle("guiScale", "GUI Scale", LiveOption::GuiScale).is_slider());
        assert!(slider("renderDistance", "Render Distance").is_slider());
        assert!(!cycle("enableVsync", "VSync").is_slider());
        // A nav button and a footer button are never sliders.
        assert!(!nav("Controls...", SettingsPage::Controls).is_slider());
        assert!(!done().is_slider());
    }

    #[test]
    fn header_heights_follow_the_first_entry_rule() {
        // `OptionsList.addHeader`: `paddingTop` is `0` when the list is empty
        // and `18` after, and the entry's height is `paddingTop + 9 + 4`
        // (`OptionsList.java:56-60`). Video opens with a header and has two
        // more, so it exercises both branches.
        let entries = SettingsPage::Video.entries();
        assert!(matches!(entries[0], Entry::Header(_)));
        assert_eq!(entry_height(entries, 0), 13.0, "0 + 9 + 4");
        assert!(matches!(entries[6], Entry::Header(_)));
        assert_eq!(entry_height(entries, 6), 31.0, "18 + 9 + 4");
        // Every non-header entry is the list's `itemHeight`.
        assert_eq!(entry_height(entries, 1), 25.0);
        assert_eq!(entry_height(entries, 2), 25.0);
        // The control: if `header_padding_top` ignored the index, the two
        // header heights above would be equal.
        assert_ne!(entry_height(entries, 0), entry_height(entries, 6));
    }

    #[test]
    fn a_list_rows_geometry_is_vanillas_own_arithmetic() {
        // Hand-derived from the jar, not from this file: on a 480-wide canvas
        // `getRowLeft()` is `480 / 2 - 155 = 85` and the second column is
        // `+160`. The first entry's widget is at
        // `headerHeight(33) + getFirstEntryY()'s 2 + getContentY()'s 2 = 37`.
        let page = SettingsPage::Mouse;
        assert_eq!(list_cell_origin(page, 0, 0, 0, 480.0), (85.0, 37.0));
        assert_eq!(list_cell_origin(page, 0, 0, 1, 480.0), (245.0, 37.0));
        // Entry 2 is two 25 px entries down.
        assert_eq!(list_cell_origin(page, 2, 0, 0, 480.0), (85.0, 87.0));
        // Scrolled so entry 2 is at the top, entry 2 lands where entry 0 was.
        assert_eq!(list_cell_origin(page, 2, 2, 0, 480.0), (85.0, 37.0));
        // Java integer division on an odd width: `481 / 2 == 240`, not 240.5.
        assert_eq!(row_left(481.0, 0), 85.0);
        assert_eq!(row_left(480.0, 0), 85.0);
        // A header's `StringWidget` is at `getContentY() + paddingTop`, and the
        // 18 px padding is what separates a mid-list header from its neighbour.
        let video = SettingsPage::Video;
        assert_eq!(list_header_origin(video, 0, 0, 480.0), (85.0, 37.0));
        let (_, quality_y) = list_header_origin(video, 6, 0, 480.0);
        let (_, cell_y) = list_cell_origin(video, 6, 0, 0, 480.0);
        assert_eq!(quality_y - cell_y, 18.0, "the second header's paddingTop");
    }

    #[test]
    fn the_visible_window_never_overruns_the_footer_at_the_smallest_canvas() {
        // The premise this whole windowing scheme rests on: at the *shortest*
        // logical canvas `calculate_gui_scale` can produce, every emitted row
        // must finish above the footer band. If it does not, rows paint over
        // the Done button at some gui_scale on some window — invisible in a
        // screenshot taken at any other size.
        let height = crate::config::MIN_SCALED_HEIGHT as f32;
        let footer_top = height - FOOTER_HEIGHT;
        for page in PAGES {
            let entries = page.entries();
            for first in 0..entries.len().max(1) {
                for entry in visible_entries(entries, first) {
                    let (_, y) = list_cell_origin(page, entry, first, 0, 480.0);
                    let bottom = match entries[entry] {
                        Entry::Header(_) => y + header_padding_top(entry) + HEADER_LINE_HEIGHT,
                        _ => y + WIDGET_H,
                    };
                    assert!(
                        bottom <= footer_top,
                        "{page:?} first={first} entry={entry} ends at {bottom}, footer at {footer_top}"
                    );
                }
            }
        }
        // The control: a window one entry larger must break that. Measured
        // rather than described — `LIST_WINDOW_PX` plus one entry's worth.
        let entries = SettingsPage::Chat.entries();
        let window = visible_entries(entries, 0);
        assert!(window.len() >= 6, "at least six 25 px rows fit 172 px");
        let overrun = list_cell_origin(SettingsPage::Chat, window.end, 0, 0, 480.0).1 + WIDGET_H;
        assert!(
            overrun > footer_top,
            "the first entry the window rejects must be the one that would not fit \
             (it ends at {overrun}, footer at {footer_top})"
        );
    }

    #[test]
    fn scrolling_reaches_every_entry_on_the_longest_page() {
        // Video is 20 entries in a 7-entry window. Stepping the cursor from the
        // top to the bottom must make every entry visible at some point, or
        // rows exist that no player can ever see — the failure mode the module
        // docs' departure (4) is about.
        let page = SettingsPage::Video;
        let mut nav = SettingsNav::new();
        nav.page = page;
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..all_controls(page).len() {
            for entry in visible_entries(page.entries(), nav.first()) {
                seen.insert(entry);
            }
            nav.step(true);
        }
        let all: std::collections::BTreeSet<usize> = (0..page.entries().len()).collect();
        assert_eq!(seen, all, "unreachable entries: {:?}", &all - &seen);
    }

    #[test]
    fn the_settings_rows_are_in_the_order_click_assumes() {
        // Issue #391's guard, re-pointed. `app.rs` reports a **row index** into
        // the frame; `SettingsNav::click_row` indexes `visible()`. If the two
        // disagree, the mouse acts on a different control from the one under
        // it — which is exactly the bug where clicking GUI SCALE toggled View
        // Bobbing and persisted it.
        let options = crate::config::Options::default();
        for page in PAGES {
            let mut nav = SettingsNav::new();
            nav.page = page;
            for first in 0..page.entries().len().max(1) {
                nav.first = first;
                let frame = settings_frame(&nav, &options, false, None);
                let visible = nav.visible();
                assert_eq!(
                    frame.rows.len(),
                    visible.len(),
                    "{page:?} first={first}: the frame and the control list must \
                     have the same length or every index past the difference is wrong"
                );
                for (row, control) in visible.iter().enumerate() {
                    assert_eq!(
                        frame.rows[row].label,
                        control.cell.label(&options),
                        "{page:?} first={first} row {row}"
                    );
                    assert_eq!(frame.rows[row].enabled, control.cell.is_live());
                    assert_eq!(
                        frame.rows[row].slot.map(|s| s.origin),
                        Some(Origin::Settings(control.placement))
                    );
                }
            }
        }
    }

    #[test]
    fn a_click_acts_on_the_row_it_landed_on_and_nothing_else() {
        // The #391 shape directly: find the GUI Scale row and the row next to
        // it, click each, and assert the outcome names the control that was
        // under the cursor.
        let mut nav = SettingsNav::new();
        nav.page = SettingsPage::Video;
        // Scroll so the GUI Scale row is on screen.
        let entry = entry_of_control(
            SettingsPage::Video,
            all_controls(SettingsPage::Video)
                .iter()
                .position(|c| matches!(c, Cell::Option(s) if s.accessor == "guiScale"))
                .expect("Video carries guiScale"),
        )
        .expect("and it is a list cell");
        nav.first = entry;
        let visible = nav.visible();
        let scale_row = visible
            .iter()
            .position(|c| matches!(c.cell, Cell::Option(s) if s.accessor == "guiScale"))
            .expect("visible after scrolling to its entry");
        assert_eq!(
            nav.click_row(scale_row),
            SettingsOutcome::Cycle(LiveOption::GuiScale)
        );
        // Its left-hand neighbour is `inactivityFpsLimit`, which we do not
        // honour: clicking it must do **nothing**, not fall through to whatever
        // Enter last meant. This is the assertion #391 would have failed.
        let neighbour = visible
            .iter()
            .position(|c| matches!(c.cell, Cell::Option(s) if s.accessor == "inactivityFpsLimit"))
            .expect("the same row's first column");
        assert_eq!(nav.click_row(neighbour), SettingsOutcome::None);
        // And a click past the end of the frame must be inert rather than
        // reaching the keyboard path — the other half of #391's fix.
        assert_eq!(nav.click_row(visible.len() + 5), SettingsOutcome::None);
    }

    #[test]
    fn navigation_walks_the_tree_and_escape_unwinds_it() {
        let mut nav = SettingsNav::new();
        assert_eq!(nav.page(), SettingsPage::Root);
        // Root -> Video, through the grid button's own cell.
        let video = all_controls(SettingsPage::Root)
            .iter()
            .position(|c| matches!(c, Cell::Nav { page: Some(SettingsPage::Video), .. }))
            .expect("the root links to Video");
        nav.cursor = video;
        assert_eq!(nav.enter(), SettingsOutcome::None);
        assert_eq!(nav.page(), SettingsPage::Video);
        assert_eq!(nav.cursor(), 0, "a fresh page starts at its first control");
        // Escape returns, and only then asks to close.
        assert_eq!(nav.escape(), SettingsOutcome::None);
        assert_eq!(nav.page(), SettingsPage::Root);
        assert_eq!(nav.escape(), SettingsOutcome::Close);
        // The graph, not a tree: Accessibility -> Controls, and Escape goes
        // back to Accessibility rather than to the root that also links there.
        let mut nav = SettingsNav::new();
        nav.page = SettingsPage::Accessibility;
        let controls_link = all_controls(SettingsPage::Accessibility)
            .iter()
            .position(|c| matches!(c, Cell::Nav { page: Some(SettingsPage::Controls), .. }))
            .expect("Accessibility links to Controls");
        nav.cursor = controls_link;
        nav.enter();
        assert_eq!(nav.page(), SettingsPage::Controls);
        nav.escape();
        assert_eq!(
            nav.page(),
            SettingsPage::Accessibility,
            "the stack is history, not structure"
        );
        // A nav button to a screen we do not build must be inert.
        let mut nav = SettingsNav::new();
        let language = all_controls(SettingsPage::Root)
            .iter()
            .position(|c| matches!(c, Cell::Nav { label: "Language...", page: None }))
            .expect("Language is present and unbuilt");
        nav.cursor = language;
        assert_eq!(nav.enter(), SettingsOutcome::None);
        assert_eq!(nav.page(), SettingsPage::Root, "and must not move");
        // Done on the root closes; Done on a sub-page goes back.
        let mut nav = SettingsNav::new();
        nav.cursor = all_controls(SettingsPage::Root).len() - 1;
        assert_eq!(nav.enter(), SettingsOutcome::Close);
    }

    #[test]
    fn the_cursor_never_leaves_the_visible_window() {
        // `selected_row` is what draws the highlight; a `None` here means the
        // player is moving a cursor they cannot see.
        for page in PAGES {
            let mut nav = SettingsNav::new();
            nav.page = page;
            for _ in 0..all_controls(page).len() * 2 {
                assert!(
                    nav.selected_row().is_some(),
                    "{page:?}: cursor {} off-window at first={}",
                    nav.cursor(),
                    nav.first()
                );
                nav.step(true);
            }
            for _ in 0..all_controls(page).len() * 2 {
                assert!(nav.selected_row().is_some(), "{page:?}: backwards too");
                nav.step(false);
            }
        }
    }

    #[test]
    fn hover_and_the_cursor_agree_on_every_visible_row() {
        // The mouse and the keyboard share one index space (see `Control`). If
        // `hover_row` resolved a row to the wrong control, a click after a
        // hover would act one row off — the shape of both #391 and the
        // `ServerEdit` field bug.
        for page in PAGES {
            let mut nav = SettingsNav::new();
            nav.page = page;
            for first in 0..page.entries().len().max(1) {
                nav.first = first;
                for row in 0..nav.visible().len() {
                    nav.first = first;
                    nav.hover_row(row);
                    assert_eq!(
                        nav.selected_row(),
                        Some(row),
                        "{page:?} first={first}: hovering row {row} must select row {row}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_root_layout_is_the_arranged_header_and_footer_layouts_own() {
        // `HeaderAndFooterLayout`'s first production consumer. The expected
        // values are hand-derived from the Java, outside the code under test:
        //
        //   header: a vertical LinearLayout spacing 8 of a 9 px StringWidget
        //           and a 308 px (150+8+150) row, so 308x37, centred in a
        //           480x61 band -> x = (480-308)/2 = 86, y = round((61-37)/2) = 12.
        //           The pair row is 8+9 = 17 below the title, i.e. y = 29.
        //   grid:   2 columns of (150 + 4 + 4) = 316 wide, 5 rows of (20 + 4)
        //           = 120 tall, centred -> x = (480-316)/2 = 82, and each cell's
        //           child sits 4 px in from its left padding, so column 0 is 86.
        //           content y = min(61 + 30, 320 - 33 - 120) = min(91, 167) = 91.
        //   footer: a 200 px Done centred at x = (480-200)/2 = 140,
        //           y = 320 - 33 + round((33-20)/2) = 287 + 7 = 294.
        let rects = root_widget_rects(480.0, 320.0);
        assert_eq!(rects.len(), 14, "title, FOV, Online, ten grid cells, Done");
        assert_eq!(rects[0], (240.0, 12.0, 0.0, HEADER_LINE_HEIGHT), "title");
        assert_eq!(rects[1], (86.0, 29.0, 150.0, 20.0), "FOV");
        assert_eq!(rects[2], (244.0, 29.0, 150.0, 20.0), "Online");
        assert_eq!(rects[3], (86.0, 91.0, 150.0, 20.0), "grid 0,0");
        assert_eq!(rects[4], (244.0, 91.0, 150.0, 20.0), "grid 0,1");
        assert_eq!(rects[5], (86.0, 115.0, 150.0, 20.0), "grid 1,0 — 24 px pitch");
        assert_eq!(rects[12], (244.0, 187.0, 150.0, 20.0), "grid 4,1");
        assert_eq!(rects[13], (140.0, 294.0, 200.0, 20.0), "Done");
    }

    #[test]
    fn the_root_title_is_centred_on_the_header_block() {
        // `string_widget`'s zero width is a stand-in, and this is the assertion
        // that makes it harmless: the title's arranged x must be the centre of
        // the 308 px header row, so drawing it with `Align::Centre` about that
        // x lands where a real `font.width`-wide `StringWidget` centred in the
        // same column would.
        for width in [320.0f32, 427.0, 480.0, 854.0] {
            let rects = root_widget_rects(width, 320.0);
            let (title_x, _, _, _) = rects[0];
            let (fov_x, _, _, _) = rects[1];
            let (online_x, _, online_w, _) = rects[2];
            // The row spans the FOV button's left edge to the Online button's
            // right edge; its centre is where a real StringWidget would be
            // centred, and an odd canvas width must not make the two disagree.
            assert_eq!(
                title_x,
                (fov_x + (online_x + online_w)) / 2.0,
                "at width {width}"
            );
        }
    }

    #[test]
    fn the_content_band_clamps_upward_so_the_grid_cannot_reach_the_footer() {
        // `HeaderAndFooterLayout`'s `Math.min(headerHeight + 30, height -
        // footer - contentHeight)` — which reads like a maximum until you
        // remember y grows downward. On a short canvas the second term wins.
        let tall = root_widget_rects(480.0, 400.0);
        assert_eq!(tall[3].1, 91.0, "61 + 30 on a canvas with room");
        let short = root_widget_rects(480.0, 240.0);
        assert_eq!(short[3].1, 87.0, "240 - 33 - 120 when there is not");
        // And the last grid row still ends above the footer on the short one.
        assert!(short[12].1 + WIDGET_H <= 240.0 - FOOTER_HEIGHT);
    }

    #[test]
    fn the_footer_block_is_arranged_for_both_shapes() {
        // One 200 px Done, and Accessibility's two 150 px buttons 8 px apart.
        // Both derived from the same real layout; the expected numbers are the
        // hand-derived ones in the root-layout test's comment.
        let one = footer_rects(480.0, 320.0, 1);
        assert_eq!(one, vec![(140.0, 294.0, 200.0, 20.0)]);
        let two = footer_rects(480.0, 320.0, 2);
        assert_eq!(
            two,
            vec![(86.0, 294.0, 150.0, 20.0), (244.0, 294.0, 150.0, 20.0)],
            "308 px block: (480-308)/2 = 86, second at +158"
        );
        // Only Accessibility asks for two.
        for page in PAGES {
            let expected = if page == SettingsPage::Accessibility { 2 } else { 1 };
            assert_eq!(page.footer().len(), expected, "{page:?}");
        }
    }

    #[test]
    fn every_placement_resolves_to_a_rect_on_screen() {
        // The anti-island assertion at this layer: every control of every page,
        // at every scroll position, must land inside the canvas. A `Placement`
        // whose index ran past its arranged tree resolves to the -1000 sentinel
        // in `placement_anchor`, so it fails here rather than drawing nothing
        // and looking like a table that was never wired.
        for page in PAGES {
            for first in 0..page.entries().len().max(1) {
                for control in controls(page, first) {
                    let (x, y) = placement_anchor(control.placement, 480.0, 320.0);
                    assert!(
                        x >= 0.0 && y >= 0.0 && x + control.width <= 480.0 && y + WIDGET_H <= 320.0,
                        "{page:?} first={first} {:?} at ({x}, {y})",
                        control.placement
                    );
                }
            }
        }
    }

    #[test]
    fn the_title_sits_in_its_band_on_every_page() {
        for page in PAGES {
            let y = title_y(page);
            assert_eq!(
                y, 12.0,
                "{page:?}: a 9 px line centred in a 33 px band rounds to 12, and the \
                 root's 61 px band puts its 37 px column at 12 too"
            );
            assert!(y + HEADER_LINE_HEIGHT <= page.header_height());
        }
    }

    #[test]
    fn the_frame_carries_a_header_label_for_every_visible_header() {
        // `OptionsList.HeaderEntry` is a `StringWidget`, not a control, so it
        // reaches the frame as a `MenuLabel`. If it did not, the three Video
        // headers would be missing and the rows below them would still be at
        // their (correct) header-padded positions — a gap with no caption,
        // which reads as a layout bug rather than a missing draw.
        let options = crate::config::Options::default();
        let mut nav = SettingsNav::new();
        nav.page = SettingsPage::Video;
        let frame = settings_frame(&nav, &options, false, None);
        // The page title plus the "Display" header.
        let texts: Vec<&str> = frame.labels.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.contains(&"Video Settings"), "{texts:?}");
        assert!(texts.contains(&"Display"), "{texts:?}");
        // The control: scrolled past it, the header must be gone rather than
        // drawn at a stale position.
        nav.first = 7;
        let frame = settings_frame(&nav, &options, false, None);
        let texts: Vec<&str> = frame.labels.iter().map(|l| l.text.as_str()).collect();
        assert!(!texts.contains(&"Display"), "{texts:?}");
        assert!(texts.contains(&"Video Settings"), "the title stays");
    }

    #[test]
    fn the_root_header_button_follows_vanillas_in_world_fork() {
        // `OptionsScreen.init`'s `if (this.inWorld)` (`:56-66`). Both are
        // inactive, so the label is the only observable difference — which is
        // why it is asserted rather than assumed to be untestable.
        let options = crate::config::Options::default();
        let nav = SettingsNav::new();
        let out = settings_frame(&nav, &options, false, None);
        assert_eq!(out.rows[1].label, "Online...");
        let in_world = settings_frame(&nav, &options, true, None);
        assert_eq!(in_world.rows[1].label, "World Options...");
        assert!(!in_world.rows[1].enabled, "and neither is live");
    }

    #[test]
    fn a_save_failure_reaches_the_frame_rather_than_being_swallowed() {
        let options = crate::config::Options::default();
        let nav = SettingsNav::new();
        let frame = settings_frame(&nav, &options, false, Some("could not save options.json"));
        assert!(
            frame
                .labels
                .iter()
                .any(|l| l.text.contains("could not save")),
            "a `vanilla` frame draws no `message`, so it has to be a label"
        );
        // The control: no error, no label.
        let clean = settings_frame(&nav, &options, false, None);
        assert!(
            !clean
                .labels
                .iter()
                .any(|l| l.text.contains("could not save"))
        );
    }

    #[test]
    fn resetting_returns_to_the_root_from_any_depth() {
        // Re-entering Options must not resume three pages deep, which is what
        // `MenuNav`'s Options arms call this for.
        let mut nav = SettingsNav::new();
        nav.page = SettingsPage::Video;
        nav.stack.push(SettingsPage::Root);
        nav.cursor = 5;
        nav.first = 4;
        nav.reset();
        assert_eq!(nav.page(), SettingsPage::Root);
        assert_eq!((nav.cursor(), nav.first()), (0, 0));
        assert_eq!(nav.escape(), SettingsOutcome::Close, "with an empty stack");
    }
}
