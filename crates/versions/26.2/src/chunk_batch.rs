//! Chunk-batch flow control for protocol 776.
//!
//! The server streams chunks in batches and gates further delivery on the client
//! acknowledging each one: `PlayerChunkSender` stops sending once ten batches go
//! unacknowledged and only ever decrements that counter when the client replies
//! with `chunk_batch_received`. A client that never acknowledges therefore loads
//! the spawn area and then stalls permanently — walking produces void — so the
//! acknowledgement is not optional.
//!
//! The acknowledgement carries a desired chunks-per-tick rate. The server clamps
//! it to `[0.01, 64.0]` and uses it to pace delivery, so a wrong value does not
//! error; it silently makes chunk streaming pathologically slow or bursty. The
//! estimator below therefore mirrors vanilla's `ChunkBatchSizeCalculator`
//! exactly: a weighted running average of per-chunk processing cost, with each
//! sample clamped to within 3× of the current average.

/// Running estimator of per-chunk processing cost, mirroring vanilla's
/// `ChunkBatchSizeCalculator`.
///
/// The wall clock is deliberately kept out of this type so the aggregation math
/// can be exercised against hand-computed values; callers measure each batch's
/// duration and pass it in explicitly.
#[derive(Debug, Clone)]
pub struct ChunkBatchSizeCalculator {
    aggregated_nanos_per_chunk: f64,
    old_samples_weight: u32,
}

impl Default for ChunkBatchSizeCalculator {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkBatchSizeCalculator {
    /// Cap on the weight given to the accumulated history, so the average keeps
    /// adapting instead of freezing once many samples have been folded in.
    const MAX_OLD_SAMPLES_WEIGHT: u32 = 49;
    /// Seed cost, matching vanilla's initial `aggregatedNanosPerChunk`.
    const INITIAL_NANOS_PER_CHUNK: f64 = 2_000_000.0;

    /// Creates a calculator seeded with vanilla's starting estimate.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            aggregated_nanos_per_chunk: Self::INITIAL_NANOS_PER_CHUNK,
            old_samples_weight: 1,
        }
    }

    /// Folds one finished batch into the running average.
    ///
    /// `batch_size` is the server-reported chunk count and `batch_duration_nanos`
    /// the wall-clock time the batch took to arrive. Empty batches are ignored,
    /// matching vanilla, so a zero-chunk batch never perturbs the estimate.
    pub fn on_batch_finished(&mut self, batch_size: i32, batch_duration_nanos: f64) {
        if batch_size > 0 {
            let nanos_per_chunk = batch_duration_nanos / f64::from(batch_size);
            let lower = self.aggregated_nanos_per_chunk / 3.0;
            let upper = self.aggregated_nanos_per_chunk * 3.0;
            let clamped = nanos_per_chunk.clamp(lower, upper);
            let weight = f64::from(self.old_samples_weight);
            self.aggregated_nanos_per_chunk =
                (self.aggregated_nanos_per_chunk * weight + clamped) / (weight + 1.0);
            self.old_samples_weight =
                (self.old_samples_weight + 1).min(Self::MAX_OLD_SAMPLES_WEIGHT);
        }
    }

    /// The desired chunks-per-tick rate to report to the server, mirroring
    /// vanilla's `getDesiredChunksPerTick`.
    #[must_use]
    pub fn desired_chunks_per_tick(&self) -> f32 {
        (7_000_000.0 / self.aggregated_nanos_per_chunk) as f32
    }
}
