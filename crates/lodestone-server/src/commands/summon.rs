//! `/summon` — the mechanism this command needed was a synchronous
//! single-mob spawn entry point reachable from an
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
//! The real `<nbt>` argument needs a textual SNBT parser, which does not
//! exist anywhere in this tree yet (see `crate::commands`' module doc's
//! "Known gaps" for the same reason `/give`'s component patch is refused).
//! The real build-height range check is also not reproduced — this crate
//! has no command-reachable build-height constant plumbed here, and an
//! out-of-range spawn simply produces a mob at an out-of-range position
//! rather than a refusal, a smaller gap than a command that silently fails
//! on a well-formed position.

use lodestone_command_mc::{Coordinates, EntityTypeArg, EntityTypeInput, Vec3Arg};
use lodestone_model::{Difficulty, Vec3};

use super::registrar::{Ctx, Registrar};
use super::source::SourceEntity;
use super::CommandResult;

/// The game-masters permission level.
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

/// The real create-and-spawn-entity rule, minus NBT and the build-height
/// bounds check — see the module doc for both.
fn summon(ctx: &mut Ctx<'_>, entity: &EntityTypeInput, pos: Vec3) -> CommandResult {
    spawn_entity(ctx, entity, pos)?;
    ctx.send_success(format!("Summoned {}", entity.entity_type));
    Ok(1)
}

/// The mechanism `summon` needed and `execute summon <entity>` reuses
/// verbatim (the real `execute summon` modifier calls the identical
/// create-entity rule): spawn one mob into the live sim and hand
/// back the [`SourceEntity`] a caller can fold into a [`CommandSource`
/// (super::source::CommandSource)`]. Split out of [`summon`] rather than
/// having `execute`'s modifier re-derive the difficulty/peaceful/no-`mobs`
/// checks a second time.
///
/// # Errors
///
/// The difficulty-gated peaceful refusal, or "no live connection" when
/// [`super::registrar::CommandWorld::mobs`] is `None` — the same two cases
/// [`summon`]'s own executor already reports.
pub(super) fn spawn_entity(
    ctx: &Ctx<'_>,
    entity: &EntityTypeInput,
    pos: Vec3,
) -> Result<SourceEntity, String> {
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
    let (uuid, entity_id) = mobs.with(|sim| {
        // Spawn-time attribute/equipment randomisation (the finalize-spawn
        // step run on natural/command spawns) has no port here yet — see
        // `crate::mobs`' own scope notes on `spawn_species`. Marked persistent
        // so the command-spawn exemption from natural despawn is at least
        // honoured, the one finalize-spawn-adjacent property this crate can
        // cheaply keep.
        let mob = sim.spawn_species(entity_type, pos);
        mob.set_persistent(true);
        (mob.uuid(), mob.id())
    });

    Ok(SourceEntity {
        uuid,
        entity_id,
        // A summoned mob has no username — the real `@s` feedback for a
        // non-player source falls back to the entity's own display name (its
        // translated type name absent an NBT `CustomName`). This crate
        // carries no localisation table, so the canonical id is the closest
        // stand-in; it is never compared against a real login name anywhere
        // this crate resolves selectors.
        username: entity.entity_type.to_string(),
    })
}
