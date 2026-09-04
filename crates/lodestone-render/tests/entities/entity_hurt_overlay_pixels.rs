//! Prove the per-entity hurt/death red overlay reaches pixels.
//!
//! A full-screen "hurt flash" tint and a camera shake on explosions were both
//! considered and checked against the decompiled 26.2 `client-src` before
//! writing any code, and neither held up:
//!
//! - There is no full-screen, screen-space red tint anywhere in vanilla tied to
//!   the hurt-time counter. The underwater/fire overlay pass this port
//!   already has (`crates/lodestone-render/src/screen_effects.rs`) has no
//!   vanilla analogue that reads hurt-time, and neither does the HUD, the
//!   world renderer or the game renderer. The only two things vanilla ties to
//!   a local player's own hurt-time counter are a camera *roll* (the damage
//!   tilt — the "screen tilt thing") and the per-entity overlay below.
//! - There is no camera-shake mechanism anywhere in `client-src` for
//!   explosions (or anything else): a shake-keyword search over the whole
//!   decompiled client source
//!   turns up exactly the bow-draw item wobble, nothing camera-related.
//!   Client-side explosion tracking only ever spawns particles.
//!
//! What vanilla **does** have, and this gate proves reaches pixels, is a
//! per-entity model overlay: the entity renderer sets a red-overlay flag
//! whenever the entity's hurt-time or death-time counter is nonzero, sampled
//! from a baked lookup texture — a flat `(255, 0, 0)` at alpha
//! `178/255` for every entity whose hurt-time is nonzero (the lookup's
//! `y < 8` row is the constant ARGB `-1291911168`, i.e. `(178, 255, 0, 0)`).
//! This is a **blend**, not a multiply — multiplying by red would crush the mob
//! toward black — and it applies to *any* drawn living entity, not the local
//! player's own screen.
//!
//! `EntityInstanceRaw::with_hurt_overlay` (`entity_pipeline.rs`) and the shader
//! change in `ENTITY_WGSL`'s `fs_main` are the render-side half of this. The
//! other half — computing `hurtTime > 0` per drawn entity and calling
//! `with_hurt_overlay` from the real per-frame draw path — needs
//! `crates/lodestone-shell/src/entities.rs`, which is out of scope for the
//! agent that wrote this gate (see the patch spec in `docs/combat.md`). This
//! gate proves the mechanism this crate owns; it does not and cannot prove
//! production ever calls it.
//!
//! # The controls
//!
//! - **Off is bit-identical to before the feature existed.** `control_a` never
//!   calls `with_hurt_overlay` at all (the exact call `upload_instances` makes
//!   today); `control_b` calls it with `false`. They must render pixel-for-pixel
//!   identical — an executed proof that the new code path is a true no-op when
//!   `HurtTime` is zero, not just "visually close."
//! - **Determinism.** Two renders of the same `hurt = true` configuration must
//!   differ by zero pixels, or the per-pixel colour comparisons below prove
//!   nothing.
//! - **Located, not averaged.** The reddening is checked per-pixel inside the
//!   mob's own silhouette (computed the same way
//!   `entity_variant_pixels.rs::silhouette` does, against the *actual* clear
//!   colour), never as a whole-frame average — a global tint or a stray
//!   full-screen pass would show up as a shifted background, not a bounded
//!   silhouette, and this gate's failure output prints the bounding box.

#[path = "../gate_harness/mod.rs"]
mod gate_harness;

use glam::Vec3;
use lodestone_assets::entity_models::zombie_model;
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{EntityInstance, EntityMesh, plan_entities};
use lodestone_render::entity_anim::AnimInput;
use lodestone_render::entity_pipeline::{
    EntityInstanceRaw, EntityPipeline, GpuEntityModel, HURT_OVERLAY_ALPHA_BYTE,
};
use wgpu::util::DeviceExt;

const W: u32 = 256;
const H: u32 = 256;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const BODY_YAW: f32 = 90.0;

const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.40,
    g: 0.60,
    b: 0.95,
    a: 1.0,
};

/// A flat, fully opaque magenta sheet, `(230, 30, 200)` — the same values
/// `entity_variant_pixels.rs::flat_sheet` uses. Uniform colour means any
/// per-pixel change between two renders of the same geometry can only be the
/// overlay, never a UV or lighting difference landing on a different texel.
fn flat_sheet() -> lodestone_assets::Image {
    const N: u32 = 64;
    lodestone_assets::Image {
        width: N,
        height: N,
        rgba: (0..N * N).flat_map(|_| [230u8, 30, 200, 255]).collect(),
    }
}

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
                label: Some("entity_hurt_overlay_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

fn upload_sheet(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    img: &lodestone_assets::Image,
) -> (wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hurt-overlay-sheet"),
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
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("hurt-overlay-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

fn side_camera() -> Camera {
    Camera {
        position: Vec3::new(0.0, 0.9, -2.2),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    }
}

/// Build the instance buffer directly rather than through
/// `entity_pipeline::upload_instances`, since that helper has no overlay
/// parameter (by design — nothing in `lodestone-shell` calls it with one yet,
/// see the module doc). `hurt` is applied identically to every instance.
fn upload_instances_hurt(
    device: &wgpu::Device,
    transforms: &[glam::Mat4],
    lights: &[u32],
    hurt: Option<bool>,
) -> Option<wgpu::Buffer> {
    if transforms.is_empty() {
        return None;
    }
    let fallback = u32::from(lodestone_render::entity::ENTITY_FULLBRIGHT);
    let raw: Vec<EntityInstanceRaw> = transforms
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let inst = EntityInstanceRaw::new(*m, lights.get(i).copied().unwrap_or(fallback));
            match hurt {
                Some(active) => inst.with_hurt_overlay(active),
                None => inst,
            }
        })
        .collect();
    Some(
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-hurt-instances"),
            contents: bytemuck::cast_slice(&raw),
            usage: wgpu::BufferUsages::VERTEX,
        }),
    )
}

/// Render a flat-magenta zombie. `hurt`: `None` = never call
/// `with_hurt_overlay` (the pre-feature code path), `Some(false)`/`Some(true)`
/// = call it explicitly with that value.
fn render(gpu: &Gpu, mesh: &EntityMesh, hurt: Option<bool>) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = side_camera();
    let img = flat_sheet();

    let inst = EntityInstance::new("mob", mesh, Vec3::ZERO, BODY_YAW, 1.0, &AnimInput::REST);
    let frame = plan_entities(std::slice::from_ref(&inst), &camera.frustum());
    assert_eq!(frame.instance_count(), 1, "the mob was culled");

    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let (tex_view, sampler) = upload_sheet(device, queue, &img);
    let cam_buf = pipeline.camera_buffer(device, &camera);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);
    let gpu_mesh = GpuEntityModel::upload(device, mesh).expect("mesh is non-empty");

    let mut per_part: Vec<(u32, std::ops::Range<u32>, wgpu::Buffer)> = Vec::new();
    for batch in &frame.batches {
        for (range, mats) in gpu_mesh.parts.iter().zip(&batch.parts) {
            if range.index_count == 0 {
                continue;
            }
            if let Some(buf) = upload_instances_hurt(device, mats, &batch.lights, hurt) {
                per_part.push((
                    mats.len() as u32,
                    range.index_start..range.index_start + range.index_count,
                    buf,
                ));
            }
        }
    }
    assert!(!per_part.is_empty(), "no part produced an instance buffer");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hurt-overlay-color"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = DepthBuffer::new(device, W, H);

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("hurt-overlay-pass"),
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
                view: &depth.view,
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
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[]);
        pass.set_bind_group(1, &tex_bg, &[]);
        for (count, range, buf) in &per_part {
            pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
            pass.set_vertex_buffer(1, buf.slice(..));
            pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(range.clone(), 0, 0..*count);
        }
    }

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("hurt-overlay-readback"),
        size: u64::from(padded) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
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
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
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
    readback.unmap();
    out
}

fn differing(a: &[u8], b: &[u8]) -> u32 {
    (0..(W * H) as usize)
        .filter(|i| a[i * 4..i * 4 + 3] != b[i * 4..i * 4 + 3])
        .count() as u32
}

fn is_mob(frame: &[u8], i: usize) -> bool {
    let clear = [
        (CLEAR.r * 255.0).round() as u8,
        (CLEAR.g * 255.0).round() as u8,
        (CLEAR.b * 255.0).round() as u8,
    ];
    frame[i..i + 3]
        .iter()
        .zip(clear)
        .any(|(got, want)| got.abs_diff(want) > 8)
}

/// `(min_x, max_x, min_y, max_y, area)` of the mob silhouette, so a failure can
/// print *where* the frame went wrong instead of just a fraction.
fn bbox(frame: &[u8]) -> (u32, u32, u32, u32, u32) {
    let (mut x0, mut x1, mut y0, mut y1, mut area) = (W, 0u32, H, 0u32, 0u32);
    for y in 0..H {
        for x in 0..W {
            if is_mob(frame, ((y * W + x) * 4) as usize) {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
                area += 1;
            }
        }
    }
    assert!(area > 3000, "only {area} px of mob found");
    (x0, x1, y0, y1, area)
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to prove the hurt overlay reaches pixels"]
fn hurt_overlay_reddens_the_mob_silhouette_and_nothing_else() {
    let Some(gpu) = setup() else {
        panic!(
            "entity_hurt_overlay_pixels: no GPU adapter. This test is #[ignore]d, so running it \
             is an explicit request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let mesh = EntityMesh::from_model(&zombie_model());

    // Executed negative controls: HurtTime==0 is represented two ways — never
    // calling `with_hurt_overlay` at all (control_a, the exact pre-feature call
    // shape) and calling it with `false` explicitly (control_b).
    let control_a = render(&gpu, &mesh, None);
    let control_b = render(&gpu, &mesh, Some(false));
    let hurt = render(&gpu, &mesh, Some(true));
    let hurt_repeat = render(&gpu, &mesh, Some(true));

    let off_vs_off = differing(&control_a, &control_b);
    let determinism = differing(&hurt, &hurt_repeat);
    let (x0, x1, y0, y1, area) = bbox(&control_a);

    // Per-pixel, per-channel comparison inside the silhouette only — the
    // background is CLEAR either way, and if the overlay ever leaked outside
    // the silhouette this loop would need to catch that too, so it scans the
    // whole frame, not just the bbox.
    let mut reddened = 0u32;
    let mut outside_silhouette_changed = 0u32;
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let mob_here = is_mob(&control_a, i);
            let r_off = control_a[i];
            let b_off = control_a[i + 2];
            let r_on = hurt[i];
            let b_on = hurt[i + 2];
            if mob_here {
                // Blending flat magenta (230,30,200) toward pure red (255,0,0)
                // at alpha 178/255 must raise R and lower B at every mob pixel,
                // regardless of per-face diffuse shading (which affects both
                // frames identically, so it cancels out of this comparison).
                if r_on > r_off && b_on < b_off {
                    reddened += 1;
                }
            } else if control_a[i..i + 3] != hurt[i..i + 3] {
                outside_silhouette_changed += 1;
            }
        }
    }

    println!("=== HURT OVERLAY PIXEL GATE ===");
    println!("mob bbox: x[{x0}..{x1}] y[{y0}..{y1}], area {area} px");
    println!("control A (no with_hurt_overlay call) vs control B (with_hurt_overlay(false)): {off_vs_off} px differ (must be 0)");
    println!("determinism (hurt x2): {determinism} px differ (must be 0)");
    println!("reddened mob pixels: {reddened} / {area}");
    println!("background pixels changed by the overlay: {outside_silhouette_changed} (must be 0)");
    println!("overlay alpha byte: {HURT_OVERLAY_ALPHA_BYTE} (vanilla's baked overlay lookup, red row)");

    assert_eq!(
        off_vs_off, 0,
        "never calling with_hurt_overlay differs from calling it with false by {off_vs_off} px — \
         the new code path is not a true no-op at HurtTime==0"
    );
    assert_eq!(
        determinism, 0,
        "two renders of the same hurt=true config differ by {determinism} px — the pipeline is \
         not deterministic, so the reddened count above proves nothing"
    );
    assert_eq!(
        outside_silhouette_changed, 0,
        "the overlay changed {outside_silhouette_changed} background pixels — this must be a \
         per-entity effect, never a full-screen one"
    );
    assert!(
        reddened as f32 > area as f32 * 0.95,
        "only {reddened} of {area} mob pixels reddened when hurt=true — expected the overlay to \
         cover essentially the whole silhouette, bbox x[{x0}..{x1}] y[{y0}..{y1}]"
    );
}
