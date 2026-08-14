//! [`ContainerRenderer`] — the GPU half: pipelines, buffers and the pass
//! ordering that keeps vanilla's strata in the right order.
//!
//! Split out of `container.rs` verbatim.

use std::sync::Arc;

use lodestone_assets::ItemAtlas;
use lodestone_render::{BlockModels, GpuAtlas};

use crate::hud::VanillaFont;
use crate::hud::item_icon::{self, IconAssets, IconRenderer};

use super::background::ContainerBackground;
use super::frame::ContainerFrame;
use super::geometry::ContainerGeometry;
use super::player_preview::PlayerPreview;
use super::{BG_FLOATS_PER_VERTEX, CONTAINER_BG_WGSL, CONTAINER_WGSL, FLOATS_PER_VERTEX};

/// GPU renderer for the container overlay.
#[derive(Debug)]
pub struct ContainerRenderer {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
    /// The flat item atlas and the 3-D block-item pass, shared verbatim with the
    /// hotbar. Both halves start detached, so [`render`](Self::render) alone
    /// keeps the pre-icon behaviour.
    icons: IconRenderer,
    /// The vanilla proportional font, resolved once per process exactly like
    /// [`HudRenderer`](crate::hud::HudRenderer)'s. `None` on a jar-less run,
    /// where stack counts draw with the fixed-advance debug font.
    font: Option<Arc<VanillaFont>>,
    /// Vanilla's real `container/*.png` panel art. Starts detached,
    /// so [`render`](Self::render)/[`render_with_icons`](Self::render_with_icons)
    /// alone keep the pre-texture flat-fill behaviour — the jar-less path and
    /// the negative control the pixel gate leans on.
    background: Option<ContainerBackgroundGpu>,
    /// The **inventory avatar** — the player standing in the panel's recess with
    /// their head tracking the cursor. Starts detached, so every existing caller
    /// and every headless gate keeps the empty recess it has always drawn.
    ///
    /// Detached is also the honest state on a jar-less run: without the skin sheet
    /// there is nothing to draw and no synthetic fallback, exactly as
    /// [`background`](Self::background) and `gpu/entities.rs`'s armour sheets
    /// behave. See [`super::player_preview`].
    player_preview: Option<PlayerPreview>,
}

/// The GPU half of [`ContainerBackground`]: its own tiny textured pipeline,
/// sampling a **different** atlas than [`IconRenderer`]'s item-sprite pass, so
/// it cannot share that pipeline or bind group.
#[derive(Debug)]
struct ContainerBackgroundGpu {
    /// Kept alive because the bind group's texture view is derived from it, and
    /// so [`ContainerBackground::quads`] stays reachable from the render path.
    data: Arc<ContainerBackground>,
    #[allow(dead_code)]
    gpu: GpuAtlas,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
}

impl ContainerRenderer {
    /// Builds the overlay pipeline.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("container-shader"),
            source: wgpu::ShaderSource::Wgsl(CONTAINER_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("container-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("container-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (FLOATS_PER_VERTEX * 4) as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 8,
                            shader_location: 1,
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
        let capacity_floats = 4096;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("container-verts"),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            buffer,
            capacity_floats,
            icons: IconRenderer::new(),
            font: VanillaFont::shared(),
            background: None,
            player_preview: None,
        }
    }

    /// Attach the **inventory avatar** — the player rig drawn into the panel's
    /// recess with their head tracking the cursor.
    ///
    /// Independent of every other `attach_*` here, and a no-op returning `false`
    /// on a jar-less run (no skin sheet, so the recess stays the empty hole in
    /// `inventory.png` that it was before this existed). Requires a depth view at
    /// draw time for the same reason the 3-D block-item pass does: the rig is
    /// depth-tested against itself.
    pub fn attach_player_preview(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
    ) -> bool {
        self.player_preview = PlayerPreview::new(device, queue, color_format);
        self.player_preview.is_some()
    }

    /// Whether the inventory avatar is bound — the "is this attached" gate for the
    /// avatar, in the same shape as
    /// [`background_attached`](Self::background_attached) and for the same reason:
    /// without it a missing jar silently degrades to an empty recess and a
    /// coverage-only assertion still passes.
    #[must_use]
    pub fn player_preview_attached(&self) -> bool {
        self.player_preview.is_some()
    }

    /// Bind a real skin to the inventory avatar: a declared rig, and optionally
    /// the sheet to draw it with (`None` uses the pack's own sheet for that rig).
    ///
    /// **This is the seam that fix's fetch half lands against.** Nothing in this
    /// workspace fetches a skin yet — see `player_preview.rs`'s
    /// `local_skin_override`, which is what keeps the slim rig reachable in the
    /// meantime — so today's callers are that override and this method's own
    /// gates. Returns `false` when the avatar is not attached at all, or when the
    /// rig or sheet cannot be resolved; never leaves a half-applied state.
    pub fn set_player_skin(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        model: lodestone_assets::PlayerModelType,
        sheet: Option<&lodestone_assets::Image>,
    ) -> bool {
        self.player_preview
            .as_mut()
            .is_some_and(|p| p.set_skin(device, queue, model, sheet))
    }

    /// Which rig the inventory avatar is drawing, or `None` when it is not
    /// attached. The assertable half of [`set_player_skin`](Self::set_player_skin).
    #[must_use]
    pub fn player_preview_model(&self) -> Option<lodestone_assets::PlayerModelType> {
        self.player_preview.as_ref().map(|p| p.skin_model())
    }

    /// Attach vanilla's real `container/*.png` panel art, so the
    /// screen draws the real texture instead of the flat programmatic fill.
    /// Independent of [`attach_items`](Self::attach_items)/
    /// [`attach_item_models`](Self::attach_item_models) — an atlas-less run can
    /// still have the real panel art (or vice versa).
    pub fn attach_background(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        background: Arc<ContainerBackground>,
    ) {
        let gpu = GpuAtlas::from_atlas(device, queue, background.atlas());
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("container-bg-shader"),
            source: wgpu::ShaderSource::Wgsl(CONTAINER_BG_WGSL.into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("container-bg-bgl"),
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
            label: Some("container-bg-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("container-bg-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (8 * 4) as wgpu::BufferAddress,
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
            label: Some("container-bg-bind"),
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
        let capacity_floats = 512;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("container-bg-verts"),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.background = Some(ContainerBackgroundGpu {
            data: background,
            gpu,
            pipeline,
            bind_group,
            buffer,
            capacity_floats,
        });
    }

    /// Whether the real vanilla `container/*.png` art is bound — the gate for
    /// "this screen looks like vanilla" must assert this, exactly
    /// as [`MenuRenderer::gui_attached`](crate::menu::render::MenuRenderer::gui_attached)
    /// gates the title/pause screens' buttons: without it a missing jar
    /// silently degrades to the flat-fill fallback and a coverage-only
    /// assertion still passes.
    #[must_use]
    pub fn background_attached(&self) -> bool {
        self.background.is_some()
    }

    /// Whether the vanilla proportional font resolved — the second half of "this
    /// screen looks like vanilla". Without it the two container labels fall back
    /// to the fixed-advance 5×7 debug font, which is *legible* and therefore
    /// invisible to a coverage assertion: exactly how that fix's "wrong font"
    /// survived. A gate asserting typeface must assert this first.
    #[must_use]
    pub fn font_attached(&self) -> bool {
        self.font.is_some()
    }

    /// The attached vanilla proportional font, for a caller building its own
    /// [`ContainerGeometry`] to hand to
    /// [`render_geometry_scaled`](Self::render_geometry_scaled) — the creative
    /// screen. Reading it here rather than threading a second copy
    /// through `app.rs` is what keeps the two screens' text identical.
    #[must_use]
    pub fn font(&self) -> Option<&VanillaFont> {
        self.font.as_deref()
    }

    /// The attached [`ContainerBackground`], for the same reason
    /// [`font`](Self::font) exists.
    #[must_use]
    pub fn background_data(&self) -> Option<&ContainerBackground> {
        self.background.as_ref().map(|bg| bg.data.as_ref())
    }

    /// The attached flat item-sprite atlas, for the same reason
    /// [`font`](Self::font) exists.
    #[must_use]
    pub fn item_atlas(&self) -> Option<Arc<ItemAtlas>> {
        self.icons.item_atlas()
    }

    /// Attach the flat item-sprite [`ItemAtlas`] so container slots draw real
    /// item icons instead of the colour-swatch fallback. Mirrors
    /// [`HudRenderer::attach_items`](crate::hud::HudRenderer::attach_items) and
    /// costs a second upload of the (small) item atlas; the *block* atlas, the
    /// expensive one, is borrowed rather than uploaded by
    /// [`attach_item_models`](Self::attach_item_models).
    pub fn attach_items(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        atlas: Arc<ItemAtlas>,
    ) {
        self.icons
            .attach_items(device, queue, color_format, atlas, "container-item");
    }

    /// Attach the 2-D GUI enchantment-glint pass, so an enchanted stack in a slot
    /// or on the cursor shimmers. Mirrors
    /// [`HudRenderer::attach_glint`](crate::hud::HudRenderer::attach_glint), and
    /// like it must follow [`attach_items`](Self::attach_items).
    pub fn attach_glint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        img: &lodestone_assets::Image,
    ) {
        self.icons
            .attach_glint(device, queue, color_format, img, "container-glint");
    }

    /// Push vanilla's **Glint Speed**/**Glint Strength** accessibility options to
    /// this screen's 2-D GUI glint pass. Mirrors
    /// [`HudRenderer::set_glint_options`](crate::hud::HudRenderer::set_glint_options),
    /// and both are needed because each owns its own [`IconRenderer`] — an
    /// enchanted stack in a slot and the same stack in the hotbar would otherwise
    /// shimmer at different rates and out of phase.
    pub fn set_glint_options(&mut self, speed: f64, strength: f32) {
        self.icons.set_glint_options(speed, strength);
    }

    /// This frame's GUI glint speed and strength as the uniform will see them —
    /// already clamped, for the same reason
    /// [`HudRenderer::glint_options`](crate::hud::HudRenderer::glint_options)
    /// exists.
    #[must_use]
    pub fn glint_options(&self) -> (f64, f32) {
        self.icons.glint_options()
    }

    /// Attach the **3-D block-item** pass, so container slots holding a block
    /// draw vanilla's isometric mini-block. Every resource is borrowed from the
    /// world renderer — the same block atlas, tint palette and animation slots
    /// the terrain and the hotbar use.
    pub fn attach_item_models(
        &mut self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
        palette: &wgpu::Buffer,
        anim: &wgpu::Buffer,
    ) {
        self.icons.attach_item_models(
            device,
            color_format,
            atlas_view,
            atlas_sampler,
            palette,
            anim,
            "container-item-model",
        );
    }

    /// Draws the container overlay over the current frame, with **no** item
    /// icons: slot contents fall back to the colour swatch. The plain entry
    /// point, kept so existing callers and the headless gates are unchanged.
    /// Always lays out against [`crate::config::AUTO_GUI_SCALE`]; use
    /// [`render_scaled`](Self::render_scaled) for the real windowed path,
    /// which has a persisted `Options.gui_scale` to honour.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &ContainerFrame<'_>,
        width: u32,
        height: u32,
    ) {
        self.render_with_icons(device, queue, view, None, frame, None, width, height);
    }

    /// As [`render`](Self::render), but against an explicit `gui_scale` (`0` =
    /// auto) so the drawn panel matches whatever scale [`hit_test_with_scale`]
    /// is being called with for the same frame.
    pub fn render_scaled(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &ContainerFrame<'_>,
        gui_scale: u32,
        width: u32,
        height: u32,
    ) {
        self.render_with_icons_scaled(
            device, queue, view, None, frame, None, gui_scale, width, height,
        );
    }

    /// Draws the container overlay including **real item icons**.
    ///
    /// `models` supplies baked block-item geometry (`None` falls back to flat
    /// sprites only) and `depth` is a depth attachment matching the target size,
    /// normally [`RenderState::depth_view`](crate::gpu::RenderState::depth_view).
    /// Both are needed for a mini-block to draw; either being `None` degrades to
    /// flat sprites rather than erroring. The flat icons themselves need
    /// [`attach_items`](Self::attach_items) and nothing else.
    ///
    /// # Pass structure
    ///
    /// Three passes, in this order, all loading the existing colour — the same
    /// shape, and for the same reasons, as the HUD's:
    ///
    /// 1. **chrome** (no depth) — panel, slot wells, title;
    /// 2. **item models** (depth, **cleared**) — the isometric mini-blocks;
    /// 3. **flat icons + text** (no depth) — sprite icons, stack counts,
    ///    durability bars.
    ///
    /// The chrome must precede the icons (it is the well they sit in), and the
    /// counts must follow them (they sit on top). The model pass clears depth
    /// because the world's is still resident and would swallow a GUI item at
    /// clip depth ~0.5.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_icons(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        frame: &ContainerFrame<'_>,
        models: Option<&BlockModels>,
        width: u32,
        height: u32,
    ) {
        self.render_with_icons_scaled(
            device,
            queue,
            view,
            depth,
            frame,
            models,
            crate::config::AUTO_GUI_SCALE,
            width,
            height,
        );
    }

    /// As [`render_with_icons`](Self::render_with_icons), but against an
    /// explicit `gui_scale` (`0` = auto) — see [`render_scaled`](Self::render_scaled).
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_icons_scaled(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        frame: &ContainerFrame<'_>,
        models: Option<&BlockModels>,
        gui_scale: u32,
        width: u32,
        height: u32,
    ) {
        self.render_with_icons_scaled_between_strata(
            device,
            queue,
            view,
            depth,
            frame,
            models,
            gui_scale,
            width,
            height,
            || {},
        );
    }

    /// As [`render_with_icons_scaled`](Self::render_with_icons_scaled), but with
    /// a draw hook at vanilla's **`nextStratum()` boundary** — the one place an
    /// overlay belonging to a container screen may be submitted.
    ///
    /// # Why this hook exists instead of "draw it afterwards"
    ///
    /// `AbstractRecipeBookScreen.extractRenderState` is the record, and it is
    /// explicit about the order:
    ///
    /// ```text
    /// extractContents          panel art, labels, slots, both slot highlights
    /// nextStratum()
    /// recipeBookComponent      <- the overlay: THIS hook
    /// nextStratum()
    /// extractCarriedItem       the stack on the cursor
    /// extractTooltip           the hovered slot's tooltip
    /// recipeBookComponent.extractTooltip
    /// ```
    ///
    /// So the recipe-book panel sits **above** every slot and **below** the
    /// carried stack and the item tooltip. This renderer draws the carried
    /// stratum and the tooltip itself (see [`ContainerGeometry`]'s
    /// `slot_vertex_count` family), so an overlay submitted *after* the whole
    /// container call lands on top of both — which is exactly the reported
    /// defect: the recipe book painted over the stack being dragged and over the
    /// tooltip of whatever slot the pointer was on.
    ///
    /// Passing the overlay *in* rather than sequencing it at the call site is the
    /// point: there is one entry point, the hook is a parameter of it, and the
    /// carried stratum is unreachable to a caller, so a future overlay cannot be
    /// appended after the cursor stack by accident. Anything drawn from here must
    /// submit its own encoder — this call has already flushed the slot stratum's,
    /// and creates a fresh one for the carried stratum afterwards.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_icons_scaled_between_strata(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        frame: &ContainerFrame<'_>,
        models: Option<&BlockModels>,
        gui_scale: u32,
        width: u32,
        height: u32,
        between_strata: impl FnOnce(),
    ) {
        // Only ask for model geometry when there is somewhere to draw it.
        let want_models = self.icons.models_attached() && depth.is_some();
        let item_atlas = self.icons.item_atlas();
        let geo = ContainerGeometry::build_inner(
            frame,
            width,
            height,
            gui_scale,
            &IconAssets {
                items: item_atlas.as_deref(),
                models: models.filter(|_| want_models),
            },
            self.font.as_deref(),
            self.background.as_ref().map(|bg| bg.data.as_ref()),
        );
        self.render_geometry_scaled_between_strata(
            device,
            queue,
            view,
            depth,
            &geo,
            gui_scale,
            width,
            height,
            between_strata,
        );
    }

    /// Draw an already-built [`ContainerGeometry`] through this renderer's
    /// passes.
    ///
    /// Split out of [`render_with_icons_scaled`](Self::render_with_icons_scaled)
    /// for the creative-inventory screen, which builds its own
    /// geometry from [`super::creative_geometry`] rather than from a
    /// [`ContainerFrame`] — vanilla's creative screen is backed by a client-only
    /// `ItemPickerMenu` with no `Menu` behind it, so it cannot go through
    /// `build_inner`. Everything *below* that seam is shared: same pipelines,
    /// same bind groups, same four-pass order, and therefore the same guarantee
    /// about stack counts landing over their icons rather than under them.
    ///
    /// `gui_scale`/`width`/`height` must be the triple `geo` was built with.
    #[allow(clippy::too_many_arguments)]
    pub fn render_geometry_scaled(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        geo: &ContainerGeometry,
        gui_scale: u32,
        width: u32,
        height: u32,
    ) {
        self.render_geometry_scaled_between_strata(
            device, queue, view, depth, geo, gui_scale, width, height, || {},
        );
    }

    /// As [`render_geometry_scaled`](Self::render_geometry_scaled), with the
    /// `nextStratum()` draw hook —
    /// [`render_with_icons_scaled_between_strata`](Self::render_with_icons_scaled_between_strata)
    /// carries the whole argument for why an overlay goes *there* rather than
    /// after this call.
    #[allow(clippy::too_many_arguments)]
    pub fn render_geometry_scaled_between_strata(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        geo: &ContainerGeometry,
        gui_scale: u32,
        width: u32,
        height: u32,
        between_strata: impl FnOnce(),
    ) {
        // A skin fetched after startup lands here, not at
        // construction: `PlayerPreview` is built once during `app::lifecycle`'s
        // resume and never re-reads the cache, while sign-in happens later in the
        // same run. Draining on the frame is what makes the fetch reach pixels
        // without a restart — see `crate::skin_fetch`. Cheap: one uncontended
        // `Mutex::lock` per container frame, `None` on all but one of them.
        if let Some((model, sheet)) = crate::skin_fetch::take_pending() {
            let applied = self.set_player_skin(device, queue, model, Some(&sheet));
            tracing::info!(
                target: "assets",
                model = model.serialized_name(),
                applied,
                "bound the fetched skin to the inventory avatar"
            );
        }
        // `geo.special` counts too — see the same guard in
        // `HudRenderer::render_with_item_models`. A frame whose only content is a
        // chest icon must not be discarded before it reaches `upload`.
        if geo.verts.is_empty()
            && geo.item_verts.is_empty()
            && geo.model_verts.is_empty()
            && geo.bg_verts.is_empty()
            && geo.special.is_empty()
            // …and so does the inventory avatar, for the same reason: it is a
            // placement rather than a vertex stream, so it is invisible to every
            // emptiness check above.
            && geo.player_avatar.is_none()
        {
            // The hook still fires. An overlay is not conditional on the
            // container having drawn anything — a frame whose geometry is empty
            // (no menu, nothing attached) would otherwise silently swallow it,
            // and "the recipe book vanishes on some frames" is a worse bug than
            // the one this hook exists to fix.
            between_strata();
            return;
        }
        if geo.verts.len() > self.capacity_floats {
            self.capacity_floats = geo.verts.len().next_power_of_two();
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("container-verts"),
                size: (self.capacity_floats * 4) as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !geo.verts.is_empty() {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&geo.verts));
        }
        // The background pass's own dynamic buffer, grown the same way as the
        // chrome one above.
        let bg_count = if let Some(bg) = self.background.as_mut() {
            if geo.bg_verts.len() > bg.capacity_floats {
                bg.capacity_floats = geo.bg_verts.len().next_power_of_two();
                bg.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("container-bg-verts"),
                    size: (bg.capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            if !geo.bg_verts.is_empty() {
                queue.write_buffer(&bg.buffer, 0, bytemuck::cast_slice(&geo.bg_verts));
            }
            (geo.bg_verts.len() / BG_FLOATS_PER_VERTEX) as u32
        } else {
            0
        };
        // As in `HudRenderer::render_with_item_models`: `upload` feeds these
        // straight to `gui_ortho`, which must match the logical canvas
        // `ContainerGeometry::build_inner` posed the 3-D block-item vertices
        // into above, not the raw physical framebuffer.
        let (logical_w, logical_h) = crate::menu::render::logical_canvas(gui_scale, width, height);
        let (item_count, model_count) = self.icons.upload(
            device,
            queue,
            &geo.item_verts,
            &geo.model_verts,
            &geo.special,
            geo.slot_special_count,
            logical_w.max(1.0) as u32,
            logical_h.max(1.0) as u32,
            "container-item-verts",
        );
        let glint_count =
            self.icons
                .upload_glint(device, queue, &geo.glint_verts, "container-glint-verts");

        let vertex_count = geo.vertex_count() as u32;
        let chrome_count = (geo.chrome_vertex_count as u32).min(vertex_count);
        let dim_count = (geo.dim_vertex_count as u32).min(chrome_count);
        // The three carried-stack splits. Clamped against what
        // `upload` actually reported so a stream whose half is not attached (no
        // atlas, no depth) still yields an empty range rather than a bogus one.
        let slot_colour_count = (geo.slot_vertex_count as u32).clamp(chrome_count, vertex_count);
        let slot_item_count = (geo.slot_item_vertex_count as u32).min(item_count);
        let slot_glint_count = (geo.slot_glint_vertex_count as u32).min(glint_count);
        let slot_model_count = (geo.slot_model_vertex_count as u32).min(model_count);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("container"),
        });
        // Draw order matters here and mirrors vanilla's own
        // `extractBackground`: the dim gradient goes down first (it sits under
        // everything, including the panel art), then the real panel texture (if
        // attached) draws on top of it, and only *then* the rest of this
        // stream's "chrome" — the flat-fill fallback (when there is no texture),
        // the title, the slot wells. Sandwiching the texture between the two
        // `verts` ranges is what keeps the dim from also darkening the panel
        // itself.
        if dim_count > 0 {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-dim-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            pass.draw(0..dim_count, 0..1);
        }
        // The background stream draws in **two** ranges, split at
        // `bg_slot_vertex_count`: the panel art, the back highlight and the
        // empty-slot placeholders here, and the hover highlight's front sprite
        // after the slot item passes below. See that field's doc comment.
        let bg_slot_count = (geo.bg_slot_vertex_count as u32).min(bg_count);
        if bg_slot_count > 0
            && let Some(bg) = self.background.as_ref()
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-bg-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&bg.pipeline);
            pass.set_bind_group(0, &bg.bind_group, &[]);
            pass.set_vertex_buffer(0, bg.buffer.slice(..));
            pass.draw(0..bg_slot_count, 0..1);
        }
        if chrome_count > dim_count {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            pass.draw(dim_count..chrome_count, 0..1);
        }

        // The inventory avatar, immediately after the panel art it stands in and
        // before the slot icons — which is where vanilla calls it, from
        // `InventoryScreen.extractBackground`, right after its own `INVENTORY_LOCATION`
        // blit. Ordering against the slots is free (the recess holds no slot), but
        // ordering against the *panel* is not: drawn first it would be painted over.
        //
        // Its pass clears depth and the model pass below clears it again, so
        // neither inherits the other's — see `PlayerPreview::draw`.
        if let (Some(preview), Some(avatar), Some(depth_view)) =
            (self.player_preview.as_ref(), geo.player_avatar, depth)
        {
            preview.draw(
                device,
                queue,
                &mut encoder,
                view,
                depth_view,
                &avatar,
                gui_scale,
                width,
                height,
            );
        }

        self.icons.draw_models_range(
            &mut encoder,
            view,
            depth,
            0..slot_model_count,
            item_icon::IconStratum::Slots,
            "container-item-model-pass",
        );

        if slot_item_count > 0 || slot_colour_count > chrome_count {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-item-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.icons.draw_sprites_range(&mut pass, 0..slot_item_count);
            // The glint over the slot icons, before the counts and bars so those
            // stay legible on an enchanted stack.
            self.icons.draw_glint_range(&mut pass, 0..slot_glint_count);
            // Stack counts, durability bars and the atlas-less swatch fallback,
            // over whichever kind of icon drew beneath them.
            if slot_colour_count > chrome_count {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.buffer.slice(..));
                pass.draw(chrome_count..slot_colour_count, 0..1);
            }
        }

        // `extractSlotHighlightFront` (`AbstractContainerScreen.java:159-163`):
        // the second highlight sprite, over the hovered slot's item and under the
        // carried stack's stratum — exactly where vanilla calls it, between
        // `extractSlots` and `extractCarriedItem`.
        if bg_count > bg_slot_count
            && let Some(bg) = self.background.as_ref()
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-bg-front-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&bg.pipeline);
            pass.set_bind_group(0, &bg.bind_group, &[]);
            pass.set_vertex_buffer(0, bg.buffer.slice(..));
            pass.draw(bg_slot_count..bg_count, 0..1);
        }

        // ---- vanilla's first `nextStratum()` -------------------------------
        //
        // Everything above is the **slot stratum**. Flush it, then let the
        // caller's overlay — today the recipe-book panel — submit its own work
        // *between* the two strata, exactly where
        // `AbstractRecipeBookScreen.extractRenderState` draws
        // `recipeBookComponent`. wgpu executes submissions in order, so two
        // encoders either side of the hook is what buys the layering; a single
        // encoder finished at the end of this function could not, because the
        // overlay's own submit would already have landed.
        queue.submit(std::iter::once(encoder.finish()));
        between_strata();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("container-carried"),
        });

        // ---- vanilla's second `nextStratum()` ------------------------------
        //
        // The carried stack replays all three streams *after* every slot, and its
        // model pass **clears depth again** — that clear is what stops a slot
        // block item's near faces winning over a block on the cursor. See the
        // layering table in `build_inner` for the four cases and which two append
        // order alone could not fix. The hovered slot's tooltip rides the tail of
        // the same colour range, which is why it too ends up above the overlay.
        self.icons.draw_models_range(
            &mut encoder,
            view,
            depth,
            slot_model_count..model_count,
            item_icon::IconStratum::Carried,
            "container-carried-model-pass",
        );
        if item_count > slot_item_count
            || glint_count > slot_glint_count
            || vertex_count > slot_colour_count
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-carried-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.icons
                .draw_sprites_range(&mut pass, slot_item_count..item_count);
            self.icons
                .draw_glint_range(&mut pass, slot_glint_count..glint_count);
            if vertex_count > slot_colour_count {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.buffer.slice(..));
                pass.draw(slot_colour_count..vertex_count, 0..1);
            }
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}
