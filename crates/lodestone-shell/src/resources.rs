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
use std::sync::{Arc, OnceLock};

use lodestone_assets::{
    ItemAtlas, Language, ParticleAtlas, ResourceManager, ResourceSource, ZipSource,
};
use lodestone_render::{
    BlockAtlas, BlockModels, EntityModelSet, GuiAtlas, ScreenEffectRenderer, SkyRenderer,
    entity_texture_candidates,
};
// The *path*-taking loader is `cfg(not(wasm32))` in `lodestone-render` (it is
// `std::fs`-based and confined to its own gated file); the bytes-taking
// `BlocksJsonRegistry::from_slice` it wraps is not. So the browser arm below needs
// no new render-crate API — only bytes.
#[cfg(not(target_arch = "wasm32"))]
use lodestone_render::blocks_json_registry;

use uuid::Uuid;

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
    /// The vanilla `en_us.json` language table, loaded from the same pack, for
    /// resolving server-authored `translate` components (death messages,
    /// scoreboard titles, tab-list names, …) into words. `None` on the demo
    /// palette or when the pack has no language file — resolution then falls
    /// back to the component's own `fallback`/key, never an error.
    pub language: Option<Arc<Language>>,
    /// The stitched particle atlas, for sheet-sourced particles (smoke, flame,
    /// crits, splashes). `None` on the demo palette or when the pack has no
    /// particle textures — every sheet particle then resolves to nothing and is
    /// counted into [`ParticleFrame::unresolved`](crate::particles::ParticleFrame)
    /// rather than dropped silently, so the gap stays observable.
    ///
    /// Separate from [`Self::vanilla_atlas`] because particle sprites are their
    /// own stitch: they are not reachable from any blockstate, so the block
    /// atlas never contains them.
    pub particle_atlas: Option<Arc<ParticleAtlas>>,
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
            Ok((atlas, language)) => {
                let atlas = Arc::new(atlas);
                tracing::info!(
                    target: "assets",
                    sprites = atlas.sprite_count(),
                    lang_keys = language.as_ref().map_or(0, Language::len),
                    "loaded vanilla block atlas for the live world"
                );
                Self {
                    classifier: ShellClassifier::Vanilla(Arc::clone(&atlas)),
                    vanilla_atlas: Some(atlas),
                    banner: None,
                    language: language.map(Arc::new),
                    particle_atlas: load_particle_atlas(),
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
            language: None,
            particle_atlas: None,
        }
    }

    /// Load the real vanilla assets: the block registry from
    /// `generated/reports/blocks.json` and every model/texture from `client.jar`,
    /// stitched into a [`BlockAtlas`]. The `en_us.json` language table rides along
    /// from the same jar (absent is not an error — it just disables translation).
    /// Errors are stringified with the offending path so the fallback banner
    /// names the fix.
    fn try_vanilla() -> Result<(BlockAtlas, Option<Language>), String> {
        // Browser: the bytes were `fetch`ed and installed by `web/` before the app
        // started. Only the *acquisition* differs — `ZipSource::from_bytes` and
        // `BlocksJsonRegistry::from_slice` are the same parsers the native arm
        // below reaches, so everything downstream of these two `let`s is shared.
        // See `crate::platform::assets`.
        #[cfg(target_arch = "wasm32")]
        let (bytes, registry) = {
            let bundle = crate::platform::assets::bundle().ok_or_else(|| {
                "no asset bundle installed — a browser has no filesystem to scan, so \
                 web/ must fetch client.jar + generated/reports/blocks.json and call \
                 lodestone::platform::assets::install() before app::run (live world \
                 uses the demo palette until then)"
                    .to_string()
            })?;
            let registry =
                lodestone_render::BlocksJsonRegistry::from_slice(&bundle.blocks_report)
                    .map_err(|e| format!("load blocks.json bytes: {e}"))?;
            (bundle.client_jar.clone(), registry)
        };

        #[cfg(not(target_arch = "wasm32"))]
        let (bytes, registry) = {
            let root = asset_root().ok_or_else(|| {
                "no vanilla resource pack found — set LODESTONE_ASSETS to a pack root \
                 containing client.jar + generated/reports/blocks.json (live world uses \
                 the demo palette until then)"
                    .to_string()
            })?;
            let jar = root.join("client.jar");
            let report = root.join("generated/reports/blocks.json");
            let bytes = std::fs::read(&jar).map_err(|e| format!("read {}: {e}", jar.display()))?;
            let registry = blocks_json_registry(&report)
                .map_err(|e| format!("load {}: {e}", report.display()))?;
            (bytes, registry)
        };

        let zip = ZipSource::from_bytes(bytes).map_err(|e| format!("open client.jar: {e}"))?;
        // The user's selected packs sit on top of the built-in jar (issue #415),
        // so a pack that ships `assets/minecraft/textures/block/**` changes the
        // world's appearance from this session on. This is the block atlas' own
        // stack, not a shared one — see `selected_pack_sources`' doc.
        let manager = build_pack_stack(Box::new(zip));
        // The live `mipmapLevels` video setting's actual consumer: `mipmap_levels()`
        // returns the shipped default until a player drags the slider, at which
        // point `set_mipmap_levels` has already bumped `PACK_GENERATION`, so the
        // *next* call here (a live reload, not just the initial load) rebuilds at
        // the new depth.
        let atlas = BlockAtlas::build_with_mip_levels(&manager, &registry, mipmap_levels())
            .map_err(|e| format!("build atlas from the vanilla pack: {e}"))?;
        // Bake the per-state model geometry (cross-plants, slabs, stairs,
        // translucency) against the same registry and attach it, so the model
        // render path resolves state ids to real quads instead of full cubes.
        //
        // At the **same** mip depth as the atlas above, and that is not a
        // tidiness point: a live session draws terrain through the model pass,
        // which binds this object's own atlas rather than the `BlockAtlas`
        // stitched above, so a `mipmapLevels` change that reached only the
        // latter rebuilt an atlas nothing sampled and moved no pixels. See
        // `BlockModels::build_with_mip_levels`.
        let models = BlockModels::build_with_mip_levels(&manager, &registry, mipmap_levels())
            .map_err(|e| format!("build models from the vanilla pack: {e}"))?;
        if tracing::enabled!(target: "pack_trace", tracing::Level::DEBUG) {
            let player_head = "minecraft:player_head"
                .parse::<lodestone_assets::ResourceLocation>()
                .expect("static player-head item id parses");
            let forms = models.item_forms(&player_head);
            let outputs = forms.map(|forms| {
                forms
                    .definition()
                    .resolve(&lodestone_assets::GuiItemContext)
            });
            let first_person_outputs = forms.map(|forms| {
                forms.definition().resolve(&lodestone_render::ItemStateContext::new(
                    lodestone_assets::DisplaySlot::FirstPersonRightHand,
                ))
            });
            let third_person_outputs = forms.map(|forms| {
                forms.definition().resolve(&lodestone_render::ItemStateContext::new(
                    lodestone_assets::DisplaySlot::ThirdPersonRightHand,
                ))
            });
            let special_forms: Vec<_> = forms
                .into_iter()
                .flat_map(|forms| forms.special_forms())
                .map(|(base, form)| {
                    (
                        base.to_string(),
                        form.kind.clone(),
                        form.transformation.clone(),
                        form.display.get(lodestone_assets::DisplaySlot::Gui),
                        form.display.get(lodestone_assets::DisplaySlot::FirstPersonRightHand),
                        form.display.get(lodestone_assets::DisplaySlot::ThirdPersonRightHand),
                    )
                })
                .collect();
            tracing::debug!(
                target: "pack_trace",
                definition_present = forms.is_some(),
                ?outputs,
                ?first_person_outputs,
                ?third_person_outputs,
                ?special_forms,
                "player-head definition parsed after the current resource-pack stack rebuilt"
            );
        }
        tracing::info!(
            target: "assets",
            state_count = models.state_count(),
            "baked per-state model geometry for {} block states",
            models.state_count(),
        );
        // The language table is **merged** across the whole pack stack, not
        // read from whichever pack wins (`manager.read` would do that, and
        // used to be what this called) — a pack that ships its own partial
        // `lang/en_us.json` (a handful of custom keys for its own items or
        // fonts) must not blank out the ~7,000 vanilla keys underneath it.
        // See `Language::merged_from_stack`'s own doc for the vanilla record
        // this reproduces (`ClientLanguage.loadFrom`/`getResourceStack`) and
        // the symptom a single-winner read produced: a raw translation key
        // like `container.crafting` reaching the screen instead of the word.
        // A missing file on every layer just disables translation rather
        // than failing the whole live load; a malformed layer is skipped by
        // `merged_from_stack` itself.
        let language = Language::merged_from_stack(&manager, "minecraft", "en_us");
        Ok((atlas.with_models(models), language))
    }
}

/// Opens `<root>/client.jar` **plus every selected user resource pack** as a
/// [`ResourceManager`], warning and returning `None` on a jar failure — the
/// fail-open pack discovery every loader in this module shares below
/// [`BlockResources::try_vanilla`], whose own errors must propagate as a
/// fallback-banner reason instead of a log line, so it builds its stack through
/// [`selected_pack_sources`] itself rather than going through this helper.
///
/// **This is the one function that makes the Resource Packs screen do
/// anything.** Every `load_*` in this module goes through it, so a pack that
/// overrides GUI sprites, item art, the sky, the container panels or the block
/// textures is picked up by whichever loader owns that art the next time it
/// runs. See [`selected_pack_sources`] for what "the next time it runs" means
/// per loader.
///
/// `pub` so `tests/resource_pack_stack.rs` can drive the **production** stack
/// rather than reassembling one that happens to look the same — the difference
/// between proving the wire and proving a copy of it.
#[must_use]
pub fn open_pack_stack(root: &Path) -> Option<ResourceManager> {
    let jar = root.join("client.jar");
    let bytes = match std::fs::read(&jar) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "assets", "read {}: {e}", jar.display());
            return None;
        }
    };
    let zip = match ZipSource::from_bytes(bytes) {
        Ok(z) => z,
        Err(e) => {
            tracing::warn!(target: "assets", "open {}: {e}", jar.display());
            return None;
        }
    };
    Some(build_pack_stack(Box::new(zip)))
}

/// Opens the production vanilla pack stack, version- **and platform-free**.
///
/// Every `load_*` helper below used to call `asset_root()` and
/// [`open_pack_stack`] directly. `asset_root` is plain `std::env`/`std::fs`,
/// which is not `cfg`-gated because it does not need to be — `std::fs` on
/// wasm32 returns `Err(Unsupported)` rather than trapping — but the practical
/// effect is that it **always** returns `None` in a browser, so every one of
/// those call sites silently fell back to "no pack found" even after `web/`
/// had already `fetch`ed and installed the jar. [`vanilla_manager`] is the
/// choke point that actually knows about the wasm bundle (see its own doc),
/// but nothing in this file routed through it except [`BlockResources::try_vanilla`]
/// and, until this became `pub(crate)`, the font loader in
/// `hud/vanilla_font.rs`. This is the fix: one function every target goes
/// through, so a browser session's GUI atlas, panorama, sky, item/particle
/// atlases, entity textures, container art, recipe corpus **and font**
/// load from the same bytes.
///
/// Native: identical to `open_pack_stack(&asset_root()?)` — same discovery,
/// same user-pack layering. Browser: the same `fetch`ed bytes
/// [`vanilla_manager`] reads, run through [`build_pack_stack`] so a selected
/// user pack would still layer on top (today that list is always empty on
/// wasm32, since [`open_pack_source`] has nothing to open there, but the
/// call is the same shape as the native side rather than a special case).
///
/// `pub(crate)` so `hud/vanilla_font.rs`'s font loader can route through it
/// too, instead of [`vanilla_manager`] (the raw jar, no pack layering): a
/// font loaded from the jar alone can never see a resource pack's own
/// `font/*.json` providers, no matter how faithfully the text model carries
/// a `"font"` id or how correct the pack's own JSON is — the manager it asks
/// never held the pack to begin with.
#[must_use]
pub(crate) fn open_vanilla_pack_stack() -> Option<ResourceManager> {
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = crate::platform::assets::bundle()?.client_jar.clone();
        let zip = ZipSource::from_bytes(bytes).ok()?;
        Some(build_pack_stack(Box::new(zip)))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        open_pack_stack(&asset_root()?)
    }
}

/// Lays the currently selected user packs on top of the built-in pack.
///
/// `builtin` is the bottom of the stack — vanilla's own `Pack.Position.BOTTOM`
/// fixed-position built-in pack (`Pack.java`), which is why the
/// Resource Packs screen can never move or remove it.
fn build_pack_stack(builtin: Box<dyn ResourceSource>) -> ResourceManager {
    let mut sources: Vec<Box<dyn ResourceSource>> = vec![builtin];
    // `ResourceManager::new` is lowest-priority-first and `selected_pack_sources`
    // returns the UI's own highest-first order, so the extend is `.rev()`. See
    // `ResourceManager::from_priority_order`'s doc for both attestations.
    sources.extend(selected_pack_sources().into_iter().rev());
    ResourceManager::new(sources)
}

// -- the pack repository (issue #415) ----------------------------------------

/// The user's `resourcepacks/` folder — vanilla's `FolderRepositorySource`
/// root, alongside `saves/`, `servers.json` and `options.json` in the same
/// platform data directory (and honouring the same `LODESTONE_DATA_DIR`
/// override).
#[must_use]
pub fn resource_packs_dir() -> PathBuf {
    crate::menu::servers::data_dir().join("resourcepacks")
}

/// The user's `datapacks/` folder — Create New World's Data Packs sub-screen
/// (issue #592's More tab) scans this the same way [`resource_packs_dir`] is
/// scanned for resource packs, through the identical [`scan_resource_packs_in`]
/// / [`DiscoveredPack`] pair: a data pack and a resource pack share the exact
/// on-disk shape (`pack.mcmeta` plus an optional `pack.png`), so nothing about
/// the scan itself needed to be data-pack-specific — only the directory does.
/// Vanilla scans a *world's own* `datapacks/` folder plus the running
/// instance's staging area; this client has no per-world folder yet at
/// creation time (`world_select`'s own module docs — no `LevelStorageSource`),
/// so this is the one staging location, alongside `resourcepacks/` in the same
/// platform data directory.
#[must_use]
pub fn data_packs_dir() -> PathBuf {
    crate::menu::servers::data_dir().join("datapacks")
}

/// Whether a discovered pack is a directory tree or a zip archive. Both are the
/// same on-disk format to [`ResourceSource`], so this only exists to decide
/// which constructor to call and what to show the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackKind {
    /// A directory tree containing `pack.mcmeta`.
    Directory,
    /// A `.zip` archive.
    Zip,
}

/// One resource pack found in [`resource_packs_dir`], with everything the
/// Resource Packs screen draws.
#[derive(Debug, Clone)]
pub struct DiscoveredPack {
    /// Vanilla's own pack id: `"file/<filename>"`
    /// (`FolderRepositorySource.createDiscoveredFilePackInfo`). This is what is
    /// persisted in `resource_packs.json`, so renaming the file on disk
    /// deselects the pack — exactly as it does in vanilla.
    pub id: String,
    /// The display title: the bare filename, as
    /// `Component.literal(nameFromPath(content))`.
    pub title: String,
    /// The `pack.mcmeta` description, flattened to plain text.
    pub description: String,
    /// The pack's declared `pack_format`. Recorded but **not** validated — see
    /// this module's note on `docs/resource-packs.md`.
    pub pack_format: u32,
    /// The decoded `pack.png`, when the pack ships one.
    pub icon: Option<lodestone_assets::Image>,
    /// Where it lives, for reopening it as a source.
    pub path: PathBuf,
    /// Directory or zip.
    pub kind: PackKind,
}

/// Scans [`resource_packs_dir`] for packs, accepting **both** directories and
/// `.zip` files, sorted by id so the Available column is stable across runs.
///
/// Fail-open at every level: an unreadable folder yields an empty list, and an
/// entry that will not open (a corrupt zip, a directory with no `pack.mcmeta`)
/// is skipped with a warning rather than failing the scan. That mirrors
/// `FolderRepositorySource.loadPacks`, which logs and continues.
#[must_use]
pub fn scan_resource_packs() -> Vec<DiscoveredPack> {
    scan_resource_packs_in(&resource_packs_dir())
}

/// As [`scan_resource_packs`], from an explicit directory — the form tests use,
/// so no test ever reads the developer's own pack folder.
#[must_use]
pub fn scan_resource_packs_in(dir: &Path) -> Vec<DiscoveredPack> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Skip the dotfiles a Finder/Explorer visit leaves behind.
        if name.starts_with('.') {
            continue;
        }
        let kind = if path.is_dir() {
            PackKind::Directory
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
        {
            PackKind::Zip
        } else {
            continue;
        };
        let Some(source) = open_pack_source(&path, kind) else {
            continue;
        };
        // A directory with no `pack.mcmeta` is not a pack — it is a stray
        // folder. A zip without one is a stray archive. Vanilla's own
        // `Pack.readMetaAndCreate` returns null in both cases and the entry is
        // dropped, which is what the `?` here reproduces.
        let Some(meta_bytes) = source.read("pack.mcmeta") else {
            tracing::debug!(target: "assets", "{}: no pack.mcmeta, not a resource pack", path.display());
            continue;
        };
        let meta = match lodestone_assets::PackMeta::parse(&meta_bytes) {
            Ok(meta) => meta,
            Err(e) => {
                tracing::warn!(target: "assets", "{}: malformed pack.mcmeta: {e}", path.display());
                continue;
            }
        };
        let icon = source
            .read("pack.png")
            .and_then(|png| match lodestone_assets::Image::decode_png(&png) {
                Ok(img) => Some(img),
                Err(e) => {
                    tracing::warn!(target: "assets", "{}: decode pack.png: {e}", path.display());
                    None
                }
            });
        out.push(DiscoveredPack {
            id: format!("file/{name}"),
            title: name.to_string(),
            description: meta.description.plain_text(),
            pack_format: meta.pack_format,
            icon,
            path,
            kind,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Opens one discovered pack as a [`ResourceSource`], warning on failure.
fn open_pack_source(path: &Path, kind: PackKind) -> Option<Box<dyn ResourceSource>> {
    // Browser: neither variant can exist. `DirectorySource` and `ZipSource::open`
    // are both `cfg(not(wasm32))` in `lodestone-assets` because they read paths, and
    // there is nothing here to substitute — a user-selected pack is a *file the user
    // picked off their disk*, and `scan_resource_packs` cannot enumerate one either
    // (`read_dir` returns `Err(Unsupported)`), so this is unreachable rather than
    // merely unsupported. Browser resource packs would arrive as bytes through a
    // file input or a `fetch`, i.e. through `platform::assets`, not through here.
    #[cfg(target_arch = "wasm32")]
    {
        tracing::debug!(
            target: "assets",
            "ignoring pack {} ({kind:?}): a browser has no pack files on disk",
            path.display(),
        );
        return None;
    }
    #[cfg(not(target_arch = "wasm32"))]
    match kind {
        PackKind::Directory => match lodestone_assets::DirectorySource::new(path) {
            Ok(s) => Some(Box::new(s) as Box<dyn ResourceSource>),
            Err(e) => {
                tracing::warn!(target: "assets", "open pack dir {}: {e}", path.display());
                None
            }
        },
        PackKind::Zip => match ZipSource::open(path) {
            Ok(s) => Some(Box::new(s) as Box<dyn ResourceSource>),
            Err(e) => {
                tracing::warn!(target: "assets", "open pack zip {}: {e}", path.display());
                None
            }
        },
    }
}

/// The process-wide selected-pack order, **highest priority first** (the
/// Resource Packs screen's own top-to-bottom Selected column).
///
/// A `RwLock` rather than a `OnceLock` because the whole point of the screen is
/// that this changes mid-run; `None` means "not read from disk yet", which
/// [`selected_packs`] resolves on first use.
static SELECTED_PACKS: std::sync::RwLock<Option<Vec<String>>> = std::sync::RwLock::new(None);

/// Bumped by every [`set_selected_packs`] **or** [`set_mipmap_levels`] call —
/// the trigger a live reload polls for, since neither the pack stack nor the
/// mip depth carries a cheap "did this change" signal of its own (two
/// `Vec<String>`s are only comparable by a full equality check, and the
/// poller already needs an `Ordering::Relaxed` atomic load to be cheap enough
/// to run every frame, exactly like `TerrainMesh::set_cutout_leaves`'s own
/// equality guard).
///
/// One counter for both triggers rather than two: `Sim::reload_resource_pack_atlas`
/// does not care *why* the atlas is stale, only *that* it is, and it rebuilds
/// from whatever [`selected_packs`] and [`mipmap_levels`] currently say
/// either way — a second counter would only be a second poll doing the same
/// job. See that method for the consumer.
static PACK_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Replaces the selected-pack order, highest priority first, and bumps
/// [`pack_generation`]. Anything built after this call sees the new stack;
/// see [`selected_pack_sources`] for which loaders that actually reaches
/// *without* a live reload, and `Sim::reload_resource_pack_atlas` for the one
/// consumer that now polls the generation to reach the rest live.
pub fn set_selected_packs(ids: Vec<String>) {
    if let Ok(mut guard) = SELECTED_PACKS.write() {
        *guard = Some(ids);
    }
    PACK_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// The current pack-selection-or-mip-depth generation. Changes exactly once
/// per [`set_selected_packs`] or [`set_mipmap_levels`] call; never decreases;
/// carries no meaning beyond (in)equality with a previously observed value.
#[must_use]
pub fn pack_generation() -> u64 {
    PACK_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// A server-pushed resource pack currently installed for this session — the
/// bytes `net.rs`'s resource-pack flow downloaded and SHA-1-verified, held
/// **entirely in memory**. There is deliberately nowhere on disk for these
/// bytes to land: `lodestone_assets::ZipSource::from_bytes` reads a zip
/// archive straight out of a `Vec<u8>` (the same version-free parser a local
/// third-party `.zip` pack already goes through — see [`open_pack_source`]),
/// so there is no extraction step and therefore no path a malicious archive
/// entry could escape to. `None` when no server pack is installed, or once
/// it is withdrawn ([`clear_server_pack`]).
///
/// Keyed by the pack id so a pop naming a *different*, already-superseded id
/// cannot remove a newer push it raced with — see [`clear_server_pack`].
static SERVER_PACK: std::sync::RwLock<Option<(Uuid, Vec<u8>)>> = std::sync::RwLock::new(None);

/// Presentation data for the active in-memory server pack. This is deliberately
/// separate from [`DiscoveredPack`]: it has no filesystem path and must never
/// enter the persisted player-selection list.
#[derive(Debug, Clone)]
pub struct ServerPackInfo {
    /// Session-local identity, never persisted in the player pack order.
    pub id: String,
    /// Stable source label for the selection screen.
    pub title: String,
    /// The pack's own `pack.mcmeta` description, flattened for the menu row.
    pub description: String,
    /// Decoded `pack.png`, if the server pack supplied one.
    pub icon: Option<lodestone_assets::Image>,
}

/// The active server pack's Resource Packs screen entry, if any.
#[must_use]
pub fn server_pack_info() -> Option<ServerPackInfo> {
    let guard = SERVER_PACK.read().ok()?;
    let (id, bytes) = guard.as_ref()?;
    let source = ZipSource::from_bytes(bytes.clone()).ok()?;
    let description = source
        .read("pack.mcmeta")
        .and_then(|raw| lodestone_assets::PackMeta::parse(&raw).ok())
        .map_or_else(
            || "Resources supplied by this server".to_string(),
            |meta| meta.description.plain_text(),
        );
    let icon = source
        .read("pack.png")
        .and_then(|raw| lodestone_assets::Image::decode_png(&raw).ok());
    Some(ServerPackInfo {
        id: format!("server/{id}"),
        title: "Server resource pack".to_string(),
        description,
        icon,
    })
}

/// Installs (or replaces) the live server pack, after checking it actually
/// opens as one. A corrupt or truncated download must not silently blank
/// the block atlas on the next reload — the same fail-closed discipline
/// [`BlockResources::try_vanilla`] uses for the bundled pack — so this returns `false`
/// (vanilla's own `FAILED_RELOAD`) rather than installing bytes that will
/// only fail later, deeper in the pipeline, with a worse error.
///
/// Bumps [`pack_generation`] on success, which is the entire live-reload
/// signal: `Sim::reload_resource_pack_atlas` already polls it every frame
/// for the local-pack-selection screen, and does not care *why* the atlas is
/// stale, so a server pack reaches the world exactly the same way a locally
/// selected one does — no second wiring path to fall out of sync with the
/// first.
#[must_use]
pub fn set_server_pack(id: Uuid, bytes: Vec<u8>) -> bool {
    let source = match ZipSource::from_bytes(bytes.clone()) {
        Ok(source) => source,
        Err(e) => {
            tracing::warn!(target: "assets", "server resource pack {id} did not open: {e}");
            return false;
        }
    };
    log_server_pack_render_inputs(id, &source, bytes.len());
    if let Ok(mut guard) = SERVER_PACK.write() {
        *guard = Some((id, bytes));
    }
    PACK_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    true
}

/// Records the few server-pack entries that decide the two most failure-prone
/// item render paths. Kept behind `RUST_LOG=pack_trace=debug`: this is a
/// diagnostic snapshot at pack installation, not a normal asset-load log.
fn log_server_pack_render_inputs(id: Uuid, source: &ZipSource, bytes: usize) {
    if !tracing::enabled!(target: "pack_trace", tracing::Level::DEBUG) {
        return;
    }
    const DIAMOND_SWORD_DEFINITION: &str = "assets/minecraft/items/diamond_sword.json";
    const DIAMOND_SWORD_LEGACY_MODEL: &str = "assets/minecraft/models/item/diamond_sword.json";
    const PLAYER_HEAD_DEFINITION: &str = "assets/minecraft/items/player_head.json";
    const PLAYER_HEAD_SHEET: &str = "assets/minecraft/textures/entity/player/wide/steve.png";
    tracing::debug!(
        target: "pack_trace",
        %id,
        bytes,
        entries = source.list("assets/").len(),
        diamond_sword_definition = source.read(DIAMOND_SWORD_DEFINITION).is_some(),
        diamond_sword_legacy_model = source.read(DIAMOND_SWORD_LEGACY_MODEL).is_some(),
        player_head_definition = source.read(PLAYER_HEAD_DEFINITION).is_some(),
        player_head_sheet = source.read(PLAYER_HEAD_SHEET).is_some(),
        "server pack render inputs scanned; items/<id>.json is the active item-definition format"
    );
}

/// Withdraws the live server pack — `ClientboundResourcePackPopPacket`.
/// `Some(id)` clears only if `id` is the pack currently installed (so a pop
/// racing a newer push cannot remove the newer one); `None` clears
/// unconditionally, matching vanilla's own "remove every pack". Bumps
/// [`pack_generation`] only when something actually changed, so an
/// already-absent pack does not trigger a pointless reload.
pub fn clear_server_pack(id: Option<Uuid>) {
    let Ok(mut guard) = SERVER_PACK.write() else {
        return;
    };
    let changed = match id {
        Some(id) => {
            if guard.as_ref().is_some_and(|(current, _)| *current == id) {
                *guard = None;
                true
            } else {
                false
            }
        }
        None => guard.take().is_some(),
    };
    drop(guard);
    if changed {
        PACK_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The live server pack as a [`ResourceSource`], if one is installed and
/// still opens. A pack that verified when it was installed but no longer
/// parses would be a `set_server_pack` bug, not a runtime state this should
/// ever hit — but the same fail-open-with-a-warning discipline every other
/// pack open in this module uses applies here too, rather than a `panic!` or
/// a silently empty world.
#[must_use]
fn server_pack_source() -> Option<Box<dyn ResourceSource>> {
    let guard = SERVER_PACK.read().ok()?;
    let (id, bytes) = guard.as_ref()?;
    match ZipSource::from_bytes(bytes.clone()) {
        Ok(source) => Some(Box::new(source) as Box<dyn ResourceSource>),
        Err(e) => {
            tracing::warn!(target: "assets", "server resource pack {id} stopped opening: {e}");
            None
        }
    }
}

/// The process-wide requested block-atlas mip depth (the live `mipmapLevels`
/// video setting). `None` means "not read from `options.json` yet" — the same
/// lazy-seed shape as [`SELECTED_PACKS`], resolved on first use by
/// [`mipmap_levels`].
static MIPMAP_LEVELS: std::sync::RwLock<Option<u32>> = std::sync::RwLock::new(None);

/// Sets the requested block-atlas mip depth and bumps [`pack_generation`] —
/// the mip-setting half of the trigger [`Sim::reload_resource_pack_atlas`]
/// polls, [`set_selected_packs`]'s sibling. Called from
/// `menu::nav::MenuNav`'s slider-drag and click-step writers for
/// `mipmapLevels`, never from the render crate: this module is the one place
/// that decides what depth the next atlas build asks for.
///
/// Clamped to `0..=`[`lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS`] — the
/// same `IntRange(0, 4)` bound `menu::options::INT_RANGE_SLIDERS`'s
/// `"mipmapLevels"` row places the handle with — so a caller cannot request a
/// depth the slider itself cannot reach.
///
/// [`Sim::reload_resource_pack_atlas`]: crate::sim::Sim::reload_resource_pack_atlas
pub fn set_mipmap_levels(levels: u32) {
    let levels = levels.min(lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS);
    if let Ok(mut guard) = MIPMAP_LEVELS.write() {
        *guard = Some(levels);
    }
    PACK_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// The current requested block-atlas mip depth, seeding itself from the
/// persisted `options.json` value on first use — [`selected_packs`]'s lazy
/// shape, for the same ordering reason: `Sim` (and the first atlas build) is
/// built before anything else has a natural place to push the persisted
/// setting in.
///
/// The seed call still bumps [`pack_generation`] like any other
/// [`set_mipmap_levels`] call, which looks like a false "changed" signal on
/// the very first read — it is not one in practice, because
/// `Sim::build` reads `pack_generation()` **after** the initial
/// [`BlockResources::load`] call (which is what triggers this seed), so the
/// bump is folded into `last_pack_generation`'s own starting value rather
/// than surfacing as a reload on the first frame. [`selected_packs`] already
/// relies on the identical ordering.
#[must_use]
pub fn mipmap_levels() -> u32 {
    if let Ok(guard) = MIPMAP_LEVELS.read() {
        if let Some(levels) = *guard {
            return levels;
        }
    }
    let levels = load_persisted_mipmap_levels();
    set_mipmap_levels(levels);
    levels
}

/// The persisted mip depth. A `#[cfg(test)]` **fork**, [`load_persisted_selection`]'s
/// reason: a unit test must never read the developer's real `options.json`.
#[cfg(not(test))]
fn load_persisted_mipmap_levels() -> u32 {
    crate::config::Options::load().mipmap_levels
}

#[cfg(test)]
fn load_persisted_mipmap_levels() -> u32 {
    lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS
}

/// The current selected-pack order, highest priority first, seeding itself from
/// [`crate::config::SelectedPacks`] on first use.
///
/// **Lazy rather than an explicit `init` call from `app::lifecycle`** because the
/// ordering would be a trap: `Sim` is built (and with it the block atlas, via
/// [`BlockResources::load`]) before `lifecycle` has a GPU, so an init placed
/// anywhere obvious would land *after* the first consumer and the selection
/// would silently miss the launch it was made on.
#[must_use]
pub fn selected_packs() -> Vec<String> {
    if let Ok(guard) = SELECTED_PACKS.read() {
        if let Some(ids) = guard.as_ref() {
            return ids.clone();
        }
    }
    let ids = load_persisted_selection();
    tracing::info!(target: "assets", packs = ids.len(), "loaded the selected resource pack order");
    set_selected_packs(ids.clone());
    ids
}

/// The persisted order. A `#[cfg(test)]` **fork**, not an early return on
/// `cfg!(test)`, so no unit test can read the developer's real
/// `resource_packs.json` and the interception is a property of the build rather
/// than a silent skip (`CLAUDE.md` §12.44). A test drives
/// [`set_selected_packs`] directly.
#[cfg(not(test))]
fn load_persisted_selection() -> Vec<String> {
    crate::config::SelectedPacks::load().into_ids()
}

#[cfg(test)]
fn load_persisted_selection() -> Vec<String> {
    Vec::new()
}

/// Opens every currently selected pack, **highest priority first**, dropping
/// any whose file has since disappeared.
///
/// # What a change here does and does not reach
///
/// Nothing in this module holds a live `ResourceManager`; each `load_*` opens
/// its own and is called from one place. So changing the selection takes effect
/// at each consumer's own next build:
///
/// - the **block atlas and per-state models** rebuild per session
///   (`sim/build.rs`'s `BlockResources::load`), i.e. on the next world join —
///   which is the visible acceptance condition for issue #415;
/// - the **GUI/menu atlases, item atlas, sky, container panels, weather, glint
///   and entity sheets** rebuild when their owner is next constructed
///   (`app/lifecycle.rs`, `menu/render/renderer.rs`, `gpu/entities.rs`);
/// - the **particle atlas** is the one exception: [`load_particle_atlas`] caches
///   in a `OnceLock` on purpose (two consumers must share one object), so it
///   keeps whatever stack was live at its first call for the rest of the
///   process.
#[must_use]
fn selected_pack_sources() -> Vec<Box<dyn ResourceSource>> {
    // The live server pack, if any, always goes first — highest priority,
    // matching vanilla's own `Pack.Position.TOP` for a downloaded pack. It
    // is never part of `selected`/`scan_resource_packs`: a server pack does
    // not live in the packs folder and is not player-toggleable from the
    // local Resource Packs screen, the same way vanilla's own
    // `DownloadedPackSource` keeps it out of `PackRepository`.
    let mut out: Vec<Box<dyn ResourceSource>> = server_pack_source().into_iter().collect();

    let selected = selected_packs();
    if selected.is_empty() {
        return out;
    }
    let discovered = scan_resource_packs();
    for id in selected {
        let Some(pack) = discovered.iter().find(|p| p.id == id) else {
            tracing::warn!(target: "assets", "selected pack {id} is no longer in the packs folder");
            continue;
        };
        if let Some(source) = open_pack_source(&pack.path, pack.kind) {
            out.push(source);
        }
    }
    out
}

/// Load real per-mob entity textures from `client.jar`, keyed by the render
/// crate's model name (`"pig"`, `"zombie"`, …). Version-free and **fail-open**:
/// returns an empty map when no pack is found or the jar can't be opened, so the
/// entity renderer keeps its synthetic-colour placeholder per model rather than
/// the run failing or a mob turning invisible.
///
/// For each baked model in the [`EntityModelSet`], the first
/// [`entity_texture_candidates`] path the jar actually contains is decoded to
/// RGBA8. A model whose sheet is missing (or that has no known sheet) is simply
/// absent from the map and falls back to the placeholder — the same
/// loud-but-non-fatal discipline the block atlas uses.
#[must_use]
pub fn load_entity_textures() -> std::collections::HashMap<&'static str, lodestone_assets::Image> {
    use lodestone_assets::Image;

    let mut out = std::collections::HashMap::new();
    let Some(manager) = open_vanilla_pack_stack() else {
        return out;
    };

    for (name, _mesh) in EntityModelSet::load().iter() {
        for path in entity_texture_candidates(name) {
            let Some(png) = manager.read(path) else {
                continue;
            };
            match Image::decode_png(&png) {
                Ok(img) => {
                    out.insert(name, img);
                    break;
                }
                Err(e) => {
                    tracing::warn!(target: "assets", "decode {path}: {e}");
                }
            }
        }
    }
    tracing::info!(
        target: "assets",
        loaded = out.len(),
        "loaded vanilla entity textures"
    );
    out
}

/// Decode every **variant** mob sheet, keyed by the corpus *reference*
/// (`entity/wolf/wolf_ashen`) that [`lodestone_render::entity_variant_sheet`]
/// resolves to.
///
/// Version-free and fail-open, exactly like [`load_entity_textures`]: an empty map
/// means no pack was found, and every mob then draws its model's default sheet — the
/// pre-existing behaviour, so a missing pack costs the refinement and never the mob.
///
/// # Keyed by reference, and loaded by *listing* rather than by enumeration
///
/// [`load_entity_textures`] keys by model name because one model has one default
/// sheet. A variant does not: nine wolf breeds and three climates share one mesh, so
/// the key has to be the sheet, the same choice [`load_block_entity_textures`]
/// makes.
///
/// The set is found by walking the pack for everything under
/// [`lodestone_render::entity_variant_sheet_dirs`]' prefixes, **not** by enumerating
/// the variant enums. That is deliberate: an enumeration would be a second table
/// beside the corpus's own `select` functions, free to drift the moment 26.3 adds a
/// breed, and it would have to be maintained in a crate that has no reason to know
/// how many wolves there are. Listing costs a few dozen extra PNG decodes at
/// startup and needs no change for a new variant at all.
///
/// This also loads sheets no variant currently resolves to (a wolf's `_tame` and
/// `_angry` files, for instance, which nothing asks for while the tame flag does not
/// reach the client). That is the *cheap* direction of the trade: an unused decoded
/// image costs memory, whereas a missing one costs a visibly wrong skin.
///
/// # The one enumerated set: the built-in player identities
///
/// The eighteen `DefaultPlayerSkin` sheets are the exception, and deliberately so.
/// They are not a variant axis at all — the player rigs carry a `Fixed` texture, so
/// their directories never appear in `entity_variant_sheet_dirs` — but they resolve
/// through this same by-reference map, because a player with no declared skin binds
/// one of them by name. The set is closed and vanilla owns the list, so it is read
/// from `lodestone_assets::skin::default_skins` by exact path: listing that
/// directory instead would make any stray sibling PNG a bindable identity.
#[must_use]
pub fn load_entity_variant_textures()
-> std::collections::HashMap<String, lodestone_assets::Image> {
    use lodestone_assets::Image;

    let mut out = std::collections::HashMap::new();
    let Some(manager) = open_vanilla_pack_stack() else {
        return out;
    };

    // The eighteen built-in player identities, by **exact path** rather than by
    // listing. They are not a variant axis — `player_wide`/`player_slim` carry a
    // `Fixed` texture, so `entity_variant_sheet_dirs` does not name their
    // directories — but they are consumed through this same by-reference map: a
    // player with no declared skin resolves `DefaultPlayerSkin.get(uuid)` and the
    // draw binds that sheet by name. Without these entries every such player falls
    // through to the model's own sheet and the whole hash pick collapses onto Steve
    // and Alex.
    //
    // Enumerated rather than listed because the set is closed and known: the array
    // *is* vanilla's `DEFAULT_SKINS`, and a stray sibling PNG under
    // `entity/player/wide/` must not become a bindable identity.
    for skin in lodestone_assets::skin::default_skins() {
        let reference = skin.texture;
        if out.contains_key(reference) {
            continue;
        }
        let path = format!("assets/minecraft/textures/{reference}.png");
        let Some(png) = manager.read(&path) else {
            tracing::warn!(
                target: "assets",
                "the pack ships no {path}; players hashing to that identity draw the \
                 model's own sheet instead"
            );
            continue;
        };
        match Image::decode_png(&png) {
            Ok(img) => {
                out.insert(reference.to_owned(), img);
            }
            Err(e) => {
                tracing::warn!(target: "assets", "decode {path}: {e}");
            }
        }
    }

    for dir in lodestone_render::entity_variant_sheet_dirs() {
        for path in manager.list(dir) {
            let Some(reference) = lodestone_render::sheet_reference_of(&path) else {
                // A `.mcmeta` sidecar or anything else that is not a sheet.
                continue;
            };
            let reference = reference.to_owned();
            if out.contains_key(&reference) {
                continue;
            }
            let Some(png) = manager.read(&path) else {
                continue;
            };
            match Image::decode_png(&png) {
                Ok(img) => {
                    out.insert(reference, img);
                }
                Err(e) => {
                    tracing::warn!(target: "assets", "decode {path}: {e}");
                }
            }
        }
    }
    tracing::info!(
        target: "assets",
        loaded = out.len(),
        "loaded vanilla entity variant textures"
    );
    out
}

/// Decode every **block-entity** sheet the renderer can ask for, keyed by the
/// same texture *stem* the renderer resolves (`entity/chest/normal_left`).
///
/// Version-free and fail-open: an empty map means no pack was found, and a chest
/// then draws nothing rather than a synthetic-colour box. See
/// `gpu/block_entities.rs`'s module doc for why the asymmetry with
/// [`load_entity_textures`]' placeholder is deliberate.
///
/// # Keyed by stem, not by model
///
/// [`load_entity_textures`] keys by *model name* because a mob's sheet is
/// determined by its model. A chest's is not: plain, trapped, christmas, ender
/// and four copper stages all share three meshes, so the key has to be the sheet.
/// Reusing the model-keyed shape here would load one sheet per mesh and draw
/// every trapped chest in plain oak.
///
/// # Why these are individual PNGs and not the chest *atlas*
///
/// 26.2 stitches `textures/entity/chest/*.png` into `textures/atlas/chest.png`
/// and `ChestRenderer` submits a `SpriteId` into it. The per-file PNGs are still
/// in the jar and each sprite **is** the whole 64×64 sheet, so the model's own
/// UVs (normalised against 64×64 by the bake) address a direct upload correctly
/// and identically. Going through the atlas would only add a UV remap this
/// renderer does not need.
#[must_use]
pub fn load_block_entity_textures()
-> std::collections::HashMap<&'static str, lodestone_assets::Image> {
    use lodestone_assets::Image;

    let mut out = std::collections::HashMap::new();
    let Some(manager) = open_vanilla_pack_stack() else {
        return out;
    };

    for stem in lodestone_render::block_entity_texture_stems() {
        let path = format!("assets/minecraft/textures/{stem}.png");
        let player_head_sheet = stem == "entity/player/wide/steve";
        let server_override = player_head_sheet
            && server_pack_source().is_some_and(|source| source.read(&path).is_some());
        let Some(png) = manager.read(&path) else {
            tracing::warn!(target: "assets", "missing block-entity sheet {path}");
            if player_head_sheet {
                tracing::debug!(
                    target: "pack_trace",
                    %path,
                    server_override,
                    "player-head fallback sheet is absent from the merged resource-pack stack"
                );
            }
            continue;
        };
        match Image::decode_png(&png) {
            Ok(img) => {
                if player_head_sheet {
                    tracing::debug!(
                        target: "pack_trace",
                        %path,
                        server_override,
                        width = img.width,
                        height = img.height,
                        "player-head fallback sheet decoded from the merged resource-pack stack"
                    );
                }
                out.insert(stem, img);
            }
            Err(e) => {
                tracing::warn!(target: "assets", "decode {path}: {e}");
                if player_head_sheet {
                    tracing::debug!(
                        target: "pack_trace",
                        %path,
                        server_override,
                        error = %e,
                        "player-head fallback sheet failed to decode; the player-head renderer has no usable default sheet"
                    );
                }
            }
        }
    }
    tracing::info!(
        target: "assets",
        loaded = out.len(),
        "loaded vanilla block-entity textures"
    );
    out
}

/// Load the vanilla GUI sprite atlas (`assets/<ns>/textures/gui/sprites/**`) from
/// `client.jar`, for the HUD. Version-free and fail-open: returns `None` when no
/// pack is found or the jar can't be opened/stitched, so the HUD keeps its
/// procedural fallback rather than the whole run failing. Only the jar is needed
/// (no `blocks.json`), so this succeeds even on a pack that can't build the block
/// atlas.
/// The recipe book's **panel background**, which is the one piece of its art
/// vanilla does not put through the sprite atlas.
///
/// `RecipeBookComponent.java` declares it as a raw texture path
/// (`RECIPE_BOOK_LOCATION = "textures/gui/recipe_book.png"`) and `:305` blits a
/// `147×166` window at `(1, 1)` out of the `256×256` sheet. Everything else the
/// book draws — the toggle button, tabs, filter, page arrows and recipe slots —
/// lives under `gui/sprites/recipe_book/**` and so is already in
/// [`GuiAtlas::build`]'s own enumeration with no help from this list.
///
/// It is an **extra** on the HUD's atlas rather than a second stitch because
/// the recipe-book panel draws through `HudRenderer`'s existing sprite pipeline
/// and bind group (see `HudRenderer::render_recipe_book_panel`); a separate
/// atlas would mean a second texture, bind group and pipeline for one quad.
/// Unlike [`MENU_TEXTURES`]'s 1024×256 logo, this is a 256×256 sheet, and the
/// concern recorded on [`load_menu_gui_atlas`] — that an extra repacks every
/// other sprite — is harmless here: nothing reads a UV as a constant. Every
/// consumer resolves UVs from the atlas at runtime.
pub const RECIPE_BOOK_TEXTURES: &[(&str, &str)] = &[(
    "recipe_book/panel",
    "assets/minecraft/textures/gui/recipe_book.png",
)];

/// The sprite id [`RECIPE_BOOK_TEXTURES`] registers the panel sheet under.
///
/// Deliberately *inside* the `recipe_book/` namespace but a name vanilla does
/// not use (vanilla has no `recipe_book/panel` sprite), so it can never collide
/// with a real sprite — and `build_with_extras` skips an extra whose id is
/// already claimed, which would otherwise fail silently.
pub const RECIPE_BOOK_PANEL_SPRITE: &str = "recipe_book/panel";

#[must_use]
pub fn load_gui_atlas() -> Option<Arc<GuiAtlas>> {
    let manager = open_vanilla_pack_stack()?;
    match GuiAtlas::build_with_extras(&manager, RECIPE_BOOK_TEXTURES) {
        Ok(atlas) => {
            tracing::info!(
                target: "assets",
                sprites = atlas.sprite_count(),
                "loaded vanilla GUI sprite atlas for the HUD"
            );
            Some(Arc::new(atlas))
        }
        Err(e) => {
            tracing::warn!(target: "assets", "build GUI atlas: {e}");
            None
        }
    }
}

/// Load and build the sky pass (celestial atlas + cloud texture, from
/// `client.jar`) via [`SkyRenderer::new`]. Version-free and fail-open like
/// [`load_gui_atlas`]: `None` on a jar-less run, or on a pack missing the
/// sun/moon/cloud textures, leaves [`crate::gpu::RenderState`] with no sky
/// installed — the pre-existing "clear straight to the fog colour" behaviour,
/// not a startup failure.
///
/// Unlike the other `load_*` helpers here, this one needs GPU handles
/// (`SkyRenderer::new` uploads the atlas/cloud textures immediately rather than
/// deferring to a later `attach_*` call): it does the `client.jar` IO this
/// crate's `gpu.rs` deliberately has none of, then hands `RenderState::install_sky`
/// an already-built renderer, mirroring how `RenderState::new` itself is handed
/// an already-built [`BlockAtlas`] rather than opening its own jar.
#[must_use]
pub fn load_sky(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    color_format: wgpu::TextureFormat,
) -> Option<SkyRenderer> {
    let manager = open_vanilla_pack_stack()?;
    match SkyRenderer::new(device, queue, color_format, &manager) {
        Ok(sky) => {
            tracing::info!(target: "assets", "loaded vanilla sky (sun/moon/stars/clouds)");
            Some(sky)
        }
        Err(e) => {
            tracing::warn!(target: "assets", "build sky renderer: {e}");
            None
        }
    }
}

/// Load and build the underwater/fire screen-overlay pass (`textures/misc/underwater.png`,
/// `textures/block/fire_1.png`, from `client.jar`) via [`ScreenEffectRenderer::new`].
/// Same shape as [`load_sky`]: fail-open, `None` on a jar-less run or a pack
/// missing either texture, leaving [`crate::gpu::RenderState`] with no overlay
/// pass installed rather than failing startup.
#[must_use]
pub fn load_screen_effects(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    color_format: wgpu::TextureFormat,
) -> Option<ScreenEffectRenderer> {
    let manager = open_vanilla_pack_stack()?;
    match ScreenEffectRenderer::new(device, queue, color_format, &manager) {
        Ok(fx) => {
            tracing::info!(target: "assets", "loaded underwater/fire screen overlays");
            Some(fx)
        }
        Err(e) => {
            tracing::warn!(target: "assets", "build screen-effect renderer: {e}");
            None
        }
    }
}

/// Load the two precipitation sheets (`textures/environment/rain.png`,
/// `textures/environment/snow.png`) for the weather pass.
///
/// Same shape as [`load_sky`] and [`load_screen_effects`] — fail-open, `None` on a
/// jar-less run or a pack missing either texture — with one difference worth
/// naming: unlike those two, this returns the **decoded images** rather than a
/// built renderer, because the weather pass needs the depth format of the
/// caller's own depth buffer and `crate::gpu::RenderState::install_weather` is the
/// only thing that knows it.
///
/// A `None` here is *not* "no weather": rain and thunder still darken the sky,
/// the fog and the lightmap, because that half is composed in `crate::app` from
/// scalars and needs no textures at all. Only the visible droplets are lost.
#[must_use]
pub fn load_weather_textures() -> Option<lodestone_render::WeatherTextures> {
    let manager = open_vanilla_pack_stack()?;
    match lodestone_render::load_weather_textures(&manager) {
        Ok(textures) => {
            tracing::info!(
                target: "assets",
                rain = format!("{}x{}", textures.rain.width, textures.rain.height),
                snow = format!("{}x{}", textures.snow.width, textures.snow.height),
                "loaded vanilla rain/snow textures"
            );
            Some(textures)
        }
        Err(e) => {
            tracing::warn!(target: "assets", "load weather textures: {e}");
            None
        }
    }
}

/// The beacon beam's scrolling texture (`textures/entity/beacon/beacon_beam.png`),
/// for [`crate::gpu`]'s beacon-beam pass. Same fail-open shape as
/// [`load_glint_texture`]: `None` on a jar-less run, and the pass simply
/// draws nothing rather than the run failing.
#[must_use]
pub fn load_beacon_beam_texture() -> Option<lodestone_assets::Image> {
    let manager = open_vanilla_pack_stack()?;
    let path = "assets/minecraft/textures/entity/beacon/beacon_beam.png";
    let png = manager.read(path)?;
    match lodestone_assets::Image::decode_png(&png) {
        Ok(img) => {
            tracing::info!(
                target: "assets",
                beacon_beam = format!("{}x{}", img.width, img.height),
                "loaded vanilla beacon-beam texture"
            );
            Some(img)
        }
        Err(e) => {
            tracing::warn!(target: "assets", "decode {path}: {e}");
            None
        }
    }
}

/// The end gateway teleport beam's scrolling texture
/// (`textures/entity/end_portal/end_gateway_beam.png`), for
/// [`crate::gpu`]'s beacon-beam pass — the same shader and pipeline the
/// beacon's own beam uses, bound to a second texture. Same fail-open shape
/// as [`load_beacon_beam_texture`].
#[must_use]
pub fn load_end_gateway_beam_texture() -> Option<lodestone_assets::Image> {
    let manager = open_vanilla_pack_stack()?;
    let path = "assets/minecraft/textures/entity/end_portal/end_gateway_beam.png";
    let png = manager.read(path)?;
    match lodestone_assets::Image::decode_png(&png) {
        Ok(img) => {
            tracing::info!(
                target: "assets",
                end_gateway_beam = format!("{}x{}", img.width, img.height),
                "loaded vanilla end-gateway-beam texture"
            );
            Some(img)
        }
        Err(e) => {
            tracing::warn!(target: "assets", "decode {path}: {e}");
            None
        }
    }
}

/// The end portal/end gateway star-field shader's two textures
/// (`textures/environment/end_sky.png`, `textures/entity/end_portal/end_portal.png`)
/// for [`crate::gpu`]'s end-portal pass. Same fail-open shape as
/// [`load_beacon_beam_texture`]: `None` on a jar-less run or a pack missing
/// either file, and the pass draws nothing rather than the run failing.
/// Bundled as one `Option<(sky, portal)>` rather than two separate loaders
/// because the pass needs both or neither — a partial load (one texture
/// present, one missing) has no sensible degraded rendering to fall back to.
#[must_use]
pub fn load_end_portal_textures() -> Option<(lodestone_assets::Image, lodestone_assets::Image)> {
    let manager = open_vanilla_pack_stack()?;
    let sky_path = "assets/minecraft/textures/environment/end_sky.png";
    let portal_path = "assets/minecraft/textures/entity/end_portal/end_portal.png";
    let sky_png = manager.read(sky_path)?;
    let portal_png = manager.read(portal_path)?;
    let sky = match lodestone_assets::Image::decode_png(&sky_png) {
        Ok(img) => img,
        Err(e) => {
            tracing::warn!(target: "assets", "decode {sky_path}: {e}");
            return None;
        }
    };
    let portal = match lodestone_assets::Image::decode_png(&portal_png) {
        Ok(img) => img,
        Err(e) => {
            tracing::warn!(target: "assets", "decode {portal_path}: {e}");
            return None;
        }
    };
    tracing::info!(
        target: "assets",
        end_sky = format!("{}x{}", sky.width, sky.height),
        end_portal = format!("{}x{}", portal.width, portal.height),
        "loaded vanilla end-portal star-field textures"
    );
    Some((sky, portal))
}

/// Decode vanilla's enchantment-glint sheet
/// (`assets/minecraft/textures/misc/enchanted_glint_item.png`) for the
/// first-person glint second pass.
///
/// Same shape as [`load_weather_textures`] — fail-open, `None` on a jar-less run
/// or a pack missing the texture — and, like it, returns the **decoded image**
/// rather than a built renderer: `crate::gpu::RenderState::install_glint` is the
/// only thing that knows the target colour format. The upload inside
/// `crate::gpu::glint::GlintPass` is deliberately `Rgba8Unorm` (non-sRGB) — the
/// mirror image of every diffuse loader — and that choice is justified in
/// `gpu/glint.rs`'s module doc, not here.
///
/// A `None` here is *not* "no glint possible": it is just "no glint texture, so
/// an enchanted held item renders without its shimmer", matching the
/// pass-not-installed convention `RenderState::glint` documents.
#[must_use]
pub fn load_glint_texture() -> Option<lodestone_assets::Image> {
    let manager = open_vanilla_pack_stack()?;
    let path = "assets/minecraft/textures/misc/enchanted_glint_item.png";
    let png = manager.read(path)?;
    match lodestone_assets::Image::decode_png(&png) {
        Ok(img) => {
            tracing::info!(
                target: "assets",
                glint = format!("{}x{}", img.width, img.height),
                "loaded vanilla enchantment-glint sheet"
            );
            Some(img)
        }
        Err(e) => {
            tracing::warn!(target: "assets", "decode {path}: {e}");
            None
        }
    }
}

/// The **loose** GUI textures the title screen needs, as
/// `(lookup id, in-pack path)` pairs for
/// [`GuiAtlas::build_with_extras`](lodestone_render::GuiAtlas::build_with_extras).
///
/// Vanilla's `LogoRenderer` blits these two by raw path
/// (`LogoRenderer.java`), not through the sprite atlas, so they live
/// outside `textures/gui/sprites/**` and [`load_gui_atlas`] can never see them.
///
/// Both are **hi-res** in 26.2 — `minecraft.png` is 1024×256 and `edition.png`
/// 512×64 — while vanilla declares them as 256×64 and 128×16 logical pixels and
/// blits only the top 44 / 14 rows. Everything below those cuts was measured
/// **fully transparent** (max alpha 0 over rows 176.. and 56.. of the real
/// files), so drawing the whole sprite stretched into a 256×64 / 128×16 logical
/// rect is pixel-identical to vanilla's sub-rect blit at the same origin — which
/// is why the menu needs no sub-rect blit primitive.
pub const TITLE_TEXTURES: &[(&str, &str)] = &[
    (
        "title/minecraft",
        "assets/minecraft/textures/gui/title/minecraft.png",
    ),
    (
        "title/edition",
        "assets/minecraft/textures/gui/title/edition.png",
    ),
];

/// The server list's fallback favicon —
/// `ServerSelectionList`'s `FaviconTexture.MISSING_ICON`, blitted at 32×32 for
/// any row whose server sent no usable icon.
///
/// Loose, like [`TITLE_TEXTURES`]: it lives at `textures/misc/`, so
/// [`load_gui_atlas`]'s `gui/sprites/**` glob structurally cannot see it. Not a
/// gap to "fix" by widening that glob — see this module's note at
/// [`load_gui_atlas`] and `container.rs`'s deliberate workaround.
pub const UNKNOWN_SERVER_TEXTURE: (&str, &str) = (
    "misc/unknown_server",
    "assets/minecraft/textures/misc/unknown_server.png",
);

/// The Resource Packs screen's fallback pack icon —
/// `PackSelectionScreen.DEFAULT_ICON` (`PackSelectionScreen.java`), blitted at
/// 32×32 for any pack that ships no readable `pack.png`, which includes the
/// built-in row.
///
/// Loose for [`UNKNOWN_SERVER_TEXTURE`]'s reason and in the same directory: it
/// lives at `textures/misc/`, outside `gui/sprites/**`.
pub const UNKNOWN_PACK_TEXTURE: (&str, &str) = (
    "misc/unknown_pack",
    "assets/minecraft/textures/misc/unknown_pack.png",
);

/// The shared book screen sheet — `BookViewScreen.BOOK_LOCATION`, also used by
/// `BookEditScreen` and `BookSignScreen`. It is a loose `256×256` texture;
/// each screen blits its top-left `192×192` logical region, so this cannot be
/// discovered by the `gui/sprites/**` atlas walk.
pub const BOOK_GUI_TEXTURE: (&str, &str) = (
    "book/background",
    "assets/minecraft/textures/gui/book.png",
);

/// `Screen.MENU_BACKGROUND`, the tiled raw texture behind every ordinary
/// out-of-world screen except the title screen itself. Vanilla's bundled file
/// is uniform black at alpha 64, but it is still a resource-pack override point
/// rather than a colour constant.
pub const MENU_BACKGROUND_TEXTURE: (&str, &str) = (
    "menu/background",
    "assets/minecraft/textures/gui/menu_background.png",
);

/// `Screen.INWORLD_MENU_BACKGROUND`, the raw tiled texture behind pause-style
/// screens. It is byte-identical to [`MENU_BACKGROUND_TEXTURE`] in vanilla
/// 26.2, but server packs may supply distinct in-world screen art.
pub const INWORLD_MENU_BACKGROUND_TEXTURE: (&str, &str) = (
    "menu/inworld_background",
    "assets/minecraft/textures/gui/inworld_menu_background.png",
);

/// Every loose texture the **menu** atlas carries: [`TITLE_TEXTURES`] plus
/// [`UNKNOWN_SERVER_TEXTURE`], [`UNKNOWN_PACK_TEXTURE`] and
/// [`BOOK_GUI_TEXTURE`], and the two raw full-screen backgrounds.
///
/// A superset rather than an addition to [`TITLE_TEXTURES`], because that
/// constant means "what `LogoRenderer` blits by path" and the two list fallback
/// icons are not that. The `assert!` below is a compile-time guard: this list
/// spells the title pair out by index, so a third title texture would otherwise
/// be dropped from the menu atlas silently.
pub const MENU_TEXTURES: &[(&str, &str)] = &[
    TITLE_TEXTURES[0],
    TITLE_TEXTURES[1],
    UNKNOWN_SERVER_TEXTURE,
    UNKNOWN_PACK_TEXTURE,
    BOOK_GUI_TEXTURE,
    MENU_BACKGROUND_TEXTURE,
    INWORLD_MENU_BACKGROUND_TEXTURE,
];

const _: () = assert!(
    TITLE_TEXTURES.len() == 2,
    "MENU_TEXTURES spells the title textures out by index"
);

/// As [`load_gui_atlas`], plus the [`MENU_TEXTURES`] the title screen and the
/// server list draw — the atlas the **menu** renderer binds.
///
/// Deliberately a second stitch rather than extras bolted onto
/// [`load_gui_atlas`]: that atlas is the HUD's, its sprite set is pinned by the
/// HUD's own pixel gates, and adding a 1024×256 logo to it would move every
/// other sprite's packing for no benefit — the menu renderer owns its own
/// pipeline and bind group anyway (see `menu/render.rs`), so it would not be
/// sharing the upload even if the contents matched. The duplication is one
/// extra GUI atlas (a few MB) and is noted in `docs/main-menu.md`; the tidier
/// end state is one shared atlas built with the extras and handed to both
/// renderers from `app.rs`.
#[must_use]
pub fn load_menu_gui_atlas() -> Option<Arc<GuiAtlas>> {
    let manager = open_vanilla_pack_stack()?;
    match GuiAtlas::build_with_extras(&manager, MENU_TEXTURES) {
        Ok(atlas) => {
            tracing::info!(
                target: "assets",
                sprites = atlas.sprite_count(),
                logo = atlas.contains("title/minecraft"),
                "loaded vanilla GUI sprite atlas for the menu screens"
            );
            Some(Arc::new(atlas))
        }
        Err(e) => {
            tracing::warn!(target: "assets", "build menu GUI atlas: {e}");
            None
        }
    }
}

/// An in-memory [`ObjectBytesSource`](crate::asset_objects::ObjectBytesSource)
/// over whatever jar-shadowed asset-object bytes `web/` managed to fetch and
/// install — see [`crate::platform::assets::Bundle::panorama`].
///
/// The wasm32 counterpart to [`crate::asset_objects::AssetObjectStore`]: there is
/// no filesystem to open a real store over in a browser, so this wraps a flat
/// `HashMap` built from whatever `(key, bytes)` pairs the bundle carries instead.
/// An absent key (bundle empty, or that particular face didn't stage) reads as
/// `None`, exactly as an unresolved store entry does — the caller,
/// `crate::menu::panorama::load`, already treats that as "fall back to the jar
/// stub for this face" with no wasm-specific branch of its own.
#[cfg(target_arch = "wasm32")]
struct WasmObjectBytes(std::collections::HashMap<String, Vec<u8>>);

#[cfg(target_arch = "wasm32")]
impl crate::asset_objects::ObjectBytesSource for WasmObjectBytes {
    fn object_bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.0.get(key).cloned()
    }
}

/// Load the title screen's panorama cubemap —
/// `textures/gui/title/background/panorama_{0..5}.png`, decoded and stacked into
/// cubemap layer order by [`crate::menu::panorama::load`].
///
/// **This is the one loader here that must not read `client.jar` first.** The jar
/// ships 69-byte 1×1 grey stubs for all six faces and the real 1024×1024 art
/// comes from the launcher's asset-object store. Native opens an
/// [`AssetObjectStore`](crate::asset_objects::AssetObjectStore) over the on-disk
/// root and hands it to `panorama::load`, which prefers it per face; wasm32 has
/// no filesystem to open one over, so it hands in a [`WasmObjectBytes`] built
/// from whatever `web/` fetched instead (see
/// [`crate::platform::assets::Bundle::panorama`]) — `web/Trunk.toml`'s
/// `post_build` hook is what populates *that*, resolved from the very same
/// `.cache/mc/<version>` store a native run reads directly. Either way, a root
/// (or bundle) with no populated store still loads — from the stubs, with a
/// warning that says so — because a flat title screen beats a failed startup.
/// `cargo run -p xtask -- fetch-assets --version <v>` is what populates the
/// on-disk store both routes ultimately read from.
///
/// Same fail-open contract as every other loader here otherwise: `None` on a
/// jar-less run, a missing face, or faces that disagree in size, which leaves the
/// menu screens on their flat backdrop rather than failing startup. The six faces
/// are *not* added to [`MENU_TEXTURES`] because they are not atlas sprites: a
/// cubemap has to be six equal layers of one texture, and stitching them into a
/// sheet is the one thing that would make them unusable.
#[must_use]
pub fn load_panorama() -> Option<Arc<crate::menu::panorama::PanoramaFaces>> {
    let manager = open_vanilla_pack_stack()?;
    // Absent or unreadable is not fatal: `panorama::load` falls back to the jar
    // per face and reports how many faces it actually got from the store.
    #[cfg(not(target_arch = "wasm32"))]
    let objects: Option<Box<dyn crate::asset_objects::ObjectBytesSource>> = asset_root()
        .and_then(|root| match crate::asset_objects::AssetObjectStore::open(&root) {
            Ok(store) => {
                Some(Box::new(store) as Box<dyn crate::asset_objects::ObjectBytesSource>)
            }
            Err(e) => {
                tracing::warn!(
                    target: "assets",
                    "no asset-object store at {}: {e} — the panorama will fall back to \
                     client.jar's 1x1 stub faces, which render a flat grey sky",
                    root.display()
                );
                None
            }
        });
    // `asset_root` is plain `std::fs` and always `None` in a browser (there is
    // no on-disk store there) — `WasmObjectBytes` is the wasm32 substitute,
    // built from whatever `web/`'s fetch actually landed.
    #[cfg(target_arch = "wasm32")]
    let objects: Option<Box<dyn crate::asset_objects::ObjectBytesSource>> =
        crate::platform::assets::bundle().and_then(|bundle| {
            if bundle.panorama.is_empty() {
                tracing::warn!(
                    target: "assets",
                    "no panorama faces staged for the browser build — the panorama will \
                     fall back to client.jar's 1x1 stub faces, which render a flat grey \
                     sky. Run: cargo run -p xtask -- fetch-assets --version <version>, \
                     then rebuild with `just run-wasm` so web/Trunk.toml's post_build \
                     hook can stage them"
                );
                None
            } else {
                Some(Box::new(WasmObjectBytes(bundle.panorama.iter().cloned().collect()))
                    as Box<dyn crate::asset_objects::ObjectBytesSource>)
            }
        });
    match crate::menu::panorama::load(&manager, objects.as_deref()) {
        Ok(faces) => {
            if faces.is_real_art() {
                tracing::info!(
                    target: "assets",
                    face = faces.size,
                    "loaded the title-screen panorama cubemap from the asset-object store"
                );
            } else {
                tracing::warn!(
                    target: "assets",
                    face = faces.size,
                    from_object_store = faces.from_object_store,
                    "the title-screen panorama fell back to client.jar stubs for {} of \
                     6 faces; the sky will be flat. Run: cargo run -p xtask -- \
                     fetch-assets --version <version>",
                    6 - faces.from_object_store
                );
            }
            Some(Arc::new(faces))
        }
        Err(e) => {
            tracing::warn!(target: "assets", "load panorama cubemap: {e}");
            None
        }
    }
}

/// Load vanilla's real container-panel art (issue #51):
/// `container/{generic_54,crafting_table,inventory}.png`, stitched into one
/// small atlas via [`crate::container::ContainerBackground`]. Version-free and
/// fail-open like every other loader here: `None` when no pack is found or the
/// jar can't be opened/stitched, so the container screen keeps its flat
/// programmatic fill rather than the whole run failing.
#[must_use]
pub fn load_container_background() -> Option<Arc<crate::container::ContainerBackground>> {
    let manager = open_vanilla_pack_stack()?;
    match crate::container::ContainerBackground::build(&manager) {
        Ok(background) => {
            tracing::info!(target: "assets", "loaded vanilla container background art");
            Some(Arc::new(background))
        }
        Err(e) => {
            tracing::warn!(target: "assets", "build container background atlas: {e}");
            None
        }
    }
}

/// Stitch the vanilla particle atlas from `client.jar`.
///
/// Mirrors [`load_item_atlas`] exactly, including its fail-open contract:
/// every failure path returns `None` and warns rather than propagating, because
/// a jar-less or headless run is a supported mode. With `None`, sheet-sourced
/// particles resolve to nothing and are *counted* into
/// `ParticleFrame::unresolved` — the gap stays visible in the HUD instead of
/// becoming a silent no-op.
///
/// Particle sprites need their own stitch: they are unreachable from any
/// blockstate, so the block atlas the terrain renderer owns never contains
/// them. Vanilla ships no pre-baked `particles.png` — 26.2 has 288 loose PNGs
/// under `textures/particle/` that the client stitches at runtime, exactly as
/// it does for blocks and items.
///
/// # Why this one is cached and the others are not
///
/// This atlas has **two consumers that must agree**: the emitter
/// ([`crate::particles::Particles::with_particle_atlas`]) reads its sprite
/// *rects* and the renderer
/// ([`crate::gpu::RenderState::install_particle_sheet_atlas`]) uploads its
/// *pixels*. Issue #45 was precisely a UV table addressing an image other than
/// the one bound, so handing both sides the same `Arc` — rather than two stitches
/// that happen to pack identically — makes that class of mismatch
/// unrepresentable instead of merely unlikely. The [`OnceLock`] is what buys
/// that: every caller in the process gets the same object.
///
/// (Two `AtlasBuilder` runs over one pack *are* byte-identical today — the
/// definition paths are sorted and deduplicated for exactly that reason — but
/// that is a property of the packer, not a guarantee, and the failure mode if it
/// ever changes is invisible in every counter.)
#[must_use]
pub fn load_particle_atlas() -> Option<Arc<ParticleAtlas>> {
    static CACHE: OnceLock<Option<Arc<ParticleAtlas>>> = OnceLock::new();
    CACHE.get_or_init(build_particle_atlas).clone()
}

fn build_particle_atlas() -> Option<Arc<ParticleAtlas>> {
    let manager = open_vanilla_pack_stack()?;
    let (atlas, report) = match ParticleAtlas::build_reported(&manager) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(target: "assets", "build particle atlas: {e}");
            return None;
        }
    };
    tracing::info!(
        target: "assets",
        definitions = report.definitions,
        sprites = report.sprites,
        missing_textures = report.missing_textures.len(),
        parse_errors = report.parse_errors.len(),
        "loaded vanilla particle atlas"
    );
    Some(Arc::new(atlas))
}

/// Builds the flat item-sprite [`ItemAtlas`] from the vanilla `client.jar`,
/// version-free, using the same pack discovery as [`load_gui_atlas`]. The atlas
/// turns each item id into a flat GUI sprite (the `item/generated` case, the
/// overwhelming majority); block-model and code-driven `special` items resolve
/// to no flat sprite and are reported, not treated as failures. Returns `None`
/// when no pack is found so the HUD simply draws empty wells rather than
/// panicking.
#[must_use]
pub fn load_item_atlas() -> Option<Arc<ItemAtlas>> {
    let manager = open_vanilla_pack_stack()?;
    let (atlas, report) = match ItemAtlas::build_reported(&manager) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(target: "assets", "build item atlas: {e}");
            return None;
        }
    };
    let pack_layers = manager.len().saturating_sub(1);
    tracing::info!(
        target: "assets",
        items = report.items,
        drawable = report.drawable,
        sprites = report.sprites,
        from_packs = report.layered_sprites,
        pack_layers,
        missing_textures = report.missing_textures.len(),
        parked_special = report.missing_special_bases.len(),
        "loaded vanilla item-sprite atlas for the HUD"
    );
    // The line that makes a silent fallback audible. A pack whose
    // `textures/item/*.png` never reached the stitch is pixel-identical to a
    // pack that overrides no item at all, and every other counter above agrees
    // in both cases — `sprites` counts what was stitched, not where it came
    // from. `from_packs = 0` with a pack on the stack is the whole symptom of
    // "the server's pack does not change any item art", stated once, at the
    // moment it happens, instead of costing an investigation.
    // A `Sprite`-layer texture an item definition references and the stack
    // cannot serve is a real failure by `ItemAtlasReport`'s own definition — a
    // generated item with no art, which draws as an empty well. It was counted
    // and never named, and a count cannot be acted on: with the vanilla jar
    // alone it is always 0, so any non-zero value here is something a *pack*
    // introduced, and the fix is always in that pack's own file tree. Measured:
    // one real third-party pack takes 714 stitched sprites down to 692 this way.
    if !report.missing_textures.is_empty() {
        let named: Vec<&str> = report
            .missing_textures
            .iter()
            .take(8)
            .map(String::as_str)
            .collect();
        tracing::warn!(
            target: "assets",
            missing = report.missing_textures.len(),
            pack_layers,
            "{} item sprite(s) an item definition references are absent from the \
             whole pack stack and were skipped; those items draw as empty wells. \
             First: {named:?}",
            report.missing_textures.len()
        );
    }
    if pack_layers > 0 && report.layered_sprites == 0 {
        tracing::warn!(
            target: "assets",
            pack_layers,
            sprites = report.sprites,
            "every item sprite in the atlas came from the built-in pack: the \
             resource pack layer(s) above it override no `textures/item/*.png` \
             that any item definition references, so item art is unchanged. If a \
             pack *is* installed and its item art should be showing, the stack \
             this atlas was built over is the stale half — not the stitch"
        );
    }
    if atlas.is_empty() {
        tracing::warn!(target: "assets", "item atlas is empty; drawing empty wells");
        return None;
    }
    Some(Arc::new(atlas))
}

/// Loads the real crafting-recipe and item-tag corpus from `client.jar`'s
/// `data/minecraft/{recipe,tags/item}/**` entries, version-free and fail-open
/// like every other loader in this module: `None` when no pack is found or the
/// jar can't be opened, so a jar-less/headless run simply has no recipe-book
/// prediction rather than a hard error. The crafting **result slot itself is
/// unaffected either way** — it is always the server's `container_set_slot`
/// that fills it (see `docs/crafting.md`); this corpus only feeds a local
/// prediction drawn when that slot is still empty.
///
/// Deliberately does **not** call [`lodestone_game::recipe_json::load_data_root`]:
/// that walks a real filesystem directory, and the corpus here lives inside a
/// **zip** (`client.jar`). [`ResourceManager::list`] already returns every
/// entry under a prefix regardless of nesting depth, so the "flat `read_dir`
/// drops nested tags" trap that function's own docs warn about (33 of 224 tags
/// live under `tags/item/enchantable/*`) does not apply here — a prefix filter
/// has no notion of depth to get wrong in the first place.
#[must_use]
pub fn load_recipe_book() -> Option<lodestone_game::recipe::RecipeBook> {
    use lodestone_game::recipe_json::CorpusBuilder;

    let manager = open_vanilla_pack_stack()?;

    let mut builder = CorpusBuilder::new();
    for path in manager.list("data/") {
        if let Some(id) = recipe_entry_id(&path, "recipe") {
            if let Some(text) = manager
                .read(&path)
                .and_then(|bytes| String::from_utf8(bytes).ok())
            {
                builder.push_recipe(id, &text);
            }
        } else if let Some(id) = recipe_entry_id(&path, "tags/item") {
            if let Some(text) = merged_tag_json(&manager, &path) {
                builder.push_tag(id, &text);
            }
        }
    }

    let recipes = builder.recipe_count();
    let tags = builder.tag_count();
    let failures = builder.failures().len();
    let book = builder.finish();
    tracing::info!(
        target: "assets",
        recipes,
        tags,
        failures,
        "loaded vanilla recipe corpus"
    );
    if book.is_empty() {
        tracing::warn!(target: "assets", "recipe corpus is empty; crafting predictions disabled");
        return None;
    }
    Some(book)
}

/// Parses an in-pack path `data/<namespace>/<kind>/<rest>.json` into the
/// `<namespace>:<rest>` [`Identifier`](lodestone_model::Identifier) vanilla's
/// own `FileToIdConverter` derives, or `None` if `path` is not under
/// `data/*/<kind>/` or is not a `.json` file. `kind` is `"recipe"` or
/// `"tags/item"`.
fn recipe_entry_id(path: &str, kind: &str) -> Option<lodestone_model::Identifier> {
    let rest = path.strip_prefix("data/")?;
    let (namespace, rest) = rest.split_once('/')?;
    let rest = rest.strip_prefix(kind)?.strip_prefix('/')?;
    let rest = rest.strip_suffix(".json")?;
    lodestone_model::Identifier::new(namespace, rest).ok()
}

/// Merges **every pack layer's own copy** of an item-tag document at `path`
/// into one synthesized `{"values": [...]}` document, the shape
/// `TagLoader.load` requires: it iterates
/// `lister.listMatchingResourceStacks(resourceManager)` — every layer that
/// carries the path, lowest priority first — accumulating each layer's
/// `values` and honouring its own `"replace"` flag (`true` resets the
/// accumulated list before appending that layer's own entries; the default,
/// `false`, appends to what came before). A single-winner `manager.read()`
/// (what this replaced) discarded every lower-priority layer outright, so a
/// server pack shipping its own `data/minecraft/tags/item/planks.json` with
/// `"replace": false` and one custom entry would have **dropped every
/// vanilla plank** from the tag instead of adding to it.
///
/// A layer whose bytes are not valid UTF-8/JSON, or whose document has no
/// `values` array, is skipped — matching `TagLoader.load`'s own
/// `catch (Exception e) { LOGGER.error(...); }` per-entry tolerance — so one
/// malformed layer never blanks out the layers around it. Returns `None`
/// only when no layer at all could be read.
fn merged_tag_json(manager: &ResourceManager, path: &str) -> Option<String> {
    let mut values: Vec<serde_json::Value> = Vec::new();
    let mut any = false;
    for bytes in manager.read_stack(path) {
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(layer_values) = doc.get("values").and_then(serde_json::Value::as_array) else {
            continue;
        };
        any = true;
        let replace = doc
            .get("replace")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if replace {
            values.clear();
        }
        values.extend(layer_values.iter().cloned());
    }
    if !any {
        return None;
    }
    Some(serde_json::json!({ "values": values }).to_string())
}

/// Open the vanilla `client.jar` as a [`ResourceManager`], version-free, using
/// the same discovery as the atlas loaders. Returns `None` when no pack is found,
/// so a caller fails *closed and loud* rather than silently substituting
/// something.
///
/// Used by GPU gates (build a [`GuiAtlas`] and read the raw sprite PNGs from one
/// manager, to compare rendered pixels against source art) **and** by production
/// loaders that need a jar-backed manager of their own —
/// `gpu::entities::load_trim_sprites` is the first.
///
/// **This was `#[cfg(test)]`, and four copies of its pack-discovery rule grew while it
/// was** — in `gpu::entities`' three jar loaders and `hud::vanilla_font::jar_manager`,
/// each with a comment asking for exactly that attribute change. All four now call
/// this function and the copies are deleted.
///
/// Keep it that way, and the reason is stronger than de-duplication: this is the only
/// place that knows a **browser** session's jar arrives as `fetch`ed bytes through
/// [`crate::platform::assets`] rather than from a path. A new hand-rolled copy would
/// find no pack in a browser and return `None`, and every caller here treats `None` as
/// "draw the fallback" — so the failure would be a title screen with no glyphs, or
/// armourless players, with nothing in the log to say why.
pub(crate) fn vanilla_manager() -> Option<ResourceManager> {
    // Browser: the jar bytes were `fetch`ed and installed by `web/` before the app
    // started. This is the choke point every `load_*` helper in this module reaches —
    // fonts, the GUI atlas, the panorama, entity textures, the item and particle
    // atlases, the recipe corpus — so routing it here is what makes the browser draw
    // a readable title screen rather than an untextured one. Only the byte
    // acquisition differs; `ZipSource::from_bytes` below is shared.
    #[cfg(target_arch = "wasm32")]
    let bytes = crate::platform::assets::bundle()?.client_jar.clone();
    #[cfg(not(target_arch = "wasm32"))]
    let bytes = {
        let root = asset_root()?;
        let jar = root.join("client.jar");
        std::fs::read(&jar).ok()?
    };
    let zip = ZipSource::from_bytes(bytes).ok()?;
    Some(ResourceManager::new(vec![
        Box::new(zip) as Box<dyn ResourceSource>
    ]))
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

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_assets::MemorySource;

    /// The bug this reproduces: a server pack ships its own
    /// `data/minecraft/tags/item/planks.json` with `"replace": false` (the
    /// default) and one custom entry. A single-winner `manager.read()` load
    /// (what [`merged_tag_json`] replaced) discarded the jar's own layer
    /// outright, so the correct rule (merge, honouring `replace`) must yield
    /// a *different* result from the wrong one — two packs whose tag
    /// documents are the same would not distinguish them.
    #[test]
    fn merged_tag_json_appends_a_non_replacing_overlay_to_the_base_layer() {
        const PATH: &str = "data/minecraft/tags/item/planks.json";
        let mut vanilla = MemorySource::new("vanilla");
        vanilla.insert(PATH, br#"{"values":["minecraft:oak_planks","minecraft:spruce_planks"]}"#.to_vec());
        let mut server_pack = MemorySource::new("server");
        server_pack.insert(PATH, br#"{"replace":false,"values":["mymod:custom_planks"]}"#.to_vec());
        let manager = ResourceManager::new(vec![Box::new(vanilla), Box::new(server_pack)]);

        let merged = merged_tag_json(&manager, PATH).expect("at least one layer present");
        let parsed: serde_json::Value = serde_json::from_str(&merged).expect("valid json");
        let values: Vec<&str> = parsed["values"]
            .as_array()
            .expect("values array")
            .iter()
            .map(|v| v.as_str().expect("string entry"))
            .collect();
        assert_eq!(
            values,
            vec!["minecraft:oak_planks", "minecraft:spruce_planks", "mymod:custom_planks"],
            "a non-replacing overlay must append to, not discard, the base layer's entries"
        );

        // The control: a naive single-winner read (what this replaced) sees
        // only the highest-priority layer and loses every vanilla entry.
        let winner_only = manager.read(PATH).and_then(|b| String::from_utf8(b).ok()).unwrap();
        let winner_parsed: serde_json::Value = serde_json::from_str(&winner_only).unwrap();
        let winner_values: Vec<&str> = winner_parsed["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            winner_values,
            vec!["mymod:custom_planks"],
            "control: single-winner read must NOT see the vanilla layer's entries \
             (this is the exact bug `merged_tag_json` fixes) — got {winner_values:?}"
        );
    }

    /// `"replace": true` on the higher-priority layer must reset the
    /// accumulated list rather than append to it — the other half of
    /// `TagFile.replace()`'s contract, and the case a pure-append
    /// implementation would get backwards.
    #[test]
    fn merged_tag_json_honours_a_replacing_overlay() {
        const PATH: &str = "data/minecraft/tags/item/planks.json";
        let mut vanilla = MemorySource::new("vanilla");
        vanilla.insert(PATH, br#"{"values":["minecraft:oak_planks"]}"#.to_vec());
        let mut server_pack = MemorySource::new("server");
        server_pack.insert(
            PATH,
            br#"{"replace":true,"values":["mymod:only_plank"]}"#.to_vec(),
        );
        let manager = ResourceManager::new(vec![Box::new(vanilla), Box::new(server_pack)]);

        let merged = merged_tag_json(&manager, PATH).expect("at least one layer present");
        let parsed: serde_json::Value = serde_json::from_str(&merged).unwrap();
        let values: Vec<&str> = parsed["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            values,
            vec!["mymod:only_plank"],
            "a replacing overlay must reset the accumulated list, not append to it"
        );
    }

    #[test]
    fn merged_tag_json_returns_none_when_no_layer_has_the_path() {
        let manager = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        assert!(merged_tag_json(&manager, "data/minecraft/tags/item/planks.json").is_none());
    }

    /// [`pack_generation`] is the equality guard `Sim::reload_resource_pack_atlas`
    /// polls every frame, so it must actually move on every
    /// [`set_selected_packs`] call — a generation that never changes would make
    /// that guard permanently "nothing changed" and the whole live-reload path
    /// silently dead. `PACK_GENERATION` is process-global, so this only checks
    /// monotonic increase across two calls made back to back, never an absolute
    /// value — a fixed starting point would be flaky against any other test in
    /// this binary that also calls `set_selected_packs`.
    #[test]
    fn pack_generation_strictly_increases_on_every_selection_change() {
        let before = pack_generation();
        set_selected_packs(vec!["a".to_string()]);
        let after_one = pack_generation();
        assert!(
            after_one > before,
            "generation did not move: before={before}, after_one={after_one}"
        );
        set_selected_packs(vec!["a".to_string(), "b".to_string()]);
        let after_two = pack_generation();
        assert!(
            after_two > after_one,
            "generation did not move on a second change: after_one={after_one}, \
             after_two={after_two}"
        );
    }

    /// [`set_mipmap_levels`] must move [`pack_generation`] too — it is the
    /// mip-setting half of the same trigger [`set_selected_packs`] drives, and a
    /// generation that never moved would make `Sim::reload_resource_pack_atlas`
    /// think nothing changed forever, the same silent-island shape
    /// `pack_generation_strictly_increases_on_every_selection_change` guards for
    /// pack selection.
    #[test]
    fn mipmap_level_changes_also_move_the_shared_generation() {
        let before = pack_generation();
        set_mipmap_levels(2);
        let after = pack_generation();
        assert!(
            after > before,
            "generation did not move on a mip-level change: before={before}, after={after}"
        );
    }

    /// The getter must actually return whatever the setter just stored — a
    /// generation bump alone is not evidence the *value* a live reload would
    /// rebuild against moved too; that would be `pack_generation`'s own gate
    /// passing while `mipmap_levels()` silently kept returning the old depth.
    #[test]
    fn mipmap_levels_reads_back_what_was_just_set() {
        set_mipmap_levels(1);
        assert_eq!(mipmap_levels(), 1);
        set_mipmap_levels(3);
        assert_eq!(mipmap_levels(), 3);
    }

    /// A depth above the shipped max must be clamped, not passed through — an
    /// unclamped caller could otherwise request a level count the `mipmapLevels`
    /// slider itself has no track position for.
    #[test]
    fn mipmap_levels_clamps_to_the_shipped_max() {
        set_mipmap_levels(9);
        assert_eq!(
            mipmap_levels(),
            lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS
        );
    }

    /// Builds a real, valid single-entry zip archive in memory — the same
    /// parser `ZipSource::from_bytes` (and therefore `set_server_pack`) reads
    /// production packs through, not a hand-rolled byte string standing in
    /// for one. `path` is the in-archive path; `contents` is what a
    /// `read(path)` on the resulting [`ZipSource`] must return.
    fn build_test_pack_zip(path: &str, contents: &[u8]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file(path, zip::write::SimpleFileOptions::default())
            .expect("start_file");
        std::io::Write::write_all(&mut writer, contents).expect("write pack entry");
        writer
            .finish()
            .expect("finish zip")
            .into_inner()
    }

    /// `set_server_pack` must actually reach `selected_pack_sources` — the
    /// chain `net.rs`'s downloader depends on to make textures change, and
    /// the whole point of the feature over a dialog that lies. Proven by
    /// **content**, not by a generation bump alone (`pack_generation`
    /// increasing is necessary but not sufficient — see the sibling tests
    /// above for why a bump alone is not evidence the *value* moved): the
    /// installed pack's own bytes, read back through the exact function the
    /// block-atlas loader calls, must be the fixture's bytes and not some
    /// other pack's.
    #[test]
    fn a_set_server_pack_reaches_selected_pack_sources_and_outranks_local_selection() {
        let id = Uuid::from_u128(0xF00D);
        let marker = b"a distinctive server-pushed texture, not the local one";
        let zip = build_test_pack_zip("assets/minecraft/textures/block/marker.png", marker);
        assert!(
            set_server_pack(id, zip),
            "a real, valid zip must be accepted"
        );

        let sources = selected_pack_sources();
        assert!(
            !sources.is_empty(),
            "the server pack must appear even with no local pack selected"
        );
        let read_back = sources[0]
            .read("assets/minecraft/textures/block/marker.png")
            .expect("the server pack's own entry must be readable");
        assert_eq!(
            read_back, marker,
            "selected_pack_sources' first source did not read back the server \
             pack's own bytes — a stale/other pack would fail this, and an \
             empty stack would panic on the `expect` above"
        );

        // It must be **first** (highest priority) even when a local
        // selection is also present — vanilla's own `Pack.Position.TOP` for
        // a downloaded pack.
        set_selected_packs(vec!["some-local-pack".to_string()]);
        let sources = selected_pack_sources();
        assert!(
            sources[0]
                .read("assets/minecraft/textures/block/marker.png")
                .is_some(),
            "the server pack must still be first ahead of an (unresolvable, \
             but present-in-the-selection) local pack"
        );

        clear_server_pack(None);
        set_selected_packs(vec![]);
    }

    /// The pixels of one stitched sprite, for comparing two sprites in the same
    /// atlas without needing a PNG decoder or a hand-written expected image.
    fn sprite_pixels(atlas: &ItemAtlas, id: &str) -> (u32, u32, Vec<u8>) {
        let loc = lodestone_assets::ResourceLocation::parse(id).expect("valid location");
        let sprite = atlas
            .sprite(&loc)
            .unwrap_or_else(|| panic!("{id} is not in the stitched item atlas"));
        let img = atlas.atlas();
        let mut out = Vec::with_capacity((sprite.width * sprite.height * 4) as usize);
        for row in 0..sprite.height {
            let start = (((sprite.y + row) * img.width + sprite.x) * 4) as usize;
            out.extend_from_slice(&img.rgba[start..start + (sprite.width * 4) as usize]);
        }
        (sprite.width, sprite.height, out)
    }

    /// **The acceptance condition for "a server-pushed pack re-textures items".**
    ///
    /// `a_set_server_pack_reaches_selected_pack_sources_and_outranks_local_selection`
    /// above proves the pack reaches the *source list*; it says nothing about
    /// whether the item atlas — a separate stitch with its own owner, built by
    /// [`load_item_atlas`] rather than by the block-atlas loader — reads through
    /// that list. This closes the rest of the chain, through the production
    /// functions rather than a reassembled equivalent: `set_server_pack` (what
    /// `net.rs`'s downloader calls) then `load_item_atlas` (what both the HUD
    /// bring-up and the live-reload block call), with nothing in between.
    ///
    /// # The expected value comes from outside this code
    ///
    /// The pack serves the jar's **own `item/stick.png` bytes** as
    /// `item/diamond.png`. So the assertion is that two sprites vanilla ships as
    /// different art become byte-identical, which no encode/decode symmetry of
    /// ours can satisfy by accident, and which needs no PNG writer. The control
    /// is the same comparison with no pack installed: it must differ, or the
    /// post-install equality would prove nothing.
    ///
    /// `#[ignore]`d for the reason every vanilla gate here is: it needs a real
    /// `client.jar`, which is not repo state, and a missing one must fail loud
    /// rather than pass vacuously. It also mutates the process-global
    /// `SERVER_PACK`, so run it single-threaded alongside its siblings.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn a_server_pushed_pack_retextures_the_item_atlas() {
        const DIAMOND_PNG: &str = "assets/minecraft/textures/item/diamond.png";
        const STICK_PNG: &str = "assets/minecraft/textures/item/stick.png";

        clear_server_pack(None);
        set_selected_packs(Vec::new());

        let manager = open_vanilla_pack_stack().expect(
            "no vanilla pack found; set LODESTONE_ASSETS to a root with client.jar",
        );
        let stick_bytes = manager
            .read(STICK_PNG)
            .expect("the jar must carry item/stick.png");

        // Control: without a pack the two are different art, so the equality
        // asserted below is a real observation rather than a tautology.
        let plain = load_item_atlas().expect("the item atlas must build from client.jar");
        let plain_diamond = sprite_pixels(&plain, "minecraft:item/diamond");
        let plain_stick = sprite_pixels(&plain, "minecraft:item/stick");
        assert_ne!(
            plain_diamond, plain_stick,
            "control failed: the jar's diamond and stick sprites are already \
             identical, so this gate could not tell an override from a no-op"
        );

        assert!(
            set_server_pack(
                Uuid::from_u128(0x17E_D1A),
                build_test_pack_zip(DIAMOND_PNG, &stick_bytes)
            ),
            "a real, valid zip must be accepted"
        );

        let packed = load_item_atlas().expect("the item atlas must still build with a pack");
        let packed_diamond = sprite_pixels(&packed, "minecraft:item/diamond");
        let packed_stick = sprite_pixels(&packed, "minecraft:item/stick");
        clear_server_pack(None);

        assert_eq!(
            packed_diamond, packed_stick,
            "the server pack's textures/item/diamond.png did not reach the item \
             atlas: the diamond sprite still differs from the stick art the pack \
             served for it, so load_item_atlas built over a stack without the pack"
        );
        assert_ne!(
            packed_diamond, plain_diamond,
            "the diamond sprite is unchanged from the pack-free build"
        );
    }

    /// `ItemAtlasReport::layered_sprites` is what `load_item_atlas` warns on, so
    /// it has to move with reality rather than being a field nobody computes:
    /// zero with no pack, non-zero the moment a pack carries an item texture an
    /// item definition references. Without this the warning could be silently
    /// permanent (always zero) or silently vacuous (never zero) and the log line
    /// would be worse than none.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn the_item_atlas_reports_how_many_sprites_a_pack_layer_served() {
        const DIAMOND_PNG: &str = "assets/minecraft/textures/item/diamond.png";

        clear_server_pack(None);
        set_selected_packs(Vec::new());

        let manager = open_vanilla_pack_stack().expect("no vanilla pack found");
        let stick_bytes = manager
            .read("assets/minecraft/textures/item/stick.png")
            .expect("the jar must carry item/stick.png");
        let (_, bare) = ItemAtlas::build_reported(&manager).expect("build");
        assert_eq!(
            bare.layered_sprites, 0,
            "with only the built-in pack on the stack nothing can have come from a layer above it"
        );

        assert!(set_server_pack(
            Uuid::from_u128(0x17E_D1B),
            build_test_pack_zip(DIAMOND_PNG, &stick_bytes)
        ));
        let manager = open_vanilla_pack_stack().expect("no vanilla pack found");
        let (_, layered) = ItemAtlas::build_reported(&manager).expect("build");
        clear_server_pack(None);
        assert_eq!(
            layered.layered_sprites, 1,
            "the pack carries exactly one referenced item texture, so exactly one \
             stitched sprite must be attributed to it"
        );
    }

    /// A corrupt/truncated download must not be installed — `set_server_pack`
    /// is the point that turns `FAILED_DOWNLOAD` bytes into
    /// `FAILED_RELOAD`'s "installed but unusable", so this is the boundary
    /// that has to refuse them.
    #[test]
    fn a_corrupt_pack_is_refused_and_never_reaches_selected_pack_sources() {
        let id = Uuid::from_u128(0xBAD);
        let garbage = b"not a zip file at all".to_vec();
        assert!(!set_server_pack(id, garbage), "corrupt bytes must be refused");
        // And refusing it must not have installed a poisoned entry that a
        // later read would trip over.
        let sources = selected_pack_sources();
        assert!(
            sources.is_empty() || sources[0].read("assets/minecraft/textures/block/marker.png").is_none(),
            "a refused pack must not appear in the stack"
        );
    }

    /// `clear_server_pack` must only remove the pack it names — a stale pop
    /// for a superseded id must not remove a *newer* push that raced with
    /// it, and `None` (a bare pop-all) must remove whatever is live.
    #[test]
    fn clear_server_pack_only_clears_a_matching_id() {
        let old_id = Uuid::from_u128(1);
        let new_id = Uuid::from_u128(2);
        assert!(set_server_pack(
            old_id,
            build_test_pack_zip("a.txt", b"old")
        ));
        assert!(set_server_pack(
            new_id,
            build_test_pack_zip("a.txt", b"new")
        ));

        // A pop for the superseded id must not remove the live (newer) one.
        clear_server_pack(Some(old_id));
        let sources = selected_pack_sources();
        assert_eq!(
            sources.first().and_then(|s| s.read("a.txt")),
            Some(b"new".to_vec()),
            "a stale pop must not have removed the newer push"
        );

        // The matching id does clear it.
        clear_server_pack(Some(new_id));
        assert!(selected_pack_sources().is_empty());
    }

    /// The vanilla load must carry a real `en_us.json` that resolves known keys,
    /// proving the shell's classifier and its translation table come from the
    /// same pack. Ignored without assets so a missing pack fails loud rather than
    /// masquerading as a pass.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn vanilla_load_carries_a_resolving_language_table() {
        let resources = BlockResources::load(true);
        assert!(
            resources.vanilla_atlas.is_some(),
            "vanilla assets did not load; set LODESTONE_ASSETS to a pack root with \
             client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        );
        let lang = resources
            .language
            .expect("vanilla load produced no language table");
        assert!(
            lang.len() > 1000,
            "en_us.json looks truncated: {} keys",
            lang.len()
        );
        // A raw key the death message uses must lower to real words.
        assert_eq!(lang.get("entity.minecraft.spider"), Some("Spider"));
        assert!(
            lang.get("death.attack.mob").is_some(),
            "the death-message format key is missing from the loaded table"
        );
    }

    /// The real entity sheets must load from the jar with vanilla dimensions and
    /// carry actual art — not a uniform colour that a placeholder would produce.
    /// Ignored without a pack so a missing jar fails loud rather than passing
    /// vacuously. This is the plumbing gate; the on-screen gate is the live
    /// screenshot.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn entity_textures_load_real_art_from_the_jar() {
        let textures = load_entity_textures();
        assert!(
            !textures.is_empty(),
            "no entity textures loaded; set LODESTONE_ASSETS to a pack root with client.jar"
        );

        // The humanoid zombie sheet is 64×64 in modern packs; the pig sheet too.
        let zombie = textures.get("zombie").expect("zombie sheet must load");
        assert_eq!(
            (zombie.width, zombie.height),
            (64, 64),
            "zombie sheet is not the expected 64×64"
        );

        // A placeholder is one flat colour; real art is not. Count distinct RGB
        // pixels and require many — this is the check that a synthetic 2×2 (or a
        // solid fill) can never pass.
        let distinct = {
            use std::collections::HashSet;
            let mut set = HashSet::new();
            for px in zombie.rgba.chunks_exact(4) {
                set.insert([px[0], px[1], px[2]]);
            }
            set.len()
        };
        assert!(
            distinct > 20,
            "zombie sheet has only {distinct} distinct colours — looks like a placeholder, not art"
        );

        // The plains farm mobs resolve their 26.2 temperature variant.
        assert!(textures.contains_key("pig"), "pig sheet must load");
        assert!(textures.contains_key("cow"), "cow sheet must load");
    }
}
