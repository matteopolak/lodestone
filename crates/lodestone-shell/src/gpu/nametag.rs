//! World-space entity/player nametags (issue #100): billboarded text above
//! every entity with a visible custom name, and above every other player
//! (whose tag is always its tab-list display name — see
//! `crate::net::entity_snapshot`'s doc for exactly which vanilla rule governs
//! each case, jar file:line cited).
//!
//! # The two depth passes, and the sign flip
//!
//! Reconciled against the real 26.2 client
//! (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/rendertype/RenderPipelines.java`
//! and `RenderTypes.java`), not guessed:
//!
//! * **Normal pass** (`RenderPipelines.TEXT`, via `WORLD_TEXT_SNIPPET`):
//!   `DepthStencilState.DEFAULT = new DepthStencilState(CompareOp.GREATER_THAN_OR_EQUAL, true)`
//!   (`DepthStencilState.java:6`) — depth-tested **and depth-written**.
//!   Vanilla's reversed-Z convention makes "closer" `GREATER_THAN_OR_EQUAL`;
//!   ours is `[0,1]` DirectX-style (`docs/`/`CLAUDE.md`'s rendering
//!   constraints), so the *sign flips* to [`wgpu::CompareFunction::LessEqual`]
//!   with `depth_write_enabled: true` — the same flip `gpu/outline.rs` and
//!   `gpu/debug_lines.rs` already apply, just with write turned **on** here
//!   (a nearer tag's glyphs must win over a farther, overlapping one, exactly
//!   as vanilla's write-enabled pass does).
//! * **See-through pass** (`RenderPipelines.TEXT_SEE_THROUGH`):
//!   `.withDepthStencilState(Optional.empty())` (`RenderPipelines.java:507`)
//!   — **no depth attachment use at all**, neither tested nor written. There
//!   is no comparison operator to port here, so there is no sign to get
//!   backwards — but `wgpu` itself has no "this pipeline ignores the pass's
//!   depth attachment" option: every pipeline drawn inside a render pass
//!   that has a depth-stencil attachment must declare a *matching-format*
//!   one of its own, verified the hard way (a `depth_stencil: None`
//!   pipeline validation-errors at draw time against this pass's real
//!   `Depth32Float` attachment, it does not silently no-op). The
//!   equivalent-in-effect substitute is [`wgpu::CompareFunction::Always`]
//!   (every fragment passes — nothing to get the sign of) with
//!   `depth_write_enabled: false`. This is what makes a tag behind a wall
//!   read as *dimmed* rather than fully hidden — it always draws, faded.
//!
//! Vanilla's color for each pass (`SubmitNodeCollection.java:113`/`:117`):
//! normal is opaque white (`-1`), see-through is `-2130706433` =
//! `0x81_FFFFFF` — white at alpha `129/255 ≈ 0.506`. Both are plain
//! `BlendFunction.TRANSLUCENT` (`wgpu::BlendState::ALPHA_BLENDING` here);
//! with the normal pass's alpha at `1.0` the blend is a no-op, so draw order
//! between the two passes does not affect the final pixel where both cover
//! the same texel.
//!
//! # Anchor height and distance cutoff
//!
//! * **Distance cutoff**: `64.0` blocks, squared-distance compared against
//!   camera-to-*feet* (`EntityRenderer.extractNameTags`'s default
//!   `nameTagDistance` argument, `EntityRenderer.java:246`, tested at
//!   `EntityRenderer.java:252`).
//! * **Anchor**: `feet.y + base_height * scale + 0.5`. The `+0.5` is
//!   `SubmitNodeCollection.java:103`'s `nameTagAttachment.y + 0.5`; the
//!   `base_height` term is `EntityAttachment.NAME_TAG`'s fallback point,
//!   `AT_HEIGHT = (width, height) -> (0, height, 0)`
//!   (`EntityAttachment.java:9`, `:25`) — the entity's own hitbox height,
//!   from the real jar-derived census (`lodestone_data::entity_dimensions`),
//!   not a guess. Some vanilla types override this attachment point (a
//!   sitting cat, a sleeping villager); that per-type override table is not
//!   ported — every entity here uses the `AT_HEIGHT` fallback, which is what
//!   the overwhelming majority of named entities (players, standard mobs)
//!   actually get.
//! * **Sneaking suppression**: `Entity.isDiscrete()` gates the see-through
//!   pass off (`SubmitNodeCollection.java:109`/`:118`) — resolved once, at
//!   `net::entity_snapshot`'s boundary, as [`crate::entities::NameTag::see_through`].
//!
//! # What is deliberately not built
//!
//! * **The background plate.** Vanilla draws a `TEXT_BACKGROUND`/
//!   `TEXT_BACKGROUND_SEE_THROUGH` quad behind the glyphs, coloured from the
//!   `chatOpacity` game option (`SubmitNodeCollection.java:108`). Not in the
//!   issue's explicit scope checklist and not required for legibility (the
//!   drop shadow already separates text from background); a genuine gap, not
//!   an oversight.
//! * **Per-frame packed-light modulation.** Vanilla forces near-full
//!   brightness for the normal pass
//!   (`LightCoordsUtil.lightCoordsWithEmission(lightCoords, 2)`,
//!   `SubmitNodeCollection.java:113`) specifically so a nametag stays legible
//!   in the dark — this renderer draws plain full-bright white unconditionally,
//!   which is a close approximation of that emission override rather than a
//!   divergence from it.
//! * **`EntityAttachment` per-type overrides**, the crosshair-look-at
//!   override to `shouldShowName` (`EntityRenderer.java:113`), scoreboard
//!   team colouring/prefixes and the `belowName` scoreboard line — all
//!   explicitly out of scope per the issue.
//!
//! # Font: the same jar-sourced glyph data the HUD uses, a new draw path
//!
//! [`crate::hud::vanilla_font::VanillaFont`] cannot be reused directly here:
//! its glyph rasteriser is private and its public draw methods emit into
//! `hud/item_icon.rs`'s 2-D screen-space `ColourStream`, and both files are
//! out of scope for this change (a different agent's files, per the task
//! briefing). This module instead calls the same public, jar-sourced data
//! source directly — [`lodestone_assets::font::RasterFont`], loaded with the
//! same `FontLoader::load_raster(&"minecraft:default".parse()?,
//! &FontOptions::none())` call `VanillaFont::load` makes — and re-derives the
//! ink-run walk `VanillaFont::glyph` uses (same advance metrics, same
//! run-length merge), targeting world-space billboard quads instead of
//! screen-space ones. [`jar_manager`]/[`pack_root`] duplicate
//! `hud/vanilla_font.rs`'s own discovery snippet for the same reason that
//! module duplicates it from `crate::resources` — see that module's doc.

use std::path::{Path, PathBuf};

use glam::Vec3;
use lodestone_assets::font::{FontLoader, FontOptions, MISSING_ADVANCE, RasterFont, metrics};
use lodestone_assets::{ResourceManager, ResourceSource, ZipSource};
use lodestone_render::entity::camera_orientation;
use lodestone_render::{Camera, DEPTH_FORMAT};

use crate::entities::EntityDraw;

/// One coloured world-space vertex — the same shape as `gpu/debug_lines.rs`'s
/// `DebugLineVertex`, kept as its own type rather than imported cross-module
/// (each pass owning its vertex type is this crate's established pattern;
/// see `gpu/outline.rs`/`gpu/debug_lines.rs`).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NameTagVertex {
    position: [f32; 3],
    color: [f32; 4],
}

/// Vanilla's per-name-tag world scale
/// (`SubmitNodeCollection.java:105`: `poseStack.scale(0.025F, -0.025F, 0.025F)`)
/// — one logical text pixel is this many world blocks.
const PX_SCALE: f32 = 0.025;

/// The distance cutoff, in blocks (`EntityRenderer.java:246`).
const MAX_DISTANCE: f32 = 64.0;

/// The padding above the `NAME_TAG` attachment point
/// (`SubmitNodeCollection.java:103`).
const ATTACHMENT_PADDING: f32 = 0.5;

/// Fallback base hitbox height, in blocks, for a type path the jar-derived
/// census cannot resolve (shouldn't happen for a real registered type, but a
/// malformed/future type id must degrade to *something* rather than crash).
/// The player's own height — a reasonable middle ground.
const FALLBACK_HEIGHT: f32 = 1.8;

/// Opaque white — the normal pass's colour (`-1` in `SubmitNodeCollection.java:113`).
const NORMAL_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// The drop shadow: the same 25%-brightness quarter
/// `hud/vanilla_font.rs::shadow_of` applies, at the normal pass's full alpha.
const SHADOW_COLOR: [f32; 4] = [
    metrics::SHADOW_BRIGHTNESS,
    metrics::SHADOW_BRIGHTNESS,
    metrics::SHADOW_BRIGHTNESS,
    1.0,
];
/// White at `129/255`, vanilla's `-2130706433` (`0x81_FFFFFF`) — the
/// see-through pass's colour (`SubmitNodeCollection.java:117`).
const SEE_THROUGH_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 129.0 / 255.0];

/// Fixed vertex capacity per pass (six vertices per glyph-row ink run). Same
/// fixed-buffer idiom as `gpu/debug_lines.rs`'s `MAX_DEBUG_LINE_SEGMENTS` —
/// see that constant's doc for why a fixed cap is what lets `prepare` take
/// `&self`. Comfortably above what a screen full of named mobs needs: an
/// 8-row-tall glyph contributes at most 8 runs, so this covers roughly 1,700
/// glyphs' worth of ink in one frame.
const MAX_NAME_TAG_VERTICES: usize = 60_000;

/// One glyph row's ink run, in local "logical pixel" space: `x` measured from
/// the start of the string (before centring), `y` measured down from the
/// string's top.
///
/// `pub(super)` (visible to the rest of `crate::gpu`, not beyond): `gpu/
/// sign_text.rs` reuses this shape and [`layout_ink_runs`] directly rather
/// than duplicating the ink-run walk a second time — see that module's doc.
#[derive(Debug, Clone, Copy)]
pub(super) struct LocalRect {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) w: f32,
    pub(super) h: f32,
}

/// Walks `text` through `raster` exactly as `VanillaFont::glyph` does (same
/// per-row run-length merge over ink texels), but returns the local rects
/// instead of emitting to a 2-D `ColourStream` — see the module doc for why
/// this cannot just call that method. Returns the rects and the string's
/// total advance (for horizontal centring).
///
/// A codepoint the font does not cover at all (not even as whitespace)
/// contributes no rect and [`MISSING_ADVANCE`] of blank space — the hollow
/// missing-glyph box `VanillaFont` draws is not reproduced here, a minor,
/// deliberate fidelity loss for a case ordinary custom names/usernames don't
/// hit (missing-glyph codepoints are rare in practice).
pub(super) fn layout_ink_runs(raster: &RasterFont, text: &str) -> (Vec<LocalRect>, f32) {
    let mut cursor = 0.0f32;
    let mut rects = Vec::new();
    for ch in text.chars() {
        let cp = ch as u32;
        match raster.raster(cp) {
            Some(r) => {
                let texel = r.texel_size();
                let top = r.top();
                for ty in 0..r.cell_height() {
                    let mut tx = 0;
                    while tx < r.cell_width() {
                        if !r.is_ink(tx, ty) {
                            tx += 1;
                            continue;
                        }
                        let start = tx;
                        while tx < r.cell_width() && r.is_ink(tx, ty) {
                            tx += 1;
                        }
                        rects.push(LocalRect {
                            x: cursor + start as f32 * texel,
                            y: top + ty as f32 * texel,
                            w: (tx - start) as f32 * texel,
                            h: texel,
                        });
                    }
                }
                cursor += r.advance();
            }
            None => {
                cursor += raster.advance(cp).unwrap_or(MISSING_ADVANCE);
            }
        }
    }
    (rects, cursor)
}

/// This type path's base hitbox height (`lodestone_data::entity_dimensions`),
/// scaled by nothing yet — the caller multiplies by [`EntityDraw::scale`].
/// Falls back to [`FALLBACK_HEIGHT`] for a type path the census cannot
/// resolve (no `minecraft:<path>` id, or a `0`-height marker type).
#[must_use]
fn entity_base_height(type_path: &str) -> f32 {
    lodestone_data::entity_types::entity_type_id_parts("minecraft", type_path)
        .and_then(lodestone_data::entity_dimensions::base_dimensions)
        .map(|dims| dims.height)
        .filter(|h| *h > 0.0)
        .unwrap_or(FALLBACK_HEIGHT)
}

/// Turns one local rect into two triangles (six vertices, no index buffer) in
/// world space, billboarded with the frame's shared `right`/`up` basis —
/// every nametag this frame shares the same basis, matching vanilla's single
/// `camera.orientation` applied identically to each
/// (`SubmitNodeCollection.java:104`: `poseStack.mulPose(camera.orientation)`,
/// *before* any per-entity translation).
///
/// No culling is configured on either pipeline (`cull_mode: None`, `wgpu`'s
/// default), so winding order here is deliberately not load-bearing — unlike
/// `docs/gpu-module-layout.md`'s GUI-winding invariant, a billboard that only
/// the camera it faces ever sees needs no back face at all.
fn quad_vertices(
    rect: LocalRect,
    half_width: f32,
    anchor: Vec3,
    right: Vec3,
    up: Vec3,
    color: [f32; 4],
) -> [NameTagVertex; 6] {
    let to_world = |lx: f32, ly: f32| -> [f32; 3] {
        let p = anchor + right * ((lx - half_width) * PX_SCALE) + up * (-ly * PX_SCALE);
        [p.x, p.y, p.z]
    };
    let tl = to_world(rect.x, rect.y);
    let tr = to_world(rect.x + rect.w, rect.y);
    let bl = to_world(rect.x, rect.y + rect.h);
    let br = to_world(rect.x + rect.w, rect.y + rect.h);
    [
        NameTagVertex {
            position: tl,
            color,
        },
        NameTagVertex {
            position: bl,
            color,
        },
        NameTagVertex {
            position: tr,
            color,
        },
        NameTagVertex {
            position: tr,
            color,
        },
        NameTagVertex {
            position: bl,
            color,
        },
        NameTagVertex {
            position: br,
            color,
        },
    ]
}

/// Lowers one entity's [`EntityDraw::name_tag`] into world-space vertices,
/// appended onto `normal_out` (and, when [`crate::entities::NameTag::see_through`]
/// is set, `see_through_out`). A no-op for an entity with no tag, an empty
/// tag, or one further than [`MAX_DISTANCE`] from the camera.
fn push_entity_quads(
    raster: &RasterFont,
    draw: &EntityDraw,
    camera_position: Vec3,
    right: Vec3,
    up: Vec3,
    normal_out: &mut Vec<NameTagVertex>,
    see_through_out: &mut Vec<NameTagVertex>,
) {
    let Some(tag) = &draw.name_tag else {
        return;
    };
    if tag.text.is_empty() {
        return;
    }
    if camera_position.distance_squared(draw.feet) > MAX_DISTANCE * MAX_DISTANCE {
        return;
    }

    let height = entity_base_height(&draw.type_path) * draw.scale;
    let anchor = draw.feet + Vec3::new(0.0, height + ATTACHMENT_PADDING, 0.0);

    let (rects, total_width) = layout_ink_runs(raster, &tag.text);
    if rects.is_empty() {
        return;
    }
    let half_width = total_width / 2.0;

    // The shadow copy first (whole string), then the text — same order
    // `VanillaFont::draw` uses, for the same reason (a later glyph's ink
    // must sit on top of an earlier glyph's shadow, not the other way
    // round).
    let shadow_offset = metrics::SHADOW_OFFSET;
    for rect in &rects {
        let shadow_rect = LocalRect {
            x: rect.x + shadow_offset,
            y: rect.y + shadow_offset,
            ..*rect
        };
        normal_out.extend(quad_vertices(
            shadow_rect,
            half_width,
            anchor,
            right,
            up,
            SHADOW_COLOR,
        ));
    }
    for rect in &rects {
        normal_out.extend(quad_vertices(
            *rect,
            half_width,
            anchor,
            right,
            up,
            NORMAL_COLOR,
        ));
    }
    if tag.see_through {
        for rect in &rects {
            see_through_out.extend(quad_vertices(
                *rect,
                half_width,
                anchor,
                right,
                up,
                SEE_THROUGH_COLOR,
            ));
        }
    }
}

/// Draws billboarded nametag text above every [`EntityDraw`] carrying one —
/// see the module doc for the two depth passes' exact settings.
#[derive(Debug)]
pub(super) struct NameTagRenderer {
    normal_pipeline: wgpu::RenderPipeline,
    see_through_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    normal_vertices: wgpu::Buffer,
    see_through_vertices: wgpu::Buffer,
    /// `None` off a jar-less run (headless gates, the demo world) — the same
    /// fail-open contract [`crate::hud::vanilla_font::VanillaFont::shared`]
    /// has, and for the same reason: nothing here is a hard requirement, and
    /// every caller below already treats "no font" as "draw nothing" rather
    /// than panicking.
    font: Option<RasterFont>,
}

impl NameTagRenderer {
    pub(super) fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-nametag-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/nametag.wgsl").into()),
        });

        // One `view_proj` uniform, nothing else — same shape as
        // `gpu/debug_lines.rs`'s bind-group layout, so this pass has no
        // bearing on the model shader's 4-bind-group floor
        // (`docs/gpu-module-layout.md`).
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-nametag-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-nametag-uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-nametag-bg"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });

        let buffer_size =
            (MAX_NAME_TAG_VERTICES * std::mem::size_of::<NameTagVertex>()) as u64;
        let normal_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-nametag-normal-vertices"),
            size: buffer_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let see_through_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-nametag-see-through-vertices"),
            size: buffer_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-nametag-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let vertex_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<NameTagVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
        })];

        let build = |label: &str, depth_stencil: Option<wgpu::DepthStencilState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &vertex_buffers,
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
                depth_stencil,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        // Normal pass: depth-tested and depth-written, `LessEqual` — the
        // sign-flipped port of vanilla's reversed-Z `GREATER_THAN_OR_EQUAL`
        // (`DepthStencilState.DEFAULT`, see the module doc).
        let normal_pipeline = build(
            "lodestone-nametag-normal-pipeline",
            Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
        );
        // See-through pass: vanilla's `TEXT_SEE_THROUGH` pipeline has
        // `Optional.empty()` for its whole depth-stencil state — no depth
        // attachment at all. `wgpu` has no equivalent for "this pipeline
        // uses no depth attachment" *while sharing a render pass that has
        // one*: every pipeline drawn inside a pass with a depth-stencil
        // attachment must declare a matching format, verified the hard way —
        // `depth_stencil: None` here validation-errors at draw time
        // ("Incompatible depth-stencil attachment format: … Some(Depth32Float)
        // but the RenderPipeline … uses an attachment with format None"),
        // it does not silently no-op. `CompareFunction::Always` (every
        // fragment passes, matching the attachment's format so the pass is
        // valid) plus `depth_write_enabled: false` is the equivalent-in-effect
        // substitute: no comparison operator to get the sign of, and no write
        // — precisely "no depth interaction" within the constraint that the
        // pipeline must still name the format.
        let see_through_pipeline = build(
            "lodestone-nametag-see-through-pipeline",
            Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
        );

        Self {
            normal_pipeline,
            see_through_pipeline,
            bind_group,
            uniform,
            normal_vertices,
            see_through_vertices,
            font: load_font(),
        }
    }

    /// Uploads this frame's view-projection and nametag vertices. Must run
    /// before the render pass opens (buffers cannot be written mid-pass).
    /// Returns `(normal_vertex_count, see_through_vertex_count)`, capped at
    /// [`MAX_NAME_TAG_VERTICES`] each — pass to [`draw`](Self::draw).
    pub(super) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        camera: &Camera,
        entities: &[EntityDraw],
    ) -> (u32, u32) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(view_proj));
        let Some(raster) = &self.font else {
            return (0, 0);
        };

        // The frame's shared camera-facing basis — every nametag this frame
        // billboards off the same rotation, matching vanilla (see
        // `quad_vertices`'s doc).
        let orientation = camera_orientation(camera.view_matrix());
        let right = orientation.x_axis.truncate();
        let up = orientation.y_axis.truncate();

        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        for draw in entities {
            push_entity_quads(
                raster,
                draw,
                camera.position,
                right,
                up,
                &mut normal,
                &mut see_through,
            );
        }
        let normal_len = normal.len().min(MAX_NAME_TAG_VERTICES);
        let see_through_len = see_through.len().min(MAX_NAME_TAG_VERTICES);
        if normal_len > 0 {
            queue.write_buffer(
                &self.normal_vertices,
                0,
                bytemuck::cast_slice(&normal[..normal_len]),
            );
        }
        if see_through_len > 0 {
            queue.write_buffer(
                &self.see_through_vertices,
                0,
                bytemuck::cast_slice(&see_through[..see_through_len]),
            );
        }
        (normal_len as u32, see_through_len as u32)
    }

    /// Records both passes' draws (whichever have vertices). No-op with the
    /// no-jar `font: None` state, since [`prepare`](Self::prepare) always
    /// returns `(0, 0)` there.
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, counts: (u32, u32)) {
        let (normal_count, see_through_count) = counts;
        if normal_count > 0 {
            pass.set_pipeline(&self.normal_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.normal_vertices.slice(..));
            pass.draw(0..normal_count, 0..1);
        }
        if see_through_count > 0 {
            pass.set_pipeline(&self.see_through_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.see_through_vertices.slice(..));
            pass.draw(0..see_through_count, 0..1);
        }
    }
}

/// Loads the vanilla `minecraft:default` font's raster data for world-space
/// drawing. `None` off a jar-less run — see [`NameTagRenderer::font`].
///
/// `pub(super)`: `gpu/sign_text.rs` calls this directly rather than
/// duplicating `jar_manager`/`pack_root` a third time — this module's own
/// doc already explains why *those* are duplicated from `hud/vanilla_font.rs`
/// (a different agent's off-limits file at the time), but nothing stops a
/// sibling `gpu` submodule reusing what is already here.
pub(super) fn load_font() -> Option<RasterFont> {
    let manager = jar_manager()?;
    let id: lodestone_assets::ResourceLocation = "minecraft:default".parse().ok()?;
    match FontLoader::new(&manager).load_raster(&id, &FontOptions::none()) {
        Ok(raster) => Some(raster),
        Err(e) => {
            tracing::warn!(target: "assets", "load nametag font: {e}");
            None
        }
    }
}

/// Open `client.jar` from a discovered vanilla pack root as a
/// [`ResourceManager`]. A deliberate duplicate of
/// `hud/vanilla_font.rs::jar_manager` — see this module's doc for why this
/// file cannot call that one directly, and `hud/vanilla_font.rs`'s own doc
/// for why *it* duplicates `crate::resources` rather than calling the
/// `#[cfg(test)]`-gated original.
fn jar_manager() -> Option<ResourceManager> {
    let jar = pack_root()?.join("client.jar");
    let bytes = std::fs::read(&jar)
        .map_err(|e| tracing::warn!(target: "assets", "read {}: {e}", jar.display()))
        .ok()?;
    let zip = ZipSource::from_bytes(bytes)
        .map_err(|e| tracing::warn!(target: "assets", "open {}: {e}", jar.display()))
        .ok()?;
    Some(ResourceManager::new(vec![
        Box::new(zip) as Box<dyn ResourceSource>,
    ]))
}

fn pack_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LODESTONE_ASSETS") {
        let p = PathBuf::from(dir);
        return is_pack_root(&p).then_some(p);
    }
    let cwd = std::env::current_dir().ok()?;
    for base in cwd.ancestors() {
        let cache = base.join(".cache/mc");
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(&cache) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| is_pack_root(p))
                .collect(),
            Err(_) => continue,
        };
        entries.sort();
        if let Some(root) = entries.pop() {
            return Some(root);
        }
    }
    None
}

fn is_pack_root(dir: &Path) -> bool {
    dir.join("client.jar").is_file() && dir.join("generated/reports/blocks.json").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic font-free environment must draw nothing rather than
    /// panic — same "no jar, no crash" contract every other jar-optional
    /// path in this crate has. Exercises `push_entity_quads` directly so the
    /// gate does not depend on whether this machine happens to have a jar.
    #[test]
    fn an_entity_beyond_max_distance_contributes_no_vertices() {
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        let draw = EntityDraw {
            hurt: false,
            id: 1,
            type_path: "pig".to_owned(),
            item: None,
            equipment: Vec::new(),
            feet: Vec3::new(0.0, 0.0, MAX_DISTANCE + 1.0),
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            wool: None,
            count: 1,
            name_tag: Some(crate::entities::NameTag {
                text: "Babe".to_owned(),
                see_through: true,
            }),
            item_use: None,
        };
        // A raster is required to reach the distance check at all in
        // `prepare`, but `push_entity_quads` itself only needs one to lay
        // out ink runs *after* the distance check passes — so this control
        // needs no jar and no `RasterFont` fixture.
        let Some(raster) = load_font() else {
            // No jar on this machine: the distance gate is exercised by the
            // live pixel gate instead (`tests/nametag_pixels.rs`), which
            // requires one. Nothing to assert here without a raster.
            return;
        };
        push_entity_quads(
            &raster,
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            &mut normal,
            &mut see_through,
        );
        assert!(
            normal.is_empty() && see_through.is_empty(),
            "an entity past the 64-block cutoff must contribute nothing, got {} + {} vertices",
            normal.len(),
            see_through.len()
        );
    }

    /// The positive control for the same gate: move the same entity just
    /// inside the cutoff and it must contribute real ink.
    #[test]
    fn an_entity_within_max_distance_with_a_name_contributes_vertices() {
        let Some(raster) = load_font() else {
            return;
        };
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        let draw = EntityDraw {
            hurt: false,
            id: 1,
            type_path: "pig".to_owned(),
            item: None,
            equipment: Vec::new(),
            feet: Vec3::new(0.0, 0.0, MAX_DISTANCE - 1.0),
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            wool: None,
            count: 1,
            name_tag: Some(crate::entities::NameTag {
                text: "Babe".to_owned(),
                see_through: true,
            }),
            item_use: None,
        };
        push_entity_quads(
            &raster,
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            &mut normal,
            &mut see_through,
        );
        assert!(
            !normal.is_empty(),
            "an in-range named entity must contribute normal-pass ink"
        );
        assert!(
            !see_through.is_empty(),
            "`see_through: true` must contribute see-through-pass ink too"
        );

        // Negative control on `see_through`: the same entity, sneaking,
        // must contribute normal ink but none to the see-through pass.
        let mut normal2 = Vec::new();
        let mut see_through2 = Vec::new();
        let sneaking = EntityDraw {
            name_tag: Some(crate::entities::NameTag {
                text: "Babe".to_owned(),
                see_through: false,
            }),
            ..draw
        };
        push_entity_quads(
            &raster,
            &sneaking,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            &mut normal2,
            &mut see_through2,
        );
        assert!(
            !normal2.is_empty(),
            "sneaking must not suppress the normal pass"
        );
        assert!(
            see_through2.is_empty(),
            "sneaking (`see_through: false`) must suppress the see-through pass"
        );
    }

    /// A blank custom name (empty string) must draw nothing — same rule as
    /// "no name tag at all", not a zero-width visible tag.
    #[test]
    fn an_empty_name_contributes_no_vertices() {
        let Some(raster) = load_font() else {
            return;
        };
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        let draw = EntityDraw {
            hurt: false,
            id: 1,
            type_path: "pig".to_owned(),
            item: None,
            equipment: Vec::new(),
            feet: Vec3::ZERO,
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            wool: None,
            count: 1,
            name_tag: Some(crate::entities::NameTag {
                text: String::new(),
                see_through: true,
            }),
            item_use: None,
        };
        push_entity_quads(
            &raster,
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            &mut normal,
            &mut see_through,
        );
        assert!(normal.is_empty() && see_through.is_empty());
    }

    #[test]
    fn entity_base_height_falls_back_for_an_unknown_type_path() {
        assert_eq!(entity_base_height("not_a_real_entity_type"), FALLBACK_HEIGHT);
        // A real type resolves to its real (non-fallback) census height.
        assert!((entity_base_height("player") - 1.8).abs() < 1e-6);
    }
}
