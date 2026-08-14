//! Offscreen gate: the GUI atlas ([`GuiAtlas`]) must not bleed a neighbouring
//! sprite's texels into a minified, `Linear`-filtered edge sample — the same
//! defect `atlas_mip_edge_bleed_gate.rs` guards for the block atlas (issue
//! #575), but reached a different way here because the GUI atlas carries no
//! mip pyramid at all.
//!
//! ## Why this gate exists
//!
//! `GuiAtlas::build_with_extras` stitches every `gui/sprites/**` PNG with a
//! deliberately mip-free [`AtlasBuilder`] — no HUD element is ever drawn far
//! enough away to need a mip chain. That fact was read, in the code this
//! module used to ship, as "the HUD only ever magnifies", and the atlas was
//! packed with **zero gutter** on that premise.
//!
//! The premise is false on any resource pack denser than the 16x baseline. A
//! HUD element (the inventory slot highlight, for one) is drawn at a fixed
//! *declared* pixel size regardless of pack resolution, while a sprite's
//! *native* pixel size scales with the pack — a 32x pack ships a sprite at
//! 2x the 16x baseline. So the draw is **minified**, not magnified, and
//! `GpuAtlas::from_atlas`'s sampler is `min_filter: Linear` for every atlas
//! it uploads, GUI included. A zero-gutter, minified, `Linear` sample at a
//! sprite's own edge blends in whatever the bin packer placed next to it —
//! exactly the "thin strip of a weird texture" the owner reported hovering
//! creative inventory slots. No mip level is involved: with only one level
//! to sample, the worst case is a single 2×2 bilinear tap straddling the
//! seam, which is why one texel of padding ([`GUI_ATLAS_PADDING`] in
//! `gui_atlas.rs`, mirrored here as a literal) is enough.
//!
//! ## The gate and its controls
//!
//! Two atlases are built from the same two adjacent, distinctly-coloured
//! (red, blue) sprites, sized the way a 32x pack would ship a 16x-declared
//! 8×8 HUD icon (native 16×16): one with the fix's padding, one without (the
//! historical bug, reproduced in-test as the negative control). Both go
//! through the exact production upload path ([`GpuAtlas::from_atlas`]).
//!
//! * **Primary (minified, padded)** — sample the red sprite's right edge UV
//!   through a `Linear`-filtered view of the atlas's only level. Bilinear
//!   filtering at the exact edge UV lands the sample half in the sprite,
//!   half past it — the worst case a declared-size-smaller-than-native draw
//!   produces. Assert the result reads as its own sprite's colour, not a
//!   blend toward blue.
//! * **Negative control (minified, unpadded)** — the identical sample
//!   against the zero-gutter atlas. This must show the blue contamination
//!   the primary assertion claims is absent, proving the gate is sensitive
//!   (`CLAUDE.md`'s "assertions of an absence need a control proving the
//!   detector works") — and this control is **run and observed to fail**
//!   before the fix, not merely described.
//! * **Close-range control (magnified, unpadded)** — the same unpadded atlas,
//!   sampled well inside the sprite through a `Nearest`-filtered view
//!   (matching the real sampler's `mag_filter: Nearest`). Must show no
//!   artefact even on the buggy atlas, matching a real close-up (magnified)
//!   HUD draw, which this bug never touches.
//!
//! This is a per-texel effect at a sprite seam, so a vertex-count probe
//! cannot see it — the assertions read back real rasterised pixels from a
//! real render.
//!
//! `#[ignore]`d because it needs a real GPU adapter; run explicitly:
//! `cargo test -p lodestone-render --test gui_atlas_edge_bleed_gate -- --ignored --nocapture`.

use lodestone_assets::{AtlasBuilder, Image, ResourceLocation};
use lodestone_render::{
    GpuAtlas, GpuModelMesh, ModelMesh, ModelPipeline, ModelVertex, model_anim_buffer,
    model_palette_buffer, model_shared_camera_buffer, section_origin_buffer,
};

const W: u32 = 4;
const H: u32 = 4;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Mirrors `gui_atlas::GUI_ATLAS_PADDING`, duplicated as a literal here so
/// this gate does not silently track a future change to that constant
/// without the test author noticing.
const GUI_ATLAS_PADDING: u32 = 1;
/// A 16×16 native sprite is what a 32x pack ships for an 8×8-declared HUD
/// icon — real pixels exceeding the declared baseline is exactly the
/// minification condition this gate exists to catch.
const NATIVE: u32 = 16;

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
                label: Some("gui_atlas_edge_bleed_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Image {
    Image {
        width,
        height,
        rgba: rgba.repeat((width * height) as usize),
    }
}

fn loc(path: &str) -> ResourceLocation {
    ResourceLocation::parse(&format!("minecraft:gui/sprites/{path}")).expect("valid location")
}

/// Two adjacent opaque `NATIVE`×`NATIVE` sprites — pure red and pure blue —
/// built either with production's [`GUI_ATLAS_PADDING`] gutter or (the
/// historical bug) with none. No mip levels requested either way, matching
/// `GuiAtlas::build_with_extras` exactly.
fn two_sprite_atlas(padded: bool) -> lodestone_assets::Atlas {
    let mut builder = AtlasBuilder::new();
    if padded {
        builder = builder.with_padding(GUI_ATLAS_PADDING);
    }
    builder.add_texture(loc("a_red"), solid(NATIVE, NATIVE, [255, 0, 0, 255]), None);
    builder.add_texture(loc("b_blue"), solid(NATIVE, NATIVE, [0, 0, 255, 255]), None);
    builder.build().expect("two-sprite atlas builds")
}

/// A quad whose every vertex carries the *same* UV, so every pixel of the
/// render target samples exactly one atlas coordinate regardless of target
/// size — no screen-space UV derivative to fight.
fn quad_const_uv(u: f32, v: f32) -> ModelMesh {
    let vtx = |x: f32, y: f32| ModelVertex {
        position: [x, y, 0.5],
        uv: [u, v],
        ao: 1.0,
        light: 0xFF,
        tint: 255, // untinted
        anim: 0,
        cutout_bypass: 1, // paint the raw sampled texel, alpha included
        tint_rgb_override: [0, 0, 0, 0],
    };
    ModelMesh {
        vertices: vec![vtx(-1.0, -1.0), vtx(1.0, -1.0), vtx(1.0, 1.0), vtx(-1.0, 1.0)],
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

/// Render `mesh` sampling `view` through a custom sampler (bypassing
/// [`GpuAtlas`]'s own production sampler so the filter mode is under the
/// test's exact control), and read back the centre pixel.
fn render_center_custom_sampler(
    gpu: &Gpu,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    mesh: &ModelMesh,
) -> (u8, u8, u8, u8) {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    let atlas_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("gui-edge-bleed-atlas-bg"),
        layout: &pipeline.atlas_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });

    let palette = vec![[1.0_f32, 1.0, 1.0, 1.0]; 256];
    let palette_buffer = model_palette_buffer(device, &palette);
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    let anim_buffer = model_anim_buffer(device, &[]);
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);

    let cam_buffer = model_shared_camera_buffer(device, glam::Mat4::IDENTITY.to_cols_array_2d());
    let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);
    let gpu_mesh = GpuModelMesh::upload(device, mesh).expect("non-empty quad");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gui edge bleed target"),
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
            label: Some("gui atlas edge bleed gate"),
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

    let padded_row = (W * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded_row) * u64::from(H),
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
                bytes_per_row: Some(padded_row),
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
    let i = (cy * padded_row + cx * 4) as usize;
    (data[i], data[i + 1], data[i + 2], data[i + 3])
}

/// Build a `GpuAtlas` through the exact production path
/// ([`GpuAtlas::from_atlas`]) and return it plus its (only) level's view,
/// with an explicit sampler bypassing production's own.
fn level_view(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &lodestone_assets::Atlas,
    filter: wgpu::FilterMode,
) -> (GpuAtlas, wgpu::TextureView, wgpu::Sampler) {
    let gpu_atlas = GpuAtlas::from_atlas(device, queue, atlas);
    let view = gpu_atlas
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("gui-edge-bleed-gate-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: filter,
        min_filter: filter,
        ..Default::default()
    });
    (gpu_atlas, view, sampler)
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to watch the negative control fail"]
fn gui_atlas_edge_sample_stays_on_its_own_sprite() {
    let Some(gpu) = setup() else {
        panic!(
            "gui_atlas_edge_bleed_gate: no GPU adapter. This test is #[ignore]d, so running it \
             is an explicit request for a real GPU frame — a headless CI box has none and should \
             not run it."
        );
    };

    let padded_atlas = two_sprite_atlas(true);
    let unpadded_atlas = two_sprite_atlas(false);
    let red = padded_atlas.sprite(&loc("a_red")).expect("red sprite");
    let red_unpadded = unpadded_atlas.sprite(&loc("a_red")).expect("red sprite");
    // The atlas is repacked (different cell sizes) between the two builds, so
    // each atlas's own sprite record is queried independently rather than
    // assuming identical placement.
    let v_mid_padded = (red.uv_min[1] + red.uv_max[1]) / 2.0;
    let v_mid_unpadded = (red_unpadded.uv_min[1] + red_unpadded.uv_max[1]) / 2.0;

    // --- Primary: minified (Linear at the atlas's single level), padded. ----
    // Sampling exactly at `uv_max[0]` (the sprite's own right edge) is the
    // worst case a declared-size-smaller-than-native GUI draw produces:
    // bilinear interpolation lands half in the sprite, half past it.
    let (_atlas_a, view_a, sampler_a) =
        level_view(&gpu.device, &gpu.queue, &padded_atlas, wgpu::FilterMode::Linear);
    let (r, g, b, a) = render_center_custom_sampler(
        &gpu,
        &view_a,
        &sampler_a,
        &quad_const_uv(red.uv_max[0], v_mid_padded),
    );
    println!("primary (minified, padded):   rgba=({r},{g},{b},{a})  expect ~pure opaque red");
    assert!(
        b < 40,
        "a minified (Linear) sample at the padded red sprite's own edge must not pick up the \
         neighbouring blue sprite: got blue={b}, rgba=({r},{g},{b},{a})"
    );
    assert!(
        a > 240,
        "the padded edge sample must not fade toward transparent either (alpha={a} implies the \
         gutter is zero-filled/transparent instead of extruded from the sprite's own opaque \
         edge): got rgba=({r},{g},{b},{a})"
    );

    // --- Negative control: minified, Linear, UNPADDED atlas. ----------------
    // Same exact sample against the historical (zero-gutter) configuration —
    // this must show the bleed the primary assertion above claims is absent,
    // proving the gate is actually sensitive to the bug it exists to catch.
    let (_atlas_b, view_b, sampler_b) = level_view(
        &gpu.device,
        &gpu.queue,
        &unpadded_atlas,
        wgpu::FilterMode::Linear,
    );
    let (r2, g2, b2, _a2) = render_center_custom_sampler(
        &gpu,
        &view_b,
        &sampler_b,
        &quad_const_uv(red_unpadded.uv_max[0], v_mid_unpadded),
    );
    println!(
        "control (minified, UNPADDED): rgb=({r2},{g2},{b2})  expect strong blue contamination \
         -- this is the strip the owner reported hovering creative inventory slots"
    );
    assert!(
        b2 > 100,
        "negative control failed: the zero-gutter atlas must show clear blue contamination at \
         this exact edge sample, or this gate proves nothing about what padding fixes. got \
         rgb=({r2},{g2},{b2})"
    );

    // --- Close-range control: magnified, Nearest, UNPADDED atlas. -----------
    // Same buggy atlas, sampled the way a close-up (magnified) HUD draw
    // actually samples: `Nearest` never interpolates across a boundary, and
    // the UV sits a hair inside the sprite (not exactly on the seam). Must
    // show no artefact even though the atlas itself is the buggy one.
    let (_atlas_c, view_c, sampler_c) = level_view(
        &gpu.device,
        &gpu.queue,
        &unpadded_atlas,
        wgpu::FilterMode::Nearest,
    );
    let uv_span = red_unpadded.uv_max[0] - red_unpadded.uv_min[0];
    let texel_w = uv_span / red_unpadded.width as f32;
    let close_u = red_unpadded.uv_max[0] - (texel_w * 0.1).max(1e-6);
    let (r3, g3, b3, _a3) = render_center_custom_sampler(
        &gpu,
        &view_c,
        &sampler_c,
        &quad_const_uv(close_u, v_mid_unpadded),
    );
    println!(
        "close-range control (magnified, UNPADDED, Nearest): rgb=({r3},{g3},{b3})  expect pure \
         red -- no artefact up close, even on the buggy atlas"
    );
    assert!(
        b3 < 10,
        "a magnified (Nearest) sample just inside the buggy atlas's red sprite must show no \
         blue at all: got rgb=({r3},{g3},{b3})"
    );
    assert!(
        r3 > 240,
        "a magnified Nearest sample just inside the sprite should read as essentially pure red: \
         got rgb=({r3},{g3},{b3})"
    );
}
