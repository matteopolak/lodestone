//! **The wire path for commands, end to end.**
//!
//! Plugin commands landed registered, permission-gated and covered by sixteen
//! registry-driven tests — and reached zero players, because nothing decoded
//! serverbound `chat_command`. That is the island shape, and the *only* thing
//! that closes it is a gate which starts at a real frame on a real wire and
//! ends at an observable effect. A test that constructs a dispatcher and calls
//! it proves nothing about whether a player can reach it, and neither does one
//! asserting the registry contains a command.
//!
//! # What is real here, and why each piece has to be
//!
//! Nothing in this file is a stand-in for the thing it is testing:
//!
//! | piece | the real thing |
//! |---|---|
//! | the sender | `lodestone-client`'s `ClientHandle::command`, driving the real `V770Adapter` encoder |
//! | the frame | protocol 776 `chat_command` (id 7), length-prefixed on a real `Connection` |
//! | the server | `V770ServerProtocol::decode` and `lodestone_server::serve_connection_with_commands` |
//! | the dispatcher | `lodestone_ecs::commands::dispatch` against a real `CommandRegistry` in a real bevy `World` |
//! | the gate | a `Resource` the registered handler mutated, read back out of that same `World` |
//! | the reply | a real clientbound `system_chat` frame, decoded by the real client into `ClientEvent::Chat` |
//!
//! **Drive the registry, not the type**: [`EcsCommandSink`] never names the
//! command it is going to run. It receives a string off the wire and hands it
//! to `dispatch`, which resolves it through the registry — so a command that
//! failed to register, or registered under a different name, fails these tests.
//!
//! # Why this file lives here
//!
//! [`EcsCommandSink`] is the glue a real host writes, and a host is by
//! definition a crate that links **both** `lodestone-server` and
//! `lodestone-ecs`. `lodestone-server` must never link `lodestone-ecs` — that
//! prohibition is the reason this gate cannot live
//! in `lodestone-server`'s own tests without putting the forbidden edge into
//! the crate under test's dev-dependencies. This crate already depends on
//! `lodestone-server` and on the real client, and adding `lodestone-ecs` here
//! creates no cycle (see this crate's `Cargo.toml`).
//!
//! # The controls
//!
//! Two negative controls were **run and observed to fail**, not described. Both
//! required editing a file this test cannot reach, so both were performed by
//! hand against a scratchpad backup and restored with an md5 check; the
//! observed output of each is recorded on the test it controls.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy_app::App;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_ecs::commands::{
    CommandAnchor, CommandEntity, CommandExecutionContext, CommandOutcome, CommandRegistry,
    CommandSource, PluginCommand, PluginCommandsPlugin, dispatch,
};
use lodestone_ecs::permissions::{PermissionSubject, Permissions};
use lodestone_model::{ClientEvent, GameMode, Rotation, Text, Vec3};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    ChunkSource, CommandCaller, CommandDispatch, CommandResponse, CommandSink,
    CommandSource as ServerCommandSource, ContextualCommandRequest, ContextualCommandResponse,
    ContextualEntityAnchor, NoEntities, IntegratedServer, PlayerCandidate, ServerCommands,
    UNKNOWN_COMMAND, WorldgenChunkSource,
    serve_connection_with_commands,
};
use lodestone_server::commands::{CommandWorld, overworld_dimension};
use lodestone_server::dimension::{Dimension, DimensionalSource};
use lodestone_server::game_rules::GameRulesHandle;
use lodestone_server::portal::PortalIndex;
use lodestone_v26_2::{V770ServerProtocol, adapter};
use lodestone_worldgen::density::Density;

// ---------------------------------------------------------------------------
// The observable effect
// ---------------------------------------------------------------------------

/// What the registered command's handler mutates, and the *only* thing these
/// tests read to decide whether the command ran.
///
/// A counter rather than a flag on purpose: the gates below predict an exact
/// value (`1`, or `0`), so a double-dispatch and a no-dispatch are different
/// failures rather than both "not true".
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
struct Beacons {
    lit: u32,
}

/// Sources the real plugin handler observed. This lets the bridge test prove
/// the context reached the registry handler itself, rather than merely a host
/// adapter that reconstructed it and never dispatched.
#[derive(Resource, Default, Debug, Clone, PartialEq)]
struct ObservedSources(Vec<CommandSource>);

/// The permission node the test command is gated behind.
const LIGHT_BEACON: &str = "lodestone.test.beacon.light";

/// The feedback line the handler returns, asserted byte-for-byte on the client
/// side so a reply that merely *arrives* is not enough — it has to be this one.
const LIT_MESSAGE: &str = "Beacon lit.";

// ---------------------------------------------------------------------------
// The host glue this seam exists to make possible
// ---------------------------------------------------------------------------

/// A [`CommandSink`] over a real bevy `World` holding a real
/// [`CommandRegistry`] — i.e. exactly the ~30 lines a host writes to connect
/// `lodestone-server`'s wire to `lodestone-ecs`'s dispatcher.
///
/// The `Mutex` is the implementor's problem by design: [`CommandSink::run`]
/// takes `&self` because several connection tasks may call it, while `dispatch`
/// needs `&mut World`. Pushing that inward would have meant
/// `lodestone-server` knowing what a `World` is, which is the thing the seam
/// exists to avoid.
struct EcsCommandSink {
    /// The `World`, not the `App` it was built through: `App` owns a
    /// `Box<dyn FnOnce(App) -> AppExit>` runner and is therefore not `Send`,
    /// while [`CommandSink`] must be `Send + Sync` to cross onto a connection
    /// task. A real host has the same constraint and resolves it the same way
    /// — dispatch needs a `World`, never an `App`.
    world: Mutex<World>,
}

impl EcsCommandSink {
    /// Reads the effect back out of the same `World` the handler wrote to.
    fn beacons(&self) -> Beacons {
        *self.world.lock().expect("world lock").resource::<Beacons>()
    }

    fn observed_sources(&self) -> ObservedSources {
        self.world.lock().expect("world lock").resource::<ObservedSources>().clone()
    }
}

impl CommandSink for EcsCommandSink {
    fn run(&self, caller: &CommandCaller, command: &str) -> CommandResponse {
        let mut world = self.world.lock().expect("world lock");
        // The caller identity comes from the *connection's* login, never from
        // the command text — see `lodestone_server::command`'s module doc. This
        // line is where that guarantee is cashed in: the permission subject
        // `dispatch` resolves against is this uuid and nothing else.
        let source = CommandSource::player(caller.uuid, caller.username.clone());
        match dispatch(&mut world, &source, command) {
            Ok(CommandOutcome::Success(_)) => CommandResponse::Ran {
                feedback: vec![LIT_MESSAGE.to_owned()],
            },
            Ok(CommandOutcome::Failure(message)) => CommandResponse::refused(message),
            Err(error) => CommandResponse::refused(error.message()),
        }
    }

    fn run_contextual(&self, request: &ContextualCommandRequest) -> ContextualCommandResponse {
        let mut world = self.world.lock().expect("world lock");
        let entity = request.source.entity.as_ref().map(|entity| CommandEntity {
            uuid: entity.uuid,
            entity_id: entity.entity_id,
            username: entity.username.clone(),
        });
        let subject = entity
            .as_ref()
            .map_or(PermissionSubject::Console, |entity| PermissionSubject::Player(entity.uuid));
        let source = CommandSource::contextual(
            subject,
            request.source.name.clone(),
            CommandExecutionContext {
                entity,
                position: request.source.position,
                rotation: request.source.rotation,
                dimension: request.source.dimension.clone(),
                anchor: match request.source.anchor {
                    ContextualEntityAnchor::Feet => CommandAnchor::Feet,
                    ContextualEntityAnchor::Eyes => CommandAnchor::Eyes,
                },
                permission_level: request.source.permission_level,
            },
        );
        match dispatch(&mut world, &source, &request.command) {
            Ok(CommandOutcome::Success(result)) => ContextualCommandResponse::ran(result),
            Ok(CommandOutcome::Failure(message)) => ContextualCommandResponse::refused(message),
            Err(error) => ContextualCommandResponse::refused(error.message()),
        }
    }
}

/// Builds the host side: a real `App` with the real plugin, one registered
/// command whose handler increments [`Beacons`], and the permission state the
/// caller is given.
///
/// `permissions` is a three-way switch rather than a bool because the three
/// cases are genuinely different and one of them is the security-shaped one:
/// `Some(true)` grants the node, `Some(false)` installs `Permissions` but
/// grants nothing, and `None` **omits the `Permissions` resource entirely** —
/// the "a missing resource must not mean allow everything" case.
fn host(caller_uuid: uuid::Uuid, permissions: Option<bool>) -> Arc<EcsCommandSink> {
    let mut app = App::new();
    app.add_plugins(PluginCommandsPlugin);
    app.insert_resource(Beacons::default());
    app.insert_resource(ObservedSources::default());

    let mut command = PluginCommand::new("beacon");
    let root = command.root();
    let light = command.literal(root, "light");
    command.require_permission(light, LIGHT_BEACON);
    command.on_execute(light, |invocation| {
        invocation.world.resource_mut::<Beacons>().lit += 1;
        CommandOutcome::ok()
    });
    app.world_mut()
        .resource_mut::<CommandRegistry>()
        .register(command)
        .expect("the test command must register");

    let mut observe = PluginCommand::new("observe");
    let root = observe.root();
    observe.on_execute(root, |invocation| {
        invocation.world.resource_mut::<ObservedSources>().0.push(invocation.source.clone());
        CommandOutcome::Success(3)
    });
    app.world_mut()
        .resource_mut::<CommandRegistry>()
        .register(observe)
        .expect("the context-observing command must register");

    let mut fail = PluginCommand::new("fail");
    let root = fail.root();
    fail.on_execute(root, |_| CommandOutcome::Failure("plugin failed".to_owned()));
    app.world_mut()
        .resource_mut::<CommandRegistry>()
        .register(fail)
        .expect("the failing command must register");

    match permissions {
        Some(granted) => {
            let mut perms = Permissions::new();
            if granted {
                perms.grant(caller_uuid, LIGHT_BEACON);
            }
            app.insert_resource(perms);
        }
        // `PluginCommandsPlugin` inserts a `Permissions` of its own, so
        // reaching the missing-resource state means explicitly removing it.
        // Built this way rather than on a bare `World` precisely because the
        // plugin's job is to make this state unreachable — which is what makes
        // it worth proving the layer underneath still fails closed.
        None => {
            app.world_mut().remove_resource::<Permissions>();
        }
    }

    // Take the `World` out of the `App`, leaving the (non-`Send`) `App`
    // shell behind. Everything the plugin installed — `CommandRegistry`,
    // `Permissions`, `PlayerDirectory` — lives in the `World`, so nothing the
    // dispatcher reads is lost. The schedules are, and are not wanted:
    // `dispatch` is a direct call, not a system.
    let world = std::mem::take(app.world_mut());
    Arc::new(EcsCommandSink {
        world: Mutex::new(world),
    })
}

/// Direct plugin roots retain their established caller identity, while the new
/// contextual method is required to deliver `/execute`'s rewritten source to
/// the real registry handler and preserve its integer return value.
#[test]
fn direct_and_contextual_plugin_dispatch_keep_their_distinct_source_contracts() {
    let alice = uuid::Uuid::from_u128(1);
    let bob = uuid::Uuid::from_u128(2);
    let sink = host(alice, Some(true));

    assert!(sink.run(&CommandCaller::new(alice, "alice"), "observe").is_ran());
    let direct = sink.observed_sources();
    assert_eq!(direct.0.len(), 1);
    assert_eq!(direct.0[0].subject, PermissionSubject::Player(alice));
    assert_eq!(direct.0[0].name, "alice");

    let rules = GameRulesHandle::new();
    let state = lodestone_server::world_state::WorldStateHandle::new();
    let players = vec![
        PlayerCandidate {
            uuid: alice,
            entity_id: 41,
            username: "alice".to_owned(),
            position: Vec3::new(0.0, 64.0, 0.0),
            rotation: Rotation { yaw: 0.0, pitch: 0.0 },
            game_mode: GameMode::Survival,
            xp_level: 0,
            xp_points: 0,
        },
        PlayerCandidate {
            uuid: bob,
            entity_id: 42,
            username: "bob".to_owned(),
            position: Vec3::new(12.0, 70.0, -4.0),
            rotation: Rotation { yaw: 30.0, pitch: -15.0 },
            game_mode: GameMode::Survival,
            xp_level: 0,
            xp_points: 0,
        },
    ];
    let source = ServerCommandSource::player(
        alice,
        41,
        "alice",
        Vec3::new(0.0, 64.0, 0.0),
        Rotation { yaw: 0.0, pitch: 0.0 },
        overworld_dimension(),
        4,
    );
    let world = CommandWorld {
        rules: &rules,
        players: &players,
        state: &state,
        mobs: None,
        border: None,
        access: None,
        blocks: None,
    };
    let outcome = ServerCommands::new()
        .run_with_contextual_dispatch(
            &world,
            &source,
            "execute as bob at bob anchored eyes run observe",
            &CommandDispatch::installed(sink.clone()),
            &CommandCaller::new(alice, "alice"),
        )
        .expect("`execute` is a server root");
    assert!(outcome.response.is_ran(), "{outcome:?}");

    let observed = sink.observed_sources();
    let contextual = observed.0.last().expect("the contextual handler must run");
    assert_eq!(contextual.subject, PermissionSubject::Player(bob));
    assert_eq!(contextual.name, "bob");
    assert_eq!(contextual.execution.as_ref().and_then(|context| context.entity.as_ref().map(|entity| entity.entity_id)), Some(42));
    assert_eq!(contextual.execution.as_ref().map(|context| context.position), Some(Vec3::new(12.0, 70.0, -4.0)));
    assert_eq!(contextual.execution.as_ref().map(|context| context.rotation), Some(Rotation { yaw: 30.0, pitch: -15.0 }));
    assert_eq!(contextual.execution.as_ref().map(|context| context.dimension.clone()), Some(overworld_dimension()));
    assert_eq!(contextual.execution.as_ref().map(|context| context.anchor), Some(CommandAnchor::Eyes));
    assert_eq!(contextual.execution.as_ref().map(|context| context.permission_level), Some(4));
}

// ---------------------------------------------------------------------------
// The transport
// ---------------------------------------------------------------------------

fn profile(name: &str, uuid: uuid::Uuid) -> LoginProfile {
    LoginProfile {
        username: name.into(),
        uuid,
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

/// Deterministic, noise-free terrain — content is irrelevant here, but the
/// vertical extent must be the real overworld shape or the client's hardcoded
/// decode misaligns. Same source `server_liveness.rs` uses, for the same reason.
fn cheap_source() -> WorldgenChunkSource {
    WorldgenChunkSource::new(
        Density::YClampedGradient {
            from_y: -64.0,
            to_y: 64.0,
            from_value: 1.0,
            to_value: -1.0,
        },
        -64,
        384,
    )
}

/// The outcome of one end-to-end run: what the world recorded, and every chat
/// line the client actually decoded off the wire.
struct Run {
    beacons: Beacons,
    observed_sources: ObservedSources,
    chat: Vec<String>,
}

/// Joins a real client to a real server serving `sink`, sends `command` as a
/// real `chat_command` frame, and returns what happened on both sides.
///
/// Deliberately drives `handle.command(..)` rather than writing bytes: that is
/// the same call the game's own chat box makes, so the encoder under test is
/// the production one.
async fn run_command(sink: Arc<EcsCommandSink>, uuid: uuid::Uuid, command: &str) -> Run {
    run_commands(sink, uuid, &[command], 1).await
}

/// The multi-command form of [`run_command`]. `expected_replies` is explicit
/// because successful plugin terminals are allowed to be silent; a later
/// built-in readback supplies the observable replies for store tests.
async fn run_commands(
    sink: Arc<EcsCommandSink>,
    uuid: uuid::Uuid,
    commands: &[&str],
    expected_replies: usize,
) -> Run {
    run_commands_from_source(sink, uuid, commands, expected_replies, cheap_source()).await
}

/// [`run_commands`] with an explicit connection source. This makes the
/// dimension regression drive the same real `ChatCommand` wire arm after the
/// connection source has changed dimensions.
async fn run_commands_from_source<S>(
    sink: Arc<EcsCommandSink>,
    uuid: uuid::Uuid,
    commands: &[&str],
    expected_replies: usize,
    source: S,
) -> Run
where
    S: ChunkSource + 'static,
{
    let (client_end, server_end) = memory_pair();
    let dispatch = CommandDispatch::installed(sink.clone());

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        let _ = serve_connection_with_commands(
            &mut conn,
            &V770ServerProtocol,
            &source,
            &NoEntities,
            0,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &dispatch,
        )
        .await;
    });

    let (mut handle, mut events) = ClientBuilder::new(
        address(),
        profile("Commander", uuid),
        Box::new(adapter()),
    )
    .connect_with(client_end);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");

    for command in commands {
        handle.command(*command).expect("send the command");
    }

    // Collect chat until the reply arrives or the window closes. A bounded
    // wait, not an unbounded one: a test that hangs when the wire is broken is
    // a worse failure report than one that returns an empty `chat`.
    let mut chat = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && chat.len() < expected_replies {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::Chat { text, .. })) => {
                let line = plain(&text);
                // The join sequence sends its own welcome line first; it is not
                // what this gate is about.
                if line != "Welcome to Lodestone" {
                    chat.push(line);
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }

    let beacons = sink.beacons();
    let observed_sources = sink.observed_sources();
    handle.shutdown();
    server.abort();
    Run { beacons, observed_sources, chat }
}

/// The local production constructor, not just the lower-level serving helper.
/// This is the route `lodestone-shell::net` takes for a native singleplayer
/// session after it installs its ECS host adapter.
async fn run_integrated_commands(
    sink: Arc<EcsCommandSink>,
    uuid: uuid::Uuid,
    commands: &[&str],
    expected_replies: usize,
) -> Run {
    let dispatch = CommandDispatch::installed(sink.clone());
    let (server, client_end) = IntegratedServer::open_in_memory_with_mobs_and_commands(
        V770ServerProtocol,
        cheap_source(),
        (-1..=1, -1..=1),
        (8, 8),
        0,
        0,
        dispatch,
    );

    run_integrated_connection(sink, uuid, commands, expected_replies, server, client_end).await
}

/// The portable items constructor used by browser singleplayer. This test runs
/// it natively to exercise its real duplex/dispatch shape; the wasm target
/// check proves that identical constructor and shell call site compile there.
async fn run_items_integrated_commands(
    sink: Arc<EcsCommandSink>,
    uuid: uuid::Uuid,
    commands: &[&str],
    expected_replies: usize,
) -> Run {
    let dispatch = CommandDispatch::installed(sink.clone());
    let (server, client_end) = IntegratedServer::open_in_memory_with_items_and_commands(
        V770ServerProtocol,
        cheap_source(),
        0,
        dispatch,
    );

    run_integrated_connection(sink, uuid, commands, expected_replies, server, client_end).await
}

/// Drives one real integrated duplex after a constructor selected the host's
/// local connection shape.
async fn run_integrated_connection(
    sink: Arc<EcsCommandSink>,
    uuid: uuid::Uuid,
    commands: &[&str],
    expected_replies: usize,
    server: IntegratedServer,
    client_end: tokio::io::DuplexStream,
) -> Run {
    let (mut handle, mut events) = ClientBuilder::new(
        address(),
        profile("Commander", uuid),
        Box::new(adapter()),
    )
    .connect_with(client_end);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("integrated client never spawned");
    for command in commands {
        handle.command(*command).expect("send the command");
    }

    let mut chat = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && chat.len() < expected_replies {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::Chat { text, .. })) => {
                let line = plain(&text);
                if line != "Welcome to Lodestone" {
                    chat.push(line);
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }

    let beacons = sink.beacons();
    let observed_sources = sink.observed_sources();
    handle.shutdown();
    drop(server);
    Run { beacons, observed_sources, chat }
}

/// The plain text of a chat component.
fn plain(text: &Text) -> String {
    text.to_plain_string()
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// **The gate this whole issue is about.** A real `chat_command` frame from a
/// real client reaches a registered plugin command's handler, and the handler's
/// effect is observable.
///
/// The prediction is exact — `lit == 1`, not "nonzero" — so a double-dispatch
/// fails here too, and the reply is compared byte-for-byte rather than merely
/// counted.
///
/// **Control 1, run and observed.** Replacing this crate's `CHAT_COMMAND`
/// decode arm with `let _ = decode_full::<ChatCommand>(payload);
/// ServerBound::Ignored` — stranding it in exactly the state the other 43
/// decoded-but-unconnected serverbound variants are in today — turned this
/// file from `4 passed` into `1 passed; 3 failed`:
///
/// ```text
/// assertion `left == right` failed: a permitted command sent as a real
///   chat_command frame must run exactly once
///   left: Beacons { lit: 0 }
///  right: Beacons { lit: 1 }
/// ```
///
/// The effect vanished and the client received no reply at all (the sibling
/// no-sink test failed with `left: None`). So the detector is armed: this gate
/// fails when the decode hop that wires a real chat-command frame to its
/// registered handler is removed. Restored by `cp` from a scratchpad backup,
/// md5 verified identical.
#[tokio::test]
async fn a_real_chat_command_frame_reaches_a_registered_handler_and_its_effect_is_observable() {
    let uuid = uuid::Uuid::new_v4();
    let run = run_command(host(uuid, Some(true)), uuid, "beacon light").await;

    assert_eq!(
        run.beacons,
        Beacons { lit: 1 },
        "a permitted command sent as a real chat_command frame must run exactly once"
    );
    assert_eq!(
        run.chat,
        vec![LIT_MESSAGE.to_owned()],
        "the handler's feedback must come back as a real system_chat frame"
    );
}

/// A terminal plugin root under `/execute ... run` is still delivered over the
/// real wire, with the rewritten source and its integer result observed by the
/// existing store machinery. This specifically exercises the production Play
/// command arm rather than calling `ServerCommands` as a unit-level shortcut.
#[tokio::test]
async fn execute_run_reaches_a_plugin_over_the_wire_with_context_and_store_outcomes() {
    let uuid = uuid::Uuid::new_v4();
    let run = run_integrated_commands(
        host(uuid, Some(true)),
        uuid,
        &[
            "scoreboard objectives add out dummy",
            "execute as @s at @s anchored eyes run observe",
            "execute store result score @s out run observe",
            "scoreboard players get @s out",
            "execute store success score @s out run fail",
            "scoreboard players get @s out",
        ],
        4,
    )
    .await;

    let source = run
        .observed_sources
        .0
        .first()
        .expect("the terminal plugin handler must run over the wire");
    assert_eq!(source.subject, PermissionSubject::Player(uuid));
    let execution = source
        .execution
        .as_ref()
        .expect("/execute must carry a rewritten execution context");
    assert_eq!(execution.anchor, CommandAnchor::Eyes);
    assert_eq!(execution.entity.as_ref().map(|entity| entity.uuid), Some(uuid));
    assert_eq!(execution.permission_level, 4);
    assert!(
        run.chat.iter().any(|line| line == "Commander has 3 out"),
        "store result must preserve the plugin's integer outcome: {:?}",
        run.chat
    );
    assert!(
        run.chat.iter().any(|line| line == "Commander has 0 out"),
        "store success must write zero when the plugin handler fails: {:?}",
        run.chat
    );
}

/// A `ChatCommand` source takes its dimension from the live connection source,
/// not an overworld literal. The protocol's initial login still says overworld
/// in this focused fixture; the server-side source is deliberately Nether so
/// the handler can prove the context the dispatcher actually receives.
#[tokio::test]
async fn execute_run_forwards_the_connection_dimension_to_the_plugin_handler() {
    let uuid = uuid::Uuid::new_v4();
    let source = DimensionalSource::alone(cheap_source(), Dimension::Nether, PortalIndex::new());
    let run = run_commands_from_source(
        host(uuid, Some(true)),
        uuid,
        &["execute as @s at @s run observe", "gamerule keepinventory"],
        1,
        source,
    )
    .await;

    let execution = run
        .observed_sources
        .0
        .first()
        .and_then(|source| source.execution.as_ref())
        .expect("the contextual plugin handler must run over the wire");
    let nether: lodestone_model::ResourceKey = "minecraft:the_nether"
        .parse()
        .expect("literal resource key");
    assert_eq!(execution.dimension, nether);
}

/// Browser singleplayer has no tick loop and therefore chooses the portable
/// items constructor. Its local connection must still reach the same host
/// dispatch and carry `/execute`'s rewritten context.
#[tokio::test]
async fn portable_items_constructor_runs_execute_plugin_terminals() {
    let uuid = uuid::Uuid::new_v4();
    let run = run_items_integrated_commands(
        host(uuid, Some(true)),
        uuid,
        &["execute as @s at @s anchored eyes run observe", "gamerule keepinventory"],
        1,
    )
    .await;

    let execution = run
        .observed_sources
        .0
        .first()
        .and_then(|source| source.execution.as_ref())
        .expect("the portable local constructor must reach the plugin handler");
    assert_eq!(execution.anchor, CommandAnchor::Eyes);
    assert_eq!(execution.entity.as_ref().map(|entity| entity.uuid), Some(uuid));
}

/// **Permissions are enforced on the wire path, not only at the registry.**
///
/// Identical to the gate above in every respect except that the caller was not
/// granted [`LIGHT_BEACON`]. Same frame, same registry, same handler — and the
/// effect must not happen.
///
/// The `lit == 0` prediction is the load-bearing half. The refusal *message*
/// is deliberately not asserted: `lodestone-server` cannot tell a permission
/// denial from a parse failure (by design — see `CommandResponse::Refused`), so
/// asserting the text would be asserting the host's wording, not the property.
///
/// **Control 2, run and observed.** Making `CommandTree::node_allowed`
/// (`crates/lodestone-command/src/parse.rs`) return an unconditional `true`, so
/// a permission-gated node parses for anyone, failed exactly this test and
/// only this test — `3 passed; 1 failed`:
///
/// ```text
///   left: Beacons { lit: 1 }
///  right: Beacons { lit: 0 }
/// ```
///
/// The unprivileged command ran. That the other three stayed green is the
/// second half of the control: it moved the thing it was aimed at and nothing
/// else. Restored by `cp` from a scratchpad backup, md5 verified identical.
#[tokio::test]
async fn an_unprivileged_caller_cannot_run_a_gated_command_over_the_wire() {
    let uuid = uuid::Uuid::new_v4();
    let run = run_command(host(uuid, Some(false)), uuid, "beacon light").await;

    assert_eq!(
        run.beacons,
        Beacons { lit: 0 },
        "a caller without the permission node must not reach the handler"
    );
    assert_eq!(
        run.chat.len(),
        1,
        "the refusal must still be reported to the player, not silently dropped"
    );
}

/// **A missing `Permissions` resource must refuse, never ungate.**
///
/// The wire-path replica of
/// `dispatch_refuses_rather_than_ungates_when_permissions_are_missing` in
/// `crates/lodestone-ecs/tests/plugin_command_registry.rs`. That test
/// holds the property at the registry; this one holds it for a real frame
/// arriving from a real player, which is the configuration an operator would
/// actually be exposed by.
///
/// **Stated because it is the honest reading of the controls: neither control
/// moves this test.** It survived both — control 1 because a stranded frame
/// also leaves `lit == 0`, control 2 because the refusal here comes from the
/// missing *resource* (`CommandDispatchError::NotInstalled`) before any
/// parse-level filter runs. On its own it is therefore an absence assertion
/// with no detector of its own, the vacuous shape this repo keeps paying for.
///
/// What arms it is the **sibling** gate in this same file: the two differ only
/// in whether `Permissions` is present, and
/// `a_real_chat_command_frame_reaches_a_registered_handler_and_its_effect_is_observable`
/// proves that same frame, same registry and same handler *do* produce
/// `lit == 1` when it is. Read either alone and it proves little; the pair is
/// what shows the refusal is caused by the missing resource. Do not delete one
/// without the other.
#[tokio::test]
async fn a_missing_permissions_resource_refuses_the_wire_path_rather_than_ungating_it() {
    let uuid = uuid::Uuid::new_v4();
    let run = run_command(host(uuid, None), uuid, "beacon light").await;

    assert_eq!(
        run.beacons,
        Beacons { lit: 0 },
        "no Permissions resource must mean nothing runs, never that everything does"
    );
}

/// **The fail-closed direction of the seam itself**, one layer out from the
/// tests above: a server serving with no sink installed at all.
///
/// This is the configuration every pre-existing `serve_connection*` entry point
/// is in, so it is not hypothetical — it is what LAN and singleplayer do today
/// until a host installs a dispatcher. The frame must still decode and still be
/// answered, and the answer must be a refusal.
///
/// Asserting the exact `UNKNOWN_COMMAND` text, not merely that *a* reply came
/// back: a reply that arrives with the wrong body would be a decode bug wearing
/// a passing test.
#[tokio::test]
async fn a_server_with_no_sink_installed_answers_a_command_with_a_refusal() {
    let (client_end, server_end) = memory_pair();
    let source = cheap_source();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        let _ = serve_connection_with_commands(
            &mut conn,
            &V770ServerProtocol,
            &source,
            &NoEntities,
            0,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            // The `Default` — and the whole point of it being the `Default`.
            &CommandDispatch::none(),
        )
        .await;
    });

    let (mut handle, mut events) = ClientBuilder::new(
        address(),
        profile("Unhosted", uuid::Uuid::new_v4()),
        Box::new(adapter()),
    )
    .connect_with(client_end);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");
    handle.command("beacon light").expect("send the command");

    let mut refusal = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while refusal.is_none() && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::Chat { text, .. })) => {
                let line = plain(&text);
                if line != "Welcome to Lodestone" {
                    refusal = Some(line);
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }

    assert_eq!(
        refusal.as_deref(),
        Some(UNKNOWN_COMMAND),
        "with no dispatcher installed the frame must still decode and be refused"
    );

    handle.shutdown();
    server.abort();
}
