use super::*;
use crate::emit::{
    ash, campfire_smoke, fly_towards_position, spore_blossom_air, white_smoke,
};

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
            "the random per-particle sprite-set pick should vary the plume, got frames {frames:?}"
        );
    }
