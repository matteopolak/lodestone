//! `/damage <target> <amount> [<damageType> [at <location> | by <entity> [from <cause>]]]`.
//!
//! # What it is
//!
//! A hit of `amount` delivered through the target's own damage pipeline with the
//! flags of a real `minecraft:damage_type` (default `generic`): the
//! invulnerability-frame gate, then armour, resistance and enchantment
//! reduction and absorption, each skipped as the type's tags say.
//!
//! # How it works
//!
//! - A world entity is hit by `MobSim::damage_entity`: mobs through
//!   `SimMob::try_apply_damage`, or `MobSim::attack` when the source has a
//!   position, which also knocks the mob back away from it; the ender dragon and
//!   wither through their own damage entry points.
//! - A player gets `Effect::Damage`, applied by their connection against its live
//!   vitals and inventory. A creative or spectator player is refused here unless the
//!   type bypasses invulnerability.
//! - `at` gives the source position, `by` the position of the named entity; `from`
//!   (the causing entity) changes nothing the sim models.
//!
//! # How to change it
//!
//! Damage type behaviour belongs in the `lodestone_data::damage_types` tag
//! queries, not here. Death messages for a `/damage` kill of a player use the
//! generic message, not the type's.

use lodestone_command::FloatArgument;
use lodestone_command_mc::{DamageTypeArg, EntityArg, Vec3Arg};
use lodestone_data::damage_types::{DamageType, DamageTypeTag};
use lodestone_entity::DamageFlags;
use lodestone_model::{GameMode, Vec3};

use super::entity_data::resolve_single;
use super::registrar::{ArgKey, Ctx, Registrar};
use super::source::{CommandTarget, SelectorSubject};
use super::CommandResult;

/// The game-masters permission level.
const DAMAGE_LEVEL: u8 = 2;

const INVULNERABLE: &str = "Target is invulnerable to the given damage type";

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let damage = registrar.literal(root, "damage");
    registrar.require_level(damage, DAMAGE_LEVEL);
    let (target_node, target_key) = registrar.arg(damage, "target", EntityArg::entity());
    let (amount_node, amount_key) = registrar.arg(target_node, "amount", FloatArgument::bounded(0.0, f32::MAX));
    registrar.exec(amount_node, move |ctx| {
        run(ctx, target_key, amount_key, DamageType::from_name("generic").expect("generic is registered"), Source::None)
    });

    let (type_node, type_key) = registrar.arg(amount_node, "damageType", DamageTypeArg);
    registrar.exec(type_node, move |ctx| run(ctx, target_key, amount_key, *ctx.get(type_key), Source::None));

    let at = registrar.literal(type_node, "at");
    let (loc_node, loc_key) = registrar.arg(at, "location", Vec3Arg::new());
    registrar.exec(loc_node, move |ctx| {
        let coords = *ctx.get(loc_key);
        let origin = (ctx.source.position.x, ctx.source.position.y, ctx.source.position.z);
        let rotation = (ctx.source.rotation.yaw, ctx.source.rotation.pitch);
        let (x, y, z) = coords.resolve(origin, rotation);
        run(ctx, target_key, amount_key, *ctx.get(type_key), Source::At(Vec3::new(x, y, z)))
    });

    let by = registrar.literal(type_node, "by");
    let (by_node, by_key) = registrar.arg(by, "entity", EntityArg::entity());
    registrar.exec(by_node, move |ctx| run(ctx, target_key, amount_key, *ctx.get(type_key), Source::By(by_key)));
    let from = registrar.literal(by_node, "from");
    let (cause_node, _cause_key) = registrar.arg(from, "cause", EntityArg::entity());
    registrar.exec(cause_node, move |ctx| run(ctx, target_key, amount_key, *ctx.get(type_key), Source::By(by_key)));
}

/// Where the damage comes from.
#[derive(Clone, Copy)]
enum Source {
    None,
    At(Vec3),
    By(ArgKey<lodestone_command_mc::EntitySelector>),
}

fn run(
    ctx: &mut Ctx<'_>,
    target_key: ArgKey<lodestone_command_mc::EntitySelector>,
    amount_key: ArgKey<f32>,
    ty: DamageType,
    source: Source,
) -> CommandResult {
    let target_selector = ctx.get(target_key).clone();
    let target = resolve_single(ctx, &target_selector)?;
    let amount = *ctx.get(amount_key);
    let from = match source {
        Source::None => None,
        Source::At(at) => Some(at),
        Source::By(key) => {
            let selector = ctx.get(key).clone();
            Some(resolve_single(ctx, &selector)?.position())
        }
    }
    .filter(|_| !ty.is_in(DamageTypeTag::NoKnockback));
    let bypasses_invulnerability = ty.is_in(DamageTypeTag::BypassesInvulnerability);

    let landed = match &target {
        CommandTarget::Player(player) => {
            let invulnerable = matches!(player.game_mode, GameMode::Creative | GameMode::Spectator);
            if invulnerable && !bypasses_invulnerability {
                false
            } else {
                ctx.effect(
                    player.uuid,
                    crate::commands::Effect::Damage { amount, damage_type: ty.name().to_owned(), from },
                );
                true
            }
        }
        CommandTarget::World(entity) => {
            let Some(mobs) = ctx.world.mobs else {
                return Err("This command needs a live connection and is not available here".to_owned());
            };
            let flags = DamageFlags::for_damage_type(ty);
            let outcome = mobs.with(|sim| sim.damage_entity(entity.uuid, amount, flags, from, bypasses_invulnerability));
            matches!(outcome, crate::mobs::DamageOutcome::Landed { .. })
        }
    };
    if !landed {
        return Err(INVULNERABLE.to_owned());
    }
    ctx.send_success(format!("Applied {amount:?} damage to {}", target.display_name()));
    Ok(1)
}
