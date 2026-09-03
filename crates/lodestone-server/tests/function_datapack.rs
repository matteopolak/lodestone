//! `/function` and `/reload` driven end to end: a
//! real `.mcfunction` file under a real datapack directory on disk, loaded
//! by [`FunctionHandle::load_from`] the same way `IntegratedServer`'s own
//! persistent-world constructor does, dispatched through the real
//! [`ServerCommands::run`] — the production entry point `crate::server`'s
//! `ChatCommand` arm and RCON both call — not a unit test calling the parser
//! or the loader directly.
//!
//! Each command line inside the function body reaches the *same* tree this
//! file's own top-level `/function`/`/reload` sit on
//! (`registrar::Ctx::run_command`), so a `/gamerule` line inside a function
//! is observed the identical way a player typing it would be: through
//! [`lodestone_server::world_state::WorldStateHandle::rules`].

use lodestone_model::{Rotation, Vec3};
use lodestone_server::commands::function_store::DatapackLoadReport;
use lodestone_server::commands::registrar::RuleStore;
use lodestone_server::commands::{CommandSource, CommandWorld, ServerCommands, overworld_dimension};
use lodestone_server::world_state::WorldStateHandle;
use lodestone_server::{CommandOutcome, CommandResponse};

/// A unique scratch world directory per test, so concurrent test runs never
/// collide on the same path — the same shape
/// `crate::commands::function_store`'s own unit tests already use, restated
/// here because this crate's integration tests cannot see that module's
/// private helper.
fn scratch_world(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("lodestone-function-datapack-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &std::path::Path, path: &str, contents: &str) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, contents).unwrap();
}

/// The caller: alice, level 4 — same shape `tests/builtin_commands.rs`'s own
/// `source` fixture takes.
fn source() -> CommandSource {
    CommandSource::player(
        uuid::Uuid::from_u128(1),
        1001,
        "alice",
        Vec3::new(0.0, 64.0, 0.0),
        Rotation { yaw: 0.0, pitch: 0.0 },
        overworld_dimension(),
        4,
    )
}

fn run(commands: &ServerCommands, state: &WorldStateHandle, text: &str) -> Option<CommandOutcome> {
    let world = CommandWorld {
        rules: state as &(dyn RuleStore + Sync),
        players: &[],
        state,
        mobs: None,
        border: None,
        access: None,
        blocks: None,
    };
    commands.run(&world, &source(), text)
}

fn feedback(outcome: &CommandOutcome) -> &[String] {
    match &outcome.response {
        CommandResponse::Ran { feedback } => feedback,
        CommandResponse::Refused { message } => panic!("expected the command to run, refused: {message}"),
    }
}

/// The end-to-end case the issue asks for: a real `.mcfunction` file, under
/// a real `datapacks/` directory, loaded from disk and executed through the
/// real dispatcher — not a hand-built in-memory fixture.
#[test]
fn a_real_mcfunction_file_runs_its_lines_through_the_real_dispatcher() {
    let world_dir = scratch_world("basic");
    write(
        &world_dir,
        "datapacks/pack/data/test/function/setup.mcfunction",
        "# a comment, stripped\ngamerule advance_time false\ngamerule advance_weather false\n",
    );

    let state = WorldStateHandle::new();
    assert_eq!(state.functions().load_from(&world_dir), DatapackLoadReport { functions: 1, tags: 0 });

    // Control: before running the function, the rule is still at its vanilla
    // default (`true`) — proving the assertion below observes the
    // function's own effect rather than a rule that was already this value.
    assert_eq!(
        state.rules().get("advance_time").map(|v| v.serialize()),
        Some("true".to_string())
    );

    let commands = ServerCommands::new();
    let outcome = run(&commands, &state, "function test:setup").expect("/function is a built-in root");
    let lines = feedback(&outcome);
    assert!(
        lines.iter().any(|l| l.contains("Ran 2 command(s)")),
        "expected a summary line reporting both lines ran, got {lines:?}"
    );

    assert_eq!(
        state.rules().get("advance_time").map(|v| v.serialize()),
        Some("false".to_string()),
        "the function's own `gamerule` line must have reached the real rule store"
    );
    assert_eq!(state.rules().get("advance_weather").map(|v| v.serialize()), Some("false".to_string()));
}

/// An unknown single function is a hard refusal — vanilla's own
/// `FunctionArgument.ERROR_UNKNOWN_FUNCTION`.
#[test]
fn an_unknown_single_function_is_refused() {
    let world_dir = scratch_world("unknown-single");
    let state = WorldStateHandle::new();
    state.functions().load_from(&world_dir);
    let commands = ServerCommands::new();
    let outcome = run(&commands, &state, "function test:does_not_exist").unwrap();
    assert!(matches!(outcome.response, CommandResponse::Refused { .. }));
}

/// The asymmetric control: an unknown *tag*, unlike an unknown single
/// function, is not an error at all — matching vanilla's own
/// `getTag(...).getOrDefault(tag, List.of())`.
#[test]
fn an_unknown_tag_runs_as_a_no_op_rather_than_refusing() {
    let world_dir = scratch_world("unknown-tag");
    let state = WorldStateHandle::new();
    state.functions().load_from(&world_dir);
    let commands = ServerCommands::new();
    let outcome = run(&commands, &state, "function #test:nothing").unwrap();
    let lines = feedback(&outcome);
    assert!(lines.iter().any(|l| l.contains("Ran 0 command(s)")), "{lines:?}");
}

/// `#tag` runs every function it names, real files each.
#[test]
fn a_tag_runs_every_member_function() {
    let world_dir = scratch_world("tag-members");
    write(&world_dir, "datapacks/pack/data/test/function/a.mcfunction", "gamerule raids false\n");
    write(&world_dir, "datapacks/pack/data/test/function/b.mcfunction", "gamerule mob_drops false\n");
    write(
        &world_dir,
        "datapacks/pack/data/test/tags/function/both.json",
        r##"{"values": ["test:a", "test:b"]}"##,
    );
    let state = WorldStateHandle::new();
    state.functions().load_from(&world_dir);
    let commands = ServerCommands::new();
    let outcome = run(&commands, &state, "function #test:both").unwrap();
    let lines = feedback(&outcome);
    assert!(lines.iter().any(|l| l.contains("Ran 2 command(s)")), "{lines:?}");
    assert_eq!(state.rules().get("raids").map(|v| v.serialize()), Some("false".to_string()));
    assert_eq!(state.rules().get("mob_drops").map(|v| v.serialize()), Some("false".to_string()));
}

/// A self-referencing function is refused by the depth guard rather than
/// overflowing the real call stack — the control that proves the guard
/// actually engages, not merely that it compiles.
///
/// A refused nested command's own message is not itself folded into the
/// parent's feedback (`Ctx::run_command`'s `Refused` arm returns `Err`
/// without touching `self.feedback` — the same "a line's own failure is
/// silent, not logged to the caller" rule vanilla's own function execution
/// takes), so the guard's exact firing point has to be read off the shape
/// of the accumulated summary lines instead: every nesting level up to and
/// including the one whose *own* nested call was refused reports "Ran 0
/// command(s)" for that one line failing, and every shallower level reports
/// "Ran 1" once its own single (successful, from its point of view) nested
/// call returns. With `MAX_FUNCTION_DEPTH` levels able to *enter* before the
/// guard refuses entry to the next one, that is exactly one "Ran 0" followed
/// by `MAX_FUNCTION_DEPTH - 1` "Ran 1"s — a discriminating count a silently
/// uncapped recursion could never produce (it would not return at all, and
/// would eventually overflow the real stack instead).
#[test]
fn a_self_referencing_function_is_refused_by_the_depth_guard_instead_of_overflowing() {
    const MAX_FUNCTION_DEPTH: usize = 256;

    let world_dir = scratch_world("self-recursive");
    write(&world_dir, "datapacks/pack/data/test/function/loop.mcfunction", "function test:loop\n");
    let state = WorldStateHandle::new();
    state.functions().load_from(&world_dir);
    let commands = ServerCommands::new();
    let outcome = run(&commands, &state, "function test:loop").unwrap();
    let lines = feedback(&outcome);

    assert_eq!(lines.len(), MAX_FUNCTION_DEPTH, "one summary line per level that was able to enter: {lines:?}");
    assert_eq!(lines[0], "Ran 0 command(s) from function", "the deepest level's own line must be the one that failed");
    assert!(
        lines[1..].iter().all(|l| l == "Ran 1 command(s) from function"),
        "every shallower level's single nested call must have reported success: {lines:?}"
    );
}

/// `/reload` genuinely re-reads the directory — a function added to disk
/// after the world opened is invisible until `/reload` runs, and reachable
/// immediately afterward.
#[test]
fn reload_makes_a_newly_added_function_reachable() {
    let world_dir = scratch_world("reload-e2e");
    let state = WorldStateHandle::new();
    state.functions().load_from(&world_dir);
    let commands = ServerCommands::new();

    let before = run(&commands, &state, "function test:added_later").unwrap();
    assert!(matches!(before.response, CommandResponse::Refused { .. }), "must not exist yet");

    write(&world_dir, "datapacks/pack/data/test/function/added_later.mcfunction", "gamerule spawn_phantoms false\n");

    let reload_outcome = run(&commands, &state, "reload").unwrap();
    let reload_lines = feedback(&reload_outcome);
    assert!(reload_lines.iter().any(|l| l.contains("Reloaded 1 function")), "{reload_lines:?}");

    let after = run(&commands, &state, "function test:added_later").unwrap();
    let after_lines = feedback(&after);
    assert!(after_lines.iter().any(|l| l.contains("Ran 1 command(s)")), "{after_lines:?}");
    assert_eq!(state.rules().get("spawn_phantoms").map(|v| v.serialize()), Some("false".to_string()));
}

/// `/reload` against a world with no datapacks directory ever configured
/// (this crate's own test helper's default, and every in-memory/browser
/// world in production) is an honest no-op, not a refusal or a panic.
#[test]
fn reload_with_no_datapacks_configured_is_an_honest_no_op() {
    let state = WorldStateHandle::new();
    let commands = ServerCommands::new();
    let outcome = run(&commands, &state, "reload").unwrap();
    let lines = feedback(&outcome);
    assert!(lines.iter().any(|l| l.contains("No datapacks are configured")), "{lines:?}");
}
