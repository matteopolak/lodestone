//! The simple screens' frame builders: the title screen's corner strings,
//! [`pause_frame`], [`death_frame`], [`command_block_frame`], and the
//! `DisconnectedScreen` and credits screens with their metrics.
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use crate::sim::{SessionEnd, SessionEndKind};
use super::*;

/// The bottom-right corner string on the title screen, occupying vanilla's
/// `title.credits` slot (`TitleScreen.init`, vanilla's own title-screen rendering). Vanilla's own
/// text there — `"Copyright Mojang AB. Do not distribute!"` — is a notice about
/// Mojang's own client assets and, on its face, an instruction not to
/// distribute; neither is true of this project's own build, so it is replaced
/// with Lodestone's own notice rather than reproduced.
pub(super) const COPYRIGHT: &str = "Lodestone — a from-scratch, open-source client";

/// The bottom-left corner string, occupying vanilla's
/// `"Minecraft " + version.name()` slot (+ `menu.modded` for a modified
/// client, vanilla's own title-screen rendering).
///
/// Phrased as Lodestone's own version string, not vanilla's: it leads with
/// this project's name and version, with the protocol version it speaks
/// (`26.2`) kept alongside because that is functionally meaningful — it says
/// which server family this build can join — rather than because it is
/// vanilla's own line.
pub(super) fn version_line() -> String {
    format!("Lodestone {} (protocol: Minecraft 26.2)", env!("CARGO_PKG_VERSION"))
}

/// Builds the pause menu's overlay frame: vanilla's widgets at vanilla's
/// rects (see [`pause_slot`] and [`super::nav::PauseButton`]) — **ten** of
/// them, six present-and-disabled, or **nine** once the hosted world is
/// published and [`super::nav::MenuNav::pause_buttons`] drops
/// [`super::nav::PauseButton::OpenToLan`] (scope 2; see that
/// variant's own doc for why an *omission* rather than a disabled row) — with
/// the highlight tracking [`super::nav::MenuNav::pause_index`].
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
    // The *layout* question — "does the Options row collapse to full width" —
    // not the raw publish flag: see `MenuNav::open_to_lan_available`'s own
    // doc for why a multiplayer session must take this branch too, even
    // though `is_lan_published()` alone cannot tell it apart from an
    // unpublished singleplayer world.
    let published = !nav.open_to_lan_available();
    MenuFrame {
        rows: nav
            .pause_buttons()
            .iter()
            .map(|b| MenuRow {
                label: b.label().to_string(),
                enabled: b.enabled(),
                slot: Some(pause_slot(*b, published)),
                icon: b.icon(),
                ..Default::default()
            })
            .collect(),
        selected: nav.pause_index(),
        gui_scale: nav.gui_scale(),
        backdrop: MenuBackdrop::Dim,
        // Vanilla blurs behind the pause menu — `PauseScreen` never overrides
        // `isInGameUi()`, so `Screen::extractBackground`'s default fork
        // applies (see `blur`'s module doc). Not implied by `backdrop`; see
        // `MenuFrame::blur`'s own doc.
        blur: true,
        vanilla: true,
        // `PauseScreen.init` adds a `StringWidget` with the screen title at
        // y=40 when the pause menu is showing; the
        // title itself is `menu.game` == "Game Menu".
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
/// value substituted.
const DEATH_SCORE_UNTRACKED: &str = "Score: 0";

/// Builds the death screen's overlay frame: vanilla's
/// `DeathScreen` — the title, the server's death message, the score line, and
/// two buttons (Respawn / Title Screen) at vanilla's rects (see
/// [`death_slot`] and [`super::nav::DeathButton`]) — reproduced from
/// vanilla's own death-screen class.
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
/// (this.causeOfDeath != null)` guard.
///
/// Two simplifications named rather than silently taken:
/// - **No hardcore variant.** This client has no hardcore mode (nothing
///   decodes a client-visible hardcore flag), so the title is always
///   `deathScreen.title` ("You Died!") and the first button is always
///   `deathScreen.respawn` ("Respawn"), never the hardcore
///   `deathScreen.title.hardcore` ("Game Over!") / `deathScreen.spectate`
///   pair — see [`super::nav::DeathButton`].
/// - **The score line is always [`DEATH_SCORE_UNTRACKED`].** Vanilla's score
///   is vanilla's own local-player get-score accessor, synced through a `Player`-entity metadata
///   field (`Player.DATA_SCORE_ID`) nothing in this workspace decodes yet.
///   Drawing the vanilla line at the vanilla position with the only value
///   available (0) is the same "present, honestly simplified" choice
///   `docs/main-menu.md`/`docs/pause-menu.md` make for a present-but-disabled
///   button, rather than omitting the line and drawing a screen vanilla would
///   not recognise the shape of.
///
/// The backdrop is [`MenuBackdrop::DeathGradient`], not the flat
/// [`MenuBackdrop::Dim`] [`pause_frame`] draws: vanilla's own
/// `DeathScreen.extractDeathBackground` calls `fillGradient` with two
/// distinct ARGB endpoints, not `Screen`'s flat
/// `extractTransparentBackground`, so the screen reads noticeably redder
/// toward the bottom. See [`super::DEATH_GRADIENT_TOP`]/
/// [`super::DEATH_GRADIENT_BOTTOM`] for the decoded constants and
/// [`Quads::rect_vgradient`] for the per-vertex quad that draws them.
#[must_use]
pub fn death_frame(nav: &super::nav::MenuNav, message: Option<&str>) -> MenuFrame<'static> {
    use super::nav::DEATH_BUTTONS;

    let mut labels = vec![
        // `output.defaultParameters(normalParameters.withScale(2.0F))` then
        // drawn at `(middleLine / 2, 30)` — see
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
        // — `middleLine == width / 2`, i.e.
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
    // — always drawn, message or not.
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
        backdrop: MenuBackdrop::DeathGradient,
        // `DeathScreen` does not override `isInGameUi()` either — same fork
        // as `pause_frame`, see `MenuFrame::blur`'s own doc.
        blur: true,
        vanilla: true,
        labels,
        ..Default::default()
    }
}

/// Builds the command block edit screen's overlay frame: vanilla's
/// `CommandBlockEditScreen` — see [`super::command_block`]'s module doc for
/// the full geometry citation and the two named islands (no tree ever reaches
/// this client yet; nothing yet opens this screen from a real interaction).
///
/// Like [`pause_frame`]/[`death_frame`], not gated by [`owns_frame`]: the
/// world keeps rendering (and, on a live server, ticking) behind it, matching
/// vanilla's own `isInGameUi() == true`.
///
/// `tree` carries the real server's command tree. This used to read "threaded
/// through purely so this function is testable — every production caller passes
/// `None` today", which was true when written and is not now: #470 decodes
/// clientbound `COMMANDS`, #471 routes it to the shell, and `app/redraw.rs`'s
/// overlay block (#474) passes `self.nav.command_tree()` here.
///
/// `None` remains the honest state before a tree arrives — a session that has
/// not received one, or a hermetic test — and draws no suggestion popup at all
/// rather than a fabricated one.
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
    //, which a freshly
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
        backdrop: MenuBackdrop::Dim,
        // `blur` stays at its `..Default::default()` `false`: vanilla's
        // `AbstractCommandBlockEditScreen` overrides `isInGameUi()` to
        // `true`, the fork `Screen::extractBackground` skips the blur for —
        // see `MenuFrame::blur`'s own doc.
        vanilla: true,
        labels,
        ..Default::default()
    }
}

/// Builds the sign-editing screen's overlay frame: [`sign_edit::SignEditState`]'s
/// four line fields plus its Done row. See [`sign_edit`]'s module doc for what
/// this deliberately does not attempt (vanilla's pseudo-3D sign face).
///
/// Row indices match [`super::nav::sign_edit_row`]: the four lines, then
/// [`super::nav::sign_edit_row::DONE`].
pub fn sign_edit_frame(state: &sign_edit::SignEditState) -> MenuFrame<'static> {
    let labels = vec![MenuLabel {
        text: sign_edit::TITLE_TEXT.to_string(),
        origin: Origin::ScreenTop,
        dx: 0.0,
        dy: sign_edit::TITLE_Y,
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    }];

    let mut rows: Vec<MenuRow> = state
        .lines
        .iter()
        .enumerate()
        .map(|(i, line)| MenuRow {
            field: true,
            edit: Some(line.clone()),
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: sign_edit::LINE_DX,
                dy: sign_edit::LINE_START_Y + i as f32 * sign_edit::LINE_SPACING,
                w: sign_edit::LINE_W,
                h: sign_edit::LINE_H,
            }),
            ..Default::default()
        })
        .collect();
    debug_assert_eq!(rows.len(), nav::sign_edit_row::DONE);

    rows.push(MenuRow {
        label: "Done".to_string(),
        enabled: true,
        slot: Some(Slot {
            origin: Origin::ScreenTop,
            dx: sign_edit::DONE_DX,
            dy: sign_edit::DONE_Y,
            w: sign_edit::DONE_W,
            h: sign_edit::DONE_H,
        }),
        ..Default::default()
    });

    MenuFrame {
        rows,
        selected: usize::MAX,
        hovered: state.done_hovered.then_some(nav::sign_edit_row::DONE),
        backdrop: MenuBackdrop::Dim,
        // `blur` stays `false` for `command_block_frame`'s own reason:
        // `AbstractSignEditScreen` overrides `isInGameUi()` to `true` too.
        vanilla: true,
        labels,
        ..Default::default()
    }
}

/// Builds the book-editing screen's overlay frame:
/// [`book_edit::BookEditState`]'s page or title layout, chosen by
/// [`book_edit::BookEditState::signing`]. See [`book_edit`]'s module doc for
/// what this deliberately does not attempt (per-pixel caret placement inside
/// the page, a pseudo-3D book mesh).
///
/// The page's text draws as one [`MenuLabel`] per wrapped visual line —
/// informational only, not part of `rows`' click system, the same split
/// [`command_block_frame`]'s title/labels make. Row indices match
/// [`book_edit::page_row`] while not signing, [`book_edit::sign_row`] while
/// signing — two disjoint tables sharing one `rows` vec, the same shape
/// `Screen::Settings`'s per-page row tables already use.
pub fn book_edit_frame(state: &book_edit::BookEditState) -> MenuFrame<'static> {
    const PAGE_DX: f32 = -60.0;
    const PAGE_Y: f32 = 30.0;
    const PAGE_LINE_H: f32 = 9.0;
    const BOOK_TEXT: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
    const BOOK_AUTHOR: [f32; 4] = [85.0 / 255.0, 85.0 / 255.0, 85.0 / 255.0, 1.0];

    if state.signing {
        let labels = vec![
            MenuLabel {
                text: "Enter a Book Title:".to_string(),
                origin: Origin::ScreenTop,
                dx: 0.0,
                dy: 34.0,
                align: Align::Centre,
                colour: BOOK_TEXT,
                scale: 1.0,
            },
            MenuLabel {
                text: state.author_line(),
                origin: Origin::ScreenTop,
                dx: 0.0,
                dy: 60.0,
                align: Align::Centre,
                colour: BOOK_AUTHOR,
                scale: 1.0,
            },
        ];
        let rows = vec![
            MenuRow {
                field: true,
                edit: Some(state.title.clone()),
                slot: Some(Slot {
                    origin: Origin::ScreenTop,
                    dx: -60.0,
                    dy: 50.0,
                    w: 114.0,
                    h: 20.0,
                }),
                ..Default::default()
            },
            MenuRow {
                label: "Finalize".to_string(),
                enabled: state.can_finalize(),
                slot: Some(Slot {
                    origin: Origin::ScreenTop,
                    dx: -100.0,
                    dy: 196.0,
                    w: 98.0,
                    h: 20.0,
                }),
                ..Default::default()
            },
            MenuRow {
                label: "Cancel".to_string(),
                enabled: true,
                slot: Some(Slot {
                    origin: Origin::ScreenTop,
                    dx: 2.0,
                    dy: 196.0,
                    w: 98.0,
                    h: 20.0,
                }),
                ..Default::default()
            },
        ];
        debug_assert_eq!(rows.len(), book_edit::sign_row::CANCEL + 1);
        return MenuFrame {
            rows,
            selected: usize::MAX,
            hovered: state.hovered,
            backdrop: MenuBackdrop::Dim,
            vanilla: true,
            book_background: true,
            labels,
            ..Default::default()
        };
    }

    let (current, total) = state.page_indicator();
    let mut labels = Vec::new();
    for (i, line) in state.page.lines().iter().enumerate() {
        let text: String = state
            .page
            .value()
            .chars()
            .skip(line.begin)
            .take(line.len())
            .collect();
        labels.push(MenuLabel {
            text,
            origin: Origin::ScreenTop,
            dx: PAGE_DX,
            dy: PAGE_Y + i as f32 * PAGE_LINE_H,
            align: Align::Left,
            colour: BOOK_TEXT,
            scale: 1.0,
        });
    }
    labels.push(MenuLabel {
        text: format!("Page {current} of {total}"),
        origin: Origin::ScreenTop,
        dx: 52.0,
        dy: 18.0,
        align: Align::Right,
        colour: BOOK_TEXT,
        scale: 1.0,
    });

    let footer_y = 159.0;
    let rows = vec![
        MenuRow {
            label: "<".to_string(),
            enabled: current > 1,
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: -53.0,
                dy: footer_y,
                w: 23.0,
                h: 13.0,
            }),
            book_page: Some(BookPageButton::Backward),
            ..Default::default()
        },
        MenuRow {
            label: ">".to_string(),
            enabled: true,
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: 20.0,
                dy: footer_y,
                w: 23.0,
                h: 13.0,
            }),
            book_page: Some(BookPageButton::Forward),
            ..Default::default()
        },
        MenuRow {
            label: "Sign".to_string(),
            enabled: true,
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: -100.0,
                dy: 196.0,
                w: 98.0,
                h: 20.0,
            }),
            ..Default::default()
        },
        MenuRow {
            label: "Done".to_string(),
            enabled: true,
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: 2.0,
                dy: 196.0,
                w: 98.0,
                h: 20.0,
            }),
            ..Default::default()
        },
    ];
    debug_assert_eq!(rows.len(), book_edit::page_row::DONE + 1);

    MenuFrame {
        rows,
        selected: usize::MAX,
        hovered: state.hovered,
        backdrop: MenuBackdrop::Dim,
        vanilla: true,
        book_background: true,
        labels,
        ..Default::default()
    }
}

/// Builds the signed-book reading screen's overlay frame — vanilla's
/// `BookViewScreen`, the read-only sibling of [`book_edit_frame`]'s page
/// layout.
///
/// Deliberately the *same* page geometry as [`book_edit_frame`]'s
/// non-signing layout: the two screens draw the same book and vanilla gives
/// them the same `TEXT_WIDTH`, so a reader who signs a draft should see the
/// text land where it already was. The differences are all removals — no
/// Sign button, no caret, and `>` is disabled on the last page rather than
/// appending one, because a signed book is immutable.
///
/// Row indices match [`book_view::page_row`]. The page text is emitted as
/// authored styled runs, informational only and outside `rows`' click system,
/// exactly as [`book_edit_frame`]'s is.
pub fn book_view_frame(state: &book_view::BookViewState) -> MenuFrame<'static> {
    const PAGE_DX: f32 = -60.0;
    const PAGE_Y: f32 = 32.0;
    const PAGE_LINE_H: f32 = 9.0;
    const BOOK_TEXT: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    let (current, total) = state.page_indicator();

    let mut labels = Vec::new();
    for (i, line) in state.visible_styled_lines().into_iter().enumerate() {
        let mut dx = PAGE_DX;
        for span in line {
            let width = span.text.chars().count() as f32 * 6.0;
            labels.push(MenuLabel {
                text: span.text,
                origin: Origin::ScreenTop,
                dx,
                dy: PAGE_Y + i as f32 * PAGE_LINE_H,
                align: Align::Left,
                colour: span
                    .style
                    .color
                    .map_or(BOOK_TEXT, |colour| rgb_text_colour(colour.rgb())),
                scale: 1.0,
            });
            dx += width;
        }
    }
    labels.push(MenuLabel {
        text: format!("Page {current} of {total}"),
        origin: Origin::ScreenTop,
        dx: 52.0,
        dy: 18.0,
        align: Align::Right,
        colour: BOOK_TEXT,
        scale: 1.0,
    });

    let footer_y = 159.0;
    let rows = vec![
        MenuRow {
            label: "<".to_string(),
            enabled: state.can_page_back(),
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: -53.0,
                dy: footer_y,
                w: 23.0,
                h: 13.0,
            }),
            book_page: Some(BookPageButton::Backward),
            ..Default::default()
        },
        MenuRow {
            label: ">".to_string(),
            enabled: state.can_page_forward(),
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: 20.0,
                dy: footer_y,
                w: 23.0,
                h: 13.0,
            }),
            book_page: Some(BookPageButton::Forward),
            ..Default::default()
        },
        MenuRow {
            label: "Done".to_string(),
            enabled: true,
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: -100.0,
                dy: 196.0,
                w: 200.0,
                h: 20.0,
            }),
            ..Default::default()
        },
    ];
    debug_assert_eq!(rows.len(), book_view::page_row::DONE + 1);

    MenuFrame {
        rows,
        selected: usize::MAX,
        hovered: state.hovered,
        backdrop: MenuBackdrop::Dim,
        vanilla: true,
        book_background: true,
        labels,
        ..Default::default()
    }
}

/// Converts model text colour bytes to the menu renderer's 0..1 RGBA form.
/// The book reader uses this at the last possible boundary so `TextSpan`
/// inheritance and legacy-format expansion remain owned by `lodestone-model`.
fn rgb_text_colour(rgb: u32) -> [f32; 4] {
    [
        ((rgb >> 16) & 0xff) as f32 / 255.0,
        ((rgb >> 8) & 0xff) as f32 / 255.0,
        (rgb & 0xff) as f32 / 255.0,
        1.0,
    ]
}

/// Builds the Spectator Menu's overlay frame (issue #613's
/// `TeleportToEntity` remainder — see [`spectator_menu`]'s module doc for
/// what the vertical-list layout deliberately simplifies against vanilla's
/// paginated icon-slot bar).
///
/// Row indices match [`spectator_menu::SpectatorMenuState::visible`]'s own
/// index space exactly — row 0 is either the first root entry, or (while a
/// team category is expanded) the "< Back" row — so a click resolved
/// against this frame's [`MenuRow`]s and a call to
/// [`spectator_menu::SpectatorMenuState::activate`] can never disagree about
/// what row `N` means.
#[must_use]
pub fn spectator_menu_frame(state: &spectator_menu::SpectatorMenuState) -> MenuFrame<'static> {
    const TITLE_Y: f32 = 20.0;
    const ROW_Y0: f32 = 44.0;
    const ROW_H: f32 = 20.0;
    const ROW_GAP: f32 = 2.0;
    const ROW_W: f32 = 200.0;

    let title = if state.expanded_team().is_some() {
        "Team Teleport".to_string()
    } else {
        "Spectator Menu".to_string()
    };
    let mut labels = vec![MenuLabel {
        text: title,
        origin: Origin::ScreenTop,
        dx: 0.0,
        dy: TITLE_Y,
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    }];

    let visible = state.visible();
    let mut rows: Vec<MenuRow> = Vec::with_capacity(visible.len());
    for (i, row) in visible.iter().enumerate() {
        let slot = Some(Slot {
            origin: Origin::ScreenTop,
            dx: -ROW_W / 2.0,
            dy: ROW_Y0 + i as f32 * (ROW_H + ROW_GAP),
            w: ROW_W,
            h: ROW_H,
        });
        rows.push(match row {
            spectator_menu::SpectatorMenuRow::Back => MenuRow {
                label: "< Back".to_string(),
                enabled: true,
                slot,
                ..Default::default()
            },
            spectator_menu::SpectatorMenuRow::Team { label, count } => MenuRow {
                label: format!("{label} ({count})"),
                enabled: true,
                head: Some(default_head_icon()),
                slot,
                ..Default::default()
            },
            spectator_menu::SpectatorMenuRow::Player(player) => MenuRow {
                label: player.name.clone(),
                enabled: true,
                head: Some(default_head_icon()),
                slot,
                ..Default::default()
            },
        });
    }

    let hidden = state.hidden_row_count();
    let message = (hidden > 0 && rows.len() == crate::menu::spectator_menu::MAX_VISIBLE_ROWS)
        .then(|| format!("...and {hidden} more not shown"));
    if rows.is_empty() {
        labels.push(MenuLabel {
            text: "No one else is here to teleport to.".to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: ROW_Y0,
            align: Align::Centre,
            colour: widget::argb_to_rgba(widget::INACTIVE_MESSAGE_ARGB),
            scale: 1.0,
        });
    }

    MenuFrame {
        rows,
        selected: usize::MAX,
        hovered: state.hovered,
        backdrop: MenuBackdrop::Dim,
        vanilla: true,
        labels,
        message,
        ..Default::default()
    }
}

/// Builds the loading screen's frame: the panorama under
/// `menu_background.png`'s wash, with one centred line of text — the current
/// `ConnectPhase`'s own vanilla string
/// (`connect.connecting`/`connect.joining`) while the handshake and
/// configuration phase runs, `multiplayer.downloadingTerrain` while the
/// player's own chunk streams in after login.
///
/// This is the bar-less variant: no vanilla `LevelLoadingScreen` chrome. See
/// [`loading_frame_with_progress`] for the one that also draws a bar, and
/// `crate::menu::loading` for why the connect phases deliberately have no
/// progress to show.
///
/// Same shape as [`error_frame`]: a `vanilla` frame whose only geometry is the
/// backdrop and one [`MenuLabel`]. Unlike every other `vanilla` frame it
/// carries no [`MenuRow`]s — there is nothing to click on a loading screen, and
/// input is deliberately inert for its whole lifetime (see
/// [`super::Screen::Connecting`]'s doc and [`super::nav::MenuNav::key`]'s
/// catch-all arm, which routes only Escape, and Escape is a no-op there).
///
/// # The backdrop, and what it used to be
///
/// Both callers take the **default** [`MenuBackdrop::Panorama`], and that is a
/// fix rather than a simplification. This used to set `overlay: true`, and that
/// one flag did two jobs — it chose the translucent backdrop colour *and* it was
/// the only thing suppressing the panorama — so the screen came out as a flat
/// clear with a translucent quad over it and no sky at all. No vanilla path
/// produces a flat fill:
///
/// - `ConnectScreen` overrides no background at all, so it takes the base
///   `Screen.extractBackground`: panorama (its `minecraft.level == null` gate is
///   satisfied while connecting), blur, then the wash.
/// - `LevelLoadingScreen.extractBackground`'s `OTHER` arm — the ordinary
///   loading reason — calls `extractPanorama` with **no** `level == null` gate,
///   so the panorama covers even a live level. Its other two arms are the nether
///   and end portal animations, which we do not have and which are a separate
///   piece of work, not this frame's.
///
/// Both callers therefore want the same thing, which is why neither names a
/// backdrop. The post-login caller still goes through
/// [`MenuRenderer::render_overlay`], and its `Load` op is now merely harmless:
/// the panorama covers every pixel of the world it draws over, exactly as
/// vanilla's does.
#[must_use]
pub fn loading_frame(text: &str) -> MenuFrame<'static> {
    MenuFrame {
        vanilla: true,
        labels: vec![MenuLabel {
            text: text.to_string(),
            origin: Origin::Centre,
            dx: 0.0,
            dy: 0.0,
            align: Align::Centre,
            colour: LABEL,
            scale: 1.0,
        }],
        ..Default::default()
    }
}

/// The phase label pushed up to make room for the bar. Vanilla's own text sits
/// 12 px above its bar (`LevelLoadingScreen`: bar at `textTop + 12`), and the
/// label's own line height is 9, so -14 puts the same gap under the text.
const LOADING_LABEL_DY: f32 = -14.0;

/// The count line, one line under the bar.
const LOADING_DETAIL_DY: f32 = 8.0;

/// The `en_us` grey vanilla uses for the secondary line under a progress bar
/// (`0xFFA0A0A0`) — dimmer than the phase name so the phase reads first.
const LOADING_DETAIL: [f32; 4] = [160.0 / 255.0, 160.0 / 255.0, 160.0 / 255.0, 1.0];

/// As [`loading_frame`], but with vanilla's `LevelLoadingScreen` progress bar
/// and a raw count line under it.
///
/// This is the terrain-streaming half of the loading screen, and the *only*
/// screen in this file that draws a number. The number has to be real: it is
/// the client's own loaded-column count over the view square the server is
/// going to send (see [`crate::menu::loading::TerrainProgress`]), and the bar
/// cannot reach the end, because the screen is dismissed by a real predicate
/// (`Sim::terrain_loading`) and not by the bar filling. A bar that could read
/// as complete while the screen was still up would turn an honest freeze into a
/// false reassurance, which is the one failure mode the issue singles out.
///
/// `detail` is drawn dimmer than the phase name so a glance reads the phase
/// first and the count second — the count is for diagnosing a stall ("stuck at
/// 37/441"), not for watching.
#[must_use]
pub fn loading_frame_with_progress(
    text: &str,
    progress: crate::menu::loading::TerrainProgress,
) -> MenuFrame<'static> {
    loading_frame_with_progress_and_grid(text, progress, None)
}

/// The gap between the chunk grid's bottom edge and the phase label above it,
/// in logical pixels. Vanilla puts its own text *above* a grid centred on
/// the screen (`LevelLoadingScreen.extractRenderState`:
/// `textTop = yCenter - statusView.radius() * 2 - 9 * 3`); this frame instead
/// keeps the existing label/bar/count block exactly where issue #449 put it
/// (screen centre) and stacks the grid above *that* block, since this frame
/// also draws a raw count line vanilla's screen does not. The two arrangements
/// agree on the part that matters — the grid sits above the text, never
/// overlapping the bar or the count — without re-deriving vanilla's own
/// vertical arithmetic for a layout this frame no longer has.
const CHUNK_GRID_GAP: f32 = 6.0;

/// The chunk grid's vertical centre for a grid of the given `radius`, in the
/// same "logical pixels from screen centre" convention [`MenuProgress::dy`]
/// uses — placed so its bottom edge sits [`CHUNK_GRID_GAP`] above the phase
/// label, for any radius.
///
/// A free function, not inlined into [`loading_frame_with_progress_and_grid`],
/// so a layout gate can compute the same value the frame was built with
/// instead of restating the arithmetic as a second, driftable copy.
#[must_use]
pub fn chunk_grid_dy(radius: u32) -> f32 {
    let half =
        crate::menu::loading::TerrainChunkGrid::diameter(radius) as f32 * super::CHUNK_CELL_SIZE
            * 0.5;
    LOADING_LABEL_DY - CHUNK_GRID_GAP - half
}

/// As [`loading_frame_with_progress`], but also carrying vanilla's
/// `LevelLoadingScreen` chunk-status grid when one is available.
///
/// `grid` is `None` under exactly the conditions
/// `Sim::terrain_chunk_grid` returns `None` for — no session, or no declared
/// view radius yet — and a `None` here draws nothing extra, same as
/// [`loading_frame_with_progress`] always did. The grid is genuinely real
/// per-column state (see [`crate::menu::loading::ChunkCellStatus`]'s doc for
/// what makes it so and why it has only two colours), never a scalar dressed
/// up to look spatial.
#[must_use]
pub fn loading_frame_with_progress_and_grid(
    text: &str,
    progress: crate::menu::loading::TerrainProgress,
    grid: Option<crate::menu::loading::TerrainChunkGrid>,
) -> MenuFrame<'static> {
    MenuFrame {
        vanilla: true,
        // Default `MenuBackdrop::Panorama`, as [`loading_frame`] explains.
        labels: vec![
            MenuLabel {
                text: text.to_string(),
                origin: Origin::Centre,
                dx: 0.0,
                dy: LOADING_LABEL_DY,
                align: Align::Centre,
                colour: LABEL,
                scale: 1.0,
            },
            MenuLabel {
                text: progress.detail(),
                origin: Origin::Centre,
                dx: 0.0,
                dy: LOADING_DETAIL_DY,
                align: Align::Centre,
                colour: LOADING_DETAIL,
                scale: 1.0,
            },
        ],
        progress: Some(super::MenuProgress {
            fraction: progress.fraction(),
            dy: 0.0,
        }),
        chunk_grid: grid.map(|grid| super::ChunkGridView {
            dy: chunk_grid_dy(grid.radius),
            grid,
        }),
        ..Default::default()
    }
}

// -- vanilla's `DisconnectedScreen` metrics -----------------------------------

/// Vanilla's own button builder, called with a width of 200 at every call site
/// — not [`widget::DEFAULT_WIDTH`]'s
/// 150.
const ERROR_BUTTON_W: f32 = 200.0;
/// Room reserved above the bottom edge for the one button this screen draws:
/// [`WIDGET_H`] plus a margin roughly matching vanilla's `padding(2)` between
/// stack children plus some slack so the
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
///, which is canvas-*dependent* and therefore
/// not expressible as a fixed [`MenuNotice::w`] (the same reason
/// [`ACCOUNTS_ROW_W`] is fixed rather than derived per-canvas). Sized off
/// [`crate::config::MIN_SCALED_WIDTH`] so it is correct even at the smallest
/// canvas `calculate_gui_scale` can produce — the same conservative-at-minimum
/// trade [`super::options::LIST_WINDOW_PX`] makes vertically.
const ERROR_NOTICE_W: f32 = crate::config::MIN_SCALED_WIDTH as f32 - 50.0;

/// Builds vanilla's `DisconnectedScreen` (framework epic — this
/// screen was still the pre-framework centred row stack, with no [`Slot`] on
/// its row and no wrapped-text bound on its reason, until now):
/// title, the disconnect reason wrapped and bounded exactly like
/// [`accounts_failed_frame`]'s failure message, and one real button
/// (vanilla's own disconnected-screen class).
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
/// **The title is per-case, from [`SessionEndKind`]**, which is how vanilla does
/// it: `DisconnectedScreen` takes its `title` as a constructor argument and puts
/// it in its own `StringWidget` *above* the reason's `MultiLineTextWidget` —
/// never glued onto the reason as a prefix.
///
/// | end kind | vanilla title | vanilla call site |
/// |---|---|---|
/// | [`SessionEndKind::Disconnected`] | `disconnect.lost` — "Connection Lost" | `ClientCommonPacketListenerImpl.onDisconnect`'s `GENERIC_DISCONNECT_MESSAGE` |
/// | [`SessionEndKind::Failed`] | `connect.failed` — "Failed to connect to the server" | `ClientHandshakePacketListenerImpl.onDisconnect` and `ConnectScreen`'s `connectFailedTitle` |
/// | [`SessionEndKind::Died`] | "Connection Lost" | not a vanilla path at all — this arm only exists as the death gate's negative control |
///
/// This used to pick "Connection Lost" for every case and say so, because the
/// two causes were indistinguishable by the time they reached here: both arrived
/// as one `format!("disconnected: {…}")` string. They are separate now.
///
/// `shouldCloseOnEsc()` is `false` in vanilla (`:82-85`) — Escape does
/// **not** dismiss this screen there, so a misclick cannot swallow a network
/// error before it is read. This client's Escape *does* dismiss it (see
/// `nav::MenuNav`'s `Screen::Error` arm), which is a pre-existing, separately
/// tested behaviour this pass does not change — this function is layout, not
/// input semantics.
#[must_use]
pub(super) fn error_frame(end: Option<&SessionEnd>) -> MenuFrame<'static> {
    let title = match end.map(|e| e.kind) {
        Some(SessionEndKind::Failed) => "Failed to connect to the server",
        _ => "Connection Lost",
    };
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
            text: title.to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: ERROR_TITLE_Y,
            align: Align::Centre,
            colour: LABEL,
            scale: 1.0,
        }],
        // An empty reason never happens in production (`session_failed` always
        // carries a real message), but an empty notice would still draw zero
        // lines correctly — no special-casing needed, unlike `death_frame`'s
        // optional message.
        //
        // `spans` is what makes a kicked player see the server's own colours:
        // the reason arrives as a `Text` tree and `to_spans` resolves style
        // inheritance through its `extra` children, so a root with no text and
        // all the content in `extra` — the ordinary shape of a server's kick
        // message — renders per-run rather than in one flat colour. `text` is
        // still the plain string because wrapping has to be measured in the
        // font, and `FG_BAD` is the fallback for a reason that specifies no
        // colour of its own.
        notice: end.map(|end| MenuNotice {
            text: end.reason.to_plain_string(),
            spans: end.reason.to_spans(),
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

// -- the ownership gate ------------------------------------------------

/// The gate's button width. [`ERROR_BUTTON_W`]'s 200 px, for the same reason
/// that one is 200: a lone centred button at vanilla's own widest widget size,
/// on a screen with nothing else competing for the width.
const OWNERSHIP_BUTTON_W: f32 = 200.0;

/// Gap between the gate's two stacked buttons — vanilla's own 4 px widget
/// spacing, which is what every stacked-button layout in this menu uses.
const OWNERSHIP_BUTTON_GAP: f32 = 4.0;

/// Distance from the canvas bottom to the *top* of the lower button.
/// [`ERROR_BUTTON_BOTTOM_MARGIN`]'s value, so the gate's last row sits exactly
/// where the disconnect screen's single row does.
const OWNERSHIP_BUTTON_BOTTOM_MARGIN: f32 = WIDGET_H + 20.0;

/// The gate title's baseline offset from the canvas top. [`ERROR_TITLE_Y`]'s
/// 40 px, for the same reason: both are single-message screens with a title, a
/// paragraph and buttons, and putting them at different heights would make a
/// disconnect look like a different kind of screen from this one.
const OWNERSHIP_TITLE_Y: f32 = 40.0;

/// Wrap width for the explanatory paragraph. [`ERROR_NOTICE_W`]'s derivation —
/// `MIN_SCALED_WIDTH` less a 25 px margin each side — so the text is correct at
/// the smallest canvas `calculate_gui_scale` can produce.
const OWNERSHIP_NOTICE_W: f32 = crate::config::MIN_SCALED_WIDTH as f32 - 50.0;

/// The gate's heading.
const OWNERSHIP_TITLE: &str = "Sign in to play";

/// The gate's explanatory paragraph.
///
/// It has to answer three questions at once, because a player who has just
/// launched the game and cannot reach *anything* will otherwise assume the
/// build is broken: what is being asked, why singleplayer is included, and
/// what happens to the account afterwards.
const OWNERSHIP_BODY: &str = "Lodestone needs at least one Microsoft account                               that owns Minecraft before you can play — including                               singleplayer and offline play. Add one and it joins                               your account list, where you can switch between                               accounts or set the name you play offline under.";

/// Builds the ownership gate ([`super::Screen::Ownership`]): a title, the
/// paragraph above, and [`super::nav::OWNERSHIP_BUTTONS`] stacked at the
/// bottom.
///
/// **Not vanilla geometry**, and there is nothing to be faithful to: real
/// Minecraft resolves an account in a separate launcher before the game process
/// starts, so it has no in-game equivalent of this screen. The metrics are
/// therefore borrowed from [`error_frame`], which is the closest thing this menu
/// already has — one message, one decision, no list.
///
/// The buttons are laid out **upwards from the bottom**, so the last row lands
/// on the same baseline the disconnect screen's single row does regardless of
/// how many rows there are.
#[must_use]
pub(super) fn ownership_frame(nav: &super::nav::MenuNav) -> MenuFrame<'static> {
    use super::nav::OWNERSHIP_BUTTONS;
    let count = OWNERSHIP_BUTTONS.len();
    MenuFrame {
        rows: OWNERSHIP_BUTTONS
            .iter()
            .enumerate()
            .map(|(i, b)| {
                // `count - 1 - i` rows sit below this one, each costing a
                // button plus a gap. Derived from the row's own index rather
                // than written as two literal offsets, so adding a third button
                // cannot leave one of them behind.
                let below = (count - 1 - i) as f32;
                MenuRow {
                    label: b.label().to_string(),
                    enabled: true,
                    slot: Some(Slot {
                        origin: Origin::ScreenBottom,
                        dx: -(OWNERSHIP_BUTTON_W * 0.5),
                        dy: -(OWNERSHIP_BUTTON_BOTTOM_MARGIN
                            + below * (WIDGET_H + OWNERSHIP_BUTTON_GAP)),
                        w: OWNERSHIP_BUTTON_W,
                        h: WIDGET_H,
                    }),
                    ..Default::default()
                }
            })
            .collect(),
        selected: nav.ownership_index(),
        vanilla: true,
        labels: vec![MenuLabel {
            text: OWNERSHIP_TITLE.to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: OWNERSHIP_TITLE_Y,
            align: Align::Centre,
            colour: LABEL,
            scale: 1.0,
        }],
        notice: Some(MenuNotice {
            text: OWNERSHIP_BODY.to_string(),
            spans: Vec::new(),
            origin: Origin::ScreenTop,
            dx: -(OWNERSHIP_NOTICE_W * 0.5),
            dy: OWNERSHIP_TITLE_Y + LINE_H * 3.0,
            w: OWNERSHIP_NOTICE_W,
            bottom: OWNERSHIP_BUTTON_BOTTOM_MARGIN
                + WIDGET_H
                + (count - 1) as f32 * (WIDGET_H + OWNERSHIP_BUTTON_GAP),
            colour: LABEL,
        }),
        ..Default::default()
    }
}

// -- the credits/end-poem screen ---------------------------------
//
// **Not vanilla geometry.** Vanilla's own win-screen rendering draws no widgets at all: it is a
// full-height scrolling column of text (the end poem, then a real Mojang
// employee credits roll) advanced by an elapsed-time tick every frame, with
// **any** keypress skipping straight to the end of the scroll
// (vanilla's own win-screen rendering's own `keyPressed`/`mouseClicked` overrides). Two things
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
//    contributors did not write Mojang's game). `version_line`'s "Lodestone …"
//    a few lines up in this file already drew this same line once: naming
//    this project rather than borrowing vanilla's.
//
// So this screen is a short, honestly-Lodestone-authored placeholder: it
// proves the screen/session-teardown mechanism (own scope is
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
            // Our own text, so no styled runs to preserve.
            spans: Vec::new(),
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
