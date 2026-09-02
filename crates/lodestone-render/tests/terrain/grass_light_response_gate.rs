//! Offscreen gate for **"grass and leaves stay bright at night"**: every block
//! class the model path renders must respond to sky light by the *same* factor.
//!
//! ## The report, and why the obvious candidates were wrong
//!
//! A player reported grass, tall grass and leaves reading too bright — and
//! staying bright at midnight — while stone and dirt looked right. Three
//! candidates were proposed and all three are refuted by measurement rather than
//! by argument (the numbers are printed by this gate's sibling assertions):
//!
//! * **"Cross plants bypass directional shading."** They do, and so does
//!   vanilla: `block/cross` and `block/tinted_cross` both mark every element
//!   `"shade": false` in the real 26.2 jar, so `face_shade` returning `1.0` for a
//!   grass blade *is* the vanilla behaviour. Shading them would be the defect.
//! * **"The tint is missing or doubled."** It is neither. Baked against the real
//!   jar, `grass_block`'s top and `short_grass`/`tall_grass`'s blades all carry
//!   palette slot 0 = `#91BD59`, and `oak_leaves` carries slot 1 = `#77AB2F` —
//!   distinct slots, both applied exactly once.
//! * **"The cutout layer takes a different pipeline."** It does not. The shell's
//!   live terrain path builds one [`ModelPipeline`] (`Solid`) for all opaque
//!   model geometry; `RenderLayer::Cutout` never selects a second pipeline, and
//!   `for_layer` returns a byte-identical descriptor for `Solid` and `Cutout`
//!   anyway.
//!
//! The **actual** cause of the report was found elsewhere and fixed in
//! `fda948f`: `mesh_models` lit every block from its *own* cell, and the light
//! engine stores sky `0` inside an opaque solid, so 99.49% of solid cells
//! rendered at the shader's dark floor while the 0.51% with sky > 0 — precisely
//! the non-opaque set: grass, leaves, plants, fences — rendered at `1.0`. Each
//! quad is now lit from the cell its face opens into.
//!
//! What this file measures is the surviving *invariant* that fix has to keep
//! true, plus one genuine defect the same investigation turned up.
//!
//! 1. [`tinted_surfaces_respond_to_sky_light_exactly_as_stone_does`] — the
//!    **light multiply reaches the tinted population at all**, *and* does so on
//!    vanilla's curve. The model shader folds sky/block light into `shade` via
//!    `lightmap.fsh`'s `get_brightness` plus `notGamma`; if a tinted or cutout
//!    quad bypassed that term it would render at its daylight value regardless of
//!    the hour, which is the reported symptom stated as a measurement. This is a
//!    shader/mesh property, deliberately independent of *which cell* the light is
//!    sampled from — the light byte is supplied by the harness, so `fda948f`'s
//!    sampling rule is `crates/lodestone-shell`'s to gate.
//! 2. [`the_grass_block_side_overlay_survives_the_depth_test`] — a **coplanar
//!    overlay must win over the element beneath it**. Unrelated to light, found
//!    while auditing why grass looked wrong, and real: `grass_block` bakes 10
//!    quads, 4 of them tinted `#overlay` faces sitting at exactly the base
//!    cube's coordinates. A strict "nearer wins" comparison rejects all four.
//! 3. [`unlit_faces_reach_vanillas_ambient_floor_and_not_the_retired_ramps`] —
//!    the retired ramp's `0.2` floor is **gone**, replaced by vanilla's own
//!    `AmbientColor` floor of `0.0935` rather than by pure black, on real baked
//!    geometry including the tinted classes, where a tint applied outside the
//!    light multiply would be the only way to survive it.
//!
//! ## The measurement: by location, not by frame average
//!
//! Four *separate* populations are rendered and read back independently — a
//! stone top (untinted control, the surface the player says is correct), a
//! `grass_block` top (tinted, `shade` 1.0), an `oak_leaves` top (tinted,
//! different palette slot), and a `short_grass` blade (tinted, `shade: false`,
//! no `cullface`). Each is rendered twice, at sky 15 and sky 7. Averaging them
//! into one number would merge exactly the populations the report distinguishes.
//!
//! Geometry, UVs, tint indices, the stitched atlas and the palette are all the
//! **real** baked ones from the vanilla jar, and the mesh is produced by the
//! real [`mesh_models`]. Only the quad's screen positions (a full-frame quad, so
//! the centre pixel is guaranteed covered) and the light byte are the harness's.
//! A synthetic quad would be the *world* species of vacuous test: it could not
//! exercise the tint palette, the cutout alpha, or `shade: false`.
//!
//! ## The three predictions, named
//!
//! [`RATIO_LIGHT_APPLIED`] (`0.423`) is what a correct build produces at sky 7
//! against sky 15, re-derived from `lightmap.fsh`; [`RATIO_AMBIENT_FREE`]
//! (`0.363`) is the same chain with `AmbientColor` wrongly dropped;
//! [`RATIO_OLD_RAMP`] (`0.573`)
//! is what the retired `0.2 + 0.8 * l` ramp produced for the same pair; and
//! [`RATIO_LIGHT_IGNORED`] (`1.0`) is what the reported bug would produce for the
//! tinted classes. Both multiplies land in gamma space against an sRGB target, so
//! the displayed byte ratio *is* the light-term ratio. The acceptance band cannot
//! admit more than one of the three, and that exclusion is **asserted** in the
//! test body — which is what an "is it darker?" assertion cannot do, since it
//! passes under any build that darkens at all.
//!
//! The second measurement point is sky **7**, not sky 0, because the interior of
//! the curve is the only place two candidate curves can be distinguished — they
//! meet exactly at both endpoints. (A sky-0 frame would once have been useless for
//! a different reason: while `AmbientColor` was wrongly dropped, light 0 rendered
//! pure black, so every ratio against it was `0.000` under any darkening build,
//! including one that draws nothing. Vanilla's real `0.0935` floor restores it as
//! a usable measurement, which is what assertion 3 now uses.)
//!
//! ## Negative control
//!
//! [`light_response_predicate_rejects_a_light_ignoring_build`] runs the *same*
//! predicate over a deliberately light-ignoring render (both frames drawn at sky
//! 15, i.e. the light byte never varying) and asserts the predicate rejects it.
//! It is executed, not described.
//!
//! `#[ignore]`d: needs a GPU adapter **and** a fetched `client.jar`. Run with
//! `cargo test -p lodestone-render --test grass_light_response_gate -- --ignored --nocapture`.

use std::collections::BTreeMap;

use lodestone_assets::{BakedQuad, Direction, ResourceManager, ZipSource};
use lodestone_model::{BlockStateRegistry, Identifier};
use lodestone_render::{
    BlockModels, GpuAtlas, GpuModelMesh, ModelPipeline, ModelSectionView, blocks_json_registry,
    mesh_models, model_anim_buffer, model_palette_buffer, model_shared_camera_buffer,
    section_origin_buffer,
};

#[path = "../gate_harness/mod.rs"]
mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

const W: u32 = 64;
const H: u32 = 64;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Packed light byte for a face open to full daylight: sky `15`, block `0`. The
/// model shader's light term evaluates to exactly `1.0` — `get_brightness(1)` is
/// `1` and `notGamma(1)` is `1`, so this endpoint is unmoved by the curve change.
const SKY_FULL: u8 = 0xF0;
/// Packed light byte for a **dim** face: sky `7`, block `0`. This is the second
/// measurement point because it sits where the candidate curves disagree most,
/// not because light 0 is unusable — vanilla's `AmbientColor` floor keeps light 0
/// at `0.0935`, so a ratio against it is meaningful.
/// [`unlit_faces_reach_vanillas_ambient_floor_and_not_the_retired_ramps`] is where
/// light 0 is asserted.
const SKY_DIM: u8 = 0x70;
/// Packed light byte for a face in total darkness: sky `0`, block `0`. Vanilla's
/// `get_brightness(0)` is `0`, but its `AmbientColor` seed (`0x0A0A0A` in the
/// overworld) renders this at `0.0935` of daylight — not black. The retired
/// `0.2 + 0.8 * l` ramp floored it at `0.2`.
const SKY_NONE: u8 = 0x00;

/// **The correct build's prediction**, `light_term(SKY_DIM) / light_term(SKY_FULL)`,
/// re-derived from `assets/minecraft/shaders/core/lightmap.fsh` in the real 26.2
/// `client.jar` rather than from this crate's output:
///
/// ```text
/// get_brightness(7/15) = (7/15) / (4 - 3 * 7/15) = 0.179487
/// + AmbientColor       = 10 / 255                = 0.039216
/// notGamma(c)          = 1 - (1 - c)^4            (grey: no division by max)
/// mix(c, notGamma(c), BrightnessFactor = 0.5)     = 0.423067
/// ```
///
/// against a full-light denominator of exactly `1.0` (the ambient term clamps away
/// up there). Because the shader's light multiply happens in gamma space
/// (`srgb_to_linear(linear_to_srgb(tex) * tint * shade)`) and the target is an
/// sRGB surface, the transfer functions cancel and the *displayed byte* ratio is
/// the light-term ratio itself — no `^1/2.4` softening.
const RATIO_LIGHT_APPLIED: f32 = 0.423_07;

/// **The ambient-free prediction**, i.e. vanilla's chain with `AmbientColor`
/// dropped as a believed no-op — what this gate asserted before
/// `DimensionTypes.java:36` was read. `0.363117`. It is only `0.06` from the
/// correct value, which is why [`BAND`] has to be tight.
const RATIO_AMBIENT_FREE: f32 = 0.363_12;

/// **The retired linear ramp's prediction** for the same pair:
/// `(0.2 + 0.8 * 7/15) / 1.0 = 0.573333`. The band must exclude this, or the gate
/// cannot tell vanilla's curve from the ramp it replaced.
const RATIO_OLD_RAMP: f32 = 0.573_33;

/// **The reported bug's prediction.** If the light term never reaches a class of
/// quads, that class renders at its daylight value at every hour, so the dim
/// frame equals the sky-15 frame and the ratio is `1.0`.
const RATIO_LIGHT_IGNORED: f32 = 1.0;

/// Acceptance band around [`RATIO_LIGHT_APPLIED`]. Deliberately narrow relative
/// to the gaps to *all three* wrong predictions: any band that admitted
/// [`RATIO_AMBIENT_FREE`], [`RATIO_OLD_RAMP`] or [`RATIO_LIGHT_IGNORED`] would be
/// the *assertion* species of vacuous test, and every exclusion is asserted below
/// rather than described. [`RATIO_AMBIENT_FREE`] is the binding constraint at
/// `0.06` away — the widest this band can be is roughly half that.
const BAND: std::ops::RangeInclusive<f32> = 0.406..=0.440;

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
                label: Some("grass_light_response_gate device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    })
}

/// The first state id whose block matches `block` and whose properties are a
/// superset of `want`.
fn find_state(reg: &dyn BlockStateRegistry, block: &str, want: &[(&str, &str)]) -> Option<u32> {
    let ident: Identifier = block.parse().ok()?;
    let wanted: BTreeMap<&str, &str> = want.iter().copied().collect();
    (0..reg.state_count()).find(|&id| {
        let Some(state) = reg.resolve(id) else {
            return false;
        };
        if *state.block != ident {
            return false;
        }
        wanted
            .iter()
            .all(|(k, v)| state.properties.get(*k).map(String::as_str) == Some(*v))
    })
}

/// A one-cell [`ModelSectionView`] holding a single real baked quad at
/// `(0, 0, 0)`, never occluded, answering `light` for every face.
///
/// Deliberately a real view driven through the real [`mesh_models`]: that is
/// what folds `face_shade` into the `ao` slot and the palette index into the
/// `tint` byte, so a regression in either is inside this gate's blast radius.
struct OneQuad {
    quads: Vec<BakedQuad>,
    light: u8,
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
    fn face_light_at(&self, _x: usize, _y: usize, _z: usize, _dir: Direction) -> u8 {
        self.light
    }
    // Smooth lighting ([`quad_corner_sample`]) samples the four AO/light
    // corners around the cell each face opens into, independently of
    // `face_light_at`. Left at the trait's `0xF0` default, this view claimed a
    // face lit at `SKY_NONE` (0x00) while every corner around it read
    // full-bright — self-inconsistent input, since `occludes_at` is also
    // hardcoded `false` above so the corner substitution that would otherwise
    // paper over a mismatch (`smoothBlend`, centre light > threshold) never
    // fires. The blend then averaged centre 0 with three full-bright corners
    // to ~11/15, and the harness's own `v.light == light` sanity assertion
    // (this file, `render_frame`) caught it as a panic rather than silently
    // producing a wrong ratio. A lone synthetic quad with no real
    // neighbourhood has no "different" light to report at its corners, so the
    // only self-consistent answer is the same light the view claims for the
    // face itself.
    fn corner_light_at(&self, _x: i32, _y: i32, _z: i32) -> u8 {
        self.light
    }
}

/// Re-seat a real baked quad onto a full-frame clip-space rectangle, keeping
/// **everything else** — UVs, `direction`, `shade`, `tint_index`, `anim` —
/// exactly as baked. The camera is the identity and the block sits at
/// `(0, 0, 0)`, so `mesh_models`' block-origin offset is zero and these
/// positions reach the rasteriser unchanged.
fn full_frame(mut q: BakedQuad) -> BakedQuad {
    q.positions = [
        [-1.0, -1.0, 0.5],
        [1.0, -1.0, 0.5],
        [1.0, 1.0, 0.5],
        [-1.0, 1.0, 0.5],
    ];
    // `cullface` would ask the (absent) neighbour; `occludes_at` already answers
    // false, but clearing it keeps the harness independent of that.
    q.cullface = None;
    q
}

/// Render one real baked quad at one light level through the real model
/// pipeline against the real stitched atlas and palette, and read back the
/// centre pixel.
fn render_center(gpu: &Gpu, models: &BlockModels, quad: &BakedQuad, light: u8) -> (u8, u8, u8) {
    let frame = render_frame(gpu, models, vec![full_frame(quad.clone())], light);
    frame[(H / 2 * W + W / 2) as usize]
}

/// Render already-seated quads (drawn in the order given) and read back the
/// whole `W × H` frame, row-major. Callers pick their probe **by location**:
/// several of these sprites are cutouts whose centre texel is transparent, so a
/// fixed centre sample would silently measure the clear colour.
#[allow(clippy::too_many_lines)]
fn render_frame(
    gpu: &Gpu,
    models: &BlockModels,
    quads: Vec<BakedQuad>,
    light: u8,
) -> Vec<(u8, u8, u8)> {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    let atlas = GpuAtlas::from_atlas(device, queue, models.atlas());
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);

    let palette_buffer = model_palette_buffer(device, models.tint_palette());
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    // Tick 0 of the real slot table: a static sprite reads the sentinel and an
    // animated one reads its own frame 0, so no quad silently samples garbage.
    let anim_buffer = model_anim_buffer(device, &models.anim_slot_uniforms(0));
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);

    let cam_buffer = model_shared_camera_buffer(device, glam::Mat4::IDENTITY.to_cols_array_2d());
    let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);

    let expected = quads.len();
    let view = OneQuad { quads, light };
    let mesh = mesh_models(&view);
    assert_eq!(
        mesh.quad_count(),
        expected,
        "the model path must have emitted exactly the quads this gate placed"
    );
    assert!(
        mesh.vertices.iter().all(|v| v.light == light),
        "mesh_models must carry the view's light byte onto every vertex"
    );
    let gpu_mesh = GpuModelMesh::upload(device, &mesh).expect("non-empty quad");

    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("grass light target"),
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
            label: Some("grass light response gate"),
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

    let mut out = Vec::with_capacity((W * H) as usize);
    for y in 0..H {
        for x in 0..W {
            let i = (y * padded + x * 4) as usize;
            out.push((data[i], data[i + 1], data[i + 2]));
        }
    }
    out
}

/// Rec. 709 luma of an sRGB byte triple. Used as a single scalar per population;
/// the ratio between two frames of the *same* population is hue-independent, so
/// this cannot launder a colour change into a brightness one.
fn luma(rgb: (u8, u8, u8)) -> f32 {
    0.2126 * f32::from(rgb.0) + 0.7152 * f32::from(rgb.1) + 0.0722 * f32::from(rgb.2)
}

/// One measured population: the block class, the quad it came from, and its two
/// frames.
struct Population {
    label: &'static str,
    tint: Option<i32>,
    shade_flag: bool,
    direction: Direction,
    bright: (u8, u8, u8),
    dark: (u8, u8, u8),
}

impl Population {
    fn ratio(&self) -> f32 {
        luma(self.dark) / luma(self.bright).max(1.0)
    }
    fn report(&self) -> String {
        format!(
            "{:<24} dir={:<6?} shade_flag={:<5} tint={:<8} sky15={:>3?} sky7={:>3?} ratio={:.3}",
            self.label,
            self.direction,
            self.shade_flag,
            self.tint.map_or("none".to_string(), |t| t.to_string()),
            self.bright,
            self.dark,
            self.ratio()
        )
    }
}

/// The predicate under test, factored out so the negative control can run the
/// **same** code over a deliberately broken render.
fn responds_to_light(ratio: f32) -> bool {
    BAND.contains(&ratio)
}

fn build_models() -> (BlockModels, Box<dyn BlockStateRegistry>) {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    let models = BlockModels::build(&manager, &registry).expect("bake block models");
    (models, Box::new(registry))
}

/// Pick a representative quad for a block state: the `Up` face when it has one
/// (grass/leaves/stone tops), otherwise the first quad (a cross plant's blade).
fn representative<'a>(models: &'a BlockModels, state: u32) -> &'a BakedQuad {
    let quads = models.quads(state);
    assert!(!quads.is_empty(), "state {state} baked to no geometry");
    quads
        .iter()
        .find(|q| q.direction == Direction::Up && q.tint_index.is_some())
        .or_else(|| quads.iter().find(|q| q.direction == Direction::Up))
        .unwrap_or(&quads[0])
}

#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn tinted_surfaces_respond_to_sky_light_exactly_as_stone_does() {
    let Some(gpu) = setup() else {
        panic!(
            "grass_light_response_gate: no GPU adapter. This test is #[ignore]d, so running it \
             is an explicit request for a real GPU frame — a headless CI box has none and should \
             not run it."
        );
    };
    let (models, reg) = build_models();

    let cases: [(&'static str, &str, &[(&str, &str)]); 4] = [
        ("stone (control, untinted)", "minecraft:stone", &[]),
        (
            "grass_block top (tinted)",
            "minecraft:grass_block",
            &[("snowy", "false")],
        ),
        ("oak_leaves (tinted)", "minecraft:oak_leaves", &[]),
        ("short_grass blade (tinted)", "minecraft:short_grass", &[]),
    ];

    let mut pops = Vec::new();
    for (label, block, props) in cases {
        let state = find_state(reg.as_ref(), block, props)
            .unwrap_or_else(|| panic!("no state for {block} {props:?}"));
        let quad = representative(&models, state).clone();
        pops.push(Population {
            label,
            tint: quad.tint_index,
            shade_flag: quad.shade,
            direction: quad.direction,
            bright: render_center(&gpu, &models, &quad, SKY_FULL),
            dark: render_center(&gpu, &models, &quad, SKY_DIM),
        });
    }

    println!("=== GRASS / LEAVES SKY-LIGHT RESPONSE (by location, not frame average) ===");
    for p in &pops {
        println!("  {}", p.report());
    }
    println!("  correct-build prediction (vanilla curve) = {RATIO_LIGHT_APPLIED:.3}");
    println!("  retired linear ramp                     = {RATIO_OLD_RAMP:.3}");
    println!("  reported-bug prediction (light ignored)  = {RATIO_LIGHT_IGNORED:.3}");
    println!(
        "  negative control: `light_response_predicate_rejects_a_light_ignoring_build` runs this \
         same predicate over a render whose dim frame is drawn at sky 15, observes ratio ~\
         {RATIO_LIGHT_IGNORED:.3}, and asserts the predicate rejects it."
    );

    // The band must reject *both* wrong hypotheses. Executed, not described.
    assert!(
        responds_to_light(RATIO_LIGHT_APPLIED)
            && !responds_to_light(RATIO_AMBIENT_FREE)
            && !responds_to_light(RATIO_OLD_RAMP)
            && !responds_to_light(RATIO_LIGHT_IGNORED),
        "the band {BAND:?} must admit {RATIO_LIGHT_APPLIED:.3} and reject every wrong \
         prediction ({RATIO_AMBIENT_FREE:.3} for a dropped `AmbientColor`, \
         {RATIO_OLD_RAMP:.3} for the retired `0.2 + 0.8 * l` ramp, \
         {RATIO_LIGHT_IGNORED:.3} for a light-ignoring build), so this \
         gate cannot tell which curve is installed"
    );

    // Anti-vacuity next: the control population must genuinely respond, or an
    // all-broken build would satisfy every "grass matches stone" comparison.
    let stone = &pops[0];
    assert!(
        responds_to_light(stone.ratio()),
        "the untinted control must respond to sky light at ~{RATIO_LIGHT_APPLIED:.3}; got \
         {:.3}. If even stone ignores light, this gate's comparison below is vacuous",
        stone.ratio()
    );

    for p in &pops {
        assert!(
            responds_to_light(p.ratio()),
            "{} must darken with sky light by {RATIO_LIGHT_APPLIED:.3}, got {:.3}. A value near \
             {RATIO_LIGHT_IGNORED:.3} is the reported defect: this population renders at its \
             daylight value at every hour; a value near {RATIO_OLD_RAMP:.3} means the retired \
             linear ramp is back. Note the band {BAND:?} admits only one of the three \
             predictions — an 'it got darker' assertion would pass under all of them",
            p.label,
            p.ratio()
        );
        // ...and it must respond by the *same* factor as the surface the player
        // reports as correct. This is what "too bright relative to blocks" means
        // when stated as a measurement.
        assert!(
            (p.ratio() - stone.ratio()).abs() < 0.03,
            "{} must track the untinted control's light response ({:.3}); got {:.3}",
            p.label,
            stone.ratio(),
            p.ratio()
        );
    }
}

/// Render a *list* of real baked quads, all re-seated onto the same full-frame
/// rectangle at the same depth, in the order given — the coplanar case.
fn render_coplanar(
    gpu: &Gpu,
    models: &BlockModels,
    quads: &[BakedQuad],
    light: u8,
) -> Vec<(u8, u8, u8)> {
    let seated: Vec<BakedQuad> = quads.iter().cloned().map(full_frame).collect();
    render_frame(gpu, models, seated, light)
}

/// `minecraft:grass_block`'s two coplanar `North` quads: the base cube's
/// `#side` (opaque dirt-and-fringe, emitted first) and element 1's `#overlay`
/// (the biome-tinted grass mask, emitted second, at the **same** `from`/`to`).
///
/// Vanilla draws both and the overlay wins, because vanilla's terrain depth
/// function admits an exact tie (`GREATER_THAN_OR_EQUAL`). This renderer is
/// reversed-Z too, so the faithful port is
/// [`lodestone_render::DEPTH_COMPARE_NEARER_OR_EQUAL`]; under a **strict**
/// comparison the second of two exactly-coplanar quads can never pass, which is
/// the failure this gate exists to catch.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn the_grass_block_side_overlay_survives_the_depth_test() {
    let Some(gpu) = setup() else {
        panic!("grass_light_response_gate: no GPU adapter; see the sibling test's message.");
    };
    let (models, reg) = build_models();
    let state = find_state(reg.as_ref(), "minecraft:grass_block", &[("snowy", "false")])
        .expect("grass_block state");

    // The two North-facing quads, in baked order. Anti-vacuity: assert the
    // fixture really is the coplanar pair before measuring it.
    let north: Vec<BakedQuad> = models
        .quads(state)
        .iter()
        .filter(|q| q.direction == Direction::North)
        .cloned()
        .collect();
    assert_eq!(
        north.len(),
        2,
        "grass_block must bake two coplanar North quads (base #side, then #overlay)"
    );
    assert!(
        north[0].tint_index.is_none() && north[1].tint_index.is_some(),
        "the first North quad must be the untinted #side and the second the tinted #overlay; \
         got {:?} then {:?}",
        north[0].tint_index,
        north[1].tint_index
    );

    let base_only = render_coplanar(&gpu, &models, &north[..1], SKY_FULL);
    let overlay_only = render_coplanar(&gpu, &models, &north[1..], SKY_FULL);
    let both = render_coplanar(&gpu, &models, &north, SKY_FULL);

    // **By location, not by frame average.** `grass_block_side_overlay` is a
    // cutout: only 45 of its 256 texels are opaque (measured), so a centre sample
    // or a whole-frame mean would be mostly the cleared background and would read
    // the same under either depth function. The probe set is exactly the pixels
    // where the overlay draws — the only pixels at which "which quad won" is a
    // question with an answer.
    let mask: Vec<usize> = (0..overlay_only.len())
        .filter(|&i| overlay_only[i] != (0, 0, 0))
        .collect();
    assert!(
        mask.len() > 100,
        "the overlay must cover a measurable area for this gate to have a probe set; got {} px \
         of {}",
        mask.len(),
        overlay_only.len()
    );
    let mean = |frame: &[(u8, u8, u8)]| {
        let (mut r, mut g, mut b) = (0f64, 0f64, 0f64);
        for &i in &mask {
            r += f64::from(frame[i].0);
            g += f64::from(frame[i].1);
            b += f64::from(frame[i].2);
        }
        let n = mask.len() as f64;
        ((r / n) as f32, (g / n) as f32, (b / n) as f32)
    };
    // Green-over-red inside the probe set. The tinted overlay is `#91BD59`-ish, so
    // its G/R sits well above the untinted dirt side's — a hue ratio, which cannot
    // be satisfied by the frame merely getting brighter or darker.
    let gr = |c: (f32, f32, f32)| (c.1 + 1.0) / (c.0 + 1.0);
    let (base_c, overlay_c, both_c) = (mean(&base_only), mean(&overlay_only), mean(&both));
    println!("=== GRASS_BLOCK SIDE OVERLAY vs THE DEPTH FUNCTION ===");
    println!("  probe set: {} px where #overlay is opaque", mask.len());
    println!("  #side alone          rgb={base_c:?}  G/R={:.3}", gr(base_c));
    println!(
        "  #overlay alone       rgb={overlay_c:?}  G/R={:.3}",
        gr(overlay_c)
    );
    println!("  both, in baked order rgb={both_c:?}  G/R={:.3}", gr(both_c));
    println!(
        "  vanilla's GREATER_THAN_OR_EQUAL, which this reversed-Z renderer shares, predicts \
         the overlay wins; a strict nearer-wins comparison predicts the base wins"
    );

    // The control that makes the measurement legible: the two textures must be
    // distinguishable in the first place, or "which one won" is unanswerable.
    assert!(
        (gr(overlay_c) - gr(base_c)).abs() > 0.15,
        "the two coplanar quads must be visually distinguishable for this gate to mean \
         anything: #side G/R {:.3} vs #overlay G/R {:.3}",
        gr(base_c),
        gr(overlay_c)
    );

    assert!(
        (gr(both_c) - gr(overlay_c)).abs() < 0.05,
        "the tinted #overlay must be what a grass block's side shows, but the composite reads \
         G/R {:.3} against the overlay's {:.3} and the bare #side's {:.3}. That is the overlay \
         being rejected by a strict nearer-wins comparison: vanilla's terrain depth comparison \
         is `GREATER_THAN_OR_EQUAL` (26.2 `DepthStencilState.DEFAULT`), which this reversed-Z \
         renderer spells `DEPTH_COMPARE_NEARER_OR_EQUAL`, and grass_block.json places both \
         elements at exactly [0,0,0]..[16,16,16], so the second is coplanar with the first and \
         can never pass a strict comparison",
        gr(both_c),
        gr(overlay_c),
        gr(base_c)
    );
}

/// **The floor at light 0 is vanilla's `0.0935`, on real baked geometry** — not
/// the retired ramp's `0.2`, and not zero.
///
/// Issue #386 named the `0.2` floor as the mechanism and was right, but the first
/// fix overshot to pure black: `get_brightness(0)` is `0`, yet `lightmap.fsh`
/// seeds its accumulator with `AmbientColor`, which the overworld sets to
/// `0x0A0A0A` (`DimensionTypes.java:36`). After the `notGamma` mix that is
/// `0.0935`.
///
/// Asserted on the same four populations, because a tinted quad is the
/// interesting case: the tint is a *multiply*, so the unlit frame must be the
/// **same fraction** of the tinted daylight frame in every channel. A build that
/// applied the tint outside the light multiply would leave the unlit frame near
/// full tint strength — a ratio near `1.0` — and shows up here and nowhere else.
///
/// The control is the sky-15 frame of the same quad: it must be far from black,
/// or a quad that never drew would satisfy the ratio trivially.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn unlit_faces_reach_vanillas_ambient_floor_and_not_the_retired_ramps() {
    let Some(gpu) = setup() else {
        panic!("grass_light_response_gate: no GPU adapter; see the sibling test's message.");
    };
    let (models, reg) = build_models();

    let cases: [(&'static str, &str, &[(&str, &str)]); 4] = [
        ("stone (control, untinted)", "minecraft:stone", &[]),
        (
            "grass_block top (tinted)",
            "minecraft:grass_block",
            &[("snowy", "false")],
        ),
        ("oak_leaves (tinted)", "minecraft:oak_leaves", &[]),
        ("short_grass blade (tinted)", "minecraft:short_grass", &[]),
    ];

    // The three live hypotheses, as a fraction of the daylight frame. `shade`
    // multiplies gamma bytes, so a light-term ratio is a pixel ratio.
    const VANILLA_FLOOR: f32 = 0.093_545_4;
    const RETIRED_RAMP_FLOOR: f32 = 0.2;
    const AMBIENT_DROPPED: f32 = 0.0;
    const TINT_OUTSIDE_LIGHT: f32 = 1.0;
    const BAND: std::ops::RangeInclusive<f32> = 0.055..=0.145;

    assert!(
        BAND.contains(&VANILLA_FLOOR)
            && !BAND.contains(&RETIRED_RAMP_FLOOR)
            && !BAND.contains(&AMBIENT_DROPPED)
            && !BAND.contains(&TINT_OUTSIDE_LIGHT),
        "the band {BAND:?} must admit vanilla's {VANILLA_FLOOR} and reject \
         {RETIRED_RAMP_FLOOR} (retired ramp), {AMBIENT_DROPPED} (ambient dropped) and \
         {TINT_OUTSIDE_LIGHT} (tint applied outside the light multiply)"
    );

    println!(
        "=== LIGHT FLOOR IS VANILLA'S AMBIENT {VANILLA_FLOOR:.4} OF DAYLIGHT \
         (not {RETIRED_RAMP_FLOOR}, not 0) ==="
    );
    for (label, block, props) in cases {
        let state = find_state(reg.as_ref(), block, props)
            .unwrap_or_else(|| panic!("no state for {block} {props:?}"));
        let quad = representative(&models, state).clone();
        let bright = render_center(&gpu, &models, &quad, SKY_FULL);
        let unlit = render_center(&gpu, &models, &quad, SKY_NONE);
        let expected = (
            (f32::from(bright.0) * VANILLA_FLOOR).round() as u8,
            (f32::from(bright.1) * VANILLA_FLOOR).round() as u8,
            (f32::from(bright.2) * VANILLA_FLOOR).round() as u8,
        );
        println!(
            "  {label:<26} sky15={bright:>3?} light0={unlit:>3?}  expected ~{expected:?}  \
             (retired ramp would give ~{:?})",
            (
                (f32::from(bright.0) * RETIRED_RAMP_FLOOR).round() as u8,
                (f32::from(bright.1) * RETIRED_RAMP_FLOOR).round() as u8,
                (f32::from(bright.2) * RETIRED_RAMP_FLOOR).round() as u8,
            )
        );

        // Deliberately a low bar: `oak_leaves` is the darkest of the four (its own
        // texture is already dark green *and* it takes a biome tint, measured
        // around luma 50 at full light), so a threshold tuned to stone would fail
        // on a correct build. All this control has to establish is that the quad
        // drew something visible.
        assert!(
            luma(bright) > 20.0,
            "{label}: control's premise is false — the sky-15 frame is nearly black \
             ({bright:?}), so the assertion below would pass under a build that draws nothing"
        );
        // Luma rather than per-channel, because a tinted quad's weakest channel
        // can be single-digit at full light and 8-bit quantisation then makes its
        // ratio meaningless — `oak_leaves`' blue is the worst case. Luma is the
        // same multiply and survives the rounding.
        let ratio = luma(unlit) / luma(bright);
        assert!(
            BAND.contains(&ratio),
            "{label}: a face at light 0 must render at {VANILLA_FLOOR:.4} of daylight — \
             vanilla's `AmbientColor` floor of 0x0A0A0A after the `notGamma` mix — but \
             the ratio is {ratio:.4} (light0 {unlit:?} vs daylight {bright:?}, expected \
             ~{expected:?}). {RETIRED_RAMP_FLOOR} is the retired ramp's floor; near \
             {AMBIENT_DROPPED} means AmbientColor was dropped; near \
             {TINT_OUTSIDE_LIGHT} means the tint is applied outside the light multiply"
        );
    }
}

/// **The negative control, executed.** Render the same real grass quad twice at
/// sky 15 — a stand-in for a build in which the light term never reaches this
/// population — and confirm the gate's predicate rejects it.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn light_response_predicate_rejects_a_light_ignoring_build() {
    let Some(gpu) = setup() else {
        panic!("grass_light_response_gate: no GPU adapter; see the sibling test's message.");
    };
    let (models, reg) = build_models();
    let state = find_state(reg.as_ref(), "minecraft:grass_block", &[("snowy", "false")])
        .expect("grass_block state");
    let quad = representative(&models, state).clone();

    // Both frames at full sky: the light byte no longer varies, which is exactly
    // what "the light multiply never reaches this quad" looks like downstream.
    let bright = render_center(&gpu, &models, &quad, SKY_FULL);
    let ignored = render_center(&gpu, &models, &quad, SKY_FULL);
    let ratio = luma(ignored) / luma(bright).max(1.0);

    println!("=== NEGATIVE CONTROL (light-ignoring build) ===");
    println!("  sky15={bright:?}  'sky0'={ignored:?}  ratio={ratio:.3}");
    println!("  expected ~{RATIO_LIGHT_IGNORED:.3}, and the gate's predicate must reject it");

    assert!(
        (ratio - RATIO_LIGHT_IGNORED).abs() < 0.02,
        "the control must reproduce the bug's prediction, got {ratio:.3}"
    );
    assert!(
        !responds_to_light(ratio),
        "the gate's predicate must REJECT a light-ignoring build; it accepted ratio {ratio:.3}, \
         which means the acceptance band {BAND:?} is wide enough to pass the very defect this \
         gate exists to catch"
    );
}
