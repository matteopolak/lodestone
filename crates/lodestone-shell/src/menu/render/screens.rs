//! The simple screens' frame builders: the title screen's corner strings,
//! [`pause_frame`], [`death_frame`], [`command_block_frame`], and the
//! `DisconnectedScreen` and credits screens with their metrics.
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use super::*;

/// Vanilla's `title.credits` string (`en_us.json`), drawn bottom-right on the
/// title screen exactly as `TitleScreen.init` does
/// (`TitleScreen.java:49,110-111,150-160`). It refers to the Mojang GUI assets
/// this screen is drawn with, which are genuinely Mojang's, so it is reproduced
/// verbatim.
pub(super) const COPYRIGHT: &str = "Copyright Mojang AB. Do not distribute!";

/// The bottom-left corner string, vanilla's
/// `"Minecraft " + version.name()` (+ `menu.modded` for a modified client,
/// `TitleScreen.java:314-323`).
///
/// A from-scratch reimplementation is about as "modified" as a client gets, so
/// naming Lodestone and its version here is this line's honest equivalent —
/// claiming to be plain `Minecraft 26.2` would be the dishonest option.
pub(super) fn version_line() -> String {
    format!("Minecraft 26.2 (Lodestone {})", env!("CARGO_PKG_VERSION"))
}

/// Builds the pause menu's overlay frame: vanilla's **nine** widgets at
/// vanilla's rects (see [`pause_slot`] and [`super::nav::PauseButton`]), six of
/// them present-and-disabled, with the highlight tracking
/// [`super::nav::MenuNav::pause_index`].
///
/// Unlike [`frame_for`], this is not gated by [`owns_frame`] and takes no
/// `UiState`/`StatusCache`/`FaviconCache` — the pause menu has no server list
/// or connection status to show, just the nav's own selection. Callers draw it
/// with [`MenuRenderer::render_overlay`], not [`MenuRenderer::render`], every
/// frame the game is paused, over whatever the world/HUD/container passes
/// already drew — see the [`super::Screen::Paused`] doc comment for why that
/// split exists.
#[must_use]
pub fn pause_frame(nav: &super::nav::MenuNav) -> MenuFrame<'static> {
    use super::nav::PAUSE_BUTTONS;
    MenuFrame {
        rows: PAUSE_BUTTONS
            .iter()
            .map(|b| MenuRow {
                label: b.label().to_string(),
                enabled: b.enabled(),
                slot: Some(pause_slot(*b)),
                icon: b.icon(),
                ..Default::default()
            })
            .collect(),
        selected: nav.pause_index(),
        gui_scale: nav.gui_scale(),
        overlay: true,
        vanilla: true,
        // `PauseScreen.init` adds a `StringWidget` with the screen title at
        // y=40 when the pause menu is showing (`PauseScreen.java:87-88`); the
        // title itself is `menu.game` == "Game Menu" (`PauseScreen.java:63,73`).
        labels: vec![MenuLabel {
            text: "Game Menu".to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: PAUSE_TITLE_Y,
            align: Align::Centre,
            colour: LABEL,
            scale: 1.0,
        }],
        ..Default::default()
    }
}

/// The score line's format, vanilla's `deathScreen.score.value` with the
/// value substituted (`DeathScreen.java:38-39`).
const DEATH_SCORE_UNTRACKED: &str = "Score: 0";

/// Builds the death screen's overlay frame (issue #103): vanilla's
/// `DeathScreen` — the title, the server's death message, the score line, and
/// two buttons (Respawn / Title Screen) at vanilla's rects (see
/// [`death_slot`] and [`super::nav::DeathButton`]) — reproduced from
/// `.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/DeathScreen.java`.
///
/// Like [`pause_frame`], not gated by [`owns_frame`]: the world (and, on a
/// live server, the session) keeps rendering and ticking behind it — a dead
/// player is held with no chunk stream until the respawn this screen gates,
/// and this overlay must not itself stop that (see
/// [`super::Screen::Death`]'s doc comment). Callers draw it with
/// [`MenuRenderer::render_overlay`] every frame the death screen is up, and
/// resolve the highlighted row through [`super::nav::MenuNav::death_index`]
/// exactly like [`pause_frame`] does for [`super::nav::MenuNav::pause_index`].
///
/// `message` is the server's own death message
/// (`net::NetUpdate::Death`/`Sim::death_message`, already flattened to plain
/// text) — `None` draws no message line, matching vanilla's own `if
/// (this.causeOfDeath != null)` guard (`DeathScreen.java:122-124`).
///
/// Two simplifications named rather than silently taken:
/// - **No hardcore variant.** This client has no hardcore mode (nothing
///   decodes a client-visible hardcore flag), so the title is always
///   `deathScreen.title` ("You Died!") and the first button is always
///   `deathScreen.respawn` ("Respawn"), never the hardcore
///   `deathScreen.title.hardcore` ("Game Over!") / `deathScreen.spectate`
///   pair — see [`super::nav::DeathButton`].
/// - **The score line is always [`DEATH_SCORE_UNTRACKED`].** Vanilla's score
///   is `LocalPlayer.getScore()`, synced through a `Player`-entity metadata
///   field (`Player.DATA_SCORE_ID`) nothing in this workspace decodes yet.
///   Drawing the vanilla line at the vanilla position with the only value
///   available (0) is the same "present, honestly simplified" choice
///   `docs/main-menu.md`/`docs/pause-menu.md` make for a present-but-disabled
///   button, rather than omitting the line and drawing a screen vanilla would
///   not recognise the shape of.
///
/// The backdrop is [`OVERLAY_BG`] — the same flat dim [`pause_frame`] draws
/// — rather than vanilla's own reddish `fillGradient`
/// (`DeathScreen.java:134-136`): this pipeline's [`Quads::rect`] takes one
/// flat colour with no per-vertex gradient, and reproducing the gradient
/// would mean extending it for one screen. Left for polish, like the
/// panorama/splash-text gaps `docs/main-menu.md` names for the title screen.
#[must_use]
pub fn death_frame(nav: &super::nav::MenuNav, message: Option<&str>) -> MenuFrame<'static> {
    use super::nav::DEATH_BUTTONS;

    let mut labels = vec![
        // `output.defaultParameters(normalParameters.withScale(2.0F))` then
        // drawn at `(middleLine / 2, 30)` (`DeathScreen.java:119-120`) — see
        // `Origin::DeathTitle`'s doc for why that x is `width / 4`, not the
        // screen centre.
        MenuLabel {
            text: "You Died!".to_string(),
            origin: Origin::DeathTitle,
            dx: 0.0,
            dy: 30.0,
            align: Align::Centre,
            colour: LABEL,
            scale: 2.0,
        },
    ];
    if let Some(text) = message
        && !text.is_empty()
    {
        // `output.accept(CENTER, middleLine, 85, this.causeOfDeath)`
        // (`DeathScreen.java:123`) — `middleLine == width / 2`, i.e.
        // `Origin::ScreenTop`, at normal (1.0) scale.
        labels.push(MenuLabel {
            text: text.to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: 85.0,
            align: Align::Centre,
            colour: LABEL,
            scale: 1.0,
        });
    }
    // `output.accept(CENTER, middleLine, 100, this.deathScore)`
    // (`DeathScreen.java:126`) — always drawn, message or not.
    labels.push(MenuLabel {
        text: DEATH_SCORE_UNTRACKED.to_string(),
        origin: Origin::ScreenTop,
        dx: 0.0,
        dy: 100.0,
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    });

    MenuFrame {
        rows: DEATH_BUTTONS
            .iter()
            .map(|b| MenuRow {
                label: b.label().to_string(),
                enabled: true,
                slot: Some(death_slot(*b)),
                ..Default::default()
            })
            .collect(),
        selected: nav.death_index(),
        gui_scale: nav.gui_scale(),
        overlay: true,
        vanilla: true,
        labels,
        ..Default::default()
    }
}

/// Builds the command block edit screen's overlay frame (issue #47): vanilla's
/// `CommandBlockEditScreen` — see [`super::command_block`]'s module doc for
/// the full geometry citation and the two named islands (no tree ever reaches
/// this client yet; nothing yet opens this screen from a real interaction).
///
/// Like [`pause_frame`]/[`death_frame`], not gated by [`owns_frame`]: the
/// world keeps rendering (and, on a live server, ticking) behind it, matching
/// vanilla's own `isInGameUi() == true`
/// (`AbstractCommandBlockEditScreen.java:123-126`).
///
/// `tree` is threaded through purely so this function is testable against a
/// real completion list — every production caller passes `None` today (see
/// [`super::command_block`]'s module doc), and `None` here draws no suggestion
/// popup at all rather than a fabricated one.
#[must_use]
pub fn command_block_frame(
    state: &command_block::CommandBlockState,
    tree: Option<&CommandTree>,
) -> MenuFrame<'static> {
    use command_block::{CommandBlockRow, COMMAND_BLOCK_ROWS, PREVIOUS_OUTPUT_ROW};

    let dim = widget::argb_to_rgba(widget::INACTIVE_MESSAGE_ARGB);
    let mut labels = vec![
        MenuLabel {
            text: command_block::TITLE_TEXT.to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: command_block::TITLE_Y,
            align: Align::Centre,
            colour: LABEL,
            scale: 1.0,
        },
        MenuLabel {
            text: command_block::COMMAND_LABEL_TEXT.to_string(),
            origin: Origin::ScreenTop,
            dx: command_block::COMMAND_LABEL_DX,
            dy: command_block::COMMAND_LABEL_Y,
            align: Align::Left,
            colour: dim,
            scale: 1.0,
        },
    ];
    // Vanilla's own guard is `!previousEdit.getValue().isEmpty()`
    // (`AbstractCommandBlockEditScreen.java:159`), which a freshly
    // `setValue("-")`-ed box always passes — see
    // `CommandBlockState::previous_output_text`'s own doc.
    if !state.previous_output_text().is_empty() {
        labels.push(MenuLabel {
            text: command_block::PREVIOUS_LABEL_TEXT.to_string(),
            origin: Origin::ScreenTop,
            dx: command_block::PREVIOUS_LABEL_DX,
            dy: command_block::PREVIOUS_LABEL_Y,
            align: Align::Left,
            colour: dim,
            scale: 1.0,
        });
    }

    let slot = |dx: f32, dy: f32, w: f32, h: f32| {
        Some(Slot {
            origin: Origin::ScreenTop,
            dx,
            dy,
            w,
            h,
        })
    };

    let mut rows: Vec<MenuRow> = COMMAND_BLOCK_ROWS
        .iter()
        .map(|row| match row {
            CommandBlockRow::Command => MenuRow {
                field: true,
                edit: Some(state.command.clone()),
                slot: slot(
                    command_block::COMMAND_DX,
                    command_block::COMMAND_Y,
                    command_block::COMMAND_W,
                    command_block::COMMAND_H,
                ),
                ..Default::default()
            },
            CommandBlockRow::TrackOutput => MenuRow {
                label: command_block::track_output_label(state.track_output).to_string(),
                enabled: true,
                slot: slot(
                    command_block::OUTPUT_DX,
                    command_block::PREVIOUS_Y,
                    command_block::OUTPUT_W,
                    command_block::OUTPUT_H,
                ),
                ..Default::default()
            },
            CommandBlockRow::Mode => MenuRow {
                label: command_block::mode_label(state.mode).to_string(),
                enabled: true,
                slot: slot(
                    command_block::MODE_DX,
                    command_block::EXTRA_ROW_Y,
                    command_block::EXTRA_ROW_W,
                    command_block::EXTRA_ROW_H,
                ),
                ..Default::default()
            },
            CommandBlockRow::Conditional => MenuRow {
                label: command_block::conditional_label(state.conditional).to_string(),
                enabled: true,
                slot: slot(
                    command_block::CONDITIONAL_DX,
                    command_block::EXTRA_ROW_Y,
                    command_block::EXTRA_ROW_W,
                    command_block::EXTRA_ROW_H,
                ),
                ..Default::default()
            },
            CommandBlockRow::Automatic => MenuRow {
                label: command_block::automatic_label(state.automatic).to_string(),
                enabled: true,
                slot: slot(
                    command_block::AUTOEXEC_DX,
                    command_block::EXTRA_ROW_Y,
                    command_block::EXTRA_ROW_W,
                    command_block::EXTRA_ROW_H,
                ),
                ..Default::default()
            },
            CommandBlockRow::Done => MenuRow {
                label: "Done".to_string(),
                enabled: true,
                slot: Some(Slot {
                    origin: Origin::CommandBlockFooter,
                    dx: command_block::DONE_DX,
                    dy: 0.0,
                    w: command_block::FOOTER_W,
                    h: command_block::FOOTER_H,
                }),
                ..Default::default()
            },
            CommandBlockRow::Cancel => MenuRow {
                label: "Cancel".to_string(),
                enabled: true,
                slot: Some(Slot {
                    origin: Origin::CommandBlockFooter,
                    dx: command_block::CANCEL_DX,
                    dy: 0.0,
                    w: command_block::FOOTER_W,
                    h: command_block::FOOTER_H,
                }),
                ..Default::default()
            },
        })
        .collect();
    debug_assert_eq!(rows.len(), PREVIOUS_OUTPUT_ROW);

    // Row 7: the read-only previous-output field — never a click target, see
    // `command_block`'s module doc on why it takes no keyboard focus either.
    let mut previous = EditBox::new(
        0.0,
        0.0,
        command_block::PREVIOUS_W,
        command_block::PREVIOUS_H,
        "Previous Output",
    );
    previous.is_editable = false;
    previous.set_value(state.previous_output_text());
    rows.push(MenuRow {
        field: true,
        edit: Some(previous),
        slot: slot(
            command_block::PREVIOUS_DX,
            command_block::PREVIOUS_Y,
            command_block::PREVIOUS_W,
            command_block::PREVIOUS_H,
        ),
        ..Default::default()
    });

    // The suggestion popup (vanilla's `CommandSuggestions.SuggestionsList`) —
    // appended past every real control, so its row indices never collide with
    // `COMMAND_BLOCK_ROWS`'s. Only ever non-empty in a test today; see this
    // function's own doc on why `tree` is always `None` in production.
    if let Completion::Local { start, candidates } = state.completions(tree) {
        let popup_w = candidates
            .iter()
            .map(|c| state.command.measure(&c.text))
            .fold(0.0_f32, f32::max);
        // `getScreenX(range.start)`: the box's own text-x plus the fixed
        // advance of everything before `start` — `displayPos` is ignored,
        // matching a short, unscrolled command (see `command_block`'s module
        // doc on the fixed-advance approximation `EditBox` already makes
        // everywhere).
        let unclamped_dx = command_block::COMMAND_DX
            + edit_box::BORDER_INSET
            + state.command.advance * start as f32;
        for (i, candidate) in candidates.iter().enumerate() {
            rows.push(MenuRow {
                label: candidate.text.clone(),
                enabled: true,
                slot: Some(Slot {
                    origin: Origin::CommandBlockSuggestion {
                        dx: unclamped_dx,
                        popup_w,
                    },
                    dx: 0.0,
                    dy: 12.0 * i as f32,
                    w: popup_w + 1.0,
                    h: 12.0,
                }),
                ..Default::default()
            });
        }
    }

    MenuFrame {
        rows,
        selected: usize::MAX,
        hovered: state.hovered,
        overlay: true,
        vanilla: true,
        labels,
        ..Default::default()
    }
}

// -- vanilla's `DisconnectedScreen` metrics -----------------------------------

/// `Button.builder(…).width(200)`, every call site
/// (`DisconnectedScreen.java:52,57,61,63`) — not [`widget::DEFAULT_WIDTH`]'s
/// 150.
const ERROR_BUTTON_W: f32 = 200.0;
/// Room reserved above the bottom edge for the one button this screen draws:
/// [`WIDGET_H`] plus a margin roughly matching vanilla's `padding(2)` between
/// stack children (`DisconnectedScreen.java:47`) plus some slack so the
/// button never crowds the edge on a small canvas.
const ERROR_BUTTON_BOTTOM_MARGIN: f32 = WIDGET_H + 20.0;
/// Where the title sits, from [`Origin::ScreenTop`].
///
/// Vanilla has no fixed y here — the whole stack is centred vertically by
/// `FrameLayout.centerInRectangle` (`:73-75`), which needs the reason text's
/// *wrapped line count* to size the stack, a draw-time fact `frame_for`
/// cannot see (it runs before the canvas is known — see [`Slot`]'s docs).
/// This anchors the title near the top instead, the same trade
/// [`accounts_failed_frame`] already makes for an identically-shaped screen.
const ERROR_TITLE_Y: f32 = 40.0;
/// The wrap column the reason text is bounded to.
///
/// Vanilla bounds its `MultiLineTextWidget` to `this.width - 50`
/// (`DisconnectedScreen.java:46`), which is canvas-*dependent* and therefore
/// not expressible as a fixed [`MenuNotice::w`] (the same reason
/// [`ACCOUNTS_ROW_W`] is fixed rather than derived per-canvas). Sized off
/// [`crate::config::MIN_SCALED_WIDTH`] so it is correct even at the smallest
/// canvas `calculate_gui_scale` can produce — the same conservative-at-minimum
/// trade [`super::options::LIST_WINDOW_PX`] makes vertically.
const ERROR_NOTICE_W: f32 = crate::config::MIN_SCALED_WIDTH as f32 - 50.0;

/// Builds vanilla's `DisconnectedScreen` (issue #392's framework epic — this
/// screen was still the pre-framework centred row stack, with no [`Slot`] on
/// its row and no wrapped-text bound on its reason, until now):
/// title, the disconnect reason wrapped and bounded exactly like
/// [`accounts_failed_frame`]'s failure message, and one real button
/// (`.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/DisconnectedScreen.java:42-70`).
///
/// **Two vanilla widgets are never built here.** The `gui.report_to_server`
/// and `gui.open_report_dir` buttons only appear when a `DisconnectionDetails`
/// carries a bug-report link or a saved crash report (`:48-58`); nothing in
/// this workspace produces either, so their absence is "present only when
/// vanilla would show it", not a missing row — the same rule the
/// multiplayer-screen footer's `Direct Connection` button already follows in
/// the other direction (present, but inactive).
///
/// **The button's label is vanilla's `gui.toTitle`** ("Back to Title
/// Screen"), not the `gui.toMenu` default ("Back to Server List") a
/// `DisconnectedScreen` shows when `allowsMultiplayer()` is true
/// (`:59-64`). [`super::UiState::dismiss_error`] always returns to
/// [`super::Screen::MainMenu`], never to a server list — that is vanilla's
/// `!allowsMultiplayer()` branch, reproduced honestly, rather than a label
/// that promises a screen this client does not return to.
///
/// **The title is `disconnect.lost`** ("Connection Lost"), vanilla's own
/// title for `ClientPacketListener.onDisconnect`'s ordinary mid-session
/// disconnect — the case [`super::UiState::session_failed`] models most often.
/// A failed *initial* connection attempt is titled `connect.failed` in
/// vanilla instead; this client has one generic error screen for both causes,
/// so one title has to be picked, and the mid-session one is both the more
/// common path and the truthful one when there was a session to lose.
///
/// `shouldCloseOnEsc()` is `false` in vanilla (`:82-85`) — Escape does
/// **not** dismiss this screen there, so a misclick cannot swallow a network
/// error before it is read. This client's Escape *does* dismiss it (see
/// `nav::MenuNav`'s `Screen::Error` arm), which is a pre-existing, separately
/// tested behaviour this pass does not change — this function is layout, not
/// input semantics.
#[must_use]
pub(super) fn error_frame(reason: Option<&str>) -> MenuFrame<'static> {
    MenuFrame {
        rows: vec![MenuRow {
            label: "Back to Title Screen".to_string(),
            enabled: true,
            slot: Some(Slot {
                origin: Origin::ScreenBottom,
                dx: -(ERROR_BUTTON_W * 0.5),
                dy: -ERROR_BUTTON_BOTTOM_MARGIN,
                w: ERROR_BUTTON_W,
                h: WIDGET_H,
            }),
            ..Default::default()
        }],
        selected: 0,
        vanilla: true,
        labels: vec![MenuLabel {
            text: "Connection Lost".to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: ERROR_TITLE_Y,
            align: Align::Centre,
            colour: LABEL,
            scale: 1.0,
        }],
        // `reason.is_empty()` never happens in production (`session_failed`
        // always carries a real message), but an empty notice would still
        // draw zero lines correctly — no special-casing needed, unlike
        // `death_frame`'s optional message.
        notice: reason.map(|text| MenuNotice {
            text: text.to_string(),
            origin: Origin::ScreenTop,
            dx: -(ERROR_NOTICE_W * 0.5),
            dy: ERROR_TITLE_Y + LINE_H * 3.0,
            w: ERROR_NOTICE_W,
            bottom: ERROR_BUTTON_BOTTOM_MARGIN + WIDGET_H,
            colour: FG_BAD,
        }),
        ..Default::default()
    }
}

// -- the credits/end-poem screen (issue #192) ---------------------------------
//
// **Not vanilla geometry.** `WinScreen.java` draws no widgets at all: it is a
// full-height scrolling column of text (the end poem, then a real Mojang
// employee credits roll) advanced by an elapsed-time tick every frame, with
// **any** keypress skipping straight to the end of the scroll
// (`WinScreen.java`'s own `keyPressed`/`mouseClicked` overrides). Two things
// rule out a faithful port here rather than a scope cut:
//
// 1. **No time source reaches this pipeline.** [`frame_for`] is a pure
//    function of [`super::UiState`]/[`super::nav::MenuNav`] with no elapsed-time
//    parameter — every other timed effect in this menu (`panorama.rs`'s
//    background) is advanced from *outside* this frame-building code, by
//    whatever owns the render loop each real frame. Wiring a tick in here
//    would be a `MenuNav` field plus an `app.rs` call every frame, the same
//    shape as the queued patches this batch of work already defers — and it
//    buys nothing without point 2.
// 2. **The content itself is not this project's to reproduce.** The real end
//    poem is Julian Gough's text, commissioned by Mojang, and the real
//    credits roll names actual Mojang employees — reproducing either here
//    would be copying a copyrighted creative work wholesale in one case and
//    fabricating attribution to real people in the other (this project's own
//    contributors did not write Mojang's game). `version_line`'s "Minecraft
//    26.2 (Lodestone …)" a few lines up in this file already drew this same
//    line once: naming this project rather than borrowing vanilla's.
//
// So this screen is a short, honestly-Lodestone-authored placeholder: it
// proves the screen/session-teardown mechanism (issue #192's own scope is
// "the scrolling text screen itself" plus "the trigger", and the trigger is
// out of this crate's ownership for this batch — see [`super::Screen::Credits`]'s
// doc) without inventing scroll geometry that has no elapsed-time input to
// drive it, or copying text that is not this project's to copy. If a real
// jar-asset extraction pipeline for `texts/end.txt`-equivalent content ever
// lands (see `docs/ui-framework.md`'s asset-sourcing precedent for textures/
// sounds/lang, all loaded from the user's own legitimately-owned files rather
// than transliterated into source), this is the function to point at it —
// nothing about [`super::Screen::Credits`]'s wiring below depends on the text
// being a placeholder.
const CREDITS_BUTTON_W: f32 = 200.0;
const CREDITS_BUTTON_BOTTOM_MARGIN: f32 = WIDGET_H + 20.0;
const CREDITS_TITLE_Y: f32 = 40.0;
const CREDITS_NOTICE_W: f32 = crate::config::MIN_SCALED_WIDTH as f32 - 50.0;

/// `gui.stats`-style short line — not a vanilla string (there is no vanilla
/// equivalent that fits this screen's honest scope), see the module doc above.
const CREDITS_TITLE: &str = "The End?";
const CREDITS_BODY: &str = "Thanks for playing Lodestone.\n\nThis screen stands in for vanilla's end poem and credits roll, which this project does not reproduce (see docs/ui-framework.md).";

/// Builds the credits/end-poem frame. Same shape as [`error_frame`]: one
/// full-width Done button anchored from [`Origin::ScreenBottom`], a title at
/// [`Origin::ScreenTop`], a wrapped body via [`MenuNotice`].
#[must_use]
pub(super) fn credits_frame() -> MenuFrame<'static> {
    MenuFrame {
        rows: vec![MenuRow {
            label: "Done".to_string(),
            enabled: true,
            slot: Some(Slot {
                origin: Origin::ScreenBottom,
                dx: -(CREDITS_BUTTON_W * 0.5),
                dy: -CREDITS_BUTTON_BOTTOM_MARGIN,
                w: CREDITS_BUTTON_W,
                h: WIDGET_H,
            }),
            ..Default::default()
        }],
        selected: 0,
        vanilla: true,
        labels: vec![MenuLabel {
            text: CREDITS_TITLE.to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: CREDITS_TITLE_Y,
            align: Align::Centre,
            colour: LABEL,
            scale: 1.0,
        }],
        notice: Some(MenuNotice {
            text: CREDITS_BODY.to_string(),
            origin: Origin::ScreenTop,
            dx: -(CREDITS_NOTICE_W * 0.5),
            dy: CREDITS_TITLE_Y + LINE_H * 3.0,
            w: CREDITS_NOTICE_W,
            bottom: CREDITS_BUTTON_BOTTOM_MARGIN + WIDGET_H,
            colour: LABEL,
        }),
        ..Default::default()
    }
}

