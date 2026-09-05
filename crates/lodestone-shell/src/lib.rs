//! Lodestone — the playable game shell.
//!
//! This binary is a **thin client over `lodestone-client` and the render/physics
//! crates**: it opens a window, runs the real bit-exact physics, meshes world
//! sections off the main thread, and draws them with the wgpu block pipeline —
//! but it owns no protocol, version, or rendering *logic* of its own. Every
//! capability is expressed through a library seam:
//!
//! * version selection is a *protocol number* handed to
//!   [`lodestone_registry::adapter_for_protocol`] — the shell names no version;
//! * networking is [`lodestone_client`] only, preserving the `Transport` seam;
//! * world storage, meshing, camera math and the GPU pipeline come from
//!   `lodestone-world` / `lodestone-render`.
//!
//! The logic is factored into pure, testable modules ([`input`], [`worldgen`],
//! [`blocks`], [`collision`], [`mesher`], [`camera_rig`], [`hud`], [`sim`]); the
//! winit/wgpu surface in [`app`] is kept as small as possible.
//!
//! ## The `window` feature, and what stays winit-free without it
//!
//! [`app`] — `WindowApp`'s `ApplicationHandler`, every winit event type, the
//! whole windowed driver — compiles in only behind the `window` Cargo
//! feature (on by default, alongside `live`/`runtime-presentation`). With it
//! off, `winit` is not merely unused, it is **absent from the dependency
//! graph**: `cargo tree -p lodestone-shell --no-default-features -i winit`
//! reports nothing, and `xtask`'s `check-no-winit-headless` subcommand
//! (`just check-seam` calls it) fails loudly if that ever regresses.
//!
//! This is a real, checked build configuration, not just a flag that
//! compiles: [`diagnostics::run_headless`] (one-shot offscreen GPU render)
//! and [`diagnostics::run_connect`] (the live event-stream diagnostic) live
//! outside [`app`] specifically so a windowless build keeps both. [`run`] is
//! the entry point that works either way — it delegates to [`app::run`] when
//! `window` is on, and to [`diagnostics`] directly when it is off. The one
//! reach-in a plain `--no-default-features` build cannot resolve is
//! opening an actual window ([`config::Mode::Window`]), which [`run`]
//! refuses with a named error rather than silently downgrading to something
//! else. See `docs/runtime-presentation.md`.
//!
//! [`keybinds::Key`]/[`keybinds::MouseButton`] are the seam that makes this
//! possible: [`keybinds::Binding`] (persisted in `options.json`, read by
//! [`config`] and [`menu::nav`]) names its own physical-key/mouse-button
//! types rather than winit's, and the `From<winit::keyboard::KeyCode>`/
//! `From<winit::event::MouseButton>` conversions exist only behind `window`
//! — the one place a raw winit key becomes one of these.
//!
//! ## Known seam gaps (reported, not worked around)
//!
//! Live terrain does not yet reach the shell: [`lodestone_model::ClientEvent`]'s
//! `ChunkLoaded` carries only a position, so the shell renders a local
//! [`worldgen`] world through the *same* world → classify → mesh → GPU chain a
//! real chunk would use. See [`net`] and the accompanying report.

#[cfg(feature = "window")]
pub mod app;
pub mod asset_objects;
pub mod audio;
pub mod block_entities;
pub mod blocks;
pub mod camera_rig;
pub mod chat;
pub mod collision;
pub mod command_block_source;
pub mod config;
pub mod consume;
pub mod container;
/// `Mode::Headless`/`Mode::Connect` — see this module's own doc for why they
/// live outside [`app`] and stay in every build regardless of the `window`
/// feature.
pub mod diagnostics;
pub mod display_entities;
pub mod effects;
pub mod entities;
/// Account-scoped Friends polling and session ownership. The window driver
/// feeds it selected-account and activity changes; menu code receives only its
/// credential-free [`friends_runtime::FriendsView`].
pub mod friends_runtime;
pub mod gpu;
pub mod hud;
pub(crate) mod horizon;
/// Bounded, headless profiler input for the distant-terrain horizon.
pub mod horizon_profile;
pub mod interact;
/// Which username and UUID a join presents: the selected Microsoft account, or
/// the persisted offline identity when there is none. One producer, so the
/// account switcher and the join path cannot disagree. See
/// `docs/join-identity.md`.
pub mod join_identity;
pub mod keybinds;
pub mod menu;
pub mod mesher;
pub mod net;
pub mod offline_identity;
pub mod overlay;
pub mod particles;
/// The native/browser seam: a portable monotonic `Instant` and wall clock, for
/// the `wasm32` build that `web/` consumes. `Instant::now()` and
/// `SystemTime::now()` both compile for wasm32 and panic at runtime, so nothing
/// outside this module may name them — see `scripts/wasm-check.sh`'s confinement
/// guards and `docs/browser-shell-port.md`.
pub mod platform;
pub mod raycast;
/// Other players' skins: the `textures` profile property off the tab list,
/// through vanilla's host allow list, to a per-player texture in the world
/// entity pass. See `docs/player-skins.md`.
pub mod remote_skins;
pub mod resources;
pub mod saves;
pub mod scoreboard;
pub mod screenshot;
pub mod sign_diagnostics;
pub mod sim;
/// Fetching the signed-in account's own skin and getting it onto the inventory
/// avatar in the same session. See `docs/player-skins.md`, and
/// [`remote_skins`] for the other-players half.
pub mod skin_fetch;
pub mod tablist;
/// Native interactive terminal surfaces selected by `--surface`.
#[cfg(not(target_arch = "wasm32"))]
pub mod terminal;
pub mod worldgen;
/// Native runtime discovery for capability-gated WASM plugins. Kept out of the
/// browser graph because the Wasmtime host cannot compile for wasm32.
#[cfg(not(target_arch = "wasm32"))]
pub mod wasm_plugins;

pub use config::{CliOutcome, Config, Mode};

/// Entry point: dispatch on the configured mode, whether or not this build
/// was compiled with the `window` Cargo feature.
///
/// `main.rs` calls this rather than `app::run` directly for exactly that
/// reason — `app` (and therefore `app::run`) does not exist at all without
/// `window`, while `Mode::Headless`/`Mode::Connect` (via [`diagnostics`])
/// stay available either way. The browser build (`web/`) keeps calling
/// `app::run` directly instead: it always builds with `window` on (a browser
/// session is always [`Mode::Window`]) and has no command line to select a
/// diagnostic mode from, so there is nothing here for it to gain.
///
/// # Errors
/// Returns an error if GPU bring-up, the event loop, or a diagnostic mode's
/// own work fails — or, with `window` off, if `config.mode` needs a window
/// this build was not compiled with one.
#[cfg(not(target_arch = "wasm32"))]
pub fn run(config: Config) -> anyhow::Result<()> {
    #[cfg(feature = "window")]
    {
        app::run(config)
    }
    #[cfg(not(feature = "window"))]
    {
        match config.mode {
            Mode::Headless => diagnostics::run_headless(diagnostics::require_owned_account()?, config),
            Mode::Connect => diagnostics::run_connect(diagnostics::require_owned_account()?, config),
            Mode::Stdio => terminal::run_stdio(diagnostics::require_owned_account()?, config),
            Mode::Terminal => terminal::run_terminal(diagnostics::require_owned_account()?, config),
            Mode::Window => Err(anyhow::anyhow!(
                "this build was compiled without the `window` Cargo feature, so it has \
                 no winit and cannot open a window. Rebuild with `--features window` \
                 (on by default) to open one, or use --surface stdio/terminal, \
                 --headless, or --connect instead."
            )),
        }
    }
}

/// [`run`], around a caller-composed [`lodestone_app::App`] — the seam a
/// downstream crate uses to register a plugin into the real, on-screen game
/// rather than a headless consumer only. Build the `App` from [`sim::Sim::client_app`],
/// `add_plugins` the plugin, then hand the result here in place of a plain
/// [`Config`]. See `docs/plugin-api.md`'s "Registration" section.
///
/// Only exists with `window` on: there is no windowed entry point to reach
/// without it, and `window`-off builds have no use for an `App` that only
/// `Mode::Window` consumes (see [`app::run_with_app`]'s own doc for why every
/// other mode is refused there rather than silently dropping the plugin).
///
/// # Errors
/// Same as [`run`], plus a named error if `config.mode` is not [`Mode::Window`].
#[cfg(all(not(target_arch = "wasm32"), feature = "window"))]
pub fn run_with_app(app: lodestone_app::App, config: Config) -> anyhow::Result<()> {
    app::run_with_app(app, config)
}
