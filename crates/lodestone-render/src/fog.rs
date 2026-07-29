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

    /// Dense, near, red-tinted Nether fog.
    ///
    /// Vanilla's Nether fog is not a render-distance edge fade like the
    /// overworld's: `the_nether` dimension type fixes
    /// `visual/fog_start_distance`/`visual/fog_end_distance` at `10.0`/`96.0`
    /// blocks regardless of render distance (`AtmosphericFogEnvironment.setupFog`
    /// reads those two attributes directly), so the haze is thick and close no
    /// matter how far the player can see. The colour is the `nether_wastes`
    /// biome's `visual/fog_color` (`#330808`) — the dimension type itself
    /// carries no `fog_color` override, only the per-biome attribute does, and
    /// every other Nether biome (crimson/warped forest, soul sand valley,
    /// basalt deltas) has its own distinct value the shell cannot yet reach
    /// (the biome the player is standing in is not threaded to this call) —
    /// the same documented-fallback shape `lodestone-shell`'s `water_fog`
    /// already uses for its one ocean default.
    ///
    /// `render_distance` (in chunks) is honoured only as an upper clamp: a
    /// render distance shorter than 96 blocks (6 chunks) must not fog *past*
    /// the loaded world, exactly like [`FogSettings::for_view_distance`]'s
    /// `end`.
    #[must_use]
    pub fn nether(render_distance: u32) -> Self {
        let end = 96.0_f32.min(render_distance as f32 * 16.0);
        let start = 10.0_f32.min(end);
        Self {
            color: srgb_u8_to_linear(NETHER_FOG_SRGB),
            start,
            end,
        }
    }

    /// The End's fog: a flat, near-black backdrop, since the dimension type
    /// carries no `visual/fog_start_distance`/`visual/fog_end_distance`
    /// override (so vanilla's environmental-fog attributes fall back to their
    /// defaults, `0`/`1024` blocks — effectively never triggering inside any
    /// normal render distance) and the visible darkening instead comes from
    /// `visual/fog_color` (`#181318`) mixed with `visual/sky_color`
    /// (`#000000`) at the render-distance edge, exactly the mechanism
    /// [`FogSettings::for_view_distance`] already models for the overworld.
    ///
    /// This reuses that edge-fade shape with the End's colour rather than
    /// vanilla's separate `sky_color`/`fog_color` blend curve
    /// (`AtmosphericFogEnvironment.getBaseColor`'s `skyColorMixFactor`): with
    /// no sky dome to blend into (the End draws its own starfield, which nothing
    /// in this renderer attempts), a single flat colour is the closest
    /// approximation reachable without a second bind-group slot or a new
    /// uniform lane. `start_fraction` should be the same value the caller uses
    /// for overworld fog (`crate::gpu::FOG_START_FRACTION` in the shell) so the
    /// edge dissolves at the same fraction of view distance in every dimension.
    #[must_use]
    pub fn the_end(render_distance: u32, start_fraction: f32) -> Self {
        Self::for_view_distance(
            srgb_u8_to_linear(END_FOG_SRGB),
            render_distance as f32 * 16.0,
            start_fraction,
        )
    }
}

/// `nether_wastes`'s `minecraft:visual/fog_color`, sRGB (see
/// `.cache/mc/26.2/client-src/data/minecraft/worldgen/biome/nether_wastes.json`).
const NETHER_FOG_SRGB: [u8; 3] = [0x33, 0x08, 0x08];

/// `the_end` dimension type's `minecraft:visual/fog_color`, sRGB (see
/// `.cache/mc/26.2/client-src/data/minecraft/dimension_type/the_end.json`).
const END_FOG_SRGB: [u8; 3] = [0x18, 0x13, 0x18];

/// Converts one sRGB `0xRRGGBB`-space colour (bytes `0..=255`) to linear RGB
/// (`0.0..=1.0`), using the accurate piecewise sRGB EOTF — the same formula
/// the model/entity WGSL shaders' own `srgb_to_linear` implements (see
/// `crate::model_pipeline`), so a CPU-computed dimension colour and a
/// shader-computed one agree bit-for-bit in shape.
///
/// Every dimension-colour constant here is stored as its real sRGB hex (as
/// authored in the decompiled data files) and converted through this function
/// rather than hand-typed as a linear literal, unlike `lodestone-shell`'s
/// `SKY_COLOR` — that constant's own doc comment records that a hand-typed
/// linear value was once silently the *sRGB* value instead, washing the sky
/// out. Computing it once, here, removes that whole class of transcription
/// error for every dimension colour added after it.
#[must_use]
pub fn srgb_u8_to_linear(rgb: [u8; 3]) -> [f32; 3] {
    rgb.map(|c| {
        let c = f32::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    })
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

    #[test]
    fn srgb_to_linear_hits_known_anchors() {
        // Pure black and pure white round-trip exactly regardless of the
        // piecewise toe.
        assert_eq!(srgb_u8_to_linear([0, 0, 0]), [0.0, 0.0, 0.0]);
        assert_eq!(srgb_u8_to_linear([255, 255, 255]), [1.0, 1.0, 1.0]);
        // Mid-grey (0x80) is the textbook sRGB check value: linear ~0.2159,
        // never the naive /255 = 0.502 an un-linearised read would give.
        let [r, g, b] = srgb_u8_to_linear([0x80, 0x80, 0x80]);
        for c in [r, g, b] {
            assert!((c - 0.215_86).abs() < 1e-4, "got {c}");
        }
        // Below the 0.04045 toe threshold the curve is exactly linear (c/12.92),
        // not the power curve — 0x0A/255 = 0.0392, under the threshold.
        let [r, ..] = srgb_u8_to_linear([0x0A, 0, 0]);
        assert!((r - (10.0 / 255.0 / 12.92)).abs() < 1e-6);
    }

    #[test]
    fn nether_fog_is_dense_red_and_clamped_to_render_distance() {
        // Vanilla: fixed 10..96 block range, `nether_wastes`' `#330808` fog
        // colour — red channel clearly dominant, green/blue near black.
        let f = FogSettings::nether(32);
        assert_eq!(f.start, 10.0);
        assert_eq!(f.end, 96.0);
        assert!(f.color[0] > f.color[1] * 4.0, "expected red-dominant fog");
        assert!(f.color[0] > 0.0 && f.color[0] < 1.0);

        // A render distance shorter than the vanilla range must not fog past
        // the loaded world, exactly like `for_view_distance`'s `end` clamp.
        let short = FogSettings::nether(2); // 32 blocks
        assert_eq!(short.end, 32.0);
        assert_eq!(short.start, 10.0);

        // And a render distance so short even the *start* would fall outside
        // the loaded world must not produce start > end (a degenerate,
        // fog-disabling range would silently turn off Nether fog entirely).
        let tiny = FogSettings::nether(0);
        assert_eq!(tiny.end, 0.0);
        assert_eq!(tiny.start, 0.0);
        assert!(tiny.start <= tiny.end);
    }

    #[test]
    fn the_end_fog_is_a_flat_near_black_edge_fade() {
        let f = FogSettings::the_end(16, 0.75);
        assert_eq!(f.end, 256.0);
        assert_eq!(f.start, 192.0);
        // `#181318` is dark and very slightly red/blue-leaning, but overall
        // near-black — nothing like the overworld's saturated sky blue.
        assert!(f.color.iter().all(|c| *c < 0.02), "expected near-black: {:?}", f.color);
        assert_eq!(f.color[0], f.color[2], "R and B channels match (#18__18)");
    }

    #[test]
    fn nether_and_end_fog_are_disjoint_from_the_overworld_sky() {
        // A regression pin for the actual bug this module exists to fix: the
        // Nether/End must not silently fall back to inheriting the caller's
        // sky-blue constant just because a dimension branch was missed.
        let overworld_sky = [0.242_867, 0.462_361, 0.827_571]; // gpu::SKY_COLOR
        assert_ne!(FogSettings::nether(16).color, overworld_sky);
        assert_ne!(FogSettings::the_end(16, 0.75).color, overworld_sky);
    }
}
