// The per-connection feeds the native and browser loops share: chat, arm swings, commands,
// night skips, resource-pack and plugin-channel pushes, the sleep vote's voter count, the
// periodic player save, and the deferred dig. All of these ride the one `serve_play` loop, so a
// native run here exercises the browser's loop too.

use lodestone_server::{
    CommandCaller, CommandResponse, CommandSink, PlayerAwareSource, PlayerRegistry, ResourcePackPush, ResourcePackUrl,
    SleepEvent, live_save::LiveSaveSlot, serve_connection_with_live_save,
    serve_connection_with_plugin_channels, serve_connection_with_resource_pack,
};

/// Lets the server's timers run, then returns everything it sent.
async fn settle(client: &mut Connection<DuplexStream>) -> Vec<(i32, Vec<u8>)> {
    tokio::time::sleep(Duration::from_millis(1500)).await;
    drain_available(client).await
}

fn ids(packets: &[(i32, Vec<u8>)], id: i32) -> Vec<&Vec<u8>> {
    packets.iter().filter(|(got, _)| *got == id).map(|(_, p)| p).collect()
}

fn payload_string(payload: &[u8]) -> String {
    Reader::new(payload).string(256).expect("string payload")
}

async fn send_string(client: &mut Connection<DuplexStream>, id: i32, text: &str) {
    let mut w = Writer::default();
    w.string(text);
    client.write_packet(id, w.as_slice()).await.expect("send");
}

/// Joins `name` on a fresh connection served against the shared `registry` and `world`.
async fn join_shared(
    name: &str,
    registry: &PlayerRegistry,
    world: &WorldStateHandle,
) -> (Connection<DuplexStream>, tokio::task::JoinHandle<Result<lodestone_server::ServeSummary, ServerError>>) {
    let (client_end, server_end) = memory_pair();
    let entities = PlayerAwareSource::new(NoEntities, registry.clone());
    let world = world.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection_with_access_and_state(
            &mut conn,
            &FakeProtocol,
            &AirSource,
            &entities,
            0,
            &AccessHandle::default(),
            &world,
            &BlockEntityHandle::default(),
            None,
        )
        .await
    });
    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, name, 1).await;
    (client, server)
}

#[tokio::test(start_paused = true)]
async fn chat_reaches_the_other_player() {
    let registry = PlayerRegistry::new();
    let world = WorldStateHandle::default();
    let (mut a, _sa) = join_shared("PeerA", &registry, &world).await;
    let (mut b, _sb) = join_shared("PeerB", &registry, &world).await;
    let _ = settle(&mut a).await;
    let _ = settle(&mut b).await;

    send_string(&mut a, CHAT_C2S, "hello there").await;
    let heard = settle(&mut b).await;
    let lines: Vec<String> = ids(&heard, SYSTEM_CHAT_S2C).into_iter().map(|p| payload_string(p)).collect();
    assert!(lines.iter().any(|l| l == "<PeerA> hello there"), "B did not hear A: {lines:?}");
}

#[tokio::test(start_paused = true)]
async fn a_swing_is_broadcast_to_other_players_and_not_back_to_the_swinger() {
    let registry = PlayerRegistry::new();
    let world = WorldStateHandle::default();
    let (mut a, _sa) = join_shared("PeerA", &registry, &world).await;
    let (mut b, _sb) = join_shared("PeerB", &registry, &world).await;
    let _ = settle(&mut a).await;
    let _ = settle(&mut b).await;

    a.write_packet(SWING_C2S, &[]).await.expect("swing");
    let seen_by_b = settle(&mut b).await;
    let seen_by_a = settle(&mut a).await;
    assert_eq!(ids(&seen_by_b, ANIMATE_S2C).len(), 1, "B sees exactly A's swing");
    assert!(ids(&seen_by_a, ANIMATE_S2C).is_empty(), "the swinger is not told about its own swing");
}

struct Refuses;

impl CommandSink for Refuses {
    fn run(&self, caller: &CommandCaller, command: &str) -> CommandResponse {
        CommandResponse::refused(format!("{} ran {command}", caller.username))
    }
}

#[tokio::test(start_paused = true)]
async fn a_command_runs_through_the_host_sink_and_its_answer_reaches_the_sender() {
    let (client_end, server_end) = memory_pair();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        lodestone_server::serve_connection_with_commands(
            &mut conn,
            &FakeProtocol,
            &AirSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
            &BlockTickFeed::default(),
            &ExplosionFeed::default(),
            &CommandDispatch::installed(Arc::new(Refuses)),
        )
        .await
    });
    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Caller", 1).await;
    send_string(&mut client, CHAT_COMMAND_C2S, "frobnicate now").await;
    let got = settle(&mut client).await;
    let lines: Vec<String> = ids(&got, SYSTEM_CHAT_S2C).into_iter().map(|p| payload_string(p)).collect();
    assert!(lines.iter().any(|l| l.contains("Caller ran frobnicate now")), "{lines:?}");
    drop(client);
    let _ = server.await;
}

#[tokio::test(start_paused = true)]
async fn a_skipped_night_reanchors_the_clock_and_the_voter_count_follows_the_registry() {
    let registry = PlayerRegistry::new();
    let world = WorldStateHandle::default();
    let (mut a, _sa) = join_shared("PeerA", &registry, &world).await;
    let (mut b, _sb) = join_shared("PeerB", &registry, &world).await;
    let _ = settle(&mut a).await;
    let _ = settle(&mut b).await;
    assert_eq!(world.sleep_vote().active(), 2, "every registered player is a voter");

    world.sleep_feed().publish(SleepEvent::SkippedNight { game_time: 777_000, morning: 24_000 });
    for client in [&mut a, &mut b] {
        let got = settle(client).await;
        let anchored = ids(&got, SET_TIME_S2C).into_iter().any(|p| {
            let mut r = Reader::new(p);
            r.i64().unwrap() == 777_000 && r.bool().unwrap() && r.i64().unwrap() == 24_000
        });
        assert!(anchored, "a client missed the night skip");
    }
}

#[tokio::test(start_paused = true)]
async fn a_plugin_broadcast_reaches_a_client_that_registered_the_channel() {
    let (client_end, server_end) = memory_pair();
    let channels = PluginChannelRegistry::default();
    let channels_s = channels.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection_with_plugin_channels(
            &mut conn,
            &FakeProtocol,
            &AirSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
            &BlockTickFeed::default(),
            &ExplosionFeed::default(),
            &channels_s,
        )
        .await
    });
    let mut client = Connection::new(client_end);
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    send_string(&mut client, LOGIN_START, "Plugged").await;
    let _ = client.read_packet().await.expect("read");
    client.write_packet(LOGIN_ACKNOWLEDGED, &[]).await.expect("ack");
    let mut w = Writer::default();
    w.string("minecraft:register");
    w.bytes(b"test:chan");
    client.write_packet(CUSTOM_PAYLOAD_C2S, w.as_slice()).await.expect("register");
    client.write_packet(FINISH_CONFIGURATION, &[]).await.expect("finish");
    let _ = client.read_packet().await.expect("time");
    let _ = drain_join_view(&mut client, 1).await;

    channels.broadcast("test:chan".parse().expect("channel"), b"payload");
    let got = settle(&mut client).await;
    let sent: Vec<_> = ids(&got, CUSTOM_PAYLOAD_S2C);
    assert_eq!(sent.len(), 1, "one broadcast on the registered channel");
    assert_eq!(payload_string(sent[0]), "test:chan");
    drop(client);
    let _ = server.await;
}

#[tokio::test(start_paused = true)]
async fn the_player_save_lands_in_the_memory_store_periodically() {
    let (client_end, server_end) = memory_pair();
    let slot = LiveSaveSlot::with_memory_store();
    let store = slot.memory_store().expect("memory store");
    let world = WorldStateHandle::default();
    let slot_s = slot.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection_with_live_save(
            &mut conn,
            &FakeProtocol,
            &AirSource,
            &NoEntities,
            0,
            &world,
            &BlockEntityHandle::default(),
            &slot_s,
        )
        .await
    });
    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Saver", 1).await;
    send_player_moved(&mut client, 3.5, 70.0, 4.5).await;
    // Past the periodic-save interval while staying connected: keep answering keep-alives.
    for _ in 0..40 {
        for (id, payload) in settle(&mut client).await {
            if id == KEEP_ALIVE_S2C {
                client.write_packet(KEEP_ALIVE_C2S, &payload).await.expect("answer keep-alive");
            }
        }
    }
    let saved = store.read(Uuid::nil()).expect("read").expect("a periodic save happened while connected");
    assert!((saved.pos.x - 3.5).abs() < 1e-6 && (saved.pos.z - 4.5).abs() < 1e-6, "{:?}", saved.pos);
    drop(client);
    let _ = server.await;
}

#[tokio::test(start_paused = true)]
async fn a_pushed_resource_pack_reaches_the_client() {
    let (client_end, server_end) = memory_pair();
    let feed = ResourcePackPushFeed::default();
    let feed_s = feed.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection_with_resource_pack(
            &mut conn,
            &FakeProtocol,
            &AirSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
            &BlockTickFeed::default(),
            &ExplosionFeed::default(),
            &feed_s,
        )
        .await
    });
    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Packed", 1).await;
    feed.publish(ResourcePackPush {
        id: Uuid::from_u128(9),
        url: ResourcePackUrl::parse("https://packs.example/p.zip").expect("url"),
        hash: String::new(),
        required: false,
        prompt: None,
    });
    let got = settle(&mut client).await;
    let sent = ids(&got, RESOURCE_PACK_PUSH_S2C);
    assert_eq!(sent.len(), 1, "one push");
    assert_eq!(payload_string(sent[0]), "https://packs.example/p.zip");
    drop(client);
    let _ = server.await;
}
