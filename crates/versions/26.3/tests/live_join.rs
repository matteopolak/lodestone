//! Live acceptance: a headless client joins a real 26.3 dedicated server.
//!
//! Ignored by default because it needs a running vanilla 26.3 server in
//! offline mode. Point it at one with `LODESTONE_26_3_SERVER=host:port`
//! (default `127.0.0.1:25580`) and run with `-- --ignored --nocapture`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use tracing_subscriber::Layer as _;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

/// Records every `ERROR` event, so a packet the driver drops and continues past
/// still fails the test.
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

fn server_address() -> ServerAddress {
    let raw = std::env::var("LODESTONE_26_3_SERVER").unwrap_or_else(|_| "127.0.0.1:25580".into());
    let (host, port) = raw.rsplit_once(':').expect("LODESTONE_26_3_SERVER is host:port");
    ServerAddress {
        host: host.into(),
        port: port.parse().expect("port is a number"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a live vanilla 26.3 server"]
async fn a_headless_client_joins_a_vanilla_26_3_server_and_streams_terrain() {
    let errors = ErrorLog::default();
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_test_writer()
                .with_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "warn".into()),
                ),
        )
        .with(errors.clone())
        .try_init();
    let adapter = lodestone_v26_3::adapter();
    let profile = LoginProfile {
        username: "LodestoneProbe".into(),
        uuid: uuid::Uuid::new_v4(),
    };
    let (handle, mut events) = ClientBuilder::new(server_address(), profile, Box::new(adapter))
        .connect()
        .await
        .expect("connect to the 26.3 server");
    let drain = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(event) = events.recv().await {
            let text = format!("{event:?}");
            if text.starts_with("Disconnect") || text.starts_with("SessionFailed") {
                seen.push(text);
            }
        }
        seen
    });
    handle
        .wait_for_spawn(Duration::from_secs(60))
        .await
        .expect("the client never spawned on the 26.3 server");
    // Past the server's first keep-alive (sent every 15 s), so an unanswered
    // one would have disconnected the session.
    tokio::time::sleep(Duration::from_secs(40)).await;
    let chunks = handle.loaded_chunk_count();
    let position = handle.position();
    eprintln!("26.3 live join: position={position:?} loaded_chunks={chunks}");
    drop(handle);
    let failures = tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    assert!(failures.is_empty(), "the session ended badly: {failures:#?}");
    let errors = errors.0.lock().unwrap().clone();
    assert!(errors.is_empty(), "the client logged {} errors: {errors:#?}", errors.len());
    assert!(chunks >= 25, "only {chunks} chunks loaded after 40 s in play");
}
