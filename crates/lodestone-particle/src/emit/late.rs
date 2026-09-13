use super::*;

pub fn fly_towards_position(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xd: f64,
    yd: f64,
    zd: f64,
    sheet: Sheet,
) {
    let rng = engine.rng();
    let mut p = Particle::new(x, y, z, SpriteSource::Sheet { sheet, frame: 0 }, rng);
    p.xd = xd;
    p.yd = yd;
    p.zd = zd;
    // `xStart/yStart/zStart` are the *target*, captured before the jump below.
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
    p.behaviour = Behaviour::FlyTowardsPosition;
    let frame = engine.rng().next_i32_bound(i32::from(sheet.frame_count()));
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "bounded by the sheet's frame count"
    )]
    {
        p.sprite = SpriteSource::Sheet {
            sheet,
            frame: frame as u16,
        };
    }
    engine.add(p);
}

/// Vanilla's own drip particle — one particle of the hang → fall → land chain.
///
/// Every difference between vanilla's seventeen drip registry types is in the
/// table below: vanilla's own drip particle itself is one constructor and one
/// tick step, and the four "subclasses" are two hook methods. Reading it as
/// seventeen classes is what makes this look like seventeen ports.
///
/// `vel` is inherited from the previous phase (a hanging drip hands its own
/// velocity to the falling one; a falling one hands zero to the landing one) and
/// is zero for the phase the server itself asked for, which is vanilla's own
/// drip particle's own zero-velocity constructor.
///
/// **`gravity` here is applied raw, not through the base tick's `0.04` scale**
/// — see [`crate::Particle::tick_drip`] — which is why a hanging drip's value
/// is `1.2e-3` and honey's `1.2e-5` rather than the `0.06`-ish numbers the rest
/// of this file uses.
pub fn drip(
    engine: &mut ParticleEngine,
    kind: DripKind,
    phase: DripPhase,
    [x, y, z]: [f64; 3],
    [xd, yd, zd]: [f64; 3],
) {
    let sheet = match phase {
        DripPhase::Hang => Sheet::DripHang,
        DripPhase::Fall => Sheet::DripFall,
        DripPhase::Land => Sheet::DripLand,
    };
    let rng = engine.rng();
    let mut p = Particle::new(x, y, z, SpriteSource::Sheet { sheet, frame: 0 }, rng);
    p.xd = xd;
    p.yd = yd;
    p.zd = zd;
    p.set_size(0.01, 0.01);
    // Every lifetime in the table below that is not a flat number is
    // `(int)(n / (nextFloat() * 0.8 + 0.2))`, so the draw happens once here
    // whether or not it is used — matching vanilla, where the base constructor
    // has already run by the time the provider overrides it.
    let spread = f64::from(rng_next(engine)).mul_add(0.8, 0.2);
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    let varying = |n: f64| (n / spread) as i32;

    let (gravity, lifetime, colour) = match (kind, phase) {
        // Vanilla's own drip-hang particle sets `gravity *= 0.02F` on the base
        // `0.06F` and a flat 40-tick lifetime; the honey and obsidian providers
        // then multiply by a further `0.01F` and raise it to 100.
        (DripKind::Water | DripKind::DripstoneWater, DripPhase::Hang) => {
            (0.0012, 40, WATER_DRIP)
        }
        // The lava hanging phase's colour is recomputed every tick by
        // vanilla's own cooling-drip-hang particle; this is only its first
        // frame, white-hot.
        (DripKind::Lava | DripKind::DripstoneLava, DripPhase::Hang) => {
            (0.0012, 40, [1.0, 1.0, 0.5])
        }
        (DripKind::Honey, DripPhase::Hang) => (0.000_012, 100, [0.622, 0.508, 0.082]),
        (DripKind::ObsidianTear, DripPhase::Hang) => (0.000_012, 100, OBSIDIAN_TEAR),

        // Vanilla's own fall-and-land particle's own `lifetime = (int)(64.0 / …)`,
        // with the base `0.06F` gravity unless the provider overrides it.
        (DripKind::Water | DripKind::DripstoneWater, DripPhase::Fall) => {
            (0.06, varying(64.0), WATER_DRIP)
        }
        (DripKind::Lava | DripKind::DripstoneLava, DripPhase::Fall) => {
            (0.06, varying(64.0), LAVA_DRIP)
        }
        (DripKind::Honey, DripPhase::Fall) => (0.01, varying(64.0), [0.582, 0.448, 0.082]),
        (DripKind::ObsidianTear, DripPhase::Fall) => (0.01, varying(64.0), OBSIDIAN_TEAR),
        (DripKind::Nectar, DripPhase::Fall) => (0.007, varying(16.0), [0.92, 0.782, 0.72]),
        // Vanilla's own bounded-random-float helper called with `(random, 0.1F, 0.9F)`
        // rather than the shared `nextFloat() * 0.8 + 0.2` — a *wider* spread with the same midpoint,
        // so a spore blossom's fall lasts anywhere from 71 to 640 ticks against
        // a nectar drip's 17 to 80.
        (DripKind::SporeBlossom, DripPhase::Fall) => {
            #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
            let lifetime = (64.0 / rng_next(engine).mul_add(0.8, 0.1)) as i32;
            (0.005, lifetime, [0.32, 0.5, 0.22])
        }

        // Vanilla's own drip-land particle, whose numerator is the one thing
        // that differs between the three kinds that have a landing phase: 16
        // for lava, 128 for honey (a honey splat lingers eight times as long),
        // 28 for an obsidian tear.
        (DripKind::Lava | DripKind::DripstoneLava, DripPhase::Land) => {
            (0.06, varying(16.0), LAVA_DRIP)
        }
        (DripKind::Honey, DripPhase::Land) => (0.06, varying(128.0), [0.522, 0.408, 0.082]),
        (DripKind::ObsidianTear, DripPhase::Land) => (0.06, varying(28.0), OBSIDIAN_TEAR),
        // The combinations vanilla has **no provider for**: water lands as a
        // `splash` rather than a drip, and nectar and spore blossom neither
        // hang nor land. Nothing in this crate constructs them —
        // `Particle::tick_drip` chains only into phases that exist — so this
        // arm is reachable only from a caller inventing one, and a one-tick
        // particle is a truer answer there than a panic or a silent
        // full-lifetime one. Enumerated rather than wildcarded so adding a
        // `DripKind` is a compile error listing exactly which phases it needs.
        (
            DripKind::Water | DripKind::DripstoneWater,
            DripPhase::Land,
        )
        | (DripKind::Nectar | DripKind::SporeBlossom, DripPhase::Hang | DripPhase::Land) => {
            (0.06, 1, WATER_DRIP)
        }
    };

    p.gravity = gravity;
    p.lifetime = lifetime.max(1);
    p.colour = colour;
    p.behaviour = Behaviour::Drip { kind, phase };
    engine.add(p);
}

/// Vanilla's own drip-particle water-hang provider's tint — vanilla sets water
/// drips to `0.2F, 0.3F, 1.0F` rather than the biome water colour, so a cave
/// drip reads blue everywhere including in swamp water.
const WATER_DRIP: [f32; 3] = [0.2, 0.3, 1.0];

/// Vanilla's own drip-particle lava-fall provider's tint,
/// `1.0F, 0.2857143F, 0.083333336F` — and also exactly where
/// [`crate::Particle::tick_drip`]'s cooling ramp arrives after 40 ticks,
/// which is the check that the two constants in that formula are
/// transcribed right.
const LAVA_DRIP: [f32; 3] = [1.0, 0.285_714_3, 0.083_333_336];

/// Vanilla's own drip-particle obsidian-tear providers' shared tint,
/// `0.51171875F, 0.03125F, 0.890625F`. All three phases share it, and all
/// three are marked glowing.
const OBSIDIAN_TEAR: [f32; 3] = [0.511_718_75, 0.031_25, 0.890_625];

/// Vanilla's own dust-particle-base colour randomizer — a fresh `nextFloat`
/// draw per call, so three calls (r, g, b) each consume their own random
/// number even though the base brightness factor is shared across all three.
fn randomize_dust_channel(engine: &mut ParticleEngine, channel: f32, base_factor: f32) -> f32 {
    rng_next(engine).mul_add(0.2, 0.8) * channel * base_factor
}

/// Shared dust-particle-base constructor body (26.2 decompile) — the physics
/// and sizing every `minecraft:dust`-family particle has in common.
/// `color` starts at the shared single-quad particle base class's
/// draw-quad-size point (already run by [`Particle::with_velocity`]),
/// matching the constructor order: `super(...)` runs the velocity jitter and
/// quad-size draw, *then* `xd/yd/zd *= 0.1`, *then* the lifetime redraw below.
fn dust_particle(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    scale: f32,
) -> Particle {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        xa,
        ya,
        za,
        SpriteSource::Sheet { sheet: Sheet::Generic, frame: 0 },
        rng,
    );
    p.friction = 0.96;
    p.speed_up_when_y_blocked = true;
    p.xd *= 0.1;
    p.yd *= 0.1;
    p.zd *= 0.1;
    p.quad_size *= 0.75 * scale;
    // base lifetime is 8.0 divided by a uniform double in [0.2, 1.0), truncated to int;
    // then scaled by `scale` and clamped to a floor of 1.0, truncated again.
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    let base_lifetime = (8.0 / engine.rng().next_f64().mul_add(0.8, 0.2)) as i32;
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "mirrors Java's int/float arithmetic and (int) cast"
    )]
    {
        p.lifetime = (base_lifetime as f32 * scale).max(1.0) as i32;
    }
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Generic,
        frame: Sheet::Generic.frame_for_age(0, p.lifetime),
    };
    p
}

/// Vanilla's own dust-particle provider (`minecraft:dust`, the wire particle
/// `minecraft:dust` decodes into).
///
/// `color` is the dust particle option's own colour accessor — the packed
/// RGB24 already unpacked to `[0, 1]` components — and `scale` its shared
/// scalable-particle-option scale. The colour is randomised once here
/// (vanilla's own dust particle's constructor body, which runs *after* its
/// base class's) and held for the particle's whole life; see
/// [`dust_color_transition`] for the sibling that doesn't.
pub fn dust(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    color: [f32; 3],
    scale: f32,
) {
    let mut p = dust_particle(engine, x, y, z, xa, ya, za, scale);
    let base_factor = rng_next(engine).mul_add(0.4, 0.6);
    p.colour = [
        randomize_dust_channel(engine, color[0], base_factor),
        randomize_dust_channel(engine, color[1], base_factor),
        randomize_dust_channel(engine, color[2], base_factor),
    ];
    p.behaviour = Behaviour::Dust;
    engine.add(p);
}

/// Vanilla's own dust-color-transition particle provider
/// (`minecraft:dust_color_transition` — the sculk-sensor/sculk-shrieker particle).
///
/// Same physics as [`dust`]; the colour lerps from `from_color` to `to_color`
/// over the particle's life instead of staying fixed — see
/// [`Behaviour::DustColorTransition`] for how the lerp itself is ticked.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors DustColorTransitionOptions plus position/velocity/engine"
)]
pub fn dust_color_transition(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    from_color: [f32; 3],
    to_color: [f32; 3],
    scale: f32,
) {
    let mut p = dust_particle(engine, x, y, z, xa, ya, za, scale);
    let base_factor = rng_next(engine).mul_add(0.4, 0.6);
    let from = [
        randomize_dust_channel(engine, from_color[0], base_factor),
        randomize_dust_channel(engine, from_color[1], base_factor),
        randomize_dust_channel(engine, from_color[2], base_factor),
    ];
    let to = [
        randomize_dust_channel(engine, to_color[0], base_factor),
        randomize_dust_channel(engine, to_color[1], base_factor),
        randomize_dust_channel(engine, to_color[2], base_factor),
    ];
    p.colour = from;
    p.behaviour = Behaviour::DustColorTransition { from, to };
    engine.add(p);
}

