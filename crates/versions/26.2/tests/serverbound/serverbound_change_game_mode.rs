//! Hermetic byte-exact test for the `change_game_mode` serverbound backlog
//! encoder.
//!
//! Expected payload is built independently of the adapter's own codec, so a
//! symmetric bug cannot pass. Layout is verified against 26.2's
//! `ServerboundChangeGameModePacket` (a single VarInt `GameType` id via
//! `vanilla's own byte buf codecs's own id mapper`: `0` survival, `1` creative, `2` adventure, `3`
//! spectator — matching `GameType`'s declared enum order exactly).
//!
//! Sent by the singleplayer/LAN cheats-enabled F4 game-mode switcher. This
//! action has no live call site yet (no game-mode-switcher UI), but the
//! server remains authoritative regardless — it's free to ignore the
//! request if the sender lacks permission.

use lodestone_model::{ClientAction, ConnectionState, GameMode, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;

fn varint(v: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut u = v as u32;
    loop {
        let byte = (u & 0x7F) as u8;
        u >>= 7;
        if u != 0 {
            out.push(byte | 0x80);
        } else {
            out.push(byte);
            break;
        }
    }
    out
}

#[test]
fn change_game_mode_ordinals_match_declared_enum_order() {
    let adapter = V770Adapter::new();
    let cases = [
        (GameMode::Survival, 0i32),
        (GameMode::Creative, 1),
        (GameMode::Adventure, 2),
        (GameMode::Spectator, 3),
    ];
    for (mode, ordinal) in cases {
        let encoded = adapter
            .encode_action(
                ConnectionState::Play,
                &ClientAction::ChangeGameMode { mode },
            )
            .expect("encode change game mode");
        assert_eq!(
            encoded,
            Some((play::serverbound::CHANGE_GAME_MODE, varint(ordinal))),
            "mismatch for {mode:?}"
        );
    }
}

#[test]
fn change_game_mode_is_not_encoded_outside_play() {
    let adapter = V770Adapter::new();
    assert_eq!(
        adapter
            .encode_action(
                ConnectionState::Configuration,
                &ClientAction::ChangeGameMode {
                    mode: GameMode::Creative,
                }
            )
            .expect("encode outside play"),
        None
    );
}

async fn next_abilities(events: &mut lodestone_client::EventStream) -> (bool, bool, bool, bool) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let lodestone_model::ClientEvent::AbilitiesChanged {
                invulnerable, flying, can_fly, instabuild, ..
            } = events.recv().await.expect("ability response stream stays open") {
                return (invulnerable, flying, can_fly, instabuild);
            }
        }
    }).await.expect("server answers the game-mode request")
}

#[test]
fn player_abilities_decode_only_the_flight_bit_and_reject_partial_frames() {
    use lodestone_core::State;
    use lodestone_server::{ServerBound, ServerProtocol};
    use lodestone_v26_2::V770ServerProtocol;
    for (flags, flying) in [(0, false), (1, false), (2, true), (0xfd, false), (0xff, true)] {
        assert!(matches!(
            V770ServerProtocol.decode(State::Play, play::serverbound::PLAYER_ABILITIES, &[flags]),
            ServerBound::PlayerAbilitiesChanged { flying: actual } if actual == flying
        ));
    }
    for body in [&[][..], &[2, 0][..]] {
        assert!(matches!(
            V770ServerProtocol.decode(State::Play, play::serverbound::PLAYER_ABILITIES, body),
            ServerBound::Ignored
        ));
    }
}

#[tokio::test]
async fn reported_flight_survives_creative_mode_packets_and_commands() {
    use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
    use lodestone_server::{IntegratedServer, WorldgenChunkSource};
    use lodestone_v26_2::{V770ServerProtocol, adapter};
    use lodestone_worldgen::density::Density;
    use std::time::Duration;

    let source = WorldgenChunkSource::new(Density::YClampedGradient {
        from_y: -64.0, to_y: 64.0, from_value: 1.0, to_value: -1.0,
    }, -64, 384);
    let (server, io) = IntegratedServer::open_in_memory(V770ServerProtocol, source, 0);
    let (mut handle, mut events) = ClientBuilder::new(
        ServerAddress { host: "memory".into(), port: 0 },
        LoginProfile { username: "Flyer".into(), uuid: uuid::Uuid::new_v4() },
        Box::new(adapter()),
    ).connect_with(io);
    handle.wait_for_spawn(Duration::from_secs(10)).await.unwrap();
    next_abilities(&mut events).await;

    handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Creative }).unwrap();
    assert_eq!(next_abilities(&mut events).await, (true, false, true, true));
    handle.send_action(ClientAction::SetFlying { flying: true }).unwrap();
    handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Creative }).unwrap();
    assert_eq!(next_abilities(&mut events).await, (true, true, true, true), "creative preserves reported flight");
    handle.send_action(ClientAction::SetFlying { flying: false }).unwrap();
    handle.command("gamemode creative").unwrap();
    assert_eq!(next_abilities(&mut events).await, (true, false, true, true));
    handle.send_action(ClientAction::SetFlying { flying: true }).unwrap();
    handle.command("gamemode creative").unwrap();
    assert_eq!(next_abilities(&mut events).await, (true, true, true, true), "command path also preserves flight");
    handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Survival }).unwrap();
    assert_eq!(next_abilities(&mut events).await, (false, false, false, false));
    handle.send_action(ClientAction::SetFlying { flying: true }).unwrap();
    handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Creative }).unwrap();
    assert_eq!(next_abilities(&mut events).await, (true, false, true, true), "survival cannot pre-arm flight");
    handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Spectator }).unwrap();
    assert_eq!(next_abilities(&mut events).await, (true, true, true, false));
    handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Creative }).unwrap();
    assert_eq!(next_abilities(&mut events).await, (true, true, true, true));

    handle.send_action(ClientAction::PlayerLoaded).unwrap();
    let rotation = lodestone_model::Rotation::new(0.0, 0.0);
    let top = lodestone_model::Vec3::new(8.5, 30.0, 8.5);
    let bottom = lodestone_model::Vec3::new(8.5, 19.25, 8.5);
    handle.move_to(top, rotation, false, false).unwrap();
    handle.move_to(bottom, rotation, false, false).unwrap();
    handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Survival }).unwrap();
    next_abilities(&mut events).await;
    handle.move_to(bottom, rotation, true, false).unwrap();
    handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Survival }).unwrap();
    next_abilities(&mut events).await;
    assert_eq!(handle.health(), Some(20.0), "flight descent must not become a later survival fall");

    // Control: the same 10.75-block descent without flight costs floor(10.75 + 1e-6 - 3) = 7 health.
    handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Creative }).unwrap();
    next_abilities(&mut events).await;
    handle.move_to(top, rotation, false, false).unwrap();
    handle.send_action(ClientAction::SetFlying { flying: true }).unwrap();
    handle.send_action(ClientAction::SetFlying { flying: false }).unwrap();
    handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Survival }).unwrap();
    next_abilities(&mut events).await;
    handle.move_to(bottom, rotation, true, false).unwrap();
    handle.send_action(ClientAction::ChangeGameMode { mode: GameMode::Survival }).unwrap();
    next_abilities(&mut events).await;
    assert_eq!(handle.health(), Some(13.0), "ordinary fall damage control must execute");
    handle.shutdown();
    server.shutdown().await;
}
