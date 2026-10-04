//! Control for `hosted_join.rs`: the built-in 26.2 framing must not satisfy a
//! 26.3 client. A separate binary so its decode errors stay out of the other
//! test's process-wide error log.

use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_server::{IntegratedServer, WorldgenChunkSource};
use lodestone_worldgen::density::Density;

fn cheap_source() -> WorldgenChunkSource {
    let density = Density::YClampedGradient {
        from_y: -64.0,
        to_y: 64.0,
        from_value: 1.0,
        to_value: -1.0,
    };
    WorldgenChunkSource::new(density, -64, 384)
}

/// Without this the join test would pass against any server a client tolerates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_26_2_framing_does_not_satisfy_a_26_3_client() {
    let (server, client_io) =
        IntegratedServer::open_in_memory(lodestone_v26_2::V770ServerProtocol, cheap_source(), 0);
    let profile = LoginProfile { username: "Control263".into(), uuid: uuid::Uuid::new_v4() };
    let address = ServerAddress { host: "memory".into(), port: 0 };
    let (handle, _events) =
        ClientBuilder::new(address, profile, Box::new(lodestone_v26_3::adapter()))
            .connect_with(client_io);
    let joined = async {
        handle.wait_for_spawn(Duration::from_secs(10)).await?;
        handle.wait_for_chunks(1, Duration::from_secs(10)).await
    }
    .await;
    assert!(joined.is_err(), "a 26.2-framed server joined a 26.3 client");
    server.shutdown().await;
}
