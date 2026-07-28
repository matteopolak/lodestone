//! Linear distance fog: the shared math and GPU uniform for fading distant
//! terrain into a flat fog colour.
//!
//! Fog is what hides the render-distance edge — without it the loaded world
//! ends in a hard wall of geometry against the sky. Vanilla fades the last few
//! chunks into the sky (or, submerged, into a short biome-coloured water fog)
//! so the edge is never a visible seam. This module owns the *decision* half:
//! a pure `fog_factor` over a fragment's view distance and the [`FogUniform`]
//! the shader reads, both constructible and testable without a GPU. The shader
//! applies `mix(fragment, fog_colour, fog_factor)` using exactly this math.
//!
//! The factor is **linear** between `start` and `end` (vanilla's `RENDER`
//! distance fog is linear; the exponential water fog is a separate, later
//! concern). `start`/`end` are world-space distances from the eye.

use bytemuck::{Pod, Zeroable};

/// Linear distance-fog parameters, in world units from the eye.
///
/// `color` is the colour distant geometry fades to (sky colour above water,
/// biome water colour when submerged). Fog is *off* when `end <= start`
/// (a degenerate range), which callers use to disable fog without a branch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FogSettings {
    /// Linear RGB colour distant geometry fades to.
    pub color: [f32; 3],
    /// Distance from the eye at which fog begins (factor 0).
    pub start: f32,
    /// Distance from the eye at which fog is full (factor 1).
    pub end: f32,
}

impl FogSettings {
    /// Fog disabled: a degenerate range so [`fog_factor`] is always 0.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            color: [0.0, 0.0, 0.0],
            start: 0.0,
            end: 0.0,
        }
    }

    /// Distance fog that fades the outer edge of a `view_distance`-block render
    /// volume. Fog begins at `start_fraction` of the view distance and reaches
    /// full at the view distance itself, so the edge chunks dissolve rather
    /// than pop. `start_fraction` is clamped to `0.0..=1.0`.
    #[must_use]
    pub fn for_view_distance(color: [f32; 3], view_distance: f32, start_fraction: f32) -> Self {
        let end = view_distance.max(0.0);
        let start = end * start_fraction.clamp(0.0, 1.0);
        Self { color, start, end }
    }
}

/// The linear fog factor for a fragment `distance` world units from the eye:
/// `0.0` nearer than `start`, `1.0` beyond `end`, linearly interpolated
/// between, and always `0.0` for a degenerate range (`end <= start`) so fog can
/// be disabled by collapsing the range.
#[must_use]
pub fn fog_factor(distance: f32, start: f32, end: f32) -> f32 {
    if end <= start {
        return 0.0;
    }
    ((distance - start) / (end - start)).clamp(0.0, 1.0)
}

/// Blend a fragment `color` toward `fog_color` by `factor` (component-wise
/// `mix`). `factor` is assumed already clamped to `0.0..=1.0`.
#[must_use]
pub fn apply_fog(color: [f32; 3], fog_color: [f32; 3], factor: f32) -> [f32; 3] {
    [
        color[0] + (fog_color[0] - color[0]) * factor,
        color[1] + (fog_color[1] - color[1]) * factor,
        color[2] + (fog_color[2] - color[2]) * factor,
    ]
}

/// GPU uniform for the fog pass: the eye's world position (so the shader can
/// measure each fragment's view distance) plus the fog colour and range.
///
/// Laid out as three `vec4`s for std140 uniform alignment. `enabled` is `0.0`
/// or `1.0`; the shader multiplies the computed factor by it so a disabled fog
/// costs one multiply rather than a divergent branch.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FogUniform {
    /// `xyz` = eye world position; `w` unused.
    pub eye: [f32; 4],
    /// `rgb` = fog colour; `w` = `start` distance.
    pub color_start: [f32; 4],
    /// `x` = `end` distance; `y` = `enabled` (0/1); `zw` unused.
    pub end_enabled: [f32; 4],
}

impl FogUniform {
    /// Build the uniform from settings and the eye's world position. Fog is
    /// marked enabled unless the range is degenerate (`end <= start`).
    #[must_use]
    pub fn new(settings: &FogSettings, eye: [f32; 3]) -> Self {
        let enabled = if settings.end > settings.start {
            1.0
        } else {
            0.0
        };
        Self {
            eye: [eye[0], eye[1], eye[2], 0.0],
            color_start: [
                settings.color[0],
                settings.color[1],
                settings.color[2],
                settings.start,
            ],
            end_enabled: [settings.end, enabled, 0.0, 0.0],
        }
    }

    /// A disabled-fog uniform (factor always 0), for frames with no fog.
    #[must_use]
    pub fn disabled() -> Self {
        Self::new(&FogSettings::disabled(), [0.0, 0.0, 0.0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factor_is_zero_before_start_and_one_after_end() {
        assert_eq!(fog_factor(0.0, 10.0, 20.0), 0.0);
        assert_eq!(fog_factor(10.0, 10.0, 20.0), 0.0);
        assert_eq!(fog_factor(20.0, 10.0, 20.0), 1.0);
        assert_eq!(fog_factor(100.0, 10.0, 20.0), 1.0);
    }

    #[test]
    fn factor_is_linear_between_start_and_end() {
        assert!((fog_factor(15.0, 10.0, 20.0) - 0.5).abs() < 1e-6);
        assert!((fog_factor(12.5, 10.0, 20.0) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn degenerate_range_disables_fog() {
        // end == start and end < start both yield no fog, whatever the distance.
        assert_eq!(fog_factor(1000.0, 20.0, 20.0), 0.0);
        assert_eq!(fog_factor(1000.0, 20.0, 10.0), 0.0);
    }

    #[test]
    fn apply_fog_interpolates_toward_fog_colour() {
        let frag = [0.2, 0.4, 0.6];
        let fog = [1.0, 1.0, 1.0];
        assert_eq!(apply_fog(frag, fog, 0.0), frag);
        assert_eq!(apply_fog(frag, fog, 1.0), fog);
        let mid = apply_fog(frag, fog, 0.5);
        assert!((mid[0] - 0.6).abs() < 1e-6);
        assert!((mid[1] - 0.7).abs() < 1e-6);
        assert!((mid[2] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn for_view_distance_puts_end_at_the_distance() {
        let f = FogSettings::for_view_distance([0.5, 0.6, 0.7], 160.0, 0.75);
        assert_eq!(f.end, 160.0);
        assert_eq!(f.start, 120.0);
        // A fragment at the very edge is fully fogged; one at 3/4 is not yet.
        assert_eq!(fog_factor(160.0, f.start, f.end), 1.0);
        assert_eq!(fog_factor(120.0, f.start, f.end), 0.0);
    }

    #[test]
    fn uniform_marks_enabled_only_for_a_real_range() {
        let on = FogUniform::new(
            &FogSettings::for_view_distance([0.1; 3], 100.0, 0.5),
            [1.0, 2.0, 3.0],
        );
        assert_eq!(on.eye, [1.0, 2.0, 3.0, 0.0]);
        assert_eq!(on.color_start[3], 50.0); // start
        assert_eq!(on.end_enabled[0], 100.0); // end
        assert_eq!(on.end_enabled[1], 1.0); // enabled

        let off = FogUniform::disabled();
        assert_eq!(off.end_enabled[1], 0.0);
    }

    #[test]
    fn uniform_is_48_bytes_three_vec4s() {
        assert_eq!(std::mem::size_of::<FogUniform>(), 48);
    }
}
