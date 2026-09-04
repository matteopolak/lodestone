//! A singleplayer world must arrive with **non-air blocks in it**, at the
//! render distance a real player actually has configured.
//!
//! # Why this exists next to `singleplayer_persistence.rs`
//!
//! That file already drives the real [`NetClient::open_singleplayer`] path and
//! reads blocks back over the wire — but every gate in it runs at
//! `view_radius` 1 or 2, because a composed column is expensive to generate.
//! A whole class of defect lives only at a *large* radius: the join has to
//! enumerate, generate and encode `(2r + 1)²` columns, which is nine columns
//! at radius 1 and **4,489** at the shipped-and-configurable maximum. A gate
//! whose whole corpus shares one small radius cannot see anything that only
//! goes wrong when that number is large — a keep-alive that expires inside one
//! unserviced generation window, a store capacity derived from the radius, a
//! ring enumeration that overflows.
//!
//! So the discriminating input here is **the radius itself**, and the assertion
//! is deliberately the weakest useful one: *some* non-air block reaches the
//! client's own world. "Chunk packets were sent" passes with a square of air;
//! this does not.
//!
//! # What it does not assert
//!
//! Nothing about *how many* columns arrive. At a large radius the outer rings
//! are still streaming long after the player is in the world, and asserting a
//! full square would make this a multi-minute test measuring generator
//! throughput rather than correctness.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use lodestone::net::{NetClient, NetUpdate};
use lodestone_client::{BlockPos, ChunkPos};

/// `V770ServerProtocol::begin_play`'s hardcoded spawn column, which is why
/// chunk `(0, 0)` is the one always streamed first.
const SPAWN_X: i32 = 8;
const SPAWN_Z: i32 = 8;

/// Comfortably above the overworld ceiling, so a read here is air in every
/// world — used to learn the wire id of air without a registry.
const DEFINITELY_AIR_Y: i32 = 310;

const SEARCH_TOP: i32 = 300;
const SEARCH_BOTTOM: i32 = -64;

/// Generous: a debug-profile column is slow, and this deadline must not be the
/// thing that fails on a loaded machine.
const DEADLINE: Duration = Duration::from_secs(240);

/// The configured render distance under test, plus the
/// mesher's buffer ring — the exact arithmetic
/// `app::session::tick_render_distance` and the launch path both apply
/// (`render_distance + 1`), so this is the radius a real session at
/// `"render_distance": 32` asks the server for.
const OWNER_VIEW_RADIUS: i32 = 33;

/// The small radius shared by the other singleplayer gates. It is the control:
/// if both arms fail the defect is not radius-dependent, and if only the large
/// arm fails the radius *is* the discriminator.
const SMALL_VIEW_RADIUS: i32 = 1;

fn open_session(seed: i64, view_radius: i32, world_dir: Option<PathBuf>) -> Option<NetClient> {
    open_session_of_type(
        seed,
        lodestone::menu::create_world::WorldTypePreset::Normal,
        view_radius,
        world_dir,
    )
}

/// [`open_session`], but naming the
/// [`WorldTypePreset`](lodestone::menu::create_world::WorldTypePreset) rather
/// than hardcoding [`WorldTypePreset::Normal`](lodestone::menu::create_world::WorldTypePreset::Normal)
/// — see [`a_singleplayer_world_honours_the_selected_world_type_end_to_end`]
/// below, which is the one caller that needs a different arm.
fn open_session_of_type(
    seed: i64,
    world_type: lodestone::menu::create_world::WorldTypePreset,
    view_radius: i32,
    world_dir: Option<PathBuf>,
) -> Option<NetClient> {
    let protocol = lodestone::Config::default().protocol;
    let server_protocol = lodestone_registry::server_protocol_for_protocol(protocol)?;
    Some(NetClient::open_singleplayer(
        server_protocol,
        protocol,
        seed,
        world_type,
        view_radius,
        None,
        world_dir,
    ))
}

/// Pumps until `ready`, or panics naming the errors the session reported.
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
            assert!(
                errors.is_empty(),
                "reached `{what}` but the session reported errors: {errors:?}"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for `{what}`; errors: {errors:?}");
}

/// The whole point: a loaded chunk that is *entirely air* is exactly what the
/// A loaded chunk that is *entirely air* is the observable form of an empty-world
/// failure from here, and a count of arriving chunks cannot tell the two apart.
fn assert_spawn_column_has_terrain(net: &NetClient, radius: i32) {
    let air = net
        .block_at(BlockPos::new(SPAWN_X, DEFINITELY_AIR_Y, SPAWN_Z))
        .expect("a loaded chunk must answer for a y inside the world");

    let surface = (SEARCH_BOTTOM..=SEARCH_TOP)
        .rev()
        .find(|&y| net.block_at(BlockPos::new(SPAWN_X, y, SPAWN_Z)).is_some_and(|id| id != air));

    let solid = (SEARCH_BOTTOM..=SEARCH_TOP)
        .filter(|&y| net.block_at(BlockPos::new(SPAWN_X, y, SPAWN_Z)).is_some_and(|id| id != air))
        .count();

    assert!(
        surface.is_some(),
        "view_radius {radius}: the spawn column arrived but is entirely air \
         (air id {air}) — this is the empty-world symptom, not a delivery failure"
    );
    println!(
        "view_radius {radius}: spawn column surface y={surface:?}, {solid} non-air blocks, air id {air}"
    );
}

#[test]
fn a_singleplayer_world_arrives_with_terrain_at_the_shared_small_radius() {
    let Some(net) = open_session(4242, SMALL_VIEW_RADIUS, None) else {
        assert!(!cfg!(feature = "live"), "the default build must host singleplayer");
        return;
    };
    pump_until(&net, "the spawn chunk", |net| {
        net.is_chunk_loaded(ChunkPos { x: 0, z: 0 })
    });
    assert_spawn_column_has_terrain(&net, SMALL_VIEW_RADIUS);
}

#[test]
fn a_singleplayer_world_arrives_with_terrain_at_the_owners_render_distance() {
    let Some(net) = open_session(4242, OWNER_VIEW_RADIUS, None) else {
        assert!(!cfg!(feature = "live"), "the default build must host singleplayer");
        return;
    };
    pump_until(&net, "the spawn chunk", |net| {
        net.is_chunk_loaded(ChunkPos { x: 0, z: 0 })
    });
    assert_spawn_column_has_terrain(&net, OWNER_VIEW_RADIUS);
}

/// `WorldTypePreset::Amplified` must reach `NetClient::open_singleplayer` from a
/// create-world configuration and change the served terrain, not merely carry a
/// differently named generator that the wire never uses.
///
/// The expected values are not derived here — they are read from
/// `crates/lodestone-server/tests/world_type_selection.rs`'s own
/// `NORMAL_TOP_Y`/`AMPLIFIED_TOP_Y` constants (64/130 at seed 4242, chunk
/// `(0, 0)`, **local** `(0, 0)`, i.e. block `(0, *, 0)` — not the spawn
/// column at `(8, 8)` a couple of blocks over, which is a different noise
/// sample and would not reproduce either number). That file already proves
/// `overworld_generator_of_type` itself is a real, effective parameter one
/// layer down; this test covers the integration layer: `Origin::Integrated`'s
/// `world_type` field reaches the `overworld_chunk_source_of_type` call in
/// `net.rs`'s `run_async`, and the live session serves what it built rather than
/// something byte-identical to the default.
#[test]
fn a_singleplayer_world_honours_the_selected_world_type_end_to_end() {
    const SEED: i64 = 4242;
    const BLOCK_X: i32 = 0;
    const BLOCK_Z: i32 = 0;
    const NORMAL_TOP_Y: i32 = 64;
    const AMPLIFIED_TOP_Y: i32 = 130;

    fn top_non_air_y(net: &NetClient) -> i32 {
        let air = net
            .block_at(BlockPos::new(BLOCK_X, DEFINITELY_AIR_Y, BLOCK_Z))
            .expect("a loaded chunk must answer for a y inside the world");
        (SEARCH_BOTTOM..=SEARCH_TOP)
            .rev()
            .find(|&y| {
                net.block_at(BlockPos::new(BLOCK_X, y, BLOCK_Z))
                    .is_some_and(|id| id != air)
            })
            .expect("the column arrived but is entirely air")
    }

    let Some(overworld) = open_session_of_type(
        SEED,
        lodestone::menu::create_world::WorldTypePreset::Normal,
        SMALL_VIEW_RADIUS,
        None,
    ) else {
        assert!(!cfg!(feature = "live"), "the default build must host singleplayer");
        return;
    };
    pump_until(&overworld, "the spawn chunk", |net| {
        net.is_chunk_loaded(ChunkPos { x: 0, z: 0 })
    });
    assert_eq!(
        top_non_air_y(&overworld),
        NORMAL_TOP_Y,
        "the default arm must still serve exactly what it served before world_type \
         was threaded — a changed default here means the wiring touched the \
         Overworld path, not just added a new one"
    );

    let Some(amplified) = open_session_of_type(
        SEED,
        lodestone::menu::create_world::WorldTypePreset::Amplified,
        SMALL_VIEW_RADIUS,
        None,
    ) else {
        return; // Already asserted unreachable-only-without-`live` above.
    };
    pump_until(&amplified, "the spawn chunk", |net| {
        net.is_chunk_loaded(ChunkPos { x: 0, z: 0 })
    });
    assert_eq!(
        top_non_air_y(&amplified),
        AMPLIFIED_TOP_Y,
        "Origin::Integrated's world_type field must reach net.rs's \
         overworld_chunk_source_of_type call — a live singleplayer session \
         selecting Amplified must serve Amplified terrain over the real wire, \
         not silently fall back to the Overworld default"
    );
}

/// The four world presets exposed by the singleplayer entry point must reach the
/// wire through `net.rs`'s `preset_chunk_source`, not merely compile.
///
/// `Flat`/`DebugAllBlockStates` get a **strong**, externally-sourced check —
/// the exact block states `crates/lodestone-server/src/worldgen_data.rs`'s
/// own `flat_chunk_source_set_block_persists_and_stays_chunk_local`/
/// `debug_chunk_source_set_block_persists_and_stays_chunk_local` tests already
/// measure at those crate-internal `ChunkSource`s directly (a `classic_flat`
/// column's `(0, -61, 0)` is `minecraft:grass_block[snowy=false]`; the debug
/// grid's `(1, 60, 1)` is `minecraft:barrier`) — reused here rather than
/// re-derived, and now asked of a **live session** instead of a bare
/// `ChunkSource`, which is the integration layer this test exercises.
/// `SingleBiomeSurface` gets the same weak-but-real "terrain arrived" check
/// [`open_session`] uses: there is no independently measured oracle value for
/// it reachable from this crate (its default biome, `minecraft:plains`, has
/// no fixed sea-level surface block asserted anywhere this crate can cite
/// without re-deriving the external biome and surface rules), so this stops at
/// proving the session reaches real, non-air terrain rather than guessing a
/// stronger assertion.
#[test]
fn the_four_newly_wired_presets_serve_their_own_generator_end_to_end() {
    use lodestone::menu::create_world::WorldTypePreset;

    // `lodestone_data::block_states::block_name` returns only the *base* name
    // (`"minecraft:grass_block"`), never a bracketed property string — the
    // property values are a separate accessor. So a state that carries
    // properties has to be matched on both, not on a hand-assembled name
    // string that no lookup in this crate ever produces.
    fn state_id_with(name: &str, props: &[(&str, &str)]) -> u32 {
        (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| {
                lodestone_data::block_states::block_name(id) == Some(name)
                    && lodestone_data::block_states::properties(id) == Some(props)
            })
            .unwrap_or_else(|| panic!("{name} with {props:?} is not in the 26.2 block-state table"))
    }

    let Some(flat) = open_session_of_type(4242, WorldTypePreset::Flat, SMALL_VIEW_RADIUS, None)
    else {
        assert!(!cfg!(feature = "live"), "the default build must host singleplayer");
        return;
    };
    pump_until(&flat, "the spawn chunk", |net| net.is_chunk_loaded(ChunkPos { x: 0, z: 0 }));
    assert_eq!(
        flat.block_at(BlockPos::new(0, -61, 0)),
        Some(state_id_with("minecraft:grass_block", &[("snowy", "false")])),
        "a live session selecting WorldTypePreset::Flat must serve the exact classic_flat \
         layer stack `flat_chunk_source`'s own test measures, not silently fall back to \
         the Normal default"
    );

    let Some(debug) =
        open_session_of_type(4242, WorldTypePreset::DebugAllBlockStates, SMALL_VIEW_RADIUS, None)
    else {
        return; // Already asserted unreachable-only-without-`live` above.
    };
    pump_until(&debug, "the spawn chunk", |net| net.is_chunk_loaded(ChunkPos { x: 0, z: 0 }));
    assert_eq!(
        debug.block_at(BlockPos::new(1, 60, 1)),
        // Barrier is waterloggable in 26.2, defaulting `false` — not a
        // stateless block, so the bracketed property has to be matched too.
        Some(state_id_with("minecraft:barrier", &[("waterlogged", "false")])),
        "a live session selecting WorldTypePreset::DebugAllBlockStates must serve the exact \
         debug grid `debug_chunk_source`'s own test measures"
    );

    let Some(single_biome) = open_session_of_type(
        4242,
        WorldTypePreset::SingleBiomeSurface,
        SMALL_VIEW_RADIUS,
        None,
    ) else {
        return;
    };
    pump_until(&single_biome, "the spawn chunk", |net| {
        net.is_chunk_loaded(ChunkPos { x: 0, z: 0 })
    });
    assert_spawn_column_has_terrain(&single_biome, SMALL_VIEW_RADIUS);
}

/// One full section face is `16 * 16 = 256` quads; a real terrain column is
/// thousands. The floor matches the one `live_world_mesh.rs` uses against its
/// external terrain oracle, for the same reason: it separates "meshed something" from
/// "meshed a single stray face".
const MIN_QUADS: usize = 256;

/// The second half of "the world loads completely empty": blocks can arrive
/// (asserted above) and still reach **zero geometry**, which looks identical
/// from a chair. This runs the shell's own live snapshot+mesh path — the same
/// two functions `TerrainMesh`'s workers call — over the spawn column of a real
/// integrated session, so a classifier/atlas/id-space fault that leaves every
/// column meshing to nothing fails here rather than only on screen.
///
/// Deliberately **not** a GPU gate: it stops at the quads, because everything
/// past that point needs an adapter and this must run in the ordinary suite.
#[test]
// `BlockResources::load(true)` finding no terrain atlas is the empty-world
// symptom this gate exists to catch, so it fails loudly rather than skipping
// — which also means it cannot run where there is no `.cache/mc/<version>/`.
// The two gates above it in this file need no jar and still run everywhere.
#[ignore = "requires a fetched vanilla client.jar + blocks.json under .cache/mc/<version>/"]
fn a_singleplayer_spawn_column_meshes_into_real_geometry() {
    use lodestone::blocks::ShellClassifier;
    use lodestone::mesher::{SectionKey, mesh_snapshot, snapshot_section_live};
    use lodestone::resources::BlockResources;

    let resources = BlockResources::load(true);
    let Some(_atlas) = resources.vanilla_atlas.as_ref() else {
        // `Sim::refresh_mesh_policy` sets `MeshPolicy::id_spaces_agree` from
        // exactly this `Option`, and a live session with no terrain atlas
        // drops **every** column. So a missing atlas is the empty-world
        // symptom itself, not a reason to skip.
        panic!(
            "BlockResources::load(true) produced no vanilla atlas — with a live session that \
             sets MeshPolicy::id_spaces_agree = false and every column is dropped unmeshed, \
             which is the empty-world report. Check .cache/mc/<ver>/{{client.jar, \
             generated/reports/blocks.json}}."
        );
    };
    let classifier: ShellClassifier = resources.classifier;

    let Some(net) = open_session(4242, SMALL_VIEW_RADIUS, None) else {
        assert!(!cfg!(feature = "live"), "the default build must host singleplayer");
        return;
    };
    pump_until(&net, "the spawn chunk", |net| {
        net.is_chunk_loaded(ChunkPos { x: 0, z: 0 }) && net.world_dimensions().is_some()
    });
    let dims = net.world_dimensions().expect("dimensions after login");
    let section_count = dims.section_count();

    let mut quads = 0usize;
    let mut meshed_sections = 0usize;
    for si in 0..section_count {
        let key = SectionKey { cx: 0, cz: 0, si, min_y: dims.min_y };
        if let Some(snap) = snapshot_section_live(&net, key, section_count).any() {
            let mesh = mesh_snapshot(&snap, &classifier);
            if mesh.quad_count() > 0 {
                meshed_sections += 1;
            }
            quads += mesh.quad_count();
        }
    }
    println!("spawn column: {meshed_sections} sections meshed, {quads} quads");
    assert!(
        quads > MIN_QUADS,
        "the spawn column meshed only {quads} quads across {meshed_sections} sections — below \
         the {MIN_QUADS} floor. Blocks arrived (the gates above prove it) and produced no \
         geometry: that is the empty-world symptom, and it lives in the classifier/atlas, not \
         in chunk delivery."
    );
}

/// The whole production chain, minus the GPU: `Sim::new` → `attach_net` →
/// `step()` → `drain_meshes()`, at the owner's own render distance.
///
/// The two gates above check the ends — blocks arrive, and a column *can* be
/// meshed — and neither touches the middle: `Sim`'s arrival→dirty→schedule→
/// upload pipeline, which is the layer between them and the only one a windowed
/// session actually runs. `drain_meshes` is exactly what `app/redraw.rs` feeds
/// to `RenderState::upload_section`, so a non-empty drain is "geometry reached
/// the edge of the renderer".
///
/// The discriminating input is again `render_distance`: every other `Sim`-level
/// gate in this crate runs at 8 (`live_camera_follows_server_spawn`'s
/// `live_config`), and 8 is a value the empty-world failure does *not* come
/// from.
#[test]
// Same resource-pack precondition as its sibling above: with no terrain atlas,
// `Sim::refresh_mesh_policy` sets `id_spaces_agree = false` and every column
// is dropped unmeshed, so the drain is empty for an environmental reason
// rather than a code one.
#[ignore = "requires a fetched vanilla client.jar + blocks.json under .cache/mc/<version>/"]
fn a_sim_at_the_owners_render_distance_drains_real_terrain_meshes() {
    use lodestone::sim::Sim;
    use lodestone::{Config, Mode};

    let protocol = Config::default().protocol;
    let Some(server_protocol) = lodestone_registry::server_protocol_for_protocol(protocol) else {
        assert!(!cfg!(feature = "live"), "the default build must host singleplayer");
        return;
    };
    let config = Config {
        mode: Mode::Window,
        protocol,
        // The configured persisted `"render_distance": 32`. The streamed radius is
        // this + 1, the same arithmetic `app/session.rs` applies.
        render_distance: 32,
        ..Config::default()
    };
    let mut sim = Sim::new(config);
    assert!(
        sim.vanilla_atlas().is_some(),
        "no vanilla atlas, so `Sim::refresh_mesh_policy` sets `id_spaces_agree = false` and \
         every column is dropped unmeshed — the empty-world symptom itself. Banner: {:?}",
        sim.asset_banner()
    );
    sim.attach_net(NetClient::open_singleplayer(
        server_protocol,
        protocol,
        4242,
        lodestone::menu::create_world::WorldTypePreset::Normal,
        OWNER_VIEW_RADIUS,
        None,
        None,
    ));

    let deadline = Instant::now() + DEADLINE;
    let mut meshes = 0usize;
    let mut quads = 0usize;
    while Instant::now() < deadline && meshes < 16 {
        sim.step(1.0 / 20.0);
        for meshed in sim.drain_meshes() {
            quads += meshed.mesh.quad_count();
            meshes += 1;
        }
        let _ = sim.drain_removals();
        std::thread::sleep(Duration::from_millis(10));
    }

    println!("Sim at render_distance 32: drained {meshes} section meshes, {quads} quads");
    assert!(
        meshes > 0 && quads > MIN_QUADS,
        "a real Sim at the owner's render distance drained {meshes} section meshes / {quads} \
         quads — nothing reached the renderer. Chunks arrive and a column meshes standalone \
         (the two gates above), so an empty drain localises the fault to Sim's own \
         arrival→schedule→upload path."
    );
}
