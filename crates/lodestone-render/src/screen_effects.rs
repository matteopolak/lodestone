//! The two full-screen overlays vanilla draws in `ScreenEffectRenderer.submit`
//! (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/ScreenEffectRenderer.java`):
//! the underwater tint + scrolling texture, and the looping fire overlay.
//! Issues #108 and #112.
//!
//! # One pass, two textures
//!
//! Both are a textured, alpha-blended, depth-less quad drawn late in the
//! frame (after the world and the first-person hand, before the HUD) — see
//! `GameRenderer.java:568-577`, which calls `screenEffectRenderer.submit`
//! immediately after `renderItemInHand` and before `featureRenderDispatcher`.
//! Vanilla's two pipelines (`BLOCK_SCREEN_EFFECT`, `FIRE_SCREEN_EFFECT`) are
//! textually identical builds of the same `GUI_TEXTURED_SNIPPET` base
//! (`RenderPipelines.java:713-718`) — position+uv+colour, `TRANSLUCENT` blend,
//! no depth attachment — so one pipeline here draws both, parameterised only
//! by which texture bind group is active.
//!
//! # Bind groups: one, not four
//!
//! Same constraint as the sky pass (`sky_pipeline.rs`'s module doc): the model
//! shader is already at wgpu's 4-bind-group floor. This pipeline uses exactly
//! **one** bind group (a texture + sampler; there is no camera matrix at all,
//! see below), so it can never be the thing that pushes an adapter over the
//! floor.
//!
//! # Screen-space, not world-space
//!
//! Vanilla submits both quads through a small local `PoseStack` under a
//! perspective `hud3dProjection`, at a fixed depth (`z = -0.5`) with a size
//! chosen so it fills the frame regardless of FOV. Reproducing that exact
//! perspective would buy nothing here — the quads have no other 3-D content to
//! interact with — so this pass places them directly in NDC (`x, y` in
//! `-1.0..1.0`, no camera uniform, no projection). The underwater quad still
//! fills the screen either way. **Deliberate simplification, not a decode
//! error**: the fire overlay's *placement* (a tiled strip across the bottom of
//! the frame here, vs. vanilla's two rotated 3-D quads either side of the
//! reticle) is chosen to match the visible result issue #112 asks for — a
//! flame texture across the bottom of the screen — rather than vanilla's exact
//! pose-stack transform. The **texture, its 32-frame animation, the tint
//! maths and the alpha blend** are all real.
//!
//! # Underwater: a tint, not a second fog
//!
//! `submitWater` multiplies the `underwater.png` texel by a **grayscale**
//! colour (`ARGB.colorFromFloat(0.1F, brightness, brightness, brightness)`,
//! `ScreenEffectRenderer.java:159`) at alpha `0.1` — not blue; whatever blue
//! cast the overlay has comes entirely from the texture's own pixels. This is
//! wholly independent of the dimension fog this codebase already models
//! (`crate::fog`): fog fades *world geometry* into a colour as it recedes,
//! while this is a flat, non-fading screen-space layer with its own texture,
//! composited after the world and the hand are already drawn. Vanilla runs
//! both at once when submerged; nothing here changes `fog.rs`.
//!
//! # No double quotes in these shaders, ever
//!
//! Same rule as every other shader in this crate: `"` inside a WGSL comment
//! ends the Rust raw string early. Backticks only.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use lodestone_assets::{
    ResourceManager, ScreenEffectAssetError, fire_frame_count, load_fire_texture,
    load_underwater_texture,
};

// ---------------------------------------------------------------------------
// Pure geometry (no GPU handles) — testable with no device.
// ---------------------------------------------------------------------------

/// One overlay vertex: NDC position, texture UV, and a baked RGBA tint
/// (multiplied onto the sampled texel — see the module doc's gamma note in
/// [`OVERLAY_WGSL`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct ScreenOverlayVertex {
    /// NDC position (`x, y` in `-1.0..1.0`).
    pub position: [f32; 2],
    /// Texture UV.
    pub uv: [f32; 2],
    /// Baked RGBA tint, straight (non-premultiplied) alpha.
    pub color: [f32; 4],
}

fn vertex(position: [f32; 2], uv: [f32; 2], color: [f32; 4]) -> ScreenOverlayVertex {
    ScreenOverlayVertex { position, uv, color }
}

/// The underwater overlay's per-fragment tint alpha — vanilla's constant
/// `0.1F` in `ScreenEffectRenderer.submitWater`.
pub const UNDERWATER_TINT_ALPHA: f32 = 0.1;

/// How many times the underwater texture tiles across the quad — vanilla's
/// constant `4.0F` (`uvSize` in `submitWater`).
pub const UNDERWATER_TILE_COUNT: f32 = 4.0;

/// Vanilla's `Lightmap.getBrightness` is a per-dimension gamma-corrected
/// curve table this codebase has not ported. This is the same approximation
/// the block shader already applies to packed light
/// (`model_pipeline.rs`'s `light_term = 0.2 + 0.8 * max(sky * sky_darken, block)`,
/// minus the sky-darken factor this pass has no clock for): a floor so a
/// fully dark cell does not tint pure black, rising linearly with the
/// brighter of the two channels. Reused rather than invented, so the two
/// gamma-adjacent quantities in this codebase agree on one curve shape.
///
/// `packed` is `sky << 4 | block`, the same encoding
/// [`crate::entity`]'s light sampling already uses.
#[must_use]
pub fn underwater_brightness(packed_light: u8) -> f32 {
    let sky = f32::from((packed_light >> 4) & 0x0F) / 15.0;
    let block = f32::from(packed_light & 0x0F) / 15.0;
    0.2 + 0.8 * sky.max(block)
}

/// Builds the underwater overlay's one NDC quad. `yaw_degrees`/`pitch_degrees`
/// are the camera's look direction (matching vanilla's `getYRot()`/`getXRot()`
/// convention: yaw about `+Y`, `0` facing `+Z`); the UV scroll formula and
/// vertex/UV pairing are transcribed unchanged from
/// `ScreenEffectRenderer.submitWater`/`buildQuad`.
#[must_use]
pub fn underwater_overlay_quad(
    yaw_degrees: f32,
    pitch_degrees: f32,
    packed_light: u8,
) -> [ScreenOverlayVertex; 4] {
    let brightness = underwater_brightness(packed_light);
    let color = [brightness, brightness, brightness, UNDERWATER_TINT_ALPHA];
    let u0 = -yaw_degrees / 64.0;
    let v0 = pitch_degrees / 64.0;
    let (u1, v1) = (u0 + UNDERWATER_TILE_COUNT, v0 + UNDERWATER_TILE_COUNT);
    // `buildQuad(x0,y0,x1,y1, u0=u1,v0=v1, u1=u0,v1=v0)` — vanilla passes the
    // *far* UV corner as its own `u0`/`v0` parameter; transcribed literally
    // rather than renamed, so this stays checkable against the source line.
    [
        vertex([-1.0, -1.0], [u1, v1], color),
        vertex([1.0, -1.0], [u0, v1], color),
        vertex([1.0, 1.0], [u0, v0], color),
        vertex([-1.0, 1.0], [u1, v0], color),
    ]
}

/// The underwater quad as two CCW triangles (`0,1,2` / `2,3,0`, matching
/// [`crate::sky::quad_indices`]), for a caller building a plain (non-indexed)
/// vertex buffer.
#[must_use]
pub fn underwater_overlay_triangles(
    yaw_degrees: f32,
    pitch_degrees: f32,
    packed_light: u8,
) -> [ScreenOverlayVertex; 6] {
    let q = underwater_overlay_quad(yaw_degrees, pitch_degrees, packed_light);
    [q[0], q[1], q[2], q[2], q[3], q[0]]
}

/// The fire overlay's translucency — vanilla's vertex colour constant
/// `-436207617` (`ARGB` `(229, 255, 255, 255)`, i.e. white at alpha
/// `229/255`) in `ScreenEffectRenderer.submitFire`/`buildFireQuad`.
pub const FIRE_TINT: [f32; 4] = [1.0, 1.0, 1.0, 229.0 / 255.0];

/// How many tiled quads span the bottom strip — see the module doc on why
/// this pass places the fire overlay as a horizontal strip rather than
/// vanilla's two rotated 3-D quads.
pub const FIRE_TILE_COUNT: u32 = 4;

/// NDC height of the fire strip (`-1.0` is the bottom edge of the screen).
pub const FIRE_STRIP_TOP: f32 = -0.3;

/// Builds the fire overlay's tiled bottom strip for animation frame
/// `frame_index` (wrapped by `frame_count`, which callers get from
/// [`lodestone_assets::fire_frame_count`]). Alternate tiles are horizontally
/// mirrored (swap `u` endpoints) purely so a repeating strip does not read as
/// one texture stamped copy-paste; vanilla's two-quad layout has no such
/// artifact to avoid because it only ever draws two quads.
#[must_use]
pub fn fire_overlay_triangles(
    frame_index: u32,
    frame_count: u32,
) -> [ScreenOverlayVertex; (FIRE_TILE_COUNT * 6) as usize] {
    let frame_count = frame_count.max(1);
    let frame = frame_index % frame_count;
    let v0 = frame as f32 / frame_count as f32;
    let v1 = (frame + 1) as f32 / frame_count as f32;

    let mut out = [vertex([0.0, 0.0], [0.0, 0.0], FIRE_TINT); (FIRE_TILE_COUNT * 6) as usize];
    let width = 2.0 / FIRE_TILE_COUNT as f32;
    for i in 0..FIRE_TILE_COUNT {
        let x0 = -1.0 + width * i as f32;
        let x1 = x0 + width;
        let mirror = i % 2 == 1;
        let (ul, ur) = if mirror { (1.0, 0.0) } else { (0.0, 1.0) };
        let quad = [
            vertex([x0, -1.0], [ul, v1], FIRE_TINT),
            vertex([x1, -1.0], [ur, v1], FIRE_TINT),
            vertex([x1, FIRE_STRIP_TOP], [ur, v0], FIRE_TINT),
            vertex([x0, FIRE_STRIP_TOP], [ul, v0], FIRE_TINT),
        ];
        let tris = [quad[0], quad[1], quad[2], quad[2], quad[3], quad[0]];
        out[(i * 6) as usize..(i * 6 + 6) as usize].copy_from_slice(&tris);
    }
    out
}

// ---------------------------------------------------------------------------
// WGSL — one pipeline, shared by both textures.
// ---------------------------------------------------------------------------

const OVERLAY_WGSL: &str = r"
@group(0) @binding(0) var overlay_tex: texture_2d<f32>;
@group(0) @binding(1) var overlay_smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(position, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

// Same gamma round-trip every tint in this codebase uses (see
// `model_pipeline.rs`): vanilla multiplies a gamma-space colour by a
// gamma-space tint on gamma-space bytes, never in linear light. Only the
// tint's RGB goes through the round-trip — alpha is coverage, not colour, and
// is not gamma-encoded.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let tex = textureSample(overlay_tex, overlay_smp, in.uv);
    let tinted = srgb_to_linear(linear_to_srgb(tex.rgb) * in.color.rgb);
    return vec4<f32>(tinted, tex.a * in.color.a);
}
";

fn texture_bind_group_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
    })
}

fn texture_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    label: &str,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

fn upload_plain_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    width: u32,
    height: u32,
    rgba: &[u8],
    address_mode: wgpu::AddressMode,
    filter: wgpu::FilterMode,
) -> (wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * width.max(1)),
            rows_per_image: Some(height.max(1)),
        },
        wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: address_mode,
        address_mode_v: address_mode,
        address_mode_w: address_mode,
        mag_filter: filter,
        min_filter: filter,
        ..Default::default()
    });
    (view, sampler)
}

fn build_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    color_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(OVERLAY_WGSL.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    const ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Float32x4,
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<ScreenOverlayVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &ATTRS,
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
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            ..Default::default()
        },
        // No depth attachment — see the module doc; this draws after the
        // world and the hand, straight into the colour target.
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn vertex_buffer(device: &wgpu::Device, label: &str, verts: &[ScreenOverlayVertex]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(verts),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
    })
}

/// Owns the GPU resources for both overlays and drives them per frame.
#[derive(Debug)]
pub struct ScreenEffectRenderer {
    pipeline: wgpu::RenderPipeline,
    underwater_bind_group: wgpu::BindGroup,
    fire_bind_group: wgpu::BindGroup,
    fire_frame_count: u32,
    underwater_vbuf: wgpu::Buffer,
    fire_vbuf: wgpu::Buffer,
}

impl ScreenEffectRenderer {
    /// Loads both overlay textures from `manager` and builds the pass.
    ///
    /// # Errors
    ///
    /// Returns [`ScreenEffectAssetError`] if either texture is missing or
    /// fails to decode.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        manager: &ResourceManager,
    ) -> Result<Self, ScreenEffectAssetError> {
        let underwater_image = load_underwater_texture(manager)?;
        let fire_image = load_fire_texture(manager)?;
        let fire_frame_count = fire_frame_count(&fire_image);

        let layout = texture_bind_group_layout(device, "lodestone-screen-effect-tex-bgl");
        let pipeline = build_pipeline(device, "lodestone-screen-effect-pipeline", &layout, color_format);

        let (uw_view, uw_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-underwater-texture",
            underwater_image.width,
            underwater_image.height,
            &underwater_image.rgba,
            wgpu::AddressMode::Repeat,
            wgpu::FilterMode::Linear,
        );
        let underwater_bind_group = texture_bind_group(
            device,
            &layout,
            "lodestone-underwater-texture-bg",
            &uw_view,
            &uw_sampler,
        );

        // Nearest, and clamp rather than repeat: this is a vertical strip of
        // independent animation frames, not a tileable texture — linear
        // filtering or wraparound at a frame's top/bottom edge would blend in
        // the neighbouring frame.
        let (fire_view, fire_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-fire-texture",
            fire_image.width,
            fire_image.height,
            &fire_image.rgba,
            wgpu::AddressMode::ClampToEdge,
            wgpu::FilterMode::Nearest,
        );
        let fire_bind_group = texture_bind_group(
            device,
            &layout,
            "lodestone-fire-texture-bg",
            &fire_view,
            &fire_sampler,
        );

        let underwater_vbuf = vertex_buffer(
            device,
            "lodestone-underwater-vbuf",
            &underwater_overlay_triangles(0.0, 0.0, 0xFF),
        );
        let fire_vbuf = vertex_buffer(device, "lodestone-fire-vbuf", &fire_overlay_triangles(0, fire_frame_count));

        Ok(Self {
            pipeline,
            underwater_bind_group,
            fire_bind_group,
            fire_frame_count,
            underwater_vbuf,
            fire_vbuf,
        })
    }

    /// The fire strip's frame count, from the loaded texture — a caller
    /// ticking the animation forward derives its own frame index modulo this.
    #[must_use]
    pub fn fire_frame_count(&self) -> u32 {
        self.fire_frame_count
    }

    /// Draws the underwater overlay (screen tint + scrolling texture) as its
    /// own render pass, with `Load` (never `Clear`) — this runs after the
    /// world, entities and the first-person hand, and must not erase them.
    /// `yaw_degrees`/`pitch_degrees` are the live camera look direction;
    /// `packed_light` is `sky << 4 | block` at the player's eye, the same
    /// encoding [`crate::entity`]'s light sampling uses.
    pub fn draw_underwater(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        yaw_degrees: f32,
        pitch_degrees: f32,
        packed_light: u8,
    ) {
        let verts = underwater_overlay_triangles(yaw_degrees, pitch_degrees, packed_light);
        queue.write_buffer(&self.underwater_vbuf, 0, bytemuck::cast_slice(&verts));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lodestone-underwater-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.underwater_bind_group, &[]);
        pass.set_vertex_buffer(0, self.underwater_vbuf.slice(..));
        pass.draw(0..verts.len() as u32, 0..1);
    }

    /// Draws the fire overlay (looping flame strip) as its own `Load` render
    /// pass, for the reasons on [`Self::draw_underwater`]. `tick` selects the
    /// animation frame (`tick % `[`Self::fire_frame_count`]`, vanilla's
    /// default one-frame-per-tick `fire_1.png.mcmeta`).
    pub fn draw_fire(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        tick: u64,
    ) {
        let frame = (tick % u64::from(self.fire_frame_count)) as u32;
        let verts = fire_overlay_triangles(frame, self.fire_frame_count);
        queue.write_buffer(&self.fire_vbuf, 0, bytemuck::cast_slice(&verts));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lodestone-fire-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.fire_bind_group, &[]);
        pass.set_vertex_buffer(0, self.fire_vbuf.slice(..));
        pass.draw(0..verts.len() as u32, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underwater_brightness_floors_at_0_2_and_caps_at_1() {
        assert!((underwater_brightness(0x00) - 0.2).abs() < 1e-6, "fully dark floors at 0.2");
        assert!((underwater_brightness(0xFF) - 1.0).abs() < 1e-6, "fully lit reaches 1.0");
    }

    #[test]
    fn underwater_brightness_takes_the_brighter_channel() {
        // block=15, sky=0 should read identically to sky=15, block=0.
        let block_lit = underwater_brightness(0x0F);
        let sky_lit = underwater_brightness(0xF0);
        assert!((block_lit - sky_lit).abs() < 1e-6);
    }

    #[test]
    fn underwater_quad_alpha_is_vanillas_point_one() {
        let q = underwater_overlay_quad(0.0, 0.0, 0xFF);
        for v in q {
            assert!((v.color[3] - 0.1).abs() < 1e-6);
        }
    }

    #[test]
    fn underwater_quad_covers_the_full_ndc_screen() {
        let q = underwater_overlay_quad(0.0, 0.0, 0xFF);
        let xs: Vec<f32> = q.iter().map(|v| v.position[0]).collect();
        let ys: Vec<f32> = q.iter().map(|v| v.position[1]).collect();
        assert_eq!(xs.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
        assert_eq!(ys.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
    }

    #[test]
    fn underwater_uv_scrolls_with_yaw_and_pitch() {
        let still = underwater_overlay_quad(0.0, 0.0, 0xFF);
        let turned = underwater_overlay_quad(90.0, 0.0, 0xFF);
        assert_ne!(still[0].uv, turned[0].uv, "yaw must move the scroll");
        let tilted = underwater_overlay_quad(0.0, 45.0, 0xFF);
        assert_ne!(still[0].uv, tilted[0].uv, "pitch must move the scroll");
    }

    #[test]
    fn underwater_uv_tiles_four_times() {
        let q = underwater_overlay_quad(0.0, 0.0, 0xFF);
        // The quad's UV span (max - min on either axis) is the tile count.
        let us: Vec<f32> = q.iter().map(|v| v.uv[0]).collect();
        let span = us.iter().cloned().fold(f32::NEG_INFINITY, f32::max)
            - us.iter().cloned().fold(f32::INFINITY, f32::min);
        assert!((span - UNDERWATER_TILE_COUNT).abs() < 1e-6);
    }

    #[test]
    fn fire_tint_alpha_matches_vanillas_argb_constant() {
        assert!((FIRE_TINT[3] - 229.0 / 255.0).abs() < 1e-6);
        assert_eq!(&FIRE_TINT[0..3], &[1.0, 1.0, 1.0]);
    }

    #[test]
    fn fire_strip_spans_the_full_ndc_width_with_no_gaps() {
        let tris = fire_overlay_triangles(0, 32);
        let xs: Vec<f32> = tris.iter().map(|v| v.position[0]).collect();
        assert_eq!(xs.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
    }

    #[test]
    fn fire_strip_sits_along_the_bottom_edge() {
        let tris = fire_overlay_triangles(0, 32);
        let ys: Vec<f32> = tris.iter().map(|v| v.position[1]).collect();
        assert_eq!(ys.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(
            ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
            FIRE_STRIP_TOP
        );
    }

    #[test]
    fn fire_frame_selects_the_right_v_slice() {
        let frame5 = fire_overlay_triangles(5, 32);
        let v0 = 5.0 / 32.0;
        let v1 = 6.0 / 32.0;
        for vert in frame5 {
            assert!(vert.uv[1] >= v0 - 1e-6 && vert.uv[1] <= v1 + 1e-6);
        }
    }

    #[test]
    fn fire_frame_wraps_past_the_last_frame() {
        let wrapped = fire_overlay_triangles(32, 32);
        let first = fire_overlay_triangles(0, 32);
        assert_eq!(wrapped, first, "frame 32 of a 32-frame strip is frame 0 again");
    }

    #[test]
    fn fire_alternate_tiles_are_mirrored() {
        let tris = fire_overlay_triangles(0, 32);
        // Tile 0's first two verts (bottom-left, bottom-right) vs tile 1's.
        let tile0_u = [tris[0].uv[0], tris[1].uv[0]];
        let tile1_u = [tris[6].uv[0], tris[7].uv[0]];
        assert_eq!(tile0_u, [tile1_u[1], tile1_u[0]], "tile 1 must be a horizontal mirror of tile 0");
    }
}
