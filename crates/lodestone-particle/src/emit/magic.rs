use super::*;

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
