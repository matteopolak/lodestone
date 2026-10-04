//! Bounded opt-in records of successful surface presentation submissions.

use crate::platform::Instant;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

/// Enough for a ten-second stationary window at about 3,000 presents per second.
pub(super) const MAX_ROWS: usize = 32_768;
/// Nine nullable integers per row, each at most twenty decimal digits.
pub(super) const MAX_JSON_BYTES: usize = MAX_ROWS * (9 * 21 + 2) + 8192;
const MAX_PENDING_COMPLETIONS: usize = 3;

type Row = [Option<u64>; 9];
type Completion = (Option<usize>, u64, u64);

#[derive(Debug, Clone, Copy)]
pub(super) enum SubmissionKind {
    Menu = 1,
    World = 2,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum SkipReason {
    Unclassified,
    Paced,
    MissingMenuState,
    MissingWorldState,
    MenuAcquire(lodestone_render::TargetError),
    WorldAcquire(lodestone_render::TargetError),
}

const SKIP_NAMES: [&str; 14] = [
    "unclassified", "paced", "missing-menu-state", "missing-world-state",
    "menu-acquire-timeout", "menu-acquire-occluded", "menu-acquire-outdated",
    "menu-acquire-lost", "menu-acquire-validation", "world-acquire-timeout",
    "world-acquire-occluded", "world-acquire-outdated", "world-acquire-lost",
    "world-acquire-validation",
];

impl SkipReason {
    fn index(self) -> usize {
        use lodestone_render::TargetError;
        let (base, error) = match self {
            Self::Unclassified => return 0,
            Self::Paced => return 1,
            Self::MissingMenuState => return 2,
            Self::MissingWorldState => return 3,
            Self::MenuAcquire(error) => (4, error),
            Self::WorldAcquire(error) => (9, error),
        };
        base + match error {
            TargetError::Timeout => 0,
            TargetError::Occluded => 1,
            TargetError::Outdated => 2,
            TargetError::Lost => 3,
            TargetError::Validation => 4,
        }
    }
}

#[derive(Debug)]
struct Attempt {
    sequence: u64,
    started: Instant,
    target_fps: Option<u32>,
    vsync: Option<bool>,
}

#[derive(Debug)]
struct Active {
    origin: Instant,
    pending: Option<Attempt>,
    previous_submission: Option<Instant>,
    completion_sender: SyncSender<Completion>,
    completion_receiver: Receiver<Completion>,
    report: CaptureReport,
}

/// No sample storage exists until a capture is explicitly started.
#[derive(Debug, Default)]
pub(super) struct PresentationCapture {
    active: Option<Active>,
}

#[derive(Debug)]
pub(super) struct CaptureReport {
    rows: Vec<Row>,
    elapsed_us: u64,
    attempts: u64,
    submissions: u64,
    skipped: u64,
    skip_reasons: [u64; SKIP_NAMES.len()],
    menu_submissions: u64,
    world_submissions: u64,
    dropped_rows: u64,
    rejected_submissions: u64,
    interval_count: u64,
    interval_sum_us: u64,
    interval_min_us: Option<u64>,
    interval_max_us: Option<u64>,
    completion_requests: u64,
    completion_observed: u64,
    completion_pending: usize,
    completion_skipped: u64,
    completion_sum_us: u64,
    completion_max_us: Option<u64>,
}

fn micros(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

impl PresentationCapture {
    pub(super) fn start(&mut self, now: Instant) -> Result<(), &'static str> {
        if self.active.is_some() {
            return Err("a presentation capture is already active");
        }
        let (completion_sender, completion_receiver) = sync_channel(MAX_PENDING_COMPLETIONS);
        self.active = Some(Active {
            origin: now,
            pending: None,
            previous_submission: None,
            completion_sender,
            completion_receiver,
            report: CaptureReport {
                rows: Vec::with_capacity(MAX_ROWS),
                elapsed_us: 0,
                attempts: 0,
                submissions: 0,
                skipped: 0,
                skip_reasons: [0; SKIP_NAMES.len()],
                menu_submissions: 0,
                world_submissions: 0,
                dropped_rows: 0,
                rejected_submissions: 0,
                interval_count: 0,
                interval_sum_us: 0,
                interval_min_us: None,
                interval_max_us: None,
                completion_requests: 0,
                completion_observed: 0,
                completion_pending: 0,
                completion_skipped: 0,
                completion_sum_us: 0,
                completion_max_us: None,
            },
        });
        Ok(())
    }

    pub(super) fn begin_attempt(&mut self, now: Instant) {
        let Some(active) = self.active.as_mut() else { return };
        active.finish_skipped(now, SkipReason::Unclassified);
        active.report.attempts = active.report.attempts.saturating_add(1);
        active.pending = Some(Attempt {
            sequence: active.report.attempts,
            started: now,
            target_fps: None,
            vsync: None,
        });
    }

    pub(super) fn set_context(&mut self, target_fps: Option<u32>, vsync: bool) {
        if let Some(attempt) = self.active.as_mut().and_then(|active| active.pending.as_mut()) {
            attempt.target_fps = target_fps;
            attempt.vsync = Some(vsync);
        }
    }

    pub(super) fn skipped(&mut self, now: Instant, reason: SkipReason) {
        if let Some(active) = self.active.as_mut() {
            active.finish_skipped(now, reason);
        }
    }

    /// Called only after the existing surface present operation returns.
    pub(super) fn submitted(&mut self, now: Instant, kind: SubmissionKind) {
        let Some(active) = self.active.as_mut() else { return };
        let Some(attempt) = active.pending.take() else {
            active.report.rejected_submissions = active.report.rejected_submissions.saturating_add(1);
            return;
        };
        let interval = active.previous_submission.map(|last| micros(now.saturating_duration_since(last)));
        active.previous_submission = Some(now);
        let report = &mut active.report;
        report.submissions = report.submissions.saturating_add(1);
        match kind {
            SubmissionKind::Menu => report.menu_submissions = report.menu_submissions.saturating_add(1),
            SubmissionKind::World => report.world_submissions = report.world_submissions.saturating_add(1),
        }
        if let Some(interval) = interval {
            report.interval_count = report.interval_count.saturating_add(1);
            report.interval_sum_us = report.interval_sum_us.saturating_add(interval);
            report.interval_min_us = Some(report.interval_min_us.map_or(interval, |last| last.min(interval)));
            report.interval_max_us = Some(report.interval_max_us.map_or(interval, |last| last.max(interval)));
        }
        let sequence = report.submissions;
        active.push_row(attempt, now, kind as u64, Some(sequence), interval);
    }

    pub(super) fn stop(&mut self, now: Instant) -> Result<CaptureReport, &'static str> {
        let Some(mut active) = self.active.take() else {
            return Err("no presentation capture is active");
        };
        active.finish_skipped(now, SkipReason::Unclassified);
        active.harvest_completions();
        active.report.elapsed_us = micros(now.saturating_duration_since(active.origin));
        Ok(active.report)
    }

    pub(super) fn submitted_to_queue(
        &mut self, now: Instant, kind: SubmissionKind, device: &wgpu::Device, queue: &wgpu::Queue,
    ) {
        let Some(active) = self.active.as_mut() else { return };
        let row = (active.report.rows.len() < MAX_ROWS).then_some(active.report.rows.len());
        let has_attempt = active.pending.is_some();
        let _ = device.poll(wgpu::PollType::Poll);
        self.submitted(now, kind);
        if !has_attempt { return }
        let active = self.active.as_mut().unwrap();
        active.harvest_completions();
        if !active.reserve_completion() { return }
        let origin = active.origin;
        let sender = active.completion_sender.clone();
        queue.on_submitted_work_done(move || {
            let completed = Instant::now();
            let _ = sender.try_send((
                row, micros(completed.saturating_duration_since(origin)),
                micros(completed.saturating_duration_since(now)),
            ));
        });
    }
}

impl Active {
    fn reserve_completion(&mut self) -> bool {
        if self.report.completion_pending == MAX_PENDING_COMPLETIONS {
            self.report.completion_skipped = self.report.completion_skipped.saturating_add(1);
            return false;
        }
        self.report.completion_requests = self.report.completion_requests.saturating_add(1);
        self.report.completion_pending += 1;
        true
    }

    fn harvest_completions(&mut self) {
        while let Ok((row, completed_us, latency_us)) = self.completion_receiver.try_recv() {
            self.report.completion_pending -= 1;
            self.report.completion_observed = self.report.completion_observed.saturating_add(1);
            self.report.completion_sum_us = self.report.completion_sum_us.saturating_add(latency_us);
            self.report.completion_max_us = Some(
                self.report.completion_max_us.map_or(latency_us, |last| last.max(latency_us)),
            );
            if let Some(row) = row {
                self.report.rows[row][8] = Some(completed_us);
            }
        }
    }

    fn finish_skipped(&mut self, now: Instant, reason: SkipReason) {
        if let Some(attempt) = self.pending.take() {
            self.report.skipped = self.report.skipped.saturating_add(1);
            let count = &mut self.report.skip_reasons[reason.index()];
            *count = count.saturating_add(1);
            self.push_row(attempt, now, 0, None, None);
        }
    }

    fn push_row(&mut self, attempt: Attempt, now: Instant, outcome: u64, sequence: Option<u64>, interval: Option<u64>) {
        if self.report.rows.len() == MAX_ROWS {
            self.report.dropped_rows = self.report.dropped_rows.saturating_add(1);
            return;
        }
        self.report.rows.push([
            Some(attempt.sequence),
            Some(micros(attempt.started.saturating_duration_since(self.origin))),
            Some(micros(now.saturating_duration_since(self.origin))),
            Some(outcome),
            sequence,
            interval,
            attempt.target_fps.map(u64::from),
            attempt.vsync.map(u64::from),
            None,
        ]);
    }
}

impl CaptureReport {
    pub(super) fn to_json(&self) -> Result<String, serde_json::Error> {
        let json = serde_json::to_string(&serde_json::json!({
            "schema": 2,
            "metric": "successful-presentation-submission",
            "clock": "portable-monotonic",
            "timeUnit": "microseconds",
            "rowScope": "retained-prefix",
            "aggregateScope": "whole-capture",
            "maxRows": MAX_ROWS,
            "elapsedUs": self.elapsed_us,
            "attempts": self.attempts,
            "submissions": self.submissions,
            "skippedAttempts": self.skipped,
            "skipReasons": SKIP_NAMES.iter().zip(self.skip_reasons)
                .collect::<std::collections::BTreeMap<_, _>>(),
            "menuSubmissions": self.menu_submissions,
            "worldSubmissions": self.world_submissions,
            "droppedRows": self.dropped_rows,
            "rejectedSubmissions": self.rejected_submissions,
            "intervalCount": self.interval_count,
            "intervalSumUs": self.interval_sum_us,
            "intervalMinUs": self.interval_min_us,
            "intervalMaxUs": self.interval_max_us,
            "completionMetric": "gpu-queue-completion-callback",
            "completionMaxPending": MAX_PENDING_COMPLETIONS,
            "completionRequests": self.completion_requests,
            "completionObserved": self.completion_observed,
            "completionPendingAtStop": self.completion_pending,
            "completionSkipped": self.completion_skipped,
            "completionLatencySumUs": self.completion_sum_us,
            "completionLatencyMaxUs": self.completion_max_us,
            "columns": ["attempt", "startedUs", "finishedUs", "outcome", "submission", "intervalUs", "targetFps", "vsync", "gpuCompletionCallbackUs"],
            "outcomes": { "0": "not-submitted", "1": "menu", "2": "world" },
            "rows": self.rows,
        }))?;
        debug_assert!(json.len() < MAX_JSON_BYTES);
        Ok(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn skip_causes_are_exact_after_row_overflow_and_do_not_count_submissions() {
        use lodestone_render::TargetError;
        let origin = Instant::now();
        let mut capture = PresentationCapture::default();
        capture.skipped(origin, SkipReason::Paced);
        capture.start(origin).unwrap();
        let mut reasons = vec![
            SkipReason::Paced, SkipReason::MissingMenuState, SkipReason::MissingWorldState,
        ];
        for error in [
            TargetError::Timeout, TargetError::Occluded, TargetError::Outdated,
            TargetError::Lost, TargetError::Validation,
        ] {
            reasons.extend([SkipReason::MenuAcquire(error), SkipReason::WorldAcquire(error)]);
        }
        for reason in reasons {
            capture.begin_attempt(origin);
            capture.skipped(origin, reason);
            capture.skipped(origin, reason);
        }
        for _ in 0..MAX_ROWS {
            capture.begin_attempt(origin);
            capture.skipped(origin, SkipReason::MenuAcquire(TargetError::Occluded));
        }
        capture.begin_attempt(origin);
        capture.submitted(origin, SubmissionKind::World);
        capture.skipped(origin, SkipReason::Paced);
        capture.begin_attempt(origin);
        let report = capture.stop(origin + Duration::from_millis(8)).unwrap();
        assert_eq!(report.skip_reasons[0], 1);
        assert_eq!(report.skip_reasons[5], MAX_ROWS as u64 + 1);
        for (index, count) in report.skip_reasons.iter().enumerate() {
            if index != 5 { assert_eq!(*count, 1, "{}", SKIP_NAMES[index]); }
        }
        assert_eq!(report.skip_reasons.iter().sum::<u64>(), report.skipped);
        assert_eq!(report.submissions, 1);
        assert_eq!(report.rows.len(), MAX_ROWS);
        assert_eq!(report.dropped_rows, 15);
        let json: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();
        assert_eq!(json["skipReasons"]["menu-acquire-occluded"], MAX_ROWS as u64 + 1);
        assert_eq!(json["skipReasons"]["world-acquire-validation"], 1);
    }

    #[test]
    fn completion_backpressure_and_capture_stop_do_not_fabricate_ready_frames() {
        let origin = Instant::now();
        let mut capture = PresentationCapture::default();
        capture.start(origin).unwrap();
        capture.begin_attempt(origin);
        capture.submitted(origin + Duration::from_millis(2), SubmissionKind::World);
        let active = capture.active.as_mut().unwrap();
        for _ in 0..MAX_PENDING_COMPLETIONS { assert!(active.reserve_completion()); }
        assert!(!active.reserve_completion());
        active.completion_sender.try_send((Some(0), 17_000, 15_000)).unwrap();
        active.harvest_completions();
        assert_eq!(active.report.rows[0][8], Some(17_000));
        assert_eq!(active.report.completion_max_us, Some(15_000));
        assert_eq!(active.report.completion_pending, 2);
        let late_sender = active.completion_sender.clone();
        let report = capture.stop(origin + Duration::from_millis(20)).unwrap();
        assert_eq!((report.completion_requests, report.completion_observed), (3, 1));
        assert_eq!((report.completion_pending, report.completion_skipped), (2, 1));
        capture.start(origin + Duration::from_secs(1)).unwrap();
        assert!(late_sender.try_send((None, 30_000, 28_000)).is_err());
        let fresh = capture.stop(origin + Duration::from_secs(2)).unwrap();
        assert_eq!(fresh.completion_observed, 0);
        assert_eq!(fresh.completion_max_us, None);
    }

    #[test]
    #[ignore = "requires a live GPU adapter"]
    fn real_queue_completion_is_observed_without_a_presentation_claim() {
        let context = lodestone_render::GpuContext::new_headless_blocking().unwrap();
        let device = context.device();
        let queue = context.queue();
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("completion-control"), size: 4,
            usage: wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.clear_buffer(&buffer, 0, None);
        queue.submit([encoder.finish()]);
        let origin = Instant::now();
        let mut capture = PresentationCapture::default();
        capture.start(origin).unwrap();
        capture.begin_attempt(origin);
        capture.submitted_to_queue(Instant::now(), SubmissionKind::World, device, queue);
        let active = capture.active.as_ref().unwrap();
        assert_eq!(active.report.completion_observed, 0);
        assert_eq!(active.report.rows[0][8], None);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let report = capture.stop(Instant::now()).unwrap();
        assert_eq!((report.completion_requests, report.completion_observed), (1, 1));
        assert_eq!(report.completion_pending, 0);
        assert!(report.rows[0][8].unwrap() >= report.rows[0][2].unwrap());
        eprintln!("queue completion control: {}", report.to_json().unwrap());
    }

    #[test]
    fn menu_world_and_skipped_attempts_have_independent_counts() {
        let origin = Instant::now();
        let mut capture = PresentationCapture::default();
        capture.start(origin).unwrap();
        capture.begin_attempt(origin);
        capture.set_context(Some(120), true);
        capture.submitted(origin + Duration::from_millis(2), SubmissionKind::Menu);
        capture.begin_attempt(origin + Duration::from_millis(10));
        capture.begin_attempt(origin + Duration::from_millis(20));
        capture.submitted(origin + Duration::from_millis(23), SubmissionKind::World);
        let report = capture.stop(origin + Duration::from_millis(30)).unwrap();
        assert_eq!((report.attempts, report.submissions, report.skipped), (3, 2, 1));
        assert_eq!((report.menu_submissions, report.world_submissions), (1, 1));
        assert_eq!((report.interval_count, report.interval_sum_us), (1, 21_000));
        assert_eq!(report.rows[0][5], None);
        assert_eq!(report.rows[1][4], None);
        assert_eq!(report.rows[2][5], Some(21_000));
        assert_eq!(report.elapsed_us, 30_000);
    }

    #[test]
    fn unsubmitted_tail_and_duplicate_submission_cannot_inflate_fps() {
        let origin = Instant::now();
        let mut capture = PresentationCapture::default();
        capture.begin_attempt(origin);
        capture.submitted(origin, SubmissionKind::Menu);
        assert!(capture.active.is_none());
        capture.start(origin).unwrap();
        assert!(capture.start(origin).is_err());
        capture.submitted(origin, SubmissionKind::Menu);
        capture.begin_attempt(origin);
        capture.submitted(origin + Duration::from_millis(1), SubmissionKind::World);
        capture.submitted(origin + Duration::from_millis(2), SubmissionKind::World);
        capture.begin_attempt(origin + Duration::from_millis(3));
        let report = capture.stop(origin + Duration::from_millis(5)).unwrap();
        assert_eq!((report.attempts, report.submissions, report.skipped), (2, 1, 1));
        assert_eq!(report.rejected_submissions, 2);
        assert_eq!(report.interval_count, 0);
        assert_eq!(report.rows[1][3], Some(0));
        assert!(capture.stop(origin).is_err());
    }

    #[test]
    fn overflow_preserves_complete_counts_and_interval_max_with_bounded_export() {
        let origin = Instant::now();
        let mut capture = PresentationCapture::default();
        capture.start(origin).unwrap();
        for index in 0..MAX_ROWS + 2 {
            let at = origin + Duration::from_micros(index as u64 * 1_000);
            capture.begin_attempt(at);
            capture.submitted(at, SubmissionKind::World);
        }
        let last = origin + Duration::from_micros((MAX_ROWS as u64 + 1) * 1_000);
        let late = last + Duration::from_micros(5_903_000);
        capture.begin_attempt(late);
        capture.submitted(late, SubmissionKind::Menu);
        let report = capture.stop(late).unwrap();
        assert_eq!(report.rows.len(), MAX_ROWS);
        assert_eq!(report.dropped_rows, 3);
        assert_eq!(report.submissions, MAX_ROWS as u64 + 3);
        assert_eq!(report.interval_count, report.submissions - 1);
        assert_eq!(report.interval_max_us, Some(5_903_000));
        let json = report.to_json().unwrap();
        assert!(json.len() < MAX_JSON_BYTES);
        assert!(MAX_JSON_BYTES < 8 * 1_048_576);
        let worst_row = [Some(u64::MAX); 9];
        let worst_rows = vec![worst_row; MAX_ROWS];
        assert!(serde_json::to_string(&worst_rows).unwrap().len() + 4096 < MAX_JSON_BYTES);
    }
}
