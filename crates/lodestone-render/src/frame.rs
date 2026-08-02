//! Frame-loop scaffold: pass sequencing, frame pacing, and explicit
//! surface-lost / outdated recovery.
//!
//! The renderer here draws one trivial, verifiable thing — a solid-colour test
//! triangle over a known clear colour — so the entire acquire → record → submit
//! → present path is provably exercised end to end, including on a headless
//! target that can be read back pixel-for-pixel.

use std::time::Duration;

use crate::target::{RenderTarget, TargetError};

/// Native-only clock, confined to its own wholly-gated file so `Instant` cannot
/// leak onto the wasm path. Re-exported below on non-wasm targets.
#[cfg(not(target_arch = "wasm32"))]
#[path = "frame_native.rs"]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::SystemClock;

/// A monotonic time source for frame pacing.
///
/// The clock is injected rather than hardcoded because [`std::time::Instant::now`]
/// *compiles* on `wasm32-unknown-unknown` but **panics at runtime** there. A
/// browser build supplies its own source backed by `performance.now()`; native
/// builds use [`SystemClock`]. Keeping the seam here stops `Instant::now()` from
/// silently leaking into new tick code as the renderer grows — and the
/// `no_wasm_trap_symbols_are_confined` test *enforces* that rather than trusting
/// it.
pub trait TimeSource {
    /// Monotonic time elapsed since this source's fixed origin. Only the
    /// *difference* between successive calls is meaningful; the origin itself is
    /// arbitrary and never assumed to be the Unix epoch or process start.
    fn now(&self) -> Duration;
}

/// A fixed-timestep frame pacer. Purely arithmetic (no sleeping) and free of any
/// real clock — time is injected via [`FramePacer::tick_at`] or a [`TimeSource`]
/// via [`FramePacer::tick`], so it is fully testable and wasm-safe.
#[derive(Debug, Clone)]
pub struct FramePacer {
    target_frame: Duration,
    last: Option<Duration>,
    frame_index: u64,
}

/// The timing result of one [`FramePacer::tick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTiming {
    /// Monotonic frame counter.
    pub frame_index: u64,
    /// Time since the previous tick (zero on the first).
    pub delta: Duration,
    /// How long to sleep to hit the target rate (zero if already behind).
    pub sleep_hint: Duration,
}

impl FramePacer {
    /// Create a pacer targeting `fps` frames per second.
    #[must_use]
    pub fn new(fps: u32) -> Self {
        let fps = fps.max(1);
        Self {
            target_frame: Duration::from_secs_f64(1.0 / f64::from(fps)),
            last: None,
            frame_index: 0,
        }
    }

    /// The target per-frame duration.
    #[must_use]
    pub fn target_frame(&self) -> Duration {
        self.target_frame
    }

    /// Advance to the next frame, using `now` — a monotonic timestamp from a
    /// [`TimeSource`] — as the current time. Injected so this is deterministically
    /// testable and contains no reference to any real (and on wasm, trapping)
    /// clock.
    pub fn tick_at(&mut self, now: Duration) -> FrameTiming {
        let delta = self.last.map_or(Duration::ZERO, |l| now.saturating_sub(l));
        self.last = Some(now);
        let idx = self.frame_index;
        self.frame_index += 1;
        let sleep_hint = self.target_frame.saturating_sub(delta);
        FrameTiming {
            frame_index: idx,
            delta,
            sleep_hint,
        }
    }

    /// Advance using an injected [`TimeSource`] — [`SystemClock`] natively, or a
    /// `performance.now()`-backed source in the browser. This replaces any direct
    /// call to `Instant::now()`, which panics at runtime on `wasm32`.
    pub fn tick(&mut self, clock: &impl TimeSource) -> FrameTiming {
        self.tick_at(clock.now())
    }
}

/// What happened when a frame was pumped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    /// A frame was recorded and presented.
    Presented {
        /// Whether the surface reported the frame as suboptimal.
        suboptimal: bool,
    },
    /// The frame was skipped because the target needed reconfiguring; the
    /// target was reconfigured and the next frame should succeed.
    Reconfigured,
    /// The frame was skipped for a transient reason (timeout / occluded).
    Skipped(TargetError),
}

/// Draws the trivial test triangle and owns the frame-pump recovery logic.
#[derive(Debug)]
pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    format: wgpu::TextureFormat,
    clear: wgpu::Color,
}

impl Renderer {
    /// RGBA colour the fragment shader emits for the test triangle.
    pub const TRIANGLE_RGBA: [u8; 4] = [255, 128, 0, 255];

    /// Build a renderer whose pipeline outputs to `format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-render test triangle"),
            source: wgpu::ShaderSource::Wgsl(TRIANGLE_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-render pipeline layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-render test pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            format,
            clear: wgpu::Color {
                r: 0.02,
                g: 0.02,
                b: 0.05,
                a: 1.0,
            },
        }
    }

    /// The colour format this renderer targets.
    #[must_use]
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Pump one frame to `target`, handling acquisition failure explicitly:
    /// outdated/lost targets are reconfigured (frame skipped, next succeeds),
    /// transient failures are reported as skipped. On success the triangle is
    /// drawn and presented.
    pub fn render_frame<T: RenderTarget>(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &mut T,
    ) -> FrameOutcome {
        let frame = match target.acquire() {
            Ok(f) => f,
            Err(e) if e.needs_reconfigure() => {
                target.reconfigure(device);
                return FrameOutcome::Reconfigured;
            }
            Err(e) => return FrameOutcome::Skipped(e),
        };
        let suboptimal = frame.suboptimal;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("lodestone-render frame encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lodestone-render color pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: frame.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.draw(0..3, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
        frame.present(queue);
        FrameOutcome::Presented { suboptimal }
    }
}

/// WGSL for the test triangle: a large centred triangle in a known colour.
const TRIANGLE_WGSL: &str = include_str!("shaders/triangle.wgsl");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacer_first_tick_has_zero_delta() {
        let mut p = FramePacer::new(60);
        let t0 = Duration::ZERO;
        let a = p.tick_at(t0);
        assert_eq!(a.frame_index, 0);
        assert_eq!(a.delta, Duration::ZERO);
        // A full frame of sleep is suggested when no time has passed.
        assert_eq!(a.sleep_hint, p.target_frame());
    }

    #[test]
    fn pacer_counts_and_measures_delta() {
        let mut p = FramePacer::new(100); // 10ms/frame
        let t0 = Duration::ZERO;
        let _ = p.tick_at(t0);
        let b = p.tick_at(t0 + Duration::from_millis(4));
        assert_eq!(b.frame_index, 1);
        assert_eq!(b.delta, Duration::from_millis(4));
        assert_eq!(b.sleep_hint, Duration::from_millis(6));
    }

    #[test]
    fn pacer_no_sleep_when_behind() {
        let mut p = FramePacer::new(100);
        let t0 = Duration::ZERO;
        let _ = p.tick_at(t0);
        let b = p.tick_at(t0 + Duration::from_millis(25));
        assert_eq!(b.sleep_hint, Duration::ZERO, "already behind schedule");
    }

    #[derive(Debug)]
    struct FakeClock(std::cell::Cell<Duration>);

    impl TimeSource for FakeClock {
        fn now(&self) -> Duration {
            self.0.get()
        }
    }

    #[test]
    fn pacer_tick_uses_injected_time_source() {
        let mut p = FramePacer::new(100); // 10ms/frame
        let clock = FakeClock(std::cell::Cell::new(Duration::ZERO));
        let a = p.tick(&clock);
        assert_eq!(a.frame_index, 0);
        assert_eq!(a.delta, Duration::ZERO);

        clock.0.set(Duration::from_millis(4));
        let b = p.tick(&clock);
        assert_eq!(b.frame_index, 1);
        assert_eq!(b.delta, Duration::from_millis(4));
        assert_eq!(b.sleep_hint, Duration::from_millis(6));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn system_clock_is_monotonic() {
        let c = SystemClock::new();
        let a = c.now();
        let b = c.now();
        assert!(b >= a);
    }

    /// CI guard for the wasm runtime-trap symbol family (`Instant::now`,
    /// `std::fs`, `std::thread::spawn`, `tokio::time`): these compile green for
    /// `wasm32-unknown-unknown` and only panic at runtime, so `wasm-check.sh`
    /// (compile-only) is structurally blind to them. This test makes the
    /// constraint *checkable* instead of trusting discipline: every occurrence
    /// must live in an explicitly allow-listed, wholly-gated file, and a fresh
    /// ungated use anywhere else fails here naming the file and line.
    ///
    /// Patterns are assembled by concatenation so this guard's own source never
    /// contains the contiguous banned substring (which would flag itself).
    #[test]
    fn no_wasm_trap_symbols_are_confined() {
        use std::fs;
        use std::path::PathBuf;

        let instant_now = format!("Instant{}", "::now");
        let fs_call = format!("std::{}::", "fs");
        let thread_spawn = format!("std::thread::{}", "spawn");
        let tokio_time = format!("tokio::{}", "time");
        // (banned pattern, files where it is permitted)
        let rules: [(&str, &[&str]); 4] = [
            (instant_now.as_str(), &["frame_native.rs"]),
            (fs_call.as_str(), &["blocks_json_native.rs"]),
            (thread_spawn.as_str(), &[]),
            (tokio_time.as_str(), &[]),
        ];

        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src];
        let mut files = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).expect("read_dir src") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        assert!(!files.is_empty(), "guard found no source files to scan");

        let mut violations = Vec::new();
        for path in &files {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = fs::read_to_string(path).expect("read source");
            for (lineno, raw) in text.lines().enumerate() {
                // Drop line/doc comments so documentation may name the symbols.
                let code = raw.split("//").next().unwrap_or("");
                for (pat, allowed) in &rules {
                    if code.contains(pat) && !allowed.contains(&name.as_str()) {
                        violations.push(format!(
                            "{}:{}:{}",
                            path.display(),
                            lineno + 1,
                            raw.trim()
                        ));
                    }
                }
            }
        }
        assert!(
            violations.is_empty(),
            "wasm runtime-trap symbols found outside their confined module \
             (these compile green but panic in a browser):\n{}",
            violations.join("\n")
        );
    }
}
