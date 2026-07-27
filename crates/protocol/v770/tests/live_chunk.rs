//! Live chunk-seam acceptance test (Phase 2 gate).
//!
//! Gated behind the `live-chunk` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run it against the real vanilla 26.2 server
//! (offline mode, flat world) on `127.0.0.1:25565` with:
//!
//! ```text
//! cargo test -p lodestone-v770 --features live-chunk --test live_chunk -- --ignored --nocapture
//! ```
//!
//! This drives the join flow at the packet level over
//! [`lodestone_net::Connection`], and consumes chunks through the **public
//! [`VersionAdapter::handle_packet`] seam**: every `level_chunk_with_light`
//! (id 45) packet is lifted by the adapter and applied to the client-owned
//! [`World`](lodestone_world::World) sink, exactly as a real consumer's world
//! is populated. The adapter emits only a lightweight
//! [`ClientEvent::ChunkLoaded`] *notification* carrying the position; the test
//! then reads the decoded data back out of the world by that key — proving the
//! data reached a queryable consumer, not just that the decoder works. An
//! earlier version reached into `packets::chunk` and decoded the bytes itself,
//! which proved the decoder but bypassed the seam. Asserted properties:
//!
//! * the adapter accepts every real chunk (its internal `ensure_empty` makes a
//!   subtly wrong layout an error, so a silent misparse cannot pass) — the
//!   **zero trailing bytes** guarantee, now enforced by production code;
//! * every accepted chunk is present in the `World` afterwards (225 of them);
//! * the flat world's known layers land at the right Y (catches a byte-correct
//!   but YZX-transposed decode);
//! * it reports chunks stored, palette-strategy distribution, and the honest
//!   measured heap of the full chunk data (blocks, biomes, light, heightmaps)
//!   actually held in the world.
//!
//! Note: this queries the `World` the adapter fills directly. The final
//! Phase-2 gate — querying that world through `lodestone-client`'s public
//! handle — additionally depends on the client's world-query surface, which is
//! `impl-client`'s half of the seam.
#![cfg(feature = "live-chunk")]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use lodestone_model::{
    ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_v770::V770Adapter;
use lodestone_v770::block_states;
use lodestone_v770::packet_ids::play;
use lodestone_world::{
    ChunkColumn, ChunkPos as WorldChunkPos, LightProperties, PalettedContainer, World,
    compute_column_light, diff_column_light,
};
use tokio::net::TcpStream;
use uuid::Uuid;

mod common;
use common::unique_username;

#[derive(Default, Debug, Clone, Copy)]
struct PaletteStats {
    single: usize,
    indirect: usize,
    direct: usize,
}

impl PaletteStats {
    fn record(&mut self, container: &PalettedContainer) {
        if container.is_single() {
            self.single += 1;
        } else if container.palette_len() > 0 {
            self.indirect += 1;
        } else {
            self.direct += 1;
        }
    }
}

/// Returns whether the bottom section of `column` is horizontally uniform at
/// every one of its lowest 16 planes — i.e. a genuinely flat column.
fn bottom_is_flat(column: &ChunkColumn) -> bool {
    let min_y = column.min_y();
    (0..16).all(|dy| {
        let y = min_y + dy;
        let first = column.get_block(0, y, 0);
        (0..16).all(|x| (0..16).all(|z| column.get_block(x, y, z) == first))
    })
}

/// Applies one non-chunk directive against the live connection, updating the
/// tracked connection state.
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
        Directive::Emit(_) => {}
        Directive::Disconnect(reason) => {
            panic!("server disconnected us: {}", reason.to_plain_string());
        }
        _ => {}
    }
}

#[tokio::test]
#[ignore = "requires a live Minecraft server on 127.0.0.1:25565"]
async fn decodes_real_chunks_from_live_server() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let profile = LoginProfile {
        // Unique per run, and NOT a name shared with other live tests.
        //
        // In offline mode the server derives the account UUID from the
        // *username* (`OfflinePlayer:<name>`) and ignores the UUID we send, so
        // every test using the same name shares one persisted player file. If
        // any run leaves that player dead (a mob kill is enough), vanilla holds
        // the rejoining client on the death screen and sends **zero chunks**
        // until it receives `client_command(perform_respawn)` — the join, the
        // keep-alives and the entity traffic all look perfectly healthy while
        // the chunk stream is silently empty. Costing a unique name per run
        // removes the shared mutable state entirely.
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V770Adapter::new();
    // The client-owned world the adapter applies decoded chunks to. Each chunk
    // is stored once; the ChunkLoaded events below are bare notifications and we
    // read the data back out of here by position.
    let mut world = World::new();

    let mut conn = Connection::connect("127.0.0.1:25565")
        .await
        .expect("connect to live server");
    let mut state = ConnectionState::Handshaking;

    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    // Positions announced via ChunkLoaded across the public seam.
    let mut positions: Vec<(i32, i32)> = Vec::new();
    let mut palette = PaletteStats::default();
    let mut unloaded = 0usize;
    let mut reached_play = false;
    // First chunk seen, and the first *provably-flat* column position — see the
    // old note: the seed-`lodestone` flat world has a village whose foundations
    // make some columns non-flat, and arrival order is nondeterministic, so we
    // assert on the first flat column and keep the first chunk as a fallback so
    // a YZX-transposed decode (which leaves no flat column) still trips the
    // uniformity assertions.
    let mut sample: Option<(i32, i32)> = None;
    let mut flat_sample: Option<(i32, i32)> = None;

    let overall = Duration::from_secs(60);
    let read_timeout = Duration::from_secs(5);
    let collect_window = Duration::from_secs(8);
    let target_chunks = 225usize;
    let mut first_chunk_at: Option<Instant> = None;
    let mut play_packets = 0usize;

    let _ = tokio::time::timeout(overall, async {
        loop {
            if let Some(t) = first_chunk_at
                && (positions.len() >= target_chunks || t.elapsed() >= collect_window)
            {
                break;
            }
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let packet = match read {
                Err(_) => break, // no packet within read_timeout: server quiet
                Ok(Ok(Some(packet))) => packet,
                Ok(Ok(None)) => break, // clean EOF
                Ok(Err(err)) => panic!("read error: {err}"),
            };
            let (packet_id, payload) = packet;

            if state == ConnectionState::Play {
                play_packets += 1;
                if !reached_play {
                    reached_play = true;
                    eprintln!("reached Play; receiving chunks…");
                }
            }

            // Vanilla throttles chunk delivery: after each batch it sends
            // chunk_batch_finished and waits for our chunk_batch_received ACK
            // before sending the next batch, gating off permanently after a
            // small number of unacknowledged batches. The adapter now *models*
            // this flow-control — its `chunk_batch_finished` handler emits the
            // `chunk_batch_received` ACK as a `Directive::Send` — so this driver
            // no longer ACKs by hand; the batch packets fall through to the
            // public seam below and the emitted ACK is written by `apply`. The
            // dedicated duration regression (`live_chunk_flow`) proves this loop
            // keeps delivery alive past the credit window, with a negative
            // control that stalls at the first batch when the ACK is suppressed.

            // Everything else — including chunks — goes through the public
            // adapter seam. A chunk that fails to decode (including any trailing
            // bytes, which the adapter's `ensure_empty` rejects) must fail the
            // test loudly; other loosely-modelled play packets may legitimately
            // be unhandled and are tolerated.
            let is_chunk = state == ConnectionState::Play
                && packet_id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT;
            let result = adapter.handle_packet(&mut world, state, packet_id, &payload);
            let directives = if is_chunk {
                result.expect(
                    "real chunk decodes through the public adapter seam \
                     (zero trailing bytes enforced by the adapter)",
                )
            } else {
                result.unwrap_or_default()
            };

            for directive in directives {
                match directive {
                    Directive::Emit(ClientEvent::ChunkLoaded { pos }) => {
                        // The adapter applied the chunk to the world before
                        // emitting this notification; read it back by position.
                        let loaded = world
                            .get(WorldChunkPos::new(pos.x, pos.z))
                            .expect("chunk present in world at its ChunkLoaded notification");
                        let column = &loaded.column;
                        for index in 0..column.section_count() {
                            if let Some(section) = column.section(index) {
                                palette.record(section.block_states());
                            }
                        }
                        if flat_sample.is_none() && bottom_is_flat(column) {
                            flat_sample = Some((pos.x, pos.z));
                        }
                        if sample.is_none() {
                            sample = Some((pos.x, pos.z));
                        }
                        positions.push((pos.x, pos.z));
                        first_chunk_at.get_or_insert_with(Instant::now);
                    }
                    Directive::Emit(ClientEvent::ChunkUnloaded { .. }) => {
                        unloaded += 1;
                    }
                    other => apply(&mut conn, &mut state, other).await,
                }
            }
        }
    })
    .await;

    eprintln!("play packets seen: {play_packets}");
    assert!(reached_play, "never reached Play / received a chunk");
    // Anti-vacuity floor: the lab flat world at the server's render distance
    // delivers ~225 columns (a 15×15 area). A bare `!is_empty()` would let a
    // truncated stream that trickled in a single chunk pass while proving almost
    // nothing about the seam under load. Require a substantial fraction so a
    // near-empty stream (server misconfigured, chunk flow-control broken, world
    // failed to load) FAILS loudly rather than passing on one column. The floor
    // sits well below 225 so ordinary timing margin cannot flake it.
    const MIN_CHUNKS: usize = 100;
    assert!(
        positions.len() >= MIN_CHUNKS,
        "expected the live flat world to stream at least {MIN_CHUNKS} chunks, got {} \
         (is `lodestone-mc262` healthy and streaming a full render distance? \
         truncated chunk flow-control or a stalled world load looks like this)",
        positions.len()
    );
    assert_eq!(
        world.len(),
        positions.len(),
        "every announced chunk must be stored once in the world"
    );

    // ---- Flat-world structure: pins YZX and the known layering. ----
    let (sx, sz) = flat_sample.or(sample).expect("at least one chunk");
    let sample_chunk = world
        .get(WorldChunkPos::new(sx, sz))
        .expect("sample chunk present in world");
    let column = &sample_chunk.column;
    let min_y = column.min_y();

    // For the lowest 16 blocks, classify each horizontal plane as uniform
    // (all 256 x,z equal) and capture its value. A YZX-transposed decode would
    // shatter plane-uniformity, because the flat world's layers are constant in
    // x and z at a fixed y.
    let mut layers: Vec<(i32, Option<u32>)> = Vec::new();
    for dy in 0..16 {
        let y = min_y + dy;
        let first = column.get_block(0, y, 0);
        let uniform = (0..16).all(|x| (0..16).all(|z| column.get_block(x, y, z) == first));
        layers.push((y, uniform.then_some(first)));
    }

    for (y, value) in &layers {
        assert!(
            value.is_some(),
            "plane at y={y} is not uniform — decode is likely YZX-transposed"
        );
    }

    let bedrock = layers[0].1.unwrap();
    assert_ne!(
        bedrock, 0,
        "lowest layer (y={min_y}) should be solid, not air"
    );

    let solid_planes = layers.iter().take_while(|(_, v)| *v != Some(0)).count();
    assert!(solid_planes >= 1, "expected at least one solid layer");
    assert!(
        solid_planes < 16,
        "expected air above the terrain within the bottom section"
    );
    for (y, value) in layers.iter().skip(solid_planes) {
        assert_eq!(*value, Some(0), "expected air at y={y} above the terrain");
    }

    assert_eq!(column.get_block(0, 200, 0), 0, "air far above terrain");

    // ---- Report ----
    let total_bytes = world.heap_bytes();
    let per_chunk = total_bytes as f64 / world.len() as f64;
    eprintln!("\n=== LIVE CHUNK DECODE REPORT (public seam → World) ===");
    eprintln!("chunks stored in World    : {}", world.len());
    eprintln!("ChunkLoaded notifications : {}", positions.len());
    eprintln!("chunk unloads seen        : {unloaded}");
    eprintln!("trailing bytes per chunk  : 0 (adapter ensure_empty rejects any misparse)");
    eprintln!(
        "block-state palettes      : single={} indirect={} direct={}",
        palette.single, palette.indirect, palette.direct
    );
    eprint!("flat-world layers (y: id) : ");
    for (y, value) in &layers {
        if *value == Some(0) {
            eprint!("[y{y}:air] ");
        } else {
            eprint!("[y{y}:{}] ", value.unwrap());
        }
    }
    eprintln!();
    eprintln!(
        "measured world heap       : {total_bytes} bytes total (blocks+biomes+light+heightmaps), {per_chunk:.0} B/chunk avg"
    );
    eprintln!("======================================================\n");
}

// ============================================================================
// Live light oracle: impl-world's version-free light engine judged against the
// real server.
//
// `compute_column_light` / `diff_column_light` (in lodestone-world) are the
// engine and its cell-by-cell comparator. The engine needs two inputs, and this
// crate is the only place that holds both:
//
//   * a protocol-776 `LightProperties` — block-state id → (opacity, emission).
//     Only a version crate can supply this: the id numbering is 776-specific.
//   * a real server's decoded `ColumnLight`. Every `level_chunk_with_light`
//     carries the column's light and the adapter stores it on `LoadedChunk.light`.
//
// So the oracle is hosted here rather than in lodestone-world, which has neither.
// ============================================================================

/// A protocol-776 [`LightProperties`], indexed by block-state id.
///
/// The opacity/emission of a block are not in Mojang's data-generator report
/// (they are code constants), so they come from the committable community
/// dataset (`vendor/minecraft-data`), whose `filterLight`/`emitLight` are exactly
/// vanilla's light-dampening and emission. We deliberately do **not** trust
/// minecraft-data's own state-id numbering (it may drift from 776): instead we
/// key its values by block *name* and resolve each 776 state id → name through
/// this crate's authoritative generated table ([`block_states::block_name`]).
struct V770LightProps {
    /// `(opacity, emission)` per block-state id, `0..block_states::STATE_COUNT`.
    by_state: Vec<(u8, u8)>,
}

impl V770LightProps {
    /// Builds the table from vendored minecraft-data, keyed by block name.
    fn load() -> Self {
        // 1.21.11 is the newest vendored dataset; its block *names* cover the flat
        // world's blocks (air/bedrock/dirt/grass_block). Blocks that exist in 26.2
        // but not here default to opaque and are counted below — they cannot occur
        // in the featureless flat column the gate actually judges.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../vendor/minecraft-data/data/pc/1.21.11/blocks.json"
        );
        let text = std::fs::read_to_string(path)
            .expect("read vendored minecraft-data blocks.json (committable light source)");
        let blocks: serde_json::Value =
            serde_json::from_str(&text).expect("parse minecraft-data blocks.json");

        let mut by_name: HashMap<String, (u8, u8)> = HashMap::new();
        for block in blocks.as_array().expect("blocks.json is a JSON array") {
            let name = block["name"]
                .as_str()
                .expect("block has a name")
                .to_string();
            let opacity = block["filterLight"].as_u64().unwrap_or(0) as u8;
            let emission = block["emitLight"].as_u64().unwrap_or(0) as u8;
            by_name.insert(name, (opacity, emission));
        }

        let mut by_state = vec![(0u8, 0u8); block_states::STATE_COUNT as usize];
        let mut unmapped = 0usize;
        for id in 0..block_states::STATE_COUNT {
            let full = block_states::block_name(id).expect("state id in range");
            let short = full.strip_prefix("minecraft:").unwrap_or(full);
            match by_name.get(short) {
                Some(&pair) => by_state[id as usize] = pair,
                None => {
                    // Default opaque, non-emitting: conservative for an unknown
                    // solid, and irrelevant to the featureless column judged.
                    by_state[id as usize] = (15, 0);
                    unmapped += 1;
                }
            }
        }
        eprintln!(
            "light props: {} of {} block-state ids mapped from minecraft-data 1.21.11 \
             ({unmapped} unmapped, defaulted opaque)",
            block_states::STATE_COUNT as usize - unmapped,
            block_states::STATE_COUNT
        );
        Self { by_state }
    }
}

impl LightProperties for V770LightProps {
    fn opacity(&self, state: u32) -> u8 {
        self.by_state.get(state as usize).map_or(0, |&(o, _)| o)
    }

    fn emission(&self, state: u32) -> u8 {
        self.by_state.get(state as usize).map_or(0, |&(_, e)| e)
    }
}

/// A deliberately wrong [`LightProperties`] where every block is transparent and
/// unlit. Used as the gate's built-in negative control: with nothing blocking
/// sky light it floods below the surface, so the oracle *must* report
/// disagreements against the real server — proving the comparison actually
/// checks values rather than trivially returning zero.
struct AllTransparentProps;

impl LightProperties for AllTransparentProps {
    fn opacity(&self, _state: u32) -> u8 {
        0
    }

    fn emission(&self, _state: u32) -> u8 {
        0
    }
}

/// Whether `column` is air at every cell from `min_y + 4` upward — i.e. only the
/// flat world's 4-block bedrock/dirt/dirt/grass floor is present and nothing
/// above it casts a shadow.
///
/// Such a column has a purely vertical sky-light profile identical to every
/// other flat column, so its interior light does not depend on the (unseen)
/// neighbouring chunks. That is what lets the oracle compare with
/// `interior_margin = 0` — every cell, including chunk borders — without seam
/// false positives. The seed-`lodestone` flat world also contains a village, so
/// this rejects any column carrying structure.
fn column_is_featureless_above(column: &ChunkColumn) -> bool {
    let min_y = column.min_y();
    let top = min_y + (column.section_count() as i32) * 16;
    for y in (min_y + 4)..top {
        for x in 0..16usize {
            for z in 0..16usize {
                if column.get_block(x, y, z) != 0 {
                    return false;
                }
            }
        }
    }
    true
}

/// Judges [`compute_column_light`] against the light the real server computed
/// and sent us — an oracle we neither control nor can accidentally satisfy.
///
/// Full invocation (both are required):
///
/// ```text
/// cargo test -p lodestone-v770 --features live-chunk --test live_chunk \
///     -- --ignored --nocapture computed_light_matches_server_oracle_on_flat_world
/// ```
///
/// Without `--features live-chunk` this whole file is `#![cfg]`-compiled to
/// nothing and the run prints `ok. 0 passed`, which reads exactly like success —
/// so the feature flag is not optional. Without `--ignored` the test is skipped.
/// If the server is unreachable the test FAILS (never skips), per §12.52.
///
/// The result is reported as a **count** ("0 of N cells differ"), not a boolean:
/// a nonzero count and its sky/block split tell `impl-world` immediately whether
/// a regression is the vertical-vs-horizontal attenuation asymmetry or a section
/// seam. A built-in negative control (all-transparent props) proves the diff is
/// actually comparing values and not vacuously agreeing.
#[tokio::test]
#[ignore = "requires a live Minecraft server on 127.0.0.1:25565"]
async fn computed_light_matches_server_oracle_on_flat_world() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V770Adapter::new();
    let mut world = World::new();

    // FAIL, never skip, when the server is unreachable (§12.52): a skipped gate
    // reads as a pass, and this one exists to catch a light-engine regression.
    let mut conn = Connection::connect("127.0.0.1:25565")
        .await
        .expect("connect to live server (gate fails, never skips, if unreachable)");
    let mut state = ConnectionState::Handshaking;
    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    // Collect chunks. Light rides along in level_chunk_with_light and the adapter
    // stores it on LoadedChunk.light; we ACK batches via `apply` exactly as the
    // main test does, so delivery does not stall.
    let mut chunks = 0usize;
    let overall = Duration::from_secs(60);
    let read_timeout = Duration::from_secs(5);
    let collect_window = Duration::from_secs(8);
    let target = 225usize;
    let mut first_chunk_at: Option<Instant> = None;

    let _ = tokio::time::timeout(overall, async {
        loop {
            if let Some(t) = first_chunk_at
                && (chunks >= target || t.elapsed() >= collect_window)
            {
                break;
            }
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let packet = match read {
                Err(_) => break,
                Ok(Ok(Some(packet))) => packet,
                Ok(Ok(None)) => break,
                Ok(Err(err)) => panic!("read error: {err}"),
            };
            let (packet_id, payload) = packet;
            let is_chunk = state == ConnectionState::Play
                && packet_id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT;
            let result = adapter.handle_packet(&mut world, state, packet_id, &payload);
            let directives = if is_chunk {
                result.expect("real chunk decodes through the public seam")
            } else {
                result.unwrap_or_default()
            };
            for directive in directives {
                match directive {
                    Directive::Emit(ClientEvent::ChunkLoaded { .. }) => {
                        chunks += 1;
                        first_chunk_at.get_or_insert_with(Instant::now);
                    }
                    Directive::Emit(_) => {}
                    other => apply(&mut conn, &mut state, other).await,
                }
            }
        }
    })
    .await;

    assert!(
        chunks >= 100,
        "expected the flat world to stream chunks (got {chunks}); without chunks \
         there is no server light to judge against"
    );

    let props = V770LightProps::load();

    // Find the first column that is featureless above the surface, then judge our
    // computed light against the server's for that column, cell by cell.
    let mut judged = 0usize;
    let mut report: Option<(i32, i32, usize)> = None;
    for (pos, loaded) in world.iter() {
        if !column_is_featureless_above(&loaded.column) {
            continue;
        }
        let ours = compute_column_light(&loaded.column, &props);
        // interior_margin = 0: compare every cell. The flat overworld has no
        // horizontal light gradient, so even border cells are exact — there is no
        // neighbour-chunk contribution to exclude. (impl-world suggested 15, but
        // for a 16-wide column that collapses the compared range to empty
        // [lo=15, hi=1] and would compare zero cells, the vacuous pass this
        // project bans — reported back to impl-world.)
        let d = diff_column_light(&ours, &loaded.light, 0);
        println!(
            "light oracle @ chunk ({}, {}): {} of {} cells differ (sky {}, block {})",
            pos.x,
            pos.z,
            d.disagreements(),
            d.cells_compared,
            d.sky_disagreements,
            d.block_disagreements
        );
        assert!(
            d.cells_compared > 0,
            "oracle compared zero cells at ({}, {}) — did the server elide all \
             light sections?",
            pos.x,
            pos.z
        );
        assert_eq!(
            d.disagreements(),
            0,
            "computed light disagrees with the live server at chunk ({}, {}): \
             sky {}, block {} of {} cells",
            pos.x,
            pos.z,
            d.sky_disagreements,
            d.block_disagreements,
            d.cells_compared
        );

        // Built-in negative control: an all-transparent world lets sky light
        // flood below the surface, so the same comparison MUST now disagree.
        // Without this, "0 of N differ" could mean the diff never actually
        // checks anything (the vacuity this project keeps finding).
        let broken = compute_column_light(&loaded.column, &AllTransparentProps);
        let nd = diff_column_light(&broken, &loaded.light, 0);
        println!(
            "negative control @ chunk ({}, {}): {} of {} cells differ (transparent world)",
            pos.x,
            pos.z,
            nd.disagreements(),
            nd.cells_compared
        );
        assert!(
            nd.disagreements() > 0,
            "negative control: a transparent world must disagree with the server's \
             light, but the oracle reported full agreement — the comparison is not \
             actually checking cell values"
        );

        judged += 1;
        report = Some((pos.x, pos.z, d.cells_compared));
        break;
    }

    let (cx, cz, cells) = report.expect("found no featureless flat column to judge light against");
    assert!(judged > 0);
    eprintln!("\n=== LIVE LIGHT ORACLE (compute_column_light vs server) ===");
    eprintln!("judged chunk              : ({cx}, {cz})");
    eprintln!("cells compared            : {cells} (0 disagreements)");
    eprintln!("props source              : vendor/minecraft-data 1.21.11 blocks.json");
    eprintln!("interior_margin           : 0 (flat overworld: no horizontal gradient)");
    eprintln!("==========================================================\n");
}
