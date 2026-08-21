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

/// `new BreakingItemParticle(...)` — one crumb of an item.
///
/// The same shape as [`terrain_particle`] and deliberately so: vanilla's
/// `BreakingItemParticle` and `TerrainParticle` have **byte-identical**
/// `getU0`/`getU1`/`getV0`/`getV1` overrides (a quarter sub-sprite at
/// `(uo + 1) / 4 .. uo / 4`, `uo`/`vo` each `random.nextFloat() * 3.0F`), the same
/// `gravity = 1.0F` and the same `quadSize /= 2.0F`, so [`Behaviour::Terrain`]
/// describes both. Only the sprite source and the absence of a `0.6` grey differ:
/// an item crumb is drawn at full brightness.
///
/// # The velocity is *not* `setPower`
///
/// `BreakingItemParticle`'s public constructor chains to the zero-velocity one and
/// then does `xd *= 0.1F; … xd += xa;`, a **plain multiply of all three
/// components** followed by an add.
/// [`Particle::set_power`](crate::Particle::set_power) is the wrong tool: it
/// deliberately preserves [`Particle::with_velocity`](crate::Particle::with_velocity)'s
/// `0.1` upward bias across the scale, and vanilla here scales that bias too (to
/// `0.01`). Using `set_power` leaves the crumbs drifting upward roughly ten times
/// too fast, which reads as "the particles are wrong" rather than as an arithmetic
/// slip.
#[must_use]
pub fn item_particle(
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    item: u32,
    rng: &mut JavaRandom,
) -> Particle {
    // The 4-argument constructor: zero given velocity, so the whole of `xd/yd/zd`
    // is `Particle`'s own randomised jitter.
    let mut p = Particle::with_velocity(x, y, z, 0.0, 0.0, 0.0, SpriteSource::Item(item), rng);
    p.gravity = 1.0;
    p.quad_size /= 2.0;
    // Two more draws, *after* the quad size — order matters for replay, exactly as
    // in `terrain_particle`.
    let uo = rng.next_float() * 3.0;
    let vo = rng.next_float() * 3.0;
    p.behaviour = Behaviour::Terrain { uo, vo };
    // `xd *= 0.1F; yd *= 0.1F; zd *= 0.1F; xd += xa; …` — see the note above on why
    // this is not `set_power`.
    p.xd = p.xd * 0.1 + xa;
    p.yd = p.yd * 0.1 + ya;
    p.zd = p.zd * 0.1 + za;
    p
}

/// `LivingEntity.spawnItemParticles(itemStack, count)` — the crumbs that fly from
/// an entity's mouth while it eats, and the same burst `breakItem` throws when a
/// tool snaps.
///
/// `count` is **5** per periodic emission while consuming and **16** on the final
/// bite (`ItemStack.onUseTick` and `Consumable.onConsume` respectively); it is a
/// parameter because those are the two call sites and neither number belongs here.
///
/// # Everything is in the entity's own facing frame
///
/// Both the spawn offset and the velocity are built in a body-local frame
/// (`+z` forward, `0.6` blocks ahead of the eye) and then rotated by `-xRot` and
/// `-yRot`, so crumbs leave the mouth rather than a fixed world direction. Getting
/// either sign wrong puts them behind the head, where they are invisible from first
/// person and therefore read as "no particles" — the failure this ordering exists to
/// avoid. `y_rot_deg` is vanilla's yaw (`0` = south / `+z`) and `x_rot_deg` its pitch
/// (positive = looking down).
///
/// The vertical spawn offset is `-nextFloat() * 0.6 - 0.3`, i.e. **below** the eye,
/// which is where a mouth is.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors `spawnItemParticles` plus the eye position and facing it reads off the entity"
)]
pub fn spawn_item_particles(
    engine: &mut ParticleEngine,
    eye_x: f64,
    eye_y: f64,
    eye_z: f64,
    x_rot_deg: f32,
    y_rot_deg: f32,
    item: u32,
    count: u32,
) {
    let x_rad = -x_rot_deg.to_radians();
    let y_rad = -y_rot_deg.to_radians();
    for _ in 0..count {
        let (dx, dy, dz) = {
            let rng = engine.rng();
            // `new Vec3((nextFloat() - 0.5) * 0.1, nextFloat() * 0.1 + 0.1, 0.0)`
            let d = (
                (f64::from(rng.next_float()) - 0.5) * 0.1,
                f64::from(rng.next_float()).mul_add(0.1, 0.1),
                0.0,
            );
            let d = x_rot(d, x_rad);
            y_rot(d, y_rad)
        };
        let (px, py, pz) = {
            let rng = engine.rng();
            // `double y1 = -nextFloat() * 0.6 - 0.3;`
            // `new Vec3((nextFloat() - 0.5) * 0.3, y1, 0.6)` — note vanilla draws
            // `y1` *before* the horizontal jitter, so the two `nextFloat()` calls
            // are in that order and swapping them desynchronises the sequence.
            let y1 = (-f64::from(rng.next_float())).mul_add(0.6, -0.3);
            let p = ((f64::from(rng.next_float()) - 0.5) * 0.3, y1, 0.6);
            let p = x_rot(p, x_rad);
            y_rot(p, y_rad)
        };
        let p = item_particle(
            eye_x + px,
            eye_y + py,
            eye_z + pz,
            dx,
            // `addParticle(..., d.y + 0.05, ...)` — the bias is applied at the
            // call site, not inside the rotation.
            dy + 0.05,
            dz,
            item,
            engine.rng(),
        );
        engine.add(p);
    }
}

/// `Vec3.xRot(radians)`.
fn x_rot((x, y, z): (f64, f64, f64), radians: f32) -> (f64, f64, f64) {
    let (cos, sin) = (f64::from(radians.cos()), f64::from(radians.sin()));
    (x, y * cos + z * sin, z * cos - y * sin)
}

/// `Vec3.yRot(radians)`.
fn y_rot((x, y, z): (f64, f64, f64), radians: f32) -> (f64, f64, f64) {
    let (cos, sin) = (f64::from(radians.cos()), f64::from(radians.sin()));
    (x * cos + z * sin, y, z * cos - x * sin)
}

/// `CritParticle` — the sparkle on a critical hit.
pub fn crit(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let p = crit_particle(engine, x, y, z, xa, ya, za, Sheet::CriticalHit);
    engine.add(p);
}

/// `CritParticle.MagicProvider` (`ParticleTypes.ENCHANTED_HIT`) — the sparkle
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

/// `CritParticle.DamageIndicatorProvider` (`ParticleTypes.DAMAGE_INDICATOR`) —
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

/// The `CritParticle` constructor, shared by its three providers.
///
/// Returned rather than added so each provider can apply its own
/// post-construction tint or lifetime before the particle goes live — the same
/// split vanilla gets for free by returning the object from `createParticle`.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the `CritParticle` constructor argument for argument, plus the sheet \
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

/// `AttackSweepParticle` — the arc thrown by a sweeping melee hit.
///
/// `.cache/mc/26.2/client-src/net/minecraft/client/particle/AttackSweepParticle.java`:
/// no `move()` call at all (stationary for its whole life — see
/// [`crate::Particle::tick_sweep_attack`]), full-bright, 4-tick lifetime, a
/// grey tint drawn once (`nextFloat() * 0.6F + 0.4F`), and
/// `quadSize = 1.0F - (float) size * 0.5F`.
///
/// `size` is the constructor's own `xAux` parameter — but the one real
/// vanilla call site
/// (`.cache/mc/26.2/src/net/minecraft/world/entity/player/Player.java:1191`,
/// `serverLevel.sendParticles(ParticleTypes.SWEEP_ATTACK, x, y, z, 0, dx, 0.0,
/// dz, 0.0)`) sends `count == 0` with `maxSpeed == 0.0F`, and
/// `ClientPacketListener.handleParticleEvent`'s `count == 0` branch computes
/// `xAux = maxSpeed * xDist`, so the value that actually reaches this
/// constructor in real play is always `0.0`, regardless of `dx` — i.e.
/// `quadSize` is always `1.0` in practice. Taking `size` as a parameter
/// anyway (rather than hardcoding that) keeps this a faithful transcription
/// of the Java constructor for any future caller (a datapack or `/particle`
/// invocation can still pass a nonzero `xAux`).
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

/// `NoteParticle` — the coloured chime above a played note block.
///
/// `.cache/mc/26.2/client-src/net/minecraft/client/particle/NoteParticle.java`:
/// zero initial velocity, `friction = 0.66F`,
/// `speedUpWhenYMotionIsBlocked = true`, `yd += 0.2`, a fixed `lifetime = 6`
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

/// The shared `HeartParticle` constructor body
/// (`.cache/mc/26.2/client-src/net/minecraft/client/particle/HeartParticle.java`):
/// zero initial velocity, `speedUpWhenYMotionIsBlocked = true`,
/// `friction = 0.86F`, `yd += 0.1`, `quadSize *= 1.5F`, `lifetime = 16`,
/// `hasPhysics = false`. [`heart`] and [`angry_villager`] are its two
/// registered providers — same class, different sprite and vertical offset
/// at the emit site (the `+ 0.5` in `AngryVillagerProvider.createParticle`).
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

/// `HeartParticle.Provider` — breeding hearts (`ParticleTypes.HEART`).
pub fn heart(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let p = heart_particle(engine, x, y, z, Sheet::Heart);
    engine.add(p);
}

/// `HeartParticle.AngryVillagerProvider` — the villager "angry" icon
/// (`ParticleTypes.ANGRY_VILLAGER`). Same physics as [`heart`], a different
/// sprite (`particle/angry`, not `particle/heart`), and vanilla raises the
/// spawn point by `0.5` at the call site rather than in the particle class —
/// reproduced here since this function *is* that call site.
pub fn angry_villager(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let p = heart_particle(engine, x, y + 0.5, z, Sheet::Angry);
    engine.add(p);
}

/// `SuspendedTownParticle.HappyVillagerProvider` — the villager "happy" icon
/// (`ParticleTypes.HAPPY_VILLAGER`).
///
/// `.cache/mc/26.2/client-src/net/minecraft/client/particle/SuspendedTownParticle.java`:
/// a jittered-velocity construction (vanilla's `super(level, x, y, z, xa, ya,
/// za, sprite)` — the same `Particle(level, x, y, z, xa, ya, za)` shape
/// [`Particle::with_velocity`] already reproduces) followed by a dim grey
/// tint (`nextFloat() * 0.1F + 0.2F`), a `0.02`×`0.02` box, a
/// `nextFloat() * 0.6F + 0.5F` quad-size jitter, the velocity damped to a
/// hundredth, and `lifetime = (int)(20.0 / (nextFloat() * 0.8F + 0.2F))`.
/// `HappyVillagerProvider` itself then calls `setColor(1, 1, 1)`, which is
/// redundant here since white is this crate's own particle default.
pub fn happy_villager(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        xa,
        ya,
        za,
        SpriteSource::Sheet {
            sheet: Sheet::Glint,
            frame: 0,
        },
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
    engine.add(p);
}

/// `SpellParticle.WitchProvider` — the purple motes above a drinking witch
/// (`ParticleTypes.WITCH`).
///
/// `.cache/mc/26.2/client-src/net/minecraft/client/particle/SpellParticle.java`:
/// the constructor jitters its *own* horizontal velocity from a
/// process-wide static `RandomSource` (`SpellParticle.RANDOM`) rather than
/// the per-particle stream every other emitter in this crate draws from —
/// drawn from this engine's RNG instead, since particle-burst randomness is
/// disclosed as not needing bit-exact replay (see
/// [`crate::Particles::spawn_particles`]'s module docs in the shell for the
/// same policy applied to the network dispatch). `friction = 0.96F`,
/// `gravity = -0.1F`, `speedUpWhenYMotionIsBlocked = true`, `yd *= 0.2F`, and
/// — using the constructor's *original*, unjittered `xa`/`za` parameters,
/// not the ones just fed into the velocity jitter — a further `xd`/`zd`
/// damp to a tenth when both were exactly zero. `quadSize *= 0.75F`,
/// `lifetime = (int)(8.0 / (nextFloat() * 0.8F + 0.2F))`, `hasPhysics =
/// false`. `WitchProvider` then sets the colour: `nextFloat() * 0.5F +
/// 0.35F` brightness times `(1, 0, 1)` — magenta, never green.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the `SpellParticle` constructor argument for argument"
)]
pub fn witch(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let mut p = spell_particle(engine, x, y, z, xa, ya, za, Sheet::Spell);
    let rb = rng_next(engine).mul_add(0.5, 0.35);
    p.colour = [rb, 0.0, rb];
    engine.add(p);
}

/// `SpellParticle` over an arbitrary sheet and a fixed tint — the shared shape
/// behind `effect`, `entity_effect`, `instant_effect`, `infested`, `raid_omen`
/// and `trial_omen`.
///
/// Vanilla registers six registry types against `SpellParticle`, over **four
/// different sheets**: `EFFECT`/`ENTITY_EFFECT` name `effect_7…0`,
/// `INSTANT_EFFECT`/`WITCH` name `spell_7…0`, and `INFESTED`/`RAID_OMEN`/
/// `TRIAL_OMEN` each name a single texture of their own. The class does not
/// decide the sheet; the type's own `particles/<name>.json` does, which is why
/// this takes one.
///
/// `colour` is the provider's `setColor` call. The three
/// `ParticleOptions`-carrying types (`effect` and `instant_effect` read a
/// `SpellParticleOption`; `entity_effect` reads a `ColorParticleOption`) supply
/// theirs from the wire — neither payload is decoded by any protocol family
/// here yet, so the shell passes white and the caller sees an uncoloured mote.
/// The parameter exists so that gap lives at the decoder rather than in this
/// transcription.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the `SpellParticle` constructor argument for argument, plus the sheet \
              and tint its provider supplies"
)]
pub fn spell(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    sheet: Sheet,
    colour: [f32; 3],
) {
    let mut p = spell_particle(engine, x, y, z, xa, ya, za, sheet);
    p.colour = colour;
    engine.add(p);
}

/// The `SpellParticle` constructor itself, shared by its four providers.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the `SpellParticle` constructor argument for argument, plus its sheet"
)]
fn spell_particle(
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
    let jitter_x = 0.5 - rng.next_double();
    let jitter_z = 0.5 - rng.next_double();
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        jitter_x,
        ya,
        jitter_z,
        SpriteSource::Sheet { sheet, frame: 0 },
        rng,
    );
    p.friction = 0.96;
    p.gravity = -0.1;
    p.speed_up_when_y_blocked = true;
    p.yd *= f64::from(0.2_f32);
    if xa == 0.0 && za == 0.0 {
        p.xd *= f64::from(0.1_f32);
        p.zd *= f64::from(0.1_f32);
    }
    p.quad_size *= 0.75;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (8.0 / f64::from(rng_next(engine).mul_add(0.8, 0.2))) as i32;
    p.lifetime = lifetime;
    p.has_physics = false;
    p.behaviour = Behaviour::Spell;
    p.sprite = SpriteSource::Sheet {
        sheet,
        frame: sheet.frame_for_age(0, p.lifetime),
    };
    p
}

/// `TotemParticle` — the burst when a totem of undying saves its holder
/// (`ParticleTypes.TOTEM_OF_UNDYING`).
///
/// `.cache/mc/26.2/client-src/net/minecraft/client/particle/TotemParticle.java`:
/// extends `SimpleAnimatedParticle` (`friction = 0.91F` overridden
/// immediately back down to `0.6F`, `gravity = 1.25F`), takes its velocity
/// **directly** from the caller with no jitter at all (`xd = xa` etc.),
/// `quadSize *= 0.75F`, `lifetime = 60 + nextInt(12)`, and a 1-in-4 chance of
/// a "golden" tint (`0.6..0.8, 0.6..0.9, 0..0.2`) versus the usual "green"
/// one (`0.1..0.3, 0.4..0.7, 0..0.2`) — both branches draw exactly three
/// `nextFloat()`s, so the RNG stream length does not depend on which
/// branch is taken. No `setFadeColor`, so only alpha fades
/// ([`Behaviour::SimpleAnimated`]'s existing `fade: None` path already
/// covers this exactly).
pub fn totem_of_undying(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
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
    p.friction = 0.6;
    p.gravity = 1.25;
    p.xd = xa;
    p.yd = ya;
    p.zd = za;
    p.quad_size *= 0.75;
    let extra = engine.rng().next_int_bound(12);
    p.lifetime = 60 + extra;
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Glitter,
        frame: Sheet::Glitter.frame_for_age(0, p.lifetime),
    };
    let golden = engine.rng().next_int_bound(4) == 0;
    p.colour = if golden {
        [
            rng_next(engine).mul_add(0.2, 0.6),
            rng_next(engine).mul_add(0.3, 0.6),
            rng_next(engine) * 0.2,
        ]
    } else {
        [
            rng_next(engine).mul_add(0.2, 0.1),
            rng_next(engine).mul_add(0.3, 0.4),
            rng_next(engine) * 0.2,
        ]
    };
    p.behaviour = Behaviour::SimpleAnimated { fade: None };
    engine.add(p);
}

/// `HugeExplosionSeedParticle.Provider` — `ParticleTypes.EXPLOSION_EMITTER`,
/// the particle a `ClientboundExplodePacket`'s `explosionParticle` field
/// almost always names (`Level.java:593,619,645`).
///
/// `.cache/mc/26.2/client-src/net/minecraft/client/particle/
/// HugeExplosionSeedParticle.java`: `super(level, x, y, z, 0.0, 0.0, 0.0)` —
/// the zero-velocity `Particle(level, x, y, z, xa, ya, za)` constructor, the
/// same shape [`Particle::with_velocity`] already reproduces for every other
/// emitter — then a hardcoded `lifetime = 8` that **overwrites** whatever the
/// base constructor's own lifetime draw produced (matching how [`note`]/
/// [`heart_particle`] overwrite theirs). The particle itself is never drawn
/// (`NoRenderParticle`); it exists purely to schedule
/// [`Behaviour::HugeExplosionSeed`]'s per-tick follow-up spawns — see
/// [`Particle::tick_huge_explosion_seed`] for that schedule.
pub fn explosion_emitter(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        0.0,
        0.0,
        0.0,
        // Never sampled — `NoRenderParticle` is excluded from `extract`
        // before any sprite lookup happens — but every `Particle` needs a
        // `SpriteSource`, so this names the sheet its own follow-ups use
        // rather than an arbitrary placeholder.
        SpriteSource::Sheet {
            sheet: Sheet::Explosion,
            frame: 0,
        },
        rng,
    );
    p.lifetime = 8;
    p.behaviour = Behaviour::HugeExplosionSeed;
    engine.add(p);
}

/// `HugeExplosionParticle.Provider` — `ParticleTypes.EXPLOSION`. Spawned
/// directly by a real vanilla packet only rarely (`ServerExplosion`'s
/// small/large split can choose it), but far more often as
/// [`explosion_emitter`]'s own six-per-tick follow-up, via
/// [`ParticleEngine::tick`].
///
/// `.cache/mc/26.2/client-src/net/minecraft/client/particle/
/// HugeExplosionParticle.java`: zero-velocity construction, then
/// `lifetime = 6 + random.nextInt(4)` (range `[6, 10)`), a grey tint
/// (`random.nextFloat() * 0.6F + 0.4F`, same value on every channel — one
/// draw, not three), and `quadSize = 2.0F * (1.0F - size * 0.5F)` — `size`
/// being this function's own `size` parameter, vanilla's constructor
/// argument (the seed's `age / lifetime` ratio when called from there, or
/// the network `xAux` when called directly from a packet). No override on
/// gravity, friction or collision, so the particle just sits at its spawn
/// point for its whole life — vanilla's constructor never touches those
/// fields either.
pub fn huge_explosion(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, size: f32) {
    let rng = engine.rng();
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        0.0,
        0.0,
        0.0,
        SpriteSource::Sheet {
            sheet: Sheet::Explosion,
            frame: 0,
        },
        rng,
    );
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's (int) cast on nextInt's own already-integral result; kept for the \
                  same reason every other emitter in this module spells out the cast"
    )]
    let extra = engine.rng().next_int_bound(4);
    p.lifetime = 6 + extra;
    let col = rng_next(engine).mul_add(0.6, 0.4);
    p.colour = [col, col, col];
    // `2.0F * (1.0F - size * 0.5F)`, i.e. `2.0 - size`, written as the same
    // `mul_add` shape the constant is transcribed from rather than the
    // algebraically-simplified form, so this line matches the Java source
    // token for token.
    p.quad_size = 2.0 * size.mul_add(-0.5, 1.0);
    p.behaviour = Behaviour::HugeExplosion;
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Explosion,
        frame: Sheet::Explosion.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

#[cfg(test)]
mod tests {
    use super::{
        FULL_CUBE, Face, angry_villager, breaking_block_effect, bubble, crit, destroy_block_effect,
        explosion_emitter, firework, flame, fly_towards_position, happy_villager, heart,
        huge_explosion, note, smoke, splash, sweep_attack, totem_of_undying, witch,
    };
    use crate::{Behaviour, ParticleEngine, Sheet, SpriteSource};
    use lodestone_physics::{Aabb, CollisionView};

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

    /// The sweep-attack particle (#12's split-out remainder): exactly the
    /// vanilla shape, not merely "a particle appeared". Lifetime and light
    /// coords are exact constants in the Java source, not RNG-derived, so
    /// they are asserted exactly rather than as a range.
    #[test]
    fn sweep_attack_has_the_exact_vanilla_lifetime_and_colour_range() {
        let mut engine = ParticleEngine::seeded(100);
        sweep_attack(&mut engine, 0.0, 64.0, 0.0, 0.0);
        assert_eq!(engine.len(), 1, "sweep_attack must spawn exactly one quad");
        let p = &engine.particles()[0];
        assert_eq!(p.lifetime, 4, "AttackSweepParticle.lifetime = 4, hardcoded");
        assert!(matches!(p.behaviour, Behaviour::SweepAttack));
        assert!(matches!(
            p.sprite,
            SpriteSource::Sheet {
                sheet: Sheet::SweepAttack,
                ..
            }
        ));
        // `size == 0.0` (the real call site's value, see this fn's docs) means
        // `quadSize = 1.0 - 0.0 * 0.5 = 1.0` exactly, not a range.
        assert!(
            (p.quad_size - 1.0).abs() < 1e-6,
            "quad_size {} should be exactly 1.0 when size == 0.0",
            p.quad_size
        );
        // `nextFloat() * 0.6F + 0.4F` is bounded [0.4, 1.0) by the formula.
        for c in p.colour {
            assert!((0.4..1.0).contains(&c), "colour {c} outside 0.4..1.0");
        }
    }

    /// A negative control for the removal timing: `tick_sweep_attack` must
    /// remove the particle on exactly its 5th tick (ages 0..3 alive, age 4
    /// removed), reproducing Java's post-increment `age++ >= lifetime` check
    /// rather than an off-by-one pre/post variant.
    #[test]
    fn sweep_attack_dies_on_exactly_the_fifth_tick() {
        use lodestone_physics::{Aabb, CollisionView};
        struct Empty;
        impl CollisionView for Empty {
            fn collision_boxes(&self, _: i32, _: i32, _: i32, _: &mut Vec<Aabb>) {}
        }
        let mut engine = ParticleEngine::seeded(101);
        sweep_attack(&mut engine, 0.0, 64.0, 0.0, 0.0);
        for tick in 0..4 {
            engine.tick(&Empty);
            assert_eq!(
                engine.len(),
                1,
                "sweep_attack must still be alive after tick {tick}"
            );
        }
        engine.tick(&Empty);
        assert!(engine.is_empty(), "sweep_attack must be removed on tick 4");
    }

    /// `NoteParticle`'s colour formula is exact and external
    /// (`NoteParticle.java`'s three phase-shifted sines), so the expected
    /// value is computed independently here from the same formula rather than
    /// merely checking "some colour resulted".
    #[test]
    fn note_colour_matches_the_three_phase_shifted_sine_formula() {
        let mut engine = ParticleEngine::seeded(1);
        note(&mut engine, 0.0, 64.0, 0.0, 0.5);
        let p = &engine.particles()[0];
        assert_eq!(p.lifetime, 6, "NoteParticle hardcodes lifetime = 6");
        assert!(p.speed_up_when_y_blocked);
        let c = 0.5_f32;
        let tau = std::f32::consts::TAU;
        let expect = |offset: f32| ((c + offset) * tau).sin().mul_add(0.65, 0.35).max(0.0);
        let want = [expect(0.0), expect(0.333_333_34), expect(0.666_666_7)];
        for (got, want) in p.colour.iter().zip(want) {
            assert!(
                (got - want).abs() < 1e-6,
                "colour channel {got} != predicted {want}"
            );
        }
    }

    /// `HeartParticle` is physics-free with a fixed 16-tick life — both
    /// `heart` (breeding) and `angry_villager` share this constructor;
    /// `angry_villager` additionally raises the spawn point by 0.5 and uses a
    /// different sprite, which this test also pins.
    #[test]
    fn heart_and_angry_villager_share_physics_but_not_sprite_or_height() {
        let mut engine = ParticleEngine::seeded(2);
        heart(&mut engine, 1.0, 64.0, 1.0);
        angry_villager(&mut engine, 1.0, 64.0, 1.0);
        let particles = engine.particles();
        assert_eq!(particles.len(), 2);
        for p in particles {
            assert!(!p.has_physics, "HeartParticle sets hasPhysics = false");
            assert_eq!(p.lifetime, 16, "HeartParticle hardcodes lifetime = 16");
            assert!(matches!(p.behaviour, Behaviour::Heart));
        }
        assert!(matches!(
            particles[0].sprite,
            SpriteSource::Sheet {
                sheet: Sheet::Heart,
                ..
            }
        ));
        assert!(matches!(
            particles[1].sprite,
            SpriteSource::Sheet {
                sheet: Sheet::Angry,
                ..
            }
        ));
        assert!(
            (particles[1].y - 64.5).abs() < 1e-9,
            "angry_villager must raise the spawn point by 0.5, got y={}",
            particles[1].y
        );
        assert!(
            (particles[0].y - 64.0).abs() < 1e-9,
            "heart must not raise the spawn point"
        );
    }

    /// `SuspendedTownParticle`'s tick is a `lifetime`-countdown with no
    /// collision, not the usual `age`-increment: this pins that the particle
    /// survives exactly `lifetime` ticks of movement (not `lifetime + 1` or
    /// `lifetime - 1`, the two off-by-one variants a literal `age`-based
    /// rewrite would produce) and that it moves through solid geometry
    /// unimpeded, unlike every collision-driven behaviour in this module.
    #[test]
    fn happy_villager_survives_exactly_lifetime_ticks_and_ignores_collision() {
        use lodestone_physics::{Aabb, CollisionView};
        struct Wall;
        impl CollisionView for Wall {
            fn collision_boxes(&self, _x: i32, y: i32, _z: i32, out: &mut Vec<Aabb>) {
                if y == 64 {
                    out.push(Aabb::new(-10.0, 64.0, -10.0, 10.0, 65.0, 10.0));
                }
            }
        }
        let mut engine = ParticleEngine::seeded(3);
        happy_villager(&mut engine, 0.0, 64.5, 0.0, 5.0, 0.0, 0.0);
        let lifetime = engine.particles()[0].lifetime;
        assert!(lifetime > 0, "lifetime must be positive");
        // `lifetime--` is checked *before* decrementing on every tick, so the
        // field reaches 0 (without removing) after exactly `lifetime` ticks
        // of movement, and removal itself happens on tick `lifetime + 1` —
        // the post-decrement semantics `tick_suspended`'s own doc comment
        // spells out.
        for _ in 0..lifetime {
            assert_eq!(engine.len(), 1, "must still be alive during its `lifetime` ticks");
            engine.tick(&Wall);
        }
        assert_eq!(
            engine.len(),
            1,
            "must still be alive right after its `lifetime`th tick of movement"
        );
        engine.tick(&Wall);
        assert!(
            engine.is_empty(),
            "happy_villager must be removed on tick `lifetime + 1`"
        );

        // Positive control: the same nominal velocity through the *same* wall
        // must actually cross it, proving collision genuinely was skipped
        // rather than the wall never being consulted at all (e.g. the AABB
        // never overlapping the particle's own box).
        let mut engine = ParticleEngine::seeded(3);
        happy_villager(&mut engine, -1.0, 64.5, 0.0, 5.0, 0.0, 0.0);
        let start_x = engine.particles()[0].x;
        engine.tick(&Wall);
        let after_x = engine.particles()[0].x;
        assert!(
            after_x > start_x,
            "particle should have moved despite the wall at x={start_x}"
        );
    }

    /// `SpellParticle.WitchProvider` always tints magenta (`(1, 0, 1)` scaled
    /// by a shared brightness) — green is structurally impossible from this
    /// formula, which is the exact property that distinguishes "witch" from
    /// the green-tinted mob-effect variants of the same Java class (neither
    /// of which this pass builds, since they need `ColorParticleOption`
    /// decode).
    #[test]
    fn witch_particles_are_always_magenta_never_green() {
        let mut engine = ParticleEngine::seeded(4);
        for _ in 0..20 {
            witch(&mut engine, 0.0, 64.0, 0.0, 0.0, 0.0, 0.0);
        }
        for p in engine.particles() {
            assert!(!p.has_physics, "SpellParticle sets hasPhysics = false");
            assert!(matches!(p.behaviour, Behaviour::Spell));
            assert_eq!(p.colour[1], 0.0, "witch's green channel must be exactly 0");
            assert!(
                (0.35..0.85).contains(&p.colour[0]),
                "red {} outside nextFloat()*0.5+0.35's range",
                p.colour[0]
            );
            assert_eq!(
                p.colour[0], p.colour[2],
                "red and blue must match — the formula scales (1,0,1) by one shared brightness"
            );
        }
    }

    /// `TotemParticle`'s lifetime is `60 + nextInt(12)`, bounded to
    /// `[60, 72)`, and it takes its velocity **directly** from the caller
    /// with no jitter — unlike almost every other emitter in this module.
    #[test]
    fn totem_of_undying_lifetime_is_bounded_and_velocity_is_unjittered() {
        let mut engine = ParticleEngine::seeded(5);
        totem_of_undying(&mut engine, 0.0, 64.0, 0.0, 0.3, 0.7, -0.2);
        let p = &engine.particles()[0];
        assert!(
            (60..72).contains(&p.lifetime),
            "lifetime {} outside vanilla's 60 + nextInt(12) range",
            p.lifetime
        );
        assert!((p.xd - 0.3).abs() < 1e-12, "xd must equal the raw input");
        assert!((p.yd - 0.7).abs() < 1e-12, "yd must equal the raw input");
        assert!((p.zd - -0.2).abs() < 1e-12, "zd must equal the raw input");
        assert!(matches!(p.behaviour, Behaviour::SimpleAnimated { fade: None }));
    }

    /// `firework`'s lifetime is `48 + nextInt(12)`, bounded to `[48, 60)`,
    /// takes its velocity directly with no jitter (the same `TotemParticle`
    /// shape [`totem_of_undying_lifetime_is_bounded_and_velocity_is_unjittered`]
    /// pins), and — unlike totem — leaves colour at the base white and sets
    /// `alpha = 0.99` (`SparkProvider.createParticle`'s own line), never `1.0`.
    #[test]
    fn firework_lifetime_is_bounded_velocity_is_unjittered_and_alpha_is_099() {
        let mut engine = ParticleEngine::seeded(7);
        firework(&mut engine, 0.0, 64.0, 0.0, 0.4, -0.1, 0.6);
        let p = &engine.particles()[0];
        assert!(
            (48..60).contains(&p.lifetime),
            "lifetime {} outside vanilla's 48 + nextInt(12) range",
            p.lifetime
        );
        assert!((p.xd - 0.4).abs() < 1e-12, "xd must equal the raw input");
        assert!((p.yd - -0.1).abs() < 1e-12, "yd must equal the raw input");
        assert!((p.zd - 0.6).abs() < 1e-12, "zd must equal the raw input");
        assert_eq!(p.colour, [1.0, 1.0, 1.0], "SparkParticle never calls setColor");
        assert!((p.alpha - 0.99).abs() < 1e-6, "SparkProvider sets alpha to 0.99, not 1.0");
        assert!(matches!(p.behaviour, Behaviour::SimpleAnimated { fade: None }));
        assert!(
            matches!(p.sprite, SpriteSource::Sheet { sheet: Sheet::Spark, .. }),
            "firework must draw from its own Spark sheet, not Glow \
             (electric_spark/glow's sheet) — the two are visually similar but \
             physically distinct textures"
        );
    }

    /// `Sheet::Spark`'s frame order matches `firework.json`'s own declared
    /// list (`spark_7` first, `spark_0` last) — the same "the pack file order
    /// is the frame sequence, not an assumption" control every other
    /// multi-frame sheet's doc already carries.
    #[test]
    fn spark_sheet_frames_match_firework_json_order() {
        assert_eq!(
            Sheet::Spark.frames(),
            &["spark_7", "spark_6", "spark_5", "spark_4", "spark_3", "spark_2", "spark_1", "spark_0"]
        );
    }

    /// The 1-in-4 "golden" branch versus the usual "green" branch: both must
    /// be individually reachable, and — the magnitude check — a golden
    /// sample's red channel must exceed a green sample's, since the ranges
    /// (`0.6..0.8` vs `0.1..0.3`) are disjoint.
    #[test]
    fn totem_of_undying_has_two_disjoint_colour_populations() {
        let mut greens: u32 = 0;
        let mut goldens: u32 = 0;
        for seed in 0..200 {
            let mut engine = ParticleEngine::seeded(seed);
            totem_of_undying(&mut engine, 0.0, 64.0, 0.0, 0.0, 0.0, 0.0);
            let r = engine.particles()[0].colour[0];
            if r >= 0.6 {
                goldens += 1;
                assert!((0.6..0.8).contains(&r), "golden red {r} outside 0.6..0.8");
            } else {
                greens += 1;
                assert!((0.1..0.3).contains(&r), "green red {r} outside 0.1..0.3");
            }
        }
        assert!(greens > 0, "the ~75% green branch never fired in 200 draws");
        assert!(goldens > 0, "the ~25% golden branch never fired in 200 draws");
    }

    /// `HugeExplosionSeedParticle` is a `NoRenderParticle`, and it hardcodes
    /// `lifetime = 8` — overwriting the base constructor's own RNG-drawn
    /// lifetime, exactly the way `note`/`heart_particle` overwrite theirs.
    #[test]
    fn explosion_emitter_is_a_fixed_eight_tick_seed() {
        let mut engine = ParticleEngine::seeded(200);
        explosion_emitter(&mut engine, 0.0, 64.0, 0.0);
        assert_eq!(engine.len(), 1);
        let p = &engine.particles()[0];
        assert_eq!(p.lifetime, 8, "HugeExplosionSeedParticle hardcodes lifetime = 8");
        assert!(matches!(p.behaviour, Behaviour::HugeExplosionSeed));
    }

    /// The seed's `tick()` is a full override with no `super.tick()` call, so
    /// `age` must advance by exactly one per tick and removal must land on
    /// the tick where `age` *becomes* `lifetime` (`age == lifetime`, not
    /// `age > lifetime` — the off-by-one that would give it a 9th tick and
    /// spawn six explosion particles too many).
    ///
    /// This drives [`Particle::tick`] directly on a cloned copy of the seed,
    /// rather than through [`ParticleEngine::tick`], deliberately: the engine
    /// also ages every already-spawned `HugeExplosion` follow-up on the same
    /// call, and those have their *own* independently-rolled lifetime
    /// (`6 + nextInt(4)`, i.e. as low as 6) — a follow-up spawned on the
    /// seed's first tick can legitimately die of old age by the seed's 8th,
    /// which would make an assertion on `engine.particles().len()` flaky for
    /// a reason that has nothing to do with the seed's own schedule. Isolating
    /// the seed removes that confound entirely.
    #[test]
    fn explosion_emitter_seeds_six_explosions_per_tick_for_exactly_eight_ticks() {
        use lodestone_physics::{Aabb, CollisionView};
        struct Empty;
        impl CollisionView for Empty {
            fn collision_boxes(&self, _: i32, _: i32, _: i32, _: &mut Vec<Aabb>) {}
        }
        let mut engine = ParticleEngine::seeded(201);
        explosion_emitter(&mut engine, 0.0, 64.0, 0.0);
        let mut seed = engine.particles()[0].clone();

        let mut total_spawns = 0usize;
        for tick in 0..8 {
            assert!(seed.is_alive(), "seed died before its 8th tick, at tick {tick}");
            let spawns = seed.tick(&Empty);
            assert_eq!(
                spawns.len(),
                6,
                "tick {tick} produced {} spawns, expected exactly 6",
                spawns.len()
            );
            total_spawns += spawns.len();
        }
        assert!(!seed.is_alive(), "seed must be removed after exactly 8 ticks");
        assert_eq!(total_spawns, 48, "6 spawns/tick x 8 ticks = 48 total");
    }

    /// Every spawned `HugeExplosion` lands within the seed's own `± 4` block
    /// jitter box (`(nextDouble() - nextDouble()) * 4.0`, whose range is
    /// `(-4.0, 4.0)` since each `nextDouble()` is `[0, 1)`), and `size`
    /// (the seed's `age / lifetime`) is exactly one of the eight equally
    /// spaced values `0/8 .. 7/8` — it never reaches `8/8`, matching the
    /// "size read before the increment" ordering `tick_huge_explosion_seed`'s
    /// own doc comment calls out.
    #[test]
    fn every_seeded_explosion_lands_in_the_four_block_jitter_box_with_a_valid_size() {
        use lodestone_physics::{Aabb, CollisionView};
        struct Empty;
        impl CollisionView for Empty {
            fn collision_boxes(&self, _: i32, _: i32, _: i32, _: &mut Vec<Aabb>) {}
        }
        let mut engine = ParticleEngine::seeded(202);
        explosion_emitter(&mut engine, 10.0, 64.0, -5.0);
        for _ in 0..8 {
            engine.tick(&Empty);
        }
        let valid_sizes: Vec<f32> = (0..8).map(|i| i as f32 / 8.0).collect();
        for p in engine.particles() {
            assert!(
                (6.0..14.0).contains(&p.x),
                "x={} outside the seed's own ±4 jitter box around 10.0",
                p.x
            );
            assert!(
                (60.0..68.0).contains(&p.y),
                "y={} outside the seed's own ±4 jitter box around 64.0",
                p.y
            );
            assert!(
                (-9.0..-1.0).contains(&p.z),
                "z={} outside the seed's own ±4 jitter box around -5.0",
                p.z
            );
            let quad_size = p.quad_size;
            // `quadSize = 2.0 - size` for `size` in `{0/8, .., 7/8}`, so
            // `quad_size` must land in `{2.0, 1.875, .., 1.125}`.
            let matches_a_valid_size = valid_sizes
                .iter()
                .any(|&s| (quad_size - (2.0 - s)).abs() < 1e-5);
            assert!(
                matches_a_valid_size,
                "quad_size {quad_size} does not correspond to any of vanilla's eight \
                 size values 0/8..7/8"
            );
        }
    }

    /// `HugeExplosionParticle`'s own exact formulas, computed independently
    /// here from the Java source rather than read off the implementation:
    /// `lifetime` in `[6, 10)`, colour bounded by `nextFloat()*0.6+0.4` with
    /// all three channels equal (one draw, not three), full-bright, opaque,
    /// and `quadSize` the exact `2.0 - size` value for `size = 0.5` (the
    /// midpoint) — `2.0 * (1.0 - 0.5*0.5) = 2.0 * 0.75 = 1.5` exactly, not a
    /// range, since `size` is a caller-supplied constant here rather than
    /// RNG-derived.
    #[test]
    fn huge_explosion_matches_the_exact_vanilla_formulas() {
        let mut engine = ParticleEngine::seeded(203);
        huge_explosion(&mut engine, 1.0, 64.0, 2.0, 0.5);
        assert_eq!(engine.len(), 1);
        let p = &engine.particles()[0];
        assert!(
            (6..10).contains(&p.lifetime),
            "lifetime {} outside vanilla's 6 + nextInt(4) range [6, 10)",
            p.lifetime
        );
        assert!(matches!(p.behaviour, Behaviour::HugeExplosion));
        assert_eq!(p.colour[0], p.colour[1], "one grey draw, not three channels");
        assert_eq!(p.colour[1], p.colour[2]);
        assert!(
            (0.4..1.0).contains(&p.colour[0]),
            "colour {} outside nextFloat()*0.6+0.4's range",
            p.colour[0]
        );
        let want_quad_size = 2.0_f32 * (1.0 - 0.5 * 0.5);
        assert!(
            (p.quad_size - want_quad_size).abs() < 1e-6,
            "quad_size {} != predicted {want_quad_size} for size=0.5",
            p.quad_size
        );
        assert!(
            matches!(p.sprite, SpriteSource::Sheet { sheet: Sheet::Explosion, .. }),
            "must sample Sheet::Explosion, the 16-frame 'explosion_N' sheet, not 'generic'"
        );
    }

    /// The two extremes of `size` (`0.0` and `1.0`) must give the two
    /// extremes of the vanilla formula exactly: `quadSize = 2.0` and
    /// `quadSize = 1.0`.
    #[test]
    fn huge_explosion_quad_size_spans_exactly_two_to_one_over_size_zero_to_one() {
        let mut small = ParticleEngine::seeded(1);
        huge_explosion(&mut small, 0.0, 64.0, 0.0, 1.0);
        let shrunk = small.particles()[0].quad_size;
        assert!(
            (shrunk - 1.0).abs() < 1e-6,
            "size=1.0 must give quad_size=1.0 exactly, got {shrunk}"
        );

        let mut large = ParticleEngine::seeded(1);
        huge_explosion(&mut large, 0.0, 64.0, 0.0, 0.0);
        let full = large.particles()[0].quad_size;
        assert!(
            (full - 2.0).abs() < 1e-6,
            "size=0.0 must give quad_size=2.0 exactly, got {full}"
        );
    }

    /// A negative control for the seed's exclusion from rendering: a *plain*
    /// sheet particle at the same seed must still extract a quad, so
    /// `explosion_emitter`'s own particle producing zero quads is proof the
    /// exclusion fired, not that extraction is broken in general.
    #[test]
    fn the_seed_itself_never_extracts_a_quad_but_a_sibling_particle_still_does() {
        let mut engine = ParticleEngine::seeded(204);
        explosion_emitter(&mut engine, 0.0, 64.0, 0.0);
        crit(&mut engine, 5.0, 64.0, 5.0, 0.0, 0.0, 0.0);
        assert_eq!(engine.len(), 2, "the seed and the crit are both live particles");

        let mut out = Vec::new();
        engine.extract(
            lodestone_physics::Vec3d::ZERO,
            0.0,
            &|_, _, _| Some(0),
            &mut out,
        );
        assert_eq!(
            out.len(),
            1,
            "exactly one quad expected: the crit. The seed (NoRenderParticle) must \
             contribute none, even though it is still alive in the engine"
        );
    }

    /// `HugeExplosionParticle.getLightCoords` hardcodes vanilla's
    /// `15728880` (`FULL_BRIGHT`), independently of the world light sampler —
    /// mirrors `self_lit_particles_ignore_the_light_sampler_entirely` in
    /// `lib.rs`'s own test module for `SimpleAnimated`/`SweepAttack`.
    #[test]
    fn huge_explosion_is_full_bright_regardless_of_world_light() {
        let mut engine = ParticleEngine::seeded(205);
        huge_explosion(&mut engine, 0.0, 64.0, 0.0, 0.0);
        let mut out = Vec::new();
        engine.extract(
            lodestone_physics::Vec3d::ZERO,
            0.0,
            &|_, _, _| Some(0),
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].light,
            crate::FULL_BRIGHT,
            "HugeExplosionParticle.getLightCoords must return 15728880 unconditionally"
        );
    }

    /// A seeded burst of the whole emitter→follow-up chain must replay
    /// exactly — the same property `a_seeded_burst_replays_exactly` proves for
    /// `destroy_block_effect`, extended across the two-generation spawn this
    /// module's other emitters never need.
    #[test]
    fn a_seeded_explosion_chain_replays_exactly() {
        use lodestone_physics::{Aabb, CollisionView};
        struct Empty;
        impl CollisionView for Empty {
            fn collision_boxes(&self, _: i32, _: i32, _: i32, _: &mut Vec<Aabb>) {}
        }
        let run = |seed| {
            let mut e = ParticleEngine::seeded(seed);
            explosion_emitter(&mut e, 0.0, 64.0, 0.0);
            for _ in 0..8 {
                e.tick(&Empty);
            }
            e.particles()
                .iter()
                .map(|p| (p.x, p.y, p.z, p.lifetime, p.colour))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(500), run(500));
        assert_ne!(run(500), run(501));
    }

    /// `fly_towards_position`'s three velocity words are an **offset**, and the
    /// flight is a quartic sag rather than `Portal`'s linear rise. Both
    /// readings are evaluated here and the measurement must land on one.
    ///
    /// The wrong hypotheses are not hypothetical: `xd` reads as a velocity in
    /// every other emitter in this file, and the sibling closed-form behaviour
    /// (`Portal`) really does add a linear `1 - age/lifetime` to `y`. Either
    /// mistake still produces a live, drawable, plausibly-moving particle, so
    /// only a predicted value can separate them.
    #[test]
    fn an_enchant_glyph_starts_at_the_offset_and_sags_quartically_towards_the_table() {
        struct Empty;
        impl CollisionView for Empty {
            fn collision_boxes(&self, _: i32, _: i32, _: i32, _: &mut Vec<Aabb>) {}
        }
        // A pure +x offset with no vertical component, so the sag term is the
        // only thing that can move `y` at all.
        let (tx, ty, tz) = (0.0, 64.0, 0.0);
        let offset = 2.0;
        let mut e = ParticleEngine::seeded(9);
        fly_towards_position(&mut e, tx, ty, tz, offset, 0.0, 0.0, Sheet::Enchant);

        let start = &e.particles()[0];
        let lifetime = start.lifetime;
        assert!(
            (30..40).contains(&lifetime),
            "`(int)(nextFloat() * 10) + 30` must land in 30..=39, got {lifetime}"
        );
        assert!(
            (start.x - (tx + offset)).abs() < 1e-9,
            "a glyph must be drawn out at the bookshelf on its first frame \
             (x = {}), not at the table (x = {tx}) as a velocity reading gives",
            start.x
        );

        // Halfway through: `pos = 1 - a/L = 0.5`, so x is half the offset and
        // the sag is `(1 - 0.5)^4 * 1.2`. The linear (`Portal`) reading would
        // give `0.5 * 1.2 = 0.6` — eight times as deep.
        let half = lifetime / 2;
        for _ in 0..half {
            e.tick(&Empty);
        }
        let p = &e.particles()[0];
        #[expect(clippy::cast_precision_loss, reason = "tick counts are small")]
        let pos = 1.0 - (half as f64 / f64::from(lifetime));
        let quartic = ty - (1.0 - pos).powi(4) * 1.2;
        let linear = ty - (1.0 - pos) * 1.2;
        assert!(
            (p.x - offset * pos).abs() < 1e-6,
            "x must converge linearly on the table: got {}, want {}",
            p.x,
            offset * pos
        );
        assert!(
            (p.y - quartic).abs() < 1e-6,
            "y must follow the quartic sag ({quartic}), not the linear one ({linear}); got {}",
            p.y
        );

        // The final live frame is `age == lifetime`, **not** `lifetime - 1`:
        // the removal test reads the pre-increment `age`, so a particle
        // survives the tick that takes `age` up to `lifetime` and is dropped on
        // the one after. `pos` is then exactly `0`, putting the glyph on the
        // target horizontally while the sag is at its **deepest** — a full
        // `1.2` blocks, since `(1 - pos)^4` is maximal at `pos == 0`.
        //
        // So a glyph finishes its flight diving *into* the table rather than
        // resting on it, and "it lands on the target" — the plausible round
        // answer, and this test's first prediction — is wrong by 1.2 blocks.
        for _ in half..lifetime {
            e.tick(&Empty);
        }
        let last = &e.particles()[0];
        assert!(
            last.x.abs() < 1e-9 && (last.y - (ty - 1.2)).abs() < 1e-6,
            "final live frame must be (0, {}), got ({}, {})",
            ty - 1.2,
            last.x,
            last.y
        );
        e.tick(&Empty);
        assert!(
            e.particles().is_empty(),
            "the glyph must be removed the tick `age` reaches `lifetime`"
        );
    }
}

// ---------------------------------------------------------------------------
// Ambient and environmental types
// ---------------------------------------------------------------------------
//
// Vanilla's `RisingParticle` is the shared base for `flame`, `soul_fire_flame`
// and `soul`, and its whole constructor is the four lines [`rising`] transcribes.
// `flame` above predates it and keeps its own copy; everything below goes through
// this one.

/// `RisingParticle`'s constructor: `friction = 0.96`, the requested velocity with
/// a 1% scatter, a ±0.05 positional jitter and a `8 / (rand*0.8 + 0.2) + 4`
/// lifetime.
///
/// The velocity line is `this.xd * 0.01F + xd`: the *scattered* component is
/// almost entirely discarded and replaced by what the caller asked for, which is
/// what makes a rising column tight rather than a puff.
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
    let jitter = |r: &mut JavaRandom| f64::from((r.next_float() - r.next_float()) * 0.05);
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

/// `SOUL_FIRE_FLAME` — `FlameParticle.Provider` over the `soul_fire_flame`
/// sprite, so the physics are `flame`'s exactly and only the sheet differs.
pub fn soul_fire_flame(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xd: f64, yd: f64, zd: f64) {
    let mut p = rising(engine, x, y, z, xd, yd, zd, Sheet::SoulFireFlame);
    p.behaviour = Behaviour::Flame;
    engine.add(p);
}

/// `SoulParticle` — a rising, sheet-animated mote, 1.5× scale and translucent.
///
/// [`Behaviour::AshSmoke`] is the right behaviour despite the name: what that
/// variant *does* is "ordinary physics, advance the sheet by age", which is
/// `SoulParticle.tick`'s `super.tick(); setSpriteFromAge(sprites);` verbatim.
/// Unlike `flame` it does **not** override `move`, so a soul mote collides.
pub fn soul(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xd: f64, yd: f64, zd: f64) {
    let mut p = rising(engine, x, y, z, xd, yd, zd, Sheet::Soul);
    p.scale(1.5);
    p.alpha = 1.0;
    p.behaviour = Behaviour::AshSmoke;
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Soul,
        frame: Sheet::Soul.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

/// `PortalParticle` — the nether-portal / ender shimmer.
///
/// `xd/yd/zd` here are an **amplitude**, not a velocity: [`Behaviour::Portal`]
/// recomputes the position from [`Particle::spawn`] every tick and never damps
/// them. The caller passes the offset the mote should converge *from*, which for
/// a portal block is a unit-normal-distributed offset and for
/// `EnderMan`/chorus-fruit teleports is the distance travelled.
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

/// `CampfireSmokeParticle` — the tall column over a campfire.
///
/// `signal` picks between the two lifetimes, and they are far apart on purpose:
/// `rand(50) + 80` cosy against `rand(50) + 280` signal, which is the whole
/// reason a signal fire's plume reaches above the treeline.
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
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::BigSmoke,
            frame: 0,
        },
        rng,
    );
    p.scale(3.0);
    p.set_size(0.25, 0.25);
    let base = if signal { 280 } else { 80 };
    p.lifetime = engine.rng().next_int_bound(50) + base;
    p.gravity = 3.0e-6;
    p.xd = xa;
    p.yd = ya + f64::from(rng_next(engine)) / 500.0;
    p.zd = za;
    p.behaviour = Behaviour::CampfireSmoke;
    engine.add(p);
}

/// `EndRodParticle` — a `SimpleAnimatedParticle` at `gravity = 0.0125` that fades
/// toward `0xF2E9C9` and, like the flame, passes through the block it sits on.
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
    p.lifetime = 60 + engine.rng().next_int_bound(12);
    // `has_physics = false` rather than `Behaviour::Flame`: vanilla overrides
    // `move` to skip collision but keeps the ordinary base tick, and the `Flame`
    // behaviour would take flame's own quad-size curve with it.
    p.has_physics = false;
    p.behaviour = Behaviour::SimpleAnimated {
        // `setFadeColor(15916745)` == `0xF2D9C9`, split the way
        // `SimpleAnimatedParticle.setFadeColor` splits it: each channel `/ 255`.
        fade: Some([0xF2 as f32 / 255.0, 0xDE as f32 / 255.0, 0xC9 as f32 / 255.0]),
    };
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Glitter,
        frame: Sheet::Glitter.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

/// A single-sprite spark: `ELECTRIC_SPARK` and `GLOW`, both `particle/glow`.
///
/// `ElectricSparkParticle` is a plain `SingleQuadParticle` with `friction = 0.9`,
/// a 0.25 velocity scale and a short life; `GlowParticle` is a
/// `SimpleAnimatedParticle`, but over a one-frame sheet the two are visually the
/// same thing and share this emitter.
pub fn spark(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::Glow,
            frame: 0,
        },
        rng,
    );
    p.friction = 0.9;
    p.gravity = 0.0;
    let scale = 0.25;
    p.xd = xa * scale;
    p.yd = ya * scale;
    p.zd = za * scale;
    p.lifetime = 8 + engine.rng().next_int_bound(4);
    p.behaviour = Behaviour::Plain;
    engine.add(p);
}

/// `FireworkParticles.SparkParticle` via `SparkProvider` — `ParticleTypes.FIREWORK`,
/// the plain wire-spawned spark (not the rocket-explosion burst, which is a
/// client-side-only `Starter`/`NoRenderParticle` this client never receives
/// as a wire particle at all).
///
/// `.cache/mc/26.2/client-src/net/minecraft/client/particle/FireworkParticles.java`:
/// `SparkParticle`'s constructor is `super(level, x, y, z, sprites, 0.1F)` —
/// `SimpleAnimatedParticle`'s third-from-last parameter is **gravity**, not a
/// size scale (confirmed against `SimpleAnimatedParticle.java`'s own
/// constructor, which the [`totem_of_undying`] doc already reads the same
/// way), and that base constructor also hardcodes `friction = 0.91F`
/// unconditionally — `SparkParticle` never overrides either back down the way
/// [`totem_of_undying`]'s `TotemParticle` does. Velocity is taken **directly**
/// from the caller with no jitter (`xd = xa` etc., matching `TotemParticle`
/// again), `quadSize *= 0.75F`, `lifetime = 48 + nextInt(12)`, no colour set
/// (stays the base white), and `SparkProvider.createParticle` — the only
/// creation path a plain `SimpleParticleType` particle reaches — sets
/// `alpha = 0.99F` on every instance. `trail`/`twinkle` both default `false`
/// and are never set here; they only matter for the child sparks a rocket's
/// own `Starter` spawns from its `tick()`, which is a different, client-only
/// production path this emitter does not model.
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
    let extra = engine.rng().next_int_bound(12);
    p.lifetime = 48 + extra;
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Spark,
        frame: Sheet::Spark.frame_for_age(0, p.lifetime),
    };
    p.alpha = 0.99;
    p.behaviour = Behaviour::SimpleAnimated { fade: None };
    engine.add(p);
}

/// An animated ambient sheet with ordinary physics — `SCULK_CHARGE`, `GUST`,
/// `SMALL_GUST` and `SONIC_BOOM`, which differ from each other in sheet, scale
/// and lifetime rather than in tick shape.
///
/// [`Behaviour::AshSmoke`] again for [`soul`]'s reason: it means "advance the
/// sheet by age", which is all `setSpriteFromAge` in each of these classes does.
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

/// `FlyTowardsPositionParticle` — the enchanting-table glyphs (`enchant`, over
/// [`Sheet::Enchant`]'s twenty-six Standard Galactic letters) and the conduit's
/// homing mote (`nautilus`). Vanilla's `EnchantProvider` and `NautilusProvider`
/// are byte-identical apart from the sprite set.
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
    reason = "mirrors the `FlyTowardsPositionParticle` constructor argument for argument, \
              plus the sheet its provider supplies"
)]
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
    let frame = engine.rng().next_int_bound(i32::from(sheet.frame_count()));
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

/// A `DripParticle` — hanging, falling or landing.
///
/// The three sheets are the three phases vanilla models as separate particle
/// types (`dripping_*` hangs under a block, `falling_*` is in free fall,
/// `landing_*` is the splash), and `colour` is the fluid's own: water is
/// `0x2389D8`-ish and lava a hot orange, which is the only thing distinguishing a
/// water drip from a lava drip on screen since both share the sprite.
pub fn drip(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    sheet: Sheet,
    colour: [f32; 3],
    gravity: f32,
) {
    let rng = engine.rng();
    let mut p = Particle::new(x, y, z, SpriteSource::Sheet { sheet, frame: 0 }, rng);
    p.xd = 0.0;
    p.yd = 0.0;
    p.zd = 0.0;
    p.gravity = gravity;
    p.colour = colour;
    p.set_size(0.01, 0.01);
    // `DripParticle`'s own `lifetime = (int)(64.0 / (random.nextDouble() * 0.8 +
    // 0.2))` — a hanging drip waits a long, *variable* time before it falls,
    // which is what stops a cave ceiling dripping in lockstep.
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    let lifetime = (64.0 / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime.max(1);
    p.behaviour = Behaviour::WaterDrop;
    engine.add(p);
}

/// `DustParticleBase.randomizeColor` — a fresh `nextFloat` draw per call, so
/// three calls (r, g, b) each consume their own random number even though
/// `baseFactor` is shared across all three.
fn randomize_dust_channel(engine: &mut ParticleEngine, channel: f32, base_factor: f32) -> f32 {
    rng_next(engine).mul_add(0.2, 0.8) * channel * base_factor
}

/// Shared `DustParticleBase` constructor body — the physics and sizing every
/// `minecraft:dust`-family particle has in common
/// (`.cache/mc/26.2/client-src/net/minecraft/client/particle/DustParticleBase.java`).
/// `color` starts at `SingleQuadParticle`'s draw-quad-size point (already run
/// by [`Particle::with_velocity`]), matching the constructor order: `super(...)`
/// runs the velocity jitter and quad-size draw, *then* `xd/yd/zd *= 0.1`,
/// *then* the lifetime redraw below.
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
    // `(int)(8.0 / (random.nextDouble() * 0.8 + 0.2))`, then
    // `(int) Math.max(baseLifetime * scale, 1.0F)`.
    #[expect(clippy::cast_possible_truncation, reason = "Java's `(int)` cast; small")]
    let base_lifetime = (8.0 / engine.rng().next_double().mul_add(0.8, 0.2)) as i32;
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

/// `DustParticle.Provider` (`ParticleTypes.DUST`, the wire particle
/// `minecraft:dust` decodes into).
///
/// `color` is `DustParticleOptions::getColor()` — the packed RGB24 already
/// unpacked to `[0, 1]` components — and `scale` its `ScalableParticleOptionsBase`
/// scale. The colour is randomised once here (`DustParticle`'s constructor
/// body, which runs *after* `DustParticleBase`'s) and held for the particle's
/// whole life; see [`dust_color_transition`] for the sibling that doesn't.
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

/// `DustColorTransitionParticle.Provider` (`ParticleTypes.DUST_COLOR_TRANSITION`,
/// `minecraft:dust_color_transition` — the sculk-sensor/sculk-shrieker particle).
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
