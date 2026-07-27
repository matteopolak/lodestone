//! Live smoke tests for transport hardening. All are `#[ignore]`d: they need a
//! real server and/or network access and must never run in the hermetic suite.
//!
//! Run explicitly, e.g.:
//! `cargo test -p lodestone-net --test live_transport -- --ignored --nocapture`

use lodestone_net::{ServerListPing, lookup_minecraft_srv, resolve_server_address};

/// Modern Server List Ping against the local vanilla server on :25565.
///
/// Proves the real transport + codec path: a genuine vanilla server accepts our
/// handshake and status request and its JSON round-trips back through the codec.
#[tokio::test]
#[ignore = "requires the live vanilla server on 127.0.0.1:25565"]
async fn modern_ping_local_vanilla() {
    let status = ServerListPing::new(770)
        .status("127.0.0.1", Some(25565))
        .await
        .expect("status ping should succeed against the local server");
    println!("status json: {}", status.json);
    println!("latency: {:?} ms", status.latency_ms);
    assert!(
        status.json.contains("version"),
        "status JSON should carry a version object: {}",
        status.json
    );
    assert!(status.latency_ms.is_some());
}

/// SRV resolution end to end through the system resolver.
///
/// A name with no `_minecraft._tcp` record must resolve cleanly to `Ok(None)`
/// (the common case), proving the hickory integration builds a resolver from
/// system config and issues a query without erroring.
#[tokio::test]
#[ignore = "requires outbound DNS"]
async fn srv_lookup_missing_record_is_none() {
    let out = lookup_minecraft_srv("example.com")
        .await
        .expect("resolver should build and query");
    println!("example.com SRV: {out:?}");
    assert!(out.is_none(), "example.com has no minecraft SRV record");
}

/// `resolve_server_address` honors an explicit port with no DNS traffic.
#[tokio::test]
#[ignore = "requires outbound DNS for the SRV branch"]
async fn resolve_prefers_explicit_port() {
    let addr = resolve_server_address("example.com", Some(25599))
        .await
        .unwrap();
    assert_eq!(addr.host, "example.com");
    assert_eq!(addr.port, 25599);
}
