//! The stitched particle-sprite atlas (`assets/<ns>/textures/particle/**`).
//!
//! Vanilla 26.2 ships one loose PNG per particle frame under
//! `textures/particle/` — there is no pre-baked `particles.png` sheet on disk,
//! same as blocks and items. The client stitches those loose sprites into an
//! atlas at load time; this module does the same, reusing [`AtlasBuilder`]
//! rather than growing a second stitcher.
//!
//! [`ParticleDefinition`] (see [`crate::particle`]) names which sprite
//! textures a given particle type animates through — `assets/<ns>/particles/
//! *.json`, sitting alongside `blockstates/` and `models/` in the pack. This
//! module discovers every such definition in the pack stack (mirroring how
//! [`crate::item_atlas::ItemAtlas`] discovers `items/*.json`), unions the
//! sprites they reference, and stitches that union into one [`Atlas`].
//!
//! # Naming sprites in the atlas
//!
//! A particle definition's `textures` entries are bare names (`"minecraft:
//! flame"`, `"minecraft:big_smoke_0"`) — the `particle/` directory segment is
//! implicit, matching [`ParticleDefinition::texture_paths`]. The atlas keys
//! each stitched sprite by the *directory-qualified* location
//! (`minecraft:particle/flame`) instead, because that is what actually
//! resolves through [`AtlasBuilder::load`] (`textures/<path>.png`) and it is
//! also the form `lodestone_particle::Sheet::texture_name` produces — the
//! shell can go straight from a live particle's `Sheet`/frame to an atlas
//! lookup with no extra string surgery.

use std::collections::BTreeSet;

use crate::atlas::{Atlas, AtlasBuilder, AtlasSprite};
use crate::error::{AtlasError, ParticleAtlasError};
use crate::location::ResourceLocation;
use crate::manager::ResourceManager;
use crate::particle::ParticleDefinition;
use crate::texture::Image;

/// A census of what a [`ParticleAtlas::build_reported`] run produced.
#[derive(Debug, Clone, Default)]
pub struct ParticleAtlasReport {
    /// Particle definitions discovered under `assets/<ns>/particles/`.
    pub definitions: usize,
    /// Distinct sprites successfully stitched into the atlas.
    pub sprites: usize,
    /// Sprite textures referenced by a definition but absent/undecodable
    /// (named `namespace:particle/path`).
    pub missing_textures: Vec<String>,
    /// Definition files that failed to parse (named `path: reason`).
    pub parse_errors: Vec<String>,
}

/// The stitched vanilla particle-sprite atlas.
#[derive(Debug)]
pub struct ParticleAtlas {
    atlas: Atlas,
}

impl ParticleAtlas {
    /// Builds the atlas over `manager`, discarding the report.
    ///
    /// # Errors
    ///
    /// Returns [`ParticleAtlasError`] only if the underlying atlas cannot be
    /// built at all (see [`Self::build_reported`]). Missing individual
    /// textures or unparsable definitions are recorded, not fatal.
    pub fn build(manager: &ResourceManager) -> Result<Self, ParticleAtlasError> {
        Ok(Self::build_reported(manager)?.0)
    }

    /// Builds the atlas and returns a coverage [`ParticleAtlasReport`]
    /// alongside it.
    ///
    /// # Errors
    ///
    /// See [`Self::build`].
    pub fn build_reported(
        manager: &ResourceManager,
    ) -> Result<(Self, ParticleAtlasReport), ParticleAtlasError> {
        let mut report = ParticleAtlasReport::default();
        let mut sprite_locs: BTreeSet<ResourceLocation> = BTreeSet::new();

        for path in particle_definition_paths(manager) {
            let Some(bytes) = manager.read(&path) else {
                continue;
            };
            match ParticleDefinition::parse(&bytes) {
                Ok(def) => {
                    report.definitions += 1;
                    for t in &def.textures {
                        // `texture_paths` prepends `textures/particle/` to the
                        // raw asset *path*; do the equivalent to the
                        // `ResourceLocation` itself so `AtlasBuilder::load`
                        // (which appends `textures/<path>.png`) resolves the
                        // same file.
                        if let Ok(loc) =
                            ResourceLocation::new(t.namespace(), format!("particle/{}", t.path()))
                        {
                            sprite_locs.insert(loc);
                        }
                    }
                }
                Err(e) => report.parse_errors.push(format!("{path}: {e}")),
            }
        }

        let mut builder = AtlasBuilder::new();
        let mut loaded = 0usize;
        for loc in &sprite_locs {
            match builder.load(manager, loc) {
                Ok(_) => loaded += 1,
                Err(AtlasError::TextureMissing { location }) => {
                    report.missing_textures.push(location);
                }
                Err(AtlasError::Texture { location, source }) => {
                    report.missing_textures.push(format!("{location}: {source}"));
                }
                Err(other) => return Err(ParticleAtlasError::Atlas(other)),
            }
        }

        // A pack with no particle definitions (or none whose sprites decode)
        // still needs a valid atlas to build; seed a 1x1 transparent sprite,
        // matching `ItemAtlas`'s fallback for the same situation.
        if loaded == 0 {
            builder.add_texture(
                ResourceLocation::new("minecraft", "particle/empty")
                    .expect("valid literal location"),
                Image {
                    width: 1,
                    height: 1,
                    rgba: vec![0, 0, 0, 0],
                },
                None,
            );
        }

        let atlas = builder.build()?;
        report.sprites = loaded;
        Ok((Self { atlas }, report))
    }

    /// The stitched CPU atlas (upload once to the GPU).
    #[must_use]
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// The stitched sprite for a directory-qualified texture location (e.g.
    /// `minecraft:particle/flame`, `minecraft:particle/generic_3`).
    #[must_use]
    pub fn sprite(&self, location: &ResourceLocation) -> Option<&AtlasSprite> {
        self.atlas.sprite(location)
    }
}

/// Discovers particle definition paths by scanning for
/// `assets/<ns>/particles/<path>.json`. Sorted and deduplicated so a given
/// pack stack yields a byte-identical atlas, mirroring `item_atlas::item_ids`.
fn particle_definition_paths(manager: &ResourceManager) -> Vec<String> {
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for path in manager.list("assets/") {
        let Some(rest) = path.strip_prefix("assets/") else {
            continue;
        };
        let Some((_namespace, tail)) = rest.split_once('/') else {
            continue;
        };
        if tail.starts_with("particles/") && tail.ends_with(".json") {
            paths.insert(path);
        }
    }
    paths.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;

    fn loc(s: &str) -> ResourceLocation {
        ResourceLocation::parse(s).unwrap()
    }

    /// A solid `w`x`h` RGBA PNG of one colour.
    fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for px in buf.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&buf).unwrap();
        }
        out
    }

    /// A pack with two definitions: a single-frame `flame` and a 4-frame
    /// `splash`, mirroring vanilla's real shape (single texture vs. a
    /// numbered strip) without needing the jar.
    fn manager() -> ResourceManager {
        let mut src = MemorySource::new("test");
        let mut ins = |path: &str, body: Vec<u8>| src.insert(path.to_string(), body);

        ins(
            "assets/minecraft/particles/flame.json",
            br#"{"textures":["minecraft:flame"]}"#.to_vec(),
        );
        ins(
            "assets/minecraft/textures/particle/flame.png",
            png(8, 8, [255, 128, 0, 255]),
        );

        ins(
            "assets/minecraft/particles/splash.json",
            br#"{"textures":["minecraft:splash_0","minecraft:splash_1","minecraft:splash_2","minecraft:splash_3"]}"#
                .to_vec(),
        );
        for i in 0..4u8 {
            ins(
                &format!("assets/minecraft/textures/particle/splash_{i}.png"),
                png(6, 6, [0, 128, 255, 255]),
            );
        }

        ResourceManager::new(vec![Box::new(src)])
    }

    #[test]
    fn stitches_referenced_sprites_with_uvs() {
        let mgr = manager();
        let (atlas, report) = ParticleAtlas::build_reported(&mgr).expect("build");

        assert_eq!(report.definitions, 2);
        assert_eq!(report.sprites, 5, "1 flame frame + 4 splash frames");
        assert!(report.missing_textures.is_empty());
        assert!(report.parse_errors.is_empty());

        let flame = atlas
            .sprite(&loc("minecraft:particle/flame"))
            .expect("flame sprite in atlas");
        assert!(flame.uv_max[0] > flame.uv_min[0]);
        assert!(flame.uv_max[1] > flame.uv_min[1]);

        for i in 0..4 {
            assert!(
                atlas
                    .sprite(&loc(&format!("minecraft:particle/splash_{i}")))
                    .is_some(),
                "splash_{i} must be stitched"
            );
        }
    }

    /// A missing sprite is recorded, not fatal — the rest of the atlas still
    /// builds, matching `ItemAtlas`'s discipline.
    #[test]
    fn missing_texture_is_reported_not_fatal() {
        let mut src = MemorySource::new("test");
        src.insert(
            "assets/minecraft/particles/ghost.json".to_string(),
            br#"{"textures":["minecraft:ghost_sprite"]}"#.to_vec(),
        );
        let mgr = ResourceManager::new(vec![Box::new(src)]);

        let (_atlas, report) = ParticleAtlas::build_reported(&mgr).expect("build");
        assert_eq!(report.definitions, 1);
        assert_eq!(report.sprites, 0);
        assert!(
            report
                .missing_textures
                .iter()
                .any(|m| m.contains("ghost_sprite")),
            "missing texture should be named, got {:?}",
            report.missing_textures
        );
    }

    /// A pack with no particle definitions still yields a valid (fallback)
    /// atlas rather than erroring, so a headless caller with an empty pack
    /// doesn't have to special-case construction.
    #[test]
    fn empty_pack_still_builds_a_valid_atlas() {
        let mgr = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        let atlas = ParticleAtlas::build(&mgr).expect("build over empty pack");
        assert!(!atlas.atlas().sprites().is_empty());
    }
}
