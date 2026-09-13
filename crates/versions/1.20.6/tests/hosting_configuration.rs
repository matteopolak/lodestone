//! Records complete configuration data from the version-matched headless oracle.
//! No per-packet-id cap is applied: every registry is required by hosting.

use std::time::Duration;
use lodestone_model::{ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter};
use lodestone_net::Connection;
use lodestone_world::World;

struct EmptySource;
impl lodestone_server::ChunkSource for EmptySource {
    fn column(&self, _x: i32, _z: i32) -> lodestone_server::ChunkColumn {
        lodestone_server::ChunkColumn::new(-64, 384)
    }
    fn block_state(&self, _x: i32, _y: i32, _z: i32) -> String { "minecraft:air".to_owned() }
    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String { "minecraft:plains".to_owned() }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        panic!("configuration-only fixture does not accept gameplay edits");
    }
}

#[tokio::test]
async fn integrated_server_delivers_complete_configuration_fixture_before_finish() {
    let protocol = lodestone_registry::server_protocol_for_protocol(766).unwrap();
    let (server, io) = lodestone_server::IntegratedServer::open_in_memory(protocol, EmptySource, 0);
    let captured = tokio::time::timeout(Duration::from_secs(10), capture_connection(Connection::new(io)))
        .await.expect("hosted configuration completes");
    assert!(captured == include_str!("../src/generated/hosting-configuration.txt"),
        "every captured registry, feature and tag payload must arrive before FinishConfiguration");
    server.shutdown().await;
}

#[tokio::test]
#[ignore = "requires the version-matched headless oracle; set LODESTONE_REGEN=1 to replace fixture"]
async fn complete_configuration_matches_the_oracle() {
    let capture = tokio::time::timeout(Duration::from_secs(30), capture()).await
        .expect("configuration must finish within 30 seconds");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/generated/hosting-configuration.txt");
    let actual = capture.lines().filter(|line| line.starts_with("configuration ")).count();
    if std::env::var("LODESTONE_REGEN").as_deref() == Ok("1") {
        std::fs::write(path, capture).expect("write captured hosting configuration");
    } else {
        let expected = std::fs::read_to_string(path).expect("committed hosting configuration");
        assert_eq!(actual, expected.lines().filter(|line| line.starts_with("configuration ")).count(),
            "complete oracle configuration packet count; regenerate the truncated fixture");
        assert!(capture == expected, "configuration payloads differ from the version-matched oracle");
    }
}

async fn capture() -> String {
    let conn = Connection::connect(("127.0.0.1", 25598)).await.expect("connect oracle");
    capture_connection(conn).await
}

async fn capture_connection<T: lodestone_net::Transport>(mut conn: Connection<T>) -> String {
    let adapter = lodestone_v1_20_6::adapter_for(766);
    let mut world = World::new();
    let profile = LoginProfile {
        username: lodestone_testsupport::unique_username(),
        uuid: uuid::Uuid::new_v4(),
    };
    let server = ServerAddress { host: "127.0.0.1".to_owned(), port: 25598 };
    let mut state = ConnectionState::Handshaking;
    for directive in adapter.begin_login(&profile, &server).unwrap() {
        apply(&mut conn, &mut state, directive).await;
    }
    let mut output = "# Complete Configuration registries, features and tags captured from Minecraft 1.20.6 (766).\n".to_owned();
    let mut registries = std::collections::BTreeSet::new();
    loop {
        let (id, body) = conn.read_packet().await.expect("read oracle").expect("oracle remains connected");
        if state == ConnectionState::Configuration {
            if id == 3 { break; }
            if [7, 12, 13].contains(&id) {
                use std::fmt::Write;
                write!(output, "configuration {id} ").unwrap();
                for byte in &body { write!(output, "{byte:02x}").unwrap(); }
                output.push('\n');
                if id == 7 {
                    use lodestone_core::Decode;
                    let mut reader = lodestone_core::Reader::new(&body);
                    let registry = lodestone_v1_20_6::packets::configuration::RegistryData::decode(
                        &mut reader, lodestone_core::Ctx { version: 766 },
                    ).expect("registry payload");
                    reader.ensure_empty().unwrap();
                    assert!(registry.entries.iter().all(|entry| entry.data.is_some()),
                        "known-pack response must request complete entry payloads");
                    assert!(registries.insert(registry.registry.clone()), "registry sent twice");
                    println!("registry {} entries={}", registry.registry, registry.entries.len());
                }
            }
        }
        let directives = adapter.handle_packet(&mut world, state, id, &body).expect("configuration decoder");
        for directive in directives { apply(&mut conn, &mut state, directive).await; }
    }
    assert!(registries.contains("minecraft:dimension_type"));
    assert!(registries.contains("minecraft:worldgen/biome"));
    println!("complete registry count={}", registries.len());
    output
}

async fn apply<T: lodestone_net::Transport>(conn: &mut Connection<T>, state: &mut ConnectionState, directive: Directive) {
    match directive {
        Directive::Send { packet_id, payload } => conn.write_packet(packet_id, &payload).await.unwrap(),
        Directive::SetState(next) => *state = next,
        Directive::SetCompression(threshold) => conn.set_compression(threshold),
        _ => {}
    }
}
