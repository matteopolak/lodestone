//! Finite release-profile input for the real integrated singleplayer join.
//!
//! The binary drives the same `NetClient::open_singleplayer` path as the
//! windowed shell, then stops after a fixed view square is resident. Its one
//! JSON record is deliberately aggregate-only so it is useful both as a
//! repeatable timing control and as a Samply workload.

#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashSet;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use lodestone_time::Instant;

#[cfg(not(target_arch = "wasm32"))]
use lodestone::net::{NetClient, NetUpdate};
#[cfg(not(target_arch = "wasm32"))]
use lodestone_client::ChunkPos;

#[cfg(not(target_arch = "wasm32"))]
const MAX_REPORTED_MISSING_VIEW_COORDINATES: usize = 169;

#[cfg(not(target_arch = "wasm32"))]
fn view_center(net: &NetClient) -> ChunkPos {
    net.server_position().map_or(ChunkPos { x: 0, z: 0 }, |position| {
        ChunkPos {
            x: (position.x.floor() as i32).div_euclid(16),
            z: (position.z.floor() as i32).div_euclid(16),
        }
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn missing_view_coordinates(
    net: &NetClient,
    center: ChunkPos,
    radius: i32,
) -> (Vec<ChunkPos>, usize, bool) {
    let loaded: HashSet<_> = net.loaded_chunks().into_iter().collect();
    let mut missing = Vec::new();
    let mut missing_count = 0usize;
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            let coordinate = ChunkPos {
                x: center.x + dx,
                z: center.z + dz,
            };
            if loaded.contains(&coordinate) {
                continue;
            }
            missing_count += 1;
            if missing.len() < MAX_REPORTED_MISSING_VIEW_COORDINATES {
                missing.push(coordinate);
            }
        }
    }
    let truncated = missing_count > missing.len();
    (missing, missing_count, truncated)
}

#[cfg(not(target_arch = "wasm32"))]
fn init_diagnostic_logging() {
    let diagnostics_requested = std::env::var_os("RUST_LOG").is_some()
        || std::env::var_os("LODESTONE_JOIN_TRACE").is_some()
        || std::env::var_os("LODESTONE_WORLDGEN_LEDGER_TRACE").is_some();
    if !diagnostics_requested {
        return;
    }
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,lodestone_join_trace=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(not(target_arch = "wasm32"))]
fn parse<T: std::str::FromStr>(args: &mut impl Iterator<Item = String>, name: &str, default: T) -> T {
    args.next()
        .map(|value| value.parse().unwrap_or_else(|_| panic!("invalid {name}: {value}")))
        .unwrap_or(default)
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    init_diagnostic_logging();
    let mut args = std::env::args().skip(1);
    let seed: i64 = parse(&mut args, "seed", 4242);
    let radius: i32 = parse(&mut args, "view radius", 1);
    let deadline_seconds: u64 = parse(&mut args, "deadline seconds", 240);
    assert!((0..=32).contains(&radius), "view radius must be between 0 and 32");
    assert!(deadline_seconds > 0, "deadline seconds must be positive");

    let protocol = lodestone::Config::default().protocol;
    let server_protocol = lodestone_registry::server_protocol_for_protocol(protocol)
        .unwrap_or_else(|| panic!("no server protocol for {protocol}"));
    let expected = ((radius * 2 + 1) as usize).saturating_pow(2);
    let target = ChunkPos { x: 0, z: 0 };
    let world_dir = std::env::var_os("LODESTONE_JOIN_PROFILE_WORLD_DIR")
        .map(std::path::PathBuf::from);
    let spawn_before = lodestone_server::spawn_search_metrics();
    let started = Instant::now();
    let net = NetClient::open_singleplayer(
        server_protocol,
        protocol,
        seed,
        lodestone::menu::create_world::WorldTypePreset::Normal,
        radius,
        false,
        None,
        world_dir,
    );
    let returned = started.elapsed();
    let deadline = started + Duration::from_secs(deadline_seconds);
    let mut connecting = None;
    let mut joining = None;
    let mut logged_in = None;
    let mut first_chunk_event = None;
    let mut latest_chunk_event = None;
    let mut first_resident = None;
    let mut spawn_published = None;
    let mut all_resident = None;
    let mut dimensions = None;
    let mut errors = Vec::new();
    let mut update_count = 0usize;
    let mut chunk_events = 0usize;
    let mut poll_count = 0usize;
    let mut nonempty_poll_count = 0usize;
    let mut max_resident = 0usize;

    while Instant::now() < deadline {
        poll_count += 1;
        let updates = net.poll();
        if !updates.is_empty() {
            nonempty_poll_count += 1;
        }
        update_count += updates.len();
        for update in updates {
            let elapsed = started.elapsed();
            match update {
                NetUpdate::Connecting => {
                    connecting.get_or_insert(elapsed);
                }
                NetUpdate::ConnectPhase(phase) => match phase {
                    lodestone::menu::loading::ConnectPhase::Connecting => {
                        connecting.get_or_insert(elapsed);
                    }
                    lodestone::menu::loading::ConnectPhase::Joining => {
                        joining.get_or_insert(elapsed);
                    }
                    lodestone::menu::loading::ConnectPhase::LoadingTerrain => {}
                },
                NetUpdate::LoggedIn { .. } => {
                    logged_in.get_or_insert(elapsed);
                }
                NetUpdate::Chunk { .. } => {
                    chunk_events += 1;
                    first_chunk_event.get_or_insert(elapsed);
                    latest_chunk_event = Some(elapsed);
                }
                NetUpdate::Error(error) => errors.push(error),
                NetUpdate::Disconnected(reason) => {
                    errors.push(format!("disconnected: {reason:?}"));
                }
                _ => {}
            }
        }

        let resident = net.loaded_chunks().len();
        max_resident = max_resident.max(resident);
        if resident > 0 {
            first_resident.get_or_insert(started.elapsed());
        }
        if net.server_position().is_some() {
            spawn_published.get_or_insert(started.elapsed());
        }
        if resident >= expected && net.is_chunk_loaded(target) {
            all_resident.get_or_insert(started.elapsed());
            dimensions = net.world_dimensions();
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let finished = started.elapsed();
    let timed_out = all_resident.is_none();
    let final_center = view_center(&net);
    let (missing_coordinates, missing_coordinate_count, missing_coordinates_truncated) =
        missing_view_coordinates(&net, final_center, radius);
    dimensions = dimensions.or_else(|| net.world_dimensions());
    let shutdown_started = Instant::now();
    drop(net);
    let shutdown = shutdown_started.elapsed();
    let spawn_after = lodestone_server::spawn_search_metrics();

    let duration_ms = |value: Option<Duration>| value.map(|duration| duration.as_secs_f64() * 1000.0);
    let between = |start: Option<Duration>, end: Option<Duration>| {
        end.zip(start).map(|(end, start)| end.saturating_sub(start))
    };
    let report = serde_json::json!({
        "schema": "lodestone-integrated-join-profile-v1",
        "seed": seed,
        "view_radius": radius,
        "expected_columns": expected,
        "timed_out": timed_out,
        "errors": errors,
        "session_to_joining_ms": duration_ms(joining),
        "joining_to_login_ms": duration_ms(between(joining, logged_in)),
        "first_chunk_event_ms": duration_ms(first_chunk_event),
        "login_to_first_chunk_ms": duration_ms(between(logged_in, first_chunk_event)),
        "login_to_first_resident_ms": duration_ms(between(logged_in, first_resident)),
        "login_to_spawn_position_ms": duration_ms(between(logged_in, spawn_published)),
        "login_to_all_resident_ms": duration_ms(between(logged_in, all_resident)),
        "total_ms": finished.as_secs_f64() * 1000.0,
        "open_call_ms": returned.as_secs_f64() * 1000.0,
        "shutdown_ms": shutdown.as_secs_f64() * 1000.0,
        "connecting_ms": duration_ms(connecting),
        "joining_phase_ms": duration_ms(joining),
        "logged_in_phase_ms": duration_ms(logged_in),
        "update_count": update_count,
        "chunk_events": chunk_events,
        "latest_chunk_event_ms": duration_ms(latest_chunk_event),
        "view_center": { "x": final_center.x, "z": final_center.z },
        "missing_view_coordinate_count": missing_coordinate_count,
        "missing_view_coordinates": missing_coordinates
            .iter()
            .map(|coordinate| [coordinate.x, coordinate.z])
            .collect::<Vec<_>>(),
        "missing_view_coordinates_truncated": missing_coordinates_truncated,
        "max_resident_columns": max_resident,
        "resident_columns": max_resident,
        "poll_count": poll_count,
        "nonempty_poll_count": nonempty_poll_count,
        "world_dimensions": dimensions.map(|value| serde_json::json!({
            "min_y": value.min_y,
            "height": value.height,
        })),
        "spawn_search": {
            "searches": spawn_after.searches.saturating_sub(spawn_before.searches),
            "candidate_chunks": spawn_after.candidate_chunks.saturating_sub(spawn_before.candidate_chunks),
            "columns_requested": spawn_after.columns_requested.saturating_sub(spawn_before.columns_requested),
            "horizon_samples": spawn_after.horizon_samples.saturating_sub(spawn_before.horizon_samples),
            "accepted": spawn_after.accepted.saturating_sub(spawn_before.accepted),
            "fallbacks": spawn_after.fallbacks.saturating_sub(spawn_before.fallbacks),
            "elapsed_ms": spawn_after.elapsed_nanos.saturating_sub(spawn_before.elapsed_nanos) as f64 / 1_000_000.0,
        },
        "generation_requests": {
            "raw_ensure_calls": spawn_after.raw_ensure_calls.saturating_sub(spawn_before.raw_ensure_calls),
            "request_session_leaders": spawn_after.request_session_leaders.saturating_sub(spawn_before.request_session_leaders),
            "existing_hits": spawn_after.existing_hits.saturating_sub(spawn_before.existing_hits),
            "packet_neighbour_admissions": spawn_after.packet_neighbour_admissions.saturating_sub(spawn_before.packet_neighbour_admissions),
        },
    });
    println!("JOIN_PROFILE {report}");
    if timed_out || !report["errors"].as_array().is_some_and(Vec::is_empty) {
        std::process::exit(1);
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    eprintln!("join-profile is a native profiling harness");
}
