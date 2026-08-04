//! End-to-end proof that `lodestone-event-logger` is a real consumer of
//! `lodestone_ecs`'s plugin event bus, not an island with a plausible-looking
//! unit test: builds a real `App`, writes `GameEvent`s the way
//! `lodestone_client::state::SharedState::apply` does (`World::write_message`,
//! no schedule of its own), runs the real `GameTick` schedule, and reads the
//! result back through the public `EventLog` handle — never by reaching into
//! the plugin's internals.

use lodestone_ecs::app::App;
use lodestone_ecs::{GameEvent, GameTick};
use lodestone_event_logger::EventLoggerPlugin;
use lodestone_model::ClientEvent;

fn ping(id: i32) -> ClientEvent {
    ClientEvent::Ping { id }
}

/// The exact number of events written must be the exact number observed —
/// predicting the value, not merely its direction, per `CLAUDE.md`'s
/// "predict exact values" standard.
#[test]
fn every_written_event_is_observed_exactly_once() {
    let (plugin, log) = EventLoggerPlugin::new();
    let mut app = App::new();
    app.add_plugins(plugin);

    assert!(log.is_empty(), "precondition: nothing observed yet");

    for id in 0..5 {
        app.world_mut().write_message(GameEvent(ping(id)));
    }
    app.world_mut().run_schedule(GameTick);

    assert_eq!(log.len(), 5, "exactly the five written events, no more, no fewer");
    assert_eq!(
        log.events(),
        (0..5).map(ping).collect::<Vec<_>>(),
        "arrival order must be preserved"
    );
}

/// **The negative control.** A `GameEvent` written but the schedule never
/// run must not be observed — proof the log is fed by the real
/// `MessageReader` pipeline (which only drains on a `GameTick` run) rather
/// than by some side channel that would make the positive test above
/// vacuous.
#[test]
fn an_event_written_without_running_the_schedule_is_not_observed_yet() {
    let (plugin, log) = EventLoggerPlugin::new();
    let mut app = App::new();
    app.add_plugins(plugin);

    app.world_mut().write_message(GameEvent(ping(1)));
    assert!(
        log.is_empty(),
        "a system that has not run yet cannot have observed anything"
    );

    app.world_mut().run_schedule(GameTick);
    assert_eq!(log.len(), 1, "running the schedule is what makes it observed");
}

/// Two independent loggers, each with its own plugin instance, must not
/// share a log — the same "two independent states" shape
/// `lodestone_client::state::tests` uses for the chunk store, applied here to
/// prove `EventLoggerPlugin::new`'s `Arc` really is per-instance.
#[test]
fn two_loggers_do_not_share_a_log() {
    let (plugin_a, log_a) = EventLoggerPlugin::new();
    let mut app_a = App::new();
    app_a.add_plugins(plugin_a);

    let (plugin_b, log_b) = EventLoggerPlugin::new();
    let mut app_b = App::new();
    app_b.add_plugins(plugin_b);

    app_a.world_mut().write_message(GameEvent(ping(1)));
    app_a.world_mut().run_schedule(GameTick);

    assert_eq!(log_a.len(), 1);
    assert!(log_b.is_empty(), "b's log must be untouched by a's event");
}
