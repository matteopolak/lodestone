use super::*;

    // ---- the local player's server-reported state -------------------------

    /// The whole vitals collapse in one test: five event families, six
    /// components, one fold, reached **through the schedule**.
    #[test]
    fn the_local_players_server_state_folds_onto_components() {
        let (mut app, entity) = session_app();

        // Everything is unknown before the server says anything — the state the
        // offline fixture world draws no bars from.
        {
            let world = app.world();
            assert_eq!(world.get::<Vitals>(entity).unwrap().health, None);
            assert_eq!(world.get::<Xp>(entity).unwrap().0, None);
            assert_eq!(world.get::<ServerEntityId>(entity).unwrap().0, None);
            assert_eq!(world.get::<ServerGameMode>(entity).unwrap().0, None);
            assert_eq!(world.get::<ServerDimension>(entity).unwrap().0, None);
            assert!(
                world.get::<ServerAlive>(entity).unwrap().0,
                "a client nobody has told otherwise is alive"
            );
        }

        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 7,
                game_mode: GameMode::Creative,
                dimension: dim("overworld"),
            },
        );
        fold(
            &mut app,
            ClientEvent::HealthChanged {
                health: 18.0,
                food: 15,
                saturation: 2.5,
            },
        );
        fold(
            &mut app,
            ClientEvent::ExperienceChanged {
                progress: 0.375,
                level: 12,
                total: 289,
            },
        );

        let world = app.world();
        let vitals = *world.get::<Vitals>(entity).unwrap();
        assert_eq!(vitals.health, Some(18.0));
        assert_eq!(vitals.food, Some(15));
        assert_eq!(vitals.saturation, Some(2.5));
        assert_eq!(world.get::<Xp>(entity).unwrap().0, Some((0.375, 12, 289)));
        assert_eq!(world.get::<ServerEntityId>(entity).unwrap().0, Some(7));
        assert_eq!(
            world.get::<ServerGameMode>(entity).unwrap().0,
            Some(GameMode::Creative)
        );
        assert_eq!(
            world.get::<ServerDimension>(entity).unwrap().0,
            Some(dim("overworld"))
        );
    }

    /// `Respawned` moves the dimension, because it is how the server reports a
    /// **portal trip** and not only a death. A fold that only handled `Login`
    /// froze `dimension` at whatever the player logged into — the too-bright-Nether
    /// bug reached by traversal.
    ///
    /// The `Login` assertion is the control: it proves the field is genuinely
    /// rewritten rather than having started out as the expected value.
    #[test]
    fn a_respawn_moves_the_dimension_and_revives() {
        let (mut app, entity) = session_app();
        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 7,
                game_mode: GameMode::Survival,
                dimension: dim("overworld"),
            },
        );
        assert_eq!(
            app.world().get::<ServerDimension>(entity).unwrap().0,
            Some(dim("overworld"))
        );

        fold(
            &mut app,
            ClientEvent::Death {
                message: Text::literal("you died"),
            },
        );
        assert!(!app.world().get::<ServerAlive>(entity).unwrap().0);

        fold(
            &mut app,
            ClientEvent::Respawned {
                dimension: dim("the_nether"),
                game_mode: GameMode::Adventure,
                previous_game_mode: Some(GameMode::Survival),
                last_death_location: None,
            },
        );
        let world = app.world();
        assert_eq!(
            world.get::<ServerDimension>(entity).unwrap().0,
            Some(dim("the_nether")),
            "a respawn/portal trip must move the dimension"
        );
        assert_eq!(
            world.get::<ServerGameMode>(entity).unwrap().0,
            Some(GameMode::Adventure)
        );
        assert!(
            world.get::<ServerAlive>(entity).unwrap().0,
            "a respawn is exactly when the player stops being dead"
        );
    }

    /// A `World` carrying **both** halves of the fold — the session scalars and
    /// the per-entity ingest that owns [`Vitals::air`]/[`Vitals::on_fire`].
    ///
    /// [`session_app`] alone cannot express the respawn-clears-vitals case: `air` and `on_fire` are
    /// written only by `crate::ingest::apply_local_player_air_supply` /
    /// `apply_local_player_on_fire`, which are registered by `IngestPlugin`. A
    /// test that reached the drowned state by assigning `Vitals` directly would
    /// be asserting against a state the fold might never be able to produce —
    /// so both plugins go on, and every value below arrives as an event.
    fn drowning_app() -> (App, bevy_ecs::entity::Entity) {
        let mut app = App::new();
        app.add_plugins(SessionPlugin);
        app.add_plugins(crate::ingest::IngestPlugin);
        let entity = spawn_session(app.world_mut());
        (app, entity)
    }

    /// Drown, die, respawn: `EntityMetadataUpdate` naming our own id.
    fn air_and_fire(entity_id: i32, air: i32, on_fire: bool) -> ClientEvent {
        ClientEvent::EntityMetadataUpdated {
            entity_id,
            metadata: lodestone_model::EntityMetadataUpdate {
                air_supply: Some(air),
                flags: Some(if on_fire { 0x01 } else { 0x00 }),
                ..Default::default()
            },
        }
    }

    /// **A respawn clears the two entity-metadata-fed vitals.**
    ///
    /// Reported from play — after drowning, the bubble row rendered *completely
    /// empty* until the server's next metadata packet arrived with 300, which is
    /// what the player saw as an instant refill on touching water. Nothing wrote
    /// `air` on `Respawned`, so the dead entity's last reading (`0`) survived
    /// into the new life.
    ///
    /// `None`, not `Some(300)`: `None` is the documented "no reading yet" state
    /// and reads as full downstream, so the row stays hidden until the server
    /// says otherwise rather than us inventing a number.
    ///
    /// `on_fire` is checked in the same pass because it has the identical shape
    /// and the *quieter* polarity — its absence reads as `false`, so a stale
    /// `Some(true)` paints the fire overlay on a freshly respawned player with
    /// nothing to make the failure obvious.
    ///
    /// Every intermediate assertion here is a control: the drowned/burning state
    /// is asserted **before** the respawn, so a `None` afterwards is a clearing
    /// and not a value that was never set. See
    /// `a_respawn_does_not_clear_air_and_fire_for_a_still_drowning_player` for
    /// the other direction.
    #[test]
    fn a_respawn_clears_the_drowned_air_supply_and_the_burning_flag() {
        let (mut app, entity) = drowning_app();
        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 7,
                game_mode: GameMode::Survival,
                dimension: dim("overworld"),
            },
        );
        assert_eq!(
            app.world().get::<Vitals>(entity).unwrap().air,
            None,
            "nothing has reported air yet — the join state the row must stay hidden for"
        );

        // Drown to zero, on fire on the way down (a burning player who then
        // drowns is unusual but it is the state that proves both fields move).
        fold(&mut app, air_and_fire(7, 0, true));
        let drowned = *app.world().get::<Vitals>(entity).unwrap();
        assert_eq!(
            drowned.air,
            Some(0),
            "control: the fold must actually reach zero, or the clear below asserts nothing"
        );
        assert_eq!(drowned.on_fire, Some(true), "control: and the flag must be set");

        fold(
            &mut app,
            ClientEvent::Death {
                message: Text::literal("drowned"),
            },
        );
        assert_eq!(
            app.world().get::<Vitals>(entity).unwrap().air,
            Some(0),
            "control: the death packet alone must NOT clear it — that would make the \
             respawn arm below untestable by hiding which packet does the work"
        );

        fold(
            &mut app,
            ClientEvent::Respawned {
                dimension: dim("overworld"),
                game_mode: GameMode::Survival,
                previous_game_mode: Some(GameMode::Survival),
                last_death_location: None,
            },
        );
        let after = *app.world().get::<Vitals>(entity).unwrap();
        assert_eq!(
            after.air, None,
            "#390: a respawn is a brand-new entity on both sides — the drowned \
             reading must not survive it, or the bubble row draws empty until the \
             server's next metadata packet"
        );
        assert_eq!(
            after.on_fire, None,
            "#390's sibling: a stale burning flag paints the fire overlay on a \
             player who just respawned"
        );
    }

    /// **The other control.** The clear must be keyed to `Respawned` and nothing
    /// else: a still-drowning player who has not died keeps their reading, so the
    /// assertion above is not satisfied by a fold that clears `air` on every
    /// event (or by one that never lets it hold a value at all).
    ///
    /// A portal trip *does* clear, and that is correct rather than a false
    /// positive: `Respawned` is the same packet, vanilla builds the same new
    /// `LocalPlayer` for it, and 300-on-arrival is exactly what the server then
    /// reports.
    #[test]
    fn a_respawn_does_not_clear_air_and_fire_for_a_still_drowning_player() {
        let (mut app, entity) = drowning_app();
        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 7,
                game_mode: GameMode::Survival,
                dimension: dim("overworld"),
            },
        );
        fold(&mut app, air_and_fire(7, 120, true));

        // Everything a drowning player's tick actually carries, short of dying.
        fold(
            &mut app,
            ClientEvent::HealthChanged {
                health: 6.0,
                food: 17,
                saturation: 0.0,
            },
        );
        fold(
            &mut app,
            ClientEvent::GameModeChanged {
                game_mode: GameMode::Survival,
            },
        );
        let mid = *app.world().get::<Vitals>(entity).unwrap();
        assert_eq!(
            mid.air,
            Some(120),
            "an un-respawned player must keep their air reading — otherwise the \
             bubble row could never draw at all and #390's assertion is vacuous"
        );
        assert_eq!(mid.on_fire, Some(true));
    }

    /// A dimension type as the adapter builds it from `registry_data`.
    fn dim_type(name: &str, has_skylight: bool) -> DimensionTypeInfo {
        DimensionTypeInfo {
            name: format!("minecraft:{name}").parse().expect("valid key"),
            has_skylight,
            has_ceiling: !has_skylight,
            has_fixed_time: !has_skylight,
            coordinate_scale: if has_skylight { 1.0 } else { 8.0 },
            min_y: if has_skylight { -64 } else { 0 },
            height: if has_skylight { 384 } else { 256 },
            logical_height: if has_skylight { 384 } else { 128 },
            ambient_light: if has_skylight { 0.0 } else { 0.1 },
            // Not this fixture's concern — these tests are about the
            // dimension type reaching the ECS through the schedule, not about
            // the colour itself. See `lodestone_render::light`'s tests for
            // that.
            ambient_light_color: None,
        }
    }

    /// The registry-driven dimension type must reach
    /// [`ServerDimensionType`] **through the schedule**, and must move on a
    /// portal trip the same way [`ServerDimension`] does.
    ///
    /// The pre-login assertion is the control: it proves the component starts
    /// `None` and is genuinely written, rather than having happened to hold the
    /// expected value all along.
    #[test]
    fn a_net_ingest_run_folds_the_registry_dimension_type_and_a_portal_trip_moves_it() {
        let (mut app, entity) = session_app();
        assert_eq!(
            app.world().get::<ServerDimensionType>(entity).unwrap().info,
            None,
            "pre-login there is no dimension type — not a defaulted overworld"
        );

        fold(
            &mut app,
            ClientEvent::DimensionTypeChanged {
                holder_id: 0,
                dimension_type: Some(dim_type("overworld", true)),
                is_flat: false,
            },
        );
        let folded = app
            .world()
            .get::<ServerDimensionType>(entity)
            .unwrap()
            .info
            .clone()
            .expect("the fold must reach the component");
        assert!(folded.has_skylight);
        assert_eq!(folded.min_y, -64);
        assert_eq!(folded.height, 384);

        // A portal trip: the adapter emits this off `respawn`'s own dimension-type
        // holder id, immediately before `Respawned`.
        fold(
            &mut app,
            ClientEvent::DimensionTypeChanged {
                holder_id: 3,
                dimension_type: Some(dim_type("the_nether", false)),
                is_flat: false,
            },
        );
        let nether = app
            .world()
            .get::<ServerDimensionType>(entity)
            .unwrap()
            .info
            .clone()
            .expect("a portal trip must install the new type");
        assert!(
            !nether.has_skylight,
            "the Nether's has_skylight is the value the mesher's sky default reads"
        );
        assert_eq!(nether.logical_height, 128);

        // An unresolvable dimension **clears** rather than keeping the last one:
        // a stale `has_skylight` renders a dark dimension lit, which is worse
        // than an honest `None` that makes the consumer state its fallback.
        fold(
            &mut app,
            ClientEvent::DimensionTypeChanged {
                holder_id: 99,
                dimension_type: None,
                is_flat: false,
            },
        );
        assert_eq!(
            app.world().get::<ServerDimensionType>(entity).unwrap().info,
            None,
            "an unresolved holder id must clear the previous dimension type"
        );
    }

    /// `ServerAlive` tracks `health > 0.0` as well as the death packet, and that
    /// is the *whole* reason it cannot merge with `crate::player::Dead` — which is
    /// set only by `Death`, and only when the driver's `recover_from_death` switch
    /// allows it. Both directions are asserted here, because a fold that only
    /// handled `Death` would pass a one-sided version of this.
    #[test]
    fn zero_health_kills_and_positive_health_revives_without_a_death_packet() {
        let (mut app, entity) = session_app();
        let health = |app: &mut App, health: f32| {
            fold(
                app,
                ClientEvent::HealthChanged {
                    health,
                    food: 0,
                    saturation: 0.0,
                },
            );
        };

        health(&mut app, 0.0);
        assert!(
            !app.world().get::<ServerAlive>(entity).unwrap().0,
            "health reaching zero must clear liveness even with no Death packet"
        );
        health(&mut app, 20.0);
        assert!(
            app.world().get::<ServerAlive>(entity).unwrap().0,
            "…and positive health must restore it, again with no packet"
        );
        assert!(
            !app.world().entity(entity).contains::<crate::player::Dead>(),
            "and none of that may insert the driver's `Dead` marker — health \
             reaching zero is not a session event"
        );
    }
