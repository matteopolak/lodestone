//! The registry-driven gate for plugin command registration (#118), argument
//! types and tab completion (#119), and per-node permission checks (#122).
//!
//! # Why this file exists rather than more unit tests
//!
//! `crates/lodestone-ecs/src/commands.rs` has unit tests, and they are a
//! **closed loop**: every one of them constructs a `PluginCommand` or a
//! `CommandTree` directly and asserts something about it. All of them would
//! still pass if `CommandRegistry` were never inserted into any `World`, if
//! `PluginCommandsPlugin` did nothing, and if `dispatch` were never callable.
//! That is precisely the island this repo's `CLAUDE.md` names as its dominant
//! defect class.
//!
//! So every test here goes through the **public plugin path**, in the same
//! order a third-party crate would:
//!
//! 1. build a `bevy_app::App`,
//! 2. `add_plugins(TeleportPlugin)` — a plugin defined in this file that has no
//!    privileged access to anything, exactly like `crates/plugins/*`,
//! 3. let its `Plugin::build` reach `CommandRegistry` through the `World`,
//! 4. `dispatch` a **real input string**,
//! 5. assert a resource the handler mutated actually changed.
//!
//! A test that called the handler closure directly would prove nothing about
//! registration, which is the whole thing under test.
//!
//! # What this gate structurally cannot prove
//!
//! **That a player's typed `/command` reaches `dispatch`.** It does not, on
//! either side, and no test in this crate can show otherwise:
//! `lodestone-server` never decodes serverbound `CHAT_COMMAND` (id 7) — it falls
//! to `_ => ServerBound::Ignored` — and no protocol family encodes clientbound
//! `COMMANDS` (id 16), so no client is ever sent the tree. Both fixes live in
//! crates outside this work's ownership. Stated here rather than left implicit,
//! because a green file named "registry gate" is exactly the kind of thing that
//! gets mistaken for end-to-end coverage.

use std::sync::Arc;

use bevy_app::{App, Plugin};
use bevy_ecs::resource::Resource;
use lodestone_command::{IntegerArgument, ParseErrorKind, StringArgument};
use lodestone_ecs::commands::{
    CommandDispatchError, CommandOutcome, CommandRegistry, CommandSource, PlayerDirectory,
    PluginCommand, PluginCommandsPlugin, command_tree_for, dispatch, suggest,
};
use lodestone_ecs::permissions::{PermissionDefault, PermissionLevel, Permissions};
use lodestone_ecs::session::SessionTabList;
use lodestone_ecs::GameTick;
use uuid::Uuid;

/// The observable effect. A handler writes here; every assertion about "did the
/// command actually run" reads it. Deliberately a *world* resource rather than
/// a captured `Arc<AtomicUsize>`, so the thing being proven is that the handler
/// received the real `&mut World`.
#[derive(Resource, Default, Debug, PartialEq, Eq)]
struct TeleportLog {
    calls: usize,
    last_target: String,
    last_amount: i32,
}

/// A third-party-shaped plugin. Nothing here is privileged: it uses only the
/// public API a crate under `crates/plugins/` would have.
struct TeleportPlugin;

impl Plugin for TeleportPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PluginCommandsPlugin);
        app.init_resource::<TeleportLog>();

        let mut command = PluginCommand::new("warp");
        command.description("teleport around");
        command.alias("w");

        let root = command.root();

        // `/warp list` — ungated, everyone may use it. This is the node that
        // proves a gated sibling does not gate the whole command.
        let list = command.literal(root, "list");
        command.on_execute(list, |invocation| {
            invocation.world.resource_mut::<TeleportLog>().calls += 1;
            CommandOutcome::ok()
        });

        // `/warp admin …` — gated on `warp.admin`.
        let admin = command.literal(root, "admin");
        command.require_permission(admin, "warp.admin");

        // `/warp admin reload` — a permitted child *under* the gated parent, to
        // prove subtree pruning rather than per-node checking.
        let reload = command.literal(admin, "reload");
        command.on_execute(reload, |invocation| {
            let mut log = invocation.world.resource_mut::<TeleportLog>();
            log.calls += 1;
            log.last_target = "reload".to_string();
            CommandOutcome::ok()
        });

        // `/warp admin set <name> <amount>` — two argument types, one of them
        // bounded, so the gate exercises #119's parsing rather than only
        // literals.
        let set = command.literal(admin, "set");
        let name = command.argument(set, "name", Arc::new(StringArgument::word()));
        let amount = command.argument(name, "amount", Arc::new(IntegerArgument::bounded(1, 64)));
        command.on_execute(amount, |invocation| {
            let target = invocation.string("name").unwrap_or_default().to_string();
            let value = invocation.integer("amount").unwrap_or(-1);
            let mut log = invocation.world.resource_mut::<TeleportLog>();
            log.calls += 1;
            log.last_target = target;
            log.last_amount = value;
            CommandOutcome::Success(value)
        });

        app.world_mut()
            .resource_mut::<CommandRegistry>()
            .register(command)
            .expect("registering `warp` must succeed on a fresh registry");
    }
}

fn player_id() -> Uuid {
    Uuid::from_u128(0xABCD)
}

fn player() -> CommandSource {
    CommandSource::player(player_id(), "Tester")
}

/// An `App` with the plugin installed and a plain non-op player.
fn app() -> App {
    let mut app = App::new();
    app.add_plugins(TeleportPlugin);
    // Declare the gated node `False` so the *default* does not grant it. Without
    // this, Bukkit's `DEFAULT_PERMISSION` = `Op` would make an undeclared node
    // deny for a non-op anyway — but by accident rather than by declaration, and
    // a later change to the default would silently un-gate every test here.
    app.world_mut()
        .resource_mut::<Permissions>()
        .declare("warp.admin", PermissionDefault::False);
    app
}

// ---------------------------------------------------------------------------
// #118 — the registry actually dispatches
// ---------------------------------------------------------------------------

/// **The island gate.** A command registered through a plugin's `Plugin::build`
/// is reachable by dispatching a real input string, and its handler really
/// mutates the world.
#[test]
fn a_plugin_registered_command_dispatches_and_mutates_the_world() {
    let mut app = app();
    assert_eq!(app.world().resource::<TeleportLog>().calls, 0);

    let outcome = dispatch(app.world_mut(), &player(), "/warp list")
        .expect("an ungated subcommand must dispatch for a plain player");

    assert_eq!(outcome, CommandOutcome::Success(1));
    assert_eq!(
        app.world().resource::<TeleportLog>().calls,
        1,
        "the handler must have run against the real World"
    );
}

/// The negative control for the gate above: the *same* app, a well-formed input
/// naming a command nobody registered, leaves the counter untouched. Without
/// this, a handler that ran on every dispatch would pass the test above.
#[test]
fn an_unregistered_command_is_refused_and_runs_nothing() {
    let mut app = app();

    let error = dispatch(app.world_mut(), &player(), "/nosuchcommand foo")
        .expect_err("an unregistered root literal must not dispatch");

    assert_eq!(
        error,
        CommandDispatchError::UnknownCommand {
            name: "nosuchcommand".to_string()
        }
    );
    assert_eq!(error.message(), "Unknown or incomplete command");
    assert_eq!(
        app.world().resource::<TeleportLog>().calls,
        0,
        "nothing may have run"
    );
}

/// An alias resolves to the canonical command — the rewrite happens before
/// parsing, so the tree never needs the alias.
#[test]
fn an_alias_dispatches_to_the_canonical_command() {
    let mut app = app();
    dispatch(app.world_mut(), &player(), "/w list").expect("alias `w` must resolve to `warp`");
    assert_eq!(app.world().resource::<TeleportLog>().calls, 1);
}

/// Arguments reach the handler with their parsed values and types intact
/// (#119). The console is used because this path is gated on `warp.admin`.
#[test]
fn parsed_arguments_reach_the_handler_with_their_values() {
    let mut app = app();

    let outcome = dispatch(
        app.world_mut(),
        &CommandSource::console(),
        "/warp admin set home 42",
    )
    .expect("the console holds every permission");

    assert_eq!(outcome, CommandOutcome::Success(42));
    let log = app.world().resource::<TeleportLog>();
    assert_eq!(log.last_target, "home");
    assert_eq!(log.last_amount, 42);
}

/// The bound on the integer argument is enforced through the dispatch path, not
/// only in a unit test of the tree — and the failure is a *parse* error, not a
/// permission one, so the two error classes stay distinguishable.
#[test]
fn an_out_of_range_argument_fails_dispatch_without_running_the_handler() {
    let mut app = app();

    let error = dispatch(
        app.world_mut(),
        &CommandSource::console(),
        "/warp admin set home 999",
    )
    .expect_err("999 is outside the 1..=64 bound");

    assert!(
        matches!(
            error,
            CommandDispatchError::Parse(ref e)
                if matches!(e.kind, ParseErrorKind::IntegerTooHigh { max: 64, found: 999 })
        ),
        "expected an IntegerTooHigh parse error, got {error:?}"
    );
    assert!(
        !error.is_permission_denied(),
        "a range failure must not be reported as a permission denial"
    );
    assert_eq!(app.world().resource::<TeleportLog>().calls, 0);
}

// ---------------------------------------------------------------------------
// #122 — per-node permission gating, both halves
// ---------------------------------------------------------------------------

/// **The gating pair.** The identical input is refused before the grant and
/// succeeds after it, with the counter proving the handler did not run the first
/// time. One test rather than two, so the *only* difference between the two
/// outcomes is the grant.
#[test]
fn a_gated_branch_is_refused_until_the_permission_is_granted() {
    let mut app = app();

    let error = dispatch(app.world_mut(), &player(), "/warp admin reload")
        .expect_err("a plain player must not reach a gated branch");
    assert!(
        error.is_permission_denied(),
        "expected a permission denial, got {error:?}"
    );
    assert!(
        matches!(
            error,
            CommandDispatchError::Parse(ref e)
                if matches!(&e.kind, ParseErrorKind::NoPermission { permission } if permission == "warp.admin")
        ),
        "the error must name the required node, got {error:?}"
    );
    assert_eq!(
        app.world().resource::<TeleportLog>().calls,
        0,
        "the handler must not have run"
    );

    // The only change: grant the node.
    app.world_mut()
        .resource_mut::<Permissions>()
        .grant(player_id(), "warp.admin");

    dispatch(app.world_mut(), &player(), "/warp admin reload")
        .expect("the same input must now succeed");
    assert_eq!(app.world().resource::<TeleportLog>().calls, 1);
}

/// A gate on one branch must not gate its siblings — the mistake that would
/// make a command-level permission out of a node-level one.
#[test]
fn a_gated_branch_does_not_gate_its_ungated_sibling() {
    let mut app = app();
    assert!(dispatch(app.world_mut(), &player(), "/warp admin reload").is_err());
    dispatch(app.world_mut(), &player(), "/warp list")
        .expect("`list` is ungated and must still work");
    assert_eq!(app.world().resource::<TeleportLog>().calls, 1);
}

/// A wildcard grant reaches the gated node, joining the permission resolver to
/// the command gate — the two subsystems this cluster exists to connect.
#[test]
fn a_wildcard_permission_grant_opens_a_gated_command_branch() {
    let mut app = app();
    app.world_mut()
        .resource_mut::<Permissions>()
        .grant(player_id(), "warp.*");

    dispatch(app.world_mut(), &player(), "/warp admin reload")
        .expect("`warp.*` must cover `warp.admin`");
    assert_eq!(app.world().resource::<TeleportLog>().calls, 1);
}

/// An op reaches a node declared `Op`, which is the op-level half of #127
/// arriving through the command gate rather than through a direct
/// `Permissions::has` call.
#[test]
fn an_op_reaches_a_node_declared_op_through_the_command_gate() {
    let mut app = App::new();
    app.add_plugins(TeleportPlugin);
    app.world_mut()
        .resource_mut::<Permissions>()
        .declare("warp.admin", PermissionDefault::Op);

    // Non-op first: the control that the declaration is doing the work.
    assert!(
        dispatch(app.world_mut(), &player(), "/warp admin reload").is_err(),
        "a non-op must not reach an Op-defaulted node"
    );

    app.world_mut()
        .resource_mut::<Permissions>()
        .store
        .set_level(player_id(), PermissionLevel::Gamemasters);

    dispatch(app.world_mut(), &player(), "/warp admin reload")
        .expect("an op must reach an Op-defaulted node");
    assert_eq!(app.world().resource::<TeleportLog>().calls, 1);
}

// ---------------------------------------------------------------------------
// #122 — the suggestion half, which is silent where dispatch is loud
// ---------------------------------------------------------------------------

/// A gated branch is **absent** from tab completion, and appears once granted.
/// Both directions in one test, so the grant is the only variable.
#[test]
fn a_gated_branch_is_hidden_from_tab_completion_until_granted() {
    let mut app = app();

    let hidden = suggest(app.world(), &player(), "/warp ");
    assert!(
        hidden.contains(&"list".to_string()),
        "the ungated sibling must be suggested, got {hidden:?}"
    );
    assert!(
        !hidden.contains(&"admin".to_string()),
        "the gated branch must be silently absent, got {hidden:?}"
    );

    app.world_mut()
        .resource_mut::<Permissions>()
        .grant(player_id(), "warp.admin");

    let visible = suggest(app.world(), &player(), "/warp ");
    assert!(
        visible.contains(&"admin".to_string()),
        "granting must reveal it, got {visible:?}"
    );
}

/// **Subtree pruning, vanilla's `fillUsableCommands` semantics.** `reload` and
/// `set` carry no permission of their own, so a per-*node* check would happily
/// offer them; they must be invisible because their **parent** is denied.
///
/// This is the assertion that distinguishes real pruning from a naive
/// per-node check, and it is the one a wrong implementation passes every other
/// test without.
#[test]
fn a_permitted_child_of_a_denied_parent_is_invisible() {
    let mut app = app();

    let denied = suggest(app.world(), &player(), "/warp admin ");
    assert!(
        !denied.contains(&"reload".to_string()) && !denied.contains(&"set".to_string()),
        "nothing beneath a denied parent may be suggested, got {denied:?}"
    );
    // What *is* offered is `warp`'s own children: the walk stopped at the denied
    // node, so completion falls back to the last node it did reach. That is
    // vanilla-consistent — the client was never sent `admin`, so its own
    // best-effort parse would stop in exactly the same place and
    // `getCompletionSuggestions` would offer the same set. Asserted rather than
    // left as "empty", which is what this test wrongly expected at first.
    assert_eq!(
        denied,
        vec!["list".to_string()],
        "completion must fall back to the last reachable node's children"
    );

    app.world_mut()
        .resource_mut::<Permissions>()
        .grant(player_id(), "warp.admin");

    let allowed = suggest(app.world(), &player(), "/warp admin ");
    assert!(
        allowed.contains(&"reload".to_string()) && allowed.contains(&"set".to_string()),
        "both children must appear once the parent is permitted, got {allowed:?}"
    );
}

/// Command *names* are completed on the first token, and a command whose
/// root-literal permission the subject lacks is omitted entirely.
#[test]
fn command_names_are_completed_and_wholly_gated_commands_are_omitted() {
    let mut app = app();

    let names = suggest(app.world(), &player(), "/wa");
    assert_eq!(names, vec!["warp".to_string()]);

    // Register a second command gated at its root, and confirm it is hidden
    // while `warp` still is not.
    let mut secret = PluginCommand::new("secret");
    let root = secret.root();
    secret.permission("secret.use");
    secret.on_execute(root, |_| CommandOutcome::ok());
    app.world_mut()
        .resource_mut::<CommandRegistry>()
        .register(secret)
        .unwrap();
    app.world_mut()
        .resource_mut::<Permissions>()
        .declare("secret.use", PermissionDefault::False);

    let all = suggest(app.world(), &player(), "");
    assert!(all.contains(&"warp".to_string()));
    assert!(
        !all.contains(&"secret".to_string()),
        "a root-gated command must not be listed, got {all:?}"
    );

    app.world_mut()
        .resource_mut::<Permissions>()
        .grant(player_id(), "secret.use");
    assert!(suggest(app.world(), &player(), "").contains(&"secret".to_string()));
}

/// `command_tree_for` prunes the same way, for the future clientbound `COMMANDS`
/// encoder — with a control showing the count actually changes.
#[test]
fn command_tree_for_prunes_a_denied_subtree() {
    let app = app();
    let command = app
        .world()
        .resource::<CommandRegistry>()
        .get("warp")
        .unwrap()
        .clone();

    let all = command_tree_for(&command, &|_| true);
    let pruned = command_tree_for(&command, &|node| node != "warp.admin");

    assert!(
        all.len() > pruned.len(),
        "denying `warp.admin` must remove nodes: all={} pruned={}",
        all.len(),
        pruned.len()
    );
    // `admin` plus `reload`, `set`, `name`, `amount` — five nodes, so the
    // difference is a subtree and not just the gated node itself. Predicting the
    // exact number rather than asserting `>` is what makes this not a
    // direction-only assertion.
    assert_eq!(
        all.len() - pruned.len(),
        5,
        "the whole `admin` subtree must go, not only the gated node"
    );
}

// ---------------------------------------------------------------------------
// The security-shaped control
// ---------------------------------------------------------------------------

/// **A missing `Permissions` resource must never mean "allow everything".**
///
/// The failure mode this guards is silent and would only be noticed by someone
/// who did not have the permission they just successfully used. Built by hand on
/// a bare `World` rather than through the plugin, because the plugin's whole job
/// is to make this state unreachable.
#[test]
fn dispatch_refuses_rather_than_ungates_when_permissions_are_missing() {
    let mut world = bevy_ecs::world::World::new();
    let mut registry = CommandRegistry::new();
    let mut command = PluginCommand::new("warp");
    let root = command.root();
    command.permission("warp.use");
    command.on_execute(root, |_| CommandOutcome::ok());
    registry.register(command).unwrap();
    world.insert_resource(registry);
    // Deliberately no `Permissions`.

    let error = dispatch(&mut world, &player(), "/warp")
        .expect_err("a world with no Permissions must refuse, not allow");
    assert_eq!(
        error,
        CommandDispatchError::NotInstalled {
            missing: "Permissions"
        }
    );

    // The control: insert the resource and grant the node, and the identical
    // call now succeeds — so the refusal above was about the missing resource
    // and not about something else being wrong with the setup.
    let mut permissions = Permissions::new();
    permissions.grant(player_id(), "warp.use");
    world.insert_resource(permissions);
    assert!(dispatch(&mut world, &player(), "/warp").is_ok());
}

/// `suggest` on a world with no registry returns nothing rather than panicking —
/// it is called from a UI path where a panic would take the client down.
#[test]
fn suggest_on_a_bare_world_is_empty_rather_than_a_panic() {
    let world = bevy_ecs::world::World::new();
    assert!(suggest(&world, &player(), "/warp ").is_empty());
}

// ---------------------------------------------------------------------------
// #119 — live suggestions, driven through the real system
// ---------------------------------------------------------------------------

/// The player-name argument's suggestions come from the tab list, through the
/// system the plugin registers — not from a value poked into the directory by
/// the test.
///
/// This drives `GameTick`, so it fails if `sync_player_directory` is not
/// actually registered in a schedule the driver runs: registering a system in
/// the wrong set or schedule is the other half of this repo's island problem.
#[test]
fn player_name_suggestions_come_from_the_tab_list_via_the_registered_system() {
    use lodestone_game::tablist::{GameProfile, PlayerListEntry};

    let mut app = app();

    // Precondition, asserted rather than assumed: nothing is suggested yet.
    assert!(
        app.world().resource::<PlayerDirectory>().names().is_empty(),
        "the directory must start empty, or this test cannot attribute the change"
    );

    let mut list = SessionTabList::default();
    list.0.insert(PlayerListEntry::new(GameProfile::new(
        Uuid::from_u128(1),
        "Alice",
    )));
    list.0.insert(PlayerListEntry::new(GameProfile::new(
        Uuid::from_u128(2),
        "Bob",
    )));
    app.world_mut().spawn(list);

    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        app.world().resource::<PlayerDirectory>().names(),
        vec!["Alice".to_string(), "Bob".to_string()],
        "the registered system must have folded the tab list into the directory"
    );
}
