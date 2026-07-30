//! Offscreen gate for **per-face shade colour fidelity on the model path**: two
//! faces of the same textured, untinted quad — one `Up` (vanilla shade `1.0`),
//! one `East` (vanilla shade `0.6`) — must render at the **gamma-space**
//! ratio (~`0.6`), not the linear-space one (~`0.794`).
//!
//! ## Why this gate exists
//!
//! This is [`tint_gamma_gate`](./tint_gamma_gate.rs)'s sibling: that gate proves
//! the *tint* half of `srgb_to_linear(linear_to_srgb(tex.rgb) * tint_col * shade)`
//! survives the sRGB round-trip; this one proves the *shade* half does, on the
//! path that actually carries non-trivial shade values.
//!
//! That distinction matters because there are two meshers.
//! [`crate::mesh`]'s demo/offline path (`mesh_simple`, driving
//! `cargo run --bin lodestone -- --headless`) computes `ao` from corner
//! occlusion only — `1.0` unoccluded, `0.2` occluded — and never touches
//! [`face_shade`](lodestone_render::models) at all. [`mesh_models`] (this
//! crate's model path, which live server terrain actually goes through via
//! `lodestone-shell`'s mesher) is the one that calls `face_shade` and folds
//! vanilla's per-face constants (`Up 1.0, Down 0.5, North/South 0.8,
//! East/West 0.6`) into the `ao` slot the shader reads as `shade`.
//!
//! A colour-space fix to the `shade` multiply was once verified only against
//! `--headless`'s single-orientation demo scene and reported "byte-for-byte
//! identical" / "numerically inert" — true for that scene, false for the model
//! path. This gate renders **through [`mesh_models`], not `mesh_simple`**, and
//! puts two different `Direction`s in the same frame so a regression to
//! linear-space shading cannot hide behind an all-`Up` (or otherwise
//! single-orientation) test scene.
//!
//! ## The gate and its negative control
//!
//! Render a mid-grey, untinted quad twice through the real [`ModelPipeline`] +
//! [`mesh_models`] — once with an `Up`-facing [`BakedQuad`] (`shade = 1.0`),
//! once with an `East`-facing one (`shade = 0.6`), same texture, same
//! (absent) tint — and read back each frame's centre pixel. The **negative
//! control**, computed and printed alongside the gate, is the linear-space
//! ratio a regressed shader would produce (~`0.794`): the two predictions are
//! far enough apart (`0.6` vs `0.794`) that a `0.55..=0.65` acceptance band
//! admits the correct value and rejects the regressed one with room to spare.
//!
//! `#[ignore]`d because it needs a real GPU adapter; run explicitly:
//! `cargo test -p lodestone-render --test model_shade_gamma_gate -- --ignored --nocapture`.

use lodestone_assets::{BakedQuad, Direction};
use lodestone_render::{
    GpuAtlas, GpuModelMesh, ModelPipeline, ModelSectionView, mesh_models, model_anim_buffer,
    model_palette_buffer, model_shared_camera_buffer, section_origin_buffer,
};

const W: u32 = 64;
const H: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Predicted displayed-byte ratio (`East` / `Up`) if the shade multiply happens
/// in **gamma space** — `srgb_to_linear(linear_to_srgb(tex.rgb) * shade)`, the
/// current, correct shader. `face_shade` gives `East` `0.6` and `Up` `1.0`; a
/// gamma-space multiply lands the *sRGB byte* directly on that ratio (the
/// `linear_to_srgb`/`srgb_to_linear` round trip cancels around the multiply,
/// and the render target's own sRGB encode on write is the identity on an
/// already-sRGB-encoded value), so the displayed ratio is just `0.6 / 1.0`.
const GAMMA_SPACE_RATIO: f32 = 0.6;

/// Predicted displayed-byte ratio if the multiply instead happens in **linear
/// space** — `tex.rgb * shade` with no round-trip, the pre-fix / regressed
/// form. The sRGB re-encode after a *linear* multiply is a concave (`^1/2.4`)
/// curve, which pulls the ratio toward `1.0`: a mid-grey (byte `128`) texel at
/// `shade = 0.6` lands around `0.794`, not `0.6`. This is the "washed out"
/// look Fix B corrects — a `0.6` face reads roughly a third of the way back
/// toward full brightness.
const LINEAR_SPACE_RATIO: f32 = 0.794;

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
                label: Some("model_shade_gamma_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// A full-frame [`BakedQuad`] facing `dir`, carrying no tint (`tint_index:
/// None`) so only `face_shade`'s per-direction constant — not a palette colour
/// — can move the displayed pixel. Positions are a full-frame clip-space quad
/// under the identity camera below (exactly [`tint_gamma_gate`](./tint_gamma_gate.rs)'s
/// `quad()` helper); `direction` is what `face_shade` reads, and is otherwise
/// independent of the (irrelevant here) geometric positions, same as this
/// crate's own `cube_face` test helper in `src/models.rs`.
fn face_quad(dir: Direction) -> BakedQuad {
    BakedQuad {
        positions: [
            [-1.0, -1.0, 0.5],
            [1.0, -1.0, 0.5],
            [1.0, 1.0, 0.5],
            [-1.0, 1.0, 0.5],
        ],
        uvs: [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
        direction: dir,
        cullface: None,
        tint_index: None,
        shade: true,
        layer: 0,
        anim: 0,
    }
}

/// A [`ModelSectionView`] with a single populated cell `(0, 0, 0)` holding one
/// quad, never occluded. Mirrors the `OneBlock` test fixture in
/// `src/models.rs`.
struct OneQuad {
    quads: Vec<BakedQuad>,
}

impl ModelSectionView for OneQuad {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        if (x, y, z) == (0, 0, 0) {
            &self.quads
        } else {
            &[]
        }
    }
    fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

/// Mesh a single `dir`-facing quad through the real [`mesh_models`] (the model
/// path, not `mesh_simple`), render it through [`ModelPipeline`], and read
/// back the centre pixel's red channel. The texture is neutral mid-grey and
/// the quad is untinted, so R, G and B all move together and the red channel
/// alone is a faithful luma proxy.
fn render_face_luma(gpu: &Gpu, dir: Direction) -> u8 {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    // Neutral mid-grey texture: any brightness change in the readback comes
    // from `face_shade`, not the texel.
    let atlas = GpuAtlas::from_rgba(device, queue, 4, 4, &[128, 128, 128, 255].repeat(16), &[]);
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);

    // All-white palette: tint_index is None on the quad (-> slot 255, also
    // untouched), so this buffer only exists to satisfy the pipeline's bind
    // group layout and never contributes colour.
    let palette = vec![[1.0_f32, 1.0, 1.0, 1.0]; 256];
    let palette_buffer = model_palette_buffer(device, &palette);
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    let anim_buffer = model_anim_buffer(device, &[]);
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);

    let cam_buffer = model_shared_camera_buffer(device, glam::Mat4::IDENTITY.to_cols_array_2d());
    let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);

    let view = OneQuad {
        quads: vec![face_quad(dir)],
    };
    let mesh = mesh_models(&view);
    assert_eq!(
        mesh.quad_count(),
        1,
        "expected exactly one quad meshed for direction {dir:?} — the model path \
         (mesh_models) must have emitted the single face this gate placed"
    );
    let gpu_mesh = GpuModelMesh::upload(device, &mesh).expect("non-empty quad");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("model shade target"),
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
            label: Some("model shade gamma gate"),
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

    let (cx, cy) = (W / 2, H / 2);
    let i = (cy * padded + cx * 4) as usize;
    data[i]
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fail"]
fn model_path_face_shade_renders_at_gamma_space_ratio_not_linear() {
    let Some(gpu) = setup() else {
        panic!(
            "model_shade_gamma_gate: no GPU adapter. This test is #[ignore]d, so running it is \
             an explicit request for a real GPU frame — a headless CI box has none and should \
             not run it."
        );
    };

    let up = render_face_luma(&gpu, Direction::Up); // face_shade = 1.0
    let east = render_face_luma(&gpu, Direction::East); // face_shade = 0.6
    let ratio = f32::from(east) / f32::from(up);

    println!("=== MODEL PATH FACE-SHADE GAMMA GATE ===");
    println!("Up face   (shade 1.0): byte = {up}");
    println!("East face (shade 0.6): byte = {east}");
    println!("measured ratio (East/Up)      = {ratio:.3}");
    println!("gamma-space prediction  (fix) = {GAMMA_SPACE_RATIO:.3}");
    println!("linear-space prediction (bug) = {LINEAR_SPACE_RATIO:.3}");
    println!(
        "negative control: reverting the shader's `srgb_to_linear(linear_to_srgb(tex.rgb) * \
         tint_col * shade)` to the linear-space `tex.rgb * tint_col * shade` renders this same \
         two-face scene at ratio ~{LINEAR_SPACE_RATIO:.3}, which falls outside the 0.55..=0.65 \
         band below and fails the assertion."
    );

    // The two predictions (0.6 vs 0.794) are ~0.19 apart; a 0.10-wide band
    // centred on the gamma-space value admits it with slack while rejecting
    // the linear-space value with more than double the band's half-width to
    // spare.
    assert!(
        (0.55..=0.65).contains(&ratio),
        "East/Up face-shade ratio must land near the gamma-space prediction {GAMMA_SPACE_RATIO:.3}, \
         got {ratio:.3} (Up={up}, East={east}); a value near {LINEAR_SPACE_RATIO:.3} means the \
         shade multiply is happening in linear space (the regression this gate guards) — this is \
         exactly the defect a single-orientation scene (e.g. `--headless`'s mesh_simple demo, \
         which never calls face_shade at all) cannot detect, because it never renders two \
         differently-shaded faces of the same texture to compare"
    );
}
