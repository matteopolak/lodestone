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
//! `as`, `at`, `positioned` (+ `as`), `rotated` (+ redirect only — see gap
//! below for why `rotated as` is not registered), `facing` (`<pos>` and
//! `entity <targets> <anchor>`), `align`, `anchored`, `in` (single-dimension
//! census — see [`lodestone_command_mc::DimensionArg`]'s own doc), `run`, and
//! `if`/`unless entity`/`if`/`unless dimension`.
//!
//! # What is not built, and why — each names its own missing subsystem
//!
//! * **`rotated as <targets>`.** Vanilla copies the target's own rotation
//!   (`entity.getRotationVector()`). `crate::commands::source::PlayerCandidate`
//!   carries a position but no rotation — the identical, already-documented gap
//!   `crate::commands::teleport`'s module doc names for `/tp <targets>
//!   <destination>`. Unlike `at` (which still delivers a real position change
//!   even without a rotation copy), rotation is `rotated as`'s *entire*
//!   purpose, so shipping it as a silent no-op would be worse than not
//!   registering it — this subtree is simply absent, a disclosed reduction
//!   rather than a silent one.
//! * **`at`'s rotation.** Same root cause: position and dimension transfer for
//!   real, rotation does not — the pre-`at` source's own rotation is kept.
//! * **`store`, `if`/`unless score`, `predicate`, `data`, `items`, `function`,
//!   `stopwatch`, `if`/`unless block`/`biome`/`blocks`/`loaded`, and `on
//!   <relation>`.** Each needs a subsystem this server has nowhere: a
//!   scoreboard (`store score`, `if score`), NBT storage/paths (`store …
//!   <path>`, `if data`), a loot-predicate engine (`if predicate`), functions
//!   (explicitly out of scope for this unit — issue #48's remainder tracks
//!   functions/datapacks separately), a stopwatch registry, a read-only block/
//!   biome/chunk-residency query on [`CommandWorld`] (which today only ever
//!   *writes* blocks, through [`super::Effect::SetBlock`]/[`super::Effect::Fill`]
//!   — see that enum's own doc for why even those two are self-targeted-only),
//!   and entity relationship queries (owner/leasher/target/attacker/vehicle/
//!   controller/origin/passengers) this crate's mob simulation does not expose
//!   to a command executor.
//! * **`execute summon <entity>`** (the modifier form that also changes the
//!   acting entity). Not needed as its own subtree: `/summon` is already a
//!   root command, so `execute at @s run summon minecraft:cow` reaches it
//!   through `run` with no new code at all — only the source-rewriting
//!   `.redirect(execute, spawnEntityAndRedirect)` form (which additionally
//!   makes the *newly summoned* entity the acting source for anything chained
//!   after it) is absent.
//! * **`positioned over <heightmap>`.** Needs a heightmap query `CommandWorld`
//!   does not carry.
//!
//! # Command blocks are the other half of this issue and are not this file
//!
//! See `crate::block_entities`'s `BlockEntity::CommandBlock` variant and
//! `crate::command_block` for the data model and pure tick semantics ported
//! from `CommandBlockEntity`/`BaseCommandBlock`/`CommandBlock.java`, and their
//! own module docs for exactly how far that got and what is still needed to
//! reach a running command block end-to-end.

use lodestone_command::NodeId;
use lodestone_command_mc::{
    AnchorInput, Axes, DimensionArg, EntityAnchorArg, EntityArg, EntitySelector, RotationArg,
    SwizzleArg, Vec3Arg,
};
use lodestone_model::Rotation;

use super::registrar::{Ctx, Registrar};
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
}

/// `literal("run").redirect(dispatcher.getRoot())` — no modifier, so the
/// current source set passes through unchanged into a full re-parse of the
/// whole tree. See this module's doc for why that alone is what makes
/// `execute as Steve run kill` affect Steve.
fn register_run(registrar: &mut Registrar, execute: NodeId, root: NodeId) {
    let run = registrar.literal(execute, "run");
    registrar.redirect(run, root);
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
/// *where*, leaving the acting entity untouched. Rotation is not transferred;
/// see this module's doc for why (`PlayerCandidate` carries no rotation).
/// Dimension is `base.dimension` unchanged for the same reason `/tp`'s own
/// module doc gives elsewhere: every candidate on this server's one roster is
/// already in the one hosted dimension.
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

    // `rotated as <targets>` is not registered — see this module's doc.
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
