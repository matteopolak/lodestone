//! Native policy delegation through the server's existing permission consumer.

use std::sync::Arc;

use lodestone_server::access::PermissionLevelContext;
use lodestone_server::{AccessHandle, AccessLists};
use uuid::Uuid;

#[test]
fn provider_overrides_and_abstains_without_changing_stored_ops() {
    let player = Uuid::from_u128(7);
    let stranger = Uuid::from_u128(9);
    let mut lists = AccessLists::new();
    lists.op(player, "player", 3);
    let access = AccessHandle::new(lists);
    access.set_permission_provider(Some(Arc::new(move |context: PermissionLevelContext| {
        if context.uuid == player {
            assert_eq!(context.fallback_level, 3);
            Some(1)
        } else { None }
    })));
    assert_eq!(access.command_permission_level(player), 1);
    assert_eq!(access.command_permission_level(stranger), 0);
    assert_eq!(access.permission_level(player), 3);
    access.clone().set_permission_provider(None);
    assert_eq!(access.command_permission_level(player), 3);
}

#[test]
fn provider_may_read_access_lists_and_remove_itself_without_nested_locks() {
    let access = AccessHandle::default();
    let reader = access.clone();
    access.set_permission_provider(Some(Arc::new(move |context: PermissionLevelContext| {
        assert_eq!(reader.permission_level(context.uuid), 0);
        reader.set_permission_provider(None);
        Some(2)
    })));
    assert_eq!(access.command_permission_level(Uuid::nil()), 2);
    assert_eq!(access.command_permission_level(Uuid::nil()), 4);
}

#[test]
fn invalid_provider_level_fails_closed() {
    let access = AccessHandle::default();
    access.set_permission_provider(Some(Arc::new(|_: PermissionLevelContext| Some(255))));
    assert_eq!(access.command_permission_level(Uuid::nil()), 0);
}

#[tokio::test]
async fn provider_changes_real_command_acceptance_without_op_file_edits() {
    use std::time::Duration;
    use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
    use lodestone_model::{ClientEvent, GameMode};
    use lodestone_net::{Connection, memory_pair};
    use lodestone_server::{BlockEntityHandle, NoEntities, WorldgenChunkSource};
    use lodestone_server::world_state::WorldStateHandle;
    use lodestone_v26_2::{V770ServerProtocol, adapter};

    async fn run(override_level: Option<u8>) -> bool {
        let player = Uuid::from_u128(71);
        let access = AccessHandle::default();
        access.set_owner(Some(Uuid::from_u128(72)));
        if let Some(level) = override_level {
            access.set_permission_provider(Some(Arc::new(move |context: PermissionLevelContext| {
                assert_eq!(context.uuid, player, "provider must see the logged-in identity");
                assert_eq!(context.fallback_level, 0);
                Some(level)
            })));
        }
        let (client_io, server_io) = memory_pair();
        let server = tokio::spawn(async move {
            let source = WorldgenChunkSource::new(
                lodestone_worldgen::density::Density::YClampedGradient {
                    from_y: -64.0, to_y: 64.0, from_value: 1.0, to_value: -1.0,
                }, -64, 384,
            );
            lodestone_server::serve_connection_with_access_and_state(
                &mut Connection::new(server_io), &V770ServerProtocol, &source, &NoEntities, 0,
                &access, &WorldStateHandle::new(), &BlockEntityHandle::default(), None,
            ).await
        });
        let (mut handle, mut events) = ClientBuilder::new(
            ServerAddress { host: "memory".into(), port: 0 },
            LoginProfile { username: "PolicyPlayer".into(), uuid: player },
            Box::new(adapter()),
        ).connect_with(client_io);
        handle.wait_for_spawn(Duration::from_secs(15)).await.expect("client spawn");
        handle.command("gamemode creative").expect("send real command");
        let accepted = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match events.recv().await.expect("client event stream") {
                    ClientEvent::GameModeChanged { game_mode: GameMode::Creative } => break true,
                    ClientEvent::Chat { text, .. } if text.to_plain_string().contains("permission") => break false,
                    _ => {}
                }
            }
        }).await.expect("command must visibly succeed or refuse");
        handle.shutdown();
        server.abort();
        let _ = server.await;
        accepted
    }

    assert!(!run(None).await, "configured non-op control must refuse");
    assert!(run(Some(2)).await, "provider grant must reach the command executor");
    assert!(!run(Some(1)).await, "provider below command level must refuse");
}
