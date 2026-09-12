use super::*;

/// Vanilla's own crit particle — the sparkle on a critical hit.
pub fn crit(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let p = crit_particle(engine, x, y, z, xa, ya, za, Sheet::CriticalHit);
    engine.add(p);
}

/// Vanilla's own crit-particle magic provider (`minecraft:enchanted_hit`) — the sparkle
/// an enchanted weapon throws instead of the plain white crit.
///
/// The same constructor as [`crit`] over [`Sheet::EnchantedHit`]'s own texture,
/// with the provider's two post-construction tints applied: `rCol *= 0.3F` and
/// `gCol *= 0.8F`, blue untouched. Since the constructor already drew a grey
/// `nextFloat() * 0.3F + 0.6F` into all three channels, the result is a violet
/// mote rather than a recoloured white one — multiplying, not replacing, is the
/// part that matters.
pub fn enchanted_hit(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let mut p = crit_particle(engine, x, y, z, xa, ya, za, Sheet::EnchantedHit);
    p.colour[0] *= 0.3;
    p.colour[1] *= 0.8;
    engine.add(p);
}

/// Vanilla's own crit-particle damage-indicator provider (`minecraft:damage_indicator`) —
/// the mote thrown by a hit that actually dealt damage.
///
/// Two provider-level differences from [`crit`], both easy to lose: the
/// vertical aux is passed **`ya + 1.0`**, so the indicator is launched upward
/// regardless of what the packet asked for, and `setLifetime(20)` *replaces*
/// the constructor's randomised lifetime rather than scaling it. Its sheet is
/// [`Sheet::Damage`], not [`Sheet::CriticalHit`] — `damage_indicator.json`
/// names its own texture.
pub fn damage_indicator(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
) {
    let mut p = crit_particle(engine, x, y, z, xa, ya + 1.0, za, Sheet::Damage);
    p.lifetime = 20;
    engine.add(p);
}

/// Vanilla's own crit-particle constructor, shared by its three providers.
///
/// Returned rather than added so each provider can apply its own
/// post-construction tint or lifetime before the particle goes live — the same
/// split vanilla gets for free by returning the object from its own "create particle" step.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own crit-particle constructor argument for argument, plus the sheet \
              its own particle definition names"
)]
fn crit_particle(
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
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        0.0,
        0.0,
        0.0,
        SpriteSource::Sheet { sheet, frame: 0 },
        rng,
    );
    p.friction = 0.7;
    p.gravity = 0.5;
    // The scattered velocity is damped to a tenth and the *requested* direction
    // added back at 0.4, so a crit mostly follows the hit direction with a small
    // random spray — the opposite balance to a block break.
    p.xd = p.xd.mul_add(f64::from(0.1_f32), xa * 0.4);
    p.yd = p.yd.mul_add(f64::from(0.1_f32), ya * 0.4);
    p.zd = p.zd.mul_add(f64::from(0.1_f32), za * 0.4);
    let col = rng_next(engine).mul_add(0.3, 0.6);
    p.colour = [col, col, col];
    p.quad_size *= 0.75;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (6.0 / f64::from(rng_next(engine)).mul_add(0.8, 0.6)) as i32;
    p.lifetime = lifetime.max(1);
    p.has_physics = false;
    p.behaviour = Behaviour::Crit;
    p
}

