//! [`Origin`] and [`Slot`]: the anchor expressions vanilla's `init` methods
//! use, and one widget rect measured from one of them. This is what lets a
//! layout be resolved against a canvas size only known at draw time.
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use super::*;

/// The anchor a [`Slot`] is measured from.
///
/// Vanilla never places a widget at a plain fraction of the canvas, so these are
/// the actual expressions from the two screens' `init` methods rather than
/// normalised alignments. Keeping them as named origins is what lets one `Slot`
/// be resolved against any canvas size — which the layout has to be, because the
/// logical canvas is only known at draw time (see [`logical_canvas`]).
// No longer `Eq`: `Origin::CommandBlockSuggestion` (issue #47) carries two
// `f32`s, which cannot implement `Eq`. Nothing here relied on `Origin: Eq`
// specifically — `Slot`, which wraps an `Origin`, was already `PartialEq`
// only (never `Eq`) before this variant existed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Origin {
    /// `(w / 2, 0)` — the top of the screen, for the logo band and the pause
    /// screen's title. `this.width` is `int` everywhere vanilla anchors off it
    /// (e.g. `this.width / 2 - 100` at `TitleScreen.java:144`), so `w / 2` is
    /// Java integer division — hence the `floor` (issue #401).
    ScreenTop,
    /// `(floor(w / 2), floor(h / 4) + 48)` — vanilla `TitleScreen.init`'s
    /// `topPos` (`TitleScreen.java:113`) for y, and the same `this.width / 2`
    /// as [`Origin::ScreenTop`] for x. Both are Java integer division, hence
    /// both `floor`s (issue #401: only the y one used to be here).
    TitleTop,
    /// The top-left of vanilla `PauseScreen`'s **arranged** `GridLayout`:
    /// `(floor((w - 212) / 2), floor((h - 166) / 4))`.
    ///
    /// That comes from `FrameLayout.alignInRectangle(grid, 0, 0, w, h, 0.5, 0.25)`
    /// (`PauseScreen.java:181`), and since #394 it is *evaluated* rather than
    /// restated: [`layout::align_in_dimension`] applied to
    /// [`pause_grid_size`], which is the arranged
    /// [`GridLayout`](layout::GridLayout)'s own output. The `floor`s in the
    /// formula above are vanilla's truncating `(int)` cast
    /// (`FrameLayout.java:113-116`); the two differ only for a canvas narrower
    /// than the grid, which `calculate_gui_scale`'s 320 px floor rules out.
    PauseGrid,
    /// `(0, h)` — bottom-left corner text (the title screen's version string).
    BottomLeft,
    /// `(w, h)` — bottom-right corner text (the copyright line).
    BottomRight,
    /// `(w, 0)` — top-right corner, for the non-vanilla `Accounts` title-screen
    /// button (see [`super::nav::MainButton::Accounts`]). Vanilla's own eight
    /// widgets already fill a 320×240 canvas (`config::MIN_SCALED_*`, the real
    /// floor `calculate_gui_scale` can produce) to within 16 px, so a ninth
    /// row appended below them does not fit at the minimum window size —
    /// measured, not assumed: `every_vanilla_widget_is_on_screen_and_none_overlap`
    /// caught it the first time this button was placed as `full(TITLE_PITCH * 5.0)`.
    /// The gap above the logo (`y < LOGO_Y`, i.e. `y < 30`) is free at every
    /// canvas size instead, which is where this corner sits.
    TopRight,
    /// `(floor(w / 2), h)` — bottom-centre, for the footer band of the account
    /// screen (Add Account / Select / Remove / Back) and the multiplayer
    /// screen's seven. Not vanilla-sourced like the others above: nothing in
    /// `TitleScreen`/`PauseScreen` anchors a widget row to the bottom edge. Since
    /// #396 it is where both `HeaderAndFooterLayout` footers are pinned, which is
    /// canvas-independent even though the arranged rects are not — see
    /// [`ACCOUNTS_REF_CANVAS`]. `floor`ed for the same reason as
    /// [`Origin::ScreenTop`] (issue #401): every consumer of this origin is a
    /// `Slot` centred *about* this x, and an unfloored anchor at an odd width
    /// puts that centring a half-pixel off whole, which blurs the text drawn
    /// there.
    ScreenBottom,
    /// `(floor(w / 4), 0)` — the death screen's title anchor (issue #103).
    /// `DeathScreen.visitText` draws it at `middleLine / 2` where
    /// `middleLine = this.width / 2` (`DeathScreen.java:118-120`), i.e.
    /// **centred on the screen's left quarter, not the middle** — this is
    /// vanilla's own layout (seemingly an oversight nobody ever fixed, not a
    /// deliberate design), reproduced faithfully rather than "corrected" to
    /// [`Origin::ScreenTop`]. Both are Java integer division —
    /// `floor(floor(w/2)/2) == floor(w/4)` for a non-negative `w`, so the two
    /// chained truncations collapse to the one `floor` here — and #401's audit
    /// of every unfloored `Origin::anchor` term caught this arm too, alongside
    /// [`Origin::ScreenTop`]/[`Origin::TitleTop`]/[`Origin::ScreenBottom`].
    DeathTitle,
    /// A widget of the settings tree (issue #55), resolved by
    /// [`super::options::placement_anchor`].
    ///
    /// The only [`Origin`] that carries data, and it has to: a settings row's
    /// position depends on the page, the entry, **and how far the list is
    /// scrolled**, none of which anything downstream of [`frame_for`] knows —
    /// this enum is precisely the seam where a canvas-dependent term gets to
    /// live, and the scroll rides along with it. The three shapes it covers are
    /// `OptionsScreen`'s arranged `HeaderAndFooterLayout`, an
    /// `OptionsSubScreen`'s footer band, and an `OptionsList` row; see
    /// [`super::options::Placement`].
    Settings(super::options::Placement),
    /// A widget of the Key Binds screen (issue #15), resolved by
    /// [`super::key_binds::placement_anchor`].
    ///
    /// A second data-carrying variant for the same reason
    /// [`Origin::Settings`] is one: a row's position depends on which action
    /// it is and how far the list is scrolled. Not folded into
    /// [`Origin::Settings`]/[`super::options::Placement`] — see
    /// [`super::key_binds`]'s module docs on why this screen's list geometry
    /// (a flat 20 px row height, two right-anchored buttons per row) does not
    /// fit `OptionsList`'s shape.
    KeyBinds(super::key_binds::KeyPlacement),
    /// A widget of the Social Interactions screen (issue #189), resolved by
    /// [`super::social::placement_anchor`]. A third data-carrying variant for
    /// the same reason [`Origin::KeyBinds`] is one: this screen's rows are not
    /// `OptionsList` geometry either (a name label plus two right-anchored
    /// buttons, not `OptionsList`'s two-column captions).
    Social(super::social::SocialPlacement),
    /// A widget of the Language screen (issue #415), resolved by
    /// [`super::language::placement_anchor`]. A fourth data-carrying variant
    /// for the same reason [`Origin::Social`] is one — this screen's rows are
    /// a third geometry entirely (a single centred line per row, not
    /// `OptionsList`'s or `KeyBindsList`'s shapes).
    Language(super::language::LanguagePlacement),
    /// A widget of the Telemetry screen's **header** (issue #415), resolved
    /// by [`super::telemetry::placement_anchor`]. The footer reuses
    /// [`Origin::Settings`]`(`[`super::options::Placement::Footer`]`)`
    /// directly instead of a fifth variant — see
    /// [`super::telemetry::TelemetryPlacement`]'s own doc.
    Telemetry(super::telemetry::TelemetryPlacement),
    /// A widget of the Resource Packs screen (issue #415), resolved by
    /// [`super::packs::placement_anchor`]. The footer reuses
    /// [`Origin::Settings`]`(`[`super::options::Placement::Footer`]`)`
    /// directly, same as [`Origin::Telemetry`].
    Packs(super::packs::PacksPlacement),
    /// The command block edit screen's Done/Cancel row (issue #47):
    /// `(floor(w/2), floor(h/4) + 132)` —
    /// `AbstractCommandBlockEditScreen.java:71,74`'s
    /// `this.height / 4 + 120 + 12` for `y`, the same `width/2` x-anchor as
    /// every other widget on that screen ([`Origin::ScreenTop`]). Not folded
    /// into [`Origin::TitleTop`] (`floor(h/4) + 48`): the two screens' extra
    /// offsets (`0` vs `+84`) are unrelated constants that happen to share a
    /// `floor(h/4)` term, and giving `TitleTop` a second use would make a
    /// future change to the title screen's `+48` silently move this screen's
    /// buttons too.
    CommandBlockFooter,
    /// One row of the command block screen's tab-completion popup (issue
    /// #47): vanilla's `CommandSuggestions.SuggestionsList` — see
    /// [`command_block_frame`]'s own doc for why `dx`/`popup_w` are computed
    /// there rather than carried as a fixed offset like every other row on
    /// this screen.
    ///
    /// `dx` is the **unclamped** desired offset from [`Origin::ScreenTop`]'s
    /// anchor (the command box's own `text_x`, plus the fixed advance of
    /// everything before the completed word); `popup_w` is the widest
    /// candidate's measured width. Both are needed to reproduce vanilla's own
    /// clamp (`CommandSuggestions.showSuggestions`: `Mth.clamp(x, 0,
    /// input.getScreenX(0) + innerWidth - maxSuggestionWidth)`), which is an
    /// **absolute-screen** bound this variant's `anchor` is the only place
    /// that knows `width` in order to express.
    CommandBlockSuggestion {
        /// See this variant's own doc.
        dx: f32,
        /// See this variant's own doc.
        popup_w: f32,
    },
}

impl Origin {
    /// The anchor point in logical pixels for a canvas of `width`×`height`.
    #[must_use]
    pub fn anchor(self, width: f32, height: f32) -> (f32, f32) {
        match self {
            Origin::ScreenTop => ((width * 0.5).floor(), 0.0),
            Origin::TitleTop => ((width * 0.5).floor(), (height / 4.0).floor() + 48.0),
            Origin::PauseGrid => {
                let (grid_w, grid_h) = pause_grid_size();
                (
                    layout::align_in_dimension(0.0, width, grid_w, 0.5),
                    layout::align_in_dimension(0.0, height, grid_h, 0.25),
                )
            }
            Origin::BottomLeft => (0.0, height),
            Origin::BottomRight => (width, height),
            Origin::TopRight => (width, 0.0),
            Origin::ScreenBottom => ((width * 0.5).floor(), height),
            Origin::DeathTitle => ((width * 0.25).floor(), 0.0),
            // Unlike every arm above, this one *runs a layout* rather than
            // evaluating an expression — `OptionsScreen`'s tree cannot be
            // arranged once per process the way `pause_block` is, because
            // `HeaderAndFooterLayout` places its content band from the canvas
            // height. See `super::options::root_widget_rects`.
            Origin::Settings(placement) => {
                super::options::placement_anchor(placement, width, height)
            }
            Origin::KeyBinds(placement) => {
                super::key_binds::placement_anchor(placement, width, height)
            }
            Origin::Social(placement) => super::social::placement_anchor(placement, width, height),
            Origin::Language(placement) => {
                super::language::placement_anchor(placement, width, height)
            }
            Origin::Telemetry(placement) => {
                super::telemetry::placement_anchor(placement, width, height)
            }
            Origin::Packs(placement) => super::packs::placement_anchor(placement, width, height),
            Origin::CommandBlockFooter => {
                ((width * 0.5).floor(), (height / 4.0).floor() + 132.0)
            }
            Origin::CommandBlockSuggestion { dx, popup_w } => {
                let cx = (width * 0.5).floor();
                // `input.getScreenX(0) + innerWidth - maxSuggestionWidth`:
                // the command box's left text edge (`cx + COMMAND_DX +
                // BORDER_INSET`) plus its inner width (`COMMAND_W - 2 *
                // BORDER_INSET`), minus the popup's own width. `.max(0.0)`
                // guards the same inverted-clamp case vanilla's `Mth.clamp`
                // handles by construction and `f32::clamp` panics on.
                let upper = (cx
                    + super::command_block::COMMAND_DX
                    + super::edit_box::BORDER_INSET
                    + (super::command_block::COMMAND_W - 2.0 * super::edit_box::BORDER_INSET)
                    - popup_w)
                    .max(0.0);
                (
                    (cx + dx).clamp(0.0, upper),
                    // `y - (bordered ? 1 : 0)`, `y == 72` (not anchored to
                    // bottom) — `CommandSuggestions.showSuggestions`/
                    // `SuggestionsList`'s constructor.
                    71.0,
                )
            }
        }
    }
}

/// Where one vanilla-laid-out widget sits: an [`Origin`], an offset from it, and
/// a size. Pure — [`Slot::resolve`] turns it into a pixel rect for a given
/// canvas, and that rect is the **single** definition the renderer, the mouse
/// hover and the click hit-test all read (through [`row_rect`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slot {
    /// The anchor this slot is measured from.
    pub origin: Origin,
    /// Horizontal offset from the anchor, in logical pixels.
    pub dx: f32,
    /// Vertical offset from the anchor, in logical pixels.
    pub dy: f32,
    /// Widget width in logical pixels.
    pub w: f32,
    /// Widget height in logical pixels.
    pub h: f32,
}

impl Slot {
    /// The pixel rect `(x, y, w, h)` for a canvas of `width`×`height`.
    #[must_use]
    pub fn resolve(self, width: f32, height: f32) -> (f32, f32, f32, f32) {
        let (ax, ay) = self.origin.anchor(width, height);
        (ax + self.dx, ay + self.dy, self.w, self.h)
    }
}

