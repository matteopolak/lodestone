//! Marsaglia polar Gaussian, matching vanilla's `MarsagliaPolarGaussian`.
//!
//! Vanilla caches the second value of each generated pair, so the draw pattern
//! (and therefore which underlying `nextDouble`s are consumed on which call)
//! must match exactly for parity. This is a small state machine shared by both
//! random sources.

/// Cached-pair Gaussian state. `reset` is called on every `set_seed`.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Gaussian {
    next: f64,
    have_next: bool,
}

impl Gaussian {
    pub(crate) fn reset(&mut self) {
        self.have_next = false;
    }

    /// Draws the next standard-normal value, pulling uniforms from `next_double`.
    pub(crate) fn next(&mut self, mut next_double: impl FnMut() -> f64) -> f64 {
        if self.have_next {
            self.have_next = false;
            return self.next;
        }
        loop {
            let x = 2.0 * next_double() - 1.0;
            let y = 2.0 * next_double() - 1.0;
            let r2 = x * x + y * y;
            if r2 >= 1.0 || r2 == 0.0 {
                continue;
            }
            let multiplier = (-2.0 * r2.ln() / r2).sqrt();
            self.next = y * multiplier;
            self.have_next = true;
            return x * multiplier;
        }
    }
}
