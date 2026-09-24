use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldgenProgress {
    pub session: u64,
    pub admitted: u32,
    pub completed: u32,
    pub committed: u32,
    pub queued: u32,
    pub retained_bytes: usize,
    pub stage: &'static str,
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
