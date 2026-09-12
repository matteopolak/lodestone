use super::*;
use bevy_app::App;
use bevy_ecs::prelude::{Query, With};
use bevy_ecs::schedule::IntoScheduleConfigs;
use lodestone_model::event::{
        CollisionRule, DisplaySlot, ObjectiveMode, TeamAction, TeamColor, TeamParameters,
        Visibility,
    };
use lodestone_model::{
    BossAction, BossColor, BossOverlay, ClientEvent, Difficulty, DimensionId, DimensionTypeInfo,
    GameMode, PlayerListEntry, Text,
};
use crate::player::{LocalPlayer, SelectedSlot};
use crate::schedules::{GameTick, NetIngest};
use crate::sets::IngestSet;
use uuid::Uuid;

    #[derive(bevy_ecs::prelude::Resource, Debug, Default, PartialEq, Eq)]
    struct ObservedSimulationDistance(Option<i32>);

    /// A consumer ordered after the complete fold set. Kept outside the test
    /// body so the scheduler validates the exact function signature when the
    /// registration tuple changes.
    fn observe_simulation_distance_after_fold(
        distances: Query<&ServerSimulationDistance, With<LocalPlayer>>,
        mut observed: bevy_ecs::prelude::ResMut<ObservedSimulationDistance>,
    ) {
        observed.0 = distances
            .single()
            .expect("the test owns exactly one local session")
            .0;
    }

    fn dim(path: &str) -> DimensionId {
        format!("minecraft:{path}")
            .parse()
            .expect("valid dimension id")
    }

    /// Build the net-thread shape: `SessionPlugin` plus one session entity.
    fn session_app() -> (App, bevy_ecs::entity::Entity) {
        let mut app = App::new();
        app.add_plugins(SessionPlugin);
        let entity = spawn_session(app.world_mut());
        (app, entity)
    }

    fn fold(app: &mut App, event: ClientEvent) {
        app.world_mut()
            .resource_mut::<crate::ingest::IngestQueue>()
            .push(event);
        app.world_mut().run_schedule(NetIngest);
    }

    fn fold_batch(app: &mut App, events: impl IntoIterator<Item = ClientEvent>) {
        {
            let mut queue = app
                .world_mut()
                .resource_mut::<crate::ingest::IngestQueue>();
            for event in events {
                queue.push(event);
            }
        }
        app.world_mut().run_schedule(NetIngest);
    }

    /// The fold must be reachable **through the schedule**. A directly-called
    /// `Scoreboard::apply` passes its own unit tests while the schedule
    /// registration is missing, which is the island this migration has found
    /// nine times.
    #[test]
    fn a_net_ingest_run_folds_a_scoreboard_objective_onto_the_component() {
        let (mut app, entity) = session_app();
        fold(
            &mut app,
            ClientEvent::ObjectiveUpdate {
                name: "kills".into(),
                mode: ObjectiveMode::Add,
                display_name: Some(Text::literal("Kills")),
                render_type: None,
                number_format: None,
            },
        );
        fold(
            &mut app,
            ClientEvent::DisplayObjective {
                slot: DisplaySlot::Sidebar,
                objective: Some("kills".into()),
            },
        );
        fold(
            &mut app,
            ClientEvent::ScoreUpdate {
                holder: "Alice".into(),
                objective: "kills".into(),
                value: 7,
                display: None,
                number_format: None,
            },
        );

        let board = &app.world().get::<SessionScoreboard>(entity).unwrap().0;
        assert_eq!(
            board.displayed(lodestone_game::scoreboard::DisplaySlot::Sidebar),
            Some("kills")
        );
        assert_eq!(board.score("kills", "Alice").map(|e| e.value), Some(7));
    }

    /// The negative control for the above: an event no session system claims
    /// must leave every component alone, so "the component changed" is
    /// actually discriminating rather than true of any schedule run.
    #[test]
    fn an_unclaimed_event_changes_nothing() {
        let (mut app, entity) = session_app();
        let before = app
            .world()
            .get::<SessionScoreboard>(entity)
            .unwrap()
            .clone();
        fold(&mut app, ClientEvent::KeepAlive { id: 7 });
        let after = app.world().get::<SessionScoreboard>(entity).unwrap();
        assert_eq!(&before, after);
        assert!(
            !handles_event(&ClientEvent::KeepAlive { id: 7 }),
            "…and the routing switch must agree, or the event is silently dropped"
        );
    }

    /// `PlayerListRemove` must actually remove. The fold this replaces
    /// (`Inner::apply`'s `PlayerListUpdate` arm) had **no** remove arm at all,
    /// so a player who left the server stayed in the read-model forever.
    #[test]
    fn a_player_who_leaves_is_removed_from_the_tab_list() {
        let (mut app, entity) = session_app();
        let alice = Uuid::from_u128(1);
        fold(
            &mut app,
            ClientEvent::PlayerListUpdate {
                entries: vec![PlayerListEntry {
                    uuid: Some(alice),
                    name: Some("Alice".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(12),
                    display_name: None,
                    listed: Some(true),
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                }],
            },
        );
        assert_eq!(
            app.world().get::<SessionTabList>(entity).unwrap().0.len(),
            1
        );

        fold(
            &mut app,
            ClientEvent::PlayerListRemove {
                profile_ids: vec![alice],
            },
        );
        assert_eq!(
            app.world().get::<SessionTabList>(entity).unwrap().0.len(),
            0,
            "the old fold never removed anyone; this is the regression guard"
        );
    }

    /// Boss bars reach a component at all — `BossBarSet::apply` was complete
    /// and unit-tested with zero production callers before this stage.
    #[test]
    fn a_boss_bar_add_reaches_the_component_in_render_order() {
        let (mut app, entity) = session_app();
        for (n, title) in [(1u128, "First"), (2, "Second")] {
            fold(
                &mut app,
                ClientEvent::BossBarUpdate {
                    id: Uuid::from_u128(n),
                    action: BossAction::Add {
                        title: Box::new(Text::literal(title)),
                        progress: 0.5,
                        color: BossColor::Red,
                        overlay: BossOverlay::Progress,
                        darken: false,
                        music: false,
                        fog: false,
                    },
                },
            );
        }
        let bars = &app.world().get::<SessionBossBars>(entity).unwrap().0;
        let titles: Vec<String> = bars
            .iter()
            .map(|(_, bar)| bar.title.to_plain_string())
            .collect();
        assert_eq!(titles, vec!["First".to_string(), "Second".to_string()]);
    }

    /// The HUD overlays must be aged by the 20 Hz schedule, not by a driver
    /// calling `tick` by hand — otherwise the component set is authoritative
    /// but inert.
    #[test]
    fn a_game_tick_run_expires_the_action_bar() {
        let mut app = App::new();
        app.add_plugins(SessionHudPlugin);
        let entity = app.world_mut().spawn(LocalPlayer).id();
        insert_hud_components(app.world_mut(), entity);

        app.world_mut()
            .get_mut::<ActionBarOverlay>(entity)
            .unwrap()
            .0
            .set(Text::literal("hi"));
        assert!(
            app.world()
                .get::<ActionBarOverlay>(entity)
                .unwrap()
                .0
                .text()
                .is_some()
        );

        for _ in 0..lodestone_game::player_state::ActionBar::DISPLAY_TICKS {
            app.world_mut().run_schedule(GameTick);
        }
        assert!(
            app.world()
                .get::<ActionBarOverlay>(entity)
                .unwrap()
                .0
                .text()
                .is_none(),
            "60 GameTick runs must expire a vanilla action bar"
        );
    }

    /// The shape **production** uses: `new_ingest_handle`, i.e. `IngestPlugin`
    /// *and* `SessionPlugin` on one `World`.
    ///
    /// This exists because the session tests above are a closed loop over
    /// `SessionPlugin` alone, and that loop was green while the real
    /// configuration folded **nothing**: both plugins used to register
    /// `drain_ingest_queue`, the second copy cleared the batch the first had
    /// filled, and every `Apply` system saw an empty slice. A crate's own tests
    /// being green while the crate is inert is the defect class `CLAUDE.md`
    /// names, and this is its detector.
    #[test]
    fn the_real_both_plugin_world_still_folds_a_scoreboard() {
        let handle = crate::new_ingest_handle();
        let entity = spawn_session(&mut handle.write());
        {
            let mut world = handle.write();
            world.resource_mut::<crate::ingest::IngestQueue>().push(
                ClientEvent::DisplayObjective {
                    slot: DisplaySlot::Sidebar,
                    objective: Some("kills".into()),
                },
            );
            world.run_schedule(NetIngest);
        }
        assert_eq!(
            handle
                .read()
                .get::<SessionScoreboard>(entity)
                .unwrap()
                .0
                .displayed(lodestone_game::scoreboard::DisplaySlot::Sidebar),
            Some("kills"),
            "the entity+session World must fold session events, not swallow them"
        );
    }

    /// …and the same `World` must still fold an *entity* event, so the fix for
    /// the double drain cannot have been "drop one of the plugins' systems".
    #[test]
    fn the_real_both_plugin_world_still_folds_an_entity_spawn() {
        use lodestone_model::{ResourceKey, Vec3};
        use std::str::FromStr;

        let handle = crate::new_ingest_handle();
        {
            let mut world = handle.write();
            world
                .resource_mut::<crate::ingest::IngestQueue>()
                .push(ClientEvent::EntitySpawned {
                    entity_id: 5,
                    uuid: None,
                    entity_type: ResourceKey::from_str("minecraft:pig").unwrap(),
                    pos: Vec3::new(1.0, 2.0, 3.0),
                    rotation: lodestone_model::Rotation::default(),
                    velocity: None,
                });
            world.run_schedule(NetIngest);
        }
        assert!(
            handle
                .read()
                .resource::<crate::entity::EntityIndex>()
                .get(5)
                .is_some()
        );
    }

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

    // ---- the menu family, real-schedule coverage --------------------------
    //
    // `apply_menus` is registered in `SessionPlugin`'s `NetIngest` chain and
    // `lodestone_game::menus::Menus::apply` is exhaustively unit-tested in
    // `lodestone-game`, but until this test nothing in *this* crate ever fed
    // `ScreenOpened`/`ContainerContent`/`ContainerSlot`/`ContainerData`/
    // `CursorItemChanged`/`InventorySlotChanged` through the real schedule —
    // the exact closed-loop shape `CLAUDE.md` names: a fold can be correct and
    // green in its own crate while the wiring one layer up is untested and
    // could be silently broken.

    fn key(s: &str) -> lodestone_model::ids::ResourceKey {
        s.parse().expect("valid resource key")
    }

    fn ms(item: &str, count: u32) -> lodestone_model::ItemStack {
        lodestone_model::ItemStack::new(key(item), count)
    }

    #[test]
    fn menu_family_events_reach_session_menus_through_the_real_schedule() {
        let (mut app, entity) = session_app();

        // Pre-condition: nothing open yet.
        assert_eq!(
            app.world().get::<SessionMenus>(entity).unwrap().0.opened_window_id(),
            None
        );

        fold(
            &mut app,
            ClientEvent::ScreenOpened {
                window_id: 5,
                menu_type: key("minecraft:generic_9x3"),
                title: Text::literal("Chest"),
            },
        );
        // `ScreenOpened` alone only records a *pending* open — `opened` stays
        // `None` until the content packet arrives. Asserting that here is what
        // proves the value actually came from `ScreenOpened`'s own fold rather
        // than being some other default: if this fold never ran, the metadata
        // below (title / menu type) could not appear either.
        assert_eq!(
            app.world().get::<SessionMenus>(entity).unwrap().0.opened_window_id(),
            None,
            "ScreenOpened must not open a menu by itself"
        );

        // A 9x3 chest: 27 container slots + 36 player-inventory slots.
        let mut items = vec![None; 63];
        items[0] = Some(ms("minecraft:gold_ingot", 5));
        fold(
            &mut app,
            ClientEvent::ContainerContent {
                window_id: 5,
                state_id: lodestone_model::ContainerStateId::new(1),
                items,
                carried_item: None,
            },
        );
        {
            let menus = &app.world().get::<SessionMenus>(entity).unwrap().0;
            assert_eq!(menus.opened_window_id(), Some(5));
            assert_eq!(menus.opened_title(), Some(&Text::literal("Chest")));
            assert_eq!(menus.opened_menu_type(), Some(&key("minecraft:generic_9x3")));
            assert!(
                menus.opened().unwrap().slot_item(0).is_some(),
                "the gold ingot from ContainerContent must have landed"
            );
        }

        fold(
            &mut app,
            ClientEvent::ContainerSlot {
                window_id: 5,
                state_id: lodestone_model::ContainerStateId::new(1),
                slot: 1,
                item: Some(ms("minecraft:diamond", 1)),
            },
        );
        assert!(
            app.world()
                .get::<SessionMenus>(entity)
                .unwrap()
                .0
                .opened()
                .unwrap()
                .slot_item(1)
                .is_some(),
            "ContainerSlot must reach the open menu"
        );

        fold(
            &mut app,
            ClientEvent::ContainerData {
                window_id: 5,
                property: 0,
                value: 42,
            },
        );
        assert_eq!(
            app.world()
                .get::<SessionMenus>(entity)
                .unwrap()
                .0
                .container_data(0),
            Some(42),
            "ContainerData must reach the open menu's properties"
        );

        fold(
            &mut app,
            ClientEvent::CursorItemChanged {
                item: Some(ms("minecraft:apple", 1)),
            },
        );
        assert!(
            app.world()
                .get::<SessionMenus>(entity)
                .unwrap()
                .0
                .opened()
                .unwrap()
                .carried()
                .is_some(),
            "CursorItemChanged must reach the carried-item slot"
        );

        fold(
            &mut app,
            ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(ms("minecraft:stone", 32)),
            },
        );
        assert!(
            app.world()
                .get::<SessionMenus>(entity)
                .unwrap()
                .0
                .player_native(0)
                .is_some(),
            "InventorySlotChanged must reach the player-inventory native slots"
        );

        fold(&mut app, ClientEvent::ScreenClosed { window_id: 5 });
        assert_eq!(
            app.world().get::<SessionMenus>(entity).unwrap().0.opened_window_id(),
            None,
            "ScreenClosed must close the menu"
        );
    }

    // ---- scoreboard: the two members with no dedicated schedule test ------

    fn red_team_params() -> Box<TeamParameters> {
        Box::new(TeamParameters {
            display_name: Text::literal("Red Team"),
            prefix: Text::literal("["),
            suffix: Text::literal("]"),
            name_tag_visibility: Visibility::HideForOtherTeams,
            collision_rule: CollisionRule::PushOwnTeam,
            color: Some(TeamColor::Red),
            friendly_fire: false,
            see_friendly_invisibles: true,
        })
    }

    /// `ScoreUpdate`/`ObjectiveUpdate`/`DisplayObjective` already have a
    /// real-schedule test above (`a_net_ingest_run_folds_a_scoreboard_objective
    /// _onto_the_component`); `ScoreReset` and `TeamUpdate` shared the same
    /// `apply_scoreboard` system but had no schedule-level test of their own.
    #[test]
    fn score_reset_and_team_update_reach_the_scoreboard_through_the_real_schedule() {
        let (mut app, entity) = session_app();
        fold(
            &mut app,
            ClientEvent::ObjectiveUpdate {
                name: "kills".into(),
                mode: ObjectiveMode::Add,
                display_name: Some(Text::literal("Kills")),
                render_type: None,
                number_format: None,
            },
        );
        fold(
            &mut app,
            ClientEvent::ScoreUpdate {
                holder: "Alice".into(),
                objective: "kills".into(),
                value: 7,
                display: None,
                number_format: None,
            },
        );
        assert_eq!(
            app.world()
                .get::<SessionScoreboard>(entity)
                .unwrap()
                .0
                .score("kills", "Alice")
                .map(|e| e.value),
            Some(7),
            "precondition: the score exists before it is reset"
        );

        fold(
            &mut app,
            ClientEvent::ScoreReset {
                holder: "Alice".into(),
                objective: Some("kills".into()),
            },
        );
        assert_eq!(
            app.world()
                .get::<SessionScoreboard>(entity)
                .unwrap()
                .0
                .score("kills", "Alice")
                .map(|e| e.value),
            None,
            "ScoreReset must reach the scoreboard through the real schedule"
        );

        fold(
            &mut app,
            ClientEvent::TeamUpdate {
                name: "red".into(),
                action: TeamAction::Create {
                    params: red_team_params(),
                    members: vec!["Bob".into()],
                },
            },
        );
        let board = &app.world().get::<SessionScoreboard>(entity).unwrap().0;
        assert_eq!(board.team_of("Bob").map(|t| t.name.as_str()), Some("red"));
    }

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
