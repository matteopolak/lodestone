use super::*;
#[test]
fn camera_interpolates_between_ticks() {
    // Force a known prev/current split and a half-way alpha, then check the
    // camera eye sits between the two feet positions.
    let mut sim = Sim::new(test_config());
    sim.set_prev_position(Vec3d::new(0.0, 64.0, 0.0));
    sim.player_mut(|p| p.position = Vec3d::new(10.0, 64.0, 0.0));
    sim.clock_mut(|c| c.interp_alpha = 0.5);
    let cam = sim.camera(1.0);
    assert!(
        (cam.position.x - 5.0).abs() < 1e-4,
        "expected midpoint x=5, got {}",
        cam.position.x
    );
}

#[test]
fn frames_per_tick_tracks_ratio() {
    let mut sim = Sim::new(test_config());
    // Two frames of one full tick each ⇒ 2 frames / 2 ticks = 1.0.
    sim.step(1.0 / 20.0);
    sim.step(1.0 / 20.0);
    assert!((sim.frames_per_tick() - 1.0).abs() < 1e-6);
    // A frame with no accumulated tick still counts as a frame, so the
    // frames-per-tick ratio rises above 1.
    sim.step(0.0);
    assert!(sim.frames_per_tick() > 1.0, "extra frame raises the ratio");
}

#[test]
fn sprint_moves_faster_than_walk_via_attribute_seam() {
    // Walk forward for a second, then sprint the same time from the same
    // spot; sprinting must cover more ground. This drives the physics
    // `with_movement_speed` seam from a real caller.
    //
    // The local world is now real vanilla terrain (`lodestone-worldgen`),
    // so spawn sits on a slope and walking north walls the player out after
    // ~0.2 blocks — a wall, not the speed seam, would otherwise decide the
    // result. Flatten a private corridor along the walking line so what we
    // measure is physics speed and nothing else.
    fn distance(sprint: bool) -> f64 {
        let mut sim = Sim::new(test_config());
        // Player spawns at (0.5, feet, 0.5) facing north (-Z, yaw 180).
        // Lay a solid floor and clear head-room along -Z so the walk is
        // unobstructed regardless of the generated surface.
        let feet_y = sim.player().position.y.floor() as i32;
        for dz in -25..=1 {
            for dx in -1..=1 {
                sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
                sim.set_block_world([dx, feet_y, dz], id::AIR);
                sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
                sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
            }
        }
        // Settle on the fresh floor first.
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        let start = sim.player().position;
        sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
        sim.input_mut(|i| i.set(lodestone_controller::Action::Sprint, sprint));
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        let d = sim.player().position.subtract(start);
        (d.x * d.x + d.z * d.z).sqrt()
    }
    let walk = distance(false);
    let sprint = distance(true);
    assert!(
        sprint > walk * 1.1,
        "sprint ({sprint:.3}) should clearly exceed walk ({walk:.3})"
    );
}

/// Swimming has to reach the *player*, not just exist in the physics crate.
/// Flood a pool in the demo world (whose palette has a real water block), hold
/// sprint + forward, and check the pose actually flips: `swimming` set, the eye
/// dropped to `Pose.SWIMMING`'s `0.4`, and the camera moved with it.
///
/// The first phase is the control: standing in exactly the same water without
/// sprinting must **not** swim, so the assertions below are about sprinting
/// while submerged and not about "being wet".
#[test]
fn sprinting_underwater_enters_the_swim_pose_and_drops_the_camera() {
    let mut sim = Sim::new(test_config());
    let feet_y = sim.player().position.y.floor() as i32;
    // A private pool: stone floor, water from the feet to well over the eye,
    // wide enough that a second of swimming (~1 block) stays inside it. Filling
    // the column with water is also what flattens the generated slope the player
    // spawns on — see `sprint_moves_faster_than_walk_via_attribute_seam`.
    for dz in -5..=5 {
        for dx in -5..=5 {
            sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
            for dy in 0..=4 {
                sim.set_block_world([dx, feet_y + dy, dz], id::WATER);
            }
        }
    }

    for _ in 0..10 {
        sim.step(1.0 / 20.0);
    }
    assert!(
        sim.fluid_state().under_water(),
        "the pool must actually submerge the eye, or this gate proves nothing"
    );
    assert!(
        !sim.player().swimming,
        "control: submerged but not sprinting is not swimming"
    );
    assert_eq!(
        sim.player().eye_height,
        lodestone_physics::player::DEFAULT_EYE_HEIGHT
    );

    sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
    sim.input_mut(|i| i.set(lodestone_controller::Action::Sprint, true));
    // Step until the pose flips, so the tick the change lands on is known.
    let mut ticks_to_swim = None;
    for tick in 0..10 {
        sim.step(1.0 / 20.0);
        if sim.player().swimming {
            ticks_to_swim = Some(tick);
            break;
        }
    }
    assert!(
        ticks_to_swim.is_some(),
        "sprinting while submerged must enter the swim pose"
    );
    assert_eq!(
        sim.player().eye_height,
        SWIMMING_EYE_HEIGHT,
        "the shell owns the pose eye height; physics only reads it"
    );

    // Helper: pin the *position* interpolation so a camera assertion is about
    // the eye height, not about where between two ticks the feet are.
    //
    // `alpha` is deliberately a parameter, because it selects **which** of the
    // smoother's two values you see: `lerp(0.0)` is the *previous* tick's eased
    // eye height and `lerp(1.0)` is this tick's. That is the whole point of the
    // `O` twin, and reading at `0.0` right after a pose flip therefore shows the
    // pre-flip height — correct, and not what a mid-ease assertion wants.
    let camera_offset = |sim: &mut Sim, alpha: f32| {
        let settled = sim.player().position;
        sim.set_prev_position(settled);
        sim.clock_mut(|c| c.interp_alpha = alpha);
        sim.camera(1.0).position.y - sim.player().position.y as f32
    };

    // **The camera must NOT have snapped.** Vanilla's own per-tick camera
    // update eases its own eye
    // height toward the entity's — `eyeHeight += (target - eyeHeight) * 0.5F` —
    // so one tick after the pose flips it is still most of the way up at the
    // standing height. This is the assertion that proves `Sim::camera` reads
    // `eye_height_smoother` and not the raw pose value; before that existed the
    // view jerked 1.22 blocks in a single frame on entering water.
    let standing = lodestone_physics::player::DEFAULT_EYE_HEIGHT;
    let after_flip = camera_offset(&mut sim, 1.0);
    assert!(
        after_flip > SWIMMING_EYE_HEIGHT + 0.1 && after_flip < standing,
        "camera should be mid-ease between {SWIMMING_EYE_HEIGHT} and {standing} \
         one tick after the pose flip, got {after_flip}"
    );

    // …and it must converge. Each tick halves the remaining gap, so the
    // original `1e-4` tolerance needs ~14 ticks from a 1.22-block step; 24 is
    // comfortably past it without being sensitive to the exact rate.
    for _ in 0..24 {
        sim.step(1.0 / 20.0);
    }
    let settled_offset = camera_offset(&mut sim, 1.0);
    assert!(
        (settled_offset - SWIMMING_EYE_HEIGHT).abs() < 1e-4,
        "swim camera should settle {SWIMMING_EYE_HEIGHT} above the feet: got \
         {settled_offset}"
    );
}

/// Sneak is how you swim *downward* (`goDownInWater`), so the land-side
/// "sneaking cancels sprint" gate must not apply while submerged — otherwise
/// holding shift underwater stops the swim dead. Control: the same shift+sprint
/// on dry land still cancels sprint.
///
/// The *rule* now lives in `lodestone_controller::swim_adjusted_intent` and
/// is tested there against the pure function, and in that crate's
/// `the_intent_system_reads_submersion_for_the_swim_exception` against the
/// system. This one is deliberately kept as well, and asserts something
/// neither of those can: that a `Sim::step` — the real driver, with the real
/// `RawInput` resource and the real `Submersion` component — reaches the
/// intent the physics set will read. Without it, `Sim` could stop feeding the
/// ECS entirely and both of the controller's tests would still pass.
#[test]
fn sneak_cancels_sprint_on_land_but_not_under_water() {
    let mut sim = Sim::new(test_config());
    sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
    sim.input_mut(|i| i.set(lodestone_controller::Action::Sprint, true));
    sim.input_mut(|i| i.set(lodestone_controller::Action::Sneak, true));

    sim.step(lodestone_ecs::TICK_PERIOD);
    assert!(
        !sim.movement_intent().sprint,
        "control: on land, sneaking still vetoes sprint"
    );

    sim.set_fluid_state(FluidState {
        water_height: 2.0,
        eye_in_water: true,
        ..FluidState::NONE
    });
    sim.step(lodestone_ecs::TICK_PERIOD);
    let intent = sim.movement_intent();
    assert!(
        intent.sprint,
        "submerged, shift must not cancel a swim-sprint"
    );
    assert!(
        intent.sneak,
        "…and shift itself must survive, or the sink impulse is lost"
    );
}

/// The server derives the swimming pose itself, from `isSprinting()` — and it
/// only learns that from `ServerboundPlayerCommandPacket`, never from the input
/// packet's `sprint` bit. So the sprint *edge* has to reach the wire as a
/// `PlayerCommand`, exactly once per change.
#[test]
fn sprint_edges_reach_the_wire_as_player_commands() {
    use crate::net::NetUpdate;
    use lodestone_ecs::ecs::system::RunSystemOnce;
    use lodestone_model::PlayerCommand;

    let (net, actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    // Both halves of one login packet, because the packet carries the entity id
    // and `send_sprint_command` will not send without one. `NetUpdate::LoggedIn`
    // drives the phase (and therefore `Egress::in_world`); `ClientEvent::Login`
    // is what folds `ServerEntityId`, on the net thread, since the vitals
    // collapse deleted `poll_net`'s duplicate `set_server_entity_id` write.
    // Feeding only the `NetUpdate` left the id `None`, which made the whole
    // test a *precondition*-species vacuity: the query hit
    // `let Some(entity_id) = … else { continue }` every time, so the two
    // "no packet" assertions below held for a reason that had nothing to do
    // with edge-triggering.
    ingest(&mut sim, login_event(7));
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    sim.poll_net();
    assert_eq!(
        sim.server_entity_id(),
        Some(7),
        "setup: without the folded id no sprint command can be sent at all, \
         and every assertion below passes vacuously"
    );
    while actions.try_recv().is_ok() {}

    // `EndClientTick` is filtered out, not asserted on: `drain_action_queue`
    // appends vanilla's tick tail on every call once `Egress::in_world` holds
    // (see its own doc), and `sprint_once` below sets `in_world`. This test is
    // about the sprint *edge*, and the tail is exactly as much noise here as the
    // per-tick movement packet the comment below explains away — that packet is
    // avoided by running one system rather than the schedule, which cannot work
    // for something the drain itself adds.
    // `connected_sim_emits_one_move_per_physics_tick` is where the tail is
    // asserted, so filtering here does not hide it from every gate.
    let drain = |actions: &std::sync::mpsc::Receiver<ClientAction>| -> Vec<ClientAction> {
        std::iter::from_fn(|| actions.try_recv().ok())
            .filter(|a| !matches!(a, ClientAction::EndClientTick))
            .collect()
    };

    // Since Stage 5 the sprint edge is `crate::interact::send_sprint_command`,
    // a `TickSet::Send` system. Run *that system* and then the driver's own
    // queue drain, rather than the whole `GameTick` schedule: the schedule also
    // emits the per-tick movement packet, which would swamp the
    // "no edge, no packet" assertions below. Deliberately **not** an assertion
    // on `ActionQueue` — the queue is not the wire, and this test's whole point
    // is that the command reaches the socket.
    //
    // `Egress` has to be set by hand for the same reason the old direct call
    // needed no gate: the demo fixture has no vanilla atlas, so `is_live()` is
    // false and `step` would derive `live: false`. The gate moved from the call
    // site into the system, which is where `send_player_input` already keeps
    // its identical one.
    let sprint_once = |sim: &mut Sim| {
        {
            let mut world = sim.ecs().write();
            world.insert_resource(Egress {
                in_world: true,
                live: true,
            });
            world
                .run_system_once(crate::interact::send_sprint_command)
                .expect("send_sprint_command runs");
        }
        sim.drain_action_queue();
    };

    // Not sprinting and never was: no packet at all (vanilla's `wasSprinting`
    // starts false).
    sprint_once(&mut sim);
    assert!(
        drain(&actions).is_empty(),
        "no sprint edge, no sprint packet"
    );

    sim.player_mut(|p| p.sprinting = true);
    sprint_once(&mut sim);
    assert_eq!(
        drain(&actions),
        vec![ClientAction::PlayerCommand {
            entity_id: 7,
            command: PlayerCommand::StartSprinting,
        }]
    );

    // Edge-triggered: holding sprint must not spam the server every tick.
    sprint_once(&mut sim);
    sprint_once(&mut sim);
    assert!(drain(&actions).is_empty(), "sprint is edge-triggered");

    sim.player_mut(|p| p.sprinting = false);
    sprint_once(&mut sim);
    assert_eq!(
        drain(&actions),
        vec![ClientAction::PlayerCommand {
            entity_id: 7,
            command: PlayerCommand::StopSprinting,
        }]
    );
}

/// Sneaking can cancel sprint on the same tick. The dedicated sprint command
/// must reach the queue after the changed input bitset and before the position
/// computed from that input.
#[test]
fn sneak_that_stops_sprinting_orders_command_input_then_move() {
    use crate::net::NetUpdate;
    use lodestone_ecs::player::{LastPlayerInput, LastSprintingSent};
    use lodestone_model::{PlayerCommand, PlayerInput};

    let (net, actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(client_config());
    sim.attach_net(net);
    ingest(&mut sim, login_event(7));
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    sim.poll_net();
    while actions.try_recv().is_ok() {}

    // The preceding physics phase has already ended sprinting for this sneak
    // edge. Pin that result directly: a loopback client has no collision view,
    // so its physics path deliberately cannot derive the live-world result.
    sim.player_mut(|player| player.sprinting = false);
    sim.write_local(|world, local| {
        world.insert_resource(Egress {
            in_world: true,
            live: true,
        });
        world.get_mut::<LastSprintingSent>(local).unwrap().0 = Some(true);
        world.get_mut::<LastPlayerInput>(local).unwrap().0 = Some(PlayerInput {
            sprint: true,
            ..PlayerInput::EMPTY
        });
        world.resource_mut::<RawInput>().0.set(
            lodestone_controller::Action::Sneak,
            true,
        );
        world.run_schedule(lodestone_ecs::GameTick);
    });
    sim.drain_action_queue();

    let relevant = std::iter::from_fn(|| actions.try_recv().ok())
        .filter(|action| {
            matches!(
                action,
                ClientAction::PlayerCommand { .. }
                    | ClientAction::SetPlayerInput(_)
                    | ClientAction::Move { .. }
            )
        })
        .collect::<Vec<_>>();
    assert!(
        matches!(
            relevant.as_slice(),
            [
                ClientAction::SetPlayerInput(PlayerInput {
                    shift: true,
                    sprint: false,
                    ..
                }),
                ClientAction::PlayerCommand {
                    command: PlayerCommand::StopSprinting,
                    ..
                },
                ClientAction::Move { .. }
            ]
        ),
        "combined sneak/sprint edge must be PlayerInput -> StopSprinting -> Move, got {relevant:?}"
    );
}

#[test]
fn breaking_the_target_clears_it_and_schedules_a_remesh() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    // Aim straight down at the block under the player's feet.
    let feet = sim.player().position;
    sim.set_target(Some(crate::raycast::RayHit::face_center(
        [
            feet.x.floor() as i32,
            feet.y.floor() as i32 - 1,
            feet.z.floor() as i32,
        ],
        [0, 1, 0],
    )));
    assert!(sim.break_block(), "should break the solid block");
    assert!(sim.target().is_none(), "target cleared after break");
    assert!(sim.pending_meshes() > 0, "a remesh was scheduled");
}

// -----------------------------------------------------------------------
// Arm swing: the producer -> consumer wiring
// -----------------------------------------------------------------------
//
// `lodestone_entity::pose` proves the swing clock ticks and
// `lodestone_render::entity` proves the arm matrix moves. Neither can prove
// that anything in this shell ever *starts* a swing — the failure this repo
// has hit nine times. These gates assert the seam: a swing produced the way
// the real producers produce one reaches `hand_swing_progress` (which
// `app.rs` hands `RenderState::set_hand_swing_source`) and
// `third_person_body_state` (which feeds the self-avatar's
// `setupAttackAnimation`).

/// Aim straight down at the block under the player's feet, like
/// `breaking_the_target_clears_it_and_schedules_a_remesh`.
fn aim_at_the_floor(sim: &mut Sim) {
    let feet = sim.player().position;
    sim.set_target(Some(crate::raycast::RayHit::face_center(
        [
            feet.x.floor() as i32,
            feet.y.floor() as i32 - 1,
            feet.z.floor() as i32,
        ],
        [0, 1, 0],
    )));
}

/// Run whole ticks and report the largest swing progress seen.
pub(super) fn peak_swing_over(sim: &mut Sim, ticks: u32) -> f32 {
    let mut peak = 0.0f32;
    for _ in 0..ticks {
        sim.step(1.0 / 20.0);
        peak = peak.max(sim.hand_swing_progress());
    }
    peak
}

#[test]
fn a_queued_main_hand_swing_reaches_the_arm_pose() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();

    // The negative control first, and it is the one that matters: with no
    // swing produced, the arm must sit at exact rest for the whole window.
    // Without this, "progress > 0" is also satisfied by a clock that free-runs
    // off frame time — which is the specific bug `entities.rs` documents
    // finding in the limb-swing code.
    let idle_peak = peak_swing_over(&mut sim, 20);
    assert_eq!(
        idle_peak, 0.0,
        "an idle player's arm must be at rest, but progress peaked at {idle_peak}"
    );

    // Now produce a swing exactly the way `lodestone_game::mining` does — it
    // pushes `SwingArm { Main }` onto `ActionQueue`, and `drive_mining`
    // forwards that queue verbatim. `mining.rs`'s own tests already pin that
    // it emits one; this pins that the shell animates it.
    sim.write(|w| {
        w.resource_mut::<ActionQueue>()
            .0
            .push(ClientAction::SwingArm { hand: Hand::Main });
    });
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "a queued main-hand swing must drive the arm pose, but progress \
         peaked at only {peak} — `drain_action_queue` is not calling `swing_hand`, \
         or `hand_swing_progress` is not reading the clock it sets"
    );

    // And it ends: the swing is 6 ticks, so well after that the arm is rested
    // again. A swing that never finishes reads as a permanently cocked arm.
    let after = peak_swing_over(&mut sim, 30);
    assert_eq!(
        after, 0.0,
        "the swing must return to rest, but progress still peaked at {after}"
    );
}

/// An **off-hand** swing must not drive the arm. `drain_action_queue` matches
/// on `Hand::Main` specifically; without this control that match is untested
/// and a `SwingArm { .. }` wildcard would swing the right arm for a left-hand
/// action.
#[test]
fn an_off_hand_swing_does_not_drive_the_main_arm() {
    let mut sim = Sim::new(test_config());
    sim.write(|w| {
        w.resource_mut::<ActionQueue>()
            .0
            .push(ClientAction::SwingArm { hand: Hand::Off });
    });
    let peak = peak_swing_over(&mut sim, 10);
    assert_eq!(
        peak, 0.0,
        "an off-hand swing must leave the main arm at rest, got {peak}"
    );
}

/// The demo world has no action queue to piggy-back on, so `break_block` and
/// `place_block` start the swing themselves. This is the only world a headless
/// scene can exercise, so if it did not swing, no offline gate ever could.
#[test]
fn a_demo_world_break_swings_the_arm() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    aim_at_the_floor(&mut sim);
    // Load-bearing: if the break did not happen this test would pass
    // vacuously by asserting nothing about a swing that was never produced.
    assert!(sim.break_block(), "the demo block should have broken");
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "a demo-world break must swing the arm, progress peaked at {peak}"
    );
}

/// A demo-world left-click with **nothing** targeted must still swing — the
/// client attack dispatch swings unconditionally after target selection,
/// `MISS` included. `Sim::begin_attack` must call the swing path in addition
/// to `break_block()` on the demo world,
/// which swings only on a *successful* break and produces nothing when
/// there is no target.
#[test]
fn begin_attack_swings_the_arm_on_a_demo_world_miss() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert!(
        sim.target().is_none(),
        "test setup: nothing should be targeted yet"
    );
    sim.begin_attack();
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "a miss must still swing the arm, progress peaked at {peak}"
    );
}

/// Regression companion to the miss test above: routing `begin_attack`
/// through the new demo/live split must not break the existing
/// successful-break path.
#[test]
fn begin_attack_still_breaks_a_targeted_demo_block() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    aim_at_the_floor(&mut sim);
    sim.begin_attack();
    assert!(
        sim.target().is_none(),
        "a successful break clears the target, as `break_block` always did"
    );
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "breaking a targeted demo block must still swing, progress peaked at {peak}"
    );
}

/// The live-path miss case: no block, no entity, and the arm still
/// swings. Exercises `begin_attack_live` directly (no net connection is
/// needed — the swing is client-side and does not require one, matching
/// every other swing site's contract).
#[test]
fn begin_attack_live_swings_on_a_miss() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert!(sim.target().is_none());
    assert!(sim.entity_target().is_none());
    sim.begin_attack_live();
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "a live miss must still swing the arm, progress peaked at {peak}"
    );
}

/// The `BLOCK`-only case: with no entity targeted, `begin_attack_live`
/// must still arm the hold-to-mine loop exactly as it did before this
/// change (the pre-existing, unmodified behaviour this fix must not
/// regress).
#[test]
fn begin_attack_live_arms_mining_when_only_a_block_is_targeted() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    aim_at_the_floor(&mut sim);
    sim.begin_attack_live();
    let attacking = sim.read(|w| w.resource::<Attacking>().0);
    assert!(
        attacking,
        "a block-only target must still arm the hold-to-mine loop"
    );
}

/// `case ENTITY` takes priority over `case BLOCK`: with both an entity and
/// a block targeted, attacking the entity must swing the arm and must
/// **not** also arm the hold-to-mine loop — vanilla's `hitResult` is one
/// value, never both at once.
#[test]
fn begin_attack_live_prefers_an_entity_target_over_mining() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    aim_at_the_floor(&mut sim);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));
    sim.begin_attack_live();
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "attacking an entity target must swing the arm, progress peaked at {peak}"
    );
    let attacking = sim.read(|w| w.resource::<Attacking>().0);
    assert!(
        !attacking,
        "an entity attack must not also arm the hold-to-mine loop"
    );
}

/// The owner's own bug report: punching the air must put a `SwingArm` on the
/// wire, not just animate the local arm. `begin_attack_live_swings_on_a_miss`
/// above already proved the *local* half; it used no net connection at all
/// (deliberately, per its own doc) and so could not have caught this — the
/// wire send and the local animation were, before this fix, two different
/// calls (`ActionQueue`'s `SwingArm` vs `Sim::swing_hand` directly), and only
/// one of them was reached on a miss. A client with no `SwingArm` on the wire
/// here is exactly the reported defect: other players never see the swing.
#[test]
fn begin_attack_live_sends_swing_arm_on_the_wire_on_a_miss() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    assert!(sim.target().is_none(), "precondition: no block targeted");
    assert!(sim.entity_target().is_none(), "precondition: no entity targeted");

    sim.begin_attack_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main })),
        "a live miss must send a main-hand SwingArm over the wire so other \
         players see it, got {sent:?}"
    );
}

/// The `ENTITY` arm has the identical defect, one hop earlier: `attack_entity`
/// sends `InteractEntity { interaction: Attack, .. }` but never `SwingArm`
/// itself, and `begin_attack_live` used to reach only the local-only
/// `Sim::swing_hand` afterward. Vanilla's own animation call (`player.swing`
/// in `Minecraft.startAttack`) is unconditional and outside the switch, so it
/// covers `ENTITY` too — the attack packet does not carry the swing.
#[test]
fn begin_attack_live_sends_swing_arm_on_the_wire_on_an_entity_hit() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));

    sim.begin_attack_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    let attack = sent.iter().position(|a| {
        matches!(
            a,
            ClientAction::InteractEntity {
                interaction: EntityInteraction::Attack,
                ..
            }
        )
    });
    let swing = sent
        .iter()
        .position(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main }));
    assert!(
        attack.is_some(),
        "control precondition: the attack packet itself must still be sent, \
         or the absence below is just an unwired Sim — got {sent:?}"
    );
    assert!(
        swing.is_some(),
        "attacking an entity must also send a main-hand SwingArm over the \
         wire so other players see the swing, got {sent:?}"
    );
    assert!(
        attack < swing,
        "vanilla's `MultiPlayerGameMode.attack` sends the attack packet, and \
         only afterward does `Minecraft.startAttack`'s unconditional \
         `player.swing(...)` send the swing — got {sent:?}"
    );
}

/// A dead local player must not attack — mirrors `use_item_live`'s own
/// `is_dead()` guard, and vanilla drops input entirely on the death
/// screen.
#[test]
fn begin_attack_live_does_nothing_while_dead() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let local = sim.local_player();
    sim.write(|w| {
        w.entity_mut(local).insert(Dead);
        w.resource_mut::<EntityRayTarget>().0 = Some(42);
    });
    sim.begin_attack_live();
    let peak = peak_swing_over(&mut sim, 10);
    assert_eq!(peak, 0.0, "a dead player must not swing on attack");
}

/// `ClientAction::SpectatorAction` must have a producer. The spectator branch is
/// checked *before* any item or
/// hit-result logic — [`begin_attack_live_prefers_an_entity_target_over_mining`]
/// above proves the ordinary switch prefers an entity target; this proves a
/// spectator's left-click on that same entity target takes a completely
/// different branch and never reaches `attack_entity`/`Attacking` at all.
#[test]
fn begin_attack_live_spectates_the_entity_target_instead_of_attacking() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    let local = sim.local;
    sim.write(|w| {
        w.entity_mut(local)
            .insert(ServerGameMode(Some(lodestone_client::GameMode::Spectator)));
        w.resource_mut::<EntityRayTarget>().0 = Some(42);
    });

    sim.begin_attack_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert_eq!(
        sent,
        vec![ClientAction::SpectatorAction {
            target_entity_id: Some(42)
        }],
        "a spectator's left-click on an entity must send SpectatorAction(Some(id)) \
         and nothing else — no attack packet, no swing"
    );
    let attacking = sim.read(|w| w.resource::<Attacking>().0);
    assert!(!attacking, "spectating must not arm the hold-to-mine loop");
}

/// The other half of the same gate: a spectator's left-click with no entity
/// target (a block, or nothing at all) sends `SpectatorAction(None)` —
/// `MultiPlayerGameMode.spectatorNoAction`. Distinct from the miss-swings-the-arm
/// case a non-spectator hits, matching vanilla's own "neither arm swings"
/// behaviour.
#[test]
fn begin_attack_live_sends_no_target_spectator_action_on_a_miss() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    let local = sim.local;
    sim.write(|w| {
        w.entity_mut(local)
            .insert(ServerGameMode(Some(lodestone_client::GameMode::Spectator)));
    });
    assert!(sim.entity_target().is_none(), "precondition: no entity targeted");

    sim.begin_attack_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert_eq!(
        sent,
        vec![ClientAction::SpectatorAction {
            target_entity_id: None
        }],
        "a spectator's left-click with no entity under the crosshair must send \
         SpectatorAction(None), got {sent:?}"
    );
    let peak = peak_swing_over(&mut sim, 10);
    assert_eq!(peak, 0.0, "a spectator's click must not swing the arm either way");
}

/// Puts `item` into the local player's main-hand hotbar slot (native
/// index 0, [`Sim::selected_slot`]'s default) via the same
/// [`lodestone_ecs::SessionMenus`] fold a real `ContainerSetSlot`
/// packet drives — the pattern
/// `closing_a_server_menu_clears_it_locally_without_waiting_for_the_server`
/// already established for writing menu state directly in a hermetic
/// test.
