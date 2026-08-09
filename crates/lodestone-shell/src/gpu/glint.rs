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
    GlintPipeline, GlintUniform, Scale, clamp_speed, clamp_strength, glint_sampler,
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
    /// Group 0: the [`GlintUniform`] for the **hand** glint draw.
    pub(super) uniform_buffer: wgpu::Buffer,
    pub(super) uniform_bind_group: wgpu::BindGroup,
    /// Group 0 for the **world** glint draw (dropped enchanted items).
    ///
    /// A second buffer rather than a second write of the first, because the two
    /// draws need different `view_proj` matrices **within one submit**: the world
    /// items draw in the main pass and the hand in its own pass at the end of the
    /// frame, and `queue.write_buffer` is ordered against the *submit*, not against
    /// the encoder — so a single buffer written twice would hand both passes the
    /// last value and the shimmer would land nowhere.
    pub(super) world_uniform_buffer: wgpu::Buffer,
    pub(super) world_uniform_bind_group: wgpu::BindGroup,
}

impl std::fmt::Debug for GlintPass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The underlying pipeline (lodestone_render) and wgpu handles are not
        // `Debug`; the sheet's dimensions are the useful part anyway.
        f.debug_struct("GlintPass")
            .field("sheet", &self.texture.size())
            .finish_non_exhaustive()
    }
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
        let world_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-glint-uniform-world"),
            size: std::mem::size_of::<GlintUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let world_uniform_bind_group =
            pipeline.uniform_bind_group(device, &world_uniform_buffer);

        Self {
            pipeline,
            texture,
            texture_bind_group,
            uniform_buffer,
            uniform_bind_group,
            world_uniform_buffer,
            world_uniform_bind_group,
        }
    }
}

/// Wall-clock milliseconds for the glint scroll — vanilla's
/// `System.currentTimeMillis()` (see `TextureTransform.setupGlintTexturing`),
/// the same origin `crate::app::recipe_toast_now_ms` uses for the recipe toast.
#[must_use]
pub(super) fn glint_now_ms() -> f64 {
    // `crate::platform::epoch_duration`, not `SystemTime::now()`, which traps on
    // wasm32. This runs per glint draw, so a browser would die on the first
    // enchanted item on screen.
    crate::platform::epoch_duration().as_secs_f64() * 1000.0
}

/// The shared `GlintUniform` value for a glint draw under `view_proj`, at the
/// player's **Glint Speed** and **Glint Strength**. `section_origin` is zero on
/// every current site — item geometry carries its own position — mirroring the
/// model pass's reserved zero origin slot.
///
/// # Both were `DEFAULT_SPEED`/`DEFAULT_STRENGTH` until now
///
/// `glint_clock` and `GlintUniform::new` have taken them as parameters since the
/// glint landed, so nothing downstream needed changing — this call site handing
/// over the constants was the whole of why the two accessibility rows did
/// nothing. Note the two constants *are* vanilla's shipped option values (`0.5`
/// and `0.75`), so an untouched install is byte-identical to the old behaviour
/// and no gate written against the defaults can tell the difference.
///
/// Clamped to `[0, 1]` because both options are `UnitDouble`s and this reaches a
/// GPU uniform: a hand-edited negative strength would make `dst += src * src`
/// darken the item instead of shimmering it.
#[must_use]
pub(super) fn glint_uniform(
    view_proj: [[f32; 4]; 4],
    speed: f64,
    strength: f32,
) -> GlintUniform {
    GlintUniform::new(
        view_proj,
        [0.0, 0.0, 0.0],
        glint_now_ms(),
        clamp_speed(speed),
        clamp_strength(strength),
        Scale::Item,
    )
}

#[cfg(test)]
mod tests {
    use lodestone_render::glint::{DEFAULT_SPEED, DEFAULT_STRENGTH, glint_texture_matrix};

    const IDENTITY: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    /// The pushed **strength** reaches `GlintAlpha`, at a value where the frozen
    /// `DEFAULT_STRENGTH` hypothesis fails.
    ///
    /// `0.25` is not arbitrary: it is a third of the shipped `0.75`, so a uniform
    /// still carrying the constant is off by `0.5` — half the whole alpha range —
    /// and no rounding or blend subtlety can hide that. `0.0` is the second anchor
    /// because it is the value the accessibility option exists for, and it is the
    /// one a "did anything arrive?" check would confuse with an unwritten buffer.
    #[test]
    fn the_pushed_glint_strength_reaches_glint_alpha() {
        let quarter = super::glint_uniform(IDENTITY, DEFAULT_SPEED, 0.25);
        assert_eq!(quarter.origin_and_alpha[3], 0.25);
        assert!(
            (quarter.origin_and_alpha[3] - DEFAULT_STRENGTH).abs() > 0.4,
            "the uniform is still carrying DEFAULT_STRENGTH"
        );

        let off = super::glint_uniform(IDENTITY, DEFAULT_SPEED, 0.0);
        assert_eq!(
            off.origin_and_alpha[3], 0.0,
            "a strength of zero must arrive as zero, not fall back to the default"
        );

        // The control: the default really is `DEFAULT_STRENGTH`, so the hypothesis
        // the first assertion distances itself from is the right one.
        let default = super::glint_uniform(IDENTITY, DEFAULT_SPEED, DEFAULT_STRENGTH);
        assert_eq!(default.origin_and_alpha[3], DEFAULT_STRENGTH);
    }

    /// The pushed **speed** reaches the texture matrix.
    ///
    /// The expectation comes from `glint_texture_matrix` evaluated at a fixed
    /// clock outside this function — the same clock `glint_uniform` cannot be given
    /// (it reads the wall clock), which is why the comparison is at `millis == 0`:
    /// there, the scroll offsets are zero at *every* speed and the matrices agree
    /// trivially, so the assertion is instead that a **frozen** glint's matrix
    /// differs from a full-speed one at a non-zero clock. That is what separates
    /// "the argument is threaded" from "the argument is ignored" without needing a
    /// clock injection this call site does not have.
    #[test]
    fn the_speed_argument_changes_the_texture_matrix() {
        const MILLIS: f64 = 5_000.0;
        let frozen = glint_texture_matrix(MILLIS, 0.0, super::Scale::Item);
        let shipped = glint_texture_matrix(MILLIS, DEFAULT_SPEED, super::Scale::Item);
        let full = glint_texture_matrix(MILLIS, 1.0, super::Scale::Item);
        assert_ne!(
            frozen.to_cols_array(),
            shipped.to_cols_array(),
            "a zero-speed glint must not scroll like the shipped default"
        );
        assert_ne!(
            shipped.to_cols_array(),
            full.to_cols_array(),
            "full speed must not scroll like the shipped default"
        );
        // And `glint_uniform` really does put that matrix in the uniform, rather
        // than building its own: at a zero speed the translation column is exactly
        // the un-scrolled one whatever the wall clock reads, which is the one
        // property of the live call that is clock-independent.
        let uniform = super::glint_uniform(IDENTITY, 0.0, DEFAULT_STRENGTH);
        assert_eq!(
            uniform.tex_matrix,
            glint_texture_matrix(0.0, 0.0, super::Scale::Item).to_cols_array_2d(),
            "glint_uniform is not passing its speed argument to \
             glint_texture_matrix"
        );
    }
}
