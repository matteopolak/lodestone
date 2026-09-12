use super::*;

/// Vanilla's own noxious-gas particle provider (`minecraft:noxious_gas`) — the
/// puff the deep-dark's potent-sulfur block throws.
///
/// Vanilla's own base ash/smoke particle's parameters (the same `[0.1, 0.1, 0.1]`
/// scatter, `5`-tick base lifetime and `-0.02` gravity as its own constructor),
/// fixed at scale `3.0` — the provider's own constant, not wire-driven — with a
/// white tint drawn *after* the colour-random draw and then overwritten,
/// the same "consume the draw, then override" shape [`white_smoke`]/[`white_ash`]
/// already use so the RNG stream length matches vanilla's regardless of the
/// override. [`Behaviour::NoxiousGas`] documents the extra alpha fade this
/// family has that plain [`Behaviour::AshSmoke`] does not.
pub fn noxious_gas(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let mut p = base_ash_smoke(
        engine,
        (x, y, z),
        (xa, ya, za),
        Sheet::NoxiousGas,
        3.0,
        AshSmokeParams {
            dir: [0.1, 0.1, 0.1],
            colour_random: 0.3,
            max_lifetime: 5,
            gravity: -0.02,
            has_physics: true,
        },
    );
    p.colour = [1.0, 1.0, 1.0];
    p.behaviour = Behaviour::NoxiousGas;
    engine.add(p);
}

/// Vanilla's own noxious-gas-cloud particle provider
/// (`minecraft:noxious_gas_cloud`) — a non-rendering particle that seeds
/// [`noxious_gas`] puffs around itself. See [`Behaviour::NoxiousGasCloudSeed`]
/// for the per-tick schedule and the one simplification this port makes
/// (no line-of-sight check back to the source block).
///
/// Vanilla's own constructor takes no velocity at all (the plain
/// position-only base), so this draws only the one random number every
/// particle's lifetime draw costs — no quad-size, no velocity scatter.
pub fn noxious_gas_cloud(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        // Never sampled — excluded from `extract` before any sprite lookup —
        // but every `Particle` needs a `SpriteSource`, so this names the
        // sheet its own follow-up puffs use.
        SpriteSource::Sheet { sheet: Sheet::NoxiousGas, frame: 0 },
        rng,
    );
    p.lifetime = 20;
    p.behaviour = Behaviour::NoxiousGasCloudSeed;
    engine.add(p);
}

/// Vanilla's own sulfur-bubble particle provider (`minecraft:sulfur_bubbles`)
/// — the potent-sulfur spring's rising bubble.
///
/// Vanilla's own constructor takes no velocity scatter at all (the plain
/// sprite-only base, like [`fly_towards_position`]'s), then sets its own
/// gravity/friction/size directly. See [`Behaviour::SulfurBubble`] for the
/// per-tick removal conditions.
pub fn sulfur_bubbles(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, za: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet { sheet: Sheet::BubbleWhite, frame: 0 },
        rng,
    );
    p.gravity = -0.04;
    p.friction = 0.85;
    p.set_size(0.02, 0.02);
    p.xd = xa.mul_add(0.2, f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.02));
    p.zd = za.mul_add(0.2, f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.02));
    let size_start = 0.02 + 0.02 * rng_next(engine);
    p.quad_size = size_start;
    p.lifetime = i32::MAX;
    let y_start = p.yo;
    let y_end = y_start + 4.0 - 1.0;
    p.behaviour = Behaviour::SulfurBubble { y_start, y_end, size_start };
    engine.add(p);
}

/// Vanilla's own sulfur-cube-goo particle provider (`minecraft:sulfur_cube_goo`)
/// — the debris a broken sulfur cube throws.
///
/// The wire never carries a velocity for this type (vanilla's own provider
/// calls the sprite-only breaking-item constructor, which passes zero
/// through), so this is exactly [`terrain_particle`]'s shape with no tint
/// override and no wire velocity: the same randomised scatter, the same
/// `gravity = 1.0`/`quadSize /= 2.0`, and the same quarter-sprite
/// [`Behaviour::Terrain`] every other breaking-item/block-debris particle in
/// this crate shares.
pub fn sulfur_cube_goo(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        0.0,
        0.0,
        0.0,
        SpriteSource::Sheet { sheet: Sheet::SulfurCubeGoo, frame: 0 },
        rng,
    );
    p.gravity = 1.0;
    p.quad_size /= 2.0;
    let uo = rng_next(engine) * 3.0;
    let vo = rng_next(engine) * 3.0;
    p.behaviour = Behaviour::Terrain { uo, vo };
    engine.add(p);
}

/// Vanilla's own trial-spawner/vault detection-rune particle provider
/// (`minecraft:trial_spawner_detection`/`minecraft:trial_spawner_detection_ominous`)
/// — the two share one class and differ only in `sheet`.
///
/// Vanilla's own constructor passes zero through the velocity-scatter base
/// (so this crate's usual random jitter still happens and is then almost
/// entirely discarded — `xd`/`zd` are zeroed outright and `yd` keeps 90% of
/// its scattered value) before the packet's own velocity is added in. Scale
/// is vanilla's own provider's fixed `1.5`, not wire-driven.
pub fn trial_spawner_detection(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    sheet: Sheet,
) {
    const SCALE: f32 = 1.5;
    let rng = engine.rng();
    let mut p =
        Particle::with_velocity(x, y, z, 0.0, 0.0, 0.0, SpriteSource::Sheet { sheet, frame: 0 }, rng);
    p.friction = 0.96;
    p.gravity = -0.1;
    p.speed_up_when_y_blocked = true;
    p.xd = xa;
    p.yd = p.yd * 0.9 + ya;
    p.zd = za;
    p.quad_size *= 0.75 * SCALE;
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    let lifetime = (8.0 / rng_next(engine).mul_add(0.5, 0.5) * SCALE) as i32;
    p.lifetime = lifetime.max(1);
    p.has_physics = true;
    p.behaviour = Behaviour::TrialSpawnerDetection;
    p.sprite = SpriteSource::Sheet { sheet, frame: sheet.frame_for_age(0, p.lifetime) };
    engine.add(p);
}

/// Vanilla's own vault-connection particle provider (`minecraft:vault_connection`)
/// — [`fly_towards_position`]'s flight curve, glowing, `1.5×` size, and an
/// alpha that ramps in over the back three quarters of life instead of
/// holding opaque. See [`Behaviour::FlyTowardsPositionFading`].
pub fn vault_connection(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xd: f64, yd: f64, zd: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet { sheet: Sheet::VaultConnection, frame: 0 },
        rng,
    );
    p.xd = xd;
    p.yd = yd;
    p.zd = zd;
    p.spawn = [x, y, z];
    p.set_pos(x + xd, y + yd, z + zd);
    p.xo = p.x;
    p.yo = p.y;
    p.zo = p.z;
    p.quad_size = 0.1 * rng_next(engine).mul_add(0.5, 0.2);
    let br = rng_next(engine).mul_add(0.6, 0.4);
    p.colour = [0.9 * br, 0.9 * br, br];
    p.has_physics = false;
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    let lifetime = (rng_next(engine) * 10.0) as i32 + 30;
    p.lifetime = lifetime;
    p.alpha = 0.0;
    p.scale(1.5);
    p.behaviour = Behaviour::FlyTowardsPositionFading;
    engine.add(p);
}

/// Vanilla's own ominous-spawning particle provider (`minecraft:ominous_spawning`)
/// — a straight-line homing mote (unlike [`fly_towards_position`]'s quartic
/// dip) whose colour lerps from a fixed light blue to white over its life.
/// Scale is vanilla's own provider's own `3.0..5.0` random draw, not
/// wire-driven. See [`Behaviour::FlyStraightTowards`].
pub fn ominous_spawning(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xd: f64, yd: f64, zd: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet { sheet: Sheet::OminousSpawning, frame: 0 },
        rng,
    );
    p.xd = xd;
    p.yd = yd;
    p.zd = zd;
    p.spawn = [x, y, z];
    p.set_pos(x + xd, y + yd, z + zd);
    p.xo = p.x;
    p.yo = p.y;
    p.zo = p.z;
    p.quad_size = 0.1 * rng_next(engine).mul_add(0.5, 0.2);
    p.has_physics = false;
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    let lifetime = (rng_next(engine) * 5.0) as i32 + 25;
    p.lifetime = lifetime;
    let scale = rng_next(engine).mul_add(2.0, 3.0);
    p.scale(scale);
    p.behaviour = Behaviour::FlyStraightTowards;
    engine.add(p);
}

/// Vanilla's own wind-charge/gust-emitter seed particle provider
/// (`minecraft:gust_emitter_large`/`minecraft:gust_emitter_small`) — a
/// non-rendering particle that throws `gust` puffs around itself.
/// `scale`/`lifetime`/`tick_delay` are vanilla's own two provider
/// registrations' own constants (`(3.0, 7, 0)` for `_large`, `(1.0, 3, 2)`
/// for `_small`), never wire-driven. See [`Behaviour::GustSeed`].
pub fn gust_emitter(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    scale: f64,
    lifetime: i32,
    tick_delay: i32,
) {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        0.0,
        0.0,
        0.0,
        // Never sampled — excluded from `extract` — but every `Particle`
        // needs a `SpriteSource`, so this names the sheet its own follow-up
        // puffs use.
        SpriteSource::Sheet { sheet: Sheet::Gust, frame: 0 },
        rng,
    );
    p.lifetime = lifetime;
    p.behaviour = Behaviour::GustSeed { scale, tick_delay };
    engine.add(p);
}

/// Vanilla's own simple-vertical particle provider
/// (`minecraft:pause_mob_growth`/`minecraft:reset_mob_growth`) — a fixed
/// 8-tick billboard over [`Sheet::Glint`] (the same texture the villager
/// "happy" icon uses; `pause_mob_growth.json`/`reset_mob_growth.json` both
/// name it), drifting up (`reset`) or down (`pause`) at a constant `0.03`.
pub fn simple_vertical(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    upwards: bool,
) {
    let rng = engine.rng();
    let mut p =
        Particle::new(x, y, z, SpriteSource::Sheet { sheet: Sheet::Glint, frame: 0 }, rng);
    p.xd = xa;
    p.zd = za;
    p.yd = ya + if upwards { 0.03 } else { -0.03 };
    p.gravity = 0.0;
    p.quad_size *= rng_next(engine).mul_add(0.6, 0.5);
    p.lifetime = 8;
    p.behaviour = Behaviour::SimpleVertical;
    engine.add(p);
}

/// Vanilla's own shriek particle provider (`minecraft:shriek`) — the sculk
/// shrieker's shockwave. `delay` is the wire payload's own field; a
/// connection whose protocol family gives this client no payload for
/// it draws with delay `0` (immediate), which only costs the initial pause
/// rather than the particle itself.
///
/// Vanilla draws this as two crossed planes at a fixed pitch rather than a
/// camera-facing billboard; this crate has no non-camera-facing billboard
/// mode, so it draws one upright billboard instead — see
/// `docs/particle-catalogue.md`.
pub fn shriek(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, delay: i32) {
    let rng = engine.rng();
    let mut p =
        Particle::new(x, y, z, SpriteSource::Sheet { sheet: Sheet::Shriek, frame: 0 }, rng);
    p.quad_size = 0.85;
    p.lifetime = 30;
    p.gravity = 0.0;
    p.xd = 0.0;
    p.yd = 0.1;
    p.zd = 0.0;
    p.alpha = 1.0;
    p.behaviour = Behaviour::Shriek { delay };
    engine.add(p);
}

/// Vanilla's own geyser-eruption particle provider (`minecraft:geyser`) — a
/// non-rendering particle that throws [`geyser_base`](geyser_base_or_poof)/
/// [`geyser_plume`]/[`geyser_poof`](geyser_base_or_poof) puffs around itself.
/// See [`Behaviour::GeyserEruptionSeed`] for the three throw schedules.
///
/// Vanilla's own constructor stores the wire velocity as its own fields
/// rather than threading it through `Particle`'s velocity mechanism at all
/// (its own `super()` call takes no velocity), so this draws only the one
/// random number every particle's lifetime draw costs.
pub fn geyser(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    water_blocks: i32,
) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        // Never sampled — excluded from `extract` — but every `Particle`
        // needs a `SpriteSource`, so this names a sheet one of its own
        // follow-ups uses.
        SpriteSource::Sheet { sheet: Sheet::GeyserPlume, frame: 0 },
        rng,
    );
    p.lifetime = 20;
    p.behaviour = Behaviour::GeyserEruptionSeed { water_blocks, vel: [xa, ya, za] };
    engine.add(p);
}

/// Vanilla's own geyser-base/geyser-poof particle provider
/// (`minecraft:geyser_base`/`minecraft:geyser_poof`) — one class, two sheets
/// and two `burst_impulse_base` constants (`1.5` for `geyser_base`, `2.0` for
/// `geyser_poof` — vanilla's own eruption particle's own two child
/// constructions, never wire-driven).
///
/// Shares [`base_ash_smoke`]'s scatter (`dir` set to the burst impulse on all
/// three axes rather than a fixed `[0.1, 0.1, 0.1]`) and its `AshSmoke`
/// fade-in/sheet-advance, then overrides friction, forces the vertical
/// component upward, fixes a white tint and draws its own lifetime — the
/// four things vanilla's own subclass constructor changes after its own
/// `super()` call.
#[expect(clippy::too_many_arguments, reason = "mirrors vanilla's own two-child construction")]
pub fn geyser_base_or_poof(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    water_blocks: i32,
    burst_impulse_base: f32,
    sheet: Sheet,
) {
    // The provider's own position jitter, drawn before the particle's own
    // constructor.
    let jx = f64::from(rng_next(engine).mul_add(1.0, -0.5) * 0.5);
    let jy = f64::from(rng_next(engine).mul_add(1.0, -0.5) * 0.5) + 0.2;
    let jz = f64::from(rng_next(engine).mul_add(1.0, -0.5) * 0.5);
    #[expect(clippy::cast_precision_loss, reason = "water_blocks is a small positive int")]
    let water_blocks_f = water_blocks as f32;
    let burst_impulse = burst_impulse_base + 0.25 * water_blocks_f;
    let size = 3.0 + 0.125 * water_blocks_f;
    let mut p = base_ash_smoke(
        engine,
        (x + jx, y + jy, z + jz),
        (xa, ya, za),
        sheet,
        size,
        AshSmokeParams {
            dir: [burst_impulse; 3],
            colour_random: 0.0,
            // Vanilla's own subclass passes `0` here and then overwrites
            // `lifetime` unconditionally below, so the base constructor's own
            // formula produces exactly `0 / anything = 0` and is dead on
            // arrival either way; this port skips drawing the (irrelevant)
            // random denominator for it rather than reproducing a draw whose
            // result never survives.
            max_lifetime: 0,
            gravity: 0.0,
            has_physics: true,
        },
    );
    p.friction = 0.725;
    p.colour = [1.0, 1.0, 1.0];
    p.yd = p.yd.abs();
    let lifetime_factor = rng_next(engine).mul_add(0.2, 0.8);
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    {
        p.lifetime = (25.0 * lifetime_factor) as i32;
    }
    engine.add(p);
}

/// Vanilla's own geyser-plume particle provider (`minecraft:geyser_plume`) —
/// the rising jet. See [`Behaviour::GeyserPlume`] for the per-tick climb.
///
/// The ctor-scattered `xd`/`zd` vanilla's own version derives from the
/// wire velocity are skipped here (set to zero instead): the very first tick
/// overwrites both from the height-progress drift regardless, so the
/// scattered values only ever matter for one drawn frame before any tick has
/// run, which this port does not attempt to reproduce.
pub fn geyser_plume(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    _xa: f64,
    _ya: f64,
    _za: f64,
    water_blocks: i32,
) {
    // The provider's own position jitter, drawn before the particle's own
    // constructor. Vanilla's own vertical jitter is a bare `nextFloat()` — no
    // `-0.5`/scale, unlike the horizontal two.
    let jx = f64::from(rng_next(engine).mul_add(1.0, -0.5) * 0.2);
    let jy = f64::from(rng_next(engine));
    let jz = f64::from(rng_next(engine).mul_add(1.0, -0.5) * 0.2);
    let rng = engine.rng();
    let mut p = Particle::new(
        x + jx,
        y + jy,
        z + jz,
        SpriteSource::Sheet { sheet: Sheet::GeyserPlume, frame: 0 },
        rng,
    );
    let plume_height = 5 * water_blocks.max(1);
    p.has_physics = true;
    p.speed_up_when_y_blocked = true;
    p.lifetime = plume_height * 5;
    p.yd = 0.0;
    p.xd = 0.0;
    p.zd = 0.0;
    let y_start = p.y;
    let y_max = y_start + f64::from(plume_height) - 1.0;
    p.friction = 1.0;
    #[expect(clippy::cast_precision_loss, reason = "plume_height is a small positive int")]
    let plume_height_f = plume_height as f32;
    let initial_propulsion = (if water_blocks == 1 { 1.5 } else { 1.0 }) * plume_height_f * 1.45;
    p.gravity = -initial_propulsion;
    let initially_randomized_size = p.quad_size * 0.75;
    let min_size = initially_randomized_size * (2.0 + plume_height_f / 8.0);
    let max_size = initially_randomized_size * (3.0 + plume_height_f / 8.0);
    p.quad_size = min_size;
    let horiz_x = rng_next(engine).mul_add(1.0, -0.5) * 0.2;
    let horiz_z = rng_next(engine).mul_add(1.0, -0.5) * 0.2;
    p.behaviour = Behaviour::GeyserPlume {
        y_start,
        y_max,
        initial_propulsion,
        horiz_x,
        horiz_z,
        min_size,
        max_size,
        done: false,
    };
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::GeyserPlume,
        frame: Sheet::GeyserPlume.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}
