use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldgenProgress {
    pub session: u64,
    pub target: (i32, i32),
    pub admitted: u32,
    pub completed: u32,
    pub committed: u32,
    pub queued: u32,
    pub retained_bytes: usize,
    pub stage: &'static str,
}

impl WorldgenProgress {
    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn wire_delivered(target: (i32, i32), delivered: usize, remaining: usize) -> Self {
        let count = |value: usize| value.min(u32::MAX as usize) as u32;
        Self {
            session: 0,
            target,
            admitted: count(delivered.saturating_add(remaining)),
            completed: count(delivered),
            committed: 0,
            queued: count(remaining),
            retained_bytes: 0,
            stage: "wire-delivered",
        }
    }
}

pub type WorldgenProgressSink = fn(WorldgenProgress);

static SINK: OnceLock<WorldgenProgressSink> = OnceLock::new();

pub fn install_sink(sink: WorldgenProgressSink) -> Result<(), WorldgenProgressSink> {
    SINK.set(sink)
}

pub(crate) fn emit(progress: WorldgenProgress) {
    if let Some(sink) = SINK.get() {
        sink(progress);
    }
}

#[cfg(test)]
mod tests {
    use super::WorldgenProgress;

    #[test]
    fn wire_progress_counts_delivered_and_outstanding_targets_separately() {
        let progress = WorldgenProgress::wire_delivered((-3, 5), 17, 24);
        assert_eq!(progress.target, (-3, 5));
        assert_eq!((progress.admitted, progress.completed, progress.queued), (41, 17, 24));
        let finished = WorldgenProgress::wire_delivered((-3, 5), 41, 0);
        assert_eq!((finished.admitted, finished.completed, finished.queued), (41, 41, 0));
    }
}
