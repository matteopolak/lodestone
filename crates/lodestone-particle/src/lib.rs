//! Vanilla's particle simulation, version-free and render-free.
//!
//! This crate is the simulation half of Minecraft's particle system: it owns the
//! particles, ticks them with vanilla's exact physics, and extracts camera-facing
//! quads. It deliberately knows nothing about wgpu, atlases or texture files —
//! [`extract`](ParticleEngine::extract) emits positions, sizes, sprite-local UVs
//! and colours, and the shell maps those onto whatever atlas it has built.
//!
//! # Why the split
//!
//! Every other visual subsystem in this project that was built as a single
//! render-coupled unit ended up an *island*: complete, unit-tested, and consumed
//! by nothing. Keeping the simulation independently runnable means the parity
//! tests here exercise the same code the game does, and a headless bot can
//! observe particles (for example, to see another player's block breaking) with
//! no GPU at all.
//!
//! # Float widths are load-bearing
//!
//! Vanilla stores positions and velocities as `double` and gravity, friction,
//! colours and quad sizes as `float`, then mixes them freely — `xd * friction`
//! is a `double * float` that Java promotes. Writing `xd *= 0.98` instead of
//! `xd *= f64::from(0.98_f32)` looks identical and is not: `0.98_f32` widens to
//! `0.980000019073486`, and after a few hundred ticks the trajectories visibly
//! part. Every such promotion is spelled out explicitly below, with the Java
//! expression it came from in the comment.
//!
//! Particle randomness is **not** parity-critical — no server ever sees it — but
//! it is reproduced exactly anyway so tests can assert concrete values. See
//! [`rng`].
//!
//! # Scope
//!
//! The base class ([`Particle`], [`SingleQuadParticle`]) is complete. The
//! per-type behaviours in [`Behaviour`] cover the ones the client needs to make
//! block interaction and the water surface read correctly; adding another is a
//! transcription of one small Java class plus a test, not a design exercise.

pub mod emit;
pub mod rng;

use lodestone_physics::collision::collide;
use lodestone_physics::{Aabb, CollisionView, Vec3d};
use rng::JavaRandom;

/// `Mth.square(100.0)` — above this speed vanilla skips collision entirely,
/// because sweeping a very fast particle would gather an enormous block region
/// for something that lives a fraction of a second.
const MAXIMUM_COLLISION_VELOCITY_SQUARED: f64 = 100.0 * 100.0;

/// Vanilla's fully-lit light coords (`15728880`), used by particles that ignore
/// world lighting.
pub const FULL_BRIGHT: u32 = 15_728_880;

/// The light coords vanilla falls back to in an unloaded chunk (`15728640`).
pub const UNLOADED_LIGHT: u32 = 15_728_640;

/// Which vanilla particle texture sheet a particle draws from.
///
/// The crate names the sheet; the shell resolves it to atlas coordinates. Sheet
/// names are stable across versions, which is why naming them here does not make
/// this crate version-aware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Sheet {
    /// `particle/generic_0` … `generic_7` — smoke, explosions, most puffs.
    Generic,
    /// `particle/critical_hit` — the crit and magic-crit sparkle.
    CriticalHit,
    /// `particle/enchanted_hit`.
    EnchantedHit,
    /// `particle/flame`.
    Flame,
    /// `particle/splash_0` … `splash_3`.
    Splash,
    /// `particle/bubble`.
    Bubble,
    /// `particle/note`.
    Note,
    /// `particle/heart`.
    Heart,
    /// `particle/effect_0` … `effect_7` — potion and spell effects.
    Effect,
    /// `particle/glitter_0` … `glitter_7` — `TotemParticle`/`EndRodParticle`.
    Glitter,
    /// `particle/sweep_0` … `sweep_7` — `AttackSweepParticle`.
    SweepAttack,
    /// `particle/spell_0` … `spell_7` — `SpellParticle` (witch, instant/mob
    /// effect). A separate physical sheet from `Effect`: `effect.json` and
    /// `witch.json` name different textures (`effect_N` vs `spell_N`) even
    /// though both classes are `SpellParticle`-family.
    Spell,
    /// `particle/angry` — `HeartParticle.AngryVillagerProvider`.
    Angry,
    /// `particle/glint` — `SuspendedTownParticle.HappyVillagerProvider`.
    Glint,
    /// `particle/explosion_0` … `explosion_15` — `HugeExplosionParticle`
    /// (`ParticleTypes.EXPLOSION`). Sixteen frames, confirmed against
    /// `assets/minecraft/particles/explosion.json`'s own texture list rather
    /// than assumed from the registry name — the doc's own warning about not
    /// assuming a sheet stem matches the registry name holds here too, it
    /// just happens both to be "explosion" *and* to need its own frame count
    /// (16, not the 8 every other multi-frame sheet in this enum uses).
    Explosion,
}

impl Sheet {
    /// How many numbered frames the sheet has. A sheet with one frame has no
    /// numeric suffix in its file name — see [`Self::texture_name`].
    #[must_use]
    pub const fn frame_count(self) -> u16 {
        match self {
            Self::Explosion => 16,
            Self::Generic | Self::Effect | Self::Glitter | Self::SweepAttack | Self::Spell => 8,
            Self::Splash => 4,
            Self::CriticalHit
            | Self::EnchantedHit
            | Self::Flame
            | Self::Bubble
            | Self::Note
            | Self::Heart
            | Self::Angry
            | Self::Glint => 1,
        }
    }

    /// The file stem under `assets/minecraft/textures/particle/`.
    #[must_use]
    pub const fn stem(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::CriticalHit => "critical_hit",
            Self::EnchantedHit => "enchanted_hit",
            Self::Flame => "flame",
            Self::Splash => "splash",
            Self::Bubble => "bubble",
            Self::Note => "note",
            Self::Heart => "heart",
            Self::Effect => "effect",
            Self::Glitter => "glitter",
            Self::SweepAttack => "sweep",
            Self::Spell => "spell",
            Self::Angry => "angry",
            Self::Glint => "glint",
            Self::Explosion => "explosion",
        }
    }

    /// Resource path of one frame, e.g. `particle/generic_3`, or `particle/flame`
    /// for a single-frame sheet.
    ///
    /// `frame` is clamped into range rather than panicking: a sprite lookup is
    /// not worth aborting a frame over.
    #[must_use]
    pub fn texture_name(self, frame: u16) -> String {
        if self.frame_count() == 1 {
            format!("particle/{}", self.stem())
        } else {
            let frame = frame.min(self.frame_count() - 1);
            format!("particle/{}_{frame}", self.stem())
        }
    }

    /// Every frame of the sheet, in order. Convenience for atlas construction.
    #[must_use]
    pub fn texture_names(self) -> Vec<String> {
        (0..self.frame_count())
            .map(|f| self.texture_name(f))
            .collect()
    }

    /// Every sheet this crate can emit, so a caller can build a complete atlas
    /// without enumerating the variants itself (the enum is `non_exhaustive`).
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::Generic,
            Self::CriticalHit,
            Self::EnchantedHit,
            Self::Flame,
            Self::Splash,
            Self::Bubble,
            Self::Note,
            Self::Heart,
            Self::Effect,
            Self::Glitter,
            Self::SweepAttack,
            Self::Spell,
            Self::Angry,
            Self::Glint,
            Self::Explosion,
        ]
    }

    /// `SpriteSet.get(age, lifetime)` — the frame for a particle at `age` of
    /// `lifetime`.
    ///
    /// Vanilla's `MutableSpriteSet.get` is `sprites[age * count / lifetime]`
    /// clamped into range, and clamps a dead particle (`age >= lifetime`) to the
    /// last frame rather than wrapping to the first.
    #[must_use]
    pub fn frame_for_age(self, age: i32, lifetime: i32) -> u16 {
        let count = i32::from(self.frame_count());
        if lifetime <= 0 || count <= 1 {
            return 0;
        }
        let index = age.saturating_mul(count) / lifetime;
        #[expect(
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation,
            reason = "clamped into 0..count first, and count fits u16"
        )]
        {
            index.clamp(0, count - 1) as u16
        }
    }
}

/// Where a particle's texture comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpriteSource {
    /// A frame of a named particle sheet.
    Sheet {
        /// Which sheet.
        sheet: Sheet,
        /// Frame index within it.
        frame: u16,
    },
    /// The particle texture of a block state — `TerrainParticle` takes the
    /// block's model particle sprite, which is why a broken oak log throws
    /// bark-coloured fragments rather than generic grey ones. The shell resolves
    /// the state to a sprite through the block model set.
    BlockState(u32),
}

/// Which pass a particle draws in.
///
/// Vanilla's `SingleQuadParticle.Layer`. `Opaque` particles still have an alpha
/// channel and are alpha-tested; `Translucent` ones are blended and must be
/// drawn after the world's translucent geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// Alpha-tested, depth-writing.
    Opaque,
    /// Alpha-blended.
    Translucent,
}

/// A per-type behaviour override.
///
/// Vanilla expresses these as subclasses overriding `tick`, `getQuadSize`,
/// `move` and `getLightCoords`. An enum keeps particles in one flat `Vec` with
/// no per-particle allocation or vtable, which matters when a single explosion
/// spawns hundreds.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Behaviour {
    /// `SingleQuadParticle` with no override.
    Plain,
    /// `TerrainParticle` — a fragment of a block, textured from a random quarter
    /// of the block's particle sprite. `uo`/`vo` are the quarter offsets, each
    /// drawn from `random.nextFloat() * 3.0`.
    Terrain {
        /// Horizontal sub-sprite offset in `[0, 3)`.
        uo: f32,
        /// Vertical sub-sprite offset in `[0, 3)`.
        vo: f32,
    },
    /// `BaseAshSmokeParticle` — smoke, large smoke, campfire smoke, ash.
    AshSmoke,
    /// `CritParticle` — desaturates towards red as it ages.
    Crit,
    /// `FlameParticle` (a `RisingParticle`): ignores collision entirely and
    /// shrinks quadratically.
    Flame,
    /// `WaterDropParticle` / `SplashParticle` — custom tick that dies on contact
    /// with a surface.
    WaterDrop,
    /// `BubbleParticle` — rises, and dies the moment it leaves water.
    Bubble,
    /// `SimpleAnimatedParticle` — full-bright, fades out over the back half of
    /// its life, optionally towards a second colour.
    SimpleAnimated {
        /// `setFadeColor` target, if any.
        fade: Option<[f32; 3]>,
    },
    /// `AttackSweepParticle` — a full `tick()` override with no `move()` call
    /// at all: it never collides, never falls, and just counts down its
    /// 4-tick lifetime advancing through its sheet. See
    /// [`Particle::tick_sweep_attack`].
    SweepAttack,
    /// `NoteParticle` — a note-block chime. Ordinary physics; only the colour
    /// formula and the fast-fade-in quad size are special.
    Note,
    /// `HeartParticle` — breeding hearts and the villager "angry" icon (same
    /// Java class, different sprite and vertical offset at the emit site).
    /// Physics-free, like [`Self::Crit`].
    Heart,
    /// `SuspendedTownParticle` — the villager "happy" icon (and the wider
    /// family of ambient specks this class covers in vanilla). A full
    /// `tick()` override: no gravity or friction, a `lifetime`-countdown
    /// rather than an `age`-increment, and a `move()` that skips collision
    /// entirely. See [`Particle::tick_suspended`].
    Suspended,
    /// `SpellParticle` — witch/potion effect motes. Translucent, animates
    /// through its sheet every tick with no fade.
    Spell,
    /// `HugeExplosionSeedParticle` (`ParticleTypes.EXPLOSION_EMITTER`) — a
    /// `NoRenderParticle`: never drawn, and its `tick()` is a full override
    /// that calls neither `super.tick()` nor `move()`. Instead it spawns six
    /// [`Self::HugeExplosion`] particles per tick for its 8-tick life, at a
    /// jittered offset with a `size` that grows from `0` to `7/8`. See
    /// [`Particle::tick_huge_explosion_seed`]. Excluded from
    /// [`ParticleEngine::extract`] explicitly, since `layer()` has no "not
    /// drawn at all" value to return.
    HugeExplosionSeed,
    /// `HugeExplosionParticle` (`ParticleTypes.EXPLOSION`) — the visible
    /// shockwave puff a seed spawns. Ordinary physics (no override on
    /// `move`/gravity/friction — vanilla's constructor never touches them),
    /// full-bright (`getLightCoords` hardcodes `15728880`, `FULL_BRIGHT`),
    /// opaque, and animates through [`Sheet::Explosion`] every tick via the
    /// same `setSpriteFromAge` call [`Self::AshSmoke`]/[`Self::Spell`] use.
    HugeExplosion,
}

impl Behaviour {
    /// The sheet a behaviour animates through, if it animates.
    const fn animated_sheet(self, sprite: SpriteSource) -> Option<Sheet> {
        match (self, sprite) {
            (
                Self::AshSmoke
                | Self::SimpleAnimated { .. }
                | Self::SweepAttack
                | Self::Spell
                | Self::HugeExplosion,
                SpriteSource::Sheet { sheet, .. },
            ) => Some(sheet),
            _ => None,
        }
    }

    /// `getLayer()`.
    ///
    /// [`Self::HugeExplosionSeed`] is never actually asked this — it is
    /// excluded from [`ParticleEngine::extract`] before `layer()` would be
    /// consulted, since a `NoRenderParticle` has no vanilla `Layer` at all —
    /// but the match must still be exhaustive, so it takes the harmless
    /// `Opaque` bucket rather than a wildcard arm that could silently swallow
    /// a real future variant.
    #[must_use]
    pub const fn layer(self) -> Layer {
        match self {
            Self::SimpleAnimated { .. } | Self::Spell => Layer::Translucent,
            Self::Plain
            | Self::Terrain { .. }
            | Self::AshSmoke
            | Self::Crit
            | Self::Flame
            | Self::WaterDrop
            | Self::Bubble
            | Self::SweepAttack
            | Self::Note
            | Self::Heart
            | Self::Suspended
            | Self::HugeExplosionSeed
            | Self::HugeExplosion => Layer::Opaque,
        }
    }
}

/// One live particle.
///
/// Field names and units follow the decompiled source (`xo`/`yo`/`zo` are the
/// previous tick's position, used for render interpolation; `xd`/`yd`/`zd` are
/// velocity per tick), so the transcription can be checked line by line against
/// `Particle.java`.
#[derive(Debug, Clone, PartialEq)]
pub struct Particle {
    /// Previous-tick position, for interpolation at extract time.
    pub xo: f64,
    /// Previous-tick position.
    pub yo: f64,
    /// Previous-tick position.
    pub zo: f64,
    /// Current position. `y` is the **bottom** of the box, not the centre.
    pub x: f64,
    /// Current position.
    pub y: f64,
    /// Current position.
    pub z: f64,
    /// Velocity per tick.
    pub xd: f64,
    /// Velocity per tick.
    pub yd: f64,
    /// Velocity per tick.
    pub zd: f64,
    bb: Aabb,
    /// Set when the last vertical move was blocked from below.
    pub on_ground: bool,
    /// Whether the particle collides with blocks at all.
    pub has_physics: bool,
    stopped_by_collision: bool,
    /// Set once the particle should be dropped at the next sweep.
    pub removed: bool,
    bb_width: f32,
    bb_height: f32,
    /// Ticks lived.
    pub age: i32,
    /// Ticks to live.
    pub lifetime: i32,
    /// Multiplier on the `0.04` per-tick downward acceleration. Note this is
    /// *not* the entity gravity constant — a particle with `gravity = 1.0` falls
    /// at half an entity's rate.
    pub gravity: f32,
    /// Per-tick velocity damping. `0.98` by default.
    pub friction: f32,
    /// `speedUpWhenYMotionIsBlocked` — smoke spreads sideways under a ceiling.
    pub speed_up_when_y_blocked: bool,
    /// Half-extent of the drawn quad, in blocks.
    pub quad_size: f32,
    /// Tint, multiplied with the texture.
    pub colour: [f32; 3],
    /// Alpha.
    pub alpha: f32,
    /// Roll about the view axis, and its previous-tick value.
    pub roll: f32,
    /// Previous-tick roll.
    pub o_roll: f32,
    /// Texture.
    pub sprite: SpriteSource,
    /// Per-type overrides.
    pub behaviour: Behaviour,
}

impl Particle {
    /// `Particle(level, x, y, z)` — the zero-velocity base constructor.
    ///
    /// Draws exactly one random number (the lifetime), which matters when
    /// replaying a seeded burst.
    #[must_use]
    pub fn new(x: f64, y: f64, z: f64, sprite: SpriteSource, rng: &mut JavaRandom) -> Self {
        let mut p = Self::base(x, y, z, sprite, rng);
        p.draw_quad_size(rng);
        p
    }

    /// The `Particle(level, x, y, z)` body alone, without
    /// `SingleQuadParticle`'s `quadSize` draw.
    ///
    /// Kept separate because **draw order is part of the reproduction**. Java
    /// runs `super(...)` before the subclass body, so a particle constructed
    /// with a velocity draws lifetime, then five velocity numbers, and only
    /// *then* its quad size. Folding the quad-size draw into the base
    /// constructor would reorder the stream and silently desynchronise a
    /// seeded replay.
    fn base(x: f64, y: f64, z: f64, sprite: SpriteSource, rng: &mut JavaRandom) -> Self {
        let mut p = Self {
            xo: x,
            yo: y,
            zo: z,
            x,
            y,
            z,
            xd: 0.0,
            yd: 0.0,
            zd: 0.0,
            bb: Aabb::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
            on_ground: false,
            has_physics: true,
            stopped_by_collision: false,
            removed: false,
            bb_width: 0.6,
            bb_height: 1.8,
            age: 0,
            lifetime: 0,
            gravity: 0.0,
            friction: 0.98,
            speed_up_when_y_blocked: false,
            quad_size: 0.0,
            colour: [1.0, 1.0, 1.0],
            alpha: 1.0,
            roll: 0.0,
            o_roll: 0.0,
            sprite,
            behaviour: Behaviour::Plain,
        };
        p.set_size(0.2, 0.2);
        p.set_pos(x, y, z);
        // `(int)(4.0F / (nextFloat() * 0.9F + 0.1F))` — float arithmetic, then
        // truncation towards zero.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Java's `(int)` cast on a float truncates; reproduced deliberately"
        )]
        {
            p.lifetime = (4.0_f32 / rng.next_float().mul_add(0.9, 0.1)) as i32;
        }
        p
    }

    /// `SingleQuadParticle`'s `quadSize` initialiser.
    fn draw_quad_size(&mut self, rng: &mut JavaRandom) {
        self.quad_size = 0.1 * rng.next_float().mul_add(0.5, 0.5) * 2.0;
    }

    /// `Particle(level, x, y, z, xa, ya, za)` — the constructor that scatters an
    /// initial velocity.
    ///
    /// The incoming `xa`/`ya`/`za` are *not* used directly: they are jittered,
    /// normalised, rescaled to a random speed and then biased upwards by `0.1`.
    /// This is why a block-break burst puffs outward and up rather than firing
    /// along the direction given.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors `Particle(level, x, y, z, xa, ya, za)` plus sprite and rng; \
                  grouping the coordinates into a vector type would obscure the \
                  line-by-line correspondence with the Java constructor"
    )]
    #[must_use]
    pub fn with_velocity(
        x: f64,
        y: f64,
        z: f64,
        xa: f64,
        ya: f64,
        za: f64,
        sprite: SpriteSource,
        rng: &mut JavaRandom,
    ) -> Self {
        let mut p = Self::base(x, y, z, sprite, rng);
        // `xa + (nextFloat() * 2.0F - 1.0F) * 0.4F` — the jitter is computed in
        // float and then widened, so it is quantised to float precision.
        p.xd = xa + f64::from(rng.next_float().mul_add(2.0, -1.0) * 0.4);
        p.yd = ya + f64::from(rng.next_float().mul_add(2.0, -1.0) * 0.4);
        p.zd = za + f64::from(rng.next_float().mul_add(2.0, -1.0) * 0.4);
        // `(nextFloat() + nextFloat() + 1.0F) * 0.15F`, in float, then widened.
        let speed = f64::from((rng.next_float() + rng.next_float() + 1.0) * 0.15);
        let dd = p.xd.mul_add(p.xd, p.yd.mul_add(p.yd, p.zd * p.zd)).sqrt();
        let scale = f64::from(0.4_f32);
        p.xd = p.xd / dd * speed * scale;
        p.yd = (p.yd / dd).mul_add(speed * scale, 0.1);
        p.zd = p.zd / dd * speed * scale;
        p.draw_quad_size(rng);
        p
    }

    /// `setPower(float)` — scales the velocity while preserving the `0.1` upward
    /// bias applied by [`Self::with_velocity`].
    pub fn set_power(&mut self, power: f32) {
        let power = f64::from(power);
        self.xd *= power;
        self.yd = (self.yd - 0.1).mul_add(power, 0.1);
        self.zd *= power;
    }

    /// `scale(float)` — grows both the collision box and the drawn quad.
    pub fn scale(&mut self, scale: f32) {
        self.quad_size *= scale;
        self.set_size(0.2 * scale, 0.2 * scale);
    }

    /// `setSize` — resizes the box about its horizontal centre, keeping the
    /// bottom face fixed.
    pub fn set_size(&mut self, w: f32, h: f32) {
        if (w - self.bb_width).abs() > f32::EPSILON || (h - self.bb_height).abs() > f32::EPSILON {
            self.bb_width = w;
            self.bb_height = h;
            let bb = self.bb;
            let w = f64::from(w);
            let new_min_x = (bb.min_x + bb.max_x - w) / 2.0;
            let new_min_z = (bb.min_z + bb.max_z - w) / 2.0;
            self.bb = Aabb::new(
                new_min_x,
                bb.min_y,
                new_min_z,
                new_min_x + w,
                bb.min_y + f64::from(self.bb_height),
                new_min_z + w,
            );
        }
    }

    /// `setPos` — moves the particle and rebuilds the box around it.
    pub fn set_pos(&mut self, x: f64, y: f64, z: f64) {
        self.x = x;
        self.y = y;
        self.z = z;
        let w = f64::from(self.bb_width / 2.0);
        let h = f64::from(self.bb_height);
        self.bb = Aabb::new(x - w, y, z - w, x + w, y + h, z + w);
    }

    /// The collision box.
    #[must_use]
    pub const fn bounding_box(&self) -> Aabb {
        self.bb
    }

    /// `isAlive()`.
    #[must_use]
    pub const fn is_alive(&self) -> bool {
        !self.removed
    }

    /// `remove()`.
    pub const fn remove(&mut self) {
        self.removed = true;
    }

    /// `getQuadSize(partialTick)` — the drawn half-extent, which several
    /// behaviours animate.
    #[must_use]
    pub fn quad_size(&self, partial_tick: f32) -> f32 {
        let normalised = || {
            if self.lifetime <= 0 {
                1.0
            } else {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "tick counts are small; this mirrors Java's int-to-float promotion"
                )]
                {
                    (self.age as f32 + partial_tick) / self.lifetime as f32
                }
            }
        };
        match self.behaviour {
            // `quadSize * clamp((age + a) / lifetime * 32, 0, 1)` — a fast fade
            // *in* over the first 1/32 of life, not a fade out. `NoteParticle`
            // and `HeartParticle` both override `getQuadSize` with this exact
            // expression too.
            Behaviour::Crit | Behaviour::AshSmoke | Behaviour::Note | Behaviour::Heart => {
                self.quad_size * (normalised() * 32.0).clamp(0.0, 1.0)
            }
            // `quadSize * (1 - s * s * 0.5)`.
            Behaviour::Flame => {
                let s = normalised();
                self.quad_size * s.mul_add(-s * 0.5, 1.0)
            }
            _ => self.quad_size,
        }
    }

    /// Sprite-local UVs as `(u0, u1, v0, v1)`, each in `[0, 1]` within the
    /// particle's own sprite.
    ///
    /// [`Behaviour::Terrain`] takes a random *quarter* of the block's sprite so
    /// that fragments of the same block do not all look identical, and returns
    /// `u0 > u1` — vanilla's `getU0` is `(uo + 1) / 4` while `getU1` is `uo / 4`,
    /// which mirrors the fragment horizontally. That inversion is intentional;
    /// "fixing" it makes terrain particles subtly disagree with vanilla.
    #[must_use]
    pub fn uv_local(&self) -> [f32; 4] {
        match self.behaviour {
            Behaviour::Terrain { uo, vo } => [
                (uo + 1.0) / 4.0,
                uo / 4.0,
                vo / 4.0,
                (vo + 1.0) / 4.0,
            ],
            _ => [0.0, 1.0, 0.0, 1.0],
        }
    }

    /// `tick()`.
    ///
    /// `view` supplies block geometry for collision; a particle with
    /// `has_physics == false` never touches it.
    ///
    /// Returns any `(x, y, z, size)` follow-up spawns this tick produced —
    /// empty for every behaviour except [`Behaviour::HugeExplosionSeed`],
    /// which is the one particle in this crate whose own `tick()` creates
    /// more particles. Returning them rather than spawning directly is what
    /// lets [`ParticleEngine::tick`] do it: a `for p in &mut self.particles`
    /// loop already holds `self.particles` mutably borrowed, so a particle
    /// cannot push a sibling into that same `Vec` from inside its own `tick`.
    pub fn tick(&mut self, view: &dyn CollisionView) -> Vec<(f64, f64, f64, f32)> {
        match self.behaviour {
            Behaviour::WaterDrop => {
                self.tick_water_drop(view);
                Vec::new()
            }
            Behaviour::Bubble => {
                self.tick_bubble(view);
                Vec::new()
            }
            Behaviour::SweepAttack => {
                self.tick_sweep_attack();
                Vec::new()
            }
            Behaviour::Suspended => {
                self.tick_suspended();
                Vec::new()
            }
            Behaviour::HugeExplosionSeed => self.tick_huge_explosion_seed(),
            _ => {
                self.tick_base(view);
                self.tick_overrides();
                Vec::new()
            }
        }
    }

    /// The `Particle.tick()` body, shared by everything that calls `super.tick()`.
    fn tick_base(&mut self, view: &dyn CollisionView) {
        self.xo = self.x;
        self.yo = self.y;
        self.zo = self.z;
        self.age += 1;
        if self.age > self.lifetime {
            self.remove();
            return;
        }
        // `yd -= 0.04 * gravity` — `double * float`, so the float widens.
        self.yd -= 0.04 * f64::from(self.gravity);
        self.move_by(self.xd, self.yd, self.zd, view);
        if self.speed_up_when_y_blocked && (self.y - self.yo).abs() < f64::EPSILON {
            self.xd *= 1.1;
            self.zd *= 1.1;
        }
        let friction = f64::from(self.friction);
        self.xd *= friction;
        self.yd *= friction;
        self.zd *= friction;
        if self.on_ground {
            // `0.7F` widened, i.e. 0.699999988079071 — not 0.7.
            let ground_drag = f64::from(0.7_f32);
            self.xd *= ground_drag;
            self.zd *= ground_drag;
        }
    }

    /// The per-subclass work that runs *after* `super.tick()`.
    fn tick_overrides(&mut self) {
        match self.behaviour {
            Behaviour::Crit => {
                // Green and blue decay faster than red, so a crit sparkle warms
                // towards orange as it ages.
                self.colour[1] *= 0.96;
                self.colour[2] *= 0.9;
            }
            Behaviour::AshSmoke | Behaviour::Spell | Behaviour::HugeExplosion => {
                self.set_sprite_from_age();
            }
            Behaviour::SimpleAnimated { fade } => {
                self.set_sprite_from_age();
                let half = self.lifetime / 2;
                if self.age > half {
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "mirrors Java's int-to-float promotion in the same expression"
                    )]
                    {
                        self.alpha = 1.0 - (self.age - half) as f32 / self.lifetime as f32;
                    }
                    if let Some(fade) = fade {
                        for (c, target) in self.colour.iter_mut().zip(fade) {
                            *c += (target - *c) * 0.2;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// `setSpriteFromAge(SpriteSet)`.
    fn set_sprite_from_age(&mut self) {
        if self.removed {
            return;
        }
        if let Some(sheet) = self.behaviour.animated_sheet(self.sprite) {
            self.sprite = SpriteSource::Sheet {
                sheet,
                frame: sheet.frame_for_age(self.age, self.lifetime),
            };
        }
    }

    /// `WaterDropParticle.tick()` — a full override, not a `super` call.
    ///
    /// Two things differ from the base tick and both are visible: it decrements
    /// `lifetime` instead of incrementing `age` (so `getQuadSize`'s age ratio
    /// never applies), and it removes itself when it lands on or enters a
    /// surface, which is what stops rain drips accumulating on the floor.
    fn tick_water_drop(&mut self, view: &dyn CollisionView) {
        self.xo = self.x;
        self.yo = self.y;
        self.zo = self.z;
        self.lifetime -= 1;
        if self.lifetime < 0 {
            self.remove();
            return;
        }
        self.yd -= f64::from(self.gravity);
        self.move_by(self.xd, self.yd, self.zd, view);
        let drag = f64::from(0.98_f32);
        self.xd *= drag;
        self.yd *= drag;
        self.zd *= drag;
        if self.on_ground {
            // Half of the drops that land vanish immediately; the rest skid.
            if self.rng_probe() < 0.5 {
                self.remove();
            }
            let ground_drag = f64::from(0.7_f32);
            self.xd *= ground_drag;
            self.zd *= ground_drag;
        }
        let (bx, by, bz) = block_containing(self.x, self.y, self.z);
        let surface = view
            .collision_top(bx, by, bz)
            .max(fluid_height(view, bx, by, bz));
        if surface > 0.0 && self.y < f64::from(by) + surface {
            self.remove();
        }
    }

    /// `BubbleParticle.tick()` — rises gently and dies outside water.
    fn tick_bubble(&mut self, view: &dyn CollisionView) {
        self.xo = self.x;
        self.yo = self.y;
        self.zo = self.z;
        self.lifetime -= 1;
        if self.lifetime < 0 {
            self.remove();
            return;
        }
        self.yd += 0.002;
        self.move_by(self.xd, self.yd, self.zd, view);
        let drag = f64::from(0.85_f32);
        self.xd *= drag;
        self.yd *= drag;
        self.zd *= drag;
        let (bx, by, bz) = block_containing(self.x, self.y, self.z);
        if !view.is_water(bx, by, bz) {
            self.remove();
        }
    }

    /// `AttackSweepParticle.tick()` — a full override with no `move()` call at
    /// all: the sweep quad is stationary for its whole 4-tick life.
    ///
    /// Java: `if (this.age++ >= this.lifetime) { this.remove(); } else {
    /// this.setSpriteFromAge(this.sprites); }` — post-increment, so the
    /// removal check reads `age` *before* the increment, but the increment
    /// happens on both branches. Reproduced as a saved pre-increment check
    /// rather than a literal transliteration, since Rust has no postfix `++`.
    fn tick_sweep_attack(&mut self) {
        self.xo = self.x;
        self.yo = self.y;
        self.zo = self.z;
        let should_remove = self.age >= self.lifetime;
        self.age += 1;
        if should_remove {
            self.remove();
        } else {
            self.set_sprite_from_age();
        }
    }

    /// `SuspendedTownParticle.tick()` — a full override: no gravity, no
    /// friction, no collision, and a `lifetime`-*countdown* rather than an
    /// `age`-increment (so behaviours built on it never age past halfway —
    /// there is no halfway to reach).
    ///
    /// Java: `if (this.lifetime-- <= 0) { this.remove(); } else {
    /// this.move(xd, yd, zd); xd *= 0.99; yd *= 0.99; zd *= 0.99; }` —
    /// `move()` is itself overridden to skip collision entirely, matching
    /// [`Behaviour::Flame`]'s move override, so it is inlined here directly
    /// rather than routed through [`Self::move_by`].
    fn tick_suspended(&mut self) {
        self.xo = self.x;
        self.yo = self.y;
        self.zo = self.z;
        let should_remove = self.lifetime <= 0;
        self.lifetime -= 1;
        if should_remove {
            self.remove();
            return;
        }
        self.bb = self.bb.moved(self.xd, self.yd, self.zd);
        self.set_location_from_bounding_box();
        let damp = f64::from(0.99_f32);
        self.xd *= damp;
        self.yd *= damp;
        self.zd *= damp;
    }

    /// `HugeExplosionSeedParticle.tick()` — a full override, like
    /// [`Self::tick_sweep_attack`]/[`Self::tick_suspended`]: no `super.tick()`,
    /// no `move()`, just a fixed schedule of follow-up spawns.
    ///
    /// Java:
    /// ```text
    /// for (i = 0; i < 6; i++) {
    ///     xx = x + (nextDouble() - nextDouble()) * 4.0;   // ditto yy, zz
    ///     level.addParticle(EXPLOSION, xx, yy, zz, (float)age / lifetime, 0.0, 0.0);
    /// }
    /// age++;
    /// if (age == lifetime) remove();
    /// ```
    /// `size` is read *before* `age` is incremented, so the six spawns on a
    /// given tick all share one `size` and the sequence over the particle's
    /// 8-tick life is `0/8, 1/8, …, 7/8` — it never reaches `8/8`, since the
    /// particle removes itself the moment `age` *becomes* `lifetime` rather
    /// than after one more tick past it.
    ///
    /// Returns the six `(x, y, z, size)` requests for
    /// [`ParticleEngine::tick`] to turn into real [`Behaviour::HugeExplosion`]
    /// particles — see that function's own doc for why a spawn cannot happen
    /// directly here.
    fn tick_huge_explosion_seed(&mut self) -> Vec<(f64, f64, f64, f32)> {
        self.xo = self.x;
        self.yo = self.y;
        self.zo = self.z;
        #[expect(
            clippy::cast_precision_loss,
            reason = "age and lifetime are tiny (age < 8); this mirrors Java's int-to-float \
                      promotion in `(float) this.age / this.lifetime`"
        )]
        let size = self.age as f32 / self.lifetime as f32;
        let mut rng = self.tick_rng();
        let mut spawns = Vec::with_capacity(6);
        for _ in 0..6 {
            let jitter = |r: &mut JavaRandom| (r.next_double() - r.next_double()) * 4.0;
            let xx = self.x + jitter(&mut rng);
            let yy = self.y + jitter(&mut rng);
            let zz = self.z + jitter(&mut rng);
            spawns.push((xx, yy, zz, size));
        }
        self.age += 1;
        if self.age == self.lifetime {
            self.remove();
        }
        spawns
    }

    /// A deterministic per-tick [`JavaRandom`], derived from the particle's
    /// own state rather than a shared engine stream — see [`Self::rng_probe`]
    /// (its sole pre-existing caller) for why that is an acceptable stand-in
    /// for vanilla's per-particle `random`: particle-burst randomness is not
    /// parity-critical (module docs), only reproducible, and both callers of
    /// this need *several* draws in one tick, which `rng_probe`'s single
    /// `next_float()` cannot give them.
    fn tick_rng(&self) -> JavaRandom {
        let age_bits = u64::from(self.age.unsigned_abs());
        let seed = (self.x.to_bits() ^ self.z.to_bits() ^ age_bits).cast_signed();
        JavaRandom::new(seed)
    }

    /// A stand-in for the per-particle `random` in the two behaviours that draw
    /// during `tick`. Derived from the particle's own state so it stays
    /// deterministic without threading the engine RNG through every call.
    fn rng_probe(&self) -> f32 {
        self.tick_rng().next_float()
    }

    /// `move(double, double, double)`.
    ///
    /// [`Behaviour::Flame`] overrides this to translate without collision, which
    /// is why flames pass through the campfire logs they sit in.
    fn move_by(&mut self, xa: f64, ya: f64, za: f64, view: &dyn CollisionView) {
        if matches!(self.behaviour, Behaviour::Flame) {
            self.bb = self.bb.moved(xa, ya, za);
            self.set_location_from_bounding_box();
            return;
        }
        if self.stopped_by_collision {
            return;
        }
        let (original_xa, original_ya, original_za) = (xa, ya, za);
        let (mut xa, mut ya, mut za) = (xa, ya, za);

        let moving = xa != 0.0 || ya != 0.0 || za != 0.0;
        let speed_sq = xa.mul_add(xa, ya.mul_add(ya, za * za));
        if self.has_physics && moving && speed_sq < MAXIMUM_COLLISION_VELOCITY_SQUARED {
            // `Entity.collideBoundingBox(...)`: the swept resolve *without* the
            // auto-step mechanic. `collide` with `max_up_step == 0.0` skips the
            // step-up branch entirely, so this is exactly that function.
            let resolved = collide(view, Vec3d::new(xa, ya, za), self.bb, false, 0.0);
            xa = resolved.x;
            ya = resolved.y;
            za = resolved.z;
        }

        if xa != 0.0 || ya != 0.0 || za != 0.0 {
            self.bb = self.bb.moved(xa, ya, za);
            self.set_location_from_bounding_box();
        }

        // Once a falling particle is stopped hard it never moves again — this is
        // what pins block fragments to the floor instead of letting them creep.
        if original_ya.abs() >= f64::from(1.0e-5_f32) && ya.abs() < f64::from(1.0e-5_f32) {
            self.stopped_by_collision = true;
        }

        self.on_ground = original_ya != ya && original_ya < 0.0;
        if original_xa != xa {
            self.xd = 0.0;
        }
        if original_za != za {
            self.zd = 0.0;
        }
    }

    fn set_location_from_bounding_box(&mut self) {
        self.x = (self.bb.min_x + self.bb.max_x) / 2.0;
        self.y = self.bb.min_y;
        self.z = (self.bb.min_z + self.bb.max_z) / 2.0;
    }
}

/// `BlockPos.containing(double, double, double)` — floor, not truncate. The
/// difference only shows below y=0, which is exactly where the deepslate layers
/// are, so truncation here would misplace every particle in a cave.
fn block_containing(x: f64, y: f64, z: f64) -> (i32, i32, i32) {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "block coordinates are bounded well within i32"
    )]
    {
        (x.floor() as i32, y.floor() as i32, z.floor() as i32)
    }
}

/// `FluidState.getHeight` for the cell, or `0.0` where the view exposes no fluid
/// detail. Falls back to treating a present water cell as full, matching the
/// coarseness the live adapter already commits to elsewhere.
///
/// Uses `getOwnHeight` (`amount / 9`) rather than the `hasSameFluidAbove ? 1.0`
/// form: a water drop should die on the *surface* of a fluid column, and the
/// full-height variant only applies to a cell with more fluid stacked on top of
/// it, which by definition is not the surface.
fn fluid_height(view: &dyn CollisionView, x: i32, y: i32, z: i32) -> f64 {
    view.fluid_at(x, y, z).map_or_else(
        || if view.is_water(x, y, z) { 1.0 } else { 0.0 },
        |cell| f64::from(cell.own_height()),
    )
}

/// One extracted, camera-facing quad.
///
/// Positions are **relative to the camera**, matching vanilla's
/// `extractRotatedQuad`, which keeps the coordinates small and the float
/// precision good even thousands of blocks from the origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParticleQuad {
    /// Camera-relative centre.
    pub position: [f32; 3],
    /// Half-extent of the quad, in blocks.
    pub size: f32,
    /// Sprite-local UVs `(u0, u1, v0, v1)`; see [`Particle::uv_local`].
    pub uv: [f32; 4],
    /// Which texture to sample.
    pub sprite: SpriteSource,
    /// Linear RGBA tint.
    pub colour: [f32; 4],
    /// Packed block/sky light coords.
    pub light: u32,
    /// Roll about the view axis, in radians.
    pub roll: f32,
    /// Which pass to draw in.
    pub layer: Layer,
}

/// The live particle set.
///
/// Ticking is `O(n)` with no spatial structure, matching vanilla — particles are
/// short-lived and the cost is dominated by collision, which each particle
/// performs against its own small neighbourhood.
#[derive(Debug)]
pub struct ParticleEngine {
    particles: Vec<Particle>,
    rng: JavaRandom,
    capacity: usize,
}

impl ParticleEngine {
    /// Vanilla has no single global particle cap — it limits some types
    /// individually through `ParticleLimit` and scales spawn *rates* with the
    /// particle setting. A hard ceiling is ours, not vanilla's, and exists so a
    /// pathological emitter cannot stall a frame. It is high enough that normal
    /// play never reaches it.
    pub const DEFAULT_CAPACITY: usize = 16_384;

    /// A new engine seeded from the clock.
    #[must_use]
    pub fn new() -> Self {
        Self::with_rng(JavaRandom::from_entropy())
    }

    /// A new engine with a fixed seed, so a burst replays exactly. Used by every
    /// test in this crate.
    #[must_use]
    pub fn seeded(seed: i64) -> Self {
        Self::with_rng(JavaRandom::new(seed))
    }

    fn with_rng(rng: JavaRandom) -> Self {
        Self {
            particles: Vec::new(),
            rng,
            capacity: Self::DEFAULT_CAPACITY,
        }
    }

    /// Overrides the ceiling described on [`Self::DEFAULT_CAPACITY`].
    #[must_use]
    pub const fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    /// The engine's RNG, for emitters that need to draw before constructing.
    pub const fn rng(&mut self) -> &mut JavaRandom {
        &mut self.rng
    }

    /// Live particle count.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.particles.len()
    }

    /// Whether the engine holds no particles.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.particles.is_empty()
    }

    /// The live particles, for inspection and tests.
    #[must_use]
    pub fn particles(&self) -> &[Particle] {
        &self.particles
    }

    /// Adds a particle, silently dropping it if the engine is at capacity.
    ///
    /// Dropping rather than evicting is deliberate: evicting the oldest would
    /// make a large burst delete the smoke trail a player is currently watching.
    pub fn add(&mut self, particle: Particle) {
        if self.particles.len() < self.capacity {
            self.particles.push(particle);
        }
    }

    /// Removes every particle.
    pub fn clear(&mut self) {
        self.particles.clear();
    }

    /// Advances every particle one tick and sweeps the dead ones.
    ///
    /// [`Behaviour::HugeExplosionSeed`] is the one particle in this crate
    /// that spawns more particles from inside its own `tick()`
    /// (`HugeExplosionSeedParticle.tick()`'s `level.addParticle(EXPLOSION,
    /// …)` calls). [`Particle::tick`] cannot call [`Self::add`] itself — the
    /// loop below already holds `self.particles` mutably borrowed — so it
    /// returns its spawn requests instead, and they are turned into real
    /// particles only once the loop (and the borrow) has ended.
    pub fn tick(&mut self, view: &dyn CollisionView) {
        let mut spawns: Vec<(f64, f64, f64, f32)> = Vec::new();
        for p in &mut self.particles {
            spawns.extend(p.tick(view));
        }
        self.particles.retain(Particle::is_alive);
        for (x, y, z, size) in spawns {
            emit::huge_explosion(self, x, y, z, size);
        }
    }

    /// Extracts camera-relative quads for rendering.
    ///
    /// `partial_tick` is the fraction through the current tick, so particles
    /// interpolate smoothly at any frame rate rather than stepping 20 times a
    /// second. `light` samples packed light coords at a block position;
    /// behaviours that ignore world lighting never call it.
    pub fn extract(
        &self,
        camera: Vec3d,
        partial_tick: f32,
        light: &dyn Fn(i32, i32, i32) -> Option<u32>,
        out: &mut Vec<ParticleQuad>,
    ) {
        out.reserve(self.particles.len());
        let t = f64::from(partial_tick);
        for p in &self.particles {
            // `HugeExplosionSeedParticle` is a `NoRenderParticle` — vanilla
            // never gives it a quad at all, and `Behaviour::layer()` has no
            // "not drawn" value to return, so the exclusion lives here
            // instead, at the one place that turns a live particle into a
            // drawable quad.
            if matches!(p.behaviour, Behaviour::HugeExplosionSeed) {
                continue;
            }
            let x = p.xo + (p.x - p.xo) * t - camera.x;
            let y = p.yo + (p.y - p.yo) * t - camera.y;
            let z = p.zo + (p.z - p.zo) * t - camera.z;
            let light = match p.behaviour {
                // `SimpleAnimatedParticle.getLightCoords` returns full bright
                // unconditionally — spell and note particles are self-lit.
                // `AttackSweepParticle.getLightCoords` overrides to the same
                // constant explicitly (`15728880`), independently of
                // `SimpleAnimatedParticle`. `HugeExplosionParticle.
                // getLightCoords` overrides to the identical constant too.
                Behaviour::SimpleAnimated { .. } | Behaviour::SweepAttack | Behaviour::HugeExplosion => {
                    FULL_BRIGHT
                }
                _ => {
                    let (bx, by, bz) = block_containing(p.x, p.y, p.z);
                    light(bx, by, bz).unwrap_or(UNLOADED_LIGHT)
                }
            };
            #[expect(
                clippy::cast_possible_truncation,
                reason = "camera-relative coordinates are small by construction"
            )]
            out.push(ParticleQuad {
                position: [x as f32, y as f32, z as f32],
                size: p.quad_size(partial_tick),
                uv: p.uv_local(),
                sprite: p.sprite,
                colour: [p.colour[0], p.colour[1], p.colour[2], p.alpha],
                light,
                roll: p.o_roll + (p.roll - p.o_roll) * partial_tick,
                layer: p.behaviour.layer(),
            });
        }
    }
}

impl Default for ParticleEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Behaviour, Particle, ParticleEngine, Sheet, SpriteSource, block_containing, rng::JavaRandom,
    };
    use lodestone_physics::{Aabb, CollisionView, Vec3d};

    /// A world that is solid below `floor_y` and empty above.
    struct Floor {
        floor_y: i32,
        water_above: bool,
    }

    impl CollisionView for Floor {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if y < self.floor_y {
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

        fn is_water(&self, _x: i32, y: i32, _z: i32) -> bool {
            self.water_above && y >= self.floor_y
        }
    }

    const EMPTY: Floor = Floor {
        floor_y: i32::MIN,
        water_above: false,
    };

    fn plain(rng: &mut JavaRandom) -> Particle {
        Particle::new(
            0.5,
            10.0,
            0.5,
            SpriteSource::Sheet {
                sheet: Sheet::Generic,
                frame: 0,
            },
            rng,
        )
    }

    #[test]
    fn base_lifetime_is_the_vanilla_four_over_jitter_formula() {
        // `(int)(4.0F / (nextFloat() * 0.9F + 0.1F))` is bounded by construction:
        // the divisor lies in [0.1, 1.0), so the lifetime lies in (4, 40].
        // Deriving the bound from the *formula in the Java source* rather than
        // from this implementation is the point of the assertion.
        let mut rng = JavaRandom::new(5);
        for _ in 0..1_000 {
            let p = plain(&mut rng);
            assert!(
                (4..=40).contains(&p.lifetime),
                "lifetime {} outside the range the vanilla formula can produce",
                p.lifetime
            );
        }
    }

    #[test]
    fn a_particle_dies_exactly_when_its_age_passes_its_lifetime() {
        let mut rng = JavaRandom::new(11);
        let mut p = plain(&mut rng);
        p.lifetime = 3;
        for _ in 0..3 {
            p.tick(&EMPTY);
            assert!(p.is_alive(), "died early at age {}", p.age);
        }
        p.tick(&EMPTY);
        assert!(!p.is_alive(), "should be removed once age exceeds lifetime");
    }

    /// Gravity is `0.04 * gravity` per tick, *not* the entity constant `0.08`.
    /// Confusing the two makes every particle fall at twice the right speed,
    /// which is subtle enough to ship.
    #[test]
    fn gravity_is_four_hundredths_scaled_and_friction_follows_it() {
        let mut rng = JavaRandom::new(3);
        let mut p = plain(&mut rng);
        p.lifetime = 100;
        p.gravity = 1.0;
        p.xd = 0.0;
        p.yd = 0.0;
        p.zd = 0.0;
        p.tick(&EMPTY);
        // yd = (0 - 0.04 * 1.0) * 0.98, with 0.98 widened from f32.
        let expected = -0.04 * f64::from(0.98_f32);
        assert!(
            (p.yd - expected).abs() < 1e-12,
            "yd was {}, expected {expected}",
            p.yd
        );
    }

    /// The float-widening rule stated in the crate docs, asserted rather than
    /// merely documented: `0.98_f32` is not `0.98`.
    #[test]
    fn friction_uses_the_widened_float_not_the_double_literal() {
        let mut rng = JavaRandom::new(4);
        let mut p = plain(&mut rng);
        p.lifetime = 100;
        p.gravity = 0.0;
        p.xd = 1.0;
        p.tick(&EMPTY);
        assert!(
            (p.xd - f64::from(0.98_f32)).abs() < 1e-18,
            "xd was {}",
            p.xd
        );
        assert!(
            (p.xd - 0.98).abs() > 1e-10,
            "xd matched the f64 literal, so the widening was lost"
        );
    }

    #[test]
    fn a_falling_particle_lands_on_the_floor_and_stops() {
        let world = Floor {
            floor_y: 8,
            water_above: false,
        };
        let mut rng = JavaRandom::new(21);
        let mut p = plain(&mut rng);
        p.lifetime = 200;
        p.gravity = 1.0;
        for _ in 0..200 {
            p.tick(&world);
            if !p.is_alive() {
                break;
            }
        }
        assert!(
            (p.y - 8.0).abs() < 1e-6,
            "came to rest at y={} rather than on the floor at 8",
            p.y
        );
        assert!(p.on_ground, "should report standing on the floor");
    }

    /// A negative control for the test above: with `has_physics` off, the same
    /// particle must fall *through* the floor. Without this, a collision test
    /// that silently never collides would still pass.
    #[test]
    fn without_physics_the_same_particle_falls_through_the_floor() {
        let world = Floor {
            floor_y: 8,
            water_above: false,
        };
        let mut rng = JavaRandom::new(21);
        let mut p = plain(&mut rng);
        p.lifetime = 200;
        p.gravity = 1.0;
        p.has_physics = false;
        for _ in 0..60 {
            p.tick(&world);
        }
        assert!(
            p.y < 8.0,
            "physics-free particle stopped at y={}, so the floor was consulted",
            p.y
        );
    }

    #[test]
    fn flame_ignores_collision_entirely() {
        let world = Floor {
            floor_y: 8,
            water_above: false,
        };
        let mut rng = JavaRandom::new(31);
        let mut p = plain(&mut rng);
        p.behaviour = Behaviour::Flame;
        p.lifetime = 200;
        p.gravity = 1.0;
        for _ in 0..60 {
            p.tick(&world);
        }
        assert!(
            p.y < 8.0,
            "flame stopped at y={}, but FlameParticle overrides move() to skip collision",
            p.y
        );
    }

    #[test]
    fn a_bubble_dies_the_moment_it_leaves_water() {
        let world = Floor {
            floor_y: 0,
            water_above: false,
        };
        let mut rng = JavaRandom::new(41);
        let mut p = plain(&mut rng);
        p.behaviour = Behaviour::Bubble;
        p.lifetime = 50;
        p.tick(&world);
        assert!(!p.is_alive(), "bubble survived outside water");
    }

    #[test]
    fn a_bubble_in_water_survives_and_rises() {
        let world = Floor {
            floor_y: 0,
            water_above: true,
        };
        let mut rng = JavaRandom::new(41);
        let mut p = plain(&mut rng);
        p.behaviour = Behaviour::Bubble;
        p.lifetime = 50;
        p.yd = 0.0;
        p.tick(&world);
        assert!(p.is_alive(), "bubble died in water");
        assert!(p.yd > 0.0, "bubble should gain upward velocity");
    }

    #[test]
    fn crit_particles_warm_towards_red_as_they_age() {
        let mut rng = JavaRandom::new(51);
        let mut p = plain(&mut rng);
        p.behaviour = Behaviour::Crit;
        p.lifetime = 40;
        p.colour = [1.0, 1.0, 1.0];
        for _ in 0..10 {
            p.tick(&EMPTY);
        }
        assert!(
            p.colour[0] > p.colour[1] && p.colour[1] > p.colour[2],
            "expected r > g > b after ageing, got {:?}",
            p.colour
        );
    }

    /// The mirrored UV range is easy to mistake for a bug, so it is pinned.
    #[test]
    fn terrain_uvs_take_a_mirrored_quarter_of_the_block_sprite() {
        let mut rng = JavaRandom::new(61);
        let mut p = plain(&mut rng);
        p.behaviour = Behaviour::Terrain { uo: 2.0, vo: 1.0 };
        let [u0, u1, v0, v1] = p.uv_local();
        assert!((u0 - 0.75).abs() < 1e-6, "u0 was {u0}");
        assert!((u1 - 0.5).abs() < 1e-6, "u1 was {u1}");
        assert!(u0 > u1, "u0 must exceed u1 — vanilla mirrors the fragment");
        assert!((v0 - 0.25).abs() < 1e-6, "v0 was {v0}");
        assert!((v1 - 0.5).abs() < 1e-6, "v1 was {v1}");
    }

    #[test]
    fn sprite_frames_advance_with_age_and_clamp_at_the_last_one() {
        assert_eq!(Sheet::Generic.frame_for_age(0, 8), 0);
        assert_eq!(Sheet::Generic.frame_for_age(4, 8), 4);
        assert_eq!(Sheet::Generic.frame_for_age(7, 8), 7);
        // Past the end it must clamp, not wrap back to frame 0 — a wrap makes a
        // dying smoke puff flash bright for one frame.
        assert_eq!(Sheet::Generic.frame_for_age(80, 8), 7);
        // A single-frame sheet has no numeric suffix at all.
        assert_eq!(Sheet::Flame.texture_name(0), "particle/flame");
        assert_eq!(Sheet::Generic.texture_name(3), "particle/generic_3");
    }

    #[test]
    fn block_positions_floor_rather_than_truncate() {
        assert_eq!(block_containing(0.5, 0.5, 0.5), (0, 0, 0));
        // The case that separates floor from truncation, and the one that
        // matters underground.
        assert_eq!(block_containing(-0.5, -0.5, -0.5), (-1, -1, -1));
    }

    #[test]
    fn the_engine_sweeps_dead_particles_and_respects_its_ceiling() {
        let mut engine = ParticleEngine::seeded(7).with_capacity(4);
        for _ in 0..10 {
            let p = plain(engine.rng());
            engine.add(p);
        }
        assert_eq!(engine.len(), 4, "capacity should have refused the rest");
        for p in &mut engine.particles {
            p.lifetime = 1;
        }
        engine.tick(&EMPTY);
        engine.tick(&EMPTY);
        assert!(engine.is_empty(), "dead particles were not swept");
    }

    #[test]
    fn extraction_is_camera_relative_and_interpolates_between_ticks() {
        let mut engine = ParticleEngine::seeded(9);
        let mut p = plain(engine.rng());
        p.lifetime = 100;
        p.xo = 0.0;
        p.x = 2.0;
        p.yo = 10.0;
        p.y = 10.0;
        p.zo = 0.0;
        p.z = 0.0;
        engine.add(p);

        let mut out = Vec::new();
        engine.extract(Vec3d::new(1.0, 10.0, 0.0), 0.5, &|_, _, _| Some(0), &mut out);
        assert_eq!(out.len(), 1);
        // Halfway from x=0 to x=2 is x=1; the camera sits at x=1, so 0.
        assert!(
            out[0].position[0].abs() < 1e-5,
            "expected the particle at the camera, got {:?}",
            out[0].position
        );
    }

    #[test]
    fn unlit_particles_fall_back_to_the_unloaded_chunk_light() {
        let mut engine = ParticleEngine::seeded(13);
        let p = plain(engine.rng());
        engine.add(p);
        let mut out = Vec::new();
        engine.extract(Vec3d::ZERO, 0.0, &|_, _, _| None, &mut out);
        assert_eq!(out[0].light, super::UNLOADED_LIGHT);
    }

    #[test]
    fn self_lit_particles_ignore_the_light_sampler_entirely() {
        let mut engine = ParticleEngine::seeded(17);
        let mut p = plain(engine.rng());
        p.behaviour = Behaviour::SimpleAnimated { fade: None };
        engine.add(p);
        let mut out = Vec::new();
        engine.extract(Vec3d::ZERO, 0.0, &|_, _, _| Some(0), &mut out);
        assert_eq!(
            out[0].light,
            super::FULL_BRIGHT,
            "SimpleAnimatedParticle must be full bright regardless of the world"
        );
    }
}
