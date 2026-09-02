//! Prove that the enchantment-glint pass reaches pixels, composites with
//! vanilla's own blend function, and is confined to the item it shimmers.
//!
//! ## What the glint is
//!
//! A **second pass over the item's own geometry** — not a texture swap, not a flat
//! overlay quad. Vanilla re-emits every baked quad into a glint buffer
//! (`ItemFeatureRenderer.java:74-84`) and draws it with `RenderPipelines.GLINT`,
//! whose depth state is `CompareOp.EQUAL` with **zero** bias
//! (`RenderPipelines.java:431`). That only works if the two passes rasterise
//! byte-identical clip positions, which is why `glint.wgsl` recomputes `clip` the
//! same way `model.wgsl` does and why the glint pipeline consumes `ModelVertex`'s
//! own vertex layout so it can be handed the *same* vertex buffer.
//!
//! ## The measurement, and why it predicts a value rather than a direction
//!
//! "The item got brighter" is satisfied by any additive pass of any strength, and
//! is the *magnitude* species of vacuous test — the one that shipped a hurt overlay
//! here at 70% red where vanilla renders 30%. So this gate predicts the exact
//! composited byte, and it predicts it three ways so the measurement has to choose:
//!
//! | hypothesis | jar | linear-space result |
//! |---|---|---|
//! | `GLINT` (correct) | `BlendFunction.java:8` — `SRC_COLOR, ONE` | `dst + src²` |
//! | `ADDITIVE` | `BlendFunction.java:17` — `ONE, ONE` | `dst + src` |
//! | `TRANSLUCENT` | `BlendFunction.java:10-12` — `SRC_ALPHA, ONE_MINUS_SRC_ALPHA` | `src` (at α=1) |
//!
//! `SRC_COLOR` is the source **squared**, and it is neither of the two obvious
//! guesses. At the synthetic glint value used here the three predictions are tens
//! of `1/255` apart, far outside quantisation.
//!
//! ## Why a synthetic glint texture for the exact-byte test
//!
//! Predicting a byte requires knowing the source value, and with the real
//! `enchanted_glint_item.png` that means replicating the scroll matrix, the
//! rotation, the `REPEAT` wrap and the bilinear filter per pixel — i.e. predicting
//! the code under test with a second copy of the code under test, which is the
//! `decode(encode(x))` trap. A **uniform** synthetic texture makes the source
//! value a known constant, so the prediction comes from the jar's *blend* and
//! *strength* constants alone and nothing else has to be modelled.
//!
//! That is not a substitute for the real asset:
//! [`the_real_jar_glint_texture_produces_a_varying_pattern`] loads the actual PNG
//! out of `client.jar`, asserts its jar-verified 128x128 dimensions, and requires
//! the composited silhouette to **vary** across itself — which is the one property
//! a uniform texture structurally cannot show and a flat wash would fail.
//!
//! ## No `ALPHA_BLENDING`, so no unpredictable byte
//!
//! The measured warning elsewhere in this repo is that the *effective blend alpha*
//! through `ALPHA_BLENDING` on this Metal backend is a real, repeatable,
//! non-trivial function of the raw fragment alpha. It does not apply here, and not
//! by luck: `BlendFunction.GLINT`'s colour equation is `SRC_COLOR/ONE` and its
//! alpha equation is `ZERO/ONE`, so **no alpha enters the colour blend at all** and
//! the destination alpha is never touched. Every pixel compared here is a
//! fully-opaque interior fragment of the item's slab.
//!
//! ## Controls, run rather than described
//!
//! * [`suppressing_the_glint_pass_leaves_the_frame_byte_identical`] renders with
//!   the glint pass omitted and asserts the frame equals the item-only frame
//!   exactly. If it did not, the "the glint changed these pixels" measurement
//!   would be attributing someone else's drawing to the glint.
//! * [`the_glint_is_confined_to_the_items_own_silhouette`] asserts every pixel the
//!   item did *not* write is byte-identical across the two frames — which is the
//!   real test of depth-`EQUAL`, and would fail if the glint drew a full-screen
//!   quad or if the depth compare were ported with a flipped sense.
//! * Failure output prints a **bounding box**, never a bare fraction: a percentage
//!   cannot tell a uniform-but-wrong frame from a localised blob.
//!
//! `#[ignore]`d and **fail-closed**: needs a GPU adapter and a fetched
//! `client.jar`. Run with
//! `cargo test -p lodestone-render --test glint_pixels -- --ignored --nocapture`.

use lodestone_assets::{Image, ResourceManager, ResourceSource, ZipSource};
use lodestone_render::entity::{dropped_item_mesh, ground_transform_for};
use glam::Vec4Swizzles as _;
use lodestone_render::glint::{
    self, DEFAULT_SPEED, DEFAULT_STRENGTH, GlintPipeline, GlintUniform, Scale,
};
use lodestone_render::{
    BlockModels, Camera, GpuAtlas, GpuModelMesh, ItemGeometry, ModelMesh, ModelPipeline,
    blocks_json_registry, model_anim_buffer, model_palette_buffer, model_shared_camera_buffer,
    section_origin_buffer,
};
use wgpu::util::DeviceExt;

#[path = "../gate_harness/mod.rs"]
mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

const W: u32 = 256;
const H: u32 = 256;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// The subject: a flat-sprite item, i.e. the overwhelmingly common form and the
/// one a player sees glinting in a hotbar. `diamond_sword` is enchantable, which
/// makes it the honest choice over an item that could never carry the component.
const ITEM: &str = "minecraft:diamond_sword";

/// The real glint texture inside `client.jar`
/// (`ItemFeatureRenderer.java:23`).
const GLINT_PNG: &str = "assets/minecraft/textures/misc/enchanted_glint_item.png";

/// Jar-verified dimensions of both glint PNGs.
const GLINT_SIZE: u32 = 128;

/// The synthetic glint texel used for the exact-byte test. Mid-grey rather than
/// white: at white the `GLINT` and `ADDITIVE` hypotheses both saturate to 255 over
/// a bright destination and stop discriminating.
const SYNTHETIC_TEXEL: u8 = 128;

const DROP: glam::Vec3 = glam::Vec3::new(0.5, 0.0, 0.5);
const CAM_DISTANCE: f32 = 1.2;

const CLEAR: wgpu::Color = wgpu::Color {
    r: 1.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};

/// A fixed, non-zero time so the scroll offsets are both non-zero and the frame is
/// reproducible. 1234 ms of monotonic clock.
const MILLIS: f64 = 1_234.0;

/// Minimum silhouette pixels for a measurement rather than a coincidence.
const MIN_SILHOUETTE_PX: usize = 200;

/// The correct hypothesis must sit within this mean absolute error, in `0..=1`
/// units. One quantisation step is `1/255`; three steps covers rounding in both
/// renders plus the depth-EQUAL exactness.
const MAX_CORRECT_MAE: f32 = 3.0 / 255.0;

/// Each wrong hypothesis must sit at least this far out.
const MIN_WRONG_MAE: f32 = 12.0 / 255.0;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn setup() -> Gpu {
    let gpu = pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
                apply_limit_buckets: false,
            })
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("glint_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    });
    gpu.expect("glint_pixels: no GPU adapter, and this gate must not skip")
}

fn jar() -> ResourceManager {
    let path = require_client_jar();
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let zip = ZipSource::from_bytes(bytes).unwrap_or_else(|e| panic!("open jar: {e}"));
    ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>])
}

fn build_models() -> BlockModels {
    let path = require_client_jar();
    let report = require_blocks_report(&path);
    let source = ZipSource::open(&path).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    BlockModels::build(&manager, &registry).expect("bake block models")
}

fn geometry<'a>(models: &'a BlockModels, item: &str) -> &'a ItemGeometry {
    let id = item.parse().expect("valid resource location");
    models
        .item(&id)
        .unwrap_or_else(|| panic!("{item} has no baked geometry"))
}

fn drop_mesh(geometry: &ItemGeometry) -> ModelMesh {
    let ground = ground_transform_for(geometry.gui_light);
    dropped_item_mesh(
        &geometry.quads,
        geometry.gui_light,
        &ground,
        DROP,
        0.0,
        0.0,
        0xF0,
    )
}

fn camera(centre: glam::Vec3) -> Camera {
    Camera {
        position: glam::Vec3::new(DROP.x, centre.y, DROP.z + CAM_DISTANCE),
        yaw: 180.0,
        pitch: 0.0,
        aspect: 1.0,
        ..Camera::default()
    }
}

/// A uniform `value`-grey opaque glint texture. See the module doc for why the
/// exact-byte test needs a known constant source rather than the real PNG.
fn synthetic_glint(value: u8) -> Image {
    Image {
        width: GLINT_SIZE,
        height: GLINT_SIZE,
        rgba: (0..GLINT_SIZE * GLINT_SIZE)
            .flat_map(|_| [value, value, value, 255])
            .collect(),
    }
}

/// Upload an [`Image`] as a **non-sRGB** `Rgba8Unorm` texture.
///
/// Deliberately not `Rgba8UnormSrgb`: vanilla is not colour-managed and its
/// `texture(Sampler0, uv)` yields the raw byte over 255, with no transfer function
/// applied. Uploading the glint sheet as sRGB would silently linearise it and make
/// the shimmer darker than the game's, and — for this gate — would put an
/// unmodelled decode between the known texel and the predicted byte.
fn upload_glint(device: &wgpu::Device, queue: &wgpu::Queue, img: &Image) -> wgpu::TextureView {
    let size = wgpu::Extent3d {
        width: img.width,
        height: img.height,
        depth_or_array_layers: 1,
    };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glint sheet"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
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
        size,
    );
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Render the item, optionally followed by the glint pass over the **same** vertex
/// buffer, and read the frame back row-major.
///
/// `glint_sheet` of `None` omits the glint pass entirely — the control.
#[allow(clippy::too_many_lines)]
fn render(
    gpu: &Gpu,
    models: &BlockModels,
    mesh: &ModelMesh,
    cam: &Camera,
    glint_sheet: Option<&Image>,
) -> Vec<(u8, u8, u8)> {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    let atlas = GpuAtlas::from_atlas(device, queue, models.atlas());
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);
    let palette_buffer = model_palette_buffer(device, models.tint_palette());
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    let anim_buffer = model_anim_buffer(device, &models.anim_slot_uniforms(0));
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);
    let vp = cam.view_projection().to_cols_array_2d();
    let cam_buffer = model_shared_camera_buffer(device, vp);
    let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);

    // The glint pass. Built unconditionally so the *control* frame differs from
    // the test frame in exactly one respect — whether the draw is recorded — and
    // not also in which resources were created.
    let glint_pipeline = GlintPipeline::new(device, FORMAT, DEPTH_FORMAT);
    let glint_sampler = glint::glint_sampler(device);
    let glint_uniform = GlintUniform::new(
        vp,
        [0.0, 0.0, 0.0],
        MILLIS,
        DEFAULT_SPEED,
        DEFAULT_STRENGTH,
        Scale::Item,
        // The sheet these vertices carry UVs into. Not a constant: the stitched
        // model atlas's size follows the mip gutter, and the glint scale is
        // expressed in atlas-normalised units — see `glint::atlas_correction`.
        [models.atlas().width, models.atlas().height],
    );
    let glint_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("glint uniform"),
        contents: bytemuck::bytes_of(&glint_uniform),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let glint_uniform_bg = glint_pipeline.uniform_bind_group(device, &glint_buffer);
    let sheet = glint_sheet.map(|img| upload_glint(device, queue, img));
    let glint_tex_bg = sheet
        .as_ref()
        .map(|view| glint_pipeline.texture_bind_group(device, view, &glint_sampler));

    let size = wgpu::Extent3d {
        width: W,
        height: H,
        depth_or_array_layers: 1,
    };
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glint target"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

    let gpu_mesh = GpuModelMesh::upload(device, mesh).expect("the subject item has geometry");
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("glint pixels"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(lodestone_render::DEPTH_CLEAR),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        // Pass 1: the item itself.
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[0]);
        pass.set_bind_group(1, &atlas_bg, &[]);
        pass.set_bind_group(2, &palette_bg, &[]);
        pass.set_bind_group(3, &anim_bg, &[]);
        pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
        pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);

        // Pass 2: the glint, over the *same* vertex and index buffers. That
        // identity is what depth-EQUAL requires; binding a different buffer here
        // (or re-meshing) would z-fail the whole pass and draw nothing.
        if let Some(glint_tex_bg) = &glint_tex_bg {
            pass.set_pipeline(&glint_pipeline.pipeline);
            pass.set_bind_group(0, &glint_uniform_bg, &[]);
            pass.set_bind_group(1, glint_tex_bg, &[]);
            pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
            pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
        }
    }

    let padded = (W * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &color,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        size,
    );
    queue.submit(std::iter::once(enc.finish()));
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = readback.slice(..).get_mapped_range().expect("mapped range");

    let mut out = Vec::with_capacity((W * H) as usize);
    for y in 0..H {
        for x in 0..W {
            let i = (y * padded + x * 4) as usize;
            out.push((data[i], data[i + 1], data[i + 2]));
        }
    }
    out
}

fn clear_rgb() -> (u8, u8, u8) {
    (255, 0, 255)
}

fn lit(px: (u8, u8, u8)) -> bool {
    px != clear_rgb()
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn bbox(pixels: &[(u32, u32)]) -> (u32, u32, u32, u32) {
    let mut b = (u32::MAX, u32::MAX, 0, 0);
    for &(x, y) in pixels {
        b.0 = b.0.min(x);
        b.1 = b.1.min(y);
        b.2 = b.2.max(x);
        b.3 = b.3.max(y);
    }
    b
}

/// Every pixel the item wrote (lit in the item-only frame).
fn silhouette(item_only: &[(u8, u8, u8)]) -> Vec<usize> {
    item_only
        .iter()
        .enumerate()
        .filter(|(_, px)| lit(**px))
        .map(|(i, _)| i)
        .collect()
}

/// The load-bearing test: the composited byte lands on vanilla's `GLINT` blend and
/// far from both obvious alternatives.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn the_glint_pass_composites_with_the_src_color_blend() {
    let gpu = setup();
    let models = build_models();
    let geom = geometry(&models, ITEM);
    let mesh = drop_mesh(geom);
    let centre = mesh
        .vertices
        .iter()
        .fold(glam::Vec3::ZERO, |a, v| a + glam::Vec3::from(v.position))
        / mesh.vertices.len() as f32;
    let cam = camera(centre);

    let sheet = synthetic_glint(SYNTHETIC_TEXEL);
    let item_only = render(&gpu, &models, &mesh, &cam, None);
    let with_glint = render(&gpu, &models, &mesh, &cam, Some(&sheet));

    // The source value the shader emits, from the jar's constants only: the raw
    // texel (non-sRGB upload, so no transfer function) times `GlintAlpha`.
    let src = (f32::from(SYNTHETIC_TEXEL) / 255.0) * DEFAULT_STRENGTH;
    println!(
        "synthetic texel {SYNTHETIC_TEXEL} * GlintAlpha {DEFAULT_STRENGTH} => src {src:.5}; \
         src^2 {:.5}",
        src * src
    );

    let sil = silhouette(&item_only);
    let mut n = 0usize;
    let mut mae_glint = 0.0f32;
    let mut mae_additive = 0.0f32;
    let mut mae_translucent = 0.0f32;
    let mut compared = Vec::new();

    for &i in &sil {
        let d = item_only[i];
        let m = with_glint[i];
        // Skip pixels that saturated: at 255 every hypothesis that overshoots
        // clamps to the same byte and the comparison stops discriminating. This
        // is an exclusion for *validity*, not for flattery — it removes pixels
        // where the correct hypothesis is not uniquely identifiable.
        if m.0 >= 254 && m.1 >= 254 && m.2 >= 254 {
            continue;
        }
        for c in 0..3 {
            let dv = match c {
                0 => d.0,
                1 => d.1,
                _ => d.2,
            };
            let mv = match c {
                0 => m.0,
                1 => m.1,
                _ => m.2,
            };
            if mv >= 254 {
                continue;
            }
            let dst_lin = srgb_to_linear(f32::from(dv) / 255.0);
            let obs = f32::from(mv) / 255.0;
            // The blend runs in the target's linear space (Rgba8UnormSrgb decodes
            // on read and encodes on write), so each hypothesis is evaluated
            // there and then re-encoded to compare against the observed byte.
            let p_glint = linear_to_srgb((dst_lin + src * src).min(1.0));
            let p_additive = linear_to_srgb((dst_lin + src).min(1.0));
            let p_translucent = linear_to_srgb(src.min(1.0));
            mae_glint += (obs - p_glint).abs();
            mae_additive += (obs - p_additive).abs();
            mae_translucent += (obs - p_translucent).abs();
            n += 1;
        }
        compared.push(((i as u32) % W, (i as u32) / W));
    }

    assert!(n > 0, "no comparable channels; the item may not be drawing");
    let (mae_glint, mae_additive, mae_translucent) = (
        mae_glint / n as f32,
        mae_additive / n as f32,
        mae_translucent / n as f32,
    );
    println!(
        "silhouette={} compared_channels={n} bbox={:?}\n  GLINT(dst+src^2) mae={mae_glint:.5}\n  \
         ADDITIVE(dst+src)  mae={mae_additive:.5}\n  TRANSLUCENT(src)   mae={mae_translucent:.5}",
        sil.len(),
        bbox(&compared)
    );

    assert!(
        compared.len() >= MIN_SILHOUETTE_PX,
        "only {} comparable pixels (bbox {:?}) — too few to measure",
        compared.len(),
        bbox(&compared)
    );
    assert!(
        mae_glint <= MAX_CORRECT_MAE,
        "the GLINT blend prediction is {mae_glint:.5} from the measured bytes (limit \
         {MAX_CORRECT_MAE:.5}). ADDITIVE={mae_additive:.5} TRANSLUCENT={mae_translucent:.5}. \
         bbox {:?}",
        bbox(&compared)
    );
    assert!(
        mae_additive >= MIN_WRONG_MAE,
        "the ADDITIVE hypothesis is only {mae_additive:.5} away (floor {MIN_WRONG_MAE:.5}), so \
         this frame does not discriminate SRC_COLOR from ONE and the pass above proves nothing"
    );
    assert!(
        mae_translucent >= MIN_WRONG_MAE,
        "the TRANSLUCENT hypothesis is only {mae_translucent:.5} away (floor {MIN_WRONG_MAE:.5})"
    );
}

/// Depth-`EQUAL` confines the glint to the pixels the item itself wrote. A
/// full-screen wash, or a flipped depth compare, fails here.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn the_glint_is_confined_to_the_items_own_silhouette() {
    let gpu = setup();
    let models = build_models();
    let geom = geometry(&models, ITEM);
    let mesh = drop_mesh(geom);
    let centre = mesh
        .vertices
        .iter()
        .fold(glam::Vec3::ZERO, |a, v| a + glam::Vec3::from(v.position))
        / mesh.vertices.len() as f32;
    let cam = camera(centre);

    let sheet = synthetic_glint(SYNTHETIC_TEXEL);
    let item_only = render(&gpu, &models, &mesh, &cam, None);
    let with_glint = render(&gpu, &models, &mesh, &cam, Some(&sheet));

    let mut outside_changed = Vec::new();
    let mut inside_changed = Vec::new();
    for i in 0..item_only.len() {
        if item_only[i] == with_glint[i] {
            continue;
        }
        if lit(item_only[i]) {
            inside_changed.push(((i as u32) % W, (i as u32) / W));
        } else {
            outside_changed.push(((i as u32) % W, (i as u32) / W));
        }
    }

    println!(
        "inside changed={} bbox={:?}; outside changed={} bbox={:?}",
        inside_changed.len(),
        bbox(&inside_changed),
        outside_changed.len(),
        bbox(&outside_changed)
    );
    assert!(
        inside_changed.len() >= MIN_SILHOUETTE_PX,
        "the glint changed only {} pixels inside the item (bbox {:?}) — depth-EQUAL may be \
         z-failing the whole pass, which draws nothing and looks like 'no glint implemented'",
        inside_changed.len(),
        bbox(&inside_changed)
    );
    assert!(
        outside_changed.is_empty(),
        "the glint wrote {} pixels the item never touched, bbox {:?} — it is not confined to the \
         item's silhouette",
        outside_changed.len(),
        bbox(&outside_changed)
    );
}

/// The control: omit the glint draw and the frame must be **byte-identical** to
/// the item-only frame. Proves the two frames the other tests compare differ only
/// because of the glint pass.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn suppressing_the_glint_pass_leaves_the_frame_byte_identical() {
    let gpu = setup();
    let models = build_models();
    let geom = geometry(&models, ITEM);
    let mesh = drop_mesh(geom);
    let centre = mesh
        .vertices
        .iter()
        .fold(glam::Vec3::ZERO, |a, v| a + glam::Vec3::from(v.position))
        / mesh.vertices.len() as f32;
    let cam = camera(centre);

    let a = render(&gpu, &models, &mesh, &cam, None);
    let b = render(&gpu, &models, &mesh, &cam, None);
    let differing: Vec<(u32, u32)> = a
        .iter()
        .zip(b.iter())
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(i, _)| ((i as u32) % W, (i as u32) / W))
        .collect();
    println!("two glint-less frames differ in {} pixels", differing.len());
    assert!(
        differing.is_empty(),
        "two identical glint-less renders differ in {} pixels, bbox {:?} — the renderer is not \
         deterministic and no frame-difference measurement here means anything",
        differing.len(),
        bbox(&differing)
    );

    // And the glint frame *does* differ, so the control is a control rather than
    // a statement that nothing ever changes.
    let sheet = synthetic_glint(SYNTHETIC_TEXEL);
    let glinted = render(&gpu, &models, &mesh, &cam, Some(&sheet));
    let changed = a.iter().zip(glinted.iter()).filter(|(x, y)| x != y).count();
    println!("adding the glint pass changes {changed} pixels");
    assert!(
        changed >= MIN_SILHOUETTE_PX,
        "adding the glint pass changed only {changed} pixels"
    );
}

/// The real jar asset: correct dimensions, the vanilla-derived number of glint
/// texels across the item, and a composited silhouette whose added light matches
/// a **prediction computed from the PNG itself**.
///
/// # Why this predicts a range instead of thresholding a spread
///
/// The property that matters is that the glint is a *pattern*, not a wash. An
/// earlier form of this test asserted `max - min > 0.02`, an undecorated round
/// number, and that predicate turned out to be measuring the wrong thing: the
/// spread of the added light depends on how large a window of the 128x128 sheet
/// the item happens to cover, which depends on the atlas packing, which is not a
/// property of the glint at all. It passed while the atlas was small, then failed
/// when the atlas grew a mip gutter — and it went on failing after the glint was
/// corrected back to vanilla's own coverage, because vanilla's window is smaller
/// than the one the threshold had been calibrated against.
///
/// So this predicts the delta range for **both** hypotheses, from outside the
/// renderer: the PNG's own bytes, bilinearly sampled over the glint-UV rect,
/// squared through `BlendFunction.GLINT` and scaled by `GlintAlpha`. The two
/// hypotheses are the atlas-corrected scale and the uncorrected one (vanilla's
/// `8.0` applied straight to our larger sheet's UVs, which is what shipped), and
/// the discriminating statistic is the **floor**: a smaller window sits inside
/// one lobe of the pattern and never reaches its dark side, so an uncorrected
/// glint has a floor several times higher than a corrected one even though both
/// have a similar peak. A "did the spread exceed a constant" test cannot see
/// that; a floor can.
///
/// The coverage assertion above it is independent of both: `Scale::Item`'s 8.0
/// over vanilla's own 2048-texel atlas puts `8.0 * 128 * 16 / 2048 = 8` glint
/// texels across a 16-px sprite, and the 10 degree rotation widens an axis-aligned
/// rect's bounding box by `cos 10 + sin 10`. Nothing in that derivation comes
/// from our renderer.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn the_real_jar_glint_texture_produces_a_varying_pattern() {
    let gpu = setup();
    let jar = jar();
    let png = jar
        .read(GLINT_PNG)
        .unwrap_or_else(|| panic!("{GLINT_PNG} missing from client.jar"));
    let sheet = Image::decode_png(&png).unwrap_or_else(|e| panic!("decode {GLINT_PNG}: {e}"));
    assert_eq!(
        (sheet.width, sheet.height),
        (GLINT_SIZE, GLINT_SIZE),
        "enchanted_glint_item.png is 128x128 in 26.2"
    );

    let models = build_models();
    let geom = geometry(&models, ITEM);
    let mesh = drop_mesh(geom);
    let centre = mesh
        .vertices
        .iter()
        .fold(glam::Vec3::ZERO, |a, v| a + glam::Vec3::from(v.position))
        / mesh.vertices.len() as f32;
    let cam = camera(centre);

    let atlas_px = [models.atlas().width, models.atlas().height];
    let (uv0, uv1) = mesh_uv_rect(&mesh);
    println!(
        "atlas {}x{}, sprite uv rect [{:.6},{:.6}]x[{:.6},{:.6}] = {:.2}x{:.2} atlas px",
        atlas_px[0],
        atlas_px[1],
        uv0.x,
        uv1.x,
        uv0.y,
        uv1.y,
        (uv1.x - uv0.x) * atlas_px[0] as f32,
        (uv1.y - uv0.y) * atlas_px[1] as f32
    );
    assert!(
        ((uv1.x - uv0.x) * atlas_px[0] as f32 - SPRITE_PX).abs() < 0.5,
        "the subject is meant to be a plain {SPRITE_PX}-px sprite; if it is not, every derived \
         glint-texel count below is about a different item"
    );

    // --- The coverage claim, derived entirely outside this renderer. ---
    let matrix = glint::glint_texture_matrix(MILLIS, DEFAULT_SPEED, Scale::Item, atlas_px);
    let (g0, g1) = glint_uv_rect(&mesh, matrix);
    let measured_texels = (g1.x - g0.x) * f32::from(GLINT_SIZE as u16);
    let rot = ROTATION_BBOX_WIDENING;
    let want_texels = VANILLA_SPRITE_GLINT_TEXELS * rot;
    println!(
        "glint window {:.3} x {:.3} texels; vanilla's {VANILLA_SPRITE_GLINT_TEXELS} widened by \
         the 10 degree rotation is {want_texels:.3}",
        measured_texels,
        (g1.y - g0.y) * f32::from(GLINT_SIZE as u16)
    );
    assert!(
        (measured_texels - want_texels).abs() < 0.1,
        "a {SPRITE_PX}-px sprite must receive vanilla's {VANILLA_SPRITE_GLINT_TEXELS} glint \
         texels ({want_texels:.3} once the 10 degree rotation widens the bounding box), got \
         {measured_texels:.3}. Our atlas is {}x{} against vanilla's {}, so this is the \
         atlas-relative correction in `glint::atlas_correction`; without it the count is \
         {VANILLA_SPRITE_GLINT_TEXELS} * {}/{} of that.",
        atlas_px[0],
        atlas_px[1],
        glint::VANILLA_ATLAS_PX,
        glint::VANILLA_ATLAS_PX,
        atlas_px[0]
    );

    // --- Both predictions, from the PNG's own bytes. ---
    let correct = predict_delta_range(&sheet, &mesh, matrix);
    let shipped_bug = predict_delta_range(
        &sheet,
        &mesh,
        // The uncorrected matrix: vanilla's 8.0 straight onto our sheet's UVs,
        // reproduced by telling the correction our atlas *is* vanilla's.
        glint::glint_texture_matrix(
            MILLIS,
            DEFAULT_SPEED,
            Scale::Item,
            [glint::VANILLA_ATLAS_PX as u32, glint::VANILLA_ATLAS_PX as u32],
        ),
    );
    println!(
        "predicted from the PNG: corrected [{:.5}, {:.5}], uncorrected [{:.5}, {:.5}]",
        correct.0, correct.1, shipped_bug.0, shipped_bug.1
    );
    // Executed control: an input on which the two hypotheses coincide would make
    // every assertion below vacuous, and the floor is the statistic they are
    // asserted on.
    assert!(
        shipped_bug.0 > correct.0 * 2.0,
        "the two hypotheses predict floors {:.5} and {:.5}, which are too close to tell apart — \
         this item/time is not a discriminating input and the assertions below prove nothing",
        correct.0,
        shipped_bug.0
    );

    let item_only = render(&gpu, &models, &mesh, &cam, None);
    let with_glint = render(&gpu, &models, &mesh, &cam, Some(&sheet));

    // Per-pixel added brightness, in linear light, over the silhouette.
    let mut deltas = Vec::new();
    let mut pixels = Vec::new();
    for &i in &silhouette(&item_only) {
        let d = srgb_to_linear(f32::from(item_only[i].1) / 255.0);
        let m = srgb_to_linear(f32::from(with_glint[i].1) / 255.0);
        deltas.push(m - d);
        pixels.push(((i as u32) % W, (i as u32) / W));
    }
    assert!(
        deltas.len() >= MIN_SILHOUETTE_PX,
        "silhouette is only {} pixels, bbox {:?}",
        deltas.len(),
        bbox(&pixels)
    );

    let mean = deltas.iter().sum::<f32>() / deltas.len() as f32;
    let var = deltas.iter().map(|d| (d - mean).powi(2)).sum::<f32>() / deltas.len() as f32;
    let (min, max) = deltas.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &d| {
        (lo.min(d), hi.max(d))
    });
    println!(
        "real glint over {} px, bbox {:?}: mean delta {mean:.5}, sd {:.5}, range [{min:.5}, \
         {max:.5}]",
        deltas.len(),
        bbox(&pixels),
        var.sqrt()
    );

    // The peak: predicted from the PNG, not thresholded. The silhouette samples a
    // subset of the quad's UV rect, so it can fall a little short of the rect's
    // own peak but must not exceed it.
    assert!(
        max <= correct.1 + PREDICTION_TOLERANCE && max >= correct.1 - PREDICTION_TOLERANCE,
        "the composited peak is {max:.5}, and the PNG over this glint-UV rect predicts \
         {:.5} (bbox {:?})",
        correct.1,
        bbox(&pixels)
    );
    // The floor, which is what separates a pattern from a wash: a window that
    // fits inside one bright lobe never darkens, and that is exactly what the
    // uncorrected scale produces.
    assert!(
        (min - correct.0).abs() < (min - shipped_bug.0).abs(),
        "the composited floor is {min:.5}, nearer the UNCORRECTED prediction {:.5} than the \
         corrected one {:.5} (bbox {:?}) — the glint is sitting inside one lobe of the pattern \
         and never reaching its dark side, i.e. a wash rather than a shimmer. Check \
         `glint::atlas_correction` and the atlas dimensions handed to \
         `glint_texture_matrix`.",
        shipped_bug.0,
        correct.0,
        bbox(&pixels)
    );
}

/// A 16x16 item sprite, the size every assertion about glint texels is derived
/// against. Asserted rather than assumed.
const SPRITE_PX: f32 = 16.0;

/// `Scale::Item.factor() * GLINT_SIZE * SPRITE_PX / VANILLA_ATLAS_PX`, i.e. how
/// many glint texels vanilla lands across one item sprite: `8 * 128 * 16 / 2048`.
/// Written out rather than computed so the arithmetic is visible.
const VANILLA_SPRITE_GLINT_TEXELS: f32 = 8.0;

/// `cos 10 + sin 10`: how much the glint matrix's 10 degree rotation widens an
/// axis-aligned UV rect's axis-aligned bounding box.
const ROTATION_BBOX_WIDENING: f32 = 1.157_691;

/// Peak-prediction tolerance, in linear light. One 8-bit step near the item's own
/// brightness is worth well under this; the slack is for the silhouette sampling
/// a subset of the quad's UV rect rather than all of it.
const PREDICTION_TOLERANCE: f32 = 0.003;

/// The mesh's atlas-UV bounding rect.
fn mesh_uv_rect(mesh: &ModelMesh) -> (glam::Vec2, glam::Vec2) {
    mesh.vertices.iter().fold(
        (glam::Vec2::splat(f32::MAX), glam::Vec2::splat(f32::MIN)),
        |(lo, hi), v| {
            let uv = glam::Vec2::from(v.uv);
            (lo.min(uv), hi.max(uv))
        },
    )
}

/// The same rect after the glint texture matrix.
fn glint_uv_rect(mesh: &ModelMesh, matrix: glam::Mat4) -> (glam::Vec2, glam::Vec2) {
    mesh.vertices.iter().fold(
        (glam::Vec2::splat(f32::MAX), glam::Vec2::splat(f32::MIN)),
        |(lo, hi), v| {
            let t = matrix * glam::Vec4::new(v.uv[0], v.uv[1], 0.0, 1.0);
            (lo.min(t.xy()), hi.max(t.xy()))
        },
    )
}

/// Bilinear `REPEAT` sample of the glint sheet's green channel, matching the
/// sampler the pass binds. Green because the sheet is grey and the gate reads the
/// framebuffer's green channel.
fn sample_sheet(sheet: &Image, u: f32, v: f32) -> f32 {
    let w = sheet.width as i32;
    let h = sheet.height as i32;
    let fx = u * sheet.width as f32 - 0.5;
    let fy = v * sheet.height as f32 - 0.5;
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let at = |xi: i32, yi: i32| -> f32 {
        let x = xi.rem_euclid(w) as usize;
        let y = yi.rem_euclid(h) as usize;
        f32::from(sheet.rgba[(y * sheet.width as usize + x) * 4 + 1]) / 255.0
    };
    let (xi, yi) = (x0 as i32, y0 as i32);
    let a = at(xi, yi) * (1.0 - tx) + at(xi + 1, yi) * tx;
    let b = at(xi, yi + 1) * (1.0 - tx) + at(xi + 1, yi + 1) * tx;
    a * (1.0 - ty) + b * ty
}

/// `(min, max)` of the linear light `BlendFunction.GLINT` would add over the
/// mesh's UV rect under `matrix`, computed from the sheet's own bytes.
///
/// `GLINT` is `SRC_COLOR, ONE`, so the added light is the source **squared**, and
/// the source is the raw texel (the sheet is uploaded `Rgba8Unorm`, uncorrected,
/// exactly as vanilla samples it) times `GlintAlpha`.
fn predict_delta_range(sheet: &Image, mesh: &ModelMesh, matrix: glam::Mat4) -> (f32, f32) {
    let (uv0, uv1) = mesh_uv_rect(mesh);
    const N: u32 = 256;
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for i in 0..=N {
        for j in 0..=N {
            let u = uv0.x + (uv1.x - uv0.x) * (i as f32 / N as f32);
            let v = uv0.y + (uv1.y - uv0.y) * (j as f32 / N as f32);
            let t = matrix * glam::Vec4::new(u, v, 0.0, 1.0);
            let g = sample_sheet(sheet, t.x.rem_euclid(1.0), t.y.rem_euclid(1.0));
            let src = g * DEFAULT_STRENGTH;
            let d = src * src;
            lo = lo.min(d);
            hi = hi.max(d);
        }
    }
    (lo, hi)
}
