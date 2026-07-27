//! Deterministic CPU-side texture-atlas stitching.
//!
//! This module turns decoded [`Image`]s (and their optional animation metadata)
//! into a single packed atlas plus per-sprite placement and animation info. It
//! is deliberately **GPU-free**: the output is plain RGBA8 bytes and layout
//! metadata that a renderer crate uploads however it likes. Keeping it GPU-free
//! means the atlas can be built and tested headlessly and reused by non-visual
//! consumers (for example a bot).
//!
//! # Determinism
//!
//! For a given input set the atlas bytes and every UV are byte-identical across
//! runs: sprites are placed in a fixed, sorted order (never a `HashMap`
//! iteration order) and the backing buffer is zero-initialised.
//!
//! # Layout: 2D atlas vs. texture array
//!
//! The concrete output here is a single 2D atlas (`layers == 1`) with real UV
//! rectangles. Each [`AtlasSprite`] also carries a `layer` field so the same
//! structure can describe a texture-array layout in future without an API break
//! — see the crate-level docs for the recommendation.
//!
//! # Animations
//!
//! An animated texture is a vertical strip of equally sized frames. The whole
//! strip is placed as one region; the sprite records the physical `frame_count`,
//! the `frame_height`, and the playback order, so frames stay individually
//! addressable (via [`AtlasSprite::frame_pixel_rect`]) rather than being
//! flattened to frame zero.
//!
//! # Interpolation and the atlas upload strategy
//!
//! Because every physical frame is retained in the atlas as part of one region,
//! animation — **including `interpolate: true`** — needs no atlas mutation at
//! runtime. The renderer advances an animated sprite purely by choosing which
//! frame sub-rect to sample ([`AtlasSprite::frame_uv`]); interpolation is a
//! sample-time blend between the current frame `N` and the next frame `N+1`
//! (both already resident in the atlas), done in the shader. This deliberately
//! keeps the atlas **immutable after bake**: there is no per-tick full-atlas
//! re-upload, and animated sprites live in the *same* atlas as static ones with
//! no separate dynamically-updated region and no risk of a seam. The cost is a
//! little extra atlas area (every frame is stored), which is negligible versus
//! streaming uploads. The sprite exposes `frame_count`, per-frame `frames`
//! (each carrying its own `time`), and `interpolate` so the renderer has
//! everything it needs to drive this without touching texture memory.

use crate::error::AtlasError;
use crate::location::ResourceLocation;
use crate::manager::ResourceManager;
use crate::mipmap::{MipStrategy, generate_mip_levels, max_mip_level};
use crate::texture::{AnimationFrame, Image, TextureMeta};
use std::collections::HashMap;

/// The result of resolving an animation tick: the physical frames to sample and
/// the blend between them. See [`AtlasSprite::frame_at_tick`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteFrameSample {
    /// The physical frame index currently displayed.
    pub current: u32,
    /// The physical frame index blended toward (the next slot's frame, wrapping
    /// at the end of the loop). Equal to `current` when there is nothing to
    /// blend to.
    pub next: u32,
    /// Interpolation factor in `[0, 1)` from `current` toward `next`, meaningful
    /// only when [`AtlasSprite::interpolate`] is set.
    pub blend: f32,
}

/// A texture placed within an [`Atlas`].
#[derive(Debug, Clone, PartialEq)]
pub struct AtlasSprite {
    /// The texture's resource location (for example `minecraft:block/stone`).
    pub location: ResourceLocation,
    /// The atlas layer this sprite lives on. Always `0` for the single-atlas
    /// layout; reserved for a future texture-array layout.
    pub layer: u32,
    /// Left edge of the placed region, in atlas pixels.
    pub x: u32,
    /// Top edge of the placed region, in atlas pixels.
    pub y: u32,
    /// Width of the placed region, in atlas pixels.
    pub width: u32,
    /// Height of the placed region (the whole strip for animations), in pixels.
    pub height: u32,
    /// Top-left UV of the region, normalised to `[0, 1]`.
    pub uv_min: [f32; 2],
    /// Bottom-right UV of the region, normalised to `[0, 1]`.
    pub uv_max: [f32; 2],
    /// Number of physical frames stacked in the strip (`1` when static).
    pub frame_count: u32,
    /// Height of a single frame, in pixels (`== height` when static).
    pub frame_height: u32,
    /// Default frame duration in ticks.
    pub frametime: u32,
    /// Whether the renderer should interpolate between frames.
    pub interpolate: bool,
    /// Playback order over the physical frames. For a static sprite this is a
    /// single entry `{ index: 0 }`.
    pub frames: Vec<AnimationFrame>,
}

impl AtlasSprite {
    /// Whether this sprite is animated (has more than one physical frame).
    pub fn is_animated(&self) -> bool {
        self.frame_count > 1
    }

    /// Returns the pixel rectangle `[x, y, width, height]` of physical frame
    /// `index` within the atlas, or `None` if the index is out of range.
    pub fn frame_pixel_rect(&self, index: u32) -> Option<[u32; 4]> {
        if index >= self.frame_count {
            return None;
        }
        Some([
            self.x,
            self.y + index * self.frame_height,
            self.width,
            self.frame_height,
        ])
    }

    /// Returns the normalised UV rectangle `(uv_min, uv_max)` of physical frame
    /// `index`, given the dimensions of the atlas it belongs to.
    pub fn frame_uv(
        &self,
        index: u32,
        atlas_width: u32,
        atlas_height: u32,
    ) -> Option<([f32; 2], [f32; 2])> {
        let [x, y, w, h] = self.frame_pixel_rect(index)?;
        let aw = atlas_width as f32;
        let ah = atlas_height as f32;
        Some((
            [x as f32 / aw, y as f32 / ah],
            [(x + w) as f32 / aw, (y + h) as f32 / ah],
        ))
    }

    /// Duration in ticks of playback slot `slot`: its explicit per-frame `time`,
    /// or the sprite's default `frametime`.
    fn slot_time(&self, slot: usize) -> u32 {
        self.frames
            .get(slot)
            .and_then(|f| f.time)
            .unwrap_or(self.frametime)
            .max(1)
    }

    /// Total length in ticks of one full animation loop (the sum of every
    /// playback slot's duration). For a static sprite this is just `frametime`.
    pub fn cycle_ticks(&self) -> u32 {
        (0..self.frames.len())
            .map(|s| self.slot_time(s))
            .sum::<u32>()
            .max(1)
    }

    /// Resolves an absolute `tick` count to the physical frames to sample and the
    /// interpolation blend between them, mirroring vanilla's
    /// `SpriteContents.AnimationState`.
    ///
    /// This is the draw-time animation seam: the renderer holds the clock and,
    /// each frame, calls this with the current tick to learn which two atlas
    /// sub-rects to sample ([`frame_uv`](Self::frame_uv) with `current`/`next`)
    /// and — when [`interpolate`](Self::interpolate) is set — how far to blend
    /// between them. No texture memory is mutated: both frames are already
    /// resident in the immutable atlas.
    pub fn frame_at_tick(&self, tick: u64) -> SpriteFrameSample {
        let len = self.frames.len();
        if len == 0 {
            return SpriteFrameSample {
                current: 0,
                next: 0,
                blend: 0.0,
            };
        }
        let cycle = self.cycle_ticks() as u64;
        let mut t = tick % cycle;
        let mut slot = 0usize;
        while slot < len {
            let d = self.slot_time(slot) as u64;
            if t < d {
                break;
            }
            t -= d;
            slot += 1;
        }
        if slot >= len {
            slot = len - 1;
        }
        let current = self.frames[slot].index;
        let next = self.frames[(slot + 1) % len].index;
        let blend = t as f32 / self.slot_time(slot) as f32;
        SpriteFrameSample {
            current,
            next,
            blend,
        }
    }
}

/// A stitched, CPU-side texture atlas.
#[derive(Debug, Clone)]
pub struct Atlas {
    /// Atlas width in pixels.
    pub width: u32,
    /// Atlas height in pixels.
    pub height: u32,
    /// Number of layers (always `1` for the single-atlas layout).
    pub layers: u32,
    /// RGBA8 pixel data, `width * height * 4` bytes (per layer). This is mip
    /// level 0.
    pub rgba: Vec<u8>,
    sprites: Vec<AtlasSprite>,
    index: HashMap<ResourceLocation, usize>,
    /// Mip levels **1..=n** (level 0 is [`rgba`](Atlas::rgba)). Empty when no
    /// mips were requested.
    mips: Vec<MipLevel>,
    /// Present only when the requested mip depth was reduced by an
    /// awkwardly-sized sprite (see [`MipCap`]).
    mip_cap: Option<MipCap>,
}

/// Diagnostic emitted when a sprite's dimensions force the whole atlas to fewer
/// mip levels than were requested.
///
/// This mirrors vanilla's `SpriteLoader`, which logs *"Texture ... with size
/// ... limits mip level from ... to ..."* for the offending sprite. A silent cap
/// otherwise resurfaces weeks later as "why are my distant textures blurry", so
/// the builder surfaces the offender rather than swallowing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MipCap {
    /// Mip levels requested via [`AtlasBuilder::with_mip_levels`].
    pub requested: u32,
    /// Mip levels actually generated (equals [`Atlas::mip_count`] minus one).
    pub effective: u32,
    /// The sprites sitting at the limiting level — those whose smaller dimension
    /// has the fewest trailing zeros. Sorted by location for determinism.
    pub limiting_sprites: Vec<ResourceLocation>,
}

/// One owned mip level's pixels (levels `>= 1`).
#[derive(Debug, Clone)]
struct MipLevel {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// A borrowed view of one atlas mip level, including level 0.
#[derive(Debug, Clone, Copy)]
pub struct MipLevelRef<'a> {
    /// Level width in pixels.
    pub width: u32,
    /// Level height in pixels.
    pub height: u32,
    /// RGBA8 pixels, `width * height * 4` bytes.
    pub rgba: &'a [u8],
}

impl Atlas {
    /// All sprites, in a deterministic order (sorted by location).
    pub fn sprites(&self) -> &[AtlasSprite] {
        &self.sprites
    }

    /// Looks up a sprite by its resource location.
    pub fn sprite(&self, location: &ResourceLocation) -> Option<&AtlasSprite> {
        self.index.get(location).map(|&i| &self.sprites[i])
    }

    /// The number of mip levels including level 0 (so `1` when no mips were
    /// generated). This is the count the renderer sizes its texture allocation
    /// against.
    pub fn mip_count(&self) -> u32 {
        1 + self.mips.len() as u32
    }

    /// Borrows a mip level's pixels (`level == 0` is the base atlas), or `None`
    /// if the level is out of range.
    pub fn mip(&self, level: u32) -> Option<MipLevelRef<'_>> {
        if level == 0 {
            Some(MipLevelRef {
                width: self.width,
                height: self.height,
                rgba: &self.rgba,
            })
        } else {
            self.mips.get(level as usize - 1).map(|m| MipLevelRef {
                width: m.width,
                height: m.height,
                rgba: &m.rgba,
            })
        }
    }

    /// The mip-cap diagnostic, present only when an awkwardly-sized sprite forced
    /// the atlas below the requested mip depth (vanilla-faithful behaviour — see
    /// [`MipCap`]). `None` when the full requested depth was reached.
    pub fn mip_cap(&self) -> Option<&MipCap> {
        self.mip_cap.as_ref()
    }
}

/// An input texture staged for stitching.
#[derive(Debug, Clone)]
struct Input {
    location: ResourceLocation,
    image: Image,
    meta: Option<TextureMeta>,
}

/// Builds an [`Atlas`] from a set of decoded textures.
///
/// Add textures with [`add_texture`](Self::add_texture) or load them from a
/// [`ResourceManager`] with [`load`](Self::load), then call
/// [`build`](Self::build).
#[derive(Debug, Default)]
pub struct AtlasBuilder {
    inputs: Vec<Input>,
    seen: HashMap<ResourceLocation, usize>,
    forced_width: Option<u32>,
    padding: u32,
    mip_levels: u32,
}

impl AtlasBuilder {
    /// Creates an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forces the atlas width (in pixels) instead of choosing one automatically.
    /// The width is still raised to at least the widest sprite. Mainly useful
    /// for deterministic tests.
    pub fn with_width(mut self, width: u32) -> Self {
        self.forced_width = Some(width);
        self
    }

    /// Sets a gutter of `padding` pixels around every sprite, filled by
    /// extruding each sprite's edge pixels outward (edge-clamp).
    ///
    /// This is how vanilla 26.2 prevents mip bleed: `TextureAtlasSprite` computes
    /// its UV rect from `(x + padding)` (see the decompiled source), so the sprite
    /// occupies the *interior* of its cell and box-filter mip levels sample the
    /// replicated gutter — same-sprite pixels — instead of the neighbouring
    /// sprite. It replaces the older per-quad `uvShrinkRatio` inset, which no
    /// longer exists in 26.2. Prefer this over [`BakeOptions::uv_inset_texels`]
    /// when the renderer generates mips: it is size-correct for mixed sprite
    /// resolutions and keeps a sprite's full texel range addressable. A padding
    /// of `1 << max_mip_level` fully contains the deepest mip a renderer samples.
    pub fn with_padding(mut self, padding: u32) -> Self {
        self.padding = padding;
        self
    }

    /// Requests a mip pyramid of up to `levels` extra levels (so the atlas ends
    /// up with `levels + 1` levels including the base).
    ///
    /// Mips are generated **per sprite** with vanilla's algorithm ([`mipmap`]:
    /// linear-light mean, cutout `solidify`, alpha-coverage preservation) and
    /// composited into aligned atlas mip levels, exactly as vanilla's
    /// `SpriteContents`/atlas do — never by box-filtering the stitched atlas,
    /// which would bleed across sprite boundaries. The effective level count is
    /// capped by the smallest sprite (a sprite dimension must be divisible by
    /// `2^level`), and sprite cells are aligned so `(x >> L, y >> L)` lands each
    /// sprite's own downsample with no neighbour mixing.
    ///
    /// Requesting mips also forces a padding of at least `1 << levels` so the
    /// aligned layout has room; a larger [`with_padding`](Self::with_padding)
    /// value is respected.
    pub fn with_mip_levels(mut self, levels: u32) -> Self {
        self.mip_levels = levels;
        self
    }

    /// Adds a decoded texture and its optional animation metadata. A later add
    /// with the same location replaces the earlier one (last wins), mirroring
    /// pack-override semantics.
    pub fn add_texture(
        &mut self,
        location: ResourceLocation,
        image: Image,
        meta: Option<TextureMeta>,
    ) -> &mut Self {
        let input = Input {
            location: location.clone(),
            image,
            meta,
        };
        if let Some(&i) = self.seen.get(&location) {
            self.inputs[i] = input;
        } else {
            self.seen.insert(location, self.inputs.len());
            self.inputs.push(input);
        }
        self
    }

    /// Loads a texture (and its sibling `*.png.mcmeta`, if any) from a resource
    /// manager and stages it. `location` is a texture location such as
    /// `minecraft:block/stone`.
    pub fn load(
        &mut self,
        manager: &ResourceManager,
        location: &ResourceLocation,
    ) -> Result<&mut Self, AtlasError> {
        let png = manager
            .read_asset(location, "textures", "png")
            .ok_or_else(|| AtlasError::TextureMissing {
                location: location.to_string(),
            })?;
        let image = Image::decode_png(&png).map_err(|source| AtlasError::Texture {
            location: location.to_string(),
            source,
        })?;
        let meta_path = format!(
            "{}.mcmeta",
            ResourceManager::asset_path(location, "textures", "png")
        );
        let meta = match manager.read(&meta_path) {
            Some(bytes) => {
                Some(
                    TextureMeta::parse(&bytes).map_err(|source| AtlasError::Texture {
                        location: location.to_string(),
                        source,
                    })?,
                )
            }
            None => None,
        };
        self.add_texture(location.clone(), image, meta);
        Ok(self)
    }

    /// Stitches all staged textures into a single atlas.
    ///
    /// Fails with [`AtlasError::Empty`] if nothing was staged, or
    /// [`AtlasError::BadAnimationStrip`] if an animated texture's frame height
    /// does not divide its image height.
    pub fn build(self) -> Result<Atlas, AtlasError> {
        if self.inputs.is_empty() {
            return Err(AtlasError::Empty);
        }

        // Derive per-sprite frame geometry up front (may fail).
        struct Staged {
            input: Input,
            frame_count: u32,
            frame_height: u32,
            frametime: u32,
            interpolate: bool,
            frames: Vec<AnimationFrame>,
        }
        let mut staged: Vec<Staged> = Vec::with_capacity(self.inputs.len());
        for input in self.inputs {
            let (frame_count, frame_height, frametime, interpolate, frames) =
                frame_geometry(&input)?;
            staged.push(Staged {
                input,
                frame_count,
                frame_height,
                frametime,
                interpolate,
                frames,
            });
        }

        // Deterministic placement order: tallest first (good shelves), then
        // widest, then by location name to break ties reproducibly.
        staged.sort_by(|a, b| {
            b.input
                .image
                .height
                .cmp(&a.input.image.height)
                .then(b.input.image.width.cmp(&a.input.image.width))
                .then(
                    a.input
                        .location
                        .to_string()
                        .cmp(&b.input.location.to_string()),
                )
        });

        let max_width = staged
            .iter()
            .map(|s| s.input.image.width)
            .max()
            .unwrap_or(1);

        // The effective mip level count is capped by the smallest sprite: a
        // sprite dimension must be divisible by `2^level` for `(x >> L)` to place
        // its downsample exactly. `gran` is the alignment granularity (1 when no
        // mips are requested, so the layout is unchanged).
        //
        // This whole-atlas cap is deliberately vanilla-faithful, not a
        // limitation: `SpriteLoader.stitch` computes
        // `lowestOneBit = min over sprites of min(lowestOneBit(w), lowestOneBit(h))`
        // and, if `log2(lowestOneBit) < requested`, drops the *entire* atlas to
        // that level (logging a warning per offending texture). Because
        // `2^trailing_zeros(x) <= x`, vanilla's parallel `minTexelSize` term never
        // binds, so this reduces exactly to `min over sprites of max_mip_level`.
        // A single NPOT or odd-sized sprite therefore caps mips for the whole
        // sheet in vanilla too; matching that is correct, and changing it would
        // silently diverge from the real game.
        let effective_levels = if self.mip_levels == 0 {
            0
        } else {
            let cap = staged
                .iter()
                .map(|s| max_mip_level(s.input.image.width, s.input.image.height))
                .min()
                .unwrap_or(0);
            self.mip_levels.min(cap)
        };
        let gran = 1u32 << effective_levels;

        // If an awkward sprite reduced the depth, name the offender(s) — those
        // sitting exactly at the limiting level — the way vanilla logs it.
        let mip_cap = if self.mip_levels > 0 && effective_levels < self.mip_levels {
            let mut limiting_sprites: Vec<ResourceLocation> = staged
                .iter()
                .filter(|s| {
                    max_mip_level(s.input.image.width, s.input.image.height) == effective_levels
                })
                .map(|s| s.input.location.clone())
                .collect();
            limiting_sprites.sort();
            Some(MipCap {
                requested: self.mip_levels,
                effective: effective_levels,
                limiting_sprites,
            })
        } else {
            None
        };

        // Requesting mips only requires sprite origins to be aligned to `gran`
        // (so `origin >> L` places each sprite's own downsample exactly). Padding
        // is therefore *not* forced to a minimum; it defaults to 0 (footprints
        // tile exactly — correct for NEAREST sampling + per-sprite mips). An
        // explicit gutter is rounded up to a whole granule so origins stay
        // aligned; vanilla uses a full `2^mipLevel` gutter for linear/anisotropic
        // filtering, which this supports via `with_padding(1 << levels)`.
        let pad = if effective_levels > 0 {
            self.padding.next_multiple_of(gran)
        } else {
            self.padding
        };
        let cell_extra = pad * 2;
        let total_area: u64 = staged
            .iter()
            .map(|s| {
                (align_up(s.input.image.width + cell_extra, gran) as u64)
                    * (align_up(s.input.image.height + cell_extra, gran) as u64)
            })
            .sum();
        let width = match self.forced_width {
            Some(w) => next_pow2(w.max(max_width + cell_extra)),
            None => next_pow2((max_width + cell_extra).max(isqrt(total_area))),
        };

        // Shelf-pack to determine positions and the total height. Each cell
        // reserves `pad` pixels of gutter on every side; cell sizes are aligned to
        // `gran` so every placed sprite origin is a multiple of `gran`.
        let mut placements: Vec<(usize, u32, u32)> = Vec::with_capacity(staged.len());
        let mut cursor_x = 0u32;
        let mut cursor_y = 0u32;
        let mut shelf_height = 0u32;
        for (i, s) in staged.iter().enumerate() {
            let w = align_up(s.input.image.width + cell_extra, gran);
            let h = align_up(s.input.image.height + cell_extra, gran);
            if cursor_x + w > width {
                cursor_y += shelf_height;
                cursor_x = 0;
                shelf_height = 0;
            }
            placements.push((i, cursor_x, cursor_y));
            cursor_x += w;
            shelf_height = shelf_height.max(h);
        }
        let height = next_pow2(cursor_y + shelf_height).max(gran);

        // Blit into the backing buffer. The sprite pixels land at `+pad`, and the
        // surrounding gutter is filled by extruding the sprite's edge pixels.
        let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];
        for &(i, x, y) in &placements {
            blit(&mut rgba, width, &staged[i].input.image, x + pad, y + pad);
            if pad > 0 {
                extrude_border(
                    &mut rgba,
                    width,
                    &staged[i].input.image,
                    x + pad,
                    y + pad,
                    pad,
                );
            }
        }

        // Build sprites (in placement order first), then sort by location for a
        // deterministic public ordering.
        let mut sprites: Vec<AtlasSprite> = placements
            .iter()
            .map(|&(i, cell_x, cell_y)| {
                let s = &staged[i];
                let w = s.input.image.width;
                let h = s.input.image.height;
                let x = cell_x + pad;
                let y = cell_y + pad;
                AtlasSprite {
                    location: s.input.location.clone(),
                    layer: 0,
                    x,
                    y,
                    width: w,
                    height: h,
                    uv_min: [x as f32 / width as f32, y as f32 / height as f32],
                    uv_max: [
                        (x + w) as f32 / width as f32,
                        (y + h) as f32 / height as f32,
                    ],
                    frame_count: s.frame_count,
                    frame_height: s.frame_height,
                    frametime: s.frametime,
                    interpolate: s.interpolate,
                    frames: s.frames.clone(),
                }
            })
            .collect();
        sprites.sort_by_key(|s| s.location.to_string());

        let index = sprites
            .iter()
            .enumerate()
            .map(|(i, s)| (s.location.clone(), i))
            .collect();

        // Per-sprite mip generation, composited into aligned atlas mip levels.
        // Each sprite's full region (the whole animation strip for animated
        // sprites) is mipped independently with vanilla's algorithm and blitted
        // at `(origin >> level)`, so a level never averages across sprites.
        let mips = if effective_levels > 0 {
            let chains: Vec<Vec<Image>> = placements
                .iter()
                .map(|&(i, _, _)| {
                    generate_mip_levels(
                        &staged[i].input.image,
                        effective_levels,
                        MipStrategy::Auto,
                        0.0,
                    )
                })
                .collect();
            let mut levels = Vec::with_capacity(effective_levels as usize);
            for level in 1..=effective_levels {
                let lw = width >> level;
                let lh = height >> level;
                let mut buf = vec![0u8; (lw as usize) * (lh as usize) * 4];
                for (p, &(_, cell_x, cell_y)) in placements.iter().enumerate() {
                    let sx = (cell_x + pad) >> level;
                    let sy = (cell_y + pad) >> level;
                    blit(&mut buf, lw, &chains[p][level as usize], sx, sy);
                }
                levels.push(MipLevel {
                    width: lw,
                    height: lh,
                    rgba: buf,
                });
            }
            levels
        } else {
            Vec::new()
        };

        Ok(Atlas {
            width,
            height,
            layers: 1,
            rgba,
            sprites,
            index,
            mips,
            mip_cap,
        })
    }
}

/// Rounds `n` up to the nearest multiple of `align` (a power of two `>= 1`).
fn align_up(n: u32, align: u32) -> u32 {
    if align <= 1 {
        n
    } else {
        n.next_multiple_of(align)
    }
}

/// Computes `(frame_count, frame_height, frametime, interpolate, frames)` for an
/// input, deriving animation geometry from its metadata and image.
#[allow(clippy::type_complexity)]
fn frame_geometry(input: &Input) -> Result<(u32, u32, u32, bool, Vec<AnimationFrame>), AtlasError> {
    let img_h = input.image.height;
    let img_w = input.image.width;
    match input.meta.as_ref().and_then(|m| m.animation.as_ref()) {
        None => Ok((
            1,
            img_h,
            1,
            false,
            vec![AnimationFrame {
                index: 0,
                time: None,
            }],
        )),
        Some(anim) => {
            // Frame height defaults to the frame width, which defaults to the
            // image width (vanilla: square frames stacked vertically).
            let frame_height = anim
                .frame_height
                .unwrap_or(anim.frame_width.unwrap_or(img_w));
            if frame_height == 0 || !img_h.is_multiple_of(frame_height) {
                return Err(AtlasError::BadAnimationStrip {
                    location: input.location.to_string(),
                    frame_height,
                    image_height: img_h,
                });
            }
            let frame_count = img_h / frame_height;
            let frames = if anim.frames.is_empty() {
                (0..frame_count)
                    .map(|index| AnimationFrame { index, time: None })
                    .collect()
            } else {
                anim.frames.clone()
            };
            Ok((
                frame_count,
                frame_height,
                anim.frametime,
                anim.interpolate,
                frames,
            ))
        }
    }
}

/// Copies an image's pixels into the atlas buffer at `(x, y)`.
fn blit(atlas: &mut [u8], atlas_width: u32, image: &Image, x: u32, y: u32) {
    let aw = atlas_width as usize;
    let iw = image.width as usize;
    for row in 0..image.height as usize {
        let src = row * iw * 4;
        let dst = ((y as usize + row) * aw + x as usize) * 4;
        atlas[dst..dst + iw * 4].copy_from_slice(&image.rgba[src..src + iw * 4]);
    }
}

/// Fills a `pad`-pixel gutter around a sprite blitted at `(x, y)` by clamping to
/// its edge pixels (edge extension). This makes box-filter mip levels sample the
/// sprite's own border instead of the neighbouring sprite, preventing bleed —
/// the same effect vanilla 26.2 achieves with atlas padding.
fn extrude_border(atlas: &mut [u8], atlas_width: u32, image: &Image, x: u32, y: u32, pad: u32) {
    let aw = atlas_width as i64;
    let iw = image.width as i64;
    let ih = image.height as i64;
    let ox = x as i64;
    let oy = y as i64;
    let pad = pad as i64;
    // Walk the whole padded cell; interior sprite pixels are already blitted.
    for dy in -pad..(ih + pad) {
        for dx in -pad..(iw + pad) {
            if dx >= 0 && dx < iw && dy >= 0 && dy < ih {
                continue;
            }
            let cx = dx.clamp(0, iw - 1);
            let cy = dy.clamp(0, ih - 1);
            let src = ((cy * iw + cx) * 4) as usize;
            let dst = (((oy + dy) * aw + (ox + dx)) * 4) as usize;
            let px = [
                image.rgba[src],
                image.rgba[src + 1],
                image.rgba[src + 2],
                image.rgba[src + 3],
            ];
            atlas[dst..dst + 4].copy_from_slice(&px);
        }
    }
}

/// Smallest power of two `>= n` (with a floor of 1).
fn next_pow2(n: u32) -> u32 {
    if n <= 1 {
        1
    } else {
        1u32 << (32 - (n - 1).leading_zeros())
    }
}

/// Integer square root, rounded up, as a `u32`.
fn isqrt(n: u64) -> u32 {
    if n == 0 {
        return 0;
    }
    let mut x = (n as f64).sqrt() as u64;
    while x * x < n {
        x += 1;
    }
    x as u32
}
