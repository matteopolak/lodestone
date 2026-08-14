//! The two textures vanilla's mob-fire billboard alternates between
//! (`FlameFeatureRenderer.buildGroup`: `ModelBakery.FIRE_0`/`FIRE_1`, resolved
//! against the block atlas as `textures/block/fire_0.png` /
//! `textures/block/fire_1.png`).
//!
//! Loaded here as two plain, unatlased [`Image`]s and combined into one
//! side-by-side texture — the same "standalone strip, not the shared block
//! atlas" choice [`crate::screen_effects::load_fire_texture`] already makes for
//! the first-person fire overlay, and for the identical reason: reaching into
//! the model pipeline's atlas for one small texture would cost either a fifth
//! bind group or plumbing a texture view across an unrelated module boundary.
//! This module is independent of `screen_effects` (which only ever needed
//! `fire_1`) because the mob billboard alternates *both* sprites per quad
//! (`FlameFeatureRenderer.prepare`) and `screen_effects` should not gain a
//! second texture just to feed a different render pass.

use crate::error::EntityFlameAssetError;
use crate::manager::ResourceManager;
use crate::texture::Image;

/// The fixed frame size (both width and height) of `fire_0.png`/`fire_1.png`'s
/// animation strips — 16×16, the same constant
/// [`crate::screen_effects::FIRE_FRAME_SIZE`] documents for `fire_1` alone.
pub const FLAME_FRAME_SIZE: u32 = 16;

fn load_plain(
    manager: &ResourceManager,
    path: &str,
    location: &str,
) -> Result<Image, EntityFlameAssetError> {
    let bytes = manager
        .read(path)
        .ok_or_else(|| EntityFlameAssetError::Missing {
            location: location.to_string(),
        })?;
    Image::decode_png(&bytes).map_err(|source| EntityFlameAssetError::Texture {
        location: location.to_string(),
        source,
    })
}

/// Loads `textures/block/fire_0.png` (16×512 in vanilla: 32 stacked 16×16
/// frames), in **raw PNG row order** — see [`reorder_fire0_display_frames`] for
/// why that is not yet display order.
///
/// # Errors
///
/// Returns [`EntityFlameAssetError`] if the texture is missing or fails to
/// decode.
pub fn load_fire0_texture(manager: &ResourceManager) -> Result<Image, EntityFlameAssetError> {
    load_plain(
        manager,
        "assets/minecraft/textures/block/fire_0.png",
        "minecraft:block/fire_0",
    )
}

/// Loads `textures/block/fire_1.png` (16×512 in vanilla: 32 stacked 16×16
/// frames), already in display order — see [`reorder_fire0_display_frames`].
///
/// # Errors
///
/// Returns [`EntityFlameAssetError`] if the texture is missing or fails to
/// decode.
pub fn load_fire1_texture(manager: &ResourceManager) -> Result<Image, EntityFlameAssetError> {
    load_plain(
        manager,
        "assets/minecraft/textures/block/fire_1.png",
        "minecraft:block/fire_1",
    )
}

/// The number of animation frames in a loaded strip (its height divided by
/// [`FLAME_FRAME_SIZE`], floored, at least 1 so a malformed/short image still
/// yields a sample-able frame count). Identical formula to
/// [`crate::screen_effects::fire_frame_count`]; kept as its own function
/// because the two modules are otherwise independent (see the module doc).
#[must_use]
pub fn flame_frame_count(image: &Image) -> u32 {
    (image.height / FLAME_FRAME_SIZE).max(1)
}

/// Reorders `fire_0.png`'s 32 stacked frames from **PNG row order** into
/// **display order**.
///
/// `assets/minecraft/textures/block/fire_0.png.mcmeta` carries an explicit
/// `frames` list — `[16, 17, …, 31, 0, 1, …, 15]` — while `fire_1.png.mcmeta`
/// is `{}` (no override, so vanilla's default is the identity sequence
/// `0, 1, …, 31`). `fire_0`'s frames are therefore stored in the PNG in a
/// *different* order than they are ever displayed; sampling row `k` directly
/// would show frame `k`'s content at the wrong point in the cycle for 16 of
/// the 32 steps.
///
/// This produces a new [`Image`] whose row `k` holds whatever
/// `load_fire0_texture` returned at PNG row `frames[k]`, so a caller that
/// indexes the *result* by `tick % 32` (exactly what [`flame_frame_count`] and
/// [`crate::screen_effects::fire_frame_count`]'s callers already do for
/// `fire_1`) gets the correct animation for both sprites with one uniform
/// rule, rather than needing to special-case `fire_0`'s permutation at every
/// call site. `fire_1` needs no such reordering (its own default order already
/// *is* the display order), so there is no `reorder_fire1_...` counterpart.
///
/// Frame count is read from the image itself via [`flame_frame_count`] rather
/// than hardcoded, but the permutation table below is `fire_0`'s specific
/// 32-entry list — an image with any other frame count is returned unchanged
/// (a malformed/replaced texture should not panic here).
#[must_use]
pub fn reorder_fire0_display_frames(image: &Image) -> Image {
    // `assets/minecraft/textures/block/fire_0.png.mcmeta`'s `frames` array,
    // verbatim.
    const DISPLAY_ORDER: [u32; 32] = [
        16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 0, 1, 2, 3, 4, 5, 6, 7, 8,
        9, 10, 11, 12, 13, 14, 15,
    ];
    let frame_count = flame_frame_count(image);
    if frame_count != DISPLAY_ORDER.len() as u32 {
        return image.clone();
    }
    let row_bytes = (image.width * 4) as usize;
    let frame_bytes = row_bytes * FLAME_FRAME_SIZE as usize;
    let mut rgba = vec![0u8; image.rgba.len()];
    for (display_slot, &source_frame) in DISPLAY_ORDER.iter().enumerate() {
        let src_start = source_frame as usize * frame_bytes;
        let dst_start = display_slot * frame_bytes;
        if src_start + frame_bytes > image.rgba.len() || dst_start + frame_bytes > rgba.len() {
            continue;
        }
        rgba[dst_start..dst_start + frame_bytes]
            .copy_from_slice(&image.rgba[src_start..src_start + frame_bytes]);
    }
    Image {
        width: image.width,
        height: image.height,
        rgba,
    }
}

/// Combines `fire_0` (already reordered into display order —
/// [`reorder_fire0_display_frames`]) and `fire_1` side by side into one
/// texture: `fire_0` occupies the left half (`u` in `0.0..0.5`), `fire_1` the
/// right half (`u` in `0.5..1.0`), both spanning the full frame strip in `v`.
///
/// A single combined texture, rather than two separate ones, is what lets the
/// flame render pass bind exactly one extra texture (group 1 of
/// [`EntityPipeline`](../../lodestone_render/entity_pipeline/struct.EntityPipeline.html)'s
/// existing, reused bind-group layout) instead of two — see
/// `crates/lodestone-render/src/entity_pipeline.rs`'s `flame_pipeline` doc.
///
/// The output height is the taller of the two inputs (both are 512 in vanilla,
/// so this is normally exact); a shorter input is top-aligned and the
/// remainder left transparent black, so a malformed pack degrades rather than
/// panicking.
#[must_use]
pub fn combine_flame_halves(fire0_display_order: &Image, fire1: &Image) -> Image {
    let width = fire0_display_order.width + fire1.width;
    let height = fire0_display_order.height.max(fire1.height);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let blit = |dst: &mut [u8], src: &Image, x_offset: u32| {
        for y in 0..src.height.min(height) {
            let src_row_start = (y * src.width * 4) as usize;
            let src_row_end = src_row_start + (src.width * 4) as usize;
            let Some(src_row) = src.rgba.get(src_row_start..src_row_end) else {
                continue;
            };
            let dst_row_start = ((y * width + x_offset) * 4) as usize;
            let dst_row_end = dst_row_start + (src.width * 4) as usize;
            if let Some(dst_row) = dst.get_mut(dst_row_start..dst_row_end) {
                dst_row.copy_from_slice(src_row);
            }
        }
    };
    blit(&mut rgba, fire0_display_order, 0);
    blit(&mut rgba, fire1, fire0_display_order.width);
    Image {
        width,
        height,
        rgba,
    }
}

/// Loads both flame strips and combines them into the one texture the flame
/// render pass binds — [`load_fire0_texture`] reordered by
/// [`reorder_fire0_display_frames`], plus [`load_fire1_texture`] unchanged,
/// combined by [`combine_flame_halves`].
///
/// # Errors
///
/// Returns [`EntityFlameAssetError`] if either texture is missing or fails to
/// decode.
pub fn load_combined_flame_texture(
    manager: &ResourceManager,
) -> Result<Image, EntityFlameAssetError> {
    let fire0 = reorder_fire0_display_frames(&load_fire0_texture(manager)?);
    let fire1 = load_fire1_texture(manager)?;
    Ok(combine_flame_halves(&fire0, &fire1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemorySource;

    fn strip_png(w: u32, h: u32, frame_color: impl Fn(u32) -> [u8; 4]) -> Vec<u8> {
        let frames = h / FLAME_FRAME_SIZE;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for frame in 0..frames {
            let color = frame_color(frame);
            let row_bytes = (w * 4) as usize;
            let frame_start = (frame * FLAME_FRAME_SIZE) as usize * row_bytes;
            let frame_len = FLAME_FRAME_SIZE as usize * row_bytes;
            for px in buf[frame_start..frame_start + frame_len].chunks_exact_mut(4) {
                px.copy_from_slice(&color);
            }
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

    /// Each frame `k`'s pixel value is `k` itself (in the red channel), so a
    /// reorder can be checked by reading the channel value back rather than
    /// comparing raw bytes.
    fn manager() -> ResourceManager {
        let mut src = MemorySource::new("test");
        src.insert(
            "assets/minecraft/textures/block/fire_0.png".to_string(),
            strip_png(16, 512, |frame| [frame as u8, 0, 0, 255]),
        );
        src.insert(
            "assets/minecraft/textures/block/fire_1.png".to_string(),
            strip_png(16, 512, |frame| [0, frame as u8, 0, 255]),
        );
        ResourceManager::new(vec![Box::new(src)])
    }

    #[test]
    fn fire0_and_fire1_load_with_32_frames() {
        let fire0 = load_fire0_texture(&manager()).expect("load fire_0");
        let fire1 = load_fire1_texture(&manager()).expect("load fire_1");
        assert_eq!((fire0.width, fire0.height), (16, 512));
        assert_eq!(flame_frame_count(&fire0), 32);
        assert_eq!((fire1.width, fire1.height), (16, 512));
        assert_eq!(flame_frame_count(&fire1), 32);
    }

    #[test]
    fn missing_flame_textures_are_reported() {
        let mgr = ResourceManager::new(vec![Box::new(MemorySource::new("empty"))]);
        let err0 = load_fire0_texture(&mgr).expect_err("must fail closed");
        assert!(matches!(err0, EntityFlameAssetError::Missing { .. }), "{err0:?}");
        let err1 = load_fire1_texture(&mgr).expect_err("must fail closed");
        assert!(matches!(err1, EntityFlameAssetError::Missing { .. }), "{err1:?}");
    }

    /// The load-bearing case: `fire_0.png.mcmeta`'s `frames` array says PNG row
    /// 16 is displayed *first* (display slot 0), not PNG row 0 — see
    /// `reorder_fire0_display_frames`'s doc. `manager()` stamps each PNG row's
    /// frame index into the red channel, so display slot 0 must read back
    /// `16`, not `0`.
    #[test]
    fn fire0_display_order_matches_its_mcmeta_frames_list() {
        let fire0 = load_fire0_texture(&manager()).expect("load fire_0");
        let reordered = reorder_fire0_display_frames(&fire0);
        assert_eq!((reordered.width, reordered.height), (16, 512));
        // `red_at_row` takes a **display slot** (0..32) and reads the pixel row
        // that slot's frame starts at (`slot * FLAME_FRAME_SIZE`) — not the
        // slot number itself, which would only ever probe frame 0's own 16
        // pixel rows.
        let red_at_row =
            |image: &Image, slot: u32| image.rgba[(slot * FLAME_FRAME_SIZE * image.width * 4) as usize];
        // Display slot 0 must show PNG row 16's content.
        assert_eq!(red_at_row(&reordered, 0), 16, "display slot 0 must be PNG row 16");
        // Display slot 15 must show PNG row 31's content (end of the first half
        // of the mcmeta list).
        assert_eq!(
            red_at_row(&reordered, 15),
            31,
            "display slot 15 must be PNG row 31"
        );
        // Display slot 16 wraps back to PNG row 0.
        assert_eq!(red_at_row(&reordered, 16), 0, "display slot 16 must be PNG row 0");
        // Display slot 31 is PNG row 15, the mcmeta list's last entry.
        assert_eq!(red_at_row(&reordered, 31), 15, "display slot 31 must be PNG row 15");
    }

    /// The negative control for the above: `fire_1` has no `frames` override
    /// (`{}`), so its own display order must be the identity — reordering
    /// would be a bug for this sprite, not a fix.
    #[test]
    fn fire1_needs_no_reordering() {
        let fire1 = load_fire1_texture(&manager()).expect("load fire_1");
        let red_at_row = |image: &Image, row: u32| image.rgba[(row * image.width * 4) as usize + 1];
        for frame in 0..32u32 {
            assert_eq!(
                red_at_row(&fire1, frame * FLAME_FRAME_SIZE),
                frame as u8,
                "fire_1 frame {frame} must sit at its own PNG row"
            );
        }
    }

    #[test]
    fn combined_texture_is_32_wide_with_fire0_on_the_left_and_fire1_on_the_right() {
        let combined = load_combined_flame_texture(&manager()).expect("load combined");
        assert_eq!((combined.width, combined.height), (32, 512));
        // Display slot 0, left half (fire_0 reordered): PNG row 16's red-channel
        // stamp, at x = 0.
        let px = |image: &Image, x: u32, y: u32, channel: usize| {
            image.rgba[((y * image.width + x) * 4) as usize + channel]
        };
        assert_eq!(px(&combined, 0, 0, 0), 16, "left half, row 0 must be fire_0's display slot 0");
        // Right half (fire_1, unreordered): its own green-channel stamp at
        // x = 16 (fire_0's width), frame 0.
        assert_eq!(px(&combined, 16, 0, 1), 0, "right half, row 0 must be fire_1's frame 0");
        assert_eq!(
            px(&combined, 16, 16, 1),
            1,
            "right half, row 16 must be fire_1's frame 1"
        );
    }

    /// A frame count is never zero even for a malformed (shorter-than-one-frame)
    /// strip — mirrors `screen_effects::fire_frame_count_floors_at_one`.
    #[test]
    fn flame_frame_count_floors_at_one() {
        let mut src = MemorySource::new("short");
        src.insert(
            "assets/minecraft/textures/block/fire_0.png".to_string(),
            strip_png(16, 8, |_| [1, 2, 3, 255]),
        );
        let mgr = ResourceManager::new(vec![Box::new(src)]);
        let image = load_fire0_texture(&mgr).expect("load");
        assert_eq!(flame_frame_count(&image), 1);
        // And the reorder function must not panic or corrupt a non-32-frame
        // image — it returns the image unchanged.
        let unchanged = reorder_fire0_display_frames(&image);
        assert_eq!(unchanged.rgba, image.rgba);
    }
}
