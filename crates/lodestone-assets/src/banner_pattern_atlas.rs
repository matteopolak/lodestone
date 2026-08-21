//! The real banner-pattern mask atlas —
//! `assets/minecraft/textures/entity/banner/*.png`, discovered the way
//! vanilla itself discovers them, not by a hand-transcribed filename list.
//!
//! # Why "discovered", not "hand-listed"
//!
//! `docs/banner-shield-patterns.md` settled, by listing `client.jar`
//! directly, that these are individual loose PNGs (one per pattern, plus
//! `base.png` and the plain-cloth `banner_base.png`) and that
//! `assets/minecraft/atlases/banner_patterns.json` is vanilla's own
//! *directory-source* atlas descriptor for them —
//! `{"sources": [{"type": "minecraft:directory", "prefix": "entity/banner/",
//! "source": "entity/banner"}]}` — telling the runtime stitcher "every `.png`
//! under `textures/entity/banner/` is a sprite here", not a format to
//! transcribe by hand.
//!
//! [`crate::atlas_source::AtlasDefinition`] already parses exactly that
//! descriptor shape and already resolves a `directory` source by scanning a
//! [`ResourceManager`]'s real path list (`AtlasSource::resolve`,
//! `crates/lodestone-assets/src/atlas_source.rs`) — the same mechanism the
//! panorama fix (`d365ae5`) established the general rule for: get the real
//! list from the thing that actually enumerates it, never by guessing
//! filenames or hand-transcribing a registry (the `DyeColor::LIME` one-hex
//! typo this crate's own history has is exactly the failure mode a
//! hand-transcribed list invites). This module is a thin, typed wrapper
//! around that existing resolver plus [`Image::decode_png`] — no new
//! discovery logic, no atlas *stitching*.
//!
//! # Why not [`crate::atlas::AtlasBuilder`]
//!
//! `AtlasBuilder`/`Atlas` (used by [`crate::particle_atlas`],
//! [`crate::item_atlas`]) stitch many loose sprites into one packed GPU
//! sheet. `docs/banner-shield-patterns.md` rules that out **for this
//! consumer**: a banner's pattern layers draw translucent, depth-write-off,
//! in item-stored order, so they cannot ride a shared-texture instanced
//! batch the way opaque geometry can — the real fix there is a small ordered
//! per-layer draw list, each its own draw call, not a shared packed sheet.
//! This module produces exactly what that shape needs: one decoded
//! [`Image`] per pattern id, addressable individually — mirroring
//! [`crate::block_entity`]'s (in `lodestone-render`)
//! `chest_texture_stems`/`skull_texture_stems` "stem-list-plus-loader" shape,
//! not a stitcher.
//!
//! # Keying
//!
//! Sprites are keyed by their **bare pattern asset id** (`"base"`,
//! `"creeper"`, …) — the same un-namespaced string
//! `lodestone_render::banner_pattern::StoredPatternLayer::pattern_asset_id`
//! and `PatternLayer::sprite`'s path (minus the `entity/banner/` prefix)
//! carry, so a caller holding either can look itself up here with no string
//! surgery beyond what [`BannerPatternAtlas::get_sprite`] already does.
//! `banner_base` (the plain wood/cloth texture `submitBanner` draws under
//! the *opaque* body/flag pass, `Sheets.BANNER_BASE` — not a pattern mask at
//! all, see the module doc on `crates/lodestone-render/src/block_entity.rs`)
//! is deliberately excluded: including it under a real-sounding key would
//! let a caller bind the wrong texture for a pattern layer and have it look
//! plausible (a solid near-white texture) until compared against vanilla.

use std::collections::HashMap;

use crate::atlas_source::AtlasDefinition;
use crate::error::BannerPatternAtlasError;
use crate::location::ResourceLocation;
use crate::manager::ResourceManager;
use crate::texture::Image;

/// In-pack path of vanilla's own banner-pattern atlas descriptor.
pub const BANNER_PATTERNS_ATLAS_PATH: &str = "assets/minecraft/atlases/banner_patterns.json";

/// The plain cloth/wood sheet `submitBanner`'s opaque body/flag pass uses —
/// present in the same directory as every pattern mask but not itself a
/// pattern, so [`BannerPatternAtlas::load_reported`] excludes it.
const NON_PATTERN_STEM: &str = "banner_base";

/// A census of what [`BannerPatternAtlas::load_reported`] produced —
/// mirrors [`crate::particle_atlas::ParticleAtlasReport`]'s shape.
#[derive(Debug, Clone, Default)]
pub struct BannerPatternAtlasReport {
    /// Sprites the directory source named that decoded successfully.
    pub loaded: usize,
    /// Sprites the directory source named whose bytes were not found in any
    /// pack (named by pattern id).
    pub missing_textures: Vec<String>,
    /// Sprites whose bytes were found but failed to decode as a PNG (named
    /// `pattern id: reason`).
    pub decode_errors: Vec<String>,
}

/// Every real banner-pattern mask, decoded — the always-present `base` mask
/// plus every named pattern (`creeper`, `cross`, …), keyed by bare pattern
/// asset id. See the module doc for why this is a flat map of individually
/// addressable images, not a stitched sheet.
#[derive(Debug, Default)]
pub struct BannerPatternAtlas {
    sprites: HashMap<String, Image>,
}

impl BannerPatternAtlas {
    /// Loads every real pattern mask, discarding the report.
    ///
    /// # Errors
    ///
    /// Returns [`BannerPatternAtlasError`] only if
    /// `atlases/banner_patterns.json` itself is missing or unparsable — an
    /// individual missing or undecodable sprite is recorded in the report,
    /// not fatal (see [`Self::load_reported`]).
    pub fn load(manager: &ResourceManager) -> Result<Self, BannerPatternAtlasError> {
        Ok(Self::load_reported(manager)?.0)
    }

    /// Loads every real pattern mask and returns a coverage report alongside
    /// it.
    ///
    /// # Errors
    ///
    /// See [`Self::load`].
    pub fn load_reported(
        manager: &ResourceManager,
    ) -> Result<(Self, BannerPatternAtlasReport), BannerPatternAtlasError> {
        let (sprites, report) = load_pattern_directory(
            manager,
            BANNER_PATTERNS_ATLAS_PATH,
            "entity/banner/",
            &[NON_PATTERN_STEM],
        )?;
        Ok((Self { sprites }, report))
    }

    /// Looks up a decoded mask by its bare pattern asset id (e.g.
    /// `"creeper"`, `"base"`).
    #[must_use]
    pub fn get(&self, pattern_id: &str) -> Option<&Image> {
        self.sprites.get(pattern_id)
    }

    /// Looks up a decoded mask by a resolver's full sprite location (e.g.
    /// [`lodestone_render`]'s `PatternLayer::sprite`,
    /// `minecraft:entity/banner/creeper`) — the convenience
    /// [`Self::get`] cannot offer without the caller stripping the prefix
    /// itself. Returns `None` for a shield sprite (`entity/shield/…`) or any
    /// location outside this atlas's own namespace, exactly as a missing key
    /// would.
    #[must_use]
    pub fn get_sprite(&self, sprite: &ResourceLocation) -> Option<&Image> {
        sprite
            .path()
            .strip_prefix("entity/banner/")
            .and_then(|id| self.get(id))
    }

    /// The always-present base-colour mask (`"base"`) — every banner and
    /// shield draws this as layer 0, per
    /// `docs/banner-shield-patterns.md`'s "the base layer is not optional"
    /// rule.
    #[must_use]
    pub fn base(&self) -> Option<&Image> {
        self.get("base")
    }

    /// Number of decoded masks (the real vanilla count is 43 as of 26.2: the
    /// base mask plus 42 named patterns — measured directly against
    /// `client.jar`, see `docs/banner-shield-patterns.md`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.sprites.len()
    }

    /// Whether no masks decoded at all — the pack has no `client.jar`-shaped
    /// texture tree.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sprites.is_empty()
    }

    /// Every decoded pattern id, in no particular order.
    pub fn pattern_ids(&self) -> impl Iterator<Item = &str> {
        self.sprites.keys().map(String::as_str)
    }
}

/// Shared `minecraft:directory`-atlas loader for [`BannerPatternAtlas`] and
/// [`ShieldPatternAtlas`] — the two are vanilla's identical directory-source
/// atlas shape (see the module doc's "Why 'discovered', not 'hand-listed'")
/// over a different `entity/<family>/` tree, differing only in which
/// non-pattern stems living in the same directory (the plain cloth sheet for
/// a banner, the two base/no-pattern sheets for a shield) to exclude from
/// the pattern set.
fn load_pattern_directory(
    manager: &ResourceManager,
    atlas_path: &str,
    prefix: &str,
    exclude: &[&str],
) -> Result<(HashMap<String, Image>, BannerPatternAtlasReport), BannerPatternAtlasError> {
    // Stacked, not single-winner: a server pack shipping its own
    // `banner_patterns.json`/`shield_patterns.json` must extend the jar's
    // `directory` source, not replace it outright
    // (`AtlasDefinition::load_stacked`'s own doc).
    let definition = AtlasDefinition::load_stacked(manager, atlas_path).ok_or_else(|| {
        BannerPatternAtlasError::DescriptorMissing {
            path: atlas_path.to_string(),
        }
    })?;

    let mut sprites = HashMap::new();
    let mut report = BannerPatternAtlasReport::default();
    for entry in definition.resolve(manager) {
        // `entry.sprite` is e.g. `minecraft:entity/banner/creeper` (or
        // `entity/shield/creeper`); the directory source's own `prefix` is
        // what guarantees this strip succeeds for every entry it produces.
        let Some(id) = entry.sprite.path().strip_prefix(prefix) else {
            continue;
        };
        if exclude.contains(&id) {
            continue;
        }
        match manager.read(&entry.texture_path) {
            Some(png) => match Image::decode_png(&png) {
                Ok(image) => {
                    sprites.insert(id.to_string(), image);
                    report.loaded += 1;
                }
                Err(e) => report.decode_errors.push(format!("{id}: {e}")),
            },
            None => report.missing_textures.push(id.to_string()),
        }
    }
    Ok((sprites, report))
}

/// In-pack path of vanilla's own shield-pattern atlas descriptor —
/// `Sheets.SHIELD_PATTERN_BASE`'s directory source, the shield sibling of
/// [`BANNER_PATTERNS_ATLAS_PATH`].
pub const SHIELD_PATTERNS_ATLAS_PATH: &str = "assets/minecraft/atlases/shield_patterns.json";

/// The two non-pattern sheets living in `entity/shield/` alongside every
/// pattern mask — [`crate::block_entity_models::shield_model`]'s own two
/// base sheets, `Sheets.SHIELD_BASE`/`Sheets.SHIELD_BASE_NO_PATTERN` — so
/// [`ShieldPatternAtlas::load_reported`] excludes them the same way
/// [`NON_PATTERN_STEM`] excludes a banner's plain cloth.
const SHIELD_NON_PATTERN_STEMS: [&str; 2] = ["shield_base", "shield_base_nopattern"];

/// Every real shield-pattern mask, decoded — the sibling of
/// [`BannerPatternAtlas`] for `entity/shield/*.png` rather than
/// `entity/banner/*.png`. **A separate, differently-drawn texture set**, not
/// a re-keying of the same images: vanilla ships one PNG per pattern under
/// each of the two directories, cropped and laid out for that family's own
/// mesh, so a caller must not substitute one atlas for the other even though
/// the pattern ids (`"creeper"`, `"base"`, …) are identical strings in both.
#[derive(Debug, Default)]
pub struct ShieldPatternAtlas {
    sprites: HashMap<String, Image>,
}

impl ShieldPatternAtlas {
    /// Loads every real shield-pattern mask, discarding the report.
    ///
    /// # Errors
    ///
    /// Returns [`BannerPatternAtlasError`] only if
    /// `atlases/shield_patterns.json` itself is missing or unparsable — see
    /// [`BannerPatternAtlas::load`]'s identical contract.
    pub fn load(manager: &ResourceManager) -> Result<Self, BannerPatternAtlasError> {
        Ok(Self::load_reported(manager)?.0)
    }

    /// Loads every real shield-pattern mask and returns a coverage report
    /// alongside it.
    ///
    /// # Errors
    ///
    /// See [`Self::load`].
    pub fn load_reported(
        manager: &ResourceManager,
    ) -> Result<(Self, BannerPatternAtlasReport), BannerPatternAtlasError> {
        let (sprites, report) = load_pattern_directory(
            manager,
            SHIELD_PATTERNS_ATLAS_PATH,
            "entity/shield/",
            &SHIELD_NON_PATTERN_STEMS,
        )?;
        Ok((Self { sprites }, report))
    }

    /// Looks up a decoded mask by its bare pattern asset id (e.g.
    /// `"creeper"`, `"base"`).
    #[must_use]
    pub fn get(&self, pattern_id: &str) -> Option<&Image> {
        self.sprites.get(pattern_id)
    }

    /// Looks up a decoded mask by a resolver's full sprite location (e.g.
    /// [`lodestone_render`]'s `PatternLayer::sprite`,
    /// `minecraft:entity/shield/creeper`). Returns `None` for a banner
    /// sprite (`entity/banner/…`) or any location outside this atlas's own
    /// namespace, exactly as a missing key would.
    #[must_use]
    pub fn get_sprite(&self, sprite: &ResourceLocation) -> Option<&Image> {
        sprite
            .path()
            .strip_prefix("entity/shield/")
            .and_then(|id| self.get(id))
    }

    /// Number of decoded masks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sprites.len()
    }

    /// Whether no masks decoded at all — the pack has no `client.jar`-shaped
    /// texture tree.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sprites.is_empty()
    }

    /// Every decoded pattern id, in no particular order.
    pub fn pattern_ids(&self) -> impl Iterator<Item = &str> {
        self.sprites.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;

    /// A minimal valid 1×1 RGBA PNG, so tests exercise the real
    /// [`Image::decode_png`] path rather than a fixture that skips it.
    fn tiny_png() -> Vec<u8> {
        // Generated once via the `png` crate at 1x1 RGBA8, opaque white.
        // Re-decoded by every test that uses it, so a corrupt literal would
        // fail loudly rather than silently passing.
        let mut buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buf, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("write PNG header");
            writer.write_image_data(&[255, 255, 255, 255]).expect("write PNG data");
        }
        buf
    }

    fn manager_with(entries: &[&str]) -> ResourceManager {
        let mut source = MemorySource::new("test-pack");
        source.insert(
            BANNER_PATTERNS_ATLAS_PATH,
            br#"{"sources":[{"type":"minecraft:directory","source":"entity/banner","prefix":"entity/banner/"}]}"#
                .to_vec(),
        );
        for name in entries {
            source.insert(
                format!("assets/minecraft/textures/entity/banner/{name}.png"),
                tiny_png(),
            );
        }
        ResourceManager::new(vec![Box::new(source)])
    }

    /// Discovery goes through the real directory-source resolver, not a
    /// hand-listed set: adding a texture to the fixture with no code change
    /// here is enough for it to appear.
    #[test]
    fn discovers_every_png_under_the_directory_without_a_hand_list() {
        let manager = manager_with(&["base", "creeper", "cross"]);
        let atlas = BannerPatternAtlas::load(&manager).expect("descriptor present");
        assert_eq!(atlas.len(), 3);
        assert!(atlas.get("creeper").is_some());
        assert!(atlas.get("cross").is_some());
        assert!(atlas.base().is_some());
    }

    /// `banner_base.png` (the plain cloth texture) sits in the same
    /// directory as every pattern mask but is not itself a pattern — a
    /// consumer that bound it for a pattern *layer* would draw a plausible
    /// near-white rectangle instead of the requested mask shape.
    #[test]
    fn the_plain_cloth_texture_is_not_a_pattern() {
        let manager = manager_with(&["base", "banner_base", "creeper"]);
        let atlas = BannerPatternAtlas::load(&manager).expect("descriptor present");
        assert_eq!(atlas.len(), 2, "banner_base must not be counted as a pattern");
        assert!(atlas.get("banner_base").is_none());
    }

    /// A caller holding a resolver's full sprite location (as
    /// `banner_pattern_layers` actually returns them) can look itself up
    /// with no manual prefix surgery.
    #[test]
    fn get_sprite_strips_the_banner_prefix_a_resolver_actually_produces() {
        let manager = manager_with(&["base", "creeper"]);
        let atlas = BannerPatternAtlas::load(&manager).expect("descriptor present");
        let sprite = ResourceLocation::parse("minecraft:entity/banner/creeper").unwrap();
        assert!(atlas.get_sprite(&sprite).is_some());
        assert_eq!(
            atlas.get_sprite(&sprite).map(|i| (i.width, i.height)),
            atlas.get("creeper").map(|i| (i.width, i.height)),
        );
    }

    /// A shield sprite (different namespace prefix) is not a banner sprite,
    /// even if the bare id happens to coincide — `get_sprite` must not
    /// silently cross the banner/shield split
    /// `docs/banner-shield-patterns.md` documents.
    #[test]
    fn get_sprite_rejects_a_shield_location() {
        let manager = manager_with(&["creeper"]);
        let atlas = BannerPatternAtlas::load(&manager).expect("descriptor present");
        let shield_sprite = ResourceLocation::parse("minecraft:entity/shield/creeper").unwrap();
        assert!(atlas.get_sprite(&shield_sprite).is_none());
    }

    /// A missing descriptor is a hard error (there is no source list to
    /// resolve at all), distinct from an individual missing sprite, which
    /// [`BannerPatternAtlasReport`] records instead of failing the whole
    /// load.
    #[test]
    fn a_missing_descriptor_is_an_error_not_an_empty_atlas() {
        let manager = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        let err = BannerPatternAtlas::load(&manager).unwrap_err();
        assert!(matches!(err, BannerPatternAtlasError::DescriptorMissing { .. }));
    }

    /// A source whose `list` names a path its own `read` refuses — the only
    /// way to exercise "the directory source named a sprite but its bytes
    /// are absent" deterministically, since a real [`MemorySource`]'s list
    /// and read are always consistent with each other.
    #[derive(Debug)]
    struct ListsMoreThanItHas {
        descriptor: Vec<u8>,
        base_png: Vec<u8>,
    }

    impl crate::source::ResourceSource for ListsMoreThanItHas {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            match path {
                BANNER_PATTERNS_ATLAS_PATH => Some(self.descriptor.clone()),
                "assets/minecraft/textures/entity/banner/base.png" => Some(self.base_png.clone()),
                _ => None,
            }
        }

        fn list(&self, prefix: &str) -> Vec<String> {
            [
                "assets/minecraft/textures/entity/banner/base.png",
                // Named here, but `read` above has no bytes for it.
                "assets/minecraft/textures/entity/banner/creeper.png",
            ]
            .into_iter()
            .filter(|p| p.starts_with(prefix))
            .map(str::to_string)
            .collect()
        }
    }

    /// A sprite the directory source names but whose bytes are absent from
    /// every pack is reported, not fatal — the same "resource packs are
    /// untrusted" contract [`Image::decode_png`] itself documents.
    #[test]
    fn a_named_sprite_with_no_bytes_is_reported_not_fatal() {
        let manager = ResourceManager::new(vec![Box::new(ListsMoreThanItHas {
            descriptor: br#"{"sources":[{"type":"minecraft:directory","source":"entity/banner","prefix":"entity/banner/"}]}"#
                .to_vec(),
            base_png: tiny_png(),
        })]);
        let (atlas, report) = BannerPatternAtlas::load_reported(&manager).expect("descriptor present");
        assert_eq!(atlas.len(), 1, "only base.png actually had bytes");
        assert!(atlas.get("base").is_some());
        assert_eq!(report.loaded, 1);
        assert_eq!(report.missing_textures, vec!["creeper".to_string()]);
        assert!(report.decode_errors.is_empty());
    }
}
