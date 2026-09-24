//! Live capture of **every** Configuration-phase `registry_data` payload plus
//! `update_tags`, against a real vanilla 26.2 server.
//!
//! `tests/live_registry_data.rs` already proved (in its own module docs) that
//! a real server sends exactly the 29 registries named in
//! `RegistryDataLoader::SYNCHRONIZED_REGISTRIES`, and persisted fixtures for
//! two of them (`dimension_type`, `world_clock`) that this crate's own decoder
//! already parses into typed data. This file is the sibling that persists a
//! fixture for **every** synchronized registry, plus `update_tags`, so
//! `lodestone-server`'s `ServerProtocol::encode_registry_data` can replay real
//! vanilla bytes for the ones this crate has no typed model for (`biome`,
//! `enchantment`, `banner_pattern`, …) instead of sending nothing.
//!
//! Nothing here re-encodes a registry from our own understanding of its
//! contents: every fixture is the *whole packet payload* a real server wrote,
//! captured verbatim, so a real client reads bytes it already knows how to
//! parse — `decode(encode(x)) == x` is worthless for exactly this reason
//! (`CLAUDE.md`, evidence standards).
//!
//! Run with:
//!
//! ```text
//! ./scripts/live-oracles/creative.sh
//! cargo test -p lodestone-v26-2 --features live-registry --test live_registry_data_full_set \
//!     -- --ignored --nocapture
//! ```
//!
//! Set `LODESTONE_CAPTURE_FIXTURES=1` to rewrite the fixtures from this run.

#![cfg(feature = "live-registry")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_model::{ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter};
use lodestone_net::Connection;
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::configuration;
use lodestone_v26_2::packets::registry::RegistryData;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

#[path = "../common/mod.rs"]
mod common;
use common::unique_username;

const SERVER_ADDR: &str = "127.0.0.1:25570";

const REPAIR: &str = "recreate the creative oracle with: ./scripts/live-oracles/creative.sh \
    (expected a vanilla 26.2 flat creative server on 127.0.0.1:25570)";

const CTX: Ctx = Ctx { version: 776 };

/// The 29 entries of vanilla's own registry-data loader's own
/// synchronized-registries list,
/// read directly off the decompiled source rather than
/// `generated/reports/registries.json` — that file is authoritative about
/// registry *contents*, not about which registries are synchronized to a
/// client, and it omits `dimension_type` and `world_clock` entirely (both are
/// data-pack-loaded registries, so `registries.json` reports them absent).
/// This is a floor this test asserts against, so a data-pack-added registry
/// on a differently configured server would not fail it — only a missing one
/// would.
const SYNCHRONIZED_REGISTRIES: &[&str] = &[
    "minecraft:worldgen/biome",
    "minecraft:chat_type",
    "minecraft:trim_pattern",
    "minecraft:trim_material",
    "minecraft:wolf_variant",
    "minecraft:wolf_sound_variant",
    "minecraft:pig_variant",
    "minecraft:pig_sound_variant",
    "minecraft:frog_variant",
    "minecraft:cat_variant",
    "minecraft:cat_sound_variant",
    "minecraft:cow_sound_variant",
    "minecraft:cow_variant",
    "minecraft:chicken_sound_variant",
    "minecraft:chicken_variant",
    "minecraft:zombie_nautilus_variant",
    "minecraft:painting_variant",
    "minecraft:sulfur_cube_archetype",
    "minecraft:dimension_type",
    "minecraft:damage_type",
    "minecraft:banner_pattern",
    "minecraft:enchantment",
    "minecraft:jukebox_song",
    "minecraft:instrument",
    "minecraft:test_environment",
    "minecraft:test_instance",
    "minecraft:dialog",
    "minecraft:world_clock",
    "minecraft:timeline",
];

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

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

/// The short filename stem for a registry's fixture: the identifier's path
/// segment(s) after `minecraft:`, with `/` turned into `_` so
/// `minecraft:worldgen/biome` becomes `worldgen_biome` rather than colliding
/// with a directory separator.
fn fixture_stem(registry: &str) -> String {
    registry
        .strip_prefix("minecraft:")
        .unwrap_or(registry)
        .replace('/', "_")
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

/// Walks one `update_tags` payload just far enough to prove it consumed to
/// exactly the end — the same "zero trailing bytes" check
/// `tests/registry_data.rs` runs for `registry_data`, hand-rolled here because
/// the adapter's own `decode_update_tags` is private to
/// `crate::adapter::connection`. Wire shape from that function's own doc
/// comment: `VarInt registry_count` then, per registry, a string key, a
/// `VarInt tag_count`, then per tag a string name and a `VarInt`-prefixed list
/// of `VarInt` element ids.
fn assert_update_tags_decodes_fully(payload: &[u8]) {
    let mut r = Reader::new(payload);
    let registry_count = r.var_i32().expect("registry count");
    for _ in 0..registry_count {
        let _registry_key = r.string(32767).expect("registry key");
        let tag_count = r.var_i32().expect("tag count");
        for _ in 0..tag_count {
            let _tag_name = r.string(32767).expect("tag name");
            let id_count = r.var_i32().expect("id count");
            for _ in 0..id_count {
                r.var_i32().expect("element id");
            }
        }
    }
    r.ensure_empty()
        .expect("update_tags payload must decode with zero trailing bytes");
}

#[tokio::test]
#[ignore = "requires the flat creative 26.2 oracle on 127.0.0.1:25570"]
async fn every_synchronized_registry_plus_update_tags_round_trips_from_a_real_server() {
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

    let mut captured_registries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut captured_update_tags: Option<Vec<u8>> = None;
    let overall = Duration::from_secs(60);

    let outcome = tokio::time::timeout(overall, async {
        loop {
            let (packet_id, payload) = match conn.read_packet().await {
                Ok(Some(p)) => p,
                Ok(None) => return false,
                Err(err) => panic!("read error: {err}"),
            };

            if state == ConnectionState::Configuration {
                if packet_id == configuration::clientbound::REGISTRY_DATA {
                    let mut reader = Reader::new(&payload);
                    let data = RegistryData::decode(&mut reader, CTX)
                        .expect("a real registry_data payload must decode");
                    reader
                        .ensure_empty()
                        .unwrap_or_else(|err| panic!("{} left {err}", data.registry));
                    captured_registries.insert(data.registry.clone(), payload.clone());
                } else if packet_id == configuration::clientbound::UPDATE_TAGS {
                    assert_update_tags_decodes_fully(&payload);
                    captured_update_tags = Some(payload.clone());
                }
            }

            let directives = match adapter.handle_packet(&mut World::new(), state, packet_id, &payload)
            {
                Ok(directives) => directives,
                Err(err) => panic!("adapter rejected packet {packet_id} in {state:?}: {err}"),
            };
            let mut reached_play = false;
            for directive in directives {
                if matches!(directive, Directive::SetState(ConnectionState::Play)) {
                    reached_play = true;
                }
                apply(&mut conn, &mut state, directive).await;
            }
            if reached_play {
                return true;
            }
        }
    })
    .await;

    assert_eq!(
        outcome,
        Ok(true),
        "never reached Play within {overall:?} (captured so far: {:?})",
        captured_registries.keys().collect::<Vec<_>>()
    );

    println!(
        "captured {} registries, update_tags = {}",
        captured_registries.len(),
        captured_update_tags.is_some()
    );

    // --- The set itself: every synchronized registry must have arrived ----
    let mut missing = Vec::new();
    for &name in SYNCHRONIZED_REGISTRIES {
        if !captured_registries.contains_key(name) {
            missing.push(name);
        }
    }
    assert!(
        missing.is_empty(),
        "missing registries from a real server's Configuration phase: {missing:?} \
         (captured: {:?})",
        captured_registries.keys().collect::<Vec<_>>()
    );
    assert!(
        captured_update_tags.is_some(),
        "a real server must send update_tags during Configuration"
    );

    // --- Persist one fixture per registry, plus update_tags ---------------
    //
    // `minecraft:dimension_type` and `minecraft:world_clock` are deliberately
    // skipped here: `tests/live_registry_data.rs` already owns
    // `registry_data_dimension_type.hex` / `registry_data_world_clock.hex`
    // (captured the same way, from the same oracle) and `tests/registry_data.rs`
    // asserts specific decoded fields against them. Re-capturing here too would
    // overwrite those files with a byte-for-byte *different but equally valid*
    // capture — vanilla's own NBT compound key order is not stable across server
    // runs (measured: `overworld`'s `visual` compound's child key order differed
    // between two real captures of the same registry) — which is harmless to any
    // consumer that reads NBT as a keyed map, but would still be a needless
    // foreign-file rewrite of a fixture this file does not own.
    for (registry, bytes) in &captured_registries {
        if registry == "minecraft:dimension_type" || registry == "minecraft:world_clock" {
            continue;
        }
        let stem = fixture_stem(registry);
        record_fixture(
            &format!("registry_data_{stem}.hex"),
            &format!(
                "Raw clientbound `registry_data` payload for `{registry}`,\n\
                 captured from a real vanilla 26.2 server (flat creative oracle, :25570)\n\
                 during Configuration. Payload only — no packet-length or packet-id prefix.\n\
                 \n\
                 Recapture: cargo test -p lodestone-v26-2 --features live-registry \\\n\
                 --test live_registry_data_full_set -- --ignored --nocapture\n\
                 (with LODESTONE_CAPTURE_FIXTURES=1 to overwrite)"
            ),
            bytes,
        );
    }
    record_fixture(
        "update_tags_configuration.hex",
        "Raw clientbound `update_tags` payload, captured from a real vanilla 26.2\n\
         server (flat creative oracle, :25570) during Configuration. Payload only —\n\
         no packet-length or packet-id prefix.\n\
         \n\
         Recapture: cargo test -p lodestone-v26-2 --features live-registry \\\n\
         --test live_registry_data_full_set -- --ignored --nocapture\n\
         (with LODESTONE_CAPTURE_FIXTURES=1 to overwrite)",
        captured_update_tags.as_deref().unwrap_or_default(),
    );
}
