//! Block-appearance resources: the [`ShellClassifier`] the mesher runs on and
//! the atlas the GPU binds, resolved once per session.
//!
//! The shell has two mutually exclusive block-id worlds (see [`ShellClassifier`]):
//! the offline **demo palette** and the **vanilla** registry a live server
//! streams. This module is where the choice is made and where the real vanilla
//! assets are loaded off disk into a [`BlockAtlas`]. It is deliberately
//! *fail-closed and loud*: a vanilla load failure falls back to the demo palette
//! and records a human-readable reason for the debug overlay, never a
//! silently-empty atlas that would render an invisible world.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lodestone_assets::{ResourceManager, ResourceSource, ZipSource};
use lodestone_render::{BlockAtlas, blocks_json_registry};

use crate::blocks::{DemoClassifier, ShellClassifier};

/// Everything the render pipeline needs to turn block-state ids into pixels,
/// resolved once at session start.
#[derive(Debug)]
pub struct BlockResources {
    /// The classifier the mesh workers use.
    pub classifier: ShellClassifier,
    /// The stitched vanilla atlas (the same `Arc` the classifier's
    /// [`Vanilla`](ShellClassifier::Vanilla) variant holds), for the GPU. `None`
    /// when running on the demo palette.
    pub vanilla_atlas: Option<Arc<BlockAtlas>>,
    /// A line for the debug overlay when the vanilla load was attempted but fell
    /// back, naming the cause. `None` on success or when vanilla was never
    /// attempted.
    pub banner: Option<String>,
}

impl BlockResources {
    /// Resolve resources for a session. `want_vanilla` is true for a live
    /// multiplayer session (whose world streams vanilla ids); the offline dev
    /// world passes `false` and always uses the demo palette.
    #[must_use]
    pub fn load(want_vanilla: bool) -> Self {
        if !want_vanilla {
            return Self::demo(None);
        }
        match Self::try_vanilla() {
            Ok(atlas) => {
                let atlas = Arc::new(atlas);
                tracing::info!(
                    target: "assets",
                    sprites = atlas.sprite_count(),
                    "loaded vanilla block atlas for the live world"
                );
                Self {
                    classifier: ShellClassifier::Vanilla(Arc::clone(&atlas)),
                    vanilla_atlas: Some(atlas),
                    banner: None,
                }
            }
            Err(reason) => Self::demo(Some(reason)),
        }
    }

    fn demo(banner: Option<String>) -> Self {
        if let Some(b) = &banner {
            tracing::warn!(target: "assets", "{b}");
        }
        Self {
            classifier: ShellClassifier::Demo(DemoClassifier),
            vanilla_atlas: None,
            banner,
        }
    }

    /// Load the real vanilla assets: the block registry from
    /// `generated/reports/blocks.json` and every model/texture from `client.jar`,
    /// stitched into a [`BlockAtlas`]. Errors are stringified with the offending
    /// path so the fallback banner names the fix.
    fn try_vanilla() -> Result<BlockAtlas, String> {
        let root = asset_root().ok_or_else(|| {
            "no vanilla resource pack found — set LODESTONE_ASSETS to a pack root \
             containing client.jar + generated/reports/blocks.json (live world uses \
             the demo palette until then)"
                .to_string()
        })?;
        let jar = root.join("client.jar");
        let report = root.join("generated/reports/blocks.json");

        let bytes = std::fs::read(&jar).map_err(|e| format!("read {}: {e}", jar.display()))?;
        let zip = ZipSource::from_bytes(bytes).map_err(|e| format!("open {}: {e}", jar.display()))?;
        let manager = ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>]);
        let registry =
            blocks_json_registry(&report).map_err(|e| format!("load {}: {e}", report.display()))?;
        BlockAtlas::build(&manager, &registry)
            .map_err(|e| format!("build atlas from {}: {e}", root.display()))
    }
}

/// Locate a vanilla resource-pack root, version-free: honour `LODESTONE_ASSETS`
/// if set, else search upward from the current directory for a `.cache/mc/<any>`
/// entry that holds **both** a `client.jar` and a `generated/reports/blocks.json`
/// (both are required to stitch the atlas), picking the highest-sorting such
/// directory. Naming no version in code is deliberate — the shell must not name a
/// protocol version; the cache directory's own name carries it.
///
/// The ancestor walk makes this robust to the working directory: the binary runs
/// from the workspace root while integration tests run from the crate directory,
/// and both need to find the same repo-root `.cache/mc`.
fn asset_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LODESTONE_ASSETS") {
        let p = PathBuf::from(dir);
        return is_pack_root(&p).then_some(p);
    }
    let cwd = std::env::current_dir().ok()?;
    for base in cwd.ancestors() {
        if let Some(root) = best_pack_in(&base.join(".cache/mc")) {
            return Some(root);
        }
    }
    None
}

/// True when `dir` holds both files needed to stitch the vanilla atlas.
fn is_pack_root(dir: &Path) -> bool {
    dir.join("client.jar").is_file() && dir.join("generated/reports/blocks.json").is_file()
}

/// The highest-sorting complete pack directly under `cache_dir`, or `None`.
fn best_pack_in(cache_dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(cache_dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| is_pack_root(p))
        .collect();
    entries.sort();
    entries.pop()
}
