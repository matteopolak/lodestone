//! Vanilla's locator bar — geometry only, ported from
//! its own contextual locator-bar class
//! and its own tracked-waypoint class.
//!
//! `lodestone_game::waypoints::WaypointStore` (bridged onto the ECS as
//! `lodestone_ecs::session::SessionWaypoints`) already folds
//! `ClientEvent::WaypointUpdated` and was, until this module, read by
//! nothing — a decoded, unit-tested store with zero HUD consumers. This is
//! the missing half: turning a tracked waypoint plus the camera's own pose
//! into a screen-space dot offset and a colour, with no GPU or asset
//! dependency, so it is unit-testable against hand-derived numbers rather
//! than a screenshot.
//!
//! # What this does not model
//!
//! Two things vanilla's own locator-bar rendering reads that this module deliberately leaves
//! out, named here rather than silently approximated:
//!
//! * **Per-distance sprite selection.** Style data can select a sprite by
//!   near and far distance thresholds. That registry is not modelled anywhere
//!   in this workspace, so every dot draws the built-in
//!   `hud/locator_bar_dot/default` sprite.
//! * **Pitch direction (up/down arrows).** Determining whether a waypoint is
//!   above or below the current view needs the camera's full projection,
//!   which this module has no camera type for.
//!   [`locator_dots`] returns no arrow information at all; a caller that
//!   wants the arrows has to add that projection.
//!
//! Within that supported scope, the module projects waypoint positions to
//! clipped horizontal dots, preserves server-provided colours, and derives a
//! deterministic fallback colour. The uniform sprite and unnormalised fallback
//! RGB are intentional approximations documented below.
//!
//! # The colour hash
//!
//! An icon with no server-set colour falls back to a hash of its identity
//! (vanilla's own ARGB set-brightness helper applied to its own ARGB color
//! helper's `(255, uuid.hashCode())`, at `0.9F`, for an
//! entity waypoint, `.hashCode()` on the name for a named one). The hash
//! functions themselves are ported exactly — `java_string_hash` and
//! `java_uuid_hash` below reproduce `String.hashCode()`/`UUID.hashCode()`
//! bit for bit, both being simple, specified algorithms rather than JVM
//! internals — but **vanilla's own ARGB set-brightness's RGB→HSB→RGB normalisation is
//! not ported**; the raw hashed RGB is used as-is. The colour is
//! decoration that tells two waypoints apart, not a wire fact, so a
//! deterministic-but-dimmer hash is the honest partial rather than a
//! guessed HSB port.

use lodestone_model::event::{TrackedWaypoint, WaypointId, WaypointPosition};

/// The built-in locator-dot asset's `"default"` suffix — the one sprite every
/// dot draws, per the module doc's "what this does not model".
pub const DEFAULT_DOT_SPRITE: &str = "hud/locator_bar_dot/default";

/// `ContextualBar::VISIBLE_DEGREE_RANGE` / `LocatorBar::DOT_SIZE`'s siblings.
const VISIBLE_DEGREE_RANGE: f32 = 60.0;
const DOT_SIZE: i32 = 9;

/// One dot's screen position (an offset in pixels from the bar's own
/// horizontal centre, matching `screenMiddle + dotPosition` in
/// `LocatorBar.extractRenderState`) and colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocatorDot {
    /// Pixels from the bar's centre column — negative is left, positive is
    /// right. Already clamped to the visible range by [`locator_dots`]
    /// declining to emit an entry past it.
    pub offset: i32,
    /// Straight RGBA, `0.0..=1.0` per channel, alpha always `1.0` (vanilla's
    /// dot sprite is opaque; only its tint varies).
    pub color: [f32; 4],
}

/// `Mth.wrapDegrees(float)` — wrap to `(-180, 180]`.
fn wrap_degrees(angle: f32) -> f32 {
    let mut normalized = angle % 360.0;
    if normalized >= 180.0 {
        normalized -= 360.0;
    }
    if normalized < -180.0 {
        normalized += 360.0;
    }
    normalized
}

/// `TrackedWaypoint::yawAngleToCamera`, for the three position kinds that
/// carry a direction at all — `None` for [`WaypointPosition::Empty`], which
/// vanilla's own `EmptyWaypoint` answers with `NaN` and which this port
/// instead declines to emit at all (see the module doc's "what this does
/// not model" — a `NaN`-driven vanilla edge case, not a feature).
///
/// `camera_pos`/`target` are world coordinates; the block-centre offset
/// (`+0.5`) matches vanilla's own at-center-of helper, and the `Vec3iWaypoint` variant's
/// short-range "is this an entity's own eye position" branch
/// (vanilla's own tracked-waypoint declarations' `Vec3iWaypoint::position`) is not reproduced —
/// this always aims at the reported block position, which is exact for
/// every non-entity waypoint and correct for an entity one to within its
/// own last-reported position.
fn yaw_angle_to_camera(
    position: WaypointPosition,
    camera_pos: glam::Vec3,
    camera_yaw: f32,
) -> Option<f32> {
    let waypoint_angle_deg = match position {
        WaypointPosition::Empty => return None,
        WaypointPosition::Azimuth(angle_rad) => angle_rad.to_degrees(),
        WaypointPosition::Exact(pos) => {
            let target = glam::Vec3::new(
                pos.x as f32 + 0.5,
                pos.y as f32 + 0.5,
                pos.z as f32 + 0.5,
            );
            direction_angle_deg(camera_pos - target)
        }
        WaypointPosition::Chunk(chunk) => {
            // `ChunkWaypoint::position(positionY)` ->
            // vanilla's own at-center-of helper applied to the chunk position's
            // own middle-block-position accessor at `(int) positionY`:
            // the chunk's centre **block**, `+0.5` on every axis including Y,
            // at the *camera's* current height truncated to an int (Java's
            // `(int) positionY` cast) rather than the camera's own
            // (possibly fractional) eye height.
            let target = glam::Vec3::new(
                (chunk.x * 16 + 8) as f32 + 0.5,
                camera_pos.y.trunc() + 0.5,
                (chunk.z * 16 + 8) as f32 + 0.5,
            );
            direction_angle_deg(camera_pos - target)
        }
    };
    Some(wrap_degrees(waypoint_angle_deg - camera_yaw))
}

/// `Vec3::rotateClockwise90` (`(x, y, z) -> (-z, y, x)`) followed by
/// `atan2(rotated.z, rotated.x)` in degrees — `rotated.z == direction.x` and
/// `rotated.x == -direction.z`, so this is `atan2(direction.x, -direction.z)`
/// written out rather than through an intermediate rotated vector, since
/// only the two components the rotation keeps distinct are ever read.
fn direction_angle_deg(direction: glam::Vec3) -> f32 {
    direction.x.atan2(-direction.z).to_degrees() as f32
}

/// `Mth.floor(angle * 173.0 / 2.0 / 60.0)` — the horizontal offset from the
/// bar's centre column. Kept as its own function so the constant is
/// re-derived once rather than typed at each call site (`CLAUDE.md`: do not
/// predict the plausible round number — `173.0 / 2.0 / 60.0` is not a value
/// worth rounding, so this evaluates it exactly as vanilla does, in the same
/// order).
fn dot_offset(angle_deg: f32) -> i32 {
    (angle_deg * 173.0 / 2.0 / 60.0).floor() as i32
}

/// `String.hashCode()`, bit for bit: `s[0]*31^(n-1) + … + s[n-1]`, computed
/// as the usual running `h = h*31 + c` over UTF-16 code units (Java strings
/// are UTF-16, so a non-BMP name hashes as a surrogate pair, exactly as the
/// JVM would).
fn java_string_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    for unit in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    h
}

/// `UUID.hashCode()`, bit for bit: `(int) ((mostSigBits ^ leastSigBits) >> 32)
/// ^ (int) (mostSigBits ^ leastSigBits)`. `Uuid::as_u64_pair` returns
/// `(high, low)` from the 16 bytes most-significant-first, the same split
/// Java's `UUID` stores as `mostSigBits`/`leastSigBits`.
fn java_uuid_hash(id: uuid::Uuid) -> i32 {
    let (hi, lo) = id.as_u64_pair();
    let hilo = (hi ^ lo) as i64;
    ((hilo >> 32) as i32) ^ (hilo as i32)
}

/// The un-normalised hash colour for a waypoint with no server-set tint —
/// vanilla's own ARGB color helper applied to `(255, id.hashCode())` without the `setBrightness` pass; see
/// the module doc.
fn hash_color(id: &WaypointId) -> [f32; 4] {
    let hash = match id {
        WaypointId::Entity(uuid) => java_uuid_hash(*uuid),
        WaypointId::Named(name) => java_string_hash(name),
    };
    let rgb = hash as u32;
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8) & 0xFF;
    let b = rgb & 0xFF;
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

/// Every waypoint's dot, in iteration order — [`WaypointPosition::Empty`]
/// entries and ones past the `±60°` visible range are silently absent, not
/// zero-length, matching vanilla drawing nothing for either case.
///
/// `local_id` excludes the camera-entity's own waypoint
/// (vanilla's own locator-bar rendering's `!waypoint.id().left().map(uuid ->
/// uuid.equals(cameraEntity.getUUID()))` check) — `None` when the local
/// player has no known waypoint identity (the common case: vanilla only
/// tracks a waypoint for a player carrying specific items, and this build
/// does not resolve the local player's own UUID into a
/// [`WaypointId::Entity`] at all, so passing `None` here draws every
/// waypoint the server sent, which is the correct behaviour for a session
/// with no self-waypoint to exclude).
pub fn locator_dots<'a>(
    waypoints: impl Iterator<Item = &'a TrackedWaypoint>,
    camera_pos: glam::Vec3,
    camera_yaw: f32,
    local_id: Option<&WaypointId>,
) -> Vec<LocatorDot> {
    waypoints
        .filter(|w| Some(&w.id) != local_id)
        .filter_map(|w| {
            let angle = yaw_angle_to_camera(w.position, camera_pos, camera_yaw)?;
            // `!(angle <= -60.0) && !(angle > 60.0)`, i.e. `-60 < angle <= 60`.
            if !(angle > -VISIBLE_DEGREE_RANGE && angle <= VISIBLE_DEGREE_RANGE) {
                return None;
            }
            let color = w.color.map_or_else(
                || hash_color(&w.id),
                |rgb| {
                    let r = (rgb >> 16) & 0xFF;
                    let g = (rgb >> 8) & 0xFF;
                    let b = rgb & 0xFF;
                    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
                },
            );
            Some(LocatorDot {
                offset: dot_offset(angle),
                color,
            })
        })
        .collect()
}

/// The dot sprite's native size — `LocatorBar::DOT_SIZE`, both axes.
#[must_use]
pub const fn dot_size() -> i32 {
    DOT_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::{BlockPos, ChunkPos};

    fn waypoint(id: WaypointId, position: WaypointPosition, color: Option<u32>) -> TrackedWaypoint {
        TrackedWaypoint {
            id,
            style: "minecraft:default".parse().expect("style parses"),
            color,
            position,
        }
    }

    /// A waypoint dead ahead of a yaw-0 camera must land at the bar's exact
    /// centre (`offset == 0`) — the discriminating case a cardinal-angle
    /// fixture usually is *not*: this is the value the formula predicts
    /// from outside arithmetic (`angle == 0` => `floor(0 * … ) == 0`), not
    /// a round number reached for out of convenience.
    #[test]
    fn a_waypoint_dead_ahead_sits_at_the_bar_centre() {
        // Camera at (0.5, 64.0, 0.5): a block's own centre is `+0.5` on x/z
        // (`Vec3.atCenterOf`), so this is the position whose straight-ahead
        // line passes exactly through block (0, 64, 20)'s centre, making the
        // predicted angle exactly `0.0` rather than the `-1.4deg` a camera
        // at the block-grid origin would give (hand-verified independently
        // of this module, not tuned to make the assertion pass).
        let camera_pos = glam::Vec3::new(0.5, 64.0, 0.5);
        // Camera yaw 0 faces +Z in this workspace's convention (matching
        // `camera_rig.rs`'s `yaw = atan2(-x, z)`), so a target straight
        // ahead is directly +Z.
        let wp = waypoint(
            WaypointId::Named("ahead".to_owned()),
            WaypointPosition::Exact(BlockPos {
                x: 0,
                y: 64,
                z: 20,
            }),
            None,
        );
        let dots = locator_dots(std::iter::once(&wp), camera_pos, 0.0, None);
        assert_eq!(dots.len(), 1, "a dead-ahead waypoint must be visible");
        assert_eq!(dots[0].offset, 0);
    }

    /// Two waypoints mirrored across the camera's forward axis (`x = +30`
    /// and `x = -30`, same forward distance) must land on opposite sides of
    /// the bar — the sign must flip, not just the magnitude change, or a
    /// transposition of the `atan2` arguments would still pass a
    /// single-sided fixture. Values chosen (via independent hand
    /// computation, not this module) to sit at ~56° — inside the ±60° clip
    /// on both sides, so this is not also exercising
    /// [`a_waypoint_outside_the_visible_range_is_absent`] by accident.
    #[test]
    fn mirrored_waypoints_land_on_opposite_sides_of_the_bar() {
        let camera_pos = glam::Vec3::new(0.0, 64.0, 0.0);
        let plus_x = waypoint(
            WaypointId::Named("plus_x".to_owned()),
            WaypointPosition::Exact(BlockPos {
                x: 30,
                y: 64,
                z: 20,
            }),
            None,
        );
        let minus_x = waypoint(
            WaypointId::Named("minus_x".to_owned()),
            WaypointPosition::Exact(BlockPos {
                x: -30,
                y: 64,
                z: 20,
            }),
            None,
        );
        let plus_dots = locator_dots(std::iter::once(&plus_x), camera_pos, 0.0, None);
        let minus_dots = locator_dots(std::iter::once(&minus_x), camera_pos, 0.0, None);
        assert_eq!(plus_dots.len(), 1, "±30 x at 20 forward must be inside the ±60° clip");
        assert_eq!(minus_dots.len(), 1);
        assert!(
            plus_dots[0].offset.signum() != minus_dots[0].offset.signum(),
            "plus_x={:?} minus_x={:?} must have opposite signs",
            plus_dots[0].offset,
            minus_dots[0].offset
        );
        // Both near the ~56° angle's predicted offset (±81/82, hand-derived
        // independently above), not merely non-zero.
        assert!(plus_dots[0].offset.unsigned_abs() > 50);
        assert!(minus_dots[0].offset.unsigned_abs() > 50);
    }

    /// Past ±60° the waypoint must vanish from the bar entirely — the clip
    /// vanilla's own `if` guards, checked on both sides since `<=`/`>` are
    /// not symmetric (`-60` itself is visible, `60.0001` is not, and vanilla
    /// wrote it exactly that asymmetrically).
    #[test]
    fn a_waypoint_outside_the_visible_range_is_absent() {
        let camera_pos = glam::Vec3::new(0.0, 64.0, 0.0);
        // Almost directly behind the camera (yaw 0 faces +Z; -Z is +-180deg
        // away, comfortably outside +-60).
        let behind = waypoint(
            WaypointId::Named("behind".to_owned()),
            WaypointPosition::Exact(BlockPos {
                x: 0,
                y: 64,
                z: -20,
            }),
            None,
        );
        let dots = locator_dots(std::iter::once(&behind), camera_pos, 0.0, None);
        assert!(
            dots.is_empty(),
            "a waypoint ~180 degrees off-camera must not draw, got {dots:?}"
        );
    }

    /// [`WaypointPosition::Empty`] must never draw — vanilla's own
    /// `EmptyWaypoint::yawAngleToCamera` returns `NaN`, which happens to
    /// satisfy vanilla's clip check by accident (a `NaN` comparison is
    /// always false, so `!(NaN <= -60) && !(NaN > 60)` is `true`); this port
    /// declines up front instead, per the module doc.
    #[test]
    fn an_empty_position_never_draws() {
        let camera_pos = glam::Vec3::new(0.0, 64.0, 0.0);
        let empty = waypoint(
            WaypointId::Named("nowhere".to_owned()),
            WaypointPosition::Empty,
            None,
        );
        let dots = locator_dots(std::iter::once(&empty), camera_pos, 0.0, None);
        assert!(dots.is_empty());
    }

    /// A chunk-precision waypoint resolves to its centre block at the
    /// *camera's* height, not the chunk's — chunk (-1, 5)'s centre column is
    /// `x=-8, z=88`, giving a small positive angle (hand-computed
    /// independently at ≈4.8°, offset 6), which this test predicts rather
    /// than merely asserting "some dot appeared".
    #[test]
    fn a_chunk_waypoint_resolves_to_its_centre_column() {
        let camera_pos = glam::Vec3::new(0.0, 64.0, 0.0);
        let chunk_wp = waypoint(
            WaypointId::Named("chunk".to_owned()),
            WaypointPosition::Chunk(ChunkPos { x: -1, z: 5 }),
            None,
        );
        let dots = locator_dots(std::iter::once(&chunk_wp), camera_pos, 0.0, None);
        assert_eq!(dots.len(), 1);
        assert_eq!(
            dots[0].offset, 6,
            "chunk (-1, 5)'s centre block (x=-8, y=64, z=88, each +0.5 per \
             Vec3.atCenterOf) predicts offset 6 by the same formula as the exact-position \
             tests, hand-derived independently of this module"
        );
    }

    /// An azimuth-only waypoint (past tracking range) uses the raw angle
    /// directly rather than a position at all — `PI/4` radians (45°, chosen
    /// to stay inside the ±60° clip) predicts offset 64 by hand, proving the
    /// radians-to-degrees conversion and the wrap share the same formula as
    /// the position-based path rather than merely landing on the same side.
    #[test]
    fn an_azimuth_waypoint_uses_the_raw_angle() {
        let camera_pos = glam::Vec3::new(0.0, 64.0, 0.0);
        let far = waypoint(
            WaypointId::Named("far".to_owned()),
            WaypointPosition::Azimuth(std::f32::consts::FRAC_PI_4),
            None,
        );
        let dots = locator_dots(std::iter::once(&far), camera_pos, 0.0, None);
        assert_eq!(dots.len(), 1);
        assert_eq!(dots[0].offset, 64);
    }

    /// `local_id` excludes exactly the matching waypoint and no other —
    /// checked with two present waypoints so the filter's effect is visible
    /// against a control that must still draw.
    #[test]
    fn the_local_players_own_waypoint_is_excluded() {
        let camera_pos = glam::Vec3::new(0.0, 64.0, 0.0);
        let self_id = WaypointId::Named("self".to_owned());
        let other_id = WaypointId::Named("other".to_owned());
        let mine = waypoint(
            self_id.clone(),
            WaypointPosition::Exact(BlockPos { x: 0, y: 64, z: 20 }),
            None,
        );
        let theirs = waypoint(
            other_id,
            WaypointPosition::Exact(BlockPos { x: 5, y: 64, z: 20 }),
            None,
        );
        let dots = locator_dots(
            [&mine, &theirs].into_iter(),
            camera_pos,
            0.0,
            Some(&self_id),
        );
        assert_eq!(dots.len(), 1, "exactly the non-self waypoint must remain");
    }

    /// An explicit server colour is used verbatim, distinct from the hashed
    /// fallback — two pairwise-distinct channel values so a byte-order
    /// transposition (e.g. red/blue swapped) cannot survive unnoticed.
    #[test]
    fn a_server_set_colour_is_used_verbatim() {
        let camera_pos = glam::Vec3::new(0.0, 64.0, 0.0);
        let wp = waypoint(
            WaypointId::Named("tinted".to_owned()),
            WaypointPosition::Exact(BlockPos { x: 0, y: 64, z: 20 }),
            Some(0x11_44_88), // r=0x11 g=0x44 b=0x88, pairwise distinct
        );
        let dots = locator_dots(std::iter::once(&wp), camera_pos, 0.0, None);
        assert_eq!(dots.len(), 1);
        let [r, g, b, a] = dots[0].color;
        assert!((r - 0x11 as f32 / 255.0).abs() < 1e-6);
        assert!((g - 0x44 as f32 / 255.0).abs() < 1e-6);
        assert!((b - 0x88 as f32 / 255.0).abs() < 1e-6);
        assert_eq!(a, 1.0);
    }

    /// Two different identities must hash to different colours in the
    /// overwhelming common case — not a mathematical certainty (hash
    /// collisions exist), but `"alice"` and `"bob"` colliding would be a
    /// suspicious enough coincidence to indicate the hash is constant
    /// rather than identity-derived.
    #[test]
    fn unset_colours_are_derived_from_identity_not_constant() {
        let camera_pos = glam::Vec3::new(0.0, 64.0, 0.0);
        let alice = waypoint(
            WaypointId::Named("alice".to_owned()),
            WaypointPosition::Exact(BlockPos { x: 0, y: 64, z: 20 }),
            None,
        );
        let bob = waypoint(
            WaypointId::Named("bob".to_owned()),
            WaypointPosition::Exact(BlockPos { x: 0, y: 64, z: 20 }),
            None,
        );
        let alice_dots = locator_dots(std::iter::once(&alice), camera_pos, 0.0, None);
        let bob_dots = locator_dots(std::iter::once(&bob), camera_pos, 0.0, None);
        assert_ne!(alice_dots[0].color, bob_dots[0].color);
    }

    /// `String.hashCode()` ported bit for bit, checked against literal
    /// values computed independently of this port (`"a"` and `""` are
    /// JLS-specified: `hashCode()` of a 1-char string is the char's own
    /// UTF-16 value, and the empty string hashes to zero by definition).
    #[test]
    fn java_string_hash_matches_the_specified_algorithm() {
        assert_eq!(java_string_hash(""), 0);
        assert_eq!(java_string_hash("a"), 97);
        // "ab" = 'a'*31 + 'b' = 97*31 + 98 = 3105
        assert_eq!(java_string_hash("ab"), 3105);
    }

    /// `UUID.hashCode()` ported bit for bit against a hand-computed value:
    /// `mostSigBits=0x00000000_00000001`, `leastSigBits=0x00000000_00000002`
    /// (a UUID whose only set bits are the low bit of each half) gives
    /// `hilo = 1 ^ 2 = 3`, so `(3 >> 32) ^ 3 == 0 ^ 3 == 3`.
    #[test]
    fn java_uuid_hash_matches_the_specified_algorithm() {
        let id = uuid::Uuid::from_u64_pair(1, 2);
        assert_eq!(java_uuid_hash(id), 3);
    }
}
