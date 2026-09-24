//! The "Customize Type" screen's collected choice must reach the terrain a
//! freshly created world actually serves, not merely a file on disk nobody
//! reads back.
//!
//! # What this closes
//!
//! `saves.rs`'s own `customizing_a_flat_world_writes_the_layers_and_a_real_seed_at_creation_time`
//! proves the write half: a chosen Flat layer stack lands in
//! `world_gen_settings.dat`. `lodestone-server`'s
//! `overworld_chunk_source_override_builds_the_customized_flat_world_from_disk`
//! proves the read half in isolation: given that exact file, the right
//! `ChunkSource` comes back. Neither proves the **shell's own session path**
//! ever calls the read half — that is `net.rs`'s job, and a test that only
//! exercises the two halves separately cannot see a call that was never
//! made. This drives the real [`NetClient::open_singleplayer`] entry point
//! (the same one `app::launch_singleplayer` calls) with a world directory
//! this test wrote a Flat override into, and reads the served terrain back
//! over the wire.
//!
//! # Why the world type is deliberately wrong
//!
//! `world_type` is passed as [`WorldTypePreset::Normal`] — the opposite of
//! what created this world — and the requested seed does not match the one
//! written to disk either. `net.rs`'s own doc for this path says a saved
//! world's stored generator wins over both; if the override were not
//! actually read back, this session would serve ordinary Normal terrain at
//! the mismatched seed instead of the flat void asserted below, so a
//! passing run rules out "the world type argument happened to already
//! match" as an explanation.
//!
//! # Why a single bedrock layer plus air, not vanilla's own default flat
//!
//! Vanilla's bundled default flat preset (bottom-to-top grass/dirt/stone)
//! looks enough like shallow Normal terrain that a bug regenerating the
//! bundled default instead of the stored override could accidentally pass a
//! weak assertion. One bedrock layer with nothing above it out to the
//! overworld ceiling has no natural-terrain lookalike: a real Normal or
//! Amplified column is never almost entirely air from one block above
//! bedrock to the sky.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use lodestone::menu::create_world::WorldTypePreset;
use lodestone::net::{NetClient, NetUpdate};
use lodestone::saves::{create_world_in, GeneratorOverride};
use lodestone_client::{BlockPos, ChunkPos};

const SPAWN_X: i32 = 8;
const SPAWN_Z: i32 = 8;

/// Comfortably above the overworld ceiling, so a read here is air in every
/// world — used to learn the wire id of air without a registry.
const DEFINITELY_AIR_Y: i32 = 310;

/// The stored `world_gen_settings.dat` seed. The session below asks for a
/// **different** one on purpose — see the module doc.
const STORED_SEED: &str = "4242";
const REQUESTED_SEED: i64 = 1;

/// Tiny: nine columns is enough to prove the spawn column's own shape.
const VIEW_RADIUS: i32 = 1;

const DEADLINE: Duration = Duration::from_secs(240);

fn temp_root() -> PathBuf {
    let dir = std::env::temp_dir().join("lodestone-customize-type-reaches-the-generator");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch root");
    dir
}

fn pump_until(net: &NetClient, what: &str, mut ready: impl FnMut(&NetClient) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    let mut errors: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        for update in net.poll() {
            match update {
                NetUpdate::Error(e) => errors.push(e),
                NetUpdate::Disconnected(reason) => errors.push(format!("disconnected: {reason:?}")),
                _ => {}
            }
        }
        if ready(net) {
            assert!(errors.is_empty(), "reached `{what}` but the session reported errors: {errors:?}");
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for `{what}`; errors: {errors:?}");
}

#[test]
fn a_flat_customize_choice_survives_into_the_served_singleplayer_world() {
    let root = temp_root();
    let generator = GeneratorOverride::Flat {
        layers: vec![("minecraft:bedrock".to_string(), 1), ("minecraft:air".to_string(), 59)],
        biome: "minecraft:plains".to_string(),
        features: false,
        lakes: false,
    };
    let world_dir = create_world_in(&root, "Customized", 0, &[], Some((&generator, STORED_SEED)))
        .expect("creates the world directory and writes world_gen_settings.dat");

    let protocol = lodestone::Config::default().protocol;
    let Some(server_protocol) = lodestone_registry::server_protocol_for_protocol(protocol) else {
        assert!(!cfg!(feature = "live"), "the default build must host singleplayer");
        return;
    };
    // `world_type: Normal` and `REQUESTED_SEED` both disagree with what is on
    // disk — see the module doc on why that is the point.
    let net = NetClient::open_singleplayer(
        server_protocol,
        protocol,
        REQUESTED_SEED,
        WorldTypePreset::Normal,
        VIEW_RADIUS,
        None,
        Some(world_dir),
    );

    pump_until(&net, "the spawn chunk", |net| net.is_chunk_loaded(ChunkPos { x: 0, z: 0 }));

    let air = net
        .block_at(BlockPos::new(SPAWN_X, DEFINITELY_AIR_Y, SPAWN_Z))
        .expect("a loaded chunk must answer for a y inside the world");

    let bottom = net
        .block_at(BlockPos::new(SPAWN_X, -64, SPAWN_Z))
        .expect("the overworld floor must answer");
    assert_ne!(
        bottom, air,
        "the stored Flat override's single bedrock layer must reach the served world, \
         not the bundled default generator `world_type: Normal` would otherwise pick"
    );

    // Above the one-layer stack: vanilla's own flat generator leaves
    // everything past its configured layers as air, all the way to the
    // ceiling. A real Normal or Amplified column is never this shape.
    for y in [-40, -10, 0, 60, 150] {
        let block = net
            .block_at(BlockPos::new(SPAWN_X, y, SPAWN_Z))
            .unwrap_or_else(|| panic!("y={y} must be inside the loaded chunk's answered range"));
        assert_eq!(
            block, air,
            "y={y} must be air for the stored Flat override to have reached the served \
             world — a Normal-terrain fallback would put solid blocks here instead"
        );
    }
}
