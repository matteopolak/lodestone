//! Texture upload: the atlas-vs-array decision, seam-free mip generation, and
//! the GPU-side atlas.
//!
//! `lodestone-assets` produces a CPU-side [`Atlas`](lodestone_assets::atlas::Atlas)
//! (`width`, `height`, `layers`, `rgba`, plus per-sprite UV rects and animation
//! info) and deliberately leaves anything GPU-shaped to us. This module takes
//! that data, decides how to lay it out on the GPU, builds a mip chain that does
//! not bleed across sprite boundaries, and uploads it.
//!
//! # Atlas vs. texture array (measured, not assumed)
//!
//! The vanilla block atlas is **1233 sprites**, ~93% of them exactly 16×16, with
//! nearly all of the rest 16-wide vertical animation strips of 16×16 frames, and
//! only ~42 sprites wider than 16px. Two layouts:
//!
//! * **2D atlas** — every sprite stitched into one texture. Fits anything, but a
//!   naive box-filter mip mixes texels across sprite borders, so distant terrain
//!   shows seams. Solvable with per-sprite mip isolation (below).
//! * **Texture array** — one 16×16 sprite per layer. Each layer has its own mip
//!   chain, so mip bleed is *impossible* by construction; but all layers must
//!   share dimensions, and the layer count is capped
//!   (`max_texture_array_layers = 2048` on this machine).
//!
//! The crucial arithmetic: **one physical frame per layer**. Static sprites are
//! one layer each, but an animated strip needs one layer per frame. The static
//! 16×16 set alone (~1147 sprites) fits comfortably in 2048 layers, but once you
//! expand every animation frame the total physical frame count exceeds 2048, so
//! a *pure* one-frame-per-layer array does **not** fit — and the ~42 wide sprites
//! do not fit a 16×16 layer at all. See [`array_layers_needed`] / [`layout_fits`].
//!
//! VRAM is not the deciding factor: at 16×16×RGBA both layouts are only a couple
//! of MB with mips (see [`mip_pyramid_bytes`]). The decision is **mip
//! correctness vs. flexibility**. Recommendation logic lives in
//! [`recommend_layout`]; the report carries the concrete numbers.
//!
//! # Physical layout vs. binding model — two independent axes
//!
//! An earlier version of [`recommend_layout`] gated the array layout behind a
//! `bindless` flag. That was **wrong**, and the wasm feasibility spike caught it:
//! a `texture_2d_array` sampled `textureSample(t, s, uv, layer)` with the layer
//! carried per vertex requires **neither** `TEXTURE_BINDING_ARRAY` **nor**
//! non-uniform indexing. Only *true bindless* — a `binding_array<texture_2d>`
//! indexed non-uniformly per fragment — needs those, and **neither WebGPU nor
//! WebGL2 exposes them**. So the two questions are orthogonal:
//!
//! * *Physical layout* ([`recommend_layout`]) — what fits: `Atlas2D`,
//!   `TextureArray`, or `Hybrid`, decided only by sprite counts and the layer cap.
//! * *Binding model* ([`select_binding_model`]) — how the shader reaches it:
//!   `SingleTexture2D`, the portable `Texture2DArray`, or the optional `Bindless`
//!   upgrade. The array path is first-class on the web, **not** a degraded mode.
//!
//! Portability note: WebGPU only *guarantees* `maxTextureArrayLayers = 256`
//! ([`GUARANTEED_MAX_ARRAY_LAYERS_WEBGPU`]), far below the 2048 measured on Metal.
//! At 256 layers the 1233-sprite atlas cannot live in an array at all, so the
//! portable web layout is `Atlas2D`. The packed vertex's 11-bit sprite field
//! (2048 sprites) is unaffected: in the `Atlas2D` path it indexes a UV-lookup
//! table, not a texture-array layer, so it addresses the whole atlas everywhere.

use lodestone_assets::Atlas;

use crate::caps::GpuCapabilities;

/// This adapter's texture-array layer cap, as measured on the primary target
/// (Apple M5 / Metal). A browser build cannot rely on this — see
/// [`GUARANTEED_MAX_ARRAY_LAYERS_WEBGPU`].
pub const MEASURED_MAX_ARRAY_LAYERS: u32 = 2048;

/// Requested mip depth for the block/model atlases, matching vanilla's
/// `mipmapLevels` default (`Options.java`: `IntRange(0, 4)`, default `4`).
///
/// This is the depth `BlockAtlas::build` (this crate's `block_resolver`) uses
/// for a session that has never touched the setting, and it is also the
/// source of the padding that path requests: vanilla's `Stitcher.padding = 1
/// << mipLevel << clamp(anisotropyBit - 1, 0, 4)`, and with no anisotropic
/// filtering modelled here that reduces to `1 << levels` — see [`GpuAtlas`]'s
/// sampler (`min_filter: Linear`, minifying) and
/// [`lodestone_assets::AtlasBuilder::with_padding`]'s doc for why a
/// `Linear`-sampled atlas needs a real gutter and not just isolated mip
/// *generation*.
///
/// The live `mipmapLevels` video setting now has a real consumer:
/// `crate::block_resolver::BlockAtlas::build_with_mip_levels` takes an
/// explicit depth, and `lodestone-shell/src/resources.rs`'s `mipmap_levels`
/// (seeded from this constant, then overridden by whatever the player last
/// chose) is what the shell's atlas loader actually passes. Changing the
/// setting bumps the same `pack_generation` counter a resource-pack selection
/// change does, so it rebuilds the atlas, remeshes the world and swaps the GPU
/// bind groups through the identical live-reload path — see that module's
/// `set_mipmap_levels` for the trigger.
pub const BLOCK_ATLAS_MIP_LEVELS: u32 = 4;

/// WebGPU's *guaranteed minimum* `maxTextureArrayLayers`. The spec's default
/// limit is **256**, so a portable renderer must assume no more than this until
/// it queries the real adapter. This is far below the 2048 we measured on Metal
/// ([`MEASURED_MAX_ARRAY_LAYERS`]): with only 256 layers a one-frame-per-layer
/// array of the 1233-sprite block atlas (whose 16×16 static majority alone is
/// ~1147 sprites) does **not** fit, so the portable layout on web is the
/// stitched [`TextureLayout::Atlas2D`], not an array.
pub const GUARANTEED_MAX_ARRAY_LAYERS_WEBGPU: u32 = 256;

/// A rectangular region of an atlas image, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteRect {
    /// Left edge in pixels.
    pub x: u32,
    /// Top edge in pixels.
    pub y: u32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

/// A summary of an atlas's sprite population, enough to choose a layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasStats {
    /// Total distinct sprites.
    pub total_sprites: u32,
    /// Sprites that are exactly 16×16 (one array layer each).
    pub static_16: u32,
    /// Total physical frames across *all* sprites (static counts as 1).
    ///
    /// This is the number of 16×16 layers a per-frame array would need for the
    /// 16-wide-compatible sprites.
    pub total_frames: u32,
    /// Sprites wider than 16px (cannot share a 16×16 array layer).
    pub wide_sprites: u32,
}

impl AtlasStats {
    /// Derive stats from a built [`Atlas`], counting physical animation frames.
    #[must_use]
    pub fn from_atlas(atlas: &Atlas) -> Self {
        let mut static_16 = 0;
        let mut total_frames = 0;
        let mut wide = 0;
        for s in atlas.sprites() {
            total_frames += s.frame_count.max(1);
            if s.width > 16 {
                wide += 1;
            } else if s.width == 16 && s.frame_height == 16 && s.frame_count == 1 {
                static_16 += 1;
            }
        }
        AtlasStats {
            total_sprites: atlas.sprites().len() as u32,
            static_16,
            total_frames,
            wide_sprites: wide,
        }
    }
}

/// Packing occupancy of a stitched 2D atlas: how much of its pixel area is
/// actually spoken for by sprite rectangles, versus padding/unused space.
///
/// The seam issue #160 asked for and this crate did not have: `AtlasStats`
/// reports sprite/frame *population* (counts), and [`GpuAtlas`] (below)
/// exposes only `width`/`height` — neither answers "how full is this atlas".
/// Deliberately CPU-side and GPU-adapter-free: every input
/// ([`Atlas::width`]/[`height`](Atlas::height) and the per-frame rectangles
/// [`sprite_rects`] already derives) lives on the asset-pipeline
/// [`Atlas`], so a bench can compute this for any built atlas — synthetic or
/// real — with no `wgpu::Device` at all. [`GpuAtlas`] carries the identical
/// `width`/`height` the physical texture was created at, so this figure
/// describes the uploaded texture too, not just the CPU-side source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtlasOccupancy {
    /// Sum of every physical frame's pixel area (`w * h`), animation frames
    /// counted individually since each occupies its own physical region —
    /// the same unit [`sprite_rects`] enumerates.
    pub used_pixels: u64,
    /// `width * height` of the atlas as a whole.
    pub total_pixels: u64,
    /// `used_pixels / total_pixels`, in `[0.0, 1.0]` for any atlas the builder
    /// produced (it never places a sprite outside the image). `0.0` on a
    /// zero-area atlas rather than dividing by zero.
    pub fraction: f64,
}

/// Compute [`AtlasOccupancy`] for a built [`Atlas`]. Cheap: one pass over
/// [`sprite_rects`], no allocation beyond that `Vec`.
#[must_use]
pub fn atlas_occupancy(atlas: &Atlas) -> AtlasOccupancy {
    let used_pixels: u64 = sprite_rects(atlas)
        .iter()
        .map(|r| u64::from(r.w) * u64::from(r.h))
        .sum();
    let total_pixels = u64::from(atlas.width) * u64::from(atlas.height);
    let fraction = if total_pixels == 0 {
        0.0
    } else {
        used_pixels as f64 / total_pixels as f64
    };
    AtlasOccupancy {
        used_pixels,
        total_pixels,
        fraction,
    }
}

/// Which GPU texture layout to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureLayout {
    /// One stitched 2D atlas with per-sprite-isolated mips.
    Atlas2D,
    /// One 16×16 sprite (frame) per array layer.
    TextureArray,
    /// 16×16 array for the static majority, a small 2D atlas for animations and
    /// oversized sprites.
    Hybrid,
}

/// Layers a pure one-frame-per-layer 16×16 array would need: every physical
/// frame of every 16-wide-compatible sprite. Wide sprites are excluded because
/// they cannot live in a 16×16 layer.
#[must_use]
pub const fn array_layers_needed(stats: AtlasStats) -> u32 {
    // total_frames already counts each physical frame once; subtract the wide
    // sprites (each counted as at least one frame) since they go elsewhere.
    stats.total_frames.saturating_sub(stats.wide_sprites)
}

/// Whether a pure texture array fits within `max_layers`.
#[must_use]
pub const fn layout_fits(stats: AtlasStats, max_layers: u32) -> bool {
    stats.wide_sprites == 0 && array_layers_needed(stats) <= max_layers
}

/// Recommend a **physical** layout given the atlas population and the adapter's
/// texture-array layer cap. This is purely about *what fits*, and deliberately
/// takes no `bindless` argument: a `texture_2d_array` needs no bindless features
/// at all (see [`AtlasBindingModel`]), so gating the array layout on bindless —
/// as an earlier version did — was a conflation of two independent axes.
///
/// * Fits a pure 16×16 array within `max_layers` → **TextureArray**.
/// * Static 16×16 majority fits but animations/wide sprites push it over →
///   **Hybrid** (array for the majority, small 2D atlas for the rest).
/// * Even the static majority overflows `max_layers` (e.g. the 256-layer WebGPU
///   guarantee) → **Atlas2D**, the universal stitched fallback.
#[must_use]
pub const fn recommend_layout(stats: AtlasStats, max_layers: u32) -> TextureLayout {
    if layout_fits(stats, max_layers) {
        TextureLayout::TextureArray
    } else if stats.static_16 <= max_layers {
        TextureLayout::Hybrid
    } else {
        TextureLayout::Atlas2D
    }
}

/// How the shader *reaches* atlas sprites — an axis independent of the physical
/// [`TextureLayout`]. This is the distinction the wasm feasibility spike turned
/// on: a `texture_2d_array` sampled with a per-vertex layer index needs
/// **neither** `TEXTURE_BINDING_ARRAY` **nor** non-uniform indexing, so it is a
/// first-class path on WebGPU/WebGL2. Only *true bindless* — a
/// `binding_array<texture_2d>` indexed non-uniformly per fragment — needs those
/// features, and neither web target has them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasBindingModel {
    /// A single `texture_2d` sampled by UV. Pairs with [`TextureLayout::Atlas2D`].
    /// Works everywhere; relies on per-sprite mip isolation to avoid bleed.
    SingleTexture2D,
    /// One `texture_2d_array` sampled as `textureSample(t, s, uv, layer)` with
    /// the layer carried per vertex. Each layer owns its mip chain, so bleed is
    /// impossible. **No bindless features required** — usable on WebGPU/WebGL2.
    /// This is the portable array path, not a degraded mode.
    Texture2DArray,
    /// A `binding_array<texture_2d>` indexed non-uniformly per fragment. Requires
    /// [`GpuCapabilities::supports_bindless_atlas`]; unavailable on WebGPU/WebGL2.
    /// An optional upgrade for heterogeneous sprites, never a requirement for our
    /// uniform-dimension 16×16 layers.
    Bindless,
}

/// Choose a binding model purely from capabilities and the physical layout.
///
/// A 2D atlas always binds as a single texture. An array/hybrid layout binds as
/// a portable [`AtlasBindingModel::Texture2DArray`] unless the adapter actually
/// supports bindless, in which case it may take the [`AtlasBindingModel::Bindless`]
/// upgrade. Because the array path is portable and sufficient, the *absence* of
/// bindless is never a degraded state — it is the common, first-class case
/// (every web target lands here by construction).
#[must_use]
pub fn select_binding_model(caps: &GpuCapabilities, layout: TextureLayout) -> AtlasBindingModel {
    match layout {
        TextureLayout::Atlas2D => AtlasBindingModel::SingleTexture2D,
        TextureLayout::TextureArray | TextureLayout::Hybrid => {
            if caps.supports_bindless_atlas() {
                AtlasBindingModel::Bindless
            } else {
                AtlasBindingModel::Texture2DArray
            }
        }
    }
}

/// Total bytes of an RGBA8 mip pyramid for a `width × height × layers` texture,
/// including all mip levels down to 1×1.
#[must_use]
pub const fn mip_pyramid_bytes(width: u32, height: u32, layers: u32) -> usize {
    let mut total = 0usize;
    let mut w = width;
    let mut h = height;
    loop {
        total += (w as usize) * (h as usize) * 4 * (layers as usize);
        if w == 1 && h == 1 {
            break;
        }
        w = if w > 1 { w / 2 } else { 1 };
        h = if h > 1 { h / 2 } else { 1 };
    }
    total
}

/// Number of mip levels for a `width × height` texture (down to 1×1).
#[must_use]
pub const fn mip_level_count(width: u32, height: u32) -> u32 {
    let mut max = if width > height { width } else { height };
    let mut levels = 1;
    while max > 1 {
        max /= 2;
        levels += 1;
    }
    levels
}

/// One level of an isolated mip chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MipLevel {
    /// Level width in pixels.
    pub width: u32,
    /// Level height in pixels.
    pub height: u32,
    /// RGBA8 pixels, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Generate a mip chain for a 2D atlas where **each sprite is downsampled using
/// only its own texels** (2×2 box filter clamped to the sprite's rectangle), so
/// no sprite ever bleeds into a neighbour at any mip level.
///
/// Level 0 is a copy of the source. Each subsequent level halves the atlas
/// dimensions and each sprite's rectangle, filtering within the sprite's bounds.
/// Sprite rectangles are assumed to start and size on even boundaries (the 16×16
/// grid does); odd remainders clamp.
///
/// The atlas width/height need **not** be powers of two. At mip levels deep
/// enough that the whole atlas has shrunk below the sprite grid spacing (e.g. a
/// 160×16 strip whose level-6 atlas is only 2px wide), a sprite's shifted origin
/// can land outside the level buffer; such destinations are clamped into the
/// level rather than panicking. Those levels are already sub-sprite-resolution,
/// so the low levels that matter for sampling are unaffected.
#[must_use]
pub fn generate_isolated_mips(
    width: u32,
    height: u32,
    rgba: &[u8],
    sprites: &[SpriteRect],
    levels: u32,
) -> Vec<MipLevel> {
    let mut out = Vec::with_capacity(levels as usize);
    out.push(MipLevel {
        width,
        height,
        rgba: rgba.to_vec(),
    });

    for level in 1..levels {
        let prev = &out[(level - 1) as usize];
        let lw = (width >> level).max(1);
        let lh = (height >> level).max(1);
        let mut data = vec![0u8; (lw * lh * 4) as usize];

        for sprite in sprites {
            // The sprite's rect in this level and the previous level.
            let sx = sprite.x >> level;
            let sy = sprite.y >> level;
            let sw = (sprite.w >> level).max(1);
            let sh = (sprite.h >> level).max(1);
            let psx = sprite.x >> (level - 1);
            let psy = sprite.y >> (level - 1);
            let psw = (sprite.w >> (level - 1)).max(1);
            let psh = (sprite.h >> (level - 1)).max(1);

            for oy in 0..sh {
                for ox in 0..sw {
                    // Source 2×2 block in the previous level, clamped to sprite.
                    let mut acc = [0u32; 4];
                    let mut n = 0u32;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let px = (ox * 2 + dx).min(psw - 1);
                            let py = (oy * 2 + dy).min(psh - 1);
                            let gx = (psx + px).min(prev.width - 1);
                            let gy = (psy + py).min(prev.height - 1);
                            let idx = ((gy * prev.width + gx) * 4) as usize;
                            for (c, a) in acc.iter_mut().enumerate() {
                                *a += prev.rgba[idx + c] as u32;
                            }
                            n += 1;
                        }
                    }
                    let gx = (sx + ox).min(lw - 1);
                    let gy = (sy + oy).min(lh - 1);
                    let out_idx = ((gy * lw + gx) * 4) as usize;
                    for (c, a) in acc.iter().enumerate() {
                        data[out_idx + c] = (a / n) as u8;
                    }
                }
            }
        }

        out.push(MipLevel {
            width: lw,
            height: lh,
            rgba: data,
        });
    }

    out
}

/// Collect per-physical-frame sprite rectangles from an [`Atlas`]. Each
/// animation frame is its own rectangle, so mip isolation keeps frames from
/// bleeding into each other too.
#[must_use]
pub fn sprite_rects(atlas: &Atlas) -> Vec<SpriteRect> {
    let mut rects = Vec::new();
    for s in atlas.sprites() {
        let frames = s.frame_count.max(1);
        for f in 0..frames {
            if let Some([x, y, w, h]) = s.frame_pixel_rect(f) {
                rects.push(SpriteRect { x, y, w, h });
            }
        }
    }
    rects
}

/// A GPU-resident 2D atlas texture with an isolated mip chain, plus a sampler.
#[derive(Debug)]
pub struct GpuAtlas {
    /// The uploaded texture.
    pub texture: wgpu::Texture,
    /// A default view over all mip levels.
    pub view: wgpu::TextureView,
    /// A trilinear, clamped sampler.
    pub sampler: wgpu::Sampler,
    /// Atlas width in pixels.
    pub width: u32,
    /// Atlas height in pixels.
    pub height: u32,
}

impl GpuAtlas {
    /// Upload RGBA8 atlas pixels, generating per-sprite-isolated mips.
    ///
    /// `sprites` are the per-frame rectangles used to keep mip downsampling
    /// within sprite boundaries; pass an empty slice to treat the whole image as
    /// one region (only correct if there are no internal sprite seams).
    ///
    /// This regenerates mips locally with a per-sprite box filter. It is the
    /// right path for **synthetic** atlases (tests, tools) that carry no pyramid;
    /// for a real [`Atlas`] built by lodestone-assets, prefer [`Self::from_atlas`],
    /// which consumes the vanilla-faithful pyramid instead of regenerating it.
    #[must_use]
    pub fn from_rgba(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        rgba: &[u8],
        sprites: &[SpriteRect],
    ) -> Self {
        let levels = mip_level_count(width, height);
        let whole = [SpriteRect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        }];
        let src = if sprites.is_empty() {
            &whole[..]
        } else {
            sprites
        };
        let mips = generate_isolated_mips(width, height, rgba, src, levels);
        Self::upload_mips(device, queue, width, height, &mips, wgpu::FilterMode::Nearest)
    }

    /// Create the texture, upload every mip level, and build the sampler. Shared
    /// by [`Self::from_rgba`] (regenerated mips) and [`Self::from_atlas`]
    /// (asset-supplied mips). The texture's `mip_level_count` follows `mips.len()`
    /// so an asset that capped its pyramid on an awkward sprite is honoured
    /// exactly.
    ///
    /// `mag_filter` is the one thing that differs between the two kinds of
    /// consumer, and vanilla differs the same way: `TextureAtlas` keeps its own
    /// sampler at `getClampToEdge(FilterMode.NEAREST)` — right for a GUI, item
    /// or particle sheet, which is only ever drawn at or above 1:1 and must
    /// stay crisp — while `LevelRenderer` builds a **separate**
    /// `chunkLayerSampler` for terrain with `LINEAR` for *both* filters. See
    /// [`Self::from_atlas_terrain`].
    fn upload_mips(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        mips: &[MipLevel],
        mag_filter: wgpu::FilterMode,
    ) -> Self {
        let levels = mips.len().max(1) as u32;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lodestone-atlas"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        for (level, mip) in mips.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &mip.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(mip.width * 4),
                    rows_per_image: Some(mip.height),
                },
                wgpu::Extent3d {
                    width: mip.width,
                    height: mip.height,
                    depth_or_array_layers: 1,
                },
            );
        }

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lodestone-atlas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        GpuAtlas {
            texture,
            view,
            sampler,
            width,
            height,
        }
    }

    /// Upload directly from an [`Atlas`], consuming its mip pyramid.
    ///
    /// When the atlas already carries a pyramid (built by lodestone-assets with a
    /// vanilla-faithful linear-light mean, cutout `solidify`, and alpha-coverage
    /// preservation), those levels are uploaded verbatim via [`atlas_mip_levels`].
    /// Regenerating here would be wasteful and *wrong* — a naive box filter mixes
    /// gamma-encoded sRGB values and bleeds transparent black.
    #[must_use]
    pub fn from_atlas(device: &wgpu::Device, queue: &wgpu::Queue, atlas: &Atlas) -> Self {
        let mips = atlas_mip_levels(atlas);
        Self::upload_mips(
            device,
            queue,
            atlas.width,
            atlas.height,
            &mips,
            wgpu::FilterMode::Nearest,
        )
    }

    /// [`Self::from_atlas`] with the sampler **terrain** wants: `LINEAR`
    /// magnification as well as minification.
    ///
    /// Vanilla keeps two samplers over the same block atlas, and this is the
    /// second one: `TextureAtlas`'s own is `getClampToEdge(FilterMode.NEAREST)`,
    /// but `LevelRenderer` creates `chunkLayerSampler` as
    /// `createSampler(CLAMP_TO_EDGE, CLAMP_TO_EDGE, LINEAR, LINEAR,
    /// maxAnisotropy, empty)` and binds *that* as `Sampler0` for every chunk
    /// layer. So the mag filter is not a preference here, it is which of the
    /// two samplers a draw is using.
    ///
    /// It matters because `model.wgsl`'s default sampling path is now
    /// `sample_nearest`, and that function's whole mechanism is `snap_uv`:
    /// it moves the sample *within* a texel, rescaling the sub-texel offset by
    /// screen-pixels-per-texel and clamping, so a magnified surface gets point
    /// sampling with a one-pixel anti-aliased ramp at each texel edge. Against
    /// a `Nearest` sampler that whole rescale is a **no-op** — moving a
    /// coordinate inside a texel cannot change a point fetch — so the ramp
    /// vanishes and magnified terrain gets hard, aliased texel edges instead of
    /// vanilla's. Use this for anything the terrain pass samples; use
    /// [`Self::from_atlas`] for GUI, item, container and particle sheets, where
    /// `Nearest` magnification is what vanilla does and what keeps them crisp.
    #[must_use]
    pub fn from_atlas_terrain(device: &wgpu::Device, queue: &wgpu::Queue, atlas: &Atlas) -> Self {
        let mips = atlas_mip_levels(atlas);
        Self::upload_mips(
            device,
            queue,
            atlas.width,
            atlas.height,
            &mips,
            wgpu::FilterMode::Linear,
        )
    }
}

/// The mip levels to upload for an [`Atlas`] — **exactly the ones it carries**,
/// never more.
///
/// lodestone-assets already downsampled them vanilla-faithfully (linear-light
/// RGB mean, cutout `solidify`, alpha-coverage preservation, and a gutter
/// re-extruded from each sprite's own edge at every level), so they are
/// returned verbatim. An atlas that carries no pyramid uploads a **single**
/// level, which is vanilla's own arithmetic: `TextureAtlas.createTexture` asks
/// for `mipLevel + 1` levels, so `mipmapLevels = 0` is one level and no mip
/// chain at all.
///
/// This used to fall back to [`generate_isolated_mips`] over the *stitched*
/// image, and that was a shipped defect rather than a nicety. Two things go
/// wrong at once, and both were measured against the real `client.jar` block
/// atlas at `mipmapLevels = 0`:
///
/// * **The gutter is never written.** That generator allocates each level
///   zero-filled and then writes only the sprite rectangles, which carry no
///   padding — so every texel between sprites is transparent **black** from
///   level 1 down. The block atlas's sampler is `min_filter: Linear`, so a tap
///   at a face's own edge blends that in: alpha falls below `model.wgsl`'s
///   cutout threshold on an alpha-tested quad (a background-coloured pinprick
///   at a block edge, from a fully opaque sprite) and the colour is dragged
///   toward black on one that bypasses the test. The contaminated share of a
///   face is half a texel out of `16 >> level`, so it *grows* with distance —
///   3% at level 0, 25% at level 3, 50% at level 4 — and then stops growing
///   when the level clamps. Thin near, thicker further, then constant.
/// * **It invents levels the sprites cannot support.** A 2048-wide atlas asks
///   for 11 extra levels; by level 5 a 16x16 sprite is under one texel, sprites
///   collide in the destination and the deepest levels are an average of
///   unrelated textures. Measured beside `block/stone`: `0x00000000` at levels
///   1-4, then `2a766f`, `9b9c62`, and a flat `f7c526` at the top.
///
/// [`generate_isolated_mips`] stays for [`GpuAtlas::from_rgba`], whose callers
/// are synthetic atlases with no asset pyramid to consume.
#[must_use]
pub fn atlas_mip_levels(atlas: &Atlas) -> Vec<MipLevel> {
    (0..atlas.mip_count())
        .filter_map(|level| atlas.mip(level))
        .map(|m| MipLevel {
            width: m.width,
            height: m.height,
            rgba: m.rgba.to_vec(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vanilla block-atlas facts the task gave us, as a stats fixture.
    /// `total_frames` is modelled to exceed 2048 (a pure per-frame array does
    /// not fit) while the static 16×16 set stays under it.
    fn vanilla_stats() -> AtlasStats {
        AtlasStats {
            total_sprites: 1233,
            static_16: 1147, // ~93%
            total_frames: 2600,
            wide_sprites: 42,
        }
    }

    #[test]
    fn static_set_fits_but_full_frame_array_does_not() {
        let s = vanilla_stats();
        // The static 16×16 majority fits in 2048 layers.
        assert!(s.static_16 <= MEASURED_MAX_ARRAY_LAYERS);
        // But a pure per-frame array (all animation frames) does not, and there
        // are wide sprites that can't live in a 16×16 layer anyway.
        assert!(!layout_fits(s, MEASURED_MAX_ARRAY_LAYERS));
        assert!(array_layers_needed(s) > MEASURED_MAX_ARRAY_LAYERS);
    }

    #[test]
    fn recommendation_matches_reasoning() {
        let s = vanilla_stats();
        // Physical layout depends only on *fit*, never on bindless: a
        // texture_2d_array needs no bindless features, so gating it on bindless
        // was the conflation. Vanilla doesn't fit a pure array (2600 frames >
        // 2048) but its static majority does → Hybrid on a 2048-layer adapter.
        assert_eq!(
            recommend_layout(s, MEASURED_MAX_ARRAY_LAYERS),
            TextureLayout::Hybrid
        );
        // On the WebGPU guaranteed minimum (256), even the static majority
        // (1147) overflows the layer cap → the portable stitched atlas.
        assert_eq!(
            recommend_layout(s, GUARANTEED_MAX_ARRAY_LAYERS_WEBGPU),
            TextureLayout::Atlas2D
        );
        // A hypothetical small atlas that fits → clean array.
        let small = AtlasStats {
            total_sprites: 500,
            static_16: 500,
            total_frames: 500,
            wide_sprites: 0,
        };
        assert_eq!(
            recommend_layout(small, MEASURED_MAX_ARRAY_LAYERS),
            TextureLayout::TextureArray
        );
    }

    #[test]
    fn array_binding_model_needs_no_bindless_features() {
        // Web-like adapter: no binding array, no non-uniform indexing.
        let web = GpuCapabilities::baseline();
        assert!(!web.supports_bindless_atlas());
        // A texture_2d_array is still fully available: the array layout binds as
        // Texture2DArray, NOT as a degraded fallback.
        assert_eq!(
            select_binding_model(&web, TextureLayout::TextureArray),
            AtlasBindingModel::Texture2DArray
        );
        assert_eq!(
            select_binding_model(&web, TextureLayout::Hybrid),
            AtlasBindingModel::Texture2DArray
        );
        // A 2D atlas always binds as a single texture.
        assert_eq!(
            select_binding_model(&web, TextureLayout::Atlas2D),
            AtlasBindingModel::SingleTexture2D
        );
    }

    #[test]
    fn bindless_only_selected_when_supported() {
        let mut caps = GpuCapabilities::baseline();
        caps.texture_binding_array = true;
        caps.nonuniform_binding_array_indexing = true;
        assert!(caps.supports_bindless_atlas());
        // With bindless available, an array layout may use it as an upgrade.
        assert_eq!(
            select_binding_model(&caps, TextureLayout::Hybrid),
            AtlasBindingModel::Bindless
        );
        // But a plain 2D atlas never needs it.
        assert_eq!(
            select_binding_model(&caps, TextureLayout::Atlas2D),
            AtlasBindingModel::SingleTexture2D
        );
    }

    #[test]
    fn webgpu_measured_caps_are_not_bindless() {
        // Fix #4 audit: unlike the multi-draw probe (which read a *downlevel
        // flag* and was optimistic), the bindless probe reads the real
        // `TEXTURE_BINDING_ARRAY` / non-uniform-indexing feature bits, which
        // WebGPU and WebGL2 genuinely do not advertise. So a measured WebGPU
        // adapter (indirect_first_instance present, everything native absent)
        // must report no bindless and take the portable texture_2d_array path.
        let webgpu = GpuCapabilities {
            indirect_execution: true,
            indirect_first_instance: true,
            multi_draw_indirect_count: false,
            texture_binding_array: false,
            nonuniform_binding_array_indexing: false,
            ..GpuCapabilities::baseline()
        };
        assert!(!webgpu.supports_bindless_atlas());
        assert_eq!(
            select_binding_model(&webgpu, TextureLayout::TextureArray),
            AtlasBindingModel::Texture2DArray
        );
    }

    #[test]
    fn webgpu_guaranteed_layers_is_the_spec_minimum() {
        // The WebGPU spec's default maxTextureArrayLayers is 256; a portable
        // renderer must assume no more without querying.
        assert_eq!(GUARANTEED_MAX_ARRAY_LAYERS_WEBGPU, 256);
        const { assert!(GUARANTEED_MAX_ARRAY_LAYERS_WEBGPU < MEASURED_MAX_ARRAY_LAYERS) };
    }

    #[test]
    fn vram_is_a_couple_of_mb_either_way() {
        // 16×16 array of 2048 layers vs a 2D atlas holding ~the same pixels.
        let array = mip_pyramid_bytes(16, 16, 2048);
        let atlas = mip_pyramid_bytes(1024, 512, 1); // 2048 sprites' worth
        // Both are single-digit MB; neither dominates the decision.
        assert!(array < 4 * 1024 * 1024, "array {array}");
        assert!(atlas < 4 * 1024 * 1024, "atlas {atlas}");
    }

    #[test]
    fn mip_level_count_is_log2_plus_one() {
        assert_eq!(mip_level_count(16, 16), 5); // 16,8,4,2,1
        assert_eq!(mip_level_count(1024, 512), 11);
        assert_eq!(mip_level_count(1, 1), 1);
    }

    #[test]
    fn isolated_mips_do_not_bleed_across_sprites() {
        // 4×2 atlas: sprite A (red) at x0..2, sprite B (blue) at x2..4.
        let width = 4;
        let height = 2;
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                if x < 2 {
                    rgba[idx] = 255; // red
                    rgba[idx + 3] = 255;
                } else {
                    rgba[idx + 2] = 255; // blue
                    rgba[idx + 3] = 255;
                }
            }
        }
        let sprites = [
            SpriteRect {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
            },
            SpriteRect {
                x: 2,
                y: 0,
                w: 2,
                h: 2,
            },
        ];
        let mips = generate_isolated_mips(
            width,
            height,
            &rgba,
            &sprites,
            mip_level_count(width, height),
        );
        // Level 1 is 2×1: pixel 0 belongs to A (pure red), pixel 1 to B (pure blue).
        let l1 = &mips[1];
        assert_eq!(l1.width, 2);
        assert_eq!(l1.height, 1);
        assert_eq!(
            &l1.rgba[0..4],
            &[255, 0, 0, 255],
            "A mip must stay pure red"
        );
        assert_eq!(
            &l1.rgba[4..8],
            &[0, 0, 255, 255],
            "B mip must stay pure blue"
        );
        // A naive whole-image box filter would have produced (127,0,127,255).
    }

    #[test]
    fn mip_chain_has_expected_levels_and_sizes() {
        let width = 8;
        let height = 8;
        let rgba = vec![200u8; (width * height * 4) as usize];
        let sprites = [SpriteRect {
            x: 0,
            y: 0,
            w: 8,
            h: 8,
        }];
        let mips = generate_isolated_mips(
            width,
            height,
            &rgba,
            &sprites,
            mip_level_count(width, height),
        );
        assert_eq!(mips.len(), 4); // 8,4,2,1
        assert_eq!((mips[0].width, mips[0].height), (8, 8));
        assert_eq!((mips[3].width, mips[3].height), (1, 1));
        // A uniform sprite stays uniform through every level.
        assert!(mips[3].rgba.iter().take(3).all(|&c| c == 200));
    }

    #[test]
    fn isolated_mips_handle_non_power_of_two_atlas_width() {
        // 160×16 strip: 10 sprites of 16×16 side by side. Width is not a power of
        // two, so the high mip levels shrink the *atlas* below the sprite grid
        // spacing (level 6 atlas is 2px wide while sprite 9 shifts to column 2).
        // That is the exact shape that panicked with an out-of-bounds write when
        // the sprite origin was shifted without being clamped to the level.
        let width = 160;
        let height = 16;
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for s in 0..10u32 {
            for y in 0..16u32 {
                for x in 0..16u32 {
                    let gx = s * 16 + x;
                    let idx = ((y * width + gx) * 4) as usize;
                    rgba[idx] = (s * 25) as u8; // distinct per-sprite colour
                    rgba[idx + 1] = 255 - (s * 25) as u8;
                    rgba[idx + 3] = 255;
                }
            }
        }
        let sprites: Vec<SpriteRect> = (0..10u32)
            .map(|s| SpriteRect {
                x: s * 16,
                y: 0,
                w: 16,
                h: 16,
            })
            .collect();
        let levels = mip_level_count(width, height);

        // Must not panic on a non-pow2 atlas with multiple sprites.
        let mips = generate_isolated_mips(width, height, &rgba, &sprites, levels);

        // Every level's buffer is exactly (width>>level) × (height>>level), each
        // clamped to ≥1, and fully sized — no OOB, no short buffer.
        for (level, m) in mips.iter().enumerate() {
            let lw = (width >> level).max(1);
            let lh = (height >> level).max(1);
            assert_eq!((m.width, m.height), (lw, lh), "level {level} size");
            assert_eq!(
                m.rgba.len(),
                (lw * lh * 4) as usize,
                "level {level} buffer length"
            );
        }
        // Low levels stay isolated: sprite 0's mip-1 texel is its own colour, not
        // bled from sprite 1.
        assert_eq!(
            &mips[1].rgba[0..4],
            &[0, 255, 0, 255],
            "sprite 0 keeps its own colour at mip level 1"
        );
    }

    /// The live path (`from_atlas`) must **consume** the mip pyramid
    /// lodestone-assets already built — a vanilla-faithful linear-light mean with
    /// cutout `solidify` and alpha-coverage preservation — rather than
    /// regenerating a naive sRGB box filter here. Regenerating is both wasteful
    /// and *wrong*: it mixes gamma-encoded values and bleeds transparent black.
    #[test]
    fn atlas_mip_levels_consumes_asset_pyramid() {
        use lodestone_assets::{AtlasBuilder, Image, ResourceLocation};

        // A 16×16 checkerboard of black/white *opaque* texels. Averaging two such
        // texels in sRGB space (~127) is far from the linear-light mean (~188),
        // so a regenerated mip is observably different from the asset's.
        let mut rgba = Vec::with_capacity(16 * 16 * 4);
        for y in 0..16u32 {
            for x in 0..16u32 {
                let v = if (x + y) % 2 == 0 { 0u8 } else { 255u8 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let mut builder = AtlasBuilder::new().with_width(64).with_mip_levels(1);
        builder.add_texture(
            ResourceLocation::parse("minecraft:block/checker").unwrap(),
            Image {
                width: 16,
                height: 16,
                rgba,
            },
            None,
        );
        let atlas = builder.build().expect("atlas builds");
        assert!(
            atlas.mip_count() >= 2,
            "fixture must carry a real pyramid, got {} levels",
            atlas.mip_count()
        );

        let chosen = atlas_mip_levels(&atlas);

        // Every level is the asset's own bytes, verbatim.
        assert_eq!(chosen.len(), atlas.mip_count() as usize, "level count");
        for (level, m) in chosen.iter().enumerate() {
            let want = atlas.mip(level as u32).expect("asset level present");
            assert_eq!(
                (m.width, m.height),
                (want.width, want.height),
                "level {level} size"
            );
            assert_eq!(
                m.rgba, want.rgba,
                "level {level} pixels are the asset's own"
            );
        }

        // Guard against a silent regression to regeneration: a naive box filter
        // over the same bytes yields a *different* level 1, so equality above can
        // only hold if we truly consumed the asset's pyramid.
        let regen = generate_isolated_mips(
            atlas.width,
            atlas.height,
            &atlas.rgba,
            &[SpriteRect {
                x: 0,
                y: 0,
                w: atlas.width,
                h: atlas.height,
            }],
            atlas.mip_count(),
        );
        let asset_l1 = atlas.mip(1).expect("asset level 1");
        assert_ne!(
            regen[1].rgba, asset_l1.rgba,
            "regeneration must differ from the asset pyramid, else the guard is vacuous"
        );
    }
}
