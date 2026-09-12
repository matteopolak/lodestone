#[cfg(test)]
mod tests {
    use super::*;

    /// All seven 26.2 spear items resolve to the sprint-preserving override;
    /// every other id — including a non-spear weapon and an empty-hand
    /// sentinel — resolves to `DEFAULT`. Pairwise-distinct items on each side
    /// so a producer that always returns one constant cannot pass this.
    #[test]
    fn for_item_picks_out_exactly_the_seven_spears() {
        for spear in [
            "minecraft:wooden_spear",
            "minecraft:stone_spear",
            "minecraft:copper_spear",
            "minecraft:iron_spear",
            "minecraft:golden_spear",
            "minecraft:diamond_spear",
            "minecraft:netherite_spear",
        ] {
            assert_eq!(
                UseEffects::for_item(spear),
                UseEffects::SPEAR,
                "{spear} must resolve to UseEffects::SPEAR"
            );
        }
        for other in ["minecraft:bow", "minecraft:apple", "minecraft:shield"] {
            assert_eq!(
                UseEffects::for_item(other),
                UseEffects::DEFAULT,
                "{other} must resolve to UseEffects::DEFAULT"
            );
        }
        // Namespace-agnostic — see the method's own doc for why.
        assert_eq!(
            UseEffects::for_item("mymodpack:crystal_spear"),
            UseEffects::SPEAR
        );
    }

    #[test]
    fn player_speed_walk_and_sprint_bits() {
        let p = PhysicsProfile::mc_1_21();
        assert_eq!(player_speed(&p, false), 0.1f32);
        // Sprint speed derived via the attribute math: 0.13000001f (0x3e051eb9).
        assert_eq!(player_speed(&p, true).to_bits(), 0x3e05_1eb9);
    }

    #[test]
    fn friction_influenced_speed_default_ground_is_getspeed() {
        // On default 0.6 friction, the 0.216.../f^3 factor is exactly 1.0f.
        let p = PhysicsProfile::mc_1_21();
        let mut s = PlayerState::at(Vec3d::new(0.0, 0.0, 0.0), 0.0);
        s.on_ground = true;
        let bf = mth::compute_modified_friction(0.6, 1.0);
        assert_eq!(friction_influenced_speed(&p, &s, bf), 0.1f32);
    }

    #[test]
    fn injected_attribute_speed_replaces_not_stacks_with_sprint() {
        // Reconciled attribute seam. The entity layer's MOVEMENT_SPEED value
        // already folds sprint in, so a Some(v) override must be used verbatim
        // (as f32) even while `sprinting` is true — never re-multiplied here.
        let p = PhysicsProfile::mc_1_21();
        let bf = mth::compute_modified_friction(0.6, 1.0);

        // base 0.1 + sprint (AddMultipliedTotal 0.3) + Speed I (AddMultipliedTotal
        // 0.2), all one class => 0.1 * (1+0.3) * (1+0.2), per calculateValue().
        let attr = 0.1_f64 * (1.0 + 0.3) * (1.0 + 0.2);
        let mut s = PlayerState::at(Vec3d::new(0.0, 0.0, 0.0), 0.0).with_movement_speed(attr);
        s.on_ground = true;
        s.sprinting = true; // must be ignored while the override is present

        assert_eq!(friction_influenced_speed(&p, &s, bf), attr as f32);
        // Guard against the folding failure: it is NOT the sprint-stacked value.
        assert_ne!(friction_influenced_speed(&p, &s, bf), (attr * 1.3) as f32);
    }

    #[test]
    fn no_override_falls_back_to_standalone_sprint() {
        let p = PhysicsProfile::mc_1_21();
        let bf = mth::compute_modified_friction(0.6, 1.0);
        let mut s = PlayerState::at(Vec3d::new(0.0, 0.0, 0.0), 0.0);
        s.on_ground = true;
        s.sprinting = true;
        assert_eq!(
            friction_influenced_speed(&p, &s, bf).to_bits(),
            player_speed(&p, true).to_bits()
        );
    }

    struct WaterEverywhere;
    impl CollisionView for WaterEverywhere {
        fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
            true
        }
    }

    struct LavaEverywhere;
    impl CollisionView for LavaEverywhere {
        fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        fn is_lava(&self, _x: i32, _y: i32, _z: i32) -> bool {
            true
        }
    }

    /// A hand-built world: explicit solid cells, explicit water cells with a
    /// real fluid amount, so the fluid **height** branches (jump threshold,
    /// hop-out) are exercised rather than the coarse full-cell fallback.
    #[derive(Default)]
    struct Pool {
        solid: std::collections::HashSet<(i32, i32, i32)>,
        water: std::collections::HashMap<(i32, i32, i32), u8>,
    }

    impl Pool {
        fn solid(&mut self, x: i32, y: i32, z: i32) -> &mut Self {
            self.solid.insert((x, y, z));
            self
        }
        fn water(&mut self, x: i32, y: i32, z: i32, amount: u8) -> &mut Self {
            self.water.insert((x, y, z), amount);
            self
        }
        fn floor(&mut self, y: i32) -> &mut Self {
            for x in -2..=2 {
                for z in -2..=2 {
                    self.solid.insert((x, y, z));
                }
            }
            self
        }
    }

    impl CollisionView for Pool {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
            if self.solid.contains(&(x, y, z)) {
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
        fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
            self.water.contains_key(&(x, y, z))
        }
        fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<crate::fluid::FluidCell> {
            self.water
                .get(&(x, y, z))
                .map(|&amount| crate::fluid::FluidCell {
                    kind: crate::fluid::FluidKind::Water,
                    amount,
                    falling: false,
                })
        }
        fn blocks_motion(&self, x: i32, y: i32, z: i32) -> bool {
            self.solid.contains(&(x, y, z))
        }
    }

    #[test]
    fn fluid_jump_threshold_boundary_is_the_swimming_eye_height() {
        // Vanilla's own fluid-jump threshold = `getEyeHeight() < 0.4 ? 0.0 : 0.4`.
        // The swimming pose's eye height is *exactly* 0.4, so it sits on
        // the false side of a strict `<` and
        // keeps the 0.4 threshold. Coding this as `<=` would collapse a swimmer's
        // threshold to zero and turn every swim-up into a standing jump.
        assert_eq!(fluid_jump_threshold(DEFAULT_EYE_HEIGHT), 0.4);
        assert_eq!(fluid_jump_threshold(0.4), 0.4);
        assert_eq!(fluid_jump_threshold(0.399), 0.0);
    }

    #[test]
    fn shallow_water_jumps_but_deep_water_swims_up() {
        // The sinking-versus-swimming decision (vanilla's own per-tick jump
        // block). Expected magnitudes come from
        // vanilla constants, not from this port:
        //   * shallow  -> jumpFromGround, JUMP_STRENGTH = 0.42F, then the water
        //     tick's own `* 0.8F` vertical drag and `- gravity/16` buoyancy step
        //     => 0.42*0.8 - 0.005 = 0.331
        //   * deep     -> jumpInLiquid, +0.04F  => 0.04*0.8 - 0.005 = 0.027
        // A single order of magnitude apart, so the branch cannot be mistaken.
        let p = PhysicsProfile::mc_1_21();

        // amount 3 => own height 3/9 = 0.333 < the 0.4 threshold: a puddle.
        let mut shallow = Pool::default();
        shallow.floor(0).water(0, 1, 0, 3);
        let mut s = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        s.on_ground = true;
        let jump = MovementInput {
            jump: true,
            ..MovementInput::NONE
        };
        tick(&mut s, jump, &shallow, &p);
        assert!(
            (s.velocity.y - (0.42 * 0.8 - 0.005)).abs() < 1.0e-6,
            "shallow water must produce a real jump, got vy = {}",
            s.velocity.y
        );

        // amount 8 => 8/9 = 0.888 > 0.4: deep enough to swim in.
        let mut deep = Pool::default();
        deep.floor(0).water(0, 1, 0, 8).water(0, 2, 0, 8);
        let mut s = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        s.on_ground = true;
        tick(&mut s, jump, &deep, &p);
        assert!(
            (s.velocity.y - (0.04 * 0.8 - 0.005)).abs() < 1.0e-6,
            "deep water must swim up, not jump, got vy = {}",
            s.velocity.y
        );
    }

    #[test]
    fn sneaking_sinks_and_not_sneaking_barely_does() {
        // Vanilla's own client per-tick update -> sneak-to-sink impulse:
        // shift while in water adds -0.04F. Expected values from vanilla constants:
        // the tick's vertical drag is 0.8F and buoyancy is -gravity/16 = -0.005, so
        //   no shift: 0.0  * 0.8 - 0.005 = -0.005
        //   shift:   -0.04 * 0.8 - 0.005 = -0.037
        // The pair is the point: without the sneak-to-sink impulse both read
        // -0.005 and the only way down is to release jump and wait.
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;

        let mut idle = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        tick(&mut idle, MovementInput::NONE, &view, &p);
        assert!(
            (idle.velocity.y - (-0.005)).abs() < 1.0e-8,
            "idle sink vy = {}",
            idle.velocity.y
        );

        let mut sinking = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        tick(
            &mut sinking,
            MovementInput {
                sneak: true,
                ..MovementInput::NONE
            },
            &view,
            &p,
        );
        assert!(
            (sinking.velocity.y - (-0.04 * 0.8 - 0.005)).abs() < 1.0e-8,
            "shift-sink vy = {}",
            sinking.velocity.y
        );
        assert!(
            sinking.position.y < idle.position.y,
            "shift must actually move the player down further"
        );
    }

    #[test]
    fn swimming_into_a_ledge_hops_out_of_the_water() {
        // Vanilla's own jump-out-of-fluid hop: a horizontal
        // collision plus a lifted box that is free of blocks *and* of liquid
        // replaces vertical velocity with a flat 0.3F. The expected value is that
        // literal, straight from the source.
        //
        // Geometry: a one-deep pool (water only at y = 1) with a shore block at
        // z = 1, and the player floating with its feet near the surface so the box
        // still overlaps the shore block. Swimming +Z (yaw 0 faces +Z) presses into
        // the shore.
        let p = PhysicsProfile::mc_1_21();
        let forward = MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        };

        let mut pool = Pool::default();
        pool.floor(0).water(0, 1, 0, 8).solid(0, 1, 1);
        let mut s = PlayerState::at(Vec3d::new(0.5, 1.9, 0.5), 0.0);
        let mut hopped = false;
        for _ in 0..60 {
            tick(&mut s, forward, &pool, &p);
            if (s.velocity.y - f64::from(0.3f32)).abs() < 1.0e-12 {
                hopped = true;
                break;
            }
        }
        assert!(hopped, "never hopped out; final state {s:?}");

        // Control: the identical pool with no shore block. The jump-out-of-
        // fluid step is gated on the horizontal-collision flag, so with
        // nothing to swim into the same detector must never fire — proving
        // the assertion above is not just "0.3 appears sometimes".
        let mut open = Pool::default();
        open.floor(0).water(0, 1, 0, 8);
        let mut s = PlayerState::at(Vec3d::new(0.5, 1.9, 0.5), 0.0);
        for _ in 0..60 {
            tick(&mut s, forward, &open, &p);
            assert!(
                (s.velocity.y - f64::from(0.3f32)).abs() > 1.0e-12,
                "open water must never hop: vy = {}",
                s.velocity.y
            );
        }
    }

    #[test]
    fn depth_strider_attribute_speeds_up_swimming() {
        // Vanilla's own in-water travel lerps both the horizontal slow-down
        // (toward 0.546_000_06) and the input speed (toward its own speed
        // accessor) by the resolved Depth Strider attribute value.
        // No caller can reach that attribute value yet (see the field docs on
        // `PlayerState::water_movement_efficiency` for exactly what is missing), so
        // this drives it directly: the *arithmetic* is what is under test, and the
        // direction it must move in (faster) is fixed by the source, not by this port.
        //
        // Note the halving when airborne (`if (!onGround()) waterWalker *= 0.5F`):
        // a swimmer is airborne, so a level-III boot (0.99) acts as 0.495.
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;
        let forward = MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        };

        let travel = |efficiency: f32| {
            let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0)
                .with_water_movement_efficiency(efficiency);
            for _ in 0..40 {
                tick(&mut s, forward, &view, &p);
            }
            s.position.z - 0.5
        };

        let bare = travel(0.0);
        let strider = travel(0.99);
        assert!(
            strider > bare * 1.5,
            "Depth Strider must materially speed up swimming: {strider} vs {bare}"
        );
    }

    #[test]
    fn lava_sink_converges_to_terminal() {
        // First-principles anchor (not the oracle): the deep-lava step is
        // `vy = 0.5*vy - baseGravity/4`, so terminal solves `0.5*vy = -0.02`,
        // i.e. vy = -0.04. Different from water's -0.025 — a different branch.
        let p = PhysicsProfile::mc_1_21();
        let view = LavaEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        for _ in 0..400 {
            tick(&mut s, MovementInput::NONE, &view, &p);
        }
        assert!(
            (s.velocity.y - (-0.04)).abs() < 1.0e-9,
            "terminal vy = {}",
            s.velocity.y
        );
    }

    #[test]
    fn levitation_makes_player_rise() {
        // First-principles anchor: Levitation replaces gravity with a pull toward
        // 0.05*(amp+1) > 0, so with no other input the player must gain height.
        struct Air;
        impl CollisionView for Air {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        }
        let p = PhysicsProfile::mc_1_21();
        let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0).with_effects(StatusEffects {
            levitation: Some(0),
            ..StatusEffects::default()
        });
        for _ in 0..40 {
            tick(&mut s, MovementInput::NONE, &Air, &p);
        }
        assert!(s.position.y > 100.5, "levitation y = {}", s.position.y);
        assert!(s.velocity.y > 0.0, "levitation vy = {}", s.velocity.y);
    }

    #[test]
    fn slow_falling_revives_the_dead_water_clamp() {
        // The satisfying test: at default gravity the -0.003 fluid clamp is dead
        // (proven by `fluid_clamp_is_dead_at_default_gravity`). Slow Falling drops
        // effective gravity to 0.01 while descending, moving baseGravity/16 off
        // 0.005 so the clamp becomes reachable. Confirm effective_gravity and that
        // the clamp fires at least once during a slow-falling submerged sink.
        assert_eq!(effective_gravity(0.08, true, true), 0.01);
        assert_eq!(effective_gravity(0.08, false, true), 0.08); // not falling: base
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0).with_effects(StatusEffects {
            slow_falling: true,
            ..StatusEffects::default()
        });
        let mut clamp_fired = false;
        for _ in 0..120 {
            tick(&mut s, MovementInput::NONE, &view, &p);
            if s.velocity.y == -0.003 {
                clamp_fired = true;
            }
        }
        assert!(clamp_fired, "slow-falling never revived the -0.003 clamp");
    }

    #[test]
    fn water_sink_converges_to_terminal() {
        // First-principles anchor (not the oracle): steady state solves
        // vy = 0.8*vy - baseGravity/16, i.e. vy = -0.005 / 0.2 = -0.025.
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        for _ in 0..400 {
            tick(&mut s, MovementInput::NONE, &view, &p);
        }
        assert!(
            (s.velocity.y - (-0.025)).abs() < 1.0e-6,
            "terminal vy = {}",
            s.velocity.y
        );
    }

    #[test]
    fn tick_sets_swimming_and_eye_in_water_when_sprinting_submerged() {
        // The real consumer of the eye-in-fluid state inside physics:
        // vanilla's own swimming-state update. A sprinting player fully
        // under water enters the
        // sprint-swimming pose, and the eye/box submersion flags are recorded on
        // the state for the shell (fog / overlay / ambient sound) to read.
        let p = PhysicsProfile::mc_1_21();
        let view = WaterEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        s.sprinting = true;
        tick(&mut s, MovementInput::NONE, &view, &p);
        assert!(s.eye_in_water, "eye is submerged");
        assert!(s.swimming, "sprinting + underwater => swimming pose");
    }

    #[test]
    fn tick_does_not_swim_when_sprinting_in_air() {
        // Negative control: sprinting with no water must not set the swim pose,
        // and must not spuriously report the eye in water.
        struct Air;
        impl CollisionView for Air {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
        }
        let p = PhysicsProfile::mc_1_21();
        let mut s = PlayerState::at(Vec3d::new(0.5, 100.0, 0.5), 0.0);
        s.sprinting = true;
        tick(&mut s, MovementInput::NONE, &Air, &p);
        assert!(!s.eye_in_water && !s.swimming);
    }

    #[test]
    fn swimming_is_sustained_while_sprinting_in_water_even_when_eye_surfaces() {
        // Once swimming, the pose persists on `sprinting && isInWater()` alone —
        // you keep swimming as you break the surface (eye leaves the water) until
        // you stop sprinting or leave the water. Uses a one-block-deep pool so the
        // box is in water but the eye (feet + 1.62) is above it.
        struct ShallowPool;
        impl CollisionView for ShallowPool {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
            fn is_water(&self, _x: i32, y: i32, _z: i32) -> bool {
                y == 94
            }
        }
        let p = PhysicsProfile::mc_1_21();
        let view = ShallowPool;
        let mut s = PlayerState::at(Vec3d::new(0.5, 94.0, 0.5), 0.0);
        s.sprinting = true;
        s.swimming = true; // already swimming from a prior submerged tick
        tick(&mut s, MovementInput::NONE, &view, &p);
        assert!(!s.eye_in_water, "eye is above the one-deep pool");
        assert!(s.swimming, "swim pose sustained by sprinting-in-water");
    }

    #[test]
    fn fluid_clamp_is_dead_at_default_gravity() {
        // With baseGravity/16 == 0.005, the two clamp conditions
        // (|y-0.005| >= 0.003 AND |y-0.005| < 0.003) are mutually exclusive, so
        // the -0.003 slow-sink never fires. Verify it degrades to y - 0.005.
        let g = f64::from(PhysicsProfile::mc_1_21().gravity);
        for &y in &[-0.01, -0.005, 0.0, 0.005, 0.02] {
            let out = fluid_falling_adjusted_movement(g, true, false, Vec3d::new(0.0, y, 0.0));
            assert_eq!(out.y, y - g / 16.0, "y = {y}");
        }
    }

    #[test]
    fn fluid_clamp_fires_under_reduced_gravity() {
        // Under slow-falling-style reduced gravity, baseGravity/16 != 0.005, so
        // the clamp can engage near terminal. Pick movement.y so both hold.
        let base = 0.01_f64; // baseGravity/16 = 0.000625
        let y = 0.001_f64; // |y-0.005|=0.004 >= 0.003, |y-0.000625|=0.000375 < 0.003
        let out = fluid_falling_adjusted_movement(base, true, false, Vec3d::new(0.0, y, 0.0));
        assert_eq!(out.y, -0.003);
    }

    #[test]
    fn profile_selects_structural_input_model_per_version() {
        // The 1.8-vs-modern difference is a *branch*, not a scalar: the profiles
        // must declare different `InputModel`s even though their numbers match.
        assert_eq!(
            PhysicsProfile::mc_1_21().input_model,
            InputModel::UnitSquareProjection
        );
        assert_eq!(
            PhysicsProfile::mc_1_8().input_model,
            InputModel::LegacyMoveFlying
        );
        assert_eq!(PhysicsProfile::mc_1_21().fluid_model, FluidModel::Modern);
        assert_eq!(PhysicsProfile::mc_1_8().fluid_model, FluidModel::Legacy1_8);
    }

    #[test]
    fn modern_input_path_is_selected_and_pure() {
        // The validated modern arm must be reachable through the enum dispatch and
        // produce the same result as the underlying unit-square function.
        let via_enum = modify_input(InputModel::UnitSquareProjection, 1.0, 1.0, None, false, 0.3);
        let direct = modify_input_unit_square(1.0, 1.0, None, false, 0.3);
        assert_eq!(via_enum.0.to_bits(), direct.0.to_bits());
        assert_eq!(via_enum.1.to_bits(), direct.1.to_bits());
    }

    #[test]
    fn use_item_default_scales_straight_input_by_the_vanilla_0_2_constant() {
        // Vanilla's own default use-effects speed multiplier = `0.2F`,
        // applied inside vanilla's own input modifier between the existing
        // `0.98` scale and the sneak scale. Straight-forward input has no
        // diagonal clamp to interact with, so the predicted magnitude is
        // exactly the product of the two outside-sourced record constants —
        // not a rounded guess.
        let (sx, sy) = modify_input_unit_square(0.0, 1.0, Some(UseEffects::DEFAULT), false, 0.3);
        assert_eq!(sx, 0.0);
        let expected = 1.0f32 * 0.98 * 0.2;
        assert!(
            (sy - expected).abs() < 1.0e-6,
            "expected {expected}, got {sy}"
        );
    }

    #[test]
    fn spear_use_effects_apply_no_slowdown_the_discriminating_item() {
        // Every use-item except a spear gets vanilla's own default use
        // effects (`speed_multiplier = 0.2`); the seven spear items are the
        // *only* override, at `1.0` (vanilla's own per-item spear override).
        // A fixture corpus containing only food cannot tell "the multiplier is
        // applied" from "the multiplier is applied to the wrong item set" —
        // the spear is the one input where the two hypotheses diverge.
        let baseline = modify_input_unit_square(0.0, 1.0, None, false, 0.3);
        let spear = modify_input_unit_square(0.0, 1.0, Some(UseEffects::SPEAR), false, 0.3);
        assert_eq!(baseline.1.to_bits(), spear.1.to_bits());
    }

    #[test]
    fn use_item_and_sneak_scales_combine_rather_than_one_overriding_the_other() {
        // The wrong hypothesis this guards against: an implementation that
        // treats sneaking and using-an-item as mutually exclusive branches
        // (`if sneak { .. } else if using_item { .. }`) instead of two
        // independent multiplies. Three distinct, independently-derived
        // magnitudes separate "both apply" from either single-gate reading —
        // this is not a sign check, it lands on one specific value.
        let (_, sy) = modify_input_unit_square(0.0, 1.0, Some(UseEffects::DEFAULT), true, 0.3);
        let both = 1.0f32 * 0.98 * 0.2 * 0.3;
        let sneak_only = 1.0f32 * 0.98 * 0.3;
        let use_item_only = 1.0f32 * 0.98 * 0.2;
        assert!(
            (sy - both).abs() < 1.0e-6,
            "expected both scales combined ({both}), got {sy} (sneak-only would \
             be {sneak_only}, use-item-only would be {use_item_only})"
        );
    }

    #[test]
    fn moving_slowly_scale_trails_both_shift_edges_by_one_tick() {
        let profile = PhysicsProfile::mc_1_21();
        let input = MovementInput {
            forward: 1.0,
            sneak: true,
            ..MovementInput::NONE
        };
        let mut state = PlayerState::at(Vec3d::new(0.5, 80.0, 0.5), 0.0)
            .with_pose(Pose::Standing);

        let (_, first_press_forward) =
            set_sprint_and_modify_input(&mut state, input, &profile);
        assert_eq!(
            first_press_forward.to_bits(),
            0.98_f32.to_bits(),
            "the first shift tick still reads the preceding standing state"
        );

        state = state.with_pose(Pose::Crouching);
        let (_, first_release_forward) = set_sprint_and_modify_input(
            &mut state,
            MovementInput {
                forward: 1.0,
                sneak: false,
                ..MovementInput::NONE
            },
            &profile,
        );
        assert_eq!(
            first_release_forward.to_bits(),
            (0.98_f32 * profile.sneaking_speed).to_bits(),
            "the first unshift tick still reads the preceding crouching state"
        );
    }

    #[test]
    fn use_item_scale_applies_before_the_unit_square_clamp_not_after() {
        // Vanilla's own input modifier applies its own item-use speed
        // multiplier to the raw strafe/forward *before* its own square-movement
        // normalization's `length * dist_to_unit_square` clamp to `1.0`. A
        // diagonal input is
        // the discriminating shape: full-magnitude diagonal input saturates
        // that clamp, but a 5x-reduced one (0.2 multiplier) typically does
        // not — so scaling *before* the clamp and scaling the *already-
        // clamped* output afterward land on genuinely different magnitudes,
        // not just a uniform 0.2x of each other.
        let unreduced = modify_input_unit_square(1.0, 1.0, None, false, 0.3);
        let unreduced_len = (unreduced.0 * unreduced.0 + unreduced.1 * unreduced.1).sqrt();
        assert!(
            (unreduced_len - 1.0).abs() < 1.0e-5,
            "control: an un-reduced diagonal must saturate the unit-square \
             clamp, got length {unreduced_len}"
        );

        let (rx, ry) = modify_input_unit_square(1.0, 1.0, Some(UseEffects::DEFAULT), false, 0.3);
        // Derived independently (not from this function): raw sx = sy = 0.98,
        // scaled to 0.196 each *before* the unit-square transform, giving a
        // pre-clamp length of 0.196*sqrt(2) ≈ 0.27719 — under 1.0, so the
        // clamp never engages and the diagonal's `dist_to_unit_square` factor
        // (sqrt(2)) is folded in below full saturation.
        let expected = 0.277_185_9_f32;
        assert!(
            (rx - expected).abs() < 1.0e-5 && (ry - expected).abs() < 1.0e-5,
            "expected ({expected}, {expected}), got ({rx}, {ry})"
        );
        // The wrong hypothesis (scale the already-clamped unit-diagonal output
        // by 0.2 afterward) would read {0.14142, 0.14142} instead — well
        // outside tolerance of the correct value, so this assertion cannot be
        // satisfied by both hypotheses at once.
        let wrong_hypothesis = unreduced.0 * 0.2;
        assert!(
            (rx - wrong_hypothesis).abs() > 0.02,
            "the pre-projection scale must diverge from naively scaling the \
             clamped output; got {rx}, wrong hypothesis would be {wrong_hypothesis}"
        );
    }

    #[test]
    #[should_panic(expected = "1.8 moveFlying input pipeline")]
    fn legacy_input_fails_loudly_not_silently() {
        // The whole point of the seam: a 1.8 profile must NOT silently run modern
        // math. Until the 1.8 pipeline is modelled and JVM-validated, it panics.
        let _ = modify_input(InputModel::LegacyMoveFlying, 1.0, 1.0, None, false, 0.3);
    }

    #[test]
    #[should_panic(expected = "1.8 fluid movement")]
    fn legacy_fluid_fails_loudly_not_silently() {
        let p = PhysicsProfile::mc_1_8();
        let view = WaterEverywhere;
        let mut s = PlayerState::at(Vec3d::new(0.5, 95.0, 0.5), 0.0);
        tick_water(&mut s, MovementInput::NONE, &FluidState::NONE, &view, &p);
    }

    #[test]
    fn jump_boost_power_is_tenth_per_level_as_float() {
        // getJumpBoostPower() = 0.1F*(amp+1) in float. Amp 0 (Jump Boost I) => 0.1F;
        // amp 1 (Jump Boost II) => 0.2F. The float literal matters (0.1 is inexact).
        assert_eq!(jump_boost_power(None), 0.0f32);
        assert_eq!(jump_boost_power(Some(0)).to_bits(), 0.1f32.to_bits());
        assert_eq!(
            jump_boost_power(Some(1)).to_bits(),
            (0.1f32 * 2.0).to_bits()
        );
    }

    #[test]
    fn slime_reverses_downward_velocity_and_sneak_cancels_it() {
        // First-principles anchor (not the oracle): a full slime cube has
        // bounce_restitution 1.0, so a player landing on it leaves with upward
        // velocity; the same fall while sneaking rests instead (vy path -> ~0).
        struct SlimeFloor;
        impl CollisionView for SlimeFloor {
            fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
                if y == 0 {
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
            fn bounce_restitution(&self, _x: i32, y: i32, _z: i32) -> f32 {
                if y == 0 { 1.0 } else { 0.0 }
            }
        }
        let p = PhysicsProfile::mc_1_21();

        let mut bounced = false;
        let mut s = PlayerState::at(Vec3d::new(0.5, 6.0, 0.5), 0.0);
        for _ in 0..40 {
            tick(&mut s, MovementInput::NONE, &SlimeFloor, &p);
            if s.velocity.y > 0.05 {
                bounced = true;
                break;
            }
        }
        assert!(bounced, "player never bounced off slime");

        let mut peak: f64 = 1.0;
        let mut s = PlayerState::at(Vec3d::new(0.5, 6.0, 0.5), 0.0);
        let sneak = MovementInput {
            forward: 0.0,
            strafe: 0.0,
            jump: false,
            sneak: true,
            sprint: false,
            using_item: None,
        };
        for _ in 0..80 {
            tick(&mut s, sneak, &SlimeFloor, &p);
            assert!(
                s.velocity.y <= 0.05,
                "sneak failed to cancel the bounce: vy = {}",
                s.velocity.y
            );
            peak = peak.max(s.position.y);
        }
        // Never launched back above the drop height once landed.
        assert!(peak <= 6.0, "sneaking player gained height: {peak}");
    }
}
