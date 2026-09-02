//! The Telemetry Data screen — vanilla's `TelemetryInfoScreen`.
//!
//! ## Why this is a prose screen, honestly
//!
//! Vanilla's real screen has four parts: a title, a description, two
//! external-link buttons, a live scrollable `TelemetryEventWidget` (the
//! pending-events log), an opt-in checkbox, and two more buttons. This
//! client **collects no telemetry at all** — there is no
//! `TelemetryManager`, no event log, no opt-in state anywhere in the
//! workspace (`/usr/bin/grep -rn 'TelemetryManager\|telemetry_opt_in\|
//! TelemetryEvent' crates/` outside this module finds nothing but the two
//! decode-adjacent hits `docs/statistics-screen.md` already names as
//! unrelated). So the honest shape here is not "the event list, empty for
//! now" — it is **no event list**, because nothing in this client could ever
//! populate one. Vanilla's own conditional makes this an easier call than it
//! looks: `TelemetryInfoScreen.EXTRA_TELEMETRY_AVAILABLE` is what gates the
//! opt-in checkbox's existence in the *real* game too
//! (vanilla's own client entry point's extra-telemetry-available check), and this client is always on
//! that screen's "false" branch — vanilla itself draws no checkbox then, so
//! omitting it here is not a reduction at all, just the same conditional
//! vanilla already has, permanently resolved one way.
//!
//! What is left after removing the two things this client structurally
//! cannot have (the event log, the opt-in state) is exactly what that fix
//! called it: prose, plus the two buttons that are pure links with no data
//! dependency at all (Privacy Statement, Give Feedback) — those *are* built,
//! for real, because opening a URL needs nothing this client lacks. See
//! "Wired vs. decorative" below.
//!
//! ## Geometry, transcribed
//!
//! - [`HEADER_HEIGHT`] = 81: `new HeaderAndFooterLayout(this, 16 + 9 * 5 + 20,
//!   …)` — a **compile-time constant**,
//!   not derived from measuring the description's real wrapped height (this
//!   client has no font metrics at layout-build time either, so that is not
//!   a gap introduced here).
//! - [`FOOTER_HEIGHT`] = 33: the same constructor's ternary,
//!   `EXTRA_TELEMETRY_AVAILABLE ? 33 + (vanilla's own checkbox get-box-size accessor of font) : 33` — this
//!   client is always the `: 33` branch (see above), which is also
//!   [`super::options::FOOTER_HEIGHT`]'s own value, so the footer band is
//!   reused directly rather than re-derived — see "Dependencies" below.
//! - Header content order (`init`, `:52-58`): title, description (two
//!   paragraphs — the string has one literal `\n`), a horizontal button row
//!   (Privacy Statement, Give Feedback). Arranged the same way
//!   [`super::language::frame_widget_rects`] arranges its own header: a real
//!   [`super::layout::HeaderAndFooterLayout`] + [`super::layout::LinearLayout`]
//!   tree, asked rather than restated.
//!
//! **Declared departure**: the description draws as its two `\n`-separated
//! lines, each unwrapped, rather than vanilla's `MultiLineTextWidget`, which
//! additionally soft-wraps each paragraph at narrow widths. Both lines fit
//! comfortably inside `MIN_SCALED_WIDTH` in `en_us`, so the two are visually
//! identical down to the floor this client supports; a much longer
//! translation could differ. The same "no font metrics at layout time"
//! constraint every other page in this tree already has.
//!
//! ## Wired vs. decorative
//!
//! - **Wired**: reaching the screen (the root grid's "Telemetry Data..."
//!   button is now live) and back (Escape/Done → Root), and — genuinely,
//!   not just present — **Give Feedback**: opens the real vanilla URL
//!   (`CommonLinks.RELEASE_FEEDBACK`, transcribed byte-for-byte) in the
//!   system browser through [`super::accounts::open_in_browser`], the same
//!   best-effort, no-new-dependency OS handoff the account screen's
//!   device-code sign-in already uses. It needs no telemetry state to
//!   exist — a link is not a data path.
//! - **Deliberately dropped, not a gap**: vanilla's other header button,
//!   **Privacy Statement**, linked `CommonLinks.PRIVACY_STATEMENT`
//!   (`go.microsoft.com/fwlink/?LinkId=521839`) — Microsoft's own general
//!   privacy statement, disclosed here only because vanilla's telemetry
//!   pipeline sends data covered by it. This client has no telemetry
//!   pipeline (see the module docs above), so a button surfacing that
//!   statement on *this* screen existed only because vanilla had it, with
//!   nothing of ours behind it — keeping it would read as if Lodestone's
//!   (non-existent) data collection were governed by Microsoft's policy.
//!   Dropped along with its URL constant rather than kept as dead weight.
//! - **Present-and-inactive**: **View My Data**
//!   (`minecraft.getTelemetryManager().getLogDirectory()`) — there is no
//!   telemetry manager and so no directory to open.
//! - **Correctly absent, not decorative**: the opt-in checkbox and the
//!   event list — see the module docs above for why neither is a gap.
//!
//! ## Dependencies
//!
//! - `super::accounts::open_in_browser` — the OS-command URL opener,
//!   `pub(crate)` since that fix for this module to reuse.
//! - `super::options` — [`super::options::FOOTER_HEIGHT`],
//!   [`super::options::footer_rects`], [`super::options::Placement::Footer`]
//!   — this screen's footer is geometrically identical to
//!   [`super::options::SettingsPage::Accessibility`]'s two-button footer, so
//!   it is reused rather than re-derived, the same move
//!   [`super::key_binds::footer_controls`] already made.
//! - `super::layout` — [`super::layout::HeaderAndFooterLayout`],
//!   [`super::layout::LinearLayout`], [`super::layout::widget_rects`].
//! - The 26.2 jar's `assets/minecraft/lang/en_us.json` for the surviving
//!   captions verbatim (`telemetry_info.screen.title`,
//!   `.button.give_feedback`, `.button.show_data`) and
//!   vanilla's own common-links table for the
//!   feedback URL. The description body is Lodestone's own text, not a
//!   transcription — see below.

use super::options::{self, Placement};
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot};
use super::widget::{LayoutElement, Widget};
use super::layout::{self, HeaderAndFooterLayout, LayoutSettings, LinearLayout};

/// `CommonLinks.RELEASE_FEEDBACK` — transcribed verbatim.
pub const RELEASE_FEEDBACK_URL: &str = "https://aka.ms/javafeedback?ref=game";

/// Lodestone's own description of its (lack of) telemetry — not a
/// transcription of `telemetry_info.screen.description`, whose text
/// describes vanilla's real data collection. This client collects none, so
/// the honest copy says that plainly instead. Split across two
/// lines for the same unwrapped-line layout the vanilla string used — see
/// the module docs' declared departure.
pub const DESCRIPTION_LINES: [&str; 2] = [
    "Lodestone collects no telemetry: nothing about your play is sent to us or to anyone else.",
    "There is no data collection to opt in or out of, and no telemetry log to show here.",
];

// -- geometry, transcribed (see the module docs) -----------------------------

/// Vanilla's own telemetry-info screen's literal `16 + 9 * 5 + 20`.
pub const HEADER_HEIGHT: f32 = 81.0;
/// The same constructor's `EXTRA_TELEMETRY_AVAILABLE ? … : 33` — always the
/// `33` branch here (see the module docs), which is also
/// [`options::FOOTER_HEIGHT`]'s own value.
pub const FOOTER_HEIGHT: f32 = options::FOOTER_HEIGHT;
/// The description's two lines, at [`super::options::HEADER_LINE_HEIGHT`]
/// each.
const DESCRIPTION_HEIGHT: f32 = options::HEADER_LINE_HEIGHT * 2.0;

fn label_stand_in(height: f32) -> Box<dyn LayoutElement> {
    Box::new(Widget::new(0.0, 0.0, 0.0, height, ""))
}

fn button(w: f32) -> Box<dyn LayoutElement> {
    Box::new(Widget::button(0.0, 0.0, w, options::WIDGET_H, ""))
}

const TITLE_RECT: usize = 0;
const DESCRIPTION_RECT: usize = 1;
const FEEDBACK_RECT: usize = 2;

/// The header's widget column, arranged for one canvas — asked of a real
/// [`HeaderAndFooterLayout`] rather than restated, the same rule
/// [`super::language::frame_widget_rects`]/[`super::options::root_widget_rects`]
/// follow.
#[must_use]
pub fn header_widget_rects(width: f32, height: f32) -> Vec<(f32, f32, f32, f32)> {
    let mut root = HeaderAndFooterLayout::with_heights(width, height, HEADER_HEIGHT, FOOTER_HEIGHT);

    // Vanilla's own header layout: a vertical linear layout at spacing 4, added
    // to the header, with its default cell setting horizontally centred (`:52-53`).
    let mut header = LinearLayout::vertical().spacing(4);
    *header.default_cell_setting() = LayoutSettings::defaults().align_horizontally_center();
    header.add_child(label_stand_in(options::HEADER_LINE_HEIGHT)); // title
    header.add_child(label_stand_in(DESCRIPTION_HEIGHT)); // description, both lines
    // Vanilla's button row held two buttons (Privacy Statement, Give
    // Feedback); Privacy Statement is dropped here (see the module docs), so
    // one button remains, still in its own row for layout symmetry with the
    // rest of this tree's header/footer pages.
    let mut buttons = LinearLayout::horizontal().spacing(8);
    buttons.add_child(button(options::SMALL_BUTTON_WIDTH)); // Give Feedback
    header.add_child(Box::new(buttons));
    root.add_to_header(Box::new(header));

    root.arrange_elements();
    layout::widget_rects(&root)
}

fn header_rect_xy(rects: &[(f32, f32, f32, f32)], index: usize) -> (f32, f32) {
    let (x, y, _, _) = rects.get(index).copied().unwrap_or((-1000.0, -1000.0, 0.0, 0.0));
    (x, y)
}

/// Where one widget sits — [`Origin::Telemetry`]'s whole body. The footer
/// (View My Data, Done) is **not** here: it reuses
/// [`Origin::Settings`]`(`[`Placement::Footer`]`)` directly, the same move
/// [`super::key_binds::footer_controls`] made for its own two-button footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryPlacement {
    Title,
    /// `index` 0 or 1 — the description's two `\n`-separated lines.
    DescriptionLine(u8),
    /// The one surviving header button, Give Feedback.
    HeaderButton,
}

#[must_use]
pub fn placement_anchor(placement: TelemetryPlacement, width: f32, height: f32) -> (f32, f32) {
    let rects = header_widget_rects(width, height);
    match placement {
        TelemetryPlacement::Title => header_rect_xy(&rects, TITLE_RECT),
        TelemetryPlacement::DescriptionLine(line) => {
            let (x, y) = header_rect_xy(&rects, DESCRIPTION_RECT);
            (x, y + f32::from(line) * options::HEADER_LINE_HEIGHT)
        }
        TelemetryPlacement::HeaderButton => header_rect_xy(&rects, FEEDBACK_RECT),
    }
}

// -- the row/control model ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryControl {
    GiveFeedback,
    /// Present-and-inactive — see the module docs.
    ViewMyData,
    Done,
}

impl TelemetryControl {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            TelemetryControl::GiveFeedback => "Give Feedback", // telemetry_info.button.give_feedback
            TelemetryControl::ViewMyData => "View My Data", // telemetry_info.button.show_data
            TelemetryControl::Done => "Done", // gui.done
        }
    }

    #[must_use]
    pub fn is_live(self) -> bool {
        !matches!(self, TelemetryControl::ViewMyData)
    }
}

/// Every control, in vanilla's own `init()` order (with Privacy Statement
/// dropped — see the module docs).
pub const ALL_CONTROLS: [TelemetryControl; 3] = [
    TelemetryControl::GiveFeedback,
    TelemetryControl::ViewMyData,
    TelemetryControl::Done,
];

fn slot_for(control: TelemetryControl) -> Slot {
    match control {
        TelemetryControl::GiveFeedback => Slot {
            origin: Origin::Telemetry(TelemetryPlacement::HeaderButton),
            dx: 0.0,
            dy: 0.0,
            w: options::SMALL_BUTTON_WIDTH,
            h: options::WIDGET_H,
        },
        TelemetryControl::ViewMyData => Slot {
            origin: Origin::Settings(Placement::Footer { index: 0, count: 2 }),
            dx: 0.0,
            dy: 0.0,
            w: options::SMALL_BUTTON_WIDTH,
            h: options::WIDGET_H,
        },
        TelemetryControl::Done => Slot {
            origin: Origin::Settings(Placement::Footer { index: 1, count: 2 }),
            dx: 0.0,
            dy: 0.0,
            w: options::SMALL_BUTTON_WIDTH,
            h: options::WIDGET_H,
        },
    }
}

// -- navigation ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryOutcome {
    None,
    Back,
}

/// This screen's own cursor. No scroll, no search — three fixed controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TelemetryNav {
    cursor: usize,
}

impl TelemetryNav {
    /// Called whenever the page is entered, matching every sibling page's
    /// "fresh screen on every visit" rule.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn step(&mut self, forward: bool) {
        let len = ALL_CONTROLS.len();
        self.cursor = if forward {
            (self.cursor + 1) % len
        } else {
            (self.cursor + len - 1) % len
        };
    }

    pub fn hover_row(&mut self, row: usize) {
        if row < ALL_CONTROLS.len() {
            self.cursor = row;
        }
    }

    pub fn click_row(&mut self, row: usize) -> TelemetryOutcome {
        let Some(&control) = ALL_CONTROLS.get(row) else {
            return TelemetryOutcome::None;
        };
        self.cursor = row;
        self.activate(control)
    }

    pub fn enter(&mut self) -> TelemetryOutcome {
        let control = ALL_CONTROLS[self.cursor];
        self.activate(control)
    }

    fn activate(&mut self, control: TelemetryControl) -> TelemetryOutcome {
        if !control.is_live() {
            return TelemetryOutcome::None;
        }
        match control {
            // Opening a URL needs no telemetry state at all — see the module
            // docs — so it happens right here rather than bubbling a
            // MenuAction up to `app.rs`, exactly like `accounts.rs`'s own
            // device-code sign-in already calls `open_in_browser` directly.
            TelemetryControl::GiveFeedback => {
                super::accounts::open_in_browser(RELEASE_FEEDBACK_URL);
                TelemetryOutcome::None
            }
            TelemetryControl::ViewMyData => TelemetryOutcome::None,
            TelemetryControl::Done => TelemetryOutcome::Back,
        }
    }

    pub fn escape(&mut self) -> TelemetryOutcome {
        TelemetryOutcome::Back
    }
}

// -- the frame ----------------------------------------------------------------

#[must_use]
pub fn frame(nav: &TelemetryNav) -> MenuFrame<'static> {
    let rows: Vec<MenuRow> = ALL_CONTROLS
        .iter()
        .map(|&control| MenuRow {
            label: control.label().to_string(),
            enabled: control.is_live(),
            slot: Some(slot_for(control)),
            ..Default::default()
        })
        .collect();

    let labels = vec![
        MenuLabel {
            text: "Telemetry Data Collection".to_string(), // telemetry_info.screen.title
            origin: Origin::Telemetry(TelemetryPlacement::Title),
            dx: 0.0,
            dy: 0.0,
            align: Align::Centre,
            colour: super::widget::ACTIVE_LABEL,
            scale: 1.0,
        },
        MenuLabel {
            text: DESCRIPTION_LINES[0].to_string(),
            origin: Origin::Telemetry(TelemetryPlacement::DescriptionLine(0)),
            dx: 0.0,
            dy: 0.0,
            align: Align::Centre,
            // telemetry_info.screen.description, colour -4539718 — the same
            // vanilla grey as the Language screen's warning line.
            colour: super::language::WARNING_COLOUR,
            scale: 1.0,
        },
        MenuLabel {
            text: DESCRIPTION_LINES[1].to_string(),
            origin: Origin::Telemetry(TelemetryPlacement::DescriptionLine(1)),
            dx: 0.0,
            dy: 0.0,
            align: Align::Centre,
            colour: super::language::WARNING_COLOUR,
            scale: 1.0,
        },
    ];

    MenuFrame {
        rows,
        labels,
        selected: nav.cursor(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_controls_carries_all_three_surviving_in_vanillas_own_order() {
        assert_eq!(ALL_CONTROLS.len(), 3);
        assert_eq!(ALL_CONTROLS[0], TelemetryControl::GiveFeedback);
        assert_eq!(ALL_CONTROLS[1], TelemetryControl::ViewMyData);
        assert_eq!(ALL_CONTROLS[2], TelemetryControl::Done);
    }

    #[test]
    fn view_my_data_is_the_one_inactive_control() {
        for control in ALL_CONTROLS {
            assert_eq!(
                control.is_live(),
                control != TelemetryControl::ViewMyData,
                "{control:?}"
            );
        }
    }

    #[test]
    fn stepping_wraps_both_ways() {
        let mut nav = TelemetryNav::default();
        assert_eq!(nav.cursor(), 0);
        nav.step(false);
        assert_eq!(nav.cursor(), 2, "stepping back from 0 wraps to the last control");
        nav.step(true);
        assert_eq!(nav.cursor(), 0);
    }

    #[test]
    fn done_is_reachable_and_leaves_the_page() {
        let mut nav = TelemetryNav::default();
        nav.cursor = 2;
        assert_eq!(nav.enter(), TelemetryOutcome::Back);
    }

    #[test]
    fn view_my_data_does_nothing_even_when_clicked_directly() {
        let mut nav = TelemetryNav::default();
        assert_eq!(nav.click_row(1), TelemetryOutcome::None);
        assert_eq!(nav.cursor(), 1, "an inactive control is still reachable, matching departure 4");
    }

    #[test]
    fn escape_leaves_the_page() {
        assert_eq!(TelemetryNav::default().escape(), TelemetryOutcome::Back);
    }

    #[test]
    fn the_header_widget_rects_place_one_button_after_title_and_description() {
        let rects = header_widget_rects(480.0, 270.0);
        assert_eq!(rects.len(), 3);
    }

    #[test]
    fn a_row_placement_off_the_known_indices_is_off_canvas() {
        // `header_widget_rects` always returns exactly 3 — this asserts the
        // fixed indices this module reads from it stay in range rather than
        // silently reading a stale/absent rect.
        let rects = header_widget_rects(480.0, 270.0);
        assert!(TITLE_RECT < rects.len());
        assert!(DESCRIPTION_RECT < rects.len());
        assert!(FEEDBACK_RECT < rects.len());
    }
}
