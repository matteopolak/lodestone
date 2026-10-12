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
//! Sorting every section every frame is unaffordable. The reference
//! quantizes the camera to **section granularity per axis, clamped to
//! `{-1, 0, 1}`** relative to the section being drawn, and re-sorts when that
//! triple changes, or when the camera crosses a block boundary and the section
//! is nearby or axis-aligned with it. [`SortViewpoint`] and [`should_resort`]
//! reproduce that.

/// Which pass a quad is drawn in. Ordering of the variants is the draw order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RenderLayer {
    /// Fully opaque geometry. Depth-write on, blend off, any draw order.
    Solid,
    /// Alpha-tested geometry (leaves, grass, iron bars): each texel is fully
    /// opaque or fully transparent, so it is still order-independent.
    Cutout,
    /// Partially transparent geometry (water, stained glass): order-dependent,
    /// drawn back-to-front after being [sorted](QuadSort::resort).
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
/// translucency point of view.
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

    /// Whether the camera shares a section slab with this section on at least
    /// one axis.
    #[must_use]
    pub fn is_axis_aligned(self) -> bool {
        self.octant.contains(&0)
    }
}

/// Whether a section's translucent quads must be re-sorted this frame.
///
/// The rule is the reference client's: re-sort when the camera's octant
/// relative to the section changed, or when the camera crossed a block
/// boundary and the section is either axis-aligned with the camera (one octant
/// component is `0`) or nearby. A section never sorted always re-sorts.
#[must_use]
pub fn should_resort(
    last: Option<SortViewpoint>,
    now: SortViewpoint,
    camera_block_changed: bool,
    nearby: bool,
) -> bool {
    now.needs_resort(last) || (camera_block_changed && (now.is_axis_aligned() || nearby))
}

/// One section's translucent quads and the order they are drawn in.
///
/// Holds each quad's six mesh-local indices and its centroid (section-local).
/// [`resort`](Self::resort) orders the quads farthest-first from a camera and
/// returns the index buffer to upload in that order. Vertices never move, so
/// only the index span of the resident mesh is rewritten.
#[derive(Debug, Clone)]
pub struct QuadSort {
    quads: Vec<[u32; 6]>,
    centroids: Vec<[f32; 3]>,
    order: Vec<u32>,
    keys: Vec<f32>,
    out: Vec<u32>,
    last: Option<SortViewpoint>,
}

impl QuadSort {
    /// Build from consecutive 6-index quads. `position` maps a mesh-local
    /// vertex index to its section-local position. `None` when there are fewer
    /// than two quads, since one quad has no order to get wrong.
    #[must_use]
    pub fn new(indices: &[u32], position: impl Fn(u32) -> [f32; 3]) -> Option<Self> {
        let quads: Vec<[u32; 6]> = indices
            .chunks_exact(6)
            .map(|c| [c[0], c[1], c[2], c[3], c[4], c[5]])
            .collect();
        if quads.len() < 2 {
            return None;
        }
        let centroids = quads
            .iter()
            .map(|idx| {
                let mut seen = [0u32; 6];
                let mut n = 0;
                for &i in idx {
                    if !seen[..n].contains(&i) {
                        seen[n] = i;
                        n += 1;
                    }
                }
                let mut c = [0.0f32; 3];
                for &i in &seen[..n] {
                    let p = position(i);
                    c[0] += p[0];
                    c[1] += p[1];
                    c[2] += p[2];
                }
                let n = n as f32;
                [c[0] / n, c[1] / n, c[2] / n]
            })
            .collect();
        Some(Self {
            order: (0..quads.len() as u32).collect(),
            keys: vec![0.0; quads.len()],
            out: Vec::with_capacity(quads.len() * 6),
            quads,
            centroids,
            last: None,
        })
    }

    /// Build from a [`ModelMesh`](crate::models::ModelMesh).
    #[must_use]
    pub fn from_model_mesh(mesh: &crate::models::ModelMesh) -> Option<Self> {
        Self::new(&mesh.indices, |i| mesh.vertices[i as usize].position)
    }

    /// The viewpoint of the last [`resort`](Self::resort), if any.
    #[must_use]
    pub fn last_viewpoint(&self) -> Option<SortViewpoint> {
        self.last
    }

    /// Order the quads farthest-first from `camera_local` (section-local
    /// blocks) and return the index buffer in that order. `viewpoint` is stored
    /// for [`should_resort`].
    pub fn resort(&mut self, camera_local: [f32; 3], viewpoint: SortViewpoint) -> &[u32] {
        self.last = Some(viewpoint);
        for (key, c) in self.keys.iter_mut().zip(&self.centroids) {
            let d = [
                c[0] - camera_local[0],
                c[1] - camera_local[1],
                c[2] - camera_local[2],
            ];
            *key = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
        }
        let keys = &self.keys;
        self.order
            .sort_by(|&a, &b| keys[b as usize].total_cmp(&keys[a as usize]));
        self.out.clear();
        for &q in &self.order {
            self.out.extend_from_slice(&self.quads[q as usize]);
        }
        &self.out
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
    fn rule_resorts_on_octant_change_or_block_move_near_or_aligned() {
        let inside = SortViewpoint::new([8.0, 8.0, 8.0], [0, 0, 0]);
        let diagonal = SortViewpoint::new([100.0, 100.0, 100.0], [0, 0, 0]);
        assert!(should_resort(None, inside, false, false), "never sorted");
        assert!(!should_resort(Some(inside), inside, false, false));
        assert!(should_resort(Some(inside), diagonal, false, false), "octant changed");
        assert!(should_resort(Some(inside), inside, true, false), "axis aligned");
        assert!(!should_resort(Some(diagonal), diagonal, true, false), "far diagonal");
        assert!(should_resort(Some(diagonal), diagonal, true, true), "nearby");
    }

    /// Two quads facing -Z at z=2 and z=14 (quad 0 and quad 1).
    fn two_quads() -> (Vec<u32>, Vec<[f32; 3]>) {
        let mut indices = Vec::new();
        let mut positions = Vec::new();
        for (qi, z) in [2.0f32, 14.0].into_iter().enumerate() {
            let base = (qi * 4) as u32;
            for (x, y) in [(4.0f32, 4.0f32), (12.0, 4.0), (12.0, 12.0), (4.0, 12.0)] {
                positions.push([x, y, z]);
            }
            indices.extend_from_slice(&[base, base + 1, base + 2, base + 2, base + 3, base]);
        }
        (indices, positions)
    }

    #[test]
    fn order_is_back_to_front_and_indices_follow() {
        let (indices, positions) = two_quads();
        let mut qs = QuadSort::new(&indices, |i| positions[i as usize]).unwrap();
        let vp = SortViewpoint::new([8.0, 8.0, -10.0], [0, 0, 0]);
        // z=14 is the farther quad, so its indices (4..8) come first.
        assert_eq!(qs.resort([8.0, 8.0, -10.0], vp), [4, 5, 6, 6, 7, 4, 0, 1, 2, 2, 3, 0]);
        assert_eq!(qs.last_viewpoint(), Some(vp));
        // Camera past the far quad: the order flips.
        assert_eq!(qs.resort([8.0, 8.0, 30.0], vp), [0, 1, 2, 2, 3, 0, 4, 5, 6, 6, 7, 4]);
    }

    #[test]
    fn a_single_quad_needs_no_sort() {
        let (indices, positions) = two_quads();
        assert!(QuadSort::new(&indices[..6], |i| positions[i as usize]).is_none());
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
}
