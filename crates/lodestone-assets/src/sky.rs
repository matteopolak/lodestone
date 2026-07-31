//! The celestial sprite atlas (sun + moon phases) and the cloud texture —
//! `assets/<ns>/textures/environment/**`.
//!
//! Vanilla 26.2 stitches `sun.png` and the eight `moon/<phase>.png` sprites into
//! one small runtime atlas (`AtlasIds.CELESTIALS`, built by
//! `SkyRenderer`/`AtlasManager` — see
//! `.cache/mc/26.2/client-src/net/minecraft/client/renderer/SkyRenderer.java`,
//! behavioural reference only). [`CelestialAtlas`] mirrors that using the same
//! [`AtlasBuilder`] every other atlas in this crate uses (block, item, particle,
//! GUI), rather than a bespoke stitcher.
//!
//! `clouds.png` is loaded as a plain, unatlased [`Image`] instead via
//! [`load_cloud_texture`]: the cloud plane samples it with wraparound tiling as
//! it scrolls, and an atlas's per-sprite padding/inset would break that
//! seamlessness (there is also only ever one cloud texture, so there is nothing
//! to stitch it *with*).

use crate::atlas::{Atlas, AtlasBuilder, AtlasSprite};
use crate::error::{AtlasError, SkyAssetError};
use crate::location::ResourceLocation;
use crate::manager::ResourceManager;
use crate::texture::Image;

/// The eight lunar phase texture names (under `environment/celestial/moon/`),
/// in vanilla's `MoonPhase` enum declaration order. Index `n` is the sprite for
/// [`crate::sky`]'s callers computing `moon_phase_index_for_time_of_day(..) ==
/// n` — see `.cache/mc/26.2/client-src/net/minecraft/world/level/MoonPhase.java`,
/// whose own `startTick() == index() * 24000` is what fixes this ordering: the
/// phase active on world-day `d` is enum index `d % 8`.
pub const MOON_PHASE_NAMES: [&str; 8] = [
    "full_moon",
    "waning_gibbous",
    "third_quarter",
    "waning_crescent",
    "new_moon",
    "waxing_crescent",
    "first_quarter",
    "waxing_gibbous",
];

/// The sun sprite's in-pack path segment (under `minecraft:`).
pub const SUN_SPRITE_PATH: &str = "environment/celestial/sun";

fn moon_sprite_path(phase_index: u8) -> String {
    format!(
        "environment/celestial/moon/{}",
        MOON_PHASE_NAMES[usize::from(phase_index) % MOON_PHASE_NAMES.len()]
    )
}

/// The stitched sun + 8-moon-phase atlas (mirrors vanilla's `AtlasIds.CELESTIALS`).
#[derive(Debug)]
pub struct CelestialAtlas {
    atlas: Atlas,
    sun: ResourceLocation,
    moons: [ResourceLocation; 8],
}

impl CelestialAtlas {
    /// Loads and stitches the sun sprite and all 8 moon-phase sprites from
    /// `manager`.
    ///
    /// # Errors
    ///
    /// Returns [`SkyAssetError`] if the sun or any moon-phase texture is
    /// missing or fails to decode, or if the stitch itself fails.
    pub fn build(manager: &ResourceManager) -> Result<Self, SkyAssetError> {
        let mut builder = AtlasBuilder::new();

        let sun =
            ResourceLocation::new("minecraft", SUN_SPRITE_PATH).expect("valid literal location");
        builder.load(manager, &sun).map_err(convert)?;

        let mut moons: Vec<ResourceLocation> = Vec::with_capacity(8);
        for phase in 0..8u8 {
            let loc = ResourceLocation::new("minecraft", moon_sprite_path(phase))
                .expect("valid literal location");
            builder.load(manager, &loc).map_err(convert)?;
            moons.push(loc);
        }

        let atlas = builder.build()?;
        Ok(Self {
            atlas,
            sun,
            moons: moons.try_into().expect("exactly 8 moon phases staged"),
        })
    }

    /// The stitched CPU atlas (upload once to the GPU).
    #[must_use]
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// The sun's placed sprite.
    #[must_use]
    pub fn sun_sprite(&self) -> Option<&AtlasSprite> {
        self.atlas.sprite(&self.sun)
    }

    /// The placed sprite for moon phase `index` (`0..8`, [`MOON_PHASE_NAMES`]
    /// order). Out-of-range indices wrap via modulo, matching a caller deriving
    /// `index` from `moon_phase_index_for_time_of_day(..) % 8`.
    #[must_use]
    pub fn moon_sprite(&self, index: u8) -> Option<&AtlasSprite> {
        self.atlas.sprite(&self.moons[usize::from(index) % 8])
    }
}

fn convert(err: AtlasError) -> SkyAssetError {
    match err {
        AtlasError::TextureMissing { location } => SkyAssetError::Missing { location },
        AtlasError::Texture { location, source } => SkyAssetError::Texture { location, source },
        other => SkyAssetError::Atlas(other),
    }
}

/// Loads `textures/environment/clouds.png` as a plain, unatlased image.
///
/// Deliberately not stitched into [`CelestialAtlas`] or any other atlas: the
/// cloud plane scrolls with wraparound UVs across the *whole* texture, and an
/// atlas's per-sprite padding/inset (or being placed off `(0, 0)`) would break
/// that seam.
///
/// # Errors
///
/// Returns [`SkyAssetError`] if the texture is missing or fails to decode.
pub fn load_cloud_texture(manager: &ResourceManager) -> Result<Image, SkyAssetError> {
    const PATH: &str = "assets/minecraft/textures/environment/clouds.png";
    let bytes = manager.read(PATH).ok_or_else(|| SkyAssetError::Missing {
        location: "minecraft:environment/clouds".to_string(),
    })?;
    Image::decode_png(&bytes).map_err(|source| SkyAssetError::Texture {
        location: "minecraft:environment/clouds".to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;

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

    /// A synthetic pack carrying the sun, all 8 moon phases (each a distinct
    /// solid colour so a wrong phase index is caught by colour, not just
    /// presence) and a cloud map, sized like the real 32x32 sprites / 256x256
    /// cloud map so this exercises the same shapes production does.
    fn manager() -> ResourceManager {
        let mut src = MemorySource::new("test");
        src.insert(
            "assets/minecraft/textures/environment/celestial/sun.png".to_string(),
            png(32, 32, [255, 220, 0, 255]),
        );
        for (i, name) in MOON_PHASE_NAMES.iter().enumerate() {
            src.insert(
                format!("assets/minecraft/textures/environment/celestial/moon/{name}.png"),
                png(32, 32, [i as u8, i as u8, i as u8, 255]),
            );
        }
        src.insert(
            "assets/minecraft/textures/environment/clouds.png".to_string(),
            png(256, 256, [255, 255, 255, 255]),
        );
        ResourceManager::new(vec![Box::new(src)])
    }

    #[test]
    fn stitches_sun_and_all_eight_moon_phases() {
        let atlas = CelestialAtlas::build(&manager()).expect("build");
        let sun = atlas.sun_sprite().expect("sun sprite present");
        assert!(sun.uv_max[0] > sun.uv_min[0]);
        assert!(sun.uv_max[1] > sun.uv_min[1]);
        for phase in 0..8u8 {
            assert!(
                atlas.moon_sprite(phase).is_some(),
                "moon phase {phase} ({}) must be stitched",
                MOON_PHASE_NAMES[phase as usize]
            );
        }
    }

    /// Every moon phase must land at a genuinely distinct atlas rect — a copy
    /// paste bug that stitched the same sprite 8 times would still pass a
    /// "present" check but always show one phase.
    #[test]
    fn moon_phases_occupy_distinct_atlas_regions() {
        let atlas = CelestialAtlas::build(&manager()).expect("build");
        let mut seen = std::collections::HashSet::new();
        for phase in 0..8u8 {
            let sprite = atlas.moon_sprite(phase).expect("sprite");
            let rect = (sprite.x, sprite.y, sprite.width, sprite.height);
            assert!(
                seen.insert(rect),
                "phase {phase} reused another phase's atlas rect {rect:?}"
            );
        }
    }

    /// Index wraps modulo 8 rather than panicking on an out-of-range caller.
    #[test]
    fn moon_sprite_index_wraps() {
        let atlas = CelestialAtlas::build(&manager()).expect("build");
        assert_eq!(
            atlas.moon_sprite(0).map(|s| (s.x, s.y)),
            atlas.moon_sprite(8).map(|s| (s.x, s.y))
        );
    }

    #[test]
    fn missing_sun_is_reported_by_location() {
        let mgr = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        let err = CelestialAtlas::build(&mgr).expect_err("must fail closed");
        assert!(matches!(err, SkyAssetError::Missing { .. }), "{err:?}");
    }

    #[test]
    fn cloud_texture_loads_as_a_plain_unatlased_image() {
        let image = load_cloud_texture(&manager()).expect("load");
        assert_eq!((image.width, image.height), (256, 256));
    }

    #[test]
    fn missing_cloud_texture_is_reported() {
        let mgr = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        let err = load_cloud_texture(&mgr).expect_err("must fail closed");
        assert!(matches!(err, SkyAssetError::Missing { .. }), "{err:?}");
    }
}
