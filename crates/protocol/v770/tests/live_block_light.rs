//! Live **block-light** oracle (light-engine emission/decay gate).
//!
//! The sky-light oracle in `live_chunk.rs` judges [`compute_column_light`]
//! against a real server, but a *flat overworld* has zero block light anywhere,
//! so it exercises only the sky path. This gate reaches the other half of the
//! engine — **seed at `getLightEmission`, decay by `max(1, opacity)` per step** —
//! by placing a known light source in mid-air over RCON and diffing the block
//! light the server itself relights and sends back against the light our engine
//! computes from the same blocks. It is an oracle we neither control nor can
//! accidentally satisfy: the vanilla server computed the numbers.
//!
//! Full invocation (all three parts are required):
//!
//! ```text
//! cargo test -p lodestone-v770 --features live-block-light --test live_block_light \
//!     -- --ignored --nocapture
//! ```
//!
//! * Without `--features live-block-light` this whole file is `#![cfg]`-compiled
//!   to nothing and the run prints `ok. 0 passed`, which reads exactly like
//!   success — so the feature flag is not optional.
//! * Without `--ignored` the test is skipped.
//! * It targets the purpose-built **RCON oracle** (game on `:25567`, RCON on
//!   `:25575`, password `lodestone`) — the one server where we can both *place*
//!   a known block and *watch* the server's recomputed light arrive. The mc262
//!   server on `:25565` has no reachable RCON, so a source cannot be placed
//!   there. If the oracle (game or RCON) is unreachable the test **FAILS**, it
//!   never skips (§12.52): a skipped gate reads as a pass.
//!
//! The result is reported as a **count** ("0 of N block cells differ"), not a
//! boolean, per the project's evidence standard. A built-in non-vacuity check
//! asserts the server actually lit the source (max block light ≥ 14 in the band)
//! so "0 differ" can never mean "nothing was compared".
#![cfg(feature = "live-block-light")]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use lodestone_model::{
    ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_testsupport::RconClient;
use lodestone_v770::V770Adapter;
use lodestone_v770::block_states;
use lodestone_v770::packet_ids::play;
use lodestone_world::{
    ChunkColumn, ChunkPos as WorldChunkPos, ColumnLight, LightData, LightProperties, World,
    compute_column_light, diff_column_light,
};
use tokio::net::TcpStream;
use uuid::Uuid;

mod common;
use common::unique_username;

/// The purpose-built RCON oracle: game on `:25567`, RCON on `:25575`.
const GAME_ADDR: &str = "127.0.0.1:25567";
const RCON_ADDR: &str = "127.0.0.1:25575";
const RCON_PASSWORD: &str = "lodestone";

/// How far above the player's head to place the source. High enough that no
/// terrain (this column's or a neighbour's) sits within the source's 15-block
/// reach, so the lit region is pure air lit by a single in-column source — a
/// case where our engine and the server must agree cell-for-cell with no
/// seam or neighbour-chunk contribution to exclude.
const SOURCE_ELEVATION: i32 = 24;

/// A protocol-776 [`LightProperties`] keyed by block-state id, built from the
/// committable community dataset (`vendor/minecraft-data`), whose
/// `filterLight`/`emitLight` are exactly vanilla's light-dampening and emission.
///
/// We deliberately do **not** trust minecraft-data's own state-id numbering (it
/// may drift from 776): each value is keyed by block *name* and resolved to a
/// 776 state id through this crate's authoritative generated table
/// ([`block_states::block_name`]). This mirrors the canonical copy in
/// `live_chunk.rs`; the two must stay in sync (both feed the same live oracle),
/// but each test binary is compiled independently so the helper is duplicated
/// rather than sharing a dependency edge (see `common/mod.rs`).
struct V770LightProps {
    by_state: Vec<(u8, u8)>,
}

impl V770LightProps {
    fn load() -> Self {
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
            let name = block["name"].as_str().expect("block has a name").to_string();
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

/// Applies one non-chunk directive against the live connection, updating the
/// tracked connection state (keep-alive/ack replies ride through here).
async fn apply(conn: &mut Connection<TcpStream>, state: &mut ConnectionState, directive: Directive) {
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

/// Parse a `data get entity ... Pos` RCON response's `[x, y, z]` list.
fn parse_list3(resp: &str) -> Option<(f64, f64, f64)> {
    let open = resp.find('[')?;
    let close = resp[open..].find(']')? + open;
    let inner = &resp[open + 1..close];
    let nums: Vec<f64> = inner
        .split(',')
        .filter_map(|s| s.trim().trim_end_matches('d').parse::<f64>().ok())
        .collect();
    (nums.len() == 3).then(|| (nums[0], nums[1], nums[2]))
}

/// The light-section index and section-local coordinates of world cell
/// `(wx, wy, wz)` within `column`. Light section 0 is the below-world section
/// (`minLightSection = minSection - 1`), so a block section maps to light
/// section `block_section + 1`.
fn light_cell(column: &ChunkColumn, wx: i32, wy: i32, wz: i32) -> (usize, usize, usize, usize) {
    let min_y = column.min_y();
    let block_sec = (wy - min_y).div_euclid(16) as usize;
    let light_sec = block_sec + 1;
    let ly = (wy - min_y).rem_euclid(16) as usize;
    let lx = wx.rem_euclid(16) as usize;
    let lz = wz.rem_euclid(16) as usize;
    (light_sec, lx, ly, lz)
}

/// Cell-by-cell block-light disagreement count over the light sections in
/// `band`, skipping sections the server elided (`Missing`) so an absent section
/// is never counted as a `0`-vs-value mismatch. Returns
/// `(cells_compared, disagreements, max_server_block_light)`.
///
/// Restricting to a vertical band around the source keeps the judgement on the
/// pure-air region a single in-column point source lights exactly — where our
/// engine and the server must agree with no neighbour-chunk contribution — and
/// away from any terrain block light a neighbour chunk (unseen by our
/// single-column engine) might cast near the world surface.
fn band_block_diff(
    ours: &ColumnLight,
    server: &ColumnLight,
    band: std::ops::RangeInclusive<usize>,
) -> (usize, usize, u8) {
    let mut compared = 0usize;
    let mut disagreements = 0usize;
    let mut max_server = 0u8;
    for i in band {
        if i >= server.light_section_count() || i >= ours.light_section_count() {
            continue;
        }
        if matches!(server.block(i), LightData::Missing) {
            continue;
        }
        let os = ours.section_light(i);
        let ss = server.section_light(i);
        for y in 0..16usize {
            for z in 0..16usize {
                for x in 0..16usize {
                    let sv = ss.block_at(x, y, z);
                    let ov = os.block_at(x, y, z);
                    max_server = max_server.max(sv);
                    compared += 1;
                    if ov != sv {
                        disagreements += 1;
                    }
                }
            }
        }
    }
    (compared, disagreements, max_server)
}

#[tokio::test]
#[ignore = "requires the live vanilla-26.2 RCON oracle on :25567 (+ RCON :25575)"]
async fn computed_block_light_matches_server_oracle_around_a_placed_source() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25567,
    };
    let profile = LoginProfile {
        // Unique per run: in offline mode a shared name is a mutual eviction that
        // presents as a silent chunk blackout while login and keep-alives look
        // healthy (§7 trap).
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V770Adapter::new();
    let mut world = World::new();

    // FAIL, never skip, when the oracle is unreachable (§12.52).
    let mut conn = Connection::connect(GAME_ADDR)
        .await
        .expect("connect to the vanilla-26.2 oracle on :25567 (gate fails, never skips)");
    let mut state = ConnectionState::Handshaking;
    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    // --- Phase 1: reach Play and let the player's neighbourhood stream in. ----
    let read_timeout = Duration::from_secs(5);
    let mut chunks = 0usize;
    let mut first_chunk_at: Option<Instant> = None;
    let collect_window = Duration::from_secs(8);
    let _ = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Some(t) = first_chunk_at
                && (chunks >= 64 || t.elapsed() >= collect_window)
            {
                break;
            }
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let packet = match read {
                Err(_) => break,
                Ok(Ok(Some(p))) => p,
                Ok(Ok(None)) => break,
                Ok(Err(err)) => panic!("read error: {err}"),
            };
            let (packet_id, payload) = packet;
            let directives = adapter
                .handle_packet(&mut world, state, packet_id, &payload)
                .unwrap_or_default();
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

    assert_eq!(
        state,
        ConnectionState::Play,
        "never reached Play on the oracle {GAME_ADDR} — login/connection fault, not the light path"
    );
    assert!(
        chunks > 0,
        "the oracle streamed no chunks; without a loaded column there is nothing to light"
    );

    // --- Phase 2: place a known light source in mid-air over RCON. -----------
    // v770 does not emit TeleportPlayer, so the read-model's position never
    // populates on 26.2 — RCON is the only way to learn where the server put us.
    let mut rcon = RconClient::connect(RCON_ADDR, RCON_PASSWORD).expect(
        "oracle RCON reachable/authenticated at :25575 — is the vanilla-26.2 oracle up? \
         A missing RCON is a harness failure, not a passing light path.",
    );
    let pos = rcon.cmd("data get entity @p Pos");
    let (px, py, pz) =
        parse_list3(&pos).expect("player Pos readable via RCON after join — otherwise no spawn");

    let gx = px.floor() as i32;
    let gz = pz.floor() as i32;
    let gy = py.floor() as i32 + SOURCE_ELEVATION;
    let cx = gx >> 4;
    let cz = gz >> 4;

    // Force-load the column so it keeps ticking (and relighting) while we watch,
    // clear any stale source from a prior run, then place a glowstone (emission
    // 15) high in the air above the player.
    rcon.cmd(&format!("forceload add {gx} {gz}"));
    rcon.cmd(&format!("setblock {gx} {gy} {gz} minecraft:air"));
    let placed = rcon.cmd(&format!("setblock {gx} {gy} {gz} minecraft:glowstone"));
    eprintln!("placed source: setblock {gx} {gy} {gz} minecraft:glowstone -> {}", placed.trim());

    // --- Phase 3: pump packets until the server relights and sends it back. ---
    // The block update lands the glowstone in our column; a follow-up
    // light_update carries the server's recomputed block light. They can arrive a
    // tick apart, so poll until BOTH are visible at the source cell.
    let lx = gx.rem_euclid(16) as usize;
    let lz = gz.rem_euclid(16) as usize;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut lit = false;
    while Instant::now() < deadline {
        let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
        let packet = match read {
            Err(_) => continue,
            Ok(Ok(Some(p))) => p,
            Ok(Ok(None)) => break,
            Ok(Err(err)) => panic!("read error: {err}"),
        };
        let (packet_id, payload) = packet;
        let directives = adapter
            .handle_packet(&mut world, state, packet_id, &payload)
            .unwrap_or_default();
        for directive in directives {
            if let Directive::Emit(_) = directive {
                // notifications only
            } else {
                apply(&mut conn, &mut state, directive).await;
            }
        }
        if let Some(loaded) = world.get(WorldChunkPos::new(cx, cz)) {
            let block_present = loaded.column.get_block(lx, gy, lz) != 0;
            let (light_sec, slx, sly, slz) = light_cell(&loaded.column, gx, gy, gz);
            let server_lit = light_sec < loaded.light.light_section_count()
                && loaded.light.section_light(light_sec).block_at(slx, sly, slz) >= 14;
            if block_present && server_lit {
                lit = true;
                break;
            }
        }
    }
    assert!(
        lit,
        "timed out waiting for the server's recomputed block light to reach the source cell \
         at ({gx}, {gy}, {gz}) in chunk ({cx}, {cz}) — the setblock or the light_update path \
         did not complete (gate fails, never skips)"
    );

    // --- Judge our engine against the server's recomputed block light. -------
    let loaded = world
        .get(WorldChunkPos::new(cx, cz))
        .expect("source column present in world");
    let props = V770LightProps::load();
    let ours = compute_column_light(&loaded.column, &props);

    let (light_sec, _, _, _) = light_cell(&loaded.column, gx, gy, gz);
    let lo = light_sec.saturating_sub(1);
    let hi = (light_sec + 1).min(loaded.light.light_section_count().saturating_sub(1));
    let (compared, disagreements, max_server) = band_block_diff(&ours, &loaded.light, lo..=hi);

    // Full-column numbers for context (informational — terrain block light from
    // unseen neighbour chunks near the surface can legitimately differ for a
    // single-column engine; the hard judgement is the mid-air band above).
    let full = diff_column_light(&ours, &loaded.light, 0);
    println!(
        "block-light oracle @ chunk ({cx}, {cz}) source ({gx}, {gy}, {gz}):\n\
         band light sections {lo}..={hi}: {disagreements} of {compared} block cells differ \
         (max server block light in band = {max_server})\n\
         full column via diff_column_light: sky {} / block {} of {} cells differ",
        full.sky_disagreements, full.block_disagreements, full.cells_compared
    );

    // Non-vacuity: the server must actually have lit the source, or "0 differ"
    // would mean "nothing was compared".
    assert!(
        compared > 0,
        "band compared zero cells — the server elided every light section around the source"
    );
    assert!(
        max_server >= 14,
        "server block light around the source peaks at {max_server} (<14): the glowstone did \
         not light the column, so this comparison would be vacuous"
    );
    assert_eq!(
        disagreements, 0,
        "our light engine disagrees with the live server's block light around the source: \
         {disagreements} of {compared} cells (band light sections {lo}..={hi}). The count and \
         its position tell impl-world whether this is the seed-vs-decay (emission) path or a \
         section seam."
    );

    eprintln!("\n=== LIVE BLOCK-LIGHT ORACLE (emission/decay path) ===");
    eprintln!("oracle                    : {GAME_ADDR} (RCON {RCON_ADDR})");
    eprintln!("source                    : glowstone @ ({gx}, {gy}, {gz}) chunk ({cx}, {cz})");
    eprintln!("band light sections       : {lo}..={hi}");
    eprintln!("block cells compared      : {compared} (0 disagreements)");
    eprintln!("max server block light    : {max_server}");
    eprintln!("props source              : vendor/minecraft-data 1.21.11 blocks.json");
    eprintln!("=====================================================\n");

    // Best-effort cleanup so repeated runs start clean; failure here is harmless.
    let _ = rcon.command(&format!("setblock {gx} {gy} {gz} minecraft:air"));
    let _ = rcon.command(&format!("forceload remove {gx} {gz}"));
}
