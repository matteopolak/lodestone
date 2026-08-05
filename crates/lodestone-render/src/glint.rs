//! The enchantment glint: vanilla's scrolling foil shimmer.
//!
//! # What it is
//!
//! An enchanted item draws a second pass over its **own geometry**, sampling a
//! scrolling, rotated, additively-blended `enchanted_glint_*.png`. This module
//! owns the maths (the texture matrix), the gate (does this stack glint at all),
//! and the pipeline; the shader is
//! [`shaders/glint.wgsl`](../src/shaders/glint.wgsl).
//!
//! Ported from 26.2's `TextureTransform.setupGlintTexturing`
//! (`TextureTransform.java:31-38`), `RenderPipelines.GLINT`
//! (`RenderPipelines.java:419-433`) and `ItemStack.hasFoil`
//! (`ItemStack.java:968-971`).
//!
//! # It is not a texture swap, and it is not one draw
//!
//! Two beliefs worth killing before touching this.
//!
//! **"The glint is drawn twice with two transforms."** It was, historically. In
//! 26.2 it is a *single* draw whose one translation uses **two different periods
//! on the two UV axes** — `U` scrolls negative on a 110000 clock and `V` positive
//! on a 30000 clock (`TextureTransform.java:33-35`). Both components share one
//! rotation and one scale. Drawing it twice doubles the brightness.
//!
//! **"Armour's glint is rotated differently."** It was, historically (`-50°`). In
//! 26.2 every glint pass rotates by exactly `+10°`; armour differs only in
//! [`Scale`] and in a small model-view nudge toward the camera.
//!
//! # Depth: the one comparison that does not flip
//!
//! Vanilla's glint pipeline is `DepthStencilState(CompareOp.EQUAL, false)` with
//! **zero** depth bias, which works only because the glint pass rasterises
//! byte-identical clip positions to the pass beneath it. Our depth is `[0,1]`
//! DirectX-style rather than vanilla's reversed-Z, so every other ported depth
//! comparison flips sign — but equality is orientation-independent and
//! `CompareOp.EQUAL` ports across unchanged. See [`DEPTH_COMPARE`].
//!
//! # Bind groups: 2 of 4, so there is room
//!
//! The model pipeline is at wgpu's portable `max_bind_groups` floor of 4
//! (camera / atlas / palette / anim) and cannot take a fifth group — a 5-group
//! shader validates on an adapter reporting 8 and crashes at startup on the
//! floor. **The glint does not need one.** It is a *separate pipeline*, and the
//! floor is per-pipeline: this one spends **2** groups (uniform / glint texture),
//! and even those fold `GlintAlpha` and the section origin into the group-0
//! uniform rather than adding a binding, following the same reasoning that folded
//! fog into the camera uniform. Vanilla's own `GLINT` pipeline likewise declares
//! four bind-group *layouts* against a GL backend with no such floor; the port is
//! narrower, not wider.

use crate::models::ModelVertex;

/// Vanilla's glint textures. Both are 128x128 and both are sampled `REPEAT` /
/// `LINEAR` with no mipmaps — their `.mcmeta` is `{"texture":{"blur":true}}` with
/// no `clamp`, which `ReloadableTexture.java:24-29` turns into exactly that.
pub mod textures {
    /// `ItemFeatureRenderer.ENCHANTED_GLINT_ITEM` (`ItemFeatureRenderer.java:23`).
    /// Used by the `glint`, `glint_translucent` and `entity_glint` render types,
    /// i.e. every item form: GUI icon, dropped, held, and the trident/shield
    /// special models. 8-bit RGB with **no alpha channel**.
    pub const ITEM: &str = "minecraft:misc/enchanted_glint_item";

    /// `ItemFeatureRenderer.ENCHANTED_GLINT_ARMOR` (`ItemFeatureRenderer.java:22`).
    /// Used only by `armor_entity_glint`, for worn equipment. Palettised 8-bit.
    ///
    /// There is **no** `enchanted_glint_entity.png` in 26.2; the entity path
    /// reuses [`ITEM`]. A port that looks for one will not find it.
    pub const ARMOUR: &str = "minecraft:misc/enchanted_glint_armor";
}

/// `TextureTransform.MAX_ENCHANTMENT_GLINT_SPEED_MILLIS`
/// (`TextureTransform.java:9`). Dimensionless, applied to a millisecond count.
///
/// Note the constant is declared in the jar but **not referenced** by
/// `setupGlintTexturing`, which hard-codes the literal `8.0` at
/// `TextureTransform.java:32`. Same value; recorded here so a future divergence
/// in the jar is visible rather than surprising.
pub const SPEED_MILLIS: f64 = 8.0;

/// The default of vanilla's `glintSpeed` option (`Options.java:858-865`, a
/// `UnitDouble` in `0.0..=1.0`).
///
/// The effective multiplier at defaults is therefore `8.0 * 0.5 = 4.0`, which
/// makes the real-time wrap periods `110000 / 4 = 27500 ms` on `U` and
/// `30000 / 4 = 7500 ms` on `V`. A port that forgets this factor scrolls twice as
/// fast as the game and nothing about the frame looks wrong in a screenshot.
pub const DEFAULT_SPEED: f64 = 0.5;

/// The default of vanilla's `glintStrength` option (`Options.java:867-874`),
/// reaching the shader as the `GlintAlpha` global (`GameRenderer.java:412`).
///
/// At `0.75` this is a visible 25% reduction in the glint's RGB, so treating it
/// as `1.0` is a *magnitude* error of exactly the kind that shipped a hurt
/// overlay here at 70% red where vanilla renders 30%.
pub const DEFAULT_STRENGTH: f32 = 0.75;

/// The `U`-axis wrap period, in scaled milliseconds (`TextureTransform.java:33`).
pub const U_PERIOD: i64 = 110_000;

/// The `V`-axis wrap period, in scaled milliseconds (`TextureTransform.java:34`).
pub const V_PERIOD: i64 = 30_000;

/// The rotation applied to the glint UV, in degrees: `(float)(Math.PI / 18)` =
/// exactly `10°` (`TextureTransform.java:36`). One angle for every pass.
pub const ROTATION_DEGREES: f32 = 10.0;

/// The depth comparison for the glint pass: `CompareOp.EQUAL`
/// (`RenderPipelines.java:431`), unflipped — see this module's doc for why this
/// is the one ported comparison that keeps its sense.
pub const DEPTH_COMPARE: wgpu::CompareFunction = wgpu::CompareFunction::Equal;

/// Which glint scale to use, i.e. which of vanilla's four glint render types this
/// draw corresponds to. The *only* thing that differs between them (plus armour's
/// depth nudge); the texture, rotation and scroll periods are shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    /// `GLINT_TEXTURING`, scale `8.0` (`TextureTransform.java:13`). The `glint`
    /// and `glint_translucent` render types: **every item form** — GUI slot icon,
    /// dropped item, held item.
    Item,
    /// `ENTITY_GLINT_TEXTURING`, scale `0.5` (`TextureTransform.java:14`). The
    /// `entity_glint` render type: the trident and shield special models only.
    Entity,
    /// `ARMOR_ENTITY_GLINT_TEXTURING`, scale `0.16`
    /// (`TextureTransform.java:15`). The `armor_entity_glint` render type: worn
    /// equipment. Uses [`textures::ARMOUR`] rather than [`textures::ITEM`], and
    /// additionally carries [`ARMOUR_VIEW_OFFSET_SCALE`].
    Armour,
}

impl Scale {
    /// The scalar passed to `setupGlintTexturing`.
    #[must_use]
    pub const fn factor(self) -> f32 {
        match self {
            Self::Item => 8.0,
            Self::Entity => 0.5,
            Self::Armour => 0.16,
        }
    }

    /// The glint texture this scale's render type binds.
    #[must_use]
    pub const fn texture(self) -> &'static str {
        match self {
            Self::Item | Self::Entity => textures::ITEM,
            Self::Armour => textures::ARMOUR,
        }
    }
}

/// `armor_entity_glint`'s `LayeringTransform.VIEW_OFFSET_Z_LAYERING` under a
/// perspective projection: a uniform model-view scale by `1 - 1/4096`
/// (`LayeringTransform.java:11-13`, `ProjectionType.java:6`).
///
/// Armour is the only glint type with a layering transform, and it coexists with
/// depth-`EQUAL`, so worn equipment's glint is deliberately nudged toward the
/// camera. Under an orthographic projection vanilla instead translates `+1/512`
/// in Z ([`ARMOUR_VIEW_OFFSET_Z_ORTHO`]).
pub const ARMOUR_VIEW_OFFSET_SCALE: f32 = 1.0 - 1.0 / 4096.0;

/// The orthographic form of [`ARMOUR_VIEW_OFFSET_SCALE`]: `+1/512` in Z
/// (`ProjectionType.java:7`).
pub const ARMOUR_VIEW_OFFSET_Z_ORTHO: f32 = 1.0 / 512.0;

/// The scaled-millisecond clock the two UV offsets are taken modulo:
/// `(long)(Util.getMillis() * glintSpeed * 8.0)` (`TextureTransform.java:32`).
///
/// The `(long)` cast is **truncation, and it is observable**: the offsets step in
/// discrete units of `1/110000` and `1/30000` rather than moving continuously, so
/// computing this in `f64` and skipping the floor gives a subtly different
/// animation. `speed` is vanilla's `glintSpeed` option; pass
/// [`DEFAULT_SPEED`] for the shipped default.
#[must_use]
pub fn glint_clock(millis: f64, speed: f64) -> i64 {
    (millis * speed * SPEED_MILLIS) as i64
}

/// The two UV offsets as `(u_off, v_off)`, each in `[0, 1)`.
///
/// `u_off = (m % 110000) / 110000` and `v_off = (m % 30000) / 30000`
/// (`TextureTransform.java:33-34`). The sign is **not** applied here — `U` is
/// negated by [`glint_texture_matrix`], matching
/// `translation(-layerOffset0, layerOffset1, 0)`.
#[must_use]
pub fn glint_offsets(clock: i64) -> (f32, f32) {
    // `%` on a negative clock would give a negative offset. A monotonic
    // millisecond source never goes negative, but `rem_euclid` costs nothing and
    // makes the range claim above true unconditionally rather than by assumption.
    let u = clock.rem_euclid(U_PERIOD) as f32 / U_PERIOD as f32;
    let v = clock.rem_euclid(V_PERIOD) as f32 / V_PERIOD as f32;
    (u, v)
}

/// The glint texture matrix: `T(-u_off, +v_off, 0) * Rz(10°) * S(scale)`.
///
/// # Composition order, which JOML makes easy to get backwards
///
/// Vanilla builds it as
/// `new Matrix4f().translation(...).rotateZ(...).scale(...)`
/// (`TextureTransform.java:35-37`). JOML's `translation` **sets** the matrix while
/// `rotateZ` and `scale` **post-multiply**, so the result is
/// `T · Rz · S` — read right-to-left when applied to a vector: scale the incoming
/// UV, then rotate `+10°` about Z, then translate. Reading the fluent chain
/// left-to-right as the application order gives `S · Rz · T`, which scales the
/// translation by 8 and sends the glint off the texture entirely.
///
/// The shader applies it as `(M * vec4(uv, 0, 1)).xy`, exactly vanilla's
/// `glint.vsh`.
#[must_use]
pub fn glint_texture_matrix(millis: f64, speed: f64, scale: Scale) -> glam::Mat4 {
    let (u, v) = glint_offsets(glint_clock(millis, speed));
    glam::Mat4::from_translation(glam::Vec3::new(-u, v, 0.0))
        * glam::Mat4::from_rotation_z(ROTATION_DEGREES.to_radians())
        * glam::Mat4::from_scale(glam::Vec3::splat(scale.factor()))
}

/// `ItemStack.hasFoil()` (`ItemStack.java:968-971`), as far as this client's
/// modelled components allow — and the shortfall is the point.
///
/// Vanilla is `enchantment_glint_override ?? item.isFoil(stack)`, where the
/// default `Item.isFoil` is `!ENCHANTMENTS.isEmpty()` (`Item.java:346-348`,
/// `ItemStack.java:999-1001`). We model `enchantments` and **not**
/// `enchantment_glint_override`, so:
///
/// * an ordinary enchanted item glints correctly — the common case, and the one
///   an inventory is full of;
/// * a stack that explicitly *suppresses* its glint with
///   `[minecraft:enchantment_glint_override=false]` glints anyway;
/// * the seven items whose glint comes only from a baked
///   `ENCHANTMENT_GLINT_OVERRIDE=true` do **not** glint —
///   `enchanted_golden_apple`, `experience_bottle`, `written_book`, `nether_star`,
///   `enchanted_book`, `end_crystal`, `debug_stick` (`Items.java:1122`, `:1471`,
///   `:1481`, `:1557`, `:1571`, `:1609`, `:1697`).
///
/// That last group is not fixable by decoding harder: the override is a
/// *prototype* component baked into `Item.Properties`, so a clientbound stack
/// carries no mention of it at all, exactly like `max_stack_size`. It needs an
/// item-prototype census behind the version seam, not new wire decode. Note also
/// that `enchanted_book`'s enchantments live in `STORED_ENCHANTMENTS`, which
/// vanilla's `isEnchanted` deliberately does not read, so it would not glint even
/// with the override modelled by way of the enchantments list.
///
/// `minecraft:compass` is the one item with a code-level override —
/// `LODESTONE_TRACKER` present means foil (`CompassItem.java:29-31`) — and is
/// likewise out of reach.
#[must_use]
pub fn has_foil(components: &lodestone_model::item::ItemComponents) -> bool {
    !components.enchantments.is_empty()
}

/// The group-0 uniform for [`shaders/glint.wgsl`](../src/shaders/glint.wgsl).
///
/// `view_proj` and `origin_and_alpha.xyz` mirror the model pipeline's camera and
/// section origin **exactly**, because depth-`EQUAL` requires the glint pass to
/// rasterise identical clip positions. `origin_and_alpha.w` carries `GlintAlpha`,
/// folded in rather than given its own binding.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GlintUniform {
    /// Column-major `view_proj`, as the model pipeline's camera uniform holds it.
    pub view_proj: [[f32; 4]; 4],
    /// The glint texture matrix from [`glint_texture_matrix`], column-major.
    pub tex_matrix: [[f32; 4]; 4],
    /// `.xyz` the section origin, `.w` `GlintAlpha` ([`DEFAULT_STRENGTH`]).
    pub origin_and_alpha: [f32; 4],
}

impl GlintUniform {
    /// Build the uniform for one glint draw.
    #[must_use]
    pub fn new(
        view_proj: [[f32; 4]; 4],
        section_origin: [f32; 3],
        millis: f64,
        speed: f64,
        strength: f32,
        scale: Scale,
    ) -> Self {
        Self {
            view_proj,
            tex_matrix: glint_texture_matrix(millis, speed, scale).to_cols_array_2d(),
            origin_and_alpha: [
                section_origin[0],
                section_origin[1],
                section_origin[2],
                strength,
            ],
        }
    }
}

/// `BlendFunction.GLINT` (`BlendFunction.java:8`):
/// `(SRC_COLOR, ONE, ZERO, ONE)`, both equations `ADD`.
///
/// So colour is `dst += src * src` — additive, but with the source **squared**
/// rather than scaled by its alpha — and alpha is `dst * 1 + src * 0`, i.e. the
/// destination alpha is left completely untouched.
///
/// It is neither `TRANSLUCENT` (`SRC_ALPHA, ONE_MINUS_SRC_ALPHA, …`,
/// `BlendFunction.java:10-12`) nor `ADDITIVE` (`ONE, ONE`, `:17`). Reaching for
/// either is the obvious guess and both are wrong.
///
/// One useful consequence: **no alpha enters the colour equation at all.** The
/// measured warning elsewhere in this repo — that the effective blend alpha
/// through `ALPHA_BLENDING` on this Metal backend is a non-trivial, unpredictable
/// function of the raw fragment alpha — therefore does not apply to this pass.
#[must_use]
pub const fn glint_blend() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Src,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
    }
}

/// The glint pass pipeline: two bind groups, `ModelVertex`'s own vertex layout,
/// depth-`EQUAL` with no write, and [`glint_blend`].
pub struct GlintPipeline {
    /// The render pipeline.
    pub pipeline: wgpu::RenderPipeline,
    /// Group 0: the [`GlintUniform`].
    pub uniform_layout: wgpu::BindGroupLayout,
    /// Group 1: the glint texture and its sampler.
    pub texture_layout: wgpu::BindGroupLayout,
}

impl GlintPipeline {
    /// Build the pipeline for a colour target of `format` and the model pass's
    /// depth format.
    ///
    /// `cull_mode` is `None`, matching vanilla's `withCull(false)`
    /// (`RenderPipelines.java:427`). That is not incidental: the item slab's back
    /// face is drawn by the model pass, and a culled glint pass would leave it
    /// unshimmered.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-glint"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/glint.wgsl").into()),
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-glint-uniform"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(
                        std::mem::size_of::<GlintUniform>() as u64,
                    ),
                },
                count: None,
            }],
        });

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-glint-texture"),
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
            label: Some("lodestone-glint-layout"),
            // Two groups. See this module's doc: the model pipeline is at the
            // portable 4-group floor, and this pass sidesteps that entirely by
            // being its own pipeline rather than by adding a group to that one.
            bind_group_layouts: &[Some(&uniform_layout), Some(&texture_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-glint-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                // `ModelVertex::vertex_layout()` unchanged, so this pass can be
                // handed the *same* vertex buffer the model pass drew — which is
                // what makes depth-EQUAL viable at all.
                buffers: &[Some(ModelVertex::vertex_layout())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(glint_blend()),
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
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                // `DepthStencilState(CompareOp.EQUAL, false)`: no depth write.
                depth_write_enabled: Some(false),
                depth_compare: Some(DEPTH_COMPARE),
                stencil: wgpu::StencilState::default(),
                // Zero bias, as vanilla's two-argument constructor fills in. A
                // bias here would break the EQUAL test rather than help it.
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            uniform_layout,
            texture_layout,
        }
    }

    /// The group-0 bind group over an uploaded [`GlintUniform`] buffer.
    #[must_use]
    pub fn uniform_bind_group(
        &self,
        device: &wgpu::Device,
        buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-glint-uniform-bg"),
            layout: &self.uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        })
    }

    /// The group-1 bind group over a glint texture view and sampler.
    ///
    /// Build the sampler with [`glint_sampler`] — `REPEAT`/`LINEAR` is not a
    /// stylistic choice, it is what the texture's `.mcmeta` resolves to, and
    /// `CLAMP_TO_EDGE` would smear one edge texel across the whole item once the
    /// scroll offset carried the UV past 1.0.
    #[must_use]
    pub fn texture_bind_group(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-glint-texture-bg"),
            layout: &self.texture_layout,
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
}

/// The sampler both glint textures resolve to: `REPEAT` on both axes, `LINEAR`
/// min and mag, no mipmaps.
///
/// Derived rather than chosen: `withTexture("Sampler0", …)` supplies a `null`
/// sampler (`RenderSetup.java:138-141`), so the sampler comes from the texture's
/// own `.mcmeta`, which for both glint textures is
/// `{"texture":{"blur":true}}` — no `clamp`. `ReloadableTexture.java:24-29` maps
/// that to `clamp=false → REPEAT` and `blur=true → LINEAR`, with mipmaps off.
#[must_use]
pub fn glint_sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("lodestone-glint-sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::Repeat,
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every scale factor against the jar literal.
    #[test]
    fn scale_factors_match_texture_transform() {
        assert!((Scale::Item.factor() - 8.0).abs() < f32::EPSILON);
        assert!((Scale::Entity.factor() - 0.5).abs() < f32::EPSILON);
        assert!((Scale::Armour.factor() - 0.16).abs() < 1e-6);
    }

    /// `Scale::Armour` is the only one on the armour texture, and there is no
    /// third `enchanted_glint_entity.png`.
    #[test]
    fn only_armour_uses_the_armour_texture() {
        assert_eq!(Scale::Item.texture(), textures::ITEM);
        assert_eq!(Scale::Entity.texture(), textures::ITEM);
        assert_eq!(Scale::Armour.texture(), textures::ARMOUR);
        assert_ne!(textures::ITEM, textures::ARMOUR);
    }

    /// The clock is a **truncating** cast of `millis * speed * 8.0`.
    #[test]
    fn the_clock_truncates_rather_than_rounds() {
        // 1.0 ms at default speed: 1 * 0.5 * 8 = 4.0 exactly.
        assert_eq!(glint_clock(1.0, DEFAULT_SPEED), 4);
        // 0.9 ms: 0.9 * 0.5 * 8 = 3.6 → 3, not 4. `round()` fails here.
        assert_eq!(glint_clock(0.9, DEFAULT_SPEED), 3);
        // The effective multiplier at defaults is 4.0.
        assert_eq!(glint_clock(1000.0, DEFAULT_SPEED), 4000);
        // And 8.0 at full speed.
        assert_eq!(glint_clock(1000.0, 1.0), 8000);
    }

    /// The two offsets use **different** periods, and both stay in `[0, 1)`.
    ///
    /// The discriminating input is a clock between the two periods: at
    /// `m = 30000` the `V` offset has wrapped to exactly `0` while `U` has not.
    /// A port that used one period for both axes gives equal offsets here.
    #[test]
    fn the_two_axes_use_different_periods() {
        let (u, v) = glint_offsets(30_000);
        assert!((v - 0.0).abs() < 1e-6, "V wraps at 30000, so v == 0");
        assert!(
            (u - 30_000.0 / 110_000.0).abs() < 1e-6,
            "U does not wrap at 30000"
        );
        assert_ne!(
            u, v,
            "one period used for both axes would make these equal — the historical \
             two-layer glint collapsed into a single diagonal scroll"
        );

        for m in [0_i64, 1, 29_999, 30_000, 109_999, 110_000, 250_000] {
            let (u, v) = glint_offsets(m);
            assert!((0.0..1.0).contains(&u), "u out of range at {m}: {u}");
            assert!((0.0..1.0).contains(&v), "v out of range at {m}: {v}");
        }
        // Both wrap to zero together only at a common multiple.
        let (u, v) = glint_offsets(0);
        assert_eq!((u, v), (0.0, 0.0));
    }

    /// Real-time wrap periods at the shipped defaults: 27.5 s on `U`, 7.5 s on
    /// `V`. Computed from the option default rather than hardcoded, so a change
    /// to [`DEFAULT_SPEED`] shows up as a failure here.
    #[test]
    fn real_time_wrap_periods_at_default_speed() {
        let mult = DEFAULT_SPEED * SPEED_MILLIS;
        assert!((mult - 4.0).abs() < 1e-9);
        assert_eq!((U_PERIOD as f64 / mult) as i64, 27_500);
        assert_eq!((V_PERIOD as f64 / mult) as i64, 7_500);
    }

    /// The matrix is `T · Rz · S`, i.e. **scale first**, then rotate, then
    /// translate.
    ///
    /// This is the assertion that catches JOML's fluent-chain trap. At `t = 0`
    /// both offsets are zero so the translation drops out, leaving `Rz(10°) ·
    /// S(8)`: a UV of `(1, 0)` must land at `8 * (cos 10°, sin 10°)`. Under the
    /// reversed composition `S · Rz` the same input lands in the same place —
    /// which is exactly why the zero-time case alone is *not* sufficient, and why
    /// the next test uses a non-zero time.
    #[test]
    fn the_matrix_scales_then_rotates() {
        let m = glint_texture_matrix(0.0, DEFAULT_SPEED, Scale::Item);
        let out = m * glam::Vec4::new(1.0, 0.0, 0.0, 1.0);
        let a = ROTATION_DEGREES.to_radians();
        assert!((out.x - 8.0 * a.cos()).abs() < 1e-4, "x = {}", out.x);
        assert!((out.y - 8.0 * a.sin()).abs() < 1e-4, "y = {}", out.y);
    }

    /// The load-bearing composition test: the translation is applied **last** and
    /// is therefore **not** scaled by 8.
    ///
    /// Two hypotheses, both computed from the constants rather than asserted as a
    /// direction:
    ///
    /// * correct (`T · Rz · S`): the origin maps to `(-u_off, +v_off)`.
    /// * reversed (`S · Rz · T`): the origin maps to `8 · Rz · (-u, v)`, i.e.
    ///   eight times further out and rotated.
    ///
    /// The measurement must land on the first. Getting this backwards scales the
    /// scroll offset by 8 and pushes the glint off the texture; with `REPEAT`
    /// sampling that still produces *a* moving pattern, so it looks plausible in
    /// a screenshot and is invisible to any "does it shimmer?" assertion.
    #[test]
    fn the_translation_is_not_scaled_by_the_glint_scale() {
        // A time whose offsets are both clearly non-zero and unequal.
        let millis = 1_234.0;
        let clock = glint_clock(millis, DEFAULT_SPEED);
        let (u, v) = glint_offsets(clock);
        assert!(u > 0.0 && v > 0.0 && (u - v).abs() > 1e-3, "u={u} v={v}");

        let m = glint_texture_matrix(millis, DEFAULT_SPEED, Scale::Item);
        let origin = m * glam::Vec4::new(0.0, 0.0, 0.0, 1.0);

        // Hypothesis A: T applied last.
        let correct = glam::Vec2::new(-u, v);
        // Hypothesis B: the fluent chain read left-to-right, S applied last.
        let reversed = {
            let rot = glam::Mat4::from_rotation_z(ROTATION_DEGREES.to_radians());
            let p = rot * glam::Vec4::new(-u, v, 0.0, 1.0);
            glam::Vec2::new(p.x, p.y) * Scale::Item.factor()
        };

        let d_correct = (glam::Vec2::new(origin.x, origin.y) - correct).length();
        let d_reversed = (glam::Vec2::new(origin.x, origin.y) - reversed).length();
        assert!(
            d_correct < 1e-5,
            "origin {origin:?} is {d_correct} from the correct hypothesis {correct:?}"
        );
        assert!(
            d_reversed > 0.1,
            "the two hypotheses are only {d_reversed} apart at this time — pick another \
             time, this one does not discriminate"
        );
    }

    /// `U` scrolls negative and `V` positive
    /// (`translation(-layerOffset0, layerOffset1, 0)`).
    #[test]
    fn u_scrolls_negative_and_v_positive() {
        let m = glint_texture_matrix(1_234.0, DEFAULT_SPEED, Scale::Item);
        let origin = m * glam::Vec4::new(0.0, 0.0, 0.0, 1.0);
        assert!(origin.x < 0.0, "U offset must be negative: {}", origin.x);
        assert!(origin.y > 0.0, "V offset must be positive: {}", origin.y);
    }

    /// `GlintAlpha`'s default is `0.75`, not `1.0`. A port that drops it is 33%
    /// too bright and no direction-only assertion notices.
    #[test]
    fn glint_strength_default_is_not_one() {
        assert!((DEFAULT_STRENGTH - 0.75).abs() < f32::EPSILON);
        assert_ne!(DEFAULT_STRENGTH, 1.0);
    }

    /// The blend is `SRC_COLOR/ONE` on colour and `ZERO/ONE` on alpha — not
    /// `TRANSLUCENT`, not plain `ADDITIVE`.
    #[test]
    fn the_blend_is_src_color_additive_and_leaves_destination_alpha_alone() {
        let b = glint_blend();
        assert_eq!(b.color.src_factor, wgpu::BlendFactor::Src);
        assert_eq!(b.color.dst_factor, wgpu::BlendFactor::One);
        assert_eq!(b.color.operation, wgpu::BlendOperation::Add);
        assert_eq!(b.alpha.src_factor, wgpu::BlendFactor::Zero);
        assert_eq!(b.alpha.dst_factor, wgpu::BlendFactor::One);
        // The two wrong guesses, named so the assertion says what it excludes.
        assert_ne!(
            b.color.src_factor,
            wgpu::BlendFactor::SrcAlpha,
            "TRANSLUCENT, not GLINT"
        );
        assert_ne!(b.color.src_factor, wgpu::BlendFactor::One, "ADDITIVE, not GLINT");
    }

    /// Depth is `EQUAL` with no write, and `EQUAL` does not flip under our
    /// `[0,1]` depth convention.
    #[test]
    fn depth_compare_is_equal_unflipped() {
        assert_eq!(DEPTH_COMPARE, wgpu::CompareFunction::Equal);
        // The flipped forms, which is what a mechanical port of every other
        // depth comparison in this crate would have produced.
        assert_ne!(DEPTH_COMPARE, wgpu::CompareFunction::LessEqual);
        assert_ne!(DEPTH_COMPARE, wgpu::CompareFunction::GreaterEqual);
    }

    /// Armour's Z nudge, both projection forms.
    #[test]
    fn armour_view_offset_constants() {
        assert!((ARMOUR_VIEW_OFFSET_SCALE - 0.999_755_86).abs() < 1e-7);
        assert!((ARMOUR_VIEW_OFFSET_Z_ORTHO - 0.001_953_125).abs() < 1e-9);
    }

    /// The foil gate reads the enchantments list, and the shortfall is asserted
    /// rather than merely documented: a stack with no enchantments does not
    /// glint, which is the state every one of the seven baked-override items
    /// arrives in.
    #[test]
    fn has_foil_reads_the_enchantments_list() {
        use lodestone_model::item::ItemComponents;

        let plain = ItemComponents::default();
        assert!(!has_foil(&plain));

        // An enchanted stack. Constructed through the public field so this test
        // fails to compile rather than silently passing if the field moves.
        let mut enchanted = ItemComponents::default();
        enchanted
            .enchantments
            .push(lodestone_model::item::ItemEnchantment {
                id: 0,
                level: 1,
            });
        assert!(has_foil(&enchanted));
    }

    /// The `GlintUniform` is `std140`-compatible for a uniform buffer: two mat4s
    /// and a vec4, all 16-byte aligned, 144 bytes total. A wrong size here is a
    /// pipeline-creation failure that only a GPU gate would otherwise catch.
    #[test]
    fn glint_uniform_layout_is_std140_compatible() {
        assert_eq!(std::mem::size_of::<GlintUniform>(), 64 + 64 + 16);
        assert_eq!(std::mem::size_of::<GlintUniform>() % 16, 0);
        assert_eq!(std::mem::align_of::<GlintUniform>(), 4);
    }

    /// The uniform carries the texture matrix that [`glint_texture_matrix`]
    /// produces — i.e. the constructor is wired to the maths, not to a second
    /// copy of it.
    #[test]
    fn the_uniform_carries_the_computed_texture_matrix() {
        let vp = glam::Mat4::IDENTITY.to_cols_array_2d();
        let u = GlintUniform::new(
            vp,
            [1.0, 2.0, 3.0],
            1_234.0,
            DEFAULT_SPEED,
            DEFAULT_STRENGTH,
            Scale::Item,
        );
        assert_eq!(
            u.tex_matrix,
            glint_texture_matrix(1_234.0, DEFAULT_SPEED, Scale::Item).to_cols_array_2d()
        );
        assert_eq!(u.origin_and_alpha, [1.0, 2.0, 3.0, DEFAULT_STRENGTH]);
    }
}
