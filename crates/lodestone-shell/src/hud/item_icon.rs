//! The shared item-icon pass: everything needed to turn an item id into drawn
//! pixels in a 16×16 GUI slot, independent of *which* screen owns the slot.
//!
//! This started life inside [`crate::hud`], serving the nine hotbar cells. The
//! container/inventory screen ([`crate::container`]) needs exactly the same
//! thing — an icon, a stack count, a durability bar — for up to 46 slots, and
//! the alternative to sharing was a second copy of the atlas upload, the two
//! pipelines, the growable buffers and the pose/mesh call. Two copies of a
//! *pipeline* is not a style problem: it is a second 30 MB atlas upload and a
//! second tint palette that can silently drift green from the world's.
//!
//! # The two halves
//!
//! * **CPU** — [`draw_item_icon`] appends to three streams it does not own
//!   ([`IconSink`]): flat sprite quads, 3-D model vertices, and plain coloured
//!   quads for the count/durability chrome. Each screen keeps its own buffers
//!   and its own layout; only the per-slot emission is shared.
//! * **GPU** — [`IconRenderer`] owns the item-sprite pipeline (its own atlas
//!   texture) and the 3-D item-model pass (which *borrows* the world renderer's
//!   block atlas, tint palette and animation slots). Both are attached
//!   explicitly and both are absent by default, so a jar-less or headless run
//!   draws no icons at all — the negative control both pixel gates exercise.
//!
//! # Two kinds of icon
//!
//! Which stream a slot reaches is decided by the [`IconPart`] the item atlas
//! resolved:
//!
//! * [`IconPart::Sprite`] — the flat `item/generated` majority, one textured
//!   quad per layer off the item atlas;
//! * [`IconPart::Model`] — a **block** item, drawn as vanilla's isometric
//!   mini-block from geometry baked against the *block* atlas
//!   ([`BlockModels::item`]).
//!
//! [`IconPart::Special`] (chests, shulkers, shields, banners) is a code-driven
//! renderer we do not have, and deliberately draws nothing rather than a wrong
//! or missing-texture square — but its count and durability still draw, keeping
//! the well honest.

use std::sync::Arc;

use lodestone_assets::{IconPart, ItemAtlas, ResourceLocation};
use lodestone_render::{
    BlockModels, CameraUniform, GpuAtlas, GuiSpriteQuad, ModelCameraUniform, ModelPipeline,
    ModelVertex, RenderLayer, gui_item_pose, gui_ortho, mesh_item_quads, model_camera_buffer,
};

use super::font;
use super::{FLOATS_PER_VERTEX, HUD_SPRITE_WGSL, SPRITE_FLOATS_PER_VERTEX};

/// One occupied slot's drawable state, resolved shell-side from a
/// [`lodestone_game::menu::Menu`]. `item` is the item id
/// (`minecraft:diamond_pickaxe`) used to look up the resolved icon in the
/// [`ItemAtlas`]; `count` drives the stack number; `damage`/`max_damage` drive
/// the durability bar; `enchanted` marks items that should get the glint overlay
/// (deferred).
///
/// Re-exported as [`crate::hud::HotbarSlot`], which is the name the hotbar has
/// always called it; the container screen builds the same record per menu slot.
#[derive(Debug, Clone)]
pub struct ItemIcon {
    /// The item id, e.g. `minecraft:stone` — the [`ItemAtlas`] icon key.
    pub item: ResourceLocation,
    /// Stack size; the number is drawn bottom-right when `> 1`.
    pub count: u32,
    /// Current damage, `Some` for damageable items that have taken damage.
    pub damage: Option<u32>,
    /// Max durability, `Some` for damageable items; pairs with `damage` for the
    /// bar fraction.
    pub max_damage: Option<u32>,
    /// Whether the stack is enchanted (glint overlay is deferred; see notes).
    pub enchanted: bool,
}

/// The baked resources an icon resolves against. Both are optional and both
/// being `None` is the jar-less path, where [`draw_item_icon`] emits chrome
/// (count, durability) but no icon.
#[derive(Debug, Clone, Copy)]
pub(crate) struct IconAssets<'a> {
    /// The flat item-sprite atlas; `None` leaves sprite icons undrawn.
    pub items: Option<&'a ItemAtlas>,
    /// The baked model set, for items whose icon is a 3-D mini-block; `None`
    /// leaves block icons undrawn.
    pub models: Option<&'a BlockModels>,
}

/// The three vertex streams an icon appends to, borrowed from whichever screen
/// is drawing. Kept as three `&mut Vec` rather than an owned struct so the
/// caller's existing buffers are filled in place and nothing is copied.
pub(crate) struct IconSink<'o> {
    /// The count text and durability bar.
    pub colour: ColourStream<'o>,
    /// Flat `[x, y, u, v, r, g, b, a]` quads sampling the **item** atlas.
    pub sprite: &'o mut Vec<f32>,
    /// 3-D block-item geometry, already posed into GUI pixel space.
    pub model: &'o mut Vec<ModelVertex>,
}

/// Draw one slot's icon into the `size`×`size` rect at `(x, y)`: the icon
/// itself, its durability bar, and its stack count. `view` is the target
/// viewport in pixels, which is all the NDC conversion needs.
///
/// See the [module docs](self) for which icon kind reaches which stream.
pub(crate) fn draw_item_icon(
    sink: &mut IconSink<'_>,
    assets: &IconAssets<'_>,
    view: (f32, f32),
    slot: &ItemIcon,
    x: f32,
    y: f32,
    size: f32,
) {
    let (vw, vh) = view;
    let scale = size / 16.0;
    if let Some(atlas) = assets.items
        && let Some(icon) = atlas.icon(&slot.item)
    {
        for part in &icon.parts {
            match part {
                IconPart::Sprite { layers } => {
                    for layer in layers {
                        // Tint resolution (leather/potion/spawn-egg dyes,
                        // foliage) is deferred; untinted white is correct for
                        // the vast majority of items.
                        if let Some(spr) = atlas.sprite(&layer.sprite) {
                            push_sprite_quad(
                                sink.sprite,
                                vw,
                                vh,
                                GuiSpriteQuad {
                                    dst: [x, y, size, size],
                                    uv_min: spr.uv_min,
                                    uv_max: spr.uv_max,
                                },
                                [1.0, 1.0, 1.0, 1.0],
                            );
                        }
                    }
                }
                // The part's own `model`/`transform`/`gui_light` are *not* used
                // here: `BlockModels::build` resolved and baked exactly this
                // part (through the same `GuiItemContext`) and stored the
                // transform alongside the quads, so the item id is the whole
                // key. Going back to the part would risk the two disagreeing.
                IconPart::Model { .. } => {
                    push_item_model(sink.model, assets.models, &slot.item, x, y, size);
                }
                IconPart::Special { .. } => {}
            }
        }
    }

    // Durability bar: a 13px track 2px above the slot bottom, filled by the
    // remaining fraction, hue lerped green→red. Vanilla only draws it for a
    // damaged item, so a pristine tool shows none.
    if let (Some(dmg), Some(max)) = (slot.damage, slot.max_damage)
        && dmg > 0
        && max > 0
    {
        let remaining = 1.0 - (dmg.min(max) as f32 / max as f32);
        let bx = x + 2.0 * scale;
        let bw = 13.0 * scale;
        let by = y + size - 3.0 * scale;
        let bh = 2.0 * scale;
        sink.colour.rect(bx, by, bw, bh, [0.0, 0.0, 0.0, 1.0]);
        let col = [1.0 - remaining, remaining, 0.0, 1.0];
        sink.colour
            .rect(bx, by, (bw * remaining).max(1.0 * scale), bh, col);
    }

    // Stack count, bottom-right, on the colour stream so it lands on top of the
    // icon. A 1px drop shadow mirrors vanilla's number rendering.
    if slot.count > 1 {
        let s = slot.count.to_string();
        let tscale = scale * 2.0;
        let tw = text_w(&s, tscale);
        let tx = x + size - tw;
        let ty = y + size - font::GLYPH_H as f32 * tscale;
        sink.colour
            .text(&s, tx + tscale, ty + tscale, tscale, [0.0, 0.0, 0.0, 1.0]);
        sink.colour.text(&s, tx, ty, tscale, [1.0, 1.0, 1.0, 1.0]);
    }
}

/// Emit the 3-D isometric mini-block for a **block** item into the `size`×`size`
/// pixel rect at `(x, y)`.
///
/// The geometry was baked once at asset-load time against the *block* atlas and
/// interned through the *block* tint palette, so nothing is resolved here: the
/// pose is `gui_item_pose` over the stored `display.gui` transform, and
/// [`mesh_item_quads`] applies it, fixes the light full-bright, and puts
/// `gui_light` in the `ao` slot. The result is already in **GUI pixel space** (x
/// right, y down, z toward the viewer) — the pass's `view_proj` is
/// [`gui_ortho`], which finishes the job.
///
/// Indices are expanded into a flat triangle list because the other two streams
/// are non-indexed; the expansion preserves the mesh's winding, which is
/// load-bearing (see `docs/item-gui-geometry.md`: the visible faces are the ones
/// back-face culling keeps, and they must also be the nearest under the depth
/// test). A no-op when no model set is attached or the item has no baked
/// geometry, so jar-less runs keep the old empty well.
fn push_item_model(
    out: &mut Vec<ModelVertex>,
    models: Option<&BlockModels>,
    item: &ResourceLocation,
    x: f32,
    y: f32,
    size: f32,
) {
    let Some(models) = models else {
        return;
    };
    let Some(geometry) = models.item(item) else {
        return;
    };
    let pose = gui_item_pose([x, y, size, size], &geometry.transform);
    let mesh = mesh_item_quads(&geometry.quads, pose, geometry.gui_light);
    out.reserve(mesh.indices.len());
    for &i in &mesh.indices {
        out.push(mesh.vertices[i as usize]);
    }
}

/// Push one textured quad (two triangles) from an absolute-pixel destination
/// rect and its atlas UVs, tinted by `c`, into a `SPRITE_FLOATS_PER_VERTEX`
/// stream. Shared by the item atlas and the GUI atlas: the vertex layout is the
/// same, only the bound texture differs.
pub(crate) fn push_sprite_quad(
    verts: &mut Vec<f32>,
    vw: f32,
    vh: f32,
    q: GuiSpriteQuad,
    c: [f32; 4],
) {
    let to_ndc = |px: f32, py: f32| (2.0 * px / vw - 1.0, 1.0 - 2.0 * py / vh);
    let [dx, dy, dw, dh] = q.dst;
    let (x0, y0) = to_ndc(dx, dy);
    let (x1, y1) = to_ndc(dx + dw, dy + dh);
    let [u0, v0] = q.uv_min;
    let [u1, v1] = q.uv_max;
    let mut v = |vx: f32, vy: f32, tu: f32, tv: f32| {
        verts.extend_from_slice(&[vx, vy, tu, tv, c[0], c[1], c[2], c[3]]);
    };
    v(x0, y0, u0, v0);
    v(x1, y0, u1, v0);
    v(x1, y1, u1, v1);
    v(x0, y0, u0, v0);
    v(x1, y1, u1, v1);
    v(x0, y1, u0, v1);
}

/// A borrowed handle onto a screen's **colour** vertex stream plus the viewport
/// it is measured against — the three values every pixel-space primitive needs.
/// Bundled so `rect`/`text`/`glyph` take their own arguments and not the
/// stream's, and so the two screens' `Builder`s share one implementation of each.
pub(crate) struct ColourStream<'a> {
    /// Flat `[x, y, r, g, b, a]` per vertex, positions in NDC.
    pub verts: &'a mut Vec<f32>,
    /// Viewport width in pixels.
    pub w: f32,
    /// Viewport height in pixels.
    pub h: f32,
}

impl ColourStream<'_> {
    /// Emit a pixel-space rectangle as two triangles in NDC.
    pub(crate) fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        debug_assert_eq!(FLOATS_PER_VERTEX, 6);
        let to_ndc = |px: f32, py: f32| (2.0 * px / self.w - 1.0, 1.0 - 2.0 * py / self.h);
        let (x0, y0) = to_ndc(x, y);
        let (x1, y1) = to_ndc(x + w, y + h);
        let verts = &mut *self.verts;
        let mut v = |vx: f32, vy: f32| {
            verts.extend_from_slice(&[vx, vy, c[0], c[1], c[2], c[3]]);
        };
        v(x0, y0);
        v(x1, y0);
        v(x1, y1);
        v(x0, y0);
        v(x1, y1);
        v(x0, y1);
    }

    /// Draw a single glyph with its top-left at `(x, y)`. Space and unknown
    /// handling match [`font::glyph_rows`]; blanks emit no quads.
    pub(crate) fn glyph(&mut self, ch: char, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        if ch == ' ' {
            return;
        }
        let rows = font::glyph_rows(ch);
        for (ry, row) in rows.iter().enumerate() {
            for rx in 0..font::GLYPH_W {
                let bit = (row >> (font::GLYPH_W - 1 - rx)) & 1;
                if bit == 1 {
                    self.rect(
                        x + rx as f32 * scale,
                        y + ry as f32 * scale,
                        scale,
                        scale,
                        c,
                    );
                }
            }
        }
    }

    /// Emit a string starting at pixel `(x, y)` (top-left of the first glyph).
    pub(crate) fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        let advance = (font::GLYPH_W as f32 + 1.0) * scale;
        let mut cursor = x;
        for ch in s.chars() {
            self.glyph(ch, cursor, y, scale, c);
            cursor += advance;
        }
    }
}

/// Width in pixels of `s` at `scale` in the shell's fixed-width bitmap font.
pub(crate) fn text_w(s: &str, scale: f32) -> f32 {
    s.chars().count() as f32 * (font::GLYPH_W as f32 + 1.0) * scale
}

/// The colour attachment every GUI overlay pass uses: load what was already
/// drawn, store what we add. A free function rather than a closure because each
/// pass needs the borrow of `view` to end with its own `RenderPassDescriptor`.
pub(crate) fn load_colour_attachment(
    view: &wgpu::TextureView,
) -> wgpu::RenderPassColorAttachment<'_> {
    wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Load,
            store: wgpu::StoreOp::Store,
        },
    }
}

/// The GPU resources for drawing item icons from the flat [`ItemAtlas`]: the
/// uploaded item-sprite atlas, its textured pipeline + bind group, and a dynamic
/// vertex buffer.
#[derive(Debug)]
struct SpriteIcons {
    atlas: Arc<ItemAtlas>,
    #[allow(dead_code)]
    gpu: GpuAtlas,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
}

/// The GPU resources for drawing **3-D block items** in GUI slots.
///
/// Unlike [`SpriteIcons`] this owns no texture: it reuses the world's
/// [`ModelPipeline`] and binds the *same* block atlas, tint palette and
/// animation slots the terrain pass does. Four bind groups is exactly what the
/// model shader declares — camera (0), atlas (1), palette (2), animation (3) —
/// and `wgpu`'s portable `max_bind_groups` floor is **4**, so there is no room
/// for a fifth. That is why the GUI camera and its (disabled) fog share group 0
/// as a single [`ModelCameraUniform`], and why nothing here introduces a new
/// group: a five-group variant validates on an adapter that reports 8 and fails
/// on the floor, which is a bug no local screenshot can find.
///
/// The three sharings are each load-bearing rather than merely tidy:
///
/// * **atlas** — a block item's faces *are* block textures; a second upload would
///   cost tens of megabytes to draw a handful of 16 px icons;
/// * **palette** — a grass block's slot icon and the world block resolve to the
///   same slot, so they cannot drift to different greens;
/// * **animation slots** — magma, sea lantern and prismarine icons advance in
///   lock-step with the world, for free, off the one per-frame uniform write.
#[derive(Debug)]
struct ModelIcons {
    pipeline: ModelPipeline,
    /// Group 0: the GUI orthographic `view_proj` with a zero section origin and
    /// fog disabled. Rewritten each frame because it depends on the target size.
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    /// Group 1: the shared block atlas (view + sampler borrowed at attach time;
    /// the bind group holds its own strong reference).
    atlas_bind_group: wgpu::BindGroup,
    /// Group 2: the shared tint palette.
    palette_bind_group: wgpu::BindGroup,
    /// Group 3: the shared per-slot animation uniforms.
    anim_bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    capacity_bytes: usize,
}

/// The GPU half of the item-icon pass, held by every screen that draws slots.
///
/// Both halves start detached. `attach_items` gives flat icons somewhere to
/// draw; `attach_item_models` gives block icons somewhere to draw. Neither is
/// required, and a renderer with neither draws no icons at all — the jar-less
/// runtime behaviour and the executed negative control in both pixel gates.
#[derive(Debug, Default)]
pub(crate) struct IconRenderer {
    sprites: Option<SpriteIcons>,
    models: Option<ModelIcons>,
}

impl IconRenderer {
    /// A detached renderer: no atlas, no model pass, no icons.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The attached flat item atlas, if any. Cloned (an `Arc`) so the caller can
    /// hand it to the CPU builder without holding a borrow of `self`.
    pub(crate) fn item_atlas(&self) -> Option<Arc<ItemAtlas>> {
        self.sprites.as_ref().map(|s| Arc::clone(&s.atlas))
    }

    /// Whether the 3-D item-model pass is attached. Building model geometry when
    /// it is not is pure waste — there is nowhere to draw it.
    pub(crate) fn models_attached(&self) -> bool {
        self.models.is_some()
    }

    /// Attach the flat item-sprite [`ItemAtlas`] so slots draw real item icons.
    /// Uploads the atlas texture, builds a textured pipeline, and binds it.
    pub(crate) fn attach_items(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        atlas: Arc<ItemAtlas>,
        label: &'static str,
    ) {
        let gpu = GpuAtlas::from_atlas(device, queue, atlas.atlas());
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(HUD_SPRITE_WGSL.into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (SPRITE_FLOATS_PER_VERTEX * 4) as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 2,
                        },
                    ],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&gpu.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                },
            ],
        });
        let capacity_floats = 4096;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.sprites = Some(SpriteIcons {
            atlas,
            gpu,
            pipeline,
            bind_group,
            buffer,
            capacity_floats,
        });
    }

    /// Attach the GPU side of the **3-D block-item** icon pass, so slots holding
    /// a block draw vanilla's isometric mini-block instead of an empty well.
    ///
    /// Every resource is *borrowed from the world renderer* rather than created:
    /// `atlas_view`/`atlas_sampler` are the stitched block atlas
    /// ([`RenderState::model_atlas_view`](crate::gpu::RenderState::model_atlas_view)),
    /// `palette` its tint palette, `anim` its per-slot animation uniforms. See
    /// [`ModelIcons`] for why each of those sharings matters. `wgpu` resources
    /// are `Arc`-backed and a bind group keeps its own strong reference, so the
    /// borrows need not outlive this call.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn attach_item_models(
        &mut self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
        palette: &wgpu::Buffer,
        anim: &wgpu::Buffer,
        label: &'static str,
    ) {
        // `Solid` (not `Translucent`): depth writes on, back-face culling on with
        // `FrontFace::Ccw`. Both are required — culling is what removes the three
        // far faces of the cube, and the depth test is what stops the near ones
        // being overdrawn by them. The shader's cutout discard also drops
        // near-transparent texels, so a cross-shaped model does not paint a box.
        let pipeline = ModelPipeline::for_layer(device, color_format, RenderLayer::Solid);
        // A placeholder view_proj; `upload` rewrites it from the live target size
        // before every draw. `model_camera_buffer` sizes the buffer for the
        // camera **and** the folded fog block, and writes fog disabled.
        let camera_buffer = model_camera_buffer(device, CameraUniform {
            view_proj: [[0.0; 4]; 4],
            section_origin: [0.0; 4],
        });
        let camera_bind_group = pipeline.camera_bind_group(device, &camera_buffer);
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &pipeline.atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(atlas_sampler),
                },
            ],
        });
        let palette_bind_group = pipeline.palette_bind_group(device, palette);
        let anim_bind_group = pipeline.anim_bind_group(device, anim);
        let capacity_bytes = 16 * 1024;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: capacity_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.models = Some(ModelIcons {
            pipeline,
            camera_buffer,
            camera_bind_group,
            atlas_bind_group,
            palette_bind_group,
            anim_bind_group,
            buffer,
            capacity_bytes,
        });
    }

    /// Grow the buffers as needed, upload both streams, and rewrite the model
    /// pass's GUI camera for the current target size. Returns the
    /// `(sprite, model)` vertex counts the caller should draw — **zero** for a
    /// stream whose half is not attached, so the draws can be issued
    /// unconditionally.
    ///
    /// `gui_ortho` is the whole projection for the model stream: the vertices
    /// are already posed into GUI pixel space, the section origin is zero (they
    /// are not section-local), and fog is disabled (an inventory slot is not in
    /// the world).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sprite_verts: &[f32],
        model_verts: &[ModelVertex],
        width: u32,
        height: u32,
        label: &'static str,
    ) -> (u32, u32) {
        let mut sprite_count = 0;
        let mut model_count = 0;

        if !sprite_verts.is_empty()
            && let Some(s) = self.sprites.as_mut()
        {
            if sprite_verts.len() > s.capacity_floats {
                s.capacity_floats = sprite_verts.len().next_power_of_two();
                s.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: (s.capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&s.buffer, 0, bytemuck::cast_slice(sprite_verts));
            sprite_count = (sprite_verts.len() / SPRITE_FLOATS_PER_VERTEX) as u32;
        }

        if !model_verts.is_empty()
            && let Some(m) = self.models.as_mut()
        {
            let bytes = std::mem::size_of_val(model_verts);
            if bytes > m.capacity_bytes {
                m.capacity_bytes = bytes.next_power_of_two();
                m.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: m.capacity_bytes as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&m.buffer, 0, bytemuck::cast_slice(model_verts));
            queue.write_buffer(
                &m.camera_buffer,
                0,
                bytemuck::bytes_of(&ModelCameraUniform {
                    camera: CameraUniform {
                        view_proj: gui_ortho(width, height).to_cols_array_2d(),
                        section_origin: [0.0; 4],
                    },
                    fog: lodestone_render::fog::FogUniform::disabled(),
                }),
            );
            model_count = model_verts.len() as u32;
        }

        (sprite_count, model_count)
    }

    /// Record the flat item-sprite draw into an **already-open** pass, so the
    /// caller can keep icons in the same pass as its other 2-D chrome. A no-op
    /// when `count` is zero or the atlas is not attached.
    pub(crate) fn draw_sprites(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
        if count == 0 {
            return;
        }
        let Some(s) = &self.sprites else {
            return;
        };
        pass.set_pipeline(&s.pipeline);
        pass.set_bind_group(0, &s.bind_group, &[]);
        pass.set_vertex_buffer(0, s.buffer.slice(..));
        pass.draw(0..count, 0..1);
    }

    /// Record the 3-D block-item pass. It gets its **own** pass because it is
    /// the only part of a GUI overlay that needs a depth buffer, and it *clears*
    /// depth rather than loading it: the world's depth is still resident from
    /// the terrain pass and would occlude a GUI item sitting at clip depth ~0.5.
    /// Nothing later in the frame reads depth, so clearing it here is free.
    ///
    /// A no-op when `count` is zero, the pass is not attached, or the caller has
    /// no depth attachment to lend.
    pub(crate) fn draw_models(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        count: u32,
        label: &'static str,
    ) {
        if count == 0 {
            return;
        }
        let (Some(m), Some(depth_view)) = (&self.models, depth) else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(load_colour_attachment(view))],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&m.pipeline.pipeline);
        pass.set_bind_group(0, &m.camera_bind_group, &[]);
        pass.set_bind_group(1, &m.atlas_bind_group, &[]);
        pass.set_bind_group(2, &m.palette_bind_group, &[]);
        pass.set_bind_group(3, &m.anim_bind_group, &[]);
        pass.set_vertex_buffer(0, m.buffer.slice(..));
        pass.draw(0..count, 0..1);
    }
}
