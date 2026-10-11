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
//! # NBT is read for item frames only, and there is no position-validity check
//!
//! The `<nbt>` argument is parsed for every entity but applied only to item
//! and glow item frames, whose record (`Facing`, `Item`, `ItemRotation`,
//! `ItemDropChance`, `Invisible`, `Fixed`, `Invulnerable`, `block_pos`) is read
//! exactly as a saved one is; any other type refuses it rather than dropping it
//! silently. The real build-height range check is not reproduced — this crate
//! has no command-reachable build-height constant plumbed here, and an
//! out-of-range spawn simply produces a mob at an out-of-range position
//! rather than a refusal, a smaller gap than a command that silently fails
//! on a well-formed position.

use lodestone_command_mc::{Coordinates, EntityTypeArg, EntityTypeInput, NbtCompoundArg, SnbtValue, Vec3Arg};
use lodestone_core::{Nbt, NbtTag};
use lodestone_model::{Difficulty, Rotation, Vec3};
use uuid::Uuid;

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
    // Bare `/summon <entity>` — the caller's own position, get position.
    registrar.exec(entity_node, move |ctx| {
        let entity = ctx.get(entity_key).clone();
        let pos = ctx.source.position;
        summon(ctx, &entity, pos, None)
    });

    let (pos_node, pos_key) = registrar.arg(entity_node, "pos", Vec3Arg::new());
    registrar.exec(pos_node, move |ctx| {
        let entity = ctx.get(entity_key).clone();
        let coords = *ctx.get(pos_key);
        let pos = resolve_pos(ctx, coords);
        summon(ctx, &entity, pos, None)
    });

    let (nbt_node, nbt_key) = registrar.arg(pos_node, "nbt", NbtCompoundArg);
    registrar.exec(nbt_node, move |ctx| {
        let entity = ctx.get(entity_key).clone();
        let coords = *ctx.get(pos_key);
        let nbt = ctx.get(nbt_key).clone();
        let pos = resolve_pos(ctx, coords);
        summon(ctx, &entity, pos, Some(nbt))
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
fn summon(
    ctx: &mut Ctx<'_>,
    entity: &EntityTypeInput,
    pos: Vec3,
    nbt: Option<Vec<(String, SnbtValue)>>,
) -> CommandResult {
    spawn_entity(ctx, entity, pos, nbt.as_deref())?;
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
    nbt: Option<&[(String, SnbtValue)]>,
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
    if matches!(entity_type.path(), "item_frame" | "glow_item_frame") {
        let record = frame_record(&entity_type, pos, nbt.unwrap_or_default());
        let (uuid, entity_id) = mobs.with(|sim| sim.summon_item_frame(&record));
        return Ok(SourceEntity { uuid, entity_id, username: entity_type.to_string() });
    }
    if nbt.is_some() {
        return Err("Only item frames take entity data here".to_string());
    }
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

/// The saved-entity record `/summon` hands the mob simulation for a frame: the
/// command position stands in for the box centre, so the frame hangs in the
/// cell containing it unless the data names `block_pos`.
fn frame_record(entity_type: &lodestone_model::ResourceKey, pos: Vec3, nbt: &[(String, SnbtValue)]) -> crate::entity_record::SavedEntity {
    let extra = nbt
        .iter()
        .filter(|(name, _)| !matches!(name.as_str(), "id" | "UUID" | "Pos" | "Motion" | "Rotation"))
        .map(|(name, value)| (name.clone(), snbt_to_nbt(value)))
        .collect();
    crate::entity_record::SavedEntity {
        id: entity_type.clone(),
        uuid: Uuid::new_v4(),
        pos,
        motion: Vec3::new(0.0, 0.0, 0.0),
        rotation: Rotation::new(0.0, 0.0),
        health: None,
        item: None,
        age: None,
        pickup_delay: None,
        extra,
    }
}

fn snbt_to_nbt(value: &SnbtValue) -> Nbt {
    match value {
        SnbtValue::Byte(v) => Nbt::Byte(*v),
        SnbtValue::Short(v) => Nbt::Short(*v),
        SnbtValue::Int(v) => Nbt::Int(*v),
        SnbtValue::Long(v) => Nbt::Long(*v),
        SnbtValue::Float(v) => Nbt::Float(*v),
        SnbtValue::Double(v) => Nbt::Double(*v),
        SnbtValue::String(v) => Nbt::String(v.clone()),
        SnbtValue::ByteArray(v) => Nbt::ByteArray(v.clone()),
        SnbtValue::IntArray(v) => Nbt::IntArray(v.clone()),
        SnbtValue::LongArray(v) => Nbt::LongArray(v.clone()),
        SnbtValue::Compound(entries) => {
            Nbt::Compound(entries.iter().map(|(name, value)| (name.clone(), snbt_to_nbt(value))).collect())
        }
        SnbtValue::List(items) => {
            let elements: Vec<Nbt> = items.iter().map(snbt_to_nbt).collect();
            let element_type = match items.first() {
                None => NbtTag::End,
                Some(SnbtValue::Byte(_)) => NbtTag::Byte,
                Some(SnbtValue::Short(_)) => NbtTag::Short,
                Some(SnbtValue::Int(_)) => NbtTag::Int,
                Some(SnbtValue::Long(_)) => NbtTag::Long,
                Some(SnbtValue::Float(_)) => NbtTag::Float,
                Some(SnbtValue::Double(_)) => NbtTag::Double,
                Some(SnbtValue::String(_)) => NbtTag::String,
                Some(SnbtValue::ByteArray(_)) => NbtTag::ByteArray,
                Some(SnbtValue::IntArray(_)) => NbtTag::IntArray,
                Some(SnbtValue::LongArray(_)) => NbtTag::LongArray,
                Some(SnbtValue::List(_)) => NbtTag::List,
                Some(SnbtValue::Compound(_)) => NbtTag::Compound,
            };
            Nbt::List { element_type, elements }
        }
    }
}
