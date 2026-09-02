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

use crate::rng::JavaRandom;
use crate::{
    Behaviour, DripKind, DripPhase, Layer, Particle, ParticleEngine, Sheet, SpriteSource,
};
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
    state: u32,
    tint: [f32; 3],
    rng: &mut JavaRandom,
) -> Particle {
    let mut p = Particle::with_velocity(x, y, z, xa, ya, za, SpriteSource::BlockState(state), rng);
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
    state: u32,
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
    state: u32,
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
    state: u32,
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
    state: u32,
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
pub fn block_marker(engine: &mut ParticleEngine, pos: [f64; 3], state: u32) {
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

/// The parameters vanilla's own base ash/smoke particle's constructor takes, which is what
/// separates its five subclasses from one another.
///
/// Vanilla's base takes eight of these positionally after the coordinates, so a
/// caller reads as a wall of bare floats in which two adjacent same-typed
/// arguments transpose without a trace. Naming them is not decoration: `smoke`
/// and `ash` differ only in `dir.1`'s **sign**, `colour_random`, `max_lifetime`
/// and `gravity`'s sign, and every one of those is a lone number.
#[derive(Debug, Clone, Copy)]
pub struct AshSmokeParams {
    /// `dirX/dirY/dirZ` — per-axis damping applied to the *scattered* velocity
    /// before the caller's own is added. Negative flips that axis.
    pub dir: [f32; 3],
    /// Vanilla's own "color random" field — the greyscale tint is
    /// `nextFloat() * colorRandom`, so `0.0` means black before any color
    /// override the subclass applies.
    pub colour_random: f32,
    /// Vanilla's own "max lifetime" field — the numerator of `(int)(maxLifetime / (nextFloat() * 0.8
    /// + 0.2) * scale)`.
    pub max_lifetime: i32,
    /// `gravity`. Negative rises.
    pub gravity: f32,
    /// Vanilla's own "has physics" field — smoke collides, ash does not.
    pub has_physics: bool,
}

/// Vanilla's own base ash/smoke particle's constructor, shared by `smoke`,
/// `large_smoke`, `ash`, `white_ash` and `white_smoke`.
///
/// Vanilla's own "set sprite from age" step runs at the end of the constructor, before the first
/// tick, which is why the sprite is re-stamped here rather than left on frame
/// zero.
pub fn base_ash_smoke(
    engine: &mut ParticleEngine,
    (x, y, z): (f64, f64, f64),
    (xa, ya, za): (f64, f64, f64),
    sheet: Sheet,
    scale: f32,
    params: AshSmokeParams,
) -> Particle {
    let rng = engine.rng();
    let mut p =
        Particle::with_velocity(x, y, z, 0.0, 0.0, 0.0, SpriteSource::Sheet { sheet, frame: 0 }, rng);
    p.friction = 0.96;
    p.gravity = params.gravity;
    // Smoke that hits a ceiling spreads sideways instead of piling up.
    p.speed_up_when_y_blocked = true;
    p.xd = p.xd.mul_add(f64::from(params.dir[0]), xa);
    p.yd = p.yd.mul_add(f64::from(params.dir[1]), ya);
    p.zd = p.zd.mul_add(f64::from(params.dir[2]), za);
    let col = rng_next(engine) * params.colour_random;
    p.colour = [col, col, col];
    p.quad_size *= 0.75 * scale;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(params.max_lifetime) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)
        * f64::from(scale)) as i32;
    p.lifetime = lifetime.max(1);
    p.has_physics = params.has_physics;
    p.behaviour = Behaviour::AshSmoke;
    p.sprite = SpriteSource::Sheet {
        sheet,
        frame: sheet.frame_for_age(0, p.lifetime),
    };
    p
}

/// Vanilla's own smoke particle — the base ash/smoke particle with smoke's parameters
/// (`0.3` colour jitter, 8-tick base lifetime, `-0.1` gravity so it rises).
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own smoke-particle constructor argument for argument"
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
    let p = base_ash_smoke(
        engine,
        (x, y, z),
        (xa, ya, za),
        Sheet::Generic,
        scale,
        AshSmokeParams {
            dir: [0.1, 0.1, 0.1],
            colour_random: 0.3,
            max_lifetime: 8,
            gravity: -0.1,
            has_physics: true,
        },
    );
    engine.add(p);
}

/// Vanilla's own white-smoke particle — smoke's parameters exactly, over a
/// fixed lilac-grey tint (`0xBAB1C2`) rather than the greyscale draw.
///
/// The color-random draw still happens (it is inside the base constructor) and
/// is then overwritten, so the RNG stream length matches vanilla's.
pub fn white_smoke(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let mut p = base_ash_smoke(
        engine,
        (x, y, z),
        (xa, ya, za),
        Sheet::Generic,
        1.0,
        AshSmokeParams {
            dir: [0.1, 0.1, 0.1],
            colour_random: 0.3,
            max_lifetime: 8,
            gravity: -0.1,
            has_physics: true,
        },
    );
    p.colour = ASH_WHITE;
    engine.add(p);
}

/// Vanilla's own ash particle — the black flakes drifting down through the soul sand valley.
///
/// Three sign-level differences from [`smoke`], each a lone number in vanilla's
/// positional argument list: the vertical scatter direction is **negative**
/// (the scattered vertical component is inverted), gravity is **positive**
/// `0.1` so it falls rather than rises, and collision is **off** so it drifts
/// through the terrain.
/// Its sheet is [`Sheet::Generic0`], a single frame — `ash.json` names one
/// texture, so ash does not animate at all.
pub fn ash(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let p = base_ash_smoke(
        engine,
        (x, y, z),
        (0.0, 0.0, 0.0),
        Sheet::Generic0,
        1.0,
        AshSmokeParams {
            dir: [0.1, -0.1, 0.1],
            colour_random: 0.5,
            max_lifetime: 20,
            gravity: 0.1,
            has_physics: false,
        },
    );
    engine.add(p);
}

/// Vanilla's own white-ash particle — the basalt delta's pale drift.
///
/// Vanilla's own ash particle's shape with a far gentler `0.0125` gravity, a
/// color-random field of **zero** (so the greyscale draw yields black and the fixed tint below is
/// the whole colour), and a provider-supplied initial velocity: three products
/// of two `nextFloat()`s each, all negative, so the flakes always drift down
/// and toward `-x`/`-z`.
pub fn white_ash(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let rng = engine.rng();
    let xa = f64::from(rng.next_f32()) * -1.9 * f64::from(rng.next_f32()) * 0.1;
    let ya = f64::from(rng.next_f32()) * -0.5 * f64::from(rng.next_f32()) * 0.1 * 5.0;
    let za = f64::from(rng.next_f32()) * -1.9 * f64::from(rng.next_f32()) * 0.1;
    let mut p = base_ash_smoke(
        engine,
        (x, y, z),
        (xa, ya, za),
        Sheet::Generic0,
        1.0,
        AshSmokeParams {
            dir: [0.1, -0.1, 0.1],
            colour_random: 0.0,
            max_lifetime: 20,
            gravity: 0.0125,
            has_physics: false,
        },
    );
    p.colour = ASH_WHITE;
    engine.add(p);
}

/// `0xBAB1C2` as `[f32; 3]` — the tint vanilla's own white-ash and
/// white-smoke particles both declare as their own packed-RGB constant and then
/// unpack channel by channel (`186, 177, 194`).
const ASH_WHITE: [f32; 3] = [186.0 / 255.0, 177.0 / 255.0, 194.0 / 255.0];

/// Vanilla's own flame particle — a rising particle that ignores collision.
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
    let jitter = |r: &mut JavaRandom| f64::from((r.next_f32() - r.next_f32()) * 0.05);
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

/// Vanilla's own bubble particle — rises through water and pops the instant it leaves.
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
    let scatter = |r: &mut JavaRandom| f64::from(r.next_f32().mul_add(2.0, -1.0) * 0.02);
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

/// Vanilla's own splash particle — a water-drop particle launched by something entering water.
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
    // Vanilla's own water-drop particle's constructor.
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
    // Vanilla's own splash particle overrides gravity and, for purely horizontal input,
    // replaces the velocity outright so the drop arcs upward.
    p.gravity = 0.04;
    if ya == 0.0 && (xa != 0.0 || za != 0.0) {
        p.xd = xa;
        p.yd = 0.1;
        p.zd = za;
    }
    p.behaviour = Behaviour::WaterDrop;
    let frame = engine.rng().next_i32_bound(i32::from(Sheet::Splash.frame_count()));
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
    engine.rng().next_f32()
}

/// Vanilla's own attack-sweep particle — the arc thrown by a sweeping melee hit.
///
/// From vanilla's own attack-sweep particle constructor (26.2 decompile):
/// no move step call at all (stationary for its whole life — see
/// [`crate::Particle::tick_sweep_attack`]), full-bright, 4-tick lifetime, a
/// grey tint drawn once (`nextFloat() * 0.6F + 0.4F`), and
/// `quadSize = 1.0F - (float) size * 0.5F`.
///
/// `size` is the constructor's own auxiliary-X parameter — but the one real
/// vanilla call site (the player's own attack step, sending the sweep-attack
/// particle with count `0` and max speed `0.0F`) means the client's own
/// particle-event handler's `count == 0` branch computes the auxiliary value
/// as `maxSpeed * xDist`, so the value that actually reaches this
/// constructor in real play is always `0.0`, regardless of `dx` — i.e.
/// `quadSize` is always `1.0` in practice. Taking `size` as a parameter
/// anyway (rather than hardcoding that) keeps this a faithful transcription
/// of the Java constructor for any future caller (a datapack or `/particle`
/// invocation can still pass a nonzero auxiliary value).
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

/// Vanilla's own note particle — the coloured chime above a played note block.
///
/// From vanilla's own note particle constructor (26.2 decompile):
/// zero initial velocity, `friction = 0.66F`,
/// speed-up-when-Y-motion-is-blocked = true, `yd += 0.2`, a fixed `lifetime = 6`
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

/// Vanilla's own heart-particle constructor body (26.2 decompile):
/// zero initial velocity, speed-up-when-Y-motion-is-blocked = true,
/// `friction = 0.86F`, `yd += 0.1`, `quadSize *= 1.5F`, `lifetime = 16`,
/// `hasPhysics = false`. [`heart`] and [`angry_villager`] are its two
/// registered providers — same class, different sprite and vertical offset
/// at the emit site (the `+ 0.5` its own angry-villager provider applies).
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

/// Vanilla's own heart-particle provider — breeding hearts (`minecraft:heart`).
pub fn heart(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let p = heart_particle(engine, x, y, z, Sheet::Heart);
    engine.add(p);
}

/// Vanilla's own heart-particle angry-villager provider — the villager "angry" icon
/// (`minecraft:angry_villager`). Same physics as [`heart`], a different
/// sprite (`particle/angry`, not `particle/heart`), and vanilla raises the
/// spawn point by `0.5` at the call site rather than in the particle class —
/// reproduced here since this function *is* that call site.
pub fn angry_villager(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
    let p = heart_particle(engine, x, y + 0.5, z, Sheet::Angry);
    engine.add(p);
}

/// Vanilla's own suspended-town-decoration particle's happy-villager
/// provider — the villager "happy" icon (`minecraft:happy_villager`).
///
/// From vanilla's own suspended-town-decoration particle constructor (26.2
/// decompile): a jittered-velocity construction (the same shape
/// [`Particle::with_velocity`] already reproduces) followed by a dim grey
/// tint (`nextFloat() * 0.1F + 0.2F`), a `0.02`×`0.02` box, a
/// `nextFloat() * 0.6F + 0.5F` quad-size jitter, the velocity damped to a
/// hundredth, and `lifetime = (int)(20.0 / (nextFloat() * 0.8F + 0.2F))`.
/// Its own happy-villager provider itself then sets the colour to white,
/// which is redundant here since white is this crate's own particle default.
pub fn happy_villager(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let p = suspended_town(engine, x, y, z, xa, ya, za, Sheet::Glint);
    engine.add(p);
}

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
        reason = "mirrors `(int) Math.max(baseLifetime * 2.5F, 1.0F)` exactly"
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

/// One standard-normal draw, for the providers whose velocity vanilla takes
/// from its own random-source's Gaussian accessor.
///
/// A Box–Muller transform over the engine's own stream rather than a second
/// `java.util.Random` reimplementation, for the reason this crate's `JavaRandom`
/// docs already give: nothing observes particle randomness across the wire.
fn gaussian(engine: &mut ParticleEngine) -> f64 {
    let rng = engine.rng();
    let u1 = rng.next_f64().max(1e-12);
    let u2 = rng.next_f64();
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

/// Vanilla's own spell-particle witch provider — the purple motes above a
/// drinking witch (`minecraft:witch`).
///
/// From vanilla's own spell-particle constructor (26.2 decompile):
/// the constructor jitters its *own* horizontal velocity from a
/// process-wide static random source (its own class-level constant) rather
/// than the per-particle stream every other emitter in this crate draws from —
/// drawn from this engine's RNG instead, since particle-burst randomness is
/// disclosed as not needing bit-exact replay (see
/// [`crate::Particles::spawn_particles`]'s module docs in the shell for the
/// same policy applied to the network dispatch). `friction = 0.96F`,
/// `gravity = -0.1F`, `speedUpWhenYMotionIsBlocked = true`, `yd *= 0.2F`, and
/// — using the constructor's *original*, unjittered `xa`/`za` parameters,
/// not the ones just fed into the velocity jitter — a further `xd`/`zd`
/// damp to a tenth when both were exactly zero. `quadSize *= 0.75F`,
/// `lifetime = (int)(8.0 / (nextFloat() * 0.8F + 0.2F))`, `hasPhysics =
/// false`. The witch provider then sets the colour: `nextFloat() * 0.5F +
/// 0.35F` brightness times `(1, 0, 1)` — magenta, never green.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own spell-particle constructor argument for argument"
)]
pub fn witch(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let mut p = spell_particle(engine, x, y, z, xa, ya, za, Sheet::Spell);
    let rb = rng_next(engine).mul_add(0.5, 0.35);
    p.colour = [rb, 0.0, rb];
    engine.add(p);
}

/// Vanilla's own spell particle over an arbitrary sheet and a fixed tint — the shared shape
/// behind `effect`, `entity_effect`, `instant_effect`, `infested`, `raid_omen`
/// and `trial_omen`.
///
/// Vanilla registers six registry types against its own spell particle, over **four
/// different sheets**: `minecraft:effect`/`minecraft:entity_effect` name `effect_7…0`,
/// `minecraft:instant_effect`/`minecraft:witch` name `spell_7…0`, and
/// `minecraft:infested`/`minecraft:raid_omen`/`minecraft:trial_omen` each name
/// a single texture of their own. The provider does not decide the sheet; the
/// type's own `particles/<name>.json` does, which is why this takes one.
///
/// `colour` is the provider's own "set color" call. This entry point is the
/// one for the three types registered against vanilla's own spell-particle
/// provider, which take a bare simple-particle-type and therefore never
/// colour themselves at all:
/// `infested`, `raid_omen` and `trial_omen`, whose sprites are already tinted
/// in the texture. The three payload-carrying types have their own entry
/// points — [`spell_instant`] and [`spell_mob_effect`] — because their tint is
/// a wire field rather than a caller's constant.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own spell-particle constructor argument for argument, plus the sheet \
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

/// Vanilla's own spell-particle instant provider — `effect` and
/// `instant_effect`, whose own particle-option type names both a tint and a velocity multiplier.
///
/// The provider is its own "set color" step followed by
/// its own "set power" step, in that order. "Set power" scales `xd`/`zd` and
/// rescales `yd` about the `0.1` upward bias the base constructor applied, so
/// it must run **after** [`spell_particle`]'s own `yd *= 0.2F` damp rather than
/// being folded into the velocity the caller passes — a power applied to the
/// constructor's arguments instead would multiply a different quantity.
///
/// `effect` names [`Sheet::Effect`] and `instant_effect` [`Sheet::Spell`];
/// the class decides neither, so the sheet is a parameter as it is for
/// [`spell`].
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own spell-particle constructor argument for argument, plus the sheet \
              and the two particle-option fields its provider applies"
)]
pub fn spell_instant(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    sheet: Sheet,
    colour: [f32; 3],
    power: f32,
) {
    let mut p = spell_particle(engine, x, y, z, xa, ya, za, sheet);
    p.colour = colour;
    p.set_power(power);
    engine.add(p);
}

/// Vanilla's own spell-particle mob-effect provider — `entity_effect`, whose
/// own colour particle-option type is a four-component ARGB word.
///
/// Its own "set color" step then its own "set alpha" step. The alpha is the
/// part it is easiest to drop: vanilla's own mob-effect provider is the only
/// spell-particle provider that sets one, and an ambient mob-effect mote is
/// drawn part-transparent by design. Vanilla's own "set alpha" step also
/// records the value as its own original-alpha field and its own per-tick
/// step lerps back towards it — that lerp only ever has something to do when
/// its own "is close to scoping player" check has forced the alpha to zero (a spyglass
/// held in first person), which this crate does not model, so holding the
/// alpha fixed is the same result rather than an approximation.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own spell-particle constructor argument for argument, plus the sheet \
              and the ARGB word its provider applies"
)]
pub fn spell_mob_effect(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
    sheet: Sheet,
    colour: [f32; 4],
) {
    let mut p = spell_particle(engine, x, y, z, xa, ya, za, sheet);
    p.colour = [colour[0], colour[1], colour[2]];
    p.alpha = colour[3];
    engine.add(p);
}

/// Vanilla's own spell-particle constructor itself, shared by its four providers.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors vanilla's own spell-particle constructor argument for argument, plus its sheet"
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
    let jitter_x = 0.5 - rng.next_f64();
    let jitter_z = 0.5 - rng.next_f64();
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

/// Vanilla's own totem particle — the burst when a totem of undying saves its
/// holder (`minecraft:totem_of_undying`).
///
/// From vanilla's own totem particle (26.2 decompile):
/// extends its own simple-animated particle (`friction = 0.91F` overridden
/// immediately back down to `0.6F`, `gravity = 1.25F`), takes its velocity
/// **directly** from the caller with no jitter at all (`xd = xa` etc.),
/// `quadSize *= 0.75F`, `lifetime = 60 + nextInt(12)`, and a 1-in-4 chance of
/// a "golden" tint (`0.6..0.8, 0.6..0.9, 0..0.2`) versus the usual "green"
/// one (`0.1..0.3, 0.4..0.7, 0..0.2`) — both branches draw exactly three
/// `nextFloat()`s, so the RNG stream length does not depend on which
/// branch is taken. No fade colour is set, so only alpha fades
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
    let extra = engine.rng().next_i32_bound(12);
    p.lifetime = 60 + extra;
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Glitter,
        frame: Sheet::Glitter.frame_for_age(0, p.lifetime),
    };
    let golden = engine.rng().next_i32_bound(4) == 0;
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

/// Vanilla's own huge-explosion-seed-particle provider —
/// `minecraft:explosion_emitter`, the particle a server-side explode
/// packet's own explosion-particle field almost always names.
///
/// From vanilla's own huge-explosion-seed particle constructor (26.2
/// decompile): the zero-velocity base constructor, the
/// same shape [`Particle::with_velocity`] already reproduces for every other
/// emitter — then a hardcoded `lifetime = 8` that **overwrites** whatever the
/// base constructor's own lifetime draw produced (matching how [`note`]/
/// [`heart_particle`] overwrite theirs). The particle itself is never drawn
/// (vanilla's own non-rendering-particle base); it exists purely to schedule
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
        // Never sampled — vanilla's own non-rendering-particle base is excluded from `extract`
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

/// Vanilla's own huge-explosion-particle provider — `minecraft:explosion`. Spawned
/// directly by a real vanilla packet only rarely (vanilla's own server-side
/// explosion logic's small/large split can choose it), but far more often as
/// [`explosion_emitter`]'s own six-per-tick follow-up, via
/// [`ParticleEngine::tick`].
///
/// From vanilla's own huge-explosion particle (26.2 decompile):
/// zero-velocity construction, then
/// `lifetime = 6 + random.nextInt(4)` (range `[6, 10)`), a grey tint
/// (`random.nextFloat() * 0.6F + 0.4F`, same value on every channel — one
/// draw, not three), and `quadSize = 2.0F * (1.0F - size * 0.5F)` — `size`
/// being this function's own `size` parameter, vanilla's constructor
/// argument (the seed's `age / lifetime` ratio when called from there, or
/// the network aux-X value when called directly from a packet). No override on
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
    let extra = engine.rng().next_i32_bound(4);
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

/// Vanilla's own water-drop particle — the splash a raindrop makes where it
/// lands, and the base class [`splash`]'s own splash particle extends.
///
/// **This is not the falling rain.** The streaks are vanilla's own
/// weather-effect renderer's textured columns, which live in the renderer and never become particles;
/// this is the pop on impact, which the server sends as `minecraft:rain`.
///
/// The one number that separates it from [`splash`] is `gravity`: `0.06` here
/// against the splash's `0.04`, and the splash additionally replaces the whole
/// velocity when the packet's is purely horizontal. Copying [`splash`] and
/// leaving the gravity alone gives raindrops that hang in the air.
pub fn rain(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
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
    p.xd *= f64::from(0.3_f32);
    p.zd *= f64::from(0.3_f32);
    p.yd = f64::from(rng_next(engine).mul_add(0.2, 0.1));
    p.set_size(0.01, 0.01);
    p.gravity = 0.06;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(8.0_f32) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime;
    p.behaviour = Behaviour::WaterDrop;
    // Vanilla's own water-drop-particle provider draws its frame from the sheet
    // (`sprite.get(random)`), exactly as the splash provider does.
    let frame = engine.rng().next_i32_bound(i32::from(Sheet::Splash.frame_count()));
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

/// Vanilla's own bubble-column-up particle — a soul-sand column's rising bubble.
///
/// The negative `gravity` is what lifts it: the shared tick's
/// `yd -= 0.04 * gravity` becomes an upward term. Flipping the sign and adding
/// a positive `yd` instead would look right for one tick and then sink.
pub fn bubble_column_up(
    engine: &mut ParticleEngine,
    x: f64,
    y: f64,
    z: f64,
    xa: f64,
    ya: f64,
    za: f64,
) {
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
    p.gravity = -0.125;
    p.friction = 0.85;
    p.set_size(0.02, 0.02);
    p.quad_size *= rng_next(engine).mul_add(0.6, 0.2);
    // `xa * 0.2F + (nextFloat() * 2 - 1) * 0.02F`, per axis and in this order.
    p.xd = xa.mul_add(f64::from(0.2_f32), f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.02));
    p.yd = ya.mul_add(f64::from(0.2_f32), f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.02));
    p.zd = za.mul_add(f64::from(0.2_f32), f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.02));
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(40.0_f32) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime;
    p.behaviour = Behaviour::BubbleColumnUp;
    engine.add(p);
}

/// Vanilla's own downward water-current particle — a magma-block column's sinking, spiralling
/// bubble.
///
/// `has_physics = false`, so it passes through geometry and only the water test
/// and the `on_ground` flag can kill it — and `on_ground` can never be set
/// without physics, which is vanilla's own arrangement.
pub fn current_down(engine: &mut ParticleEngine, x: f64, y: f64, z: f64) {
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
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast on a small float"
    )]
    let lifetime = (rng_next(engine) * 60.0) as i32;
    p.lifetime = lifetime + 30;
    p.has_physics = false;
    p.xd = 0.0;
    p.yd = -0.05;
    p.zd = 0.0;
    p.set_size(0.02, 0.02);
    p.quad_size *= rng_next(engine).mul_add(0.6, 0.2);
    p.gravity = 0.002;
    p.behaviour = Behaviour::WaterCurrentDown { angle: 0.0 };
    engine.add(p);
}

/// Vanilla's own snowflake particle — a powder-snow cauldron's and a snow golem's flakes.
///
/// `friction = 1.0` is deliberate and load-bearing: the per-axis damping in
/// [`Behaviour::Snowflake`]'s tick is the *whole* of this particle's drag, so
/// leaving the default `0.98` here would damp it twice.
pub fn snowflake(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
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
    p.gravity = 0.225;
    p.friction = 1.0;
    p.xd = xa + f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.05);
    p.yd = ya + f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.05);
    p.zd = za + f64::from(rng_next(engine).mul_add(2.0, -1.0) * 0.05);
    // `0.1F * (nextFloat() * nextFloat() * 1.0F + 1.0F)` — two draws, and the
    // product is what biases the flakes small.
    let a = rng_next(engine);
    let b = rng_next(engine);
    p.quad_size = 0.1 * a.mul_add(b, 1.0);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java's `(int)` cast truncates; the value is small"
    )]
    let lifetime = (f64::from(16.0_f32) / f64::from(rng_next(engine)).mul_add(0.8, 0.2)) as i32;
    p.lifetime = lifetime + 2;
    // Vanilla's own snowflake-particle provider tints every flake the same pale blue.
    p.colour = [0.923, 0.964, 0.999];
    p.behaviour = Behaviour::Snowflake;
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Generic,
        frame: Sheet::Generic.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

/// Vanilla's own dust-plume particle — the puff a block dropped into a
/// decorated pot throws up, and the one brushing a suspicious block makes.
///
/// Vanilla's own base ash/smoke particle whose colour the subclass **overwrites**: the base
/// constructor's grey draw still happens (so the RNG stream matches) and its
/// result is discarded in favour of `0xBAB1C2` shifted down by a second draw.
/// `ya` is biased `+0.15` before the base sees it, which is the whole of the
/// plume's initial lift.
pub fn dust_plume(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    /// Vanilla's own dust-plume particle's packed-RGB constant.
    const COLOUR_RGB24: u32 = 12_235_202;

    let mut p = base_ash_smoke(
        engine,
        (x, y, z),
        (xa, ya + f64::from(0.15_f32), za),
        Sheet::Generic,
        1.0,
        AshSmokeParams {
            dir: [0.7, 0.6, 0.7],
            colour_random: 0.5,
            max_lifetime: 7,
            gravity: 0.5,
            has_physics: false,
        },
    );
    let shift = rng_next(engine) * 0.2;
    p.colour = [
        f32::from(((COLOUR_RGB24 >> 16) & 0xff) as u8) / 255.0 - shift,
        f32::from(((COLOUR_RGB24 >> 8) & 0xff) as u8) / 255.0 - shift,
        f32::from((COLOUR_RGB24 & 0xff) as u8) / 255.0 - shift,
    ];
    p.behaviour = Behaviour::DustPlume;
    engine.add(p);
}

/// Vanilla's own wake particle — the expanding ring a fishing bobber leaves on the water,
/// which the server sends as `minecraft:fishing`.
///
/// Every one of the base constructor's velocity terms is drawn and then
/// **overwritten** by the packet's, and its `set_size` is likewise superseded
/// by the tick's per-tick resize. Both are kept because the draws are part of
/// the stream and dropping them shifts everything after.
pub fn fishing(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
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
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::Splash,
        frame: Sheet::Splash.frame_for_age(0, p.lifetime),
    };
    p.gravity = 0.0;
    p.xd = xa;
    p.yd = ya;
    p.zd = za;
    p.behaviour = Behaviour::Wake;
    engine.add(p);
}

/// Vanilla's own bubble-pop particle — the five-frame burst a column's bubble makes as it
/// breaks the surface.
pub fn bubble_pop(engine: &mut ParticleEngine, x: f64, y: f64, z: f64, xa: f64, ya: f64, za: f64) {
    let rng = engine.rng();
    let mut p = Particle::new(
        x,
        y,
        z,
        SpriteSource::Sheet {
            sheet: Sheet::BubblePop,
            frame: 0,
        },
        rng,
    );
    p.lifetime = 4;
    p.gravity = 0.008;
    p.xd = xa;
    p.yd = ya;
    p.zd = za;
    p.behaviour = Behaviour::BubblePop;
    p.sprite = SpriteSource::Sheet {
        sheet: Sheet::BubblePop,
        frame: Sheet::BubblePop.frame_for_age(0, p.lifetime),
    };
    engine.add(p);
}

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
    // Java's own `Math.cos`/`Math.sin` on a `double` — the library trig, not
    // vanilla's quantized table, because vanilla itself calls the library here.
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
    item: u32,
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

#[cfg(test)]
mod tests {
    use super::{
        FULL_CUBE, Face, angry_villager, breaking_block_effect, bubble, crit, destroy_block_effect,
        ash, campfire_smoke, drip, explosion_emitter, firework, flame, fly_towards_position,
        happy_villager, heart, huge_explosion, lava, note, poof, smoke, splash,
        spore_blossom_air, sweep_attack, totem_of_undying, white_smoke, witch,
    };
    use crate::{Behaviour, DripKind, DripPhase, ParticleEngine, Sheet, SpriteSource};
    use lodestone_physics::{Aabb, CollisionView};

    const STONE: u32 = 1;
    const WHITE: [f32; 3] = [1.0, 1.0, 1.0];

    /// The counts here come from the **formula in vanilla's own "add destroy block effect" step**
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
        assert!(!p.has_physics, "vanilla's own crit particle sets hasPhysics = false");
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
            "smoke sets its own speed-up-when-Y-motion-is-blocked flag"
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

    /// The sweep-attack particle: exactly the
    /// vanilla shape, not merely "a particle appeared". Lifetime and light
    /// coords are exact constants in the Java source, not RNG-derived, so
    /// they are asserted exactly rather than as a range.
    #[test]
    fn sweep_attack_has_the_exact_vanilla_lifetime_and_colour_range() {
        let mut engine = ParticleEngine::seeded(100);
        sweep_attack(&mut engine, 0.0, 64.0, 0.0, 0.0);
        assert_eq!(engine.len(), 1, "sweep_attack must spawn exactly one quad");
        let p = &engine.particles()[0];
        assert_eq!(p.lifetime, 4, "vanilla's own attack-sweep particle's lifetime = 4, hardcoded");
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

    /// Vanilla's own note particle's colour formula is exact and external
    /// (its own three phase-shifted sines), so the expected
    /// value is computed independently here from the same formula rather than
    /// merely checking "some colour resulted".
    #[test]
    fn note_colour_matches_the_three_phase_shifted_sine_formula() {
        let mut engine = ParticleEngine::seeded(1);
        note(&mut engine, 0.0, 64.0, 0.0, 0.5);
        let p = &engine.particles()[0];
        assert_eq!(p.lifetime, 6, "vanilla's own note particle hardcodes lifetime = 6");
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

    /// Vanilla's own heart particle is physics-free with a fixed 16-tick life — both
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
            assert!(!p.has_physics, "vanilla's own heart particle sets hasPhysics = false");
            assert_eq!(p.lifetime, 16, "vanilla's own heart particle hardcodes lifetime = 16");
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

    /// Vanilla's own suspended-town particle's tick is a `lifetime`-countdown
    /// with no collision, not the usual `age`-increment: this pins that the particle
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

    /// Vanilla's own spell-particle witch provider always tints magenta (`(1, 0, 1)`
    /// scaled by a shared brightness) — green is structurally impossible from this
    /// formula, which is the exact property that distinguishes "witch" from
    /// the green-tinted mob-effect variants of the same particle family (neither
    /// of which this pass builds, since they need a colour-carrying particle
    /// option decode).
    #[test]
    fn witch_particles_are_always_magenta_never_green() {
        let mut engine = ParticleEngine::seeded(4);
        for _ in 0..20 {
            witch(&mut engine, 0.0, 64.0, 0.0, 0.0, 0.0, 0.0);
        }
        for p in engine.particles() {
            assert!(!p.has_physics, "vanilla's own spell particle sets hasPhysics = false");
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

    /// Vanilla's own totem particle's lifetime is `60 + nextInt(12)`, bounded to
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
    /// takes its velocity directly with no jitter (the same vanilla totem-particle
    /// shape [`totem_of_undying_lifetime_is_bounded_and_velocity_is_unjittered`]
    /// pins), and — unlike totem — leaves colour at the base white and sets
    /// `alpha = 0.99` (vanilla's own firework spark provider's own line), never `1.0`.
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
        assert_eq!(p.colour, [1.0, 1.0, 1.0], "vanilla's own spark particle never sets a custom colour");
        assert!((p.alpha - 0.99).abs() < 1e-6, "vanilla's own spark provider sets alpha to 0.99, not 1.0");
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

    /// Vanilla's own huge-explosion-seed particle is a non-rendering particle,
    /// and it hardcodes `lifetime = 8` — overwriting the base constructor's own
    /// RNG-drawn lifetime, exactly the way `note`/`heart_particle` overwrite theirs.
    #[test]
    fn explosion_emitter_is_a_fixed_eight_tick_seed() {
        let mut engine = ParticleEngine::seeded(200);
        explosion_emitter(&mut engine, 0.0, 64.0, 0.0);
        assert_eq!(engine.len(), 1);
        let p = &engine.particles()[0];
        assert_eq!(p.lifetime, 8, "vanilla's own huge-explosion-seed particle hardcodes lifetime = 8");
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

    /// Vanilla's own huge-explosion particle's own exact formulas, computed
    /// independently here from the Java source rather than read off the implementation:
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
             contribute none, even though it is still alive in the engine — the seed is
             vanilla's own non-rendering placeholder particle"
        );
    }

    /// Vanilla's own huge-explosion particle's light-coordinate accessor hardcodes
    /// its own `15728880` full-bright constant, independently of the world light
    /// sampler — mirrors `self_lit_particles_ignore_the_light_sampler_entirely` in
    /// `lib.rs`'s own test module for the simple-animated and sweep-attack particles.
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

    /// A lava pop trails smoke, and the odds of doing so fall to zero over its
    /// life.
    ///
    /// This is the second particle in the crate whose own tick spawns another,
    /// and unlike a drip's hand-off it is **probabilistic** — the roll is
    /// `nextFloat() > age / lifetime`, so a fresh pop trails almost every tick
    /// and an old one almost never does. Dropping the roll entirely leaves a
    /// bare orange dot where vanilla has a smoking ember, and the particle count
    /// is the only thing that shows it.
    ///
    /// Measured across the pop's whole life rather than tick by tick, because a
    /// single tick's roll is a coin flip: the discriminating claim is that the
    /// *early* half of the life produces strictly more smoke than the late half,
    /// which no constant-probability implementation (and certainly no absent
    /// one) satisfies.
    #[test]
    fn a_lava_pop_trails_smoke_and_stops_as_it_ages() {
        struct Empty;
        impl CollisionView for Empty {
            fn collision_boxes(&self, _: i32, _: i32, _: i32, _: &mut Vec<Aabb>) {}
        }
        let smoke_count = |e: &ParticleEngine| {
            e.particles()
                .iter()
                .filter(|p| p.behaviour == Behaviour::AshSmoke)
                .count()
        };

        let mut e = ParticleEngine::seeded(31);
        lava(&mut e, 0.5, 65.0, 0.5);
        let lifetime = e.particles()[0].lifetime;
        assert!(lifetime > 8, "need a long enough pop to split in half: {lifetime}");

        let mut early = 0usize;
        for _ in 0..lifetime / 2 {
            let before = smoke_count(&e);
            e.tick(&Empty);
            early += smoke_count(&e).saturating_sub(before);
        }
        let mut late = 0usize;
        for _ in lifetime / 2..lifetime {
            let before = smoke_count(&e);
            e.tick(&Empty);
            late += smoke_count(&e).saturating_sub(before);
        }
        assert!(
            early > 0,
            "a fresh lava pop must trail smoke at all; got {early} in its first \
             {} ticks",
            lifetime / 2
        );
        assert!(
            early > late,
            "the trail must thin as the pop ages: {early} early vs {late} late over a \
             {lifetime}-tick life"
        );
    }

    /// A hanging drip must let go and become a falling one, and the falling one
    /// must land as a splash.
    ///
    /// This is the property the previous one-shot emitter could not have: it
    /// spawned whichever phase the packet named, with a hardcoded lifetime, and
    /// removed it. A cave ceiling grew drips that hung and blinked out. The
    /// chain lives in vanilla's own drip particle's own tick step, not in any
    /// spawn site, so nothing upstream could have supplied it.
    ///
    /// The three counts asserted here are the discriminating ones: a hang that
    /// merely dies leaves **zero** particles, a hang that spawns a fall leaves
    /// one, and only a fall that reaches the ground leaves a splash.
    #[test]
    fn a_hanging_water_drip_falls_and_the_falling_drip_splashes() {
        // A floor at y = 64 and nothing else, so a released drip has somewhere
        // to land. No fluid anywhere, so the "dies inside its own fluid" arm
        // cannot be what removes it.
        struct Floor;
        impl CollisionView for Floor {
            fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
                if y == 64 {
                    out.push(Aabb::new(
                        f64::from(x),
                        f64::from(y),
                        f64::from(z),
                        f64::from(x) + 1.0,
                        f64::from(y) + 1.0,
                        f64::from(z) + 1.0,
                    ));
                }
            }
        }

        let mut e = ParticleEngine::seeded(21);
        drip(&mut e, DripKind::Water, DripPhase::Hang, [0.5, 70.0, 0.5], [0.0; 3]);
        let hang = &e.particles()[0];
        assert_eq!(
            hang.lifetime, 40,
            "vanilla's own drip-hang particle sets a flat 40, not the `64 / nextFloat` draw the \
             one-shot emitter used"
        );
        assert_eq!(hang.behaviour, Behaviour::Drip { kind: DripKind::Water, phase: DripPhase::Hang });

        // 41 ticks: `lifetime--` is a post-decrement tested against zero, so the
        // drip lives one tick longer than its lifetime says.
        for _ in 0..41 {
            e.tick(&Floor);
        }
        let after: Vec<Behaviour> = e.particles().iter().map(|p| p.behaviour).collect();
        assert_eq!(
            after,
            vec![Behaviour::Drip { kind: DripKind::Water, phase: DripPhase::Fall }],
            "the hanging drip must have been replaced by exactly one falling one"
        );

        // Now let it fall the ~6 blocks to the floor. Its own lifetime is a
        // `64 / nextFloat` draw with a floor of 71 ticks, so the landing is what
        // ends it, not the clock.
        let fall_lifetime = e.particles()[0].lifetime;
        let mut landed = None;
        for tick in 0..fall_lifetime {
            e.tick(&Floor);
            if e.particles().iter().any(|p| p.behaviour == Behaviour::WaterDrop) {
                landed = Some(tick);
                break;
            }
        }
        assert!(
            landed.is_some(),
            "a falling water drip must land as a `splash` (vanilla's own water-drop \
             particle) within its {fall_lifetime}-tick lifetime"
        );
        assert!(
            !e.particles()
                .iter()
                .any(|p| matches!(p.behaviour, Behaviour::Drip { .. })),
            "the falling drip must be gone once it has splashed"
        );
    }

    /// A lava drip cools from white-hot to exactly the lava tint over its 40
    /// hanging ticks.
    ///
    /// Vanilla's own cooling-drip-hang particle is two constants — `g = 16 / (elapsed + 16)`
    /// and `b = 4 / (elapsed + 8)` — and the check that they are transcribed
    /// right is that after 40 ticks they arrive on vanilla's own drip-particle
    /// lava-fall provider's **independently specified** `setColor(1.0F, 0.2857143F, 0.083333336F)`.
    /// That is an outside expectation rather than a restatement: nothing in the
    /// cooling formula mentions the falling phase's colour, and two different
    /// vanilla methods have to agree for this to hold.
    #[test]
    fn a_hanging_lava_drip_cools_onto_the_falling_phases_own_tint() {
        struct Empty;
        impl CollisionView for Empty {
            fn collision_boxes(&self, _: i32, _: i32, _: i32, _: &mut Vec<Aabb>) {}
        }
        let mut e = ParticleEngine::seeded(4);
        drip(&mut e, DripKind::Lava, DripPhase::Hang, [0.5, 70.0, 0.5], [0.0; 3]);
        assert_eq!(
            e.particles()[0].colour,
            [1.0, 1.0, 0.5],
            "a fresh lava drip is white-hot: `16/16` and `4/8`"
        );
        // The colour is recomputed from the **pre**-decrement `lifetime`, so the
        // k-th tick sees `elapsed == k - 1`: the first tick recomputes the same
        // white-hot value it was constructed with, and after 40 ticks the drip
        // is still alive with `lifetime == 0` and `elapsed == 39`. Counting 40
        // ticks as 40 steps of the ramp is off by one in the direction that
        // looks like a wrong constant — this test's first prediction was
        // `16 / 55` after 39 ticks and measured `16 / 54`.
        for _ in 0..40 {
            e.tick(&Empty);
        }
        let cooled = e.particles()[0].colour;
        let want = [1.0, 16.0 / 55.0, 4.0 / 47.0];
        assert!(
            (cooled[1] - want[1]).abs() < 1e-6 && (cooled[2] - want[2]).abs() < 1e-6,
            "cooled to {cooled:?}, want {want:?}"
        );
        // One more tick recomputes the ramp at `elapsed == 40` and *then*
        // removes the drip, handing off to the falling phase — so the arrival
        // value is the last thing the hanging particle ever holds. Asserting the
        // identity rather than reading it off a corpse: the point is that
        // vanilla's own cooling-drip-hang particle's two constants and vanilla's
        // own drip-particle lava-fall provider's own colour setter are transcribed
        // from two different vanilla methods that never mention each other, and
        // they have to meet.
        let arrival = [1.0_f32, 16.0 / 56.0, 4.0 / 48.0];
        let lava_fall = [1.0_f32, 0.285_714_3, 0.083_333_336];
        assert!(
            (arrival[1] - lava_fall[1]).abs() < 1e-6 && (arrival[2] - lava_fall[2]).abs() < 1e-6,
            "the cooling ramp must arrive on vanilla's own lava-fall provider's tint: \
             {arrival:?} vs {lava_fall:?}"
        );
    }

    /// A mob-death puff must be full size on its very first frame.
    ///
    /// This is the whole of the `Animated` vs `AshSmoke` distinction, and the
    /// two hypotheses are computed here rather than asserted as a direction:
    /// vanilla's own explode particle has no size-at-age override, so its size
    /// at age 0 is the constructor's own draw, while vanilla's own base
    /// ash-smoke particle's override multiplies by `clamp(age / lifetime * 32, 0, 1)`
    /// — which at age 0 is
    /// exactly **zero**. Borrowing the wrong behaviour therefore makes every
    /// poof invisible on spawn and swell in over its first thirty-second, and
    /// nothing about the particle count or its sprite would show it.
    #[test]
    fn a_poof_is_full_size_on_its_first_frame_and_a_smoke_puff_is_not() {
        let mut e = ParticleEngine::seeded(3);
        poof(&mut e, 0.0, 64.0, 0.0, 0.0, 0.0, 0.0);
        let p = &e.particles()[0];
        // The measurement comes before any assertion about *which* behaviour is
        // set: reading the behaviour first aborts on a restatement of the
        // implementation and never prints the number the test exists for.
        let constructed = p.quad_size;
        let drawn = p.quad_size(0.0);
        assert!(constructed > 0.0, "the constructor must draw a real size");
        assert!(
            (drawn - constructed).abs() < 1e-9,
            "a poof must draw at its constructed size ({constructed}); got {drawn}, and the \
             ash-smoke fade-in hypothesis predicts exactly 0.0 (behaviour: {:?})",
            p.behaviour
        );

        // The positive control: the class that *does* have the override still
        // gets it, so this test is measuring the split and not the absence of
        // any override at all.
        let mut e = ParticleEngine::seeded(3);
        smoke(&mut e, 0.0, 64.0, 0.0, 0.0, 0.0, 0.0, 1.0);
        let s = &e.particles()[0];
        assert!(
            s.quad_size(0.0).abs() < 1e-9,
            "smoke must fade in from zero, got {} (behaviour: {:?})",
            s.quad_size(0.0),
            s.behaviour
        );
    }

    /// `ash` falls and `white_smoke` rises, and both come out of the same
    /// parameterised base constructor.
    ///
    /// The two differ by the sign of one number in vanilla's positional
    /// argument list (`gravity` `0.1F` against `-0.1F`) plus the sign of a
    /// second (the vertical scatter direction), which is exactly the
    /// transposition-shaped mistake [`AshSmokeParams`] exists to make
    /// unspellable. Asserting the sign of
    /// `gravity` rather than an observed drift keeps this independent of how
    /// many ticks a fixture happens to run.
    #[test]
    fn ash_falls_through_terrain_and_white_smoke_rises_and_collides() {
        let mut e = ParticleEngine::seeded(11);
        ash(&mut e, 0.0, 64.0, 0.0);
        white_smoke(&mut e, 0.0, 64.0, 0.0, 0.0, 0.0, 0.0);
        let (a, w) = (&e.particles()[0], &e.particles()[1]);
        assert!(a.gravity > 0.0 && !a.has_physics, "ash: {} {}", a.gravity, a.has_physics);
        assert!(w.gravity < 0.0 && w.has_physics, "white smoke: {} {}", w.gravity, w.has_physics);
        // Vanilla's own "color random" field is 0.5 for ash and the fixed
        // `0xBAB1C2` for white smoke, so the two are never the same grey by
        // accident.
        assert_eq!(w.colour, [186.0 / 255.0, 177.0 / 255.0, 194.0 / 255.0]);
        assert!(a.colour[0] < 0.5, "ash tint is `nextFloat() * 0.5`: {:?}", a.colour);
        assert_eq!(
            a.sprite,
            SpriteSource::Sheet { sheet: Sheet::Generic0, frame: 0 },
            "`ash.json` names one texture, so ash must not animate"
        );
    }

    /// `spore_blossom_air` hangs for hundreds of ticks; it is not a drip.
    ///
    /// It shares `drip_fall`'s texture with `falling_spore_blossom` and was
    /// wired as vanilla's own drip particle on the strength of that. The discriminating
    /// measurement is the lifetime: vanilla's own suspended-particle spore-blossom-air
    /// provider draws a flat `500..=1000`, while the drip particle's is
    /// `(int)(64 / (nextFloat() * 0.8 + 0.2))`, whose **maximum** is 320 — so
    /// the two ranges do not overlap at all and a single sample separates them.
    #[test]
    fn spore_blossom_air_outlives_every_possible_drip() {
        const DRIP_LIFETIME_CEILING: i32 = 320; // 64 / 0.2
        let mut e = ParticleEngine::seeded(7);
        for _ in 0..16 {
            spore_blossom_air(&mut e, 0.0, 64.0, 0.0);
        }
        let mut out_of_range: Vec<i32> = Vec::new();
        for p in e.particles() {
            if !(500..=1000).contains(&p.lifetime) {
                out_of_range.push(p.lifetime);
            }
        }
        assert!(
            out_of_range.is_empty(),
            "lifetimes outside 500..=1000 (a drip tops out at {DRIP_LIFETIME_CEILING}): \
             {out_of_range:?}"
        );
        let p = &e.particles()[0];
        assert!(p.gravity > 0.0 && !p.has_physics, "it drifts down without colliding");
        assert_eq!(p.colour, [0.32, 0.5, 0.22]);
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

    #[test]
    fn campfire_providers_pick_across_the_sprite_set_and_apply_their_alpha() {
        let mut engine = ParticleEngine::seeded(4096);
        for signal in [false, true] {
            for _ in 0..12 {
                campfire_smoke(&mut engine, 0.5, 64.5, 0.5, 0.0, 0.07, 0.0, signal);
            }
        }

        let cosy = &engine.particles()[..12];
        let signal = &engine.particles()[12..];
        assert!(cosy.iter().all(|particle| (particle.alpha - 0.9).abs() < 1e-6));
        assert!(signal.iter().all(|particle| (particle.alpha - 0.95).abs() < 1e-6));
        let frames = engine
            .particles()
            .iter()
            .map(|particle| match particle.sprite {
                SpriteSource::Sheet {
                    sheet: Sheet::BigSmoke,
                    frame,
                } => frame,
                other => panic!("campfire smoke used the wrong sprite: {other:?}"),
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            frames.len() >= 6,
            "SpriteSet.get(random) should vary the plume, got frames {frames:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Ambient and environmental types
// ---------------------------------------------------------------------------
//
// Vanilla's own rising-particle class is the shared base for `flame`,
// `soul_fire_flame` and `soul`, and its whole constructor is the four lines
// [`rising`] transcribes. `flame` above predates it and keeps its own copy;
// everything below goes through this one.

/// Vanilla's own rising-particle constructor: `friction = 0.96`, the requested velocity with
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
    // `(int)(8.0 / (random.nextDouble() * 0.8 + 0.2))`, then
    // `(int) Math.max(baseLifetime * scale, 1.0F)`.
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
