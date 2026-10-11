//! `/ride <target> mount <vehicle>` and `/ride <target> dismount`.
//!
//! # What it is
//!
//! Seats a player on a vehicle, or lifts them off, through the passenger
//! mechanics boarding by hand already uses (`MobSim::mount_player` and
//! `MobSim::dismount_player`, which pick the boat, minecart, cushion or mob
//! rule by the vehicle's kind).
//!
//! # How it works
//!
//! The refusals are the reference's, in its order: already riding, players as
//! vehicles, mounting on itself, then the vehicle's own refusal (occupied,
//! capsized, a cart that is not rideable). The sim records the rider at once; the
//! rider's connection sends the vehicle's passenger list when it applies
//! `Effect::Mount`, and an ejection the connection already drains sends the empty
//! list on dismount.
//!
//! # Gaps
//!
//! Only players ride: the sim keeps one player-id rider slot per vehicle, so a
//! non-player target is refused with the generic "couldn't start riding"
//! message, and cannot be reported as riding anything. Dimension is not
//! compared; the server hosts the sim's single dimension.

use lodestone_command_mc::EntityArg;

use super::entity_data::resolve_single;
use super::registrar::{ArgKey, Ctx, Registrar};
use super::source::CommandTarget;
use super::CommandResult;

/// The game-masters permission level.
const RIDE_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let ride = registrar.literal(root, "ride");
    registrar.require_level(ride, RIDE_LEVEL);
    let (target_node, target_key) = registrar.arg(ride, "target", EntityArg::entity());

    let mount = registrar.literal(target_node, "mount");
    let (vehicle_node, vehicle_key) = registrar.arg(mount, "vehicle", EntityArg::entity());
    registrar.exec(vehicle_node, move |ctx| mount_command(ctx, target_key, vehicle_key));

    let dismount = registrar.literal(target_node, "dismount");
    registrar.exec(dismount, move |ctx| dismount_command(ctx, target_key));
}

/// The name feedback uses for the entity with network id `entity_id`.
fn vehicle_name(ctx: &Ctx<'_>, entity_id: i32) -> String {
    ctx.world
        .mobs
        .and_then(|mobs| {
            mobs.with(|sim| sim.command_entities().into_iter().find(|e| e.entity_id == entity_id))
        })
        .map_or_else(|| "entity".to_owned(), |e| if e.name.is_empty() { e.entity_type.to_string() } else { e.name })
}

fn mount_command(
    ctx: &mut Ctx<'_>,
    target_key: ArgKey<lodestone_command_mc::EntitySelector>,
    vehicle_key: ArgKey<lodestone_command_mc::EntitySelector>,
) -> CommandResult {
    let target_selector = ctx.get(target_key).clone();
    let vehicle_selector = ctx.get(vehicle_key).clone();
    let target = resolve_single(ctx, &target_selector)?;
    let vehicle = resolve_single(ctx, &vehicle_selector)?;
    let Some(mobs) = ctx.world.mobs else {
        return Err("This command needs a live connection and is not available here".to_owned());
    };
    let (target_name, vehicle_label) = (target.display_name(), vehicle.display_name());

    if let CommandTarget::Player(player) = &target
        && let Some((current, _)) = mobs.with(|sim| sim.ridden_by(player.entity_id))
    {
        return Err(format!("{target_name} is already riding {}", vehicle_name(ctx, current)));
    }
    if matches!(vehicle, CommandTarget::Player(_)) {
        return Err("Players can't be ridden".to_owned());
    }
    if target.entity_id() == vehicle.entity_id() {
        return Err("Can't mount entity on itself or any of its passengers".to_owned());
    }
    let CommandTarget::Player(player) = &target else {
        return Err(format!("{target_name} couldn't start riding {vehicle_label}"));
    };
    let CommandTarget::World(vehicle_entity) = &vehicle else { unreachable!("players were refused above") };
    match mobs.with(|sim| sim.mount_player(vehicle_entity.uuid, player.entity_id)) {
        crate::mobs::MountOutcome::Mounted { vehicle: id } => {
            ctx.effect(player.uuid, crate::commands::Effect::Mount { vehicle: id });
            ctx.send_success(format!("{target_name} started riding {vehicle_label}"));
            Ok(1)
        }
        _ => Err(format!("{target_name} couldn't start riding {vehicle_label}")),
    }
}

fn dismount_command(
    ctx: &mut Ctx<'_>,
    target_key: ArgKey<lodestone_command_mc::EntitySelector>,
) -> CommandResult {
    let target_selector = ctx.get(target_key).clone();
    let target = resolve_single(ctx, &target_selector)?;
    let name = target.display_name();
    let not_riding = || format!("{name} is not riding any vehicle");
    let CommandTarget::Player(player) = &target else { return Err(not_riding()) };
    let Some(mobs) = ctx.world.mobs else {
        return Err("This command needs a live connection and is not available here".to_owned());
    };
    let Some((vehicle, _)) = mobs.with(|sim| sim.ridden_by(player.entity_id)) else { return Err(not_riding()) };
    let label = vehicle_name(ctx, vehicle);
    mobs.with(|sim| sim.dismount_player(player.entity_id));
    ctx.send_success(format!("{name} stopped riding {label}"));
    Ok(1)
}
