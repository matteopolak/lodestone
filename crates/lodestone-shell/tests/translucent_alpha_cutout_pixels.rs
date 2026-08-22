//! Vanilla's terrain alpha test is **per pipeline**, and this gate is what
//! stops that fact drifting back out of `model.wgsl`.
//!
//! # The defect this was built for
//!
//! `terrain.fsh`'s discard is `#ifdef ALPHA_CUTOUT`, and `RenderPipelines`
//! gives three terrain pipelines three different answers: `SOLID_TERRAIN`
//! defines nothing and runs no test, `CUTOUT_TERRAIN` uses `0.5`, and
//! `TRANSLUCENT_TERRAIN` uses `0.1`. `model.wgsl` hardcoded `0.5` for every
//! pass — correct for cutout, five times too strict for translucent.
//!
//! That is not a subtle divergence, because real translucent block textures sit
//! squarely in the gap. Read straight out of the 26.2 `client.jar`,
//! `block/white_stained_glass.png` has exactly three distinct alpha values —
//! `102`, `155` and `163` — and **191 of its 256 texels carry `102`**, i.e.
//! `0.400`. A `0.5` test therefore discarded three quarters of every
//! stained-glass face, and whatever was behind it painted instead: for a pane
//! or a wall silhouetted against the sky, that is the sky, in a scattered
//! per-texel pattern.
//!
//! # What this asserts
//!
//! The same shape as `near_grazing_face_coverage_pixels`: an independent
//! ray-cast oracle (a direction transcribed from `Camera::basis`'s closed form,
//! marched through `World::block_state_at`) says which pixels a wall of stained
//! glass covers, and the renderer has to paint them. Painted means "differs
//! from the identical frame with no terrain uploaded" — a partial alpha
//! composited over the sky is a long way from the sky, so a surviving texel is
//! unambiguous.
//!
//! # Measured
//!
//! Measured over 12,110 px of oracle area for the glass wall and 13,344 px for
//! the stone control, in the same frame under the same camera:
//!
//! | threshold on the translucent pipeline | glass | stone |
//! |---|---|---|
//! | `0.5` (as shipped) | **0.2423** | 0.9992 |
//! | `0.1` (vanilla) | **0.9991** | 0.9992 |
//!
//! The before-figure is an observation, not a construction: the neuter was run
//! and watched to fail. It is also close to what the texture's own histogram
//! predicts unaided — 65 of 256 texels clear `0.5`, i.e. 0.254 — and the small
//! shortfall is the mip chain, whose deeper levels average `102` in with its
//! neighbours and push more of the sprite under the threshold, not fewer.
//!
//! **The stone control does not move**: 0.9992 in both runs, to four decimal
//! places. That is what localises the change to the translucent pass rather
//! than to the gate, the fixture or the oracle.
//!
//! # Scope, stated plainly
//!
//! This proves the **draw** — real baked geometry from the vanilla
//! `client.jar`, the real mesher's own layer split, the real translucent
//! pipeline. It installs no wire and no ECS input.
//!
//! Fail-closed: no GPU adapter and no vanilla `client.jar` are failures, never
//! skips.
//!
//! ```text
//! cargo test -p lodestone-shell --test translucent_alpha_cutout_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR, ThirdPersonBodyState};
use lodestone::mesher::{
    SectionGeometry, SectionKey, mesh_snapshot_models_layers, snapshot_section, snapshot_visibility,
};
use lodestone::resources::BlockResources;
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, cull::within_view_distance,
    entity_anim::AnimInput, fog::FogSettings,
};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

const W: u32 = 640;
const H: u32 = 480;
const FOV_Y_DEGREES: f32 = 70.0;
const RD_CHUNKS: i32 = 8;
const MIN_Y: i32 = 0;
const FLOOR_TOP: i32 = 64;
const SECTION_COUNT: usize = 8;
const EYE_HEIGHT: f32 = 1.62;
/// Both subject walls run across the view at this distance, above eye level so
/// the sky — not the floor — is what shows through a wrongly discarded texel.
const WALL_Z: i32 = 20;
const WALL_BOTTOM: i32 = FLOOR_TOP + 2;
const WALL_TOP: i32 = FLOOR_TOP + 5;
/// The glass wall spans this half-width in `x`, the stone control the mirror
/// image, so both are in the same frame under the same camera.
const HALF: i32 = 10;

fn suppress_first_person_arm(state: &mut RenderState) {
    state.set_third_person_body_source(|| {
        Some(ThirdPersonBodyState {
            player_skin: None,
            feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
            body_yaw_deg: 0.0,
            anim: AnimInput::default(),
            scale: 1.0,
            swim_amount: 0.0,
            slim: false,
            equipment: Vec::new(),
        })
    });
}

const DIFFERS_FROM_REFERENCE: i32 = 60;

fn differs(subject: &[u8], reference: &[u8]) -> bool {
    let d = (i32::from(subject[0]) - i32::from(reference[0])).abs()
        + (i32::from(subject[1]) - i32::from(reference[1])).abs()
        + (i32::from(subject[2]) - i32::from(reference[2])).abs();
    d > DIFFERS_FROM_REFERENCE
}

fn first_state_named(name: &str) -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the 26.2 block-state table"))
}

fn oracle_ray_dir(camera: &Camera, px: f32, py: f32) -> glam::Vec3 {
    let (sy, cy) = camera.yaw.to_radians().sin_cos();
    let (sp, cp) = camera.pitch.to_radians().sin_cos();
    let right = glam::Vec3::new(-cy, 0.0, -sy);
    let up = glam::Vec3::new(-sy * sp, cp, cy * sp);
    let forward = glam::Vec3::new(-sy * cp, -sp, cy * cp);
    let half_y = (camera.fov_y_degrees.to_radians() * 0.5).tan();
    let half_x = half_y * camera.aspect;
    let ndc_x = 2.0 * px / W as f32 - 1.0;
    let ndc_y = 1.0 - 2.0 * py / H as f32;
    (forward + right * (ndc_x * half_x) + up * (ndc_y * half_y)).normalize()
}

fn oracle_first_solid_cell(
    world: &World,
    air: u32,
    origin: glam::Vec3,
    dir: glam::Vec3,
    max_dist: f32,
) -> Option<[i32; 3]> {
    let o = [origin.x, origin.y, origin.z];
    let d = [dir.x, dir.y, dir.z];
    let mut cell = [o[0].floor() as i32, o[1].floor() as i32, o[2].floor() as i32];
    let mut step = [0i32; 3];
    let mut t_max = [f32::INFINITY; 3];
    let mut t_delta = [f32::INFINITY; 3];
    for a in 0..3 {
        if d[a] > 0.0 {
            step[a] = 1;
            t_delta[a] = 1.0 / d[a];
            t_max[a] = ((cell[a] + 1) as f32 - o[a]) / d[a];
        } else if d[a] < 0.0 {
            step[a] = -1;
            t_delta[a] = -1.0 / d[a];
            t_max[a] = (cell[a] as f32 - o[a]) / d[a];
        }
    }
    let mut t = 0.0f32;
    while t < max_dist {
        if let Some(state) = world.block_state_at(cell[0], cell[1], cell[2]) {
            if state != air {
                return Some(cell);
            }
        }
        let axis = if t_max[0] < t_max[1] && t_max[0] < t_max[2] {
            0
        } else if t_max[1] < t_max[2] {
            1
        } else {
            2
        };
        if !t_max[axis].is_finite() {
            break;
        }
        t = t_max[axis];
        cell[axis] += step[axis];
        t_max[axis] += t_delta[axis];
    }
    None
}

/// `0` = the glass wall, `1` = the stone control wall, `usize::MAX` = neither.
fn wall_of(cell: [i32; 3]) -> usize {
    if cell[2] != WALL_Z || cell[1] < WALL_BOTTOM || cell[1] > WALL_TOP {
        return usize::MAX;
    }
    if cell[0] < 0 { 0 } else { 1 }
}

/// Sub-pixel oracle coverage per wall, sampled on a 4x4 sub-grid only where the
/// centre-ray classification is not locally uniform — the same construction
/// `near_grazing_face_coverage_pixels` uses, and for the same reason: the
/// silhouette is where a whole-pixel painted count and a sub-pixel oracle count
/// legitimately disagree, and eroding it away would remove real surface.
fn oracle_coverage(world: &World, air: u32, camera: &Camera) -> Vec<(usize, f32)> {
    let camera_chunk = (
        (camera.position.x / 16.0).floor() as i32,
        (camera.position.z / 16.0).floor() as i32,
    );
    let at = |x: f32, y: f32| -> usize {
        let dir = oracle_ray_dir(camera, x, y);
        match oracle_first_solid_cell(world, air, camera.position, dir, 60.0) {
            Some(cell) if within_view_distance(camera_chunk, (cell[0] >> 4, cell[2] >> 4), RD_CHUNKS as u32) => {
                wall_of(cell)
            }
            _ => usize::MAX,
        }
    };
    let mut centre = vec![usize::MAX; (W * H) as usize];
    for y in 0..H {
        for x in 0..W {
            centre[(y * W + x) as usize] = at(x as f32 + 0.5, y as f32 + 0.5);
        }
    }
    let mut out = vec![(usize::MAX, 0.0f32); centre.len()];
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) as usize;
            let mut uniform = x > 0 && y > 0 && x + 1 < W && y + 1 < H;
            if uniform {
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let n = ((y as i32 + dy) as u32 * W + (x as i32 + dx) as u32) as usize;
                        uniform &= centre[n] == centre[i];
                    }
                }
            }
            if uniform {
                out[i] = (centre[i], if centre[i] == usize::MAX { 0.0 } else { 1.0 });
                continue;
            }
            let mut counts = [0u32; 2];
            for sy in 0..4 {
                for sx in 0..4 {
                    let c = at(x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0);
                    if c < 2 {
                        counts[c] += 1;
                    }
                }
            }
            let best = if counts[0] >= counts[1] { 0 } else { 1 };
            if counts[best] > 0 {
                out[i] = (best, counts[best] as f32 / 16.0);
            }
        }
    }
    out
}

fn build_world(glass: u32, stone: u32, air: u32) -> World {
    let mut world = World::new();
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            let column = ChunkColumn::new(
                MIN_Y,
                SECTION_COUNT,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                air,
                0,
            );
            let mut light = ColumnLight::new(SECTION_COUNT);
            for i in 0..light.light_section_count() {
                *light.sky_mut(i) = lodestone_world::LightData::Uniform(15);
                *light.block_mut(i) = lodestone_world::LightData::Uniform(0);
            }
            world.load(
                ChunkPos::new(cx, cz),
                LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()),
            );
        }
    }
    let lo = -RD_CHUNKS * 16;
    let hi = RD_CHUNKS * 16 + 15;
    let floor = world.fill_region([lo, MIN_Y, lo], [hi, FLOOR_TOP - 1, hi], stone);
    assert!(floor > 0, "fixture: floor must actually write blocks");
    let g = world.fill_region([-HALF, WALL_BOTTOM, WALL_Z], [-1, WALL_TOP, WALL_Z], glass);
    assert!(g > 0, "fixture: the glass wall must actually write blocks");
    let s = world.fill_region([0, WALL_BOTTOM, WALL_Z], [HALF, WALL_TOP, WALL_Z], stone);
    assert!(s > 0, "fixture: the stone control wall must actually write blocks");
    world
}

const FADE_COMPLETE_TICK: u64 = 200;

fn render_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    target: &mut HeadlessTarget,
    atlas: &lodestone_render::BlockAtlas,
    world: &World,
    models: &lodestone_render::BlockModels,
    camera: &Camera,
    upload_terrain: bool,
) -> Vec<u8> {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    state.set_fog(FogSettings::for_render_distance(SKY_COLOR, RD_CHUNKS as u32), RD_CHUNKS as u32);
    if upload_terrain {
        let mut uploaded = 0usize;
        for cx in -RD_CHUNKS..=RD_CHUNKS {
            for cz in -RD_CHUNKS..=RD_CHUNKS {
                for si in 0..SECTION_COUNT {
                    let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                    let Some(snap) = snapshot_section(world, key) else { continue };
                    // The production split, not `mesh_snapshot_models`: stained
                    // glass is `RenderLayer::Translucent` and reaches the
                    // translucent pipeline — the one whose threshold this gate
                    // is about — only through this call.
                    let (opaque, translucent_blocks) =
                        mesh_snapshot_models_layers(&snap, models, true, 2);
                    let visibility = snapshot_visibility(&snap, models);
                    state.upload_section(
                        device,
                        queue,
                        key,
                        &SectionGeometry::Model {
                            opaque,
                            water: ModelMesh::default(),
                            translucent_blocks,
                            visibility,
                        },
                    );
                    uploaded += 1;
                }
            }
        }
        assert!(uploaded > 0, "fixture: some sections must have uploaded");
    }
    state.update_animation(queue, FADE_COMPLETE_TICK);
    let frame = target.acquire().expect("headless acquire");
    state.render(device, queue, frame.view(), camera, None, &[]);
    target.read_texels(device, queue)
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn stained_glass_paints_the_area_the_oracle_says_it_covers() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "vanilla assets did not load (banner: {:?}) — this gate needs a real client.jar \
             under .cache/mc/26.2 (LODESTONE_ASSETS)",
            resources.banner
        )
    });
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let glass = first_state_named("minecraft:white_stained_glass");
    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let world = build_world(glass, stone, air);

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = Camera {
        position: glam::Vec3::new(0.5, FLOOR_TOP as f32 + EYE_HEIGHT, 0.5),
        yaw: 0.0,
        pitch: -6.0,
        fov_y_degrees: FOV_Y_DEGREES,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS as u32, 0),
    };

    let reference =
        render_frame(device, queue, format, &mut target, &atlas, &world, models, &camera, false);
    let subject =
        render_frame(device, queue, format, &mut target, &atlas, &world, models, &camera, true);
    let coverage = oracle_coverage(&world, air, &camera);

    let names = ["white_stained_glass (translucent pass)", "stone (opaque control)"];
    let mut expected = [0.0f32; 2];
    let mut painted = [0.0f32; 2];
    for (i, &(class, cov)) in coverage.iter().enumerate() {
        if class > 1 {
            continue;
        }
        expected[class] += cov;
        if differs(&subject[i * 4..i * 4 + 4], &reference[i * 4..i * 4 + 4]) {
            painted[class] += cov;
        }
    }
    for c in 0..2 {
        eprintln!(
            "  {:<40} oracle area {:>9.1}  painted {:>9.1}  ratio {:.4}",
            names[c],
            expected[c],
            painted[c],
            painted[c] / expected[c].max(1.0)
        );
    }
    for c in 0..2 {
        assert!(
            expected[c] > 2_000.0,
            "{}: only {:.1} px of oracle area — the fixture is not showing the wall this gate \
             exists to measure",
            names[c],
            expected[c]
        );
    }
    // Both walls, one number. The unfixed shader measured 0.2423 for the glass
    // and left the stone at 0.9992, so a single threshold separates the two
    // hypotheses by a factor of four and the control cannot move with it.
    for c in 0..2 {
        let ratio = painted[c] / expected[c];
        assert!(
            ratio > 0.95,
            "{} painted {ratio:.4} of the area an independent ray-cast oracle says it covers. \
             For the translucent pass this is the `ALPHA_CUTOUT` divergence: vanilla's \
             TRANSLUCENT_TERRAIN tests at 0.1 and 191 of white_stained_glass's 256 texels carry \
             alpha 0.400, so a 0.5 test deletes three quarters of every face.",
            names[c]
        );
    }
}
