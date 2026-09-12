use super::*;

    // ---- world-level admin state: the nine variants un-stranded ------------
    //
    // Each of these drives the event through `fold()` — the real `IngestQueue`
    // plus `run_schedule(NetIngest)` — rather than calling the system. Calling
    // `apply_world_border(...)` directly would pass whether or not the system is
    // registered in `SessionPlugin`, which is one of the two ways this island
    // class forms. (The other half, whether `route()` sends the event here at
    // all, cannot be proven from inside this crate: see
    // `lodestone_client::state`'s `apply_routes_*_through_the_real_path` gates.)

    /// `TabListChanged` needed no new fold — only the route flag. This proves
    /// the pre-existing arm now runs.
    #[test]
    fn tab_list_header_and_footer_reach_the_fold_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        // Precondition, so the assertion below cannot pass on a default.
        assert_eq!(
            app.world().get::<SessionTabList>(entity).unwrap().0.header,
            None,
            "header must start unreported"
        );
        fold(
            &mut app,
            ClientEvent::TabListChanged {
                header: Text::literal("HEADER"),
                footer: Text::literal("FOOTER"),
            },
        );
        let list = &app.world().get::<SessionTabList>(entity).unwrap().0;
        assert_eq!(
            list.header.as_ref().map(Text::to_plain_string),
            Some("HEADER".to_owned())
        );
        assert_eq!(
            list.footer.as_ref().map(Text::to_plain_string),
            Some("FOOTER".to_owned())
        );
    }

    #[test]
    fn world_border_family_reaches_the_fold_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        let before = *app.world().get::<SessionWorldBorder>(entity).unwrap();
        assert!(!before.0.initialized, "must start uninitialised");

        fold(
            &mut app,
            ClientEvent::WorldBorderInitialized {
                x: 100.0,
                z: -200.0,
                old_size: 512.0,
                new_size: 512.0,
                lerp_time_ms: 0,
                absolute_max_size: 29_999_984,
                warning_blocks: 9,
                warning_time: 44,
            },
        );
        let b = app.world().get::<SessionWorldBorder>(entity).unwrap().0;
        assert!(b.initialized);
        assert!((b.center_x - 100.0).abs() < f64::EPSILON);
        assert!((b.center_z + 200.0).abs() < f64::EPSILON);
        assert!((b.target_size() - 512.0).abs() < f64::EPSILON);
        assert_eq!(b.warning_blocks.blocks(), 9);
        assert_eq!(b.warning_time.seconds(), 44);

        // Now each incremental variant, so a missing arm in `apply` cannot hide
        // behind `Initialized` having set everything already.
        fold(
            &mut app,
            ClientEvent::WorldBorderCenterChanged { x: 1.0, z: 2.0 },
        );
        fold(&mut app, ClientEvent::WorldBorderSizeChanged { size: 64.0 });
        fold(
            &mut app,
            ClientEvent::WorldBorderWarningDistanceChanged { warning_blocks: 3 },
        );
        fold(
            &mut app,
            ClientEvent::WorldBorderWarningDelayChanged { warning_time: 7 },
        );
        let b = app.world().get::<SessionWorldBorder>(entity).unwrap().0;
        assert!((b.center_x - 1.0).abs() < f64::EPSILON, "center X arm");
        assert!((b.center_z - 2.0).abs() < f64::EPSILON, "center Z arm");
        assert!((b.target_size() - 64.0).abs() < f64::EPSILON, "size arm");
        assert_eq!(b.warning_blocks.blocks(), 3, "warning distance arm");
        assert_eq!(b.warning_time.seconds(), 7, "warning delay arm");
    }

    /// The resize interpolates on [`crate::FrameClock`], and the value is
    /// *predicted* rather than merely asserted to be between the endpoints.
    ///
    /// The plausible wrong implementation reads the clock at fold time and again
    /// at fold time (so the size never moves) or interpolates on `WorldTime`
    /// ticks (so a 4 s resize completes in 200 ms). At `t = 0.5` the correct
    /// answer is 300.0 and both wrong ones give an endpoint, so the two-sided
    /// assertion separates all three.
    #[test]
    fn a_border_resize_interpolates_on_the_frame_clock() {
        let (mut app, entity) = session_app();
        // `SessionPlugin` alone carries no `FrameClock` (see
        // `apply_world_border`'s docs), so install one and control it by hand.
        app.world_mut().insert_resource(crate::FrameClock {
            secs: 1_000.0,
            ..crate::FrameClock::default()
        });
        fold(
            &mut app,
            ClientEvent::WorldBorderSizeLerping {
                old_size: 200.0,
                new_size: 400.0,
                lerp_time_ms: 4_000,
            },
        );
        let b = app.world().get::<SessionWorldBorder>(entity).unwrap().0;
        assert!(b.is_resizing(), "a differing-endpoint lerp must be Moving");
        // Stamped at fold time, so t=0 here.
        assert!(
            (b.size_at(1_000.0) - 200.0).abs() < 1e-9,
            "at the stamp instant the border still reads its old size, got {}",
            b.size_at(1_000.0)
        );
        let mid = b.size_at(1_002.0);
        assert!(
            (mid - 300.0).abs() < 1e-9,
            "half way through a 4s resize must read the predicted 300.0, got {mid}"
        );
        assert!(
            (mid - 400.0).abs() > 50.0,
            "and must not have completed early (the tick/ms confusion), got {mid}"
        );
        assert!((b.size_at(1_004.0) - 400.0).abs() < 1e-9, "complete at t=1");
    }

    #[test]
    fn spawn_position_reaches_the_fold_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        assert!(
            !app.world()
                .get::<SessionSpawnPoint>(entity)
                .unwrap()
                .0
                .is_reported(),
            "spawn must start unreported, or the assertion below is vacuous"
        );
        fold(
            &mut app,
            ClientEvent::SpawnPositionChanged {
                dimension: dim("overworld"),
                pos: lodestone_model::math::BlockPos::new(-48, 71, 300),
                angle: 180.0,
                pitch: 0.0,
            },
        );
        let sp = &app.world().get::<SessionSpawnPoint>(entity).unwrap().0;
        assert!(sp.is_reported());
        assert_eq!(
            sp.pos(),
            Some(lodestone_model::math::BlockPos::new(-48, 71, 300))
        );
    }

    #[test]
    fn game_rules_reach_the_fold_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        assert!(
            app.world()
                .get::<SessionGameRules>(entity)
                .unwrap()
                .0
                .is_empty(),
            "rules must start empty"
        );
        fold(
            &mut app,
            ClientEvent::GameRulesChanged {
                values: vec![
                    (
                        "minecraft:immediate_respawn".parse().unwrap(),
                        "true".to_owned(),
                    ),
                    (
                        "minecraft:random_tick_speed".parse().unwrap(),
                        "12".to_owned(),
                    ),
                ],
            },
        );
        let rules = &app.world().get::<SessionGameRules>(entity).unwrap().0;
        assert_eq!(rules.immediate_respawn(), Some(true));
        assert_eq!(rules.random_tick_speed(), Some(12));
        // Absence stays distinct from false even after a real fold.
        assert_eq!(rules.bool_rule("minecraft:keep_inventory"), None);
    }

    /// Every book gets a *distinct* pair, so a fold that mapped all four to one
    /// value — or transposed two of them — could not pass.
    #[test]
    fn recipe_book_settings_reach_the_fold_through_the_real_schedule() {
        use lodestone_model::{RecipeBookType, RecipeBookTypeSettings};
        let (mut app, entity) = session_app();
        assert!(
            !app.world()
                .get::<SessionRecipeBookSettings>(entity)
                .unwrap()
                .0
                .reported,
            "precondition: unreported"
        );
        fold(
            &mut app,
            ClientEvent::RecipeBookSettingsChanged {
                crafting: RecipeBookTypeSettings { open: true, filtering: false },
                furnace: RecipeBookTypeSettings { open: false, filtering: true },
                blast_furnace: RecipeBookTypeSettings { open: true, filtering: true },
                smoker: RecipeBookTypeSettings { open: false, filtering: false },
            },
        );
        let s = app
            .world()
            .get::<SessionRecipeBookSettings>(entity)
            .unwrap()
            .0;
        assert!(s.reported);
        assert_eq!(
            s.for_type(RecipeBookType::Crafting),
            RecipeBookTypeSettings { open: true, filtering: false }
        );
        assert_eq!(
            s.for_type(RecipeBookType::Furnace),
            RecipeBookTypeSettings { open: false, filtering: true },
            "furnace must not pick up crafting's pair"
        );
        assert_eq!(
            s.for_type(RecipeBookType::BlastFurnace),
            RecipeBookTypeSettings { open: true, filtering: true }
        );
        assert_eq!(
            s.for_type(RecipeBookType::Smoker),
            RecipeBookTypeSettings { open: false, filtering: false }
        );
    }

    /// The reset path. `insert_session_components` is what
    /// `Sim::end_session` calls, so this proves a quit-to-title cannot carry a
    /// previous server's border into the next session — the one failure mode of
    /// these three components with a visible symptom.
    #[test]
    fn quit_to_title_clears_the_world_border_spawn_and_rules() {
        let (mut app, entity) = session_app();
        fold(
            &mut app,
            ClientEvent::WorldBorderInitialized {
                x: 5_000.0,
                z: 5_000.0,
                old_size: 32.0,
                new_size: 32.0,
                lerp_time_ms: 0,
                absolute_max_size: 29_999_984,
                warning_blocks: 1,
                warning_time: 1,
            },
        );
        fold(
            &mut app,
            ClientEvent::SpawnPositionChanged {
                dimension: dim("overworld"),
                pos: lodestone_model::math::BlockPos::new(9, 9, 9),
                angle: 0.0,
                pitch: 0.0,
            },
        );
        // Precondition: the state really is dirty before the reset, so a reset
        // that did nothing could not pass.
        assert!(
            app.world()
                .get::<SessionWorldBorder>(entity)
                .unwrap()
                .0
                .initialized
        );
        assert!(
            app.world()
                .get::<SessionSpawnPoint>(entity)
                .unwrap()
                .0
                .is_reported()
        );

        insert_session_components(app.world_mut(), entity);

        let b = app.world().get::<SessionWorldBorder>(entity).unwrap().0;
        assert!(!b.initialized, "a new session must not inherit a border");
        assert!((b.center_x).abs() < f64::EPSILON);
        assert!(
            !app.world()
                .get::<SessionSpawnPoint>(entity)
                .unwrap()
                .0
                .is_reported(),
            "a new session must not inherit a spawn point"
        );
    }
