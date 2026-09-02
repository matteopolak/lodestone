//! Translucent geometry ordering.
//!
//! Opaque terrain is order-independent: the depth buffer resolves it. Translucent
//! surfaces (water, stained glass, ice) are not — alpha blending is
//! order-dependent, so within a section the translucent quads must be drawn
//! **back-to-front** from the camera, and re-sorted when the camera moves enough
//! to change that order.
//!
//! # Render layers
//!
//! Geometry is bucketed into three [`RenderLayer`]s drawn in order: `Solid`
//! (opaque, depth-write, any order), `Cutout` (alpha-tested — fully opaque or
//! fully transparent texels, e.g. leaves/grass, still order-independent), and
//! `Translucent` (partial alpha, order-dependent, sorted).
//!
//! **Note / correction:** the task described routing quads "by
//! `BakedQuad.layer`", but that field is the *atlas* layer (which texture-array
//! slice a sprite lives on — always `0` today), **not** the render layer.
//! `lodestone-assets` does not expose a per-quad render type, so the render-layer
//! decision is a renderer concern. The renderer **derives** it from the sprite's
//! alpha channel in the stitched atlas — the data it already has — via
//! [`RenderLayer::from_sprite_alpha`]. That is a heuristic, not vanilla's
//! authoritative per-block `RenderType` (which lives only in version-specific
//! Java, absent from every generated data report); [`RenderLayer::classify`]
//! stays the hook for that table if a version crate ever surfaces it. This is
//! flagged in the R1 report.
//!
//! # Re-sort trigger
//!
//! Sorting every section every frame is unaffordable. Vanilla
//! (`TranslucencyPointOfView`) quantizes the camera to **section granularity per
//! axis, clamped to `{-1, 0, 1}`** relative to the section being drawn, and only
//! re-sorts a section when that triple changes. Moving within the same octant
//! leaves the back-to-front order unchanged (the relative geometry hasn't
//! flipped), so the sort is skipped. [`SortViewpoint`] reproduces this exactly.

use crate::mesh::Mesh;
use crate::vertex::PackedVertex;

/// Which pass a quad is drawn in. Ordering of the variants is the draw order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RenderLayer {
    /// Fully opaque geometry. Depth-write on, blend off, any draw order.
    Solid,
    /// Alpha-tested geometry (leaves, grass, iron bars): each texel is fully
    /// opaque or fully transparent, so it is still order-independent.
    Cutout,
    /// Partially transparent geometry (water, stained glass): order-dependent,
    /// drawn back-to-front after being [sorted](TranslucentMesh::update).
    Translucent,
}

impl RenderLayer {
    /// Classify a quad into a render layer.
    ///
    /// `force_translucent` mirrors the model JSON hint 26.2 added
    /// (`{"sprite": …, "force_translucent": true}`); `has_partial_alpha` is
    /// whether the sprite has any texel with `0 < a < 255`. `cutout` marks
    /// alpha-tested sprites. This is the classification `lodestone-assets` will
    /// eventually feed once it exposes render types; until then the caller
    /// decides.
    #[must_use]
    pub fn classify(force_translucent: bool, has_partial_alpha: bool, cutout: bool) -> Self {
        if force_translucent || has_partial_alpha {
            RenderLayer::Translucent
        } else if cutout {
            RenderLayer::Cutout
        } else {
            RenderLayer::Solid
        }
    }

    /// **Derive** a render layer directly from a sprite's alpha channel — the
    /// data the renderer actually has, since `lodestone-assets` exposes no
    /// per-block render type (see the module docs and the R1 report).
    ///
    /// `alpha` is the sprite's per-texel alpha (one byte per texel, any order):
    ///
    /// * any texel with `0 < a < 255` → [`Translucent`](RenderLayer::Translucent)
    ///   (partial coverage — water, stained glass, ice);
    /// * else any fully-transparent texel (`a == 0`) with the rest opaque →
    ///   [`Cutout`](RenderLayer::Cutout) (alpha-tested holes — leaves, panes,
    ///   cross-shaped plants, the grass side-overlay);
    /// * else (every texel opaque) → [`Solid`](RenderLayer::Solid).
    ///
    /// This is a heuristic, and the report says so: vanilla's authoritative
    /// source is a hardcoded per-block `RenderType` in version-specific Java
    /// (`Blocks`'s own decompiled source), which is **not** in any generated data report. The alpha
    /// scan agrees with it on the common cases (opaque cubes → Solid; leaves and
    /// panes → Cutout; water and stained glass → Translucent), but cannot see
    /// vanilla's overrides — e.g. a block vanilla forces onto the translucent
    /// pass despite fully-opaque texels. If a version crate ever surfaces the
    /// render-type table, feed it through [`classify`](RenderLayer::classify)
    /// and this becomes the fallback. An empty slice is treated as `Solid`.
    #[must_use]
    pub fn from_sprite_alpha(alpha: &[u8]) -> Self {
        let mut has_transparent = false;
        for &a in alpha {
            if a != 0 && a != 255 {
                return RenderLayer::Translucent;
            }
            has_transparent |= a == 0;
        }
        if has_transparent {
            RenderLayer::Cutout
        } else {
            RenderLayer::Solid
        }
    }
}

/// The camera quantized to a per-section octant, matching vanilla's
/// `TranslucencyPointOfView`.
///
/// Two viewpoints comparing equal require no re-sort of the section's
/// translucent quads. Construct with [`SortViewpoint::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SortViewpoint {
    octant: [i32; 3],
}

/// Section side length in blocks.
const SECTION: f64 = 16.0;

fn block_to_section(coord: f64) -> i32 {
    coord.div_euclid(SECTION) as i32
}

impl SortViewpoint {
    /// Quantize `camera_world` (block coordinates) relative to the section at
    /// section-grid coordinate `section_coord`.
    ///
    /// Per axis: which section the camera is in minus this section, clamped to
    /// `{-1, 0, 1}`. `0` means the camera is inside this section's slab on that
    /// axis; `±1` means it is at least one section away.
    #[must_use]
    pub fn new(camera_world: [f64; 3], section_coord: [i32; 3]) -> Self {
        let axis = |c: f64, s: i32| (block_to_section(c) - s).clamp(-1, 1);
        Self {
            octant: [
                axis(camera_world[0], section_coord[0]),
                axis(camera_world[1], section_coord[1]),
                axis(camera_world[2], section_coord[2]),
            ],
        }
    }

    /// Whether moving from `previous` to `self` requires a re-sort. `None`
    /// (never sorted) always requires one.
    #[must_use]
    pub fn needs_resort(self, previous: Option<SortViewpoint>) -> bool {
        previous != Some(self)
    }
}

/// A translucent section mesh that can be re-sorted back-to-front on demand.
///
/// Holds the vertices plus, per quad, its six indices and centroid (in
/// section-local space). [`update`](Self::update) checks the [`SortViewpoint`]
/// and re-sorts only when the octant changed, then [`indices`](Self::indices)
/// yields the current draw order.
#[derive(Debug, Clone)]
pub struct TranslucentMesh {
    vertices: Vec<PackedVertex>,
    quads: Vec<QuadRef>,
    order: Vec<usize>,
    last: Option<SortViewpoint>,
    section_coord: [i32; 3],
}

#[derive(Debug, Clone, Copy)]
struct QuadRef {
    indices: [u32; 6],
    centroid: [f32; 3],
}

impl TranslucentMesh {
    /// Build from a [`Mesh`] whose indices are consecutive 6-index quads (the
    /// form both [`mesh_simple`](crate::mesh::mesh_simple) and
    /// [`mesh_greedy`](crate::mesh::mesh_greedy) produce). `section_coord` is the
    /// section's grid coordinate, used by [`update`](Self::update).
    #[must_use]
    pub fn from_mesh(mesh: &Mesh, section_coord: [i32; 3]) -> Self {
        let mut quads = Vec::with_capacity(mesh.indices.len() / 6);
        for chunk in mesh.indices.chunks_exact(6) {
            let idx: [u32; 6] = [chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5]];
            // The two triangles of a quad share four distinct vertices; the
            // first triangle's three plus the one unique vertex of the second
            // cover all four. Averaging the six positions (with the shared two
            // double-counted) still yields a point on the quad, but we take the
            // four distinct corners for a true centroid.
            let mut seen: Vec<u32> = Vec::with_capacity(4);
            for &i in &idx {
                if !seen.contains(&i) {
                    seen.push(i);
                }
            }
            let mut c = [0.0f32; 3];
            for &i in &seen {
                let p = mesh.vertices[i as usize].unpack().pos;
                c[0] += p[0] as f32;
                c[1] += p[1] as f32;
                c[2] += p[2] as f32;
            }
            let n = seen.len() as f32;
            quads.push(QuadRef {
                indices: idx,
                centroid: [c[0] / n, c[1] / n, c[2] / n],
            });
        }
        let order = (0..quads.len()).collect();
        Self {
            vertices: mesh.vertices.clone(),
            quads,
            order,
            last: None,
            section_coord,
        }
    }

    /// The vertex buffer (draw-order independent).
    #[must_use]
    pub fn vertices(&self) -> &[PackedVertex] {
        &self.vertices
    }

    /// The index buffer in the current back-to-front order.
    #[must_use]
    pub fn indices(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.quads.len() * 6);
        for &q in &self.order {
            out.extend_from_slice(&self.quads[q].indices);
        }
        out
    }

    /// Number of quads.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.quads.len()
    }

    /// Re-sort iff the camera crossed into a new octant relative to this
    /// section. Returns `true` when a re-sort actually happened.
    ///
    /// `camera_world` is in block coordinates; it is converted to section-local
    /// space (subtracting the section origin) for the distance comparison, so
    /// the ordering is exact regardless of how far the section is from the
    /// origin.
    pub fn update(&mut self, camera_world: [f64; 3]) -> bool {
        let vp = SortViewpoint::new(camera_world, self.section_coord);
        if !vp.needs_resort(self.last) {
            return false;
        }
        self.last = Some(vp);
        let origin = [
            self.section_coord[0] as f64 * SECTION,
            self.section_coord[1] as f64 * SECTION,
            self.section_coord[2] as f64 * SECTION,
        ];
        let cam_local = [
            (camera_world[0] - origin[0]) as f32,
            (camera_world[1] - origin[1]) as f32,
            (camera_world[2] - origin[2]) as f32,
        ];
        self.resort(cam_local);
        true
    }

    /// Sort quads farthest-first from `camera_local` (section-local space).
    /// Exposed for the GPU test, which drives the order directly.
    pub fn resort(&mut self, camera_local: [f32; 3]) {
        let dist2 = |c: [f32; 3]| {
            let dx = c[0] - camera_local[0];
            let dy = c[1] - camera_local[1];
            let dz = c[2] - camera_local[2];
            dx * dx + dy * dy + dz * dz
        };
        let quads = &self.quads;
        self.order.sort_by(|&a, &b| {
            dist2(quads[b].centroid)
                .partial_cmp(&dist2(quads[a].centroid))
                .unwrap_or(core::cmp::Ordering::Equal)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octant_is_zero_inside_the_section() {
        // Section (0,0,0) spans blocks 0..16. Camera at (8,8,8) is inside.
        let vp = SortViewpoint::new([8.0, 8.0, 8.0], [0, 0, 0]);
        assert_eq!(vp.octant, [0, 0, 0]);
    }

    #[test]
    fn octant_signs_and_clamps() {
        // Camera far below/left, far above/right of section (2,2,2) (blocks 32..48).
        let below = SortViewpoint::new([-100.0, 8.0, 40.0], [2, 2, 2]);
        assert_eq!(below.octant, [-1, -1, 0]);
        let above = SortViewpoint::new([1000.0, 40.0, 200.0], [2, 2, 2]);
        assert_eq!(above.octant, [1, 0, 1]);
    }

    #[test]
    fn moving_within_an_octant_does_not_resort() {
        // Two camera positions both inside section (0,0,0): same octant.
        let a = SortViewpoint::new([1.0, 1.0, 1.0], [0, 0, 0]);
        let b = SortViewpoint::new([14.9, 9.0, 2.0], [0, 0, 0]);
        assert_eq!(a, b);
        assert!(!b.needs_resort(Some(a)));
    }

    #[test]
    fn crossing_a_section_boundary_resorts() {
        // Camera moves from inside section 0 to inside section 1 on X.
        let inside = SortViewpoint::new([8.0, 8.0, 8.0], [0, 0, 0]);
        let crossed = SortViewpoint::new([20.0, 8.0, 8.0], [0, 0, 0]);
        assert_ne!(inside, crossed);
        assert!(crossed.needs_resort(Some(inside)));
    }

    #[test]
    fn first_update_always_resorts_then_stabilises() {
        let mesh = two_quad_mesh();
        let mut tm = TranslucentMesh::from_mesh(&mesh, [0, 0, 0]);
        assert!(tm.update([8.0, 8.0, -5.0]), "first update must sort");
        assert!(
            !tm.update([8.0, 9.0, -4.0]),
            "second update in same octant must not re-sort"
        );
        assert!(
            tm.update([8.0, 8.0, 40.0]),
            "crossing to the far side must re-sort"
        );
    }

    #[test]
    fn order_is_back_to_front() {
        // Two quads at z=2 (near) and z=14 (far-ish); camera at z=-10 sees the
        // z=2 quad as nearer, so back-to-front puts z=14 first.
        let mesh = two_quad_mesh();
        let mut tm = TranslucentMesh::from_mesh(&mesh, [0, 0, 0]);
        tm.resort([8.0, 8.0, -10.0]);
        let far_first = tm.order;
        // Quad 0 is at z=2, quad 1 at z=14 (see two_quad_mesh).
        assert_eq!(far_first, vec![1, 0], "farther quad (z=14) drawn first");

        // Camera on the far side: order flips.
        let mut tm = TranslucentMesh::from_mesh(&mesh, [0, 0, 0]);
        tm.resort([8.0, 8.0, 30.0]);
        assert_eq!(tm.order, vec![0, 1], "now z=2 quad is farther, drawn first");
    }

    #[test]
    fn indices_follow_the_sorted_order() {
        let mesh = two_quad_mesh();
        let mut tm = TranslucentMesh::from_mesh(&mesh, [0, 0, 0]);
        tm.resort([8.0, 8.0, -10.0]);
        // order [1,0] → quad 1's six indices (vertices 4..8) then quad 0's.
        assert_eq!(tm.indices(), vec![4, 5, 6, 6, 7, 4, 0, 1, 2, 2, 3, 0]);
    }

    #[test]
    fn classify_prefers_translucent_then_cutout() {
        assert_eq!(
            RenderLayer::classify(true, false, false),
            RenderLayer::Translucent
        );
        assert_eq!(
            RenderLayer::classify(false, true, true),
            RenderLayer::Translucent
        );
        assert_eq!(
            RenderLayer::classify(false, false, true),
            RenderLayer::Cutout
        );
        assert_eq!(
            RenderLayer::classify(false, false, false),
            RenderLayer::Solid
        );
        assert!(RenderLayer::Solid < RenderLayer::Translucent);
    }

    #[test]
    fn from_sprite_alpha_derives_layer_from_texel_coverage() {
        // All opaque → Solid (dirt, stone, a plain cube face).
        assert_eq!(
            RenderLayer::from_sprite_alpha(&[255, 255, 255, 255]),
            RenderLayer::Solid
        );
        // Opaque + fully transparent holes, nothing partial → Cutout (leaves,
        // panes, cross plants, the grass side-overlay).
        assert_eq!(
            RenderLayer::from_sprite_alpha(&[255, 0, 255, 0]),
            RenderLayer::Cutout
        );
        // Any partial texel → Translucent (water, stained glass, ice), and it
        // wins even when transparent texels are also present.
        assert_eq!(
            RenderLayer::from_sprite_alpha(&[255, 0, 128, 255]),
            RenderLayer::Translucent
        );
        assert_eq!(
            RenderLayer::from_sprite_alpha(&[10]),
            RenderLayer::Translucent
        );
        // Degenerate empty sprite → Solid (no coverage information).
        assert_eq!(RenderLayer::from_sprite_alpha(&[]), RenderLayer::Solid);
    }

    /// Two axis-aligned quads facing -Z, one at z=2 and one at z=14, each four
    /// distinct vertices, six indices. Centroids differ only in z.
    fn two_quad_mesh() -> Mesh {
        use crate::section::Face;
        use crate::vertex::VertexFields;

        let mut mesh = Mesh::default();
        for (qi, z) in [2u32, 14u32].into_iter().enumerate() {
            let base = (qi * 4) as u32;
            for (x, y) in [(4u32, 4u32), (12, 4), (12, 12), (4, 12)] {
                mesh.vertices.push(PackedVertex::pack(VertexFields {
                    pos: [x, y, z],
                    normal: Face::NegZ,
                    ao: 255,
                    sky_light: 255,
                    block_light: 0,
                    sprite: 0,
                    u: 0,
                    v: 0,
                }));
            }
            mesh.indices
                .extend_from_slice(&[base, base + 1, base + 2, base + 2, base + 3, base]);
        }
        mesh
    }
}
