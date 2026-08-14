//! `RenderState::render_inner`'s CPU submit cost (#133) and the terrain
//! path's draw-call/bind-group counts (#128), for the packed/demo terrain
//! path — the one live GPU path this crate can stand up with no `client.jar`.
//!
//! # Why the demo path, and why that is a legitimate stand-in here
//!
//! `crates/lodestone-render/benches/render_submit.rs`'s own module doc names
//! the exact seams this file exists to close: `RenderState::render_inner` is
//! private and `RenderStats` has no bind-group field, so neither #128's
//! bind-group half nor #133 could be built from `lodestone-render` — both are
//! `lodestone-shell` types. **The claim that `lodestone-shell` had no
//! `benches/` directory was stale by the time it was checked**:
//! `benches/entity_tick.rs` already existed. What was missing was a bench
//! *in* that directory pointed at the terrain-submit path, and the two public
//! seams needed to read it — `RenderStats::terrain_camera_bind_group_switches`
//! and `RenderState::{model,packed}_origin_arena_stats` — added alongside
//! this file (`crates/lodestone-shell/src/gpu/{stats,terrain,frame}.rs`).
//!
//! The demo/packed world (`crate::worldgen::generate`) needs no vanilla
//! `client.jar`: every headless GPU test in `gpu/pixel_gates.rs` and
//! `gpu/sections.rs` already builds against it for exactly that reason. It is
//! also the path issue #76 most recently touched (the packed table's own
//! shared camera + dynamic-offset arena), so it is a real regression surface,
//! not a synthetic stand-in invented for this bench. The **live-vanilla model
//! path** (`self.model`, built only when `RenderState::new` is given a real
//! `BlockAtlas`) needs `crate::resources::BlockResources::load(true)`, which
//! degrades to `None` without a jar rather than failing — so it is not
//! reachable from a hermetic bench that must run the same way in CI and on a
//! machine with `.cache/mc/26.2` present. That half is out of scope here;
//! `gpu/pixel_gates.rs`'s `#[ignore]`d GPU tests already exercise it
//! qualitatively (pixel readback), just not as a tracked count/duration.
//!
//! The demo world's own cap (`sim.rs`'s `MAX_WORLD_RADIUS = 6`, ~4056
//! sections) lands within the same order of magnitude as issue #75's own
//! profile (`sections=3880`, peaking near 5000) — see
//! `docs/section-camera-uniform.md`'s `SectionOriginArena` doc for the
//! arithmetic — so a radius-6 sweep point is not an arbitrary choice, it is
//! this path's own realistic ceiling.
//!
//! # What is asserted (count) vs. recorded (duration)
//!
//! Per `CLAUDE.md`, a duration is a species of vacuous test whose flaw is not
//! readable from the test source, and a wall-clock number taken on a shared
//! machine is a sample, not a measurement. So:
//!
//! * `draw_calls >= sections_drawn`, `terrain_camera_bind_group_switches <=
//!   1`, and — the real shape gate — `draw_calls - sections_drawn` identical
//!   across radius 1/3/6, are real assertions. That last one exists because
//!   the first cut of this bench assumed `draw_calls == sections_drawn`
//!   exactly and it was wrong: the first-person bare-arm draw fires on this
//!   path too (needs no vanilla pack), so the honest claim is "the non-terrain
//!   overhead is a per-frame constant", not "there is no overhead" — the
//!   measured counterpart to #128's "bind-group count independent of section
//!   count" ask, not a code-reading argument for it (`CLAUDE.md`'s own
//!   example of the mistake #128 explicitly forbids).
//! * CPU submit time is recorded via `support::record` as a provisional
//!   baseline only, exactly like `render_submit.rs`'s existing timings, with
//!   no pass/fail — #133 asks for the *shape* (flat-ish per-section
//!   dispatch cost as section count grows), which the arena/bind-group count
//!   gates above already certify; the millisecond figure is a secondary,
//!   noise-affected number to watch over time.
//!
//! Run with `cargo bench -p lodestone-shell --bench render_submit`. Needs a
//! GPU adapter; skips loudly (registering a stable criterion target either
//! way) when none is available.

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};

use lodestone::blocks::DemoClassifier;
use lodestone::gpu::RenderState;
use lodestone::mesher::{SectionGeometry, SectionKey, mesh_snapshot, snapshot_section};
use lodestone::worldgen;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

/// Build a `RenderState` with a `radius`-chunk packed/demo world uploaded (no
/// vanilla jar needed — see the module doc). Returns the state plus the
/// number of sections that meshed to non-empty geometry, so a caller can
/// assert against a real count rather than assume the world populated.
fn build_demo_world(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    radius: i32,
) -> (RenderState, usize) {
    let world = worldgen::generate(radius);
    let classifier = DemoClassifier;
    let mut state = RenderState::new(device, queue, FORMAT, WIDTH, HEIGHT, None);
    let mut sections = 0usize;
    for cz in -radius..=radius {
        for cx in -radius..=radius {
            for si in 0..worldgen::SECTION_COUNT {
                let key = SectionKey {
                    cx,
                    cz,
                    si,
                    min_y: worldgen::MIN_Y,
                };
                let Some(snap) = snapshot_section(&world, key) else {
                    continue;
                };
                let mesh = mesh_snapshot(&snap, &classifier);
                if mesh.indices.is_empty() {
                    continue;
                }
                sections += 1;
                state.upload_section(device, queue, key, &SectionGeometry::Packed(mesh));
            }
        }
    }
    (state, sections)
}

/// A camera positioned and far-planed to see the whole `radius`-chunk world,
/// mirroring `gpu/sections.rs`'s own headless test camera.
fn camera_for(radius: i32) -> Camera {
    let feet = worldgen::spawn_feet();
    Camera {
        position: glam::Vec3::new(feet[0] as f32, feet[1] as f32 + 6.0, feet[2] as f32 - 18.0),
        yaw: 0.0,
        pitch: 22.0,
        fov_y_degrees: 70.0,
        aspect: WIDTH as f32 / HEIGHT as f32,
        near: 0.05,
        far: Camera::far_for_render_distance((radius.max(8)) as u32, 0),
    }
}

/// **Issues #128/#133** — terrain draw-call and camera-bind-group counts, plus
/// a CPU submit-time baseline, swept across section counts spanning issue
/// #75's own measured order of magnitude via the packed/demo path.
fn bench_terrain_submit(c: &mut Criterion) {
    let Ok(ctx) = GpuContext::new_headless_blocking() else {
        println!(
            "terrain_submit (#128/#133): SKIPPED, no GPU adapter. NOT RUN: the \
             draw_calls==sections_drawn count gate, the terrain_camera_bind_group_switches \
             flatness gate, and the CPU submit-time baseline. Re-run on a machine with an \
             adapter."
        );
        // Still register a criterion function so the bench binary's target list is
        // stable whether or not an adapter exists.
        c.bench_function("render_submit/terrain_submit_skipped", |b| b.iter(|| black_box(0u8)));
        return;
    };
    let device = ctx.device();
    let queue = ctx.queue();
    let mut target = HeadlessTarget::new(device, WIDTH, HEIGHT, FORMAT);

    // `draw_calls - sections_drawn` per radius: **not** asserted to be zero.
    // First run found it is not — `RenderState::render` also draws the
    // first-person bare arm every frame (`gpu/first_person.rs`'s
    // `FirstPersonHand::Arm` branch draws once per rig part, 2 parts on this
    // pack-less fallback rig), which needs no vanilla pack and so fires on
    // this demo-only path too: radius=1 measured 48 draw calls for 46
    // sections drawn, a real, reproducible +2, not noise (identical on a
    // second run). That is a genuine per-frame overhead this bench did not
    // expect going in — the useful, measured claim is not "zero overhead" but
    // "the overhead does not scale with section count", so this collects the
    // per-radius difference and asserts they are all equal after the sweep.
    let mut extra_draw_calls_per_radius: Vec<(i32, usize)> = Vec::new();

    for radius in [1i32, 3, 6] {
        let (state, sections) = build_demo_world(device, queue, radius);
        assert!(sections > 0, "radius={radius}: some sections must have meshed");
        let camera = camera_for(radius);

        // Warm-up: the first frame pays one-time driver/allocator costs #75's
        // profile was not measuring (a steady-state per-frame cost is).
        {
            let frame = target.acquire().expect("headless acquire");
            let _ = state.render(device, queue, frame.view(), &camera, None, &[]);
        }

        const ITERS: usize = 20;
        let mut times_us: Vec<f64> = Vec::with_capacity(ITERS);
        let mut last_stats = None;
        for _ in 0..ITERS {
            let frame = target.acquire().expect("headless acquire");
            let start = Instant::now();
            let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
            times_us.push(start.elapsed().as_secs_f64() * 1e6);
            last_stats = Some(stats);
        }
        times_us.sort_by(f64::total_cmp);
        let median_us = times_us[times_us.len() / 2];
        let stats = last_stats.expect("at least one frame rendered above");

        // The count gate the #75 shape actually claims: the *terrain* portion
        // of `draw_calls` is one call per drawn section (the culled remainder
        // is the reduction, not a bind-group cost), plus a frame-constant
        // non-terrain overhead (the first-person arm — see the note above the
        // loop) that must **not** scale with section count. `>=` first, so a
        // future regression that drops below one call per section (a hole in
        // the world) fails loudly rather than silently passing the
        // flat-overhead check below.
        assert!(
            stats.draw_calls >= stats.sections_drawn,
            "radius={radius}: {} draw calls for {} sections drawn — fewer calls than drawn \
             sections on a path with no batching (packed sections each bind their own buffers), \
             which means some section drew without a call or was double-counted",
            stats.draw_calls,
            stats.sections_drawn
        );
        extra_draw_calls_per_radius.push((radius, stats.draw_calls - stats.sections_drawn));

        // At most one terrain camera bind-group *object* used across the
        // whole frame (see `RenderStats::terrain_camera_bind_group_switches`'s
        // doc). Only the packed path draws terrain here (`vanilla: None`), so
        // a value above 1 would mean the packed table stopped sharing
        // `packed_cam_bind_group` — the exact reversal issue #76 fixed.
        assert!(
            stats.terrain_camera_bind_group_switches <= 1,
            "radius={radius}: {} terrain camera bind-group switches across {} sections drawn — \
             the shared-bind-group shape issues #75/#76 fixed has regressed toward one \
             bind-group per section (a count that would scale with section count, not stay at \
             1)",
            stats.terrain_camera_bind_group_switches,
            stats.sections_drawn,
        );

        // Arena headroom against `PACKED_ORIGIN_ARENA_SLOTS`'s fixed ceiling
        // (issue #133's ceiling-approach ask) — `AllocStats` carries bytes and
        // a live-allocation count, not the slot constant itself (`pub(super)`,
        // deliberately not widened further than the accessor this pass
        // added), so occupancy is reported as a percentage of byte capacity
        // rather than a slot count against a constant this bench cannot name.
        // `+ 1`, not `sections` exactly: `SectionOriginArena::new` reserves
        // slot 0 permanently at construction (a zeroed, always-live
        // allocation the dropped-item/first-person-item passes bind), for
        // both the packed and model arenas alike — see that type's doc. First
        // run of this bench asserted plain `sections` and found 53 for 52 at
        // radius=1: not a leak, the permanent reservation counted once.
        let arena = state.packed_origin_arena_stats();
        assert_eq!(
            arena.live_allocations, sections + 1,
            "radius={radius}: {} live origin-arena slots for {sections} meshed sections (+1 \
             expected for the permanently reserved zero slot) — a leak or a double-allocation \
             in the packed upload path",
            arena.live_allocations
        );
        let arena_pct = 100.0 * arena.used as f64 / arena.capacity as f64;
        println!(
            "terrain submit (packed/demo): radius={radius} sections_meshed={sections} \
             sections_drawn={} draw_calls={} bind_group_switches={} origin_arena \
             {}/{} bytes ({arena_pct:.2}%, {} live slots) median={median_us:.1}us over {ITERS} \
             frames PROVISIONAL: shared machine",
            stats.sections_drawn,
            stats.draw_calls,
            stats.terrain_camera_bind_group_switches,
            arena.used,
            arena.capacity,
            arena.live_allocations,
        );

        let scene = format!("demo world radius={radius} sections_drawn={}", stats.sections_drawn);
        for (metric, value, unit) in [
            ("terrain_sections_drawn", stats.sections_drawn as f64, "sections"),
            ("terrain_draw_calls", stats.draw_calls as f64, "calls"),
            (
                "terrain_bind_group_switches",
                stats.terrain_camera_bind_group_switches as f64,
                "switches",
            ),
            ("terrain_submit_median_us", median_us, "us"),
            (
                "packed_origin_arena_live_slots",
                arena.live_allocations as f64,
                "slots",
            ),
            ("packed_origin_arena_pct_of_capacity", arena_pct, "%"),
        ] {
            support::record(support::Record {
                bench: "render_submit",
                metric,
                scene: &scene,
                value,
                unit,
            });
        }
    }

    // The actual #75/#128 shape gate for the non-terrain overhead identified
    // above: whatever it is, it must be the **same number** at radius 1, 3
    // and 6 — a per-frame constant, not something that grows with the world.
    // If it ever started scaling with section count, this is where it would
    // be caught; a bare `assert_eq!(draw_calls, sections_drawn)` would not
    // have caught that either, since it already fails on the constant term
    // alone (see the note above the loop).
    let first = extra_draw_calls_per_radius[0].1;
    assert!(
        extra_draw_calls_per_radius.iter().all(|&(_, n)| n == first),
        "non-terrain draw-call overhead is not flat across section counts: {:?} — the constant \
         (first-person arm) term above should not depend on how much terrain is resident",
        extra_draw_calls_per_radius
    );
    println!(
        "non-terrain draw-call overhead: {first} calls, flat across radii {:?} \
         (first-person arm draw, present even with vanilla: None)",
        extra_draw_calls_per_radius.iter().map(|&(r, _)| r).collect::<Vec<_>>()
    );

    let (state, _sections) = build_demo_world(device, queue, 3);
    let camera = camera_for(3);
    c.bench_function("render_submit/terrain_render_radius3", |b| {
        b.iter(|| {
            let frame = target.acquire().expect("headless acquire");
            black_box(state.render(device, queue, frame.view(), black_box(&camera), None, &[]))
        });
    });
}

criterion_group!(benches, bench_terrain_submit);
criterion_main!(benches);
