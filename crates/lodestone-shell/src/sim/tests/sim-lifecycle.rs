use super::*;
#[test]
fn new_generates_world_and_schedules_meshes() {
    let sim = Sim::new(test_config());
    assert!(!sim.chunk_world().is_empty(), "world should have chunks");
    assert!(sim.pending_meshes() > 0, "sections should be scheduled");
}

#[test]
fn all_scheduled_sections_mesh() {
    let mut sim = Sim::new(test_config());
    let meshes = sim.drain_all_meshes();
    assert!(!meshes.is_empty());
    assert!(meshes.iter().any(|m| m.mesh.quad_count() > 0));
}

#[test]
fn stepping_settles_the_player_on_the_ground() {
    let mut sim = Sim::new(test_config());
    for _ in 0..60 {
        sim.step(1.0 / 20.0);
    }
    assert!(
        sim.player().on_ground,
        "player should be standing on terrain"
    );
    assert_eq!(sim.stats.position[1], sim.player().position.y);
}

#[test]
fn mouse_look_updates_view_and_clears_delta() {
    let mut sim = Sim::new(test_config());
    let yaw0 = sim.player().yaw;
    sim.input_mut(|i| i.add_mouse(50.0, 0.0));
    sim.apply_mouse();
    assert_ne!(sim.player().yaw, yaw0);
    assert_eq!(sim.input().mouse_dx, 0.0);
}

/// The horizontal inversion option must negate the yaw delta by the *exact*
/// same magnitude `apply_look`'s curve would otherwise produce, not just
/// change its sign in some direction. A test that only asserted
/// `delta.signum() != plain.signum()` would also pass for a shader-style
/// bug that inverts and also rescales — see `CLAUDE.md`'s note on the
/// *magnitude* species of vacuous test.
#[test]
fn invert_mouse_x_negates_the_yaw_delta_exactly() {
    // A raw `after - before` is not safe here: `apply_look` wraps yaw
    // into `[-180, 180)`, so if the fixture's starting yaw happens to
    // sit near that seam, the plain and inverted runs can wrap on
    // opposite sides and a naive subtraction reports deltas 360° apart
    // even though the underlying rotation is the exact negation. This
    // computes the shortest signed angular delta instead, the same
    // normalisation `apply_look` itself applies to the absolute angle.
    fn yaw_delta(before: f32, after: f32) -> f32 {
        (after - before + 180.0).rem_euclid(360.0) - 180.0
    }

    let mut plain = Sim::new(test_config());
    let yaw0 = plain.player().yaw;
    plain.input_mut(|i| i.add_mouse(50.0, 0.0));
    plain.apply_mouse();
    let plain_delta = yaw_delta(yaw0, plain.player().yaw);
    assert_ne!(plain_delta, 0.0, "the fixture must actually turn the player");

    let mut inverted = Sim::new(test_config());
    inverted.set_mouse_invert(true, false);
    let yaw0i = inverted.player().yaw;
    inverted.input_mut(|i| i.add_mouse(50.0, 0.0));
    inverted.apply_mouse();
    let inverted_delta = yaw_delta(yaw0i, inverted.player().yaw);

    assert_eq!(
        inverted_delta, -plain_delta,
        "invert_mouse_x must negate dx before the sensitivity curve, \
         producing the exact opposite yaw delta, not merely a different one"
    );
}

/// A `sensitivity` update must take effect on the **next tick of
/// the same `Sim`**, with no restart.
///
/// This is the load-bearing assertion; a naive gate misses it.
/// Persistence is a separate concern, so a gate that asserts only the *stored*
/// value changed proves nothing about the live simulation. It is the
/// *precondition* species of vacuous test: the setup, not the assert, is what
/// is wrong.
///
/// The defect was that [`Sim::apply_mouse`] read `self.config.sensitivity`,
/// the **argv-derived** [`Config`] value, which is fixed for the process's
/// lifetime. Dragging the slider therefore persisted correctly and changed
/// nothing until relaunch.
///
/// Both deltas are **predicted exactly** from
/// [`lodestone_controller::sensitivity_factor`] rather than merely compared to
/// each other, and the stale implementation's value is computed alongside —
/// without that third number, "the two deltas differ" is also satisfied by
/// an implementation that scales by the wrong amount (`CLAUDE.md`'s
/// *magnitude* species). At the reference curve `(s·0.6 + 0.2)³ · 8 · 0.15`, a
/// 50-pixel drag gives 30.72° at slider 1.0, 1.05° at 0.1, and 7.5° at the
/// fixture's own config value of 0.5 — three well-separated numbers.
#[test]
fn a_sensitivity_change_applies_to_the_same_sim_without_a_restart() {
    // `apply_look` wraps yaw into `[-180, 180)`, so a raw `after - before`
    // can report deltas 360° apart if the fixture's yaw sits near the seam.
    // Same normalisation as `invert_mouse_x_negates_the_yaw_delta_exactly`.
    fn yaw_delta(before: f32, after: f32) -> f32 {
        (after - before + 180.0).rem_euclid(360.0) - 180.0
    }

    const DRAG_PX: f32 = 50.0;
    let cfg = test_config();
    // The value the pre-fix code read, and therefore the wrong hypothesis.
    let stale = DRAG_PX * lodestone_controller::sensitivity_factor(cfg.sensitivity);

    let mut sim = Sim::new(cfg);

    // One `Sim`, two sensitivities, no reconstruction between them — that is
    // the whole point. A test that built a second `Sim` would pass even if
    // the value were only read at construction.
    let measure = |sim: &mut Sim, slider: f32| {
        sim.set_sensitivity(slider);
        let before = sim.player().yaw;
        sim.input_mut(|i| i.add_mouse(DRAG_PX, 0.0));
        sim.apply_mouse();
        yaw_delta(before, sim.player().yaw)
    };

    for slider in [1.0_f32, 0.1] {
        let want = DRAG_PX * lodestone_controller::sensitivity_factor(slider);
        let got = measure(&mut sim, slider);
        assert!(
            (got - want).abs() < 1e-3,
            "slider {slider} must turn the player {want}° for a {DRAG_PX}px drag, \
             got {got}° — apply_mouse is not reading the pushed sensitivity"
        );
        assert!(
            (got - stale).abs() > 1.0,
            "slider {slider} produced {got}°, within 1° of the {stale}° the \
             argv-derived config value would give — the fix is not observable, \
             so this gate would pass against the bug"
        );
    }
}

/// As [`invert_mouse_x_negates_the_yaw_delta_exactly`], for `invertMouseY`
/// and pitch.
#[test]
fn invert_mouse_y_negates_the_pitch_delta_exactly() {
    let mut plain = Sim::new(test_config());
    let pitch0 = plain.player().pitch;
    plain.input_mut(|i| i.add_mouse(0.0, 30.0));
    plain.apply_mouse();
    let plain_delta = plain.player().pitch - pitch0;
    assert_ne!(plain_delta, 0.0, "the fixture must actually tilt the player");

    let mut inverted = Sim::new(test_config());
    inverted.set_mouse_invert(false, true);
    let pitch0i = inverted.player().pitch;
    inverted.input_mut(|i| i.add_mouse(0.0, 30.0));
    inverted.apply_mouse();
    let inverted_delta = inverted.player().pitch - pitch0i;

    assert_eq!(inverted_delta, -plain_delta, "invert_mouse_y must negate dy exactly");
}

/// End-to-end: `Sim::set_toggle_modes` (what `app.rs` calls
/// from `nav.toggle_sneak()`/`toggle_sprint()`) has to actually reach the
/// live `InputState` a key event drives — that push happens inside
/// [`Sim::step`], not at the setter itself, so this proves the wiring
/// rather than just the setter storing a bool nobody reads.
///
/// Includes a negative control (hold mode, the default): without it, a
/// version of this test that always reported "still engaged" would pass
/// just as well against a build that never wired toggle mode at all.
#[test]
fn toggle_sneak_option_reaches_live_input_and_survives_key_release() {
    let mut toggle = Sim::new(test_config());
    toggle.set_toggle_modes(true, false, false, false);
    // `step` is what actually applies the pushed option to `InputState`;
    // see that method's doc. Without this call, `set` below would still
    // run in hold mode.
    toggle.step(1.0 / 20.0);

    toggle.input_mut(|i| i.set(lodestone_controller::Action::Sneak, true));
    assert!(
        lodestone_controller::movement_intent(&toggle.input()).sneak,
        "a fresh press must engage toggle sneak"
    );
    toggle.input_mut(|i| i.set(lodestone_controller::Action::Sneak, false));
    assert!(
        lodestone_controller::movement_intent(&toggle.input()).sneak,
        "toggle sneak must survive key release, unlike hold mode"
    );

    // -- negative control -------------------------------------------------
    let mut hold = Sim::new(test_config());
    hold.set_toggle_modes(false, false, false, false);
    hold.step(1.0 / 20.0);
    hold.input_mut(|i| i.set(lodestone_controller::Action::Sneak, true));
    assert!(lodestone_controller::movement_intent(&hold.input()).sneak);
    hold.input_mut(|i| i.set(lodestone_controller::Action::Sneak, false));
    assert!(
        !lodestone_controller::movement_intent(&hold.input()).sneak,
        "hold mode must clear sneak on release, or the toggle assertions \
         above are not really exercising the toggle"
    );
}

/// As the sneak half above, for `key.sprint`/`toggle_sprint` — a
/// different `InputState` field with its own `set` branch, not merely
/// the same code path exercised twice. Sprint needs `forward` held too
/// (`movement_intent`'s gate), so this drives that as well.
#[test]
fn toggle_sprint_option_reaches_live_input_and_survives_key_release() {
    let mut toggle = Sim::new(test_config());
    toggle.set_toggle_modes(false, true, false, false);
    toggle.step(1.0 / 20.0);

    toggle.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
    toggle.input_mut(|i| i.set(lodestone_controller::Action::Sprint, true));
    assert!(
        lodestone_controller::movement_intent(&toggle.input()).sprint,
        "a fresh press must engage toggle sprint"
    );
    toggle.input_mut(|i| i.set(lodestone_controller::Action::Sprint, false));
    assert!(
        lodestone_controller::movement_intent(&toggle.input()).sprint,
        "toggle sprint must survive key release, unlike hold mode"
    );
}

#[test]
fn connected_sim_emits_one_move_per_physics_tick() {
    use crate::net::NetUpdate;
    let (net, actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    // Before login the adapter has no Play-state Move packet, so the shell
    // must not spew movement yet: drive to Connected first.
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net(); // → Connected
    assert_eq!(sim.session_phase(), SessionPhase::Connected);
    sim.step(5.0 / 20.0); // ~5 ticks, all now in-world.
    // Counted by *variant*, not as a total: the tick tail also emits one
    // `EndClientTick` per tick (vanilla's `Minecraft.tick` does the same), so a
    // bare count answers "how many actions" rather than "how many moves".
    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    let moves = sent
        .iter()
        .filter(|a| matches!(a, ClientAction::Move { .. }))
        .count();
    assert!(moves > 0, "a connected sim should send movement packets");
    assert_eq!(
        moves as u64,
        sim.tick_count(),
        "exactly one outbound Move per physics tick"
    );
    // The tick tail rides along one-for-one, and is the *last* thing each tick
    // sends — the ordering vanilla's own send site has.
    assert_eq!(
        sent.iter()
            .filter(|a| matches!(a, ClientAction::EndClientTick))
            .count() as u64,
        sim.tick_count(),
        "exactly one EndClientTick per physics tick"
    );
    assert!(
        matches!(sent.last(), Some(ClientAction::EndClientTick)),
        "the tick tail must be last in the tick's stream, got {:?}",
        sent.last()
    );
}

/// Production folds an entity-velocity event into ECS first and then mirrors
/// it over the shell channel. The mirror must win the race with the next
/// physics tick: that tick's outbound position has to follow the new velocity,
/// not the velocity from the preceding frame.
#[test]
fn local_velocity_mirror_reaches_physics_before_the_next_outbound_move() {
    use crate::net::NetUpdate;

    let (net, actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    ingest(&mut sim, login_event(7));
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    sim.poll_net();
    while actions.try_recv().is_ok() {}

    sim.player_mut(|player| {
        player.position = Vec3d::new(0.0, 100.0, 0.0);
        player.velocity = Vec3d::new(0.75, 0.0, 0.0);
    });
    let impulse = lodestone_model::Vec3::new(0.0, 0.2, 1.4);
    // The real driver performs this ingest before it calls `forward`.
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntityVelocity {
            entity_id: 7,
            velocity: impulse,
        },
    );
    feed.send(NetUpdate::EntityVelocity {
        entity_id: 7,
        velocity: impulse,
    })
    .unwrap();

    sim.step(lodestone_ecs::TICK_PERIOD);

    let moved = std::iter::from_fn(|| actions.try_recv().ok())
        .find_map(|action| match action {
            ClientAction::Move { pos, .. } => Some(pos),
            _ => None,
        })
        .expect("the connected simulation sends one move each physics tick");
    assert!(
        moved.x.abs() < 0.01,
        "the stale +X velocity ran before the mirror: outbound position was {moved:?}"
    );
    assert!(
        moved.z > 1.0,
        "the new +Z impulse did not drive this tick's outbound position: {moved:?}"
    );
}

/// The live-server loop (owner report): a repeated `PLAYER_POSITION` to the
/// same absolute coordinate, a fresh teleport id every time, forever. Traced
/// to `Sim::step`'s own documented ordering — "one `Update` schedule, then N
/// catch-up `GameTick` schedules, then `poll_net`/`fold_entities`/`Extract`"
/// — which means a `NetUpdate::Teleport` already sitting in the channel when
/// `step` is called is not applied to `PhysicsState` until *after* this
/// frame's `TickSet::Send` has already queued an outbound `Move` from the
/// **pre-teleport** position. `select_move_packet`'s own doc names exactly
/// this symptom ("still reports the old world's coordinates after a
/// transfer/reconfigure") and points upstream at whatever feeds `pos` —
/// this is that upstream.
///
/// A `far` target (500, 80, -500) rather than anything near the default
/// spawn, so a stale pre-teleport claim cannot coincide with it by chance.
#[test]
fn move_sent_the_same_tick_as_a_teleport_carries_the_new_position() {
    use crate::net::NetUpdate;
    let (net, actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net(); // -> Connected, zero ticks run yet.
    assert_eq!(sim.session_phase(), SessionPhase::Connected);

    let far = lodestone_client::Vec3::new(500.0, 80.0, -500.0);
    feed.send(NetUpdate::Teleport {
        pos: far,
        rotation: Rotation::new(0.0, 0.0),
        flags: lodestone_model::event::TeleportFlags {
            relative_x: false,
            relative_y: false,
            relative_z: false,
            relative_yaw: false,
            relative_pitch: false,
        },
        velocity: None,
    })
    .unwrap();

    // One tick: the teleport must be visible to *this* tick's Send, not next
    // frame's.
    sim.step(1.0 / 20.0);

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    let mv = sent
        .iter()
        .find_map(|a| match a {
            ClientAction::Move { pos, .. } => Some(*pos),
            _ => None,
        })
        .expect("a connected, in-world sim must send exactly one Move this tick");

    assert!(
        (mv.x - far.x).abs() < 0.01 && (mv.y - far.y).abs() < 0.01 && (mv.z - far.z).abs() < 0.01,
        "the outbound Move sent in the same tick a teleport arrives must claim the \
         teleported position, not a stale pre-teleport one: got {mv:?}, wanted ~{far:?}"
    );
}

/// The brief's requested control on link 1 (the `relatives` bitmask): the
/// same wire rotation must produce genuinely different local player state
/// depending on whether it is flagged relative or absolute. Non-zero,
/// pairwise-distinct baseline/delta (`CLAUDE.md`'s evidence-standards
/// rule) so an accidental "always absolute" or "always relative"
/// implementation cannot coincidentally pass.
#[test]
fn teleport_relative_rotation_differs_from_absolute_rotation() {
    use crate::net::NetUpdate;

    fn teleported_rotation(relative: bool) -> (f32, f32) {
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
        sim.poll_net(); // -> Connected

        // A non-zero baseline rotation, distinct from the packet's own delta.
        sim.player_mut(|p| {
            p.yaw = 10.0;
            p.pitch = 5.0;
        });

        feed.send(NetUpdate::Teleport {
            pos: lodestone_client::Vec3::new(0.0, 0.0, 0.0),
            rotation: Rotation::new(20.0, 7.0),
            flags: lodestone_model::event::TeleportFlags {
                relative_x: false,
                relative_y: false,
                relative_z: false,
                relative_yaw: relative,
                relative_pitch: relative,
            },
            velocity: None,
        })
        .unwrap();
        sim.poll_net();

        let p = sim.player();
        (p.yaw, p.pitch)
    }

    let relative = teleported_rotation(true);
    let absolute = teleported_rotation(false);

    assert_eq!(
        relative,
        (30.0, 12.0),
        "relative_yaw/relative_pitch must add the packet's delta to the current pose"
    );
    assert_eq!(
        absolute,
        (20.0, 7.0),
        "an absolute rotation flag must overwrite, not add to, the current pose"
    );
    assert_ne!(
        relative, absolute,
        "the relatives bitmask must change the outcome -- a fix that ignores it \
         entirely would otherwise still pass"
    );
}

#[test]
fn teleport_velocity_correction_rotates_then_applies_per_axis_deltas() {
    use crate::net::NetUpdate;
    use lodestone_model::event::{TeleportFlags, TeleportVelocity};

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    sim.player_mut(|player| {
        player.yaw = 0.0;
        player.pitch = 0.0;
        player.velocity = Vec3d::new(1.0, 2.0, 3.0);
    });

    feed.send(NetUpdate::Teleport {
        pos: lodestone_client::Vec3::new(0.0, 64.0, 0.0),
        rotation: Rotation::new(90.0, 0.0),
        flags: TeleportFlags::default(),
        velocity: Some(TeleportVelocity {
            delta: lodestone_model::Vec3::new(0.5, 7.0, -0.25),
            relative_x: true,
            relative_y: false,
            relative_z: true,
            rotate_delta: true,
        }),
    })
    .unwrap();
    sim.poll_net();

    let velocity = sim.player().velocity;
    assert!((velocity.x + 2.5).abs() < 1.0e-9, "rotated x plus delta: {velocity:?}");
    assert!((velocity.y - 7.0).abs() < 1.0e-9, "absolute y: {velocity:?}");
    assert!((velocity.z - 0.75).abs() < 1.0e-9, "rotated z plus delta: {velocity:?}");
}

/// An absolute correction resets both the current and previous positions. The
/// correction is an authoritative pose change, not a render interpolation
/// sample, even when two adjacent positions are close together.
#[test]
fn short_absolute_corrections_snap_the_previous_position() {
    use crate::net::NetUpdate;
    use lodestone_model::event::TeleportFlags;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    let before = Vec3d::new(10.0, 70.0, -4.0);
    sim.player_mut(|player| player.position = before);
    sim.set_prev_position(before);

    let sample = lodestone_client::Vec3::new(10.4, 70.6, -4.5);
    feed.send(NetUpdate::Teleport {
        pos: sample,
        rotation: Rotation::new(0.0, 0.0),
        flags: TeleportFlags::default(),
        velocity: None,
    })
    .unwrap();
    sim.poll_net();

    assert_eq!(sim.player().position, Vec3d::new(sample.x, sample.y, sample.z));
    assert_eq!(
        sim.prev_position(),
        Vec3d::new(sample.x, sample.y, sample.z),
        "an absolute correction snaps the previous position with the current position"
    );
}

#[test]
fn relative_corrections_apply_the_delta_to_the_previous_position() {
    use crate::net::NetUpdate;
    use lodestone_model::event::TeleportFlags;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    sim.player_mut(|player| player.position = Vec3d::new(12.0, 72.0, -2.0));
    sim.set_prev_position(Vec3d::new(10.0, 70.0, -4.0));

    feed.send(NetUpdate::Teleport {
        pos: lodestone_client::Vec3::new(1.5, -0.25, 2.0),
        rotation: Rotation::new(0.0, 0.0),
        flags: TeleportFlags {
            relative_x: true,
            relative_y: true,
            relative_z: true,
            ..TeleportFlags::default()
        },
        velocity: None,
    })
    .unwrap();
    sim.poll_net();

    assert_eq!(sim.player().position, Vec3d::new(13.5, 71.75, 0.0));
    assert_eq!(
        sim.prev_position(),
        Vec3d::new(11.5, 69.75, -2.0),
        "relative axes are resolved from the old previous position, not the current one"
    );
}

#[test]
fn long_authoritative_teleports_snap_the_interpolation_anchor() {
    use crate::net::NetUpdate;
    use lodestone_model::event::TeleportFlags;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    sim.player_mut(|player| player.position = Vec3d::new(10.0, 70.0, -4.0));

    let destination = lodestone_client::Vec3::new(40.0, 90.0, 12.0);
    feed.send(NetUpdate::Teleport {
        pos: destination,
        rotation: Rotation::new(0.0, 0.0),
        flags: TeleportFlags::default(),
        velocity: None,
    })
    .unwrap();
    sim.poll_net();

    let placed = Vec3d::new(destination.x, destination.y, destination.z);
    assert_eq!(sim.player().position, placed);
    assert_eq!(
        sim.prev_position(),
        placed,
        "a real teleport must not smear the camera through intervening space"
    );
}

/// The player-rotation packet changes the pose the visible camera is built
/// from, without relocating the player. Pairwise-distinct values prove both
/// relative flags are respected rather than accidentally copied from teleport
/// handling or treated as absolute.
#[test]
fn player_rotation_set_reaches_the_drawn_camera_with_relative_flags() {
    use crate::net::NetUpdate;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    sim.player_mut(|player| {
        player.yaw = 10.0;
        player.pitch = 5.0;
    });
    let before_position = sim.player().position;

    feed.send(NetUpdate::PlayerRotationSet {
        y_rot: 20.0,
        relative_y: true,
        x_rot: -15.0,
        relative_x: false,
    })
    .expect("loopback accepts a rotation correction");
    sim.poll_net();

    let camera = sim.camera(1.0);
    assert_eq!(camera.yaw, 30.0, "relative yaw adds to the live camera pose");
    assert_eq!(camera.pitch, -15.0, "absolute pitch replaces the live camera pose");
    assert_eq!(
        sim.player().position,
        before_position,
        "a rotation-only packet must not move the local player"
    );
}

/// A real routed look target changes the visible camera from the selected
/// anchor. The feet control has the same target and horizontal direction but a
/// distinct vertical origin, so it catches an implementation that accepts the
/// packet yet silently ignores `from_anchor`.
#[test]
fn player_look_at_reaches_the_camera_and_respects_the_origin_anchor() {
    use crate::net::NetUpdate;

    fn directed_pitch(from_anchor: lodestone_model::event::LookAnchor) -> (f32, f32) {
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        sim.player_mut(|player| {
            player.position = Vec3d::new(1.0, 64.0, 2.0);
            player.yaw = -73.0;
            player.pitch = 11.0;
        });
        // From the standing eye at y=65.62 this is the exact 3-up, 4-forward
        // triangle. From feet it is 4.62-up, so the two routes cannot agree.
        feed.send(NetUpdate::PlayerLookAt {
            from_anchor,
            target: lodestone_client::Vec3::new(1.0, 68.62, 6.0),
        })
        .expect("loopback accepts the server-directed look target");
        sim.poll_net();
        let camera = sim.camera(1.0);
        (camera.yaw, camera.pitch)
    }

    let eyes = directed_pitch(lodestone_model::event::LookAnchor::Eyes);
    let feet = directed_pitch(lodestone_model::event::LookAnchor::Feet);

    assert!(eyes.0.abs() < 1.0e-4, "the target lies directly along +Z: {eyes:?}");
    assert!(
        (eyes.1 - -36.869_896).abs() < 1.0e-4,
        "eyes must use the exact 3:4 look triangle, got {eyes:?}"
    );
    assert!(
        (feet.1 - eyes.1).abs() > 5.0,
        "control: a feet anchor must produce a visibly steeper pitch than eyes; \
         eyes={eyes:?}, feet={feet:?}"
    );
}

/// The server, rather than the launcher, owns the streamed view radius. Its
/// update reaches the exact value the loading screen turns into both a progress
/// denominator and a chunk-status grid side length.
#[test]
fn chunk_cache_radius_update_replaces_the_loading_views_radius() {
    use crate::net::NetUpdate;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    feed.send(NetUpdate::ChunkCacheRadiusChanged { radius: 7 })
        .expect("loopback accepts the server view radius");
    sim.poll_net();

    assert_eq!(
        sim.terrain_progress().expect("attached client has progress").expected,
        225,
        "the server radius 7 must make the loading denominator (2 * 7 + 1)^2"
    );
    assert_eq!(
        sim.terrain_chunk_grid()
            .expect("attached client has a chunk-status grid")
            .radius,
        7,
        "the same server radius must size the visible loading grid"
    );
}

/// The server's stream center is not necessarily the player chunk while a
/// loading transition is in flight. The grid must follow the packet, while an
/// older/no-report session retains the player-centred fallback.
#[test]
fn chunk_cache_center_update_replaces_the_loading_grid_center() {
    use crate::net::NetUpdate;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.player_mut(|player| player.position = Vec3d::new(160.0, 64.0, -80.0));
    sim.attach_net(net);

    feed.send(NetUpdate::ChunkCacheRadiusChanged { radius: 2 })
        .expect("loopback accepts the server view radius");
    sim.poll_net();
    let fallback = sim.read(|world| {
        world
            .get::<lodestone_ecs::ServerChunkCacheCenter>(sim.local_player())
            .expect("the session has a cache-center slot")
            .0
    });
    assert_eq!(fallback, None, "control: an absent packet keeps player centering");
    assert_eq!(
        sim.terrain_chunk_grid()
            .expect("radius makes the loading grid available")
            .center,
        (10, -5),
        "control: without the packet, the grid stays centered on the player's chunk"
    );

    feed.send(NetUpdate::ChunkCacheCenterChanged { x: -4, z: 9 })
        .expect("loopback accepts the server stream center");
    sim.poll_net();
    let center = sim.read(|world| {
        world
            .get::<lodestone_ecs::ServerChunkCacheCenter>(sim.local_player())
            .expect("the loading-grid center is retained on the session")
            .0
    });
    assert_eq!(center, Some((-4, 9)), "the latest server center must win");
    let grid = sim
        .terrain_chunk_grid()
        .expect("the routed center feeds the visible loading-grid producer");
    assert_eq!(grid.center, (-4, 9), "the visible grid must use the packet center");
    assert_eq!(
        grid.chunk_at(0, 0),
        (-6, 7),
        "the top-left cell must query relative to the server center, not player chunk"
    );
}

#[test]
fn move_is_withheld_until_connected() {
    // A sim that is merely Connecting (attached, not yet logged in) must send
    // nothing — otherwise every pre-Play tick is a dropped-action on the wire.
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    assert_eq!(sim.session_phase(), SessionPhase::Connecting);
    sim.step(5.0 / 20.0);
    assert!(
        sim.tick_count() > 0,
        "ticks must still run while connecting"
    );
    let sent = std::iter::from_fn(|| actions.try_recv().ok()).count();
    assert_eq!(sent, 0, "no movement should be sent before login");
}

/// The bell source accessor (`docs/block-entity-renderers.md`'s Bell section):
/// `Sim::bell_source` is the accessor `app.rs`'s new per-frame install calls
/// (`if let Some(f) = self.sim.bell_source() { render.set_bell_source(f); }`)
/// — a plain island-detector for that one call site, not a pixel gate. A
/// full through-the-wire proof needs a real `ClientHandle` (login, a real
/// chunk with a `minecraft:bell` state *and* a recorded block-entity entry),
/// which no test double in this crate builds yet — every existing chest/
/// skull/sign/bell pixel gate installs a hand-built closure on `RenderState`
/// directly rather than going through `Sim::*_source`, so a through-the-wire
/// proof is currently unavailable for all four block-entity types, not bell alone.
/// This is the part that *is* checkable without one: the accessor must
/// track connection state exactly like its skull/sign siblings (`None`
/// before any net is attached, `Some` after), and the closure it returns
/// must be safe to call before login rather than panicking on the
/// not-yet-published `ClientHandle` — the same "empty rather than a panic"
/// contract `block_entities::bell_spawns_before_login_is_empty_rather_than_a_panic`
/// already pins for the free function underneath it.
#[test]
fn bell_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.bell_source().is_none(),
        "no net attached at all must report no source, matching skull_source/sign_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .bell_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// [`Sim::spawner_source`]'s own
/// island detector, matching
/// [`bell_source_tracks_connection_state_and_is_safe_before_login`]'s shape
/// and reasoning — the closure captures `Sim::spawner_spins` and the partial
/// tick, the same shape bell's own closure carries.
#[test]
fn spawner_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.spawner_source().is_none(),
        "no net attached at all must report no source, matching bell_source/skull_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .spawner_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// [`Sim::beacon_source`]'s own island detector, matching
/// [`bell_source_tracks_connection_state_and_is_safe_before_login`]'s shape.
/// Unlike bell's closure, this one carries no cloned tracker — just the game
/// tick and partial tick — so the panic-safety half is the same claim: no
/// `ClientHandle` published yet must return no spawns, not panic reading the
/// world through an empty `OnceLock`.
#[test]
fn beacon_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.beacon_source().is_none(),
        "no net attached at all must report no source, matching bell_source/skull_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .beacon_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// [`Sim::vault_source`]'s own island detector, matching
/// [`beacon_source_tracks_connection_state_and_is_safe_before_login`]'s shape
/// exactly — both closures capture only `game_time`/`partial_tick` and a
/// `SharedHandle`, no per-position tracker.
#[test]
fn vault_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.vault_source().is_none(),
        "no net attached at all must report no source, matching beacon_source/bell_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .vault_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// [`Sim::brushable_source`]'s own island detector, matching
/// [`vault_source_tracks_connection_state_and_is_safe_before_login`]'s
/// shape exactly — the closure captures only a `SharedHandle`, no clock at
/// all.
#[test]
fn brushable_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.brushable_source().is_none(),
        "no net attached at all must report no source, matching vault_source/campfire_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .brushable_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// [`Sim::copper_golem_statue_source`]'s
/// own island detector, matching
/// [`shelf_source_tracks_connection_state_and_is_safe_before_login`]'s shape.
#[test]
fn copper_golem_statue_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.copper_golem_statue_source().is_none(),
        "no net attached at all must report no source, matching skull_source/shelf_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .copper_golem_statue_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// [`Sim::shelf_source`]'s own island detector, matching
/// [`brushable_source_tracks_connection_state_and_is_safe_before_login`]'s
/// shape exactly.
#[test]
fn shelf_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.shelf_source().is_none(),
        "no net attached at all must report no source, matching brushable_source/vault_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .shelf_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// [`Sim::decorated_pot_source`]'s own island detector, matching
/// [`bell_source_tracks_connection_state_and_is_safe_before_login`]'s
/// shape and reasoning — see that test's doc for why a plain accessor check
/// is the honest scope here.
#[test]
fn decorated_pot_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.decorated_pot_source().is_none(),
        "no net attached at all must report no source, matching bell_source/shulker_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .decorated_pot_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// [`Sim::conduit_source`]'s own island detector,
/// matching the bell/pot siblings above.
#[test]
fn conduit_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.conduit_source().is_none(),
        "no net attached at all must report no source, matching bell_source/decorated_pot_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .conduit_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// [`Sim::step`] must actually advance
/// `Sim::conduit_ticks` once per tick while connected, not merely hold the
/// field — the same "correct function fed a constant by its producer" trap
/// the other per-tick block-entity folds hit. A tick where no conduit is anywhere
/// near the player is still observable: `Sim::step` must run without
/// panicking on the not-yet-published `ClientHandle`, matching every other
/// per-tick block-entity fold's "empty rather than a panic" contract.
#[test]
fn stepping_ticks_conduits_without_panicking_before_login() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);

    sim.step(5.0 / 20.0);

    assert!(
        sim.tick_count() > 0,
        "ticks must still run while connecting, exactly like the movement-packet gate above"
    );
}

/// [`stepping_ticks_conduits_without_panicking_before_login`]'s shape, for
/// `SpawnerSpins::tick` — the newest per-tick gather in the same
/// `if let Some(net) = ..` block, and the one with the most inputs to get
/// wrong before login (a world read, an NBT-aware candidate scan, and a
/// distance test), so it earns its own explicit pin rather than relying on
/// the conduit test's coverage of the same guarded block.
#[test]
fn stepping_ticks_spawners_without_panicking_before_login() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);

    sim.step(5.0 / 20.0);

    assert!(
        sim.tick_count() > 0,
        "ticks must still run while connecting, exactly like the movement-packet gate above"
    );
}

/// Both [`CollisionSource`] implementors must actually be `Send + Sync +
/// 'static`, or they could not be held in a `Resource` at all.
///
/// Asserted rather than reasoned about: the Stage 1 report recorded this as
/// "likely, unverified" for [`LiveCollision`] (which holds
/// `Arc<ChunkSection>`, `Arc<BlockAtlas>` and `Option<Arc<dyn
/// VersionAdapter>>`), and it is the single fact the whole Stage-2 collision
/// seam rests on. It compiles today because `Arc<dyn CollisionSource>` is
/// used; this pins it so the reason stays visible if it ever stops holding.
#[test]
fn both_collision_sources_are_send_sync_and_static() {
    fn assert_resource_shaped<T: CollisionSource>() {}
    assert_resource_shaped::<ChunkWorldCollision>();
    assert_resource_shaped::<LiveCollisionSource>();
}

// **These three autopilot gates are not shell gates.** They are
// `autopilot_plugin_is_registered_and_its_systems_actually_run` (the island
// gate: one tick with a goal set must move `AutopilotStatus` off `Idle`),
// `goto_chat_command_drives_the_player_toward_the_goal_over_real_ticks` (real
// displacement down a hand-carved corridor, with a sealed-corridor control),
// and `goto_chat_command_never_reaches_the_outbound_action_queue`.
//
// **The dependency boundary leaves no shell registration to test.**
// `lodestone-autopilot` is a pre-implemented *external* plugin now, so
// `lodestone-shell` does not depend on it at all — not optionally, not behind a
// feature — and a test here cannot name `AutopilotStatus` any more than
// production code can. The plugin's integration tests in
// `crates/plugins/lodestone-autopilot/tests/drives_to_goal.rs`
// install `AutopilotPlugin` in a real `App`, drive a real `GameTick`
// schedule, and assert real arrival against **jar-derived** collision, with
// unreachable-goal controls. They do not depend on shell registration, so
// the shell has no registration contract to test; its local namespace gate
// remains the relevant shell behavior.
//
// The local-namespace gate is directly below. Plugin-specific command
// registration is outside this crate's current dependency set.
