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
pub mod audio;
pub mod blocks;
pub mod camera_rig;
pub mod chat;
pub mod collision;
pub mod config;
pub mod container;
pub mod entities;
pub mod gpu;
pub mod hud;
pub mod menu;
pub mod mesher;
pub mod net;
pub mod overlay;
pub mod raycast;
pub mod scoreboard;
pub mod sim;
pub mod tablist;
pub mod worldgen;

pub use config::{CliOutcome, Config, Mode};
