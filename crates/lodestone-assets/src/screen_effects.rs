//! The two full-screen overlay textures vanilla draws from `ScreenEffectRenderer`
//! (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/ScreenEffectRenderer.java`):
//! the underwater tint texture and the fire-overlay sprite strip.
//!
//! Both are loaded as plain, unatlased [`Image`]s, the same way [`crate::sky::load_cloud_texture`]
//! loads `clouds.png` — each is sampled with its own wraparound/strip addressing
//! that an atlas's per-sprite padding would break, and there is exactly one of
//! each, so there is nothing to stitch either with.

use crate::error::ScreenEffectAssetError;
use crate::manager::ResourceManager;
use crate::texture::Image;

fn load_plain(
    manager: &ResourceManager,
    path: &str,
    location: &str,
) -> Result<Image, ScreenEffectAssetError> {
    let bytes = manager
        .read(path)
        .ok_or_else(|| ScreenEffectAssetError::Missing {
            location: location.to_string(),
        })?;
    Image::decode_png(&bytes).map_err(|source| ScreenEffectAssetError::Texture {
        location: location.to_string(),
        source,
    })
}

/// Loads `textures/misc/underwater.png` (16x16 in vanilla), the texture the
/// underwater overlay tiles 4x4 and scrolls by look direction — see
/// `ScreenEffectRenderer.submitWater`.
///
/// # Errors
///
/// Returns [`ScreenEffectAssetError`] if the texture is missing or fails to
/// decode.
pub fn load_underwater_texture(manager: &ResourceManager) -> Result<Image, ScreenEffectAssetError> {
    load_plain(
        manager,
        "assets/minecraft/textures/misc/underwater.png",
        "minecraft:misc/underwater",
    )
}

/// The fixed frame size (both width and height) of `fire_1.png`'s animation
/// strip. Vanilla's animated textures are always square frames stacked
/// vertically; there is no metadata field carrying this, it is inferred from
/// `width == 16` the same way vanilla's own `SpriteContents` does.
pub const FIRE_FRAME_SIZE: u32 = 16;

/// Loads `textures/block/fire_1.png` (16x512 in vanilla: 32 stacked 16x16
/// frames), the sprite `ScreenEffectRenderer.submitFire` draws — see
/// `ModelBakery.FIRE_1`. This is the *block-atlas* fire sprite the fire block
/// itself renders with, loaded here as its own standalone strip rather than
/// through the block atlas: the overlay pass is deliberately a separate, tiny
/// pipeline kept off the model pipeline's already-full four bind groups (see
/// `crates/lodestone-render/src/screen_effects.rs`), so reaching into
/// `ModelRenderer`'s atlas would mean either a fifth bind group or plumbing a
/// texture view across an unrelated module boundary for one small texture.
///
/// # Errors
///
/// Returns [`ScreenEffectAssetError`] if the texture is missing or fails to
/// decode.
pub fn load_fire_texture(manager: &ResourceManager) -> Result<Image, ScreenEffectAssetError> {
    load_plain(
        manager,
        "assets/minecraft/textures/block/fire_1.png",
        "minecraft:block/fire_1",
    )
}

/// The number of animation frames in a loaded `fire_1.png` strip (its height
/// divided by [`FIRE_FRAME_SIZE`], floored, at least 1 so a malformed/short
/// image still yields a sample-able frame count rather than a division
/// producing zero).
#[must_use]
pub fn fire_frame_count(image: &Image) -> u32 {
    (image.height / FIRE_FRAME_SIZE).max(1)
}

/// Loads `textures/misc/pumpkinblur.png`, the full-screen texture a worn
/// carved pumpkin overlays. Unlike underwater/fire, this is not
/// hardcoded in vanilla's renderer at all: it is the `camera_overlay` field of
/// `carved_pumpkin`'s `minecraft:equippable` data component
/// (`.cache/mc/26.2/generated/reports/minecraft/components/item/carved_pumpkin.json`,
/// `"camera_overlay": "minecraft:misc/pumpkinblur"`), drawn generically for
/// *any* equipped item that declares one by
/// `Hud.extractCameraOverlays`/`extractTextureOverlay` —
/// carved pumpkin is simply the only item that ships with the field set.
/// Loaded here as its own standalone plain texture for the same reason
/// underwater/fire are: this pass is deliberately kept off the model
/// pipeline's already-full four bind groups (see
/// `crates/lodestone-render/src/screen_effects.rs`).
///
/// # Errors
///
/// Returns [`ScreenEffectAssetError`] if the texture is missing or fails to
/// decode.
pub fn load_pumpkin_overlay_texture(manager: &ResourceManager) -> Result<Image, ScreenEffectAssetError> {
    load_plain(
        manager,
        "assets/minecraft/textures/misc/pumpkinblur.png",
        "minecraft:misc/pumpkinblur",
    )
}

/// Loads `textures/misc/powder_snow_outline.png` (256x256 in vanilla), the
/// freezing vignette `Hud.extractCameraOverlays` draws whenever
/// `player.getTicksFrozen() > 0`, at alpha `player.getPercentFrozen()`,
/// via `Hud.extractTextureOverlay`.
///
/// # Errors
///
/// Returns [`ScreenEffectAssetError`] if the texture is missing or fails to
/// decode.
pub fn load_freeze_overlay_texture(manager: &ResourceManager) -> Result<Image, ScreenEffectAssetError> {
    load_plain(
        manager,
        "assets/minecraft/textures/misc/powder_snow_outline.png",
        "minecraft:misc/powder_snow_outline",
    )
}

/// Loads `textures/misc/spyglass_scope.png` (256x256 in vanilla), the lens
/// texture `Hud.extractSpyglassOverlay` blits at the screen centre while
/// scoping. The four black letterbox bars
/// around it are not a texture at all in vanilla (`graphics.fill`, a flat
/// colour) — see `spyglass_letterbox_triangles` in
/// `crates/lodestone-render/src/screen_effects.rs`, which reuses the
/// pipeline's own procedural 1x1 white texture rather than loading a second
/// asset for a solid fill.
///
/// # Errors
///
/// Returns [`ScreenEffectAssetError`] if the texture is missing or fails to
/// decode.
pub fn load_spyglass_scope_texture(manager: &ResourceManager) -> Result<Image, ScreenEffectAssetError> {
    load_plain(
        manager,
        "assets/minecraft/textures/misc/spyglass_scope.png",
        "minecraft:misc/spyglass_scope",
    )
}

/// Loads `textures/misc/nausea.png` (256x256 in vanilla), the confusion
/// overlay `Hud.extractConfusionOverlay` draws while the Nausea effect is
/// active and the screen-effect-scale option is below `1.0`.
///
/// # Errors
///
/// Returns [`ScreenEffectAssetError`] if the texture is missing or fails to
/// decode.
pub fn load_nausea_overlay_texture(manager: &ResourceManager) -> Result<Image, ScreenEffectAssetError> {
    load_plain(
        manager,
        "assets/minecraft/textures/misc/nausea.png",
        "minecraft:misc/nausea",
    )
}

/// Loads `textures/block/nether_portal.png` (16x512 in vanilla: 32 stacked
/// 16x16 frames, `{"animation": {}}` — the exact same animated-strip shape as
/// [`load_fire_texture`]/[`fire_frame_count`], see both docs), the sprite
/// `Hud.extractPortalOverlay` draws while `Entity.portalEffectIntensity > 0`.
///
/// Vanilla reaches this texture through the *block-atlas particle material*
/// for `Blocks.NETHER_PORTAL`
/// (`getModelManager().getBlockStateModelSet().getParticleMaterial(...)`),
/// which is how its atlas-driven animation advances; this port loads the raw
/// strip standalone instead, for the identical reason `load_fire_texture`
/// gives (this pass is deliberately kept off the model pipeline's four
/// bind groups) — [`fire_frame_count`] already generalises over any strip
/// whose height is a multiple of 16, so no second frame-count function is
/// needed here.
///
/// # Errors
///
/// Returns [`ScreenEffectAssetError`] if the texture is missing or fails to
/// decode.
pub fn load_portal_overlay_texture(manager: &ResourceManager) -> Result<Image, ScreenEffectAssetError> {
    load_plain(
        manager,
        "assets/minecraft/textures/block/nether_portal.png",
        "minecraft:block/nether_portal",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;

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

    fn manager() -> ResourceManager {
        let mut src = MemorySource::new("test");
        src.insert(
            "assets/minecraft/textures/misc/underwater.png".to_string(),
            png(16, 16, [40, 90, 180, 255]),
        );
        src.insert(
            "assets/minecraft/textures/block/fire_1.png".to_string(),
            png(16, 512, [230, 130, 20, 255]),
        );
        src.insert(
            "assets/minecraft/textures/misc/pumpkinblur.png".to_string(),
            png(16, 16, [10, 10, 10, 255]),
        );
        src.insert(
            "assets/minecraft/textures/misc/powder_snow_outline.png".to_string(),
            png(256, 256, [200, 230, 255, 255]),
        );
        src.insert(
            "assets/minecraft/textures/misc/spyglass_scope.png".to_string(),
            png(256, 256, [20, 20, 20, 255]),
        );
        src.insert(
            "assets/minecraft/textures/misc/nausea.png".to_string(),
            png(256, 256, [255, 255, 255, 255]),
        );
        src.insert(
            "assets/minecraft/textures/block/nether_portal.png".to_string(),
            png(16, 512, [140, 20, 200, 255]),
        );
        ResourceManager::new(vec![Box::new(src)])
    }

    #[test]
    fn underwater_texture_loads_as_a_plain_16x16_image() {
        let image = load_underwater_texture(&manager()).expect("load");
        assert_eq!((image.width, image.height), (16, 16));
    }

    #[test]
    fn missing_underwater_texture_is_reported() {
        let mgr = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        let err = load_underwater_texture(&mgr).expect_err("must fail closed");
        assert!(matches!(err, ScreenEffectAssetError::Missing { .. }), "{err:?}");
    }

    #[test]
    fn fire_texture_loads_with_32_frames() {
        let image = load_fire_texture(&manager()).expect("load");
        assert_eq!((image.width, image.height), (16, 512));
        assert_eq!(fire_frame_count(&image), 32);
    }

    #[test]
    fn missing_fire_texture_is_reported() {
        let mgr = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        let err = load_fire_texture(&mgr).expect_err("must fail closed");
        assert!(matches!(err, ScreenEffectAssetError::Missing { .. }), "{err:?}");
    }

    #[test]
    fn pumpkin_overlay_texture_loads() {
        let image = load_pumpkin_overlay_texture(&manager()).expect("load");
        assert_eq!((image.width, image.height), (16, 16));
    }

    #[test]
    fn missing_pumpkin_overlay_texture_is_reported() {
        let mgr = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        let err = load_pumpkin_overlay_texture(&mgr).expect_err("must fail closed");
        assert!(matches!(err, ScreenEffectAssetError::Missing { .. }), "{err:?}");
    }

    /// A frame count is never zero even for a malformed (shorter-than-one-frame)
    /// strip, so a caller computing `tick % frame_count` never divides by zero.
    #[test]
    fn fire_frame_count_floors_at_one() {
        let mut src = MemorySource::new("short");
        src.insert(
            "assets/minecraft/textures/block/fire_1.png".to_string(),
            png(16, 8, [255, 0, 0, 255]),
        );
        let mgr = ResourceManager::new(vec![Box::new(src)]);
        let image = load_fire_texture(&mgr).expect("load");
        assert_eq!(fire_frame_count(&image), 1);
    }

    #[test]
    fn freeze_overlay_texture_loads_as_a_plain_256x256_image() {
        let image = load_freeze_overlay_texture(&manager()).expect("load");
        assert_eq!((image.width, image.height), (256, 256));
    }

    #[test]
    fn missing_freeze_overlay_texture_is_reported() {
        let mgr = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        let err = load_freeze_overlay_texture(&mgr).expect_err("must fail closed");
        assert!(matches!(err, ScreenEffectAssetError::Missing { .. }), "{err:?}");
    }

    #[test]
    fn spyglass_scope_texture_loads_as_a_plain_256x256_image() {
        let image = load_spyglass_scope_texture(&manager()).expect("load");
        assert_eq!((image.width, image.height), (256, 256));
    }

    #[test]
    fn missing_spyglass_scope_texture_is_reported() {
        let mgr = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        let err = load_spyglass_scope_texture(&mgr).expect_err("must fail closed");
        assert!(matches!(err, ScreenEffectAssetError::Missing { .. }), "{err:?}");
    }

    #[test]
    fn nausea_overlay_texture_loads_as_a_plain_256x256_image() {
        let image = load_nausea_overlay_texture(&manager()).expect("load");
        assert_eq!((image.width, image.height), (256, 256));
    }

    #[test]
    fn missing_nausea_overlay_texture_is_reported() {
        let mgr = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        let err = load_nausea_overlay_texture(&mgr).expect_err("must fail closed");
        assert!(matches!(err, ScreenEffectAssetError::Missing { .. }), "{err:?}");
    }

    #[test]
    fn portal_overlay_texture_loads_with_32_frames() {
        let image = load_portal_overlay_texture(&manager()).expect("load");
        assert_eq!((image.width, image.height), (16, 512));
        assert_eq!(fire_frame_count(&image), 32);
    }

    #[test]
    fn missing_portal_overlay_texture_is_reported() {
        let mgr = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        let err = load_portal_overlay_texture(&mgr).expect_err("must fail closed");
        assert!(matches!(err, ScreenEffectAssetError::Missing { .. }), "{err:?}");
    }
}
