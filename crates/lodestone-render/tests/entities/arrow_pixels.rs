//! Prove **arrows reach pixels, pointed the way they are flying** (issue #380).
//!
//! `lodestone-entity`'s `projectile.rs` has modelled arrow motion in detail for a
//! long time — gravity `0.05`, air inertia `0.99`, the different in-water step
//! order per `AbstractArrow.tick` — and drew exactly zero pixels, because no
//! renderer existed. That is this project's dominant defect class: a subsystem
//! that is individually built, individually tested, and nothing consumes. So the
//! bar for closing it is not "the rig bakes" (a corpus test already sweeps that)
//! and not "the matrix looks right" (`entity.rs`'s unit tests already pin that
//! against hand-derived values). It is *pixels*.
//!
//! # What this gate asserts, and what each control rules out
//!
//! 1. **An arrow covers its own projected rect, with the real vanilla sheet.**
//!    The rect is derived from the instance's own world AABB pushed through the
//!    same `view_projection` the draw uses — never hardcoded — so a change in
//!    framing moves the rect with the arrow instead of silently emptying it.
//!    *Control:* the identical measurement over a frame rendered with **no
//!    instances** must find zero arrow pixels in that same rect. Without it,
//!    "the rect has non-clear pixels in it" is also satisfied by a detector that
//!    cannot tell the clear colour from anything else. The real sheet (not a flat
//!    one) is deliberate here: `arrow.png` is mostly *transparent* in the strip
//!    the shaft box samples, and the shader cutouts at `alpha < 0.5`, so "the
//!    geometry is on screen" and "the arrow is visible" are genuinely different
//!    claims and this is the one that matters to a player.
//!
//! 2. **The shaft follows yaw and pitch.** Three poses whose silhouettes must be
//!    *shaped* differently: along `+X` (screen-horizontal), along `+Y` (screen-
//!    vertical), and along `+Z` (straight away from the camera, foreshortened to
//!    a stub). Measured as the silhouette's own bounding box, and **printed as a
//!    box on failure, never as a percentage** — an arrow is a few hundred pixels
//!    in a 512×512 frame, so any frame-average reading is swamped by the sky.
//!    These use a flat opaque sheet, because the question is silhouette shape and
//!    a multi-hued sheet with alpha holes would make "the long axis" a statement
//!    about the texture.
//!
//! 3. **The projectile placement is the one being used, not the mob placement.**
//!    This is the assertion with the strongest available control, because the
//!    *wrong* answer is a real function in the tree: `EntityInstance::new` puts
//!    the model origin `MODEL_FEET_OFFSET` = 1.501 blocks **above** the reported
//!    position and mirrors it. (Above, not below — the lift is applied before
//!    `LivingEntityRenderer`'s `scale(-1, -1, 1)`, so it comes back out positive.
//!    #380's notes said below, and so did this file's first draft; see
//!    `entity.rs`'s `reusing_the_mob_matrix_would_lift_an_arrow_and_reverse_it`.)
//!    Rendering the same arrow both ways and requiring the mob-placed one to land
//!    *outside* the projectile rect makes the pre-fix build its own negative
//!    control.
//!
//! # Measured: assertion 1 alone is not enough
//!
//! With `projectile_pitch_offset_deg("arrow")` deliberately returning `None` — i.e.
//! arrows back on the mob placement, the wrong answer — assertion 1 **still
//! passed**: `320 px` inside its rect and `0` outside, at rect `x238..274 y84..144`
//! where the correct build has `y240..272`. The reason is the reason the rect is
//! derived rather than hardcoded, turned against itself: it comes from the
//! *instance's own* AABB, so it rose 1.5 blocks with the wrongly-placed arrow and
//! the arrow filled it perfectly. Assertions 2 and 3 both failed.
//!
//! So "the arrow reaches pixels" is a genuinely weaker claim than "the arrow reaches
//! the *right* pixels", and the two extra tests are not belt-and-braces.
//!
//! # What this gate cannot distinguish, and why that is not a gap
//!
//! **A Y flip.** #380's issue text asked for a two-direction long-axis test and
//! warned, correctly, that such a test cannot see a wrong `scale(1, -1, 1)`. The
//! answer is stronger than the warning: on this rig a Y flip changes **no pixel at
//! all**, so no pixel gate and no live oracle could settle it — a vanilla frame
//! and a Y-flipped frame are the same frame. That is proved, in two places, rather
//! than argued here:
//!
//! * `lodestone-assets`' `a_y_flip_of_the_arrow_rig_moves_no_geometry` shows the
//!   flip moves **no vertex** anywhere in the rig (so the silhouette is
//!   flip-invariant from every angle — the fact the long-axis gate could not have
//!   caught) and changes **no UV on the two shaft planes**, which are the parts that
//!   sample the arrowhead, the sheet's only genuinely Y-asymmetric region. The
//!   trident rig is its control.
//! * `real_jar.rs`'s `arrow_fletching_patch_is_fully_symmetric` closes the one
//!   residual — the fletching plane keeps its four UV corners but reassigns them —
//!   against Mojang's own PNG: that 5×5 patch is a plus sign, invariant under the
//!   whole dihedral group. Its control is the arrowhead patch, which must fail the
//!   same check.
//!
//! The flip *would* become observable if the entity pipeline ever enabled back-face
//! culling (a flip reverses winding); today `cull_mode` is `None` and the shader
//! takes `abs(dot(n, light))`, so it does not.
//!
//! **Sub-90° roll about the shaft**, for the same symmetry reason: the fletching
//! planes sit at 45° and 135°, a set that maps to itself under a 90° roll.
//!
//! **A wrong `arrow_tipped` selection.** `TippableArrowRenderer` picks a second
//! sheet when `state.isTipped`; that bit is not decoded, so there is nothing to
//! test.
//!
//! # Fail closed
//!
//! `#[ignore]`d, so running it is an explicit opt-in; once opted in, a missing
//! adapter or a missing `client.jar` is a **failure**, never a skip.

#[path = "../gate_harness/mod.rs"]
mod gate_harness;

use glam::{Mat4, Vec3, Vec4};
use lodestone_assets::{Image, ResourceManager, ResourceSource, ZipSource};
use lodestone_render::block::DepthBuffer;
use lodestone_render::camera::Camera;
use lodestone_render::entity::{
    EntityInstance, EntityMesh, EntityModelSet, entity_texture_candidates, plan_entities,
};
use lodestone_render::entity_anim::AnimInput;
use lodestone_render::entity_pipeline::{EntityPipeline, GpuEntityModel, upload_instances};

const W: u32 = 512;
const H: u32 = 512;

/// `Rgba8Unorm`, not `…Srgb`: nothing here compares a *brightness* against a
/// fixed threshold (see the module note on why the Y flip is unmeasurable), only
/// "is this pixel the clear colour", so the transfer function is irrelevant and a
/// linear target keeps the sky byte exactly [`CLEAR`]'s.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A blue that no texel of `arrow.png` (greys and browns) and no texel of the flat
/// test sheet (magenta) comes near, so "not the clear colour" is an exact
/// silhouette test rather than a threshold.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.10,
    g: 0.35,
    b: 0.95,
    a: 1.0,
};

/// Where the arrow sits. Off the origin on every axis so a placement bug that
/// happens to be a no-op at the origin (a missing translate, a scale about the
/// wrong point) still moves it.
const POS: Vec3 = Vec3::new(1.5, 64.25, -3.0);

/// The tight framing, used only by the arrow-shape assertion. Close enough that a
/// ~0.9-block arrow spans a useful share of a 512-px frame: the arrow's *visible*
/// strip is thin — the shaft box is 4 model texels tall and only the middle fifth
/// of its texture strip is opaque — so framing it small is how a gate ends up
/// measuring a dozen edge pixels.
///
/// This started at `1.1`, which ran the broadside arrow off the right edge of the
/// frame. Every extent reading was then a statement about the viewport rather than
/// about the arrow, and the in-rect count still looked healthy because
/// [`projected_rect`] clamps to the viewport too. [`assert_not_clipped`] is the
/// guard that caught it and is now called on every silhouette this file measures.
const NEAR_FRAMING: f32 = 2.6;

/// The wide framing, for the two tests that need more than one arrow's worth of
/// room:
///
/// * the placement comparison has to fit **two** arrows 1.5 blocks apart (at
///   [`NEAR_FRAMING`] the mob-placed control was frustum-*culled* — arguably even
///   better evidence that the placements differ, but it produces no box to compare);
/// * the trident is nearly **2 blocks** long (`TridentModel`'s pole spans 31
///   texels) and hangs off its tip rather than its centre, so at [`NEAR_FRAMING`]
///   it ran off the right edge and [`assert_not_clipped`] caught it.
const WIDE_FRAMING: f32 = 5.0;

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
                label: Some("arrow_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// The camera: straight down `+Z` at [`POS`], so world `+X` is screen-horizontal
/// and world `+Y` screen-vertical. Both of the shaft directions assertion 2
/// compares are therefore in the image plane, and the third is along the view
/// axis — no reading in this file depends on re-deriving the camera's yaw
/// convention.
fn camera(distance: f32) -> Camera {
    Camera {
        position: Vec3::new(POS.x, POS.y, POS.z - distance),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.02,
        far: 64.0,
    }
}

/// The real 26.2 `client.jar`. Fails closed: this test is `#[ignore]`d, so a
/// missing jar is an environment failure, not a skip.
fn jar() -> ResourceManager {
    let path = gate_harness::require_client_jar();
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let zip = ZipSource::from_bytes(bytes).unwrap_or_else(|e| panic!("open jar: {e}"));
    ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>])
}

/// The vanilla sheet a corpus model resolves to, taken through
/// [`entity_texture_candidates`] rather than by naming the path — so a wrong
/// texture reference in the corpus entry fails here instead of drawing the arrow
/// in some other entity's skin.
fn vanilla_sheet(jar: &ResourceManager, model: &str) -> Image {
    let path = entity_texture_candidates(model)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("{model} has no texture candidate"));
    let png = jar
        .read(path)
        .unwrap_or_else(|| panic!("{path} missing from client.jar"));
    Image::decode_png(&png).unwrap_or_else(|e| panic!("decode {path}: {e}"))
}

/// A fully opaque flat sheet, for the assertions about silhouette *shape*: every
/// quad texel is drawn, so the measured box is the geometry's own and not a
/// statement about where `arrow.png` happens to be transparent.
fn flat_sheet() -> Image {
    const N: u32 = 32;
    Image {
        width: N,
        height: N,
        rgba: (0..N * N).flat_map(|_| [230u8, 30, 200, 255]).collect(),
    }
}

fn upload_sheet(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    img: &Image,
) -> (wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("arrow-sheet"),
        size: wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
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
    (
        texture.create_view(&wgpu::TextureViewDescriptor::default()),
        device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("arrow-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        }),
    )
}

/// Render `instances` with `img` as the sheet and return the RGBA frame,
/// row-major and tightly packed.
///
/// Takes an already-built instance list rather than a type path, because two of
/// the three tests need to place the *same* arrow through two different
/// placements — that difference is the thing under test, so it cannot be hidden
/// inside the renderer.
fn render(
    gpu: &Gpu,
    instances: &[EntityInstance],
    mesh: &EntityMesh,
    img: &Image,
    camera: &Camera,
) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let frame = plan_entities(instances, &camera.frustum());
    assert_eq!(
        frame.instance_count(),
        instances.len(),
        "an instance was frustum-culled; this gate measures what is drawn, so \
         everything it hands over must be on screen"
    );

    let pipeline = EntityPipeline::new(device, COLOR_FORMAT);
    let (tex_view, sampler) = upload_sheet(device, queue, img);
    let cam_buf = pipeline.camera_buffer(device, camera);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let tex_bg = pipeline.texture_bind_group(device, &tex_view, &sampler);
    let gpu_mesh = GpuEntityModel::upload(device, mesh).expect("arrow mesh is non-empty");

    // One instance buffer per *part*: the mesh's vertices are part-local, so
    // drawing the whole index range against `batch.transforms` would collapse the
    // fletching and both shaft planes onto the model origin.
    let mut per_part: Vec<(u32, std::ops::Range<u32>, wgpu::Buffer)> = Vec::new();
    for batch in &frame.batches {
        for (range, mats) in gpu_mesh.parts.iter().zip(&batch.parts) {
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
    if !instances.is_empty() {
        assert!(
            !per_part.is_empty(),
            "no part produced an instance buffer — nothing would be drawn"
        );
    }

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("arrow-color"),
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
            label: Some("arrow-pass"),
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
        label: Some("arrow-readback"),
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

/// Anything that is not the clear colour. The sheets in play are magenta and
/// vanilla's greys/browns, none of which is within 8 of [`CLEAR`]'s blue, so this
/// needs no brightness threshold and cannot mistake a dark face for sky.
fn is_arrow(frame: &[u8], i: usize) -> bool {
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

/// An inclusive pixel box, printed as a box rather than reduced to a fraction —
/// `CLAUDE.md`'s "make failure output say *where*".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Box2 {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Box2 {
    fn width(self) -> u32 {
        self.x1 + 1 - self.x0
    }
    fn height(self) -> u32 {
        self.y1 + 1 - self.y0
    }
    fn contains(self, x: u32, y: u32) -> bool {
        (self.x0..=self.x1).contains(&x) && (self.y0..=self.y1).contains(&y)
    }
}

impl std::fmt::Display for Box2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "x{}..{} y{}..{} ({}x{})",
            self.x0,
            self.x1,
            self.y0,
            self.y1,
            self.width(),
            self.height()
        )
    }
}

/// The drawn silhouette's box and pixel count, or `None` if nothing was drawn.
fn silhouette(frame: &[u8]) -> Option<(Box2, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (W, H, 0u32, 0u32);
    let mut area = 0u32;
    for y in 0..H {
        for x in 0..W {
            if is_arrow(frame, ((y * W + x) * 4) as usize) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
                area += 1;
            }
        }
    }
    (area > 0).then_some((Box2 { x0, y0, x1, y1 }, area))
}

/// Drawn pixels **inside** `rect`, and drawn pixels outside it.
fn inside_outside(frame: &[u8], rect: Box2) -> (u32, u32) {
    let (mut inside, mut outside) = (0u32, 0u32);
    for y in 0..H {
        for x in 0..W {
            if is_arrow(frame, ((y * W + x) * 4) as usize) {
                if rect.contains(x, y) {
                    inside += 1;
                } else {
                    outside += 1;
                }
            }
        }
    }
    (inside, outside)
}

/// The screen box an instance's own world AABB projects to, through the **same**
/// `view_projection` the draw uses.
///
/// Derived rather than hardcoded on purpose: `CLAUDE.md`'s "derive layout from
/// the same expression the draw uses" — a HUD gate that hardcoded a moving anchor
/// measured 20 px above a row that was drawing perfectly and reported zero.
fn projected_rect(instance: &EntityInstance, camera: &Camera) -> Box2 {
    let vp: Mat4 = camera.view_projection();
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for i in 0..8 {
        let corner = Vec3::new(
            if i & 1 == 0 {
                instance.aabb_min.x
            } else {
                instance.aabb_max.x
            },
            if i & 2 == 0 {
                instance.aabb_min.y
            } else {
                instance.aabb_max.y
            },
            if i & 4 == 0 {
                instance.aabb_min.z
            } else {
                instance.aabb_max.z
            },
        );
        let clip: Vec4 = vp * corner.extend(1.0);
        assert!(
            clip.w > 1e-4,
            "AABB corner {corner} is at or behind the camera plane (w={}) — the \
             projected rect would be meaningless",
            clip.w
        );
        let ndc = clip.truncate() / clip.w;
        // wgpu NDC is Y-up; the frame is Y-down.
        let px = (ndc.x * 0.5 + 0.5) * W as f32;
        let py = (0.5 - ndc.y * 0.5) * H as f32;
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px);
        y1 = y1.max(py);
    }
    // One pixel of slack on each side: the box is derived in continuous
    // coordinates and the rasteriser fills half-open pixel centres.
    Box2 {
        x0: (x0.floor() as i64 - 1).clamp(0, W as i64 - 1) as u32,
        y0: (y0.floor() as i64 - 1).clamp(0, H as i64 - 1) as u32,
        x1: (x1.ceil() as i64 + 1).clamp(0, W as i64 - 1) as u32,
        y1: (y1.ceil() as i64 + 1).clamp(0, H as i64 - 1) as u32,
    }
}

/// Resolve `type_path` through the **public seam** the shell uses, so
/// `canonical_model_name` and `projectile_pitch_offset_deg` are both *inside* the
/// gate. Reaching past them into the corpus is how a gate stays green while the
/// wrong placement is on screen.
fn projectile(set: &EntityModelSet, type_path: &str, yaw: f32, pitch: f32) -> EntityInstance {
    set.resolve_posed(type_path, POS, yaw, pitch, 1.0, &AnimInput::REST)
        .unwrap_or_else(|| panic!("{type_path} must resolve to a model"))
}

/// A silhouette that touches the viewport border has been **cut off**, and every
/// extent reading taken from it is a statement about the framing rather than about
/// the arrow. This is not a hypothetical: the first framing here (1.1 blocks) ran
/// the broadside arrow off the right edge, and the in-rect pixel count still looked
/// healthy because `projected_rect` clamps to the viewport too — so the aspect
/// assertions were silently comparing a clipped box against an unclipped one.
fn assert_not_clipped(label: &str, b: Box2) {
    assert!(
        b.x0 > 0 && b.y0 > 0 && b.x1 < W - 1 && b.y1 < H - 1,
        "{label}: silhouette {b} touches the {W}x{H} viewport border, so it is \
         clipped and its extents cannot be compared with anything"
    );
}

/// **Assertion 1.** An arrow drawn with the real vanilla sheet covers its own
/// projected rect — and the identical measurement over an empty scene finds
/// nothing, which is what makes the first number mean something.
#[test]
#[ignore = "requires a GPU adapter and .cache/mc/26.2/client.jar; run explicitly"]
fn an_arrow_reaches_pixels_inside_its_own_projected_rect() {
    let Some(gpu) = setup() else {
        panic!(
            "arrow_pixels: no GPU adapter. This test is #[ignore]d, so running it is an explicit \
             request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let set = EntityModelSet::load();
    let jar = jar();
    // Wide, because the trident shares this test and is nearly 2 blocks long.
    let cam = camera(WIDE_FRAMING);

    for type_path in ["arrow", "spectral_arrow", "trident"] {
        // Broadside: shaft along world +X, which this camera puts across the
        // screen. Yaw 90 because a projectile's yRot is `atan2(mx, mz)`, so +X
        // motion is yRot = 90 — the *projectile* convention, not a mob's.
        let inst = projectile(&set, type_path, 90.0, 0.0);
        let mesh = set.get(inst.model).expect("resolved model is in the set");
        let rect = projected_rect(&inst, &cam);
        let sheet = vanilla_sheet(&jar, inst.model);

        let frame = render(&gpu, std::slice::from_ref(&inst), mesh, &sheet, &cam);
        let Some((drawn, area)) = silhouette(&frame) else {
            panic!(
                "{type_path}: nothing drawn anywhere in the frame. Expected a silhouette inside \
                 {rect} (the instance's own projected AABB)."
            );
        };
        let (inside, outside) = inside_outside(&frame, rect);
        eprintln!("{type_path:<15} rect {rect}  drawn {drawn}  area {area}px  in/out {inside}/{outside}");
        assert_not_clipped(type_path, drawn);

        assert_eq!(
            outside, 0,
            "{type_path}: {outside} px drawn OUTSIDE the projected rect {rect} (silhouette {drawn}) \
             — the instance's AABB and its geometry disagree, which is a culling bug waiting to \
             happen at the screen edge"
        );
        // 120 px is a floor, not a target: the visible strip of `arrow.png` is the
        // middle fifth of a 4-texel-tall box, so a correct arrow at this framing is
        // a few hundred pixels. A floor of 1 would pass on a single stray texel.
        assert!(
            inside >= 120,
            "{type_path}: only {inside} px of arrow inside {rect}. Silhouette was {drawn}. \
             `arrow.png` is largely transparent in the strip the shaft box samples and the \
             shader cutouts below alpha 0.5, so a too-thin reading here means the UVs, not \
             the geometry."
        );

        // The control. Same camera, same rect, same detector, no instances: if
        // this finds pixels, the reading above is measuring the sky.
        let empty = render(&gpu, &[], mesh, &sheet, &cam);
        let (ci, co) = inside_outside(&empty, rect);
        assert_eq!(
            (ci, co),
            (0, 0),
            "control failed: an instance-free frame reports {ci} px inside {rect} and {co} \
             outside, so `is_arrow` cannot tell the clear colour from geometry and assertion \
             1 proves nothing"
        );
    }
}

/// **Assertion 2.** The shaft points where the entity is flying: three poses, three
/// distinguishable silhouette shapes.
///
/// A gate that only asked "did the arrow draw" passes on an arrow pointing the
/// wrong way, which is the *magnitude* species of vacuous test — measuring
/// whether rather than which way.
#[test]
#[ignore = "requires a GPU adapter; run explicitly"]
fn the_shaft_follows_yaw_and_pitch() {
    let Some(gpu) = setup() else {
        panic!(
            "arrow_pixels: no GPU adapter. This test is #[ignore]d, so running it is an explicit \
             request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let set = EntityModelSet::load();
    let sheet = flat_sheet();
    let cam = camera(NEAR_FRAMING);

    // (label, yaw, pitch). The expected world shaft direction is derived
    // independently, from `Projectile.shoot`'s `yRot = atan2(mx, mz)` /
    // `xRot = atan2(my, hdist)`, so the poses are not just "three numbers": +X
    // motion is `yRot = 90`, rising motion is a positive `xRot`.
    let poses = [
        ("along +X", 90.0f32, 0.0f32),
        ("along +Y", 90.0, 90.0),
        ("along +Z", 0.0, 0.0),
    ];
    let mut boxes = Vec::new();
    for (label, yaw, pitch) in poses {
        let inst = projectile(&set, "arrow", yaw, pitch);
        let mesh = set.get(inst.model).expect("arrow mesh");
        let frame = render(&gpu, std::slice::from_ref(&inst), mesh, &sheet, &cam);
        let (b, area) = silhouette(&frame)
            .unwrap_or_else(|| panic!("{label}: nothing drawn — cannot measure an axis"));
        eprintln!("{label:<10} yaw {yaw:>5} pitch {pitch:>5} -> {b} area {area}px");
        assert_not_clipped(label, b);
        boxes.push((label, b, area));
    }
    let (_, horizontal, broadside_area) = boxes[0];
    let (_, vertical, _) = boxes[1];
    let (_, into_screen, into_area) = boxes[2];

    // Along +X the box must be wider than tall; along +Y taller than wide. Stated
    // as ratios and against each other rather than as absolute pixel counts, so the
    // framing constants above can change without retuning the assertion.
    assert!(
        horizontal.width() > horizontal.height() * 2,
        "shaft along +X drew {horizontal}, which is not a horizontal bar. Vertical pose \
         drew {vertical}. A pitch applied about X instead of Z, or a swapped yaw term, \
         lands here."
    );
    assert!(
        vertical.height() > vertical.width() * 2,
        "shaft along +Y drew {vertical}, which is not a vertical bar. Horizontal pose \
         drew {horizontal}."
    );
    // The two long axes must genuinely be different axes, not the same box read
    // twice — the cheapest way this gate could go vacuous.
    assert!(
        vertical.height() > horizontal.height() && horizontal.width() > vertical.width(),
        "the two poses produced boxes that are not each other's transpose: +X {horizontal}, \
         +Y {vertical}"
    );
    // Along the view axis the arrow points at the camera, so it projects to a
    // roughly **isotropic** blob rather than a bar — the third reading, and the one
    // a "pitch about X instead of Z" bug cannot produce, because such a bug leaves
    // every arrow broadside whatever the pitch. Isotropy rather than smallness is
    // the right statement: the arrow's near end is much closer than its far end at
    // this framing, so perspective keeps the blob wide even though its long axis is
    // gone.
    let long = into_screen.width().max(into_screen.height());
    let short = into_screen.width().min(into_screen.height());
    assert!(
        long < short * 2,
        "shaft along +Z drew {into_screen} — aspect {long}:{short}, still a bar. \
         An arrow pointing at the camera has no long screen axis; broadside was \
         {horizontal}"
    );
    // And it must cover less than the broadside pose: a shaft seen end-on shows
    // its cross-section, not its length.
    assert!(
        into_area < broadside_area,
        "end-on area {into_area}px is not smaller than broadside {broadside_area}px \
         — the arrow may not be rotating with yaw at all"
    );
}

/// **Assertion 3.** The arrow is on the *projectile* placement, with the mob
/// placement as its own negative control.
///
/// `EntityInstance::new` is the wrong answer and it is a real function in the
/// tree: it applies `LivingEntityRenderer`'s `scale(-1, -1, 1)` and its
/// `translate(0, -1.501, 0)`. So this renders the identical mesh both ways and
/// requires the mob-placed arrow to fall *outside* the projectile arrow's rect —
/// 1.5 blocks at this framing is most of the frame height, so the two cannot
/// overlap unless the placements have converged.
#[test]
#[ignore = "requires a GPU adapter; run explicitly"]
fn the_mob_placement_would_draw_the_arrow_above_its_own_rect() {
    let Some(gpu) = setup() else {
        panic!(
            "arrow_pixels: no GPU adapter. This test is #[ignore]d, so running it is an explicit \
             request for the full GPU path — run it on a machine with an adapter."
        );
    };
    let set = EntityModelSet::load();
    let sheet = flat_sheet();
    // The wide framing: both arrows have to be *on screen* for their boxes to be
    // comparable. At `NEAR_FRAMING` the mob-placed control was frustum-culled.
    let cam = camera(WIDE_FRAMING);

    let good = projectile(&set, "arrow", 90.0, 0.0);
    let mesh = set.get(good.model).expect("arrow mesh");
    let rect = projected_rect(&good, &cam);

    let good_frame = render(&gpu, std::slice::from_ref(&good), mesh, &sheet, &cam);
    let (good_box, good_area) = silhouette(&good_frame).expect("the arrow must draw");
    let (good_inside, good_outside) = inside_outside(&good_frame, rect);
    eprintln!("projectile placement: {good_box} area {good_area}px in/out {good_inside}/{good_outside}");
    assert_not_clipped("projectile placement", good_box);
    assert_eq!(good_outside, 0, "projectile arrow {good_box} escapes {rect}");
    assert!(good_inside >= 150, "only {good_inside} px inside {rect}");

    // The control: the same mesh, the same reported position, the mob matrix.
    let bad = EntityInstance::new(good.model, mesh, POS, 90.0, 1.0, &AnimInput::REST);
    let bad_frame = render(&gpu, std::slice::from_ref(&bad), mesh, &sheet, &cam);
    let (bad_box, bad_area) = silhouette(&bad_frame)
        .expect("the control must draw something, or it is not a control");
    let (bad_inside, _) = inside_outside(&bad_frame, rect);
    eprintln!("mob placement (control): {bad_box} area {bad_area}px inside-good-rect {bad_inside}");
    assert_eq!(
        bad_inside, 0,
        "the mob-placed arrow put {bad_inside} px inside the projectile rect {rect} \
         (control box {bad_box}). The two placements must be visibly different, or \
         assertion 3 is measuring nothing."
    );
    // And in the direction that matters: the mob placement is *higher* in the
    // world, so **lower** in screen-Y numbers, because screen Y grows downward.
    // The offset's sign is the thing this project already got backwards once (see
    // the module docs), so it is asserted rather than implied.
    assert!(
        bad_box.y1 < good_box.y0,
        "the control box {bad_box} is not above the projectile box {good_box} in the \
         world (i.e. smaller screen-Y). `entity_model_matrix` applies its \
         `translate(0, -1.501, 0)` *before* `scale(-1, -1, 1)`, so the model origin \
         lands at feet + 1.501 — if this fires, check the sign before changing the \
         assertion."
    );
}
