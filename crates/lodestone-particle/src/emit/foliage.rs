use super::*;

/// The provider constants that separate `cherry_leaves`, `pale_oak_leaves` and
/// `tinted_leaves`, which are otherwise one class.
///
/// Grouped rather than passed loose because the three sets differ in *five*
/// numbers at once and a transposed pair is invisible: cherry alone flows away
/// and does not swirl, and it is the only one with a zero start velocity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeafParams {
    /// Vanilla's own "fall acceleration" field.
    pub fall_acceleration: f32,
    /// Vanilla's own "side acceleration" field, i.e. its own "wind big" field.
    pub side_acceleration: f32,
    /// Whether the swirl term is enabled.
    pub swirl: bool,
    /// Whether the flow-away term is enabled.
    pub flow_away: bool,
    /// Multiplier on the quad size.
    pub scale: f32,
    /// Initial downward speed, applied as `yd = -start_velocity`.
    pub start_velocity: f32,
    /// Which sheet the leaf is drawn from.
    pub sheet: Sheet,
}

impl LeafParams {
    /// Vanilla's own falling-leaves-particle cherry provider —
    /// `(0.25, 2.0, swirl=false, flowAway=true, 1.0, 0.0)`.
    #[must_use]
    pub const fn cherry() -> Self {
        Self {
            fall_acceleration: 0.25,
            side_acceleration: 2.0,
            swirl: false,
            flow_away: true,
            scale: 1.0,
            start_velocity: 0.0,
            sheet: Sheet::CherryLeaves,
        }
    }

    /// Vanilla's own falling-leaves-particle pale-oak provider —
    /// `(0.07, 10.0, swirl=true, flowAway=false, 2.0, 0.021)`.
    #[must_use]
    pub const fn pale_oak() -> Self {
        Self {
            fall_acceleration: 0.07,
            side_acceleration: 10.0,
            swirl: true,
            flow_away: false,
            scale: 2.0,
            start_velocity: 0.021,
            sheet: Sheet::PaleOakLeaves,
        }
    }

    /// Vanilla's own falling-leaves-particle tinted-leaves provider — the pale-oak constants
    /// exactly, on the untinted `leaf_N` sheet and with a wire colour.
    #[must_use]
    pub const fn tinted() -> Self {
        Self {
            sheet: Sheet::TintedLeaves,
            ..Self::pale_oak()
        }
    }
}

/// Vanilla's own falling-leaves particle — the drifting leaves under a cherry or pale-oak
/// canopy, and the tinted variant a resource pack can colour.
///
/// `colour` is `None` for the two simple particle-type variants and `Some` for
/// `tinted_leaves`, whose provider calls its own "set color" step from its
/// own colour particle-option payload.
///
/// The draw order is Java's: the provider picks the sprite first, then the base
/// constructor runs, then the two instance-field initialisers (vanilla's own
/// rotation-speed and spin-acceleration fields) — which in Java execute
/// **after** the superclass constructor — then the constructor body's size
/// and flow draws.
pub fn falling_leaves(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    params: LeafParams,
    colour: Option<[f32; 3]>,
) {
    /// Vanilla's own falling-leaves particle's acceleration-scale constant.
    const ACCELERATION_SCALE: f32 = 0.0025;

    // `this.sprites.get(random)` in the provider, before the constructor.
    let frame = engine.rng().next_i32_bound(i32::from(params.sheet.frame_count()));
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: params.sheet,
            frame: 0,
        },
        rng,
    );
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "bounded by the sheet's frame count"
    )]
    {
        p.sprite = SpriteSource::Sheet {
            sheet: params.sheet,
            frame: frame as u16,
        };
    }
    // The two field initialisers, in declaration order.
    let rot_speed = if engine.rng().next_bool() { -30.0_f32 } else { 30.0 }.to_radians();
    let spin_acceleration = if engine.rng().next_bool() { -5.0_f32 } else { 5.0 }.to_radians();

    p.lifetime = 300;
    p.gravity = params.fall_acceleration * 1.2 * ACCELERATION_SCALE;
    let size = params.scale * if engine.rng().next_bool() { 0.05 } else { 0.075 };
    p.quad_size = size;
    p.set_size(size, size);
    p.friction = 1.0;
    p.yd = f64::from(-params.start_velocity);
    let particle_random = rng_next(engine);
    // full-precision `f64` trig, not the quantized sine table used elsewhere in this
    // file — this particular call site uses library trig on a double, not the table.
    let radians = f64::from(particle_random * 60.0).to_radians();
    let xa_flow_scale = radians.cos() * f64::from(params.side_acceleration);
    let za_flow_scale = radians.sin() * f64::from(params.side_acceleration);
    let swirl_period = f64::from(particle_random.mul_add(3000.0, 1000.0)).to_radians();
    if let Some(colour) = colour {
        p.colour = colour;
    }
    p.behaviour = Behaviour::FallingLeaves {
        wind_big: params.side_acceleration,
        swirl: params.swirl,
        flow_away: params.flow_away,
        xa_flow_scale,
        za_flow_scale,
        swirl_period,
        rot_speed,
        spin_acceleration,
    };
    engine.add(p);
}

/// Vanilla's own firefly particle — the firefly bush's drifting mote.
///
/// The provider builds the velocity itself rather than passing the packet's
/// through: `x` and `z` are `0.5 - nextDouble()` regardless of what the server
/// sent, and only `ya` survives — with a coin flip on its **sign**, which is
/// what makes half the swarm rise and half sink from one emission.
pub fn firefly(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, ya: f64) {
    let xa = 0.5 - engine.rng().next_f64();
    let ya = if engine.rng().next_bool() { ya } else { -ya };
    let za = 0.5 - engine.rng().next_f64();
    let frame = engine.rng().next_i32_bound(i32::from(Sheet::Firefly.frame_count()));
    let rng = engine.rng();
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "bounded by the sheet's frame count"
    )]
    let sprite = SpriteSource::Sheet {
        sheet: Sheet::Firefly,
        frame: frame as u16,
    };
    let mut p = Particle::with_velocity(x, y, z, xa, ya, za, sprite, rng);
    p.speed_up_when_y_blocked = true;
    p.friction = 0.96;
    p.quad_size *= 0.75;
    p.yd *= f64::from(0.8_f32);
    p.xd *= f64::from(0.8_f32);
    p.zd *= f64::from(0.8_f32);
    // `random.nextIntBetweenInclusive(200, 300)`.
    p.lifetime = 200 + engine.rng().next_i32_bound(101);
    p.scale(1.5);
    // The provider's own `setAlpha(0.0F)`: a firefly is invisible on the tick
    // it spawns and the ramp brings it up from there.
    p.alpha = 0.0;
    p.behaviour = Behaviour::Firefly;
    engine.add(p);
}

/// Vanilla's own breaking-item particle's **four**-argument constructor — the one
/// `item_slime`, `item_cobweb` and `item_snowball` reach.
///
/// Those three are simple particle types with no wire payload at all: each
/// provider hardcodes its own item (`minecraft:slime_ball`, `minecraft:cobweb`,
/// `minecraft:snowball`) and calls this constructor, so the shell supplies the
/// registry id and nothing comes off the wire.
///
/// **Not [`item_particle`]**, which is the seven-argument sibling: that one
/// additionally damps the constructor's jitter to a tenth and adds the caller's
/// velocity on top. Routing these three through it leaves their crumbs
/// essentially motionless, since the velocity they would add is zero and the
/// jitter is all they have.
#[must_use]
pub fn item_burst_particle(
    x: f64,
    y: f64,
    z: f64,
    item: lodestone_data::item::Item,
    rng: &mut JavaRandom,
) -> Particle {
    let mut p = Particle::with_velocity(x, y, z, 0.0, 0.0, 0.0, SpriteSource::Item(item), rng);
    p.gravity = 1.0;
    p.quad_size /= 2.0;
    // Two more draws, *after* the quad size — order matters for replay, exactly
    // as in `terrain_particle` and `item_particle`.
    let uo = rng.next_f32() * 3.0;
    let vo = rng.next_f32() * 3.0;
    p.behaviour = Behaviour::Terrain { uo, vo };
    p
}

/// Vanilla's own firework-particles overlay particle — the white bloom a firework star paints
/// over its own burst, which the server sends as `minecraft:flash`.
///
/// Its `colour` and `alpha` both come off the wire as a colour-carrying
/// particle option, and its size and alpha are then functions of age alone — see
/// [`Behaviour::FireworkFlash`], whose curves start *negative* by design.
pub fn flash(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, colour: [f32; 4]) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::Flash,
            frame: 0,
        },
        rng,
    );
    p.lifetime = 4;
    p.colour = [colour[0], colour[1], colour[2]];
    p.alpha = colour[3];
    p.behaviour = Behaviour::FireworkFlash;
    engine.add(p);
}

