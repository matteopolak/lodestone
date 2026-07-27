//! Vanilla's particle emitters.
//!
//! Each function here is a transcription of one method that vanilla calls when
//! something happens in the world. They are separated from [`crate::Particle`]
//! because the *shape* of a burst — how many fragments, where in the block, with
//! what spread — is where the character of an effect lives, and is far more
//! visible than any individual particle's trajectory.
//!
//! # Block shapes come from the caller
//!
//! Vanilla reads `BlockState.getShape` (the **outline** shape, the one the
//! selection box traces) rather than the collision shape. The two differ for a
//! meaningful set of blocks: `short_grass` has a small outline and *no*
//! collision at all, so driving a break burst from collision geometry would emit
//! nothing when a player breaks grass — one of the most common actions there is.
//!
//! So these functions take the boxes as an argument rather than querying a
//! world. The renderer already knows the true outline geometry from the block
//! model, which makes it the correct source, and it keeps this crate free of a
//! dependency on any particular world representation.

use crate::rng::JavaRandom;
use crate::{Behaviour, Particle, ParticleEngine, Sheet, SpriteSource};
use lodestone_physics::Aabb;

/// A face of a block, for the mining-hit emitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    /// −Y
    Down,
    /// +Y
    Up,
    /// −Z
    North,
    /// +Z
    South,
    /// −X
    West,
    /// +X
    East,
}

/// The unit cube, in block-local coordinates — the shape to pass for an ordinary
/// full block, and the honest fallback where outline geometry is unavailable.
pub const FULL_CUBE: Aabb = Aabb {
    min_x: 0.0,
    min_y: 0.0,
    min_z: 0.0,
    max_x: 1.0,
    max_y: 1.0,
    max_z: 1.0,
};

/// `new TerrainParticle(...)` — one fragment of a block.
///
/// `tint` is the block's colour multiplier at this position (grass and foliage
/// are biome-tinted; everything else is white). Vanilla starts every terrain
/// particle at `0.6` grey and multiplies the tint into it, which is why block
/// fragments always look slightly darker than the block they came from.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the `TerrainParticle` constructor argument for argument"
)]
#[must_use]
pub fn terrain_particle(
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    state: u32,
    tint: [f32; 3],
    rng: &mut JavaRandom,
) -> Particle {
    let mut p = Particle::with_velocity(x, y, z, xa, ya, za, SpriteSource::BlockState(state), rng);
    p.gravity = 1.0;
    p.colour = [0.6 * tint[0], 0.6 * tint[1], 0.6 * tint[2]];
    p.quad_size /= 2.0;
    // Two more draws, *after* the colour is set — order matters for replay.
    let uo = rng.next_float() * 3.0;
    let vo = rng.next_float() * 3.0;
    p.behaviour = Behaviour::Terrain { uo, vo };
    p
}

/// `ClientLevel.addDestroyBlockEffect` — the burst when a block is destroyed.
///
/// Vanilla subdivides the block's outline shape at a density of `0.25`, with a
/// floor of two samples per axis, and emits one fragment per cell moving
/// *outward* from the block centre. A full cube therefore produces `4³ = 64`
/// fragments and a slab produces `4 × 2 × 4 = 32`, so a thin block visibly
/// throws less debris — a detail that reads immediately as wrong if the count is
/// fixed instead of derived.
///
/// `shape` is in block-local coordinates; pass [`FULL_CUBE`] for an ordinary
/// block. An empty `shape` emits nothing, matching a block that should not spawn
/// terrain particles at all.
pub fn destroy_block_effect(
    engine: &mut ParticleEngine,
    block: (i32, i32, i32),
    state: u32,
    tint: [f32; 3],
    shape: &[Aabb],
) {
    /// `double density = 0.25` in `addDestroyBlockEffect`.
    const DENSITY: f64 = 0.25;

    let (bx, by, bz) = block;
    for aabb in shape {
        let width_x = (aabb.max_x - aabb.min_x).min(1.0);
        let width_y = (aabb.max_y - aabb.min_y).min(1.0);
        let width_z = (aabb.max_z - aabb.min_z).min(1.0);
        let count_x = subdivisions(width_x, DENSITY);
        let count_y = subdivisions(width_y, DENSITY);
        let count_z = subdivisions(width_z, DENSITY);

        for xx in 0..count_x {
            for yy in 0..count_y {
                for zz in 0..count_z {
                    let rel_x = midpoint(xx, count_x);
                    let rel_y = midpoint(yy, count_y);
                    let rel_z = midpoint(zz, count_z);
                    let p = terrain_particle(
                        f64::from(bx) + rel_x.mul_add(width_x, aabb.min_x),
                        f64::from(by) + rel_y.mul_add(width_y, aabb.min_y),
                        f64::from(bz) + rel_z.mul_add(width_z, aabb.min_z),
                        // The velocity is the offset from the block centre, so
                        // fragments fly apart rather than in a common direction.
                        rel_x - 0.5,
                        rel_y - 0.5,
                        rel_z - 0.5,
                        state,
                        tint,
                        engine.rng(),
                    );
                    engine.add(p);
                }
            }
        }
    }
}

/// `Math.max(2, Mth.ceil(width / density))`.
fn subdivisions(width: f64, density: f64) -> i32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the quotient is at most 4 for a unit block"
    )]
    let raw = (width / density).ceil() as i32;
    raw.max(2)
}

/// `(i + 0.5) / count` — the centre of the `i`th cell.
fn midpoint(i: i32, count: i32) -> f64 {
    (f64::from(i) + 0.5) / f64::from(count)
}

/// `ClientLevel.addBreakingBlockEffect` — the single fragment that pops off the
/// face a player is currently mining.
///
/// Vanilla emits one of these every few ticks while a dig is in progress, which
/// together with the crack overlay is what makes mining feel like it is doing
/// something. The particle spawns just *outside* the struck face (by `0.1`) so
/// it is not immediately swallowed by the block it came from.
pub fn breaking_block_effect(
    engine: &mut ParticleEngine,
    block: (i32, i32, i32),
    state: u32,
    tint: [f32; 3],
    face: Face,
    shape: Aabb,
) {
    let (bx, by, bz) = block;
    let (x, y, z) = (f64::from(bx), f64::from(by), f64::from(bz));

    let rng = engine.rng();
    // Inset by 0.1 on every axis so the fragment starts inside the face, then
    // one axis is overridden below to sit just outside it.
    let mut xp = rng
        .next_double()
        .mul_add(shape.max_x - shape.min_x - 0.2, 0.1)
        + x
        + shape.min_x;
    let mut yp = rng
        .next_double()
        .mul_add(shape.max_y - shape.min_y - 0.2, 0.1)
        + y
        + shape.min_y;
    let mut zp = rng
        .next_double()
        .mul_add(shape.max_z - shape.min_z - 0.2, 0.1)
        + z
        + shape.min_z;

    match face {
        Face::Down => yp = y + shape.min_y - 0.1,
        Face::Up => yp = y + shape.max_y + 0.1,
        Face::North => zp = z + shape.min_z - 0.1,
        Face::South => zp = z + shape.max_z + 0.1,
        Face::West => xp = x + shape.min_x - 0.1,
        Face::East => xp = x + shape.max_x + 0.1,
    }

    let mut p = terrain_particle(xp, yp, zp, 0.0, 0.0, 0.0, state, tint, engine.rng());
    // `.setPower(0.2F).scale(0.6F)` — a mining chip is slower and smaller than a
    // destruction fragment.
    p.set_power(0.2);
    p.scale(0.6);
    engine.add(p);
}

/// `CritParticle` — the sparkle on a critical hit.
pub fn crit(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        0.0,
        0.0,
        0.0,
        SpriteSource::Sheet {
            sheet: Sheet::CriticalHit,
            frame: 0,
        },
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
    engine.add(p);
}

/// `SmokeParticle` — `BaseAshSmokeParticle` with smoke's parameters
/// (`0.3` colour jitter, 8-tick base lifetime, `-0.1` gravity so it rises).
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the `SmokeParticle` constructor argument for argument"
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
    p.gravity = -0.1;
    // Smoke that hits a ceiling spreads sideways instead of piling up.
    p.speed_up_when_y_blocked = true;
    // Smoke's direction scale is 0.1 on every axis.
    let dir = f64::from(0.1_f32);
    p.xd = p.xd.mul_add(dir, xa);
    p.yd = p.yd.mul_add(dir, ya);
    p.zd = p.zd.mul_add(dir, za);
    let col = rng_next(engine) * 0.3;
    p.colour = [col, col, col];
    p.quad_size *= 0.75 * scale;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(8.0_f32) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)
        * f64::from(scale)) as i32;
    p.lifetime = lifetime.max(1);
    p.behaviour = Behaviour::AshSmoke;
    // `setSpriteFromAge` runs in the constructor, before the first tick.
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Generic,
        frame: Sheet::Generic.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

/// `FlameParticle` — a `RisingParticle` that ignores collision.
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
    let jitter = |r: &mut JavaRandom| f64::from((r.next_float() - r.next_float()) * 0.05);
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

/// `BubbleParticle` — rises through water and pops the instant it leaves.
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
    let scatter = |r: &mut JavaRandom| f64::from(r.next_float().mul_add(2.0, -1.0) * 0.02);
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

/// `SplashParticle` — a `WaterDropParticle` launched by something entering water.
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
    // `WaterDropParticle`'s constructor.
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
    // `SplashParticle` overrides gravity and, for purely horizontal input,
    // replaces the velocity outright so the drop arcs upward.
    p.gravity = 0.04;
    if ya == 0.0 && (xa != 0.0 || za != 0.0) {
        p.xd = xa;
        p.yd = 0.1;
        p.zd = za;
    }
    p.behaviour = Behaviour::WaterDrop;
    let frame = engine.rng().next_int_bound(i32::from(Sheet::Splash.frame_count()));
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

/// One `nextFloat()` from the engine's RNG.
fn rng_next(engine: &mut ParticleEngine) -> f32 {
    engine.rng().next_float()
}

#[cfg(test)]
mod tests {
    use super::{
        FULL_CUBE, Face, breaking_block_effect, bubble, crit, destroy_block_effect, flame, smoke,
        splash,
    };
    use crate::{Behaviour, ParticleEngine, SpriteSource};
    use lodestone_physics::Aabb;

    const STONE: u32 = 1;
    const WHITE: [f32; 3] = [1.0, 1.0, 1.0];

    /// The counts here come from the **formula in `addDestroyBlockEffect`**
    /// (`max(2, ceil(width / 0.25))` per axis), not from running this code: a
    /// full cube is `4 × 4 × 4` and a bottom slab is `4 × 2 × 4`.
    #[test]
    fn a_full_cube_throws_sixty_four_fragments() {
        let mut engine = ParticleEngine::seeded(1);
        destroy_block_effect(&mut engine, (0, 64, 0), STONE, WHITE, &[FULL_CUBE]);
        assert_eq!(engine.len(), 64);
    }

    #[test]
    fn a_thinner_block_throws_proportionally_less_debris() {
        let slab = Aabb::new(0.0, 0.0, 0.0, 1.0, 0.5, 1.0);
        let mut engine = ParticleEngine::seeded(1);
        destroy_block_effect(&mut engine, (0, 64, 0), STONE, WHITE, &[slab]);
        assert_eq!(engine.len(), 32, "a half-height slab should emit 4 x 2 x 4");
    }

    /// The `max(2, ...)` floor: a shape thinner than the density step must still
    /// emit two samples on that axis, not zero. A carpet that emitted nothing
    /// would look like broken particle code rather than a thin block.
    #[test]
    fn a_very_thin_shape_still_emits_two_samples_on_that_axis() {
        let carpet = Aabb::new(0.0, 0.0, 0.0, 1.0, 0.0625, 1.0);
        let mut engine = ParticleEngine::seeded(1);
        destroy_block_effect(&mut engine, (0, 64, 0), STONE, WHITE, &[carpet]);
        assert_eq!(engine.len(), 32, "expected 4 x 2 x 4 from the minimum floor");
    }

    #[test]
    fn an_empty_shape_emits_nothing() {
        let mut engine = ParticleEngine::seeded(1);
        destroy_block_effect(&mut engine, (0, 64, 0), STONE, WHITE, &[]);
        assert!(engine.is_empty());
    }

    #[test]
    fn multi_box_shapes_emit_from_every_box() {
        let lower = Aabb::new(0.0, 0.0, 0.0, 1.0, 0.5, 1.0);
        let upper = Aabb::new(0.0, 0.5, 0.0, 1.0, 1.0, 1.0);
        let mut engine = ParticleEngine::seeded(1);
        destroy_block_effect(&mut engine, (0, 64, 0), STONE, WHITE, &[lower, upper]);
        assert_eq!(engine.len(), 64, "two half-boxes should match one full cube");
    }

    #[test]
    fn every_fragment_lands_inside_the_block_it_came_from() {
        let mut engine = ParticleEngine::seeded(2);
        destroy_block_effect(&mut engine, (3, 64, -7), STONE, WHITE, &[FULL_CUBE]);
        for p in engine.particles() {
            assert!(
                (3.0..4.0).contains(&p.x) && (64.0..65.0).contains(&p.y) && (-7.0..-6.0).contains(&p.z),
                "fragment spawned outside its block at ({}, {}, {})",
                p.x,
                p.y,
                p.z
            );
        }
    }

    /// Terrain particles carry the block's identity, which is the whole point —
    /// breaking oak must throw wood-coloured chips, not generic grey ones.
    #[test]
    fn fragments_carry_the_block_state_and_the_vanilla_grey() {
        let mut engine = ParticleEngine::seeded(3);
        destroy_block_effect(&mut engine, (0, 64, 0), 42, WHITE, &[FULL_CUBE]);
        let p = &engine.particles()[0];
        assert_eq!(p.sprite, SpriteSource::BlockState(42));
        assert!(matches!(p.behaviour, Behaviour::Terrain { .. }));
        for c in p.colour {
            assert!((c - 0.6).abs() < 1e-6, "expected 0.6 grey, got {c}");
        }
        assert!(p.gravity > 0.99, "terrain fragments must fall");
    }

    #[test]
    fn a_biome_tint_multiplies_into_the_fragment_colour() {
        let mut engine = ParticleEngine::seeded(3);
        destroy_block_effect(&mut engine, (0, 64, 0), 42, [0.5, 1.0, 0.25], &[FULL_CUBE]);
        let c = engine.particles()[0].colour;
        assert!((c[0] - 0.3).abs() < 1e-6, "r was {}", c[0]);
        assert!((c[1] - 0.6).abs() < 1e-6, "g was {}", c[1]);
        assert!((c[2] - 0.15).abs() < 1e-6, "b was {}", c[2]);
    }

    /// Every face must place the chip just *outside* the block, or it spawns
    /// inside the geometry and is invisible for its whole life.
    #[test]
    fn mining_chips_spawn_just_outside_the_struck_face() {
        let cases = [
            (Face::Up, 65.1_f64),
            (Face::Down, 63.9),
            (Face::North, -0.1),
            (Face::South, 1.1),
        ];
        for (face, expected) in cases {
            let mut engine = ParticleEngine::seeded(4);
            breaking_block_effect(&mut engine, (0, 64, 0), STONE, WHITE, face, FULL_CUBE);
            assert_eq!(engine.len(), 1, "{face:?} emitted the wrong count");
            let p = &engine.particles()[0];
            let got = match face {
                Face::Up | Face::Down => p.y,
                Face::North | Face::South => p.z,
                Face::East | Face::West => p.x,
            };
            assert!(
                (got - expected).abs() < 1e-9,
                "{face:?} chip at {got}, expected {expected}"
            );
        }
    }

    #[test]
    fn a_mining_chip_is_smaller_and_slower_than_a_destruction_fragment() {
        let mut chip_engine = ParticleEngine::seeded(5);
        breaking_block_effect(&mut chip_engine, (0, 64, 0), STONE, WHITE, Face::Up, FULL_CUBE);
        let chip = &chip_engine.particles()[0];

        let mut burst_engine = ParticleEngine::seeded(5);
        destroy_block_effect(&mut burst_engine, (0, 64, 0), STONE, WHITE, &[FULL_CUBE]);
        let fragment = &burst_engine.particles()[0];

        assert!(
            chip.quad_size < fragment.quad_size,
            "chip {} should be smaller than fragment {}",
            chip.quad_size,
            fragment.quad_size
        );
        let speed = |p: &crate::Particle| p.xd.hypot(p.zd);
        assert!(
            speed(chip) < speed(fragment),
            "chip should be slower than a destruction fragment"
        );
    }

    #[test]
    fn crits_are_physics_free_and_start_pale() {
        let mut engine = ParticleEngine::seeded(6);
        crit(&mut engine, 0.0, 64.0, 0.0, 0.0, 0.0, 0.0);
        let p = &engine.particles()[0];
        assert!(!p.has_physics, "CritParticle sets hasPhysics = false");
        assert!(p.lifetime >= 1, "lifetime is floored at 1");
        // `nextFloat() * 0.3F + 0.6F` is bounded by the formula, not by us.
        for c in p.colour {
            assert!((0.6..0.9).contains(&c), "colour {c} outside 0.6..0.9");
        }
    }

    #[test]
    fn smoke_rises_and_spreads_under_a_ceiling() {
        let mut engine = ParticleEngine::seeded(7);
        smoke(&mut engine, 0.0, 64.0, 0.0, 0.0, 0.0, 0.0, 1.0);
        let p = &engine.particles()[0];
        assert!(p.gravity < 0.0, "smoke must have negative gravity to rise");
        assert!(
            p.speed_up_when_y_blocked,
            "smoke sets speedUpWhenYMotionIsBlocked"
        );
        assert!(matches!(p.behaviour, Behaviour::AshSmoke));
    }

    #[test]
    fn flame_and_bubble_and_splash_get_their_own_behaviours() {
        let mut engine = ParticleEngine::seeded(8);
        flame(&mut engine, 0.0, 64.0, 0.0, 0.0, 0.01, 0.0);
        bubble(&mut engine, 0.0, 64.0, 0.0, 0.0, 0.0, 0.0);
        splash(&mut engine, 0.0, 64.0, 0.0, 0.0, 0.0, 0.0);
        let kinds: Vec<_> = engine.particles().iter().map(|p| p.behaviour).collect();
        assert!(matches!(kinds[0], Behaviour::Flame));
        assert!(matches!(kinds[1], Behaviour::Bubble));
        assert!(matches!(kinds[2], Behaviour::WaterDrop));
    }

    #[test]
    fn a_seeded_burst_replays_exactly() {
        let burst = |seed| {
            let mut e = ParticleEngine::seeded(seed);
            destroy_block_effect(&mut e, (0, 64, 0), STONE, WHITE, &[FULL_CUBE]);
            e.particles()
                .iter()
                .map(|p| (p.x, p.y, p.z, p.xd, p.yd, p.zd, p.lifetime))
                .collect::<Vec<_>>()
        };
        assert_eq!(burst(1234), burst(1234));
        assert_ne!(burst(1234), burst(1235));
    }
}
