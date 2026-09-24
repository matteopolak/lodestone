use super::*;

/// Vanilla's own suspended-town-decoration particle — the ambient-speck family: the villager mood icons,
/// `mycelium`'s brown motes, a composter's white puff, an `egg_crack`, and a
/// dolphin's speed trail.
///
/// Returned rather than added so each provider can apply its own tint, alpha
/// and lifetime override first. Five registry types reach this over **two**
/// sheets — `glint` for the ones that read as a sparkle, `generic_0` for the
/// ones that read as dust — so the sheet is a parameter, as always.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own suspended-town-decoration particle constructor argument for argument, plus \
              the sheet its provider supplies"
)]
pub fn suspended_town(
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
        xa,
        ya,
        za,
        SpriteSource::Sheet { sheet, frame: 0 },
        rng,
    );
    let br = rng_next(engine).mul_add(0.1, 0.2);
    p.colour = [br, br, br];
    p.set_size(0.02, 0.02);
    p.quad_size *= rng_next(engine).mul_add(0.6, 0.5);
    let damp = f64::from(0.02_f32);
    p.xd *= damp;
    p.yd *= damp;
    p.zd *= damp;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (20.0 / f64::from(rng_next(engine).mul_add(0.8, 0.2))) as i32;
    p.lifetime = lifetime;
    p.behaviour = Behaviour::Suspended;
    p
}

/// Vanilla's own suspended-town-decoration particle provider (`minecraft:mycelium`) — the brown
/// motes drifting off a mycelium block. No tint override, so the constructor's
/// own dim grey (`nextFloat() * 0.1 + 0.2`) stands.
pub fn mycelium(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let p = suspended_town(engine, x, y, z, xa, ya, za, Sheet::Generic0);
    engine.add(p);
}

/// Vanilla's own suspended-town-decoration particle composter-fill provider — the puff when a composter
/// takes an item. White, and far shorter-lived than its siblings:
/// `3 + nextInt(5)` ticks against the constructor's ~20–100.
pub fn composter(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let mut p = suspended_town(engine, x, y, z, xa, ya, za, Sheet::Glint);
    p.colour = [1.0, 1.0, 1.0];
    p.lifetime = 3 + engine.rng().next_i32_bound(5);
    engine.add(p);
}

/// Vanilla's own suspended-town-decoration particle egg-crack provider — the flecks off a hatching turtle
/// egg. The constructor's grey replaced by white, and nothing else.
pub fn egg_crack(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let mut p = suspended_town(engine, x, y, z, xa, ya, za, Sheet::Glint);
    p.colour = [1.0, 1.0, 1.0];
    engine.add(p);
}

/// Vanilla's own suspended-town-decoration particle dolphin-speed provider — the blue trail behind a
/// player riding Dolphin's Grace.
///
/// A per-particle **alpha** draw (`1 - nextFloat() * 0.7`) as well as a tint, so
/// the trail is a spread of translucencies rather than a uniform ribbon, and
/// half the constructor's lifetime.
pub fn dolphin(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let mut p = suspended_town(engine, x, y, z, xa, ya, za, Sheet::Generic0);
    p.colour = [0.3, 0.5, 1.0];
    p.alpha = rng_next(engine).mul_add(-0.7, 1.0);
    p.lifetime /= 2;
    engine.add(p);
}

/// Vanilla's own suspended particle — the *other* ambient-speck class, and not a variant of
/// [`suspended_town`] despite the name.
///
/// Four differences that matter: it is spawned **`0.125` blocks below** the
/// requested `y`, it has no tick override at all (ordinary physics, with
/// `friction = 1.0` and `gravity = 0.0` so it neither slows nor falls), its
/// lifetime numerator is `16` rather than `20`, and its quad-size jitter
/// depends on **which constructor** ran: `nextFloat() * 0.6 + 0.2` for the
/// zero-velocity one and `+ 0.6` for the one taking a velocity. `velocity` is
/// `None` to select the former.
pub fn suspended(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    velocity: Option<(f64, f64, f64)>,
    sheet: Sheet,
) -> Particle {
    let y = y - 0.125;
    let rng = engine.rng();
    let sprite = SpriteSource::Sheet { sheet, frame: 0 };
    let (mut p, quad_bias) = match velocity {
        Some((xd, yd, zd)) => (
            Particle::with_velocity(x, y, z, xd, yd, zd, sprite, rng),
            0.6,
        ),
        None => (Particle::new(x, y, z, sprite, rng), 0.2),
    };
    p.set_size(0.01, 0.01);
    p.quad_size *= rng_next(engine).mul_add(0.6, quad_bias);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (16.0 / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime.max(1);
    p.has_physics = false;
    p.friction = 1.0;
    p.gravity = 0.0;
    p.behaviour = Behaviour::Plain;
    p
}

/// Vanilla's own suspended-particle underwater provider — the pale motes suspended in ocean
/// water. The **zero-velocity** constructor, so it hangs exactly where it
/// spawned.
pub fn underwater(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let mut p = suspended(engine, x, y, z, None, Sheet::Generic0);
    p.colour = [0.4, 0.4, 0.7];
    engine.add(p);
}

/// Vanilla's own suspended-particle crimson-spore provider — the pink drift of a crimson
/// forest. Its velocity is three gaussians at wildly different scales: `1e-6`
/// horizontally against `1e-4` vertically, i.e. essentially a slow vertical
/// wander with no lateral motion at all.
pub fn crimson_spore(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let (xa, za) = (gaussian(engine) * 1e-6, gaussian(engine) * 1e-6);
    let ya = gaussian(engine) * 1e-4;
    let mut p = suspended(engine, x, y, z, Some((xa, ya, za)), Sheet::Generic0);
    p.colour = [0.9, 0.4, 0.5];
    engine.add(p);
}

/// Vanilla's own suspended-particle warped-spore provider — the blue drift of a warped forest.
/// Purely vertical (`nextFloat() * -1.9 * nextFloat() * 0.1`, always downward)
/// and a tenth of a crimson spore's collision box.
pub fn warped_spore(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let rng = engine.rng();
    let ya = f64::from(rng.next_f32()) * -1.9 * f64::from(rng.next_f32()) * 0.1;
    let mut p = suspended(engine, x, y, z, Some((0.0, ya, 0.0)), Sheet::Generic0);
    p.colour = [0.1, 0.1, 0.3];
    p.set_size(0.001, 0.001);
    engine.add(p);
}

/// Vanilla's own suspended-particle spore-blossom-air provider — the green motes hanging under
/// a spore blossom.
///
/// **This is a suspended particle, not a drip particle.** It shares
/// `drip_fall`'s *texture* with `falling_spore_blossom` and nothing else: it
/// hangs in the air rather than falling to a splash, its lifetime is a flat
/// `500..=1000` ticks rather than a `64 / nextFloat` draw, and it carries a
/// `0.01` gravity of its own. The sheet stem is what makes the two look
/// interchangeable, which is the same trap `Sheet::Spell` documents one level
/// up.
pub fn spore_blossom_air(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let mut p = suspended(engine, x, y, z, Some((0.0, -0.8, 0.0)), Sheet::DripFall);
    p.lifetime = 500 + engine.rng().next_i32_bound(501);
    p.gravity = 0.01;
    p.colour = [0.32, 0.5, 0.22];
    engine.add(p);
}

/// Vanilla's own explode particle — the puff a mob leaves when it dies, a spawner throws
/// when it spawns, and an animal throws when it breeds (`poof`); and a llama's
/// `spit`.
///
/// [`Behaviour::Animated`] rather than [`Behaviour::AshSmoke`]: this class
/// advances its sheet by age like the ash-smoke family but does **not**
/// override its own quad-size accessor, so a puff is full size from its first frame.
///
/// Its quad size is `0.1 * (nextFloat() * nextFloat() * 6 + 1)` — a *product*
/// of two draws, which biases the distribution hard towards small puffs with an
/// occasional large one, unlike the uniform jitters elsewhere in this file.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own explode-particle constructor argument for argument, plus its sheet"
)]
pub fn explode(
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
    let mut p = Particle::new(x, y, z, SpriteSource::Sheet { sheet, frame: 0 }, rng);
    p.gravity = -0.1;
    p.friction = 0.9;
    let scatter = |e: &mut ParticleEngine| f64::from(rng_next(e).mul_add(2.0, -1.0) * 0.05);
    p.xd = xa + scatter(engine);
    p.yd = ya + scatter(engine);
    p.zd = za + scatter(engine);
    let col = rng_next(engine).mul_add(0.3, 0.7);
    p.colour = [col, col, col];
    p.quad_size = 0.1 * (rng_next(engine) * rng_next(engine)).mul_add(6.0, 1.0);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (16.0 / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32 + 2;
    p.lifetime = lifetime;
    p.behaviour = Behaviour::Animated { layer: Layer::Opaque };
    p.sprite = SpriteSource::Sheet {
        sheet,
        frame: sheet.frame_for_age(0, p.lifetime),
    };
    p
}

/// Vanilla's own explode-particle provider (`minecraft:poof`) — the death, breeding and
/// spawn puff, and one of the most frequently spawned particles in the game.
pub fn poof(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let p = explode(engine, x, y, z, xa, ya, za, Sheet::Generic);
    engine.add(p);
}

/// Vanilla's own spit particle — the explode particle with `gravity = 0.5F` instead of `-0.1F`,
/// so a llama's spit arcs down rather than drifting up. One number, opposite
/// sign, six times the magnitude.
pub fn spit(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let mut p = explode(engine, x, y, z, xa, ya, za, Sheet::Generic);
    p.gravity = 0.5;
    engine.add(p);
}
