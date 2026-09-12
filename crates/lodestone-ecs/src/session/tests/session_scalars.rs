use super::*;

    /// **Exactly one system writes each session component.**
    ///
    /// `docs/bevy-migration.md` Stage 3 asks for this specifically, because the
    /// stage exists to delete a duplicate fold and the cheapest way to grow a new
    /// one is two systems writing one component with no ordering between them.
    /// `ambiguity_detection: LogLevel::Error` turns that into a schedule *build
    /// failure* rather than a race whose outcome depends on registration order.
    ///
    /// azalea logs the same check at `Warn` (`AmbiguityLoggerPlugin`,
    /// `azalea-client/src/client.rs`); an error is right here because the
    /// invariant is the point of the stage, not a diagnostic.
    ///
    /// The `AbilitiesChanged` routing control. The decode has been correct since v26-2 landed and
    /// the event still reached **nothing**, because `SharedState::apply` forwards
    /// only what `ingest::handles_event` or [`handles_event`] lists. This pair —
    /// "someone claims it, and it is the right someone" — is the check that has
    /// caught this exact island four times now.
    #[test]
    fn abilities_changed_is_claimed_by_this_module_and_not_by_ingest() {
        let event = ClientEvent::AbilitiesChanged {
            invulnerable: false,
            flying: true,
            can_fly: true,
            instabuild: true,
            flying_speed: 0.05,
            walking_speed: 0.1,
        };
        assert!(
            handles_event(&event),
            "without this arm the abilities packet is decoded into nothing at all"
        );
        assert!(
            !crate::ingest::handles_event(&event),
            "abilities are a session scalar, not per-entity ingest; two claimants \
             would be two folds of one event"
        );

        // Same pair for `GameModeChanged`, whose absence froze `ServerGameMode` at
        // whatever the player logged in as.
        let mode = ClientEvent::GameModeChanged {
            game_mode: GameMode::Creative,
        };
        assert!(handles_event(&mode));
        assert!(!crate::ingest::handles_event(&mode));
    }

    #[test]
    fn entity_status_reaches_the_session_fold_for_local_permissions() {
        let event = ClientEvent::EntityStatus {
            entity_id: 7,
            status: 26,
        };
        assert!(
            crate::ingest::handles_event(&event),
            "the existing per-entity status fold must keep receiving death events"
        );
        assert!(
            handles_event(&event),
            "the local-player permission meaning of status 24..28 needs the session fold"
        );
    }

    #[test]
    fn local_entity_statuses_fold_permission_level_without_claiming_other_entities() {
        let (mut app, entity) = session_app();
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
            ClientEvent::EntityStatus {
                entity_id: 7,
                status: 26,
            },
        );
        assert_eq!(app.world().get::<ServerPermissionLevel>(entity).unwrap().0, 2);

        fold(
            &mut app,
            ClientEvent::EntityStatus {
                entity_id: 7,
                status: 25,
            },
        );
        assert_eq!(app.world().get::<ServerPermissionLevel>(entity).unwrap().0, 1);

        fold(
            &mut app,
            ClientEvent::EntityStatus {
                entity_id: 7,
                status: 27,
            },
        );
        assert_eq!(app.world().get::<ServerPermissionLevel>(entity).unwrap().0, 3);

        fold(
            &mut app,
            ClientEvent::EntityStatus {
                entity_id: 8,
                status: 28,
            },
        );
        assert_eq!(
            app.world().get::<ServerPermissionLevel>(entity).unwrap().0,
            3,
            "a non-local entity's status cannot grant local permissions"
        );

        fold(
            &mut app,
            ClientEvent::EntityStatus {
                entity_id: 7,
                status: 24,
            },
        );
        assert_eq!(app.world().get::<ServerPermissionLevel>(entity).unwrap().0, 0);

        fold(
            &mut app,
            ClientEvent::EntityStatus {
                entity_id: 7,
                status: 28,
            },
        );
        fold(
            &mut app,
            ClientEvent::Respawned {
                dimension: dim("overworld"),
                game_mode: GameMode::Creative,
                previous_game_mode: Some(GameMode::Creative),
                last_death_location: None,
            },
        );
        assert_eq!(
            app.world().get::<ServerPermissionLevel>(entity).unwrap().0,
            0,
            "a respawn replaces vanilla's LocalPlayer and must not retain permissions"
        );
    }

    /// The `BiomeVisuals` routing control, the fifth instance of the same pair.
    ///
    /// The per-biome sky tint was blocked for two sessions on exactly one missing
    /// link: the decoded colours never crossed the version-free seam. Once they
    /// do, the next place they can vanish is this switch — `SharedState::apply`
    /// forwards only what `ingest::handles_event` or [`handles_event`] lists, and
    /// an event neither claims falls through to the dead legacy scalar fallback.
    /// Registry data is a session-scoped scalar, so `ingest` must **not** claim
    /// it; guessing `ingest` for a session fact has misrouted work twice.
    #[test]
    fn biome_visuals_is_claimed_by_this_module_and_not_by_ingest() {
        let event = ClientEvent::BiomeVisuals {
            sky_colors: vec![Some(0x0078_a7ff), None],
        };
        assert!(
            handles_event(&event),
            "without this arm the biome registry is decoded into nothing at all and the \
             sky tint reaches zero pixels"
        );
        assert!(
            !crate::ingest::handles_event(&event),
            "biome registry data is a session scalar, not per-entity ingest"
        );
    }

    /// The biome sky-colour table must reach
    /// [`ServerBiomeSkyColors`] **through the schedule**, keep its holder-id
    /// indexing, and be *replaced* rather than merged.
    ///
    /// The pre-login assertion is the control: it proves the component starts
    /// empty and is genuinely written, rather than having happened to hold the
    /// expected value all along.
    #[test]
    fn a_net_ingest_run_folds_the_biome_sky_colours_and_a_resend_replaces_them() {
        let (mut app, entity) = session_app();
        assert!(
            app.world()
                .get::<ServerBiomeSkyColors>(entity)
                .unwrap()
                .0
                .is_empty(),
            "pre-login there is no biome table — not a defaulted overworld blue"
        );

        // Real 26.2 values at deliberately non-trivial holder ids: a colourless
        // entry first, so an implementation that skipped `None`s would shift
        // every later colour by one and still look plausible on screen.
        fold(
            &mut app,
            ClientEvent::BiomeVisuals {
                sky_colors: vec![None, Some(0x00b9_b9b9), Some(0x006e_b1ff)],
            },
        );
        let folded = app
            .world()
            .get::<ServerBiomeSkyColors>(entity)
            .unwrap()
            .0
            .clone();
        assert_eq!(
            &*folded,
            [None, Some(0x00b9_b9b9), Some(0x006e_b1ff)],
            "the fold must reach the component with holder ids intact"
        );

        // Re-entering configuration resends the registries. Appending would put
        // the new table's biomes at holder ids 3.. while the stale ones kept
        // answering 0.., which is the same failure `ClientRegistries::apply`
        // guards against one layer down.
        fold(
            &mut app,
            ClientEvent::BiomeVisuals {
                sky_colors: vec![Some(0x0085_9dff)],
            },
        );
        assert_eq!(
            &*app
                .world()
                .get::<ServerBiomeSkyColors>(entity)
                .unwrap()
                .0
                .clone(),
            [Some(0x0085_9dff)],
            "a resent registry replaces the table"
        );

        // And an empty table clears it: a server switch that sends no biome
        // registry has to stop tinting, not keep painting the last world's sky.
        fold(
            &mut app,
            ClientEvent::BiomeVisuals {
                sky_colors: Vec::new(),
            },
        );
        assert!(
            app.world()
                .get::<ServerBiomeSkyColors>(entity)
                .unwrap()
                .0
                .is_empty(),
            "an empty table must clear, not preserve"
        );
    }

    /// The fold itself, **through the schedule** rather than by calling the system
    /// directly — a hermetic call passes whether or not the routing arm above
    /// exists, which is precisely why the island survived review three times.
    #[test]
    fn a_net_ingest_run_folds_abilities_onto_the_component() {
        let (mut app, entity) = session_app();

        // PRECONDITION, asserted rather than assumed: a fresh session must not
        // believe it may fly. If this defaulted to `true` the test below would
        // pass against a fold that did nothing.
        let before = *app.world().get::<Abilities>(entity).expect("Abilities");
        assert_eq!(before, Abilities::default());
        assert!(!before.may_fly, "a fresh session has no flight grant");
        assert!(!before.flying);

        fold(
            &mut app,
            ClientEvent::AbilitiesChanged {
                invulnerable: true,
                flying: true,
                can_fly: true,
                instabuild: true,
                flying_speed: 0.075,
                walking_speed: 0.15,
            },
        );
        let after = *app.world().get::<Abilities>(entity).expect("Abilities");
        assert!(after.flying);
        assert!(after.may_fly);
        assert!(after.invulnerable);
        assert!(after.instabuild);
        assert_eq!(after.flying_speed, 0.075);
        assert_eq!(after.walking_speed, 0.15);
    }

    /// **The server-gating test, and it needs a genuinely negative input.**
    ///
    /// A "flight is server-gated" test whose fixture has `can_fly: true`
    /// throughout proves nothing about the gate — the *world* species of vacuous
    /// test, whose flaw lives in the input data and is invisible in the test
    /// source. So this feeds `can_fly: false` explicitly, and additionally proves
    /// a revocation clears a previously-granted bit.
    #[test]
    fn a_server_that_revokes_flight_clears_both_bits() {
        let (mut app, entity) = session_app();
        let grant = |flying, can_fly| ClientEvent::AbilitiesChanged {
            invulnerable: false,
            flying,
            can_fly,
            instabuild: false,
            flying_speed: 0.05,
            walking_speed: 0.1,
        };

        fold(&mut app, grant(true, true));
        assert!(app.world().get::<Abilities>(entity).unwrap().may_fly);

        // A survival server, or `/gamemode survival` mid-session.
        fold(&mut app, grant(false, false));
        let after = *app.world().get::<Abilities>(entity).unwrap();
        assert!(
            !after.may_fly,
            "the record is replaced wholesale; a stale grant must not survive"
        );
        assert!(!after.flying);
    }

    /// A runtime `/gamemode` must reach `ServerGameMode`, which before this was
    /// written only by `Login`/`Respawned`.
    #[test]
    fn a_runtime_game_mode_change_reaches_the_component() {
        let (mut app, entity) = session_app();
        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 1,
                game_mode: GameMode::Survival,
                dimension: dim("overworld"),
            },
        );
        assert_eq!(
            app.world().get::<ServerGameMode>(entity).unwrap().0,
            Some(GameMode::Survival),
            "precondition: login must seed the mode, or the change below is invisible"
        );
        fold(
            &mut app,
            ClientEvent::GameModeChanged {
                game_mode: GameMode::Creative,
            },
        );
        assert_eq!(
            app.world().get::<ServerGameMode>(entity).unwrap().0,
            Some(GameMode::Creative)
        );
    }

    /// The first of the three `HudState`-shaped islands
    /// (`docs/event-routing.md`): `HeldSlotChanged` reached
    /// `lodestone_game::player_state::HudState::select_slot`, which nothing
    /// called in production. Drives the real `NetIngest` schedule, not
    /// `HudState::apply` directly — a closed loop over the fold would have
    /// passed the whole time this was an island.
    #[test]
    fn held_slot_changed_reaches_selected_slot_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        // `SelectedSlot` is inserted by `spawn_local_player`
        // (`lodestone-ecs::player`), not `insert_session_components` — added
        // by hand here to exercise the write path. The real client always
        // carries both on one entity; `held_slot_changed_is_a_no_op_without_
        // selected_slot_present` below is the control for a harness that does
        // not.
        app.world_mut().entity_mut(entity).insert(SelectedSlot(0));

        fold(&mut app, ClientEvent::HeldSlotChanged { slot: 5 });
        assert_eq!(app.world().get::<SelectedSlot>(entity).unwrap().0, 5);

        // Vanilla ignores an out-of-range wire value rather than corrupting
        // the selection — `HudState::select_slot`'s clamp, reproduced here.
        fold(&mut app, ClientEvent::HeldSlotChanged { slot: 9 });
        assert_eq!(
            app.world().get::<SelectedSlot>(entity).unwrap().0,
            5,
            "an out-of-range slot must be ignored, not miscast into a wrong \
             in-range value"
        );
        fold(&mut app, ClientEvent::HeldSlotChanged { slot: -1 });
        assert_eq!(app.world().get::<SelectedSlot>(entity).unwrap().0, 5);
    }

    /// The control for the `Option<&mut SelectedSlot>` term: a harness with no
    /// `SelectedSlot` at all — the real shape `spawn_session` alone
    /// produces — must not panic, and every *other* field in the same query
    /// must still fold (proven by the game-mode/difficulty tests running
    /// against the same `session_app()` with no `SelectedSlot` either).
    #[test]
    fn held_slot_changed_is_a_no_op_without_selected_slot_present() {
        let (mut app, entity) = session_app();
        fold(&mut app, ClientEvent::HeldSlotChanged { slot: 5 });
        assert!(app.world().get::<SelectedSlot>(entity).is_none());
    }

    /// The second `HudState`-shaped island: `DifficultyChanged` reached
    /// `HudState::apply`'s difficulty arm, which nothing called.
    #[test]
    fn difficulty_changed_reaches_server_difficulty_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        assert_eq!(
            app.world().get::<ServerDifficulty>(entity).unwrap().0,
            None,
            "precondition: unreported before the first packet"
        );
        fold(
            &mut app,
            ClientEvent::DifficultyChanged {
                difficulty: Difficulty::Hard,
                locked: true,
            },
        );
        assert_eq!(
            app.world().get::<ServerDifficulty>(entity).unwrap().0,
            Some((Difficulty::Hard, true))
        );
    }

    /// This must enter through the real scheduled session fold: an adapter can
    /// decode the scalar perfectly while an omitted registration leaves F3 with
    /// no server decision to display.
    #[test]
    fn simulation_distance_reaches_its_session_component_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        assert_eq!(
            app.world()
                .get::<ServerSimulationDistance>(entity)
                .unwrap()
                .0,
            None,
            "control: no report must not masquerade as a zero distance"
        );

        fold(
            &mut app,
            ClientEvent::SimulationDistanceChanged { distance: 11 },
        );
        assert_eq!(
            app.world()
                .get::<ServerSimulationDistance>(entity)
                .unwrap()
                .0,
            Some(11),
            "the scheduled fold must preserve the server's exact scalar"
        );

        fold(&mut app, ClientEvent::Ping { id: 7 });
        assert_eq!(
            app.world()
                .get::<ServerSimulationDistance>(entity)
                .unwrap()
                .0,
            Some(11),
            "control: an unrelated client-only event must not overwrite the scalar"
        );
    }

    /// This must run through `NetIngest`: a route-table arm alone only asks the
    /// session router to inspect an event. The F3 reader needs the packet's
    /// exact text and optional icon to survive this scheduled fold.
    #[test]
    fn server_data_reaches_its_session_component_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        assert!(
            app.world()
                .get::<SessionServerData>(entity)
                .unwrap()
                .0
                .is_none(),
            "control: no packet must not invent public server data"
        );

        fold(
            &mut app,
            ClientEvent::ServerDataReceived {
                motd: Text::literal("Copper Canyon"),
                icon: Some(vec![0x89, 0x50, 0x4e, 0x47]),
            },
        );
        let data = app
            .world()
            .get::<SessionServerData>(entity)
            .unwrap()
            .0
            .as_ref()
            .expect("the scheduled fold must retain the packet");
        assert_eq!(data.motd.to_plain_string(), "Copper Canyon");
        assert_eq!(data.icon.as_deref(), Some(&[0x89, 0x50, 0x4e, 0x47][..]));

        // Exact negative control: an unrelated client-only packet must not
        // accidentally match this fold and replace the announced identity.
        fold(&mut app, ClientEvent::Ping { id: 7 });
        assert_eq!(
            app.world()
                .get::<SessionServerData>(entity)
                .unwrap()
                .0
                .as_ref()
                .unwrap()
                .motd
                .to_plain_string(),
            "Copper Canyon"
        );
    }

    /// Combat enter/end packets reach the real scheduled fold as one session
    /// record. The exact end duration is server-authored, so it is retained
    /// instead of being reconstructed from the client's clock.
    #[test]
    fn combat_session_tracks_enter_end_repeats_and_unrelated_events() {
        fn combat(app: &App, entity: bevy_ecs::entity::Entity) -> Option<CombatSession> {
            app.world().get::<SessionCombat>(entity).unwrap().0
        }

        let (mut app, entity) = session_app();
        assert_eq!(combat(&app, entity), None, "control: no packet means no combat state");

        fold(&mut app, ClientEvent::PlayerCombatEntered);
        assert_eq!(combat(&app, entity), Some(CombatSession::Active));

        // Repeated enter packets do not invent a separate client-side session.
        fold(&mut app, ClientEvent::PlayerCombatEntered);
        assert_eq!(combat(&app, entity), Some(CombatSession::Active));

        fold(
            &mut app,
            ClientEvent::PlayerCombatEnded {
                duration_ticks: 240,
            },
        );
        assert_eq!(
            combat(&app, entity),
            Some(CombatSession::Ended {
                duration_ticks: 240,
            }),
            "the fold must preserve the server's exact duration"
        );

        // The latest repeated end packet is authoritative too.
        fold(
            &mut app,
            ClientEvent::PlayerCombatEnded { duration_ticks: 7 },
        );
        assert_eq!(
            combat(&app, entity),
            Some(CombatSession::Ended { duration_ticks: 7 })
        );

        fold(&mut app, ClientEvent::Ping { id: 7 });
        assert_eq!(
            combat(&app, entity),
            Some(CombatSession::Ended { duration_ticks: 7 }),
            "an unrelated event must not overwrite the combat session"
        );
    }

    /// The production registration uses three nested chains to stay beneath
    /// the scheduler's tuple-arity limit. A consumer after `SessionSet::Fold`
    /// must still observe the final group's local-player write in the same
    /// `NetIngest` run; otherwise the arity repair has changed global order.
    #[test]
    fn nested_session_fold_registration_runs_before_later_consumers() {
        let (mut app, entity) = session_app();
        app.insert_resource(ObservedSimulationDistance::default());
        app.add_systems(
            NetIngest,
            observe_simulation_distance_after_fold.after(SessionSet::Fold),
        );

        fold(
            &mut app,
            ClientEvent::SimulationDistanceChanged { distance: 13 },
        );
        assert_eq!(
            app.world().resource::<ObservedSimulationDistance>().0,
            Some(13),
            "the observer after SessionSet::Fold must see the final nested group's write"
        );

        fold(&mut app, ClientEvent::Ping { id: 7 });
        assert_eq!(
            app.world().resource::<ObservedSimulationDistance>().0,
            Some(13),
            "control: an unrelated terminal event must not create an observer value"
        );
        assert_eq!(
            app.world()
                .get::<ServerSimulationDistance>(entity)
                .unwrap()
                .0,
            Some(13),
            "the observer must report the real component rather than a test-only copy"
        );
    }

    /// The third: `BlockDestruction` reached
    /// `lodestone_game::mining::BlockDestructionOverlays::apply`, which
    /// nothing called outside its own file and tests.
    #[test]
    fn block_destruction_reaches_the_session_overlay_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        let p = lodestone_model::math::BlockPos::new(3, 64, 3);
        fold(
            &mut app,
            ClientEvent::BlockDestruction {
                entity_id: 7,
                pos: p,
                progress: 4,
            },
        );
        assert_eq!(
            app.world()
                .get::<SessionBlockDestruction>(entity)
                .unwrap()
                .0
                .stage_at(p),
            Some(4)
        );

        // A stage >= 10 clears the overlay, matching vanilla
        // (`LevelRenderer.setBlockBreakProgress`) — proven through the same
        // real schedule run, not by calling `BlockDestructionOverlays::apply`.
        fold(
            &mut app,
            ClientEvent::BlockDestruction {
                entity_id: 7,
                pos: p,
                progress: 10,
            },
        );
        assert_eq!(
            app.world()
                .get::<SessionBlockDestruction>(entity)
                .unwrap()
                .0
                .stage_at(p),
            None
        );

        // Entity lifecycle packets can share a batch with progress. They must
        // clear through this same writer, while preserving the batch's event
        // order so a later progress packet for a newly spawned id survives.
        fold_batch(
            &mut app,
            [
                ClientEvent::BlockDestruction {
                    entity_id: 8,
                    pos: p,
                    progress: 2,
                },
                ClientEvent::EntitySpawned {
                    entity_id: 8,
                    uuid: None,
                    entity_type: "minecraft:item".parse().expect("valid entity type"),
                    pos: lodestone_model::Vec3::new(0.0, 64.0, 0.0),
                    rotation: lodestone_model::Rotation::new(0.0, 0.0),
                    velocity: None,
                },
                ClientEvent::BlockDestruction {
                    entity_id: 8,
                    pos: p,
                    progress: 7,
                },
            ],
        );
        assert_eq!(
            app.world()
                .get::<SessionBlockDestruction>(entity)
                .unwrap()
                .0
                .stage_at(p),
            Some(7),
            "progress after an id replacement must survive in the same batch"
        );

        let removed = lodestone_model::math::BlockPos::new(4, 64, 3);
        fold_batch(
            &mut app,
            [
                ClientEvent::BlockDestruction {
                    entity_id: 9,
                    pos: removed,
                    progress: 2,
                },
                ClientEvent::EntityRemoved {
                    entity_ids: vec![9],
                },
            ],
        );
        assert_eq!(
            app.world()
                .get::<SessionBlockDestruction>(entity)
                .unwrap()
                .0
                .stage_at(removed),
            None,
            "entity removal must clear progress without a reset packet"
        );
        assert_eq!(
            app.world()
                .get::<SessionBlockDestruction>(entity)
                .unwrap()
                .0
                .stage_at(p),
            Some(7),
            "an unrelated entity's progress must survive the removal"
        );

        // A new login/respawn view must not retain progress from the previous
        // player-level instance when no explicit reset packet accompanies it.
        fold(
            &mut app,
            ClientEvent::BlockDestruction {
                entity_id: 8,
                pos: p,
                progress: 2,
            },
        );
        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 11,
                game_mode: GameMode::Creative,
                dimension: dim("overworld"),
            },
        );
        assert!(
            app.world()
                .get::<SessionBlockDestruction>(entity)
                .unwrap()
                .0
                .is_empty(),
            "login must clear progress from the previous player-level view"
        );

        fold(
            &mut app,
            ClientEvent::BlockDestruction {
                entity_id: 8,
                pos: p,
                progress: 2,
            },
        );
        fold(
            &mut app,
            ClientEvent::Respawned {
                dimension: dim("the_nether"),
                game_mode: GameMode::Survival,
                previous_game_mode: None,
                last_death_location: None,
            },
        );
        assert!(
            app.world()
                .get::<SessionBlockDestruction>(entity)
                .unwrap()
                .0
                .is_empty(),
            "respawn must clear progress from the previous player-level view"
        );
    }

    #[test]
    fn exactly_one_system_writes_each_session_component() {
        assert!(
            !net_ingest_is_ambiguous(false),
            "the shipped NetIngest schedule must have no unordered conflicting pair"
        );
    }

    /// The control that proves the detector above works: add a second writer of
    /// [`SessionScoreboard`] in the same set with no ordering, and the build must
    /// fail. Without this, `exactly_one_system_writes_each_session_component`
    /// would pass just as well against a detector that was switched off.
    #[test]
    fn a_second_unordered_scoreboard_writer_fails_the_ambiguity_check() {
        assert!(
            net_ingest_is_ambiguous(true),
            "a second unordered writer of SessionScoreboard must be reported"
        );
    }

    /// Build `SessionPlugin`'s `NetIngest` with ambiguity detection promoted to
    /// an error, optionally adding a rogue second writer first.
    fn net_ingest_is_ambiguous(with_rogue_writer: bool) -> bool {
        use bevy_ecs::schedule::{LogLevel, ScheduleBuildSettings};

        fn rogue(mut boards: Query<&mut SessionScoreboard>) {
            for mut board in &mut boards {
                board.0.remove_objective("anything");
            }
        }

        let mut app = App::new();
        app.add_plugins(SessionPlugin);
        if with_rogue_writer {
            app.add_systems(NetIngest, rogue.in_set(IngestSet::Apply));
        }
        // Deliberately *not* run first: an already-built schedule is not rebuilt,
        // so `initialize` would return `Ok` without ever consulting the new
        // settings — which is exactly how this assertion would go vacuous.
        app.world_mut()
            .schedule_scope(NetIngest, |world, schedule| {
                schedule.set_build_settings(ScheduleBuildSettings {
                    ambiguity_detection: LogLevel::Error,
                    ..ScheduleBuildSettings::default()
                });
                schedule.initialize(world).is_err()
            })
    }

    /// The negative control for the tick test: without the schedule the same 60
    /// iterations leave the message up, so the assertion above is pinning the
    /// registration and not merely the passage of loop iterations.
    #[test]
    fn without_the_tick_system_the_action_bar_never_expires() {
        let mut app = App::new();
        app.add_plugins(crate::CorePlugin);
        let entity = app.world_mut().spawn(LocalPlayer).id();
        insert_hud_components(app.world_mut(), entity);
        app.world_mut()
            .get_mut::<ActionBarOverlay>(entity)
            .unwrap()
            .0
            .set(Text::literal("hi"));
        for _ in 0..lodestone_game::player_state::ActionBar::DISPLAY_TICKS {
            app.world_mut().run_schedule(GameTick);
        }
        assert!(
            app.world()
                .get::<ActionBarOverlay>(entity)
                .unwrap()
                .0
                .text()
                .is_some()
        );
    }

    /// The `Riding` fold, through the schedule (Tier 1 item 8): our own id
    /// appearing in a vehicle's passenger list is what mounts us, and the list
    /// going empty is what dismounts us.
    #[test]
    fn our_own_id_in_a_passenger_list_mounts_us_and_an_empty_list_dismounts_us() {
        let (mut app, entity) = session_app();
        fold(
            &mut app,
            ClientEvent::Login {
                entity_id: 7,
                game_mode: GameMode::Creative,
                dimension: dim("overworld"),
            },
        );
        assert_eq!(
            app.world().get::<Riding>(entity).copied(),
            Some(Riding(None)),
            "a freshly logged-in player is on foot"
        );
        fold(
            &mut app,
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 42,
                passenger_ids: vec![7],
            },
        );
        assert_eq!(
            app.world().get::<Riding>(entity).copied(),
            Some(Riding(Some(42)))
        );
        fold(
            &mut app,
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 42,
                passenger_ids: Vec::new(),
            },
        );
        assert_eq!(app.world().get::<Riding>(entity).copied(), Some(Riding(None)));
    }

    /// **The `else if` this pins is the difference between riding a boat and being
    /// ejected from it by an unrelated mob.** `SET_PASSENGERS` is broadcast for
    /// every vehicle in view distance, so "our id is not in this list" only means
    /// "we dismounted" when the list belongs to the vehicle we are actually in.
    #[test]
    fn another_vehicles_passenger_list_does_not_dismount_us() {
        let (mut app, entity) = session_app();
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
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 42,
                passenger_ids: vec![7],
            },
        );
        // Precondition, asserted: without a live seat the assertion below cannot
        // distinguish "was not ejected" from "was never aboard".
        assert_eq!(
            app.world().get::<Riding>(entity).copied(),
            Some(Riding(Some(42))),
            "precondition: we must be aboard 42"
        );
        // Some pig two chunks away gains a rider.
        fold(
            &mut app,
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 99,
                passenger_ids: vec![1234],
            },
        );
        assert_eq!(
            app.world().get::<Riding>(entity).copied(),
            Some(Riding(Some(42))),
            "an unrelated vehicle's passenger list must leave our own seat alone"
        );
        // And the negative control for the same detector: a list for *our* vehicle
        // that omits us does dismount, so the assertion above is about the vehicle
        // id being compared and not about the fold being inert.
        fold(
            &mut app,
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 42,
                passenger_ids: vec![1234],
            },
        );
        assert_eq!(
            app.world().get::<Riding>(entity).copied(),
            Some(Riding(None)),
            "our own vehicle's list without us must dismount"
        );
    }

    /// A respawn lands us on foot. Vanilla builds a brand-new `ServerPlayer`
    /// (`PlayerList.respawn`) which is never a passenger, and no
    /// `SET_PASSENGERS` follows — so without the explicit clear the seat pin
    /// would hold a respawned player at a vehicle nothing can free them from.
    #[test]
    fn respawning_clears_the_ride_state() {
        let (mut app, entity) = session_app();
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
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 42,
                passenger_ids: vec![7],
            },
        );
        assert_eq!(
            app.world().get::<Riding>(entity).copied(),
            Some(Riding(Some(42))),
            "precondition: aboard before dying"
        );
        fold(
            &mut app,
            ClientEvent::Respawned {
                dimension: dim("overworld"),
                game_mode: GameMode::Survival,
                previous_game_mode: None,
                last_death_location: None,
            },
        );
        assert_eq!(app.world().get::<Riding>(entity).copied(), Some(Riding(None)));
    }
