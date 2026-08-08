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
    let world = CommandWorld { rules: rules as &(dyn RuleStore + Sync), players };
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
