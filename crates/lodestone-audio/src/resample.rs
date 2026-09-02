//! Linear-interpolation resampling, used both for **pitch** (a playback-rate
//! multiplier) and for reconciling a sound's native sample rate with the output
//! device rate.
//!
//! # Vanilla parity
//!
//! Vanilla sets `AL_PITCH` on the OpenAL source, which resamples playback by
//! that factor, and clamps the pitch to `[0.5, 2.0]` before applying it.
//! Pitch and rate conversion compose into a single read ratio:
//! for each output frame the read head advances
//! `pitch * source_rate / output_rate` source frames.
//!
//! The interpolation *kernel* (OpenAL-Soft uses a higher-order/band-limited
//! resampler by default) is not reproduced; we use linear interpolation, which
//! is transparent for the small integer-ish ratios sounds actually use and keeps
//! the core dependency-free. This is an explicit approximation, not a parity
//! claim.

/// Linear interpolation between `a` and `b` by `t` in `[0, 1]`.
#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Reads `samples` at fractional position `pos` (in samples) with linear
/// interpolation. Positions at or past the last sample return the last sample;
/// negative positions return the first. An empty slice returns `0.0`.
pub fn lerp_read(samples: &[f32], pos: f64) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    if pos <= 0.0 {
        return samples[0];
    }
    let last = samples.len() - 1;
    let i = pos.floor() as usize;
    if i >= last {
        return samples[last];
    }
    let t = (pos - i as f64) as f32;
    lerp(samples[i], samples[i + 1], t)
}

/// Resamples a mono `input` by `ratio`, the number of input samples consumed per
/// output sample. `ratio > 1` speeds up / raises pitch and shortens the output;
/// `ratio < 1` slows down / lowers pitch and lengthens it. `ratio == 1` is the
/// identity. A non-positive `ratio` or empty input yields an empty vector.
pub fn resample_linear(input: &[f32], ratio: f64) -> Vec<f32> {
    if input.is_empty() || ratio <= 0.0 {
        return Vec::new();
    }
    if ratio == 1.0 {
        return input.to_vec();
    }
    let out_len = ((input.len() as f64) / ratio).ceil() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        out.push(lerp_read(input, i as f64 * ratio));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_read_interpolates_midpoint() {
        let s = [0.0, 1.0];
        assert_eq!(lerp_read(&s, 0.0), 0.0);
        assert_eq!(lerp_read(&s, 0.5), 0.5);
        assert_eq!(lerp_read(&s, 1.0), 1.0);
    }

    #[test]
    fn lerp_read_clamps_out_of_range() {
        let s = [2.0, 4.0, 6.0];
        assert_eq!(lerp_read(&s, -1.0), 2.0);
        assert_eq!(lerp_read(&s, 10.0), 6.0);
        assert_eq!(lerp_read(&[], 0.5), 0.0);
    }

    #[test]
    fn ratio_one_is_identity() {
        let s = vec![0.1, 0.2, 0.3, 0.4];
        assert_eq!(resample_linear(&s, 1.0), s);
    }

    #[test]
    fn double_ratio_halves_and_interpolates_a_ramp() {
        // A linear ramp resampled at 2x should stay a (steeper) linear ramp,
        // sampled at even indices: positions 0,2,4,6 -> values 0,2,4,6.
        let ramp: Vec<f32> = (0..8).map(|i| i as f32).collect();
        let out = resample_linear(&ramp, 2.0);
        assert_eq!(out.len(), 4);
        assert_eq!(out, vec![0.0, 2.0, 4.0, 6.0]);
    }

    #[test]
    fn half_ratio_doubles_and_interpolates_a_ramp() {
        // Slowing a ramp inserts the interpolated midpoints: positions
        // 0,0.5,1,1.5,... -> 0,0.5,1,1.5,...
        let ramp: Vec<f32> = (0..4).map(|i| i as f32).collect(); // 0,1,2,3
        let out = resample_linear(&ramp, 0.5);
        assert_eq!(out.len(), 8);
        assert_eq!(out, vec![0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.0]);
    }

    #[test]
    fn non_positive_ratio_and_empty_are_empty() {
        assert!(resample_linear(&[1.0, 2.0], 0.0).is_empty());
        assert!(resample_linear(&[1.0, 2.0], -1.0).is_empty());
        assert!(resample_linear(&[], 2.0).is_empty());
    }
}
