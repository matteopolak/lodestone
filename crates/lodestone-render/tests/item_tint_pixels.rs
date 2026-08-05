//! Prove that an **item definition's own `tints` list** reaches pixels, at the
//! colour the jar names, multiplied in **gamma** space.
//!
//! ## The defect
//!
//! Two of them, stacked, which is why this went unnoticed.
//!
//! 1. `lodestone_assets::item_model::parse_tint` read only the JSON key
//!    `default`. Seven of vanilla's eight tint sources use that name;
//!    `minecraft:constant` alone uses **`value`** (`Constant.java:22`). So every
//!    constant item tint in the game parsed to `None` and its colour was thrown
//!    away — the six leaves items, `vine`, `lily_pad`, `filled_map`'s layer 0,
//!    `firework_star`'s layer 0, `wolf_armor`.
//! 2. `extruded_sprite_geometry` emitted every quad untinted regardless, so even
//!    the sources that *did* parse (`potion`, `map_color`, `dye`, `firework`,
//!    `grass`) reached nothing.
//!
//! Neither failed loudly. A greyscale sprite rendered with the multiplicative
//! identity looks exactly like a sprite with no tint authored, which is why a
//! white lily pad survived.
//!
//! ## Subjects, and why these two
//!
//! * **`minecraft:lily_pad`** — one layer, `minecraft:constant` `-9321636` =
//!   `0x71C35C`. It is the case that discriminates *item* tints from *block*
//!   tints: our `vanilla_tint_kind` table gives the lily pad block
//!   `LILY_PAD_IN_WORLD` = `0x208030`, and vanilla's item renderer never consults
//!   `BlockColors` at all (`CuboidItemModelWrapper.java:89` evaluates the item
//!   definition's own list). Leaves and `grass_block` happen to agree between the
//!   two mechanisms — `0x48B518` either way — which is exactly why substituting
//!   one for the other looked fine.
//! * **`minecraft:vine`** — also one layer, `constant` `0x48B518`. A second
//!   colour so the pixel measurement does not rest on one item's texture.
//! * **`minecraft:potion`** — two layers, and only the **first** is tinted
//!   (`tints` has one entry; `models/item/potion.json` is `layer0 =
//!   potion_overlay`, `layer1 = potion`). So it proves the tint is applied
//!   *per layer* rather than to the whole stack: a bug that tints every layer
//!   passes a single-layer subject and fails here.
//!
//!   It appears in the **bake** test only, not the pixel one — its untinted
//!   second layer sits at the same depth as its tinted first, so a per-pixel
//!   ratio cannot attribute a pixel to a layer. Measured: run against `potion`
//!   the pixel test reports `gamma_mae=0.263` *and* `linear_mae=0.247`, i.e. both
//!   hypotheses wrong, which is the signature of measuring the wrong geometry
//!   rather than of a colour-space bug.
//!
//! ## Why the pixel measurement is not the *magnitude* species of vacuous test
//!
//! "The item got greener" is satisfied by any tint of any strength, and that is
//! the exact shape that shipped a hurt overlay here at 70% red where vanilla
//! renders 30%. So this gate predicts the **value**, and it predicts it from two
//! competing hypotheses computed out of constants that originate in the jar:
//!
//! | hypothesis | predicted sRGB byte |
//! |---|---|
//! | gamma (vanilla) | `untinted_byte * tint_byte / 255` |
//! | linear (the bug) | `linear_to_srgb(srgb_to_linear(untinted) * tint/255) * 255` |
//!
//! The measurement must land on the first and be far from the second. The
//! untinted byte is not assumed — it is **rendered**, by running the identical
//! frame with an all-white palette, which is simultaneously the negative control
//! (§ *Controls* below). Taking a ratio against a real render makes the
//! prediction independent of the sprite's own colours, its shade, its AO and its
//! lighting, so none of those has to be modelled to predict the byte.
//!
//! For `lily_pad`'s red channel (`0x71` = 113, factor `0.443`) at a mid texel the
//! two hypotheses are ~30/255 apart, which is far outside quantisation.
//!
//! ## No `ALPHA_BLENDING` is involved, deliberately
//!
//! The exact composited byte through `ALPHA_BLENDING` on this Metal backend is
//! **not** predictable — measured elsewhere in this repo, and an agent lost a
//! cycle to it. This gate avoids the question rather than fighting it: the model
//! pipeline's solid path does no blending, the sprite's own alpha is a hard
//! discard, and every pixel compared here is a fully-opaque interior fragment.
//!
//! ## Controls, run rather than described
//!
//! * [`an_all_white_palette_is_the_untinted_frame`] renders the control frame and
//!   asserts it *differs* from the tinted one inside the item's box. If the two
//!   were identical the ratio test above would be comparing a frame with itself
//!   and would pass vacuously for any tint.
//! * The same control's premise is checked against "what else paints here": the
//!   comparison is confined to pixels lit in **both** frames, and a bounding box
//!   is printed on failure rather than a bare percentage, because a fraction
//!   cannot tell a uniform-but-wrong frame from a localised blob.
//!
//! `#[ignore]`d and **fail-closed**: needs a fetched `client.jar`, and the pixel
//! test additionally needs a GPU adapter. Once opted in, a missing adapter is a
//! failure, never a skip. Run with
//! `cargo test -p lodestone-render --test item_tint_pixels -- --ignored --nocapture`.

use lodestone_assets::{ResourceManager, ZipSource};
use lodestone_render::entity::{dropped_item_mesh, ground_transform_for};
use lodestone_render::{
    BlockModels, Camera, GpuAtlas, GpuModelMesh, ItemGeometry, ModelMesh, ModelPipeline,
    blocks_json_registry, model_anim_buffer, model_palette_buffer, model_shared_camera_buffer,
    section_origin_buffer,
};

mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

const W: u32 = 256;
const H: u32 = 256;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// `assets/minecraft/items/lily_pad.json`'s single tint:
/// `{"type": "minecraft:constant", "value": -9321636}`.
const LILY_PAD_TINT: u32 = 0x71_C35C;

/// `BlockColors`' lily-pad colour, which is what our **block** tint table gives
/// the same id. Asserted *different* from [`LILY_PAD_TINT`], because that
/// inequality is the whole reason this subject discriminates the two mechanisms.
const LILY_PAD_BLOCK_TINT: u32 = 0x20_8030;

/// `PotionContents.BASE_POTION_COLOR` (`PotionContents.java:46`, `-13083194`),
/// which is also the `default` on `items/potion.json`'s one `minecraft:potion`
/// tint.
const POTION_TINT: u32 = 0x38_5DC6;

/// `assets/minecraft/items/vine.json`'s single tint: `constant -12012264` =
/// `0x48B518`. A second single-layer subject with a different colour, so the
/// pixel gate is not resting on one item's texture.
const VINE_TINT: u32 = 0x48_B518;

/// The palette index meaning "untinted" — [`lodestone_render`]'s `UNTINTED`.
const UNTINTED: i32 = 255;

const DROP: glam::Vec3 = glam::Vec3::new(0.5, 0.0, 0.5);
const CAM_DISTANCE: f32 = 1.2;

const CLEAR: wgpu::Color = wgpu::Color {
    r: 1.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};

/// Minimum number of pixels lit in both frames for the ratio test to be a
/// measurement rather than a coincidence.
const MIN_COMPARED_PX: usize = 200;

/// The gamma hypothesis must sit within this mean absolute error, in `0..=1`
/// sRGB units. One 8-bit quantisation step is `1/255 ≈ 0.0039`; three steps of
/// headroom covers rounding in both renders.
const MAX_GAMMA_MAE: f32 = 3.0 / 255.0;

/// The linear hypothesis must sit at least this far out. Predicted ~30/255 for
/// `lily_pad`'s red channel; 12/255 is a conservative floor that is still three
/// times [`MAX_GAMMA_MAE`], so the two verdicts cannot both hold.
const MIN_LINEAR_MAE: f32 = 12.0 / 255.0;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn setup() -> Gpu {
    let gpu = pollster::block_on(async {
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
                label: Some("item_tint_pixels device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(Gpu { device, queue })
    });
    gpu.expect("item_tint_pixels: no GPU adapter, and this gate must not skip")
}

fn build_models() -> BlockModels {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    BlockModels::build(&manager, &registry).expect("bake block models")
}

fn geometry<'a>(models: &'a BlockModels, item: &str) -> &'a ItemGeometry {
    let id = item.parse().expect("valid resource location");
    models
        .item(&id)
        .unwrap_or_else(|| panic!("{item} has no baked geometry"))
}

/// The distinct `tint_index` values across an item's baked quads, sorted.
fn distinct_tint_indices(geometry: &ItemGeometry) -> Vec<i32> {
    let mut v: Vec<i32> = geometry
        .quads
        .iter()
        .map(|q| q.tint_index.unwrap_or(UNTINTED))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The palette entry at `slot`, back as a `0xRRGGBB` byte triple. The palette
/// holds straight `0..=1` sRGB multipliers (`block_models::rgb_to_rgba`), so this
/// is a plain scale by 255 with rounding.
fn palette_rgb(models: &BlockModels, slot: i32) -> u32 {
    let c = models.tint_palette()[slot as usize];
    let b = |v: f32| ((v * 255.0).round() as u32) & 0xFF;
    (b(c[0]) << 16) | (b(c[1]) << 8) | b(c[2])
}

fn camera(centre: glam::Vec3) -> Camera {
    Camera {
        position: glam::Vec3::new(DROP.x, centre.y, DROP.z + CAM_DISTANCE),
        yaw: 180.0,
        pitch: 0.0,
        aspect: 1.0,
        ..Camera::default()
    }
}

/// The dropped-item mesh for `geometry`, posed exactly as the world pass poses a
/// drop. `age_ticks`/`bob_offset` are zero so the pose is deterministic and both
/// frames of a comparison are pixel-aligned.
fn drop_mesh(geometry: &ItemGeometry) -> ModelMesh {
    let ground = ground_transform_for(geometry.gui_light);
    dropped_item_mesh(
        &geometry.quads,
        geometry.gui_light,
        &ground,
        DROP,
        0.0,
        0.0,
        0xF0,
    )
}

/// Render one mesh through the real [`ModelPipeline`], with `palette` bound as
/// the tint palette. Passing `models.tint_palette()` is the production frame;
/// passing an all-white palette is the untinted control.
#[allow(clippy::too_many_lines)]
fn render(
    gpu: &Gpu,
    models: &BlockModels,
    mesh: &ModelMesh,
    cam: &Camera,
    palette: &[[f32; 4]],
) -> Vec<(u8, u8, u8)> {
    let device = &gpu.device;
    let queue = &gpu.queue;

    let pipeline = ModelPipeline::new(device, FORMAT);
    let atlas = GpuAtlas::from_atlas(device, queue, models.atlas());
    let atlas_bg = pipeline.atlas_bind_group(device, &atlas);
    let palette_buffer = model_palette_buffer(device, palette);
    let palette_bg = pipeline.palette_bind_group(device, &palette_buffer);
    let anim_buffer = model_anim_buffer(device, &models.anim_slot_uniforms(0));
    let anim_bg = pipeline.anim_bind_group(device, &anim_buffer);
    let cam_buffer = model_shared_camera_buffer(device, cam.view_projection().to_cols_array_2d());
    let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buffer, &origin_buffer);

    let size = wgpu::Extent3d {
        width: W,
        height: H,
        depth_or_array_layers: 1,
    };
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("item tint target"),
        size,
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
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

    let gpu_mesh = GpuModelMesh::upload(device, mesh);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("item tint pixels"),
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
        if let Some(gpu_mesh) = &gpu_mesh {
            pass.set_pipeline(&pipeline.pipeline);
            pass.set_bind_group(0, &cam_bg, &[0]);
            pass.set_bind_group(1, &atlas_bg, &[]);
            pass.set_bind_group(2, &palette_bg, &[]);
            pass.set_bind_group(3, &anim_bg, &[]);
            pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
            pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
        }
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
        size,
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

fn clear_rgb() -> (u8, u8, u8) {
    (255, 0, 255)
}

fn lit(px: (u8, u8, u8)) -> bool {
    px != clear_rgb()
}

/// An all-white palette: the untinted control, and the exact frame the build
/// produced before this feature existed (every quad's `tint_index` resolved to a
/// white slot).
fn white_palette(len: usize) -> Vec<[f32; 4]> {
    vec![[1.0, 1.0, 1.0, 1.0]; len]
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Bounding box of a pixel set as `(x0, y0, x1, y1)`, inclusive. Printed on
/// failure so a bad frame says *where*: a mean error alone cannot distinguish a
/// uniformly wrong frame from a localised blob.
fn bbox(pixels: &[(u32, u32)]) -> (u32, u32, u32, u32) {
    let mut b = (u32::MAX, u32::MAX, 0, 0);
    for &(x, y) in pixels {
        b.0 = b.0.min(x);
        b.1 = b.1.min(y);
        b.2 = b.2.max(x);
        b.3 = b.3.max(y);
    }
    b
}

/// Mean absolute error of each hypothesis against the measured tinted frame,
/// over the pixels lit in **both** frames, for one channel.
///
/// Returns `(compared_px, gamma_mae, linear_mae, compared_pixels)`.
fn discriminate(
    tinted: &[(u8, u8, u8)],
    untinted: &[(u8, u8, u8)],
    channel: usize,
    tint_byte: u32,
) -> (usize, f32, f32, Vec<(u32, u32)>) {
    let pick = |px: (u8, u8, u8)| match channel {
        0 => px.0,
        1 => px.1,
        _ => px.2,
    };
    let t = tint_byte as f32 / 255.0;
    let mut n = 0usize;
    let mut gamma_err = 0.0f32;
    let mut linear_err = 0.0f32;
    let mut pixels = Vec::new();
    for (i, (&a, &b)) in tinted.iter().zip(untinted.iter()).enumerate() {
        if !lit(a) || !lit(b) {
            continue;
        }
        let w = f32::from(pick(b)) / 255.0;
        // A near-black control pixel carries almost no signal and its ratio is
        // dominated by quantisation, so it is excluded — not to flatter the
        // result, but because both hypotheses predict ~0 there and it would
        // dilute the discrimination in the *gamma* hypothesis's favour.
        if w < 0.15 {
            continue;
        }
        let obs = f32::from(pick(a)) / 255.0;
        let pred_gamma = w * t;
        let pred_linear = linear_to_srgb(srgb_to_linear(w) * t);
        gamma_err += (obs - pred_gamma).abs();
        linear_err += (obs - pred_linear).abs();
        n += 1;
        pixels.push(((i as u32) % W, (i as u32) / W));
    }
    if n == 0 {
        return (0, f32::NAN, f32::NAN, pixels);
    }
    (n, gamma_err / n as f32, linear_err / n as f32, pixels)
}

/// The bake-level wiring: the item definition's own tint reaches the baked quads
/// and the palette slot holds the jar's colour exactly.
///
/// Needs the jar but no GPU, so it is the cheap half and the one that localises a
/// regression to the bake rather than the shader.
#[test]
#[ignore = "requires a fetched vanilla client.jar; run explicitly"]
fn the_item_definitions_own_tints_reach_the_baked_palette() {
    let models = build_models();

    // `lily_pad`: one layer, one tint, so every quad must carry it.
    let lily = geometry(&models, "minecraft:lily_pad");
    let lily_slots = distinct_tint_indices(lily);
    assert_eq!(
        lily_slots.len(),
        1,
        "lily_pad has exactly one sprite layer, so one tint slot; got {lily_slots:?}"
    );
    let slot = lily_slots[0];
    assert_ne!(
        slot, UNTINTED,
        "lily_pad's quads are untinted — the item definition's \
         `{{\"type\": \"minecraft:constant\", \"value\": -9321636}}` reached nothing. \
         If this is the only failure, suspect `parse_tint` reading `default` but not `value`."
    );
    assert_eq!(
        palette_rgb(&models, slot),
        LILY_PAD_TINT,
        "lily_pad's palette slot must hold the item definition's constant"
    );
    // The discriminating inequality. If these were equal this subject would
    // prove nothing about which of the two mechanisms produced the colour.
    assert_ne!(
        LILY_PAD_TINT, LILY_PAD_BLOCK_TINT,
        "premise of this subject: the item tint and the block tint differ"
    );
    assert_ne!(
        palette_rgb(&models, slot),
        LILY_PAD_BLOCK_TINT,
        "lily_pad resolved its *block* tint (BlockColors.LILY_PAD_IN_WORLD) rather than its \
         item definition's — vanilla's item renderer never consults BlockColors"
    );

    // `vine`: also one layer, a different constant. Two subjects with different
    // colours rule out a slot that happens to hold the right value by accident.
    let vine = geometry(&models, "minecraft:vine");
    let vine_slots = distinct_tint_indices(vine);
    assert_eq!(vine_slots.len(), 1, "vine has one layer; got {vine_slots:?}");
    assert_ne!(vine_slots[0], UNTINTED, "vine's constant tint reached nothing");
    assert_eq!(
        palette_rgb(&models, vine_slots[0]),
        VINE_TINT,
        "vine's palette slot must hold its item definition's constant"
    );
    assert_ne!(
        vine_slots[0], slot,
        "vine and lily_pad have different tint colours and so must intern to different slots"
    );

    // `potion`: two layers, one tint. Layer 0 tinted, layer 1 not — so both an
    // untinted slot and the potion slot must be present. A build that tinted the
    // whole stack would show one slot; a build that tinted nothing would show
    // only `UNTINTED`.
    let potion = geometry(&models, "minecraft:potion");
    let potion_slots = distinct_tint_indices(potion);
    assert_eq!(
        potion_slots.len(),
        2,
        "potion has two layers of which exactly one is tinted, so two distinct slots; \
         got {potion_slots:?} (one slot means the tint was applied per-stack, not per-layer)"
    );
    assert!(
        potion_slots.contains(&UNTINTED),
        "potion's layer1 (the glass bottle) carries no tint entry and must stay untinted; \
         got {potion_slots:?}"
    );
    let tinted = potion_slots
        .iter()
        .copied()
        .find(|&s| s != UNTINTED)
        .expect("one tinted slot");
    assert_eq!(
        palette_rgb(&models, tinted),
        POTION_TINT,
        "potion's tinted layer must hold PotionContents.BASE_POTION_COLOR"
    );
}

/// The negative control, rendered and observed: the all-white palette frame must
/// actually differ from the tinted one. Without this the ratio test in
/// [`an_item_tint_multiplies_in_gamma_space_at_the_jars_colour`] could be
/// comparing a frame with itself.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn an_all_white_palette_is_the_untinted_frame() {
    let gpu = setup();
    let models = build_models();
    let lily = geometry(&models, "minecraft:lily_pad");
    let mesh = drop_mesh(lily);
    let centre = mesh
        .vertices
        .iter()
        .fold(glam::Vec3::ZERO, |a, v| a + glam::Vec3::from(v.position))
        / mesh.vertices.len() as f32;
    let cam = camera(centre);

    let tinted = render(&gpu, &models, &mesh, &cam, models.tint_palette());
    let untinted = render(
        &gpu,
        &models,
        &mesh,
        &cam,
        &white_palette(models.tint_palette().len()),
    );

    let differing: Vec<(u32, u32)> = tinted
        .iter()
        .zip(untinted.iter())
        .enumerate()
        .filter(|(_, (a, b))| lit(**a) && lit(**b) && a != b)
        .map(|(i, _)| ((i as u32) % W, (i as u32) / W))
        .collect();

    println!(
        "control: {} pixels differ between tinted and white-palette frames, bbox {:?}",
        differing.len(),
        bbox(&differing)
    );
    assert!(
        differing.len() >= MIN_COMPARED_PX,
        "the white-palette control frame is (nearly) identical to the tinted one — only {} \
         differing pixels, bbox {:?}. Either the tint never reached the shader, or the control \
         is not a control.",
        differing.len(),
        bbox(&differing)
    );
}

/// The load-bearing measurement: the composited byte lands on the **gamma**
/// hypothesis and far from the **linear** one, per channel, for both subjects.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar; run explicitly"]
fn an_item_tint_multiplies_in_gamma_space_at_the_jars_colour() {
    let gpu = setup();
    let models = build_models();
    let white = white_palette(models.tint_palette().len());

    // **Single-layer subjects only**, and that restriction is load-bearing
    // rather than convenient. `minecraft:potion` is a two-layer item whose
    // *untinted* layer1 (the glass bottle) is extruded at essentially the same
    // depth as the tinted layer0, so which of the two owns any given pixel is
    // decided by depth ordering. A per-pixel ratio there is not measuring the
    // tinted layer at all: run against `potion` this loop reports
    // `gamma_mae=0.263` and `linear_mae=0.247` — both hypotheses wrong, because
    // neither describes a pixel that came from the other layer. Asking "what
    // else already paints in this rect?" is the check that catches it, and the
    // answer for a multi-layer item is "the item's own other layers".
    //
    // Per-layer assignment is proved instead by
    // [`the_item_definitions_own_tints_reach_the_baked_palette`], which reads the
    // slots off the baked quads where the layers are still distinguishable. That
    // is the right instrument for it, and a pixel gate is not.
    for (item, tint) in [
        ("minecraft:lily_pad", LILY_PAD_TINT),
        ("minecraft:vine", VINE_TINT),
    ] {
        let geom = geometry(&models, item);
        let mesh = drop_mesh(geom);
        let centre = mesh
            .vertices
            .iter()
            .fold(glam::Vec3::ZERO, |a, v| a + glam::Vec3::from(v.position))
            / mesh.vertices.len() as f32;
        let cam = camera(centre);

        let tinted = render(&gpu, &models, &mesh, &cam, models.tint_palette());
        let untinted = render(&gpu, &models, &mesh, &cam, &white);

        // The channel whose tint factor is furthest from 1.0 discriminates best;
        // assert on every channel that is meaningfully below full, so a
        // per-channel swap (R and B transposed) cannot pass.
        let bytes = [(tint >> 16) & 0xFF, (tint >> 8) & 0xFF, tint & 0xFF];
        let mut checked = 0;
        for (channel, &tint_byte) in bytes.iter().enumerate() {
            // At tint ≈ 255 the two hypotheses coincide (both are the identity),
            // so such a channel cannot discriminate and asserting on it would be
            // a vacuous pass. Skip it explicitly rather than silently.
            if tint_byte > 235 {
                println!("{item} channel {channel}: tint {tint_byte} ~= identity, not discriminating");
                continue;
            }
            let (n, gamma_mae, linear_mae, pixels) =
                discriminate(&tinted, &untinted, channel, tint_byte);
            println!(
                "{item} channel {channel}: tint={tint_byte} n={n} gamma_mae={:.5} \
                 linear_mae={:.5} bbox={:?}",
                gamma_mae,
                linear_mae,
                bbox(&pixels)
            );
            assert!(
                n >= MIN_COMPARED_PX,
                "{item} channel {channel}: only {n} comparable pixels (bbox {:?}) — too few to \
                 measure; the item may not be reaching the frame at all",
                bbox(&pixels)
            );
            assert!(
                gamma_mae <= MAX_GAMMA_MAE,
                "{item} channel {channel}: measured bytes are {gamma_mae:.5} from the GAMMA \
                 prediction (limit {MAX_GAMMA_MAE:.5}); linear prediction is {linear_mae:.5} \
                 away. bbox {:?}",
                bbox(&pixels)
            );
            assert!(
                linear_mae >= MIN_LINEAR_MAE,
                "{item} channel {channel}: the LINEAR hypothesis is only {linear_mae:.5} away \
                 (floor {MIN_LINEAR_MAE:.5}), so this frame does not discriminate the two and \
                 the gamma pass above proves nothing. bbox {:?}",
                bbox(&pixels)
            );
            assert!(
                linear_mae > gamma_mae * 3.0,
                "{item} channel {channel}: gamma_mae={gamma_mae:.5} linear_mae={linear_mae:.5} \
                 are too close to call"
            );
            checked += 1;
        }
        assert!(
            checked > 0,
            "{item}: every channel was too close to the identity to discriminate — this subject \
             cannot test gamma at all and the gate is vacuous for it"
        );
    }
}
