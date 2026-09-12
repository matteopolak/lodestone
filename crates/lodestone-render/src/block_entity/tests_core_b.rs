
    #[test]
    fn every_ported_skull_type_bakes_and_resolves() {
        let set = set();
        for t in SKULL_TYPES {
            let spawn = SkullSpawn {
                skull_type: *t,
                texture: BlockEntityTexture::Static(skull_texture_stem(*t)),
                ..SkullSpawn::at([0, 0, 0])
            };
            let inst = set
                .resolve_skull(&spawn)
                .unwrap_or_else(|| panic!("{t:?} did not resolve"));
            assert!(!inst.part_transforms.is_empty(), "{t:?}");
            assert_eq!(inst.texture, skull_texture_stem(*t));
        }
    }
    /// The dragon's jaw and the piglin's ears are **assigned** by
    /// vanilla's own animation update, not added to their authored rest pose, and at rest the
    /// assigned value differs from the authored one in both cases. Predicting
    /// both hypotheses is the point: reading the mesh's own rest pose gives
    /// `0.0` for the jaw and `±PI/6` for the ears, and both are plausible
    /// enough to survive a look at the screen.
    #[test]
    fn dragon_jaw_and_piglin_ears_rest_away_from_their_authored_pose() {
        let jaw = dragon_head_jaw_x_rot(SKULL_RESTING_ANIMATION_POS);
        assert!((jaw - 0.2).abs() < 1e-6, "jaw at rest is {jaw}, want 0.2");
        assert!(
            jaw.abs() > 1e-3,
            "the rest-pose hypothesis (a clamped-shut 0.0 jaw) must not also satisfy this"
        );

        let (left, right) = piglin_head_ear_z_rots(SKULL_RESTING_ANIMATION_POS);
        assert!((left + 0.7).abs() < 1e-6, "left ear at rest is {left}, want -0.7");
        assert!((right - 0.7).abs() < 1e-6, "right ear at rest is {right}, want 0.7");
        let authored = std::f32::consts::FRAC_PI_6;
        assert!(
            (right - authored).abs() > 0.15,
            "0.7 must be distinguishable from the authored +PI/6 ({authored})"
        );
    }

    /// The `1.2` asymmetry on the *left* ear only. It is invisible at rest —
    /// both ears evaluate to `±0.7` with or without it — so this gate has to
    /// pick a position where the two hypotheses separate. At `12.5` the left
    /// ear's own cosine is at `3*PI` (`-1`) while the shared one is at
    /// `2.5*PI` (`0`), which is the widest the two readings ever get:
    /// `-0.3` against `-0.5`.
    #[test]
    fn the_piglin_ear_asymmetry_is_only_visible_off_rest() {
        let rest = piglin_head_ear_z_rots(SKULL_RESTING_ANIMATION_POS);
        assert!(
            (rest.0 + rest.1).abs() < 1e-6,
            "at rest the two ears are exact mirrors, so rest cannot discriminate"
        );

        let (left, right) = piglin_head_ear_z_rots(12.5);
        assert!((left + 0.3).abs() < 1e-5, "left ear is {left}, want -0.3");
        assert!((right - 0.5).abs() < 1e-5, "right ear is {right}, want 0.5");
        // The wrong hypothesis: no `1.2`, so the left ear mirrors the right.
        let without_asymmetry = -right;
        assert!(
            (left - without_asymmetry).abs() > 0.15,
            "left {left} must not land on the no-asymmetry value {without_asymmetry}"
        );
    }

    /// `resolve_skull` must actually *apply* those two poses — the island
    /// check for the override block, since a correct formula nothing calls
    /// draws exactly like no formula at all. Compares each posed child's own
    /// world matrix against the same mesh resolved with no override.
    #[test]
    fn resolve_skull_poses_the_dragon_jaw_and_both_piglin_ears() {
        let set = set();
        // Collected across *both* subjects and every part, not asserted inside
        // the loop: an `assert!` per iteration stops at the dragon's jaw and
        // leaves both piglin ears an argument rather than an observation.
        // Under the neuter this reports 3 of 3.
        let mut unchanged: Vec<String> = Vec::new();
        for (skull_type, model, parts) in [
            (SkullType::Dragon, SKULL_DRAGON, &[DRAGON_HEAD_JAW_PART][..]),
            (SkullType::Piglin, SKULL_PIGLIN, &PIGLIN_HEAD_EAR_PARTS[..]),
        ] {
            let spawn = SkullSpawn {
                skull_type,
                texture: BlockEntityTexture::Static(skull_texture_stem(skull_type)),
                ..SkullSpawn::at([0, 0, 0])
            };
            let inst = set
                .resolve_skull(&spawn)
                .unwrap_or_else(|| panic!("{skull_type:?} did not resolve"));
            let mesh = set.get(model).expect("model in corpus");
            let unposed = mesh.part_transforms(inst.transform, &[]);
            for name in parts {
                let i = mesh.index_of(name).expect("posed part in mesh");
                if inst.part_transforms[i].abs_diff_eq(unposed[i], 1e-5) {
                    unchanged.push(format!("{skull_type:?}/{name}"));
                }
            }
        }
        assert!(
            unchanged.is_empty(),
            "kept their authored rest pose: {unchanged:?}"
        );
    }

    /// Ground and wall placement both preserve orientation (`det == +1`),
    /// same as the chest placements — measured, not assumed, because this is
    /// the one block-entity placement that *does* apply the entity-style
    /// `scale(-1, -1, 1)` flip and a sign mistake there would show up as a
    /// negative determinant, not merely "upside down".
    #[test]
    fn skull_placement_preserves_orientation() {
        for seg in [0u8, 4, 8, 12, 15] {
            let m = skull_ground_placement_matrix([1, 2, 3], seg);
            assert!(
                (m.determinant() - 1.0).abs() < 1e-4,
                "segment {seg}: det {}",
                m.determinant()
            );
        }
        for yaw in [0.0_f32, 90.0, 180.0, 270.0] {
            let m = skull_wall_placement_matrix([1, 2, 3], yaw);
            assert!(
                (m.determinant() - 1.0).abs() < 1e-4,
                "yaw {yaw}: det {}",
                m.determinant()
            );
        }
    }

    /// Unlike a chest, a floor skull genuinely flips Y — the mirror image of
    /// `placement_does_not_flip_or_lift`'s chest assertion. Getting this
    /// backwards would bury the head texture upside down while every bounds
    /// and determinant check stayed green.
    #[test]
    fn ground_skull_flips_y_like_an_entity_head() {
        let m = skull_ground_placement_matrix([0, 0, 0], 0);
        let up = m.transform_vector3(Vec3::Y);
        assert!(up.y < 0.0, "expected an entity-style flip, got {up}");
    }

    /// The rotation segment spins the head about the block's own centre
    /// pivot `(0.5, 0, 0.5)`, so that pivot must land in the same world point
    /// regardless of segment — only the *head*, not the block position,
    /// rotates.
    #[test]
    fn ground_segment_rotates_about_the_block_centre() {
        let pos = [2, 5, -3];
        let unrotated = skull_ground_placement_matrix(pos, 0);
        let rotated = skull_ground_placement_matrix(pos, 8); // 180 degrees
        let a = unrotated.transform_point3(Vec3::ZERO);
        let b = rotated.transform_point3(Vec3::ZERO);
        assert!(a.abs_diff_eq(b, 1e-4), "pivot moved: {a} vs {b}");
        let expected = Vec3::new(2.5, 5.0, -2.5);
        assert!(a.abs_diff_eq(expected, 1e-4), "{a}");
    }

    /// `dir.getStepX()/getStepZ()` recovered by trig against a hand-verified
    /// table (not derived from the function under test): south `(0, 1)`,
    /// west `(-1, 0)`, north `(0, -1)`, east `(1, 0)`. A sign slip here
    /// offsets a wall skull toward the wrong wall while it still renders a
    /// plausible skull shape.
    #[test]
    fn wall_offset_moves_toward_the_named_direction() {
        let cases = [
            ("south", 0.0_f32, 0.0_f32, 1.0_f32),
            ("west", 90.0, -1.0, 0.0),
            ("north", 180.0, 0.0, -1.0),
            ("east", 270.0, 1.0, 0.0),
        ];
        for (name, yaw, step_x, step_z) in cases {
            let m = skull_wall_placement_matrix([0, 0, 0], yaw);
            let origin = m.transform_point3(Vec3::ZERO);
            let expected = Vec3::new(0.5 - step_x * 0.25, 0.25, 0.5 - step_z * 0.25);
            assert!(
                origin.abs_diff_eq(expected, 1e-4),
                "{name}: got {origin}, expected {expected}"
            );
        }
    }

    /// A chest and a skull share neither model nor texture, so a frame
    /// holding both must batch them separately — the same coverage the chest
    /// `planning_batches_by_model_and_texture_and_culls_what_is_behind` test
    /// gives two chest materials, now across two entirely different corpora,
    /// proving [`plan_block_entities`]/[`BlockEntityInstance`] are generic
    /// over block-entity *family*, not just over chest variants.
    #[test]
    fn chests_and_skulls_batch_independently_in_one_frame() {
        let set = set();
        let chest = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let skull = set.resolve_skull(&SkullSpawn::at([1, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[chest, skull],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.drawn, 2);
        assert_eq!(
            frame.batches.len(),
            2,
            "a chest and a skull must not share a batch"
        );
    }

    #[test]
    fn bell_stem_is_in_the_preload_list() {
        let stems = bell_texture_stems();
        assert_eq!(stems, vec![BELL_TEXTURE_STEM]);
        assert!(block_entity_texture_stems().contains(&BELL_TEXTURE_STEM));
    }

    /// Every stem [`shulker_texture_stem`] can return is preloaded, and the
    /// colour order is `DyeColor`'s **ordinal** order rather than the
    /// alphabetical one the texture directory suggests — reading it off the
    /// listing shifts every dyed box one sprite along, which draws a plausible
    /// wrong colour instead of nothing.
    #[test]
    fn every_shulker_stem_is_in_the_preload_list_in_dye_ordinal_order() {
        // `DyeColor`'s first four and last, from the enum's own declaration order
        // (vanilla's dye-colour registration), not from this table.
        assert_eq!(
            &SHULKER_COLOURS[..4],
            &["white", "orange", "magenta", "light_blue"]
        );
        assert_eq!(SHULKER_COLOURS[15], "black");
        assert_eq!(SHULKER_COLOURS.len(), 16);

        let preload = block_entity_texture_stems();
        assert!(preload.contains(&shulker_texture_stem(None)));
        for colour in SHULKER_COLOURS {
            let stem = shulker_texture_stem(Some(colour));
            assert_ne!(
                stem, SHULKER_DEFAULT_TEXTURE_STEM,
                "{colour} fell through to the undyed sheet"
            );
            assert!(preload.contains(&stem), "{stem} missing from the preload list");
        }
        // An unrecognised name degrades to the undyed sheet rather than being
        // dropped — a plain `shulker_box` has no colour segment at all.
        assert_eq!(
            shulker_texture_stem(Some("chartreuse")),
            SHULKER_DEFAULT_TEXTURE_STEM
        );
    }

    /// An upward-facing shulker box occupies its own block cell and nothing else.
    ///
    /// The expectation comes from geometry rather than from the matrix: the box is
    /// authored as a 16×20 texel stack (`base` 8 tall from y=−8, `lid` 12 tall from
    /// y=−16, both at pivot y=24), so once vanilla's `scale(1, -1, -1)` and
    /// `translate(0, -1, 0)` are folded in it must sit in `0..1` on every axis, at
    /// `0.9995` scale about the block centre. Reusing
    /// [`block_entity_placement_matrix`] instead (a floor pivot, no flip) puts the
    /// box a half-block low and upside down — which still looks like a box.
    #[test]
    fn an_upward_shulker_sits_inside_its_own_block() {
        let set = set();
        let box_at = set.resolve_shulker(&ShulkerSpawn::at([3, 5, -2])).unwrap();
        let lo = Vec3::from(box_at.aabb_min);
        let hi = Vec3::from(box_at.aabb_max);
        let cell = Vec3::new(3.0, 5.0, -2.0);
        assert!(
            lo.cmpge(cell - Vec3::splat(0.001)).all() && hi.cmple(cell + Vec3::splat(1.001)).all(),
            "an up-facing box escaped its own cell: {lo} .. {hi}"
        );
        // And it fills nearly all of it — the `0.9995` shrink, not a half-height
        // box. A `0.5`-tall result is the floor-pivot mistake above.
        let size = hi - lo;
        assert!(
            size.min_element() > 0.99,
            "the box is not block-sized: {size}"
        );
        assert_eq!(box_at.texture, SHULKER_DEFAULT_TEXTURE_STEM);
    }

    /// A closed box needs no part override at all; an open one moves only `lid`.
    /// This is what lets a shulker box share the existing `(model, texture)` batch
    /// key with no per-instance animation state.
    #[test]
    fn a_closed_shulker_is_the_rest_pose_and_an_open_one_moves_only_the_lid() {
        let set = set();
        let mesh = set.get(SHULKER_BOX).unwrap();
        let lid = mesh.index_of("lid").expect("the lid part is named `lid`");
        let base = mesh.index_of("base").expect("the base part is named `base`");

        let closed = set.resolve_shulker(&ShulkerSpawn::at([0, 0, 0])).unwrap();
        let rest = mesh.part_transforms(shulker_placement_matrix([0, 0, 0], ShulkerFacing::Up), &[]);
        assert_eq!(closed.part_transforms[lid], rest[lid]);

        let open = set
            .resolve_shulker(&ShulkerSpawn {
                progress: 1.0,
                ..ShulkerSpawn::at([0, 0, 0])
            })
            .unwrap();
        assert_ne!(open.part_transforms[lid], closed.part_transforms[lid]);
        assert_eq!(
            open.part_transforms[base], closed.part_transforms[base],
            "opening a box moved its base"
        );
        // `lid.setPos(0, 24 - progress * 0.5 * 16, 0)` and `yRot = 270 * progress`
        // — predicted from the jar, not read back out of the port.
        assert_eq!(shulker_lid_pose(0.0), (24.0, 0.0));
        let (y, y_rot) = shulker_lid_pose(1.0);
        assert_eq!(y, 16.0);
        assert!((y_rot - 270.0_f32.to_radians()).abs() < 1e-5, "{y_rot}");
    }

    /// The six facings are six distinct placements, and a down-facing box is the
    /// up-facing one turned over — the direction-to-rotation port.
    #[test]
    fn every_shulker_facing_is_a_distinct_placement() {
        let facings = [
            ShulkerFacing::Up,
            ShulkerFacing::Down,
            ShulkerFacing::North,
            ShulkerFacing::South,
            ShulkerFacing::West,
            ShulkerFacing::East,
        ];
        let mats: Vec<Mat4> = facings
            .iter()
            .map(|f| shulker_placement_matrix([0, 0, 0], *f))
            .collect();
        for i in 0..mats.len() {
            for j in (i + 1)..mats.len() {
                assert!(
                    !mats[i].abs_diff_eq(mats[j], 1e-5),
                    "{:?} and {:?} share a placement",
                    facings[i],
                    facings[j]
                );
            }
        }
        // Every facing still lands the box in its own cell, which is the property
        // an axis mix-up in `ShulkerFacing::rotation` breaks.
        let set = set();
        for facing in facings {
            let drawn = set
                .resolve_shulker(&ShulkerSpawn {
                    facing,
                    ..ShulkerSpawn::at([0, 0, 0])
                })
                .unwrap();
            let lo = Vec3::from(drawn.aabb_min);
            let hi = Vec3::from(drawn.aabb_max);
            assert!(
                lo.cmpge(Vec3::splat(-0.001)).all() && hi.cmple(Vec3::splat(1.001)).all(),
                "{facing:?} escaped its own cell: {lo} .. {hi}"
            );
        }
        assert_eq!(ShulkerFacing::from_name("up"), Some(ShulkerFacing::Up));
        assert_eq!(ShulkerFacing::from_name("sideways"), None);
        assert_eq!(ShulkerFacing::default(), ShulkerFacing::Up);
    }

    /// Vanilla's own bell animation update's exact formula, predicted independently of the
    /// port rather than by re-deriving its own arithmetic: choosing
    /// `ticks = pi^2 / 2` makes `sin(ticks / pi) == sin(pi/2) == 1` exactly,
    /// so the only remaining unknown is `base_rot = 1 / (4 + ticks/3)` and
    /// each direction's sign/axis — a magnitude check, not merely a sign
    /// flip (`CLAUDE.md`'s "predict the value, do not merely assert the
    /// sign" rule).
    #[test]
    fn bell_shake_angle_matches_the_exact_vanilla_formula() {
        assert_eq!(bell_shake_angle(None, 999.0), (0.0, 0.0), "no direction, no motion");
        assert_eq!(
            bell_shake_angle(Some(BellShakeDirection::North), 0.0),
            (0.0, 0.0),
            "sin(0) is zero at tick 0"
        );

        let ticks = std::f32::consts::PI * std::f32::consts::PI / 2.0;
        let expected = 1.0 / (4.0 + ticks / 3.0);

        let (x, z) = bell_shake_angle(Some(BellShakeDirection::North), ticks);
        assert!((x - -expected).abs() < 1e-4, "north x_rot {x}");
        assert_eq!(z, 0.0);

        let (x, z) = bell_shake_angle(Some(BellShakeDirection::South), ticks);
        assert!((x - expected).abs() < 1e-4, "south x_rot {x}");
        assert_eq!(z, 0.0);

        let (x, z) = bell_shake_angle(Some(BellShakeDirection::East), ticks);
        assert_eq!(x, 0.0);
        assert!((z - -expected).abs() < 1e-4, "east z_rot {z}");

        let (x, z) = bell_shake_angle(Some(BellShakeDirection::West), ticks);
        assert_eq!(x, 0.0);
        assert!((z - expected).abs() < 1e-4, "west z_rot {z}");
    }

    /// The rim (`bell_base`) has no override of its own — if shaking the body
    /// did not also move it, that would mean the parent/child nesting broke
    /// (see `bell_model`'s doc), not merely that the shake is small.
    #[test]
    fn shaking_the_body_moves_the_rim_too_because_it_is_a_child() {
        let set = set();
        let resting = set.resolve_bell(&BellSpawn::at([0, 0, 0])).unwrap();
        assert_eq!(resting.texture, BELL_TEXTURE_STEM);
        assert!(!resting.part_transforms.is_empty());

        let mesh = set.get(BELL).unwrap();
        let body = mesh.index_of("bell_body").unwrap();
        let base = mesh.index_of("bell_base").unwrap();

        let ticks = std::f32::consts::PI * std::f32::consts::PI / 2.0;
        let shaking = set
            .resolve_bell(&BellSpawn {
                shake: Some((BellShakeDirection::East, ticks)),
                ..BellSpawn::at([0, 0, 0])
            })
            .unwrap();

        assert_ne!(
            shaking.part_transforms[body], resting.part_transforms[body],
            "the body itself must move"
        );
        assert_ne!(
            shaking.part_transforms[base], resting.part_transforms[base],
            "the rim must move with its parent"
        );
    }

    #[test]
    fn bells_batch_independently_from_chests_and_skulls() {
        let set = set();
        let chest = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let skull = set.resolve_skull(&SkullSpawn::at([1, 0, 0])).unwrap();
        let bell = set.resolve_bell(&BellSpawn::at([2, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[chest, skull, bell],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.drawn, 3);
        assert_eq!(
            frame.batches.len(),
            3,
            "a chest, a skull and a bell must not share a batch"
        );
    }

    // --- banner -----------------------------------------------------------

    /// `banner_ground_placement_matrix`'s scale flips **two** axes (Y and Z),
    /// like `skull_ground_placement_matrix`'s single-axis flip is paired with
    /// the rotation's own handedness — the product of an even number of sign
    /// flips preserves orientation. Measured, not assumed: this is the same
    /// "measure the determinant, don't assert it" discipline
    /// `placement_preserves_orientation` already holds the chest placement
    /// to, generalised to a matrix whose magnitude is `(2/3)^3`, not `1`, so
    /// the assertion is on the *sign* of the determinant, not its value.
    #[test]
    fn banner_ground_placement_preserves_orientation() {
        for segment in [0u8, 1, 4, 8, 12, 15] {
            let m = banner_ground_placement_matrix([3, 64, -7], segment);
            assert!(
                m.determinant() > 0.0,
                "segment {segment}: det {} should be positive (two axis flips cancel)",
                m.determinant()
            );
        }
    }

    /// The two flips are real, individually — not merely a determinant that
    /// happens to be positive by some other route. `+Y` and `+Z` must each
    /// reverse under the placement's linear part, the mirror image of
    /// `placement_does_not_flip_or_lift`'s "chest does not flip" assertion.
    #[test]
    fn banner_ground_placement_flips_y_and_z_but_not_x() {
        let m = banner_ground_placement_matrix([0, 0, 0], 0);
        let up = m.transform_vector3(Vec3::Y);
        let fwd = m.transform_vector3(Vec3::Z);
        let right = m.transform_vector3(Vec3::X);
        assert!(up.y < 0.0, "expected a Y flip, got {up}");
        assert!(fwd.z < 0.0, "expected a Z flip, got {fwd}");
        assert!(right.x > 0.0, "X must not flip, got {right}");
        // Magnitude is the real `2/3` scale, not `1` — skipping it would
        // render a banner 1.5x too large.
        assert!((up.length() - 2.0 / 3.0).abs() < 1e-5, "up length {}", up.length());
    }

    /// `banner_phase`'s exact `floorMod` formula: zero at the origin with no
    /// game time, wraps every 100 ticks, and a negative-leaning block
    /// coordinate sum still lands in `0..1` rather than going negative
    /// (Rust's `%` truncates toward zero and would fail this).
    #[test]
    fn banner_phase_matches_the_floor_mod_formula_and_wraps() {
        assert_eq!(banner_phase([0, 0, 0], 0, 0.0), 0.0);
        // sum = 7 (x=1), game_time 93 -> 100 -> floorMod 0.
        assert_eq!(banner_phase([1, 0, 0], 93, 0.0), 0.0);
        // Partial tick folds in additively, still divided by 100.
        let with_partial = banner_phase([0, 0, 0], 0, 0.5);
        assert!((with_partial - 0.005).abs() < 1e-6, "{with_partial}");
        // A coordinate sum that goes negative must still wrap into 0..100,
        // not produce a negative phase.
        let negative = banner_phase([-5, 0, 0], 0, 0.0);
        assert!((0.0..1.0).contains(&negative), "{negative}");
        // 7 * -5 = -35; floorMod(-35, 100) = 65 -> phase 0.65.
        assert!((negative - 0.65).abs() < 1e-6, "{negative}");
    }

    /// `banner_flag_x_rot`'s exact formula at three phases — a magnitude
    /// prediction, not merely "the sign changes" (`CLAUDE.md`'s "predict the
    /// value" rule). `cos` is exactly `1`, `0` and `-1` at these three
    /// phases, so every intermediate multiply is exact rather than
    /// approximate.
    #[test]
    fn banner_flag_x_rot_matches_the_exact_vanilla_formula() {
        let pi = std::f32::consts::PI;
        // phase 0: cos(0) = 1 -> (-0.0125 + 0.01) * pi = -0.0025 * pi.
        assert!((banner_flag_x_rot(0.0) - (-0.0025 * pi)).abs() < 1e-5);
        // phase 0.25: cos(pi/2) = 0 -> -0.0125 * pi exactly.
        assert!((banner_flag_x_rot(0.25) - (-0.0125 * pi)).abs() < 1e-5);
        // phase 0.5: cos(pi) = -1 -> (-0.0125 - 0.01) * pi = -0.0225 * pi.
        assert!((banner_flag_x_rot(0.5) - (-0.0225 * pi)).abs() < 1e-5);
    }

    /// The base mask is always present and first, even with zero stored
    /// patterns, and every stored pattern follows in its own order —
    /// `resolve_banner` reaching all the way to `banner_pattern_layers`'
    /// own contract (`no_patterns_still_draws_the_base_layer`/
    /// `pattern_order_is_preserved_exactly` in `banner_pattern.rs`), not
    /// re-deriving it.
    #[test]
    fn resolve_banner_produces_the_base_layer_plus_every_pattern_in_order() {
        let set = set();
        let patterns = vec![
            StoredPatternLayer {
                pattern_asset_id: "creeper".to_string(),
                color: DyeColor::Lime,
            },
            StoredPatternLayer {
                pattern_asset_id: "stripe_top".to_string(),
                color: DyeColor::Black,
            },
        ];
        let banner = set
            .resolve_banner(&BannerSpawn {
                base_color: DyeColor::Red,
                patterns,
                ..BannerSpawn::at([0, 0, 0])
            })
            .expect("banner_body and banner_flag must both be in the corpus");
        assert_eq!(banner.layers.len(), 3, "base + 2 patterns");
        assert_eq!(banner.layers[0].color, DyeColor::Red.gamma_rgb());
        assert_eq!(banner.layers[1].color, DyeColor::Lime.gamma_rgb());
        assert_eq!(banner.layers[2].color, DyeColor::Black.gamma_rgb());
        assert!(
            banner.layers[0].sprite.path().ends_with("banner/base"),
            "{:?}",
            banner.layers[0].sprite
        );
        assert!(
            banner.layers[1].sprite.path().ends_with("banner/creeper"),
            "{:?}",
            banner.layers[1].sprite
        );
    }

    /// Every layer reuses the *flag's* posed transform, never the body's —
    /// pattern masks paint over the cloth, not the pole/bar, and a wrong
    /// wiring here would have every mask draw at the pole's own (much
    /// smaller, differently pivoted) rect instead of the flag's.
    #[test]
    fn resolve_banner_layers_share_the_flag_transform_not_the_body() {
        let set = set();
        let banner = set.resolve_banner(&BannerSpawn::at([0, 0, 0])).unwrap();
        let flag_mesh = set.get(BANNER_FLAG).unwrap();
        let flag_index = flag_mesh.index_of("flag").unwrap();
        let expected = banner.flag.part_transforms[flag_index];
        for (i, layer) in banner.layers.iter().enumerate() {
            assert_eq!(layer.transform, expected, "layer {i} transform must equal the flag's");
        }
        assert_ne!(
            banner.layers[0].transform, banner.body.transform,
            "the layer transform must not be the bare placement (the body's)"
        );
    }

    /// The sway moves the flag's own transform, and every layer moves with
    /// it — the same "does it move geometry, not just produce a different
    /// number" standard `opening_moves_the_lid_and_lock_and_leaves_the_bottom_alone`
    /// holds the chest lid to, and `shaking_the_body_moves_the_rim_too_because_it_is_a_child`
    /// holds the bell rim to.
    #[test]
    fn resolve_banner_sway_moves_the_flag_and_every_layer_with_it() {
        let set = set();
        let resting = set
            .resolve_banner(&BannerSpawn {
                patterns: vec![StoredPatternLayer {
                    pattern_asset_id: "creeper".to_string(),
                    color: DyeColor::Lime,
                }],
                ..BannerSpawn::at([0, 0, 0])
            })
            .unwrap();
        let swaying = set
            .resolve_banner(&BannerSpawn {
                phase: 0.5,
                patterns: vec![StoredPatternLayer {
                    pattern_asset_id: "creeper".to_string(),
                    color: DyeColor::Lime,
                }],
                ..BannerSpawn::at([0, 0, 0])
            })
            .unwrap();
        assert_ne!(
            resting.flag.part_transforms, swaying.flag.part_transforms,
            "the flag itself must move"
        );
        assert_eq!(
            resting.body.part_transforms, swaying.body.part_transforms,
            "the pole/bar must not move — only the flag sways"
        );
        assert_ne!(
            resting.layers[0].transform, swaying.layers[0].transform,
            "every pattern layer must move with the flag"
        );
        assert_ne!(
            resting.layers[1].transform, swaying.layers[1].transform,
            "including the base layer"
        );
    }

    #[test]
    fn banner_texture_stem_is_shared_by_body_and_flag_and_in_the_preload_list() {
        let set = set();
        let banner = set.resolve_banner(&BannerSpawn::at([0, 0, 0])).unwrap();
        assert_eq!(banner.body.texture, BANNER_BASE_TEXTURE_STEM);
        assert_eq!(banner.flag.texture, BANNER_BASE_TEXTURE_STEM);
        assert_eq!(banner_texture_stems(), vec![BANNER_BASE_TEXTURE_STEM]);
        assert!(block_entity_texture_stems().contains(&BANNER_BASE_TEXTURE_STEM));
    }

    /// A banner's opaque body+flag batch independently from a chest — the
    /// same coverage `bells_batch_independently_from_chests_and_skulls`
    /// gives bells, now for the fourth family. The banner's own translucent
    /// `layers` are not part of `plan_block_entities` at all (by design —
    /// see `BannerInstances`' doc), so only `body`/`flag` go into this call.
    #[test]
    fn banner_body_and_flag_batch_independently_from_a_chest() {
        let set = set();
        let chest = set.resolve_chest(&ChestSpawn::at([2, 0, 0])).unwrap();
        let banner = set.resolve_banner(&BannerSpawn::at([0, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[chest, banner.body, banner.flag],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.drawn, 3);
        assert_eq!(
            frame.batches.len(),
            3,
            "chest, banner body and banner flag must all batch independently \
             (different model *and* different model between body/flag)"
        );
    }

    /// **A wall banner draws the pole-less rig, at the wall's own height.**
    ///
    /// Three assertions, each catching a different way this goes wrong while still
    /// drawing a recognisable banner:
    ///
    /// * the models are the *wall* pair, so the standing rig's 42-texel pole
    ///   cannot end up hanging in mid-air off a block face;
    /// * the wall body really has no `pole` part and the standing one does — an
    ///   `if (standing)` in `createBodyLayer` that was transcribed as
    ///   unconditional would give both a pole and pass any "two banner meshes
    ///   exist" check;
    /// * the two flags sit at **different heights**, which is the whole content of
    ///   the `standing ? -44 : -20.5` pose ternary. Their *cubes* are
    ///   byte-identical, so a copy that reused the standing pose produces a wall
    ///   banner buried two blocks into the floor and no assertion about geometry
    ///   would notice.
    #[test]
    fn a_wall_banner_uses_the_poleless_rig_and_hangs_at_its_own_height() {
        let set = set();
        let wall = set
            .resolve_banner(&BannerSpawn::on_wall([0, 0, 0], 180.0))
            .expect("both wall banner models must be in the corpus");
        assert_eq!(wall.body.model, BANNER_WALL_BODY);
        assert_eq!(wall.flag.model, BANNER_WALL_FLAG);
        assert_eq!(wall.body.texture, BANNER_BASE_TEXTURE_STEM, "one shared sheet");
        assert_eq!(wall.flag.texture, BANNER_BASE_TEXTURE_STEM);

        let standing_body = set.get(BANNER_BODY).unwrap();
        let wall_body = set.get(BANNER_WALL_BODY).unwrap();
        assert!(
            standing_body.index_of("pole").is_some(),
            "the standing body must have a pole"
        );
        assert!(
            wall_body.index_of("pole").is_none(),
            "createBodyLayer(false) adds no pole"
        );
        assert!(wall_body.index_of("bar").is_some());

        // The pose ternary, measured through the same `part_transforms` the draw
        // uses rather than by restating -44 and -20.5.
        let standing_flag = set.get(BANNER_FLAG).unwrap();
        let wall_flag = set.get(BANNER_WALL_FLAG).unwrap();
        let flag_y = |mesh: &BlockEntityMesh| {
            let i = mesh.index_of("flag").unwrap();
            mesh.part_transforms(Mat4::IDENTITY, &[])[i]
                .transform_point3(Vec3::ZERO)
                .y
        };
        let (standing_y, wall_y) = (flag_y(standing_flag), flag_y(wall_flag));
        assert!(
            (standing_y - -44.0 / 16.0).abs() < 1e-5,
            "standing flag pivot {standing_y}"
        );
        assert!(
            (wall_y - -20.5 / 16.0).abs() < 1e-5,
            "wall flag pivot {wall_y}"
        );
        assert!(
            wall_y > standing_y,
            "a wall banner hangs higher in model space than a standing one's \
             cloth: {wall_y} vs {standing_y}"
        );

        // The cubes are identical, which is exactly why the pose above is the
        // only thing separating them.
        assert_eq!(standing_flag.quad_count(), wall_flag.quad_count());
    }

    /// The two placements are one function with two angles — but the *angle
    /// conventions* are not interchangeable, and this is what stops a caller
    /// handing a wall banner a rotation segment.
    ///
    /// A segment is `22.5°` per step and a facing is `90°`, so segment `4` and
    /// facing `west` are the same `90°` rotation while segment `4` read as a
    /// *facing* would be nothing at all. The gate pins the shared shape and the
    /// distinct convention together: equal matrices at equal *angles*, and a
    /// deliberately unequal pair at the same numeric input.
    #[test]
    fn the_two_banner_placements_share_one_transform_and_two_angle_conventions() {
        let pos = [3, 70, -5];
        // Segment 4 is 4 * 22.5 = 90 degrees, which is also `west`'s toYRot.
        assert_eq!(
            banner_ground_placement_matrix(pos, 4),
            banner_wall_placement_matrix(pos, horizontal_facing_yaw("west").unwrap()),
            "one modelTransformation, two callers"
        );
        // The same *number* means different things to the two.
        assert_ne!(
            banner_ground_placement_matrix(pos, 4),
            banner_wall_placement_matrix(pos, 4.0),
            "a segment is 22.5 degrees per step; a facing yaw is degrees"
        );
        // And neither has skull's push away from the wall: the block's own
        // corner-plus-half is the whole translation.
        let at_origin = banner_wall_placement_matrix([0, 0, 0], 0.0).transform_point3(Vec3::ZERO);
        assert!(
            (at_origin - Vec3::new(0.5, 0.0, 0.5)).length() < 1e-6,
            "no extra offset away from the wall, got {at_origin}"
        );
    }

    /// Vanilla's own clockwise-rotated yaw, hand-expanded from the jar's own two
    /// tables (the clockwise-turn table: north→east→south→west→north, and
    /// the yaw table: south 0, west 90, north 180, east 270).
    ///
    /// The wrong hypothesis is not an error but a quarter turn, and it is
    /// spelled with the function *next to* the right one, so this asserts both
    /// arms in the same run: every facing's clockwise yaw must differ from its
    /// plain yaw by exactly 90°, and the four expected values are written out
    /// rather than derived from `horizontal_facing_yaw` (which would make the
    /// test agree with whatever the implementation does).
    #[test]
    fn a_lecterns_yaw_is_the_facing_turned_clockwise_not_the_facing() {
        for (facing, clockwise, plain) in [
            ("north", 270.0_f32, 180.0_f32),
            ("east", 0.0, 270.0),
            ("south", 90.0, 0.0),
            ("west", 180.0, 90.0),
        ] {
            assert_eq!(
                horizontal_facing_clockwise_yaw(facing),
                Some(clockwise),
                "{facing}"
            );
            assert_eq!(horizontal_facing_yaw(facing), Some(plain), "{facing}");
            assert_ne!(
                clockwise, plain,
                "{facing}: the two must differ, or this test proves nothing"
            );
        }
        assert_eq!(horizontal_facing_clockwise_yaw("up"), None);
    }

    /// Vanilla's own book animation-state constructor, called with
    /// `(0.0, 0.1, 0.9, 1.2)`, collapses to a
    /// constant, computed here from the jar's four literals rather than by
    /// reading [`LECTERN_BOOK_OPENNESS`] back.
    ///
    /// The point is the `sin(progress * 0.02)` term: it is dead at
    /// `progress == 0`, which is why a lectern book must not be given a live
    /// clock. The second assertion is the control — with a *non*-zero progress
    /// the same formula does move, so the constant is a property of the
    /// lectern's arguments and not of the formula being inert.
    #[test]
    fn a_lectern_books_openness_is_constant_because_its_progress_term_is_dead() {
        fn for_animation(progress: f32, openness: f32) -> f32 {
            ((progress * 0.02).sin() * 0.1 + 1.25) * openness
        }
        assert!((for_animation(0.0, 1.2) - LECTERN_BOOK_OPENNESS).abs() < 1e-6);
        assert!((for_animation(0.0, 1.2) - 1.5).abs() < 1e-6);
        assert!(
            (for_animation(100.0, 1.2) - 1.5).abs() > 1e-3,
            "a live progress *would* move openness, so the constant above is \
             about the lectern's own arguments"
        );
    }

    /// The six posed parts, against vanilla's own book animation update
    /// transcribed by hand.
    ///
    /// `seam` must be absent from the list: the jar never poses it, and its rest
    /// `rotation(0, PI/2, 0)` is the spine's quarter turn — an override with a
    /// zero `y_rot` would flatten it into the covers, which still draws a
    /// plausible book.
    #[test]
    fn the_books_six_poses_match_setup_anim_and_leave_the_seam_alone() {
        let openness = 1.5_f32;
        let poses = book_part_poses(openness, (0.1, 0.9));
        let by_name = |name: &str| {
            poses
                .iter()
                .find(|(n, _, _)| *n == name)
                .copied()
                .unwrap_or_else(|| panic!("{name} is not posed"))
        };

        let slide = openness.sin();
        for (name, expected_y_rot, expected_x) in [
            ("left_lid", std::f32::consts::PI + 1.5, None),
            ("right_lid", -1.5, None),
            ("left_pages", 1.5, Some(slide)),
            ("right_pages", -1.5, Some(slide)),
            // openness - openness*2*flip: 1.5 - 0.3 and 1.5 - 2.7.
            ("flip_page1", 1.2, Some(slide)),
            ("flip_page2", -1.2, Some(slide)),
        ] {
            let (_, y_rot, x) = by_name(name);
            assert!(
                (y_rot - expected_y_rot).abs() < 1e-5,
                "{name}: y_rot {y_rot} != {expected_y_rot}"
            );
            match (x, expected_x) {
                (Some(a), Some(b)) => assert!((a - b).abs() < 1e-6, "{name}: x"),
                (None, None) => {}
                _ => panic!("{name}: x presence"),
            }
        }
        assert!(
            !poses.iter().any(|(n, _, _)| *n == "seam"),
            "the seam is never posed by the jar"
        );

        // The two flip pages must land on opposite sides of the spine — that is
        // what makes a book look mid-turn rather than shut. A transcription that
        // dropped the `* 2` gives 1.35 and 0.15: both positive, same side, and a
        // sign-only assertion would pass.
        let (_, flip1, _) = by_name("flip_page1");
        let (_, flip2, _) = by_name("flip_page2");
        assert!(flip1 > 0.0 && flip2 < 0.0, "{flip1} / {flip2}");
    }

    /// The `67.5°` tilt about **Z** is what makes a lectern book face a reader,
    /// and it is the whole difference from [`block_entity_placement_matrix`].
    ///
    /// Expectation from the transform algebra, not from the implementation: `Ry`
    /// preserves a vector's `y` component, so the angle between the book's own
    /// up axis and world up is exactly the tilt for **every** facing. Reusing
    /// the chest placement matrix gives `0°` — the wrong hypothesis is computed
    /// here and required to be far away, in the same run.
    #[test]
    fn the_books_placement_tilts_it_by_the_jars_angle_at_every_facing() {
        let up = Vec3::Y;
        for facing in ["north", "east", "south", "west"] {
            let yaw = horizontal_facing_clockwise_yaw(facing).unwrap();
            let m = lectern_book_placement_matrix([3, 4, 5], yaw);
            let book_up = m.transform_vector3(up).normalize();
            let angle = book_up.dot(up).clamp(-1.0, 1.0).acos().to_degrees();
            assert!(
                (angle - 67.5).abs() < 1e-3,
                "{facing}: tilt {angle} != 67.5"
            );

            // The wrong hypothesis, in the same run.
            let flat = block_entity_placement_matrix([3, 4, 5], yaw);
            let flat_angle = flat
                .transform_vector3(up)
                .normalize()
                .dot(up)
                .clamp(-1.0, 1.0)
                .acos()
                .to_degrees();
            assert!(flat_angle < 1e-3, "the chest matrix does not tilt at all");
        }

        // The facing really does turn the book: opposite facings must put the
        // book's horizontal lean in opposite directions. A placement that
        // dropped the `Ry` term entirely would satisfy the tilt assertion above
        // at all four facings and fail here.
        let north = lectern_book_placement_matrix(
            [0, 0, 0],
            horizontal_facing_clockwise_yaw("north").unwrap(),
        )
        .transform_vector3(Vec3::Y);
        let south = lectern_book_placement_matrix(
            [0, 0, 0],
            horizontal_facing_clockwise_yaw("south").unwrap(),
        )
        .transform_vector3(Vec3::Y);
        let horizontal = |v: Vec3| Vec3::new(v.x, 0.0, v.z);
        assert!(
            horizontal(north).dot(horizontal(south)) < 0.0,
            "north {north} vs south {south}"
        );
    }

    /// The lectern reaches the batcher, batches on its own key, and the six
    /// overrides really are in the instance's `part_transforms`.
    ///
    /// The last part is the one a "does it draw" check misses: a book whose
    /// overrides were dropped is a *shut* book, which still batches, still
    /// culls, still draws, and still looks like a book from any distance.
    #[test]
    fn a_lectern_batches_on_its_own_key_with_its_overrides_applied() {
        let set = set();
        let mesh = set.get(BOOK).unwrap();
        let lectern = set.resolve_lectern(&LecternSpawn::at([0, 0, 0])).unwrap();
        assert_eq!(lectern.model, BOOK);
        assert_eq!(lectern.texture, BOOK_TEXTURE_STEM);
        assert_eq!(lectern.part_transforms.len(), mesh.parts.len());

        // Rest transforms through the *same* placement, so the only difference
        // between the two is the override list.
        let placement = lectern_book_placement_matrix([0, 0, 0], LecternSpawn::at([0; 3]).facing_yaw_deg);
        let rest = mesh.part_transforms(placement, &[]);
        for name in [
            "left_lid",
            "right_lid",
            "left_pages",
            "right_pages",
            "flip_page1",
            "flip_page2",
        ] {
            let i = mesh.index_of(name).unwrap();
            assert_ne!(
                rest[i], lectern.part_transforms[i],
                "{name} was not posed"
            );
        }
        // …and the seam is, correctly, untouched.
        let seam = mesh.index_of("seam").unwrap();
        assert_eq!(rest[seam], lectern.part_transforms[seam]);

        let chest = set.resolve_chest(&ChestSpawn::at([2, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[chest, lectern],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.drawn, 2);
        assert_eq!(frame.batches.len(), 2, "a book is its own model and sheet");
    }
