//! `/ride <target> mount <vehicle>` and `/ride <target> dismount`.
//!
//! # What it is
//!
//! Seats an entity on a vehicle, or lifts it off. A player boards through the
//! passenger mechanics boarding by hand uses (`MobSim::mount_player`, which
//! picks the boat, minecart, cushion or mob rule by the vehicle's kind); a mob
//! boards through `MobSim::mount_mob_rider`, the link table in
//! `crate::mobs`' `mob_riders` module.
//!
//! # How it works
//!
//! The refusals are the reference's, in its order: already riding, players as
//! vehicles, the vehicle being the target or one of its passengers, then the
//! boarding refusal ("couldn't start riding"). A player's vehicle may also
//! refuse for its own rule (occupied, capsized, a cart that is not rideable);
//! a mob rider ignores occupancy, as the forced reference mount does.
//!
//! A player's rider's connection sends the vehicle's passenger list when it
//! applies `Effect::Mount`, and an ejection the connection already drains
//! sends the empty list on dismount. A mob rider reaches clients through
//! `EntitySnapshot::passengers`, which the entity streamer diffs per viewer.
//!
//! # Gaps
//!
//! Only players and mobs ride. A vehicle never carries a player and a mob
//! together (the two passenger lists travel by different routes), mob riders
//! are not saved, and dimension is not compared; the server hosts one.

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

    let current = match &target {
        CommandTarget::Player(player) => mobs.with(|sim| sim.ridden_by(player.entity_id)).map(|(id, _)| id),
        CommandTarget::World(entity) => mobs.with(|sim| sim.mob_rider_vehicle(entity.entity_id)),
    };
    if let Some(current) = current {
        return Err(format!("{target_name} is already riding {}", vehicle_name(ctx, current)));
    }
    if matches!(vehicle, CommandTarget::Player(_)) {
        return Err("Players can't be ridden".to_owned());
    }
    if mobs.with(|sim| sim.self_and_passengers(target.entity_id()).contains(&vehicle.entity_id())) {
        return Err("Can't mount entity on itself or any of its passengers".to_owned());
    }
    let refused = || format!("{target_name} couldn't start riding {vehicle_label}");
    let CommandTarget::World(vehicle_entity) = &vehicle else { unreachable!("players were refused above") };
    let boarded = match &target {
        CommandTarget::Player(player) => {
            if !mobs.with(|sim| sim.mob_riders_of(vehicle.entity_id()).is_empty()) {
                return Err(refused());
            }
            match mobs.with(|sim| sim.mount_player(vehicle_entity.uuid, player.entity_id)) {
                crate::mobs::MountOutcome::Mounted { vehicle: id } => {
                    ctx.effect(player.uuid, crate::commands::Effect::Mount { vehicle: id });
                    true
                }
                _ => false,
            }
        }
        CommandTarget::World(rider) => matches!(
            mobs.with(|sim| sim.mount_mob_rider(vehicle_entity.uuid, rider.uuid)),
            crate::mobs::MobMountOutcome::Mounted { .. }
        ),
    };
    if !boarded {
        return Err(refused());
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
    let not_riding = || format!("{name} is not riding any vehicle");
    let Some(mobs) = ctx.world.mobs else {
        return Err("This command needs a live connection and is not available here".to_owned());
    };
    let vehicle = match &target {
        CommandTarget::Player(player) => mobs.with(|sim| sim.ridden_by(player.entity_id)).map(|(id, _)| id),
        CommandTarget::World(rider) => mobs.with(|sim| sim.mob_rider_vehicle(rider.entity_id)),
    };
    let Some(vehicle) = vehicle else { return Err(not_riding()) };
    let label = vehicle_name(ctx, vehicle);
    match &target {
        CommandTarget::Player(player) => {
            mobs.with(|sim| sim.dismount_player(player.entity_id));
        }
        CommandTarget::World(rider) => {
            mobs.with(|sim| sim.dismount_mob_rider(rider.entity_id));
        }
    }
    ctx.send_success(format!("{name} stopped riding {label}"));
    Ok(1)
}
