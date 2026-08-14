//! `/tp`/`/teleport`, from `TeleportCommand.java` — the mechanism this
//! command needed was a reusable post-join teleport encoder
//! ([`crate::ServerProtocol::encode_teleport`]), not new command-tree
//! machinery: the existing directed-[`Effect`] outbox already reaches any
//! connected player, so this is the same shape `/kill` and `/gamemode
//! <target>` already use.
//!
//! # `/tp` and `/teleport` are two independently-built trees, not a redirect
//!
//! Vanilla registers `teleport` fully and then redirects `tp` to it
//! (`Commands::redirect`). [`super::registrar::Registrar::redirect`] exists
//! but has **no production caller yet** — every built-in here is a plain,
//! self-contained tree — and this command is not the place to be the first:
//! a redirect changes which node the dispatch walk's argument-depth
//! accounting sees, and getting that wrong would be a `/tp`-shaped panic
//! rather than a `/tp`-shaped teleport. So this registers the same subtree
//! twice, under both names. No captured fixture pins either command's wire
//! shape (see `crate::commands`' module doc — only the original four have
//! one), so the duplication costs nothing this crate is gated on.
//!
//! # `<location>` resolves against the **command source**, never the target
//!
//! `Vec3Argument.getCoordinates(c, "location")` is resolved by
//! `Coordinates.getPosition(CommandSourceStack source)` — the issuer's own
//! position and rotation, not any of `<targets>`'s. So `/tp Steve ~5 ~ ~`
//! moves Steve five blocks from *the operator's* position, not Steve's own —
//! surprising in English, exactly what the record specifies, and why
//! `resolve_location` below takes `ctx.source`, never a target.
//!
//! # `<rotation>` is two plain floats, not `minecraft:rotation`
//!
//! Same documented approximation `crate::commands::world_spawn_commands` makes
//! for `/setworldspawn`'s angle: `lodestone-command-mc` has no `RotationArg`
//! parser yet, so this splits vanilla's one `~`-capable two-component argument
//! into two absolute `brigadier:float` nodes (`yaw`, `pitch`). Relative
//! rotation (`/tp @s ~ ~45`) is therefore not supported — a real gap, not a
//! silent one.
//!
//! # Facing preservation is resolved at *application* time, not here
//!
//! When no rotation is given, the target keeps its own current facing —
//! vanilla's `entity.getYRot()`/`getXRot()`. This crate's
//! [`crate::commands::source::PlayerCandidate`] carries a position but no
//! rotation (see that module's own doc for why), so an executor cannot look
//! it up. [`Effect::Teleport`]'s `yaw`/`pitch` are therefore `Option<f32>`,
//! and `None` is resolved by whichever connection actually applies the
//! effect — its own for a self-teleport, the target's own connection for a
//! directed one — because that is the only place a live `player_rot` for
//! that specific player is ever in scope. The same reasoning, and the same
//! gap, applies to `/tp <targets> <destination>`: vanilla copies the
//! destination's rotation too (`performTeleport`'s `destination.getYRot()`),
//! and this crate cannot read a `PlayerCandidate`'s rotation any more than it
//! can a target's — so the entity-destination form preserves the *target's
//! own* facing instead, a documented approximation rather than the vanilla
//! copy.
//!
//! # There is no bare, `@s`-free `/tp <entity>` self-form
//!
//! Vanilla's own tree has `<location>`, `<destination>` and `<targets>` as
//! three *simultaneous* argument children of `teleport`, and disambiguating
//! `/tp Steve` (self, to the player named Steve — the bare `<destination>`
//! path) from `/tp Steve ~5 ~ ~` (move Steve — `<targets>` then `<location>`)
//! needs exactly the backtracking `lodestone_command::CommandTree::parse`'s
//! own doc comment says it does not have: "argument children are tried in
//! insertion order and the **first** success wins" — no retry across
//! siblings when the winning branch turns out incomplete. A bare name is
//! valid syntax for *both* `<destination>` (single) and `<targets>`
//! (multi), so whichever is registered first always wins outright, and every
//! tree order was tried: putting `<destination>` first breaks every
//! `<targets>`-prefixed form (`/tp Steve ~5 ~ ~` reads `Steve` as the whole
//! command and fails on the leftover `~5 ~ ~`); putting `<targets>` first
//! breaks the bare self-form (`/tp Steve` alone commits to `<targets>`,
//! which has no executor of its own, and refuses instead of falling back).
//!
//! Losing the general, `<targets>`-prefixed forms would be losing the more
//! valuable half of the command, so this tree drops the bare top-level
//! `<destination>` node entirely: `/tp <targets>` always wins the "starts
//! with an entity selector" position, and self-to-entity is reached through
//! it with an explicit `@s` (`/tp @s Steve`), which is unambiguous — `@s` is
//! never a bare name, so it cannot collide with `<location>`'s numeric/`~`/
//! `^` grammar either. A real, disclosed reduction from vanilla's own tree,
//! not a silent one; `/tp <location>` (also bare, self, but numeric-first so
//! it never competes with an entity selector) is unaffected.

use lodestone_command::FloatArgument;
use lodestone_command_mc::{Coordinates, EntityArg, EntitySelector, Vec3Arg};

use super::effect::Effect;
use super::registrar::{Ctx, Registrar};
use super::source::PlayerCandidate;
use super::CommandResult;

/// `Commands.LEVEL_GAMEMASTERS`.
const TELEPORT_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    for name in ["teleport", "tp"] {
        register_tree(registrar, root, name);
    }
}

fn register_tree(registrar: &mut Registrar, root: lodestone_command::NodeId, name: &str) {
    let tp = registrar.literal(root, name);
    registrar.require_level(tp, TELEPORT_LEVEL);

    // ---- /tp <location> — self ------------------------------------------
    let (loc_node, loc_key) = registrar.arg(tp, "location", Vec3Arg::new());
    registrar.exec(loc_node, move |ctx| {
        let me = self_uuid(ctx)?;
        let coords = *ctx.get(loc_key);
        let (x, y, z) = resolve_location(ctx, coords);
        let name = ctx.source.name.clone();
        ctx.effect(me, Effect::Teleport { x, y, z, yaw: None, pitch: None });
        ctx.send_success(format!("Teleported {name} to {}", fmt_pos(x, y, z)));
        Ok(1)
    });

    // No bare top-level `<destination>` (self to entity, no `@s`) — see the
    // module doc's "There is no bare, `@s`-free `/tp <entity>` self-form"
    // section for why `<targets>` must win the "starts with an entity
    // selector" position outright, and `/tp @s <entity>` (through
    // `<targets>` → `<destination>` below) is the reachable equivalent.

    // ---- /tp <targets> <location> [<yaw> <pitch>] ------------------------
    let (targets_node, targets_key) = registrar.arg(tp, "targets", EntityArg::players());
    let (targets_loc_node, targets_loc_key) = registrar.arg(targets_node, "location", Vec3Arg::new());
    registrar.exec(targets_loc_node, move |ctx| {
        let selector = ctx.get(targets_key).clone();
        let coords = *ctx.get(targets_loc_key);
        let (x, y, z) = resolve_location(ctx, coords);
        teleport_targets(ctx, &selector, x, y, z, None, None)
    });

    let (yaw_node, yaw_key) =
        registrar.arg(targets_loc_node, "yaw", FloatArgument::bounded(-180.0, 180.0));
    let (pitch_node, pitch_key) =
        registrar.arg(yaw_node, "pitch", FloatArgument::bounded(-90.0, 90.0));
    registrar.exec(pitch_node, move |ctx| {
        let selector = ctx.get(targets_key).clone();
        let coords = *ctx.get(targets_loc_key);
        let (x, y, z) = resolve_location(ctx, coords);
        let yaw = *ctx.get(yaw_key);
        let pitch = *ctx.get(pitch_key);
        teleport_targets(ctx, &selector, x, y, z, Some(yaw), Some(pitch))
    });

    // ---- /tp <targets> <destination> -------------------------------------
    let (targets_dest_node, targets_dest_key) =
        registrar.arg(targets_node, "destination", EntityArg::player());
    registrar.exec(targets_dest_node, move |ctx| {
        let selector = ctx.get(targets_key).clone();
        let dest_selector = ctx.get(targets_dest_key).clone();
        let destination = resolve_one(ctx, &dest_selector)?;
        let (x, y, z) = (destination.position.x, destination.position.y, destination.position.z);
        teleport_targets(ctx, &selector, x, y, z, None, None)
    });
}

/// The caller's own uuid, or vanilla's console refusal.
fn self_uuid(ctx: &Ctx<'_>) -> Result<uuid::Uuid, String> {
    ctx.source.uuid().ok_or_else(|| "That command can only be used by a player".to_string())
}

/// One entity from a selector, or vanilla's `NO_ENTITIES_FOUND` shape.
fn resolve_one(ctx: &Ctx<'_>, selector: &EntitySelector) -> Result<PlayerCandidate, String> {
    let candidates = ctx.resolve(selector)?;
    candidates.into_iter().next().ok_or_else(|| "No entity was found".to_string())
}

/// `Vec3Argument.getCoordinates(c, "location")`'s resolution:
/// [`Coordinates::resolve`] against the **command source**'s own position and
/// rotation — see this module's doc for why it is never a target's.
fn resolve_location(ctx: &Ctx<'_>, coords: Coordinates) -> (f64, f64, f64) {
    let origin = (ctx.source.position.x, ctx.source.position.y, ctx.source.position.z);
    let rotation = (ctx.source.rotation.yaw, ctx.source.rotation.pitch);
    coords.resolve(origin, rotation)
}

/// Queues [`Effect::Teleport`] for every resolved target and reports vanilla's
/// single/multiple split.
fn teleport_targets(
    ctx: &mut Ctx<'_>,
    selector: &EntitySelector,
    x: f64,
    y: f64,
    z: f64,
    yaw: Option<f32>,
    pitch: Option<f32>,
) -> CommandResult {
    let targets = ctx.resolve(selector)?;
    for target in &targets {
        ctx.effect(target.uuid, Effect::Teleport { x, y, z, yaw, pitch });
    }
    if let [only] = targets.as_slice() {
        ctx.send_success(format!("Teleported {} to {}", only.username, fmt_pos(x, y, z)));
    } else {
        ctx.send_success(format!("Teleported {} entities to {}", targets.len(), fmt_pos(x, y, z)));
    }
    Ok(i32::try_from(targets.len()).unwrap_or(i32::MAX))
}

fn fmt_pos(x: f64, y: f64, z: f64) -> String {
    format!("{x:.2}, {y:.2}, {z:.2}")
}
