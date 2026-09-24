use super::*;

pub fn rain(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
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
    p.xd *= f64::from(0.3_f32);
    p.zd *= f64::from(0.3_f32);
    p.yd = f64::from(rng_next(engine).mul_add(0.2, 0.1));
    p.set_size(0.01, 0.01);
    p.gravity = 0.06;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(8.0_f32) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime;
    p.behaviour = Behaviour::WaterDrop;
    // Vanilla's own water-drop-particle provider draws its frame from the sheet
    // (`sprite.get(random)`), exactly as the splash provider does.
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

/// Vanilla's own bubble-column-up particle — a soul-sand column's rising bubble.
///
/// The negative `gravity` is what lifts it: the shared tick's
/// `yd -= 0.04 * gravity` becomes an upward term. Flipping the sign and adding
/// a positive `yd` instead would look right for one tick and then sink.
pub fn bubble_column_up(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
) {
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
    p.gravity = -0.125;
    p.friction = 0.85;
    p.set_size(0.02, 0.02);
    p.quad_size *= rng_next(engine).mul_add(0.6, 0.2);
    // `xa * 0.2F + (nextFloat() * 2 - 1) * 0.02F`, per axis and in this order.
    p.xd = xa.mul_add(f64::from(0.2_f32), f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.02));
    p.yd = ya.mul_add(f64::from(0.2_f32), f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.02));
    p.zd = za.mul_add(f64::from(0.2_f32), f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.02));
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(40.0_f32) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime;
    p.behaviour = Behaviour::BubbleColumnUp;
    engine.add(p);
}

/// Vanilla's own downward water-current particle — a magma-block column's sinking, spiralling
/// bubble.
///
/// `has_physics = false`, so it passes through geometry and only the water test
/// and the `on_ground` flag can kill it — and `on_ground` can never be set
/// without physics, which is vanilla's own arrangement.
pub fn current_down(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
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
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast on a small float"
    )]
    let lifetime = (rng_next(engine) * 60.0) as i32;
    p.lifetime = lifetime + 30;
    p.has_physics = false;
    p.xd = 0.0;
    p.yd = -0.05;
    p.zd = 0.0;
    p.set_size(0.02, 0.02);
    p.quad_size *= rng_next(engine).mul_add(0.6, 0.2);
    p.gravity = 0.002;
    p.behaviour = Behaviour::WaterCurrentDown { angle: 0.0 };
    engine.add(p);
}

/// Vanilla's own snowflake particle — a powder-snow cauldron's and a snow golem's flakes.
///
/// `friction = 1.0` is deliberate and load-bearing: the per-axis damping in
/// [`Behaviour::Snowflake`]'s tick is the *whole* of this particle's drag, so
/// leaving the default `0.98` here would damp it twice.
pub fn snowflake(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::Generic,
            frame: 0,
        },
        rng,
    );
    p.gravity = 0.225;
    p.friction = 1.0;
    p.xd = xa + f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.05);
    p.yd = ya + f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.05);
    p.zd = za + f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.05);
    // `0.1F * (nextFloat() * nextFloat() * 1.0F + 1.0F)` — two draws, and the
    // product is what biases the flakes small.
    let a = rng_next(engine);
    let b = rng_next(engine);
    p.quad_size = 0.1 * a.mul_add(b, 1.0);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(16.0_f32) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime + 2;
    // Vanilla's own snowflake-particle provider tints every flake the same pale blue.
    p.colour = [0.923, 0.964, 0.999];
    p.behaviour = Behaviour::Snowflake;
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Generic,
        frame: Sheet::Generic.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

/// Vanilla's own dust-plume particle — the puff a block dropped into a
/// decorated pot throws up, and the one brushing a suspicious block makes.
///
/// Vanilla's own base ash/smoke particle whose colour the subclass **overwrites**: the base
/// constructor's grey draw still happens (so the RNG stream matches) and its
/// result is discarded in favour of `0xBAB1C2` shifted down by a second draw.
/// `ya` is biased `+0.15` before the base sees it, which is the whole of the
/// plume's initial lift.
pub fn dust_plume(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    /// Vanilla's own dust-plume particle's packed-RGB constant.
    const COLOUR_RGB24: u32 = 12_235_202;

    let mut p = base_ash_smoke(
        engine,
        (x, y, z),
        (xa, ya + f64::from(0.15_f32), za),
        Sheet::Generic,
        1.0,
        AshSmokeParams {
            dir: [0.7, 0.6, 0.7],
            colour_random: 0.5,
            max_lifetime: 7,
            gravity: 0.5,
            has_physics: false,
        },
    );
    let shift = rng_next(engine) * 0.2;
    p.colour = [
        f32::from(((COLOUR_RGB24 >> 16) & 0xff) as u8) / 255.0 - shift,
        f32::from(((COLOUR_RGB24 >> 8) & 0xff) as u8) / 255.0 - shift,
        f32::from((COLOUR_RGB24 & 0xff) as u8) / 255.0 - shift,
    ];
    p.behaviour = Behaviour::DustPlume;
    engine.add(p);
}

/// Vanilla's own wake particle — the expanding ring a fishing bobber leaves on the water,
/// which the server sends as `minecraft:fishing`.
///
/// Every one of the base constructor's velocity terms is drawn and then
/// **overwritten** by the packet's, and its `set_size` is likewise superseded
/// by the tick's per-tick resize. Both are kept because the draws are part of
/// the stream and dropping them shifts everything after.
pub fn fishing(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
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
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Splash,
        frame: Sheet::Splash.frame_for_age(0, p.lifetime),
    };
    p.gravity = 0.0;
    p.xd = xa;
    p.yd = ya;
    p.zd = za;
    p.behaviour = Behaviour::Wake;
    engine.add(p);
}

/// Vanilla's own bubble-pop particle — the five-frame burst a column's bubble makes as it
/// breaks the surface.
pub fn bubble_pop(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::BubblePop,
            frame: 0,
        },
        rng,
    );
    p.lifetime = 4;
    p.gravity = 0.008;
    p.xd = xa;
    p.yd = ya;
    p.zd = za;
    p.behaviour = Behaviour::BubblePop;
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::BubblePop,
        frame: Sheet::BubblePop.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

