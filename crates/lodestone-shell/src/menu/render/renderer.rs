//! [`MenuRenderer`]: the two GPU pipelines, their growable vertex buffers,
//! the lazily-attached GUI atlas and panorama, and the pass ordering.
//!
//! The two `include_str!` shader handles stay in the module root — their
//! paths are relative to the file they are written in, so moving them here
//! would silently change what they read.
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use super::*;

/// Number of `f32`s per vertex on the colour stream (`[x, y, r, g, b, a]`).
pub(super) const FLOATS_PER_VERTEX: usize = 6;
/// Number of `f32`s per vertex on the sprite stream
/// (`[x, y, u, v, r, g, b, a]`). Matches `hud.rs`'s stride, because
/// `item_icon::push_sprite_quad` writes both streams' vertices — but that
/// constant is private to `hud`, so it is restated (and pinned by a test)
/// rather than reached into.
pub(super) const SPRITE_FLOATS_PER_VERTEX: usize = 8;

/// The uploaded GUI atlas and the textured pipeline that samples it: what turns
/// a `widget/button` nine-slice into pixels. Absent on a jar-less run, where the
/// menu falls back to flat coloured button fills.
#[derive(Debug)]
struct MenuSprites {
    atlas: Arc<GuiAtlas>,
    /// Kept alive because the bind group's texture view is derived from it.
    #[allow(dead_code)]
    gpu: GpuAtlas,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
}

/// GPU renderer for the menu screens: a coloured-quad pipeline, a textured GUI
/// sprite pipeline, and a growable dynamic vertex buffer for each, plus the
/// cubemap panorama ([`crate::menu::panorama`]) that draws behind all three on an
/// out-of-world screen. Drawn in a `Clear` pass for a screen that owns the frame
/// and a `Load` pass for the pause overlay.
#[derive(Debug)]
pub struct MenuRenderer {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
    /// The target format, kept so the sprite pipeline can be built later —
    /// [`MenuRenderer::new`] cannot build it, because uploading the atlas needs
    /// a `Queue` and `new` is only given a `Device`.
    color_format: wgpu::TextureFormat,
    /// The GUI sprite half, attached lazily on the first draw (see
    /// [`MenuRenderer::ensure_gui`]).
    sprites: Option<MenuSprites>,
    /// Whether the lazy load has already been tried. Without this a jar-less run
    /// would re-stitch (and fail) an atlas every single frame.
    gui_attempted: bool,
    /// Vanilla's proportional font, resolved once per process from the same jar.
    /// Needs no GPU resources, so it is resolved in `new` exactly as
    /// `HudRenderer` does. `None` on a jar-less run.
    font: Option<Arc<VanillaFont>>,
    /// The title screen's spinning cubemap, attached lazily on the first draw
    /// (see [`MenuRenderer::ensure_panorama`]). `None` leaves every screen on the
    /// flat [`BG`] backdrop, which is the pre-panorama behaviour.
    panorama: Option<PanoramaRenderer>,
    /// Whether the lazy panorama load has already been tried — same purpose as
    /// [`Self::gui_attempted`]: without it a jar-less run re-decodes six PNGs
    /// every frame.
    panorama_attempted: bool,
}

impl MenuRenderer {
    /// Builds the menu pipeline for a target of `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("menu-shader"),
            source: wgpu::ShaderSource::Wgsl(MENU_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("menu-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("menu-pipeline"),
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

        let capacity_floats = 1 << 16;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("menu-verts"),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            buffer,
            capacity_floats,
            color_format,
            sprites: None,
            gui_attempted: false,
            font: VanillaFont::shared(),
            panorama: None,
            panorama_attempted: false,
        }
    }

    /// Whether the real GUI sprite atlas is bound, i.e. whether the buttons draw
    /// as vanilla's nine-slice `widget/button*` art rather than flat fills.
    ///
    /// A gate that means to measure vanilla button chrome **must assert this**:
    /// without it a missing jar silently degrades to the coloured-rectangle
    /// fallback and every "something drew in the button's rect" assertion still
    /// passes. Same discipline as `HudRenderer::font_attached`.
    #[must_use]
    pub fn gui_attached(&self) -> bool {
        self.sprites.is_some()
    }

    /// Whether vanilla text is in play. See [`Self::gui_attached`].
    #[must_use]
    pub fn font_attached(&self) -> bool {
        self.font.is_some()
    }

    /// Bind a GUI sprite atlas: uploads it, builds the textured pipeline, and
    /// binds it.
    ///
    /// The atlas must be one built with
    /// [`crate::resources::TITLE_TEXTURES`](crate::resources::TITLE_TEXTURES)
    /// for the title logo to draw; a plain [`GuiAtlas::build`] atlas gives
    /// correct buttons and no logo, because the logo is not a `gui/sprites`
    /// texture. Calling this replaces whatever was bound.
    pub fn attach_gui(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: Arc<GuiAtlas>,
    ) {
        let sp = crate::hud::item_icon::build_sprite_pipeline(
            device,
            queue,
            atlas.atlas(),
            MENU_SPRITE_WGSL,
            self.color_format,
            1 << 14,
            "menu-sprite",
        );
        self.gui_attempted = true;
        self.sprites = Some(MenuSprites {
            atlas,
            gpu: sp.gpu,
            pipeline: sp.pipeline,
            bind_group: sp.bind_group,
            buffer: sp.buffer,
            capacity_floats: sp.capacity_floats,
        });
    }

    /// Drop back to the flat coloured-rectangle buttons. The executed negative
    /// control for every "the real vanilla sprite drew here" assertion: with this
    /// called, a gate claiming to see `widget/button` must fail.
    pub fn detach_gui(&mut self) {
        self.sprites = None;
        // Deliberately leaves `gui_attempted` set, so `ensure_gui` does not
        // helpfully undo the control on the next draw.
        self.gui_attempted = true;
    }

    /// Load and bind the GUI atlas on first use.
    ///
    /// Lazy rather than an `attach_gui` call from `app.rs` for one reason: it
    /// needs a `Queue`, which `MenuRenderer::new`'s call site has but does not
    /// pass, and `app.rs` is not this change's to edit. Every draw path already
    /// receives both a `Device` and a `Queue`, so this is the one place that has
    /// what the upload needs. `attach_gui` stays public so `app.rs` can hand in a
    /// shared atlas later and skip the second stitch.
    fn ensure_gui(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.gui_attempted {
            return;
        }
        self.gui_attempted = true;
        if let Some(atlas) = crate::resources::load_menu_gui_atlas() {
            self.attach_gui(device, queue, atlas);
        }
    }

    /// Whether the title screen's cubemap panorama is bound, i.e. whether the
    /// out-of-world screens draw vanilla's spinning sky rather than the flat
    /// [`BG`] backdrop.
    ///
    /// Same discipline as [`Self::gui_attached`]: a gate that means to measure the
    /// panorama **must assert this**, because a jar-less run degrades silently to
    /// a fill that satisfies any "something drew here" assertion.
    #[must_use]
    pub fn panorama_attached(&self) -> bool {
        self.panorama.is_some()
    }

    /// How many of the bound panorama's six faces came from the launcher's
    /// asset-object store rather than `client.jar` — 6 is vanilla's real art, 0 is
    /// the jar's 1×1 grey stubs. `0` when no panorama is bound at all.
    ///
    /// `panorama_attached()` is **not** enough for a gate that means to measure the
    /// real sky: the jar stubs bind and draw perfectly, as a flat colour. See
    /// [`crate::asset_objects`].
    #[must_use]
    pub fn panorama_faces_from_object_store(&self) -> usize {
        self.panorama
            .as_ref()
            .map_or(0, PanoramaRenderer::faces_from_object_store)
    }

    /// Bind a panorama cubemap: uploads the six layers and builds its pipeline.
    ///
    /// Public so a gate can hand in a synthetic cubemap with six distinguishable
    /// faces — which is the only way to check the [`panorama::FACE_SUFFIXES`]
    /// order from pixels, since vanilla's shipped faces in 26.2 are a single flat
    /// grey (see `docs/menu-panorama.md`).
    pub fn attach_panorama(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        faces: &PanoramaFaces,
    ) {
        self.panorama_attempted = true;
        self.panorama = Some(PanoramaRenderer::new(
            device,
            queue,
            self.color_format,
            faces,
        ));
    }

    /// Drop back to the flat [`BG`] backdrop. The executed negative control for
    /// every "the panorama reached pixels" assertion.
    pub fn detach_panorama(&mut self) {
        self.panorama = None;
        // As `detach_gui`: leave the attempted flag set so the next draw does not
        // helpfully undo the control.
        self.panorama_attempted = true;
    }

    /// Load and bind the panorama cubemap on first use — twin of
    /// [`Self::ensure_gui`], and lazy for the same reason (the upload needs a
    /// `Queue`, which only the draw paths have).
    fn ensure_panorama(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.panorama_attempted {
            return;
        }
        self.panorama_attempted = true;
        if let Some(faces) = crate::resources::load_panorama() {
            self.attach_panorama(device, queue, &faces);
        }
    }

    /// Draws one menu frame, clearing the target first. For a screen owning
    /// the whole frame (see [`owns_frame`]) — nothing renders behind a menu,
    /// so clearing rather than loading is what keeps the last world frame
    /// from showing through.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &MenuFrame<'_>,
        width: u32,
        height: u32,
    ) {
        self.draw(
            device,
            queue,
            view,
            frame,
            width,
            height,
            wgpu::LoadOp::Clear(wgpu::Color {
                r: f64::from(BG[0]),
                g: f64::from(BG[1]),
                b: f64::from(BG[2]),
                a: 1.0,
            }),
        );
    }

    /// Draws one frame **over** whatever `view` already holds instead of
    /// clearing it first — for the pause menu (see [`pause_frame`]), which
    /// sits on top of the world, HUD and container passes the caller already
    /// ran this frame rather than replacing them (mirrors
    /// [`crate::effects::EffectsRenderer`]'s own Load-pass overlay). Every
    /// other detail — buffer growth, the vertex layout, the pipeline — is
    /// identical to [`render`](Self::render); only the load op differs, so a
    /// caller must never invoke both in the same frame — `Screen::Paused` is
    /// not an [`owns_frame`] screen for exactly this reason: [`render`] and
    /// `render_overlay` are alternatives, not a pair meant to compose.
    pub fn render_overlay(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &MenuFrame<'_>,
        width: u32,
        height: u32,
    ) {
        self.draw(
            device,
            queue,
            view,
            frame,
            width,
            height,
            wgpu::LoadOp::Load,
        );
    }

    /// Shared body of [`render`](Self::render) and
    /// [`render_overlay`](Self::render_overlay); only the pass's load op
    /// differs between them.
    fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &MenuFrame<'_>,
        width: u32,
        height: u32,
        load: wgpu::LoadOp<wgpu::Color>,
    ) {
        self.ensure_gui(device, queue);
        // The panorama is vanilla's out-of-world background: `extractBackground`
        // draws it whenever `minecraft.level == null`, which for this renderer is
        // exactly `!frame.overlay` (the one overlay frame is the pause menu, drawn
        // over a live world). See `docs/menu-panorama.md`.
        // `frame.logo` is set for the title screen and nothing else, which is the
        // one screen whose `extractBackground` override is empty.
        let panorama_dim = panorama::dim_for_screen(frame.logo);
        if !frame.overlay {
            self.ensure_panorama(device, queue);
            if let Some(pano) = self.panorama.as_mut() {
                // Vanilla's **Panorama Scroll Speed** accessibility option. Applied
                // here, immediately before `advance`, rather than once at attach
                // time: the option is editable while the panorama is on screen (the
                // settings tree is itself a non-overlay screen, so it is drawn over
                // this very panorama), and a speed pushed only at attach would take
                // effect on the next launch.
                //
                // `set_speed` had **zero callers** before this — an island in the
                // "built, tested, reaches no pixels" direction — so the title screen
                // always span at `DEFAULT_SPIN_SPEED` whatever the option said.
                //
                // `None` leaves the renderer's own speed untouched; see
                // `MenuFrame::panorama_speed` on why an unstamped frame must not read
                // as a stationary one.
                if let Some(speed) = frame.panorama_speed {
                    pano.set_speed(speed);
                }
                pano.advance(crate::platform::Instant::now());
                pano.prepare(queue, width, height, panorama_dim);
            }
        }
        let panorama_drawn = !frame.overlay && self.panorama.is_some();
        let (logical_w, logical_h) = logical_canvas(frame.gui_scale, width, height);
        let geo = build(
            frame,
            self.sprites.as_ref().map(|s| s.atlas.as_ref()),
            self.font.as_deref(),
            logical_w,
            logical_h,
        );
        if geo.colour.len() > self.capacity_floats {
            self.capacity_floats = geo.colour.len().next_power_of_two();
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("menu-verts"),
                size: (self.capacity_floats * 4) as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&geo.colour));

        if let Some(sprites) = self.sprites.as_mut()
            && !geo.sprite.is_empty()
        {
            if geo.sprite.len() > sprites.capacity_floats {
                sprites.capacity_floats = geo.sprite.len().next_power_of_two();
                sprites.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("menu-sprite-verts"),
                    size: (sprites.capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&sprites.buffer, 0, bytemuck::cast_slice(&geo.sprite));
        }

        // Three colour/sprite draws, one pass (four with the panorama in front of
        // them). The split is `MenuGeometry::backdrop_floats`:
        // backdrop, then every GUI sprite, then the rest of the colour stream —
        // so a button's label lands *on* its nine-slice background rather than
        // under it. A render pass can rebind its pipeline, so this needs no
        // extra pass (and must not have one: the load op is only correct once).
        let backdrop_verts = (geo.backdrop_floats / FLOATS_PER_VERTEX) as u32;
        let colour_verts = (geo.colour.len() / FLOATS_PER_VERTEX) as u32;
        let sprite_verts = (geo.sprite.len() / SPRITE_FLOATS_PER_VERTEX) as u32;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("menu"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("menu-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // The panorama replaces the flat backdrop rather than sitting under
            // it: it covers every pixel (you are inside a closed cube), so an
            // opaque `BG` quad drawn afterwards would hide it entirely. The
            // `menu_background.png` wash vanilla puts on top on every screen but
            // the title screen travels as `panorama_dim` in its own shader — see
            // `panorama::dim_for_screen`, and `docs/menu-panorama.md` on why the
            // multiply and a black quad at alpha 64/255 are the same operation.
            if let Some(pano) = self.panorama.as_ref()
                && panorama_drawn
            {
                pano.draw(&mut pass);
            }
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            if backdrop_verts > 0 && !panorama_drawn {
                pass.draw(0..backdrop_verts, 0..1);
            }
            if let Some(sprites) = self.sprites.as_ref()
                && sprite_verts > 0
            {
                pass.set_pipeline(&sprites.pipeline);
                pass.set_bind_group(0, &sprites.bind_group, &[]);
                pass.set_vertex_buffer(0, sprites.buffer.slice(..));
                pass.draw(0..sprite_verts, 0..1);
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.buffer.slice(..));
            }
            if colour_verts > backdrop_verts {
                pass.draw(backdrop_verts..colour_verts, 0..1);
            }
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

