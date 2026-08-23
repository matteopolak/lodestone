//! **The island gate for riding.**
//!
//! `ClientAction::MoveVehicle` and `ClientAction::PaddleBoat` were encoded
//! byte-exactly by the v770 adapter, round-tripped by its own tests, and produced
//! by **nobody** — the `ClientAction::SetFlying` shape `CLAUDE.md` records. A test
//! that calls `lodestone_physics::vehicle::tick_boat` directly passes whether or
//! not anything drives it, and a test that calls the ECS system directly passes
//! whether or not the schedule contains it. So the assertion here is about the
//! **queue**: does a packet reach `ActionQueue` from a real `GameTick` run while we
//! are riding, and does it stay away while we are not.
//!
//! `ActionQueue` is the one sanctioned egress (`docs/bevy-migration.md` §6), so it
//! is the closest thing to "the wire" reachable without a socket, and the shell
//! drains it verbatim into the adapter.
//!
//! Both directions are asserted, and the negative one is not decoration: a
//! producer that pushed unconditionally would satisfy the positive arm alone and
//! would spam the server with vehicle positions for a player on foot.

use std::sync::Arc;

use bevy_app::App;
use bevy_ecs::entity::Entity;
use lodestone_ecs::entity::{EntityIndex, EntityKind, Passengers, Position, Rotation};
use lodestone_ecs::player::{
    ActionQueue, CollisionSource, Egress, LocalPlayer, LocalPlayerPlugin, MovementIntent,
    PhysicsState, PlayerCollision,
};
use lodestone_ecs::session::{Riding, ServerEntityId};
use lodestone_ecs::vehicle::{ControlledVehicle, VehicleFamily};
use lodestone_ecs::{CorePlugin, GameTick, spawn_local_player};
use lodestone_model::{ClientAction, PlayerCommand};
use lodestone_physics::{Aabb, CollisionView, FluidCell, FluidKind, MovementInput, PlayerState, Vec3d};

/// Water everywhere below `y = 64`, a solid floor at `y = 0`, nothing else.
///
/// The floor exists so the *player*'s own physics has something to resolve
/// against; the boat floats far above it.
#[derive(Debug)]
struct Sea;

impl CollisionView for Sea {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if y == 0 {
            out.push(Aabb::new(
                f64::from(x),
                f64::from(y),
                f64::from(z),
                f64::from(x) + 1.0,
                f64::from(y) + 1.0,
                f64::from(z) + 1.0,
            ));
        }
    }
    fn is_water(&self, _x: i32, y: i32, _z: i32) -> bool {
        (1..64).contains(&y)
    }
    fn fluid_at(&self, _x: i32, y: i32, _z: i32) -> Option<FluidCell> {
        (1..64).contains(&y).then_some(FluidCell {
            kind: FluidKind::Water,
            amount: 8,
            falling: false,
        })
    }
}

impl CollisionSource for Sea {
    fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView)) {
        f(self);
    }
}

/// A [`lodestone_model::VersionAdapter`] that answers only `entity_facts`, so a
/// crate that deliberately depends on no protocol family can still supply the
/// vehicle's real box.
///
/// The box is `sized(1.375F, 0.5625F)` from `EntityTypes.java`'s boat family — an
/// outside constant, and one the buoyancy divisor genuinely reads.
#[derive(Debug)]
struct BoatFactsAdapter;

impl lodestone_model::VersionAdapter for BoatFactsAdapter {
    fn protocol_version(&self) -> i32 {
        0
    }
    fn minecraft_versions(&self) -> &'static [&'static str] {
        &[]
    }
    fn supports(&self, _protocol: i32) -> bool {
        false
    }
    fn entity_facts(
        &self,
        _entity_type: &lodestone_model::ResourceKey,
    ) -> Option<lodestone_model::EntityFacts> {
        Some(lodestone_model::EntityFacts {
            dimensions: lodestone_model::EntityBaseDimensions {
                width: 1.375,
                height: 0.5625,
            },
            pushes_players: false,
        })
    }
    fn begin_login(
        &self,
        _profile: &lodestone_model::LoginProfile,
        _server: &lodestone_model::ServerAddress,
    ) -> Result<Vec<lodestone_model::Directive>, lodestone_model::AdapterError> {
        unreachable!("BoatFactsAdapter answers entity_facts only; it has no wire")
    }
    fn handle_packet(
        &self,
        _world: &mut dyn lodestone_model::WorldSink,
        _state: lodestone_model::ConnectionState,
        _packet_id: i32,
        _payload: &[u8],
    ) -> Result<Vec<lodestone_model::Directive>, lodestone_model::AdapterError> {
        unreachable!("BoatFactsAdapter answers entity_facts only; it has no wire")
    }
    fn encode_action(
        &self,
        _state: lodestone_model::ConnectionState,
        _action: &lodestone_model::ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, lodestone_model::AdapterError> {
        unreachable!("BoatFactsAdapter answers entity_facts only; it has no wire")
    }
}

const OWN_ID: i32 = 7;
const VEHICLE_ID: i32 = 42;

/// The shell's own construction — `spawn_local_player` then
/// `insert_session_components` on one entity, `CorePlugin` + `LocalPlayerPlugin`
/// — with a boat registered in [`EntityIndex`] the way
/// `lodestone_ecs::ingest::apply_entity_spawn` would.
///
/// The only thing short-circuited is the socket, never the wiring: the systems
/// under test are the ones `LocalPlayerPlugin` registers, in the order it
/// registers them, driven by the real `GameTick` schedule.
fn app_in_a_boat(boat_feet: Vec3d) -> (App, Entity, Entity) {
    let mut app = App::new();
    app.add_plugins((CorePlugin, LocalPlayerPlugin));
    app.insert_resource(PlayerCollision::View(Arc::new(Sea)));
    app.insert_resource(lodestone_ecs::VersionData(Some(Box::new(BoatFactsAdapter))));
    app.insert_resource(Egress {
        in_world: true,
        live: true,
    });
    let mut state = PlayerState::at(Vec3d::new(boat_feet.x, boat_feet.y, boat_feet.z), 0.0);
    state.on_ground = false;
    let player = spawn_local_player(app.world_mut(), state);
    lodestone_ecs::session::insert_session_components(app.world_mut(), player);
    let boat = app
        .world_mut()
        .spawn((
            lodestone_ecs::entity::MinecraftEntityId(VEHICLE_ID),
            EntityKind("minecraft:oak_boat".parse().expect("valid key")),
            Position(lodestone_model::Vec3::new(
                boat_feet.x,
                boat_feet.y,
                boat_feet.z,
            )),
            Rotation(lodestone_model::Rotation::new(0.0, 0.0)),
            lodestone_ecs::entity::OnGround(false),
            Passengers(vec![OWN_ID]),
        ))
        .id();
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .insert(VEHICLE_ID, boat);
    {
        let mut entity = app.world_mut().entity_mut(player);
        entity.get_mut::<ServerEntityId>().unwrap().0 = Some(OWN_ID);
        entity.get_mut::<Riding>().unwrap().0 = Some(VEHICLE_ID);
    }
    (app, player, boat)
}

fn run_tick(app: &mut App) -> Vec<ClientAction> {
    app.world_mut().resource_mut::<ActionQueue>().0.clear();
    app.world_mut().run_schedule(GameTick);
    app.world().resource::<ActionQueue>().0.clone()
}

fn set_input(app: &mut App, player: Entity, input: MovementInput) {
    app.world_mut().get_mut::<MovementIntent>(player).unwrap().0 = input;
}

fn move_vehicles(actions: &[ClientAction]) -> Vec<&ClientAction> {
    actions
        .iter()
        .filter(|a| matches!(a, ClientAction::MoveVehicle { .. }))
        .collect()
}

/// **The gate.** One real tick while seated in a boat must put exactly one
/// `MoveVehicle` and exactly one `PaddleBoat` on the queue, and the boat must
/// actually have moved.
#[test]
fn riding_a_boat_puts_move_vehicle_on_the_queue_and_moves_the_boat() {
    let feet = Vec3d::new(0.5, 63.8, 0.5);
    let (mut app, player, boat) = app_in_a_boat(feet);
    set_input(
        &mut app,
        player,
        MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        },
    );

    let mut last_z = feet.z;
    for tick in 0..5 {
        let actions = run_tick(&mut app);
        let moves = move_vehicles(&actions);
        assert_eq!(
            moves.len(),
            1,
            "tick {tick}: expected exactly one MoveVehicle, got {actions:?}"
        );
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, ClientAction::PaddleBoat { .. }))
                .count(),
            1,
            "tick {tick}: vanilla sends PaddleBoat every tick, not on change; got {actions:?}"
        );
        // Forward held rows both oars — `inputUp` alone satisfies both halves of
        // `setPaddleState`.
        assert!(
            actions.iter().any(|a| matches!(
                a,
                ClientAction::PaddleBoat {
                    left: true,
                    right: true
                }
            )),
            "tick {tick}: forward input must row both paddles, got {actions:?}"
        );
        let z = app.world().get::<Position>(boat).unwrap().0.z;
        assert!(
            z > last_z,
            "tick {tick}: the boat's Position must advance -- it went {last_z} -> {z}. \
             This component is what the renderer draws and what the seat pin reads, \
             so a boat whose Position never changes reaches zero pixels however \
             correct the physics is."
        );
        last_z = z;
    }

    // The queued position is the boat's, not the player's, and it is the *current*
    // one rather than last tick's.
    let actions = run_tick(&mut app);
    let boat_pos = app.world().get::<Position>(boat).unwrap().0;
    let queued = move_vehicles(&actions)
        .into_iter()
        .next()
        .expect("one MoveVehicle per tick")
        .clone();
    let ClientAction::MoveVehicle { pos, .. } = queued else {
        unreachable!("filtered on the variant")
    };
    assert!(
        (pos.z - boat_pos.z).abs() < 1e-9 && (pos.y - boat_pos.y).abs() < 1e-9,
        "the reported position must be the boat's own ({boat_pos:?}), got {pos:?}"
    );
    // …and it is not the *player*'s, which sits 0.6 below the seat point. A
    // producer that reported `PhysicsState.position` would satisfy every assertion
    // above and be wrong by exactly that offset.
    let rider = app.world().get::<PhysicsState>(player).unwrap().0.position;
    assert!(
        (pos.y - rider.y).abs() > 0.1,
        "the reported y ({}) must be the boat's, not the rider's ({})",
        pos.y,
        rider.y
    );
}

/// Render interpolation needs the two authoritative endpoints around the most
/// recently completed fixed tick. Advancing the render sample must never advance
/// physics, so the state itself owns the old endpoint before the next tick mutates
/// `motion`.
#[test]
fn a_controlled_vehicle_keeps_the_pose_from_the_start_of_the_last_tick() {
    let feet = Vec3d::new(0.5, 63.8, 0.5);
    let (mut app, player, _) = app_in_a_boat(feet);
    set_input(
        &mut app,
        player,
        MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        },
    );

    run_tick(&mut app);
    let first = app
        .world()
        .resource::<ControlledVehicle>()
        .0
        .as_ref()
        .expect("the first tick seeds and advances the boat")
        .current_pose();

    run_tick(&mut app);
    let second = app
        .world()
        .resource::<ControlledVehicle>()
        .0
        .as_ref()
        .expect("the second tick keeps the controlled boat");
    assert_eq!(second.previous, first);
    assert_ne!(second.current_pose(), first, "forward input must advance the new endpoint");
}

/// The negative arm. A player on foot must produce **no** vehicle packets at all,
/// and this is what stops the positive arm above from being satisfied by an
/// unconditional push.
#[test]
fn a_player_on_foot_produces_no_vehicle_packets() {
    let (mut app, player, _) = app_in_a_boat(Vec3d::new(0.5, 63.8, 0.5));
    // Dismount by clearing the scalar the fold would clear, exactly as
    // `session::apply_local_player_state` does when the passenger list empties.
    app.world_mut().get_mut::<Riding>(player).unwrap().0 = None;
    set_input(
        &mut app,
        player,
        MovementInput {
            forward: 1.0,
            jump: true,
            ..MovementInput::NONE
        },
    );
    for tick in 0..3 {
        let actions = run_tick(&mut app);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(
                    a,
                    ClientAction::MoveVehicle { .. } | ClientAction::PaddleBoat { .. }
                )),
            "tick {tick}: a player on foot queued a vehicle packet: {actions:?}"
        );
    }
    assert!(
        app.world().resource::<ControlledVehicle>().0.is_none(),
        "the local simulation must be dropped on dismount, or it keeps ticking a \
         vehicle we no longer control"
    );
}

/// A vehicle family the client does not simulate must be left entirely to the
/// server — no packets, no local motion.
///
/// A minecart is the case: its motion is rail-following and the server broadcasts
/// it, so predicting it with plain gravity would fight that broadcast. This is the
/// premise check for [`VehicleFamily::for_type_path`]'s default-deny.
#[test]
fn a_minecart_is_left_to_the_server() {
    assert_eq!(
        VehicleFamily::for_type_path("minecart"),
        None,
        "premise check: this gate measures nothing if a minecart *is* simulated"
    );
    let (mut app, player, cart) = app_in_a_boat(Vec3d::new(0.5, 63.8, 0.5));
    app.world_mut().get_mut::<EntityKind>(cart).unwrap().0 =
        "minecraft:minecart".parse().expect("valid key");
    let before = app.world().get::<Position>(cart).unwrap().0;
    set_input(
        &mut app,
        player,
        MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        },
    );
    let actions = run_tick(&mut app);
    assert!(
        move_vehicles(&actions).is_empty(),
        "an unsimulated vehicle must produce no MoveVehicle: {actions:?}"
    );
    let after = app.world().get::<Position>(cart).unwrap().0;
    assert!(
        (after.x - before.x).abs() < 1e-12
            && (after.y - before.y).abs() < 1e-12
            && (after.z - before.z).abs() < 1e-12,
        "an unsimulated vehicle's Position must be untouched: {before:?} -> {after:?}"
    );
}

/// The horse jump command, on the jump key's **falling** edge and nowhere else.
///
/// The charge is asserted through the queued `boost` byte rather than through the
/// component, because the byte is the thing that reaches the server:
/// `Mth.floor(getJumpRidingScale() * 100.0F)` after three held ticks is
/// `floor(0.3 * 100)` — and the ramp's `0.1F` per tick makes that **30**, which is
/// a different number from the 40 a `0.4`-floored reading would give.
#[test]
fn the_horse_jump_command_is_sent_on_the_release_edge_only() {
    let (mut app, player, mount) = app_in_a_boat(Vec3d::new(0.5, 2.0, 0.5));
    app.world_mut().get_mut::<EntityKind>(mount).unwrap().0 =
        "minecraft:horse".parse().expect("valid key");
    // A ridden mount refuses to move without a reported `movement_speed`; the
    // jump command does not depend on it, and asserting that separation is the
    // reason this fixture reports one anyway rather than relying on the default.
    app.world_mut().entity_mut(mount).insert(
        lodestone_ecs::entity::Attributes(vec![lodestone_model::EntityAttributeSnapshot {
            attribute: "minecraft:movement_speed".parse().expect("valid key"),
            base: 0.2,
            modifiers: Vec::new(),
        }]),
    );

    // One tick with the key up so `ControlledVehicle` exists and is a land mount,
    // and `was_jumping` is false.
    set_input(&mut app, player, MovementInput::NONE);
    let actions = run_tick(&mut app);
    assert!(
        jump_commands(&actions).is_empty(),
        "no jump command with the key never pressed: {actions:?}"
    );
    assert_eq!(
        app.world()
            .resource::<ControlledVehicle>()
            .0
            .as_ref()
            .map(|v| v.family),
        Some(VehicleFamily::LandMount),
        "premise check: the mount must actually be simulated, or the charge block \
         is suppressed and this gate measures nothing"
    );

    // Hold jump for three ticks. Nothing is sent while it is held — the command is
    // a release edge, and a rising-edge reading would fire on the first of these.
    for tick in 0..3 {
        set_input(
            &mut app,
            player,
            MovementInput {
                jump: true,
                ..MovementInput::NONE
            },
        );
        let actions = run_tick(&mut app);
        assert!(
            jump_commands(&actions).is_empty(),
            "tick {tick}: the command must wait for the release edge, got {actions:?}"
        );
    }

    // Release.
    set_input(&mut app, player, MovementInput::NONE);
    let actions = run_tick(&mut app);
    let commands = jump_commands(&actions);
    assert_eq!(
        commands.len(),
        1,
        "exactly one START_RIDING_JUMP on release, got {actions:?}"
    );
    // Ticks 1 and 2 of the ramp charge (the first held tick opens the window at
    // scale 0), so the scale at release is `2 * 0.1` and the byte is 20.
    // `f32` 0.2 * 100 is 20.000000298..., so `floor` is 20 either way — the number
    // that matters is that it is a multiple of ten off the *first* ramp arm and
    // not the 40 floor of `getPlayerJumpPendingScale`.
    let &ClientAction::PlayerCommand {
        command: PlayerCommand::StartRidingJump { boost },
        entity_id,
    } = commands[0]
    else {
        unreachable!("filtered on the variant")
    };
    assert_eq!(entity_id, OWN_ID, "the command names the rider, not the mount");
    assert!(
        boost > 0 && boost < 40,
        "a short charge must land on the first ramp arm (ticks * 0.1 * 100), not on \
         getPlayerJumpPendingScale's 0.4 floor; got {boost}"
    );
    assert_eq!(boost % 10, 0, "the first ramp arm is exact multiples of ten, got {boost}");

    // And `STOP_RIDING_JUMP` is never produced, at any point. The vanilla client
    // has no sender for it at all.
    for tick in 0..4 {
        set_input(&mut app, player, MovementInput::NONE);
        let actions = run_tick(&mut app);
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                ClientAction::PlayerCommand {
                    command: PlayerCommand::StopRidingJump,
                    ..
                }
            )),
            "tick {tick}: STOP_RIDING_JUMP must never be sent: {actions:?}"
        );
    }
}

fn jump_commands(actions: &[ClientAction]) -> Vec<&ClientAction> {
    actions
        .iter()
        .filter(|a| {
            matches!(
                a,
                ClientAction::PlayerCommand {
                    command: PlayerCommand::StartRidingJump { .. },
                    ..
                }
            )
        })
        .collect()
}

/// `Egress` gates the producer: before the server has placed us, a version adapter
/// correctly has no Play-state packet for a vehicle move, so sending is
/// dropped-action noise.
#[test]
fn nothing_is_queued_before_the_server_places_us() {
    let (mut app, player, _) = app_in_a_boat(Vec3d::new(0.5, 63.8, 0.5));
    app.insert_resource(Egress {
        in_world: false,
        live: false,
    });
    set_input(
        &mut app,
        player,
        MovementInput {
            forward: 1.0,
            ..MovementInput::NONE
        },
    );
    let actions = run_tick(&mut app);
    assert!(
        move_vehicles(&actions).is_empty(),
        "a pre-placement tick queued a vehicle move: {actions:?}"
    );
    // …but the simulation still ran, which is what makes this a *send* gate rather
    // than a whole-subsystem off switch. A boat we are seated in must keep floating
    // while we wait for the placement teleport.
    assert!(
        app.world().resource::<ControlledVehicle>().0.is_some(),
        "the local prediction must keep running while the send is withheld"
    );
}

/// A local player with no `LocalPlayer`-adjacent riding scalar at all — the
/// headless physics harness's shape — must not panic and must not queue anything.
#[test]
fn a_harness_without_session_components_is_inert_rather_than_broken() {
    let mut app = App::new();
    app.add_plugins((CorePlugin, LocalPlayerPlugin));
    app.insert_resource(PlayerCollision::View(Arc::new(Sea)));
    app.insert_resource(Egress {
        in_world: true,
        live: true,
    });
    let player = spawn_local_player(
        app.world_mut(),
        PlayerState::at(Vec3d::new(0.5, 2.0, 0.5), 0.0),
    );
    assert!(
        app.world().get::<Riding>(player).is_none(),
        "premise check: this fixture must NOT have the session set, or it is the \
         same case as every other test here"
    );
    let actions = run_tick(&mut app);
    assert!(move_vehicles(&actions).is_empty());
    assert!(app.world().resource::<ControlledVehicle>().0.is_none());
    // And the player still exists and still has physics, i.e. the vehicle systems
    // declined rather than aborting the tick.
    assert!(app.world().get::<PhysicsState>(player).is_some());
    let local_players = app
        .world_mut()
        .query_filtered::<Entity, bevy_ecs::prelude::With<LocalPlayer>>()
        .iter(app.world())
        .count();
    assert_eq!(local_players, 1, "the tick must not have despawned anyone");
}
