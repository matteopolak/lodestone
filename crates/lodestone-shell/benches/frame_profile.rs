//! Where a frame actually goes — **CPU and GPU side by side**, over a fixed
//! camera path across a fixed world, so two runs are comparable.
//!
//! # The question this exists to answer
//!
//! Every number the shell's live frame profiler reported before this bench
//! landed was **CPU** time: how long it took to *record* commands. That says
//! almost nothing about how long the GPU took to execute them —
//! `queue.submit` only enqueues. A frame can be 5 ms of CPU recording and
//! 20 ms of GPU work, and optimising the recording is then wasted effort. So
//! the first thing this prints, per waypoint, is a **verdict line**: the CPU
//! cost of recording the world command buffer next to the GPU cost of
//! executing it.
//!
//! The GPU figures come from real `TIMESTAMP_QUERY` pass timings
//! (`gpu::gpu_timing`), not from CPU spans around `submit`. Four segments:
//! `world_total` (the whole world command buffer), `world` (the block pass
//! alone), `first_person` (the hand pass alone) and `hud_total` (everything
//! submitted after the world command buffer — near-zero *in this harness*,
//! which drives `RenderState` directly and has no HUD; see the control below).
//!
//! # What is asserted, and what is merely recorded
//!
//! Per `CLAUDE.md`, a wall-clock duration taken on a shared machine is a
//! sample and not a measurement, so nothing here passes or fails on a
//! millisecond figure. What *is* asserted is either a count or a relation
//! between two numbers measured in the same run:
//!
//! * **`world_total`'s median is at or above the block pass's median.** The
//!   instrument validating itself: a mis-stamped span reads as near-zero or
//!   garbage, not as a few percent under the pass it encloses, so this still
//!   fails outright if the bracketing stamps are recorded in the wrong order,
//!   if the resolve executes before an edge is written, or if the four segment
//!   indices are rebased by a reordering of `RenderState`'s segment-name list.
//!
//!   The stronger, obvious forms are **not** invariants here, and both were
//!   tried and observed failing. `world_total >= world + first_person` failed
//!   on the first run (0.674 ms of span against 0.797 ms of summed passes at
//!   `high_down`): passes on a tile-based deferred GPU pipeline rather than
//!   execute serially, so summed pass durations legitimately exceed the wall
//!   span containing them — **a sum of per-pass GPU numbers is not a frame's
//!   GPU time**. Weakening it to `>= max(pass)` failed too, once in thirty
//!   readbacks, for two further reasons: an empty bracketing pass has no work
//!   and can retire before a long pass it nominally encloses finishes its
//!   fragment stage, and `results_ms` holds each segment's last good reading
//!   independently, so under readback backpressure two segments in one report
//!   can come from different frames. The bracket is an **estimate, not a
//!   bound**. Both per-readback violation rates are printed as measurements
//!   rather than folded into a tolerance, which would be fitting a threshold
//!   to the answer.
//! * **Residency does not move under pure rotation.** `CLAUDE.md` records
//!   `vram_bytes` having once been accumulated *inside* the terrain draw loops
//!   after the cull — a per-frame *drawn* quantity wearing a *residency*
//!   label, which moved 26% when the camera turned on the spot. Turning the
//!   camera cannot change what is resident, so this bench turns it 180° from
//!   one eye position and requires `vram_bytes` to be **byte-identical**,
//!   while requiring `sections_drawn` to actually differ (otherwise the
//!   control has not established that the rotation did anything at all).
//! * **Every GPU segment produces a reading.** A query pool that has never
//!   resolved reports `None`, and a caller must render that as "no data", not
//!   as `0.0 ms`. After a warm-up longer than the readback ring, `None` means
//!   the instrument is broken, so it is a failure here rather than a blank
//!   column.
//!
//! Everything else — the millisecond medians — is recorded through
//! `support::record` against a same-machine, same-scene baseline, exactly like
//! `render_submit.rs`'s timings, and printed with an explicit noise estimate
//! (see `Samples::noise`) so a figure gathered while the machine was busy says
//! so instead of being quietly attributed to a code change.
//!
//! # What this cannot see
//!
//! The demo/packed world (`crate::worldgen::generate`) needs no vanilla
//! `client.jar`, which is why every headless GPU test in this crate uses it —
//! but it means the **live-vanilla model path** (`RenderState`'s model arena,
//! built only from a real `BlockAtlas`) is not exercised, and that is the path
//! a real session draws through. The `hud_total` segment is likewise near-zero
//! here for a structural reason and not a happy one: nothing in this harness
//! submits a HUD. Both are stated rather than left for a reader to infer from
//! a suspiciously small number.
//!
//! Run with `just bench-frame`, or
//! `cargo bench -p lodestone-shell --bench frame_profile`. Needs a GPU
//! adapter; skips loudly (registering a stable criterion target either way)
//! when none is available.

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
/// 1280x720 rather than `render_submit.rs`'s 320x240. Fragment cost scales
/// with pixels and this bench's whole point is the GPU half: at 320x240 the
/// block pass is vertex-bound on any modern adapter and the GPU figure would
/// answer a question nobody asked.
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

/// Demo-world radius. 6 is `sim.rs`'s own `MAX_WORLD_RADIUS` — this path's
/// realistic ceiling, not an arbitrary pick; see `render_submit.rs`'s module
/// doc for the arithmetic.
const RADIUS: i32 = 6;

/// Frames rendered and discarded before any sample is kept. Must exceed
/// `gpu_timing`'s `FRAMES_IN_FLIGHT` readback ring (3) by enough that the
/// first *kept* frame already has a resolved GPU reading — and it also pays
/// the one-time driver/allocator costs a steady-state per-frame figure is not
/// interested in.
const WARMUP: usize = 12;

/// Frames kept per waypoint. A median over 30 is stable enough to compare
/// across runs while keeping the whole sweep under a few seconds.
const ITERS: usize = 30;

/// Build a `RenderState` with a `radius`-chunk packed/demo world uploaded.
/// Mirrors `render_submit.rs`'s helper — deliberately duplicated rather than
/// shared, because `benches/support.rs` is the recording helper and growing it
/// into a scene library would make two benches co-vary on one fixture, which
/// is the shared-construction-path blindness `CLAUDE.md` warns about.
fn build_demo_world(device: &wgpu::Device, queue: &wgpu::Queue, radius: i32) -> (RenderState, usize) {
    let world = worldgen::generate(radius);
    let classifier = DemoClassifier;
    let mut state = RenderState::new(device, queue, FORMAT, WIDTH, HEIGHT, None);
    let mut sections = 0usize;
    for cz in -radius..=radius {
        for cx in -radius..=radius {
            for si in 0..worldgen::SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: worldgen::MIN_Y };
                let Some(snap) = snapshot_section(&world, key) else { continue };
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

/// One point on the fixed camera path: an eye offset from spawn plus a look
/// direction.
struct Waypoint {
    label: &'static str,
    offset: glam::Vec3,
    yaw: f32,
    pitch: f32,
}

/// The path. Four waypoints chosen to put the renderer in genuinely different
/// regimes rather than to sample one regime four times:
///
/// * `ground_forward` — eye at spawn height, level. The ordinary case.
/// * `ground_oblique` — same eye, yawed **37°**. Not 45 and not 90: an
///   axis-aligned yaw makes the frustum symmetric about the chunk grid, which
///   is exactly the coincidence `CLAUDE.md`'s round-number rule warns about
///   for a fixture *input*, and it would make the cull's two halves agree by
///   construction.
/// * `high_down` — 46 blocks up, pitched down 53°. Maximum sections in view;
///   this is where a draw-call-bound frame shows up.
/// * `low_up` — inside the terrain looking up. Minimum sections in view, so a
///   cost that does *not* fall here is not terrain cost.
const PATH: [Waypoint; 4] = [
    Waypoint { label: "ground_forward", offset: glam::Vec3::new(0.0, 6.0, -18.0), yaw: 0.0, pitch: 0.0 },
    Waypoint { label: "ground_oblique", offset: glam::Vec3::new(0.0, 6.0, -18.0), yaw: 37.0, pitch: 0.0 },
    Waypoint { label: "high_down", offset: glam::Vec3::new(11.0, 46.0, -7.0), yaw: 37.0, pitch: 53.0 },
    Waypoint { label: "low_up", offset: glam::Vec3::new(-13.0, 2.0, 5.0), yaw: 197.0, pitch: -41.0 },
];

fn camera_at(offset: glam::Vec3, yaw: f32, pitch: f32) -> Camera {
    let feet = worldgen::spawn_feet();
    Camera {
        position: glam::Vec3::new(feet[0] as f32, feet[1] as f32, feet[2] as f32) + offset,
        yaw,
        pitch,
        fov_y_degrees: 70.0,
        aspect: WIDTH as f32 / HEIGHT as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RADIUS.max(8) as u32, 0),
    }
}

/// A kept series of one quantity, in milliseconds.
#[derive(Default)]
struct Samples(Vec<f64>);

impl Samples {
    fn push(&mut self, ms: f64) {
        self.0.push(ms);
    }

    fn median(&self) -> f64 {
        let mut v = self.0.clone();
        v.sort_by(f64::total_cmp);
        if v.is_empty() { 0.0 } else { v[v.len() / 2] }
    }

    /// `max / median`, the harness's own statement about how noisy the machine
    /// was while it ran.
    ///
    /// `CLAUDE.md` is explicit that a timing gathered while other work runs on
    /// the machine gets attributed to the wrong cause, and equally explicit
    /// that load average is the worst available proxy for that. So this asks
    /// the samples themselves: a quiet run's slowest frame sits close to its
    /// median, and a run that was fighting for a core does not. It is a
    /// property of the measurement rather than of the machine, which is the
    /// only thing this process can honestly observe.
    fn noise(&self) -> f64 {
        let mut v = self.0.clone();
        v.sort_by(f64::total_cmp);
        match (v.last(), self.median()) {
            (Some(max), med) if med > 0.0 => max / med,
            _ => 1.0,
        }
    }
}

/// Look up one GPU segment by name in a `gpu_timing_report()` result.
/// `Err(())` distinguishes "the segment exists but has never resolved" from
/// "there is no such segment", because those two want different diagnoses and
/// both would otherwise read as a missing number.
fn segment(report: &[(&'static str, Option<f32>)], name: &str) -> Result<Option<f32>, String> {
    report
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, ms)| *ms)
        .ok_or_else(|| {
            format!(
                "no GPU segment named {name:?}; RenderState's segment list has {:?}. A segment \
                 renamed on one side only is invisible to every compiler check here.",
                report.iter().map(|(n, _)| *n).collect::<Vec<_>>()
            )
        })
}

fn bench_frame_profile(c: &mut Criterion) {
    let Ok(ctx) = GpuContext::new_headless_blocking() else {
        println!(
            "frame_profile: SKIPPED, no GPU adapter. NOT RUN: the per-waypoint CPU-vs-GPU \
             verdict, the world_total >= world + first_person span gate, and the \
             rotation-invariance control on vram_bytes. Re-run on a machine with an adapter."
        );
        c.bench_function("frame_profile/skipped", |b| b.iter(|| black_box(0u8)));
        return;
    };
    let device = ctx.device();
    let queue = ctx.queue();
    let mut target = HeadlessTarget::new(device, WIDTH, HEIGHT, FORMAT);
    let (state, meshed) = build_demo_world(device, queue, RADIUS);
    assert!(meshed > 0, "the demo world must mesh some sections");

    if !state.gpu_timing_available() {
        println!(
            "frame_profile: this device was NOT granted Features::TIMESTAMP_QUERY, so every GPU \
             column below is absent rather than zero. The CPU columns and the count gates still \
             ran; the CPU-vs-GPU verdict did not."
        );
    }

    println!(
        "\n=== frame profile: demo world radius={RADIUS}, {meshed} meshed sections, \
         {WIDTH}x{HEIGHT}, {ITERS} frames per waypoint after {WARMUP} warm-up ===\n"
    );

    for wp in &PATH {
        let camera = camera_at(wp.offset, wp.yaw, wp.pitch);

        for _ in 0..WARMUP {
            let frame = target.acquire().expect("headless acquire");
            let _ = state.render(device, queue, frame.view(), &camera, None, &[]);
            state.gpu_timing_end_frame(device, queue);
            let _ = state.take_world_subphase_report();
        }

        let mut cpu_world = Samples::default();
        let mut cpu_timing_end = Samples::default();
        let mut sub_prepare = Samples::default();
        let mut sub_terrain = Samples::default();
        let mut sub_other = Samples::default();
        let mut sub_submit = Samples::default();
        let mut gpu_total = Samples::default();
        let mut gpu_block = Samples::default();
        let mut gpu_hand = Samples::default();
        let mut gpu_hud = Samples::default();
        let mut last_stats = None;
        let mut visited = None;
        // Two different relations, counted separately, because only one of
        // them is an invariant — see the assertion block below.
        let mut span_shorter_than_a_pass = 0usize;
        let mut span_shorter_than_the_sum = 0usize;

        for _ in 0..ITERS {
            let frame = target.acquire().expect("headless acquire");
            let t0 = Instant::now();
            let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
            cpu_world.push(t0.elapsed().as_secs_f64() * 1e3);

            let t1 = Instant::now();
            state.gpu_timing_end_frame(device, queue);
            cpu_timing_end.push(t1.elapsed().as_secs_f64() * 1e3);

            let (subs, counts) = state.take_world_subphase_report();
            for (name, ms) in subs {
                let Some(ms) = ms else { continue };
                match name {
                    "world.prepare_buffers" => sub_prepare.push(f64::from(ms)),
                    "world.terrain_cull_draw" => sub_terrain.push(f64::from(ms)),
                    "world.other_draws" => sub_other.push(f64::from(ms)),
                    "world.submit" => sub_submit.push(f64::from(ms)),
                    other => panic!(
                        "unknown world sub-phase {other:?} — this bench's match arms and \
                         gpu::gpu_timing::WorldSubphase have drifted apart"
                    ),
                }
            }
            if counts.is_some() {
                visited = counts;
            }

            let report = state.gpu_timing_report();
            if !report.is_empty() {
                for (name, sink) in [
                    ("world_total", &mut gpu_total),
                    ("world", &mut gpu_block),
                    ("first_person", &mut gpu_hand),
                    ("hud_total", &mut gpu_hud),
                ] {
                    match segment(&report, name).unwrap_or_else(|e| panic!("{e}")) {
                        Some(ms) => sink.push(f64::from(ms)),
                        // A `None` this late is the instrument failing, not an
                        // empty column: WARMUP already exceeds the readback
                        // ring. Asserted after the loop so the message can name
                        // the waypoint and the segment together.
                        None => {}
                    }
                }
                // The span relation is checked **per readback**, not between
                // three independently-taken medians: a median of sums is not
                // a sum of medians, and mixing them is how an aggregate
                // invents a violation that no individual frame committed.
                if let (Ok(Some(t)), Ok(Some(w)), Ok(Some(f))) = (
                    segment(&report, "world_total"),
                    segment(&report, "world"),
                    segment(&report, "first_person"),
                ) {
                    if t < w || t < f {
                        span_shorter_than_a_pass += 1;
                    }
                    if t < w + f {
                        span_shorter_than_the_sum += 1;
                    }
                }
            }
            last_stats = Some(stats);
        }

        let stats = last_stats.expect("at least one frame rendered");

        if state.gpu_timing_available() {
            for (name, s) in [
                ("world_total", &gpu_total),
                ("world", &gpu_block),
                ("first_person", &gpu_hand),
                ("hud_total", &gpu_hud),
            ] {
                assert!(
                    !s.0.is_empty(),
                    "waypoint {}: GPU segment {name:?} produced no reading across {ITERS} frames \
                     after {WARMUP} warm-up frames — the readback ring is 3 deep, so this is the \
                     query pool failing, not latency. A blank column here would read as \"that \
                     pass is free\".",
                    wp.label
                );
            }
            // The instrument validating itself — **at the median**, not per
            // readback, and that distinction was paid for twice.
            //
            // The obvious invariant is that a span cannot be shorter than
            // what it brackets. It is not true here, in either form. Asserting
            // `world_total >= world + first_person` failed on the first run
            // (0.674 ms of span against 0.797 ms of summed passes at
            // `high_down`) because passes on a tile-based deferred GPU
            // pipeline rather than execute serially, so summed pass durations
            // legitimately exceed the wall span containing them. Weakening it
            // to `>= max(pass)` failed too, once in thirty readbacks, for two
            // further reasons: an empty bracketing pass has no work and can
            // retire before a long pass it nominally encloses finishes its
            // fragment stage, and `GpuQueryTimer::results_ms` holds each
            // segment's *last good* reading independently, so under readback
            // backpressure two segments in one report can come from different
            // frames.
            //
            // So the bracket is an **estimate, not a bound**, and the honest
            // assertion is the one that still fails loudly for a real pairing
            // bug — a mis-stamped span reads as near-zero or garbage, not as
            // "a few percent under the block pass" — while tolerating the
            // hardware's actual behaviour. Both per-readback violation rates
            // are printed as measurements rather than folded into a tolerance,
            // because a threshold placed there would be fitted to the answer.
            assert!(
                gpu_total.median() >= gpu_block.median(),
                "waypoint {}: world_total median {:.3} ms is below the block pass median \
                 {:.3} ms it brackets. Occasional per-readback inversions are expected (pass \
                 pipelining, and per-segment readback staleness), but the medians crossing means \
                 the timestamps are being paired wrongly — check the stamp order in gpu::frame \
                 and that the resolve executes after both edges.",
                wp.label,
                gpu_total.median(),
                gpu_block.median(),
            );
        }

        let cpu_ms = cpu_world.median();
        let gpu_ms = gpu_total.median();
        let verdict = if !state.gpu_timing_available() {
            "GPU timing unavailable on this device".to_string()
        } else if gpu_ms > cpu_ms * 1.25 {
            format!("GPU-bound ({:.2}x the CPU recording cost)", gpu_ms / cpu_ms.max(1e-9))
        } else if cpu_ms > gpu_ms * 1.25 {
            format!("CPU-bound ({:.2}x the GPU execution cost)", cpu_ms / gpu_ms.max(1e-9))
        } else {
            "balanced — neither side is more than 25% ahead".to_string()
        };

        let (packed_visited, model_visited) = visited.unwrap_or((0, 0));
        println!(
            "-- {} (yaw {:.0}, pitch {:.0})\n\
             \x20  verdict           {verdict}\n\
             \x20  cpu  world encode {:>8.3} ms   (noise max/median {:.2}x)\n\
             \x20  cpu  gpu-timing   {:>8.3} ms   <- this instrument's own per-frame cost\n\
             \x20  cpu  .prepare_buf {:>8.3} ms\n\
             \x20  cpu  .cull+draw   {:>8.3} ms\n\
             \x20  cpu  .other_draws {:>8.3} ms\n\
             \x20  cpu  .submit      {:>8.3} ms\n\
             \x20  gpu  world_total  {:>8.3} ms   (noise max/median {:.2}x)\n\
             \x20  gpu  world (block){:>8.3} ms\n\
             \x20  gpu  first_person {:>8.3} ms\n\
             \x20  gpu  hud_total    {:>8.3} ms   <- no HUD in this harness; see the module doc\n\
             \x20  gpu  bracket fit  {:>7.0}% of readbacks summed above the span, {:.0}% had one \
             pass above it\n\
             \x20  cnt  sections     {} drawn / {} visited packed + {} model\n\
             \x20  cnt  culled       {} distance, {} frustum, {} occlusion\n\
             \x20  cnt  draw_calls   {}, quads {}, entities {}\n\
             \x20  cnt  residency    {} bytes resident, {} reserved\n\
             \x20  cnt  readback     {} stalled frames\n",
            wp.label,
            wp.yaw,
            wp.pitch,
            cpu_ms,
            cpu_world.noise(),
            cpu_timing_end.median(),
            sub_prepare.median(),
            sub_terrain.median(),
            sub_other.median(),
            sub_submit.median(),
            gpu_ms,
            gpu_total.noise(),
            gpu_block.median(),
            gpu_hand.median(),
            gpu_hud.median(),
            100.0 * span_shorter_than_the_sum as f64 / ITERS as f64,
            100.0 * span_shorter_than_a_pass as f64 / ITERS as f64,
            stats.sections_drawn,
            packed_visited,
            model_visited,
            stats.sections_culled_distance,
            stats.sections_culled_frustum,
            stats.sections_culled_occlusion,
            stats.draw_calls,
            stats.total_quads,
            stats.entities_drawn,
            stats.vram_bytes,
            stats.vram_reserved_bytes,
            state.gpu_timing_stalled_frames(),
        );

        let scene = format!("demo radius={RADIUS} {}x{HEIGHT} waypoint={}", WIDTH, wp.label);
        for (metric, value, unit) in [
            ("cpu_world_encode_median_ms", cpu_ms, "ms"),
            ("cpu_gpu_timing_end_median_ms", cpu_timing_end.median(), "ms"),
            ("cpu_prepare_buffers_median_ms", sub_prepare.median(), "ms"),
            ("cpu_terrain_cull_draw_median_ms", sub_terrain.median(), "ms"),
            ("cpu_other_draws_median_ms", sub_other.median(), "ms"),
            ("cpu_submit_median_ms", sub_submit.median(), "ms"),
            ("gpu_world_total_median_ms", gpu_total.median(), "ms"),
            ("gpu_world_block_pass_median_ms", gpu_block.median(), "ms"),
            ("gpu_first_person_median_ms", gpu_hand.median(), "ms"),
            ("gpu_hud_total_median_ms", gpu_hud.median(), "ms"),
            ("sections_drawn", stats.sections_drawn as f64, "sections"),
            ("draw_calls", stats.draw_calls as f64, "calls"),
            ("total_quads", stats.total_quads as f64, "quads"),
            ("resident_mesh_bytes", stats.vram_bytes as f64, "bytes"),
        ] {
            support::record(support::Record {
                bench: "frame_profile",
                metric,
                scene: &scene,
                value,
                unit,
            });
        }
    }

    rotation_does_not_move_residency(device, queue, &mut target, &state);
    submit_cost_versus_residency(device, queue, &mut target);

    // A criterion target so the bench binary's function list is stable
    // whether or not an adapter exists. The medians above are the actual
    // output; this exists so `cargo bench` has something to report.
    let camera = camera_at(PATH[0].offset, PATH[0].yaw, PATH[0].pitch);
    c.bench_function("frame_profile/world_encode_ground_forward", |b| {
        b.iter(|| {
            let frame = target.acquire().expect("headless acquire");
            let stats = state.render(device, queue, frame.view(), black_box(&camera), None, &[]);
            state.gpu_timing_end_frame(device, queue);
            let _ = state.take_world_subphase_report();
            black_box(stats)
        });
    });
}

/// Does per-frame CPU cost scale with how much terrain is **resident**, or
/// only with how much is **drawn**?
///
/// This is the question behind "45 fps where it feels like it should be 200",
/// and the two answers point at completely different fixes. If the cost
/// tracks drawn sections, culling harder or batching draws helps. If it
/// tracks resident sections — sections the camera cannot even see — then the
/// per-frame work is proportional to the world you are holding rather than
/// the world you are looking at, and no amount of culling will touch it.
///
/// The sweep holds the camera fixed and grows the world, so `sections_drawn`
/// moves far less than the resident count does. Radii 2/4/6 rather than a
/// pair, because two points cannot distinguish a slope from an offset.
///
/// Nothing here asserts a millisecond figure. The assertion is the control
/// that the sweep did anything at all: residency must actually grow with
/// radius, or the whole comparison is between three copies of one scene.
fn submit_cost_versus_residency(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &mut HeadlessTarget,
) {
    println!("-- CPU cost against residency (camera fixed, world grown)");
    let mut rows: Vec<(i32, usize, usize, f64, f64)> = Vec::new();
    for radius in [2i32, 4, 6] {
        let (state, meshed) = build_demo_world(device, queue, radius);
        let camera = camera_at(PATH[0].offset, PATH[0].yaw, PATH[0].pitch);
        for _ in 0..WARMUP {
            let frame = target.acquire().expect("headless acquire");
            let _ = state.render(device, queue, frame.view(), &camera, None, &[]);
            let _ = state.take_world_subphase_report();
        }
        let mut encode = Samples::default();
        let mut submit = Samples::default();
        let mut drawn = 0usize;
        for _ in 0..ITERS {
            let frame = target.acquire().expect("headless acquire");
            let t0 = Instant::now();
            let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
            encode.push(t0.elapsed().as_secs_f64() * 1e3);
            drawn = stats.sections_drawn;
            let (subs, _) = state.take_world_subphase_report();
            for (name, ms) in subs {
                if name == "world.submit"
                    && let Some(ms) = ms
                {
                    submit.push(f64::from(ms));
                }
            }
        }
        rows.push((radius, meshed, drawn, encode.median(), submit.median()));
    }

    assert!(
        rows.windows(2).all(|w| w[1].1 > w[0].1),
        "the residency sweep did not actually grow the world: meshed sections were {:?} across \
         radii {:?}, so any cost comparison between these three rows is a comparison between \
         three copies of the same scene.",
        rows.iter().map(|r| r.1).collect::<Vec<_>>(),
        rows.iter().map(|r| r.0).collect::<Vec<_>>(),
    );

    for &(radius, meshed, drawn, encode_ms, submit_ms) in &rows {
        // The two normalisations side by side are the discriminator: whichever
        // one stays **flat** across the sweep is the quantity the cost is
        // actually proportional to. Reading the raw ratios instead cannot
        // separate them, because the demo world grows resident and drawn
        // together.
        println!(
            "   radius={radius}  resident {meshed:>5}, drawn {drawn:>5}  |  encode \
             {encode_ms:>7.3} ms (submit {submit_ms:>7.3} ms, {:>3.0}%)  |  per drawn \
             {:>6.2} us, per resident {:>6.2} us",
            100.0 * submit_ms / encode_ms.max(1e-9),
            1000.0 * encode_ms / drawn.max(1) as f64,
            1000.0 * encode_ms / meshed.max(1) as f64,
        );
        let scene = format!("demo radius={radius} {WIDTH}x{HEIGHT} residency-sweep");
        for (metric, value, unit) in [
            ("cpu_world_encode_median_ms", encode_ms, "ms"),
            ("cpu_submit_median_ms", submit_ms, "ms"),
            ("resident_sections", meshed as f64, "sections"),
            ("sections_drawn", drawn as f64, "sections"),
        ] {
            support::record(support::Record {
                bench: "frame_profile",
                metric,
                scene: &scene,
                value,
                unit,
            });
        }
    }

    // The two ratios, side by side, because that comparison *is* the finding
    // — and printed rather than asserted, since both are wall-clock medians.
    // Cost growing like residency while drawn barely moves means the per-frame
    // work is proportional to the world being held, not the world being
    // looked at.
    let (first, last) = (rows[0], rows[rows.len() - 1]);
    let per_drawn = |r: (i32, usize, usize, f64, f64)| r.3 / r.2.max(1) as f64;
    let per_resident = |r: (i32, usize, usize, f64, f64)| r.3 / r.1.max(1) as f64;
    println!(
        "   scaling: resident x{:.2}, drawn x{:.2}  ->  encode x{:.2}, submit x{:.2}\n   \
         per-section drift across the sweep: per drawn x{:.2}, per resident x{:.2} \
         (whichever is nearer 1.00 is what the cost tracks)\n",
        last.1 as f64 / first.1 as f64,
        last.2 as f64 / first.2.max(1) as f64,
        last.3 / first.3.max(1e-9),
        last.4 / first.4.max(1e-9),
        per_drawn(last) / per_drawn(first).max(1e-9),
        per_resident(last) / per_resident(first).max(1e-9),
    );
}

/// The counter-validation control `CLAUDE.md` prescribes: feed the instrument
/// an input that **cannot physically affect** the quantity it claims to
/// report, and require the quantity not to move.
///
/// A pure camera rotation from one eye position changes what is *drawn* and
/// cannot change what is *resident*. `vram_bytes` was once accumulated inside
/// the terrain draw loops after the cull — a drawn quantity wearing a
/// residency label — and moved 26% under exactly this input, which is how the
/// conclusion drawn from it came out backwards twice.
///
/// Both halves are load-bearing. Requiring `sections_drawn` to differ is what
/// stops this being vacuous: if the rotation somehow drew the same set, a
/// byte-identical residency figure would prove nothing at all.
fn rotation_does_not_move_residency(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &mut HeadlessTarget,
    state: &RenderState,
) {
    let eye = glam::Vec3::new(0.0, 6.0, -18.0);
    let mut render_at = |yaw: f32| {
        let camera = camera_at(eye, yaw, 0.0);
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        let _ = state.take_world_subphase_report();
        stats
    };
    let facing = render_at(0.0);
    let away = render_at(180.0);

    assert_ne!(
        facing.sections_drawn, away.sections_drawn,
        "turning the camera 180° on the spot drew the same {} sections both ways, so this \
         control established nothing about the residency counter below it. Move the eye or \
         widen the world until the two views genuinely differ.",
        facing.sections_drawn
    );
    assert_eq!(
        facing.vram_bytes, away.vram_bytes,
        "resident mesh bytes moved from {} to {} under a pure camera rotation from one eye \
         position. Rotation cannot change residency, so this counter is reporting a per-frame \
         DRAWN quantity under a residency label — the exact defect RenderState::resident_mesh_bytes \
         was introduced to fix.",
        facing.vram_bytes, away.vram_bytes
    );
    assert_eq!(
        facing.vram_reserved_bytes, away.vram_reserved_bytes,
        "reserved mesh bytes moved under a pure camera rotation; see the message above — the \
         reserved figure has the same contract as the resident one."
    );
    println!(
        "residency control: {} bytes resident, byte-identical across a 180° rotation that moved \
         sections_drawn {} -> {} (so the rotation demonstrably did something)\n",
        facing.vram_bytes, facing.sections_drawn, away.sections_drawn
    );
}

criterion_group!(benches, bench_frame_profile);
criterion_main!(benches);
