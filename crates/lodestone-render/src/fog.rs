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
//! The factor is **linear** between `start` and `end` (vanilla's own
//! render-distance
//! fog is linear; the exponential water fog is a separate, later
//! concern). `start`/`end` are world-space distances from the eye.

use bytemuck::{Pod, Zeroable};

/// Linear distance-fog parameters, in world units from the eye — plus the sky
/// colour they are set alongside.
///
/// `color` is the colour distant geometry fades to (sky colour above water,
/// biome water colour when submerged). Fog is *off* when `end <= start`
/// (a degenerate range), which callers use to disable fog without a branch.
///
/// # Why the sky colour lives in a struct named for fog
///
/// Because in vanilla they are one record and they must not be settable apart.
/// Vanilla's per-dimension environmental attributes carry `visual/fog_color`
/// and `visual/sky_color` side by side, its fog-colour computation blends
/// them, and the sky disc's horizon *is* its fog end (its shader fogs the
/// flat disc, so the gradient runs
/// from `sky_color` at the centre to `color` at the rim). Two setters that can
/// be called independently is exactly how the horizon has previously banded in
/// a colour the sky never is — see `RenderState::set_clear_color`'s doc. One
/// struct, set in one call, cannot drift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FogSettings {
    /// Linear RGB colour distant geometry fades to. This is also the **horizon
    /// end** of the sky disc's gradient.
    pub color: [f32; 3],
    /// Linear RGB colour at the **centre** of the sky disc, before the
    /// `SKY_COLOR` day/night track multiplies it.
    ///
    /// Vanilla's `minecraft:visual/sky_color`: per-biome in the overworld,
    /// and every constructor here defaults it to `color` — one flat colour
    /// for both ends of the gradient, distinguished only by the two
    /// day/night tracks — unless a caller overrides it with the standing
    /// biome's own declared value.
    pub sky_color: [f32; 3],
    /// Distance from the eye at which the **render-distance** fog term begins
    /// (factor 0). Measured **cylindrically** (`max(horizontal, |dy|)`,
    /// vanilla's shader-side distance metric) in the shader — see [`FogUniform`].
    pub start: f32,
    /// Distance from the eye at which the render-distance term is full
    /// (factor 1).
    pub end: f32,
    /// Distance from the eye at which vanilla's **environmental** fog term
    /// begins (factor 0). Measured **spherically** (`length(pos)`), and
    /// combined with the render-distance term by `max`
    /// (vanilla's total-fog combine), the same combine
    /// [`total_fog_factor`] and the shaders' `fog_amount` implement.
    ///
    /// Vanilla's overworld/End declare no
    /// `visual/fog_start_distance`/`visual/fog_end_distance` override, so this
    /// falls back to the registered default `0.0..1024.0`
    /// — a real, very slow haze, not an
    /// inert placeholder; see [`for_render_distance`](Self::for_render_distance).
    /// Defaults to `0.0` (degenerate with `environmental_end`, i.e. no
    /// contribution) for constructors that do not set it.
    pub environmental_start: f32,
    /// Distance from the eye at which the environmental term is full (factor
    /// 1). See [`environmental_start`](Self::environmental_start).
    pub environmental_end: f32,
}

impl FogSettings {
    /// Fog disabled: a degenerate range so [`fog_factor`] is always 0.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            color: [0.0, 0.0, 0.0],
            sky_color: [0.0, 0.0, 0.0],
            start: 0.0,
            end: 0.0,
            environmental_start: 0.0,
            environmental_end: 0.0,
        }
    }

    /// The same settings with a different sky-disc centre colour — the
    /// per-biome `minecraft:visual/sky_color`.
    ///
    /// The fog colour is deliberately left alone: the biome's own
    /// `visual/fog_color` is a separate attribute this client does not decode
    /// yet, and painting the horizon with the *sky* colour would flatten the
    /// gradient the sky pass exists to draw.
    #[must_use]
    pub fn with_sky_color(mut self, sky_color: [f32; 3]) -> Self {
        self.sky_color = sky_color;
        self
    }

    /// [`with_sky_color`](Self::with_sky_color) when the standing biome declared
    /// one, and a **no-op** on `None`.
    ///
    /// `None` is the single meaning "the server has not told us" that every hop
    /// in this resolution chain uses — pre-login, a server that sent no biome
    /// registry, an unstreamed column, or one of the ten Nether/End biomes that
    /// declare no `sky_color`. Each falls back to whatever colour the caller
    /// already computed for the dimension, and never to a plausible-looking
    /// overworld blue (matching by name instead of by declared per-dimension
    /// data is exactly the hardcoded-fallback shape that goes wrong for a
    /// non-standard dimension).
    #[must_use]
    pub fn with_biome_sky_color(self, sky_color: Option<[f32; 3]>) -> Self {
        match sky_color {
            Some(sky_color) => self.with_sky_color(sky_color),
            None => self,
        }
    }

    /// A plain linear ramp over a `view_distance`-block range, beginning at
    /// `start_fraction` of it. `start_fraction` is clamped to `0.0..=1.0`.
    ///
    /// This is the **generic** constructor, and it is the wrong
    /// one for the render-distance edge fade — use
    /// [`for_render_distance`](Self::for_render_distance), which implements
    /// vanilla's actual span rather than a fraction. What is left here is
    /// vanilla's *environmental* fog shape: a range the caller states outright,
    /// which is what vanilla's own water and lava fog environments, and the declared
    /// `visual/fog_start_distance`/`visual/fog_end_distance` attributes are.
    /// Those must **not** acquire a render-distance span — water fog ramps from
    /// the eye (`start_fraction` 0), and folding the span in would push its
    /// start out to within four blocks of its end.
    ///
    /// # Why this still writes `start`/`end`, not `environmental_start`/`end`
    ///
    /// Vanilla's own water and lava fog environments set exactly the
    /// *environmental* attributes, so the semantically pure port would land
    /// this range in `environmental_start`/`environmental_end` and leave
    /// `start`/`end` disabled. That was tried and reverted: `sim.rs`'s water
    /// and dry-eye fog are compared field-for-field in several tests (e.g.
    /// `a_submerged_eye_selects_short_dense_fog_over_the_sky_fog`), and moving
    /// this constructor's output to a different pair of fields flips which
    /// term wins the shader's `max` for every existing caller without
    /// touching a single one of them. This constructor's range stays in the
    /// render-distance pair, which keeps every current caller — water, lava,
    /// and `sim::fog_for_render_distance`'s pre-#388 fraction path — pixel-
    /// identical to the range it always wrote. The one measurable cost: the shader
    /// now measures this pair **cylindrically** rather than spherically, which
    /// is a few percent of a block of difference at the short ranges these
    /// callers use and does not reach the reported symptom (open-sky
    /// hilltop/dropoff fog, `docs/fog.md`'s F2/F3).
    #[must_use]
    pub fn for_view_distance(color: [f32; 3], view_distance: f32, start_fraction: f32) -> Self {
        let end = view_distance.max(0.0);
        let start = end * start_fraction.clamp(0.0, 1.0);
        Self {
            color,
            sky_color: color,
            start,
            end,
            environmental_start: 0.0,
            environmental_end: 0.0,
        }
    }

    /// Distance fog that fades the outer edge of the loaded world, on vanilla's
    /// own curve.
    ///
    /// Vanilla does **not** start this fog at a fraction of the view distance.
    /// Vanilla's render-distance fog setup instead derives the fade span as a
    /// clamped fraction of the render distance itself: a tenth of the render
    /// distance in blocks, clamped to between 4 and 64, and the fog starts
    /// that span back from the render-distance edge.
    ///
    /// The fade band is an **absolute, capped width** measured back from the
    /// edge — a tenth of the view distance, never narrower than 4 blocks and
    /// never wider than 64 — not a proportion of it. The two models diverge
    /// fast, and always in the direction of this client being hazier: the old
    /// `start_fraction = 0.75` put the onset at 75% of the view distance where
    /// vanilla puts it at 90%, so at render distance 16 a fragment 240 blocks
    /// out read **0.75** fogged against vanilla's **0.375** — twice the haze,
    /// with the onset 38 blocks nearer.
    ///
    /// Note the cap is what makes this not merely "0.9 instead of 0.75": beyond
    /// 640 blocks (render distance 40) the band stops widening, so the fade is a
    /// progressively thinner rim rather than a quarter of an ever-larger volume.
    ///
    /// # The environmental term, and the shader's distance metric
    ///
    /// Vanilla combines *two* terms in its total-fog shader function:
    /// `max(linear(spherical, environmental_start, environmental_end),
    /// linear(cylindrical, render_distance_start, render_distance_end))`. This
    /// constructor builds the second (render-distance) term in `start`/`end`,
    /// measured **cylindrically** in the shader
    /// (`max(length(rel.xz), abs(rel.y))`), and now also
    /// fills in the first: the plain overworld and the End declare no
    /// `visual/fog_start_distance`/`visual/fog_end_distance` override, so
    /// vanilla's environmental term is the registered default
    /// `0.0..1024.0` — a real, very slow
    /// spherical haze, not an inert placeholder. Reaches `0.125` at 128 blocks
    /// and `0.5` at 512, so it is the dominant term in the *middle* of the
    /// view where the render-distance band has not started yet, which is what
    /// closes the "vanilla has a longer dropoff" gap. A dimension whose
    /// environmental attributes actually differ (the Nether) overrides these
    /// two fields after calling this — see [`FogSettings::nether`].
    #[must_use]
    pub fn for_render_distance(color: [f32; 3], render_distance_chunks: u32) -> Self {
        let end = render_distance_chunks as f32 * 16.0;
        let start = (end - render_distance_fade_span(end)).max(0.0);
        Self {
            color,
            sky_color: color,
            start,
            end,
            environmental_start: 0.0,
            environmental_end: 1024.0,
        }
    }

    /// Dense, near, red-tinted Nether fog.
    ///
    /// Vanilla's Nether fog is not a render-distance edge fade like the
    /// overworld's: `the_nether` dimension type fixes
    /// `visual/fog_start_distance`/`visual/fog_end_distance` at `10.0`/`96.0`
    /// blocks regardless of render distance (vanilla's own atmospheric
    /// fog-environment's fog-setup function
    /// reads those two attributes directly), so the haze is thick and close no
    /// matter how far the player can see. The colour is the `nether_wastes`
    /// biome's `visual/fog_color` (`0x330808`) — the dimension type itself
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
    ///
    /// Vanilla applies the render-distance term here **as well** — vanilla's
    /// own fog-setup function
    /// sets its own render-distance start/end fields unconditionally, after the environment
    /// hook, and its own total-fog-value function takes the `max` of the two. Both terms are
    /// now modelled explicitly: `environmental_start`/`environmental_end`
    /// carry the fixed `10.0`/`96.0` (clamped to the loaded world, exactly as
    /// this constructor always clamped its one pair before), and `start`/`end`
    /// carry [`FogSettings::for_render_distance`]'s real render-distance band
    /// rather than being repurposed to hold the Nether's own short range —
    /// that repurposing was the pre-F2/F3 approximation, exact for every
    /// render distance at or above 6 chunks (the render-distance ramp does not
    /// leave zero until `renderDistanceInBlocks - span`, which is ≥ 86.4
    /// blocks by then, and by 96 blocks the environmental term is already
    /// saturated at 1.0, so the `max` could never pick the other one) but
    /// wrong in the general case, and wrong for the reported "too extreme"
    /// symptom's twin (F3): the render-distance term the old single pair fed
    /// was measured against a spherical distance in the shader, not the
    /// cylindrical one vanilla actually uses.
    #[must_use]
    pub fn nether(render_distance: u32) -> Self {
        let color = srgb_u8_to_linear(NETHER_FOG_SRGB);
        let mut s = Self::for_render_distance(color, render_distance);
        let env_end = 96.0_f32.min(render_distance as f32 * 16.0);
        let env_start = 10.0_f32.min(env_end);
        s.environmental_start = env_start;
        s.environmental_end = env_end;
        s
    }

    /// The End's fog: a flat, near-black backdrop, since the dimension type
    /// carries no `visual/fog_start_distance`/`visual/fog_end_distance`
    /// override, so vanilla's environmental-fog attributes fall back to their
    /// registered defaults, `0.0`/`1024.0`, and the visible darkening instead
    /// comes from
    /// `visual/fog_color` (`0x181318`) mixed with `visual/sky_color`
    /// (`0x000000`) at the render-distance edge, exactly the mechanism
    /// [`FogSettings::for_render_distance`] models for the overworld.
    ///
    /// Those defaults are **not** inert, which an earlier version of this
    /// comment claimed: `linear(spherical, 0.0, 1024.0)` is a real, very slow
    /// spherical haze that reaches `0.125` at 128 blocks and `0.5` at 512, so
    /// vanilla's End (and overworld) carry a mild mid-field wash. It is the
    /// *environmental* term, and this constructor now draws it: it inherits
    /// [`for_render_distance`](Self::for_render_distance)'s
    /// `environmental_start`/`environmental_end` (`0.0`/`1024.0`) unchanged,
    /// since the End declares no override for those attributes either.
    ///
    /// This reuses that edge-fade shape with the End's colour rather than
    /// vanilla's separate `sky_color`/`fog_color` blend curve
    /// (vanilla's own atmospheric fog-environment's base-colour function's own
    /// sky-colour mix factor): with
    /// no sky dome to blend into (the End draws its own starfield, which nothing
    /// in this renderer attempts), a single flat colour is the closest
    /// approximation reachable without a second bind-group slot or a new
    /// uniform lane.
    ///
    /// `start_fraction` is a **floor**, not the shape: the band is vanilla's
    /// [`render_distance_fade_span`] unless the caller asks for an even later
    /// onset. Since every caller passes something at or below `0.9`
    /// (`crate::gpu::FOG_START_FRACTION` in the shell), the span is what
    /// actually decides, and the End's render-distance term fades on exactly
    /// the same curve as the overworld's, with the environmental term layered
    /// on top of both exactly as it is for the overworld (above). The
    /// parameter survives only because removing it would break the shell's
    /// call site, which another change owns.
    #[must_use]
    pub fn the_end(render_distance: u32, start_fraction: f32) -> Self {
        let mut s = Self::for_render_distance(srgb_u8_to_linear(END_FOG_SRGB), render_distance);
        s.start = s.start.max(s.end * start_fraction.clamp(0.0, 1.0));
        s
    }
}

/// The width, in blocks, of vanilla's render-distance fade band for a view
/// distance of `view_distance_blocks`: the render distance in blocks divided
/// by 10, clamped between 4 and 64.
///
/// A tenth of the view distance, floored at 4 blocks and **capped at 64**. The
/// cap is the part a "fraction of view distance" model cannot express: past 640
/// blocks the band stops growing, so the proportion of the visible world that is
/// hazy shrinks as the render distance rises instead of staying fixed.
///
/// Worked values, so the curve is legible without running it: 32 blocks (RD 2)
/// → 4.0 (floored); 128 (RD 8) → 12.8; 256 (RD 16) → 25.6; 512 (RD 32) → 51.2;
/// 768 (RD 48) → 64.0 (capped).
#[must_use]
pub fn render_distance_fade_span(view_distance_blocks: f32) -> f32 {
    (view_distance_blocks / 10.0).clamp(4.0, 64.0)
}

/// How far above the world bottom the void darkening starts, and where the
/// bottom is — the two numbers vanilla's void fog is a function of.
///
/// Vanilla's void-fog colour computation reads exactly these two and nothing
/// else: it computes a `darkness` fraction as `(onset_range + min_y - eye_y)
/// / onset_range`, clamped to `0..1`, then multiplies each fog channel by the
/// *square* of `1 - darkness`.
///
/// Note what that expression actually says, because the sign is easy to get
/// backwards from a summary: `darkness` is `0` at `min_y + onset_range` and
/// `1` **at** `min_y`, so the fog goes black as the eye *descends* to the world
/// bottom. The multiplier is the *square* of `1 - darkness`, so the falloff is
/// quadratic rather than linear.
///
/// `onset_range` is not a constant: vanilla's level-data accessor for it
/// returns `1.0` for a **flat** world and `32.0`
/// otherwise, so a superflat world's void fog is a 1-block-tall snap rather
/// than a 32-block fade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoidFog {
    /// The dimension's bottom Y, in blocks.
    pub min_y: f32,
    /// How many blocks above [`min_y`](Self::min_y) the darkening begins.
    pub onset_range: f32,
}

impl VoidFog {
    /// A normal (non-flat) overworld: `min_y = -64`, `onset_range = 32`.
    ///
    /// This is the bring-up default `RenderState` seeds itself with, not a
    /// claim that every dimension matches it — see
    /// `docs/sky-and-air-bubbles.md` on why the dimension's real height is not
    /// threaded to the render layer yet.
    pub const OVERWORLD: Self = Self {
        min_y: -64.0,
        onset_range: 32.0,
    };

    /// The void fog for a connected level: the dimension type's own `min_y`,
    /// and vanilla's void-darkness onset range, which forks on flatness —
    /// `1.0` when the level uses the flat world generator, `32.0` otherwise.
    ///
    /// **The flat arm is not a rounding of the normal one, it is a different
    /// picture.** With an onset of `1.0` the fade is a one-block snap at the
    /// very bottom of the world, so a superflat world whose ground sits a few
    /// blocks above `min_y` renders under a normal sky; with `32.0` the same
    /// ground is deep inside the fade and the sky goes nearly black. Neither
    /// number can be guessed from the geometry — only the level's own
    /// `is_flat` flag separates them.
    #[must_use]
    pub fn for_level(min_y: f32, is_flat: bool) -> Self {
        Self {
            min_y,
            onset_range: if is_flat { 1.0 } else { 32.0 },
        }
    }

    /// Void fog turned off: a degenerate onset range, which
    /// [`brightness`](Self::brightness) reports as always `1.0` (no darkening)
    /// rather than dividing by zero.
    pub const DISABLED: Self = Self {
        min_y: f32::NEG_INFINITY,
        onset_range: 0.0,
    };

    /// The multiplier vanilla applies to the **gamma-space** fog colour for an
    /// eye at `eye_y`: `1.0` at or above `min_y + onset_range`, `0.0` at
    /// `min_y`, quadratic between.
    ///
    /// Gamma-space is not a detail: vanilla's fog-colour computation scales
    /// each channel as a raw `byte / 255`, never linearised — so
    /// applying this to a linear-light colour would pull the whole curve toward
    /// `1.0` and wash the darkening out, exactly the failure `CLAUDE.md`
    /// records for tint and shade. Use [`scale_gamma`] rather than multiplying
    /// a linear colour by this directly.
    #[must_use]
    pub fn brightness(&self, eye_y: f32) -> f32 {
        if self.onset_range <= 0.0 {
            return 1.0;
        }
        let darkness =
            ((self.onset_range + self.min_y - eye_y) / self.onset_range).clamp(0.0, 1.0);
        let b = 1.0 - darkness;
        b * b
    }
}

/// Multiply a **linear** RGB colour by a gamma-space scalar, the way vanilla's
/// non-colour-managed pipeline does: decode to sRGB, scale, re-encode.
///
/// `CLAUDE.md`'s rendering constraints spell out why this cannot be a plain
/// linear multiply — a scale applied in linear space pulls every factor toward
/// `1.0`, so a void-fog `brightness` of `0.25` would read as roughly `0.53` of
/// the original brightness on screen instead of a quarter of it.
#[must_use]
pub fn scale_gamma(linear: [f32; 3], scale: f32) -> [f32; 3] {
    linear.map(|c| srgb_to_linear_f32(linear_to_srgb_f32(c) * scale))
}

/// Multiply a **linear** RGB colour by a per-channel gamma-space factor, the
/// way vanilla's packed-colour multiply does (`red(lhs) * red(rhs) / 255`,
/// straight byte arithmetic on sRGB values).
///
/// `factor` is in `0.0..=1.0` sRGB units, i.e. a `#RRGGBB` divided by 255, not
/// a linear-light ratio. The per-channel twin of [`scale_gamma`].
#[must_use]
pub fn multiply_gamma(linear: [f32; 3], factor: [f32; 3]) -> [f32; 3] {
    [
        srgb_to_linear_f32(linear_to_srgb_f32(linear[0]) * factor[0]),
        srgb_to_linear_f32(linear_to_srgb_f32(linear[1]) * factor[1]),
        srgb_to_linear_f32(linear_to_srgb_f32(linear[2]) * factor[2]),
    ]
}

/// Component-wise linear → sRGB (the accurate piecewise OETF), matching the
/// `linear_to_srgb` every WGSL shader in this crate implements.
#[must_use]
pub fn linear_to_srgb_f32(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.max(0.0).powf(1.0 / 2.4) - 0.055
    }
}

/// Component-wise sRGB → linear (the accurate piecewise EOTF); the inverse of
/// [`linear_to_srgb_f32`] and the float twin of [`srgb_u8_to_linear`].
#[must_use]
pub fn srgb_to_linear_f32(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c.max(0.0) + 0.055) / 1.055).powf(2.4)
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

/// Vanilla's total-fog shader function: the `max` of two
/// independent linear ramps over two different distance metrics — the
/// **environmental** term (spherical distance, `environmental_start/end`) and
/// the **render-distance** term (cylindrical distance, `start/end`,
/// `max(length(pos.xz), abs(pos.y))`). This is the CPU twin
/// of every fogged shader's `fog_amount`, so the headless gates describe the
/// shader rather than a separate model of it — see `model.wgsl`,
/// `entity.wgsl`, `fluid.wgsl`.
///
/// `eye`/`world` are both world-space; `world - eye` is the fragment-relative
/// vector each metric is measured over, exactly as the shaders compute it.
#[must_use]
pub fn total_fog_factor(settings: &FogSettings, eye: [f32; 3], world: [f32; 3]) -> f32 {
    let rel = [world[0] - eye[0], world[1] - eye[1], world[2] - eye[2]];
    let spherical = (rel[0] * rel[0] + rel[1] * rel[1] + rel[2] * rel[2]).sqrt();
    let cylindrical = (rel[0] * rel[0] + rel[2] * rel[2]).sqrt().max(rel[1].abs());
    let env = fog_factor(spherical, settings.environmental_start, settings.environmental_end);
    let rd = fog_factor(cylindrical, settings.start, settings.end);
    env.max(rd)
}

/// Blend a fragment `color` toward `fog_color` by `factor` (component-wise
/// `mix`), **in whatever space the inputs are in**. `factor` is assumed already
/// clamped to `0.0..=1.0`.
///
/// This is the CPU twin of `sky_disc.wgsl`'s fragment stage, which mixes its two
/// linear-light vertex colours directly. It is **not** the twin of the world
/// shaders any more: `model.wgsl`, `fluid.wgsl` and `entity.wgsl` mix in gamma
/// space, per vanilla — use [`apply_fog_gamma`] for those.
#[must_use]
pub fn apply_fog(color: [f32; 3], fog_color: [f32; 3], factor: f32) -> [f32; 3] {
    [
        color[0] + (fog_color[0] - color[0]) * factor,
        color[1] + (fog_color[1] - color[1]) * factor,
        color[2] + (fog_color[2] - color[2]) * factor,
    ]
}

/// Blend a **linear** fragment colour toward a **linear** `fog_color` by
/// `factor` the way vanilla does — in gamma space — and return linear light.
/// The CPU twin of the fog mix in `model.wgsl`, `fluid.wgsl` and `entity.wgsl`.
///
/// # Why gamma, and how much it mattered
///
/// Vanilla's fog-apply shader step is `mix(color, fog_color, fog_value)` over
/// a terrain fragment colour that is itself `texture * vertex_color` — raw,
/// non-colour-managed bytes — against a fog colour that came from unpacking a
/// packed 32-bit colour into a 0..1 vector, i.e.
/// bytes over 255. Nothing in that chain is linear light, exactly as
/// `CLAUDE.md`'s rendering constraints record for tint and shade.
///
/// This client mixed in linear light until it did not. The defect is a
/// *magnitude* one — the sign is right, so "distant things are foggier" holds
/// under both — and linear mixing always pulls the result toward the brighter
/// colour, i.e. toward the fog, with the **largest** error at the **smallest**
/// factor. That is what a player sees as "too foggy too early".
///
/// Worked from constants outside this code (sRGB 0.3 fragment, sRGB 0.75 fog),
/// as apparent gamma-space fog factor:
///
/// | true factor | correct (gamma) | as shipped (linear) | overshoot |
/// |---|---|---|---|
/// | 0.25 | 0.25 | 0.373 | +49% |
/// | 0.50 | 0.50 | 0.627 | +25% |
/// | 1.00 | 1.00 | 1.00 | none — the two spaces agree at both ends |
///
/// The endpoints agreeing is why `fog_gate.rs` (which measures a *fully* fogged
/// fragment) could not see this, and why the horizon seam between the sky disc
/// — still a linear mix, see [`apply_fog`] — and terrain stays invisible.
#[must_use]
pub fn apply_fog_gamma(color: [f32; 3], fog_color: [f32; 3], factor: f32) -> [f32; 3] {
    let c = color.map(linear_to_srgb_f32);
    let f = fog_color.map(linear_to_srgb_f32);
    [
        srgb_to_linear_f32(c[0] + (f[0] - c[0]) * factor),
        srgb_to_linear_f32(c[1] + (f[1] - c[1]) * factor),
        srgb_to_linear_f32(c[2] + (f[2] - c[2]) * factor),
    ]
}

/// GPU uniform for the fog pass: the eye's world position (so the shader can
/// measure each fragment's view distance) plus the fog colour and range.
///
/// Laid out as four `vec4`s for std140 uniform alignment. `enabled` is `0.0`
/// or `1.0`; the shader multiplies the computed factor by it so a disabled fog
/// costs one multiply rather than a divergent branch.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FogUniform {
    /// `xyz` = eye world position; `w` = `environmental_start` (gates F2/F3 —
    /// the two lanes this struct did not grow to add, per
    /// `docs/fog.md`'s bind-group budget note).
    pub eye: [f32; 4],
    /// `rgb` = fog colour; `w` = the **render-distance** term's `start`
    /// distance (measured cylindrically in the shader).
    pub color_start: [f32; 4],
    /// `x` = the render-distance term's `end` distance; `y` = `enabled`
    /// (0/1); `z` = sky-darken factor (set by callers after [`new`](Self::new),
    /// not by this constructor); `w` = `environmental_end` (measured
    /// spherically in the shader).
    pub end_enabled: [f32; 4],
    /// `rgb` = this frame's dimension ambient-light colour — the floor
    /// vanilla's lightmap shader seeds its accumulator with before either
    /// light half is
    /// added (`crate::light::light_color_from_levels`'s `ambient` parameter).
    /// Grey in the overworld, warm brown in the Nether, sage in the End; see
    /// `crate::light::OVERWORLD_AMBIENT_LIGHT` and `rgb24_to_channels`. `w` =
    /// this frame's clock, in the same seconds a section's `build_time` uses
    /// (see `model_pipeline::section_visibility`) — the only consumer of this
    /// lane is the per-section fade-in mix in `model.wgsl`/`fluid.wgsl`, which
    /// never reads `rgb` from it. Set by callers after [`new`](Self::new),
    /// exactly like `end_enabled`'s sky-darken lane — `new` has no dimension
    /// or clock to ask, so it defaults `rgb` to the overworld's own value (see
    /// that function's doc) and `w` to `0.0`.
    ///
    /// This is a **fourth** `vec4`, not a reused spare lane: every lane of the
    /// first three was already spoken for (`docs/fog.md`'s "previously-free
    /// lanes" note), and growing this struct's byte size does not cost a bind
    /// group — the 4-bind-group floor in `CLAUDE.md` limits how many *groups*
    /// a shader binds, not the size of one group's uniform buffer, and fog
    /// already rides group 0 for exactly that reason.
    pub ambient_light: [f32; 4],
}

impl FogUniform {
    /// Build the uniform from settings and the eye's world position. Fog is
    /// marked enabled unless **both** ranges are degenerate — a settings
    /// value with only the environmental pair populated (nothing currently
    /// constructs one, but nothing should silently disable it either) must
    /// still fog.
    ///
    /// [`FogUniform::ambient_light`] defaults to the overworld's own
    /// ambient-light colour (`crate::light::OVERWORLD_AMBIENT_LIGHT`) — the
    /// same "safe, not brighter than before" default every other unset source
    /// in this codebase uses (compare `SkyDarkenSource`'s `1.0`/permanent-noon
    /// default). A caller with a real dimension overwrites it after
    /// construction, exactly like the sky-darken lane.
    #[must_use]
    pub fn new(settings: &FogSettings, eye: [f32; 3]) -> Self {
        let enabled = if settings.end > settings.start
            || settings.environmental_end > settings.environmental_start
        {
            1.0
        } else {
            0.0
        };
        let ambient = crate::light::OVERWORLD_AMBIENT_LIGHT;
        Self {
            eye: [eye[0], eye[1], eye[2], settings.environmental_start],
            color_start: [
                settings.color[0],
                settings.color[1],
                settings.color[2],
                settings.start,
            ],
            end_enabled: [settings.end, enabled, 0.0, settings.environmental_end],
            ambient_light: [ambient[0], ambient[1], ambient[2], 0.0],
        }
    }

    /// A disabled-fog uniform (factor always 0), for frames with no fog.
    #[must_use]
    pub fn disabled() -> Self {
        Self::new(&FogSettings::disabled(), [0.0, 0.0, 0.0])
    }
}

#[cfg(test)]
mod ramp_gate {
    //! The discriminator that matters: **where the render-distance ramp starts and
    //! how wide it is**, sampled along a ray at known distances.
    //!
    //! Not "distant things are foggier" — the old 0.75-fraction model satisfies
    //! that too, and satisfied it while being twice as hazy as vanilla in the
    //! outer fifth of the view. Every expectation below is a literal, written
    //! out from vanilla's arithmetic by hand rather than computed by calling
    //! [`render_distance_fade_span`], so this is not
    //! `decode(encode(x)) == x` against our own formula. The source is
    //! vanilla's render-distance fog setup and shader ramp:
    //!
    //! ```text
    //! span  = clamp(rd_blocks / 10, 4, 64)
    //! start = rd_blocks - span
    //! end   = rd_blocks
    //! f(d)  = clamp((d - start) / span, 0, 1)
    //! ```
    //!
    //! Failure output is a **bounding box** over the ray — the first and last
    //! sampled distance that disagreed, plus the worst one — never a fraction of
    //! samples. A count cannot distinguish "the ramp is 38 blocks too early"
    //! from "one endpoint is off by a rounding error", and telling those apart
    //! is the entire job here.

    use super::*;

    /// A point `s` blocks from `eye` along unit direction `dir`.
    fn along(eye: [f32; 3], dir: [f32; 3], s: f32) -> [f32; 3] {
        [
            eye[0] + dir[0] * s,
            eye[1] + dir[1] * s,
            eye[2] + dir[2] * s,
        ]
    }

    /// What the shaders measure: `length(world - eye)`. Spherical, per
    /// `model.wgsl`/`entity.wgsl`/`fluid.wgsl`'s `fog_amount(length(in.world -
    /// camera.fog_eye.xyz))`.
    fn spherical(eye: [f32; 3], p: [f32; 3]) -> f32 {
        let d = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    }

    /// Vanilla's cylindrical fog-distance function:
    /// `max(length(pos.xz), abs(pos.y))`. This client does **not** use it; it is
    /// here so the gap can be measured rather than described.
    fn cylindrical(eye: [f32; 3], p: [f32; 3]) -> f32 {
        let d = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
        (d[0] * d[0] + d[2] * d[2]).sqrt().max(d[1].abs())
    }

    /// March `dir` from `eye`, comparing the settings' factor at each listed
    /// distance against the hand-written expectation. Panics with a bounding box
    /// over the ray, in blocks.
    fn assert_ramp(what: &str, fog: &FogSettings, eye: [f32; 3], dir: [f32; 3], table: &[(f32, f32)]) {
        const TOL: f32 = 1e-4;
        let mut bad: Vec<(f32, f32, f32)> = Vec::new();
        for &(s, expected) in table {
            let p = along(eye, dir, s);
            let got = fog_factor(spherical(eye, p), fog.start, fog.end);
            if (got - expected).abs() > TOL {
                bad.push((s, got, expected));
            }
        }
        if bad.is_empty() {
            return;
        }
        let lo = bad.iter().map(|b| b.0).fold(f32::INFINITY, f32::min);
        let hi = bad.iter().map(|b| b.0).fold(f32::NEG_INFINITY, f32::max);
        let worst = bad
            .iter()
            .max_by(|a, b| {
                (a.1 - a.2)
                    .abs()
                    .partial_cmp(&(b.1 - b.2).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
            .unwrap_or((0.0, 0.0, 0.0));
        let mut detail = String::new();
        for (s, got, expected) in &bad {
            detail.push_str(&format!("\n    d={s:>7.1}  got {got:.4}  want {expected:.4}"));
        }
        panic!(
            "{what}: ramp disagrees with vanilla over [{lo:.1}, {hi:.1}] blocks along the ray \
             ({} of {} samples); worst at d={:.1}: got {:.4}, want {:.4}. \
             Our ramp is {:.1}..{:.1}.{detail}",
            bad.len(),
            table.len(),
            worst.0,
            worst.1,
            worst.2,
            fog.start,
            fog.end,
        );
    }

    /// Eye deliberately off the origin and not at a round number, so a sample
    /// that accidentally measured a world position instead of an eye-relative
    /// one would not pass by coincidence.
    const EYE: [f32; 3] = [137.5, 71.25, -412.0];
    /// Level, +X. Chosen because `spherical == cylindrical` along it, which is
    /// what makes these expectations vanilla-*exact* rather than an
    /// approximation this client happens to agree with. The pitched case, where
    /// the two metrics diverge, is pinned separately below.
    const LEVEL_X: [f32; 3] = [1.0, 0.0, 0.0];

    /// Render distance 16 — the case in the report. Vanilla: `span =
    /// clamp(256/10, 4, 64) = 25.6`, so the fade runs `230.4 → 256`.
    #[test]
    fn ramp_matches_vanilla_at_render_distance_16() {
        let fog = FogSettings::for_render_distance([0.2, 0.4, 0.8], 16);
        assert_eq!(fog.end, 256.0);
        assert_ramp(
            "RD 16",
            &fog,
            EYE,
            LEVEL_X,
            &[
                (96.0, 0.0),
                (128.0, 0.0),
                (192.0, 0.0),
                (224.0, 0.0),
                (230.4, 0.0),
                (236.8, 0.25),
                (243.2, 0.50),
                (249.6, 0.75),
                (256.0, 1.0),
                (300.0, 1.0),
            ],
        );
    }

    /// The floor and the cap, which are the two things a fraction cannot
    /// express. RD 2 (`32/10 = 3.2`, floored to 4) and RD 48 (`768/10 = 76.8`,
    /// capped at 64).
    #[test]
    fn ramp_matches_vanilla_at_the_span_floor_and_cap() {
        let near = FogSettings::for_render_distance([0.2, 0.4, 0.8], 2);
        assert_ramp(
            "RD 2 (span floored to 4)",
            &near,
            EYE,
            LEVEL_X,
            &[(24.0, 0.0), (28.0, 0.0), (29.0, 0.25), (30.0, 0.5), (32.0, 1.0)],
        );

        let far = FogSettings::for_render_distance([0.2, 0.4, 0.8], 48);
        assert_ramp(
            "RD 48 (span capped at 64)",
            &far,
            EYE,
            LEVEL_X,
            &[
                (640.0, 0.0),
                (704.0, 0.0),
                (720.0, 0.25),
                (736.0, 0.5),
                (768.0, 1.0),
            ],
        );
    }

    /// RD 8 and RD 32, the two other distances anyone actually plays at.
    #[test]
    fn ramp_matches_vanilla_at_render_distances_8_and_32() {
        assert_ramp(
            "RD 8",
            &FogSettings::for_render_distance([0.2, 0.4, 0.8], 8),
            EYE,
            LEVEL_X,
            &[
                (64.0, 0.0),
                (96.0, 0.0),
                (115.2, 0.0),
                (118.4, 0.25),
                (121.6, 0.5),
                (124.8, 0.75),
                (128.0, 1.0),
            ],
        );
        assert_ramp(
            "RD 32",
            &FogSettings::for_render_distance([0.2, 0.4, 0.8], 32),
            EYE,
            LEVEL_X,
            &[
                (384.0, 0.0),
                (460.8, 0.0),
                (473.6, 0.25),
                (486.4, 0.5),
                (499.2, 0.75),
                (512.0, 1.0),
            ],
        );
    }

    /// **The negative control.** The shipped-until-#388 model — a fixed 0.75
    /// fraction of the view distance — must fail the RD 16 table, and must fail
    /// it *in the outer fifth of the view*, not at the endpoints. Both endpoints
    /// agree (0 near, 1 at the edge), which is exactly why a "distant things are
    /// foggier" assertion could never see this.
    ///
    /// Asserting the panic message's bounding box, rather than merely that it
    /// panicked, is what makes this a control for the *detector*: it proves the
    /// gate localises the defect instead of reporting a global fraction.
    #[test]
    fn the_old_fraction_model_fails_this_gate() {
        let old = FogSettings::for_view_distance([0.2, 0.4, 0.8], 16.0 * 16.0, 0.75);
        assert_eq!((old.start, old.end), (192.0, 256.0), "the model being controlled for");

        let err = std::panic::catch_unwind(|| {
            assert_ramp(
                "control",
                &old,
                EYE,
                LEVEL_X,
                &[
                    (96.0, 0.0),
                    (128.0, 0.0),
                    (192.0, 0.0),
                    (224.0, 0.0),
                    (230.4, 0.0),
                    (236.8, 0.25),
                    (243.2, 0.50),
                    (249.6, 0.75),
                    (256.0, 1.0),
                    (300.0, 1.0),
                ],
            );
        })
        .expect_err("the 0.75-fraction ramp must not satisfy vanilla's table");

        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "<non-string panic>".to_owned());
        assert!(
            msg.contains("[224.0, 249.6]"),
            "the control must localise to the outer fifth, where the two models \
             actually differ — got: {msg}"
        );
        // And the magnitude, not just the location: at 224 blocks the old model
        // is half-fogged where vanilla is perfectly clear.
        assert!(msg.contains("d=  224.0  got 0.5000  want 0.0000"), "got: {msg}");
    }

    /// A frame's *average* fog moves under almost any change to these numbers,
    /// so it is not evidence. This pins that the frame-average statistic the
    /// gate above deliberately avoids genuinely cannot tell the two models
    /// apart at the resolution that matters.
    #[test]
    fn a_frame_average_could_not_have_caught_this() {
        let vanilla = FogSettings::for_render_distance([0.0; 3], 16);
        let old = FogSettings::for_view_distance([0.0; 3], 256.0, 0.75);
        // Averaged over the whole view volume both models are mostly zero, and
        // the means land within a few percent of each other...
        let mean = |f: &FogSettings| {
            let n = 256;
            (0..n)
                .map(|i| fog_factor(i as f32, f.start, f.end))
                .sum::<f32>()
                / n as f32
        };
        let (a, b) = (mean(&vanilla), mean(&old));
        assert!(
            (a - b).abs() < 0.13,
            "frame means {a:.3} vs {b:.3} — if these ever separate cleanly, say so"
        );
        // ...while at a single sampled location they differ by half the range.
        let d = 224.0;
        let gap = fog_factor(d, old.start, old.end) - fog_factor(d, vanilla.start, vanilla.end);
        assert!(
            gap > 0.49,
            "at d={d} the two models must differ by ~0.5, got {gap:.3}"
        );
    }

    /// **Gate C (F2/F3), a pin that the gap is
    /// closed.** Vanilla's render-distance term measures a
    /// **cylindrical** distance (`max(length(pos.xz), abs(pos.y))`),
    /// and every shader here now measures the same cylindrical distance too.
    /// Along a ray pitched down 36.87° the cylindrical distance is `0.8 ×` the
    /// spherical, so this client reached full fog while vanilla was barely
    /// into its ramp — this test's own history is the old assertion
    /// `ours - vanilla > 0.6`, which passed before F2/F3 landed. `ours` now
    /// goes through [`total_fog_factor`], the CPU twin of the shaders'
    /// `fog_amount`, instead of the single-term `fog_factor` the old version
    /// called directly — that is the whole fix, restated as a test.
    #[test]
    fn cylindrical_distance_and_the_environmental_term_close_the_pitched_ray_gap() {
        let fog = FogSettings::for_render_distance([0.2, 0.4, 0.8], 16);
        let dir = [0.8, -0.6, 0.0];
        let s = 300.0;
        let p = along(EYE, dir, s);

        let sph = spherical(EYE, p);
        let cyl = cylindrical(EYE, p);
        assert!((sph - 300.0).abs() < 1e-3, "spherical {sph}");
        assert!((cyl - 240.0).abs() < 1e-3, "cylindrical {cyl}");

        // Vanilla's total: max(environmental, render-distance). The overworld
        // declares no fog distances, so environmental is the registered
        // default 0..1024, matching
        // `for_render_distance`'s new `environmental_start`/`environmental_end`.
        let vanilla_env = fog_factor(sph, 0.0, 1024.0);
        let vanilla_rd = fog_factor(cyl, fog.start, fog.end);
        let vanilla = vanilla_env.max(vanilla_rd);
        assert!((vanilla_rd - 0.375).abs() < 1e-4, "vanilla rd term {vanilla_rd}");
        assert!((vanilla - 0.375).abs() < 1e-4, "vanilla total {vanilla}");

        let ours = total_fog_factor(&fog, EYE, p);
        assert!(
            (ours - vanilla).abs() < 1e-4,
            "ours {ours} must now match vanilla {vanilla} — this is the gap that used \
             to be pinned open here"
        );

        // Negative control, executed and observed to fail: the pre-fix
        // single-term spherical model must still clearly overshoot, proving
        // this gate would have caught the bug it now confirms is fixed.
        let old_single_term = fog_factor(sph, fog.start, fog.end);
        assert!((old_single_term - 1.0).abs() < 1e-4, "control {old_single_term}");
        assert!(
            old_single_term - vanilla > 0.6,
            "control must clearly overshoot vanilla: got {old_single_term}, vanilla {vanilla}"
        );
    }

    /// **Gate B (F2).** The environmental term reaching zero was the "no
    /// dropoff at all until the last 10%" complaint. Expectations are
    /// vanilla's environmental-fog combine plus the registered
    /// default (`0.0..1024.0`), evaluated along [`LEVEL_X`] where spherical
    /// and cylindrical distance agree, so the table is metric-independent —
    /// the same property [`ramp_matches_vanilla_at_render_distances_8_and_32`]
    /// relies on.
    #[test]
    fn environmental_term_extends_the_ramp_past_the_old_hard_wall() {
        let fog = FogSettings::for_render_distance([0.2, 0.4, 0.8], 8);
        assert_eq!((fog.environmental_start, fog.environmental_end), (0.0, 1024.0));

        for &(d, expected) in &[
            (16.0, 0.015_625),
            (32.0, 0.031_25),
            (64.0, 0.0625),
            (96.0, 0.093_75),
            (115.2, 0.1125),
            (121.6, 0.5),
            (128.0, 1.0),
        ] {
            let p = along(EYE, LEVEL_X, d);
            let got = total_fog_factor(&fog, EYE, p);
            assert!(
                (got - expected).abs() < 1e-4,
                "d={d}: got {got:.6}, want {expected:.6}"
            );
        }

        // Negative control, executed and observed to fail: the pre-F2 model
        // (the render-distance term alone, via plain `fog_factor`) reads
        // exactly zero at every one of these distances short of 115.2 — the
        // "hard wall" this term exists to remove.
        for d in [16.0_f32, 32.0, 64.0, 96.0] {
            let p = along(EYE, LEVEL_X, d);
            let old = fog_factor(spherical(EYE, p), fog.start, fog.end);
            assert_eq!(old, 0.0, "control: pre-F2 fog at d={d} must read zero");
        }
    }

    /// **Gate C (F3), the "mundane hilltop" case** — an ordinary sightline,
    /// not a pinned extreme ray. Eye at `y = 140`, valley floor at `y = 64`,
    /// 110 blocks out horizontally, RD 8. This is the diagnosis's own worked
    /// example: spherical `sqrt(110² + 76²) = 133.7` fully fogs today
    /// (`fog_factor(133.7, 115.2, 128) = 1.0`); vanilla's cylindrical
    /// `max(110, 76) = 110` misses the render-distance band entirely
    /// (`< 115.2`, so that term is `0.0`) and vanilla's total is just the
    /// environmental term measured on the **spherical** distance,
    /// `133.7 / 1024 = 0.13057`.
    #[test]
    fn cylindrical_distance_uncovers_the_valley_below_an_ordinary_hilltop() {
        let fog = FogSettings::for_render_distance([0.2, 0.4, 0.8], 8);
        let eye = [0.0, 140.0, 0.0];
        let world = [110.0, 64.0, 0.0];

        let sph = spherical(eye, world);
        let cyl = cylindrical(eye, world);
        assert!((sph - 133.7).abs() < 0.05, "spherical {sph}");
        assert!((cyl - 110.0).abs() < 1e-4, "cylindrical {cyl}");

        let vanilla_env = fog_factor(sph, 0.0, 1024.0);
        let vanilla_rd = fog_factor(cyl, fog.start, fog.end);
        let vanilla = vanilla_env.max(vanilla_rd);
        assert!((vanilla_env - 0.13057).abs() < 1e-3, "vanilla env term {vanilla_env}");
        assert_eq!(vanilla_rd, 0.0, "cylindrical distance misses the render-distance band");
        assert!((vanilla - 0.13057).abs() < 1e-3, "vanilla total {vanilla}");

        let ours = total_fog_factor(&fog, eye, world);
        assert!((ours - vanilla).abs() < 1e-4, "ours {ours} must match vanilla {vanilla}");

        // Negative control, executed and observed to fail: the pre-fix
        // spherical single-term model erases the valley completely.
        let old = fog_factor(sph, fog.start, fog.end);
        assert_eq!(old, 1.0, "control: the old model must be fully fogged here");
        assert!(
            old - vanilla > 0.85,
            "control must clearly overshoot vanilla: got {old}, vanilla {vanilla}"
        );
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

    /// The world shaders' fog mix, as a **magnitude** test rather than a
    /// direction one — the species `CLAUDE.md` records as the subtlest, where
    /// everything about the gate is right except that its predicate is satisfied
    /// by both hypotheses.
    ///
    /// Both numbers below are computed by hand from the sRGB transfer function,
    /// not by calling anything in this module: a fragment at sRGB `0.3` against a
    /// fog at sRGB `0.75`.
    ///
    /// | factor | gamma mix (correct) | linear mix (the bug) |
    /// |---|---|---|
    /// | 0.25 | sRGB `0.4125` → linear `0.141799` | linear `0.185560` |
    /// | 0.50 | sRGB `0.5250` → linear `0.237916` | linear `0.297880` |
    ///
    /// The measurement has to land on the first column *and miss the second*.
    /// Asserting only "the fogged value is between the fragment and the fog"
    /// passes under either, which is how the linear mix survived a fog gate, an
    /// entity fog gate and a ramp gate.
    #[test]
    fn the_world_fog_mix_is_in_gamma_space_at_a_predicted_magnitude() {
        // The two endpoints, stated in sRGB and converted here so the linear
        // inputs below are derived from the sRGB literals rather than guessed.
        let frag_srgb = 0.3_f32;
        let fog_srgb = 0.75_f32;
        let frag = [srgb_to_linear_f32(frag_srgb); 3];
        let fog = [srgb_to_linear_f32(fog_srgb); 3];

        for (factor, correct, bug) in [
            (0.25_f32, 0.141_799_f32, 0.185_560_f32),
            (0.50, 0.237_916, 0.297_880),
        ] {
            let got = apply_fog_gamma(frag, fog, factor)[0];
            assert!(
                (got - correct).abs() < 2e-3,
                "factor {factor}: gamma mix should be {correct:.6} linear, got {got:.6}"
            );
            // And it must be nowhere near the linear-space value, which is the
            // whole point: the two are 31% apart in linear light at 0.25.
            assert!(
                (got - bug).abs() > 0.02,
                "factor {factor}: {got:.6} is indistinguishable from the linear-mix \
                 value {bug:.6}, so this test cannot tell the two implementations apart"
            );
            // Control: the *linear* mix must land on the second column, proving
            // the two columns really are the two implementations and not two
            // arbitrary numbers.
            let linear = apply_fog(frag, fog, factor)[0];
            assert!(
                (linear - bug).abs() < 2e-3,
                "control failed: the linear mix should be {bug:.6}, got {linear:.6} — \
                 the hand-computed predictions above are wrong, not the code"
            );
        }

        // Both spaces agree exactly at the ends. This is not padding: it is why
        // `fog_gate.rs`, which measures a fully-fogged fragment, was blind to
        // the difference, and why the sky disc (still a linear mix) and terrain
        // do not band against each other at the horizon.
        for factor in [0.0_f32, 1.0] {
            let g = apply_fog_gamma(frag, fog, factor);
            let l = apply_fog(frag, fog, factor);
            assert!(
                (g[0] - l[0]).abs() < 1e-6,
                "the two spaces must agree at factor {factor}: {g:?} vs {l:?}"
            );
        }
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
        // `for_view_distance` leaves the environmental pair disabled (see its
        // own doc on why its range stays in the render-distance pair), so
        // `eye.w`/`end_enabled.w` must read as the degenerate 0.0/0.0 here.
        assert_eq!(on.eye, [1.0, 2.0, 3.0, 0.0]);
        assert_eq!(on.color_start[3], 50.0); // start
        assert_eq!(on.end_enabled[0], 100.0); // end
        assert_eq!(on.end_enabled[1], 1.0); // enabled
        assert_eq!(on.end_enabled[3], 0.0); // environmental_end

        let off = FogUniform::disabled();
        assert_eq!(off.end_enabled[1], 0.0);

        // A settings value with *only* the environmental pair populated (the
        // render-distance pair left degenerate) must still read enabled — the
        // control for the `||` in `FogUniform::new`'s enabled check.
        let env_only = FogUniform::new(
            &FogSettings {
                color: [0.1; 3],
                sky_color: [0.1; 3],
                start: 0.0,
                end: 0.0,
                environmental_start: 0.0,
                environmental_end: 1024.0,
            },
            [0.0; 3],
        );
        assert_eq!(env_only.eye[3], 0.0); // environmental_start
        assert_eq!(env_only.end_enabled[3], 1024.0); // environmental_end
        assert_eq!(env_only.end_enabled[1], 1.0, "environmental-only fog must still be enabled");
    }

    #[test]
    fn uniform_is_64_bytes_four_vec4s() {
        // Grew by one `vec4` (`ambient_light`) when the per-dimension
        // ambient-light colour was added — every original lane was already
        // spoken for, so this is a genuine size change, not a reused lane.
        // See `FogUniform::ambient_light`'s doc for why that does not cost a
        // bind group.
        assert_eq!(std::mem::size_of::<FogUniform>(), 64);
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
        // Vanilla: fixed 10..96 block *environmental* range (F2/F3),
        // `nether_wastes`' `0x330808` fog colour — red channel clearly
        // dominant, green/blue near black. The render-distance pair now
        // carries the real vanilla-exact edge fade instead of being
        // repurposed to hold this 10..96 range.
        let f = FogSettings::nether(32);
        assert_eq!(f.environmental_start, 10.0);
        assert_eq!(f.environmental_end, 96.0);
        // The render-distance pair is now the real vanilla-exact span for RD
        // 32 (`end = 512`, `span = clamp(51.2, 4, 64) = 51.2`), not the
        // Nether's own 10..96 range the pre-F2/F3 constructor repurposed it
        // to hold.
        assert_eq!(f.end, 512.0);
        assert_eq!(f.start, 460.8);
        assert!(f.color[0] > f.color[1] * 4.0, "expected red-dominant fog");
        assert!(f.color[0] > 0.0 && f.color[0] < 1.0);

        // A render distance shorter than the vanilla range must not fog past
        // the loaded world, exactly like `for_view_distance`'s `end` clamp.
        let short = FogSettings::nether(2); // 32 blocks
        assert_eq!(short.environmental_end, 32.0);
        assert_eq!(short.environmental_start, 10.0);

        // And a render distance so short even the *start* would fall outside
        // the loaded world must not produce start > end (a degenerate,
        // fog-disabling range would silently turn off the Nether's own thick
        // fog entirely).
        let tiny = FogSettings::nether(0);
        assert_eq!(tiny.environmental_end, 0.0);
        assert_eq!(tiny.environmental_start, 0.0);
        assert!(tiny.environmental_start <= tiny.environmental_end);
    }

    #[test]
    fn the_end_fog_is_a_flat_near_black_edge_fade() {
        let f = FogSettings::the_end(16, 0.75);
        assert_eq!(f.end, 256.0);
        // Vanilla's span, not the caller's fraction: `256 - clamp(25.6, 4, 64)`.
        // The End declares no `visual/fog_start_distance`, so its fog *is* the
        // render-distance term and must fade on the overworld's curve.
        assert_eq!(f.start, 230.4);
        // The fraction is only a floor, so a caller demanding a *later* onset
        // than vanilla's span still gets it.
        assert_eq!(FogSettings::the_end(16, 0.95).start, 243.2);
        // `0x181318` is dark and very slightly red/blue-leaning, but overall
        // near-black — nothing like the overworld's saturated sky blue.
        assert!(f.color.iter().all(|c| *c < 0.02), "expected near-black: {:?}", f.color);
        assert_eq!(f.color[0], f.color[2], "R and B channels match (#18__18)");

        // F2: the End's environmental term is the same registered default as
        // the overworld's, inherited from `for_render_distance` unchanged.
        assert_eq!(f.environmental_start, 0.0);
        assert_eq!(f.environmental_end, 1024.0);
    }

    /// The three anchors vanilla's expression fixes, in the direction it fixes
    /// them: no darkening at or above `min_y + onset_range`, total darkness at
    /// `min_y`, and **quadratic** (not linear) in between — halfway down the
    /// onset range is `0.25`, never `0.5`.
    #[test]
    fn void_fog_brightness_is_quadratic_and_darkens_downward() {
        let v = VoidFog::OVERWORLD;
        assert_eq!(v.brightness(-32.0), 1.0, "at min_y + onset_range: undarkened");
        assert_eq!(v.brightness(64.0), 1.0, "well above: undarkened");
        assert_eq!(v.brightness(-64.0), 0.0, "at min_y: black");
        assert_eq!(v.brightness(-96.0), 0.0, "below min_y: clamped, still black");
        // Halfway down (eye at -48, i.e. 16 above min_y): darkness 0.5,
        // brightness 0.25. A linear ramp would read 0.5 here.
        let mid = v.brightness(-48.0);
        assert!((mid - 0.25).abs() < 1e-5, "expected quadratic 0.25, got {mid}");
    }

    /// A flat world's void-darkness onset range is `1.0`, not `32.0`,
    /// so the same eye height that is fully
    /// undarkened on a superflat world is deep into the fade on a normal one.
    #[test]
    fn flat_worlds_have_a_one_block_void_onset() {
        let flat = VoidFog::for_level(-64.0, true);
        assert_eq!(flat.onset_range, 1.0);
        assert_eq!(flat.brightness(-63.0), 1.0);
        assert_eq!(flat.brightness(-64.0), 0.0);
        assert!(VoidFog::OVERWORLD.brightness(-63.0) < 0.01);
    }

    /// [`VoidFog::for_level`] is the only place the `1.0`/`32.0` fork lives,
    /// and this measures the difference where it is actually visible: the
    /// superflat oracle's own ground, `y = -57.4`, roughly 6.6 blocks above a
    /// `-64` world bottom.
    ///
    /// Both hypotheses are computed rather than one asserted as a direction —
    /// a flat level is fully lit there (`1.0`) and a non-flat one is all but
    /// black (`0.042`), a 24x difference, which is exactly the "any superflat
    /// world looks wrong" report.
    #[test]
    fn for_level_separates_the_two_hypotheses_at_the_superflat_oracles_own_ground() {
        let ground = -57.4;
        let flat = VoidFog::for_level(-64.0, true).brightness(ground);
        let normal = VoidFog::for_level(-64.0, false).brightness(ground);
        assert_eq!(flat, 1.0, "a flat level is undarkened at its own surface");
        assert!(
            (normal - 0.042).abs() < 0.001,
            "the non-flat reading at the same height is {normal}, want ~0.042"
        );
        // Non-flat matches the constant this used to be hardcoded to, so the
        // change is provably a no-op for every ordinary world.
        assert_eq!(normal, VoidFog::OVERWORLD.brightness(ground));
    }

    /// `min_y` is the other half, and it was hardcoded to the overworld's
    /// `-64` for every dimension. The Nether and End start at `0`, so the fade
    /// belongs below `y = 32` there — at a Nether floor of `y = 10` the two
    /// readings are `0.098` and a completely undarkened `1.0`.
    #[test]
    fn for_level_uses_the_dimensions_own_min_y_not_the_overworlds() {
        let nether = VoidFog::for_level(0.0, false).brightness(10.0);
        assert!(
            (nether - 0.098).abs() < 0.001,
            "the Nether reading at y=10 is {nether}, want ~0.098"
        );
        assert_eq!(
            VoidFog::OVERWORLD.brightness(10.0),
            1.0,
            "the old hardcoded min_y suppressed the Nether's void fog entirely"
        );
    }

    #[test]
    fn disabled_void_fog_never_darkens_and_never_divides_by_zero() {
        for y in [-1024.0, -64.0, 0.0, 320.0] {
            assert_eq!(VoidFog::DISABLED.brightness(y), 1.0);
        }
    }

    /// The gamma-space scale is the whole point of `scale_gamma`: scaling in
    /// linear space is *measurably* brighter, which is the washed-out failure
    /// `CLAUDE.md` records. This pins the gap rather than describing it.
    #[test]
    fn scale_gamma_is_darker_than_a_linear_multiply() {
        let sky = [0.242_867, 0.462_361, 0.827_571]; // gpu::SKY_COLOR
        let gamma = scale_gamma(sky, 0.5);
        for (i, c) in gamma.iter().enumerate() {
            let naive = sky[i] * 0.5;
            assert!(
                *c < naive,
                "channel {i}: gamma-space {c} should be darker than linear {naive}"
            );
        }
        // 0.5 in gamma space is ~0.2159 in linear (the textbook sRGB mid-grey),
        // so a mid-grey scaled by 0.5 lands near 0.2159 * ... — check the
        // round-trip identity instead, which is exact and version-free.
        assert_eq!(scale_gamma(sky, 1.0), sky.map(|c| srgb_to_linear_f32(linear_to_srgb_f32(c))));
        assert_eq!(scale_gamma(sky, 0.0), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn float_transfer_functions_round_trip_and_match_the_u8_version() {
        for byte in [0u8, 1, 10, 0x80, 200, 255] {
            let via_u8 = srgb_u8_to_linear([byte, byte, byte])[0];
            let via_f32 = srgb_to_linear_f32(f32::from(byte) / 255.0);
            assert!((via_u8 - via_f32).abs() < 1e-6, "byte {byte}: {via_u8} vs {via_f32}");
        }
        for linear in [0.0f32, 0.001, 0.2159, 0.5, 1.0] {
            let back = srgb_to_linear_f32(linear_to_srgb_f32(linear));
            assert!((back - linear).abs() < 1e-5, "{linear} -> {back}");
        }
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
