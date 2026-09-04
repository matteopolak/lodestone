//! Offscreen gate for **per-corner ambient occlusion on the model path**
//! (`quad_corner_sample` inside [`mesh_models`]), plus the new
//! `ambientocclusion`-flag gate ([`ModelSectionView::ambient_occlusion_at`]).
//!
//! ## Which mesher this drives
//!
//! This renders through the real [`mesh_models`] + [`ModelPipeline`] — the
//! **model** path live server terrain actually uses via `lodestone-shell`'s
//! mesher (`crates/lodestone-shell/src/mesher.rs` calls `mesh_models`
//! directly). It does **not** touch `crate::mesh`'s `mesh_simple`/`mesh_greedy`
//! (the packed full-cube path, driving `--headless`'s demo scene, with its own
//! already-gated `ao_darkens_against_an_occluder`/`ao_flat_where_unoccluded`
//! unit tests in `src/mesh.rs`) — that is a different mesher with its own AO
//! implementation and is out of scope here.
//!
//! ## Why per-pixel, not per-frame average
//!
//! AO is a per-*corner* gradient. Averaging the whole frame cannot distinguish
//! "one corner is 20% darker" from "the whole face is 5% darker" — exactly the
//! measure-by-location trap. So this gate places a single, hand-derived
//! occluder that darkens **exactly one** of a quad's four corners (verified
//! against `quad_corner_sample`'s own unit test,
//! `ao_matches_vanillas_one_occluder_ratio_and_leaves_the_far_corner_bright`,
//! in `src/models.rs`), renders it full-frame so each texture corner lands
//! near the corresponding screen corner, and then:
//!
//! 1. samples all four screen corners individually and prints each byte, and
//! 2. scans the **whole frame** for a bounding box of "darkened" pixels and
//!    prints it, so a global dimming (which would fill the frame) is visibly
//!    distinguishable from a genuine single-corner gradient (a small box in
//!    one quadrant).
//!
//! ## Three scenes, two of them executed negative controls
//!
//! * [`OneOccluder`] — the real scene: one diagonal occluder, AO enabled
//!   (the `ModelSectionView::ambient_occlusion_at` default). Expects a small
//!   dark region in exactly one quadrant.
//! * [`NoOccluder`] — **executed negative control A**: identical scene, no
//!   occluder. Must render uniformly bright — proving the darkening in the
//!   first scene is caused by the occluder, not some fixed shader constant or
//!   a UV/lighting artefact.
//! * [`OneOccluderAoFlat`] — **executed negative control B**, exercising
//!   the flat-shading fallback directly: the *same* occluder as scene
//!   1, but `ambient_occlusion_at` returns `false`. Per vanilla's flat-mesh
//!   fallback, this must render uniformly bright **despite**
//!   the occluder — proving `mesh_models` actually consults the flag rather
//!   than always computing smooth AO.
//!
//! ## And three more scenes for the **occluder predicate** itself
//!
//! [`ao_occluder_predicate_is_shade_brightness_not_face_culling`] is a second
//! gate in this file, added when the AO term stopped reading
//! [`ModelSectionView::occludes_at`] (a *face-culling* predicate) and started
//! reading [`ModelSectionView::ao_occludes_at`] (vanilla's
//! per-block shade-brightness override, a *collision* predicate). The three scenes are chosen
//! so that each of the two candidate predicates produces a **different**
//! prediction, which is the only way a gate can tell them apart:
//!
//! | scene | `occludes_at` | `ao_occludes_at` | correct byte | byte if the old predicate were still in use |
//! |---|---|---|---|---|
//! | [`LeavesOccluder`] | `false` (cutout sprite) | `true` (full collision cube) | ~204 | 255 |
//! | [`BarrierOccluder`] | `false` | `false` (barrier's shade-brightness override is `1.0`) | 255 | 255 |
//! | [`CullingOccluderOnly`] | `true` | `false` | 255 | ~204 |
//!
//! `LeavesOccluder` is the bug this closes — the underside of a tree canopy. Its
//! occluder is a real `minecraft:*_leaves` state id, looked up by name in
//! `lodestone_data::block_states` and answered by
//! `lodestone_data::shade_brightness`, so nothing about the block population is
//! hand-typed here.
//!
//! `BarrierOccluder` is the control that fails if the predicate went the *other*
//! way: `barrier`'s collision shape **is** a full block, so a naive
//! full-collision-shape derivation with the seven per-block shade-brightness
//! overrides dropped would darken this scene. `CullingOccluderOnly` is the
//! control that fails if the AO term still reads `occludes_at` at all — it must
//! be byte 255, not the ~204 a face-culling-based predicate would produce.
//!
//! `#[ignore]`d because it needs a real GPU adapter; run explicitly:
//! `cargo test -p lodestone-render --test model_ao_corner_gate -- --ignored --nocapture`.

use lodestone_assets::{BakedQuad, Direction};
use lodestone_render::{
    GpuAtlas, GpuModelMesh, ModelPipeline, ModelSectionView, mesh_models, model_anim_buffer,
    model_palette_buffer, model_shared_camera_buffer, section_origin_buffer,
};

const W: u32 = 64;
const H: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Vanilla's one-occluder AO ratio: `(1.0 + 1.0 + 1.0 + 0.2) / 4`. Mirrors
/// `AO_OCCLUDED` in `src/models.rs`; duplicated here (as a documented
/// prediction, not a re-export) because this crate's own unit test already
/// proves the constant — this gate proves the **pixel** consequence of it.
const ONE_OCCLUDER_RATIO: f32 = 0.8;

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
                label: Some("model_ao_corner_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// A single `Up`-facing quad whose four corners are placed at the four screen
/// corners under the identity camera below (`world == clip`, exactly like
/// `model_shade_gamma_gate`'s `face_quad`), but — unlike that flat, single-shade
/// quad — with **real per-vertex block-local positions** so `quad_corner_sample`
/// sees four genuinely different corners instead of collapsing two of them.
///
/// `positions[i][0]` (screen x) doubles as `quad_corner_sample`'s `v`-axis sign
/// (`sv`, from `p[axis_of(v)] = p[0]` for an `Up` face) and `positions[i][2]`
/// doubles as its `u`-axis sign (`su`, from `p[axis_of(u)] = p[2]`) — `0.4`/`0.6`
/// straddle the function's `>= 0.5` threshold exactly like a cube's `0`/`1`
/// corners would, while staying safely inside wgpu's `[0, 1]` clip-space depth
/// range (unlike `-1`/`1`, which would put two corners at or past the far
/// plane). The four `(su, sv)` pairs this produces are all distinct, matching
/// `CORNERS`' `(0,0) -> (1,0) -> (1,1) -> (0,1)` order:
///
/// | vertex | screen (x, y) | (su, sv) |
/// |---|---|---|
/// | 0 | bottom-left  (-1,-1) | (-1,-1) |
/// | 1 | bottom-right (+1,-1) | (-1,+1) |
/// | 2 | top-right    (+1,+1) | (+1,+1) |
/// | 3 | top-left     (-1,+1) | (+1,-1) |
fn corner_ao_quad() -> BakedQuad {
    BakedQuad {
        positions: [
            [-1.0, -1.0, 0.4],
            [1.0, -1.0, 0.4],
            [1.0, 1.0, 0.6],
            [-1.0, 1.0, 0.6],
        ],
        uvs: [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
        direction: Direction::Up,
        cullface: None,
        tint_index: None,
        shade: true,
        layer: 0,
        anim: 0,
        sprite: 0,
    }
}

/// The block is at `(0, 0, 0)`, and the AO ring is centred on **that same
/// cell**, not on `(0, 1, 0)`.
///
/// That is the part worth reading before changing this constant.
/// `mesh_models` only samples the neighbour cell for a quad it can name a
/// lighting face for: one carrying a `cullface`, one whose plane is flush with
/// the cube face (`quad_is_on_face_boundary`), or one belonging to a block that
/// is itself a full occluding cube. [`corner_ao_quad`] is none of those — it is
/// an unculled `Up` quad whose four positions straddle `y = ±1` precisely so
/// they reach the screen corners under the identity camera, so its plane is
/// nowhere near `y = 1`. Vanilla's `faceCubic == false` branch therefore
/// applies and both the light ring and the AO ring move back onto the block's
/// own cell, `np = (0, 0, 0)`.
///
/// With that `np`, vertex 0 (`su = -1, sv = -1`) samples
/// `a = np + su*u = (0,0,-1)`, `b = np + sv*v = (-1,0,0)` and
/// `d = a + sv*v = (-1,0,-1)` (`u = (0,0,1)`, `v = (1,0,0)` for an `Up` face —
/// see `face_uv_axes` in `src/models.rs`). Occluding **only** `d` darkens
/// vertex 0 alone: vertices 1-3's own triples are
/// `{(0,0,-1),(1,0,0),(1,0,-1)}`, `{(0,0,1),(1,0,0),(1,0,1)}` and
/// `{(0,0,1),(-1,0,0),(-1,0,1)}`, none of which contains `(-1, 0, -1)`. The
/// executed negative controls below are what actually proves it.
///
/// This constant read `(-1, 1, -1)` — the same corner one cell higher — from
/// the day this gate landed, which was correct then and stopped being correct
/// when the cross-plant fix moved a non-flush quad's ring onto its own cell.
/// Nothing in the gate could see that: every corner simply read full-bright,
/// which is also what a genuinely broken AO term looks like.
const OCCLUDER: [i32; 3] = [-1, 0, -1];

struct OneOccluder;
impl ModelSectionView for OneOccluder {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        static ONCE: std::sync::OnceLock<Vec<BakedQuad>> = std::sync::OnceLock::new();
        if (x, y, z) == (0, 0, 0) {
            ONCE.get_or_init(|| vec![corner_ao_quad()])
        } else {
            &[]
        }
    }
    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        [x, y, z] == OCCLUDER
    }
}

struct NoOccluder;
impl ModelSectionView for NoOccluder {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        static ONCE: std::sync::OnceLock<Vec<BakedQuad>> = std::sync::OnceLock::new();
        if (x, y, z) == (0, 0, 0) {
            ONCE.get_or_init(|| vec![corner_ao_quad()])
        } else {
            &[]
        }
    }
    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

/// Identical occluder to [`OneOccluder`], but `ambient_occlusion_at` reports
/// `false` — the direct exercise of the flat-shading fallback. Per vanilla's
/// flat-mesh fallback, `mesh_models` must render this uniformly bright
/// despite the occluder.
struct OneOccluderAoFlat;
impl ModelSectionView for OneOccluderAoFlat {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        static ONCE: std::sync::OnceLock<Vec<BakedQuad>> = std::sync::OnceLock::new();
        if (x, y, z) == (0, 0, 0) {
            ONCE.get_or_init(|| vec![corner_ao_quad()])
        } else {
            &[]
        }
    }
    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        [x, y, z] == OCCLUDER
    }
    fn ambient_occlusion_at(&self, _x: usize, _y: usize, _z: usize) -> bool {
        false
    }
}

/// First state id whose block name matches `name`, from the committed vanilla
/// block-state census. Panics rather than skipping — a missing block would make
/// the scenes below vacuous, which is the "precondition species" of vacuous test
/// `CLAUDE.md` names.
fn first_id_named(name: &str) -> lodestone_data::block_states::StateId {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .and_then(lodestone_data::block_states::StateId::new)
        .unwrap_or_else(|| panic!("{name} present in the block-state census"))
}

/// A [`ModelSectionView`] whose single occluding cell is a real vanilla block
/// state, answering `ao_occludes_at` from
/// `lodestone_data::shade_brightness::occludes_ambient_light` — a faithful
/// miniature of the production override in
/// `crates/lodestone-shell/src/mesher.rs`.
///
/// `occludes_at` is hard-`false`, which is *not* a simplification: it is what
/// `BlockModels::occludes` genuinely answers for every block in this family
/// (leaves are cutout, `barrier` has no geometry at all), and it is exactly what
/// made the canopy bug invisible. So a scene that darkens here can only have
/// consulted `ao_occludes_at`.
struct CensusOccluder {
    state_id: lodestone_data::block_states::StateId,
}

impl ModelSectionView for CensusOccluder {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        static ONCE: std::sync::OnceLock<Vec<BakedQuad>> = std::sync::OnceLock::new();
        if (x, y, z) == (0, 0, 0) {
            ONCE.get_or_init(|| vec![corner_ao_quad()])
        } else {
            &[]
        }
    }
    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
    fn ao_occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        [x, y, z] == OCCLUDER
            && lodestone_data::shade_brightness::occludes_ambient_light(self.state_id)
    }
}

/// The mirror control: the *culling* predicate says the diagonal occludes and
/// the *AO* predicate says it does not. This scene darkened at ~204 before the
/// predicate swap and must be uniformly full-bright after it — the single
/// cleanest way to observe that the AO term changed which method it calls.
struct CullingOccluderOnly;
impl ModelSectionView for CullingOccluderOnly {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        static ONCE: std::sync::OnceLock<Vec<BakedQuad>> = std::sync::OnceLock::new();
        if (x, y, z) == (0, 0, 0) {
            ONCE.get_or_init(|| vec![corner_ao_quad()])
        } else {
            &[]
        }
    }
    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        [x, y, z] == OCCLUDER
    }
    fn ao_occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

/// Renders `view` through the real [`mesh_models`] + [`ModelPipeline`] and
/// reads back the whole `W x H` frame's red channel (the texture is neutral
/// white and untinted, so R == G == B and R alone is a faithful `shade =
/// ao * light_term` proxy; `light_term` is pinned to exactly `1.0` by the default
/// full-bright `corner_light_at`/`light_at` — vanilla's curve reaches `1.0` at
/// full light just as the retired linear ramp did — so only `ao` can move the
/// byte).
fn render_frame(gpu: &Gpu, view: &dyn ModelSectionView) -> Vec<u8> {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    // White, untinted texture: displayed byte == round(255 * shade) directly,
    // with no texel or tint factor to disentangle.
    let atlas = GpuAtlas::from_rgba(device, queue, 4, 4, &[255, 255, 255, 255].repeat(16), &[]);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);

    let palette = vec![[1.0_f32, 1.0, 1.0, 1.0]; 256];
    let palette_buffer = model_palette_buffer(device, &palette);
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    let anim_buffer = model_anim_buffer(device, &[]);
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);

    let cam_buffer = model_shared_camera_buffer(device, glam::Mat4::IDENTITY.to_cols_array_2d());
    let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);

    let mesh = mesh_models(view);
    assert_eq!(mesh.quad_count(), 1, "expected exactly one quad meshed");
    let gpu_mesh = GpuModelMesh::upload(device, &mesh).expect("non-empty quad");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("model ao corner target"),
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
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("model ao corner gate"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[0]);
        pass.set_bind_group(1, &atlas_bg, &[]);
        pass.set_bind_group(2, &palette_bg, &[]);
        pass.set_bind_group(3, &anim_bg, &[]);
        pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
        pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
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
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(enc.finish()));
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let data = readback.slice(..).get_mapped_range().expect("mapped range");

    // De-pad into a plain W*H red-channel buffer.
    let mut red = vec![0u8; (W * H) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = (y * padded + x * 4) as usize;
            red[(y * W + x) as usize] = data[i];
        }
    }
    red
}

/// Bounding box (inclusive, `(min_x, min_y, max_x, max_y)`) of pixels whose
/// red byte is below `threshold`, or `None` if no pixel qualifies.
fn dark_bbox(frame: &[u8], threshold: u8) -> Option<(u32, u32, u32, u32)> {
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for y in 0..H {
        for x in 0..W {
            if frame[(y * W + x) as usize] < threshold {
                bbox = Some(match bbox {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    bbox
}

/// The four screen-corner samples, inset `INSET` px from each edge, labelled
/// by which vertex of [`corner_ao_quad`] they correspond to under the
/// identity camera's NDC -> framebuffer mapping (`x=-1 -> col 0`,
/// `x=+1 -> col W-1`, `y=+1 -> row 0` (top), `y=-1 -> row H-1` (bottom), the
/// standard D3D-style viewport transform this renderer uses throughout — see
/// `docs/camera.md`. Only the `x`/`y` mapping matters here; the depth range is
/// reversed-Z and this gate does not read it.
const INSET: u32 = 3;

fn corner_samples(frame: &[u8]) -> [(&'static str, u8); 4] {
    let at = |x: u32, y: u32| frame[(y * W + x) as usize];
    [
        ("vertex 0 (bottom-left, occluded)", at(INSET, H - 1 - INSET)),
        ("vertex 1 (bottom-right)", at(W - 1 - INSET, H - 1 - INSET)),
        ("vertex 2 (top-right)", at(W - 1 - INSET, INSET)),
        ("vertex 3 (top-left)", at(INSET, INSET)),
    ]
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative controls fail"]
fn model_path_ao_darkens_exactly_one_corner_and_the_flag_can_disable_it() {
    let Some(gpu) = setup() else {
        panic!(
            "model_ao_corner_gate: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for a real GPU frame — a headless CI box has none and should not \
             run it."
        );
    };

    let occluded = render_frame(&gpu, &OneOccluder);
    let bare = render_frame(&gpu, &NoOccluder);
    let flat = render_frame(&gpu, &OneOccluderAoFlat);

    let predicted_byte = (255.0 * ONE_OCCLUDER_RATIO).round() as i32;

    println!("=== MODEL PATH PER-CORNER AO GATE ===");
    println!("--- scene 1: one diagonal occluder, AO enabled ---");
    for (label, byte) in corner_samples(&occluded) {
        println!("  {label}: byte = {byte}");
    }
    let occluded_bbox = dark_bbox(&occluded, 245);
    println!("  dark-region (byte < 245) bounding box: {occluded_bbox:?}");
    // A *much* tighter threshold, close to the exact predicted corner byte
    // (204): under linear interpolation across the triangle, only pixels
    // whose barycentric weight on vertex 0 is already high (>~69%) read this
    // dark, so — unlike the < 245 box above, which legitimately spans much of
    // the triangle as the gradient climbs back to full bright — this one is
    // tightly confined to the immediate neighbourhood of vertex 0 alone. That
    // confinement, not the < 245 box's size, is the actual "per-corner, not
    // global" evidence.
    let near_corner_bbox = dark_bbox(&occluded, 220);
    println!("  near-corner-value (byte < 220) bounding box: {near_corner_bbox:?}");
    println!("  predicted darkened-corner byte (ratio {ONE_OCCLUDER_RATIO}) ~= {predicted_byte}");

    println!("--- scene 2 (negative control): no occluder ---");
    for (label, byte) in corner_samples(&bare) {
        println!("  {label}: byte = {byte}");
    }
    let bare_bbox = dark_bbox(&bare, 245);
    println!("  dark-region bounding box: {bare_bbox:?}");

    println!("--- scene 3 (negative control): same occluder, ambient_occlusion_at = false ---");
    for (label, byte) in corner_samples(&flat) {
        println!("  {label}: byte = {byte}");
    }
    let flat_bbox = dark_bbox(&flat, 245);
    println!("  dark-region bounding box: {flat_bbox:?}");

    // --- Scene 1: exactly one corner darkened, confined to one quadrant ---
    let [v0, v1, v2, v3] = corner_samples(&occluded).map(|(_, b)| b);
    assert!(
        (predicted_byte - 12..=predicted_byte + 12).contains(&i32::from(v0)),
        "vertex 0 (the occluded corner) must read near the one-occluder ratio's byte \
         ({predicted_byte}), got {v0} — this is the actual smooth-AO darkening \
         `quad_corner_sample` computes, read back from a real rendered frame"
    );
    for (label, byte) in [("vertex 1", v1), ("vertex 2", v2), ("vertex 3", v3)] {
        assert!(
            byte >= 250,
            "{label} shares no AO-corner sample with the single diagonal occluder and must \
             stay full-bright (>=250), got {byte} — a value near vertex 0's darkened byte here \
             would mean the darkening leaked across the whole quad instead of staying per-corner"
        );
    }
    occluded_bbox.expect(
        "scene 1 must have a non-empty dark region — the occluder is real and must darken \
         something",
    );
    let (nx0, ny0, nx1, ny1) = near_corner_bbox.expect(
        "scene 1 must have a non-empty near-corner-value (<220) region — vertex 0 itself reads \
         near byte 210, well under this threshold",
    );
    let (nw, nh) = (nx1 - nx0 + 1, ny1 - ny0 + 1);
    assert!(
        nw <= W / 3 && nh <= H / 3,
        "the near-corner-value region must be tightly confined to one corner's neighbourhood \
         (<= {}x{}), got {nw}x{nh} at {near_corner_bbox:?} — a region this large at this \
         threshold would mean the darkening is not the sharp, single-corner gradient AO is \
         supposed to produce",
        W / 3,
        H / 3
    );
    // And it must actually sit in the same quadrant as vertex 0 (bottom-left:
    // low x, high y), not merely be small and located somewhere else.
    assert!(
        nx1 < W / 2 && ny0 >= H / 2,
        "the near-corner-value region {near_corner_bbox:?} must be in the bottom-left quadrant \
         (x < {}, y >= {}) — vertex 0's own screen location — not merely small",
        W / 2,
        H / 2
    );

    // --- Scene 2 (executed negative control): removing the occluder must
    // remove the darkening entirely, proving scene 1's dark region really is
    // caused by the occluder and not some fixed artefact of this quad/camera
    // setup that would darken a corner regardless.
    assert!(
        bare_bbox.is_none(),
        "control premise violated: with no occluder at all, every corner must render \
         full-bright — got a dark region at {bare_bbox:?}. If this fires, the darkening in \
         scene 1 cannot be attributed to the occluder and this gate proves nothing"
    );
    for (label, byte) in corner_samples(&bare) {
        assert!(byte >= 250, "{label} must be full-bright with no occluder, got {byte}");
    }

    // --- Scene 3 (executed negative control, and the feature under test):
    // the *same* occluder as scene 1, but `ambient_occlusion_at` says no.
    // Per vanilla's flat-mesh fallback, this must render exactly like scene 2 —
    // uniformly bright — proving `mesh_models` actually branches on the flag
    // rather than always computing smooth AO regardless of it.
    assert!(
        flat_bbox.is_none(),
        "ambient_occlusion_at() = false must suppress AO entirely, but the occluder still \
         darkened a region at {flat_bbox:?} — mesh_models is not consulting the flag"
    );
    for (label, byte) in corner_samples(&flat) {
        assert!(
            byte >= 250,
            "{label} must be full-bright when ambient_occlusion_at() = false, got {byte} — \
             the same occluder darkens vertex 0 in scene 1, so this can only be the flag \
             failing to gate the AO computation"
        );
    }
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative controls fail"]
fn ao_occluder_predicate_is_shade_brightness_not_face_culling() {
    let Some(gpu) = setup() else {
        panic!(
            "model_ao_corner_gate: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for a real GPU frame — a headless CI box has none and should not \
             run it."
        );
    };

    // Anti-vacuity, before any pixel: the census must actually disagree with the
    // culling predicate on these two blocks in *opposite* directions, or the two
    // scenes below measure nothing. Both facts are anchored to the real 26.2
    // server in `crates/lodestone-data/tests/shade_brightness.rs`.
    let leaves = first_id_named("minecraft:oak_leaves");
    let barrier = first_id_named("minecraft:barrier");
    assert_eq!(
        lodestone_data::shade_brightness::occludes_ambient_light(leaves),
        true,
        "premise: oak_leaves darkens an AO corner in vanilla (full collision cube, no \
         shade-brightness override). Without this the leaves scene proves nothing"
    );
    assert_eq!(
        lodestone_data::shade_brightness::occludes_ambient_light(barrier),
        false,
        "premise: barrier does NOT darken an AO corner (its shade-brightness override \
         is 1.0) even though its collision shape IS a full block — this is \
         what makes it a control against the naive collision derivation"
    );

    let leaves_frame = render_frame(&gpu, &CensusOccluder { state_id: leaves });
    let barrier_frame = render_frame(&gpu, &CensusOccluder { state_id: barrier });
    let culling_only = render_frame(&gpu, &CullingOccluderOnly);

    let predicted_byte = (255.0 * ONE_OCCLUDER_RATIO).round() as i32;

    println!("=== AO OCCLUDER PREDICATE GATE ===");
    println!("--- scene A: leaves occluder (occludes_at = false, ao_occludes_at = true) ---");
    for (label, byte) in corner_samples(&leaves_frame) {
        println!("  {label}: byte = {byte}");
    }
    let leaves_bbox = dark_bbox(&leaves_frame, 220);
    println!("  near-corner-value (byte < 220) bounding box: {leaves_bbox:?}");
    println!("  predicted darkened byte {predicted_byte}; 255 if the old predicate were in use");

    println!("--- scene B (control): barrier occluder, vanilla-exempt ---");
    for (label, byte) in corner_samples(&barrier_frame) {
        println!("  {label}: byte = {byte}");
    }
    let barrier_bbox = dark_bbox(&barrier_frame, 245);
    println!("  dark-region bounding box: {barrier_bbox:?}");

    println!("--- scene C (control): occludes_at = true, ao_occludes_at = false ---");
    for (label, byte) in corner_samples(&culling_only) {
        println!("  {label}: byte = {byte}");
    }
    let culling_bbox = dark_bbox(&culling_only, 245);
    println!("  dark-region bounding box: {culling_bbox:?}");

    // --- Scene A: the canopy. Predicts a *value*, and the wrong-hypothesis
    // value (255, full-bright) is outside the tolerance band, so this cannot be
    // satisfied by both predicates the way a sign-only assertion could be.
    let [v0, v1, v2, v3] = corner_samples(&leaves_frame).map(|(_, b)| b);
    assert!(
        (predicted_byte - 12..=predicted_byte + 12).contains(&i32::from(v0)),
        "vertex 0's neighbour is a leaf block: vanilla's shade brightness is 0.2 there, so \
         the corner must read near {predicted_byte}, got {v0}. A reading of 255 means the AO \
         term is still asking occludes_at (leaves are cutout, so it answers `false`) — the \
         canopy-underside bug"
    );
    for (label, byte) in [("vertex 1", v1), ("vertex 2", v2), ("vertex 3", v3)] {
        assert!(
            byte >= 250,
            "{label} shares no AO-corner sample with the leaf occluder and must stay \
             full-bright (>=250), got {byte}"
        );
    }
    let (lx0, ly0, lx1, ly1) = leaves_bbox.expect(
        "scene A must have a non-empty near-corner-value (<220) region — vertex 0 reads near \
         the predicted byte, well under this threshold",
    );
    let (lw, lh) = (lx1 - lx0 + 1, ly1 - ly0 + 1);
    assert!(
        lw <= W / 3 && lh <= H / 3 && lx1 < W / 2 && ly0 >= H / 2,
        "scene A's darkening must be the same tightly-confined bottom-left corner gradient \
         scene 1 produces, got {lw}x{lh} at {leaves_bbox:?} — a larger or differently-placed \
         region means something other than one AO corner moved"
    );

    // --- Scene B: the exemption control. If someone "simplifies" the predicate
    // to a `collision_shapes`-derived full-block check, barrier becomes
    // an occluder and this fires.
    assert!(
        barrier_bbox.is_none(),
        "barrier is exempt in vanilla (its shade-brightness override returns 1.0) but \
         darkened a region at {barrier_bbox:?} — the predicate has been replaced by something \
         that only looks at the collision shape, where barrier IS a full block"
    );
    for (label, byte) in corner_samples(&barrier_frame) {
        assert!(byte >= 250, "{label} must be full-bright for a barrier occluder, got {byte}");
    }

    // --- Scene C: the mirror control. This is the frame that changed sign with
    // this fix: ~204 before, 255 after.
    assert!(
        culling_bbox.is_none(),
        "the AO term must no longer consult occludes_at: this scene's diagonal occludes for \
         culling but not for shade brightness, and it darkened a region at {culling_bbox:?}"
    );
    for (label, byte) in corner_samples(&culling_only) {
        assert!(
            byte >= 250,
            "{label} must be full-bright when only occludes_at (not ao_occludes_at) reports \
             the diagonal as occluding, got {byte} — the AO term is reading the culling \
             predicate again"
        );
    }
}
