//! `/setworldspawn` and `/spawnpoint`, from `SetWorldSpawnCommand.java` and
//! `SpawnPointCommand.java`.
//!
//! # `/spawnpoint` is self-only here, unlike vanilla's `<targets>` form
//!
//! Vanilla's tree accepts `/spawnpoint <targets> <pos> <angle>` for any set of
//! players. [`crate::world_spawn::RespawnPoint`] is a **connection-local**
//! variable (`dispatch_play_packet`'s own `respawn: &mut Option<RespawnPoint>`
//! parameter, written today only by the bed-click arm), and reaching another
//! connection's copy of it would need the same directed-effect plumbing
//! `/gamemode <target>` uses — [`crate::commands::Effect`] carries no variant for
//! it yet. So this registers only the no-target and `@s`-implicit forms; a
//! multi-target `/spawnpoint` is future work once a `SetRespawnPoint` effect
//! exists.
//!
//! # The angle argument is a plain bounded float, not `minecraft:angle`
//!
//! Vanilla's `<angle>` node is `AngleArgument.angle()`, wired as
//! `minecraft:angle` (id 28, no payload). `lodestone-command-mc` exposes no
//! `McArg` for that parser yet, so this uses `FloatArgument` bounded to
//! `-180.0..=180.0` — the same *value* domain, wired as `brigadier:float`
//! instead. No captured fixture pins either command's tree shape, so this is a
//! documented approximation rather than a parity gap against anything real.

use lodestone_command::FloatArgument;
use lodestone_command_mc::{BlockPosArg, Coordinates};
use lodestone_model::Vec3;

use super::registrar::{Ctx, Registrar};
use super::CommandResult;

/// `Commands.LEVEL_GAMEMASTERS`.
const SPAWN_LEVEL: u8 = 2;

/// Resolves a parsed [`Coordinates`] against the caller's own position — the
/// shared half of both commands' "no explicit position" and "explicit position"
/// forms.
fn resolve_pos(ctx: &Ctx<'_>, coords: Coordinates) -> Vec3 {
    let origin = (ctx.source.position.x, ctx.source.position.y, ctx.source.position.z);
    let rotation = (ctx.source.rotation.yaw, ctx.source.rotation.pitch);
    let (x, y, z) = coords.resolve(origin, rotation);
    Vec3::new(x, y, z)
}

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();

    // ---- /setworldspawn -----------------------------------------------------
    let setworldspawn = registrar.literal(root, "setworldspawn");
    registrar.require_level(setworldspawn, SPAWN_LEVEL);
    registrar.exec(setworldspawn, |ctx| {
        let pos = ctx.source.position;
        let yaw = ctx.source.rotation.yaw;
        set_world_spawn(ctx, pos, yaw)
    });

    let (pos_node, pos_key) = registrar.arg(setworldspawn, "pos", BlockPosArg);
    registrar.exec(pos_node, move |ctx| {
        let pos = resolve_pos(ctx, *ctx.get(pos_key));
        set_world_spawn(ctx, pos, 0.0)
    });

    let (angle_node, angle_key) =
        registrar.arg(pos_node, "angle", FloatArgument::bounded(-180.0, 180.0));
    registrar.exec(angle_node, move |ctx| {
        let pos = resolve_pos(ctx, *ctx.get(pos_key));
        let yaw = *ctx.get(angle_key);
        set_world_spawn(ctx, pos, yaw)
    });

    // ---- /spawnpoint (self-only; see module doc) -----------------------------
    let spawnpoint = registrar.literal(root, "spawnpoint");
    registrar.require_level(spawnpoint, SPAWN_LEVEL);
    registrar.exec(spawnpoint, |ctx| {
        let pos = ctx.source.position;
        set_spawnpoint(ctx, pos)
    });

    let (sp_pos_node, sp_pos_key) = registrar.arg(spawnpoint, "pos", BlockPosArg);
    registrar.exec(sp_pos_node, move |ctx| {
        let pos = resolve_pos(ctx, *ctx.get(sp_pos_key));
        set_spawnpoint(ctx, pos)
    });

    // Vanilla's angle node exists on `/spawnpoint` too, but `RespawnPoint` here
    // carries no yaw at all (see the module doc) — parsed and discarded rather
    // than left off the tree, so a client that typed one still gets a command
    // that runs instead of a parse error.
    let (sp_angle_node, _sp_angle_key) =
        registrar.arg(sp_pos_node, "angle", FloatArgument::bounded(-180.0, 180.0));
    registrar.exec(sp_angle_node, move |ctx| {
        let pos = resolve_pos(ctx, *ctx.get(sp_pos_key));
        set_spawnpoint(ctx, pos)
    });
}

fn set_world_spawn(ctx: &mut Ctx<'_>, pos: Vec3, yaw: f32) -> CommandResult {
    ctx.world.state.set_world_spawn(crate::world_spawn::WorldSpawn { pos, yaw, pitch: 0.0 });
    ctx.send_success(format!(
        "Set the world spawn point to ({}, {}, {})",
        pos.x.floor() as i64,
        pos.y.floor() as i64,
        pos.z.floor() as i64
    ));
    Ok(1)
}

fn set_spawnpoint(ctx: &mut Ctx<'_>, pos: Vec3) -> CommandResult {
    let Some(uuid) = ctx.source.uuid() else {
        return Err("That command can only be used by a player".to_string());
    };
    let block_pos = lodestone_model::BlockPos::new(
        pos.x.floor() as i32,
        pos.y.floor() as i32,
        pos.z.floor() as i32,
    );
    ctx.effect(uuid, crate::commands::Effect::SetRespawnPoint { pos: block_pos });
    ctx.send_success(format!(
        "Set the spawn point to ({}, {}, {}) in Overworld",
        block_pos.x, block_pos.y, block_pos.z
    ));
    Ok(1)
}
