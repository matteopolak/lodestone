//! Dimension-separated world-generation throughput measurement.
//!
//! This executable deliberately stops at the generated column. It does not
//! load or save a region, propagate light, or encode a packet, so its numbers
//! are a direct measure of the world-generation library rather than an
//! end-to-end chunk-send number.
//!
//! Run in release mode with a deterministic 16x16 (256 chunk) grid:
//!
//! ```text
//! cargo run --release -p lodestone-worldgen --example throughput -- 42 16 all
//! ```
//!
//! Use `32` for a 1024-chunk grid. The optional third argument selects
//! `overworld`, `nether`, `end`, or `all`. An optional fourth argument selects
//! `shaped`, `decorated`, or `all`; every selected mode reports a cold pass
//! (fresh generator) and a warm pass (the same grid repeated on that generator).

#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use lodestone_server::{end_generator, nether_generator, overworld_generator};
use lodestone_worldgen::aquifer::BlockKind;
use lodestone_worldgen::end::EndGenerator;
use lodestone_worldgen::nether::NetherGenerator;
use lodestone_worldgen::overworld::OverworldGenerator;

struct CountingAllocator;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the system allocator receives the original layout unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: this pointer and layout originated from `Self::alloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy)]
enum DimensionName {
    Overworld,
    Nether,
    End,
}

impl DimensionName {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "overworld" => Some(Self::Overworld),
            "nether" => Some(Self::Nether),
            "end" => Some(Self::End),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Overworld => "overworld",
            Self::Nether => "nether",
            Self::End => "end",
        }
    }
}

enum Generator {
    Overworld(OverworldGenerator),
    Nether(NetherGenerator),
    End(EndGenerator),
}

impl Generator {
    fn new(dimension: DimensionName, seed: i64) -> Self {
        match dimension {
            DimensionName::Overworld => Self::Overworld(overworld_generator(seed)),
            DimensionName::Nether => Self::Nether(nether_generator(seed)),
            DimensionName::End => Self::End(end_generator(seed)),
        }
    }

    fn generate(&self, mode: Mode, cx: i32, cz: i32) -> u64 {
        match (self, mode) {
            (Self::Overworld(generator), Mode::Shaped) => {
                let column = generator.column_shaped(cx, cz);
                column.non_air_count() as u64
            }
            (Self::Overworld(generator), Mode::Decorated) => {
                let column = generator.column(cx, cz);
                column.non_air_count() as u64
            }
            (Self::Nether(generator), Mode::Shaped) => {
                let column = generator.column_shaped(cx, cz);
                column.non_air_count() as u64
            }
            (Self::Nether(generator), Mode::Decorated) => {
                let column = generator.column(cx, cz);
                column.non_air_count() as u64
            }
            // The End exposes its pre-surface terrain as a block-kind field,
            // while its public column method includes surface, structures and
            // decoration. Counting solid cells keeps both paths comparable.
            (Self::End(generator), Mode::Shaped) => generator
                .shape_field(cx, cz)
                .iter()
                .filter(|&&state| !matches!(state, BlockKind::Air))
                .count() as u64,
            (Self::End(generator), Mode::Decorated) => {
                let column = generator.column(cx, cz);
                column.non_air_count() as u64
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Shaped,
    Decorated,
}

impl Mode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "shaped" => Some(Self::Shaped),
            "decorated" => Some(Self::Decorated),
            _ => None,
        }
    }
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Shaped => "shaped",
            Self::Decorated => "decorated",
        }
    }
}

struct Pass {
    seconds: f64,
    chunks: usize,
    digest: u64,
    peak_rss_growth: Option<u64>,
    cpu_utilization: Option<f64>,
}

fn run_pass(generator: &Generator, mode: Mode, coords: &[(i32, i32)]) -> Pass {
    let baseline_rss = rss_bytes();
    let cpu_before = cpu_seconds();
    let started = Instant::now();
    let mut digest = 0xcbf2_9ce4_8422_2325u64;
    let mut peak_rss = baseline_rss;
    for &(cx, cz) in coords {
        let non_air = generator.generate(mode, cx, cz);
        digest ^= non_air;
        digest = digest.wrapping_mul(0x0100_0000_01b3);
        if let Some(rss) = rss_bytes() {
            peak_rss = Some(peak_rss.unwrap_or(rss).max(rss));
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    let cpu_utilization = cpu_before
        .zip(cpu_seconds())
        .map(|(before, after)| (after - before) / seconds.max(f64::MIN_POSITIVE) * 100.0);
    black_box(digest);
    Pass {
        seconds,
        chunks: coords.len(),
        digest,
        peak_rss_growth: baseline_rss.zip(peak_rss).map(|(base, peak)| peak.saturating_sub(base)),
        cpu_utilization,
    }
}

fn count_allocations(generator: &Generator, mode: Mode, sample: &[(i32, i32)]) -> u64 {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNT_ALLOCATIONS.store(true, Ordering::Relaxed);
    let digest = sample
        .iter()
        .map(|&(cx, cz)| generator.generate(mode, cx, cz))
        .fold(0u64, |digest, value| digest.rotate_left(7) ^ value);
    COUNT_ALLOCATIONS.store(false, Ordering::Relaxed);
    black_box(digest);
    ALLOCATIONS.load(Ordering::Relaxed)
}

fn allocation_sample(
    dimension: DimensionName,
    seed: i64,
    warm_generator: &Generator,
    mode: Mode,
    coords: &[(i32, i32)],
) -> (u64, u64) {
    let sample_len = coords.len().min(16);
    let sample = &coords[..sample_len];
    // The cold count uses a fresh generator so the timing pass above cannot
    // accidentally turn it into a cache-hit measurement. The warm count is
    // taken from the generator after its full-grid cold and warm timing passes.
    let cold_generator = Generator::new(dimension, seed);
    let cold = count_allocations(&cold_generator, mode, sample);
    let warm = count_allocations(warm_generator, mode, sample);
    (cold, warm)
}

fn rss_bytes() -> Option<u64> {
    memory_stats::memory_stats().map(|stats| stats.physical_mem as u64)
}

#[cfg(unix)]
fn cpu_seconds() -> Option<f64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` writes the complete `rusage` value for RUSAGE_SELF.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    // SAFETY: the successful call initialized `usage`.
    let usage = unsafe { usage.assume_init() };
    Some(timeval_seconds(usage.ru_utime) + timeval_seconds(usage.ru_stime))
}

#[cfg(unix)]
fn timeval_seconds(value: libc::timeval) -> f64 {
    value.tv_sec as f64 + value.tv_usec as f64 / 1_000_000.0
}

#[cfg(not(unix))]
fn cpu_seconds() -> Option<f64> {
    None
}

fn print_result(dimension: DimensionName, mode: Mode, cold: &Pass, warm: &Pass, allocs: (u64, u64)) {
    let cold_allocs = allocs.0 as f64 / cold.chunks.min(16) as f64;
    let warm_allocs = allocs.1 as f64 / warm.chunks.min(16) as f64;
    println!(
        "{:<10} {:<9} cold {:>8.2} chunks/s warm {:>8.2} chunks/s cpu cold={} warm={} rss-growth cold={} warm={} allocs/chunk sample16 cold={cold_allocs:.1} warm={warm_allocs:.1} digest={:#x}",
        dimension.label(),
        mode.label(),
        cold.chunks as f64 / cold.seconds,
        warm.chunks as f64 / warm.seconds,
        format_percent(cold.cpu_utilization),
        format_percent(warm.cpu_utilization),
        format_bytes(cold.peak_rss_growth),
        format_bytes(warm.peak_rss_growth),
        warm.digest,
    );
}

fn format_percent(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_string(), |value| format!("{value:.1}%"))
}

fn format_bytes(value: Option<u64>) -> String {
    value.map_or_else(|| "n/a".to_string(), |value| format!("{:.1}MiB", value as f64 / (1 << 20) as f64))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let seed = args.next().and_then(|value| value.parse().ok()).unwrap_or(42);
    let side = args.next().and_then(|value| value.parse().ok()).unwrap_or(16);
    assert!(side > 0, "grid side must be positive");
    let dimension_arg = args.next().unwrap_or_else(|| "all".to_string());
    let mode_arg = args.next().unwrap_or_else(|| "all".to_string());
    assert!(args.next().is_none(), "usage: throughput [seed] [grid-side] [all|overworld|nether|end] [all|shaped|decorated]");
    let dimensions: Vec<DimensionName> = if dimension_arg == "all" {
        vec![DimensionName::Overworld, DimensionName::Nether, DimensionName::End]
    } else {
        vec![DimensionName::parse(&dimension_arg).expect("dimension must be all, overworld, nether, or end")]
    };
    let modes: Vec<Mode> = if mode_arg == "all" {
        vec![Mode::Shaped, Mode::Decorated]
    } else {
        vec![Mode::parse(&mode_arg).expect("mode must be all, shaped, or decorated")]
    };
    let coords: Vec<(i32, i32)> = (0..side)
        .flat_map(|cz| (0..side).map(move |cx| (cx, cz)))
        .collect();
    println!(
        "worldgen throughput seed={seed} grid={side}x{side} chunks={} (generation only; no persistence/light/packet encoding)",
        coords.len()
    );
    println!("dimension  mode      cold chunks/s warm chunks/s cpu cold warm rss-growth cold warm allocations");
    for dimension in dimensions {
        for mode in &modes {
            let mode = *mode;
            let generator = Generator::new(dimension, seed);
            let cold = run_pass(&generator, mode, &coords);
            let warm = run_pass(&generator, mode, &coords);
            let allocs = allocation_sample(dimension, seed, &generator, mode, &coords);
            print_result(dimension, mode, &cold, &warm, allocs);
        }
    }
}
