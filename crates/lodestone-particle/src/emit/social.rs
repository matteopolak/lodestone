use super::*;

/// The parameters vanilla's own base ash/smoke particle's constructor takes, which is what
/// separates its five subclasses from one another.
///
/// Vanilla's base takes eight of these positionally after the coordinates, so a
/// caller reads as a wall of bare floats in which two adjacent same-typed
/// arguments transpose without a trace. Naming them is not decoration: `smoke`
/// and `ash` differ only in `dir.1`'s **sign**, `colour_random`, `max_lifetime`
/// and `gravity`'s sign, and every one of those is a lone number.
#[derive(Debug, Clone, Copy)]
pub struct AshSmokeParams {
    /// `dirX/dirY/dirZ` — per-axis damping applied to the *scattered* velocity
    /// before the caller's own is added. Negative flips that axis.
    pub dir: [f32; 3],
    /// Vanilla's own "color random" field — the greyscale tint is
    /// `nextFloat() * colorRandom`, so `0.0` means black before any color
    /// override the subclass applies.
    pub colour_random: f32,
    /// Vanilla's own "max lifetime" field — the numerator of `(int)(maxLifetime / (nextFloat() * 0.8
    /// + 0.2) * scale)`.
    pub max_lifetime: i32,
    /// `gravity`. Negative rises.
    pub gravity: f32,
    /// Vanilla's own "has physics" field — smoke collides, ash does not.
    pub has_physics: bool,
}

/// Vanilla's own base ash/smoke particle's constructor, shared by `smoke`,
/// `large_smoke`, `ash`, `white_ash` and `white_smoke`.
///
/// Vanilla's own "set sprite from age" step runs at the end of the constructor, before the first
/// tick, which is why the sprite is re-stamped here rather than left on frame
/// zero.
pub fn base_ash_smoke(
    engine: &mut ParticleEngine,
    (x, y, z): (f64, f64, f64),
    (xa, ya, za): (f64, f64, f64),
    sheet: Sheet,
    scale: f32,
    params: AshSmokeParams,
) -> Particle {
    let rng = engine.rng();
    let mut p =
        Particle::with_velocity(x, y, z, 0.0, 0.0, 0.0, SpriteSource::Sheet { sheet, frame: 0 }, rng);
    p.friction = 0.96;
    p.gravity = params.gravity;
    // Smoke that hits a ceiling spreads sideways instead of piling up.
    p.speed_up_when_y_blocked = true;
    p.xd = p.xd.mul_add(f64::from(params.dir[0]), xa);
    p.yd = p.yd.mul_add(f64::from(params.dir[1]), ya);
    p.zd = p.zd.mul_add(f64::from(params.dir[2]), za);
    let col = rng_next(engine) * params.colour_random;
    p.colour = [col, col, col];
    p.quad_size *= 0.75 * scale;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(params.max_lifetime) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)
        * f64::from(scale)) as i32;
    p.lifetime = lifetime.max(1);
    p.has_physics = params.has_physics;
    p.behaviour = Behaviour::AshSmoke;
    p.sprite = SpriteSource::Sheet {
        sheet,
        frame: sheet.frame_for_age(0, p.lifetime),
    };
    p
}

/// Vanilla's own smoke particle — the base ash/smoke particle with smoke's parameters
/// (`0.3` colour jitter, 8-tick base lifetime, `-0.1` gravity so it rises).
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own smoke-particle constructor argument for argument"
)]
pub fn smoke(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    scale: f32,
) {
    let p = base_ash_smoke(
        engine,
        (x, y, z),
        (xa, ya, za),
        Sheet::Generic,
        scale,
        AshSmokeParams {
            dir: [0.1, 0.1, 0.1],
            colour_random: 0.3,
            max_lifetime: 8,
            gravity: -0.1,
            has_physics: true,
        },
    );
    engine.add(p);
}

/// Vanilla's own white-smoke particle — smoke's parameters exactly, over a
/// fixed lilac-grey tint (`0xBAB1C2`) rather than the greyscale draw.
///
/// The color-random draw still happens (it is inside the base constructor) and
/// is then overwritten, so the RNG stream length matches vanilla's.
pub fn white_smoke(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let mut p = base_ash_smoke(
        engine,
        (x, y, z),
        (xa, ya, za),
        Sheet::Generic,
        1.0,
        AshSmokeParams {
            dir: [0.1, 0.1, 0.1],
            colour_random: 0.3,
            max_lifetime: 8,
            gravity: -0.1,
            has_physics: true,
        },
    );
    p.colour = ASH_WHITE;
    engine.add(p);
}

/// Vanilla's own ash particle — the black flakes drifting down through the soul sand valley.
///
/// Three sign-level differences from [`smoke`], each a lone number in vanilla's
/// positional argument list: the vertical scatter direction is **negative**
/// (the scattered vertical component is inverted), gravity is **positive**
/// `0.1` so it falls rather than rises, and collision is **off** so it drifts
/// through the terrain.
/// Its sheet is [`Sheet::Generic0`], a single frame — `ash.json` names one
/// texture, so ash does not animate at all.
pub fn ash(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let p = base_ash_smoke(
        engine,
        (x, y, z),
        (0.0, 0.0, 0.0),
        Sheet::Generic0,
        1.0,
        AshSmokeParams {
            dir: [0.1, -0.1, 0.1],
            colour_random: 0.5,
            max_lifetime: 20,
            gravity: 0.1,
            has_physics: false,
        },
    );
    engine.add(p);
}

/// Vanilla's own white-ash particle — the basalt delta's pale drift.
///
/// Vanilla's own ash particle's shape with a far gentler `0.0125` gravity, a
/// color-random field of **zero** (so the greyscale draw yields black and the fixed tint below is
/// the whole colour), and a provider-supplied initial velocity: three products
/// of two `nextFloat()`s each, all negative, so the flakes always drift down
/// and toward `-x`/`-z`.
pub fn white_ash(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let rng = engine.rng();
    let xa = f64::from(rng.next_f32()) * -1.9 * f64::from(rng.next_f32()) * 0.1;
    let ya = f64::from(rng.next_f32()) * -0.5 * f64::from(rng.next_f32()) * 0.1 * 5.0;
    let za = f64::from(rng.next_f32()) * -1.9 * f64::from(rng.next_f32()) * 0.1;
    let mut p = base_ash_smoke(
        engine,
        (x, y, z),
        (xa, ya, za),
        Sheet::Generic0,
        1.0,
        AshSmokeParams {
            dir: [0.1, -0.1, 0.1],
            colour_random: 0.0,
            max_lifetime: 20,
            gravity: 0.0125,
            has_physics: false,
        },
    );
    p.colour = ASH_WHITE;
    engine.add(p);
}

/// `0xBAB1C2` as `[f32; 3]` — the tint vanilla's own white-ash and
/// white-smoke particles both declare as their own packed-RGB constant and then
/// unpack channel by channel (`186, 177, 194`).
const ASH_WHITE: [f32; 3] = [186.0 / 255.0, 177.0 / 255.0, 194.0 / 255.0];

/// Vanilla's own flame particle — a rising particle that ignores collision.
pub fn flame(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xd: f64, yd: f64, zd: f64) {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        xd,
        yd,
        zd,
        SpriteSource::Sheet {
            sheet: Sheet::Flame,
            frame: 0,
        },
        rng,
    );
    p.friction = 0.96;
    // `xd * 0.01F + xd` — the scattered component is almost entirely discarded
    // and replaced by the requested velocity, so flames rise in a tight column.
    let damp = f64::from(0.01_f32);
    p.xd = p.xd.mul_add(damp, xd);
    p.yd = p.yd.mul_add(damp, yd);
    p.zd = p.zd.mul_add(damp, zd);
    let jitter = |r: &mut JavaRandom| f64::from((r.next_f32() - r.next_f32()) * 0.05);
    let rng = engine.rng();
    let (jx, jy, jz) = (jitter(rng), jitter(rng), jitter(rng));
    p.set_pos(p.x + jx, p.y + jy, p.z + jz);
    p.xo = p.x;
    p.yo = p.y;
    p.zo = p.z;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(8.0_f32) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32 + 4;
    p.lifetime = lifetime;
    p.behaviour = Behaviour::Flame;
    engine.add(p);
}

/// Vanilla's own bubble particle — rises through water and pops the instant it leaves.
pub fn bubble(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::Bubble,
            frame: 0,
        },
        rng,
    );
    let rng = engine.rng();
    let scatter = |r: &mut JavaRandom| f64::from(r.next_f32().mul_add(2.0, -1.0) * 0.02);
    p.xd = xa.mul_add(f64::from(0.2_f32), scatter(rng));
    p.yd = ya.mul_add(f64::from(0.2_f32), scatter(rng));
    p.zd = za.mul_add(f64::from(0.2_f32), scatter(rng));
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(8.0_f32) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime;
    p.behaviour = Behaviour::Bubble;
    engine.add(p);
}

/// Vanilla's own splash particle — a water-drop particle launched by something entering water.
pub fn splash(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        0.0,
        0.0,
        0.0,
        SpriteSource::Sheet {
            sheet: Sheet::Splash,
            frame: 0,
        },
        rng,
    );
    // Vanilla's own water-drop particle's constructor.
    p.xd *= f64::from(0.3_f32);
    p.zd *= f64::from(0.3_f32);
    p.yd = f64::from(rng_next(engine).mul_add(0.2, 0.1));
    p.set_size(0.01, 0.01);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(8.0_f32) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime;
    // Vanilla's own splash particle overrides gravity and, for purely horizontal input,
    // replaces the velocity outright so the drop arcs upward.
    p.gravity = 0.04;
    if ya == 0.0 && (xa != 0.0 || za != 0.0) {
        p.xd = xa;
        p.yd = 0.1;
        p.zd = za;
    }
    p.behaviour = Behaviour::WaterDrop;
    let frame = engine.rng().next_i32_bound(i32::from(Sheet::Splash.frame_count()));
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "bounded by the sheet's frame count"
    )]
    {
        p.sprite = SpriteSource::Sheet {
            sheet: Sheet::Splash,
            frame: frame as u16,
        };
    }
    engine.add(p);
}

/// One uniform random float in [0, 1) from the engine's RNG.
fn rng_next(engine: &mut ParticleEngine) -> f32 {
    engine.rng().next_f32()
}

/// The attack-sweep particle — the arc thrown by a sweeping melee hit.
///
/// Behaviour: no movement step at all (stationary for its whole life — see
/// [`crate::Particle::tick_sweep_attack`]), full-bright, 4-tick lifetime, a
/// grey tint drawn once from a uniform random float scaled to [0.4, 1.0), and
/// a quad size of `1.0 - size * 0.5`.
///
/// `size` is an auxiliary scale parameter on construction — but the one real
/// call site that spawns this particle (a player's melee attack, with the
/// spawn event's count and max-speed fields both zero) resolves that
/// auxiliary value to `max_speed * x_distance`, which is always `0.0` for
/// that event, regardless of the arc's true x-distance — i.e. the quad size
/// is always `1.0` in practice. Taking `size` as a parameter anyway (rather
/// than hardcoding that) keeps this general for any future caller (a
/// datapack or `/particle` invocation can still pass a nonzero value).
pub fn sweep_attack(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, size: f32) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::SweepAttack,
            frame: 0,
        },
        rng,
    );
    p.lifetime = 4;
    let col = rng_next(engine).mul_add(0.6, 0.4);
    p.colour = [col, col, col];
    p.quad_size = size.mul_add(-0.5, 1.0);
    p.behaviour = Behaviour::SweepAttack;
    engine.add(p);
}

/// Vanilla's own note particle — the coloured chime above a played note block.
///
/// From vanilla's own note particle constructor (26.2 decompile):
/// zero initial velocity, `friction = 0.66F`,
/// speed-up-when-Y-motion-is-blocked = true, `yd += 0.2`, a fixed `lifetime = 6`
/// (overwriting whatever the base constructor's lifetime draw produced), and
/// `quadSize *= 1.5F`. The RGB formula reads a note-block "colour" in `[0,
/// 1)` (vanilla passes `note / 24.0`, the tuned-pitch index over its 24-note
/// range) and derives three phase-shifted sine waves from it.
pub fn note(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, color: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::Note,
            frame: 0,
        },
        rng,
    );
    p.friction = 0.66;
    p.speed_up_when_y_blocked = true;
    p.yd += 0.2;
    // `(float) color` widens the double parameter down before every use.
    let c = color as f32;
    let tau = std::f32::consts::TAU;
    let phase = |offset: f32| ((c + offset) * tau).sin().mul_add(0.65, 0.35).max(0.0);
    p.colour = [phase(0.0), phase(0.333_333_34), phase(0.666_666_7)];
    p.quad_size *= 1.5;
    p.lifetime = 6;
    p.behaviour = Behaviour::Note;
    engine.add(p);
}

/// Vanilla's own heart-particle constructor body (26.2 decompile):
/// zero initial velocity, speed-up-when-Y-motion-is-blocked = true,
/// `friction = 0.86F`, `yd += 0.1`, `quadSize *= 1.5F`, `lifetime = 16`,
/// `hasPhysics = false`. [`heart`] and [`angry_villager`] are its two
/// registered providers — same class, different sprite and vertical offset
/// at the emit site (the `+ 0.5` its own angry-villager provider applies).
fn heart_particle(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, sheet: Sheet) -> Particle {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet { sheet, frame: 0 },
        rng,
    );
    p.speed_up_when_y_blocked = true;
    p.friction = 0.86;
    p.yd += 0.1;
    p.quad_size *= 1.5;
    p.lifetime = 16;
    p.has_physics = false;
    p.behaviour = Behaviour::Heart;
    p
}

/// Vanilla's own heart-particle provider — breeding hearts (`minecraft:heart`).
pub fn heart(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let p = heart_particle(engine, x, y, z, Sheet::Heart);
    engine.add(p);
}

/// Vanilla's own heart-particle angry-villager provider — the villager "angry" icon
/// (`minecraft:angry_villager`). Same physics as [`heart`], a different
/// sprite (`particle/angry`, not `particle/heart`), and vanilla raises the
/// spawn point by `0.5` at the call site rather than in the particle class —
/// reproduced here since this function *is* that call site.
pub fn angry_villager(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let p = heart_particle(engine, x, y + 0.5, z, Sheet::Angry);
    engine.add(p);
}

/// Vanilla's own suspended-town-decoration particle's happy-villager
/// provider — the villager "happy" icon (`minecraft:happy_villager`).
///
/// From vanilla's own suspended-town-decoration particle constructor (26.2
/// decompile): a jittered-velocity construction (the same shape
/// [`Particle::with_velocity`] already reproduces) followed by a dim grey
/// tint (`nextFloat() * 0.1F + 0.2F`), a `0.02`×`0.02` box, a
/// `nextFloat() * 0.6F + 0.5F` quad-size jitter, the velocity damped to a
/// hundredth, and `lifetime = (int)(20.0 / (nextFloat() * 0.8F + 0.2F))`.
/// Its own happy-villager provider itself then sets the colour to white,
/// which is redundant here since white is this crate's own particle default.
pub fn happy_villager(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let p = suspended_town(engine, x, y, z, xa, ya, za, Sheet::Glint);
    engine.add(p);
}

