//! Prove the **first-person arm swing** reaches pixels.
//!
//! `entity.rs`'s unit tests prove `first_person_arm_chain`'s matrix moves when
//! `attack_anim` moves, and `lodestone_entity::pose`'s prove the clock that feeds
//! it advances per tick. Neither can prove the moved matrix is *uploaded*, that
//! the arm's part range still lines up with its instance buffer, or that the
//! swung arm stays inside the hand projection's frustum instead of swinging
//! straight out of frame or behind the near plane. All three are silent: the
//! matrix tests stay green and the screen shows a rigid arm.
//!
//! This repo has shipped that exact shape — a verified subsystem nothing draws —
//! nine times, so the arm is not finished until pixels move.
//!
//! # Why this gate is separate from the shell's headless arm gate
//!
//! `lodestone-shell`'s `the_shell_draws_a_mob_and_the_arm` renders through the
//! real `RenderState` and asserts the arm covers the bottom-right quadrant. It
//! passes unchanged whether or not the swing works, because a rested arm covers
//! that quadrant too. It also cannot vary the swing without a `Sim`. This gate
//! drives `attack_anim` directly through the same pipeline the shell's arm pass
//! uses (`EntityPipeline` + `hand_projection`) and measures the difference.
//!
//! # The assertions, and their controls
//!
//! 1. **The swing moves the arm.** Two frames at different `attack_anim` differ
//!    in a real number of pixels, not a handful of anti-aliasing texels.
//! 2. **A rested arm does not move.** The identical comparison with
//!    `attack_anim = 0.0` on both sides must produce **zero** differing pixels.
//!    Without this control, "the frames differ" is equally satisfied by a
//!    non-deterministic renderer, an uninitialised instance buffer, or a
//!    mismatched readback stride — none of which have anything to do with the
//!    swing. This is the negative control: it must fail assertion 1's threshold.
//! 3. **The arm is still on screen throughout.** Every phase of the swing has to
//!    cover a plausible share of the frame. A chain with a sign error still
//!    "moves pixels" — by swinging the arm out of frame, which assertion 1 would
//!    happily report as a large difference. Only a per-phase coverage floor
//!    separates "the arm swung" from "the arm left".
//!
//! # Fail closed
//!
//! `#[ignore]`d, so running it is an explicit opt-in; once opted in, a missing
//! adapter is a **failure**, never a skip. Needs no vanilla pack: `player_wide`'s
//! geometry is code-authored in `lodestone-assets`, and the sheet here is a flat
//! synthetic colour on purpose (see [`arm_texture`]).

use lodestone_render::block::{CameraUniform, DepthBuffer};
use lodestone_render::entity::{
    Arm, EntityMesh, first_person_arm_parts, first_person_arm_pose, hand_projection,
};
use lodestone_render::entity_pipeline::{
    EntityCameraUniform, EntityPipeline, GpuEntityModel, entity_camera_buffer, upload_instances,
};
use lodestone_render::fog::FogUniform;

const W: u32 = 256;
const H: u32 = 256;

/// **sRGB, matching the real swapchain.** The entity shader multiplies its shade
/// into the texel in *gamma* space, so a fixed brightness threshold calibrated on
/// a plain `Unorm` target quietly stops finding the arm at all — the same trap
/// `entity_anim_pixels.rs` documents.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Full-bright, like the arm pass's own light: this gate is about geometry, and a
/// dark arm would make the "is this an arm pixel?" test a lighting test.
const LIGHT: u32 = 0xF0;

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
                label: Some("arm_swing_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// Flat opaque magenta. Every arm texel is one colour, so a differing pixel can
/// only mean the silhouette moved — never that a texture seam shifted under it.
fn arm_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::TextureView, wgpu::Sampler) {
    const TW: u32 = 64;
    const TH: u32 = 64;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("arm-sheet"),
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
    let pixels: Vec<u8> = (0..TW * TH).flat_map(|_| [230u8, 30, 200, 255]).collect();
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
        label: Some("arm-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

/// The `player_wide` ("Steve") rig — the same one `prepare_first_person_hand`
/// draws, and the same construction `entity.rs`'s own `player_mesh` test helper
/// uses. Code-authored in `lodestone-assets`, so this needs no vanilla pack.
fn player_mesh() -> EntityMesh {
    EntityMesh::from_named_model(
        "player_wide",
        &lodestone_assets::entity::player_model(false),
    )
}

/// Render the right arm at `attack_anim` and return the RGBA frame, row-major and
/// tightly packed.
///
/// This mirrors `RenderState::prepare_first_person_hand` exactly where it matters:
/// group 0 is [`hand_projection`] **alone** (the arm pose is already in camera
/// space — feeding a view-projection here parks it at the world origin), and the
/// arm and its sleeve share one matrix.
fn render_arm(gpu: &Gpu, mesh: &EntityMesh, attack_anim: f32) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pose = first_person_arm_pose(mesh, Arm::Right, attack_anim)
        .expect("player_wide has a right arm");
    let parts = first_person_arm_parts(mesh, Arm::Right);
    assert!(
        !parts.is_empty(),
        "no arm parts to draw — this gate would measure an empty frame"
    );

    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let (tex_view, sampler) = arm_texture(device, queue);
    let cam_buf = entity_camera_buffer(
        device,
        EntityCameraUniform {
            camera: CameraUniform {
                view_proj: hand_projection(W as f32 / H as f32).to_cols_array_2d(),
                section_origin: [0.0; 4],
            },
            fog: FogUniform::disabled(),
        },
    );
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);

    let gpu_model = GpuEntityModel::upload(device, mesh).expect("player_wide mesh is non-empty");
    let mut per_part: Vec<(std::ops::Range<u32>, wgpu::Buffer)> = Vec::new();
    for index in parts {
        let range = gpu_model.parts[index];
        if range.index_count == 0 {
            continue;
        }
        let buffer =
            upload_instances(device, &[pose], &[LIGHT]).expect("one instance is never empty");
        per_part.push((range.index_start..range.index_start + range.index_count, buffer));
    }
    assert!(
        !per_part.is_empty(),
        "no part produced an instance buffer — nothing would be drawn"
    );

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("arm-color"),
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
            label: Some("arm-pass"),
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
                    // `[0,1]` DirectX-style depth, so the far plane is 1.0 — not
                    // vanilla's reversed-Z 0.0.
                    load: wgpu::LoadOp::Clear(1.0),
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
        for (range, buf) in &per_part {
            pass.set_vertex_buffer(0, gpu_model.vertices.slice(..));
            pass.set_vertex_buffer(1, buf.slice(..));
            pass.set_index_buffer(gpu_model.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(range.clone(), 0, 0..1);
        }
    }

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("arm-readback"),
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

/// Is this pixel part of the arm?
///
/// Green separates the magenta arm (low) from the blue sky clear (high) and is
/// independent of how dark a face's shade is — the same discriminator
/// `entity_anim_pixels.rs` uses, for the same reason.
fn is_arm(frame: &[u8], i: usize) -> bool {
    frame[i + 1] < 120 && frame[i] > 40
}

fn arm_pixels(frame: &[u8]) -> usize {
    (0..(W * H) as usize)
        .filter(|&p| is_arm(frame, p * 4))
        .count()
}

/// Pixels that are arm in exactly one of the two frames — the silhouette's
/// symmetric difference. Counting *changed classification* rather than changed
/// bytes ignores shading wobble and reports only real movement.
fn silhouette_difference(a: &[u8], b: &[u8]) -> usize {
    (0..(W * H) as usize)
        .filter(|&p| is_arm(a, p * 4) != is_arm(b, p * 4))
        .count()
}

#[test]
#[ignore = "requires a GPU adapter"]
fn the_first_person_arm_swing_moves_pixels() {
    let gpu = setup().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter), don't 'skip' — a silent pass \
         here would assert nothing",
    );
    let mesh = player_mesh();

    // Rest, and two phases where vanilla's shaping is far from rest. 0.25 is the
    // `sqrt`-exact point `arm_swing_terms_match_hand_evaluated_vanilla` pins.
    let rest = render_arm(&gpu, &mesh, 0.0);
    let quarter = render_arm(&gpu, &mesh, 0.25);
    let half = render_arm(&gpu, &mesh, 0.5);

    let rest_px = arm_pixels(&rest);
    let quarter_px = arm_pixels(&quarter);
    let half_px = arm_pixels(&half);

    // Assertion 2 (the negative control) FIRST, so a non-deterministic renderer
    // cannot masquerade as a working swing. Re-rendering the same phase must be
    // pixel-identical.
    let rest_again = render_arm(&gpu, &mesh, 0.0);
    let control = silhouette_difference(&rest, &rest_again);

    let moved_quarter = silhouette_difference(&rest, &quarter);
    let moved_half = silhouette_difference(&rest, &half);

    eprintln!("=== first-person arm swing (headless) ===");
    eprintln!("arm px @ 0.00   = {rest_px}");
    eprintln!("arm px @ 0.25   = {quarter_px}");
    eprintln!("arm px @ 0.50   = {half_px}");
    eprintln!("moved 0.00→0.25 = {moved_quarter}");
    eprintln!("moved 0.00→0.50 = {moved_half}");
    eprintln!("control (0→0)   = {control}");

    assert_eq!(
        control, 0,
        "the negative control moved {control} px: two renders of the *same* rest pose \
         differ, so this gate cannot attribute any difference to the swing"
    );

    // Assertion 3: the arm is on screen at every phase. Checked before assertion
    // 1, because a swing that flings the arm out of frame produces a *huge*
    // difference and would sail through it.
    for (phase, count) in [(0.0, rest_px), (0.25, quarter_px), (0.5, half_px)] {
        assert!(
            count > 800,
            "at attack_anim {phase} the arm covers only {count} px — the swing has \
             pushed it out of frame or behind the near plane, which a difference \
             count alone reads as success"
        );
        assert!(
            count < (W * H) as usize / 2,
            "at attack_anim {phase} the arm covers {count} px, over half the frame — \
             the swing has dragged it into the camera"
        );
    }

    // Assertion 1: the swing actually moves the silhouette. The threshold is far
    // above anti-aliasing noise (the control above is exactly 0) but well below
    // the arm's own footprint, so this measures motion, not disappearance.
    assert!(
        moved_quarter > 300,
        "attack_anim 0.25 moved only {moved_quarter} px of silhouette — the swing \
         terms are not reaching the uploaded instance matrix"
    );
    assert!(
        moved_half > 300,
        "attack_anim 0.50 moved only {moved_half} px of silhouette"
    );

    // And the two phases differ from *each other*, not merely from rest: a chain
    // that snapped to one fixed "swinging" pose would pass everything above.
    let between = silhouette_difference(&quarter, &half);
    eprintln!("moved 0.25→0.50 = {between}");
    assert!(
        between > 200,
        "0.25 and 0.50 differ by only {between} px — the arm is snapping to a single \
         pose rather than following the swing curve"
    );
}

/// Sweep the **whole** swing, not three sampled phases.
///
/// `the_first_person_arm_swing_moves_pixels` checks coverage at `0.0`, `0.25` and
/// `0.5`. A sign error in one term can leave those three fine and still send the
/// arm off screen somewhere in between — the swing is not monotonic (`ySwingPosition`
/// crosses zero mid-arc and `zSwingRotation` peaks late), so interpolating between
/// three good samples proves nothing about the rest.
///
/// Measured coverage across the swing, for the thresholds below: the arm's
/// footprint moves between roughly 1.1k and 4.8k px of a 65,536 px frame. It is
/// smallest around `0.5`, where the extra `Ry` turns the limb nearly edge-on —
/// which is why the floor here is well under the rest pose's own 2.7k rather than
/// anywhere near it.
#[test]
#[ignore = "requires a GPU adapter"]
fn the_arm_stays_on_screen_for_every_phase_of_the_swing() {
    let gpu = setup().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter), don't 'skip' — a silent pass \
         here would assert nothing",
    );
    let mesh = player_mesh();

    let mut counts = Vec::new();
    for step in 0..=16 {
        let phase = step as f32 / 16.0;
        let px = arm_pixels(&render_arm(&gpu, &mesh, phase));
        eprintln!("attack_anim {phase:.4} -> {px} px");
        counts.push((phase, px));
    }

    for (phase, px) in &counts {
        assert!(
            *px > 500,
            "at attack_anim {phase} the arm covers only {px} px — some phase of the \
             swing pushes it off screen or behind the near plane"
        );
        assert!(
            *px < (W * H) as usize / 2,
            "at attack_anim {phase} the arm covers {px} px, over half the frame"
        );
    }

    // The endpoints must agree: `attack_anim` 0 and 1 are both "arm at rest" (all
    // three position terms and the Y rotation return to zero), which is the
    // property that lets `lodestone_entity::pose`'s wrapped `attack_anim_lerp`
    // carry a finished swing forward to 1.0 instead of rewinding it.
    let first = counts.first().expect("swept").1;
    let last = counts.last().expect("swept").1;
    let drift = first.abs_diff(last);
    assert!(
        drift * 20 < first,
        "attack_anim 0.0 ({first} px) and 1.0 ({last} px) should both be the rested \
         arm, but they differ by {drift} px — the swing does not close its loop, so \
         the end of every swing will visibly snap"
    );
}
