//! `lodestone-server` — a standalone, headless dedicated server.
//!
//! Drop this binary into a directory, run it, and it hosts a real world over
//! TCP: `server.properties` and `eula.txt` are read (and, on a first run,
//! written with vanilla's own defaults) from that directory, exactly as
//! vanilla's own `server.jar` does. See `docs/dedicated-server.md` for the
//! full picture — this file is orchestration only; every real decision (what
//! a key means, what the EULA gate does, how a console command runs) lives in
//! `lodestone-server`'s own `properties`/`eula`/`console` modules and is
//! exercised by their tests, not this crate's.
//!
//! # Usage
//!
//! ```text
//! lodestone-server [directory]
//! ```
//!
//! `directory` defaults to the current directory. It is created if missing.

use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;

use lodestone_server::AccessHandle;
use lodestone_server::CommandDispatch;
use lodestone_server::IntegratedServer;
use lodestone_server::OnlineModeConfig;
use lodestone_server::PublishConfig;
use lodestone_server::RconConfig;
use lodestone_server::ServerProperties;
use lodestone_server::WorldType;
use lodestone_server::dimension::Dimension;
use lodestone_server::{eula, parse_seed};

/// How often the world autosaves while running. Vanilla has no
/// `server.properties` key for this (its own autosave cadence is
/// hard-coded), so this crate has none either — the same value
/// `lodestone-shell`'s own singleplayer autosave already uses
/// (`AUTOSAVE_INTERVAL` in `crates/lodestone-shell/src/net.rs`), reused
/// rather than picked fresh so the two hosting paths agree on how much work
/// a clean shutdown can lose at most.
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(30);

/// Clamp on the tick/mob-simulation radius derived from `simulation-distance`
/// (see [`sim_radius`]) — **measured, not guessed**: a fresh world with
/// vanilla's own real default (`simulation-distance=10`, a 21×21 = 441-column
/// area) was tried first, unclamped, against this crate's debug build, and
/// the tick loop fell behind without recovering — "Can't keep up!" at 143
/// ticks behind within 10 seconds of boot, climbing to 1591 (79.5s) before
/// the process was killed. `crate::integrated`'s own `LAN_TICK_RADIUS`
/// constant already documents exactly this cost ("widening it costs a full
/// generator run per chunk per tick") and keeps LAN hosting at radius 2 (25
/// columns) for that reason — this mirrors that established, load-tested
/// number rather than trusting vanilla's `simulation-distance` default
/// against a tick-loop architecture that (by that same constant's own doc,
/// issue #289) has no loaded-chunk ticket-driven set yet. `sim_radius` only
/// clamps **down** from here — an operator's smaller `simulation-distance`
/// is honoured, a larger one is not, and `docs/dedicated-server.md` says so.
const MAX_SIM_RADIUS: i32 = 2;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let dir = server_directory();
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::error!("could not create server directory {}: {err}", dir.display());
        std::process::exit(1);
    }

    // The EULA gate. Same shape as vanilla: absent or `eula=false` writes the
    // file (if missing) and refuses to start, `eula=true` proceeds. See
    // `lodestone_server::eula`'s own doc comment for why the wording is a
    // constant an operator (or this project's owner) fills in, not something
    // decided here.
    match eula::check(&dir.join("eula.txt")) {
        Ok(true) => {}
        Ok(false) => {
            tracing::error!(
                "You need to agree to the EULA in order to run the server. \
                 Go to eula.txt for more info."
            );
            std::process::exit(1);
        }
        Err(err) => {
            tracing::error!("could not read or write eula.txt: {err}");
            std::process::exit(1);
        }
    }

    let (props, created) = match ServerProperties::load_or_create(&dir.join("server.properties")) {
        Ok(pair) => pair,
        Err(err) => {
            tracing::error!("could not read or write server.properties: {err}");
            std::process::exit(1);
        }
    };
    if created {
        tracing::info!("wrote a fresh server.properties with vanilla's own defaults");
    }

    // Access control (issue #336's four JSON files) lives at the server
    // root, next to server.properties/eula.txt — vanilla's own layout, not
    // inside the world save directory. A malformed file is a real error
    // (unlike a missing one, which is an empty list); refusing to start on
    // one is the safe direction, since starting with silently-ignored bans
    // would be the wrong failure mode for exactly the file this exists to
    // enforce.
    let access = match AccessHandle::load(&dir) {
        Ok(access) => access,
        Err(err) => {
            tracing::error!("could not read ops/whitelist/ban lists: {err}");
            std::process::exit(1);
        }
    };
    access.set_whitelist_enabled(props.white_list);
    access.with(|lists| lists.set_max_players(Some(props.max_players.max(0) as usize)));

    let seed = parse_seed(&props.level_seed).unwrap_or_else(random_seed);
    let world_type = level_type_to_world_type(&props.level_type);
    let source = lodestone_server::overworld_chunk_source_of_type(seed, world_type);
    let world_dir = dir.join(&props.level_name);

    let radius = sim_radius(props.simulation_distance);
    let mob_area = (-radius..=radius, -radius..=radius);
    let mob_center = (0, 0);

    let protocol = lodestone_v770::V770ServerProtocol;
    let (mut server, client_end, _world) = match IntegratedServer::open_persistent_with_mobs(
        protocol,
        &world_dir,
        source,
        Dimension::Overworld.min_y(),
        Dimension::Overworld.height(),
        mob_area,
        mob_center,
        0, // no demo-mob fixture ring — see `IntegratedServer::demo_mob_count`'s
        // own doc for why that ring was always a development fixture, never
        // real spawning; real mob spawning runs from the tick loop
        // regardless of this argument.
        props.view_distance,
        AUTOSAVE_INTERVAL,
    ) {
        Ok(triple) => triple,
        Err(err) => {
            tracing::error!("could not open world {}: {err}", world_dir.display());
            std::process::exit(1);
        }
    };
    // Headless: there is no local player. `open_persistent_with_mobs` always
    // returns an in-memory duplex "local connection" end because it is also
    // singleplayer's own constructor — dropping the client half here (rather
    // than handing it to anything) makes that connection's server-side task
    // observe an immediate EOF and return, exactly like any other client
    // that connects and disconnects before `LoginStart`. No player ever
    // joins through it, so nothing is lost by not reading from it.
    drop(client_end);

    // Issues #327/#328: the default mode/difficulty a fresh world starts
    // with. Set *before* `publish_with_config` opens the listener, so no
    // connection can join ahead of it.
    server
        .world_state()
        .set_default_game_mode(props.gamemode);
    server.world_state().set_difficulty(props.difficulty);

    let online_mode = if props.online_mode {
        // The same crypto-provider install `lodestone-auth`'s own login path
        // requires before the first `reqwest` TLS call — see that crate's
        // `tls` module. Idempotent; safe to call even if nothing ever
        // authenticates.
        lodestone_auth::install_crypto_provider();
        Some(OnlineModeConfig::new(reqwest::Client::new()))
    } else {
        None
    };

    let bind_ip = if props.server_ip.is_empty() {
        "0.0.0.0".to_string()
    } else {
        props.server_ip.clone()
    };
    let addr = (bind_ip.as_str(), props.server_port);
    let publish_config = PublishConfig {
        access: access.clone(),
        commands: CommandDispatch::none(),
        online_mode,
    };
    let bound = match server.publish_with_config(addr, None, publish_config).await {
        Ok(bound) => bound,
        Err(err) => {
            tracing::error!("could not bind {}:{}: {err}", bind_ip, props.server_port);
            std::process::exit(1);
        }
    };
    tracing::info!("Done! Listening on {bound}");

    if props.enable_rcon {
        if props.rcon_password.is_empty() {
            tracing::warn!("enable-rcon=true but rcon.password is empty; RCON left disabled");
        } else {
            let rcon_addr = std::net::SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
                props.rcon_port,
            );
            let players = server.players().cloned();
            let config = RconConfig::new(rcon_addr, props.rcon_password.clone(), CommandDispatch::none())
                .with_world(server.world_state().clone(), players);
            match server.start_rcon(config) {
                Ok(addr) => tracing::info!("RCON listening on {addr}"),
                Err(err) => tracing::warn!("RCON failed to bind {rcon_addr}: {err}"),
            }
        }
    }

    run_until_shutdown(server).await;
}

/// Reads stdin lines as console commands and races them against SIGINT/
/// SIGTERM, both of which — like a `stop` typed at the console — flush and
/// close the world through [`IntegratedServer::shutdown`] rather than exiting
/// the process out from under it.
async fn run_until_shutdown(server: IntegratedServer) {
    let world_state = server.world_state().clone();
    let players = server.players().cloned();
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();

    // SIGTERM has no portable `tokio::signal` equivalent — `ctrl_c()` alone
    // answers SIGINT, but a supervisor (systemd, Docker, a hosting panel's
    // "stop server" button) sends SIGTERM, not a synthetic Ctrl-C, and a
    // dedicated server that only reacted to the terminal signal would lose
    // its world under exactly the stop mechanism a hosting panel actually
    // uses. `tokio::signal::unix` is Unix-only by construction (there is no
    // SIGTERM on Windows to listen for) — this binary targets the platforms a
    // dedicated server actually runs on (Linux/macOS hosting, matching this
    // repo's own dev platform), and does not build for wasm32 regardless
    // (see this crate's own `Cargo.toml`).
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to install SIGTERM handler");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received Ctrl-C, saving and stopping");
                break;
            },
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, saving and stopping");
                break;
            },
            line = lines.next_line() => {
                match line {
                    Ok(Some(command)) => {
                        let trimmed = command.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if trimmed.eq_ignore_ascii_case("stop") {
                            tracing::info!("stop command received, saving and stopping");
                            break;
                        }
                        let response = lodestone_server::console::run(&world_state, players.as_ref(), trimmed);
                        if !response.is_empty() {
                            println!("{response}");
                        }
                    }
                    // stdin closed (e.g. running under a supervisor with no
                    // console attached) — keep serving; only a signal or an
                    // explicit `stop` ends the loop from here on.
                    Ok(None) => {
                        std::future::pending::<()>().await;
                    }
                    Err(err) => {
                        tracing::warn!("console stdin error: {err}");
                        std::future::pending::<()>().await;
                    }
                }
            },
        }
    }

    server.shutdown().await;
    tracing::info!("world saved, server stopped");
}

/// The directory to serve, from `argv[1]`, defaulting to the current
/// directory — matching vanilla's own `server.jar` (run it from wherever the
/// world should live).
fn server_directory() -> PathBuf {
    std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `WorldOptions.randomSeed()`'s own shape: a fresh, opaque `i64` with no
/// algorithm callers may rely on. `RandomState`'s per-process random key
/// (rather than a clock read) — this binary is native-only anyway, but
/// matching `lodestone-shell`'s own `random_seed` keeps the two hosting paths
/// picking a random seed the identical, deliberate way.
fn random_seed() -> i64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    hasher.finish() as i64
}

/// `level-type` → [`WorldType`]. `minecraft:normal`/`default` (and no value
/// at all) is [`WorldType::Overworld`]; `minecraft:large_biomes`/
/// `largebiomes` and `minecraft:amplified` map directly. Anything else —
/// **including `minecraft:flat`**, which needs `generator-settings`' JSON
/// this crate's properties reader does not parse — falls back to normal with
/// a warning: see `docs/dedicated-server.md`'s accepted-and-ignored table.
fn level_type_to_world_type(level_type: &str) -> WorldType {
    match level_type.trim().to_ascii_lowercase().as_str() {
        "minecraft:normal" | "default" | "normal" | "" => WorldType::Overworld,
        "minecraft:large_biomes" | "largebiomes" | "large_biomes" => WorldType::LargeBiomes,
        "minecraft:amplified" | "amplified" => WorldType::Amplified,
        other => {
            tracing::warn!(
                "level-type={other:?} is not implemented by this crate's worldgen \
                 (flat/single-biome/debug presets are not read from server.properties yet); \
                 falling back to a normal overworld"
            );
            WorldType::Overworld
        }
    }
}

/// `simulation-distance` → the tick/mob-simulation area radius, clamped to
/// [`MAX_SIM_RADIUS`]. There is no dedicated per-config knob for this in
/// `lodestone-server`'s constructors today (`open_to_lan`'s own
/// `LAN_TICK_RADIUS` is a fixed constant, by that module's own admission
/// pending a real loaded-chunk registry — issue #289) — but
/// `open_persistent_with_mobs` already takes the tick area as a plain
/// argument, so this is a real, direct use of the config value rather than
/// an accepted-and-ignored one.
fn sim_radius(simulation_distance: i32) -> i32 {
    simulation_distance.clamp(0, MAX_SIM_RADIUS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_type_mapping_matches_vanillas_real_identifiers() {
        assert_eq!(level_type_to_world_type("minecraft:normal"), WorldType::Overworld);
        assert_eq!(level_type_to_world_type(""), WorldType::Overworld);
        assert_eq!(level_type_to_world_type("minecraft:large_biomes"), WorldType::LargeBiomes);
        assert_eq!(level_type_to_world_type("minecraft:amplified"), WorldType::Amplified);
        // The discriminating case: an unimplemented preset must degrade to
        // normal, not panic or silently pick the wrong real type.
        assert_eq!(level_type_to_world_type("minecraft:flat"), WorldType::Overworld);
    }

    #[test]
    fn sim_radius_clamps_a_runaway_config_value() {
        // Vanilla's own real default, unclamped, is exactly what overloaded
        // the tick loop in the measurement `MAX_SIM_RADIUS`'s own doc
        // comment records — so the discriminating assertion here is that
        // *that* default gets clamped down, not passed through.
        assert_eq!(sim_radius(10), MAX_SIM_RADIUS);
        assert_eq!(sim_radius(0), 0);
        assert_eq!(sim_radius(1), 1);
        assert_eq!(sim_radius(-5), 0);
        assert_eq!(sim_radius(1000), MAX_SIM_RADIUS);
    }
}
