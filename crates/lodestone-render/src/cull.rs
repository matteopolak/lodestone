//! The per-frame terrain cull the production draw loops consult: vanilla's
//! circular view membership, then the view frustum, then (optionally) the
//! section occlusion graph's reachable set.
//!
//! # Why this type exists rather than three predicates at the call site
//!
//! `gpu/frame.rs` has three terrain loops (packed table, live opaque, live
//! water) and every cull has to apply identically to all three or water and
//! terrain disagree about what exists. Bundling the tests behind one
//! [`TerrainCull::visible`] keeps that single-source-of-truth property, and it
//! is the seam the occlusion walk plugs into without touching the loops again
//! (see [`TerrainCull::with_reachable`]).
//!
//! # Ordering, and why it is the cheap test first
//!
//! [`visible`](TerrainCull::visible) evaluates *distance* before *frustum*: the
//! distance predicate is four integer ops and rejects ~23–29% of the resident
//! set at any heading, while the frustum test is six plane evaluations against
//! two derived AABB corners. Both are exact-outside/conservative-inside, so the
//! order changes cost and not the result — the counters are attributed to
//! whichever test fired first, which is why `sections_culled_distance` and
//! `sections_culled_frustum` are reported separately and must be summed, not
//! compared.
//!
//! # The 8-block camera-cube offset (the one thing that is easy to get wrong)
//!
//! An exact frustum test culls the section you are *standing in* half the time,
//! because the near plane slices through it — the classic
//! "correct-looking cull that fails at certain positions". Vanilla handles this
//! in `Frustum.offsetToFullyIncludeCameraCube(8)` by walking the frustum origin
//! backwards until the camera's own 8-block-aligned cube is fully inside; we do
//! the tighter equivalent in [`Frustum::offset_to_include_camera_cube`], pushing
//! only the planes that cut that cube and only as far as needed.

use glam::Vec3;

use crate::camera::{Camera, Frustum};
use crate::section::SECTION_SIZE;
use crate::visibility::SectionCoord;

/// The half-section cell vanilla aligns its camera cube to
/// (`SectionOcclusionGraph`'s 8-block invalidation grid, and the argument to
/// `Frustum.offsetToFullyIncludeCameraCube`).
pub const CAMERA_CUBE_BLOCKS: f32 = 8.0;

/// Vanilla's chunk view-membership predicate, verbatim from
/// `ChunkTrackingView.isWithinDistance`
/// (`.cache/mc/26.2/src/net/minecraft/server/level/ChunkTrackingView.java:71-80`):
///
/// ```java
/// long dx = Math.max(0, Math.abs(chunkX - centerX) - 1);
/// long dz = Math.max(0, Math.abs(chunkZ - centerZ) - 1);
/// return dx * dx + dz * dz < viewDistance * viewDistance;
/// ```
///
/// A **rounded circle with a one-chunk buffer**, not the streamed square. The
/// buffer is what makes the strict `<` safe: the ring at exactly `viewDistance`
/// is still kept (`(rd-1)² < rd²`), so this never removes a column the fog has
/// not already taken to its end value (`fog.rs`'s render-distance end is
/// `rd·16`).
///
/// Porting it exactly is vanilla parity by construction — a `<=` here would draw
/// a whole extra ring and nothing else in this crate would notice, which is why
/// [`the boundary case is pinned by a test`](self).
#[must_use]
pub fn within_view_distance(
    center: (i32, i32),
    chunk: (i32, i32),
    view_distance: u32,
) -> bool {
    let dx = i64::from((chunk.0 - center.0).abs()).saturating_sub(1).max(0);
    let dz = i64::from((chunk.1 - center.1).abs()).saturating_sub(1).max(0);
    let rd = i64::from(view_distance);
    dx * dx + dz * dz < rd * rd
}

/// Why a section was not drawn, or that it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CullVerdict {
    /// Draw it.
    Visible,
    /// Outside vanilla's circular view membership ([`within_view_distance`]).
    Distance,
    /// Outside the view frustum.
    Frustum,
    /// Not reachable from the camera through connected open space — only ever
    /// returned when a reachable set has been installed via
    /// [`TerrainCull::with_reachable`].
    Occlusion,
}

/// One frame's terrain cull: the frustum (already camera-cube-offset) plus the
/// camera's chunk column and render distance.
#[derive(Debug, Clone)]
pub struct TerrainCull {
    frustum: Frustum,
    camera_chunk: (i32, i32),
    view_distance: u32,
    /// `None` disables the reachability test entirely (the pre-U3 behaviour and
    /// the permanent behaviour whenever the occlusion graph has no entry for the
    /// camera's own section — see [`with_reachable`](Self::with_reachable)).
    reachable: Option<std::collections::HashSet<SectionCoord>>,
    /// `false` makes every [`classify`](Self::classify) return
    /// [`CullVerdict::Visible`] — see [`disabled`](Self::disabled).
    enabled: bool,
}

impl TerrainCull {
    /// Build this frame's cull from the live camera and render distance.
    ///
    /// `render_distance_chunks == 0` disables the distance test rather than
    /// culling everything: zero is what an uninitialised `RenderState` would
    /// hold, and a cull that blanks the world on a default-constructed state is
    /// indistinguishable from a broken renderer.
    #[must_use]
    pub fn new(camera: &Camera, render_distance_chunks: u32) -> Self {
        let frustum = camera
            .frustum()
            .offset_to_include_camera_cube(camera.position, CAMERA_CUBE_BLOCKS);
        let size = SECTION_SIZE as f32;
        Self {
            frustum,
            camera_chunk: (
                (camera.position.x / size).floor() as i32,
                (camera.position.z / size).floor() as i32,
            ),
            view_distance: render_distance_chunks,
            reachable: None,
            enabled: true,
        }
    }

    /// Turn the whole cull off, so every resident section draws.
    ///
    /// This is the live false-cull diagnostic and the A/B lever for the
    /// instruction harness — vanilla has the same switch (`smartCull`, which it
    /// disables for a spectator inside a block). If terrain reappears with this
    /// off, a cull dropped it; if it does not, the section was never resident.
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.enabled = !disabled;
        self
    }

    /// Attach the occlusion graph's reachable set, so [`visible`](Self::visible)
    /// also rejects sections the camera cannot see through connected open space.
    ///
    /// Passing `None` — which is what a graph missing the camera's own section
    /// must produce — leaves the cull at frustum ∩ distance. That fallback is
    /// deliberate and is also the failure mode to watch for: a walk that
    /// silently degrades draws *more*, never less.
    #[must_use]
    pub fn with_reachable(
        mut self,
        reachable: Option<std::collections::HashSet<SectionCoord>>,
    ) -> Self {
        self.reachable = reachable;
        self
    }

    /// Whether a reachable set is installed (i.e. whether occlusion culling is
    /// actually in force this frame, as opposed to having degraded to
    /// frustum ∩ distance).
    #[must_use]
    pub fn occlusion_active(&self) -> bool {
        self.enabled && self.reachable.is_some()
    }

    /// The camera-cube-offset frustum, for callers that need it directly.
    #[must_use]
    pub fn frustum(&self) -> &Frustum {
        &self.frustum
    }

    /// Classify one section by its grid coordinate.
    #[must_use]
    pub fn classify(&self, coord: SectionCoord) -> CullVerdict {
        if !self.enabled {
            return CullVerdict::Visible;
        }
        if self.view_distance > 0
            && !within_view_distance(self.camera_chunk, (coord.0, coord.2), self.view_distance)
        {
            return CullVerdict::Distance;
        }
        if !self.frustum.section_visible(coord) {
            return CullVerdict::Frustum;
        }
        if let Some(reachable) = &self.reachable
            && !reachable.contains(&coord)
        {
            return CullVerdict::Occlusion;
        }
        CullVerdict::Visible
    }

    /// Whether a section should be drawn this frame.
    #[must_use]
    pub fn visible(&self, coord: SectionCoord) -> bool {
        self.classify(coord) == CullVerdict::Visible
    }
}

impl Frustum {
    /// Push outward any plane that cuts the camera's `offset`-aligned cube, so a
    /// section straddling the camera can never be culled.
    ///
    /// Vanilla's `Frustum.offsetToFullyIncludeCameraCube(offset)` walks the
    /// frustum's origin backwards along the view vector in 4-block steps until
    /// `cubeCompletelyInFrustum` holds for the camera's cube. This is the
    /// closed-form equivalent: for each plane, take the cube corner furthest
    /// *against* the plane normal and, if it is outside, translate that plane by
    /// exactly the deficit. Only planes that actually cut the cube move, and each
    /// moves the minimum amount — so this is never *more* permissive than
    /// vanilla's loop, and never culls the camera's own section.
    #[must_use]
    pub fn offset_to_include_camera_cube(mut self, camera: Vec3, offset: f32) -> Self {
        if offset <= 0.0 {
            return self;
        }
        let lo = (camera / offset).floor() * offset;
        let hi = lo + Vec3::splat(offset);
        for plane in &mut self.planes {
            let n = plane.normal;
            // The cube corner furthest against the normal: if this one is
            // inside, the whole cube is.
            let worst = Vec3::new(
                if n.x >= 0.0 { lo.x } else { hi.x },
                if n.y >= 0.0 { lo.y } else { hi.y },
                if n.z >= 0.0 { lo.z } else { hi.z },
            );
            let distance = plane.signed_distance(worst);
            if distance < 0.0 {
                // n·p + d' == 0 at the worst corner.
                plane.d -= distance;
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three rows of `docs/plans/render-performance.md`'s U2 table, whose
    /// expected values were computed from the Java expression, not from this
    /// code. The square is the *streamed* extent (`view_radius = rd + 1`).
    fn membership(rd: u32) -> (usize, usize) {
        let r = rd as i32 + 1;
        let mut inside = 0;
        let mut total = 0;
        for x in -r..=r {
            for z in -r..=r {
                total += 1;
                if within_view_distance((0, 0), (x, z), rd) {
                    inside += 1;
                }
            }
        }
        (inside, total)
    }

    #[test]
    fn circular_membership_matches_vanilla_counts() {
        assert_eq!(membership(8), (257, 361));
        assert_eq!(membership(16), (921, 1225));
        assert_eq!(membership(32), (3461, 4489));
    }

    #[test]
    fn boundary_ring_pins_the_strict_inequality() {
        // (8,0) at rd 8: (8-1)^2 = 49 < 64 -> in. (9,0): (9-1)^2 = 64, not < 64
        // -> out. A `<=` transcription draws a whole extra ring and nothing else
        // notices; this is the case that fails.
        assert!(within_view_distance((0, 0), (8, 0), 8));
        assert!(!within_view_distance((0, 0), (9, 0), 8));
        assert!(!within_view_distance((0, 0), (9, 9), 8));
    }

    fn cam(position: Vec3, yaw: f32) -> Camera {
        Camera {
            position,
            yaw,
            pitch: 0.0,
            aspect: 16.0 / 9.0,
            fov_y_degrees: 70.0,
            near: 0.05,
            far: 2048.0,
        }
    }

    #[test]
    fn camera_own_section_survives_at_every_offset_within_it() {
        // Sweep the camera across its own section in 1-block steps at eight
        // headings: the section it stands in must never be culled. Without the
        // camera-cube offset this fails at the positions where the near plane
        // slices the section.
        for step in 0..16 {
            let p = Vec3::new(0.5 + step as f32, 40.5, 0.5 + step as f32);
            for turn in 0..8 {
                let cull = TerrainCull::new(&cam(p, turn as f32 * 45.0), 32);
                assert_eq!(
                    cull.classify((0, 2, 0)),
                    CullVerdict::Visible,
                    "camera at {p:?} yaw {} culled its own section",
                    turn * 45
                );
            }
        }
    }

    #[test]
    fn behind_the_camera_is_culled_but_ahead_is_not() {
        // Yaw 0 looks south, i.e. toward **+Z** (`camera.rs`'s own
        // `cam_looking_south` fixture); assert on both directions so a
        // convention flip cannot pass by symmetry.
        let cull = TerrainCull::new(&cam(Vec3::new(8.0, 40.0, 8.0), 0.0), 32);
        let ahead = (0..8).filter(|i| cull.visible((0, 2, *i))).count();
        let behind = (1..9).filter(|i| cull.visible((0, 2, -i))).count();
        assert_eq!(ahead, 8, "sections dead ahead must all be drawn");
        assert_eq!(behind, 0, "sections dead behind must all be culled");
    }

    #[test]
    fn zero_render_distance_disables_the_distance_test() {
        let cull = TerrainCull::new(&cam(Vec3::new(8.0, 40.0, 8.0), 0.0), 0);
        // A section 40 chunks ahead would be a `Distance` verdict at any real
        // render distance; at 0 the distance predicate does not run at all, so
        // it survives to the frustum test and passes it.
        assert_eq!(cull.classify((0, 2, 40)), CullVerdict::Visible);
        // Behind is still culled — "distance off" is not "cull off".
        assert_eq!(cull.classify((0, 2, -40)), CullVerdict::Frustum);
    }

    #[test]
    fn reachable_set_rejects_unreachable_sections() {
        let base = TerrainCull::new(&cam(Vec3::new(8.0, 40.0, 8.0), 0.0), 32);
        assert_eq!(base.classify((0, 2, 2)), CullVerdict::Visible);
        assert!(!base.occlusion_active());
        let cull = base.with_reachable(Some([(0, 2, 0)].into_iter().collect()));
        assert!(cull.occlusion_active());
        assert_eq!(cull.classify((0, 2, 2)), CullVerdict::Occlusion);
        assert_eq!(cull.classify((0, 2, 0)), CullVerdict::Visible);
    }
}
