//! Opt-in timings for the first container frame that reaches the GPU.
//!
//! The normal frame profiler intentionally reports rolling frame statistics.
//! That is useful for steady-state regressions, but it hides a one-time open
//! cost. This small profile records the real container stages once, only when
//! the `container_profile` tracing target is enabled, and includes the
//! workload that produced the timings so a location can be compared with a
//! controlled frame.

use std::time::Duration;

use crate::platform::Instant;

pub(super) const TARGET: &str = "container_profile";

#[derive(Debug, Clone, Copy)]
pub(super) enum Stage {
    GeometryBuild,
    PlayerPreview,
    IconUpload,
    SlotSubmit,
    BetweenStrata,
    CarriedSubmit,
}

impl Stage {
    const fn index(self) -> usize {
        match self {
            Self::GeometryBuild => 0,
            Self::PlayerPreview => 1,
            Self::IconUpload => 2,
            Self::SlotSubmit => 3,
            Self::BetweenStrata => 4,
            Self::CarriedSubmit => 5,
        }
    }
}

const STAGE_COUNT: usize = 6;

/// Counts and attachment state for the exact geometry passed to the renderer.
/// These fields make a timing line useful as a location-level measurement:
/// comparing a 46-slot inventory with a one-slot headless fixture does not
/// mistake different work for a timing regression.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Workload {
    pub slots: usize,
    pub colour_vertices: usize,
    pub sprite_vertices: usize,
    pub model_vertices: usize,
    pub special_icons: usize,
    pub background_vertices: usize,
    pub player_preview: bool,
    pub item_atlas: bool,
    pub item_models: bool,
    pub special_pass_before: bool,
    pub special_pass_after: bool,
    pub font_ink_runs_after: usize,
}

/// The one-shot state attached to a [`super::ContainerRenderer`].
#[derive(Debug, Default)]
pub(super) struct ContainerProfile {
    /// A profile is deliberately one-shot per renderer. Repeating it every
    /// frame would turn an opt-in diagnostic into a source of its own noise.
    reported: bool,
    active: Option<Run>,
}

#[derive(Debug)]
struct Run {
    source: &'static str,
    started: Instant,
    last: Instant,
    stages: [Option<Duration>; STAGE_COUNT],
    slots: usize,
    font_ink_runs_before: usize,
}

impl ContainerProfile {
    /// Start a sample if this process asked for the diagnostic. `source` is
    /// `menu` for the production container path and `prebuilt` for creative or
    /// other callers that hand the renderer geometry directly.
    pub(super) fn begin(&mut self, source: &'static str) {
        if self.reported
            || self.active.is_some()
            || !tracing::enabled!(target: TARGET, tracing::Level::DEBUG)
        {
            return;
        }
        let now = Instant::now();
        self.active = Some(Run {
            source,
            started: now,
            last: now,
            stages: [None; STAGE_COUNT],
            slots: 0,
            font_ink_runs_before: 0,
        });
    }

    /// Record the menu's actual slot count while the menu reference is still
    /// available to the outer geometry-building call.
    pub(super) fn set_slots(&mut self, slots: usize) {
        if let Some(run) = self.active.as_mut() {
            run.slots = slots;
        }
    }

    /// Record the font cache size after pack refresh and before geometry build,
    /// so a first-open glyph rasterisation shows up as a cache delta.
    pub(super) fn set_font_ink_runs_before(&mut self, count: usize) {
        if let Some(run) = self.active.as_mut() {
            run.font_ink_runs_before = count;
        }
    }

    /// Mark the end of a stage. The timer is absent, rather than guessed, when
    /// a caller did not execute that stage (for example geometry was supplied
    /// by a creative-screen caller).
    pub(super) fn mark(&mut self, stage: Stage) {
        let Some(run) = self.active.as_mut() else {
            return;
        };
        let now = Instant::now();
        run.stages[stage.index()] = Some(now.saturating_duration_since(run.last));
        run.last = now;
    }

    /// Discard a sample that never reached a drawable container. The menu seam
    /// can be called while no menu is active; that must not consume the one-shot
    /// profile and hide the first real open.
    pub(super) fn cancel(&mut self) {
        self.active = None;
    }

    /// Emit the measured stages and workload, then latch the sample. The latch
    /// is set only after a real render reaches this point, so a disabled target
    /// or an empty frame does not make the positive control vacuous.
    pub(super) fn finish(&mut self, workload: Workload) {
        let Some(run) = self.active.take() else {
            return;
        };
        let total = Instant::now().saturating_duration_since(run.started);
        self.reported = true;

        tracing::debug!(
            target: TARGET,
            source = run.source,
            total_ms = duration_ms(Some(total)),
            geometry_build_ms = duration_ms(run.stages[Stage::GeometryBuild.index()]),
            player_preview_ms = duration_ms(run.stages[Stage::PlayerPreview.index()]),
            icon_upload_ms = duration_ms(run.stages[Stage::IconUpload.index()]),
            slot_submit_ms = duration_ms(run.stages[Stage::SlotSubmit.index()]),
            between_strata_ms = duration_ms(run.stages[Stage::BetweenStrata.index()]),
            carried_submit_ms = duration_ms(run.stages[Stage::CarriedSubmit.index()]),
            slots = run.slots.max(workload.slots),
            colour_vertices = workload.colour_vertices,
            sprite_vertices = workload.sprite_vertices,
            model_vertices = workload.model_vertices,
            special_icons = workload.special_icons,
            background_vertices = workload.background_vertices,
            player_preview = workload.player_preview,
            item_atlas = workload.item_atlas,
            item_models = workload.item_models,
            special_pass_before = workload.special_pass_before,
            special_pass_after = workload.special_pass_after,
            font_ink_runs_before = run.font_ink_runs_before,
            font_ink_runs_after = workload.font_ink_runs_after,
            "first drawable container frame timings"
        );
    }
}

fn duration_ms(duration: Option<Duration>) -> f64 {
    duration.map_or(0.0, |duration| duration.as_secs_f64() * 1_000.0)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{ContainerProfile, Stage, Workload};

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("capture lock")).into_owned()
        }
    }

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn sample(profile: &mut ContainerProfile) {
        profile.begin("menu");
        profile.mark(Stage::GeometryBuild);
        profile.mark(Stage::PlayerPreview);
        profile.mark(Stage::IconUpload);
        profile.mark(Stage::SlotSubmit);
        profile.mark(Stage::BetweenStrata);
        profile.mark(Stage::CarriedSubmit);
        profile.finish(Workload {
            slots: 46,
            colour_vertices: 120,
            sprite_vertices: 24,
            model_vertices: 36,
            special_icons: 1,
            background_vertices: 48,
            player_preview: true,
            item_atlas: true,
            item_models: true,
            special_pass_before: false,
            special_pass_after: true,
            font_ink_runs_after: 1,
        });
    }

    fn captured(filter: &str, f: impl FnOnce(&mut ContainerProfile)) -> String {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .finish();
        let mut profile = ContainerProfile::default();
        tracing::subscriber::with_default(subscriber, || f(&mut profile));
        capture.text()
    }

    /// Positive control: the production target reaches a real subscriber and
    /// exposes all location-level fields, not merely a boolean "slow" marker.
    #[test]
    fn profile_emits_location_timings_when_enabled() {
        let output = captured("container_profile=debug", sample);
        assert!(
            output.contains("first drawable container frame timings"),
            "enabled target must emit a sample, got {output:?}"
        );
        for field in [
            "geometry_build_ms",
            "player_preview_ms",
            "icon_upload_ms",
            "slot_submit_ms",
            "between_strata_ms",
            "carried_submit_ms",
            "font_ink_runs_before",
            "font_ink_runs_after",
        ] {
            assert!(output.contains(field), "sample is missing {field}: {output:?}");
        }
    }

    /// Negative control: the identical stage sequence must remain silent when
    /// the diagnostic target is off. Without this, an unconditional logger
    /// would make the positive control meaningless.
    #[test]
    fn profile_is_silent_when_disabled() {
        let output = captured("container_profile=off", sample);
        assert!(output.is_empty(), "disabled target must stay silent: {output:?}");
    }

    /// A first-open profile is one sample, not a rolling stream. Calling the
    /// same production-shaped sequence twice proves the latch rather than only
    /// asserting that one call happened to log.
    #[test]
    fn profile_reports_only_once_per_renderer() {
        let output = captured("container_profile=debug", |profile| {
            sample(profile);
            sample(profile);
        });
        assert_eq!(
            output.matches("first drawable container frame timings").count(),
            1,
            "one renderer must emit one first-open sample: {output:?}"
        );
    }

    /// Cancelling an empty call must leave the one-shot latch available to the
    /// next real container frame.
    #[test]
    fn an_empty_call_does_not_consume_the_first_open_sample() {
        let output = captured("container_profile=debug", |profile| {
            profile.begin("menu");
            profile.cancel();
            sample(profile);
        });
        assert_eq!(
            output.matches("first drawable container frame timings").count(),
            1,
            "an empty frame must not hide the first drawable one: {output:?}"
        );
    }
}
