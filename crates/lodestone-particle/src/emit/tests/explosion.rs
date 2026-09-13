use super::*;

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
