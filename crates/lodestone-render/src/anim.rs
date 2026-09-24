//! Block-texture animation as **immutable atlas + in-shader blend**.
//!
//! `impl-assets` settled the animation question by measurement against the real
//! jar: every physical animation frame is retained as its own atlas region
//! (`frame_pixel_rect` / `frame_uv`), so an interpolating sprite has *both*
//! frame N and frame N+1 resident simultaneously. The consequences that shape
//! this module:
//!
//! * The block atlas is **immutable after build** — no per-tick re-upload, no
//!   dynamic sub-region, no split atlas, no seam. [`crate::texture::GpuAtlas`]
//!   already uploads once and never mutates; we deliberately build **no**
//!   mutation machinery.
//! * Animation is a **per-material uniform** (which two regions to sample and
//!   how far between them), not a texture update. The fragment shader samples
//!   region N and region N+1 and lerps by the blend factor.
//!
//! This module is the pure, GPU-free half. Given a sprite's frame timeline and
//! the current game tick, [`SpriteAnimation::sample`] yields the two region
//! indices and a blend factor; [`AnimUniform`] is the 16-byte POD the shader
//! consumes. A producer (an assets bridge, later) fills [`SpriteAnimation`] from
//! `Atlas` animation info — nothing here depends on `lodestone-assets`, so the
//! timing logic is unit-tested headlessly. ~2,600 physical frames span the 1,233
//! block sprites, of which only 52 are animated, so the per-material uniform set
//! is tiny.

use bytemuck::{Pod, Zeroable};

/// One frame in a sprite's animation timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimFrame {
    /// Atlas region index of this frame — an index into the per-frame sprite
    /// rectangle list produced by [`crate::texture::sprite_rects`]. The shader
    /// turns this into a UV rect (2D atlas) or array layer.
    pub region: u32,
    /// How many game ticks this frame is held before advancing to the next.
    /// Must be at least 1 to contribute to the cycle; a zero-hold frame is
    /// skipped for timing purposes.
    pub hold_ticks: u32,
}

/// A sprite's animation timeline, decoupled from any asset representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpriteAnimation {
    /// Frames in play order. Wraps back to the first after the last.
    pub frames: Vec<AnimFrame>,
    /// Whether to cross-fade between consecutive frames (vanilla `interpolate`).
    /// When `false`, [`AnimSample::blend`] is always `0.0`.
    pub interpolate: bool,
}

/// The resolved animation state for one tick: the two regions to sample and how
/// far to blend from the first to the second (`0.0..1.0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimSample {
    /// Region currently shown (blend `0.0`).
    pub region_a: u32,
    /// Region blended toward (blend `1.0`).
    pub region_b: u32,
    /// Fraction from `region_a` toward `region_b`, in `0.0..1.0`.
    pub blend: f32,
}

impl SpriteAnimation {
    /// Total ticks in one full cycle (sum of every frame's hold time).
    #[must_use]
    pub fn cycle_ticks(&self) -> u64 {
        self.frames.iter().map(|f| u64::from(f.hold_ticks)).sum()
    }

    /// Resolve the animation state at an absolute game `tick`.
    ///
    /// Walks the timeline modulo the cycle length. When [`Self::interpolate`] is
    /// set, `blend` advances linearly across each frame's hold time exactly as
    /// vanilla does (`subFrame / frametime`), reaching but never equalling
    /// `1.0`; otherwise `blend` is `0.0`. A single-frame or zero-length timeline
    /// is static: both regions equal and `blend == 0.0`.
    #[must_use]
    pub fn sample(&self, tick: u64) -> AnimSample {
        let n = self.frames.len();
        if n == 0 {
            return AnimSample {
                region_a: 0,
                region_b: 0,
                blend: 0.0,
            };
        }
        let cycle = self.cycle_ticks();
        if n == 1 || cycle == 0 {
            let r = self.frames[0].region;
            return AnimSample {
                region_a: r,
                region_b: r,
                blend: 0.0,
            };
        }

        let mut t = tick % cycle;
        let mut i = 0;
        loop {
            let hold = u64::from(self.frames[i].hold_ticks);
            if t < hold || i == n - 1 {
                let hold = hold.max(1);
                let a = self.frames[i];
                let b = self.frames[(i + 1) % n];
                let blend = if self.interpolate {
                    // subFrame / frametime, clamped into 0.0..1.0.
                    (t as f32 / hold as f32).min(1.0)
                } else {
                    0.0
                };
                return AnimSample {
                    region_a: a.region,
                    region_b: b.region,
                    blend,
                };
            }
            t -= hold;
            i += 1;
        }
    }
}

/// The per-material animation state uploaded to the shader: which two atlas
/// regions to sample and how far to blend between them. 16 bytes so it packs
/// into a uniform array without straddling alignment.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct AnimUniform {
    /// Region shown at blend `0.0`.
    pub region_a: u32,
    /// Region shown at blend `1.0`.
    pub region_b: u32,
    /// Blend fraction, `0.0..1.0`.
    pub blend: f32,
    /// Padding to a 16-byte stride (std140/std430-friendly).
    pub _pad: f32,
}

impl From<AnimSample> for AnimUniform {
    fn from(s: AnimSample) -> Self {
        Self {
            region_a: s.region_a,
            region_b: s.region_b,
            blend: s.blend,
            _pad: 0.0,
        }
    }
}

/// The per-slot animation state the **model/fluid shaders** actually consume:
/// the two vertical UV offsets (in normalised atlas units) to add to a quad's
/// baked frame-0 V, and the blend between them.
///
/// The shader only knows a quad's baked frame-0 UV, not the atlas geometry of
/// the sprite behind it. So the region indices in [`AnimSample`] are resolved on
/// the CPU into concrete V offsets here — `region * frame_v` — keeping the
/// shader trivial: `sample(uv + vec2(0, v_off_a))` blended toward
/// `sample(uv + vec2(0, v_off_b))`. Slot `0` (static) is all-zero, so an
/// unanimated quad reads a no-op offset. 16 bytes for a std140-friendly array.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct AnimSlotUniform {
    /// V offset (normalised atlas units) of the frame shown at blend `0.0`.
    pub v_off_a: f32,
    /// V offset of the frame blended toward at blend `1.0`.
    pub v_off_b: f32,
    /// Blend fraction, `0.0..1.0`.
    pub blend: f32,
    /// Padding to a 16-byte stride (std140-friendly).
    pub _pad: f32,
}

impl AnimSlotUniform {
    /// The static (no-offset) slot uniform, for slot `0` and any static sprite.
    #[must_use]
    pub const fn static_slot() -> Self {
        Self {
            v_off_a: 0.0,
            v_off_b: 0.0,
            blend: 0.0,
            _pad: 0.0,
        }
    }

    /// Resolve a timeline sample into concrete V offsets, given the normalised
    /// height of one physical frame (`frame_height / atlas_height`).
    #[must_use]
    pub fn from_sample(sample: AnimSample, frame_v: f32) -> Self {
        Self {
            v_off_a: sample.region_a as f32 * frame_v,
            v_off_b: sample.region_b as f32 * frame_v,
            blend: sample.blend,
            _pad: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(spec: &[(u32, u32)]) -> Vec<AnimFrame> {
        spec.iter()
            .map(|&(region, hold_ticks)| AnimFrame { region, hold_ticks })
            .collect()
    }

    #[test]
    fn empty_timeline_is_static_region_zero() {
        let a = SpriteAnimation {
            frames: vec![],
            interpolate: true,
        };
        assert_eq!(
            a.sample(123),
            AnimSample {
                region_a: 0,
                region_b: 0,
                blend: 0.0
            }
        );
    }

    #[test]
    fn single_frame_never_blends() {
        let a = SpriteAnimation {
            frames: frames(&[(7, 4)]),
            interpolate: true,
        };
        for t in [0, 1, 3, 99] {
            let s = a.sample(t);
            assert_eq!((s.region_a, s.region_b), (7, 7));
            assert_eq!(s.blend, 0.0);
        }
    }

    #[test]
    fn two_frames_uniform_hold_no_interpolation() {
        let a = SpriteAnimation {
            frames: frames(&[(0, 2), (1, 2)]),
            interpolate: false,
        };
        assert_eq!((a.sample(0).region_a, a.sample(0).region_b), (0, 1));
        assert_eq!(a.sample(0).blend, 0.0);
        // Still in frame 0 at t=1.
        assert_eq!(a.sample(1).region_a, 0);
        // Advanced to frame 1 at t=2, which wraps toward frame 0.
        assert_eq!((a.sample(2).region_a, a.sample(2).region_b), (1, 0));
        // Cycle length is 4: t=4 is identical to t=0.
        assert_eq!(a.sample(4), a.sample(0));
    }

    #[test]
    fn interpolation_advances_linearly_across_hold() {
        let a = SpriteAnimation {
            frames: frames(&[(0, 4), (1, 4)]),
            interpolate: true,
        };
        assert_eq!(a.sample(0).blend, 0.0);
        assert_eq!(a.sample(1).blend, 0.25);
        assert_eq!(a.sample(2).blend, 0.5);
        assert_eq!(a.sample(3).blend, 0.75);
        // Crossing into frame 1 resets the blend against frame 1→0.
        let s = a.sample(4);
        assert_eq!((s.region_a, s.region_b), (1, 0));
        assert_eq!(s.blend, 0.0);
    }

    #[test]
    fn non_uniform_hold_times_walk_correctly() {
        // frame 0 held 1 tick, frame 1 held 3 ticks; cycle = 4.
        let a = SpriteAnimation {
            frames: frames(&[(5, 1), (6, 3)]),
            interpolate: true,
        };
        assert_eq!(a.cycle_ticks(), 4);
        assert_eq!(a.sample(0).region_a, 5);
        assert_eq!(a.sample(0).blend, 0.0);
        // t=1..=3 are inside frame 1 (hold 3): blends 0, 1/3, 2/3.
        assert_eq!(a.sample(1).region_a, 6);
        assert!((a.sample(1).blend - 0.0).abs() < 1e-6);
        assert!((a.sample(2).blend - 1.0 / 3.0).abs() < 1e-6);
        assert!((a.sample(3).blend - 2.0 / 3.0).abs() < 1e-6);
        // Wrap.
        assert_eq!(a.sample(4), a.sample(0));
    }

    #[test]
    fn uniform_is_sixteen_bytes_and_pod() {
        assert_eq!(core::mem::size_of::<AnimUniform>(), 16);
        let u = AnimUniform::from(AnimSample {
            region_a: 3,
            region_b: 4,
            blend: 0.5,
        });
        let bytes: &[u8] = bytemuck::bytes_of(&u);
        assert_eq!(bytes.len(), 16);
        assert_eq!(u.region_a, 3);
        assert_eq!(u.region_b, 4);
        assert_eq!(u.blend, 0.5);
    }
}
