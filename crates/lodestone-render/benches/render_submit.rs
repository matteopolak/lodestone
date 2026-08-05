//! Per-frame **counts** for the render-submit path, plus mesh-arena occupancy
//! (issues #106, #128, #160).
//!
//! # Why this file is mostly counts and barely any timings
//!
//! `CLAUDE.md`'s measurement rules say a wall-clock number taken while a dozen
//! other agents build is a sample, not a measurement — and that a *ratio* of two
//! sequential durations is no safer, because the two arms do not see the same
//! load. Counts are immune to all of that. Every issue this file addresses is
//! fortunately count-shaped:
//!
//! * **#128** wants draw-call and bind-group counts to be *measured*, not
//!   assumed from reading the #75 fix.
//! * **#106** wants entity render planning gated on "draw-call count staying
//!   flat as entity count grows", explicitly in preference to a raw timing.
//! * **#160** wants arena occupancy, which is bytes and allocation counts.
//!
//! So the assertions here are all counts and the few durations are recorded
//! advisory baselines, labelled provisional.
//!
//! # What #128 asks for that this file cannot supply, and the exact seam
//!
//! #128 asks for per-frame **draw-call** *and* **bind-group-bind** counts, for
//! three paths (model/fluid terrain, packed/demo, entity). Verified state of the
//! tree, not assumed:
//!
//! * **Draw calls are already counted**, but on the shell side:
//!   `RenderStats::draw_calls` (`crates/lodestone-shell/src/gpu/stats.rs:11`),
//!   incremented at 12 call sites in `gpu/frame.rs` and `gpu/first_person.rs`.
//! * **Bind-group binds are counted nowhere.** There are 179 `set_bind_group`
//!   call sites in the workspace and no wrapper, no counter and no
//!   `RenderStats` field. Neither are `write_buffer` calls — which is notable,
//!   because a `write_buffer`-per-section was the actual #75 defect.
//! * `RenderState::render_inner` (#133) is a **private** fn in
//!   `crates/lodestone-shell/src/gpu/frame.rs:147`, and `lodestone-shell` has no
//!   `benches/` directory. `MODEL_ORIGIN_ARENA_SLOTS` (#133/#160) is
//!   `pub(super)` at `crates/lodestone-shell/src/gpu/terrain.rs:62`, so its
//!   occupancy is unreachable from any test or bench in either crate.
//!
//! Closing those halves needs three shell-side seams that do not exist:
//! `RenderStats::bind_group_binds`, `RenderStats::buffer_writes`, and an
//! accessor for `SectionOriginArena`'s used slots. That is a `lodestone-shell`
//! `src/gpu/` change, deliberately out of scope for this pass. What *is* here is
//! the whole draw-call side of the terrain path, measured CPU-only through the
//! same `WorldScene::plan_frame` the real frame builds its draw list from.
//!
//! Run with `cargo bench -p lodestone-render --bench render_submit`. The
//! occupancy bench needs a GPU adapter and says so loudly when there is none;
//! everything else is hermetic.

mod support;

use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use glam::{Mat4, Vec3};
use lodestone_render::mesher::SectionSource;
use lodestone_render::{
    BlockClassifier, Camera, Cell, DrawRegion, EntityInstance, GpuContext, SectionVisibility,
    SpriteId, WorldMesher, WorldScene, section_of,
};
use lodestone_render::entity::plan_entities;
use lodestone_testsupport::bench_fixtures::{MODERN_SECTIONS, synthetic_overworld_column};
use lodestone_world::ChunkSection;

const INDICES_PER_QUAD: u32 = 6;
const AIR: u32 = 0;

/// Same classifier as `benches/meshing.rs` and `tests/world_mesher_bench.rs`.
#[derive(Debug)]
struct TerrainClassifier;

impl BlockClassifier for TerrainClassifier {
    fn classify(&self, state_id: u32, block_light: u8, sky_light: u8) -> Cell {
        if state_id == AIR {
            Cell { occludes: false, surface: None, block_light, sky_light }
        } else {
            let mut c = Cell::solid(SpriteId(state_id as u16));
            c.block_light = block_light;
            c.sky_light = sky_light;
            c
        }
    }
}

fn solid_mesh(instance: u32, quads: u32) -> DrawRegion {
    DrawRegion {
        first_index: 0,
        index_count: quads * INDICES_PER_QUAD,
        base_vertex: 0,
        instance,
        visible: true,
    }
}

fn air_mesh() -> DrawRegion {
    DrawRegion { first_index: 0, index_count: 0, base_vertex: 0, instance: 0, visible: true }
}

/// A flat world at render distance `rd`, the same shape `tests/scene_bench.rs`
/// builds: buried layer, surface layer, air layer the camera stands in.
fn flat_scene(rd: i32) -> WorldScene {
    let mut scene = WorldScene::new();
    let stride = 2 * rd + 1;
    for x in -rd..=rd {
        for z in -rd..=rd {
            let instance = ((x + rd) * stride + (z + rd)) as u32;
            scene.insert_section((x, 0, z), solid_mesh(instance, 128), SectionVisibility::solid());
            scene.insert_section(
                (x, 1, z),
                solid_mesh(instance + 100_000, 128),
                SectionVisibility::solid(),
            );
            scene.insert_section((x, 2, z), air_mesh(), SectionVisibility::all());
        }
    }
    scene
}

fn standing_camera() -> Camera {
    Camera { position: Vec3::new(8.0, 40.0, 8.0), yaw: 0.0, pitch: 20.0, ..Camera::default() }
}

/// **Issue #128 (draw-call half)** — real per-frame draw-list sizes for the
/// terrain path, measured, plus the per-strategy API-call count they imply.
///
/// # A correction this bench's first run produced
///
/// This function originally asserted `plan.regions.len() == stats.drawn`, on the
/// assumption that `FramePlan::regions` *is* the frame's draw list. It failed
/// immediately: 578 regions against 101 drawn at rd=8. Reading the actual
/// construction (`src/scene.rs:207`) rather than the name: `regions` holds one
/// entry per **drawable** section (`index_count > 0`) with a per-frame `visible`
/// flag, and *culled regions are deliberately retained with `visible = false`*
/// so an indirect strategy can zero their instance count without resizing the
/// list. So `regions.len() == stats.drawable`, and
/// `visible_regions().count() == stats.drawn`. Two different numbers, and the
/// draw-call count depends on which one the active strategy consumes:
///
/// | strategy (`src/strategy.rs`) | API draw calls per frame | GPU draw slots |
/// |---|---|---|
/// | `PerDraw` (`record`, one `draw_indexed` per **visible** region) | `stats.drawn` | — |
/// | `MdiZeroInstance` (one `multi_draw_indexed_indirect`) | **1** | `drawable` (culled ones zeroed) |
/// | `MdiCount` (one `multi_draw_indexed_indirect_count`) | **1** | GPU-provided count |
///
/// That table is the honest answer to "how many draw calls does the terrain path
/// issue": it is strategy-dependent, and on any adapter that gets an MDI
/// strategy it is **1 per frame, flat in section count** — which is the #128
/// shape gate. The list sizes below are measured; the mapping to API calls is
/// read off those three `record` implementations, and this bench prints the
/// strategy `select_strategy` actually picks on this machine when an adapter is
/// available, so the applicable row is not a guess either.
///
/// **What is still not counted anywhere, and this bench cannot supply:** the
/// API-level call count as observed at the wgpu boundary, and bind-group binds.
/// wgpu exposes no counters, and the shell's `RenderStats` has `draw_calls` but
/// no bind-group field — see this file's module docs for the exact seam.
fn bench_terrain_draw_calls(c: &mut Criterion) {
    let camera = standing_camera();
    assert_eq!(section_of(camera.position), (0, 2, 0));

    // Which strategy would this machine use? A real query, not an assumption.
    match GpuContext::new_headless_blocking() {
        Ok(ctx) => {
            let kind = lodestone_render::select_strategy(ctx.capabilities());
            println!("terrain draw strategy on this adapter: {kind:?} (from select_strategy)");
        }
        Err(e) => println!("terrain draw strategy: unknown, no GPU adapter ({e})"),
    }

    let mut mdi_calls_per_rd = Vec::new();
    for rd in [8i32, 12, 16] {
        let scene = flat_scene(rd);
        let plan = scene.plan_frame(&camera);
        let loaded = scene.loaded_len();
        let drawable = plan.regions.len();
        let drawn = plan.visible_regions().count();

        // The plan and its own accounting must agree, on both numbers. A
        // mismatch means the stats describe a different frame from the one
        // submitted, which would make every figure below meaningless.
        assert_eq!(
            drawable, plan.stats.drawable,
            "rd={rd}: {drawable} regions but stats claim {} drawable",
            plan.stats.drawable
        );
        assert_eq!(
            drawn, plan.stats.drawn,
            "rd={rd}: {drawn} visible regions but stats claim {} drawn",
            plan.stats.drawn
        );
        assert!(
            drawn < drawable,
            "rd={rd}: every drawable section is visible ({drawn}/{drawable}) — culling did not \
             run, so no draw-call number here describes a real frame"
        );
        assert!(
            plan.stats.is_meaningful(),
            "rd={rd}: frame must both draw and cull: {:?}",
            plan.stats
        );

        // Per-strategy API draw calls, from the measured list sizes.
        let per_draw_calls = drawn;
        let mdi_calls = 1usize; // one multi_draw_* call for the whole frame
        mdi_calls_per_rd.push(mdi_calls);

        let scene_label = format!("flat_world rd={rd} sections={loaded}");
        println!(
            "terrain draw list: rd={rd} loaded={loaded} drawable={drawable} drawn={drawn} \
             culled_frustum={} culled_occlusion={} -> PerDraw would issue {per_draw_calls} \
             draw_indexed calls; MdiZeroInstance/MdiCount issue {mdi_calls} \
             (with {drawable} GPU draw slots)",
            plan.stats.culled_frustum, plan.stats.culled_occlusion,
        );
        for (metric, value, unit) in [
            ("terrain_drawable_sections", drawable as f64, "sections"),
            ("terrain_drawn_sections", drawn as f64, "sections"),
            ("terrain_per_draw_api_calls", per_draw_calls as f64, "calls"),
            ("terrain_mdi_api_calls", mdi_calls as f64, "calls"),
            ("terrain_mdi_draw_slots", drawable as f64, "slots"),
        ] {
            support::record(support::Record {
                bench: "render_submit",
                metric,
                scene: &scene_label,
                value,
                unit,
            });
        }
    }

    // NO assertion is made here that the indirect strategies' per-frame API
    // call count is "flat in section count". It is flat — one
    // `multi_draw_indexed_indirect` per frame, `src/strategy.rs:262` — but that
    // figure is *read off the source*, not measured, so asserting it would be
    // comparing a constant this file wrote to itself: the vacuous-gate shape
    // `CLAUDE.md` warns about, dressed as a regression gate. #128 explicitly
    // forbids substituting a code-reading argument for a measured count, and
    // the honest position is that the measured part of this bench is the
    // draw-list sizes above; the API-level count needs the shell-side counter
    // named in this file's module docs.
    let _ = mdi_calls_per_rd;

    let scene = flat_scene(12);
    c.bench_function("render_submit/terrain_plan_frame_rd12", |b| {
        b.iter(|| black_box(scene.plan_frame(black_box(&camera))))
    });
}

/// Builds `count` entity instances spread across `models` distinct model types,
/// **half in front of `camera` and half behind it**, so the plan is guaranteed
/// to both draw and cull.
///
/// Positions are derived from `Camera::forward()` — the same expression
/// `Camera::frustum()` is built from — rather than from hardcoded world
/// coordinates. The first version of this function placed a ring at hardcoded
/// positions and every single entity was culled (`drawn: 0, culled_frustum: 10`),
/// which is `CLAUDE.md`'s rule about deriving layout from the draw's own
/// expression instead of restating constants, learned again.
///
/// Fields are set directly rather than through `EntityInstance::new`, which
/// would need a baked `EntityMesh` (and therefore assets) for no benefit — the
/// subject under test is the planner's culling and grouping, which reads only
/// the AABB, the model name and the transforms.
fn entity_instances(
    count: usize,
    models: &[&'static str],
    camera: &Camera,
) -> Vec<EntityInstance> {
    let forward = camera.forward();
    let right = forward.cross(Vec3::Y).normalize_or_zero();
    (0..count)
        .map(|i| {
            // Alternate in front of / behind the camera, at a spread of
            // distances well inside the far plane (512) and outside near (0.05).
            let ahead = if i % 2 == 0 { 1.0 } else { -1.0 };
            let d = 8.0 + (i % 13) as f32 * 3.0;
            let lateral = ((i % 7) as f32 - 3.0) * 2.0;
            let pos = camera.position + forward * (ahead * d) + right * lateral;
            EntityInstance {
                model: models[i % models.len()],
                transform: Mat4::from_translation(pos),
                part_transforms: vec![Mat4::IDENTITY; 6],
                hand_transforms: [None, None],
                aabb_min: pos - Vec3::splat(0.4),
                aabb_max: pos + Vec3::splat(1.4),
                light: 15,
            }
        })
        .collect()
}

/// **Issue #106** — entity render planning and instance upload, gated the way
/// that issue asks: on counts that must stay flat as the crowd grows, not on a
/// millisecond figure.
///
/// `plan_entities` groups survivors by model type, so **the batch count is the
/// instanced-draw count**. The #75 shape (per-entity buffer, per-entity bind
/// group) would show up here as a batch count that tracks entity count. So:
///
/// * batch count is asserted `<= distinct model types`, at every crowd size
///   from 10 to 5000 — the flat-as-N-grows gate;
/// * instance count is asserted to equal `stats.drawn`, so the batches really
///   do carry every surviving entity (a planner that dropped instances would
///   otherwise look impressively flat);
/// * `stats.is_meaningful()` (drew something *and* culled something) is the
///   anti-vacuity control, so "flat" can never be achieved by culling all.
///
/// The GPU upload half runs only when an adapter is available and asserts the
/// same shape one level down: `upload_instances` produces **one buffer per
/// batch**, never per entity.
fn bench_entity_render_planning(c: &mut Criterion) {
    const MODELS: [&str; 3] = ["minecraft:zombie", "minecraft:creeper", "minecraft:pig"];
    let camera = standing_camera();
    let frustum = camera.frustum();

    for n in [10usize, 100, 1000, 5000] {
        let instances = entity_instances(n, &MODELS, &camera);
        let t = Instant::now();
        let frame = plan_entities(black_box(&instances), &frustum);
        let us = t.elapsed().as_secs_f64() * 1e6;

        assert!(
            frame.batches.len() <= MODELS.len(),
            "n={n}: {} batches for {} distinct models — the planner is emitting per-entity \
             batches, which is exactly the per-entity-draw shape #75 removed from terrain",
            frame.batches.len(),
            MODELS.len()
        );
        assert_eq!(
            frame.instance_count(),
            frame.stats.drawn,
            "n={n}: batches carry {} instances but stats claim {} drawn — the planner is losing \
             entities, and a flat batch count achieved by dropping work proves nothing",
            frame.instance_count(),
            frame.stats.drawn
        );
        assert!(
            frame.stats.is_meaningful(),
            "n={n}: the entity frame must both draw and cull something: {:?}",
            frame.stats
        );

        let scene = format!("crowd n={n} models={}", MODELS.len());
        println!(
            "entity planning: n={n} -> {} batches, {} drawn, {} culled, {us:.1}us \
             ({:.3} us/entity, PROVISIONAL: shared machine)",
            frame.batches.len(),
            frame.stats.drawn,
            frame.stats.culled_frustum,
            us / n as f64,
        );
        support::record(support::Record {
            bench: "render_submit",
            metric: "entity_plan_batches",
            scene: &scene,
            value: frame.batches.len() as f64,
            unit: "batches",
        });
        support::record(support::Record {
            bench: "render_submit",
            metric: "entity_plan_us",
            scene: &scene,
            value: us,
            unit: "us",
        });
        support::record(support::Record {
            bench: "render_submit",
            metric: "entity_plan_us_per_entity",
            scene: &scene,
            value: us / n as f64,
            unit: "us",
        });
    }

    // Instance upload: one buffer per batch, independent of crowd size.
    match GpuContext::new_headless_blocking() {
        Ok(ctx) => {
            for n in [100usize, 5000] {
                let instances = entity_instances(n, &MODELS, &camera);
                let frame = plan_entities(&instances, &frustum);
                let mut buffers = 0usize;
                let t = Instant::now();
                for batch in &frame.batches {
                    if lodestone_render::entity_pipeline::upload_instances(
                        ctx.device(),
                        &batch.transforms,
                        &batch.lights,
                    )
                    .is_some()
                    {
                        buffers += 1;
                    }
                }
                let us = t.elapsed().as_secs_f64() * 1e6;
                assert_eq!(
                    buffers,
                    frame.batches.len(),
                    "n={n}: {buffers} instance buffers for {} batches — upload must be one \
                     buffer per batch, never per entity",
                    frame.batches.len()
                );
                assert!(
                    buffers <= MODELS.len(),
                    "n={n}: {buffers} instance buffers for {} model types",
                    MODELS.len()
                );
                println!(
                    "entity instance upload: n={n} -> {buffers} buffers ({} instances), {us:.1}us \
                     PROVISIONAL",
                    frame.instance_count()
                );
                support::record(support::Record {
                    bench: "render_submit",
                    metric: "entity_upload_buffers",
                    scene: &format!("crowd n={n} models={}", MODELS.len()),
                    value: buffers as f64,
                    unit: "buffers",
                });
            }
        }
        Err(e) => {
            println!(
                "entity instance upload: SKIPPED, no GPU adapter ({e}). The planning counts above \
                 still ran; the upload buffer-count gate did not."
            );
        }
    }

    let instances = entity_instances(1000, &MODELS, &camera);
    c.bench_function("render_submit/entity_plan_1000", |b| {
        b.iter(|| black_box(plan_entities(black_box(&instances), &frustum)))
    });
}

struct MapWorld(HashMap<(i32, i32, i32), Arc<ChunkSection>>);

impl SectionSource for MapWorld {
    fn section(&self, coord: (i32, i32, i32)) -> Option<Arc<ChunkSection>> {
        self.0.get(&coord).cloned()
    }
}

fn map_world(rd: i32) -> MapWorld {
    let mut map = HashMap::new();
    for cz in -rd..=rd {
        for cx in -rd..=rd {
            let col =
                synthetic_overworld_column((cx.unsigned_abs() as u64 * 7) ^ cz.unsigned_abs() as u64);
            for si in 0..MODERN_SECTIONS {
                if let Some(arc) = col.section_arc(si) {
                    map.insert((cx, si as i32, cz), arc);
                }
            }
        }
    }
    MapWorld(map)
}

/// **Issue #160** — mesh-arena occupancy as a tracked number, at two render
/// distances, plus the two count invariants the existing
/// `gpu_world_mesher_upload_evict_roundtrip` test does not cover (it checks the
/// load→nonzero, unload→zero lifecycle once; this checks *occupancy under load*
/// and the allocation/section correspondence).
///
/// Needs a GPU adapter, because `SectionArena` is a `wgpu::Buffer` suballocator.
/// Skips with a loud message otherwise, and says explicitly which gates did not
/// run — a silent skip is the "precondition species" of vacuous test.
///
/// **Texture-atlas occupancy is not measured here, and cannot be.** Verified:
/// nothing in `lodestone-render` or `lodestone-assets` exposes atlas occupancy.
/// `AtlasStats` (`src/texture.rs:93`) reports sprite/frame *population*
/// (`total_sprites`, `static_16`, `total_frames`, `wide_sprites`), and `GpuAtlas`
/// exposes only `width`/`height` — there is no used/free/slot accessor to read,
/// so an occupancy figure would have to be derived from summed sprite rects,
/// which is a new accessor on someone else's crate rather than a measurement.
/// That half of #160 stays open; the seam is a `used_area`/`slot_occupancy`
/// method on `Atlas` or `GpuAtlas`.
fn bench_arena_occupancy(c: &mut Criterion) {
    let Ok(ctx) = GpuContext::new_headless_blocking() else {
        println!(
            "arena occupancy (#160): SKIPPED, no GPU adapter. NOT RUN: vertex/index occupancy at \
             rd=3/rd=6, the live_allocations==resident_sections count gate, and the \
             unload→zero-reclaim gate. Re-run on a machine with an adapter."
        );
        // Still register a criterion function so the bench binary's target list
        // is stable whether or not an adapter exists.
        c.bench_function("render_submit/arena_occupancy_skipped", |b| b.iter(|| black_box(0u8)));
        return;
    };

    for rd in [3i32, 6] {
        let world = map_world(rd);
        let mut mesher = WorldMesher::new(ctx.device(), 96 << 20, 48 << 20, true);
        let rows = 0..MODERN_SECTIONS as i32;
        let t = Instant::now();
        for cz in -rd..=rd {
            for cx in -rd..=rd {
                mesher.load_column(&world, ctx.queue(), &TerrainClassifier, cx, cz, rows.clone());
            }
        }
        let load_ms = t.elapsed().as_secs_f64() * 1e3;

        let vs = mesher.arena().vertex_stats();
        let is = mesher.arena().index_stats();
        let resident = mesher.arena().resident_len();
        assert!(resident > 0, "rd={rd}: nothing resident after loading — nothing measured");
        assert!(vs.used > 0, "rd={rd}: vertex arena reports zero used bytes with {resident} resident");

        // Count gate: exactly one live vertex allocation per resident section.
        // A leak (evict that forgets to free) or a double-upload would break
        // this without changing any byte total enough to notice.
        assert_eq!(
            vs.live_allocations, resident,
            "rd={rd}: {} live vertex allocations for {resident} resident sections — the arena and \
             the residency map disagree, which is a leak or a double upload",
            vs.live_allocations
        );

        let vpct = 100.0 * vs.used as f64 / vs.capacity as f64;
        let ipct = 100.0 * is.used as f64 / is.capacity as f64;
        println!(
            "arena occupancy: rd={rd} resident={resident} vertex {}/{} bytes ({vpct:.2}% of \
             capacity, fragmentation {:.4}) index {}/{} ({ipct:.2}%) — load took {load_ms:.0}ms \
             PROVISIONAL",
            vs.used, vs.capacity, vs.fragmentation(), is.used, is.capacity,
        );

        let scene = format!("fixture=synthetic_overworld_column rd={rd} resident={resident}");
        for (metric, value, unit) in [
            ("arena_vertex_used_bytes", vs.used as f64, "bytes"),
            ("arena_vertex_pct_of_capacity", vpct, "%"),
            ("arena_vertex_fragmentation", vs.fragmentation(), "x"),
            ("arena_index_used_bytes", is.used as f64, "bytes"),
            ("arena_index_pct_of_capacity", ipct, "%"),
            ("arena_resident_sections", resident as f64, "sections"),
            ("arena_bytes_per_section", vs.used as f64 / resident as f64, "bytes"),
        ] {
            support::record(support::Record {
                bench: "render_submit",
                metric,
                scene: &scene,
                value,
                unit,
            });
        }

        // Reclaim gate at session shape: unload everything and require the
        // arena to return to exactly zero. This is the control that proves the
        // occupancy numbers above are measuring live residency rather than
        // monotonically-growing allocation.
        for cz in -rd..=rd {
            for cx in -rd..=rd {
                mesher.unload_column(cx, cz, rows.clone());
            }
        }
        let after = mesher.arena().vertex_stats();
        assert_eq!(
            after.used, 0,
            "rd={rd}: vertex arena still holds {} bytes after unloading every column — the \
             occupancy figures above would then be growth, not residency",
            after.used
        );
        assert_eq!(after.live_allocations, 0, "rd={rd}: live allocations survived a full unload");
    }

    c.bench_function("render_submit/arena_occupancy_measured", |b| b.iter(|| black_box(1u8)));
}

criterion_group!(
    benches,
    bench_terrain_draw_calls,
    bench_entity_render_planning,
    bench_arena_occupancy
);
criterion_main!(benches);
