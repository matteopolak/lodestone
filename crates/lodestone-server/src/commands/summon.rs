//! `/summon`, from `SummonCommand.java` — the mechanism this command needed
//! was a synchronous single-mob spawn entry point reachable from an
//! executor, and it already existed: [`crate::mobs::MobHandle::with`] plus
//! [`crate::mobs::MobSim::spawn_species`] are both `pub` on the mob
//! simulation's own handle, so this is an API-shape gap closed by
//! [`super::registrar::CommandWorld`] carrying a handle, not a new mob-sim
//! capability.
//!
//! # Why a freshly summoned mob is not an island
//!
//! `MobHandle` is the **same shared handle** the world tick loop's mob
//! population lives behind — `IntegratedServer::open_in_memory_with_mobs`
//! clones one handle to both the connection's `dispatch_play_packet` (which
//! this command reaches through) and `crate::mobs::run_mob_tick_loop`, which
//! republishes the sim's snapshots into `LiveMobSource` on its own cadence.
//! A mob this command pushes into the sim is therefore picked up by the very
//! next tick-loop publish with no second wire to build.
//!
//! # No NBT, and no position-validity check
//!
//! Vanilla's `<nbt>` argument needs a textual SNBT parser, which does not
//! exist anywhere in this tree yet (see `crate::commands`' module doc's
//! "Known gaps" for the same reason `/give`'s component patch is refused).
//! `Level.isInSpawnableBounds` (a build-height range check) is also not
//! reproduced — this crate has no command-reachable build-height constant
//! plumbed here, and an out-of-range spawn simply produces a mob at an
//! out-of-range position rather than a refusal, a smaller gap than a command
//! that silently fails on a well-formed position.

use lodestone_command_mc::{Coordinates, EntityTypeArg, EntityTypeInput, Vec3Arg};
use lodestone_model::{Difficulty, Vec3};

use super::registrar::{Ctx, Registrar};
use super::CommandResult;

/// `Commands.LEVEL_GAMEMASTERS`.
const SUMMON_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let summon_node = registrar.literal(root, "summon");
    registrar.require_level(summon_node, SUMMON_LEVEL);

    let (entity_node, entity_key) = registrar.arg(summon_node, "entity", EntityTypeArg);
    // Bare `/summon <entity>` — the caller's own position, `getPosition()`.
    registrar.exec(entity_node, move |ctx| {
        let entity = ctx.get(entity_key).clone();
        let pos = ctx.source.position;
        summon(ctx, &entity, pos)
    });

    let (pos_node, pos_key) = registrar.arg(entity_node, "pos", Vec3Arg::new());
    registrar.exec(pos_node, move |ctx| {
        let entity = ctx.get(entity_key).clone();
        let coords = *ctx.get(pos_key);
        let pos = resolve_pos(ctx, coords);
        summon(ctx, &entity, pos)
    });
}

fn resolve_pos(ctx: &Ctx<'_>, coords: Coordinates) -> Vec3 {
    let origin = (ctx.source.position.x, ctx.source.position.y, ctx.source.position.z);
    let rotation = (ctx.source.rotation.yaw, ctx.source.rotation.pitch);
    let (x, y, z) = coords.resolve(origin, rotation);
    Vec3::new(x, y, z)
}

/// `SummonCommand.createEntity` + `spawnEntity`, minus NBT and the
/// build-height bounds check — see the module doc for both.
fn summon(ctx: &mut Ctx<'_>, entity: &EntityTypeInput, pos: Vec3) -> CommandResult {
    let (difficulty, _) = ctx.world.state.difficulty();
    if difficulty == Difficulty::Peaceful
        && !crate::mob_spawn::allowed_in_peaceful(entity.entity_type.path())
    {
        return Err("Cannot summon that entity because the difficulty is peaceful".to_string());
    }

    let Some(mobs) = ctx.world.mobs else {
        return Err("This command needs a live connection and is not available here".to_string());
    };

    let entity_type = entity.entity_type.clone();
    mobs.with(|sim| {
        // `finalize_spawn` (vanilla's `Mob::finalizeSpawn`, attribute/equipment
        // randomisation on natural/command spawns) has no port here yet — see
        // `crate::mobs`' own scope notes on `spawn_species`. Marked persistent
        // so `EntitySpawnReason::COMMAND`'s exemption from natural despawn is at
        // least honoured, the one `finalizeSpawn`-adjacent property this crate
        // can cheaply keep.
        sim.spawn_species(entity_type, pos).set_persistent(true);
    });

    ctx.send_success(format!("Summoned {}", entity.entity_type));
    Ok(1)
}
