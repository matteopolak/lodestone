use super::*;
#[test]
fn a_hash_prefixed_line_is_consumed_locally_and_never_reaches_the_outbound_queue() {
    let (net, actions, _feed) = crate::net::NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    assert!(
        sim.send_chat("/say hi"),
        "control: an ordinary command line must report that it sent"
    );
    assert!(
        actions.try_recv().is_ok(),
        "control: the outbound action queue must actually carry an ordinary \
         line -- otherwise the emptiness asserted below proves nothing"
    );

    for line in ["#goto 3 4", "#goto", "#follow 1 2", "#"] {
        assert!(
            !sim.send_chat(line),
            "`{line}` is client-local and unhandled, so send_chat must report \
             that nothing was sent"
        );
        assert!(
            actions.try_recv().is_err(),
            "`{line}` must be consumed locally, never handed to the outbound \
             action queue where other players would read it"
        );
    }
}

/// The runtime half of the boundary decision: **the shipped client does not
/// navigate itself.** A type-level absence (`cargo tree` reporting no
/// `lodestone-autopilot` edge) says the crate is not linked; it does not say
/// the *behaviour* is gone, because the shell could in principle have grown its
/// own walker. This asserts the behaviour.
///
/// Deliberately the exact mirror of the deleted
/// `goto_chat_command_drives_the_player_toward_the_goal_over_real_ticks`: the
/// same flat corridor, the same `#goto 0 5`, the same 200 driven ticks. That
/// test measured the player closing to within 1.5 blocks of (0, _, 5) from
/// about 5 blocks out. Here the player must **not move**, which is why the
/// corridor is worth building at all — it removes the "they were stuck on
/// terrain anyway" explanation for a stationary result.
///
/// This is not a test that nothing is registered; it is a test that no chat
/// line makes the player walk. Re-registering `AutopilotPlugin` alone would
/// leave it passing (nothing sets an `AutopilotGoal`), which is correct: the
/// capability under test is the `#goto`-drives-the-player pair, and that pair
/// is what was removed.
#[test]
fn no_chat_line_makes_the_shipped_client_walk_itself() {
    let mut sim = Sim::new(test_config());
    let feet_y = sim.player().position.y.floor() as i32;
    // Same corridor the deleted drive gate carved, running +Z from spawn.
    for dz in -1..=6 {
        for dx in -1..=1 {
            sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
            sim.set_block_world([dx, feet_y, dz], id::AIR);
            sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
            sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
        }
    }
    for _ in 0..20 {
        sim.step(1.0 / 20.0);
    }

    let before = sim.player().position;
    assert!(
        !sim.send_chat("#goto 0 5"),
        "`#goto` must be refused now that no plugin claims the `#` namespace"
    );
    for _ in 0..200 {
        sim.step(1.0 / 20.0);
    }
    let after = sim.player().position;

    let moved = ((after.x - before.x).powi(2) + (after.z - before.z).powi(2)).sqrt();
    assert!(
        moved < 0.5,
        "no chat line may drive the player: moved {moved:.2} blocks \
         horizontally over 200 ticks after `#goto 0 5` \
         (from {before:?} to {after:?}). The deleted issue-#38 drive gate \
         measured ~4 blocks of travel on this same corridor, so movement here \
         means something in the shell is navigating for the player again."
    );
}

/// The authority test for the stage, at the shell level: the components are
/// the *only* store, so a write through the `World` — which is what a plugin
/// gets — changes what the server is told on the next tick.
///
/// If `Sim` still held a `PlayerState` of its own, this would pass a write
/// into a field nobody reads and the wire would report the unmodified pose.
#[test]
fn a_write_through_the_world_reaches_the_wire() {
    use crate::net::NetUpdate;
    let (net, actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    while actions.try_recv().is_ok() {}

    let local = sim.local_player();
    sim.ecs()
        .write()
        .get_mut::<PhysicsState>(local)
        .expect("local player")
        .0
        .position = Vec3d::new(11.5, 200.0, -3.5);

    sim.step(lodestone_ecs::TICK_PERIOD);
    let moved: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok())
        .filter_map(|a| match a {
            ClientAction::Move { pos, .. } => Some(pos),
            _ => None,
        })
        .collect();
    assert_eq!(moved.len(), 1, "one move per tick");
    // No world to collide against in this fixture beyond the demo terrain far
    // below, so the tick's only change is gravity — x and z are untouched.
    assert!((moved[0].x - 11.5).abs() < 1e-9, "got {moved:?}");
    assert!((moved[0].z + 3.5).abs() < 1e-9, "got {moved:?}");
    // …and the accessor agrees with the wire, because there is one store.
    assert!((sim.player().position.x - 11.5).abs() < 1e-9);
}

/// The other half of the authority test: `Sim`'s accessors are views onto the
/// same components, not onto a copy. A write through the accessor must be
/// visible in the `World` a plugin queries.
#[test]
fn the_accessors_and_the_world_are_the_same_store() {
    let mut sim = Sim::new(test_config());
    sim.player_mut(|p| p.yaw = 42.0);
    sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));

    let local = sim.local_player();
    let world = sim.ecs().read();
    assert_eq!(world.get::<PhysicsState>(local).expect("local").0.yaw, 42.0);
    assert_eq!(
        lodestone_controller::movement_intent(&world.resource::<RawInput>().0).forward,
        1.0
    );
}

#[test]
fn server_camera_subject_replaces_the_rendered_view_and_local_id_restores_it() {
    use crate::net::NetUpdate;
    use lodestone_ecs::entity::{EntityIndex, Position, Rotation};

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    ingest(&mut sim, login_event(7));

    sim.write(|world| {
        let target = world
            .spawn((
                Position(lodestone_model::Vec3::new(40.0, 72.0, -16.0)),
                Rotation(lodestone_model::Rotation::new(123.0, -21.0)),
            ))
            .id();
        world.resource_mut::<EntityIndex>().insert(99, target);
    });

    let local = sim.render_camera(1.0);
    feed.send(NetUpdate::CameraSet { entity_id: 99 }).unwrap();
    sim.poll_net();
    let followed = sim.render_camera(1.0);
    assert_eq!(followed.position.x, 40.0);
    assert_eq!(followed.position.y, 73.62);
    assert_eq!(followed.position.z, -16.0);
    assert_eq!((followed.yaw, followed.pitch), (123.0, -21.0));
    assert_ne!(
        followed.position, local.position,
        "the camera packet must change the rendered frame, not only retain an id"
    );

    feed.send(NetUpdate::CameraSet { entity_id: 7 }).unwrap();
    sim.poll_net();
    let restored = sim.render_camera(1.0);
    assert_eq!(restored.position, local.position);
    assert_eq!((restored.yaw, restored.pitch), (local.yaw, local.pitch));
}

/// **Stage 4's authority test at the shell level.** The `ChunkWorld` resource
/// is the *only* chunk store, so a write through the handle a plugin would get
/// (`sim.chunk_world()`, or `sim.ecs().resource::<ChunkWorld>()`) is what the
/// sim collides against, raycasts into and meshes.
///
/// If `Sim` still owned a `World` field, this would write into a store nobody
/// reads and `block_at_world` would report the pre-edit block.
#[test]
fn a_write_through_the_chunk_world_resource_is_what_the_sim_reads() {
    let sim = Sim::new(test_config());
    let feet = sim.player().position;
    let (bx, bz) = (feet.x.floor() as i32 + 4, feet.z.floor() as i32 + 4);
    let above = crate::worldgen::surface_height(bx, bz) + 4;

    assert_eq!(
        sim.block_at_world([bx, above, bz]),
        id::AIR,
        "the cell starts empty"
    );

    // The write goes through the *write* resource handle, not through any `Sim`
    // method — the read handle `sim.chunk_world()` has no write path.
    {
        let store = sim.chunk_world_write();
        let mut world = store.write();
        let chunk = world
            .get_mut(ChunkPos {
                x: bx.div_euclid(16),
                z: bz.div_euclid(16),
            })
            .expect("the fixture holds this column");
        chunk.column.set_block(
            bx.rem_euclid(16) as usize,
            above,
            bz.rem_euclid(16) as usize,
            PLACE_BLOCK,
        );
    }

    assert_eq!(
        sim.block_at_world([bx, above, bz]),
        PLACE_BLOCK,
        "the sim reads the store a plugin writes, with no propagation step"
    );
    // And collision sees it in the same instant — there is no cached clone to
    // invalidate any more. Before Stage 4 this needed
    // `Sim::set_block_world` to clear `demo_collision` by hand, and a missed
    // clear read as "I mined the block but still cannot walk through it".
    let source = sim.chunk_collision();
    let mut solid = false;
    source.with_view(&mut |view: &dyn CollisionView| {
        let mut boxes = Vec::new();
        view.collision_boxes(bx, above, bz, &mut boxes);
        solid = !boxes.is_empty();
    });
    assert!(
        solid,
        "the collision source reads the same store, uncached — a plugin's edit \
         is collidable on the next tick"
    );
}

/// The control for the test above: the same probe against a cell nobody wrote
/// must report empty, so "solid" is a measurement rather than a constant.
#[test]
fn the_collision_source_reports_empty_where_nothing_was_written() {
    let sim: Sim = Sim::new(test_config());
    let feet = sim.player().position;
    let (bx, bz) = (feet.x.floor() as i32 + 4, feet.z.floor() as i32 + 4);
    let above = crate::worldgen::surface_height(bx, bz) + 4;

    let source = sim.chunk_collision();
    let mut solid = false;
    source.with_view(&mut |view: &dyn CollisionView| {
        let mut boxes = Vec::new();
        view.collision_boxes(bx, above, bz, &mut boxes);
        solid = !boxes.is_empty();
    });
    assert!(!solid, "control: an untouched air cell must not collide");
}

/// `heal_dirty_columns` must actually be registered in the `Update` schedule
/// `Sim::step` runs — the island check for Stage 4's one system. A dirtied
/// column that `run_schedule(Update)` does not drain is a chunk seam that
/// stays baked against air forever.
#[test]
fn the_update_schedule_drains_the_dirty_column_set() {
    let mut sim = Sim::new(test_config());
    let _ = sim.drain_all_meshes();
    let pos = *sim
        .chunk_world()
        .read()
        .iter()
        .next()
        .expect("the fixture holds a column")
        .0;
    sim.terrain_mut(|t| t.dirty_columns.insert((pos.x, pos.z)));
    assert_eq!(sim.pending_meshes(), 0, "drained to a clean slate");

    sim.ecs().write().run_schedule(lodestone_ecs::Update);

    assert!(
        sim.terrain(|t| t.dirty_columns.is_empty()),
        "the Update schedule must drain the dirty set"
    );
    assert!(
        sim.pending_meshes() > 0,
        "and draining it must submit real mesh jobs, not just empty the set"
    );
}

#[test]
fn disconnected_sim_sends_nothing() {
    // Without a net attached, stepping must not attempt to send.
    let mut sim = Sim::new(test_config());
    sim.step(5.0 / 20.0);
    assert!(sim.net.is_none());
}

fn effect_id(path: &str) -> lodestone_data::mob_effects::MobEffectId {
    lodestone_data::mob_effects::mob_effect_id(path).expect("test names a built-in mob effect")
}

#[test]
fn mob_effect_applied_for_local_player_reaches_status_effects() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    // `ServerEntityId` — the "is this effect ours" test — is folded from
    // `ClientEvent::Login` on the net thread, not from `NetUpdate::LoggedIn`.
    // Production sees both for one packet; so does this test.
    ingest(&mut sim, login_event(7));
    sim.poll_net();
    assert_eq!(
        sim.server_entity_id(),
        Some(7),
        "setup: the id must be folded"
    );
    assert!(sim.player().effects.levitation.is_none());

    feed.send(NetUpdate::EffectApplied {
        entity_id: 7,
        effect: lodestone_data::mob_effects::MobEffectId::LEVITATION,
        amplifier: 2,
        duration_ticks: 200,
        ambient: false,
        show_particles: true,
        show_icon: true,
        blend: true,
    })
    .unwrap();
    sim.poll_net();
    assert_eq!(
        sim.player().effects.levitation,
        Some(2),
        "the wire→StatusEffects seam must fold an effect for the local entity id"
    );
    // The same event must also reach the display models with its full data —
    // both of them, because the HUD overlay and the inventory column are
    // separate folds of this one state and either could be the dead one.
    let icons = crate::effects::hud_icons(&sim.active_effects());
    assert_eq!(icons.len(), 1, "the HUD effect model must fold it too");
    assert_eq!(icons[0].icon, "mob_effect/levitation");
    assert!(
        !icons[0].beneficial,
        "levitation is HARMFUL, so it belongs in the overlay's lower row"
    );
    let rows = crate::effects::inventory_rows(&sim.active_effects(), &|_| None);
    assert_eq!(rows.len(), 1, "the inventory column must fold it too");
    assert_eq!(rows[0].duration, "00:10"); // 200 ticks -> 10 s
    assert!(
        sim.active_effects().iter().next().unwrap().show_particles,
        "the normal-particle bit must survive the wire fold"
    );

    feed.send(NetUpdate::EffectRemoved {
        entity_id: 7,
        effect: lodestone_data::mob_effects::MobEffectId::LEVITATION,
    })
    .unwrap();
    sim.poll_net();
    assert!(sim.player().effects.levitation.is_none());
    assert!(
        sim.active_effects().is_empty(),
        "removal must clear the HUD effect model as well"
    );
}

#[test]
fn night_vision_reaches_the_shared_effect_light_source_without_an_icon_or_particles() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    ingest(&mut sim, login_event(7));
    sim.poll_net();

    feed.send(NetUpdate::EffectApplied {
        entity_id: 7,
        effect: effect_id("night_vision"),
        amplifier: 0,
        duration_ticks: 3_600,
        ambient: false,
        show_particles: false,
        show_icon: false,
        blend: false,
    })
    .unwrap();
    sim.poll_net();

    let active_effects = sim.active_effects();
    let effect = active_effects
        .iter()
        .next()
        .expect("Night Vision must be retained");
    assert!(!effect.show_particles, "the server can suppress normal particles");
    assert!(!effect.show_icon, "the server can suppress the HUD icon independently");
    assert!(
        crate::effects::hud_icons(&active_effects).is_empty(),
        "the hidden-icon control proves this did not accidentally test the HUD path"
    );

    let floor = sim.effect_light_source()().expect("Night Vision must feed the render source");
    assert_eq!(
        floor,
        lodestone_render::NIGHT_VISION_COLOR,
        "a fresh Night Vision effect must reach the shared lightmap floor"
    );

    feed.send(NetUpdate::EffectRemoved {
        entity_id: 7,
        effect: effect_id("night_vision"),
    })
    .unwrap();
    sim.poll_net();
    assert_eq!(
        sim.effect_light_source()(),
        None,
        "removal must restore the dimension-only lightmap path"
    );
}

#[test]
fn mob_effect_for_a_different_entity_is_not_applied_to_the_local_player() {
    use crate::net::NetUpdate;
    // `update_mob_effect` is entity-agnostic on the wire; only the entity id
    // that matches the local player's should ever mutate `sim.player`.
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    ingest(&mut sim, login_event(7));
    sim.poll_net();

    feed.send(NetUpdate::EffectApplied {
        entity_id: 1234, // some other (mob) entity, not the local player
        effect: lodestone_data::mob_effects::MobEffectId::LEVITATION,
        amplifier: 0,
        duration_ticks: 200,
        ambient: false,
        show_particles: true,
        show_icon: true,
        blend: true,
    })
    .unwrap();
    sim.poll_net();
    assert!(
        sim.player().effects.levitation.is_none(),
        "a remote entity's effect must not leak into the local player's StatusEffects"
    );
    assert!(
        sim.active_effects().is_empty(),
        "a remote entity's effect must not reach the local HUD overlay either"
    );
    assert!(
        sim.read(|world| {
            world
                .resource::<lodestone_ecs::EntityStatusEffects>()
                .get(1234)
                .is_some_and(|effects| !effects.is_empty())
        }),
        "remote effects must remain available to the continuous particle emitter"
    );

    feed.send(NetUpdate::EffectRemoved {
        entity_id: 1234,
        effect: lodestone_data::mob_effects::MobEffectId::LEVITATION,
    })
    .unwrap();
    sim.poll_net();
    assert!(
        sim.read(|world| {
            world
                .resource::<lodestone_ecs::EntityStatusEffects>()
                .get(1234)
                .is_none()
        }),
        "an explicit removal must prune a remote entity's particle state"
    );
}

#[test]
fn status_particle_source_respects_the_wire_visible_flag() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    ingest(&mut sim, login_event(7));
    sim.poll_net();

    let apply = |show_particles| NetUpdate::EffectApplied {
        entity_id: 7,
        effect: lodestone_data::mob_effects::MobEffectId::SPEED,
        amplifier: 0,
        duration_ticks: 200,
        ambient: false,
        show_particles,
        show_icon: false,
        blend: false,
    };
    feed.send(apply(false)).unwrap();
    sim.poll_net();
    assert!(
        sim.status_particle_sources([0.0; 3]).is_empty(),
        "the visible flag must suppress continuous status-particle extraction"
    );

    feed.send(apply(true)).unwrap();
    sim.poll_net();
    assert_eq!(
        sim.status_particle_sources([0.0; 3]).len(),
        1,
        "the same active effect must reach the particle source when visibility is restored"
    );
}

#[test]
fn nausea_blend_flag_controls_the_live_screen_effect_source() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    ingest(&mut sim, login_event(7));
    sim.poll_net();

    feed.send(NetUpdate::EffectApplied {
        entity_id: 7,
        effect: effect_id("nausea"),
        amplifier: 0,
        duration_ticks: 200,
        ambient: false,
        show_particles: false,
        show_icon: false,
        blend: false,
    })
    .unwrap();
    sim.poll_net();
    assert_eq!(
        sim.nausea_intensity(),
        1.0,
        "the wire's no-blend control must adopt the effect immediately"
    );

    feed.send(NetUpdate::EffectApplied {
        entity_id: 7,
        effect: effect_id("nausea"),
        amplifier: 0,
        duration_ticks: 200,
        ambient: false,
        show_particles: false,
        show_icon: false,
        blend: true,
    })
    .unwrap();
    sim.poll_net();
    assert_eq!(sim.nausea_intensity(), 0.0, "a blended replacement starts dark");
    sim.step(1.0 / 20.0);
    assert!(
        (0.0..0.01).contains(&sim.nausea_intensity()),
        "one 20 Hz tick must begin, but not complete, the 150-tick Nausea ramp"
    );
}

/// Hermetic proof that `NetUpdate::Particles` actually reaches the
/// emitter: idle, `stats`/the HUD counter would also read
/// `particles=0/0+0unres`, which cannot distinguish "the route works but
/// nothing has fired" from "the route is missing" (`grep -rn
/// "ClientEvent::Particles" crates/lodestone-shell/src/` returned zero
/// hits in the missing-route state). So this feeds a live event and asserts the
/// *caused* output, not the idle baseline.
#[test]
fn net_particles_reaches_the_emitter_and_resolves() {
    use crate::net::NetUpdate;
    use lodestone_client::Vec3;
    use lodestone_particle::Sheet;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();

    // A headless `Sim` has no vanilla jar, so `flame`'s sheet has no atlas
    // UVs by default — install the same kind of fixture table
    // `particles.rs`'s own hermetic tests use, so `unresolved == 0` is
    // actually reachable without fetching `client.jar`.
    let rect = [0.0f32, 0.0, 0.0625, 0.0625];
    sim.particles_mut(|p| {
        p.install_test_sheet_uv(HashMap::from([((Sheet::Flame, 0u16), rect)]));
    });

    // Keep the particle origin within vanilla's 32-block render cutoff of
    // wherever `Sim::new` spawned the player.
    let origin = sim.player().position;
    feed.send(NetUpdate::Particles {
        kind: "flame".into(),
        long_distance: false,
        always_show: false,
        pos: Vec3::new(origin.x, origin.y, origin.z),
        offset: Vec3f::new(0.1, 0.1, 0.1),
        max_speed: 0.02,
        count: 9,
        options: lodestone_model::event::ParticleOptions::None,
    })
    .unwrap();
    sim.poll_net();

    assert_eq!(
        sim.particles_mut(|p| p.engine_mut().particles().len()),
        9,
        "count must be honoured exactly once the event reaches the emitter"
    );
    let cam = sim.camera(1.0);
    let frame = sim.particles_mut(|p| {
        p.extract(&cam, 0.0, &|_, _, _| Some(lodestone_particle::FULL_BRIGHT))
    });
    assert_eq!(frame.alive, 9);
    assert_eq!(
        frame.unresolved, 0,
        "flame is a sheet-sourced type with an installed atlas entry"
    );
    assert_eq!(frame.drawn, 9);
}

/// Explosion knockback is an impulse, so it stacks on the predicted velocity
/// the local simulation already has. A packet without a player-specific
/// knockback vector must leave that velocity untouched.
#[test]
fn net_explosion_adds_knockback_and_none_is_a_noop() {
    use crate::net::NetUpdate;
    use lodestone_client::Vec3;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    sim.player_mut(|player| player.velocity = Vec3d::new(1.0, -2.0, 3.0));

    feed.send(NetUpdate::Explosion {
        pos: Vec3::new(10.0, 64.0, -4.0),
        radius: 4.0,
        affected_blocks: vec![[1, -2, 3]],
        knockback: Some(Vec3::new(0.25, 1.5, -0.75)),
    })
    .unwrap();
    sim.poll_net();
    assert_eq!(sim.player().velocity, Vec3d::new(1.25, -0.5, 2.25));

    feed.send(NetUpdate::Explosion {
        pos: Vec3::new(10.0, 64.0, -4.0),
        radius: 4.0,
        affected_blocks: Vec::new(),
        knockback: None,
    })
    .unwrap();
    sim.poll_net();
    assert_eq!(sim.player().velocity, Vec3d::new(1.25, -0.5, 2.25));
}

/// How many particles the two hold measurements below run over. High enough
/// to be a real workload rather than an empty engine trivially satisfying
/// the assertion below — the *world*-species guard `CLAUDE.md` asks for —
/// and well under `ParticleEngine::DEFAULT_CAPACITY` (16 384) so the engine
/// does not silently drop the tail.
const HOLD_MEASUREMENT_PARTICLES: i32 = 4_000;

/// The small end of the volume check, a tenth of
/// [`HOLD_MEASUREMENT_PARTICLES`]. Measured at both this and the full count
/// — see
/// [`extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work`]
/// for why the guard-acquisition count has to come out identical at both ends
/// despite the tenfold difference in particle volume.
const HOLD_MEASUREMENT_PARTICLES_SMALL: i32 = HOLD_MEASUREMENT_PARTICLES / 10;

/// The exact guard-acquisition count [`guard_acquisitions_for_extract`] must
/// report, at any particle count: [`Sim::reset_lock_holds`]'s own hold (its
/// doc explains why zeroing the counters still leaves one behind), plus
/// `extract_particles`'s `self.clock()` read, plus the two writes
/// `with_particles_unlocked` takes to move `ParticleSim` out of the `World`
/// and back. See
/// [`extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work`]'s
/// own doc for why this is derived rather than guessed.
const EXTRACT_EXPECTED_HOLDS: u64 = 4;

/// The exact guard-acquisition count [`guard_acquisitions_for_prefix_shape`]
/// must report, at any particle count: [`Sim::reset_lock_holds`]'s own hold,
/// plus the single `hold_write` that wraps the entire pre-fix extract.
const PREFIX_EXPECTED_HOLDS: u64 = 2;

/// Spawns `count` live particles around the player and returns the `Sim` and a
/// camera to extract them with.
fn sim_with_particles(count: i32) -> (Sim, Camera) {
    let mut sim = Sim::new(test_config());
    let origin = sim.player().position;
    sim.particles_mut(|p| {
        p.spawn_particles(
            "smoke",
            [origin.x, origin.y, origin.z],
            [0.5, 0.5, 0.5],
            0.02,
            count,
            lodestone_model::event::ParticleOptions::None,
        );
    });
    let camera = sim.camera(1.0);
    (sim, camera)
}

/// [`Sim::lock_holds`]'s guard-*acquisition* count (not duration) for one
/// `extract_particles` call over `count` particles, plus how many particles
/// actually survived to be extracted — the volume half of the *world*-species
/// check, since a count measured over an empty engine would satisfy any
/// assertion trivially.
///
/// [`Sim::reset_lock_holds`] first, so the count below is this one call's,
/// not the whole session's — `Sim::new`'s own setup already takes guards of
/// its own before this function ever runs.
fn guard_acquisitions_for_extract(count: i32) -> (u64, usize) {
    let (mut sim, camera) = sim_with_particles(count);
    sim.reset_lock_holds();
    let alive = sim.extract_particles(&camera).alive;
    (sim.lock_holds().holds, alive)
}

/// As [`guard_acquisitions_for_extract`], but for the **pre-fix shape**: the
/// whole extract run inside one write guard — the exact shape
/// `Sim::extract_particles` used to be before the fix this file documents.
///
/// `light` is the offline arm (`self.net == None`), which is also what the
/// positive case measures against — see [`sim_with_particles`] — so the two
/// shapes are compared on identical inputs.
fn guard_acquisitions_for_prefix_shape(count: i32) -> (u64, usize) {
    let (mut sim, camera) = sim_with_particles(count);
    sim.reset_lock_holds();
    let alive = lodestone_ecs::hold_write(sim.ecs(), |w| {
        w.resource_mut::<ParticleSim>()
            .0
            .extract(&camera, 0.0, &|_, _, _| None)
    })
    .alive;
    (sim.lock_holds().holds, alive)
}

/// **The measurement §4.1(c) could not make, re-expressed as a counter.**
///
/// `Sim::extract_particles` was the longest `World` guard hold in the process:
/// it took the write guard by hand and held it across the whole extract *and*
/// one chunk-store lookup per live particle for light. `docs/world-unification.md`
/// bounded that structurally — "no guard spans a frame" — and said so out loud:
/// *treat the bound as structural, not measured*. A duration claim with nothing
/// measuring the duration is the species of vacuous test `CLAUDE.md` names, so
/// this used to be a ratio of **guarded time at two particle counts** instead.
///
/// That duration form was itself real — it correctly distinguished the two
/// shapes on an otherwise-idle machine — but it flaked whenever this checkout's
/// other agents were compiling: both arms are measured *sequentially*, so a
/// load spike landing between the small-count and large-count extract
/// corrupts the ratio in exactly the direction that looks like a regression.
/// Measured at 5.34x against a 3x bound on a loaded machine, standing on
/// unmodified code. Three separate agents independently re-ran it alone and
/// single-threaded, confirmed it passes there, and correctly concluded "not a
/// defect" — cost paid three times for the same non-finding.
///
/// The property under test was never actually about *time*: it is structural
/// — does the `World` guard span the per-particle loop? — and `Sim::lock_holds`
/// already answers that with a **count**, not a clock:
/// `with_particles_unlocked` (`sim.rs`) takes the guard *exactly twice* per
/// call — once to remove `ParticleSim` out of the `World`, once to put it back
/// — with the entire per-particle extract running in the gap between them,
/// under no guard at all. `extract_particles` itself takes one more, a read,
/// for `self.clock().interp_alpha` ahead of that. That count cannot be
/// inflated by scheduler noise or concurrent load the way a nanosecond figure
/// can, because acquiring and releasing a lock costs the same whether or not
/// anything else on the machine is busy, and it cannot be inflated by particle
/// volume either, because nothing about *how many* particles get processed in
/// the unguarded gap changes how many times the guard itself is taken.
///
/// The expected value is [`EXTRACT_EXPECTED_HOLDS`] rather than the "3" the
/// paragraph above adds up to, because [`Sim::reset_lock_holds`]'s own doc
/// says why: it records its *own* guard hold **after** zeroing the counters,
/// so the baseline the moment it returns is 1, not 0. Missing that the first
/// time this was written produced a failing assertion against a real,
/// deterministic count — not flakiness, just an under-counted expectation —
/// which is worth recording exactly because it is the failure mode "predict
/// the exact count" trades a duration bound's vagueness for: get the count
/// wrong and it fails **every time**, loudly, rather than most of the time,
/// quietly. So the expected count is exact and identical at 400 particles and
/// at 4,000 — [`EXTRACT_EXPECTED_HOLDS`], not "close to it" — which is the
/// sharper form of "predict the expected value, don't merely bound it"
/// `CLAUDE.md`'s *magnitude*-species note asks for.
///
/// Its negative control is
/// [`the_pre_fix_shape_of_extract_particles_fails_the_hold_bound`], which
/// reproduces the old shape (the whole extract inside *one* guard, plus the
/// same reset artifact) and must report [`PREFIX_EXPECTED_HOLDS`], not
/// [`EXTRACT_EXPECTED_HOLDS`], at both particle counts — still flat in the
/// count, but the wrong flat number, which is exactly what distinguishes "the
/// guard is taken twice, briefly, around O(1) resource moves" from "the guard
/// is taken once, around the whole call, including every particle in the loop
/// that produced this run's `alive` count".
#[test]
fn extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work() {
    let (small_holds, small_alive) =
        guard_acquisitions_for_extract(HOLD_MEASUREMENT_PARTICLES_SMALL);
    let (large_holds, large_alive) = guard_acquisitions_for_extract(HOLD_MEASUREMENT_PARTICLES);

    // The *world*-species guard: the flaw in a vacuous test lives in the
    // input, not the assert. An extract over an empty engine would satisfy the
    // count below trivially, so assert the volume first — at both ends, since
    // the count is meaningless as a claim about "per-particle work" if either
    // side did no work.
    assert!(
        small_alive >= HOLD_MEASUREMENT_PARTICLES_SMALL as usize
            && large_alive >= HOLD_MEASUREMENT_PARTICLES as usize,
        "the measurement needs real volume at both ends; alive={small_alive} and {large_alive}"
    );

    // The exact, load-independent claim: precisely `EXTRACT_EXPECTED_HOLDS`
    // guard acquisitions — the reset call's own hold, a clock read, then
    // remove `ParticleSim` and put it back — regardless of how many particles
    // sat in the unguarded gap between the last two.
    assert_eq!(
        small_holds, EXTRACT_EXPECTED_HOLDS,
        "extract_particles over {small_alive} particles took {small_holds} World guard \
         acquisitions, not the expected {EXTRACT_EXPECTED_HOLDS} (reset's own hold, a clock \
         read, remove ParticleSim, put it back)"
    );
    assert_eq!(
        large_holds, EXTRACT_EXPECTED_HOLDS,
        "extract_particles over {large_alive} particles took {large_holds} World guard \
         acquisitions, not the expected {EXTRACT_EXPECTED_HOLDS} — a 10x particle count must \
         not change how many times the guard is acquired, or the guard has started spanning \
         the per-particle work again"
    );
}

/// The negative control for the count above, and the reason it is evidence
/// rather than decoration: the *pre-fix shape* — extract run inside the write
/// guard — must report the wrong count, measured by the same instrument.
///
/// This is deliberately hand-written rather than a switch on `Sim`: a test
/// switch would have to survive in production code, and what needs proving is
/// that the detector distinguishes two shapes, not that a flag works.
///
/// Unlike the duration form this replaces, this control's expectation does not
/// depend on load or timing at all: `lodestone_ecs::hold_write` wraps the
/// *entire* extract in one guard by construction (see this function's own
/// body), so the guard-acquisition count is exactly
/// [`PREFIX_EXPECTED_HOLDS`] — deterministically, on any machine, at any
/// particle count — never "close to it" or "usually that".
#[test]
fn the_pre_fix_shape_of_extract_particles_fails_the_hold_bound() {
    let (small_holds, small_alive) =
        guard_acquisitions_for_prefix_shape(HOLD_MEASUREMENT_PARTICLES_SMALL);
    let (large_holds, large_alive) =
        guard_acquisitions_for_prefix_shape(HOLD_MEASUREMENT_PARTICLES);

    assert!(
        small_alive >= HOLD_MEASUREMENT_PARTICLES_SMALL as usize
            && large_alive >= HOLD_MEASUREMENT_PARTICLES as usize,
        "same input volume as the positive case; alive={small_alive} and {large_alive}"
    );

    assert_eq!(
        small_holds, PREFIX_EXPECTED_HOLDS,
        "the pre-fix shape must take exactly {PREFIX_EXPECTED_HOLDS} World guards over \
         {small_alive} particles (reset's own hold, plus the whole extract wrapped in one \
         `hold_write`), got {small_holds}"
    );
    assert_eq!(
        large_holds, PREFIX_EXPECTED_HOLDS,
        "the pre-fix shape must take exactly {PREFIX_EXPECTED_HOLDS} World guards over \
         {large_alive} particles, got {large_holds}"
    );
    assert_ne!(
        small_holds, EXTRACT_EXPECTED_HOLDS,
        "the detector must fire on the shape it exists to reject: this control's guard count \
         must disagree with the correct shape's count of {EXTRACT_EXPECTED_HOLDS} in \
         `extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work`, or \
         that bound is not discriminating"
    );
}

/// The frame-level claim, also measured: `Sim::step` takes **many short
/// guards**, not one long one.
///
/// `docs/world-unification.md` said "counted from the code it takes on the
/// order of 15 short guards plus ~8 per catch-up tick". This counts them, so a
/// future refactor that coalesced the frame into one long guard — which would
/// read as a tidy-up and would stall ingest for a whole frame — fails here.
/// The control for the mechanism is `lodestone_ecs`'s
/// `the_hold_meter_reports_a_deliberately_long_hold`.
#[test]
fn a_frame_takes_many_short_world_guards_and_no_long_one() {
    let mut sim = Sim::with_demo_world(test_config());
    // One frame long enough to run at least one catch-up tick.
    sim.step(0.1);

    sim.reset_lock_holds();
    let started = crate::platform::Instant::now();
    sim.step(0.1);
    let wall = started.elapsed();
    let holds = sim.lock_holds();

    eprintln!(
        "Sim::step(0.1): wall {:?}, {} holds totalling {} ns, longest {} ns",
        wall, holds.holds, holds.total_ns, holds.longest_ns
    );
    assert!(
        holds.holds >= 15,
        "a frame must be many short guards rather than one long one; counted {}",
        holds.holds
    );
    // A ceiling, not a target: 25 ms is "no single guard spans a 40 fps frame".
    // Absolute rather than a ratio here because a whole `step` legitimately
    // *is* mostly its two `run_schedule` holds, so a ratio would assert
    // nothing. Loose enough to survive a preempted CI core; the control above
    // shows a 30 ms hold is visible, so this ceiling can actually be crossed.
    assert!(
        holds.longest_ns < 25_000_000,
        "no single `World` guard in a frame may approach a frame: longest was {} ns",
        holds.longest_ns
    );
}

/// The other half of `ClientLevel.doAddParticle`'s filter: `options.particles`.
///
/// The pair is chosen so both arms are **deterministic**.
/// `Particles::particle_level_permits` transcribes
/// `calculateParticleLevel`, which is probabilistic for `DECREASED` (one spawn
/// in three is folded down to `MINIMAL`) but not for the other two: `ALL` never
/// folds, and `MINIMAL` is lifted only by the always-show flag, which this
/// fixture deliberately leaves clear. So `All` -> 9 and `Minimal` -> 0 are
/// exact counts, and a `Decreased` arm would need a statistical bound to say
/// anything — deliberately not asserted here rather than asserted loosely.
///
/// Both arms feed the **same** event to the **same** `Sim`, differing only in
/// the pushed level, so the second arm is the control for the first: it proves
/// the count is attributable to the option rather than to anything else about
/// the fixture.
#[test]
fn the_particles_option_gates_the_spawn_and_all_is_not_a_no_op() {
    use crate::net::NetUpdate;
    use lodestone_client::Vec3;
    use lodestone_particle::Sheet;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    sim.particles_mut(|p| {
        p.install_test_sheet_uv(HashMap::from([(
            (Sheet::Flame, 0u16),
            [0.0f32, 0.0, 0.0625, 0.0625],
        )]));
    });

    let origin = sim.player().position;
    let burst = |feed: &std::sync::mpsc::SyncSender<NetUpdate>| {
        feed.send(NetUpdate::Particles {
            kind: "flame".into(),
            long_distance: false,
            always_show: false,
            pos: Vec3::new(origin.x, origin.y, origin.z),
            offset: Vec3f::new(0.1, 0.1, 0.1),
            max_speed: 0.02,
            count: 9,
            options: lodestone_model::event::ParticleOptions::None,
        })
        .unwrap();
    };

    sim.set_particle_level(crate::config::ParticleLevel::Minimal);
    burst(&feed);
    sim.poll_net();
    assert_eq!(
        sim.particles_mut(|p| p.engine_mut().particles().len()),
        0,
        "Minimal must drop a nearby, non-override burst entirely — \
         `doAddParticle` spawns only when the folded level is not MINIMAL"
    );

    sim.set_particle_level(crate::config::ParticleLevel::All);
    burst(&feed);
    sim.poll_net();
    assert_eq!(
        sim.particles_mut(|p| p.engine_mut().particles().len()),
        9,
        "control failed to fail: All must spawn the same burst the Minimal arm \
         dropped, or the zero above says nothing about the option"
    );
}

/// `alwaysShow` — the second bool on `ClientboundLevelParticlesPacket`, which
/// was decoded and then dropped: `ClientEvent::Particles` did not carry it, so
/// `net_apply.rs` passed a literal `false` and the **Minimal** setting deleted
/// every packet particle that did not also set `overrideLimiter`.
///
/// The rule is a *reprieve*, not an exemption, which is what makes this gate's
/// shape unusual. `ClientLevel.calculateParticleLevel` lifts `MINIMAL` to
/// `DECREASED` one time in ten, and `DECREASED` folds back down one time in
/// three, so an always-show burst on `Minimal` survives with probability
/// `1/10 x 2/3 = 1/15`. A single send therefore proves nothing in either
/// direction and only a count over many sends can separate the hypotheses:
///
/// | hypothesis | spawns out of `SENDS` bursts |
/// |---|---|
/// | the flag never reaches `particle_level_permits` (the bug) | exactly 0 |
/// | the flag is an *exemption* (the plausible wrong port) | exactly `SENDS` |
/// | vanilla's reprieve | ~`SENDS / 15` |
///
/// `SENDS = 900` puts the expected count at 60 and makes a zero result
/// impossible in practice — `(14/15)^900` is about `1e-27` — while the
/// exemption hypothesis is separated by a factor of fifteen, far outside any
/// binomial spread. The bounds below are therefore wide on purpose: they are
/// there to tell three hypotheses apart, not to pin a number.
///
/// The `always_show: false` arm is the control, and it is *exact*: with the
/// flag clear the fold is deterministic and no burst may survive at all. It
/// runs on the same `Sim`, at the same level, with the same one-particle
/// burst, so the only difference between the two arms is the field under test.
#[test]
fn always_show_gives_a_minimal_setting_particle_a_reprieve_and_not_an_exemption() {
    use crate::net::NetUpdate;
    use lodestone_client::Vec3;
    use lodestone_particle::Sheet;

    const SENDS: usize = 900;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    sim.particles_mut(|p| {
        p.install_test_sheet_uv(HashMap::from([(
            (Sheet::Flame, 0u16),
            [0.0f32, 0.0, 0.0625, 0.0625],
        )]));
    });
    sim.set_particle_level(crate::config::ParticleLevel::Minimal);

    let origin = sim.player().position;
    // One particle per burst and a zero lifetime is not available, so the
    // engine is drained between sends instead: the count under test is "how
    // many bursts got through", and a burst that spawns leaves exactly one
    // particle behind.
    // Each burst carries `count: 1` with no offset, so a burst that survives
    // the filter leaves exactly one particle behind and the engine's population
    // *is* the number that got through. Queued in one batch and drained with a
    // single `poll_net` rather than one step per send, which is the whole
    // difference between a two-second gate and a half-minute one; the relay
    // channel holds 1024, comfortably above `SENDS`.
    let survivors = |feed: &std::sync::mpsc::SyncSender<NetUpdate>,
                         sim: &mut Sim,
                         always_show: bool| {
        for _ in 0..SENDS {
            feed.send(NetUpdate::Particles {
                kind: "flame".into(),
                // Clear, so nothing bypasses the level filter by the other
                // route: `overrideLimiter` skips the particle-level test
                // entirely, and with it set this gate would pass whatever
                // `always_show` did.
                long_distance: false,
                always_show,
                pos: Vec3::new(origin.x, origin.y, origin.z),
                offset: Vec3f::new(0.0, 0.0, 0.0),
                max_speed: 0.0,
                count: 1,
                options: lodestone_model::event::ParticleOptions::None,
            })
            .unwrap();
        }
        sim.poll_net();
        sim.particles_mut(|p| {
            let n = p.engine_mut().particles().len();
            p.engine_mut().clear();
            n
        })
    };

    let with_flag = survivors(&feed, &mut sim, true);
    let without_flag = survivors(&feed, &mut sim, false);

    assert_eq!(
        without_flag, 0,
        "control: with always_show clear, Minimal must drop every one of {SENDS} \
         non-override bursts — if this is non-zero the level filter is not \
         running at all and the other arm says nothing"
    );
    assert!(
        with_flag > 0,
        "always_show never reached particle_level_permits: {SENDS} bursts and not \
         one survived, where vanilla's one-in-fifteen reprieve makes zero a \
         1e-27 event"
    );
    assert!(
        with_flag < SENDS / 2,
        "always_show is being treated as an exemption rather than a reprieve: \
         {with_flag} of {SENDS} bursts survived, where vanilla's fold predicts \
         about {}",
        SENDS / 15
    );
}

/// Vanilla's render cutoff (`ClientLevel.doAddParticle`): a particle
/// farther than 32 blocks from the viewer is dropped unless the packet
/// sets `long_distance`. Two events at the same far-away position, one
/// with the flag and one without, must differ in whether anything
/// spawns — proving the cutoff is actually wired to the flag rather than
/// always on or always off.
#[test]
fn long_distance_flag_gates_the_far_away_cutoff() {
    use crate::net::NetUpdate;
    use lodestone_client::Vec3;
    use lodestone_particle::Sheet;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    sim.particles_mut(|p| {
        p.install_test_sheet_uv(HashMap::from([(
            (Sheet::Flame, 0u16),
            [0.0f32, 0.0, 0.0625, 0.0625],
        )]));
    });

    // Comfortably past the 32-block (sqrt(1024)) cutoff on every axis.
    let origin = sim.player().position;
    let far = Vec3::new(origin.x + 1000.0, origin.y, origin.z);

    feed.send(NetUpdate::Particles {
        kind: "flame".into(),
        long_distance: false,
        always_show: false,
        pos: far,
        offset: Vec3f::new(0.0, 0.0, 0.0),
        max_speed: 0.0,
        count: 3,
        options: lodestone_model::event::ParticleOptions::None,
    })
    .unwrap();
    sim.poll_net();
    assert_eq!(
        sim.particles_mut(|p| p.engine_mut().particles().len()),
        0,
        "a far-away burst without long_distance must be dropped, not spawned off-screen"
    );

    feed.send(NetUpdate::Particles {
        kind: "flame".into(),
        long_distance: true,
        always_show: false,
        pos: far,
        offset: Vec3f::new(0.0, 0.0, 0.0),
        max_speed: 0.0,
        count: 3,
        options: lodestone_model::event::ParticleOptions::None,
    })
    .unwrap();
    sim.poll_net();
    assert_eq!(
        sim.particles_mut(|p| p.engine_mut().particles().len()),
        3,
        "the same burst with long_distance set must bypass the cutoff"
    );
}
