//! Typed plugin message delivery and retention on the server's custom clock.

use bevy_ecs::prelude::*;

use super::{GameTick, ServerApp, ServerTaskScheduler};

#[derive(Message)]
struct Notice(u64);

#[test]
fn plugin_messages_expire_after_two_game_tick_boundaries() {
    let mut server = ServerApp::bootstrap_with(|app| {
        app.add_message::<Notice>();
        app.world_mut().write_message(Notice(17));
    });
    assert_eq!(server.app().world().resource::<Messages<Notice>>().len(), 1);
    server.run_game_tick();
    assert_eq!(server.app().world().resource::<Messages<Notice>>().len(), 1);
    server.run_game_tick();
    assert_eq!(server.app().world().resource::<Messages<Notice>>().len(), 0);
}

#[test]
fn scheduler_messages_survive_their_emission_tick_and_reach_a_late_reader() {
    let server = ServerApp::bootstrap_with(|app| {
        app.add_message::<Notice>();
        app.world_mut().resource_mut::<ServerTaskScheduler>()
            .schedule_once(1, |world, _| { world.write_message(Notice(23)); });
    });
    let mut world = server.into_world();
    world.run_schedule(GameTick);
    world.run_schedule(GameTick);
    let messages = world.resource::<Messages<Notice>>();
    let mut cursor = messages.get_cursor();
    assert_eq!(cursor.read(messages).map(|notice| notice.0).collect::<Vec<_>>(), [23]);
    assert_eq!(cursor.read(messages).count(), 0);
    world.run_schedule(GameTick);
    assert!(world.resource::<Messages<Notice>>().is_empty());
}
