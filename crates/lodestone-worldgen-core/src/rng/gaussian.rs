//! Marsaglia polar Gaussian, matching vanilla's `MarsagliaPolarGaussian`.
//!
//! Vanilla caches the second value of each generated pair, so the draw pattern
//! (and therefore which underlying `nextDouble`s are consumed on which call)
//! must match exactly for parity. This is a small state machine shared by both
//! random sources.
//!
//! ## The one 1-ulp exposure in this crate's numerics, and its blast radius
//!
//! Vanilla's own Marsaglia-polar Gaussian sampler is
//! `Math.sqrt(-2.0 * Math.log(radiusSquared) / radiusSquared)`. `Math.sqrt` is
//! correctly rounded and therefore exact, but **`Math.log` is specified only to
//! within 1 ulp** — and it is `Math`, not `StrictMath`, so *vanilla's own*
//! Gaussian is not bit-reproducible across JVM implementations. Our `r2.ln()`
//! is the platform libm, which is a third value with the same 1-ulp latitude.
//! There is no formulation that fixes this, because there is no single value to
//! be exact *to*.
//!
//! What makes it acceptable is the blast radius, which is zero blocks:
//!
//! * **Terrain never calls it.** `WorldgenRandom::next_gaussian` is
//!   `unimplemented!()` (`rng/mod.rs`) precisely because no terrain path draws a
//!   Gaussian; a caller appearing there would hit the panic, not a silent
//!   divergence.
//! * The only production consumer is `lodestone-render`'s weather jitter, a
//!   visual effect that reaches no world state.
//! * `rng_parity`'s `nextGaussian` comparison is a JVM oracle check, so if a
//!   platform libm ever did disagree it would surface as a **loud test failure
//!   on that machine** rather than as divergent worlds.
//!
//! If a terrain feature ever needs a Gaussian, this note is the reason it must
//! not simply be wired up: the value would have to be declared a bounded
//! divergence, not assumed exact.

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
