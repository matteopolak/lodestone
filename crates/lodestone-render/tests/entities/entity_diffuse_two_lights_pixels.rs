//! Offscreen gate for issue #383's **mechanism 1**: the entity/arm diffuse term.
//!
//! # The defect this exists to catch
//!
//! The entity shader used to light every surface from **one** direction with an
//! `abs()`:
//!
//! ```text
//! let light_dir = normalize(vec3<f32>(0.3, 1.0, 0.55));
//! let diffuse = 0.4 + 0.6 * clamp(abs(dot(n, light_dir)), 0.0, 1.0);
//! ```
//!
//! Vanilla sums **two** non-negative contributions instead
//! (`assets/minecraft/shaders/include/light.glsl` in the 26.2 client jar):
//!
//! ```text
//! lightValue = max(vec2(0.0), vec2(dot(L0, n), dot(L1, n)));
//! lightAccum = min(1.0, (lightValue.x + lightValue.y) * 0.6 + 0.4);
//! ```
//!
//! with `L0 = normalize(0.2, 1.0, -0.7)` and `L1 = normalize(-0.2, 1.0, 0.7)`
//! (`com.mojang.blaze3d.platform.Lighting.DIFFUSE_LIGHT_0/1`, selected for the
//! world by `Lighting.updateLevel(DEFAULT)`). Both constants and both vectors are
//! read out of the jar, not out of this repo — see [`vanilla_diffuse`].
//!
//! # Why an ordering assertion would be vacuous here
//!
//! "The sides are darker than the top" is true of *both* formulas, so a gate that
//! only checks ordering passes on the broken shader — the **magnitude** species of
//! vacuous test that shipped a 70%-red hurt overlay where vanilla renders 30%.
//! So every assertion here predicts an absolute byte from constants that
//! originate outside the shader, and the two hypotheses are far apart on every
//! cluster:
//!
//! | surface normal | vanilla | one abs light | byte (vanilla / abs) |
//! |---|---|---|---|
//! | `+Y` up        | 1.0000 | 0.9085 | 128 / 116 |
//! | `-Y` down      | 0.4000 | 0.9085 |  51 / 116 |
//! | `±Z` north/south | 0.7396 | 0.6796 |  95 /  87 |
//! | `±X` east/west | 0.4970 | 0.5525 |  64 /  71 |
//!
//! The signature worth naming: the old formula **cannot tell up from down** (both
//! `0.9085`), because `abs()` lights a face pointing away from the light exactly
//! as brightly as one pointing into it. Vanilla's spread there is 128 → 51.
//!
//! # Measured by cluster, never by frame average
//!
//! Every reading is a *population*: the frame is bucketed by displayed byte, and
//! each bucket is reported with its pixel count and **bounding box**. A frame
//! average cannot tell a uniformly-wrong frame from one wrong patch, and a
//! percentage cannot say *where*. The sheet is flat mid-grey and the texel is the
//! same for every surface, so a difference between buckets can only be shading.
//!
//! # What else paints here
//!
//! Nothing. These passes are built in this file rather than through
//! `RenderState`, so the first-person arm — which the shell draws in *every*
//! frame whenever `third_person_body_drawn` is false, and which has already
//! polluted two other gates' controls — is present only in
//! [`the_first_person_arm_matches_vanillas_two_light_diffuse`], where it is the
//! subject. Each gate asserts a coverage floor so an empty frame cannot pass.
//!
//! `#[ignore]`d: needs a real GPU adapter, and once opted in a missing adapter is
//! a failure, never a skip.

use std::collections::BTreeMap;

use glam::{Mat3, Vec3};
use lodestone_render::block::{CameraUniform, DepthBuffer};
use lodestone_render::camera::Camera;
use lodestone_render::entity::{
    Arm, EntityInstance, EntityMesh, first_person_arm_parts, first_person_arm_pose,
    hand_projection, plan_entities,
};
use lodestone_render::entity_anim::AnimInput;
use lodestone_render::entity_pipeline::{
    EntityCameraUniform, EntityPipeline, GpuEntityModel, entity_camera_buffer, upload_instances,
};
use lodestone_render::fog::FogUniform;

const W: u32 = 256;
const H: u32 = 256;

/// **sRGB, matching the real swapchain (`Bgra8UnormSrgb`).** The shade multiply
/// lands in *gamma* space, so a plain `Unorm` target would report different bytes
/// than the screen does and every prediction in this file would be wrong.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The one texel every surface here is painted with, in **sRGB bytes**. Chosen
/// mid-grey so `byte_for(d) = round(TEXEL * d)` and a bucket's byte reads as a
/// diffuse factor directly.
const TEXEL: u8 = 128;

/// `sky = 15, block = 0`. With the sky-darken lane at its unset sentinel (which
/// the shader reads as full daylight) the vertex shader's `light_term` is exactly
/// `1.0` — `get_brightness(1)` is `1` and `notGamma(1)` is `1`, so vanilla's curve
/// pins this endpoint just as the retired linear ramp did — and the displayed byte
/// isolates **diffuse**.
const LIGHT_FULL: u8 = 15 << 4;

/// Background. Nothing else in these frames is this colour, which is what
/// separates subject pixels from sky.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.40,
    g: 0.60,
    b: 0.95,
    a: 1.0,
};

/// How far a measured byte may sit from its prediction. One byte of transfer
/// rounding on each of two conversions, plus one for the `round`.
const TOL: i32 = 2;

/// The share of subject pixels allowed to miss every prediction. Face-boundary
/// pixels get a garbage `dpdx`/`dpdy` normal from a 2x2 quad that straddles two
/// faces — an unavoidable property of derivative normals, not of the formula —
/// and a box silhouette is mostly edge at this size.
const OUTLIER_BUDGET: f64 = 0.06;

// --------------------------------------------------------------- the oracle

/// Vanilla's diffuse factor for a surface normal, transcribed from the 26.2
/// client jar and **not** from this repo's shader:
///
/// * `assets/minecraft/shaders/include/light.glsl` —
///   `MINECRAFT_LIGHT_POWER 0.6`, `MINECRAFT_AMBIENT_LIGHT 0.4`,
///   `min(1.0, (max(0,d0) + max(0,d1)) * POWER + AMBIENT)`.
/// * `com.mojang.blaze3d.platform.Lighting` — `DIFFUSE_LIGHT_0 =
///   new Vector3f(0.2F, 1.0F, -0.7F).normalize()`, `DIFFUSE_LIGHT_1 =
///   new Vector3f(-0.2F, 1.0F, 0.7F).normalize()`, both written to the `LEVEL`
///   entry by `updateLevel(DEFAULT)`.
///
/// The world pass and the first-person hand pass both run under `LEVEL`:
/// `renderItemInHand` is called from inside `renderLevel`, and the only
/// `setupFor(ITEMS_3D)` in `GameRenderer` is after the level is finished, for the
/// GUI.
fn vanilla_diffuse(n: Vec3) -> f32 {
    let l0 = Vec3::new(0.2, 1.0, -0.7).normalize();
    let l1 = Vec3::new(-0.2, 1.0, 0.7).normalize();
    let n = n.normalize();
    (n.dot(l0).max(0.0) + n.dot(l1).max(0.0)).mul_add(0.6, 0.4).min(1.0)
}

/// The **suspected-wrong** hypothesis: one light and an `abs()`, exactly as the
/// shader read before this gate existed. Present so the gate can assert the
/// measurement lands on the right one of two predictions rather than merely
/// moving in the right direction.
fn one_abs_light_diffuse(n: Vec3) -> f32 {
    let dir = Vec3::new(0.3, 1.0, 0.55).normalize();
    0.6f32.mul_add(n.normalize().dot(dir).abs().clamp(0.0, 1.0), 0.4)
}

/// The byte a surface with diffuse `d` displays.
///
/// The texel is sampled from an sRGB texture (so it arrives linear), the shader
/// re-encodes it, multiplies, and decodes; the sRGB target then re-encodes on
/// write. The two round trips cancel, leaving `TEXEL * d` in gamma bytes.
fn byte_for(d: f32) -> i32 {
    (f32::from(TEXEL) * d).round() as i32
}

// ------------------------------------------------------------------ clusters

/// One population of equal-valued pixels: how many, and where.
struct Bucket {
    count: u32,
    x0: u32,
    x1: u32,
    y0: u32,
    y1: u32,
}

/// Bucket the subject's pixels by displayed red byte (the sheet is neutral, so
/// red is a faithful luma) and record each bucket's bounding box.
fn buckets(frame: &[u8]) -> BTreeMap<u8, Bucket> {
    let mut out: BTreeMap<u8, Bucket> = BTreeMap::new();
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if !is_subject(frame, i) {
                continue;
            }
            let e = out.entry(frame[i]).or_insert(Bucket {
                count: 0,
                x0: u32::MAX,
                x1: 0,
                y0: u32::MAX,
                y1: 0,
            });
            e.count += 1;
            e.x0 = e.x0.min(x);
            e.x1 = e.x1.max(x);
            e.y0 = e.y0.min(y);
            e.y1 = e.y1.max(y);
        }
    }
    out
}

/// The modal byte of the first row, scanning from the top (`from_top`) or the
/// bottom, that carries at least 20 subject pixels.
///
/// This is what binds a *value* to a *location*. Asserting only that the frame's
/// set of byte values matches vanilla's set is invariant under a **global normal
/// flip**: flipping the sign turns the up face's `128` into `51` and the
/// underside's `51` into `128`, so both values are still present and a set-wise
/// assertion still passes. That is not a hypothetical — it was measured here, and
/// the sign-flip control passed the set-wise gate before this existed.
///
/// The row floor of 20 skips the one or two rows of the silhouette's tip, where a
/// 2x2 derivative quad straddles a face boundary and the reconstructed normal is
/// meaningless.
fn modal_byte_at_edge_row(frame: &[u8], from_top: bool) -> (u32, u8) {
    let rows: Vec<u32> = if from_top {
        (0..H).collect()
    } else {
        (0..H).rev().collect()
    };
    for y in rows {
        let mut hist: BTreeMap<u8, u32> = BTreeMap::new();
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if is_subject(frame, i) {
                *hist.entry(frame[i]).or_default() += 1;
            }
        }
        let n: u32 = hist.values().sum();
        if n >= 20 {
            let byte = *hist
                .iter()
                .max_by_key(|(_, count)| **count)
                .expect("non-empty")
                .0;
            return (y, byte);
        }
    }
    panic!("no row carried 20 subject pixels — the frame is empty or a sliver");
}

/// Is this pixel part of the subject rather than the background?
///
/// The clear is specified in **linear** light and the target is `_srgb`, so what
/// lands in the readback is the *encoded* clear. Comparing against `CLEAR * 255`
/// classifies the whole frame as subject — the mistake two of the shell's entity
/// gates shipped.
fn is_subject(frame: &[u8], i: usize) -> bool {
    let encode = |c: f64| {
        let v = if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (v * 255.0).round() as u8
    };
    let clear = [encode(CLEAR.r), encode(CLEAR.g), encode(CLEAR.b)];
    frame[i..i + 3]
        .iter()
        .zip(clear)
        .any(|(got, want)| got.abs_diff(want) > 8)
}

/// Assert every bucket lands within [`TOL`] of one of `predicted`, and report the
/// full census either way.
///
/// `label` names the subject; `predicted` is `(name, diffuse)` per expected
/// surface normal. Failure prints the offending buckets' **bounding boxes**, so
/// the message says where the wrong pixels are rather than what fraction of them
/// there were.
fn assert_buckets_match(
    label: &str,
    frame: &[u8],
    predicted: &[(String, f32)],
    rival: &[(String, f32)],
    min_pixels: u32,
) {
    let buckets = buckets(frame);
    let total: u32 = buckets.values().map(|b| b.count).sum();
    assert!(
        total >= min_pixels,
        "{label}: only {total} subject pixels (floor {min_pixels}) — every reading \
         would be a sliver"
    );

    println!("--- {label}: {total} subject pixels ---");
    println!("  predicted (vanilla, two lights):");
    for (name, d) in predicted {
        println!("    {name:<28} d={d:.4}  byte {}", byte_for(*d));
    }
    println!("  rival hypothesis (one abs light):");
    for (name, d) in rival {
        println!("    {name:<28} d={d:.4}  byte {}", byte_for(*d));
    }

    let mut outliers = 0u32;
    let mut ox = (u32::MAX, 0u32);
    let mut oy = (u32::MAX, 0u32);
    for (byte, b) in &buckets {
        let hit = predicted
            .iter()
            .find(|(_, d)| (i32::from(*byte) - byte_for(*d)).abs() <= TOL);
        let rival_hit = rival
            .iter()
            .find(|(_, d)| (i32::from(*byte) - byte_for(*d)).abs() <= TOL);
        let verdict = match (hit, rival_hit) {
            (Some((name, _)), _) => format!("vanilla {name}"),
            (None, Some((name, _))) => format!("*** RIVAL {name} ***"),
            (None, None) => "*** unpredicted ***".to_string(),
        };
        println!(
            "  byte {byte:>3}  n={:<6} bbox x{}..{} y{}..{}  {verdict}",
            b.count, b.x0, b.x1, b.y0, b.y1
        );
        if hit.is_none() {
            outliers += b.count;
            ox = (ox.0.min(b.x0), ox.1.max(b.x1));
            oy = (oy.0.min(b.y0), oy.1.max(b.y1));
        }
    }

    let share = f64::from(outliers) / f64::from(total);
    assert!(
        share <= OUTLIER_BUDGET,
        "{label}: {outliers}/{total} pixels match no vanilla prediction \
         (bbox x{}..{} y{}..{}); budget is {:.0}% for face-boundary derivative \
         normals only",
        ox.0,
        ox.1,
        oy.0,
        oy.1,
        OUTLIER_BUDGET * 100.0
    );
}

// ------------------------------------------------------------- gpu plumbing

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn setup() -> Option<Gpu> {
    pollster::block_on(async {
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
                label: Some("entity_diffuse device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// A flat mid-grey sheet, sRGB so the sampled texel is linear-light exactly like
/// the real entity sheets. Fully opaque: the shader cutout-discards `a < 0.5`,
/// and a transparent texel would remove pixels this gate needs to count.
fn flat_sheet(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::TextureView, wgpu::Sampler) {
    const N: u32 = 16;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("diffuse-sheet"),
        size: wgpu::Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let rgba: Vec<u8> = [TEXEL, TEXEL, TEXEL, 255].repeat((N * N) as usize);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(N * 4),
            rows_per_image: Some(N),
        },
        wgpu::Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 1,
        },
    );
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("diffuse-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (
        texture.create_view(&wgpu::TextureViewDescriptor::default()),
        sampler,
    )
}

macro_rules! pass_desc {
    ($color:expr, $depth:expr) => {
        wgpu::RenderPassDescriptor {
            label: Some("entity-diffuse-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: $color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: $depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(lodestone_render::DEPTH_CLEAR),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }
    };
}

fn color_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("entity-diffuse-color"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = color.create_view(&wgpu::TextureViewDescriptor::default());
    (color, view)
}

fn readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mut enc: wgpu::CommandEncoder,
    color: &wgpu::Texture,
) -> Vec<u8> {
    let padded = (W * 4).next_multiple_of(256);
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("entity-diffuse-readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: color,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().expect("map readback");
    let data = slice.get_mapped_range().expect("mapped range");
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H as usize {
        let start = y * padded as usize;
        out.extend_from_slice(&data[start..start + (W * 4) as usize]);
    }
    drop(data);
    buf.unmap();
    out
}

// ------------------------------------------------------------------ subjects

/// A camera above and to one side of the origin, looking at `(0, 1, 0)`.
///
/// Derived from [`Camera::forward`] rather than guessed: `forward` is
/// `(-sin(yaw)cos(pitch), -sin(pitch), cos(yaw)cos(pitch))`, so `yaw = 45`,
/// `pitch = 35.264` is the unit vector `(-1, -1, 1)/sqrt(3)` and a camera at
/// `(0.8, 1.8, -0.8)` looks exactly at `(0, 1, 0)`. From there a box at the
/// origin shows its `+Y`, `+X` and `-Z` faces — three different diffuse
/// populations in one frame, which is the point.
fn three_face_camera() -> Camera {
    Camera {
        position: Vec3::new(0.8, 1.8, -0.8),
        yaw: 45.0,
        pitch: 35.264_39,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    }
}

/// The mirror of [`three_face_camera`] below the subject, looking *up* at
/// `(0, 1, 0)` from `(0.8, 0.2, -0.8)`. Shows the `-Y` face, which vanilla
/// renders at `0.4` and the single-`abs()` light renders at `0.9085` — the widest
/// disagreement between the two hypotheses anywhere on a box.
fn under_camera() -> Camera {
    Camera {
        position: Vec3::new(0.8, 0.2, -0.8),
        yaw: 45.0,
        pitch: -35.264_39,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    }
}

/// The `player_wide` ("Steve") rig the shell's hand pass actually draws.
fn player_mesh() -> EntityMesh {
    EntityMesh::from_named_model(
        "player_wide",
        &lodestone_assets::entity::player_model(false),
    )
}

/// The planned instance matrix for one named part of a mob standing at the
/// origin, taken from the **real** [`plan_entities`] output rather than rebuilt,
/// so the pose under test is the one the shell would upload.
fn planned_part_pose(mesh: &EntityMesh, name: &'static str, part: &str, camera: &Camera) -> (usize, glam::Mat4) {
    let index = mesh
        .skeleton
        .index_of(part)
        .unwrap_or_else(|| panic!("{name} has no part named {part}"));
    let inst = EntityInstance::new(name, mesh, Vec3::ZERO, 0.0, 1.0, &AnimInput::REST)
        .with_light(LIGHT_FULL);
    let planned = plan_entities(std::slice::from_ref(&inst), &camera.frustum());
    assert_eq!(planned.instance_count(), 1, "the mob must be on screen");
    let batch = planned.batches.first().expect("one batch");
    let mats = &batch.parts[index];
    assert_eq!(mats.len(), 1, "one instance of {part}");
    (index, mats[0])
}

/// Render exactly **one part** of `mesh`, posed by `pose`, under `view_proj`.
///
/// One part rather than the whole mob on purpose. A rig's other parts are not all
/// axis-aligned at rest — `player_wide`'s arms carry a ~5.7° rotation and a
/// zombie's are held out in front — so a whole-mob frame contains face normals
/// between the six axis predictions, and widening the prediction set to cover
/// them is how a magnitude gate turns into a gate that matches anything. Real
/// baked geometry either way: a hand-authored quad would let a winding or
/// normal-sign mistake pass unseen.
fn render_part(
    gpu: &Gpu,
    mesh: &EntityMesh,
    part_index: usize,
    pose: glam::Mat4,
    view_proj: glam::Mat4,
) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let pipeline = EntityPipeline::new(device, FORMAT);
    let (tex_view, sampler) = flat_sheet(device, queue);
    let cam_buf = entity_camera_buffer(
        device,
        EntityCameraUniform {
            camera: CameraUniform {
                view_proj: view_proj.to_cols_array_2d(),
                section_origin: [0.0; 4],
            },
            fog: FogUniform::disabled(),
        },
    );
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);
    let gpu_model = GpuEntityModel::upload(device, mesh).expect("mesh is non-empty");

    let range = gpu_model.parts[part_index];
    assert!(range.index_count > 0, "part {part_index} has no geometry");
    let buffer =
        upload_instances(device, &[pose], &[u32::from(LIGHT_FULL)]).expect("one instance");

    let (color, color_view) = color_target(device);
    let depth = DepthBuffer::new(device, W, H);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&pass_desc!(&color_view, &depth.view));
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[]);
        pass.set_bind_group(1, &tex_bg, &[]);
        pass.set_vertex_buffer(0, gpu_model.vertices.slice(..));
        pass.set_vertex_buffer(1, buffer.slice(..));
        pass.set_index_buffer(gpu_model.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(range.index_start..range.index_start + range.index_count, 0, 0..1);
    }
    readback(device, queue, enc, &color)
}

/// The six axis normals a box's faces carry, named.
fn axis_normals() -> Vec<(String, Vec3)> {
    vec![
        ("+Y up".to_string(), Vec3::Y),
        ("-Y down".to_string(), Vec3::NEG_Y),
        ("+X east".to_string(), Vec3::X),
        ("-X west".to_string(), Vec3::NEG_X),
        ("+Z south".to_string(), Vec3::Z),
        ("-Z north".to_string(), Vec3::NEG_Z),
    ]
}

// -------------------------------------------------------------------- gates

/// **A mob's every face must land on vanilla's two-light diffuse.**
///
/// Axis-aligned box geometry, so the whole predicted set is
/// `{1.0, 0.7396, 0.4970, 0.4}` — bytes `{128, 95, 64, 51}`. The rival
/// hypothesis predicts `{116, 87, 71}` and shares no value with it, so the
/// assertion cannot be satisfied by both.
#[test]
#[ignore = "requires a GPU adapter; run explicitly"]
fn a_mobs_faces_match_vanillas_two_light_diffuse() {
    let Some(gpu) = setup() else {
        panic!(
            "entity_diffuse_two_lights_pixels: no GPU adapter; this test is \
             #[ignore]d so a missing one is a failure"
        )
    };
    let mesh = player_mesh();
    let predicted: Vec<(String, f32)> = axis_normals()
        .into_iter()
        .map(|(n, v)| (n, vanilla_diffuse(v)))
        .collect();
    let rival: Vec<(String, f32)> = axis_normals()
        .into_iter()
        .map(|(n, v)| (n, one_abs_light_diffuse(v)))
        .collect();

    // Two cameras, because a box shows at most three faces at once and the up/down
    // pair is the whole magnitude question. `body` is the part: it is the rig's
    // one large axis-aligned box at rest.
    let mut brightest = 0i32;
    let mut dimmest = 255i32;
    for (label, camera) in [
        ("from above", three_face_camera()),
        ("from below", under_camera()),
    ] {
        let (index, pose) = planned_part_pose(&mesh, "player_wide", "body", &camera);
        let rot = Mat3::from_mat4(pose);
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            let v = (rot * axis).normalize();
            let best = v.x.abs().max(v.y.abs()).max(v.z.abs());
            assert!(
                best > 0.999,
                "the body part's matrix rotates {axis:?} to ({:.3},{:.3},{:.3}), \
                 off-axis — the six axis normals are not this part's prediction set",
                v.x,
                v.y,
                v.z
            );
        }
        let frame = render_part(&gpu, &mesh, index, pose, camera.view_projection());
        assert_buckets_match(
            &format!("player_wide body at rest, {label}"),
            &frame,
            &predicted,
            &rival,
            4_000,
        );
        let observed = buckets(&frame);
        for (byte, b) in &observed {
            if b.count > 200 {
                brightest = brightest.max(i32::from(*byte));
                dimmest = dimmest.min(i32::from(*byte));
            }
        }

        // Value bound to *location*, which is what makes the normal's sign
        // testable. Seen from above, the topmost band of the box is its `+Y` face
        // and must be vanilla's `1.0`; seen from below, the bottommost band is its
        // `-Y` face and must be vanilla's `0.4`. A globally flipped normal swaps
        // the two, leaves the frame's *set* of byte values untouched, and passes
        // every set-wise assertion above — measured, not supposed.
        let (want_normal, want_name, from_top) = if label == "from above" {
            (Vec3::Y, "+Y up", true)
        } else {
            (Vec3::NEG_Y, "-Y down", false)
        };
        let (row, byte) = modal_byte_at_edge_row(&frame, from_top);
        let want = byte_for(vanilla_diffuse(want_normal));
        let flipped = byte_for(vanilla_diffuse(-want_normal));
        println!(
            "  {label}: {} edge row y={row} reads byte {byte}; {want_name} predicts \
             {want}, a flipped normal predicts {flipped}",
            if from_top { "top" } else { "bottom" }
        );
        assert!(
            (i32::from(byte) - want).abs() <= TOL,
            "{label}: the {} face at row y={row} reads byte {byte}, not {want}. \
             {flipped} would mean the reconstructed normal points away from the eye \
             rather than toward it",
            want_name
        );
    }

    // The signature the rival formula structurally cannot produce: `abs()` lights a
    // face pointing *away* from the light exactly as brightly as one pointing into
    // it, so its up and down are both 0.9085 and no bucket of its can be near 51.
    // Vanilla's spread over the same two faces is 128 -> 51.
    println!("  brightest bucket {brightest}, dimmest bucket (>200 px) {dimmest}");
    let vanilla_spread = byte_for(vanilla_diffuse(Vec3::Y)) - byte_for(vanilla_diffuse(Vec3::NEG_Y));
    let rival_spread =
        byte_for(one_abs_light_diffuse(Vec3::Y)) - byte_for(one_abs_light_diffuse(Vec3::X));
    assert!(
        brightest - dimmest >= 60,
        "brightest-to-dimmest spread is only {} bytes; vanilla's up-to-down spread \
         is {vanilla_spread} and the widest spread one abs light can reach over any \
         two axis faces is {rival_spread}",
        brightest - dimmest,
    );
}

/// **The first-person arm must land on vanilla's two-light diffuse too** — the
/// surface the issue was reported against.
///
/// The arm is the interesting case precisely because it is **not** axis-aligned:
/// `first_person_arm_pose` rotates it, so its face normals fall wherever the pose
/// puts them, and the old formula's dark band (its `0.4` floor at normals
/// *perpendicular* to a single light) is reachable. Axis-aligned geometry can
/// never reach it, which is why a mob-only gate would have missed the reported
/// defect.
///
/// The predictions are computed from the real pose matrix, not hardcoded.
///
/// The bucket-set assertion alone is **not** sensitive to a flipped normal here —
/// the arm's six rotated normals produce the same six values either way, and a
/// sign-flip control was observed passing it. The brightness floor below is what
/// makes this gate see the reported symptom: it is a magnitude claim about the
/// dominant visible surface, measured at byte 112 with vanilla's two lights, 64
/// with one `abs()` light, and 51 with the normal's sign flipped.
#[test]
#[ignore = "requires a GPU adapter; run explicitly"]
fn the_first_person_arm_matches_vanillas_two_light_diffuse() {
    let Some(gpu) = setup() else {
        panic!(
            "entity_diffuse_two_lights_pixels: no GPU adapter; this test is \
             #[ignore]d so a missing one is a failure"
        )
    };
    let mesh = player_mesh();
    let pose = first_person_arm_pose(&mesh, Arm::Right, 0.0).expect("right arm");

    // The pose must be rigid for `mat3 * n` to be the normal transform. If it
    // ever gains a non-uniform scale this assertion fires rather than the
    // predictions silently drifting.
    let rot = Mat3::from_mat4(pose);
    for (i, col) in [rot.x_axis, rot.y_axis, rot.z_axis].into_iter().enumerate() {
        assert!(
            (col.length() - 1.0).abs() < 1e-3,
            "arm pose column {i} has length {} — not a rotation, so `mat3 * n` is \
             not the normal transform and every prediction below is wrong",
            col.length()
        );
    }

    let predicted: Vec<(String, f32)> = axis_normals()
        .into_iter()
        .map(|(name, v)| {
            let n = (rot * v).normalize();
            (
                format!("{name} -> ({:.3},{:.3},{:.3})", n.x, n.y, n.z),
                vanilla_diffuse(n),
            )
        })
        .collect();
    let rival: Vec<(String, f32)> = axis_normals()
        .into_iter()
        .map(|(name, v)| {
            let n = (rot * v).normalize();
            (name, one_abs_light_diffuse(n))
        })
        .collect();

    let index = *first_person_arm_parts(&mesh, Arm::Right)
        .first()
        .expect("player_wide has a right arm");
    let frame = render_part(
        &gpu,
        &mesh,
        index,
        pose,
        hand_projection(W as f32 / H as f32),
    );
    assert_buckets_match(
        "first-person right arm, rested",
        &frame,
        &predicted,
        &rival,
        1_500,
    );

    // **The reported symptom, as a magnitude.** The arm is one long box, so a
    // single face carries the overwhelming majority of its pixels; that face is
    // what a player calls "the arm". Measured on this rested pose:
    //
    //   vanilla two lights   byte 112  (diffuse 0.877)
    //   one abs light        byte  64  (diffuse 0.497)  <- what shipped
    //   flipped normal       byte  51  (diffuse 0.400)
    //
    // The floor is set between the correct value and both wrong ones rather than
    // at an ordering: "the arm is darker than the top of a mob" is true in vanilla
    // too and would pass on all three.
    let observed = buckets(&frame);
    let (dominant_byte, dominant) = observed
        .iter()
        .max_by_key(|(_, b)| b.count)
        .expect("some arm pixels");
    let total: u32 = observed.values().map(|b| b.count).sum();
    println!(
        "  dominant arm surface: byte {dominant_byte}, {}/{total} px, bbox \
         x{}..{} y{}..{}",
        dominant.count, dominant.x0, dominant.x1, dominant.y0, dominant.y1
    );
    assert!(
        dominant.count * 2 > total,
        "no single surface carries most of the arm ({}/{total}) — this assertion \
         assumes the long side dominates",
        dominant.count
    );
    assert!(
        i32::from(*dominant_byte) >= 100,
        "the arm's dominant surface reads byte {dominant_byte}. Vanilla's two \
         lights put it at 112 here; 64 is the single-abs-light value this issue \
         was filed about and 51 is what a flipped surface normal produces. bbox \
         x{}..{} y{}..{}",
        dominant.x0,
        dominant.x1,
        dominant.y0,
        dominant.y1
    );
}

/// The census, printed and not asserted: the numbers the diagnosis rests on, so
/// they can be re-derived on any machine without reading this file's asserts.
#[test]
fn diffuse_census_both_hypotheses() {
    println!("=== diffuse per surface normal (texel {TEXEL}, light_term 1.0) ===");
    println!("normal            vanilla  byte |  one abs light  byte");
    for (name, n) in axis_normals() {
        let v = vanilla_diffuse(n);
        let o = one_abs_light_diffuse(n);
        println!(
            "{name:<16}  {v:.4}   {:>3} |  {o:.4}          {:>3}",
            byte_for(v),
            byte_for(o)
        );
    }
    // The worst case, and the shape of the reported symptom: a normal
    // *perpendicular* to the single light sits at that formula's 0.4 floor while
    // vanilla lights it at 0.91. Only off-axis geometry — the arm, a rotated
    // limb — can reach it, which is why mobs standing still looked acceptable.
    let worst = Vec3::new(0.0, 0.466_025, -0.847_428);
    let v = vanilla_diffuse(worst);
    let o = one_abs_light_diffuse(worst);
    println!(
        "perpendicular {worst:?}: vanilla {v:.4} (byte {}) vs one abs light {o:.4} (byte {})",
        byte_for(v),
        byte_for(o)
    );
    assert!(
        (o - 0.4).abs() < 1e-3,
        "this normal is meant to be perpendicular to the single light; got {o:.4}"
    );
    assert!(
        v > 0.9,
        "vanilla should light this normal brightly; got {v:.4}"
    );
}
