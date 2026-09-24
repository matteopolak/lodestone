use super::*;

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
            destroy_block_effect(&mut e, (0, 64, 0), state(1), WHITE, &[FULL_CUBE]);
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
