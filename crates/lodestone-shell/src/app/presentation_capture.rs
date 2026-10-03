//! Bounded opt-in records of successful surface presentation submissions.

use crate::platform::Instant;

pub(super) const MAX_ROWS: usize = 4096;
/// Eight nullable integers per row, each at most twenty decimal digits.
pub(super) const MAX_JSON_BYTES: usize = MAX_ROWS * (8 * 21 + 2) + 8192;

type Row = [Option<u64>; 8];

#[derive(Debug, Clone, Copy)]
pub(super) enum SubmissionKind {
    Menu = 1,
    World = 2,
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
    menu_submissions: u64,
    world_submissions: u64,
    dropped_rows: u64,
    rejected_submissions: u64,
    interval_count: u64,
    interval_sum_us: u64,
    interval_min_us: Option<u64>,
    interval_max_us: Option<u64>,
}

fn micros(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

impl PresentationCapture {
    pub(super) fn start(&mut self, now: Instant) -> Result<(), &'static str> {
        if self.active.is_some() {
            return Err("a presentation capture is already active");
        }
        self.active = Some(Active {
            origin: now,
            pending: None,
            previous_submission: None,
            report: CaptureReport {
                rows: Vec::with_capacity(MAX_ROWS),
                elapsed_us: 0,
                attempts: 0,
                submissions: 0,
                skipped: 0,
                menu_submissions: 0,
                world_submissions: 0,
                dropped_rows: 0,
                rejected_submissions: 0,
                interval_count: 0,
                interval_sum_us: 0,
                interval_min_us: None,
                interval_max_us: None,
            },
        });
        Ok(())
    }

    pub(super) fn begin_attempt(&mut self, now: Instant) {
        let Some(active) = self.active.as_mut() else { return };
        active.finish_skipped(now);
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
        active.finish_skipped(now);
        active.report.elapsed_us = micros(now.saturating_duration_since(active.origin));
        Ok(active.report)
    }
}

impl Active {
    fn finish_skipped(&mut self, now: Instant) {
        if let Some(attempt) = self.pending.take() {
            self.report.skipped = self.report.skipped.saturating_add(1);
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
        ]);
    }
}

impl CaptureReport {
    pub(super) fn to_json(&self) -> Result<String, serde_json::Error> {
        let json = serde_json::to_string(&serde_json::json!({
            "schema": 1,
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
            "menuSubmissions": self.menu_submissions,
            "worldSubmissions": self.world_submissions,
            "droppedRows": self.dropped_rows,
            "rejectedSubmissions": self.rejected_submissions,
            "intervalCount": self.interval_count,
            "intervalSumUs": self.interval_sum_us,
            "intervalMinUs": self.interval_min_us,
            "intervalMaxUs": self.interval_max_us,
            "columns": ["attempt", "startedUs", "finishedUs", "outcome", "submission", "intervalUs", "targetFps", "vsync"],
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
        let late = origin + Duration::from_secs(10);
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
        assert!(MAX_JSON_BYTES < 1_048_576);
        let worst_row = [Some(u64::MAX); 8];
        let worst_rows = vec![worst_row; MAX_ROWS];
        assert!(serde_json::to_string(&worst_rows).unwrap().len() + 4096 < MAX_JSON_BYTES);
    }
}
