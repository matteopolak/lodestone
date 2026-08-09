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

/// `AbstractSliderButton`'s track (`AbstractSliderButton.java:20-21,36-38`),
/// expressed through the **3**-argument collapse with `enabled` and `disabled`
/// deliberately equal.
///
/// This is a deliberate exception to the module docs' "construct a slider with
/// [`Widget::new`], not [`Widget::button`]". That rule is about not inventing
/// `widget/slider_disabled`, which does not exist in the pack (the
/// `gui/sprites/widget/` listing has `slider`, `slider_highlighted`,
/// `slider_handle` and `slider_handle_highlighted`, and no `_disabled`
/// anywhere) — it is not about the track, which vanilla very much does draw.
///
/// ## Why the 3-argument form and not the 2-argument one
///
/// The 2-argument form reads like the obvious way to say "no disabled art", and
/// it is **wrong here**. `WidgetSprites::get` treats its two arguments
/// independently, while vanilla's predicate is a conjunction:
///
/// ```text
/// getSprite() = isActive() && isFocused() && !canChangeValue ? HIGHLIGHTED : SLIDER
/// ```
///
/// So `!isActive()` must give the plain track *whatever* focus did — and
/// [`WidgetSprites::focusable`] puts the **focused** sprite in
/// `disabledFocused`, which would light an inactive slider up under the cursor.
/// [`WidgetSprites::with_disabled`] puts the **disabled** sprite there, which is
/// exactly the conjunction. The two collapses are not interchangeable and the
/// difference is only observable on a *disabled, focused* widget.
///
/// It is observable here: the settings tree's cursor deliberately stops on
/// inactive rows (`super::options`' module docs, departure 4), because otherwise
/// 117 of its 135 controls would be unreachable and unscrollable. So this is not
/// a theoretical distinction — the first version of this constant used
/// `focusable` and `a_slider_has_a_track_but_no_disabled_track` caught it
/// immediately, reporting `widget/slider_highlighted` for a greyed-out slider.
///
/// `EditBox` genuinely is the 2-argument form (`EditBox.java:29-31`) and is not
/// affected, because `nextFocusPath` refuses focus to an inactive widget, so its
/// disabled-and-focused state does not arise in vanilla.
pub const SLIDER_SPRITES: WidgetSprites = WidgetSprites::with_disabled(
    "widget/slider",
    "widget/slider",
    "widget/slider_highlighted",
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

    /// An `AbstractSliderButton`'s track: [`Self::new`] plus
    /// [`SLIDER_SPRITES`]. Used by every numeric option on the settings tree
    /// (see [`super::options`]).
    ///
    /// The **handle** is still not this type's business — vanilla draws it at
    /// `x + value * (width - 8)` from the widget's own `value`
    /// (`AbstractSliderButton.java:66-80`), and a `Widget` holds no value, so
    /// the caller passes one to the draw. This used to say nothing drew a
    /// handle at all, because every slider the settings tree rendered was
    /// inactive; that stopped being true once issue #203 gave
    /// `mouseWheelSensitivity` a real live value, and player report #<TBD>
    /// (2026-08-04) is what caught the gap it left — see
    /// [`super::render::MenuRow::slider_value`] and
    /// [`Self::slider_handle_sprite`] for the handle this type still does not
    /// draw itself.
    #[must_use]
    pub fn slider(x: f32, y: f32, width: f32, height: f32, message: impl Into<String>) -> Self {
        Self {
            sprites: Some(SLIDER_SPRITES),
            ..Self::new(x, y, width, height, message)
        }
    }

    /// `AbstractSliderButton.getSprite()` (`AbstractSliderButton.java:36-38`):
    /// `isActive() && isFocused() && !canChangeValue ? HIGHLIGHTED : SLIDER`.
    ///
    /// **Both** arguments differ from [`Self::background_sprite`]'s, in the same
    /// two ways `EditBox`'s do (see [`super::edit_box`]): `isActive()` rather
    /// than the raw `active` field, and `isFocused()` **alone** — so hovering a
    /// slider does not highlight it, where hovering a button does. `canChangeValue`
    /// is vanilla's "the keyboard has taken the slider over" latch, which nothing
    /// here sets, so it is `false` and drops out.
    ///
    /// Written when every slider we drew was inactive, so `get(false, _)` was
    /// `widget/slider` regardless of `focused` and the highlighted branch was
    /// unobservable — noted at the time as "exactly the kind of claim that
    /// goes stale the moment one goes live", which is what issue #203's
    /// `mouseWheelSensitivity` then did: it is `is_active() == true`, so its
    /// row keyboard-focused now genuinely shows `widget/slider_highlighted`
    /// rather than the plain track.
    #[must_use]
    pub fn slider_background_sprite(&self) -> Option<&'static str> {
        self.sprites.map(|s| s.get(self.is_active(), self.focused))
    }

    /// `AbstractSliderButton.getHandleSprite()`
    /// (`AbstractSliderButton.java:41-43`):
    /// `!isActive() || (!isHovered && !canChangeValue) ? SLIDER_HANDLE :
    /// SLIDER_HANDLE_HIGHLIGHTED`.
    ///
    /// # `canChangeValue` is **not** always false, and reading it that way was
    /// the bug
    ///
    /// This used to collapse the condition to `isActive() && self.hovered`,
    /// borrowing [`Self::slider_background_sprite`]'s claim that
    /// `canChangeValue` is a latch nothing here sets. Read
    /// `AbstractSliderButton.setFocused` instead of the summary:
    ///
    /// ```java
    /// public void setFocused(final boolean focused) {
    ///    super.setFocused(focused);
    ///    if (!focused) { this.canChangeValue = false; }
    ///    else {
    ///       InputType lastInputType = Minecraft.getInstance().getLastInputType();
    ///       if (lastInputType == InputType.MOUSE || lastInputType == InputType.KEYBOARD_TAB) {
    ///          this.canChangeValue = true;
    ///       }
    ///    }
    /// }
    /// ```
    ///
    /// A slider focused by mouse or by Tab — i.e. every way a player focuses one
    /// — has `canChangeValue == true`. So the honest collapse is
    /// `isActive() && (hovered || focused)`, and the consequence the owner
    /// reported is real: this screen never populates
    /// [`super::render::MenuFrame::hovered`] (only `ServerEdit` and
    /// `WorldSelect` do), so with `hovered` alone the knob could never highlight
    /// at all, however the row was reached.
    ///
    /// # The track's own predicate is left alone, and it is now the odd one
    ///
    /// `getSprite()` is `isActive() && isFocused() && !canChangeValue`, so with
    /// `canChangeValue` true for a focused slider the *track* highlights in
    /// vanilla essentially never. [`Self::slider_background_sprite`] still
    /// highlights on focus, which is a knowing divergence: a settings row that
    /// gave no visual response to the cursor at all would be worse, and the
    /// owner's report was about the knob. Recorded here rather than silently
    /// harmonised.
    #[must_use]
    pub fn slider_handle_sprite(&self) -> &'static str {
        if self.is_active() && (self.hovered || self.focused) {
            "widget/slider_handle_highlighted"
        } else {
            "widget/slider_handle"
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

/// `SelectableEntry.mouseOverRightHalf` (`SelectableEntry.java:12-14`): the
/// cursor is in the right half of a `size`×`size` icon whose top-left is the
/// origin of `(rel_x, rel_y)`.
///
/// The three predicates below are vanilla's own, and they live here — beside
/// [`Widget`], the components layer — rather than in either caller, because the
/// **draw** (which sprite to highlight) and the **click** (what activating that
/// quadrant does) have to agree exactly. Two copies of `rel_x >= size / 2` is
/// how a highlight ends up one quadrant away from what a click does, and no
/// pixel gate on the highlight alone can see it.
///
/// Note the halves are open at the top: `rel_x < size`, so a cursor one pixel
/// past the icon is over nothing. Vanilla's are `int`s; ours take `f32` because
/// the logical canvas is fractional (`render::logical_canvas` divides by an
/// integer scale), and `size / 2` is the same 16 for the 32 px icon either way.
#[must_use]
pub fn over_right_half(rel_x: f32, rel_y: f32, size: f32) -> bool {
    rel_x >= size * 0.5 && rel_x < size && rel_y >= 0.0 && rel_y < size
}

/// `SelectableEntry.mouseOverTopLeftQuarter` (`:24-26`) — the move-up quadrant.
#[must_use]
pub fn over_top_left_quarter(rel_x: f32, rel_y: f32, size: f32) -> bool {
    rel_x >= 0.0 && rel_x < size * 0.5 && rel_y >= 0.0 && rel_y < size * 0.5
}

/// `SelectableEntry.mouseOverBottomLeftQuarter` (`:28-30`) — the move-down
/// quadrant.
#[must_use]
pub fn over_bottom_left_quarter(rel_x: f32, rel_y: f32, size: f32) -> bool {
    rel_x >= 0.0 && rel_x < size * 0.5 && rel_y >= size * 0.5 && rel_y < size
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

// -- the scrollable list ----------------------------------------------------

/// `AbstractScrollArea.SCROLLBAR_WIDTH` (`AbstractScrollArea.java:13`), which is
/// also the `scrollbarWidth` every `defaultSettings` record carries (`:146`).
pub const SCROLLBAR_WIDTH: f32 = 6.0;

/// `AbstractScrollArea.SCROLLBAR_MIN_HEIGHT` (`AbstractScrollArea.java:14`), the
/// floor [`ScrollList::scroller_height`] clamps the thumb to.
pub const SCROLLBAR_MIN_HEIGHT: f32 = 32.0;

/// The gap `scrollerHeight()` leaves at the bottom of its own clamp:
/// `Mth.clamp(…, 32, this.height - 8)` (`AbstractScrollArea.java:97`).
pub const SCROLLBAR_HEIGHT_INSET: f32 = 8.0;

/// `AbstractSelectionList.Entry.CONTENT_PADDING` (`AbstractSelectionList.java:435`)
/// — also the `+ 2` in `getFirstEntryY()` (`:104-106`) and half the `+ 4` in
/// `contentHeight()` (`:198-206`). One constant because vanilla derives all three
/// from the same 2 px inset, and splitting them is how they drift.
pub const LIST_CONTENT_PADDING: f32 = 2.0;

/// The thumb sprite: `AbstractScrollArea.SCROLLER_SPRITE`
/// (`AbstractScrollArea.java:15`).
pub const SCROLLER_SPRITE: &str = "widget/scroller";

/// The track sprite: `AbstractScrollArea.SCROLLER_BACKGROUND_SPRITE`
/// (`AbstractScrollArea.java:16`).
pub const SCROLLER_BACKGROUND_SPRITE: &str = "widget/scroller_background";

/// Vanilla's `AbstractScrollArea` + `AbstractSelectionList` scroll model: a
/// **pixel** scroll offset, a scrollbar, and a `hovered`/`selected` pair that are
/// two separate pieces of state.
///
/// ## What it is
///
/// The shared substrate for every list-shaped menu screen. It owns four things
/// and deliberately nothing else:
///
/// | field | vanilla |
/// |---|---|
/// | [`Self::scroll`] | `AbstractScrollArea.scrollAmount` (`:18`) |
/// | [`Self::selected`] | `AbstractSelectionList.selected` (`:40`) |
/// | [`Self::hovered`] | `AbstractSelectionList.hovered` (`:41`) |
/// | [`Self::dragging`] | `AbstractScrollArea.scrolling` (`:19`) |
///
/// It holds **no entries**. A screen keeps its own rows in whatever shape suits
/// it and tells this type only how many there are, so adopting the primitive
/// never means restructuring a screen's model.
///
/// ## The offset is pixels, and that is the whole point
///
/// `scrollAmount` is a `double` in vanilla (`AbstractScrollArea.java:18`) and
/// every consumer treats it as pixels: `repositionEntries` subtracts it straight
/// from a y (`AbstractSelectionList.java:143-150`), and `mouseScrolled` moves it
/// by `scrollY * scrollRate()` where `scrollRate` is `defaultEntryHeight / 2`
/// (`:44` via `AbstractScrollArea.defaultSettings`, `:145-147`).
///
/// **Both of this shell's lists previously stored a row *index*** — `MenuNav`'s
/// `server_scroll: usize` and `accounts::State::scroll: usize` — so one wheel
/// notch jumped a whole 36 px entry. That is not a smoothing problem to be eased
/// over; it is the wrong representation, and this type is the fix. A row index
/// **cannot** express vanilla's half-entry notch, which is the assertion
/// `one_notch_is_half_an_entry_in_pixels` makes.
///
/// ## There is no scroll animation in 26.2
///
/// Checked rather than assumed, because "smooth" invites one:
/// `setScrollAmount` is an immediate `Mth.clamp` with no target, no velocity and
/// no per-frame approach (`AbstractScrollArea.java:67-69`), and
/// `smoothScroll`/`scrollAnimation`/`targetScroll` appear **nowhere** in
/// `client/gui`. Smoothness in vanilla is entirely a consequence of the offset
/// being pixel-granular. **Do not add easing** — it would be invention, and it
/// would desynchronise the draw from the hit-test, which read the same
/// [`Self::row_top`].
///
/// ## Hover is not selection
///
/// These are separate fields in vanilla and nothing ever copies one into the
/// other. `hovered` is recomputed from the mouse position at the top of every
/// extract (`AbstractSelectionList.java:210`) and is only ever *read* as a
/// boolean argument to the entry's own draw (`:360`). `selected` moves on a
/// click, on a keyboard arrow, or through `setFocused` (`:299-311`) — never on a
/// hover.
///
/// [`Self::set_hovered`] therefore cannot touch `selected`, by construction:
/// there is no code path from one to the other. That is what fixes "hovering an
/// account shouldn't focus it" at the level of the representation rather than by
/// deleting one assignment from one screen.
///
/// ## How to change it
///
/// - **Keep every formula a citation.** Each method below names the vanilla line
///   it ports. A convenience that has no vanilla counterpart belongs in the
///   screen, not here.
/// - **The integer truncations are load-bearing.** `scrollerHeight` casts to
///   `int` before clamping (`AbstractScrollArea.java:97`) and `scrollBarY` does
///   `(int)scrollAmount * (height - scrollerHeight()) / maxScrollAmount()` in
///   **integer** arithmetic (`:104-108`). The `floor`s here are that arithmetic,
///   not defensive rounding — deleting them moves the thumb by up to a pixel and
///   no test of ours would say why.
/// - **Row heights may be uniform or per-entry, and uniform is the degenerate
///   case of the same arithmetic.** `AbstractSelectionList` allows a per-entry
///   height (`addEntry(entry, height)`, `:122-129`) and `super::options`'
///   settings list genuinely needs it, so [`Self::new_variable`] takes the
///   heights and every offset goes through [`Self::row_offset`] /
///   [`Self::row_height`]. `row_h` remains `defaultEntryHeight` in both modes,
///   because that is what `scrollRate` is defined against (`:44`) — it is *not*
///   "the height of a row" once the heights are explicit, and conflating the two
///   is the one way to get this wrong.
/// - **[`Self::visible_range`] is closed-form only in the uniform case.** The
///   algebra below inverts a multiply, which a prefix sum does not have; the
///   variable case walks, exactly as vanilla's `repositionEntries` does. Both
///   are held to agreeing with [`Self::row_visible`] entry-by-entry by
///   `visible_range_agrees_with_row_visible_at_every_offset`, which now sweeps
///   both modes.
///
/// ## Dependencies
///
/// None — pure arithmetic. The scrollbar's *pixels* are drawn by
/// [`super::render`], which reads [`Self::scrollbar_rects`].
#[derive(Debug, Clone, PartialEq)]
pub struct ScrollList {
    /// Scroll offset in **logical pixels** — `AbstractScrollArea.scrollAmount`
    /// (`:18`). Always inside `0..=max_scroll()`; every writer goes through
    /// [`Self::set_scroll`].
    scroll: f32,
    /// `AbstractSelectionList.defaultEntryHeight` (`:37`) — the height of an
    /// entry added without one, **and** the basis of
    /// [`Self::scroll_rate`] (`:44`) in both modes. When [`Self::prefix`] is
    /// `None` it is also every entry's height.
    row_h: f32,
    /// Running sum of per-entry heights when the list is **not** uniform:
    /// `prefix[i]` is the total height of entries `0..i`, so it has `len + 1`
    /// elements and `prefix[len]` is the content's entry height.
    ///
    /// `None` is the uniform case, where `prefix[i]` would be `i * row_h` — kept
    /// as an absence rather than a materialised table so the common list costs
    /// no allocation and the closed-form [`Self::visible_range`] stays
    /// reachable.
    prefix: Option<Box<[f32]>>,
    /// The band's top y — `getY()`.
    top: f32,
    /// The band's height — `getHeight()`.
    height: f32,
    /// Number of entries — `getItemCount()` (`:160-162`).
    len: usize,
    /// `AbstractSelectionList.selected` (`:40`). What Enter/Select acts on.
    selected: Option<usize>,
    /// `AbstractSelectionList.hovered` (`:41`). Draw-only, mouse-derived, and
    /// **never** written by anything that writes [`Self::selected`].
    hovered: Option<usize>,
    /// `AbstractScrollArea.scrolling` (`:19`) — a thumb drag is in progress.
    dragging: bool,
}

impl ScrollList {
    /// A list of `len` entries of `row_h` each, in a band at `top` of `height`.
    ///
    /// Nothing is selected and nothing is hovered, matching a freshly
    /// constructed `AbstractSelectionList` (both fields are `@Nullable` and start
    /// null).
    #[must_use]
    pub fn new(row_h: f32, top: f32, height: f32, len: usize) -> Self {
        Self {
            scroll: 0.0,
            row_h,
            prefix: None,
            top,
            height,
            len,
            selected: None,
            hovered: None,
            dragging: false,
        }
    }

    /// A list whose entries have **individual** heights — vanilla's
    /// `addEntry(entry, height)` (`AbstractSelectionList.java:122-129`), where
    /// `repositionEntries` advances its running `y` by each child's own height
    /// (`:143-152`) rather than by a constant.
    ///
    /// `default_row_h` is `defaultEntryHeight`, which is **not** derivable from
    /// `heights`: it is what [`Self::scroll_rate`] is defined against (`:44`),
    /// so a settings list of mixed 20 px and 25 px rows still scrolls at its
    /// declared default rate. Passing `heights.first()` instead would make the
    /// wheel rate depend on which entry happens to be first.
    ///
    /// `len` comes from `heights.len()`, so the two can never disagree — the
    /// failure mode a separate count would allow is an index into `prefix` that
    /// is in range for `len` and out of range for the table.
    #[must_use]
    pub fn new_variable(default_row_h: f32, top: f32, height: f32, heights: &[f32]) -> Self {
        Self {
            scroll: 0.0,
            row_h: default_row_h,
            prefix: Some(Self::build_prefix(heights)),
            top,
            height,
            len: heights.len(),
            selected: None,
            hovered: None,
            dragging: false,
        }
    }

    /// `heights` accumulated into a `len + 1` running sum, starting at 0.
    ///
    /// A negative or non-finite height is clamped to 0 rather than rejected: a
    /// caller derives these from a layout, and one bad value must not make every
    /// *subsequent* offset nonsense (a `NaN` in a prefix sum poisons the whole
    /// tail, and `row_top` would then place every later row off-screen).
    fn build_prefix(heights: &[f32]) -> Box<[f32]> {
        let mut out = Vec::with_capacity(heights.len() + 1);
        let mut running = 0.0_f32;
        out.push(0.0);
        for &h in heights {
            running += if h.is_finite() { h.max(0.0) } else { 0.0 };
            out.push(running);
        }
        out.into_boxed_slice()
    }

    /// Whether entry heights are per-entry rather than all [`Self::row_h`].
    #[must_use]
    pub fn is_variable(&self) -> bool {
        self.prefix.is_some()
    }

    /// The distance from the first entry's top to entry `index`'s top —
    /// `repositionEntries`' running `y` minus its start (`:143-152`).
    ///
    /// `index == len` is legal and yields the content's full entry height, which
    /// is what makes [`Self::content_height`] the same expression in both modes.
    #[must_use]
    pub fn row_offset(&self, index: usize) -> f32 {
        match &self.prefix {
            Some(p) => p.get(index).copied().unwrap_or_else(|| {
                // Past the end: the last running total. Reachable only through
                // a stale index, and answering the content height is the
                // clamped answer rather than a panic mid-draw.
                p.last().copied().unwrap_or(0.0)
            }),
            None => index as f32 * self.row_h,
        }
    }

    /// Entry `index`'s own height — `child.getHeight()`.
    ///
    /// Falls back to [`Self::row_h`] for an out-of-range index so that callers
    /// hit-testing a stale index get a sane box rather than a zero-height one
    /// that silently matches nothing.
    #[must_use]
    pub fn row_height(&self, index: usize) -> f32 {
        match &self.prefix {
            Some(p) if index + 1 < p.len() => p[index + 1] - p[index],
            Some(_) => self.row_h,
            None => self.row_h,
        }
    }

    // -- geometry ---------------------------------------------------------

    /// `getBottom()`.
    #[must_use]
    pub fn bottom(&self) -> f32 {
        self.top + self.height
    }

    /// `getY()`.
    #[must_use]
    pub fn top(&self) -> f32 {
        self.top
    }

    /// `getHeight()`.
    #[must_use]
    pub fn height(&self) -> f32 {
        self.height
    }

    /// `defaultEntryHeight`.
    #[must_use]
    pub fn row_h(&self) -> f32 {
        self.row_h
    }

    /// `getItemCount()` (`AbstractSelectionList.java:160-162`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the list has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Re-seat the band and the entry count, then re-clamp — vanilla's
    /// `updateSizeAndPosition` (`AbstractSelectionList.java:186-195`), which
    /// ends in `refreshScrollAmount()`.
    ///
    /// **Call this every frame**, before reading any geometry. The band depends
    /// on the canvas, and a list that keeps a stale `height` reports a stale
    /// `max_scroll`, which is how a shrunk window ends up scrolled past its own
    /// content. The re-clamp is why this cannot be a plain field write.
    pub fn resize(&mut self, top: f32, height: f32, len: usize) {
        self.top = top;
        self.height = height;
        self.len = len;
        // A uniform resize on a variable list would leave a `prefix` of the old
        // length, so every offset past the change would be read from stale data
        // — worse than a wrong count, because `row_offset` would still answer
        // plausibly. Dropping to uniform is the honest degradation, and
        // `resize_variable` is the call that keeps the heights.
        self.prefix = None;
        if let Some(s) = self.selected
            && s >= len
        {
            self.selected = None;
        }
        if let Some(h) = self.hovered
            && h >= len
        {
            self.hovered = None;
        }
        self.refresh_scroll();
    }

    /// [`Self::resize`] for a list with per-entry heights: re-seat the band and
    /// **replace** the height table, then re-clamp.
    ///
    /// `len` is taken from `heights`, for the same reason
    /// [`Self::new_variable`] does it.
    pub fn resize_variable(&mut self, top: f32, height: f32, heights: &[f32]) {
        self.top = top;
        self.height = height;
        self.len = heights.len();
        self.prefix = Some(Self::build_prefix(heights));
        if let Some(s) = self.selected
            && s >= self.len
        {
            self.selected = None;
        }
        if let Some(h) = self.hovered
            && h >= self.len
        {
            self.hovered = None;
        }
        self.refresh_scroll();
    }

    /// `getFirstEntryY() = getY() + 2` (`AbstractSelectionList.java:104-106`).
    #[must_use]
    pub fn first_entry_y(&self) -> f32 {
        self.top + LIST_CONTENT_PADDING
    }

    /// `contentHeight()`: the entries' total height plus 4
    /// (`AbstractSelectionList.java:198-206`) — the 2 px above the first entry
    /// and the 2 px below the last.
    #[must_use]
    pub fn content_height(&self) -> f32 {
        self.row_offset(self.len) + 2.0 * LIST_CONTENT_PADDING
    }

    /// `maxScrollAmount() = max(0, contentHeight() - height)`
    /// (`AbstractScrollArea.java:84-86`).
    #[must_use]
    pub fn max_scroll(&self) -> f32 {
        (self.content_height() - self.height).max(0.0)
    }

    /// `scrollable() = maxScrollAmount() > 0` (`AbstractScrollArea.java:88-90`).
    /// Also the gate on the scrollbar being drawn at all (`:126`).
    #[must_use]
    pub fn scrollable(&self) -> bool {
        self.max_scroll() > 0.0
    }

    /// The top of entry `index`, in the same space the band is measured in:
    /// `getFirstEntryY() - scrollAmount() + index * height`, which is
    /// `repositionEntries`' running `y` (`AbstractSelectionList.java:143-152`).
    ///
    /// Vanilla truncates the offset once, at `(int)this.scrollAmount()`
    /// (`:144`), rather than per entry — so the whole column moves as a unit and
    /// entries stay exactly `row_h` apart. Reproduced with a single `floor`
    /// outside the multiply for that reason.
    #[must_use]
    pub fn row_top(&self, index: usize) -> f32 {
        self.first_entry_y() - self.scroll.floor() + self.row_offset(index)
    }

    /// Whether entry `index` is inside the band —
    /// `child.getY() + child.getHeight() >= getY() && child.getY() <= getBottom()`
    /// (`AbstractSelectionList.java:346-352`).
    ///
    /// **This is a *partial*-overlap test, and now that is the whole point.** It
    /// was previously stood in for by "skip any row that is not wholly inside",
    /// because the menu pipeline had no scissor and a half-row would have painted
    /// over the header or footer. [`super::render`]'s `Quads` now clips, so a row
    /// that straddles the edge is drawn *and cut*, exactly as vanilla's
    /// `enableScissor` (`:242-249`) does it.
    #[must_use]
    pub fn row_visible(&self, index: usize) -> bool {
        if index >= self.len {
            return false;
        }
        let top = self.row_top(index);
        top + self.row_height(index) >= self.top && top <= self.bottom()
    }

    /// The half-open range of entries that overlap the band at all.
    ///
    /// Derived by **inverting** [`Self::row_visible`]'s two inequalities rather
    /// than by scanning, so a long list costs no walk. Writing
    /// `row_top(i) = top + 2 - s + i*row_h` (with `s = floor(scroll)`) into
    /// `row_top(i) + row_h >= top` and `row_top(i) <= bottom()` and solving for
    /// `i` gives, with `d = s - 2`:
    ///
    /// ```text
    /// i >= d/row_h - 1        ->  first = ceil(d/row_h) - 1
    /// i <= (d + height)/row_h ->  last  = floor((d + height)/row_h)   (inclusive)
    /// ```
    ///
    /// **The `ceil - 1` is not interchangeable with a `floor`.** At `s = 38` and
    /// a 36 px row, row 0's bottom edge lands exactly on the band's top, so it is
    /// visible by the `>=`; `floor(36/36)` says the range starts at 1 and drops a
    /// row that is being drawn. A first version of this used `floor` plus a
    /// compensating `+ 1` on the far end and over-reported the last row by one at
    /// rest — caught by `visible_range_agrees_with_row_visible_at_every_offset`,
    /// which sweeps the whole span and is the reason this is stated as algebra
    /// rather than adjusted until the obvious cases passed.
    #[must_use]
    pub fn visible_range(&self) -> core::ops::Range<usize> {
        if self.len == 0 {
            return 0..0;
        }
        // The variable case has no multiply to invert, so it walks — vanilla's
        // own `repositionEntries` does too. Cheap in practice because the walk
        // stops at the first entry past the band's bottom, and a settings page
        // is tens of rows, not thousands.
        if self.is_variable() {
            let mut first = None;
            let mut end = 0;
            for i in 0..self.len {
                if self.row_visible(i) {
                    if first.is_none() {
                        first = Some(i);
                    }
                    end = i + 1;
                } else if first.is_some() {
                    // Heights are non-negative, so visibility is one contiguous
                    // run; the first miss after a hit ends it.
                    break;
                }
            }
            return match first {
                Some(f) => f..end,
                None => 0..0,
            };
        }
        if self.row_h <= 0.0 {
            return 0..0;
        }
        let d = self.scroll.floor() - LIST_CONTENT_PADDING;
        let first = ((d / self.row_h).ceil() - 1.0).max(0.0) as usize;
        let last_inclusive = ((d + self.height) / self.row_h).floor();
        if last_inclusive < 0.0 {
            return 0..0;
        }
        let end = (last_inclusive as usize).saturating_add(1);
        first.min(self.len)..end.min(self.len)
    }

    // -- the offset -------------------------------------------------------

    /// `scrollAmount()` (`AbstractScrollArea.java:63-65`) — **pixels**.
    #[must_use]
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// `setScrollAmount`: `Mth.clamp(scrollAmount, 0, maxScrollAmount())`
    /// (`AbstractScrollArea.java:67-69`).
    ///
    /// A NaN would survive `clamp` and poison every y downstream, so it is
    /// mapped to 0 — vanilla cannot receive one (its inputs are all `int`-derived
    /// doubles) and we can, via a `PixelDelta` wheel event.
    pub fn set_scroll(&mut self, scroll: f32) {
        self.scroll = if scroll.is_nan() {
            0.0
        } else {
            scroll.clamp(0.0, self.max_scroll())
        };
    }

    /// `refreshScrollAmount()` (`AbstractScrollArea.java:80-82`) — re-apply the
    /// clamp after the band or the content changed.
    pub fn refresh_scroll(&mut self) {
        let s = self.scroll;
        self.set_scroll(s);
    }

    /// `scrollRate()`, which for a selection list is `defaultEntryHeight / 2`
    /// (`AbstractSelectionList.java:44` → `AbstractScrollArea.defaultSettings`,
    /// `:145-147`).
    ///
    /// **Integer division, and `scrollRate` is an `int` field of the record**
    /// (`:155`) — so a 36 px entry gives exactly 18, and a 25 px entry gives 12,
    /// not 12.5. The `floor` is that, and it is why this returns a value a row
    /// index could never represent.
    #[must_use]
    pub fn scroll_rate(&self) -> f32 {
        (self.row_h / 2.0).floor()
    }

    /// `mouseScrolled`: `setScrollAmount(scrollAmount() - scrollY * scrollRate())`
    /// (`AbstractScrollArea.java:28-36`).
    ///
    /// `notches` is winit's `scrollY`, so **positive scrolls up** (toward entry
    /// 0), matching vanilla's sign.
    pub fn mouse_scrolled(&mut self, notches: f32) {
        self.set_scroll(self.scroll - notches * self.scroll_rate());
    }

    /// `scrollToEntry` (`AbstractSelectionList.java:251-261`): the minimum move
    /// that brings entry `index` fully inside the band.
    ///
    /// Both deltas are computed against the *current* offset and applied in
    /// order, exactly as vanilla does — the second `if` sees the first one's
    /// effect, which is what makes an entry taller than the band settle at its
    /// top rather than oscillating.
    pub fn scroll_to_entry(&mut self, index: usize) {
        if index >= self.len {
            return;
        }
        let top_delta = self.row_top(index) - self.top - LIST_CONTENT_PADDING;
        if top_delta < 0.0 {
            self.set_scroll(self.scroll + top_delta);
        }
        let bottom_delta =
            self.bottom() - self.row_top(index) - self.row_height(index) - LIST_CONTENT_PADDING;
        if bottom_delta < 0.0 {
            self.set_scroll(self.scroll - bottom_delta);
        }
    }

    /// `centerScrollOn` (`AbstractSelectionList.java:263-276`).
    pub fn center_on(&mut self, index: usize) {
        if index >= self.len {
            return;
        }
        let y = self.row_offset(index) + self.row_height(index) / 2.0;
        self.set_scroll(y - self.height / 2.0);
    }

    // -- selection and hover, which are different things -------------------

    /// `getSelected()` (`AbstractSelectionList.java:49-51`).
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// `setSelected` (`AbstractSelectionList.java:53-62`), including its
    /// scroll-into-view.
    ///
    /// Vanilla scrolls when the entry is clipped at either edge **or** when the
    /// last input was the keyboard (`:58`). The clipped tests are ported;
    /// `getLastInputType().isKeyboard()` has no equivalent here, so callers that
    /// mean "the keyboard moved this" pass `keyboard = true`. A click passes
    /// `false`, which is what stops a click on a partially-visible row from
    /// yanking the list — vanilla's own behaviour, and easy to lose.
    pub fn set_selected(&mut self, selected: Option<usize>, keyboard: bool) {
        self.selected = selected.filter(|&i| i < self.len);
        if let Some(i) = self.selected {
            let top_clipped = self.row_top(i) + LIST_CONTENT_PADDING < self.top;
            let bottom_clipped =
                self.row_top(i) + self.row_height(i) - LIST_CONTENT_PADDING > self.bottom();
            if keyboard || top_clipped || bottom_clipped {
                self.scroll_to_entry(i);
            }
        }
    }

    /// `getHovered()` (`AbstractSelectionList.java:416-418`).
    #[must_use]
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// `this.hovered = isMouseOver(…) ? getEntryAtPosition(…) : null`
    /// (`AbstractSelectionList.java:210`).
    ///
    /// **This method must never touch [`Self::selected`], and there is no code
    /// path here by which it could.** That is not a stylistic note — it is the
    /// fix for "hovering an account shouldn't focus it". The two screens that had
    /// this bug both wrote *both* fields from one hover handler
    /// (`accounts::AccountsNav::hover` set `highlighted` **and** `focus`), and
    /// nothing about either line looked wrong in isolation.
    pub fn set_hovered(&mut self, hovered: Option<usize>) {
        self.hovered = hovered.filter(|&i| i < self.len);
    }

    /// `getEntryAtPosition` (`AbstractSelectionList.java:168-176`) restricted to
    /// entries actually inside the band, then folded through
    /// [`Self::set_hovered`].
    ///
    /// `row_left`/`row_w` come from the caller because row width is a screen's
    /// choice (`getRowWidth()`, `:389-391`, is overridable).
    pub fn hover_at(&mut self, x: f32, y: f32, row_left: f32, row_w: f32) {
        let inside_x = x >= row_left && x < row_left + row_w;
        let inside_band = y >= self.top && y < self.bottom();
        if !inside_x || !inside_band {
            self.set_hovered(None);
            return;
        }
        let hit = self
            .visible_range()
            .find(|&i| y >= self.row_top(i) && y < self.row_top(i) + self.row_height(i));
        self.set_hovered(hit);
    }

    /// The entry at `y`, ignoring hover state — what a click hit-tests against.
    ///
    /// Restricted to [`Self::visible_range`] *and* to the band, so a click can
    /// never land on an entry that is not on screen. Before this primitive that
    /// guarantee was absent: `render::row_rect` still answered for a row the draw
    /// had skipped, so clicking empty space below a short list selected an
    /// invisible entry (recorded against issue #402).
    #[must_use]
    pub fn entry_at(&self, x: f32, y: f32, row_left: f32, row_w: f32) -> Option<usize> {
        if x < row_left || x >= row_left + row_w || y < self.top || y >= self.bottom() {
            return None;
        }
        self.visible_range()
            .find(|&i| y >= self.row_top(i) && y < self.row_top(i) + self.row_height(i))
    }

    // -- the scrollbar ----------------------------------------------------

    /// `scrollbarWidth()` (`AbstractScrollArea.java:92-94`).
    #[must_use]
    pub fn scrollbar_width(&self) -> f32 {
        SCROLLBAR_WIDTH
    }

    /// `scrollBarX()` — and note **`AbstractSelectionList` overrides it**:
    /// `getRowRight() + scrollbarWidth() + 2` (`AbstractSelectionList.java:289-291`),
    /// *not* `AbstractScrollArea`'s `getRight() - scrollbarWidth()` (`:100-102`).
    ///
    /// So the bar sits 8 px to the **right** of the row, outside it, rather than
    /// being inset into the list's right edge. Taking `row_right` rather than
    /// reading a width is what keeps this honest: the caller passes the same
    /// `getRowRight()` its rows are drawn at.
    #[must_use]
    pub fn scrollbar_x(&self, row_right: f32) -> f32 {
        row_right + SCROLLBAR_WIDTH + 2.0
    }

    /// `scrollerHeight() = Mth.clamp((int)((float)(height * height) / contentHeight()), 32, height - 8)`
    /// (`AbstractScrollArea.java:96-98`).
    ///
    /// The `floor` is vanilla's `(int)` cast. Note the upper clamp can be
    /// *below* the lower one on a very short band, and `Mth.clamp` resolves that
    /// to the **upper** bound; `min` after `max` reproduces that order.
    #[must_use]
    pub fn scroller_height(&self) -> f32 {
        let content = self.content_height();
        if content <= 0.0 {
            return SCROLLBAR_MIN_HEIGHT;
        }
        let ideal = (self.height * self.height / content).floor();
        ideal
            .max(SCROLLBAR_MIN_HEIGHT)
            .min(self.height - SCROLLBAR_HEIGHT_INSET)
    }

    /// `scrollBarY()` (`AbstractScrollArea.java:104-108`).
    ///
    /// **Integer arithmetic throughout in vanilla**:
    /// `(int)scrollAmount * (height - scrollerHeight()) / maxScrollAmount() + getY()`,
    /// so the numerator truncates *before* the divide. The two `floor`s are that,
    /// in that order.
    #[must_use]
    pub fn scrollbar_y(&self) -> f32 {
        let max = self.max_scroll();
        if max <= 0.0 {
            return self.top;
        }
        let travel = self.height - self.scroller_height();
        let y = (self.scroll.floor() * travel / max).floor() + self.top;
        y.max(self.top)
    }

    /// The track and thumb rects as `(x, y, w, h)`, or `None` when the list does
    /// not scroll — `extractScrollbar` draws nothing unless `scrollable()`
    /// (`AbstractScrollArea.java:126-136`), and this shell has no
    /// `disabledScrollerSprite`, so the `!scrollable()` branch (`:114-124`) has
    /// no counterpart.
    ///
    /// Returned as a pair rather than drawn here because `widget.rs` owns no
    /// pixels; [`super::render`]'s `draw_scrollbar` is the consumer.
    #[must_use]
    pub fn scrollbar_rects(&self, row_right: f32) -> Option<(Rect, Rect)> {
        if !self.scrollable() {
            return None;
        }
        let x = self.scrollbar_x(row_right);
        let w = self.scrollbar_width();
        Some((
            (x, self.top, w, self.height),
            (x, self.scrollbar_y(), w, self.scroller_height()),
        ))
    }

    /// `isOverScrollbar` (`AbstractScrollArea.java:76-78`). Note the asymmetry
    /// vanilla itself has: inclusive in x on **both** edges, half-open in y.
    #[must_use]
    pub fn is_over_scrollbar(&self, x: f32, y: f32, row_right: f32) -> bool {
        let bar_x = self.scrollbar_x(row_right);
        x >= bar_x && x <= bar_x + self.scrollbar_width() && y >= self.top && y < self.bottom()
    }

    /// `updateScrolling` (`AbstractScrollArea.java:71-74`): begin a thumb drag if
    /// the press landed on the bar of a scrollable list. Returns whether it did,
    /// which is vanilla's own "I consumed this click".
    pub fn begin_drag(&mut self, x: f32, y: f32, row_right: f32) -> bool {
        self.dragging = self.scrollable() && self.is_over_scrollbar(x, y, row_right);
        self.dragging
    }

    /// `onRelease` (`AbstractScrollArea.java:58-61`).
    pub fn end_drag(&mut self) {
        self.dragging = false;
    }

    /// Whether a thumb drag is in progress — `AbstractScrollArea.scrolling`.
    #[must_use]
    pub fn dragging(&self) -> bool {
        self.dragging
    }

    /// `mouseDragged` (`AbstractScrollArea.java:38-56`), the thumb-drag branch.
    ///
    /// Three cases, all vanilla's: above the band snaps to 0, below it snaps to
    /// `maxScrollAmount()`, and inside it multiplies the mouse delta by
    /// `max(1, maxScroll / (height - scrollerHeight()))` so dragging the thumb
    /// one pixel moves the content one *page-fraction*. A no-op unless
    /// [`Self::begin_drag`] armed it.
    pub fn drag_to(&mut self, y: f32, dy: f32) {
        if !self.dragging {
            return;
        }
        if y < self.top {
            self.set_scroll(0.0);
        } else if y > self.bottom() {
            let max = self.max_scroll();
            self.set_scroll(max);
        } else {
            let max = self.max_scroll().max(1.0);
            let travel = self.height - self.scroller_height();
            let scale = if travel > 0.0 { (max / travel).max(1.0) } else { 1.0 };
            self.set_scroll(self.scroll + dy * scale);
        }
    }
}

/// A screen's **canvas-independent** declaration of its scrolling list: everything
/// a [`ScrollList`] needs except the one fact only the caller knows, the canvas.
///
/// ## What it is
///
/// The generic hook the scrollbar draw and the mouse wheel both go through, so
/// that "this screen has a scrolling list" is a thing a screen can *say* rather
/// than something the draw and the input layer each hardcode one screen's answer
/// to.
///
/// It exists because of a measured island. `render::draw`'s scrollbar block used to
/// call `server_scroll_list` **by name**, and `app`'s wheel arm was gated on
/// `Screen::ServerList`, so those two pixels-and-input paths knew about exactly one
/// screen. A second screen adopting [`ScrollList`] would then have had correct
/// geometry, green unit tests and **zero pixels** — no bar, no wheel. Converting a
/// screen before this type existed produced an island *by construction*, which is
/// why this landed first and the conversions second.
///
/// ## How it works
///
/// A screen declares one of these; [`Self::model`] turns it into the live
/// [`ScrollList`] once a canvas is known. **Both consumers call `model`**, so the
/// bar the draw paints and the offset the wheel clamps come from one expression
/// rather than two that agree today — the property that stops a thumb drifting away
/// from its rows, and the same reasoning `server_scroll_model`'s doc records.
///
/// The band is declared as `top` plus `footer_h`, not as a finished height, because
/// that is the form the screens here already state their layout in (a
/// `content_top` and a footer constant) and it is what keeps the declaration
/// independent of the canvas. Likewise the row *edge* is carried as a [`RowBand`]
/// and [`Self::row_right`] derives the scrollbar's anchor, rather than the anchor
/// being passed in already computed, so the bar cannot sit somewhere the rows are
/// not.
///
/// ## How to change it
///
/// To give a screen a scrollbar and a wheel, return one of these from
/// `MenuNav::active_list` and store the offset in **pixels**. A screen whose offset
/// is a row *index* cannot use this honestly: it would report a `scroll` that is
/// always a multiple of the row height, which is precisely the snap-to-row
/// behaviour the wheel work existed to remove. Convert the field first.
///
/// `heights` being `Some` selects [`ScrollList::new_variable`]; leave it `None`
/// for a uniform list. `row_h` is `defaultEntryHeight` in **both** cases — it is
/// what `scrollRate` is defined against, never "the height of a row" — see
/// [`ScrollList::new_variable`].
///
/// ## Dependencies
///
/// None beyond [`ScrollList`]; pure data plus one constructor.
#[derive(Debug, Clone, PartialEq)]
pub struct ListSpec {
    /// `AbstractSelectionList.defaultEntryHeight`, and the basis of
    /// [`ScrollList::scroll_rate`] whether or not [`Self::heights`] is set.
    pub row_h: f32,
    /// The band's top y, in logical pixels — a screen's `content_top`.
    pub top: f32,
    /// How much of the canvas below the band the footer occupies. The band's
    /// height is `canvas_height - footer_h - top`.
    pub footer_h: f32,
    /// Number of entries. Ignored when [`Self::heights`] is `Some`, which carries
    /// its own length — see [`ScrollList::new_variable`] on why the two can never
    /// be allowed to disagree.
    pub len: usize,
    /// Per-entry heights for a non-uniform list, or `None` for a uniform one.
    pub heights: Option<Vec<f32>>,
    /// The current offset, in **pixels**.
    pub scroll: f32,
    /// Where the rows sit horizontally — used only to derive [`Self::row_left`]
    /// and [`Self::row_right`]. See [`RowBand`].
    pub band: RowBand,
    /// Where the band's own chrome — the tinted background and the two
    /// separators — spans. See [`ListChrome`].
    pub chrome: ListChrome,
}

/// Where a list's **chrome** spans horizontally: the tinted band background and
/// the two 2 px separators that fence it off from the header and the footer.
///
/// ## What it is, and why it is not [`RowBand`]
///
/// These are two different rectangles and conflating them is the trap this type
/// exists to prevent. [`RowBand`] is `getRowLeft()`/`getRowRight()` — the column
/// an *entry* is laid into, 310 px on a settings page. The chrome is the list
/// **widget's own** `getX()`/`getWidth()`, and `AbstractSelectionList`'s
/// constructor is `super(0, y, width, height, …)`, so for every list whose screen
/// hands it `this.width` the chrome is the whole canvas while the rows are a
/// narrow centred column. Drawing the tint at `row_w` would leave the canvas
/// margins untinted, which is not what vanilla looks like.
///
/// ## How to change it
///
/// [`Self::Canvas`] is the answer for every list whose vanilla constructor takes
/// the screen width, which is all of them here bar one. Reach for
/// [`Self::None`] only when a screen's real vanilla geometry is *several*
/// narrower lists that this crate models as one band — see its own doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListChrome {
    /// `super(0, y, screen.width, …)`: the chrome spans the canvas.
    Canvas,
    /// No chrome at all.
    ///
    /// The Resource Packs screen: vanilla runs **two** 200 px-wide
    /// `TransferableSelectionList`s side by side, each with its own background
    /// and its own pair of separators, and this crate models the pair as one
    /// band so that a single clip rect and a single scrollbar can serve both.
    /// One canvas-wide tint would therefore paint the gutter *between* the two
    /// columns that vanilla leaves clear. Deliberately unported rather than
    /// approximated; the fix is a per-column chrome rect, not a wider one.
    None,
}

/// Where a list's rows sit horizontally on the canvas.
///
/// ## What it is
///
/// The one thing [`ListSpec`] needs a canvas *width* to answer, split into the
/// two shapes the screens here actually have. [`Self::row_left`]/
/// [`Self::row_right`] are what [`ScrollList::scrollbar_x`] hangs the bar off, so
/// this is also "where the scrollbar goes".
///
/// ## Why there are two
///
/// [`Self::Centred`] alone was the whole of `ListSpec` until issue #445 reached
/// Social Interactions, and it is `AbstractSelectionList`'s own model:
/// `getRowLeft()` is `width / 2 - rowWidth / 2`, a fixed-width row centred on the
/// canvas. Multiplayer (340), accounts (305), key binds (340), language (270) and
/// statistics (300) are all that shape, and for them a single `row_w` constant
/// answers both edges.
///
/// Social Interactions is not, and it is the reason this enum exists rather than a
/// fourth `row_w` constant. Its rows are **full-width**: the player name sits at a
/// flat 4 px inset from the canvas's left edge and the Hide/Report buttons are
/// anchored off `width - RIGHT_MARGIN`, so the row grows and shrinks with the
/// window. **No constant `row_w` can express that** — `row_left(row_w) + row_w`
/// tracks the canvas centre at half the rate the right edge moves, so a value
/// tuned at 854 px puts the scrollbar through the Report button at 1280 and off
/// the canvas at 640. That is not a tuning problem, it is the wrong shape, which
/// is why `social.rs` was blocked on this type rather than on effort.
///
/// ## How to change it
///
/// Prefer [`Self::Centred`]; it is what vanilla does and what five of the six
/// lists here are. Reach for [`Self::Inset`] only when a screen's own geometry is
/// genuinely canvas-relative, and note the gutter rule below — an `Inset` right
/// edge with no room for the bar puts the bar off the canvas, silently, because
/// nothing clamps it.
///
/// **The right inset must reserve `SCROLLBAR_WIDTH + 2 + SCROLLBAR_WIDTH` = 14 px**
/// beyond wherever the row's own content ends. `AbstractSelectionList` overrides
/// `scrollBarX()` to `getRowRight() + scrollbarWidth() + 2`, so the bar lives
/// *outside* the row, not inset into it — see [`ScrollList::scrollbar_x`]. A
/// centred list gets that gutter for free from the canvas margin either side; a
/// full-width one has to declare it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RowBand {
    /// `AbstractSelectionList.getRowLeft()`: a fixed-width row centred on the
    /// canvas.
    Centred {
        /// `getRowWidth()`.
        row_w: f32,
    },
    /// A row that spans the canvas, inset by `left` from its left edge and
    /// `right` from its right. The row's width is therefore a function of the
    /// canvas, which is exactly what [`Self::Centred`] cannot express.
    Inset {
        /// Distance from the canvas's left edge to the row's left edge.
        left: f32,
        /// Distance from the row's right edge to the canvas's right edge. Must
        /// leave room for the scrollbar — see this type's "How to change it".
        right: f32,
    },
}

impl RowBand {
    /// `getRowLeft()`.
    #[must_use]
    pub fn row_left(self, width: f32) -> f32 {
        match self {
            // Two *separate* `floor`s, not one on the difference: that is
            // vanilla's own integer arithmetic, and it is the expression
            // `server_row_left`/`accounts_row_left` already used.
            RowBand::Centred { row_w } => (width * 0.5).floor() - (row_w * 0.5).floor(),
            RowBand::Inset { left, .. } => left,
        }
    }

    /// `getRowRight()` — what [`ScrollList::scrollbar_rects`] hangs the bar off.
    #[must_use]
    pub fn row_right(self, width: f32) -> f32 {
        match self {
            RowBand::Centred { row_w } => self.row_left(width) + row_w,
            RowBand::Inset { right, .. } => width - right,
        }
    }

    /// `getRowWidth()` at this canvas width. Constant for [`Self::Centred`] and a
    /// function of the canvas for [`Self::Inset`], which is the whole difference.
    #[must_use]
    pub fn row_w(self, width: f32) -> f32 {
        self.row_right(width) - self.row_left(width)
    }
}

impl ListSpec {
    /// A uniform list: `len` entries of `row_h`, in the band `top..(h - footer_h)`.
    #[must_use]
    pub fn uniform(row_h: f32, top: f32, footer_h: f32, len: usize, row_w: f32) -> Self {
        Self {
            row_h,
            top,
            footer_h,
            len,
            heights: None,
            scroll: 0.0,
            band: RowBand::Centred { row_w },
            chrome: ListChrome::Canvas,
        }
    }

    /// This spec with no band chrome — see [`ListChrome::None`], which is the one
    /// screen that needs it and says why.
    #[must_use]
    pub fn without_chrome(mut self) -> Self {
        self.chrome = ListChrome::None;
        self
    }

    /// The chrome's rect: `(x, y, w, h)` in logical pixels at this canvas, or
    /// `None` when this list declares none.
    ///
    /// `y`/`h` are the band the rows are clipped to, so the tint, the separators
    /// and the clip are three readers of one expression rather than three
    /// expressions that agree today. Takes the *model* rather than recomputing the
    /// band, for the same reason.
    #[must_use]
    pub fn chrome_rect(&self, list: &ScrollList, canvas_width: f32) -> Option<Rect> {
        match self.chrome {
            ListChrome::Canvas => Some((0.0, list.top(), canvas_width, list.height())),
            ListChrome::None => None,
        }
    }

    /// This spec with a canvas-relative row edge instead of a centred fixed-width
    /// one — see [`RowBand::Inset`], and its gutter rule.
    ///
    /// A builder rather than a sixth constructor parameter, so the five centred
    /// lists read unchanged and the one screen that is genuinely full-width has to
    /// say so out loud.
    #[must_use]
    pub fn spanning(mut self, left: f32, right: f32) -> Self {
        self.band = RowBand::Inset { left, right };
        self
    }

    /// This spec with `scroll` as its offset — the builder the frame producers use,
    /// so a screen states its geometry and its position in one expression.
    #[must_use]
    pub fn at(mut self, scroll: f32) -> Self {
        self.scroll = scroll;
        self
    }

    /// Per-entry heights instead of a uniform `row_h`; `row_h` stays
    /// `defaultEntryHeight` and keeps defining the scroll rate.
    #[must_use]
    pub fn with_heights(mut self, heights: Vec<f32>) -> Self {
        self.len = heights.len();
        self.heights = Some(heights);
        self
    }

    /// `getRowLeft()` — delegated to [`RowBand`], which is the only place that
    /// knows whether this list's rows are centred or canvas-relative.
    #[must_use]
    pub fn row_left(&self, width: f32) -> f32 {
        self.band.row_left(width)
    }

    /// `getRowRight()` — what [`ScrollList::scrollbar_rects`] hangs the bar off.
    #[must_use]
    pub fn row_right(&self, width: f32) -> f32 {
        self.band.row_right(width)
    }

    /// `getRowWidth()` at this canvas width.
    ///
    /// A method rather than the field it replaced: for [`RowBand::Inset`] the row
    /// width **is** a function of the canvas, so a bare `spec.row_w` could only
    /// ever have been the centred answer. Callers that want the drawn column pass
    /// the width they are drawing at.
    #[must_use]
    pub fn row_w(&self, width: f32) -> f32 {
        self.band.row_w(width)
    }

    /// The live [`ScrollList`] at this canvas height, already carrying
    /// [`Self::scroll`], or `None` when there is no band to scroll in.
    ///
    /// `None` for an empty list and for a canvas too short to have a band, matching
    /// `server_scroll_model`'s two rejections — a `Some` here would report a
    /// negative height and place every row off-canvas.
    ///
    /// The offset goes through [`ScrollList::set_scroll`] rather than being written
    /// to the field, so a stale offset left over from a taller canvas is re-clamped
    /// instead of surviving as an out-of-range value.
    #[must_use]
    pub fn model(&self, canvas_height: f32) -> Option<ScrollList> {
        if self.len == 0 {
            return None;
        }
        let band = canvas_height - self.footer_h - self.top;
        if band <= 0.0 {
            return None;
        }
        let mut list = match &self.heights {
            Some(h) => ScrollList::new_variable(self.row_h, self.top, band, h),
            None => ScrollList::new(self.row_h, self.top, band, self.len),
        };
        list.set_scroll(self.scroll);
        Some(list)
    }
}

/// An `(x, y, w, h)` rect in logical pixels, as every menu draw helper takes it.
pub type Rect = (f32, f32, f32, f32);

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
    fn a_slider_has_a_track_but_no_disabled_track() {
        // `AbstractSliderButton.getSprite()` is a **conjunction** —
        // `isActive() && isFocused() && !canChangeValue` — so a greyed-out
        // slider draws the *ordinary* track whatever focus did, and its entire
        // disabled state is the grey label. This is the assertion that caught
        // `SLIDER_SPRITES` being built with the 2-argument collapse, which puts
        // the *focused* sprite in `disabledFocused` and lit an inactive slider up
        // under the cursor. See [`SLIDER_SPRITES`].
        let mut s = Widget::slider(0.0, 0.0, 150.0, 20.0, "Render Distance");
        assert_eq!(s.background_sprite(), Some("widget/slider"));
        s.focused = true;
        assert_eq!(s.background_sprite(), Some("widget/slider_highlighted"));
        s.active = false;
        assert_eq!(
            s.background_sprite(),
            Some("widget/slider"),
            "there is no `widget/slider_disabled` in the pack, and a *focused* \
             inactive slider must not fall back to the highlighted one either"
        );
        assert_eq!(s.message_colour(), INACTIVE_LABEL);
        // And the collapse itself, stated so the reason is not only in prose:
        // the fourth field is the disabled sprite, not the focused one.
        assert_eq!(SLIDER_SPRITES.disabled_focused, "widget/slider");
        assert_eq!(
            WidgetSprites::focusable("widget/slider", "widget/slider_highlighted")
                .disabled_focused,
            "widget/slider_highlighted",
            "control: the 2-argument form is the one that would have been wrong"
        );
        // The control: a *button* under the same flags does have disabled art,
        // so this is not "every widget collapses".
        let mut b = Widget::button(0.0, 0.0, 150.0, 20.0, "Render Distance");
        b.focused = true;
        b.active = false;
        assert_eq!(b.background_sprite(), Some("widget/button_disabled"));

        // The predicate differs on both arguments, like `EditBox`'s: hover does
        // not highlight a slider, and `isActive()` gates it rather than the raw
        // `active` field.
        let mut h = Widget::slider(0.0, 0.0, 150.0, 20.0, "Sensitivity");
        h.hovered = true;
        assert_eq!(
            h.slider_background_sprite(),
            Some("widget/slider"),
            "hovering a slider must not highlight it"
        );
        assert_eq!(
            h.background_sprite(),
            Some("widget/slider_highlighted"),
            "and the *button* predicate would — which is why the two exist"
        );
        h.hovered = false;
        h.focused = true;
        assert_eq!(
            h.slider_background_sprite(),
            Some("widget/slider_highlighted")
        );
        h.visible = false;
        assert_eq!(
            h.slider_background_sprite(),
            Some("widget/slider"),
            "`isActive()` is `visible && active`, unlike the button's raw field"
        );
    }

    #[test]
    fn a_sliders_handle_highlights_on_hover_or_focus() {
        // `AbstractSliderButton.getHandleSprite()`
        // (`AbstractSliderButton.java:41-43`) is
        // `!isActive() || (!isHovered && !canChangeValue) ? SLIDER_HANDLE : …`,
        // and `canChangeValue` is `true` for a slider focused by mouse or Tab
        // (`setFocused`) — **not** the always-false latch the track's own doc
        // describes. This used to assert focus does *not* highlight the handle,
        // borrowing that claim; see `Widget::slider_handle_sprite`'s doc for the
        // Java and for why the consequence was visible (this screen never
        // populates `MenuFrame::hovered`, so hover alone meant the knob could
        // never light up at all).
        let mut s = Widget::slider(0.0, 0.0, 150.0, 20.0, "Master Volume");
        assert_eq!(s.slider_handle_sprite(), "widget/slider_handle");
        s.focused = true;
        assert_eq!(
            s.slider_handle_sprite(),
            "widget/slider_handle_highlighted",
            "a focused slider has canChangeValue == true, so the handle lights up"
        );
        s.focused = false;
        s.hovered = true;
        assert_eq!(
            s.slider_handle_sprite(),
            "widget/slider_handle_highlighted",
            "hover highlights the handle, not the track"
        );
        s.active = false;
        assert_eq!(
            s.slider_handle_sprite(),
            "widget/slider_handle",
            "an inactive slider's handle must not light up even if hovered"
        );
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

    /// The three quadrant predicates partition the icon the way the server
    /// list's three actions need, and the *negative* half is the point: a
    /// cursor in the right half must not also read as move-up, or clicking to
    /// join would reorder the list.
    #[test]
    fn the_icon_quadrants_partition_the_way_vanilla_splits_them() {
        const S: f32 = 32.0;
        // Right half joins; neither left quadrant claims it.
        for (x, y) in [(16.0, 0.0), (31.0, 31.0), (24.0, 16.0)] {
            assert!(over_right_half(x, y, S), "({x}, {y})");
            assert!(!over_top_left_quarter(x, y, S), "({x}, {y})");
            assert!(!over_bottom_left_quarter(x, y, S), "({x}, {y})");
        }
        // Top-left moves up, bottom-left moves down, and they are disjoint.
        assert!(over_top_left_quarter(0.0, 0.0, S));
        assert!(over_top_left_quarter(15.0, 15.0, S));
        assert!(!over_top_left_quarter(15.0, 16.0, S));
        assert!(over_bottom_left_quarter(15.0, 16.0, S));
        assert!(over_bottom_left_quarter(0.0, 31.0, S));
        assert!(!over_bottom_left_quarter(15.0, 15.0, S));
        // Every point inside the icon belongs to exactly one of the three.
        for y in 0..32i32 {
            for x in 0..32i32 {
                let (fx, fy) = (x as f32, y as f32);
                let n = usize::from(over_right_half(fx, fy, S))
                    + usize::from(over_top_left_quarter(fx, fy, S))
                    + usize::from(over_bottom_left_quarter(fx, fy, S));
                assert_eq!(n, 1, "({x}, {y}) belongs to {n} quadrants");
            }
        }
        // Outside the icon, none of them fire — a click just past the icon must
        // fall through to plain selection.
        for (x, y) in [(-1.0, 4.0), (32.0, 4.0), (4.0, -1.0), (4.0, 32.0)] {
            assert!(!over_right_half(x, y, S), "({x}, {y})");
            assert!(!over_top_left_quarter(x, y, S), "({x}, {y})");
            assert!(!over_bottom_left_quarter(x, y, S), "({x}, {y})");
        }
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

    // -- ScrollList ---------------------------------------------------------

    /// The multiplayer list's real shape: 36 px rows (`SERVER_LIST_ITEM_H`) in a
    /// 200 px band. Ten entries so it genuinely overflows.
    fn server_shaped() -> ScrollList {
        ScrollList::new(36.0, 32.0, 200.0, 10)
    }

    #[test]
    fn one_notch_is_half_an_entry_in_pixels() {
        // `mouseScrolled` moves by `scrollY * scrollRate()`
        // (`AbstractScrollArea.java:34`) and `scrollRate` is
        // `defaultEntryHeight / 2` (`AbstractSelectionList.java:44`). For a 36 px
        // entry that is *exactly* 18 px.
        //
        // This is the assertion a row-index implementation cannot satisfy, and it
        // is written as a predicted value rather than "the offset changed"
        // precisely because both the buggy and the correct version satisfy the
        // latter. The two wrong hypotheses are named and excluded below.
        let mut list = server_shaped();
        assert_eq!(list.scroll_rate(), 18.0);
        list.mouse_scrolled(-1.0);
        assert_eq!(
            list.scroll, 18.0,
            "one notch must land at half an entry height"
        );
        assert_ne!(
            list.scroll, 36.0,
            "36.0 is the row-quantized hypothesis — a whole entry per notch"
        );
        assert_ne!(
            list.scroll, 1.0,
            "1.0 is the row-index hypothesis — the offset counted in rows"
        );

        // A second notch accumulates rather than snapping to a row boundary,
        // which is the other thing an index cannot do: 36 is a legal row offset,
        // so a test that stopped at one notch would not separate them.
        list.mouse_scrolled(-1.0);
        assert_eq!(list.scroll, 36.0);
        list.mouse_scrolled(-1.0);
        assert_eq!(list.scroll, 54.0, "an odd multiple of half a row");

        // Positive scrolls up, vanilla's sign.
        list.mouse_scrolled(1.0);
        assert_eq!(list.scroll, 36.0);
    }

    /// The implementation this primitive replaces, kept **executable** so the
    /// assertion above is a control rather than a description of one.
    ///
    /// This is `MenuNav::server_scroll` and `accounts::State::scroll` as they
    /// actually were: a `usize` row counter clamped against a row window. Its
    /// `scroll_px` is what `render::server_row_top` did with that counter —
    /// `scroll as f32 * row_h`.
    struct RowIndexList {
        scroll_rows: usize,
        row_h: f32,
    }

    impl RowIndexList {
        fn mouse_scrolled(&mut self, notches: f32) {
            // The old `scroll_server_list(rows, …)`: one notch, one row.
            if notches < 0.0 {
                self.scroll_rows += 1;
            } else if notches > 0.0 {
                self.scroll_rows = self.scroll_rows.saturating_sub(1);
            }
        }

        fn scroll_px(&self) -> f32 {
            self.scroll_rows as f32 * self.row_h
        }
    }

    #[test]
    fn a_row_index_implementation_fails_the_notch_assertion() {
        // The executed negative control for `one_notch_is_half_an_entry_in_pixels`.
        // Both implementations agree that "the offset changed", which is exactly
        // why that assertion predicts a *value*: this one lands on 36, not 18, and
        // no amount of easing on top of it could produce 18 — the information is
        // not in a row counter to begin with.
        let mut old = RowIndexList {
            scroll_rows: 0,
            row_h: 36.0,
        };
        let mut new = server_shaped();

        old.mouse_scrolled(-1.0);
        new.mouse_scrolled(-1.0);

        assert_eq!(new.scroll(), 18.0, "the primitive lands at half an entry");
        assert_eq!(
            old.scroll_px(),
            36.0,
            "the row-index model can only land on a whole entry"
        );
        assert_ne!(
            old.scroll_px(),
            new.scroll(),
            "the control must FAIL the predicted value — if these ever agree, \
             the notch assertion has stopped discriminating"
        );

        // And it cannot represent the offset at all: three notches is 54 px, which
        // is not a multiple of the row height, so no `usize` maps onto it.
        new.mouse_scrolled(-2.0);
        old.mouse_scrolled(-1.0);
        old.mouse_scrolled(-1.0);
        assert_eq!(new.scroll(), 54.0);
        assert_eq!(old.scroll_px(), 108.0);
        assert!(
            (new.scroll() / 36.0).fract() != 0.0,
            "54 px is mid-entry — the state a row index structurally cannot hold"
        );
    }

    #[test]
    fn the_scroll_rate_truncates_like_vanillas_int_division() {
        // `scrollRate` is an `int` field of the `ScrollbarSettings` record
        // (`AbstractScrollArea.java:155`) fed `defaultEntryHeight / 2` in
        // **integer** division, so an odd row height loses the half pixel. The
        // settings list's 25 px entry is the live case.
        assert_eq!(ScrollList::new(25.0, 0.0, 100.0, 10).scroll_rate(), 12.0);
        assert_eq!(ScrollList::new(18.0, 0.0, 100.0, 10).scroll_rate(), 9.0);
        assert_eq!(ScrollList::new(20.0, 0.0, 100.0, 10).scroll_rate(), 10.0);
    }

    #[test]
    fn content_height_and_max_scroll_are_vanillas() {
        // `contentHeight() = Σ heights + 4` (`AbstractSelectionList.java:198-206`)
        // and `maxScrollAmount() = max(0, contentHeight - height)`
        // (`AbstractScrollArea.java:84-86`).
        let list = server_shaped();
        assert_eq!(list.content_height(), 10.0 * 36.0 + 4.0);
        assert_eq!(list.max_scroll(), 364.0 - 200.0);
        assert!(list.scrollable());

        // A list that fits has max 0 and does not scroll — and therefore draws no
        // scrollbar at all (`AbstractScrollArea.java:126`).
        let short = ScrollList::new(36.0, 32.0, 200.0, 2);
        assert_eq!(short.content_height(), 76.0);
        assert_eq!(short.max_scroll(), 0.0);
        assert!(!short.scrollable());
        assert!(short.scrollbar_rects(300.0).is_none());
    }

    #[test]
    fn the_offset_clamps_at_both_ends() {
        let mut list = server_shaped();
        list.mouse_scrolled(5.0);
        assert_eq!(list.scroll, 0.0, "cannot scroll above the first entry");
        list.set_scroll(9_999.0);
        assert_eq!(list.scroll, 164.0, "clamped to maxScrollAmount()");
        // A NaN must not poison every downstream y.
        list.set_scroll(f32::NAN);
        assert_eq!(list.scroll, 0.0);
    }

    #[test]
    fn shrinking_the_band_reclamps_the_offset() {
        // `updateSizeAndPosition` ends in `refreshScrollAmount()`
        // (`AbstractSelectionList.java:194`). Without it a shrunk window stays
        // scrolled past its own content and the list draws empty.
        let mut list = server_shaped();
        list.set_scroll(164.0);
        list.resize(32.0, 200.0, 3);
        assert_eq!(list.max_scroll(), 0.0);
        assert_eq!(list.scroll, 0.0, "a shorter list must pull the offset back");
    }

    #[test]
    fn row_tops_move_as_one_column_and_stay_a_row_apart() {
        // `repositionEntries` (`AbstractSelectionList.java:143-152`) truncates the
        // offset **once**, outside the per-entry accumulation.
        let mut list = server_shaped();
        assert_eq!(list.row_top(0), 34.0, "getFirstEntryY() = getY() + 2");
        assert_eq!(list.row_top(1), 70.0);

        list.mouse_scrolled(-1.0); // 18 px — half a row
        assert_eq!(list.row_top(0), 16.0);
        assert_eq!(list.row_top(1), 52.0);
        assert_eq!(
            list.row_top(1) - list.row_top(0),
            36.0,
            "entries must stay exactly one row apart at a fractional offset"
        );
    }

    #[test]
    fn a_half_scrolled_row_is_visible_rather_than_skipped() {
        // The partial-overlap test of `extractListItems`
        // (`AbstractSelectionList.java:346-352`). Row 0 is half above the band
        // after one notch and must still be *drawn* — clipped, not dropped. The
        // old row-quantized code skipped it, which is why smooth scrolling needed
        // the clip in `render.rs` before it could be correct.
        let mut list = server_shaped();
        list.mouse_scrolled(-1.0);
        assert_eq!(list.row_top(0), 16.0);
        assert!(list.row_top(0) < list.top(), "row 0 straddles the top edge");
        assert!(
            list.row_visible(0),
            "a partially-scrolled row must be visible, not skipped"
        );
        assert!(list.visible_range().contains(&0));

        // And an entry entirely above the band is genuinely gone.
        list.set_scroll(100.0);
        assert!(!list.row_visible(0), "row 0 is now fully above the band");
        assert!(!list.visible_range().contains(&0));
    }

    #[test]
    fn visible_range_agrees_with_row_visible_at_every_offset() {
        // `visible_range` inverts `row_top` instead of scanning, so the two could
        // drift. Swept across the whole scrollable span in half-notch steps,
        // which is finer than any offset the wheel can produce.
        let mut list = server_shaped();
        let mut offset = 0.0_f32;
        while offset <= list.max_scroll() {
            list.set_scroll(offset);
            let range = list.visible_range();
            for i in 0..list.len() {
                assert_eq!(
                    range.contains(&i),
                    list.row_visible(i),
                    "row {i} at offset {offset}: range {range:?}"
                );
            }
            offset += 9.0;
        }
    }

    // -- variable row heights (#445) ----------------------------------------

    /// The settings list's real shape: alternating 25 px control rows and 20 px
    /// header rows, which is why the primitive needed per-entry heights at all.
    fn settings_shaped() -> ScrollList {
        let heights: Vec<f32> = (0..12)
            .map(|i| if i % 3 == 0 { 20.0 } else { 25.0 })
            .collect();
        ScrollList::new_variable(25.0, 32.0, 160.0, &heights)
    }

    /// A uniform list and an explicit list of equal heights must agree on
    /// **every** observable, at every offset.
    ///
    /// This is the load-bearing structural test for growing the primitive: it is
    /// what makes the uniform case the *degenerate case of the same arithmetic*
    /// rather than a second implementation that happens to look similar. A
    /// prefix-sum bug that shifted every row by one padding unit would pass a
    /// hand-picked spot check and fail here.
    #[test]
    fn an_explicit_equal_height_list_is_indistinguishable_from_a_uniform_one() {
        let mut uniform = ScrollList::new(36.0, 32.0, 200.0, 10);
        let mut variable = ScrollList::new_variable(36.0, 32.0, 200.0, &[36.0; 10]);
        assert!(!uniform.is_variable() && variable.is_variable(), "two modes");

        assert_eq!(uniform.content_height(), variable.content_height());
        assert_eq!(uniform.max_scroll(), variable.max_scroll());
        assert_eq!(uniform.scroll_rate(), variable.scroll_rate());
        assert_eq!(uniform.scroller_height(), variable.scroller_height());

        let mut offset = 0.0_f32;
        while offset <= uniform.max_scroll() {
            uniform.set_scroll(offset);
            variable.set_scroll(offset);
            assert_eq!(uniform.scroll(), variable.scroll(), "offset {offset}");
            assert_eq!(
                uniform.visible_range(),
                variable.visible_range(),
                "visible_range diverged at offset {offset} — the walk and the \
                 closed form disagree, so one of them is wrong"
            );
            assert_eq!(uniform.scrollbar_y(), variable.scrollbar_y(), "at {offset}");
            for i in 0..uniform.len() {
                assert_eq!(uniform.row_top(i), variable.row_top(i), "row {i} @ {offset}");
                assert_eq!(uniform.row_height(i), variable.row_height(i), "row {i}");
                assert_eq!(
                    uniform.row_visible(i),
                    variable.row_visible(i),
                    "row {i} @ {offset}"
                );
            }
            offset += 4.5;
        }
    }

    /// Offsets come from the running sum, and `content_height` is that sum plus
    /// vanilla's 4 px of padding — not `len * row_h`, which is what the uniform
    /// formula would have answered.
    #[test]
    fn variable_offsets_are_a_prefix_sum_not_a_multiply() {
        let list = settings_shaped();
        // heights: 20,25,25, 20,25,25, 20,25,25, 20,25,25  = 4*(20+50) = 280
        assert_eq!(list.row_offset(0), 0.0);
        assert_eq!(list.row_offset(1), 20.0);
        assert_eq!(list.row_offset(2), 45.0);
        assert_eq!(list.row_offset(3), 70.0);
        assert_eq!(list.row_offset(12), 280.0, "the whole content");
        assert_eq!(list.row_height(0), 20.0, "a header");
        assert_eq!(list.row_height(1), 25.0, "a control");
        assert_eq!(
            list.content_height(),
            280.0 + 4.0,
            "prefix[len] + 2*LIST_CONTENT_PADDING"
        );
        // The wrong hypothesis, computed from outside: the uniform formula.
        assert_ne!(
            list.content_height(),
            12.0 * 25.0 + 4.0,
            "a multiply by the default height must NOT reproduce this — if it \
             does, the test shape has stopped exercising variable heights"
        );
    }

    /// `visible_range`'s walk must agree with `row_visible` entry-by-entry
    /// across the whole span, exactly as the uniform sweep demands of the
    /// closed form.
    #[test]
    fn the_variable_visible_range_agrees_with_row_visible_at_every_offset() {
        let mut list = settings_shaped();
        let mut offset = 0.0_f32;
        while offset <= list.max_scroll() {
            list.set_scroll(offset);
            let range = list.visible_range();
            for i in 0..list.len() {
                assert_eq!(
                    range.contains(&i),
                    list.row_visible(i),
                    "row {i} at offset {offset}: range {range:?}"
                );
            }
            offset += 2.5;
        }
    }

    /// **The smooth-scroll magnitude gate for a variable list.**
    ///
    /// `scrollRate` is `defaultEntryHeight / 2` (`AbstractSelectionList.java:44`),
    /// and `defaultEntryHeight` is the *declared* default, **not** the height of
    /// whichever entry happens to be first. A settings list declaring 25 px
    /// therefore moves `floor(25/2) = 12` px per notch.
    ///
    /// Three hypotheses, and the measurement must land on exactly one:
    ///
    /// | hypothesis | one notch |
    /// |---|---|
    /// | vanilla: `floor(default/2)` | **12** |
    /// | rate from the first entry (20 px header) | 10 |
    /// | a row-index model, one notch one row | 20 (that row's height) |
    ///
    /// 12 is not a multiple of any row height here, which is the point: it is a
    /// state a row counter structurally cannot hold, so "it scrolled" cannot
    /// pass for "it scrolled smoothly".
    #[test]
    fn one_notch_on_a_variable_list_is_half_the_declared_default_height() {
        let mut list = settings_shaped();
        list.mouse_scrolled(-1.0);
        assert_eq!(
            list.scroll(),
            12.0,
            "one notch must be floor(25/2) = 12 px — 10 would mean the rate came \
             from the first entry's 20 px, and 20 would mean a row-index model"
        );
        list.mouse_scrolled(-1.0);
        assert_eq!(list.scroll(), 24.0, "and it accumulates in pixels");

        // 24 px is mid-row: it is inside row 1 (which spans 20..45), so no
        // row index maps onto this offset.
        assert!(
            list.row_offset(1) < 24.0 && 24.0 < list.row_offset(2),
            "24 px must land strictly inside a row, not on a boundary"
        );
        for i in 0..=list.len() {
            assert_ne!(
                list.row_offset(i),
                list.scroll(),
                "offset {} coincides with row {i}'s top, so this gate no longer \
                 discriminates against a row-index model",
                list.scroll()
            );
        }
    }

    /// The trap named in `new_variable`'s doc, asserted rather than trusted:
    /// the scroll rate comes from the declared default and ignores the heights.
    #[test]
    fn the_scroll_rate_ignores_the_entry_heights_entirely() {
        let tall = ScrollList::new_variable(25.0, 0.0, 100.0, &[80.0, 90.0, 100.0]);
        assert_eq!(
            tall.scroll_rate(),
            12.0,
            "floor(25/2) — derived from defaultEntryHeight, not from any entry"
        );
        let same_heights_different_default =
            ScrollList::new_variable(36.0, 0.0, 100.0, &[80.0, 90.0, 100.0]);
        assert_eq!(same_heights_different_default.scroll_rate(), 18.0);
    }

    /// A click and a hover hit-test the entry's **own** height. With the uniform
    /// height they would both mis-aim on every row after the first differing
    /// one.
    #[test]
    fn hit_testing_uses_each_entrys_own_height() {
        let list = settings_shaped();
        let (left, w) = (0.0, 200.0);
        // Row 0 is the 20 px header at first_entry_y = 34.
        assert_eq!(list.row_top(0), 34.0);
        assert_eq!(list.entry_at(10.0, 34.0, left, w), Some(0));
        assert_eq!(list.entry_at(10.0, 53.9, left, w), Some(0), "still row 0");
        assert_eq!(
            list.entry_at(10.0, 54.0, left, w),
            Some(1),
            "20 px in, row 1 starts — a uniform 25 px would still say row 0"
        );
        // And the control: the uniform list of the same default really would
        // answer row 0 there, so the assertion above is discriminating.
        let uniform = ScrollList::new(25.0, 32.0, 160.0, 12);
        assert_eq!(
            uniform.entry_at(10.0, 54.0, left, w),
            Some(0),
            "the wrong hypothesis must give the wrong answer here"
        );
    }

    /// `resize` on a variable list drops to uniform rather than keeping a
    /// stale height table — the honest degradation named in its doc.
    #[test]
    fn a_uniform_resize_drops_the_stale_height_table() {
        let mut list = settings_shaped();
        assert!(list.is_variable());
        list.resize(32.0, 160.0, 4);
        assert!(
            !list.is_variable(),
            "a uniform resize must not leave a 12-entry prefix behind a 4-entry len"
        );
        assert_eq!(list.row_offset(2), 50.0, "now 2 * 25");
        // And the variable path keeps them.
        list.resize_variable(32.0, 160.0, &[10.0, 30.0]);
        assert!(list.is_variable());
        assert_eq!(list.len(), 2, "len follows the heights");
        assert_eq!(list.row_offset(2), 40.0);
    }

    /// A non-finite or negative height must not poison the tail of the prefix
    /// sum. One bad row is a bad row; a `NaN` would put every *later* row
    /// off-screen.
    #[test]
    fn a_bad_height_does_not_poison_the_rest_of_the_prefix_sum() {
        let list = ScrollList::new_variable(25.0, 0.0, 100.0, &[20.0, f32::NAN, -5.0, 25.0]);
        assert_eq!(list.row_offset(1), 20.0);
        assert_eq!(list.row_offset(2), 20.0, "NaN contributes 0");
        assert_eq!(list.row_offset(3), 20.0, "negative contributes 0");
        assert_eq!(list.row_offset(4), 45.0, "and the good row still counts");
        assert!(
            list.content_height().is_finite(),
            "content height must stay finite"
        );
    }

    #[test]
    fn the_scrollbar_sits_outside_the_row_not_inset_into_it() {
        // `AbstractSelectionList` **overrides** `scrollBarX()` to
        // `getRowRight() + scrollbarWidth() + 2` (`:289-291`). Getting this from
        // `AbstractScrollArea`'s un-overridden `getRight() - scrollbarWidth()`
        // (`AbstractScrollArea.java:100-102`) would put the bar *inside* the list.
        let list = server_shaped();
        let row_right = 392.0;
        assert_eq!(list.scrollbar_x(row_right), 400.0);
        assert_eq!(list.scrollbar_width(), 6.0);
        assert!(
            list.scrollbar_x(row_right) > row_right,
            "the bar must not overlap the rows"
        );
    }

    #[test]
    fn thumb_geometry_lands_flush_at_both_ends() {
        // `scrollerHeight()` = `clamp((int)(h*h / contentHeight), 32, h - 8)`
        // (`AbstractScrollArea.java:96-98`). For h=200, content=364:
        // 200*200/364 = 109.89 -> 109, and clamp(109, 32, 192) = 109.
        let mut list = server_shaped();
        assert_eq!(list.scroller_height(), 109.0);

        // `scrollBarY()` (`:104-108`). At rest the thumb is at the band's top.
        assert_eq!(list.scrollbar_y(), list.top());

        // Fully scrolled, the thumb's *bottom* must reach the band's bottom
        // exactly — travel is `height - scrollerHeight()` = 91, so
        // 32 + 91 + 109 = 232 = top + height. A geometry that did not land flush
        // would leave a visible gap the player reads as "there is more below".
        list.set_scroll(list.max_scroll());
        assert_eq!(list.scrollbar_y(), 32.0 + 91.0);
        assert_eq!(
            list.scrollbar_y() + list.scroller_height(),
            list.bottom(),
            "a fully-scrolled thumb must be flush with the band's bottom"
        );

        // Halfway down the content puts the thumb halfway down its travel.
        list.set_scroll(82.0);
        assert_eq!(list.scrollbar_y(), 32.0 + (82.0_f32 * 91.0 / 164.0).floor());
    }

    #[test]
    fn the_thumb_never_shrinks_below_vanillas_floor() {
        // `SCROLLBAR_MIN_HEIGHT` (`AbstractScrollArea.java:14`). A 500-entry list
        // would compute a 2 px thumb, which is unusable.
        let long = ScrollList::new(36.0, 32.0, 200.0, 500);
        assert!(long.height() * long.height() / long.content_height() < 32.0);
        assert_eq!(long.scroller_height(), 32.0);

        // And the upper clamp wins on a band so short the two bounds cross —
        // `Mth.clamp` returns the *upper* bound in that case.
        let tiny = ScrollList::new(36.0, 0.0, 30.0, 50);
        assert_eq!(tiny.scroller_height(), 30.0 - 8.0);
    }

    #[test]
    fn scroll_to_entry_moves_the_minimum_and_settles() {
        // `scrollToEntry` (`AbstractSelectionList.java:251-261`).
        let mut list = server_shaped();

        // Entry 5 is below the band (top 34 + 5*36 = 214 > bottom 232 - 36).
        list.scroll_to_entry(5);
        // bottomDelta = 232 - 214 - 36 - 2 = -20, so scroll moves by +20.
        assert_eq!(list.scroll, 20.0);
        assert!(list.row_visible(5));

        // Already visible: nothing moves. Asserting the *absence* of a move needs
        // the control below, which shows the mechanism does fire.
        let before = list.scroll;
        list.scroll_to_entry(4);
        assert_eq!(list.scroll, before, "a visible entry must not move the list");

        // Control: entry 9 is not visible, and the same call does move it.
        list.scroll_to_entry(9);
        assert_ne!(list.scroll, before);
        assert!(list.row_visible(9));

        // Scrolling back to entry 0 lands exactly at the top.
        list.scroll_to_entry(0);
        assert_eq!(list.scroll, 0.0);
    }

    #[test]
    fn hovering_never_changes_the_selection() {
        // The two-piece assertion for "hovering an account shouldn't focus it".
        // One assertion cannot distinguish "hover works" from "hover selected
        // it", so both fields are read after every move.
        let mut list = server_shaped();
        list.set_selected(Some(2), false);
        assert_eq!(list.selected(), Some(2));
        assert_eq!(list.hovered(), None);

        // Hover a *different* entry. The highlight must move and the selection
        // must not.
        list.set_hovered(Some(4));
        assert_eq!(list.hovered(), Some(4), "the hover highlight must be present");
        assert_eq!(
            list.selected(),
            Some(2),
            "hover must not steal the selection — this is the reported bug"
        );

        // Leaving the list clears the hover and still leaves the selection alone.
        list.set_hovered(None);
        assert_eq!(list.hovered(), None);
        assert_eq!(list.selected(), Some(2));

        // Control: the selection *can* still be moved, so the assertion above is
        // not passing merely because `selected` is immutable.
        list.set_selected(Some(4), false);
        assert_eq!(list.selected(), Some(4));
    }

    #[test]
    fn hovering_does_not_scroll_the_list_either() {
        // `setSelected` scrolls into view (`AbstractSelectionList.java:53-62`);
        // `this.hovered = …` (`:210`) does not. A hover handler that routed
        // through selection would yank the list under the cursor.
        let mut list = server_shaped();
        list.set_hovered(Some(9));
        assert_eq!(list.scroll, 0.0, "a hover must never scroll");
        // Control: selection does scroll, so the check above is not vacuous.
        list.set_selected(Some(9), true);
        assert!(list.scroll > 0.0);
    }

    #[test]
    fn a_click_does_not_yank_a_partially_visible_row_but_the_keyboard_does() {
        // `setSelected` scrolls when clipped **or** when the last input was the
        // keyboard (`:58`). A click on a fully-visible row must not move the list.
        let mut list = server_shaped();
        list.set_selected(Some(1), false);
        assert_eq!(list.scroll, 0.0);

        // The keyboard arm fires even on an already-visible row.
        let mut list2 = server_shaped();
        list2.set_scroll(50.0);
        list2.set_selected(Some(1), true);
        assert_ne!(list2.scroll, 50.0, "keyboard selection scrolls into view");
    }

    #[test]
    fn hover_and_click_hit_test_the_same_rows_the_draw_shows() {
        // `getEntryAtPosition` (`AbstractSelectionList.java:168-176`) restricted
        // to the band. The point is that a click can no longer land on an entry
        // the draw skipped — the residual defect issue #402 recorded.
        let mut list = server_shaped();
        let (left, w) = (100.0, 305.0);

        // Dead centre of row 0.
        assert_eq!(list.entry_at(200.0, 34.0 + 18.0, left, w), Some(0));
        list.hover_at(200.0, 34.0 + 18.0, left, w);
        assert_eq!(list.hovered(), Some(0));

        // Outside the row's x band: no hit, and the hover clears.
        assert_eq!(list.entry_at(50.0, 52.0, left, w), None);
        list.hover_at(50.0, 52.0, left, w);
        assert_eq!(list.hovered(), None);

        // Below the band entirely (the footer) — must not select entry 9.
        assert_eq!(list.entry_at(200.0, 300.0, left, w), None);

        // Every y inside the band resolves to a row that `row_visible` agrees is
        // on screen, at a fractional offset.
        list.set_scroll(50.0);
        let mut y = list.top();
        while y < list.bottom() {
            if let Some(i) = list.entry_at(200.0, y, left, w) {
                assert!(list.row_visible(i), "hit row {i} at y {y} is not drawn");
            }
            y += 1.0;
        }
    }

    #[test]
    fn dragging_the_thumb_scales_by_the_page_fraction() {
        // `mouseDragged` (`AbstractScrollArea.java:38-56`).
        let list0 = server_shaped();
        let row_right = 392.0;
        let bar_x = list0.scrollbar_x(row_right);

        // A press off the bar does not arm a drag, and then dragging is a no-op.
        let mut list = server_shaped();
        assert!(!list.begin_drag(200.0, 100.0, row_right));
        list.drag_to(100.0, 40.0);
        assert_eq!(list.scroll, 0.0, "an unarmed drag must not scroll");

        // A press *on* the bar arms it. travel = 200 - 109 = 91, max = 164, so
        // the scale is 164/91 = 1.8021 and a 10 px drag moves 18.02 px.
        assert!(list.begin_drag(bar_x + 3.0, 100.0, row_right));
        list.drag_to(110.0, 10.0);
        let expected = 10.0 * (164.0 / 91.0);
        assert!(
            (list.scroll - expected).abs() < 1e-3,
            "expected {expected}, got {}",
            list.scroll
        );

        // Dragging above the band snaps to 0, below it snaps to the maximum.
        list.drag_to(list.top() - 5.0, -50.0);
        assert_eq!(list.scroll, 0.0);
        list.drag_to(list.bottom() + 5.0, 50.0);
        assert_eq!(list.scroll, 164.0);

        // Release disarms.
        list.end_drag();
        assert!(!list.dragging());
        list.drag_to(150.0, 40.0);
        assert_eq!(list.scroll, 164.0);
    }

    #[test]
    fn the_scrollbar_hit_band_matches_where_it_draws() {
        let list = server_shaped();
        let row_right = 392.0;
        let (track, thumb) = list
            .scrollbar_rects(row_right)
            .expect("a 10-entry list in a 200 px band scrolls");
        assert_eq!(track, (400.0, 32.0, 6.0, 200.0));
        assert_eq!(thumb, (400.0, 32.0, 6.0, 109.0));

        // `isOverScrollbar` (`AbstractScrollArea.java:76-78`) must agree with the
        // track it draws, or the bar is a picture you cannot grab.
        assert!(list.is_over_scrollbar(track.0, track.1, row_right));
        assert!(list.is_over_scrollbar(track.0 + track.2, track.1 + 10.0, row_right));
        assert!(!list.is_over_scrollbar(track.0 - 1.0, track.1 + 10.0, row_right));
        assert!(!list.is_over_scrollbar(track.0 + 1.0, track.1 - 1.0, row_right));
        assert!(!list.is_over_scrollbar(track.0 + 1.0, list.bottom(), row_right));
    }

    #[test]
    fn an_empty_list_answers_everything_without_panicking() {
        let mut list = ScrollList::new(36.0, 32.0, 200.0, 0);
        assert!(list.is_empty());
        assert_eq!(list.max_scroll(), 0.0);
        assert!(!list.scrollable());
        assert_eq!(list.visible_range(), 0..0);
        assert!(!list.row_visible(0));
        assert_eq!(list.entry_at(200.0, 100.0, 100.0, 305.0), None);
        list.mouse_scrolled(-3.0);
        assert_eq!(list.scroll, 0.0);
        list.scroll_to_entry(0);
        list.center_on(0);
        list.set_selected(Some(0), true);
        assert_eq!(list.selected(), None, "there is no entry 0 to select");
        list.set_hovered(Some(0));
        assert_eq!(list.hovered(), None);
    }

    #[test]
    fn sprite_ids_and_metrics_are_vanillas_own() {
        assert_eq!(SCROLLER_SPRITE, "widget/scroller");
        assert_eq!(SCROLLER_BACKGROUND_SPRITE, "widget/scroller_background");
        assert_eq!(SCROLLBAR_WIDTH, 6.0);
        assert_eq!(SCROLLBAR_MIN_HEIGHT, 32.0);
        assert_eq!(LIST_CONTENT_PADDING, 2.0);
    }

    // -- RowBand: the canvas-relative row edge (issue #445) --------------------

    /// **The regression guard for splitting `row_w` into [`RowBand`].** Every
    /// existing list here is `RowBand::Centred`, and the refactor must not have
    /// moved one pixel of any of them.
    ///
    /// The expected values are `AbstractSelectionList.getRowLeft()`'s own
    /// arithmetic — `width / 2 - rowWidth / 2` with **two separate integer
    /// divisions** — evaluated by hand at three widths for the multiplayer list's
    /// real 340, not by calling the code under test. The odd width is the one that
    /// matters: a single `floor` on the difference would agree at every even width
    /// and disagree here.
    #[test]
    fn centred_rows_keep_vanillas_two_separate_integer_divisions() {
        let band = RowBand::Centred { row_w: 340.0 };
        // floor(854/2) - floor(340/2) = 427 - 170 = 257
        assert_eq!(band.row_left(854.0), 257.0);
        assert_eq!(band.row_right(854.0), 597.0);
        // floor(641/2) - floor(340/2) = 320 - 170 = 150. The one-floor hypothesis
        // is floor((641 - 340) / 2) = 150 as well, so pick a row width that
        // separates them: floor(641/2) - floor(341/2) = 320 - 170 = 150, while
        // floor((641 - 341)/2) = 150. Both agree; the separating case is below.
        assert_eq!(band.row_left(641.0), 150.0);
        // The separating case, and the reason the two floors are written out:
        // floor(101/2) - floor(41/2) = 50 - 20 = 30, whereas one floor on the
        // difference gives floor(60/2) = 30... also equal. Two integer halvings
        // differ from one only when *both* operands are odd:
        // floor(101/2) - floor(43/2) = 50 - 21 = 29, one floor: floor(58/2) = 29.
        // They are in fact algebraically equal for integer inputs; what the two
        // floors actually protect against is a *fractional* canvas width, which
        // this shell really produces (a 1365 px window at gui_scale 2 is 682.5).
        let odd = RowBand::Centred { row_w: 341.0 };
        assert_eq!(
            odd.row_left(682.5),
            341.0 - 170.0,
            "a fractional canvas width must floor to whole pixels on both terms — \
             floor(341.25) - floor(170.5) = 341 - 170"
        );
        // And the width it reports is the constant it was built with, at any canvas.
        assert_eq!(band.row_w(854.0), 340.0);
        assert_eq!(band.row_w(640.0), 340.0);
    }

    /// **Why `RowBand::Inset` had to exist, as a measurement rather than a claim.**
    ///
    /// `social.rs` carried a doc comment asserting that no constant `row_w` makes a
    /// centred row's right edge land in that screen's right margin at every canvas
    /// width. That is the reason issue #445 listed the screen as blocked, and it
    /// was prose. This is the arithmetic.
    ///
    /// Social's own geometry: the name sits at a flat `NAME_LEFT_INSET` (4) from
    /// the left edge and `report_button_x` is `width - RIGHT_MARGIN - REPORT_
    /// BUTTON_W`, so the row's content ends `RIGHT_MARGIN` (10) from the right
    /// edge. **Two hypotheses**, both computed from outside this type:
    ///
    /// * `Inset { left: 4, right: 10 }` — the right edge tracks the canvas 1:1.
    /// * `Centred { row_w }` for the `row_w` that is *exactly right at 854 px* —
    ///   the most favourable constant a tuner could have picked.
    ///
    /// The centred hypothesis must then be wrong by a large, growing margin at
    /// other widths, because a centred row's right edge moves at **half** the rate
    /// the canvas edge does. Half the width delta, exactly — that is the
    /// prediction, not "it differs".
    #[test]
    fn no_constant_row_width_can_express_a_full_width_row() {
        const NAME_LEFT_INSET: f32 = 4.0;
        const RIGHT_MARGIN: f32 = 10.0;

        let inset = RowBand::Inset {
            left: NAME_LEFT_INSET,
            right: RIGHT_MARGIN,
        };
        // The premise: the inset band really does put the right edge in the margin
        // at every width, which is what the screen's own `report_button_x` needs.
        for w in [640.0_f32, 854.0, 1280.0, 1920.0] {
            assert_eq!(
                inset.row_right(w),
                w - RIGHT_MARGIN,
                "an inset row's right edge must track the canvas at {w} px"
            );
            assert_eq!(inset.row_left(w), NAME_LEFT_INSET);
            assert_eq!(inset.row_w(w), w - RIGHT_MARGIN - NAME_LEFT_INSET);
        }

        // The best possible centred constant: tuned so `row_right` is exact at 854.
        // floor(427) - floor(row_w/2) + row_w == 844  =>  row_w == 834 gives
        // 427 - 417 + 834 = 844. Exact.
        let tuned_w = 834.0;
        let centred = RowBand::Centred { row_w: tuned_w };
        assert_eq!(
            centred.row_right(854.0),
            854.0 - RIGHT_MARGIN,
            "premise: the centred hypothesis is tuned to be EXACT at 854 px, so \
             what follows is not a straw man"
        );

        // And now the two hypotheses separate, by exactly half the width delta.
        for w in [640.0_f32, 1280.0, 1920.0] {
            let want = inset.row_right(w);
            let got = centred.row_right(w);
            // A centred right edge is `w/2 + row_w/2`; the wanted edge is
            // `w - RIGHT_MARGIN`. Subtracting, the error is `(854 - w) / 2` for a
            // constant tuned to zero at 854 — half the width delta, opposite sign,
            // because the centred edge moves at half the canvas edge's rate.
            let predicted_error = (854.0 - w) / 2.0;
            assert!(
                (got - want - predicted_error).abs() <= 1.0,
                "at {w} px the tuned centred row's right edge is off by {}, and \
                 the prediction is half the width delta ({predicted_error}) — a \
                 centred edge moves at half the rate a canvas edge does",
                got - want
            );
            assert!(
                (got - want).abs() > 100.0,
                "and the error at {w} px is {} px, far past anything a constant \
                 could absorb: this is the wrong shape, not a mistuned value",
                (got - want).abs()
            );
        }
    }

    /// **The gutter rule, as a gate.** `AbstractSelectionList` overrides
    /// `scrollBarX()` to `getRowRight() + scrollbarWidth() + 2`, so the bar lives
    /// *outside* the row and nothing clamps it to the canvas. An `Inset` right edge
    /// that only reserves the row's own margin therefore pushes the bar off the
    /// screen — silently, since a rect off the canvas simply does not draw.
    ///
    /// This is the trap a screen adopting `RowBand::Inset` walks into, so it is
    /// stated in executable form: the required reservation is
    /// `SCROLLBAR_WIDTH + 2 + SCROLLBAR_WIDTH == 14`.
    ///
    /// The **control** is the first half: social's own `RIGHT_MARGIN` of 10 is
    /// observed to fail, which is what makes the 14 a measurement rather than a
    /// round number.
    #[test]
    fn an_inset_rows_right_gutter_must_reserve_room_for_the_scrollbar() {
        const CANVAS_W: f32 = 854.0;
        let list = ScrollList::new(20.0, 33.0, 174.0, 40);
        assert!(
            list.scrollable(),
            "premise: 40 rows of 20 px in a 174 px band really does scroll, or \
             `scrollbar_rects` answers `None` and this measures nothing"
        );

        // The control: social's existing 10 px margin is NOT enough, and the bar
        // runs off the right edge.
        let too_tight = RowBand::Inset {
            left: 4.0,
            right: 10.0,
        };
        let bar_right = list.scrollbar_x(too_tight.row_right(CANVAS_W)) + SCROLLBAR_WIDTH;
        assert!(
            bar_right > CANVAS_W,
            "control: a 10 px right inset must be observed to push the bar off \
             the canvas (bar ends at {bar_right}, canvas is {CANVAS_W}) — \
             otherwise the 14 px rule below is an arbitrary number"
        );

        // The rule: 14 px is the smallest inset that fits, and it fits exactly.
        let required = SCROLLBAR_WIDTH + 2.0 + SCROLLBAR_WIDTH;
        assert_eq!(required, 14.0);
        let just_enough = RowBand::Inset {
            left: 4.0,
            right: required,
        };
        assert_eq!(
            list.scrollbar_x(just_enough.row_right(CANVAS_W)) + SCROLLBAR_WIDTH,
            CANVAS_W,
            "a 14 px right inset must land the bar's far edge exactly on the \
             canvas edge — `getRowRight() + 6 + 2` then 6 wide"
        );
        // One pixel less and it overflows, which is what makes 14 the boundary
        // rather than merely sufficient.
        let one_less = RowBand::Inset {
            left: 4.0,
            right: required - 1.0,
        };
        assert!(
            list.scrollbar_x(one_less.row_right(CANVAS_W)) + SCROLLBAR_WIDTH > CANVAS_W,
            "and 13 px must not be enough, or 14 is not the boundary"
        );
    }

    /// `ListSpec::uniform` must still build a **centred** band, so every existing
    /// caller is unchanged by the split, and `spanning` must be the only way to get
    /// the other shape.
    #[test]
    fn uniform_stays_centred_and_spanning_is_opt_in() {
        let spec = ListSpec::uniform(20.0, 33.0, 33.0, 12, 340.0);
        assert_eq!(spec.band, RowBand::Centred { row_w: 340.0 });
        assert_eq!(
            spec.row_left(854.0),
            257.0,
            "the value `accounts_row_left`/`server_row_left` produced before the \
             split"
        );
        let spanning = spec.clone().spanning(4.0, 14.0);
        assert_eq!(
            spanning.band,
            RowBand::Inset {
                left: 4.0,
                right: 14.0
            }
        );
        // Everything else survives the builder untouched — it is one field.
        assert_eq!(
            (spanning.row_h, spanning.top, spanning.footer_h, spanning.len),
            (spec.row_h, spec.top, spec.footer_h, spec.len)
        );
        // And the two really do disagree, so the builder is not a no-op.
        assert_ne!(spanning.row_right(854.0), spec.row_right(854.0));
    }
}
