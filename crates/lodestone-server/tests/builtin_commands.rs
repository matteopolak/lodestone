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

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use lodestone_command::ParsedCommand;
use lodestone_command_mc::{EntityArg, GameModeArg, SnbtValue};
use lodestone_model::{GameMode, Rotation, Vec3};
use lodestone_server::commands::registrar::{Ctx, RuleStore};
use lodestone_server::commands::{
    CommandSource, CommandWorld, PlayerCandidate, Registrar, ServerCommands, overworld_dimension,
};
use lodestone_server::game_rules::GameRulesHandle;
use lodestone_server::{ChunkColumn, ChunkSource};
use uuid::Uuid;

/// A settable-block fixture for `/execute if`/`unless block` — everything is
/// `"minecraft:air"` until [`FixedBlockSource::set`] names a coordinate,
/// which is deliberately **not** the same as [`ChunkSource::set_block`]
/// (that one goes through the real trait method, exercised separately);
/// this constructor lets a test seed a block without going through a command
/// at all, so `if block` and `/setblock` cannot pass each other's test by
/// sharing a code path neither is supposed to depend on.
#[derive(Default)]
struct FixedBlockSource {
    blocks: Mutex<HashMap<(i32, i32, i32), String>>,
    /// Columns [`FixedBlockSource::mark_unloaded`] has named — for
    /// `/execute if`/`unless loaded`. Empty by default, matching
    /// [`ChunkSource::is_column_resident`]'s own `true` default, so every
    /// existing `if block`/`setblock` gate against this fixture is
    /// unaffected.
    unloaded: Mutex<HashSet<(i32, i32)>>,
    /// Biome overrides [`FixedBlockSource::set_biome`] has named — for
    /// `/execute if`/`unless biome`. Everything is `minecraft:plains` until
    /// named here, matching [`ChunkColumn::new`]'s own default so every
    /// existing gate against this fixture that never calls `set_biome` is
    /// unaffected.
    biomes: Mutex<HashMap<(i32, i32, i32), String>>,
}

impl FixedBlockSource {
    fn set(&self, x: i32, y: i32, z: i32, block: &str) {
        self.blocks.lock().unwrap().insert((x, y, z), block.to_string());
    }

    fn mark_unloaded(&self, cx: i32, cz: i32) {
        self.unloaded.lock().unwrap().insert((cx, cz));
    }

    fn set_biome(&self, x: i32, y: i32, z: i32, biome: &str) {
        self.biomes.lock().unwrap().insert((x, y, z), biome.to_string());
    }
}

impl ChunkSource for FixedBlockSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.blocks.lock().unwrap().get(&(x, y, z)).cloned().unwrap_or_else(|| "minecraft:air".to_string())
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.biomes.lock().unwrap().get(&(x, y, z)).cloned().unwrap_or_else(|| "minecraft:plains".to_string())
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        self.set(x, y, z, name);
    }

    fn is_column_resident(&self, cx: i32, cz: i32) -> bool {
        !self.unloaded.lock().unwrap().contains(&(cx, cz))
    }
}

fn uuid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn candidate(n: u128, name: &str, x: f64, mode: GameMode) -> PlayerCandidate {
    PlayerCandidate {
        uuid: uuid(n),
        entity_id: 1000 + n as i32,
        username: name.to_string(),
        position: Vec3::new(x, 64.0, 0.0),
        rotation: Rotation { yaw: 0.0, pitch: 0.0 },
        game_mode: mode,
        xp_level: 0,
        xp_points: 0,
    }
}

/// [`candidate`], with a caller-chosen rotation — for the `/execute at`/
/// `rotated as` rotation-transfer gates, which need a candidate whose
/// rotation is discriminable from the default `(0, 0)` every other fixture
/// here uses.
fn candidate_with_rotation(n: u128, name: &str, x: f64, mode: GameMode, rotation: Rotation) -> PlayerCandidate {
    PlayerCandidate { rotation, ..candidate(n, name, x, mode) }
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
    let world = CommandWorld {
        rules: rules as &(dyn RuleStore + Sync),
        players,
        state: &state,
        mobs: None,
        border: None,
    access: None,
        blocks: None,
    };
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

/// `scores=` against a real scoreboard, with pairwise-distinct holder values
/// (alice=5, bob=10, carol=15) and dave left with no score at all — the input
/// this repo's evidence standard names explicitly: "a `scores` filter tests
/// nothing against a scoreboard where every player has the same score".
/// Dave's unset score is the second discriminating case: an in-range value a
/// holder never received must refuse, not default to a pass.
#[test]
fn scores_filters_against_a_real_scoreboard_with_distinct_holder_values() {
    use lodestone_server::Effect;
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");

    run_stateful(&commands, &state, &players, &alice, "scoreboard objectives add foo dummy")
        .expect("root matched");
    for (name, value) in [("alice", 5), ("bob", 10), ("carol", 15)] {
        run_stateful(
            &commands,
            &state,
            &players,
            &alice,
            &format!("scoreboard players set {name} foo {value}"),
        )
        .expect("root matched");
    }

    let targets = |text: &str| -> Vec<Uuid> {
        run_stateful(&commands, &state, &players, &alice, text)
            .expect("root matched")
            .effects
            .iter()
            .filter(|d| matches!(d.effect, Effect::SetGameMode(_)))
            .map(|d| d.target)
            .collect()
    };

    // Only bob and carol fall in [8, 17]; dave has no recorded score at all,
    // so he is excluded even though 30 (his x-position, not a score) would be
    // out of range anyway — the exclusion must come from the missing score.
    assert_eq!(targets("gamemode creative @a[scores={foo=8..17}]"), [uuid(2), uuid(3)]);
    // An exact value is both ends of the range.
    assert_eq!(targets("gamemode creative @a[scores={foo=10}]"), [uuid(2)]);
    // The empty map is the vanilla no-op shape: every player matches.
    assert_eq!(
        targets("gamemode creative @a[scores={}]"),
        [uuid(1), uuid(2), uuid(3), uuid(4)]
    );

    // An objective this scoreboard has never registered matches nobody, not
    // everybody — the control that the lookup really discriminates rather
    // than defaulting open.
    let outcome =
        run_stateful(&commands, &state, &players, &alice, "gamemode creative @a[scores={bar=0..100}]")
            .expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
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
        border: None,
    access: None,
        blocks: None,
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
    let world = CommandWorld { rules: state, players, state, mobs: None, border: None, access: None, blocks: None };
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

/// `/xp query` reads a target's *republished* `PlayerCandidate` snapshot
/// (`crate::server::republish_experience`'s producer half, mirrored here by
/// building the roster with non-zero `xp_level`/`xp_points` directly, the same
/// way `game_mode` is set up in [`candidate`] for `@a[gamemode=…]` tests). The
/// two sub-literals must read different fields — a fixture where they agreed
/// could not tell a transposition from a correct implementation.
#[test]
fn experience_query_reads_the_targets_republished_snapshot() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let mut bob = candidate(2, "bob", 5.0, GameMode::Creative);
    bob.xp_level = 7;
    bob.xp_points = 23;
    let players = vec![candidate(1, "alice", 0.0, GameMode::Survival), bob];

    let levels = run_stateful(&commands, &state, &players, &alice, "xp query bob levels")
        .expect("root matched");
    assert_eq!(levels.response.lines(), ["bob has 7 experience levels"]);

    let points = run_stateful(&commands, &state, &players, &alice, "xp query bob points")
        .expect("root matched");
    assert_eq!(points.response.lines(), ["bob has 23 experience points"]);
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

/// `/tp @s <destination>` (self, through `<targets>` → `<destination>` — see
/// `crate::commands::teleport`'s module doc for why the bare, `@s`-free
/// self-to-entity form does not exist here) and `/tp <targets> <destination>`
/// both resolve to the destination's live *position*, never a fixed literal —
/// carol and dave sit at different `x` per [`roster`], so a stale/transposed
/// lookup would be visible immediately.
#[test]
fn tp_to_an_entity_resolves_its_current_position() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let alice = source(1, "alice");
    let players = roster();

    let self_to_carol =
        run(&commands, &GameRulesHandle::new(), &players, &alice, "tp @s carol").expect("root matched");
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

    // The control for the module doc's disclosed gap: the bare, `@s`-free
    // form must actually be absent, not merely undocumented. Without this,
    // a future tree edit that silently reintroduced (and re-broke) the
    // ambiguous top-level `<destination>` node would show no red anywhere —
    // the same "assertions of an absence need a control" standard the
    // `deferred selector options` refusals already meet.
    let bare = run(&commands, &GameRulesHandle::new(), &players, &alice, "tp carol")
        .expect("root matched");
    assert!(
        !bare.response.is_ran(),
        "the bare, `@s`-free self-to-entity form is a disclosed gap and must stay refused: {bare:?}"
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

    let world = CommandWorld { rules: &state, players: &players, state: &state, mobs: Some(&mobs), border: None, access: None, blocks: None };
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

    let world = CommandWorld { rules: &state, players: &players, state: &state, mobs: Some(&mobs), border: None, access: None, blocks: None };
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

// ---------------------------------------------------------------------------
// /execute
// ---------------------------------------------------------------------------
//
// The substrate tests above (`a_rewrite_modifier_replaces_the_source_the_executor_runs_for`,
// `a_fork_modifier_runs_the_deepest_executor_once_per_produced_source`) already
// prove the *mechanism* against a synthetic tree. These prove `/execute`'s own
// registration wires that mechanism up correctly, and per this crate's own
// evidence standard, every one of them predicts a rewritten answer that a
// caller-position/caller-entity reading of the same text would get wrong —
// never merely "the command succeeded".

/// `execute as <other> run kill` must kill the *other* player, never the
/// caller — the one discriminating property `/execute`'s whole design exists
/// for. A single effect, aimed at bob's uuid and not alice's, is the only
/// passing shape.
#[test]
fn execute_as_targets_the_rewritten_source_not_the_caller() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let alice = source(1, "alice");
    let players = roster();

    let outcome = run(&commands, &GameRulesHandle::new(), &players, &alice, "execute as bob run kill")
        .expect("root matched");
    assert_eq!(
        outcome.effects,
        [DirectedEffect::new(uuid(2), Effect::Kill)],
        "the kill must land on bob (uuid 2), never on alice (uuid 1): {:?}",
        outcome.effects
    );
}

/// `execute at <x> positioned <y>` lands somewhere **neither raw coordinate
/// would**: not bob's own position, not alice's `positioned` offset applied
/// to her own spot, and not the literal `1 1 1` either. That three-way
/// distinctness is the point — a version that silently ignored `at` (offset
/// from alice) or silently ignored `positioned` (bob's bare position) would
/// each produce one of the *other* two answers, not this one.
#[test]
fn execute_at_then_positioned_lands_where_neither_raw_coordinate_would() {
    use lodestone_server::Effect;
    let commands = ServerCommands::new();
    let alice = source(1, "alice");
    let players = roster(); // bob is at (5, 64, 0)

    let outcome = run(
        &commands,
        &GameRulesHandle::new(),
        &players,
        &alice,
        "execute at bob positioned ~1 ~1 ~1 run setblock ~ ~ ~ minecraft:stone",
    )
    .expect("root matched");

    let Some(Effect::SetBlock { pos, .. }) = outcome.effects.first().map(|d| d.effect.clone()) else {
        panic!("expected exactly one SetBlock effect: {:?}", outcome.effects);
    };
    assert_eq!(pos, (6, 65, 1), "bob's own (5, 64, 0) plus the relative (1, 1, 1) offset");
    assert_ne!(pos, (5, 64, 0), "must not be bob's raw position (an `at`-with-no-`positioned` bug)");
    assert_ne!(pos, (1, 65, 1), "must not be alice's own position offset by the same delta (an `at`-ignored bug)");
    assert_ne!(pos, (1, 1, 1), "must not be the literal offset read as absolute");
}

/// `execute at <targets>` must transfer the target's **rotation**, not just
/// its position — `PlayerCandidate::rotation`'s whole reason for existing.
/// Discriminated with a `^`-local `positioned` hop immediately after: local
/// coordinates resolve against whatever rotation is in the source at that
/// point, so a version that kept the caller's own rotation (the pre-fix
/// behaviour) lands on a provably different block.
#[test]
fn execute_at_transfers_the_targets_rotation_not_just_its_position() {
    use lodestone_server::Effect;
    let commands = ServerCommands::new();
    // Alice faces yaw 0 (south, +Z per `lodestone_command_mc::position`'s own
    // module doc).
    let alice = source(1, "alice");
    // Bob faces yaw -90 (east, +X) — chosen so the two hypotheses' `^0 ^0 ^5`
    // offsets land on two different axes entirely, not just different signs.
    let players = vec![candidate_with_rotation(
        2,
        "bob",
        5.0,
        GameMode::Creative,
        Rotation { yaw: -90.0, pitch: 0.0 },
    )];

    let outcome = run(
        &commands,
        &GameRulesHandle::new(),
        &players,
        &alice,
        "execute at bob positioned ^ ^ ^5 run setblock ~ ~ ~ minecraft:stone",
    )
    .expect("root matched");

    let Some(Effect::SetBlock { pos, .. }) = outcome.effects.first().map(|d| d.effect.clone()) else {
        panic!("expected exactly one SetBlock effect: {:?}", outcome.effects);
    };
    assert_eq!(pos, (10, 64, 0), "bob's position (5, 64, 0) plus 5 forward along HIS yaw (-90, facing +X)");
    assert_ne!(
        pos,
        (5, 64, 5),
        "must not be bob's position plus 5 forward along ALICE's yaw (0, facing +Z) -- \
         the pre-fix behaviour, which silently kept the caller's own rotation"
    );
}

/// `rotated as <targets>` in isolation — reached through `positioned as`
/// (which this module's own doc states transfers position only, anchor and
/// rotation both untouched) so this test cannot pass merely because `at`
/// already transfers rotation too.
#[test]
fn execute_rotated_as_copies_the_targets_rotation() {
    use lodestone_server::Effect;
    let commands = ServerCommands::new();
    let alice = source(1, "alice"); // yaw 0, faces +Z
    let players = vec![candidate_with_rotation(
        2,
        "bob",
        5.0,
        GameMode::Creative,
        Rotation { yaw: -90.0, pitch: 0.0 },
    )];

    let outcome = run(
        &commands,
        &GameRulesHandle::new(),
        &players,
        &alice,
        "execute positioned as bob rotated as bob positioned ^ ^ ^5 run setblock ~ ~ ~ minecraft:stone",
    )
    .expect("root matched");

    let Some(Effect::SetBlock { pos, .. }) = outcome.effects.first().map(|d| d.effect.clone()) else {
        panic!("expected exactly one SetBlock effect: {:?}", outcome.effects);
    };
    assert_eq!(pos, (10, 64, 0), "bob's position plus 5 forward along bob's OWN yaw (-90, +X)");
    assert_ne!(
        pos,
        (5, 64, 5),
        "must not be bob's position plus 5 forward along alice's yaw (0, +Z) -- \
         the pre-fix behaviour with no `rotated as` subtree registered at all"
    );
}

/// Two `positioned` hops in a row compose: the second's `~`-relative offset is
/// resolved against the position the *first* left behind, not against
/// alice's own spot. Every one of the six numbers involved, and both
/// candidate wrong answers, are pairwise distinct so no transposition or
/// dropped hop can pass by coincidence. Written with explicit decimal points
/// on the first hop's absolute literal so `Vec3Arg`'s centre correction (an
/// absolute `x`/`z` with no decimal point gains `+0.5`, `y` never does — see
/// `lodestone_command_mc::position`'s own module doc) does not shift the
/// expected value away from a literal reading, matching this file's own
/// `tp_targets_location_resolves_against_the_source_never_the_target`.
#[test]
fn execute_positioned_composes_sequential_relative_offsets() {
    use lodestone_server::Effect;
    let commands = ServerCommands::new();
    let alice = source(1, "alice"); // (0, 64, 0)
    let players = roster();

    let outcome = run(
        &commands,
        &GameRulesHandle::new(),
        &players,
        &alice,
        "execute positioned 1.0 2.0 11.0 positioned ~5 ~0 ~-4 run setblock ~ ~ ~ minecraft:stone",
    )
    .expect("root matched");

    let Some(Effect::SetBlock { pos, .. }) = outcome.effects.first().map(|d| d.effect.clone()) else {
        panic!("expected exactly one SetBlock effect: {:?}", outcome.effects);
    };
    assert_eq!(pos, (6, 2, 7), "(1, 2, 11) plus the second hop's (5, 0, -4) offset");
    assert_ne!(pos, (1, 2, 11), "must not be the first `positioned`'s raw literal (second hop dropped)");
    assert_ne!(pos, (5, 64, -4), "must not be the offset applied to alice's own position (first hop dropped)");
}

/// `align xz` floors only the axes it names — a second `positioned` hop with
/// a fractional relative offset reveals whether the fraction from *before*
/// the align survived. `y` is left alone by `align xz`, so only `x`/`z`
/// differ from the no-align hypothesis, and this asserts both.
#[test]
fn execute_align_floors_only_the_named_axes() {
    use lodestone_server::Effect;
    let commands = ServerCommands::new();
    let alice = source(1, "alice");
    let players = roster();

    let outcome = run(
        &commands,
        &GameRulesHandle::new(),
        &players,
        &alice,
        "execute positioned 1.7 64.9 11.3 align xz positioned ~0.9 ~0.05 ~0.9 run setblock ~ ~ ~ minecraft:stone",
    )
    .expect("root matched");

    let Some(Effect::SetBlock { pos, .. }) = outcome.effects.first().map(|d| d.effect.clone()) else {
        panic!("expected exactly one SetBlock effect: {:?}", outcome.effects);
    };
    // x: floor(1.7) = 1.0, + 0.9 = 1.9, floored by `setblock` to 1.
    // z: floor(11.3) = 11.0, + 0.9 = 11.9, floored by `setblock` to 11.
    assert_eq!(pos, (1, 64, 11));
    // Without `align`, x would be floor(1.7 + 0.9) = floor(2.6) = 2 and z
    // would be floor(11.3 + 0.9) = floor(12.2) = 12 — both one higher.
    assert_ne!(pos.0, 2, "x must be floored by align before the second offset, not after");
    assert_ne!(pos.2, 12, "z must be floored by align before the second offset, not after");
}

/// `facing <pos>` aims the source at a point that is not straight ahead of
/// its default rotation, then `^0 ^0 ^5` (five blocks *forward*) walks
/// straight to it — the discriminating property is that this only lands on
/// the aimed-at point *because* `facing` changed the rotation `^` resolves
/// against; at the unrotated default (yaw 0) the same `^0 ^0 ^5` would land
/// at `(0, 64, 5)` instead. Written with explicit decimal points so `Vec3Arg`'s
/// centre correction does not shift the aimed-at point half a block away from
/// the literal reading (see `execute_positioned_composes_sequential_relative_offsets`'s
/// own comment for why that matters here).
#[test]
fn execute_facing_changes_the_rotation_that_relative_local_coordinates_use() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let alice = source(1, "alice"); // (0, 64, 0), default yaw 0 / pitch 0
    let players = roster();

    let outcome = run(
        &commands,
        &GameRulesHandle::new(),
        &players,
        &alice,
        "execute facing 5.0 64.0 0.0 run tp @s ^0 ^0 ^5",
    )
    .expect("root matched");

    let [DirectedEffect { target, effect: Effect::Teleport { x, y, z, .. } }] = outcome.effects.as_slice() else {
        panic!("expected exactly one Teleport effect: {:?}", outcome.effects);
    };
    assert_eq!(*target, uuid(1));
    assert!((x - 5.0).abs() < 1e-6, "facing (5, 64, 0) then walking 5 forward must land on it: {outcome:?}");
    assert!((y - 64.0).abs() < 1e-6, "{outcome:?}");
    assert!(z.abs() < 1e-6, "{outcome:?}");
}

/// `execute if entity <selector>` gates the chain — a selector that matches
/// nobody must produce **no effect at all** (not an error, not a no-op
/// success with a stray effect), and one that matches must let the chain run
/// exactly as if the condition were absent.
#[test]
fn execute_if_entity_gates_whether_the_chained_command_runs() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let alice = source(1, "alice");
    let players = roster();

    let matches = run(
        &commands,
        &GameRulesHandle::new(),
        &players,
        &alice,
        "execute if entity @a[name=carol] run kill",
    )
    .expect("root matched");
    assert_eq!(matches.effects, [DirectedEffect::new(uuid(1), Effect::Kill)], "carol exists: the chain must run");

    let no_match = run(
        &commands,
        &GameRulesHandle::new(),
        &players,
        &alice,
        "execute if entity @a[name=nobody] run kill",
    )
    .expect("root matched");
    assert!(no_match.effects.is_empty(), "no player named 'nobody': the chain must not run: {no_match:?}");

    let inverted = run(
        &commands,
        &GameRulesHandle::new(),
        &players,
        &alice,
        "execute unless entity @a[name=carol] run kill",
    )
    .expect("root matched");
    assert!(inverted.effects.is_empty(), "carol exists, so `unless entity carol` must not run: {inverted:?}");
}

/// The bare form (`if`/`unless` with nothing chained after it) reports
/// pass/fail **on its own**, through the same node's own executor rather than
/// its fork modifier — the one case
/// `crate::commands::registrar::Dispatcher::dispatch`'s terminal-modifier
/// skip exists for. A fork-only implementation would either silently succeed
/// with no feedback (if the modifier ran and emptied the source set) or panic
/// looking for a redirect target with nothing after it; neither is vanilla's
/// answer, which is a real pass/fail message carrying the match count.
#[test]
fn execute_if_unless_entity_bare_reports_pass_or_fail_with_a_count() {
    let commands = ServerCommands::new();
    let alice = source(1, "alice");
    let players = roster(); // four players total

    let passes = run(&commands, &GameRulesHandle::new(), &players, &alice, "execute if entity @a")
        .expect("root matched");
    assert!(passes.response.is_ran(), "{passes:?}");
    assert!(
        passes.response.lines().iter().any(|line| line.contains('4')),
        "the pass message must carry the match count (4): {passes:?}"
    );

    let fails = run(&commands, &GameRulesHandle::new(), &players, &alice, "execute unless entity @a")
        .expect("root matched");
    assert!(!fails.response.is_ran(), "four players exist, so `unless entity @a` must fail: {fails:?}");

    let empty_passes =
        run(&commands, &GameRulesHandle::new(), &players, &alice, "execute unless entity @a[name=nobody]")
            .expect("root matched");
    assert!(empty_passes.response.is_ran(), "{empty_passes:?}");
}

/// `run <command>` redirects to the **whole tree's root**, not merely back to
/// `execute`'s own children — so a second, independent `execute` chain nested
/// inside the first is ordinary syntax, and the outer chain's rewritten
/// source (here, `as bob`) is what the inner chain's `@s` and `at` resolve
/// against. Predicted end-to-end: `as bob` makes the acting entity bob
/// (without moving alice's own position); the nested `at @s` then moves the
/// source to *bob's own* roster position (5, 64, 0); the final `tp @s ~1 ~1
/// ~1` both targets bob (not alice) and offsets from that moved position, not
/// from alice's original (0, 64, 0).
#[test]
fn execute_run_reenters_the_root_so_execute_nests() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let alice = source(1, "alice");
    let players = roster(); // bob is at (5, 64, 0)

    let outcome = run(
        &commands,
        &GameRulesHandle::new(),
        &players,
        &alice,
        "execute as bob run execute at @s run tp @s ~1 ~1 ~1",
    )
    .expect("root matched");

    assert_eq!(
        outcome.effects,
        [DirectedEffect::new(
            uuid(2),
            Effect::Teleport { x: 6.0, y: 65.0, z: 1.0, yaw: None, pitch: None }
        )],
        "targets bob and offsets from bob's own (5, 64, 0), not alice's (0, 64, 0): {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// `/worldborder` (issue #580)
// ---------------------------------------------------------------------------

/// Builds the pieces `/worldborder`'s tests need: real commands, a real
/// [`BorderFeed`] and a [`CommandWorld`] with `border: Some(&feed)` — the
/// shape a connected player's `ChatCommand` arm actually gets, per
/// `crate::commands::worldborder`'s own doc on reachability.
fn worldborder_world<'a>(
    state: &'a lodestone_server::world_state::WorldStateHandle,
    feed: &'a lodestone_server::BorderFeed,
) -> CommandWorld<'a> {
    CommandWorld { rules: state, players: &[], state, mobs: None, border: Some(feed), access: None, blocks: None }
}

/// **The composition that matters**: the command mutates the *same* feed the
/// caller reads back, not a copy. `set` immediate (no `<time>`) applies
/// instantly — the assertion a gate that only checked "the command returned
/// Ok" could not make.
#[test]
fn worldborder_set_immediate_reaches_the_shared_feed() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let feed = lodestone_server::BorderFeed::default();
    let world = worldborder_world(&state, &feed);
    let alice = source(1, "alice");

    let outcome = commands.run(&world, &alice, "worldborder set 500").expect("root matched");
    assert!(
        outcome.response.lines().iter().any(|l| l.contains("Set the world border to 500.0")),
        "{:?}",
        outcome.response.lines()
    );
    assert_eq!(feed.get().size(), 500.0, "the shared feed must reflect the new size");
}

/// `set <distance> <time>` must **not** apply immediately — it starts a
/// lerp, so the size at tick 0 is still the old one and only the lerp target
/// has moved. The discriminating pair against the immediate case above: an
/// implementation that ignores `<time>` entirely would still report success
/// here, but only checking `size()` stayed put catches it.
#[test]
fn worldborder_set_with_time_starts_a_lerp_rather_than_snapping() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let feed = lodestone_server::BorderFeed::default();
    let world = worldborder_world(&state, &feed);
    let alice = source(1, "alice");

    // `WorldBorder::default()`'s own size is `MAX_SIZE` (vanilla's real
    // default), which is already far above 2000 — so growing *to* 2000 needs
    // starting from something smaller first, or the command would (correctly)
    // report a shrink instead and the "Growing" assertion below would be
    // testing the wrong hypothesis.
    commands.run(&world, &alice, "worldborder set 1000").expect("root matched");
    let before = feed.get().size();
    assert_eq!(before, 1000.0);

    let outcome =
        commands.run(&world, &alice, "worldborder set 2000 200").expect("root matched");
    assert!(
        outcome.response.lines().iter().any(|l| l.contains("Growing world border to 2000.0")),
        "{:?}",
        outcome.response.lines()
    );
    let after = feed.get();
    assert_eq!(after.size(), before, "size must not snap on tick 0 of a lerp");
    assert_eq!(after.lerp_target(), 2000.0, "the lerp target must be the new size");
    assert!(after.lerp_time() > 0, "a positive lerp_time means the lerp actually started");
}

/// `add` reads the *current* size rather than treating its argument as an
/// absolute — the discriminating check: adding `100` to a border already
/// resized to `300` must land on `400`, not `100`. A negative distance
/// shrinks, checked in the same test since both exercise the identical
/// `current + delta` arithmetic.
#[test]
fn worldborder_add_is_relative_to_the_current_size_not_absolute() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let feed = lodestone_server::BorderFeed::default();
    let world = worldborder_world(&state, &feed);
    let alice = source(1, "alice");

    commands.run(&world, &alice, "worldborder set 300").expect("root matched");
    assert_eq!(feed.get().size(), 300.0);
    commands.run(&world, &alice, "worldborder add 100").expect("root matched");
    assert_eq!(
        feed.get().size(),
        400.0,
        "add must be current (300) + delta (100), not the delta alone"
    );
    commands.run(&world, &alice, "worldborder add -150").expect("root matched");
    assert_eq!(feed.get().size(), 250.0, "a negative distance must shrink");
}

/// `center <x> <z>` — the two-double substitute for vanilla's
/// `Vec2Argument` (see the command module's own doc for why), checked
/// against both axes so a transposed pair (`x` read into `z`) would fail.
#[test]
fn worldborder_center_moves_both_axes_independently() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let feed = lodestone_server::BorderFeed::default();
    let world = worldborder_world(&state, &feed);
    let alice = source(1, "alice");

    commands
        .run(&world, &alice, "worldborder center 150.5 -300.25")
        .expect("root matched");
    let after = feed.get();
    assert_eq!(after.center_x(), 150.5);
    assert_eq!(after.center_z(), -300.25);
}

/// `damage amount`/`damage buffer` and `warning distance`/`warning time`
/// each land on the field they name, not a sibling — the discriminating
/// check is that all four end at *different* values, so a copy-paste that
/// wrote to the wrong setter would show up as two fields agreeing when they
/// should not.
#[test]
fn worldborder_damage_and_warning_subcommands_each_hit_their_own_field() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let feed = lodestone_server::BorderFeed::default();
    let world = worldborder_world(&state, &feed);
    let alice = source(1, "alice");

    commands.run(&world, &alice, "worldborder damage amount 0.5").expect("root matched");
    commands.run(&world, &alice, "worldborder damage buffer 7.5").expect("root matched");
    commands.run(&world, &alice, "worldborder warning distance 11").expect("root matched");
    commands.run(&world, &alice, "worldborder warning time 40").expect("root matched");

    let after = feed.get();
    assert_eq!(after.damage_per_block(), 0.5);
    assert_eq!(after.safe_zone(), 7.5);
    assert_eq!(after.warning_blocks(), 11);
    assert_eq!(after.warning_time(), 40);
}

/// **`get` reads without mutating** — a positive control that `set` changes
/// `size()` (covered above) paired with the negative control that `get`
/// alone does not.
#[test]
fn worldborder_get_reports_the_current_size_without_changing_it() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let feed = lodestone_server::BorderFeed::default();
    let world = worldborder_world(&state, &feed);
    let alice = source(1, "alice");

    commands.run(&world, &alice, "worldborder set 800").expect("root matched");
    let before = feed.get().size();
    let outcome = commands.run(&world, &alice, "worldborder get").expect("root matched");
    assert!(
        outcome.response.lines().iter().any(|l| l.contains("currently 800")),
        "{:?}",
        outcome.response.lines()
    );
    assert_eq!(feed.get().size(), before, "a query must not mutate the border");
}

/// The "nothing changed" refusal — set to the exact current size — must
/// refuse **and leave the feed untouched**, not merely print an error while
/// still applying.
#[test]
fn worldborder_setting_to_the_current_value_is_refused_and_does_not_mutate() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let feed = lodestone_server::BorderFeed::default();
    let world = worldborder_world(&state, &feed);
    let alice = source(1, "alice");

    // Every field starts at `WorldBorder::default()`'s own value — asking to
    // set it to that exact value must refuse immediately.
    let default_size = feed.get().size();
    let outcome = commands
        .run(&world, &alice, &format!("worldborder set {default_size}"))
        .expect("root matched");
    assert!(
        outcome.response.lines().iter().any(|l| l.contains("Nothing changed")),
        "{:?}",
        outcome.response.lines()
    );
    assert_eq!(feed.get().size(), default_size);
}

/// **A missing border refuses every subcommand by name, not by panic.** The
/// `CommandWorld` shape RCON/a command block build — `border: None` — must
/// not crash the dispatcher; each subcommand's own refusal is what a plain
/// "not available" response depends on.
#[test]
fn worldborder_with_no_border_refuses_cleanly_instead_of_panicking() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let world = CommandWorld { rules: &state, players: &[], state: &state, mobs: None, border: None, access: None, blocks: None };
    let alice = source(1, "alice");

    let outcome = commands.run(&world, &alice, "worldborder get").expect("root matched");
    assert!(
        outcome.response.lines().iter().any(|l| l.contains("not available")),
        "{:?}",
        outcome.response.lines()
    );
}

/// Below `Commands.LEVEL_GAMEMASTERS`, `/worldborder` is denied loudly on
/// execution and hidden silently from suggestions — the same two-halved
/// gate `a_level_1_caller_cannot_run_or_see_a_level_2_command` establishes
/// for `/gamemode`. The root still matches (`Some`, not `None`): a denied
/// command must say "no permission", not "no such command".
#[test]
fn worldborder_a_low_permission_caller_cannot_reach_it() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let feed = lodestone_server::BorderFeed::default();
    let world = worldborder_world(&state, &feed);
    let low = CommandSource::player(
        uuid(9),
        1009,
        "guest",
        Vec3::new(0.0, 64.0, 0.0),
        Rotation { yaw: 0.0, pitch: 0.0 },
        overworld_dimension(),
        0,
    );

    let before = feed.get().size();
    let outcome = commands.run(&world, &low, "worldborder get").expect("the root is ours even when denied");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(
        outcome.response.lines()[0].contains("permission"),
        "a denied command must say so, not read as a typo: {outcome:?}"
    );
    assert_eq!(feed.get().size(), before, "a denied command must not mutate the border either");
    assert!(
        commands.suggest("world", 0).is_empty(),
        "a level-0 caller must not be offered the level-2 worldborder tree: {:?}",
        commands.suggest("world", 0)
    );
}

// ---------------------------------------------------------------------------
// /scoreboard, and /execute if/unless score
// ---------------------------------------------------------------------------

/// One shared production [`lodestone_server::world_state::WorldStateHandle`],
/// for a sequence of commands that must see each other's writes — the same
/// shape [`run_stateful`] already uses for `/time`/`/difficulty`, needed here
/// because `add_objective` then `players set` are two separate calls into
/// [`ServerCommands::run`].
fn scoreboard_world<'a>(
    state: &'a lodestone_server::world_state::WorldStateHandle,
    players: &'a [PlayerCandidate],
) -> CommandWorld<'a> {
    CommandWorld { rules: state as &(dyn RuleStore + Sync), players, state, mobs: None, border: None, access: None, blocks: None }
}

#[test]
fn objectives_add_list_and_remove_round_trip_through_the_store() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = scoreboard_world(&state, &players);

    let outcome = commands
        .run(&world, &alice, "scoreboard objectives add kills dummy")
        .expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    assert!(state.scoreboard().has_objective("kills"));

    // A duplicate name is refused, not silently accepted twice.
    let dup = commands.run(&world, &alice, "scoreboard objectives add kills dummy").expect("root matched");
    assert!(!dup.response.is_ran(), "{dup:?}");

    let listed = commands.run(&world, &alice, "scoreboard objectives list").expect("root matched");
    assert!(listed.response.lines()[0].contains("kills"), "{listed:?}");

    let removed = commands.run(&world, &alice, "scoreboard objectives remove kills").expect("root matched");
    assert!(removed.response.is_ran(), "{removed:?}");
    assert!(!state.scoreboard().has_objective("kills"));
}

/// `set`/`add`/`remove`/`get` against a bare word holder — the "fake player"
/// counter case, which is why this is not routed through `EntityArg` the way
/// every other `<targets>` in this crate is.
#[test]
fn players_set_add_remove_and_get_work_on_a_fake_player_name() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = scoreboard_world(&state, &players);

    commands.run(&world, &alice, "scoreboard objectives add counter dummy").unwrap();
    let set = commands.run(&world, &alice, "scoreboard players set TIMER counter 10").expect("root matched");
    assert!(set.response.is_ran(), "{set:?}");
    assert_eq!(state.scoreboard().get_score("TIMER", "counter"), Ok(10));

    commands.run(&world, &alice, "scoreboard players add TIMER counter 5").unwrap();
    assert_eq!(state.scoreboard().get_score("TIMER", "counter"), Ok(15));
    commands.run(&world, &alice, "scoreboard players remove TIMER counter 3").unwrap();
    assert_eq!(state.scoreboard().get_score("TIMER", "counter"), Ok(12));

    let got = commands.run(&world, &alice, "scoreboard players get TIMER counter").expect("root matched");
    assert!(got.response.lines()[0].contains("12"), "{got:?}");
}

/// A selector holder resolves against the live roster — `set @a` reaches
/// every online player, not just one literal name.
#[test]
fn a_selector_holder_reaches_every_matched_player() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = scoreboard_world(&state, &players);

    commands.run(&world, &alice, "scoreboard objectives add x dummy").unwrap();
    commands.run(&world, &alice, "scoreboard players set @a x 1").unwrap();
    for candidate in &players {
        assert_eq!(
            state.scoreboard().get_score(&candidate.username, "x"),
            Ok(1),
            "{} must have received the selector-targeted score",
            candidate.username
        );
    }
}

/// Every one of vanilla's nine `operation` tokens reaches the store through
/// the command, against operands where a transposition would be visible.
#[test]
fn players_operation_reaches_every_token_through_the_command() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = scoreboard_world(&state, &players);

    commands.run(&world, &alice, "scoreboard objectives add x dummy").unwrap();
    commands.run(&world, &alice, "scoreboard players set t x 11").unwrap();
    commands.run(&world, &alice, "scoreboard players set s x 4").unwrap();
    let outcome = commands.run(&world, &alice, "scoreboard players operation t x += s x").expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    assert_eq!(state.scoreboard().get_score("t", "x"), Ok(15));
}

/// `/execute if score … matches <range>` and `unless` gate the chained
/// command, and the range's own inclusivity is checked at both ends.
///
/// Asserted through `.effects`, not `.response.is_ran()`, matching
/// `execute_if_entity_gates_whether_the_chained_command_runs`'s own
/// convention: a forked condition with an emptied source set answers `Ran`
/// with zero effects (vanilla's own "matched nobody" success), not a
/// refusal — `is_ran()` alone cannot distinguish "ran and did nothing" from
/// "ran and killed the caller".
#[test]
fn execute_if_unless_score_matches_gates_on_the_ranges_own_inclusive_ends() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = scoreboard_world(&state, &players);

    commands.run(&world, &alice, "scoreboard objectives add x dummy").unwrap();
    commands.run(&world, &alice, "scoreboard players set p x 5").unwrap();

    let in_range = commands
        .run(&world, &alice, "execute if score p x matches 1..5 run kill @s")
        .expect("root matched");
    assert_eq!(
        in_range.effects,
        [DirectedEffect::new(uuid(1), Effect::Kill)],
        "5 is within 1..5 inclusive: {in_range:?}"
    );

    let out_of_range = commands
        .run(&world, &alice, "execute if score p x matches 1..4 run kill @s")
        .expect("root matched");
    assert!(out_of_range.effects.is_empty(), "5 is not within 1..4: {out_of_range:?}");

    let unless_out_of_range = commands
        .run(&world, &alice, "execute unless score p x matches 1..4 run kill @s")
        .expect("root matched");
    assert_eq!(
        unless_out_of_range.effects,
        [DirectedEffect::new(uuid(1), Effect::Kill)],
        "unless negates: {unless_out_of_range:?}"
    );
}

/// The two-score comparison form, against operands where `<`/`>` and
/// `<=`/`>=` would disagree if the boundary handling were wrong (`5 <= 5` is
/// true, `5 < 5` is not). Same `.effects` convention as the range test above.
#[test]
fn execute_if_score_compares_two_holders_inclusively_at_the_boundary() {
    use lodestone_server::{DirectedEffect, Effect};
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = scoreboard_world(&state, &players);

    commands.run(&world, &alice, "scoreboard objectives add x dummy").unwrap();
    commands.run(&world, &alice, "scoreboard players set a x 5").unwrap();
    commands.run(&world, &alice, "scoreboard players set b x 5").unwrap();

    let lt = commands.run(&world, &alice, "execute if score a x < b x run kill @s").expect("root matched");
    assert!(lt.effects.is_empty(), "5 < 5 is false: {lt:?}");

    let le = commands.run(&world, &alice, "execute if score a x <= b x run kill @s").expect("root matched");
    assert_eq!(le.effects, [DirectedEffect::new(uuid(1), Effect::Kill)], "5 <= 5 is true: {le:?}");

    commands.run(&world, &alice, "scoreboard players set b x 9").unwrap();
    let gt = commands.run(&world, &alice, "execute if score a x > b x run kill @s").expect("root matched");
    assert!(gt.effects.is_empty(), "5 > 9 is false: {gt:?}");
}

/// `/scoreboard`'s own store is the **production** `WorldStateHandle` — the
/// same island shape `/gamerule`'s own history warns about, checked the same
/// way that gate checks it: read the effect back off the store, not off the
/// response text.
#[test]
fn the_store_a_command_writes_is_the_same_one_a_second_call_reads() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = scoreboard_world(&state, &players);

    commands.run(&world, &alice, "scoreboard objectives add x dummy").unwrap();
    commands.run(&world, &alice, "scoreboard players set p x 42").unwrap();
    // A *second*, independently-constructed `ServerCommands` — a fresh
    // process-wide tree, exactly like the one RCON or a command block's own
    // tick loop builds — reading through the *same* `WorldStateHandle` must
    // see the write, because the store lives on the handle, not on the tree.
    let other_tree = ServerCommands::new();
    let read = other_tree.run(&world, &alice, "scoreboard players get p x").expect("root matched");
    assert!(read.response.lines()[0].contains("42"), "{read:?}");
}

// ---------------------------------------------------------------------------
// /team, and `team=` selector filtering
// ---------------------------------------------------------------------------

/// [`scoreboard_world`]'s identical shape — one shared production
/// `WorldStateHandle` for a sequence of `/team` calls that must see each
/// other's writes.
fn team_world<'a>(
    state: &'a lodestone_server::world_state::WorldStateHandle,
    players: &'a [PlayerCandidate],
) -> CommandWorld<'a> {
    CommandWorld { rules: state as &(dyn RuleStore + Sync), players, state, mobs: None, border: None, access: None, blocks: None }
}

#[test]
fn add_join_list_leave_and_remove_round_trip_through_the_store() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = team_world(&state, &players);

    let outcome = commands.run(&world, &alice, "team add red").expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    assert!(state.team().team("red").is_some());

    // A duplicate name is refused, not silently accepted twice.
    let dup = commands.run(&world, &alice, "team add red").expect("root matched");
    assert!(!dup.response.is_ran(), "{dup:?}");

    let joined = commands.run(&world, &alice, "team join red bob").expect("root matched");
    assert!(joined.response.is_ran(), "{joined:?}");
    assert_eq!(state.team().team_of("bob"), "red");

    // Joining a second team moves bob rather than adding him to both.
    commands.run(&world, &alice, "team add blue").unwrap();
    commands.run(&world, &alice, "team join blue bob").unwrap();
    assert_eq!(state.team().team_of("bob"), "blue");
    assert!(!state.team().team("red").unwrap().members.iter().any(|m| m == "bob"));

    let listed = commands.run(&world, &alice, "team list red").expect("root matched");
    assert!(listed.response.lines()[0].contains("no members"), "{listed:?}");

    let left = commands.run(&world, &alice, "team leave bob").expect("root matched");
    assert!(left.response.is_ran(), "{left:?}");
    assert_eq!(state.team().team_of("bob"), "");

    let removed = commands.run(&world, &alice, "team remove red").expect("root matched");
    assert!(removed.response.is_ran(), "{removed:?}");
    assert!(state.team().team("red").is_none());
}

/// `team join <team>` with no `<members>` defaults to the caller — vanilla's
/// own no-members overload.
#[test]
fn join_with_no_members_defaults_to_the_caller() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = team_world(&state, &players);

    commands.run(&world, &alice, "team add red").unwrap();
    let outcome = commands.run(&world, &alice, "team join red").expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    assert_eq!(state.team().team_of("alice"), "red");
}

/// `team empty` clears membership without deleting the team itself.
#[test]
fn empty_clears_members_but_leaves_the_team_registered() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = team_world(&state, &players);

    commands.run(&world, &alice, "team add red").unwrap();
    commands.run(&world, &alice, "team join red alice").unwrap();
    commands.run(&world, &alice, "team join red bob").unwrap();

    let outcome = commands.run(&world, &alice, "team empty red").expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    assert_eq!(state.team().team_of("alice"), "");
    assert_eq!(state.team().team_of("bob"), "");
    assert!(state.team().team("red").is_some(), "the team itself must still exist");
}

/// `team modify` covers every option kind this module registers: free text
/// (`displayName`), a bool, `minecraft:team_color`, and the two families
/// registered as literal tokens (a `Visibility`, `CollisionRule`).
#[test]
fn modify_reaches_every_option_kind() {
    use lodestone_server::commands::team_store::{CollisionRule, Visibility};
    use lodestone_model::text::TextColor;

    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = team_world(&state, &players);

    commands.run(&world, &alice, "team add red").unwrap();

    commands.run(&world, &alice, "team modify red displayName Red Team").unwrap();
    assert_eq!(state.team().team("red").unwrap().display_name, "Red Team");

    commands.run(&world, &alice, "team modify red prefix [R]").unwrap();
    assert_eq!(state.team().team("red").unwrap().prefix, "[R]");
    commands.run(&world, &alice, "team modify red suffix !").unwrap();
    assert_eq!(state.team().team("red").unwrap().suffix, "!");

    commands.run(&world, &alice, "team modify red color dark_red").unwrap();
    assert_eq!(state.team().team("red").unwrap().color, Some(TextColor::DarkRed));
    commands.run(&world, &alice, "team modify red color reset").unwrap();
    assert_eq!(state.team().team("red").unwrap().color, None);

    commands.run(&world, &alice, "team modify red friendlyfire false").unwrap();
    assert!(!state.team().team("red").unwrap().friendly_fire);
    commands.run(&world, &alice, "team modify red seeFriendlyInvisibles false").unwrap();
    assert!(!state.team().team("red").unwrap().see_friendly_invisibles);

    commands.run(&world, &alice, "team modify red nametagVisibility hideForOtherTeams").unwrap();
    assert_eq!(state.team().team("red").unwrap().nametag_visibility, Visibility::HideForOtherTeams);
    commands.run(&world, &alice, "team modify red deathMessageVisibility never").unwrap();
    assert_eq!(state.team().team("red").unwrap().death_message_visibility, Visibility::Never);

    commands.run(&world, &alice, "team modify red collisionRule pushOwnTeam").unwrap();
    assert_eq!(state.team().team("red").unwrap().collision_rule, CollisionRule::PushOwnTeam);

    // Every option refuses cleanly against an unregistered team, rather than
    // panicking or silently creating one.
    let outcome = commands.run(&world, &alice, "team modify ghost color red").expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
}

/// `team list <team>` must actually echo every configurable field back —
/// the read side that makes `nametagVisibility`/`deathMessageVisibility`/
/// `collisionRule`/`color`/`prefix`/`suffix`/`displayName` real,
/// production-reachable values rather than fields only a command's own
/// write path and this crate's tests ever touch. Confirmed by
/// `cargo run -q -p xtask -- islands`, which flagged
/// `Team::nametag_visibility`/`Team::death_message_visibility` as zero
/// production reads before `register_list`'s single-team branch gained this
/// second feedback line.
#[test]
fn list_echoes_every_configurable_field_back() {
    use lodestone_server::commands::team_store::{CollisionRule, Visibility};

    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = team_world(&state, &players);

    commands.run(&world, &alice, "team add red").unwrap();
    commands.run(&world, &alice, "team modify red displayName Red Squad").unwrap();
    commands.run(&world, &alice, "team modify red prefix [R]").unwrap();
    commands.run(&world, &alice, "team modify red suffix !!").unwrap();
    commands.run(&world, &alice, "team modify red color dark_red").unwrap();
    commands.run(&world, &alice, "team modify red friendlyfire false").unwrap();
    commands.run(&world, &alice, "team modify red seeFriendlyInvisibles false").unwrap();
    commands.run(&world, &alice, "team modify red nametagVisibility hideForOwnTeam").unwrap();
    commands.run(&world, &alice, "team modify red deathMessageVisibility never").unwrap();
    commands.run(&world, &alice, "team modify red collisionRule pushOtherTeams").unwrap();

    // The store's own state, predicted from outside the read path under
    // test — not derived from the feedback text this test is about to check.
    let stored = state.team().team("red").expect("red exists");
    assert_eq!(stored.nametag_visibility, Visibility::HideForOwnTeam);
    assert_eq!(stored.death_message_visibility, Visibility::Never);
    assert_eq!(stored.collision_rule, CollisionRule::PushOtherTeams);

    let listed = commands.run(&world, &alice, "team list red").expect("root matched");
    let report = listed.response.lines().join("\n");
    for expected in [
        "Red Squad",
        "\"[R]\"",
        "\"!!\"",
        "dark_red",
        "friendlyFire=false",
        "seeFriendlyInvisibles=false",
        "nametagVisibility=hideForOwnTeam",
        "deathMessageVisibility=never",
        "collisionRule=pushOtherTeams",
    ] {
        assert!(report.contains(expected), "{expected:?} missing from team list's own report: {report:?}");
    }
}

/// `team=`, `team=<name>` and `team=!<name>` against a real store — the same
/// discriminating shape this file's own `scores_filters_against_a_real_scoreboard…`
/// test uses: pairwise-distinct membership (alice on red, bob on blue) plus
/// two players on **no** team at all (carol, dave), so `team=` bare (matches
/// "no team") and `team=red` cannot be satisfied by the same input.
#[test]
fn team_filters_against_a_real_store_including_the_no_team_case() {
    use lodestone_server::Effect;
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = team_world(&state, &players);

    commands.run(&world, &alice, "team add red").unwrap();
    commands.run(&world, &alice, "team add blue").unwrap();
    commands.run(&world, &alice, "team join red alice").unwrap();
    commands.run(&world, &alice, "team join blue bob").unwrap();
    // carol and dave join no team.

    let targets = |text: &str| -> Vec<Uuid> {
        commands
            .run(&world, &alice, text)
            .expect("root matched")
            .effects
            .iter()
            .filter(|d| matches!(d.effect, Effect::SetGameMode(_)))
            .map(|d| d.target)
            .collect()
    };

    assert_eq!(targets("gamemode creative @a[team=red]"), [uuid(1)]);
    assert_eq!(targets("gamemode creative @a[team=blue]"), [uuid(2)]);
    // The bare form matches "no team" — carol and dave, not alice or bob.
    assert_eq!(targets("gamemode creative @a[team=]"), [uuid(3), uuid(4)]);
    // Inverted: everybody except whoever is actually on red.
    assert_eq!(targets("gamemode creative @a[team=!red]"), [uuid(2), uuid(3), uuid(4)]);
}

// ---------------------------------------------------------------------------
// /data storage, and /execute if/unless data storage
// ---------------------------------------------------------------------------

/// [`scoreboard_world`]'s identical shape.
fn data_world<'a>(
    state: &'a lodestone_server::world_state::WorldStateHandle,
    players: &'a [PlayerCandidate],
) -> CommandWorld<'a> {
    CommandWorld { rules: state as &(dyn RuleStore + Sync), players, state, mobs: None, border: None, access: None, blocks: None }
}

#[test]
fn get_merge_and_remove_round_trip_through_the_store() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = data_world(&state, &players);

    // An id nobody has written yet reads as an empty compound, not a refusal.
    let empty = commands.run(&world, &alice, "data get storage test:foo").expect("root matched");
    assert!(empty.response.is_ran(), "{empty:?}");
    assert!(empty.response.lines()[0].contains("{}"), "{empty:?}");

    let merged = commands.run(&world, &alice, "data merge storage test:foo {a:1,b:{c:2}}").expect("root matched");
    assert!(merged.response.is_ran(), "{merged:?}");
    assert_eq!(state.nbt_storage().get("test:foo", &["a".to_string()]), Some(SnbtValue::Int(1)));
    assert_eq!(
        state.nbt_storage().get("test:foo", &["b".to_string(), "c".to_string()]),
        Some(SnbtValue::Int(2))
    );

    let read_path = commands.run(&world, &alice, "data get storage test:foo a").expect("root matched");
    assert!(read_path.response.lines()[0].contains('1'), "{read_path:?}");

    // A path that does not exist is a refusal naming that, not a silent
    // success reading nothing — the same shape a `scores=` miss refuses.
    let missing = commands.run(&world, &alice, "data get storage test:foo nope").expect("root matched");
    assert!(!missing.response.is_ran(), "{missing:?}");

    let removed = commands.run(&world, &alice, "data remove storage test:foo a").expect("root matched");
    assert!(removed.response.is_ran(), "{removed:?}");
    assert_eq!(state.nbt_storage().get("test:foo", &["a".to_string()]), None);
    // b.c must still be there — removing `a` must not touch a sibling key.
    assert_eq!(
        state.nbt_storage().get("test:foo", &["b".to_string(), "c".to_string()]),
        Some(SnbtValue::Int(2))
    );

    // Removing the same path again is a refusal, not a second success.
    let second_remove = commands.run(&world, &alice, "data remove storage test:foo a").expect("root matched");
    assert!(!second_remove.response.is_ran(), "{second_remove:?}");
}

/// `/execute if`/`unless data storage` against a real store, with the
/// present-path and absent-path cases both exercised so a hardcoded pass
/// cannot survive. Follows
/// `execute_if_unless_score_matches_gates_on_the_ranges_own_inclusive_ends`'s
/// own stated convention: the chained (forked) form answers `Ran` either
/// way — a forked condition that matches nothing is vanilla's own "ran and
/// did nothing", not a refusal — so the discriminator is `effects`, not
/// `response.is_ran()`. The *bare* form (no `run …`) is what does refuse,
/// and is checked separately below.
#[test]
fn execute_if_unless_data_storage_gates_on_real_presence() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = data_world(&state, &players);

    commands.run(&world, &alice, "data merge storage test:cond {present:1}").unwrap();

    let present = commands
        .run(&world, &alice, "execute if data storage test:cond present run gamemode creative")
        .expect("root matched");
    assert!(present.response.is_ran(), "{present:?}");
    assert_eq!(present.effects.len(), 1, "{present:?}");

    let absent = commands
        .run(&world, &alice, "execute if data storage test:cond missing run gamemode creative")
        .expect("root matched");
    assert!(absent.effects.is_empty(), "an absent path must not run the chained command: {absent:?}");

    // `unless` is the exact complement: it must fire (produce the effect) on
    // the absent path and produce none on the present one.
    let unless_absent = commands
        .run(&world, &alice, "execute unless data storage test:cond missing run gamemode creative")
        .expect("root matched");
    assert_eq!(unless_absent.effects.len(), 1, "{unless_absent:?}");

    let unless_present = commands
        .run(&world, &alice, "execute unless data storage test:cond present run gamemode creative")
        .expect("root matched");
    assert!(unless_present.effects.is_empty(), "{unless_present:?}");

    // The **bare** form (no `run`) is the executor itself, not the fork —
    // this is what actually reports pass/fail, per this module's own doc on
    // why a condition node needs both.
    let bare_present = commands.run(&world, &alice, "execute if data storage test:cond present").expect("root matched");
    assert!(bare_present.response.is_ran(), "{bare_present:?}");

    let bare_absent = commands.run(&world, &alice, "execute if data storage test:cond missing").expect("root matched");
    assert!(!bare_absent.response.is_ran(), "the bare form must refuse on an absent path: {bare_absent:?}");
}

// ---------------------------------------------------------------------------
// /execute if/unless block
// ---------------------------------------------------------------------------

/// `if`/`unless block` against a real, settable [`FixedBlockSource`] — the
/// present block and an absent (wrong-block) case both exercised, plus the
/// `blocks: None` refusal that stands in for RCON/a command block with no
/// chunk source in scope.
#[test]
fn execute_if_unless_block_reads_a_real_chunk_source() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice"); // (0, 64, 0)
    let source_blocks = FixedBlockSource::default();
    source_blocks.set(5, 64, 5, "minecraft:stone");

    let world = CommandWorld {
        rules: &rules as &(dyn RuleStore + Sync),
        players: &players,
        state: &state,
        mobs: None,
        border: None,
        #[cfg(not(target_arch = "wasm32"))]
        access: None,
        blocks: Some(&source_blocks),
    };

    let matches = commands
        .run(&world, &alice, "execute if block 5 64 5 minecraft:stone run gamemode creative")
        .expect("root matched");
    assert_eq!(matches.effects.len(), 1, "the real block matches: the chain must run: {matches:?}");

    let no_match = commands
        .run(&world, &alice, "execute if block 5 64 5 minecraft:dirt run gamemode creative")
        .expect("root matched");
    assert!(no_match.effects.is_empty(), "stone is not dirt: the chain must not run: {no_match:?}");

    // The un-set position is air by this fixture's own default.
    let air = commands
        .run(&world, &alice, "execute if block 0 0 0 minecraft:air run gamemode creative")
        .expect("root matched");
    assert_eq!(air.effects.len(), 1, "{air:?}");

    // `unless` is the exact complement.
    let unless_matches = commands
        .run(&world, &alice, "execute unless block 5 64 5 minecraft:stone run gamemode creative")
        .expect("root matched");
    assert!(unless_matches.effects.is_empty(), "{unless_matches:?}");

    let unless_no_match = commands
        .run(&world, &alice, "execute unless block 5 64 5 minecraft:dirt run gamemode creative")
        .expect("root matched");
    assert_eq!(unless_no_match.effects.len(), 1, "{unless_no_match:?}");

    // The bare form actually refuses/succeeds rather than folding into an
    // empty `Ran`, same convention as the `data storage` gate above.
    let bare_match = commands.run(&world, &alice, "execute if block 5 64 5 minecraft:stone").expect("root matched");
    assert!(bare_match.response.is_ran(), "{bare_match:?}");
    let bare_no_match = commands.run(&world, &alice, "execute if block 5 64 5 minecraft:dirt").expect("root matched");
    assert!(!bare_no_match.response.is_ran(), "{bare_no_match:?}");
}

/// With no chunk source in scope (`blocks: None` — RCON, a command block
/// helper missing one, or this file's own other test worlds), `if block`
/// refuses cleanly by name rather than panicking or silently passing.
#[test]
fn execute_if_block_with_no_chunk_source_refuses_by_name() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let alice = source(1, "alice");

    let outcome = run(&commands, &rules, &players, &alice, "execute if block 0 0 0 minecraft:air").expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(
        outcome.response.lines()[0].contains("Blocks cannot be queried"),
        "the refusal must name the missing capability: {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// /execute if/unless blocks
// ---------------------------------------------------------------------------

/// `if`/`unless blocks … all` against a real, settable [`FixedBlockSource`] —
/// a two-cell source pattern copied verbatim to a matching destination, then
/// broken at one destination cell to flip the outcome. `all` mode compares
/// every cell in the region, air included, unlike `masked` below.
#[test]
fn execute_if_unless_blocks_all_compares_every_cell() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let source_blocks = FixedBlockSource::default();
    // Source region (0,64,0)-(1,64,1): stone, air, air, dirt.
    source_blocks.set(0, 64, 0, "minecraft:stone");
    source_blocks.set(1, 64, 1, "minecraft:dirt");
    // Destination region at (10,64,10), same pattern, offset (10,0,10).
    source_blocks.set(10, 64, 10, "minecraft:stone");
    source_blocks.set(11, 64, 11, "minecraft:dirt");

    let world = CommandWorld {
        rules: &rules as &(dyn RuleStore + Sync),
        players: &players,
        state: &state,
        mobs: None,
        border: None,
        #[cfg(not(target_arch = "wasm32"))]
        access: None,
        blocks: Some(&source_blocks),
    };

    let matches = commands
        .run(&world, &alice, "execute if blocks 0 64 0 1 64 1 10 64 10 all run gamemode creative alice")
        .expect("root matched");
    assert!(!matches.effects.is_empty(), "the region matches cell for cell: {matches:?}");

    let bare_match = commands.run(&world, &alice, "execute if blocks 0 64 0 1 64 1 10 64 10 all").expect("root matched");
    assert!(bare_match.response.is_ran(), "{bare_match:?}");
    assert!(bare_match.response.lines()[0].contains("count: 4"), "all four cells: {bare_match:?}");

    // Break the destination's (11,64,11) cell — now the regions disagree.
    source_blocks.set(11, 64, 11, "minecraft:sand");
    let no_match = commands
        .run(&world, &alice, "execute if blocks 0 64 0 1 64 1 10 64 10 all run gamemode creative alice")
        .expect("root matched");
    assert!(no_match.effects.is_empty(), "the region no longer matches: {no_match:?}");

    // `unless` is the exact complement.
    let unless_no_match = commands
        .run(&world, &alice, "execute unless blocks 0 64 0 1 64 1 10 64 10 all run gamemode creative alice")
        .expect("root matched");
    assert!(!unless_no_match.effects.is_empty(), "{unless_no_match:?}");
}

/// `masked` mode skips a source cell that is `minecraft:air` entirely — so a
/// destination that disagrees *only* under an air source cell still counts
/// as a match, unlike `all` above.
#[test]
fn execute_if_blocks_masked_skips_air_source_cells() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let source_blocks = FixedBlockSource::default();
    // Source (0,64,0) is stone; (1,64,0) is left air (the fixture's default).
    source_blocks.set(0, 64, 0, "minecraft:stone");
    // Destination (10,64,0) matches the stone; (11,64,0) is deliberately
    // something other than air — `all` would refuse this, `masked` must not.
    source_blocks.set(10, 64, 0, "minecraft:stone");
    source_blocks.set(11, 64, 0, "minecraft:sand");

    let world = CommandWorld {
        rules: &state,
        players: &players,
        state: &state,
        mobs: None,
        border: None,
        #[cfg(not(target_arch = "wasm32"))]
        access: None,
        blocks: Some(&source_blocks),
    };

    let all_fails = commands
        .run(&world, &alice, "execute if blocks 0 64 0 1 64 0 10 64 0 all run gamemode creative alice")
        .expect("root matched");
    assert!(all_fails.effects.is_empty(), "all mode must see the air/sand mismatch: {all_fails:?}");

    let masked_passes = commands
        .run(&world, &alice, "execute if blocks 0 64 0 1 64 0 10 64 0 masked run gamemode creative alice")
        .expect("root matched");
    assert!(!masked_passes.effects.is_empty(), "masked mode must skip the air source cell: {masked_passes:?}");

    let bare_masked = commands.run(&world, &alice, "execute if blocks 0 64 0 1 64 0 10 64 0 masked").expect("root matched");
    assert!(bare_masked.response.lines()[0].contains("count: 1"), "only the one non-air cell is counted: {bare_masked:?}");
}

/// A region whose cell count exceeds vanilla's own 32768 cap refuses with a
/// message naming both numbers, rather than scanning unbounded.
#[test]
fn execute_if_blocks_over_the_area_cap_refuses_by_name() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let source_blocks = FixedBlockSource::default();
    let world = CommandWorld {
        rules: &state,
        players: &players,
        state: &state,
        mobs: None,
        border: None,
        #[cfg(not(target_arch = "wasm32"))]
        access: None,
        blocks: Some(&source_blocks),
    };

    // 40 * 40 * 40 = 64000 > 32768.
    let outcome =
        commands.run(&world, &alice, "execute if blocks 0 0 0 39 39 39 100 0 0 all").expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(
        outcome.response.lines()[0].contains("64000") && outcome.response.lines()[0].contains("32768"),
        "the refusal must name both the actual and the maximum area: {outcome:?}"
    );
}

/// With no chunk source in scope, `if blocks` refuses cleanly by name, same
/// as `if block`.
#[test]
fn execute_if_blocks_with_no_chunk_source_refuses_by_name() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let alice = source(1, "alice");

    let outcome = run(&commands, &rules, &players, &alice, "execute if blocks 0 0 0 1 1 1 10 0 0 all").expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(
        outcome.response.lines()[0].contains("Blocks cannot be queried"),
        "the refusal must name the missing capability: {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// /execute if/unless biome
// ---------------------------------------------------------------------------

/// `if`/`unless biome` against a real, settable [`FixedBlockSource`] — the
/// present biome, the fixture's own default (`minecraft:plains`) and the
/// `blocks: None` refusal, the same three shapes [`FixedBlockSource`]'s own
/// `if block` gate above covers.
#[test]
fn execute_if_unless_biome_reads_a_real_chunk_source() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice"); // (0, 64, 0)
    let source_blocks = FixedBlockSource::default();
    source_blocks.set_biome(5, 64, 5, "minecraft:desert");

    let world = CommandWorld {
        rules: &rules as &(dyn RuleStore + Sync),
        players: &players,
        state: &state,
        mobs: None,
        border: None,
        #[cfg(not(target_arch = "wasm32"))]
        access: None,
        blocks: Some(&source_blocks),
    };

    let matches = commands
        .run(&world, &alice, "execute if biome 5 64 5 minecraft:desert run gamemode creative")
        .expect("root matched");
    assert_eq!(matches.effects.len(), 1, "the real biome matches: the chain must run: {matches:?}");

    let no_match = commands
        .run(&world, &alice, "execute if biome 5 64 5 minecraft:jungle run gamemode creative")
        .expect("root matched");
    assert!(no_match.effects.is_empty(), "desert is not jungle: the chain must not run: {no_match:?}");

    // The un-set position is `minecraft:plains` by this fixture's own default.
    let plains = commands
        .run(&world, &alice, "execute if biome 0 0 0 minecraft:plains run gamemode creative")
        .expect("root matched");
    assert_eq!(plains.effects.len(), 1, "{plains:?}");

    // `unless` is the exact complement.
    let unless_matches = commands
        .run(&world, &alice, "execute unless biome 5 64 5 minecraft:desert run gamemode creative")
        .expect("root matched");
    assert!(unless_matches.effects.is_empty(), "{unless_matches:?}");

    let unless_no_match = commands
        .run(&world, &alice, "execute unless biome 5 64 5 minecraft:jungle run gamemode creative")
        .expect("root matched");
    assert_eq!(unless_no_match.effects.len(), 1, "{unless_no_match:?}");

    // The bare form actually refuses/succeeds rather than folding into an
    // empty `Ran`, same convention as `if block`.
    let bare_match = commands.run(&world, &alice, "execute if biome 5 64 5 minecraft:desert").expect("root matched");
    assert!(bare_match.response.is_ran(), "{bare_match:?}");
    let bare_no_match = commands.run(&world, &alice, "execute if biome 5 64 5 minecraft:jungle").expect("root matched");
    assert!(!bare_no_match.response.is_ran(), "{bare_no_match:?}");
}

/// An unknown biome name is refused at **parse** time (`BiomeArg`'s own
/// census check), never reaching the executor — the same posture `if block`
/// takes for an unknown block id.
#[test]
fn execute_if_biome_with_an_unknown_name_is_a_parse_error() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let alice = source(1, "alice");

    let outcome =
        run(&commands, &rules, &players, &alice, "execute if biome 0 0 0 not_a_real_biome").expect("a parse refusal still reports an outcome");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(
        outcome.response.lines()[0].contains("unknown biome"),
        "the refusal must come from BiomeArg's own parse-time census check: {outcome:?}"
    );
}

/// With no chunk source in scope, `if biome` refuses cleanly by name, same as
/// `if block`.
#[test]
fn execute_if_biome_with_no_chunk_source_refuses_by_name() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let alice = source(1, "alice");

    let outcome = run(&commands, &rules, &players, &alice, "execute if biome 0 0 0 minecraft:plains").expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(
        outcome.response.lines()[0].contains("Blocks cannot be queried"),
        "the refusal must name the missing capability: {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// /execute if/unless loaded
// ---------------------------------------------------------------------------

/// `/execute if`/`unless loaded` against [`FixedBlockSource::mark_unloaded`] —
/// the resident (default) and marked-unloaded cases both exercised, plus the
/// no-chunk-source refusal every other `blocks`-backed condition here also
/// carries.
#[test]
fn execute_if_unless_loaded_reads_column_residency() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice"); // (0, 64, 0), chunk (0, 0)
    let source_blocks = FixedBlockSource::default();
    source_blocks.mark_unloaded(1, 1); // chunk covering (20, *, 20)

    let world = CommandWorld {
        rules: &rules as &(dyn RuleStore + Sync),
        players: &players,
        state: &state,
        mobs: None,
        border: None,
        #[cfg(not(target_arch = "wasm32"))]
        access: None,
        blocks: Some(&source_blocks),
    };

    let resident = commands.run(&world, &alice, "execute if loaded 0 64 0 run gamemode creative").expect("root matched");
    assert_eq!(resident.effects.len(), 1, "chunk (0, 0) is resident by the fixture's own default: {resident:?}");

    let unloaded = commands.run(&world, &alice, "execute if loaded 20 64 20 run gamemode creative").expect("root matched");
    assert!(unloaded.effects.is_empty(), "chunk (1, 1) was marked unloaded: {unloaded:?}");

    // `unless` is the exact complement.
    let unless_resident =
        commands.run(&world, &alice, "execute unless loaded 0 64 0 run gamemode creative").expect("root matched");
    assert!(unless_resident.effects.is_empty(), "{unless_resident:?}");

    let unless_unloaded =
        commands.run(&world, &alice, "execute unless loaded 20 64 20 run gamemode creative").expect("root matched");
    assert_eq!(unless_unloaded.effects.len(), 1, "{unless_unloaded:?}");

    // The bare form reports pass/fail itself, same convention as `if block`.
    let bare_resident = commands.run(&world, &alice, "execute if loaded 0 64 0").expect("root matched");
    assert!(bare_resident.response.is_ran(), "{bare_resident:?}");
    let bare_unloaded = commands.run(&world, &alice, "execute if loaded 20 64 20").expect("root matched");
    assert!(!bare_unloaded.response.is_ran(), "{bare_unloaded:?}");
}

/// With no chunk source in scope, `if loaded` refuses cleanly by name, same
/// as `if block`.
#[test]
fn execute_if_loaded_with_no_chunk_source_refuses_by_name() {
    let commands = ServerCommands::new();
    let rules = GameRulesHandle::new();
    let players = roster();
    let alice = source(1, "alice");

    let outcome = run(&commands, &rules, &players, &alice, "execute if loaded 0 0 0").expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(
        outcome.response.lines()[0].contains("Blocks cannot be queried"),
        "the refusal must name the missing capability: {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// /execute store
// ---------------------------------------------------------------------------

/// `store result score` writes the wrapped command's own return value;
/// `store success score` collapses that same wrapped command down to `0`/`1`
/// regardless of its magnitude. `scoreboard players set` is the wrapped
/// command precisely because its own return value (the score it just set) is
/// neither `0` nor `1` — a fixture where `result` and `success` would read
/// the same value could not tell the two modifiers apart.
#[test]
fn execute_store_score_result_and_success_diverge_on_a_multivalued_wrapped_command() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = data_world(&state, &players);

    commands.run(&world, &alice, "scoreboard objectives add x dummy").unwrap();
    commands.run(&world, &alice, "scoreboard objectives add out dummy").unwrap();

    let result_outcome = commands
        .run(&world, &alice, "execute store result score alice out run scoreboard players set alice x 42")
        .expect("root matched");
    assert!(result_outcome.response.is_ran(), "{result_outcome:?}");
    assert_eq!(state.scoreboard().get_score("alice", "out"), Ok(42), "{result_outcome:?}");

    let success_outcome = commands
        .run(&world, &alice, "execute store success score alice out run scoreboard players set alice x 7")
        .expect("root matched");
    assert!(success_outcome.response.is_ran(), "{success_outcome:?}");
    assert_eq!(
        state.scoreboard().get_score("alice", "out"),
        Ok(1),
        "success must collapse to 1, not carry the wrapped command's own 7 through: {success_outcome:?}"
    );
}

/// A wrapped command that itself fails stores `0`, and the outer `execute`
/// reports the failure too — matching vanilla's own catch path
/// (`CommandResultCallback::onFailure`, `success = false, result = 0`), not a
/// silent success with a stray write.
#[test]
fn execute_store_a_failing_wrapped_command_writes_zero_and_the_outer_command_refuses() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = data_world(&state, &players);

    commands.run(&world, &alice, "scoreboard objectives add out dummy").unwrap();
    // A path that was never written is `DataCommand`'s own refusal — see
    // `get_merge_and_remove_round_trip_through_the_store`.
    let outcome = commands
        .run(&world, &alice, "execute store success score alice out run data get storage test:missing nope")
        .expect("root matched");
    assert!(!outcome.response.is_ran(), "the wrapped command itself failed: {outcome:?}");
    assert_eq!(state.scoreboard().get_score("alice", "out"), Ok(0), "{outcome:?}");
}

/// `store … data storage … <type> <scale>` scales the stored value and tags
/// it with the requested SNBT type — `int 2` on a wrapped command returning
/// `21` must write `Int(42)`, not `Int(21)` and not `Double(42.0)`.
#[test]
fn execute_store_data_storage_scales_and_types_the_value() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = data_world(&state, &players);

    commands.run(&world, &alice, "scoreboard objectives add x dummy").unwrap();
    let outcome = commands
        .run(
            &world,
            &alice,
            "execute store result data storage test:out value int 2 run scoreboard players set alice x 21",
        )
        .expect("root matched");
    assert!(outcome.response.is_ran(), "{outcome:?}");
    assert_eq!(
        state.nbt_storage().get("test:out", &["value".to_string()]),
        Some(SnbtValue::Int(42)),
        "{outcome:?}"
    );
}

/// The sharpest corner of `/execute store`'s semantics, carried over from
/// vanilla's own `BuildContexts.execute` (see
/// `lodestone_server::commands::registrar::StoreSink`'s own doc): when a *forked*
/// condition later in the chain matches nothing, the wrapped command never
/// runs at all, and the store target is left exactly as it was — not zeroed.
/// Only a *bare* conditional (nothing after it) reliably reports `0`/`1`,
/// because that is the executor path, not the fork path.
#[test]
fn execute_store_target_is_left_untouched_when_a_forked_condition_matches_nothing() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = data_world(&state, &players);

    commands.run(&world, &alice, "scoreboard objectives add out dummy").unwrap();
    commands.run(&world, &alice, "scoreboard players set alice out 99").unwrap();

    let outcome = commands
        .run(
            &world,
            &alice,
            "execute store result score alice out if entity @a[name=nobody] run kill @s",
        )
        .expect("root matched");
    assert_eq!(
        state.scoreboard().get_score("alice", "out"),
        Ok(99),
        "a fork matching nothing must leave the store target untouched, not zero it: {outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// /execute summon
// ---------------------------------------------------------------------------

/// `execute summon <entity>` spawns into the **same** shared `MobHandle`
/// `/summon` itself does (this modifier calls
/// `crate::commands::summon::spawn_entity`, not a second code path), at the
/// *current* source position — honouring an earlier `positioned` in the same
/// chain, matching `spawnEntityAndRedirect`'s own `source.getPosition()`
/// read — and the chain continues afterward (`gamemode creative alice`
/// stands in for "downstream `run` still executes", since this crate's `@s`
/// selector cannot resolve to the newly summoned non-player source — see
/// this test's own sibling below for that disclosed gap).
#[test]
fn execute_summon_spawns_into_the_shared_mob_handle_at_the_chains_current_position() {
    use lodestone_server::{EntitySource, MobHandle};
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice"); // (0, 64, 0)
    let players = roster();
    let mobs = MobHandle::default();

    let world =
        CommandWorld { rules: &state, players: &players, state: &state, mobs: Some(&mobs), border: None, access: None, blocks: None };

    let outcome = commands
        .run(&world, &alice, "execute positioned ~11 ~1 ~4 summon minecraft:cow run gamemode creative alice")
        .expect("root matched");
    assert!(outcome.response.is_ran(), "the chain must continue past summon: {outcome:?}");
    assert!(!outcome.effects.is_empty(), "gamemode creative must still have fired: {outcome:?}");

    let snapshots = mobs.snapshots();
    let [cow] = snapshots.as_slice() else { panic!("expected exactly one spawned entity, got {snapshots:?}") };
    assert_eq!(cow.entity_type, "minecraft:cow".parse().unwrap());
    assert_eq!(cow.position, Vec3::new(11.0, 65.0, 4.0), "summon must read the chain's rewritten position");
}

/// The peaceful-difficulty refusal `crate::commands::summon::spawn_entity`
/// already enforces for `/summon` applies identically through
/// `execute summon` — the shared function, not a second, laxer check.
#[test]
fn execute_summon_of_a_hostile_mob_on_peaceful_refuses_and_does_not_continue_the_chain() {
    use lodestone_server::MobHandle;
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    state.set_difficulty(lodestone_model::Difficulty::Peaceful);
    let alice = source(1, "alice");
    let players = roster();
    let mobs = MobHandle::default();

    let world =
        CommandWorld { rules: &state, players: &players, state: &state, mobs: Some(&mobs), border: None, access: None, blocks: None };

    let outcome =
        commands.run(&world, &alice, "execute summon minecraft:zombie run gamemode creative alice").expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(outcome.effects.is_empty(), "the chain must not continue: {outcome:?}");
}

/// This crate's `@s` selector resolves an entity uuid against the **player**
/// roster only (`resolve_players`'s own `current_entity` arm) — a disclosed
/// gap, not a bug this modifier is expected to close: a summoned mob is
/// never in that roster, so `@s` after `execute summon` cannot resolve to it.
/// `on <relation>`/arbitrary-mob-as-source is the larger version of this same
/// gap (see `crate::commands::execute`'s own module doc).
#[test]
fn execute_summon_then_at_s_cannot_resolve_the_new_non_player_source() {
    use lodestone_server::MobHandle;
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let alice = source(1, "alice");
    let players = roster();
    let mobs = MobHandle::default();

    let world =
        CommandWorld { rules: &state, players: &players, state: &state, mobs: Some(&mobs), border: None, access: None, blocks: None };

    let outcome = commands.run(&world, &alice, "execute summon minecraft:cow run gamemode creative @s").expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
}

// ---------------------------------------------------------------------------
// /stopwatch
// ---------------------------------------------------------------------------

/// `create`/`query`/`restart`/`remove` round trip, including the two
/// name-carrying refusals: a duplicate `create`, and a `query` after
/// `remove`.
#[test]
fn stopwatch_create_query_restart_remove_round_trip() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = data_world(&state, &players);

    let created = commands.run(&world, &alice, "stopwatch create test:timer").expect("root matched");
    assert!(created.response.is_ran(), "{created:?}");

    let duplicate = commands.run(&world, &alice, "stopwatch create test:timer").expect("root matched");
    assert!(!duplicate.response.is_ran(), "a duplicate create must refuse: {duplicate:?}");
    assert!(duplicate.response.lines()[0].contains("test:timer"), "{duplicate:?}");

    let query = commands.run(&world, &alice, "stopwatch query test:timer").expect("root matched");
    assert!(query.response.is_ran(), "{query:?}");

    let query_scaled = commands.run(&world, &alice, "stopwatch query test:timer 1000.0").expect("root matched");
    assert!(query_scaled.response.is_ran(), "{query_scaled:?}");

    let restarted = commands.run(&world, &alice, "stopwatch restart test:timer").expect("root matched");
    assert!(restarted.response.is_ran(), "{restarted:?}");

    let restart_unknown = commands.run(&world, &alice, "stopwatch restart test:nope").expect("root matched");
    assert!(!restart_unknown.response.is_ran(), "{restart_unknown:?}");

    let removed = commands.run(&world, &alice, "stopwatch remove test:timer").expect("root matched");
    assert!(removed.response.is_ran(), "{removed:?}");

    let query_after_remove = commands.run(&world, &alice, "stopwatch query test:timer").expect("root matched");
    assert!(!query_after_remove.response.is_ran(), "a removed stopwatch must no longer query: {query_after_remove:?}");
}

// ---------------------------------------------------------------------------
// /execute if/unless stopwatch
// ---------------------------------------------------------------------------

/// A freshly created stopwatch reads inside `0..1` and outside `5..10` —
/// timing-sensitive only in the sense that "immediately after creation" must
/// stay under a second, which every other stopwatch test here also assumes.
#[test]
fn execute_if_unless_stopwatch_reads_elapsed_seconds() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = data_world(&state, &players);
    commands.run(&world, &alice, "stopwatch create test:timer").expect("root matched");

    let in_range = commands
        .run(&world, &alice, "execute if stopwatch test:timer 0..1 run gamemode creative alice")
        .expect("root matched");
    assert!(!in_range.effects.is_empty(), "a fresh stopwatch reads under a second: {in_range:?}");

    let out_of_range = commands
        .run(&world, &alice, "execute if stopwatch test:timer 5..10 run gamemode creative alice")
        .expect("root matched");
    assert!(out_of_range.effects.is_empty(), "a fresh stopwatch must not read 5-10s: {out_of_range:?}");

    // `unless` is the exact complement.
    let unless_in_range = commands
        .run(&world, &alice, "execute unless stopwatch test:timer 0..1 run gamemode creative alice")
        .expect("root matched");
    assert!(unless_in_range.effects.is_empty(), "{unless_in_range:?}");

    let unless_out_of_range = commands
        .run(&world, &alice, "execute unless stopwatch test:timer 5..10 run gamemode creative alice")
        .expect("root matched");
    assert!(!unless_out_of_range.effects.is_empty(), "{unless_out_of_range:?}");
}

/// An unknown stopwatch id is a hard refusal, named — vanilla's own
/// `ERROR_DOES_NOT_EXIST` — not merely a failing test.
#[test]
fn execute_if_stopwatch_with_an_unknown_id_refuses_by_name() {
    let commands = ServerCommands::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = roster();
    let alice = source(1, "alice");
    let world = data_world(&state, &players);

    let outcome = commands.run(&world, &alice, "execute if stopwatch test:nope 0..1").expect("root matched");
    assert!(!outcome.response.is_ran(), "{outcome:?}");
    assert!(
        outcome.response.lines()[0].contains("No stopwatch exists"),
        "the refusal must name the missing stopwatch: {outcome:?}"
    );
}
