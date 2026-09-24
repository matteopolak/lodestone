//! GPU-requiring pixel gate for the sky pass ([`SkyRenderer`]).
//!
//! `#[ignore]`d so the default `cargo test` run stays hermetic and headless —
//! run with `cargo test -p lodestone-render --test sky_pipeline_gpu --
//! --ignored --nocapture` on a machine with a real adapter.
//!
//! The tests above `// --- real client.jar gates below ---` use a synthetic
//! in-memory resource pack (solid-colour sun/moon/cloud textures), not the
//! real jar: they are about proving the *pass* paints pixels end to end (disc
//! + celestial + star + cloud draws, in one render pass with no depth
//! attachment), not about matching real vanilla art.
//!
//! **That was the whole gap.** Nothing in this crate (or `lodestone-shell`'s
//! `sky_pixels.rs`) ever loaded real celestial/cloud art before this file's
//! `real_jar_*` tests: `sun.png` in the 26.2 client jar has no alpha channel
//! at all (a near-black-to-white radial falloff baked straight into opaque
//! RGB — vanilla only ever *adds* it onto the sky, never replaces), and
//! `clouds.png` is a hard binary alpha mask, not a soft one. A solid-colour
//! synthetic sun/cloud texture cannot exercise either property, so the wiring
//! gates above stayed green throughout the two-defect regression this file's
//! new tests were written to catch (see `docs/sky-and-air-bubbles.md`'s
//! "known gaps" note).

use lodestone_assets::{CelestialAtlas, MemorySource, ResourceManager, ZipSource};
use lodestone_render::{
    Camera, CelestialVertex, CloudVertex, GpuContext, HeadlessTarget, RenderTarget, SUN_HEIGHT,
    SUN_SIZE, SkyRenderer, celestial_angle_for_time_of_day, celestial_quad_positions,
    celestial_quad_uvs, quad_indices,
};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 64;

fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&rgba);
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(&buf).unwrap();
    }
    out
}

/// A synthetic pack with an opaque sun, all 8 moon phases, and an opaque
/// cloud texture (fully opaque so a single cloud draw covers the whole
/// screen, keeping the pixel assertion simple and robust).
fn manager() -> ResourceManager {
    let mut src = MemorySource::new("sky-gpu-test");
    src.insert(
        "assets/minecraft/textures/environment/celestial/sun.png".to_string(),
        png(8, 8, [255, 220, 0, 255]),
    );
    for name in lodestone_assets::MOON_PHASE_NAMES {
        src.insert(
            format!("assets/minecraft/textures/environment/celestial/moon/{name}.png"),
            png(8, 8, [200, 200, 200, 255]),
        );
    }
    src.insert(
        "assets/minecraft/textures/environment/clouds.png".to_string(),
        png(4, 4, [255, 255, 255, 255]),
    );
    ResourceManager::new(vec![Box::new(src)])
}

/// Counts pixels that differ noticeably from pure black (the render pass's
/// clear colour), from a tightly-packed RGBA8 readback.
fn non_black_fraction(pixels: &[u8]) -> f64 {
    let mut non_black = 0usize;
    let mut total = 0usize;
    for px in pixels.chunks_exact(4) {
        total += 1;
        if px[0] > 8 || px[1] > 8 || px[2] > 8 {
            non_black += 1;
        }
    }
    non_black as f64 / total.max(1) as f64
}

fn ctx() -> Option<GpuContext> {
    match GpuContext::new_headless_blocking() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipping: no GPU adapter available: {e}");
            None
        }
    }
}

/// **Negative control.** A target that the sky pass never touches must read
/// back as the fully-black texture wgpu hands out on creation. This is what
/// proves the affirmative test below is measuring something real: if the
/// detector (`non_black_fraction`) could not tell a painted target from an
/// untouched one, the affirmative test would be worthless.
#[test]
#[ignore = "requires a GPU adapter"]
fn control_an_untouched_target_reads_back_as_black() {
    let Some(ctx) = ctx() else { return };
    let mut target = HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, wgpu::TextureFormat::Rgba8UnormSrgb);
    let _ = target.acquire().expect("acquire");
    let pixels = target.read_texels(ctx.device(), ctx.queue());
    let frac = non_black_fraction(&pixels);
    assert!(
        frac < 0.01,
        "control failed: an untouched target should read back as black, got {:.1}% non-black",
        frac * 100.0
    );
}

/// The sky pass, run once at night (so the star/moon draws are active too,
/// not just the disc), must leave the color target majority non-black.
///
/// The camera looks steeply *up* (`pitch = -60`, this project's convention is
/// positive pitch looks down — see `camera.rs`), not level: the sky disc is a
/// flat plane 16 units above the camera and this pass deliberately does not
/// draw vanilla's below-horizon "dark disc" (see `SkyRenderer::render`'s
/// doc comment on that omission), so a level camera only ever paints the
/// upper ~half of the frame — correct (the lower half is where a real
/// terrain pass would draw), but not what this gate is checking. Looking up
/// keeps the frustum inside the painted region.
#[test]
#[ignore = "requires a GPU adapter"]
fn sky_pass_paints_the_whole_frame() {
    let Some(ctx) = ctx() else { return };
    let sky = SkyRenderer::new(
        ctx.device(),
        ctx.queue(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &manager(),
    )
    .expect("build sky renderer over the synthetic pack");

    let mut target = HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, wgpu::TextureFormat::Rgba8UnormSrgb);
    let frame = target.acquire().expect("acquire");

    let camera = Camera {
        position: glam::Vec3::new(0.0, 70.0, 0.0),
        yaw: 0.0,
        pitch: -60.0,
        fov_y_degrees: 90.0,
        aspect: WIDTH as f32 / HEIGHT as f32,
        near: 0.05,
        far: 1024.0,
    };

    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sky-gpu-test-encoder"),
        });
    // Midnight: `time_of_day = 18_000` — stars and the moon are both fully
    // active (`star_brightness_for_time_of_day(18_000) > 0`), so this frame
    // exercises every one of the four draws, not just the disc.
    sky.render(
        ctx.device(),
        ctx.queue(),
        &mut encoder,
        frame.view(),
        &camera,
        // `.with_cloud_status(Fast)`: this gate's ">50% non-black" threshold was
        // set against FAST's near-unbounded quad (`CLOUD_PLANE_HALF_EXTENT` =
        // 768 blocks, alpha-tested every pixel with no radial cutoff). FANCY
        // (the default — see `CloudStatus`'s doc) only builds real
        // geometry within `CLOUD_FANCY_RADIUS_CELLS` (192 blocks) of the
        // camera, a deliberately bounded per-frame-CPU-rebuild cost — at this
        // steep upward pitch that mesh subtends far less of the frame than
        // FAST's quad does, and the disc itself paints solid **black** at
        // midnight (`night_sky_is_black_but_night_fog_is_not`), so this
        // specific camera/time combination is not a fair coverage test of
        // FANCY. `fancy_clouds_paint_real_pixels_near_the_camera` below is the
        // dedicated anti-island proof for FANCY; this one keeps proving what
        // it was written for — that all four passes paint, not just the disc.
        &lodestone_render::SkyFrame::new(18_000, [0.24, 0.46, 0.83])
            .with_cloud_status(lodestone_render::CloudStatus::Fast),
        // Deliberately black, *not* `SkyFrame::clear_color`: this gate's whole
        // metric is "did anything paint here", and the shipped clear (the fog
        // colour) satisfies it for free. See `SkyRenderer::render`'s doc.
        wgpu::Color::BLACK,
    );
    ctx.queue().submit(std::iter::once(encoder.finish()));

    let pixels = target.read_texels(ctx.device(), ctx.queue());
    let frac = non_black_fraction(&pixels);
    assert!(
        frac > 0.5,
        "expected the sky pass to paint most of the frame (disc alone is opaque \
         and covers the FOV), only {:.1}% non-black",
        frac * 100.0
    );
}

// ---------------------------------------------------------------------------
// Real client.jar gates below — see the module docs for why these exist.
// ---------------------------------------------------------------------------

/// A representative day sky colour, matching what `app.rs`/`gpu.rs` actually
/// hand `SkyRenderer::render` as `day_sky_color` (`gpu.rs::SKY_COLOR`).
const DAY_SKY: [f32; 3] = [0.25, 0.46, 0.83];

fn clear_to(color: [f32; 3]) -> wgpu::Color {
    wgpu::Color {
        r: f64::from(color[0]),
        g: f64::from(color[1]),
        b: f64::from(color[2]),
        a: 1.0,
    }
}

/// Locates a fetched `.cache/mc/<version>/client.jar`, preferring `26.2` —
/// this repo's active scope (`CLAUDE.md`). `None` when no jar has been
/// fetched; every test below fails closed on that rather than skipping (same
/// discipline as `lodestone-assets/tests/real_jar.rs` and the `lodestone-shell`
/// `*_pixels.rs` gates: "a missing GPU or a missing `client.jar` is a failure,
/// never a skip").
fn real_jar_manager() -> Option<ResourceManager> {
    let cache = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .join(".cache/mc");
    let preferred = cache.join("26.2/client.jar");
    let jar = if preferred.is_file() {
        preferred
    } else {
        std::fs::read_dir(&cache)
            .ok()?
            .flatten()
            .map(|e| e.path().join("client.jar"))
            .find(|p| p.is_file())?
    };
    let source = ZipSource::open(&jar).ok()?;
    Some(ResourceManager::new(vec![Box::new(source)]))
}

/// Builds a [`Camera`] at the origin whose forward vector is `direction`,
/// inverting `Camera::forward`'s own formula
/// (`crates/lodestone-render/src/camera.rs`'s module docs): `forward =
/// (-cos(pitch)*sin(yaw), -sin(pitch), cos(pitch)*cos(yaw))`.
fn camera_facing(direction: glam::Vec3, aspect: f32, fov_y_degrees: f32) -> Camera {
    let d = direction.normalize();
    let pitch = (-d.y).clamp(-1.0, 1.0).asin().to_degrees();
    let yaw = (-d.x).atan2(d.z).to_degrees();
    Camera {
        position: glam::Vec3::new(0.0, 70.0, 0.0),
        yaw,
        pitch,
        fov_y_degrees,
        aspect,
        near: 0.05,
        far: 4096.0,
    }
}

/// Fraction of pixels whose every channel is below `threshold` — the
/// discriminator the sun gate below uses. Neither the day sky colour
/// (`DAY_SKY`, bytes ~`[64, 117, 212]`) nor an opaque, additively-blended sun
/// texel is ever this dark; only the reported "solid black" sun is.
fn near_black_fraction(pixels: &[u8], threshold: u8) -> f64 {
    let mut dark = 0usize;
    let mut total = 0usize;
    for px in pixels.chunks_exact(4) {
        total += 1;
        if px[0] < threshold && px[1] < threshold && px[2] < threshold {
            dark += 1;
        }
    }
    dark as f64 / total.max(1) as f64
}

/// Fraction of pixels that are close to **neither** `a` nor `b` (Manhattan
/// distance over RGB, `> tolerance` from both) — the discriminator the cloud
/// gate below uses instead of [`near_black_fraction`].
///
/// A literal near-black threshold turned out to be the wrong tool here: the
/// cloud fragment shader multiplies the sampled colour by a `~0.75`-average
/// tint (`cloud_tint` in `SkyRenderer::render`), so a partially-blended
/// boundary texel (say 50% of the way from transparent to opaque) lands at
/// roughly *half of an already-dimmed* colour — well above any reasonable
/// "near black" byte threshold even though it is visibly, meaningfully darker
/// than a correctly-drawn cloud. What actually distinguishes a broken
/// (Linear-filtered) render from a correct (Nearest-filtered) one is that the
/// broken one has a *third population* of pixel colours at all: with Nearest,
/// every drawn pixel is either the exact background (`b`, discarded) or the
/// exact full tint (`a`, drawn) — never anything else, since Nearest sampling
/// can never return a partial-coverage texel. Linear sampling of a hard-edged
/// mask produces exactly that third population, at the boundary between every
/// opaque and transparent texel. So "close to neither known colour" is the
/// direct signature of the bug, where "near black" was only an indirect proxy
/// for it that happened to be too narrow a band to reliably register.
fn fringe_fraction(pixels: &[u8], a: [u8; 3], b: [u8; 3], tolerance: i32) -> f64 {
    let dist = |px: &[u8], reference: [u8; 3]| {
        (i32::from(px[0]) - i32::from(reference[0])).abs()
            + (i32::from(px[1]) - i32::from(reference[1])).abs()
            + (i32::from(px[2]) - i32::from(reference[2])).abs()
    };
    let mut fringe = 0usize;
    let mut total = 0usize;
    for px in pixels.chunks_exact(4) {
        total += 1;
        if dist(px, a) > tolerance && dist(px, b) > tolerance {
            fringe += 1;
        }
    }
    fringe as f64 / total.max(1) as f64
}

/// Renders just the real sun quad, sampling the real jar's celestial atlas,
/// with a caller-chosen blend state, and returns [`near_black_fraction`] of
/// the result. Used only to reproduce the pre-fix `ALPHA_BLENDING` setting as
/// an **executed** control — the shipped `CelestialPipeline` no longer has a
/// way to select that blend at all (`CELESTIAL_BLEND` in `sky_pipeline.rs` is
/// its only option now), so this rebuilds the minimal equivalent pipeline
/// directly against the public API rather than reaching into private items.
fn render_sun_with_blend(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    manager: &ResourceManager,
    camera: &Camera,
    blend: wgpu::BlendState,
) -> f64 {
    const SHADER: &str = r"
struct Camera { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(position, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let color = textureSample(atlas_tex, atlas_smp, in.uv);
    if color.a < 0.05 {
        discard;
    }
    return color;
}
";

    let atlas = CelestialAtlas::build(manager).expect("build celestial atlas from the real jar");
    let sun_sprite = atlas.sun_sprite().expect("real jar has a sun sprite");
    let rect = [
        sun_sprite.uv_min[0],
        sun_sprite.uv_min[1],
        sun_sprite.uv_max[0],
        sun_sprite.uv_max[1],
    ];
    let uvs = celestial_quad_uvs(rect, false);
    let angle = celestial_angle_for_time_of_day(6_000) * std::f32::consts::TAU;
    let positions = celestial_quad_positions(angle, SUN_HEIGHT, SUN_SIZE);
    let verts: Vec<CelestialVertex> = (0..4)
        .map(|i| CelestialVertex {
            position: positions[i],
            uv: uvs[i],
        })
        .collect();

    let raw = atlas.atlas();
    let pixels = render_textured_quad(
        device,
        queue,
        format,
        width,
        height,
        camera.sky_view_projection(),
        SHADER,
        &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2],
        std::mem::size_of::<CelestialVertex>() as u64,
        bytemuck::cast_slice(&verts),
        raw.width,
        raw.height,
        &raw.rgba,
        wgpu::AddressMode::ClampToEdge,
        wgpu::FilterMode::Linear,
        Some(blend),
    );
    near_black_fraction(&pixels, 20)
}

/// Renders just the real cloud plane, sampling the real jar's `clouds.png`,
/// with a caller-chosen sampler filter, and returns [`fringe_fraction`] of the
/// result (against the same `DAY_SKY`/`DAY_SKY * 0.9` reference colours the
/// caller test uses for its subject measurement). Used only to reproduce the
/// pre-fix `Linear` filtering as an **executed** control — `SkyRenderer` now
/// always uploads the cloud texture with `Nearest`
/// (`upload_plain_texture`'s caller in `sky_pipeline.rs`), so this rebuilds
/// the minimal equivalent pipeline directly against the public API rather
/// than reaching into private items.
fn render_clouds_with_filter(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    manager: &ResourceManager,
    filter: wgpu::FilterMode,
) -> f64 {
    const SHADER: &str = r"
struct Camera { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var cloud_tex: texture_2d<f32>;
@group(0) @binding(2) var cloud_smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(position, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let sampled = textureSample(cloud_tex, cloud_smp, in.uv);
    if sampled.a < 0.04 {
        discard;
    }
    return vec4<f32>(sampled.rgb * in.color.rgb, in.color.a);
}
";

    let cloud_image =
        lodestone_assets::load_cloud_texture(manager).expect("load real clouds.png from the jar");

    // A full-screen NDC quad sampling a small, **hand-verified** boundary
    // window of the real texture — texels `x in 2..8, y in 21..27` — rather
    // than the shipped `cloud_plane_geometry`'s actual 3-D placement, or this
    // control's own first two attempts (a partial-texel window at the
    // subject's on-screen size, then the whole texture shrunk into a small
    // target to force minification). All three read back 0.0% near-black
    // regardless of filter — measured, not assumed. The reason: without
    // knowing exactly where a transparent/opaque boundary sits in the real
    // 256x256 texture, an emergently-chosen window has no guarantee of
    // containing one at all (`clouds.png` is ~28% opaque overall, in
    // organic, unevenly-distributed blobs), so a plausible-looking window can
    // easily land entirely inside one uniform region. Confirmed by decoding
    // the real file directly: `(4, 23)` is opaque white, `(5, 23)` is fully
    // transparent, one texel apart — a real edge. Magnifying exactly that
    // known 6x6-texel neighbourhood (`~11px/texel` at `CONTROL_SIZE = 64`)
    // makes the interpolated boundary band large enough on screen to read
    // back deterministically, regardless of filter-vs-minification questions.
    const CONTROL_SIZE: u32 = 64;
    let (u0, v0) = (2.0 / cloud_image.width as f32, 21.0 / cloud_image.height as f32);
    let (u1, v1) = (8.0 / cloud_image.width as f32, 27.0 / cloud_image.height as f32);
    let positions: [[f32; 3]; 4] = [
        [-1.0, -1.0, 0.0],
        [1.0, -1.0, 0.0],
        [1.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0],
    ];
    let uvs: [[f32; 2]; 4] = [[u0, v0], [u1, v0], [u1, v1], [u0, v1]];
    // The same tint the shipped path now uses: the real `CLOUD_COLOR` attribute,
    // pure white at alpha 0.8 (`sky::CLOUD_COLOR_RGB`/`CLOUD_COLOR_ALPHA`), which
    // at noon the `CLOUD_COLOR` track leaves untouched. A control whose tint
    // differed from the subject's would not be a control for the subject.
    let tint = [1.0, 1.0, 1.0, lodestone_render::sky::CLOUD_COLOR_ALPHA];
    let verts: Vec<CloudVertex> = (0..4)
        .map(|i| CloudVertex {
            position: positions[i],
            uv: uvs[i],
            color: tint,
        })
        .collect();

    let pixels = render_textured_quad(
        device,
        queue,
        format,
        CONTROL_SIZE,
        CONTROL_SIZE,
        glam::Mat4::IDENTITY,
        SHADER,
        &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x4],
        std::mem::size_of::<CloudVertex>() as u64,
        bytemuck::cast_slice(&verts),
        cloud_image.width,
        cloud_image.height,
        &cloud_image.rgba,
        wgpu::AddressMode::Repeat,
        filter,
        // Alpha-blended, matching the shipped `CloudPipeline`'s `CLOUD_BLEND`
        // (vanilla's translucent blend function). This was `None` while the
        // shipped pipeline was opaque.
        Some(wgpu::BlendState::ALPHA_BLENDING),
    );
    let background = DAY_SKY.map(|c| (c * 255.0).round() as u8);
    let full_color = cloud_over_sky();
    fringe_fraction(&pixels, full_color, background, 12)
}

/// The byte value a fully-covered cloud pixel reads back as: vanilla's white
/// cloud colour composited over the day sky at [`CLOUD_COLOR_ALPHA`]'s `0.8`.
///
/// `render_textured_quad` and the real-jar cloud gate both target
/// **`Rgba8Unorm`** (see that gate's comment on why), so the shader's linear
/// output lands in the bytes unchanged and `dst = a*1.0 + (1-a)*sky` is the whole
/// arithmetic. Derived from the constants rather than written out, because the
/// alpha it depends on is vanilla's and belongs in one place.
///
/// It used to be `DAY_SKY * 0.9` — the invented cloud darkening this replaced.
fn cloud_over_sky() -> [u8; 3] {
    let a = lodestone_render::sky::CLOUD_COLOR_ALPHA;
    DAY_SKY.map(|c| ((a + (1.0 - a) * c) * 255.0).round() as u8)
}

/// Shared plumbing for both control renderers above: one textured quad, one
/// combined (uniform + texture + sampler) bind group, cleared to [`DAY_SKY`]
/// first so every pixel in the readback is attributable either to that clear
/// or to the draw. Returns the raw RGBA8 readback rather than a metric — the
/// two callers want different metrics ([`near_black_fraction`] for the sun,
/// [`fringe_fraction`] for the clouds), so this only owns the GPU plumbing.
/// Not shared with `sky_pipeline.rs` itself — that module's
/// `build_pipeline`/`upload_plain_texture`/etc. are private, which is the
/// point: these controls exist to prove what happens *without* this crate's
/// shipped, fixed pipeline construction, so they cannot reuse it.
#[allow(clippy::too_many_arguments)]
fn render_textured_quad(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    view_proj: glam::Mat4,
    shader_src: &str,
    attrs: &[wgpu::VertexAttribute],
    vertex_stride: u64,
    vertex_bytes: &[u8],
    tex_width: u32,
    tex_height: u32,
    tex_rgba: &[u8],
    address_mode: wgpu::AddressMode,
    filter: wgpu::FilterMode,
    blend: Option<wgpu::BlendState>,
) -> Vec<u8> {
    use wgpu::util::DeviceExt;

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("real-jar-control-texture"),
        size: wgpu::Extent3d {
            width: tex_width.max(1),
            height: tex_height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
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
        tex_rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * tex_width.max(1)),
            rows_per_image: Some(tex_height.max(1)),
        },
        wgpu::Extent3d {
            width: tex_width.max(1),
            height: tex_height.max(1),
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("real-jar-control-sampler"),
        address_mode_u: address_mode,
        address_mode_v: address_mode,
        address_mode_w: address_mode,
        mag_filter: filter,
        min_filter: filter,
        ..Default::default()
    });

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct CamUniform {
        view_proj: [[f32; 4]; 4],
    }
    let cam_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("real-jar-control-camera"),
        contents: bytemuck::bytes_of(&CamUniform {
            view_proj: view_proj.to_cols_array_2d(),
        }),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("real-jar-control-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("real-jar-control-bg"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: cam_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("real-jar-control-shader"),
        source: wgpu::ShaderSource::Wgsl(shader_src.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("real-jar-control-pipeline-layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("real-jar-control-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: vertex_stride,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: attrs,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
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
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    let indices = quad_indices();
    let index_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("real-jar-control-indices"),
        contents: bytemuck::cast_slice(&indices[..]),
        usage: wgpu::BufferUsages::INDEX,
    });
    let vertex_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("real-jar-control-vertices"),
        contents: vertex_bytes,
        usage: wgpu::BufferUsages::VERTEX,
    });

    let mut target = HeadlessTarget::new(device, width, height, format);
    let frame = target.acquire().expect("headless acquire");
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("real-jar-control-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("real-jar-control-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: frame.view(),
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear_to(DAY_SKY)),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.set_vertex_buffer(0, vertex_buf.slice(..));
        pass.set_index_buffer(index_buf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..6, 0, 0..1);
    }
    queue.submit(std::iter::once(encoder.finish()));

    target.read_texels(device, queue)
}

/// **Real-jar regression gate for the reported "sun is far too big, and solid
/// black" defect.**
///
/// Root cause: `environment/celestial/sun.png` in the 26.2 client jar is a
/// fully **opaque** PNG (palette-indexed, no `tRNS` chunk — confirmed by
/// walking its raw chunks) whose RGB is a near-black-to-bright-white radial
/// falloff. Vanilla's celestial render pipeline
/// (its decompiled render-pipelines source, 26.2)
/// blends it with an additive overlay blend function — `dst_factor: One` — so
/// that near-black RGB only ever *adds* a sliver onto the sky.
/// `CelestialPipeline` used ordinary `SrcAlpha`/`OneMinusSrcAlpha` blending
/// before this fix, which *replaces* the destination wherever alpha is 1.0 —
/// i.e. everywhere in this texture — painting the whole opaque 60-block-wide
/// quad as a mostly-black square: the reported bug, and also why the sun
/// *looked* oversized. The geometry itself was never wrong — `SUN_SIZE =
/// 30.0` matches vanilla's own half-extent exactly (`SkyRenderer`'s own decompiled source's
/// `modelViewStack.scale(30.0F, 1.0F, 30.0F)` applied to a `-1..1` quad is the
/// same half-extent-of-30 this crate's `celestial_quad_positions` computes) —
/// only how much of the quad was visible changed.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn real_jar_sun_is_not_solid_black() {
    let Some(ctx) = ctx() else {
        panic!(
            "no GPU adapter available — a missing GPU is a failure, never a skip \
             (this repo's own convention, see e.g. dropped_item_pixels.rs)"
        );
    };
    let manager = real_jar_manager().expect(
        "no client.jar under .cache/mc/<version>/ — fetch it first; a missing jar is a \
         failure, never a skip",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    const W: u32 = 128;
    const H: u32 = 128;

    // Noon (`celestial_angle_for_time_of_day(6_000) == 0.0`): the sun sits at
    // a known, directly-recomputable direction, and the moon (opposite side)
    // and stars (`star_brightness_for_time_of_day(6_000) == 0`) cannot
    // contribute pixels that would confuse the measurement.
    let angle = celestial_angle_for_time_of_day(6_000) * std::f32::consts::TAU;
    let sun_quad = celestial_quad_positions(angle, SUN_HEIGHT, SUN_SIZE);
    let sun_center = sun_quad
        .iter()
        .fold(glam::Vec3::ZERO, |acc, p| acc + glam::Vec3::from(*p))
        / 4.0;
    // Half-FOV (25 deg) comfortably wider than the sun's own angular
    // half-size (`atan(SUN_SIZE / SUN_HEIGHT) ~= 16.7 deg`), so the whole
    // disc sits inside the frame with margin.
    let camera = camera_facing(sun_center, W as f32 / H as f32, 50.0);

    // ---- Subject: the real, shipped SkyRenderer, real jar art. ----
    let sky =
        SkyRenderer::new(device, queue, format, &manager).expect("build sky renderer over the real jar");
    let mut target = HeadlessTarget::new(device, W, H, format);
    let frame = target.acquire().expect("headless acquire");
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("real-jar-sun-subject"),
    });
    sky.render(
        device,
        queue,
        &mut encoder,
        frame.view(),
        &camera,
        &lodestone_render::SkyFrame::new(6_000, DAY_SKY),
        // Black, not the shipped fog-coloured clear: this gate counts
        // *near-black* pixels inside the sun's footprint, so a lit clear would
        // hide the defect it exists to catch.
        wgpu::Color::BLACK,
    );
    queue.submit(std::iter::once(encoder.finish()));
    let subject_pixels = target.read_texels(device, queue);
    let subject_dark = near_black_fraction(&subject_pixels, 20);

    // ---- Control, EXECUTED: same real sun texture, pre-fix blend. ----
    let control_dark =
        render_sun_with_blend(device, queue, format, W, H, &manager, &camera, wgpu::BlendState::ALPHA_BLENDING);

    eprintln!("=== real-jar sun gate ===");
    eprintln!(
        "subject (shipped, CELESTIAL_BLEND=OVERLAY): {:.1}% near-black",
        subject_dark * 100.0
    );
    eprintln!(
        "control (pre-fix ALPHA_BLENDING), same real art: {:.1}% near-black",
        control_dark * 100.0
    );

    assert!(
        subject_dark < 0.15,
        "the real sun should not paint predominantly near-black through the shipped, \
         additively-blended celestial pipeline; got {:.1}% near-black pixels in a frame \
         centred on the sun",
        subject_dark * 100.0
    );
    // Measured on this machine: 28.9%. The sun's opaque square only covers
    // part of the 128x128 frame at this FOV (the rest is `DAY_SKY`
    // background, itself never near-black), so "predominantly near-black"
    // means predominantly near-black *within the quad's own footprint*, not
    // the whole frame — 0.2 is comfortably below the measured value with
    // margin, and comfortably above the ~0% a correctly-blended sun leaves.
    assert!(
        control_dark > 0.2,
        "control failed to fail: the pre-fix ALPHA_BLENDING setting, sampling the exact \
         same real sun texture, should paint a substantial near-black patch where the \
         sun's opaque square sits (that IS the reported bug — measured 28.9% on the \
         machine this gate was written on) — only {:.1}% near-black, so this gate's \
         detector would not actually have caught the regression",
        control_dark * 100.0
    );
}

/// **Real-jar regression gate for the reported "clouds are a rounded black
/// outline with a gradient drop-off inside" defect.**
///
/// Root cause: `clouds.png` in the 26.2 client jar is a hard **binary** alpha
/// mask — every texel is either fully transparent black `(0,0,0,0)` or fully
/// opaque white `(255,255,255,255)`, confirmed by decoding the real file
/// (`load_cloud_texture`'s doc). `CLOUD_WGSL`'s fragment shader alpha-tests at
/// `0.04` and (this pipeline being opaque, no blend state) writes the sampled
/// colour straight through with no blending. Sampling that binary mask with
/// **linear** filtering (this pipeline's setting before this fix) produces a
/// fringe of partial-coverage texels at every cell boundary whose colour and
/// alpha both interpolate proportionally from black toward white; the ones
/// that just clear the `0.04` alpha threshold still carry mostly-black colour,
/// which gets written as-is — the reported black rim, with a soft gradient
/// just inside it as the interpolation continues toward full white. Nearest
/// sampling never produces a partial-coverage texel, so this fringe cannot
/// occur; it also reads closer to vanilla's real per-*cell* (not
/// per-pixel-sampled) cloud mesh, per `cloud_plane_geometry`'s module docs.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn real_jar_clouds_are_not_black_fringed() {
    let Some(ctx) = ctx() else {
        panic!(
            "no GPU adapter available — a missing GPU is a failure, never a skip \
             (this repo's own convention, see e.g. dropped_item_pixels.rs)"
        );
    };
    let manager = real_jar_manager().expect(
        "no client.jar under .cache/mc/<version>/ — fetch it first; a missing jar is a \
         failure, never a skip",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    // `Rgba8Unorm`, not `Rgba8UnormSrgb`: this test's `fringe_fraction` reasons
    // about exact byte values against hand-computed reference colours
    // (`full_color`/`background` below), which is only straightforward
    // without an implicit sRGB gamma curve between the shader's linear output
    // and the bytes read back.
    let format = wgpu::TextureFormat::Rgba8Unorm;
    const W: u32 = 128;
    const H: u32 = 128;

    // Straight up: `Camera::forward` at `yaw = 0, pitch = -90` is exactly
    // `(0, 1, 0)` (see `camera_facing`'s doc on the same formula), which
    // points directly at the overhead cloud plane and fills the frame with
    // it, the same way the existing synthetic-pack gates above look steeply
    // up to fill the frame with the disc.
    let camera = Camera {
        position: glam::Vec3::new(0.0, 70.0, 0.0),
        yaw: 0.0,
        pitch: -90.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 4096.0,
    };

    // The two known colours a correctly-drawn frame can show: the disc's sky
    // colour (`DAY_SKY` exactly, at noon — `sky_color_for_time_of_day`'s day
    // endpoint) wherever nothing else covers it, and vanilla's white cloud
    // colour composited over it at alpha 0.8 wherever the cloud plane does.
    let background = DAY_SKY.map(|c| (c * 255.0).round() as u8);
    let full_color = cloud_over_sky();

    // ---- Subject: the real, shipped SkyRenderer, real jar art. ----
    let sky =
        SkyRenderer::new(device, queue, format, &manager).expect("build sky renderer over the real jar");
    let mut target = HeadlessTarget::new(device, W, H, format);
    let frame = target.acquire().expect("headless acquire");
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("real-jar-clouds-subject"),
    });
    sky.render(
        device,
        queue,
        &mut encoder,
        frame.view(),
        &camera,
        // `.with_cloud_status(Fast)`: this gate is specifically about the FAST
        // alpha-tested quad's fringe artifact, so it must
        // stay pinned to FAST regardless of `SkyFrame::new`'s own default —
        // see `crate::sky::CloudStatus`'s doc for why that default is now
        // `Fancy`. A FANCY 3D mesh has real screen-space gaps
        // between faces that `fringe_fraction` would misclassify as this
        // gate's fringe, which is a different defect entirely.
        &lodestone_render::SkyFrame::new(6_000, DAY_SKY)
            .with_cloud_status(lodestone_render::CloudStatus::Fast),
        // Black, not the shipped fog-coloured clear: `fringe_fraction` classifies
        // pixels against `background`/`full_color`, and a third colour under the
        // quad would be scored as a fringe.
        wgpu::Color::BLACK,
    );
    queue.submit(std::iter::once(encoder.finish()));
    let subject_pixels = target.read_texels(device, queue);
    let subject_fringe = fringe_fraction(&subject_pixels, full_color, background, 12);

    // ---- Control, EXECUTED: same real cloud texture, pre-fix (Linear) filter. ----
    let control_fringe = render_clouds_with_filter(device, queue, format, &manager, wgpu::FilterMode::Linear);

    eprintln!("=== real-jar cloud gate ===");
    eprintln!(
        "subject (shipped, Nearest filter): {:.1}% fringe (neither sky nor full cloud colour)",
        subject_fringe * 100.0
    );
    eprintln!(
        "control (pre-fix Linear filter), same real art: {:.1}% fringe",
        control_fringe * 100.0
    );

    // Measured on this machine: 1.6% (the shipped, full production
    // `SkyRenderer::render` pass, not the isolated cloud draw the control
    // below uses — the residual is attributable to disc/cloud-plane edge
    // rasterisation elsewhere in the same frame, not to cloud sampling:
    // `Nearest` genuinely cannot return a partial-coverage texel, so it is
    // not the mechanism this gate exists to catch). 0.1 keeps ~6x margin
    // above that measured baseline while staying ~3x below the control's own
    // threshold, so a regression back to Linear filtering — an 18x jump in
    // the actual measurement — still fails loudly.
    assert!(
        subject_fringe < 0.1,
        "the real cloud plane should read as close to exactly two colours (sky background, \
         full cloud tint) through the shipped Nearest-filtered sampler — Nearest can never \
         return a partial-coverage texel — but {:.1}% of pixels were neither, well above the \
         ~1.6% baseline this gate was calibrated against",
        subject_fringe * 100.0
    );
    assert!(
        control_fringe > 0.05,
        "control failed to fail: the pre-fix Linear filter, sampling the exact same real \
         cloud texture, should produce a measurable band of partial-coverage (neither sky \
         nor full cloud colour) pixels at cell boundaries (that IS the reported bug) — only \
         {:.1}% fringe, so this gate's detector would not actually have caught the \
         regression",
        control_fringe * 100.0
    );
}

/// **Anti-island proof for FANCY clouds.** `cloud_mesh.rs` landed
/// as `dc8a028` with 11 hermetic tests and zero consumers, disclosed as
/// unwired at the time. `sky.rs`'s `fancy_cloud_geometry`/`cloud_face_vertices`
/// and this crate's own unit tests prove the *math*, but every one of them is
/// GPU-free and cannot see a wrong bind group, an untouched vertex layout, or
/// a draw call that never runs — exactly the class of defect `CLAUDE.md`'s
/// rule 1 exists to catch. This is the gate that proves the wiring reaches
/// the framebuffer.
///
/// Camera sits at `CLOUD_HEIGHT` itself, looking straight up
/// (`Camera::forward` at `yaw = 0, pitch = -90` is `(0, 1, 0)`, same as
/// `real_jar_sun_is_not_solid_black`'s doc). The mesh always builds the
/// interior 3x3 cells around the camera's own cell with every face flagged
/// `FLAG_INSIDE_FACE` when any of those nine cells is filled
/// (`cloud_mesh::push_extruded_cell`), so this only needs the real
/// `clouds.png` to have *a* filled cell near the origin — true of the shipped
/// asset without hunting for a specific coordinate (most of the 256x256
/// texture is filled; a fully-transparent origin would make this gate flaky,
/// which the executed control below would catch by both readings coming back
/// near-zero).
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn fancy_clouds_paint_real_pixels_near_the_camera() {
    let Some(ctx) = ctx() else {
        panic!(
            "no GPU adapter available — a missing GPU is a failure, never a skip \
             (this repo's own convention, see e.g. dropped_item_pixels.rs)"
        );
    };
    let manager = real_jar_manager().expect(
        "no client.jar under .cache/mc/<version>/ — fetch it first; a missing jar is a \
         failure, never a skip",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    const W: u32 = 128;
    const H: u32 = 128;

    let camera = Camera {
        position: glam::Vec3::new(0.0, lodestone_render::CLOUD_HEIGHT, 0.0),
        yaw: 0.0,
        pitch: -90.0,
        fov_y_degrees: 90.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 512.0,
    };

    let sky =
        SkyRenderer::new(device, queue, format, &manager).expect("build sky renderer over the real jar");

    // ---- Subject: FANCY, explicit (not relying on the default, so this test
    // keeps failing if that default is ever reverted). ----
    let mut subject_target = HeadlessTarget::new(device, W, H, format);
    let subject_frame = subject_target.acquire().expect("headless acquire (subject)");
    let mut subject_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("fancy-clouds-subject"),
    });
    sky.render(
        device,
        queue,
        &mut subject_encoder,
        subject_frame.view(),
        &camera,
        &lodestone_render::SkyFrame::new(6_000, DAY_SKY).with_cloud_status(lodestone_render::CloudStatus::Fancy),
        wgpu::Color::BLACK,
    );
    queue.submit(std::iter::once(subject_encoder.finish()));
    let subject_pixels = subject_target.read_texels(device, queue);
    let subject_frac = non_black_fraction(&subject_pixels);

    // ---- Control, EXECUTED: same camera and scene, FAST clouds. Not a
    // FAST-vs-FANCY comparison — it proves this camera/texture combination is
    // *capable* of painting non-black pixels at all, so a subject failure
    // cannot be laid at the scene's door instead of FANCY's wiring.
    let mut control_target = HeadlessTarget::new(device, W, H, format);
    let control_frame = control_target.acquire().expect("headless acquire (control)");
    let mut control_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("fancy-clouds-control"),
    });
    sky.render(
        device,
        queue,
        &mut control_encoder,
        control_frame.view(),
        &camera,
        &lodestone_render::SkyFrame::new(6_000, DAY_SKY).with_cloud_status(lodestone_render::CloudStatus::Fast),
        wgpu::Color::BLACK,
    );
    queue.submit(std::iter::once(control_encoder.finish()));
    let control_pixels = control_target.read_texels(device, queue);
    let control_frac = non_black_fraction(&control_pixels);

    eprintln!("=== fancy cloud anti-island gate ===");
    eprintln!("subject (FANCY, looking straight up from inside the layer): {:.1}% non-black", subject_frac * 100.0);
    eprintln!("control (FAST, same camera): {:.1}% non-black", control_frac * 100.0);

    assert!(
        subject_frac > 0.05,
        "FANCY clouds painted essentially nothing ({:.1}%) looking straight up from inside \
         the real cloud layer — the mesh, pipeline, bind group, or draw call is not reaching \
         the framebuffer",
        subject_frac * 100.0
    );
    assert!(
        control_frac > 0.05,
        "control failed to fail: FAST painted nothing either ({:.1}%) from the same camera \
         and real texture, so this scene cannot distinguish a broken FANCY draw from an \
         empty one — the camera/texture setup needs to change, not the assertion",
        control_frac * 100.0
    );
}

// ---------------------------------------------------------------------------
// `SkyMode::None` — the Nether draws no sky geometry, only the clear.
// ---------------------------------------------------------------------------

/// Renders one frame through `SkyRenderer::render` at `mode` and returns the
/// RGBA8 readback.
///
/// One helper so the two arms below cannot differ in anything except the mode:
/// same camera, same clock, same clear, same textures. A per-arm setup is how a
/// paired gate ends up comparing two scenes rather than two modes.
fn render_sky_at_mode(
    ctx: &GpuContext,
    sky: &SkyRenderer,
    mode: lodestone_render::SkyMode,
    clear: wgpu::Color,
) -> Vec<u8> {
    let mut target =
        HeadlessTarget::new(ctx.device(), WIDTH, HEIGHT, wgpu::TextureFormat::Rgba8UnormSrgb);
    let frame = target.acquire().expect("acquire");
    let camera = Camera {
        position: glam::Vec3::new(0.0, 70.0, 0.0),
        yaw: 0.0,
        // Steeply up, for the reason `sky_pass_paints_the_whole_frame` records:
        // the disc is a finite overhead plane, so a level camera paints only the
        // upper half and the lower half is where terrain would go.
        pitch: -60.0,
        fov_y_degrees: 90.0,
        aspect: WIDTH as f32 / HEIGHT as f32,
        near: 0.05,
        far: 1024.0,
    };
    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sky-mode-gate-encoder"),
        });
    sky.render(
        ctx.device(),
        ctx.queue(),
        &mut encoder,
        frame.view(),
        &camera,
        // Midnight, so the star and moon draws are live too — a gate run at noon
        // would leave `star_brightness == 0` and could not tell a suppressed star
        // pass from an inactive one.
        &lodestone_render::SkyFrame::new(18_000, [0.24, 0.46, 0.83])
            .with_cloud_status(lodestone_render::CloudStatus::Fast)
            .with_sky_mode(mode),
        clear,
    );
    ctx.queue().submit(std::iter::once(encoder.finish()));
    target.read_texels(ctx.device(), ctx.queue())
}

/// The count of pixels differing from the frame's own top-left pixel, and their
/// bounding box as `(min_x, min_y, max_x, max_y)`.
///
/// A **bounding box, not a fraction**: a fraction cannot tell a uniform-but-wrong
/// frame from a localised blob, and the failure message has to be able to say
/// *where* the geometry that should not be there landed.
fn differing_from_corner(pixels: &[u8]) -> (usize, Option<(u32, u32, u32, u32)>) {
    let reference = &pixels[0..4];
    let mut count = 0usize;
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let i = ((y * WIDTH + x) * 4) as usize;
            let px = &pixels[i..i + 4];
            // A tolerance of 2/255 per channel: the readback goes through an
            // `Rgba8UnormSrgb` target, so a byte of rounding is expected and is
            // not geometry.
            let differs = (0..4).any(|c| px[c].abs_diff(reference[c]) > 2);
            if differs {
                count += 1;
                bbox = Some(match bbox {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    (count, bbox)
}

/// **The Nether gate.** With `SkyMode::None`, the frame must be exactly the
/// clear colour everywhere — no disc, no band, no sun, no moon, no stars, no
/// clouds — while the *same* frame at `SkyMode::Overworld` must not be, and the
/// clear must still have happened.
///
/// # Three assertions, because uniformity alone is the trap
///
/// A full-screen quad painting over everything is *also* uniform, and that is
/// precisely the shape a coverage probe cannot see (a new full-screen element is
/// what a point- or vertex-sampled check certifies as "nothing paints here").
/// So the uniform value is checked to be the **red clear** and not the sky's
/// blue or the synthetic sun's white:
///
/// 1. the `None` frame differs from its own corner pixel in **zero** locations;
/// 2. that uniform value is red-dominant and not black — the clear ran, so this
///    is "clear and draw nothing", not "do nothing" (which would leave the
///    target black and would silently make the block pass `Load` garbage);
/// 3. the `Overworld` frame *does* differ, with a printed bounding box — the
///    control, without which arm 1's zero would be measuring an empty scene
///    rather than a suppressed one.
#[test]
#[ignore = "requires a GPU adapter"]
fn the_nether_paints_only_the_clear_while_the_overworld_paints_geometry() {
    let Some(ctx) = ctx() else { return };
    let sky = SkyRenderer::new(
        ctx.device(),
        ctx.queue(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        &manager(),
    )
    .expect("build sky renderer over the synthetic pack");

    // The Nether's own fog hue, and deliberately not black or grey: a
    // red-dominant clear is what lets arm 2 tell the clear apart from the disc
    // (blue), the synthetic sun/cloud (white) and an untouched target (black).
    let clear = wgpu::Color {
        r: 0.20,
        g: 0.02,
        b: 0.02,
        a: 1.0,
    };

    let nether = render_sky_at_mode(&ctx, &sky, lodestone_render::SkyMode::None, clear);
    let overworld = render_sky_at_mode(&ctx, &sky, lodestone_render::SkyMode::Overworld, clear);

    let (nether_diff, nether_bbox) = differing_from_corner(&nether);
    let (overworld_diff, overworld_bbox) = differing_from_corner(&overworld);
    let corner = [nether[0], nether[1], nether[2], nether[3]];

    eprintln!("=== SkyMode::None gate ===");
    eprintln!("nether: {nether_diff} px differ from the corner, bbox {nether_bbox:?}");
    eprintln!("nether corner rgba: {corner:?}");
    eprintln!("overworld (control): {overworld_diff} px differ, bbox {overworld_bbox:?}");

    assert_eq!(
        nether_diff, 0,
        "SkyMode::None painted geometry: {nether_diff} px differ from the clear, \
         bounding box {nether_bbox:?} in a {WIDTH}x{HEIGHT} frame"
    );
    assert!(
        corner[0] > corner[1] && corner[0] > corner[2] && corner[0] > 20,
        "the uniform frame is not the red clear ({corner:?}) — either the clear \
         never ran (an untouched target reads black, and the block pass would then \
         Load garbage) or a full-screen quad painted over it, which is exactly the \
         case a uniformity check alone cannot see"
    );
    assert!(
        overworld_diff > 0,
        "control failed to fail: SkyMode::Overworld painted nothing over the same \
         clear from the same camera at the same clock, so this scene cannot tell a \
         suppressed sky from an empty one — the camera/time setup needs changing, \
         not the assertion"
    );
}
