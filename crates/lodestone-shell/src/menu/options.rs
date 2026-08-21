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
//! the census here is a table ([`Entry`]/[`Cell`]) rather than 143 hand-placed
//! widgets, and why adding a screen is adding a `static`.
//!
//! ## `active = false` is the entire disabled path
//!
//! There is no disabled widget type in vanilla and none here — see
//! [`super::widget`]'s module docs. [`Cell::is_live`] is what decides it, and
//! it answers `false` for **100 or 101 of the 143** controls this module
//! renders. The 43-or-42 live ones break down as:
//!
//! - **25 option rows**, driving **22** distinct [`LiveOption`]s — three of them
//!   (`textBackgroundOpacity`, `chatOpacity`, `chatLineSpacing`) are placed on
//!   two pages each, which is vanilla's own shape and why the row count exceeds
//!   the option count. The 22 are `guiScale`/`bobView` (#55),
//!   `toggleCrouch`/`toggleSprint`/`invertMouseX`/`invertMouseY`/
//!   `mouseWheelSensitivity` (#200/#202/#203), the eight chat options
//!   (`9eba2bb`), `sensitivity`/`renderDistance` (#443), and — #444 —
//!   `discreteMouseScroll` plus the four remaining Controls rows
//!   (`toggleAttack`/`toggleUse`/`autoJump`/`sprintWindow`).
//! - **9 `Done` buttons**, one per page, always live.
//! - **13 or 12 working nav buttons** — the swing is the root's Online button:
//!   live outside a world, the inactive World Options placeholder inside one
//!   (see [`online_cell`]), which is the whole of the 43-vs-42 difference.
//!
//! **These numbers are asserted, not maintained by hand** —
//! `the_disabled_majority_is_the_point_and_it_is_measured` and
//! `the_root_online_button_is_the_one_row_that_changes_with_in_world` fail
//! loudly on any change, so a stale count here is a build-time failure rather
//! than a quiet drift. (This paragraph *was* stale for a while, claiming
//! "twenty-four or twenty-five" and "seven real options", which is what the
//! assertions are for.)
//! That ratio is the point of the issue: a greyed row in vanilla's own
//! position makes the gap between this client and vanilla *visible*, where a
//! missing row silently changes the screen's shape.
//!
//! Vanilla disables its own controls for exactly this reason — the narrator
//! button (`OptionsSubScreen.java`), the anisotropy slider
//! (`VideoSettingsScreen.java`), telemetry
//! (`OptionsScreen.java`) — so this is copying an idiom, not inventing
//! one.
//!
//! ## What is deliberately *not* faithful, and why
//!
//! Four departures, each measured rather than guessed:
//!
//! 1. **An inactive option shows its caption alone**, where vanilla shows
//!    `genericValueLabel(caption, value)` — `"%s: %s"`
//!    (`Options.java`). We hold no value for an option we do not
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
//! 3. **The keyboard's scroll-into-view runs against the shortest canvas.** This
//!    departure used to read "the scroll snaps to whole entries and the visible
//!    window is fixed at [`LIST_WINDOW_PX`] … this menu pipeline has no scissor,
//!    so a row that overran the band would paint over the footer", and **both
//!    halves went stale without looking it**. Issue #445 converted this screen to
//!    a continuous pixel offset and gave the pipeline a real CPU scissor
//!    ([`super::render`]'s `Quads::with_clip`), so neither the snapping nor the
//!    fixed window survived — but the prose did, and it was still being cited as
//!    the reason a limitation existed. That is `CLAUDE.md`'s staleness class in
//!    its most expensive form: a *correct-when-written* explanation for a
//!    behaviour that had already been fixed, standing in front of a behaviour
//!    that had **not**.
//!
//!    What is really left is narrower and still real: a keypress has no canvas,
//!    so [`SettingsNav::scroll_to_cursor`] clamps against
//!    `config::MIN_SCALED_HEIGHT` and can therefore ask for an offset a *taller*
//!    canvas has no room for. [`drawn_scroll`] re-clamps where the canvas is
//!    first known — vanilla's own `refreshScrollAmount` — so the rows, the
//!    scrollbar and the clip are three readers of one expression. Read its doc:
//!    it carries the player report that found this, and the two numbers (330 at
//!    the shortest canvas, 90 at 854×480) that made it visible.
//!
//!    The clip itself is [`super::render::Origin::is_scrolling_list_row`], and it
//!    had to be added: `with_clip` reached the three screens whose rows are list
//!    *entries* and not the settings tree, whose rows are slotted widgets.
//! 4. **Up/Down move the cursor over *every* control, including inactive
//!    ones** — where `AbstractWidget.nextFocusPath` skips them
//!    (`AbstractWidget.java`), as [`super::nav`]'s `step_enabled` does
//!    on the title and pause screens. On a screen whose *content* is the
//!    inactive majority, skipping them would leave most rows unreachable
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
//! - [`crate::config`] — the seven options that are real (see [`LiveOption`]).
//!   Pre-existing staleness fixed in passing: this line said "the two options
//!   that are real" since #55, unchanged through #200/#202/#203 adding five
//!   more.

use super::layout::{self, HeaderAndFooterLayout, LayoutSettings, LinearLayout};
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot};
use super::widget::{self, LayoutElement, Widget};

// -- vanilla's metrics ------------------------------------------------------
//
// Every number below is transcribed from `.cache/mc/26.2/client-src`, with the
// file and line named, in logical GUI pixels. Nothing here is measured off our
// own output.

/// `OptionsList.BIG_BUTTON_WIDTH` (`OptionsList.java`) — an `addBig` row, and
/// also the row width `getRowWidth()` returns (`:64-66`).
pub const BIG_BUTTON_WIDTH: f32 = 310.0;
/// The width `OptionInstance.createButton(options)` defaults to
/// (`OptionInstance.java`), i.e. every `addSmall` control.
/// `Button.DEFAULT_WIDTH` (`Button.java`) is the same 150.
pub const SMALL_BUTTON_WIDTH: f32 = widget::DEFAULT_WIDTH;
/// `OptionsList.DEFAULT_ITEM_HEIGHT` (`OptionsList.java`), passed as the
/// list's `itemHeight` (`:24`).
pub const DEFAULT_ITEM_HEIGHT: f32 = 25.0;
/// `OptionsList.Entry.X_OFFSET` (`OptionsList.java`): the pitch between the
/// two columns of an `addSmall` row. Note it is **not** `SMALL_BUTTON_WIDTH`
/// plus a gap that anything else in the file names — 160 is written down.
pub const COLUMN_PITCH: f32 = 160.0;
/// `OptionsList.Entry.extractContent`'s `this.screen.width / 2 - 155`
/// (`OptionsList.java`). Kept as the inset rather than `BIG_BUTTON_WIDTH /
/// 2` because that is how the jar spells it, and because the two would silently
/// stop agreeing if `getRowWidth()` ever changed alone.
pub const ROW_LEFT_INSET: f32 = 155.0;
/// `AbstractSelectionList.getFirstEntryY()`'s `getY() + 2`
/// (`AbstractSelectionList.java`).
pub const LIST_TOP_INSET: f32 = 2.0;
/// `AbstractSelectionList.Entry.getContentY()`'s `getY() + 2` (`:481-483`) —
/// where a row's widget is placed inside its 25 px entry.
pub const ENTRY_CONTENT_INSET: f32 = 2.0;
/// The `int lineHeight = 9` in `OptionsList.addHeader` (`OptionsList.java`),
/// which is also `StringWidget`'s own height (`StringWidget.java`).
pub const HEADER_LINE_HEIGHT: f32 = 9.0;
/// `OptionsList.addHeader`'s `paddingTop` for every header **after** the first:
/// `lineHeight * 2` (`OptionsList.java`). The first header in a list gets
/// `0`, which is the whole reason this is a function of position rather than a
/// constant height.
pub const HEADER_PADDING_TOP: f32 = HEADER_LINE_HEIGHT * 2.0;
/// The `+ 4` in `addHeader`'s `paddingTop + lineHeight + 4` (`:59`).
pub const HEADER_PADDING_BOTTOM: f32 = 4.0;

/// `HeaderAndFooterLayout.DEFAULT_HEADER_AND_FOOTER_HEIGHT` — every
/// `OptionsSubScreen`'s header band, and *every* page's footer band
/// (`OptionsSubScreen.java` takes the 1-argument constructor).
pub const SUB_HEADER_HEIGHT: f32 = layout::DEFAULT_HEADER_AND_FOOTER_HEIGHT;
/// The footer band, on every page including the root.
pub const FOOTER_HEIGHT: f32 = layout::DEFAULT_HEADER_AND_FOOTER_HEIGHT;
/// `new HeaderAndFooterLayout(this, 61, 33)` (`OptionsScreen.java`) — the
/// root screen is the one page with a taller header, because its header carries
/// the FOV slider and the Online button under the title.
pub const ROOT_HEADER_HEIGHT: f32 = 61.0;
/// `LinearLayout.vertical().spacing(8)` and `LinearLayout.horizontal()…
/// spacing(8)` in `OptionsScreen.init` (`:52,55`), and the accessibility
/// footer's `spacing(8)` (`AccessibilityOptionsScreen.java`).
pub const ROOT_SPACING: i32 = 8;
/// `gridLayout.defaultCellSetting().paddingHorizontal(4)`
/// (`OptionsScreen.java`).
pub const GRID_PADDING_H: i32 = 4;
/// `…paddingBottom(4)` on the same line.
pub const GRID_PADDING_BOTTOM: i32 = 4;
/// `OptionsScreen.COLUMNS` (`OptionsScreen.java`).
pub const GRID_COLUMNS: usize = 2;
/// `Button.builder(GUI_DONE, …).width(200)` (`OptionsSubScreen.java`,
/// `OptionsScreen.java`).
pub const DONE_WIDTH: f32 = 200.0;
/// Every menu button's height — `Button.DEFAULT_HEIGHT` (`Button.java`).
pub const WIDGET_H: f32 = widget::DEFAULT_HEIGHT;

/// How many pixels of list a page may show, measured from
/// `getFirstEntryY()`.
///
/// This is the **shortest** content band any `gui_scale` can produce:
/// `calculate_gui_scale` clamps the logical canvas to at least
/// [`crate::config::MIN_SCALED_HEIGHT`] (vanilla's `Window.java`), so a band
/// of `MIN_SCALED_HEIGHT - header - footer` is available at every scale and the
/// window derived from it can never overrun the footer. Deliberately
/// conservative on a tall canvas — see the module docs' departure (3).
pub const LIST_WINDOW_PX: f32 =
    crate::config::MIN_SCALED_HEIGHT as f32 - SUB_HEADER_HEIGHT - FOOTER_HEIGHT - LIST_TOP_INSET;

// -- the option model -------------------------------------------------------

/// Which widget vanilla's `OptionInstance.createButton` builds for an option
/// (`OptionInstance.java`).
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
    /// `options.showSubtitles` → [`crate::config::Options::show_subtitles`]
    /// (issue #198). Gates the sound-subtitle caption overlay; read per presented
    /// frame by `app/redraw.rs`. Vanilla carries the same option on **two** pages
    /// (Sound and Accessibility), and so do both rows here.
    ShowSubtitles,
    /// `key.sneak` → [`crate::config::Options::toggle_sneak`] (issue #202).
    /// Fed to `InputState::set_toggle_modes` every tick.
    ToggleSneak,
    /// `key.sprint` → [`crate::config::Options::toggle_sprint`] (issue #202).
    ToggleSprint,
    /// `key.attack` → [`crate::config::Options::toggle_attack`] (issue #444).
    ToggleAttack,
    /// `key.use` → [`crate::config::Options::toggle_use`] (issue #444).
    ToggleUse,
    /// `options.autoJump` → [`crate::config::Options::auto_jump`] (issue #444).
    /// Fed to `Sim::set_auto_jump`, read by the tick loop's auto-jump gate.
    AutoJump,
    /// `options.sprintWindow` → [`crate::config::Options::sprint_window_ticks`]
    /// (issue #444). An `IntRange(0, 10)`, so it goes through
    /// `SliderRange`/[`slider_fraction`] like `RenderDistance`, not
    /// [`Self::unit_double`]. Fed to `Sim::set_sprint_window_ticks`.
    SprintWindow,
    /// `options.invertMouseX` → [`crate::config::Options::invert_mouse_x`]
    /// (issue #203). Fed to `apply_look_inverted`.
    InvertMouseX,
    /// `options.invertMouseY` → [`crate::config::Options::invert_mouse_y`]
    /// (issue #203).
    InvertMouseY,
    /// `options.discreteMouseScroll` →
    /// [`crate::config::Options::discrete_mouse_scroll`] (issue #444).
    ///
    /// The first row of #444's six, and the one that needed no new subsystem:
    /// `MouseHandler.onScroll` applies it at the input boundary
    /// (`MouseHandler.java`), which is `app/lifecycle.rs` here — so it
    /// affects **both** wheel consumers, the hotbar and every menu list, from one
    /// place. The other four are now live too (`toggleAttack`/`toggleUse`/
    /// `autoJump`/`sprintWindow` — see this enum's variants); only
    /// `allowCursorChanges` and `rawMouseInput` still have no subsystem, so no
    /// row exists for them.
    DiscreteMouseScroll,
    /// `options.mouseWheelSensitivity` →
    /// [`crate::config::Options::mouse_wheel_sensitivity`] (issue #203). Fed
    /// to the hotbar scroll handler.
    MouseWheelSensitivity,
    /// `options.chat.scale` → [`crate::config::Options::chat_scale`].
    ///
    /// This and the seven below were the inverse of this repo's usual island:
    /// the field was persisted, `app.rs` already handed it to
    /// `hud_frame.chat_options`, and `hud.rs` already had a magnitude gate
    /// proving the draw reads it — and the settings row was still drawn
    /// **greyed**, so no player could ever reach any of it. The consumer chain
    /// was complete at both ends with no control in the middle.
    ChatScale,
    /// `options.chat.width` → [`crate::config::Options::chat_width`].
    ChatWidth,
    /// `options.chat.height.focused` →
    /// [`crate::config::Options::chat_height_focused`].
    ChatHeightFocused,
    /// `options.chat.height.unfocused` →
    /// [`crate::config::Options::chat_height_unfocused`].
    ChatHeightUnfocused,
    /// `options.chat.line_spacing` →
    /// [`crate::config::Options::chat_line_spacing`].
    ChatLineSpacing,
    /// `options.chat.opacity` → [`crate::config::Options::chat_opacity`].
    ChatOpacity,
    /// `options.accessibility.text_background_opacity` →
    /// [`crate::config::Options::chat_background_opacity`]. Appears on **two**
    /// pages — Chat and Accessibility — as it does in vanilla; both rows drive
    /// this one field, which is why liveness is a property of the accessor
    /// rather than of the page.
    TextBackgroundOpacity,
    /// `options.chatColors` → [`crate::config::Options::chat_colors`].
    ChatColors,
    /// `options.sensitivity` → [`crate::config::Options::sensitivity`]. A
    /// `UnitDouble` (`Options.java`).
    ///
    /// Live since issue #443 moved it off the argv-only
    /// [`crate::config::Config`]. Before that a row for it would have been
    /// fabricated persistence — the value reverted on restart — which is why
    /// this enum's doc used to name it as explicitly *not* here.
    Sensitivity,
    /// `options.renderDistance` → [`crate::config::Options::render_distance`].
    ///
    /// An `IntRange(2, 32)` (`Options.java`), so unlike every other
    /// live slider its handle position comes from [`SliderRange`] rather than
    /// from the stored value directly — [`LiveOption::unit_double`] answers
    /// `None` for it on purpose.
    RenderDistance,
    /// `options.damageTiltStrength` →
    /// [`crate::config::Options::damage_tilt_strength`]. A `UnitDouble`
    /// defaulting to `1.0`, labelled with `Options::percentValueOrOffLabel`
    /// (`Options.java`'s `damageTiltStrength` field) — so a stored `0.0` prints
    /// **OFF**, not `0%`, unlike every other percent slider here.
    ///
    /// This was the exact inverse of the chat batch above, and worse: the field
    /// was persisted **and** `app/redraw.rs` already fed
    /// `MenuNav::damage_tilt_strength` to `RenderState::set_damage_tilt_strength`
    /// every frame, so the whole camera-tilt consumer was live and honoured — and
    /// the only way to reach it was to hand-edit `options.json`, because the row
    /// drew from [`UNIT_DOUBLE_DEFAULTS`]' frozen `1.0`. Links 1 and 5 present,
    /// links 2–4 missing.
    DamageTiltStrength,
    /// `options.accessibility.panorama_speed` →
    /// [`crate::config::Options::panorama_speed`]. A `UnitDouble` defaulting to
    /// `1.0` with the plain `Options::percentValueLabel` (so `0.0` prints `0%`,
    /// **not** OFF — a stationary panorama is a legitimate value, not an
    /// off state).
    ///
    /// Its consumer was an island in the other direction:
    /// `panorama::PanoramaRenderer::set_speed` existed, was unit-tested, and had
    /// **zero callers**, so the title screen always span at vanilla's default
    /// rate. The value now rides [`super::render::MenuFrame::panorama_speed`]
    /// beside `gui_scale`, for the same reason that one does — a screen must not
    /// have to remember to tell the draw.
    PanoramaSpeed,
    /// One of the eleven `soundSource.*` volume sliders →
    /// [`crate::config::Options::sound_volumes`]`[index]`.
    ///
    /// **One variant with an index rather than eleven variants**, because the
    /// eleven differ in exactly one number: the payload is the index into
    /// [`crate::config::SOUND_CATEGORY_NAMES`], which is *also* the
    /// `SoundSource` ordinal, the `sound_volume_<name>` file key and the mixer
    /// bus. Eleven variants would be eleven chances for the row's accessor and
    /// the array slot to disagree — a **transposed pair**, which is the failure
    /// an eleven-wide array invites and which a uniform default hides
    /// completely. `sound_rows_index_the_category_they_name` is the guard.
    ///
    /// An out-of-range index reads and writes nothing rather than panicking:
    /// the tree's rows are `const`, so a bad index is a build-time authoring
    /// mistake, and a settings screen is the wrong place to abort a session.
    ///
    /// Vanilla builds all eleven from one factory,
    /// `Options::createSoundSliderOptionInstance`, whose stringifier is
    /// `Options::percentValueOrOffLabel` and whose default is `1.0` — so a
    /// muted bus reads **OFF**, not `0%`.
    SoundVolume(u8),
    /// `options.fov` → [`crate::config::Options::fov`].
    ///
    /// An **`IntRange(30, 110)`** defaulting to `70`, so its handle comes from
    /// [`SliderRange`] like [`Self::RenderDistance`]'s and
    /// [`Self::unit_double`] answers `None` for it. The `Codec.DOUBLE.xmap`
    /// between those two lines in `Options.java` is a *persistence* codec on the
    /// seven-argument `OptionInstance` overload, not a `ValueSet::xmap`; reading
    /// it as one puts the value at `70 * 40 + 70`.
    ///
    /// Its consumer (`camera_rig::build_camera` → the projection matrix) was
    /// pinned to the module constant `FOV_Y_DEGREES`, which *is* vanilla's `70`,
    /// and now takes the degrees from here. [`INT_RANGE_SLIDERS`] already
    /// carried the `("fov", 30..=110, 70)` row for the inactive handle draw, so
    /// the live handle and the frozen one are placed by one table.
    Fov,
    /// `options.glintSpeed` → [`crate::config::Options::glint_speed`]. A
    /// `UnitDouble` defaulting to `0.5`, labelled with
    /// `Options::percentValueOrOffLabel` — so a stored `0.0` reads **OFF**, and
    /// a frozen glint is a legitimate choice rather than the option being unset.
    GlintSpeed,
    /// `options.glintStrength` → [`crate::config::Options::glint_strength`]. A
    /// `UnitDouble` defaulting to `0.75`, same `percentValueOrOffLabel`.
    ///
    /// The pair reaches **three** glint sites, not two — the world pass, the
    /// first-person hand, and the 2-D GUI icon pass, which is a separate
    /// pipeline with its own uniform (`crate::hud::item_icon::GuiGlint`) and was
    /// the one missed. All three key off the same wall clock, so pushing the
    /// options to only some of them puts them out of phase as well as at the
    /// wrong rate.
    GlintStrength,
    /// `options.renderClouds` → [`crate::config::Options::cloud_status`].
    ///
    /// Three states, not a boolean: `CloudStatus` is `OFF, FAST, FANCY`
    /// (`CloudStatus.java`) and the cycle visits them in that declaration order,
    /// which is `CycleButton`'s own order.
    ///
    /// **The one live option in the tree whose label is the value alone** — see
    /// [`Self::value_is_the_whole_label`]. Its stringifier is
    /// `(caption, value) -> value.caption()`, which discards the caption it is
    /// handed, so vanilla's button reads "Fancy" rather than "Clouds: Fancy".
    CloudStatus,
    /// `options.framerateLimit` → [`crate::config::Options::framerate_limit`].
    /// An `IntRange(1, 26).xmap(*10)` like [`Self::RenderDistance`], through
    /// [`INT_RANGE_SLIDERS`]'s existing `"framerateLimit"` row — making the
    /// row live does not move the handle a player was already looking at.
    ///
    /// Reaches `app::pacing::effective_target_fps`, folded per-frame with
    /// [`Self::InactivityFpsLimit`]'s AFK clock.
    FramerateLimit,
    /// `options.vsync` → [`crate::config::Options::enable_vsync`]. A plain
    /// boolean (composes with its caption, unlike [`Self::CloudStatus`]).
    /// Reaches `WindowApp::sync_vsync_present_mode`.
    EnableVsync,
    /// `options.inactivityFpsLimit` →
    /// [`crate::config::Options::inactivity_fps_limit`]. Two states,
    /// `Minimized`/`Afk`. **Discards its caption**, like [`Self::CloudStatus`]
    /// — vanilla's stringifier is `(caption, value) -> value.caption()`
    /// (`Options.java`) — so [`Self::value_is_the_whole_label`] covers it
    /// too.
    InactivityFpsLimit,
    /// `options.graphics.preset` → [`crate::config::Options::graphics_preset`].
    /// A `SliderableEnum` over four values (`Fast, Fancy, Fabulous, Custom`),
    /// placed and dragged by index rather than through [`SliderRange`] — see
    /// [`graphics_preset_slider_fraction`]/[`graphics_preset_from_fraction`]
    /// for why this is a third shape alongside [`Self::unit_double`] and
    /// [`Self::int_range`].
    GraphicsPreset,
    /// `options.cutoutLeaves` → [`crate::config::Options::cutout_leaves`]. A
    /// plain boolean; see that field's doc for the render-side consumer.
    CutoutLeaves,
    /// `options.mipmapLevels` → [`crate::config::Options::mipmap_levels`].
    ///
    /// An **`IntRange(0, 4)`** (`Options.java`) like [`Self::RenderDistance`]
    /// and [`Self::Fov`], so its handle comes from [`SliderRange`] and
    /// [`Self::unit_double`] answers `None` for it.
    ///
    /// Its consumer was a real block-atlas island: the shell
    /// always built the atlas at the frozen
    /// `lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS`, and this row moved
    /// a handle nothing read. It now reaches
    /// `crate::resources::set_mipmap_levels` through
    /// `MenuNav::set_live_slider`/`MenuNav::step_mipmap_levels`, which bumps
    /// the same `pack_generation` counter a resource-pack selection change
    /// does — one live-reload path, not two, for both triggers.
    MipmapLevels,
    /// `options.entityShadows` → [`crate::config::Options::entity_shadows`]. A
    /// plain boolean (composes with its caption, unlike
    /// [`Self::CloudStatus`]); see that field's doc for the render-side
    /// consumer — `RenderState::set_entity_shadows_enabled`, which gates
    /// `RenderState::prepare_shadows`.
    EntityShadows,
    /// `options.weatherRadius` → [`crate::config::Options::weather_radius`].
    ///
    /// An **`IntRange(3, 10)`** (`Options.java`) like [`Self::RenderDistance`],
    /// so its handle comes from [`SliderRange`] and [`Self::unit_double`]
    /// answers `None` for it.
    ///
    /// Its consumer was already correct and already parameterised:
    /// `lodestone_render::extract_columns` and
    /// `lodestone_render::column_instance` both take a `radius`, and
    /// `app::weather::weather_columns_for_frame` handed each of them
    /// `lodestone_render::DEFAULT_WEATHER_RADIUS` — a correct function fed a
    /// constant by its producer, not a missing consumer. That function now takes
    /// the radius from here.
    ///
    /// It reaches **both** of those, and that is load-bearing rather than
    /// thorough: `column_instance` fades a column's alpha out toward the radius,
    /// so wiring only the extraction would draw a smaller square of rain still
    /// faded for a 10-block one — visible as an abrupt edge instead of a falloff.
    WeatherRadius,
    /// `options.menuBackgroundBlurriness` →
    /// [`crate::config::Options::menu_background_blurriness`].
    ///
    /// An **`IntRange(0, 10)`** (`Options.java`) like [`Self::RenderDistance`],
    /// so its handle comes from [`SliderRange`] and [`Self::unit_double`]
    /// answers `None` for it.
    ///
    /// Appears on **two** pages — Video and Accessibility — as it does in
    /// vanilla, like [`Self::TextBackgroundOpacity`]; both rows drive this one
    /// field, which is why liveness is a property of the accessor rather than of
    /// the page.
    ///
    /// Its consumer was the frozen `menu::render::blur::BLUR_RADIUS`, whose own
    /// module doc named this row as the wiring it was waiting for and gave the
    /// reason it had not happened — that `config::Options` was outside its
    /// ownership boundary. Both are in `lodestone-shell`.
    ///
    /// **`0` is a real value, not an unset one**: vanilla's stringifier here is
    /// `genericValueOrOffLabel`, so zero reads OFF, and
    /// `Screen.extractBlurredBackground` runs the pass only at `>= 1.0`.
    MenuBackgroundBlurriness,
    /// `options.attackIndicator` →
    /// [`crate::config::Options::attack_indicator`].
    ///
    /// Three states, not a boolean: `AttackIndicatorStatus` is
    /// `OFF, CROSSHAIR, HOTBAR` (`AttackIndicatorStatus.java`) and the cycle
    /// visits them in that declaration order, `CloudStatus`'s shape.
    ///
    /// **Another whole-label option** — its stringifier is
    /// `(caption, value) -> ((AttackIndicatorStatus)value).caption()`
    /// (`Options.java`), which discards the caption exactly as
    /// `cloudStatus`' and `inactivityFpsLimit`' do. See
    /// [`Self::value_is_the_whole_label`].
    ///
    /// The consumer existed and was pinned to `CROSSHAIR`: `hud.rs`'s crosshair
    /// draw site drew the 16x4 strength bar unconditionally, above a comment
    /// saying so and naming the missing toggle. `Off` now hides it and `Hotbar`
    /// moves it to vanilla's 18x18 gauge beside the hotbar, which is a real new
    /// draw rather than a re-anchoring of the same one — the two sprites, the
    /// two sizes and the fill *direction* all differ.
    AttackIndicator,
}

impl LiveOption {
    /// The `[0, 1]` value of this option, for the live options that are built
    /// on `OptionInstance.UnitDouble.INSTANCE`, or `None` for the ones that are
    /// not.
    ///
    /// `UnitDouble.toSliderValue` is the **identity**
    /// (`OptionInstance.java`), so for these options the stored value
    /// *is* the slider fraction and [`Cell::slider_fraction`] can return it
    /// directly — no range to port, which is why this set was reachable
    /// without first closing issue #424.
    #[must_use]
    fn unit_double(self, options: &crate::config::Options) -> Option<f32> {
        match self {
            LiveOption::ChatScale => Some(options.chat_scale),
            LiveOption::ChatWidth => Some(options.chat_width),
            LiveOption::ChatHeightFocused => Some(options.chat_height_focused),
            LiveOption::ChatHeightUnfocused => Some(options.chat_height_unfocused),
            LiveOption::ChatLineSpacing => Some(options.chat_line_spacing),
            LiveOption::ChatOpacity => Some(options.chat_opacity),
            LiveOption::TextBackgroundOpacity => Some(options.chat_background_opacity),
            LiveOption::Sensitivity => Some(options.sensitivity),
            LiveOption::DamageTiltStrength => Some(options.damage_tilt_strength),
            LiveOption::PanoramaSpeed => Some(options.panorama_speed),
            LiveOption::GlintSpeed => Some(options.glint_speed),
            LiveOption::GlintStrength => Some(options.glint_strength),
            // `get` rather than `[index]`: the index comes from a `const` row on
            // some page, so an out-of-range one is an authoring mistake, and
            // reading nothing is a handle that does not draw rather than a panic
            // in the middle of a settings screen.
            LiveOption::SoundVolume(index) => {
                options.sound_volumes.get(index as usize).copied()
            }
            // `RenderDistance` is an `IntRange`, **not** a `UnitDouble`: its
            // stored value is a chunk count, so returning it here would put the
            // handle at `min(8, 1) = 1.0`, pinned to the far end of the track for
            // every value above 1. It goes through `SliderRange` instead.
            LiveOption::RenderDistance
            | LiveOption::GuiScale
            | LiveOption::ViewBobbing
            | LiveOption::ShowSubtitles
            | LiveOption::ToggleSneak
            | LiveOption::ToggleSprint
            | LiveOption::ToggleAttack
            | LiveOption::ToggleUse
            | LiveOption::AutoJump
            | LiveOption::SprintWindow
            | LiveOption::InvertMouseX
            | LiveOption::InvertMouseY
            | LiveOption::DiscreteMouseScroll
            | LiveOption::MouseWheelSensitivity
            | LiveOption::ChatColors
            // `Fov` is the second `IntRange` on this tree, for
            // `RenderDistance`'s reason: its stored value is 30..=110 degrees,
            // so returning it here would pin every handle to the far right.
            | LiveOption::Fov
            // A three-state cycle, not a slider at all.
            | LiveOption::CloudStatus
            // `FramerateLimit` is the third `IntRange`, `RenderDistance`'s
            // reason again.
            | LiveOption::FramerateLimit
            | LiveOption::EnableVsync
            // A two-state cycle, `CloudStatus`'s shape.
            | LiveOption::InactivityFpsLimit
            // A four-value `SliderableEnum`, placed by index — see
            // `graphics_preset_slider_fraction`, not this table.
            | LiveOption::GraphicsPreset
            | LiveOption::CutoutLeaves
            // `MipmapLevels` is the fourth `IntRange`, `RenderDistance`'s
            // reason again: its stored value is 0..=4 mip levels, so
            // returning it here would pin every handle near the far left.
            | LiveOption::MipmapLevels
            | LiveOption::EntityShadows
            // `WeatherRadius` is the fifth `IntRange`, `RenderDistance`'s
            // reason again: its stored value is 3..=10 blocks, so returning it
            // here would pin every handle to the far right.
            | LiveOption::WeatherRadius
            // The sixth `IntRange`, `RenderDistance`'s reason again.
            | LiveOption::MenuBackgroundBlurriness
            // A three-state cycle, not a slider at all — `CloudStatus`'s shape.
            | LiveOption::AttackIndicator => None,
        }
    }

    /// The mutable partner of [`Self::unit_double`] — the write side a slider
    /// **drag** needs (the settings-menu drag work).
    ///
    /// Kept immediately beside its reader on purpose: a slider whose handle is
    /// placed from one field and set on another is the exact class of bug
    /// `Cell::slider_fraction`'s own doc records, and the two matches being
    /// adjacent is what makes a mismatch visible on inspection. **Both arms
    /// must stay exhaustive and must list the same variants**, which the
    /// compiler enforces for the enum but not for the `Some`/`None` split — see
    /// `every_unit_double_option_is_readable_and_writable`.
    pub(super) fn unit_double_mut(
        self,
        options: &mut crate::config::Options,
    ) -> Option<&mut f32> {
        match self {
            LiveOption::ChatScale => Some(&mut options.chat_scale),
            LiveOption::ChatWidth => Some(&mut options.chat_width),
            LiveOption::ChatHeightFocused => Some(&mut options.chat_height_focused),
            LiveOption::ChatHeightUnfocused => Some(&mut options.chat_height_unfocused),
            LiveOption::ChatLineSpacing => Some(&mut options.chat_line_spacing),
            LiveOption::ChatOpacity => Some(&mut options.chat_opacity),
            LiveOption::TextBackgroundOpacity => Some(&mut options.chat_background_opacity),
            LiveOption::Sensitivity => Some(&mut options.sensitivity),
            LiveOption::DamageTiltStrength => Some(&mut options.damage_tilt_strength),
            LiveOption::PanoramaSpeed => Some(&mut options.panorama_speed),
            LiveOption::GlintSpeed => Some(&mut options.glint_speed),
            LiveOption::GlintStrength => Some(&mut options.glint_strength),
            LiveOption::SoundVolume(index) => options.sound_volumes.get_mut(index as usize),
            LiveOption::RenderDistance
            | LiveOption::GuiScale
            | LiveOption::ViewBobbing
            | LiveOption::ShowSubtitles
            | LiveOption::ToggleSneak
            | LiveOption::ToggleSprint
            | LiveOption::ToggleAttack
            | LiveOption::ToggleUse
            | LiveOption::AutoJump
            | LiveOption::SprintWindow
            | LiveOption::InvertMouseX
            | LiveOption::InvertMouseY
            | LiveOption::DiscreteMouseScroll
            | LiveOption::MouseWheelSensitivity
            | LiveOption::ChatColors
            | LiveOption::Fov
            | LiveOption::CloudStatus
            | LiveOption::FramerateLimit
            | LiveOption::EnableVsync
            | LiveOption::InactivityFpsLimit
            | LiveOption::GraphicsPreset
            | LiveOption::CutoutLeaves
            // `MipmapLevels` is the fourth `IntRange`, `RenderDistance`'s
            // reason again: its stored value is 0..=4 mip levels, so
            // returning it here would pin every handle near the far left.
            | LiveOption::MipmapLevels
            | LiveOption::EntityShadows
            // `WeatherRadius` is the fifth `IntRange`, `RenderDistance`'s
            // reason again: its stored value is 3..=10 blocks, so returning it
            // here would pin every handle to the far right.
            | LiveOption::WeatherRadius
            // The sixth `IntRange`, `RenderDistance`'s reason again.
            | LiveOption::MenuBackgroundBlurriness
            // A three-state cycle, not a slider at all — `CloudStatus`'s shape.
            | LiveOption::AttackIndicator => None,
        }
    }

    /// Whether this option's vanilla stringifier **discards the caption** it is
    /// handed, so [`Cell::label`] must not compose one in front of the value.
    ///
    /// True for three options on the tree, and it is not a stylistic
    /// choice: `cloudStatus`' stringifier is `(caption, value) ->
    /// value.caption()` (the `cloudStatus` field in `Options.java`), which throws
    /// its `caption` argument away and returns `CloudStatus.caption()` alone — so
    /// vanilla's Clouds button reads "Fancy", never "Clouds: Fancy". Every other
    /// live option here goes through `genericValueLabel`, `percentValueLabel` or
    /// `pixelValueLabel`, all three of which compose, which is why
    /// [`Cell::label`] composes by default.
    ///
    /// `InactivityFpsLimit`'s stringifier is the identical shape
    /// (`(caption, value) -> value.caption()`, `Options.java`), so it joins
    /// `CloudStatus` here — vanilla's "Reduce FPS when" button reads "AFK" or
    /// "Minimized" alone. `attackIndicator`'s is the same again
    /// (`(caption, value) -> ((AttackIndicatorStatus)value).caption()`), so
    /// vanilla's Attack Indicator button reads "Crosshair", never
    /// "Attack Indicator: Crosshair".
    ///
    /// These are the only three, and the sweep
    /// `every_live_row_carries_both_its_name_and_its_value_or_is_a_named_exception`
    /// asserts the **count** rather than merely tolerating them — a fourth row
    /// falling into this branch has to be justified here first.
    #[must_use]
    fn value_is_the_whole_label(self) -> bool {
        matches!(
            self,
            LiveOption::CloudStatus
                | LiveOption::InactivityFpsLimit
                | LiveOption::AttackIndicator
        )
    }

    /// This option's `IntRange` bounds, for the ones built on one.
    ///
    /// Reads [`INT_RANGE_SLIDERS`] by accessor rather than restating the pair,
    /// so the range a **drag** writes through and the range
    /// [`Cell::slider_fraction`] places the handle with are one table.
    #[must_use]
    pub(super) fn int_range(self) -> Option<SliderRange> {
        let accessor = match self {
            LiveOption::RenderDistance => "renderDistance",
            LiveOption::SprintWindow => "sprintWindow",
            LiveOption::Fov => "fov",
            LiveOption::FramerateLimit => "framerateLimit",
            LiveOption::MipmapLevels => "mipmapLevels",
            LiveOption::WeatherRadius => "weatherRadius",
            LiveOption::MenuBackgroundBlurriness => "menuBackgroundBlurriness",
            _ => return None,
        };
        INT_RANGE_SLIDERS
            .iter()
            .find(|(a, _, _)| *a == accessor)
            .map(|(_, r, _)| *r)
    }
}

/// `ChatComponent.getWidth` (`ChatComponent.java`):
/// `Mth.floor(pct * 280.0 + 40.0)`, i.e. 40px at `0.0` and 320px at `1.0`.
#[must_use]
fn chat_width_px(pct: f32) -> i32 {
    (pct as f64 * 280.0 + 40.0).floor() as i32
}

/// `ChatComponent.getHeight` (`ChatComponent.java`):
/// `Mth.floor(pct * 160.0 + 20.0)`, i.e. 20px at `0.0` and 180px at `1.0`.
#[must_use]
fn chat_height_px(pct: f32) -> i32 {
    (pct as f64 * 160.0 + 20.0).floor() as i32
}

/// The **value half** of `Options.percentValueLabel` (`Options.java`).
///
/// Vanilla's is `translatable("options.percent_value", caption, (int)(value *
/// 100.0))` with the `en_us.json` pattern `"%s: %s%%"`. This client composes
/// captions in exactly one place — [`Cell::label`], via
/// [`generic_value_label`]'s `"%s: %s"` — so returning `"N%"` here reproduces
/// `percentValueLabel`'s full output *by construction*:
/// `generic_value_label(c, "100%") == "c: 100%" == percent_value(c, 1.0)`.
/// Duplicating the caption here instead would be the "fact declared in two
/// places" the module docs' departure (1) exists to avoid.
///
/// The cast is a C-style **truncation**, not a round — `0.999` prints `99%`,
/// which is why the gates predict `floor`ed integers rather than rounded ones.
#[must_use]
fn percent_value(value: f32) -> String {
    format!("{}%", (value as f64 * 100.0) as i32)
}

/// The value half of `Options.pixelValueLabel` (`Options.java`),
/// pattern `"%s: %spx"`. See [`percent_value`] for why the caption is absent.
#[must_use]
fn pixel_value(value: i32) -> String {
    format!("{value}px")
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
    /// (`AccessibilityOptionsScreen.java`) and Credits & Attribution
    /// (`OptionsScreen.java`).
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
    /// `"%s: %s"` (`Options.java`) — when we hold a value for it, and
    /// its **caption alone** when we do not. See the module docs' departure (1)
    /// for why that is not an omission.
    ///
    /// The one exception is an option whose own vanilla stringifier discards the
    /// caption; see [`LiveOption::value_is_the_whole_label`].
    #[must_use]
    pub fn label(self, options: &crate::config::Options) -> String {
        match self {
            Cell::Option(spec) => match spec.live {
                Some(live) if live.value_is_the_whole_label() => live_value(live, options),
                Some(live) => generic_value_label(spec.caption, &live_value(live, options)),
                None => spec.caption.to_string(),
            },
            Cell::Nav { label, .. } | Cell::Act { label, .. } => label.to_string(),
        }
    }

    /// This control's hover tooltip, or `None` — `AbstractWidget.setTooltip`.
    ///
    /// Only an option carries one on this tree, and only 33 of them do; see
    /// [`OPTION_TOOLTIPS`] for the census and for the two vanilla tooltips that land
    /// on rows this tree does not have. A nav or action button gets none, which is
    /// vanilla's shape too — the tooltips vanilla sets on *buttons* in this tree are
    /// all conditional "this is disabled because …" text (`OptionsScreen`'s telemetry
    /// button, `AccessibilityOptionsScreen`'s high-contrast error), and reproducing
    /// them would mean fabricating the condition that triggers them.
    ///
    /// **Independent of [`Self::is_live`] on purpose.** Vanilla's `setTooltip` is not
    /// gated on `active`, and an inactive row is exactly where a player most wants to
    /// know what the option would have done — this tree is the inactive majority by
    /// design.
    #[must_use]
    pub fn tooltip(self) -> Option<&'static str> {
        match self {
            Cell::Option(spec) => option_tooltip(spec.accessor),
            Cell::Nav { .. } | Cell::Act { .. } => None,
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

    /// The `[0, 1]` fraction along the track where
    /// `AbstractSliderButton.extractWidgetRenderState` blits the handle
    /// (`AbstractSliderButton.java`), or `None` for a non-slider `Cell`
    /// **or** a slider this client holds no value for at all.
    ///
    /// Two sources, neither a guess:
    ///
    /// - `mouseWheelSensitivity` (issue #203) is the one live slider on the
    ///   tree, so its fraction comes from the real, persisted config value
    ///   via [`mouse_wheel_slider_fraction`].
    /// - Every other slider is inactive — this client wires no behaviour to
    ///   it — but vanilla's own `OptionInstance` still boots with a concrete
    ///   default double, and for an option built on
    ///   `OptionInstance.UnitDouble.INSTANCE` that default *is* the slider
    ///   fraction, because `UnitDouble.toSliderValue` is the identity
    ///   (`OptionInstance.java`). [`UNIT_DOUBLE_DEFAULTS`] is that
    ///   set, one entry per accessor, each cited to the `Options.java` line
    ///   it boots from.
    ///
    /// A slider whose accessor is not in [`UNIT_DOUBLE_DEFAULTS`] is built on
    /// some other value set — an `IntRange` or an `IntRange.xmap` — whose
    /// range this client has not ported (`renderDistance`,
    /// `menuBackgroundBlurriness`, `chatDelay`, `notificationDisplayTime`, …).
    /// Those return `None` rather than a fabricated position; porting each
    /// range is a bigger job than a handle draw and is tracked separately
    /// (issue #424).
    #[must_use]
    pub fn slider_fraction(self, options: &crate::config::Options) -> Option<f32> {
        let Cell::Option(spec) = self else { return None };
        if spec.widget != OptionWidget::Slider {
            return None;
        }
        if spec.live == Some(LiveOption::MouseWheelSensitivity) {
            return Some(mouse_wheel_slider_fraction(options.mouse_wheel_sensitivity));
        }
        // A live `IntRange` option: the handle comes from the **stored chunk
        // count** run through the range, not from the table's frozen default.
        // Without this arm the row would move the world and leave its own handle
        // parked at 12 — the same lie the chat sliders told before their arm
        // existed.
        if spec.live == Some(LiveOption::RenderDistance) {
            return Some(render_distance_slider_fraction(options.render_distance));
        }
        // `framerateLimit`'s `IntRange(1, 26)` over `fps / 10` — same shape as
        // the arm above, same reason.
        if spec.live == Some(LiveOption::FramerateLimit) {
            return Some(framerate_limit_slider_fraction(options.framerate_limit));
        }
        // `graphicsPreset`'s `SliderableEnum`, placed by index rather than
        // through `SliderRange` — see `graphics_preset_slider_fraction`.
        if spec.live == Some(LiveOption::GraphicsPreset) {
            return Some(graphics_preset_slider_fraction(options.graphics_preset));
        }
        // Same shape, for `sprintWindow`'s `IntRange(0, 10)` (issue #444): the
        // handle must track the live tick count, not the frozen default 7.
        if spec.live == Some(LiveOption::SprintWindow) {
            return Some(sprint_window_slider_fraction(options.sprint_window_ticks));
        }
        // And for `fov`'s `IntRange(30, 110)`. Its row sits on the **root** page
        // rather than in a list, which changes nothing here — liveness is a
        // property of the cell, not of where it is placed.
        if spec.live == Some(LiveOption::Fov) {
            return Some(fov_slider_fraction(options.fov));
        }
        // `mipmapLevels`' `IntRange(0, 4)` — the fourth of the identical
        // quartet (see `render_distance_slider_fraction`,
        // `sprint_window_slider_fraction`, `fov_slider_fraction`). Without
        // this arm the handle would stay parked at the frozen default even
        // after a drag changed `options.mipmap_levels`, the same lie the
        // other three IntRange rows told before their own arm existed.
        if spec.live == Some(LiveOption::MipmapLevels) {
            return Some(mipmap_levels_slider_fraction(options.mipmap_levels));
        }
        // `weatherRadius`' `IntRange(3, 10)` — the fifth of the identical
        // family, same reason as the four above.
        if spec.live == Some(LiveOption::WeatherRadius) {
            return Some(weather_radius_slider_fraction(options.weather_radius));
        }
        // `menuBackgroundBlurriness`' `IntRange(0, 10)` — the sixth of the
        // family, same reason again.
        if spec.live == Some(LiveOption::MenuBackgroundBlurriness) {
            return Some(menu_background_blurriness_slider_fraction(
                options.menu_background_blurriness,
            ));
        }
        // A **live** `UnitDouble` option reads its handle position from the
        // real, persisted value; only an inactive one falls through to the
        // frozen default below. Without this arm the chat sliders would move
        // the chat and leave their own handles parked at vanilla's boot value —
        // a control that silently lies about its state.
        if let Some(live) = spec.live {
            if let Some(value) = live.unit_double(options) {
                return Some(value.clamp(0.0, 1.0));
            }
        }
        if let Some(fraction) = unit_double_default_fraction(spec.accessor) {
            return Some(fraction);
        }
        int_range_default_fraction(spec.accessor)
    }
}

/// Every settings-tree slider built on `OptionInstance.UnitDouble.INSTANCE`,
/// paired with the literal default double each one constructs with — see
/// [`Cell::slider_fraction`]'s doc for why the default *is* the fraction.
///
/// `fovEffectScale`/`darknessEffectScale` additionally `.xmap(Mth::square,
/// Math::sqrt)` (`Options.java`), i.e. `toSliderValue(v) =
/// sqrt(v)`; both default to `1.0`, and `sqrt(1.0) == 1.0`, so the xmap does
/// not change the number recorded here.
///
/// Exhaustive over `grep -n "UnitDouble.INSTANCE" Options.java` — every
/// accessor that string touches is listed, so a slider added later that is
/// *not* here is provably not one of these, rather than merely uncounted.
/// Every `OptionInstance` on this tree that carries a tooltip, keyed by
/// [`OptionSpec::accessor`], with the text verbatim from
/// `assets/minecraft/lang/en_us.json`.
///
/// ## Why a side table and not a field on `OptionSpec`
///
/// Because the tooltip belongs to the **option**, not to the row, and three options
/// are placed on two pages each (`textBackgroundOpacity`, `chatOpacity`,
/// `chatLineSpacing`). A field would have to be repeated per placement and could
/// drift between them; keying by accessor makes "one `OptionInstance`, one tooltip"
/// structural, which is vanilla's own shape. It also keeps 143 table rows untouched,
/// exactly as [`UNIT_DOUBLE_DEFAULTS`] and [`INT_RANGE_SLIDERS`] already do.
///
/// The accessor is a safe key here in a way a field *name* would not be — see
/// `CLAUDE.md` on NBT's `Age` — because these are `Options.java`'s own field names
/// and are unique within that class by construction.
///
/// ## What is in it, and what is deliberately not
///
/// Derived from `grep -n cachedConstantTooltip Options.java` (34 sites) plus
/// `OnlineOptionsScreen`'s two `withTooltip` call sites, resolved through the
/// declaring field name and then through `en_us.json`. Of those, **33 land on rows
/// this tree has**; two do not and are not omissions:
///
/// - `japaneseGlyphVariants` — the row itself is absent from our Video table.
/// - `telemetryOptInExtra` — it lives on `TelemetryInfoScreen`, which is
///   [`super::telemetry`]'s frame, not an `OptionsList` page.
///
/// `narratorHotkey`'s text is the **non-Mac** variant. Vanilla forks on
/// `InputQuirks.REPLACE_CTRL_KEY_WITH_CMD_KEY` between
/// `options.accessibility.narrator_hotkey.tooltip` ("Ctrl + B") and its `.mac`
/// sibling ("Cmd + B"); this client has no such quirk table, and the row is inactive
/// anyway, so naming the platform fork here is more honest than guessing the host.
const OPTION_TOOLTIPS: &[(&str, &str)] = &[
    ("allowCursorChanges", "Allows the mouse cursor to change shape when over certain UI elements."),
    ("allowFriendRequests", "Allow other players to send you friend requests"),
    ("allowServerListing", "Servers may list online players as part of their public status.\nWith this option off, your name will not show up in such lists."),
    ("chunkSectionFadeInTime", "How long in seconds chunks should fade in when they're first rendered, if at all."),
    ("cutoutLeaves", "Allows you to see through gaps in leaves. Disabling improves performance."),
    ("damageTiltStrength", "The amount of camera shake caused by being hurt."),
    ("darkMojangStudiosBackground", "Changes the Mojang Studios loading screen background color to black."),
    ("darknessEffectScale", "Controls how much the Darkness effect pulses when a Warden or Sculk Shrieker gives it to you."),
    ("fovEffectScale", "Controls how much the field of view can change with gameplay effects."),
    ("glintSpeed", "Controls how fast the visual glint shimmers across enchanted items."),
    ("glintStrength", "Controls how transparent the visual glint is on enchanted items."),
    ("graphicsPreset", "Sets \"Quality & Performance\" settings to reasonable defaults corresponding to the desired quality."),
    ("hideLightningFlash", "Prevents Lightning Bolts or other environmental effects from making the sky flash. The sources of flashes themselves will still be visible."),
    ("hideMatchedNames", "3rd-party Servers may send chat messages in non-standard formats.\nWith this option on, hidden players will be matched based on chat sender names."),
    ("hideSplashTexts", "Hides the yellow splash text in the main menu."),
    ("highContrast", "Enhances the contrast of UI elements."),
    ("highContrastBlockOutline", "Enhances the block outline contrast of the targeted block."),
    ("improvedTransparency", "An experimental approach that uses screen shaders for drawing weather, clouds, and particles behind translucent blocks and water.\nThis will impact GPU performance."),
    ("inGameNotification", "Show Friend notifications in-game"),
    ("maxAnisotropyBit", "Each level significantly improves how smooth textures look, but impacts performance and significantly impacts video memory usage. Requires Texture Filtering to be set to Anisotropic."),
    ("menuBackgroundBlurriness", "Changes the blurriness of menu backgrounds."),
    ("musicFrequency", "Changes how frequently music plays while in a game world."),
    ("narratorHotkey", "Allows the Narrator to be toggled on and off with 'Ctrl + B'."),
    ("notificationDisplayTime", "Affects the length of time that all notifications stay visible on the screen."),
    ("onlyShowSecureChat", "Only display messages from other players that can be verified to have been sent by that player, and have not been modified."),
    ("realmsNotifications", "Fetches Realms news and invites in the title screen and displays their respective icon on the Realms button."),
    ("rotateWithMinecart", "Whether the player's view should rotate with a turning Minecart. Only available in worlds with the 'Minecart Improvements' experimental setting turned on."),
    ("saveChatDrafts", "Unsent messages will be saved and can be sent the next time chat is opened."),
    ("screenEffectScale", "Strength of Nausea and Nether Portal screen distortion effects.\nAt lower values, the Nausea effect is replaced with a green overlay."),
    ("showSubtitles", "Enables captions for sounds played in the game."),
    ("sprintWindow", "Time window in ticks where double-tapping the forward key activates sprint."),
    ("vignette", "This is a subtle texture over the game screen used for reducing brightness towards the edges of the screen and warning about the world border."),
    ("weatherRadius", "Radius of the area where rain and snow effects are visible. Very low performance impact."),
];

/// `Tooltip.MAX_WIDTH`: the pixel width `Tooltip.splitTooltip` wraps to.
pub const TOOLTIP_MAX_WIDTH: f32 = 170.0;

/// Every accessor [`OPTION_TOOLTIPS`] holds text for.
///
/// Exists for the census's *other* direction: a key here with no row on any page is
/// a tooltip that can never show, and no assertion derived from the rows can see it —
/// the reachable set would simply be one smaller and still self-consistent.
#[must_use]
pub fn tooltip_accessors() -> Vec<&'static str> {
    OPTION_TOOLTIPS.iter().map(|&(key, _)| key).collect()
}

/// The tooltip text for `accessor`, or `None` — see [`OPTION_TOOLTIPS`].
#[must_use]
pub fn option_tooltip(accessor: &str) -> Option<&'static str> {
    OPTION_TOOLTIPS
        .iter()
        .find(|(key, _)| *key == accessor)
        .map(|&(_, text)| text)
}

const UNIT_DOUBLE_DEFAULTS: &[(&str, f32)] = &[
    // `Options.java`, `createSoundSliderOptionInstance`'s fifth
    // argument — shared by all eleven `SoundSource` categories.
    ("soundSource.master", 1.0),
    ("soundSource.music", 1.0),
    ("soundSource.record", 1.0),
    ("soundSource.weather", 1.0),
    ("soundSource.block", 1.0),
    ("soundSource.hostile", 1.0),
    ("soundSource.neutral", 1.0),
    ("soundSource.player", 1.0),
    ("soundSource.ambient", 1.0),
    ("soundSource.voice", 1.0),
    ("soundSource.ui", 1.0),
    // `Options.java` — look sensitivity, distinct from the live
    // `mouseWheelSensitivity` below.
    ("sensitivity", 0.5),
    // `Options.java`.
    ("chatOpacity", 1.0),
    // `Options.java`.
    ("chatLineSpacing", 0.0),
    // `Options.java`.
    ("textBackgroundOpacity", 0.5),
    // `Options.java`.
    ("panoramaSpeed", 1.0),
    // `Options.java`.
    ("chatScale", 1.0),
    // `Options.java`.
    ("chatWidth", 1.0),
    // `Options.java`, default `ChatComponent.defaultUnfocusedPct()`
    // = `70.0 / (getHeight(1.0) - 20)` = `70.0 / 160.0`
    // (`ChatComponent.java`).
    ("chatHeightUnfocused", 70.0 / 160.0),
    // `Options.java`.
    ("chatHeightFocused", 1.0),
    // `Options.java`.
    ("screenEffectScale", 1.0),
    // `Options.java`, `sqrt(1.0)`.
    ("fovEffectScale", 1.0),
    // `Options.java`, `sqrt(1.0)`.
    ("darknessEffectScale", 1.0),
    // `Options.java`.
    ("glintSpeed", 0.5),
    // `Options.java`.
    ("glintStrength", 0.75),
    // `Options.java`.
    ("damageTiltStrength", 1.0),
    // `Options.java`.
    ("gamma", 0.5),
];

/// Looks up [`UNIT_DOUBLE_DEFAULTS`] by accessor. A linear scan over ~20
/// entries, once per visible slider per frame — cheaper than the allocation
/// a `HashMap` would cost for a table this size.
#[must_use]
fn unit_double_default_fraction(accessor: &str) -> Option<f32> {
    UNIT_DOUBLE_DEFAULTS
        .iter()
        .find(|(a, _)| *a == accessor)
        .map(|(_, v)| *v)
}

/// One vanilla `OptionInstance.IntRange`'s bounds — the `(minInclusive,
/// maxInclusive)` pair a slider needs before it can place a handle at all.
///
/// `IntRange` is a record of exactly those two ints plus an
/// `applyValueImmediately` flag (`OptionInstance.java`); the flag
/// changes *when* vanilla commits a drag, never where the handle draws, so it
/// is deliberately absent here.
///
/// The arithmetic lives in the `IntRangeBase` interface `IntRange` implements,
/// and [`Self::to_slider_value`] is that method transcribed — not a
/// re-derivation. See [`INT_RANGE_SLIDERS`] for the per-accessor bounds and
/// why each one is a citation rather than a plausible number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SliderRange {
    /// `IntRange.minInclusive` (`OptionInstance.java`).
    pub min: i32,
    /// `IntRange.maxInclusive` (`:267`).
    pub max: i32,
}

impl SliderRange {
    /// `IntRangeBase.toSliderValue` (`OptionInstance.java`), verbatim:
    ///
    /// ```java
    /// default double toSliderValue(final Integer value) {
    ///    if (value == this.minInclusive()) {
    ///       return 0.0;
    ///    } else {
    ///       return value == this.maxInclusive() ? 1.0
    ///          : Mth.map(value.intValue() + 0.5, this.minInclusive(),
    ///                    this.maxInclusive() + 1.0, 0.0, 1.0);
    ///    }
    /// }
    /// ```
    ///
    /// **The two endpoint special cases are load-bearing and are not an
    /// optimisation.** Without them the general `Mth.map` puts the *maximum*
    /// at `(max + 0.5 - min) / (max + 1 - min)`, which is short of 1.0 by half
    /// a step — `mipmapLevels`' max would sit at `0.9`, a handle visibly inside
    /// the track on an option whose shipped default *is* the max.
    ///
    /// **The `+ 0.5` and the `max + 1` are equally load-bearing.** They are
    /// there because an `IntRange` slider is a *bucket* selector, not a point
    /// selector: `fromSliderValue` is `floor(map(slider, 0, 1, min, max + 1))`
    /// (`:303-309`), so the handle marks the centre of the value's bucket. The
    /// naive `(value - min) / (max - min)` a hand-rolled version produces is a
    /// different function, and
    /// `the_naive_endpoint_span_hypothesis_is_measurably_wrong` requires the
    /// measurement to land off it.
    #[must_use]
    pub fn to_slider_value(self, value: i32) -> f32 {
        if value == self.min {
            return 0.0;
        }
        if value == self.max {
            return 1.0;
        }
        // `Mth.map(v, from_lo, from_hi, to_lo, to_hi)` with `to` = `0..1`
        // reduces to `(v - from_lo) / (from_hi - from_lo)`.
        let v = f64::from(value) + 0.5;
        let lo = f64::from(self.min);
        let hi = f64::from(self.max) + 1.0;
        (((v - lo) / (hi - lo)) as f32).clamp(0.0, 1.0)
    }

    /// `IntRangeBase.fromSliderValue` (`OptionInstance.java`), the
    /// inverse a slider **drag** needs:
    ///
    /// ```java
    /// default Integer fromSliderValue(final double value) {
    ///    return Mth.floor(Mth.map(value, 0.0, 1.0, this.minInclusive(),
    ///                             this.maxInclusive() + 1.0));
    /// }
    /// ```
    ///
    /// **The `max + 1` and the `floor` are the bucket model, not a fencepost
    /// slip** — see [`Self::to_slider_value`]'s doc: an `IntRange` slider selects
    /// a bucket, so a fraction of exactly `1.0` maps to `max + 1` before the
    /// floor and has to be clamped back. Without the clamp the top of the track
    /// would select a value one past the maximum, which for `renderDistance` is
    /// 33 chunks and for `sprintWindow` is 11 ticks.
    ///
    /// Round-trips with [`Self::to_slider_value`] for every value in range,
    /// which is the property `slider_values_round_trip_through_the_bucket_map`
    /// asserts — note that is *not* a `decode(encode(x))` tautology here,
    /// because both directions are transcribed from the jar independently and
    /// the endpoint special cases only exist in one of them.
    #[must_use]
    pub fn from_slider_value(self, fraction: f32) -> i32 {
        let f = f64::from(fraction.clamp(0.0, 1.0));
        let lo = f64::from(self.min);
        let hi = f64::from(self.max) + 1.0;
        let mapped = lo + f * (hi - lo);
        (mapped.floor() as i32).clamp(self.min, self.max)
    }
}

/// This client's `largeDistances`, the one bound in [`INT_RANGE_SLIDERS`] that
/// vanilla decides at runtime rather than in a literal.
///
/// `Options`' constructor reads
/// `Runtime.getRuntime().maxMemory() >= 1000000000L` **once** and uses it for
/// both distance sliders' maximum (`Options.java`):
///
/// ```java
/// boolean largeDistances = Runtime.getRuntime().maxMemory() >= 1000000000L;
/// … new OptionInstance.IntRange(2, largeDistances ? 32 : 16, false), 12, …
/// ```
///
/// That test is a question about **the JVM's `-Xmx` heap cap**, not about the
/// machine: it is `false` on a 64 GB box launched with `-Xmx512m`. This client
/// has no heap cap to interrogate — there is no JVM and no equivalent ceiling —
/// so the honest mapping is the branch a real launcher takes, which allocates
/// far above 1 GB by default. Hence `32`, recorded as a named constant so the
/// **decision** is visible rather than buried as a literal beside fifteen
/// citations. It is not itself a citation, and must not be relabelled as one.
pub const LARGE_DISTANCES_MAX: i32 = 32;

/// Every settings-tree slider built on an `OptionInstance.IntRange` — the value
/// set whose absence was issue #424 — paired with its bounds and the **integer
/// pre-image** of vanilla's shipped default.
///
/// Three columns, and the third is the subtle one. An `IntRange` slider may be
/// `.xmap`'d to a non-integer displayed value (`OptionInstance.java`),
/// and `xmap`'s `toSliderValue` calls `from.applyAsInt(value)` *first* and then
/// defers to the underlying `IntRangeBase` — so the fraction is always a
/// function of the **int**, never of the displayed double. Each `.xmap`'d row
/// below therefore records `from(default)` with the conversion spelled out, not
/// the default a player sees.
///
/// Every entry names the `Options.java` line its bounds and default are read
/// from. Exhaustive over the settings tree's `slider(...)` rows: an accessor
/// this client renders as a slider and that appears in neither this table nor
/// [`UNIT_DOUBLE_DEFAULTS`] is one of the two documented non-`IntRange`
/// leftovers — see [`int_range_default_fraction`].
const INT_RANGE_SLIDERS: &[(&str, SliderRange, i32)] = &[
    // `Options.java`: `IntRange(1, 26).xmap(v -> v * 10, v -> v / 10,
    // true)`, default `120`. Pre-image `120 / 10 = 12`.
    ("framerateLimit", SliderRange { min: 1, max: 26 }, 12),
    // `Options.java`: `IntRange(2, 20).xmap(v -> v / 4.0,
    // v -> (int)(v * 4.0), true)`, default `1.0`. Pre-image
    // `(int)(1.0 * 4.0) = 4`.
    ("entityDistanceScaling", SliderRange { min: 2, max: 20 }, 4),
    // `Options.java`: `IntRange(2, 128, true)`, default `128` — the
    // maximum, so the endpoint case pins it to exactly 1.0.
    ("cloudRange", SliderRange { min: 2, max: 128 }, 128),
    // `Options.java`: `IntRange(3, 10, true)`, default `10`.
    ("weatherRadius", SliderRange { min: 3, max: 10 }, 10),
    // `Options.java`: `IntRange(0, 40).xmap(v -> v / 20.0,
    // v -> (int)(v * 20.0), true)`, default `0.75`. Pre-image
    // `(int)(0.75 * 20.0) = 15`.
    (
        "chunkSectionFadeInTime",
        SliderRange { min: 0, max: 40 },
        15,
    ),
    // `Options.java`: `IntRange(0, 10)`, default `5`
    // (`BLURRINESS_DEFAULT_VALUE`).
    (
        "menuBackgroundBlurriness",
        SliderRange { min: 0, max: 10 },
        5,
    ),
    // `Options.java`: `IntRange(0, 60).xmap(v -> v / 10.0,
    // v -> (int)(v * 10.0), true)`, default `0.0`. Pre-image `0` — the
    // minimum, so the endpoint case pins it to exactly 0.0.
    ("chatDelay", SliderRange { min: 0, max: 60 }, 0),
    // `Options.java`: `IntRange(5, 100).xmap(v -> v / 10.0,
    // v -> (int)(v * 10.0), true)`, default `1.0`. Pre-image
    // `(int)(1.0 * 10.0) = 10`.
    (
        "notificationDisplayTime",
        SliderRange { min: 5, max: 100 },
        10,
    ),
    // `Options.java`: `IntRange(0, 4)`, default `4`.
    ("mipmapLevels", SliderRange { min: 0, max: 4 }, 4),
    // `Options.java`: `IntRange(1, 3)`, default `2`. The value is an
    // anisotropy *bit*, i.e. the displayed level is `1 << bit` — an exponent,
    // not the level, which does not change the fraction because the slider
    // maps the bit.
    ("maxAnisotropyBit", SliderRange { min: 1, max: 3 }, 2),
    // `Options.java`: `IntRange(0, 7, false)`, default `2`.
    ("biomeBlendRadius", SliderRange { min: 0, max: 7 }, 2),
    // `Options.java`: `IntRange(0, 10)`, default `7`.
    ("sprintWindow", SliderRange { min: 0, max: 10 }, 7),
    // `Options.java`: `IntRange(30, 110)`, default `70`. The
    // `Codec.DOUBLE.xmap` on the line between them is a **persistence** codec
    // (the 7-arg `OptionInstance` overload, `OptionInstance.java`), not
    // a `ValueSet::xmap`, so it does not touch the slider at all — reading it
    // as one would put the handle at `(int)(70 * 40 + 70)`, far off the track.
    ("fov", SliderRange { min: 30, max: 110 }, 70),
    // `Options.java`: `IntRange(2, largeDistances ? 32 : 16,
    // false)`, default `12`. See [`LARGE_DISTANCES_MAX`] for the max.
    (
        "renderDistance",
        SliderRange {
            min: 2,
            max: LARGE_DISTANCES_MAX,
        },
        12,
    ),
    // `Options.java`: `IntRange(DEBUG_ALLOW_LOW_SIM_DISTANCE ? 2 : 5,
    // largeDistances ? 32 : 16, false)`, default `12`. The min is `5`: the `2`
    // is behind `SharedConstants.DEBUG_ALLOW_LOW_SIM_DISTANCE`, a dev flag off
    // in a shipped client, and taking the debug branch would shift every
    // handle on this row.
    (
        "simulationDistance",
        SliderRange {
            min: 5,
            max: LARGE_DISTANCES_MAX,
        },
        12,
    ),
];

/// `graphicsPreset`'s fraction. Its value set is an
/// `OptionInstance.SliderableEnum`, not an `IntRange`, so it has its own
/// `toSliderValue` (`OptionInstance.java`):
///
/// ```java
/// if (value == this.values.getFirst()) { return 0.0; }
/// else { return value == this.values.getLast() ? 1.0
///        : Mth.map(this.values.indexOf(value), 0.0, this.values.size() - 1, 0.0, 1.0); }
/// ```
///
/// Note the divisor is `size - 1`, **not** `size` — this family spaces its
/// values at the track's two ends rather than at bucket centres, which is why
/// it cannot borrow [`SliderRange`]. `GraphicsPreset` is
/// `FAST, FANCY, FABULOUS, CUSTOM` (`GraphicsPreset.java`), so `size` is
/// 4, and the default is `FANCY` (`Options.java`) at index 1 — one third
/// along.
#[must_use]
fn graphics_preset_default_fraction() -> f32 {
    const COUNT: f32 = 4.0;
    const FANCY_INDEX: f32 = 1.0;
    FANCY_INDEX / (COUNT - 1.0)
}

/// Looks up [`INT_RANGE_SLIDERS`] by accessor and maps its default through
/// [`SliderRange::to_slider_value`], plus the one `SliderableEnum` row.
///
/// Returns `None` for an accessor in neither table. Two settings-tree sliders
/// land there **deliberately**, and both are absences with a reason rather than
/// gaps to be filled with a plausible number:
///
/// - `fullscreenResolution` is not slider-shaped in vanilla at all. Its value
///   set is a lazily-populated list of the monitor's real video modes, so its
///   "range" is a property of the display and there is no default int to place
///   a handle at. This client renders it as a slider row; the missing handle is
///   the honest answer until the row itself is reclassified.
/// - `gamma` is in [`UNIT_DOUBLE_DEFAULTS`], reached before this function.
#[must_use]
fn int_range_default_fraction(accessor: &str) -> Option<f32> {
    if accessor == "graphicsPreset" {
        return Some(graphics_preset_default_fraction());
    }
    INT_RANGE_SLIDERS
        .iter()
        .find(|(a, _, _)| *a == accessor)
        .map(|(_, range, default)| range.to_slider_value(*default))
}

/// `renderDistance`'s slider fraction from the real, persisted chunk count.
///
/// Reuses the same [`INT_RANGE_SLIDERS`] row the inactive version used, so the
/// live handle and the frozen-default handle are placed by one expression and
/// cannot drift. Falls back to the range's own minimum if the table row ever
/// goes missing, which is a visible handle at the left rather than none at all.
#[must_use]
pub fn render_distance_slider_fraction(chunks: u32) -> f32 {
    let range = INT_RANGE_SLIDERS
        .iter()
        .find(|(a, _, _)| *a == "renderDistance")
        .map_or(
            SliderRange {
                min: crate::config::MIN_RENDER_DISTANCE as i32,
                max: crate::config::MAX_RENDER_DISTANCE as i32,
            },
            |(_, r, _)| *r,
        );
    range.to_slider_value(i32::try_from(chunks).unwrap_or(range.min))
}

/// `sprintWindow`'s slider fraction from the real, persisted tick count.
///
/// Reuses the same [`INT_RANGE_SLIDERS`] row the inactive version used, so the
/// live handle and the frozen-default handle are placed by one expression and
/// cannot drift. Falls back to the range's own minimum if the table row ever
/// goes missing, which is a visible handle at the left rather than none at all.
/// Mirrors [`render_distance_slider_fraction`] — same shape, same reasoning,
/// an `IntRange(0, 10)` (`Options.java`) rather than the chunk range.
#[must_use]
pub fn sprint_window_slider_fraction(ticks: u8) -> f32 {
    let range = INT_RANGE_SLIDERS
        .iter()
        .find(|(a, _, _)| *a == "sprintWindow")
        .map_or(SliderRange { min: 0, max: 10 }, |(_, r, _)| *r);
    range.to_slider_value(i32::from(ticks))
}

/// `mipmapLevels`' slider fraction from the real, persisted mip depth.
///
/// Reuses the same [`INT_RANGE_SLIDERS`] row the inactive version used, so the
/// live handle and the frozen-default handle are placed by one expression and
/// cannot drift. Mirrors [`sprint_window_slider_fraction`] — same shape, same
/// reasoning, an `IntRange(0, 4)` (`Options.java`) rather than the tick range.
#[must_use]
pub fn mipmap_levels_slider_fraction(levels: u32) -> f32 {
    let range = INT_RANGE_SLIDERS
        .iter()
        .find(|(a, _, _)| *a == "mipmapLevels")
        .map_or(SliderRange { min: 0, max: 4 }, |(_, r, _)| *r);
    range.to_slider_value(i32::try_from(levels).unwrap_or(range.min))
}

/// `menuBackgroundBlurriness`' slider fraction from the real, persisted
/// blurriness. The sixth of the family — see [`mipmap_levels_slider_fraction`].
#[must_use]
pub fn menu_background_blurriness_slider_fraction(blurriness: u32) -> f32 {
    let range = INT_RANGE_SLIDERS
        .iter()
        .find(|(a, _, _)| *a == "menuBackgroundBlurriness")
        .map_or(SliderRange { min: 0, max: 10 }, |(_, r, _)| *r);
    range.to_slider_value(i32::try_from(blurriness).unwrap_or(range.max))
}

/// `weatherRadius`' slider fraction from the real, persisted block radius.
///
/// The fifth of the identical family (see [`mipmap_levels_slider_fraction`]),
/// reading the same [`INT_RANGE_SLIDERS`] row the inactive handle used so that
/// making the row live does not move a handle a player was already looking at.
#[must_use]
pub fn weather_radius_slider_fraction(radius: i32) -> f32 {
    let range = INT_RANGE_SLIDERS
        .iter()
        .find(|(a, _, _)| *a == "weatherRadius")
        .map_or(SliderRange { min: 3, max: 10 }, |(_, r, _)| *r);
    range.to_slider_value(radius)
}

/// `fov`'s slider fraction from the real, persisted degree count.
///
/// The third of the identical trio (see [`render_distance_slider_fraction`] and
/// [`sprint_window_slider_fraction`]) and it reads the same
/// [`INT_RANGE_SLIDERS`] row the inactive handle used, so making the row live
/// cannot move the handle a player was already looking at.
#[must_use]
pub fn fov_slider_fraction(degrees: u32) -> f32 {
    let range = INT_RANGE_SLIDERS
        .iter()
        .find(|(a, _, _)| *a == "fov")
        .map_or(
            SliderRange {
                min: crate::config::MIN_FOV as i32,
                max: crate::config::MAX_FOV as i32,
            },
            |(_, r, _)| *r,
        );
    range.to_slider_value(i32::try_from(degrees).unwrap_or(range.min))
}

/// `framerateLimit`'s slider fraction from the real, persisted fps value.
///
/// The fourth of the identical trio (see [`render_distance_slider_fraction`],
/// [`sprint_window_slider_fraction`], [`fov_slider_fraction`]) — but the *one*
/// with an `xmap` between the stored value and the bucket
/// [`SliderRange`] operates on: `INT_RANGE_SLIDERS`'s `"framerateLimit"` row is
/// `IntRange(1, 26)` over `fps / 10`, not over `fps` itself
/// (`Options.java`). Dividing before handing it to `to_slider_value` is
/// what keeps the handle and the stored fps agreeing about which bucket `120`
/// (the default) lands in — bucket `12`, handle at `11.5 / 26`.
#[must_use]
pub fn framerate_limit_slider_fraction(fps: u32) -> f32 {
    let range = INT_RANGE_SLIDERS
        .iter()
        .find(|(a, _, _)| *a == "framerateLimit")
        .map_or(SliderRange { min: 1, max: 26 }, |(_, r, _)| *r);
    range.to_slider_value(i32::try_from(fps / 10).unwrap_or(range.min))
}

/// `graphicsPreset`'s slider fraction from the real, persisted preset —
/// `SliderableEnum.toSliderValue`'s `values.indexOf(value) / (size - 1)`
/// (`OptionInstance.java`), endpoints pinned exactly like
/// [`graphics_preset_default_fraction`] already does for the inactive row.
#[must_use]
pub fn graphics_preset_slider_fraction(preset: crate::config::GraphicsPreset) -> f32 {
    let index = crate::config::GraphicsPreset::ORDER
        .iter()
        .position(|p| *p == preset)
        .unwrap_or(0);
    index as f32 / (crate::config::GraphicsPreset::ORDER.len() - 1) as f32
}

/// The inverse of [`graphics_preset_slider_fraction`] — the **drag** write
/// side. Vanilla's `SliderableValueSet` default `fromSliderValue`
/// (`OptionInstance.java`, `IntRangeBase`'s, which `SliderableEnum`
/// inherits): `floor(map(slider, 0, 1, 0, size))`, clamping a `slider >= 1.0`
/// down first so the top of the track cannot floor *past* the last index.
#[must_use]
pub fn graphics_preset_from_fraction(fraction: f32) -> crate::config::GraphicsPreset {
    use crate::config::GraphicsPreset;
    let f = fraction.clamp(0.0, 0.999_999);
    let count = GraphicsPreset::ORDER.len();
    let index = ((f * count as f32).floor() as usize).min(count - 1);
    GraphicsPreset::ORDER[index]
}

/// `mouseWheelSensitivity`'s slider fraction from the real, live config
/// value — the one place this module inverts vanilla's own stringifier
/// rather than restating a table.
///
/// Vanilla stores the option as `logMouse(intValue) = 10^(intValue / 100)`
/// over `IntRange(-200, 100)` (`Options.java`), and
/// `IntRangeBase.toSliderValue` maps that int **linearly, except at the two
/// endpoints** (`OptionInstance.java`):
/// `map(intValue + 0.5, min, max + 1, 0, 1)`. This inverts the stored double
/// back to vanilla's int via `unlogMouse` (`Options.java`,
/// `Mth.floor` is a plain `floor`) and then applies the same map, so the
/// shipped config default of `1.0`
/// ([`crate::config::Options::default`]) lands on the same fraction a fresh
/// vanilla install shows: `unlogMouse(1.0) == 0`, `map(0.5, -200, 101, 0, 1)
/// == 200.5 / 301 ≈ 0.6661`.
///
/// Clamps to the endpoint fractions for a value outside vanilla's
/// representable range rather than producing a handle off the track — this
/// client's own config does not enforce that range today, so a corrupted or
/// hand-edited `options.json` must not panic or draw off-widget.
#[must_use]
pub fn mouse_wheel_slider_fraction(value: f32) -> f32 {
    const MIN: f64 = -200.0;
    const MAX: f64 = 100.0;
    if !(value as f64).is_finite() || value <= 0.0 {
        return 0.0;
    }
    let int_value = ((value as f64).log10() * 100.0).floor().clamp(MIN, MAX);
    let fraction = if int_value <= MIN {
        0.0
    } else if int_value >= MAX {
        1.0
    } else {
        (int_value + 0.5 - MIN) / (MAX + 1.0 - MIN)
    };
    fraction as f32
}

/// `Options.genericValueLabel` (`Options.java`):
/// `Component.translatable("options.generic_value", caption, value)`, whose
/// `en_us.json` pattern is `"%s: %s"`.
#[must_use]
pub fn generic_value_label(caption: &str, value: &str) -> String {
    format!("{caption}: {value}")
}

/// The displayed value of one live option.
///
/// `guiScale`'s stringifier is `value == 0 ? "options.guiScale.auto" :
/// literal(value)` (`Options.java`) — note it returns the value **without**
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
        LiveOption::ShowSubtitles => {
            if options.show_subtitles { "ON" } else { "OFF" }.to_string()
        }
        // `ToggleKeyMapping`'s own stringifier is `value ? KEY_TOGGLE :
        // KEY_HOLD` (`ToggleKeyMapping`'s caller in `Options.java`),
        // i.e. "Toggle"/"Hold" — **not** ON/OFF, unlike every other boolean
        // option on this page. `en_us.json`'s `options.key.toggle`/
        // `options.key.hold`.
        LiveOption::ToggleSneak => {
            if options.toggle_sneak { "Toggle" } else { "Hold" }.to_string()
        }
        LiveOption::ToggleSprint => {
            if options.toggle_sprint { "Toggle" } else { "Hold" }.to_string()
        }
        LiveOption::ToggleAttack => {
            if options.toggle_attack { "Toggle" } else { "Hold" }.to_string()
        }
        LiveOption::ToggleUse => {
            if options.toggle_use { "Toggle" } else { "Hold" }.to_string()
        }
        // `createBoolean("options.autoJump", false)` (`Options.java`) —
        // the plain boolean stringifier, `OPTIONS_ON`/`OPTIONS_OFF`, not the
        // `ToggleKeyMapping` "Toggle"/"Hold".
        LiveOption::AutoJump => {
            if options.auto_jump { "ON" } else { "OFF" }.to_string()
        }
        // `IntRange(0, 10)` (`Options.java`), stringifier
        // `value == 0 ? genericValueLabel(caption, OPTION_OFF) :
        // genericValueLabel(caption, OPTION_VALUE, value)` — "OFF" at 0,
        // else the tick count.
        LiveOption::SprintWindow => {
            if options.sprint_window_ticks == 0 {
                "OFF".to_string()
            } else {
                options.sprint_window_ticks.to_string()
            }
        }
        LiveOption::InvertMouseX => {
            if options.invert_mouse_x { "ON" } else { "OFF" }.to_string()
        }
        LiveOption::InvertMouseY => {
            if options.invert_mouse_y { "ON" } else { "OFF" }.to_string()
        }
        LiveOption::DiscreteMouseScroll => {
            if options.discrete_mouse_scroll { "ON" } else { "OFF" }.to_string()
        }
        // `String.format(Locale.ROOT, "%.2f", value)` (`Options.java`).
        LiveOption::MouseWheelSensitivity => {
            format!("{:.2}", options.mouse_wheel_sensitivity)
        }
        // `value == 0.0 ? CommonComponents.optionStatus(caption, false) :
        // percentValueLabel(caption, value)` (`Options.java`) — the one
        // chat slider with an OFF caption, and `optionStatus(caption, false)`
        // is itself `genericValueLabel(caption, OPTION_OFF)`, so composing
        // `"OFF"` through [`Cell::label`] reproduces it exactly.
        LiveOption::ChatScale => {
            if options.chat_scale == 0.0 {
                "OFF".to_string()
            } else {
                percent_value(options.chat_scale)
            }
        }
        // `pixelValueLabel(caption, ChatComponent.getWidth(value))`
        // (`Options.java`).
        LiveOption::ChatWidth => pixel_value(chat_width_px(options.chat_width)),
        // `pixelValueLabel(caption, ChatComponent.getHeight(value))`
        // (`Options.java`).
        LiveOption::ChatHeightFocused => pixel_value(chat_height_px(options.chat_height_focused)),
        // As above (`Options.java`).
        LiveOption::ChatHeightUnfocused => {
            pixel_value(chat_height_px(options.chat_height_unfocused))
        }
        // Plain `Options::percentValueLabel` (`Options.java`).
        LiveOption::ChatLineSpacing => percent_value(options.chat_line_spacing),
        // **Affine, not plain percent**: `percentValueLabel(caption, value *
        // 0.9 + 0.1)` (`Options.java`). So a stored `1.0` prints
        // `100%` but a stored `0.0` prints `10%`, never `0%` — chat text is
        // never fully transparent in vanilla. Transcribing this as a plain
        // percent would be wrong at every value but `1.0`.
        LiveOption::ChatOpacity => percent_value(options.chat_opacity * 0.9 + 0.1),
        // Plain `Options::percentValueLabel` (`Options.java`).
        LiveOption::TextBackgroundOpacity => percent_value(options.chat_background_opacity),
        LiveOption::ChatColors => {
            if options.chat_colors { "ON" } else { "OFF" }.to_string()
        }
        // **`2.0 * value`, and the doubling is the whole subtlety**:
        // `value == 0.0 -> "options.sensitivity.min", value == 1.0 ->
        // "options.sensitivity.max", else percentValueLabel(caption, 2.0 *
        // value)` (`Options.java`). So the shipped default of `0.5`
        // prints **100%**, not 50%, and the maximum prints 200%. Printing the
        // stored number as a percentage directly would halve every label a
        // player reads while the mouse behaved correctly — a wire carrying the
        // right value with the wrong label.
        //
        // The two endpoint captions are `en_us.json`'s
        // `options.sensitivity.min` = `"*yawn*"` and `.max` = `"HYPERSPEED!!!"`,
        // read from the language file rather than remembered. Vanilla tests them
        // with exact `== 0.0`/`== 1.0`; the `<=`/`>=` here is identical on the
        // domain [`crate::config::Options::from_json`] already clamps to, and
        // keeps a hand-edited file from falling through to a percentage above
        // 200%.
        LiveOption::Sensitivity => {
            if options.sensitivity <= 0.0 {
                "*yawn*".to_string()
            } else if options.sensitivity >= 1.0 {
                "HYPERSPEED!!!".to_string()
            } else {
                percent_value(options.sensitivity * 2.0)
            }
        }
        // `genericValueLabel(caption, translatable("options.chunks", value))`
        // (`Options.java`). `en_us.json`'s pattern is `"%s Chunks"` — a
        // **capital** C, which is the sort of thing that only a look at the
        // language file gets right.
        LiveOption::RenderDistance => format!("{} Chunks", options.render_distance),
        // `Options::percentValueOrOffLabel`: `value == 0.0 ?
        // genericValueLabel(caption, OPTION_OFF) : percentValueLabel(caption,
        // value)`. So this is **not** the plain percent transcription its
        // neighbours use — a stored `0.0` prints "OFF", and only `0.0` does,
        // because `percentValueLabel`'s `(int)(value * 100.0)` would print `0%`
        // for anything in `[0, 0.01)` and vanilla tests the double for exact
        // equality before it gets there. `<= 0.0` rather than `== 0.0` for
        // `LiveOption::Sensitivity`'s reason: identical on the domain
        // `Options::from_json` clamps to, and it keeps a hand-edited negative out
        // of the percentage branch.
        LiveOption::DamageTiltStrength => {
            if options.damage_tilt_strength <= 0.0 {
                "OFF".to_string()
            } else {
                percent_value(options.damage_tilt_strength)
            }
        }
        // The plain `Options::percentValueLabel`, **not** the OrOff variant its
        // Accessibility-page neighbours use — vanilla's `panoramaSpeed` field
        // names `Options::percentValueLabel` directly, so a stationary panorama
        // reads `0%`. That is right rather than an oversight: zero speed is a
        // legitimate position on this slider, not the option being off.
        LiveOption::PanoramaSpeed => percent_value(options.panorama_speed),
        // All eleven volume sliders share one stringifier, because vanilla builds
        // all eleven from one factory: `createSoundSliderOptionInstance` passes
        // `Options::percentValueOrOffLabel`, so a muted bus reads **OFF** and not
        // `0%`. Same shape as `DamageTiltStrength` above, and `<= 0.0` for the
        // same reason.
        //
        // An index past the array is a `0%`-free "OFF" rather than a panic — see
        // [`LiveOption::SoundVolume`].
        LiveOption::SoundVolume(index) => {
            match options.sound_volumes.get(index as usize) {
                Some(&v) if v > 0.0 => percent_value(v),
                _ => "OFF".to_string(),
            }
        }
        // `switch (value) { case 70 -> options.fov.min; case 110 ->
        // options.fov.max; default -> value }`, each arm wrapped in
        // `genericValueLabel`, so the caption composes as usual and only the
        // **value half** varies. `en_us.json`: `options.fov.min` is "Normal" and
        // `options.fov.max` is "Quake Pro" — no exclamation mark, unlike
        // `sensitivity`'s "HYPERSPEED!!!".
        //
        // **The special case is at the default.** 70 is both vanilla's `case 70`
        // and its shipped default, so a fresh install reads "FOV: Normal" — a
        // transcription that just printed the integer would read "FOV: 70" and
        // disagree with vanilla on the *one* value every new player sees. The two
        // literals are vanilla's own; they are not written as
        // `crate::config::DEFAULT_FOV`/`MAX_FOV` because the coincidence is
        // vanilla's and not a constraint either constant is under.
        LiveOption::Fov => match options.fov {
            70 => "Normal".to_string(),
            110 => "Quake Pro".to_string(),
            degrees => degrees.to_string(),
        },
        // Both glint options are `percentValueOrOffLabel` too, and `0.0` is a
        // deliberate value on each: a zero *speed* is a frozen shimmer and a zero
        // *strength* is an invisible one, so "OFF" is the honest label rather
        // than a stand-in for "unset".
        LiveOption::GlintSpeed => {
            if options.glint_speed <= 0.0 {
                "OFF".to_string()
            } else {
                percent_value(options.glint_speed)
            }
        }
        LiveOption::GlintStrength => {
            if options.glint_strength <= 0.0 {
                "OFF".to_string()
            } else {
                percent_value(options.glint_strength)
            }
        }
        // `CloudStatus.caption()` — the enum's *own* component, keyed
        // `options.off`/`options.clouds.fast`/`options.clouds.fancy`
        // (`CloudStatus.java`), i.e. "OFF"/"Fast"/"Fancy" in `en_us.json`.
        //
        // This is the whole label, not a value half: see
        // [`LiveOption::value_is_the_whole_label`].
        LiveOption::CloudStatus => match options.cloud_status {
            lodestone_render::CloudStatus::Off => "OFF".to_string(),
            lodestone_render::CloudStatus::Fast => "Fast".to_string(),
            lodestone_render::CloudStatus::Fancy => "Fancy".to_string(),
        },
        // `value == 260 ? genericValueLabel(caption, "Unlimited") :
        // genericValueLabel(caption, "%s fps" % value)` (`Options.java`,
        // `en_us.json`'s `options.framerate`/`options.framerateLimit.max`).
        LiveOption::FramerateLimit => {
            if options.framerate_limit >= crate::config::UNLIMITED_FRAMERATE_CUTOFF {
                "Unlimited".to_string()
            } else {
                format!("{} fps", options.framerate_limit)
            }
        }
        LiveOption::EnableVsync => {
            if options.enable_vsync { "ON" } else { "OFF" }.to_string()
        }
        // `InactivityFpsLimit.caption()` — "AFK"/"Minimized"
        // (`en_us.json`'s `options.inactivityFpsLimit.afk`/`.minimized`). The
        // whole label, not a value half: see
        // [`LiveOption::value_is_the_whole_label`].
        LiveOption::InactivityFpsLimit => match options.inactivity_fps_limit {
            crate::config::InactivityFpsLimit::Minimized => "Minimized".to_string(),
            crate::config::InactivityFpsLimit::Afk => "AFK".to_string(),
        },
        // `Component.translatable(value.getKey())` — `en_us.json`'s
        // `options.graphics.fast`/`.fancy`/`.fabulous`/`.custom`.
        LiveOption::GraphicsPreset => match options.graphics_preset {
            crate::config::GraphicsPreset::Fast => "Fast".to_string(),
            crate::config::GraphicsPreset::Fancy => "Fancy".to_string(),
            crate::config::GraphicsPreset::Fabulous => "Fabulous".to_string(),
            crate::config::GraphicsPreset::Custom => "Custom".to_string(),
        },
        LiveOption::CutoutLeaves => {
            if options.cutout_leaves { "ON" } else { "OFF" }.to_string()
        }
        // `IntRange(0, 4)` (`Options.java`) with no special-case stringifier —
        // unlike `Fov`'s "Normal"/"Quake Pro" pair, vanilla's mipmap slider has
        // no named values, so the plain depth is the whole value half.
        LiveOption::MipmapLevels => options.mipmap_levels.to_string(),
        LiveOption::EntityShadows => {
            if options.entity_shadows { "ON" } else { "OFF" }.to_string()
        }
        // `genericValueLabel(caption, translatable("options.blocks", value))`
        // (`Options.java`). `en_us.json`'s pattern is `"%s Blocks"` — a
        // **capital** B, and **Blocks** rather than `RenderDistance`'s Chunks:
        // this slider is denominated in blocks and its Video-page neighbour
        // `cloudRange` is the chunk-denominated one.
        LiveOption::WeatherRadius => format!("{} Blocks", options.weather_radius),
        // `Options::genericValueOrOffLabel`: `value == 0 ?
        // genericValueLabel(caption, OPTION_OFF) : genericValueLabel(caption,
        // value)` (`Options.java`) — the **integer** sibling of the
        // `percentValueOrOffLabel` the glint and volume sliders use, so a zero
        // reads OFF and every other value is the bare number with no unit.
        LiveOption::MenuBackgroundBlurriness => {
            if options.menu_background_blurriness == 0 {
                "OFF".to_string()
            } else {
                options.menu_background_blurriness.to_string()
            }
        }
        // `AttackIndicatorStatus.caption()` — the enum's own component, keyed
        // `options.off`/`options.attack.crosshair`/`options.attack.hotbar`
        // (`AttackIndicatorStatus.java`), i.e. "OFF"/"Crosshair"/"Hotbar" in
        // `en_us.json`.
        //
        // The whole label, not a value half: see
        // [`LiveOption::value_is_the_whole_label`].
        LiveOption::AttackIndicator => match options.attack_indicator {
            crate::config::AttackIndicator::Off => "OFF".to_string(),
            crate::config::AttackIndicator::Crosshair => "Crosshair".to_string(),
            crate::config::AttackIndicator::Hotbar => "Hotbar".to_string(),
        },
    }
}

/// One row of an `OptionsList`, i.e. one `addBig` / `addSmall` / `addHeader`
/// call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// `addHeader(text)`: a `StringWidget`, not a control. Its height is
    /// `paddingTop + 9 + 4` and its `paddingTop` is `0` for the first entry in
    /// the list and `18` otherwise (`OptionsList.java`) — which is why
    /// [`entry_height`] takes the index.
    Header(&'static str),
    /// `addBig(option)`: one 310 px control on its own row.
    Big(Cell),
    /// `addSmall(a, b)`: two 150 px controls 160 px apart, or one when the
    /// option count is odd (`OptionsList.java`).
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

/// The root's second header button — vanilla's `inWorld` fork
/// (`OptionsScreen.java`). Outside a world it is a live link to
/// [`SettingsPage::Online`]; inside one it is `WorldOptionsScreen`, which this
/// client does not build, so it stays the same `no_screen` placeholder shape
/// every other unbuilt screen uses.
///
/// This is the **one** place that decides both the label and the liveness —
/// [`settings_frame`] no longer carries a second copy of this fork, because a
/// fact declared in two places is exactly the fabrication class the module
/// docs' departure (1) exists to avoid.
fn online_cell(in_world: bool) -> Cell {
    if in_world {
        no_screen("World Options...")
    } else {
        nav("Online...", SettingsPage::Online)
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
/// (`OptionsList.java`), so the two columns of a row are consecutive
/// entries of vanilla's array and the last one is alone if the count is odd.
static VIDEO: &[Entry] = &[
    head("Display"),
    big(slider("fullscreenResolution", "Fullscreen Resolution")),
    pair(
        live_slider("framerateLimit", "Max Framerate", LiveOption::FramerateLimit),
        live_cycle("enableVsync", "VSync", LiveOption::EnableVsync),
    ),
    pair(
        live_cycle(
            "inactivityFpsLimit",
            "Reduce FPS when",
            LiveOption::InactivityFpsLimit,
        ),
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
    big(live_slider("graphicsPreset", "Preset", LiveOption::GraphicsPreset)),
    pair(
        slider("biomeBlendRadius", "Biome Blend"),
        // Live since issue #443 — see `LiveOption::RenderDistance`. Its
        // neighbour `simulationDistance` below is deliberately *not*: this
        // client has no simulation-distance consumer at all, so wiring it would
        // be the fabrication #443 exists to undo, one row over.
        live_slider(
            "renderDistance",
            "Render Distance",
            LiveOption::RenderDistance,
        ),
    ),
    pair(
        cycle("prioritizeChunkUpdates", "Chunk Builder"),
        slider("simulationDistance", "Simulation Distance"),
    ),
    pair(
        cycle("ambientOcclusion", "Smooth Lighting"),
        // Three states, and the row's label is the value **alone** — vanilla's
        // stringifier here discards the caption. See `LiveOption::CloudStatus`.
        live_cycle("cloudStatus", "Clouds", LiveOption::CloudStatus),
    ),
    pair(
        cycle("particles", "Particles"),
        // Live: the block-atlas island `BLOCK_ATLAS_MIP_LEVELS`'s own doc used
        // to name. See `LiveOption::MipmapLevels`.
        live_slider("mipmapLevels", "Mipmap Levels", LiveOption::MipmapLevels),
    ),
    pair(
        // Live (owner report: "entity shadows are missing"). See
        // `LiveOption::EntityShadows`.
        live_cycle("entityShadows", "Entity Shadows", LiveOption::EntityShadows),
        slider("entityDistanceScaling", "Entity Distance"),
    ),
    pair(
        // Live: the menu background-blur pass existed and ran at the frozen
        // `menu::render::blur::BLUR_RADIUS`. See
        // `LiveOption::MenuBackgroundBlurriness`. The Accessibility page carries
        // the same option, as vanilla does — both rows drive one field.
        live_slider(
            "menuBackgroundBlurriness",
            "Menu Background Blur",
            LiveOption::MenuBackgroundBlurriness,
        ),
        slider("cloudRange", "Cloud Distance"),
    ),
    pair(
        live_cycle("cutoutLeaves", "See-Through Leaves", LiveOption::CutoutLeaves),
        cycle("improvedTransparency", "Improved Transparency"),
    ),
    pair(
        cycle("textureFiltering", "Texture Filtering"),
        slider("maxAnisotropyBit", "Anisotropic Filtering"),
    ),
    // Live: the rain/snow column walk already took a radius parameter and was
    // handed `lodestone_render::DEFAULT_WEATHER_RADIUS` at both call sites. See
    // `LiveOption::WeatherRadius`.
    lone(live_slider(
        "weatherRadius",
        "Weather Effect Radius",
        LiveOption::WeatherRadius,
    )),
    head("Preferences"),
    pair(
        cycle("showAutosaveIndicator", "Autosave Indicator"),
        cycle("vignette", "Show Vignette"),
    ),
    pair(
        // Live: the crosshair strength bar already drew, pinned to vanilla's
        // CROSSHAIR. See `LiveOption::AttackIndicator`.
        live_cycle(
            "attackIndicator",
            "Attack Indicator",
            LiveOption::AttackIndicator,
        ),
        slider("chunkSectionFadeInTime", "Chunk Fade Time"),
    ),
];

/// `ControlsScreen.addOptions` (`controls/ControlsScreen.java`).
///
/// The four `toggle*` options are the only ones in the tree whose caption is a
/// **keybind** name rather than an `options.*` key — `key.sneak`, `key.sprint`,
/// `key.attack`, `key.use` (`Options.java`) — and their values are
/// `options.key.toggle`/`options.key.hold` rather than ON/OFF.
///
/// **All four toggles are live** — Sneak/Sprint since #202
/// ([`crate::config::Options::toggle_sneak`]/`toggle_sprint`, read by
/// `InputState::set_toggle_modes`), Attack/Use since #444
/// (`toggle_attack`/`toggle_use`, carried by the same setter; the flags reach
/// the model end to end, and `interact.rs` will hang its own consumers off
/// them). **`autoJump` and `sprintWindow` are live since #444 too** — the
/// tick loop's auto-jump gate, and the double-tap-sprint window respectively.
static CONTROLS: &[Entry] = &[
    pair(
        nav("Mouse Settings...", SettingsPage::Mouse),
        // `controls.keybinds` (issue #15). No longer `no_screen`: the
        // rebindable layer (`crate::keybinds`) has had no screen in front of
        // it since it landed; `SettingsPage::KeyBinds` is that screen.
        nav("Key Binds...", SettingsPage::KeyBinds),
    ),
    pair(
        live_cycle("toggleCrouch", "Sneak", LiveOption::ToggleSneak),
        live_cycle("toggleSprint", "Sprint", LiveOption::ToggleSprint),
    ),
    pair(
        live_cycle("toggleAttack", "Attack/Destroy", LiveOption::ToggleAttack),
        live_cycle("toggleUse", "Use Item/Place Block", LiveOption::ToggleUse),
    ),
    pair(
        live_cycle("autoJump", "Auto-Jump", LiveOption::AutoJump),
        live_slider("sprintWindow", "Sprint Window", LiveOption::SprintWindow),
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
/// `invert_mouse_y`. **Sensitivity (look) is now live too** (issue #443): it
/// used to live only on [`crate::config::Config`], parsed from argv and never
/// written back, so a row for it would have been fabricated persistence; it is
/// now a real [`crate::config::Options`] field that
/// [`crate::config::Config::resolve_persisted`] folds back in at launch, and its
/// consumer (`sim/step.rs`'s `apply_mouse`) already existed. `discreteMouseScroll`,
/// `allowCursorChanges` and `rawMouseInput` are also still inactive: none of
/// the three has a consumer in this shell yet (there is no discrete-vs-continuous
/// scroll distinction, no OS cursor swap, and no raw-input toggle), so wiring
/// the label without the behaviour would be exactly the fabrication #203
/// exists to fix, one row over.
static MOUSE: &[Entry] = &[
    pair(
        live_slider("sensitivity", "Sensitivity", LiveOption::Sensitivity),
        live_slider(
            "mouseWheelSensitivity",
            "Scroll Sensitivity",
            LiveOption::MouseWheelSensitivity,
        ),
    ),
    pair(
        live_cycle(
            "discreteMouseScroll",
            "Discrete Scrolling",
            LiveOption::DiscreteMouseScroll,
        ),
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
/// (`sounds/SoundSource.java`) with `MASTER` pulled out into the `addBig`
/// row; their captions are `soundCategory.<name>`.
///
/// **All eleven are live.** Each carries its own
/// [`LiveOption::SoundVolume`] index, and that index is the `SoundSource`
/// ordinal — so it is simultaneously the slot in
/// [`crate::config::Options::sound_volumes`], the `sound_volume_<name>` key in
/// `options.json` and the mixer bus `lodestone_audio::CategoryVolumes::set_user`
/// writes. The indices below **must** match the accessor suffixes, which is a
/// property no compiler checks and `sound_rows_index_the_category_they_name`
/// therefore does: a transposed pair here would move the wrong bus while every
/// label read correctly.
static SOUND: &[Entry] = &[
    big(live_slider(
        "soundSource.master",
        "Master Volume",
        LiveOption::SoundVolume(0),
    )),
    pair(
        live_slider("soundSource.music", "Music", LiveOption::SoundVolume(1)),
        live_slider(
            "soundSource.record",
            "Jukebox/Note Blocks",
            LiveOption::SoundVolume(2),
        ),
    ),
    pair(
        live_slider("soundSource.weather", "Weather", LiveOption::SoundVolume(3)),
        live_slider("soundSource.block", "Blocks", LiveOption::SoundVolume(4)),
    ),
    pair(
        live_slider(
            "soundSource.hostile",
            "Hostile Mobs",
            LiveOption::SoundVolume(5),
        ),
        live_slider(
            "soundSource.neutral",
            "Friendly Mobs",
            LiveOption::SoundVolume(6),
        ),
    ),
    pair(
        live_slider("soundSource.player", "Players", LiveOption::SoundVolume(7)),
        live_slider(
            "soundSource.ambient",
            "Ambient/Environment",
            LiveOption::SoundVolume(8),
        ),
    ),
    pair(
        live_slider(
            "soundSource.voice",
            "Narrator/Voice",
            LiveOption::SoundVolume(9),
        ),
        live_slider("soundSource.ui", "UI", LiveOption::SoundVolume(10)),
    ),
    big(cycle("soundDevice", "Device")),
    pair(
        live_cycle("showSubtitles", "Closed Captions", LiveOption::ShowSubtitles),
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
        live_cycle("chatColors", "Colors", LiveOption::ChatColors),
    ),
    pair(
        cycle("chatLinks", "Web Links"),
        cycle("chatLinksPrompt", "Prompt on Links"),
    ),
    pair(
        live_slider("chatOpacity", "Chat Text Opacity", LiveOption::ChatOpacity),
        live_slider(
            "textBackgroundOpacity",
            "Text Background Opacity",
            LiveOption::TextBackgroundOpacity,
        ),
    ),
    pair(
        live_slider("chatScale", "Chat Text Size", LiveOption::ChatScale),
        live_slider("chatLineSpacing", "Line Spacing", LiveOption::ChatLineSpacing),
    ),
    pair(
        slider("chatDelay", "Chat Delay"),
        live_slider("chatWidth", "Width", LiveOption::ChatWidth),
    ),
    pair(
        live_slider(
            "chatHeightFocused",
            "Focused Height",
            LiveOption::ChatHeightFocused,
        ),
        live_slider(
            "chatHeightUnfocused",
            "Unfocused Height",
            LiveOption::ChatHeightUnfocused,
        ),
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
        live_cycle("showSubtitles", "Closed Captions", LiveOption::ShowSubtitles),
        cycle("highContrast", "High Contrast"),
    ),
    pair(
        // The Video page's own row for the same `OptionInstance` — vanilla
        // places this option on both screens, like the three chat sliders
        // below. Editing either moves the other's label.
        live_slider(
            "menuBackgroundBlurriness",
            "Menu Background Blur",
            LiveOption::MenuBackgroundBlurriness,
        ),
        live_slider(
            "textBackgroundOpacity",
            "Text Background Opacity",
            LiveOption::TextBackgroundOpacity,
        ),
    ),
    pair(
        cycle("backgroundForChatOnly", "Text Background"),
        live_slider("chatOpacity", "Chat Text Opacity", LiveOption::ChatOpacity),
    ),
    pair(
        live_slider("chatLineSpacing", "Line Spacing", LiveOption::ChatLineSpacing),
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
        live_slider(
            "damageTiltStrength",
            "Damage Tilt",
            LiveOption::DamageTiltStrength,
        ),
    ),
    pair(
        live_slider("glintSpeed", "Glint Speed", LiveOption::GlintSpeed),
        live_slider("glintStrength", "Glint Strength", LiveOption::GlintStrength),
    ),
    pair(
        cycle("hideLightningFlash", "Hide Sky Flashes"),
        cycle("darkMojangStudiosBackground", "Monochrome Logo"),
    ),
    pair(
        live_slider(
            "panoramaSpeed",
            "Panorama Scroll Speed",
            LiveOption::PanoramaSpeed,
        ),
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
/// (`world/entity/player/PlayerModelPart.java`) as `onOffBuilder` cycle
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

/// `OnlineOptionsScreen.addOptions` (`OnlineOptionsScreen.java`), in its
/// own call order. Every control here is decorative — see
/// [`SettingsPage::Online`]'s doc for why — so every accessor uses [`cycle`],
/// never [`live_cycle`], and the Xbox link uses [`unsupported`] exactly like
/// the Accessibility Guide and Credits buttons on other pages.
///
/// `friendsList`/`allowFriendRequests` are not `Options.java` `OptionInstance`s
/// at all — vanilla backs them with `PlayerSocialManager` state instead
/// (`:89-104`) — so their `accessor` strings are synthetic, the same
/// convention [`SKIN`] already uses for `PlayerModelPart` ids that are not
/// `Options.java` methods either.
///
/// `realmsNotifications`' caption is **not** `options.realmsNotifications`
/// ("Realms News & Invites"): the `OptionInstance` is constructed with the
/// `.button` key (`Options.java`), whose `en_us.json` string is
/// "News & Invites". Easy to get backwards by reading the accessor name alone.
static ONLINE: &[Entry] = &[
    head("Friends List"), // options.online.friends.header
    pair(
        cycle("friendsList", "Friends List"), // options.friendsList
        cycle("allowFriendRequests", "Allow Requests"), // options.allowFriendRequests
    ),
    pair(
        cycle("inGameNotification", "In-Game Notification"),
        cycle("sharePresence", "Visibility"), // options.sharePresence
    ),
    big(unsupported("Xbox Settings...")), // options.online.xboxSettings
    head("Servers"),                      // options.online.servers.header
    big(cycle("allowServerListing", "Allow Server Listings")),
    head("Realms"), // options.online.realms.header
    big(cycle("realmsNotifications", "News & Invites")), // options.realmsNotifications.button
];

/// The root screen's ten nav buttons, in `OptionsScreen.init`'s own
/// `helper.addChild` order (`:70-95`) — which is what fills the 2×5 grid
/// row-major.
static ROOT_GRID: &[Cell] = &[
    nav("Skin Customization...", SettingsPage::Skin),
    nav("Music & Sounds...", SettingsPage::Sound),
    nav("Video Settings...", SettingsPage::Video),
    nav("Controls...", SettingsPage::Controls),
    // Issue #415 — the first of the three unbuilt sub-screens to get its own
    // list widget. See `SettingsPage::Language`'s own doc.
    nav("Language...", SettingsPage::Language),
    nav("Chat Settings...", SettingsPage::Chat),
    // Issue #415 — the third and last of the three unbuilt sub-screens. It
    // *landed* as a deliberately reduced selection list (`2d9d3a18`) and this
    // comment said so until well after the real two-column screen replaced it
    // (`6bbc9940`): a folder scan, click-to-transfer between Available and
    // Selected, per-row reordering, and vanilla's own pack rows. See
    // `SettingsPage::ResourcePacks`'s own doc and `super::packs`'s.
    nav("Resource Packs...", SettingsPage::ResourcePacks),
    nav("Accessibility Settings...", SettingsPage::Accessibility),
    // Issue #415 — the second of the three unbuilt sub-screens to get its
    // own page. See `SettingsPage::Telemetry`'s own doc.
    nav("Telemetry Data...", SettingsPage::Telemetry),
    unsupported("Credits & Attribution..."),
];

/// One screen of the options tree.
///
/// All thirteen of vanilla's, as of issue #415 — every root-grid nav button
/// now opens something real, though "real" means a deliberately reduced
/// shape for two of them (see [`SettingsPage::ResourcePacks`]'s and
/// [`SettingsPage::Telemetry`]'s own docs). This table used to list the
/// screens still absent; there are none left in it, so it is kept as a
/// record of what each one needed rather than deleted:
///
/// | vanilla screen | what it needed |
/// |---|---|
/// | `LanguageSelectScreen` | the third list-widget kind (`ObjectSelectionList`) — see [`SettingsPage::Language`]/[`super::language`]. |
/// | `KeyBindsScreen` | a different list-widget kind again (`KeyBindsList`, not `OptionsList`) — see [`SettingsPage::KeyBinds`]/[`super::key_binds`]. |
/// | `OnlineOptionsScreen` | no new widget at all — the root's own header button was permanently inactive; see [`SettingsPage::Online`]. |
/// | `TelemetryInfoScreen` | no new widget either, once the event log and opt-in state this client structurally cannot have are recognised as *absent* rather than *unbuilt* — see [`SettingsPage::Telemetry`]/[`super::telemetry`]. |
/// | `PackSelectionScreen` | two transferable `ObjectSelectionList`s over a filesystem-backed `PackRepository`. Landed first as a declared reduction (one always-empty list, one always-one-entry list, no transfer controls), then **built for real**: a `resourcepacks/` folder scan accepting directories and zips, click-to-transfer, per-row reordering, `pack.mcmeta` descriptions and `pack.png` thumbnails, and the order fed into `ResourceManager`'s stack. See [`SettingsPage::ResourcePacks`]/[`super::packs`]. |
/// widget — `KeyBindsList`, not `OptionsList` — and got one: see
/// [`SettingsPage::KeyBinds`] and [`super::key_binds`], the second list-widget
/// kind #392's plan always said this tree would eventually need.
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
    /// `OnlineOptionsScreen` (`OnlineOptionsScreen.java`) — friends list,
    /// requests, in-game notifications, presence visibility, an external Xbox
    /// Settings link, server-listing opt-out and Realms news/invites. All
    /// seven are **decorative**: this client has no `PlayerSocialManager`, no
    /// Realms client and no Xbox link to send any of them to, so every control
    /// on the page is `unsupported`/`cycle` with `live: None` — see the
    /// [`ONLINE`] table. Only the page's own existence and its Done button are
    /// wired.
    ///
    /// Reached from the root's second header button when
    /// [`super::UiState::settings_in_world`] is `false` — vanilla's own fork
    /// (`OptionsScreen.java`): `in_world` picks `WorldOptionsScreen`
    /// instead, which this client does not build, so that branch stays a
    /// `no_screen` placeholder and the button reads "World Options..." and
    /// stays inactive, exactly as it always has. See [`controls`]'s
    /// `Placement::Root(2)` branch and [`SettingsNav::in_world`].
    Online,
    /// `controls/KeyBindsScreen` (issue #15) — **not** an `OptionsList` page.
    /// [`SettingsNav`] delegates every query and every input to
    /// [`super::key_binds::KeyBindsNav`] whenever `page == SettingsPage::KeyBinds`,
    /// the same way it special-cases [`SettingsPage::Root`] for a tree with no
    /// list at all. [`Self::entries`]/[`Self::footer`] therefore never run for
    /// this variant in practice; their arms return an empty/placeholder shape
    /// only so the match stays total.
    ///
    /// Reached from the Controls page's own "Key Binds..." button — vanilla's
    /// own wiring (`ControlsScreen.java`), not the root grid.
    KeyBinds,
    /// `LanguageSelectScreen` (issue #415) — **not** an `OptionsList` page,
    /// same reason as [`SettingsPage::KeyBinds`]: [`SettingsNav`] delegates to
    /// [`super::language::LanguageNav`] whenever `page ==
    /// SettingsPage::Language`, so [`Self::entries`]/[`Self::footer`] never
    /// actually run for it either — see [`super::language`]'s module docs for
    /// the whole screen.
    ///
    /// Reached from the **root grid**, unlike `KeyBinds` — vanilla's own
    /// wiring (`OptionsScreen.java`, `helper.addChild(this.openScreenButton(
    /// LANGUAGE, ...))`, the same `helper.addChild` sequence [`ROOT_GRID`]
    /// mirrors).
    Language,
    /// `TelemetryInfoScreen` (issue #415) — **not** an `OptionsList` page
    /// either, for the same structural reason as [`SettingsPage::Language`]
    /// and [`SettingsPage::KeyBinds`]: [`SettingsNav`] delegates to
    /// [`super::telemetry::TelemetryNav`] whenever `page ==
    /// SettingsPage::Telemetry`. Needed **no new list widget at all** —
    /// see [`super::telemetry`]'s module doc: once the event log and opt-in
    /// checkbox are recognised as things this client cannot have rather than
    /// things it has not built yet, what is left is a title, two paragraphs
    /// and four buttons, which this tree's existing primitives already
    /// cover.
    ///
    /// Reached from the **root grid**, vanilla's own wiring
    /// (`OptionsScreen.java`, `helper.addChild(this.openScreenButton(
    /// TELEMETRY, ...))`).
    ///
    /// **Considered departure**: vanilla itself disables this exact button
    /// (with a `TELEMETRY_DISABLED_TOOLTIP`) when `!minecraft.allowsTelemetry()`
    /// (`:89-91`) — the precedent this whole tree's disabled path already
    /// follows. This client is permanently in that state and could have kept
    /// the button inactive for that reason alone. It is live instead because
    /// the screen behind it has real content even with no telemetry system —
    /// two working external links — where vanilla's own binary choice assumes
    /// a telemetry-less screen has nothing worth reaching.
    Telemetry,
    /// `PackSelectionScreen` (issue #415) — **not** an `OptionsList` page,
    /// for the same structural reason as [`SettingsPage::Language`]/
    /// [`SettingsPage::Telemetry`]: two transferable columns over a real pack
    /// repository. Landed first as a reduced one-entry list and now built for
    /// real — the folder scan, the transfer, the priority order and the feed
    /// into [`lodestone_assets::ResourceManager`]'s stack. See
    /// [`super::packs`]'s module doc, including the one thing still skipped
    /// (pack-format validation).
    ///
    /// Reached from the **root grid**, vanilla's own wiring
    /// (`OptionsScreen.java`, `helper.addChild(this.openScreenButton(
    /// RESOURCEPACK, () -> new PackSelectionScreen(...)))`).
    ResourcePacks,
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
            // `options.online.title` (`en_us.json`).
            SettingsPage::Online => "Online Options",
            // `controls.keybinds.title` (`en_us.json`). Unreachable in
            // practice — `super::key_binds::frame` builds this page's title
            // label itself rather than through `settings_frame`'s generic
            // path — but kept accurate rather than a placeholder, since
            // nothing stops a future caller reaching it.
            SettingsPage::KeyBinds => "Key Binds",
            // `options.language.title` (`en_us.json`). Unreachable in
            // practice for the same reason `KeyBinds`' arm above is — see
            // `super::language::frame`.
            SettingsPage::Language => "Language",
            // `telemetry_info.screen.title` (`en_us.json`). Unreachable in
            // practice — same reason.
            SettingsPage::Telemetry => "Telemetry Data Collection",
            // `resourcePack.title` (`en_us.json`). Unreachable in practice —
            // same reason.
            SettingsPage::ResourcePacks => "Select Resource Packs",
        }
    }

    /// The header band's height: 61 on the root (`OptionsScreen.java`), the
    /// inherited 33 everywhere else (`OptionsSubScreen.java`).
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
            SettingsPage::Online => ONLINE,
            // Never actually read — see `SettingsPage::KeyBinds`'s doc.
            SettingsPage::KeyBinds => &[],
            // Never actually read — same reason.
            SettingsPage::Language => &[],
            // Never actually read — same reason.
            SettingsPage::Telemetry => &[],
            // Never actually read — same reason.
            SettingsPage::ResourcePacks => &[],
        }
    }

    /// The footer buttons, left to right.
    ///
    /// Every page but one is `OptionsSubScreen.addFooter`'s single 200 px Done
    /// (`:51-53`); Accessibility overrides it with a `LinearLayout.horizontal()
    /// .spacing(8)` of two default-width buttons — the external accessibility
    /// guide, then Done (`AccessibilityOptionsScreen.java`).
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
/// **`Eq` went with issue #445**: `ListCell`/`ListHeader` carry an `f32` pixel
/// scroll offset instead of a `u16` entry index, for the reason
/// [`super::key_binds::KeyPlacement`]'s doc gives. `PartialEq` + `Debug` is all
/// `assert_eq!` needs, so nothing that compared two `Placement`s loses anything.
#[derive(Debug, Clone, Copy, PartialEq)]
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
        /// The list's scroll offset in **pixels** (issue #445), not the index of
        /// the entry at the top of the window. See [`entry_offset`].
        scroll: f32,
        /// `0` or `1` — the `addSmall` column.
        column: u8,
    },
    /// A header entry's `StringWidget`.
    ListHeader {
        /// Which page's entry heights to walk.
        page: SettingsPage,
        /// The entry's absolute index.
        entry: u16,
        /// The list's scroll offset in **pixels** (issue #445).
        scroll: f32,
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
/// footer (`HeaderAndFooterLayout.java`).
///
/// `in_world` is only consulted on [`SettingsPage::Root`] — see
/// [`online_cell`] — and is otherwise ignored, exactly as vanilla's `inWorld`
/// only ever changes the one header button.
#[must_use]
pub fn controls(page: SettingsPage, scroll: f32, in_world: bool) -> Vec<Control> {
    let mut out = Vec::new();
    if page == SettingsPage::Root {
        // 1 = the FOV slider, 2 = the Online / World Options button; the title
        // at index 0 is a `StringWidget` and not focusable.
        out.push(Control {
            // Live: an `IntRange(30, 110)` reaching `Sim::set_fov_y_degrees` and
            // the projection matrix. The only live option **not** in a page's
            // `Entry` table, because the root carries it in its own taller header
            // rather than in a list — see `all_controls`, which must stay in step.
            cell: live_slider("fov", "FOV", LiveOption::Fov),
            placement: Placement::Root(1),
            width: SMALL_BUTTON_WIDTH,
        });
        out.push(Control {
            cell: online_cell(in_world),
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

    // **Every** entry, not a `visible_entries(entries, first)` window (#445).
    // The slice had to exclude any entry that did not wholly fit; clipping to the
    // band is `render::draw`'s job now, so a half-scrolled row draws its visible
    // half. `visible_entries` survives only as `LIST_WINDOW_PX`'s own
    // documentation and its tests — nothing in the draw path calls it.
    let entries = page.entries();
    for entry in 0..entries.len() {
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
                    scroll,
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
///
/// See [`controls`]'s doc for what `in_world` does — the same one thing, on
/// the same one page.
#[must_use]
pub fn all_controls(page: SettingsPage, in_world: bool) -> Vec<Cell> {
    if page == SettingsPage::Root {
        // The same live cell `controls` builds; the two must agree or the census
        // and the draw disagree about whether the row is reachable.
        let mut out = vec![
            live_slider("fov", "FOV", LiveOption::Fov),
            online_cell(in_world),
        ];
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
/// (`AbstractSelectionList.java`, `OptionsList.java`); `addHeader`
/// passes `paddingTop + lineHeight + 4` explicitly (`OptionsList.java`),
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
/// `lineHeight * 2` otherwise (`OptionsList.java`).
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
/// `getFirstEntryY() - scrollAmount` (`AbstractSelectionList.java`);
/// snapping the scroll to an entry boundary makes that sum start at `first`.
#[must_use]
pub fn entry_offset(entries: &[Entry], index: usize) -> f32 {
    (0..index).map(|k| entry_height(entries, k)).sum()
}

/// The entries visible with `first` at the top: as many as fit
/// [`LIST_WINDOW_PX`], measured to the **bottom of what each entry draws**
/// rather than to the bottom of the entry box.
///
/// The distinction is 5 px on a control row and it is load-bearing: an entry is
/// 25 px tall but paints a 20 px widget inset 2 px, so the trailing 3 px are
/// blank and excluding a row for them would drop a row that fits.
#[must_use]
/// This page's list, as the generic [`super::widget::ListSpec`] (issue #445).
///
/// **The one screen here with non-uniform rows**, which is why
/// `ScrollList::new_variable` was settled before any conversion started: a header
/// entry is `header_padding_top + HEADER_LINE_HEIGHT + HEADER_PADDING_BOTTOM` and
/// a control row is [`DEFAULT_ITEM_HEIGHT`], so a uniform-pitch list cannot place
/// this page's entries at all. The `heights` table is [`entry_height`]'s own
/// answer per entry, so the scrollbar's thumb, the wheel's clamp and
/// [`list_cell_origin`]'s walk are all reading the same numbers.
///
/// `row_h` stays [`DEFAULT_ITEM_HEIGHT`] even with `heights` set — it is
/// `defaultEntryHeight`, what `scrollRate` is defined against, and **not** "the
/// height of a row". That is what makes one notch `floor(25 / 2)` = 12 px here
/// regardless of which entry happens to be under the cursor.
///
/// The band is [`BIG_BUTTON_WIDTH`] (310) wide and centred, matching
/// [`row_left`]'s `ipx(width) / 2 - ROW_LEFT_INSET` for column 0 — 155 either
/// side of the centre. Gated in this module's tests against `row_left` itself.
#[must_use]
pub fn list_spec(page: SettingsPage, scroll: f32) -> super::widget::ListSpec {
    let entries = page.entries();
    let heights: Vec<f32> = (0..entries.len())
        .map(|i| entry_height(entries, i))
        .collect();
    super::widget::ListSpec::uniform(
        DEFAULT_ITEM_HEIGHT,
        page.header_height(),
        FOOTER_HEIGHT,
        entries.len(),
        BIG_BUTTON_WIDTH,
    )
    .with_heights(heights)
    .at(scroll)
}

/// The entries visible with entry `first` at the top.
///
/// **No longer on the draw path (issue #445)** — `controls` and `settings_frame`
/// emit every entry and let `render::draw` clip to the band. This survives as
/// [`LIST_WINDOW_PX`]'s executable documentation and for the tests that describe
/// the old window budget; nothing that positions a widget calls it.
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
/// `this.screen.width / 2 - 155 + column * 160` (`OptionsList.java`).
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
/// (`OptionsSubScreen.java`, `AbstractSelectionList.java`), so the
/// list's own `getY()` is the header height and everything below is
/// `getFirstEntryY()` + the entry walk + `getContentY()`'s inset.
#[must_use]
pub fn list_cell_origin(
    page: SettingsPage,
    entry: usize,
    scroll: f32,
    column: u8,
    width: f32,
    height: f32,
) -> (f32, f32) {
    let entries = page.entries();
    // Pixel scrolling (#445): the entry's **absolute** offset in the list, minus
    // the scroll. `entry_offset` used to sum from `first`, which made entry
    // `first` sit at offset 0 and skipped its own header padding; summing from 0
    // is vanilla's own absolute layout and the offset is subtracted once here.
    // `scroll.floor()` is vanilla's `(int)scrollAmount`.
    let y = page.header_height()
        + LIST_TOP_INSET
        + entry_offset(entries, entry)
        + ENTRY_CONTENT_INSET
        - drawn_scroll(page, scroll, height).floor();
    (row_left(width, column), y)
}

/// `scroll` re-clamped to what a `height`-tall canvas can justify — vanilla's
/// `refreshScrollAmount`, which `updateSizeAndPosition` runs after every resize
/// (`AbstractSelectionList.java`).
///
/// ## Why this exists, and what it fixed
///
/// [`SettingsNav`] has two writers of its offset and only one of them knows the
/// canvas. The wheel does (`app/lifecycle.rs` resolves a logical canvas for every
/// mouse event) and clamps exactly, through [`SettingsNav::model`]. The
/// **keyboard** does not: a keypress has no canvas, so [`SettingsNav::scroll_to_cursor`]
/// runs against [`crate::config::MIN_SCALED_HEIGHT`] — where the band is
/// `240 - 33 - 33` = 174 and `maxScrollAmount` for the Video page's 500 px of
/// content is **330**. At an 854×480 canvas the band is 414 and the real maximum
/// is **90**. So arrowing to the bottom of the Video page set an offset up to
/// 240 px past that canvas's own end, and the rows were drawn from the raw value
/// while the scrollbar — which goes through `model` — was drawn from the clamped
/// one. Two readers, two different numbers.
///
/// That is the defect a player reported (2026-08-07) as *"some text overlaps, is
/// in the wrong place, and when I scroll it doesn't reach the end"*: the list
/// jumped past its end, the top rows went up behind the header, and the next
/// wheel notch snapped it back. The **Video page is where it bites hardest**
/// because it carries the most controls of any page.
///
/// The clamp is here, in the one place that first learns the canvas, and it goes
/// through the same [`list_spec`] the scrollbar and the wheel use — so the rows,
/// the bar and the clip are three readers of one expression rather than three
/// expressions that agree today.
#[must_use]
pub fn drawn_scroll(page: SettingsPage, scroll: f32, height: f32) -> f32 {
    list_spec(page, scroll)
        .model(height)
        .map_or(scroll, |list| list.scroll())
}

/// The top-left of a header entry's `StringWidget`:
/// `(screen.width / 2 - 155, getContentY() + paddingTop)`
/// (`OptionsList.java`).
#[must_use]
pub fn list_header_origin(
    page: SettingsPage,
    entry: usize,
    scroll: f32,
    width: f32,
    height: f32,
) -> (f32, f32) {
    let (x, y) = list_cell_origin(page, entry, scroll, 0, width, height);
    (x, y + header_padding_top(entry))
}

// -- the arranged layouts --------------------------------------------------

/// A zero-width, 9 px `StringWidget` stand-in.
///
/// The real one is `font.width(text)` wide (`StringWidget.java`), which is
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
            scroll,
            column,
        } => list_cell_origin(page, usize::from(entry), scroll, column, width, height),
        Placement::ListHeader { page, entry, scroll } => {
            list_header_origin(page, usize::from(entry), scroll, width, height)
        }
    }
}

/// The y of a page title's line, inside its header band.
///
/// The band's `FrameLayout` inherits `align(0.5, 0.5)`
/// (`HeaderAndFooterLayout.java`), so a 9 px `StringWidget` in a 33 px
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
///
/// **`Eq`, not just `PartialEq`, since issue #415**: [`Self::language`] holds
/// a real [`super::edit_box::EditBox`] (for the search box), and `EditBox`
/// cannot derive `Eq` — it carries `f32` fields the same way
/// [`super::key_binds::KeyBindsNav`]'s sibling fields do not. `assert_eq!`
/// only needs `PartialEq` + `Debug`, both still here, so nothing that already
/// compared two `SettingsNav`s loses anything.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsNav {
    page: SettingsPage,
    stack: Vec<SettingsPage>,
    cursor: usize,
    /// Scroll offset in **pixels** (issue #445), not the index of the entry at
    /// the top of the window.
    scroll: f32,
    /// Vanilla's `inWorld` (`OptionsScreen.java`): whether this screen
    /// was opened from the pause menu rather than the title —
    /// [`super::UiState::settings_in_world`], threaded in once at
    /// [`Self::reset`]. It governs the root's Online/World Options button's
    /// *liveness and destination*, not only the label
    /// [`super::render::frame_for`] used to swap on its own — see
    /// [`online_cell`]. `MenuNav`'s two Options entry points
    /// (`MainButton::Options`, `PauseButton::Options`) are the only writers,
    /// and they call `reset` in the same statement that calls
    /// [`super::UiState::open_settings`]/
    /// [`super::UiState::open_settings_from_pause`] — so this and
    /// `ui.settings_in_world()` cannot drift apart for as long as the screen
    /// stays open, which is the whole lifetime this field needs to be right
    /// for.
    in_world: bool,
    /// The Key Binds screen's own cursor/scroll/capture state (issue #15) —
    /// live only while [`Self::page`] is [`SettingsPage::KeyBinds`], but kept
    /// unconditionally (like every other field here) rather than boxed away,
    /// since it is a few `usize`s and an `Option<InputAction>`. See
    /// [`super::key_binds::KeyBindsNav`] and [`Self::key_binds`].
    key_binds: super::key_binds::KeyBindsNav,
    /// The Language screen's own cursor/search/filter state (issue #415) —
    /// live only while [`Self::page`] is [`SettingsPage::Language`], kept
    /// unconditionally for the same reason as [`Self::key_binds`].
    language: super::language::LanguageNav,
    /// The Telemetry screen's own cursor (issue #415) — live only while
    /// [`Self::page`] is [`SettingsPage::Telemetry`], kept unconditionally
    /// for the same reason.
    telemetry: super::telemetry::TelemetryNav,
    /// The Resource Packs screen's own cursor (issue #415) — live only
    /// while [`Self::page`] is [`SettingsPage::ResourcePacks`], kept
    /// unconditionally for the same reason.
    packs: super::packs::PacksNav,
}

impl Default for SettingsNav {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsNav {
    /// A fresh cursor at the root page, outside a world.
    #[must_use]
    pub fn new() -> Self {
        Self {
            page: SettingsPage::Root,
            stack: Vec::new(),
            cursor: 0,
            scroll: 0.0,
            in_world: false,
            key_binds: super::key_binds::KeyBindsNav::default(),
            language: super::language::LanguageNav::default(),
            telemetry: super::telemetry::TelemetryNav::default(),
            packs: super::packs::PacksNav::default(),
        }
    }

    /// Borrow the Key Binds screen's own cursor for [`super::render::frame_for`]'s
    /// `SettingsPage::KeyBinds` branch — see [`settings_frame`].
    #[must_use]
    pub fn key_binds(&self) -> &super::key_binds::KeyBindsNav {
        &self.key_binds
    }

    /// Mutably borrow the Key Binds screen's own cursor — `super::nav::MenuNav`'s
    /// `Screen::Settings` input arms use this instead of [`Self::hover_row`]/
    /// [`Self::click_row`]/[`Self::enter`]/[`Self::escape`] whenever
    /// [`Self::page`] is [`SettingsPage::KeyBinds`], the same way `app.rs`
    /// already branches per `Screen` rather than forcing every screen's input
    /// through one shared method. See [`super::key_binds::KeyBindsNav`] for
    /// what it exposes.
    pub fn key_binds_mut(&mut self) -> &mut super::key_binds::KeyBindsNav {
        &mut self.key_binds
    }

    /// Leave [`SettingsPage::KeyBinds`] for whichever page pushed it — always
    /// Controls, since that nav button is the only way here. Exposed
    /// separately from [`Self::escape`]/[`Self::click_row`] because
    /// `KeyBindsOutcome::Back` needs the page-stack pop directly, not a
    /// second interpretation of a [`Cell`] this screen's rows do not have.
    /// Clears any in-progress key capture first, so returning to Controls
    /// then back into Key Binds never resumes mid-capture.
    pub fn leave_key_binds(&mut self) -> SettingsOutcome {
        self.key_binds.reset();
        self.back()
    }

    /// Borrow the Language screen's own cursor — mirrors [`Self::key_binds`].
    #[must_use]
    pub fn language(&self) -> &super::language::LanguageNav {
        &self.language
    }

    /// Mutably borrow the Language screen's own cursor — mirrors
    /// [`Self::key_binds_mut`].
    pub fn language_mut(&mut self) -> &mut super::language::LanguageNav {
        &mut self.language
    }

    /// Leave [`SettingsPage::Language`] for whichever page pushed it — always
    /// Root — mirrors [`Self::leave_key_binds`], resetting the search/cursor
    /// state so re-entering never resumes mid-filter.
    pub fn leave_language(&mut self) -> SettingsOutcome {
        self.language.reset();
        self.back()
    }

    /// Borrow the Telemetry screen's own cursor — mirrors [`Self::key_binds`].
    #[must_use]
    pub fn telemetry(&self) -> &super::telemetry::TelemetryNav {
        &self.telemetry
    }

    /// Mutably borrow the Telemetry screen's own cursor — mirrors
    /// [`Self::key_binds_mut`].
    pub fn telemetry_mut(&mut self) -> &mut super::telemetry::TelemetryNav {
        &mut self.telemetry
    }

    /// Leave [`SettingsPage::Telemetry`] for whichever page pushed it —
    /// always Root — mirrors [`Self::leave_language`].
    pub fn leave_telemetry(&mut self) -> SettingsOutcome {
        self.telemetry.reset();
        self.back()
    }

    /// Borrow the Resource Packs screen's own cursor — mirrors
    /// [`Self::key_binds`].
    #[must_use]
    pub fn packs(&self) -> &super::packs::PacksNav {
        &self.packs
    }

    /// Mutably borrow the Resource Packs screen's own cursor — mirrors
    /// [`Self::key_binds_mut`].
    pub fn packs_mut(&mut self) -> &mut super::packs::PacksNav {
        &mut self.packs
    }

    /// Leave [`SettingsPage::ResourcePacks`] for whichever page pushed it —
    /// always Root — mirrors [`Self::leave_telemetry`].
    pub fn leave_packs(&mut self) -> SettingsOutcome {
        self.packs.reset();
        self.back()
    }

    /// Back to the root with nothing scrolled, and [`Self::in_world`] set —
    /// called when the screen is opened, so re-entering Options does not
    /// resume three pages deep and always re-derives the root's Online/World
    /// Options fork from the entry point that was actually used, rather than
    /// carrying over whatever the previous visit left behind.
    pub fn reset(&mut self, in_world: bool) {
        *self = Self { in_world, ..Self::new() };
    }

    /// Like [`Self::reset`], but lands directly on `page` with an **empty**
    /// page stack, instead of [`SettingsPage::Root`].
    ///
    /// This is vanilla's title-screen icon buttons (`TitleScreen.java`):
    /// the Language/Accessibility icons construct `LanguageSelectScreen`/
    /// `AccessibilityOptionsScreen` directly with `lastScreen = this` (the
    /// title), never routing through `OptionsScreen`. An empty stack is what
    /// makes that faithful rather than approximate: [`Self::back`] pops the
    /// stack and falls through to [`SettingsOutcome::Close`] when it is
    /// empty, so Escape/Done from a page opened this way leaves the settings
    /// screen entirely (straight back to the title, via
    /// [`super::UiState::close_settings`]) instead of surfacing the root grid
    /// first — one Escape, matching vanilla, not two.
    pub fn open_at(&mut self, in_world: bool, page: SettingsPage) {
        self.reset(in_world);
        self.page = page;
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
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// The live [`super::widget::ScrollList`] for the current page at this canvas
    /// height, or `None` when there is nothing to scroll.
    #[must_use]
    fn model(&self, canvas_height: f32) -> Option<super::widget::ScrollList> {
        list_spec(self.page, self.scroll).model(canvas_height)
    }

    /// One mouse-wheel notch, through the primitive. Positive scrolls **up**;
    /// the negation lives in [`super::widget::ScrollList::mouse_scrolled`].
    pub fn scroll_by(&mut self, notches: f32, canvas_height: f32) {
        let Some(mut list) = self.model(canvas_height) else {
            return;
        };
        list.mouse_scrolled(notches);
        self.scroll = list.scroll();
    }

    /// The controls actually on screen, with their placements — what
    /// [`settings_frame`] draws and what a row index from `app.rs`'s hit-test
    /// indexes into.
    #[must_use]
    pub fn visible(&self) -> Vec<Control> {
        controls(self.page, self.scroll, self.in_world)
    }

    /// The cursor's position **within [`Self::visible`]**, i.e. the row index
    /// `MenuFrame::selected` wants. `None` when the cursor is off-window, which
    /// [`Self::scroll_to_cursor`] makes impossible in practice.
    #[must_use]
    pub fn selected_row(&self) -> Option<usize> {
        let all = all_controls(self.page, self.in_world);
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
        let len = all_controls(self.page, self.in_world).len();
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

    /// `AbstractSelectionList.scrollToEntry`, through
    /// [`super::widget::ScrollList::scroll_to_entry`] (issue #445) — bring the
    /// cursor's entry into the band, moving the **minimum number of pixels**.
    ///
    /// Was a `while !visible_entries(entries, self.first).contains(&entry) {
    /// self.first += 1 }` walk at *entry* granularity, so it moved a whole entry
    /// height at a time — and on this screen entry heights differ (a header is
    /// `padding_top + line + padding_bottom`, a control row is
    /// [`DEFAULT_ITEM_HEIGHT`]), which made the step size depend on which entry
    /// happened to be at the top. `scroll_to_entry` works in pixels against the
    /// same `heights` table [`list_spec`] declares, so it cannot disagree with the
    /// draw.
    ///
    /// [`crate::config::MIN_SCALED_HEIGHT`] rather than the live canvas, for the
    /// reason `stats::step` records: a keypress has no canvas in hand.
    fn scroll_to_cursor(&mut self) {
        let Some(entry) = entry_of_control(self.page, self.cursor) else {
            return;
        };
        let Some(mut list) = self.model(crate::config::MIN_SCALED_HEIGHT as f32) else {
            return;
        };
        list.scroll_to_entry(entry);
        self.scroll = list.scroll();
    }

    /// Puts the cursor on the control at visible row `row` — the mouse's half.
    ///
    /// A visible row is resolved back to an index into [`all_controls`] by
    /// matching the *cell* **and** its entry: a cell alone is not unique across a
    /// page in principle, and an index that drifted by one is precisely the #391
    /// failure mode one screen over.
    pub fn hover_row(&mut self, row: usize) {
        let page = self.page;
        let visible = controls(page, self.scroll, self.in_world);
        let Some(control) = visible.get(row).copied() else {
            return;
        };
        let entry = match control.placement {
            Placement::ListCell { entry, .. } => Some(usize::from(entry)),
            _ => None,
        };
        let all = all_controls(page, self.in_world);
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
        let all = all_controls(self.page, self.in_world);
        match all.get(self.cursor).copied() {
            Some(cell) => self.activate(cell),
            None => SettingsOutcome::None,
        }
    }

    /// Escape: unwind one page, or ask to leave the tree from the root.
    ///
    /// `Screen.shouldCloseOnEsc` is true for every options screen, and
    /// `OptionsSubScreen.onClose` returns to `lastScreen`
    /// (`OptionsSubScreen.java`) — which is the page stack here.
    pub fn escape(&mut self) -> SettingsOutcome {
        self.back()
    }

    fn back(&mut self) -> SettingsOutcome {
        match self.stack.pop() {
            Some(page) => {
                self.page = page;
                self.cursor = 0;
                self.scroll = 0.0;
                SettingsOutcome::None
            }
            None => SettingsOutcome::Close,
        }
    }

    /// The live slider option at visible row `row`, if that row is one.
    ///
    /// The mouse-**drag** half of a slider (vanilla's
    /// `AbstractSliderButton.onDrag` → `setValueFromMouse`), which this screen
    /// had no equivalent of at all: `click_row` routed every slider through
    /// `activate` → `SettingsOutcome::Cycle`, i.e. one wrapping step per click.
    /// That is why a slider "moved a tiny bit on click" instead of following the
    /// cursor.
    ///
    /// Resolves the row against [`Self::visible`] exactly as [`Self::click_row`]
    /// does, rather than indexing `all_controls`, for the #391 reason recorded
    /// there.
    ///
    /// `None` for a row that is not a live slider — which is what makes the
    /// caller fall back to the click path rather than swallowing the click.
    #[must_use]
    pub fn slider_row_option(&self, row: usize) -> Option<LiveOption> {
        let control = self.visible().get(row).copied()?;
        let Cell::Option(spec) = control.cell else {
            return None;
        };
        if spec.widget != OptionWidget::Slider || !control.cell.is_live() {
            return None;
        }
        spec.live
    }

    /// The one place a control's activation is interpreted.
    ///
    /// An **inactive** control does nothing at all, which is
    /// `AbstractWidget.mouseClicked`'s `isActive()` guard
    /// (`AbstractWidget.java`) and the same shape as
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
                self.scroll = 0.0;
                // A fresh `KeyBindsNav` on every entry, matching vanilla
                // building a new `KeyBindsScreen` each time — the same rule
                // `reset` already applies to the outer cursor, one page
                // deeper. Harmless to run when `page` is not `KeyBinds`: the
                // field just sits at its default until it is.
                if page == SettingsPage::KeyBinds {
                    self.key_binds.reset();
                }
                if page == SettingsPage::Language {
                    self.language.reset();
                }
                if page == SettingsPage::Telemetry {
                    self.telemetry.reset();
                }
                if page == SettingsPage::ResourcePacks {
                    self.packs.reset();
                }
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
/// The root header's Online/World Options fork (`OptionsScreen.java`) is
/// **not** decided here: `nav.visible()` already carries the right label *and*
/// the right liveness for [`super::UiState::settings_in_world`], because
/// [`SettingsNav::in_world`] was set from the same fact when the screen was
/// opened. This function used to carry a second, draw-only copy of that fork
/// (`in_world: bool` swapping only the label); it was deleted because a fact
/// declared in two places is exactly the fabrication class the module docs'
/// departure (1) exists to avoid — a click and a label agreeing by
/// construction rather than by two authors remembering to agree.
#[must_use]
pub fn settings_frame(
    nav: &SettingsNav,
    options: &crate::config::Options,
    save_error: Option<&str>,
) -> MenuFrame<'static> {
    // `SettingsPage::KeyBinds` (issue #15) is not an `OptionsList` page — see
    // that variant's own doc — so it builds its frame in a different module
    // entirely rather than falling through the `Cell`/`Control` path below.
    // The error label is appended here rather than in `key_binds::frame`
    // itself so there is exactly one place in this crate that knows how to
    // draw a save-error line, not two copies that could drift.
    if nav.page() == SettingsPage::KeyBinds {
        let mut frame = super::key_binds::frame(nav.key_binds(), &options.keybinds);
        if let Some(error) = save_error {
            frame.labels.push(MenuLabel {
                text: error.to_string(),
                origin: Origin::ScreenBottom,
                dx: 0.0,
                dy: -(FOOTER_HEIGHT + HEADER_LINE_HEIGHT + 2.0),
                align: Align::Centre,
                colour: ERROR_COLOUR,
                scale: 1.0,
            });
        }
        return frame;
    }
    // `SettingsPage::Language` (issue #415) is not an `OptionsList` page
    // either — same reason and same shape as the `KeyBinds` branch above.
    if nav.page() == SettingsPage::Language {
        let mut frame = super::language::frame(nav.language());
        if let Some(error) = save_error {
            frame.labels.push(MenuLabel {
                text: error.to_string(),
                origin: Origin::ScreenBottom,
                dx: 0.0,
                dy: -(FOOTER_HEIGHT + HEADER_LINE_HEIGHT + 2.0),
                align: Align::Centre,
                colour: ERROR_COLOUR,
                scale: 1.0,
            });
        }
        return frame;
    }
    // `SettingsPage::Telemetry` (issue #415) — same shape again. No
    // save-error line: this page persists nothing, so `save_error` cannot
    // fire for it, but the branch is spelled out the same way rather than
    // silently dropping a future error this page never expects.
    if nav.page() == SettingsPage::Telemetry {
        return super::telemetry::frame(nav.telemetry());
    }
    // `SettingsPage::ResourcePacks` (issue #415) — same shape again.
    if nav.page() == SettingsPage::ResourcePacks {
        return super::packs::frame(nav.packs());
    }
    let page = nav.page();
    let visible = nav.visible();
    let selected = nav.selected_row();

    let mut rows: Vec<MenuRow> = visible
        .iter()
        .map(|control| MenuRow {
            label: control.cell.label(options),
            enabled: control.cell.is_live(),
            slider: control.cell.is_slider(),
            slider_value: control.cell.slider_fraction(options),
            slot: Some(control.slot()),
            // `AbstractWidget.setTooltip`, from the option's own
            // `TooltipSupplier` — see `Cell::tooltip`. Stamped on every row
            // uniformly, so which rows have text is the table's answer and not the
            // frame builder's: a renderer wired for only some rows is the failure
            // mode worth naming, and this is the line that prevents it.
            tooltip: control.cell.tooltip().map(str::to_string),
            ..Default::default()
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
        // **Not `title_y(page)` unconditionally — that was the bug.** A player
        // report (2026-08-04, "the 'Options' text at the top is intersecting
        // some buttons") traced to exactly this line double-counting the
        // root's title y: `Origin::Settings(Placement::Root(0))`'s anchor is
        // already the arranged, **absolute** position `root_widget_rects`
        // put the title at (12 px, per `the_title_sits_in_its_band_on_every_
        // page`), and `build`'s draw adds `dy` *on top of* the anchor
        // (`y = ay + label.dy`, `render.rs`'s label loop). Adding
        // `title_y(Root)` — also `12.0` — on top of that anchor drew the
        // title at absolute `y = 24`, four pixels into the FOV/Online row's
        // own `y = 29` (`the_root_title_is_centred_on_the_header_block`'s
        // sibling assertions give that row's rect directly). `Origin::
        // ScreenTop`'s anchor is `0.0`, so every other page's `dy` genuinely
        // has to carry the whole offset — only Root's anchor already does,
        // because only Root's title comes from a real arranged layout tree
        // rather than a bare screen-top anchor.
        dy: if page == SettingsPage::Root {
            0.0
        } else {
            title_y(page)
        },
        align: Align::Centre,
        colour: HEADER_COLOUR,
        scale: 1.0,
    }];
    // `OptionsList.HeaderEntry`'s `StringWidget`s.
    let entries = page.entries();
    let mut list_labels = Vec::new();
    // Every header, clipped to the band by `render::draw` (#445). These go in
    // `list_labels` and not `labels`: they scroll, and a free text label has
    // nowhere else to carry a clip rect, so in `labels` a scrolled-away header
    // would draw over the footer. The title above does not scroll and stays.
    for entry in 0..entries.len() {
        if let Entry::Header(text) = entries[entry] {
            list_labels.push(MenuLabel {
                text: text.to_string(),
                origin: Origin::Settings(Placement::ListHeader {
                    page,
                    entry: entry as u16,
                    scroll: nav.scroll(),
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
        list_labels,
        // `list` is deliberately not set: `render::dispatch` stamps
        // `f.list = nav.active_list(ui)`, so the bar the draw paints and the
        // offset the wheel clamps stay two readers of one declaration.
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every page, so a sweep cannot silently miss one.
    const PAGES: [SettingsPage; 9] = [
        SettingsPage::Root,
        SettingsPage::Video,
        SettingsPage::Controls,
        SettingsPage::Mouse,
        SettingsPage::Sound,
        SettingsPage::Chat,
        SettingsPage::Accessibility,
        SettingsPage::Skin,
        SettingsPage::Online,
    ];

    /// `all_controls`/`controls` need an `in_world` bool wherever the census
    /// does not care about the root's Online/World Options fork — every page
    /// but `Root` ignores it outright, and even on `Root` the *count* (not the
    /// liveness) is the same either way, so `false` (the title-screen entry,
    /// matching #55's original, still-authoritative baseline) is the one to
    /// sweep with here.
    const OUTSIDE_A_WORLD: bool = false;

    #[test]
    fn the_per_screen_control_counts_are_the_censused_ones() {
        // The expected values originate **outside** this file: they are the
        // `addBig`/`addSmall` call-site counts in #55's census comment and
        // `docs/ui-framework.md`, which were counted from the jar. A table edit
        // that drops or duplicates a control fails here and names the screen.
        //
        // Each figure counts focusable widgets including the page's own footer,
        // which is how the census counted the root's Done. Online's 8 is
        // `OnlineOptionsScreen.java`'s seven controls (`:85-116`) plus its Done.
        let expected = [
            (SettingsPage::Root, 13),
            (SettingsPage::Video, 32),
            (SettingsPage::Controls, 10),
            (SettingsPage::Mouse, 8),
            (SettingsPage::Sound, 17),
            (SettingsPage::Chat, 19),
            (SettingsPage::Accessibility, 27),
            (SettingsPage::Skin, 9),
            (SettingsPage::Online, 8),
        ];
        for (page, count) in expected {
            assert_eq!(
                all_controls(page, OUTSIDE_A_WORLD).len(),
                count,
                "{page:?} should carry {count} controls"
            );
        }
        // 143 across the nine pages.
        let total: usize = PAGES
            .iter()
            .map(|&p| all_controls(p, OUTSIDE_A_WORLD).len())
            .sum();
        assert_eq!(total, 143, "13+32+10+8+17+19+27+9+8");
    }

    #[test]
    fn the_disabled_majority_is_the_point_and_it_is_measured() {
        // The whole issue is that most rows are present-and-inactive. This
        // asserts the *ratio*, so a change that quietly enabled a row it does
        // not honour has to say so here.
        let mut live = Vec::new();
        let mut total = 0;
        for page in PAGES {
            for cell in all_controls(page, OUTSIDE_A_WORLD) {
                total += 1;
                if cell.is_live() {
                    live.push((page, cell));
                }
            }
        }
        assert_eq!(total, 143);
        // The live *options*, in page order (`PAGES`) and then declaration
        // order within each page — the persisted fields of `config::Options`
        // besides `keybinds`.
        //
        // **Three appear twice**, and that is vanilla's own shape rather than a
        // duplicate row: `textBackgroundOpacity`, `chatOpacity` and
        // `chatLineSpacing` are one `OptionInstance` each, placed on *both*
        // `ChatOptionsScreen` and `AccessibilityOptionsScreen`
        // (`Options.java`, and the two screens' own option arrays). Both
        // rows drive the same `config::Options` field, so editing either moves
        // the other's label too — which is why `LiveOption` is keyed by the
        // option and not by the row.
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
                // Root page, in its own taller header rather than a list: the FOV
                // slider, whose consumer had been pinned to `FOV_Y_DEGREES` (which
                // *is* vanilla's 70, so no screenshot could show the difference at
                // the default).
                LiveOption::Fov,
                // Video page, `VIDEO`'s first `pair`: Max Framerate then VSync.
                LiveOption::FramerateLimit,
                LiveOption::EnableVsync,
                // Second `pair`: Reduce FPS When, then GUI Scale.
                LiveOption::InactivityFpsLimit,
                LiveOption::GuiScale,
                // The Quality & Performance grid's own `big` row, before the
                // pairs it can write three of — see `MenuNav::apply_graphics_preset`.
                LiveOption::GraphicsPreset,
                // Video page, on the Quality & Performance grid, next to the
                // (still inactive) Biome Blend.
                LiveOption::RenderDistance,
                // Also Video, in the `(ambientOcclusion, cloudStatus)` pair: the
                // three-state Clouds cycle, whose `SkyFrame::with_cloud_status`
                // consumer had zero production callers.
                LiveOption::CloudStatus,
                // Also Video, in the `(particles, mipmapLevels)` pair, sorting
                // between Clouds and See-Through Leaves: the block-atlas
                // mip-depth slider, whose consumer used to be the frozen
                // `BLOCK_ATLAS_MIP_LEVELS` constant.
                LiveOption::MipmapLevels,
                // Also Video, the `(entityShadows, entityDistanceScaling)` pair's
                // first half — owner report: "entity shadows are missing".
                LiveOption::EntityShadows,
                // The `(menuBackgroundBlurriness, cloudRange)` pair's first
                // half, which is the row **after** Entity Shadows: the blur
                // radius, whose pass ran at a frozen `BLUR_RADIUS`. It appears a
                // second time further down, on Accessibility, exactly as vanilla
                // places it.
                LiveOption::MenuBackgroundBlurriness,
                // The `(cutoutLeaves, improvedTransparency)` pair's first half —
                // the leaves-render-pass fix's own row.
                LiveOption::CutoutLeaves,
                // The Quality & Performance grid's last row, a `lone`: the
                // rain/snow column radius, whose consumer already took one and
                // was handed `DEFAULT_WEATHER_RADIUS`.
                LiveOption::WeatherRadius,
                // The Video page's Preferences header block, in the
                // `(attackIndicator, chunkSectionFadeInTime)` pair: the
                // three-state indicator cycle, whose crosshair half already drew
                // pinned to vanilla's CROSSHAIR.
                LiveOption::AttackIndicator,
                LiveOption::ToggleSneak,
                LiveOption::ToggleSprint,
                LiveOption::ToggleAttack,
                LiveOption::ToggleUse,
                // #444 completes the Controls page's toggle rows: Auto-Jump,
                // then the Sprint Window slider (both declared after the four
                // toggles in `CONTROLS`' third `pair`).
                LiveOption::AutoJump,
                LiveOption::SprintWindow,
                // Mouse page: look Sensitivity is the #443 migration, and it
                // is declared *before* Scroll Sensitivity in `MOUSE`'s first
                // `pair`, which is why it sorts here.
                LiveOption::Sensitivity,
                LiveOption::MouseWheelSensitivity,
                // Discrete Scrolling, issue #444 — the first item of `MOUSE`'s
                // second `pair`, so it sorts between Scroll Sensitivity and
                // Invert Mouse X.
                LiveOption::DiscreteMouseScroll,
                LiveOption::InvertMouseX,
                LiveOption::InvertMouseY,
                // Sound page: the eleven volume buses in `SoundSource` declaration
                // order, MASTER first because the page pulls it into its own
                // `addBig` row. The index *is* the ordinal — see
                // `sound_rows_index_the_category_they_name`, which is what stops a
                // transposed pair here reading as correct.
                LiveOption::SoundVolume(0),
                LiveOption::SoundVolume(1),
                LiveOption::SoundVolume(2),
                LiveOption::SoundVolume(3),
                LiveOption::SoundVolume(4),
                LiveOption::SoundVolume(5),
                LiveOption::SoundVolume(6),
                LiveOption::SoundVolume(7),
                LiveOption::SoundVolume(8),
                LiveOption::SoundVolume(9),
                LiveOption::SoundVolume(10),
                // Still Sound: Closed Captions, issue #198. Vanilla places
                // `showSubtitles` on *both* the Sound and Accessibility screens,
                // so it appears twice below, like the three chat options do.
                LiveOption::ShowSubtitles,
                // Chat page, in `ChatOptionsScreen.options` order.
                LiveOption::ChatColors,
                LiveOption::ChatOpacity,
                LiveOption::TextBackgroundOpacity,
                LiveOption::ChatScale,
                LiveOption::ChatLineSpacing,
                LiveOption::ChatWidth,
                LiveOption::ChatHeightFocused,
                LiveOption::ChatHeightUnfocused,
                // Accessibility page: Closed Captions (again), the three shared
                // with Chat, then View Bobbing.
                LiveOption::ShowSubtitles,
                // The Accessibility page's own copy of the Video row above —
                // one `OptionInstance`, two placements, like the three chat
                // sliders that follow it here.
                LiveOption::MenuBackgroundBlurriness,
                LiveOption::TextBackgroundOpacity,
                LiveOption::ChatOpacity,
                LiveOption::ChatLineSpacing,
                LiveOption::ViewBobbing,
                // Also Accessibility, further down the page: the camera tilt whose
                // consumer `app/redraw.rs` had honoured all along while the row
                // drew from the frozen `UNIT_DOUBLE_DEFAULTS`, and the title-screen
                // spin rate whose consumer (`PanoramaRenderer::set_speed`) had no
                // caller at all. See both variants' docs.
                LiveOption::DamageTiltStrength,
                // The glint pair, declared immediately after Damage Tilt in
                // `ACCESSIBILITY` and before the Panorama row.
                LiveOption::GlintSpeed,
                LiveOption::GlintStrength,
                LiveOption::PanoramaSpeed,
            ],
            "FOV on the root; GUI Scale, Render Distance, Clouds, Mipmap Levels, \
             Entity Shadows, Menu Background Blur, Weather Effect Radius and Attack \
             Indicator on Video; \
             the four toggle \
             rows and Auto-Jump/Sprint \
             Window on Controls; look sensitivity, scroll sensitivity and both \
             inverts on Mouse; the eleven volume buses and Closed Captions on \
             Sound; the eight chat options on Chat with three of them repeated on \
             Accessibility; Closed Captions again, View Bobbing, Damage Tilt, \
             both glint sliders and Panorama Scroll Speed on Accessibility — and \
             nothing else"
        );
        // The control: an option we do not persist must report itself inactive,
        // and the detector must be able to tell the difference.
        //
        // This used to use `renderDistance`, which issue #443 made live — so the
        // control's *premise* expired, and it is worth naming that it would have
        // kept passing anyway: `slider()` builds a cell with `live: None`
        // regardless of what the real row on the page carries, so it was
        // asserting a property of the constructor rather than of the tree.
        // `simulationDistance` is a real still-inactive row (this client has no
        // simulation-distance consumer), and going through `all_controls` is what
        // makes it a claim about the page.
        let sim_distance = PAGES
            .iter()
            .flat_map(|&p| all_controls(p, OUTSIDE_A_WORLD))
            .find(|c| matches!(c, Cell::Option(s) if s.accessor == "simulationDistance"))
            .expect("the Video page still carries a Simulation Distance row");
        assert!(
            !sim_distance.is_live(),
            "simulationDistance has no consumer in this shell, so its row must \
             stay inactive — a live one would be fabricated persistence"
        );
        // And the same predicate, read off the real tree, must answer true for a
        // row that *is* live — otherwise `is_live` could be stuck at `false`.
        let render_distance = PAGES
            .iter()
            .flat_map(|&p| all_controls(p, OUTSIDE_A_WORLD))
            .find(|c| matches!(c, Cell::Option(s) if s.accessor == "renderDistance"))
            .expect("the Video page carries a Render Distance row");
        assert!(
            render_distance.is_live(),
            "renderDistance is a persisted `Options` field since #443"
        );
        // The count itself, not just the ratio's ingredients: 55 live option
        // *rows* (50 distinct options, **five** of them placed twice — the three
        // Chat/Accessibility sliders, `showSubtitles` on Sound and
        // Accessibility, and now `menuBackgroundBlurriness` on Video and
        // Accessibility, so 55 - 5 == 50 — the video-settings/leaves session's
        // five, framerateLimit/enableVsync/inactivityFpsLimit/graphicsPreset/
        // cutoutLeaves, plus mipmapLevels, entityShadows, weatherRadius and
        // attackIndicator, are each placed once)
        // + 9 Done buttons (one per page, always live) + 13 working nav buttons
        // (Skin/Sound/Video/Controls/Chat/Accessibility/**Language**/
        // **Telemetry**/**Resource Packs** from the root grid,
        // Accessibility -> Controls, Controls -> Mouse, Controls -> Key Binds,
        // and the root's own Online button, live outside a world).
        // A change that adds or removes a live row anywhere must say so here.
        assert_eq!(live.len(), 77, "outside a world: {live:?}");
    }

    /// The companion to [`the_disabled_majority_is_the_point_and_it_is_measured`]:
    /// the same sweep, mid-session (`in_world == true`). The census (143) is
    /// identical — `SettingsPage::Online` and its own Done exist either way —
    /// but the live count drops by exactly one, because the root's header
    /// button is the (unbuilt) World Options fork instead of a live link to
    /// it. This is the test that would have failed had the Online page been
    /// wired live in both directions.
    #[test]
    fn the_root_online_button_is_the_one_row_that_changes_with_in_world() {
        let outside: Vec<Cell> = PAGES
            .iter()
            .flat_map(|&p| all_controls(p, false))
            .filter(|c| c.is_live())
            .collect();
        let inside: Vec<Cell> = PAGES
            .iter()
            .flat_map(|&p| all_controls(p, true))
            .filter(|c| c.is_live())
            .collect();
        // 73, not 62: `showSubtitles` is live on **both** the pages vanilla places
        // it on (Sound and Accessibility), and three chat options are on two pages
        // each. The kind A batch added fifteen — the eleven volume buses, the
        // root's FOV, both glint sliders and Clouds — the video-settings/
        // leaves session added five more: framerateLimit, enableVsync,
        // inactivityFpsLimit, graphicsPreset, cutoutLeaves — the block-atlas
        // mip-depth session added a sixth: mipmapLevels — and this session added
        // a seventh: entityShadows, an eighth: weatherRadius, a ninth:
        // attackIndicator, and
        // `menuBackgroundBlurriness`, which is **two** rows (Video and
        // Accessibility) for one option.
        assert_eq!(outside.len(), 77);
        assert_eq!(inside.len(), 76, "one fewer: the root's Online button");
        assert!(
            outside.contains(&nav("Online...", SettingsPage::Online)),
            "outside a world the root links to Online"
        );
        assert!(
            !inside.contains(&nav("Online...", SettingsPage::Online)),
            "inside a world it must not"
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
        // `particles` is still genuinely inactive on this tree (unlike
        // `entityShadows`, which used to be the example here before it went
        // live — see `LiveOption::EntityShadows`).
        assert_eq!(cycle("particles", "Particles").label(&options), "Particles");
    }

    /// Owner report: a settings row showing the value with no name at all —
    /// "Fancy" where vanilla shows a composed "`<name>`: Fancy", "AFK" where
    /// vanilla shows the full label. Swept over **every** live row on the
    /// real tree rather than the two named examples, because the report
    /// describes a *class* of bug (a value-only format string covering a
    /// whole family of rows), and a fixture that only checks the two
    /// examples named in the report cannot see a sibling instance.
    ///
    /// A fixture whose option name is empty or equals its own value cannot
    /// see this bug either — `assert_ne!(spec.caption, value, ..)` below is
    /// exactly that guard, checked on every row this sweep exercises rather
    /// than trusted once.
    #[test]
    fn every_live_row_carries_both_its_name_and_its_value_or_is_a_named_exception() {
        let options = crate::config::Options::default();
        let mut composing_rows_checked = 0;
        let mut bare_rows_checked = 0;
        for page in PAGES {
            for cell in all_controls(page, OUTSIDE_A_WORLD) {
                let Cell::Option(spec) = cell else { continue };
                let Some(live) = spec.live else { continue };
                let label = cell.label(&options);
                let value = live_value(live, &options);
                if live.value_is_the_whole_label() {
                    // The two named vanilla exceptions
                    // (`LiveOption::value_is_the_whole_label`'s own doc):
                    // the row's *entire* label is the value, and that is
                    // vanilla's own stringifier, not a bug — but it must
                    // still equal the real value, not a caption left over
                    // from a composing code path.
                    assert_eq!(
                        label, value,
                        "{live:?}: the whole-label exception must still show the real \
                         value, not a stale caption"
                    );
                    bare_rows_checked += 1;
                } else {
                    // The guard the report's own two examples would have
                    // passed without: a caption that is empty, or that
                    // happens to equal its own value, cannot distinguish
                    // "composed correctly" from "value only".
                    assert!(
                        !spec.caption.is_empty(),
                        "{live:?}: empty caption — this row cannot prove anything about \
                         name+value composition"
                    );
                    assert_ne!(
                        spec.caption, value,
                        "{live:?}: caption and value coincide ({value:?}) — this fixture \
                         cannot see a value-only regression here; pick a state where they differ"
                    );
                    assert!(
                        label.starts_with(spec.caption),
                        "{live:?}: label {label:?} does not start with its own caption \
                         {:?} — a value-only row, exactly the reported bug",
                        spec.caption
                    );
                    assert!(
                        label.ends_with(value.as_str()),
                        "{live:?}: label {label:?} does not end with its own value {value:?}"
                    );
                    assert!(
                        label.contains(": "),
                        "{live:?}: label {label:?} does not compose \"name: value\" — \
                         a value-only row, exactly the reported bug"
                    );
                    composing_rows_checked += 1;
                }
            }
        }
        // The control this sweep's own coverage needs: it must actually have
        // exercised rows of both shapes, or a change that made every row
        // fall into one branch would pass vacuously.
        assert_eq!(
            bare_rows_checked, 3,
            "expected exactly the three named whole-label exceptions (CloudStatus, \
             InactivityFpsLimit, AttackIndicator), each placed once on the Video page; \
             got {bare_rows_checked}"
        );
        assert!(
            composing_rows_checked >= 40,
            "expected most live rows to compose name+value; only {composing_rows_checked} did, \
             which is too few to be a meaningful sweep"
        );
    }

    /// Every chat option's label, predicted from vanilla's own stringifier and
    /// asserted as an exact string.
    ///
    /// Each expectation originates outside this client: the pixel figures are
    /// `ChatComponent.getWidth`/`getHeight` (`ChatComponent.java`), the
    /// percentages are `Options.percentValueLabel`'s `(int)(value * 100.0)`
    /// truncation (`Options.java`), and `chatScale`'s OFF branch is
    /// `CommonComponents.optionStatus(caption, false)` (`Options.java`).
    ///
    /// The load-bearing row is `chatOpacity`, which is **affine**:
    /// `percentValueLabel(caption, value * 0.9 + 0.1)` (`Options.java`). The
    /// wrong-but-plausible transcription — a plain percent — agrees with the
    /// correct one at `1.0` and nowhere else, so this pins two more values where
    /// the two hypotheses differ by 10 and 5 percentage points. That is the
    /// *magnitude* discrimination a direction-only assertion would miss.
    #[test]
    fn every_chat_options_label_is_vanillas_own_string() {
        let mut o = crate::config::Options::default();

        // Percent sliders. `chat_line_spacing`'s default is 0.0, and `0%` is a
        // real value here rather than an OFF caption — vanilla gives
        // `chatLineSpacing` a plain `percentValueLabel` with no OFF branch.
        let spacing = live_slider("chatLineSpacing", "Line Spacing", LiveOption::ChatLineSpacing);
        assert_eq!(spacing.label(&o), "Line Spacing: 0%");
        o.chat_line_spacing = 0.25;
        assert_eq!(spacing.label(&o), "Line Spacing: 25%");

        let bg = live_slider(
            "textBackgroundOpacity",
            "Text Background Opacity",
            LiveOption::TextBackgroundOpacity,
        );
        assert_eq!(bg.label(&o), "Text Background Opacity: 50%", "default 0.5");

        // The affine one, and the whole reason this test asserts three values
        // rather than one.
        let opacity = live_slider("chatOpacity", "Chat Text Opacity", LiveOption::ChatOpacity);
        assert_eq!(opacity.label(&o), "Chat Text Opacity: 100%", "1.0 -> 100%");
        o.chat_opacity = 0.0;
        assert_eq!(
            opacity.label(&o),
            "Chat Text Opacity: 10%",
            "0.0 -> 10%, NOT 0%: vanilla's chat text is never fully transparent. \
             A plain-percent transcription would say 0% here"
        );
        o.chat_opacity = 0.5;
        assert_eq!(
            opacity.label(&o),
            "Chat Text Opacity: 55%",
            "0.5 -> 55%, not 50% — the affine map again"
        );

        // `chatScale`, the one chat slider with an OFF caption.
        let scale = live_slider("chatScale", "Chat Text Size", LiveOption::ChatScale);
        assert_eq!(scale.label(&o), "Chat Text Size: 100%");
        o.chat_scale = 0.0;
        assert_eq!(
            scale.label(&o),
            "Chat Text Size: OFF",
            "`optionStatus(caption, false)`, not `0%`"
        );

        // The pixel sliders. 40..=320 for width, 20..=180 for both heights.
        let width = live_slider("chatWidth", "Width", LiveOption::ChatWidth);
        assert_eq!(width.label(&o), "Width: 320px", "1.0 -> floor(280 + 40)");
        o.chat_width = 0.0;
        assert_eq!(width.label(&o), "Width: 40px");
        o.chat_width = 0.5;
        assert_eq!(width.label(&o), "Width: 180px", "floor(140 + 40)");

        let focused = live_slider(
            "chatHeightFocused",
            "Focused Height",
            LiveOption::ChatHeightFocused,
        );
        assert_eq!(focused.label(&o), "Focused Height: 180px", "1.0 -> 160 + 20");
        let unfocused = live_slider(
            "chatHeightUnfocused",
            "Unfocused Height",
            LiveOption::ChatHeightUnfocused,
        );
        assert_eq!(
            unfocused.label(&o),
            "Unfocused Height: 90px",
            "`defaultUnfocusedPct() == 70/160 == 0.4375` -> floor(70 + 20)"
        );

        // The one cycle among them.
        let colors = live_cycle("chatColors", "Colors", LiveOption::ChatColors);
        assert_eq!(colors.label(&o), "Colors: ON", "vanilla's default is true");
        o.chat_colors = false;
        assert_eq!(colors.label(&o), "Colors: OFF");
    }

    /// A live `UnitDouble` slider's handle must sit at the **persisted** value,
    /// not at the frozen boot default in [`UNIT_DOUBLE_DEFAULTS`].
    ///
    /// This is the assertion that separates a wired slider from a decorative
    /// one. Before the chat options were live, `slider_fraction` fell through to
    /// the default table for all of them, so the handle was pinned at vanilla's
    /// boot value forever — a control that moved the chat and then lied about
    /// its own state. The two hypotheses differ by construction here: the stored
    /// value is set to something that is *not* the default.
    #[test]
    fn a_live_unit_double_sliders_handle_tracks_the_stored_value() {
        let mut o = crate::config::Options::default();
        let width = live_slider("chatWidth", "Width", LiveOption::ChatWidth);

        // `UnitDouble.toSliderValue` is the identity, so the fraction *is* the
        // value — no range to port, which is why this needed no part of #424.
        assert_eq!(width.slider_fraction(&o), Some(1.0), "the default, 1.0");
        o.chat_width = 0.3;
        assert_eq!(
            width.slider_fraction(&o),
            Some(0.3),
            "the handle must follow the stored value; the frozen default (1.0) \
             is the wrong-hypothesis value this distinguishes from"
        );

        // The control: an **inactive** UnitDouble slider still falls through to
        // the default table, so the mechanism above is a live-value lookup and
        // not "slider_fraction now returns whatever it is handed".
        let master = slider("soundSource.master", "Master Volume");
        o.chat_width = 0.0;
        assert_eq!(
            master.slider_fraction(&o),
            Some(1.0),
            "an inactive slider keeps vanilla's boot default"
        );

        // And a corrupt on-disk value must be clamped onto the track rather
        // than drawing a handle off the widget.
        o.chat_width = 7.5;
        assert_eq!(width.slider_fraction(&o), Some(1.0));
        o.chat_width = -3.0;
        assert_eq!(width.slider_fraction(&o), Some(0.0));
    }

    // -- issue #443: the migrated options reach the screen -------------------

    /// The two migrated rows draw their handle from the **persisted** value and
    /// their label from vanilla's own stringifier.
    ///
    /// The frozen-default fraction is the wrong hypothesis in both cases, and it
    /// is computed here rather than described, so a regression that dropped the
    /// live arm would land on it and be caught.
    #[test]
    fn the_migrated_rows_follow_the_persisted_value_not_the_frozen_default() {
        let mut o = crate::config::Options::default();

        // -- renderDistance: an IntRange, so via SliderRange.
        let rd = live_slider(
            "renderDistance",
            "Render Distance",
            LiveOption::RenderDistance,
        );
        let range = SliderRange { min: 2, max: 32 };
        o.render_distance = 8;
        assert_eq!(
            rd.slider_fraction(&o),
            Some(range.to_slider_value(8)),
            "the handle must follow the stored chunk count"
        );
        // The wrong hypothesis: the table's frozen default of 12.
        assert_ne!(
            rd.slider_fraction(&o),
            Some(range.to_slider_value(12)),
            "8 and 12 must be distinguishable, or this proves nothing"
        );
        o.render_distance = 32;
        assert_eq!(rd.slider_fraction(&o), Some(1.0), "the max pins to the end");
        o.render_distance = 2;
        assert_eq!(rd.slider_fraction(&o), Some(0.0), "and the min to the start");

        // The other wrong hypothesis, and the reason `unit_double` says `None`
        // for this option: reading a chunk count as a `UnitDouble` would clamp
        // every value above 1 to the far end of the track.
        o.render_distance = 8;
        assert_eq!(
            LiveOption::RenderDistance.unit_double(&o),
            None,
            "a chunk count must not be readable as a 0..1 fraction"
        );

        // -- sensitivity: a UnitDouble, so the value *is* the fraction.
        let sens = live_slider("sensitivity", "Sensitivity", LiveOption::Sensitivity);
        o.sensitivity = 0.25;
        assert_eq!(sens.slider_fraction(&o), Some(0.25));
        assert_ne!(
            sens.slider_fraction(&o),
            Some(crate::config::DEFAULT_SENSITIVITY),
            "must not be parked at the frozen 0.5 default"
        );
    }

    /// [`LiveOption::MipmapLevels`]'s handle and label must follow
    /// `options.mipmap_levels`, not the table's frozen default of 4 — the
    /// exact shape [`the_migrated_rows_follow_the_persisted_value_not_the_frozen_default`]
    /// checks for `renderDistance`/`sensitivity`, applied to the row this
    /// session made live. The frozen default is deliberately the wrong
    /// hypothesis picked as the control, because it is also the shipped
    /// default — a fraction test with no `assert_ne!` against it would pass
    /// on a completely inert row that never read `options` at all.
    #[test]
    fn the_mipmap_levels_row_follows_the_persisted_value_not_the_frozen_default() {
        let mut o = crate::config::Options::default();
        let mips = live_slider("mipmapLevels", "Mipmap Levels", LiveOption::MipmapLevels);
        let range = SliderRange { min: 0, max: 4 };

        o.mipmap_levels = 1;
        assert_eq!(
            mips.slider_fraction(&o),
            Some(range.to_slider_value(1)),
            "the handle must follow the stored mip depth"
        );
        assert_ne!(
            mips.slider_fraction(&o),
            Some(range.to_slider_value(4)),
            "1 and 4 must be distinguishable, or this proves nothing"
        );
        assert_eq!(live_value(LiveOption::MipmapLevels, &o), "1");

        o.mipmap_levels = 0;
        assert_eq!(mips.slider_fraction(&o), Some(0.0), "the min pins to the start");
        o.mipmap_levels = 4;
        assert_eq!(mips.slider_fraction(&o), Some(1.0), "the max pins to the end");

        // The wrong hypothesis `unit_double` would apply: reading a raw 0..=4
        // depth as a 0..1 fraction would clamp every value above 1 to the far
        // end of the track, exactly as `renderDistance`'s equivalent check
        // guards against.
        assert_eq!(
            LiveOption::MipmapLevels.unit_double(&o),
            None,
            "a mip depth must not be readable as a 0..1 fraction"
        );
    }

    /// `sensitivity`'s label doubles the stored value, and `renderDistance`'s
    /// says "Chunks" with a capital C. Both come from files, not from memory.
    #[test]
    fn the_migrated_labels_are_vanillas_own_strings() {
        let mut o = crate::config::Options::default();

        // `percentValueLabel(caption, 2.0 * value)` (`Options.java`): the
        // shipped default of 0.5 reads **100%**, and the wrong hypothesis —
        // printing the stored number as a percentage — reads 50%.
        o.sensitivity = 0.5;
        assert_eq!(live_value(LiveOption::Sensitivity, &o), "100%");
        assert_ne!(
            live_value(LiveOption::Sensitivity, &o),
            "50%",
            "forgetting the 2.0 factor halves every label a player reads while \
             the mouse behaves correctly"
        );
        // Binary-exact values only. `percent_value` truncates, exactly as
        // vanilla's `(int)(value * 100.0)` does (`Options.java`), but our
        // storage is `f32` where vanilla's is `double` — so a value like 0.35,
        // which is representable in neither, lands at 69.999... here and prints
        // **69%** where vanilla prints 70%. That divergence is a property of the
        // `f32` field shared by every `UnitDouble` option in
        // `crate::config::Options`, not of this row, and asserting it here would
        // pin an unrelated decision. 0.25/0.5/0.75 are exact in both.
        o.sensitivity = 0.25;
        assert_eq!(live_value(LiveOption::Sensitivity, &o), "50%");
        o.sensitivity = 0.75;
        assert_eq!(live_value(LiveOption::Sensitivity, &o), "150%");
        // The two endpoint captions, verbatim from `en_us.json`'s
        // `options.sensitivity.min` / `.max`.
        o.sensitivity = 0.0;
        assert_eq!(live_value(LiveOption::Sensitivity, &o), "*yawn*");
        o.sensitivity = 1.0;
        assert_eq!(live_value(LiveOption::Sensitivity, &o), "HYPERSPEED!!!");

        // `options.chunks` is `"%s Chunks"` — capital C.
        o.render_distance = 12;
        assert_eq!(live_value(LiveOption::RenderDistance, &o), "12 Chunks");

        // And the whole row label composes through `genericValueLabel`'s "%s: %s".
        let rd = live_slider(
            "renderDistance",
            "Render Distance",
            LiveOption::RenderDistance,
        );
        assert_eq!(rd.label(&o), "Render Distance: 12 Chunks");
    }

    /// The eleven volume rows' labels, at **eleven distinct** values.
    ///
    /// Distinct rather than one value repeated, and that is the whole design of
    /// this gate: a uniform value is satisfied by a **transposed pair** — two rows
    /// reading each other's bus — which is precisely the failure an eleven-wide
    /// indexed array invites. With distinct values a swap moves two labels and the
    /// assertion names which.
    ///
    /// The eleven are dyadic (`n/16`), so `value * 100.0` is exact in `f32` *and*
    /// `f64` and the truncation `percentValueLabel` performs is predictable — the
    /// `f32`-vs-`double` divergence `the_migrated_labels_are_vanillas_own_strings`
    /// records does not apply. The percentages are worked out from
    /// `(int)(value * 100.0)` rather than rounded: `0.0625 -> 6`, not 6.25 and not
    /// 7.
    #[test]
    fn every_volume_labels_its_own_bus_at_eleven_distinct_values() {
        let mut o = crate::config::Options::default();
        // (index, stored value, the percent `(int)(v * 100.0)` yields)
        let cases: [(usize, f32, &str); 11] = [
            (0, 0.0625, "6%"),
            (1, 0.125, "12%"),
            (2, 0.1875, "18%"),
            (3, 0.25, "25%"),
            (4, 0.3125, "31%"),
            (5, 0.375, "37%"),
            (6, 0.4375, "43%"),
            (7, 0.5, "50%"),
            (8, 0.5625, "56%"),
            (9, 0.625, "62%"),
            (10, 0.6875, "68%"),
        ];
        for (index, value, _) in cases {
            o.sound_volumes[index] = value;
        }
        for (index, _, want) in cases {
            let got = live_value(LiveOption::SoundVolume(index as u8), &o);
            assert_eq!(
                got,
                want,
                "bus {index} ({}) reads {got}, wanted {want} — a transposed pair \
                 shows up here and nowhere else",
                crate::config::SOUND_CATEGORY_NAMES[index]
            );
        }
        // Every row composes its own caption through `genericValueLabel`, read off
        // the **real page** rather than a synthetic cell — so this is a claim about
        // the tree, not about `live_slider`.
        let master = all_controls(SettingsPage::Sound, OUTSIDE_A_WORLD)
            .into_iter()
            .find(|c| matches!(c, Cell::Option(s) if s.accessor == "soundSource.master"))
            .expect("the Sound page carries a Master Volume row");
        assert_eq!(master.label(&o), "Master Volume: 6%");

        // `percentValueOrOffLabel`, not the plain percent: a muted bus reads OFF.
        // The wrong hypothesis is executed rather than described.
        o.sound_volumes[0] = 0.0;
        assert_eq!(live_value(LiveOption::SoundVolume(0), &o), "OFF");
        assert_ne!(
            live_value(LiveOption::SoundVolume(0), &o),
            "0%",
            "`createSoundSliderOptionInstance` passes `percentValueOrOffLabel`"
        );
        // And the handle follows the stored value rather than the frozen 1.0 in
        // `UNIT_DOUBLE_DEFAULTS` — the exact lie the chat sliders told before their
        // `slider_fraction` arm existed.
        o.sound_volumes[3] = 0.25;
        assert_eq!(master.slider_fraction(&o), Some(0.0), "muted master");
        let weather = all_controls(SettingsPage::Sound, OUTSIDE_A_WORLD)
            .into_iter()
            .find(|c| matches!(c, Cell::Option(s) if s.accessor == "soundSource.weather"))
            .expect("the Sound page carries a Weather row");
        assert_eq!(weather.slider_fraction(&o), Some(0.25));
        assert_ne!(
            weather.slider_fraction(&o),
            Some(1.0),
            "must not be parked at the frozen 1.0 default"
        );
    }

    /// `fov`, `glintSpeed`, `glintStrength` and `cloudStatus`' labels.
    ///
    /// **None of the four can be gated at its default**, and each for its own
    /// reason, which is why every value below is a non-default: `FOV_Y_DEGREES` and
    /// `glint::DEFAULT_SPEED`/`DEFAULT_STRENGTH` *are* vanilla's shipped 70/0.5/0.75,
    /// so at the default the correct and frozen-default hypotheses are
    /// byte-identical, and `CloudStatus::default()` is FANCY.
    #[test]
    fn the_kind_a_labels_are_vanillas_own_strings() {
        let mut o = crate::config::Options::default();

        // -- fov. Vanilla's stringifier cases on **70** and **110**, and 70 is
        // also the shipped default, so a fresh install reads "Normal" and not
        // "70". `en_us.json`: `options.fov.min` = "Normal", `options.fov.max` =
        // "Quake Pro" (no exclamation mark).
        o.fov = crate::config::DEFAULT_FOV;
        assert_eq!(live_value(LiveOption::Fov, &o), "Normal");
        assert_ne!(
            live_value(LiveOption::Fov, &o),
            "70",
            "the special case sits *on* the default, so printing the integer \
             disagrees with vanilla at the one value every new player sees"
        );
        o.fov = crate::config::MAX_FOV;
        assert_eq!(live_value(LiveOption::Fov, &o), "Quake Pro");
        assert_ne!(live_value(LiveOption::Fov, &o), "Quake Pro!");
        // Every other degree count is the plain integer, including the minimum —
        // vanilla names no `options.fov` string for 30.
        o.fov = crate::config::MIN_FOV;
        assert_eq!(live_value(LiveOption::Fov, &o), "30");
        o.fov = 90;
        assert_eq!(live_value(LiveOption::Fov, &o), "90");

        // The root's own row, read off the page rather than rebuilt, composing
        // through `genericValueLabel`.
        let fov_row = all_controls(SettingsPage::Root, OUTSIDE_A_WORLD)
            .into_iter()
            .find(|c| matches!(c, Cell::Option(s) if s.accessor == "fov"))
            .expect("the root page carries an FOV row");
        assert_eq!(fov_row.label(&o), "FOV: 90");

        // The handle, from the stored 90 rather than the table's frozen 70.
        //
        // **90, not 70, and the choice is load-bearing**: at 70 vanilla's
        // bucket-centre map gives `(70 + 0.5 - 30) / (110 + 1 - 30) = 40.5 / 81 =
        // 0.5` and the naive endpoint span gives `(70 - 30) / (110 - 30) = 0.5`
        // too, so the default is an input where the two hypotheses *coincide* and
        // a gate there measures only that the code runs. At 90 they differ.
        o.fov = 90;
        let want = 60.5_f32 / 81.0;
        assert_eq!(fov_row.slider_fraction(&o), Some(want));
        assert_ne!(
            fov_row.slider_fraction(&o),
            Some(0.5),
            "must not be parked at the frozen default's fraction"
        );
        assert_ne!(
            fov_row.slider_fraction(&o),
            Some(0.75),
            "0.75 is the naive endpoint span `(90 - 30) / (110 - 30)`"
        );

        // -- the glint pair. `percentValueOrOffLabel` on both, so zero reads OFF,
        // and both zeroes are real choices: a frozen shimmer and an invisible one.
        o.glint_speed = 0.25;
        o.glint_strength = 0.375;
        assert_eq!(live_value(LiveOption::GlintSpeed, &o), "25%");
        assert_eq!(live_value(LiveOption::GlintStrength, &o), "37%");
        assert_ne!(
            live_value(LiveOption::GlintSpeed, &o),
            "50%",
            "50% is `glint::DEFAULT_SPEED`, i.e. the row still reading the frozen \
             constant its consumer used to be pinned to"
        );
        assert_ne!(
            live_value(LiveOption::GlintStrength, &o),
            "75%",
            "75% is `glint::DEFAULT_STRENGTH`, same failure"
        );
        o.glint_speed = 0.0;
        o.glint_strength = 0.0;
        assert_eq!(live_value(LiveOption::GlintSpeed, &o), "OFF");
        assert_eq!(live_value(LiveOption::GlintStrength, &o), "OFF");
        assert_ne!(live_value(LiveOption::GlintSpeed, &o), "0%");

        // -- cloudStatus. The **whole label is the value**: vanilla's stringifier
        // is `(caption, value) -> value.caption()`, which discards the caption it
        // is handed. The row is read off the real page, and the composed form is
        // executed as the wrong hypothesis rather than described.
        let clouds = all_controls(SettingsPage::Video, OUTSIDE_A_WORLD)
            .into_iter()
            .find(|c| matches!(c, Cell::Option(s) if s.accessor == "cloudStatus"))
            .expect("the Video page carries a Clouds row");
        for (status, want) in [
            (lodestone_render::CloudStatus::Off, "OFF"),
            (lodestone_render::CloudStatus::Fast, "Fast"),
            (lodestone_render::CloudStatus::Fancy, "Fancy"),
        ] {
            o.cloud_status = status;
            assert_eq!(clouds.label(&o), want, "{status:?}");
            assert_ne!(
                clouds.label(&o),
                format!("Clouds: {want}"),
                "`CloudStatus.caption()` throws the caption away"
            );
        }
        // The control for that fork: the *neighbouring* row on the same page does
        // compose, so this is a property of the option and not of `Cell::label`
        // having stopped composing altogether.
        let render_distance = all_controls(SettingsPage::Video, OUTSIDE_A_WORLD)
            .into_iter()
            .find(|c| matches!(c, Cell::Option(s) if s.accessor == "renderDistance"))
            .expect("the Video page carries a Render Distance row");
        assert!(
            render_distance.label(&o).starts_with("Render Distance: "),
            "every other live row still composes its caption"
        );
        // And Clouds is a **cycle**, not a slider: `OptionInstance.Enum` builds a
        // `CycleButton`. A slider track under it would be drawn but unusable.
        assert!(!clouds.is_slider());
    }

    // -- issue #424: the `IntRange` slider ranges ----------------------------

    /// The two rival formulas an `IntRange` fraction could plausibly use, kept
    /// **executable** so the assertions below are controls and not descriptions
    /// of controls.
    ///
    /// Both are what a hand-rolled implementation actually produces, and both
    /// agree with vanilla's on "the handle is somewhere sensible" — which is
    /// exactly why the tests predict *values*.
    mod rival {
        /// Hypothesis A, "endpoint span": map the value linearly onto
        /// `min..=max`. This is the obvious reading of a range, and it is what
        /// you get by forgetting that vanilla's slider selects a **bucket**
        /// (`fromSliderValue` floors, `OptionInstance.java`) rather
        /// than a point. Differs from vanilla's by up to half a bucket.
        pub fn endpoint_span(min: i32, max: i32, value: i32) -> f32 {
            ((f64::from(value - min) / f64::from(max - min)) as f32).clamp(0.0, 1.0)
        }

        /// Hypothesis B, "unpinned centres": vanilla's bucket-centre `Mth.map`
        /// but *without* the two endpoint special cases at
        /// `OptionInstance.java`. Correct in the interior, short of the
        /// ends by half a bucket — the failure that leaves a maxed-out slider
        /// drawing its handle inside the track.
        pub fn unpinned_centres(min: i32, max: i32, value: i32) -> f32 {
            let v = f64::from(value) + 0.5;
            let lo = f64::from(min);
            let hi = f64::from(max) + 1.0;
            (((v - lo) / (hi - lo)) as f32).clamp(0.0, 1.0)
        }
    }

    /// The ported ranges put each slider's handle where vanilla's
    /// `IntRangeBase.toSliderValue` puts it.
    ///
    /// Each expectation is written as the **explicit ratio** the jar's formula
    /// yields for that row's own `(min, max, default)` — `(v + 0.5 - min) /
    /// (max + 1 - min)` worked out by hand from the numbers transcribed in
    /// [`INT_RANGE_SLIDERS`], not by calling the function under test. Hand
    /// arithmetic and the implementation are two independent paths to the same
    /// number, which is the strongest available check here: there is **no JVM
    /// runtime on this machine**, so vanilla's own `toSliderValue` cannot be
    /// executed to produce the oracle.
    #[test]
    fn every_int_range_slider_lands_on_vanillas_own_fraction() {
        let o = crate::config::Options::default();
        let expect = |accessor: &'static str, want: f32, why: &str| {
            let got = slider(accessor, "caption")
                .slider_fraction(&o)
                .unwrap_or_else(|| panic!("{accessor} still has no fraction: {why}"));
            assert!(
                (got - want).abs() < 1e-6,
                "{accessor}: handle at {got}, vanilla puts it at {want} ({why})"
            );
        };

        // Interior values: the general `Mth.map` branch.
        expect("framerateLimit", 11.5 / 26.0, "IntRange(1,26), default int 12");
        expect(
            "entityDistanceScaling",
            2.5 / 19.0,
            "IntRange(2,20), default 1.0 -> int 4",
        );
        expect(
            "chunkSectionFadeInTime",
            15.5 / 41.0,
            "IntRange(0,40), default 0.75 -> int 15",
        );
        expect(
            "menuBackgroundBlurriness",
            5.5 / 11.0,
            "IntRange(0,10), default 5",
        );
        expect(
            "notificationDisplayTime",
            5.5 / 96.0,
            "IntRange(5,100), default 1.0 -> int 10",
        );
        expect("maxAnisotropyBit", 1.5 / 3.0, "IntRange(1,3), default 2");
        expect("biomeBlendRadius", 2.5 / 8.0, "IntRange(0,7), default 2");
        expect("sprintWindow", 7.5 / 11.0, "IntRange(0,10), default 7");
        expect("fov", 40.5 / 81.0, "IntRange(30,110), default 70");
        expect(
            "renderDistance",
            10.5 / 31.0,
            "IntRange(2,32), default 12 — the max is LARGE_DISTANCES_MAX",
        );
        expect(
            "simulationDistance",
            7.5 / 28.0,
            "IntRange(5,32), default 12 — min 5, not the debug-flag 2",
        );

        // Endpoint values: the two special cases, pinned exactly.
        expect("mipmapLevels", 1.0, "IntRange(0,4), default 4 == max");
        expect("cloudRange", 1.0, "IntRange(2,128), default 128 == max");
        expect("weatherRadius", 1.0, "IntRange(3,10), default 10 == max");
        expect("chatDelay", 0.0, "IntRange(0,60), default 0.0 -> int 0 == min");

        // The one `SliderableEnum`, whose divisor is `size - 1`.
        expect(
            "graphicsPreset",
            1.0 / 3.0,
            "FAST/FANCY/FABULOUS/CUSTOM, default FANCY at index 1",
        );
    }

    /// Control for hypothesis A: the naive `(v - min) / (max - min)` span.
    ///
    /// Run, and observed to disagree. Three rows are chosen because their two
    /// hypotheses are **far enough apart to be a visible pixel difference** on
    /// the 150 px settings slider, so this is a magnitude claim and not a sign
    /// claim: `biomeBlendRadius` differs by 0.027 (≈4 px of handle travel),
    /// `entityDistanceScaling` by 0.020, `sprintWindow` by 0.018.
    ///
    /// It also records the rows that **cannot** discriminate, because a gate
    /// that happened to pick only those would pass against the wrong formula
    /// and prove nothing: `fov` (40.5/81 and 40/80 are both exactly 0.5),
    /// `menuBackgroundBlurriness` (5.5/11 == 5/10) and `maxAnisotropyBit`
    /// (1.5/3 == 1/2) are algebraic coincidences of their own bounds.
    #[test]
    fn the_naive_endpoint_span_hypothesis_is_measurably_wrong() {
        let o = crate::config::Options::default();
        // (accessor, min, max, default int, minimum fraction the two must
        // differ by)
        let discriminating = [
            ("biomeBlendRadius", 0, 7, 2, 0.026_f32),
            ("entityDistanceScaling", 2, 20, 4, 0.019_f32),
            ("sprintWindow", 0, 10, 7, 0.017_f32),
        ];
        for (accessor, min, max, default, floor) in discriminating {
            let ours = slider(accessor, "caption")
                .slider_fraction(&o)
                .expect("a ported range");
            let rival = rival::endpoint_span(min, max, default);
            let gap = (ours - rival).abs();
            assert!(
                gap > floor,
                "{accessor}: ours {ours} and the naive span {rival} are only \
                 {gap} apart — this row has stopped discriminating, so the \
                 control is vacuous and a wrong formula would pass"
            );
            // And the wrong one must actually fail the real assertion.
            let want = SliderRange { min, max }.to_slider_value(default);
            assert!(
                (rival - want).abs() > floor,
                "{accessor}: the naive span must FAIL the predicted value"
            );
        }

        // The recorded non-discriminators, asserted as equalities so that if a
        // future range change makes one of them *able* to discriminate, this
        // says so rather than silently leaving the claim stale.
        for (min, max, default) in [(30, 110, 70), (0, 10, 5), (1, 3, 2)] {
            let ours = SliderRange { min, max }.to_slider_value(default);
            assert!(
                (ours - rival::endpoint_span(min, max, default)).abs() < 1e-6,
                "({min},{max},{default}) was recorded as a coincidence where both \
                 formulas agree; it no longer is, so the doc comment is stale"
            );
        }
    }

    /// Control for hypothesis B: bucket centres without the endpoint pinning.
    ///
    /// Run, and observed to disagree at both ends. `mipmapLevels`' default *is*
    /// its maximum, and the unpinned formula puts it at 0.9 — a handle sitting
    /// a tenth of the track short of the end on a slider a player sees maxed.
    #[test]
    fn dropping_the_endpoint_special_cases_is_measurably_wrong() {
        // Maximum: pinned to 1.0, unpinned falls short.
        for (min, max, default, unpinned) in [
            (0, 4, 4, 4.5 / 5.0),      // mipmapLevels: 0.9
            (3, 10, 10, 7.5 / 8.0),    // weatherRadius: 0.9375
            (2, 128, 128, 126.5 / 127.0), // cloudRange: 0.99606
        ] {
            let ours = SliderRange { min, max }.to_slider_value(default);
            let rival = rival::unpinned_centres(min, max, default);
            assert_eq!(ours, 1.0, "the max pins to exactly 1.0");
            assert!(
                (rival - unpinned).abs() < 1e-6,
                "the control's own value moved: {rival} vs {unpinned}"
            );
            assert!(
                rival < ours,
                "the unpinned formula must FAIL the pinned value, short of the end"
            );
        }
        // The strongest single case, stated as a magnitude: mipmapLevels is a
        // tenth of the track out, which at 150 px is 15 px of handle.
        let gap = 1.0 - rival::unpinned_centres(0, 4, 4);
        assert!(
            (gap - 0.1).abs() < 1e-6,
            "mipmapLevels' unpinned error is {gap}, expected exactly 0.1"
        );

        // Minimum: pinned to 0.0, unpinned overshoots.
        let ours = SliderRange { min: 0, max: 60 }.to_slider_value(0);
        let rival = rival::unpinned_centres(0, 60, 0);
        assert_eq!(ours, 0.0, "chatDelay's default is its minimum");
        assert!(
            rival > ours,
            "the unpinned formula must FAIL at the minimum too, past the start"
        );
    }

    /// Coverage: every slider row the settings tree renders now reports a
    /// fraction, except the one documented non-slider-shaped leftover.
    ///
    /// This is the island check for #424 — a range ported into
    /// [`INT_RANGE_SLIDERS`] that no row's accessor matches would be dead data,
    /// and a row whose accessor is in neither table would silently draw no
    /// handle. Sweeping the real `PAGES` is what makes it a coverage claim
    /// rather than a spot check on the rows I happened to think of.
    #[test]
    fn every_slider_the_tree_renders_can_place_its_handle() {
        let o = crate::config::Options::default();
        // `fullscreenResolution`'s value set is the monitor's real video-mode
        // list, so it has no range and no default int — see
        // `int_range_default_fraction`'s doc.
        const KNOWN_HANDLE_LESS: &[&str] = &["fullscreenResolution"];

        let mut seen: Vec<&str> = Vec::new();
        let mut missing: Vec<&str> = Vec::new();
        for in_world in [false, true] {
            for page in PAGES {
                for cell in all_controls(page, in_world) {
                    let Cell::Option(spec) = cell else { continue };
                    if spec.widget != OptionWidget::Slider {
                        continue;
                    }
                    seen.push(spec.accessor);
                    if cell.slider_fraction(&o).is_none() {
                        missing.push(spec.accessor);
                    }
                }
            }
        }
        missing.sort_unstable();
        missing.dedup();
        assert_eq!(
            missing, KNOWN_HANDLE_LESS,
            "these slider rows draw no handle; either port their range into \
             INT_RANGE_SLIDERS or document why they cannot have one"
        );

        // The mirror direction: no ported range is dead data.
        for (accessor, _, _) in INT_RANGE_SLIDERS {
            assert!(
                seen.contains(accessor),
                "{accessor} has a ported range but no row on any page renders \
                 it — dead data, or the accessor string does not match"
            );
        }

        // And the detector works: a slider the tables do not know must still
        // report `None`, or the sweep above would pass vacuously.
        assert_eq!(
            slider("notAnOption", "caption").slider_fraction(&o),
            None,
            "an unported accessor must report no handle"
        );
    }

    /// The read and write sides of a `UnitDouble` slider must agree about which
    /// options they cover.
    ///
    /// The compiler enforces that both `match`es are exhaustive over the enum,
    /// but not that the `Some`/`None` split is the *same* split — a slider
    /// readable but not writable silently reverts under the cursor, and one
    /// writable but not readable moves the world with its handle parked.
    #[test]
    fn every_unit_double_option_is_readable_and_writable() {
        let mut o = crate::config::Options::default();
        for &live in ALL_LIVE_OPTIONS {
            let readable = live.unit_double(&o).is_some();
            let writable = live.unit_double_mut(&mut o).is_some();
            assert_eq!(
                readable, writable,
                "{live:?}: readable {readable}, writable {writable}"
            );
        }
    }

    /// [`SliderRange::from_slider_value`] and [`SliderRange::to_slider_value`]
    /// are transcribed from the jar independently and only one of them carries
    /// the endpoint special cases, so this is not a `decode(encode(x))`
    /// tautology: it is the bucket model agreeing with itself.
    #[test]
    fn slider_values_round_trip_through_the_bucket_map() {
        for (accessor, range, _) in INT_RANGE_SLIDERS {
            for value in range.min..=range.max {
                let f = range.to_slider_value(value);
                let back = range.from_slider_value(f);
                assert_eq!(
                    back, value,
                    "{accessor}: {value} -> fraction {f} -> {back}"
                );
            }
            // Both track ends land on the bounds, never one past them — the
            // `max + 1` in the bucket map is what makes that need a clamp.
            assert_eq!(range.from_slider_value(0.0), range.min, "{accessor} low");
            assert_eq!(range.from_slider_value(1.0), range.max, "{accessor} high");
        }
    }

    /// Every [`LiveOption`] there is, hand-listed.
    ///
    /// Module-scoped so the reachability sweep and the read/write parity check
    /// share **one** list: two lists would drift, and the second one to drift
    /// would pass vacuously. `every_live_option_is_reachable_from_some_row`
    /// carries the control that proves this is exhaustive over the enum.
    const ALL_LIVE_OPTIONS: &[LiveOption] = &[
        LiveOption::GuiScale,
        LiveOption::ViewBobbing,
        LiveOption::ShowSubtitles,
        LiveOption::ToggleSneak,
        LiveOption::ToggleSprint,
        LiveOption::ToggleAttack,
        LiveOption::ToggleUse,
        LiveOption::InvertMouseX,
        LiveOption::InvertMouseY,
        LiveOption::MouseWheelSensitivity,
        LiveOption::ChatScale,
        LiveOption::ChatWidth,
        LiveOption::ChatHeightFocused,
        LiveOption::ChatHeightUnfocused,
        LiveOption::ChatLineSpacing,
        LiveOption::ChatOpacity,
        LiveOption::TextBackgroundOpacity,
        LiveOption::ChatColors,
        // Issue #443's migration: both were on argv-only `Config` and are
        // now persisted `Options` fields with a real row.
        LiveOption::Sensitivity,
        LiveOption::RenderDistance,
        // Issue #444: the six Controls/Mouse rows. `discreteMouseScroll` was
        // the first, whose consumer this shell already had — `app`'s wheel
        // boundary; the other four landed with the toggles/auto-jump/sprint
        // window. See the variants' own docs.
        LiveOption::DiscreteMouseScroll,
        LiveOption::AutoJump,
        LiveOption::SprintWindow,
        // The two Accessibility sliders whose consumers were already live and
        // whose rows were not: the camera tilt `app/redraw.rs` already honoured,
        // and the title-screen spin rate `PanoramaRenderer::set_speed` already
        // implemented with no caller. See the variants' own docs.
        LiveOption::DamageTiltStrength,
        LiveOption::PanoramaSpeed,
        // The kind A batch: fifteen rows whose consumers already ran every frame
        // against a hardcoded constant. **All eleven sound indices are listed
        // individually and that is load-bearing** — `SoundVolume` is one variant,
        // so the exhaustiveness control below is satisfied by a single
        // `SoundVolume(_)` arm and cannot tell that an index is missing. Only the
        // reachability sweep, run per index, can.
        LiveOption::SoundVolume(0),
        LiveOption::SoundVolume(1),
        LiveOption::SoundVolume(2),
        LiveOption::SoundVolume(3),
        LiveOption::SoundVolume(4),
        LiveOption::SoundVolume(5),
        LiveOption::SoundVolume(6),
        LiveOption::SoundVolume(7),
        LiveOption::SoundVolume(8),
        LiveOption::SoundVolume(9),
        LiveOption::SoundVolume(10),
        LiveOption::Fov,
        LiveOption::GlintSpeed,
        LiveOption::GlintStrength,
        LiveOption::CloudStatus,
        // Video settings: `framerateLimit`/`enableVsync`/`inactivityFpsLimit`
        // were rows with zero consumers anywhere else in the crate; `cutoutLeaves`
        // and `graphicsPreset` are the leaves-render-pass fix's own two rows.
        LiveOption::FramerateLimit,
        LiveOption::EnableVsync,
        LiveOption::InactivityFpsLimit,
        LiveOption::GraphicsPreset,
        LiveOption::CutoutLeaves,
        // The block-atlas mip-depth row: its consumer used to be the frozen
        // `BLOCK_ATLAS_MIP_LEVELS` constant, so this handle moved and nothing
        // downstream ever read the new value.
        LiveOption::MipmapLevels,
        // Owner report: "entity shadows are missing". `RenderState::
        // prepare_shadows`'s own gate — its consumer did not exist at all
        // before this session, not merely an unwired row.
        LiveOption::EntityShadows,
        // The rain/snow column radius: `extract_columns` and `column_instance`
        // already took one and `weather_columns_for_frame` handed both the
        // frozen `DEFAULT_WEATHER_RADIUS`.
        LiveOption::WeatherRadius,
        // The menu background-blur radius: the pass existed and ran at the
        // frozen `menu::render::blur::BLUR_RADIUS`. Placed on two pages.
        LiveOption::MenuBackgroundBlurriness,
        // The attack-strength indicator's three states: the crosshair bar
        // already drew, pinned to CROSSHAIR, and HOTBAR is a real second draw.
        LiveOption::AttackIndicator,
    ];

    /// Every [`LiveOption`] must be placed on some page — the island check in
    /// the *outbound* direction.
    ///
    /// The census test above catches a row that claims to be live without a
    /// consumer. This catches the mirror image: an option wired all the way
    /// through `config::Options` and `MenuNav::apply_settings` that no row on any
    /// page actually offers, so a player can never reach it. Both directions have
    /// happened in this repo; a `LiveOption` is cheap to add and easy to forget
    /// to place.
    #[test]
    fn every_live_option_is_reachable_from_some_row() {
        const ALL: &[LiveOption] = ALL_LIVE_OPTIONS;
        let placed: Vec<LiveOption> = PAGES
            .iter()
            .flat_map(|&p| all_controls(p, OUTSIDE_A_WORLD))
            .filter_map(|c| match c {
                Cell::Option(spec) => spec.live,
                _ => None,
            })
            .collect();
        for live in ALL {
            assert!(
                placed.contains(live),
                "{live:?} is honoured by `MenuNav` but sits on no page — no \
                 player can reach it"
            );
        }
        // The control: `ALL` must itself be exhaustive over the enum. A `match`
        // with no wildcard makes the compiler enforce that, so adding a variant
        // and forgetting this list is a build error rather than a silent gap.
        for live in ALL {
            match live {
                LiveOption::GuiScale
                | LiveOption::ViewBobbing
                | LiveOption::ShowSubtitles
                | LiveOption::ToggleSneak
                | LiveOption::ToggleSprint
                | LiveOption::ToggleAttack
                | LiveOption::ToggleUse
                | LiveOption::InvertMouseX
                | LiveOption::InvertMouseY
                | LiveOption::MouseWheelSensitivity
                | LiveOption::ChatScale
                | LiveOption::ChatWidth
                | LiveOption::ChatHeightFocused
                | LiveOption::ChatHeightUnfocused
                | LiveOption::ChatLineSpacing
                | LiveOption::ChatOpacity
                | LiveOption::TextBackgroundOpacity
                | LiveOption::ChatColors
                | LiveOption::Sensitivity
                | LiveOption::RenderDistance
                | LiveOption::DiscreteMouseScroll
                | LiveOption::AutoJump
                | LiveOption::SprintWindow
                | LiveOption::DamageTiltStrength
                | LiveOption::PanoramaSpeed
                | LiveOption::SoundVolume(_)
                | LiveOption::Fov
                | LiveOption::GlintSpeed
                | LiveOption::GlintStrength
                | LiveOption::CloudStatus
                | LiveOption::FramerateLimit
                | LiveOption::EnableVsync
                | LiveOption::InactivityFpsLimit
                | LiveOption::GraphicsPreset
                | LiveOption::CutoutLeaves
                | LiveOption::MipmapLevels
                | LiveOption::EntityShadows
                | LiveOption::WeatherRadius
                | LiveOption::MenuBackgroundBlurriness
                | LiveOption::AttackIndicator => {}
            }
        }
        // 25 before the kind A batch, plus eleven sound buses, FOV, both glint
        // parameters and Clouds, plus the five video-settings/leaves rows that
        // session wired, plus the block-atlas mip-depth row, plus this
        // session's entity-shadows row, plus the weather-radius,
        // menu-background-blur and attack-indicator rows.
        assert_eq!(ALL.len(), 50, "fifty distinct live options");
        // And the eleven indices are all of them, none repeated: `SoundVolume` is
        // a *payload* variant, so neither the compiler nor the match above can see
        // a missing or duplicated index, and a duplicate would silently leave one
        // mixer bus unreachable while `ALL.len()` still read 40.
        let mut buses: Vec<u8> = ALL
            .iter()
            .filter_map(|l| match l {
                LiveOption::SoundVolume(i) => Some(*i),
                _ => None,
            })
            .collect();
        buses.sort_unstable();
        assert_eq!(
            buses,
            (0..crate::config::SOUND_CATEGORY_NAMES.len() as u8).collect::<Vec<u8>>(),
            "every `SoundSource` ordinal exactly once"
        );
    }

    /// Each Sound-page row's [`LiveOption::SoundVolume`] index must be the
    /// ordinal of the category its **accessor** names.
    ///
    /// The one thing no compiler checks about an eleven-wide indexed array, and
    /// the failure it invites is a **transposed pair**: two rows swapped move each
    /// other's bus while both labels read correctly and every round-trip test
    /// still passes. The expected mapping originates outside this file — it is
    /// [`crate::config::SOUND_CATEGORY_NAMES`], which is
    /// `SoundSource.getName()` in `SoundSource` declaration order, the same list
    /// the file keys and the mixer buses are derived from.
    #[test]
    fn sound_rows_index_the_category_they_name() {
        let rows: Vec<(&str, u8)> = all_controls(SettingsPage::Sound, OUTSIDE_A_WORLD)
            .into_iter()
            .filter_map(|c| match c {
                Cell::Option(spec) => match spec.live {
                    Some(LiveOption::SoundVolume(i)) => Some((spec.accessor, i)),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(rows.len(), 11, "eleven volume rows on the Sound page: {rows:?}");
        for (accessor, index) in rows {
            let name = accessor
                .strip_prefix("soundSource.")
                .expect("every volume row's accessor is `soundSource.<name>`");
            assert_eq!(
                crate::config::SOUND_CATEGORY_NAMES[index as usize],
                name,
                "{accessor} carries index {index}, which is the \
                 `{}` bus — a transposed pair",
                crate::config::SOUND_CATEGORY_NAMES[index as usize]
            );
        }
        // The control: the mapping this asserts is not the identity on the *page*
        // order, so it is not satisfied by "the rows happen to be in order". The
        // page pulls MASTER out into its own `addBig` row and pairs the rest, and
        // `record` (ordinal 2) is the **second** column of the first pair while
        // `weather` (ordinal 3) opens the next — so an implementation that indexed
        // by position within a pair, or by row, would disagree here.
        assert_eq!(crate::config::SOUND_CATEGORY_NAMES[2], "record");
        assert_eq!(crate::config::SOUND_CATEGORY_NAMES[3], "weather");
    }

    /// [`crate::config::step_unit_double`]'s wrap, including the two places it
    /// is easy to get wrong: `1.0` must be a *reachable resting* value rather
    /// than skipped, and a value off the `0.1` grid must stay off it.
    #[test]
    fn stepping_a_unit_double_wraps_at_the_top_and_never_snaps_to_a_grid() {
        use crate::config::step_unit_double;
        assert_eq!(step_unit_double(0.0, 1), 0.1);
        assert_eq!(step_unit_double(0.9, 1), 1.0, "1.0 must be reachable");
        assert_eq!(step_unit_double(1.0, 1), 0.0, "and then wrap to the bottom");

        // `chat_height_unfocused` boots at 70/160 = 0.4375, which is not on the
        // 0.1 grid. Snapping would silently move a value the user never touched;
        // the step must stay additive.
        let off_grid = 70.0_f32 / 160.0;
        let stepped = step_unit_double(off_grid, 1);
        assert!(
            (stepped - 0.5375).abs() < 1e-6,
            "expected 0.5375 (additive), got {stepped} — a snap-to-grid \
             implementation would give 0.5"
        );

        // A corrupt or hand-edited value is pulled back onto the domain.
        assert_eq!(step_unit_double(f32::NAN, 1), 0.1);
        assert_eq!(step_unit_double(99.0, 1), 0.0, "clamped to 1.0, then wraps");
    }

    #[test]
    fn only_slider_backed_options_are_sliders() {
        // `OptionInstance.createButton` dispatches on the `ValueSet`, and
        // `ClampingLazyMaxIntRange.createCycleButton()` is `true`
        // (`OptionInstance.java`) — so GUI Scale, an int range, is a
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
        // (`OptionsList.java`). Video opens with a header and has two
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
        assert_eq!(list_cell_origin(page, 0, 0.0, 0, 480.0, 480.0), (85.0, 37.0));
        assert_eq!(list_cell_origin(page, 0, 0.0, 1, 480.0, 480.0), (245.0, 37.0));
        // Entry 2 is two 25 px entries down.
        assert_eq!(list_cell_origin(page, 2, 0.0, 0, 480.0, 480.0), (85.0, 87.0));
        // The **scrolled** assertions need a canvas the offset is legal at, since
        // `list_cell_origin` now re-clamps through `drawn_scroll` (see its doc, and
        // the player report it fixed). This page fits a 480-tall canvas whole, so
        // *no* offset is legal there — asking for one and getting 0 is the clamp
        // working, not the arithmetic failing. A short canvas is the honest
        // fixture, and the premise is asserted rather than assumed.
        const SHORT: f32 = 100.0;
        let room = list_spec(page, 0.0)
            .model(SHORT)
            .expect("premise: this page scrolls at a 100 px canvas")
            .max_scroll();
        assert!(
            room >= 50.0,
            "premise: a 50 px offset must be legal at a {SHORT} px canvas, but the \
             maximum there is {room} — the assertions below would measure the clamp \
             instead of the offset"
        );
        // Scrolled by entry 2's own absolute offset (two 25 px entries = 50 px),
        // entry 2 lands exactly where entry 0 was. The third argument is
        // **pixels** (issue #445), not the index of the top entry — `2` used to
        // mean "entry 2 at the top" and `50.0` means the same thing here, which is
        // the conversion in one line.
        assert_eq!(entry_offset(page.entries(), 2), 50.0);
        assert_eq!(list_cell_origin(page, 2, 50.0, 0, 480.0, SHORT), (85.0, 37.0));
        // And a *fractional* scroll no row-index offset could express: 10 px down
        // from the top puts entry 0 ten pixels higher, not a whole row higher.
        assert_eq!(list_cell_origin(page, 0, 10.0, 0, 480.0, SHORT), (85.0, 27.0));
        // Java integer division on an odd width: `481 / 2 == 240`, not 240.5.
        assert_eq!(row_left(481.0, 0), 85.0);
        assert_eq!(row_left(480.0, 0), 85.0);
        // A header's `StringWidget` is at `getContentY() + paddingTop`, and the
        // 18 px padding is what separates a mid-list header from its neighbour.
        let video = SettingsPage::Video;
        assert_eq!(list_header_origin(video, 0, 0.0, 480.0, 480.0), (85.0, 37.0));
        let (_, quality_y) = list_header_origin(video, 6, 0.0, 480.0, 480.0);
        let (_, cell_y) = list_cell_origin(video, 6, 0.0, 0, 480.0, 480.0);
        assert_eq!(quality_y - cell_y, 18.0, "the second header's paddingTop");
    }

    /// **Scrolling reaches the end of the longest page, at every canvas** — the
    /// player report *"when I scroll it doesn't reach the end"* (2026-08-07), as a
    /// predicted value rather than "more rows than before".
    ///
    /// Driven through the **keyboard**, because that is the writer that had no
    /// canvas and therefore produced the defect: `scroll_to_cursor` runs against
    /// `MIN_SCALED_HEIGHT`, and before `drawn_scroll` existed the rows were then
    /// placed from that raw offset at whatever canvas the window happened to be.
    ///
    /// The page is **Video** specifically and the control count is a
    /// **precondition**: the `world` species of vacuous test lives in the input
    /// data, and a page whose entries fit the band cannot show a tail being
    /// unreachable. The count comes from the page's own control list, so adding a
    /// 32nd control cannot silently make the tail unreachable again — and the
    /// canvas is swept, because a fixture at one canvas cannot show that another
    /// wastes space.
    #[test]
    fn arrowing_to_the_end_of_the_video_page_reaches_its_last_control_at_every_canvas() {
        let page = SettingsPage::Video;
        let entries = page.entries();
        let all = all_controls(page, false);
        assert!(
            all.len() >= 31,
            "premise: the Video page is the longest in the tree ({} controls) — a \
             shorter one cannot exercise an unreachable tail",
            all.len()
        );
        assert_eq!(
            all.len(),
            PAGES
                .iter()
                .map(|p| all_controls(*p, false).len())
                .max()
                .unwrap(),
            "premise: and it really is the longest, so this is the page where a \
             conservative window bites hardest"
        );
        // The last control that is a *list cell* — the footer's Done is the last
        // entry of `all_controls` and is not in the band at all.
        let last = (0..all.len())
            .rev()
            .find_map(|i| entry_of_control(page, i).map(|e| (i, e)))
            .expect("premise: the page has list cells");
        let (last_cursor, last_entry) = last;
        assert_eq!(
            last_entry,
            entries.len() - 1,
            "premise: the last list control is in the page's last entry"
        );

        for height in [
            crate::config::MIN_SCALED_HEIGHT as f32,
            318.0,
            480.0,
            720.0,
        ] {
            let mut nav = SettingsNav::new();
            nav.open_at(false, page);
            assert_eq!(nav.scroll(), 0.0, "a page opens at the top");
            // Arrow down to the last list control, exactly as a player would.
            for _ in 0..last_cursor {
                nav.step(true);
            }
            assert_eq!(nav.cursor(), last_cursor, "the cursor reached the last row");

            let band_top = page.header_height();
            let band_bottom = height - FOOTER_HEIGHT;
            let (_, y) = list_cell_origin(page, last_entry, nav.scroll(), 0, 854.0, height);
            let bottom = y + WIDGET_H;
            assert!(
                y >= band_top && bottom <= band_bottom,
                "at a {height} px canvas the last Video control is drawn at \
                 {y}..{bottom}, outside the band {band_top}..{band_bottom} — this is \
                 the 'scrolling does not reach the end' defect"
            );

            // And when the page *does* overflow, the last row really is at the end
            // of the band rather than merely somewhere legal: within one entry's
            // height of the band's bottom. Predicted, not a direction.
            let max = list_spec(page, 0.0)
                .model(height)
                .map_or(0.0, |l| l.max_scroll());
            if max > 0.0 {
                assert!(
                    bottom >= band_bottom - DEFAULT_ITEM_HEIGHT,
                    "at a {height} px canvas the page overflows by {max} px but its \
                     last control stops at {bottom}, more than one {DEFAULT_ITEM_HEIGHT} \
                     px entry short of the band's bottom {band_bottom}"
                );
            } else {
                // The control for the branch above: at a canvas tall enough to
                // show the whole page there is nothing to reach, and the last row
                // must *not* be at the bottom — otherwise the assertion above
                // would be satisfied by a page that is always scrolled to its end.
                assert!(
                    bottom < band_bottom - DEFAULT_ITEM_HEIGHT,
                    "at a {height} px canvas the whole page fits, so its last \
                     control must sit well above the band's bottom"
                );
            }
        }
    }

    /// **The wheel arm, measured on the *draw* rather than on frame data.**
    ///
    /// `arrowing_to_the_end_of_the_video_page_reaches_its_last_control_at_every_canvas`
    /// drives the keyboard and reads `list_cell_origin`. This drives the wheel to
    /// its clamp and reads `render::row_rect` off a real `settings_frame` — the
    /// expression `render::draw` positions each row with, and the band it clips
    /// to. That distinction is not pedantry: the resource-pack screen had a fully
    /// green suite while drawing the wrong thing, because every test asserted on
    /// frame data and nothing asserted on the draw.
    ///
    /// What it pins, on every page at four canvases: 200 wheel notches down land
    /// exactly on `max_scroll` (the end is *reachable*), and the last list row is
    /// then wholly inside the clip band, ending `LIST_CONTENT_PADDING` above its
    /// bottom — vanilla's trailing padding, and a **predicted** value rather than
    /// "somewhere legal".
    #[test]
    fn wheeling_to_the_clamp_puts_the_last_row_at_the_end_of_the_band() {
        let options = crate::config::Options::default();
        let mut pages_measured = 0;
        for page in PAGES {
            if page.entries().is_empty() {
                continue;
            }
            for height in [crate::config::MIN_SCALED_HEIGHT as f32, 318.0, 480.0, 720.0] {
                let width = 854.0;
                let mut nav = SettingsNav::new();
                nav.open_at(false, page);
                // Far more notches than any page needs (one notch is
                // `floor(25/2)` = 12 px against a worst case of 330), so this
                // measures the clamp and not the loop bound.
                for _ in 0..200 {
                    nav.scroll_by(-1.0, height);
                }
                let max = list_spec(page, 0.0)
                    .model(height)
                    .map_or(0.0, |l| l.max_scroll());
                assert_eq!(
                    nav.scroll(), max,
                    "{page:?} at {height} px: the wheel stopped at {} of a {max} px \
                     maximum — the end is unreachable",
                    nav.scroll()
                );
                if max == 0.0 {
                    continue;
                }
                pages_measured += 1;

                let frame = settings_frame(&nav, &options, None);
                // `settings_frame` deliberately leaves `MenuFrame::list` unset —
                // `render::dispatch` stamps `nav.active_list(ui)` onto it — so the
                // band comes from the same `list_spec` that arm returns, which is
                // what the draw clips to and what the wheel clamped through above.
                let list = list_spec(page, nav.scroll())
                    .model(height)
                    .expect("a scrollable page has a band at this canvas");
                // The **last row the draw clips to the band**, found the way
                // `render::draw` decides what to clip, not by index arithmetic.
                let (last, rect) = (0..frame.rows.len())
                    .rev()
                    .filter(|&i| {
                        frame.rows[i]
                            .slot
                            .is_some_and(|s| s.origin.is_scrolling_list_row())
                    })
                    .find_map(|i| {
                        crate::menu::render::row_rect(&frame.rows, i, width, height).map(|r| (i, r))
                    })
                    .expect("a settings page draws list rows");
                let (_, y, _, h) = rect;
                assert!(
                    y >= list.top() && y + h <= list.bottom(),
                    "{page:?} at {height} px: row {last} draws at {y}..{} outside the \
                     band {}..{} — this is the 'scrolling does not reach the end' shape",
                    y + h,
                    list.top(),
                    list.bottom()
                );
                // Predicted exactly: at `max_scroll` the last *entry box* ends
                // `LIST_CONTENT_PADDING` above the band's bottom, and the widget
                // inside it is inset a further `ENTRY_CONTENT_INSET` at the top,
                // so a 20 px widget in a 25 px entry ends 5 px above that.
                let entries = page.entries();
                let entry_bottom = y - ENTRY_CONTENT_INSET + entry_height(entries, entries.len() - 1);
                assert_eq!(
                    entry_bottom,
                    list.bottom() - crate::menu::widget::LIST_CONTENT_PADDING,
                    "{page:?} at {height} px: the last entry ends at {entry_bottom}, not \
                     {} — vanilla's contentHeight() reserves exactly \
                     LIST_CONTENT_PADDING below the last entry",
                    list.bottom() - crate::menu::widget::LIST_CONTENT_PADDING
                );

                // **The two expressions for one quantity, made to agree.**
                // `list_cell_origin` walks `LIST_TOP_INSET + entry_offset +
                // ENTRY_CONTENT_INSET` while the band, the scrollbar and the clip
                // walk `ScrollList::row_top` (`first_entry_y + row_offset`). Every
                // other list in this tree derives both from one expression; this
                // page cannot, because its `entry_height` table is `OptionsList`'s
                // and not the primitive's. They agree today — `LIST_TOP_INSET` and
                // `LIST_CONTENT_PADDING` are both 2 px, and `entry_offset` is the
                // same sum `with_heights` was handed — and this is what keeps them
                // agreeing, since a drift shows up as "scrolling does not reach the
                // end" with nothing wrong at either site on its own.
                for entry in 0..entries.len() {
                    let (_, cell_y) =
                        list_cell_origin(page, entry, nav.scroll(), 0, width, height);
                    assert_eq!(
                        cell_y,
                        list.row_top(entry) + ENTRY_CONTENT_INSET,
                        "{page:?} at {height} px, entry {entry}: `list_cell_origin` and \
                         `ScrollList::row_top` disagree about where the row is"
                    );
                }
            }
        }
        assert!(
            pages_measured >= 4,
            "premise: at least four (page, canvas) pairs must actually overflow their \
             band, or this measured no scrolling at all — got {pages_measured}"
        );
    }

    /// The clamp itself, both hypotheses computed from outside constants.
    ///
    /// At the shortest canvas the Video page's band is `240 - 33 - 33` = 174 and
    /// `contentHeight` is `500 + 4`, so `maxScrollAmount` is **330**. At 854×480
    /// the band is 414 and the maximum is **90**. `scroll_to_cursor` legitimately
    /// produces the former; drawing it at the latter canvas is the defect.
    #[test]
    fn the_settings_scroll_is_clamped_to_the_canvas_it_is_drawn_at() {
        let page = SettingsPage::Video;
        let content: f32 = (0..page.entries().len())
            .map(|i| entry_height(page.entries(), i))
            .sum();
        assert_eq!(content, 500.0, "premise: the Video page's own content height");
        let short = crate::config::MIN_SCALED_HEIGHT as f32;
        assert_eq!(
            list_spec(page, 0.0).model(short).unwrap().max_scroll(),
            content + 4.0 - (short - 2.0 * FOOTER_HEIGHT),
            "330 at the shortest canvas"
        );
        assert_eq!(
            list_spec(page, 0.0).model(480.0).unwrap().max_scroll(),
            content + 4.0 - (480.0 - 2.0 * FOOTER_HEIGHT),
            "90 at 854x480"
        );
        // The keyboard's own offset, clamped for the canvas it is drawn at.
        assert_eq!(drawn_scroll(page, 330.0, 480.0), 90.0);
        assert_eq!(drawn_scroll(page, 330.0, short), 330.0, "legal at its own canvas");
        assert_ne!(
            drawn_scroll(page, 330.0, 480.0),
            330.0,
            "the unclamped hypothesis would draw the list 240 px past its own end"
        );
        // A page that fits has no legal offset at all.
        assert_eq!(drawn_scroll(SettingsPage::Controls, 200.0, 480.0), 0.0);
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
                // The pixel offset that puts entry `first` at the band's top —
                // `entry_offset` is the conversion, so this asserts exactly the
                // property it did before #445 (a window opening at entry `first`
                // never overruns the footer) in the new units.
                let first_px = entry_offset(entries, first);
                for entry in visible_entries(entries, first) {
                    // The canvas is `height`, not a fixed 480: this test's whole
                    // subject is the shortest canvas, and `list_cell_origin` now
                    // re-clamps the offset against the canvas it is *drawn* at
                    // (`drawn_scroll`). Passing 480 here would clamp every offset
                    // to what a 480-tall canvas allows and then assert the result
                    // against a 240-tall canvas's footer — which is how this
                    // assertion first went red, correctly.
                    let (_, y) = list_cell_origin(page, entry, first_px, 0, 480.0, height);
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
        let overrun =
            list_cell_origin(SettingsPage::Chat, window.end, 0.0, 0, 480.0, height).1 + WIDGET_H;
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
        for _ in 0..all_controls(page, false).len() {
            // The band the *primitive* reports visible at this pixel offset, not
            // `visible_entries`' old index window — that function is no longer on
            // the draw path (see its doc).
            if let Some(list) = list_spec(page, nav.scroll()).model(crate::config::MIN_SCALED_HEIGHT as f32) {
                for entry in list.visible_range() {
                    seen.insert(entry);
                }
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
                nav.scroll = entry_offset(page.entries(), first);
                let frame = settings_frame(&nav, &options, None);
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
            all_controls(SettingsPage::Video, false)
                .iter()
                .position(|c| matches!(c, Cell::Option(s) if s.accessor == "guiScale"))
                .expect("Video carries guiScale"),
        )
        .expect("and it is a list cell");
        nav.scroll = entry_offset(SettingsPage::Video.entries(), entry);
        let visible = nav.visible();
        let scale_row = visible
            .iter()
            .position(|c| matches!(c.cell, Cell::Option(s) if s.accessor == "guiScale"))
            .expect("visible after scrolling to its entry");
        assert_eq!(
            nav.click_row(scale_row),
            SettingsOutcome::Cycle(LiveOption::GuiScale)
        );
        // `fullscreen` is still inert: clicking it must do **nothing**, not
        // fall through to whatever Enter last meant. This is the assertion
        // #391 would have failed. (`inactivityFpsLimit`, GUI Scale's former
        // left-hand neighbour, went live alongside the rest of the video
        // settings and is exercised by its own gate now.)
        let neighbour = visible
            .iter()
            .position(|c| matches!(c.cell, Cell::Option(s) if s.accessor == "fullscreen"))
            .expect("Video still carries an inert fullscreen row");
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
        let video = all_controls(SettingsPage::Root, false)
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
        let controls_link = all_controls(SettingsPage::Accessibility, false)
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
        // A nav button to a screen we do not build must be inert. Language,
        // Telemetry and Resource Packs were this test's examples in turn —
        // issue #415 built all three, so the root grid itself has no
        // unbuilt `Cell::Nav` left at all. The one that remains is the
        // root's own header button *inside* a world, where it is the
        // inactive World Options placeholder rather than a link to Online
        // (`WorldOptionsScreen` is out of scope — see `online_cell`'s doc).
        let mut nav = SettingsNav::new();
        nav.reset(true); // inside a world
        let world_options = all_controls(SettingsPage::Root, true)
            .iter()
            .position(|c| matches!(c, Cell::Nav { label: "World Options...", page: None }))
            .expect("World Options is present and unbuilt, inside a world");
        nav.cursor = world_options;
        assert_eq!(nav.enter(), SettingsOutcome::None);
        assert_eq!(nav.page(), SettingsPage::Root, "and must not move");
        // Root -> Language (issue #415), and back — the third list-widget
        // kind, reached from the root grid rather than a sub-page.
        let mut nav = SettingsNav::new();
        let language = all_controls(SettingsPage::Root, false)
            .iter()
            .position(|c| matches!(c, Cell::Nav { page: Some(SettingsPage::Language), .. }))
            .expect("the root links to Language");
        nav.cursor = language;
        assert_eq!(nav.enter(), SettingsOutcome::None);
        assert_eq!(nav.page(), SettingsPage::Language);
        nav.escape();
        assert_eq!(nav.page(), SettingsPage::Root);
        // Root -> Telemetry (issue #415), and back.
        let mut nav = SettingsNav::new();
        let telemetry = all_controls(SettingsPage::Root, false)
            .iter()
            .position(|c| matches!(c, Cell::Nav { page: Some(SettingsPage::Telemetry), .. }))
            .expect("the root links to Telemetry");
        nav.cursor = telemetry;
        assert_eq!(nav.enter(), SettingsOutcome::None);
        assert_eq!(nav.page(), SettingsPage::Telemetry);
        nav.escape();
        assert_eq!(nav.page(), SettingsPage::Root);
        // Root -> Resource Packs (issue #415), and back.
        let mut nav = SettingsNav::new();
        let packs = all_controls(SettingsPage::Root, false)
            .iter()
            .position(|c| matches!(c, Cell::Nav { page: Some(SettingsPage::ResourcePacks), .. }))
            .expect("the root links to Resource Packs");
        nav.cursor = packs;
        assert_eq!(nav.enter(), SettingsOutcome::None);
        assert_eq!(nav.page(), SettingsPage::ResourcePacks);
        nav.escape();
        assert_eq!(nav.page(), SettingsPage::Root);
        // The new one: Root -> Online, live only outside a world.
        let mut nav = SettingsNav::new();
        assert!(!nav.in_world, "precondition: outside a world by default");
        let online = all_controls(SettingsPage::Root, false)
            .iter()
            .position(|c| matches!(c, Cell::Nav { page: Some(SettingsPage::Online), .. }))
            .expect("the root links to Online outside a world");
        nav.cursor = online;
        assert_eq!(nav.enter(), SettingsOutcome::None);
        assert_eq!(nav.page(), SettingsPage::Online);
        nav.escape();
        assert_eq!(nav.page(), SettingsPage::Root);
        // And with `in_world` true, that same root row is the inert World
        // Options placeholder — the cell at index `online` is no longer a
        // page link at all, so entering it must not move.
        let mut nav = SettingsNav::new();
        nav.reset(true);
        assert!(
            !all_controls(SettingsPage::Root, true)
                .iter()
                .any(|c| matches!(c, Cell::Nav { page: Some(SettingsPage::Online), .. })),
            "inside a world, nothing on the root links to Online"
        );
        nav.cursor = online;
        assert_eq!(nav.enter(), SettingsOutcome::None);
        assert_eq!(nav.page(), SettingsPage::Root, "and must not move");
        // Done on the root closes; Done on a sub-page goes back.
        let mut nav = SettingsNav::new();
        nav.cursor = all_controls(SettingsPage::Root, false).len() - 1;
        assert_eq!(nav.enter(), SettingsOutcome::Close);
    }

    #[test]
    fn the_cursor_never_leaves_the_visible_window() {
        // `selected_row` is what draws the highlight; a `None` here means the
        // player is moving a cursor they cannot see.
        for page in PAGES {
            let mut nav = SettingsNav::new();
            nav.page = page;
            for _ in 0..all_controls(page, false).len() * 2 {
                assert!(
                    nav.selected_row().is_some(),
                    "{page:?}: cursor {} off-window at scroll={}",
                    nav.cursor(),
                    nav.scroll()
                );
                nav.step(true);
            }
            for _ in 0..all_controls(page, false).len() * 2 {
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
                let first_px = entry_offset(page.entries(), first);
                nav.scroll = first_px;
                for row in 0..nav.visible().len() {
                    nav.scroll = first_px;
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

    /// The anti-island assertion at this layer: every control of every page, at
    /// every scroll position, must land inside the canvas **horizontally**, and
    /// every control *inside the band* must land inside the band vertically. A
    /// `Placement` whose index ran past its arranged tree resolves to the -1000
    /// sentinel in `placement_anchor`, so it fails here rather than drawing
    /// nothing and looking like a table that was never wired.
    ///
    /// **The vertical bound changed with issue #445.** It used to require every
    /// control to fit the canvas, which was true only because `controls` emitted a
    /// window that excluded anything not wholly visible. Emitting every entry and
    /// letting `render::draw` clip is the conversion, so a control below the band
    /// now resolves below the canvas on purpose — asserting the canvas would be
    /// asserting the old implementation. The x bound is unchanged and still
    /// catches the sentinel, which is negative in **both** axes.
    #[test]
    fn every_placement_resolves_to_a_rect_on_screen() {
        let mut in_band = 0usize;
        for page in PAGES {
            for first in 0..page.entries().len().max(1) {
                let first_px = entry_offset(page.entries(), first);
                // `SettingsPage::Root` is an arranged widget grid, not an
                // `OptionsList` — its `entries()` is empty, so it has no band and
                // `MenuNav::active_list` reports no list for it. Its controls are
                // `Placement::Root`/`Footer`, which do not scroll, so the x bound
                // below still covers them and only the band bound is skipped.
                let (band_top, band_bottom) = match list_spec(page, first_px).model(320.0) {
                    Some(band) => (band.top(), band.top() + band.height()),
                    None => (f32::INFINITY, f32::NEG_INFINITY),
                };
                for in_world in [false, true] {
                    for control in controls(page, first_px, in_world) {
                        let (x, y) = placement_anchor(control.placement, 480.0, 320.0);
                        assert!(
                            x >= 0.0 && x + control.width <= 480.0,
                            "{page:?} first={first} in_world={in_world} {:?} at \
                             x={x} w={w} runs off a 480 px canvas — and the -1000 \
                             sentinel fails here too, since it is negative in x",
                            control.placement,
                            w = control.width
                        );
                        // A footer control does not scroll and sits below the band
                        // by construction; a list control above or below the band
                        // is clipped. Only what is inside the band is bounded.
                        if y < band_top || y > band_bottom {
                            continue;
                        }
                        in_band += 1;
                        assert!(
                            y >= 0.0 && y + WIDGET_H <= 320.0,
                            "{page:?} first={first} in_world={in_world} {:?} is \
                             inside the band and yet off the canvas at y={y}",
                            control.placement
                        );
                    }
                }
            }
        }
        assert!(
            in_band > 200,
            "premise: this must actually have examined controls inside the band \
             ({in_band} seen) — a filter that skipped everything would pass \
             vacuously"
        );
    }

    /// **One notch is `floor(DEFAULT_ITEM_HEIGHT / 2)` = `floor(25 / 2)` = 12 px**
    /// (issue #445), and the offset must coincide with no entry top.
    ///
    /// 25, not 20: [`DEFAULT_ITEM_HEIGHT`] is `AbstractSelectionList`'s
    /// `defaultEntryHeight` and [`WIDGET_H`] is the 20 px widget drawn *inside*
    /// it. `floor(20 / 2)` is 10, so a mix-up reports 10 here — named as an
    /// excluded hypothesis rather than left to chance, because 10 is exactly what
    /// three of the other four adopted screens correctly report.
    ///
    /// **The `heights` table does not change the notch, and that is the
    /// assertion.** `scrollRate` is defined against `defaultEntryHeight`, never
    /// against the height of the row you happen to be on — so a page whose first
    /// entry is a 31 px header still scrolls 12 px per notch. Video's entry 0 *is*
    /// a header, so this page exercises that rather than assuming it.
    #[test]
    fn one_wheel_notch_is_half_a_default_entry_and_ignores_the_heights_table() {
        const CANVAS_H: f32 = 240.0;
        let page = SettingsPage::Video;
        let mut nav = SettingsNav::new();
        nav.page = page;

        // Premise, executed: this page really does overflow the band, and its
        // first entry really is a header taller than DEFAULT_ITEM_HEIGHT — so the
        // "notch ignores the heights table" claim is being measured, not assumed.
        assert!(
            list_spec(page, 0.0)
                .model(CANVAS_H)
                .is_some_and(|l| l.scrollable()),
            "premise: the Video page must overflow the band at {CANVAS_H} px"
        );
        assert!(
            matches!(page.entries().first(), Some(Entry::Header(_))),
            "premise: entry 0 is a header, whose height is not DEFAULT_ITEM_HEIGHT"
        );
        assert_ne!(
            entry_height(page.entries(), 0),
            DEFAULT_ITEM_HEIGHT,
            "premise: and its height really does differ ({} vs {DEFAULT_ITEM_HEIGHT})",
            entry_height(page.entries(), 0)
        );
        assert_eq!(nav.scroll(), 0.0, "precondition: starts at the top");

        nav.scroll_by(-1.0, CANVAS_H);
        assert_eq!(
            nav.scroll(),
            12.0,
            "one notch must be floor(DEFAULT_ITEM_HEIGHT / 2) = floor(25 / 2) = 12"
        );
        assert_ne!(
            nav.scroll(),
            10.0,
            "control: 10 is floor(WIDGET_H / 2) — WIDGET_H is the widget drawn \
             inside a 25 px entry, not the entry pitch"
        );
        assert_ne!(
            nav.scroll(),
            DEFAULT_ITEM_HEIGHT,
            "control: 25 is the entry-index answer"
        );
        assert_ne!(
            nav.scroll(),
            (entry_height(page.entries(), 0) / 2.0).floor(),
            "control: and it must NOT be half the first entry's own height — the \
             scroll rate is defined against defaultEntryHeight"
        );

        nav.scroll_by(-2.0, CANVAS_H);
        assert_eq!(nav.scroll(), 36.0, "three notches: 36");

        // The cross-check: 36 must coincide with no entry top. Computed against
        // the real `heights` walk, not against a single pitch, because on this
        // page the tops are not evenly spaced.
        let tops: Vec<f32> = (0..page.entries().len())
            .map(|i| entry_offset(page.entries(), i))
            .collect();
        assert!(
            !tops.contains(&nav.scroll()),
            "the offset {} must land on no entry top; tops are {tops:?}",
            nav.scroll()
        );
    }

    /// The band `list_spec` declares must agree with [`row_left`] on where a row
    /// starts — two expressions from two modules, at four widths.
    ///
    /// `BIG_BUTTON_WIDTH` (310) is 155 either side of the centre, which is exactly
    /// `ROW_LEFT_INSET`, so `ListSpec::row_left` and this module's own
    /// `row_left(width, 0)` must coincide. Asserted rather than eyeballed because
    /// the scrollbar hangs off `row_right` and the columns off `row_left`.
    #[test]
    fn the_declared_band_agrees_with_this_screens_own_row_left() {
        for w in [640.0_f32, 854.0, 1280.0, 1920.0] {
            let spec = list_spec(SettingsPage::Video, 0.0);
            assert_eq!(
                spec.row_left(w),
                row_left(w, 0),
                "at {w} px the primitive's row_left must equal this screen's own"
            );
            assert_eq!(
                spec.row_w(w),
                BIG_BUTTON_WIDTH,
                "and the band is a full-width cell wide"
            );
            assert_eq!(ROW_LEFT_INSET * 2.0, BIG_BUTTON_WIDTH, "155 either side");
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

    /// The y `settings_frame` positioned the named header label at, resolved
    /// through the same `placement_anchor` the draw uses.
    fn header_y(frame: &MenuFrame<'_>, text: &str) -> Option<f32> {
        let label = frame.list_labels.iter().find(|l| l.text == text)?;
        Some(label.origin.anchor(480.0, crate::config::MIN_SCALED_HEIGHT as f32).1 + label.dy)
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
        let frame = settings_frame(&nav, &options, None);
        // **The split changed with issue #445**: a header scrolls, so it now
        // reaches `list_labels` — the vector `render::draw` clips to the band —
        // while the page title, which does not scroll, stays in `labels`. This
        // assertion is what pins the two apart.
        let titles: Vec<&str> = frame.labels.iter().map(|l| l.text.as_str()).collect();
        let headers: Vec<&str> = frame.list_labels.iter().map(|l| l.text.as_str()).collect();
        assert!(titles.contains(&"Video Settings"), "{titles:?}");
        assert!(
            !titles.contains(&"Display"),
            "a scrolling header must NOT be in `labels`, or it draws over the \
             footer once scrolled away: {titles:?}"
        );
        assert!(headers.contains(&"Display"), "{headers:?}");

        // **The control, and the behaviour it measures is deliberately different
        // now.** It used to read "scrolled past it, the header must be gone
        // rather than drawn at a stale position" — because `settings_frame`
        // emitted only the visible window, so absence *was* the mechanism.
        // Emitting every entry and clipping is the whole point of the
        // conversion, so the header is still in the vector; what must change is
        // its *position*, which has to leave the band. Asserting absence here
        // would now be asserting the old implementation.
        let before = header_y(&frame, "Display").expect("the header is positioned");
        nav.scroll = entry_offset(SettingsPage::Video.entries(), 7);
        let frame = settings_frame(&nav, &options, None);
        let after = header_y(&frame, "Display").expect("still emitted, now clipped");
        let band_top = list_spec(SettingsPage::Video, nav.scroll())
            .model(crate::config::MIN_SCALED_HEIGHT as f32)
            .expect("a band")
            .top();
        assert!(
            after < before,
            "scrolling down must move the header up: {before} -> {after}"
        );
        assert!(
            after < band_top,
            "and past the band's top ({band_top}), so `render::draw` clips it — \
             it is at {after}"
        );
        assert!(
            frame
                .labels
                .iter()
                .any(|l| l.text == "Video Settings"),
            "the title stays, and stays unscrolled"
        );
    }

    #[test]
    fn the_root_header_button_follows_vanillas_in_world_fork() {
        // `OptionsScreen.init`'s `if (this.inWorld)` (`:56-66`). Outside a
        // world the button is now a **live** link to `SettingsPage::Online`;
        // inside one it stays the inactive placeholder it always was. Both the
        // label and the liveness come from `SettingsNav::in_world`, set by
        // `reset`, so this asserts both rather than only the label the way the
        // pre-Online version of this test did.
        let options = crate::config::Options::default();
        let mut nav = SettingsNav::new();
        nav.reset(false);
        let out = settings_frame(&nav, &options, None);
        assert_eq!(out.rows[1].label, "Online...");
        assert!(out.rows[1].enabled, "outside a world it is live");

        nav.reset(true);
        let in_world = settings_frame(&nav, &options, None);
        assert_eq!(in_world.rows[1].label, "World Options...");
        assert!(!in_world.rows[1].enabled, "inside a world it is not");
    }

    #[test]
    fn a_save_failure_reaches_the_frame_rather_than_being_swallowed() {
        let options = crate::config::Options::default();
        let nav = SettingsNav::new();
        let frame = settings_frame(&nav, &options, Some("could not save options.json"));
        assert!(
            frame
                .labels
                .iter()
                .any(|l| l.text.contains("could not save")),
            "a `vanilla` frame draws no `message`, so it has to be a label"
        );
        // The control: no error, no label.
        let clean = settings_frame(&nav, &options, None);
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
        nav.scroll = 4.0 * DEFAULT_ITEM_HEIGHT;
        nav.in_world = true;
        nav.reset(false);
        assert_eq!(nav.page(), SettingsPage::Root);
        assert_eq!((nav.cursor(), nav.scroll()), (0, 0.0));
        assert!(!nav.in_world, "reset must also re-derive in_world, not carry the old visit's over");
        assert_eq!(nav.escape(), SettingsOutcome::Close, "with an empty stack");
    }

    /// Plain AABB overlap, half-open on the touching-edge case (two rects that
    /// share an edge do not overlap) — used only by
    /// [`no_settings_title_ever_overlaps_a_widget`], below.
    fn rects_intersect(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
        let (ax, ay, aw, ah) = a;
        let (bx, by, bw, bh) = b;
        ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + ah
    }

    #[test]
    fn rect_intersection_predicate_has_a_working_control() {
        // Per this repo's own rule that an absence needs a control proving the
        // detector *can* fire: two manufactured overlaps and two manufactured
        // non-overlaps (one disjoint, one edge-touching), checked before this
        // predicate is trusted to find nothing wrong below.
        assert!(rects_intersect((0.0, 0.0, 10.0, 10.0), (5.0, 5.0, 10.0, 10.0)));
        assert!(
            !rects_intersect((0.0, 0.0, 10.0, 10.0), (10.0, 0.0, 10.0, 10.0)),
            "sharing an edge is not overlapping"
        );
        assert!(!rects_intersect((0.0, 0.0, 10.0, 10.0), (20.0, 20.0, 10.0, 10.0)));
    }

    #[test]
    fn no_settings_title_ever_overlaps_a_widget() {
        // The player report this exists for (2026-08-04): "the 'Options' text
        // at the top is intersecting some buttons". The cause was
        // `settings_frame`'s title `dy` double-counting the root's already-
        // absolute anchor (see that assignment's own doc) — fixed there, and
        // this is the gate that would have caught it, because it walks
        // `settings_frame`'s **own output**, the same `MenuLabel`/`Slot`s
        // `build` draws from, rather than a hand-derived y. Covers every
        // `OptionsList`-shaped page, both `in_world` states (the root's header
        // button changes, nothing else does — see `controls`'s doc), two
        // canvases standing in for two GUI scales, and — since this client
        // carries no second locale to source a real long title from — a
        // synthetic stand-in a good deal longer than the longest real one
        // (`"Accessibility Settings"`, 22 chars) for "more than one language
        // string width".
        let font = crate::hud::VanillaFont::shared();
        let text_width = |s: &str, scale: f32| match &font {
            Some(f) => f.width(s, scale),
            None => crate::menu::render::text_px(s, scale),
        };
        let options = crate::config::Options::default();
        let long_stand_in = "A Considerably Longer Hypothetical Localized Options Title";

        for &(w, h) in &[(320.0f32, 240.0f32), (854.0, 480.0)] {
            for page in PAGES {
                for in_world in [false, true] {
                    let mut nav = SettingsNav::new();
                    nav.page = page;
                    nav.in_world = in_world;
                    let frame = settings_frame(&nav, &options, None);
                    for title in [page.title(), long_stand_in] {
                        let title_label = &frame.labels[0];
                        let (ax, ay) = title_label.origin.anchor(w, h);
                        let tw = text_width(title, title_label.scale);
                        let tx = match title_label.align {
                            Align::Centre => (ax + title_label.dx - tw * 0.5).floor(),
                            Align::Left => ax + title_label.dx,
                            Align::Right => ax + title_label.dx - tw,
                        };
                        let title_rect = (tx, ay + title_label.dy, tw, HEADER_LINE_HEIGHT);

                        for row in &frame.rows {
                            let Some(slot) = row.slot else { continue };
                            let widget_rect = slot.resolve(w, h);
                            assert!(
                                !rects_intersect(title_rect, widget_rect),
                                "{page:?} in_world={in_world} title {title:?} \
                                 {title_rect:?} overlaps {widget_rect:?} at \
                                 canvas {w}x{h}"
                            );
                        }
                    }
                }
            }
        }
    }
}
