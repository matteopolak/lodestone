//! **The registration gate** (issue #436's "declared islands", item 2).
//!
//! `tests/observes_the_game_event_bus.rs` — this crate's other test — registers
//! the plugin correctly and then writes its own events with
//! `World::write_message`. That proves the *reader* works. It cannot prove the
//! plugin is reachable from a real session, because the producer in it is the
//! test itself: it is the **world** species of vacuous test `CLAUDE.md`
//! describes, where the flaw is in the input data rather than anywhere readable
//! in the assertion. The question that species demands — *which implementation
//! does this test's transport actually resolve to, and is it the one production
//! uses?* — had the answer "neither; the test is the transport".
//!
//! This file answers it properly. Nothing here writes a `GameEvent`:
//!
//! * a real [`lodestone_server::IntegratedServer`] runs on one end of a real
//!   [`lodestone_net::memory_pair`], speaking the **real** 26.2 wire format
//!   (`lodestone_registry::server_protocol_for_protocol`);
//! * the real `lodestone-client` driver runs on the other end with the real
//!   26.2 `VersionAdapter`;
//! * the plugin is registered through [`lodestone_app::client_app`] +
//!   `add_plugins` + [`lodestone_client::ClientBuilder::ecs`] — *the public
//!   composition path*, byte for byte what `lodestone_shell::sim::Sim` and any
//!   third-party embedder use. Nothing privileged, nothing test-only;
//! * every observed event is therefore produced by the server, decoded by the
//!   adapter, and pushed onto the bus by `lodestone_client`'s **own**
//!   `SharedState::apply`, which is `pub(crate)` and so cannot be short-circuited
//!   even deliberately.
//!
//! # Why the expected value originates outside the code under test
//!
//! `Driver::dispatch` does exactly this, in this order:
//!
//! ```text
//! self.read_model.apply(&event);   // -> push_to_game_event_bus -> the plugin
//! let _ = self.events.send(event).await;   // -> EventStream, a sibling path
//! ```
//!
//! So the [`lodestone_client::EventStream`] is a **second, independent**
//! delivery path for the same events, down a plain mpsc channel that touches
//! neither the bus nor this plugin. That makes it a legitimate oracle: the
//! sequence it yields is decided by a real server and a real decoder, and this
//! file asserts the plugin's log against it **value for value, in order** —
//! `starts_with`, not a count and not a direction.
//!
//! The one-sided `starts_with` (rather than equality) is not slack: `apply`
//! precedes `send`, so at the instant the stream yields event *i* the driver may
//! already have pushed event *i+1* onto the bus. The log can lead the stream by
//! a bounded amount; it can never lag it, and it can never reorder.
//!
//! # Why draining after each received event is what makes this deterministic
//!
//! `Messages<GameEvent>` is double-buffered and `age_game_event_bus` drops
//! events two ticks old, so a test that ran `GameTick` on a timer could lose
//! events to aging and flake. Because `apply` happens *before* `send`, an event
//! handed to us by the stream is already on the bus **now**; running one
//! `GameTick` at that moment observes it before any aging can occur. In
//! production the shell's frame loop is what runs `GameTick`; here this test is,
//! which is the only thing it stands in for.

use std::sync::Arc;
use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientEvent, LoginProfile, ServerAddress};
use lodestone_ecs::GameTick;
use lodestone_ecs::ecs::entity::Entity;
use lodestone_ecs::parking_lot::RwLock;
use lodestone_event_logger::{EventLog, EventLoggerPlugin};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};

/// Protocol 776 — MC 26.2, the `v770` family. The **only** family that
/// implements `ServerProtocol`, so it is the only one that can sit on the far
/// end of this connection (`CLAUDE.md`: "joining and hosting are different
/// sets").
const PROTOCOL: i32 = 776;

/// Upper bound on events read from the oracle — a **cap**, not a target.
///
/// This used to be a fixed count of 14, chosen to land well past the Play
/// transition, with a note observing that "a count is exactly the kind of
/// constant that stays green while its meaning rots". It rotted in the other
/// direction: two legitimate additions to the join sequence — the clientbound
/// `COMMANDS` tree and the window-0 inventory snapshot — pushed `ChunkLoaded`
/// past the fourteenth event, and [`reached_play`] failed on a correct session.
///
/// So the stopping condition is now the **premise itself**: read until the
/// session has demonstrably reached Play with a chunk in hand, and stop there.
/// That is strictly better than a larger number, because the next packet added
/// to the join sequence moves the count again and no constant can be chosen
/// ahead of it. The cap exists only so a session that reaches Play and never
/// sends a chunk fails on a bounded read with a clear message, rather than
/// waiting out [`DEADLINE`].
const ORACLE_EVENT_CAP: usize = 64;

/// Generous, because it is a *liveness* bound and not the thing under test: a
/// debug-build login handshake with registry data on a loaded machine is not
/// fast. Exceeding it fails loudly rather than hanging.
const DEADLINE: Duration = Duration::from_secs(60);

/// An all-air world. The terrain is irrelevant here — this file is about
/// whether events reach a registered plugin, not what they contain — and a
/// worldgen source would spend seconds per column for nothing.
struct FlatAir;

impl ChunkSource for FlatAir {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 1)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this
        // fixture is tiny and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this
        // fixture is tiny and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design. Explicit rather than inherited — issue #440.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

fn profile() -> LoginProfile {
    LoginProfile {
        username: "event-logger".into(),
        uuid: uuid::Uuid::nil(),
    }
}

/// Compose a client `World` **exactly as an embedder does** — and, when
/// `logger` is `Some`, register the plugin through the ordinary public
/// `add_plugins`, with no privileged path of any kind.
///
/// `logger` being an `Option` is what makes the negative control below share
/// this function rather than reimplement it: the control differs from the gate
/// in **one** argument, so it cannot accidentally differ in anything else. A
/// control built out of a second, parallel setup would be free to drift into
/// proving something other than "registration is what causes observation".
fn compose(logger: Option<EventLoggerPlugin>) -> (lodestone_ecs::EcsHandle, Entity) {
    // The six version-free plugins production composes. `lodestone_shell::sim::
    // Sim::client_app` is this plus four shell-local, render-shaped ones, none
    // of which the event bus touches.
    let mut app = lodestone_app::client_app();

    // The registration under test. `EventLoggerPlugin::build` installs
    // `GameEventBusPlugin` itself when absent, which is what makes
    // `SharedState::adopting`'s `contains_resource::<GameEventBus>()` probe
    // answer `true` below — the plugin turning the bus on by being present is
    // the whole opt-in mechanism, so it must happen before the `World` is taken.
    if let Some(logger) = logger {
        app.add_plugins(logger);
    }

    // After every `add_plugins`, per `spawn_session`'s own contract.
    let player = lodestone_physics::PlayerState::at(lodestone_physics::Vec3d::new(8.0, 64.0, 8.0), 0.0);
    let session = lodestone_app::spawn_session(&mut app, player);

    // Take the `World` out of the `App` and put it behind the handle lock —
    // `lodestone_ecs::new_ingest_handle`'s own last line, and azalea's shape.
    // Done by hand rather than by calling that helper because the helper
    // composes its own fixed plugin set, and the entire point here is to supply
    // one the caller composed.
    let ecs: lodestone_ecs::EcsHandle = Arc::new(RwLock::new(std::mem::take(app.world_mut())));
    (ecs, session)
}

/// Drive a real session and return every event the **`EventStream`** yielded up
/// to and including the one that satisfied [`play_reached`], having run one
/// `GameTick` immediately after each so the bus is drained while that event is
/// guaranteed still live.
///
/// Returns the oracle sequence. The caller asserts the plugin's log against it.
async fn run_session(ecs: &lodestone_ecs::EcsHandle, session: Entity) -> Vec<ClientEvent> {
    let protocol = lodestone_registry::server_protocol_for_protocol(PROTOCOL)
        .expect("the v770 feature is enabled in this crate's dev-dependencies");
    let adapter = lodestone_registry::adapter_for_protocol(PROTOCOL)
        .expect("the v770 feature is enabled in this crate's dev-dependencies");

    // `view_radius` 0: one column. Enough to reach Play; nothing here reads
    // terrain.
    let (_server, client_io) = IntegratedServer::open_in_memory(protocol, FlatAir, 0);

    let (_handle, mut events) = ClientBuilder::new(address(), profile(), adapter)
        .ecs(Arc::clone(ecs), session)
        .connect_with(client_io);

    let mut oracle = Vec::new();
    let deadline = tokio::time::Instant::now() + DEADLINE;
    // Read until the premise holds, not until a counter does — see
    // `ORACLE_EVENT_CAP`. `reached_play` below is the real stopping condition,
    // and it is re-asserted after the loop so hitting the cap fails loudly
    // rather than silently proving less.
    while !play_reached(&oracle) && oracle.len() < ORACLE_EVENT_CAP {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "the real session yielded only {} events within {DEADLINE:?} and never reached \
                     Play with a chunk — the oracle itself never ran, so nothing below would have \
                     been proven.\n{oracle:#?}",
                    oracle.len()
                )
            })
            .expect("the session ended before it reached Play with a chunk");

        // `apply` already ran for this event in `Driver::dispatch`, so it is on
        // the bus right now. Drain before aging can touch it.
        ecs.write().run_schedule(GameTick);
        oracle.push(event);
    }
    reached_play(&oracle);
    oracle
}

/// [`reached_play`]'s condition as a predicate, for use as a loop bound.
///
/// Deliberately the *same* two checks, so the stopping condition and the
/// asserted premise cannot drift apart — the failure mode a separate hand-kept
/// count had.
fn play_reached(oracle: &[ClientEvent]) -> bool {
    oracle.iter().any(|e| matches!(e, ClientEvent::Login { .. }))
        && oracle
            .iter()
            .any(|e| matches!(e, ClientEvent::ChunkLoaded { .. }))
}

/// The oracle's own premise, asserted rather than assumed: this really was a
/// **joined session**, not four registry packets and a stall.
///
/// `CLAUDE.md`: "a control's premise can be false before the feature under test
/// ever existed, and it fails in the safe-looking direction". Both tests in this
/// file rest on real events having flowed; if a future protocol change made the
/// first fourteen events all Configuration-phase, the negative control would
/// still pass (an empty log is an empty log) while silently proving much less.
/// This is the guard against that, and it runs in the control too.
fn reached_play(oracle: &[ClientEvent]) {
    assert!(
        oracle.iter().any(|e| matches!(e, ClientEvent::Login { .. })),
        "no Login event: the session never left the Configuration phase.\n{oracle:#?}"
    );
    assert!(
        oracle
            .iter()
            .any(|e| matches!(e, ClientEvent::ChunkLoaded { .. })),
        "no ChunkLoaded event: the session reached Play but the world never arrived.\n{oracle:#?}"
    );
}

/// **The gate.** A plugin registered through the public composition path must
/// observe the events of a real session, in order, by value.
///
/// This is the assertion `App::is_plugin_added::<EventLoggerPlugin>()` would
/// *not* have made: that probe passes for a plugin whose `build` stopped
/// inserting what consumers read, and would have passed throughout the entire
/// period this crate was an island. What is asserted here is the **effect** —
/// real events, in the log — which is unreachable unless registration, the bus
/// opt-in, `SharedState::adopting`'s probe, `push_to_game_event_bus` and the
/// `MessageReader` system are all simultaneously intact.
#[tokio::test]
async fn a_registered_logger_observes_a_real_session_through_the_public_plugin_api() {
    let (plugin, log) = EventLoggerPlugin::new();
    let (ecs, session) = compose(Some(plugin));

    assert!(log.is_empty(), "precondition: nothing observed before connecting");

    let oracle = run_session(&ecs, session).await;
    let observed = log.events();

    assert!(
        !oracle.is_empty() && play_reached(&oracle),
        "non-vacuity: the oracle must have reached Play with a chunk, or the comparison below \
         proves nothing.\n{oracle:#?}"
    );
    assert!(
        observed.starts_with(&oracle),
        "the plugin's log must be the EventStream's sequence, value for value and in order.\n\
         oracle ({} events):   {oracle:#?}\n\
         observed ({} events): {observed:#?}",
        oracle.len(),
        observed.len(),
    );
}

/// **The negative control, run rather than described.** The identical session,
/// composed by the identical function, differing in exactly one argument: the
/// plugin is built but never registered.
///
/// Its log must stay empty. Without this, the gate above is satisfied by any
/// world in which events reach the log by *some* route — an ambient default
/// registration, a second logger, a bus that pushes to every `Arc` it has ever
/// seen. With it, registration is shown to be the cause.
///
/// Note what this control also pins down, and why it is the honest expression
/// of the decision recorded in `docs/plugin-api.md`: if anyone later adds
/// `EventLoggerPlugin` to `Sim::client_app`'s tuple — making it a *default* part
/// of the shipped client, which it deliberately is not — the effect would not be
/// to make this test redundant. It would make it **fail**.
#[tokio::test]
async fn an_unregistered_logger_observes_nothing_from_the_same_real_session() {
    let (_plugin, log) = EventLoggerPlugin::new();
    let (ecs, session) = compose(None);

    let oracle = run_session(&ecs, session).await;

    assert!(
        !oracle.is_empty() && play_reached(&oracle),
        "the control's premise: the same real events really did flow this time too — \
         without this the empty log below would prove only that nothing happened.\n{oracle:#?}"
    );
    assert!(
        log.events().is_empty(),
        "an unregistered plugin must observe nothing, but its log holds {:#?}",
        log.events()
    );
}

/// A compile-time reminder that [`EventLog`] is the only handle a consumer
/// needs, kept so the gate above cannot be "fixed" by reaching into internals.
#[allow(dead_code)]
fn log_handle_is_the_public_surface(log: &EventLog) -> usize {
    log.len()
}
