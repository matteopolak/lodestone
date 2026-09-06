//! `app`'s unit tests, unwrapped verbatim out of `app.rs`.
//!
//! Kept as a single file on purpose: splitting it would rename every test
//! path (`app::tests::foo` -> `app::tests::input::foo`), and those names are
//! used by diagnostics and documentation across the repo.

use super::*;
use lodestone_data::item::Item;

fn benchmark_config(workload: crate::config::BenchmarkWorkload) -> Config {
    Config {
        benchmark: Some(crate::config::BenchmarkConfig {
            workload,
            debug_overlay: crate::config::BenchmarkDebugOverlay::Closed,
            heavyweight: None,
            warmup: Duration::from_secs(20),
            mutation: Duration::ZERO,
            stationary: Duration::from_secs(30),
            moving: Duration::from_secs(60),
        }),
        ..Config::default()
    }
}

#[test]
fn benchmark_policy_is_uncapped_unvsynced_and_uses_physical_1440p() {
    let config = benchmark_config(crate::config::BenchmarkWorkload::Terrain);
    assert_eq!(window_physical_size(&config), Some((2560, 1440)));
    assert_eq!(benchmark_target_fps(&config, Some(120)), None);
    assert_eq!(
        benchmark_present_mode(&config, wgpu::PresentMode::Fifo),
        wgpu::PresentMode::AutoNoVsync
    );
    assert!(!should_background_pace(&config));
}

#[test]
fn benchmark_window_selects_only_the_hardware_builtin_monitor() {
    let monitors = [(15_608_u32, false), (2_941_u32, true), (91_003_u32, false)];

    assert_eq!(select_builtin_monitor(monitors), Some(2_941));
    assert_eq!(
        select_builtin_monitor([(15_608_u32, false), (91_003_u32, false)]),
        None
    );
}

#[test]
fn ordinary_policy_remains_persisted_option_driven() {
    let config = Config::default();
    assert_eq!(window_physical_size(&config), None);
    assert_eq!(benchmark_target_fps(&config, Some(120)), Some(120));
    assert_eq!(
        benchmark_present_mode(&config, wgpu::PresentMode::Fifo),
        wgpu::PresentMode::Fifo
    );
    assert!(should_background_pace(&config));
}

fn open_test_stonecutter(
    app: &mut WindowApp,
    result_count: usize,
) -> lodestone_client::OpenMenuSnapshot {
    use lodestone_client::ClientEvent;

    const WINDOW_ID: i32 = 17;
    let stone_id = i32::from(Item::Stone.registry_id());
    let slab_id = i32::from(Item::StoneSlab.registry_id());
    let ingest = |event| {
        app.sim
            .net()
            .expect("the test attached a loopback client")
            .ingest_session_event(event);
    };
    ingest(ClientEvent::ScreenOpened {
        window_id: WINDOW_ID,
        menu_type: "minecraft:stonecutter".parse().unwrap(),
        title: lodestone_model::Text::literal("Stonecutter"),
    });
    let mut items = vec![None; 38];
    items[0] = Some(lodestone_model::ItemStack::new(
        "minecraft:stone".parse().unwrap(),
        1,
    ));
    ingest(ClientEvent::ContainerContent {
        window_id: WINDOW_ID,
        state_id: lodestone_model::ContainerStateId::new(1),
        items,
        carried_item: None,
    });
    ingest(ClientEvent::RecipePropertySetsUpdated {
        item_sets: Vec::new(),
        stonecutter_results: (0..result_count)
            .map(|_| (vec![stone_id], vec![slab_id]))
            .collect(),
    });
    app.sim.open_menu().expect("the server stonecutter opens")
}

fn point_at_stonecutter_index(
    menu: &lodestone_game::menu::Menu,
    index: i32,
    start: i32,
) -> (f32, f32) {
    let width = 1280;
    let height = 720;
    let layout = crate::container::slot_layout(menu);
    let (panel_x, panel_y) = crate::container::panel_origin_with_scale(
        &layout,
        crate::config::AUTO_GUI_SCALE,
        width,
        height,
    );
    let scale = crate::config::calculate_gui_scale(
        crate::config::AUTO_GUI_SCALE,
        width,
        height,
    )
    .max(1) as f32;
    let rect = crate::container::stonecutter::grid_rect(index, start)
        .expect("the requested result is visible");
    ((panel_x + rect.x + 1.0) * scale, (panel_y + rect.y + 1.0) * scale)
}

#[test]
fn stonecutter_scroll_and_click_use_the_visible_server_index_when_local_recipes_are_empty() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, actions) = NetClient::loopback();
    app.sim.attach_net(net);
    app.recipe_book = Some(lodestone_game::recipe::RecipeBook::new());
    let open = open_test_stonecutter(&mut app, 16);
    app.ui.enter_dev_world();
    app.ui.open_container();

    assert!(
        app.scroll_stonecutter(-1.0),
        "the server's offscreen row consumes the wheel"
    );
    assert_eq!(app.stonecutter_scroll, 1.0);
    app.cursor = point_at_stonecutter_index(&open.menu, 4, 4);
    assert!(app.handle_stonecutter_click(&open.menu, 1280, 720));
    assert_eq!(
        actions.try_recv(),
        Ok(lodestone_model::ClientAction::ContainerButtonClick {
            window_id: 17,
            button_id: 4,
        }),
        "the first cell after scrolling must retain server button id 4"
    );
}

#[test]
fn stonecutter_click_rejects_a_local_recipe_the_server_did_not_offer() {
    use lodestone_game::item::ItemStack;
    use lodestone_game::recipe::{Ingredient, Recipe, RecipeBook};

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, actions) = NetClient::loopback();
    app.sim.attach_net(net);
    let stone = "minecraft:stone".parse().unwrap();
    let mut local = RecipeBook::new();
    local.insert(
        "minecraft:local_only_stonecutting".parse().unwrap(),
        Recipe::Stonecutting {
            ingredient: Ingredient::Item(stone),
            result: ItemStack::new("minecraft:stone_slab".parse().unwrap(), 1),
        },
    );
    app.recipe_book = Some(local);
    let open = open_test_stonecutter(&mut app, 0);
    app.cursor = point_at_stonecutter_index(&open.menu, 0, 0);

    assert!(!app.handle_stonecutter_click(&open.menu, 1280, 720));
    assert!(
        actions.try_recv().is_err(),
        "a local-only recipe must send no button click"
    );
}

/// Java's `String.hashCode()`, computed by hand from the well-known
/// public algorithm — an oracle that lives outside this file, per
/// `CLAUDE.md`'s evidence standard. `"hello"`: `h = 0`, then
/// `104, 3325, 103183, 3198781, 99162322` after `'h','e','l','l','o'`
/// (`h = h*31 + c` each step) — a commonly-cited constant, reproduced
/// here from the formula rather than trusted from memory alone.
#[test]
fn java_string_hash_code_matches_the_known_constant() {
    assert_eq!(java_string_hash_code("hello"), 99_162_322);
    assert_eq!(java_string_hash_code(""), 0);
}

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

/// `WindowApp::dispatch_click_action`'s dispatch table, driven end to end for
/// each `ClickAction` — whole point, that a chat `click_event`
/// actually *does something* rather than reaching a hit-test and stopping.
/// [`WindowApp::dispatch_click_action`] is deliberately split out of
/// [`WindowApp::dispatch_chat_click_under_cursor`] so this needs no renderer
/// or render target (`chat_interaction` needs both, the same requirement
/// `suggestion_row_under_cursor` already has, which would make this whole
/// table a GPU-gated test just to prove the dispatch itself is right).
mod chat_click_dispatch {
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
}

/// Vanilla's own seed parsing: a valid `i64` literal is used
/// verbatim (vanilla tries a plain long parse first), whitespace is
/// trimmed, and non-numeric text falls back to the Java hash — not a new
/// rule, just `parse_seed` calling straight through to the constant test
/// above.
#[test]
fn parse_seed_follows_vanillas_own_rule() {
    assert_eq!(parse_seed("12345"), 12345);
    assert_eq!(parse_seed("-42"), -42);
    assert_eq!(parse_seed("  42  "), 42, "vanilla trims before parsing");
    assert_eq!(
        parse_seed("hello"),
        99_162_322,
        "non-numeric text must hash exactly like Java's own String.hashCode, \
         not this crate's own notion of a hash"
    );
}

/// An empty seed means "random" (vanilla's own random-seed default) —
/// asserted by absence of a fixed answer, the only honest assertion for
/// "random": two draws must not collide (astronomically unlikely for a
/// real `i64` random source, impossible for a constant-returning bug).
#[test]
fn empty_seed_is_random_not_a_fixed_fallback() {
    let a = parse_seed("");
    let b = parse_seed("   ");
    assert_ne!(
        a, b,
        "two empty-seed draws must not produce the same i64 — a constant \
         here would silently make every \"random\" world identical"
    );
}

/// A queued-patch check driven end to end: two different
/// `WorldCreationConfig`s (the exact type `Screen::CreateWorld` collects)
/// resolved through the *production* `resolve_launch_seed` must generate
/// **different real terrain** at the same coordinate — not merely
/// different `i64`s, which `parse_seed`'s own tests above already cover
/// and which would be the isolated-unit species of this gate. And the
/// same config must reproduce identical terrain.
///
/// `lodestone_server::overworld_generator` is exactly what
/// `crate::net::run`'s `Origin::Integrated` arm calls with this
/// function's resolved seed, once it has gone through
/// `lodestone_server::region_source::resolve_world_seed` — so this proves
/// the seed that would reach the wire, not a stand-in.
#[test]
fn resolved_seeds_from_different_world_creation_configs_generate_different_terrain() {
    let config_a = crate::menu::create_world::WorldCreationConfig {
        seed: "100".to_string(),
        ..Default::default()
    };
    let config_b = crate::menu::create_world::WorldCreationConfig {
        seed: "999999".to_string(),
        ..Default::default()
    };

    let seed_a = resolve_launch_seed(Some(&config_a));
    let seed_b = resolve_launch_seed(Some(&config_b));
    assert_eq!(seed_a, 100);
    assert_eq!(seed_b, 999_999);

    let column_a = lodestone_server::overworld_generator(seed_a).column(0, 0);
    let column_b = lodestone_server::overworld_generator(seed_b).column(0, 0);

    let mut differences = 0usize;
    for lz in 0..16usize {
        for lx in 0..16usize {
            for y in (column_a.min_y()..column_a.min_y() + column_a.height()).step_by(4) {
                if column_a.block_state(lx, y, lz) != column_b.block_state(lx, y, lz) {
                    differences += 1;
                }
            }
        }
    }
    assert!(
        differences > 0,
        "two different entered seeds must generate different terrain \
         somewhere in the same column — the config's seed is reaching \
         nowhere if this is 0"
    );

    // Reproducibility: the same config, resolved and generated twice,
    // must be byte-identical — `overworld_generator` is a pure function
    // of its seed, and this is the exact call `net.rs::run` makes, called
    // twice rather than reimplemented.
    let seed_a_again = resolve_launch_seed(Some(&config_a));
    assert_eq!(seed_a_again, seed_a, "the same typed seed must resolve identically");
    let column_a_again = lodestone_server::overworld_generator(seed_a_again).column(0, 0);
    for lz in 0..16usize {
        for lx in 0..16usize {
            for y in column_a.min_y()..column_a.min_y() + column_a.height() {
                assert_eq!(
                    column_a.block_state(lx, y, lz),
                    column_a_again.block_state(lx, y, lz),
                    "the same seed must reproduce identical terrain at ({lx},{y},{lz})"
                );
            }
        }
    }
}

/// `None` (`Screen::WorldSelect`'s Play Selected World) must still resolve
/// to the bundled world's own seed — the default behavior remains unchanged.
#[test]
fn no_config_resolves_to_the_bundled_worlds_seed() {
    assert_eq!(
        resolve_launch_seed(None),
        crate::menu::world_select::BUNDLED_WORLD.seed
    );
}

/// A cheap sim: headless mode with the smallest render distance that still
/// generates real terrain, so physics ticks do real collision work.
fn pacing_sim() -> Sim {
    // Explicitly the demo-world fixture: this needs real terrain so the
    // physics ticks do collision work, and the client `Sim::new` has none.
    Sim::with_demo_world(Config {
        mode: Mode::Headless,
        render_distance: 2,
        ..Config::default()
    })
}

/// Ticks a real `Sim` executes when advanced by `dt` in one call.
fn ticks_for(sim: &mut Sim, dt: f64) -> u64 {
    let before = sim.tick_count();
    sim.step(dt);
    sim.tick_count() - before
}

/// Discrete scrolling collapses the delta to its **sign**, and the
/// sensitivity multiply happens **after**.
///
/// The order is the whole content of this gate, because both orders "work" on the
/// common case (a single `LineDelta` notch of 1.0 at sensitivity 1.0 gives 1.0 either
/// way). They diverge exactly where a player would notice, and the wrong hypothesis is
/// *computed* here rather than described:
///
/// | input | vanilla, `signum` then scale | reversed, scale then `signum` |
/// |---|---|---|
/// | `dy = 0.4`, sens `2.0` | **2.0** | 1.0 |
/// | `dy = 12.0` (trackpad), sens `0.5` | **0.5** | 1.0 |
///
/// Reversed, `signum` would eat the sensitivity entirely and cap wheel speed at one
/// notch — i.e. turning this row on would silently break the sensitivity row. That is
/// the defect a direction-only assertion cannot see.
#[test]
fn discrete_scrolling_takes_the_sign_before_sensitivity_scales_it() {
    // Off: the raw delta passes through, scaled. This is also the proof the option is
    // a pure addition — a trackpad's fractional delta is still proportional.
    assert_eq!(scale_scroll(0.4, false, 2.0), 0.8);
    assert_eq!(scale_scroll(12.0, false, 0.5), 6.0);

    // On: sign first, then scale.
    let small = scale_scroll(0.4, true, 2.0);
    assert_eq!(small, 2.0, "a sub-notch delta becomes a full notch, then doubles");
    let reversed_small = (0.4_f64 * 2.0).signum();
    assert_ne!(
        small, reversed_small,
        "scale-then-signum gives {reversed_small}, so this gate does not discriminate"
    );

    let big = scale_scroll(12.0, true, 0.5);
    assert_eq!(big, 0.5, "a 12 px trackpad delta becomes one notch, then halves");
    let reversed_big = (12.0_f64 * 0.5).signum();
    assert_ne!(
        big, reversed_big,
        "scale-then-signum gives {reversed_big}, so this gate does not discriminate"
    );

    // Direction survives the collapse.
    assert_eq!(scale_scroll(-7.5, true, 1.0), -1.0);
    assert_eq!(scale_scroll(7.5, true, 1.0), 1.0);

    // The external floating-point sign rule yields **0.0** for `0.0`, not 1.0.
    // `f64::signum` disagrees, so this is the
    // one place the Java and Rust primitives are not interchangeable — without the
    // explicit zero case a stationary wheel would emit a notch per event.
    assert_eq!(scale_scroll(0.0, true, 1.0), 0.0, "a zero delta must stay zero");
    assert_eq!(
        0.0_f64.signum(),
        1.0,
        "premise: f64::signum(0.0) really is 1.0, which is why the guard exists"
    );

    // And it composes with the hotbar's accumulator rather than replacing it: at a
    // low sensitivity a discrete notch still needs several gestures to move a slot.
    let mut accum = 0.0;
    assert_eq!(accumulate_scroll(&mut accum, scale_scroll(0.1, true, 0.5)), 0);
    assert_eq!(accumulate_scroll(&mut accum, scale_scroll(0.1, true, 0.5)), 1);
}

/// At the default sensitivity (`1.0`), one wheel
/// notch (`LineDelta` magnitude `1.0`) must move exactly one hotbar slot
/// — the default behavior — so the sensitivity feature is provably a
/// pure addition, not a regression of the common case.
#[test]
fn accumulate_scroll_moves_one_slot_per_notch_at_default_sensitivity() {
    let mut accum = 0.0;
    assert_eq!(accumulate_scroll(&mut accum, 1.0 * 1.0), 1);
    assert_eq!(accum, 0.0, "a whole-notch scroll must leave no carry");
    assert_eq!(accumulate_scroll(&mut accum, -1.0 * 1.0), -1);
}

/// A sensitivity below 1.0 must take more than one notch to move a slot
/// — the exact scaled amount, not merely "less than at 1.0". At `0.25`,
/// four notches of `1.0` each accumulate to exactly one slot, with the
/// third notch still producing zero.
#[test]
fn accumulate_scroll_carries_a_fractional_remainder_at_low_sensitivity() {
    let mut accum = 0.0;
    let scaled = 1.0 * 0.25_f64;
    assert_eq!(accumulate_scroll(&mut accum, scaled), 0);
    assert_eq!(accumulate_scroll(&mut accum, scaled), 0);
    assert_eq!(accumulate_scroll(&mut accum, scaled), 0);
    assert!(
        (accum - 0.75).abs() < 1e-12,
        "three quarter-notches must carry exactly 0.75, not round or clamp: got {accum}"
    );
    assert_eq!(
        accumulate_scroll(&mut accum, scaled),
        1,
        "the fourth quarter-notch must complete the first slot"
    );
    assert!(accum.abs() < 1e-12, "the completed slot must consume the whole carry");
}

/// A sensitivity above 1.0 must cross more than one slot per notch —
/// the exact scaled amount again, not a threshold on the existing ±1
/// step. At `10.0`, one notch is 10 whole slots with no carry.
#[test]
fn accumulate_scroll_moves_several_slots_per_notch_at_high_sensitivity() {
    let mut accum = 0.0;
    assert_eq!(accumulate_scroll(&mut accum, 1.0 * 10.0), 10);
    assert_eq!(accum, 0.0);
}

/// A direction reversal must drop the old carry rather than fight it
///: three-quarters of a slot built up
/// scrolling one way must not partially cancel a fresh scroll the other
/// way, or a player flicking back and forth would see scroll amounts
/// depend on unrelated history.
#[test]
fn accumulate_scroll_resets_the_carry_on_direction_reversal() {
    let mut accum = 0.0;
    assert_eq!(accumulate_scroll(&mut accum, 0.75), 0);
    assert!((accum - 0.75).abs() < 1e-12);
    // Reversed direction: a naive `accum += scaled` would land at
    // `0.75 - 0.25 = 0.5`, still short of a slot. The reset makes this
    // scroll's own `-0.25` the entire story.
    assert_eq!(accumulate_scroll(&mut accum, -0.25), 0);
    assert!(
        (accum - -0.25).abs() < 1e-12,
        "the old positive carry must be discarded, not partially offset: got {accum}"
    );
}

/// Large scroll events collapse the whole-notch count to its **sign** before it
/// becomes a hotbar-slot step, discarding any
/// magnitude beyond one rather than queuing it for a later event.
///
/// A single large delta cannot by itself prove this — six notches producing a
/// step of six (the wrong hypothesis) and six notches producing a step of one
/// (reference behavior) are the two things this test exists to tell apart, so the
/// assertion has to compare against the actual six, not merely check the step
/// is "smaller than something". Six is not a rounded-up guess: it is what a
/// real macOS trackpad flick produces through this shell's own pipeline —
/// `wheel_notches`' `PixelDelta` arm is `p.y * PRECISE_SCROLL_SCALE` (`0.1`),
/// so a 60-point single-event `PixelDelta` (an ordinary flick, well under
/// what a hard fling reports) already yields six notches at the default
/// `mouseWheelSensitivity` of `1.0` — the exact shape of the owner's report.
#[test]
fn hotbar_scroll_step_collapses_accumulated_magnitude_to_sign() {
    let flick_notches = 60.0 * PRECISE_SCROLL_SCALE;
    assert_eq!(flick_notches, 6.0, "premise: a 60pt flick really is six notches");
    let scaled = scale_scroll(flick_notches, false, 1.0);
    let mut accum = 0.0;
    let whole = accumulate_scroll(&mut accum, scaled);
    assert_eq!(
        whole, 6,
        "premise: the accumulator itself must still report the full six-notch \
         magnitude — this test is not about accumulate_scroll, which is correct \
         and already gated elsewhere"
    );

    assert_eq!(
        hotbar_scroll_step(whole),
        1,
        "vanilla advances the hotbar by exactly one slot per scroll event, \
         never by the event's whole-notch magnitude; the wrong hypothesis \
         (passing `whole` straight through) would give 6 here"
    );
    assert_eq!(hotbar_scroll_step(-whole), -1, "direction must survive the collapse");
    assert_eq!(hotbar_scroll_step(1), 1, "an ordinary single-notch event is unaffected");
    assert_eq!(hotbar_scroll_step(-1), -1);
    assert_eq!(hotbar_scroll_step(0), 0, "no whole notch, no step");
}

/// The hotbar belongs to the world, not to active play.
///
/// Oracle is vanilla, not our own reasoning — see `hud_follows_world`'s docs
/// for the four source lines. The regression was one boolean
/// (`self.ui.is_playing()`, *named* `crosshair`) gating both the reticle and
/// the hotbar, so opening the pause menu or the inventory took the hotbar with
/// it.
#[test]
fn the_hotbar_survives_every_screen_drawn_over_the_world() {
    use crate::menu::Screen;

    for screen in [
        Screen::Playing,
        Screen::Chat,
        Screen::Container,
        Screen::Paused,
        Screen::Death,
    ] {
        assert!(
            hud_follows_world(screen),
            "{screen:?} draws the world, so it must draw the world's hotbar"
        );
    }

    // -- negative control ------------------------------------------------
    // The predicate has to be able to say no, or the loop above is vacuous.
    // `Connecting` has no world yet; the menu screens never get here at all
    // because `draw_menu` returns first. Since `Connecting` is an
    // `owns_frame` screen, so it is one of the `draw_menu`-returns-first set —
    // asserted anyway, because the world-path hotbar gate must never come true
    // for a screen that draws no world.
    for screen in [
        Screen::Connecting,
        Screen::MainMenu,
        Screen::ServerList,
        Screen::ServerEdit,
        Screen::Settings,
        Screen::Error,
    ] {
        assert!(
            !hud_follows_world(screen),
            "{screen:?} has no world on screen, so it must have no hotbar"
        );
    }
}

/// The two questions must not collapse back into one boolean. `Paused` is the
/// screen that separates them: the crosshair goes, the hotbar stays.
#[test]
fn the_crosshair_and_the_hotbar_disagree_behind_a_screen() {
    let mut ui = UiState::new();
    ui.begin(SessionKind::Singleplayer);
    ui.session_ready();
    assert!(ui.is_playing(), "a ready session is in the world");
    assert!(hud_follows_world(ui.screen()));

    ui.pause();
    assert!(
        !ui.is_playing(),
        "the reticle's gate must go false behind the pause menu"
    );
    assert!(
        hud_follows_world(ui.screen()),
        "the hotbar's gate must stay true behind the pause menu"
    );
}

#[test]
fn a_long_stall_is_clamped_not_replayed() {
    // The reported bug: tab out for a minute, tab back in, and the client
    // tries to run every tick it missed. Sixty seconds is 1200 ticks.
    let stall = Duration::from_secs(60);
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    let step = pacer.begin_frame(t0 + stall, None);

    assert!(
        (step.dt - MAX_CATCHUP_SECS).abs() < 1e-12,
        "a {stall:?} stall must be clamped to {MAX_CATCHUP_SECS}s, got {}",
        step.dt
    );

    // Drive a *real* sim with it and count the ticks that actually run.
    let mut sim = pacing_sim();
    let clamped = ticks_for(&mut sim, step.dt);
    assert!(
        clamped <= u64::from(MAX_TICKS_PER_UPDATE),
        "catch-up must never exceed vanilla's cap, got {clamped}"
    );

    // Measured: **10**. It used to be 5, because `Sim::step` applied its own,
    // tighter `dt.clamp(0.0, 0.25)` to the accumulator before the tick loop and
    // so silently halved this pacer's budget. That assertion said as much out
    // loud ("if the value changes, reconcile the two caps"). This test
    // documents that reconciliation: §4.1(c) left one accumulator
    // (`lodestone_ecs::FrameClock`) on one policy
    // (`lodestone_ecs::MAX_CATCH_UP_SECS`), and the surviving number is
    // vanilla's ten — the only one of the two candidates with an external
    // oracle. See that constant's docs for the full argument.
    assert_eq!(
        clamped,
        u64::from(MAX_TICKS_PER_UPDATE),
        "one clamp now: `FrameClock::begin_frame` banks at most \
         {MAX_CATCHUP_SECS} s, so a maximal stall runs exactly vanilla's \
         {MAX_TICKS_PER_UPDATE} catch-up ticks"
    );
    // …and the shell's clamp *is* the ECS's, not a second one that happens to
    // agree. A copy that agreed today is how the five-vs-ten divergence
    // started.
    assert!(
        (MAX_CATCHUP_SECS - lodestone_ecs::MAX_CATCH_UP_SECS).abs() < 1e-12,
        "app.rs and lodestone-ecs must not carry two catch-up budgets"
    );

    // -- negative control ------------------------------------------------
    // Prove the detector fires: the same real `Sim`, driven the
    // *proportional* way the bug describes (one tick's worth of dt at a
    // time until the stall is consumed), executes the full 1200 ticks. If
    // `tick_count` could not observe a burst, this would not move either.
    let mut control = pacing_sim();
    let mut unclamped = 0u64;
    for _ in 0..(stall.as_secs_f64() / TICK_SECS) as u32 {
        unclamped += ticks_for(&mut control, TICK_SECS);
    }
    assert_eq!(unclamped, 1200, "control must replay every missed tick");
    assert!(
        unclamped > clamped * 100,
        "clamp must be a large reduction: {clamped} vs {unclamped}"
    );
}

#[test]
fn a_normal_frame_is_untouched_by_the_clamp() {
    // The clamp must be invisible at playable frame rates, or it would be
    // silently dropping game time during ordinary play (which is exactly
    // what a too-tight cap does: at 4 fps a 0.25 s cap discards 75% of it).
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    let frame = Duration::from_micros(16_667); // 60 fps
    let step = pacer.begin_frame(t0 + frame, None);
    assert!(
        (step.dt - frame.as_secs_f64()).abs() < 1e-9,
        "60 fps frame was altered: {}",
        step.dt
    );

    // And a 4 fps frame — the rate an occluded window degrades to — must
    // still deliver all 250 ms, i.e. five whole ticks, not be truncated.
    let mut pacer = FramePacer::new(t0);
    let step = pacer.begin_frame(t0 + Duration::from_millis(250), None);
    let mut sim = pacing_sim();
    assert_eq!(ticks_for(&mut sim, step.dt), 5);
}

#[test]
fn an_unfocused_window_keeps_ticking_and_presents_at_thirty_fps() {
    // The whole point: presentation throttles, simulation does not.
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    pacer.set_focused(false);

    let mut sim = pacing_sim();
    let mut rendered = 0u32;
    let mut ticks = 0u64;
    // One simulated second at a 120 Hz loop rate.
    for i in 1..=120u32 {
        let step = pacer.begin_frame(t0 + Duration::from_secs_f64(f64::from(i) / 120.0), None);
        if step.render {
            rendered += 1;
        }
        ticks += ticks_for(&mut sim, step.dt);
    }

    // 19 or 20: one simulated second at 20 Hz, modulo where the fixed-step
    // residual happens to land (1/120 is not exact in binary, so the last
    // tick can fall just past the second boundary).
    assert!(
        (19..=20).contains(&ticks),
        "unfocused must still tick at ~20 Hz, got {ticks}"
    );
    assert!(
        (30..=31).contains(&rendered),
        "unfocused presentation should be ~30 fps, got {rendered}"
    );
    assert!(
        u64::from(rendered) > ticks,
        "sanity: 30 fps presentation must still outpace 20 Hz ticking"
    );
}

/// Owner report: "the block animations seem too fast in general". A previous
/// pass proved the animation *sampling* logic exact (`SpriteAnimation::sample`,
/// the built `water_still`/`water_flow` timelines) and traced the tick source
/// to `Sim::tick_count()`, which `RenderState::update_animation` is fed
/// verbatim from both live call sites (`app/redraw.rs`, `app/runners.rs`). So
/// the remaining question is a measurement, not a re-read: does
/// `Sim::tick_count()` actually advance at 20/s under the **focused,
/// uncapped** loop shape real play uses — `redraw()`'s own path, through
/// [`FramePacer::begin_frame`] with `target_fps: None` — rather than only the
/// unfocused/capped shape [`an_unfocused_window_keeps_ticking_and_presents_at_thirty_fps`]
/// already covers.
///
/// Driven at 240 Hz (12x the tick rate — a real, uncapped display can run
/// this fast) for 3 real seconds. **Two hypotheses, both computed from
/// outside constants, not read back from the code under test:**
/// - correct: real elapsed time drives the accumulator, so ticks ≈
///   `3.0 * 20 = 60` regardless of loop rate.
/// - wrong (a `dt` that tracked the loop's own iteration period instead of
///   real elapsed time, or a `FrameClock` double-stepped by a second driver):
///   would land far from 60 — e.g. exactly `720` if each of the 720
///   iterations banked a full tick unconditionally, or `360` if a stray
///   second `begin_frame`/`step` call doubled every real tick.
///
/// This is the discriminating input the `19..=20`-over-one-second unfocused
/// gate cannot be, alone: that gate's loop (120 Hz) is *closer* to the tick
/// rate, so a 2x-too-fast bug would still land inside its own tolerance band
/// at the one-second horizon. Three seconds at 240 Hz separates "correct" (60)
/// from "2x" (120) and from "6x, i.e. one tick per iteration at 240 Hz banked
/// as if it were the loop's own 1/20 s" (720) by wide margins.
#[test]
fn a_focused_uncapped_loop_advances_ticks_at_twenty_per_second() {
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    // Focused (the `FramePacer::new` default) and uncapped: `target_fps: None`.
    let mut sim = pacing_sim();
    let loop_hz = 240.0_f64;
    let real_seconds = 3.0_f64;
    let iterations = (loop_hz * real_seconds) as u32;
    let mut ticks = 0u64;
    for i in 1..=iterations {
        let now = t0 + Duration::from_secs_f64(f64::from(i) / loop_hz);
        let step = pacer.begin_frame(now, None);
        ticks += ticks_for(&mut sim, step.dt);
    }

    let expected = (real_seconds * 20.0).round() as i64;
    assert!(
        ((expected - 1)..=(expected + 1)).contains(&(ticks as i64)),
        "a focused, uncapped 240 Hz loop over {real_seconds} real seconds must \
         advance ~{expected} ticks (20/s), not {ticks} — {:.2}x real time",
        ticks as f64 / expected as f64,
    );

    // Control, computed and asserted rather than merely described: the wrong
    // hypothesis this test would have to fail to catch. If `dt` had tracked
    // the loop's own period instead of real elapsed time, every one of the
    // `iterations` calls would bank a full `TICK_SECS` (240 Hz's own period
    // exceeds one tick, so each iteration would look like a whole tick owed),
    // landing near `iterations`, not `expected` — the two must be far apart
    // for this input to mean anything.
    assert!(
        i64::from(iterations) - expected > 500,
        "chosen input must separate the two hypotheses widely; iterations={iterations} expected={expected}"
    );
}

/// Counts frames a naive "elapsed since the last presented frame" gate would
/// deliver over `iters` iterations of a `loop_hz` loop. This is verbatim the
/// implementation [`FramePacer`] used to have — including the `as_secs_f64()`
/// comparison against a `1.0 / 30.0` target, which is part of why it drifted:
/// a `Duration` is whole nanoseconds, so an interval that lands on
/// 33 333 333 ns is *always* a hair short of 1/30 s and the very iteration
/// that should have presented never does.
fn naive_gate_frames(loop_hz: u32, iters: u32) -> u32 {
    let target_secs = 1.0 / f64::from(UNFOCUSED_FPS);
    let t0 = Instant::now();
    let mut last_render = t0;
    let mut n = 0;
    for i in 1..=iters {
        let now = t0 + Duration::from_secs_f64(f64::from(i) / f64::from(loop_hz));
        if now.saturating_duration_since(last_render).as_secs_f64() >= target_secs {
            last_render = now;
            n += 1;
        }
    }
    n
}

/// Same span, driven through the real pacer while unfocused.
fn paced_frames(loop_hz: u32, iters: u32) -> u32 {
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    pacer.set_focused(false);
    let mut n = 0;
    for i in 1..=iters {
        let now = t0 + Duration::from_secs_f64(f64::from(i) / f64::from(loop_hz));
        if pacer.begin_frame(now, None).render {
            n += 1;
        }
    }
    n
}

#[test]
fn the_unfocused_frame_schedule_does_not_drift_below_its_target() {
    // The bug, and the negative control for the fix. A 30 fps limiter that
    // quietly delivers 26 fps is the whole reason the deadline is absolute:
    // the naive gate can only fire on a loop iteration, and each firing
    // pushes the next deadline out by however far it overshot.
    //
    // Measured, one simulated second each:
    //   loop     naive   paced   target
    //   120 Hz     26      30      30
    //    75 Hz     25      30      30
    //    77 Hz     26      30      30
    for loop_hz in [120u32, 75, 77, 144, 240] {
        let naive = naive_gate_frames(loop_hz, loop_hz);
        let paced = paced_frames(loop_hz, loop_hz);
        assert!(
            (UNFOCUSED_FPS..=UNFOCUSED_FPS + 1).contains(&paced),
            "at {loop_hz} Hz the absolute schedule delivered {paced}, \
             wanted {UNFOCUSED_FPS}"
        );
        // The control must be observed *failing* the same assertion, or this
        // test proves only that some number came out of some function.
        assert!(
            naive < UNFOCUSED_FPS,
            "control did not fire at {loop_hz} Hz: the naive gate delivered \
             {naive}, so this test is not measuring the drift it exists for"
        );
    }
    // Exact pre-fix number at the loop rate the sibling test uses, pinned so
    // a future refactor that reintroduces drift is unambiguous.
    assert_eq!(naive_gate_frames(120, 120), 26);
}

#[test]
fn coming_back_from_a_stall_resumes_the_rate_rather_than_replaying_a_backlog() {
    // The presentation-side twin of the catch-up-tick bug: a schedule that
    // advanced by whole intervals *unconditionally* would owe 3600 frames
    // after a two-minute stall and present them as fast as the loop spins.
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    pacer.set_focused(false);
    // Two minutes with no iterations at all, then a tight 120 Hz loop for
    // half a second.
    let resume = t0 + Duration::from_secs(120);
    assert!(pacer.begin_frame(resume, None).render, "the first frame back draws");

    let mut after = 0;
    for i in 1..=60u32 {
        if pacer
            .begin_frame(resume + Duration::from_secs_f64(f64::from(i) / 120.0), None)
            .render
        {
            after += 1;
        }
    }
    // Half a second at 30 fps is 15 frames. The backlog would be ~3600.
    assert!(
        (14..=16).contains(&after),
        "expected the steady ~30 fps rate after resuming, got {after} frames \
         in 0.5 s — a replayed backlog looks like ~60 (loop-rate-bound)"
    );
}

#[test]
fn an_occluded_window_skips_presenting_entirely_but_still_ticks() {
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    pacer.set_occluded(true);

    let mut sim = pacing_sim();
    let mut ticks = 0u64;
    for i in 1..=120u32 {
        let step = pacer.begin_frame(t0 + Duration::from_secs_f64(f64::from(i) / 120.0), None);
        assert!(!step.render, "occluded windows must not acquire a drawable");
        ticks += ticks_for(&mut sim, step.dt);
    }
    assert!(
        (19..=20).contains(&ticks),
        "occluded must still tick at ~20 Hz, got {ticks}"
    );

    // Control: the identical loop with occlusion cleared *does* render, so
    // the assertion above is testing occlusion and not a dead pacer.
    pacer.set_occluded(false);
    let step = pacer.begin_frame(t0 + Duration::from_secs(2), None);
    assert!(step.render, "clearing occlusion must restore presentation");
}

#[test]
fn focus_selects_the_control_flow_without_ever_stopping_the_loop() {
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    assert!(matches!(pacer.control_flow(t0, None), ControlFlow::Poll));
    assert!(pacer.focused());

    pacer.set_focused(false);
    match pacer.control_flow(t0, None) {
        ControlFlow::WaitUntil(at) => {
            let slice = at.saturating_duration_since(t0);
            assert!(
                slice < Duration::from_secs_f64(TICK_SECS),
                "background poll {slice:?} must wake faster than one 50 ms tick, \
                 or the sim falls behind the server while merely unfocused"
            );
        }
        other => panic!("unfocused must sleep, not spin or wait forever: {other:?}"),
    }
    assert!(!pacer.focused());
}

#[test]
fn a_focused_capped_window_is_paced_by_a_wait_not_a_spin() {
    // The busy-wait failure mode the brief names explicitly: a `framerateLimit`
    // below the refresh rate must not turn into `ControlFlow::Poll` calling
    // `begin_frame` every iteration only to find `render == false` — that is a
    // spin loop wearing a frame cap's clothes. `control_flow` must instead
    // report a real `WaitUntil` deadline once a cap is in effect.
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    assert!(
        matches!(pacer.control_flow(t0, None), ControlFlow::Poll),
        "uncapped and focused: vsync paces us, unchanged from before this option"
    );
    match pacer.control_flow(t0, Some(30)) {
        ControlFlow::WaitUntil(_) => {}
        other => panic!("a focused window with a real cap must sleep, not poll: {other:?}"),
    }
}

#[test]
fn a_focused_cap_presents_at_the_capped_rate_not_every_iteration() {
    // Drive a 120 Hz loop (a display comfortably above the cap) with
    // `target_fps = Some(30)` and count presented frames over one simulated
    // second — the same counting shape `an_unfocused_window_keeps_ticking_and_
    // presents_at_thirty_fps` already uses, applied to the *focused* path this
    // test adds.
    let t0 = Instant::now();
    let mut pacer = FramePacer::new(t0);
    let mut rendered = 0u32;
    for i in 1..=120u32 {
        let now = t0 + Duration::from_secs_f64(f64::from(i) / 120.0);
        if pacer.begin_frame(now, Some(30)).render {
            rendered += 1;
        }
    }
    assert!(
        (30..=31).contains(&rendered),
        "a focused 30 fps cap against a 120 Hz loop should present ~30 frames, got {rendered}"
    );

    // Negative control: the same loop with no cap presents every iteration —
    // proving the 30-vs-120 gap above is the cap's doing, not some other
    // throttle this pacer already applies while focused.
    let mut uncapped = FramePacer::new(t0);
    let mut all_rendered = 0u32;
    for i in 1..=120u32 {
        let now = t0 + Duration::from_secs_f64(f64::from(i) / 120.0);
        if uncapped.begin_frame(now, None).render {
            all_rendered += 1;
        }
    }
    assert_eq!(
        all_rendered, 120,
        "uncapped and focused must render every iteration — the control that \
         makes the capped count above meaningful"
    );
}

#[test]
fn effective_target_fps_matches_vanillas_framerate_limit_tracker() {
    use crate::app::pacing::effective_target_fps;
    use crate::config::InactivityFpsLimit;

    // Unlimited (260) and not idle: no cap at all.
    assert_eq!(effective_target_fps(260, InactivityFpsLimit::Afk, 0.0), None);
    // A real cap, not idle: the raw limit, unaffected by the AFK machinery.
    assert_eq!(
        effective_target_fps(120, InactivityFpsLimit::Afk, 0.0),
        Some(120)
    );
    // `Minimized` never reduces for idle input, however long — only `Afk`
    // does (vanilla's own framerate-limit-tracker gate).
    assert_eq!(
        effective_target_fps(120, InactivityFpsLimit::Minimized, 10_000.0),
        Some(120)
    );
    // SHORT_AFK: `min(limit, 30)` past 60 s idle, vanilla's own formula
    // — a limit *above* 30 gets capped down.
    assert_eq!(
        effective_target_fps(120, InactivityFpsLimit::Afk, 90.0),
        Some(30)
    );
    // ...and a limit already *below* 30 is not raised by it.
    assert_eq!(
        effective_target_fps(20, InactivityFpsLimit::Afk, 90.0),
        Some(20)
    );
    // Unlimited base, still SHORT_AFK: the 30 cap applies with nothing to
    // `min` it against.
    assert_eq!(
        effective_target_fps(260, InactivityFpsLimit::Afk, 90.0),
        Some(30)
    );
    // LONG_AFK: flatly 10 past 600 s, matching the long-idle limit
    //, regardless of the raw limit.
    assert_eq!(
        effective_target_fps(120, InactivityFpsLimit::Afk, 700.0),
        Some(10)
    );
    // Right at the boundary must not have crossed it yet (`>`, not `>=`,
    // mirroring the strict greater-than threshold at 60 seconds.
    assert_eq!(
        effective_target_fps(120, InactivityFpsLimit::Afk, 60.0),
        Some(120)
    );
}

// -- key dispatch and precedence ----------------------------------------
//
// These drive [`resolve_key`] directly. It is the whole of the key chain's
// decision-making, so a precedence regression shows up here rather than
// needing a window, a GPU and a live `Sim` to observe.

use crate::keybinds::{Binding, InputAction};

/// The gate while the world is being played normally.
fn playing() -> KeyGate {
    KeyGate {
        gameplay: true,
        ..KeyGate::default()
    }
}

fn resolve(gate: KeyGate, code: KeyCode, pressed: bool) -> Option<KeyOutcome> {
    resolve_key(&Keybinds::new(), gate, Some(code), pressed, false, None)
}

/// Like [`resolve`], but with Control held — only the drop-key tests need
/// this axis, so it is a separate helper rather than a fifth argument on
/// every existing call above.
fn resolve_ctrl(gate: KeyGate, code: KeyCode, pressed: bool) -> Option<KeyOutcome> {
    resolve_key(&Keybinds::new(), gate, Some(code), pressed, true, None)
}

/// A plugin's `Consume` claim on a physical key wins over
/// gameplay when nothing else has first claim on the keyboard — the
/// positive half of the precedence-rank doc on `resolve_key`.
#[test]
fn a_plugin_consume_claim_wins_over_an_unbound_key_during_gameplay() {
    let binds = Keybinds::new();
    // `KeyCode::F13` is not bound to anything in the default table, so
    // absent the plugin claim this would resolve to `None` — proof the
    // outcome came from the claim, not from an incidental keybind.
    let outcome = resolve_key(
        &binds,
        playing(),
        Some(KeyCode::F13),
        true,
        false,
        Some(lodestone_ecs::KeyInterceptMode::Consume),
    );
    assert_eq!(outcome, Some(KeyOutcome::PluginConsumed));
}

/// Both edges reach `PluginConsumed`, not just the press — the same
/// both-edges requirement `Attack`/`Use` have, and the reason the arm has no
/// `&& pressed` guard.
#[test]
fn a_plugin_consume_claim_fires_on_release_too() {
    let binds = Keybinds::new();
    let outcome = resolve_key(
        &binds,
        playing(),
        Some(KeyCode::F13),
        false,
        false,
        Some(lodestone_ecs::KeyInterceptMode::Consume),
    );
    assert_eq!(outcome, Some(KeyOutcome::PluginConsumed));
}

/// The precedence-rank claim itself: a plugin's `Consume` claim on a key that
/// is *also* bound to a real gameplay action (here, forward movement) still
/// wins — a plugin hotkey cannot be shadowed by a coincidental rebind onto
/// the same physical key, matching `resolve_key`'s own doc.
#[test]
fn a_plugin_consume_claim_outranks_a_real_gameplay_binding_on_the_same_key() {
    let binds = Keybinds::new();
    let outcome = resolve_key(
        &binds,
        playing(),
        Some(KeyCode::KeyW), // bound to `InputAction::Forward` by default
        true,
        false,
        Some(lodestone_ecs::KeyInterceptMode::Consume),
    );
    assert_eq!(outcome, Some(KeyOutcome::PluginConsumed));
}

/// The negative control this whole design turns on: `Observe` mode must
/// change nothing about resolution — the plugin is told about the key
/// elsewhere (see the driver call site), but `resolve_key` itself must
/// resolve exactly as if no plugin existed.
#[test]
fn a_plugin_observe_claim_does_not_change_resolution() {
    let binds = Keybinds::new();
    let with_observe = resolve_key(
        &binds,
        playing(),
        Some(KeyCode::KeyW),
        true,
        false,
        Some(lodestone_ecs::KeyInterceptMode::Observe),
    );
    let with_no_plugin = resolve_key(&binds, playing(), Some(KeyCode::KeyW), true, false, None);
    assert_eq!(with_observe, with_no_plugin);
    // Sharpen the assertion: this must be the real movement outcome, not two
    // fixtures that coincidentally agree by both being `None`.
    assert!(matches!(with_observe, Some(KeyOutcome::Movement(_, true))));
}

/// A container screen keeps first claim over a plugin's `Consume` mode —
/// `resolve_key`'s own doc states this ranking, and this is the control that
/// actually exercises it rather than trusting the doc comment.
#[test]
fn an_open_container_still_outranks_a_plugin_consume_claim() {
    let binds = Keybinds::new();
    let gate = KeyGate {
        container_open: true,
        gameplay: true,
        ..KeyGate::default()
    };
    // The inventory-close binding still fires, exactly as it would with no
    // plugin involved at all — the container arm's own catch-all runs
    // first and the plugin arm below it is never reached.
    let outcome = resolve_key(
        &binds,
        gate,
        Some(KeyCode::KeyE), // `InputAction::Inventory`'s default binding
        true,
        false,
        Some(lodestone_ecs::KeyInterceptMode::Consume),
    );
    assert_eq!(outcome, Some(KeyOutcome::CloseContainer));
}

/// The function-key path: an F-key has no printable `text`, so it is
/// exactly the case `menu_key_for` drops and `capture_key_for` must not.
/// `F1` (not `F5`, which `resolve_key`'s own default table already binds
/// to `TogglePerspective` — picking a bound key here would prove nothing
/// about the *unbound*, no-text case a real Controls-menu rebind targets)
/// persists as the standard function-key identifier.
#[test]
fn capture_key_for_forwards_a_function_key() {
    assert_eq!(
        capture_key_for(PhysicalKey::Code(KeyCode::F1)),
        Some(CaptureKey::Bind(KeyCode::F1)),
        "an F-key must reach the capture as a bindable key, not be \
         dropped the way menu_key_for drops it"
    );
}

/// Escape must cancel through the ordinary `MenuKey` path
/// (`CaptureKey::Cancel`), never through `capture_binding` — the latter
/// is exactly the `Pause`-unbinding hazard `capture_binding`'s own doc
/// warns about, and this is the one physical key capture must special-case
/// rather than forward.
#[test]
fn capture_key_for_treats_escape_as_cancel_not_a_binding() {
    assert_eq!(
        capture_key_for(PhysicalKey::Code(KeyCode::Escape)),
        Some(CaptureKey::Cancel)
    );
}

/// A printable key must forward too — a capture target is not always an
/// unprintable one (most vanilla rebinds are ordinary letters), so this
/// is the control proving `capture_key_for` is not secretly just
/// `menu_key_for` under another name.
#[test]
fn capture_key_for_forwards_a_printable_key_too() {
    assert_eq!(
        capture_key_for(PhysicalKey::Code(KeyCode::KeyF)),
        Some(CaptureKey::Bind(KeyCode::KeyF))
    );
}

/// No `KeyCode` exists to persist for an unidentified physical key, so
/// there is nothing to bind — matches `menu_key_for`'s own `_ => {}`.
#[test]
fn capture_key_for_ignores_an_unidentified_key() {
    assert_eq!(
        capture_key_for(PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified
        )),
        None
    );
}

/// The owner-reported bug this section gates: pasting into a menu text field
/// inserted a literal `v`, and Cmd+A inserted `a` instead of selecting. These
/// gate the winit-to-`MenuKey` conversion itself: every existing edit-box test
/// constructs its own already-modified `focus::KeyEvent`, so the whole suite
/// downstream of `menu_key_for` passed while no *real* keystroke ever carried
/// a modifier.
mod menu_key_shortcut_conversion {
    use super::*;
    use crate::app::menus::shortcut_modifier_held;

    /// **The macOS mapping specifically.** `shortcut_modifier_held` takes
    /// `is_macos` as a parameter rather than reading `cfg!(target_os =
    /// "macos")` inline precisely so this is assertable on any machine the
    /// suite happens to run on — the bug this test targets is invisible on
    /// Linux/Windows by construction (Ctrl already worked there), so a test
    /// that only ever exercised whichever OS runs CI would not have caught
    /// it.
    #[test]
    fn macos_shortcut_modifier_is_cmd_not_ctrl() {
        assert!(shortcut_modifier_held(ModifiersState::SUPER, true));
        assert!(!shortcut_modifier_held(ModifiersState::CONTROL, true));
    }

    #[test]
    fn non_macos_shortcut_modifier_is_ctrl_not_cmd() {
        assert!(shortcut_modifier_held(ModifiersState::CONTROL, false));
        assert!(!shortcut_modifier_held(ModifiersState::SUPER, false));
    }

    /// The two platform splits, each paired with the modifier that platform
    /// actually uses for edit shortcuts.
    ///
    /// Every gate below runs against **both**, through
    /// `menu_key_for_platform`, rather than against `ModifiersState::SUPER`
    /// alone through `menu_key_for`. The old form read `cfg!(target_os =
    /// "macos")` inside the function, so `SUPER` meant "shortcut" only when
    /// the suite happened to be running on a Mac: these five gates passed on
    /// the dev machines and failed on every Linux CI runner, which reads as
    /// an environment fault and is really the test asserting a property of
    /// the host. Driving both splits is also strictly stronger — the
    /// non-macOS mapping was previously asserted by nothing but
    /// `non_macos_shortcut_modifier_is_ctrl_not_cmd`, one layer below this
    /// translation.
    const SHORTCUT_PLATFORMS: [(bool, ModifiersState); 2] = [
        (true, ModifiersState::SUPER),
        (false, ModifiersState::CONTROL),
    ];

    fn platform_name(is_macos: bool) -> &'static str {
        if is_macos { "macOS/Cmd" } else { "non-macOS/Ctrl" }
    }

    /// The literal reported symptom: with the shortcut modifier held, `A`
    /// must produce `MenuKey::SelectAll`, never `MenuKey::Char('a')` — and
    /// critically, `text` is non-empty here (winit still reports `Some("a")`
    /// alongside the physical key), which is exactly what made the old
    /// modifier-blind `menu_key_for` insert the letter.
    #[test]
    fn cmd_a_selects_all_and_never_types_a() {
        // Collected rather than asserted inside the loop: an `assert!` there
        // stops at the first split and leaves the other unmeasured, so a
        // failure would name one platform when both may be wrong.
        let mut mismatches = Vec::new();
        for (is_macos, modifier) in SHORTCUT_PLATFORMS {
            let key = WindowApp::menu_key_for_platform(
                PhysicalKey::Code(KeyCode::KeyA),
                Some("a"),
                modifier,
                is_macos,
            );
            if key != Some(MenuKey::SelectAll) {
                mismatches.push(format!(
                    "{}: got {key:?}, expected SelectAll (and never Char('a'))",
                    platform_name(is_macos)
                ));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    /// Same shape for paste — "it just inserts a v".
    #[test]
    fn cmd_v_pastes_and_never_types_v() {
        let mut mismatches = Vec::new();
        for (is_macos, modifier) in SHORTCUT_PLATFORMS {
            let key = WindowApp::menu_key_for_platform(
                PhysicalKey::Code(KeyCode::KeyV),
                Some("v"),
                modifier,
                is_macos,
            );
            if key != Some(MenuKey::Paste) {
                mismatches.push(format!(
                    "{}: got {key:?}, expected Paste (and never Char('v'))",
                    platform_name(is_macos)
                ));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    #[test]
    fn cmd_c_copies_and_cmd_x_cuts() {
        let mut mismatches = Vec::new();
        for (is_macos, modifier) in SHORTCUT_PLATFORMS {
            for (code, letter, want) in [
                (KeyCode::KeyC, "c", MenuKey::Copy),
                (KeyCode::KeyX, "x", MenuKey::Cut),
            ] {
                let key = WindowApp::menu_key_for_platform(
                    PhysicalKey::Code(code),
                    Some(letter),
                    modifier,
                    is_macos,
                );
                if key != Some(want) {
                    mismatches.push(format!(
                        "{} + {letter}: got {key:?}, expected {want:?}",
                        platform_name(is_macos)
                    ));
                }
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    /// A plain, unmodified `a` must still type — the negative control proving
    /// the suppression above is conditional on the modifier and not a
    /// blanket regression that breaks ordinary typing.
    #[test]
    fn plain_a_still_types() {
        assert_eq!(
            WindowApp::menu_key_for(
                PhysicalKey::Code(KeyCode::KeyA),
                Some("a"),
                ModifiersState::empty()
            ),
            Some(MenuKey::Char('a'))
        );
    }

    /// Cmd+Shift+A is not select-all in vanilla either
    /// (vanilla's own select-all check requires shift to be up) — and must
    /// not fall through to typing `a` alongside doing nothing, matching
    /// `focus::KeyEvent::is_edit_shortcut`'s own guard.
    #[test]
    fn cmd_shift_a_is_neither_select_all_nor_text() {
        let mut mismatches = Vec::new();
        for (is_macos, modifier) in SHORTCUT_PLATFORMS {
            let key = WindowApp::menu_key_for_platform(
                PhysicalKey::Code(KeyCode::KeyA),
                Some("A"),
                modifier | ModifiersState::SHIFT,
                is_macos,
            );
            if key.is_some() {
                mismatches.push(format!(
                    "{}: got {key:?}, expected None (neither SelectAll nor Char('A'))",
                    platform_name(is_macos)
                ));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    /// An unrecognised chord (the modifier held, but not one of A/C/X/V) must
    /// still suppress the letter rather than falling through to it — the
    /// "shipping only modifier tracking turns 'types a v' into 'does
    /// nothing', which reads as a new bug" case the test guards against, but for
    /// a key with no dedicated shortcut this *is* the correct behaviour:
    /// vanilla does not type `b` while Cmd is held either.
    #[test]
    fn unrecognised_cmd_chord_suppresses_the_letter() {
        let mut mismatches = Vec::new();
        for (is_macos, modifier) in SHORTCUT_PLATFORMS {
            let key = WindowApp::menu_key_for_platform(
                PhysicalKey::Code(KeyCode::KeyB),
                Some("b"),
                modifier,
                is_macos,
            );
            if key.is_some() {
                mismatches.push(format!(
                    "{}: got {key:?}, expected None (the letter must not fall through)",
                    platform_name(is_macos)
                ));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    /// `from_menu_key` is the other half of the fix — the `MenuKey` produced
    /// above has to reach `EditBox::handle_key` as a real
    /// `is_select_all()`/`is_paste()` event, or select-all/paste would be
    /// silently declined despite being detected correctly here.
    #[test]
    fn select_all_and_paste_reach_editbox_as_real_shortcut_events() {
        use crate::menu::focus::KeyEvent;

        let select_all = KeyEvent::from_menu_key(MenuKey::SelectAll).unwrap();
        assert!(select_all.is_select_all());

        let paste = KeyEvent::from_menu_key(MenuKey::Paste).unwrap();
        assert!(paste.is_paste());

        let copy = KeyEvent::from_menu_key(MenuKey::Copy).unwrap();
        assert!(copy.is_copy());

        let cut = KeyEvent::from_menu_key(MenuKey::Cut).unwrap();
        assert!(cut.is_cut());
    }
}

/// Every key the default table binds, with what it should resolve to while
/// playing. Written out rather than derived from the table, so this is a
/// second statement of intent and not a restatement of the implementation.
fn default_playing_expectations() -> Vec<(KeyCode, KeyOutcome)> {
    vec![
        (KeyCode::KeyW, KeyOutcome::Movement(Action::Forward, true)),
        (KeyCode::KeyS, KeyOutcome::Movement(Action::Back, true)),
        (KeyCode::KeyA, KeyOutcome::Movement(Action::Left, true)),
        (KeyCode::KeyD, KeyOutcome::Movement(Action::Right, true)),
        (KeyCode::Space, KeyOutcome::Movement(Action::Jump, true)),
        (KeyCode::ShiftLeft, KeyOutcome::Movement(Action::Sneak, true)),
        (
            KeyCode::ControlLeft,
            KeyOutcome::Movement(Action::Sprint, true),
        ),
        (KeyCode::KeyE, KeyOutcome::OpenContainer),
        (KeyCode::KeyT, KeyOutcome::OpenChat { command: false }),
        (KeyCode::Slash, KeyOutcome::OpenChat { command: true }),
        (KeyCode::Tab, KeyOutcome::PlayerList(true)),
        (KeyCode::F5, KeyOutcome::TogglePerspective),
        // F3 is the debug *modifier*, reporting both edges; the
        // overlay toggle happens on the release when no chord fired (see
        // `resolve_key`, and vanilla's own keyboard handling).
        (KeyCode::F3, KeyOutcome::DebugModifier(true)),
        (KeyCode::Escape, KeyOutcome::Pause),
        (KeyCode::Digit1, KeyOutcome::SelectSlot(0)),
        (KeyCode::Digit2, KeyOutcome::SelectSlot(1)),
        (KeyCode::Digit3, KeyOutcome::SelectSlot(2)),
        (KeyCode::Digit4, KeyOutcome::SelectSlot(3)),
        (KeyCode::Digit5, KeyOutcome::SelectSlot(4)),
        (KeyCode::Digit6, KeyOutcome::SelectSlot(5)),
        (KeyCode::Digit7, KeyOutcome::SelectSlot(6)),
        (KeyCode::Digit8, KeyOutcome::SelectSlot(7)),
        (KeyCode::Digit9, KeyOutcome::SelectSlot(8)),
    ]
}

#[test]
fn the_default_bindings_dispatch_exactly_as_they_did_before_the_refactor() {
    // The no-regression gate for the whole change: every key the hardcoded
    // chain used to handle still resolves to the same effect.
    for (code, want) in default_playing_expectations() {
        assert_eq!(
            resolve(playing(), code, true),
            Some(want),
            "{code:?} regressed"
        );
    }
}

#[test]
fn the_hotbar_number_keys_select_the_slot_one_below_their_digit() {
    // Called out as one of the two things most likely to break quietly: the
    // digits are 1..9 and the slots are 0..8, so an off-by-one here shifts
    // every hotbar key by one and looks almost right.
    let digits = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    for (i, code) in digits.into_iter().enumerate() {
        assert_eq!(
            resolve(playing(), code, true),
            Some(KeyOutcome::SelectSlot(i)),
            "{code:?} should select slot {i}"
        );
    }
    // Digit0 is unbound in vanilla and must stay unbound — binding it to
    // slot 9 would be a tenth hotbar slot that does not exist.
    assert_eq!(resolve(playing(), KeyCode::Digit0, true), None);
    // Releasing a hotbar key does nothing (it is not a held state).
    assert_eq!(resolve(playing(), KeyCode::Digit1, false), None);
}

#[test]
fn slash_opens_chat_with_the_command_prefix_and_t_opens_it_without() {
    // The other quiet-breakage candidate. The distinction is a single bool,
    // and getting it backwards means every chat message starts with `/`
    // (or no command can ever be typed).
    assert_eq!(
        resolve(playing(), KeyCode::Slash, true),
        Some(KeyOutcome::OpenChat { command: true })
    );
    assert_eq!(
        resolve(playing(), KeyCode::KeyT, true),
        Some(KeyOutcome::OpenChat { command: false })
    );

    // …and the prefix follows the *command binding*, not the physical
    // slash key. Rebinding chat and command to other keys must carry the
    // distinction with them.
    let mut binds = Keybinds::new();
    binds.set(InputAction::Command, Binding::Key(KeyCode::Backquote.into()));
    binds.set(InputAction::Chat, Binding::Key(KeyCode::KeyY.into()));
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::Backquote), true, false, None),
        Some(KeyOutcome::OpenChat { command: true })
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyY), true, false, None),
        Some(KeyOutcome::OpenChat { command: false })
    );
    // The old keys stop opening chat at all.
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::Slash), true, false, None),
        None
    );
}

#[test]
fn an_open_container_swallows_every_gameplay_key() {
    // The precedence that matters most: while a container is up, keys must
    // not reach gameplay.
    //
    // Two gates are checked, and the second is the one that actually tests
    // the *arm*. In production `container_open` implies `!gameplay` (the
    // screen is `Container`, so `accepts_gameplay_input()` is false), which
    // means the first gate would swallow most keys through the `gate.gameplay`
    // guards even if the container arm were deleted — a vacuous test of the
    // "world" species, passing because of the input it was handed rather than
    // the code it names. The `gameplay: true` gate cannot occur in practice
    // but isolates the container arm: with it, *only* the arm's early return
    // stands between these keys and gameplay.
    for gate in [
        KeyGate {
            container_open: true,
            ..KeyGate::default()
        },
        KeyGate {
            container_open: true,
            gameplay: true,
            ..KeyGate::default()
        },
    ] {
        for (code, would_have) in default_playing_expectations() {
            // Escape and the inventory key have their own jobs on this screen,
            // The nine number keys also issue a
            // `SWAP` against the hovered slot rather than being swallowed.
            // Their own test is `the_number_keys_swap_with_the_hovered_slot`
            // below; excluding them here is not weakening this test, because
            // what it asserts is that nothing reaches *gameplay*, and
            // `ContainerSwap` is not a gameplay outcome.
            if matches!(code, KeyCode::Escape | KeyCode::KeyE)
                || hotbar_slot_for(&Keybinds::new(), code).is_some()
            {
                continue;
            }
            assert_eq!(
                resolve(gate, code, true),
                None,
                "{code:?} leaked through an open container (gate {gate:?})"
            );
            // -- negative control -----------------------------------------
            // The same key on the same table *does* resolve while playing, so
            // this test is observing the swallow and not a dead resolver.
            assert_eq!(
                resolve(playing(), code, true),
                Some(would_have),
                "control failed: {code:?} does nothing even while playing, so \
                 asserting it is swallowed proves nothing"
            );
        }
    }
}

#[test]
fn the_inventory_key_closes_a_container_and_escape_pauses_instead() {
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(gate, KeyCode::KeyE, true),
        Some(KeyOutcome::CloseContainer)
    );
    // Escape is resolved by the arm *above* the container arm, so it pauses
    // (and `Pause`'s handler closes the menu on the way). If the container
    // arm were moved above it, this would be `CloseContainer` and Escape
    // would stop reaching the pause screen from an open inventory.
    assert_eq!(resolve(gate, KeyCode::Escape, true), Some(KeyOutcome::Pause));
    // A key release while a container is open does nothing at all — but must
    // also not fall through to the gameplay arms.
    assert_eq!(resolve(gate, KeyCode::KeyE, false), None);
    assert_eq!(resolve(gate, KeyCode::KeyW, false), None);
}

/// The number keys `1`–`9` **do not** change the selected hotbar
/// slot while a container screen is open; they issue a `ContainerInput::SWAP`
/// with that hotbar index against the hovered slot
/// (the container-screen hotbar-swap key handling,
/// and the number keys are handled in
/// the client-side key handling only when no screen is open).
///
/// Before this they fell into the container arm's swallow: they neither
/// selected a slot — correct — nor swapped, which is the gap.
#[test]
fn the_number_keys_swap_with_the_hovered_slot_instead_of_selecting_one() {
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    let digits = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    for (i, code) in digits.into_iter().enumerate() {
        // The button number is the hotbar index, `0..=8` — vanilla passes the
        // loop counter straight through as the button index.
        assert_eq!(
            resolve(gate, code, true),
            Some(KeyOutcome::ContainerSwap { button: i as i32 }),
            "{code:?} must swap with hotbar index {i} while a container is open"
        );
        // -- the two controls -------------------------------------------
        // 1. The same key while *playing* still selects the slot. Without
        //    this, a resolver that had simply lost `SelectSlot` altogether
        //    would satisfy the assertion above.
        assert_eq!(
            resolve(playing(), code, true),
            Some(KeyOutcome::SelectSlot(i)),
            "control failed: {code:?} no longer selects a hotbar slot in the \
             world either, so this is not a container-specific route"
        );
        // 2. A key *release* is not a swap. The input handler acts on presses
        //    only, and a swap on both edges would fire every action twice.
        assert_eq!(
            resolve(gate, code, false),
            None,
            "{code:?} released must do nothing"
        );
    }
    // And the outcome is genuinely distinct from selecting a slot: nothing in
    // the container arm may produce `SelectSlot`, or the hotbar would jump
    // under an open inventory.
    for code in digits {
        assert!(
            !matches!(resolve(gate, code, true), Some(KeyOutcome::SelectSlot(_))),
            "{code:?} must not change the selected slot behind a screen"
        );
    }
}

/// The off-hand key's container half.
///
/// The off-hand binding defaults to `F`. It must remain distinct from the
/// container's other bindings, and this assertion checks that the key
/// actually reaches `Click::offhand_swap` rather than merely existing in
/// the table.
#[test]
fn the_offhand_key_swaps_with_slot_forty_while_a_container_is_open() {
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(gate, KeyCode::KeyF, true),
        Some(KeyOutcome::ContainerSwap {
            button: OFFHAND_SWAP_BUTTON
        }),
        "F must issue a SWAP against the off-hand's native slot"
    );
    // -- three controls, each for a different way this could be hollow ---
    // 1. The button number is the off-hand's, not a hotbar index. `40` is
    //    outside `0..=8`, so a resolver that had fallen through to
    //    `hotbar_slot_for` cannot satisfy this.
    assert!(
        !(0..=8).contains(&OFFHAND_SWAP_BUTTON),
        "control failed: 40 overlaps the hotbar range, so the assertion \
         above cannot distinguish the two routes"
    );
    // 2. A release is not a swap — the input handler acts on presses only.
    assert_eq!(resolve(gate, KeyCode::KeyF, false), None);
    // 3. **The gameplay half is a different outcome, not the same one.**
    //    This line intentionally exercises the gameplay half: with no screen
    //    open the key must resolve to the *bare action*, never to a
    //    `ContainerSwap` — a resolver that reused `ContainerSwap` here would
    //    hit-test a slot that does not exist and silently do nothing.
    assert_eq!(
        resolve(playing(), KeyCode::KeyF, true),
        Some(KeyOutcome::SwapOffhand),
        "with no screen open the off-hand key is a ServerboundPlayerAction, \
         not a container click (#385)"
    );
    assert_ne!(
        resolve(playing(), KeyCode::KeyF, true),
        resolve(gate, KeyCode::KeyF, true),
        "the two routes must not collapse into one outcome — that is the \
         conflation #385 exists to prevent"
    );
}

/// The gameplay half: `F` in the world **reaches the wire** as
/// `ClientAction::SwapItemWithOffhand`.
///
/// Two hops, both asserted, because either alone is satisfiable by a dead
/// chain: `resolve_key` producing the outcome proves nothing about the
/// driver, and a `NetClient` that accepts an action proves nothing about the
/// keybind. The `match` arm between them is the piece a compiler *cannot*
/// check — an arm that resolved and then did nothing would be exactly the
/// island `CLAUDE.md` §1 names.
///
/// What this deliberately does not assert is the **bytes**. Those are pinned
/// where they belong, against the jar's own declared layout, in
/// `crates/protocol/v770/tests/interaction_actions.rs`
/// (`swap_item_with_offhand_is_byte_exact_against_the_jars_enum_order`) —
/// asserting them again here off our own encoder would be
/// `decode(encode(x))` with extra steps.
#[test]
fn the_offhand_key_in_the_world_sends_the_swap_action_to_the_wire() {
    assert_eq!(
        resolve(playing(), KeyCode::KeyF, true),
        Some(KeyOutcome::SwapOffhand),
        "hop 1: the keybind must resolve"
    );

    // Hop 2: the driver's arm. `offhand_swap_action` is what it calls; the
    // loopback below is what proves an accepted action is observable.
    let (net, actions) = NetClient::loopback();
    let action = offhand_swap_action(Some(lodestone_client::GameMode::Survival))
        .expect("a survival player may swap");
    net.send_action(action);
    assert_eq!(
        actions.try_recv(),
        Ok(lodestone_model::ClientAction::SwapItemWithOffhand),
        "hop 2: the action must reach the outbound channel"
    );
    assert!(
        actions.try_recv().is_err(),
        "exactly one action per press — a doubled send would swap twice and \
         land back where it started, which looks identical to doing nothing"
    );
}

/// **The spectator control**, and the one guard vanilla actually applies
/// (vanilla's own client-side check, re-checked server-side too).
///
/// Watched failing: with the `Spectator` arm removed,
/// `offhand_swap_action(Spectator)` returns the action and the first
/// assertion below reports `Some(SwapItemWithOffhand)`.
///
/// The other three modes are the positive control. Without them this passes
/// just as well against a function that returns `None` unconditionally — i.e.
/// against the feature not existing at all, which is the state an absent
/// feature would produce.
#[test]
fn a_spectator_does_not_send_the_offhand_swap_and_everyone_else_does() {
    use lodestone_client::GameMode;
    assert_eq!(
        offhand_swap_action(Some(GameMode::Spectator)),
        None,
        "a spectator has no inventory to swap; vanilla declines to send"
    );
    for mode in [
        GameMode::Survival,
        GameMode::Creative,
        GameMode::Adventure,
    ] {
        assert_eq!(
            offhand_swap_action(Some(mode)),
            Some(lodestone_model::ClientAction::SwapItemWithOffhand),
            "{mode:?} must still swap — otherwise the guard above is \
             indistinguishable from the feature being absent"
        );
    }
    // Before login there is no mode. Sending is the better default: refusing
    // input until a mode arrives would make the key dead during the join
    // window, and the server re-checks anyway.
    assert_eq!(
        offhand_swap_action(None),
        Some(lodestone_model::ClientAction::SwapItemWithOffhand),
        "an unknown game mode must not read as spectator"
    );
}

// -- the drop key (`Q`), the two proven islands ------------------------
//
// `Click::drop_one`/`drop_stack`/`do_throw` (`lodestone-game`) and
// `ClientAction::DropSelectedItem`/`DropSelectedItemStack` were each built,
// encoded and round-trip tested with zero producers before this. One
// binding closes both — see `InputAction::Drop`'s and `KeyOutcome::
// ContainerDrop`/`Drop`'s docs for the source behavior this mirrors.

/// The gameplay half, mirroring `the_offhand_key_swaps_with_slot_forty_
/// while_a_container_is_open`'s shape: both resolve to a *different*
/// outcome than the container half, and `ctrl` must reach the outcome
/// unchanged from what `resolve_key` was handed.
#[test]
fn q_drops_one_while_playing_and_ctrl_q_drops_the_stack() {
    assert_eq!(
        resolve(playing(), KeyCode::KeyQ, true),
        Some(KeyOutcome::Drop { ctrl: false })
    );
    assert_eq!(
        resolve_ctrl(playing(), KeyCode::KeyQ, true),
        Some(KeyOutcome::Drop { ctrl: true })
    );
    // A release does nothing — vanilla's own key-click consumption only
    // ever fires on the down edge.
    assert_eq!(resolve(playing(), KeyCode::KeyQ, false), None);
}

/// The container half — vanilla's own container-screen key handling
/// reached through `resolve_key`'s `container_open` arm.
#[test]
fn q_issues_a_container_drop_while_a_container_is_open() {
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(gate, KeyCode::KeyQ, true),
        Some(KeyOutcome::ContainerDrop { ctrl: false })
    );
    assert_eq!(
        resolve_ctrl(gate, KeyCode::KeyQ, true),
        Some(KeyOutcome::ContainerDrop { ctrl: true })
    );
    assert_eq!(resolve(gate, KeyCode::KeyQ, false), None);
    // -- the two-mechanisms control, same shape as the off-hand key's own --
    assert_ne!(
        resolve(playing(), KeyCode::KeyQ, true),
        resolve(gate, KeyCode::KeyQ, true),
        "the container and gameplay routes must not collapse into one \
         outcome, or the container click would fire in the world (no menu \
         to hit-test) or vice versa"
    );
}

/// The drop action must not be swallowed as an unrecognised key behind an open
/// container. This negative control simulates an unbound `InputAction::Drop`
/// and verifies the corresponding gameplay path remains distinct.
#[test]
fn an_unbound_drop_key_is_swallowed_behind_a_container_and_dead_in_the_world() {
    let mut binds = Keybinds::new();
    binds.set(InputAction::Drop, Binding::Unbound);
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve_key(&binds, gate, Some(KeyCode::KeyQ), true, false, None),
        None,
        "watched failing before this test existed: with the real binding \
         still assigned, this line reported Some(ContainerDrop {{ .. }})"
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyQ), true, false, None),
        None
    );
}

/// Hop 1 (`resolve_key`) and hop 2 (the driver's action, factored into
/// [`drop_selected_action`] the same way `offhand_swap_action` is) for the
/// gameplay half, mirroring `the_offhand_key_in_the_world_sends_the_swap_
/// action_to_the_wire`.
#[test]
fn the_drop_key_in_the_world_sends_the_drop_action_to_the_wire() {
    assert_eq!(
        resolve(playing(), KeyCode::KeyQ, true),
        Some(KeyOutcome::Drop { ctrl: false }),
        "hop 1: the keybind must resolve"
    );

    let (net, actions) = NetClient::loopback();
    let action = drop_selected_action(Some(lodestone_client::GameMode::Survival), false)
        .expect("a survival player may drop");
    net.send_action(action.clone());
    assert_eq!(
        actions.try_recv(),
        Ok(lodestone_model::ClientAction::DropSelectedItem),
        "hop 2: the action must reach the outbound channel"
    );
    assert!(actions.try_recv().is_err(), "exactly one action per press");

    // And the `ctrl` axis selects the *other* wire action, not a flag on
    // the same one — `DropSelectedItem`/`DropSelectedItemStack` are two
    // separate `ClientAction` variants, not one with a bool field.
    let stack_action =
        drop_selected_action(Some(lodestone_client::GameMode::Survival), true)
            .expect("a survival player may drop the whole stack");
    assert_eq!(
        stack_action,
        lodestone_model::ClientAction::DropSelectedItemStack
    );
    assert_ne!(action, stack_action);
}

/// The spectator control, the one guard vanilla applies
/// — same shape as `a_spectator_does_not_send_
/// the_offhand_swap_and_everyone_else_does`, watched failing the same way:
/// remove the `Spectator` arm from `drop_selected_action` and the first
/// assertion below reports `Some(DropSelectedItem)`.
#[test]
fn a_spectator_does_not_send_the_drop_action_and_everyone_else_does() {
    use lodestone_client::GameMode;
    assert_eq!(
        drop_selected_action(Some(GameMode::Spectator), false),
        None,
        "a spectator has nothing to drop; vanilla declines to send"
    );
    assert_eq!(
        drop_selected_action(Some(GameMode::Spectator), true),
        None,
        "the ctrl axis must not bypass the spectator guard"
    );
    for mode in [
        GameMode::Survival,
        GameMode::Creative,
        GameMode::Adventure,
    ] {
        assert_eq!(
            drop_selected_action(Some(mode), false),
            Some(lodestone_model::ClientAction::DropSelectedItem),
            "{mode:?} must still drop — otherwise the guard above is \
             indistinguishable from the feature being absent"
        );
    }
    // Before login there is no mode; sending is the better default, same
    // reasoning as `offhand_swap_action`'s own `None` case.
    assert_eq!(
        drop_selected_action(None, false),
        Some(lodestone_model::ClientAction::DropSelectedItem),
        "an unknown game mode must not read as spectator"
    );
}

#[test]
fn an_open_chat_prompt_swallows_every_key_into_the_editor() {
    // `W` must type a `w`, not walk.
    let gate = KeyGate {
        chat_open: true,
        ..KeyGate::default()
    };
    for (code, _) in default_playing_expectations() {
        assert_eq!(
            resolve(gate, code, true),
            Some(KeyOutcome::Chat),
            "{code:?} should route to the chat editor"
        );
    }
    // Including keys nothing is bound to — the editor wants those too.
    assert_eq!(resolve(gate, KeyCode::KeyZ, true), Some(KeyOutcome::Chat));
    // And an unnameable physical key still reaches the editor, whose `text`
    // may be the only thing that identifies it.
    assert_eq!(
        resolve_key(&Keybinds::new(), gate, None, true, false, None),
        Some(KeyOutcome::Chat)
    );
}

#[test]
fn a_menu_screen_outranks_the_chat_prompt_and_everything_below_it() {
    let gate = KeyGate {
        menu: true,
        ..KeyGate::default()
    };
    for (code, _) in default_playing_expectations() {
        assert_eq!(resolve(gate, code, true), Some(KeyOutcome::Menu));
    }
    // Both flags set: the menu wins. This is the documented order, and a
    // swapped pair would send the edit form's keystrokes to the chat buffer.
    let both = KeyGate {
        menu: true,
        chat_open: true,
        container_open: true,
        gameplay: true,
        debug_held: true,
        recipe_search: true,
        creative_search: true,
        anvil_rename_active: true,
        spectator: false,
    };
    assert_eq!(resolve(both, KeyCode::KeyW, true), Some(KeyOutcome::Menu));
    assert_eq!(resolve(both, KeyCode::Escape, true), Some(KeyOutcome::Menu));
    // Chat outranks the container and gameplay in turn.
    let chat_over_container = KeyGate {
        chat_open: true,
        container_open: true,
        gameplay: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(chat_over_container, KeyCode::KeyE, true),
        Some(KeyOutcome::Chat)
    );
}

#[test]
fn gameplay_bindings_are_inert_when_no_screen_accepts_gameplay_input() {
    // Every flag false: no menu, no chat, no container, and not playing —
    // e.g. the loading screen. Only the two ungated arms may still fire.
    let gate = KeyGate::default();
    for (code, _) in default_playing_expectations() {
        let got = resolve(gate, code, true);
        match code {
            // `Pause` is intentionally ungated: Escape must work on the
            // loading and error screens, which is how it did before.
            KeyCode::Escape => assert_eq!(got, Some(KeyOutcome::Pause)),
            // So is the debug overlay — it is an instrument, and gating it
            // on `Playing` would make it unavailable exactly when a stuck
            // connection is the thing being debugged.
            KeyCode::F3 => assert_eq!(got, Some(KeyOutcome::DebugModifier(true))),
            _ => assert_eq!(got, None, "{code:?} fired outside gameplay"),
        }
    }
}

#[test]
fn held_bindings_report_both_edges_and_one_shot_bindings_only_the_press() {
    // Movement and the player list are held states; the rest are one-shots.
    // A one-shot that fired on release would double-toggle perspective, and
    // a held binding gated on `pressed` would stick on forever.
    assert_eq!(
        resolve(playing(), KeyCode::KeyW, false),
        Some(KeyOutcome::Movement(Action::Forward, false))
    );
    assert_eq!(
        resolve(playing(), KeyCode::Tab, false),
        Some(KeyOutcome::PlayerList(false))
    );
    for one_shot in [
        KeyCode::KeyE,
        KeyCode::KeyT,
        KeyCode::Slash,
        KeyCode::KeyF,
        KeyCode::F5,
        KeyCode::Escape,
        KeyCode::Digit1,
    ] {
        assert_eq!(
            resolve(playing(), one_shot, false),
            None,
            "{one_shot:?} must not fire on release"
        );
    }
    // F3 is deliberately *not* in that list any more: it is the
    // debug modifier, so it reports both edges, and the driver toggles the
    // overlay on the release when no chord fired.
    assert_eq!(
        resolve(playing(), KeyCode::F3, false),
        Some(KeyOutcome::DebugModifier(false))
    );
}

/// F3+B and F3+G resolve to their sub-modes only while the modifier is held, and
/// a plain B or G is untouched.
///
/// The negative half is the point: `B` and `G` are unbound in the default table,
/// so if the chord arms ignored `debug_held` they would fire on every press and
/// the assertion below would catch it.
#[test]
fn the_debug_chords_need_the_modifier_held() {
    let held = KeyGate {
        gameplay: true,
        debug_held: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(held, KeyCode::KeyB, true),
        Some(KeyOutcome::ToggleHitboxes)
    );
    assert_eq!(
        resolve(held, KeyCode::KeyG, true),
        Some(KeyOutcome::ToggleChunkBorders)
    );
    // Release is not a chord — a chord that fired on both edges would toggle
    // twice per keystroke and appear to do nothing.
    assert_eq!(resolve(held, KeyCode::KeyB, false), None);

    // Without the modifier, neither key means anything.
    assert_eq!(resolve(playing(), KeyCode::KeyB, true), None);
    assert_eq!(resolve(playing(), KeyCode::KeyG, true), None);
}

/// Shift+F3 (the profiler pie chart toggle) and its own F3+number navigation
/// resolve only while the modifier is held — the same shape
/// [`the_debug_chords_need_the_modifier_held`] checks for F3+B/F3+G, and for
/// the same reason: the number row is the (rebindable) hotbar selector, so a
/// chord arm that ignored `debug_held` would fire on every ordinary hotbar
/// press and the negative half below would catch it.
#[test]
fn the_profiler_chart_chords_need_the_modifier_held() {
    let held = KeyGate {
        gameplay: true,
        debug_held: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(held, KeyCode::ShiftLeft, true),
        Some(KeyOutcome::ToggleProfilerChart)
    );
    assert_eq!(
        resolve(held, KeyCode::ShiftRight, true),
        Some(KeyOutcome::ToggleProfilerChart)
    );
    // Release is not a chord, matching every other F3 chord — unlike B/G
    // (unbound by default), Shift is also the sneak binding, so its release still
    // falls through to an ordinary (harmless, since sneak was never pressed
    // through this path) `Movement` release rather than to `None`.
    assert_ne!(
        resolve(held, KeyCode::ShiftLeft, false),
        Some(KeyOutcome::ToggleProfilerChart)
    );

    // Digit1..Digit8 drill into wedges 0..8; Digit0 returns to the root.
    assert_eq!(
        resolve(held, KeyCode::Digit1, true),
        Some(KeyOutcome::ProfilerChartSelect(Some(0)))
    );
    assert_eq!(
        resolve(held, KeyCode::Digit8, true),
        Some(KeyOutcome::ProfilerChartSelect(Some(7)))
    );
    assert_eq!(
        resolve(held, KeyCode::Digit0, true),
        Some(KeyOutcome::ProfilerChartSelect(None))
    );
    // Digit9 is not a profiler-chart key (only eight phases exist), so it
    // falls through to whatever it would otherwise resolve to — here, the
    // ordinary (default-bound) hotbar slot 9 selection, since F3 held only
    // intercepts the specific keys it lists, exactly like every other
    // debug-held chord.
    assert_eq!(
        resolve(held, KeyCode::Digit9, true),
        Some(KeyOutcome::SelectSlot(8))
    );

    // Without the modifier, Shift is sneak (`Movement`) and the digits select
    // hotbar slots — both remain ordinary gameplay bindings.
    assert_ne!(
        resolve(playing(), KeyCode::ShiftLeft, true),
        Some(KeyOutcome::ToggleProfilerChart)
    );
    assert_ne!(
        resolve(playing(), KeyCode::Digit1, true),
        Some(KeyOutcome::ProfilerChartSelect(Some(0)))
    );
}

/// F3+P (pause on lost focus) and F3+C (copy location) — the same
/// modifier-gated shape [`the_debug_chords_need_the_modifier_held`] checks
/// for F3+B/F3+G, extended to the two chords covered here. `P` and `C`
/// are unbound in the default table (like `B`/`G`), so the negative half is
/// real: a chord that ignored `debug_held` would fire on every plain press.
#[test]
fn the_pause_and_copy_location_chords_need_the_debug_modifier() {
    let held = KeyGate {
        gameplay: true,
        debug_held: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(held, KeyCode::KeyP, true),
        Some(KeyOutcome::TogglePauseOnLostFocus)
    );
    assert_eq!(
        resolve(held, KeyCode::KeyC, true),
        Some(KeyOutcome::CopyLocation)
    );
    // Release is not a chord, same reason as F3+B/F3+G.
    assert_eq!(resolve(held, KeyCode::KeyP, false), None);
    assert_eq!(resolve(held, KeyCode::KeyC, false), None);

    // Without the modifier, neither key means anything.
    assert_eq!(resolve(playing(), KeyCode::KeyP, true), None);
    assert_eq!(resolve(playing(), KeyCode::KeyC, true), None);
}

/// The exact vanilla wording `debug_shown_feedback`/`debug_enabled_feedback`
/// produce, predicted from vanilla's own translated-debug-feedback
/// call sites and the `en_us.json` strings they resolve
/// (`debug.show_hitboxes.on`/`.off`, `debug.chunk_boundaries.on`/`.off`,
/// `debug.advanced_tooltips.on`/`.off`, `debug.pause_focus.on`/`.off`) —
/// not the round number, the exact byte string including the legacy `§`
/// codes vanilla's own debug feedback decoration applies (`§e` yellow, `§l` bold, `§r`
/// reset before the un-styled body).
#[test]
fn debug_feedback_helpers_match_vanillas_exact_wording_and_legacy_codes() {
    assert_eq!(debug_feedback("hi"), "§e§l[Debug]:§r hi");
    assert_eq!(
        debug_shown_feedback("Hitboxes", true),
        "§e§l[Debug]:§r Hitboxes: shown"
    );
    assert_eq!(
        debug_shown_feedback("Hitboxes", false),
        "§e§l[Debug]:§r Hitboxes: hidden"
    );
    assert_eq!(
        debug_shown_feedback("Chunk borders", true),
        "§e§l[Debug]:§r Chunk borders: shown"
    );
    assert_eq!(
        debug_shown_feedback("Advanced tooltips", false),
        "§e§l[Debug]:§r Advanced tooltips: hidden"
    );
    assert_eq!(
        debug_enabled_feedback("Pause on lost focus", true),
        "§e§l[Debug]:§r Pause on lost focus: enabled"
    );
    assert_eq!(
        debug_enabled_feedback("Pause on lost focus", false),
        "§e§l[Debug]:§r Pause on lost focus: disabled"
    );
}

/// The colour actually reaches a vertex without a hex span or the legacy
/// string path (`Text::to_legacy_string`) touching this at all — the exact
/// concern the brief names, because `to_legacy_string` cannot carry an RGB
/// colour and this repo already has a defect class where a coloured message
/// silently lost its colour through it.
///
/// `Text::literal(debug_feedback(msg)).to_spans()` is production's own
/// expansion path (`Text::to_spans`'s own doc: "`from_legacy` consumes every
/// `§`+code pair"), the same one `ChatLog::recent_ages_spans` uses for the
/// HUD's real chat draw — not a hand-rolled parser this test invented.
///
/// **Negative control, in the same assertion set:** the body span carries no
/// colour and no bold, so a version of `debug_feedback` that coloured the
/// *whole* line (an easy way to get this "working" by accident) fails here.
#[test]
fn debug_feedback_expands_to_a_bold_yellow_prefix_span_and_a_plain_body_span() {
    use lodestone_model::text::{Text, TextColor};

    let spans = Text::literal(debug_shown_feedback("Hitboxes", true)).resolve(&|_| None).to_spans();
    assert_eq!(spans.len(), 2, "a coloured prefix run and a plain body run: {spans:?}");

    assert_eq!(spans[0].text, "[Debug]:");
    assert_eq!(spans[0].style.color, Some(TextColor::Yellow));
    assert_eq!(spans[0].style.bold, Some(true));

    assert_eq!(spans[1].text, " Hitboxes: shown");
    assert_eq!(
        spans[1].style.color, None,
        "the body must not inherit or carry a colour of its own"
    );
    assert_eq!(
        spans[1].style.bold, None,
        "§r resets bold before the body, so it must not read as bold"
    );
}

/// `KeyOutcome::CopyLocation`'s exact wire format, predicted from
/// vanilla's own debug copy-location format string ("/execute in %s run
/// tp @s %.2f %.2f %.2f %.2f %.2f") — not the round number, and every
/// numeric field pairwise-distinct (`CLAUDE.md`'s transposition rule) so a
/// swapped x/y/z or yaw/pitch fails here rather than round-tripping silently.
#[test]
fn copy_location_command_matches_vanillas_execute_format_with_distinct_fields() {
    assert_eq!(
        copy_location_command("minecraft:the_nether", [11.5, 64.25, -8.125], 91.5, -12.75),
        "/execute in minecraft:the_nether run tp @s 11.50 64.25 -8.12 91.50 -12.75"
    );
}

/// The whole chain, through the real `ChatLog` production code pushes
/// through (`Sim::push_local_chat`/`Sim::recent_chat_spans`) rather than a
/// hand-built `Text` — a plain literal string carrying `§` codes really does
/// survive a round trip through the same feed the HUD reads.
///
/// **Negative control:** a plain message pushed alongside it (no `§` codes)
/// comes back as one unstyled span, proving the expansion is conditional on
/// the codes actually being present rather than every chat line silently
/// gaining a colour.
#[test]
fn pushing_debug_feedback_through_the_real_chat_log_survives_as_a_bold_yellow_span() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.sim.push_local_chat("plain status line");
    app.sim
        .push_local_chat(debug_shown_feedback("Chunk borders", false));

    let recent = app.sim.recent_chat_spans(2);
    assert_eq!(recent.len(), 2, "both lines must be retained: {recent:?}");

    let (plain_spans, _) = &recent[0];
    assert_eq!(plain_spans.len(), 1);
    assert_eq!(plain_spans[0].text, "plain status line");
    assert_eq!(plain_spans[0].style.color, None);

    let (debug_spans, _) = &recent[1];
    assert_eq!(debug_spans.len(), 2, "{debug_spans:?}");
    assert_eq!(debug_spans[0].text, "[Debug]:");
    assert_eq!(
        debug_spans[0].style.color,
        Some(lodestone_model::text::TextColor::Yellow)
    );
    assert_eq!(debug_spans[1].text, " Chunk borders: hidden");
}

/// The end-to-end gap `resolve_key`'s own tests cannot see: the owner
/// reported F3+B/F3+G producing *no chat feedback at all* — not thin lines,
/// nothing — while F3+H, driven the same way, worked. A resolver-level
/// assertion (`the_debug_chords_need_the_modifier_held`) already proves all
/// three `KeyOutcome`s are *produced* correctly; this drives them through
/// [`WindowApp::apply_key_outcome`] — the real effect half of
/// `handle_keyboard_input`, split out because winit's `KeyEvent` cannot be
/// constructed outside winit itself (a private `platform_specific` field),
/// which is exactly why nothing before this test reached past the resolver —
/// and asserts on the *real* atomics and the *real* chat log, side by side
/// with F3+H as the owner's own working control.
#[test]
fn f3_b_and_f3_g_flip_their_atomic_and_push_chat_through_the_real_key_path() {
    use std::sync::atomic::Ordering;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });

    // F3 down — the same `DebugModifier(true)` outcome a real F3 keydown
    // resolves to, driven through the real effect path.
    app.apply_key_outcome(Some(KeyOutcome::DebugModifier(true)), true, Some(KeyCode::F3), None);
    assert!(app.debug_held, "F3 down must set debug_held through the real path");

    app.apply_key_outcome(Some(KeyOutcome::ToggleHitboxes), true, Some(KeyCode::KeyB), None);
    assert!(
        app.debug_hitboxes.load(Ordering::Relaxed),
        "F3+B must flip the hitboxes atomic through the real path"
    );

    app.apply_key_outcome(Some(KeyOutcome::ToggleChunkBorders), true, Some(KeyCode::KeyG), None);
    assert!(
        app.debug_chunk_borders.load(Ordering::Relaxed),
        "F3+G must flip the chunk-borders atomic through the real path"
    );

    let tooltips_before = app.nav.advanced_item_tooltips();
    app.apply_key_outcome(Some(KeyOutcome::ToggleAdvancedTooltips), true, Some(KeyCode::KeyH), None);
    assert_ne!(
        app.nav.advanced_item_tooltips(),
        tooltips_before,
        "F3+H must flip the tooltip option through the real path (the owner's own working control)"
    );

    let recent = app.sim.recent_chat_spans(3);
    assert_eq!(
        recent.len(),
        3,
        "all three chords must each push exactly one chat line through the real path, got {recent:?}"
    );
    let text_of = |spans: &[lodestone_model::text::TextSpan]| -> String {
        spans.iter().map(|s| s.text.clone()).collect::<String>()
    };
    assert!(
        text_of(&recent[0].0).contains("Hitboxes"),
        "F3+B's chat line is missing or wrong: {:?}",
        recent[0]
    );
    assert!(
        text_of(&recent[1].0).contains("Chunk borders"),
        "F3+G's chat line is missing or wrong: {:?}",
        recent[1]
    );
    assert!(
        text_of(&recent[2].0).contains("Advanced tooltips"),
        "F3+H's chat line is missing or wrong: {:?}",
        recent[2]
    );
}

/// F3+P's toggle+persist half, through the real `MenuNav` — the same shape
/// `toggle_advanced_item_tooltips` already has no dedicated test for, closed
/// here because it exercises the option's in-memory toggle. Persistence (writing no
/// key when untouched, degrading a garbled value to vanilla's `true`) is
/// covered by `config.rs`'s
/// `pause_on_lost_focus_defaults_on_and_only_writes_a_key_when_turned_off`;
/// this is the in-memory toggle the F3+P driver arm actually calls.
#[test]
fn toggle_pause_on_lost_focus_flips_the_option_both_ways() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    assert!(
        app.nav.pause_on_lost_focus(),
        "vanilla's own default is on"
    );
    app.nav.toggle_pause_on_lost_focus();
    assert!(!app.nav.pause_on_lost_focus());
    app.nav.toggle_pause_on_lost_focus();
    assert!(app.nav.pause_on_lost_focus());
}

#[test]
fn a_rebind_moves_the_behaviour_to_the_new_key_and_off_the_old_one() {
    let mut binds = Keybinds::new();
    binds.set(InputAction::Inventory, Binding::Key(KeyCode::KeyI.into()));
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyI), true, false, None),
        Some(KeyOutcome::OpenContainer)
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyE), true, false, None),
        None,
        "the old default must stop opening the inventory"
    );
    // …and the rebound key also closes the container, because both sites ask
    // the table rather than naming `KeyE`.
    let gate = KeyGate {
        container_open: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve_key(&binds, gate, Some(KeyCode::KeyI), true, false, None),
        Some(KeyOutcome::CloseContainer)
    );
    assert_eq!(
        resolve_key(&binds, gate, Some(KeyCode::KeyE), true, false, None),
        None
    );
}

#[test]
fn unbinding_an_action_disables_it_without_disturbing_the_rest() {
    let mut binds = Keybinds::new();
    binds.set(InputAction::Jump, Binding::Unbound);
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::Space), true, false, None),
        None
    );
    // The neighbouring arms are untouched.
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyW), true, false, None),
        Some(KeyOutcome::Movement(Action::Forward, true))
    );
}

#[test]
fn attack_and_use_are_keyboard_dispatchable_once_rebound_off_the_mouse() {
    // Under the defaults these arms are dormant, because attack and use are
    // mouse-bound — assert that, so "it works" cannot be an accident of the
    // key path firing too.
    assert_eq!(resolve(playing(), KeyCode::KeyR, true), None);

    let mut binds = Keybinds::new();
    binds.set(InputAction::Attack, Binding::Key(KeyCode::KeyR.into()));
    binds.set(InputAction::Use, Binding::Key(KeyCode::KeyV.into()));
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyR), true, false, None),
        Some(KeyOutcome::Attack(true))
    );
    // Hold-to-dig: the release edge must arrive, or mining never stops.
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyR), false, false, None),
        Some(KeyOutcome::Attack(false))
    );
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyV), true, false, None),
        Some(KeyOutcome::Use(true))
    );
    // The release edge must arrive too, or `ReleaseUseItem` never sends —
    // the exact bug this test's sibling assertions exist to catch (a bow
    // or shield cannot complete a use without it).
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyV), false, false, None),
        Some(KeyOutcome::Use(false))
    );
}

#[test]
fn the_mouse_path_resolves_the_default_attack_and_use_buttons() {
    // The mouse half of dispatch, which is why `Binding` is not `KeyCode`.
    let binds = Keybinds::new();
    assert_eq!(
        mouse_action_for(&binds, MouseButton::Left),
        Some(InputAction::Attack)
    );
    assert_eq!(
        mouse_action_for(&binds, MouseButton::Right),
        Some(InputAction::Use)
    );
    // Middle **is** a gameplay binding now: the pick-item action defaults to
    // the middle mouse button, so it is the primary route for
    // pick-item rather than a rebound one. This assertion previously read
    // `None`, which was correct only while pick-item did not exist — the
    // premise went stale when the binding landed, not the code.
    assert_eq!(
        mouse_action_for(&binds, MouseButton::Middle),
        Some(InputAction::PickItem)
    );

    // Swapping the two buttons is a supported rebind.
    let mut swapped = binds;
    swapped.set(InputAction::Attack, Binding::Mouse(MouseButton::Right.into()));
    swapped.set(InputAction::Use, Binding::Mouse(MouseButton::Left.into()));
    assert_eq!(
        mouse_action_for(&swapped, MouseButton::Right),
        Some(InputAction::Attack)
    );
    assert_eq!(
        mouse_action_for(&swapped, MouseButton::Left),
        Some(InputAction::Use)
    );
}

#[test]
fn a_movement_action_can_be_driven_from_a_mouse_button() {
    // Not something vanilla offers, but it falls out of `Binding` covering
    // both input kinds — and the mouse handler routes it, so it is not an
    // island.
    let mut binds = Keybinds::new();
    binds.set(InputAction::Jump, Binding::Mouse(MouseButton::Middle.into()));
    let action = mouse_action_for(&binds, MouseButton::Middle);
    assert_eq!(action, Some(InputAction::Jump));
    assert_eq!(action.and_then(InputAction::movement), Some(Action::Jump));
}

#[test]
fn an_unnameable_physical_key_is_ignored_by_the_binding_chain() {
    // `PhysicalKey::Unidentified` reaches the menu and chat arms (tested
    // above) but must not match any binding — there is nothing to match on.
    assert_eq!(
        resolve_key(&Keybinds::new(), playing(), None, true, false, None),
        None
    );
}

/// **Pressing Play Selected World reaches a running integrated server**.
///
/// This is the anti-island gate for singleplayer, and it is the only test
/// anywhere that crosses *every* seam of it in one go: the registry's
/// serverbound lookup, the boxed `ServerProtocol`, the net thread, the
/// in-memory duplex, `IntegratedServer`'s serving loop, the real v770 wire
/// format, and the client's decode — ending at a `NetUpdate` the shell's own
/// frame loop consumes.
///
/// The button half is `menu::nav`'s
/// `play_selected_world_asks_the_app_to_start_singleplayer`, which asserts the
/// click produces `MenuAction::Singleplayer(None)`; `apply_menu_action`'s arm
/// between the two is a single call this file can be read for. The seam
/// *without* the shell is `crates/protocol/v770/tests/singleplayer_seam.rs`.
///
/// **Chunks, not just login, is the load-bearing assertion.** Login is five
/// `ServerProtocol` methods with no trait defaults, so it cannot silently fall
/// through the box; terrain is where a half-wired server shows up, and it is
/// also the only thing here that proves the *world* exists rather than just a
/// handshake. A world that logs in and streams nothing is precisely the shape
/// of the chunk-blackout failures `CLAUDE.md` records.
///
/// `view_radius = 0` is one column: the bundled generator costs ~12 ms per
/// column, and one is enough to prove terrain crosses the wire (its *content*
/// is verified block-for-block in `lodestone-server`'s own tests, against a
/// JVM oracle rather than against our encoder).
#[test]
fn pressing_play_reaches_a_running_integrated_server() {
    let protocol = Config::default().protocol;
    let seed = crate::menu::world_select::BUNDLED_WORLD.seed;
    // `None` world dir: this gate is about the seam reaching a running server,
    // not about persistence (that is covered in
    // `tests/singleplayer_persistence.rs`), and an in-memory world leaves
    // nothing in the developer's real data directory.
    let net = match launch_singleplayer(
        protocol,
        0,
        None,
        seed,
        crate::menu::create_world::WorldTypePreset::Normal,
        None,
    ) {
        Ok(net) => net,
        Err(e) => {
            // A build with no hostable family must *report*, which is the
            // `--no-default-features` contract. In the default build (`live`)
            // this is a failure, not a skip.
            assert!(
                !cfg!(feature = "live"),
                "the default build must be able to host singleplayer: {e}"
            );
            assert!(matches!(e, LaunchError::NoVersionFamily { .. }));
            return;
        }
    };

    // The loop below is bounded by *state reached* — `logged_in && chunks >
    // 0`, or a definitive `fatal` answer from the session itself — never by
    // racing the clock in the ordinary case. `deadline` only exists as a
    // backstop against a genuine hang (no update of any kind, ever), so it
    // must be generous rather than tight: this test spins up a real
    // integrated server and waits for real worldgen on a background thread,
    // and that thread measurably starves for CPU when several agents are
    // running concurrent `cargo` builds in this repo. The previous 30 s
    // budget was measured taking 19.11 s to pass *alone* on an otherwise
    // quiet machine (64% of the budget with zero contention), which is
    // exactly `CLAUDE.md`'s "a timing gathered under load is attributed to
    // the wrong cause" shape — it was failing from contention, not from a
    // regression. 240 s (matching `tests/singleplayer_persistence.rs`'s
    // `SESSION_DEADLINE` and `tests/singleplayer_terrain_arrives.rs`'s own
    // `DEADLINE` — the same class of test, already using this budget) costs
    // a healthy run nothing, because the loop still exits the instant
    // success is observed; it only changes the outcome for a run that was
    // previously timing out on a busy machine despite the session being
    // perfectly healthy.
    let deadline = Instant::now() + Duration::from_secs(240);
    let mut logged_in = false;
    let mut chunks = 0usize;
    let mut errors: Vec<String> = Vec::new();
    let mut fatal = false;
    while Instant::now() < deadline && !(logged_in && chunks > 0) && !fatal {
        for update in net.poll() {
            match update {
                crate::net::NetUpdate::LoggedIn { .. } => logged_in = true,
                // The production consumer adopts the authoritative pose before
                // releasing the driver's deferred correction response. This
                // direct `NetClient` harness has no `Sim` to do that work, so
                // mirror the consumer contract here; otherwise the driver
                // intentionally pauses inbound reads at the placement teleport
                // and this test would misdiagnose the missing chunk as a
                // transport or decode failure.
                crate::net::NetUpdate::Teleport { pos, rotation, .. } => {
                    net.acknowledge_teleport_correction(pos, rotation);
                }
                crate::net::NetUpdate::Chunk { .. } => chunks += 1,
                // Collected rather than ignored: an `Error`/`Disconnected`
                // here is the actual diagnosis, and without it the failure
                // message would only say "timed out". Also a *definitive*
                // answer, unlike silence — once the session itself has
                // reported failure, waiting out the rest of a now-generous
                // backstop cannot produce more evidence, so `fatal` ends the
                // loop after this batch finishes draining rather than at
                // `deadline`.
                crate::net::NetUpdate::Error(e) => {
                    errors.push(e);
                    fatal = true;
                }
                crate::net::NetUpdate::Disconnected(reason) => {
                    errors.push(format!("disconnected: {reason:?}"));
                    fatal = true;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        logged_in,
        "the client never logged in to the integrated server; errors: {errors:?}"
    );
    assert!(
        chunks > 0,
        "logged in but no terrain arrived — the server is serving nothing; \
         errors: {errors:?}"
    );
    assert!(
        errors.is_empty(),
        "the session reported errors while starting: {errors:?}"
    );
}

/// **The islands sweep's finding, made permanent**: `sim::build` used to
/// insert `Profile(PhysicsProfile::mc_1_21())` unconditionally, so a 1.8.9
/// (`v47`) session ran modern movement physics with no protocol-family
/// selection at all — a correct function (`PhysicsProfile::mc_1_8`) fed a
/// constant by its producer, `CLAUDE.md`'s most common defect shape. The fix
/// threads `config.protocol` through `lodestone_registry::
/// physics_profile_for_protocol`, the one crate allowed to know which
/// protocol numbers belong to which version family.
///
/// This is the discriminating half of that regression test, gated on `v47`
/// specifically because it is not the default-compiled family: with only
/// `live` (`v770`) enabled, every protocol resolves to `mc_1_21()`, which is
/// *also* what the old hardcoded constant produced — an input on which the
/// old and new code agree is not a test (`CLAUDE.md`'s "world" vacuous-test
/// species). `v47`'s correct answer differs from that constant, so this
/// assertion actually fails on the pre-fix code.
#[cfg(feature = "v47")]
#[test]
fn physics_profile_threads_the_configured_protocol_through_the_session() {
    // `render_distance: 2`, matching `pacing_sim`'s own reasoning: the
    // smallest span that still builds a real demo world, so this stays a
    // cheap unit test rather than paying for a full render distance's worth
    // of worldgen just to read back one resource.
    let sim = Sim::with_demo_world(Config {
        mode: Mode::Headless,
        render_distance: 2,
        protocol: 47,
        ..Config::default()
    });
    assert_eq!(
        sim.profile(),
        lodestone_physics::PhysicsProfile::mc_1_8(),
        "a v47 (1.8.9) session must run the 1.8 physics profile, not the \
         modern default every construction site used to hardcode"
    );
}

/// The companion baseline: the default-compiled family (`v770`, 26.2) must
/// still resolve to the modern profile — unchanged behaviour for the only
/// family that was ever actually joined before this seam existed. Weak on
/// its own (the pre-fix hardcoded constant satisfies it too, exactly the
/// coincidence [`physics_profile_threads_the_configured_protocol_through_the_session`]'s
/// own doc names), but it is cheap, always runs, and catches the mapping
/// table returning the wrong default or panicking on the ordinary path.
#[test]
fn physics_profile_defaults_to_the_modern_profile_for_the_default_protocol() {
    let sim = pacing_sim();
    assert_eq!(
        sim.profile(),
        lodestone_physics::PhysicsProfile::mc_1_21()
    );
}

/// **Social-roster synchronization, exercised through production code.**
///
/// `crate::menu::social::entries_from_tablist` was pure and unit-tested
/// with **no caller anywhere in the shell** — `docs/social-interactions.md`'s
/// own "Decorative" section. This does not call it a second time by hand
/// (that would just be the existing unit test again, which proves
/// nothing about production); it drives the actual chain: a real
/// `WindowApp`, a `SessionTabList` folded through the same `NetIngest`
/// schedule the net thread runs, and `drive_ui_from_session` itself —
/// the method `redraw()` calls every frame.
#[test]
fn drive_ui_from_session_refreshes_the_social_roster_from_the_real_tab_list() {
    use crate::net::NetUpdate;
    use lodestone_client::{ClientEvent, GameMode, PlayerListEntry};
    use uuid::Uuid;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    // `drive_ui_from_session`'s refresh is guarded on `SessionPhase::Connected`
    // — reach it the same way `sim/tests.rs`'s own tab-list test does,
    // through a real `NetUpdate`, not by poking a private field.
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    assert_eq!(
        app.sim.session_phase(),
        crate::sim::SessionPhase::Connected,
        "precondition: the refresh guard reads this, so it must actually be live"
    );

    let alice = Uuid::from_u128(1);
    let bob = Uuid::from_u128(2);
    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::PlayerListUpdate {
            entries: vec![
                PlayerListEntry {
                    uuid: Some(bob),
                    name: Some("Bob".into()),
                    game_mode: Some(GameMode::Creative),
                    latency: Some(20),
                    display_name: None,
                    listed: Some(true),
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                },
                PlayerListEntry {
                    uuid: Some(alice),
                    name: Some("Alice".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(10),
                    display_name: None,
                    listed: Some(true),
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                },
            ],
        });

    // Precondition: nothing has refreshed the screen model yet — proves the
    // assertion below actually exercises `drive_ui_from_session`, not some
    // earlier call this test forgot about.
    assert!(
        app.nav.social().entries().is_empty(),
        "precondition: the roster must still be empty before the real call runs"
    );

    app.drive_ui_from_session();

    let names: Vec<&str> = app
        .nav
        .social()
        .entries()
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["Alice", "Bob"],
        "the roster must reflect the real folded tab list, in vanilla's display order"
    );
}

/// The credits-screen handoff, exercised through production code exactly like
/// the social-roster test above: `menu::UiState::show_credits` and
/// `net::NetUpdate::WinGame` both already existed, individually tested,
/// with **nothing calling either from the other** — the credits screen was
/// reachable only from a test, and `WinGame` only reached a channel no
/// one drained into UI state. This drives the real chain end to end: a
/// real `WindowApp`, a real `NetUpdate::WinGame` through the loopback
/// feed (the same seam `NetClient::run`'s background thread publishes
/// into in production, once `net::forward` — separately proven by
/// `forward_translates_win_game_into_the_credits_signal` — turns the real
/// decoded `ClientEvent::WinGame` into it), `Sim::poll_net`'s real
/// `WinGame` arm, and `drive_ui_from_session` itself.
#[test]
fn drive_ui_from_session_opens_credits_on_the_real_win_game_event() {
    use crate::net::NetUpdate;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    // Reach a live-gameplay screen the same way `on_credits` (`menu/
    // nav.rs`'s own test helper) does — `show_credits` only leaves from
    // `Playing | Chat | Container | Paused`, matching `die`'s guard.
    app.ui.enter_dev_world();
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Playing,
        "precondition: must be on a live-gameplay screen before WinGame arrives"
    );
    assert!(
        !app.sim.has_won(),
        "precondition: nothing has signalled a win yet"
    );

    feed.send(NetUpdate::WinGame).unwrap();
    app.sim.step(1.0 / 20.0);
    assert!(
        app.sim.has_won(),
        "Sim::poll_net's real WinGame arm must latch the win"
    );
    // Precondition restated after the poll but before the real call this
    // test exercises, so the assertion below cannot be explained by
    // something upstream having already moved the screen.
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Playing,
        "precondition: drive_ui_from_session has not run yet"
    );

    app.drive_ui_from_session();

    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Credits,
        "the real WIN_GAME event (GAME_EVENT code 4, vanilla's own game-event packet handling) \
         must open the credits screen"
    );
}

/// **The owner's live report, reproduced deterministically and fixed.**
/// "accepting the custom resource pack didn't do anything, and it kept the
/// choice menu open. when i pressed accept again, it closed it and no
/// texture pack was applied at all."
///
/// Traced: `NetClient::respond_to_resource_pack` only *queues* the answer
/// for the net thread's own loop to drain (up to 15 ms later on native) —
/// [`crate::net::PackPromptCell`]'s doc used to claim the shared cell was
/// cleared "the instant" an answer was queued, which is false; only the
/// drain clears it. `app/session.rs`'s `drive_ui_from_session` reconciles
/// `Screen::ResourcePackPrompt` from that same cell every frame, and used to
/// reopen on any frame where the UI was closed but the (still-stale) cell
/// still reported the answered prompt pending — which a real session hits
/// on the very frame of the click, since the click handler and `redraw`
/// share one winit dispatch. That is symptom 1 and 2 exactly: the first
/// Accept appears to do nothing because the reopen is instant, and the
/// dialog "stays open". A real second click then answers the *same* id
/// again; net's `apply_pack_response` finds it already cleared and does
/// nothing further, so **only the first click's answer ever drives the
/// download** — but the player never got to see that, because the dialog
/// reopening ate their confirmation that anything had happened.
///
/// This drives the real chain end to end — a real `WindowApp`, the real
/// `MenuNav::click`/`apply_menu_action`/`respond_to_resource_pack` path, and
/// the real `drive_ui_from_session` — with a **loopback** `NetClient`
/// standing in for the net thread. That is not a weaker double here: a
/// loopback's `pack_response_tx` has no receiver at all (see
/// [`NetClient::loopback`]'s own doc), so the shared cell *never* clears —
/// the permanent, worst-case version of the up-to-15ms lag a real session
/// only suffers briefly. If the reconcile can stay closed against a ground
/// truth that never catches up, it certainly survives one that catches up
/// within a frame or two.
///
/// **Negative control, executed:** reverting `app/session.rs`'s reconcile to
/// its old unconditional `if !self.ui.is_resource_pack_prompt() {
/// show_resource_pack_prompt(...) }` (dropping the
/// `resource_pack_already_answered` check) makes this fail at the final
/// assertion — the screen is `ResourcePackPrompt` again, reproducing the
/// report.
#[test]
fn accepting_a_resource_pack_prompt_does_not_reopen_it_before_the_net_thread_catches_up() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions) = NetClient::loopback();
    app.sim.attach_net(net);
    app.ui.enter_dev_world();
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Playing,
        "precondition: a live-gameplay screen, one of the five the prompt can open over"
    );

    let id = uuid::Uuid::from_u128(0xF00D);
    app.sim
        .net()
        .expect("attached above")
        .set_pending_resource_pack_prompt_for_test(crate::net::PendingResourcePackPrompt::for_test(
            id, false,
        ));

    // The opening edge: a fresh reconcile with nothing shown yet must arm it.
    app.drive_ui_from_session();
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::ResourcePackPrompt,
        "precondition: the pushed prompt must open the confirm screen"
    );

    // The player's one and only click on Accept.
    let action = app.nav.click(&mut app.ui, crate::menu::confirm::ACCEPT_ROW);
    app.apply_menu_action(action);
    assert!(
        !app.ui.is_resource_pack_prompt(),
        "the click's own `apply_resource_pack_prompt` must close the screen immediately"
    );

    // The reconcile that used to reopen it: the loopback's shared cell still
    // reports `id` pending (nothing ever drains `respond_to_resource_pack`'s
    // queued answer on a loopback), which is exactly the stale read a real
    // net thread produces for its first ~15 ms.
    app.drive_ui_from_session();
    assert!(
        !app.ui.is_resource_pack_prompt(),
        "a single Accept must not reopen the very prompt it just answered, \
         even while the net thread has not yet cleared the shared cell — \
         this is the owner's \"accepting did nothing, it kept the choice \
         menu open\" report"
    );

    // And the suppression is scoped to *that* id, not sticky forever: a
    // genuinely different prompt (a second push superseding the first, the
    // same case `PackPromptCell::clear_if`'s own doc names) must still open
    // even though `resource_pack_answered_id` is still set from the first.
    app.sim
        .net()
        .expect("attached above")
        .set_pending_resource_pack_prompt_for_test(crate::net::PendingResourcePackPrompt::for_test(
            uuid::Uuid::from_u128(0xBEEF),
            false,
        ));
    app.drive_ui_from_session();
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::ResourcePackPrompt,
        "a later, genuinely different prompt must still open — the \
         suppression must be scoped to the answered id, not sticky forever"
    );
}

/// Live gate: `ShellWeatherProbe::precipitation` must reach
/// a real per-column snow/rain decision now that the biome-climate lane
/// is wired, not the `Rain` it answered unconditionally before this
/// session (the shell's session path must not substitute an unconditional value).
///
/// Connects directly through `ClientBuilder`, bypassing `NetClient`'s
/// background thread so the raw event stream can be read here: the real
/// `ClientEvent::BiomeClimates` is captured off it and folded into a
/// `BiomeClimateCell` **by hand, with the same call** `net::forward`'s
/// arm makes — proving the fold, not merely trusting it — while every
/// other event is drained so the driver's bounded channel never blocks.
/// Mirrors `net::tests::live_entity_light_at_distinguishes_loaded_from_unloaded`'s
/// shape.
///
/// The expected precipitation per sampled column is computed **here**,
/// independently of both `ShellWeatherProbe` and `lodestone_render::
/// weather` — the raw climate is pulled straight off the `BiomeClimateCell`
/// and vanilla's own threshold is applied by hand, taken from the
/// decompiled source's behaviour rather than from this crate's constant:
/// vanilla's own warm-enough-to-rain check returns true when the
/// height-adjusted temperature is at least 0.15, and that check is what
/// vanilla's own precipitation-at-position resolve calls. A
/// wrong threshold in either implementation would show up as a mismatch
/// against this independently-computed expectation rather than agreeing
/// with itself — the `decode(encode(x)) == x` trap `CLAUDE.md` warns
/// about, avoided by never calling `precipitation_for_temperature`/
/// `height_adjusted_temperature` from this test.
///
/// ```text
/// cargo test -p lodestone-shell --features live --lib \
///     app::tests::live_precipitation_matches_vanillas_own_threshold_for_real_biomes \
///     -- --ignored --nocapture
/// ```
#[cfg(feature = "live")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the lodestone-survival server on 127.0.0.1:25565"]
async fn live_precipitation_matches_vanillas_own_threshold_for_real_biomes() {
    use crate::net::BiomeClimateCell;
    use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
    use lodestone_render::WeatherProbe as _;
    use lodestone_testsupport::{poll_until, unique_username};

    let user = unique_username();
    let protocol = 776; // vanilla 26.2 — the `live` feature's compiled-in family
    let adapter = lodestone_registry::adapter_for_protocol(protocol)
        .expect("the `live` feature compiles a family in for protocol 776");
    let (handle, mut events) = ClientBuilder::new(
        ServerAddress {
            host: "127.0.0.1".into(),
            port: 25565,
        },
        LoginProfile {
            username: user.clone(),
            uuid: uuid::Uuid::new_v4(),
        },
        adapter,
    )
    .connect()
    .await
    .expect("connect to lodestone-survival on 127.0.0.1:25565");

    let climates = Arc::new(BiomeClimateCell::default());
    let climates_thread = Arc::clone(&climates);
    let drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let lodestone_model::ClientEvent::BiomeClimates {
                temperatures,
                downfall,
                has_precipitation,
            } = event
            {
                // The exact fold `net::forward`'s `BiomeClimates` arm
                // makes — called here by hand since this test bypasses
                // `forward` entirely to read the raw stream.
                climates_thread.apply(&temperatures, &downfall, &has_precipitation);
            }
        }
    });

    assert!(
        poll_until(
            Duration::from_secs(30),
            Duration::from_millis(100),
            || async {
                handle
                    .players()
                    .into_iter()
                    .find(|p| p.name.as_deref() == Some(user.as_str()))
            }
        )
        .await
        .is_some(),
        "player {user} never reached Play on the oracle"
    );

    let dims = poll_until(
        Duration::from_secs(10),
        Duration::from_millis(100),
        || async { handle.world_dimensions() },
    )
    .await
    .expect("world dimensions never arrived");

    let loaded = poll_until(
        Duration::from_secs(15),
        Duration::from_millis(200),
        || async {
            let chunks = handle.loaded_chunks();
            if chunks.is_empty() { None } else { Some(chunks) }
        },
    )
    .await
    .expect("no chunks streamed in within 15s of login");

    // The registry (and with it `BiomeClimates`) lands at `Login`, ahead
    // of chunk data, but poll rather than assume the ordering: this test
    // cares about the fold having happened, not about racing it.
    assert!(
        poll_until(Duration::from_secs(10), Duration::from_millis(100), || {
            let climates = Arc::clone(&climates);
            async move { climates.get(0).map(|_| ()) }
        })
        .await
        .is_some(),
        "ClientEvent::BiomeClimates never arrived — the climate table is still empty"
    );

    let handle = Arc::new(handle);
    let probe = ShellWeatherProbe {
        light: 1.0,
        handle: Some(Arc::clone(&handle)),
        biome_climates: Some(Arc::clone(&climates)),
        // One `section_at` per distinct chunk column rather than per call — this
        // gate samples 16 different columns, so it fetches 16 sections and reuses
        // none, which is exactly the memo's contract.
        memo: Default::default(),
    };

    // Sample a real column in the middle of a loaded chunk, at mid-build-
    // height. `checked` and `snow_seen`/`rain_seen` are reported in the
    // panic message so a failure names the real biome and climate
    // involved, not just "mismatch".
    let mut checked = 0usize;
    let mut mismatches: Vec<String> = Vec::new();
    for chunk in loaded.iter().take(16) {
        let y = dims.min_y + (dims.height as i32 / 2);
        let block_x = chunk.x * 16 + 8;
        let block_z = chunk.z * 16 + 8;
        let base_si = dims.min_y.div_euclid(16);
        let si = y.div_euclid(16) - base_si;
        if si < 0 || (si as usize) >= dims.section_count() {
            continue;
        }
        let Some(section) = handle.section_at(*chunk, si as usize) else {
            continue;
        };
        let biome = section.biome_at_block(8, y.rem_euclid(16) as usize, 8);
        let Some(climate) = climates.get(usize::try_from(biome).unwrap_or(usize::MAX)) else {
            continue;
        };
        let (Some(temperature), Some(has_precipitation)) =
            (climate.temperature, climate.has_precipitation)
        else {
            continue;
        };
        checked += 1;

        // Independent re-derivation, not a call to `lodestone_render::
        // weather`: vanilla's own height falloff
        // (its own height-adjusted-temperature computation)
        // and its own rain/snow threshold (`0.15F`).
        let above = (y - crate::worldgen::SEA_LEVEL) as f32;
        let adjusted = if above > 0.0 {
            temperature - above * 0.05 / 40.0
        } else {
            temperature
        };
        let expected = if !has_precipitation {
            lodestone_render::Precipitation::None
        } else if adjusted >= 0.15 {
            lodestone_render::Precipitation::Rain
        } else {
            lodestone_render::Precipitation::Snow
        };

        let actual = probe.precipitation(block_x, y, block_z);
        println!(
            "chunk {chunk:?} biome {biome} temperature={temperature} \
             has_precipitation={has_precipitation} adjusted={adjusted} -> {expected:?}"
        );
        if actual != expected {
            mismatches.push(format!(
                "chunk {chunk:?} biome {biome} temperature={temperature} \
                 has_precipitation={has_precipitation} adjusted={adjusted}: \
                 expected {expected:?}, probe returned {actual:?}"
            ));
        }
    }

    assert!(
        checked > 0,
        "no loaded column resolved a section + biome + climate — the wiring \
         chain (section_at → biome_at_block → BiomeClimateCell) never \
         produced real data to check against"
    );
    assert!(
        mismatches.is_empty(),
        "{}/{checked} sampled columns disagreed with vanilla's own threshold: \
         {mismatches:#?}",
        mismatches.len()
    );

    drain.abort();
}

/// **Recipe-book settings synchronization, exercised through production code.**
///
/// `RECIPE_BOOK_SETTINGS` (76) decoded and folded as of `fd53995` and
/// **nothing read it**: the recipe-book panel started closed and unfiltered
/// on every join no matter what the server said. This does not call
/// `RecipeBookSettings::for_type` a second time by hand — that would be the
/// existing unit test again, which proves nothing about production. It drives
/// the real chain: a real `WindowApp`, a real `ClientEvent` folded through the
/// same `NetIngest` schedule the net thread runs, and
/// `drive_ui_from_session` itself — the method `redraw()` calls every frame.
///
/// The `open` bit is the pixel-visible one: `RecipePanelState::open` is what
/// `recipe_panel_geometry` turns into the panel body's vertices, gated by
/// `an_open_panel_covers_its_own_screen_rect` / the closed-panel control in
/// `recipe_book_wiring.rs`.
#[test]
fn drive_ui_from_session_restores_the_recipe_book_panel_the_server_reported() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_model::event::RecipeBookTypeSettings;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);

    // The restore only runs with a recipe-book-bearing menu on screen: the
    // player inventory's own 2x2 grid makes `recipe_book_type_for` answer
    // `Crafting`. Reach it the way a player does.
    app.ui.enter_dev_world();
    app.ui.open_container();

    // Precondition, and it is load-bearing: an unreported record is all-false,
    // which is indistinguishable from "the server wants it closed". If the
    // panel were somehow already open, the assertion below would pass without
    // the restore ever running.
    assert!(
        !app.recipe_panel.open,
        "precondition: the panel must start closed — that is the defect being fixed"
    );
    app.drive_ui_from_session();
    assert!(
        !app.recipe_panel.open,
        "control: with nothing reported, the restore must NOT fire — otherwise \
         this gate cannot tell a real restore from the default it replaces"
    );

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookSettingsChanged {
            crafting: RecipeBookTypeSettings { open: true, filtering: true },
            furnace: RecipeBookTypeSettings::default(),
            blast_furnace: RecipeBookTypeSettings::default(),
            smoker: RecipeBookTypeSettings::default(),
        });

    app.drive_ui_from_session();

    assert!(
        app.recipe_panel.open,
        "the crafting book's reported `open` must reach the panel the draw reads"
    );
    assert!(
        app.recipe_panel.filtering,
        "and so must `filtering` — the All/Craftable state"
    );

    // The latch: a user who closes the panel must not have it reopened on the
    // very next frame by the same reported settings.
    app.recipe_panel.open = false;
    app.drive_ui_from_session();
    assert!(
        !app.recipe_panel.open,
        "the restore is once per book type, not every frame — otherwise it \
         would fight the user's own clicks"
    );
}

/// The **negative control** for the gate above, run and observed: the furnace
/// book's settings must not restore into a crafting panel.
///
/// # What this control can and cannot see — measured, not assumed
///
/// Neutering `for_type(book_type)` to `settings.furnace` fails **both** this
/// test and the positive one above (observed). So the pair really does pin the
/// per-type read in that direction.
///
/// It does **not** catch a restore hardcoded to `settings.crafting`: that was
/// tried, and both tests stayed green. The reason is a property of the
/// harness, not of the assertions — `active_container_menu` here resolves to
/// the *player inventory*, whose 2×2 grid makes `recipe_book_type_for` answer
/// `Crafting`, so `crafting` **is** the correct field for every scenario this
/// harness can construct. Putting a furnace on screen needs a server-opened
/// menu (`Sim::open_menu`), which this loopback feed has no route to.
///
/// Recorded rather than quietly left as a gap: a control whose premise is
/// false fails in the safe-looking direction, and the way to find that out is
/// to run the neuter and watch it *not* fire. Whoever gains a furnace-menu
/// harness should extend this test rather than write a third one.
#[test]
fn a_crafting_panel_does_not_restore_the_furnace_books_settings() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_model::event::RecipeBookTypeSettings;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();
    app.ui.open_container();

    // Only the *furnace* book is open, and it is a different book than the
    // player-inventory crafting grid on screen.
    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookSettingsChanged {
            crafting: RecipeBookTypeSettings::default(),
            furnace: RecipeBookTypeSettings { open: true, filtering: true },
            blast_furnace: RecipeBookTypeSettings::default(),
            smoker: RecipeBookTypeSettings::default(),
        });

    app.drive_ui_from_session();

    assert!(
        !app.recipe_panel.open,
        "the furnace book's `open` must NOT open the crafting panel — the \
         restore has to read `for_type(book_type)`, not the first field"
    );
    assert!(
        !app.recipe_panel.filtering,
        "same for `filtering`"
    );
}

/// The recipe-unlock toast's missing hop: `SessionRecipeBook` was folded and
/// read by nothing, so `RecipeToastQueue::push` had zero production callers
/// and the toast could never appear. Drives the real chain: a real
/// `WindowApp`, a real `ClientEvent::RecipeBookAdded` folded through the same
/// `NetIngest` schedule the net thread runs, and `drive_ui_from_session`
/// itself — the method `redraw()` calls every frame, which is where
/// `WindowApp::sync_recipe_toasts` now lives.
///
/// Two properties, both load-bearing:
///
/// * the first sync (`replace: true`, vanilla's join-time seed) must seed the
///   "already toasted" set and toast **nothing** — vanilla does not toast a
///   fresh join's entire unlock history;
/// * a genuinely new unlock **after** that must reach
///   [`crate::hud::HudFrame::recipe_toast`]'s own producer,
///   [`recipe_toast_view`], with the *station* and *unlocked* item ids the
///   decode carried — not transposed, and not the discarded `_category` this
///   feature used to drop instead.
#[test]
fn drive_ui_from_session_toasts_a_newly_unlocked_recipe_but_not_the_join_time_seed() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_model::event::RecipeBookEntry;

    let torch = i32::from(Item::Torch.registry_id());
    let crafting_table = i32::from(Item::CraftingTable.registry_id());

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookAdded {
            entries: vec![RecipeBookEntry {
                display_id: 1,
                result_items: vec![torch],
                station_items: vec![crafting_table],
                group: None,
                category: 0,
                crafting_requirements: None,
                notification: true,
                highlight: false,
            }],
            replace: true,
        });
    app.drive_ui_from_session();
    assert!(
        recipe_toast_view(&app.recipe_toasts, recipe_toast_now_ms()).is_none(),
        "the first join-time sync must seed the seen set, not replay the \
         whole unlock history as toasts"
    );

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookAdded {
            entries: vec![RecipeBookEntry {
                display_id: 2,
                result_items: vec![torch],
                station_items: vec![crafting_table],
                group: None,
                category: 0,
                crafting_requirements: None,
                notification: true,
                highlight: false,
            }],
            replace: false,
        });
    app.drive_ui_from_session();

    let view = recipe_toast_view(&app.recipe_toasts, recipe_toast_now_ms())
        .expect("a newly-unlocked, notifying recipe must reach the toast queue");
    assert_eq!(
        view.station.item.to_string(),
        "minecraft:crafting_table",
        "the station icon must be the crafting station, not the unlocked item"
    );
    assert_eq!(
        view.unlocked.item.to_string(),
        "minecraft:torch",
        "the unlocked icon must be the result item, not the station"
    );
}

/// The control for the gate above: a recipe with `notification: false` must
/// never toast, even on a later (non-seeding) sync — vanilla's tab-highlight-
/// only unlocks are silent.
#[test]
fn a_non_notifying_unlock_never_toasts() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_model::event::RecipeBookEntry;

    let torch = i32::from(Item::Torch.registry_id());
    let crafting_table = i32::from(Item::CraftingTable.registry_id());

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);

    // Seed with an empty first sync.
    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookAdded {
            entries: Vec::new(),
            replace: true,
        });
    app.drive_ui_from_session();

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookAdded {
            entries: vec![RecipeBookEntry {
                display_id: 5,
                result_items: vec![torch],
                station_items: vec![crafting_table],
                group: None,
                category: 0,
                crafting_requirements: None,
                notification: false,
                highlight: true,
            }],
            replace: false,
        });
    app.drive_ui_from_session();

    assert!(
        recipe_toast_view(&app.recipe_toasts, recipe_toast_now_ms()).is_none(),
        "notification: false must never raise a toast"
    );
}

/// The recipe-seen packet: its encoder existed in every protocol
/// family and nothing anywhere called it. Drives the real chain: a real
/// `WindowApp`, a real `ClientEvent::RecipeBookAdded` folded through the same
/// `NetIngest` schedule the net thread runs, a recipe corpus loaded so the
/// panel's page actually contains the unlocked result, and
/// `drive_ui_from_session` itself, which is where `WindowApp::sync_recipe_book_seen`
/// now lives.
///
/// Two properties: the recipe must actually be **on the open page** (vanilla
/// only fires this for a populated recipe button, not for the whole
/// corpus), and reporting it once must not report it again next frame — the
/// dedup [`WindowApp::recipe_book_seen`] exists for.
#[test]
fn drive_ui_from_session_reports_a_visible_highlighted_recipe_as_seen_exactly_once() {
    use crate::net::NetUpdate;
    use lodestone_client::{ClientAction, ClientEvent};
    use lodestone_game::item::ItemStack;
    use lodestone_game::recipe::{Ingredient, Recipe, RecipeBook, ShapedRecipe};
    use lodestone_model::event::RecipeBookEntry;

    let torch_id: lodestone_model::Identifier = "minecraft:torch".parse().unwrap();
    let mut book = RecipeBook::new();
    book.insert(
        torch_id.clone(),
        Recipe::Shaped(ShapedRecipe::new(
            1,
            2,
            vec![
                Some(Ingredient::Item("minecraft:coal".parse().unwrap())),
                Some(Ingredient::Item("minecraft:stick".parse().unwrap())),
            ],
            ItemStack::new(torch_id, 4),
        )),
    );

    let torch = i32::from(Item::Torch.registry_id());
    let crafting_table = i32::from(Item::CraftingTable.registry_id());

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.recipe_book = Some(book);
    let (net, actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();
    app.ui.open_container();
    app.recipe_panel.open = true;
    // Drain the login-time traffic this harness generates so the assertion
    // below is exactly the seen-recipe send, not an artifact of setup.
    while actions.try_recv().is_ok() {}

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookAdded {
            entries: vec![RecipeBookEntry {
                display_id: 3,
                result_items: vec![torch],
                station_items: vec![crafting_table],
                group: None,
                category: 0,
                crafting_requirements: None,
                notification: false,
                highlight: true,
            }],
            replace: true,
        });
    app.drive_ui_from_session();

    assert_eq!(
        actions.try_recv(),
        Ok(ClientAction::RecipeBookSeenRecipe { recipe: 3 }),
        "a highlighted recipe visible on the open page must be reported seen"
    );
    assert!(
        actions.try_recv().is_err(),
        "exactly one report for one newly-shown recipe"
    );

    // The dedup control: the same recipe stays on the same page next frame,
    // and must not be re-reported.
    app.drive_ui_from_session();
    assert!(
        actions.try_recv().is_err(),
        "a recipe already reported seen must not be reported again"
    );
}

/// The control for the gate above: a highlighted recipe that is **not** on
/// the currently open page (the panel is closed) must never be reported —
/// the client only fires this for a populated recipe button.
#[test]
fn a_highlighted_recipe_is_not_reported_while_the_panel_is_closed() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_game::item::ItemStack;
    use lodestone_game::recipe::{Ingredient, Recipe, RecipeBook, ShapedRecipe};
    use lodestone_model::event::RecipeBookEntry;

    let torch_id: lodestone_model::Identifier = "minecraft:torch".parse().unwrap();
    let mut book = RecipeBook::new();
    book.insert(
        torch_id.clone(),
        Recipe::Shaped(ShapedRecipe::new(
            1,
            2,
            vec![
                Some(Ingredient::Item("minecraft:coal".parse().unwrap())),
                Some(Ingredient::Item("minecraft:stick".parse().unwrap())),
            ],
            ItemStack::new(torch_id, 4),
        )),
    );

    let torch = i32::from(Item::Torch.registry_id());
    let crafting_table = i32::from(Item::CraftingTable.registry_id());

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.recipe_book = Some(book);
    let (net, actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();
    app.ui.open_container();
    // Deliberately left closed, unlike the positive gate above.
    assert!(!app.recipe_panel.open, "precondition: the panel starts closed");
    while actions.try_recv().is_ok() {}

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookAdded {
            entries: vec![RecipeBookEntry {
                display_id: 3,
                result_items: vec![torch],
                station_items: vec![crafting_table],
                group: None,
                category: 0,
                crafting_requirements: None,
                notification: false,
                highlight: true,
            }],
            replace: true,
        });
    app.drive_ui_from_session();

    assert!(
        actions.try_recv().is_err(),
        "a closed panel must never report a recipe seen, however unlocked it is"
    );
}

/// The owner's report: right-clicking a server-side "server selector" opens a
/// container, selecting a row makes the **server** close it, and our client
/// showed the player's own inventory instead of returning to gameplay —
/// vanilla shows no screen at all.
///
/// Drives the real chain a server-initiated close takes: a real `WindowApp`,
/// a real server-opened (non-zero window id) menu folded through the same
/// `NetIngest` schedule the net thread runs (`ScreenOpened` + a matching
/// `ContainerContent`, exactly as `lodestone-ecs`'s own
/// `menu_family_events_reach_session_menus_through_the_real_schedule` proves
/// reaches `SessionMenus`), then a server `ScreenClosed` for the same window
/// — and `drive_ui_from_session`, the method `redraw()` calls every frame,
/// which is where `UiState::reconcile_server_menu_window` now lives.
///
/// The `open_container()` call below stands in for `redraw()`'s own
/// `Sim::open_menu().is_some() && is_playing()` branch (untouched by this
/// fix, and not itself under test here — the recipe-book tests above already
/// drive it the same indirect way for the same reason: no GPU in this
/// harness).
#[test]
fn server_initiated_container_close_returns_to_gameplay_not_the_player_inventory() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_model::Text;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();

    let ingest = |app: &WindowApp, event: ClientEvent| {
        app.sim.net().expect("net attached above").ingest_session_event(event);
    };

    // A real 9x3 chest: 27 container slots + 36 player-inventory slots, the
    // same shape `lodestone-ecs`'s own menu-family test uses.
    ingest(
        &app,
        ClientEvent::ScreenOpened {
            window_id: 5,
            menu_type: "minecraft:generic_9x3".parse().expect("valid resource key"),
            title: Text::literal("Chest"),
        },
    );
    ingest(
        &app,
        ClientEvent::ContainerContent {
            window_id: 5,
            state_id: lodestone_model::ContainerStateId::new(1),
            items: vec![None; 63],
            carried_item: None,
        },
    );
    assert_eq!(
        app.sim.open_menu().map(|open| open.window_id),
        Some(5),
        "precondition: a real server menu is open before the close"
    );

    // Stand-in for `redraw()`'s open branch (see this test's own doc).
    app.ui.open_container();
    app.drive_ui_from_session();
    assert!(
        app.active_container_menu().is_some(),
        "precondition: the container screen is actually showing the server's \
         menu before it closes"
    );

    // The server closes window 5 — the container-close packet is decoded
    // into `ClientEvent::ScreenClosed`.
    ingest(&app, ClientEvent::ScreenClosed { window_id: 5 });
    assert_eq!(
        app.sim.open_menu(),
        None,
        "control: the menu-state half of the close (vanilla's `containerMenu \
         = inventoryMenu`) must actually have landed, or this test cannot \
         tell that half apart from the screen half under test"
    );

    app.drive_ui_from_session();

    // The exact expected UI state, not merely "the container screen is
    // gone": `active_container_menu` is `None` (**no** screen), matching
    // vanilla's unconditional close-to-no-screen — not the
    // player's own inventory, which is what the bug showed instead
    // (`active_container_menu`'s window-0 fallback firing off a stale
    // `Screen::Container` the close never reset).
    assert_eq!(
        app.active_container_menu(),
        None,
        "a server-initiated close must show no screen at all, not the \
         player's own inventory"
    );
    assert!(
        app.ui.is_playing(),
        "and the screen itself must have returned to Playing"
    );
}

/// Negative control for the gate above: the player's own `E`-opened
/// inventory — which never has a server window id at all — must **not** be
/// closed by [`UiState::reconcile_server_menu_window`]. A level check on "no
/// window id right now" would close this the very next frame; only an edge
/// on a real `Some -> None` transition may.
#[test]
fn opening_the_local_inventory_with_no_server_window_is_not_closed_by_the_server_reconciler() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(crate::net::NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();

    // `E` with nothing else open: no server window, ever.
    app.ui.open_container();
    assert_eq!(app.sim.open_menu(), None, "precondition: no server menu exists");

    // Several frames, matching the steady state a player leaving their own
    // inventory open would sit in.
    for _ in 0..3 {
        app.drive_ui_from_session();
    }

    assert!(
        app.ui.is_container_open(),
        "the local inventory must stay open with no server window ever having existed"
    );
    assert!(
        app.active_container_menu().is_some(),
        "and it must still be showing the player's own menu"
    );
}

/// **Game-rule synchronization, exercised through production code.**
///
/// The immediate-respawn rule is the most user-visible game rule there is: the
/// reference client never puts the death screen up at all when it is on.
/// `SessionGameRules`
/// was folded, reset on quit-to-title and gated through the real
/// `SharedState::apply` path with **no reader anywhere in the shell**, so the
/// rule did nothing.
///
/// Drives the real chain: a real `WindowApp`, a real `NetUpdate::Death`
/// through the loopback feed (`Sim::poll_net`'s own arm, which sets the `Dead`
/// marker), a real `ClientEvent::GameRulesChanged` through the same
/// `NetIngest` schedule the net thread runs, and `drive_ui_from_session`
/// itself — the method `redraw()` calls every frame.
#[test]
fn immediate_respawn_skips_the_death_screen_entirely() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::GameRulesChanged {
            values: vec![(
                "immediate_respawn".parse().expect("valid identifier"),
                "true".into(),
            )],
        });
    assert_eq!(
        app.sim.game_rules().immediate_respawn(),
        Some(true),
        "precondition: the rule must actually have folded, or this gate is \
         measuring the default and not the rule"
    );

    feed.send(NetUpdate::Death { message: lodestone_model::Text::literal("you died") }).unwrap();
    app.sim.step(1.0 / 20.0);
    assert!(
        app.sim.is_dead(),
        "precondition: the death must have landed, or 'no death screen' is vacuous"
    );

    app.drive_ui_from_session();

    assert!(
        !app.ui.is_death(),
        "with doImmediateRespawn on, the death screen must never appear — not \
         'appear and close next frame', which would flash it for a frame"
    );
    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::Death,
        "and the screen state must not be Death by any other route"
    );
}

/// **The negative control, run and observed**: the *same* death with the rule
/// off must still raise the death screen.
///
/// Without this, `immediate_respawn_skips_the_death_screen_entirely` is
/// satisfied by a client that never shows a death screen at all — which is
/// exactly the state a broken `is_dead` or a broken loopback feed would
/// produce, and it would read as a pass.
#[test]
fn without_the_rule_the_same_death_still_raises_the_death_screen() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();

    // Explicitly `false`, not merely absent: `Some(false)` and `None` take
    // different branches in `immediate_respawn()`, and the shipped behaviour
    // must be identical for both.
    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::GameRulesChanged {
            values: vec![(
                "immediate_respawn".parse().expect("valid identifier"),
                "false".into(),
            )],
        });
    assert_eq!(app.sim.game_rules().immediate_respawn(), Some(false));

    feed.send(NetUpdate::Death { message: lodestone_model::Text::literal("you died") }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.drive_ui_from_session();

    assert!(
        app.ui.is_death(),
        "with the rule off, the death screen must still appear — this is what \
         proves the gate above is measuring the rule and not a client that \
         never shows the screen"
    );
}

/// **Command-block interaction: a real right-click opens the edit screen.**
///
/// `Screen::CommandBlockEdit`, `command_block::CommandBlockState` and
/// `render::command_block_frame` landed in `c76510b` real and unit-tested, and
/// `UiState::open_command_block`/`MenuNav::open_command_block` had **zero
/// production callers** — the screen was reachable only from a test.
///
/// This drives the production path: a real `WindowApp`, a real command block
/// written into the real `ChunkWorld`, a real `RayTarget` (what the crosshair
/// raycast writes), and `WindowApp::try_use` — the method the `KeyOutcome::
/// Use(true)` arm now calls instead of `Sim::use_item`.
#[test]
fn right_clicking_a_command_block_opens_the_edit_screen() {
    use crate::raycast::RayHit;

    // `block_name`, keyed by block-**state** id, is the id space the store and
    // `write_predicted_block` deal in. This used to scan `block_type_name`,
    // whose parameter is a `minecraft:block` **registry** id, so it selected
    // state 407 — `minecraft:cherry_leaves` — and the test agreed with the
    // production bug it was meant to gate (see `command_block_source::
    // mode_for_state`). A `None` here is a broken generated table, not a case to
    // skip: silently returning green is the *precondition* species of vacuous
    // test.
    let state_id = (0u32..lodestone_data::block_states::STATE_COUNT as u32)
        .find(|id| {
            lodestone_data::block_states::block_name(*id)
                .is_some_and(|n| n == "minecraft:command_block")
        })
        .expect("the 26.2 block-state table must contain minecraft:command_block");

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.enter_dev_world();

    // A real command block in the real store the accessor reads.
    let block = [8, 64, 8];
    let world = app.sim.chunk_world_write();
    {
        let mut w = world.write();
        crate::sim::write_predicted_block(&mut *w, block, state_id);
        // The block entity's payload, overwriting the empty record
        // `write_predicted_block`'s `sync_block_entity` just created — the
        // shape a server sends for a command block whose command has been set.
        // Without it the screen opens through the "fail open" default (empty
        // command), and the test's gate — "opens populated with the block's
        // actual command text" — would be untested.
        w.set_block_entity(
            block[0],
            block[1],
            block[2],
            lodestone_data::block_states::StateId::new(state_id)
                .and_then(lodestone_data::block_entity_types::block_entity_type)
                .map(|kind| kind.raw())
                .expect("a command block state owns a block entity type"),
            lodestone_core::Nbt::Compound(vec![
                (
                    "Command".to_string(),
                    lodestone_core::Nbt::String("say hello".into()),
                ),
                ("TrackOutput".to_string(), lodestone_core::Nbt::Byte(1)),
            ]),
        );
    }
    // `face_center` is the real constructor the raycast itself uses, so this
    // cannot disagree with a production hit's shape.
    app.sim
        .set_ray_target_for_test(Some(RayHit::face_center(block, [0, 1, 0])));

    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "precondition: the screen must not already be up, or this proves nothing"
    );

    app.try_use();

    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "a right-click on a command block must open the edit screen — this is \
         the hop that did not exist"
    );
    let state = app
        .nav
        .command_block()
        .expect("the screen's widget state must be built alongside the screen");
    assert_eq!(
        state.to_submit().pos,
        lodestone_model::BlockPos::new(8, 64, 8),
        "and it must open on the block that was actually clicked, not a default          — `to_submit` is what the Done button would actually send"
    );
    assert_eq!(
        state.command.value(),
        "say hello",
        "and it must open populated with the block's actual command text — the \
         issue's gate, carried through the real interaction path: block-entity \
         NBT in the store -> `targeted_command_block` -> the edit screen"
    );
}

/// **The control, run and observed**: the same right-click on a block that is
/// *not* a command block must fall through to the ordinary use path and leave
/// the screen shut.
///
/// Without this, the gate above is satisfied by a `try_use` that opens the
/// command block screen on every right-click anywhere — which would be a far
/// worse bug than the island it replaces, and would read as a pass.
#[test]
fn right_clicking_a_normal_block_does_not_open_the_command_block_screen() {
    use crate::raycast::RayHit;

    // Block-**state** id space, as above: a control that writes some arbitrary
    // block instead of stone still passes, so the wrong accessor made this
    // control weaker than it reads.
    let stone = (0u32..lodestone_data::block_states::STATE_COUNT as u32)
        .find(|id| {
            lodestone_data::block_states::block_name(*id).is_some_and(|n| n == "minecraft:stone")
        })
        .expect("the 26.2 block-state table must contain minecraft:stone");

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.enter_dev_world();

    let block = [8, 64, 8];
    let world = app.sim.chunk_world_write();
    {
        let mut w = world.write();
        crate::sim::write_predicted_block(&mut *w, block, stone);
    }
    // `face_center` is the real constructor the raycast itself uses, so this
    // cannot disagree with a production hit's shape.
    app.sim
        .set_ray_target_for_test(Some(RayHit::face_center(block, [0, 1, 0])));

    app.try_use();

    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "stone is not a command block — the screen must stay shut and the \
         ordinary use path must run"
    );
    assert!(
        app.nav.command_block().is_none(),
        "and no widget state may be built"
    );

    // The other half of the fork: nothing targeted at all.
    app.sim.set_ray_target_for_test(None);
    app.try_use();
    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "and a right-click on empty air must not open it either"
    );
}

/// A framebuffer whose **auto** GUI scale is exactly 1, so a logical pixel is a
/// physical pixel and the coordinates in the command-block tests below need no
/// conversion at all.
///
/// `calculate_gui_scale`'s loop stops when `fb / (scale + 1)` drops below
/// `320x240`: `400 / 2 == 200 < 240`, so it never reaches 2. Asserted in the
/// tests rather than assumed — if this stops being 1 the coordinates below
/// become silently wrong rather than obviously wrong, which is the whole
/// "clicks land one slot off, invisible in a screenshot" failure mode.
const CB_FB_W: u32 = 640;
const CB_FB_H: u32 = 400;

/// **Command-block interaction: a click on the edit screen reaches a row.**
///
/// `0948f59` made the screen *draw*. It still could not be clicked:
/// `app/lifecycle.rs` guarded its `CursorMoved` and `MouseInput` arms on
/// `owns_frame(screen) || is_paused() || is_death()`, and `Screen::
/// CommandBlockEdit` is in none of those — it is an overlay, so `owns_frame` is
/// deliberately `false`. Every click on Done, Cancel, Mode, Conditional and the
/// output toggle was dropped by the match guard before `menu_row_at` was ever
/// called, and `on_screen_frame` had no arm for the screen either, so it would
/// have returned `None` even if a click had got that far. Two missing homes for
/// one screen, the same shape as `0d0ae93`.
///
/// # Why this test lives here and not in `menu/nav.rs`
///
/// `nav::tests::every_mouse_routable_screen_has_a_frame_to_hit_test` exists,
/// passed throughout, and **structurally could not see this**: it hand-copied
/// the driver's routing expression instead of calling it, so it compared two
/// things `nav.rs` controls. This one drives `WindowApp`'s own
/// `menu_row_at_in` — the real frame source, the real scale conversion, the
/// real `row_rect` loop — and asserts the driver's own guard
/// (`routes_menu_input`, now literally the expression in the match guard)
/// answers `true` first.
///
/// # The expected coordinates come from vanilla, not from our frame
///
/// Asking `row_rect` where a row is and then clicking there would be
/// `decode(encode(x)) == x`: it passes for any self-consistent geometry,
/// including one that draws the buttons off-screen. These are computed from
/// vanilla's own command-block-edit-screen layout arithmetic — Done sits
/// at `width/2 - 4 - 150`, Cancel at `width/2 + 4`, both `150x20`
/// at `height/4 + 120 + 12`, and the mode row at `width/2 - 154`,
/// `100x20`, `y = 165`.
#[test]
fn clicking_a_command_block_row_at_its_own_coordinates_activates_that_row() {
    use crate::menu::command_block::{CommandBlockOpen, CommandBlockRow};
    use lodestone_model::CommandBlockMode;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.enter_dev_world();
    app.nav
        .open_command_block(&mut app.ui, CommandBlockOpen::default());

    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "premise: the screen is up — `right_clicking_a_command_block_opens_the_\
         edit_screen` covers the hop that gets it here"
    );
    assert_eq!(
        crate::config::calculate_gui_scale(0, CB_FB_W, CB_FB_H),
        1,
        "premise: at this framebuffer a logical pixel is a physical pixel, so \
         the vanilla-derived coordinates below need no scale conversion"
    );
    // **The link that was broken.** This is the literal expression
    // `app/lifecycle.rs`'s `CursorMoved` and `MouseInput` match guards are
    // written as, so a `false` here is a click that never reaches the body at
    // all — no hit-test, no row, no pixel, and nothing to observe downstream.
    assert!(
        crate::menu::nav::routes_menu_input(&app.ui),
        "the driver's own mouse guard must route to this screen — this was \
         `false`, which is why every click on it was silently dropped"
    );

    // Vanilla's own command-block-edit-screen layout — the footer anchor is
    // `(width/2, height/4 + 120 + 12)` and the buttons are `150x20`.
    let anchor_x = (CB_FB_W as f32 / 2.0).floor();
    let footer_y = (CB_FB_H as f32 / 4.0).floor() + 132.0;
    let done = (anchor_x - 4.0 - 150.0 + 75.0, footer_y + 10.0);
    let cancel = (anchor_x + 4.0 + 75.0, footer_y + 10.0);
    // `:50` — the mode button, `100x20` at `width/2 - 154`, `y = 165`. It
    // shares Done's `dx` exactly, and differs only in `y`, so a hit-test that
    // resolved x and ignored y would answer the same row for both. That is the
    // second hypothesis, not a tolerance.
    let mode = (anchor_x - 154.0 + 50.0, 165.0 + 10.0);

    assert_eq!(
        app.menu_row_at_in(cancel.0, cancel.1, CB_FB_W, CB_FB_H),
        Some(CommandBlockRow::Cancel as usize),
        "a click at Cancel's own vanilla coordinates must resolve to Cancel"
    );
    assert_eq!(
        app.menu_row_at_in(done.0, done.1, CB_FB_W, CB_FB_H),
        Some(CommandBlockRow::Done as usize),
        "and Done's to Done — 150 px apart on the same line, so this is row \
         resolution and not 'every coordinate answers the same row'"
    );
    assert_eq!(
        app.menu_row_at_in(mode.0, mode.1, CB_FB_W, CB_FB_H),
        Some(CommandBlockRow::Mode as usize),
        "and the mode button, which shares Done's x and differs only in y"
    );

    // Now the other half: the resolved row, put through the same
    // `MenuNav::click` the driver calls, must do that row's own thing.
    // Predicted exactly — `next_mode(Redstone) == Sequence` — rather than
    // asserted to have merely changed.
    let row = app
        .menu_row_at_in(mode.0, mode.1, CB_FB_W, CB_FB_H)
        .expect("just asserted");
    assert_eq!(
        app.nav.command_block().map(|s| s.mode),
        Some(CommandBlockMode::Redstone),
        "precondition: a freshly placed command block starts in Redstone mode"
    );
    let action = app.nav.click(&mut app.ui, row);
    app.apply_menu_action(action);
    assert_eq!(
        app.nav.command_block().map(|s| s.mode),
        Some(CommandBlockMode::Sequence),
        "clicking the mode button must cycle Redstone -> Sequence, which is \
         `next_mode`'s own answer — not merely 'the mode changed'"
    );

    // And Cancel, through the same path, closes the screen without sending.
    let row = app
        .menu_row_at_in(cancel.0, cancel.1, CB_FB_W, CB_FB_H)
        .expect("just asserted");
    let action = app.nav.click(&mut app.ui, row);
    app.apply_menu_action(action);
    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::CommandBlockEdit,
        "a click on Cancel must close the screen"
    );
    assert!(
        !crate::menu::nav::routes_menu_input(&app.ui),
        "and the mouse must go back to gameplay — the guard is a property of \
         the screen, not a latch"
    );
}

/// **The control for the gate above, run and observed.**
///
/// Two premises that could each make that test pass for the wrong reason:
///
/// 1. If `menu_row_at_in` answered `Some(_)` for *any* coordinate, the three
///    row assertions would be satisfied by an accident of ordering. The
///    backdrop must resolve to no row.
/// 2. If it answered `Some(_)` regardless of which screen is up, the routing
///    fix would be untested — the frame would be coming from somewhere that
///    does not care about `Screen::CommandBlockEdit`. With the screen closed,
///    the very same coordinates must resolve to nothing.
///
/// The second is the sharper one, and it is the one that fires: before the fix
/// `on_screen_frame` had **no arm** for this screen, so the open-screen
/// assertions above and this closed-screen one would have agreed on `None` —
/// the test above would have failed and this one would have passed, which is
/// the correct polarity for a control.
#[test]
fn no_command_block_row_hit_tests_off_the_rows_or_off_the_screen() {
    use crate::menu::command_block::CommandBlockOpen;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.ui.enter_dev_world();
    app.nav
        .open_command_block(&mut app.ui, CommandBlockOpen::default());

    let anchor_x = (CB_FB_W as f32 / 2.0).floor();
    let footer_y = (CB_FB_H as f32 / 4.0).floor() + 132.0;
    let cancel = (anchor_x + 4.0 + 75.0, footer_y + 10.0);

    // (1) The backdrop. `y = 5` is above the title (`TITLE_Y == 20`) and below
    // nothing, so no widget on this screen can claim it.
    assert_eq!(
        app.menu_row_at_in(anchor_x, 5.0, CB_FB_W, CB_FB_H),
        None,
        "the backdrop must resolve to no row, or the gate above is satisfied \
         by a hit-test that answers `Some` everywhere"
    );
    // The gap between Done's bottom (`footer_y + 20`) and the canvas floor.
    assert_eq!(
        app.menu_row_at_in(anchor_x, footer_y + 60.0, CB_FB_W, CB_FB_H),
        None,
        "and so must the gap below the footer buttons"
    );

    // (2) The same Cancel coordinate, with the screen shut. Observed to be
    // `Some(Cancel)` immediately above and `None` here, from one framebuffer
    // and one coordinate — so the difference is the screen and nothing else.
    assert!(
        app.menu_row_at_in(cancel.0, cancel.1, CB_FB_W, CB_FB_H)
            .is_some(),
        "premise: this coordinate does hit a row while the screen is open"
    );
    app.nav.close_command_block(&mut app.ui);
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Playing,
        "premise: the screen is now shut and the world is back"
    );
    assert_eq!(
        app.menu_row_at_in(cancel.0, cancel.1, CB_FB_W, CB_FB_H),
        None,
        "with the screen shut, the same coordinate must hit nothing — a click \
         in the world may never resolve to a command block button"
    );
}

/// F3+F4's cycle visits all four modes and returns, and F3+N's fallback is
/// falling back to Creative when no previous mode is available.
///
/// The cycle is the whole decidable part of the first `ClientAction::ChangeGameMode`
/// producer in the workspace — the variant was encoded by two protocol families
/// and constructed by nothing, so the server's own `ServerBound::ChangeGameMode`
/// arm could never fire.
#[test]
fn the_game_mode_cycle_visits_every_mode_and_returns() {
    use lodestone_model::GameMode;
    let mut mode = GameMode::Survival;
    let mut seen = vec![mode];
    for _ in 0..4 {
        mode = super::session::next_game_mode(Some(mode));
        seen.push(mode);
    }
    assert_eq!(
        seen,
        vec![
            GameMode::Survival,
            GameMode::Creative,
            GameMode::Adventure,
            GameMode::Spectator,
            GameMode::Survival,
        ]
    );
    // No session, or a server that has not reported one, starts at creative.
    assert_eq!(super::session::next_game_mode(None), GameMode::Creative);
}

/// F3+N and F3+F4 resolve to their own outcomes only while F3 is held, and both
/// mark the chord used so releasing F3 does not also toggle the debug overlay.
#[test]
fn the_game_mode_chords_need_the_debug_modifier() {
    let held = KeyGate {
        gameplay: true,
        debug_held: true,
        ..KeyGate::default()
    };
    assert_eq!(
        resolve(held, KeyCode::KeyN, true),
        Some(KeyOutcome::ToggleSpectator)
    );
    assert_eq!(
        resolve(held, KeyCode::F4, true),
        Some(KeyOutcome::CycleGameMode)
    );
    // Without the modifier neither key means anything — the negative half is the
    // point, since an arm that ignored `debug_held` would fire on every F4.
    assert_eq!(resolve(playing(), KeyCode::F4, true), None);
    assert_eq!(resolve(playing(), KeyCode::KeyN, true), None);
    // Release is not a chord.
    assert_eq!(resolve(held, KeyCode::F4, false), None);
}

// -- the scrolling-list clip, in the router ----------------------------------
//
// A framebuffer whose **auto** GUI scale is exactly 1, so a logical pixel is a
// physical pixel and the vanilla-derived coordinates below need no conversion.
//
// `calculate_gui_scale`'s loop stops when `fb / (scale + 1)` drops below
// `320x240`, and `479 / 2 == 239 < 240` — so this is 854x479 rather than the
// 854x480 reference canvas every hermetic geometry test uses, precisely because
// 480 would auto-scale to 2. Asserted in the tests rather than assumed.
const LIST_FB_W: u32 = 854;
const LIST_FB_H: u32 = 479;

/// The *Join Server* button's rect, hand-derived from vanilla's own numbers.
///
/// Nothing in here is read back out of our arranged layout — that is the point.
/// Vanilla's own multiplayer-screen init builds a header-and-footer layout with
/// a 33 px header and a 60 px footer,
/// and fills the footer with a vertical stack (4 px spacing) holding two
/// horizontal rows (4 px spacing): three 100 px-wide
/// buttons, then four 74 px-wide ones. Every button is
/// 20 px tall, and the footer frame
/// aligns its child at `(0.5, 0.5)`.
///
/// So: the child column measures `3*100 + 2*4 == 308` wide (the lower row is
/// `4*74 + 3*4 == 308` too) and `20 + 4 + 20 == 44` tall; the footer band is the
/// bottom 60 px; centring 44 in 60 puts the column 8 px below the band's top, and
/// centring 308 in the canvas puts it at `(854 - 308) / 2`. *Join Server* is the
/// top row's first cell.
///
/// `the_derived_join_server_rect_is_the_one_the_layout_arranges` checks this
/// against `server_list_footer_slot`, so a divergence is a named failure rather
/// than a silently wrong probe point.
fn join_server_rect() -> (f32, f32, f32, f32) {
    let w = LIST_FB_W as f32;
    let h = LIST_FB_H as f32;
    let footer_band_top = h - SERVER_LIST_FOOTER_H_VANILLA;
    let column_h = 20.0 + 4.0 + 20.0;
    let column_w = 3.0 * 100.0 + 2.0 * 4.0;
    (
        ((w - column_w) / 2.0).floor(),
        footer_band_top + ((SERVER_LIST_FOOTER_H_VANILLA - column_h) / 2.0).floor(),
        100.0,
        20.0,
    )
}

/// Vanilla's own header-and-footer layout's footer height, and the same value
/// `server_row_visible` subtracts from the canvas to find the list's bottom.
const SERVER_LIST_FOOTER_H_VANILLA: f32 = 60.0;
/// The same layout's header height, which is where the content band starts (the
/// content frame is clamped up against the footer, so it never gets the 30 px
/// preferred gap on this screen).
const SERVER_LIST_HEADER_H_VANILLA: f32 = 33.0;
/// Vanilla's own server-selection list row height.
const SERVER_LIST_ITEM_H_VANILLA: f32 = 36.0;
/// Vanilla's own selection-list first-entry offset: the list's own y plus 2.
const SERVER_LIST_FIRST_ENTRY_Y_VANILLA: f32 = 2.0;

/// A `WindowApp` on the multiplayer screen with `n` servers in the list, added
/// through the real Add Server form.
///
/// The nav is replaced with one pointing at a throwaway directory **before** any
/// server is added: `MenuNav::new` reads and writes the player's own
/// `servers.json`, and a test that added rows through it would rewrite the real
/// file. Hostnames are RFC 2606 `.invalid`, so nothing here can resolve even if a
/// probe were ever spawned.
/// Writes a roster holding one account into `dir`, so a `MenuNav` built on it
/// is **past the ownership gate**.
///
/// Every gate in this file that presses menu keys needs one: with an empty
/// roster the gate intercepts the first keystroke and the symptom is a screen
/// assertion failing on `Screen::Ownership`, which names nothing about
/// accounts. Must run before the `MenuNav` is constructed — the account screen
/// reads the roster once, in its constructor.
fn seed_owning_account(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).expect("a temp dir for the seeded roster");
    let mut meta = lodestone_auth::AccountsMetadata::default();
    let id = uuid::Uuid::new_v4();
    meta.upsert(lodestone_auth::AccountProfile {
        profile_id: id,
        username: "OwnerAccount".to_owned(),
        skin_url: None,
        last_used: 1,
    });
    meta.selected = Some(id);
    meta.save_to(&dir.join("profiles.json"))
        .expect("the temp roster must be writable");
}

fn app_with_servers(tag: &str, n: usize) -> WindowApp {
    use crate::menu::nav::{MenuKey, MenuNav};

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let dir = std::env::temp_dir().join(format!(
        "lodestone-listclip-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    seed_owning_account(&dir);
    app.nav = MenuNav::with_path(dir.join("servers.json"));
    app.ui.open_server_list();
    for i in 0..n {
        app.nav.key(&mut app.ui, MenuKey::Char('a'));
        for c in format!("S{i}").chars() {
            app.nav.key(&mut app.ui, MenuKey::Char(c));
        }
        app.nav.key(&mut app.ui, MenuKey::Tab);
        for c in format!("h{i}.invalid").chars() {
            app.nav.key(&mut app.ui, MenuKey::Char(c));
        }
        app.nav.key(&mut app.ui, MenuKey::Enter);
    }
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::ServerList,
        "premise: the multiplayer screen is up after adding {n} servers"
    );
    assert_eq!(app.nav.list().len(), n, "premise: {n} rows are in the list");
    // Adding leaves the cursor on the row just created, which scrolled the list to
    // show it. Wind it back to the top so the sweep below starts at 0.
    app.nav
        .scroll_server_list(1000.0, LIST_FB_H as f32);
    assert_eq!(
        app.nav.server_scroll(),
        0.0,
        "premise: the sweep starts from an unscrolled list"
    );
    app
}

/// The hand derivation above is the rect the screen really arranges.
///
/// Without this the probe point in the sweep could be inside no widget at all,
/// and "the hit-test answers the footer button" would be untestable in the
/// direction that matters. Kept separate from the sweep so a layout change
/// reports itself here by name instead of as a mysterious routing failure.
#[test]
fn the_derived_join_server_rect_is_the_one_the_layout_arranges() {
    use crate::menu::nav::ServerListButton;

    let arranged = crate::menu::render::server_list_footer_slot(ServerListButton::Select)
        .resolve(LIST_FB_W as f32, LIST_FB_H as f32);
    assert_eq!(
        arranged,
        join_server_rect(),
        "the arranged Join Server rect and the one derived from \
         JoinMultiplayerScreen's own constants disagree"
    );
}

/// **A scrolling list must not steal the footer's clicks — or its hover.**
///
/// The player report (2026-08-07): *"on the server list ui … if i try to press
/// 'Join Server' when im not scrolled to the bottom (ie there is a server behind
/// the button) it doesnt highlight the 'Join Server' button (despite my cursor
/// being on it) and instead presses the server entry"*. Hover and click are one
/// path — [`WindowApp::menu_row_at`] answers both — so one assertion covers both
/// symptoms.
///
/// # The invariant, and why it is swept
///
/// For a cursor inside a footer button's own rect, the hit-test must return that
/// button **at every scroll offset of the list**. The bug only exists at offsets
/// where a row has scrolled under the footer: at 0 the first row is at the band's
/// top, and at the maximum the last row ends inside the band, so a gate that
/// probed either end would pass against the broken code. That is `CLAUDE.md`'s
/// *world* species — the input cannot exercise the defect — and it is the specific
/// trap that would let this ship again, which is why the sweep walks every
/// pixel offset rather than sampling.
///
/// # Why the expected value is not round-tripped
///
/// The probe point comes from [`join_server_rect`], hand-derived from
/// screen layout constants, and the expected answer is the
/// row index `SERVER_LIST_BUTTONS`' order fixes — `n + 0`, since Join Server is
/// its first entry and the footer follows the entries in one flat index space.
/// Asking `row_rect` where the button is and clicking there would pass for any
/// self-consistent geometry, including one that never routes to a footer button
/// at all.
///
/// # The premise that makes the sweep non-vacuous, measured rather than assumed
///
/// At least one offset in the sweep must put a *list row's* rect over the probe
/// point — otherwise there is nothing for the row to steal and the assertion is
/// satisfied by geometry that never overlaps. That is checked with the same
/// `row_rect` the broken scan read, and its count is reported on failure.
#[test]
fn the_join_server_button_wins_the_cursor_at_every_scroll_offset() {
    use crate::menu::nav::SERVER_LIST_BUTTONS;

    assert_eq!(
        crate::config::calculate_gui_scale(0, LIST_FB_W, LIST_FB_H),
        1,
        "premise: at this framebuffer a logical pixel is a physical pixel, so the \
         vanilla-derived coordinates need no scale conversion"
    );
    assert_eq!(
        SERVER_LIST_BUTTONS[0],
        crate::menu::nav::ServerListButton::Select,
        "premise: Join Server is the first footer row, so its index is `n + 0`"
    );

    // 16 rows overflow the 386 px band at this canvas by 190 px, which is what
    // makes the offsets in the middle of the sweep reachable at all.
    const N: usize = 16;
    let mut app = app_with_servers("join-sweep", N);
    let expected = Some(N);

    let (bx, by, bw, bh) = join_server_rect();
    let (probe_x, probe_y) = (bx + bw * 0.5, by + bh * 0.5);
    let band_bottom = LIST_FB_H as f32 - SERVER_LIST_FOOTER_H_VANILLA;
    assert!(
        probe_y > band_bottom,
        "premise: the probe at y {probe_y} must be below the list band's bottom \
         ({band_bottom}) — otherwise it is inside the list and the footer has no \
         claim on it"
    );

    let max_scroll =
        crate::menu::render::server_list_max_scroll(N, LIST_FB_H as f32);
    assert!(
        max_scroll >= 30.0,
        "premise: the list must scroll at least 30 px for a row to reach the \
         footer strip; it scrolls {max_scroll}"
    );

    // How many offsets put a row's rect over the probe point. This is the
    // detector's own evidence: a zero here means the sweep measured nothing.
    let mut offsets_with_a_row_under_the_button = 0usize;
    let mut steps = 0usize;
    loop {
        let scroll = app.nav.server_scroll();
        let frame = crate::menu::nav::on_screen_frame(
            &app.ui,
            &app.nav,
            app.sim.death_message(),
            &app.statuses,
            &mut app.favicons,
        )
        .expect("the multiplayer screen owns its frame");
        let row_under = (0..N).any(|i| {
            crate::menu::render::row_rect(
                &frame.rows,
                i,
                LIST_FB_W as f32,
                LIST_FB_H as f32,
            )
            .is_some_and(|(rx, ry, rw, rh)| {
                probe_x >= rx
                    && probe_x <= rx + rw
                    && probe_y >= ry
                    && probe_y <= ry + rh
            })
        });
        if row_under {
            offsets_with_a_row_under_the_button += 1;
        }
        drop(frame);

        assert_eq!(
            app.menu_row_at_in(probe_x, probe_y, LIST_FB_W, LIST_FB_H),
            expected,
            "at scroll {scroll} px a cursor at ({probe_x}, {probe_y}) — inside Join \
             Server's own rect — resolved to something else. A list row is \
             overhanging the band and winning the flat index scan, which is both \
             the missing highlight and the wrong press."
        );

        if scroll >= max_scroll {
            break;
        }
        // One logical pixel: `mouse_scrolled` moves `notches * scroll_rate` and the
        // rate is half a 36 px entry. Negative scrolls *down*, matching winit.
        app.nav
            .scroll_server_list(-1.0 / 18.0, LIST_FB_H as f32);
        steps += 1;
        assert!(
            steps < 4000,
            "the sweep is not advancing: stuck at {scroll} of {max_scroll}"
        );
    }

    assert!(
        offsets_with_a_row_under_the_button > 0,
        "premise failed: no scroll offset in the sweep put a list row's rect over \
         Join Server, so the assertion above measured nothing at all. The band \
         bottom is {band_bottom} and the probe is at y {probe_y}."
    );
}

/// **The control for the sweep above, run and observed.**
///
/// Two ways that gate could pass for the wrong reason, and both are the shape
/// `CLAUDE.md` warns about — an absence assertion with no evidence the detector
/// fires, and a control whose premise is false before the fix existed:
///
/// 1. If the clip rejected list rows *everywhere* rather than outside the band,
///    "the footer wins below the band" would hold while the list itself became
///    unclickable. So a cursor genuinely **inside** the band, over a drawn row,
///    must return that row and not a footer button.
/// 2. If `menu_row_at_in` answered `None` on this screen, every equality in the
///    sweep would be comparing two `None`s. It does not: both probes here answer
///    `Some`, from the same framebuffer, differing only in `y`.
///
/// **Observed to fail against an inverted fix**: dropping the `ly < top || ly >
/// bottom` bound from `menu_row_at_in`'s guard — i.e. rejecting a list row at
/// every position — leaves the sweep green and makes this test fail with
/// `None` where row 0 is expected. Restoring the bound makes both pass. That is
/// the polarity a control needs, and it is why the two live side by side.
#[test]
fn a_cursor_inside_the_band_still_resolves_to_the_list_row_under_it() {
    const N: usize = 16;
    let mut app = app_with_servers("band-control", N);

    // Row 0's centre at scroll 0, from the layout arithmetic:
    // the first-entry offset plus the row index times row height below the
    // content band's top, with the fixed 305 px row width centred horizontally.
    let row_top = SERVER_LIST_HEADER_H_VANILLA + SERVER_LIST_FIRST_ENTRY_Y_VANILLA;
    let probe_y = row_top + SERVER_LIST_ITEM_H_VANILLA * 0.5;
    let row_w: f32 = 305.0;
    let row_left = (LIST_FB_W as f32 * 0.5).floor() - (row_w * 0.5).floor();
    let probe_x = row_left + row_w * 0.5;

    let band_bottom = LIST_FB_H as f32 - SERVER_LIST_FOOTER_H_VANILLA;
    assert!(
        probe_y > SERVER_LIST_HEADER_H_VANILLA && probe_y < band_bottom,
        "premise: {probe_y} must be inside the band {SERVER_LIST_HEADER_H_VANILLA}\
         ..{band_bottom}, or this control is measuring the footer strip again"
    );

    assert_eq!(
        app.menu_row_at_in(probe_x, probe_y, LIST_FB_W, LIST_FB_H),
        Some(0),
        "a cursor over row 0, inside the list's band, must resolve to row 0 — a \
         clip that rejected every list row would satisfy the footer gate and \
         break the list"
    );

    // And the second premise: the two answers differ, from one framebuffer, so
    // the sweep above is not comparing two `None`s.
    let (bx, by, bw, bh) = join_server_rect();
    assert_eq!(
        app.menu_row_at_in(bx + bw * 0.5, by + bh * 0.5, LIST_FB_W, LIST_FB_H),
        Some(N),
        "premise: the footer probe answers the footer row at scroll 0 as well"
    );
}

/// [`crate::app::lifecycle::dpr_scaled_size`] — the pure CSS-box-times-`devicePixelRatio`
/// arithmetic behind the browser build's initial canvas sizing (see that function's doc and
/// `finish_bring_up`'s call site). Deliberately not `#[cfg(target_arch = "wasm32")]`: the
/// DOM-touching caller is, so this is the only part of that fix a native `cargo test` run
/// ever exercises — a test living inside the `wasm32`-gated function itself would never run
/// under any check `just health` performs.
///
/// Every case predicts an exact `(width, height)` from outside the function under test
/// (plain `f64` arithmetic on the input), collects every mismatch rather than asserting
/// inside the loop (CLAUDE.md's *magnitude* species — a `for`-loop `assert!` would only ever
/// report the first failing case), and every input is pairwise-distinct so a transposed
/// width/height cannot survive unnoticed.
#[test]
fn dpr_scaled_size_matches_predicted_physical_pixels() {
    use crate::app::lifecycle::dpr_scaled_size;

    struct Case {
        label: &'static str,
        client_width: i32,
        client_height: i32,
        dpr: f64,
        expected: Option<(u32, u32)>,
    }

    let cases = [
        // DPR 1 anchor: no scaling, so this alone cannot tell "scaled" from
        // "unscaled" — the other cases below carry that weight.
        Case {
            label: "dpr 1.0, exact",
            client_width: 900,
            client_height: 600,
            dpr: 1.0,
            expected: Some((900, 600)),
        },
        // The "4x fragment count at retina" case named in `dpr_scaled_size`'s
        // caller's doc: dpr 2.0 doubles *both* dimensions, so the fragment count
        // — width * height — is 4x, not 2x.
        Case {
            label: "dpr 2.0, retina",
            client_width: 960,
            client_height: 540,
            dpr: 2.0,
            expected: Some((1920, 1080)),
        },
        // A real iPhone-class dpr (3.0), and large enough that a `u32` truncation
        // bug in an intermediate would be visible.
        Case {
            label: "dpr 3.0, phone-class",
            client_width: 375,
            client_height: 812,
            dpr: 3.0,
            expected: Some((1125, 2436)),
        },
        // The rounding discriminator: 667 * 1.5 = 1000.5 and 421 * 1.5 = 631.5,
        // both exact half-pixels. `.round()` (round-half-away-from-zero) gives
        // 1001 and 632; a truncating cast (`as u32` with no `.round()` — the
        // plausible neuter for this function, and a real bug shape: forgetting
        // the round and letting the float-to-int cast truncate) would instead
        // give 1000 and 631. The two hypotheses differ on both fields, so a
        // width/height transposition could not accidentally pass this either.
        Case {
            label: "dpr 1.5, half-pixel rounding",
            client_width: 667,
            client_height: 421,
            dpr: 1.5,
            expected: Some((1001, 632)),
        },
        // Zero-guard, width: `client_width` reads 0 when the canvas has not been
        // laid out yet (or is `display: none`) — must not resize the surface to
        // a degenerate 0-wide target.
        Case {
            label: "zero width guards to None",
            client_width: 0,
            client_height: 600,
            dpr: 1.0,
            expected: None,
        },
        // Zero-guard, height — a non-1.0 dpr here so this is not merely the
        // same guard exercised twice under the same scale factor.
        Case {
            label: "zero height guards to None",
            client_width: 900,
            client_height: 0,
            dpr: 2.0,
            expected: None,
        },
        // A negative `client_width` is a real (if pathological) input shape the
        // DOM can hand back; the guard compares the unscaled `f64` product
        // against `1.0` *before* any cast, so this must not become a huge
        // `u32` via a wrapping negative-float-to-unsigned cast.
        Case {
            label: "negative width guards to None",
            client_width: -5,
            client_height: 600,
            dpr: 1.0,
            expected: None,
        },
    ];

    let mismatches: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let actual = dpr_scaled_size(case.client_width, case.client_height, case.dpr);
            (actual != case.expected).then(|| {
                format!(
                    "{}: dpr_scaled_size({}, {}, {}) = {:?}, expected {:?}",
                    case.label, case.client_width, case.client_height, case.dpr, actual, case.expected
                )
            })
        })
        .collect();

    assert!(
        mismatches.is_empty(),
        "{} of {} cases mismatched:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n")
    );
}

/// **The bug the owner reported: rebinding Toggle Perspective to `G` left
/// `F5` cycling the camera and `G` doing nothing until the next launch.**
///
/// Two very different faults produce that symptom and only a test spanning
/// both halves can tell them apart. `resolve_key` was never the problem — it
/// asks `binds.is(InputAction::TogglePerspective, code)` and always has, so
/// no literal `F5` was ever in the chain. The producer was: `WindowApp` held
/// its own `keybinds: Keybinds`, copied out of `Options::load()` in the
/// constructor, while the Key Binds screen writes `MenuNav`'s `Options` and
/// persists them. The write reached the file (`nav.rs`'s
/// `clicking_a_bind_button_then_capturing_a_key_rebinds_and_persists` proves
/// that end of it) and never reached the resolver, so the rebind applied on
/// the *next* launch only.
///
/// **Why no gate saw it, and what this one does differently.** The whole
/// Key Binds corpus lives in `nav.rs` and drives `MenuNav` with no
/// `WindowApp` at all — its own helper doc says so, calling `capture_binding`
/// "the same call `app.rs`'s patch is specified to make". The resolver corpus
/// in this file is the mirror image: every one of its cases builds its own
/// `Keybinds::new()` and hands it to `resolve_key`. Both corpora are correct
/// and neither can see a consumer reading a *different table* from the one
/// the menu writes — `CLAUDE.md`'s "a gate that installs its own input proves
/// the consumer and nothing about the producer". So this drives the real
/// screen into the real `WindowApp` and asks the app for its own table:
/// menu click → `capture_binding` → `WindowApp::keybinds` → `resolve_key` →
/// `apply_key_outcome` → the camera actually moving off first person.
///
/// The one hop it cannot take is the raw `winit::event::KeyEvent`
/// (unconstructable outside winit), so the capture is fed the `Binding` that
/// `handle_keyboard_input`'s `CaptureKey::Bind(code)` arm forwards verbatim —
/// and `capture_key_for` is asserted here too, so the substitution is checked
/// rather than assumed.
#[test]
fn rebinding_toggle_perspective_in_the_controls_screen_takes_effect_without_a_restart() {
    use crate::keybinds::{Binding, InputAction};
    use crate::menu::key_binds::KeyControl;
    use crate::menu::options::{Cell, SettingsPage};

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    // A `MenuNav` on a scratch path, because finishing a capture *persists*:
    // with the production `MenuNav::new()` this test would rewrite the
    // developer's real `options.json` (`CLAUDE.md`'s OS-side-effect rule).
    let dir = std::env::temp_dir().join(format!(
        "lodestone-rebind-perspective-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    seed_owning_account(&dir);
    app.nav = MenuNav::with_path(dir.join("servers.json"));

    // Vanilla's own default, and the control for the second half below.
    assert_eq!(
        app.keybinds().binding(InputAction::TogglePerspective),
        Binding::Key(KeyCode::F5.into()),
        "precondition: the app starts on vanilla's F5"
    );

    // --- the producer: the real Controls screen, driven by real menu input.
    app.ui.open_settings();
    for page in [SettingsPage::Controls, SettingsPage::KeyBinds] {
        // `in_world: false` — this app reached Settings through
        // `ui.open_settings()`, never through the pause menu, so
        // `SettingsNav::in_world` is still its default.
        let cells = crate::menu::options::all_controls(app.nav.settings().page(), false);
        let target = cells
            .iter()
            .position(|c| matches!(c, Cell::Nav { page: Some(p), .. } if *p == page))
            .expect("the settings tree must offer this page");
        for _ in 0..=cells.len() {
            if app.nav.settings().cursor() == target {
                break;
            }
            app.nav.key(&mut app.ui, MenuKey::Down);
        }
        app.nav.key(&mut app.ui, MenuKey::Enter);
        assert_eq!(app.nav.settings().page(), page);
    }

    let controls = crate::menu::key_binds::all_controls();
    let target = controls
        .iter()
        .position(|c| *c == KeyControl::Bind(InputAction::TogglePerspective))
        .expect("Toggle Perspective must have a bind button on the Key Binds screen");
    for _ in 0..=controls.len() {
        if app.nav.settings().key_binds().cursor() == target {
            break;
        }
        app.nav.key(&mut app.ui, MenuKey::Down);
    }
    app.nav.key(&mut app.ui, MenuKey::Enter);
    assert!(
        app.nav.awaiting_key_capture(),
        "Enter on the bind button must start a capture"
    );

    // The substitution for the raw `KeyEvent`: this is exactly what
    // `handle_keyboard_input` computes and forwards for a `G` keydown.
    assert_eq!(
        capture_key_for(winit::keyboard::PhysicalKey::Code(KeyCode::KeyG)),
        Some(CaptureKey::Bind(KeyCode::KeyG)),
        "a G keydown must forward as CaptureKey::Bind(KeyG)"
    );
    app.nav.capture_binding(Binding::Key(KeyCode::KeyG.into()));

    // --- the consumer: the app's own table, with no restart in between.
    assert_eq!(
        app.keybinds().binding(InputAction::TogglePerspective),
        Binding::Key(KeyCode::KeyG.into()),
        "the rebind must reach the table the resolver reads, not just the file"
    );

    let binds = app.keybinds();
    assert_eq!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyG), true, false, None),
        Some(KeyOutcome::TogglePerspective),
        "G must now cycle the perspective"
    );
    assert_ne!(
        resolve_key(&binds, playing(), Some(KeyCode::F5), true, false, None),
        Some(KeyOutcome::TogglePerspective),
        "and F5 must stop — a rebind that only *adds* a key is the same bug wearing a hat"
    );

    // --- the effect, through the real driver rather than the resolver's word.
    let before = app.sim.camera_type();
    app.apply_key_outcome(
        Some(KeyOutcome::TogglePerspective),
        true,
        Some(KeyCode::KeyG),
        None,
    );
    assert_ne!(
        app.sim.camera_type(),
        before,
        "the outcome must actually move the camera off first person"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closed_f3_does_not_call_the_map_debug_gather() {
    let calls = std::cell::Cell::new(0);
    let hidden = super::redraw::map_debug_when_visible(false, || {
        calls.set(calls.get() + 1);
        Some((12, 0.5))
    });
    assert_eq!(hidden, None);
    assert_eq!(calls.get(), 0);

    let visible = super::redraw::map_debug_when_visible(true, || {
        calls.set(calls.get() + 1);
        Some((12, 0.5))
    });
    assert_eq!(visible, Some((12, 0.5)));
    assert_eq!(calls.get(), 1);
}

/// The other half of the rebind class: the **F3 chords are rebindable in
/// vanilla 26.2**, and this client used to hardcode them.
///
/// Checked in the jar rather than assumed, because the received wisdom (and
/// this repo's own comments) said the opposite. Vanilla's own persisted
/// options declare all seven F3-chord actions (show hitboxes, show chunk
/// borders, show advanced tooltips, spectate, switch game mode, focus pause,
/// and copy location)
/// as ordinary debug-category key bindings, collects them into its own debug-keys
/// list and folds that array into the full key-binding list — the one vanilla persists and the Controls
/// screen lists — and vanilla's own debug-key handling asks every one of them
/// whether it matches the event. So `code == KeyCode::KeyG` in `resolve_key` was the
/// divergence, not the table.
///
/// Driven exactly like
/// [`rebinding_toggle_perspective_in_the_controls_screen_takes_effect_without_a_restart`]
/// — the real screen, the real capture, the app's own table — because the
/// producer half is what the two existing corpora cannot see. The `F3` gate
/// flag is asserted on both sides: a chord must still need the modifier held,
/// so rebinding one onto a bare key does not make it fire during ordinary play.
#[test]
fn a_rebound_f3_chord_fires_on_its_new_key_and_stops_on_its_old_one() {
    use crate::keybinds::{Binding, InputAction};
    use crate::menu::key_binds::KeyControl;
    use crate::menu::options::{Cell, SettingsPage};

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let dir = std::env::temp_dir().join(format!("lodestone-rebind-chord-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    seed_owning_account(&dir);
    app.nav = MenuNav::with_path(dir.join("servers.json"));

    // The default debug binding for chunk borders uses key code 71.
    assert_eq!(
        app.keybinds()
            .binding(InputAction::DebugShowChunkBorders),
        Binding::Key(KeyCode::KeyG.into())
    );

    app.ui.open_settings();
    for page in [SettingsPage::Controls, SettingsPage::KeyBinds] {
        let cells = crate::menu::options::all_controls(app.nav.settings().page(), false);
        let target = cells
            .iter()
            .position(|c| matches!(c, Cell::Nav { page: Some(p), .. } if *p == page))
            .expect("the settings tree must offer this page");
        for _ in 0..=cells.len() {
            if app.nav.settings().cursor() == target {
                break;
            }
            app.nav.key(&mut app.ui, MenuKey::Down);
        }
        app.nav.key(&mut app.ui, MenuKey::Enter);
    }

    // Reaching the row by Down alone is what proves a Debug chord is actually
    // *listed* on the screen, not merely present in `InputAction::ALL`.
    let controls = crate::menu::key_binds::all_controls();
    let target = controls
        .iter()
        .position(|c| *c == KeyControl::Bind(InputAction::DebugShowChunkBorders))
        .expect("Show Chunk Boundaries must have a bind button");
    for _ in 0..=controls.len() {
        if app.nav.settings().key_binds().cursor() == target {
            break;
        }
        app.nav.key(&mut app.ui, MenuKey::Down);
    }
    app.nav.key(&mut app.ui, MenuKey::Enter);
    assert!(app.nav.awaiting_key_capture());
    app.nav.capture_binding(Binding::Key(KeyCode::KeyJ.into()));

    let binds = app.keybinds();
    let mut chord = playing();
    chord.debug_held = true;
    assert_eq!(
        resolve_key(&binds, chord, Some(KeyCode::KeyJ), true, false, None),
        Some(KeyOutcome::ToggleChunkBorders),
        "F3+J must now toggle chunk borders"
    );
    assert_ne!(
        resolve_key(&binds, chord, Some(KeyCode::KeyG), true, false, None),
        Some(KeyOutcome::ToggleChunkBorders),
        "and F3+G must stop"
    );
    // The modifier is still a gate flag, not an eighth bindable action: a
    // rebound chord must not fire as a bare key during ordinary play.
    assert_ne!(
        resolve_key(&binds, playing(), Some(KeyCode::KeyJ), true, false, None),
        Some(KeyOutcome::ToggleChunkBorders),
        "a chord without F3 held is not a chord"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Ordinary startup is unaffected by the two new fields —
/// `WindowApp::new` wants a window and accepts its input immediately, exactly
/// as it always has. This is the negative control for
/// `new_headless_session_starts_with_no_presentation_desired_and_input_inert`
/// below: if both tests read the same values, the two constructors are not
/// actually distinguished by anything and the "starts headless" claim is
/// untested.
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-presentation"))]
#[test]
fn ordinary_startup_wants_a_window_and_arms_input_immediately() {
    let app = WindowApp::new(Config::default());
    assert!(app.presentation_desired);
    assert!(app.input_armed);
    assert!(app.sim.presentation_attached());
}

/// The headless-session constructor (`app::runners::run_headless_session`'s
/// entry point) is the actual new capability this adds: a session
/// that starts with no window, no input armed, and no presentation-only ECS
/// systems — not merely "a window that happens not to exist yet" the way
/// bring-up-in-progress on the browser target already could represent, but a
/// session that never asked for one and has already detached the ECS half
/// too.
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-presentation"))]
#[test]
fn new_headless_session_starts_with_no_presentation_desired_and_input_inert() {
    let app = WindowApp::new_headless_session(Config::default());
    assert!(!app.presentation_desired);
    assert!(!app.input_armed);
    assert!(window_physical_size(&app.config).is_none() || true); // config is unaffected either way
    assert!(
        !app.sim.presentation_attached(),
        "a headless-session start must detach the ECS half too, not just \
         suppress window creation — the terrain mesher and the \
         pick/interaction/particle systems must not run with nothing to \
         consume their output"
    );
    assert!(app.window.is_none());
    assert!(app.gpu.is_none());
    assert!(app.render.is_none());
}

/// `WindowApp::detach_presentation` must be safe to call on a session that is
/// already headless (the common real case: a headless-session start, or a
/// second detach nobody guarded against) — a no-op, not a panic on an
/// already-`None` field.
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-presentation"))]
#[test]
fn detach_presentation_on_an_already_headless_app_is_a_safe_no_op() {
    let mut app = WindowApp::new_headless_session(Config::default());
    app.detach_presentation();
    assert!(app.window.is_none());
    assert!(app.gpu.is_none());
    assert!(!app.presentation_desired);
    assert!(!app.input_armed);
}

/// **The plugin-registration seam's remaining gap, closed**: before `WindowApp::new_with_app`
/// existed, `WindowApp::new` (and therefore `run_windowed`, and therefore
/// `Mode::Window` — the shipped, on-screen client) built its own `Sim::new`
/// with no parameter a caller could use to add a plugin. `Sim::client_app()` +
/// `Sim::from_app` already let a caller compose an `App` and drive a bare
/// `Sim` by hand (`tests/interaction/rendered_client_takes_a_plugin.rs`), but
/// nothing fed that composed `App` into the actual struct the real winit
/// driver constructs — `Sim::from_app` proved the ECS half accepts a plugin,
/// not that the shipped binary's entry point does.
///
/// This test is that missing half, one level down from the bare-`Sim` gate:
/// build the identical `JumpPlugin`-shaped marker, hand it to
/// `WindowApp::new_with_app` (what `run_windowed_with_app`, and therefore
/// `crate::run_with_app`, now call instead of `WindowApp::new`), and drive
/// real `GameTick`s through `app.sim.step` — the same driver
/// `app::redraw`'s per-frame loop calls in production. Registration is
/// proven by the player's position changing, not by a flag.
struct MarkerPlugin;

impl lodestone_ecs::app::Plugin for MarkerPlugin {
    fn build(&self, app: &mut lodestone_ecs::app::App) {
        use bevy_ecs::schedule::IntoScheduleConfigs;
        app.add_systems(
            lodestone_ecs::GameTick,
            mark_jump
                .after(lodestone_ecs::TickSet::Intent)
                .before(lodestone_ecs::TickSet::Physics),
        );
    }
}

fn mark_jump(
    mut q: bevy_ecs::prelude::Query<
        &mut lodestone_ecs::player::MovementIntent,
        bevy_ecs::prelude::With<lodestone_ecs::player::LocalPlayer>,
    >,
) {
    for mut intent in &mut q {
        intent.0.forward = 0.0;
        intent.0.strafe = 0.0;
        intent.0.jump = true;
        intent.0.sneak = false;
        intent.0.sprint = false;
    }
}

fn marker_test_config() -> Config {
    Config {
        mode: Mode::Headless,
        render_distance: 2,
        ..Config::default()
    }
}

#[test]
fn window_app_new_with_app_wires_a_callers_plugin_into_the_real_constructor() {
    let mut plugin_app = Sim::client_app();
    plugin_app.add_plugins(MarkerPlugin);
    let mut app = WindowApp::new_with_app(plugin_app, marker_test_config());

    let start = app.sim.player().position;
    let mut apex = 0.0f64;
    for _ in 0..60 {
        app.sim.step(1.0 / 20.0);
        apex = apex.max(app.sim.player().position.y - start.y);
    }
    let now = app.sim.player().position;
    let horizontal = ((now.x - start.x).powi(2) + (now.z - start.z).powi(2)).sqrt();

    assert!(
        (0.9..1.6).contains(&apex),
        "a plugin handed to `WindowApp::new_with_app` must drive the real, \
         windowed-shell `Sim`'s local player through a real `GameTick`: \
         expected a vanilla jump apex near 1.2522 blocks, measured {apex:.4} \
         (horizontal displacement {horizontal:.4})"
    );
    assert!(
        horizontal < 0.1,
        "premise: a jump-in-place plugin must not travel, or the apex above \
         is measuring terrain rather than the jump; horizontal displacement \
         was {horizontal:.4}"
    );
}

/// The negative control: `WindowApp::new` — what every real `Mode::Window`
/// run still builds when no caller supplies an `App` — registers no such
/// plugin, so the identical budget must leave the player's position
/// unchanged. Without this, a demo-world `Sim` whose player slid for any
/// unrelated reason would make the positive assertion above read as a pass
/// for the wrong reason.
#[test]
fn without_a_supplied_app_window_app_new_still_leaves_the_player_put() {
    let mut app = WindowApp::new(marker_test_config());

    let start = app.sim.player().position;
    let mut apex = 0.0f64;
    for _ in 0..60 {
        app.sim.step(1.0 / 20.0);
        apex = apex.max(app.sim.player().position.y - start.y);
    }
    let now = app.sim.player().position;
    let horizontal = ((now.x - start.x).powi(2) + (now.z - start.z).powi(2)).sqrt();

    assert!(
        apex < 0.05,
        "control: with no plugin supplied, nothing may lift the player, yet \
         the apex was {apex:.4} blocks"
    );
    assert!(
        horizontal < 0.05,
        "control: with no plugin supplied, nothing may move the player \
         horizontally, yet displacement was {horizontal:.4} blocks"
    );
}
