use super::*;

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
