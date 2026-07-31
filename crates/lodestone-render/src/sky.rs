//! Sky-dome geometry and time-of-day math: the sky disc, sun/moon billboards,
//! star field and cloud plane. GPU-free and headlessly testable by design (see
//! [`crate::sky_pipeline`] for the GPU-owning half).
//!
//! # This subsystem did not exist before this change
//!
//! A tree-wide grep for sky/celestial/star/moon/cloud rendering before writing
//! any of this turned up only *sky light* (`world.rs`'s `sky_light` nibble) and
//! *fog* (`fog.rs`'s distance-fade colour) — real, wired subsystems that happen
//! to share the word "sky", not a dormant celestial renderer. There is no
//! island to reconnect here: the night sky is a genuinely new subsystem, built
//! from nothing.
//!
//! # Reuse, not duplication, of the day clock
//!
//! Every time-varying function here takes the **same** `time_of_day: i64` the
//! rest of the renderer already reads from `WorldTime`
//! (`docs/time-of-day-lighting.md`) — there is deliberately no second clock,
//! no tick counter, and no wall-clock read anywhere in this module.
//!
//! # Two formulas are intentionally duplicated from `entity.rs`, not imported
//!
//! [`celestial_angle_for_time_of_day`] and the private `sky_darken_shape` this
//! module also needs are the *same* vanilla `celestialAngle`/`getSkyDarken`
//! math [`crate::entity::sky_darken_for_time_of_day`] already computes as a
//! private intermediate — but `entity.rs` is a held file outside this change's
//! scope, so this is a second, independently-written copy rather than an
//! import. If either formula changes, the other copy must change with it, or
//! the sun's screen position and the lightmap's darken factor will visibly
//! disagree about what time it is.
//!
//! # Known divergence (#49): do not trust the dusk/dawn ramp as vanilla-exact
//!
//! 26.2 replaced the classic cosine `celestialAngle`/`getSkyDarken` with a
//! keyframed `EnvironmentAttributes` track (`SUN_ANGLE`, `MOON_ANGLE`,
//! `STAR_ANGLE`, `STAR_BRIGHTNESS`, `SKY_COLOR`, `MOON_PHASE` — see
//! `.cache/mc/26.2/client-src/net/minecraft/client/renderer/SkyRenderer.java`
//! `extractRenderState`). This module ports the *classic* pre-keyframe formulas
//! instead (the same ones `entity.rs`'s validated port already uses for
//! `sky_darken`), which match both plateaus but not necessarily the exact
//! dusk/dawn ramp shape. [`sky_color_for_time_of_day`] goes further and is not
//! a port of anything — `SKY_COLOR` is a biome-blended keyframe track with no
//! classic-era equivalent to port, so it is a clearly-labelled approximation.

use glam::{Mat4, Vec3};

// ---------------------------------------------------------------------------
// Time-of-day math
// ---------------------------------------------------------------------------

/// The fraction of a full day/night rotation completed, in `[0, 1)` — vanilla's
/// `celestialAngle`. `0.0` is noon, `0.5` is midnight (verified against the
/// same two anchor points `entity.rs`'s `sky_darken_for_time_of_day` test
/// asserts: this function returns `0.0` at `time_of_day = 6_000` and `0.5` at
/// `time_of_day = 18_000`).
#[must_use]
pub fn celestial_angle_for_time_of_day(time_of_day: i64) -> f32 {
    let day = time_of_day.rem_euclid(24_000) as f64 / 24_000.0;
    let frac = (day - 0.25).rem_euclid(1.0);
    let eased = 0.5 - (frac * std::f64::consts::PI).cos() / 2.0;
    ((frac * 2.0 + eased) / 3.0) as f32
}

/// `Level::getSkyDarken`'s shape (see module docs on why this is a second copy
/// of `entity.rs`'s private intermediate): `0.2` at midnight-plateau, `1.0` at
/// noon-plateau. Used here only as the day/night blend driving
/// [`sky_color_for_time_of_day`] — *not* re-exported as a lightmap factor,
/// which remains solely `entity.rs::sky_darken_for_time_of_day`'s job.
fn sky_darken_shape(time_of_day: i64) -> f32 {
    let celestial = celestial_angle_for_time_of_day(time_of_day);
    let mut f = 1.0 - ((celestial * std::f32::consts::TAU).cos() * 2.0 + 0.2);
    f = f.clamp(0.0, 1.0);
    f = 1.0 - f;
    f * 0.8 + 0.2
}

/// An approximate day/night sky-dome colour. **Not** a port of vanilla's
/// `EnvironmentAttributes.SKY_COLOR` keyframe track (biome-blended and
/// version-specific — see the module docs on the #49 divergence, which this
/// goes beyond since there is no classic-era formula to port at all here).
///
/// Blends `day_color` (pass the renderer's existing clear/fog sky colour, so
/// noon is visually unchanged from today) toward a fixed dark-navy night
/// colour, driven by [`sky_darken_shape`] — the same day clock every other
/// time-varying function in this module reads, not a second signal.
#[must_use]
pub fn sky_color_for_time_of_day(time_of_day: i64, day_color: [f32; 3]) -> [f32; 3] {
    const NIGHT: [f32; 3] = [0.006, 0.008, 0.02];
    let darken = sky_darken_shape(time_of_day);
    let t = ((darken - 0.2) / 0.8).clamp(0.0, 1.0);
    [
        NIGHT[0] + (day_color[0] - NIGHT[0]) * t,
        NIGHT[1] + (day_color[1] - NIGHT[1]) * t,
        NIGHT[2] + (day_color[2] - NIGHT[2]) * t,
    ]
}

/// Vanilla's legacy `getStarBrightness`: `0.0` for most of the day, ramping up
/// around dusk to a `0.5` plateau at night. Ported literally (not re-derived
/// from [`sky_darken_shape`]'s different constants) since it is vanilla's own
/// distinct formula.
#[must_use]
pub fn star_brightness_for_time_of_day(time_of_day: i64) -> f32 {
    let angle = celestial_angle_for_time_of_day(time_of_day);
    let mut f = 1.0 - ((angle * std::f32::consts::TAU).cos() * 2.0 + 0.25);
    f = f.clamp(0.0, 1.0);
    f * f * 0.5
}

/// The active moon phase, `0..8`, in [`lodestone_assets::MOON_PHASE_NAMES`]
/// order. Vanilla's `MoonPhase.startTick() == index() * 24000`
/// (`.cache/mc/26.2/client-src/net/minecraft/world/level/MoonPhase.java`) fixes
/// the mapping: the phase active on world-day `d` (`d = time_of_day / 24000`)
/// is enum index `d % 8`. This is a per-day integer cycle, not a continuous
/// keyframe track, so unlike the ramp-shaped formulas above it is not subject
/// to the #49 divergence.
#[must_use]
pub fn moon_phase_index_for_time_of_day(time_of_day: i64) -> u8 {
    time_of_day.div_euclid(24_000).rem_euclid(8) as u8
}

// ---------------------------------------------------------------------------
// Sky disc
// ---------------------------------------------------------------------------

/// Sky-disc radius in blocks (vanilla `SkyRenderer.SKY_DISC_RADIUS`).
pub const SKY_DISC_RADIUS: f32 = 512.0;

/// Camera-relative sky-disc triangle-fan positions: the centre plus 9
/// perimeter points across `-180..=180` degrees in 45-degree steps (vanilla
/// `SkyRenderer.buildSkyDisc`, `SKY_VERTICES = 10`). Pass a positive `y` for
/// the overhead sky disc, negative for the below-horizon "dark disc".
#[must_use]
pub fn sky_disc_positions(y: f32) -> [[f32; 3]; 10] {
    let x_radius = y.signum() * SKY_DISC_RADIUS;
    let mut out = [[0.0f32; 3]; 10];
    out[0] = [0.0, y, 0.0];
    for (i, deg) in (-180..=180i32).step_by(45).enumerate() {
        let rad = (deg as f32).to_radians();
        out[i + 1] = [x_radius * rad.cos(), y, SKY_DISC_RADIUS * rad.sin()];
    }
    out
}

/// Triangle-fan indices for [`sky_disc_positions`]'s 10 vertices (8 triangles,
/// all sharing vertex `0`).
#[must_use]
pub fn sky_disc_indices() -> Vec<u32> {
    let mut idx = Vec::with_capacity(8 * 3);
    for i in 1..9u32 {
        idx.push(0);
        idx.push(i);
        idx.push(i + 1);
    }
    idx
}

// ---------------------------------------------------------------------------
// Sun / moon
// ---------------------------------------------------------------------------

/// Sun billboard half-size in blocks (vanilla `SkyRenderer.SUN_SIZE`).
pub const SUN_SIZE: f32 = 30.0;
/// Sun billboard distance in blocks (vanilla `SkyRenderer.SUN_HEIGHT`).
pub const SUN_HEIGHT: f32 = 100.0;
/// Moon billboard half-size in blocks (vanilla `SkyRenderer.MOON_SIZE`).
pub const MOON_SIZE: f32 = 20.0;
/// Moon billboard distance in blocks (vanilla `SkyRenderer.MOON_HEIGHT`).
pub const MOON_HEIGHT: f32 = 100.0;

/// The rotation vanilla's pose stack accumulates before placing a celestial
/// billboard or the star field: a fixed `-90`-degree turn about `+Y` (so the
/// unrotated quad's "up" axis becomes the sky's zenith), then the time-varying
/// rotation about `+X` by `angle_rad`. Composing sun/moon/star geometry through
/// this single helper is what keeps all three agreeing on which way is "up" in
/// the sky.
#[must_use]
pub fn celestial_rotation_matrix(angle_rad: f32) -> Mat4 {
    Mat4::from_rotation_y(-90f32.to_radians()) * Mat4::from_rotation_x(angle_rad)
}

/// Camera-relative positions of a celestial billboard quad (sun or moon),
/// vertex order matching vanilla's `buildCelestialQuad`/`buildMoonPhases`:
/// `(-1,-1), (1,-1), (1,1), (-1,1)` in the quad's own `(u, v)` before the
/// transform. Composes [`celestial_rotation_matrix`] with the translate-then-
/// scale vanilla applies per billboard (`translate(0, height, 0)` then
/// `scale(size, 1, size)`).
#[must_use]
pub fn celestial_quad_positions(angle_rad: f32, height: f32, size: f32) -> [[f32; 3]; 4] {
    let m = celestial_rotation_matrix(angle_rad)
        * Mat4::from_translation(Vec3::new(0.0, height, 0.0))
        * Mat4::from_scale(Vec3::new(size, 1.0, size));
    let local = [
        Vec3::new(-1.0, 0.0, -1.0),
        Vec3::new(1.0, 0.0, -1.0),
        Vec3::new(1.0, 0.0, 1.0),
        Vec3::new(-1.0, 0.0, 1.0),
    ];
    local.map(|p| m.transform_point3(p).to_array())
}

/// UVs for a celestial quad's 4 corners (same winding as
/// [`celestial_quad_positions`]) into atlas rect `[u0, v0, u1, v1]`.
///
/// `mirrored` swaps the rect corners the way vanilla's `buildMoonPhases` does
/// relative to `buildSunQuad` (the moon texture is authored mirrored relative
/// to the sun): sun corner 0 samples `(u0, v0)`, moon corner 0 samples
/// `(u1, v1)`.
#[must_use]
pub fn celestial_quad_uvs(rect: [f32; 4], mirrored: bool) -> [[f32; 2]; 4] {
    let [u0, v0, u1, v1] = rect;
    if mirrored {
        [[u1, v1], [u0, v1], [u0, v0], [u1, v0]]
    } else {
        [[u0, v0], [u1, v0], [u1, v1], [u0, v1]]
    }
}

/// Two triangles over a quad's 4 vertices (`0,1,2,2,3,0`), for any of the quad
/// builders in this module.
#[must_use]
pub const fn quad_indices() -> [u32; 6] {
    [0, 1, 2, 2, 3, 0]
}

// ---------------------------------------------------------------------------
// Star field
// ---------------------------------------------------------------------------

/// Star count (vanilla `SkyRenderer.STAR_COUNT`). The *iteration* count, not
/// the guaranteed output count — see [`build_star_field`].
pub const STAR_COUNT: usize = 1500;
/// Star field distance in blocks (vanilla `SkyRenderer`'s star `starDistance`).
pub const STAR_DISTANCE: f32 = 100.0;
/// The seed this module uses for its own deterministic star field. **Not**
/// vanilla's seed in any meaningful sense — see [`build_star_field`]'s docs.
pub const STAR_FIELD_SEED: u64 = 10842;

/// A small, dependency-free splitmix64 PRNG, seeded once.
///
/// Deliberately not vanilla's Java `RandomSource`: reproducing that generator
/// bit-for-bit is not attempted here (the star field is a visual feature, not
/// a decode-parity gate — nothing anywhere compares it against captured server
/// bytes). This generator is chosen only for being small, dependency-free, and
/// *platform-and-run deterministic*, which is what makes [`build_star_field`]
/// reproducible and testable at all; the resulting star positions will not
/// match a real vanilla client's.
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform `f32` in `[0, 1)`, from the top 24 bits (full `f32` mantissa
    /// precision, no rounding bias toward either end).
    fn next_f32(&mut self) -> f32 {
        const TWO_POW_24: f32 = 16_777_216.0;
        ((self.next_u64() >> 40) as f32) / TWO_POW_24
    }
}

/// Builds the star field's quads (unrotated — apply [`celestial_rotation_matrix`]
/// per frame to place them, matching how vanilla rotates the static star
/// buffer by `starAngle` rather than rebuilding it).
///
/// Ports vanilla's `SkyRenderer.buildStars` algorithm: reject-sample a point in
/// the shell `0.1 <= |p| < 1.0` inside the unit cube, place a billboard quad of
/// random size at that point normalized to [`STAR_DISTANCE`], oriented to face
/// outward (tangent to the sphere) with a random in-plane rotation. [`STAR_COUNT`]
/// is the *iteration* count exactly as in vanilla — rejected samples are
/// skipped, not retried, so the returned `Vec` is shorter than `STAR_COUNT`
/// (vanilla's own `starIndexCount` is likewise runtime-derived, not a
/// constant, for the same reason).
///
/// See the struct docs on [`SplitMix64`] for why this does not reproduce
/// vanilla's exact star positions, only its distribution shape.
#[must_use]
pub fn build_star_field(seed: u64) -> Vec<[[f32; 3]; 4]> {
    let mut rng = SplitMix64::new(seed);
    let mut quads = Vec::with_capacity(STAR_COUNT);
    for _ in 0..STAR_COUNT {
        let x = rng.next_f32() * 2.0 - 1.0;
        let y = rng.next_f32() * 2.0 - 1.0;
        let z = rng.next_f32() * 2.0 - 1.0;
        let star_size = 0.15 + rng.next_f32() * 0.1;
        let len_sq = x * x + y * y + z * z;
        if len_sq <= 0.010_000_001 || len_sq >= 1.0 {
            continue;
        }
        let dir = Vec3::new(x, y, z).normalize();
        let center = dir * STAR_DISTANCE;
        let z_rot = rng.next_f32() * std::f32::consts::TAU;

        // An orthonormal basis in the plane perpendicular to `dir`, so the
        // quad is tangent to the star sphere at `center`.
        let up_hint = if dir.y.abs() > 0.99 {
            Vec3::X
        } else {
            Vec3::Y
        };
        let tangent = dir.cross(up_hint).normalize();
        let bitangent = dir.cross(tangent);
        let (s, c) = z_rot.sin_cos();
        let u = tangent * c + bitangent * s;
        let v = tangent * -s + bitangent * c;

        let corner = |su: f32, sv: f32| (center + u * (su * star_size) + v * (sv * star_size)).to_array();
        quads.push([
            corner(1.0, -1.0),
            corner(1.0, 1.0),
            corner(-1.0, 1.0),
            corner(-1.0, -1.0),
        ]);
    }
    quads
}

// ---------------------------------------------------------------------------
// Clouds
// ---------------------------------------------------------------------------

/// Cloud plane height in blocks (vanilla's cloud `bottomY`, from
/// `LevelRenderer`/dimension-type default).
pub const CLOUD_HEIGHT: f32 = 192.0;
/// Blocks per cloud-texture cell (vanilla `CloudRenderer.CELL_SIZE_IN_BLOCKS`).
pub const CLOUD_CELL_BLOCKS: f32 = 12.0;
/// Scroll speed in blocks per tick (vanilla `CloudRenderer.BLOCKS_PER_SECOND`
/// applied per-tick as `0.03` — the renderer's own comment there literally
/// reads `0.030000001F`, i.e. this constant).
pub const CLOUD_SCROLL_BLOCKS_PER_TICK: f32 = 0.030_000_001;

/// One large, alpha-tested, camera-centred quad covering `half_extent` blocks
/// in every direction, sampled with a wrapping (`Repeat`-address-mode) sampler
/// across the *whole* `clouds.png`.
///
/// # A deliberate simplification vs. vanilla's fancy clouds
///
/// Vanilla voxelizes `clouds.png` into a 3D cell grid and extrudes visible
/// faces (`CloudRenderer.buildMesh`/`buildExtrudedCell`) for its "fancy" cloud
/// mode. This instead reproduces only the flatter "fast" mode: a single quad
/// whose fragment shader alpha-tests against the same texture (a transparent
/// texel is an empty cell — `CloudRenderer.isCellEmpty`'s `alpha < 10` check —
/// so per-pixel alpha-testing this texture on a flat quad reproduces "which
/// cells are filled" with no CPU-side cell meshing at all). No cell extrusion,
/// no top/side faces, no "inside the cloud layer" cross-section. Chosen for
/// scope: implementing the full voxel mesher is a second renderer's worth of
/// work for a decoration layer, and "flat clouds" is itself a real, selectable
/// vanilla mode, not an invented approximation.
///
/// Returns `(camera-relative positions, uvs)`, both wound `0,1,2,2,3,0`-ready
/// (see [`quad_indices`]).
#[must_use]
pub fn cloud_plane_geometry(
    camera_pos: [f32; 3],
    time_of_day: i64,
    texture_width_texels: u32,
    texture_height_texels: u32,
    half_extent: f32,
) -> ([[f32; 3]; 4], [[f32; 2]; 4]) {
    let local_y = CLOUD_HEIGHT - camera_pos[1];
    let positions = [
        [-half_extent, local_y, -half_extent],
        [half_extent, local_y, -half_extent],
        [half_extent, local_y, half_extent],
        [-half_extent, local_y, half_extent],
    ];

    let scroll_x = time_of_day as f32 * CLOUD_SCROLL_BLOCKS_PER_TICK;
    let tex_w_blocks = CLOUD_CELL_BLOCKS * texture_width_texels.max(1) as f32;
    let tex_h_blocks = CLOUD_CELL_BLOCKS * texture_height_texels.max(1) as f32;
    let uvs = positions.map(|p| {
        let world_x = camera_pos[0] + p[0] + scroll_x;
        let world_z = camera_pos[2] + p[2];
        [world_x / tex_w_blocks, world_z / tex_h_blocks]
    });

    (positions, uvs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn celestial_angle_matches_the_validated_noon_and_midnight_anchors() {
        // Same anchors `entity.rs::sky_darken_for_time_of_day`'s own tests use,
        // so a drift between the two duplicated formulas would be caught here.
        assert!((celestial_angle_for_time_of_day(6_000) - 0.0).abs() < 1e-5);
        assert!((celestial_angle_for_time_of_day(18_000) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn celestial_angle_reduces_a_large_world_age_into_the_day() {
        assert_eq!(
            celestial_angle_for_time_of_day(18_000),
            celestial_angle_for_time_of_day(18_000 + 24_000 * 500)
        );
        assert_eq!(
            celestial_angle_for_time_of_day(6_000),
            celestial_angle_for_time_of_day(6_000 - 24_000 * 500)
        );
    }

    #[test]
    fn sky_color_is_bright_at_noon_and_dark_at_midnight() {
        let day = [0.25, 0.46, 0.83];
        let noon = sky_color_for_time_of_day(6_000, day);
        let midnight = sky_color_for_time_of_day(18_000, day);
        assert!((noon[2] - day[2]).abs() < 1e-3, "noon should read as day_color: {noon:?}");
        assert!(
            midnight[2] < day[2] * 0.1,
            "midnight should be much darker than day: {midnight:?}"
        );
    }

    #[test]
    fn star_brightness_is_zero_at_noon_and_positive_at_midnight() {
        assert_eq!(star_brightness_for_time_of_day(6_000), 0.0);
        assert!(star_brightness_for_time_of_day(18_000) > 0.0);
    }

    /// `MoonPhase.startTick() == index * 24000` fixes phase `n` to world-day
    /// `n`; day 8 must wrap back to phase 0, not overflow or misalign.
    #[test]
    fn moon_phase_cycles_every_eight_days() {
        assert_eq!(moon_phase_index_for_time_of_day(0), 0);
        assert_eq!(moon_phase_index_for_time_of_day(24_000), 1);
        assert_eq!(moon_phase_index_for_time_of_day(24_000 * 7), 7);
        assert_eq!(moon_phase_index_for_time_of_day(24_000 * 8), 0);
        assert_eq!(moon_phase_index_for_time_of_day(24_000 * 8 + 12_000), 0);
    }

    #[test]
    fn sky_disc_has_ten_vertices_closing_the_fan() {
        let disc = sky_disc_positions(16.0);
        assert_eq!(disc[0], [0.0, 16.0, 0.0]);
        // -180 and +180 degrees are the same point on the circle, so the
        // first and last perimeter vertices must coincide (closing the fan).
        let first_perimeter = disc[1];
        let last_perimeter = disc[9];
        assert!(
            (first_perimeter[0] - last_perimeter[0]).abs() < 1e-3
                && (first_perimeter[2] - last_perimeter[2]).abs() < 1e-3,
            "fan does not close: {first_perimeter:?} vs {last_perimeter:?}"
        );
    }

    #[test]
    fn dark_disc_uses_the_opposite_radius_sign() {
        // y < 0 flips the disc's x radius (vanilla: `Math.signum(yy) * 512`).
        let top = sky_disc_positions(16.0);
        let bottom = sky_disc_positions(-16.0);
        assert_eq!(top[1][0], -bottom[1][0]);
    }

    #[test]
    fn sky_disc_indices_form_eight_triangles_sharing_the_centre() {
        let idx = sky_disc_indices();
        assert_eq!(idx.len(), 24);
        assert!(idx.chunks(3).all(|tri| tri[0] == 0));
    }

    #[test]
    fn celestial_quad_is_centred_at_height_scaled_by_size() {
        let quad = celestial_quad_positions(0.0, SUN_HEIGHT, SUN_SIZE);
        let center = quad.iter().fold([0.0f32; 3], |acc, p| {
            [acc[0] + p[0] / 4.0, acc[1] + p[1] / 4.0, acc[2] + p[2] / 4.0]
        });
        // At angle 0 the -90-degree Y rotation puts the billboard's "up"
        // (local +Z translate) along world +X; regardless of axis, its
        // distance from the origin must equal SUN_HEIGHT and every corner
        // must be `SUN_SIZE` from that centre in the billboard plane.
        let dist = (center[0] * center[0] + center[1] * center[1] + center[2] * center[2]).sqrt();
        assert!((dist - SUN_HEIGHT).abs() < 1e-2, "centre at wrong distance: {center:?}");
        for p in quad {
            let r = ((p[0] - center[0]).powi(2)
                + (p[1] - center[1]).powi(2)
                + (p[2] - center[2]).powi(2))
            .sqrt();
            assert!((r - SUN_SIZE * std::f32::consts::SQRT_2).abs() < 1e-2, "{r}");
        }
    }

    #[test]
    fn celestial_quad_moves_as_angle_advances() {
        let a = celestial_quad_positions(0.0, MOON_HEIGHT, MOON_SIZE);
        let b = celestial_quad_positions(std::f32::consts::FRAC_PI_2, MOON_HEIGHT, MOON_SIZE);
        assert_ne!(a, b);
    }

    #[test]
    fn sun_and_moon_uvs_are_mirrored() {
        let rect = [0.0, 0.0, 0.5, 0.5];
        let sun = celestial_quad_uvs(rect, false);
        let moon = celestial_quad_uvs(rect, true);
        assert_eq!(sun[0], [0.0, 0.0]);
        assert_eq!(moon[0], [0.5, 0.5]);
    }

    #[test]
    fn star_field_is_shorter_than_the_iteration_count_but_substantial() {
        let stars = build_star_field(STAR_FIELD_SEED);
        assert!(!stars.is_empty());
        assert!(
            stars.len() < STAR_COUNT,
            "expected some rejection-sampled stars to be skipped, got {} of {}",
            stars.len(),
            STAR_COUNT
        );
        // The rejection shell keeps roughly (volume of unit ball) / (volume of
        // the enclosing cube) of samples, order ~50%; a sanity band rather
        // than an exact figure since this generator is not vanilla's.
        assert!(stars.len() > STAR_COUNT / 4, "too few stars: {}", stars.len());
    }

    /// Every star quad's centre must be very close to [`STAR_DISTANCE`] from
    /// the origin (a bug in the tangent-basis construction could shrink or
    /// blow up the placement instead of just rotating it).
    #[test]
    fn every_star_sits_at_the_configured_distance() {
        for quad in build_star_field(STAR_FIELD_SEED) {
            let center = [
                quad.iter().map(|p| p[0]).sum::<f32>() / 4.0,
                quad.iter().map(|p| p[1]).sum::<f32>() / 4.0,
                quad.iter().map(|p| p[2]).sum::<f32>() / 4.0,
            ];
            let dist = (center[0] * center[0] + center[1] * center[1] + center[2] * center[2]).sqrt();
            assert!(
                (dist - STAR_DISTANCE).abs() < 1.0,
                "star centre {center:?} at distance {dist}, expected ~{STAR_DISTANCE}"
            );
        }
    }

    #[test]
    fn same_seed_is_fully_deterministic() {
        assert_eq!(build_star_field(STAR_FIELD_SEED), build_star_field(STAR_FIELD_SEED));
    }

    #[test]
    fn different_seeds_produce_different_fields() {
        assert_ne!(build_star_field(1), build_star_field(2));
    }

    #[test]
    fn cloud_plane_is_centred_on_the_camera_horizontally() {
        let (positions, _) = cloud_plane_geometry([100.0, 70.0, -40.0], 0, 256, 256, 512.0);
        for p in positions {
            assert!((p[0].abs() - 512.0).abs() < 1e-3);
            assert!((p[2].abs() - 512.0).abs() < 1e-3);
        }
    }

    #[test]
    fn cloud_plane_height_follows_the_camera() {
        let (low, _) = cloud_plane_geometry([0.0, 60.0, 0.0], 0, 256, 256, 512.0);
        let (high, _) = cloud_plane_geometry([0.0, 190.0, 0.0], 0, 256, 256, 512.0);
        assert!(low[0][1] > high[0][1], "cloud plane must drop as the camera rises toward it");
    }

    /// The scroll offset shifts the UV horizontally without touching the Z
    /// axis (vanilla only scrolls clouds along X).
    #[test]
    fn cloud_uvs_scroll_along_x_over_time() {
        let (_, uv0) = cloud_plane_geometry([0.0, 70.0, 0.0], 0, 256, 256, 512.0);
        let (_, uv1) = cloud_plane_geometry([0.0, 70.0, 0.0], 10_000, 256, 256, 512.0);
        assert_ne!(uv0[0][0], uv1[0][0]);
        assert!((uv0[0][1] - uv1[0][1]).abs() < 1e-6);
    }

    #[test]
    fn quad_indices_are_two_ccw_triangles() {
        assert_eq!(quad_indices(), [0, 1, 2, 2, 3, 0]);
    }
}
