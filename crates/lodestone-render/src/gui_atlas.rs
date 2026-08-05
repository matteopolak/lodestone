//! GUI sprite atlas: the render-side bridge that finally reaches a pixel with
//! [`lodestone_assets`]'s otherwise-unconsumed GUI machinery.
//!
//! Vanilla 26.2 ships its HUD art as individual PNGs under
//! `assets/<ns>/textures/gui/sprites/**` (the *modern* sprite layout — there is
//! no legacy `icons.png` sheet). This module enumerates that tree, decodes each
//! sprite, honours its sibling `<name>.png.mcmeta` GUI scaling (stretch / tile /
//! nine-slice via [`lodestone_assets::gui`]), and stitches everything into one
//! [`Atlas`] using the same [`AtlasBuilder`] the block atlas uses — no forked
//! stitcher.
//!
//! Two halves, deliberately split so the pure half is testable without a GPU:
//!
//! * **Producer** — [`GuiAtlas::build`] turns a [`ResourceManager`] into a
//!   stitched atlas plus a per-sprite scaling table.
//! * **Consumer** — [`GuiAtlas::geometry`] is a *pure* function mapping a sprite
//!   id + destination rectangle to a list of [`GuiSpriteQuad`]s (destination in
//!   target pixels, source as normalised atlas UVs). This is what a HUD emits as
//!   textured quads; the same call decomposes a nine-slice panel into its nine
//!   pieces or a stretched heart into one.
//!
//! The GPU upload itself is *not* here: a consumer pairs [`GuiAtlas::atlas`]
//! with [`crate::texture::GpuAtlas::from_atlas`] to get a bound texture, then
//! feeds [`GuiAtlas::geometry`] output through its own textured pipeline.

use std::collections::HashMap;

use lodestone_assets::gui::{GuiMeta, GuiScaling};
use lodestone_assets::{Atlas, AtlasBuilder, AtlasError, Image, ResourceLocation, ResourceManager};
use thiserror::Error;

/// The in-pack path segment that marks a GUI sprite, sitting between the
/// namespace and the sprite id: `assets/<ns>/textures/gui/sprites/<id>.png`.
const SPRITES_INFIX: &str = "/textures/gui/sprites/";

/// One textured quad produced by [`GuiAtlas::geometry`].
///
/// `dst` is `[x, y, w, h]` in absolute target pixels (the draw position has
/// already been applied). `uv_min`/`uv_max` are the normalised atlas UVs of the
/// source rectangle this quad samples. A consumer emits two triangles over
/// `dst`, sampling the atlas across `[uv_min, uv_max]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuiSpriteQuad {
    /// Destination rectangle `[x, y, w, h]` in absolute target pixels.
    pub dst: [f32; 4],
    /// Top-left atlas UV of the sampled source rectangle.
    pub uv_min: [f32; 2],
    /// Bottom-right atlas UV of the sampled source rectangle.
    pub uv_max: [f32; 2],
}

/// A stitched atlas of every `gui/sprites/**` texture plus each sprite's GUI
/// scaling mode. Build it once per resource pack; query it per HUD frame.
#[derive(Debug)]
pub struct GuiAtlas {
    atlas: Atlas,
    /// id (`hud/heart/full`) → (its atlas location, its GUI scaling mode).
    sprites: HashMap<String, SpriteEntry>,
}

#[derive(Debug, Clone)]
struct SpriteEntry {
    location: ResourceLocation,
    scaling: GuiScaling,
}

/// Why a [`GuiAtlas::build`] failed. Fail-closed: a consumer that cannot build
/// the atlas falls back to its procedural HUD rather than rendering nothing, so
/// every variant here names a concrete, fixable cause.
#[derive(Debug, Error)]
pub enum GuiAtlasError {
    /// No `gui/sprites/**` PNGs were found in the pack. Almost always a wrong
    /// pack root (a jar without client GUI assets, or a stripped pack).
    #[error("no gui/sprites textures found in the resource pack")]
    NoSprites,
    /// A sprite PNG failed to decode. Carries the offending in-pack path.
    #[error("decode {path}: {source}")]
    Decode {
        /// The in-pack path of the sprite that failed to decode.
        path: String,
        /// The underlying decode error.
        source: lodestone_assets::TextureError,
    },
    /// A sprite's `.png.mcmeta` failed to parse. Carries the offending path.
    #[error("parse {path}: {message}")]
    Meta {
        /// The in-pack path of the `.png.mcmeta` that failed to parse.
        path: String,
        /// The stringified parse error.
        message: String,
    },
    /// A sprite path did not form a valid [`ResourceLocation`].
    #[error("bad sprite location for {path}: {message}")]
    Location {
        /// The in-pack path that could not be turned into a location.
        path: String,
        /// The stringified location error.
        message: String,
    },
    /// The underlying stitch failed (for example the atlas exceeded limits).
    #[error("stitch gui atlas: {0}")]
    Stitch(#[from] AtlasError),
}

impl GuiAtlas {
    /// Build the GUI atlas from a resource pack.
    ///
    /// Enumerates every `assets/<ns>/textures/gui/sprites/**.png` across the
    /// pack stack, decodes it, parses its sibling `.png.mcmeta` (defaulting to
    /// [`GuiScaling::Stretch`] when absent, exactly like vanilla), and stitches
    /// the lot with a mip-free [`AtlasBuilder`] (the HUD only ever magnifies, so
    /// mips would never be sampled and only bloat the upload).
    ///
    /// Fails closed: an empty sprite set is a [`GuiAtlasError::NoSprites`] so the
    /// caller falls back loudly rather than binding a blank atlas.
    pub fn build(manager: &ResourceManager) -> Result<Self, GuiAtlasError> {
        Self::build_with_extras(manager, &[])
    }

    /// As [`GuiAtlas::build`], plus a list of `(id, in-pack path)` **loose**
    /// textures stitched into the same atlas and looked up under `id`.
    ///
    /// This exists for the handful of GUI textures vanilla blits by raw path
    /// rather than through the sprite atlas — the title screen's
    /// `textures/gui/title/minecraft.png` logo and its `edition.png` companion.
    /// They are outside `gui/sprites/**`, so [`GuiAtlas::build`] structurally
    /// cannot see them, and the alternative for a consumer is a second texture,
    /// a second bind group and a second pipeline for two quads.
    ///
    /// Extras are **fail-open and never override a real sprite**: a missing or
    /// undecodable texture is skipped (the caller then draws nothing for that
    /// id, exactly as for any unknown id), and an id already claimed by a
    /// sprite is left alone. One absent loose texture must not take the whole
    /// pack's real HUD sprites down with it.
    ///
    /// Extras are always [`GuiScaling::Stretch`]: they have no `.mcmeta` and
    /// vanilla blits them as one quad.
    pub fn build_with_extras(
        manager: &ResourceManager,
        extras: &[(&str, &str)],
    ) -> Result<Self, GuiAtlasError> {
        // Enumerate the whole asset tree once and filter to GUI sprite PNGs.
        // Namespace-general: we do not assume `minecraft`, we read it out of the
        // path, so a resource pack that adds sprites under its own namespace is
        // picked up too.
        let mut png_paths: Vec<String> = manager
            .list("assets/")
            .into_iter()
            .filter(|p| is_sprite_png(p))
            .collect();
        // Deterministic packing: sort so a given pack always yields the same
        // atlas layout (stable UVs across runs, easier to reason about in gates).
        png_paths.sort();

        if png_paths.is_empty() {
            return Err(GuiAtlasError::NoSprites);
        }

        let mut builder = AtlasBuilder::new();
        let mut sprites: HashMap<String, SpriteEntry> = HashMap::with_capacity(png_paths.len());

        for path in &png_paths {
            let Some((namespace, id)) = split_sprite_path(path) else {
                continue;
            };

            let bytes = manager.read(path).ok_or_else(|| GuiAtlasError::Decode {
                path: path.clone(),
                source: lodestone_assets::TextureError::Decode(
                    "listed sprite path vanished from the pack".to_string(),
                ),
            })?;
            let image = Image::decode_png(&bytes).map_err(|e| GuiAtlasError::Decode {
                path: path.clone(),
                source: e,
            })?;

            // Sibling `<path>.mcmeta`. Absent → vanilla default of Stretch.
            let meta_path = format!("{path}.mcmeta");
            let scaling = match manager.read(&meta_path) {
                Some(meta_bytes) => {
                    GuiMeta::parse(&meta_bytes)
                        .map_err(|e| GuiAtlasError::Meta {
                            path: meta_path.clone(),
                            message: e.to_string(),
                        })?
                        .scaling
                }
                None => GuiScaling::Stretch,
            };

            // The atlas location is synthetic but unique and stable: it carries
            // the namespace and the full `gui/sprites/<id>` path so two sprites
            // never collide and the id round-trips for lookup.
            let location =
                ResourceLocation::new(namespace, format!("gui/sprites/{id}")).map_err(|e| {
                    GuiAtlasError::Location {
                        path: path.clone(),
                        message: e.to_string(),
                    }
                })?;

            builder.add_texture(location.clone(), image, None);
            sprites.insert(id.to_string(), SpriteEntry { location, scaling });
        }

        for (id, path) in extras {
            if sprites.contains_key(*id) {
                continue;
            }
            let Some(bytes) = manager.read(path) else {
                continue;
            };
            let Ok(image) = Image::decode_png(&bytes) else {
                continue;
            };
            // A namespace of its own so a loose texture can never collide with
            // a real sprite's synthetic location.
            let Ok(location) = ResourceLocation::new("lodestone", format!("gui/loose/{id}")) else {
                continue;
            };
            builder.add_texture(location.clone(), image, None);
            sprites.insert(
                (*id).to_string(),
                SpriteEntry {
                    location,
                    scaling: GuiScaling::Stretch,
                },
            );
        }

        let atlas = builder.build()?;
        Ok(Self { atlas, sprites })
    }

    /// The stitched atlas, for GPU upload via
    /// [`crate::texture::GpuAtlas::from_atlas`].
    #[must_use]
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// The number of sprites stitched into the atlas.
    #[must_use]
    pub fn sprite_count(&self) -> usize {
        self.sprites.len()
    }

    /// Whether a sprite id (for example `hud/heart/full`) is present.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.sprites.contains_key(id)
    }

    /// The native pixel size `(width, height)` of a sprite, or `None` if the id
    /// is unknown. GUI sprites are never animated, so this is simply the placed
    /// region's size.
    #[must_use]
    pub fn native_size(&self, id: &str) -> Option<(u32, u32)> {
        let entry = self.sprites.get(id)?;
        let sprite = self.atlas.sprite(&entry.location)?;
        Some((sprite.width, sprite.height))
    }

    /// Map a sprite id drawn into the destination rectangle `(x, y, w, h)`
    /// (target pixels) to the textured quads that render it, honouring the
    /// sprite's GUI scaling.
    ///
    /// A stretched sprite yields a single quad; a nine-slice panel yields up to
    /// nine (corners fixed, edges scaled, centre filled); a tiled sprite yields
    /// one quad per repeat. Returns an empty vector for an unknown id, so a HUD
    /// can call it unconditionally and simply draw nothing for a missing sprite.
    #[must_use]
    pub fn geometry(&self, id: &str, x: f32, y: f32, w: f32, h: f32) -> Vec<GuiSpriteQuad> {
        let Some(entry) = self.sprites.get(id) else {
            return Vec::new();
        };
        let Some(sprite) = self.atlas.sprite(&entry.location) else {
            return Vec::new();
        };

        let native_w = sprite.width;
        let native_h = sprite.height;
        let dst_w = w.round().max(0.0) as u32;
        let dst_h = h.round().max(0.0) as u32;

        let quads = entry.scaling.geometry(native_w, native_h, dst_w, dst_h);

        let atlas_w = self.atlas.width as f32;
        let atlas_h = self.atlas.height as f32;
        let sprite_x = sprite.x as f32;
        let sprite_y = sprite.y as f32;

        quads
            .into_iter()
            .map(|q| {
                // `q.src` is in native sprite pixels relative to the sprite's
                // own origin; shift into atlas pixels then normalise to UVs.
                let [sx, sy, sw, sh] = q.src;
                let uv_min = [(sprite_x + sx) / atlas_w, (sprite_y + sy) / atlas_h];
                let uv_max = [
                    (sprite_x + sx + sw) / atlas_w,
                    (sprite_y + sy + sh) / atlas_h,
                ];
                // `q.dst` is target pixels relative to the draw origin; shift by
                // the requested draw position to get absolute target pixels.
                let [dx, dy, dw, dh] = q.dst;
                GuiSpriteQuad {
                    dst: [x + dx as f32, y + dy as f32, dw as f32, dh as f32],
                    uv_min,
                    uv_max,
                }
            })
            .collect()
    }

    /// A **sub-rectangle** of a sprite, sampled at `src` (in the sprite's own
    /// native pixel space) and drawn at `dst` (`[x, y, w, h]` in target pixels).
    ///
    /// [`geometry`](Self::geometry) always maps the *whole* sprite through its
    /// [`GuiScaling`], which is right for every real `gui/sprites/**` entry —
    /// vanilla blits those through `blitSprite`, which does exactly that. It is
    /// wrong for the handful of GUI textures vanilla blits by **raw path** from
    /// a larger sheet, which [`build_with_extras`](Self::build_with_extras)
    /// stitches in whole: the recipe book's panel is
    /// `blit(RECIPE_BOOK_LOCATION, xo, yo, 1.0F, 1.0F, 147, 166, 256, 256)`
    /// (`RecipeBookComponent.java:305`) — a `147×166` window at `(1, 1)` of a
    /// `256×256` sheet. Passing that sheet to `geometry` would *stretch* all
    /// 256×256 of it into a 147×166 rect instead.
    ///
    /// `None` for an unknown id, so a caller can emit the request
    /// unconditionally and draw nothing on a pack that lacks the texture. The
    /// sprite's own `GuiScaling` is deliberately **ignored**: a sub-rect request
    /// is by definition a fixed window, and a nine-slice of an arbitrary window
    /// is not a thing vanilla ever does.
    #[must_use]
    pub fn subregion_quad(&self, id: &str, src: [f32; 4], dst: [f32; 4]) -> Option<GuiSpriteQuad> {
        let entry = self.sprites.get(id)?;
        let sprite = self.atlas.sprite(&entry.location)?;
        let (atlas_w, atlas_h) = (self.atlas.width as f32, self.atlas.height as f32);
        let [sx, sy, sw, sh] = src;
        Some(GuiSpriteQuad {
            dst,
            uv_min: [
                (sprite.x as f32 + sx) / atlas_w,
                (sprite.y as f32 + sy) / atlas_h,
            ],
            uv_max: [
                (sprite.x as f32 + sx + sw) / atlas_w,
                (sprite.y as f32 + sy + sh) / atlas_h,
            ],
        })
    }
}

/// True for `assets/<ns>/textures/gui/sprites/<id>.png` (and not its `.mcmeta`).
fn is_sprite_png(path: &str) -> bool {
    path.starts_with("assets/")
        && path.contains(SPRITES_INFIX)
        && path.ends_with(".png")
        && !path.ends_with(".png.mcmeta")
}

/// Split `assets/<ns>/textures/gui/sprites/<id>.png` into `(ns, id)`.
fn split_sprite_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("assets/")?;
    let infix_at = rest.find(SPRITES_INFIX)?;
    let namespace = &rest[..infix_at];
    if namespace.is_empty() || namespace.contains('/') {
        return None;
    }
    let after = &rest[infix_at + SPRITES_INFIX.len()..];
    let id = after.strip_suffix(".png")?;
    if id.is_empty() {
        return None;
    }
    Some((namespace, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_assets::gui::Border;
    use lodestone_assets::{MemorySource, ResourceSource};

    /// Encode a solid-colour RGBA PNG so a `MemorySource` can stand in for a
    /// real jar in a hermetic test (no GPU, no disk, no window).
    fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut data, w, h);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("png header");
            let pixels: Vec<u8> = (0..(w * h)).flat_map(|_| rgba).collect();
            writer.write_image_data(&pixels).expect("png data");
        }
        data
    }

    /// A `.png.mcmeta` requesting nine-slice scaling with a uniform border.
    fn nine_slice_mcmeta(width: u32, height: u32, border: u32) -> Vec<u8> {
        format!(
            r#"{{"gui":{{"scaling":{{"type":"nine_slice","width":{width},"height":{height},"border":{border}}}}}}}"#
        )
        .into_bytes()
    }

    /// A pack with one stretched sprite and one bordered nine-slice sprite.
    fn synthetic_manager() -> ResourceManager {
        let mut src = MemorySource::default();
        // A plain 9x9 heart-shaped stand-in (stretch, no mcmeta).
        src.insert(
            "assets/minecraft/textures/gui/sprites/hud/heart/full.png",
            solid_png(9, 9, [200, 20, 30, 255]),
        );
        // A 16x16 nine-slice panel with a 4px border and its mcmeta sibling.
        src.insert(
            "assets/minecraft/textures/gui/sprites/widget/panel.png",
            solid_png(16, 16, [40, 40, 60, 255]),
        );
        src.insert(
            "assets/minecraft/textures/gui/sprites/widget/panel.png.mcmeta",
            nine_slice_mcmeta(16, 16, 4),
        );
        // A non-sprite GUI texture that must NOT be picked up (wrong subtree).
        src.insert(
            "assets/minecraft/textures/gui/title/minecraft.png",
            solid_png(2, 2, [1, 2, 3, 255]),
        );
        ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>])
    }

    #[test]
    fn build_enumerates_only_gui_sprites() {
        let atlas = GuiAtlas::build(&synthetic_manager()).expect("atlas builds");
        // Two sprites: the heart and the panel. The `gui/title/` texture and the
        // `.png.mcmeta` sibling are both excluded.
        assert_eq!(atlas.sprite_count(), 2);
        assert!(atlas.contains("hud/heart/full"));
        assert!(atlas.contains("widget/panel"));
        assert!(!atlas.contains("title/minecraft"));
        assert_eq!(atlas.native_size("hud/heart/full"), Some((9, 9)));
    }

    /// The three air-supply-bubble sprites vanilla ships at
    /// `gui/sprites/hud/air*.png` need **no** dedicated loader: this generic
    /// `gui/sprites/**` glob already stitches them in, exactly like the heart
    /// and hunger sprites above. This is the render-side half of the
    /// air-supply-bubble feature confirmed *already wired* — see
    /// `crate::air_bubbles`, whose `BubbleSlot::sprite_id` names these same
    /// three ids. What is actually missing end-to-end is the `airSupply`
    /// *value* (not yet decoded anywhere in the protocol/ECS layers this
    /// crate sits above), not this atlas.
    #[test]
    fn air_bubble_sprites_are_covered_by_the_generic_hud_glob() {
        let mut src = MemorySource::default();
        for id in ["hud/air", "hud/air_empty", "hud/air_bursting"] {
            src.insert(
                format!("assets/minecraft/textures/gui/sprites/{id}.png"),
                solid_png(9, 9, [255, 255, 255, 255]),
            );
        }
        let manager = ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>]);
        let atlas = GuiAtlas::build(&manager).expect("atlas builds");
        for id in ["hud/air", "hud/air_empty", "hud/air_bursting"] {
            assert!(atlas.contains(id), "{id} must be stitched in with no special-casing");
            assert_eq!(atlas.native_size(id), Some((9, 9)));
        }
    }

    #[test]
    fn build_is_fail_closed_on_empty_pack() {
        let empty = ResourceManager::new(vec![
            Box::new(MemorySource::default()) as Box<dyn ResourceSource>
        ]);
        assert!(matches!(
            GuiAtlas::build(&empty),
            Err(GuiAtlasError::NoSprites)
        ));
    }

    #[test]
    fn stretch_sprite_yields_one_quad_covering_the_dest() {
        let atlas = GuiAtlas::build(&synthetic_manager()).expect("atlas builds");
        // Draw the 9x9 heart at (10, 20) scaled to 18x18.
        let quads = atlas.geometry("hud/heart/full", 10.0, 20.0, 18.0, 18.0);
        assert_eq!(quads.len(), 1, "a stretched sprite is a single quad");
        let q = quads[0];
        assert_eq!(q.dst, [10.0, 20.0, 18.0, 18.0]);

        // The UVs must bound exactly the heart's placed region in the atlas.
        let sprite = atlas
            .atlas()
            .sprite(&ResourceLocation::new("minecraft", "gui/sprites/hud/heart/full").unwrap())
            .expect("heart placed");
        let (aw, ah) = (atlas.atlas().width as f32, atlas.atlas().height as f32);
        let expect_min = [sprite.x as f32 / aw, sprite.y as f32 / ah];
        let expect_max = [
            (sprite.x + sprite.width) as f32 / aw,
            (sprite.y + sprite.height) as f32 / ah,
        ];
        assert!((q.uv_min[0] - expect_min[0]).abs() < 1e-6);
        assert!((q.uv_min[1] - expect_min[1]).abs() < 1e-6);
        assert!((q.uv_max[0] - expect_max[0]).abs() < 1e-6);
        assert!((q.uv_max[1] - expect_max[1]).abs() < 1e-6);
    }

    #[test]
    fn nine_slice_panel_decomposes_into_nine_pieces() {
        let atlas = GuiAtlas::build(&synthetic_manager()).expect("atlas builds");
        // Sanity: the panel really parsed as nine-slice, not stretch.
        let entry = atlas.sprites.get("widget/panel").expect("panel present");
        assert_eq!(
            entry.scaling,
            GuiScaling::NineSlice {
                width: 16,
                height: 16,
                border: Border::uniform(4),
                stretch_inner: false,
            }
        );

        // Drawn larger than native, a nine-slice sprite decomposes into many
        // bounded pieces — fixed corners, axis-scaled edges, and (because
        // vanilla's default `stretch_inner` is false) a *tiled* centre and
        // edges, so a 64x48 draw yields well more than one quad. Stretch would
        // give exactly one; this asserts the scaling path actually branched.
        let quads = atlas.geometry("widget/panel", 0.0, 0.0, 64.0, 48.0);
        assert!(
            quads.len() >= 9,
            "nine-slice draws many pieces (got {}), not one",
            quads.len()
        );
        // And it is genuinely sliced: at least one piece samples a sub-region
        // strictly smaller than the whole sprite (a stretch quad would sample
        // the entire sprite in its single quad).
        let full_w = 16.0 / atlas.atlas().width as f32;
        assert!(
            quads
                .iter()
                .any(|q| (q.uv_max[0] - q.uv_min[0]) < full_w - 1e-6),
            "no sub-sprite slice found — nine-slice did not decompose"
        );

        // Every quad's UVs sit inside the panel's atlas region (no bleed).
        let sprite = atlas.atlas().sprite(&entry.location).expect("panel placed");
        let (aw, ah) = (atlas.atlas().width as f32, atlas.atlas().height as f32);
        let u0 = sprite.x as f32 / aw;
        let v0 = sprite.y as f32 / ah;
        let u1 = (sprite.x + sprite.width) as f32 / aw;
        let v1 = (sprite.y + sprite.height) as f32 / ah;
        for q in &quads {
            assert!(q.uv_min[0] >= u0 - 1e-6 && q.uv_max[0] <= u1 + 1e-6);
            assert!(q.uv_min[1] >= v0 - 1e-6 && q.uv_max[1] <= v1 + 1e-6);
        }
    }

    #[test]
    fn extras_stitch_loose_textures_and_never_clobber_a_real_sprite() {
        // The title screen's logo lives at `textures/gui/title/minecraft.png`,
        // outside `gui/sprites/**` — `build` structurally cannot see it, which
        // is the whole reason `build_with_extras` exists. The synthetic pack
        // above already contains exactly that path.
        let plain = GuiAtlas::build(&synthetic_manager()).expect("atlas builds");
        assert!(
            !plain.contains("title/minecraft"),
            "the negative control: `build` must not see a loose texture"
        );

        let with = GuiAtlas::build_with_extras(
            &synthetic_manager(),
            &[
                (
                    "title/minecraft",
                    "assets/minecraft/textures/gui/title/minecraft.png",
                ),
                // Absent from the pack: must be skipped, not fail the build.
                ("title/nope", "assets/minecraft/textures/gui/title/nope.png"),
                // Collides with a real sprite: the sprite must win.
                ("hud/heart/full", "assets/minecraft/textures/gui/title/minecraft.png"),
            ],
        )
        .expect("atlas builds with extras");
        assert!(with.contains("title/minecraft"));
        assert_eq!(with.native_size("title/minecraft"), Some((2, 2)));
        assert!(!with.contains("title/nope"), "a missing extra is skipped");
        assert_eq!(
            with.native_size("hud/heart/full"),
            Some((9, 9)),
            "an extra must not override the real sprite of the same id"
        );
        // And it is drawable: one stretched quad over the requested rect.
        let quads = with.geometry("title/minecraft", 4.0, 8.0, 256.0, 64.0);
        assert_eq!(quads.len(), 1);
        assert_eq!(quads[0].dst, [4.0, 8.0, 256.0, 64.0]);
    }

    #[test]
    fn unknown_sprite_id_yields_no_geometry() {
        let atlas = GuiAtlas::build(&synthetic_manager()).expect("atlas builds");
        assert!(atlas.geometry("hud/nope", 0.0, 0.0, 9.0, 9.0).is_empty());
        assert_eq!(atlas.native_size("hud/nope"), None);
    }

    #[test]
    fn path_split_is_namespace_general() {
        assert_eq!(
            split_sprite_path("assets/mypack/textures/gui/sprites/hud/heart/full.png"),
            Some(("mypack", "hud/heart/full"))
        );
        assert!(is_sprite_png(
            "assets/minecraft/textures/gui/sprites/hud/hotbar.png"
        ));
        assert!(!is_sprite_png(
            "assets/minecraft/textures/gui/sprites/hud/hotbar.png.mcmeta"
        ));
        assert!(!is_sprite_png("assets/minecraft/textures/block/stone.png"));
    }
}
