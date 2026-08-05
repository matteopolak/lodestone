//! The shell's enchantment-glint pass: the [`GlintPipeline`] from
//! `lodestone_render::glint`, an uploaded non-sRGB glint sheet, and the one
//! uniform buffer every glint draw this frame rewrites.
//!
//! # Why the texture is uploaded as `Rgba8Unorm`, not `_Srgb`
//!
//! [`crate::gpu::entities::entity_texture_from_image`] uploads every diffuse
//! sheet as `Rgba8UnormSrgb` because a PNG's bytes are gamma-encoded and the
//! shader's tint multiply must land in linear light. The glint is the **opposite
//! case, deliberately**: vanilla's GLINT blend is `dst += src * src` in the
//! texture's **own** (gamma) space — the raw byte value is squared and added, no
//! colour management anywhere in the pipeline (see `lodestone_render::glint::
//! glint_blend`). Uploading the sheet as `_Srgb` would hand the shader the
//! *linear* decode of each byte, square that, and let an sRGB target re-encode —
//! a different number at every brightness and visibly off. So the choice is not
//! an oversight, it is the one the additive formula demands, and it is recorded
//! here because it is the mirror image of the choice every other texture loader
//! in this crate makes.
//!
//! # One uniform buffer, rewritten per glint draw
//!
//! The group-0 uniform carries `view_proj` **and** the scrolling texture matrix
//! together, and `view_proj` differs between glint sites (the world's camera for
//! dropped items, the hand camera for the held item, `gui_ortho` for icons). So
//! the pass owns one buffer and each draw site rewrites it with its own
//! `view_proj` immediately before issuing its glint draw — the render passes run
//! sequentially, so one buffer cannot be read by two draws at once.
//!
//! `millis` comes from [`glint_now_ms`], the same wall clock vanilla keys the
//! scroll off (`Util.getMillis()`), matching the convention
//! `crate::app::recipe_toast_now_ms` established for the same reason.

use lodestone_assets::Image;
use lodestone_render::glint::{
    DEFAULT_SPEED, DEFAULT_STRENGTH, GlintPipeline, GlintUniform, Scale, glint_sampler,
};

/// The uploaded glint sheet, its pass, and the shared group-0 uniform buffer.
pub(super) struct GlintPass {
    /// The pass: two bind groups, `ModelVertex`'s own layout, depth-`EQUAL`.
    pub(super) pipeline: GlintPipeline,
    /// The uploaded sheet, kept alive explicitly rather than relying on the
    /// bind group's own strong reference — the texture is the *subject* of this
    /// struct, not a side effect.
    #[allow(dead_code)]
    texture: wgpu::Texture,
    /// Group 1: the sheet plus [`glint_sampler`], built once and reused by
    /// every glint draw.
    pub(super) texture_bind_group: wgpu::BindGroup,
    /// Group 0: the [`GlintUniform`] for whichever glint draw runs next.
    pub(super) uniform_buffer: wgpu::Buffer,
    pub(super) uniform_bind_group: wgpu::BindGroup,
}

impl GlintPass {
    /// Upload `img` (the decoded `enchanted_glint_item.png`) and build the pass.
    ///
    /// `color_format` is the same target the item passes draw to and
    /// `lodestone_render::DEPTH_FORMAT` the same depth every depth-tested pass
    /// uses — both are fixed by the pipeline, not chosen.
    #[must_use]
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        img: &Image,
    ) -> Self {
        let pipeline = GlintPipeline::new(device, color_format, lodestone_render::DEPTH_FORMAT);

        // `Rgba8Unorm` — the non-sRGB upload this module's doc exists for. The
        // glint blend squares the sampled byte in gamma space, so the hardware
        // must NOT decode it to linear on the way in.
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lodestone-glint-sheet"),
            size: wgpu::Extent3d {
                width: img.width,
                height: img.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
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
            &img.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(img.width * 4),
                rows_per_image: Some(img.height),
            },
            wgpu::Extent3d {
                width: img.width,
                height: img.height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = glint_sampler(device);
        let texture_bind_group = pipeline.texture_bind_group(device, &view, &sampler);

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-glint-uniform"),
            size: std::mem::size_of::<GlintUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind_group = pipeline.uniform_bind_group(device, &uniform_buffer);

        Self {
            pipeline,
            texture,
            texture_bind_group,
            uniform_buffer,
            uniform_bind_group,
        }
    }
}

/// Wall-clock milliseconds for the glint scroll — vanilla's
/// `System.currentTimeMillis()` (see `TextureTransform.setupGlintTexturing`),
/// the same origin `crate::app::recipe_toast_now_ms` uses for the recipe toast.
#[must_use]
pub(super) fn glint_now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

/// The shared `GlintUniform` value for a glint draw under `view_proj`, at the
/// shipped option defaults. `section_origin` is zero on every current site —
/// item geometry carries its own position — mirroring the model pass's reserved
/// zero origin slot.
#[must_use]
pub(super) fn glint_uniform(view_proj: [[f32; 4]; 4]) -> GlintUniform {
    GlintUniform::new(
        view_proj,
        [0.0, 0.0, 0.0],
        glint_now_ms(),
        DEFAULT_SPEED,
        DEFAULT_STRENGTH,
        Scale::Item,
    )
}
