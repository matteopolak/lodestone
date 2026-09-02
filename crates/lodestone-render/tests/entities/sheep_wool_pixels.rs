//! Prove the sheep wool layer reaches pixels (issue #53) — not just that
//! [`sheep_wool_model`] bakes and that `sheep_wool_tint` returns the right
//! bytes, which `lodestone-assets`' hermetic tests already cover.
//!
//! # Why this gate has to exist
//!
//! `CLAUDE.md`'s dominant defect class here is the *island*: a subsystem built,
//! tested, and reaching zero pixels because nothing calls it. A unit test that
//! bakes `sheep_wool_model()` and checks its inflation cannot tell you the mesh
//! actually poses correctly off a **real animated skeleton's** part matrices —
//! the same class of gap [`entity_anim_pixels`](./entity_anim_pixels.rs) closed
//! for limb animation. This gate does the equivalent for the wool *layer
//! mechanism* itself: bake the sheep body, resolve its real per-part world
//! matrices through the ordinary [`plan_entities`] path, and pose an
//! independently-baked wool mesh off those same matrices by part **name** —
//! exactly the discipline `ArmourMesh::attach` uses for armour, reimplemented
//! here against public API only (this pass does not own
//! `lodestone-render/src/entity.rs`, where `ArmourMesh` itself lives; see
//! `docs/entity-rendering.md` for the patch that would make this a shared type
//! instead of one test's local plumbing).
//!
//! # The three assertions, and their controls
//!
//! 1. **A sheared sheep and a woolly sheep differ.** The briefing's own
//!    suggested control: draw the body alone (`sheared`) against body + wool
//!    (`woolly`) and require a real, substantial number of differing pixels —
//!    proving the mesh, the by-name pose lookup, and the upload/draw path all
//!    actually connect.
//! 2. **Two sheared renders are identical.** The determinism control: without
//!    it, assertion 1 could be satisfied by a non-deterministic renderer.
//! 3. **The dye tint reaches the shader.** Restricted to exactly the pixels
//!    the wool layer newly covers (from assertion 1's diff mask), a red-tinted
//!    wool render must differ substantially from a white-tinted one — proving
//!    `sheep_wool_tint`'s bytes actually reach the GPU instance and multiply
//!    into the sampled texel, not just that the CPU table has the right
//!    numbers.
//!
//! # Fail closed
//!
//! `#[ignore]`d, so running it is an explicit opt-in; once opted in, a missing
//! adapter is a **failure**, never a skip.

use glam::{Mat4, Vec3};
use lodestone_assets::entity_models::{sheep_wool_model, sheep_wool_tint};
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{EntityMesh, EntityModelSet, plan_entities};
use lodestone_render::entity_anim::AnimInput;
use lodestone_render::entity_pipeline::{
    EntityPipeline, GpuEntityModel, InstanceTint, upload_instances, upload_instances_tinted,
};

const W: u32 = 256;
const H: u32 = 256;
/// sRGB, matching the real swapchain — see `entity_anim_pixels`'s note on why
/// this matters for a shader that shades in gamma space.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Side-on, so the torso's growth from the wool inflation reads as a
/// horizontal/vertical silhouette change rather than mostly depth.
const BODY_YAW: f32 = 90.0;

const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.40,
    g: 0.60,
    b: 0.95,
    a: 1.0,
};

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
                label: Some("sheep_wool_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// A flat, opaque texture of one colour — any pixel classification only ever
/// has to answer "did the silhouette change", never "did a UV seam shift".
fn flat_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rgba: [u8; 4],
) -> (wgpu::TextureView, wgpu::Sampler) {
    const TW: u32 = 64;
    const TH: u32 = 32;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sheep-wool-sheet"),
        size: wgpu::Extent3d {
            width: TW,
            height: TH,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let pixels: Vec<u8> = (0..TW * TH).flat_map(|_| rgba).collect();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(TW * 4),
            rows_per_image: Some(TH),
        },
        wgpu::Extent3d {
            width: TW,
            height: TH,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("sheep-wool-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

fn camera() -> Camera {
    Camera {
        position: Vec3::new(0.0, 0.9, -2.6),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    }
}

/// The six wool part names, exactly [`sheep_wool_model`]'s and `sheep_model`'s
/// shared pre-order (pinned independently by
/// `sheep_wool_model_shares_sheep_body_part_names_and_pivots` in
/// `lodestone-assets`).
const WOOL_PARTS: [&str; 6] = [
    "head",
    "body",
    "right_hind_leg",
    "left_hind_leg",
    "right_front_leg",
    "left_front_leg",
];

/// Render one sheep. `wool_tint` is `None` for "sheared" (the wool draw is
/// skipped entirely, matching vanilla's `if (!state.isSheared)` gate) or
/// `Some(rgb)` to draw the wool layer tinted that colour.
fn render_sheep(gpu: &Gpu, models: &EntityModelSet, wool_tint: Option<[u8; 3]>) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let cam = camera();

    let sheep = models
        .resolve("sheep", Vec3::new(0.0, 0.0, 0.0), BODY_YAW, 1.0, &AnimInput::REST)
        .expect("sheep has a baked model");
    let instances = [sheep];
    let frame = plan_entities(&instances, &cam.frustum());
    assert_eq!(
        frame.instance_count(),
        1,
        "the sheep was culled — this gate measures the wool layer, so it must be on screen"
    );
    let batch = &frame.batches[0];

    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let cam_buf = pipeline.camera_buffer(device, &cam);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);

    // Flat magenta body, exactly `entity_anim_pixels`'s sheet colour.
    let (body_view, body_sampler) = flat_texture(device, queue, [230, 30, 200, 255]);
    let body_tex_bg = pipeline.texture_bind_group(device, &body_view, &body_sampler);
    let body_mesh = models.get("sheep").expect("sheep mesh");
    let gpu_body = GpuEntityModel::upload(device, body_mesh).expect("sheep mesh is non-empty");

    let mut body_instance_bufs: Vec<(u32, std::ops::Range<u32>, wgpu::Buffer)> = Vec::new();
    for (range, mats) in gpu_body.parts.iter().zip(&batch.parts) {
        if range.index_count == 0 {
            continue;
        }
        if let Some(buf) = upload_instances(device, mats, &batch.lights) {
            body_instance_bufs.push((
                mats.len() as u32,
                range.index_start..range.index_start + range.index_count,
                buf,
            ));
        }
    }
    assert!(
        !body_instance_bufs.is_empty(),
        "no sheep body part produced an instance buffer"
    );

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sheep-wool-color"),
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

    // The wool layer: an independently-baked `EntityMesh` (its own `Skeleton`,
    // unrelated to the sheep body's), posed part-by-part off the sheep body's
    // *own* already-animated `part_transforms` by matching part name — the
    // `ArmourMesh::attach` discipline, reimplemented here against public API
    // only, since this pass does not own the file that type lives in
    // (`lodestone-render/src/entity.rs`; see `docs/entity-rendering.md`).
    // `None` (the "sheared" case) builds none of this and issues no wool draw
    // at all, matching vanilla's `if (!state.isSheared)` gate exactly.
    let wool_view_sampler = wool_tint.map(|_| flat_texture(device, queue, [200, 200, 200, 255]));
    let wool_tex_bg = wool_view_sampler
        .as_ref()
        .map(|(view, sampler)| pipeline.texture_bind_group(device, view, sampler));
    let wool_mesh_owned = wool_tint.map(|_| EntityMesh::from_model(&sheep_wool_model()));
    let gpu_wool = wool_mesh_owned
        .as_ref()
        .map(|mesh| GpuEntityModel::upload(device, mesh).expect("wool mesh non-empty"));
    let wool_instance_bufs: Vec<(u32, std::ops::Range<u32>, wgpu::Buffer)> =
        match (wool_tint, &gpu_wool, &wool_mesh_owned) {
            (Some(tint), Some(gpu_wool), Some(wool_mesh)) => {
                let mut draws = Vec::new();
                for name in WOOL_PARTS {
                    let wool_idx = wool_mesh
                        .skeleton
                        .index_of(name)
                        .unwrap_or_else(|| panic!("wool mesh has no {name} part"));
                    let body_idx = body_mesh
                        .skeleton
                        .index_of(name)
                        .unwrap_or_else(|| panic!("sheep body has no {name} part"));
                    let range = gpu_wool.parts[wool_idx];
                    let world: Mat4 = batch.parts[body_idx][0];
                    if let Some(buf) =
                        upload_instances_tinted(
                            device,
                            &[world],
                            &batch.lights[..1],
                            // `InstanceTint::rgb` is the no-overlay tint: this gate
                            // measures the dye multiply, not issue #98's hurt blend.
                            &[InstanceTint::rgb(tint)],
                        )
                    {
                        draws.push((
                            1u32,
                            range.index_start..range.index_start + range.index_count,
                            buf,
                        ));
                    }
                }
                draws
            }
            _ => Vec::new(),
        };
    if wool_tint.is_some() {
        assert!(
            !wool_instance_bufs.is_empty(),
            "wool layer requested but produced no instance buffers"
        );
    }

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("sheep-wool-pass"),
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

        // Body first, exactly vanilla's draw order (the base model, then the
        // `RenderLayer`s over it).
        pass.set_bind_group(1, &body_tex_bg, &[]);
        for (count, range, buf) in &body_instance_bufs {
            pass.set_vertex_buffer(0, gpu_body.vertices.slice(..));
            pass.set_vertex_buffer(1, buf.slice(..));
            pass.set_index_buffer(gpu_body.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(range.clone(), 0, 0..*count);
        }

        // Then the wool layer, if this render engages it at all.
        if let (Some(tex_bg), Some(gpu_wool)) = (&wool_tex_bg, &gpu_wool) {
            pass.set_bind_group(1, tex_bg, &[]);
            for (count, range, buf) in &wool_instance_bufs {
                pass.set_vertex_buffer(0, gpu_wool.vertices.slice(..));
                pass.set_vertex_buffer(1, buf.slice(..));
                pass.set_index_buffer(gpu_wool.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(range.clone(), 0, 0..*count);
            }
        }
    }

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sheep-wool-readback"),
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

/// Pixels differing (RGB only) between two equal-sized frames.
fn diff_mask(a: &[u8], b: &[u8]) -> Vec<bool> {
    (0..(W * H) as usize)
        .map(|i| {
            let o = i * 4;
            a[o..o + 3] != b[o..o + 3]
        })
        .collect()
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to prove the sheep wool layer reaches pixels"]
fn woolly_sheep_differs_from_sheared_and_the_dye_tint_reaches_the_shader() {
    let Some(gpu) = setup() else {
        panic!(
            "sheep_wool_pixels: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let models = EntityModelSet::load();

    let white = sheep_wool_tint(0);
    let red = sheep_wool_tint(14);
    assert_ne!(white, red, "test is meaningless if the two tints coincide");

    let sheared_1 = render_sheep(&gpu, &models, None);
    let sheared_2 = render_sheep(&gpu, &models, None);
    let woolly_white = render_sheep(&gpu, &models, Some(white));
    let woolly_red = render_sheep(&gpu, &models, Some(red));

    // Control: two renders of the identical (sheared) input must be pixel-for-
    // pixel identical. Without this, "sheared != woolly" below could be
    // satisfied by a non-deterministic renderer instead of the wool layer.
    let determinism_mask = diff_mask(&sheared_1, &sheared_2);
    let determinism_diffs = determinism_mask.iter().filter(|d| **d).count();
    assert_eq!(
        determinism_diffs, 0,
        "two renders of a *sheared* sheep differ by {determinism_diffs} px — the pipeline is not \
         deterministic, so the sheared-vs-woolly difference below proves nothing"
    );

    // Assertion 1: sheared vs. woolly. This mask is also this test's
    // independent measurement of exactly which screen pixels the wool layer
    // newly touches.
    let wool_mask = diff_mask(&sheared_1, &woolly_white);
    let wool_px = wool_mask.iter().filter(|d| **d).count();
    let total_px = (W * H) as usize;
    println!("=== SHEEP WOOL PIXEL GATE ===");
    println!("determinism control (sheared x2) : {determinism_diffs} px differ (must be 0)");
    println!("sheared vs woolly (white tint)    : {wool_px} px differ / {total_px} total");
    assert!(
        wool_px >= 500,
        "drawing the wool layer changed only {wool_px} px. sheep_wool_model bakes and \
         sheep_wool_tint returns the right bytes per lodestone-assets' own tests, so a near-zero \
         count here means the mesh is not actually posing off the sheep body's part_transforms — \
         check the by-name lookup against Skeleton::index_of"
    );
    assert!(
        wool_px < total_px / 2,
        "the wool layer changed {wool_px} of {total_px} px — more than half the frame, which is \
         not a localised layer addition but something flung across the whole image"
    );

    // Assertion 2: the dye tint. Restricted to exactly the pixels the wool
    // layer newly covers (from the mask above), a red-tinted wool render must
    // differ substantially from a white-tinted one at the same pose. This is
    // the only assertion that can fail while assertion 1 passes, and it is the
    // one that actually exercises `sheep_wool_tint`'s bytes reaching the GPU:
    // assertion 1 alone would also pass for a hardcoded, untinted wool draw.
    let mut tinted_diff_px = 0usize;
    let mut channel_delta_sum: u64 = 0;
    for (i, touched) in wool_mask.iter().enumerate() {
        if !touched {
            continue;
        }
        let o = i * 4;
        let a = &woolly_white[o..o + 3];
        let b = &woolly_red[o..o + 3];
        if a != b {
            tinted_diff_px += 1;
        }
        for c in 0..3 {
            channel_delta_sum += u64::from(a[c].abs_diff(b[c]));
        }
    }
    let avg_channel_delta = channel_delta_sum as f64 / (wool_px.max(1) * 3) as f64;
    println!("white-tint vs red-tint, in wool region : {tinted_diff_px}/{wool_px} px differ");
    println!("average per-channel byte delta         : {avg_channel_delta:.1}");
    assert!(
        tinted_diff_px * 2 >= wool_px,
        "only {tinted_diff_px}/{wool_px} wool pixels changed colour between a white and a red dye \
         — sheep_wool_tint's bytes are not reaching the shader's per-instance tint"
    );
    assert!(
        avg_channel_delta >= 20.0,
        "wool region average per-channel delta between white and red dye is only \
         {avg_channel_delta:.1} — too small to be the tint multiply actually engaging"
    );
}

/// Render-side half of the "white sheep render with no wool" fix.
///
/// The decode seam (`crates/protocol/v770/src/adapter.rs`'s
/// `handle_add_entity`, see `sheep_wool_default.rs` in that crate's own
/// tests) now synthesizes `EntityVariant::Dyed { color: 0, sheared: false }`
/// for a sheep spawn that carries no wool byte on the wire at all — vanilla's
/// own accessor default (`Sheep.defineSynchedData`:
/// `entityData.define(DATA_WOOL_ID, (byte)0)`). This crate cannot see that
/// decode seam (`lodestone-render` depends on neither `lodestone-v770` nor
/// the shell's snapshot fold), so what belongs here is the other half of the
/// chain the fix depends on: that colour ordinal `0` is a real, visible wool
/// colour at the asset/render layer, not a sentinel for "nothing to draw".
/// If it silently mapped to a transparent or absent texture, defaulting to
/// it would reproduce the exact "no wool" bug in a different guise —
/// `sheep_wool_tint(0)` would need to differ from *itself* to catch that,
/// which is exactly assertion 1 below, restricted to the one colour the fix
/// actually picks.
#[test]
#[ignore = "requires a GPU adapter; run explicitly to prove the decode seam's default colour reaches pixels"]
fn the_synthesized_default_wool_colour_renders_visibly() {
    let Some(gpu) = setup() else {
        panic!(
            "sheep_wool_pixels: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let models = EntityModelSet::load();

    // Exactly the value `handle_add_entity` synthesizes: ordinal 0.
    let default_wool = sheep_wool_tint(0);

    let sheared = render_sheep(&gpu, &models, None);
    let woolly_default = render_sheep(&gpu, &models, Some(default_wool));

    let wool_mask = diff_mask(&sheared, &woolly_default);
    let wool_px = wool_mask.iter().filter(|d| **d).count();
    let total_px = (W * H) as usize;
    println!("=== SYNTHESIZED-DEFAULT WOOL COLOUR GATE ===");
    println!("sheared vs woolly (synthesized default) : {wool_px} px differ / {total_px} total");
    assert!(
        wool_px >= 500,
        "a sheep rendered with the decode seam's synthesized default colour (ordinal 0) is only \
         {wool_px} px different from a bare, wool-less sheep — the default the fix picks is not \
         actually reaching visible pixels, which would silently reproduce the reported bug"
    );
    assert!(
        wool_px < total_px / 2,
        "the synthesized-default wool render changed {wool_px} of {total_px} px — more than half \
         the frame, not a localised wool layer"
    );
}
