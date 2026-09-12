use super::*;


    /// The counts here come from the **formula in vanilla's own "add destroy block effect" step**
    /// (`max(2, ceil(width / 0.25))` per axis), not from running this code: a
    /// full cube is `4 × 4 × 4` and a bottom slab is `4 × 2 × 4`.
    #[test]
    fn a_full_cube_throws_sixty_four_fragments() {
        let mut engine = ParticleEngine::seeded(1);
        destroy_block_effect(&mut engine, (0, 64, 0), state(1), WHITE, &[FULL_CUBE]);
        assert_eq!(engine.len(), 64);
    }

    #[test]
    fn a_thinner_block_throws_proportionally_less_debris() {
        let slab = Aabb::new(0.0, 0.0, 0.0, 1.0, 0.5, 1.0);
        let mut engine = ParticleEngine::seeded(1);
        destroy_block_effect(&mut engine, (0, 64, 0), state(1), WHITE, &[slab]);
        assert_eq!(engine.len(), 32, "a half-height slab should emit 4 x 2 x 4");
    }

    /// The `max(2, ...)` floor: a shape thinner than the density step must still
    /// emit two samples on that axis, not zero. A carpet that emitted nothing
    /// would look like broken particle code rather than a thin block.
    #[test]
    fn a_very_thin_shape_still_emits_two_samples_on_that_axis() {
        let carpet = Aabb::new(0.0, 0.0, 0.0, 1.0, 0.0625, 1.0);
        let mut engine = ParticleEngine::seeded(1);
        destroy_block_effect(&mut engine, (0, 64, 0), state(1), WHITE, &[carpet]);
        assert_eq!(engine.len(), 32, "expected 4 x 2 x 4 from the minimum floor");
    }

    #[test]
    fn an_empty_shape_emits_nothing() {
        let mut engine = ParticleEngine::seeded(1);
        destroy_block_effect(&mut engine, (0, 64, 0), state(1), WHITE, &[]);
        assert!(engine.is_empty());
    }

    #[test]
    fn multi_box_shapes_emit_from_every_box() {
        let lower = Aabb::new(0.0, 0.0, 0.0, 1.0, 0.5, 1.0);
        let upper = Aabb::new(0.0, 0.5, 0.0, 1.0, 1.0, 1.0);
        let mut engine = ParticleEngine::seeded(1);
        destroy_block_effect(&mut engine, (0, 64, 0), state(1), WHITE, &[lower, upper]);
        assert_eq!(engine.len(), 64, "two half-boxes should match one full cube");
    }

    #[test]
    fn every_fragment_lands_inside_the_block_it_came_from() {
        let mut engine = ParticleEngine::seeded(2);
        destroy_block_effect(&mut engine, (3, 64, -7), state(1), WHITE, &[FULL_CUBE]);
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

    /// Item crumbs retain a generated item identity all the way into the
    /// particle. The distinct-item control rules out an implementation that
    /// silently substitutes one generic crumb for every caller.
    #[test]
    fn item_crumb_retains_its_validated_item() {
        let mut rng = ParticleEngine::seeded(4);
        let carrot = super::item_particle(
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            Item::Carrot,
            rng.rng(),
        );
        let beetroot = super::item_burst_particle(0.0, 0.0, 0.0, Item::Beetroot, rng.rng());
        assert_eq!(carrot.sprite, SpriteSource::Item(Item::Carrot));
        assert_eq!(beetroot.sprite, SpriteSource::Item(Item::Beetroot));
        assert_ne!(
            carrot.sprite, beetroot.sprite,
            "distinct foods must not share a generic sprite"
        );
    }

    /// Terrain particles carry the block's identity, which is the whole point —
    /// breaking oak must throw wood-coloured chips, not generic grey ones.
    #[test]
    fn fragments_carry_the_block_state_and_the_vanilla_grey() {
        let mut engine = ParticleEngine::seeded(3);
        destroy_block_effect(&mut engine, (0, 64, 0), state(42), WHITE, &[FULL_CUBE]);
        let p = &engine.particles()[0];
        assert_eq!(p.sprite, SpriteSource::BlockState(state(42)));
        assert!(matches!(p.behaviour, Behaviour::Terrain { .. }));
        for c in p.colour {
            assert!((c - 0.6).abs() < 1e-6, "expected 0.6 grey, got {c}");
        }
        assert!(p.gravity > 0.99, "terrain fragments must fall");
    }

    #[test]
    fn a_biome_tint_multiplies_into_the_fragment_colour() {
        let mut engine = ParticleEngine::seeded(3);
        destroy_block_effect(
            &mut engine,
            (0, 64, 0),
            state(42),
            [0.5, 1.0, 0.25],
            &[FULL_CUBE],
        );
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
            breaking_block_effect(
                &mut engine,
                (0, 64, 0),
                state(1),
                WHITE,
                face,
                FULL_CUBE,
            );
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
        breaking_block_effect(
            &mut chip_engine,
            (0, 64, 0),
            state(1),
            WHITE,
            Face::Up,
            FULL_CUBE,
        );
        let chip = &chip_engine.particles()[0];

        let mut burst_engine = ParticleEngine::seeded(5);
        destroy_block_effect(&mut burst_engine, (0, 64, 0), state(1), WHITE, &[FULL_CUBE]);
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

