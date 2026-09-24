//! Tests for command-block menu submission through the outbound action seam.

use super::*;

/// **Command-block submission, exercised through production code.**
///
/// The command-block screen's Done button computed a fully-tested payload
/// and **dropped it on the floor** — `activate_command_block_row`'s `Done`
/// arm bound it to `let _submit` because `MenuAction` had no variant to
/// carry it and `app.rs` had no arm to consume it. This drives the whole
/// chain rather than re-asserting either half: the real
/// [`crate::menu::nav::MenuNav::key`] on the real `Done` row produces the
/// action, the real [`WindowApp::apply_menu_action`] consumes it, and the
/// `ClientAction` is read off the socket seam a live session would write to.
///
/// **The expected value is predicted, not round-tripped.** Every field is
/// stated from the edits made below (a typed command, a cycled mode, two
/// toggles) rather than from `to_submit()`'s own output, so a payload that
/// dropped or transposed a field fails here — `decode(encode(x)) == x` would
/// not.
///
/// **Negative control, executed:** deleting the
/// `MenuAction::SetCommandBlock` arm from `apply_menu_action` (replacing it
/// with `{}`) makes this fail at `try_recv`, `Err(Empty)` — nothing reaches
/// the socket. That is the island this test closes, and it is invisible to
/// `cargo check`: an arm that matches and does nothing compiles perfectly.
///
/// Reachability is a **separate** and still-open matter: nothing opens this
/// screen from a real interaction (no command-block block-entity NBT decode,
/// no `interact.rs` trigger). This test opens it directly, exactly as
/// `MenuNav::open_command_block` is written to allow.

#[test]
fn the_command_block_done_button_sends_a_real_set_command_block_action() {
    use crate::menu::command_block::{CommandBlockOpen, CommandBlockRow, COMMAND_BLOCK_ROWS};
    use crate::menu::nav::MenuKey;
    use lodestone_model::{BlockPos, CommandBlockMode};

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);

    // `MenuNav::open_command_block` and `UiState::open_command_block` both
    // guard on `Screen::Playing` (a command block is opened from the world,
    // not from a menu), so reach that first — `enter_dev_world` is the
    // headless entry point's own route to it.
    app.ui.enter_dev_world();

    // Open the screen on a specific block with known stored contents, then
    // *edit* it — an unedited screen would let a `to_submit` that returned
    // `CommandBlockOpen`'s values verbatim pass.
    let pos = BlockPos::new(12, -7, 340);
    app.nav.open_command_block(
        &mut app.ui,
        CommandBlockOpen {
            pos,
            command: "say hi".into(),
            track_output: false,
            previous_output: None,
            mode: CommandBlockMode::Redstone,
            conditional: false,
            automatic: false,
        },
    );
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "precondition: the screen must actually be open, or every key below \
         lands somewhere else"
    );

    // Type into the command field, through the real key path.
    for ch in "!".chars() {
        let action = app.nav.key(&mut app.ui, MenuKey::Char(ch));
        app.apply_menu_action(action);
    }
    // Cycle the mode once (Redstone -> its successor) and flip two toggles,
    // each by activating that row the way a click or Enter does.
    for row in [
        CommandBlockRow::Mode,
        CommandBlockRow::TrackOutput,
        CommandBlockRow::Conditional,
    ] {
        let idx = COMMAND_BLOCK_ROWS
            .iter()
            .position(|r| *r == row)
            .expect("every CommandBlockRow is in COMMAND_BLOCK_ROWS");
        let action = app.nav.click(&mut app.ui, idx);
        app.apply_menu_action(action);
    }

    // Read the mode the cycle actually produced from the screen itself, so
    // this test does not hardcode `next_mode`'s table (which has its own
    // gate in `command_block.rs`) — but every *other* field is predicted.
    let expected_mode = app
        .nav
        .command_block()
        .expect("the screen is still open")
        .mode;
    assert_ne!(
        expected_mode,
        CommandBlockMode::Redstone,
        "precondition: cycling the mode must have changed it, or this field \
         is not under test"
    );

    // Nothing may have reached the socket yet — the control for the
    // assertion below, and it is not vacuous: the toggle rows above all
    // return `MenuAction::None`, so a `_ =>` arm that sent something for
    // every action would be caught here.
    assert!(
        actions.try_recv().is_err(),
        "no action may be sent before Done is pressed"
    );

    // Press Done.
    let done = COMMAND_BLOCK_ROWS
        .iter()
        .position(|r| *r == CommandBlockRow::Done)
        .expect("Done is a CommandBlockRow");
    let action = app.nav.click(&mut app.ui, done);
    assert!(
        matches!(action, crate::menu::nav::MenuAction::SetCommandBlock(_)),
        "the Done row must produce the action, not swallow it: {action:?}"
    );
    app.apply_menu_action(action);

    // And it reached the wire, with exactly the edited payload.
    let sent = actions
        .try_recv()
        .expect("Done must put a ClientAction on the outbound seam");
    assert_eq!(
        sent,
        lodestone_model::ClientAction::SetCommandBlock {
            pos,
            command: "say hi!".into(),
            mode: expected_mode,
            track_output: true,
            conditional: true,
            automatic: false,
        },
        "the action must carry the screen's edits, field for field"
    );

    // Vanilla closes after sending.
    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "Done sends and then closes"
    );
}
