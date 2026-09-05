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
use lodestone_server::ChunkSource;
use lodestone_server::CommandDispatch;
use lodestone_server::IntegratedServer;
use lodestone_server::OnlineModeConfig;
use lodestone_server::PublishConfig;
use lodestone_server::RconConfig;
use lodestone_server::ServerProperties;
use lodestone_server::ServerProtocol;
use lodestone_server::WorldType;
use lodestone_server::dimension::Dimension;
use lodestone_server::ecs::ServerApp;
use lodestone_server::{eula, parse_seed};

#[cfg(feature = "jvm")]
mod java_adapter;

/// How often the world autosaves while running. Vanilla has no
/// `server.properties` key for this (its own autosave cadence is
/// hard-coded), so this crate has none either — the same value
/// `lodestone-shell`'s own singleplayer autosave already uses
/// (`AUTOSAVE_INTERVAL` in `crates/lodestone-shell/src/net.rs`), reused
/// rather than picked fresh so the two hosting paths agree on how much work
/// a clean shutdown can lose at most.
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(30);

/// Configured fallback/no-anchor and mob-simulation radius for dedicated
/// hosting. Radius 2 covers 25 columns for initial mob seeding and for world
/// ticks before a player anchor is published; connected tick-follow may use
/// its separate radius-3 area. The shared `ChunkStore` retains resident
/// columns, so this bound mainly controls initial/warm-up generation and
/// fallback scan width, not regeneration on every tick. [`sim_radius`] clamps
/// inputs into `0..=MAX_SIM_RADIUS`: values already in that range are
/// preserved, and negative values become 0.
const MAX_SIM_RADIUS: i32 = 2;

/// Builds the application whose `World` the dedicated server's primary tick
/// task owns. Compiled-in native server plugins are registered here with
/// [`ServerApp::bootstrap_with`]; the default binary deliberately installs no
/// optional plugin.
fn dedicated_server_app() -> ServerApp {
    ServerApp::bootstrap()
}

/// Opens the persistent world through the same application-injection leaf an
/// embedding host uses. Keeping this orchestration in one function makes the
/// binary's registration point independently testable without binding its TCP
/// listener or accepting the EULA.
#[allow(clippy::too_many_arguments)]
fn open_persistent_server<P, S>(
    protocol: P,
    world_dir: &std::path::Path,
    source: S,
    mob_area: (std::ops::RangeInclusive<i32>, std::ops::RangeInclusive<i32>),
    mob_center: (i32, i32),
    view_radius: i32,
    server_app: ServerApp,
) -> Result<
    (
        IntegratedServer,
        tokio::io::DuplexStream,
        lodestone_server::region_source::RegionChunkSource<S>,
    ),
    lodestone_server::region_source::Error,
>
where
    P: ServerProtocol + 'static,
    S: ChunkSource + 'static,
{
    IntegratedServer::open_persistent_with_mobs_and_commands_and_server_app(
        protocol,
        world_dir,
        source,
        Dimension::Overworld.min_y(),
        Dimension::Overworld.height(),
        mob_area,
        mob_center,
        // No demo-mob fixture ring. Real mob spawning is driven by the world
        // tick independently of this development-only seed count.
        0,
        view_radius,
        AUTOSAVE_INTERVAL,
        CommandDispatch::none(),
        server_app,
    )
}

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

    // Access-control lists live at the server root beside the properties and
    // EULA files, not inside a world save. Missing lists mean no entries, but
    // malformed lists abort startup so bans, whitelist entries, and operator
    // records can never be silently ignored.
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

    // The family to host, asked for by protocol number rather than named. The
    // registry is the one crate allowed to know which families are compiled
    // in (see this crate's `Cargo.toml`), so "what can this build serve?" is
    // the union of `supported_protocols` filtered by which of them
    // `server_protocol_for_protocol` actually answers for — a family whose
    // *client* adapter is compiled in need not implement `ServerProtocol`.
    // Highest wins, so adding a newer servable family here needs no code
    // change.
    //
    // `None` is a real product state, not an error to route around: a build
    // with no servable family compiled in cannot host anything, and must say
    // so rather than starting and refusing every join.
    let Some((protocol_version, protocol)) = lodestone_registry::supported_protocols()
        .into_iter()
        .filter_map(|version| {
            lodestone_registry::server_protocol_for_protocol(version)
                .map(|protocol| (version, protocol))
        })
        .max_by_key(|(version, _)| *version)
    else {
        tracing::error!(
            "this build has no protocol family that can be hosted (client families compiled in: {:?}, \
             servable: {:?}) — rebuild with a servable version feature enabled",
            lodestone_registry::compiled_families(),
            lodestone_registry::compiled_server_families(),
        );
        std::process::exit(1);
    };
    tracing::info!(
        "hosting protocol {protocol_version} ({:?})",
        lodestone_registry::compiled_server_families()
    );
    let (mut server, client_end, _world) = match open_persistent_server(
        protocol,
        &world_dir,
        source,
        mob_area,
        mob_center,
        props.view_distance,
        dedicated_server_app(),
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

    // Configure the default game mode and difficulty before
    // `publish_with_config` opens the listener. This makes the initial world
    // state deterministic for every connection accepted by the server.
    server
        .world_state()
        .set_default_game_mode(props.gamemode);
    server.world_state().set_difficulty(props.difficulty);
    // Same "set before `publish_with_config` opens the listener"
    // ordering as the two calls above — see `lodestone_server::chat_session`'s
    // module doc for what this flag does and does not verify, and
    // `ServerProperties`'s own doc comment for why its default here is `false`
    // rather than vanilla's real `true`.
    if let Some(players) = server.players() {
        players.set_enforce_secure_profile(props.enforce_secure_profile);
    }

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
    #[cfg(not(feature = "jvm"))]
    if [
        "LODESTONE_JAVA_ADAPTER_CLASS",
        "LODESTONE_JAVA_CLASSPATH",
        "LODESTONE_JAVA_DEADLINE_MS",
        "LODESTONE_PAPER_JAR",
        "LODESTONE_PAPER_PLUGIN_DIRECTORY",
        "LODESTONE_PAPER_SHIM_PATH",
    ]
        .iter().any(|key| std::env::var_os(key).is_some())
    {
        tracing::error!("Java adapter configuration requires a build with --features jvm; saving and stopping");
        server.shutdown().await;
        return;
    }
    #[cfg(feature = "jvm")]
    let mut java_adapter = match java_adapter::JavaAdapter::from_environment() {
        Ok(adapter) => adapter,
        Err(error) => {
            tracing::error!(%error, "invalid experimental Java adapter configuration; saving and stopping");
            server.shutdown().await;
            return;
        }
    };
    #[cfg(feature = "jvm")]
    let mut java_poll = java_adapter.as_ref().map(|_| {
        let mut interval = tokio::time::interval(Duration::from_millis(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval
    });
    let world_state = server.world_state().clone();
    let players = server.players().cloned();
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut stdin_open = true;

    // SIGTERM has no portable `tokio::signal` equivalent — `ctrl_c()` alone
    // answers SIGINT, but a supervisor (systemd, Docker, a hosting panel's
    // "stop server" button) sends SIGTERM, not a synthetic Ctrl-C, and a
    // dedicated server that only reacted to the terminal signal would lose
    // its world under exactly the stop mechanism a hosting panel actually
    // uses.
    //
    // `tokio::signal::unix` is `#![cfg(unix)]` **inside tokio**, so naming it
    // unconditionally is not a portability wart that degrades on Windows — it
    // is a hard `E0433: cannot find `unix` in `signal`` that fails the whole
    // build there, and it did: the `check (windows-latest)` CI leg was red on
    // this alone while macOS and Linux were green. `cargo check` on the dev
    // machine structurally cannot see it.
    //
    // The Windows arm is `ctrl_shutdown`, which is the genuine analogue rather
    // than a stub: the OS raises it when the machine is shutting down, the same
    // "you are about to be stopped, flush now" event a supervisor's SIGTERM is.
    // What Windows does *not* have is a way for one process to send it to
    // another, so `taskkill /F` still cannot be made graceful — that is an OS
    // property, not something this loop can fix.
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to install SIGTERM handler");
    #[cfg(windows)]
    let mut terminate =
        tokio::signal::windows::ctrl_shutdown().expect("failed to install CTRL_SHUTDOWN handler");

    loop {
        tokio::select! {
            _ = async {
                #[cfg(feature = "jvm")]
                if let Some(interval) = java_poll.as_mut() {
                    interval.tick().await;
                    return;
                }
                std::future::pending::<()>().await;
            } => {
                #[cfg(feature = "jvm")]
                if let Some(adapter) = java_adapter.as_mut() {
                    if let Err(error) = adapter.poll(&server) {
                        if adapter.requires_paper_bootstrap() {
                            tracing::error!(%error, "configured Paper bootstrap failed; saving and stopping");
                            break;
                        } else {
                            tracing::error!(%error, "experimental Java adapter disabled");
                            java_adapter = None;
                            java_poll = None;
                        }
                    }
                }
            },
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received Ctrl-C, saving and stopping");
                break;
            },
            _ = terminate.recv() => {
                tracing::info!("received a termination signal, saving and stopping");
                break;
            },
            command = next_console_line(&mut lines, &mut stdin_open) => {
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
            },
        }
    }

    #[cfg(feature = "jvm")]
    drop(java_adapter);
    server.shutdown().await;
    tracing::info!("world saved, server stopped");
}

/// Reads commands while allowing the caller's other futures to progress at EOF.
async fn next_console_line<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
    open: &mut bool,
) -> String {
    if *open {
        match lines.next_line().await {
            Ok(Some(line)) => return line,
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "console stdin error"),
        }
        *open = false;
    }
    // Suspend this input future, leaving the surrounding select free to poll
    // shutdown signals and adapter work after a supervisor closes stdin.
    std::future::pending().await
}

/// The directory to serve, from `argv[1]`, defaulting to the current directory.
fn server_directory() -> PathBuf {
    std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Vanilla's own random-world-seed generation shape: a fresh, opaque `i64` with no
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

/// Converts `simulation-distance` to the tick and mob-simulation radius,
/// clamped to [`MAX_SIM_RADIUS`]. The resulting value is passed directly to
/// `open_persistent_with_mobs`, so the setting controls the work area rather
/// than being accepted and ignored.
fn sim_radius(simulation_distance: i32) -> i32 {
    simulation_distance.clamp(0, MAX_SIM_RADIUS)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use bevy_app::{App, Plugin};
    use bevy_ecs::resource::Resource;
    use bevy_ecs::schedule::IntoScheduleConfigs;
    use bevy_ecs::system::Res;
    use lodestone_server::ecs::{GameTick, ServerApp, TickSet};
    use lodestone_server::{ChunkColumn, ChunkSource};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn closed_console_preserves_timer_progress() {
        let mut control = tokio::io::BufReader::new(&b"list\n"[..]).lines();
        let mut open = true;
        assert_eq!(next_console_line(&mut control, &mut open).await, "list");
        let mut closed = tokio::io::BufReader::new(&b""[..]).lines();
        tokio::select! {
            _ = next_console_line(&mut closed, &mut open) => panic!("EOF is not a command"),
            _ = tokio::time::sleep(Duration::from_millis(1)) => {}
        }
        assert!(!open);
    }

    #[cfg(feature = "jvm")]
    #[tokio::test]
    #[ignore = "requires JAVA_HOME JDK; runs one JVM against a temporary persistent world"]
    async fn java_adapter_reads_the_running_persistent_world() {
        use std::process::Command;
        let jdk = PathBuf::from(std::env::var_os("JAVA_HOME").expect("JAVA_HOME"));
        let classes = tempfile::tempdir().unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/java/WorldAdapter.java");
        let compile = Command::new(jdk.join("bin/javac"))
            .arg("-d").arg(classes.path()).arg(source).output().unwrap();
        assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
        let temp = tempfile::tempdir().unwrap();
        let protocol = lodestone_registry::server_protocol_for_protocol(776).unwrap();
        let (server, client, _world) = open_persistent_server(protocol, temp.path(), AirWorld,
            (0..=0, 0..=0), (0, 0), 1, dedicated_server_app()).unwrap();
        drop(client);
        let limit = std::time::Instant::now() + Duration::from_secs(10);
        while server.resident_block_state_id(11, 7, 13).is_none() {
            assert!(std::time::Instant::now() < limit, "primary column did not become resident");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(server.resident_block_state_id(11, 7, 13).map(|state| state.raw()), Some(0));
        let mut adapter = java_adapter::JavaAdapter::start(
            lodestone_jvm_bridge::runtime::JvmConfig::new().with_classpath(classes.path()),
            "lodestone.fixture.WorldAdapter", Duration::from_secs(5), None).unwrap();
        let mut completed = false;
        let error = loop {
            assert!(std::time::Instant::now() < limit, "adapter did not finish");
            match adapter.poll(&server) {
                Ok(Some(_)) => completed = true,
                Err(error) => break error,
                Ok(None) => {}
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        };
        assert!(completed, "registered world read never completed: {error}");
        assert!(error.contains("primary-world block unavailable at 1000000,7,1000000"), "{error}");
        assert!(error.contains("onTick(J)V"), "{error}");
        assert_eq!(server.resident_block_state_id(1000000, 7, 1000000), None);
        drop(adapter);
        server.shutdown().await;
    }

    struct AirWorld;

    impl ChunkSource for AirWorld {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 16)
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            self.column(x.div_euclid(16), z.div_euclid(16))
                .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
                .to_string()
        }

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            self.column(x.div_euclid(16), z.div_euclid(16))
                .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
                .to_string()
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
    }

    #[derive(Resource, Clone)]
    struct FixtureTickCount(Arc<AtomicU64>);

    #[derive(Clone)]
    struct CountingPlugin(Arc<AtomicU64>);

    impl Plugin for CountingPlugin {
        fn build(&self, app: &mut App) {
            app.insert_resource(FixtureTickCount(Arc::clone(&self.0)));
            app.add_systems(
                GameTick,
                (|count: Res<FixtureTickCount>| {
                    count.0.fetch_add(1, Ordering::Relaxed);
                })
                .in_set(TickSet::Publish),
            );
        }
    }

    async fn wait_for_completed_ticks(server: &IntegratedServer, expected: u64) {
        for _ in 0..100 {
            if server
                .tick_stats()
                .is_some_and(|stats| stats.tick_count >= expected)
            {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!(
            "persistent tick task did not complete {expected} ticks; completed {}",
            server.tick_stats().map_or(0, |stats| stats.tick_count)
        );
    }

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

    #[tokio::test(start_paused = true)]
    async fn dedicated_scheduler_runs_delayed_work_on_the_persistent_primary_world() {
        use lodestone_server::ecs::ServerTaskScheduler;

        let temp = tempfile::tempdir().expect("temporary server world");
        let observed = Arc::new(AtomicU64::new(0));
        let observer = Arc::clone(&observed);
        let server_app = ServerApp::bootstrap_with(|app| {
            app.insert_resource(FixtureTickCount(observer));
            app.world_mut().resource_mut::<ServerTaskScheduler>()
                .schedule_repeating(2, 3, |world, id| {
                    let count = world.resource::<FixtureTickCount>().0.fetch_add(1, Ordering::Relaxed);
                    if count == 1 {
                        assert!(world.resource_mut::<ServerTaskScheduler>().cancel(id));
                    }
                });
        });
        let protocol = lodestone_registry::server_protocol_for_protocol(776).expect("host protocol");
        let (server, client, _world) = open_persistent_server(
            protocol, temp.path(), AirWorld, (0..=0, 0..=0), (0, 0), 1, server_app,
        ).expect("persistent fixture world");
        drop(client);
        assert_eq!(observed.load(Ordering::Relaxed), 0, "boot must not run scheduled work");
        tokio::task::yield_now().await;
        for (index, expected) in [0, 1, 1, 1, 2, 2, 2, 2].into_iter().enumerate() {
            tokio::time::advance(Duration::from_millis(50)).await;
            wait_for_completed_ticks(&server, index as u64 + 1).await;
            assert_eq!(observed.load(Ordering::Relaxed), expected, "tick {}", index + 1);
        }
        server.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn dedicated_open_runs_a_native_plugin_on_the_persistent_primary_world() {
        const TICKS: u64 = 4;

        let temp = tempfile::tempdir().expect("temporary server world must be created");
        let observed = Arc::new(AtomicU64::new(0));
        let plugin_observed = Arc::clone(&observed);
        let server_app = ServerApp::bootstrap_with(|app| {
            app.add_plugins(CountingPlugin(plugin_observed));
        });
        let protocol = lodestone_registry::server_protocol_for_protocol(776)
            .expect("the dedicated binary's v26-2 feature must provide a server protocol");
        let (server, client, _world) = open_persistent_server(
            protocol,
            temp.path(),
            AirWorld,
            (0..=0, 0..=0),
            (0, 0),
            1,
            server_app,
        )
        .expect("the persistent fixture world must open");
        drop(client);

        assert_eq!(observed.load(Ordering::Relaxed), 0);
        tokio::task::yield_now().await;
        for _ in 0..TICKS {
            tokio::time::advance(Duration::from_millis(50)).await;
        }
        wait_for_completed_ticks(&server, TICKS).await;

        assert_eq!(
            observed.load(Ordering::Relaxed),
            TICKS,
            "the binary's persistent-world helper must retain the supplied plugin"
        );
        server.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn dedicated_open_default_app_runs_no_fixture_plugin() {
        const TICKS: u64 = 4;

        let temp = tempfile::tempdir().expect("temporary server world must be created");
        let observed = Arc::new(AtomicU64::new(0));
        let protocol = lodestone_registry::server_protocol_for_protocol(776)
            .expect("the dedicated binary's v26-2 feature must provide a server protocol");
        let (server, client, _world) = open_persistent_server(
            protocol,
            temp.path(),
            AirWorld,
            (0..=0, 0..=0),
            (0, 0),
            1,
            dedicated_server_app(),
        )
        .expect("the persistent fixture world must open");
        drop(client);

        tokio::task::yield_now().await;
        for _ in 0..TICKS {
            tokio::time::advance(Duration::from_millis(50)).await;
        }
        wait_for_completed_ticks(&server, TICKS).await;

        assert_eq!(
            observed.load(Ordering::Relaxed),
            0,
            "the default binary application must not install the fixture plugin"
        );
        assert_eq!(
            server.server_tick_count(),
            Some(TICKS + 1),
            "the zero control must retain one ServerBoot run plus every real GameTick"
        );
        server.shutdown().await;
    }
}
