//! The command **substrate**: typed argument keys, the modifier/fork dispatch
//! walk, selector resolution, and the directed effect outbox.
//!
//! # Why this is separate from the wire gate
//!
//! `crates/protocol/v770/tests/builtin_commands_wire_path.rs` is the gate that
//! matters for "does a player get anything" — a real frame, a real
//! `V770ServerProtocol`, a real observable effect. It cannot live here, because it
//! needs a version crate.
//!
//! What *this* file covers is the machinery underneath, which that gate exercises
//! only along the paths the three shipped commands happen to take. The
//! modifier/fork substrate in particular has **no production caller at all** until
//! `/execute` lands, so without something here it is an island of precisely the
//! kind `crate::commands` was built to end. `ServerCommands::from_registrar` is the
//! seam that lets a gate drive it.

use lodestone_command::ParsedCommand;
use lodestone_command_mc::{EntityArg, GameModeArg};
use lodestone_model::{GameMode, Rotation, Vec3};
use lodestone_server::commands::registrar::{Ctx, RuleStore};
use lodestone_server::commands::{
    CommandSource, CommandWorld, PlayerCandidate, Registrar, ServerCommands, overworld_dimension,
};
use lodestone_server::game_rules::GameRulesHandle;
use uuid::Uuid;

fn uuid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn candidate(n: u128, name: &str, x: f64, mode: GameMode) -> PlayerCandidate {
    PlayerCandidate {
        uuid: uuid(n),
        entity_id: 1000 + n as i32,
        username: name.to_string(),
        position: Vec3::new(x, 64.0, 0.0),
        game_mode: mode,
    }
}

/// The caller: `alice`, at the origin, level 4.
fn source(uuid_n: u128, name: &str) -> CommandSource {
    CommandSource::player(
        uuid(uuid_n),
        1000 + uuid_n as i32,
        name,
        Vec3::new(0.0, 64.0, 0.0),
        Rotation { yaw: 0.0, pitch: 0.0 },
        overworld_dimension(),
        4,
    )
}

/// A roster of four players at increasing distances, in two game modes.
fn roster() -> Vec<PlayerCandidate> {
    vec![
        candidate(1, "alice", 0.0, GameMode::Survival),
        candidate(2, "bob", 5.0, GameMode::Creative),
        candidate(3, "carol", 12.0, GameMode::Survival),
        candidate(4, "dave", 30.0, GameMode::Creative),
    ]
}

fn run(
    commands: &ServerCommands,
    rules: &GameRulesHandle,
    players: &[PlayerCandidate],
    source: &CommandSource,
    text: &str,
) -> Option<lodestone_server::CommandOutcome> {
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let world =
        CommandWorld { rules: rules as &(dyn RuleStore + Sync), players, state: &state, mobs: None };
    commands.run(&world, source, text)
}

// ---------------------------------------------------------------------------
// Precedence
// ---------------------------------------------------------------------------

/// A root we do not own must fall through; a root we *do* own with a bad argument
/// must not. Falling through on every error would tell the player the command does
/// not exist when only their argument was wrong.
#[test]
fn an_unknown_root_falls_through_but_a_bad_argument_to_a_known_root_does_not() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let alice = source(1, "alice");

    assert!(run(&commands, &rules, &players, &alice, "warp spawn").is_none());
    assert!(run(&commands, &rules, &players, &alice, "gamerule no_such_rule true").is_some());
    assert!(
        run(&commands, &rules, &players, &alice, "gamerule").is_some(),
        "a bare `/gamerule` is incomplete, not unknown"
    );
}

/// Permission gating is *loud* on execution and *silent* on suggestion — the two
/// halves `lodestone_command::filter` documents, exercised through the real
/// built-in tree at its real level (2).
#[test]
fn a_level_1_caller_cannot_run_or_see_a_level_2_command() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let mut alice = source(1, "alice");
    alice.permission_level = 1;

    let outcome = run(&commands, &rules, &players, &alice, "gamemode creative")
        .expect("the root is ours even when denied");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(
        outcome.response.lines()[0].contains("permission"),
        "a denied command must say so, not read as a typo: {outcome:?}"
    );
    assert!(outcome.effects.is_empty(), "a denied command must produce no effects");

    // Silent on suggestion: the node is simply absent.
    assert!(
        commands.suggest("game", 1).is_empty(),
        "a level-1 caller must not be offered level-2 commands: {:?}",
        commands.suggest("game", 1)
    );
    // The control, on the same tree: level 2 sees all three roots.
    let visible = commands.suggest("g", 2);
    for expected in ["gamemode", "gamerule", "give"] {
        assert!(visible.contains(&expected.to_string()), "level 2 must see /{expected}: {visible:?}");
    }
}

// ---------------------------------------------------------------------------
// Effects, not actions
// ---------------------------------------------------------------------------

/// `/gamemode <mode> <targets>` produces one `SetGameMode` per resolved target
/// plus one directed `Message` for each target that is not the caller — and the
/// caller gets a confirmation line for every one.
#[test]
fn gamemode_with_a_selector_emits_one_effect_per_target_and_notifies_only_the_others() {
    use lodestone_server::Effect;
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let alice = source(1, "alice");

    let outcome = run(&commands, &rules, &players, &alice, "gamemode adventure @a")
        .expect("root matched")
        .clone();
    assert!(outcome.response.is_ran(), "{outcome:?}");

    let modes: Vec<Uuid> = outcome
        .effects
        .iter()
        .filter(|d| d.effect == Effect::SetGameMode(GameMode::Adventure))
        .map(|d| d.target)
        .collect();
    assert_eq!(
        modes,
        [uuid(1), uuid(2), uuid(3), uuid(4)],
        "@a must reach every player exactly once, in roster order"
    );

    let notified: Vec<Uuid> = outcome
        .effects
        .iter()
        .filter(|d| matches!(d.effect, Effect::Message(_)))
        .map(|d| d.target)
        .collect();
    assert_eq!(
        notified,
        [uuid(2), uuid(3), uuid(4)],
        "the caller is confirmed in chat, not notified as a target"
    );
    assert_eq!(outcome.response.lines().len(), 4, "one line per target: {outcome:?}");
    assert_eq!(outcome.response.lines()[0], "Set own game mode to Adventure Mode");
}

/// `/give` splits a count into whole stacks at the item's own max, and the total
/// is conserved — the property a per-stack assertion cannot see.
#[test]
fn give_splits_a_large_count_into_whole_stacks_and_conserves_the_total() {
    use lodestone_server::Effect;
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let alice = source(1, "alice");

    let outcome = run(&commands, &rules, &players, &alice, "give bob minecraft:diamond 100")
        .expect("root matched");
    let [directed] = outcome.effects.as_slice() else {
        panic!("exactly one target: {outcome:?}");
    };
    assert_eq!(directed.target, uuid(2), "a bare name resolves to that player alone");
    let Effect::GiveItems(stacks) = &directed.effect else {
        panic!("{directed:?}");
    };
    // 64 + 36, predicted from vanilla's own max stack size for a diamond.
    assert_eq!(stacks.iter().map(|s| s.count).collect::<Vec<_>>(), [64, 36]);
    assert_eq!(stacks.iter().map(|s| s.count).sum::<u32>(), 100);

    // The 100-stack cap (`GiveCommand.MAX_ALLOWED_ITEMSTACKS`) is per-item:
    // 64 × 100 for a diamond. One under passes, one over is refused.
    assert!(
        run(&commands, &rules, &players, &alice, "give bob minecraft:diamond 6400")
            .expect("root matched")
            .response
            .is_ran()
    );
    let refused = run(&commands, &rules, &players, &alice, "give bob minecraft:diamond 6401")
        .expect("root matched");
    assert!(!refused.response.is_ran(), "6401 exceeds 64 × 100: {refused:?}");
    // An unstackable item's cap is 1 × 100, so the *same* count that was legal
    // for diamonds is not legal here — which is what proves the cap is per-item
    // rather than a flat number.
    let refused = run(&commands, &rules, &players, &alice, "give bob minecraft:diamond_sword 101")
        .expect("root matched");
    assert!(!refused.response.is_ran(), "101 exceeds 1 × 100: {refused:?}");
}

// ---------------------------------------------------------------------------
// Selector resolution
// ---------------------------------------------------------------------------

/// `sort` is applied **before** `limit`, and the predicates before both.
///
/// The other order is the classic bug: `sort=nearest,limit=2` would give two
/// arbitrary players sorted among themselves. The expected sets are computed from
/// the roster's own hand-written coordinates, not read back from the resolver.
#[test]
fn a_selector_filters_then_sorts_then_truncates() {
    use lodestone_server::Effect;
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let alice = source(1, "alice");

    let targets = |text: &str| -> Vec<Uuid> {
        run(&commands, &rules, &players, &alice, text)
            .expect("root matched")
            .effects
            .iter()
            .filter(|d| matches!(d.effect, Effect::SetGameMode(_)))
            .map(|d| d.target)
            .collect()
    };

    // Alice is at x=0, bob 5, carol 12, dave 30.
    assert_eq!(targets("gamemode creative @a[sort=nearest,limit=2]"), [uuid(1), uuid(2)]);
    assert_eq!(targets("gamemode creative @a[sort=furthest,limit=2]"), [uuid(4), uuid(3)]);
    assert_eq!(targets("gamemode creative @a[distance=..12]"), [uuid(1), uuid(2), uuid(3)]);
    assert_eq!(targets("gamemode creative @a[distance=6..]"), [uuid(3), uuid(4)]);
    // Predicates narrow before the sort: bob and dave are creative.
    assert_eq!(targets("gamemode survival @a[gamemode=creative]"), [uuid(2), uuid(4)]);
    assert_eq!(targets("gamemode survival @a[gamemode=!creative]"), [uuid(1), uuid(3)]);
    assert_eq!(targets("gamemode creative @a[name=carol]"), [uuid(3)]);
    // `x=` overrides the origin the distance is measured from, so the *same*
    // radius selects a different set — the assertion that `x=` is read at all.
    assert_eq!(targets("gamemode creative @a[x=30,distance=..1]"), [uuid(4)]);
    assert_eq!(targets("gamemode creative @s"), [uuid(1)]);
    assert_eq!(targets("gamemode creative @p"), [uuid(1)], "@p is the nearest, which is the caller");
}

/// A selector matching nobody is a refusal naming that, not a silent success.
#[test]
fn a_selector_that_matches_nobody_is_refused_rather_than_succeeding_emptily() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let alice = source(1, "alice");

    let outcome = run(&commands, &rules, &players, &alice, "gamemode creative @a[distance=..1,name=dave]")
        .expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(outcome.effects.is_empty());
    assert!(
        outcome.response.lines()[0].contains("No player"),
        "the refusal must say nobody matched: {outcome:?}"
    );
}

/// `@s` from a source with **no entity** (the console) is refused as "must be a
/// player", not as "no player found" — the two are different messages in vanilla
/// and conflating them tells an RCON admin their world is empty.
#[test]
fn at_s_from_the_console_says_it_must_be_a_player() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let console = CommandSource::console("Rcon", overworld_dimension(), 4);

    let outcome = run(&commands, &rules, &players, &console, "gamemode creative")
        .expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(
        outcome.response.lines()[0].contains("only be used by a player"),
        "{outcome:?}"
    );

    // The control: the console *can* target a named player, so the refusal above
    // is about `@s` and not about the console being unable to run the command.
    let outcome = run(&commands, &rules, &players, &console, "gamemode creative bob")
        .expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    assert_eq!(outcome.effects.len(), 2, "the mode plus the target's notification");
}

// ---------------------------------------------------------------------------
// The modifier / fork substrate
// ---------------------------------------------------------------------------

/// A **rewrite** modifier: one source in, one (different) source out, and the
/// executor runs once, for the rewritten source.
///
/// This is `/execute as <player>`'s whole mechanism, and nothing in production
/// exercises it yet — which is exactly why it is gated here.
#[test]
fn a_rewrite_modifier_replaces_the_source_the_executor_runs_for() {
    let mut registrar = Registrar::new();
    let root = registrar.root();
    let stand_in = registrar.literal(root, "standin");
    let (target_node, target_key) = registrar.arg(stand_in, "target", EntityArg::player());
    // The modifier rewrites the source to the resolved player.
    registrar.modifier(target_node, false, move |ctx: &mut Ctx<'_>, _sources, _parsed| {
        let selector = ctx.get(target_key).clone();
        let resolved = ctx.resolve(&selector).map_err(|e| e.to_string())?;
        let target = &resolved[0];
        let mut rewritten = ctx.source.clone();
        rewritten.entity = Some(lodestone_server::commands::SourceEntity {
            uuid: target.uuid,
            entity_id: target.entity_id,
            username: target.username.clone(),
        });
        rewritten.name = target.username.clone();
        rewritten.position = target.position;
        Ok(vec![rewritten])
    });
    let whoami = registrar.literal(target_node, "whoami");
    registrar.exec(whoami, |ctx| {
        ctx.send_success(format!("{} at x={}", ctx.source.name, ctx.source.position.x));
        Ok(1)
    });
    let commands = ServerCommands::from_registrar(registrar);
    let rules = GameRulesHandle::new();
    let players = roster();

    let outcome = run(&commands, &rules, &players, &source(1, "alice"), "standin carol whoami")
        .expect("root matched");
    assert_eq!(
        outcome.response.lines(),
        ["carol at x=12"],
        "the executor must run for the rewritten source, not the caller: {outcome:?}"
    );
}

/// A **fork** modifier: one source in, many out, and the executor runs **once per
/// source**. The fork multiplicity is the dispatcher's, not the handler's.
///
/// The count is predicted exactly (four lines, in roster order), so a
/// run-once-and-loop-inside implementation and a run-per-source one are different
/// results rather than both "it worked".
#[test]
fn a_fork_modifier_runs_the_deepest_executor_once_per_produced_source() {
    let mut registrar = Registrar::new();
    let root = registrar.root();
    let each = registrar.literal(root, "each");
    let (targets_node, targets_key) = registrar.arg(each, "targets", EntityArg::players());
    registrar.modifier(targets_node, true, move |ctx: &mut Ctx<'_>, _sources, _parsed| {
        let selector = ctx.get(targets_key).clone();
        let resolved = ctx.resolve(&selector).map_err(|e| e.to_string())?;
        Ok(resolved
            .into_iter()
            .map(|target| {
                let mut forked = ctx.source.clone();
                forked.name = target.username.clone();
                forked.entity = Some(lodestone_server::commands::SourceEntity {
                    uuid: target.uuid,
                    entity_id: target.entity_id,
                    username: target.username,
                });
                forked
            })
            .collect())
    });
    let shout = registrar.literal(targets_node, "shout");
    registrar.exec(shout, |ctx| {
        ctx.send_success(format!("hello {}", ctx.source.name));
        Ok(1)
    });
    let commands = ServerCommands::from_registrar(registrar);
    let rules = GameRulesHandle::new();
    let players = roster();

    let outcome =
        run(&commands, &rules, &players, &source(1, "alice"), "each @a shout").expect("root matched");
    assert_eq!(
        outcome.response.lines(),
        ["hello alice", "hello bob", "hello carol", "hello dave"],
        "the executor must run once per forked source: {outcome:?}"
    );
}

/// The asymmetry that makes forks worth having: a failure inside a **fork** does
/// not stop the other branches, while a failure on an **unforked** path is the
/// whole answer.
///
/// `execute as @a run give @s …` must not stop at the first player whose inventory
/// is full. Both arms are built from one registrar so the difference is the
/// `forks` flag and nothing else.
#[test]
fn a_failure_aborts_an_unforked_path_but_only_its_own_branch_when_forked() {
    fn build(forks: bool) -> ServerCommands {
        let mut registrar = Registrar::new();
        let root = registrar.root();
        let each = registrar.literal(root, "each");
        let (targets_node, targets_key) = registrar.arg(each, "targets", EntityArg::players());
        registrar.modifier(targets_node, forks, move |ctx: &mut Ctx<'_>, sources, _parsed| {
            let selector = ctx.get(targets_key).clone();
            let resolved = ctx.resolve(&selector).map_err(|e| e.to_string())?;
            // When not forking, keep the single incoming source so the two arms
            // differ only in multiplicity and the flag.
            if !forks {
                return Ok(sources);
            }
            Ok(resolved
                .into_iter()
                .map(|target| {
                    let mut forked = ctx.source.clone();
                    forked.name = target.username;
                    forked
                })
                .collect())
        });
        let try_it = registrar.literal(targets_node, "try");
        registrar.exec(try_it, |ctx| {
            // `carol` always fails; everyone else succeeds.
            if ctx.source.name == "carol" {
                return Err("carol refuses".to_string());
            }
            ctx.send_success(format!("ok {}", ctx.source.name));
            Ok(1)
        });
        ServerCommands::from_registrar(registrar)
    }

    let rules = GameRulesHandle::new();
    let players = roster();

    // Forked: carol's failure is recorded and everybody else still ran.
    let outcome = run(&build(true), &rules, &players, &source(1, "alice"), "each @a try")
        .expect("root matched");
    let lines = outcome.response.lines();
    assert!(outcome.response.is_ran(), "a forked path survives one branch failing: {outcome:?}");
    assert!(lines.contains(&"ok alice".to_string()), "{lines:?}");
    assert!(lines.contains(&"ok dave".to_string()), "the branch *after* carol must still run: {lines:?}");
    assert!(lines.contains(&"carol refuses".to_string()), "{lines:?}");

    // Unforked, with carol as the single source: the failure is the answer, and
    // nothing else is reported.
    let outcome = run(&build(false), &rules, &players, &source(3, "carol"), "each @a try")
        .expect("root matched");
    assert!(!outcome.response.is_ran(), "an unforked failure aborts: {outcome:?}");
    assert_eq!(outcome.response.lines(), ["carol refuses"]);
}

// ---------------------------------------------------------------------------
// The typed-key API's residue
// ---------------------------------------------------------------------------

/// An `ArgKey` from another tree panics rather than silently reading whatever node
/// happens to share its index. One of the three documented runtime panics.
#[test]
#[should_panic(expected = "belongs to a different command tree")]
fn a_key_from_another_tree_panics_on_first_execution() {
    // Tree A hands out the key.
    let mut other = Registrar::new();
    let other_root = other.root();
    let other_literal = other.literal(other_root, "other");
    let (_, stolen) = other.arg(other_literal, "mode", GameModeArg);

    // Tree B's executor uses it.
    let mut registrar = Registrar::new();
    let root = registrar.root();
    let literal = registrar.literal(root, "here");
    let (node, _own) = registrar.arg(literal, "mode", GameModeArg);
    registrar.exec(node, move |ctx| {
        let _ = ctx.get(stolen);
        Ok(1)
    });
    let commands = ServerCommands::from_registrar(registrar);
    let rules = GameRulesHandle::new();
    let players = roster();
    let _ = run(&commands, &rules, &players, &source(1, "alice"), "here creative");
}

/// Reading an argument *deeper* on the path than the node currently executing
/// panics — the second documented runtime panic, and the one a shallow
/// `exec` on a two-executable path could plausibly hit.
#[test]
#[should_panic(expected = "reading an argument deeper")]
fn a_key_below_the_executing_node_panics_on_first_execution() {
    let mut registrar = Registrar::new();
    let root = registrar.root();
    let literal = registrar.literal(root, "here");
    let (mode_node, _mode) = registrar.arg(literal, "mode", GameModeArg);
    let (_deep_node, deep) =
        registrar.arg(mode_node, "target", EntityArg::players());
    // The *shallow* node's executor reads the *deep* node's key.
    registrar.exec(mode_node, move |ctx| {
        let _ = ctx.get(deep);
        Ok(1)
    });
    let commands = ServerCommands::from_registrar(registrar);
    let rules = GameRulesHandle::new();
    let players = roster();
    // `here creative` stops at the mode node, so `target` was never parsed.
    let _ = run(&commands, &rules, &players, &source(1, "alice"), "here creative");
}

/// The control for the two panics above: the *correct* key on the *correct* node
/// reads its value and does not panic. Without this, a `get` that panicked
/// unconditionally would satisfy both `should_panic` tests.
#[test]
fn the_correct_key_on_its_own_node_reads_its_value() {
    let mut registrar = Registrar::new();
    let root = registrar.root();
    let literal = registrar.literal(root, "here");
    let (mode_node, mode) = registrar.arg(literal, "mode", GameModeArg);
    let (target_node, target) = registrar.arg(mode_node, "target", EntityArg::players());
    // The deep node reads *both* keys — the shallow one is in scope there.
    registrar.exec(target_node, move |ctx| {
        let mode = *ctx.get(mode);
        let count = ctx.resolve(&ctx.get(target).clone()).map_err(|e| e.to_string())?.len();
        ctx.send_success(format!("{mode:?} for {count}"));
        Ok(1)
    });
    let commands = ServerCommands::from_registrar(registrar);
    let rules = GameRulesHandle::new();
    let players = roster();
    let outcome = run(&commands, &rules, &players, &source(1, "alice"), "here spectator @a")
        .expect("root matched");
    assert_eq!(outcome.response.lines(), ["Spectator for 4"]);
}

// ---------------------------------------------------------------------------
// The rule store seam
// ---------------------------------------------------------------------------

/// `/gamerule` writes and reads back through whichever [`RuleStore`] it is given —
/// including the **production** `WorldStateHandle`, which is the store the old
/// island's `&GameRulesHandle` parameter could never have reached.
#[test]
fn gamerule_writes_the_store_it_is_handed_including_the_production_world_state() {
    let commands = ServerCommands::new();
    let players = roster();
    let alice = source(1, "alice");

    // Vanilla's default is 3 (`GameRules.java:74`), predicted rather than read
    // back, so a store that answered whatever it was last told would fail here.
    let world_state = lodestone_server::world_state::WorldStateHandle::new();
    assert_eq!(world_state.random_tick_speed(), 3);

    let command_world = CommandWorld {
        rules: &world_state as &(dyn RuleStore + Sync),
        players: &players,
        state: &world_state,
        mobs: None,
    };
    let outcome = commands
        .run(&command_world, &alice, "gamerule random_tick_speed 6")
        .expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    // The effect read off the *store*, not off the response: the response could be
    // right while the write went nowhere, which is exactly what the island did.
    assert_eq!(world_state.random_tick_speed(), 6);

    // The tree enforces the type, so `GameRules::set` never sees "7".
    let refused = commands
        .run(&command_world, &alice, "gamerule keep_inventory 7")
        .expect("root matched");
    assert!(!refused.response.is_ran(), "{refused:?}");
    assert!(!world_state.keep_inventory());
    // And the control on the same store.
    assert!(
        commands
            .run(&command_world, &alice, "gamerule keep_inventory true")
            .expect("root matched")
            .response
            .is_ran()
    );
    assert!(world_state.keep_inventory());
}

/// Every rule in `GAME_RULES` is reachable through the command, and every
/// executable node has an executor.
///
/// The registration gate: a rule added to the table but not the tree, or a node
/// marked executable with nothing behind it, is invisible to every single-rule test
/// above.
#[test]
fn every_rule_is_settable_through_the_command() {
    use lodestone_server::game_rules::{GAME_RULES, GameRuleValue};
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    assert!(!GAME_RULES.is_empty());

    for spec in GAME_RULES {
        let value = match spec.default {
            GameRuleValue::Bool(b) => (!b).to_string(),
            GameRuleValue::Int(_) => spec.min.unwrap_or(0).to_string(),
        };
        let outcome = run(
            &commands,
            &rules,
            &players,
            &alice,
            &format!("gamerule {} {value}", spec.name),
        )
        .unwrap_or_else(|| panic!("`gamerule {}` must be a built-in", spec.name));
        assert!(outcome.response.is_ran(), "`gamerule {} {value}`: {outcome:?}", spec.name);
        assert_eq!(
            rules.with(|r| r.get(spec.name)).expect("rule exists").serialize(),
            value,
            "`gamerule {}` reported success without storing the value",
            spec.name
        );
    }
    // Two rules' worth of writes must both survive — a store keeping only the most
    // recent write would pass every per-rule assertion above.
    assert_eq!(rules.with(|r| r.entries().len()), GAME_RULES.len());
}

// ---------------------------------------------------------------------------
// The directed outbox
// ---------------------------------------------------------------------------

/// The outbox is **directed and drained**, not broadcast and cursored.
///
/// The distinction is the whole reason it is not a second copy of the chat log:
/// `/gamemode creative bob` must reach bob and nobody else, once.
#[test]
fn the_effect_outbox_delivers_to_one_player_once_and_refuses_a_stranger() {
    use lodestone_server::{Effect, PlayerRegistry};
    let registry = PlayerRegistry::new();
    let bob = registry.join("bob", uuid(2), Vec3::new(0.0, 64.0, 0.0));
    let _carol = registry.join("carol", uuid(3), Vec3::new(0.0, 64.0, 0.0));

    assert!(registry.push_effect(uuid(2), Effect::SetGameMode(GameMode::Creative)));
    assert!(
        !registry.push_effect(uuid(99), Effect::SetGameMode(GameMode::Creative)),
        "an effect for a player who is not connected must be refused, not queued forever"
    );

    // Directed: carol gets nothing.
    assert!(registry.take_effects(uuid(3)).is_empty());
    // And bob gets exactly one, once.
    assert_eq!(
        registry.take_effects(uuid(2)),
        [Effect::SetGameMode(GameMode::Creative)]
    );
    assert!(
        registry.take_effects(uuid(2)).is_empty(),
        "a drain must not deliver twice"
    );

    // The tracked mode is what a `gamemode=` selector on another connection reads.
    registry.set_game_mode(uuid(2), GameMode::Spectator);
    let modes: Vec<(String, GameMode)> = registry
        .candidates()
        .into_iter()
        .map(|c| (c.username, c.game_mode))
        .collect();
    assert!(modes.contains(&("bob".to_string(), GameMode::Spectator)), "{modes:?}");
    assert!(
        modes.contains(&("carol".to_string(), GameMode::Survival)),
        "the other player is untouched: {modes:?}"
    );

    // A departing player's undelivered effects go with them, rather than being
    // handed to them on a later rejoin.
    assert!(registry.push_effect(uuid(2), Effect::Message("late".to_string())));
    drop(bob);
    let rejoined = registry.join("bob", uuid(2), Vec3::new(0.0, 64.0, 0.0));
    assert!(
        registry.take_effects(uuid(2)).is_empty(),
        "a rejoining player must not receive the previous session's queue"
    );
    drop(rejoined);
}

// ---------------------------------------------------------------------------
// The new command set (issue #48)
// ---------------------------------------------------------------------------

fn run_stateful(
    commands: &ServerCommands,
    state: &lodestone_server::world_state::WorldStateHandle,
    players: &[PlayerCandidate],
    source: &CommandSource,
    text: &str,
) -> Option<lodestone_server::CommandOutcome> {
    let world = CommandWorld { rules: state, players, state, mobs: None };
    commands.run(&world, source, text)
}

/// `/time set` writes `day_time` only; `/time add` reads it back and adds; the
/// three `/time query` forms read pairwise-distinct expressions of the same
/// clock.
#[test]
fn time_set_add_and_query_agree_with_the_clock() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();

    for _ in 0..30_005 {
        state.tick_time();
    }
    // `game_time` is 30005 and untouched by `/time set`.
    let outcome = run_stateful(&commands, &state, &players, &alice, "time set 500")
        .expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    assert_eq!(state.time().day_time, 500);
    assert_eq!(state.time().game_time, 30_005, "`/time set` must not touch game_time");

    run_stateful(&commands, &state, &players, &alice, "time add 100").expect("root matched");
    assert_eq!(state.time().day_time, 600, "add reads the current value back");

    let day = run_stateful(&commands, &state, &players, &alice, "time query day")
        .expect("root matched");
    assert_eq!(day.response.lines(), ["The time is 1"], "30005 / 24000 == 1");

    let gametime = run_stateful(&commands, &state, &players, &alice, "time query gametime")
        .expect("root matched");
    assert_eq!(gametime.response.lines(), ["The time is 30005"]);

    let daytime = run_stateful(&commands, &state, &players, &alice, "time query daytime")
        .expect("root matched");
    assert_eq!(daytime.response.lines(), ["The time is 600"], "600 % 24000 == 600");

    // The named literal is a fixed constant, not the numeric argument's parser.
    run_stateful(&commands, &state, &players, &alice, "time set noon").expect("root matched");
    assert_eq!(state.time().day_time, 6_000);
}

/// A locked difficulty refuses a change, and the query reflects whichever
/// value actually won.
#[test]
fn difficulty_sets_queries_and_a_lock_refuses_a_change() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();

    let outcome = run_stateful(&commands, &state, &players, &alice, "difficulty hard")
        .expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    assert_eq!(state.difficulty(), (lodestone_model::Difficulty::Hard, false));

    let query = run_stateful(&commands, &state, &players, &alice, "difficulty").expect("root matched");
    assert_eq!(query.response.lines(), ["The difficulty is hard"]);

    state.set_difficulty_locked(true);
    let refused = run_stateful(&commands, &state, &players, &alice, "difficulty peaceful")
        .expect("root matched");
    assert!(!refused.response.is_ran(), "{refused:?}");
    assert_eq!(
        state.difficulty(),
        (lodestone_model::Difficulty::Hard, true),
        "a locked world keeps its difficulty"
    );
}

/// `/seed` reports the world seed in vanilla's own `Seed: [<n>]` shape. The
/// exact value comes from a process-global this test crate cannot set (see
/// `crate::commands::seed`'s own `#[cfg(test)]` module, in-crate, for the
/// version of this gate that pins a real value); this checks the command
/// exists and answers in the right shape rather than crashing or being routed
/// elsewhere.
#[test]
fn seed_answers_in_vanillas_bracketed_shape() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();

    let outcome = run_stateful(&commands, &state, &players, &alice, "seed").expect("root matched");
    let [line] = outcome.response.lines() else { panic!("{outcome:?}") };
    assert!(line.starts_with("Seed: ["), "{line:?}");
    assert!(line.ends_with(']'), "{line:?}");
}

/// `/setworldspawn` resolves `~`-relative coordinates against the caller's own
/// position — pairwise-distinct deltas, so a transposed x/y/z would be visible
/// — and writes the store `WorldStateHandle::world_spawn` reads back.
#[test]
fn setworldspawn_resolves_relative_coordinates_and_writes_the_store() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    // Alice stands at (0, 64, 0) per `source()`.
    let alice = source(1, "alice");
    let players = roster();

    let outcome = run_stateful(&commands, &state, &players, &alice, "setworldspawn ~11 ~1 ~4")
        .expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    // `world_spawn()` itself is `pub(crate)` — read the same value back through
    // the persisted `level.dat` field surface instead, which is `pub`.
    use lodestone_core::Nbt;
    let fields = state.level_data_fields();
    let (_, spawn) = fields.iter().find(|(name, _)| name == "spawn").expect("a spawn was set");
    let Nbt::Compound(entries) = spawn else { panic!("{spawn:?}") };
    let pos = entries.iter().find(|(k, _)| k == "pos").map(|(_, v)| v).expect("pos field");
    assert_eq!(pos, &Nbt::IntArray(vec![11, 65, 4]));
}

/// `/kill` produces exactly one [`Effect::Kill`], directed at the resolved
/// target — bare form targets self, the selector form targets whoever it
/// resolves.
#[test]
fn kill_targets_self_bare_and_a_selector_explicitly() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();

    let bare = run_stateful(&commands, &state, &players, &alice, "kill").expect("root matched");
    assert_eq!(bare.effects, [DirectedEffect::new(uuid(1), Effect::Kill)]);

    let targeted =
        run_stateful(&commands, &state, &players, &alice, "kill bob").expect("root matched");
    assert_eq!(targeted.effects, [DirectedEffect::new(uuid(2), Effect::Kill)]);
}

/// `/xp add`'s default unit is points; `levels` and `points` are explicit
/// alternatives, and the amounts stay distinct through the pipeline.
#[test]
fn experience_add_defaults_to_points_and_the_unit_literals_select_the_other() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();

    let points =
        run_stateful(&commands, &state, &players, &alice, "xp add alice 17").expect("root matched");
    assert_eq!(
        points.effects,
        [DirectedEffect::new(uuid(1), Effect::GiveExperience { levels: false, amount: 17 })]
    );

    let levels = run_stateful(&commands, &state, &players, &alice, "xp add alice 3 levels")
        .expect("root matched");
    assert_eq!(
        levels.effects,
        [DirectedEffect::new(uuid(1), Effect::GiveExperience { levels: true, amount: 3 })]
    );

    let set = run_stateful(&commands, &state, &players, &alice, "experience set bob 5 levels")
        .expect("root matched");
    assert_eq!(
        set.effects,
        [DirectedEffect::new(uuid(2), Effect::SetExperience { levels: true, amount: 5 })]
    );
}

/// `/clear` with no arguments targets self with no filter and no cap; the
/// item and count arguments narrow it, each independently observable in the
/// resulting effect.
#[test]
fn clear_narrows_by_item_and_by_count_independently() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();

    let bare = run_stateful(&commands, &state, &players, &alice, "clear").expect("root matched");
    assert_eq!(
        bare.effects,
        [DirectedEffect::new(uuid(1), Effect::ClearInventory { item: None, max_count: None })]
    );

    let filtered =
        run_stateful(&commands, &state, &players, &alice, "clear alice minecraft:diamond 5")
            .expect("root matched");
    assert_eq!(
        filtered.effects,
        [DirectedEffect::new(
            uuid(1),
            Effect::ClearInventory { item: Some("minecraft:diamond".to_string()), max_count: Some(5) }
        )]
    );
}

/// `/setblock` resolves its position against the caller and carries the
/// requested block id; `/fill` enumerates the whole (inclusive) box in either
/// corner order and refuses a volume over the vanilla cap without enumerating
/// it.
#[test]
fn setblock_resolves_position_and_fill_enumerates_the_box_and_caps_it() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();

    let outcome =
        run_stateful(&commands, &state, &players, &alice, "setblock ~11 ~1 ~4 minecraft:stone")
            .expect("root matched");
    assert_eq!(
        outcome.effects,
        [DirectedEffect::new(
            uuid(1),
            Effect::SetBlock { pos: (11, 65, 4), block: "minecraft:stone".to_string() }
        )]
    );

    // A 2x2x2 box, corners given in the "wrong" order — `to` before `from` on
    // every axis — still enumerates all eight distinct cells.
    let fill = run_stateful(
        &commands,
        &state,
        &players,
        &alice,
        "fill 5 5 5 4 4 4 minecraft:dirt",
    )
    .expect("root matched");
    let [directed] = fill.effects.as_slice() else { panic!("{fill:?}") };
    let Effect::Fill { positions, block } = &directed.effect else { panic!("{directed:?}") };
    assert_eq!(block, "minecraft:dirt");
    let mut sorted = positions.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        [
            (4, 4, 4), (4, 4, 5), (4, 5, 4), (4, 5, 5),
            (5, 4, 4), (5, 4, 5), (5, 5, 4), (5, 5, 5),
        ]
    );

    // Over the 32768 cap, refused before any position is built.
    let refused = run_stateful(
        &commands,
        &state,
        &players,
        &alice,
        "fill 0 0 0 100 100 100 minecraft:dirt",
    )
    .expect("root matched");
    assert!(!refused.response.is_ran(), "{refused:?}");
    assert!(refused.effects.is_empty());
}

/// `/say` and `/me` are self-targeted broadcasts; `/msg` is an ordinary
/// directed [`Effect::Message`] at the resolved recipient, distinct from a
/// broadcast.
#[test]
fn say_me_and_msg_produce_the_three_distinct_effect_shapes() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();

    let say = run_stateful(&commands, &state, &players, &alice, "say hello everyone")
        .expect("root matched");
    assert_eq!(
        say.effects,
        [DirectedEffect::new(
            uuid(1),
            Effect::Broadcast { sender: "Server".to_string(), message: "hello everyone".to_string() }
        )]
    );

    let me = run_stateful(&commands, &state, &players, &alice, "me waves").expect("root matched");
    assert_eq!(
        me.effects,
        [DirectedEffect::new(
            uuid(1),
            Effect::Broadcast { sender: "alice".to_string(), message: "* waves".to_string() }
        )]
    );

    let msg = run_stateful(&commands, &state, &players, &alice, "msg bob hi there")
        .expect("root matched");
    assert_eq!(
        msg.effects,
        [DirectedEffect::new(
            uuid(2),
            Effect::Message("alice whispers to you: hi there".to_string())
        )]
    );
}

/// `/spawnpoint` writes a [`Effect::SetRespawnPoint`] rather than moving the
/// player — the two are easy to conflate and only one exists here (see the
/// command's own module doc for why `/tp` itself is out of scope for now).
#[test]
fn spawnpoint_sets_a_respawn_point_effect_not_a_teleport() {
    use lodestone_server::Effect;
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();

    let outcome = run_stateful(&commands, &state, &players, &alice, "spawnpoint ~2 ~3 ~9")
        .expect("root matched");
    let [directed] = outcome.effects.as_slice() else { panic!("{outcome:?}") };
    assert_eq!(directed.target, uuid(1));
    assert_eq!(
        directed.effect,
        Effect::SetRespawnPoint { pos: lodestone_model::BlockPos::new(2, 67, 9) }
    );
}

// ---------------------------------------------------------------------------
// /tp, /summon, /weather, /defaultgamemode
// ---------------------------------------------------------------------------

/// A source at a non-axis-aligned-with-the-fixture yaw, so `~` and `^`
/// genuinely diverge for the *same* three numbers — an origin-facing-north
/// source (this file's `source()` helper) is exactly the coincidence
/// `CLAUDE.md` warns a whole corpus can share, because at yaw 0 the local
/// basis (left, up, forward) lines up with the world axes (x, y, z) and a
/// `^`-implemented-as-`~` bug would be invisible. Yaw `-90` faces `+X`
/// (`lodestone_command_mc::position`'s own basis test derives and pins
/// this), so the two dialects land on different absolute positions here.
fn rotated_source(uuid_n: u128, name: &str, pos: Vec3) -> CommandSource {
    CommandSource::player(
        uuid(uuid_n),
        1000 + uuid_n as i32,
        name,
        pos,
        Rotation { yaw: -90.0, pitch: 0.0 },
        overworld_dimension(),
        4,
    )
}

/// `~`-relative and `^`-local coordinates resolve to genuinely different
/// absolute positions from the same three numbers, at a source whose facing
/// is not axis-coincident — see [`rotated_source`]'s own doc for why that
/// matters. The three deltas are pairwise-distinct so a transposed x/y/z
/// would be visible in either form.
#[test]
fn tp_relative_and_local_coordinates_diverge_at_a_non_coincident_rotation() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let alice = rotated_source(1, "alice", Vec3::new(100.0, 64.0, -8.0));
    let players = roster();

    // `~4 ~6 ~11` is plain per-axis addition, independent of rotation.
    let relative = run(&commands, &GameRulesHandle::new(), &players, &alice, "tp ~4 ~6 ~11")
        .expect("root matched");
    assert_eq!(
        relative.effects,
        [DirectedEffect::new(
            uuid(1),
            Effect::Teleport { x: 104.0, y: 70.0, z: 3.0, yaw: None, pitch: None }
        )]
    );

    // `^4 ^6 ^11` at yaw -90 (facing +X): left = (0,0,-1), up = (0,1,0),
    // forward = (1,0,0) (per `lodestone_command_mc::position`'s own basis
    // test), so the result is `origin + left*4 + up*6 + forward*11` =
    // (100+11, 64+6, -8-4) = (111, 70, -12) — different from the `~` case
    // above in x and z, identical only in y (which both dialects treat as
    // world-up when pitch is 0, not a masked bug).
    let local = run(&commands, &GameRulesHandle::new(), &players, &alice, "tp ^4 ^6 ^11")
        .expect("root matched");
    assert_eq!(
        local.effects,
        [DirectedEffect::new(
            uuid(1),
            Effect::Teleport { x: 111.0, y: 70.0, z: -12.0, yaw: None, pitch: None }
        )]
    );
}

/// `/tp <targets> <location>` resolves the location against the **command
/// source**, never the target — vanilla's own `Vec3Argument.getCoordinates`
/// takes the `CommandSourceStack`, not the entity being moved. Bob (at
/// `x = 5` per [`roster`]) is teleported relative to Alice's position, not
/// his own, which is the surprising-in-English but correct behaviour this
/// test pins. The optional `<yaw> <pitch>` pair is carried through as
/// `Some`, distinct from the bare form's `None`.
#[test]
fn tp_targets_location_resolves_against_the_source_never_the_target() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    // Alice at (50, 70, 20), facing +Z (yaw 0) — bob's own position (roster's
    // `x = 5.0, y = 64.0, z = 0.0`) must play no part in the resolved point.
    let alice = source(1, "alice");
    let players = roster();

    let outcome = run(&commands, &GameRulesHandle::new(), &players, &alice, "tp bob ~11 ~1 ~4")
        .expect("root matched");
    assert_eq!(
        outcome.effects,
        [DirectedEffect::new(
            uuid(2),
            // Alice is at (0, 64, 0) per `source()`; bob's own (5, 64, 0) is
            // untouched by the resolution.
            Effect::Teleport { x: 11.0, y: 65.0, z: 4.0, yaw: None, pitch: None }
        )],
        "the offset must resolve against alice's position, not bob's"
    );

    // Written with explicit decimal points so `Vec3Arg`'s centre correction
    // (an absolute `x`/`z` with no decimal point gains `+0.5`, `y` never does
    // — see `lodestone_command_mc::position`'s own module doc) does not shift
    // the expected value away from a literal reading.
    let with_rotation =
        run(&commands, &GameRulesHandle::new(), &players, &alice, "tp bob 1.0 2.0 3.0 45 -30")
            .expect("root matched");
    assert_eq!(
        with_rotation.effects,
        [DirectedEffect::new(
            uuid(2),
            Effect::Teleport { x: 1.0, y: 2.0, z: 3.0, yaw: Some(45.0), pitch: Some(-30.0) }
        )]
    );
}

/// `/tp <destination>` (self) and `/tp <targets> <destination>` both resolve
/// to the destination's live *position*, never a fixed literal — carol and
/// dave sit at different `x` per [`roster`], so a stale/transposed lookup
/// would be visible immediately.
#[test]
fn tp_to_an_entity_resolves_its_current_position() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let alice = source(1, "alice");
    let players = roster();

    let self_to_carol =
        run(&commands, &GameRulesHandle::new(), &players, &alice, "tp carol").expect("root matched");
    assert_eq!(
        self_to_carol.effects,
        [DirectedEffect::new(
            uuid(1),
            Effect::Teleport { x: 12.0, y: 64.0, z: 0.0, yaw: None, pitch: None }
        )]
    );

    let bob_to_dave = run(&commands, &GameRulesHandle::new(), &players, &alice, "tp bob dave")
        .expect("root matched");
    assert_eq!(
        bob_to_dave.effects,
        [DirectedEffect::new(
            uuid(2),
            Effect::Teleport { x: 30.0, y: 64.0, z: 0.0, yaw: None, pitch: None }
        )]
    );
}

/// `/summon` reaches the **same** shared `MobHandle` the world tick loop's
/// mob population lives behind — the property that keeps a summoned mob from
/// being an island (see the command's own module doc). Verified by reading
/// the handle back through [`lodestone_server::EntitySource::snapshots`], the
/// exact surface `EntityStreamer` diffs to decide what a client is told about
/// — not a private field of the sim.
#[test]
fn summon_spawns_into_the_shared_mob_handle_at_the_resolved_position() {
    use lodestone_server::{EntitySource, MobHandle};
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();
    let mobs = MobHandle::default();

    assert!(mobs.snapshots().is_empty(), "nothing spawned yet");

    let world = CommandWorld { rules: &state, players: &players, state: &state, mobs: Some(&mobs) };
    let outcome =
        commands.run(&world, &alice, "summon minecraft:cow ~11 ~1 ~4").expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");

    let snapshots = mobs.snapshots();
    let [cow] = snapshots.as_slice() else {
        panic!("expected exactly one spawned entity, got {snapshots:?}")
    };
    assert_eq!(cow.entity_type, "minecraft:cow".parse().unwrap());
    // Alice is at (0, 64, 0) per `source()`.
    assert_eq!(cow.position, Vec3::new(11.0, 65.0, 4.0));
}

/// `/summon` refuses an unknown entity type at **parse** time — the tree
/// itself, not the executor — because [`lodestone_command_mc::EntityTypeArg`]
/// validates against the real entity-type census.
#[test]
fn summon_refuses_an_unknown_entity_type_at_parse_time() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();
    let mobs = lodestone_server::MobHandle::default();

    let world = CommandWorld { rules: &state, players: &players, state: &state, mobs: Some(&mobs) };
    let outcome =
        commands.run(&world, &alice, "summon minecraft:not_a_real_mob").expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(lodestone_server::EntitySource::snapshots(&mobs).is_empty());
}

/// `/weather clear|rain|thunder [duration]` queues a
/// [`lodestone_server::world_state::WeatherRequest`] for the tick loop to
/// apply on its own next pass — the exact split
/// `crate::sleep::SleepVote`/`SleepState` established, so this test asserts
/// the *queued request*, not a `WeatherState` this crate cannot reach from a
/// command executor at all (see the command's own module doc for why).
#[test]
fn weather_queues_a_request_the_tick_loop_will_apply() {
    use lodestone_server::world_state::WeatherRequest;
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();

    assert_eq!(state.take_weather_request(), None, "nothing queued before the command runs");

    let outcome = run_stateful(&commands, &state, &players, &alice, "weather rain 100")
        .expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    assert_eq!(state.take_weather_request(), Some(WeatherRequest::Rain { duration: 100 }));

    run_stateful(&commands, &state, &players, &alice, "weather thunder 250").expect("root matched");
    assert_eq!(state.take_weather_request(), Some(WeatherRequest::Thunder { duration: 250 }));

    run_stateful(&commands, &state, &players, &alice, "weather clear 1").expect("root matched");
    assert_eq!(state.take_weather_request(), Some(WeatherRequest::Clear { duration: 1 }));

    // The bare, no-duration form queues a request too, just with the
    // documented stand-in constant rather than a sampled one.
    run_stateful(&commands, &state, &players, &alice, "weather rain").expect("root matched");
    assert!(matches!(state.take_weather_request(), Some(WeatherRequest::Rain { duration }) if duration > 0));
}

/// `/defaultgamemode` writes `WorldStateHandle::default_game_mode`, read back
/// through the same handle a real join reads — a store that only ever agreed
/// with whatever it was last told would pass a self-referential check, so
/// this compares against the predicted vanilla default (`Survival`) first.
#[test]
fn defaultgamemode_writes_the_store_a_new_join_reads() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();

    assert_eq!(state.default_game_mode(), GameMode::Survival, "vanilla's own default");

    let outcome = run_stateful(&commands, &state, &players, &alice, "defaultgamemode creative")
        .expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    assert_eq!(state.default_game_mode(), GameMode::Creative);

    run_stateful(&commands, &state, &players, &alice, "defaultgamemode spectator")
        .expect("root matched");
    assert_eq!(state.default_game_mode(), GameMode::Spectator);
}
