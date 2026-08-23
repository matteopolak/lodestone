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
    /// `particle/generic_0` … `generic_7` **ascending** — `portal.json` and
    /// `reverse_portal.json`.
    ///
    /// The same eight physical textures as [`Self::Generic`] in the opposite
    /// order, which is why it has to be its own variant: a sheet's identity here
    /// is its *frame sequence*, not its pixels. See [`Self::frames`].
    PortalGeneric,
    /// `particle/soul_0` … `soul_10` — `soul.json`. Eleven frames, ascending.
    Soul,
    /// `particle/soul_fire_flame` — the blue flame over soul fire/soul torches.
    SoulFireFlame,
    /// `particle/sga_a` … `sga_z` — `enchant.json`'s twenty-six Standard Galactic
    /// glyphs. **Not numerically suffixed at all**, which is the reason
    /// [`Self::frames`] exists rather than a `<stem>_<n>` format string.
    Enchant,
    /// `particle/drip_hang` — a drip clinging to the underside of a block
    /// (`dripping_water`, `dripping_lava`).
    DripHang,
    /// `particle/drip_fall` — a drip in free fall (`falling_water`,
    /// `falling_lava`, `spore_blossom_air`).
    DripFall,
    /// `particle/drip_land` — a drip's splash on landing (`landing_lava`).
    DripLand,
    /// `particle/big_smoke_0` … `big_smoke_11` — campfire smoke, ascending.
    BigSmoke,
    /// `particle/sculk_charge_0` … `sculk_charge_6`, ascending.
    SculkCharge,
    /// `particle/gust_0` … `gust_11`, ascending.
    Gust,
    /// `particle/sonic_boom_0` … `sonic_boom_15`, ascending.
    SonicBoom,
    /// `particle/glow` — the single-frame spark `electric_spark` and `glow` share.
    Glow,
    /// `particle/spark_0` … `spark_7` — `FireworkParticles.SparkParticle`
    /// (`ParticleTypes.FIREWORK`). A distinct physical sheet from
    /// [`Self::Glow`]: `firework.json` names `spark_N` textures, not `glow`,
    /// so the two spark-ish particles (`firework` and `electric_spark`/`glow`)
    /// share nothing but a name pattern.
    Spark,
    /// `particle/damage` — `damage_indicator.json`'s single frame.
    ///
    /// The damage indicator is a `CritParticle`, but it does **not** share
    /// [`Self::CriticalHit`]'s sprite: its own definition names `damage`, a
    /// separate texture. Deriving a sheet from the Java class rather than from
    /// the pack's `particles/<name>.json` is exactly the mistake
    /// [`Self::Spell`] documents for the `SpellParticle` family.
    Damage,
    /// `particle/infested` — `infested.json`'s single frame.
    ///
    /// Another `SpellParticle` over a one-frame sheet rather than an
    /// eight-frame one, so `setSpriteFromAge` is a no-op for it.
    Infested,
    /// `particle/raid_omen` — `raid_omen.json`'s single frame.
    RaidOmen,
    /// `particle/trial_omen` — `trial_omen.json`'s single frame.
    TrialOmen,
    /// `particle/nautilus` — the conduit's homing mote.
    Nautilus,
    /// `particle/generic_0` **alone** — a one-frame sheet, not a frame of
    /// [`Self::Generic`].
    ///
    /// Eight registry types (`ash`, `white_ash`, `crimson_spore`,
    /// `warped_spore`, `mycelium`, `underwater`, `dolphin`, `trail`) name
    /// exactly one texture in their own `particles/<name>.json`, and it happens
    /// to be the same PNG [`Self::Generic`]'s last frame uses. Reusing
    /// `Generic` for them would animate a still particle through eight frames,
    /// so — as with [`Self::PortalGeneric`] — a sheet's identity here is its
    /// **frame sequence**, not its pixels.
    Generic0,
    /// `particle/copper_fire_flame` — copper fire's own single-frame flame,
    /// **not** a tint of [`Self::Flame`]. `copper_fire_flame.json` names its own
    /// texture even though the type is registered against the same
    /// `FlameParticle.Provider` as `flame` and `soul_fire_flame`.
    CopperFireFlame,
    /// `particle/small_gust_0` … `small_gust_6` — seven frames, ascending.
    ///
    /// **A different physical sheet from [`Self::Gust`]**, despite
    /// `ParticleTypes.SMALL_GUST` being registered against the same
    /// `GustParticle` class. `small_gust.json` names `small_gust_N`, not
    /// `gust_N`, and it has seven frames rather than twelve — so a small gust
    /// pointed at `Gust` samples the wrong texture *and* runs off the end of a
    /// sequence it does not have.
    SmallGust,
    /// `particle/lava` — its own single texture.
    Lava,
    /// `particle/sculk_charge_pop_0` … `sculk_charge_pop_3` — four frames,
    /// ascending. A different sheet from [`Self::SculkCharge`]'s seven.
    SculkChargePop,
    /// `particle/sculk_soul_0` … `sculk_soul_10` — eleven frames, ascending.
    ///
    /// Its own sheet, not [`Self::Soul`]'s: `sculk_soul.json` names
    /// `sculk_soul_N`, and only the frame *count* coincides.
    SculkSoul,
    /// `particle/generic_5`, `generic_6`, `generic_7` — `dragon_breath.json`.
    ///
    /// **Three** frames, ascending, and a *subsequence* of [`Self::Generic`]'s
    /// eight rather than a sheet of its own textures: the pack file lists only
    /// the last three, ascending, where `Generic` runs all eight descending.
    /// Another case where a sheet's identity is its frame *sequence* and not
    /// its pixels, like [`Self::PortalGeneric`] and [`Self::Generic0`].
    DragonBreath,
}

impl Sheet {
    /// Every frame's file stem under `assets/minecraft/textures/particle/`, **in
    /// the order the pack's own `particles/<type>.json` lists them**.
    ///
    /// # This is a list and not a `<stem>_<n>` format string, for two measured
    /// reasons
    ///
    /// * **Half of vanilla's multi-frame sheets are listed descending.**
    ///   `smoke.json`, `cloud.json`, `large_smoke.json`, `snowflake.json`,
    ///   `effect.json`, `witch.json`, `instant_effect.json`, `end_rod.json` and
    ///   `totem_of_undying.json` all run `…_7` down to `…_0`. A synthesised
    ///   ascending suffix therefore animated every one of them **backwards** —
    ///   smoke grew instead of dissipating — and nothing caught it, because a
    ///   sprite lookup still resolved.
    /// * **`enchant.json` is not numerically suffixed at all**: its twenty-six
    ///   frames are `sga_a` … `sga_z`, which no format string can express.
    ///
    /// Read off the real `client.jar` rather than transcribed from memory. A sheet
    /// whose identity is its *sequence* rather than its pixels is why
    /// [`Self::Generic`] and [`Self::PortalGeneric`] are two variants over the same
    /// eight textures.
    #[must_use]
    pub const fn frames(self) -> &'static [&'static str] {
        match self {
            // Descending, per `smoke.json` / `cloud.json` / `large_smoke.json`.
            Self::Generic => &[
                "generic_7", "generic_6", "generic_5", "generic_4", "generic_3", "generic_2",
                "generic_1", "generic_0",
            ],
            Self::PortalGeneric => &[
                "generic_0", "generic_1", "generic_2", "generic_3", "generic_4", "generic_5",
                "generic_6", "generic_7",
            ],
            Self::CriticalHit => &["critical_hit"],
            Self::EnchantedHit => &["enchanted_hit"],
            Self::Flame => &["flame"],
            Self::SoulFireFlame => &["soul_fire_flame"],
            Self::Splash => &["splash_0", "splash_1", "splash_2", "splash_3"],
            Self::Bubble => &["bubble"],
            Self::Note => &["note"],
            Self::Heart => &["heart"],
            // Descending, per `effect.json`.
            Self::Effect => &[
                "effect_7", "effect_6", "effect_5", "effect_4", "effect_3", "effect_2", "effect_1",
                "effect_0",
            ],
            // Descending, per `end_rod.json` and `totem_of_undying.json`.
            Self::Glitter => &[
                "glitter_7", "glitter_6", "glitter_5", "glitter_4", "glitter_3", "glitter_2",
                "glitter_1", "glitter_0",
            ],
            Self::SweepAttack => &[
                "sweep_0", "sweep_1", "sweep_2", "sweep_3", "sweep_4", "sweep_5", "sweep_6",
                "sweep_7",
            ],
            // Descending, per `witch.json` and `instant_effect.json`.
            Self::Spell => &[
                "spell_7", "spell_6", "spell_5", "spell_4", "spell_3", "spell_2", "spell_1",
                "spell_0",
            ],
            Self::Angry => &["angry"],
            Self::Glint => &["glint"],
            Self::Explosion => &[
                "explosion_0", "explosion_1", "explosion_2", "explosion_3", "explosion_4",
                "explosion_5", "explosion_6", "explosion_7", "explosion_8", "explosion_9",
                "explosion_10", "explosion_11", "explosion_12", "explosion_13", "explosion_14",
                "explosion_15",
            ],
            Self::Soul => &[
                "soul_0", "soul_1", "soul_2", "soul_3", "soul_4", "soul_5", "soul_6", "soul_7",
                "soul_8", "soul_9", "soul_10",
            ],
            Self::Enchant => &[
                "sga_a", "sga_b", "sga_c", "sga_d", "sga_e", "sga_f", "sga_g", "sga_h", "sga_i",
                "sga_j", "sga_k", "sga_l", "sga_m", "sga_n", "sga_o", "sga_p", "sga_q", "sga_r",
                "sga_s", "sga_t", "sga_u", "sga_v", "sga_w", "sga_x", "sga_y", "sga_z",
            ],
            Self::DripHang => &["drip_hang"],
            Self::DripFall => &["drip_fall"],
            Self::DripLand => &["drip_land"],
            Self::BigSmoke => &[
                "big_smoke_0", "big_smoke_1", "big_smoke_2", "big_smoke_3", "big_smoke_4",
                "big_smoke_5", "big_smoke_6", "big_smoke_7", "big_smoke_8", "big_smoke_9",
                "big_smoke_10", "big_smoke_11",
            ],
            Self::SculkCharge => &[
                "sculk_charge_0", "sculk_charge_1", "sculk_charge_2", "sculk_charge_3",
                "sculk_charge_4", "sculk_charge_5", "sculk_charge_6",
            ],
            Self::Gust => &[
                "gust_0", "gust_1", "gust_2", "gust_3", "gust_4", "gust_5", "gust_6", "gust_7",
                "gust_8", "gust_9", "gust_10", "gust_11",
            ],
            Self::SonicBoom => &[
                "sonic_boom_0", "sonic_boom_1", "sonic_boom_2", "sonic_boom_3", "sonic_boom_4",
                "sonic_boom_5", "sonic_boom_6", "sonic_boom_7", "sonic_boom_8", "sonic_boom_9",
                "sonic_boom_10", "sonic_boom_11", "sonic_boom_12", "sonic_boom_13",
                "sonic_boom_14", "sonic_boom_15",
            ],
            Self::Glow => &["glow"],
            // Descending, per `firework.json` — the same "reads the pack file
            // as the list, never assumes ascending" rule `Generic`/`Effect`/
            // `Glitter`/`Spell` above already document.
            Self::Spark => &[
                "spark_7", "spark_6", "spark_5", "spark_4", "spark_3", "spark_2", "spark_1",
                "spark_0",
            ],
            Self::Damage => &["damage"],
            Self::Infested => &["infested"],
            Self::RaidOmen => &["raid_omen"],
            Self::TrialOmen => &["trial_omen"],
            Self::Nautilus => &["nautilus"],
            Self::Generic0 => &["generic_0"],
            Self::CopperFireFlame => &["copper_fire_flame"],
            Self::SmallGust => &[
                "small_gust_0", "small_gust_1", "small_gust_2", "small_gust_3", "small_gust_4",
                "small_gust_5", "small_gust_6",
            ],
            Self::Lava => &["lava"],
            Self::SculkChargePop => &[
                "sculk_charge_pop_0", "sculk_charge_pop_1", "sculk_charge_pop_2",
                "sculk_charge_pop_3",
            ],
            Self::SculkSoul => &[
                "sculk_soul_0", "sculk_soul_1", "sculk_soul_2", "sculk_soul_3", "sculk_soul_4",
                "sculk_soul_5", "sculk_soul_6", "sculk_soul_7", "sculk_soul_8", "sculk_soul_9",
                "sculk_soul_10",
            ],
            // Ascending, per `dragon_breath.json` -- and only the last three
            // of `generic_N`, not all eight.
            Self::DragonBreath => &["generic_5", "generic_6", "generic_7"],
        }
    }

    /// How many frames the sheet has — always `frames().len()`, never a second
    /// hand-maintained number.
    #[must_use]
    pub const fn frame_count(self) -> u16 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the longest sheet is 26 frames"
        )]
        {
            self.frames().len() as u16
        }
    }

    /// Resource path of one frame, e.g. `particle/generic_3`, or `particle/flame`
    /// for a single-frame sheet.
    ///
    /// `frame` is clamped into range rather than panicking: a sprite lookup is
    /// not worth aborting a frame over.
    #[must_use]
    pub fn texture_name(self, frame: u16) -> String {
        let frames = self.frames();
        let index = usize::from(frame).min(frames.len().saturating_sub(1));
        format!("particle/{}", frames[index])
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
            Self::PortalGeneric,
            Self::Soul,
            Self::SoulFireFlame,
            Self::Enchant,
            Self::DripHang,
            Self::DripFall,
            Self::DripLand,
            Self::BigSmoke,
            Self::SculkCharge,
            Self::Gust,
            Self::SonicBoom,
            Self::Glow,
            Self::Spark,
            Self::Damage,
            Self::Infested,
            Self::RaidOmen,
            Self::TrialOmen,
            Self::Nautilus,
            Self::Generic0,
            Self::CopperFireFlame,
            Self::SmallGust,
            Self::SculkSoul,
            Self::Lava,
            Self::SculkChargePop,
            Self::DragonBreath,
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
    /// The particle texture of an **item**, by network registry id —
    /// `BreakingItemParticle`, which vanilla textures from
    /// `ItemStackRenderState.pickParticleMaterial`, i.e. the item model's own
    /// sprite.
    ///
    /// Carrying the *item* rather than a resolved sprite is what makes eating a
    /// carrot throw orange crumbs and eating a beetroot throw red ones. A generic
    /// crumb satisfies any "some particles spawned" check and is visibly wrong for
    /// every coloured food, so the identity travels with the particle and the
    /// shell resolves it the same way it resolves [`Self::BlockState`].
    Item(u32),
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
    /// `PortalParticle` — the nether-portal shimmer, and the one particle here
    /// whose position is a **closed-form function of age** rather than an
    /// integration of velocity.
    ///
    /// Its `tick()` recomputes `x/y/z` from the spawn point every tick:
    /// `pos = age/lifetime`, then `pos = 1 - (-pos + 2*pos²)`, and
    /// `x = xStart + xd*pos` (with `y` additionally taking `+ (1 - age/lifetime)`,
    /// so the mote drifts *up* toward the portal's centre as it converges). Since
    /// nothing integrates, `gravity` and `friction` are never read, and `move` is
    /// overridden to skip collision. See [`Particle::tick_portal`].
    Portal,
    /// `CampfireSmokeParticle` — the tall lazy column over a campfire, cosy or
    /// signal.
    ///
    /// A full `tick()` override: a *tiny* gravity (`3.0e-6`, five orders of
    /// magnitude below the ordinary `0.04`) applied to `yd` directly rather than
    /// through the base tick, a per-tick random horizontal nudge that makes the
    /// column wander, no friction at all, and an alpha fade over the **last 60
    /// ticks** rather than the back half of life. Getting the fade wrong is what
    /// makes signal smoke vanish halfway up. See [`Particle::tick_campfire_smoke`].
    CampfireSmoke,
    /// `DustParticle` (`ParticleTypes.DUST`) via `DustParticleBase` — the
    /// `minecraft:dust` particle option's colour, randomised once per
    /// particle and held for its whole life. Shares `dust.json`'s sheet
    /// (`generic_0`…`generic_7`, the same eight textures as [`Sheet::Generic`]
    /// itself — confirmed against the real pack rather than assumed from the
    /// registry name) and the same age-driven sheet animation as
    /// [`Self::AshSmoke`]. See [`crate::emit::dust`].
    Dust,
    /// `DustColorTransitionParticle` (`ParticleTypes.DUST_COLOR_TRANSITION`) —
    /// the sculk-to-redstone sibling of [`Self::Dust`]. Same physics and
    /// sheet; the colour itself lerps from `from` to `to` over the particle's
    /// life instead of staying fixed. Vanilla recomputes the lerp every frame
    /// from a partial tick (`DustColorTransitionParticle.lerpColors`); this
    /// port advances it once per game tick instead, the same granularity
    /// [`Self::Crit`]'s desaturation already uses rather than a per-frame
    /// interpolation. See [`crate::emit::dust_color_transition`].
    DustColorTransition {
        /// Randomised starting colour (`DustColorTransitionOptions::fromColor`,
        /// after `DustParticleBase::randomizeColor`).
        from: [f32; 3],
        /// Randomised ending colour.
        to: [f32; 3],
    },
    /// `FlyTowardsPositionParticle` — the enchanting-table glyphs and the
    /// conduit's `nautilus` mote.
    ///
    /// The second behaviour here whose position is a **closed-form function of
    /// age** rather than an integration of velocity, and it is *not*
    /// [`Self::Portal`] with different constants. Two differences that matter:
    ///
    /// * `xd/yd/zd` are the offset the mote starts at and converges *from*, so
    ///   the emitter is given `target + offset` and flies to `target` — the
    ///   opposite of a velocity, and the reason the constructor immediately
    ///   sets `x = xStart + xd`.
    /// * the vertical term is a **quartic** sag (`pp = (1 - pos)⁴`, then
    ///   `y = yStart + yd*pos - pp*1.2`), not `Portal`'s linear `1 - age/lifetime`
    ///   rise. Reading it as a rise puts the glyphs above the table instead of
    ///   dipping into it.
    ///
    /// `move()` is overridden to skip collision and nothing integrates, so
    /// `gravity` and `friction` are never read. See
    /// [`Particle::tick_fly_towards_position`].
    FlyTowardsPosition,
    /// Ordinary physics plus `setSpriteFromAge`, and **nothing else** —
    /// `ExplodeParticle` (`poof`, `spit`) and its siblings.
    ///
    /// Distinct from [`Self::AshSmoke`], which looks like the same thing and is
    /// not: `BaseAshSmokeParticle` additionally overrides `getQuadSize` with the
    /// `clamp((age + a) / lifetime * 32, 0, 1)` fade-*in*, and `ExplodeParticle`
    /// does not. Borrowing `AshSmoke` for a class without that override makes
    /// every one of its particles start at zero size and swell over the first
    /// thirty-second of its life — visible on a mob-death puff, and invisible to
    /// any test that only asks whether a particle exists.
    ///
    /// The layer is a field because the classes in this shape do not agree on
    /// it: `ExplodeParticle` is `OPAQUE` and `SculkChargePopParticle` is
    /// `TRANSLUCENT`, and there is nothing else to tell them apart.
    Animated {
        /// `getLayer()`.
        layer: Layer,
    },
    /// `PlayerCloudParticle` — the `cloud` puff and a panda's `sneeze`.
    ///
    /// Ordinary physics plus `setSpriteFromAge`, the same `* 32` fade-in
    /// [`Self::AshSmoke`] has, and a **translucent** layer — the one combination
    /// no existing variant offers.
    ///
    /// One thing is deliberately not ported: vanilla's `tick` also drags the
    /// puff down toward the nearest player within two blocks
    /// (`level.getNearestPlayer(x, y, z, 2.0, false)`), which is what makes an
    /// area-effect cloud settle around your feet. That needs a player position
    /// this crate has no access to, and it is a *drift*, not a lifetime or a
    /// colour, so its absence reads as a slightly less clingy cloud rather than
    /// a missing particle.
    Cloud,
    /// `DragonBreathParticle` — the lingering cloud a dragon's breath attack
    /// and a lingering potion leave on the ground.
    ///
    /// A full `tick()` override that never calls `super.tick()`, so none of
    /// [`Particle::tick_base`]'s gravity, vertical friction or ground drag
    /// applies: it damps `xd`/`zd` only, and accelerates *horizontally* when
    /// its height stops changing (`if (y == yo) { xd *= 1.1; zd *= 1.1; }`),
    /// which is what makes the cloud creep outward across a floor rather than
    /// settling. It also fades **in** over the first thirty-second of its life,
    /// the same `clamp((age + a) / lifetime * 32, 0, 1)` ramp
    /// [`Self::Crit`] and [`Self::AshSmoke`] have.
    ///
    /// `hit_ground` is `DragonBreathParticle.hasHitGround`, which arms the
    /// `yd += 0.002` lift and the vertical friction. It is transcribed rather
    /// than dropped even though `hasPhysics = false` means
    /// [`Particle::move_by`] can never set `on_ground` and so it can never
    /// become `true` — that is vanilla's own arrangement, and a port that
    /// silently omits a clause because the clause happens to be unreachable is
    /// how a later change to one field quietly breaks another.
    DragonBreath {
        /// Whether the cloud has touched a surface.
        hit_ground: bool,
    },
    /// `LavaParticle` — the pops off a lava surface, and the only particle in
    /// this crate that spawns a **different type** as it lives.
    ///
    /// Its quad shrinks quadratically (`quadSize * (1 - s²)`, not
    /// [`Self::Flame`]'s `1 - s²/2`), and every tick it rolls
    /// `nextFloat() > age / lifetime` and emits a smoke particle at its own
    /// position and velocity if it passes — so a fresh pop trails smoke almost
    /// every tick and an old one almost never does. Dropping that roll leaves a
    /// bare orange dot where vanilla has a smoking ember.
    Lava,
    /// `SquidInkParticle` — a squid's ink cloud and a glow squid's.
    ///
    /// [`Self::SimpleAnimated`] exactly (its alpha-fade formula is the same
    /// expression, and both are full-bright and translucent) **plus** a
    /// `yd -= 0.0074` sink whenever the cloud is in air rather than water, which
    /// is what makes ink released above the surface fall instead of hanging.
    SquidInk,
    /// `DripParticle` — the seventeen registry types that make up a drip's
    /// three-phase life: it hangs under a block, lets go and falls, and splashes
    /// where it lands.
    ///
    /// **Those three phases are three separate registry types, and vanilla
    /// chains them from inside `tick()`** — a `dripping_water` particle spawns a
    /// `falling_water` one when its 40 ticks are up, and that one spawns a
    /// `splash` when it hits the ground. Modelling only the phase the server
    /// asked for gives a cave ceiling where drips appear, hang, and blink out of
    /// existence without ever falling, which is what this client did.
    ///
    /// Not [`Self::WaterDrop`], which is `WaterDropParticle` — a different class
    /// with a different tick. See [`Particle::tick_drip`].
    Drip {
        /// Which fluid's drip this is, which decides the colour, the gravity,
        /// the lifetime and what the next phase is.
        kind: DripKind,
        /// Where in the hang → fall → land chain this particle sits.
        phase: DripPhase,
    },
}

/// Which of vanilla's seven drip fluids a [`Behaviour::Drip`] belongs to.
///
/// The fluid is what decides everything about a drip except its sprite: two
/// drips in the same phase differ only by this. `Dripstone*` are genuinely
/// separate from their plain siblings — a dripstone drip plays a sound on
/// landing and a plain one does not — even though `dripping_dripstone_water`
/// and `dripping_water` are pixel-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DripKind {
    /// `dripping_water` / `falling_water`. Lands as a `splash`.
    Water,
    /// `dripping_lava` / `falling_lava` / `landing_lava`. Its hanging phase
    /// **cools**: see [`Particle::tick_drip`].
    Lava,
    /// `dripping_honey` / `falling_honey` / `landing_honey`.
    Honey,
    /// `falling_nectar` — a bee's trail. Falling phase only; it simply vanishes.
    Nectar,
    /// `dripping_obsidian_tear` / `falling_obsidian_tear` /
    /// `landing_obsidian_tear`. The only glowing member.
    ObsidianTear,
    /// `dripping_dripstone_water` / `falling_dripstone_water`. Lands as a
    /// `splash`, like plain water.
    DripstoneWater,
    /// `dripping_dripstone_lava` / `falling_dripstone_lava`. Lands as
    /// `landing_lava` — it borrows plain lava's landing type rather than having
    /// one of its own.
    DripstoneLava,
    /// `falling_spore_blossom`. Falling phase only.
    ///
    /// Distinct from `spore_blossom_air`, which is a `SuspendedParticle` and
    /// merely shares the `drip_fall` texture.
    SporeBlossom,
}

/// Where in the hang → fall → land chain a [`Behaviour::Drip`] sits.
///
/// Vanilla models these as four `DripParticle` subclasses that differ **only**
/// in their `preMoveUpdate`/`postMoveUpdate` hooks; everything else is shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DripPhase {
    /// `DripHangParticle` — clinging under a block. Its velocity is damped to a
    /// **fiftieth** every tick so it does not drift, and when its lifetime
    /// expires it spawns the falling phase rather than simply dying.
    Hang,
    /// `FallingParticle` / `FallAndLandParticle` — in free fall. Removed on
    /// contact with the ground, spawning the landing phase if its kind has one.
    Fall,
    /// `DripLandParticle` — the splash where it landed. The end of the chain.
    Land,
}

/// A follow-up particle a live particle's own `tick` asks the engine to spawn.
///
/// Returned from [`Particle::tick`] rather than pushed directly, because a
/// `for p in &mut self.particles` loop already holds the vector mutably borrowed
/// and a particle cannot add a sibling to it from inside its own tick.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Spawn {
    /// One of the six puffs a [`Behaviour::HugeExplosionSeed`] throws per tick.
    HugeExplosion {
        /// Where.
        pos: [f64; 3],
        /// `HugeExplosionParticle`'s `size` constructor argument.
        size: f32,
    },
    /// The next phase of a [`Behaviour::Drip`]'s chain.
    Drip {
        /// The fluid, carried through unchanged.
        kind: DripKind,
        /// The phase to spawn — never the one that asked for it.
        phase: DripPhase,
        /// Where.
        pos: [f64; 3],
        /// The velocity to inherit. `DripHangParticle` hands its own on;
        /// `FallAndLandParticle` hands on zero.
        vel: [f64; 3],
    },
    /// The trail a [`Behaviour::Lava`] pop throws, at its own position and
    /// velocity.
    Smoke {
        /// Where.
        pos: [f64; 3],
        /// The pop's own velocity, handed on unchanged.
        vel: [f64; 3],
    },
    /// A `splash` — what a water drip lands as, instead of a landing phase of
    /// its own. `SplashParticle` is not a `DripParticle` at all, which is why
    /// this cannot be a [`Self::Drip`] with a third phase.
    Splash {
        /// Where.
        pos: [f64; 3],
    },
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
                | Self::HugeExplosion
                | Self::Dust
                | Self::Animated { .. }
                | Self::Cloud
                | Self::SquidInk
                | Self::DustColorTransition { .. }
                | Self::DragonBreath { .. },
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
            Self::SimpleAnimated { .. }
            | Self::Spell
            | Self::CampfireSmoke
            | Self::Cloud
            | Self::SquidInk => Layer::Translucent,
            Self::Animated { layer } => layer,
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
            | Self::HugeExplosion
            | Self::Portal
            | Self::Dust
            // `LifetimeAlpha.ALWAYS_OPAQUE` for the two types wired to it
            // (`enchant`, `nautilus`); the translucent-alpha variant of
            // `FlyTowardsPositionParticle` is `vault_connection`, which has no
            // emitter here yet.
            | Self::FlyTowardsPosition
            // `ExplodeParticle.getLayer` is `OPAQUE`.
            // `LavaParticle.getLayer` is `OPAQUE`.
            | Self::Lava
            // `DripParticle.getLayer` is `OPAQUE`.
            | Self::Drip { .. }
            // `DragonBreathParticle.getLayer` is `OPAQUE` too -- it is a
            // dense cloud, not a translucent mote.
            | Self::DragonBreath { .. }
            | Self::DustColorTransition { .. } => Layer::Opaque,
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
    /// The position the particle was emitted at, for the behaviours whose
    /// position is a closed-form function of age rather than an integration of
    /// velocity — [`Behaviour::Portal`]'s `xStart/yStart/zStart`.
    ///
    /// Seeded to the spawn position by the constructors and then left alone;
    /// `xo/yo/zo` cannot serve because those are rewritten every tick for render
    /// interpolation.
    pub spawn: [f64; 3],
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
            spawn: [x, y, z],
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
            // expression too, and so does `DustParticleBase`.
            Behaviour::Crit
            | Behaviour::AshSmoke
            | Behaviour::Note
            | Behaviour::Heart
            | Behaviour::Dust
            | Behaviour::Cloud
            | Behaviour::DragonBreath { .. }
            | Behaviour::DustColorTransition { .. } => {
                self.quad_size * (normalised() * 32.0).clamp(0.0, 1.0)
            }
            // `LavaParticle.getQuadSize`: `quadSize * (1 - s * s)`. Note the
            // missing `* 0.5F` that `Behaviour::Flame` has — a lava pop shrinks
            // to nothing over its life where a flame only halves.
            Behaviour::Lava => {
                let s = normalised();
                self.quad_size * s.mul_add(-s, 1.0)
            }
            // `quadSize * (1 - s * s * 0.5)`.
            Behaviour::Flame => {
                let s = normalised();
                self.quad_size * s.mul_add(-s * 0.5, 1.0)
            }
            // `PortalParticle.getQuadSize`: `s = 1 - age/lifetime; s *= s;
            // s = 1 - s` — an ease-*out* growth, so a portal mote appears small,
            // swells almost immediately and then holds. Reading the two `1 - s`
            // steps as cancelling gives a linear ramp and a visibly duller portal.
            Behaviour::Portal => {
                let s = 1.0 - normalised();
                self.quad_size * s.mul_add(-s, 1.0)
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
    pub fn tick(&mut self, view: &dyn CollisionView) -> Vec<Spawn> {
        match self.behaviour {
            Behaviour::Drip { kind, phase } => self.tick_drip(view, kind, phase),
            Behaviour::Portal => {
                self.tick_portal();
                Vec::new()
            }
            Behaviour::FlyTowardsPosition => {
                self.tick_fly_towards_position();
                Vec::new()
            }
            Behaviour::CampfireSmoke => {
                self.tick_campfire_smoke(view);
                Vec::new()
            }
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
            Behaviour::DragonBreath { hit_ground } => {
                self.tick_dragon_breath(view, hit_ground);
                Vec::new()
            }
            Behaviour::Lava => {
                self.tick_base(view);
                if self.removed {
                    return Vec::new();
                }
                // `if (random.nextFloat() > (float) age / lifetime)` — the odds
                // of trailing smoke fall linearly to zero over the pop's life.
                #[expect(clippy::cast_precision_loss, reason = "Java computes this in f32")]
                let odds = self.age as f32 / self.lifetime as f32;
                if self.rng_probe() > odds {
                    return vec![Spawn::Smoke {
                        pos: [self.x, self.y, self.z],
                        vel: [self.xd, self.yd, self.zd],
                    }];
                }
                Vec::new()
            }
            Behaviour::SquidInk => {
                self.tick_base(view);
                self.tick_overrides();
                if !self.removed {
                    // `if (level.getBlockState(...).isAir()) yd -= 0.0074F` —
                    // approximated as "not in water", which is the distinction
                    // the sink exists to make and the one this view can answer.
                    let (bx, by, bz) = block_containing(self.x, self.y, self.z);
                    if !view.is_water(bx, by, bz) {
                        self.yd -= f64::from(0.0074_f32);
                    }
                }
                Vec::new()
            }
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
            Behaviour::AshSmoke
            | Behaviour::Spell
            | Behaviour::HugeExplosion
            | Behaviour::Dust
            | Behaviour::Animated { .. }
            | Behaviour::Cloud => {
                self.set_sprite_from_age();
            }
            // `DustColorTransitionParticle` additionally lerps its colour —
            // vanilla does this every *frame* from `extract`'s partial tick
            // (`lerpColors`); this port advances it once per tick instead, at
            // the same granularity `Behaviour::Crit`'s desaturation already
            // uses. `age` runs 1..=lifetime here (checked and incremented at
            // the top of `tick_base` before this runs), matching vanilla's
            // `(age + partialTickTime) / (lifetime + 1.0F)` closely enough
            // that the two ends of the transition still land on `from`/`to`.
            Behaviour::DustColorTransition { from, to } => {
                self.set_sprite_from_age();
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "tick counts are small; mirrors Java's int-to-float promotion"
                )]
                let a = self.age as f32 / (self.lifetime as f32 + 1.0);
                for i in 0..3 {
                    self.colour[i] = from[i] + (to[i] - from[i]) * a;
                }
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

    /// `DragonBreathParticle.tick()` — a full override that calls neither
    /// `super.tick()` nor any gravity term.
    ///
    /// ```java
    /// this.xo = this.x; this.yo = this.y; this.zo = this.z;
    /// if (this.age++ >= this.lifetime) { this.remove(); }
    /// else {
    ///    this.setSpriteFromAge(this.sprites);
    ///    if (this.onGround) { this.yd = 0.0; this.hasHitGround = true; }
    ///    if (this.hasHitGround) { this.yd += 0.002; }
    ///    this.move(this.xd, this.yd, this.zd);
    ///    if (this.y == this.yo) { this.xd *= 1.1; this.zd *= 1.1; }
    ///    this.xd = this.xd * this.friction;
    ///    this.zd = this.zd * this.friction;
    ///    if (this.hasHitGround) { this.yd = this.yd * this.friction; }
    /// }
    /// ```
    ///
    /// Two things a reader should not "fix". The horizontal `* 1.1` fires on
    /// `y == yo` — an exact comparison against the *previous* position, so it
    /// fires whenever the cloud's height did not change at all this tick, which
    /// for a `hasPhysics = false` particle with no vertical velocity is every
    /// tick. That is the outward creep, not a bug. And `yd` is damped **only**
    /// once the cloud has hit ground: an airborne one keeps whatever vertical
    /// speed the packet gave it, undamped, forever.
    fn tick_dragon_breath(&mut self, view: &dyn CollisionView, hit_ground: bool) {
        self.xo = self.x;
        self.yo = self.y;
        self.zo = self.z;
        // `age++ >= lifetime` compares the pre-increment value, so this is the
        // same test `tick_base` spells as `age += 1; age > lifetime`.
        self.age += 1;
        if self.age > self.lifetime {
            self.remove();
            return;
        }
        self.set_sprite_from_age();
        let mut hit_ground = hit_ground;
        if self.on_ground {
            self.yd = 0.0;
            hit_ground = true;
        }
        if hit_ground {
            self.yd += 0.002;
        }
        self.move_by(self.xd, self.yd, self.zd, view);
        if (self.y - self.yo).abs() < f64::EPSILON {
            self.xd *= 1.1;
            self.zd *= 1.1;
        }
        let friction = f64::from(self.friction);
        self.xd *= friction;
        self.zd *= friction;
        if hit_ground {
            self.yd *= friction;
        }
        self.behaviour = Behaviour::DragonBreath { hit_ground };
    }

    /// `PortalParticle.tick()` — a full override that **recomputes** the
    /// position from [`Self::spawn`] rather than integrating velocity.
    ///
    /// ```java
    /// float pos = (float)this.age / this.lifetime;
    /// float a = pos;
    /// pos = -pos + pos * pos * 2.0F;
    /// pos = 1.0F - pos;
    /// this.x = this.xStart + this.xd * pos;
    /// this.y = this.yStart + this.yd * pos + (1.0F - a);
    /// this.z = this.zStart + this.zd * pos;
    /// ```
    ///
    /// `xd/yd/zd` are therefore an **amplitude**, not a speed, and are never
    /// damped — which is why neither `gravity` nor `friction` is read here. The
    /// `(1 - a)` on `y` alone is what makes the mote sink toward the portal's
    /// plane as it converges instead of collapsing straight to a point.
    fn tick_portal(&mut self) {
        self.xo = self.x;
        self.yo = self.y;
        self.zo = self.z;
        self.age += 1;
        if self.age >= self.lifetime {
            self.remove();
            return;
        }
        #[expect(clippy::cast_precision_loss, reason = "Java computes this in f32")]
        let a = self.age as f32 / self.lifetime as f32;
        let pos = f64::from(1.0 - (-a + a * a * 2.0));
        let [sx, sy, sz] = self.spawn;
        self.x = sx + self.xd * pos;
        self.y = sy + self.yd * pos + f64::from(1.0 - a);
        self.z = sz + self.zd * pos;
        self.bb = crate::Aabb::new(
            self.x - f64::from(self.bb_width) / 2.0,
            self.y,
            self.z - f64::from(self.bb_width) / 2.0,
            self.x + f64::from(self.bb_width) / 2.0,
            self.y + f64::from(self.bb_height),
            self.z + f64::from(self.bb_width) / 2.0,
        );
    }

    /// `FlyTowardsPositionParticle.tick()` — a full override, like
    /// [`Self::tick_portal`]: no `super.tick()`, no `move()`, and the position
    /// recomputed from [`Self::spawn`] every tick.
    ///
    /// Java:
    ///
    /// ```text
    /// if (age++ >= lifetime) { remove(); return; }
    /// pos = 1.0F - (float) age / lifetime;
    /// pp = 1.0F - pos; pp *= pp; pp *= pp;      // (1 - pos)^4
    /// x = xStart + xd * pos;
    /// y = yStart + yd * pos - pp * 1.2F;
    /// z = zStart + zd * pos;
    /// ```
    ///
    /// Two things a literal reading gets wrong. `pos` runs from **1 down to 0**,
    /// so the mote starts at the full offset and converges on the spawn point —
    /// which is why the emitter's `xd/yd/zd` are an offset rather than a
    /// velocity. And `pp` is `pos` complemented *and then squared twice*: a
    /// quartic, dipping the path 1.2 blocks below the straight line at the very
    /// end of the flight. Both `1 - x` steps read as cancelling and do not.
    ///
    /// The removal test is `>=` on the **pre**-increment value, so a particle
    /// with `lifetime` ticks lives through `age == lifetime - 1` and is removed
    /// the tick `age` reaches `lifetime`.
    fn tick_fly_towards_position(&mut self) {
        self.xo = self.x;
        self.yo = self.y;
        self.zo = self.z;
        let expired = self.age >= self.lifetime;
        self.age += 1;
        if expired {
            self.remove();
            return;
        }
        #[expect(clippy::cast_precision_loss, reason = "Java computes this in f32")]
        let pos = 1.0 - (self.age as f32 / self.lifetime as f32);
        let sag = {
            let mut pp = 1.0 - pos;
            pp *= pp;
            pp *= pp;
            pp
        };
        let pos = f64::from(pos);
        let [sx, sy, sz] = self.spawn;
        self.set_pos(
            sx + self.xd * pos,
            self.yd.mul_add(pos, sy) - f64::from(sag * 1.2),
            sz + self.zd * pos,
        );
    }

    /// `CampfireSmokeParticle.tick()` — a full override.
    ///
    /// Three things it does *not* do, each visible if copied from the base tick:
    /// no friction (the column keeps its drift instead of stalling), gravity
    /// applied straight to `yd` at `3.0e-6` rather than through the `0.04`
    /// coefficient, and the alpha fade keyed to the **last 60 ticks** of life
    /// rather than the back half. A signal fire lives ~300 ticks, so a
    /// back-half fade would make it transparent from halfway up the column.
    fn tick_campfire_smoke(&mut self, view: &dyn CollisionView) {
        self.xo = self.x;
        self.yo = self.y;
        self.zo = self.z;
        self.age += 1;
        if self.age > self.lifetime || self.alpha <= 0.0 {
            self.remove();
            return;
        }
        // `random.nextFloat() / 5000 * (nextBoolean() ? 1 : -1)` on both
        // horizontal axes: the wander that keeps a column from being a line.
        let nudge = |p: &mut Self| {
            let magnitude = f64::from(p.rng_probe()) / 5000.0;
            if p.tick_rng().next_bool() { magnitude } else { -magnitude }
        };
        let dx = nudge(self);
        let dz = nudge(self);
        self.xd += dx;
        self.zd += dz;
        self.yd -= f64::from(self.gravity);
        self.move_by(self.xd, self.yd, self.zd, view);
        const FADE_TICKS: i32 = 60;
        if self.age >= self.lifetime - FADE_TICKS && self.alpha > 0.01 {
            self.alpha -= 0.015;
        }
    }

    /// `DripParticle.tick()` — a full override, and the one tick in this crate
    /// that **continues into another particle**.
    ///
    /// The shape is `preMoveUpdate → gravity → move → postMoveUpdate → damp →
    /// fluid check`, with the two hooks being the entire difference between
    /// vanilla's four subclasses:
    ///
    /// | phase | `preMoveUpdate` | `postMoveUpdate` |
    /// |---|---|---|
    /// | [`DripPhase::Hang`] | expire → remove **and spawn the falling phase** | damp velocity to a fiftieth |
    /// | [`DripPhase::Fall`] | expire → remove | on ground → remove, and spawn the landing phase if the kind has one |
    /// | [`DripPhase::Land`] | expire → remove | — |
    ///
    /// Three details a literal reading loses:
    ///
    /// * **`lifetime--` is a post-decrement tested against zero**, so a drip
    ///   lives `lifetime + 1` ticks and `lifetime` itself counts *down*. The
    ///   cooling formula below reads that counter directly, so incrementing an
    ///   `age` instead silently inverts the colour ramp.
    /// * **The gravity term is `yd -= gravity`, not `yd -= 0.04 * gravity`.**
    ///   `DripParticle` applies it raw rather than through the base tick's `0.04`
    ///   scale, so a drip falls twenty-five times harder than the same `gravity`
    ///   number means anywhere else in this file. That is why the hanging phase's
    ///   value looks absurdly small (`0.0012`, or `1.2e-5` for honey).
    /// * **Lava's hanging phase cools.** `CoolingDripHangParticle` recomputes
    ///   `g = 16 / (40 - lifetime + 16)` and `b = 4 / (40 - lifetime + 8)` every
    ///   tick, so a lava drip starts white-hot (`1, 1, 0.5`) and arrives at
    ///   exactly the lava tint (`1, 0.2857, 0.0833`) as its 40 ticks run out.
    ///   The two constants are not interchangeable and neither is derivable from
    ///   the other.
    fn tick_drip(
        &mut self,
        view: &dyn CollisionView,
        kind: DripKind,
        phase: DripPhase,
    ) -> Vec<Spawn> {
        self.xo = self.x;
        self.yo = self.y;
        self.zo = self.z;
        let mut spawns = Vec::new();

        // `CoolingDripHangParticle.preMoveUpdate`, before the shared body.
        if phase == DripPhase::Hang && matches!(kind, DripKind::Lava | DripKind::DripstoneLava) {
            #[expect(clippy::cast_precision_loss, reason = "lifetime counts down from 40")]
            let elapsed = (40 - self.lifetime) as f32;
            self.colour = [1.0, 16.0 / (elapsed + 16.0), 4.0 / (elapsed + 8.0)];
        }

        let expired = self.lifetime <= 0;
        self.lifetime -= 1;
        if expired {
            self.remove();
            if phase == DripPhase::Hang {
                spawns.push(Spawn::Drip {
                    kind,
                    phase: DripPhase::Fall,
                    pos: [self.x, self.y, self.z],
                    vel: [self.xd, self.yd, self.zd],
                });
            }
            return spawns;
        }

        self.yd -= f64::from(self.gravity);
        self.move_by(self.xd, self.yd, self.zd, view);

        match phase {
            DripPhase::Hang => {
                let damp = 0.02;
                self.xd *= damp;
                self.yd *= damp;
                self.zd *= damp;
            }
            DripPhase::Fall if self.on_ground => {
                self.remove();
                let pos = [self.x, self.y, self.z];
                match kind {
                    // `FallAndLandParticle` with `ParticleTypes.SPLASH` — not a
                    // drip phase at all.
                    DripKind::Water | DripKind::DripstoneWater => {
                        spawns.push(Spawn::Splash { pos });
                    }
                    // `DripstoneLava` borrows plain lava's landing type.
                    DripKind::Lava | DripKind::DripstoneLava => spawns.push(Spawn::Drip {
                        kind: DripKind::Lava,
                        phase: DripPhase::Land,
                        pos,
                        vel: [0.0; 3],
                    }),
                    DripKind::Honey | DripKind::ObsidianTear => spawns.push(Spawn::Drip {
                        kind,
                        phase: DripPhase::Land,
                        pos,
                        vel: [0.0; 3],
                    }),
                    // `FallingParticle`, not `FallAndLandParticle`: these two
                    // land as nothing at all.
                    DripKind::Nectar | DripKind::SporeBlossom => {}
                }
                return spawns;
            }
            DripPhase::Fall | DripPhase::Land => {}
        }

        let drag = f64::from(0.98_f32);
        self.xd *= drag;
        self.yd *= drag;
        self.zd *= drag;

        // `if (this.type != Fluids.EMPTY)` — a drip dies inside **its own**
        // fluid, so a water drip vanishes on hitting water and a lava drip does
        // not. The honey/nectar/obsidian/spore kinds carry `Fluids.EMPTY` and
        // therefore skip this entirely.
        let (bx, by, bz) = block_containing(self.x, self.y, self.z);
        let in_own_fluid = match kind {
            DripKind::Water | DripKind::DripstoneWater => view.is_water(bx, by, bz),
            DripKind::Lava | DripKind::DripstoneLava => view.is_lava(bx, by, bz),
            DripKind::Honey
            | DripKind::Nectar
            | DripKind::ObsidianTear
            | DripKind::SporeBlossom => false,
        };
        if in_own_fluid && self.y < f64::from(by) + fluid_height(view, bx, by, bz) {
            self.remove();
        }
        spawns
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
    fn tick_huge_explosion_seed(&mut self) -> Vec<Spawn> {
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
            spawns.push(Spawn::HugeExplosion { pos: [xx, yy, zz], size });
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
        let mut spawns: Vec<Spawn> = Vec::new();
        for p in &mut self.particles {
            spawns.extend(p.tick(view));
        }
        self.particles.retain(Particle::is_alive);
        for spawn in spawns {
            match spawn {
                Spawn::HugeExplosion { pos: [x, y, z], size } => {
                    emit::huge_explosion(self, x, y, z, size);
                }
                Spawn::Drip { kind, phase, pos, vel } => emit::drip(self, kind, phase, pos, vel),
                Spawn::Splash { pos: [x, y, z] } => emit::splash(self, x, y, z, 0.0, 0.0, 0.0),
                Spawn::Smoke { pos: [x, y, z], vel: [xd, yd, zd] } => {
                    emit::smoke(self, x, y, z, xd, yd, zd, 1.0);
                }
            }
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
                //
                // **This list is exhaustive for what this crate models, and it
                // is short by design.** Exactly five vanilla classes return the
                // bare constant (`grep -rl 'return 15728880'`): those three plus
                // `GustParticle` and `TrailParticle`, neither of which has a
                // `Behaviour` yet. Everything else that *looks* self-lit in game
                // still samples the world and then boosts the block half —
                // `LightCoordsUtil.withBlock(super…, 15)` (lava, shriek, sculk
                // charge, vibration, glowing drips and souls) or
                // `addSmoothBlockEmission(super…, t)` (`FlameParticle`,
                // `GlowParticle`, `PortalParticle`). Neither boost is modelled
                // here, so `Flame` comes out sampled — dimmer than vanilla in
                // the dark, never brighter. Adding a new behaviour: read
                // `getLightCoords` in the jar rather than guessing from how the
                // particle looks, and add an arm here only for a bare
                // `15728880`. `FireflyParticle` is the trap — it overrides
                // `getLightCoords` to return a *fade fraction* scaled by 255,
                // which is not a packed light value at all.
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

    /// The frame order of the sheets whose `particles/*.json` lists them
    /// **descending** — smoke, spell, effect, glitter.
    ///
    /// This was a shipped bug: `texture_name` synthesised an ascending
    /// `<stem>_<n>`, so frame 0 resolved to `generic_0` where vanilla's frame 0 is
    /// `generic_7`. Every smoke plume, potion mote, witch mote, end-rod sparkle and
    /// totem sparkle therefore animated **backwards**, and nothing caught it
    /// because a sprite lookup still resolved. The expected values here come from
    /// the 26.2 jar's own JSON, not from our own tables.
    #[test]
    fn descending_sheets_start_at_their_last_numbered_frame() {
        assert_eq!(Sheet::Generic.texture_name(0), "particle/generic_7");
        assert_eq!(Sheet::Generic.texture_name(7), "particle/generic_0");
        assert_eq!(Sheet::Spell.texture_name(0), "particle/spell_7");
        assert_eq!(Sheet::Effect.texture_name(0), "particle/effect_7");
        assert_eq!(Sheet::Glitter.texture_name(0), "particle/glitter_7");
        // `portal.json` lists the *same eight textures* ascending, which is why it
        // is a separate variant rather than a flag on `Generic`.
        assert_eq!(Sheet::PortalGeneric.texture_name(0), "particle/generic_0");
        assert_eq!(Sheet::PortalGeneric.texture_name(7), "particle/generic_7");
    }

    /// `enchant.json`'s frames are letters, which is the case no `<stem>_<n>`
    /// format string can express at all — and the reason `frames()` is a list.
    #[test]
    fn every_sheet_has_frames_and_enchant_is_alphabetic() {
        for sheet in Sheet::all() {
            let frames = sheet.frames();
            assert!(!frames.is_empty(), "{sheet:?} has no frames");
            assert_eq!(usize::from(sheet.frame_count()), frames.len());
            // The last frame must be reachable and clamping must not panic.
            assert_eq!(
                sheet.texture_name(sheet.frame_count() - 1),
                format!("particle/{}", frames[frames.len() - 1])
            );
            assert_eq!(sheet.texture_name(u16::MAX), sheet.texture_name(sheet.frame_count() - 1));
        }
        assert_eq!(Sheet::Enchant.frame_count(), 26);
        assert_eq!(Sheet::Enchant.texture_name(0), "particle/sga_a");
        assert_eq!(Sheet::Enchant.texture_name(25), "particle/sga_z");
    }

    /// `PortalParticle.tick` recomputes position from the spawn point, so a mote
    /// **converges back onto its origin** as it ages rather than drifting off on
    /// its velocity. Integrating instead sends portal motes flying away.
    #[test]
    fn a_portal_mote_converges_on_its_spawn_point() {
        let mut engine = ParticleEngine::new();
        crate::emit::portal(&mut engine, 10.0, 64.0, -3.0, 0.25, 0.0, -0.25);
        let floor = Floor { floor_y: 0, water_above: false };
        let start_offset = {
            let p = &engine.particles()[0];
            (p.x - p.spawn[0]).abs() + (p.z - p.spawn[2]).abs()
        };
        // At age 0 the easing term is 1.0, so the mote sits a full amplitude away.
        assert!((start_offset - 0.0).abs() < 1.0e-9, "age 0 has not been ticked yet");
        let lifetime = engine.particles()[0].lifetime;
        let spawn = engine.particles()[0].spawn;
        // The easing, recomputed here from the Java expression rather than read
        // out of the implementation — that is the whole point of predicting it.
        let easing = |age: i32| {
            #[expect(clippy::cast_precision_loss, reason = "small tick counts, as Java")]
            let a = age as f32 / lifetime as f32;
            f64::from(1.0 - (-a + a * a * 2.0))
        };
        let mut horizontal = Vec::new();
        for age in 1..lifetime {
            engine.tick(&floor);
            let p = &engine.particles()[0];
            // Exact, not a tolerance on a trend: position is a closed form, so a
            // wrong easing lands somewhere else entirely.
            assert!(
                (p.x - (spawn[0] + 0.25 * easing(age))).abs() < 1.0e-9,
                "age {age}: x was {}",
                p.x
            );
            assert!((p.z - (spawn[2] - 0.25 * easing(age))).abs() < 1.0e-9);
            horizontal.push((p.x - spawn[0]).abs() + (p.z - spawn[2]).abs());
        }
        // And it really does converge rather than fly off, which is the visible
        // half: the easing is 1.0 at birth and ~0 at death.
        let first = horizontal[0];
        let last = *horizontal.last().expect("ticked at least once");
        assert!(
            last < first * 0.1,
            "a portal mote must converge: started {first} away, ended {last}"
        );
    }

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
        // **This line used to read `generic_3`, and that was the bug.** `frame`
        // is an index into the sheet's own frame list, and `smoke.json` lists
        // `generic_7` first — so index 3 is `generic_4`, counting down. The
        // ascending reading is what animated every smoke plume backwards; see
        // `descending_sheets_start_at_their_last_numbered_frame`.
        assert_eq!(Sheet::Generic.texture_name(3), "particle/generic_4");
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
