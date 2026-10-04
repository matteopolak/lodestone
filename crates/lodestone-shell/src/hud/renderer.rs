use super::*;

/// F3 text is diagnostic rather than input feedback; refresh its large vertex
/// stream at 10 Hz while drawing the resident buffer every frame. This keeps
/// coordinates and timing readable without paying CPU raster/vertex expansion
/// hundreds of times per second on an uncapped client.
pub(super) const DEBUG_GEOMETRY_REFRESH_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DebugGeometryStamp {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) gui_scale: u32,
    pub(super) font_revision: u64,
}

#[derive(Debug, Default)]
pub(super) struct DebugGeometryRefresh {
    pub(super) was_visible: bool,
    pub(super) stamp: Option<DebugGeometryStamp>,
    pub(super) refreshed_at: Option<Instant>,
}

impl DebugGeometryRefresh {
    pub(super) fn should_refresh(
        &mut self,
        now: Instant,
        visible: bool,
        stamp: DebugGeometryStamp,
    ) -> bool {
        if !visible {
            self.was_visible = false;
            return false;
        }
        let due = self.refreshed_at.is_none_or(|last| {
            now.saturating_duration_since(last) >= DEBUG_GEOMETRY_REFRESH_INTERVAL
        });
        let refresh = !self.was_visible || self.stamp != Some(stamp) || due;
        self.was_visible = true;
        if refresh {
            self.stamp = Some(stamp);
            self.refreshed_at = Some(now);
        }
        refresh
    }
}

/// GPU renderer for the HUD: a simple coloured-quad pipeline plus a growable
/// dynamic vertex buffer.
#[derive(Debug)]
pub struct HudRenderer {
    pub(super) pipeline: wgpu::RenderPipeline,
    /// [`Self::pipeline`] with a colour-inverting blend (`src * (1 - dst) +
    /// dst * (1 - src)`), for the crosshair: a white mark becomes `1 - dst`.
    pub(super) invert_pipeline: wgpu::RenderPipeline,
    /// The colour format [`Self::pipeline`] was built against — the *raw*
    /// (non-sRGB) sibling of the target's own format, since vanilla's 2-D GUI
    /// blending is not colour-managed.
    ///
    /// Stored so [`Self::flat_colour_view`] can hand every caller a view at
    /// exactly this format instead of each one re-deriving it. A `wgpu` pass
    /// requires its attachment's format to match every pipeline drawn into it,
    /// and that check happens at *submit* time on a pass this renderer only
    /// begins when the colour stream is non-empty — so a call site that derived
    /// the wrong view produced a validation abort in some frames and nothing at
    /// all in others. Keeping the format here removes the derivation from the
    /// call sites entirely.
    pub(super) flat_colour_format: wgpu::TextureFormat,
    pub(super) buffer: wgpu::Buffer,
    pub(super) capacity_floats: usize,
    /// Persistent F3 colour stream. It is drawn every frame but rebuilt and
    /// uploaded only when [`DebugGeometryRefresh`] requests a new snapshot.
    pub(super) debug_buffer: wgpu::Buffer,
    pub(super) debug_capacity_floats: usize,
    pub(super) debug_vertex_count: u32,
    pub(super) debug_refresh: DebugGeometryRefresh,
    pub(super) gui: Option<GuiHud>,
    /// The flat item atlas and the 3-D block-item pass, shared verbatim with the
    /// container screen. Both halves start detached.
    pub(super) icons: IconRenderer,
    /// The vanilla proportional font, resolved from the same resource-pack
    /// stack as the other atlases. `None` on a jar-less run, where the
    /// fixed-advance debug font draws instead.
    ///
    /// Unlike the atlases this needs **no GPU resources**, so it is resolved in
    /// [`HudRenderer::new`] rather than through an `attach_*` call — there is
    /// nothing for a caller to supply. [`HudRenderer::attach_font`] exists to
    /// override it (a resource pack, or a gate pinning a specific pack).
    ///
    /// **Re-resolved when the pack stack changes**, via
    /// [`Self::refresh_font_for_pack_generation`] at the top of the draw. It is
    /// a snapshot, not a permanent answer: a font resolved at bring-up and kept
    /// is one a server-pushed pack can never replace, and nothing goes red when
    /// that happens.
    pub(super) font: Option<Arc<VanillaFont>>,
    /// The `crate::resources::pack_generation` [`Self::font`] was resolved
    /// against — the only thing that can tell a current font from a stale one,
    /// since the pack stack is process-wide state with no change notification.
    pub(super) font_generation: u64,
    /// Monotonic identity for the font attached to this renderer. A raw Arc
    /// address can be reused after detach/re-attach; this revision makes an
    /// immediate retained-F3 rebuild structural instead of allocator-dependent.
    pub(super) font_revision: u64,
    /// The wall-clock origin every vitals animation's tick index is measured
    /// from — see `hud/anim.rs`'s module doc for why a wall clock stands in
    /// for the real 20Hz game tick here. Fixed at construction so a fresh
    /// renderer (a gate, a reconnect) starts at tick 0 rather than inheriting
    /// whatever the process's own uptime happens to be.
    pub(super) anim_start: Instant,
    /// Cross-frame heart blink/ghost state (`hud/anim::HeartAnim`).
    pub(super) heart_anim: anim::HeartAnim,
    /// Cross-frame per-slot hotbar pop timers (`hud/anim::HotbarPop`).
    pub(super) hotbar_pop: anim::HotbarPop,
    /// Cross-frame level-up flash state (`hud/anim::XpFlash`).
    pub(super) xp_flash: anim::XpFlash,
    /// Colour-stream buffer for the **recipe-book panel** pass
    /// ([`HudRenderer::render_recipe_book_panel`]), created lazily on the first
    /// frame the panel is open.
    ///
    /// Deliberately *not* [`Self::buffer`]: the panel draws after the HUD's own
    /// encoder has been submitted, and re-uploading the shared buffer would be
    /// correct only by virtue of `wgpu`'s queue ordering. A separate buffer
    /// makes that independence structural instead of subtle, for the cost of
    /// one allocation on the first open.
    pub(super) recipe_panel_buffer: Option<wgpu::Buffer>,
    /// Capacity of [`Self::recipe_panel_buffer`], in floats.
    pub(super) recipe_panel_capacity_floats: usize,
    /// The recipe-book panel's **textured** stream — vanilla's real
    /// `recipe_book/**` art, resolved against [`Self::gui`]'s atlas.
    ///
    /// Its own buffer rather than [`GuiHud::buffer`]: that one holds the HUD's
    /// own sprite verts for the same frame, and although the two draws are
    /// separately submitted (so a shared buffer would happen to work today),
    /// sharing it makes the panel's art silently dependent on the HUD having
    /// already been submitted this frame. One buffer per stream is the same
    /// choice [`Self::recipe_panel_buffer`] already makes for the colour stream.
    pub(super) recipe_panel_sprite_buffer: Option<wgpu::Buffer>,
    /// Capacity of [`Self::recipe_panel_sprite_buffer`], in floats.
    pub(super) recipe_panel_sprite_capacity_floats: usize,
}

/// The GPU resources for drawing HUD sprites from the vanilla GUI atlas: the
/// uploaded atlas texture, its textured pipeline + bind group, and a dynamic
/// vertex buffer. Present only once [`HudRenderer::attach_gui`] has run; absent
/// on jar-less / headless runs, where the HUD falls back to procedural quads.
#[derive(Debug)]
pub(super) struct GuiHud {
    pub(super) atlas: Arc<GuiAtlas>,
    #[allow(dead_code)]
    pub(super) gpu: GpuAtlas,
    pub(super) pipeline: wgpu::RenderPipeline,
    pub(super) bind_group: wgpu::BindGroup,
    pub(super) buffer: wgpu::Buffer,
    pub(super) capacity_floats: usize,
}

impl HudRenderer {
    /// Build the HUD's **flat-colour** pipeline (`hud.wgsl`: text, stack
    /// counts, durability bars, the chat/tab-list/scoreboard plates) for a
    /// target of `flat_colour_format`.
    ///
    /// **That argument is the target's *raw* (non-sRGB) format, not
    /// [`RenderTarget::format`](lodestone_render::target::RenderTarget::format)** —
    /// see [`Self::render_with_item_models`]'s `raw_view` parameter for why.
    /// It feeds this one pipeline and nothing else: `attach_gui`,
    /// `attach_items`, `attach_glint` and `attach_item_models` each take their
    /// own `color_format` and keep using the *corrected* one, because those
    /// pipelines draw into `view` rather than `raw_view`. Passing the corrected
    /// format here instead is not a compile error and not always a runtime one
    /// either — obtain the matching view from [`Self::flat_colour_view`] rather
    /// than deriving it at the call site.
    #[must_use]
    pub fn new(device: &wgpu::Device, flat_colour_format: wgpu::TextureFormat) -> Self {
        let color_format = flat_colour_format;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hud-shader"),
            source: wgpu::ShaderSource::Wgsl(HUD_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hud-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let make_pipeline = |label, blend| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
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
                    blend: Some(blend),
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
        let pipeline = make_pipeline("hud-pipeline", wgpu::BlendState::ALPHA_BLENDING);
        let invert = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::OneMinusDst,
            dst_factor: wgpu::BlendFactor::OneMinusSrc,
            operation: wgpu::BlendOperation::Add,
        };
        let invert_pipeline = make_pipeline(
            "hud-invert-pipeline",
            wgpu::BlendState { color: invert, alpha: wgpu::BlendComponent::OVER },
        );

        let capacity_floats = 4096;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hud-verts"),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let debug_capacity_floats = 4096;
        let debug_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hud-debug-verts"),
            size: (debug_capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            invert_pipeline,
            flat_colour_format,
            buffer,
            capacity_floats,
            debug_buffer,
            debug_capacity_floats,
            debug_vertex_count: 0,
            debug_refresh: DebugGeometryRefresh::default(),
            gui: None,
            icons: IconRenderer::new(),
            font: VanillaFont::shared(),
            // Stamped with the generation the font above was resolved against,
            // so `refresh_font_for_pack_generation` can tell "still current"
            // from "a pack landed since".
            font_generation: crate::resources::pack_generation(),
            font_revision: 0,
            anim_start: Instant::now(),
            heart_anim: anim::HeartAnim::new(),
            hotbar_pop: anim::HotbarPop::new(),
            xp_flash: anim::XpFlash::new(),
            recipe_panel_buffer: None,
            recipe_panel_capacity_floats: 0,
            recipe_panel_sprite_buffer: None,
            recipe_panel_sprite_capacity_floats: 0,
        }
    }

    /// The `raw_view` every render entry point on this type wants, built from
    /// `frame` at exactly the format [`Self::pipeline`] was compiled against.
    ///
    /// Both [`Self::render_with_item_models`] and
    /// [`Self::render_recipe_book_panel`] begin a pass whose attachment must
    /// match that pipeline's colour target. Deriving the view at the call site
    /// — `frame.create_view(target.raw_view_format())` — is correct only while
    /// the renderer was *also* built from `raw_view_format()`, and the two
    /// facts sat in different files (`app/lifecycle.rs` and `app/redraw.rs`).
    /// Asking the renderer that already knows the answer removes the chance to
    /// disagree; `AcquiredFrame`'s own doc records that both target
    /// implementations declare the sRGB and non-sRGB counterparts of their
    /// format up front, so this is always a legal view.
    #[must_use]
    pub fn flat_colour_view(&self, frame: &lodestone_render::AcquiredFrame) -> wgpu::TextureView {
        frame.create_view(self.flat_colour_format)
    }

    /// The colour format [`Self::new`] built the flat-colour pipeline for.
    /// Exists so a gate constructing its own views can assert it matches rather
    /// than discover the mismatch as a `wgpu` validation abort in whichever
    /// frames happen to carry colour vertices.
    #[must_use]
    pub fn flat_colour_format(&self) -> wgpu::TextureFormat {
        self.flat_colour_format
    }

    /// Whether vanilla text is in play. `false` means every string on screen is
    /// the fixed-advance 5×7 fallback — the state a jar-less run is in.
    ///
    /// A gate that means to measure vanilla text **must assert this**: without
    /// it, a missing jar silently degrades to the debug font and every
    /// "text drew something" assertion still passes.
    #[must_use]
    pub fn font_attached(&self) -> bool {
        self.font.is_some()
    }

    /// The vanilla font, for a caller that builds its **own** geometry and needs
    /// to lay out text with the same metrics this HUD does — the recipe-book
    /// panel's search box is the first (`app/redraw.rs`).
    ///
    /// Returns the `Arc` rather than a borrow so the caller can hold it across a
    /// `&mut self.render` borrow, which is the same constraint that made
    /// `recipe_toast_view` a free function.
    #[must_use]
    pub fn font(&self) -> Option<Arc<VanillaFont>> {
        self.font.clone()
    }

    /// Re-resolve the default font if the resource-pack stack has changed since
    /// it was last resolved.
    ///
    /// Called at the top of the draw, because there is no other seam that runs
    /// on this renderer when a pack lands: a server-pushed pack installs on the
    /// network thread and the Resource Packs screen writes the selection
    /// directly, and both only bump `crate::resources::pack_generation`. An
    /// unchanged generation costs one relaxed atomic load and an integer
    /// compare.
    ///
    /// This is the font half of a rule this renderer already needed for its GPU
    /// atlases: a resource resolved once at bring-up and never re-asked keeps
    /// serving the pre-pack answer forever, and nothing goes red when it does.
    /// Text drawn with a *custom* font id was never affected — that path asks
    /// per generation — which is why the symptom was "the pack's font applies in
    /// some places but not to chat".
    pub fn refresh_font_for_pack_generation(&mut self) {
        if vanilla_font::refresh_shared_font(&mut self.font, &mut self.font_generation) {
            self.font_revision = self.font_revision.wrapping_add(1);
        }
    }

    /// Drop back to the fixed-advance debug font. The executed negative control
    /// for every proportional-width assertion: with this called, a gate that
    /// claims to see vanilla advances must fail.
    pub fn detach_font(&mut self) {
        self.font = None;
        self.font_revision = self.font_revision.wrapping_add(1);
    }

    /// The command-suggestion popup's rect for a frame of this size — what the
    /// pointer is hit-tested against.
    ///
    /// This exists on the *renderer* rather than as a free function because the
    /// rect depends on glyph advances, and only the renderer knows which font is
    /// attached. It resolves the identical [`suggestion_layout`] the draw does,
    /// through the identical [`measure_text`], so a click can never land on a
    /// row the player is not looking at.
    ///
    /// `framebuffer_width`/`framebuffer_height` are **physical** pixels and
    /// `gui_scale` the raw option (`0` = auto), matching every other hit-test
    /// entry point here; the returned rect is in logical-canvas pixels, so
    /// convert the cursor with [`Self::canvas_cursor`] before testing it.
    #[must_use]
    pub fn suggestion_layout(
        &self,
        framebuffer_width: u32,
        framebuffer_height: u32,
        gui_scale: u32,
        opts: ChatDisplayOptions,
        popup: &SuggestionPopup<'_>,
    ) -> SuggestionLayout {
        let (w, h) =
            crate::menu::render::logical_canvas(gui_scale, framebuffer_width, framebuffer_height);
        let pose = chat_pose_scale(opts);
        let font = self.font.as_deref();
        suggestion_layout(w, h, pose, popup, |s| measure_text(font, s, pose))
    }

    /// [`chat_interaction_at`]'s own renderer-side wrapper, matching
    /// [`Self::suggestion_layout`]'s relationship to the free
    /// [`suggestion_layout`] function: this is the only place that knows
    /// which font is attached, so it is the only place that can build the
    /// `measure` closure the free function needs.
    ///
    /// **Approximates real per-run width with the concatenated plain text.**
    /// A `TextSpan`'s bold flag widens each of its glyphs by one pixel
    /// (`Font::advance_bold`); folding every run's text together before
    /// measuring loses that per-run distinction, so a hit-test boundary next
    /// to a bold/non-bold seam can be off by a glyph or two. Acceptable for a
    /// pointer hit-test (nothing here needs sub-pixel precision the way a
    /// layout gate does); worth revisiting with a real per-span measure if
    /// that ever proves visible.
    #[must_use]
    pub fn chat_interaction_at(
        &self,
        framebuffer_width: u32,
        framebuffer_height: u32,
        gui_scale: u32,
        opts: ChatDisplayOptions,
        chat_open: bool,
        entries: &[(Vec<lodestone_game::text::InteractiveSpan>, f32)],
        scrolled: usize,
        cursor: (f32, f32),
    ) -> Option<lodestone_game::text::InteractiveSpan> {
        let (cw, ch) =
            crate::menu::render::logical_canvas(gui_scale, framebuffer_width, framebuffer_height);
        let (cx, cy) = Self::canvas_cursor(framebuffer_width, framebuffer_height, gui_scale, cursor);
        let pose = chat_pose_scale(opts);
        let font = self.font.as_deref();
        let measure = |spans: &[TextSpan]| -> f32 {
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            measure_text(font, &joined, pose)
        };
        chat_interaction_at_scrolled(entries, cw, ch, opts, chat_open, scrolled, measure, cx, cy)
    }

    /// A physical-pixel cursor position in the logical-canvas pixels
    /// [`Self::suggestion_layout`] returns its rect in — the framebuffer divided
    /// by the effective integer GUI scale, vanilla's `guiScaled*`.
    #[must_use]
    pub fn canvas_cursor(
        framebuffer_width: u32,
        framebuffer_height: u32,
        gui_scale: u32,
        cursor: (f32, f32),
    ) -> (f32, f32) {
        let scale =
            crate::config::calculate_gui_scale(gui_scale, framebuffer_width, framebuffer_height)
                .max(1) as f32;
        (cursor.0 / scale, cursor.1 / scale)
    }

    /// Attach the vanilla GUI sprite atlas so the survival vitals (hearts,
    /// hunger, XP bar, hotbar frame + selection) render from real textures.
    /// Uploads the atlas, builds the textured pipeline, and binds it. Without
    /// this call the HUD keeps its procedural fallback — the jar-less runtime
    /// behaviour and the headless negative control the GPU gate exercises.
    pub fn attach_gui(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        atlas: Arc<GuiAtlas>,
    ) {
        let sp = item_icon::build_sprite_pipeline(
            device,
            queue,
            atlas.atlas(),
            HUD_SPRITE_WGSL,
            color_format,
            4096,
            "hud-sprite",
        );
        self.gui = Some(GuiHud {
            atlas,
            gpu: sp.gpu,
            pipeline: sp.pipeline,
            bind_group: sp.bind_group,
            buffer: sp.buffer,
            capacity_floats: sp.capacity_floats,
        });
    }

    /// Attach the flat item-sprite [`ItemAtlas`] so hotbar slots draw real item
    /// icons. Without this call the wells stay empty — the jar-less / headless
    /// behaviour. Delegates to the shared [`IconRenderer`]; the container screen
    /// has the identical call on `ContainerRenderer`.
    pub fn attach_items(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        atlas: Arc<ItemAtlas>,
    ) {
        self.icons
            .attach_items(device, queue, color_format, atlas, "hud-item");
    }

    /// Attach the 2-D GUI enchantment-glint pass, so an enchanted hotbar item
    /// shimmers. Must follow [`Self::attach_items`] — the pass masks
    /// itself against the item atlas — and is a no-op otherwise.
    pub fn attach_glint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        img: &lodestone_assets::Image,
    ) {
        self.icons
            .attach_glint(device, queue, color_format, img, "hud-glint");
    }

    /// Push vanilla's **Glint Speed**/**Glint Strength** accessibility options to
    /// the 2-D GUI glint pass, so an enchanted hotbar item shimmers at the
    /// player's chosen rate and opacity.
    ///
    /// This is the third of the three glint sites and the one that was missed:
    /// the world and hand passes share `crate::gpu::RenderState::glint_options`,
    /// while the GUI icon pass is a separate pipeline with its own uniform, so
    /// pushing to the first two left an enchanted item shimmering correctly in the
    /// world and in hand but at vanilla's default in a slot. The container screen
    /// has the identical call on `ContainerRenderer` — **both** are needed, since
    /// each owns its own [`IconRenderer`].
    ///
    /// Called once per presented frame from `app/redraw.rs` beside
    /// `RenderState::set_glint_options`, not once at attach time: the value can
    /// change in the settings screen while a container is open.
    pub fn set_glint_options(&mut self, speed: f64, strength: f32) {
        self.icons.set_glint_options(speed, strength);
    }

    /// This frame's GUI glint speed and strength as the uniform will see them —
    /// already clamped. Exists so a gate can predict what the shader gets rather
    /// than what was pushed; see [`item_icon::IconRenderer::glint_options`].
    #[must_use]
    pub fn glint_options(&self) -> (f64, f32) {
        self.icons.glint_options()
    }

    /// Attach the GPU side of the **3-D block-item** icon pass, so hotbar slots
    /// holding a block draw vanilla's isometric mini-block instead of an empty
    /// well.
    ///
    /// Every resource is *borrowed from the world renderer* rather than created;
    /// see [`item_icon::IconRenderer::attach_item_models`] for why each sharing
    /// is load-bearing.
    ///
    /// The **CPU** geometry is not captured here: it is passed per frame to
    /// [`render_with_item_models`](Self::render_with_item_models), because a
    /// per-slot lookup of the nine visible stacks is cheaper than cloning ~750
    /// items' quads. (`BlockModels::items` can now enumerate them, for consumers
    /// that do want an attach-time snapshot.)
    ///
    /// Without this call the icons simply do not draw — the jar-less / demo
    /// behaviour, and the negative control the pixel gate exercises.
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
            "hud-item-model",
        );
    }

    /// Build the block-entity icon pass during renderer bring-up, so the first
    /// hotbar or container frame containing a special item does not decode
    /// sheets and pattern masks on the frame thread.
    pub fn prewarm_special_icons(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.icons.prewarm_special(device, queue);
    }

    /// The flat item atlas attached by [`Self::attach_items`], if any.
    ///
    /// Exists so a caller building geometry that draws item icons — the
    /// recipe-book panel ([`Self::render_recipe_book_panel`]) — can ask for the
    /// atlas it will be drawn against instead of re-loading a second copy or
    /// threading one through as a new field. `None` on a jar-less run, which is
    /// the icon-less fallback path and the pixel gate's negative control.
    #[must_use]
    pub fn item_atlas(&self) -> Option<Arc<ItemAtlas>> {
        self.icons.item_atlas()
    }

    /// How many block-entity sheets the **special-renderer** icon pass has
    /// loaded — `0` until bring-up prewarming succeeds (or until a special
    /// frame builds it after a reload), and `0` forever on a jar-less run.
    ///
    /// Exists for the pixel gate, and it is not ornamental: a coverage-only
    /// assertion cannot tell "no chest in any slot" from "no pack, so a chest
    /// could never draw", and those two fail in opposite directions. The same
    /// distinction `RenderStats::block_entity_sheets_loaded` draws for the world
    /// pass.
    #[must_use]
    pub fn special_icon_sheets(&self) -> usize {
        self.icons.special_sheet_count()
    }

    /// Drop the special-renderer icon pass so bring-up or the next frame rebuilds
    /// it against the current pack stack — the reload-time counterpart of
    /// [`Self::attach_items`]/[`Self::attach_item_models`], which belongs in the
    /// same reload block they do. See `item_icon::IconRenderer::reload_special`
    /// for why this pass needs a *rebuild* where those two need a re-attach.
    pub fn reload_special_icons(&mut self) {
        self.icons.reload_special();
    }

    /// Draw the HUD over the current frame contents (a `Load` pass, no depth).
    ///
    /// Convenience wrapper over [`render_with_item_models`](Self::render_with_item_models)
    /// with no model set, no depth attachment, and [`AUTO_GUI_SCALE`](crate::config::AUTO_GUI_SCALE)
    /// (this call has no access to the persisted `Options.gui_scale` — its
    /// callers are the headless HUD gates and the scoreboard/tab-list overlays,
    /// none of which own one). Kept as the plain entry point so those existing
    /// callers are unchanged.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        raw_view: &wgpu::TextureView,
        frame: &HudFrame,
        width: u32,
        height: u32,
    ) {
        self.render_with_item_models(
            device,
            queue,
            view,
            raw_view,
            None,
            frame,
            None,
            crate::config::AUTO_GUI_SCALE,
            width,
            height,
        );
    }

    /// Draw the HUD, including the **3-D block-item** icons.
    ///
    /// `models` supplies the baked item geometry (`None` falls back to flat
    /// sprites only), and `depth` is a depth attachment matching the target size
    /// — normally
    /// [`RenderState::depth_view`](crate::gpu::RenderState::depth_view). Both are
    /// needed for a mini-block to draw; either being `None` degrades to the
    /// previous behaviour rather than erroring. `gui_scale` is the resolved
    /// `Options.gui_scale` (`0` = auto) — `app.rs`'s real windowed call site
    /// passes `menu::nav::MenuNav::gui_scale()` so a manual scale setting
    /// resizes the HUD exactly as it already resizes the menu screens.
    ///
    /// # Pass structure
    ///
    /// Three passes, in this order, all loading the existing colour:
    ///
    /// 1. **sprites** (no depth) — hotbar frame, vitals, flat item icons;
    /// 2. **item models** (depth, **cleared**) — the isometric mini-blocks;
    /// 3. **colour** (no depth) — text, stack counts, durability bars.
    ///
    /// The middle pass needs its own depth attachment and therefore its own pass.
    /// It *clears* depth rather than loading it: the world's depth is still
    /// resident from the terrain pass and would occlude a GUI item sitting at
    /// clip depth ~0.5. Nothing later in the frame reads depth, so clearing it
    /// here is free. Keeping it strictly between 1 and 3 is what leaves stack
    /// counts and durability bars on top of the icon rather than buried in it.
    ///
    /// `raw_view` is a second view of the *same* backing texture as `view`,
    /// reinterpreted at [`lodestone_render::target::RenderTarget::raw_view_format`]
    /// — the non-colour-managed sibling of `view`'s (sRGB) format. Only the
    /// flat-colour pass (text, stack counts, durability bars — `self.pipeline`,
    /// built at [`Self::new`] against that same raw format) draws into it;
    /// vanilla's own 2-D GUI blending is not colour-managed at all, and this is
    /// what makes our blend match it byte-for-byte instead of composited in
    /// linear space. Every other pass here (sprites, glint, 3-D item models)
    /// keeps using `view`, since those pipelines were built against `view`'s
    /// own (sRGB) format.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_item_models(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        raw_view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        frame: &HudFrame,
        models: Option<&BlockModels>,
        gui_scale: u32,
        width: u32,
        height: u32,
    ) {
        self.render_with_item_models_inner(
            device, queue, view, raw_view, depth, frame, models, gui_scale, width, height, None,
        );
    }

    /// Record the ordinary HUD into the caller's encoder without submitting it.
    /// Submit before drawing a recipe-book panel, which reuses the icon buffers.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_with_item_models(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        raw_view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        frame: &HudFrame,
        models: Option<&BlockModels>,
        gui_scale: u32,
        width: u32,
        height: u32,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        self.render_with_item_models_inner(
            device,
            queue,
            view,
            raw_view,
            depth,
            frame,
            models,
            gui_scale,
            width,
            height,
            Some(encoder),
        );
    }

    #[allow(clippy::too_many_arguments)]
    /// Draws `range` of the bound colour stream: the `crosshair` vertices
    /// through [`Self::invert_pipeline`], the rest through [`Self::pipeline`],
    /// in stream order.
    pub(super) fn draw_colour_range(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        range: std::ops::Range<u32>,
        crosshair: Option<std::ops::Range<u32>>,
    ) {
        pass.set_pipeline(&self.pipeline);
        let Some(inverted) = crosshair.filter(|c| c.start < range.end && range.start < c.end) else {
            pass.draw(range, 0..1);
            return;
        };
        let (lo, hi) = (inverted.start.max(range.start), inverted.end.min(range.end));
        if range.start < lo {
            pass.draw(range.start..lo, 0..1);
        }
        pass.set_pipeline(&self.invert_pipeline);
        pass.draw(lo..hi, 0..1);
        pass.set_pipeline(&self.pipeline);
        if hi < range.end {
            pass.draw(hi..range.end, 0..1);
        }
    }

    pub(super) fn render_with_item_models_inner(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        raw_view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        frame: &HudFrame,
        models: Option<&BlockModels>,
        gui_scale: u32,
        width: u32,
        height: u32,
        encoder: Option<&mut wgpu::CommandEncoder>,
    ) {
        // With the GUI atlas attached, the vitals come back as textured sprite
        // verts; otherwise the whole HUD is the procedural colour stream. The
        // item atlas, when attached, feeds the separate item-sprite stream.
        let gui_atlas = self.gui.as_ref().map(|g| Arc::clone(&g.atlas));
        let item_atlas = self.icons.item_atlas();
        // Only ask for model geometry when there is somewhere to draw it: no
        // attached pass or no depth attachment means the vertices could not be
        // rendered, and building them would be pure waste.
        let want_models = self.icons.models_attached() && depth.is_some();
        // Before the font is read, not after: a pack that landed since the last
        // frame has to reach *this* frame's glyphs, and this is the only seam
        // that runs on the renderer when one does.
        self.refresh_font_for_pack_generation();
        let font = self.font.clone();
        let debug_stamp = DebugGeometryStamp {
            width,
            height,
            gui_scale,
            font_revision: self.font_revision,
        };
        if self
            .debug_refresh
            .should_refresh(Instant::now(), frame.show_debug, debug_stamp)
        {
            let debug = build_debug_vertices(frame, width, height, gui_scale, font.as_deref());
            if debug.len() > self.debug_capacity_floats {
                self.debug_capacity_floats = debug.len().next_power_of_two();
                self.debug_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("hud-debug-verts"),
                    size: (self.debug_capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            self.debug_vertex_count = (debug.len() / FLOATS_PER_VERTEX) as u32;
            if !debug.is_empty() {
                queue.write_buffer(&self.debug_buffer, 0, bytemuck::cast_slice(&debug));
            }
        } else if !frame.show_debug {
            self.debug_vertex_count = 0;
        }
        // The vitals-cluster animation phases for this frame — see
        // `hud/anim.rs`. `tick` is the one place a wall clock enters; every
        // state machine it feeds is otherwise a pure function of that integer.
        let tick = anim::wall_tick(self.anim_start);
        let (heart_blink, display_health) = self.heart_anim.tick(tick, frame.health.unwrap_or(0.0));
        let regeneration_heart_index = anim::regeneration_heart_index(
            tick,
            regeneration_active(frame.effects),
            frame.max_health,
            frame.health.unwrap_or(0.0).max(0.0).ceil() as i32,
            display_health,
        );
        // `Option`, not `.unwrap_or(&[])`. Collapsing "the hotbar is hidden
        // this frame" and "the hotbar is genuinely empty" into the same empty
        // slice is exactly what made returning from a deeper menu (Options)
        // fire the pickup pop on every slot; see
        // `hud::anim::HotbarPop::tick`'s own doc.
        let hotbar_pop = self.hotbar_pop.tick(tick, frame.hotbar_items);
        let xp_flash = self.xp_flash.tick(tick, frame.xp.map(|(level, _)| level));
        let anim = HudAnim {
            heart_blink,
            display_health,
            tick,
            regeneration_heart_index,
            hotbar_pop,
            xp_flash,
        };
        let geo = HudGeometry::build_inner(
            frame,
            width,
            height,
            gui_scale,
            gui_atlas.as_deref(),
            item_atlas.as_deref(),
            models.filter(|_| want_models),
            font.as_deref(),
            anim,
            false,
        );
        // `geo.special` counts too. A hotbar holding nothing but a chest, with the
        // procedural frame suppressed, produces zero vertices in all four other
        // streams — bailing here would make the whole chest-icon chain
        // unreachable in exactly the configuration the pixel gate renders.
        if geo.verts.is_empty()
            && geo.sprite_verts.is_empty()
            && geo.item_verts.is_empty()
            && geo.model_verts.is_empty()
            && geo.special.is_empty()
            && self.debug_vertex_count == 0
        {
            return;
        }

        // Grow + upload the colour stream.
        if !geo.verts.is_empty() {
            if geo.verts.len() > self.capacity_floats {
                self.capacity_floats = geo.verts.len().next_power_of_two();
                self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("hud-verts"),
                    size: (self.capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&geo.verts));
        }

        // Grow + upload the sprite stream (only when an atlas is attached).
        if !geo.sprite_verts.is_empty()
            && let Some(g) = self.gui.as_mut()
        {
            if geo.sprite_verts.len() > g.capacity_floats {
                g.capacity_floats = geo.sprite_verts.len().next_power_of_two();
                g.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("hud-sprite-verts"),
                    size: (g.capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&g.buffer, 0, bytemuck::cast_slice(&geo.sprite_verts));
        }

        // Grow + upload both icon streams, and rewrite the model pass's GUI
        // camera for the current target size. Counts come back zero for a half
        // that is not attached, so the draws below need no further branching.
        //
        // `upload` feeds `width`/`height` straight to `gui_ortho`, the
        // projection that turns the 3-D block-item vertices' GUI-pixel-space
        // positions into clip space. Those vertices were posed by
        // `HudGeometry::build_inner` above, in the *logical* canvas (physical
        // framebuffer divided by the effective GUI scale) — so the projection
        // must be built for that same logical size, not the raw physical one,
        // or the model pass and the flat-sprite/colour passes it shares a
        // frame with would disagree about how big a "GUI pixel" is.
        let (logical_w, logical_h) = crate::menu::render::logical_canvas(gui_scale, width, height);
        let (item_count, model_count) = self.icons.upload(
            device,
            queue,
            &geo.item_verts,
            &geo.model_verts,
            &geo.special,
            // The hotbar has no carried stack, so every special icon is in the
            // slot stratum — see `IconRenderer::upload`'s `special_carried_from`.
            geo.special.len(),
            logical_w.max(1.0) as u32,
            logical_h.max(1.0) as u32,
            "hud-item-verts",
        );
        let glint_count = self
            .icons
            .upload_glint(device, queue, &geo.glint_verts, "hud-glint-verts");

        let colour_count = geo.vertex_count() as u32;
        let sprite_count = geo.sprite_vertex_count() as u32;
        let mut owned_encoder = encoder.is_none().then(|| {
            crate::gpu::gpu_timing::primary_encoder(device, "hud")
        });
        let encoder = encoder.unwrap_or_else(|| owned_encoder.as_mut().unwrap());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // GUI sprites (hotbar frame, vitals) first, then flat item icons over
            // the frame.
            if let Some(g) = &self.gui
                && sprite_count > 0
            {
                pass.set_pipeline(&g.pipeline);
                pass.set_bind_group(0, &g.bind_group, &[]);
                pass.set_vertex_buffer(0, g.buffer.slice(..));
                pass.draw(0..sprite_count, 0..1);
            }
            self.icons.draw_sprites(&mut pass, item_count);
            // The glint over the icons it belongs to, in the same pass so it
            // lands on top of them.
            self.icons.draw_glint_range(&mut pass, 0..glint_count);
        }

        // The 3-D block items, in their own pass because they are the only part
        // of the HUD that needs a depth buffer. One draw for the whole hotbar.
        self.icons.draw_models(
            encoder,
            view,
            depth,
            model_count,
            "hud-item-model-pass",
        );

        // The colour stream (text, stack counts) last, so it lands on top of both
        // kinds of icon.
        if self.debug_vertex_count > 0 || colour_count > 0 {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-colour-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(raw_view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            if self.debug_vertex_count > 0 {
                pass.set_vertex_buffer(0, self.debug_buffer.slice(..));
                pass.draw(0..self.debug_vertex_count, 0..1);
            }
            if colour_count > 0 {
                pass.set_vertex_buffer(0, self.buffer.slice(..));
                self.draw_colour_range(&mut pass, 0..colour_count, geo.crosshair.clone());
            }
        }
        if let Some(encoder) = owned_encoder {
            crate::gpu::gpu_timing::submit_primary_encoder(queue, encoder);
        }
    }

    /// Draw one frame of the **recipe-book panel** as its own pass,
    /// over whatever is already in `view`.
    ///
    /// This is the call that stops
    /// [`crate::container::recipe_book_panel_geometry_with_icons`] being an
    /// island: that function and its whole layout/hit-test family were built,
    /// unit-tested and reached zero pixels because nothing drew the vertices.
    /// Its own doc says "`app.rs` draws this in its own pass" — but the pipeline
    /// a colour/sprite/model triple needs already exists *here*, and
    /// `ContainerRenderer` exposes no entry point taking a prebuilt
    /// [`crate::container::RecipeBookPanelGeometry`], so the pass lives on the
    /// renderer that already owns matching pipelines rather than growing a
    /// fourth copy of them in `app.rs`.
    ///
    /// The streams are byte-compatible by construction, not by coincidence:
    /// `RecipeBookPanelGeometry`'s colour verts come from the same shared
    /// [`item_icon::ColourStream`] the HUD's do (6 floats, position already in
    /// NDC), and its item verts from the same `item_icon::push_sprite_quad`
    /// (8 floats). Both match [`HUD_WGSL`]/[`HUD_SPRITE_WGSL`]'s vertex layouts
    /// exactly.
    ///
    /// `gui_scale`/`width`/`height` must be the **same triple** the geometry and
    /// its layout were built from — see
    /// [`crate::container::recipe_book_panel_geometry`]'s own warning about what
    /// a mismatched triple does (every vertex lands outside the `[-1, 1]` clip
    /// range and the panel draws nothing at all).
    ///
    /// # Pass order
    ///
    /// Four passes, in the order
    /// [`ContainerRenderer::render_with_icons_scaled`](crate::container::ContainerRenderer)
    /// already uses for the main panel:
    ///
    /// 1. `verts[..chrome]` — panel, tabs, buttons, slot wells, page arrows.
    /// 2. the 3-D block-item models.
    /// 3. the flat item sprites.
    /// 4. `verts[chrome..]` — stack-count digits, durability bars, and the
    ///    jar-less fallback swatches.
    ///
    /// **The split is load-bearing and this used to be wrong.** The geometry
    /// previously kept one unsplit colour stream drawn entirely in pass 1, so a
    /// recipe result's count digits were submitted before its icon and vanished
    /// underneath it — the owner-reported "the item counts are behind the items
    /// (at least the blocks)". The "at least" is the tell: a flat item sprite is
    /// mostly transparent around its edges so some digits bled through, whereas
    /// a 3-D block model fills the bottom-right corner opaquely and hid them
    /// completely.
    ///
    /// There is no depth compare on this path, so submission order is the *only*
    /// thing deciding z. Collapsing these four passes back into fewer, or
    /// drawing all of `verts` in pass 1, reproduces the bug — see
    /// [`crate::container::RecipeBookPanelGeometry::chrome_vertex_count`].
    ///
    /// `raw_view` is the same non-colour-managed reinterpretation
    /// [`Self::render_with_item_models`] takes — see that method's doc. Every
    /// pass here that draws `self.pipeline` (the flat-colour chrome fallback
    /// and the count-digit/durability overlay) uses it instead of `view`; the
    /// sprite/model passes are unaffected and keep `view`.
    #[allow(clippy::too_many_arguments)]
    pub fn render_recipe_book_panel(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        raw_view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        geo: &crate::container::RecipeBookPanelGeometry,
        gui_scale: u32,
        width: u32,
        height: u32,
    ) {
        if geo.verts.is_empty()
            && geo.item_verts.is_empty()
            && geo.model_verts.is_empty()
            && geo.sprites.is_empty()
        {
            return;
        }

        if !geo.verts.is_empty() {
            if geo.verts.len() > self.recipe_panel_capacity_floats {
                self.recipe_panel_capacity_floats = geo.verts.len().next_power_of_two();
                self.recipe_panel_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("hud-recipe-panel-verts"),
                    size: (self.recipe_panel_capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
            if let Some(buffer) = &self.recipe_panel_buffer {
                queue.write_buffer(buffer, 0, bytemuck::cast_slice(&geo.verts));
            }
        }

        // Same logical-canvas expression the geometry itself used, so the
        // model pass's GUI projection agrees with the vertices it is drawing.
        let (logical_w, logical_h) = crate::menu::render::logical_canvas(gui_scale, width, height);

        // Resolve vanilla's real `recipe_book/**` art against whatever GUI atlas
        // is bound. This is the whole of the texture fix: `GuiAtlas` already
        // stitches every `gui/sprites/**` in the pack, so the sprites needed no
        // new atlas, pipeline or bind group — only ids and destination rects,
        // which the geometry carries (see `RecipeBookSprite`).
        //
        // Unknown ids resolve to nothing and are skipped, so a pack missing one
        // sprite loses that sprite and not the panel. On a jar-less run
        // `self.gui` is `None` and this is empty, leaving the flat-fill fallback
        // in `verts[..chrome]` as the whole picture — which is exactly what
        // every existing headless geometry gate measures.
        let panel_sprite_verts: Vec<f32> = match &self.gui {
            Some(g) => {
                let mut out = Vec::new();
                for s in &geo.sprites {
                    // A `src` is a fixed sub-rect of a larger sheet (the panel
                    // page); `None` is the ordinary whole-sprite blit, which
                    // must go through `geometry` so the sprite's own
                    // `GuiScaling` is honoured.
                    match s.src {
                        Some(src) => {
                            if let Some(q) = g.atlas.subregion_quad_declared(
                                s.id,
                                crate::container::RECIPE_PANEL_DECLARED,
                                src,
                                s.dst,
                            ) {
                                item_icon::push_sprite_quad(
                                    &mut out,
                                    logical_w,
                                    logical_h,
                                    q,
                                    [1.0, 1.0, 1.0, 1.0],
                                );
                            }
                        }
                        None => {
                            let [x, y, w, h] = s.dst;
                            for q in g.atlas.geometry(s.id, x, y, w, h) {
                                item_icon::push_sprite_quad(
                                    &mut out,
                                    logical_w,
                                    logical_h,
                                    q,
                                    [1.0, 1.0, 1.0, 1.0],
                                );
                            }
                        }
                    }
                }
                out
            }
            None => Vec::new(),
        };
        if !panel_sprite_verts.is_empty() {
            if panel_sprite_verts.len() > self.recipe_panel_sprite_capacity_floats {
                self.recipe_panel_sprite_capacity_floats =
                    panel_sprite_verts.len().next_power_of_two();
                self.recipe_panel_sprite_buffer =
                    Some(device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("hud-recipe-panel-art-verts"),
                        size: (self.recipe_panel_sprite_capacity_floats * 4)
                            as wgpu::BufferAddress,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }));
            }
            if let Some(buffer) = &self.recipe_panel_sprite_buffer {
                queue.write_buffer(buffer, 0, bytemuck::cast_slice(&panel_sprite_verts));
            }
        }
        let panel_art_count = (panel_sprite_verts.len() / SPRITE_FLOATS_PER_VERTEX) as u32;

        let (item_count, model_count) = self.icons.upload(
            device,
            queue,
            &geo.item_verts,
            &geo.model_verts,
            &geo.special,
            // The panel has no carried stack, so every special icon (none, in
            // the current corpus) is in the slot stratum — the same argument
            // `render_with_item_models` makes for the hotbar.
            geo.special.len(),
            logical_w.max(1.0) as u32,
            logical_h.max(1.0) as u32,
            "hud-recipe-panel-item-verts",
        );

        let colour_count = geo.vertex_count() as u32;
        // Clamped against what the stream actually holds, so a geometry built
        // by an older producer (or a hand-built one in a test) can never make
        // this draw a range past the end of its own buffer.
        let chrome_count = (geo.chrome_vertex_count as u32).min(colour_count);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hud-recipe-panel"),
        });
        // Pass 1: chrome, under everything — **but only when the real art did
        // not resolve.**
        //
        // The flat fills are the jar-less fallback and nothing else (see
        // `RecipeBookPanelGeometry::sprites` and the palette's own doc), and
        // drawing them *under* the art is not free: vanilla's `recipe_book.png`
        // page has **transparent rounded corners**, so an opaque near-black
        // rectangle behind it shows through at all four of them and the panel
        // reads as a square with dark corner pixels. That is the owner's "the
        // rounded corners have pixels filling them in to be square" report — the
        // fill is not covered by the sprite, it is *revealed* by it.
        //
        // Keyed on `panel_art_count`, not on `self.gui.is_some()`: a pack that
        // carries the atlas but none of the `recipe_book/**` ids resolves no
        // sprites, and that run still wants the fallback rather than an invisible
        // panel.
        if chrome_count > 0
            && panel_art_count == 0
            && let Some(buffer) = &self.recipe_panel_buffer
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-recipe-panel-colour-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(raw_view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..chrome_count, 0..1);
        }
        // Pass 1b: vanilla's real art, over the flat-fill fallback. The panel
        // page is fully opaque, so with an atlas bound this hides the fallback
        // entirely rather than blending with it — which is why the fallback
        // palette can stay unchanged and still be the right jar-less picture.
        if panel_art_count > 0
            && let Some(g) = &self.gui
            && let Some(buffer) = &self.recipe_panel_sprite_buffer
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-recipe-panel-art-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&g.pipeline);
            pass.set_bind_group(0, &g.bind_group, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..panel_art_count, 0..1);
        }
        // Pass 2: the 3-D block-item models, over the wells.
        self.icons.draw_models(
            &mut encoder,
            view,
            depth,
            model_count,
            "hud-recipe-panel-item-model-pass",
        );
        // Passes 3 and 4: flat sprites, then the icon-overlay colour range —
        // count digits and durability bars — which must land over *both* kinds
        // of icon. Order matches `ContainerRenderer::render_with_icons_scaled`'s
        // own `container-item-pass`, but the two now run as **separate** render
        // passes rather than one shared pass: pass 3's icon-sprite pipeline is
        // still built against `view`'s (sRGB) format, while pass 4's
        // `self.pipeline` is built against `raw_view`'s — and a `wgpu` pass's
        // colour attachment format is fixed for every pipeline drawn into it, so
        // mixing the two in one pass no longer validates. Both passes `Load`
        // the same underlying texture (see `item_icon::load_colour_attachment`),
        // so splitting them changes nothing about what lands on screen.
        if item_count > 0 {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-recipe-panel-sprite-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.icons.draw_sprites(&mut pass, item_count);
        }
        if colour_count > chrome_count
            && let Some(buffer) = &self.recipe_panel_buffer
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-recipe-panel-count-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(raw_view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(chrome_count..colour_count, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}
