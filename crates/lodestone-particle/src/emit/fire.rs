use super::*;

fn rising(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xd: f64,
    yd: f64,
    zd: f64,
    sheet: Sheet,
) -> Particle {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(x, y, z, xd, yd, zd, SpriteSource::Sheet { sheet, frame: 0 }, rng);
    p.friction = 0.96;
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
    p.spawn = [p.x, p.y, p.z];
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(8.0_f32) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32 + 4;
    p.lifetime = lifetime;
    p
}

/// `minecraft:soul_fire_flame` — vanilla's own flame-particle provider over the
/// `soul_fire_flame` sprite, so the physics are `flame`'s exactly and only the
/// sheet differs.
pub fn soul_fire_flame(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xd: f64, yd: f64, zd: f64) {
    let mut p = rising(engine, x, y, z, xd, yd, zd, Sheet::SoulFireFlame);
    p.behaviour = Behaviour::Flame;
    engine.add(p);
}

/// `minecraft:copper_fire_flame` — vanilla's own flame-particle provider again,
/// over [`Sheet::CopperFireFlame`]'s own texture. Three registry types share
/// that one provider across three different sheets; the provider never decides.
pub fn copper_fire_flame(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xd: f64,
    yd: f64,
    zd: f64,
) {
    let mut p = rising(engine, x, y, z, xd, yd, zd, Sheet::CopperFireFlame);
    p.behaviour = Behaviour::Flame;
    engine.add(p);
}

/// Vanilla's own flame-particle small-flame provider (`minecraft:small_flame`) —
/// a candle flame. `flame`'s sheet and physics with a single `scale(0.5F)`.
///
/// `scale` shrinks the **collision box as well as** the quad
/// (`setSize(0.2 * scale, 0.2 * scale)`), which is why it is one call rather
/// than a `quad_size` multiply — and why a small flame does not clip a candle's
/// wick the way a half-sized quad on a full-sized box would.
pub fn small_flame(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xd: f64, yd: f64, zd: f64) {
    let mut p = rising(engine, x, y, z, xd, yd, zd, Sheet::Flame);
    p.scale(0.5);
    p.behaviour = Behaviour::Flame;
    engine.add(p);
}

/// Vanilla's own soul particle — a rising, sheet-animated mote, 1.5× scale and
/// translucent.
///
/// [`Behaviour::AshSmoke`] is the right behaviour despite the name: what that
/// variant *does* is "ordinary physics, advance the sheet by age", which is
/// vanilla's own soul-particle tick step's own `super.tick(); setSpriteFromAge(sprites);`
/// verbatim. Unlike `flame` it does **not** override its move step, so a soul mote
/// collides.
pub fn soul(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xd: f64, yd: f64, zd: f64) {
    soul_over(engine, x, y, z, xd, yd, zd, Sheet::Soul);
}

/// Vanilla's own soul-particle emissive provider (`minecraft:sculk_soul`) — the
/// mote a sculk catalyst throws.
///
/// The same constructor as [`soul`] over **its own sheet**: `sculk_soul.json` names
/// `sculk_soul_0`…`sculk_soul_10`, not `soul_N`, and only the eleven-frame
/// count coincides. The provider's other two acts — setting alpha to `1.0`
/// and marking it glowing — are respectively already the constructor's value
/// and a fixed-brightness light boost this crate does not model (see
/// `ParticleEngine::extract`'s light arm, which records that omission for
/// the whole family rather than per emitter).
pub fn sculk_soul(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xd: f64, yd: f64, zd: f64) {
    soul_over(engine, x, y, z, xd, yd, zd, Sheet::SculkSoul);
}

/// Vanilla's own soul particle's constructor, over whichever sheet the registry type names.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own soul-particle constructor argument for argument, plus its sheet"
)]
fn soul_over(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xd: f64,
    yd: f64,
    zd: f64,
    sheet: Sheet,
) {
    let mut p = rising(engine, x, y, z, xd, yd, zd, sheet);
    p.scale(1.5);
    p.alpha = 1.0;
    p.behaviour = Behaviour::AshSmoke;
    p.sprite = SpriteSource::Sheet {
        sheet,
        frame: sheet.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

/// Vanilla's own portal particle — the nether-portal / ender shimmer.
///
/// `xd/yd/zd` here are an **amplitude**, not a velocity: [`Behaviour::Portal`]
/// recomputes the position from [`Particle::spawn`] every tick and never damps
/// them. The caller passes the offset the mote should converge *from*, which for
/// a portal block is a unit-normal-distributed offset and for an
/// enderman's/chorus-fruit teleports is the distance travelled.
pub fn portal(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xd: f64, yd: f64, zd: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::PortalGeneric,
            frame: 0,
        },
        rng,
    );
    p.xd = xd;
    p.yd = yd;
    p.zd = zd;
    p.spawn = [x, y, z];
    p.quad_size = 0.1 * rng_next(engine).mul_add(0.2, 0.5);
    let br = rng_next(engine).mul_add(0.6, 0.4);
    p.colour = [br * 0.9, br * 0.3, br];
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    let lifetime = (rng_next(engine) * 10.0) as i32 + 40;
    p.lifetime = lifetime;
    p.behaviour = Behaviour::Portal;
    engine.add(p);
}

/// Vanilla's own campfire-smoke particle — the tall column over a campfire.
///
/// `signal` picks between the two lifetimes, and they are far apart on purpose:
/// `rand(50) + 80` cosy against `rand(50) + 280` signal, which is the whole
/// reason a signal fire's plume reaches above the treeline. Both providers draw
/// a random frame from the sprite set once per particle rather than pinning the
/// first `big_smoke` sprite; their alpha differs too (`0.9` cosy, `0.95` signal).
pub fn campfire_smoke(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    signal: bool,
) {
    let frame = u16::try_from(
        engine
            .rng()
            .next_i32_bound(i32::from(Sheet::BigSmoke.frame_count())),
    )
    .expect("the big-smoke sprite set has fewer than u16::MAX frames");
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::BigSmoke,
            frame,
        },
        rng,
    );
    p.scale(3.0);
    p.set_size(0.25, 0.25);
    let base = if signal { 280 } else { 80 };
    p.lifetime = engine.rng().next_i32_bound(50) + base;
    p.gravity = 3.0e-6;
    p.xd = xa;
    p.yd = ya + f64::from(rng_next(engine)) / 500.0;
    p.zd = za;
    p.alpha = if signal { 0.95 } else { 0.9 };
    p.behaviour = Behaviour::CampfireSmoke;
    engine.add(p);
}

/// Vanilla's own end-rod particle — a simple-animated particle at
/// `gravity = 0.0125` that fades toward `0xF2E9C9` and, like the flame, passes
/// through the block it sits on.
pub fn end_rod(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::Glitter,
            frame: 0,
        },
        rng,
    );
    p.friction = 0.91;
    p.gravity = 0.0125;
    p.xd = xa;
    p.yd = ya;
    p.zd = za;
    p.quad_size *= 0.75;
    p.lifetime = 60 + engine.rng().next_i32_bound(12);
    // `has_physics = false` rather than `Behaviour::Flame`: vanilla overrides
    // its own move step to skip collision but keeps the ordinary base tick, and
    // the `Flame` behaviour would take flame's own quad-size curve with it.
    p.has_physics = false;
    p.behaviour = Behaviour::SimpleAnimated {
        // vanilla's own fade-colour setter is passed `15916745` == `0xF2D9C9`,
        // split the way it splits it: each channel `/ 255`.
        fade: Some([0xF2 as f32 / 255.0, 0xDE as f32 / 255.0, 0xC9 as f32 / 255.0]),
    };
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Glitter,
        frame: Sheet::Glitter.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

/// Vanilla's own glow particle — the shared base for five registry types, all
/// over `particle/glow`: `electric_spark`, `glow` (the glow squid's shimmer),
/// `scrape`, `wax_on` and `wax_off`.
///
/// Vanilla's own particle-resource registration is the only thing that says
/// so. `electric_spark`/`glow` were previously emitted here by an
/// approximation that took vanilla's own firework spark particle's shape
/// (`friction 0.9`, a `8 + nextInt(4)` lifetime, no tint, collision left on) —
/// close enough to look right in isolation and wrong in every constant:
/// vanilla's own glow particle uses `friction 0.96`, its own speed-up-when-Y-blocked
/// flag, `hasPhysics = false`, and a per-provider tint and lifetime that differ
/// by an order of magnitude between them (2–3 ticks for an electric spark
/// against 10–39 for a scrape).
///
/// Returned rather than added so each provider can set its own speed, tint and
/// lifetime — which for this family is the *whole* difference between the five.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own glow-particle constructor argument for argument, plus its sheet"
)]
pub fn glow_particle(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    sheet: Sheet,
) -> Particle {
    let rng = engine.rng();
    let mut p =
        Particle::with_velocity(x, y, z, xa, ya, za, SpriteSource::Sheet { sheet, frame: 0 }, rng);
    p.friction = 0.96;
    p.speed_up_when_y_blocked = true;
    p.quad_size *= 0.75;
    p.has_physics = false;
    // `particle/glow` is a single frame, so vanilla's own sprite-from-age step
    // is a no-op — the behaviour is still `Animated` because the particle
    // still advances its sheet, and a resource pack is free to give
    // `glow.json` more than one frame.
    p.behaviour = Behaviour::Animated { layer: Layer::Opaque };
    p
}

/// Vanilla's own glow-particle electric-spark provider — the arc a lightning rod throws.
///
/// The shortest-lived particle in this family by a wide margin: `nextInt(2) + 2`,
/// i.e. two or three ticks. Its velocity is the packet's, scaled to a quarter,
/// assigned outright rather than added to the constructor's scatter.
pub fn electric_spark(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let mut p = glow_particle(engine, x, y, z, 0.0, 0.0, 0.0, Sheet::Glow);
    p.colour = [1.0, 0.9, 1.0];
    p.xd = xa * 0.25;
    p.yd = ya * 0.25;
    p.zd = za * 0.25;
    p.lifetime = 2 + engine.rng().next_i32_bound(2);
    engine.add(p);
}

/// Vanilla's own glow-particle glow-squid provider (`minecraft:glow`) — the
/// shimmer around a glow squid.
///
/// The one provider in this family that does *not* assign its velocity: it feeds
/// `0.5 - nextDouble()` horizontally into the constructor's own scatter, damps
/// `yd` to a fifth, and damps `xd`/`zd` a further tenth when the caller asked
/// for no horizontal motion — the same shape vanilla's own spell particle uses,
/// tested against the **original** arguments rather than the jittered ones.
///
/// Its tint is a coin flip between two greens, drawn from a `nextBoolean()`, so
/// a school of them reads as two populations rather than one colour.
pub fn glow_squid(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let rng = engine.rng();
    let jitter_x = 0.5 - rng.next_f64();
    let jitter_z = 0.5 - rng.next_f64();
    let mut p = glow_particle(engine, x, y, z, jitter_x, ya, jitter_z, Sheet::Glow);
    p.colour = if engine.rng().next_bool() {
        [0.6, 1.0, 0.8]
    } else {
        [0.08, 0.4, 0.4]
    };
    p.yd *= f64::from(0.2_f32);
    if xa == 0.0 && za == 0.0 {
        p.xd *= f64::from(0.1_f32);
        p.zd *= f64::from(0.1_f32);
    }
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    let lifetime = (8.0 / engine.rng().next_f64().mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime.max(1);
    engine.add(p);
}

/// The three copper-oxidation sparkles: `scrape`, `wax_on` and `wax_off`.
///
/// All vanilla's own glow particle with a `nextInt(30) + 10` lifetime and a `0.01` speed
/// factor; they differ only in tint and in whether the horizontal speed is
/// halved. `scrape` flips a coin between two teals (the oxide it removed);
/// `wax_on` is honey-orange and `wax_off` the same pale white as an electric
/// spark.
fn copper_sparkle(
    engine: &mut ParticleEngine,
    (x, y, z): (f64, f64, f64),
    (xa, ya, za): (f64, f64, f64),
    colour: [f32; 3],
    halve_horizontal: bool,
) {
    let mut p = glow_particle(engine, x, y, z, 0.0, 0.0, 0.0, Sheet::Glow);
    p.colour = colour;
    let horizontal = if halve_horizontal { 0.01 / 2.0 } else { 0.01 };
    p.xd = xa * horizontal;
    p.yd = ya * 0.01;
    p.zd = za * horizontal;
    p.lifetime = 10 + engine.rng().next_i32_bound(30);
    engine.add(p);
}

/// Vanilla's own glow-particle scrape provider — an axe stripping oxidation off
/// copper. Full horizontal speed, unlike its two wax siblings.
pub fn scrape(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let colour = if engine.rng().next_bool() {
        [0.29, 0.58, 0.51]
    } else {
        [0.43, 0.77, 0.62]
    };
    copper_sparkle(engine, (x, y, z), (xa, ya, za), colour, false);
}

/// Vanilla's own glow-particle wax-on provider — honeycomb applied to copper.
pub fn wax_on(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    copper_sparkle(engine, (x, y, z), (xa, ya, za), [0.91, 0.55, 0.08], true);
}

/// Vanilla's own glow-particle wax-off provider — an axe removing that wax.
pub fn wax_off(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    copper_sparkle(engine, (x, y, z), (xa, ya, za), [1.0, 0.9, 1.0], true);
}

