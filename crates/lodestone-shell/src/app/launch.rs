//! Starting a session: the integrated server, seed resolution, and `LaunchError`.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

// ---------------------------------------------------------------------------
// Windowed
// ---------------------------------------------------------------------------

/// Why an integrated-server (Singleplayer) launch could not proceed.
///
/// Typed rather than a string so the Error screen can distinguish causes. There
/// is exactly one today, and it is a *build* property rather than a runtime
/// failure: everything else on this path is infallible (see
/// [`launch_singleplayer`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchError {
    /// No version family is compiled into this build that can be **hosted**, so
    /// `lodestone_registry::server_protocol_for_protocol` returned `None`.
    ///
    /// This is what `--no-default-features` produces, and it is the whole reason
    /// the shell asks the registry for a trait object instead of naming a
    /// version: the version-free build must *compile* and report, not fail to
    /// build. It is also reachable with a family compiled in but no
    /// `ServerProtocol` for it — a family can be joinable and unhostable.
    NoVersionFamily {
        /// The protocol number that found no server protocol.
        protocol: i32,
    },
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::NoVersionFamily { protocol } => {
                let compiled = lodestone_registry::compiled_server_families();
                write!(
                    f,
                    "Singleplayer is unavailable in this build: no version family \
                     compiled in can host protocol {protocol}"
                )?;
                if compiled.is_empty() {
                    write!(f, " (none are). Build with the `live` feature.")
                } else {
                    write!(f, " (this build can host: {}).", compiled.join(", "))
                }
            }
        }
    }
}

/// Start singleplayer: an integrated server in-process, with the client speaking
/// to it over an in-memory duplex.
///
/// This is vanilla's own architecture — one client, one dispatch, a different
/// transport — and the whole of it is three steps:
///
/// 1. ask the registry for the **serverbound** half of the version family
///    (`server_protocol_for_protocol`, the twin of the `adapter_for_protocol`
///    call `net.rs` already makes for the clientbound half);
/// 2. hand that trait object to [`NetClient::open_singleplayer`], which starts
///    `lodestone_server::IntegratedServer::open_in_memory` on the net thread's
///    runtime and connects the client to the returned duplex;
/// 3. attach the result to the `Sim` exactly as a multiplayer connect does.
///
/// **The shell names no version here, and that is load-bearing rather than
/// stylistic.** `cargo check -p lodestone-shell --no-default-features` exists to
/// prove this crate compiles with *no* version family, and a `V770ServerProtocol`
/// on this line would break it — which is why the previous version of this
/// function was a deliberate stub returning an error. What changed is not the
/// constraint; it is that the registry now has a serverbound table to ask.
///
/// The only failure is [`LaunchError::NoVersionFamily`]: `open_in_memory` cannot
/// fail (no port to bind), and `connect_with` cannot fail (no dial). So a
/// successful return means a server is running and a client is talking to it —
/// though login is asynchronous, so "running" is proven by the session reaching
/// `Screen::Playing`, not by this returning `Ok`.
/// `world_dir` is where this world saves; `None` opens a
/// throwaway in-memory world, which is what a test that must leave nothing
/// behind asks for. [`crate::saves::default_world_dir`] is what the menu
/// passes — see that module for the "one implicit world" product decision.
pub(crate) fn launch_singleplayer(
    protocol: i32,
    view_radius: i32,
    session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
    seed: i64,
    #[cfg(not(target_arch = "wasm32"))] world_dir: Option<std::path::PathBuf>,
) -> Result<NetClient, LaunchError> {
    let server_protocol = lodestone_registry::server_protocol_for_protocol(protocol)
        .ok_or(LaunchError::NoVersionFamily { protocol })?;
    Ok(NetClient::open_singleplayer(
        server_protocol,
        protocol,
        seed,
        view_radius,
        session,
        #[cfg(not(target_arch = "wasm32"))]
        world_dir,
    ))
}

/// [`launch_singleplayer`]'s **`Created`-only** sibling: starts the world
/// already open to LAN, on an OS-assigned port, with the real RSA/AES
/// handshake and session-server ownership check running on every connection
/// it accepts — `crate::menu::create_world::WorldCreationConfig::online_mode`
/// (issue #273's shell-side control), the one field on that struct that is
/// wired rather than decorative. See that field's own doc for why this is
/// reachable only from **Create New World** and not **Play Selected World**.
///
/// [`NetClient::open_to_lan`] is otherwise [`NetClient::open_singleplayer`]
/// with one more TCP listener bound before it returns, so this mirrors
/// `launch_singleplayer` exactly: same registry lookup, same one failure
/// mode. `port` is always `0` — the caller has not joined anyone in yet, this
/// is "host this new world from the moment it exists", not a fixed address to
/// remember.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn launch_open_to_lan_online(
    protocol: i32,
    view_radius: i32,
    session: Option<(lodestone_ecs::EcsHandle, lodestone_ecs::ecs::entity::Entity)>,
    seed: i64,
    world_dir: Option<std::path::PathBuf>,
) -> Result<NetClient, LaunchError> {
    let server_protocol = lodestone_registry::server_protocol_for_protocol(protocol)
        .ok_or(LaunchError::NoVersionFamily { protocol })?;
    Ok(NetClient::open_to_lan(
        server_protocol,
        protocol,
        seed,
        view_radius,
        session,
        world_dir,
        0,
        true,
    ))
}

/// Vanilla's own seed rule (that fix's queued patch) —
/// `WorldOptions.parseSeed`/`randomSeed()`
/// (`.cache/mc/26.2/client-src/net/minecraft/world/level/levelgen/
/// WorldOptions.java`): trim, empty means a fresh random `i64`, a valid
/// `i64` literal is used verbatim, and anything else — vanilla accepts
/// free-text seeds rather than rejecting them — falls back to Java's own
/// `String.hashCode()` widened (sign-extended) to `i64`.
///
/// `None` means "use the bundled world's own seed" (`Screen::WorldSelect`'s
/// **Play Selected World**, which collects no seed of its own); `Some(cfg)`
/// is `Screen::CreateWorld`'s **Create** button, carrying whatever the player
/// typed into the Seed field (`WorldCreationConfig::seed`, empty by default).
pub(super) fn resolve_launch_seed(config: Option<&crate::menu::create_world::WorldCreationConfig>) -> i64 {
    match config {
        Some(cfg) => parse_seed(&cfg.seed),
        None => crate::menu::world_select::BUNDLED_WORLD.seed,
    }
}

pub(super) fn parse_seed(raw: &str) -> i64 {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return random_seed();
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return n;
    }
    i64::from(java_string_hash_code(trimmed))
}

/// `RandomSource.create().nextLong()` — vanilla asks for *some* fresh long,
/// with no algorithm this port needs to match (a world seed is opaque once
/// generated); `std::collections::hash_map::RandomState` already draws a
/// fresh random key from the OS per instance for exactly this reason, so
/// hashing a timestamp through one needs no new dependency for a value this
/// crate treats as a black box.
fn random_seed() -> i64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    // `crate::platform::epoch_duration`, not `SystemTime::now()`: the latter
    // compiles for wasm32 and panics when it runs, and a seed derived from the
    // clock is one of the very first things a browser session asks for.
    let nanos = crate::platform::epoch_duration().as_nanos();
    hasher.write_u128(nanos);
    hasher.finish() as i64
}

/// Java's `String.hashCode()`: `s[0]*31^(n-1) + … + s[n-1]`, over UTF-16 code
/// units (not bytes, not `char`s) with wrapping 32-bit arithmetic — the exact
/// formula `WorldOptions.parseSeed`'s catch arm calls. Widening the result to
/// `i64` (its caller's job, not this function's) is sign-extending, matching
/// Java's own `int`→`long` widening.
pub(super) fn java_string_hash_code(s: &str) -> i32 {
    let mut h: i32 = 0;
    for unit in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    h
}

/// Whether argv asked for a connection, i.e. whether to bypass the main menu.
///
/// True for `--live`, or for any `--host`/`--port` at all.
///
/// This used to compare against `Config::default()`, which made
/// `--host 127.0.0.1 --port 25565` — spelling out the defaults — indistinguishable
/// from passing nothing, so it silently landed on the main menu. That is the
/// launch the two-worlds report came from: the user asked for a server on the
/// command line and got the title screen. [`Config::address_given`] now records
/// whether the flag was *seen*, which is the question actually being asked.
pub(super) fn requested_a_connection(config: &Config) -> bool {
    config.connect_in_window || config.address_given
}
