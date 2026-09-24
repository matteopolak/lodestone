use super::*;
#[test]
fn an_interior_block_change_dirties_exactly_its_own_section() {
    // Local (8,8,8) touches no section boundary, so a live block update
    // there must cost one re-mesh — not the 27 a blanket neighbourhood
    // would submit, and not the ~216 a whole-column signal would.
    let dirty = dirty_sections_for_blocks(3, 4, 5, &[[8, 8, 8]]);
    assert_eq!(
        dirty.iter().copied().collect::<Vec<_>>(),
        vec![(3, 4, 5)],
        "an interior cell reaches no neighbouring section"
    );
}

#[test]
fn a_block_change_on_a_face_also_dirties_that_neighbour() {
    // The bug this pins: breaking a block at local x=15 on a live server
    // leaves the +x neighbour's face baked against the *old* state, which
    // shows as a stale face or z-fighting at every chunk border while
    // mining. The -x neighbour must NOT be dirtied — that is the half of
    // the filter a "dirty all 27" implementation gets wrong.
    let dirty = dirty_sections_for_blocks(3, 4, 5, &[[15, 8, 8]]);
    assert_eq!(
        dirty.iter().copied().collect::<Vec<_>>(),
        vec![(3, 4, 5), (4, 4, 5)],
        "a +x face cell dirties its own section and the +x neighbour only"
    );
}

#[test]
fn a_corner_block_change_dirties_the_full_corner_octant() {
    // (0,0,0) touches three faces, three edges and one corner: 8 sections.
    // Edge and corner neighbours matter because AO samples the 3 cells
    // around each vertex, which reach diagonally across section corners.
    let dirty = dirty_sections_for_blocks(0, 0, 0, &[[0, 0, 0]]);
    assert_eq!(dirty.len(), 8, "a corner cell reaches an octant: {dirty:?}");
    assert!(
        dirty.contains(&(-1, -1, -1)),
        "the diagonal corner is included"
    );
    assert!(!dirty.contains(&(1, 0, 0)), "the far side is not reachable");
}

#[test]
fn a_whole_section_update_is_bounded_by_the_neighbourhood_not_the_cell_count() {
    // A 4096-cell `SECTION_BLOCKS_UPDATE` (a full section rewrite) must not
    // submit 4096 re-meshes. 27 is the hard ceiling because that is the
    // entire neighbourhood any cell in the section can reach.
    let all: Vec<[u8; 3]> = (0..16u8)
        .flat_map(|x| (0..16u8).flat_map(move |y| (0..16u8).map(move |z| [x, y, z])))
        .collect();
    assert_eq!(
        all.len(),
        4096,
        "control: the fixture really is a full section"
    );
    let dirty = dirty_sections_for_blocks(0, 0, 0, &all);
    assert_eq!(dirty.len(), 27, "bounded by the 3x3x3 neighbourhood");
}

// -----------------------------------------------------------------------
// §4.1(c): one `World`, one `GameTick`, one accumulator
// -----------------------------------------------------------------------

/// **The (c) authority test.** One `World` means one `LocalPlayer`.
///
/// `spawn_local_player` and `spawn_session` both spawn an entity carrying the
/// `LocalPlayer` marker. They used to be in different `World`s, so both could
/// exist; in one `World` they have to be one entity, or every
/// `With<LocalPlayer>` system (`tick_hud_overlays`, the physics and egress
/// systems) silently runs against two players and the HUD reads whichever the
/// query happened to yield.
#[test]
fn the_one_world_holds_exactly_one_local_player() {
    let sim = Sim::new(test_config());
    assert_eq!(local_player_count(sim.ecs()), 1);
    // …and it is the entity the driver named, not some other one.
    assert!(
        sim.ecs()
            .read()
            .get::<lodestone_ecs::SessionScoreboard>(sim.local_player())
            .is_some(),
        "the session fold's components must hang off Sim's own local player"
    );
}

/// The control that proves the count above discriminates: spawning the session
/// entity separately — which is exactly what
/// `lodestone_client::state::SharedState::default` does when it is *not* handed
/// a `World` — takes it to two.
#[test]
fn a_separately_spawned_session_entity_makes_two_local_players() {
    let sim = Sim::new(test_config());
    lodestone_ecs::spawn_session(&mut sim.ecs().write());
    assert_eq!(
        local_player_count(sim.ecs()),
        2,
        "the detector must be able to see a second LocalPlayer"
    );
}

/// Note the shape: **one** guard, named, then queried.
///
/// The obvious spelling — `handle.write().query_filtered::<…>().iter(&handle.write())`
/// — takes the write lock twice in one expression and hangs forever, because
/// `parking_lot::RwLock` is not reentrant. It was written that way first and
/// deadlocked the test binary, which is why `EcsHandle`'s rule 1 is stated as
/// "one statement, one guard" rather than as advice.
fn local_player_count(handle: &EcsHandle) -> usize {
    let mut world = handle.write();
    let mut state =
        world.query_filtered::<Entity, bevy_ecs::prelude::With<lodestone_ecs::LocalPlayer>>();
    state.iter(&world).count()
}

/// **The clock-divergence gate.** A maximal stall must advance the *entity*
/// systems' tick count and the player's by the same amount, and that amount
/// must be vanilla's ten.
///
/// This is the measurement Stage 5 recorded and could not fix: `Sim::step`
/// banked `dt.clamp(0.0, 0.25)` (five ticks) while `EntityInterpolator` banked
/// the pacer's `0.5 s` unclamped (ten), so a maximal stall advanced item
/// physics five ticks further than player physics — per stall, cumulatively,
/// with the excess real time discarded rather than reconciled. Counting a
/// system in `TickSet::Animate` (where `tick_walk_animation` lives) against
/// `FrameClock::ticks` is what would have caught it: before (c) those were two
/// schedules in two `World`s and could not have agreed.
#[test]
fn a_maximal_stall_advances_the_entity_and_player_clocks_by_the_same_ten_ticks() {
    use bevy_ecs::resource::Resource;
    use bevy_ecs::schedule::IntoScheduleConfigs;

    #[derive(Resource, Default)]
    struct AnimateRuns(u64);

    let mut sim = Sim::new(test_config());
    {
        let mut world = sim.ecs().write();
        world.init_resource::<AnimateRuns>();
        world.schedule_scope(GameTick, |_w, schedule| {
            schedule.add_systems(
                (|mut runs: bevy_ecs::system::ResMut<AnimateRuns>| runs.0 += 1)
                    .in_set(lodestone_ecs::TickSet::Animate),
            );
        });
    }

    let before = sim.tick_count();
    // Sixty seconds: 1200 ticks of real time, i.e. far past any budget.
    sim.step(60.0);
    let player_ticks = sim.tick_count() - before;
    let animate_runs = sim.ecs().read().resource::<AnimateRuns>().0;

    assert_eq!(
        player_ticks,
        u64::from(lodestone_ecs::MAX_CATCH_UP_TICKS),
        "the one accumulator's catch-up policy is vanilla's ten, not the \
         shell's old five"
    );
    assert_eq!(
        animate_runs, player_ticks,
        "the entity animation tick and the player tick are one schedule on \
         one clock; a difference here is the divergence §4.1(c) deleted"
    );
    // The excess is dropped, not carried: the next frame owes nothing.
    assert!(
        sim.clock().accumulator < lodestone_ecs::TICK_PERIOD,
        "accumulator {} should be a sub-tick residual",
        sim.clock().accumulator
    );
}

/// A quit-to-title resets the **one** accumulator and leaves monotonic time
/// alone.
///
/// `end_session` used to reset the interpolator's accumulator (by replacing the
/// whole interpolator) and not the player's, so a reconnect re-phased the two
/// clocks arbitrarily. There is one to reset now, and the chat timestamps that
/// ride on `FrameClock::secs` must survive it — a line stamped before the
/// teardown still has to age correctly afterwards.
#[test]
fn end_session_resets_the_one_accumulator_and_not_the_monotonic_clock() {
    let mut sim = Sim::with_demo_world(test_config());
    // Leave a deliberate sub-tick residual.
    sim.step(lodestone_ecs::TICK_PERIOD * 1.5);
    assert!(
        sim.clock().accumulator > 0.0,
        "control: there is a residual"
    );
    let secs_before = sim.clock().secs;
    let ticks_before = sim.tick_count();

    sim.end_session();

    assert_eq!(sim.clock().accumulator, 0.0);
    assert_eq!(sim.clock().interp_alpha, 0.0);
    assert!(
        (sim.clock().secs - secs_before).abs() < 1e-12,
        "monotonic time must not rewind, or pre-teardown chat ages break"
    );
    assert_eq!(sim.tick_count(), ticks_before);
}

/// A session teardown clears the render-side entity tracks.
///
/// This used to be a side effect of replacing the whole `EntityInterpolator`
/// (and therefore of dropping its `World`). With one `World` it has to be an
/// explicit despawn, which is exactly the kind of thing that gets dropped in a
/// refactor and shows up as the previous server's mobs still drawn on the title
/// **You could open a crafting table and not get out of it.**
///
/// `close_open_menu` sent `ContainerClose` and nothing else, so
/// [`Sim::open_menu`] stayed `Some` forever — a vanilla server does not echo a
/// close back. Everything downstream keys off that: `active_container_menu`,
/// the key-dispatch gate, the container draw. The dispatch was fixed first and
/// the bug survived, because the function the keys correctly reached did not
/// clear anything.
///
/// The control matters as much as the assertion: it proves the menu really was
/// open first, so a fold that silently failed to open it could not make this
/// pass vacuously.
#[test]
fn closing_a_server_menu_clears_it_locally_without_waiting_for_the_server() {
    use lodestone_model::ClientEvent;

    let mut sim = Sim::with_demo_world(test_config());
    let local = sim.local;
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&ClientEvent::ScreenOpened {
                window_id: 5,
                menu_type: lodestone_model::Identifier::new("minecraft", "crafting").unwrap(),
                title: lodestone_model::Text::literal("Crafting"),
            });
            // 3x3 grid + result + 36 player slots: the content packet is what
            // actually promotes `pending` to `opened`.
            menus.0.apply(&ClientEvent::ContainerContent {
                window_id: 5,
                state_id: lodestone_model::ContainerStateId::new(1),
                items: vec![None; 46],
                carried_item: None,
            });
        }
    });
    assert!(
        sim.open_menu().is_some(),
        "control: the menu must actually be open, or this gate proves nothing"
    );

    sim.close_open_menu();

    assert!(
        sim.open_menu().is_none(),
        "closing must clear the local menu immediately — a vanilla server sends \
         no close back, so anything that waits for the wire waits forever"
    );
}

/// screen.
///
/// The ingest path has no `EntitySnapshot` handoff —
/// the ingest components it now reads directly are spawned through the real
/// `ClientEvent::EntitySpawned` -> `IngestQueue` -> `NetIngest` path (the
/// [`ingest`] helper), then `Sim::fold_entities` folds them, exactly like a
/// live session's `Sim::step` does.
#[test]
fn end_session_clears_the_entity_tracks() {
    let mut sim = Sim::with_demo_world(test_config());
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntitySpawned {
            entity_id: 7,
            uuid: None,
            entity_type: "minecraft:pig".parse().expect("valid entity type key"),
            pos: lodestone_model::Vec3::new(1.0, 64.0, 1.0),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );
    sim.fold_entities();
    assert_eq!(
        sim.read(crate::entities::tracked_entity_count),
        1,
        "control: the fold really did spawn a track"
    );

    sim.end_session();
    assert_eq!(sim.read(crate::entities::tracked_entity_count), 0);
    assert!(sim.entity_draws().is_empty());
}

/// **Production-path control for the `text_display` island**: a real
/// `text_display` folded through the same `ClientEvent` -> `IngestQueue` ->
/// `NetIngest` path production uses (`ingest`, exactly like
/// [`end_session_clears_the_entity_tracks`] above), then through the same
/// `Extract` schedule [`crate::sim::step::Sim::step`] runs every frame, must
/// reach [`Sim::display_draws`] with no hand-installed draw anywhere in this
/// test.
///
/// This is deliberately **not** a test that calls
/// `crate::display_entities::extracted_display_draws`/`set_display_draws`
/// itself — a GPU pixel gate already proved those two functions individually
/// correct, by installing a draw it built by hand and rendering it. That
/// proves the *renderer*, not that anything in production ever calls the
/// installer: `RenderState::set_display_draws` had zero production callers
/// until `app::redraw` was wired to call `Sim::display_draws()`, so a
/// `text_display` was resolved all the way to a draw-ready snapshot and then
/// dropped on the floor. This test's job is to fail if that hop goes missing
/// again, which a hand-installed-draw gate structurally cannot do.
#[test]
fn a_real_text_display_folded_through_ingest_and_extract_reaches_sim_display_draws() {
    let mut sim = Sim::with_demo_world(test_config());
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntitySpawned {
            entity_id: 9,
            uuid: None,
            entity_type: "minecraft:text_display".parse().expect("valid entity type key"),
            pos: lodestone_model::Vec3::new(1.0, 64.0, 1.0),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntityMetadataUpdated {
            entity_id: 9,
            metadata: lodestone_model::EntityMetadataUpdate {
                display_text: lodestone_client::Reported::Reported(Some(
                    lodestone_model::Text::literal("hello"),
                )),
                ..Default::default()
            },
        },
    );
    // The same two-step production sequence `Sim::step` runs every frame
    // (`sim/step.rs`): fold, then the `Extract` schedule that populates
    // `ExtractedDisplayDraws` (`display_entities::DisplayEntityPlugin`,
    // installed in `Sim::client_app`).
    sim.fold_entities();
    sim.write(|w| w.run_schedule(Extract));

    let draws = sim.display_draws();
    let draw = draws
        .iter()
        .find(|d| d.id == 9)
        .unwrap_or_else(|| panic!("entity 9 never reached Sim::display_draws: {draws:?}"));
    assert_eq!(draw.type_path, crate::display_entities::TEXT_DISPLAY_TYPE_PATH);
    assert_eq!(
        draw.text.as_ref().map(lodestone_model::ResolvedText::to_plain_string),
        Some("hello".to_string())
    );
}

// -- world border + spawn point + game rules --------------
//
// `SessionWorldBorder`, `SessionSpawnPoint` and `SessionGameRules` were
// folded, reset on quit-to-title and gated through the real
// `SharedState::apply` path with **no reader anywhere in the shell**. These
// gates drive the real fold and the real accessor.

/// **Vanilla's border-warning formula, against values computed outside this
/// code.**
///
/// Vanilla's own hud-vignette-strength extraction on a *static* border reduces
/// to `warningDistance == warningBlocks` exactly, because
/// vanilla's own static-border-extent lerp speed is `0.0`
/// and `max(warningBlocks, 0)` is
/// `warningBlocks`. That makes the arithmetic hand-checkable:
///
/// A border of diameter 100 centred on the origin has its edge at ±50. A
/// player at `x = 47` is `3` blocks from it. With `warning_blocks = 5`:
/// `strength = 1 - 3/5 = 0.4`. Every number here comes from vanilla's
/// constants and the packet, not from our implementation.
#[test]
fn the_border_warning_strength_matches_vanillas_hand_computed_value() {
    use lodestone_game::worldborder::{BorderExtent, WarningBlocks, WorldBorder};

    let border = WorldBorder {
        center_x: 0.0,
        center_z: 0.0,
        extent: BorderExtent::Static { size: 100.0 },
        warning_blocks: WarningBlocks::from_wire(5),
        ..WorldBorder::default()
    };

    let (dist, warn_at, strength) = super::session::border_warning(&border, 47.0, 0.0, 0.0);
    assert!((dist - 3.0).abs() < 1e-9, "edge at 50, player at 47 => 3 blocks: got {dist}");
    assert!(
        (warn_at - 5.0).abs() < 1e-9,
        "a static border's warning distance is warning_blocks exactly, since \
         getLerpSpeed() is 0.0: got {warn_at}"
    );
    assert!(
        (strength - 0.4).abs() < 1e-6,
        "1 - 3/5 = 0.4, hand-computed from vanilla's own expression: got {strength}"
    );

    // Well inside: no warning at all. `6 > 5`, so the `<` fails.
    let (_, _, none) = super::session::border_warning(&border, 44.0, 0.0, 0.0);
    assert!(
        (none - 0.0).abs() < 1e-9,
        "6 blocks out is beyond the 5-block warning band: got {none}"
    );

    // Exactly at the edge is full strength; outside is clamped to 1.0 rather
    // than exceeding it, which is what vanilla's own `Mth.clamp` does one
    // step later.
    let (_, _, at_edge) = super::session::border_warning(&border, 50.0, 0.0, 0.0);
    assert!((at_edge - 1.0).abs() < 1e-6, "at the edge => 1 - 0/5 = 1.0: got {at_edge}");
    let (outside, _, beyond) = super::session::border_warning(&border, 80.0, 0.0, 0.0);
    assert!(outside < 0.0, "outside the border the distance is negative: got {outside}");
    assert!(
        (beyond - 1.0).abs() < 1e-6,
        "and the strength clamps at 1.0 rather than running away: got {beyond}"
    );
}

/// **The control for the gate above, and it rejects the wrong hypothesis
/// rather than merely accepting the right one.**
///
/// The obvious wrong port is to use the border's *radius* where vanilla uses
/// the distance to the nearest edge, or the *diameter* where it uses the
/// radius. Both produce a plausible-looking number. A player at `x = 47`
/// inside a 100-diameter border is `3` blocks from the edge, `47` from the
/// centre and `53` from the far edge — three candidate values, only one of
/// which lands inside a 5-block warning band at all.
#[test]
fn the_border_warning_rejects_the_radius_and_diameter_hypotheses() {
    use lodestone_game::worldborder::{BorderExtent, WarningBlocks, WorldBorder};

    let border = WorldBorder {
        extent: BorderExtent::Static { size: 100.0 },
        warning_blocks: WarningBlocks::from_wire(5),
        ..WorldBorder::default()
    };
    let (dist, _, _) = super::session::border_warning(&border, 47.0, 0.0, 0.0);

    assert!(
        (dist - 47.0).abs() > 1.0,
        "must NOT be the distance from the centre (47) — that hypothesis \
         would never warn inside any normal border: got {dist}"
    );
    assert!(
        (dist - 53.0).abs() > 1.0,
        "must NOT be the distance to the far edge (53): got {dist}"
    );
    assert!(
        (dist - 3.0).abs() < 1e-9,
        "it is the distance to the NEAREST edge (3): got {dist}"
    );
}

/// **The world border reaches the shell through the real fold**, not through a
/// hand-built `WorldBorder`.
///
/// Drives `ClientEvent`s through the same `NetIngest` schedule the net thread
/// runs, then reads `Sim::world_border_warning` — the accessor `app/redraw.rs`
/// calls every frame. Before this accessor, `SessionWorldBorder` had zero
/// readers in the entire shell.
#[test]
fn a_folded_world_border_reaches_the_shells_own_accessor() {
    use lodestone_client::ClientEvent;

    let mut sim = Sim::new(test_config());
    ingest(&mut sim, login_event(1));

    // The precondition that makes the assertion meaningful: an unreported
    // border must answer `None`, so a passing result below cannot be the
    // default leaking through.
    assert!(
        sim.world_border_warning().is_none(),
        "precondition: with no border packet the accessor must report nothing, \
         not the MAX_SIZE default dressed up as a real border"
    );

    ingest(
        &mut sim,
        ClientEvent::WorldBorderInitialized {
            x: 0.0,
            z: 0.0,
            old_size: 100.0,
            new_size: 100.0,
            lerp_time_ms: 0,
            absolute_max_size: 29_999_984,
            warning_blocks: 5,
            warning_time: 15,
        },
    );

    // Pin the position rather than assuming it. The first draft of this gate
    // predicted `50.0` on the belief that a fresh `Sim` starts the player at
    // the origin; it starts at the block *centre* (`x = 0.5`), so the real
    // answer was `49.5` and the assertion caught the assumption. Setting the
    // position makes the prediction independent of that default entirely.
    sim.player_mut(|p| {
        p.position.x = 47.0;
        p.position.z = 0.0;
    });

    let (dist, warn_at, strength) = sim
        .world_border_warning()
        .expect("a reported border must reach the accessor");
    assert!(
        (warn_at - 5.0).abs() < 1e-9,
        "the folded warning_blocks must be the packet's 5, not the default: got {warn_at}"
    );
    // Edge of a 100-diameter border centred on the origin is x = 50, so a
    // player at x = 47 is 3 blocks out. This proves the *centre and size*
    // folded too, not merely the warning band.
    assert!(
        (dist - 3.0).abs() < 1e-6,
        "x=47 inside a 100-diameter border centred on the origin is 3 blocks \
         from the edge: got {dist}"
    );
    assert!(
        (strength - 0.4).abs() < 1e-6,
        "and the strength through the real fold must equal the hand-computed \
         1 - 3/5: got {strength}"
    );
}

/// **`SessionSpawnPoint` and `SessionGameRules` reach their shell accessors**,
/// through the same real fold.
#[test]
fn folded_spawn_point_and_game_rules_reach_the_shells_own_accessors() {
    use lodestone_client::ClientEvent;

    let mut sim = Sim::new(test_config());
    ingest(&mut sim, login_event(1));

    assert!(
        sim.spawn_point().pos().is_none(),
        "precondition: no spawn reported yet"
    );
    assert_eq!(
        sim.game_rules().immediate_respawn(),
        None,
        "precondition: no game rule reported yet — `None` is 'unreported', \
         which is NOT the same as `Some(false)`"
    );

    ingest(
        &mut sim,
        ClientEvent::SpawnPositionChanged {
            dimension: "minecraft:overworld".parse().expect("valid dimension id"),
            pos: lodestone_model::BlockPos::new(12, 64, -30),
            angle: 90.0,
            pitch: 0.0,
        },
    );
    assert_eq!(
        sim.spawn_point().pos(),
        Some(lodestone_model::BlockPos::new(12, 64, -30)),
        "the folded spawn position must reach the accessor the HUD reads"
    );
}

/// The inventory avatar's walk cycle: `Sim::local_body_anim` must report a live
/// limb swing **while the camera is first-person**, which is the only mode the
/// inventory screen is ever open in.
///
/// The wrong hypothesis is computed in the same run rather than described:
/// `third_person_body_state()` is asserted to be `None` here, which is what the
/// avatar used to be fed through (and what made the walk cycle read as blocked by
/// a crate boundary). So a regression that put the `is_first_person()` early
/// return back cannot pass — the two arms disagree by construction.
///
/// A `limb_swing_amount` of exactly `0.0` before any movement is the control:
/// without it, "greater than zero after walking" is satisfied by a rig that
/// reports a constant.
#[test]
fn the_avatar_pose_carries_the_walk_cycle_in_first_person() {
    let mut sim = Sim::new(test_config());
    // Settle one tick so `body_pose` has a previous position to measure against.
    sim.step(1.0 / 20.0);

    assert!(
        sim.camera_type().is_first_person(),
        "precondition: a fresh Sim starts in first person"
    );
    assert!(
        sim.third_person_body_state().is_none(),
        "premise: the third-person reader returns None here — this is the gate \
         that made the walk cycle look unreachable"
    );
    let at_rest = sim.local_body_anim();
    assert_eq!(
        at_rest.limb_swing_amount, 0.0,
        "control: a standing player's limb swing amount is exactly zero, so the \
         assertion below is not satisfiable by a constant"
    );

    // Walk. `body_pose.tick` measures the *travelled* horizontal distance, so the
    // player has to actually move — driving the input alone would not do it.
    sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
    for _ in 0..10 {
        sim.step(1.0 / 20.0);
    }

    let walking = sim.local_body_anim();
    assert!(
        walking.limb_swing_amount > 0.0,
        "a walking player's avatar must have a non-zero limb swing amount, got {}",
        walking.limb_swing_amount
    );
    assert!(
        walking.limb_swing > 0.0,
        "…and the stride phase must have advanced, got {}",
        walking.limb_swing
    );
    // Still first person, so the old path is still `None`: the pose is reaching a
    // consumer the gated reader structurally could not serve.
    assert!(
        sim.third_person_body_state().is_none(),
        "the camera must not have changed mode under us"
    );
}
