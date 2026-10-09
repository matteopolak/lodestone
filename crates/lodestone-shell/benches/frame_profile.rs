//! CPU recording costs and real GPU spans over a fixed world and camera path.
//!
//! GPU samples cover the world multipass span and the optional first-person
//! pass. Each completed frame contributes at most once; unavailable or invalid
//! timings are counted separately and never recorded as zero. These spans do
//! not cover a whole frame or establish whether the renderer is CPU/GPU bound.
//!
//! Timings are recorded against a same-machine, same-scene baseline with a
//! noise estimate and concurrent-build witness. Count controls check that
//! residency stays byte-identical under a rotation that changes drawn sections,
//! and that the residency sweep actually grows the scene.
//!
//! The packed/demo world does not exercise the live block-model arena, HUD,
//! menus, or surface presentation. GPU timestamp calibration requires separate
//! live controls; this benchmark compares the reported spans across runs.
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
use lodestone::gpu::{GpuTimingStatus, RenderState, RenderStats, ScreenEffects};
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

/// Frames discarded to pay one-time driver/allocator costs. Producing frame
/// IDs exclude their late readbacks from the measured cohort.
const WARMUP: usize = 12;

/// CPU frames kept per waypoint; async GPU readbacks may yield fewer samples.
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
    state.set_gpu_timing_enabled(device, queue, true);
    let mut sections = 0usize;
    for cz in -radius..=radius {
        for cx in -radius..=radius {
            for si in 0..worldgen::SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: worldgen::MIN_Y };
                let Some(snap) = snapshot_section(&world, key, Default::default()) else { continue };
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

fn render_timed(
    state: &RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    view: &wgpu::TextureView,
    camera: &Camera,
) -> (RenderStats, f64, f64) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("profiled-world"),
    });
    let stats = state.encode_with_crack_and_effects(
        device, queue, view, camera, None, &[], &[], ScreenEffects::default(), &mut encoder,
    );
    let finish_started = Instant::now();
    let checkpoints = state.submit_encoded_frame(queue, encoder);
    let finish_ms = checkpoints.encoder_finished_at.duration_since(finish_started).as_secs_f64() * 1e3;
    let submit_ms = checkpoints.submitted_at.duration_since(checkpoints.encoder_finished_at).as_secs_f64() * 1e3;
    (stats, finish_ms, submit_ms)
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

    /// `max / median`, a description of this series' spread.
    fn noise(&self) -> f64 {
        let mut v = self.0.clone();
        v.sort_by(f64::total_cmp);
        match (v.last(), self.median()) {
            (Some(max), med) if med > 0.0 => max / med,
            _ => 1.0,
        }
    }
}

#[derive(Default)]
struct GpuSamples {
    measured: Samples,
    not_run: usize,
    invalid: usize,
    map_error: usize,
}

impl GpuSamples {
    fn push(&mut self, status: GpuTimingStatus, duration_ms: Option<f32>) {
        match status {
            GpuTimingStatus::Measured => {
                self.measured.push(f64::from(duration_ms.expect("measured GPU duration")));
            }
            GpuTimingStatus::NotRun => self.not_run += 1,
            GpuTimingStatus::Invalid => self.invalid += 1,
            GpuTimingStatus::MapError => self.map_error += 1,
        }
    }

    fn summary(&self) -> String {
        let timing = if self.measured.0.is_empty() {
            "no measured data".to_string()
        } else {
            format!(
                "{:.3} ms (noise max/median {:.2}x)",
                self.measured.median(), self.measured.noise(),
            )
        };
        format!(
            "{timing}; n={}, not_run={}, invalid={}, map_error={}",
            self.measured.0.len(), self.not_run, self.invalid, self.map_error,
        )
    }
}

/// Count concurrent compiler/test processes at both ends of the run. A failed
/// process inventory is unknown, rather than evidence of a quiet machine.
fn concurrent_build_processes() -> Option<usize> {
    let me = std::process::id().to_string();
    let out = std::process::Command::new("/bin/ps").args(["-Ao", "pid=,command="]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .filter(|line| {
                let mut parts = line.trim_start().splitn(2, char::is_whitespace);
                let (Some(pid), Some(cmd)) = (parts.next(), parts.next()) else {
                    return false;
                };
                // This bench's own process, and the cargo that spawned it, are
                // not contention — excluding them is what makes `0` mean quiet
                // rather than merely "nothing besides me and my parent".
                if pid == me || cmd.contains("frame_profile-") {
                    return false;
                }
                cmd.contains("/rustc")
                    || cmd.contains("bin/cargo")
                    || cmd.contains("target/debug/")
                    || cmd.contains("target/release/")
            })
            .count(),
    )
}

/// Render a start/end pair from [`concurrent_build_processes`] as one line.
fn busy_witness(before: Option<usize>, after: Option<usize>) -> String {
    match (before, after) {
        (Some(0), Some(0)) => {
            "quiet machine: no other cargo/rustc/test processes at either end of the run"
                .to_string()
        }
        (Some(b), Some(a)) => format!(
            "MACHINE WAS BUSY: {b} other cargo/rustc/test processes at the start, {a} at the \
             end. Every millisecond figure below is a sample taken under contention -- compare \
             it only against another run with the same witness, never against a quiet one."
        ),
        _ => "machine load unknown: `ps` could not be sampled, so nothing here establishes \
              whether the timings were taken under contention"
            .to_string(),
    }
}

fn bench_frame_profile(c: &mut Criterion) {
    let Ok(ctx) = GpuContext::new_headless_blocking() else {
        println!(
            "frame_profile: SKIPPED, no GPU adapter. NOT RUN: CPU/GPU span comparisons, \
             residency sweep, and rotation-invariance control. Re-run with an adapter."
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
             ran."
        );
    }

    // Sampled before the first waypoint and again after the last, and printed
    // at both ends: see `concurrent_build_processes` for why this is a process
    // count rather than a load average, and why a run under contention has to
    // announce it in its own output.
    let busy_before = concurrent_build_processes();
    println!(
        "\n=== frame profile: demo world radius={RADIUS}, {meshed} meshed sections, \
         {WIDTH}x{HEIGHT}, {ITERS} frames per waypoint after {WARMUP} warm-up ===\n\
         \x20  other cargo/rustc/test processes at the start of this run: {}\n",
        busy_before.map_or_else(|| "unknown (`ps` unreadable)".to_string(), |n| n.to_string()),
    );

    let mut last_consumed_gpu_sample = None;

    for wp in &PATH {
        let camera = camera_at(wp.offset, wp.yaw, wp.pitch);

        for _ in 0..WARMUP {
            let frame = target.acquire().expect("headless acquire");
            let _ = state.render(device, queue, frame.view(), &camera, None, &[]);
            state.gpu_timing_end_frame(device, queue);
            let _ = state.take_world_subphase_report();
        }

        let gpu_before = state.gpu_timing_snapshot();
        let warmup_cutoff = gpu_before.as_ref().map_or(0, |s| s.current_frame_id);
        let dropped_before = gpu_before.as_ref().map_or(0, |s| s.dropped_frames);
        let mut cpu_world = Samples::default();
        let mut cpu_timing_end = Samples::default();
        let mut sub_prepare = Samples::default();
        let mut sub_terrain = Samples::default();
        let mut sub_other = Samples::default();
        // Sum within each frame to preserve the CPU submit metric's meaning.
        let mut sub_submit = Samples::default();
        let mut sub_finish = Samples::default();
        let mut sub_queue = Samples::default();
        let mut gpu_world = GpuSamples::default();
        let mut gpu_hand = GpuSamples::default();
        let mut gpu_frame_ids = Vec::new();
        let mut last_stats = None;
        let mut visited = None;

        for _ in 0..ITERS {
            let frame = target.acquire().expect("headless acquire");
            let t0 = Instant::now();
            let (stats, finish_ms, queue_ms) = render_timed(&state, device, queue, frame.view(), &camera);
            cpu_world.push(t0.elapsed().as_secs_f64() * 1e3);
            sub_finish.push(finish_ms);
            sub_queue.push(queue_ms);
            sub_submit.push(finish_ms + queue_ms);

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
                    other => panic!(
                        "unknown world sub-phase {other:?} — this bench's match arms and \
                         gpu::gpu_timing::WorldSubphase have drifted apart"
                    ),
                }
            }
            if counts.is_some() {
                visited = counts;
            }

            if let Some(snapshot) = state.gpu_timing_snapshot()
                && let Some(sample) = snapshot.sample
                && Some((sample.timer_id, sample.frame_id)) != last_consumed_gpu_sample
                && sample.frame_id > warmup_cutoff
            {
                last_consumed_gpu_sample = Some((sample.timer_id, sample.frame_id));
                // Readback may finish after a camera change; classify by the
                // producing frame, rather than the iteration that observes it.
                gpu_frame_ids.push(sample.frame_id);
                for (name, sink) in [
                    ("world", &mut gpu_world),
                    ("first_person", &mut gpu_hand),
                ] {
                    let segment = sample.segments.iter()
                        .find(|s| s.name == name)
                        .unwrap_or_else(|| panic!("missing GPU segment {name:?}"));
                    sink.push(segment.status, segment.duration_ms);
                }
            }
            last_stats = Some(stats);
        }

        let stats = last_stats.expect("at least one frame rendered");

        let cpu_ms = cpu_world.median();
        let gpu_after = state.gpu_timing_snapshot();
        let dropped = gpu_after.as_ref().map_or(0, |s| s.dropped_frames - dropped_before);
        let sample_age = gpu_after.as_ref().and_then(|s| s.sample_age_frames);

        let (packed_visited, model_visited) = visited.unwrap_or((0, 0));
        println!(
            "-- {} (yaw {:.0}, pitch {:.0})\n\
             \x20  cpu  world encode {:>8.3} ms   (noise max/median {:.2}x)\n\
             \x20  cpu  gpu-readback {:>8.3} ms\n\
             \x20  cpu  .prepare_buf {:>8.3} ms\n\
             \x20  cpu  .cull+draw   {:>8.3} ms\n\
             \x20  cpu  .other_draws {:>8.3} ms\n\
             \x20  cpu  .submit      {:>8.3} ms   (finish {:.3}, queue.submit {:.3})\n\
             \x20  gpu  world span   {}\n\
             \x20  gpu  first_person {}\n\
             \x20  cnt  sections     {} drawn / {} visited packed + {} model\n\
             \x20  cnt  culled       {} distance, {} frustum, {} occlusion\n\
             \x20  cnt  draw_calls   {}, quads {}, entities {}\n\
             \x20  cnt  residency    {} bytes resident, {} reserved\n\
             \x20  cnt  readback     {} fresh frames, IDs {:?}..{:?}, latest age {:?}, \
             {} dropped measurements\n",
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
            sub_finish.median(),
            sub_queue.median(),
            gpu_world.summary(),
            gpu_hand.summary(),
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
            gpu_frame_ids.len(),
            gpu_frame_ids.first(),
            gpu_frame_ids.last(),
            sample_age,
            dropped,
        );

        let scene = format!("demo radius={RADIUS} {}x{HEIGHT} waypoint={}", WIDTH, wp.label);
        for (metric, value, unit) in [
            ("cpu_world_encode_median_ms", cpu_ms, "ms"),
            ("cpu_gpu_timing_end_median_ms", cpu_timing_end.median(), "ms"),
            ("cpu_prepare_buffers_median_ms", sub_prepare.median(), "ms"),
            ("cpu_terrain_cull_draw_median_ms", sub_terrain.median(), "ms"),
            ("cpu_other_draws_median_ms", sub_other.median(), "ms"),
            ("cpu_submit_median_ms", sub_submit.median(), "ms"),
            ("cpu_encoder_finish_median_ms", sub_finish.median(), "ms"),
            ("cpu_queue_submit_median_ms", sub_queue.median(), "ms"),
            ("sections_drawn", stats.sections_drawn as f64, "sections"),
            ("draw_calls", stats.draw_calls as f64, "calls"),
            ("total_quads", stats.total_quads as f64, "quads"),
            ("resident_mesh_bytes", stats.vram_bytes as f64, "bytes"),
            ("gpu_world_span_samples", gpu_world.measured.0.len() as f64, "samples"),
            ("gpu_first_person_samples", gpu_hand.measured.0.len() as f64, "samples"),
            ("gpu_fresh_frames", gpu_frame_ids.len() as f64, "frames"),
            ("gpu_dropped_measurements", dropped as f64, "frames"),
        ] {
            support::record(support::Record {
                bench: "frame_profile",
                metric,
                scene: &scene,
                value,
                unit,
            });
        }
        for (metric, samples) in [
            ("gpu_world_span_median_ms", &gpu_world.measured),
            ("gpu_first_person_median_ms", &gpu_hand.measured),
        ] {
            if !samples.0.is_empty() {
                support::record(support::Record {
                    bench: "frame_profile",
                    metric,
                    scene: &scene,
                    value: samples.median(),
                    unit: "ms",
                });
            }
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

    // The second half of the busy witness, printed last so it qualifies every
    // figure above it. Both readings are shown: a run that started quiet and
    // ended contended is exactly as untrustworthy as one that was busy
    // throughout, and only the pair can say which happened.
    println!("\n{}\n", busy_witness(busy_before, concurrent_build_processes()));
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
            let (stats, finish_ms, queue_ms) = render_timed(&state, device, queue, frame.view(), &camera);
            encode.push(t0.elapsed().as_secs_f64() * 1e3);
            drawn = stats.sections_drawn;
            let _ = state.take_world_subphase_report();
            submit.push(finish_ms + queue_ms);
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
