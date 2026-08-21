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
//! ## Known seam gaps (reported, not worked around)
//!
//! Live terrain does not yet reach the shell: [`lodestone_model::ClientEvent`]'s
//! `ChunkLoaded` carries only a position, so the shell renders a local
//! [`worldgen`] world through the *same* world → classify → mesh → GPU chain a
//! real chunk would use. See [`net`] and the accompanying report.

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
pub mod display_entities;
pub mod effects;
pub mod entities;
pub mod gpu;
pub mod hud;
pub mod interact;
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
pub mod sim;
/// Fetching the signed-in account's own skin and getting it onto the inventory
/// avatar in the same session. See `docs/player-skins.md`, and
/// [`remote_skins`] for the other-players half.
pub mod skin_fetch;
pub mod tablist;
pub mod worldgen;

pub use config::{CliOutcome, Config, Mode};
