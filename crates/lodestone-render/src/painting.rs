//! Paintings: the variant table, the per-variant mesh, and the wall placement.
//!
//! # What it is
//!
//! A painting is not a rig and not a billboard — it is a flat slab of `width x
//! height` blocks hung on a wall, its front face carrying the variant's own
//! standalone sprite and its back and four edges carrying one shared `back`
//! tile. `PaintingRenderer.renderPainting` builds it as a grid of 1x1 cells,
//! and this module reproduces that grid rather than collapsing it to one quad,
//! for the reason [`painting_mesh`] documents.
//!
//! # Where the numbers come from
//!
//! * the **geometry** is `PaintingRenderer.renderPainting` (26.2), transcribed
//!   quad by quad;
//! * the **placement** is `PaintingRenderer.submit`'s single
//!   `Axis.YP.rotationDegrees(180 - direction.get2DDataValue() * 90)`;
//! * the **variant table** is the 51 files of `data/minecraft/painting_variant/`
//!   in the pinned 26.2 jar, read mechanically.
//!
//! See `docs/painting-rendering.md`.

use glam::{Mat4, Vec3};

use crate::models::ModelVertex;

/// Every 26.2 painting variant, as `(registry name, width, height)` in blocks.
///
/// Read out of the pinned jar's own `data/minecraft/painting_variant/*.json` —
/// 51 files, each carrying `width`, `height` and `asset_id` — rather than
/// transcribed from `PaintingVariants.bootstrap`. Sorted by name, because
/// **nothing here depends on registry order**: the wire carries a holder id
/// into the *server's* `minecraft:painting_variant` registry, and the name that
/// id resolves to is what reaches this table. Sorting by registry order instead
/// would invite exactly the mistake of indexing this slice with a wire id.
///
/// `asset_id` is not a column because for all 51 vanilla variants it is
/// `minecraft:<name>` — checked mechanically over the same 51 files, with zero
/// exceptions. [`painting_texture_path`] is where that equality is spent, and
/// it is the one place to change if a future version breaks it.
pub const PAINTING_VARIANTS: &[(&str, u32, u32)] = &[
    ("alban", 1, 1),
    ("aztec", 1, 1),
    ("aztec2", 1, 1),
    ("backyard", 3, 4),
    ("baroque", 2, 2),
    ("bomb", 1, 1),
    ("bouquet", 3, 3),
    ("burning_skull", 4, 4),
    ("bust", 2, 2),
    ("cavebird", 3, 3),
    ("changing", 4, 2),
    ("cotan", 3, 3),
    ("courbet", 2, 1),
    ("creebet", 2, 1),
    ("dennis", 3, 3),
    ("donkey_kong", 4, 3),
    ("earth", 2, 2),
    ("endboss", 3, 3),
    ("fern", 3, 3),
    ("fighters", 4, 2),
    ("finding", 4, 2),
    ("fire", 2, 2),
    ("graham", 1, 2),
    ("humble", 2, 2),
    ("kebab", 1, 1),
    ("lowmist", 4, 2),
    ("match", 2, 2),
    ("meditative", 1, 1),
    ("orb", 4, 4),
    ("owlemons", 3, 3),
    ("passage", 4, 2),
    ("pigscene", 4, 4),
    ("plant", 1, 1),
    ("pointer", 4, 4),
    ("pond", 3, 4),
    ("pool", 2, 1),
    ("prairie_ride", 1, 2),
    ("sea", 2, 1),
    ("skeleton", 4, 3),
    ("skull_and_roses", 2, 2),
    ("stage", 2, 2),
    ("sunflowers", 3, 3),
    ("sunset", 2, 1),
    ("tides", 3, 3),
    ("unpacked", 4, 4),
    ("void", 2, 2),
    ("wanderer", 1, 2),
    ("wasteland", 1, 1),
    ("water", 2, 2),
    ("wind", 2, 2),
    ("wither", 2, 2),
];

/// The shared back/edge tile every painting uses, whatever its variant.
///
/// `PaintingRenderer.BACK_SPRITE_LOCATION` is the bare sprite id `back`, which
/// resolves through the paintings atlas (`assets/minecraft/atlases/paintings.json`,
/// a single `minecraft:directory` source over `painting` with an empty prefix)
/// to this file. This engine binds the file directly rather than stitching an
/// atlas, so the empty prefix is why the sprite id has no `painting/` in it and
/// the path does.
pub const PAINTING_BACK_TEXTURE: &str = "assets/minecraft/textures/painting/back.png";

/// One painting variant's size in blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaintingSize {
    /// Width in blocks, 1..=4 across the 51 vanilla variants.
    pub width: u32,
    /// Height in blocks, 1..=4 across the 51 vanilla variants.
    pub height: u32,
}

/// The size of the variant `name`, which may be namespaced
/// (`minecraft:kebab`) or bare (`kebab`), or `None` for a name this build has
/// no table entry for.
///
/// A `None` here is the honest answer for a **data-pack-added** variant: its
/// size arrives over the wire in the `painting_variant` registry payload, which
/// this engine does not model (only `dimension_type` and two biome fields have
/// their payloads decoded), and its texture would not be in the jar either. A
/// caller must draw nothing rather than guess a size — a 1x1 stand-in for a 4x4
/// painting reads as a rendering bug, not as an unsupported pack.
#[must_use]
pub fn painting_size(name: &str) -> Option<PaintingSize> {
    let bare = name.strip_prefix("minecraft:").unwrap_or(name);
    PAINTING_VARIANTS
        .iter()
        .find(|(variant, ..)| *variant == bare)
        .map(|&(_, width, height)| PaintingSize { width, height })
}

/// The table's own `&'static str` for the variant `name` (namespaced or bare),
/// or `None` for a name this build has no entry for.
///
/// Exists so a caller can narrow a wire-supplied key to a static one **once**,
/// at the point the entity is extracted, rather than carrying an owned string
/// per painting per frame — the same shape
/// `lodestone_shell::entities::EntityDraw::variant_sheet` uses. Narrowing at
/// that boundary is also where an unknown data-pack variant becomes `None`, so
/// the draw site never has to decide what to do about one.
#[must_use]
pub fn painting_variant_name(name: &str) -> Option<&'static str> {
    let bare = name.strip_prefix("minecraft:").unwrap_or(name);
    PAINTING_VARIANTS
        .iter()
        .find(|(variant, ..)| *variant == bare)
        .map(|&(variant, ..)| variant)
}

/// The jar path of the variant `name`'s own sprite, namespaced or bare.
///
/// Spends the `asset_id == "minecraft:" + name` equality [`PAINTING_VARIANTS`]
/// records: vanilla resolves `variant.assetId()` through the paintings atlas,
/// and every one of the 51 asset ids is its own registry name.
#[must_use]
pub fn painting_texture_path(name: &str) -> String {
    let bare = name.strip_prefix("minecraft:").unwrap_or(name);
    format!("assets/minecraft/textures/painting/{bare}.png")
}

/// Every distinct `(width, height)` a vanilla painting takes, so a caller can
/// bake one mesh per shape rather than one per variant.
///
/// Nine of them across 51 variants, derived from [`PAINTING_VARIANTS`] rather
/// than listed again — a second hand-written list would be a place for the two
/// to disagree.
#[must_use]
pub fn painting_sizes() -> Vec<PaintingSize> {
    let mut out: Vec<PaintingSize> = Vec::new();
    for &(_, width, height) in PAINTING_VARIANTS {
        let size = PaintingSize { width, height };
        if !out.contains(&size) {
            out.push(size);
        }
    }
    out
}

/// Half the painting's thickness in blocks — `PaintingRenderer.renderPainting`'s
/// `0.03125F`, i.e. half of `Painting.DEPTH`'s `0.0625F`, one texel.
const HALF_DEPTH: f32 = 0.03125;

/// The fraction of the back sprite an edge quad samples —
/// `back.getV(0.0625F)` / `back.getU(0.0625F)`. One sixteenth, i.e. one texel
/// row of the 16px back tile, matching the painting's own 1/16-block depth.
const EDGE_SPRITE_FRACTION: f32 = 0.0625;

/// A painting's two meshes: the variant's front face, and the shared back and
/// edges.
///
/// Two rather than one because they sample **different textures** — the
/// variant's own sprite and the shared `back` tile — and this engine binds one
/// texture per draw. Vanilla escapes that by putting both in the paintings
/// atlas and emitting a single interleaved stream.
#[derive(Debug, Clone)]
pub struct PaintingMesh {
    /// The front face's quads, one per 1x1 cell, sampling the variant sprite
    /// over `(0, 0)..(1, 1)`.
    pub front: (Vec<ModelVertex>, Vec<u32>),
    /// The back face plus the four boundary edges, all sampling `back.png`.
    pub frame: (Vec<ModelVertex>, Vec<u32>),
}

/// Build the two meshes for a `width` x `height` painting, in the entity's own
/// local space (Y **up**, unflipped — a painting is an `EntityRenderer`, not a
/// `LivingEntityRenderer`, so there is no `scale(-1, -1, 1)` and no 1.501 lift).
///
/// # Why the cell grid is reproduced rather than collapsed
///
/// For the front face a single quad spanning the whole painting would be
/// pixel-identical: the per-cell UVs are an exact subdivision of the same
/// sprite, so `width * height` cells and one quad sample the same texels. The
/// grid is kept anyway for two reasons that are not aesthetic. The **back and
/// edges tile** — each cell samples the *whole* `back` sprite, so a 4x4
/// painting's back is 16 copies of the tile, and one stretched quad would be
/// visibly wrong. And vanilla's grid exists to carry **one light sample per
/// cell**, which this engine cannot express today (light is per *instance*
/// here, so all cells share the painting's own probe); keeping the geometry
/// means closing that gap later is a change to the light lane, not a re-bake.
///
/// The cell loop, the vertex order and the UV expressions are
/// `PaintingRenderer.renderPainting`'s, transcribed. Note `x0` is the cell's
/// **+1** edge and `x1` its base edge, and that the front UVs count *down*
/// (`width - segment_x`), which is what draws the image unmirrored once the
/// placement's `180 - yaw` rotation is applied.
#[must_use]
pub fn painting_mesh(width: u32, height: u32) -> PaintingMesh {
    let mut front = QuadSink::default();
    let mut frame = QuadSink::default();

    let offset_x = -(width as f32) / 2.0;
    let offset_y = -(height as f32) / 2.0;
    let delta_u = 1.0 / width as f32;
    let delta_v = 1.0 / height as f32;
    // The back sprite is bound as a standalone texture rather than as an atlas
    // region, so vanilla's `back.getU0()`/`getU1()` are 0 and 1 and its
    // `getU(f)`/`getV(f)` are `f` itself.
    let (back_u0, back_u1, back_v0, back_v1) = (0.0, 1.0, 0.0, 1.0);
    let (tb_u0, tb_u1, tb_v0, tb_v1) = (0.0, 1.0, 0.0, EDGE_SPRITE_FRACTION);
    let (lr_u0, lr_u1, lr_v0, lr_v1) = (0.0, EDGE_SPRITE_FRACTION, 0.0, 1.0);

    for segment_x in 0..width {
        for segment_y in 0..height {
            let x0 = offset_x + (segment_x + 1) as f32;
            let x1 = offset_x + segment_x as f32;
            let y0 = offset_y + (segment_y + 1) as f32;
            let y1 = offset_y + segment_y as f32;
            let front_u0 = delta_u * (width - segment_x) as f32;
            let front_u1 = delta_u * (width - (segment_x + 1)) as f32;
            let front_v0 = delta_v * (height - segment_y) as f32;
            let front_v1 = delta_v * (height - (segment_y + 1)) as f32;

            front.quad([
                ([x0, y1, -HALF_DEPTH], [front_u1, front_v0]),
                ([x1, y1, -HALF_DEPTH], [front_u0, front_v0]),
                ([x1, y0, -HALF_DEPTH], [front_u0, front_v1]),
                ([x0, y0, -HALF_DEPTH], [front_u1, front_v1]),
            ]);
            frame.quad([
                ([x0, y0, HALF_DEPTH], [back_u1, back_v0]),
                ([x1, y0, HALF_DEPTH], [back_u0, back_v0]),
                ([x1, y1, HALF_DEPTH], [back_u0, back_v1]),
                ([x0, y1, HALF_DEPTH], [back_u1, back_v1]),
            ]);
            if segment_y == height - 1 {
                frame.quad([
                    ([x0, y0, -HALF_DEPTH], [tb_u0, tb_v0]),
                    ([x1, y0, -HALF_DEPTH], [tb_u1, tb_v0]),
                    ([x1, y0, HALF_DEPTH], [tb_u1, tb_v1]),
                    ([x0, y0, HALF_DEPTH], [tb_u0, tb_v1]),
                ]);
            }
            if segment_y == 0 {
                frame.quad([
                    ([x0, y1, HALF_DEPTH], [tb_u0, tb_v0]),
                    ([x1, y1, HALF_DEPTH], [tb_u1, tb_v0]),
                    ([x1, y1, -HALF_DEPTH], [tb_u1, tb_v1]),
                    ([x0, y1, -HALF_DEPTH], [tb_u0, tb_v1]),
                ]);
            }
            if segment_x == width - 1 {
                frame.quad([
                    ([x0, y0, HALF_DEPTH], [lr_u1, lr_v0]),
                    ([x0, y1, HALF_DEPTH], [lr_u1, lr_v1]),
                    ([x0, y1, -HALF_DEPTH], [lr_u0, lr_v1]),
                    ([x0, y0, -HALF_DEPTH], [lr_u0, lr_v0]),
                ]);
            }
            if segment_x == 0 {
                frame.quad([
                    ([x1, y0, -HALF_DEPTH], [lr_u1, lr_v0]),
                    ([x1, y1, -HALF_DEPTH], [lr_u1, lr_v1]),
                    ([x1, y1, HALF_DEPTH], [lr_u0, lr_v1]),
                    ([x1, y0, HALF_DEPTH], [lr_u0, lr_v0]),
                ]);
            }
        }
    }

    PaintingMesh {
        front: (front.vertices, front.indices),
        frame: (frame.vertices, frame.indices),
    }
}

/// Accumulates quads into one vertex/index pair, four vertices and six indices
/// at a time, in the same `[0, 1, 2, 0, 2, 3]` winding every other baked quad
/// in this crate uses.
#[derive(Default)]
struct QuadSink {
    vertices: Vec<ModelVertex>,
    indices: Vec<u32>,
}

impl QuadSink {
    fn quad(&mut self, corners: [([f32; 3], [f32; 2]); 4]) {
        let base = self.vertices.len() as u32;
        for (position, uv) in corners {
            self.vertices.push(ModelVertex {
                position,
                uv,
                ao: 1.0,
                // Light, tint and animation all arrive per *instance* on this
                // pass, exactly as the orb and flame meshes leave them: a
                // painting's light is its entity probe and its tint is white.
                light: 0,
                tint: 255,
                anim: 0,
                cutout_bypass: 0,
                tint_rgb_override: [0, 0, 0, 0],
            });
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// The world placement for one painting: `PaintingRenderer.submit`'s whole pose
/// stack.
///
/// ```text
/// T(position) · Ry(180 - yaw)
/// ```
///
/// `position` is the entity's wire position, which for a painting is the slab's
/// **centre** rather than a mob's feet — `Painting.calculateBoundingBox` places
/// it there, which is why [`painting_mesh`]'s local frame is centred on the
/// origin in both axes.
///
/// `yaw_degrees` is the entity's ordinary body yaw off the wire, and that is
/// not a coincidence worth glossing over: `HangingEntity.setDirection` does
/// `setYRot(direction.get2DDataValue() * 90)`, so a painting's facing **is**
/// its yaw and needs no separate direction field decoded from the spawn
/// packet's Object Data. The four legal values are exactly `0` (south), `90`
/// (west), `180` (north) and `270` (east), each of which survives the wire's
/// byte-angle quantisation exactly.
///
/// No `scale(-1, -1, 1)` and no 1.501 lift, unlike
/// [`entity_model_matrix`](crate::entity::entity_model_matrix): `PaintingRenderer`
/// extends `EntityRenderer`, not `LivingEntityRenderer`, and the mesh above is
/// authored Y-up to match.
#[must_use]
pub fn painting_matrix(position: Vec3, yaw_degrees: f32) -> Mat4 {
    Mat4::from_translation(position) * Mat4::from_rotation_y((180.0 - yaw_degrees).to_radians())
}
