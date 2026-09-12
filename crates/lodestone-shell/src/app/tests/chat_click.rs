//! Tests for chat click actions, insertion, confirmation, and book-page dispatch.

use super::*;

use lodestone_model::ClientAction;
use lodestone_model::text::{ClickAction, ClickEvent};

fn headless_app_with_loopback() -> (WindowApp, std::sync::mpsc::Receiver<ClientAction>) {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, actions) = NetClient::loopback();
    app.sim.attach_net(net);
    (app, actions)
}

/// `run_command` reaches the wire exactly as typing the same text and
/// pressing Enter would — `Sim::send_chat` → `compose_chat_action`'s own
/// leading-`/` rule, unmodified. The leading `/` is stripped, matching
/// `compose_chat_action`'s own `SendCommand` shape.
#[test]
fn run_command_reaches_the_wire_as_a_real_command() {
    let (mut app, actions) = headless_app_with_loopback();
    app.dispatch_click_action(&ClickEvent {
        action: ClickAction::RunCommand,
        value: "/help".to_string(),
    });
    assert_eq!(
        actions.try_recv(),
        Ok(ClientAction::SendCommand { command: "help".to_string() }),
        "a run_command click must send exactly what typing it would have"
    );
    assert!(actions.try_recv().is_err(), "exactly one action per click");
}

/// `suggest_command` fills the chat input for the player to review and
/// send themselves — it must **not** reach the wire on its own, the
/// discriminating difference from `run_command` above.
#[test]
fn suggest_command_fills_the_input_and_sends_nothing() {
    let (mut app, actions) = headless_app_with_loopback();
    app.dispatch_click_action(&ClickEvent {
        action: ClickAction::SuggestCommand,
        value: "/give @s diamond".to_string(),
    });
    assert_eq!(app.chat_input.as_str(), "/give @s diamond");
    assert!(
        actions.try_recv().is_err(),
        "suggest_command must never send on its own — that is what run_command is for"
    );
}

/// `copy_to_clipboard` reaches the test-safe recorder — proof the OS
/// clipboard shell-out this click would otherwise trigger is reachable
/// through the real dispatch path, without ever touching a real
/// clipboard during `cargo test`. See `menu::accounts::copy_to_clipboard`'s
/// own doc for the incident this interception exists to prevent.
#[test]
fn copy_to_clipboard_reaches_the_test_safe_recorder() {
    let (mut app, _actions) = headless_app_with_loopback();
    let _ = crate::menu::accounts::test_clipboard::taken();
    app.dispatch_click_action(&ClickEvent {
        action: ClickAction::CopyToClipboard,
        value: "copied-from-chat".to_string(),
    });
    assert_eq!(
        crate::menu::accounts::test_clipboard::taken(),
        vec!["copied-from-chat".to_string()]
    );
}

/// `open_url` must never call the OS browser handoff before its untrusted
/// link confirmation is explicitly accepted.
#[test]
fn open_url_never_opens_the_browser_before_confirmation() {
    let (mut app, _actions) = headless_app_with_loopback();
    let _ = crate::menu::accounts::test_browser_opens::taken();
    app.dispatch_click_action(&ClickEvent {
        action: ClickAction::OpenUrl,
        value: "https://example.invalid/probe".to_string(),
    });
    assert!(
        crate::menu::accounts::test_browser_opens::taken().is_empty(),
        "open_url must not open a browser without the player confirming"
    );
    assert!(app.nav.server_links().returns_to_chat());
}

/// `open_file` gets the identical treatment as `open_url` above — same
/// external-effect boundary, same "surface, do not act" answer.
#[test]
fn open_file_also_never_acts_automatically() {
    let (mut app, _actions) = headless_app_with_loopback();
    let _ = crate::menu::accounts::test_browser_opens::taken();
    app.dispatch_click_action(&ClickEvent {
        action: ClickAction::OpenFile,
        value: "/etc/passwd".to_string(),
    });
    assert!(crate::menu::accounts::test_browser_opens::taken().is_empty());
    let recent = app.sim.recent_chat_spans(1);
    assert_eq!(recent.len(), 1);
    assert!(crate::overlay::spans_text(&recent[0].0).contains("/etc/passwd"));
}

/// With **no book open**, `change_page` and an unrecognised action are
/// both inert — the negative control proving the match's fallback arm
/// does not accidentally fall through to one of the real effects above.
///
/// `change_page` is not inert in general (see
/// [`change_page_turns_the_open_books_page`] below); a book is its only
/// consumer, and a server may still put one on an ordinary chat line.
#[test]
fn change_page_with_no_book_open_and_unknown_actions_do_nothing_observable() {
    let (mut app, actions) = headless_app_with_loopback();
    let _ = crate::menu::accounts::test_browser_opens::taken();
    let _ = crate::menu::accounts::test_clipboard::taken();
    let before_chat = app.sim.recent_chat_spans(10).len();
    let before_input = app.chat_input.as_str().to_string();

    for action in [ClickAction::ChangePage, ClickAction::Other("mystery".to_string())] {
        app.dispatch_click_action(&ClickEvent { action, value: "3".to_string() });
    }

    assert!(actions.try_recv().is_err());
    assert!(crate::menu::accounts::test_browser_opens::taken().is_empty());
    assert!(crate::menu::accounts::test_clipboard::taken().is_empty());
    assert_eq!(app.sim.recent_chat_spans(10).len(), before_chat);
    assert_eq!(app.chat_input.as_str(), before_input);
}

/// A **shift**-click inserts the run's insertion text at the caret and
/// does not run its click; an unshifted click on the same run does the
/// reverse. The run carries both, which is what makes this
/// discriminating: a dispatch that ignored the modifier would satisfy
/// either assertion alone.
#[test]
fn shift_click_inserts_and_leaves_the_click_action_alone() {
    use lodestone_game::text::InteractiveSpan;
    use lodestone_model::text::TextStyle;

    let both = InteractiveSpan {
        text: "<Notch>".to_string(),
        style: TextStyle::default(),
        click: Some(ClickEvent {
            action: ClickAction::SuggestCommand,
            value: "/msg Notch ".to_string(),
        }),
        hover: None,
        insertion: Some("Notch".to_string()),
    };

    let (mut app, _actions) = headless_app_with_loopback();
    app.chat_input.set("hello ");
    assert!(app.dispatch_chat_interaction(both.clone(), true));
    assert_eq!(
        app.chat_input.as_str(),
        "hello Notch",
        "the insertion appends at the caret rather than replacing the line"
    );

    let (mut app, _actions) = headless_app_with_loopback();
    app.chat_input.set("hello ");
    assert!(app.dispatch_chat_interaction(both, false));
    assert_eq!(
        app.chat_input.as_str(),
        "/msg Notch ",
        "unshifted, the suggest_command replaces the line as it always did"
    );
}

/// A shift-click on a run with **no** insertion is inert — it must not
/// fall through to the click action. That fall-through is what would make
/// shift-clicking a player name whisper them by accident.
#[test]
fn shift_click_without_an_insertion_does_not_fall_through_to_the_click() {
    use lodestone_game::text::InteractiveSpan;
    use lodestone_model::text::TextStyle;

    let (mut app, actions) = headless_app_with_loopback();
    let click_only = InteractiveSpan {
        text: "[Teleport]".to_string(),
        style: TextStyle::default(),
        click: Some(ClickEvent {
            action: ClickAction::RunCommand,
            value: "/tp @s 0 64 0".to_string(),
        }),
        hover: None,
        insertion: None,
    };

    assert!(!app.dispatch_chat_interaction(click_only.clone(), true));
    assert!(
        actions.try_recv().is_err(),
        "a shift-click with no insertion must send nothing"
    );

    // The control: the same run, unshifted, does reach the wire — so the
    // silence above is the modifier's doing and not a broken fixture.
    assert!(app.dispatch_chat_interaction(click_only, false));
    assert!(actions.try_recv().is_ok(), "unshifted, the same run runs its command");
}

/// `change_page` turns the open reading screen's page — the production
/// dispatch, through the same `dispatch_click_action` a page-run click
/// and a chat click both go through.
///
/// The argument is 1-based (a page number, not an index), so `"3"` on a
/// three-page book is the last page and the indicator reads `3 of 3`.
/// Predicted from the payload rather than read back: the discriminating
/// wrong answers are page 4 (off-by-one the other way) and page 1 (an
/// argument that never arrived).
#[test]
fn change_page_turns_the_open_books_page() {
    use crate::menu::book_view::BookViewOpen;
    use lodestone_model::ResolvedText;

    let (mut app, _actions) = headless_app_with_loopback();
    app.ui.enter_dev_world();
    app.nav.open_book_view(
        &mut app.ui,
        BookViewOpen {
            title: "Contents".to_owned(),
            author: "Steve".to_owned(),
            generation: 0,
            pages: vec![
                ResolvedText::literal("one"),
                ResolvedText::literal("two"),
                ResolvedText::literal("three"),
            ],
        },
    );
    assert_eq!(
        app.nav.book_view().map(crate::menu::book_view::BookViewState::page_indicator),
        Some((1, 3)),
        "control: the reader opens on the first page"
    );

    app.dispatch_click_action(&ClickEvent {
        action: ClickAction::ChangePage,
        value: "3".to_string(),
    });

    assert_eq!(
        app.nav.book_view().map(crate::menu::book_view::BookViewState::page_indicator),
        Some((3, 3))
    );
    assert_eq!(
        app.nav
            .book_view()
            .map(crate::menu::book_view::BookViewState::visible_lines),
        Some(vec!["three".to_owned()]),
        "the page the indicator names must be the page the screen shows"
    );
}
