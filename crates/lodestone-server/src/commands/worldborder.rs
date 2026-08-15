//! `/worldborder`, from `WorldBorderCommand.java` (issue #580).
//!
//! # What it is
//!
//! The command half of a resize: `set`/`add` change the size (optionally
//! lerping over `<time>` ticks), `center` moves it, `damage amount`/`damage
//! buffer` and `warning distance`/`warning time` change the enforcement/HUD
//! scalars `crate::border::WorldBorder` already carries, and `get` reports
//! the current size. Every subcommand is `ctx.world.border`'s
//! [`crate::border::BorderFeed::with`] applied to one setter — the decision
//! logic (refusal messages, clamping) is the only real content here, ported
//! clause by clause from the jar's own `setSize`/`setCenter`/etc.
//!
//! # `Vec2Argument` has no wire type here, so `center` takes two doubles
//!
//! Vanilla's tree uses one `Vec2Argument` node for `<pos>`; this server's
//! [`lodestone_model::command_tree::ArgumentParser`] has no wire entry for
//! it, the same gap [`crate::commands::difficulty`]'s own doc names for
//! `DifficultyArgument`. Two chained [`DoubleArgument`] nodes (`x` then `z`)
//! parse the identical input and reach the identical two `f64`s; only tab
//! completion's node shape differs from vanilla's, not what a typed command
//! does.
//!
//! # Reachability (see `CommandWorld::border`'s own doc)
//!
//! A connected player's `ChatCommand` arm always has `Some(border)` — the
//! shared [`crate::border::BorderFeed`] `crate::tick::run_tick_loop_with_weather`
//! now ticks. RCON, a command block, and this module's own test helper build
//! a [`super::registrar::CommandWorld`] with `border: None`, in which case
//! every subcommand here refuses with a stated reason rather than mutating a
//! border nothing reads.

use lodestone_command::{DoubleArgument, FloatArgument, IntegerArgument};
use lodestone_command_mc::TimeArg;

use super::registrar::{Ctx, Registrar};
use super::CommandResult;
use crate::border::{BorderFeed, MAX_CENTER_COORDINATE, MAX_SIZE};

/// `Commands.LEVEL_GAMEMASTERS`.
const WORLDBORDER_LEVEL: u8 = 2;

/// The refusal every subcommand shares when `ctx.world.border` is `None` —
/// see the module doc's "Reachability" section for which callers hit this.
const NO_BORDER: &str = "The world border is not available from here";

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let worldborder = registrar.literal(root, "worldborder");
    registrar.require_level(worldborder, WORLDBORDER_LEVEL);

    // `/worldborder get`.
    let get = registrar.literal(worldborder, "get");
    registrar.exec(get, get_size);

    // `/worldborder set <distance> [<time>]`.
    let set = registrar.literal(worldborder, "set");
    let (set_distance_node, set_distance_key) =
        registrar.arg(set, "distance", DoubleArgument::bounded(-MAX_SIZE, MAX_SIZE));
    registrar.exec(set_distance_node, move |ctx| {
        let distance = *ctx.get(set_distance_key);
        set_size(ctx, distance, 0)
    });
    let (set_time_node, set_time_key) = registrar.arg(set_distance_node, "time", TimeArg { min: 0 });
    registrar.exec(set_time_node, move |ctx| {
        let distance = *ctx.get(set_distance_key);
        let ticks = *ctx.get(set_time_key);
        set_size(ctx, distance, i64::from(ticks))
    });

    // `/worldborder add <distance> [<time>]`.
    let add = registrar.literal(worldborder, "add");
    let (add_distance_node, add_distance_key) =
        registrar.arg(add, "distance", DoubleArgument::bounded(-MAX_SIZE, MAX_SIZE));
    registrar.exec(add_distance_node, move |ctx| {
        let delta = *ctx.get(add_distance_key);
        add_size(ctx, delta, 0)
    });
    let (add_time_node, add_time_key) = registrar.arg(add_distance_node, "time", TimeArg { min: 0 });
    registrar.exec(add_time_node, move |ctx| {
        let delta = *ctx.get(add_distance_key);
        let ticks = *ctx.get(add_time_key);
        add_size(ctx, delta, i64::from(ticks))
    });

    // `/worldborder center <x> <z>` — see the module doc for why this is two
    // chained doubles rather than one `Vec2Argument`-shaped node.
    let center = registrar.literal(worldborder, "center");
    let (center_x_node, center_x_key) = registrar.arg(center, "x", DoubleArgument::new());
    let (center_z_node, center_z_key) = registrar.arg(center_x_node, "z", DoubleArgument::new());
    registrar.exec(center_z_node, move |ctx| {
        let x = *ctx.get(center_x_key);
        let z = *ctx.get(center_z_key);
        set_center(ctx, x, z)
    });

    // `/worldborder damage amount <damagePerBlock>` and `.../buffer <distance>`.
    let damage = registrar.literal(worldborder, "damage");
    let damage_amount = registrar.literal(damage, "amount");
    let (damage_amount_node, damage_amount_key) =
        registrar.arg(damage_amount, "damagePerBlock", FloatArgument::bounded(0.0, f32::MAX));
    registrar.exec(damage_amount_node, move |ctx| {
        let damage_per_block = *ctx.get(damage_amount_key);
        set_damage_amount(ctx, damage_per_block)
    });
    let damage_buffer = registrar.literal(damage, "buffer");
    let (damage_buffer_node, damage_buffer_key) =
        registrar.arg(damage_buffer, "distance", FloatArgument::bounded(0.0, f32::MAX));
    registrar.exec(damage_buffer_node, move |ctx| {
        let distance = *ctx.get(damage_buffer_key);
        set_damage_buffer(ctx, distance)
    });

    // `/worldborder warning distance <distance>` and `.../time <time>`.
    let warning = registrar.literal(worldborder, "warning");
    let warning_distance = registrar.literal(warning, "distance");
    let (warning_distance_node, warning_distance_key) =
        registrar.arg(warning_distance, "distance", IntegerArgument::bounded(0, i32::MAX));
    registrar.exec(warning_distance_node, move |ctx| {
        let distance = *ctx.get(warning_distance_key);
        set_warning_distance(ctx, distance)
    });
    let warning_time = registrar.literal(warning, "time");
    let (warning_time_node, warning_time_key) = registrar.arg(warning_time, "time", TimeArg { min: 0 });
    registrar.exec(warning_time_node, move |ctx| {
        let ticks = *ctx.get(warning_time_key);
        set_warning_time(ctx, ticks)
    });
}

fn border<'a>(ctx: &Ctx<'a>) -> Result<&'a BorderFeed, String> {
    ctx.world.border.ok_or_else(|| NO_BORDER.to_string())
}

/// `WorldBorderCommand.setSize` (`:189-211`) — the size half every `set`/`add`
/// call ends in. `ticks <= 0` is vanilla's plain `setSize`; `ticks > 0` is
/// `lerpSizeBetween`, both gated by the same three refusals (`current ==
/// distance`, `distance < 1.0`, `distance > MAX_SIZE`) checked in that order.
fn set_size(ctx: &mut Ctx<'_>, distance: f64, ticks: i64) -> CommandResult {
    let feed = border(ctx)?;
    let current = feed.get().size();
    if current == distance {
        return Err("Nothing changed. The world border is already that size".to_string());
    }
    if distance < 1.0 {
        return Err("The world border cannot be that small".to_string());
    }
    if distance > MAX_SIZE {
        return Err(format!(
            "The world border cannot be bigger than {MAX_SIZE:.0} blocks wide"
        ));
    }
    if ticks > 0 {
        let game_time = ctx.world.state.tick_time().game_time;
        feed.with(|b| b.lerp_size_between(current, distance, ticks, game_time));
        let seconds = ticks as f64 / 20.0;
        if distance > current {
            ctx.send_success(format!(
                "Growing world border to {distance:.1} blocks wide over {seconds:.1} seconds"
            ));
        } else {
            ctx.send_success(format!(
                "Shrinking world border to {distance:.1} blocks wide over {seconds:.1} seconds"
            ));
        }
    } else {
        feed.with(|b| b.set_size(distance));
        ctx.send_success(format!("Set the world border to {distance:.1} blocks wide"));
    }
    Ok((distance - current) as i32)
}

/// `/worldborder add`: reads the current size once, then defers to
/// [`set_size`] with `current + delta` — `WorldBorderCommand`'s own `add`
/// arm does the identical read-then-delegate.
fn add_size(ctx: &mut Ctx<'_>, delta: f64, ticks: i64) -> CommandResult {
    let feed = border(ctx)?;
    let current = feed.get().size();
    set_size(ctx, current + delta, ticks)
}

/// `WorldBorderCommand.setCenter` (`:213-227`).
fn set_center(ctx: &mut Ctx<'_>, x: f64, z: f64) -> CommandResult {
    let feed = border(ctx)?;
    let current = feed.get();
    if current.center_x() == x && current.center_z() == z {
        return Err("Nothing changed. That's where the world border center already is".to_string());
    }
    if x.abs() > MAX_CENTER_COORDINATE || z.abs() > MAX_CENTER_COORDINATE {
        return Err("The world border center is too far out".to_string());
    }
    feed.with(|b| b.set_center(x, z));
    ctx.send_success(format!("Set the world border center to {x:.2}, {z:.2}"));
    Ok(0)
}

/// `WorldBorderCommand.setDamageAmount` (`:165-172`).
fn set_damage_amount(ctx: &mut Ctx<'_>, damage_per_block: f32) -> CommandResult {
    let feed = border(ctx)?;
    if feed.get().damage_per_block() == f64::from(damage_per_block) {
        return Err("Nothing changed. The world border damage is already that amount".to_string());
    }
    feed.with(|b| b.set_damage_per_block(f64::from(damage_per_block)));
    ctx.send_success(format!(
        "Set the world border damage to {damage_per_block:.2} per block each second"
    ));
    Ok(damage_per_block as i32)
}

/// `WorldBorderCommand.setDamageBuffer` (`:157-164`).
fn set_damage_buffer(ctx: &mut Ctx<'_>, distance: f32) -> CommandResult {
    let feed = border(ctx)?;
    if feed.get().safe_zone() == f64::from(distance) {
        return Err("Nothing changed. The world border damage buffer is already that distance".to_string());
    }
    feed.with(|b| b.set_safe_zone(f64::from(distance)));
    ctx.send_success(format!("Set the world border damage buffer to {distance:.2} blocks"));
    Ok(distance as i32)
}

/// `WorldBorderCommand.setWarningDistance` (`:181-188`).
fn set_warning_distance(ctx: &mut Ctx<'_>, distance: i32) -> CommandResult {
    let feed = border(ctx)?;
    if feed.get().warning_blocks() == distance {
        return Err("Nothing changed. The world border warning is already that distance".to_string());
    }
    feed.with(|b| b.set_warning_blocks(distance));
    ctx.send_success(format!("Set the world border warning distance to {distance} blocks"));
    Ok(distance)
}

/// `WorldBorderCommand.setWarningTime` (`:173-180`).
fn set_warning_time(ctx: &mut Ctx<'_>, ticks: i32) -> CommandResult {
    let feed = border(ctx)?;
    if feed.get().warning_time() == ticks {
        return Err("Nothing changed. The world border warning is already that amount of time".to_string());
    }
    feed.with(|b| b.set_warning_time(ticks));
    let seconds = f64::from(ticks) / 20.0;
    ctx.send_success(format!("Set the world border warning time to {seconds:.2} seconds"));
    Ok(ticks)
}

/// `WorldBorderCommand.getSize` (`:154-157`).
fn get_size(ctx: &mut Ctx<'_>) -> CommandResult {
    let feed = border(ctx)?;
    let size = feed.get().size();
    ctx.send_success(format!("The world border is currently {size:.0} blocks wide"));
    Ok((size + 0.5).floor() as i32)
}
