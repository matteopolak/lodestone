//! Prove a drawn bow reaches pixels: the **arms move**, at arm height, and only
//! on a rig vanilla actually poses (issue #57).
//!
//! Reported as "skeletons charging bows do not animate, and neither does the
//! player". The state half of that (decoding `LivingEntity`'s using-item bit into
//! an `ItemUse` component) is covered by hermetic tests in `lodestone-v770` and
//! `lodestone-ecs`; this is the half that decides whether any of it counts, because
//! a crate's own test suite is a closed loop and every piece of this could be green
//! while the screen is unchanged.
//!
//! # What is measured, and why not a differing-pixel *count*
//!
//! A count answers "did anything change", which is also satisfied by a mob that
//! merely shifted, scaled, or moved its legs — and a pose applied to the wrong
//! parts does exactly that. The bow pose's defining property is specific and
//! local: **both arms swing up to horizontal in front of the chest**
//! (`HumanoidModel.poseRightArm`'s `case BOW_AND_ARROW` assigns `xRot = -PI/2`).
//! So the readings are all *locations*:
//!
//! * the **bounding box of the changed pixels**, which must sit inside the mob's
//!   own silhouette and in its upper half — at the arms, not at the feet;
//! * the silhouette's **width**, which must grow, because arms rotated forward
//!   from hanging-down extend the broadside profile;
//! * the silhouette's **sole row**, which must not move, because posing arms is
//!   not a translation.
//!
//! Per `CLAUDE.md`: a fraction cannot distinguish a uniform-but-wrong frame from a
//! localised blob, so every failure message prints a box.
//!
//! # Controls, and what each one would catch
//!
//! 1. **Rest rendered twice** must differ by *zero* pixels. Without it, "the
//!    frames differ" is also satisfied by a non-deterministic pipeline.
//! 2. **A zombie rig must not move at all.** This is the control that makes the
//!    measurement specific rather than merely non-zero: `animateZombieArms`
//!    assigns over both arms after the item pose, so vanilla shows a bow-holding
//!    zombie the ordinary arms-forward zombie pose. A gate that fired on the
//!    zombie too would be measuring "some AnimInput field changed", not "the bow
//!    pose landed".
//! 3. **The crossbow charge fraction must move pixels between 0.0 and 1.0.**
//!    `CrossbowCharge` is the one pose carrying a continuous parameter; if it were
//!    ignored, the two endpoints would be byte-identical and every crossbow would
//!    wind instantly. This is the same class of defect as the creeper whose
//!    `pose_swelling` ignored its argument.
//! 4. **Neither silhouette touches the frame border.** A clipped mob's width is
//!    set by the viewport, not by the pose — and arms swung forward are exactly
//!    what runs a broadside mob out of frame.
//!
//! # The premise this gate depends on, checked rather than assumed
//!
//! `CLAUDE.md`'s canonical false control asserted that a sky-less frame clears
//! uniformly, and failed because the first-person bare arm paints in every frame.
//! The equivalent question here is **what else paints in this rect**, and the
//! answer is enforced: this test drives `EntityPipeline` alone, on a flat
//! single-colour sheet, with no HUD, no hand pass, no sky and no terrain. The only
//! thing that can put a non-clear pixel on screen is the mob, and the only thing
//! that can move one is the pose.
//!
//! Note also that the mob is **broadside** (`BODY_YAW`) on purpose: face-on, arms
//! rotating forward move almost entirely into the depth buffer and the silhouette
//! barely changes, which would make a working pose read as a dead one.
//!
//! # Fail closed
//!
//! `#[ignore]`d, so running it is an explicit opt-in; once opted in, a missing
//! adapter is a **failure**, never a skip.

use glam::Vec3;
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{EntityModelSet, plan_entities};
use lodestone_render::entity_pipeline::{EntityPipeline, GpuEntityModel, upload_instances};
use lodestone_render::{AnimInput, ArmPose};

const W: u32 = 256;
const H: u32 = 256;

/// sRGB, matching the real swapchain — see `entity_anim_pixels` for why a plain
/// `Unorm` target quietly darkens the mob past a fixed brightness threshold.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Turn the mob broadside. Arms rotating from hanging-down to forward-horizontal
/// then sweep *across* the screen instead of into the depth buffer — face-on, a
/// correct pose is nearly invisible in silhouette.
const BODY_YAW: f32 = 90.0;

/// Minimum extra silhouette width the bow pose must add, in pixels.
///
/// Two arms swinging from vertical to horizontal each project roughly the arm's
/// length (12 texels ≈ 0.75 blocks) forward. At this camera that is tens of
/// pixels, so a bound of 6 is far above rasterisation jitter and far below the
/// real effect — it is sized to separate "the pose ran" from "it did not", not to
/// pin an exact projection.
const MIN_WIDTH_GAIN_PX: u32 = 6;

/// How far the sole row may drift. Posing arms is not a translation, so the
/// correct answer is zero; one pixel of allowance covers edge rasterisation on a
/// silhouette whose lowest row is a foot's flat bottom.
const MAX_SOLE_DRIFT_PX: u32 = 1;

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
                label: Some("bow_draw_pose_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// Flat opaque magenta: one colour over the whole sheet, so a differing pixel can
/// only mean the silhouette moved, never that a texture seam shifted.
fn test_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> (wgpu::TextureView, wgpu::Sampler) {
    const TW: u32 = 64;
    const TH: u32 = 64;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bow-sheet"),
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
        label: Some("bow-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (view, sampler)
}

/// Far enough back that a ~2-block mob with both arms extended still leaves clear
/// sky on every side. The border check enforces that rather than trusting it.
fn framing_camera() -> Camera {
    Camera {
        position: Vec3::new(0.0, 1.0, -3.6),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: 64.0,
    }
}

/// Render one mob with the given animation input and return the RGBA frame,
/// row-major, tightly packed.
///
/// Goes through [`EntityModelSet::resolve`] — the same call the live entity pass
/// makes — rather than assembling the instance by hand, so the pose reaches the
/// GPU by the production route and not a test-only one.
fn render_mob(gpu: &Gpu, models: &EntityModelSet, model: &str, anim: &AnimInput) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;
    let camera = framing_camera();

    let mesh = models.get(model).expect("model has a baked mesh");
    let instance = models
        .resolve(model, Vec3::ZERO, BODY_YAW, 1.0, anim)
        .expect("model resolves");

    let instances = [instance];
    let frame = plan_entities(&instances, &camera.frustum());
    assert_eq!(
        frame.instance_count(),
        1,
        "{model} was culled — this gate measures its silhouette, so it must be on screen"
    );

    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let (tex_view, sampler) = test_texture(device, queue);
    let cam_buf = pipeline.camera_buffer(device, &camera);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);
    let gpu_model = GpuEntityModel::upload(device, mesh).expect("mesh is non-empty");

    // One instance buffer per part: vertices are part-local, so each part is drawn
    // against its own matrices. This is what makes the gate sensitive to a *joint*
    // rotation at all — a single whole-model matrix could not express one.
    let mut per_part: Vec<(u32, std::ops::Range<u32>, wgpu::Buffer)> = Vec::new();
    for batch in &frame.batches {
        for (range, mats) in gpu_model.parts.iter().zip(&batch.parts) {
            if range.index_count == 0 {
                continue;
            }
            if let Some(buf) = upload_instances(device, mats, &batch.lights) {
                per_part.push((
                    mats.len() as u32,
                    range.index_start..range.index_start + range.index_count,
                    buf,
                ));
            }
        }
    }
    assert!(
        !per_part.is_empty(),
        "no part produced an instance buffer — nothing would be drawn"
    );

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bow-color"),
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
            label: Some("bow-pass"),
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
            pass.set_vertex_buffer(0, gpu_model.vertices.slice(..));
            pass.set_vertex_buffer(1, buf.slice(..));
            pass.set_index_buffer(gpu_model.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(range.clone(), 0, 0..*count);
        }
    }

    let bytes_per_row = W * 4;
    let padded = bytes_per_row.next_multiple_of(256);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bow-readback"),
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

/// Is this pixel part of the mob? Green separates the magenta mob (~20) from the
/// blue sky clear (~200) independently of how dark a face's shade is — the same
/// discriminator `entity_anim_pixels` and `creeper_swell_pixels` use.
fn is_mob(frame: &[u8], i: usize) -> bool {
    frame[i + 1] < 120 && frame[i] > 40
}

/// An inclusive pixel box, printed on every failure so a reader learns *where*
/// rather than *how much*.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Box {
    top: u32,
    bottom: u32,
    left: u32,
    right: u32,
    area: u32,
}

impl Box {
    fn width(self) -> u32 {
        self.right + 1 - self.left
    }

    fn height(self) -> u32 {
        self.bottom + 1 - self.top
    }
}

impl std::fmt::Display for Box {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rows {}..={} cols {}..={} ({}x{}, {} px)",
            self.top,
            self.bottom,
            self.left,
            self.right,
            self.width(),
            self.height(),
            self.area
        )
    }
}

/// Bounding box of every pixel for which `hit` is true, or `None` if there are
/// none.
fn box_of(mut hit: impl FnMut(usize, u32, u32) -> bool) -> Option<Box> {
    let (mut top, mut bottom) = (H, 0u32);
    let (mut left, mut right) = (W, 0u32);
    let mut area = 0u32;
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if hit(i, x, y) {
                top = top.min(y);
                bottom = bottom.max(y);
                left = left.min(x);
                right = right.max(x);
                area += 1;
            }
        }
    }
    (area > 0).then_some(Box {
        top,
        bottom,
        left,
        right,
        area,
    })
}

fn silhouette(frame: &[u8]) -> Box {
    box_of(|i, _, _| is_mob(frame, i)).expect("no mob silhouette found at all")
}

/// Bounding box of the pixels that differ between two frames, or `None` when they
/// are identical.
fn changed(a: &[u8], b: &[u8]) -> Option<Box> {
    box_of(|i, _, _| a[i..i + 3] != b[i..i + 3])
}

/// Fails if the silhouette reaches the viewport border, where its width would be
/// set by the frame rather than by the pose.
fn assert_unclipped(label: &str, b: Box) {
    assert!(
        b.top > 0 && b.bottom < H - 1 && b.left > 0 && b.right < W - 1,
        "{label}: silhouette is {b} in a {W}x{H} frame — clipped, so its width measures the \
         viewport rather than the arm pose"
    );
}

fn rest() -> AnimInput {
    AnimInput::REST
}

fn bow() -> AnimInput {
    AnimInput {
        arm_pose: ArmPose::BowAndArrow,
        ..AnimInput::REST
    }
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to prove the bow draw pose reaches pixels"]
fn a_drawn_bow_moves_a_skeletons_arms_and_leaves_a_zombies_alone() {
    let Some(gpu) = setup() else {
        panic!(
            "bow_draw_pose_pixels: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let models = EntityModelSet::load();

    let skel_rest = render_mob(&gpu, &models, "skeleton", &rest());
    let skel_bow = render_mob(&gpu, &models, "skeleton", &bow());
    // Control 1: the identical render path, identical input on both sides.
    let skel_rest_again = render_mob(&gpu, &models, "skeleton", &rest());

    let rest_box = silhouette(&skel_rest);
    let bow_box = silhouette(&skel_bow);
    let determinism = changed(&skel_rest, &skel_rest_again);
    let moved = changed(&skel_rest, &skel_bow);

    println!("=== BOW DRAW POSE PIXEL GATE (skeleton, broadside) ===");
    println!("rest silhouette : {rest_box}");
    println!("bow  silhouette : {bow_box}");
    println!(
        "changed by pose : {}",
        moved.map_or("NOTHING".to_string(), |b| b.to_string())
    );
    println!(
        "determinism ctl : {}",
        determinism.map_or("identical (correct)".to_string(), |b| b.to_string())
    );

    assert!(
        determinism.is_none(),
        "two renders of the *same* resting skeleton differ at {} — the pipeline is not \
         deterministic, so nothing measured below is attributable to the pose",
        determinism.unwrap()
    );
    assert_unclipped("rest", rest_box);
    assert_unclipped("bow", bow_box);

    // (a) The pose must move pixels at all. This is the assertion that fails on
    //     the build this change replaced, where nothing decoded the using-item bit
    //     and `setup_anim` had no item-pose branch.
    let moved = moved.unwrap_or_else(|| {
        panic!(
            "the bow pose changed ZERO pixels. rest silhouette {rest_box}, bow silhouette \
             {bow_box} — `Skeleton::pose_arms_for_item` is not reaching the drawn parts"
        )
    });

    // (b) ...*inside the mob's own rect*, not somewhere else in the frame. The
    //     changed box may extend past the resting silhouette (that is the point:
    //     the arms swing out beyond it) so it is compared against the union.
    let union_left = rest_box.left.min(bow_box.left);
    let union_right = rest_box.right.max(bow_box.right);
    let union_top = rest_box.top.min(bow_box.top);
    let union_bottom = rest_box.bottom.max(bow_box.bottom);
    assert!(
        moved.left >= union_left
            && moved.right <= union_right
            && moved.top >= union_top
            && moved.bottom <= union_bottom,
        "the pose changed pixels at {moved}, outside the mob's own rows {union_top}..={union_bottom} \
         cols {union_left}..={union_right} — something other than the mob moved"
    );

    // (c) ...at *arm* height, expressed as two bounds rather than one.
    //
    //     The first form of this assertion was "nothing below the waist differs",
    //     and it **failed on a working pose** — the changed box reached row 144
    //     against a waist of 126. The premise was false, and false before the
    //     feature existed: an arm is 12 texels long and hangs *downward* from the
    //     shoulder, so rotating it forward vacates every row its resting form
    //     occupied, and those run a full arm's length below the shoulder. On this
    //     framing the resting hand sits near row 148, which is exactly where the
    //     measured change stops. `CLAUDE.md`'s rule about a control's premise being
    //     false in the safe-looking direction applies to a gate's own assertions too.
    //
    //     So: the change must *start* at the shoulders (upper half) and must *not*
    //     reach the legs below the arms' reach. A pose wired to the legs fails the
    //     first; a whole-model transform fails both, since it would move the crown
    //     and the soles as well.
    //
    //     Both bounds are derived from the measured silhouette, never a restated
    //     constant: the anchor moves with the camera, and a hardcoded row is how a
    //     HUD gate once reported 0 px for a row that was drawing perfectly.
    let waist = rest_box.top + rest_box.height() / 2;
    let below_arm_reach = rest_box.bottom - rest_box.height() / 5;
    assert!(
        moved.top <= waist,
        "the pose's topmost changed row is {} but the waist is row {waist} (silhouette \
         {rest_box}) — a bow draw pivots at the shoulders, so the change must begin in the \
         upper half. Changed box {moved}",
        moved.top
    );
    assert!(
        moved.bottom <= below_arm_reach,
        "the pose changed pixels down to row {} , past row {below_arm_reach} where the arms can \
         no longer reach (silhouette {rest_box}) — the legs or the whole model are moving, not \
         just the arms. Changed box {moved}",
        moved.bottom
    );

    // (d) The broadside profile must get *wider*: two arms rotating from hanging
    //     to forward-horizontal project the arm's length ahead of the chest.
    let gain = bow_box.width().saturating_sub(rest_box.width());
    assert!(
        gain >= MIN_WIDTH_GAIN_PX,
        "the bow silhouette is {} px wide against the resting {} px, a gain of {gain} (needs \
         >= {MIN_WIDTH_GAIN_PX}). Arms swung forward must extend the broadside profile; a pose \
         that only rolls them about their own axis does not. rest {rest_box}, bow {bow_box}",
        bow_box.width(),
        rest_box.width()
    );

    // (e) ...and it must not be a translation. Posing arms leaves the feet put.
    assert!(
        bow_box.bottom.abs_diff(rest_box.bottom) <= MAX_SOLE_DRIFT_PX,
        "the soles moved from row {} to {} (allowance {MAX_SOLE_DRIFT_PX} px) — the arm pose is \
         being applied to the root instead of the arms",
        rest_box.bottom,
        bow_box.bottom
    );

    // Control 2: a zombie rig must be *untouched*. `animateZombieArms` assigns
    // over both arms after the item pose, which is vanilla's own behaviour, so
    // this is the control that makes the gate specific to the bow pose rather
    // than to "some AnimInput field changed".
    let zombie_rest = render_mob(&gpu, &models, "zombie", &rest());
    let zombie_bow = render_mob(&gpu, &models, "zombie", &bow());
    let zombie_moved = changed(&zombie_rest, &zombie_bow);
    println!(
        "zombie control  : {}",
        zombie_moved.map_or("identical (correct)".to_string(), |b| b.to_string())
    );
    assert!(
        zombie_moved.is_none(),
        "the bow pose moved a ZOMBIE's pixels at {} — vanilla's `animateZombieArms` assigns over \
         both arms afterwards, so a zombie holding a bow keeps the arms-forward zombie pose. If \
         this fires, the gate above is measuring the generic anim path, not the item pose",
        zombie_moved.unwrap()
    );
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to prove the crossbow charge fraction is live"]
fn a_crossbow_winds_between_its_two_endpoints() {
    let Some(gpu) = setup() else {
        panic!(
            "bow_draw_pose_pixels: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let models = EntityModelSet::load();
    let charge = |progress: f32| AnimInput {
        arm_pose: ArmPose::CrossbowCharge { progress },
        ..AnimInput::REST
    };

    let slack = render_mob(&gpu, &models, "skeleton", &charge(0.0));
    let wound = render_mob(&gpu, &models, "skeleton", &charge(1.0));
    let half = render_mob(&gpu, &models, "skeleton", &charge(0.5));

    let slack_box = silhouette(&slack);
    let wound_box = silhouette(&wound);
    let endpoints = changed(&slack, &wound);
    let midpoint = changed(&slack, &half);

    println!("=== CROSSBOW CHARGE PIXEL GATE (skeleton, broadside) ===");
    println!("progress 0.0 : {slack_box}");
    println!("progress 1.0 : {wound_box}");
    println!(
        "0.0 -> 1.0   : {}",
        endpoints.map_or("NOTHING".to_string(), |b| b.to_string())
    );
    println!(
        "0.0 -> 0.5   : {}",
        midpoint.map_or("NOTHING".to_string(), |b| b.to_string())
    );

    assert_unclipped("charge 0.0", slack_box);
    assert_unclipped("charge 1.0", wound_box);

    // This is the `pose_swelling`-ignored-its-argument defect, in crossbow form: a
    // charge that discards `progress` renders the two endpoints byte-identical and
    // every crossbow in the world winds instantly.
    let endpoints = endpoints.unwrap_or_else(|| {
        panic!(
            "the crossbow charge fraction changed ZERO pixels between 0.0 and 1.0 — `progress` is \
             being ignored. silhouette {slack_box}"
        )
    });
    // And the halfway frame must differ from *both*, which is what distinguishes a
    // live interpolation from a two-state flip at some threshold.
    let midpoint = midpoint.unwrap_or_else(|| {
        panic!(
            "halfway through the wind is byte-identical to the start — the charge is a step, not \
             a lerp. endpoints differ at {endpoints}"
        )
    });
    assert!(
        changed(&half, &wound).is_some(),
        "halfway through the wind is byte-identical to fully wound — the charge is a step, not a \
         lerp. 0.0 -> 0.5 differed at {midpoint}"
    );
}
