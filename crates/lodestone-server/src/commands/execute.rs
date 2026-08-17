//! `/execute`, from `ExecuteCommand.java` (issue #48's remainder) — the
//! subcommand chain that rewrites a [`CommandSource`] and re-dispatches, built
//! entirely on the modifier/fork substrate `crate::commands::registrar`
//! already carried with no production caller.
//!
//! # The parser needed nothing new
//!
//! `lodestone_command::CommandTree::parse`'s own doc names one real gap:
//! ambiguity between *simultaneous argument children* of one node (the
//! `/tp`-shaped problem `crate::commands::teleport`'s module doc walks
//! through). `/execute`'s entire grammar — every subcommand named below, and
//! every one this module leaves unbuilt — is **literal-disambiguated**: at
//! every branch point vanilla's own tree offers at most one argument child
//! alongside zero or more literal children (`positioned` has `<pos>` and the
//! literal `as`; `facing` has `<pos>` and the literal `entity`; the whole `if`/
//! `unless` family branches purely on literals like `block`/`entity`/`score`
//! before ever reaching an argument). A literal match is unambiguous and tried
//! before any argument child regardless of registration order
//! (`CommandTree::parse_nodes`), so the one simplification this crate's parser
//! makes never engages here. **No parser change was needed for any part of
//! `/execute`'s real grammar — only for the subsystems some of its branches
//! need and this server does not have** (see "What is not built" below).
//!
//! # Source-rewriting: one modifier per subcommand, one shared redirect target
//!
//! Every non-conditional subcommand below is exactly one [`Registrar::arg`] (or
//! bare literal) carrying a [`Registrar::modifier`] plus [`Registrar::redirect`]
//! back to [`execute_node`]'s own children — Brigadier's `.redirect(execute,
//! modifier)`. A modifier receives the **one** [`CommandSource`] flowing through
//! this branch (`Dispatcher::dispatch` always calls a modifier with exactly one
//! input; see that function's own doc) and returns either one rewritten source
//! (`as`/`at`/`positioned`/… without `as`) or, for a *forking* modifier
//! (`as`/`at`/`positioned as`/`rotated as`/`facing entity`), one source per
//! resolved target — `Registrar::modifier`'s `forks: bool` is what makes a
//! failure inside one branch not abort the others (`execute as @a run give @s
//! …` must not stop at the first full inventory).
//!
//! `run <command>` is the one subcommand with **no modifier at all**:
//! `registrar.redirect(run_node, registrar.root())` — Brigadier's own
//! `literal("run").redirect(dispatcher.getRoot())`. With no modifier
//! registered for that node, `Dispatcher::dispatch`'s walk finds nothing and
//! passes the current (possibly already-forked) source set straight through
//! into a full re-parse of the *entire* tree from the top, so `execute as
//! Steve run kill` really does kill Steve and not the caller. This crate's own
//! `lodestone-command` doc names this exact shape — "how every vanilla
//! `/execute ... run <command>` re-enters the root" — as the motivating case
//! for the redirect-cycle guard in `CommandTree::parse`, and it is exercised
//! directly by `tests/brigadier_spec.rs` there, not invented here.
//!
//! # `if`/`unless`: the one place a node needs *both* a modifier and an executor
//!
//! `ExecuteCommand.addConditional` attaches **both** `.fork(execute, modifier)`
//! *and* `.executes(numericConditionalHandler)` to the same condition node,
//! because both `execute if entity @a run …` (continue the chain, filtered)
//! and a bare `execute if entity @a` (report pass/fail on its own) are real,
//! separately-used forms. Real Brigadier's `ContextChain` only ever runs one of
//! the two for a given parse — the fork fires exclusively when there is a next
//! stage. [`registrar::Dispatcher::dispatch`] needed one small, documented
//! change to reproduce that (skip a node's own modifier when that node is also
//! the parsed path's terminal node and carries its own executor) — see that
//! function's doc for the failure mode it closes. [`add_boolean_conditional`]/
//! [`add_numeric_conditional`] are the two shapes `addConditional`/
//! `createNumericConditionalHandler` take in vanilla.
//!
//! # What is built
//!
//! `as`, `at` (position **and** rotation), `positioned` (+ `as`), `rotated`
//! (+ `as`), `facing` (`<pos>` and `entity <targets> <anchor>`), `align`,
//! `anchored`, `in` (single-dimension census — see
//! [`lodestone_command_mc::DimensionArg`]'s own doc), `run`, `summon
//! <entity>`, `store result`/`success score`/`data storage`, and
//! `if`/`unless entity`/`dimension`/`score`/`data storage`/`block`/`biome`/
//! `blocks`/`stopwatch`/`loaded`.
//!
//! # `store`: the dispatcher change, and what it turned out not to need
//!
//! `store` was flagged as needing a dispatcher architecture change — a
//! result threaded through the chain, not just a [`CommandSource`] — and
//! that much held: [`registrar::StoreSink`] is exactly that thread.
//! [`Ctx::add_store_sink`] lets `store`'s own modifier attach one (resolving
//! its target eagerly, at redirect time, matching vanilla's own
//! `storeValue`/`storeData`), [`registrar::Dispatcher::dispatch`] carries the
//! accumulated set alongside each (possibly forked) source exactly the way it
//! already carries `feedback`/`effects`, and calls every sink once the
//! terminal executor's own outcome is known. See [`registrar::StoreSink`]'s
//! own doc for the one vanilla subtlety this reproduces faithfully rather
//! than approximating: a `store` whose *later* fork (an `if`/`as`/`at`/…)
//! resolves to zero sources leaves the target **unwritten**, not zeroed —
//! confirmed against `net.minecraft.commands.execution.tasks.BuildContexts
//! .execute`, which only ever chains a `store`-wrapped source's callback onto
//! a command that actually runs.
//!
//! Only `score` and `data storage` sinks are built — `bossbar` has no
//! subsystem in this crate at all (no `/bossbar` command is registered), and
//! `entity`/`block` need the same live, mutable NBT view `if data`'s
//! `entity`/`block` targets still lack (see "What is not built" below).
//!
//! `at`'s rotation transfer and `rotated as` both needed
//! `crate::commands::source::PlayerCandidate` to carry a rotation, which it
//! now does (`crate::players::PlayerRegistry::candidates` already tracked one
//! per connection — `TrackedPlayer::rotation`, kept live by
//! `PlayerRegistry::set_rotation` — and simply never threaded it through).
//!
//! `if`/`unless score` (`register_score_conditions`) needed
//! `crate::commands::scoreboard_store` to exist first — a real scoreboard,
//! reached through `ctx.world.state.scoreboard()` exactly like `/scoreboard`
//! itself reaches it, so a score set by one and read by the other agree by
//! construction. Both of vanilla's two shapes are built: `matches <range>`
//! (`lodestone_command_mc::IntRangeArg`) and `<op> <source>
//! <sourceObjective>` as five literal comparison tokens (`<`, `<=`, `=`,
//! `>=`, `>` — vanilla's tree registers these as literals here, **not**
//! `minecraft:operation`'s nine-token argument, which is a different, larger
//! set reserved for `/scoreboard players operation`). Both resolve `<target>`/
//! `<source>` through `crate::commands::scoreboard::resolve_single`, the same
//! function `/scoreboard players get` uses, so a selector, `*`, or a bare
//! "fake player" name all mean the same thing in both places.
//!
//! `if`/`unless data storage` (`register_data_storage_condition`) needed
//! `crate::commands::nbt_storage` to exist first — a real NBT command-storage
//! engine, reached through `ctx.world.state.nbt_storage()` exactly like
//! `/data storage` itself reaches it. Only the `storage` target is built —
//! see `crate::commands::nbt_data`'s module doc for why `entity`/`block` are
//! a separate, still-missing subsystem, not an oversight here.
//!
//! `if`/`unless block` (`register_block_condition`) needed a read-only chunk
//! query on [`CommandWorld`], which it now has —
//! [`super::registrar::CommandWorld::blocks`], `Option<&dyn
//! crate::chunk::ChunkSource>`, the same `Option<&concrete-type>` shape
//! [`CommandWorld::mobs`](super::registrar::CommandWorld::mobs)/
//! [`border`](super::registrar::CommandWorld::border) already take (never a
//! `&mut World`). A live connection's `ChatCommand` arm and a command
//! block's own tick both get `Some` (the same `chunk_source` `Effect::SetBlock`/
//! `Fill` already reach through); RCON gets `Some` whenever it has a
//! `world_source` configured; this crate's own test helpers get `None` and
//! the condition refuses by name rather than panicking. Compares only the
//! base block id — the same v1 reduction
//! [`lodestone_command_mc::BlockArg`] already takes (no property list), so
//! `if block ~ ~ ~ furnace` matches regardless of `facing`/`lit`.
//!
//! `if`/`unless biome` (`register_biome_condition`) needed a biome read on
//! the same [`CommandWorld::blocks`](super::registrar::CommandWorld::blocks)
//! chunk source — [`crate::chunk::ChunkColumn::biome_state_at`] already
//! existed, but nothing exposed it on the [`crate::chunk::ChunkSource`]
//! *trait* the command layer reaches through. Added as a **required** method
//! (`biome_state_at`, no default — see that method's own doc for why a
//! defaulted version would be exactly the island-generating wrapper shape
//! this crate has already been burned by), forwarded through all ~63
//! implementors across `lodestone-server`, `lodestone-v770`'s and
//! `lodestone-event-logger`'s own test doubles. [`lodestone_command_mc::BiomeArg`]
//! is a bare id, no tag support — the same v1 reduction `BlockArg` already
//! makes — validated against a new hand-listed 66-entry census in
//! `lodestone-data` (`lodestone_data::biomes`; see that module's own doc for
//! why it needs no generated network-id table alongside it, unlike every
//! other census in that crate).
//!
//! `if`/`unless blocks` (`register_blocks_condition`) needed no `ChunkSource`
//! change at all — `block_state` plus the already-defaulted `block_entity`
//! were enough, so this is pure command-layer logic: [`compare_regions`]
//! walks the `[start, end]` box against its `destination`-relative twin,
//! comparing the raw block-state string (closer to vanilla's full
//! `BlockState` comparison than `if block`'s own reduction) and each side's
//! generated block entity. Vanilla's own fork test is **presence**-based,
//! not `count > 0` (a `masked` region whose every source cell is air matches
//! vacuously with `count == 0`), so this is its own conditional shape rather
//! than reusing [`add_boolean_conditional`]/[`add_numeric_conditional`].
//!
//! `stopwatch` (both `/stopwatch` itself and `if`/`unless stopwatch <id>
//! <range>`) needed a new registry, [`crate::commands::stopwatch_store::StopwatchHandle`],
//! a sibling store on [`crate::world_state::WorldStateHandle`] alongside
//! `scoreboard`/`team`/`nbt_storage`. Uses
//! [`crate::chat_session::now_millis`] (`web_time`-backed), never
//! `std::time::SystemTime::now` — this crate links into the wasm32 bundle
//! and that call traps at runtime there. **No persistence**: vanilla's
//! `Stopwatches` is a `SavedData` written to `data/stopwatches.dat`, and this
//! store is process-lifetime only, a disclosed gap in the same shape
//! [`crate::chunk::ChunkSource::claim_dragon_fight_start`]'s own doc already
//! accepts for the End-fight flag — a restart clears every stopwatch.
//!
//! `execute summon <entity>` (the modifier form that also changes the
//! acting entity — distinct from `execute at @s run summon minecraft:cow`,
//! which already reached `/summon` through `run` with no new code, but
//! leaves the *caller* as the acting source for anything chained after it).
//! [`crate::commands::summon::spawn_entity`] was split out of `/summon`'s own
//! executor so both call sites share one difficulty/peaceful/no-`mobs` check
//! rather than duplicating it. The new source's position is read from the
//! *current* chain source (`spawnEntityAndRedirect`'s own
//! `source.getPosition()`), honouring an earlier `positioned`/`at`. **A
//! summoned mob still cannot become the target of a later `@s`** — see
//! `on <relation>` below, which is the same gap in its general form.
//!
//! # What is not built, and why — each names its own missing subsystem
//!
//! * **`store bossbar`/`entity`/`block`, `if items`, `if`/`unless data
//!   entity`/`block`.** Each needs a live, command-reachable, **mutable** NBT
//!   or inventory view of an entity, block entity, or container slot that
//!   this crate has nowhere — `storage` needed none of that, which is why it
//!   alone is built on either side of `store`/`if data`. Concretely: a
//!   command executor only ever *pushes* a [`super::effect::Effect`] at a
//!   player's own connection (`/clear`'s own [`Effect::ClearInventory`] is
//!   the shape every inventory-touching command already takes), which is
//!   write-only and asynchronous — there is no synchronous read-back path
//!   for a live entity's inventory contents at all, so even *counting* items
//!   (`if items`) is unreachable, not just mutating them. `store bossbar`
//!   additionally needs a `/bossbar` command and store this crate has
//!   neither.
//! * **`predicate`.** The loot-*condition* engine this needs already exists
//!   and is real: `crate::loot`'s `LootCondition` enum parses and evaluates
//!   `all_of`/`any_of`/`inverted`/`match_tool`/`block_state_property`/… — the
//!   same primitives vanilla's own standalone predicate JSON is built from.
//!   The actual blocker is one layer up: vanilla's `<predicate>` argument
//!   (`ResourceOrIdArgument.lootPredicate`) resolves a resource location
//!   against the `minecraft:predicate` **registry**, which is populated
//!   **only by datapacks** (`data/<namespace>/predicate/*.json`) — and the
//!   base game ships **zero** files under that directory (verified against
//!   `.cache/mc/26.2/{src,client-src}/data/minecraft/predicate/`, unlike
//!   `data/minecraft/loot_table/`'s 1,241 vanilla-authored files, which is
//!   why `crate::loot` could bundle those the same way `assets/loot_table/`
//!   does). So `if predicate` is not "a subsystem not yet built" so much as
//!   "coupled to datapack loading" — the exact thing issue #48's remainder
//!   scopes to functions/datapacks, out of this unit. Building a
//!   lodestone-specific predicate registry with no vanilla datapack behind
//!   it would not be `if predicate` — it would be a different, invented
//!   feature wearing that name.
//! * **`function`.** Explicitly out of scope for this unit — issue #48's
//!   remainder tracks functions/datapacks separately.
//! * **`on <relation>`**
//!   (owner/leasher/target/attacker/vehicle/controller/origin/passengers).
//!   Narrower than "the mob simulation does not expose relationship queries"
//!   — several of those *are* already `pub` on `SimMob` (`owner_uuid`,
//!   `leash_holder`, `attack_target_id`, the mount/rider pair on `MobSim`).
//!   The real, deeper blocker sits one layer below: **a non-player entity
//!   can never become the acting source of an `/execute` chain at all**, on
//!   this server, today. `as <targets>`/`positioned as`/`rotated as`/`facing
//!   entity` all resolve `<targets>` through [`Ctx::resolve`] →
//!   `super::source::resolve_players`, which only ever matches against
//!   `ctx.world.players: &[PlayerCandidate]` — a mob's uuid is never in that
//!   list, selector or not. `execute summon <entity>` is the one way this
//!   file makes a non-player the acting source, and its own doc above notes
//!   the same consequence: `@s` afterward cannot resolve it either. So
//!   `on <relation>` needs selector resolution widened to cover arbitrary
//!   entities first — a `CommandSource`/`PlayerCandidate` architecture
//!   change, not an addition to this file — before the relationship
//!   accessors above are reachable from anywhere.
//! * **`positioned over <heightmap>`.** [`lodestone_command_mc::HeightmapArg`]
//!   is built and tested (the four `Heightmap.Types::keepAfterWorldgen`
//!   survivors), but unwired: computing a heightmap needs the **world's
//!   vertical bounds** to scan, and those are not reachable through
//!   [`CommandWorld::blocks`](super::registrar::CommandWorld::blocks)'s
//!   `Option<&dyn ChunkSource>` — `min_y`/`height` are real methods on
//!   `OverworldChunkSource` itself, deliberately read from the live
//!   generator rather than hardcoded (see that type's own doc: "hardcoding
//!   `(-64, 384)` … is a guess that drifts the moment the overworld's shape
//!   changes"), and a `dyn ChunkSource` trait object cannot name them.
//!   Closing this needs the same class of change `biome_state_at` just
//!   took — a required trait method (or an equivalent field on
//!   `CommandWorld` itself) — rippled through every implementor a second
//!   time; left as its own unit rather than a drive-by here. The per-cell
//!   predicate math (`lodestone_data::block_solidity::blocks_motion`, a
//!   fluid check, a leaf-block check) is otherwise ordinary and already
//!   scoped in `HeightmapKind`'s own doc.
//!
//! # Command blocks are the other half of this issue and are not this file
//!
//! See `crate::block_entities`'s `BlockEntity::CommandBlock` variant and
//! `crate::command_block` for the data model and pure tick semantics ported
//! from `CommandBlockEntity`/`BaseCommandBlock`/`CommandBlock.java`, and their
//! own module docs for exactly how far that got and what is still needed to
//! reach a running command block end-to-end.

use std::sync::Arc;

use lodestone_command::{DoubleArgument, NodeId};
use lodestone_command_mc::{
    AnchorInput, Axes, BiomeArg, BlockArg, BlockPosArg, DimensionArg, EntityAnchorArg, EntityArg,
    EntitySelector, EntityTypeArg, FloatRangeArg, IdentifierArg, IntRangeArg, NbtPathArg,
    ObjectiveArg, RotationArg, ScoreHolderArg, SnbtValue, StorageIdArg, SwizzleArg, Vec3Arg,
};
use lodestone_model::Rotation;

use super::registrar::{ArgKey, Ctx, Registrar, StoreSink};
use super::source::{CommandSource, EntityAnchor, PlayerCandidate, SelectorError, SourceEntity};

/// `Commands.LEVEL_GAMEMASTERS` — the one permission gate, on the root
/// `execute` literal; every subcommand beneath it is ungated in vanilla too.
const EXECUTE_LEVEL: u8 = 2;

/// `Player.STANDING_DIMENSIONS.eyeHeight()` — vanilla's own constant, and a
/// documented approximation in exactly the shape
/// `crate::commands::source::PlayerCandidate`'s own doc already accepts for
/// position: this crate tracks no per-player pose (crouching, swimming, …),
/// so every anchor computation here uses the standing value regardless of the
/// target's actual pose.
const EYE_HEIGHT: f64 = 1.62;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let execute = registrar.literal(root, "execute");
    registrar.require_level(execute, EXECUTE_LEVEL);

    register_run(registrar, execute, root);
    register_store(registrar, execute);
    register_conditions(registrar, execute, "if", true);
    register_conditions(registrar, execute, "unless", false);
    register_as(registrar, execute);
    register_at(registrar, execute);
    register_positioned(registrar, execute);
    register_rotated(registrar, execute);
    register_facing(registrar, execute);
    register_align(registrar, execute);
    register_anchored(registrar, execute);
    register_in(registrar, execute);
    register_summon(registrar, execute);
}

/// `literal("run").redirect(dispatcher.getRoot())` — no modifier, so the
/// current source set passes through unchanged into a full re-parse of the
/// whole tree. See this module's doc for why that alone is what makes
/// `execute as Steve run kill` affect Steve.
fn register_run(registrar: &mut Registrar, execute: NodeId, root: NodeId) {
    let run = registrar.literal(execute, "run");
    registrar.redirect(run, root);
}

// ---- store -------------------------------------------------------------

/// `wrapStores` — `store result`/`store success`, each redirecting into
/// `execute`'s own children exactly like every other subcommand here, but
/// carrying a [`StoreSink`] instead of rewriting the source. Only the `score`
/// and `data storage` targets are built: `bossbar` has no subsystem in this
/// crate (no `/bossbar` command is registered at all — see
/// `crate::commands::mod`'s registration list), and `entity`/`block` need the
/// same live, mutable NBT view this module's own doc already names as absent
/// for `if data`. See [`StoreSink`]'s own doc for exactly when a sink fires
/// and when a chain silently leaves its target unwritten.
fn register_store(registrar: &mut Registrar, execute: NodeId) {
    let store = registrar.literal(execute, "store");
    register_store_kind(registrar, store, "result", true, execute);
    register_store_kind(registrar, store, "success", false, execute);
}

/// One of `result`/`success` — `store_result` is vanilla's own
/// `storeResult` flag threaded through `storeValue`/`storeData`: `true` means
/// "write the command's own return value", `false` means "write `1`/`0` for
/// success/failure" (`storeResult ? result : (success ? 1 : 0)`, the same
/// expression duplicated at every one of vanilla's sink constructors).
fn register_store_kind(registrar: &mut Registrar, store: NodeId, literal: &str, store_result: bool, execute: NodeId) {
    let kind = registrar.literal(store, literal);

    // `store <result|success> score <targets> <objective>`.
    let score_lit = registrar.literal(kind, "score");
    let (targets_node, targets_key) = registrar.arg(score_lit, "targets", ScoreHolderArg::multiple());
    let (obj_node, obj_key) = registrar.arg(targets_node, "objective", ObjectiveArg);
    registrar.modifier(obj_node, false, move |ctx, sources, _parsed| {
        let base = one(sources);
        // Resolved *now*, at redirect time — matching vanilla's own
        // `ScoreHolderArgument.getNamesWithDefaultWildcard(c, "targets")`,
        // called once inside `wrapStores`'s redirect lambda, not deferred to
        // whenever the sink eventually fires.
        let holders = super::scoreboard::resolve_many(ctx, targets_key)?;
        let objective = ctx.get(obj_key).clone();
        let sink: StoreSink = Arc::new(move |world, success, result| {
            let value = if store_result { result } else { i32::from(success) };
            for holder in &holders {
                let _ = world.state.scoreboard().set_score(holder, &objective, value);
            }
        });
        ctx.add_store_sink(sink);
        Ok(vec![base])
    });
    registrar.redirect(obj_node, execute);

    // `store <result|success> data storage <target> <path> <type> <scale>`.
    let data_lit = registrar.literal(kind, "data");
    let storage_lit = registrar.literal(data_lit, "storage");
    let (id_node, id_key) = registrar.arg(storage_lit, "target", StorageIdArg);
    let (path_node, path_key) = registrar.arg(id_node, "path", NbtPathArg);
    register_store_scale(registrar, path_node, "byte", execute, store_result, id_key, path_key, |v| {
        #[allow(clippy::cast_possible_truncation)]
        SnbtValue::Byte(v as i8)
    });
    register_store_scale(registrar, path_node, "short", execute, store_result, id_key, path_key, |v| {
        #[allow(clippy::cast_possible_truncation)]
        SnbtValue::Short(v as i16)
    });
    register_store_scale(registrar, path_node, "int", execute, store_result, id_key, path_key, |v| {
        #[allow(clippy::cast_possible_truncation)]
        SnbtValue::Int(v as i32)
    });
    register_store_scale(registrar, path_node, "long", execute, store_result, id_key, path_key, |v| {
        #[allow(clippy::cast_possible_truncation)]
        SnbtValue::Long(v as i64)
    });
    register_store_scale(registrar, path_node, "float", execute, store_result, id_key, path_key, |v| {
        #[allow(clippy::cast_possible_truncation)]
        SnbtValue::Float(v as f32)
    });
    register_store_scale(registrar, path_node, "double", execute, store_result, id_key, path_key, SnbtValue::Double);
}

/// One of `byte`/`short`/`int`/`long`/`float`/`double` under
/// `store … data storage <target> <path>` — six identically-shaped literal
/// children (`IntTag.valueOf((int)(v * scale))` and five siblings in
/// vanilla), differing only in `construct`, which scales the `f64` result and
/// narrows it to the tag type. Rust's `as` cast saturates rather than
/// wrapping on overflow (unlike Java's narrowing primitive cast, which wraps
/// for the integral targets) — a documented divergence at the extremes, not
/// silently inherited.
#[allow(clippy::too_many_arguments)]
fn register_store_scale(
    registrar: &mut Registrar,
    path_node: NodeId,
    literal: &str,
    execute: NodeId,
    store_result: bool,
    id_key: ArgKey<String>,
    path_key: ArgKey<Vec<String>>,
    construct: fn(f64) -> SnbtValue,
) {
    let type_lit = registrar.literal(path_node, literal);
    let (scale_node, scale_key) = registrar.arg(type_lit, "scale", DoubleArgument::new());
    registrar.modifier(scale_node, false, move |ctx, sources, _parsed| {
        let base = one(sources);
        let id = ctx.get(id_key).clone();
        let path = ctx.get(path_key).clone();
        let scale = *ctx.get(scale_key);
        let sink: StoreSink = Arc::new(move |world, success, result| {
            let value = if store_result { result } else { i32::from(success) };
            let scaled = f64::from(value) * scale;
            world.state.nbt_storage().set(&id, &path, construct(scaled));
        });
        ctx.add_store_sink(sink);
        Ok(vec![base])
    });
    registrar.redirect(scale_node, execute);
}

// ---- as / at ---------------------------------------------------------------

/// `Commands.literal("as").then(argument("targets", entities()).fork(execute,
/// …withEntity(entity)…))` — rewrites *who*, leaving position/rotation/anchor
/// untouched. This is the discriminating half of `/execute`: an effect emitted
/// after `as <other>` targets `<other>`, never the caller.
fn register_as(registrar: &mut Registrar, execute: NodeId) {
    let as_lit = registrar.literal(execute, "as");
    let (targets_node, targets_key) = registrar.arg(as_lit, "targets", EntityArg::entities());
    registrar.modifier(targets_node, true, move |ctx, sources, _parsed| {
        let base = one(sources);
        let selector = ctx.get(targets_key).clone();
        let targets = resolve_optional(ctx, &selector)?;
        Ok(targets
            .into_iter()
            .map(|target| {
                let mut next = base.clone();
                next.name = target.username.clone();
                next.entity = Some(SourceEntity {
                    uuid: target.uuid,
                    entity_id: target.entity_id,
                    username: target.username,
                });
                next
            })
            .collect())
    });
    registrar.redirect(targets_node, execute);
}

/// `Commands.literal("at").then(argument("targets", entities()).fork(execute,
/// …withLevel(level).withPosition(pos).withRotation(rot)…))` — rewrites
/// *where*, leaving the acting entity untouched. Position, rotation **and**
/// anchor-relevant state all transfer: `CommandSourceStack.withPosition`/
/// `withRotation` both fire in vanilla's own `at`, and
/// `PlayerCandidate::rotation` (`crate::commands::source`) is what makes the
/// rotation half possible here. Dimension is `base.dimension` unchanged for
/// the same reason `/tp`'s own module doc gives elsewhere: every candidate on
/// this server's one roster is already in the one hosted dimension.
fn register_at(registrar: &mut Registrar, execute: NodeId) {
    let at_lit = registrar.literal(execute, "at");
    let (targets_node, targets_key) = registrar.arg(at_lit, "targets", EntityArg::entities());
    registrar.modifier(targets_node, true, move |ctx, sources, _parsed| {
        let base = one(sources);
        let selector = ctx.get(targets_key).clone();
        let targets = resolve_optional(ctx, &selector)?;
        Ok(targets
            .into_iter()
            .map(|target| {
                let mut next = base.clone();
                next.position = target.position;
                next.rotation = target.rotation;
                next
            })
            .collect())
    });
    registrar.redirect(targets_node, execute);
}

// ---- positioned -------------------------------------------------------------

fn register_positioned(registrar: &mut Registrar, execute: NodeId) {
    let positioned = registrar.literal(execute, "positioned");

    // `positioned <pos>` — `.redirect(execute, c ->
    // source.withPosition(pos).withAnchor(FEET))`. The anchor reset to `feet`
    // is vanilla's own and easy to drop by accident: an absolute `<pos>`
    // deliberately clears whatever `anchored eyes` set earlier in the chain.
    let (pos_node, pos_key) = registrar.arg(positioned, "pos", Vec3Arg::new());
    registrar.modifier(pos_node, false, move |ctx, sources, _parsed| {
        let base = one(sources);
        let coords = *ctx.get(pos_key);
        let (x, y, z) =
            coords.resolve((base.position.x, base.position.y, base.position.z), (base.rotation.yaw, base.rotation.pitch));
        let mut next = base;
        next.position = lodestone_model::Vec3::new(x, y, z);
        next.anchor = EntityAnchor::Feet;
        Ok(vec![next])
    });
    registrar.redirect(pos_node, execute);

    // `positioned as <targets>` — position only, anchor untouched (vanilla's
    // own fork does not call `withAnchor` here, unlike the bare-`<pos>` form
    // above).
    let as_lit = registrar.literal(positioned, "as");
    let (targets_node, targets_key) = registrar.arg(as_lit, "targets", EntityArg::entities());
    registrar.modifier(targets_node, true, move |ctx, sources, _parsed| {
        let base = one(sources);
        let selector = ctx.get(targets_key).clone();
        let targets = resolve_optional(ctx, &selector)?;
        Ok(targets
            .into_iter()
            .map(|target| {
                let mut next = base.clone();
                next.position = target.position;
                next
            })
            .collect())
    });
    registrar.redirect(targets_node, execute);

    // `positioned over <heightmap>` is not registered — see this module's doc.
}

// ---- rotated -----------------------------------------------------------------

fn register_rotated(registrar: &mut Registrar, execute: NodeId) {
    let rotated = registrar.literal(execute, "rotated");
    let (rot_node, rot_key) = registrar.arg(rotated, "rotation", RotationArg);
    registrar.modifier(rot_node, false, move |ctx, sources, _parsed| {
        let base = one(sources);
        let rot = *ctx.get(rot_key);
        let (yaw, pitch) = rot.resolve((base.rotation.yaw, base.rotation.pitch));
        let mut next = base;
        next.rotation = Rotation { yaw, pitch };
        Ok(vec![next])
    });
    registrar.redirect(rot_node, execute);

    // `rotated as <targets>` — `.fork(execute, c -> byAsRot(c))`, copying the
    // target's own rotation wholesale (`entity.getRotationVector()`), unlike
    // the `<rotation>` form above which resolves `~`-relative deltas against
    // the *base* source's rotation.
    let as_lit = registrar.literal(rotated, "as");
    let (targets_node, targets_key) = registrar.arg(as_lit, "targets", EntityArg::entities());
    registrar.modifier(targets_node, true, move |ctx, sources, _parsed| {
        let base = one(sources);
        let selector = ctx.get(targets_key).clone();
        let targets = resolve_optional(ctx, &selector)?;
        Ok(targets
            .into_iter()
            .map(|target| {
                let mut next = base.clone();
                next.rotation = target.rotation;
                next
            })
            .collect())
    });
    registrar.redirect(targets_node, execute);
}

// ---- facing --------------------------------------------------------------

fn register_facing(registrar: &mut Registrar, execute: NodeId) {
    let facing = registrar.literal(execute, "facing");

    // `facing <pos>` — `.redirect(execute, c -> source.facing(pos))`. `<pos>`
    // itself resolves against the plain source position (`Coordinates
    // .getPosition(source)`, never anchor-adjusted); only the *from* point of
    // the facing computation uses the source's own anchor.
    let (pos_node, pos_key) = registrar.arg(facing, "pos", Vec3Arg::new());
    registrar.modifier(pos_node, false, move |ctx, sources, _parsed| {
        let base = one(sources);
        let coords = *ctx.get(pos_key);
        let (tx, ty, tz) =
            coords.resolve((base.position.x, base.position.y, base.position.z), (base.rotation.yaw, base.rotation.pitch));
        let from = anchor_position(&base);
        let (yaw, pitch) = compute_facing((from.x, from.y, from.z), (tx, ty, tz));
        let mut next = base;
        next.rotation = Rotation { yaw, pitch };
        Ok(vec![next])
    });
    registrar.redirect(pos_node, execute);

    // `facing entity <targets> <anchor>` — the `<anchor>` argument picks the
    // *target's* point (`anchor.apply(entity)`); the *source's* own `anchor`
    // field (set by a previous `anchored`, default `feet`) still picks the
    // `from` point, unaffected by this argument.
    let entity_lit = registrar.literal(facing, "entity");
    let (targets_node, targets_key) = registrar.arg(entity_lit, "targets", EntityArg::entities());
    let (anchor_node, anchor_key) = registrar.arg(targets_node, "anchor", EntityAnchorArg);
    registrar.modifier(anchor_node, true, move |ctx, sources, _parsed| {
        let base = one(sources);
        let selector = ctx.get(targets_key).clone();
        let anchor = *ctx.get(anchor_key);
        let targets = resolve_optional(ctx, &selector)?;
        let from = anchor_position(&base);
        Ok(targets
            .into_iter()
            .map(|target| {
                let to = candidate_anchor_position(&target, anchor);
                let (yaw, pitch) = compute_facing((from.x, from.y, from.z), (to.x, to.y, to.z));
                let mut next = base.clone();
                next.rotation = Rotation { yaw, pitch };
                next
            })
            .collect())
    });
    registrar.redirect(anchor_node, execute);
}

// ---- align / anchored / in --------------------------------------------------

fn register_align(registrar: &mut Registrar, execute: NodeId) {
    let align = registrar.literal(execute, "align");
    let (axes_node, axes_key) = registrar.arg(align, "axes", SwizzleArg);
    registrar.modifier(axes_node, false, move |ctx, sources, _parsed| {
        let base = one(sources);
        let axes: Axes = *ctx.get(axes_key);
        let mut next = base;
        let mut pos = next.position;
        if axes.x {
            pos.x = pos.x.floor();
        }
        if axes.y {
            pos.y = pos.y.floor();
        }
        if axes.z {
            pos.z = pos.z.floor();
        }
        next.position = pos;
        Ok(vec![next])
    });
    registrar.redirect(axes_node, execute);
}

fn register_anchored(registrar: &mut Registrar, execute: NodeId) {
    let anchored = registrar.literal(execute, "anchored");
    let (anchor_node, anchor_key) = registrar.arg(anchored, "anchor", EntityAnchorArg);
    registrar.modifier(anchor_node, false, move |ctx, sources, _parsed| {
        let base = one(sources);
        let anchor = *ctx.get(anchor_key);
        let mut next = base;
        next.anchor = match anchor {
            AnchorInput::Feet => EntityAnchor::Feet,
            AnchorInput::Eyes => EntityAnchor::Eyes,
        };
        Ok(vec![next])
    });
    registrar.redirect(anchor_node, execute);
}

fn register_in(registrar: &mut Registrar, execute: NodeId) {
    let in_lit = registrar.literal(execute, "in");
    let (dim_node, dim_key) = registrar.arg(in_lit, "dimension", DimensionArg);
    registrar.modifier(dim_node, false, move |ctx, sources, _parsed| {
        let base = one(sources);
        let dimension = ctx.get(dim_key).clone();
        let mut next = base;
        next.dimension = dimension;
        Ok(vec![next])
    });
    registrar.redirect(dim_node, execute);
}

// ---- summon --------------------------------------------------------------

/// `Commands.literal("summon").then(argument("entity", …).redirect(execute,
/// spawnEntityAndRedirect()))` — a plain rewrite (`false`, not a fork:
/// vanilla registers this with `.redirect`, not `.fork`, so exactly one
/// source goes in and exactly one comes out), unlike `as`/`at`. Reuses
/// [`super::summon::spawn_entity`] verbatim rather than re-deriving the
/// difficulty/peaceful/no-`mobs` checks — `SummonCommand.createEntity` is the
/// one function both vanilla's `/summon` and this modifier call.
///
/// `/summon minecraft:cow` is unaffected: this is a *new* node under
/// `execute`, not a change to the root `summon` command
/// ([`super::summon::register`]) at all.
fn register_summon(registrar: &mut Registrar, execute: NodeId) {
    let summon_lit = registrar.literal(execute, "summon");
    let (entity_node, entity_key) = registrar.arg(summon_lit, "entity", EntityTypeArg);
    registrar.modifier(entity_node, false, move |ctx, sources, _parsed| {
        let base = one(sources);
        let entity = ctx.get(entity_key).clone();
        // `spawnEntityAndRedirect`'s own `source.getPosition()` — the
        // *current* source's position, honouring whatever `positioned`/`at`
        // already rewrote earlier in this same chain.
        let pos = base.position;
        let new_entity = super::summon::spawn_entity(ctx, &entity, pos)?;
        let mut next = base;
        next.name = new_entity.username.clone();
        next.entity = Some(new_entity);
        Ok(vec![next])
    });
    registrar.redirect(entity_node, execute);
}

// ---- if / unless -------------------------------------------------------------

/// `entity`/`dimension` under `if`/`unless` — see this module's doc for the
/// mechanism and for the much larger set of conditions this does not build.
fn register_conditions(registrar: &mut Registrar, execute: NodeId, literal: &str, expected: bool) {
    let parent = registrar.literal(execute, literal);

    let entity_lit = registrar.literal(parent, "entity");
    let (entities_node, entities_key) = registrar.arg(entity_lit, "entities", EntityArg::entities());
    add_numeric_conditional(registrar, entities_node, execute, expected, move |ctx| {
        let selector = ctx.get(entities_key).clone();
        let targets = resolve_optional(ctx, &selector)?;
        Ok(i32::try_from(targets.len()).unwrap_or(i32::MAX))
    });

    let dimension_lit = registrar.literal(parent, "dimension");
    let (dim_node, dim_key) = registrar.arg(dimension_lit, "dimension", DimensionArg);
    add_boolean_conditional(registrar, dim_node, execute, expected, move |ctx| {
        let dimension = ctx.get(dim_key).clone();
        Ok(dimension == ctx.source.dimension)
    });

    register_score_conditions(registrar, parent, execute, expected);
    register_data_storage_condition(registrar, parent, execute, expected);
    register_block_condition(registrar, parent, execute, expected);
    register_biome_condition(registrar, parent, execute, expected);
    register_blocks_condition(registrar, parent, execute, expected);
    register_stopwatch_condition(registrar, parent, execute, expected);
    register_loaded_condition(registrar, parent, execute, expected);
}

/// `stopwatch <id> <range>` — `checkStopwatch`'s own boolean shape
/// (`addConditional`, same as `block`/`dimension`/`biome`), reading
/// [`crate::commands::stopwatch_store::StopwatchHandle::elapsed_seconds`].
/// An unknown id is a hard **refusal** (vanilla's own
/// `StopwatchCommand.ERROR_DOES_NOT_EXIST`), not merely a failing test — the
/// same "propagate an `Err`, not a `false`" shape
/// [`register_block_condition`]'s missing-chunk-source case already takes.
fn register_stopwatch_condition(
    registrar: &mut Registrar,
    parent: NodeId,
    execute: NodeId,
    expected: bool,
) {
    let stopwatch_lit = registrar.literal(parent, "stopwatch");
    let (id_node, id_key) = registrar.arg(stopwatch_lit, "id", IdentifierArg);
    let (range_node, range_key) = registrar.arg(id_node, "range", FloatRangeArg);
    add_boolean_conditional(registrar, range_node, execute, expected, move |ctx| {
        let id = ctx.get(id_key).to_string();
        let Some(elapsed) = ctx.world.state.stopwatches().elapsed_seconds(&id) else {
            return Err(format!("No stopwatch exists by that name: {id}"));
        };
        let range = *ctx.get(range_key);
        Ok(range.matches(elapsed))
    });
}

/// `blocks <start> <end> <destination> all|masked` — `ExecuteCommand`'s own
/// `checkRegions`, restated. Neither [`add_boolean_conditional`] nor
/// [`add_numeric_conditional`] fit: vanilla's own fork test is
/// **presence**-based (`checkRegions(..).isPresent()`), not `count > 0` — a
/// `masked` region whose every source cell is air matches vacuously with
/// `count == 0`, and that must still fork/pass, unlike `if entity`'s zero
/// convention. So this is its own conditional shape, registered the same way
/// the other two are (a `forks: true` modifier plus the node's own executor).
///
/// Compares only the base block id — [`compare_regions`]'s own doc has the
/// exact string comparison — and, when either side has one, the source's own
/// *generated* block entity ([`crate::chunk::ChunkSource::block_entity`], not
/// the live, player-mutated registry — the same distinction
/// [`register_block_condition`]'s neighbours already accept). Vanilla's own
/// 32768-cell area cap is reproduced, so a malformed region refuses cleanly
/// rather than scanning unbounded on the tick thread.
fn register_blocks_condition(
    registrar: &mut Registrar,
    parent: NodeId,
    execute: NodeId,
    expected: bool,
) {
    let blocks_lit = registrar.literal(parent, "blocks");
    let (start_node, start_key) = registrar.arg(blocks_lit, "start", BlockPosArg);
    let (end_node, end_key) = registrar.arg(start_node, "end", BlockPosArg);
    let (dest_node, dest_key) = registrar.arg(end_node, "destination", BlockPosArg);

    for (literal, skip_air) in [("all", false), ("masked", true)] {
        let mode_lit = registrar.literal(dest_node, literal);
        registrar.modifier(mode_lit, true, move |ctx, sources, _parsed| {
            let base = one(sources);
            let count = compare_regions(ctx, start_key, end_key, dest_key, skip_air)?;
            Ok(if count.is_some() == expected { vec![base] } else { Vec::new() })
        });
        registrar.exec(mode_lit, move |ctx| {
            let count = compare_regions(ctx, start_key, end_key, dest_key, skip_air)?;
            match (count, expected) {
                (Some(n), true) => {
                    ctx.send_success(format!("Test passed, count: {n}"));
                    Ok(n)
                }
                (None, false) => {
                    ctx.send_success("Test passed");
                    Ok(1)
                }
                (Some(n), false) => Err(format!("Test failed, count: {n}")),
                (None, true) => Err("Test failed".to_string()),
            }
        });
        registrar.redirect(mode_lit, execute);
    }
}

/// Vanilla's own cell cap (`ExecuteCommand.ERROR_AREA_TOO_LARGE`'s bound) —
/// `BoundingBox.fromCorners(start, end)`'s span product.
const MAX_BLOCKS_REGION_AREA: i64 = 32768;

/// `BoundingBox.fromCorners` plus the cell-by-cell walk
/// (`ExecuteCommand.checkRegions`). `Some(count)` when every considered cell
/// matches — every cell under `all`, every **non-air** source cell under
/// `masked` (vanilla's own `skipAir`) — `None` on the first mismatch,
/// short-circuiting the remaining scan exactly as vanilla's early `return
/// OptionalInt.empty()` does. `destination` is the corner the `[start, end]`
/// box's own **minimum** corner maps to, matching
/// `BoundingBox.fromCorners(destPos, destPos.offset(from.getLength()))`.
///
/// The block-state comparison is the raw canonical string — including any
/// `[...]` property suffix the store's own state string may carry, unlike
/// [`register_block_condition`]'s own user-typed-literal comparison (which
/// must strip it, since [`lodestone_command_mc::BlockArg`] cannot parse one
/// in). Both sides here come from the same [`crate::chunk::ChunkSource::block_state`]
/// accessor, so a raw compare is well-defined and closer to vanilla's own
/// full-`BlockState` comparison than the base-id reduction elsewhere in this
/// file.
fn compare_regions(
    ctx: &Ctx<'_>,
    start_key: ArgKey<lodestone_command_mc::Coordinates>,
    end_key: ArgKey<lodestone_command_mc::Coordinates>,
    dest_key: ArgKey<lodestone_command_mc::Coordinates>,
    skip_air: bool,
) -> Result<Option<i32>, String> {
    let Some(blocks) = ctx.world.blocks else {
        return Err("Blocks cannot be queried here".to_string());
    };
    let origin = (ctx.source.position.x, ctx.source.position.y, ctx.source.position.z);
    let rotation = (ctx.source.rotation.yaw, ctx.source.rotation.pitch);
    let resolve = |key: ArgKey<lodestone_command_mc::Coordinates>| -> (i32, i32, i32) {
        let coords = *ctx.get(key);
        let (x, y, z) = coords.resolve(origin, rotation);
        (x.floor() as i32, y.floor() as i32, z.floor() as i32)
    };
    let start = resolve(start_key);
    let end = resolve(end_key);
    let dest = resolve(dest_key);

    let (min_x, max_x) = min_max(start.0, end.0);
    let (min_y, max_y) = min_max(start.1, end.1);
    let (min_z, max_z) = min_max(start.2, end.2);
    let (span_x, span_y, span_z) =
        (i64::from(max_x - min_x + 1), i64::from(max_y - min_y + 1), i64::from(max_z - min_z + 1));

    let area = span_x * span_y * span_z;
    if area > MAX_BLOCKS_REGION_AREA {
        return Err(format!(
            "The size of the box ({area}) is greater than the maximum allowed ({MAX_BLOCKS_REGION_AREA})"
        ));
    }

    let offset = (dest.0 - min_x, dest.1 - min_y, dest.2 - min_z);

    let mut count: i32 = 0;
    for z in min_z..=max_z {
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let source_state = blocks.block_state(x, y, z);
                if skip_air && source_state == "minecraft:air" {
                    continue;
                }
                let (dx, dy, dz) = (x + offset.0, y + offset.1, z + offset.2);
                if source_state != blocks.block_state(dx, dy, dz) {
                    return Ok(None);
                }
                if blocks.block_entity(x, y, z) != blocks.block_entity(dx, dy, dz) {
                    return Ok(None);
                }
                count += 1;
            }
        }
    }
    Ok(Some(count))
}

fn min_max(a: i32, b: i32) -> (i32, i32) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// `biome <pos> <biome>` — `ResourceOrTagArgument`'s own boolean shape
/// (`ExecuteCommand`'s `biome` branch is `addConditional`'s
/// [`add_boolean_conditional`], same as `block`/`dimension`). Reads
/// [`crate::chunk::ChunkSource::biome_state_at`] — the primitive
/// [`crate::chunk::ChunkColumn::biome_state_at`] already exposed, now on the
/// trait the command layer reaches through — and refuses cleanly rather than
/// panicking when no chunk source is in scope, exactly like
/// [`register_block_condition`]. [`lodestone_command_mc::BiomeArg`] is a bare
/// id, no tag support — see that type's own doc for why that is the same
/// reduction [`lodestone_command_mc::BlockArg`] makes.
fn register_biome_condition(
    registrar: &mut Registrar,
    parent: NodeId,
    execute: NodeId,
    expected: bool,
) {
    let biome_lit = registrar.literal(parent, "biome");
    let (pos_node, pos_key) = registrar.arg(biome_lit, "pos", BlockPosArg);
    let (biome_node, biome_key) = registrar.arg(pos_node, "biome", BiomeArg);
    add_boolean_conditional(registrar, biome_node, execute, expected, move |ctx| {
        let Some(blocks) = ctx.world.blocks else {
            return Err("Blocks cannot be queried here".to_string());
        };
        let coords = *ctx.get(pos_key);
        let origin = (ctx.source.position.x, ctx.source.position.y, ctx.source.position.z);
        let rotation = (ctx.source.rotation.yaw, ctx.source.rotation.pitch);
        let (x, y, z) = coords.resolve(origin, rotation);
        let (x, y, z) = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
        let actual = blocks.biome_state_at(x, y, z);
        let expected_id = ctx.get(biome_key).biome.to_string();
        Ok(actual == expected_id)
    });
}

/// `loaded <pos>` — `ExecuteCommand.isChunkLoaded`'s own boolean shape
/// (`addConditional`, the same as `block`/`dimension`, not the count-shaped
/// `if entity`). Vanilla's own test is narrower than "generated": it also
/// requires the chunk's status to be `FullChunkStatus.ENTITY_TICKING` and
/// `ServerLevel::areEntitiesLoaded`, neither of which this crate tracks as a
/// distinct per-column state — [`crate::chunk::ChunkSource::is_column_resident`]
/// ("generated and retained at all", already reached through the same
/// `ctx.world.blocks` field `if block` uses) is the coarser, documented stand-in,
/// not a claim of exact parity. Refuses cleanly rather than panicking when no
/// chunk source is in scope, exactly like [`register_block_condition`].
fn register_loaded_condition(registrar: &mut Registrar, parent: NodeId, execute: NodeId, expected: bool) {
    let loaded_lit = registrar.literal(parent, "loaded");
    let (pos_node, pos_key) = registrar.arg(loaded_lit, "pos", BlockPosArg);
    add_boolean_conditional(registrar, pos_node, execute, expected, move |ctx| {
        let Some(blocks) = ctx.world.blocks else {
            return Err("Blocks cannot be queried here".to_string());
        };
        let coords = *ctx.get(pos_key);
        let origin = (ctx.source.position.x, ctx.source.position.y, ctx.source.position.z);
        let rotation = (ctx.source.rotation.yaw, ctx.source.rotation.pitch);
        let (x, _y, z) = coords.resolve(origin, rotation);
        let cx = (x.floor() as i32).div_euclid(16);
        let cz = (z.floor() as i32).div_euclid(16);
        Ok(blocks.is_column_resident(cx, cz))
    });
}

/// `block <pos> <block>` — `BlockPredicateArgument`'s own boolean shape
/// (`ExecuteCommand`'s `block` branch is `addConditional`'s
/// [`add_boolean_conditional`], same as `dimension`, not the count-shaped
/// `if entity`). Compares only the base block id, matching
/// [`lodestone_command_mc::BlockArg`]'s own v1 reduction (no property list —
/// `minecraft:furnace[facing=north]` matches `if block ~ ~ ~ furnace`
/// regardless of `facing`), and refuses cleanly rather than panicking when no
/// chunk source is in scope (`ctx.world.blocks` is `None` for RCON and this
/// module's own test helpers — see [`super::registrar::CommandWorld::blocks`]'s
/// own doc for the full list of who gets `Some`).
fn register_block_condition(
    registrar: &mut Registrar,
    parent: NodeId,
    execute: NodeId,
    expected: bool,
) {
    let block_lit = registrar.literal(parent, "block");
    let (pos_node, pos_key) = registrar.arg(block_lit, "pos", BlockPosArg);
    let (block_node, block_key) = registrar.arg(pos_node, "block", BlockArg);
    add_boolean_conditional(registrar, block_node, execute, expected, move |ctx| {
        let Some(blocks) = ctx.world.blocks else {
            return Err("Blocks cannot be queried here".to_string());
        };
        let coords = *ctx.get(pos_key);
        let origin = (ctx.source.position.x, ctx.source.position.y, ctx.source.position.z);
        let rotation = (ctx.source.rotation.yaw, ctx.source.rotation.pitch);
        let (x, y, z) = coords.resolve(origin, rotation);
        let (x, y, z) = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
        let state = blocks.block_state(x, y, z);
        // The base id, stripping any `[...]` property suffix the store's
        // canonical state string may carry — see this function's own doc.
        let actual = state.split('[').next().unwrap_or(state.as_str());
        let expected_id = ctx.get(block_key).block.to_string();
        Ok(actual == expected_id)
    });
}

/// `data storage <source> <path>` — `DataCommand`'s own numeric-conditional
/// shape (`NbtPathArgument.NbtPath.count`), matching `if entity`'s count
/// rather than `if score`'s boolean: real vanilla paths can carry wildcards
/// that match more than one element, so the underlying primitive is a count
/// even though [`lodestone_command_mc::NbtPathArg`]'s v1 grammar (no
/// indices, no filter compounds) can only ever produce `0` or `1` here.
/// `if data entity`/`if data block` are not registered — see
/// `crate::commands::nbt_data`'s module doc for why only `storage` exists.
fn register_data_storage_condition(
    registrar: &mut Registrar,
    parent: NodeId,
    execute: NodeId,
    expected: bool,
) {
    let data_lit = registrar.literal(parent, "data");
    let storage_lit = registrar.literal(data_lit, "storage");
    let (id_node, id_key) = registrar.arg(storage_lit, "source", lodestone_command_mc::StorageIdArg);
    let (path_node, path_key) = registrar.arg(id_node, "path", lodestone_command_mc::NbtPathArg);
    add_numeric_conditional(registrar, path_node, execute, expected, move |ctx| {
        let id = ctx.get(id_key).clone();
        let path = ctx.get(path_key).clone();
        Ok(i32::from(ctx.world.state.nbt_storage().get(&id, &path).is_some()))
    });
}

/// `score <target> <targetObjective> matches <range>` and `score <target>
/// <targetObjective> <op> <source> <sourceObjective>` — the two shapes
/// `ExecuteCommand.addConditional`'s own `score` branch registers. Both are
/// boolean tests (a comparison result, not a count), so both use
/// [`add_boolean_conditional`], matching the reference: vanilla's own
/// `score` conditional is not fork-counted the way `if entity`'s match count
/// is.
fn register_score_conditions(registrar: &mut Registrar, parent: NodeId, execute: NodeId, expected: bool) {
    let score_lit = registrar.literal(parent, "score");
    let (target_node, target_key) = registrar.arg(score_lit, "target", ScoreHolderArg::single());
    let (target_obj_node, target_obj_key) = registrar.arg(target_node, "targetObjective", ObjectiveArg);

    // `matches <range>`.
    let matches_lit = registrar.literal(target_obj_node, "matches");
    let (range_node, range_key) = registrar.arg(matches_lit, "range", IntRangeArg);
    add_boolean_conditional(registrar, range_node, execute, expected, move |ctx| {
        let holder = super::scoreboard::resolve_single(ctx, target_key)?;
        let objective = ctx.get(target_obj_key).clone();
        let range = *ctx.get(range_key);
        let value = ctx
            .world
            .state
            .scoreboard()
            .get_score(&holder, &objective)
            .map_err(|e| e.to_string())?;
        Ok(range.matches(value))
    });

    // `<op> <source> <sourceObjective>`, one literal child per comparison
    // token — vanilla's tree registers these as five *literals*
    // (`Commands.literal("<")`, …), not `minecraft:operation`'s nine-token
    // argument (that parser is `/scoreboard players operation`'s own, a
    // different, larger token set).
    for token in ["<", "<=", "=", ">=", ">"] {
        let op_lit = registrar.literal(target_obj_node, token);
        let (source_node, source_key) = registrar.arg(op_lit, "source", ScoreHolderArg::single());
        let (source_obj_node, source_obj_key) = registrar.arg(source_node, "sourceObjective", ObjectiveArg);
        let token = token.to_string();
        add_boolean_conditional(registrar, source_obj_node, execute, expected, move |ctx| {
            let target_holder = super::scoreboard::resolve_single(ctx, target_key)?;
            let target_objective = ctx.get(target_obj_key).clone();
            let source_holder = super::scoreboard::resolve_single(ctx, source_key)?;
            let source_objective = ctx.get(source_obj_key).clone();
            let target = ctx
                .world
                .state
                .scoreboard()
                .get_score(&target_holder, &target_objective)
                .map_err(|e| e.to_string())?;
            let source = ctx
                .world
                .state
                .scoreboard()
                .get_score(&source_holder, &source_objective)
                .map_err(|e| e.to_string())?;
            Ok(match token.as_str() {
                "<" => target < source,
                "<=" => target <= source,
                "=" => target == source,
                ">=" => target >= source,
                ">" => target > source,
                _ => unreachable!("token is one of the five spelled out above"),
            })
        });
    }
}

/// `ExecuteCommand.addConditional` — a boolean test attached as both a fork
/// modifier (continuing the chain) and an executor (`execute if dimension
/// <d>` alone). See this module's doc for why both are needed on one node and
/// what closes the gap between them.
fn add_boolean_conditional(
    registrar: &mut Registrar,
    node: NodeId,
    execute: NodeId,
    expected: bool,
    test: impl Fn(&Ctx<'_>) -> Result<bool, String> + Send + Sync + Clone + 'static,
) {
    let modifier_test = test.clone();
    registrar.modifier(node, true, move |ctx, sources, _parsed| {
        let base = one(sources);
        let ok = modifier_test(ctx)?;
        Ok(if ok == expected { vec![base] } else { Vec::new() })
    });
    registrar.exec(node, move |ctx| {
        let ok = test(ctx)?;
        if ok == expected {
            ctx.send_success("Test passed");
            Ok(1)
        } else {
            Err("Test failed".to_string())
        }
    });
    registrar.redirect(node, execute);
}

/// `ExecuteCommand.createNumericConditionalHandler` — vanilla's other shape,
/// used by `if`/`unless entity` (the count is how many entities matched, not
/// merely whether any did) and by every condition this module does not build
/// (`if items`, `if data`, …). `if` succeeds and returns the count when it is
/// positive; `unless` succeeds (returning `1`) only when the count is exactly
/// zero.
fn add_numeric_conditional(
    registrar: &mut Registrar,
    node: NodeId,
    execute: NodeId,
    expected: bool,
    count: impl Fn(&Ctx<'_>) -> Result<i32, String> + Send + Sync + Clone + 'static,
) {
    let modifier_count = count.clone();
    registrar.modifier(node, true, move |ctx, sources, _parsed| {
        let base = one(sources);
        let n = modifier_count(ctx)?;
        Ok(if (n > 0) == expected { vec![base] } else { Vec::new() })
    });
    registrar.exec(node, move |ctx| {
        let n = count(ctx)?;
        if expected {
            if n > 0 {
                ctx.send_success(format!("Test passed, count: {n}"));
                Ok(n)
            } else {
                Err("Test failed".to_string())
            }
        } else if n == 0 {
            ctx.send_success("Test passed");
            Ok(1)
        } else {
            Err(format!("Test failed, count: {n}"))
        }
    });
    registrar.redirect(node, execute);
}

// ---- shared helpers -----------------------------------------------------

/// Every [`super::registrar::ModifierEntry`] here is called with exactly one
/// source — [`super::registrar::Dispatcher::dispatch`]'s own doc states this
/// invariant. Named rather than inlined so every call site above reads the
/// same way.
fn one(mut sources: Vec<CommandSource>) -> CommandSource {
    sources.pop().expect("Dispatcher::dispatch hands a modifier exactly one source")
}

/// `EntityArgument.getOptionalEntities` — unlike [`Ctx::resolve`]'s own
/// [`SelectorError::NoPlayersFound`] (right for a plain `<targets>` argument,
/// which must refuse an empty match), a fork or a numeric condition treats
/// "matched nobody" as **zero results**, not an error: `execute as @e[type=…]
/// run …` with no matches silently runs nothing, and `execute if entity
/// @e[type=…]` reports a normal (failing) test rather than a parse-shaped
/// refusal.
fn resolve_optional(ctx: &Ctx<'_>, selector: &EntitySelector) -> Result<Vec<PlayerCandidate>, String> {
    match ctx.resolve(selector) {
        Ok(candidates) => Ok(candidates),
        Err(SelectorError::NoPlayersFound) => Ok(Vec::new()),
        Err(other) => Err(other.to_string()),
    }
}

/// `EntityAnchorArgument.Anchor.apply(CommandSourceStack)` — `eyes` only adds
/// height when the source actually has a body (vanilla returns the plain
/// position outright for a bodiless source, e.g. RCON, regardless of anchor).
fn anchor_position(source: &CommandSource) -> lodestone_model::Vec3 {
    if source.entity.is_some() && source.anchor == EntityAnchor::Eyes {
        lodestone_model::Vec3::new(source.position.x, source.position.y + EYE_HEIGHT, source.position.z)
    } else {
        source.position
    }
}

/// `EntityAnchorArgument.Anchor.apply(Entity)` — a target entity always has a
/// body, so `eyes` always adds height (no bodiless case to guard against, the
/// asymmetry [`anchor_position`]'s own doc names).
fn candidate_anchor_position(candidate: &PlayerCandidate, anchor: AnchorInput) -> lodestone_model::Vec3 {
    if anchor == AnchorInput::Eyes {
        lodestone_model::Vec3::new(candidate.position.x, candidate.position.y + EYE_HEIGHT, candidate.position.z)
    } else {
        candidate.position
    }
}

/// `Mth.wrapDegrees(float)` — wraps to `[-180, 180)`.
fn wrap_degrees(value: f32) -> f32 {
    let mut result = value % 360.0;
    if result >= 180.0 {
        result -= 360.0;
    }
    if result < -180.0 {
        result += 360.0;
    }
    result
}

/// `CommandSourceStack.facing(Vec3)` — the yaw/pitch that looks from `from`
/// toward `to`, computed in `f64` throughout (matching the jar's own
/// `Vec3`-typed inputs) and only narrowed to `f32` for the final wrap, the
/// same order of operations `Mth.atan2` feeds into `wrapDegrees(float)`.
fn compute_facing(from: (f64, f64, f64), to: (f64, f64, f64)) -> (f32, f32) {
    let xd = to.0 - from.0;
    let yd = to.1 - from.1;
    let zd = to.2 - from.2;
    let sd = (xd * xd + zd * zd).sqrt();
    #[allow(clippy::cast_possible_truncation)]
    let pitch = wrap_degrees((-(yd.atan2(sd) * 180.0 / std::f64::consts::PI)) as f32);
    #[allow(clippy::cast_possible_truncation)]
    let yaw = wrap_degrees(((zd.atan2(xd) * 180.0 / std::f64::consts::PI) - 90.0) as f32);
    (yaw, pitch)
}
