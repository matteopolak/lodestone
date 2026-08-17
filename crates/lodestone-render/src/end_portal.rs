//! The end portal / end gateway star-field effect —
//! `AbstractEndPortalRenderer`/`TheEndPortalRenderer`/`TheEndGatewayRenderer`,
//! ported. The genuinely novel piece in this corpus: every other block-entity
//! type in [`crate::block_entity`] is a cuboid rig or a procedural quad-strip
//! (the beacon beam), sampling an ordinary diffuse texture. This one samples
//! **its own screen-space projection** — the swirling "void" look comes from
//! feeding each fragment's own clip-space position back in as a texture
//! coordinate, not from any per-vertex UV.
//!
//! ## What it is
//!
//! `assets/minecraft/models/block/end_portal.json`/`end_gateway.json` are
//! `{"textures":{"particle":...}}` — zero elements, the same total-absence
//! hole [`crate::block_entity`]'s module doc describes for chest/skull.
//! Before this landed, every stronghold's end portal and every End island's
//! gateway were literally invisible: the frame/gateway-ring blocks around
//! them are real geometry, but the portal surface itself drew nothing.
//!
//! ## How it works
//!
//! Two things per instance, both ported directly from the real jar:
//!
//! 1. **Which faces to draw.** `TheEndPortalBlockEntity.shouldRenderFace`
//!    tests only the axis — `direction.getAxis() == Y` — with **no** neighbor
//!    check at all, so an end portal always submits exactly its top and
//!    bottom faces regardless of what is adjacent (there is no
//!    [`end_portal_vertices`] parameter for this reason; it is not a choice
//!    the caller makes). `TheEndGatewayBlockEntity.shouldRenderFace` instead
//!    delegates to `Block.shouldRenderFace`, the ordinary neighbor-occlusion
//!    test every terrain quad already uses — [`end_gateway_vertices`] takes
//!    the resolved face list as a parameter because computing it needs a
//!    live [`lodestone_world::World`] this crate cannot depend on; see
//!    `lodestone_shell::block_entities::end_gateway_spawns` for the gather.
//! 2. **The squash.** `TheEndPortalRenderer.TRANSFORMATION` — `translate(0,
//!    0.375, 0) · scale(1, 0.375, 1)` — flattens the portal's cube into a
//!    thin slab spanning `y ∈ [0.375, 0.75]`, matching the real portal
//!    frame's height. `TheEndGatewayRenderer.submit` applies **no**
//!    transform at all before `submitCube`, so a gateway's swirl fills the
//!    *whole* block (`y ∈ [0, 1]`) — the one geometric difference between the
//!    two types, both driven through the same [`push_face`].
//!
//! Vertex data is **position only** — no UV, no colour, no normal. That is
//! not a simplification; it is what `AbstractEndPortalRenderer.FACES`
//! actually submits (`buffer.addVertex(pose, faceVertex)`, nothing else).
//! The illusion comes entirely from the fragment shader
//! (`gpu/end_portal.wgsl`), which derives its own texture coordinate from
//! each fragment's *own* clip-space position
//! (`projection.glsl#projection_from_position`) — see that shader's own doc
//! for the full derivation, since the GLSL source multiplies a `vec4` by a
//! `mat4` using **row-vector** semantics (`v * M`), which is not WGSL's
//! column-vector convention and had to be re-derived component-by-component
//! rather than transliterated.
//!
//! ## What is deliberately not ported
//!
//! * **The gateway beam** (`TheEndGatewayRenderer.submit`'s
//!   `BeaconRenderer.submitBeaconBeam` call, shown while `isSpawning()`/
//!   `isCoolingDown()`). It needs a per-position client-simulated
//!   `teleportCooldown` tracker fed by the gateway's own `BLOCK_EVENT`
//!   (`b0 == 1`, the same collision [`crate::block_entity::BellShakeDirection`]'s
//!   doc already names — told apart by the block at the position, not the
//!   packet) plus the block entity's `Age` NBT for the rarer spawn-phase arm.
//!   Not built this session: a real gateway's beam is visible for ~10 s once
//!   after placement and ~2 s after every teleport-through, a small fraction
//!   of a gateway's total invisible lifetime, so the always-visible swirl
//!   face was the priority. `beam_vertices`-shaped machinery already exists
//!   in [`crate::beacon`] for whoever picks this up — `submitBeaconBeam`'s
//!   general 9-parameter form is what `TheEndGatewayRenderer` calls, not the
//!   beacon's own accumulated-sections wrapper.
//! * **Face culling for the end portal itself.** Vanilla genuinely draws no
//!   side faces (`getAxis() == Y` only) — this is not a gap, it is the real
//!   rule; a portal frame's own real geometry occupies the sides.
//! * **Fog.** Vanilla's own `rendertype_end_portal.fsh` applies `apply_fog`;
//!   this pass does not, the same simplification `gpu/sign_text.rs` and
//!   `gpu/beacon_beam.rs` already make for their own jar-sourced-texture
//!   passes — see those modules' docs. A void that reads a little too crisp
//!   at render-distance edge is the least visible of this pass's gaps.

use lodestone_assets::Direction;
use glam::Vec3;

/// `AbstractEndPortalRenderer.FROM`/`TO` — the whole unit cube.
const FROM: Vec3 = Vec3::ZERO;
const TO: Vec3 = Vec3::ONE;

/// One end-portal block to draw this frame — just a position, since
/// `shouldRenderFace` never consults a neighbor for this type (see the
/// module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndPortalSpawn {
    /// The end-portal block's own integer corner.
    pub pos: [i32; 3],
}

/// One end-gateway block to draw this frame — a position plus the resolved
/// unoccluded face list (`lodestone_shell::block_entities::end_gateway_spawns`
/// computes this; see the module doc for why this crate cannot resolve it
/// itself).
#[derive(Debug, Clone, PartialEq)]
pub struct EndGatewaySpawn {
    /// The end-gateway block's own integer corner.
    pub pos: [i32; 3],
    /// The resolved, unoccluded face list — `Block.shouldRenderFace` already
    /// applied, so every direction here should draw.
    pub faces: Vec<Direction>,
}

/// One vertex: world-space position only. See the module doc for why there
/// is no UV/colour/normal — the shader derives its own texture coordinate
/// from clip-space position, not from anything carried per-vertex.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndPortalVertex {
    /// World-space position.
    pub position: [f32; 3],
    /// `true` for an end-gateway face, `false` for an end-portal face —
    /// carried per-vertex so the GPU pass can merge both types into one
    /// vertex buffer and one draw call while still reproducing vanilla's two
    /// different `PORTAL_LAYERS` shader-define values (15 vs 16) inside a
    /// single, statically-bounded shader loop. See `gpu/end_portal.wgsl`'s
    /// doc for why this is safe (it never affects control flow, only which
    /// terms a per-fragment sum includes).
    pub is_gateway: bool,
}

/// `FaceInfo.fromFacing(direction)`'s four corners, in the real jar's own
/// per-direction order (`AbstractEndPortalRenderer.FACES`, built from
/// `FaceInfo.getVertexInfo(0..=3)`). Transcribed directly from `FaceInfo.java`
/// rather than derived, because each direction's winding is independent —
/// there is no shared formula to generalise from.
#[must_use]
fn face_corners(direction: Direction) -> [Vec3; 4] {
    let p = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
    match direction {
        Direction::Down => [
            p(FROM.x, FROM.y, TO.z),
            p(FROM.x, FROM.y, FROM.z),
            p(TO.x, FROM.y, FROM.z),
            p(TO.x, FROM.y, TO.z),
        ],
        Direction::Up => [
            p(FROM.x, TO.y, FROM.z),
            p(FROM.x, TO.y, TO.z),
            p(TO.x, TO.y, TO.z),
            p(TO.x, TO.y, FROM.z),
        ],
        Direction::North => [
            p(TO.x, TO.y, FROM.z),
            p(TO.x, FROM.y, FROM.z),
            p(FROM.x, FROM.y, FROM.z),
            p(FROM.x, TO.y, FROM.z),
        ],
        Direction::South => [
            p(FROM.x, TO.y, TO.z),
            p(FROM.x, FROM.y, TO.z),
            p(TO.x, FROM.y, TO.z),
            p(TO.x, TO.y, TO.z),
        ],
        Direction::West => [
            p(FROM.x, TO.y, FROM.z),
            p(FROM.x, FROM.y, FROM.z),
            p(FROM.x, FROM.y, TO.z),
            p(FROM.x, TO.y, TO.z),
        ],
        Direction::East => [
            p(TO.x, TO.y, TO.z),
            p(TO.x, FROM.y, TO.z),
            p(TO.x, FROM.y, FROM.z),
            p(TO.x, TO.y, FROM.z),
        ],
    }
}

/// Pushes one face's two triangles (`0,1,2,0,2,3`, the same QUADS-to-triangle
/// fan every other ported vanilla quad in this crate uses), squashed in Y to
/// `[y_min, y_max]` and translated to `pos`.
fn push_face(
    pos: [i32; 3],
    direction: Direction,
    y_min: f32,
    y_max: f32,
    is_gateway: bool,
    out: &mut Vec<EndPortalVertex>,
) {
    let base = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    let corners = face_corners(direction);
    let mut push = |c: Vec3| {
        out.push(EndPortalVertex {
            position: [
                base.x + c.x,
                base.y + y_min + c.y * (y_max - y_min),
                base.z + c.z,
            ],
            is_gateway,
        });
    };
    push(corners[0]);
    push(corners[1]);
    push(corners[2]);
    push(corners[0]);
    push(corners[2]);
    push(corners[3]);
}

/// `TheEndPortalRenderer.submit` — always exactly {Up, Down}, squashed to
/// `y ∈ [0.375, 0.75]` (`TRANSFORMATION`). No face-list parameter: unlike the
/// gateway, `shouldRenderFace` here never consults a neighbor, so there is
/// nothing for a caller to resolve.
#[must_use]
pub fn end_portal_vertices(pos: [i32; 3]) -> Vec<EndPortalVertex> {
    let mut out = Vec::with_capacity(12);
    push_face(pos, Direction::Up, 0.375, 0.75, false, &mut out);
    push_face(pos, Direction::Down, 0.375, 0.75, false, &mut out);
    out
}

/// `TheEndGatewayRenderer.submit`'s `submitCube(state.facesToShow, ...)` —
/// the full unit cube (`y ∈ [0, 1]`, no squash), restricted to whichever
/// faces the caller resolved as unoccluded. See the module doc for why that
/// resolution lives in `lodestone_shell::block_entities::end_gateway_spawns`
/// rather than here.
#[must_use]
pub fn end_gateway_vertices(pos: [i32; 3], faces: &[Direction]) -> Vec<EndPortalVertex> {
    let mut out = Vec::with_capacity(faces.len() * 6);
    for &direction in faces {
        push_face(pos, direction, 0.0, 1.0, true, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `FaceInfo.UP`'s four corners, transcribed straight from `FaceInfo.java`
    /// (`MIN_X,MAX_Y,MIN_Z` / `MIN_X,MAX_Y,MAX_Z` / `MAX_X,MAX_Y,MAX_Z` /
    /// `MAX_X,MAX_Y,MIN_Z`) — every corner sits at `y = 1` (`TO.y`), the only
    /// property worth a dedicated assertion since the four corners are
    /// otherwise just the unit square's own points.
    #[test]
    fn up_face_corners_all_sit_at_max_y() {
        let corners = face_corners(Direction::Up);
        for c in corners {
            assert_eq!(c.y, 1.0);
        }
        assert_eq!(corners[0], Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(corners[1], Vec3::new(0.0, 1.0, 1.0));
        assert_eq!(corners[2], Vec3::new(1.0, 1.0, 1.0));
        assert_eq!(corners[3], Vec3::new(1.0, 1.0, 0.0));
    }

    /// `FaceInfo.DOWN` sits at `y = 0` (`FROM.y`) — the sibling check, and the
    /// pair together is a real regression test: swapping the UP/DOWN arms in
    /// `face_corners` would fail exactly one of these two.
    #[test]
    fn down_face_corners_all_sit_at_min_y() {
        for c in face_corners(Direction::Down) {
            assert_eq!(c.y, 0.0);
        }
    }

    /// An end portal always emits exactly 12 vertices (2 faces × 6 triangle
    /// verts) squashed into `y ∈ [0.375, 0.75]` — the real portal-frame
    /// height, not the full block. Every vertex's Y must land in that range;
    /// getting `TRANSFORMATION`'s translate/scale backwards (e.g. swapping
    /// which constant is the translate and which is the scale) would push
    /// vertices outside it.
    #[test]
    fn end_portal_is_two_faces_squashed_to_the_frame_height() {
        let verts = end_portal_vertices([5, 10, -3]);
        assert_eq!(verts.len(), 12);
        for v in &verts {
            assert!(!v.is_gateway);
            assert!(v.position[1] >= 10.0 + 0.375 - 1e-6);
            assert!(v.position[1] <= 10.0 + 0.75 + 1e-6);
        }
    }

    /// An end gateway with all six faces present fills the *whole* block
    /// (`y ∈ [0, 1]`, unsquashed) — the one geometric difference from the
    /// portal, and the reason this test's Y bound differs from the one
    /// above.
    #[test]
    fn end_gateway_with_every_face_fills_the_whole_block() {
        let faces = [
            Direction::Down,
            Direction::Up,
            Direction::North,
            Direction::South,
            Direction::West,
            Direction::East,
        ];
        let verts = end_gateway_vertices([0, 64, 0], &faces);
        assert_eq!(verts.len(), 36);
        assert!(verts.iter().all(|v| v.is_gateway));
        let min_y = verts.iter().map(|v| v.position[1]).fold(f32::MAX, f32::min);
        let max_y = verts.iter().map(|v| v.position[1]).fold(f32::MIN, f32::max);
        assert_eq!(min_y, 64.0);
        assert_eq!(max_y, 65.0);
    }

    /// A restricted face list draws only that face — the occlusion gate this
    /// crate cannot compute itself (it needs a live `World`), but whose
    /// *consumption* is this function's whole job.
    #[test]
    fn end_gateway_with_one_face_draws_only_that_face() {
        let verts = end_gateway_vertices([1, 2, 3], &[Direction::North]);
        assert_eq!(verts.len(), 6);
        for v in &verts {
            assert_eq!(v.position[2], 3.0, "North sits at min Z");
        }
    }

    /// Translation reaches every vertex, not just the first — a bug that
    /// forgot to add `base` in `push_face`'s closure would leave the first
    /// vertex of each face at the block origin's local (0/1) coordinates,
    /// not the real world position.
    #[test]
    fn end_portal_vertices_are_translated_to_the_block_position() {
        let verts = end_portal_vertices([100, 50, -200]);
        for v in &verts {
            assert!(v.position[0] >= 100.0 && v.position[0] <= 101.0);
            assert!(v.position[2] >= -200.0 && v.position[2] <= -199.0);
        }
    }
}
