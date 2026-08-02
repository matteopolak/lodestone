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
    blocks_json_registry, entity_texture_candidates,
};

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
        let root = asset_root().ok_or_else(|| {
            "no vanilla resource pack found — set LODESTONE_ASSETS to a pack root \
             containing client.jar + generated/reports/blocks.json (live world uses \
             the demo palette until then)"
                .to_string()
        })?;
        let jar = root.join("client.jar");
        let report = root.join("generated/reports/blocks.json");

        let bytes = std::fs::read(&jar).map_err(|e| format!("read {}: {e}", jar.display()))?;
        let zip =
            ZipSource::from_bytes(bytes).map_err(|e| format!("open {}: {e}", jar.display()))?;
        let manager = ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>]);
        let registry =
            blocks_json_registry(&report).map_err(|e| format!("load {}: {e}", report.display()))?;
        let atlas = BlockAtlas::build(&manager, &registry)
            .map_err(|e| format!("build atlas from {}: {e}", root.display()))?;
        // Bake the per-state model geometry (cross-plants, slabs, stairs,
        // translucency) against the same registry and attach it, so the model
        // render path resolves state ids to real quads instead of full cubes.
        let models = BlockModels::build(&manager, &registry)
            .map_err(|e| format!("build models from {}: {e}", root.display()))?;
        // The language table shares the jar; a missing or malformed file just
        // disables translation rather than failing the whole live load.
        let language = manager
            .read(&Language::resource_path("minecraft", "en_us"))
            .and_then(|bytes| match Language::from_json_bytes(&bytes) {
                Ok(lang) => Some(lang),
                Err(e) => {
                    tracing::warn!(target: "assets", "parse en_us.json: {e}");
                    None
                }
            });
        Ok((atlas.with_models(models), language))
    }
}

/// Opens `<root>/client.jar` as a [`ResourceManager`], warning and returning
/// `None` on either failure — the fail-open jar discovery every loader in this
/// module shares below [`BlockResources::try_vanilla`], whose own errors must
/// propagate as a fallback-banner reason instead of a log line, so it opens
/// the jar itself rather than going through this helper.
fn open_client_jar(root: &Path) -> Option<ResourceManager> {
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
    Some(ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>]))
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
    let Some(root) = asset_root() else {
        return out;
    };
    let Some(manager) = open_client_jar(&root) else {
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
    let Some(root) = asset_root() else {
        return out;
    };
    let Some(manager) = open_client_jar(&root) else {
        return out;
    };

    for stem in lodestone_render::chest_texture_stems() {
        let path = format!("assets/minecraft/textures/{stem}.png");
        let Some(png) = manager.read(&path) else {
            tracing::warn!(target: "assets", "missing block-entity sheet {path}");
            continue;
        };
        match Image::decode_png(&png) {
            Ok(img) => {
                out.insert(stem, img);
            }
            Err(e) => tracing::warn!(target: "assets", "decode {path}: {e}"),
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
#[must_use]
pub fn load_gui_atlas() -> Option<Arc<GuiAtlas>> {
    let root = asset_root()?;
    let manager = open_client_jar(&root)?;
    match GuiAtlas::build(&manager) {
        Ok(atlas) => {
            tracing::info!(
                target: "assets",
                sprites = atlas.sprite_count(),
                "loaded vanilla GUI sprite atlas for the HUD"
            );
            Some(Arc::new(atlas))
        }
        Err(e) => {
            tracing::warn!(target: "assets", "build GUI atlas from {}: {e}", root.display());
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
    let root = asset_root()?;
    let manager = open_client_jar(&root)?;
    match SkyRenderer::new(device, queue, color_format, &manager) {
        Ok(sky) => {
            tracing::info!(target: "assets", "loaded vanilla sky (sun/moon/stars/clouds)");
            Some(sky)
        }
        Err(e) => {
            tracing::warn!(target: "assets", "build sky renderer from {}: {e}", root.display());
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
    let root = asset_root()?;
    let manager = open_client_jar(&root)?;
    match ScreenEffectRenderer::new(device, queue, color_format, &manager) {
        Ok(fx) => {
            tracing::info!(target: "assets", "loaded underwater/fire screen overlays");
            Some(fx)
        }
        Err(e) => {
            tracing::warn!(target: "assets", "build screen-effect renderer from {}: {e}", root.display());
            None
        }
    }
}

/// The **loose** GUI textures the title screen needs, as
/// `(lookup id, in-pack path)` pairs for
/// [`GuiAtlas::build_with_extras`](lodestone_render::GuiAtlas::build_with_extras).
///
/// Vanilla's `LogoRenderer` blits these two by raw path
/// (`LogoRenderer.java:10-12`), not through the sprite atlas, so they live
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

/// Every loose texture the **menu** atlas carries: [`TITLE_TEXTURES`] plus
/// [`UNKNOWN_SERVER_TEXTURE`].
///
/// A superset rather than an addition to [`TITLE_TEXTURES`], because that
/// constant means "what `LogoRenderer` blits by path" and the server list's
/// fallback icon is not that. The `assert!` below is a compile-time guard: this
/// list spells the title pair out by index, so a third title texture would
/// otherwise be dropped from the menu atlas silently.
pub const MENU_TEXTURES: &[(&str, &str)] = &[
    TITLE_TEXTURES[0],
    TITLE_TEXTURES[1],
    UNKNOWN_SERVER_TEXTURE,
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
    let root = asset_root()?;
    let manager = open_client_jar(&root)?;
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
            tracing::warn!(target: "assets", "build menu GUI atlas from {}: {e}", root.display());
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
    let root = asset_root()?;
    let manager = open_client_jar(&root)?;
    match crate::container::ContainerBackground::build(&manager) {
        Ok(background) => {
            tracing::info!(target: "assets", "loaded vanilla container background art");
            Some(Arc::new(background))
        }
        Err(e) => {
            tracing::warn!(
                target: "assets",
                "build container background atlas from {}: {e}",
                root.display()
            );
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
/// them. Vanilla ships no pre-baked `particles.png` — 26.2 has 289 loose PNGs
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
    let root = asset_root()?;
    let manager = open_client_jar(&root)?;
    let (atlas, report) = match ParticleAtlas::build_reported(&manager) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(target: "assets", "build particle atlas from {}: {e}", root.display());
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
    let root = asset_root()?;
    let manager = open_client_jar(&root)?;
    let (atlas, report) = match ItemAtlas::build_reported(&manager) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(target: "assets", "build item atlas from {}: {e}", root.display());
            return None;
        }
    };
    tracing::info!(
        target: "assets",
        items = report.items,
        drawable = report.drawable,
        sprites = report.sprites,
        missing_textures = report.missing_textures.len(),
        parked_special = report.missing_special_bases.len(),
        "loaded vanilla item-sprite atlas for the HUD"
    );
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

    let root = asset_root()?;
    let manager = open_client_jar(&root)?;

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
            if let Some(text) = manager
                .read(&path)
                .and_then(|bytes| String::from_utf8(bytes).ok())
            {
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

/// Gate helper: open the vanilla `client.jar` as a [`ResourceManager`],
/// version-free, using the same discovery as the atlas loaders. Returns `None`
/// when no pack is found so gates can fail *closed and loud* rather than
/// silently skipping. Lets a GPU gate build a [`GuiAtlas`] and read the raw
/// sprite PNGs from one manager, to compare rendered pixels against source art.
#[cfg(test)]
pub(crate) fn vanilla_manager() -> Option<ResourceManager> {
    let root = asset_root()?;
    let jar = root.join("client.jar");
    let bytes = std::fs::read(&jar).ok()?;
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
