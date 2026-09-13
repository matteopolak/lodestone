use super::*;


/// Vanilla's own player-cloud particle — the `cloud` puff (an area-effect cloud, a dolphin's
/// wake, a thrown potion's burst) and, tinted green, a panda's `sneeze`.
///
/// Two numbers set it apart from the smoke family it superficially resembles:
/// the quad is **grown** by `1.875` rather than shrunk by `0.75`, and the
/// lifetime is the usual draw *multiplied by `2.5`* — a cloud is both bigger and
/// far longer-lived than a puff of smoke. Its colour draw also runs the other
/// way (`1 - nextFloat() * 0.3`, so near-white) against smoke's
/// `nextFloat() * 0.3` (so near-black).
fn player_cloud(
    engine: &mut ParticleEngine,
    (x, y, z): (f64, f64, f64),
    (xa, ya, za): (f64, f64, f64),
) -> Particle {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        0.0,
        0.0,
        0.0,
        SpriteSource::Sheet {
            sheet: Sheet::Generic,
            frame: 0,
        },
        rng,
    );
    p.friction = 0.96;
    let damp = f64::from(0.1_f32);
    p.xd = p.xd.mul_add(damp, xa);
    p.yd = p.yd.mul_add(damp, ya);
    p.zd = p.zd.mul_add(damp, za);
    let col = rng_next(engine).mul_add(-0.3, 1.0);
    p.colour = [col, col, col];
    p.quad_size *= 1.875;
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    let base = (8.0 / f64::from(rng_next(engine)).mul_add(0.8, 0.3)) as i32;
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "mirrors the base lifetime scaled by 2.5 and clamped to a floor of 1.0, then truncated"
    )]
    {
        p.lifetime = (base as f32 * 2.5).max(1.0) as i32;
    }
    p.has_physics = false;
    p.behaviour = Behaviour::Cloud;
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Generic,
        frame: Sheet::Generic.frame_for_age(0, p.lifetime),
    };
    p
}

/// Vanilla's own player-cloud-particle provider (`minecraft:cloud`).
pub fn cloud(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let p = player_cloud(engine, (x, y, z), (xa, ya, za));
    engine.add(p);
}

/// Vanilla's own player-cloud-particle sneeze provider — a baby panda's sneeze. The same puff
/// tinted green and dropped to `0.4` alpha.
pub fn sneeze(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let mut p = player_cloud(engine, (x, y, z), (xa, ya, za));
    p.colour = [0.22, 1.0, 0.53];
    p.alpha = 0.4;
    engine.add(p);
}

/// Vanilla's own lava particle — the popping embers over a lava surface.
///
/// Its **vertical velocity is not the caller's**: the constructor damps all
/// three axes to `0.8` and then overwrites `yd` outright with
/// `nextFloat() * 0.4 + 0.05`, so every pop launches upward regardless of what
/// the packet asked for. The quad-size jitter is also unusually wide
/// (`nextFloat() * 2.0 + 0.2`, i.e. up to eleven times the smallest), which is
/// why a lava lake throws a mix of specks and fat blobs.
///
/// [`Behaviour::Lava`] carries the trailing-smoke roll; see there.
pub fn lava(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        0.0,
        0.0,
        0.0,
        SpriteSource::Sheet {
            sheet: Sheet::Lava,
            frame: 0,
        },
        rng,
    );
    p.gravity = 0.75;
    p.friction = 0.999;
    let damp = f64::from(0.8_f32);
    p.xd *= damp;
    p.zd *= damp;
    p.yd = f64::from(rng_next(engine).mul_add(0.4, 0.05));
    p.quad_size *= rng_next(engine).mul_add(2.0, 0.2);
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    let lifetime = (16.0 / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime.max(1);
    p.behaviour = Behaviour::Lava;
    engine.add(p);
}

/// Vanilla's own squid-ink particle — a squid's ink cloud (`squid_ink`) and a glow squid's
/// (`glow_squid_ink`).
///
/// Note the lifetime: `(int)(quadSize * 12.0F / (nextFloat() * 0.8F + 0.2F))`
/// with the quad-size field **already fixed at `0.5`**, so the numerator is `6.0` and no
/// random size draw feeds it — unlike every other lifetime in this file, this
/// one is not scaled by a jittered size. `glow_squid_ink`'s only difference is
/// the tint, and it is a translucent one (`alpha 0.6` in the packed colour)
/// applied on top of a constructor that has just set alpha to `1.0`; the
/// **later** call wins, so the alpha is the packed value.
fn squid_ink_particle(
    engine: &mut ParticleEngine,
    (x, y, z): (f64, f64, f64),
    (xa, ya, za): (f64, f64, f64),
    colour: [f32; 3],
    alpha: f32,
) {
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
    p.friction = 0.92;
    p.quad_size = 0.5;
    p.alpha = alpha;
    p.colour = colour;
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    let lifetime = (f64::from(0.5_f32 * 12.0) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime.max(1);
    p.has_physics = false;
    p.xd = xa;
    p.yd = ya;
    p.zd = za;
    p.behaviour = Behaviour::SquidInk;
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Generic,
        frame: Sheet::Generic.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

/// Vanilla's own squid-ink-particle provider — plain black ink (`0xFF000000`).
pub fn squid_ink(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    squid_ink_particle(engine, (x, y, z), (xa, ya, za), [0.0, 0.0, 0.0], 1.0);
}

/// Vanilla's own squid-ink-particle glow-ink provider — its own packed-colour
/// constructor called `(1.0F, 0.2F, 0.8F, 0.6F)`, i.e. **alpha 1.0** with an
/// `(0.2, 0.8, 0.6)` teal, not the alpha-0.6 reading the argument order
/// invites. That constructor takes `(alpha, red, green, blue)`.
pub fn glow_squid_ink(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    squid_ink_particle(engine, (x, y, z), (xa, ya, za), [0.2, 0.8, 0.6], 1.0);
}

/// Vanilla's own sculk-charge-pop particle — the burst when a sculk charge finishes spreading.
///
/// Vanilla's own explode particle's tick shape ([`Behaviour::Animated`]) over its own
/// four-frame sheet, but **translucent** rather than opaque — which is the whole
/// reason that behaviour carries its layer as a field. `scale(1.0F)` is a no-op
/// on the quad and resets the collision box to the default `0.2`, and the
/// provider then assigns the packet's velocity outright over the constructor's.
pub fn sculk_charge_pop(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
) {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        xa,
        ya,
        za,
        SpriteSource::Sheet {
            sheet: Sheet::SculkChargePop,
            frame: 0,
        },
        rng,
    );
    p.friction = 0.96;
    p.scale(1.0);
    p.has_physics = false;
    p.alpha = 1.0;
    p.xd = xa;
    p.yd = ya;
    p.zd = za;
    p.lifetime = 6 + engine.rng().next_i32_bound(4);
    p.behaviour = Behaviour::Animated {
        layer: Layer::Translucent,
    };
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::SculkChargePop,
        frame: Sheet::SculkChargePop.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

