//! Tests for the GPU-free mipmap generator (faithful port of vanilla 26.2's
//! own mipmap-generator and texture-util "solidify" step).

use lodestone_assets::Image;
use lodestone_assets::mipmap::{
    MipStrategy, alpha_test_coverage, compute_transparency, generate_mip_levels, max_mip_level,
    solidify,
};

/// Builds an RGBA8 image from a flat list of `[r,g,b,a]` pixels, row-major.
fn image(width: u32, height: u32, pixels: &[[u8; 4]]) -> Image {
    assert_eq!(pixels.len(), (width * height) as usize);
    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    for p in pixels {
        rgba.extend_from_slice(p);
    }
    Image {
        width,
        height,
        rgba,
    }
}

#[test]
fn max_mip_level_is_the_power_of_two_dividing_the_smaller_dimension() {
    assert_eq!(max_mip_level(16, 16), 4); // 16 = 2^4
    assert_eq!(max_mip_level(1, 1), 0);
    assert_eq!(max_mip_level(16, 64), 4); // min(4, 6)
    assert_eq!(max_mip_level(48, 16), 4); // 48 = 16*3 -> 2^4 divides it
    assert_eq!(max_mip_level(24, 16), 3); // 24 = 8*3 -> 2^3
    assert_eq!(max_mip_level(8, 8), 3);
}

#[test]
fn rgb_is_averaged_in_linear_light_not_srgb_space() {
    // A 2x2 half-black half-white cell. In *linear* space the mean of 0 and 1.0
    // is 0.5 linear, which is sRGB ~188 — markedly brighter than a naive sRGB
    // average of (0+255)/2 = 127. This is what stops mip levels going muddy.
    let img = image(
        2,
        2,
        &[
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [255, 255, 255, 255],
            [0, 0, 0, 255],
        ],
    );
    let mips = generate_mip_levels(&img, 1, MipStrategy::Mean, 0.0);
    assert_eq!(mips.len(), 2);
    let px = mips[1].pixel(0, 0);
    assert!(
        px[0] >= 184 && px[0] <= 192,
        "expected linear-space mean ~188, got {} (a value near 127 would mean sRGB averaging)",
        px[0]
    );
    assert_eq!(px[3], 255, "opaque input stays opaque");
}

#[test]
fn alpha_is_averaged_arithmetically() {
    // Alpha uses a plain arithmetic mean (not linear-space): (0+0+255+255)/4.
    let img = image(
        2,
        2,
        &[
            [10, 20, 30, 0],
            [10, 20, 30, 0],
            [10, 20, 30, 255],
            [10, 20, 30, 255],
        ],
    );
    let mips = generate_mip_levels(&img, 1, MipStrategy::Mean, 0.0);
    assert_eq!(
        mips[1].pixel(0, 0)[3],
        127,
        "(0+0+255+255)/4 truncates to 127"
    );
}

#[test]
fn solidify_fills_transparent_texels_with_nearest_opaque_colour() {
    // One opaque reddish texel, the rest transparent. Solidify floods the
    // nearest opaque colour into every transparent texel, keeping alpha 0.
    let mut img = image(
        2,
        2,
        &[[200, 50, 50, 255], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0]],
    );
    solidify(&mut img);
    for (x, y) in [(1, 0), (0, 1), (1, 1)] {
        let p = img.pixel(x, y);
        assert_eq!(
            [p[0], p[1], p[2]],
            [200, 50, 50],
            "transparent texel at ({x},{y}) should carry the nearest opaque colour"
        );
        assert_eq!(p[3], 0, "solidify must not make texels opaque");
    }
}

#[test]
fn cutout_mip_does_not_bleed_to_black_under_transparent_areas() {
    // Reddish opaque corner, rest transparent. Without solidify the downsample
    // would average the reddish texel with black RGB and darken sharply. AUTO
    // sees transparency -> CUTOUT -> solidify, so the mip keeps the red.
    let img = image(
        2,
        2,
        &[[200, 50, 50, 255], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0]],
    );
    let mips = generate_mip_levels(&img, 1, MipStrategy::Auto, 0.0);
    let px = mips[1].pixel(0, 0);
    assert!(
        px[0] > 150 && px[1] > 30 && px[2] > 30,
        "cutout mip bled toward black: {px:?} (solidify should preserve the reddish colour)"
    );
}

#[test]
fn auto_strategy_uses_mean_when_fully_opaque() {
    // Fully opaque image -> AUTO resolves to MEAN, which does not solidify or
    // rescale alpha. All alphas stay 255.
    let img = image(
        2,
        2,
        &[
            [10, 200, 10, 255],
            [10, 200, 10, 255],
            [10, 200, 10, 255],
            [10, 200, 10, 255],
        ],
    );
    let t = compute_transparency(&img);
    assert!(!t.has_transparent && !t.has_translucent, "fully opaque");
    let mips = generate_mip_levels(&img, 1, MipStrategy::Auto, 0.0);
    assert_eq!(mips[1].pixel(0, 0)[3], 255);
}

#[test]
fn coverage_preservation_keeps_thin_opaque_features_from_dissolving() {
    // A 4x4 with a sparse set of opaque texels on a transparent field. A naive
    // box downsample halves the opaque alpha and the alpha-test coverage drops;
    // vanilla rescales the mip's alpha to preserve the original coverage.
    let o = [255, 255, 255, 255];
    let t = [0, 0, 0, 0];
    let img = image(
        4,
        4,
        &[
            o, t, o, t, //
            t, t, t, t, //
            o, t, o, t, //
            t, t, t, t, //
        ],
    );
    let base_cov = alpha_test_coverage(&img, 0.5, 1.0);
    let mips = generate_mip_levels(&img, 1, MipStrategy::Cutout, 0.0);
    let mip_cov = alpha_test_coverage(&mips[1], 0.5, 1.0);
    // The preserved coverage should track the original far better than a naive
    // downsample (whose coverage would collapse well below half).
    assert!(
        (mip_cov - base_cov).abs() < 0.25,
        "coverage not preserved: base {base_cov:.3} vs mip {mip_cov:.3}"
    );
}

#[test]
fn levels_are_capped_at_the_max_supported_for_the_size() {
    // Asking for more levels than the size supports yields only what fits.
    let img = image(2, 2, &[[1, 2, 3, 255]; 4]);
    let mips = generate_mip_levels(&img, 8, MipStrategy::Mean, 0.0);
    // 2x2 supports one reduction (to 1x1): base + level 1.
    assert_eq!(mips.len(), 2);
    assert_eq!((mips[1].width, mips[1].height), (1, 1));
}

#[test]
fn level_zero_is_the_untouched_source_for_mean() {
    let img = image(2, 2, &[[9, 8, 7, 255]; 4]);
    let mips = generate_mip_levels(&img, 1, MipStrategy::Mean, 0.0);
    assert_eq!(mips[0], img, "MEAN must not mutate the base level");
}
