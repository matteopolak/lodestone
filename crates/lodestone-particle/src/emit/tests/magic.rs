use super::*;

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
