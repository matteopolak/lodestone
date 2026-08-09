//! `menu/render.rs`'s own test module, split out verbatim and unwrapped.
//! Still `render::tests`, so **no test path changed** — splitting it further
//! into `render/tests/*.rs` would rename every one of them, which is why this
//! file is deliberately over the size target the rest of the split aims at.
//!
//! `use super::*` below is the module's own, unchanged; the sibling imports
//! after it are what the old inline module reached through the single flat
//! namespace.

use super::*;
use super::account_screen::{ACCOUNTS_BUTTON_W, ACCOUNTS_FOOTER_SPACING, ACCOUNTS_HEAD_ICON, AccountsBlock, accounts_block, accounts_button_slot, accounts_failed_frame, accounts_idle_frame};
use super::draw::{
    Quads, TOOLTIP_BG, TOOLTIP_LINE_H, TOOLTIP_MOUSE_OFFSET, TOOLTIP_PAD, wrap_bounded,
    wrap_measured,
};
use super::renderer::{FLOATS_PER_VERTEX, SPRITE_FLOATS_PER_VERTEX};
use super::screens::credits_frame;
use super::server_list::{SERVER_ENTRY_SPACING, SERVER_ICON_DARKEN, SERVER_JOIN_SPRITES, SERVER_LIST_REF_CANVAS, SERVER_LIST_ROW_W, SERVER_MOVE_DOWN_SPRITES, SERVER_MOVE_UP_SPRITES, ServerListBlock};
use super::title_pause::pause_menu_grid_with;
use super::world_list::{WorldSelectBlock, world_select_block};
use crate::menu::nav::{MenuKey, MenuNav};
use crate::menu::servers::ServerEntry;
use crate::menu::status::{ServerStatus, StatusCache, unavailable_probe};
use crate::menu::{Screen, SessionKind, UiState};

/// Vertex stride in the emitted buffer.
const STRIDE: usize = FLOATS_PER_VERTEX;

/// A nav with a temporary (never-loaded) list path, so no test reads the
/// developer's real `servers.json`.
fn test_nav(tag: &str) -> MenuNav {
    let path = std::env::temp_dir().join(format!(
        "lodestone-render-{}-{tag}/servers.json",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    MenuNav::with_path(path)
}

fn add_server(nav: &mut MenuNav, ui: &mut UiState, name: &str, addr: &str) {
    let back = ui.screen();
    if back != Screen::ServerList {
        *ui = UiState::new();
        ui.open_server_list();
    }
    nav.key(ui, MenuKey::Char('a'));
    for c in name.chars() {
        nav.key(ui, MenuKey::Char(c));
    }
    nav.key(ui, MenuKey::Tab);
    for c in addr.chars() {
        nav.key(ui, MenuKey::Char(c));
    }
    nav.key(ui, MenuKey::Enter);
}

/// Issue #47. Every one of the command block screen's seven interactive
/// rows, plus the read-only "Previous Output" row, at its exact vanilla
/// rect on a real canvas — not a restatement of `command_block.rs`'s own
/// constants, but the actual `MenuFrame`/`row_rect` output a click and a
/// draw both go through.
///
/// `854x480` is the same seed canvas `nav::SEED_CANVAS` uses, so
/// `floor(854/2) == 427` and `floor(480/4) == 120` are the two integer
/// divisions every rect below is built from.
#[test]
fn command_block_rects_match_vanillas_own_arithmetic() {
    use command_block::{CommandBlockOpen, CommandBlockRow, CommandBlockState};

    let state = CommandBlockState::new(CommandBlockOpen::default());
    let frame = command_block_frame(&state, None);
    let (w, h) = (854.0_f32, 480.0_f32);
    let rect = |row: CommandBlockRow| row_rect(&frame.rows, row as usize, w, h).unwrap();

    // Command field: `width/2 - 150, 50, 300, 20`.
    assert_eq!(rect(CommandBlockRow::Command), (277.0, 50.0, 300.0, 20.0));
    // Track Output: `width/2 + 130, 135, 20, 20`.
    assert_eq!(rect(CommandBlockRow::TrackOutput), (557.0, 135.0, 20.0, 20.0));
    // Mode/Conditional/Automatic: shared y 165, widths 100.
    assert_eq!(rect(CommandBlockRow::Mode), (273.0, 165.0, 100.0, 20.0));
    assert_eq!(rect(CommandBlockRow::Conditional), (377.0, 165.0, 100.0, 20.0));
    assert_eq!(rect(CommandBlockRow::Automatic), (481.0, 165.0, 100.0, 20.0));
    // Done/Cancel: `Origin::CommandBlockFooter`'s anchor is
    // `(427, floor(480/4) + 132) == (427, 252)`.
    let done = row_rect(&frame.rows, CommandBlockRow::Done as usize, w, h).unwrap();
    let cancel = row_rect(&frame.rows, CommandBlockRow::Cancel as usize, w, h).unwrap();
    assert_eq!(done, (273.0, 252.0, 150.0, 20.0));
    assert_eq!(cancel, (431.0, 252.0, 150.0, 20.0));
    // Rejected hypothesis: reusing `Origin::TitleTop`'s `floor(h/4) + 48`
    // (`168`, not `252`) — an 84 px miss, not a rounding difference, so a
    // wrong-origin bug here cannot pass by accident.
    assert_ne!(done.1, (h / 4.0).floor() + 48.0);

    // Row 7: the read-only previous-output field, sharing the command
    // field's x and the track-output row's y.
    let previous = row_rect(&frame.rows, command_block::PREVIOUS_OUTPUT_ROW, w, h).unwrap();
    assert_eq!(previous, (277.0, 135.0, 276.0, 20.0));

    // Captions: the default state (fresh `CommandBlockOpen`) is
    // Redstone/Unconditional/Needs-Redstone/track-output-off, matching
    // vanilla's own field initialisers.
    assert_eq!(frame.rows[CommandBlockRow::Mode as usize].label, "Impulse");
    assert_eq!(
        frame.rows[CommandBlockRow::Conditional as usize].label,
        "Unconditional"
    );
    assert_eq!(
        frame.rows[CommandBlockRow::Automatic as usize].label,
        "Needs Redstone"
    );
    assert_eq!(
        frame.rows[CommandBlockRow::TrackOutput as usize].label,
        "X"
    );
    assert_eq!(frame.rows[CommandBlockRow::Done as usize].label, "Done");
    assert_eq!(frame.rows[CommandBlockRow::Cancel as usize].label, "Cancel");
}

/// Issue #47's tab-completion popup rect, predicted against vanilla's own
/// clamp formula and checked against a rejected hypothesis: forgetting
/// the synthetic-slash offset shift (`command_block`'s module doc) would
/// place the popup 6 px too far right, not merely "somewhere near".
#[test]
fn command_block_suggestion_popup_lands_at_the_predicted_clamped_rect() {
    use command_block::{CommandBlockOpen, CommandBlockRow, CommandBlockState};
    use lodestone_model::command_tree::{CommandTree, NodeKind, RawCommandNode};

    let nodes = vec![
        RawCommandNode {
            kind: NodeKind::Root,
            children: vec![1],
            executable: false,
            restricted: false,
            redirect: None,
        },
        RawCommandNode {
            kind: NodeKind::Literal {
                name: "gamemode".to_string(),
            },
            children: vec![2, 3],
            executable: false,
            restricted: false,
            redirect: None,
        },
        RawCommandNode {
            kind: NodeKind::Literal {
                name: "creative".to_string(),
            },
            children: vec![],
            executable: true,
            restricted: false,
            redirect: None,
        },
        RawCommandNode {
            kind: NodeKind::Literal {
                name: "survival".to_string(),
            },
            children: vec![],
            executable: true,
            restricted: false,
            redirect: None,
        },
    ];
    let tree = CommandTree::new(nodes, 0).unwrap();
    let mut state = CommandBlockState::new(CommandBlockOpen::default());
    state.command.set_value("gamemode c");
    let frame = command_block_frame(&state, Some(&tree));
    let (w, h) = (854.0_f32, 480.0_f32);

    // One row past `PREVIOUS_OUTPUT_ROW`: only "creative" matches "c".
    let popup_row = command_block::PREVIOUS_OUTPUT_ROW + 1;
    assert_eq!(frame.rows.len(), popup_row + 1, "exactly one candidate");
    assert_eq!(frame.rows[popup_row].label, "creative");

    // `start == 9` (see `command_block`'s own completion test), advance
    // 6.0, `BORDER_INSET` 4.0: `unclamped_dx = -150 + 4 + 6*9 = -92`,
    // `x = 427 - 92 = 335` — comfortably inside `[0, 525]`, so the clamp
    // itself is not exercised here (a second test would need a command
    // long enough to push the popup off the right edge).
    let popup_w = 8.0 * 6.0; // "creative", 8 chars, fixed advance 6.0.
    let (px, py, pw, ph) = row_rect(&frame.rows, popup_row, w, h).unwrap();
    assert_eq!((px, py, ph), (335.0, 71.0, 12.0));
    assert_eq!(pw, popup_w + 1.0);

    // Rejected hypothesis: an adapter that forgot to shift the synthetic
    // slash's offset back by one would compute `start == 10`, landing at
    // `427 + (-150 + 4 + 60) == 341` — 6 px right of the real answer, not
    // an imperceptible rounding difference.
    let wrong_dx = 427.0 + (-150.0 + 4.0 + 6.0 * 10.0);
    assert_ne!(px, wrong_dx);
    assert_eq!(wrong_dx - px, 6.0);
}

#[test]
fn owns_frame_agrees_with_frame_for_on_every_screen() {
    // Two definitions of "this renderer owns the frame" that can disagree is
    // how a screen ends up drawn twice, or not at all. Walk every screen and
    // require the predicate and the builder to say the same thing.
    let mut nav = test_nav("owns");
    let mut fav = FaviconCache::new();
    let statuses = StatusCache::with_probe(unavailable_probe());

    let mut reached = 0;
    // `Screen::ALL`, not a list restated here: this loop's own copy was a
    // 12-entry literal plus an `assert_eq!(reached, 12)`, and #397's
    // `WorldSelect` made both stale at once — a completeness test defeated by
    // the very thing it exists to notice. The `match` below stays exhaustive,
    // which is what forces a new variant to be given a way to be *reached*;
    // `Screen::ALL`'s own docs say what that does and does not guarantee.
    for screen in Screen::ALL {
        let mut ui = UiState::new();
        match screen {
            Screen::MainMenu => {}
            Screen::ServerList => ui.open_server_list(),
            Screen::ServerEdit => {
                ui.open_server_list();
                ui.open_server_edit();
            }
            Screen::WorldSelect => ui.open_world_select(),
            Screen::Settings => ui.open_settings(),
            Screen::Accounts => ui.open_accounts(),
            Screen::Connecting => ui.begin(SessionKind::Multiplayer),
            Screen::Playing => ui.enter_dev_world(),
            Screen::Chat => {
                ui.enter_dev_world();
                ui.open_chat();
            }
            Screen::Container => {
                ui.enter_dev_world();
                ui.open_container();
            }
            Screen::CommandBlockEdit => {
                ui.enter_dev_world();
                ui.open_command_block();
            }
            Screen::Paused => {
                ui.enter_dev_world();
                ui.pause();
            }
            Screen::Death => {
                ui.enter_dev_world();
                ui.die(Some("blew up".to_string()));
            }
            Screen::Error => {
                ui.begin(SessionKind::Multiplayer);
                ui.session_failed(crate::sim::SessionEnd::disconnected(
            lodestone_model::Text::literal("connection refused"),
        ));
            }
            Screen::Credits => {
                ui.enter_dev_world();
                ui.show_credits();
            }
            Screen::Social => {
                ui.enter_dev_world();
                ui.pause();
                ui.open_social_from_pause();
            }
            Screen::Statistics => {
                ui.enter_dev_world();
                ui.pause();
                ui.open_statistics_from_pause();
            }
            Screen::Advancements => {
                ui.enter_dev_world();
                ui.pause();
                ui.open_advancements_from_pause();
            }
            Screen::CreateWorld => {
                ui.open_world_select();
                ui.open_create_world();
            }
            // Issue #540. Reached the way a player reaches it — through the world
            // list — because `open_confirm` guards on that screen for the reason
            // its own doc gives.
            Screen::Confirm => {
                ui.open_world_select();
                ui.open_confirm();
            }
        }
        assert_eq!(ui.screen(), screen, "failed to reach {screen:?}");
        reached += 1;
        let built = frame_for(&ui, &nav, &statuses, &mut fav).is_some();
        assert_eq!(
            built,
            owns_frame(screen),
            "owns_frame and frame_for disagree about {screen:?}"
        );
        // And a frame it claims must actually be drawable.
        if built {
            let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
            // A vanilla-laid-out screen has no centred heading string — its
            // heading is the logo texture (title) or a positioned
            // `MenuLabel` (pause), so requiring `title` would be requiring
            // the *un*-vanilla layout. It must still say something.
            if f.vanilla {
                assert!(
                    f.logo || !f.labels.is_empty(),
                    "{screen:?} is vanilla-laid-out but draws neither a logo nor a label"
                );
            } else {
                assert!(!f.title.is_empty(), "{screen:?} has no title");
            }
            assert!(
                !geometry(&f, 1280.0, 720.0).is_empty(),
                "{screen:?} draws nothing"
            );
        }
    }
    // Derived, not restated. This no longer catches "a screen was added"
    // (`Screen::ALL` is what does, as far as anything can) — what it still
    // catches is this loop silently skipping one, e.g. a `continue` added to
    // the reach-the-screen `match` above.
    assert_eq!(
        reached,
        Screen::ALL.len(),
        "the loop skipped a screen it was handed"
    );
    let _ = &mut nav;
}

/// Issue #192's own frame: one enabled row (Done), a title label, and a
/// non-empty body notice, all resolving on-canvas — the same shape
/// `error_frame`'s callers already get for free through the sweep above,
/// spelled out here because `credits_frame` takes no arguments (unlike
/// `error_frame`, which the sweep exercises through `ui.error()`) and so
/// is otherwise only reached indirectly.
#[test]
fn credits_frame_has_one_live_row_a_title_and_a_body() {
    let f = credits_frame();
    assert_eq!(f.rows.len(), 1, "one control: Done");
    assert!(f.rows[0].enabled);
    assert_eq!(f.rows[0].label, "Done");
    assert_eq!(f.selected, 0);
    assert!(f.vanilla, "laid out the same way error_frame is");
    assert!(!f.labels.is_empty(), "a title label must be present");
    assert!(
        f.notice.as_ref().is_some_and(|n| !n.text.is_empty()),
        "a body notice must be present and non-empty"
    );
    let (w, h) = (1280.0, 720.0);
    assert!(
        !geometry(&f, w, h).is_empty(),
        "the frame must draw something"
    );
    let (rx, ry, rw, rh) = f.rows[0].slot.unwrap().resolve(w, h);
    assert!(
        rx >= 0.0 && ry >= 0.0 && rx + rw <= w && ry + rh <= h,
        "the Done button must resolve on-canvas: ({rx}, {ry}) {rw}x{rh}"
    );
}

#[test]
fn the_server_list_shows_the_motd_players_and_latency_from_a_status() {
    // The content gate: what the status decoder produced has to appear in the
    // row, not merely be cached.
    let mut nav = test_nav("content");
    let mut ui = UiState::new();
    add_server(&mut nav, &mut ui, "HOME", "mc.example.com");

    let mut statuses = StatusCache::with_probe(std::sync::Arc::new(|_| {
        Ok(ServerStatus {
            motd: "A LODESTONE SERVER\nsecond line".into(),
            // No server styling in this fixture: the row must lay out
            // identically to before the styled path existed.
            motd_spans: Vec::new(),
            players: "3/20".into(),
            online: Some(3),
            sample: Vec::new(),
            version: "26.2".into(),
            // Our own protocol, so the row resolves to
            // `ServerState::Successful` and shows a player count rather than
            // the red version string an incompatible server gets.
            protocol: Some(crate::menu::status::STATUS_PROTOCOL),
            favicon_png: None,
            latency_ms: Some(12),
        })
    }));
    let entries = nav.list().entries().to_vec();
    statuses.refresh(&entries);
    let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
    while statuses.pump() == 0 && crate::platform::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).expect("the list draws");
    assert_eq!(
        f.rows.len(),
        1 + crate::menu::nav::SERVER_LIST_BUTTONS.len(),
        "one entry plus vanilla's seven footer buttons"
    );
    assert_eq!(f.rows[0].label, "HOME");
    let view = f.rows[0].entry.as_ref().expect("row 0 is a list entry");
    // The **whole** MOTD, newline included: the wrap to two lines happens at
    // draw time, in the font the draw measures with (`wrap_measured`).
    assert_eq!(view.motd, "A LODESTONE SERVER\nsecond line");
    assert!(!view.motd_is_error);
    // The status column is the player count, not the latency: vanilla puts
    // `formatPlayerCount` there and the round-trip only in the ping *sprite*
    // and its tooltip (`ServerStatusPinger.java:88`).
    assert_eq!(view.status, "3/20");
    assert!(!view.status_is_error);
    // 12 ms is the fastest bucket, so five bars. Asserted by identity — a gate
    // that only proved "a ping sprite drew" passes on all five.
    assert_eq!(view.status_sprite, "server_list/ping_5");
    assert!(view.selected, "the one row is the selected one");
    assert!(
        !view.can_move_up && !view.can_move_down,
        "a single row has nowhere to move"
    );
}

/// The three states that are *not* "answered by a compatible server" each get
/// their own sprite, and the assertion is by **identity**: a gate that only
/// proves a ping bar exists passes on all four rendering the same bar.
#[test]
fn every_row_state_resolves_to_its_own_status_sprite() {
    use crate::menu::status::{PINGING_SPRITES, ServerStatus};

    let mut nav = test_nav("states");
    let mut ui = UiState::new();
    add_server(&mut nav, &mut ui, "SLOW", "slow.example");

    // A compatible server, 700 ms — the fourth bucket down.
    let mut statuses = StatusCache::with_probe(std::sync::Arc::new(|_| {
        Ok(ServerStatus {
            motd: "hi".into(),
            players: "1/1".into(),
            protocol: Some(crate::menu::status::STATUS_PROTOCOL),
            latency_ms: Some(700),
            ..Default::default()
        })
    }));
    let entries = nav.list().entries().to_vec();
    // While the probe is in flight the row is `Pending`, which must animate.
    // Read *before* draining, and only asserted to be one of the five frames:
    // which one depends on a clock.
    statuses.refresh(&entries);
    let mut fav = FaviconCache::new();
    let pending = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let pending_view = pending.rows[0].entry.clone().unwrap();
    assert!(
        PINGING_SPRITES.contains(&pending_view.status_sprite),
        "an in-flight row must animate, got {}",
        pending_view.status_sprite
    );
    assert_eq!(
        pending_view.motd, "Pinging...",
        "vanilla overwrites the MOTD while pinging"
    );
    assert!(
        pending_view.status.is_empty(),
        "and blanks the status column"
    );

    let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
    while statuses.pump() == 0 && crate::platform::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let slow = frame_for(&ui, &nav, &statuses, &mut fav).unwrap().rows[0]
        .entry
        .clone()
        .unwrap();
    assert_eq!(slow.status_sprite, "server_list/ping_2", "700 ms is two bars");

    // An answered server speaking a different protocol is *incompatible*, not
    // unreachable: its own sprite, and its version in place of a player count.
    let mut old = StatusCache::with_probe(std::sync::Arc::new(|_| {
        Ok(ServerStatus {
            motd: "hi".into(),
            players: "1/1".into(),
            version: "1.21.11".into(),
            protocol: Some(1),
            latency_ms: Some(5),
            ..Default::default()
        })
    }));
    old.refresh(&entries);
    let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
    while old.pump() == 0 && crate::platform::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let view = frame_for(&ui, &nav, &old, &mut fav).unwrap().rows[0]
        .entry
        .clone()
        .unwrap();
    assert_eq!(view.status_sprite, "server_list/incompatible");
    assert_eq!(view.status, "1.21.11", "the version, where the count goes");
    assert!(view.status_is_error, "and in red");

    // And the four sprites are four different sprites.
    let mut all = vec![
        pending_view.status_sprite,
        slow.status_sprite,
        view.status_sprite,
    ];
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), 3, "two states share a sprite: {all:?}");
}

#[test]
fn a_failed_ping_shows_its_reason_in_the_error_colour() {
    let mut nav = test_nav("failed");
    let mut ui = UiState::new();
    add_server(&mut nav, &mut ui, "DEAD", "dead.example");

    let mut statuses = StatusCache::with_probe(std::sync::Arc::new(|_| {
        Err("connection refused".to_string())
    }));
    let entries = nav.list().entries().to_vec();
    statuses.refresh(&entries);
    let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
    while statuses.pump() == 0 && crate::platform::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let view = f.rows[0].entry.as_ref().expect("row 0 is a list entry");
    // The reason goes in the **MOTD** column and the status column stays
    // empty, which is vanilla's own arrangement: `onPingFailed` sets
    // `data.motd = CANT_CONNECT_MESSAGE` and `data.status` to empty
    // (`ServerStatusPinger.java:168-169`).
    assert_eq!(view.motd, "connection refused");
    assert!(
        view.motd_is_error,
        "a failure must be visually distinct from a MOTD"
    );
    assert!(view.status.is_empty(), "no player count to show");
    assert_eq!(
        view.status_sprite, "server_list/unreachable",
        "an unreachable row gets its own sprite, not a ping bar"
    );
}

/// With nothing to act on, Join / Edit / Delete are **present and inactive** —
/// `onSelectedChange`'s three, which is #393's disabled path reaching its first
/// list screen. Direct Connection is inactive whatever the selection.
///
/// The control is executed rather than described: adding a server must flip all
/// three, or "they are disabled" would pass on a screen whose buttons are
/// *always* disabled.
#[test]
fn the_footer_buttons_are_present_and_three_are_inactive_with_no_selection() {
    use crate::menu::nav::{SERVER_LIST_BUTTONS, ServerListButton as B};

    let mut nav = test_nav("emptylist");
    let mut ui = UiState::new();
    ui.open_server_list();
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

    // Every one of vanilla's seven is on screen even with an empty list — a
    // missing button is a layout that reads wrong, a greyed-out one reads
    // exactly like vanilla with the feature unavailable.
    assert_eq!(f.rows.len(), SERVER_LIST_BUTTONS.len());
    let row_of = |b: B| {
        SERVER_LIST_BUTTONS
            .iter()
            .position(|x| *x == b)
            .expect("every button is in the table")
    };
    for (i, button) in SERVER_LIST_BUTTONS.iter().enumerate() {
        assert_eq!(
            f.rows[i].label,
            button.label(),
            "row {i} is not {button:?} — the footer order is what click() assumes"
        );
    }
    for b in [B::Select, B::Edit, B::Delete, B::Direct] {
        assert!(!f.rows[row_of(b)].enabled, "{b:?} must be inactive");
    }
    for b in [B::Add, B::Refresh, B::Back] {
        assert!(f.rows[row_of(b)].enabled, "{b:?} must be active");
    }

    // Control: a selection enables three of the four, and Direct Connection
    // stays inactive because nothing here can honour it.
    add_server(&mut nav, &mut ui, "HOME", "mc.example.com");
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let base = 1;
    for b in [B::Select, B::Edit, B::Delete] {
        assert!(
            f.rows[base + row_of(b)].enabled,
            "{b:?} must be active once a row exists"
        );
    }
    assert!(
        !f.rows[base + row_of(B::Direct)].enabled,
        "Direct Connection has no screen to open, selection or not"
    );
}

/// Vanilla's own rects for `JoinMultiplayerScreen` at 854×480, hand-derived
/// from the Java rather than read back out of the layout — `CLAUDE.md`'s rule
/// that an expected value must originate outside the code under test.
///
/// The derivation, which is what a future reader has to be able to check:
///
/// - `HeaderAndFooterLayout(this, 33, 60)`, so `getContentHeight()` is
///   `480 - 33 - 60` = **387**, and the list is sized to exactly that
///   (`:61-62`). The content clamp is then `min(33 + 30, 480 - 60 - 387)` =
///   `min(63, 33)` = **33** — flush under the header, because the content
///   fills the band.
/// - `getFirstEntryY()` is `getY() + 2` = **35**, and rows stack by
///   `itemHeight` 36 with no gap.
/// - `getRowLeft()` is `0 + 854/2 - 305/2` = `427 - 152` = **275**. Note the
///   two halvings are separate integer divisions; `(854 - 305) / 2` is 274.
/// - `CONTENT_PADDING` insets the entry by 2 a side, so content is
///   `(277, 37, 301, 32)` and the 32 is exactly the favicon's height.
/// - `statusIconX = getContentRight() - 10 - 5` = `578 - 15` = **563**, at
///   `getContentY()` = 37 — the status icon is *not* vertically centred.
/// - The title is a 9 px `StringWidget` centred in the 854×33 header frame:
///   `round((33 - 9) / 2)` = **12** from the top, on `width / 2`.
/// - The footer column is `3*100 + 2*4` = 308 wide on its top row and
///   `4*74 + 3*4` = 308 on its lower one — they match, which is why the
///   column is 308 and both rows sit at its left edge — and `20 + 4 + 20` = 44
///   tall. Centred in the 854×60 footer frame pinned at y 420:
///   `((854 - 308) / 2, 420 + (60 - 44) / 2)` = **(273, 428)**.
#[test]
fn the_server_list_rects_are_vanillas_own() {
    use crate::menu::nav::{SERVER_LIST_BUTTONS, ServerListButton as B};

    let expected = [
        // Top row: 100 wide, 104 apart.
        (B::Select, (273.0, 428.0, 100.0, 20.0)),
        (B::Direct, (377.0, 428.0, 100.0, 20.0)),
        (B::Add, (481.0, 428.0, 100.0, 20.0)),
        // Lower row: 74 wide, 78 apart, 24 px below.
        (B::Edit, (273.0, 452.0, 74.0, 20.0)),
        (B::Delete, (351.0, 452.0, 74.0, 20.0)),
        (B::Refresh, (429.0, 452.0, 74.0, 20.0)),
        (B::Back, (507.0, 452.0, 74.0, 20.0)),
    ];
    for (button, want) in expected {
        assert_eq!(
            server_list_footer_slot(button).resolve(V_W, V_H),
            want,
            "{button:?} is not where vanilla puts it"
        );
        // The enum's declared width and the arranged one must agree, or the
        // footer was built with its two rows swapped.
        assert_eq!(
            server_list_footer_slot(button).w,
            button.width(),
            "{button:?}'s arranged width is not its declared one"
        );
    }
    // Both footer gutters are 4 — this screen's, not the pause screen's 8.
    let (sx, _, sw, _) = server_list_footer_slot(B::Select).resolve(V_W, V_H);
    let (dx, ..) = server_list_footer_slot(B::Direct).resolve(V_W, V_H);
    assert_eq!(dx - (sx + sw), 4.0, "top row spacing");
    let (ex, _, ew, _) = server_list_footer_slot(B::Edit).resolve(V_W, V_H);
    let (delx, ..) = server_list_footer_slot(B::Delete).resolve(V_W, V_H);
    assert_eq!(delx - (ex + ew), 4.0, "lower row spacing");
    assert_eq!(SERVER_LIST_BUTTONS.len(), 7);

    // The rows, unscrolled.
    assert_eq!(server_row_rect(0, V_W, 0.0), (275.0, 35.0, 305.0, 36.0));
    assert_eq!(
        server_row_rect(1, V_W, 0.0),
        (275.0, 71.0, 305.0, 36.0),
        "rows stack by itemHeight with no gap"
    );
    assert_eq!(
        server_row_content_rect(0, V_W, 0.0),
        (277.0, 37.0, 301.0, 32.0),
        "CONTENT_PADDING insets the entry by 2, and 36 - 4 is the icon's 32"
    );
    assert_eq!(server_entry_icon_rect(0, V_W, 0.0), (277.0, 37.0, 32.0, 32.0));
    assert_eq!(
        server_status_icon_rect(0, V_W, 0.0),
        (563.0, 37.0, 10.0, 8.0),
        "contentRight - 10 - 5, at contentY"
    );
    // A scroll of one whole row shifts every row up by one `itemHeight`
    // (#402): row 1 at scroll 0 lands exactly where row 0 sits at scroll 36.
    assert_eq!(
        server_row_rect(1, V_W, SERVER_LIST_ITEM_H),
        server_row_rect(0, V_W, 0.0),
        "scrolling by one row is the same shift as re-indexing by one row"
    );
    // #445: and a *half*-row scroll is expressible at all, which is the whole
    // conversion. 18 px is one wheel notch; the row lands 18 px above where
    // it started, not a whole entry above it and not nowhere.
    assert_eq!(
        server_row_top(0, SERVER_LIST_ITEM_H / 2.0),
        server_row_top(0, 0.0) - 18.0,
        "a one-notch offset moves the row by 18 px — the value a row index \
         could not represent"
    );
    // `getRowLeft()` is not `(width - rowWidth) / 2`, and the difference shows
    // at an odd canvas: 855/2 = 427 either way here, 856 is where they split.
    assert_eq!(server_row_left(856.0), 276.0, "floor(856/2) - 152");
    assert_eq!(
        (856.0 - SERVER_LIST_ROW_W) / 2.0,
        275.5,
        "control: the naive centring is half a pixel off"
    );

    // The title.
    let title = server_list_title_label();
    assert_eq!(title.text, crate::menu::nav::SERVER_LIST_TITLE);
    assert_eq!((title.dx, title.dy), (0.0, 12.0));
    assert_eq!(title.align, Align::Centre);
    assert_eq!(title.origin, Origin::ScreenTop);
}

/// The whole screen is arranged **once**, at a reference canvas, and every
/// rect is then expressed relative to an [`Origin`]. That is only sound if the
/// arrangement is canvas-independent once so expressed — so re-arrange at three
/// sizes and require identical slots.
///
/// This is what stands between the screen and being correct at 854×480 and
/// wrong everywhere else. It holds because the footer column measures 308 at
/// any width and the content band always starts at the header height (the list
/// is sized to `getContentHeight()`, so the clamp always picks it).
///
/// **Even widths only, and that is a real limit rather than a convenient
/// choice.** `Origin::ScreenBottom`'s x is `width * 0.5` unrounded, while
/// `FrameLayout` truncates its centring, so at an odd logical width the two
/// disagree by half a pixel — the same limit `Screen::WorldSelect`'s footer
/// has, for the same reason. It is invisible in practice because
/// `logical_canvas` divides the framebuffer by an integer scale and can
/// produce a fractional width anyway; the row geometry, which *is* floored
/// per-term, is exact at every width (see `server_row_left`).
#[test]
fn the_server_list_slots_do_not_depend_on_the_reference_canvas() {
    let reference = ServerListBlock::at(SERVER_LIST_REF_CANVAS.0, SERVER_LIST_REF_CANVAS.1);
    for (w, h) in [(320.0, 240.0), (1280.0, 720.0), (1920.0, 1080.0)] {
        let other = ServerListBlock::at(w, h);
        assert_eq!(
            other.content_top, reference.content_top,
            "the content band moved at {w}x{h}"
        );
        for i in 0..reference.footer.len() {
            assert_eq!(
                other.footer_slot(i),
                reference.footer_slot(i),
                "footer slot {i} moved at {w}x{h}"
            );
        }
        // And the slot really resolves to where that canvas' own arrangement
        // put it, which is the assertion that makes the two derivations
        // independent rather than merely equal to each other.
        for (i, want) in other.footer.iter().enumerate() {
            assert_eq!(
                reference.footer_slot(i).resolve(w, h),
                *want,
                "footer slot {i} does not land on {w}x{h}'s own arrangement"
            );
        }
    }
}

/// A nav sitting on the multiplayer screen with `servers` saved, reached the
/// way a player reaches it.
fn list_nav(tag: &str, servers: &[(&str, &str)]) -> (MenuNav, UiState) {
    let mut nav = test_nav(tag);
    let mut ui = UiState::new();
    ui.open_server_list();
    for (name, address) in servers {
        add_server(&mut nav, &mut ui, name, address);
    }
    assert_eq!(ui.screen(), Screen::ServerList, "premise: the list is up");
    assert_eq!(nav.list().len(), servers.len());
    (nav, ui)
}

/// The bounding box of every colour-stream vertex drawn in exactly `want`, in
/// logical pixels, or `None` if that colour never appeared.
///
/// Keyed on the **colour** rather than on a rect, because the thing under test
/// here is *where* a mark landed: a rect-shaped detector would need to know the
/// answer first. Reports a box, never a count, per `CLAUDE.md`.
fn colour_bounds(colour: &[f32], w: f32, h: f32, want: [f32; 4]) -> Option<(f32, f32, f32, f32)> {
    let (mut x0, mut y0) = (f32::MAX, f32::MAX);
    let (mut x1, mut y1) = (f32::MIN, f32::MIN);
    let mut seen = false;
    for v in colour.chunks_exact(STRIDE) {
        if (2..6).any(|c| (v[c] - want[c - 2]).abs() > 1e-4) {
            continue;
        }
        seen = true;
        let px = (v[0] + 1.0) * 0.5 * w;
        let py = (1.0 - v[1]) * 0.5 * h;
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px);
        y1 = y1.max(py);
    }
    seen.then_some((x0, y0, x1 - x0, y1 - y0))
}

/// #376's rule applied to this screen: the discriminator for a hover overlay
/// is **position**. A gate that proved "an overlay drew in a row" would pass
/// on an overlay nailed to row 0.
///
/// The measurement is the icon-dim quad (`fill(…, -1601138544)`), which is the
/// one part of the overlay that reaches the *colour* stream — the three arrow
/// sprites need an atlas, and they get their own gate below.
#[test]
fn the_hover_overlay_follows_the_cursor_rather_than_the_row() {
    let (nav, ui) = list_nav("hover", &[("A", "a.example"), ("B", "b.example")]);
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let mut f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

    let dim_at = |f: &MenuFrame<'_>| {
        colour_bounds(&geometry(f, V_W, V_H), V_W, V_H, SERVER_ICON_DARKEN)
    };
    // A tolerance, not `assert_eq!`: the measurement round-trips through NDC
    // and back (`2x/w - 1` then its inverse), so 277.0 comes out 277.00003.
    let is = |got: Option<(f32, f32, f32, f32)>, want: (f32, f32, f32, f32), what: &str| {
        let g = got.unwrap_or_else(|| panic!("{what}: nothing drew, expected {want:?}"));
        let near = (g.0 - want.0).abs() < 0.01
            && (g.1 - want.1).abs() < 0.01
            && (g.2 - want.2).abs() < 0.01
            && (g.3 - want.3).abs() < 0.01;
        assert!(near, "{what}: overlay at {g:?}, expected {want:?}");
    };

    // No cursor at all — a keyboard-only session, and every hermetic test.
    // This is also the control that makes the absences below real: if the
    // detector could not see the quad, every assertion here would pass on a
    // screen that never drew one.
    f.cursor = None;
    assert_eq!(dim_at(&f), None, "no cursor must mean no hover overlay");

    // Row 0, then row 1: the same overlay, one `itemHeight` lower.
    let icon0 = server_entry_icon_rect(0, V_W, 0.0);
    f.cursor = Some((icon0.0 + 4.0, icon0.1 + 4.0));
    is(dim_at(&f), icon0, "row 0's icon");
    let icon1 = server_entry_icon_rect(1, V_W, 0.0);
    f.cursor = Some((icon1.0 + 4.0, icon1.1 + 20.0));
    is(dim_at(&f), icon1, "row 1's icon");
    assert_eq!(
        icon1.1 - icon0.1,
        SERVER_LIST_ITEM_H,
        "premise: the two rows are a row apart, or this proves nothing"
    );

    // Vanilla's `hovered` is the *row*, not the icon: the cursor anywhere in
    // the row lights the icon up, and anywhere outside it does not.
    f.cursor = Some((icon0.0 + 200.0, icon0.1 + 4.0));
    is(dim_at(&f), icon0, "the whole row hovers");
    f.cursor = Some((10.0, 10.0));
    assert_eq!(dim_at(&f), None, "the backdrop is not a row");
}

/// Asserts `got` — a [`colour_bounds`] box — equals `want` within the NDC
/// round-trip's epsilon. A tolerance, not `assert_eq!`: the measurement
/// round-trips through NDC and back, so 555.0 comes out 555.00003.
fn assert_box(got: Option<(f32, f32, f32, f32)>, want: (f32, f32, f32, f32), what: &str) {
    let g = got.unwrap_or_else(|| panic!("{what}: nothing drew, expected {want:?}"));
    let near = (g.0 - want.0).abs() < 0.01
        && (g.1 - want.1).abs() < 0.01
        && (g.2 - want.2).abs() < 0.01
        && (g.3 - want.3).abs() < 0.01;
    assert!(near, "{what}: got {g:?}, expected {want:?}");
}

/// A cache of `Ok` statuses, one per entry in `nav`'s list, each spec
/// `(host, players, sample, online)` seeded so its row resolves to
/// `ServerState::Successful` — the state vanilla shows the "who's online"
/// tooltip for (`ServerSelectionList.java:410,430`).
fn ok_statuses(
    nav: &MenuNav,
    specs: &[(&str, &str, &[&str], Option<u32>)],
) -> StatusCache {
    // The probe outlives the caller's `specs` (`Probe` is a `'static` `dyn`), so
    // the specs are copied into owned data the closure can hold without borrowing
    // the test's locals.
    let owned: Vec<(String, String, Vec<String>, Option<u32>)> = specs
        .iter()
        .map(|(host, players, sample, online)| {
            (
                host.to_string(),
                players.to_string(),
                sample.iter().map(|s| s.to_string()).collect(),
                *online,
            )
        })
        .collect();
    let mut statuses = StatusCache::with_probe(std::sync::Arc::new(move |e: &ServerEntry| {
        let (_, players, sample, online) = owned
            .iter()
            .find(|(host, ..)| host == &e.host)
            .expect("the probe only ever sees an entry the test added");
        Ok(ServerStatus {
            motd: "hi".into(),
            motd_spans: Vec::new(),
            players: players.clone(),
            online: *online,
            sample: sample.clone(),
            version: "26.2".into(),
            protocol: Some(crate::menu::status::STATUS_PROTOCOL),
            favicon_png: None,
            latency_ms: Some(5),
        })
    }));
    let entries = nav.list().entries().to_vec();
    statuses.refresh(&entries);
    let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
    while statuses.pump() == 0 && crate::platform::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(statuses.len(), entries.len(), "every entry must drain");
    statuses
}

/// #421's "who's online" tooltip, from the frame side and the draw side
/// together: `server_list_frame` shapes the lines from the sample — vanilla's
/// `... and N more ...` when the sample is short of the count — and
/// `draw_server_entry` only shows the box when the cursor is over the status
/// *text* (the player count), not over the row (`ServerSelectionList.java:356-361`).
#[test]
fn the_who_is_online_tooltip_lists_the_sample_and_tracks_the_status_text() {
    let (nav, ui) = list_nav("who", &[("A", "a.example"), ("B", "b.example")]);
    let statuses = ok_statuses(
        &nav,
        &[
            ("a.example", "5/20", &["Alice", "Bob"], Some(5)),
            // A server that omits the sample: legal and common, and vanilla's
            // `else { data.playerList = List.of() }` (`ServerStatusPinger.java:109`)
            // gives it no tooltip.
            ("b.example", "1/20", &[], Some(1)),
        ],
    );
    let mut fav = FaviconCache::new();
    let mut f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

    // The frame resolves the lines once per status, exactly as
    // `ServerStatusPinger` builds `data.playerList` (`:90-110`): the two named
    // players, then the and-more line for the unnamed three.
    let a = f.rows[0].entry.as_ref().expect("row 0 is an entry");
    assert_eq!(
        a.online_players,
        ["Alice", "Bob", "... and 3 more ..."],
        "2 of 5 named must carry vanilla's `... and 3 more ...`"
    );
    assert!(
        f.rows[1].entry.as_ref().unwrap().online_players.is_empty(),
        "an empty sample is an empty tooltip"
    );

    let fill_at = |f: &MenuFrame<'_>| colour_bounds(&geometry(f, V_W, V_H), V_W, V_H, TOOLTIP_BG);

    // The status text is right-aligned to its status icon
    // (`status_x = icon_x - width - spacing`), and the box lands by
    // `DefaultTooltipPositioner` — content at the cursor + (12, -12), here with
    // no edge to clamp — so the fill is `(rx - 3, ry - 3, w + 6, h + 6)`.
    let (icon_x, ..) = server_status_icon_rect(0, V_W, 0.0);
    let (_, cy, ..) = server_row_content_rect(0, V_W, 0.0);
    let status_x = icon_x - text_px("5/20", 1.0) - SERVER_ENTRY_SPACING;
    assert_eq!((status_x, cy), (534.0, 37.0), "premise: the cursor lands on the text");
    f.cursor = Some((status_x + 12.0, cy + 4.0));
    // Width 108 is `"... and 3 more ..."` (18 chars at the fixed 6 px advance);
    // height 30 is three 10 px tooltip lines; fill is the 3 px pad on each side.
    assert_box(
        fill_at(&f),
        (555.0, 26.0, 114.0, 36.0),
        "row 0's tooltip",
    );

    // Inside the row but off the status column: no tooltip — vanilla fires it
    // over the text only, never over the row.
    f.cursor = Some((status_x - 40.0, cy + 4.0));
    assert_eq!(fill_at(&f), None, "inside the row, off the status text");

    // Row 1's status text, over an empty sample: still no tooltip.
    let (_, cy1, ..) = server_row_content_rect(1, V_W, 0.0);
    let sx1 = icon_x - text_px("1/20", 1.0) - SERVER_ENTRY_SPACING;
    f.cursor = Some((sx1 + 12.0, cy1 + 4.0));
    assert_eq!(fill_at(&f), None, "an empty sample draws no tooltip");

    // No cursor — the keyboard-only control (and every hermetic test's default).
    f.cursor = None;
    assert_eq!(fill_at(&f), None, "no cursor means no tooltip");
}

/// The tooltip is drawn **after** the band-clipped rows, so a tooltip for the
/// first row — whose top necessarily reaches above the band, because the status
/// text sits a few pixels below the band's top edge — is not scissored off. A
/// tooltip clipped to the band would measure a box whose top edge is the band's,
/// not the predicted `ry - 3`; asserting the exact unclipped value is what tells
/// "escaped the clip" from "nothing drew".
#[test]
fn the_who_is_online_tooltip_escapes_the_band_clip() {
    let (nav, ui) = list_nav("clip", &[("A", "a.example")]);
    let statuses = ok_statuses(&nav, &[("a.example", "5/20", &["Alice", "Bob"], Some(5))]);
    let mut fav = FaviconCache::new();
    let mut f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

    let (icon_x, ..) = server_status_icon_rect(0, V_W, 0.0);
    let (_, cy, ..) = server_row_content_rect(0, V_W, 0.0);
    let status_x = icon_x - text_px("5/20", 1.0) - SERVER_ENTRY_SPACING;
    // The top of the status text box (`mouseY == getContentY()` is inclusive in
    // vanilla), the highest this row can push the tooltip.
    f.cursor = Some((status_x + 12.0, cy));
    let got = colour_bounds(&geometry(&f, V_W, V_H), V_W, V_H, TOOLTIP_BG)
        .expect("the tooltip must draw");
    assert_box(Some(got), (555.0, 22.0, 114.0, 36.0), "the unclipped fill");

    let band_top = f.list.as_ref().unwrap().model(V_H).unwrap().top();
    assert!(
        got.1 < band_top,
        "the fill's top ({}) must reach above the band ({band_top}), or the \
         clip would have cut the tooltip",
        got.1
    );
}

/// A synthetic pack carrying the `server_list/*` sprites plus the button set,
/// so sprite *identity* can be asserted with no jar — `button_pack`'s trick.
fn server_list_pack() -> lodestone_assets::ResourceManager {
    use crate::menu::status::{PING_SPRITES, PINGING_SPRITES};
    use lodestone_assets::{MemorySource, ResourceSource};
    let mut src = MemorySource::default();
    for (id, border) in [
        ("widget/button", 3u32),
        ("widget/button_highlighted", 3),
        ("widget/button_disabled", 1),
    ] {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_rgba_png(200, 20, [10, 20, 30, 255]),
        );
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png.mcmeta"),
            format!(
                r#"{{"gui":{{"scaling":{{"type":"nine_slice","width":200,"height":20,"border":{border}}}}}}}"#
            )
            .into_bytes(),
        );
    }
    // Every status sprite at vanilla's own 10×8, and the three 32×32 overlays.
    for id in PING_SPRITES.iter().chain(PINGING_SPRITES.iter()).chain([
        &crate::menu::status::INCOMPATIBLE_SPRITE,
        &crate::menu::status::UNREACHABLE_SPRITE,
    ]) {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_rgba_png(10, 8, [40, 90, 200, 255]),
        );
    }
    for (a, b) in [
        SERVER_JOIN_SPRITES,
        SERVER_MOVE_UP_SPRITES,
        SERVER_MOVE_DOWN_SPRITES,
    ] {
        for id in [a, b] {
            src.insert(
                format!("assets/minecraft/textures/gui/sprites/{id}.png"),
                solid_rgba_png(32, 32, [200, 40, 90, 255]),
            );
        }
    }
    // The favicon fallback is a **loose** texture, so it arrives through the
    // extras list rather than the sprite glob — the same path the logo takes.
    src.insert(
        crate::resources::UNKNOWN_SERVER_TEXTURE.1,
        solid_rgba_png(32, 32, [70, 70, 70, 255]),
    );
    lodestone_assets::ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>])
}

/// The atlas the two sprite gates below sample against.
fn server_list_atlas() -> GuiAtlas {
    GuiAtlas::build_with_extras(
        &server_list_pack(),
        &[crate::resources::UNKNOWN_SERVER_TEXTURE],
    )
    .expect("synthetic atlas builds")
}

/// Whether any whole **quad** on the sprite stream samples inside `id`'s atlas
/// region.
///
/// `all_uvs_within`'s companion, and needed because the hover overlay blits
/// **three** sprites into the same 32×32 rect: "every UV is inside join" is
/// false by construction there, while "some quad is inside join_highlighted and
/// none is inside join" is exactly the question.
///
/// A *quad* rather than a vertex, and that is not fussiness: the packer may
/// place two sprites edge to edge, and a vertex exactly on the shared edge is
/// inside both regions to within any epsilon. A whole quad can only be inside
/// one of two equal-sized regions.
fn any_quad_within(sprite: &[f32], min: [f32; 2], max: [f32; 2]) -> bool {
    sprite
        .chunks_exact(SPRITE_FLOATS_PER_VERTEX * 6)
        .any(|quad| all_uvs_within(quad, min, max))
}

/// Every sprite-stream UV whose **destination** falls inside `rect`.
///
/// The pair of questions together — where it landed and which region it
/// sampled — is what makes a per-widget assertion possible on a stream that
/// carries every sprite on the screen at once.
fn uvs_in_dest(sprite: &[f32], w: f32, h: f32, rect: (f32, f32, f32, f32)) -> Vec<[f32; 2]> {
    let (rx, ry, rw, rh) = rect;
    sprite
        .chunks_exact(SPRITE_FLOATS_PER_VERTEX)
        .filter(|v| {
            let px = (v[0] + 1.0) * 0.5 * w;
            let py = (1.0 - v[1]) * 0.5 * h;
            px >= rx - 0.01 && px <= rx + rw + 0.01 && py >= ry - 0.01 && py <= ry + rh + 0.01
        })
        .map(|v| [v[2], v[3]])
        .collect()
}

/// The quadrant under the cursor decides which of the three overlay sprites is
/// drawn **highlighted**, and the other two must stay plain. All three blit
/// into the same rect, so this is asserted by atlas region rather than by
/// position — position is what the previous gate covers.
#[test]
fn each_hovered_icon_quadrant_highlights_its_own_sprite() {
    let atlas = server_list_atlas();
    let (nav, ui) = list_nav(
        "quadrants",
        &[("A", "a.example"), ("B", "b.example"), ("C", "c.example")],
    );
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let mut f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

    let region = |id: &str| sprite_uv_bounds(&atlas, id);
    let regions = [
        SERVER_JOIN_SPRITES,
        SERVER_MOVE_UP_SPRITES,
        SERVER_MOVE_DOWN_SPRITES,
    ];
    // The six regions must be disjoint, or "sampled inside X" proves nothing.
    let all: Vec<([f32; 2], [f32; 2])> = regions
        .into_iter()
        .flat_map(|(a, b)| [region(a), region(b)])
        .collect();
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            let (a, b) = (all[i], all[j]);
            assert!(
                a.1[0] <= b.0[0] || b.1[0] <= a.0[0] || a.1[1] <= b.0[1] || b.1[1] <= a.0[1],
                "two overlay sprites share atlas space: {a:?} {b:?}"
            );
        }
    }

    // Row 1 of three, so both move arrows apply.
    let (ix, iy, iw, ih) = server_entry_icon_rect(1, V_W, 0.0);
    let cases = [
        // (cursor, which of the three is highlighted)
        ((ix + iw * 0.75, iy + ih * 0.5), 0usize),
        ((ix + 4.0, iy + 4.0), 1),
        ((ix + 4.0, iy + ih - 4.0), 2),
    ];
    for ((mx, my), highlighted) in cases {
        f.cursor = Some((mx, my));
        let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;
        for (which, (plain, hot)) in regions.into_iter().enumerate() {
            let (p, hgt) = (region(plain), region(hot));
            if which == highlighted {
                assert!(
                    any_quad_within(&sprite, hgt.0, hgt.1),
                    "cursor ({mx}, {my}) must highlight {hot}"
                );
                assert!(
                    !any_quad_within(&sprite, p.0, p.1),
                    "and must not also draw the plain {plain}"
                );
            } else {
                assert!(
                    any_quad_within(&sprite, p.0, p.1),
                    "cursor ({mx}, {my}) must still draw the plain {plain}"
                );
                assert!(
                    !any_quad_within(&sprite, hgt.0, hgt.1),
                    "and must not highlight {hot}"
                );
            }
        }
    }

    // Row 0 has nowhere to move up to, so its arrow must not be drawn at all —
    // vanilla's `if (index > 0)` guard (`ServerSelectionList.java:375`).
    let (ix0, iy0, iw0, ih0) = server_entry_icon_rect(0, V_W, 0.0);
    f.cursor = Some((ix0 + 4.0, iy0 + 4.0));
    let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;
    let up = region(SERVER_MOVE_UP_SPRITES.0);
    let up_hot = region(SERVER_MOVE_UP_SPRITES.1);
    assert!(
        !any_quad_within(&sprite, up.0, up.1) && !any_quad_within(&sprite, up_hot.0, up_hot.1),
        "row 0 must draw no move-up arrow"
    );
    let down = region(SERVER_MOVE_DOWN_SPRITES.0);
    assert!(
        any_quad_within(&sprite, down.0, down.1),
        "control: its move-down arrow is there, so the detector works"
    );
    // And with no cursor, none of the six is drawn.
    f.cursor = None;
    let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;
    for (plain, hot) in regions {
        let (p, hgt) = (region(plain), region(hot));
        assert!(!any_quad_within(&sprite, p.0, p.1), "{plain} without a cursor");
        assert!(!any_quad_within(&sprite, hgt.0, hgt.1), "{hot} without a cursor");
    }
}

/// The status sprite is asserted **by identity through the atlas**, at the rect
/// vanilla puts it at: a gate that only proved a ping bar exists passes on all
/// four states rendering the same bar.
///
/// Also the footer's disabled path, per button, by the same joint test — where
/// it landed *and* which region it sampled. The expected sprite comes from
/// `WidgetSprites::get`, never spelled out.
#[test]
fn the_status_sprite_and_the_disabled_footer_sample_the_sprites_they_should() {
    use crate::menu::nav::{SERVER_LIST_BUTTONS, ServerListButton as B};
    use crate::menu::status::{PING_SPRITES, ServerStatus};

    let atlas = server_list_atlas();
    let (mut nav, mut ui) = list_nav("sprites", &[]);
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();

    // Empty list: Join / Edit / Delete / Direct all draw `button_disabled`,
    // each at its own rect, and the other three draw `button`.
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let stream = build(&f, Some(&atlas), None, V_W, V_H).sprite;
    let check = |stream: &[f32], button: B, enabled: bool| {
        let want = widget::BUTTON_SPRITES.get(enabled, false);
        let (min, max) = sprite_uv_bounds(&atlas, want);
        let rect = server_list_footer_slot(button).resolve(V_W, V_H);
        let uvs = uvs_in_dest(stream, V_W, V_H, rect);
        assert!(!uvs.is_empty(), "{button:?} drew nothing at {rect:?}");
        assert!(
            uvs.iter().all(|uv| {
                uv[0] >= min[0] - 1e-6
                    && uv[0] <= max[0] + 1e-6
                    && uv[1] >= min[1] - 1e-6
                    && uv[1] <= max[1] + 1e-6
            }),
            "{button:?} did not sample {want} (enabled={enabled})"
        );
    };
    for button in SERVER_LIST_BUTTONS {
        check(&stream, button, button.enabled(false));
    }

    // Control, executed: a saved server flips three of them, so the assertion
    // above measures the selection and not a screen that is always disabled.
    add_server(&mut nav, &mut ui, "HOME", "mc.example.com");
    let mut statuses = StatusCache::with_probe(std::sync::Arc::new(|_| {
        Ok(ServerStatus {
            motd: "hello".into(),
            players: "2/8".into(),
            protocol: Some(crate::menu::status::STATUS_PROTOCOL),
            latency_ms: Some(400),
            ..Default::default()
        })
    }));
    let entries = nav.list().entries().to_vec();
    statuses.refresh(&entries);
    let deadline = crate::platform::Instant::now() + std::time::Duration::from_secs(5);
    while statuses.pump() == 0 && crate::platform::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let stream = build(&f, Some(&atlas), None, V_W, V_H).sprite;
    for button in SERVER_LIST_BUTTONS {
        check(&stream, button, button.enabled(true));
    }
    for b in [B::Select, B::Edit, B::Delete] {
        assert!(b.enabled(true) && !b.enabled(false), "control premise: {b:?}");
    }

    // 400 ms is the middle bucket. Asserted at the status icon's own rect, so
    // this is both "the right sprite" and "in the right place".
    let rect = server_status_icon_rect(0, V_W, 0.0);
    let uvs = uvs_in_dest(&stream, V_W, V_H, rect);
    assert!(!uvs.is_empty(), "no status sprite at {rect:?}");
    let (min, max) = sprite_uv_bounds(&atlas, PING_SPRITES[2]);
    assert!(
        uvs.iter().all(|uv| {
            uv[0] >= min[0] - 1e-6
                && uv[0] <= max[0] + 1e-6
                && uv[1] >= min[1] - 1e-6
                && uv[1] <= max[1] + 1e-6
        }),
        "400 ms must sample {} — three bars",
        PING_SPRITES[2]
    );
    // Control: it is not sampling a *different* bucket's sprite, which is what
    // "some ping bar drew" would have accepted.
    let (fmin, fmax) = sprite_uv_bounds(&atlas, PING_SPRITES[4]);
    assert!(
        !uvs
            .iter()
            .all(|uv| uv[0] >= fmin[0] - 1e-6 && uv[0] <= fmax[0] + 1e-6
                && uv[1] >= fmin[1] - 1e-6
                && uv[1] <= fmax[1] + 1e-6),
        "the detector cannot tell ping_3 from ping_5"
    );
}

#[test]
fn the_error_screen_carries_the_disconnect_reason() {
    // Since `error_frame`'s conversion onto the framework, the reason
    // lives in `notice` (a wrapped, bounded `MenuNotice`, like the
    // account screen's failure message) rather than `message` — a
    // `vanilla` frame suppresses `message` entirely (see `MenuNotice`'s
    // own doc on why an unwrapped line was the bug this pattern fixes).
    let nav = test_nav("err");
    let mut ui = UiState::new();
    ui.begin(SessionKind::Multiplayer);
    ui.session_failed(crate::sim::SessionEnd::disconnected(
            lodestone_model::Text::literal("Server closed"),
        ));
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    assert!(f.vanilla, "the disconnect screen is on the framework now");
    assert!(f.message.is_none(), "a vanilla frame draws no `message`");
    let notice = f.notice.expect("the reason must reach the screen");
    assert!(notice.text.contains("Server closed"), "{}", notice.text);
    assert_eq!(
        f.rows[0].label, "Back to Title Screen",
        "vanilla's gui.toTitle, since dismiss_error always returns to MainMenu"
    );
    assert!(f.rows[0].slot.is_some(), "the button is vanilla-placed now");
}

#[test]
fn a_favicon_is_decoded_once_not_once_per_frame() {
    // 60 zlib inflations per second per row is the bug this prevents.
    let png = solid_png(8, [1, 2, 3, 255]);
    let mut fav = FaviconCache::new();
    assert!(fav.is_empty());
    let first = fav.get("a.example:25565", &png);
    assert!(first.is_some());
    assert_eq!(fav.len(), 1);
    for _ in 0..100 {
        assert_eq!(fav.get("a.example:25565", &png), first);
    }
    assert_eq!(fav.len(), 1, "one entry per address, whatever the frame count");

    // A failed decode is cached too, or a broken icon retries forever.
    assert!(fav.get("b.example:25565", b"not a png").is_none());
    assert_eq!(fav.len(), 2);
    fav.forget("b.example:25565");
    assert_eq!(fav.len(), 1);
}

/// What reached one rectangle of the colour stream: how many vertices, and
/// **where**.
///
/// A box rather than a fraction, per `CLAUDE.md`: a gate that reports only a
/// count cannot tell a shifted widget from a missing one, and both of the
/// control-premise failures recorded there were diagnosed by printing a
/// bounding box instead of a percentage.
#[derive(Debug)]
struct BandCoverage {
    count: usize,
    /// `(x0, y0, x1, y1)` in logical pixels, or `None` when nothing reached.
    bounds: Option<(f32, f32, f32, f32)>,
}

/// Colour-stream vertices inside `band`, in logical pixels — the inverse of
/// `Quads::rect`'s `(2x/w - 1, 1 - 2y/h)`.
///
/// **Strict on y, inclusive on x**, and the asymmetry is the whole reason
/// this reads a *band* rather than the field rect. `CLAUDE.md`'s rule is to
/// ask what else already paints here; the answer is the field's own chrome:
///
/// - its background fill and its focus outline's left/right edges sit at the
///   field's outer `x`, which is `BORDER_INSET` outside the band's — so the
///   horizontal test can be inclusive and still exclude them, which keeps the
///   caret's own left edge (exactly at `text_x`) counted;
/// - its outline's **bottom** edge, though, lands *inside* the band's
///   vertical extent while spanning the full field width. Only a strict `y`
///   keeps it out, and an inclusive one would report a bounding box the width
///   of the whole field whatever the value was — a control that fires while
///   measuring something unrelated.
fn band_coverage(
    colour: &[f32],
    w: f32,
    h: f32,
    band: (f32, f32, f32, f32),
) -> BandCoverage {
    let (bx, by, bw, bh) = band;
    let mut count = 0;
    let (mut x0, mut y0) = (f32::MAX, f32::MAX);
    let (mut x1, mut y1) = (f32::MIN, f32::MIN);
    for v in colour.chunks_exact(STRIDE) {
        let px = (v[0] + 1.0) * 0.5 * w;
        let py = (1.0 - v[1]) * 0.5 * h;
        if px >= bx - 0.01 && px <= bx + bw + 0.01 && py > by && py < by + bh {
            count += 1;
            x0 = x0.min(px);
            y0 = y0.min(py);
            x1 = x1.max(px);
            y1 = y1.max(py);
        }
    }
    BandCoverage {
        count,
        bounds: (count > 0).then_some((x0, y0, x1, y1)),
    }
}

/// #395's pixel gate: a real `EditBox` on a real screen, measured **inside
/// its own rect**, with the caret at two different positions.
///
/// Every bound here is derived from the widget rather than restated: the rect
/// comes from [`field_rect`] (the same function the draw calls) and the text
/// band from a clone of the live box repositioned into it, so the gate cannot
/// pass by agreeing with a constant that the draw does not use.
#[test]
fn the_edit_box_draws_its_text_and_its_caret_inside_its_own_rect() {
    const W: f32 = 854.0;
    const H: f32 = 480.0;
    let mut nav = test_nav("editbox-pixels");
    let mut ui = UiState::new();
    ui.open_server_list();
    nav.key(&mut ui, MenuKey::Char('a'));
    assert_eq!(ui.screen(), Screen::ServerEdit, "premise: the form is open");
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();

    // The widget as the draw sees it: a clone of the live box moved into this
    // frame's rect, exactly as `draw_edit_box` does it.
    let probe_of = |frame: &MenuFrame<'_>| -> EditBox {
        let rect = field_rect(&frame.rows, 0, W, H).expect("row 0 is the name field");
        let mut probe = frame.rows[0]
            .edit
            .clone()
            .expect("the name row must carry its EditBox, or nothing draws");
        probe.widget.x = rect.0;
        probe.widget.y = rect.1;
        probe.widget.width = rect.2;
        probe.widget.height = rect.3;
        probe
    };
    let band_of = |probe: &EditBox| -> (f32, f32, f32, f32) {
        (
            probe.text_x(),
            probe.text_y(),
            probe.inner_width(),
            // `draw_edit_box` draws at `EDIT_TEXT_SCALE`, not the
            // ordinary-row `TEXT_SCALE` — see that constant's doc for the
            // player report this measurement band would otherwise have
            // missed by 2×.
            GLYPH_H as f32 * EDIT_TEXT_SCALE,
        )
    };

    // The control, executed rather than described: an empty focused field
    // paints its caret and nothing else. If this were zero the band would be
    // pointing somewhere nothing draws and every measurement below would be of
    // the wrong rectangle.
    let empty = frame_for(&ui, &nav, &statuses, &mut fav).expect("the form owns its frame");
    let band = band_of(&probe_of(&empty));
    let blank = band_coverage(&geometry(&empty, W, H), W, H, band);
    assert!(
        blank.count > 0,
        "premise: a focused empty field paints a caret inside {band:?}, found \
         nothing — the band is in the wrong place"
    );
    let (_, blank_y0, _, blank_y1) = blank.bounds.unwrap();
    assert!(
        blank_y1 - blank_y0 < 4.0,
        "premise: with no value the band holds only the caret, so its vertical \
         extent is a bar and not a line of glyphs; got {}",
        blank_y1 - blank_y0
    );

    for c in "mc.example.com".chars() {
        nav.key(&mut ui, MenuKey::Char(c));
    }
    let typed = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let probe = probe_of(&typed);
    assert_eq!(
        band_of(&probe),
        band,
        "the field must not move between frames, or the two measurements are \
         of different rectangles"
    );
    let full = band_coverage(&geometry(&typed, W, H), W, H, band);
    assert!(
        full.count > blank.count * 8,
        "typing must paint glyphs inside the field: empty {blank:?}, typed {full:?}"
    );
    let (x0, y0, x1, y1) = full.bounds.expect("checked non-empty above");
    // The band is the *counting window*, so "it is inside the band" would be
    // vacuous. The claim is that what was painted matches the widget's **own**
    // arithmetic: the leftmost pixel is the box's `text_x` and the rightmost is
    // its caret's right edge. Both are read off the widget, never restated —
    // a draw that used the row's `PAD` (6) instead of `BORDER_INSET` (4) would
    // land two pixels out and fail here.
    let state = probe.draw_state(None);
    assert!(
        (x0 - probe.text_x()).abs() <= 0.5,
        "the value must start at the box's own text_x {}, painted from {x0}",
        probe.text_x()
    );
    assert!(
        (x1 - (state.cursor_x + probe.advance)).abs() <= 0.5,
        "the rightmost pixel must be the caret's right edge {}, painted to {x1} \
         (bounds ({x0}, {y0})..({x1}, {y1}))",
        state.cursor_x + probe.advance
    );
    assert!(
        // Margin kept proportional after the `EDIT_TEXT_SCALE` fix: the
        // blank-caret premise above requires under 4 px, so this bound
        // (5 px, i.e. `7 - 2`) still separates "just the caret bar" from
        // "a full line of glyphs" at the new, smaller scale.
        y1 - y0 >= GLYPH_H as f32 * EDIT_TEXT_SCALE - 2.0,
        "a full line of glyphs must be present, not just the caret bar: the \
         band's vertical extent is only {}",
        y1 - y0
    );

    // The caret at two positions: one Backspace and the rightmost painted
    // pixel in the band must retreat by about one character — not by nothing
    // (a frozen caret) and not by the whole field (a re-laid-out one).
    nav.key(&mut ui, MenuKey::Backspace);
    let shorter = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let after = band_coverage(&geometry(&shorter, W, H), W, H, band);
    let (_, _, x1_after, _) = after.bounds.expect("still drawing");
    let advance = probe.advance;
    assert!(
        x1_after < x1 - 1.0,
        "the caret must move left with the text: {x1} -> {x1_after}"
    );
    assert!(
        x1 - x1_after <= advance * 1.5,
        "one Backspace moved the right edge by {}, which is more than one \
         character ({advance} px)",
        x1 - x1_after
    );
    // And it landed on the shorter value's own caret, not just somewhere left.
    let shorter_probe = probe_of(&shorter);
    let shorter_state = shorter_probe.draw_state(None);
    assert!(
        (x1_after - (shorter_state.cursor_x + shorter_probe.advance)).abs() <= 0.5,
        "expected the caret's right edge at {}, painted to {x1_after}",
        shorter_state.cursor_x + shorter_probe.advance
    );
}

#[test]
fn the_edit_form_shows_both_fields_and_marks_the_focused_one() {
    use crate::menu::nav::{ADDRESS_FIELD, CANCEL_ROW, DONE_ROW, NAME_FIELD, RESOURCE_PACK_ROW};
    let mut nav = test_nav("form");
    let mut ui = UiState::new();
    ui.open_server_list();
    nav.key(&mut ui, MenuKey::Char('a'));
    for c in "abc".chars() {
        nav.key(&mut ui, MenuKey::Char(c));
    }
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    // Two text fields plus the framework conversion's three button rows.
    assert_eq!(f.rows.len(), 5);
    assert!(f.vanilla, "the framework conversion sets `vanilla`");
    assert!(f.rows[NAME_FIELD].field, "row 0 is a text field");
    assert!(f.rows[ADDRESS_FIELD].field, "row 1 is a text field");
    assert!(!f.rows[RESOURCE_PACK_ROW].field, "row 2 is a button, not text");
    assert_eq!(f.rows[NAME_FIELD].label, "abc");
    assert_eq!(f.selected, NAME_FIELD, "the name field has focus");
    // Vanilla disables Done rather than printing a message
    // (`ManageServerScreen.java:92-93`) — see `error_frame`'s sibling note
    // on why a `vanilla` frame's `message` is unused, and this screen's own
    // arm on why no extra label duplicates the disabled sprite.
    assert!(f.message.is_none(), "a vanilla frame draws no `message`");
    assert!(
        !f.rows[DONE_ROW].enabled,
        "an addressless form must not offer a working Done button"
    );
    assert!(f.rows[CANCEL_ROW].enabled, "Cancel always works");
    assert!(!f.rows[RESOURCE_PACK_ROW].enabled, "present, but inactive");
    for row in [NAME_FIELD, ADDRESS_FIELD, RESOURCE_PACK_ROW, DONE_ROW, CANCEL_ROW] {
        assert!(f.rows[row].slot.is_some(), "row {row} must be vanilla-placed");
    }

    nav.key(&mut ui, MenuKey::Tab);
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    assert_eq!(f.selected, ADDRESS_FIELD, "Tab moves focus to the address");
}

fn frame_with(rows: Vec<MenuRow>, selected: usize) -> MenuFrame<'static> {
    MenuFrame {
        title: "LODESTONE",
        subtitle: "",
        rows,
        selected,
        footer: vec![],
        message: None,
        gui_scale: 0,
        backdrop: MenuBackdrop::Panorama,
        ..Default::default()
    }
}

fn button(label: &str) -> MenuRow {
    MenuRow {
        label: label.to_string(),
        enabled: true,
        ..Default::default()
    }
}

/// Fraction of sample points inside the pixel rect `(x, y, w, h)` that any
/// emitted quad covers with a colour other than the background.
///
/// This is the coverage measurement the repo's rules call for: it asks
/// *where* pixels landed, not how many vertices came out, so a layout bug
/// that draws everything off-screen fails it.
fn coverage(verts: &[f32], w: f32, h: f32, rect: (f32, f32, f32, f32)) -> f32 {
    let (rx, ry, rw, rh) = rect;
    const N: usize = 24;
    let mut hit = 0usize;
    for iy in 0..N {
        for ix in 0..N {
            let px = rx + rw * (ix as f32 + 0.5) / N as f32;
            let py = ry + rh * (iy as f32 + 0.5) / N as f32;
            // NDC of this sample.
            let nx = 2.0 * px / w - 1.0;
            let ny = 1.0 - 2.0 * py / h;
            if covered(verts, nx, ny) {
                hit += 1;
            }
        }
    }
    hit as f32 / (N * N) as f32
}

/// Whether any emitted quad other than the full-screen background covers
/// NDC point `(nx, ny)`. Quads are axis-aligned pairs of triangles, so the
/// first and fifth vertex of each six give the corners.
fn covered(verts: &[f32], nx: f32, ny: f32) -> bool {
    verts
        .chunks_exact(STRIDE * 6)
        .skip(1) // vertex 0..6 is the background clear rect
        .any(|q| {
            let (x0, y0) = (q[0], q[1]);
            let (x1, y1) = (q[STRIDE * 4], q[STRIDE * 4 + 1]);
            let (lo_x, hi_x) = (x0.min(x1), x0.max(x1));
            let (lo_y, hi_y) = (y0.min(y1), y0.max(y1));
            nx >= lo_x && nx <= hi_x && ny >= lo_y && ny <= hi_y
        })
}

/// The colour of the *last* (i.e. topmost-painted) quad covering NDC
/// point `(nx, ny)`, or `None` if only the background is there.
///
/// Unlike `covered`, which only answers "is anything here", this can
/// tell a row's own fill (`ROW_BG`/`ROW_SEL`) apart from a border drawn
/// on top of it in a different colour — necessary because the fill
/// quad already covers every pixel the border does, so presence alone
/// cannot distinguish "outlined" from "an ordinary row". Quads are
/// pushed in paint order, so the last one in the buffer that covers the
/// point is the one actually visible there.
fn colour_at(verts: &[f32], nx: f32, ny: f32) -> Option<[f32; 4]> {
    verts
        .chunks_exact(STRIDE * 6)
        .skip(1) // vertex 0..6 is the background clear rect
        .filter(|q| {
            let (x0, y0) = (q[0], q[1]);
            let (x1, y1) = (q[STRIDE * 4], q[STRIDE * 4 + 1]);
            let (lo_x, hi_x) = (x0.min(x1), x0.max(x1));
            let (lo_y, hi_y) = (y0.min(y1), y0.max(y1));
            nx >= lo_x && nx <= hi_x && ny >= lo_y && ny <= hi_y
        })
        .last()
        .map(|q| [q[2], q[3], q[4], q[5]])
}

/// Fraction of sample points inside `(x, y, w, h)` whose topmost quad is
/// (approximately) `colour` — see `colour_at`. Where `coverage`'s
/// colour-blind "is anything here" cannot separate a highlight border
/// from the row fill it is painted over, this can.
fn coverage_of(
    verts: &[f32],
    w: f32,
    h: f32,
    rect: (f32, f32, f32, f32),
    colour: [f32; 4],
) -> f32 {
    let (rx, ry, rw, rh) = rect;
    const N: usize = 24;
    let mut hit = 0usize;
    for iy in 0..N {
        for ix in 0..N {
            let px = rx + rw * (ix as f32 + 0.5) / N as f32;
            let py = ry + rh * (iy as f32 + 0.5) / N as f32;
            let nx = 2.0 * px / w - 1.0;
            let ny = 1.0 - 2.0 * py / h;
            let matches = colour_at(verts, nx, ny)
                .is_some_and(|c| c.iter().zip(colour).all(|(a, b)| (a - b).abs() < 0.01));
            if matches {
                hit += 1;
            }
        }
    }
    hit as f32 / (N * N) as f32
}

#[test]
fn every_vertex_lands_inside_the_viewport() {
    // The island's favourite disguise: geometry that exists and is drawn
    // entirely off-screen.
    let f = frame_with(
        vec![button("SINGLEPLAYER"), button("MULTIPLAYER"), button("QUIT")],
        1,
    );
    let v = geometry(&f, 1280.0, 720.0);
    assert!(!v.is_empty(), "a menu with rows must emit geometry");
    assert_eq!(v.len() % STRIDE, 0);
    for vert in v.chunks_exact(STRIDE) {
        assert!(
            (-1.001..=1.001).contains(&vert[0]) && (-1.001..=1.001).contains(&vert[1]),
            "vertex outside NDC: {:?}",
            &vert[..2]
        );
    }
}

#[test]
fn the_selected_row_is_visibly_different_from_its_neighbours() {
    // Reading only the vertex count cannot tell a highlight from a no-op.
    // This compares the *border colour actually painted at the row's own
    // rect*, not merely whether anything is there — the row's own fill
    // (`ROW_BG`/`ROW_SEL`) already covers those pixels regardless of
    // selection, so a colour-blind `coverage` check cannot tell
    // "outlined" from "an ordinary row" (see `coverage_of`'s docs).
    let rows = vec![button("ONE"), button("TWO"), button("THREE")];
    let (w, h) = (1280.0, 720.0);
    let sel = geometry(&frame_with(rows.clone(), 1), w, h);
    let unsel = geometry(&frame_with(rows.clone(), 99), w, h);
    assert_ne!(
        sel, unsel,
        "selecting a row must change the emitted geometry"
    );

    let rect = row_rect(&rows, 1, w, h).expect("row 1 exists");
    // The selection border is 2 px inside the row; sample its top edge.
    let border = (rect.0 + 4.0, rect.1, rect.2 - 8.0, 2.0);
    assert!(
        coverage_of(&sel, w, h, border, FG) > 0.9,
        "the highlighted row should be outlined in FG: {:?}",
        coverage_of(&sel, w, h, border, FG)
    );
    assert!(
        coverage_of(&unsel, w, h, border, FG) < 0.05,
        "an unhighlighted row must not be outlined: {:?}",
        coverage_of(&unsel, w, h, border, FG)
    );
}

#[test]
fn a_rows_text_lands_inside_that_rows_rect() {
    // Negative control included: a row's glyphs must be inside *its* rect
    // and absent from the rect below it, or the layout is off by a row.
    let rows = vec![button("AAAA"), button("BBBB")];
    let (w, h) = (1280.0, 720.0);
    let v = geometry(&frame_with(rows.clone(), 99), w, h);
    let (x, y, rw, rh) = row_rect(&rows, 0, w, h).unwrap();
    // Sample a band where the glyphs are, just right of the padding.
    let band = (x + PAD, y + rh * 0.35, text_px("AAAA", TEXT_SCALE), rh * 0.3);
    assert!(
        coverage(&v, w, h, band) > 0.25,
        "row 0's label is not in row 0's rect: {}",
        coverage(&v, w, h, band)
    );
    // And the gap between rows must be background only.
    let gap = (x, y + rh + 1.0, rw, ROW_GAP - 2.0);
    assert!(
        coverage(&v, w, h, gap) < 0.05,
        "something is drawn in the inter-row gap: {}",
        coverage(&v, w, h, gap)
    );
}

#[test]
fn row_rects_are_ordered_non_overlapping_and_on_screen() {
    let rows: Vec<MenuRow> = (0..6).map(|i| button(&format!("ROW{i}"))).collect();
    let (w, h) = (1280.0, 720.0);
    let mut prev_bottom = 0.0f32;
    for i in 0..rows.len() {
        let (x, y, rw, rh) = row_rect(&rows, i, w, h).expect("row exists");
        assert!(y >= prev_bottom, "row {i} overlaps the one above");
        assert!(x >= 0.0 && x + rw <= w, "row {i} is off-screen: {x}+{rw}");
        assert!(y + rh <= h, "row {i} runs off the bottom");
        prev_bottom = y + rh;
    }
    assert!(row_rect(&rows, 99, w, h).is_none());
}

#[test]
fn a_slotted_row_sharing_a_frame_does_not_perturb_the_centred_stacks_math() {
    // The bug this guards: `row_rect`'s centred-stack total used to sum
    // *every* row's height, including slotted ones, because no screen had
    // ever mixed the two kinds before the account screen (a scrollable
    // unslotted list plus slotted nine-slice action buttons). Build one
    // unslotted-only frame and one with an extra slotted row spliced in
    // between two unslotted rows, and require the *unslotted* rows to land
    // at identical rects in both — the slotted row must be invisible to
    // their stack.
    let (w, h) = (1280.0, 720.0);
    let plain: Vec<MenuRow> = vec![button("A"), button("B")];
    let plain_rects: Vec<_> = (0..plain.len())
        .map(|i| row_rect(&plain, i, w, h).unwrap())
        .collect();

    let mut mixed = vec![button("A")];
    mixed.push(MenuRow {
        label: "SLOTTED".to_string(),
        enabled: true,
        slot: Some(Slot {
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: 0.0,
            w: 50.0,
            h: 20.0,
        }),
        ..Default::default()
    });
    mixed.push(button("B"));

    let a_rect = row_rect(&mixed, 0, w, h).unwrap();
    let b_rect = row_rect(&mixed, 2, w, h).unwrap();
    assert_eq!(a_rect, plain_rects[0], "row A must not shift because a slotted row shares the frame");
    assert_eq!(b_rect, plain_rects[1], "row B must not shift either");

    // The slotted row itself is unaffected too — it always resolves via
    // its own `Slot`, never the stack.
    let slotted_rect = row_rect(&mixed, 1, w, h).unwrap();
    assert_eq!(slotted_rect, (1280.0 * 0.5, 0.0, 50.0, 20.0));
}

#[test]
fn default_head_icon_is_a_real_mosaic_not_a_blank_or_transparent_one() {
    // The account screen's placeholder head must actually reach pixels —
    // an all-transparent or all-zero mosaic would draw nothing and look
    // exactly like a missing icon, which is indistinguishable from this
    // function being wired to nothing.
    let m = default_head_icon();
    assert_eq!(m.size, MOSAIC);
    assert_eq!(m.cells.len(), MOSAIC * MOSAIC);
    assert!(m.cells.iter().any(|c| c[3] > 0.0), "every cell was transparent");
    // Not a flat single colour either — the hairline row and eye pixels
    // must show up as *some* variation, or `head_mosaic`'s box filter
    // could be silently discarding the source detail.
    let first = m.cells[0];
    assert!(
        m.cells.iter().any(|c| c != &first),
        "the mosaic is a single flat colour; the hand-authored detail did not survive the filter"
    );
}

#[test]
fn head_mosaic_is_the_same_drawable_favicon_mosaic_is() {
    // `head_mosaic` takes raw RGBA + dimensions (what a decoded skin's
    // face region would already be), unlike `favicon_mosaic`'s PNG bytes
    // — this pins that the two still produce the same shape of output
    // (same box filter) given equivalent solid-colour input.
    let rgba = vec![10u8, 200, 30, 255].repeat(4 * 4);
    let m = head_mosaic(&rgba, 4, 4).expect("a valid RGBA buffer must decode");
    assert_eq!(m.size, MOSAIC);
    for c in &m.cells {
        assert!((c[0] - 10.0 / 255.0).abs() < 0.01);
        assert!((c[1] - 200.0 / 255.0).abs() < 0.01);
        assert!((c[2] - 30.0 / 255.0).abs() < 0.01);
    }
}

#[test]
fn a_favicon_mosaic_reaches_the_rows_icon_square() {
    // The whole point of the favicon path: real PNG bytes → pixels in the
    // row. A solid red 8x8 PNG must fill the icon square with red.
    let png = solid_png(8, [220, 20, 20, 255]);
    let m = favicon_mosaic(&png).expect("a solid PNG must decode");
    assert_eq!(m.size, MOSAIC);
    assert_eq!(m.cells.len(), MOSAIC * MOSAIC);
    for c in &m.cells {
        assert!(c[0] > 0.8 && c[1] < 0.2 && c[2] < 0.2, "cell was {c:?}");
        assert!(c[3] > 0.9, "opaque source must stay opaque: {c:?}");
    }

    let rows = vec![MenuRow {
        label: "SERVER".into(),
        detail: "a motd".into(),
        favicon: Some(m),
        enabled: true,
        ..Default::default()
    }];
    let (w, h) = (1280.0, 720.0);
    let v = geometry(&frame_with(rows.clone(), 0), w, h);
    let (x, y, _, rh) = row_rect(&rows, 0, w, h).unwrap();
    let icon = (x + PAD, y + (rh - ICON) * 0.5, ICON, ICON);
    assert!(
        coverage(&v, w, h, icon) > 0.95,
        "the favicon square is not covered: {}",
        coverage(&v, w, h, icon)
    );

    // Negative control: the same row with no favicon leaves that square to
    // the row fill, so the assertion above is measuring the icon and not
    // the row background.
    let mut bare = rows.clone();
    bare[0].favicon = None;
    let v2 = geometry(&frame_with(bare, 0), w, h);
    assert_ne!(
        v.len(),
        v2.len(),
        "dropping the favicon must remove its quads"
    );
}

#[test]
fn a_broken_favicon_is_skipped_rather_than_panicking() {
    assert!(favicon_mosaic(b"not a png").is_none());
    assert!(favicon_mosaic(&[]).is_none());
    // A valid PNG header with a truncated body.
    assert!(favicon_mosaic(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).is_none());
}

#[test]
fn a_favicon_smaller_than_the_mosaic_still_fills_every_cell() {
    // The bug this guards: integer division leaving empty source rects and
    // therefore transparent (invisible) cells for a 4x4 icon.
    let m = favicon_mosaic(&solid_png(4, [10, 200, 40, 255])).expect("decodes");
    assert!(
        m.cells.iter().all(|c| c[3] > 0.9),
        "a {MOSAIC}-cell mosaic of a 4x4 source left transparent cells"
    );
}

#[test]
fn long_labels_are_clipped_instead_of_overrunning_the_row() {
    let rows = vec![MenuRow {
        label: "X".repeat(400),
        detail: "Y".repeat(400),
        trailing: "999/999".into(),
        enabled: true,
        ..Default::default()
    }];
    let (w, h) = (1280.0, 720.0);
    let v = geometry(&frame_with(rows.clone(), 0), w, h);
    let (x, y, rw, rh) = row_rect(&rows, 0, w, h).unwrap();
    // Nothing may be drawn to the right of the row.
    let outside = (x + rw + 2.0, y, 200.0, rh);
    assert_eq!(
        coverage(&v, w, h, outside),
        0.0,
        "text overran the row's right edge"
    );
}

// -- the account screen (#66/#402) ----------------------------------------

/// A nav whose `profiles.json` holds `names`, most-recently-used **first**
/// (the order `AccountsNav::ordered` sorts into, so `names[0]` is row 0).
/// Written beside a temp `servers.json`, which is where `MenuNav::with_path`
/// looks for it.
fn accounts_nav(tag: &str, names: &[&str]) -> MenuNav {
    let path = std::env::temp_dir().join(format!(
        "lodestone-render-accounts-{}-{tag}/servers.json",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    let mut meta = lodestone_auth::metadata::AccountsMetadata::default();
    for (i, name) in names.iter().enumerate() {
        meta.upsert(lodestone_auth::metadata::AccountProfile {
            profile_id: uuid::Uuid::new_v4(),
            username: (*name).to_string(),
            skin_url: None,
            last_used: (names.len() - i) as u64,
        });
    }
    meta.save_to(&path.parent().unwrap().join("profiles.json"))
        .expect("the temp profiles file must be writable");
    MenuNav::with_path(path)
}

/// An accounts nav holding `n` generated accounts (so `n + 1` logical rows) with the
/// list parked at `scroll` **pixels**.
///
/// The offset is set through `AccountsNav::scroll_by`, i.e. the real wheel path, so a
/// test cannot park the list somewhere the wheel could never reach — and the notch
/// count is derived from the rate rather than being restated, so this helper does not
/// quietly encode a second opinion about how far a notch goes.
fn accounts_nav_scrolled(tag: &str, n: usize, scroll: f32) -> MenuNav {
    let names: Vec<String> = (0..n).map(|i| format!("p{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let nav = accounts_nav(tag, &refs);
    let accounts = nav.accounts();
    let rate = crate::menu::render::accounts_list_spec(n + 1, 0.0)
        .model(crate::config::MIN_SCALED_HEIGHT as f32)
        .expect("the band must exist at the minimum canvas")
        .scroll_rate();
    accounts.scroll_by(-(scroll / rate), crate::config::MIN_SCALED_HEIGHT as f32);
    assert_eq!(
        accounts.scroll(),
        scroll,
        "the helper failed to park the list at {scroll} px"
    );
    nav
}

#[test]
fn the_accounts_slots_do_not_depend_on_the_reference_canvas() {
    // The same argument the multiplayer screen's version of this makes: the
    // block is arranged **once** at `ACCOUNTS_REF_CANVAS`, which is sound only
    // if every rect it hands out is canvas-independent once expressed as a
    // `Slot`. Even widths only — `Origin::ScreenBottom`'s x is `width * 0.5`
    // unrounded while `FrameLayout` truncates, so an odd logical width differs
    // by half a pixel (the limit `Screen::WorldSelect`'s footer has too).
    for (w, h) in [(854.0, 480.0), (1280.0, 720.0), (640.0, 400.0)] {
        let live = AccountsBlock::at(w, h);
        assert_eq!(
            accounts_block().content_top,
            live.content_top,
            "the content band moved at {w}x{h}"
        );
        for i in 0..crate::menu::accounts::BUTTON_COUNT {
            let slot = accounts_button_slot(i);
            assert_eq!(
                slot,
                live.footer_slot(i),
                "button {i}'s slot depends on the canvas"
            );
            // ...and it must resolve onto *that* canvas' own arrangement,
            // which is what makes the two derivations independent rather than
            // merely equal.
            let got = slot.resolve(w, h);
            let want = live.footer[i];
            assert!(
                (got.0 - want.0).abs() < 0.01
                    && (got.1 - want.1).abs() < 0.01
                    && (got.2 - want.2).abs() < 0.01
                    && (got.3 - want.3).abs() < 0.01,
                "button {i} resolves to {got:?} at {w}x{h}, arranged at {want:?}"
            );
        }
    }
    // The footer column measures `4 * 74 + 3 * 4`, which is the multiplayer
    // screen's lower row exactly — the agreement `ACCOUNTS_BUTTON_W`'s doc
    // claims, asserted rather than described.
    let first = accounts_button_slot(0).resolve(854.0, 480.0);
    let last = accounts_button_slot(crate::menu::accounts::BUTTON_COUNT - 1)
        .resolve(854.0, 480.0);
    let column = last.0 + last.2 - first.0;
    let want = 4.0 * ACCOUNTS_BUTTON_W + 3.0 * ACCOUNTS_FOOTER_SPACING as f32;
    assert!(
        (column - want).abs() < 0.01,
        "the footer column is {column}, not {want}"
    );
}

#[test]
fn the_account_rows_are_in_the_order_click_assumes() {
    // `AccountsNav::hover` maps a **rendered** row index back through the
    // scroll window and then onto the four button slots, so this order is a
    // coupling between two files — the same guard shape the settings and
    // multiplayer screens carry against the same #391 bug.
    use crate::menu::accounts::{
        BUTTON_ADD, BUTTON_CANCEL, BUTTON_COUNT, BUTTON_REMOVE, BUTTON_SELECT,
    };
    let nav = accounts_nav("order", &["Alex", "Steve"]);
    let f = accounts_idle_frame(nav.accounts());

    assert_eq!(
        f.rows.len(),
        3 + BUTTON_COUNT,
        "two accounts + the offline entry + four buttons"
    );
    for (i, row) in f.rows.iter().take(3).enumerate() {
        let view = row
            .account
            .as_ref()
            .unwrap_or_else(|| panic!("row {i} is not a list row"));
        assert_eq!(view.index, i, "row {i} carries the wrong rendered index");
    }
    for (button, label) in [
        (BUTTON_ADD, "Add Account"),
        (BUTTON_SELECT, "Select"),
        (BUTTON_REMOVE, "Remove"),
        (BUTTON_CANCEL, "Back"),
    ] {
        let row = &f.rows[3 + button];
        assert_eq!(row.label, label, "button {button} is labelled wrong");
        assert_eq!(
            row.slot,
            Some(accounts_button_slot(button)),
            "{label} is not in its own footer slot"
        );
        assert!(row.account.is_none(), "{label} must not be a list row");
    }

    // The two cursors are separate: the keyboard starts on row 0, which is the
    // *list* cursor, and no footer button may be lit while it is there.
    assert!(f.rows[0].account.as_ref().unwrap().selected);
    assert_eq!(
        f.selected,
        usize::MAX,
        "a button is highlighted while focus is on a row"
    );
}

/// **The island closed: the offline row's label is the persisted name.**
///
/// `crate::offline_identity` shipped the model, the file and the derived UUID with
/// no reader — this frame hardcoded `"Play offline"`, so the one name every join
/// in this client uses reached zero pixels. Asserted through the real
/// `accounts_idle_frame`, not through `AccountsNav::offline_username`, because the
/// accessor being right is exactly what the bug was compatible with.
#[test]
fn the_offline_row_carries_the_persisted_name_through_the_real_frame() {
    // Called for its side effect: it wipes the temp root and writes a
    // `profiles.json` there. Dropped immediately — the nav under test is the
    // second one, built *after* the identity file exists, because `with_path`
    // reads the identity once at construction.
    drop(accounts_nav("offline-label", &["Alex"]));
    // Written to the *same* temp root the nav derives its own offline path from,
    // so this proves the derivation as well as the label. `MenuNav::with_path`
    // takes `servers.json`; `AccountsNav::with_path` takes `profiles.json`
    // beside it; the identity is `offline.json` beside that.
    let dir = std::env::temp_dir().join(format!(
        "lodestone-render-accounts-{}-offline-label",
        std::process::id()
    ));
    let mut id = crate::offline_identity::OfflineIdentity::default();
    id.set_username("Notch").expect("valid fixture name");
    id.save_to(&dir.join("offline.json")).expect("temp offline.json");
    let nav = MenuNav::with_path(dir.join("servers.json"));

    let f = accounts_idle_frame(nav.accounts());
    let offline = f
        .rows
        .iter()
        .find(|r| r.detail == "No sign-in required")
        .expect("the offline row must exist");
    assert_eq!(offline.label, "Notch", "the offline row is not showing the persisted name");
    assert_ne!(
        offline.label, "Play offline",
        "the pre-fix literal is still what reaches the frame"
    );

    // The control: a root with **no** `offline.json` shows the default, so the
    // assertion above is measuring the file rather than any constant.
    let fresh = accounts_nav("offline-label-default", &["Alex"]);
    let g = accounts_idle_frame(fresh.accounts());
    let fresh_row = g
        .rows
        .iter()
        .find(|r| r.detail == "No sign-in required")
        .expect("the offline row must exist");
    assert_eq!(
        fresh_row.label,
        crate::offline_identity::DEFAULT_USERNAME,
        "with no offline.json the row must show the default name"
    );
    assert_ne!(
        fresh_row.label, offline.label,
        "both roots produced the same label, so neither is reading the file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The island gate for the name editor: `frame_for` really reaches it.**
///
/// `accounts_name_edit_frame` and every one of `AccountsNav`'s editor tests form
/// a closed loop — they can all be green while `dispatch.rs`'s `Screen::Accounts`
/// arm still only ever consults `sign_in_view()`, in which case pressing "Edit
/// Name" swallows the keyboard and draws the *list* frame underneath it. Nothing
/// but a gate through `frame_for` — the function `app.rs` calls every frame — can
/// see that, so this drives that function and nothing else.
///
/// It also checks the two things an early `return` out of `frame_for` would have
/// silently dropped: `gui_scale` and `list` are stamped after the `match`, so an
/// editor frame produced by a `return` would ignore the GUI-scale setting.
#[test]
fn frame_for_reaches_the_name_editor_and_still_stamps_the_frame() {
    let nav = accounts_nav("editor-dispatch", &[]);
    // `unavailable_probe`, like every other `frame_for` test here: a real prober
    // would reach the network from a unit test.
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::default();
    let mut ui = UiState::new();
    ui.open_accounts();

    // Before: the list frame, with the offline row on it.
    let before = frame_for(&ui, &nav, &statuses, &mut fav).expect("the list frame");
    assert!(
        before.rows.iter().any(|r| r.detail == "No sign-in required"),
        "precondition: the list frame must be what shows before the editor opens"
    );

    let accounts = nav.accounts();
    accounts.hover(accounts.rows().len() + crate::menu::accounts::BUTTON_REMOVE);
    accounts.handle_key(MenuKey::Enter);
    assert!(
        accounts.is_editing_name(),
        "precondition: the editor must be open, or 'the frame changed' is vacuous"
    );

    let after = frame_for(&ui, &nav, &statuses, &mut fav).expect("the editor frame");
    assert!(
        after.rows.iter().any(|r| r.edit.is_some()),
        "frame_for did not reach the editor: no row carries an EditBox, so the \
         screen still draws the account list while the keyboard types into an \
         invisible field. Rows were {:?}",
        after.rows.iter().map(|r| &r.label).collect::<Vec<_>>()
    );
    assert!(
        !after.rows.iter().any(|r| r.detail == "No sign-in required"),
        "the list rows are still being drawn, so this is the list frame"
    );
    // Stamped, and asserted against `nav`'s own value rather than a literal —
    // the same source `frame_for`'s tail reads.
    assert_eq!(after.gui_scale, nav.gui_scale(), "gui_scale was not stamped");
    // And it really draws: measured through the same `geometry` the screen sweep
    // uses, at a canvas the whole frame is on.
    assert!(
        !geometry(&after, 1280.0, 720.0).is_empty(),
        "the editor frame draws nothing"
    );
}

/// The name editor's frame: a real [`EditBox`] row plus a Done button, with the
/// UUID of the **typed** name on screen.
///
/// The `edit.is_some()` assertion is the island guard `create_world::frame`'s own
/// doc explains at length: a focusable, typeable box that no row carries draws
/// nothing at all, and every unit test of the box itself still passes.
#[test]
fn the_name_editor_frame_carries_a_real_edit_box_and_the_typed_uuid() {
    use crate::menu::accounts::{NAME_EDIT_DONE_ROW, NAME_EDIT_FIELD_ROW};

    let nav = accounts_nav("name-editor", &[]);
    let accounts = nav.accounts();
    // Open it through the production path: the offline row is row 0 with no
    // accounts, and the third footer button is the affordance.
    accounts.hover(accounts.rows().len() + crate::menu::accounts::BUTTON_REMOVE);
    accounts.handle_key(MenuKey::Enter);
    for _ in 0..20 {
        accounts.handle_key(MenuKey::Backspace);
    }
    for ch in "Notch".chars() {
        accounts.handle_key(MenuKey::Char(ch));
    }

    let view = accounts.name_edit_view().expect("the editor must be open");
    let f = super::account_screen::accounts_name_edit_frame(&view);
    assert_eq!(f.rows.len(), 2, "the editor is a field and a Done button");
    let field = &f.rows[NAME_EDIT_FIELD_ROW];
    assert!(
        field.edit.is_some(),
        "the field row carries no EditBox, so nothing draws the caret, the \
         selection or the visible slice"
    );
    assert!(field.field, "the field row is not drawn as a field");
    assert_eq!(field.edit.as_ref().unwrap().value(), "Notch");
    assert_eq!(f.rows[NAME_EDIT_DONE_ROW].label, "Done");
    assert!(
        f.rows[NAME_EDIT_DONE_ROW].edit.is_none(),
        "a button row must not carry an EditBox"
    );
    assert_eq!(
        f.selected, NAME_EDIT_DONE_ROW,
        "the row cursor must be on Done, not behind the text field"
    );

    // The UUID line, and its expected value comes from `offline_identity`'s
    // externally-computed vector rather than from `offline_uuid` re-derived here.
    const NOTCH: &str = "b50ad385-829d-3141-a216-7e7d7539ba7f";
    assert!(
        f.labels.iter().any(|l| l.text.contains(NOTCH)),
        "the typed name's UUID is not on screen; labels were {:?}",
        f.labels.iter().map(|l| &l.text).collect::<Vec<_>>()
    );
    // Control: the *default* name's UUID must not also be there, or the label
    // could be showing the saved identity and this would still pass.
    let player = crate::offline_identity::offline_uuid("Player").to_string();
    assert!(
        !f.labels.iter().any(|l| l.text.contains(&player)),
        "the saved name's UUID is on screen too, so the line is not following \
         what was typed"
    );

    // Both rows have a `slot`, and the two must not be the same rect — a field
    // drawn on top of its own Done button is a screen with one visible widget.
    let field_slot = field.slot.expect("the field must have a slot");
    let done_slot = f.rows[NAME_EDIT_DONE_ROW]
        .slot
        .expect("Done must have a slot");
    assert_ne!(field_slot, done_slot);
    let (fx, fy, fw, fh) = field_slot.resolve(854.0, 480.0);
    let (dx, dy, dw, dh) = done_slot.resolve(854.0, 480.0);
    assert!(
        fy + fh <= dy || dy + dh <= fy,
        "the field ({fx},{fy},{fw},{fh}) and Done ({dx},{dy},{dw},{dh}) overlap \
         vertically"
    );
    // Both on-canvas, measured rather than assumed: "nothing drew" is free for a
    // rect that is off the canvas entirely.
    for (name, (x, y, w, h)) in [("field", (fx, fy, fw, fh)), ("done", (dx, dy, dw, dh))] {
        assert!(
            x >= 0.0 && y >= 0.0 && x + w <= 854.0 && y + h <= 480.0,
            "{name} resolves off-canvas at ({x},{y},{w},{h})"
        );
    }
}

#[test]
fn an_account_row_draws_inside_its_own_36px_row_and_not_the_one_below() {
    let nav = accounts_nav("rowpixels", &["Alex"]);
    let f = accounts_idle_frame(nav.accounts());
    let (w, h) = (854.0, 480.0);
    let v = geometry(&f, w, h);

    // Row 0 is Alex, row 1 the offline entry, row 2 is past the end.
    for i in 0..2 {
        let rect = accounts_row_rect(i, w, 0.0);
        assert!(
            coverage(&v, w, h, rect) > 0.05,
            "row {i} drew nothing in {rect:?}: {}",
            coverage(&v, w, h, rect)
        );
    }
    let empty = accounts_row_rect(2, w, 0.0);
    assert_eq!(
        coverage(&v, w, h, empty),
        0.0,
        "something drew in the row past the end, at {empty:?}"
    );

    // The 32 px head fills the content box's full height, which is the whole
    // point of a 36 px pitch with 2 px of padding.
    let (cx, cy, _, _) = accounts_row_content_rect(0, w, 0.0);
    let head = (cx, cy, ACCOUNTS_HEAD_ICON, ACCOUNTS_HEAD_ICON);
    assert!(
        coverage(&v, w, h, head) > 0.95,
        "the head icon does not fill {head:?}: {}",
        coverage(&v, w, h, head)
    );
}

/// **The reported bug.** The sign-in failure reason was drawn as one
/// unwrapped centred line at [`TEXT_SCALE`], so a message assembled from a
/// server's own response body was both too large to read and wider than the
/// screen.
///
/// Measured by location, against the rect the *draw* derives — `notice_rect`
/// is called here rather than restated, because `CLAUDE.md` records two gates
/// whose restated rect was itself the thing that was wrong — and the failure
/// output is a bounding box, not a fraction. The control is **executed**: the
/// same detector, on the same frame, with a deliberately unbounded wrap
/// column, must report a box outside the rect. Without it, "nothing
/// overflowed" would pass just as well on a frame where nothing drew at all.
#[test]
fn a_long_sign_in_failure_is_wrapped_and_bounded_to_the_notice_rect() {
    // `lodestone-auth`'s `step_result` formats `"{status}: {snippet}"` with up
    // to 400 characters of whatever the server actually returned, and a JSON
    // body has **no whitespace in it** — so a wrap that only breaks on spaces
    // emits one enormous line, and this passes only because `wrap_bounded`
    // breaks mid-word.
    let body = format!(
        "401:{{\"XErr\":2148916238,\"Message\":\"{}\"}}",
        "x".repeat(360)
    );
    assert!(
        !body.contains(' '),
        "premise: the message has no whitespace to wrap on"
    );

    let (w, h) = (854.0, 480.0);
    let frame = accounts_failed_frame(&body);
    let notice = frame
        .notice
        .clone()
        .expect("the failure state must carry a notice");
    let (nx, ny, nw, nh) = notice_rect(&notice, w, h);
    let v = geometry(&frame, w, h);
    let got = colour_bounds(&v, w, h, notice.colour)
        .expect("the failure message reached no pixels at all");
    assert!(
        got.0 >= nx - 0.5
            && got.0 + got.2 <= nx + nw + 0.5
            && got.1 >= ny - 0.5
            && got.1 + got.3 <= ny + nh + 0.5,
        "the failure text drew at {got:?}, outside its notice rect {:?}",
        (nx, ny, nw, nh)
    );
    // Wrapped, not merely cut: one line's box is a single glyph tall.
    assert!(
        got.3 > LINE_H,
        "the message was cut to one line instead of wrapped: box {got:?}"
    );

    // The control. Same text, same detector, a column twice the canvas wide.
    let mut unbounded = accounts_failed_frame(&body);
    unbounded
        .notice
        .as_mut()
        .expect("the control still has a notice")
        .w = w * 2.0;
    let cv = geometry(&unbounded, w, h);
    let control = colour_bounds(&cv, w, h, notice.colour)
        .expect("the control drew nothing, so it proves nothing");
    assert!(
        control.0 + control.2 > nx + nw,
        "the detector cannot see an overflow: control box {control:?} against rect {:?}",
        (nx, ny, nw, nh)
    );
}

#[test]
fn wrap_bounded_breaks_a_run_that_no_whitespace_wrap_could() {
    // The difference from `wrap_measured` in one test, with that function as
    // the control: what makes a second wrap necessary rather than a flag on
    // the first is that the multiplayer screen's greedy fallback ("a word that
    // does not fit starts a line") does nothing at all for a 400-character
    // token.
    let b = Quads::new(854.0, 480.0);
    let run = "x".repeat(400);
    let column = 120.0;

    let hard = wrap_bounded(&b, &run, column, 8);
    assert!(hard.len() > 1, "the run was not broken at all: {hard:?}");
    for (i, line) in hard.iter().enumerate() {
        let lw = b.text_width(line, 1.0);
        assert!(lw <= column, "line {i} measures {lw} in a {column} column");
    }

    let soft = wrap_measured(&b, &run, column, 8);
    assert_eq!(
        soft.len(),
        1,
        "wrap_measured's documented behaviour changed: {soft:?}"
    );
    assert!(
        b.text_width(&soft[0], 1.0) > column,
        "the control did not overflow, so it proves nothing"
    );

    // And it terminates on a column too narrow for a single glyph, rather
    // than pushing empty lines forever.
    let starved = wrap_bounded(&b, &run, 1.0, 4);
    assert_eq!(starved.len(), 4);
    assert!(starved.iter().all(|l| l.chars().count() == 1));
}

/// One wheel notch on the accounts list moves **18 px**, through the generic router.
///
/// The magnitude is the claim, not the direction: "it scrolled" is satisfied by the
/// row-index model this replaced, which is the defect the owner reported. So the
/// prediction is separated from both rivals, each computed from outside constants:
///
/// | hypothesis | one notch |
/// |---|---|
/// | vanilla, `floor(defaultEntryHeight / 2)` (`AbstractSelectionList.java:44`) | **18** |
/// | the row-index model this replaced, one notch one row | 36 |
/// | a whole band, if the notch were mistaken for a page | 147 |
///
/// It then asserts 18 lands strictly *inside* row 0 (which spans 0..36 in content
/// space) and coincides with **no** row top, so a snap-to-row implementation cannot
/// pass. Driven through `MenuNav::scroll_active_list` — the router `app`'s single
/// `MouseWheel` arm calls — rather than through `AccountsNav::scroll_by`, because the
/// router is the part that was missing: before it, `app/` had exactly two wheel arms
/// and neither was this screen's.
#[test]
fn one_notch_on_the_accounts_list_is_half_a_row_through_the_generic_router() {
    const CANVAS_H: f32 = 240.0;
    let mut nav = accounts_nav("acct-notch", &["a", "b", "c", "d", "e", "f", "g", "h"]);
    let mut ui = crate::menu::UiState::default();
    ui.open_accounts();
    assert_eq!(
        nav.accounts().scroll(),
        0.0,
        "precondition: a freshly opened list is at the top"
    );

    // Negative `dy` scrolls down, matching vanilla's sign (the negation lives in
    // `ScrollList::mouse_scrolled`, so this is winit's `scrollY` verbatim).
    let moved = nav.scroll_active_list(&ui, -1.0, CANVAS_H);
    assert!(moved, "the router must report that the accounts list moved");
    let one = nav.accounts().scroll();

    assert_eq!(one, 18.0, "one notch is floor(36 / 2), not a whole row");
    assert_ne!(one, 36.0, "the row-index model's answer must be excluded");
    assert_ne!(one, 147.0, "a page-sized notch must be excluded");

    // Strictly inside row 0, and on no row's top — the property a snap-to-row
    // implementation structurally cannot have. Row tops are derived from the same
    // helper the draw places rows with.
    let band_top = crate::menu::render::accounts_band_top();
    assert!(
        (0..9).all(|i| crate::menu::render::accounts_row_top(i, one) != band_top),
        "offset {one} coincides with a row top, so it is indistinguishable from a jump"
    );
    assert!(
        one > 0.0 && one < 36.0,
        "offset {one} must sit strictly inside the first row"
    );

    // Three notches reach 54 — not a multiple of 36, so no row counter can represent
    // it at all. This is the assertion that cannot be satisfied by rescaling a
    // row-quantized implementation.
    nav.scroll_active_list(&ui, -2.0, CANVAS_H);
    let three = nav.accounts().scroll();
    assert_eq!(three, 54.0, "three notches of travel");
    assert_ne!(three % 36.0, 0.0, "54 must not be expressible as whole rows");

    // And the clamp is the primitive's: scrolling far past the end lands exactly on
    // `max_scroll`, computed here from vanilla's own expression rather than read back.
    nav.scroll_active_list(&ui, -1000.0, CANVAS_H);
    let content = 9.0 * 36.0 + 2.0 * widget::LIST_CONTENT_PADDING;
    let band = CANVAS_H - 60.0 - 33.0;
    assert_eq!(
        nav.accounts().scroll(),
        content - band,
        "the clamp must be maxScrollAmount() = contentHeight() - height"
    );

    // Scrolling up past the top clamps at zero rather than going negative.
    nav.scroll_active_list(&ui, 1000.0, CANVAS_H);
    assert_eq!(nav.accounts().scroll(), 0.0, "the top clamp is zero");
}

/// The router answers `false` for a screen with no list, which is what lets `app`
/// have **one** wheel arm instead of one per screen.
///
/// The control for the test above: without this, `scroll_active_list` returning
/// `true` unconditionally would still pass every assertion there while making the
/// wheel do something on screens that have no list.
#[test]
fn the_wheel_router_declines_a_screen_with_no_list() {
    let mut nav = accounts_nav("router-decline", &["a", "b", "c", "d", "e", "f", "g", "h"]);
    let mut ui = crate::menu::UiState::default();
    // The title screen is `owns_frame`, so `app`'s arm *does* fire here — which is
    // exactly why the router rather than the arm has to be the thing that declines.
    assert!(
        owns_frame(ui.screen()),
        "premise: the title screen is inside the set app's wheel arm covers"
    );
    assert!(
        nav.active_list(&ui).is_none(),
        "the title screen must declare no list"
    );
    assert!(
        !nav.scroll_active_list(&ui, -1.0, 240.0),
        "the router must decline a screen with no list"
    );
    // And the accounts list, reached from the same nav, still moves — so the `false`
    // above is the screen's answer and not a broken router.
    ui.open_accounts();
    assert!(
        nav.scroll_active_list(&ui, -1.0, 240.0),
        "the same router must still move a screen that does have a list"
    );
}

/// **The accounts screen has a scrollbar, and it is the multiplayer list's.**
///
/// This is the gate for the generic `ListSpec` hook, and it is a pixel gate on
/// purpose: the hook's whole reason for existing is that a screen adopting
/// `ScrollList` before it landed would have had correct geometry, green unit tests
/// and *nothing on screen* — `render::draw` called `server_scroll_list` by name, so
/// only one screen could ever have a bar. So the claim is not "the geometry is
/// right", it is "pixels appear in the scrollbar's rect on a screen that is not the
/// multiplayer list".
///
/// Measured **by location and by colour**, never as a frame fraction:
///
/// - the thumb's rect carries `LABEL`, the colour `draw_scrollbar` gives the scroller
/// - the 8 px gutter between the rows' right edge and the bar carries nothing **but
///   the band's own tint**, which is what pins the bar *outside* the row column
///   (`scrollBarX() = getRowRight() + scrollbarWidth() + 2`) rather than inset into it
/// - a list short enough not to scroll draws **no bar at all** — vanilla's
///   `if (this.scrollable())` gate, and the control that stops the two assertions
///   above passing on a bar that is unconditionally painted
///
/// Every rect comes from `ScrollList::scrollbar_rects`, the same call the draw makes,
/// rather than from restated arithmetic.
#[test]
fn the_accounts_screen_draws_the_same_scrollbar_the_server_list_does() {
    let (w, h) = (854.0, 240.0);
    let nav = accounts_nav_scrolled("acct-bar", 8, 18.0);
    let mut ui = crate::menu::UiState::default();
    ui.open_accounts();
    let statuses =
        crate::menu::status::StatusCache::with_probe(crate::menu::status::unavailable_probe());
    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).expect("the accounts screen owns its frame");
    let v = geometry(&f, w, h);

    let spec = f
        .list
        .as_ref()
        .expect("frame_for must stamp the accounts screen's ListSpec");
    let list = spec.model(h).expect("nine 36 px rows in a 147 px band");
    let row_right = spec.row_right(w);
    let (track, thumb) = list
        .scrollbar_rects(row_right)
        .expect("premise: nine rows in a 147 px band must scroll");

    let on_thumb = coverage_of(&v, w, h, thumb, LABEL);
    assert!(
        on_thumb > 0.90,
        "the scroller is not painted at {thumb:?}: only {on_thumb} of it carries LABEL \
         (track {track:?}, row right {row_right})"
    );

    // The gutter: `scrollbar_x` is `row_right + 6 + 2`, so the 8 px immediately right
    // of the rows belongs to neither. Derived from the same expression, not restated.
    //
    // **Re-derived when the band chrome landed**, and the old form is worth recording
    // because it was correct and is now the wrong question. This asserted
    // `coverage(...) == 0` — nothing at all in the gutter — which held only because the
    // band had no background: `extractListBackground` blits `menu_list_background.png`
    // across the list widget's own `getX()..getRight()`, and that is the whole canvas,
    // so the gutter is *inside* the tint by vanilla's own construction. Asserting
    // emptiness again would mean asserting the tint away.
    //
    // The claim the gate exists to make is unchanged and is now spelled colour-wise:
    // the **topmost** thing in the gutter is the tint at every sample, so no row ink
    // and no scrollbar ink reached it. Strictly stronger than the old count — a bar
    // inset into the row column would paint `LABEL` here and fail — and it cannot be
    // satisfied by a frame that simply drew nothing, because a bare backdrop reports
    // `None` rather than the tint.
    let gutter = (row_right, list.top(), widget::SCROLLBAR_WIDTH + 2.0, list.height());
    let tinted = coverage_of(&v, w, h, gutter, LIST_BAND_TINT);
    assert_eq!(
        tinted, 1.0,
        "the gutter {gutter:?} between the rows and the bar is not uniformly the band \
         tint ({tinted} of it is) — something else painted there, so the bar is inset \
         into the row column rather than outside it"
    );

    // The control, run rather than described: two accounts is three rows, which fit
    // the band, so `scrollable()` is false and the bar must vanish entirely. If this
    // still found LABEL in the same rect, the assertion above would be measuring a
    // bar that is always drawn — which is what "the bar exists" must not mean.
    let short_nav = accounts_nav("acct-bar-short", &["A", "B"]);
    let short_f =
        frame_for(&ui, &short_nav, &statuses, &mut fav).expect("still the accounts screen");
    let short_v = geometry(&short_f, w, h);
    let short_spec = short_f.list.as_ref().expect("a short list still declares a spec");
    let short_list = short_spec.model(h).expect("and still has a band");
    assert!(
        !short_list.scrollable(),
        "premise: three 36 px rows must fit a {} px band",
        short_list.height()
    );
    assert!(
        short_list.scrollbar_rects(short_spec.row_right(w)).is_none(),
        "a list that does not scroll must report no scrollbar rects"
    );
    let on_thumb_short = coverage_of(&short_v, w, h, thumb, LABEL);
    assert_eq!(
        on_thumb_short, 0.0,
        "a non-scrolling list still painted {on_thumb_short} of {thumb:?} in LABEL"
    );
}

/// A straddling account row is **cut at the band**, not drawn over the footer.
///
/// ## Why this replaced `a_short_canvas_truncates_the_account_window_…`
///
/// That test asserted the opposite rule, and its premise expired the moment the
/// list went pixel-granular. It required `accounts_row_visible` to reject any row
/// not wholly inside the band, and checked the survivors ended above the arranged
/// button row. Both halves are now false *by design*: at an intermediate offset a
/// straddling row is the normal case, and rejecting it would drop a row at every
/// position between two whole-row stops — a worse artefact than the 36 px stepping
/// the conversion removed. `draw_account_entry` is wrapped in `Quads::with_clip`
/// instead, so the row is drawn **and cut**.
///
/// Note how it would have failed: the old premise assertion `fitting < VISIBLE_ROWS`
/// goes false (all five rows now "fit" the partial-overlap test), so the test would
/// have failed loudly rather than silently passing — which is why this is a rewrite
/// and not a deletion. The rule worth keeping is the *consequence* the old test was
/// really about, and it is the stronger claim: **no account row paints below the
/// band**, whatever the offset.
///
/// Measured by location, in the rows' own 305 px column, at an offset deliberately
/// chosen to put a row across the boundary. Failure prints the offending band.
#[test]
fn an_account_row_straddling_the_band_is_clipped_not_drawn_over_the_footer() {
    let (w, h) = (854.0, 240.0);
    // Nine logical rows in a 147 px band: the list scrolls, so an intermediate
    // offset is reachable. 18 px is one wheel notch — half a row, which guarantees
    // some row crosses each edge of the band.
    let nav = accounts_nav_scrolled("clip-band", 8, 18.0);
    // **Through `frame_for`, not `accounts_idle_frame`.** The spec is stamped by
    // `frame_for`'s tail, the same place `gui_scale` is, so calling the per-screen
    // builder directly gets a frame with `list: None` — which is exactly the island
    // this whole hook exists to prevent, and worth exercising rather than working
    // around. This assertion is therefore also the guard that the stamp happens.
    let mut ui = crate::menu::UiState::default();
    ui.open_accounts();
    let statuses = crate::menu::status::StatusCache::with_probe(
        crate::menu::status::unavailable_probe(),
    );
    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).expect("the accounts screen owns its frame");
    let v = geometry(&f, w, h);

    let spec = f
        .list
        .as_ref()
        .expect("frame_for must stamp the accounts screen's ListSpec");
    let list = spec.model(h).expect("and it has a band at 240 px");
    let band_bottom = list.top() + list.height();
    let col_x = spec.row_left(w);
    let col_w = spec.row_w(w);

    // Precondition, executed rather than assumed: a row really does straddle the
    // bottom edge at this offset. Without it this test could pass on a list that
    // simply ends above the band.
    let straddler = (0..9).find(|&i| {
        let (_, y, _, rh) = accounts_row_rect(i, w, 18.0);
        y < band_bottom && y + rh > band_bottom
    });
    assert!(
        straddler.is_some(),
        "premise: no row crosses the band bottom at {band_bottom}, so the clip is untested"
    );

    // The claim: nothing the list draws lands below the band, in the list's own
    // column.
    //
    // **The strip is band-bottom to the *button row's* top, not to the canvas
    // bottom, and getting that wrong is instructive.** The first version of this
    // measured all the way down and read 0.352 covered — which looks exactly like a
    // broken clip and is in fact the four action buttons, which legitimately own the
    // footer band. A control has to ask what *else* already paints in the rect before
    // it can attribute coverage to the thing under test; here 0.352 is almost
    // precisely the button row's own 20 px out of the 60 px band. The button y comes
    // off `accounts_button_slot` — the same arranged slot the draw places the buttons
    // from — rather than being restated as a constant.
    //
    // **The strip now starts `SEPARATOR_H` below the band, and that is a re-derivation
    // rather than a loosening.** `extractListSeparators` blits `footer_separator.png`
    // — 32×2 — at exactly `getBottom()`, so the first two rows of this strip belong to
    // the separator by vanilla's own construction, and the old bound of `band_bottom`
    // asserted them away. It read 0.083 covered when the chrome landed, which is 2 of
    // the 24 sample rows: the bar, not a row. The clip under test is unchanged.
    let (_, button_y, _, _) = accounts_button_slot(0).resolve(w, h);
    assert!(
        button_y > band_bottom + SEPARATOR_H,
        "premise: the button row at {button_y} must sit below the band at {band_bottom} \
         plus its separator"
    );
    let below = (
        col_x,
        band_bottom + SEPARATOR_H,
        col_w,
        button_y - band_bottom - SEPARATOR_H,
    );
    let spill = coverage(&v, w, h, below);
    assert_eq!(
        spill, 0.0,
        "an account row painted {spill} of the gap {below:?} between the band bottom \
         ({band_bottom}) and the button row ({button_y}); straddling row {straddler:?}"
    );

    // The control this needs: the clip must not be achieving that by drawing
    // nothing at all. The band itself has to be covered.
    let inside = (col_x, list.top(), col_w, list.height());
    let drawn = coverage(&v, w, h, inside);
    assert!(
        drawn > 0.10,
        "the band {inside:?} is nearly empty ({drawn}), so the zero above proves \
         only that the list is not drawing"
    );
}

#[test]
fn owns_frame_excludes_paused_so_the_pause_menu_never_replaces_the_world() {
    // The specific regression this module's docs warn about: adding
    // `Screen::Paused` to `owns_frame` would make `app.rs`'s `draw_menu`
    // return `true` for it, skipping the world/HUD/container render path
    // entirely — the pause menu would work, but the game behind it would
    // stop rendering for as long as it was up.
    assert!(!owns_frame(Screen::Paused));
}

#[test]
fn frame_for_defers_to_an_overlay_for_in_world_settings() {
    // The player report this exists for: Options opened from the pause
    // menu must show the paused *world* behind it, not the main-menu
    // panorama. `frame_for` returning `Some` unconditionally for
    // `Screen::Settings` was exactly the bug — `draw_menu` took the
    // `Clear` pass and the world (and its HUD/container passes) never
    // drew at all.
    //
    // Two controls in one test, by construction rather than assertion:
    // the title-screen route still gets a frame at all (a regression that
    // made *every* Options entry return `None` would still pass a
    // negative-only check), and `owns_frame` staying `true` for both
    // routes is what proves the two are meant to diverge here, not drift
    // apart by accident.
    let nav = test_nav("settings-overlay");
    let mut fav = FaviconCache::new();
    let statuses = StatusCache::with_probe(unavailable_probe());

    let mut from_title = UiState::new();
    from_title.open_settings();
    assert!(!from_title.settings_in_world());
    assert!(
        frame_for(&from_title, &nav, &statuses, &mut fav).is_some(),
        "Options from the title screen must still own the frame — it has \
         no world to show behind it"
    );

    let mut from_pause = UiState::new();
    from_pause.enter_dev_world();
    from_pause.pause();
    from_pause.open_settings_from_pause();
    assert!(from_pause.settings_in_world());
    assert!(
        frame_for(&from_pause, &nav, &statuses, &mut fav).is_none(),
        "in-world Options must defer to an overlay over the still-\
         rendering world, not the Clear pass"
    );
    // `owns_frame` itself is unchanged either way — see its doc.
    assert!(owns_frame(Screen::Settings));
}

#[test]
fn pause_frame_builds_vanillas_ten_widgets_in_order_and_tracks_the_highlight() {
    use crate::menu::nav::{PAUSE_BUTTONS, PauseButton};

    let mut nav = test_nav("pause-frame");
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.pause();
    // The last index, not 1: this screen reproduces vanilla's whole grid, so
    // Disconnect is the last widget rather than the third. The old version of this
    // test asserted a three-row stack.
    nav.hover(&ui, PAUSE_BUTTONS.len() - 1);

    let f = pause_frame(&nav);
    assert_eq!(
        f.backdrop,
        MenuBackdrop::Dim,
        "the pause menu must dim the live world it is drawn over, not replace it \
         with the panorama"
    );
    assert!(f.vanilla, "and it must be laid out from vanilla's arithmetic");
    // Ten since issue #535 put vanilla's singleplayer Open to LAN button beside
    // Options. Read from the button table rather than restated, so the two cannot
    // drift.
    assert_eq!(f.rows.len(), PAUSE_BUTTONS.len());
    assert_eq!(f.rows[0].label, PauseButton::BackToGame.label());
    assert_eq!(f.rows[1].label, PauseButton::Advancements.label());
    assert_eq!(f.rows[2].label, PauseButton::Statistics.label());
    assert_eq!(f.rows[7].label, PauseButton::Options.label());
    assert_eq!(f.rows[8].label, PauseButton::OpenToLan.label());
    assert_eq!(f.rows[9].label, PauseButton::QuitToTitle.label());
    assert_eq!(
        f.selected,
        PAUSE_BUTTONS.len() - 1,
        "selection follows the nav's pause_index"
    );
    // Seven are live: the three with actions, plus Advancements, Statistics and
    // Player Reporting since issues #167/#188/#189 built the screens behind them,
    // and Open to LAN since #535 gave `IntegratedServer::open_to_lan` a caller
    // (see `PauseButton::enabled`'s own doc for each — what each screen shows is
    // honest-but-limited, not what made the button liveness conditional).
    let live: Vec<&str> = f
        .rows
        .iter()
        .filter(|r| r.enabled)
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(
        live,
        vec![
            "Back to Game",
            "Advancements",
            "Statistics",
            "Player Reporting",
            "Options...",
            "Open to LAN",
            "Disconnect"
        ]
    );
    // The four icon buttons carry a sprite instead of a label.
    assert_eq!(f.rows.iter().filter(|r| r.icon.is_some()).count(), 4);
    assert!(f.rows.iter().all(|r| r.slot.is_some()));
    // And the heading is a positioned label, not the row stack's title.
    assert!(f.title.is_empty());
    assert_eq!(f.labels.len(), 1);
    assert_eq!(f.labels[0].text, "Game Menu");
    assert!(!geometry(&f, 1280.0, 720.0).is_empty());
}

/// The canvas vanilla's own default window resolves to (854×480 at GUI
/// scale 1 is vanilla's canonical GUI size), so the expected rects below are
/// the numbers a vanilla screenshot at that size would show.
const V_W: f32 = 854.0;
/// See [`V_W`].
const V_H: f32 = 480.0;

#[test]
fn the_title_screen_rects_are_vanillas_own() {
    use crate::menu::nav::MainButton as B;
    // Hand-derived from `TitleScreen.init` / `createNormalMenuOptions`
    // (`TitleScreen.java:105-205`) at 854×480, *not* read back out of
    // `title_slot`: topPos = 480/4 + 48 = 168, rows every 24 px, the icon
    // row from `getHorizontalPosition(n, 3, 20)` = 427 - 34 + (n-1)*24, and
    // the Options/Quit pair at `W/2 - 100` / `W/2 + 2`, 98 wide.
    //
    // Since #394 `title_slot` computes these from an arranged
    // `LinearLayout` column instead of holding them as constants, so this is
    // the **no-move gate** for that conversion: the table is vanilla's own
    // hand arithmetic (which uses no layout class at all) and the values come
    // out of the layout tree. If the two ever disagree, one of them is wrong
    // and this says which button.
    let expected = [
        (B::Singleplayer, (327.0, 168.0, 200.0, 20.0)),
        (B::Multiplayer, (327.0, 192.0, 200.0, 20.0)),
        (B::Realms, (327.0, 216.0, 200.0, 20.0)),
        (B::Friends, (393.0, 240.0, 20.0, 20.0)),
        (B::Language, (417.0, 240.0, 20.0, 20.0)),
        (B::Accessibility, (441.0, 240.0, 20.0, 20.0)),
        (B::Options, (327.0, 264.0, 98.0, 20.0)),
        (B::Quit, (429.0, 264.0, 98.0, 20.0)),
    ];
    for (button, want) in expected {
        assert_eq!(
            title_slot(button).resolve(V_W, V_H),
            want,
            "{button:?} is not where vanilla puts it"
        );
    }
    // The 4 px gutter between Options and Quit is the title screen's, and it
    // is *not* the pause screen's 8 px one — a detail that is easy to
    // conflate, so pin both.
    let (ox, _, ow, _) = title_slot(B::Options).resolve(V_W, V_H);
    let (qx, ..) = title_slot(B::Quit).resolve(V_W, V_H);
    assert_eq!(qx - (ox + ow), 4.0, "title screen gutter");
}

#[test]
fn the_pause_screen_rects_are_vanillas_own() {
    use crate::menu::nav::PauseButton as B;
    // Hand-derived from `PauseScreen.createPauseMenu` (`PauseScreen.java:91-183`)
    // through `GridLayout.arrangeElements`, at 854×480: the 212×166 grid is
    // aligned (0.5, 0.25) so its origin is (321, 78); row y offsets inside it
    // are [0, 70, 94, 118, 142] and each child sits at its own padding.
    //
    // These nine rects were `pause_slot`'s *implementation* until #394 and are
    // now its expectation: the values below come out of a real ported
    // `GridLayout`, and the table is the independent derivation they have to
    // agree with. Two derivations of the same arithmetic, one by hand from the
    // Java and one by running a port of it — which is the only shape of gate
    // that can catch a port that is self-consistently wrong.
    let gx = 321.0;
    let gy = 78.0;
    let expected = [
        (B::BackToGame, (gx + 4.0, gy + 50.0, 204.0, 20.0)),
        (B::Advancements, (gx + 4.0, gy + 74.0, 98.0, 20.0)),
        (B::Statistics, (gx + 110.0, gy + 74.0, 98.0, 20.0)),
        (B::ReportBugs, (gx + 60.0, gy + 98.0, 20.0, 20.0)),
        (B::Feedback, (gx + 84.0, gy + 98.0, 20.0, 20.0)),
        (B::Friends, (gx + 108.0, gy + 98.0, 20.0, 20.0)),
        (B::PlayerReporting, (gx + 132.0, gy + 98.0, 20.0, 20.0)),
        // Issue #535: the Options row is now vanilla's `hasSingleplayerServer()`
        // branch (`:157-160`) — two half-width cells, same 8 px gutter and same
        // row `y` as the Advancements/Statistics pair two rows up. The grid's five
        // row offsets and its 212x166 size are unchanged, which is why this repair
        // touches three lines and not the table.
        (B::Options, (gx + 4.0, gy + 122.0, 98.0, 20.0)),
        (B::OpenToLan, (gx + 110.0, gy + 122.0, 98.0, 20.0)),
        (B::QuitToTitle, (gx + 4.0, gy + 146.0, 204.0, 20.0)),
    ];
    for (button, want) in expected {
        assert_eq!(
            pause_slot(button).resolve(V_W, V_H),
            want,
            "{button:?} is not where vanilla puts it"
        );
    }
    // The grid origin itself, spelled out: 0.5/0.25 alignment of 212×166.
    assert_eq!(Origin::PauseGrid.anchor(V_W, V_H), (gx, gy));
    // A full-width pause button starts at `W/2 - 102`, not the title
    // screen's `W/2 - 100`, and the half-width pair has an 8 px gutter, not
    // 4 — both fall out of the 204+8 cell, and both are the details a
    // remembered layout gets wrong.
    assert_eq!(
        pause_slot(B::BackToGame).resolve(V_W, V_H).0,
        V_W / 2.0 - 102.0
    );
    let (ax, _, aw, _) = pause_slot(B::Advancements).resolve(V_W, V_H);
    let (sx, ..) = pause_slot(B::Statistics).resolve(V_W, V_H);
    assert_eq!(sx - (ax + aw), 8.0, "pause screen gutter");
    assert_eq!(
        (ax + aw + sx) / 2.0,
        V_W / 2.0,
        "the half-width pair straddles the centre line"
    );
}

#[test]
fn the_pause_grid_size_is_the_arranged_layouts_own() {
    // `Origin::PauseGrid` aligns the grid's *measured* size in the screen
    // rect, so that size is load-bearing for all nine rects at once — a grid
    // 2 px too wide moves every button 1 px left. `PAUSE_GRID_W`/`_H` are the
    // hand derivation (204 + 4 + 4 wide; 70 + 4 * 24 tall) and this is the
    // only place they are compared with what the port computes.
    assert_eq!(pause_grid_size(), (PAUSE_GRID_W, PAUSE_GRID_H));
    // The same numbers reached the other way, from the arranged tree rather
    // than the cache, so the cache cannot be what is agreeing with itself.
    let grid = pause_menu_grid_with(PAUSE_MENU_PADDING_TOP);
    assert_eq!((grid.width(), grid.height()), (212.0, 166.0));
    // And the grid really does hold nine drawable leaves in `PAUSE_BUTTONS`
    // order — the four icon buttons among them come from a *nested*
    // `LinearLayout`, so this is also the assertion that `visit_widgets`
    // flattens the nesting rather than yielding the row as one child.
    assert_eq!(
        layout::widget_rects(&grid).len(),
        crate::menu::nav::PAUSE_BUTTONS.len()
    );
}

#[test]
fn a_changed_cell_padding_moves_every_pause_rect() {
    // #394's negative control, executed rather than described: change one
    // `LayoutSettings` padding value and the rect assertions must go red. The
    // subject is the real builder with one argument varied, not a copy of it,
    // so this cannot pass by testing something else.
    //
    // `MENU_PADDING_TOP` is row 0's `paddingTop`. Dropping it by 10 must
    // (a) move Back to Game up 10, (b) shrink the grid 10, and therefore
    // (c) move every *later* row up 10 as well — a silently no-op arrange pass
    // would fail all three.
    let real = layout::widget_rects(&pause_menu_grid_with(PAUSE_MENU_PADDING_TOP));
    let short = layout::widget_rects(&pause_menu_grid_with(PAUSE_MENU_PADDING_TOP - 10));
    assert_eq!(real[0].1, 50.0);
    assert_eq!(short[0].1, 40.0, "row 0's padding must move row 0");
    for (i, (r, s)) in real.iter().zip(&short).enumerate() {
        assert_eq!(
            r.1 - s.1,
            10.0,
            "widget {i} did not move with the row above it"
        );
        assert_eq!(r.0, s.0, "and nothing may move horizontally");
    }
    let grid = pause_menu_grid_with(PAUSE_MENU_PADDING_TOP - 10);
    assert_eq!(
        (grid.width(), grid.height()),
        (PAUSE_GRID_W, PAUSE_GRID_H - 10.0),
        "the grid's own height is the sum of its rows, so it must shrink too"
    );
}

#[test]
fn death_frame_builds_vanillas_two_widgets_in_order_and_tracks_the_highlight() {
    use crate::menu::nav::{DEATH_BUTTONS, DeathButton};

    let mut nav = test_nav("death-frame");
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.die(Some("was slain by a Skeleton".to_string()));
    nav.hover(&ui, 1);

    let f = death_frame(&nav, ui.death_message());
    assert_eq!(
        f.backdrop,
        MenuBackdrop::Dim,
        "the death screen must dim the live world it is drawn over"
    );
    assert!(f.vanilla, "and be laid out from vanilla's arithmetic");
    assert_eq!(f.rows.len(), 2, "vanilla's death screen has two widgets");
    assert_eq!(f.rows[0].label, DeathButton::Respawn.label());
    assert_eq!(f.rows[1].label, DeathButton::TitleScreen.label());
    assert!(
        f.rows.iter().all(|r| r.enabled),
        "unlike title/pause, neither death-screen button is ever disabled"
    );
    assert!(f.rows.iter().all(|r| r.slot.is_some()));
    assert_eq!(f.selected, 1, "selection follows the nav's death_index");
    assert_eq!(DEATH_BUTTONS.len(), 2);

    // The heading is a positioned label (the title), not the row stack's
    // centred title string.
    assert!(f.title.is_empty());
    // Title + message + score.
    assert_eq!(f.labels.len(), 3);
    assert_eq!(f.labels[0].text, "You Died!");
    assert_eq!(f.labels[0].scale, 2.0, "vanilla scales the title 2x");
    assert_eq!(f.labels[1].text, "was slain by a Skeleton");
    assert_eq!(f.labels[2].text, "Score: 0");
    assert!(!geometry(&f, V_W, V_H).is_empty());

    // No message: two labels, not three, and the score line still draws —
    // matching vanilla's own `if (this.causeOfDeath != null)` guard.
    let no_message = death_frame(&nav, None);
    assert_eq!(no_message.labels.len(), 2);
    assert_eq!(no_message.labels[0].text, "You Died!");
    assert_eq!(no_message.labels[1].text, "Score: 0");
}

#[test]
fn the_death_screen_rects_are_vanillas_own() {
    use crate::menu::nav::DeathButton as B;
    // Hand-derived from `DeathScreen.init` (`DeathScreen.java:42-60`) at
    // 854×480: both buttons are `width/2-100, height/4+72|96, 200x20`,
    // and `height/4+72 == TitleTop.anchor().1 + 24` since `TitleTop` is
    // itself `floor(height/4) + 48` — 168 + 24 = 192, 168 + 48 = 216.
    let expected = [
        (B::Respawn, (327.0, 192.0, 200.0, 20.0)),
        (B::TitleScreen, (327.0, 216.0, 200.0, 20.0)),
    ];
    for (button, want) in expected {
        assert_eq!(
            death_slot(button).resolve(V_W, V_H),
            want,
            "{button:?} is not where vanilla puts it"
        );
    }
}

#[test]
fn the_death_screens_title_is_anchored_on_the_left_quarter_not_the_centre() {
    // The trap named in `Origin::DeathTitle`'s docs: `DeathScreen.
    // visitText` draws the title at `middleLine / 2` where `middleLine ==
    // width / 2`, i.e. `width / 4` — not `width / 2` like every other
    // centred heading in this file (`Origin::ScreenTop`). A layout
    // "corrected" to the screen centre would fail this by a wide margin.
    //
    // `.floor()`ed (issue #401): `854.0 / 4.0` is `213.5`, not a whole
    // pixel, where vanilla's `this.width / 2 / 2` is two Java integer
    // divisions and can only ever land on a whole pixel.
    assert_eq!(Origin::DeathTitle.anchor(V_W, V_H), ((V_W / 4.0).floor(), 0.0));
    assert_ne!(
        Origin::DeathTitle.anchor(V_W, V_H).0,
        Origin::ScreenTop.anchor(V_W, V_H).0,
        "the death title and the score/message lines are not on the same x"
    );
}

/// Issue #401: every width-derived [`Origin`] anchor is vanilla's `this.width`
/// (always `int`) divided by a constant — Java integer division — so the x
/// term must be `floor`ed. At an *even* width that is invisible, because
/// `width * 0.5` (or `* 0.25`) is already a whole pixel; **no test before
/// this one used an odd width**, which is exactly how the bug shipped. 855
/// is odd and not a multiple of 4 either, so it exercises every one of the
/// affected arms at once.
///
/// Each assertion predicts *both* hypotheses from `width` alone — floored
/// (right) and unfloored (the bug) — and requires landing on the floored
/// one, per CLAUDE.md's magnitude-species rule: asserting only "the anchor
/// moved" or "is not X.5" would pass for nearly any wrong number too.
#[test]
fn odd_width_anchors_are_floored_like_javas_integer_division() {
    let width = 855.0_f32;
    let height = 481.0_f32;

    let floored_half = (width * 0.5).floor();
    let unfloored_half = width * 0.5;
    assert_eq!(floored_half, 427.0, "sanity: floor(855/2) is 427, not 427.5");
    assert_ne!(floored_half, unfloored_half, "sanity: 855 is odd, so the two must differ");

    assert_eq!(
        Origin::ScreenTop.anchor(width, height),
        (floored_half, 0.0),
        "ScreenTop must not land on the unfloored {unfloored_half}"
    );
    assert_eq!(
        Origin::TitleTop.anchor(width, height),
        (floored_half, (height / 4.0).floor() + 48.0),
        "TitleTop's x must not land on the unfloored {unfloored_half}"
    );
    assert_eq!(
        Origin::ScreenBottom.anchor(width, height),
        (floored_half, height),
        "ScreenBottom must not land on the unfloored {unfloored_half}"
    );

    let floored_quarter = (width * 0.25).floor();
    let unfloored_quarter = width * 0.25;
    assert_eq!(floored_quarter, 213.0, "sanity: floor(855/4) is 213, not 213.75");
    assert_ne!(floored_quarter, unfloored_quarter, "sanity: 855/4 is not a whole pixel");
    assert_eq!(
        Origin::DeathTitle.anchor(width, height),
        (floored_quarter, 0.0),
        "DeathTitle must not land on the unfloored {unfloored_quarter}"
    );
}

#[test]
fn every_vanilla_widget_is_on_screen_and_none_overlap() {
    // The layout arithmetic has to hold at more than one canvas size, and a
    // widget that lands on top of another is a hit-test that activates the
    // wrong button.
    let nav = test_nav("vanilla-rects");
    let mut ui = UiState::new();
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let title = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    ui.enter_dev_world();
    ui.pause();
    let pause = pause_frame(&nav);
    ui.enter_dev_world();
    ui.die(Some("fell from a high place".to_string()));
    let death = death_frame(&nav, ui.death_message());

    for (name, frame) in [("title", &title), ("pause", &pause), ("death", &death)] {
        // 320×240 is the smallest canvas `calculate_gui_scale` will produce
        // (see `config.rs`'s MIN_SCALED_*), so it is the real lower bound.
        for (w, h) in [(320.0f32, 240.0f32), (V_W, V_H), (1280.0, 720.0)] {
            let rects: Vec<(f32, f32, f32, f32)> = (0..frame.rows.len())
                .map(|i| row_rect(&frame.rows, i, w, h).expect("a slotted row has a rect"))
                .collect();
            for (i, r) in rects.iter().enumerate() {
                assert!(
                    r.0 >= 0.0 && r.0 + r.2 <= w,
                    "{name} widget {i} off-screen horizontally at {w}x{h}: {r:?}"
                );
                assert!(
                    r.1 >= 0.0 && r.1 + r.3 <= h,
                    "{name} widget {i} off-screen vertically at {w}x{h}: {r:?}"
                );
            }
            for (i, a) in rects.iter().enumerate() {
                for (j, b) in rects.iter().enumerate().skip(i + 1) {
                    let overlap = a.0 < b.0 + b.2
                        && b.0 < a.0 + a.2
                        && a.1 < b.1 + b.3
                        && b.1 < a.1 + a.3;
                    assert!(
                        !overlap,
                        "{name} widgets {i} and {j} overlap at {w}x{h}: {a:?} {b:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn an_overlay_frames_backdrop_is_translucent_unlike_an_ordinary_menus() {
    // The whole point of `MenuFrame::overlay`: the paused world underneath
    // must stay visible, which only holds if the backdrop quad's alpha is
    // measurably below opaque. A negative control (an ordinary, non-overlay
    // frame) proves the opaque case still exists and this isn't just
    // measuring `geometry`'s general output.
    let nav = test_nav("pause-overlay-alpha");
    let overlay = pause_frame(&nav);
    let v = geometry(&overlay, 1280.0, 720.0);
    // The backdrop is the very first quad emitted (vertex 0..6); alpha is
    // the 4th of the 6 floats per vertex ([x, y, r, g, b, a]).
    let backdrop_alpha = v[5];
    assert!(
        backdrop_alpha < 0.9,
        "an overlay backdrop must let the world show through: alpha={backdrop_alpha}"
    );

    let ordinary = frame_with(vec![button("QUIT")], 0);
    let v2 = geometry(&ordinary, 1280.0, 720.0);
    assert!(
        (v2[5] - 1.0).abs() < f32::EPSILON,
        "a non-overlay menu's backdrop must stay opaque: alpha={}",
        v2[5]
    );
}

#[test]
fn the_highlighted_pause_button_is_visibly_different_from_its_neighbours() {
    // Colour-aware, because the fill quad already covers every pixel a
    // border would: `coverage`'s "is anything here" cannot separate the
    // highlighted state from an ordinary row (see `coverage_of`'s docs).
    //
    // This is the *fallback* (no atlas) chrome — flat ROW_SEL / ROW_BG /
    // ROW_OFF fills. The real `widget/button*` sprite selection is gated
    // separately by `the_button_sprite_matches_vanillas_enabled_hovered_rule`.
    let mut nav = test_nav("pause-highlight");
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.pause();

    // Options (index 7) is enabled, so it can actually be highlighted.
    nav.hover(&ui, 7);
    let (w, h) = (V_W, V_H);
    let frame = pause_frame(&nav);
    let sel = geometry(&frame, w, h);
    let mut unsel_frame = pause_frame(&nav);
    unsel_frame.selected = 99;
    let unsel = geometry(&unsel_frame, w, h);
    assert_ne!(sel, unsel, "selecting a pause row must change the geometry");

    // A strip of the button's *interior above its label*: the label's top is
    // `y + (h - 9)/2 + 1` == y+6 for a 20 px button, and the 1 px selection
    // border ends at y+1. Sampling y+2..y+4 therefore measures the fill and
    // only the fill — the first version of this test sampled the whole
    // interior and failed on the disabled row, because `colour_at` returns
    // the *topmost* quad and "Advancements" is dense enough in a 98 px button
    // to push label ink into more than 10 % of the samples.
    let inside = |i: usize| {
        let (x, y, rw, _rh) = row_rect(&frame.rows, i, w, h).expect("a slotted row has a rect");
        (x + 4.0, y + 2.0, rw - 8.0, 2.0)
    };
    assert!(
        coverage_of(&sel, w, h, inside(7), ROW_SEL) > 0.9,
        "the highlighted row is not filled with ROW_SEL: {}",
        coverage_of(&sel, w, h, inside(7), ROW_SEL)
    );
    // Negative control 1: the same rect with nothing selected is ROW_BG, and
    // carries no ROW_SEL at all.
    assert!(
        coverage_of(&unsel, w, h, inside(7), ROW_SEL) < 0.05,
        "an unhighlighted row must not use the selected fill"
    );
    assert!(
        coverage_of(&unsel, w, h, inside(7), ROW_BG) > 0.9,
        "an unhighlighted enabled row should be filled with ROW_BG"
    );
    // Negative control 2: a *disabled* row is a third, distinct colour and
    // never picks up the selected fill even when it is the selection —
    // vanilla's `WidgetSprites::get` gives disabled priority over hovered.
    //
    // The subject is **Report Bugs (index 3)**, not Advancements (index 1):
    // #167 made that one live, and a control pointed at an enabled row measures
    // nothing. Report Bugs has no screen behind it and no plan for one, so it is
    // the stable choice.
    let disabled_row = 3;
    assert!(
        !crate::menu::nav::PAUSE_BUTTONS[disabled_row].enabled(),
        "this control needs a row that is genuinely disabled"
    );
    let mut on_disabled = pause_frame(&nav);
    on_disabled.selected = disabled_row;
    let on_disabled = geometry(&on_disabled, w, h);
    assert!(
        coverage_of(&on_disabled, w, h, inside(disabled_row), ROW_OFF) > 0.9,
        "a disabled row must keep the disabled fill even while highlighted: {}",
        coverage_of(&on_disabled, w, h, inside(disabled_row), ROW_OFF)
    );
    assert!(
        coverage_of(&on_disabled, w, h, inside(disabled_row), ROW_SEL) < 0.05,
        "a disabled row must never draw the selected fill"
    );
    // And the three colours really are distinguishable, so the three
    // assertions above are measurements and not the same one three times.
    assert_ne!(ROW_SEL, ROW_BG);
    assert_ne!(ROW_SEL, ROW_OFF);
    assert_ne!(ROW_BG, ROW_OFF);
}

/// A synthetic pack carrying just the three `widget/button*` sprites, each a
/// different size so its atlas region is identifiable, and each with a
/// **different nine-slice border** in its `.mcmeta` — 3 / 3 / 1, exactly the
/// real 26.2 pack's values, which is what lets a test tell "border read from
/// the pack" apart from "border hardcoded to 3".
#[cfg(test)]
fn button_pack() -> lodestone_assets::ResourceManager {
    use lodestone_assets::{MemorySource, ResourceSource};
    let mut src = MemorySource::default();
    for (id, border) in [
        ("widget/button", 3u32),
        ("widget/button_highlighted", 3),
        ("widget/button_disabled", 1),
    ] {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_rgba_png(200, 20, [10, 20, 30, 255]),
        );
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png.mcmeta"),
            format!(
                r#"{{"gui":{{"scaling":{{"type":"nine_slice","width":200,"height":20,"border":{border}}}}}}}"#
            )
            .into_bytes(),
        );
    }
    // A 15×15 icon, so the icon-button path has something to draw too.
    src.insert(
        "assets/minecraft/textures/gui/sprites/icon/language.png",
        solid_rgba_png(15, 15, [90, 200, 90, 255]),
    );
    lodestone_assets::ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>])
}

/// The atlas rect of a sprite id, in normalised UVs — the ground truth a
/// "which sprite was sampled" assertion compares against.
fn sprite_uv_bounds(atlas: &GuiAtlas, id: &str) -> ([f32; 2], [f32; 2]) {
    let loc: lodestone_assets::ResourceLocation =
        format!("minecraft:gui/sprites/{id}").parse().expect("location");
    let s = atlas.atlas().sprite(&loc).expect("sprite placed");
    let (aw, ah) = (atlas.atlas().width as f32, atlas.atlas().height as f32);
    (
        [s.x as f32 / aw, s.y as f32 / ah],
        [
            (s.x + s.width) as f32 / aw,
            (s.y + s.height) as f32 / ah,
        ],
    )
}

/// Whether every sprite-stream vertex's UV lies inside `(min, max)`.
fn all_uvs_within(sprite: &[f32], min: [f32; 2], max: [f32; 2]) -> bool {
    !sprite.is_empty()
        && sprite.chunks_exact(SPRITE_FLOATS_PER_VERTEX).all(|v| {
            v[2] >= min[0] - 1e-6
                && v[2] <= max[0] + 1e-6
                && v[3] >= min[1] - 1e-6
                && v[3] <= max[1] + 1e-6
        })
}

/// The **destination** bounding box of every sprite-stream vertex, back in
/// logical pixels — the inverse of `Quads::rect`'s
/// `(2x/w - 1, 1 - 2y/h)`.
///
/// This is what turns "a sprite was drawn" into "a sprite was drawn *there*",
/// and it reports a box rather than a fraction so a failure says where
/// (`CLAUDE.md`: a gate that reports only a percentage cannot tell a shifted
/// widget from a missing one). `GuiAtlas::geometry`'s quads "tile the target
/// exactly, with no gaps or overlap", so for an integral rect this *is* the
/// rect — but the round trip through NDC and back costs a few `f32` ulps
/// (`327` can come back as `326.99997`), so callers compare within a hundredth
/// of a pixel rather than with `assert_eq!`. Two orders of magnitude below the
/// one pixel a real layout error moves something by.
fn sprite_dest_bounds(sprite: &[f32], w: f32, h: f32) -> (f32, f32, f32, f32) {
    assert!(!sprite.is_empty(), "no sprite quads to measure");
    let (mut x0, mut y0) = (f32::MAX, f32::MAX);
    let (mut x1, mut y1) = (f32::MIN, f32::MIN);
    for v in sprite.chunks_exact(SPRITE_FLOATS_PER_VERTEX) {
        let px = (v[0] + 1.0) * 0.5 * w;
        let py = (1.0 - v[1]) * 0.5 * h;
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px);
        y1 = y1.max(py);
    }
    (x0, y0, x1 - x0, y1 - y0)
}

/// Whether **any** emitted quad's UV *centre* lies strictly inside
/// `(min, max)`.
///
/// Centres, not vertices: the atlas packs sprites edge to edge, so a
/// neighbouring sprite's quad has vertices exactly *on* this region's
/// boundary. The first version of the icon test tested vertices and its
/// negative control failed — correctly — because a button-background quad
/// shares an edge with the icon's region.
fn any_quad_centre_in(sprite: &[f32], min: [f32; 2], max: [f32; 2]) -> bool {
    sprite
        .chunks_exact(SPRITE_FLOATS_PER_VERTEX * 6)
        .any(|q| {
            let (u0, v0) = (q[2], q[3]);
            let (u1, v1) = (q[SPRITE_FLOATS_PER_VERTEX * 4 + 2], q[SPRITE_FLOATS_PER_VERTEX * 4 + 3]);
            let (cu, cv) = ((u0 + u1) * 0.5, (v0 + v1) * 0.5);
            cu > min[0] && cu < max[0] && cv > min[1] && cv < max[1]
        })
}

#[test]
fn the_button_sprite_matches_vanillas_enabled_hovered_rule() {
    // `WidgetSprites::get(enabled, focused)` with `AbstractButton`'s
    // three-argument set (`AbstractButton.java:18-22`,
    // `WidgetSprites.java:15-25`): enabled+hovered → highlighted,
    // enabled → button, and **disabled wins over hovered** → disabled.
    //
    // The assertion is on *which atlas region the UVs sample*, not on "a
    // quad appeared" — the three states all cover the same pixels, so
    // presence alone cannot tell them apart.
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    let one = |enabled: bool, selected: bool| {
        let rows = vec![MenuRow {
            label: "Options...".into(),
            enabled,
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: -100.0,
                dy: 40.0,
                w: 200.0,
                h: 20.0,
            }),
            ..Default::default()
        }];
        let mut f = frame_with(rows, if selected { 0 } else { 99 });
        f.vanilla = true;
        build(&f, Some(&atlas), None, V_W, V_H).sprite
    };

    let plain = sprite_uv_bounds(&atlas, "widget/button");
    let hover = sprite_uv_bounds(&atlas, "widget/button_highlighted");
    let off = sprite_uv_bounds(&atlas, "widget/button_disabled");
    // The three regions must be disjoint, or "sampled inside X" proves
    // nothing. Different sizes are not enough; check the packer actually
    // separated them.
    for (a, b) in [(plain, hover), (plain, off), (hover, off)] {
        assert!(
            a.1[0] <= b.0[0] || b.1[0] <= a.0[0] || a.1[1] <= b.0[1] || b.1[1] <= a.0[1],
            "two button sprites share atlas space: {a:?} {b:?}"
        );
    }

    assert!(
        all_uvs_within(&one(true, false), plain.0, plain.1),
        "an idle enabled button must sample widget/button"
    );
    assert!(
        all_uvs_within(&one(true, true), hover.0, hover.1),
        "a hovered enabled button must sample widget/button_highlighted"
    );
    assert!(
        all_uvs_within(&one(false, true), off.0, off.1),
        "a hovered DISABLED button must still sample widget/button_disabled"
    );
    // The control that makes the last one a real measurement: the same
    // hovered flag on an *enabled* button does not sample the disabled
    // sprite, so the assertion is not passing because everything does.
    assert!(
        !all_uvs_within(&one(true, true), off.0, off.1),
        "the detector cannot tell the disabled sprite apart"
    );
    // And with no atlas there is no sprite stream at all — the jar-less
    // path, which is why the flat-fill fallback exists.
    let rows = vec![MenuRow {
        label: "Options...".into(),
        enabled: true,
        slot: Some(Slot {
            origin: Origin::ScreenTop,
            dx: -100.0,
            dy: 40.0,
            w: 200.0,
            h: 20.0,
        }),
        ..Default::default()
    }];
    let mut f = frame_with(rows, 0);
    f.vanilla = true;
    let bare = build(&f, None, None, V_W, V_H);
    assert!(bare.sprite.is_empty(), "no atlas must mean no sprite quads");
    assert!(
        bare.colour.len() > bare.backdrop_floats,
        "and the flat fallback must still draw the button"
    );
}

#[test]
fn every_title_and_pause_widget_draws_the_sprite_the_widget_layer_picks() {
    use crate::menu::nav::{MAIN_BUTTONS, PAUSE_BUTTONS};

    // The island this rules out is the one #393 could most easily have
    // landed: `menu/widget.rs` compiles, its own tests are green, and
    // `draw_widget` keeps a private three-way `if` — so the widget layer is
    // dead code while every existing gate still passes.
    //
    // The expected sprite here is produced by `WidgetSprites::get`
    // (`menu::widget`), never spelled out, and the measurement is *which
    // atlas region the frame's own UVs sample*. So a `draw_widget` that
    // stopped consulting the widget would have to keep agreeing with
    // vanilla's rule by coincidence, for all 36 (button, focused) pairs, to
    // pass — and if the rule in `widget.rs` is wrong, this fails too.
    // #394 extends it in the other direction, without new machinery: each
    // case is now drawn at that button's **own** slot, and the sprite's
    // destination rect is asserted against it. `title_slot`/`pause_slot` read
    // the arranged layout tree since #394, so this is also the gate that says
    // the layout containers reach pixels — an arrange pass that silently
    // no-opped would put every widget at the block's origin and fail here
    // while every "a button drew something" check still passed.
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    // Both real screens' real button states and real rects, labelled so a
    // failure names the button rather than an index. `icon: None` throughout:
    // the synthetic pack carries one icon sprite, and an icon quad would put a
    // second region in the stream and make `all_uvs_within` a weaker question
    // (it would not disturb `sprite_dest_bounds`, which the icon sits inside).
    let cases: Vec<(&'static str, bool, Slot)> = MAIN_BUTTONS
        .iter()
        .map(|b| (b.label(), b.enabled(), title_slot(*b)))
        .chain(
            PAUSE_BUTTONS
                .iter()
                .map(|b| (b.label(), b.enabled(), pause_slot(*b))),
        )
        .collect();
    // The premise, checked rather than assumed: both screens really do carry
    // a mix, or "the disabled sprite was chosen" is never exercised.
    assert!(
        cases.iter().any(|(_, e, _)| *e) && cases.iter().any(|(_, e, _)| !*e),
        "neither screen has a disabled button any more, so this gate is vacuous"
    );
    // And the rects are really distinct, or the position half of this gate is
    // satisfied by every widget landing in one place.
    let distinct: std::collections::BTreeSet<(i32, i32)> = cases
        .iter()
        .map(|(_, _, s)| {
            let (x, y, ..) = s.resolve(V_W, V_H);
            (x as i32, y as i32)
        })
        .collect();
    assert_eq!(
        distinct.len(),
        cases.len(),
        "two buttons share a position, so a widget stuck at the wrong one \
         could still pass"
    );

    for (label, enabled, slot) in cases {
        for focused in [false, true] {
            let rows = vec![MenuRow {
                label: label.to_string(),
                enabled,
                slot: Some(slot),
                ..Default::default()
            }];
            let mut f = frame_with(rows, if focused { 0 } else { 99 });
            f.vanilla = true;
            let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;

            let expected = widget::BUTTON_SPRITES.get(enabled, focused);
            let (min, max) = sprite_uv_bounds(&atlas, expected);
            assert!(
                all_uvs_within(&sprite, min, max),
                "{label} (enabled={enabled}, focused={focused}) did not sample \
                 {expected}, which is what WidgetSprites::get selects"
            );
            // The control for each case: flipping `active` must move the
            // sample off this region, so "inside {expected}" is a real
            // discriminator and not something every render satisfies.
            let flipped = widget::BUTTON_SPRITES.get(!enabled, focused);
            if flipped != expected {
                let (fmin, fmax) = sprite_uv_bounds(&atlas, flipped);
                assert!(
                    !all_uvs_within(&sprite, fmin, fmax),
                    "the detector cannot tell {expected} from {flipped}"
                );
            }

            // Where it drew, in logical pixels, against the layout's own
            // answer for this button. The 0.01 is the NDC round trip's float
            // error, not slack in the layout — see `sprite_dest_bounds`.
            let drawn = sprite_dest_bounds(&sprite, V_W, V_H);
            let want = slot.resolve(V_W, V_H);
            let same = [
                (drawn.0, want.0),
                (drawn.1, want.1),
                (drawn.2, want.2),
                (drawn.3, want.3),
            ]
            .iter()
            .all(|(a, b)| (a - b).abs() < 0.01);
            assert!(
                same,
                "{label} (enabled={enabled}, focused={focused}) drew at {drawn:?}, \
                 not at {want:?} where the layout placed it"
            );
        }
    }
}

#[test]
fn nine_slice_borders_come_from_the_mcmeta_not_a_constant() {
    // `widget/button` declares `border: 3` and `widget/button_disabled`
    // declares `border: 1` in the real 26.2 pack — read straight out of
    // `client.jar`. A renderer that hardcoded one border would draw the
    // disabled button's corners three times too large, which is exactly the
    // subtle wrongness the brief warned about.
    //
    // The synthetic pack repeats those two values, so the corner quad's own
    // destination size is the discriminator.
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    let corner_size = |id: &str| {
        // Drawn far wider than native so every nine-slice piece appears.
        let quads = atlas.geometry(id, 0.0, 0.0, 400.0, 60.0);
        assert!(quads.len() >= 9, "{id} did not decompose: {}", quads.len());
        // The top-left piece is the one at the draw origin.
        let tl = quads
            .iter()
            .find(|q| q.dst[0] == 0.0 && q.dst[1] == 0.0)
            .expect("a nine-slice has a top-left corner");
        (tl.dst[2], tl.dst[3])
    };
    assert_eq!(corner_size("widget/button"), (3.0, 3.0));
    assert_eq!(
        corner_size("widget/button_disabled"),
        (1.0, 1.0),
        "the disabled sprite's border must come from its own .mcmeta"
    );
}

#[test]
fn a_disabled_label_is_drawn_in_vanillas_grey_and_an_enabled_one_in_white() {
    // `AbstractWidget.WithInactiveMessage.defaultInactiveMessage` recolours
    // an inactive widget's message to `-6250336` == `0xFFA0A0A0`
    // (`AbstractWidget.java:314-335`). Assert the actual colour, with the
    // enabled case as the control.
    let slot = Slot {
        origin: Origin::ScreenTop,
        dx: -100.0,
        dy: 40.0,
        w: 200.0,
        h: 20.0,
    };
    let render = |enabled: bool| {
        let rows = vec![MenuRow {
            label: "MMMM".into(),
            enabled,
            slot: Some(slot),
            ..Default::default()
        }];
        let mut f = frame_with(rows, 99);
        f.vanilla = true;
        build(&f, None, None, V_W, V_H).colour
    };
    let (w, h) = (V_W, V_H);
    let (x, y, rw, rh) = slot.resolve(w, h);
    // Sample the label band across the middle of the button.
    let band = (x + rw * 0.3, y + rh * 0.3, rw * 0.4, rh * 0.4);
    let off = render(false);
    let on = render(true);
    assert!(
        coverage_of(&off, w, h, band, widget::INACTIVE_LABEL) > 0.02,
        "no grey label ink in a disabled button's rect: {}",
        coverage_of(&off, w, h, band, widget::INACTIVE_LABEL)
    );
    assert_eq!(
        coverage_of(&off, w, h, band, LABEL),
        0.0,
        "a disabled label must not be drawn in the enabled colour"
    );
    assert!(
        coverage_of(&on, w, h, band, LABEL) > 0.02,
        "no white label ink in an enabled button's rect: {}",
        coverage_of(&on, w, h, band, LABEL)
    );
    assert_eq!(
        coverage_of(&on, w, h, band, widget::INACTIVE_LABEL),
        0.0,
        "an enabled label must not be drawn grey"
    );
    // The colour under test comes from the widget layer, and *that* is
    // checked against vanilla's signed ARGB integer by
    // `widget::tests::vanillas_inactive_grey_is_derived_not_transcribed`
    // rather than being restated here. What this line pins is that the two
    // files still agree: the draw grey is the widget grey.
    assert_eq!(
        widget::INACTIVE_LABEL,
        widget::argb_to_rgba(widget::INACTIVE_MESSAGE_ARGB),
        "vanilla's -6250336 is 0xFFA0A0A0"
    );
}

#[test]
fn an_icon_button_draws_its_sprite_and_no_label() {
    // Vanilla's `SpriteIconButton.CenteredIcon` draws the button background
    // plus a 15×15 sprite centred in it, and no text
    // (`SpriteIconButton.java:236-244`).
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    let slot = Slot {
        origin: Origin::ScreenTop,
        dx: -10.0,
        dy: 40.0,
        w: 20.0,
        h: 20.0,
    };
    let row = |icon: Option<&'static str>| MenuRow {
        label: "Language...".into(),
        enabled: false,
        slot: Some(slot),
        icon,
        ..Default::default()
    };
    let render = |icon: Option<&'static str>| {
        let mut f = frame_with(vec![row(icon)], 99);
        f.vanilla = true;
        build(&f, Some(&atlas), None, V_W, V_H)
    };

    let icon = render(Some("icon/language"));
    let bare = render(None);
    let icon_uv = sprite_uv_bounds(&atlas, "icon/language");
    assert!(
        any_quad_centre_in(&icon.sprite, icon_uv.0, icon_uv.1),
        "the icon sprite never reached the sprite stream"
    );
    // The control: without the icon, nothing samples that atlas region.
    assert!(
        !any_quad_centre_in(&bare.sprite, icon_uv.0, icon_uv.1),
        "the detector matches the button background too"
    );
    // And it is exactly one extra quad, drawn at the centred 15×15 rect —
    // both variants draw the same nine-slice background.
    assert_eq!(
        icon.sprite.len() - bare.sprite.len(),
        SPRITE_FLOATS_PER_VERTEX * 6,
        "an icon button should add exactly one quad"
    );
    // And an icon button draws no label ink: with the icon set, the only
    // colour quads are the backdrop.
    assert_eq!(
        icon.colour.len(),
        icon.backdrop_floats,
        "an icon button must draw no text"
    );
    assert!(
        bare.colour.len() > bare.backdrop_floats,
        "but the same row *with* a label does draw text"
    );
}

#[test]
fn the_pause_overlays_backdrop_is_vanillas_measured_black_at_alpha_64() {
    // `inworld_menu_background.png` decoded out of the real `client.jar` is
    // 16×16 greyscale+alpha with every pixel grey 0 / alpha 64
    // (`Screen.java:405,418-419` tiles it at 32 px). This pins the exact
    // value rather than "translucent enough".
    let nav = test_nav("overlay-exact");
    let v = geometry(&pause_frame(&nav), V_W, V_H);
    assert_eq!(&v[2..6], &[0.0, 0.0, 0.0, 64.0 / 255.0]);
}

#[test]
#[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
fn every_sprite_id_the_vanilla_screens_name_exists_in_the_real_pack() {
    use crate::menu::nav::{MAIN_BUTTONS, PAUSE_BUTTONS};

    // The island this rules out: a mistyped sprite id draws *nothing*, and
    // every layout assertion above still passes because they use a synthetic
    // pack whose ids are the same strings the test itself wrote. Only the
    // real jar can say whether `pause_menu/social_interactions` is spelled
    // right.
    let atlas = crate::resources::load_menu_gui_atlas().expect(
        "no vanilla pack found; set LODESTONE_ASSETS to a root with client.jar",
    );
    // Every id the widget layer can select, taken from the record itself
    // rather than relisted — so a sprite added to `WidgetSprites` is covered
    // here the day it exists.
    let button_ids = [
        widget::BUTTON_SPRITES.enabled,
        widget::BUTTON_SPRITES.disabled,
        widget::BUTTON_SPRITES.enabled_focused,
        widget::BUTTON_SPRITES.disabled_focused,
    ];
    for id in button_ids {
        assert!(atlas.contains(id), "the pack has no {id}");
        assert_eq!(
            atlas.native_size(id),
            Some((200, 20)),
            "{id} is not the 200x20 its .mcmeta declares"
        );
    }
    for icon in MAIN_BUTTONS
        .iter()
        .filter_map(|b| b.icon())
        .chain(PAUSE_BUTTONS.iter().filter_map(|b| b.icon()))
    {
        assert!(atlas.contains(icon), "the pack has no icon sprite {icon}");
        assert!(atlas.native_size(icon).is_some(), "{icon} was not placed");
        // Deliberately *no* assertion on the native size, and this is a
        // belief that was held and measured false. "Vanilla's icon-button
        // sprites are 15×15" is true of every **blit** (`spriteWidth`/
        // `spriteHeight` are 15 at each call site — `CommonButtons.java:10,21`,
        // `FriendsButton.java:22`, `PauseScreen.java:104,115,134`) and true
        // of almost none of the **files**. Measured out of the real 26.2 jar:
        //
        //   icon/language 15×15, icon/accessibility 15×15,
        //   friends/friends 16×16, pause_menu/bug 13×13,
        //   pause_menu/social_interactions 20×20,
        //   pause_menu/player_reporting 15×14
        //
        // They are all `Stretch` (no `.mcmeta`), so vanilla scales each to
        // 15×15 — including *up* from 13 and *down* from 20. Two successive
        // versions of this gate asserted a native size and were failed by
        // `friends/friends` and then `pause_menu/bug`. Drawing at
        // [`ICON_SPRITE`] is what matches vanilla; the file size is not
        // something to check against.
    }
    // The two loose title textures, and their *declared* (not native) size:
    // 26.2 ships them at 4x, which is why the draw rect is 256x64 / 128x16.
    assert_eq!(atlas.native_size("title/minecraft"), Some((1024, 256)));
    assert_eq!(atlas.native_size("title/edition"), Some((512, 64)));

    // The Resource Packs screen's own sprites (issue #415): the two hover overlays
    // off `gui/sprites/transferable_list/**`, plus the *loose*
    // `misc/unknown_pack` fallback icon, which only reaches the atlas because
    // `MENU_TEXTURES` names it. A typo in any of them draws nothing at all, in
    // silence — which is this gate's whole reason.
    for id in [
        super::draw::PACK_SELECT_SPRITES.0,
        super::draw::PACK_SELECT_SPRITES.1,
        super::draw::PACK_UNSELECT_SPRITES.0,
        super::draw::PACK_UNSELECT_SPRITES.1,
        super::draw::PACK_UNKNOWN_ICON,
    ] {
        assert!(atlas.contains(id), "the pack has no {id}");
    }
    assert_eq!(
        atlas.native_size(super::draw::PACK_SELECT_SPRITES.0),
        Some((32, 32)),
        "the overlay sprites are PackEntry.ICON_SIZE square"
    );

    // The real pack's nine-slice borders, which is where the hardcoding trap
    // is: 3 for button and button_highlighted, **1** for button_disabled.
    let corner = |id: &str| {
        let q = atlas.geometry(id, 0.0, 0.0, 400.0, 60.0);
        let tl = q
            .iter()
            .find(|q| q.dst[0] == 0.0 && q.dst[1] == 0.0)
            .expect("nine-slice top-left");
        (tl.dst[2], tl.dst[3])
    };
    assert_eq!(corner(widget::BUTTON_SPRITES.enabled), (3.0, 3.0));
    assert_eq!(corner(widget::BUTTON_SPRITES.enabled_focused), (3.0, 3.0));
    assert_eq!(corner(widget::BUTTON_SPRITES.disabled), (1.0, 1.0));

    // And the whole title frame draws through it: every sprite the two
    // screens ask for resolves to at least one quad.
    let nav = test_nav("real-pack");
    let mut ui = UiState::new();
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let title = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let geo = build(&title, Some(&atlas), None, V_W, V_H);
    // 9 nine-slice backgrounds (the 8 vanilla widgets plus the
    // non-vanilla `Accounts` row — see `MainButton::Accounts`) + 3 icons
    // + 2 logo quads, so comfortably more than one quad per widget, and
    // *nothing* on the flat-fill path.
    assert!(
        geo.sprite.len() / (SPRITE_FLOATS_PER_VERTEX * 6) > MAIN_BUTTONS.len(),
        "only {} sprite quads for {} widgets plus the logo",
        geo.sprite.len() / (SPRITE_FLOATS_PER_VERTEX * 6),
        MAIN_BUTTONS.len()
    );
    assert_eq!(
        geo.colour.len(),
        geo.backdrop_floats
            + geometry(&title, V_W, V_H).len()
            - geometry_button_fill_floats(&title, V_W, V_H)
            - geo.backdrop_floats,
        "with a real atlas no button may fall back to a flat fill"
    );

    ui.enter_dev_world();
    ui.pause();
    let pause = build(&pause_frame(&nav), Some(&atlas), None, V_W, V_H);
    assert!(
        pause.sprite.len() / (SPRITE_FLOATS_PER_VERTEX * 6) > PAUSE_BUTTONS.len(),
        "the pause screen's nine widgets did not all draw a sprite"
    );
}

/// Floats the flat-fill fallback would contribute for `frame`'s slotted rows
/// (one quad each, plus a 4-quad outline for the selected one) — the term the
/// real-pack gate subtracts to say "no button fell back".
fn geometry_button_fill_floats(frame: &MenuFrame<'_>, _w: f32, _h: f32) -> usize {
    let slotted = frame.rows.iter().filter(|r| r.slot.is_some()).count();
    let selected = frame
        .rows
        .get(frame.selected)
        .is_some_and(|r| r.slot.is_some()) as usize;
    (slotted + selected * 4) * STRIDE * 6
}

/// A real single-colour PNG of arbitrary dimensions. `solid_png` below is
/// square-only and is what the favicon tests want; the button pack needs
/// 200×20.
fn solid_rgba_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("write header");
        let data: Vec<u8> = (0..w * h).flat_map(|_| rgba).collect();
        writer.write_image_data(&data).expect("write image");
    }
    out
}

#[test]
fn logical_canvas_shrinks_a_retina_style_framebuffer_back_to_visual_size() {
    // A 2x HiDPI display reports a framebuffer double an ordinary window's
    // physical size for the same visual window. Auto scale must pick up
    // roughly double the scale too, so the logical canvas (what `geometry`
    // actually lays fixed pixel constants into) lands close to the same
    // apparent size in both cases — this is the fix for the "menu draws
    // half-size on Retina" report.
    let lo_dpi = logical_canvas(0, 1280, 720);
    let hi_dpi = logical_canvas(0, 2560, 1440);
    // Not a no-op: the canvas must actually shrink relative to the raw
    // framebuffer, or this is the exact island the change was for.
    assert!(hi_dpi.0 < 2560.0 && hi_dpi.1 < 1440.0);
    // And the two logical canvases must be close in size, not 2x apart,
    // which is what "half size on Retina" looked like before this existed.
    assert!(
        (lo_dpi.0 - hi_dpi.0).abs() < lo_dpi.0 * 0.5,
        "logical canvases diverged: {lo_dpi:?} vs {hi_dpi:?}"
    );
}

#[test]
fn logical_canvas_is_the_identity_at_scale_one() {
    // A tiny framebuffer forces scale 1 (see `config`'s own tests), at
    // which point the logical canvas must equal the physical one exactly —
    // this is what keeps every fixed-size `geometry` test above valid.
    assert_eq!(logical_canvas(0, 200, 200), (200.0, 200.0));
}

#[test]
fn logical_canvas_never_divides_by_zero_for_a_degenerate_framebuffer() {
    let (w, h) = logical_canvas(0, 0, 0);
    assert!(w.is_finite() && h.is_finite());
}

#[test]
fn a_narrow_viewport_does_not_produce_out_of_range_geometry() {
    // Small windows are where layout arithmetic goes negative.
    for (w, h) in [(320.0f32, 240.0f32), (200.0, 900.0), (1.0, 1.0)] {
        let rows = vec![button("ONE"), button("TWO")];
        let v = geometry(&frame_with(rows, 0), w, h);
        for vert in v.chunks_exact(STRIDE) {
            assert!(
                vert[0].is_finite() && vert[1].is_finite(),
                "non-finite vertex at {w}x{h}"
            );
        }
    }
}

#[test]
fn an_empty_menu_still_clears_the_screen() {
    // Otherwise the last world frame stays on screen behind a blank menu.
    let f = frame_with(vec![], 0);
    let v = geometry(&f, 1280.0, 720.0);
    assert!(
        v.len() >= STRIDE * 6,
        "an empty menu must still emit the background"
    );
}


// -- world select (issue #397) --------------------------------------------

/// A nav and a `UiState` sitting on the world-select screen, reached the way
/// a player reaches it: by activating the title screen's Singleplayer button.
///
/// That is the anti-island premise for this whole screen — if the button no
/// longer opens it, every test below fails at this assertion rather than
/// quietly testing a screen nothing can reach.
fn world_select_nav(tag: &str) -> (MenuNav, UiState) {
    let mut nav = test_nav(tag);
    let mut ui = UiState::new();
    assert_eq!(
        nav.main_button(),
        crate::menu::nav::MainButton::Singleplayer,
        "premise: Singleplayer is the initially selected title-screen button"
    );
    let action = nav.key(&mut ui, MenuKey::Enter);
    assert_eq!(action, crate::menu::nav::MenuAction::None);
    assert_eq!(
        ui.screen(),
        Screen::WorldSelect,
        "the title screen's Singleplayer button must open the world list"
    );
    (nav, ui)
}

/// The same, with `names` planted in this nav's own (temp) saves root first —
/// so the list the screen enumerates is non-empty.
///
/// The worlds are written with the **production** codec
/// (`lodestone_anvil::level_dat`), because `crate::saves`' enumeration reads them
/// with it: a hand-rolled fixture here would be testing the fixture. `LastPlayed`
/// counts *down* with the index so `names[0]` sorts to row 0 under
/// `WorldSummary::cmp_for_list` (last played descending) — otherwise the row
/// order would depend on the write order, which is not what the screen sorts by.
fn world_select_nav_with_worlds(tag: &str, names: &[&str]) -> (MenuNav, UiState) {
    let nav = test_nav(tag);
    for (i, name) in names.iter().enumerate() {
        let dir = nav.saves_root().join(name);
        std::fs::create_dir_all(&dir).expect("create world dir");
        let mut level = lodestone_anvil::level_dat::LevelDat::for_new_world(
            name,
            &lodestone_anvil::level_dat::Spawn::default(),
            0,
        );
        level
            .set_last_played(1_700_000_000_000 - i as i64 * 1000)
            .expect("sets LastPlayed");
        lodestone_anvil::level_dat::write_to_file(
            &level,
            &lodestone_anvil::level_dat::path_in(&dir),
        )
        .expect("write level.dat");
    }
    let mut nav = nav;
    let mut ui = UiState::new();
    let action = nav.key(&mut ui, MenuKey::Enter);
    assert_eq!(action, crate::menu::nav::MenuAction::None);
    assert_eq!(ui.screen(), Screen::WorldSelect);
    assert_eq!(
        nav.world_select().shown_len(),
        names.len(),
        "premise: opening the screen enumerated every planted world"
    );
    (nav, ui)
}

fn world_select_frame(nav: &MenuNav, ui: &UiState) -> MenuFrame<'static> {
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    frame_for(ui, nav, &statuses, &mut fav).expect("the world list owns its frame")
}

/// Vanilla's own rects for `SelectWorldScreen`, hand-derived from the Java at
/// 854×480 rather than read back out of the layout — `CLAUDE.md`'s rule that
/// an expected value must originate outside the code under test.
///
/// The derivation, which is what a future reader has to be able to check:
///
/// - The header column is `LinearLayout.vertical().spacing(4)` holding a 9 px
///   `StringWidget` and a nested 200×20 row, so it measures 200×33 and the
///   header `FrameLayout` (854×49, `align(0.5, 0.5)`) puts it at
///   `((854-200)/2, (49-33)/2)` = (327, 8). The search box is one spacing plus
///   the title below that: y = 8 + 9 + 4 = **21**, *not* the 22 written at
///   `SelectWorldScreen.java:55`, because the layout overwrites it.
/// - The footer's four columns are all 71: Play's 150 px spanning two columns
///   with an 8 px gutter splits `Divisor(142, 2)` = 71/71, and the four 71 px
///   buttons can only match it. So the grid is `4*71 + 3*8` = **308** wide and
///   `20 + 4 + 20` = 44 tall, and the footer frame (854×60, pinned at y 420)
///   puts it at `((854-308)/2, 420 + (60-44)/2)` = (273, 428).
/// - Within it: row 1 cells start at 0 and 158 (`71+8+71+8`), row 2 cells at
///   0, 79, 158, 237, and row 2 is 24 px down.
/// - The content band's top is `min(headerHeight + 30, height - footerHeight -
///   contentHeight)` = `min(79, 480 - 60 - 371)` = **49**, i.e. flush under the
///   header, because vanilla sizes the list to `getContentHeight()` exactly.
/// - The first list row is at `getY() + 2` = 51, 270 wide
///   (`getRowWidth()`), 36 tall, centred: `427 - 135` = 292.
#[test]
fn the_world_select_rects_are_vanillas_own() {
    use crate::menu::world_select::WorldSelectButton as B;
    let expected = [
        (B::Play, (273.0, 428.0, 150.0, 20.0)),
        (B::Create, (431.0, 428.0, 150.0, 20.0)),
        (B::Edit, (273.0, 452.0, 71.0, 20.0)),
        (B::Delete, (352.0, 452.0, 71.0, 20.0)),
        (B::ReCreate, (431.0, 452.0, 71.0, 20.0)),
        (B::Back, (510.0, 452.0, 71.0, 20.0)),
    ];
    for (button, want) in expected {
        assert_eq!(
            world_select_slot(button).resolve(V_W, V_H),
            want,
            "{button:?} is not where vanilla puts it"
        );
    }
    // The footer's 8 px gutter, which is the pause screen's and not the title
    // screen's 4 — the same conflation `the_title_screen_rects_are_vanillas_own`
    // pins from the other side.
    let (ex, _, ew, _) = world_select_slot(B::Edit).resolve(V_W, V_H);
    let (dx, ..) = world_select_slot(B::Delete).resolve(V_W, V_H);
    assert_eq!(dx - (ex + ew), 8.0, "footer column gutter");

    assert_eq!(
        world_select_search_slot().resolve(V_W, V_H),
        (327.0, 21.0, 200.0, 20.0),
        "the search box is placed by the layout, not by its own constructor"
    );
    let title = world_select_title_label();
    assert_eq!(
        (title.origin.anchor(V_W, V_H).0 + title.dx, title.dy),
        (427.0, 8.0),
        "the title is centred at the top of the header band"
    );
    assert_eq!(title.align, Align::Centre);

    assert_eq!(world_list_row_rect(0, V_W, 0.0), (292.0, 51.0, 270.0, 36.0));
    assert_eq!(
        world_list_row_rect(1, V_W, 0.0),
        (292.0, 87.0, 270.0, 36.0),
        "rows stack by itemHeight with no gap"
    );
    assert_eq!(
        world_list_row_content_rect(0, V_W, 0.0),
        (294.0, 53.0, 266.0, 32.0),
        "CONTENT_PADDING insets the entry by 2, and 36 - 4 is the icon's 32"
    );
}

/// The slots must be the same at every canvas, or the screen is right at one
/// size and wrong everywhere else.
///
/// This is the condition `WORLD_SELECT_REF_CANVAS` rests on, and the only
/// thing that makes arranging a *canvas-dependent* container once legitimate.
/// 320×240 is the real floor `config::calculate_gui_scale` can produce; the
/// widths are even, because an odd logical width truncates in vanilla's
/// integer centring where `Origin`'s anchor does not — the same half-pixel
/// `title_slot` has always had.
#[test]
fn the_world_select_slots_do_not_depend_on_the_reference_canvas() {
    for (w, h) in [(320.0f32, 240.0f32), (854.0, 480.0), (1920.0, 1080.0)] {
        let block = WorldSelectBlock::at(w, h);
        for i in 0..2 {
            assert_eq!(
                block.header_slot(i),
                world_select_block().header_slot(i),
                "header slot {i} moved at {w}x{h}"
            );
        }
        for i in 0..crate::menu::world_select::WORLD_SELECT_BUTTONS.len() {
            assert_eq!(
                block.footer_slot(i),
                world_select_block().footer_slot(i),
                "footer slot {i} moved at {w}x{h}"
            );
        }
        assert_eq!(
            block.content_top,
            world_select_block().content_top,
            "the content band moved at {w}x{h}"
        );
    }
}

/// The frame is the screen vanilla draws: seven widgets in vanilla's order,
/// five of them present-and-disabled, at the rects the layout placed them.
#[test]
fn the_world_select_frame_is_the_screen_vanilla_draws() {
    use crate::menu::world_select::{SEARCH_FIELD, WORLD_SELECT_BUTTONS, WorldSelectButton};
    let (nav, ui) = world_select_nav("ws-frame");
    let f = world_select_frame(&nav, &ui);

    assert!(f.vanilla, "it reproduces one of vanilla's own screens");
    assert!(!f.logo, "the logo is the title screen's");
    assert_eq!(f.rows.len(), 1 + WORLD_SELECT_BUTTONS.len());

    // Row 0 is the search field, and it carries a real `EditBox` — the row
    // indices are `world_select`'s focus ids, so this is also the guard that
    // `app.rs`'s hit-test and the focus layer agree about what row 0 is.
    assert!(
        f.rows[SEARCH_FIELD].field && f.rows[SEARCH_FIELD].edit.is_some(),
        "row 0 must be the search box"
    );
    assert_eq!(
        f.selected, SEARCH_FIELD,
        "setInitialFocus puts the keyboard in the search box"
    );
    assert_eq!(f.hovered, None, "nothing is hovered before the mouse moves");

    // The six footer buttons, in vanilla's order, with vanilla's labels.
    let labels: Vec<&str> = f.rows[1..].iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "Play Selected World",
            "Create New World",
            "Edit",
            "Delete",
            "Re-Create",
            "Back",
        ]
    );
    // **Four** disabled here, not three, and that is the empty-list state rather
    // than a regression: `test_nav` points this nav at a temp data directory whose
    // `saves/` does not exist, so there is nothing selected and
    // `updateButtonStatus(null)` greys Play as well as Edit/Delete/Re-Create.
    // Create and Back stay live, which is what keeps a fresh install off a dead
    // end. `the_world_select_frame_with_worlds_lists_them_all` is the populated
    // arm, and between them they are also each other's control: the same frame
    // builder must answer differently for the two.
    let enabled: Vec<&str> = f.rows[1..]
        .iter()
        .filter(|r| r.enabled)
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(enabled, vec!["Create New World", "Back"]);
    assert!(
        !f.rows[WorldSelectButton::Edit.row()].enabled,
        "Edit must be present and disabled"
    );
    assert!(
        !f.rows[WorldSelectButton::Play.row()].enabled,
        "Play must be present and disabled with nothing to play"
    );

    // Every row's rect is the slot the layout placed it in, through the same
    // `row_rect` `app.rs` hit-tests with.
    assert_eq!(
        row_rect(&f.rows, SEARCH_FIELD, V_W, V_H),
        Some(world_select_search_slot().resolve(V_W, V_H))
    );
    for button in WORLD_SELECT_BUTTONS {
        assert_eq!(
            row_rect(&f.rows, button.row(), V_W, V_H),
            Some(world_select_slot(button).resolve(V_W, V_H)),
            "{button:?}'s row is not at its slot"
        );
    }

    // The two free-standing strings: the title, and `NoWorldsEntry`'s notice —
    // which is what keeps "no worlds" apart from "the list failed to draw".
    let texts: Vec<&str> = f.labels.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            crate::menu::world_select::WORLD_SELECT_TITLE,
            crate::menu::world_select::NO_WORLDS_LABEL,
        ]
    );
}

/// **The populated arm of the frame gate**: N worlds on disk become N rows, at
/// the rects `app.rs` hit-tests, carrying the three text lines
/// `WorldListEntry` draws.
///
/// This is the anti-island assertion for the save list: `crate::saves` can
/// enumerate perfectly and reach zero pixels if `frame_for` never emits a row for
/// a world. It also pins the **row order** — the ids run search → buttons →
/// worlds, which is *not* the on-screen order, and getting that wrong is #391 at
/// list scale (every click one control off).
#[test]
fn the_world_select_frame_with_worlds_lists_them_all() {
    use crate::menu::world_select::{FIRST_WORLD_ROW, WORLD_SELECT_BUTTONS, WorldSelectButton};
    let (nav, ui) = world_select_nav_with_worlds("ws-frame-worlds", &["alpha", "bravo"]);
    let f = world_select_frame(&nav, &ui);

    assert_eq!(
        f.rows.len(),
        1 + WORLD_SELECT_BUTTONS.len() + 2,
        "search field + six footer buttons + one row per world"
    );
    // The footer band is untouched by the list: every button is still at its own
    // slot, which is what would break if the world rows had been inserted between
    // the search field and the buttons.
    for button in WORLD_SELECT_BUTTONS {
        assert_eq!(
            row_rect(&f.rows, button.row(), V_W, V_H),
            Some(world_select_slot(button).resolve(V_W, V_H)),
            "{button:?}'s row moved when the list gained rows"
        );
    }
    // Play and Delete are live now (#468, #540), and Edit/Re-Create still are not.
    assert!(f.rows[WorldSelectButton::Play.row()].enabled);
    assert!(f.rows[WorldSelectButton::Delete.row()].enabled);
    assert!(!f.rows[WorldSelectButton::Edit.row()].enabled);
    assert!(!f.rows[WorldSelectButton::ReCreate.row()].enabled);

    // No `NoWorldsEntry` notice — the list is not empty.
    let texts: Vec<&str> = f.labels.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, vec![crate::menu::world_select::WORLD_SELECT_TITLE]);

    for row in 0..2 {
        let entry = &f.rows[FIRST_WORLD_ROW + row];
        let view = entry
            .world
            .as_ref()
            .expect("a world row must carry its WorldEntryView, or nothing draws");
        assert_eq!(view.index, row);
        assert_eq!(
            view.selected,
            row == 0,
            "row 0 is the selection (most recently played); row 1 is not"
        );
        assert!(entry.slot.is_none(), "a list entry is not a slotted button");
        // The three lines: display name, `folder (last played)`, `mode, Version`.
        let world = nav.world_select().world_at(row).expect("row exists");
        assert_eq!(entry.label, world.display_name);
        assert_eq!(entry.detail, world.detail_line());
        assert_eq!(entry.trailing, world.info_line());
        assert!(
            entry.detail.starts_with(&world.dir_name),
            "line 2 opens with the folder name: {:?}",
            entry.detail
        );
        assert!(
            entry.trailing.contains("Version: 26.2"),
            "line 3 carries the world version: {:?}",
            entry.trailing
        );
        // The rect is the list geometry, not a slot — and it is the same one the
        // hit-test reads.
        assert_eq!(
            row_rect(&f.rows, FIRST_WORLD_ROW + row, V_W, V_H),
            Some(world_list_row_rect(row, V_W, 0.0)),
            "world row {row} is not at its list rect"
        );
    }
    // `alpha` was planted first and therefore has the later `LastPlayed`, so it
    // sorts to row 0. Asserted so the ordering is not merely "some order".
    assert_eq!(f.rows[FIRST_WORLD_ROW].label, "alpha");
    assert_eq!(f.rows[FIRST_WORLD_ROW + 1].label, "bravo");

    // -- control ---------------------------------------------------------
    // A row beyond the list must report no rect at all, or a click below the last
    // world would hit-test onto a row that is nowhere near the cursor.
    assert_eq!(
        row_rect(&f.rows, FIRST_WORLD_ROW + 2, V_W, V_H),
        None,
        "there are only two worlds"
    );
    // And the visibility gate really does reject: at a canvas too short for row
    // 1, row 1 has no rect while row 0 still does.
    let short = world_list_row_top(1, 0.0) + 40.0;
    assert!(
        world_list_row_visible(0, V_H, 0.0) && !world_list_row_visible(1, short, 0.0),
        "the visibility gate does not discriminate: rows visible at {V_H} = {}, \
         at {short} = {}",
        world_list_visible_rows(V_H),
        world_list_visible_rows(short)
    );
    assert_eq!(row_rect(&f.rows, FIRST_WORLD_ROW + 1, V_W, short), None);
}

/// Every world-select button draws the sprite the widget layer picks, at the
/// rect the layout placed it in.
///
/// The same gate `every_title_and_pause_widget_draws_the_sprite_the_widget_layer_picks`
/// makes for the other two screens, and for the same reason: without it
/// `world_select_slot` and `WorldSelectButton::enabled` could both be correct
/// and reach zero pixels. The `enabled` flags come from the **real frame**, so
/// this cannot drift from what the screen actually says.
#[test]
fn every_world_select_button_draws_the_sprite_the_widget_layer_picks() {
    use crate::menu::world_select::WORLD_SELECT_BUTTONS;
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    let (nav, ui) = world_select_nav("ws-sprites");
    let frame = world_select_frame(&nav, &ui);

    // The premise: the screen really does carry a mix, or "the disabled
    // sprite was chosen" is never exercised.
    assert!(
        frame.rows[1..].iter().any(|r| r.enabled) && frame.rows[1..].iter().any(|r| !r.enabled),
        "this screen no longer has both an enabled and a disabled button"
    );
    // And the rects are really distinct, or a widget stuck at one position
    // could still pass.
    let distinct: std::collections::BTreeSet<(i32, i32)> = WORLD_SELECT_BUTTONS
        .iter()
        .map(|b| {
            let (x, y, ..) = world_select_slot(*b).resolve(V_W, V_H);
            (x as i32, y as i32)
        })
        .collect();
    assert_eq!(distinct.len(), WORLD_SELECT_BUTTONS.len());

    for button in WORLD_SELECT_BUTTONS {
        let row = frame.rows[button.row()].clone();
        let enabled = row.enabled;
        for focused in [false, true] {
            let mut f = frame_with(vec![row.clone()], if focused { 0 } else { 99 });
            f.vanilla = true;
            let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;

            let expected = widget::BUTTON_SPRITES.get(enabled, focused);
            let (min, max) = sprite_uv_bounds(&atlas, expected);
            assert!(
                all_uvs_within(&sprite, min, max),
                "{button:?} (enabled={enabled}, focused={focused}) did not sample \
                 {expected}, which is what WidgetSprites::get selects"
            );
            // Per-case control: flipping `active` must move the sample off
            // this region. For the five disabled buttons this is the #397
            // assertion run in reverse — an enabled Create New World must
            // *not* sample `widget/button_disabled`.
            let flipped = widget::BUTTON_SPRITES.get(!enabled, focused);
            if flipped != expected {
                let (fmin, fmax) = sprite_uv_bounds(&atlas, flipped);
                assert!(
                    !all_uvs_within(&sprite, fmin, fmax),
                    "the detector cannot tell {expected} from {flipped}"
                );
            }

            let drawn = sprite_dest_bounds(&sprite, V_W, V_H);
            let want = world_select_slot(button).resolve(V_W, V_H);
            let same = [
                (drawn.0, want.0),
                (drawn.1, want.1),
                (drawn.2, want.2),
                (drawn.3, want.3),
            ]
            .iter()
            .all(|(a, b)| (a - b).abs() < 0.01);
            assert!(
                same,
                "{button:?} (enabled={enabled}, focused={focused}) drew at {drawn:?}, \
                 not at {want:?} where the layout placed it"
            );
        }
    }
}

/// A disabled world-select button's label is vanilla's grey, and it is that
/// exact value.
///
/// Predicted, not asserted as a direction — `CLAUDE.md`'s *magnitude*
/// species. The expectation comes from `AbstractWidget.java:318`'s
/// `-6250336` unpacked by `widget::argb_to_rgba`, and the enabled button
/// beside it is the control that says the measurement can tell them apart.
#[test]
fn a_disabled_world_select_label_lands_on_vanillas_grey() {
    use crate::menu::world_select::WorldSelectButton as B;
    let (nav, ui) = world_select_nav("ws-grey");
    let frame = world_select_frame(&nav, &ui);
    let grey = widget::argb_to_rgba(widget::INACTIVE_MESSAGE_ARGB);
    assert_eq!(grey, widget::INACTIVE_LABEL);

    for (button, want, name) in [
        // Issue #190 made Create live; Edit is still present-and-disabled
        // and takes over as the disabled example here.
        (B::Edit, grey, "disabled"),
        (B::Back, widget::ACTIVE_LABEL, "enabled"),
    ] {
        let row = frame.rows[button.row()].clone();
        let rect = world_select_slot(button).resolve(V_W, V_H);
        let mut f = frame_with(vec![row], 99);
        f.vanilla = true;
        let colour = build(&f, None, None, V_W, V_H).colour;
        assert!(
            coverage_of(&colour, V_W, V_H, rect, want) > 0.0,
            "{button:?}'s {name} label did not reach {want:?} inside {rect:?}"
        );
        // The control: the *other* colour must not appear in the same rect,
        // or "the label is grey" is satisfied by a frame containing both.
        let other = if want == grey {
            widget::ACTIVE_LABEL
        } else {
            grey
        };
        assert_eq!(
            coverage_of(&colour, V_W, V_H, rect, other),
            0.0,
            "{button:?} drew {other:?} as well, so the colour is not a discriminator"
        );
    }
}

/// The list draws its one row, inside row 0's own content rect.
///
/// This is the assertion that keeps "the list has a world" distinguishable
/// from "the list failed to draw" — without it the two are the same picture,
/// which is exactly the absence-needs-a-control rule. It is also the pixel
/// half of #287's world list: the button that launches is only honest if the
/// world it launches is on screen. The band is the row's content rect from
/// `world_list_row_content_rect`, the same expression the label's position is
/// derived from, and the failure output is a bounding box rather than a
/// fraction.
///
/// Two controls, both executed: the band *below* the row must be empty (so
/// this is not measuring a frame that paints everywhere), and the same band
/// on the **title screen** must be empty too (so it is not measuring
/// something every menu draws there).
#[test]
fn the_empty_world_list_draws_its_notice_inside_row_zeros_content_rect() {
    let (nav, ui) = world_select_nav("ws-row");
    assert_eq!(
        nav.world_select().shown_len(),
        0,
        "premise: this nav's temp saves root is empty, so the notice is what draws"
    );
    let frame = world_select_frame(&nav, &ui);
    let colour = geometry(&frame, V_W, V_H);

    let band = world_list_row_content_rect(0, V_W, 0.0);
    let inside = band_coverage(&colour, V_W, V_H, band);
    assert!(
        inside.count > 0,
        "the empty-list notice reached no pixels inside {band:?}"
    );
    let bounds = inside.bounds.expect("a non-empty band has bounds");
    // It is a line of text, not a full-height fill: the notice is 9 px of
    // glyphs centred in a 32 px box, so its vertical extent must be well
    // short of the band's.
    assert!(
        bounds.3 - bounds.1 < band.3 * 0.75,
        "what drew in {band:?} spans {:?} vertically — that is a fill, not a line of text",
        (bounds.1, bounds.3)
    );
    // And it is centred, so it must straddle the screen's own centre line.
    assert!(
        bounds.0 < V_W * 0.5 && bounds.2 > V_W * 0.5,
        "the notice is not centred: bounds {bounds:?}"
    );

    // -- control 1: the row below it is empty ----------------------------
    let empty_band = world_list_row_content_rect(1, V_W, 0.0);
    assert_eq!(
        band_coverage(&colour, V_W, V_H, empty_band).count,
        0,
        "something drew in row 1 as well, so the band is not a discriminator: {:?}",
        band_coverage(&colour, V_W, V_H, empty_band).bounds
    );

    // -- control 2: the same band on the title screen is empty -----------
    // What else already paints here? On the title screen, nothing: the logo
    // ends at y 94 and the button column starts at 168, and row 0's content
    // rect is y 53..85. If that ever stops being true this fires, which is
    // the point.
    let title_nav = test_nav("ws-empty-control");
    let title_ui = UiState::new();
    assert_eq!(title_ui.screen(), Screen::MainMenu, "the control is the title");
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let title = frame_for(&title_ui, &title_nav, &statuses, &mut fav).expect("title frame");
    let title_colour = geometry(&title, V_W, V_H);
    assert_eq!(
        band_coverage(&title_colour, V_W, V_H, band).count,
        0,
        "the title screen already paints in {band:?}, so control 1 measures nothing: {:?}",
        band_coverage(&title_colour, V_W, V_H, band).bounds
    );
}

/// **The save list reaches pixels, one band per world.**
///
/// This is the re-derivation of `the_world_list_draws_its_one_row_…` for N rows,
/// and it is a strictly stronger gate rather than the same one loosened: the
/// single-row version could not distinguish "the list drew" from "row 0 drew",
/// which is exactly the failure a hardcoded row has. Every row's band is measured
/// separately, so a list that draws only its first entry fails.
///
/// Three things are asserted per row, and the second is what a coverage *fraction*
/// could never see:
///
/// 1. ink inside that row's own content rect;
/// 2. the ink starts to the **right** of the reserved 32 px icon column
///    (`getTextX() = contentX + 32 + 3`) — measured by location, not by amount,
///    which is `CLAUDE.md`'s "ask where, not what";
/// 3. it is three short lines of text, not a fill: the vertical extent spans more
///    than one 9 px line and less than the band.
///
/// The controls: the band **after** the last world must be empty (so the bands
/// discriminate at all), and the same bands on the title screen must be empty (so
/// this is not measuring something every menu paints there — the question
/// `CLAUDE.md` says to ask before believing a control).
#[test]
fn every_world_in_the_list_draws_inside_its_own_row_band() {
    use crate::menu::render::WORLD_LIST_TEXT_DX;
    let names = ["alpha", "bravo", "charlie"];
    let (nav, ui) = world_select_nav_with_worlds("ws-rows-pixels", &names);
    let frame = world_select_frame(&nav, &ui);
    let colour = geometry(&frame, V_W, V_H);

    for row in 0..names.len() {
        let band = world_list_row_content_rect(row, V_W, 0.0);
        let inside = band_coverage(&colour, V_W, V_H, band);
        assert!(
            inside.count > 0,
            "world row {row} reached no pixels inside {band:?}"
        );
        let bounds = inside.bounds.expect("a non-empty band has bounds");
        // (2) The text column starts past the icon column, for **every** row
        // including the selected one: `extractItem`'s outline is drawn at the *row*
        // rect's edge, which is `CONTENT_PADDING` outside this band, and its
        // interior fill is black — so the leftmost thing inside a content rect is
        // always `getTextX()`. That equality is checked rather than an inequality
        // where it can be: `getTextX()` is `contentX + 32 + 3` exactly.
        assert!(
            bounds.0 >= band.0 + WORLD_LIST_TEXT_DX,
            "row {row}'s text must start past the {WORLD_LIST_TEXT_DX} px icon \
             column: leftmost ink at {} for a band starting at {}",
            bounds.0,
            band.0
        );
        // (3) Three lines of 9 px text in a 32 px box: taller than one line,
        // shorter than the band.
        let ink_h = bounds.3 - bounds.1;
        assert!(
            ink_h > 9.0 && ink_h <= band.3,
            "row {row} drew {ink_h} px vertically in a {} px band — three text \
             lines measure more than one line and no more than the band",
            band.3
        );
    }

    // The selection outline — `AbstractSelectionList.extractItem` (`:354-370`) —
    // is drawn for the selected row **only**, at the row rect's own left edge.
    // Sampled by colour at a 1 px column there, which is a different measurement
    // from the vertex bands above and is the one that discriminates a selected row
    // from an unselected one. Its own control is the second assertion: row 1 is not
    // selected and must not have it.
    let selected_edge = |row: usize| {
        let (rx, ry, _, rh) = world_list_row_rect(row, V_W, 0.0);
        coverage_of(&colour, V_W, V_H, (rx, ry + 2.0, 1.0, rh - 4.0), [1.0, 1.0, 1.0, 1.0])
    };
    assert!(
        selected_edge(0) > 0.5,
        "row 0 is the selection and must draw `extractItem`'s white outline at the \
         row's left edge: coverage {}",
        selected_edge(0)
    );
    assert!(
        selected_edge(1) < 0.1,
        "row 1 is not selected and must have no outline: coverage {}",
        selected_edge(1)
    );

    // -- control 1: the band after the last world is empty ---------------
    let after = world_list_row_content_rect(names.len(), V_W, 0.0);
    let empty = band_coverage(&colour, V_W, V_H, after);
    assert_eq!(
        empty.count, 0,
        "something drew in the band after the last world, so the per-row bands are \
         not discriminators: {:?}",
        empty.bounds
    );

    // -- control 2: the same bands on the title screen are empty ---------
    // Asked of *every* band this test measures, not only row 0's: rows 1 and 2 sit
    // lower on screen, where the title screen's button column begins at y 168, so
    // "what else already paints here" has a different answer per row.
    let title_nav = test_nav("ws-rows-pixels-control");
    let title_ui = UiState::new();
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let title = frame_for(&title_ui, &title_nav, &statuses, &mut fav).expect("title frame");
    let title_colour = geometry(&title, V_W, V_H);
    for row in 0..names.len() {
        let band = world_list_row_content_rect(row, V_W, 0.0);
        let painted = band_coverage(&title_colour, V_W, V_H, band);
        assert_eq!(
            painted.count, 0,
            "the title screen already paints in row {row}'s band {band:?}, so this \
             test's assertion for that row measures nothing: {:?}",
            painted.bounds
        );
    }
}

/// The empty-list notice fits the row it is centred in.
///
/// Vanilla's `NoWorldsEntry` gives its `StringWidget` no `maxWidth`
/// (`WorldSelectionList.java:382-384`), so nothing clips it and a longer
/// string would overhang the row. Measured with [`text_px`], the same
/// fixed-advance measure the jar-less draw uses — the real vanilla font is
/// narrower, so this is the conservative direction.
///
/// `BUNDLED_WORLD.label` is measured too even though nothing draws it any more:
/// its length was the reason that constant was written the way it was, and the
/// measurement is the only place that fact survives.
#[test]
fn the_world_list_row_label_fits_the_row_it_is_centred_in() {
    let (.., content_w, _) = world_list_row_content_rect(0, V_W, 0.0);
    // **Both** empty-list strings, not just this target's. The label is
    // `cfg`-selected (a browser says the list is never saved, not "not yet"), and this
    // test runs on the host — so measuring `NO_WORLDS_LABEL` alone would leave the
    // browser string unmeasured on every machine that runs the suite. The first draft
    // of it was 53 characters against a 44-character row and nothing would have
    // caught it. A guard only covers what it names.
    for label in [
        crate::menu::world_select::NO_WORLDS_LABEL_NATIVE,
        crate::menu::world_select::NO_WORLDS_LABEL_BROWSER,
        crate::menu::world_select::BUNDLED_WORLD.label,
    ] {
        let measured = text_px(label, 1.0);
        assert!(
            measured <= content_w,
            "{label:?} measures {measured} px in a {content_w} px row"
        );
    }
}

/// A world row's three lines are clipped to `WorldListEntry`'s own
/// `maxTextWidth`, so a long name cannot overhang the row.
///
/// Unlike the notice above, these lines *are* clipped in vanilla
/// (`StringWidget.setMaxWidth`, `WorldSelectionList.java:418`), so the assertion
/// is about the **draw** rather than about the string: a 200-character world name
/// must still paint nothing outside its row.
#[test]
fn a_long_world_name_is_clipped_to_its_row_rather_than_overhanging_it() {
    let long: String = std::iter::repeat_n('W', 200).collect();
    let (nav, ui) = world_select_nav_with_worlds("ws-long-name", &[long.as_str()]);
    let frame = world_select_frame(&nav, &ui);
    let colour = geometry(&frame, V_W, V_H);

    let (rx, ry, rw, rh) = world_list_row_rect(0, V_W, 0.0);
    // The band immediately to the right of the row must be untouched.
    let right_of = (rx + rw, ry, 80.0, rh);
    let spill = band_coverage(&colour, V_W, V_H, right_of);
    assert_eq!(
        spill.count, 0,
        "a long world name spilled out of its row into {right_of:?}: {:?}",
        spill.bounds
    );

    // -- control ---------------------------------------------------------
    // The band it must *not* spill into has to be one this frame could paint in,
    // or the assertion is vacuous. The row itself is inked, and the clip width is
    // less than the row width — so there is real text being cut.
    assert!(
        band_coverage(&colour, V_W, V_H, world_list_row_content_rect(0, V_W, 0.0)).count > 0,
        "the row drew nothing at all, so the no-spill assertion measures nothing"
    );
    assert!(
        text_px(&long, 1.0) > crate::menu::render::world_list_text_width(),
        "the name is not actually long enough to be clipped, so nothing was cut"
    );
}

/// **The settings overlap gate** (the player report of 2026-08-07): a settings
/// list row is **cut** at the band's bottom rather than painted into the gap above
/// the footer.
///
/// This is the pixel half of the defect `Origin::is_scrolling_list_row` fixed.
/// Every settings-tree list draws its rows as *slotted widgets*, so they went down
/// `draw`'s unclipped path while the three `MenuRow::entry`/`account`/`world`
/// screens were clipped — and a row that overran the band painted straight over
/// the footer's Done button.
///
/// **What else already paints here** was asked first, and it is why the rect is
/// the gap *between* the band's bottom and the footer's own top rather than
/// "everything below the band": the footer paints in the latter. Both edges are
/// derived — the band from the `ListSpec::model` the clip is built from, the footer
/// from `options::footer_rects` — and the control is the same frame with its
/// `ListSpec` removed, which takes the unclipped branch and must fail the same
/// assertion.
#[test]
fn a_settings_row_is_cut_at_the_bands_bottom_rather_than_painted_over_the_footer() {
    use crate::menu::options::{self, SettingsPage};

    const H: f32 = 480.0;
    let page = SettingsPage::Video;
    let mut nav = test_nav("settings-clip-pixels");
    let mut ui = UiState::new();
    ui.open_settings();
    // Reached the way a player reaches it: the root page's own Video nav button,
    // found in the row list `app.rs` hit-tests rather than by index. That makes
    // this an anti-island premise too — if the button no longer opens the page,
    // the setup fails instead of the assertion.
    let video_row = nav
        .settings()
        .visible()
        .iter()
        .position(|c| {
            matches!(c.cell, options::Cell::Nav { page: Some(p), .. } if p == page)
        })
        .expect("premise: the root page carries a Video nav button");
    nav.click(&mut ui, video_row);
    assert_eq!(nav.settings().page(), page, "premise: the Video page is up");

    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let frame = frame_for(&ui, &nav, &statuses, &mut fav).expect("settings owns its frame");
    let spec = frame
        .list
        .as_ref()
        .expect("premise: the settings page declares a ListSpec");
    let model = spec.model(H).expect("premise: the Video page scrolls at 480");
    let band_bottom = model.top() + model.height();
    let footer_top = options::footer_rects(V_W, H, 1)
        .first()
        .expect("the footer has a Done button")
        .1;
    assert!(
        footer_top > band_bottom,
        "premise: there must be a gap between the band's bottom ({band_bottom}) and \
         the footer's top ({footer_top})"
    );
    let row_x = options::row_left(V_W, 0);
    let gutter = (row_x, band_bottom, options::BIG_BUTTON_WIDTH, footer_top - band_bottom);

    // Premise: some entry really does straddle the band's bottom edge at this
    // offset, or there is nothing for a clip to cut.
    let entries = page.entries();
    let straddler = (0..entries.len()).find(|e| {
        let (_, y) = options::list_cell_origin(page, *e, 0.0, 0, V_W, H);
        y < band_bottom && y + options::WIDGET_H > band_bottom
    });
    assert!(
        straddler.is_some(),
        "premise: no Video entry straddles the band's bottom at scroll 0, so this \
         gate cannot see a clip"
    );

    let clipped = band_coverage(&geometry(&frame, V_W, H), V_W, H, gutter);
    assert_eq!(
        clipped.count, 0,
        "a settings row painted into the gutter between the list band and the \
         footer: {:?}",
        clipped.bounds
    );

    // -- control, executed ---------------------------------------------------
    let mut unclipped = frame.clone();
    unclipped.list = None;
    let spilled = band_coverage(&geometry(&unclipped, V_W, H), V_W, H, gutter);
    assert!(
        spilled.count > 0,
        "the unclipped frame painted nothing in the gutter either, so the clip is \
         not what the assertion above measured"
    );

    // And the footer's own row is **not** clipped away — the predicate excludes it
    // deliberately, and erasing it would be the opposite defect.
    let done_rect = (footer_top - 1.0, options::DONE_WIDTH);
    let done = band_coverage(
        &geometry(&frame, V_W, H),
        V_W,
        H,
        ((V_W - done_rect.1) * 0.5, done_rect.0, done_rect.1, options::WIDGET_H + 2.0),
    );
    assert!(
        done.count > 0,
        "the footer's Done button was clipped away with the list rows — a footer \
         button is not a list row"
    );
}

/// Which visible row on the current settings page is the nav button for `page`.
fn settings_nav_row(nav: &MenuNav, page: crate::menu::options::SettingsPage) -> Option<usize> {
    use crate::menu::options;
    nav.settings()
        .visible()
        .iter()
        .position(|c| matches!(c.cell, options::Cell::Nav { page: Some(p), .. } if p == page))
}

/// Open the settings tree on `page` the way a player does — by **clicking** nav
/// buttons found in the row list `app` hit-tests, never by writing a page into a
/// field.
///
/// An anti-island premise: if the button that reaches a page stops opening it, the
/// setup fails instead of the assertion. Two levels deep, because two pages are not
/// on the root grid — Mouse hangs off Controls and Online off the root's own header
/// button — and hardcoding a parent table would be the thing this is avoiding. The
/// walk restarts from a fresh screen per candidate parent rather than backing out,
/// so a page reached down the wrong branch cannot leave a nav stack behind.
fn settings_nav_on(page: crate::menu::options::SettingsPage) -> (MenuNav, crate::menu::UiState) {
    use crate::menu::options;
    let fresh = || {
        let nav = test_nav("settings-band-chrome");
        let mut ui = crate::menu::UiState::new();
        ui.open_settings();
        (nav, ui)
    };
    let (mut nav, mut ui) = fresh();
    if let Some(row) = settings_nav_row(&nav, page) {
        nav.click(&mut ui, row);
        assert_eq!(nav.settings().page(), page, "premise: {page:?} is up");
        return (nav, ui);
    }
    let parents: Vec<options::SettingsPage> = nav
        .settings()
        .visible()
        .iter()
        .filter_map(|c| match c.cell {
            options::Cell::Nav { page: Some(p), .. } => Some(p),
            _ => None,
        })
        .collect();
    for parent in parents {
        let (mut n, mut u) = fresh();
        let Some(row) = settings_nav_row(&n, parent) else {
            continue;
        };
        n.click(&mut u, row);
        if let Some(row) = settings_nav_row(&n, page) {
            n.click(&mut u, row);
            if n.settings().page() == page {
                return (n, u);
            }
        }
    }
    panic!("premise: no nav button within two clicks of the root opens {page:?}");
}

/// **The reported bug** (2026-08-09): *"some settings menus (like the Music &
/// Sounds) still overlaps buttons at the bottom … in vanilla the button(s) anchored
/// at the bottom have their own section, with a horizontal bar separating it … the
/// middle section has a bit of a black filter in the background that tints the
/// panorama"*.
///
/// The band's chrome, measured **by location and by exact colour**: the two
/// separators sit on the band's own edges and the tint fills the band and stops
/// there. That is what makes header / content / footer read as three sections, and
/// it is the piece that was missing — the clip already reserved the footer band
/// (see `every_settings_page_keeps_its_footer_band_clear_at_every_scroll_offset`),
/// so a row cut flush against an untinted, unfenced edge was the whole of what the
/// owner saw.
///
/// Every expected value originates **outside this crate**: the four colours are the
/// pixel values of `textures/gui/{header,footer}_separator.png` and
/// `menu_list_background.png` decoded out of the 26.2 `client.jar`, and the two
/// y offsets are `extractListSeparators`' own `getY() - 2` and `getBottom()`. The
/// rects come from `ListSpec::chrome_rect`, the same call the draw makes.
///
/// **What else paints here** was asked first, and it decides every sample point:
/// the separator rows are outside the band, where only the title (centred, 9 px
/// tall, in the 33 px header) and the footer's Done (a 200 px button 7 px lower)
/// paint — so a sample at the canvas's left edge is clear of both. The tint sample
/// is likewise in the left margin, clear of the 310 px row column.
#[test]
fn the_settings_band_carries_vanillas_separators_and_its_tint() {
    use crate::menu::options::SettingsPage;

    // A canvas short enough that Sound overflows its band, so this is the same
    // frame the report was made against rather than a roomy one.
    const W: f32 = 320.0;
    const H: f32 = 240.0;
    let (nav, ui) = settings_nav_on(SettingsPage::Sound);
    let statuses = crate::menu::status::StatusCache::with_probe(
        crate::menu::status::unavailable_probe(),
    );
    let mut fav = FaviconCache::new();
    let frame = frame_for(&ui, &nav, &statuses, &mut fav).expect("settings owns its frame");
    let spec = frame.list.as_ref().expect("premise: Sound declares a ListSpec");
    let list = spec.model(H).expect("premise: Sound has a band at 240");
    let (cx, cy, cw, ch) = spec
        .chrome_rect(&list, W)
        .expect("premise: a settings list declares canvas-wide chrome");
    assert_eq!(
        (cx, cy, cw, ch),
        (0.0, 33.0, W, H - 33.0 - 33.0),
        "the chrome rect is not the 33 px-header / 33 px-footer band vanilla's \
         OptionsList is sized to"
    );
    let v = geometry(&frame, W, H);

    // A column in the left margin: outside the 310 px row column, so nothing but
    // the chrome can paint here.
    let probe_x = 4.0;
    let at = |y: f32| {
        colour_at(&v, 2.0 * probe_x / W - 1.0, 1.0 - 2.0 * y / H)
    };
    let expect = |y: f32, want: [f32; 4], what: &str| {
        let got = at(y);
        assert!(
            got.is_some_and(|c| c.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.002)),
            "{what} at y={y} is {got:?}, not the decoded {want:?}"
        );
    };
    // `header_separator.png` is light over dark; `footer_separator.png` is the
    // mirror. Sampled at row centres, since a quad spans `y..y+1`.
    expect(cy - SEPARATOR_H + 0.5, SEPARATOR_LIGHT, "the header bar's light row");
    expect(cy - 1.0 + 0.5, SEPARATOR_DARK, "the header bar's dark row");
    expect(cy + ch + 0.5, SEPARATOR_DARK, "the footer bar's dark row");
    expect(cy + ch + 1.5, SEPARATOR_LIGHT, "the footer bar's light row");
    // And the tint, over the band's whole height in that column.
    let margin = (0.0, cy, options::row_left(W, 0), ch);
    let tinted = coverage_of(&v, W, H, margin, LIST_BAND_TINT);
    assert_eq!(
        tinted, 1.0,
        "the band's left margin {margin:?} is only {tinted} tinted — the black filter \
         over the panorama does not cover the band"
    );

    // The tint **stops** at the band. Two rows that must not carry it: one above the
    // header bar, one inside the footer band beside the Done button. This is the
    // "own section" half of the report; a tint that ran the whole canvas would
    // satisfy the assertion above and still be wrong.
    for (y, what) in [
        (cy - SEPARATOR_H - 1.5, "above the header bar"),
        (cy + ch + SEPARATOR_H + 1.5, "below the footer bar"),
    ] {
        let got = at(y);
        assert!(
            !got.is_some_and(|c| c.iter().zip(LIST_BAND_TINT).all(|(a, b)| (a - b).abs() < 0.002)),
            "the band tint leaked {what} (y={y}), so the content band is not fenced off"
        );
    }

    // -- control, executed ---------------------------------------------------
    // The same frame with its `ListSpec` removed draws no chrome at all, so every
    // assertion above is measuring the chrome rather than something else that
    // happens to paint at those rows. Observed failing: `expect` on the header bar's
    // light row reports `None` here.
    let mut bare = frame.clone();
    bare.list = None;
    let bv = geometry(&bare, W, H);
    let bare_at = colour_at(&bv, 2.0 * probe_x / W - 1.0, 1.0 - 2.0 * (cy - SEPARATOR_H + 0.5) / H);
    assert!(
        bare_at.is_none(),
        "a frame with no ListSpec still painted {bare_at:?} where the header bar goes, \
         so the chrome is not what the assertions above measured"
    );
    let bare_tint = coverage_of(&bv, W, H, margin, LIST_BAND_TINT);
    assert_eq!(
        bare_tint, 0.0,
        "a frame with no ListSpec still tinted {bare_tint} of the band"
    );
}

/// **Hover tooltips on the settings tree** — the fourth item of the 2026-08-09
/// report: *"some inputs/buttons have tooltips when i hover"*.
///
/// Four claims, each measured by location and by the tooltip fill's own colour, and
/// the last two are the controls:
///
/// 1. hovering a row that carries an `OptionInstance` tooltip paints a `TOOLTIP_BG`
///    box, and it lands **at the cursor** rather than somewhere plausible;
/// 2. its content is really wrapped — a multi-line string produces a box taller than
///    a one-line one, so `Tooltip.MAX_WIDTH` is being applied rather than ignored;
/// 3. hovering a row with **no** tooltip in the table paints no box (so the box is
///    not unconditional);
/// 4. hovering where a row *used to be* before the list scrolled paints no box (so
///    the band guard in `menu_row_under` reaches this path too — a hint must not
///    hang over the footer for a row nothing can click).
///
/// The expected values originate outside this crate: which accessors have tooltips
/// and what their text is came out of `Options.java`'s `cachedConstantTooltip` sites
/// resolved through `en_us.json`, and the box geometry is
/// `TooltipRenderUtil`'s own padding and mouse offset. The cursor is set through
/// `MenuNav::set_menu_cursor`, the same call `app`'s mouse-move path makes.
#[test]
fn hovering_a_settings_row_shows_its_option_tooltip_and_only_then() {
    use crate::menu::options::{self, SettingsPage};

    const W: f32 = 854.0;
    const H: f32 = 480.0;
    let statuses = crate::menu::status::StatusCache::with_probe(
        crate::menu::status::unavailable_probe(),
    );

    // Accessibility carries the most tooltipped rows of any page.
    let (mut nav, ui) = settings_nav_on(SettingsPage::Accessibility);

    // The two subject rows, found by asking the same `Cell::tooltip` the frame
    // stamps — one with a tooltip and one without.
    let visible = nav.settings().visible();
    let tipped = visible
        .iter()
        .position(|c| c.cell.tooltip().is_some())
        .expect("premise: some Accessibility row has a tooltip");
    let without = visible
        .iter()
        .position(|c| c.cell.tooltip().is_none())
        .expect("premise: some Accessibility row has no tooltip");

    // Put the cursor in the middle of the row's own rect — from `row_rect`, the
    // function the hit-test and the draw both place rows by — through
    // `set_menu_cursor`, the call `app`'s mouse-move path makes. Returns the
    // `TOOLTIP_BG` fill's own bounding box, which is one quad, so `colour_bounds`
    // gives it exactly rather than by sampling.
    let hover_box = |nav: &mut MenuNav,
                     ui: &crate::menu::UiState,
                     row: usize|
     -> (Option<(f32, f32, f32, f32)>, (f32, f32)) {
        let probe = frame_for(ui, nav, &statuses, &mut FaviconCache::new()).expect("a frame");
        let (rx, ry, rw, rh) = row_rect(&probe.rows, row, W, H).expect("the row has a rect");
        nav.set_menu_cursor(rx + rw * 0.5, ry + rh * 0.5, W, H);
        let f = frame_for(ui, nav, &statuses, &mut FaviconCache::new()).expect("a frame");
        let at = f.cursor.expect("the cursor is stamped on the frame");
        (
            colour_bounds(&geometry(&f, W, H), W, H, TOOLTIP_BG),
            at,
        )
    };

    let (tip_box, at) = hover_box(&mut nav, &ui, tipped);
    let (bx, by, _, bh) =
        tip_box.expect("hovering a row with a tooltip painted no TOOLTIP_BG box at all");
    // (1) It is **at the cursor**, not somewhere plausible. `TooltipRenderUtil`'s
    // content box starts `MOUSE_OFFSET` right of and above the cursor and the fill is
    // `PAD` larger on every side, so both edges are exact — this canvas is wide and
    // tall enough that `draw_tooltip`'s off-screen nudge cannot fire.
    //
    // The tolerance is the pixel -> NDC -> pixel round trip inside `colour_bounds`,
    // which costs ~4e-6 at this canvas. It is not slack in the prediction.
    let want = (
        at.0 + TOOLTIP_MOUSE_OFFSET - TOOLTIP_PAD,
        at.1 - TOOLTIP_MOUSE_OFFSET - TOOLTIP_PAD,
    );
    assert!(
        (bx - want.0).abs() < 0.001 && (by - want.1).abs() < 0.001,
        "the tooltip's fill is at ({bx}, {by}), not the {want:?} the cursor {at:?} \
         implies"
    );

    // (2) The box's height is **predicted**, and the two hypotheses differ at this
    // input. `ClientTextTooltip.getHeight` is 8 px for one line and
    // `TOOLTIP_LINE_H * n` for n, and the fill adds `2 * PAD`, so:
    //
    // - wrapped at `Tooltip.MAX_WIDTH`: `10n + 6`
    // - not wrapped at all, the whole string on one line: `8 + 6` = 14
    //
    // The line count comes from `wrap_measured` at `TOOLTIP_MAX_WIDTH` — the same call
    // the draw makes, in the same font this `geometry` measures with — so the two
    // cannot disagree about the *split*; what is predicted from outside is vanilla's
    // height formula and its 170 px maximum. The `n > 1` assertion is what stops this
    // being an input where both hypotheses coincide, which is the trap that makes a
    // magnitude gate vacuous.
    let text = nav.settings().visible()[tipped]
        .cell
        .tooltip()
        .expect("premise: the row still has its tooltip");
    let lines =
        wrap_measured(&Quads::new(W, H), text, options::TOOLTIP_MAX_WIDTH, usize::MAX).len();
    assert!(
        lines > 1,
        "premise: {text:?} fits on one line in this font, so the wrapped and \
         unwrapped hypotheses agree and the height below measures nothing"
    );
    let wrapped = TOOLTIP_LINE_H * lines as f32 + 2.0 * TOOLTIP_PAD;
    let unwrapped = 8.0 + 2.0 * TOOLTIP_PAD;
    assert!(
        (bh - wrapped).abs() < 0.001,
        "the tooltip box is {bh} px, not the {wrapped} that {lines} wrapped lines \
         imply (an unsplit one would be {unwrapped})"
    );

    // (3) The control: a row with no tooltip in the table shows none. Without this,
    // (1) would pass on a box painted unconditionally.
    let (none_box, _) = hover_box(&mut nav, &ui, without);
    assert_eq!(
        none_box, None,
        "a row with no tooltip still painted a box at {none_box:?}"
    );

    // (4) The control that matters most: scroll the list, then put the cursor back
    // where the tooltipped row used to be. `menu_row_under`'s band guard means
    // nothing there is hoverable, so no box. Video is the page with room to scroll
    // at this canvas.
    let (mut vnav, vui) = settings_nav_on(SettingsPage::Video);
    let mut vfav = FaviconCache::new();
    let vframe = frame_for(&vui, &vnav, &statuses, &mut vfav).expect("a frame");
    let band_bottom = vframe
        .list
        .as_ref()
        .and_then(|s| s.model(H))
        .map(|l| l.bottom())
        .expect("premise: Video has a band");
    // A row whose centre is below the band at scroll 0 — it exists, because Video
    // overflows a 480 px canvas.
    let hidden = (0..vframe.rows.len())
        .find(|&i| {
            vframe.rows[i].is_scrolling_list_row()
                && vframe.rows[i].tooltip.is_some()
                && row_rect(&vframe.rows, i, W, H)
                    .is_some_and(|(_, ry, _, rh)| ry + rh * 0.5 > band_bottom)
        })
        .expect("premise: a tooltipped Video row sits below the band at scroll 0");
    let (hx, hy, hw, hh) = row_rect(&vframe.rows, hidden, W, H).expect("rect");
    vnav.set_menu_cursor(hx + hw * 0.5, hy + hh * 0.5, W, H);
    let f = frame_for(&vui, &vnav, &statuses, &mut vfav).expect("a frame");
    let outside = colour_bounds(&geometry(&f, W, H), W, H, TOOLTIP_BG);
    assert_eq!(
        outside, None,
        "a tooltip appeared at {outside:?} for row {hidden}, which sits below the \
         band at {band_bottom} and cannot be clicked"
    );
    // The premise for (4): the very same row, brought *into* the band, does show one.
    // Without it, the `None` above could mean the row simply has no tooltip text.
    let notches = ((hy + hh * 0.5) - band_bottom + hh) / (options::DEFAULT_ITEM_HEIGHT / 2.0).floor();
    vnav.scroll_active_list(&vui, -notches.ceil(), H);
    let scrolled = frame_for(&vui, &vnav, &statuses, &mut vfav).expect("a frame");
    let (nx, ny, nw, nh) = row_rect(&scrolled.rows, hidden, W, H).expect("rect");
    assert!(
        ny + nh * 0.5 < band_bottom,
        "premise: scrolling did not bring row {hidden} into the band (centre \
         {} vs band bottom {band_bottom})",
        ny + nh * 0.5
    );
    vnav.set_menu_cursor(nx + nw * 0.5, ny + nh * 0.5, W, H);
    let f = frame_for(&vui, &vnav, &statuses, &mut vfav).expect("a frame");
    let inside = colour_bounds(&geometry(&f, W, H), W, H, TOOLTIP_BG);
    assert!(
        inside.is_some(),
        "row {hidden} shows no tooltip even inside the band, so the None above \
         measured the absence of text rather than the band guard"
    );

    // -- the census, both directions -----------------------------------------
    //
    // The expected value comes from outside this crate: `Options.java` has 34
    // `cachedConstantTooltip` sites and `OnlineOptionsScreen` two `withTooltip` ones,
    // and of those exactly **33** resolve to an accessor this tree carries a row for
    // (`japaneseGlyphVariants` has no row here; `telemetryOptInExtra` belongs to the
    // Telemetry screen). Asserted in both directions, because either alone is
    // satisfiable by a broken table: a count of *reached* accessors catches a dropped
    // row, and "no table key is unreached" catches a typo'd key — which would
    // otherwise be a tooltip that silently never shows.
    let all: Vec<crate::menu::options::Cell> = [
        SettingsPage::Root,
        SettingsPage::Video,
        SettingsPage::Controls,
        SettingsPage::Mouse,
        SettingsPage::Sound,
        SettingsPage::Chat,
        SettingsPage::Accessibility,
        SettingsPage::Skin,
        SettingsPage::Online,
    ]
    .into_iter()
    .flat_map(|p| options::all_controls(p, false))
    .collect();
    let mut reached: Vec<&'static str> = all
        .iter()
        .filter_map(|c| match c {
            crate::menu::options::Cell::Option(spec) if c.tooltip().is_some() => {
                Some(spec.accessor)
            }
            _ => None,
        })
        .collect();
    reached.sort_unstable();
    reached.dedup();
    assert_eq!(
        reached.len(),
        33,
        "the tooltip table reaches {} distinct accessors, not the 33 the jar resolves \
         onto rows this tree has: {reached:?}",
        reached.len()
    );
    let accessors: Vec<&'static str> = all
        .iter()
        .filter_map(|c| match c {
            crate::menu::options::Cell::Option(spec) => Some(spec.accessor),
            _ => None,
        })
        .collect();
    let stranded: Vec<&'static str> = options::tooltip_accessors()
        .into_iter()
        .filter(|key| !accessors.contains(key))
        .collect();
    assert!(
        stranded.is_empty(),
        "these tooltip-table keys match no row on any page, so their text can never \
         show and the count above cannot see it: {stranded:?}"
    );
}

/// **The layout half of the same report, and it is a sweep rather than one probe.**
///
/// A page whose rows already fit cannot tell "laid out clear of the footer" from
/// "happens not to overflow", which is exactly how an overlap ships. So this runs
/// every `OptionsList` page at a canvas where the longest of them overflows by
/// hundreds of pixels, at **every** scroll offset one pixel apart, and requires the
/// strip between the band's footer separator and the footer's own top button to be
/// empty in all of them.
///
/// Three things make it a real measurement rather than a restatement:
///
/// - the offset is driven through `MenuNav::scroll_active_list` — the production
///   wheel path — a twelfth of a notch at a time, so the swept offsets are ones a
///   player can actually reach, not values written into a field;
/// - mismatches are **collected**, not asserted inside the loop, so a regression
///   reports every page and offset it touches instead of aborting on the first;
/// - the premise that a page overflows is asserted per page, and the control is the
///   same frame with its `ListSpec` removed, which must fail.
///
/// The strip's bounds are derived: the band from the `ListSpec::model` the clip is
/// built from, plus `SEPARATOR_H` for the footer bar that legitimately owns the
/// first two rows, up to `options::footer_rects`' own y.
#[test]
fn every_settings_page_keeps_its_footer_band_clear_at_every_scroll_offset() {
    use crate::menu::options::{self, SettingsPage};

    // The shortest canvas any `gui_scale` can produce, so every page that can
    // overflow does.
    const W: f32 = 320.0;
    const H: f32 = 240.0;
    // One pixel of offset: `scroll_rate` is `floor(DEFAULT_ITEM_HEIGHT / 2)` = 12,
    // and the wheel takes notches.
    let px_per_notch = (options::DEFAULT_ITEM_HEIGHT / 2.0).floor();

    let statuses = crate::menu::status::StatusCache::with_probe(
        crate::menu::status::unavailable_probe(),
    );
    let mut overflowed = Vec::new();
    let mut mismatches: Vec<String> = Vec::new();
    for page in [
        SettingsPage::Video,
        SettingsPage::Controls,
        SettingsPage::Mouse,
        SettingsPage::Sound,
        SettingsPage::Chat,
        SettingsPage::Accessibility,
        SettingsPage::Skin,
        SettingsPage::Online,
    ] {
        let (mut nav, ui) = settings_nav_on(page);
        let mut fav = FaviconCache::new();
        let footer_top = options::footer_rects(W, H, page.footer().len() as u8)
            .first()
            .expect("every page has a footer button")
            .1;
        let max = options::list_spec(page, 0.0)
            .model(H)
            .map_or(0.0, |m| m.max_scroll());
        if max > 0.0 {
            overflowed.push(page);
        }
        let mut offset = 0.0f32;
        loop {
            let frame = frame_for(&ui, &nav, &statuses, &mut fav).expect("settings owns a frame");
            let list = frame
                .list
                .as_ref()
                .and_then(|s| s.model(H))
                .expect("premise: every OptionsList page has a band at 240");
            let band_bottom = list.top() + list.height();
            let strip = (
                0.0,
                band_bottom + SEPARATOR_H,
                W,
                footer_top - band_bottom - SEPARATOR_H,
            );
            assert!(strip.3 > 0.0, "premise: {page:?} has a strip to measure");
            let ink = coverage(&geometry(&frame, W, H), W, H, strip);
            if ink != 0.0 {
                mismatches.push(format!(
                    "{page:?} at scroll {offset}: {ink} of {strip:?} painted"
                ));
            }
            if offset >= max {
                break;
            }
            offset += 1.0;
            nav.scroll_active_list(&ui, -1.0 / px_per_notch, H);
        }
    }
    assert!(
        mismatches.is_empty(),
        "settings rows painted into the reserved footer band:\n{}",
        mismatches.join("\n")
    );
    // The premise, stated as a count so it cannot pass vacuously: this sweep is
    // worthless unless some page genuinely has more rows than its band holds.
    assert!(
        overflowed.len() >= 4,
        "only {} of the OptionsList pages overflow a 240 px canvas ({overflowed:?}), so \
         this sweep mostly measured pages that could not overlap anyway",
        overflowed.len()
    );

    // -- control, executed ---------------------------------------------------
    // Sound with no `ListSpec` takes `draw`'s unclipped branch, and its rows run
    // 50 px past the band at 240, so the same strip must be inked.
    let (nav, ui) = settings_nav_on(SettingsPage::Sound);
    let mut fav = FaviconCache::new();
    let mut bare = frame_for(&ui, &nav, &statuses, &mut fav).expect("settings owns a frame");
    let band_bottom = bare
        .list
        .as_ref()
        .and_then(|s| s.model(H))
        .map(|l| l.top() + l.height())
        .expect("premise: Sound has a band");
    let footer_top = options::footer_rects(W, H, 1)[0].1;
    let strip = (
        0.0,
        band_bottom + SEPARATOR_H,
        W,
        footer_top - band_bottom - SEPARATOR_H,
    );
    bare.list = None;
    let spilled = coverage(&geometry(&bare, W, H), W, H, strip);
    assert!(
        spilled > 0.0,
        "the unclipped Sound frame painted nothing in {strip:?} either, so the sweep \
         above is not measuring the clip"
    );
}

/// **The save-list scroll gate**, in three parts, each with its control:
///
/// 1. a row straddling the band's bottom edge is **cut** rather than painted into
///    the gap above the footer — the control is the *same frame with no
///    `ListSpec`*, which takes `draw`'s unclipped branch and must fail the same
///    assertion;
/// 2. the row that was at the top of the band before scrolling is **not drawn
///    there** afterwards, measured by the selection outline's own colour — the
///    control is the pre-scroll arm, where it is;
/// 3. and the band is not simply blank afterwards: the row that *is* there draws.
///
/// **What else already paints here** was asked before believing (1): the row
/// column overlaps the search box above the band and all six footer buttons below
/// it, so an "is anything painted outside the band" rect would have measured the
/// footer. The rect used is the 8 px gutter **between** the band's bottom and the
/// footer grid's own top, and both edges are derived — the band from the
/// `ListSpec::model` the clip itself is built from, the footer from
/// `world_select_slot(Play)`. The title screen is measured in the same rect as a
/// second premise check.
///
/// Part (2) is colour-discriminated for the same reason
/// `every_world_in_the_list_draws_inside_its_own_row_band` had to be: the
/// selection outline is the one thing that separates "row 0 is here" from "some
/// row is here", and the leftmost *ink* inside a content rect is always
/// `getTextX()` whichever row it belongs to.
#[test]
fn a_scrolled_world_list_is_cut_at_the_band_and_stops_drawing_the_rows_above_it() {
    let owned: Vec<String> = (0..25).map(|i| format!("world{i:02}")).collect();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (mut nav, ui) = world_select_nav_with_worlds("ws-scroll-pixels", &names);

    // The band, from the same expression the clip is built from.
    let frame = world_select_frame(&nav, &ui);
    let spec = frame
        .list
        .as_ref()
        .expect("premise: the world list declares a ListSpec, or there is no clip and no scrollbar");
    let model = spec
        .model(V_H)
        .expect("premise: 25 rows in this band scroll");
    let band_top = model.top();
    let band_bottom = band_top + model.height();
    let footer_top = world_select_slot(crate::menu::world_select::WorldSelectButton::Play)
        .resolve(V_W, V_H)
        .1;
    assert!(
        footer_top > band_bottom,
        "premise: there must be a gap between the band's bottom ({band_bottom}) and \
         the footer's top ({footer_top}), or the only rect a cut row could paint in \
         is one the footer paints in too"
    );
    let (row_x, _, row_w, row_h) = world_list_row_rect(0, V_W, 0.0);
    let gutter = (row_x, band_bottom, row_w, footer_top - band_bottom);

    // Premise: the title screen paints nothing in the gutter, so "empty" there is
    // a fact about this screen.
    let title_nav = test_nav("ws-scroll-pixels-title");
    let title_ui = UiState::new();
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let title = frame_for(&title_ui, &title_nav, &statuses, &mut fav).expect("title frame");
    let painted = band_coverage(&geometry(&title, V_W, V_H), V_W, V_H, gutter);
    assert_eq!(
        painted.count, 0,
        "the title screen already paints in the gutter {gutter:?}, so this gate's \
         emptiness assertion measures nothing: {:?}",
        painted.bounds
    );

    // Scroll to a position where a row really does straddle the band's edge.
    let notches = 20.0;
    nav.scroll_active_list(&ui, -notches, V_H);
    let scroll = nav.world_select().scroll();
    assert_eq!(
        scroll,
        notches * model.scroll_rate(),
        "premise: the wheel moved by whole notches of `scrollRate`"
    );
    let straddler = (0..names.len())
        .find(|r| {
            let top = world_list_row_top(*r, scroll);
            top < band_bottom && top + row_h > band_bottom
        })
        .expect("premise: some row straddles the band's bottom edge at this offset");
    assert!(
        world_list_row_visible(straddler, V_H, scroll),
        "premise: the straddling row {straddler} is still a drawn row"
    );

    let frame = world_select_frame(&nav, &ui);
    let clipped = band_coverage(&geometry(&frame, V_W, V_H), V_W, V_H, gutter);
    assert_eq!(
        clipped.count, 0,
        "world row {straddler} straddles the band's bottom edge at scroll {scroll} \
         and painted into the gutter above the footer: {:?}",
        clipped.bounds
    );

    // -- control for (1), executed -------------------------------------------
    // The *same* frame with no `ListSpec` takes `draw`'s unclipped branch. It must
    // fail the assertion above, or the clip is not what made it pass.
    let mut unclipped = world_select_frame(&nav, &ui);
    unclipped.list = None;
    let spilled = band_coverage(&geometry(&unclipped, V_W, V_H), V_W, V_H, gutter);
    assert!(
        spilled.count > 0,
        "the unclipped frame painted nothing in the gutter either, so the clip is \
         not what the assertion above measured — the straddling row may not be \
         drawing at all"
    );

    // -- (2) and (3): the row above the band is no longer drawn there ---------
    // The band's first row position, and the selection outline's own colour there.
    let outline_at_top = |nav: &MenuNav, ui: &UiState| {
        let f = world_select_frame(nav, ui);
        let colour = geometry(&f, V_W, V_H);
        let (rx, ry, _, rh) = world_list_row_rect(0, V_W, nav.world_select().scroll());
        (
            coverage_of(&colour, V_W, V_H, (rx, ry + 2.0, 1.0, rh - 4.0), [1.0, 1.0, 1.0, 1.0]),
            band_coverage(&colour, V_W, V_H, world_list_row_content_rect(0, V_W, 0.0)),
        )
    };
    // Control: before scrolling, row 0 is the selection and it is at the top of
    // the band, so the outline is there.
    let (mut fresh_nav, fresh_ui) = world_select_nav_with_worlds("ws-scroll-pixels-top", &names);
    assert_eq!(fresh_nav.world_select().scroll(), 0.0);
    assert_eq!(fresh_nav.world_select().selected_row(), Some(0));
    let (outline_before, ink_before) = outline_at_top(&fresh_nav, &fresh_ui);
    assert!(
        outline_before > 0.5,
        "premise: with the list at the top, row 0's selection outline is at the \
         band's first row position: coverage {outline_before}"
    );
    assert!(ink_before.count > 0, "premise: and the row draws text there");

    // Scroll by exactly ten rows, so row 10 lands where row 0 was.
    let ten_rows = 10.0 * crate::menu::render::WORLD_LIST_ITEM_H;
    let notches = ten_rows / model.scroll_rate();
    fresh_nav.scroll_active_list(&fresh_ui, -notches, V_H);
    assert_eq!(
        fresh_nav.world_select().scroll(),
        ten_rows,
        "premise: the list moved by exactly ten rows"
    );
    assert!(
        !world_list_row_visible(0, V_H, ten_rows),
        "premise: row 0 is now out of view"
    );
    let f = world_select_frame(&fresh_nav, &fresh_ui);
    let colour = geometry(&f, V_W, V_H);
    let first_slot = world_list_row_content_rect(0, V_W, 0.0);
    let (rx, ry, _, rh) = world_list_row_rect(0, V_W, 0.0);
    let outline_after =
        coverage_of(&colour, V_W, V_H, (rx, ry + 2.0, 1.0, rh - 4.0), [1.0, 1.0, 1.0, 1.0]);
    assert!(
        outline_after < 0.1,
        "row 0 is scrolled out of view but its selection outline is still drawn at \
         the band's first row position: coverage {outline_after}"
    );
    // (3) And the band is not blank: the row that *is* there draws.
    let ink_after = band_coverage(&colour, V_W, V_H, first_slot);
    assert!(
        ink_after.count > 0,
        "the band's first row position is empty after scrolling, so the outline \
         assertion above is satisfied by nothing drawing at all"
    );
}

/// Hover and focus are two facts on this screen, and both reach the draw.
///
/// The bug this rules out is concrete: with one flag, moving the mouse over
/// the footer would pull the keyboard out of the search field. So the
/// assertion is that hovering a button changes what *that button* draws while
/// leaving the focused row alone.
#[test]
fn hovering_a_world_select_button_lights_it_without_moving_focus() {
    use crate::menu::world_select::{SEARCH_FIELD, WorldSelectButton as B};
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    let (mut nav, mut ui) = world_select_nav("ws-hover");
    nav.hover(&ui, B::Back.row());
    let frame = world_select_frame(&nav, &ui);
    assert_eq!(frame.hovered, Some(B::Back.row()));
    assert_eq!(
        frame.selected, SEARCH_FIELD,
        "hovering must not move keyboard focus"
    );

    // Vanilla's sprite argument is `isHoveredOrFocused()`, so a hovered
    // *enabled* button draws `widget/button_highlighted`.
    let row = frame.rows[B::Back.row()].clone();
    let draw = |hovered: Option<usize>| {
        let mut f = frame_with(vec![row.clone()], 99);
        f.vanilla = true;
        f.hovered = hovered;
        build(&f, Some(&atlas), None, V_W, V_H).sprite
    };
    let (hi_min, hi_max) = sprite_uv_bounds(&atlas, widget::BUTTON_SPRITES.enabled_focused);
    assert!(
        all_uvs_within(&draw(Some(0)), hi_min, hi_max),
        "a hovered enabled button must sample widget/button_highlighted"
    );
    // The control: unhovered and unfocused, it must not.
    assert!(
        !all_uvs_within(&draw(None), hi_min, hi_max),
        "the detector cannot tell the highlighted sprite apart"
    );

    // A **disabled** hovered button still draws the disabled sprite —
    // `WidgetSprites`' three-argument collapse, the single rule a hand-rolled
    // highlight gets wrong. Edit, not Create (issue #190 made Create live).
    let edit = frame.rows[B::Edit.row()].clone();
    let mut f = frame_with(vec![edit], 99);
    f.vanilla = true;
    f.hovered = Some(0);
    let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;
    let (off_min, off_max) = sprite_uv_bounds(&atlas, widget::BUTTON_SPRITES.disabled);
    assert!(
        all_uvs_within(&sprite, off_min, off_max),
        "a hovered DISABLED Edit must still sample widget/button_disabled"
    );

    // And the click that hover would have preceded does nothing on it, which
    // is the other half of "present but disabled".
    let before = ui.screen();
    assert_eq!(
        nav.click(&mut ui, B::Edit.row()),
        crate::menu::nav::MenuAction::None
    );
    assert_eq!(ui.screen(), before, "clicking Edit must not open anything");
}

/// The search box draws as a **text field**, not as a button — a slotted row
/// carrying an `EditBox` takes `draw_edit_box`'s path and not
/// `draw_widget`'s.
///
/// The discriminator is the synthetic pack itself: `button_pack()` carries
/// `widget/button*` and no `widget/text_field*`, so a field falls back to its
/// flat fill and emits **no sprite quads at all** where a button emits nine.
/// The control is the same row drawn as a button, watched emitting them.
#[test]
fn the_search_box_draws_as_a_field_inside_its_own_slot() {
    let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
    let (mut nav, mut ui) = world_select_nav("ws-search");
    // Upper-case, and `M` first, on purpose: the jar-less font's `M` is
    // `0b10001` in all seven rows (`hud/font.rs:97`), so its leftmost lit
    // column sits exactly on the box's `text_x`. That is what lets the x
    // assertion below be an equality rather than a bound — a glyph whose
    // column 0 is blank (`A`, `C`) would put the leftmost vertex a pixel or
    // two right of `text_x` and make the same test unable to tell a 2 px
    // error from a correct draw.
    for ch in "MC".chars() {
        nav.key(&mut ui, MenuKey::Char(ch));
    }
    let frame = world_select_frame(&nav, &ui);
    let row = frame.rows[0].clone();
    assert_eq!(
        row.edit.as_ref().map(|e| e.value().to_string()),
        Some("MC".to_string()),
        "typing on this screen goes into the search box"
    );

    let (fx, fy, fw, fh) = world_select_search_slot().resolve(V_W, V_H);
    let mut f = frame_with(vec![row.clone()], 0);
    f.vanilla = true;
    let drawn = build(&f, Some(&atlas), None, V_W, V_H);
    assert!(
        drawn.sprite.is_empty(),
        "the field sampled a button sprite, so it took draw_widget's path"
    );
    // Its background is the field fill, at the slot's own rect.
    assert!(
        coverage_of(&drawn.colour, V_W, V_H, (fx, fy, fw, fh), FIELD_BG) > 0.5,
        "the search box's fill did not reach {:?}",
        (fx, fy, fw, fh)
    );

    // -- control ---------------------------------------------------------
    // The same row without its `EditBox` is a button, and it must emit the
    // sprite quads the assertion above requires to be absent.
    let mut as_button = row.clone();
    as_button.edit = None;
    as_button.field = false;
    let mut g = frame_with(vec![as_button], 0);
    g.vanilla = true;
    let button_drawn = build(&g, Some(&atlas), None, V_W, V_H);
    assert!(
        !button_drawn.sprite.is_empty(),
        "a button drew no sprites either, so the discriminator measures nothing"
    );

    // The typed text lands inside the box's own text band — every bound asked
    // of a clone repositioned into the slot, exactly as `draw_edit_box` does,
    // rather than restated.
    let mut probe = row.edit.clone().expect("a live box");
    probe.widget.x = fx;
    probe.widget.y = fy;
    probe.widget.width = fw;
    probe.widget.height = fh;
    let state = probe.draw_state(None);
    // The band spans the box's **whole** width, deliberately: the question is
    // where the text starts, so a band that begins at `text_x` would clip the
    // very error it is looking for and pass on a draw 4 px to the left.
    //
    // That makes the *focus outline* the thing to be careful about, and it is
    // what this gate got wrong on its first run. `band_coverage` counts
    // **vertices**, not covered area, and the jar-less outline's bottom bar
    // spans the full field width at `y + h - 2` — inside a `glyph_h`-tall
    // band vertically, with its only vertices at the box's own `x` and
    // `x + width`. So on a focused box the leftmost vertex in this band is the
    // box's edge, not the text's, and the gate accused the draw of painting
    // 4 px left of `text_x` when the draw was right and the 4 px was
    // `BORDER_INSET` in the gate's own reasoning. (#395's `EditBox` gate dodges
    // this by insetting its band to `text_x`/`inner_width`; that is the right
    // answer for measuring *what* drew and the wrong one for measuring
    // *where* it started.)
    //
    // So: measure the text on an **unfocused** clone — no outline, no caret,
    // nothing in the box but glyphs — and use the focused draw as the control
    // that this band really can see ink at the box's edge.
    // The band's bottom is the box's own bottom edge, not a glyph-height
    // constant: the control below has to see the fallback outline's bottom
    // bar, which draws at `y + h - 2` — a fixed offset from the box's real
    // bottom, with nothing to do with any text scale. Tying the band to
    // `EDIT_TEXT_SCALE` is what broke this the moment that scale stopped
    // matching the outline's position (`2cd7c58`): the band shrank from
    // 14px to 7px, stopped reaching the bar, and the control then measured
    // text ink starting at `text_x` instead of outline ink at `fx`.
    let band = (fx, state.text_y, fw, (fy + fh) - state.text_y);
    let mut unfocused = row.clone();
    if let Some(e) = unfocused.edit.as_mut() {
        e.widget.focused = false;
    }
    let mut u = frame_with(vec![unfocused], 99);
    u.vanilla = true;
    let quiet = build(&u, Some(&atlas), None, V_W, V_H).colour;
    let inside = band_coverage(&quiet, V_W, V_H, band);
    assert!(
        inside.count > 0,
        "the typed text reached no pixels inside the box's own band {band:?}"
    );
    let bounds = inside.bounds.expect("a non-empty band has bounds");
    assert!(
        (bounds.0 - state.before_x).abs() < 0.01,
        "the text starts at {} where the box's own text_x is {} — a draw using \
         the row's PAD of 6, or the box's own x, fails here; bounds {bounds:?}",
        bounds.0,
        state.before_x
    );
    assert!(
        bounds.2 <= fx + fw + 0.01,
        "the text overran the box's right edge: bounds {bounds:?}"
    );

    // -- control ---------------------------------------------------------
    // The focused draw puts the outline's bottom bar in the same band, with a
    // corner vertex on the box's own `x`. So the band demonstrably *can* see
    // ink `BORDER_INSET` left of `text_x` — which is exactly the error the
    // assertion above denies, and without this the equality could be passing
    // because the band is blind to that column.
    let lit = band_coverage(&drawn.colour, V_W, V_H, band)
        .bounds
        .expect("a focused field paints its outline");
    assert!(
        (lit.0 - fx).abs() < 0.01,
        "the control did not reach the box's edge, so the assertion above is \
         not measuring what it claims: bounds {lit:?}"
    );
    assert!(
        state.before_x - fx > 0.0,
        "premise: text_x is inset from the box's x, or the two measurements \
         above cannot disagree"
    );
}

/// A real single-colour PNG, encoded here so the favicon test's input is a
/// genuine PNG stream (IHDR/IDAT/IEND with zlib and CRCs) rather than
/// something only our own decoder would accept.
fn solid_png(side: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, side, side);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("write header");
        let data: Vec<u8> = (0..side * side).flat_map(|_| rgba).collect();
        writer.write_image_data(&data).expect("write image");
    }
    out
}

/// The loading screen's progress bar (issue #449) reaches geometry, at vanilla's
/// `LevelLoadingScreen` rect, and its **green fill tracks the real fraction**.
///
/// The fill width is the assertion, not the bar's presence: a bar that drew at a
/// fixed width would pass a presence check while telling the player nothing. The
/// expected width is derived from `PROGRESS_BAR_W` and the fraction the frame was
/// built from, the same two terms the draw multiplies.
#[test]
fn the_loading_bar_fill_tracks_the_real_column_count() {
    use crate::menu::loading::TerrainProgress;

    let quarter = TerrainProgress { loaded: 100, expected: 400 };
    let f = loading_frame_with_progress("Loading terrain...", quarter);
    let v = geometry(&f, V_W, V_H);

    let track = colour_bounds(&v, V_W, V_H, PROGRESS_BAR_BG)
        .expect("the black track must reach the colour stream");
    let fill = colour_bounds(&v, V_W, V_H, PROGRESS_BAR_FG)
        .expect("the green fill must reach the colour stream");

    // The track is vanilla's 200x2, horizontally centred.
    assert!(
        (track.2 - PROGRESS_BAR_W).abs() < 0.5,
        "track width {} should be {PROGRESS_BAR_W}",
        track.2
    );
    assert!(
        (track.3 - PROGRESS_BAR_H).abs() < 0.5,
        "track height {} should be {PROGRESS_BAR_H}",
        track.3
    );
    assert!(
        (track.0 + track.2 * 0.5 - V_W * 0.5).abs() < 1.0,
        "track should be centred, got x0={} w={}",
        track.0,
        track.2
    );

    // A quarter-loaded view fills a quarter of the bar, from the same left edge.
    let want = (quarter.fraction() * PROGRESS_BAR_W).round();
    assert!(
        (fill.2 - want).abs() < 0.5,
        "fill width {} should be {want} for {}/{}",
        fill.2,
        quarter.loaded,
        quarter.expected
    );
    assert!((fill.0 - track.0).abs() < 0.5, "fill must start at the track's left edge");

    // The negative control that matters here: an empty view must draw the track
    // and *no* fill at all, so a full-width fill cannot masquerade as progress.
    let empty = TerrainProgress { loaded: 0, expected: 400 };
    let v0 = geometry(&loading_frame_with_progress("Loading terrain...", empty), V_W, V_H);
    assert!(
        colour_bounds(&v0, V_W, V_H, PROGRESS_BAR_BG).is_some(),
        "the track still draws at zero"
    );
    assert!(
        colour_bounds(&v0, V_W, V_H, PROGRESS_BAR_FG).is_none(),
        "a zero-column view must draw no green fill"
    );
}

/// A pack fixture for the two gates below: `name`, its description, and either a
/// flat single-pixel `pack.png` or none at all.
///
/// One flat colour rather than a picture, because `head_mosaic`'s box filter
/// averages each cell — so a uniform source makes every mosaic cell exactly
/// [`PACK_FIXTURE_ICON`], and `coverage_of` can then ask whether the *icon* landed
/// rather than whether anything did.
fn pack_fixture(name: &str, description: &str, icon: bool) -> crate::resources::DiscoveredPack {
    crate::resources::DiscoveredPack {
        id: format!("file/{name}"),
        title: name.to_string(),
        description: description.to_string(),
        pack_format: 64,
        icon: icon.then(|| lodestone_assets::Image {
            width: 1,
            height: 1,
            rgba: vec![0x20, 0xC0, 0x40, 0xFF],
        }),
        path: std::path::PathBuf::from("/nonexistent").join(name),
        kind: crate::resources::PackKind::Directory,
    }
}

/// [`pack_fixture`]'s flat `pack.png` (`0x20C040`) as a mosaic cell.
const PACK_FIXTURE_ICON: [f32; 4] = [32.0 / 255.0, 192.0 / 255.0, 64.0 / 255.0, 1.0];

/// A resource-pack row draws vanilla's selection-list entry — thumbnail, name and
/// description — and **no button fill**, with or without a `pack.png`.
///
/// This is the assertion the reported bug walked straight past. `packs::frame` has
/// always carried the icon, the label and the description, and every test in
/// `menu::packs` asserts on that frame *data* — so the suite stayed green while the
/// row was dispatched to `draw_widget` and came out as a button with one centred
/// label. The discriminator here is therefore the button fill: `ROW_BG`/`ROW_SEL`
/// is exactly what the wrong draw painted over the whole row, and a pack row must
/// paint none of it.
///
/// The built-in row is checked as well as an iconed one because it is the row a
/// player with an empty `resourcepacks/` folder sees — it has no `pack.png` at all,
/// which is how the old draw's `MenuRow::favicon` gate missed it completely.
#[test]
fn a_resource_pack_row_draws_its_icon_and_description_not_a_centred_button_label() {
    use super::draw::{PACK_DESC_DY, PACK_ENTRY_DIM, PACK_ICON, PACK_ROW_PAD, PACK_TEXT_DX};
    use crate::menu::packs::{self, MOVE_BTN, PackList, PacksNav, PacksPlacement, ROW_H, ROW_W};

    // `alpha` ships the icon and is **selected**, so it is Selected row 0 with the
    // built-in row under it; `bravo.zip` stays Available and takes the cursor
    // (control 0), which keeps the iconed row clear of the selection's own hover
    // overlay — that overlay deliberately dims the icon, and dimming it is not
    // what this gate is about.
    let nav = PacksNav::rebuild(
        vec![
            pack_fixture("alpha", "green everywhere", true),
            pack_fixture("bravo.zip", "a zip pack", false),
        ],
        &["file/alpha".to_string()],
    );
    let v = geometry(&packs::frame(&nav), V_W, V_H);
    let anchor = |list, row| {
        packs::placement_anchor(PacksPlacement::Row { list, row, scroll: 0.0 }, V_W, V_H)
    };
    // The reorder buttons live inside a Selected row's right edge (that is their
    // own gate in `menu::packs`), and they *are* buttons — so a "no button fill"
    // assertion has to stop short of them.
    let content_w = ROW_W - MOVE_BTN - 2.0 * PACK_ROW_PAD;

    // -- the iconed row ------------------------------------------------------
    let (x, y) = anchor(PackList::Selected, 0);
    let (cx, cy) = (x + PACK_ROW_PAD, y + PACK_ROW_PAD);
    let icon = coverage_of(&v, V_W, V_H, (cx, cy, PACK_ICON, PACK_ICON), PACK_FIXTURE_ICON);
    assert!(
        icon > 0.9,
        "the pack.png mosaic should fill the 32x32 icon column, covered {icon}"
    );
    let desc = coverage_of(
        &v,
        V_W,
        V_H,
        (cx + PACK_TEXT_DX, cy + PACK_DESC_DY, 100.0, LINE_H),
        PACK_ENTRY_DIM,
    );
    assert!(
        desc > 0.0,
        "the description draws in vanilla's own -8355712 grey, on the second line \
         past the icon column"
    );
    for (fill, what) in [(ROW_BG, "ROW_BG"), (ROW_SEL, "ROW_SEL"), (ROW_OFF, "ROW_OFF")] {
        let painted = coverage_of(&v, V_W, V_H, (x, y, content_w, ROW_H), fill);
        assert_eq!(
            painted, 0.0,
            "a pack row must draw no {what} button fill; it is a list entry"
        );
    }

    // -- the built-in row, which has no `pack.png` at all --------------------
    let (bx, by) = anchor(PackList::Selected, 1);
    let (bcx, bcy) = (bx + PACK_ROW_PAD, by + PACK_ROW_PAD);
    let line = (bcx + PACK_TEXT_DX, bcy + PACK_DESC_DY, 100.0, LINE_H);
    assert!(
        coverage_of(&v, V_W, V_H, line, PACK_ENTRY_DIM) > 0.0,
        "the built-in row draws its description too — it has no icon at all, which \
         is precisely the case the old `favicon`-gated draw missed"
    );
    // And it draws it *past* the icon column rather than across the row, which the
    // empty icon column here can actually witness: nothing else paints there.
    assert_eq!(
        coverage_of(
            &v,
            V_W,
            V_H,
            (bcx, bcy + PACK_DESC_DY, PACK_ICON, LINE_H),
            PACK_ENTRY_DIM
        ),
        0.0,
        "the icon column is reserved even when there is no icon to put in it"
    );
    for (fill, what) in [(ROW_BG, "ROW_BG"), (ROW_SEL, "ROW_SEL"), (ROW_OFF, "ROW_OFF")] {
        let painted = coverage_of(&v, V_W, V_H, (bx, by, ROW_W, ROW_H), fill);
        assert_eq!(painted, 0.0, "the built-in pack row must draw no {what} either");
    }
}

/// The two reorder buttons carry a **triangle**, and it points the way the button
/// says it does.
///
/// Measured by where the ink is, not by whether any is: an up arrow's apex row is
/// its narrowest, so its top half must carry less ink than its bottom half, and a
/// down arrow the reverse. A letter, a square, or a pair of arrows drawn the same
/// way round all fail that.
#[test]
fn a_move_button_draws_a_triangle_pointing_its_own_way() {
    use crate::menu::packs::{self, MOVE_BTN, PacksNav, PacksPlacement};

    // Three packs, all selected, so **row 1**'s buttons are both live (it has a
    // neighbour above and a non-built-in one below) and neither is the cursor —
    // which is control 0, Selected row 0. Row 1's arrows are therefore the only
    // `ACTIVE_LABEL` ink inside their own rects.
    let nav = PacksNav::rebuild(
        vec![
            pack_fixture("alpha", "a folder pack", false),
            pack_fixture("bravo.zip", "a zip pack", false),
            pack_fixture("charlie", "a third pack", false),
        ],
        &[
            "file/alpha".to_string(),
            "file/bravo.zip".to_string(),
            "file/charlie".to_string(),
        ],
    );
    let v = geometry(&packs::frame(&nav), V_W, V_H);

    let halves = |up: bool| {
        let (bx, by) = packs::placement_anchor(
            PacksPlacement::MoveButton { row: 1, up, scroll: 0.0 },
            V_W,
            V_H,
        );
        let half = MOVE_BTN * 0.5;
        (
            coverage_of(&v, V_W, V_H, (bx, by, MOVE_BTN, half), widget::ACTIVE_LABEL),
            coverage_of(
                &v,
                V_W,
                V_H,
                (bx, by + half, MOVE_BTN, half),
                widget::ACTIVE_LABEL,
            ),
        )
    };

    let (up_top, up_bottom) = halves(true);
    assert!(up_top > 0.0 && up_bottom > 0.0, "the up button draws something");
    assert!(
        up_top < up_bottom,
        "an up arrow's apex is its narrowest row: top {up_top} should be under bottom {up_bottom}"
    );
    let (down_top, down_bottom) = halves(false);
    assert!(
        down_top > down_bottom,
        "a down arrow is the mirror image: top {down_top} should be over bottom {down_bottom}"
    );
}

// -- composition order: a selection fill must not bury a fallback icon --------
//
// The reported symptom: selecting a server whose icon is the default one turned
// the 32 px thumbnail solid black, and so did focusing a resource pack, while a
// server with a real favicon and a pack with a real `pack.png` were both fine.
// One cause, in `MenuGeometry`: the renderer drew *every* sprite between the
// backdrop and the rest of the colour stream, so a row's opaque black selection
// fill landed on top of the two fallback icons — which are sprites — no matter
// when it was emitted, while a real icon is a `FaviconMosaic` of flat quads on the
// colour stream and stayed in emission order.

/// One quad from either stream, reduced to what a paint-order question needs.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Paint {
    /// A flat colour quad's RGBA.
    Colour([f32; 4]),
    /// A sprite quad's mean UV, which is what says *which* atlas region it
    /// sampled and therefore which sprite it is.
    Sprite([f32; 2]),
}

/// Whether a quad's destination bounding box contains `at`, in logical pixels.
///
/// **Containment of a point, not vertices-inside-a-rect.** `band_coverage` in this
/// file counts *vertices* inside a rect, so a quad that **encloses** the rect
/// contributes none and reads as zero coverage — a canvas-wide fill measured
/// straight through that probe and it still reported 0. A row's selection fill
/// encloses the 32 px icon exactly, so it is precisely the species that probe
/// cannot see, and sampling one point inside the icon is a detector that can.
fn quad_covers(quad: &[f32], stride: usize, w: f32, h: f32, at: (f32, f32)) -> bool {
    let (mut x0, mut y0) = (f32::MAX, f32::MAX);
    let (mut x1, mut y1) = (f32::MIN, f32::MIN);
    for v in quad.chunks_exact(stride) {
        let px = (v[0] + 1.0) * 0.5 * w;
        let py = (1.0 - v[1]) * 0.5 * h;
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px);
        y1 = y1.max(py);
    }
    at.0 >= x0 - 0.01 && at.0 <= x1 + 0.01 && at.1 >= y0 - 0.01 && at.1 <= y1 + 0.01
}

/// Colour quads covering `at` within `from..to` **floats** of the colour stream.
///
/// Both streams are always a whole number of six-vertex quads long, and a
/// `SpriteCut`'s `colour_floats` is a snapshot of that length, so the slice is
/// quad-aligned; the assertion says so rather than trusting it.
fn colour_paints(
    geo: &MenuGeometry,
    w: f32,
    h: f32,
    at: (f32, f32),
    from: usize,
    to: usize,
) -> Vec<Paint> {
    let to = to.min(geo.colour.len());
    if to <= from {
        return Vec::new();
    }
    assert_eq!(from % (STRIDE * 6), 0, "colour cut {from} is mid-quad");
    geo.colour[from..to]
        .chunks_exact(STRIDE * 6)
        .filter(|q| quad_covers(q, STRIDE, w, h, at))
        .map(|q| Paint::Colour([q[2], q[3], q[4], q[5]]))
        .collect()
}

/// [`colour_paints`] for the sprite stream, keyed on the quad's mean UV.
fn sprite_paints(
    geo: &MenuGeometry,
    w: f32,
    h: f32,
    at: (f32, f32),
    from: usize,
    to: usize,
) -> Vec<Paint> {
    let to = to.min(geo.sprite.len());
    if to <= from {
        return Vec::new();
    }
    assert_eq!(
        from % (SPRITE_FLOATS_PER_VERTEX * 6),
        0,
        "sprite cut {from} is mid-quad"
    );
    geo.sprite[from..to]
        .chunks_exact(SPRITE_FLOATS_PER_VERTEX * 6)
        .filter(|q| quad_covers(q, SPRITE_FLOATS_PER_VERTEX, w, h, at))
        .map(|q| {
            let (mut u, mut v) = (0.0f32, 0.0f32);
            for vert in q.chunks_exact(SPRITE_FLOATS_PER_VERTEX) {
                u += vert[2];
                v += vert[3];
            }
            Paint::Sprite([u / 6.0, v / 6.0])
        })
        .collect()
}

/// Everything painted at `at`, in the order `MenuRenderer::draw` replays it —
/// colour up to each [`SpriteCut`], that cut's sprites, then the colour tail.
///
/// Order, not a composited byte: `CLAUDE.md` records that an exact blended byte
/// through `ALPHA_BLENDING` on this backend is not predictable, so the question
/// asked here is which quad went down after which.
fn paint_at(geo: &MenuGeometry, w: f32, h: f32, at: (f32, f32)) -> Vec<Paint> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    for cut in &geo.sprite_cuts {
        out.extend(colour_paints(geo, w, h, at, cursor, cut.colour_floats));
        cursor = cut.colour_floats.max(cursor);
        out.extend(sprite_paints(
            geo,
            w,
            h,
            at,
            cut.sprite_start,
            cut.sprite_end,
        ));
    }
    out.extend(colour_paints(geo, w, h, at, cursor, geo.colour.len()));
    out
}

/// [`paint_at`] under the ordering the renderer used **before** the interleave:
/// the backdrop, then every sprite, then the whole colour tail.
///
/// This is the negative control, and it is a control rather than a description
/// because the gate below runs it and requires it to *fail* the same assertion —
/// `CLAUDE.md`: an assertion of an absence needs a detector observed firing.
fn paint_at_legacy(geo: &MenuGeometry, w: f32, h: f32, at: (f32, f32)) -> Vec<Paint> {
    let mut out = colour_paints(geo, w, h, at, 0, geo.backdrop_floats);
    out.extend(sprite_paints(geo, w, h, at, 0, geo.sprite.len()));
    out.extend(colour_paints(
        geo,
        w,
        h,
        at,
        geo.backdrop_floats,
        geo.colour.len(),
    ));
    out
}

/// Whether a [`Paint`] is a sprite quad sampling inside an atlas region.
fn is_sprite(paint: Paint, min: [f32; 2], max: [f32; 2]) -> bool {
    match paint {
        Paint::Sprite(uv) => {
            uv[0] >= min[0] && uv[0] <= max[0] && uv[1] >= min[1] && uv[1] <= max[1]
        }
        Paint::Colour(_) => false,
    }
}

/// [`sprite_uv_bounds`] for a **loose** texture. The two fallback icons live at
/// `textures/misc/`, outside the `gui/sprites/**` glob, so `build_with_extras`
/// stitches them under a synthetic `lodestone:gui/loose/<id>` location — a
/// different key space, which is why this cannot share the sprite helper.
fn loose_uv_bounds(atlas: &GuiAtlas, id: &str) -> ([f32; 2], [f32; 2]) {
    let loc: lodestone_assets::ResourceLocation = format!("lodestone:gui/loose/{id}")
        .parse()
        .expect("location");
    let s = atlas.atlas().sprite(&loc).expect("loose texture placed");
    let (aw, ah) = (atlas.atlas().width as f32, atlas.atlas().height as f32);
    (
        [s.x as f32 / aw, s.y as f32 / ah],
        [(s.x + s.width) as f32 / aw, (s.y + s.height) as f32 / ah],
    )
}

/// A synthetic atlas for the Resource Packs screen: the four
/// `transferable_list/*` overlays plus the **loose** `misc/unknown_pack`
/// fallback, which only arrives because it is named as an extra.
fn pack_screen_atlas() -> GuiAtlas {
    use lodestone_assets::{MemorySource, ResourceSource};
    let mut src = MemorySource::default();
    for id in [
        super::draw::PACK_SELECT_SPRITES.0,
        super::draw::PACK_SELECT_SPRITES.1,
        super::draw::PACK_UNSELECT_SPRITES.0,
        super::draw::PACK_UNSELECT_SPRITES.1,
    ] {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_rgba_png(32, 32, [10, 200, 10, 255]),
        );
    }
    src.insert(
        crate::resources::UNKNOWN_PACK_TEXTURE.1,
        solid_rgba_png(32, 32, [70, 70, 70, 255]),
    );
    let manager =
        lodestone_assets::ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>]);
    GuiAtlas::build_with_extras(&manager, &[crate::resources::UNKNOWN_PACK_TEXTURE])
        .expect("synthetic pack atlas builds")
}

/// Where in a paint order the row fill and the fallback icon landed, at one
/// point.
fn fill_then_icon(
    order: &[Paint],
    fill: [f32; 4],
    icon: ([f32; 2], [f32; 2]),
) -> (Option<usize>, Option<usize>) {
    (
        order.iter().position(|p| *p == Paint::Colour(fill)),
        order.iter().position(|p| is_sprite(*p, icon.0, icon.1)),
    )
}

/// A selected row's **fallback** icon is painted after the row's opaque black
/// selection fill, on both screens that have one.
///
/// ## The discriminating cell is (default icon × selected), and nothing had it
///
/// A real favicon passes under both hypotheses — it is a mosaic on the colour
/// stream, so emission order always protected it — and an *unselected* default
/// icon passes too, because no fill is drawn for it. Only the pair sees the bug.
/// No gate in this file had that pair: every default-icon assertion here runs
/// through `geometry`, which builds with **no atlas** and therefore emits no
/// sprite at all, and every icon-with-an-atlas gate asks which region was
/// sampled rather than in what order. The corpus was structurally blind to it.
///
/// Three cells and two controls per screen, collected rather than asserted in the
/// loop so a neuter cannot pass by aborting on the first one.
#[test]
fn a_selected_rows_fallback_icon_paints_over_its_selection_fill() {
    use super::draw::{PACK_ICON, PACK_ROW_PAD, PACK_SELECTION_FILL};
    use super::server_list::{SERVER_LIST_SELECTION_FILL, SERVER_UNKNOWN_ICON};
    use super::world_list::WORLD_LIST_SELECTION_FILL;
    use crate::menu::packs::{self, PackList, PacksNav, PacksPlacement};

    let mut bad: Vec<String> = Vec::new();

    // -- the multiplayer list ------------------------------------------------
    let atlas = server_list_atlas();
    let (nav, ui) = list_nav("paint-order", &[("A", "a.example"), ("B", "b.example")]);
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    let row_of = |f: &MenuFrame<'_>, index: usize| {
        f.rows
            .iter()
            .position(|r| r.entry.as_ref().is_some_and(|e| e.index == index))
            .expect("the frame carries the server row")
    };
    // **Which row is selected is read, not assumed.** `list_nav` adds each server
    // through the real Add Server path, which leaves the row it just created
    // selected — so a two-entry list arrives on row 1, and a gate that assumed
    // row 0 fails its own premise instead of measuring anything.
    let sel = nav.server_index();
    assert!(sel < 2, "premise: the selection is one of the two rows, not {sel}");
    let unsel = 1 - sel;
    let (rs, ru) = (row_of(&f, sel), row_of(&f, unsel));
    assert!(
        f.rows[rs].entry.as_ref().unwrap().selected,
        "premise: server row {sel} is the selection"
    );
    assert!(
        !f.rows[ru].entry.as_ref().unwrap().selected,
        "premise: server row {unsel} is not"
    );
    assert!(
        f.rows[rs].favicon.is_none() && f.rows[ru].favicon.is_none(),
        "premise: an unreachable server sends no favicon, so both rows draw the fallback sprite"
    );
    assert!(f.cursor.is_none(), "premise: no hover — this is about selection");

    let centre_of = |rect: (f32, f32, f32, f32)| (rect.0 + rect.2 * 0.5, rect.1 + rect.3 * 0.5);
    let icon_rect = |index: usize| server_entry_icon_rect(index, V_W, 0.0);
    let geo = build(&f, Some(&atlas), None, V_W, V_H);
    let fallback = loose_uv_bounds(&atlas, SERVER_UNKNOWN_ICON);

    // The cell that matters.
    let at = centre_of(icon_rect(sel));
    let order = paint_at(&geo, V_W, V_H, at);
    match fill_then_icon(&order, SERVER_LIST_SELECTION_FILL, fallback) {
        (Some(fi), Some(si)) if si > fi => {}
        (fi, si) => bad.push(format!(
            "server row {sel} (default icon, selected): fill at {fi:?}, fallback icon at {si:?} — \
             the icon must be painted after the fill. icon rect {:?}, order {order:?}",
            icon_rect(sel)
        )),
    }

    // Control 1 — the same icon in an **unselected** row: no fill at all, which
    // is what makes the cell above about the fill rather than about the sprite.
    let at1 = centre_of(icon_rect(unsel));
    let order1 = paint_at(&geo, V_W, V_H, at1);
    let (fill1, icon1) = fill_then_icon(&order1, SERVER_LIST_SELECTION_FILL, fallback);
    if fill1.is_some() || icon1.is_none() {
        bad.push(format!(
            "server row {unsel} (default icon, unselected): expected no fill and an icon, got fill \
             {fill1:?} icon {icon1:?}. icon rect {:?}, order {order1:?}",
            icon_rect(unsel)
        ));
    }

    // Control 2 — the ordering this fix replaced must **bury** the icon at the
    // same cell. Run and observed failing, not described.
    let legacy = paint_at_legacy(&geo, V_W, V_H, at);
    match fill_then_icon(&legacy, SERVER_LIST_SELECTION_FILL, fallback) {
        (Some(fi), Some(si)) if si < fi => {}
        (fi, si) => bad.push(format!(
            "the control did not fire: under the pre-fix ordering the fallback icon ({si:?}) must \
             land *before* the fill ({fi:?}) and be buried. icon rect {:?}, order {legacy:?}",
            icon_rect(sel)
        )),
    }

    // Control 3 — a **real** icon survives even the pre-fix ordering, which is
    // the asymmetry the report described: a mosaic is flat quads on the colour
    // stream, emitted after the fill, so nothing ever buried it.
    let mut mosaic = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
    const PROBE: [f32; 4] = [0.25, 0.5, 0.75, 1.0];
    mosaic.rows[rs].favicon = Some(FaviconMosaic {
        size: 1,
        cells: vec![PROBE],
    });
    let mgeo = build(&mosaic, Some(&atlas), None, V_W, V_H);
    for (what, order) in [
        ("now", paint_at(&mgeo, V_W, V_H, at)),
        ("pre-fix", paint_at_legacy(&mgeo, V_W, V_H, at)),
    ] {
        let fill = order
            .iter()
            .position(|p| *p == Paint::Colour(SERVER_LIST_SELECTION_FILL));
        let cell = order.iter().position(|p| *p == Paint::Colour(PROBE));
        if !matches!((fill, cell), (Some(fi), Some(ci)) if ci > fi) {
            bad.push(format!(
                "a real favicon must outlive the fill under the {what} ordering too: fill \
                 {fill:?}, mosaic cell {cell:?}. icon rect {:?}, order {order:?}",
                icon_rect(sel)
            ));
        }
    }

    // -- the Resource Packs screen -------------------------------------------
    let patlas = pack_screen_atlas();
    let pnav = PacksNav::rebuild(vec![pack_fixture("bravo.zip", "a zip pack", false)], &[]);
    let pf = packs::frame(&pnav);
    assert_eq!(
        pf.selected, 0,
        "premise: the cursor is on Available row 0, so that row is the selection"
    );
    assert!(
        pf.rows[0].pack.is_some() && pf.rows[0].favicon.is_none(),
        "premise: a pack with no pack.png draws the fallback sprite"
    );
    let (px, py) = packs::placement_anchor(
        PacksPlacement::Row {
            list: PackList::Available,
            row: 0,
            scroll: 0.0,
        },
        V_W,
        V_H,
    );
    let pack_icon_rect = (px + PACK_ROW_PAD, py + PACK_ROW_PAD, PACK_ICON, PACK_ICON);
    let pat = centre_of(pack_icon_rect);
    let pgeo = build(&pf, Some(&patlas), None, V_W, V_H);
    let pack_fallback = loose_uv_bounds(&patlas, super::draw::PACK_UNKNOWN_ICON);

    let porder = paint_at(&pgeo, V_W, V_H, pat);
    match fill_then_icon(&porder, PACK_SELECTION_FILL, pack_fallback) {
        (Some(fi), Some(si)) if si > fi => {}
        (fi, si) => bad.push(format!(
            "pack row 0 (default icon, focused): fill at {fi:?}, fallback icon at {si:?} — the \
             icon must be painted after the fill. icon rect {pack_icon_rect:?}, order {porder:?}"
        )),
    }
    let plegacy = paint_at_legacy(&pgeo, V_W, V_H, pat);
    match fill_then_icon(&plegacy, PACK_SELECTION_FILL, pack_fallback) {
        (Some(fi), Some(si)) if si < fi => {}
        (fi, si) => bad.push(format!(
            "the pack control did not fire: under the pre-fix ordering the fallback icon ({si:?}) \
             must land before the fill ({fi:?}). icon rect {pack_icon_rect:?}, order {plegacy:?}"
        )),
    }

    // -- the singleplayer world list -----------------------------------------
    //
    // This screen drew *nothing* in its 32 px icon column until the fallback
    // landed, so the cell is "the thumbnail exists at all" as well as "it survives
    // selection". Both halves matter and neither implies the other.
    let (wnav, wui) = world_select_nav_with_worlds("paint-order-worlds", &["alpha", "bravo"]);
    let wf = world_select_frame(&wnav, &wui);
    let wrow = |index: usize| {
        wf.rows
            .iter()
            .position(|r| r.world.as_ref().is_some_and(|v| v.index == index))
            .expect("the frame carries the world row")
    };
    assert!(
        wf.rows[wrow(0)].world.as_ref().unwrap().selected
            && !wf.rows[wrow(1)].world.as_ref().unwrap().selected,
        "premise: world row 0 is the selection (most recently played) and row 1 is not"
    );
    assert!(
        wf.rows[wrow(0)].favicon.is_none(),
        "premise: nothing writes a per-world image, so the row draws the fallback"
    );
    let wgeo = build(&wf, Some(&atlas), None, V_W, V_H);
    for (index, selected) in [(0usize, true), (1usize, false)] {
        let rect = world_list_icon_rect(index, V_W, 0.0);
        let order = paint_at(&wgeo, V_W, V_H, centre_of(rect));
        let (fill, icon) = fill_then_icon(&order, WORLD_LIST_SELECTION_FILL, fallback);
        let ok = match (selected, fill, icon) {
            (true, Some(fi), Some(si)) => si > fi,
            (false, None, Some(_)) => true,
            _ => false,
        };
        if !ok {
            bad.push(format!(
                "world row {index} (selected {selected}): fill {fill:?}, thumbnail {icon:?} — a \
                 selected row needs the icon after the fill, an unselected one needs no fill and \
                 still a thumbnail. icon rect {rect:?}, order {order:?}"
            ));
        }
    }

    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The hover scrim goes **under** the icon's overlay sprites, which is the same
/// cause one step further on: `SERVER_ICON_DARKEN` is a translucent grey rect and
/// vanilla fills it before blitting the join / move arrows over it
/// (`ServerSelectionList.OnlineServerEntry.extractContent`), so the pre-fix
/// ordering washed all three arrows out.
#[test]
fn the_hover_scrim_is_painted_under_the_icon_overlay_sprites() {
    let atlas = server_list_atlas();
    let (nav, ui) = list_nav("scrim-order", &[("A", "a.example"), ("B", "b.example")]);
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let mut f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

    // The icon's right half, so `over_right_half` picks the highlighted join
    // sprite — the discriminator is position, which the quadrant gate above owns;
    // here it only has to be *a* known region.
    let (ix, iy, iw, ih) = server_entry_icon_rect(0, V_W, 0.0);
    f.cursor = Some((ix + iw * 0.75, iy + ih * 0.5));
    let at = (ix + iw * 0.75, iy + ih * 0.5);
    let geo = build(&f, Some(&atlas), None, V_W, V_H);
    let join = sprite_uv_bounds(&atlas, SERVER_JOIN_SPRITES.1);

    let order = paint_at(&geo, V_W, V_H, at);
    let scrim = order
        .iter()
        .position(|p| *p == Paint::Colour(SERVER_ICON_DARKEN));
    let arrow = order.iter().position(|p| is_sprite(*p, join.0, join.1));
    assert!(
        matches!((scrim, arrow), (Some(s), Some(a)) if a > s),
        "the join arrow ({arrow:?}) must be painted after the scrim ({scrim:?}). icon rect \
         {:?}, order {order:?}",
        (ix, iy, iw, ih)
    );

    // The control: the pre-fix ordering put the scrim on top of all three.
    let legacy = paint_at_legacy(&geo, V_W, V_H, at);
    let l_scrim = legacy
        .iter()
        .position(|p| *p == Paint::Colour(SERVER_ICON_DARKEN));
    let l_arrow = legacy.iter().position(|p| is_sprite(*p, join.0, join.1));
    assert!(
        matches!((l_scrim, l_arrow), (Some(s), Some(a)) if a < s),
        "the control did not fire: under the pre-fix ordering the arrow ({l_arrow:?}) must land \
         before the scrim ({l_scrim:?}). icon rect {:?}, order {legacy:?}",
        (ix, iy, iw, ih)
    );
}

// ---------------------------------------------------------------------------
// The disconnect screen keeps the server's own colours
// ---------------------------------------------------------------------------

/// A kicked player's reason is a styled component, and both the frame and the
/// draw must keep its colours.
///
/// # The input is discriminating three ways
///
/// The root carries `gold`, one `extra` child overrides it with `red`, and a
/// second child specifies nothing and must therefore inherit `gold`. Three
/// different readings of inheritance give three different answers:
///
/// | reading | spans |
/// |---|---|
/// | correct: a child inherits what it does not override | `[Red, Gold]` |
/// | "the root's colour wins" | `[Gold, Gold]` |
/// | "no inheritance; an unstyled child is unstyled" | `[Red, None]` |
///
/// One colour cannot tell those apart, which is why there are two children with
/// different styling rather than one coloured child.
///
/// The root's own `text` is `""` with all the content in `extra` — the shape a
/// real server's kick message takes, and the shape most likely to expose a
/// root-only reader. Nothing on the disconnect path exercised a nested `extra`
/// at all before this: the corpus covered it on the *MOTD* path
/// (`tests/text_colour.rs`) and on the server *encode* path
/// (`v770`'s `server_disconnect`), and the shell's own disconnect fixtures were
/// every one of them a flat single component.
///
/// # Expected values
///
/// Transcribed from the jar's `TextColor` decimal table by hand, not read from
/// `TextColor::rgb` — calling that would make this `decode(encode(x))`, green
/// under any self-consistent misunderstanding. `named("gold", 16755200)` is
/// `0xffaa00`; `named("red", 16733525)` is `0xff5555`.
#[test]
fn a_kick_reason_keeps_the_server_s_colours_through_frame_and_draw() {
    use lodestone_model::{Text, TextColor};

    const GOLD: u32 = 0x00ff_aa00;
    const RED: u32 = 0x00ff_5555;

    let reason = Text::from_json(
        r#"{"text":"","color":"gold","extra":[
            {"text":"kick","color":"red"},
            {"text":" reason"}
        ]}"#,
    );
    let mut ui = UiState::new();
    ui.begin(crate::menu::SessionKind::Multiplayer);
    ui.session_failed(crate::sim::SessionEnd::disconnected(reason));
    assert_eq!(ui.screen(), Screen::Error);

    let frame = frame_for(
            &ui,
            &MenuNav::new(),
            &StatusCache::new(),
            &mut FaviconCache::new(),
        )
        .expect("the error screen owns a frame");
    let notice = frame.notice.as_ref().expect("the reason is a notice");

    assert_eq!(
        notice.text, "kick reason",
        "the wording must survive, and with no prefix of ours glued on"
    );
    assert!(
        !notice.text.contains("disconnected"),
        "the `disconnected: ` prefix was ours, not vanilla's — a DisconnectedScreen \
         puts its title in a separate widget above the reason: {}",
        notice.text
    );

    // Collected, not asserted in a loop: an `assert_eq!` per span aborts at the
    // first mismatch, so the "inherits" and "overrides" halves would never both
    // be reported.
    let got: Vec<Option<u32>> = notice
        .spans
        .iter()
        .map(|s| s.style.color.as_ref().map(TextColor::rgb))
        .collect();
    assert_eq!(
        got,
        vec![Some(RED), Some(GOLD)],
        "the overriding child must be red and the bare child must inherit the \
         root's gold; got {got:?} for spans {:?}",
        notice.spans
    );

    // And the draw actually uses them. `build` with no font takes
    // `text_spans`' fixed-advance fallback, which still emits one coloured quad
    // per glyph into the colour stream — so a red quad and a gold quad must both
    // be present. Before this change the notice drew through `b.text` with a
    // single `FG_BAD`, so neither colour could appear.
    let geometry = build(&frame, None, None, V_W, V_H);
    let has = |rgb: u32| {
        let want = [
            ((rgb >> 16) & 0xff) as f32 / 255.0,
            ((rgb >> 8) & 0xff) as f32 / 255.0,
            (rgb & 0xff) as f32 / 255.0,
        ];
        geometry.colour.chunks_exact(STRIDE).any(|v| {
            (v[2] - want[0]).abs() < 1.0 / 255.0
                && (v[3] - want[1]).abs() < 1.0 / 255.0
                && (v[4] - want[2]).abs() < 1.0 / 255.0
        })
    };
    let mut missing = Vec::new();
    if !has(RED) {
        missing.push("red");
    }
    if !has(GOLD) {
        missing.push("gold");
    }
    assert!(
        missing.is_empty(),
        "the draw dropped these colours: {missing:?}"
    );
}

/// The control for the test above: a reason with **no** colour must draw in
/// `FG_BAD` and produce no gold or red quad, so the two `has(...)` assertions
/// there are not satisfied by a draw that emits every colour, or by an atlas
/// sprite that happens to be gold.
///
/// It also pins the fallback that every notice this shell authors itself relies
/// on — an unstyled notice must not change appearance because the styled path
/// exists.
#[test]
fn an_uncoloured_kick_reason_still_draws_in_the_error_colour() {
    use lodestone_model::Text;

    let mut ui = UiState::new();
    ui.begin(crate::menu::SessionKind::Multiplayer);
    ui.session_failed(crate::sim::SessionEnd::disconnected(Text::literal(
        "kick reason",
    )));
    let frame = frame_for(
            &ui,
            &MenuNav::new(),
            &StatusCache::new(),
            &mut FaviconCache::new(),
        )
        .expect("the error screen owns a frame");
    let notice = frame.notice.as_ref().expect("the reason is a notice");
    assert!(
        notice.spans.iter().all(|s| s.style.color.is_none()),
        "a literal with no style must produce no colours: {:?}",
        notice.spans
    );

    let geometry = build(&frame, None, None, V_W, V_H);
    let gold_quads = geometry
        .colour
        .chunks_exact(STRIDE)
        .filter(|v| {
            (v[2] - 1.0).abs() < 1.0 / 255.0
                && (v[3] - 170.0 / 255.0).abs() < 1.0 / 255.0
                && v[4].abs() < 1.0 / 255.0
        })
        .count();
    assert_eq!(
        gold_quads, 0,
        "an uncoloured reason must emit no gold quads — otherwise the styled \
         test's assertions prove nothing"
    );
    // The error colour is what it draws instead, so the block is still visible.
    let bad_quads = geometry
        .colour
        .chunks_exact(STRIDE)
        .filter(|v| {
            (v[2] - FG_BAD[0]).abs() < 1.0 / 255.0
                && (v[3] - FG_BAD[1]).abs() < 1.0 / 255.0
                && (v[4] - FG_BAD[2]).abs() < 1.0 / 255.0
        })
        .count();
    assert!(
        bad_quads > 0,
        "an uncoloured reason must still draw, in the error colour"
    );
}

/// A client-side failure and a server disconnect get vanilla's two different
/// titles, from [`crate::sim::SessionEndKind`] rather than from the wording.
///
/// `DisconnectedScreen` takes its `title` as a constructor argument;
/// `ClientCommonPacketListenerImpl.onDisconnect` passes `disconnect.lost`
/// ("Connection Lost") and both `ClientHandshakePacketListenerImpl.onDisconnect`
/// and `ConnectScreen`'s `connectFailedTitle` pass `connect.failed` ("Failed to
/// connect to the server"). The English strings are vanilla's own `en_us.json`
/// entries for those keys, transcribed by hand.
#[test]
fn the_error_screen_titles_a_failure_differently_from_a_disconnect() {
    use lodestone_model::Text;

    let title_for = |end: crate::sim::SessionEnd| {
        let mut ui = UiState::new();
        ui.begin(crate::menu::SessionKind::Multiplayer);
        ui.session_failed(end);
        let frame = frame_for(
            &ui,
            &MenuNav::new(),
            &StatusCache::new(),
            &mut FaviconCache::new(),
        )
            .expect("the error screen owns a frame");
        frame.labels[0].text.clone()
    };

    let mut wrong = Vec::new();
    let disconnected = title_for(crate::sim::SessionEnd::disconnected(Text::literal("bye")));
    if disconnected != "Connection Lost" {
        wrong.push(format!("disconnected: {disconnected}"));
    }
    let failed = title_for(crate::sim::SessionEnd::failed(Text::literal(
        "connect: connection refused",
    )));
    if failed != "Failed to connect to the server" {
        wrong.push(format!("failed: {failed}"));
    }
    assert!(missing_titles_empty(&wrong), "wrong titles: {wrong:?}");
}

/// Reads as an assertion helper so the check above collects both arms instead of
/// aborting on the first.
fn missing_titles_empty(wrong: &[String]) -> bool {
    wrong.is_empty()
}

/// The loading screen declares the panorama backdrop, the three in-world screens
/// declare the dim one, and the two decisions the old `overlay: bool` fused are
/// now independent.
///
/// # What this can and cannot see
///
/// It can see the **declaration** and the **backdrop quad's colour**, which is
/// exactly the coupling that produced the bug: one flag chose the quad colour
/// *and* suppressed the panorama, so no screen could ask for a wash over the sky.
/// With the two separated, `Panorama` keeps the *opaque* quad — its
/// no-panorama fallback — while `Dim` keeps the translucent one.
///
/// It **cannot** see whether the panorama reaches a pixel. `build` is pure and
/// emits the backdrop quad unconditionally; the decision to skip those vertices
/// and draw the cubemap instead lives in `MenuRenderer::draw`, which needs a GPU.
/// So this is the declaration half only, and
/// `menu_panorama_pixels::the_loading_screen_draws_the_panorama_under_the_menu_background_wash`
/// is the half that measures pixels. Do not read a green run here as evidence the
/// sky draws.
#[test]
fn the_loading_screen_asks_for_the_panorama_and_the_in_world_screens_do_not() {
    let mut wrong: Vec<String> = Vec::new();

    let loading = loading_frame("Joining world...");
    if loading.backdrop != MenuBackdrop::Panorama {
        wrong.push(format!("loading_frame: {:?}", loading.backdrop));
    }
    let bar = loading_frame_with_progress(
        "Loading terrain",
        crate::menu::loading::TerrainProgress {
            loaded: 3,
            expected: 9,
        },
    );
    if bar.backdrop != MenuBackdrop::Panorama {
        wrong.push(format!("loading_frame_with_progress: {:?}", bar.backdrop));
    }

    let mut nav = test_nav("backdrop-decl");
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.pause();
    nav.hover(&ui, 0);
    if pause_frame(&nav).backdrop != MenuBackdrop::Dim {
        wrong.push(format!("pause_frame: {:?}", pause_frame(&nav).backdrop));
    }
    if death_frame(&nav, None).backdrop != MenuBackdrop::Dim {
        wrong.push(format!("death_frame: {:?}", death_frame(&nav, None).backdrop));
    }

    // The two questions the old boolean answered with one bit, now independent.
    if MenuBackdrop::Panorama.is_translucent() {
        wrong.push(
            "MenuBackdrop::Panorama must keep the OPAQUE quad: that quad is the              no-panorama fallback, and a translucent one over a Clear pass is the              flat-fill the loading screen used to draw"
                .to_owned(),
        );
    }
    if !MenuBackdrop::Panorama.wants_panorama() {
        wrong.push("Panorama must want the panorama".to_owned());
    }
    if MenuBackdrop::Dim.wants_panorama() {
        wrong.push(
            "Dim must NOT want the panorama — it exists to leave a live world              visible, which a cubemap covering every pixel would not"
                .to_owned(),
        );
    }
    if !MenuBackdrop::Dim.is_translucent() {
        wrong.push("Dim must be translucent".to_owned());
    }

    // And the quad's colour follows the enum, read straight out of the first
    // vertex rather than through a vertex-sampling coverage helper — a probe that
    // counts vertices inside a rect is blind to a quad that *encloses* it, which
    // is precisely the shape of a full-screen backdrop.
    let colour_of = |frame: &MenuFrame<'_>| -> [f32; 4] {
        let geo = build(frame, None, None, 480.0, 320.0);
        [geo.colour[2], geo.colour[3], geo.colour[4], geo.colour[5]]
    };
    let loading_bg = colour_of(&loading);
    let pause_bg = colour_of(&pause_frame(&nav));
    if (loading_bg[3] - 1.0).abs() > 1e-6 {
        wrong.push(format!(
            "the loading screen's fallback quad must be opaque, alpha was {}",
            loading_bg[3]
        ));
    }
    if pause_bg[3] >= 1.0 {
        wrong.push(format!(
            "the pause screen's quad must be translucent, alpha was {}",
            pause_bg[3]
        ));
    }
    if loading_bg == pause_bg {
        wrong.push(
            "both screens emit the same backdrop colour, so the enum is not              reaching the draw at all"
                .to_owned(),
        );
    }

    assert!(wrong.is_empty(), "{wrong:#?}");
}
