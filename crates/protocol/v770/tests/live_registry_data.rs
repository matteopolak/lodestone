//! Live `registry_data` capture and gate (issue #288).
//!
//! Joins the flat creative 26.2 oracle (game :25570) and captures the **raw
//! `registry_data` payloads the server itself authored** for
//! `minecraft:dimension_type` and `minecraft:world_clock`. Those bytes are
//! written to `tests/fixtures/` and replayed by the hermetic sibling
//! `tests/registry_data.rs`, so the decoder is never validated against bytes our
//! own encoder produced — `decode(encode(x)) == x` is satisfied by two symmetric
//! misunderstandings (`CLAUDE.md`, evidence standards).
//!
//! Run with:
//!
//! ```text
//! ./scripts/live-oracles/creative.sh
//! cargo test -p lodestone-v770 --features live-registry --test live_registry_data \
//!     -- --ignored --nocapture
//! ```
//!
//! Set `LODESTONE_CAPTURE_FIXTURES=1` to rewrite the fixtures from this run.
//!
//! # What this gate asserts beyond "it decoded"
//!
//! The decoded *content* is checked against Mojang's own data files at
//! `.cache/mc/26.2/client-src/data/minecraft/dimension_type/*.json`, which are
//! the authoritative source for these two registries and are independent of both
//! our decoder and our encoder.
//!
//! Note `generated/reports/registries.json` — which issue #288 names as the
//! cross-check — **does not contain either registry**: both are data-pack
//! registries loaded from JSON, so `registries.json` reports them as absent.
//! The `client-src/data/.../dimension_type/*.json` files are the right oracle.

#![cfg(feature = "live-registry")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::configuration;
use lodestone_v770::packets::registry::{ClientRegistries, DimensionType, RegistryData};
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

mod common;
use common::unique_username;

const SERVER_ADDR: &str = "127.0.0.1:25570";

const REPAIR: &str = "recreate the creative oracle with: ./scripts/live-oracles/creative.sh \
    (expected a vanilla 26.2 flat creative server on 127.0.0.1:25570)";

const CTX: Ctx = Ctx { version: 776 };

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Renders a captured payload as reviewable hex text: a provenance header, then
/// 16 bytes per line. Same format as the `tool_component_*` fixtures.
fn to_hex_fixture(header: &str, bytes: &[u8]) -> String {
    let mut out = String::new();
    for line in header.lines() {
        if line.is_empty() {
            out.push_str("#\n");
        } else {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }
    for chunk in bytes.chunks(16) {
        let row: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        out.push_str(&row.join(" "));
        out.push('\n');
    }
    out
}

fn from_hex_fixture(text: &str) -> Vec<u8> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|tok| u8::from_str_radix(tok, 16).expect("fixture hex byte"))
        .collect()
}

/// Writes (or asserts) a fixture. A mismatch is a hard failure naming the
/// re-capture command, so a server-side wire change surfaces here rather than as
/// a hermetic test that keeps passing against stale bytes.
fn record_fixture(name: &str, header: &str, bytes: &[u8]) {
    let path = fixture_path(name);
    let rendered = to_hex_fixture(header, bytes);
    if std::env::var_os("LODESTONE_CAPTURE_FIXTURES").is_some() || !path.exists() {
        std::fs::write(&path, &rendered).expect("write fixture");
        println!("wrote {} ({} bytes captured)", path.display(), bytes.len());
        return;
    }
    let existing = std::fs::read_to_string(&path).expect("read fixture");
    assert_eq!(
        from_hex_fixture(&existing),
        bytes,
        "captured {name} no longer matches the checked-in fixture — re-capture with \
         LODESTONE_CAPTURE_FIXTURES=1 and review the diff"
    );
}

async fn apply(
    conn: &mut Connection<TcpStream>,
    state: &mut ConnectionState,
    directive: Directive,
) {
    match directive {
        Directive::Send { packet_id, payload } => {
            conn.write_packet(packet_id, &payload)
                .await
                .expect("write packet");
        }
        Directive::SetState(next) => *state = next,
        Directive::SetCompression(threshold) => conn.set_compression(threshold),
        Directive::Disconnect(reason) => {
            panic!("server disconnected us: {}", reason.to_plain_string())
        }
        _ => {}
    }
}

/// The expected field values, read out of Mojang's own dimension-type JSON.
///
/// Deliberately parsed from the shipped data files rather than transcribed into
/// this file: a transcription is a second copy of the thing under test, and this
/// gate's whole point is that the expected values come from outside our tree.
fn mojang_dimension_type(name: &str) -> BTreeMap<String, serde_json::Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(".cache/mc/26.2/client-src/data/minecraft/dimension_type")
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "could not read Mojang's own {} ({err}) — this gate's expected values come from the \
             decompiled jar's data files, so the 26.2 cache must be present",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("Mojang dimension_type json parses")
}

fn assert_matches_mojang(name: &str, decoded: &DimensionType) {
    let expected = mojang_dimension_type(name);
    let get = |key: &str| expected.get(key).cloned();
    let bool_at = |key: &str, default: bool| {
        get(key).map_or(default, |v| v.as_bool().expect("bool field"))
    };
    assert_eq!(
        decoded.has_skylight,
        bool_at("has_skylight", false),
        "{name}: has_skylight"
    );
    assert_eq!(
        decoded.has_ceiling,
        bool_at("has_ceiling", false),
        "{name}: has_ceiling"
    );
    assert_eq!(
        decoded.has_fixed_time,
        bool_at("has_fixed_time", false),
        "{name}: has_fixed_time (absent means false)"
    );
    assert_eq!(
        decoded.has_ender_dragon_fight,
        bool_at("has_ender_dragon_fight", false),
        "{name}: has_ender_dragon_fight"
    );
    assert!(
        (decoded.coordinate_scale
            - get("coordinate_scale")
                .expect("coordinate_scale")
                .as_f64()
                .expect("number"))
        .abs()
            < 1e-9,
        "{name}: coordinate_scale {} != Mojang's {:?}",
        decoded.coordinate_scale,
        get("coordinate_scale")
    );
    assert_eq!(
        i64::from(decoded.min_y),
        get("min_y").expect("min_y").as_i64().expect("int"),
        "{name}: min_y"
    );
    assert_eq!(
        i64::from(decoded.height),
        get("height").expect("height").as_i64().expect("int"),
        "{name}: height"
    );
    assert_eq!(
        i64::from(decoded.logical_height),
        get("logical_height")
            .expect("logical_height")
            .as_i64()
            .expect("int"),
        "{name}: logical_height"
    );
    let expected_ambient = get("ambient_light")
        .expect("ambient_light")
        .as_f64()
        .expect("number");
    assert!(
        (f64::from(decoded.ambient_light) - expected_ambient).abs() < 1e-6,
        "{name}: ambient_light {} != Mojang's {expected_ambient}",
        decoded.ambient_light
    );
    assert_eq!(
        decoded.default_clock.as_deref(),
        get("default_clock").as_ref().and_then(|v| v.as_str()),
        "{name}: default_clock (absent in the Nether — it has fixed time)"
    );
}

#[tokio::test]
#[ignore = "requires the flat creative 26.2 oracle on 127.0.0.1:25570"]
async fn registry_data_from_a_real_server_decodes_and_matches_mojangs_own_data() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25570,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V770Adapter::new();

    let mut conn = match Connection::connect(SERVER_ADDR).await {
        Ok(conn) => conn,
        Err(err) => panic!("could not reach {SERVER_ADDR}: {err}. {REPAIR}"),
    };
    let mut state = ConnectionState::Handshaking;
    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    let mut captured: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut registries = ClientRegistries::default();
    let mut login_dimension_type: Option<i32> = None;
    let overall = Duration::from_secs(60);

    let outcome = tokio::time::timeout(overall, async {
        loop {
            let (packet_id, payload) = match conn.read_packet().await {
                Ok(Some(p)) => p,
                Ok(None) => return false,
                Err(err) => panic!("read error: {err}"),
            };

            if state == ConnectionState::Configuration
                && packet_id == configuration::clientbound::REGISTRY_DATA
            {
                // Decode the *raw* payload here, independently of the adapter,
                // so the captured bytes and the assertion below cannot both be
                // wrong in the same way the adapter's fold is.
                let mut reader = Reader::new(&payload);
                let data = RegistryData::decode(&mut reader, CTX)
                    .expect("a real registry_data payload must decode");
                reader
                    .ensure_empty()
                    .unwrap_or_else(|err| panic!("{} left {err}", data.registry));
                captured.insert(data.registry.clone(), payload.clone());
                registries.apply(data);
            }

            let directives = match adapter.handle_packet(&mut World::new(), state, packet_id, &payload)
            {
                Ok(directives) => directives,
                Err(err) => panic!("adapter rejected packet {packet_id} in {state:?}: {err}"),
            };
            for directive in directives {
                if let Directive::Emit(ClientEvent::DimensionTypeChanged { holder_id, .. }) =
                    &directive
                {
                    login_dimension_type = Some(*holder_id);
                }
                let reached_play = matches!(directive, Directive::SetState(ConnectionState::Play));
                apply(&mut conn, &mut state, directive).await;
                if reached_play && login_dimension_type.is_none() {
                    // Keep reading: `login` arrives a few packets into Play.
                    continue;
                }
            }
            if state == ConnectionState::Play && login_dimension_type.is_some() {
                return true;
            }
        }
    })
    .await;

    assert_eq!(
        outcome,
        Ok(true),
        "never reached Play with a resolved dimension type within {overall:?} \
         (captured registries: {:?})",
        captured.keys().collect::<Vec<_>>()
    );

    // --- The registry set itself ------------------------------------------
    //
    // 30 synchronized registries in 26.2 (`RegistryDataLoader::SYNCHRONIZED_REGISTRIES`).
    // Asserting a floor rather than the exact number: a data pack can add a
    // registry, and the count is not what this gate is about.
    println!("captured {} registries", captured.len());
    for (registry, bytes) in &captured {
        println!("  {registry}: {} bytes", bytes.len());
    }
    assert!(
        captured.contains_key("minecraft:dimension_type"),
        "the server must send minecraft:dimension_type during Configuration"
    );
    assert!(
        captured.contains_key("minecraft:world_clock"),
        "the server must send minecraft:world_clock during Configuration"
    );

    record_fixture(
        "registry_data_dimension_type.hex",
        "Raw clientbound `registry_data` payload for `minecraft:dimension_type`,\n\
         captured from a real vanilla 26.2 server (flat creative oracle, :25570)\n\
         during Configuration. Payload only — no packet-length or packet-id prefix.\n\
         \n\
         Recapture: cargo test -p lodestone-v770 --features live-registry \\\n\
         --test live_registry_data -- --ignored --nocapture\n\
         (with LODESTONE_CAPTURE_FIXTURES=1 to overwrite)",
        &captured["minecraft:dimension_type"],
    );
    record_fixture(
        "registry_data_world_clock.hex",
        "Raw clientbound `registry_data` payload for `minecraft:world_clock`,\n\
         captured from a real vanilla 26.2 server (flat creative oracle, :25570)\n\
         during Configuration. Payload only — no packet-length or packet-id prefix.\n\
         \n\
         The entries are unit compounds (`record WorldClock()`), so the ordered\n\
         *names* are the whole content: this is the id -> name map `set_time`'s\n\
         clock keys index into.\n\
         \n\
         Recapture: cargo test -p lodestone-v770 --features live-registry \\\n\
         --test live_registry_data -- --ignored --nocapture\n\
         (with LODESTONE_CAPTURE_FIXTURES=1 to overwrite)",
        &captured["minecraft:world_clock"],
    );

    // --- Content, against Mojang's own data files --------------------------
    for name in [
        "minecraft:overworld",
        "minecraft:overworld_caves",
        "minecraft:the_nether",
        "minecraft:the_end",
    ] {
        let decoded = registries
            .dimension_type_by_name(name)
            .unwrap_or_else(|| panic!("{name} must be present in the decoded registry"));
        assert_matches_mojang(name.strip_prefix("minecraft:").expect("namespaced"), decoded);
    }

    // --- The holder-id space, which is the whole point --------------------
    assert_eq!(
        registries.world_clock_id("minecraft:overworld"),
        Some(0),
        "vanilla registers the overworld clock first"
    );
    assert_eq!(
        registries.world_clock_id("minecraft:the_end"),
        Some(1),
        "…and the End clock second — which is exactly why `day_clock`'s \
         lowest-holder-id heuristic silently returned the overworld clock in the End"
    );

    let holder_id = login_dimension_type.expect("login must report a dimension type holder id");
    let (name, resolved) = registries
        .dimension_type(holder_id)
        .expect("login's dimension_type id must resolve against the decoded registry");
    println!("login reported dimension_type holder {holder_id} = {name}");
    assert_eq!(
        name, "minecraft:overworld",
        "the creative oracle spawns in the overworld"
    );
    assert_eq!(resolved.min_y, -64);
    assert_eq!(resolved.height, 384);
    assert!(resolved.has_skylight);
}

/// The control for the gate above: with **no** `registry_data` folded in, every
/// lookup the adapter depends on must report "unknown" rather than a
/// plausible-looking default. Without this, the assertions above could be
/// satisfied by a `ClientRegistries` that hardcoded the overworld.
#[test]
fn an_empty_registry_store_resolves_nothing() {
    let registries = ClientRegistries::default();
    assert!(registries.is_empty());
    assert!(registries.dimension_type(0).is_none());
    assert!(registries.dimension_type_by_name("minecraft:overworld").is_none());
    assert!(registries.world_clock_id("minecraft:overworld").is_none());
}
