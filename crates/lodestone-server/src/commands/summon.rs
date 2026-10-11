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
//! # Entity data
//!
//! The `<nbt>` argument becomes a [`crate::entity_record::SavedEntity`] (`id`
//! and `Pos` forced, a fresh `UUID`) and goes through
//! [`crate::mobs::MobSim::summon_saved`], the world-load restore path. Every
//! field the save path owns therefore applies to a summon: `CustomName`,
//! `CustomNameVisible`, `Tags`, `NoAI`, `NoGravity`, `Silent`, `Invulnerable`,
//! `PersistenceRequired`, `Health`, `Motion`, `Rotation`, `active_effects`,
//! `equipment`, an item entity's `Item`, a frame's record. Unknown keys are
//! ignored. The build-height range check is not reproduced.

use lodestone_command_mc::{Coordinates, EntityTypeArg, EntityTypeInput, NbtCompoundArg, SnbtValue, Vec3Arg};
use lodestone_core::{Nbt, NbtTag};
use lodestone_model::{Difficulty, Vec3};
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

    let record = summon_record(&entity.entity_type, pos, nbt.unwrap_or_default());
    let Some((uuid, entity_id)) = mobs.with(|sim| sim.summon_saved(&record)) else {
        return Err("Unable to summon entity".to_string());
    };

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

/// The saved-entity record `/summon` hands the mob simulation: the command's
/// data with `id` and `Pos` forced and a fresh `UUID`, read by the same
/// [`crate::entity_record::SavedEntity::from_nbt`] and restored by the same
/// path as a saved world, so a summoned entity and a loaded one cannot differ.
/// Keys no reader owns are ignored. A frame with no `block_pos` hangs in the
/// cell containing the command position.
fn summon_record(entity_type: &lodestone_model::ResourceKey, pos: Vec3, nbt: &[(String, SnbtValue)]) -> crate::entity_record::SavedEntity {
    let double = |v: f64| Nbt::Double(v);
    let mut fields: Vec<(String, Nbt)> = nbt
        .iter()
        .filter(|(name, _)| !matches!(name.as_str(), "id" | "UUID" | "Pos"))
        .map(|(name, value)| (name.clone(), super::entity_data::snbt_to_nbt(value)))
        .collect();
    fields.push(("id".to_owned(), Nbt::String(entity_type.to_string())));
    fields.push((
        "Pos".to_owned(),
        Nbt::List { element_type: NbtTag::Double, elements: vec![double(pos.x), double(pos.y), double(pos.z)] },
    ));
    let mut record = crate::entity_record::SavedEntity::from_nbt(&Nbt::Compound(fields))
        .expect("id and Pos are always present");
    record.uuid = Uuid::new_v4();
    record
}
