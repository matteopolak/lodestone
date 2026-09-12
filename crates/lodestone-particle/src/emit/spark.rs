use super::*;

/// Vanilla's own firework spark particle, spawned via its own plain provider —
/// `minecraft:firework`, the plain wire-spawned spark (not the rocket-explosion
/// burst, which is a client-side-only starter/non-rendering particle this
/// client never receives as a wire particle at all).
///
/// From vanilla's own firework-particle source (26.2 decompile): the spark's
/// constructor chains to the simple-animated base constructor with a fixed
/// `0.1F` — that base constructor's third-from-last parameter is **gravity**,
/// not a size scale (confirmed against the simple-animated particle's own
/// constructor, which the [`totem_of_undying`] doc already reads the same
/// way), and that base constructor also hardcodes `friction = 0.91F`
/// unconditionally — the spark particle never overrides either back down the
/// way [`totem_of_undying`]'s totem particle does. Velocity is taken
/// **directly** from the caller with no jitter (`xd = xa` etc., matching the
/// totem particle again), `quadSize *= 0.75F`, `lifetime = 48 + nextInt(12)`,
/// no colour set (stays the base white), and the plain provider's own creation
/// method — the only creation path a plain particle type reaches — sets
/// `alpha = 0.99F` on every instance. `trail`/`twinkle` both default `false`
/// and are never set here; they only matter for the child sparks a rocket's
/// own starter spawns from its own tick step, which is a different,
/// client-only production path this emitter does not model.
pub fn firework(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::Spark,
            frame: 0,
        },
        rng,
    );
    p.friction = 0.91;
    p.gravity = 0.1;
    p.xd = xa;
    p.yd = ya;
    p.zd = za;
    p.quad_size *= 0.75;
    let extra = engine.rng().next_i32_bound(12);
    p.lifetime = 48 + extra;
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Spark,
        frame: Sheet::Spark.frame_for_age(0, p.lifetime),
    };
    p.alpha = 0.99;
    p.behaviour = Behaviour::SimpleAnimated { fade: None };
    engine.add(p);
}

/// Vanilla's own dragon-breath particle — the cloud an ender dragon's breath
/// attack and a lingering potion leave creeping across the ground
/// (`minecraft:dragon_breath`).
///
/// The three velocity words are used **directly**: this class chains to a
/// no-velocity base constructor and then assigns `xd`/`yd`/`zd` itself, so
/// unlike most emitters here nothing jitters, normalises or rescales them.
/// `power` is the particle option's own power accessor, applied by the
/// provider as vanilla's own particle base class's power setter — which
/// rescales `yd` about the `0.1` bias even though this constructor never
/// added one, because that setter is a base-class method and does not know
/// that.
///
/// The tint is drawn per particle out of a narrow purple band and is **not**
/// wire-controlled: vanilla's own bounded-random-float helper called with
/// `(random, 0.7176471F, 0.8745098F)` for red, the same call with `0.0F` for
/// *both* bounds for green (a real draw, not a
/// constant — omitting it desynchronises every later number in the stream),
/// and `0.8235294F..0.9764706F` for blue. Its only payload is the power, which
/// is why `dragon_breath` carries a power-only particle option and not the
/// full colour-carrying spell-particle option.
///
/// `friction = 0.96F`, `quadSize *= 0.75F`,
/// `lifetime = (int)(20.0 / (nextFloat() * 0.8 + 0.2))`, `hasPhysics = false`.
/// See [`Behaviour::DragonBreath`] for the tick, which is a full override.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own dragon-breath particle constructor argument for argument, \
              plus the power its provider reads off the options"
)]
pub fn dragon_breath(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    power: f32,
) {
    /// Vanilla's own bounded-random-float helper — `nextFloat() * (max - min)
    /// + min`, which draws even when the two bounds are equal.
    fn next_float_in(engine: &mut ParticleEngine, min: f32, max: f32) -> f32 {
        rng_next(engine).mul_add(max - min, min)
    }

    let sheet = Sheet::DragonBreath;
    let rng = engine.rng();
    let mut p = Particle::new(x, y, z, SpriteSource::Sheet { sheet, frame: 0 }, rng);
    p.friction = 0.96;
    p.xd = xa;
    p.yd = ya;
    p.zd = za;
    p.colour = [
        next_float_in(engine, 0.717_647_1, 0.874_509_8),
        next_float_in(engine, 0.0, 0.0),
        next_float_in(engine, 0.823_529_4, 0.976_470_6),
    ];
    p.quad_size *= 0.75;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    {
        p.lifetime = (20.0 / f64::from(rng_next(engine).mul_add(0.8, 0.2))) as i32;
    }
    p.has_physics = false;
    p.behaviour = Behaviour::DragonBreath { hit_ground: false };
    p.sprite = SpriteSource::Sheet {
        sheet,
        frame: sheet.frame_for_age(0, p.lifetime),
    };
    p.set_power(power);
    engine.add(p);
}

/// Vanilla's own sculk-charge particle — the mote a spreading sculk charge
/// leaves behind (`minecraft:sculk_charge`).
///
/// Its own emitter rather than an [`animated_ambient`] call, because three of
/// the things its provider does are not that function's shape: the roll comes
/// off the wire (the particle option's own roll field, which is what makes a
/// charge's motes lie along the direction it is spreading instead of all
/// sharing one orientation), the lifetime is a per-particle draw
/// (`random.nextInt(12) + 8`) rather than a constant, and the provider
/// overwrites the jittered velocity outright with its own particle-speed
/// setter — so the packet's three velocity words really are the velocity
/// here, unlike in the base constructor that scattered them.
///
/// `scale(1.5F)`, `friction = 0.96F`, `hasPhysics = false`, `setAlpha(1.0F)`.
/// Its light-coordinate accessor applies a *boost* of 15 over the sampled
/// world light rather than a bare full-bright constant, which this crate does
/// not model for any behaviour, so a charge in the dark comes out dimmer than
/// vanilla and never brighter.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own sculk-charge particle constructor argument for argument, \
              plus the roll its provider reads off the options"
)]
pub fn sculk_charge(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    roll: f32,
) {
    let sheet = Sheet::SculkCharge;
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        xa,
        ya,
        za,
        SpriteSource::Sheet { sheet, frame: 0 },
        rng,
    );
    p.friction = 0.96;
    p.scale(1.5);
    p.has_physics = false;
    p.alpha = 1.0;
    // Vanilla's own particle-speed setter — the provider discards the
    // jitter `with_velocity` just applied and installs the packet's own words.
    p.xd = xa;
    p.yd = ya;
    p.zd = za;
    p.roll = roll;
    p.o_roll = roll;
    p.lifetime = engine.rng().next_i32_bound(12) + 8;
    // `Animated`, not `AshSmoke`: vanilla's own sculk-charge particle overrides
    // neither its size-at-age nor its render-layer default the way vanilla's
    // own base ash-smoke particle does, so borrowing `AshSmoke` here would add
    // a `* 32` fade-in it does not have — and its layer is `TRANSLUCENT`.
    p.behaviour = Behaviour::Animated {
        layer: Layer::Translucent,
    };
    p.sprite = SpriteSource::Sheet {
        sheet,
        frame: sheet.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

/// An animated ambient sheet with ordinary physics — `minecraft:gust`,
/// `minecraft:small_gust` and `minecraft:sonic_boom`, which differ from each
/// other in sheet, scale and lifetime rather than in tick shape.
///
/// [`Behaviour::AshSmoke`] again for [`soul`]'s reason: it means "advance the
/// sheet by age", which is all vanilla's own sprite-from-age step does in each
/// of these particles.
pub fn animated_ambient(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    sheet: Sheet,
    scale: f32,
    lifetime: i32,
) {
    let rng = engine.rng();
    let mut p = Particle::new(x, y, z, SpriteSource::Sheet { sheet, frame: 0 }, rng);
    p.friction = 0.96;
    p.gravity = 0.0;
    p.xd = xa;
    p.yd = ya;
    p.zd = za;
    p.scale(scale);
    p.lifetime = lifetime.max(1);
    p.behaviour = Behaviour::AshSmoke;
    p.sprite = SpriteSource::Sheet {
        sheet,
        frame: sheet.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

/// Vanilla's own fly-towards-position particle — the enchanting-table glyphs
/// (`enchant`, over [`Sheet::Enchant`]'s twenty-six Standard Galactic letters)
/// and the conduit's homing mote (`nautilus`). Vanilla's own enchant and
/// nautilus providers are byte-identical apart from the sprite set.
///
/// `xd/yd/zd` are an **offset**, not a velocity: the caller passes the point the
/// mote should fly *from*, relative to `x/y/z`, and the constructor immediately
/// teleports the particle to `pos + offset` so its first drawn frame is already
/// out at the bookshelf. Getting this backwards puts every glyph inside the
/// table. [`Behaviour::FlyTowardsPosition`] documents the flight curve.
///
/// The frame is drawn once at construction (`sprite.get(random)` — a uniform
/// pick, not an age ramp), which is what makes a bookshelf emit a spread of
/// different letters rather than the whole shelf spelling the same one.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own fly-towards-position particle constructor argument for \
              argument, plus the sheet its provider supplies"
)]
