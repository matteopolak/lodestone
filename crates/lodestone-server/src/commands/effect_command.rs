//! `/effect` — the producer that makes [`crate::mob_effects`] reachable.
//!
//! # The tree, as the real command declares it
//!
//! ```text
//! literal("effect").requires(game-masters level)
//!   ├─ literal("clear")                                     [executable]
//!   │    └─ argument("targets", any entity)                 [executable]
//!   │         └─ argument("effect", a resource id)           [executable]
//!   └─ literal("give")
//!        └─ argument("targets", any entity)
//!             └─ argument("effect", a resource id)           [executable: infinite, amplifier 0]
//!                  └─ argument("seconds", integer(1, …))     [executable]
//!                       └─ argument("amplifier", integer(0, 255)) [executable]
//!                            └─ argument("hideParticles", bool()) [executable]
//! ```
//!
//! Two things a from-memory reconstruction gets wrong, both load-bearing:
//!
//! * **`clear` and `give` are sibling literals under `effect`**, and `clear` is
//!   executable at three depths (bare, with targets, with a specific effect) while
//!   `give` is executable only from `effect` onwards. So `/effect give @s` is *not* a
//!   command; `/effect clear` is.
//! * **`<targets>` comes before `<effect>`**, the opposite of how the command reads in
//!   English — the same ordering trap `give`'s own doc records.
//!
//! # What this landing implements, and what it leaves out
//!
//! `give` down to `amplifier`, and `clear` at all three depths. Deliberately absent:
//!
//! * **The hide-particles flag.** Purely presentational (a visibility flag on the
//!   effect instance), and `crate::mob_effects` does not model the
//!   presentational fields at all — see its own doc for why. A node that
//!   parsed the flag and discarded it would be worse than one that is not
//!   there.
//! * **The duration `infinite` literal** the real command accepts in place
//!   of `<seconds>`. An effect instance supports an infinite duration and
//!   the two-argument `give` form uses it, which is the real default; only
//!   the explicit spelling is missing.
//! * **Non-player targets.** The real `<targets>` argument accepts mobs;
//!   `Effect` is per-*player* by construction (`DirectedEffect` carries a profile
//!   uuid), and `MobSim` holds no effect state. So this uses `players()`, and the
//!   narrowing is visible in the wire tree rather than silent at runtime.
//!
//! # Why seconds, not ticks
//!
//! The real command multiplies by 20 (`duration = seconds * 20`) and clamps
//! the *seconds* argument, not the ticks. So the maximum is `1_000_000`
//! seconds, i.e. 20,000,000 ticks — comfortably inside `i32`, which is why
//! the real rule does not need a saturating multiply and this does not
//! either.

use lodestone_command::{IntegerArgument, StringArgument};
use lodestone_command_mc::EntityArg;

use super::effect::Effect;
use super::registrar::{Ctx, Registrar};
use super::CommandResult;

/// The game-masters permission level.
const EFFECT_LEVEL: u8 = 2;

/// The real seconds-to-ticks factor.
const TICKS_PER_SECOND: i32 = 20;

/// The default `give` duration when no `<seconds>` is supplied. The real
/// two-argument form is infinite, not 30 seconds — a plausible wrong default that
/// would make `/effect give @s minecraft:poison` a fixed-length effect.
const DEFAULT_DURATION: i32 = crate::mob_effects::INFINITE_DURATION;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let effect = registrar.literal(root, "effect");
    registrar.require_level(effect, EFFECT_LEVEL);

    // ---- clear, executable at three depths -----------------------------------
    let clear = registrar.literal(effect, "clear");
    registrar.exec(clear, |ctx| {
        // Bare `/effect clear` targets the sender. The real zero-argument
        // overload resolves the source to its own entity — and from the
        // console there is no entity, so the command fails rather than
        // silently affecting nobody.
        let Some(sender) = ctx.source.entity.clone() else {
            return Err("That command can only be used by a player".to_string());
        };
        ctx.effect(sender.uuid, Effect::ClearEffects { effect: None });
        ctx.send_success(format!("Took every effect from {}", sender.username));
        Ok(1)
    });

    let (clear_targets, clear_targets_key) =
        registrar.arg(clear, "targets", EntityArg::players());
    registrar.exec(clear_targets, move |ctx| {
        let selector = ctx.get(clear_targets_key).clone();
        clear_effects(ctx, &selector, None)
    });

    let (clear_effect, clear_effect_key) =
        registrar.arg(clear_targets, "effect", StringArgument::word());
    registrar.exec(clear_effect, move |ctx| {
        let selector = ctx.get(clear_targets_key).clone();
        let id = ctx.get(clear_effect_key).clone();
        clear_effects(ctx, &selector, Some(id))
    });

    // ---- give, executable from <effect> onwards -------------------------------
    let give = registrar.literal(effect, "give");
    let (give_targets, give_targets_key) = registrar.arg(give, "targets", EntityArg::players());
    let (give_effect, give_effect_key) =
        registrar.arg(give_targets, "effect", StringArgument::word());
    registrar.exec(give_effect, move |ctx| {
        let selector = ctx.get(give_targets_key).clone();
        let id = ctx.get(give_effect_key).clone();
        give_effect_to(ctx, &selector, &id, DEFAULT_DURATION, 0)
    });

    let (give_seconds, give_seconds_key) = registrar.arg(
        give_effect,
        "seconds",
        // The real bounds are on the *seconds*, which is why no saturating multiply
        // is needed below.
        IntegerArgument::bounded(1, 1_000_000),
    );
    registrar.exec(give_seconds, move |ctx| {
        let selector = ctx.get(give_targets_key).clone();
        let id = ctx.get(give_effect_key).clone();
        let seconds = *ctx.get(give_seconds_key);
        give_effect_to(ctx, &selector, &id, seconds * TICKS_PER_SECOND, 0)
    });

    let (give_amplifier, give_amplifier_key) =
        registrar.arg(give_seconds, "amplifier", IntegerArgument::bounded(0, 255));
    registrar.exec(give_amplifier, move |ctx| {
        let selector = ctx.get(give_targets_key).clone();
        let id = ctx.get(give_effect_key).clone();
        let seconds = *ctx.get(give_seconds_key);
        let amplifier = *ctx.get(give_amplifier_key);
        give_effect_to(
            ctx,
            &selector,
            &id,
            seconds * TICKS_PER_SECOND,
            u32::try_from(amplifier).unwrap_or(0),
        )
    });
}

/// Canonicalises an effect id the way the rest of this crate reads block and item
/// names: a bare path gets the `minecraft:` namespace.
///
/// The real rule resolves through the `mob_effect` registry and refuses an unknown id.
/// There is no such registry here, so an unknown id is accepted and simply does
/// nothing — which is honest but worth knowing: `crate::mob_effects` ticks only the
/// four periodic effects and stores the rest for a consumer that may not exist yet.
fn canonical_effect_id(raw: &str) -> String {
    if raw.contains(':') {
        raw.to_owned()
    } else {
        format!("minecraft:{raw}")
    }
}

/// The real give-effect rule.
fn give_effect_to(
    ctx: &mut Ctx<'_>,
    selector: &lodestone_command_mc::EntitySelector,
    raw_id: &str,
    duration: i32,
    amplifier: u32,
) -> CommandResult {
    let id = canonical_effect_id(raw_id);
    let targets = ctx.resolve(selector)?;
    for target in &targets {
        ctx.effect(
            target.uuid,
            Effect::ApplyEffect {
                effect: id.clone(),
                duration,
                amplifier,
            },
        );
    }
    if let [only] = targets.as_slice() {
        ctx.send_success(format!("Applied effect {id} to {}", only.username));
    } else {
        ctx.send_success(format!("Applied effect {id} to {} players", targets.len()));
    }
    Ok(i32::try_from(targets.len()).unwrap_or(i32::MAX))
}

/// The real clear-effects/clear-one-effect rule.
fn clear_effects(
    ctx: &mut Ctx<'_>,
    selector: &lodestone_command_mc::EntitySelector,
    raw_id: Option<String>,
) -> CommandResult {
    let id = raw_id.as_deref().map(canonical_effect_id);
    let targets = ctx.resolve(selector)?;
    for target in &targets {
        ctx.effect(
            target.uuid,
            Effect::ClearEffects {
                effect: id.clone(),
            },
        );
    }
    match (&id, targets.as_slice()) {
        (Some(id), [only]) => {
            ctx.send_success(format!("Took effect {id} from {}", only.username));
        }
        (Some(id), _) => {
            ctx.send_success(format!("Took effect {id} from {} players", targets.len()));
        }
        (None, [only]) => {
            ctx.send_success(format!("Took every effect from {}", only.username));
        }
        (None, _) => {
            ctx.send_success(format!(
                "Took every effect from {} players",
                targets.len()
            ));
        }
    }
    Ok(i32::try_from(targets.len()).unwrap_or(i32::MAX))
}
