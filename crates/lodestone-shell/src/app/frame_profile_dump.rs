//! Raw per-frame CSV dump for [`super::frame_profile::FrameProfiler`], behind
//! the `LODESTONE_FRAME_PROFILE_DUMP` env var — see
//! `docs/frame-profiling.md` for the operator-facing doc.
//!
//! # Why a file at all
//!
//! The F3 overlay and the periodic `tracing` line only ever show a rolling
//! window's mean/p95/p99. That is enough to notice *that* something is slow
//! live, but a real stutter investigation wants the raw sequence — was it one
//! spike or a sustained regime change, did it correlate with a chunk load, is
//! it periodic — none of which survives being reduced to a percentile. This
//! writes one row per frame so a session can be pulled into a spreadsheet or
//! a quick script after the fact instead of being squinted at over a
//! shoulder while playing.
//!
//! # Never silent
//!
//! Setting the env var to a path that cannot be opened (a typo, a missing
//! parent directory, a read-only mount) must not silently produce "no
//! dump" — that is exactly the class of degradation this repo's rules ask to
//! be logged, never absent. [`DumpWriter::open`] always succeeds as a value
//! (there is no `Result` for a caller to forget to check); if the underlying
//! file could not be opened it logs a `tracing::warn!` once, at construction,
//! and every subsequent [`DumpWriter::write_row`] becomes a harmless no-op
//! rather than one warning per frame.
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use super::frame_profile::{FramePhase, PHASE_COUNT};
use crate::gpu::gpu_timing::{WORLD_SUBPHASE_COUNT, WorldSubphase};

#[derive(Debug)]
pub(crate) struct DumpWriter {
    /// `None` when the file could not be opened — see the module doc. Every
    /// method on this type degrades to a no-op rather than panicking or
    /// re-warning.
    file: Option<BufWriter<File>>,
}

impl DumpWriter {
    /// Open `path` for the dump, writing the CSV header immediately. Always
    /// returns a usable value; see the module doc for what happens on
    /// failure.
    pub(crate) fn open(path: &Path) -> Self {
        let file = match File::create(path) {
            Ok(f) => Some(BufWriter::new(f)),
            Err(e) => {
                tracing::warn!(
                    target: "frame_profile",
                    "{}={path:?} could not be opened ({e}); raw per-frame samples will not be recorded this session",
                    super::frame_profile::DUMP_ENV_VAR,
                );
                None
            }
        };
        let mut this = Self { file };
        this.write_header();
        this
    }

    fn write_header(&mut self) {
        let Some(w) = &mut self.file else { return };
        let mut header = String::from("frame");
        for phase in FramePhase::ALL {
            header.push(',');
            header.push_str(phase.name());
        }
        // `world_encode_submit`'s own internal breakdown — see
        // `gpu::gpu_timing::WorldSubphase`'s doc. Always present in the
        // header even on a session where the bridge never has data (e.g. no
        // frame ever reaches `render_inner`), matching every other column
        // here: the header names every phase this instrument *can* report,
        // not only the ones a given session happened to exercise.
        for subphase in WorldSubphase::ALL {
            header.push(',');
            header.push_str(subphase.name());
        }
        if let Err(e) = writeln!(w, "{header}") {
            tracing::warn!(target: "frame_profile", "failed writing dump header: {e}");
            self.file = None;
        }
    }

    /// Write one frame's row. `values[i]` is `None` for a phase that was
    /// skipped this frame (see `FrameProfiler`'s module doc) — written as an
    /// empty CSV field, never `0`, so a spreadsheet does not average a skip
    /// into a real cost. `world_subphases[i]` is likewise `None` whenever
    /// `world_encode_submit` itself did not run this frame, or the bridge
    /// had nothing recorded for that slot — see
    /// `FrameProfiler::drain_world_subphases`'s doc.
    pub(crate) fn write_row(
        &mut self,
        frame: u64,
        values: [Option<f32>; PHASE_COUNT],
        world_subphases: [Option<f32>; WORLD_SUBPHASE_COUNT],
    ) {
        let Some(w) = &mut self.file else { return };
        let mut line = frame.to_string();
        for v in values.into_iter().chain(world_subphases) {
            line.push(',');
            if let Some(ms) = v {
                line.push_str(&format!("{ms:.4}"));
            }
        }
        if let Err(e) = writeln!(w, "{line}") {
            tracing::warn!(target: "frame_profile", "failed writing dump row {frame}: {e}");
            self.file = None;
            return;
        }
        // A crash mid-session should not cost the whole recording — flush
        // periodically rather than only at (an unreachable, for a GUI app)
        // clean shutdown. Every 64 rows: often enough that a kill -9 loses at
        // most a fraction of a second of frames, rare enough that this is not
        // itself a per-frame syscall.
        if frame % 64 == 0 {
            let _ = w.flush();
        }
    }
}
