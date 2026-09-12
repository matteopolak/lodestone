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
//! Vanilla reads its own block-state outline-shape accessor (the **outline**
//! shape, the one the selection box traces) rather than the collision shape. The two differ for a
//! meaningful set of blocks: `short_grass` has a small outline and *no*
//! collision at all, so driving a break burst from collision geometry would emit
//! nothing when a player breaks grass — one of the most common actions there is.
//!
//! So these functions take the boxes as an argument rather than querying a
//! world. The renderer already knows the true outline geometry from the block
//! model, which makes it the correct source, and it keeps this crate free of a
//! dependency on any particular world representation.

use super::*;

use crate::rng::JavaRandom;
use crate::{
    Behaviour, DripKind, DripPhase, Layer, Particle, ParticleEngine, Sheet, SpriteSource,
};
use lodestone_data::block_states::StateId;
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

/// Vanilla's own terrain-particle constructor — one fragment of a block.
///
/// `tint` is the block's colour multiplier at this position (grass and foliage
/// are biome-tinted; everything else is white). Vanilla starts every terrain
/// particle at `0.6` grey and multiplies the tint into it, which is why block
/// fragments always look slightly darker than the block they came from.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own terrain-particle constructor argument for argument"
)]
#[must_use]
pub fn terrain_particle(
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    state: StateId,
    tint: [f32; 3],
    rng: &mut JavaRandom,
) -> Particle {
    let mut p = Particle::with_velocity(
        x,
        y,
        z,
        xa,
        ya,
        za,
        SpriteSource::BlockState(state),
        rng,
    );
    p.gravity = 1.0;
    p.colour = [0.6 * tint[0], 0.6 * tint[1], 0.6 * tint[2]];
    p.quad_size /= 2.0;
    // Two more draws, *after* the colour is set — order matters for replay.
    let uo = rng.next_f32() * 3.0;
    let vo = rng.next_f32() * 3.0;
    p.behaviour = Behaviour::Terrain { uo, vo };
    p
}

/// Vanilla's own "add destroy block effect" step — the burst when a block is destroyed.
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
    state: StateId,
    tint: [f32; 3],
    shape: &[Aabb],
) {
    /// `double density = 0.25` in vanilla's own "add destroy block effect" step.
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

/// Vanilla's own subdivision-count formula: at least 2, else vanilla's own
/// quantized ceiling of `width / density`.
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

/// Vanilla's own "add breaking block effect" step — the single fragment that pops off the
/// face a player is currently mining.
///
/// Vanilla emits one of these every few ticks while a dig is in progress, which
/// together with the crack overlay is what makes mining feel like it is doing
/// something. The particle spawns just *outside* the struck face (by `0.1`) so
/// it is not immediately swallowed by the block it came from.
pub fn breaking_block_effect(
    engine: &mut ParticleEngine,
    block: (i32, i32, i32),
    state: StateId,
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
        .next_f64()
        .mul_add(shape.max_x - shape.min_x - 0.2, 0.1)
        + x
        + shape.min_x;
    let mut yp = rng
        .next_f64()
        .mul_add(shape.max_y - shape.min_y - 0.2, 0.1)
        + y
        + shape.min_y;
    let mut zp = rng
        .next_f64()
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
    // Vanilla's own "set power" step at `0.2F` then a `0.6F` scale — a mining chip is slower and smaller than a
    // destruction fragment.
    p.set_power(0.2);
    p.scale(0.6);
    engine.add(p);
}

/// Vanilla's own terrain-particle provider — the wire-driven `minecraft:block` particle.
///
/// The plain provider: a [`terrain_particle`] built from the packet's own
/// position and velocity, with nothing overridden afterwards. This is *not* the
/// same thing as [`destroy_block_effect`], which is the local block-break burst
/// and derives sixty-four positions from a block's outline shape; a server that
/// sends `minecraft:block` is asking for exactly one fragment where it said.
pub fn block_fragment(
    engine: &mut ParticleEngine,
    pos: [f64; 3],
    vel: [f64; 3],
    state: StateId,
    tint: [f32; 3],
) {
    let p = terrain_particle(
        pos[0], pos[1], pos[2], vel[0], vel[1], vel[2], state, tint, engine.rng(),
    );
    engine.add(p);
}

/// Vanilla's own terrain-particle crumbling provider (`minecraft:block_crumble`)
/// — the flecks a creaking heart and a trial-spawner ejection shed off a block.
///
/// A [`terrain_particle`] whose velocity is then **discarded entirely** and
/// whose lifetime is re-rolled short: `setParticleSpeed(0, 0, 0)` and
/// `setLifetime(nextInt(10) + 1)`, so a crumb hangs where it was placed for at
/// most half a second. The construction still happens with the packet's
/// velocity — vanilla builds the particle first and overrides afterwards — so
/// the RNG draws that jitter it are made and thrown away, exactly as there.
pub fn block_crumble(
    engine: &mut ParticleEngine,
    pos: [f64; 3],
    vel: [f64; 3],
    state: StateId,
    tint: [f32; 3],
) {
    let mut p = terrain_particle(
        pos[0], pos[1], pos[2], vel[0], vel[1], vel[2], state, tint, engine.rng(),
    );
    p.xd = 0.0;
    p.yd = 0.0;
    p.zd = 0.0;
    p.lifetime = engine.rng().next_i32_bound(10) + 1;
    engine.add(p);
}

/// Vanilla's own terrain-particle dust-pillar provider (`minecraft:dust_pillar`) — the column a
/// mace's smash attack throws up out of the ground it lands on.
///
/// A [`terrain_particle`] whose velocity is replaced by
/// `(gaussian/30, ya + gaussian/2, gaussian/30)` and whose lifetime is re-rolled
/// to `nextInt(20) + 20`. The vertical term is the packet's **own** `ya` plus a
/// gaussian, not a gaussian alone — that additive base is the whole reason the
/// pillar goes up rather than merely dispersing, and it is the one term a
/// reading of "three gaussians at different scales" loses.
pub fn dust_pillar(
    engine: &mut ParticleEngine,
    pos: [f64; 3],
    vel: [f64; 3],
    state: StateId,
    tint: [f32; 3],
) {
    let mut p = terrain_particle(
        pos[0], pos[1], pos[2], vel[0], vel[1], vel[2], state, tint, engine.rng(),
    );
    p.xd = gaussian(engine) / 30.0;
    p.yd = vel[1] + gaussian(engine) / 2.0;
    p.zd = gaussian(engine) / 30.0;
    p.lifetime = engine.rng().next_i32_bound(20) + 20;
    engine.add(p);
}

/// Vanilla's own block-marker provider (`minecraft:block_marker`) — the ghost block a light
/// block or a barrier shows while you hold its item.
///
/// The only member of the block-particle-option family that is **not** a
/// vanilla terrain particle, and every one of its four constructor lines is a
/// departure from one: it takes the block's *whole* particle sprite rather than
/// a random quarter, it is untinted (no `0.6` grey, no tint-source multiply),
/// it has `gravity = 0`, `hasPhysics = false` and no velocity, and its
/// its own quad-size accessor returns a flat `0.5F` for its whole 80-tick life.
///
/// That last one is why this carries [`Behaviour::Plain`] rather than a variant
/// of its own: `Plain`'s size is `quad_size` unchanged, so setting the field to
/// `0.5` *is* the override. The constructed random size is drawn and discarded,
/// as in vanilla, since the draw happens in the superclass constructor.
pub fn block_marker(engine: &mut ParticleEngine, pos: [f64; 3], state: StateId) {
    let mut p = Particle::new(
        pos[0],
        pos[1],
        pos[2],
        SpriteSource::BlockState(state),
        engine.rng(),
    );
    p.gravity = 0.0;
    p.lifetime = 80;
    p.has_physics = false;
    p.quad_size = 0.5;
    engine.add(p);
}

/// Vanilla's own falling-dust-particle provider (`minecraft:falling_dust`) — the trickle under
/// an unsupported sand, gravel or concrete-powder column.
///
/// Textured from [`Sheet::Generic`] and **tinted** from the block, which is the
/// inverse of the other four in this family: they wear the block's own atlas
/// sprite and (mostly) no tint, this one wears a generic grey mote and carries
/// all of the block's identity in its colour.
///
/// `tint` is that colour, already resolved by the caller. Vanilla resolves it
/// through a three-step chain — vanilla's own falling-block dust-color accessor, else the block's
/// tint source, else `state.getMapColor(level, pos).col` — and this client has
/// data for the middle step only, so an untinted block arrives here as white
/// rather than as its map colour. See `docs/particle-catalogue.md`; the visible
/// consequence is that a sand mote is pale rather than sand-coloured, not that
/// it is missing.
///
/// Note the lifetime is **two** expressions, not one: a base
/// `(int)(32.0 / (nextFloat() * 0.8 + 0.2))` and then
/// `(int) max(base * 0.9F, 1.0F)`. Folding the `0.9` into the divisor changes
/// the truncation point and therefore the distribution, and the `max(…, 1)`
/// floor is what stops a mote with a zero-length life from being drawn at all.
pub fn falling_dust(engine: &mut ParticleEngine, pos: [f64; 3], tint: [f32; 3]) {
    let mut p = Particle::new(
        pos[0],
        pos[1],
        pos[2],
        SpriteSource::Sheet { sheet: Sheet::Generic, frame: 0 },
        engine.rng(),
    );
    p.colour = tint;
    p.quad_size *= 0.674_999_95;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates towards zero; reproduced deliberately"
    )]
    {
        let base = (32.0 / f64::from(rng_next(engine).mul_add(0.8, 0.2))) as i32;
        #[expect(
            clippy::cast_precision_loss,
            reason = "the base lifetime is at most 160; Java promotes it to float here"
        )]
        let scaled = (base as f32 * 0.9).max(1.0);
        p.lifetime = scaled as i32;
    }
    let rot_speed = (rng_next(engine) - 0.5) * 0.1;
    p.roll = rng_next(engine) * core::f32::consts::TAU;
    p.behaviour = Behaviour::FallingDust { rot_speed };
    // Vanilla's constructor also calls `setSpriteFromAge(sprites)`. That is a
    // no-op at construction — `frame_for_age` at age 0 is frame 0 for every
    // sheet, which is what `Sheet::Generic`'s first texture already is — so it
    // is deliberately not repeated here rather than accidentally omitted.
    engine.add(p);
}

/// Vanilla's own breaking-item particle constructor — one crumb of an item.
///
/// The same shape as [`terrain_particle`] and deliberately so: vanilla's
/// own breaking-item particle and its terrain particle have **byte-identical**
/// its own U0/U1/V0/V1 accessor overrides (a quarter sub-sprite at
/// `(uo + 1) / 4 .. uo / 4`, `uo`/`vo` each `random.nextFloat() * 3.0F`), the same
/// `gravity = 1.0F` and the same `quadSize /= 2.0F`, so [`Behaviour::Terrain`]
/// describes both. Only the sprite source and the absence of a `0.6` grey differ:
/// an item crumb is drawn at full brightness.
///
/// # The velocity is *not* vanilla's own "set power" step
///
/// Vanilla's own breaking-item particle's public constructor chains to the
/// zero-velocity one and then does `xd *= 0.1F; … xd += xa;`, a **plain multiply
/// of all three
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
    item: lodestone_data::item::Item,
    rng: &mut JavaRandom,
) -> Particle {
    // The 4-argument constructor: zero given velocity, so the whole of `xd/yd/zd`
    // is `Particle`'s own randomised jitter.
    let mut p = Particle::with_velocity(x, y, z, 0.0, 0.0, 0.0, SpriteSource::Item(item), rng);
    p.gravity = 1.0;
    p.quad_size /= 2.0;
    // Two more draws, *after* the quad size — order matters for replay, exactly as
    // in `terrain_particle`.
    let uo = rng.next_f32() * 3.0;
    let vo = rng.next_f32() * 3.0;
    p.behaviour = Behaviour::Terrain { uo, vo };
    // `xd *= 0.1F; yd *= 0.1F; zd *= 0.1F; xd += xa; …` — see the note above on why
    // this is not `set_power`.
    p.xd = p.xd * 0.1 + xa;
    p.yd = p.yd * 0.1 + ya;
    p.zd = p.zd * 0.1 + za;
    p
}

/// Vanilla's own "spawn item particles" step — the crumbs that fly from
/// an entity's mouth while it eats, and the same burst vanilla's own "break item"
/// step throws when a tool snaps.
///
/// `count` is **5** per periodic emission while consuming and **16** on the final
/// bite (vanilla's own item "on use tick" and "on consume" steps respectively); it is a
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
    reason = "mirrors vanilla's own \"spawn item particles\" step plus the eye position and facing it reads off the entity"
)]
pub fn spawn_item_particles(
    engine: &mut ParticleEngine,
    eye_x: f64,
    eye_y: f64,
    eye_z: f64,
    x_rot_deg: f32,
    y_rot_deg: f32,
    item: lodestone_data::item::Item,
    count: u32,
) {
    let x_rad = -x_rot_deg.to_radians();
    let y_rad = -y_rot_deg.to_radians();
    for _ in 0..count {
        let (dx, dy, dz) = {
            let rng = engine.rng();
            // `new Vec3((nextFloat() - 0.5) * 0.1, nextFloat() * 0.1 + 0.1, 0.0)`
            let d = (
                (f64::from(rng.next_f32()) - 0.5) * 0.1,
                f64::from(rng.next_f32()).mul_add(0.1, 0.1),
                0.0,
            );
            let d = x_rot(d, x_rad);
            y_rot(d, y_rad)
        };
        let (px, py, pz) = {
            let rng = engine.rng();
            // `double y1 = -nextFloat() * 0.6 - 0.3;`
            // `new Vec3((nextFloat() - 0.5) * 0.3, y1, 0.6)` — note vanilla draws
            // `y1` *before* the horizontal jitter, so the two RNG draws
            // are in that order and swapping them desynchronises the sequence.
            let y1 = (-f64::from(rng.next_f32())).mul_add(0.6, -0.3);
            let p = ((f64::from(rng.next_f32()) - 0.5) * 0.3, y1, 0.6);
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

/// Vanilla's own vector X-rotation.
fn x_rot((x, y, z): (f64, f64, f64), radians: f32) -> (f64, f64, f64) {
    let (cos, sin) = (f64::from(radians.cos()), f64::from(radians.sin()));
    (x, y * cos + z * sin, z * cos - y * sin)
}

/// Vanilla's own vector Y-rotation.
fn y_rot((x, y, z): (f64, f64, f64), radians: f32) -> (f64, f64, f64) {
    let (cos, sin) = (f64::from(radians.cos()), f64::from(radians.sin()));
    (x * cos + z * sin, y, z * cos - x * sin)
}
