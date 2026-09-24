use super::*;

    // ---- the consolidated coverage table -----------------------------------

    /// The `session`-side twin of
    /// `crate::ingest::tests::handles_event_covers_exactly_the_variants_with_a_system`.
    ///
    /// Until this test, `ingest` had a single table enumerating every variant
    /// it claims and proving a system runs behind each one; `session` had the
    /// same route-table guarantee (a variant is a compile error until routed)
    /// but no analogous *runtime* enumeration — coverage of individual
    /// families was scattered across many separate tests, so a future variant
    /// routed `session: true` with no fold could compile, leave every existing
    /// test green, and still drop silently. This is that missing table.
    ///
    /// Every arm below is `assert!(handles_event(...))`, i.e. the routing
    /// claim; the comment beside each names the test elsewhere in this module
    /// that proves a *system* actually consumes it through the real schedule
    /// (`fold()` + `NetIngest`), which is the half `handles_event` alone
    /// cannot prove.
    #[test]
    fn handles_event_covers_exactly_the_session_claimed_variants() {
        // Login / Respawned / Death — apply_local_player_state; see
        // `respawning_clears_the_ride_state` and the vitals tests above.
        assert!(handles_event(&ClientEvent::Login {
            entity_id: 1,
            game_mode: GameMode::Survival,
            dimension: dim("overworld"),
        }));
        assert!(handles_event(&ClientEvent::Respawned {
            dimension: dim("overworld"),
            game_mode: GameMode::Survival,
            previous_game_mode: None,
            last_death_location: None,
        }));
        assert!(handles_event(&ClientEvent::Death {
            message: Text::literal("died")
        }));
        // HealthChanged / ExperienceChanged / GameModeChanged / AbilitiesChanged
        // / DimensionTypeChanged / BiomeVisuals — apply_local_player_state; see
        // the dedicated vitals/xp/abilities/dimension-type/biome tests above.
        assert!(handles_event(&ClientEvent::HealthChanged {
            health: 20.0,
            food: 20,
            saturation: 5.0,
        }));
        assert!(handles_event(&ClientEvent::ExperienceChanged {
            progress: 0.0,
            level: 0,
            total: 0,
        }));
        assert!(handles_event(&ClientEvent::GameModeChanged {
            game_mode: GameMode::Creative,
        }));
        assert!(handles_event(&ClientEvent::AbilitiesChanged {
            invulnerable: false,
            flying: false,
            can_fly: false,
            instabuild: false,
            flying_speed: 0.05,
            walking_speed: 0.1,
        }));
        assert!(handles_event(&ClientEvent::DimensionTypeChanged {
            holder_id: 0,
            dimension_type: None,
            is_flat: false,
        }));
        assert!(handles_event(&ClientEvent::BiomeVisuals {
            sky_colors: Vec::new(),
        }));
        // HeldSlotChanged / DifficultyChanged / SimulationDistanceChanged —
        // local session scalars; see the dedicated schedule tests below.
        assert!(handles_event(&ClientEvent::HeldSlotChanged { slot: 0 }));
        assert!(handles_event(&ClientEvent::DifficultyChanged {
            difficulty: Difficulty::Normal,
            locked: false,
        }));
        assert!(handles_event(&ClientEvent::SimulationDistanceChanged { distance: 8 }));
        // BlockDestruction — apply_block_destruction; see the dedicated tests
        // above this one.
        assert!(handles_event(&ClientEvent::BlockDestruction {
            entity_id: 1,
            pos: lodestone_model::math::BlockPos::new(0, 0, 0),
            progress: 0,
        }));
        // EntityPassengersChanged — claimed by *both* routers; the ingest-side
        // assertion pinning that is
        // `ingest::tests::handles_event_covers_exactly_the_variants_with_a_system`.
        assert!(handles_event(&ClientEvent::EntityPassengersChanged {
            vehicle_id: 1,
            passenger_ids: vec![2],
        }));
        // Scoreboard family — apply_scoreboard; see
        // `a_net_ingest_run_folds_a_scoreboard_objective_onto_the_component` and
        // `score_reset_and_team_update_reach_the_scoreboard_through_the_real_schedule`.
        assert!(handles_event(&ClientEvent::ObjectiveUpdate {
            name: "kills".into(),
            mode: ObjectiveMode::Add,
            display_name: None,
            render_type: None,
            number_format: None,
        }));
        assert!(handles_event(&ClientEvent::DisplayObjective {
            slot: DisplaySlot::Sidebar,
            objective: None,
        }));
        assert!(handles_event(&ClientEvent::ScoreUpdate {
            holder: "Alice".into(),
            objective: "kills".into(),
            value: 0,
            display: None,
            number_format: None,
        }));
        assert!(handles_event(&ClientEvent::ScoreReset {
            holder: "Alice".into(),
            objective: None,
        }));
        assert!(handles_event(&ClientEvent::TeamUpdate {
            name: "red".into(),
            action: TeamAction::Remove,
        }));
        // Tab list / boss bars — apply_tab_list / apply_boss_bars; see
        // `a_player_who_leaves_is_removed_from_the_tab_list` and
        // `a_boss_bar_add_reaches_the_component_in_render_order`.
        assert!(handles_event(&ClientEvent::PlayerListUpdate {
            entries: Vec::new()
        }));
        assert!(handles_event(&ClientEvent::PlayerListRemove {
            profile_ids: Vec::new()
        }));
        assert!(handles_event(&ClientEvent::PlayerListRemoveByName {
            profile_names: Vec::new()
        }));
        assert!(handles_event(&ClientEvent::BossBarUpdate {
            id: Uuid::from_u128(1),
            action: BossAction::Remove,
        }));
        // Menu family — apply_menus; see
        // `menu_family_events_reach_session_menus_through_the_real_schedule`.
        assert!(handles_event(&ClientEvent::ScreenOpened {
            window_id: 1,
            menu_type: key("minecraft:generic_9x1"),
            title: Text::literal("T"),
        }));
        assert!(handles_event(&ClientEvent::MountScreenOpened {
            container_id: 1,
            inventory_columns: 3,
            entity_id: 7,
        }));
        assert!(handles_event(&ClientEvent::ScreenClosed { window_id: 1 }));
        assert!(handles_event(&ClientEvent::ContainerContent {
            window_id: 1,
            state_id: lodestone_model::ContainerStateId::new(1),
            items: Vec::new(),
            carried_item: None,
        }));
        assert!(handles_event(&ClientEvent::ContainerSlot {
            window_id: 1,
            state_id: lodestone_model::ContainerStateId::new(1),
            slot: 0,
            item: None,
        }));
        assert!(handles_event(&ClientEvent::ContainerData {
            window_id: 1,
            property: 0,
            value: 0,
        }));
        assert!(handles_event(&ClientEvent::CursorItemChanged { item: None }));
        assert!(handles_event(&ClientEvent::InventorySlotChanged {
            slot: 0,
            item: None,
        }));
        // TabListChanged — apply_tab_list; see
        // `tab_list_header_and_footer_reach_the_fold_through_the_real_schedule`.
        // The fold arm predates the route flag: this variant was decoded and
        // foldable and simply never asked for.
        assert!(handles_event(&ClientEvent::TabListChanged {
            header: Text::literal("H"),
            footer: Text::literal("F"),
        }));
        // World-border family — apply_world_border; see
        // `world_border_family_reaches_the_fold_through_the_real_schedule` and
        // `a_border_resize_interpolates_on_the_frame_clock`.
        assert!(handles_event(&ClientEvent::WorldBorderCenterChanged {
            x: 0.0,
            z: 0.0,
        }));
        assert!(handles_event(&ClientEvent::WorldBorderSizeLerping {
            old_size: 1.0,
            new_size: 2.0,
            lerp_time_ms: 1,
        }));
        assert!(handles_event(&ClientEvent::WorldBorderSizeChanged { size: 1.0 }));
        assert!(handles_event(&ClientEvent::WorldBorderWarningDelayChanged {
            warning_time: 1,
        }));
        assert!(handles_event(
            &ClientEvent::WorldBorderWarningDistanceChanged { warning_blocks: 1 }
        ));
        assert!(handles_event(&ClientEvent::WorldBorderInitialized {
            x: 0.0,
            z: 0.0,
            old_size: 1.0,
            new_size: 1.0,
            lerp_time_ms: 0,
            absolute_max_size: 1,
            warning_blocks: 1,
            warning_time: 1,
        }));
        // SpawnPositionChanged — apply_spawn_point; see
        // `spawn_position_reaches_the_fold_through_the_real_schedule`.
        assert!(handles_event(&ClientEvent::SpawnPositionChanged {
            dimension: dim("overworld"),
            pos: lodestone_model::math::BlockPos::new(0, 0, 0),
            angle: 0.0,
            pitch: 0.0,
        }));
        // GameRulesChanged — apply_game_rules; see
        // `game_rules_reach_the_fold_through_the_real_schedule`.
        assert!(handles_event(&ClientEvent::GameRulesChanged {
            values: Vec::new(),
        }));
        // RecipeBookSettingsChanged — apply_recipe_book_settings; see
        // `recipe_book_settings_reach_the_fold_through_the_real_schedule`.
        assert!(handles_event(&ClientEvent::RecipeBookSettingsChanged {
            crafting: lodestone_model::RecipeBookTypeSettings::default(),
            furnace: lodestone_model::RecipeBookTypeSettings::default(),
            blast_furnace: lodestone_model::RecipeBookTypeSettings::default(),
            smoker: lodestone_model::RecipeBookTypeSettings::default(),
        }));
        // MapItemData — apply_maps.
        assert!(handles_event(&ClientEvent::MapItemData {
            map_id: 1,
            scale: 0,
            locked: false,
            decorations: None,
            color_patch: None,
        }));
        // AdvancementsUpdated — apply_advancements.
        assert!(handles_event(&ClientEvent::AdvancementsUpdated {
            reset: true,
            added: Vec::new(),
            removed: Vec::new(),
            progress: Vec::new(),
            show_advancements: true,
        }));

        // ---- the negative controls: ingest-only and client-only events ----
        //
        // Mirrors `ingest`'s own cross-checks, from the other side: an event
        // this module must NOT claim, or a router-fork mistake (the exact class
        // `CLAUDE.md` records costing work twice) compiles and drops silently.
        let entity_moved = ClientEvent::EntityMoved {
            entity_id: 1,
            movement: lodestone_model::event::EntityMovement::Absolute(lodestone_model::Vec3::new(
                0.0, 0.0, 0.0,
            )),
            rotation: None,
            on_ground: true,
        };
        assert!(
            !handles_event(&entity_moved),
            "per-entity ECS state belongs to ingest, not session"
        );
        assert!(
            crate::ingest::handles_event(&entity_moved),
            "…and ingest must actually claim it, or the event reaches neither router"
        );
        assert!(
            !handles_event(&ClientEvent::KeepAlive { id: 1 }),
            "KeepAlive is answered inside lodestone-client, not by a session fold"
        );
    }
