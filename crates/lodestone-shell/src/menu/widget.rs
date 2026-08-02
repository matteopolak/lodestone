//! Vanilla's **menu widget contract** — `AbstractWidget`, `AbstractButton`,
//! `Button` and `WidgetSprites` — and with it the **disabled render path**.
//!
//! ## What it is
//!
//! One type, [`Widget`], that owns a menu control's bounds, message and state
//! (`active` / `visible` / `focused`) and answers the two questions a screen used
//! to answer for itself: **which sprite is my background** and **what colour is
//! my label**. [`super::render`]'s `draw_widget` is its consumer; the title
//! screen, the pause menu, the death screen and the account screen's action row
//! all draw through it, so there is one definition of vanilla's rules rather than
//! one per screen.
//!
//! This is the first child of the menu-framework epic (#392/#393). The layout
//! containers that arrange these (#394) live in [`super::layout`] and attach
//! through [`LayoutElement`]; focus/tab dispatch (#395) is still absent.
//!
//! ## The disabled path is `active = false` and nothing else
//!
//! There is **no disabled widget type** in vanilla. Setting `active = false` does
//! exactly two visible things:
//!
//! 1. **The sprite changes.** [`WidgetSprites::get`] is handed `active` and
//!    routes to the disabled member of a four-state record.
//! 2. **The label goes grey.** `AbstractWidget.WithInactiveMessage` merges
//!    `Style.EMPTY.withColor(-6250336)` into the message
//!    (`AbstractWidget.java:314-335`) — that is [`INACTIVE_MESSAGE_ARGB`], and it
//!    is the *whole* visual difference for a widget with no disabled sprite.
//!
//! Plus one invisible thing: `AbstractWidget.nextFocusPath` returns `null` when
//! `isActive()` is false (`AbstractWidget.java:152-158`), so an inactive widget
//! is unreachable by **keyboard**, not merely unclickable. [`Widget::takes_focus`]
//! is that predicate; [`super::nav::MenuNav`]'s `step_enabled` is what already
//! implements it for the two converted screens.
//!
//! **Do not invent disabled art for `Checkbox`, `EditBox` or
//! `AbstractSliderButton`.** Verified against the 26.2 jar rather than taken on
//! trust: `Checkbox.java` and `AbstractSliderButton.java` do not mention
//! `WidgetSprites` at all and pick between a plain and a `_highlighted` sprite by
//! hand (`Checkbox.java:109-117`, `AbstractSliderButton.java:38,42`), and
//! `EditBox.java:29-31` uses the **two**-argument `WidgetSprites` constructor —
//! `widget/text_field` and `widget/text_field_highlighted` — which collapses
//! `disabled` onto `enabled`. All three rely on the grey label plus blocked input.
//! Constructing them with [`Widget::new`] (no sprites) rather than
//! [`Widget::button`] is how that is expressed here, and
//! `a_spriteless_widget_has_no_disabled_art` asserts it.
//!
//! ## How to change it
//!
//! - **Add state to [`Widget`], not to a screen.** The point of the type is that
//!   the third screen does not write the blit a third time.
//! - **The sprite's second argument is `isHoveredOrFocused()`, not
//!   `isFocused()`.** This is worth reading twice, because both #393's body and
//!   `docs/ui-framework.md` say `isFocused()` and the jar disagrees:
//!   `AbstractButton.extractDefaultSprite` passes
//!   `SPRITES.get(this.active, … this.isHoveredOrFocused())`
//!   (`AbstractButton.java:43-53`), and `isHoveredOrFocused()` is
//!   `isHovered() || isFocused()` (`AbstractWidget.java:211-213`).
//!
//!   #393 carried both facts in one `focused` field, because the shell had a
//!   single row cursor that the keyboard *and* [`super::nav::MenuNav::hover`]
//!   moved. **#395 split them**: [`super::focus::FocusSet`] owns real keyboard
//!   focus, so [`Widget::hovered`] is now its own field and
//!   [`Widget::is_hovered_or_focused`] is the join. Keep it an `||`. Dropping
//!   either side compiles, passes every existing test that sets only `focused`,
//!   and changes how every button in the client highlights.
//!
//!   [`super::edit_box::EditBox`] does **not** share this: `EditBox.java:407`
//!   passes `isFocused()` alone, so hovering a text field does not draw its
//!   highlighted sprite. The `||` belongs to the button, not to the widget
//!   contract.
//! - **The two `get` arguments are not the same predicate.** `AbstractButton`
//!   passes the raw `active` field, while `EditBox.java:407` passes
//!   `isActive()` (i.e. `visible && active`). [`Widget::background_sprite`]
//!   follows the button, because a widget that is not visible is not drawn at all
//!   (`AbstractWidget.java:56-62`) so the distinction is unobservable for it.
//! - **Never restate a colour or a sprite id in a screen.** Read
//!   [`BUTTON_SPRITES`] and [`Widget::message_colour`]; the grey is *derived*
//!   from vanilla's own signed ARGB constant by [`argb_to_rgba`], with
//!   `vanillas_inactive_grey_is_derived_not_transcribed` proving the derivation
//!   rather than a hand-typed `160.0 / 255.0` agreeing with itself.
//! - **`button_disabled`'s nine-slice border is 1 where its siblings' are 3.**
//!   Nothing here encodes any of them: [`lodestone_render::GuiAtlas`] reads each
//!   from the sibling `.png.mcmeta`. Measured in #66; do not hardcode.
//!
//! ## Not here, on purpose
//!
//! **No tooltip.** Vanilla's `WidgetTooltipHolder` is what makes "disabled with
//! an explanation" honest (`TitleScreen.java:196`,
//! `OptionsScreen.java:88-92`), and it belongs on this type eventually — but
//! nothing in this shell draws a hover tooltip, so a `tooltip` field would reach
//! zero pixels. It lands with the screen-level input layer (#395), which is what
//! knows how long the cursor has rested.
//!
//! ## Dependencies
//!
//! None beyond `core` — the type is pure data and arithmetic. Sprite ids are
//! resolved by [`lodestone_render::GuiAtlas`] at draw time, in
//! [`super::render`].

/// Vanilla's inactive-message colour as the **signed** ARGB integer the jar
/// writes: `Style.EMPTY.withColor(-6250336)` in
/// `AbstractWidget.WithInactiveMessage.defaultInactiveMessage`
/// (`AbstractWidget.java:318`).
///
/// `-6250336 as u32` is `0xFF_A0_A0_A0` — opaque grey 160. Kept in vanilla's own
/// spelling so it can be grepped for in the decompiled source, with
/// [`argb_to_rgba`] doing the conversion.
pub const INACTIVE_MESSAGE_ARGB: i32 = -6250336;

/// An inactive widget's label colour: [`INACTIVE_MESSAGE_ARGB`] unpacked.
///
/// sRGB 0..1 written verbatim, which is this shell's convention for GUI text
/// (see `docs/vanilla-hud-text.md`) — vanilla is not colour-managed, so a menu
/// label's channel values go to the framebuffer as stored.
pub const INACTIVE_LABEL: [f32; 4] = [160.0 / 255.0, 160.0 / 255.0, 160.0 / 255.0, 1.0];

/// An active widget's label colour: plain white.
///
/// `AbstractButton` tints only the *sprite* with `ARGB.white(this.alpha)`
/// (`AbstractButton.java:51`); the label keeps the component's own default, which
/// for every menu button is white.
pub const ACTIVE_LABEL: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// `Button.SMALL_WIDTH` (`Button.java:12`).
pub const SMALL_WIDTH: f32 = 120.0;
/// `Button.DEFAULT_WIDTH` (`Button.java:13`).
pub const DEFAULT_WIDTH: f32 = 150.0;
/// `Button.BIG_WIDTH` (`Button.java:14`) — the title screen's full-width rows.
pub const BIG_WIDTH: f32 = 200.0;
/// `Button.DEFAULT_HEIGHT` (`Button.java:15`) — every menu button's height.
pub const DEFAULT_HEIGHT: f32 = 20.0;
/// `Button.DEFAULT_SPACING` (`Button.java:16`), for #394's layout containers.
pub const DEFAULT_SPACING: f32 = 8.0;

/// `AbstractButton.TEXT_MARGIN` (`AbstractButton.java:17`): the inset the label's
/// scroll window is measured from, passed as
/// `extractScrollingStringOverContents(output, message, 2)`
/// (`AbstractButton.java:39-41`).
pub const TEXT_MARGIN: f32 = 2.0;

/// Unpacks a vanilla signed-ARGB integer into this shell's `[r, g, b, a]` in
/// 0..1.
///
/// Deliberately **not** a `const fn`: it exists so that
/// [`INACTIVE_LABEL`]-shaped constants can be *checked* against vanilla's
/// integer in a test rather than each being a transcription that agrees with
/// itself. `decode(encode(x)) == x` is satisfied by two symmetric
/// misunderstandings; an integer lifted verbatim out of the jar is not.
#[must_use]
pub fn argb_to_rgba(argb: i32) -> [f32; 4] {
    let bits = argb as u32;
    [
        ((bits >> 16) & 0xFF) as f32 / 255.0,
        ((bits >> 8) & 0xFF) as f32 / 255.0,
        (bits & 0xFF) as f32 / 255.0,
        ((bits >> 24) & 0xFF) as f32 / 255.0,
    ]
}

/// Vanilla's `WidgetSprites` record (`WidgetSprites.java:5`): the four sprite ids
/// a widget picks between, keyed by `(enabled, focused)`.
///
/// The three collapsing constructors are vanilla's own, and the collapse is the
/// interesting part — it is how a widget declares that it has *no* disabled art
/// without a second type:
///
/// | vanilla arity | ours | result |
/// |---|---|---|
/// | 1 | [`Self::uniform`] | all four the same |
/// | 2 | [`Self::focusable`] | `(sprite, sprite, focused, focused)` |
/// | 3 | [`Self::with_disabled`] | `(enabled, disabled, focused, disabled)` |
///
/// Note what the 3-argument form does with the fourth field: `disabledFocused`
/// is the **disabled** sprite, not the focused one. That is why a greyed-out
/// button under the cursor still looks greyed out, and it is the rule most
/// likely to be got wrong by a hand-rolled highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetSprites {
    /// Drawn when enabled and neither hovered nor focused.
    pub enabled: &'static str,
    /// Drawn when disabled and neither hovered nor focused.
    pub disabled: &'static str,
    /// Drawn when enabled and hovered or focused.
    pub enabled_focused: &'static str,
    /// Drawn when disabled and hovered or focused — equal to [`Self::disabled`]
    /// for every vanilla widget that uses [`Self::with_disabled`].
    pub disabled_focused: &'static str,
}

impl WidgetSprites {
    /// All four ids given explicitly — vanilla's canonical record constructor.
    #[must_use]
    pub const fn new(
        enabled: &'static str,
        disabled: &'static str,
        enabled_focused: &'static str,
        disabled_focused: &'static str,
    ) -> Self {
        Self {
            enabled,
            disabled,
            enabled_focused,
            disabled_focused,
        }
    }

    /// Vanilla's 1-argument constructor (`WidgetSprites.java:6-8`): one sprite
    /// for every state.
    #[must_use]
    pub const fn uniform(sprite: &'static str) -> Self {
        Self::new(sprite, sprite, sprite, sprite)
    }

    /// Vanilla's 2-argument constructor (`WidgetSprites.java:10-12`): a focused
    /// variant but **no disabled art** — `EditBox`'s form.
    #[must_use]
    pub const fn focusable(sprite: &'static str, focused: &'static str) -> Self {
        Self::new(sprite, sprite, focused, focused)
    }

    /// Vanilla's 3-argument constructor (`WidgetSprites.java:14-16`):
    /// `(enabled, disabled, focused, disabled)` — `AbstractButton`'s form.
    #[must_use]
    pub const fn with_disabled(
        enabled: &'static str,
        disabled: &'static str,
        focused: &'static str,
    ) -> Self {
        Self::new(enabled, disabled, focused, disabled)
    }

    /// `WidgetSprites.get(enabled, focused)` (`WidgetSprites.java:18-24`),
    /// transcribed branch for branch.
    #[must_use]
    pub const fn get(self, enabled: bool, focused: bool) -> &'static str {
        if enabled {
            if focused {
                self.enabled_focused
            } else {
                self.enabled
            }
        } else if focused {
            self.disabled_focused
        } else {
            self.disabled
        }
    }
}

/// `AbstractButton.SPRITES` (`AbstractButton.java:18-22`): the three
/// `widget/button*` ids every menu button selects between, through vanilla's
/// 3-argument collapse.
///
/// All three are `nine_slice` in the pack and their border widths are read from
/// the sibling `.png.mcmeta` by [`lodestone_render::GuiAtlas`], **not** stated
/// here — which matters, because `widget/button_disabled`'s border is **1**
/// while the other two are **3** (measured in #66).
pub const BUTTON_SPRITES: WidgetSprites = WidgetSprites::with_disabled(
    "widget/button",
    "widget/button_disabled",
    "widget/button_highlighted",
);

/// Vanilla's `AbstractWidget` (`AbstractWidget.java:28-48`): a menu control's
/// bounds, message and state.
///
/// Field names and defaults follow the jar: `active` and `visible` are `true`,
/// `focused` is `false`, and both `active` and `visible` are public because they
/// are public fields there too (`AbstractWidget.java:35-36`) — every vanilla
/// disable site is a plain `button.active = …` assignment
/// (`OptionsSubScreen.java:43-46`, `TitleScreen.java:196`).
#[derive(Debug, Clone, PartialEq)]
pub struct Widget {
    /// Left edge, in logical GUI pixels.
    pub x: f32,
    /// Top edge, in logical GUI pixels.
    pub y: f32,
    /// Width in logical GUI pixels.
    pub width: f32,
    /// Height in logical GUI pixels.
    pub height: f32,
    /// The label. Carried even for an icon-only widget, where vanilla uses it as
    /// the narration and tooltip text rather than drawing it.
    pub message: String,
    /// Whether the widget can be interacted with. `false` is the **entire**
    /// disabled API — see the module docs.
    pub active: bool,
    /// Whether the widget is drawn at all. `AbstractWidget.extractRenderState`
    /// wraps everything in `if (this.visible)` (`AbstractWidget.java:56-62`).
    pub visible: bool,
    /// `AbstractWidget.focused` — **keyboard** focus alone
    /// (`AbstractWidget.java:211-218`).
    ///
    /// This used to carry `isHoveredOrFocused()`, because the shell had one row
    /// cursor that both the keyboard and the mouse moved. #395 split them:
    /// [`super::focus::FocusSet`] owns real focus, so hover is now a separate
    /// fact and [`Self::hovered`] holds it. The sprite predicate joins the two
    /// with `||` — see [`Self::is_hovered_or_focused`], which is the one place
    /// that must not pick a side.
    pub focused: bool,
    /// `AbstractWidget.isHovered`, set from **geometry alone** every frame
    /// (`AbstractWidget.java:56-62`) and never consulted about `active` — which
    /// is why a greyed-out button under the cursor still looks greyed out rather
    /// than vanishing.
    pub hovered: bool,
    /// The background sprite set, or `None` for a widget with no sprite
    /// background of its own (the `Checkbox`/`EditBox`/slider family).
    pub sprites: Option<WidgetSprites>,
    /// A sprite drawn centred **instead of** [`Self::message`] — vanilla's
    /// `SpriteIconButton.CenteredIcon` (`SpriteIconButton.java:236-244`).
    pub icon: Option<&'static str>,
}

impl Default for Widget {
    /// A zero-sized, unlabelled, **active and visible** widget — vanilla's own
    /// field initialisers, which is why this is not `derive`d (that would give
    /// `active = false` and quietly grey everything out).
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            message: String::new(),
            active: true,
            visible: true,
            focused: false,
            hovered: false,
            sprites: None,
            icon: None,
        }
    }
}

impl Widget {
    /// A widget with **no** background sprite set: the `Checkbox` / `EditBox` /
    /// `AbstractSliderButton` shape, whose disabled state is the grey label and
    /// blocked input alone.
    #[must_use]
    pub fn new(x: f32, y: f32, width: f32, height: f32, message: impl Into<String>) -> Self {
        Self {
            x,
            y,
            width,
            height,
            message: message.into(),
            ..Self::default()
        }
    }

    /// A `Button`: [`Self::new`] plus [`BUTTON_SPRITES`].
    #[must_use]
    pub fn button(x: f32, y: f32, width: f32, height: f32, message: impl Into<String>) -> Self {
        Self {
            sprites: Some(BUTTON_SPRITES),
            ..Self::new(x, y, width, height, message)
        }
    }

    /// `AbstractWidget.isActive()` (`AbstractWidget.java:216-218`):
    /// `visible && active`.
    ///
    /// Not the same question as [`Self::active`], and the difference is what
    /// makes an invisible widget unfocusable as well as undrawn.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.visible && self.active
    }

    /// `(x, y, width, height)` — vanilla's `LayoutElement.getRectangle()`.
    #[must_use]
    pub fn rect(&self) -> (f32, f32, f32, f32) {
        (self.x, self.y, self.width, self.height)
    }

    /// Whether `(mx, my)` is inside the bounds, ignoring state — vanilla's
    /// `areCoordinatesInRectangle`. Inclusive on both edges, matching
    /// `super::render::row_rect`'s hit-test in `app.rs` so the two cannot
    /// disagree about a boundary pixel.
    #[must_use]
    pub fn contains(&self, mx: f32, my: f32) -> bool {
        mx >= self.x && mx <= self.x + self.width && my >= self.y && my <= self.y + self.height
    }

    /// `AbstractWidget.isMouseOver` (`AbstractWidget.java:160-163`):
    /// `isActive() && areCoordinatesInRectangle(..)`.
    ///
    /// This is the **click** half of the disabled path — the half that matters,
    /// because vanilla still *hovers* a disabled widget (`isHovered` is set from
    /// geometry alone, `AbstractWidget.java:58`) and merely refuses to act on it.
    #[must_use]
    pub fn is_mouse_over(&self, mx: f32, my: f32) -> bool {
        self.is_active() && self.contains(mx, my)
    }

    /// Whether a focus-navigation event would land on this widget:
    /// `AbstractWidget.nextFocusPath` returns a leaf only when `isActive()` and
    /// not already focused, and `null` otherwise (`AbstractWidget.java:152-158`).
    ///
    /// So an inactive widget is skipped by Tab and by the arrow keys, not just
    /// unclickable — which is what
    /// [`super::nav::MenuNav`]'s `step_enabled` already does for the two
    /// converted screens.
    #[must_use]
    pub fn takes_focus(&self) -> bool {
        self.is_active() && !self.focused
    }

    /// `AbstractWidget.isHoveredOrFocused()` (`AbstractWidget.java:211-213`):
    /// `isHovered() || isFocused()`.
    ///
    /// **The `||` is the whole point of this method existing.** #393 collapsed
    /// hover and focus into one flag because the shell had a single row cursor;
    /// #395 split them, and this is the exact join where picking one of the two
    /// would silently change every button's highlight — a focused-but-unhovered
    /// button would stop lighting up, or a hovered-but-unfocused one would.
    /// Neither shows up in a `cargo check`.
    #[must_use]
    pub fn is_hovered_or_focused(&self) -> bool {
        self.hovered || self.focused
    }

    /// The background sprite id, or `None` when this widget has no sprite
    /// background.
    ///
    /// `AbstractButton.extractDefaultSprite` (`AbstractButton.java:43-53`):
    /// `SPRITES.get(this.active, this.isHoveredOrFocused())`. Note the first
    /// argument is the raw `active` field, not `isActive()` — see the module
    /// docs — and the second is [`Self::is_hovered_or_focused`], **not**
    /// [`Self::focused`].
    ///
    /// `EditBox` differs on *both* arguments: `EditBox.java:407` is
    /// `SPRITES.get(this.isActive(), this.isFocused())` — hover does not
    /// highlight a text field, only focus does. That is
    /// [`super::edit_box::EditBox::background_sprite`], deliberately not this.
    #[must_use]
    pub fn background_sprite(&self) -> Option<&'static str> {
        self.sprites
            .map(|s| s.get(self.active, self.is_hovered_or_focused()))
    }

    /// The label colour: [`ACTIVE_LABEL`] or, when inactive, [`INACTIVE_LABEL`].
    ///
    /// `AbstractWidget.WithInactiveMessage.getMessage()` is
    /// `this.active ? super.getMessage() : this.inactiveMessage`
    /// (`AbstractWidget.java:326-329`) — keyed on `active`, like the sprite, not
    /// on `isActive()`.
    #[must_use]
    pub fn message_colour(&self) -> [f32; 4] {
        if self.active { ACTIVE_LABEL } else { INACTIVE_LABEL }
    }

    /// The horizontal window the label is centred in: `(x + 2, x + width - 2)`.
    ///
    /// `extractScrollingStringOverContents(output, message, TEXT_MARGIN)` →
    /// `left = getX() + margin`, `right = getX() + getWidth() - margin`
    /// (`AbstractWidget.java:92-98`).
    #[must_use]
    pub fn content_span(&self) -> (f32, f32) {
        (self.x + TEXT_MARGIN, self.x + self.width - TEXT_MARGIN)
    }

    /// The label's top row for a font of `line_height`:
    /// `(top + bottom - lineHeight) / 2 + 1`
    /// (`ActiveTextCollector.java:59,73`, reached through
    /// `acceptScrollingWithDefaultCenter`).
    ///
    /// Floored before the `+ 1`, which is what integer arithmetic in the jar
    /// does and where a half-pixel of label drift would come from.
    #[must_use]
    pub fn label_top(&self, line_height: f32) -> f32 {
        ((self.y + self.y + self.height - line_height) / 2.0).floor() + 1.0
    }

    /// The top-left of a `side`×`side` icon centred in the widget.
    ///
    /// `SpriteIconButton.CenteredIcon.extractContents`
    /// (`SpriteIconButton.java:236-244`); `spriteOffset` is zero at every call
    /// site, so this is a plain centre. Floored, so the icon lands on a pixel.
    #[must_use]
    pub fn icon_rect(&self, side: f32) -> (f32, f32) {
        (
            (self.x + (self.width - side) * 0.5).floor(),
            (self.y + (self.height - side) * 0.5).floor(),
        )
    }
}

/// Vanilla's `LayoutElement` (`gui/layouts/LayoutElement.java`), the interface
/// every `AbstractLayout` arranges.
///
/// **The seam.** [`super::layout`] (#394) ports `LinearLayout`,
/// `HeaderAndFooterLayout`, `FrameLayout` and `GridLayout`; all any of them needs
/// of a child is to read its size, *write* its position, and hand its leaves to a
/// screen — which is exactly this trait. `Debug` is a supertrait so a container
/// holding `Box<dyn LayoutElement>` children can still derive it (the workspace
/// warns on `missing_debug_implementations`); every implementor here is plain
/// data, so it costs nothing.
///
/// Two methods are not where vanilla puts them, both deliberately:
///
/// - **`visitWidgets`** is here rather than only on `Layout`, as in vanilla
///   (`LayoutElement.java:29`), and it is **required** — vanilla makes it
///   abstract too, which is why `SpacerElement` has to write an explicit empty
///   body (`SpacerElement.java:61-63`). A defaulted no-op would let a future
///   element type silently never reach a screen.
/// - **`arrange_elements`** is here with a no-op default, where vanilla has it on
///   `Layout` and tests `child instanceof Layout` in the default body
///   (`Layout.java:14-20`). Behaviourally identical, and it saves a downcast from
///   `dyn LayoutElement`.
pub trait LayoutElement: core::fmt::Debug {
    /// `getX()`.
    fn x(&self) -> f32;
    /// `getY()`.
    fn y(&self) -> f32;
    /// `getWidth()`.
    fn width(&self) -> f32;
    /// `getHeight()`.
    fn height(&self) -> f32;
    /// `setX(int)`.
    fn set_x(&mut self, x: f32);
    /// `setY(int)`.
    fn set_y(&mut self, y: f32);

    /// `visitWidgets(Consumer<AbstractWidget>)`: hand every drawable leaf under
    /// this element to `visitor`, in insertion order.
    ///
    /// This is the only route from a layout tree to a draw — vanilla's screens
    /// are literally `layout.visitWidgets(this::addRenderableWidget)`
    /// (`PauseScreen.java:182`) — and the reason a `SpacerElement` is measured
    /// but never drawn.
    fn visit_widgets(&self, visitor: &mut dyn FnMut(&Widget));

    /// `setPosition(int, int)` — a default, as in vanilla.
    fn set_position(&mut self, x: f32, y: f32) {
        self.set_x(x);
        self.set_y(y);
    }

    /// `getRectangle()` as `(x, y, width, height)` — a default, as in vanilla.
    fn rectangle(&self) -> (f32, f32, f32, f32) {
        (self.x(), self.y(), self.width(), self.height())
    }

    /// `Layout.arrangeElements()`: size this element from its children and place
    /// them. A no-op for a leaf, which is what makes the recursion in
    /// [`super::layout`]'s containers a plain `visit_children`.
    fn arrange_elements(&mut self) {}
}

impl LayoutElement for Widget {
    fn x(&self) -> f32 {
        self.x
    }

    fn y(&self) -> f32 {
        self.y
    }

    fn width(&self) -> f32 {
        self.width
    }

    fn height(&self) -> f32 {
        self.height
    }

    fn set_x(&mut self, x: f32) {
        self.x = x;
    }

    fn set_y(&mut self, y: f32) {
        self.y = y;
    }

    /// A widget *is* a leaf: it visits itself, exactly as
    /// `AbstractWidget.visitWidgets` does (`AbstractWidget.java:282-284`).
    fn visit_widgets(&self, visitor: &mut dyn FnMut(&Widget)) {
        visitor(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanillas_inactive_grey_is_derived_not_transcribed() {
        // The expected value originates **outside** this file's own constant:
        // `-6250336` is lifted verbatim from `AbstractWidget.java:318`, and
        // unpacking it must land on `INACTIVE_LABEL`. Without this the array
        // would only ever agree with itself.
        assert_eq!(argb_to_rgba(INACTIVE_MESSAGE_ARGB), INACTIVE_LABEL);
        // And the channel values that agreement implies, stated so a future
        // reader can check them against the jar without running anything:
        // 0xFFA0A0A0 -> opaque grey 160.
        assert_eq!(INACTIVE_MESSAGE_ARGB as u32, 0xFF_A0_A0_A0);
        // The control: a *different* vanilla colour must not unpack to the same
        // thing, or the detector is insensitive to the input. `EditBox`'s
        // `DEFAULT_TEXT_COLOR` is -2039584 (`EditBox.java:34`).
        assert_ne!(argb_to_rgba(-2_039_584), INACTIVE_LABEL);
        // White, for the active side.
        assert_eq!(argb_to_rgba(-1), ACTIVE_LABEL);
    }

    #[test]
    fn the_collapsing_constructors_match_vanillas_record() {
        // `WidgetSprites.java:6-16`. The 3-argument form's fourth field is the
        // one that carries the whole "disabled wins over hovered" rule.
        let one = WidgetSprites::uniform("a");
        assert_eq!(
            (one.enabled, one.disabled, one.enabled_focused, one.disabled_focused),
            ("a", "a", "a", "a")
        );
        let two = WidgetSprites::focusable("a", "f");
        assert_eq!(
            (two.enabled, two.disabled, two.enabled_focused, two.disabled_focused),
            ("a", "a", "f", "f")
        );
        let three = WidgetSprites::with_disabled("e", "d", "f");
        assert_eq!(
            (
                three.enabled,
                three.disabled,
                three.enabled_focused,
                three.disabled_focused
            ),
            ("e", "d", "f", "d"),
            "the 3-argument form's disabledFocused is the DISABLED sprite"
        );
    }

    #[test]
    fn get_routes_all_four_states_and_disabled_beats_focused() {
        assert_eq!(BUTTON_SPRITES.get(true, false), "widget/button");
        assert_eq!(BUTTON_SPRITES.get(true, true), "widget/button_highlighted");
        assert_eq!(BUTTON_SPRITES.get(false, false), "widget/button_disabled");
        assert_eq!(
            BUTTON_SPRITES.get(false, true),
            "widget/button_disabled",
            "a hovered disabled button must not draw the highlighted sprite"
        );
        // All four states are distinguishable for a set that *does* have four
        // distinct members, so `get` is not accidentally ignoring an argument.
        let four = WidgetSprites::new("e", "d", "ef", "df");
        assert_eq!(
            [
                four.get(true, false),
                four.get(true, true),
                four.get(false, false),
                four.get(false, true)
            ],
            ["e", "ef", "d", "df"]
        );
    }

    #[test]
    fn a_spriteless_widget_has_no_disabled_art() {
        // `Checkbox`, `EditBox` and `AbstractSliderButton` have none in the jar
        // (see the module docs). A `None` here is what stops one being invented.
        let mut w = Widget::new(0.0, 0.0, 20.0, 20.0, "Fancy Graphics");
        assert_eq!(w.background_sprite(), None);
        w.active = false;
        assert_eq!(
            w.background_sprite(),
            None,
            "disabling a spriteless widget must not conjure a sprite"
        );
        // Its whole visible disabled state is the grey label.
        assert_eq!(w.message_colour(), INACTIVE_LABEL);
        // The control: the same widget with a sprite set does change art.
        let mut b = Widget::button(0.0, 0.0, 200.0, 20.0, "Options...");
        assert_eq!(b.background_sprite(), Some("widget/button"));
        b.active = false;
        assert_eq!(b.background_sprite(), Some("widget/button_disabled"));
    }

    #[test]
    fn a_widget_is_active_and_visible_by_default() {
        // `AbstractWidget.java:35-36` — and a `derive(Default)` would give the
        // opposite, greying out every widget built from `..Default::default()`.
        let w = Widget::default();
        assert!(w.active && w.visible && !w.focused);
        assert!(w.is_active());
        assert_eq!(w.message_colour(), ACTIVE_LABEL);
    }

    #[test]
    fn invisible_is_inactive_but_still_carries_its_active_flag() {
        // `isActive()` is `visible && active` (`AbstractWidget.java:216-218`),
        // while the *sprite* and the *label* key on the raw `active` field.
        let mut w = Widget::button(0.0, 0.0, 200.0, 20.0, "Multiplayer");
        w.visible = false;
        assert!(!w.is_active(), "an invisible widget is not active");
        assert!(w.active, "but its own flag is untouched");
        assert_eq!(
            w.background_sprite(),
            Some("widget/button"),
            "vanilla keys the sprite on `active`, not `isActive()`"
        );
        assert!(!w.takes_focus(), "and focus navigation must skip it");
        assert!(!w.is_mouse_over(100.0, 10.0), "as must a click");
    }

    #[test]
    fn an_inactive_widget_is_unreachable_by_keyboard_and_by_click() {
        // The invisible half of the disabled path: `nextFocusPath` returns null
        // when `!isActive()` (`AbstractWidget.java:152-158`), so Tab skips it.
        let mut w = Widget::button(10.0, 20.0, 200.0, 20.0, "Minecraft Realms");
        assert!(w.takes_focus());
        assert!(w.is_mouse_over(50.0, 25.0));
        w.active = false;
        assert!(!w.takes_focus());
        assert!(!w.is_mouse_over(50.0, 25.0));
        // But it is still *hovered* — vanilla sets `isHovered` from geometry
        // alone and never consults `active` (`AbstractWidget.java:58`), which is
        // why a greyed-out button under the cursor looks greyed out rather than
        // vanishing.
        assert!(w.contains(50.0, 25.0));
        // Already-focused is the other `null` branch, and it is not the same as
        // inactive: an active, focused widget also yields no new focus path.
        let mut focused = Widget::button(0.0, 0.0, 200.0, 20.0, "Singleplayer");
        focused.focused = true;
        assert!(!focused.takes_focus());
        assert!(focused.is_active(), "but it is still active and clickable");
    }

    #[test]
    fn the_sprite_predicate_is_hovered_or_focused_and_stays_an_or() {
        // #395 split hover from focus. This is the join: `AbstractButton` passes
        // `isHoveredOrFocused()`, so *either* alone must light the button up and
        // the two must be independently observable — which is the thing a port
        // that quietly picks one side still compiles through.
        let mut w = Widget::button(0.0, 0.0, 200.0, 20.0, "Singleplayer");
        assert!(!w.is_hovered_or_focused());
        assert_eq!(w.background_sprite(), Some("widget/button"));

        w.focused = true;
        assert!(w.is_hovered_or_focused(), "keyboard focus alone");
        assert_eq!(w.background_sprite(), Some("widget/button_highlighted"));

        w.focused = false;
        w.hovered = true;
        assert!(w.is_hovered_or_focused(), "hover alone");
        assert_eq!(
            w.background_sprite(),
            Some("widget/button_highlighted"),
            "a hovered-but-unfocused button highlights; dropping `hovered` from \
             the join would silently stop that and break no test that only sets \
             `focused`"
        );

        // Both, and then the disabled override: `WidgetSprites`'s 3-argument
        // form puts the *disabled* sprite in `disabledFocused`, so hover and
        // focus together still lose to `active = false`.
        w.focused = true;
        assert_eq!(w.background_sprite(), Some("widget/button_highlighted"));
        w.active = false;
        assert_eq!(w.background_sprite(), Some("widget/button_disabled"));

        // Focus and hover are genuinely separate facts now: `takes_focus` reads
        // only `focused`, so hovering a widget must not make Tab skip it.
        let mut h = Widget::button(0.0, 0.0, 200.0, 20.0, "Multiplayer");
        h.hovered = true;
        assert!(
            h.takes_focus(),
            "hover is not focus — `nextFocusPath` keys on `isFocused()` alone"
        );
        h.focused = true;
        assert!(!h.takes_focus());
    }

    #[test]
    fn label_and_icon_geometry_match_vanillas_own_arithmetic() {
        // A 200x20 button at (140, 128) — the title screen's Singleplayer rect
        // on a 480x320 canvas, which is the geometry
        // `tests/menu_button_pixels.rs` measures.
        let w = Widget::button(140.0, 128.0, 200.0, 20.0, "Singleplayer");
        assert_eq!(w.rect(), (140.0, 128.0, 200.0, 20.0));
        assert_eq!(w.content_span(), (142.0, 338.0));
        // (128 + 128 + 20 - 9) / 2 = 133.5 -> floor 133, + 1 = 134.
        assert_eq!(w.label_top(9.0), 134.0);
        // A 20x20 icon button with a 15x15 sprite: (20 - 15) / 2 = 2.5 -> 2.
        let icon = Widget::button(100.0, 200.0, 20.0, 20.0, "Language...");
        assert_eq!(icon.icon_rect(15.0), (102.0, 202.0));
    }

    #[test]
    fn the_layout_seam_reads_size_and_writes_position() {
        // #394's containers do exactly this and nothing more.
        let mut w = Widget::button(0.0, 0.0, BIG_WIDTH, DEFAULT_HEIGHT, "Options...");
        assert_eq!((w.width(), w.height()), (200.0, 20.0));
        w.set_position(140.0, 128.0);
        assert_eq!(w.rectangle(), (140.0, 128.0, 200.0, 20.0));
        // And the trait agrees with the inherent accessor, so a container
        // arranging through the trait cannot place a widget somewhere the draw
        // does not read.
        assert_eq!(w.rectangle(), w.rect());
    }

    #[test]
    fn buttons_carry_vanillas_own_metrics() {
        // `Button.java:12-16`, so a screen never restates one.
        assert_eq!(
            [SMALL_WIDTH, DEFAULT_WIDTH, BIG_WIDTH, DEFAULT_HEIGHT, DEFAULT_SPACING],
            [120.0, 150.0, 200.0, 20.0, 8.0]
        );
        assert_eq!(TEXT_MARGIN, 2.0);
    }
}
