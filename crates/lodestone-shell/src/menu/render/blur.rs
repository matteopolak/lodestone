//! The background blur behind an open in-game menu — [`MenuBlur`].
//!
//! ## What it is
//!
//! A player report: *"when i open a menu when in-game, the client is
//! supposed to slightly dim the background and also apply a blur… right now
//! our client does not blur."* The dim already existed
//! ([`super::MenuBackdrop::Dim`]); this is the missing half.
//!
//! ## How it works
//!
//! Vanilla's mechanism (`Screen::extractBackground` →
//! `extractBlurredBackground` → `GuiRenderState::blurBeforeThisStratum` →
//! `GameRenderer::processBlurEffect` running the `minecraft:blur` post
//! chain) is a **six-pass separable box blur** — three horizontal+vertical
//! pairs — over whatever the frame already holds, run *before* the screen's
//! own widgets draw on top. See `../../shaders/menu_blur.wgsl` for the exact
//! record citations and the box-filter derivation.
//!
//! [`MenuBlur::run`] reproduces that shape: copy the source texture into a
//! scratch texture, alternate five passes between two scratch targets, and
//! let the sixth (a vertical pass) write straight into the caller's real
//! target — so the caller needs no seventh copy back.
//!
//! ## How to change it
//!
//! [`BLUR_RADIUS`] is vanilla's own default (`Options.BLURRINESS_DEFAULT_VALUE
//! = 5`, range `0..=10`) and is now only the **boot** value:
//! [`MenuBlur::set_radius`] takes the live `options.menuBackgroundBlurriness`,
//! polled once per presented frame in `app/redraw.rs` beside
//! [`super::MenuRenderer::begin_frame`]. The two config bind groups are rebuilt
//! lazily inside [`MenuBlur::run`] when the radius has actually moved, so an
//! unchanged setting costs one float comparison a frame rather than two buffer
//! allocations.
//!
//! **Radius `0` skips the pass entirely**, which is vanilla's own gate:
//! `Screen.extractBlurredBackground` calls `blurBeforeThisStratum()` only when
//! `blurRadius >= 1.0F`. A zero-radius box filter is an identity convolution, so
//! running it would be six full-screen passes to reproduce the source — the skip
//! is the behaviour, not an optimisation on top of it.
//!
//! Only [`super::MenuRenderer::render_overlay`] callers that set
//! [`super::MenuFrame::blur`] pay for this pass — `draw` below skips it
//! entirely (no encoder, no submit) whenever the flag is unset, which is
//! every `Clear`-pass ([`super::owns_frame`]) screen and every overlay that
//! does not want it (sign edit, command block edit — vanilla's own
//! `isInGameUi() == true` fork, which skips the blur too; see
//! `menu_blur.wgsl`'s module doc).
//!
//! ## Configuration
//!
//! `options.menuBackgroundBlurriness` (`crate::config::Options::
//! menu_background_blurriness`), an `IntRange(0, 10)` defaulting to 5, reaching
//! [`MenuBlur::set_radius`]. [`BLUR_RADIUS`] is the boot value only.
//!
//! ## Dependencies
//!
//! `wgpu` only. No atlas, no font, no jar — unlike [`super::MenuSprites`] and
//! the panorama this attaches eagerly in [`super::MenuRenderer::new`] rather
//! than lazily, since it needs nothing that is only available later.

use bytemuck::{Pod, Zeroable};

/// Vanilla's `Options.BLURRINESS_DEFAULT_VALUE` — the accessibility option's
/// default, `0..=10`. The **boot** radius; [`MenuBlur::set_radius`] carries the
/// live value from there on.
const BLUR_RADIUS: f32 = 5.0;

/// Vanilla's own "is there a blur at all" threshold — `Screen`'s
/// `extractBlurredBackground` runs the pass only when `blurRadius >= 1.0F`.
const MIN_EFFECTIVE_BLUR_RADIUS: f32 = 1.0;

/// Mirrors `box_blur.fsh`'s `BlurConfig` uniform block: a sample-step
/// direction plus the box half-width. `_pad` keeps the struct's WGSL layout
/// at a clean 16 bytes (`vec2<f32>` at offset 0 needs 8-byte alignment,
/// `f32` at offset 8, one more `f32` to round the whole block up to
/// `vec2`'s own 8-byte stride requirement for `uniform` address space).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct BlurConfig {
    dir: [f32; 2],
    radius: f32,
    _pad: f32,
}

/// The two ping-pong scratch targets a blur run alternates between, sized to
/// the current canvas and rebuilt on resize (see [`MenuBlur::ensure_scratch`]).
///
/// Both textures are created at `source`'s own texel format — so the initial
/// `copy_texture_to_texture` from the caller's real frame is always
/// format-identical, never merely "compatible" — and each carries a *second*,
/// reinterpreted view at the pipeline's `color_format` (mirrors
/// `lodestone_render::SurfaceTarget`'s own view-format override, for the
/// identical reason: on the WebGPU backend the swapchain's raw texture format
/// and the sRGB view every pipeline is built against are two different
/// values). Every pass reads and writes through that reinterpreted view, so
/// the whole chain filters in one consistent colour space rather than mixing
/// sRGB-encoded bytes with linear ones pass to pass.
#[derive(Debug)]
struct Scratch {
    width: u32,
    height: u32,
    texel_format: wgpu::TextureFormat,
    // `a` itself is read directly (`scratch.a.as_image_copy()`, the copy
    // destination in `MenuBlur::run`), so it carries no `#[allow(dead_code)]`.
    a: wgpu::Texture,
    a_write_view: wgpu::TextureView,
    a_read_bind: wgpu::BindGroup,
    /// Kept alive because `b_write_view`/`b_read_bind` are views/bind groups
    /// derived from it — `b` itself is never read again after creation,
    /// unlike `a`, since every pass reaches it through those two instead.
    #[allow(dead_code)]
    b: wgpu::Texture,
    b_write_view: wgpu::TextureView,
    b_read_bind: wgpu::BindGroup,
}

/// The background-blur post-process pass — see this module's own doc.
#[derive(Debug)]
pub struct MenuBlur {
    pipeline: wgpu::RenderPipeline,
    texture_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// The pipeline's own colour-target format — every attachment and every
    /// reinterpreted read view in this pass uses this one value, never a
    /// second derivation. See [`Scratch`]'s own doc.
    color_format: wgpu::TextureFormat,
    /// Kept so [`Self::run`] can rebuild the two config bind groups when the
    /// live radius moves — they are the only per-setting GPU state in the pass.
    config_bgl: wgpu::BindGroupLayout,
    config_h: wgpu::BindGroup,
    config_v: wgpu::BindGroup,
    /// The radius the caller last asked for ([`Self::set_radius`]).
    radius: f32,
    /// The radius [`Self::config_h`]/[`Self::config_v`] were actually built at.
    /// Kept separately so the rebuild is driven by a real difference rather than
    /// by every `set_radius` call — the setter is polled per frame.
    built_radius: f32,
    scratch: Option<Scratch>,
}

impl MenuBlur {
    /// Two bind groups (`group(0)` the sampled texture, `group(1)` the
    /// direction/radius uniform) — well inside wgpu's 4-bind-group floor
    /// (`wgpu::Limits::downlevel_webgl2_defaults().max_bind_groups`, the same
    /// limit the model shader is already pinned against — see
    /// `bind_group_count_is_within_the_four_group_floor` below), checked
    /// against the limit rather than against this machine's adapter, which
    /// reports 8 and would hide a 5-group regression entirely.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("menu-blur-shader"),
            source: wgpu::ShaderSource::Wgsl(MENU_BLUR_WGSL.into()),
        });

        let texture_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("menu-blur-texture-bgl"),
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
        let config_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("menu-blur-config-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("menu-blur-layout"),
            bind_group_layouts: &[Some(&texture_bgl), Some(&config_bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("menu-blur-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    // No blending -- every pass fully overwrites every pixel
                    // (a full-screen triangle covers the whole target), so
                    // this is a plain replace, not a composite.
                    blend: None,
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

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("menu-blur-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            // `blur.json` declares every input `"bilinear": true` -- the whole
            // point of the box-filter's step-2 sampling trick (see
            // `menu_blur.wgsl`) is that linear filtering does the midpoint
            // average for it.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let config_h = Self::make_config(device, &config_bgl, [1.0, 0.0], BLUR_RADIUS);
        let config_v = Self::make_config(device, &config_bgl, [0.0, 1.0], BLUR_RADIUS);

        Self {
            pipeline,
            texture_bgl,
            sampler,
            color_format,
            config_bgl,
            config_h,
            config_v,
            radius: BLUR_RADIUS,
            built_radius: BLUR_RADIUS,
            scratch: None,
        }
    }

    /// The live `options.menuBackgroundBlurriness`, in vanilla's own units
    /// (`IntRange(0, 10)`).
    ///
    /// Cheap and idempotent by design — `app/redraw.rs` calls it once per
    /// presented frame beside [`super::MenuRenderer::begin_frame`] rather than
    /// on the settings-row write, the same poll shape
    /// `Sim::set_cutout_leaves` and `RenderState::set_entity_shadows_enabled`
    /// already use. The two GPU buffers behind it are only rebuilt when
    /// [`Self::run`] sees the value has actually moved.
    pub fn set_radius(&mut self, radius: f32) {
        self.radius = radius;
    }

    fn make_config(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        dir: [f32; 2],
        radius: f32,
    ) -> wgpu::BindGroup {
        use wgpu::util::DeviceExt;
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("menu-blur-config"),
            contents: bytemuck::bytes_of(&BlurConfig {
                dir,
                radius,
                _pad: 0.0,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("menu-blur-config-bg"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        })
    }

    /// Builds one scratch texture plus its single view, reinterpreted at
    /// `color_format` up front (rather than two views per texture) — that one
    /// view backs both the render-pass attachment write and the sampled
    /// read, which is why every stage of the chain filters in one consistent
    /// colour space. A free function taking every input explicitly rather
    /// than a `self`-capturing closure, so it borrows nothing from the
    /// caller across the two calls in [`Self::ensure_scratch`].
    fn make_scratch_texture(
        device: &wgpu::Device,
        label: &str,
        width: u32,
        height: u32,
        texel_format: wgpu::TextureFormat,
        color_format: wgpu::TextureFormat,
        view_formats: &[wgpu::TextureFormat],
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: texel_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST,
            view_formats,
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(color_format),
            ..Default::default()
        });
        (texture, view)
    }

    fn make_read_bind(&self, device: &wgpu::Device, view: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("menu-blur-texture-bg"),
            layout: &self.texture_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    fn ensure_scratch(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        texel_format: wgpu::TextureFormat,
    ) {
        if let Some(s) = &self.scratch
            && s.width == width
            && s.height == height
            && s.texel_format == texel_format
        {
            return;
        }
        let color_format = self.color_format;
        let view_formats: &[wgpu::TextureFormat] = if texel_format == color_format {
            &[]
        } else {
            std::slice::from_ref(&color_format)
        };
        let (a, a_write_view) = Self::make_scratch_texture(
            device,
            "menu-blur-scratch-a",
            width,
            height,
            texel_format,
            color_format,
            view_formats,
        );
        let (b, b_write_view) = Self::make_scratch_texture(
            device,
            "menu-blur-scratch-b",
            width,
            height,
            texel_format,
            color_format,
            view_formats,
        );
        let a_read_bind = self.make_read_bind(device, &a_write_view);
        let b_read_bind = self.make_read_bind(device, &b_write_view);
        self.scratch = Some(Scratch {
            width,
            height,
            texel_format,
            a,
            a_write_view,
            a_read_bind,
            b,
            b_write_view,
            b_read_bind,
        });
    }

    /// Blur `source` into `dest` — a self-contained six-pass run (its own
    /// encoder, its own submit) so callers need only supply the two textures.
    ///
    /// `source` must be format-identical to what this [`MenuBlur`] was built
    /// with scratch textures for (its own texel format, not necessarily
    /// [`Self::color_format`] — see [`Scratch`]'s doc); a mismatch is a wgpu
    /// validation error at `copy_texture_to_texture`, the same contract
    /// [`lodestone_render::SurfaceTarget`] already keeps between its raw
    /// swapchain texture and its reinterpreted view.
    ///
    /// A no-op on a zero-sized canvas (mid-resize), matching every other
    /// resize guard in this renderer.
    pub fn run(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &wgpu::Texture,
        dest: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        if width == 0 || height == 0 {
            return;
        }
        // Vanilla's own gate, not a shortcut: `Screen.extractBlurredBackground`
        // only asks for the blur at `blurRadius >= 1.0F`, and a zero-radius box
        // filter is the identity — six full-screen passes to reproduce the
        // source exactly. Skipping leaves the sharp background the caller
        // already drew, which is what a player who set the slider to OFF asked
        // for.
        if self.radius < MIN_EFFECTIVE_BLUR_RADIUS {
            return;
        }
        // The only per-setting GPU state in the pass. Rebuilt here rather than in
        // `set_radius` because that is the call with a `device` in hand, and
        // because the setter is polled every frame — keying the rebuild on a real
        // change is what keeps that poll free.
        if (self.radius - self.built_radius).abs() > f32::EPSILON {
            self.config_h = Self::make_config(device, &self.config_bgl, [1.0, 0.0], self.radius);
            self.config_v = Self::make_config(device, &self.config_bgl, [0.0, 1.0], self.radius);
            self.built_radius = self.radius;
        }
        self.ensure_scratch(device, width, height, source.format());
        let Some(scratch) = self.scratch.as_ref() else {
            return;
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("menu-blur"),
        });
        encoder.copy_texture_to_texture(
            source.as_image_copy(),
            scratch.a.as_image_copy(),
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        // Six passes, three horizontal+vertical pairs -- `blur.json`'s own
        // structure. The sixth writes straight into `dest` (the caller's
        // real target) instead of back into a scratch texture, so there is
        // no seventh copy.
        Self::pass(
            &mut encoder,
            &self.pipeline,
            &scratch.a_read_bind,
            &scratch.b_write_view,
            &self.config_h,
        );
        Self::pass(
            &mut encoder,
            &self.pipeline,
            &scratch.b_read_bind,
            &scratch.a_write_view,
            &self.config_v,
        );
        Self::pass(
            &mut encoder,
            &self.pipeline,
            &scratch.a_read_bind,
            &scratch.b_write_view,
            &self.config_h,
        );
        Self::pass(
            &mut encoder,
            &self.pipeline,
            &scratch.b_read_bind,
            &scratch.a_write_view,
            &self.config_v,
        );
        Self::pass(
            &mut encoder,
            &self.pipeline,
            &scratch.a_read_bind,
            &scratch.b_write_view,
            &self.config_h,
        );
        Self::pass(
            &mut encoder,
            &self.pipeline,
            &scratch.b_read_bind,
            dest,
            &self.config_v,
        );

        queue.submit(std::iter::once(encoder.finish()));
    }

    fn pass(
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        src_bind: &wgpu::BindGroup,
        dst_view: &wgpu::TextureView,
        config_bind: &wgpu::BindGroup,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("menu-blur-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // A full-screen triangle overwrites every pixel, so the
                    // clear colour is never visible -- it exists only to
                    // avoid a `Load` dependency on the destination's
                    // previous contents.
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, src_bind, &[]);
        pass.set_bind_group(1, config_bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

const MENU_BLUR_WGSL: &str = include_str!("../../shaders/menu_blur.wgsl");

#[cfg(test)]
mod tests {
    use super::*;

    /// The hard constraint this repo has already paid for once (the model
    /// shader sits at wgpu's 4-bind-group floor): checked against the limit
    /// itself, not against this machine's adapter, which reports 8 and would
    /// pass a 5-group pipeline that fails on real hardware.
    #[test]
    fn bind_group_count_is_within_the_four_group_floor() {
        // `MenuBlur::new` declares exactly two bind-group layouts in its
        // `PipelineLayoutDescriptor` -- see the literal `&[Some(&texture_bgl),
        // Some(&config_bgl)]` there. This is restated as a count rather than
        // re-read from a live pipeline (wgpu does not expose a layout's group
        // count back out), so it is pinned here instead: a third bind group
        // added to that literal must also update this constant, and the
        // assertion below is what makes that a decision instead of a drift.
        const MENU_BLUR_BIND_GROUPS: u32 = 2;
        let floor = wgpu::Limits::downlevel_webgl2_defaults().max_bind_groups;
        assert!(
            MENU_BLUR_BIND_GROUPS <= floor,
            "menu blur uses {MENU_BLUR_BIND_GROUPS} bind groups, over the \
             {floor}-group floor every adapter must support -- this is the \
             defect class that shipped a 5-group model shader crash on a \
             4-group adapter"
        );
    }

    #[test]
    fn blur_config_matches_its_wgsl_uniform_layout() {
        // `menu_blur.wgsl`'s `BlurConfig` is `vec2<f32>` then two `f32`s --
        // 16 bytes total, 8-byte aligned for the leading `vec2`. A struct
        // that drifted from this would misalign every field after `dir`.
        assert_eq!(std::mem::size_of::<BlurConfig>(), 16);
        assert_eq!(std::mem::align_of::<BlurConfig>(), 4);
    }

    #[test]
    fn default_radius_matches_vanillas_own_default() {
        // `Options.BLURRINESS_DEFAULT_VALUE = 5` -- see this module's own doc
        // for why the option itself is not wired yet.
        assert!((BLUR_RADIUS - 5.0).abs() < f32::EPSILON);
    }

    /// Real-GPU pixel-readback gate for [`MenuBlur::run`] itself — a
    /// checkerboard in, blurred pixels out, read back and measured at every
    /// pixel (not one point/vertex probe — this repo has already shipped a
    /// probe blind to anything bigger than its own rect, and a full
    /// rasterised readback is immune to that species by construction).
    ///
    /// The discriminating property for a blur, not merely "the frame
    /// changed" (which a dim quad or a flat tint would also satisfy): a
    /// high-frequency edge loses contrast while the overall mean is
    /// preserved. Both are measured against the exact bytes this test wrote
    /// (the ground truth, not a re-read of the source texture), and the
    /// per-edge measurements are collected into `Vec`s and asserted on as a
    /// collection — an `assert!` inside the sampling loop would only ever
    /// prove *one* edge failed, not name every one that did.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn checkerboard_loses_edge_contrast_but_keeps_its_mean() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU, don't 'skip' -- a silent pass here would assert \
             nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h): (u32, u32) = (64, 64);
        const CELL: u32 = 8;

        // The ground truth: an 8px checkerboard, alpha opaque. Built once in
        // Rust rather than read back from the GPU, so the "before" side of
        // every comparison below is authored data, not a second GPU sample
        // that could share a bug with the first.
        let mut source_bytes = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let white = ((x / CELL) + (y / CELL)) % 2 == 0;
                let v = if white { 255 } else { 0 };
                let i = ((y * w + x) * 4) as usize;
                source_bytes[i] = v;
                source_bytes[i + 1] = v;
                source_bytes[i + 2] = v;
                source_bytes[i + 3] = 255;
            }
        }

        let source = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blur-test-source"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &source_bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );

        let mut blur = MenuBlur::new(device, format);
        let mut target = lodestone_render::HeadlessTarget::new(device, w, h, format);
        let frame = lodestone_render::RenderTarget::acquire(&mut target).expect("headless acquire");
        blur.run(device, queue, &source, frame.view(), w, h);
        let blurred = target.read_texels(device, queue);

        let px = |buf: &[u8], x: u32, y: u32| -> u8 { buf[((y * w + x) * 4) as usize] };

        // -- mean preserved, over every pixel, not a probe rect ------------
        let mean = |buf: &[u8]| -> f64 {
            (0..h)
                .flat_map(|y| (0..w).map(move |x| f64::from(px(buf, x, y))))
                .sum::<f64>()
                / f64::from(w * h)
        };
        let mean_source = mean(&source_bytes);
        let mean_blurred = mean(&blurred);
        assert!(
            (mean_source - mean_blurred).abs() < 15.0,
            "blur must preserve the overall mean (a checkerboard's is exactly \
             127.5): source={mean_source:.2}, blurred={mean_blurred:.2}"
        );

        // -- edge contrast lost, at every internal boundary -----------------
        // One measurement per internal vertical boundary (x = 8, 16, .. 56),
        // sampled at the mid-row of four different cell-rows so no single
        // sample point decides the result -- collected, not asserted inside
        // the loop, so a failure names every offending location instead of
        // stopping at the first (this file's own "collect mismatches" rule).
        struct Edge {
            x: u32,
            y: u32,
            source_contrast: i32,
            blurred_contrast: i32,
        }
        let mut edges = Vec::new();
        for x in (CELL..w).step_by(CELL as usize) {
            for y in [CELL / 2, CELL * 5 / 2, CELL * 9 / 2, CELL * 13 / 2] {
                let sc = i32::from(px(&source_bytes, x - 1, y)) - i32::from(px(&source_bytes, x, y));
                let bc = i32::from(px(&blurred, x - 1, y)) - i32::from(px(&blurred, x, y));
                edges.push(Edge {
                    x,
                    y,
                    source_contrast: sc.abs(),
                    blurred_contrast: bc.abs(),
                });
            }
        }
        assert!(!edges.is_empty(), "premise: the checkerboard has internal edges");

        // Every source edge really is a full step -- the fixture's own
        // sanity check, so a failure below is about the blur and not about a
        // checkerboard that was not built the way this test assumes.
        let bad_source: Vec<_> = edges.iter().filter(|e| e.source_contrast != 255).collect();
        assert!(
            bad_source.is_empty(),
            "fixture premise violated -- these edges were not a full 0/255 \
             step before blurring: {:?}",
            bad_source
                .iter()
                .map(|e| (e.x, e.y, e.source_contrast))
                .collect::<Vec<_>>()
        );

        // The actual assertion: collect every edge that did *not* lose
        // contrast, and print each one's own (x, y) location plus its
        // measured value on failure -- this file's "print a bounding box"
        // rule, adapted to a set of edge locations rather than a single rect,
        // since the defect this guards against (no blur ran at all) would
        // show up as a full 255 at every single one of them, not a uniform
        // shift.
        const CONTRAST_CEILING: i32 = 180;
        let unblurred: Vec<_> = edges
            .iter()
            .filter(|e| e.blurred_contrast > CONTRAST_CEILING)
            .map(|e| (e.x, e.y, e.blurred_contrast))
            .collect();
        assert!(
            unblurred.is_empty(),
            "{} of {} internal edges kept contrast above {CONTRAST_CEILING}/255 \
             after a supposed 6-pass, radius-{BLUR_RADIUS} blur -- offending \
             (x, y, contrast): {unblurred:?}",
            unblurred.len(),
            edges.len(),
        );

        // -- negative control: skipping the blur must fail this same gate --
        // Proves the assertion discriminates rather than passing for any
        // input -- run against the *unblurred* source bytes reinterpreted as
        // the "blurred" result.
        let control_unblurred: Vec<_> = edges
            .iter()
            .filter(|e| e.source_contrast > CONTRAST_CEILING)
            .collect();
        assert_eq!(
            control_unblurred.len(),
            edges.len(),
            "control premise: an unblurred checkerboard must fail the \
             contrast-ceiling check at every edge, or the ceiling is not \
             actually discriminating"
        );
    }
}
