//! `/ride <target> mount <vehicle>` and `/ride <target> dismount`.
//!
//! # What it is
//!
//! Seats an entity on a vehicle, or lifts it off, through the one rider relation
//! in `crate::mobs`' `riders` module (`MobSim::mount_forced`, `MobSim::dismount`).
//!
//! # How it works
//!
//! The refusals are the reference's, in its order: already riding, players as
//! vehicles, the vehicle being the target or one of its passengers, then the
//! boarding refusal ("couldn't start riding"). The mount is forced, as in the
//! reference: it skips capacity, sneaking and capsizing, so a vehicle can be
//! over-filled and can carry a player and mobs together.
//!
//! Clients learn the new passenger list from the entity streamer, which diffs
//! `EntitySnapshot::passengers` per viewer; the command sends no packet.
//!
//! # Gaps
//!
//! Dimension is not compared; the server hosts one.

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

    if let Some(current) = mobs.with(|sim| sim.vehicle_of(target.entity_id())) {
        return Err(format!("{target_name} is already riding {}", vehicle_name(ctx, current)));
    }
    let CommandTarget::World(vehicle_entity) = &vehicle else {
        return Err("Players can't be ridden".to_owned());
    };
    if mobs.with(|sim| sim.self_and_passengers(target.entity_id()).contains(&vehicle.entity_id())) {
        return Err("Can't mount entity on itself or any of its passengers".to_owned());
    }
    let rider = match &target {
        CommandTarget::Player(player) => crate::mobs::Rider::Player(player.entity_id),
        CommandTarget::World(entity) => crate::mobs::Rider::Mob(entity.uuid),
    };
    match mobs.with(|sim| sim.mount_forced(vehicle_entity.uuid, rider)) {
        crate::mobs::MountOutcome::Mounted { .. } => {}
        _ => return Err(format!("{target_name} couldn't start riding {vehicle_label}")),
    }
    ctx.send_success(format!("{target_name} started riding {vehicle_label}"));
    Ok(1)
}

fn dismount_command(
    ctx: &mut Ctx<'_>,
    target_key: ArgKey<lodestone_command_mc::EntitySelector>,
) -> CommandResult {
    let target_selector = ctx.get(target_key).clone();
    let target = resolve_single(ctx, &target_selector)?;
    let name = target.display_name();
    let Some(mobs) = ctx.world.mobs else {
        return Err("This command needs a live connection and is not available here".to_owned());
    };
    let Some(vehicle) = mobs.with(|sim| sim.vehicle_of(target.entity_id())) else {
        return Err(format!("{name} is not riding any vehicle"));
    };
    let label = vehicle_name(ctx, vehicle);
    mobs.with(|sim| sim.dismount(target.entity_id()));
    ctx.send_success(format!("{name} stopped riding {label}"));
    Ok(1)
}
