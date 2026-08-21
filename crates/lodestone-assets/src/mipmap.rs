//! GPU-free mipmap generation, a faithful port of vanilla 26.2's
//! `MipmapGenerator` and `TextureUtil.solidify`.
//!
//! # Why this lives here (and not on the GPU)
//!
//! WebGPU has **no built-in mipmap generation** (unlike GL's `glGenerateMipmap`),
//! and even a hand-written GPU box-filter downsample reproduces none of the three
//! things vanilla does to keep the world looking right at distance:
//!
//! 1. **Linear-light averaging.** RGB is averaged in linear space (through an
//!    sRGB↔linear LUT), not in gamma space, so mip levels don't go muddy. Alpha
//!    is averaged arithmetically.
//! 2. **`solidify`.** Before a *cutout* texture is downsampled, every fully
//!    transparent texel has its RGB replaced by the nearest opaque texel's colour
//!    (a multi-source BFS). Without this, foliage and other cutouts bleed toward
//!    transparent-black at distance — one of the most visible voxel artifacts.
//! 3. **Alpha-coverage preservation.** After each cutout downsample the mip's
//!    alpha is rescaled so its alpha-test coverage matches the base image's, so
//!    thin features (leaves, grass, bars) don't dissolve as they recede.
//!
//! Because these are CPU pixel operations they belong in this GPU-free crate; the
//! renderer uploads the resulting per-level bytes. This is exactly vanilla's
//! split: `SpriteContents` generates a per-sprite mip chain on the CPU and
//! uploads each level into the atlas texture.
//!
//! # Determinism
//!
//! The LUTs and every operation are integer/float-deterministic, so a given input
//! produces byte-identical mip levels across runs and platforms.

use crate::texture::Image;

/// How a texture's mip levels are downsampled, mirroring vanilla's
/// `MipmapStrategy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MipStrategy {
    /// Resolve to [`Cutout`](MipStrategy::Cutout) if the image has any fully
    /// transparent texel, otherwise [`Mean`](MipStrategy::Mean). This is the
    /// vanilla default for textures without explicit metadata.
    Auto,
    /// Plain linear-light mean. Used for fully opaque / translucent textures.
    Mean,
    /// `solidify` the base, mean-downsample, then preserve alpha-test coverage at
    /// a 0.5 reference. The default for textures with transparent texels.
    Cutout,
    /// Like [`Cutout`](MipStrategy::Cutout) but with a stricter 0.3 alpha
    /// reference (used by a few vanilla textures).
    StrictCutout,
    /// Fill transparent areas with a darkened copy of the darkest opaque colour,
    /// then blend only non-transparent texels while still dividing by four
    /// (vanilla's `dark_cutout`).
    DarkCutout,
}

/// Whether an image contains transparent (`alpha == 0`) and/or translucent
/// (`0 < alpha < 255`) texels — vanilla's `Transparency`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transparency {
    /// True if any texel is fully transparent (`alpha == 0`).
    pub has_transparent: bool,
    /// True if any texel is partially transparent (`0 < alpha < 255`).
    pub has_translucent: bool,
}

/// Classifies an image's transparency, as vanilla's `NativeImage.computeTransparency`.
pub fn compute_transparency(image: &Image) -> Transparency {
    let mut has_transparent = false;
    let mut has_translucent = false;
    for px in image.rgba.chunks_exact(4) {
        match px[3] {
            0 => has_transparent = true,
            255 => {}
            _ => has_translucent = true,
        }
    }
    Transparency {
        has_transparent,
        has_translucent,
    }
}

/// The greatest mip level supported by a `width`×`height` image: the largest `n`
/// such that `2^n` divides both dimensions. A 16×16 sprite supports 4 levels
/// (16→8→4→2→1); a 1×1 supports 0.
pub fn max_mip_level(width: u32, height: u32) -> u32 {
    if width == 0 || height == 0 {
        return 0;
    }
    width.trailing_zeros().min(height.trailing_zeros())
}

// --- sRGB <-> linear lookup tables, built exactly as vanilla's `ARGB`. -------

/// Depth of the linear channel table (vanilla `LINEAR_CHANNEL_DEPTH`).
const LINEAR_DEPTH: usize = 1024;

struct Luts {
    /// 8-bit sRGB channel -> linear, quantised to `0..=1023`.
    srgb_to_linear: [u16; 256],
    /// linear `0..=1023` -> 8-bit sRGB channel.
    linear_to_srgb: [u8; LINEAR_DEPTH],
}

impl Luts {
    fn build() -> Self {
        let mut srgb_to_linear = [0u16; 256];
        for (i, slot) in srgb_to_linear.iter_mut().enumerate() {
            let channel = i as f32 / 255.0;
            *slot = (compute_srgb_to_linear(channel) * 1023.0).round() as u16;
        }
        let mut linear_to_srgb = [0u8; LINEAR_DEPTH];
        for (i, slot) in linear_to_srgb.iter_mut().enumerate() {
            let channel = i as f32 / 1023.0;
            *slot = (compute_linear_to_srgb(channel) * 255.0).round() as u8;
        }
        Self {
            srgb_to_linear,
            linear_to_srgb,
        }
    }

    /// Linear-light mean of four sRGB channel bytes, matching `ARGB.linearChannelMean`.
    fn linear_channel_mean(&self, c1: u8, c2: u8, c3: u8, c4: u8) -> u8 {
        let sum = self.srgb_to_linear[c1 as usize] as u32
            + self.srgb_to_linear[c2 as usize] as u32
            + self.srgb_to_linear[c3 as usize] as u32
            + self.srgb_to_linear[c4 as usize] as u32;
        self.linear_to_srgb[(sum / 4) as usize]
    }

    fn to_linear(&self, c: u8) -> u32 {
        self.srgb_to_linear[c as usize] as u32
    }

    fn encode_srgb(&self, linear: u32) -> u8 {
        self.linear_to_srgb[(linear as usize).min(LINEAR_DEPTH - 1)]
    }
}

fn compute_srgb_to_linear(x: f32) -> f32 {
    if x >= 0.04045 {
        ((x + 0.055) / 1.055).powf(2.4)
    } else {
        x / 12.92
    }
}

fn compute_linear_to_srgb(x: f32) -> f32 {
    if x >= 0.003_130_8 {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    } else {
        12.92 * x
    }
}

thread_local! {
    static LUTS: Luts = Luts::build();
}

/// `ARGB.meanLinear`: alpha is the arithmetic mean; RGB is the linear-light mean.
fn mean_linear(luts: &Luts, a: [u8; 4], b: [u8; 4], c: [u8; 4], d: [u8; 4]) -> [u8; 4] {
    [
        luts.linear_channel_mean(a[0], b[0], c[0], d[0]),
        luts.linear_channel_mean(a[1], b[1], c[1], d[1]),
        luts.linear_channel_mean(a[2], b[2], c[2], d[2]),
        ((a[3] as u32 + b[3] as u32 + c[3] as u32 + d[3] as u32) / 4) as u8,
    ]
}

/// `MipmapGenerator.darkenedAlphaBlend`: linear-average the four texels but skip
/// any with `alpha == 0`, always dividing by four (which darkens edges).
fn darkened_alpha_blend(luts: &Luts, texels: [[u8; 4]; 4]) -> [u8; 4] {
    let mut acc = [0f32; 4];
    for t in texels {
        if t[3] != 0 {
            acc[0] += luts.to_linear(t[0]) as f32;
            acc[1] += luts.to_linear(t[1]) as f32;
            acc[2] += luts.to_linear(t[2]) as f32;
            acc[3] += luts.to_linear(t[3]) as f32;
        }
    }
    for v in &mut acc {
        *v /= 4.0;
    }
    // `acc` is indexed R, G, B, A -- the same order the `[u8; 4]` texels are.
    // Vanilla's own line reads `ARGB.color(aTotal, rTotal, gTotal, bTotal)`,
    // which is that same tuple written in *its* channel order, not a rotation
    // of it; transcribing the argument order literally rotates every channel
    // by one and paints the alpha into blue.
    [
        luts.encode_srgb(acc[0] as u32),
        luts.encode_srgb(acc[1] as u32),
        luts.encode_srgb(acc[2] as u32),
        luts.encode_srgb(acc[3] as u32),
    ]
}

/// Replaces every fully transparent texel's RGB with the nearest opaque texel's
/// colour (keeping `alpha == 0`), via a 4-neighbour multi-source BFS. This is
/// vanilla's `TextureUtil.solidify` and is what prevents cutouts bleeding toward
/// transparent-black when downsampled.
pub fn solidify(image: &mut Image) {
    let w = image.width as usize;
    let h = image.height as usize;
    let n = w * h;
    if n == 0 {
        return;
    }
    let mut nearest = vec![[0u8; 3]; n];
    let mut dist = vec![u32::MAX; n];
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();

    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let a = image.rgba[i * 4 + 3];
            if a != 0 {
                dist[i] = 0;
                nearest[i] = [
                    image.rgba[i * 4],
                    image.rgba[i * 4 + 1],
                    image.rgba[i * 4 + 2],
                ];
                queue.push_back(i);
            }
        }
    }

    const DIRS: [(i64, i64); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    while let Some(i) = queue.pop_front() {
        let x = (i % w) as i64;
        let y = (i / w) as i64;
        for (dx, dy) in DIRS {
            let nx = x + dx;
            let ny = y + dy;
            if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                continue;
            }
            let ni = (ny as usize) * w + (nx as usize);
            if dist[ni] > dist[i] + 1 {
                dist[ni] = dist[i] + 1;
                nearest[ni] = nearest[i];
                queue.push_back(ni);
            }
        }
    }

    for (px, near) in image.rgba.chunks_exact_mut(4).zip(&nearest) {
        if px[3] == 0 {
            px[0] = near[0];
            px[1] = near[1];
            px[2] = near[2];
        }
    }
}

/// Fills transparent areas with a darkened copy of the darkest opaque colour,
/// matching vanilla's `TextureUtil.fillEmptyAreasWithDarkColor`.
fn fill_empty_with_dark(image: &mut Image) {
    let mut darkest = [0u8; 3];
    let mut min_brightness = u32::MAX;
    for px in image.rgba.chunks_exact(4) {
        if px[3] != 0 {
            let b = px[0] as u32 + px[1] as u32 + px[2] as u32;
            if b < min_brightness {
                min_brightness = b;
                darkest = [px[0], px[1], px[2]];
            }
        }
    }
    let dark = [
        (3 * darkest[0] as u32 / 4) as u8,
        (3 * darkest[1] as u32 / 4) as u8,
        (3 * darkest[2] as u32 / 4) as u8,
    ];
    for px in image.rgba.chunks_exact_mut(4) {
        if px[3] == 0 {
            px[0] = dark[0];
            px[1] = dark[1];
            px[2] = dark[2];
        }
    }
}

/// Estimates the fraction of the image that passes an alpha test at `alpha_ref`,
/// bilinearly supersampling each 2×2 texel quad on a 4×4 grid — vanilla's
/// `MipmapGenerator.alphaTestCoverage`.
pub fn alpha_test_coverage(image: &Image, alpha_ref: f32, alpha_scale: f32) -> f32 {
    let w = image.width as i64;
    let h = image.height as i64;
    if w < 2 || h < 2 {
        return 0.0;
    }
    let a = |x: i64, y: i64| -> f32 {
        (image.pixel(x as u32, y as u32)[3] as f32 / 255.0 * alpha_scale).clamp(0.0, 1.0)
    };
    let mut coverage = 0.0f32;
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            let a00 = a(x, y);
            let a10 = a(x + 1, y);
            let a01 = a(x, y + 1);
            let a11 = a(x + 1, y + 1);
            let mut texel = 0.0f32;
            for sy in 0..4 {
                let fy = (sy as f32 + 0.5) / 4.0;
                for sx in 0..4 {
                    let fx = (sx as f32 + 0.5) / 4.0;
                    let alpha = a00 * (1.0 - fx) * (1.0 - fy)
                        + a10 * fx * (1.0 - fy)
                        + a01 * (1.0 - fx) * fy
                        + a11 * fx * fy;
                    if alpha > alpha_ref {
                        texel += 1.0;
                    }
                }
            }
            coverage += texel / 16.0;
        }
    }
    coverage / ((w - 1) * (h - 1)) as f32
}

/// Rescales an image's alpha so its alpha-test coverage matches `desired`, then
/// biases it — vanilla's `MipmapGenerator.scaleAlphaToCoverage`.
fn scale_alpha_to_coverage(image: &mut Image, desired: f32, alpha_ref: f32, bias: f32) {
    let mut min_scale = 0.0f32;
    let mut max_scale = 4.0f32;
    let mut scale = 1.0f32;
    let mut best_scale = 1.0f32;
    let mut best_error = f32::MAX;

    for _ in 0..5 {
        let coverage = alpha_test_coverage(image, alpha_ref, scale);
        let error = (coverage - desired).abs();
        if error < best_error {
            best_error = error;
            best_scale = scale;
        }
        if coverage < desired {
            min_scale = scale;
        } else if coverage > desired {
            max_scale = scale;
        } else {
            break;
        }
        scale = (min_scale + max_scale) * 0.5;
    }

    for px in image.rgba.chunks_exact_mut(4) {
        let alpha = px[3] as f32 / 255.0;
        let scaled = (alpha * best_scale + bias + 0.025).clamp(0.0, 1.0);
        px[3] = (scaled * 255.0).round() as u8;
    }
}

/// Downsamples an image to half size using the given per-quad combiner.
fn downsample(image: &Image, combine: impl Fn([[u8; 4]; 4]) -> [u8; 4]) -> Image {
    let dw = image.width >> 1;
    let dh = image.height >> 1;
    let mut rgba = vec![0u8; (dw as usize) * (dh as usize) * 4];
    for y in 0..dh {
        for x in 0..dw {
            let c = [
                image.pixel(x * 2, y * 2),
                image.pixel(x * 2 + 1, y * 2),
                image.pixel(x * 2, y * 2 + 1),
                image.pixel(x * 2 + 1, y * 2 + 1),
            ];
            let out = combine(c);
            let i = ((y * dw + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&out);
        }
    }
    Image {
        width: dw,
        height: dh,
        rgba,
    }
}

/// Generates the mip chain for a single image, faithfully reproducing vanilla's
/// `MipmapGenerator.generateMipLevels`.
///
/// Returns `levels + 1` images (`[0]` is the base), capped at
/// [`max_mip_level`]. `alpha_cutoff_bias` is vanilla's per-texture bias (default
/// `0.0`). The base is treated as a block texture (never an `item/` sprite), so
/// cutout base solidification is applied.
pub fn generate_mip_levels(
    image: &Image,
    levels: u32,
    strategy: MipStrategy,
    alpha_cutoff_bias: f32,
) -> Vec<Image> {
    let levels = levels.min(max_mip_level(image.width, image.height));

    let strategy = match strategy {
        MipStrategy::Auto => {
            if compute_transparency(image).has_transparent {
                MipStrategy::Cutout
            } else {
                MipStrategy::Mean
            }
        }
        other => other,
    };

    let is_cutout = matches!(
        strategy,
        MipStrategy::Cutout | MipStrategy::StrictCutout | MipStrategy::DarkCutout
    );
    let cutout_ref = if strategy == MipStrategy::StrictCutout {
        0.3
    } else {
        0.5
    };

    // Base-level preparation (vanilla applies this once, to non-`item/` textures).
    let mut base = image.clone();
    match strategy {
        MipStrategy::Cutout | MipStrategy::StrictCutout => solidify(&mut base),
        MipStrategy::DarkCutout => fill_empty_with_dark(&mut base),
        _ => {}
    }

    let mut result = Vec::with_capacity(levels as usize + 1);
    let original_coverage = if is_cutout {
        alpha_test_coverage(&base, cutout_ref, 1.0)
    } else {
        0.0
    };
    result.push(base);

    LUTS.with(|luts| {
        for level in 1..=levels as usize {
            let prev = &result[level - 1];
            let mut data = if strategy == MipStrategy::DarkCutout {
                downsample(prev, |c| darkened_alpha_blend(luts, c))
            } else {
                downsample(prev, |c| mean_linear(luts, c[0], c[1], c[2], c[3]))
            };
            if is_cutout {
                scale_alpha_to_coverage(
                    &mut data,
                    original_coverage,
                    cutout_ref,
                    alpha_cutoff_bias,
                );
            }
            result.push(data);
        }
    });

    result
}
