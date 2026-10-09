//! A 26.3 client joins the integrated server hosting protocol 777.
//!
//! The client adapter is the independently written 26.3 decoder, so every
//! packet the server frames with a 26.2 layout or id fails here as a decode
//! error rather than passing a symmetric round trip.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ChunkPos, ClientBuilder, LoginProfile, ServerAddress};
use lodestone_server::{IntegratedServer, StoneFloorSource};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Clone, Default)]
struct ErrorLog(Arc<Mutex<Vec<String>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ErrorLog {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        if *event.metadata().level() != tracing::Level::ERROR {
            return;
        }
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push_str(&format!("{}={value:?} ", field.name()));
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }
}

fn cheap_source() -> StoneFloorSource {
    StoneFloorSource::new(-64, 384, 0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_26_3_client_joins_the_integrated_server_and_receives_terrain() {
    let errors = ErrorLog::default();
    let _ = tracing_subscriber::registry().with(errors.clone()).try_init();

    let (server, client_io) =
        IntegratedServer::open_in_memory(lodestone_v26_3::server_protocol(), cheap_source(), 0);
    let profile = LoginProfile { username: "Hosted263".into(), uuid: uuid::Uuid::new_v4() };
    let address = ServerAddress { host: "memory".into(), port: 0 };
    let (handle, _events) =
        ClientBuilder::new(address, profile, Box::new(lodestone_v26_3::adapter()))
            .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("the 26.3 client never spawned on the hosted server");
    handle
        .wait_for_chunks(1, Duration::from_secs(60))
        .await
        .expect("the initial column never arrived");
    assert!(handle.is_chunk_loaded(ChunkPos::new(0, 0)));
    assert!(!handle.is_finished(), "the session ended during the join");
    let errors = errors.0.lock().unwrap().clone();
    assert!(errors.is_empty(), "the client logged errors: {errors:#?}");
    server.shutdown().await;
}
