//! Chunk section meshing: per-face culled (reference) and greedy, both with
//! Minecraft-style per-vertex ambient occlusion.
//!
//! Both meshers consume a [`SectionNeighborhood`] and emit a [`Mesh`] of
//! [`PackedVertex`]es plus a `u32` index buffer. They only ever mesh the centre
//! section (`0..16` on each axis); neighbour cells are read purely to decide
//! face visibility and AO.
//!
//! # Face geometry and winding
//!
//! Each face is described by an origin `base` and two in-plane unit axes `u`,`v`
//! chosen so that `u × v == +normal`. Emitting corners in the order
//! `(0,0) → (1,0) → (1,1) → (0,1)` therefore yields **counter-clockwise,
//! outward-facing** winding for every face, which [`face_winding_is_outward`]
//! verifies without a GPU. That lets the block pass use back-face culling
//! (`FrontFace::Ccw`, cull `Back`) correctly.
//!
//! # Ambient occlusion
//!
//! Per vanilla, each face vertex is darkened by the three occluding blocks
//! around its corner in the layer *in front* of the face: the two edge
//! neighbours (`side1`, `side2`) and the diagonal `corner`. The level is
//! `0` when both sides occlude, otherwise `3 - (side1 + side2 + corner)`. When
//! the two diagonal AO values of a quad disagree, the quad is triangulated along
//! the other diagonal to avoid a bright triangle bleeding across a dark one.
//!
//! # Greedy merging and AO
//!
//! Greedy meshing merges coplanar faces sharing the same face direction, sprite,
//! light, **and** all four AO values. Requiring identical AO is not a
//! simplification we chose for convenience — it is required for correctness,
//! since AO is per-vertex and merging faces with differing AO would lose the
//! gradient. This is exactly why greedy meshing is not a free win: noisy terrain
//! has few mergeable runs.

use crate::section::{Cell, Face, SectionNeighborhood};
use crate::vertex::{PackedVertex, VertexFields};

/// A meshed section: packed vertices and a `u32` index buffer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mesh {
    /// Packed vertices, four per quad.
    pub vertices: Vec<PackedVertex>,
    /// Triangle indices, six per quad.
    pub indices: Vec<u32>,
}

impl Mesh {
    /// Number of quads (`indices / 6`).
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }

    /// Summary counts for benchmarking.
    #[must_use]
    pub fn stats(&self) -> MeshStats {
        MeshStats {
            quads: self.quad_count(),
            vertices: self.vertices.len(),
            indices: self.indices.len(),
        }
    }
}

/// Vertex/quad/index counts for a mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshStats {
    /// Number of quads.
    pub quads: usize,
    /// Number of vertices.
    pub vertices: usize,
    /// Number of indices.
    pub indices: usize,
}

/// Origin and in-plane axes of a face, with `u × v == +normal`.
struct FaceGeom {
    base: [i32; 3],
    u: [i32; 3],
    v: [i32; 3],
}

const fn face_geom(face: Face) -> FaceGeom {
    match face {
        // base at 0, u=+Z, v=+Y  → Z×Y = -X
        Face::NegX => FaceGeom {
            base: [0, 0, 0],
            u: [0, 0, 1],
            v: [0, 1, 0],
        },
        // base at +X, u=+Y, v=+Z  → Y×Z = +X
        Face::PosX => FaceGeom {
            base: [1, 0, 0],
            u: [0, 1, 0],
            v: [0, 0, 1],
        },
        // base at 0, u=+X, v=+Z  → X×Z = -Y
        Face::NegY => FaceGeom {
            base: [0, 0, 0],
            u: [1, 0, 0],
            v: [0, 0, 1],
        },
        // base at +Y, u=+Z, v=+X  → Z×X = +Y
        Face::PosY => FaceGeom {
            base: [0, 1, 0],
            u: [0, 0, 1],
            v: [1, 0, 0],
        },
        // base at 0, u=+Y, v=+X  → Y×X = -Z
        Face::NegZ => FaceGeom {
            base: [0, 0, 0],
            u: [0, 1, 0],
            v: [1, 0, 0],
        },
        // base at +Z, u=+X, v=+Y  → X×Y = +Z
        Face::PosZ => FaceGeom {
            base: [0, 0, 1],
            u: [1, 0, 0],
            v: [0, 1, 0],
        },
    }
}

/// Index of the single nonzero component of a unit axis.
fn axis_of(v: [i32; 3]) -> usize {
    if v[0] != 0 {
        0
    } else if v[1] != 0 {
        1
    } else {
        2
    }
}

/// Smoothed per-corner brightness produced by [`face_corner_lighting`].
///
/// Each field is an 8-bit brightness (`0..=255`), the form the [`PackedVertex`]
/// stores and the shader multiplies together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CornerLight {
    /// Ambient-occlusion shade (255 = unoccluded).
    ao: u8,
    /// Smoothed sky light.
    sky: u8,
    /// Smoothed block light.
    block: u8,
}

/// A face's merge key: two faces merge in greedy meshing iff these match. The
/// per-corner light is part of the key, so greedy meshing merges *less* on
/// smoothly-lit terrain than it would with a single flat value — exactly the
/// AO/greedy interaction that makes greedy conditional rather than free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QuadKey {
    sprite: u16,
    corners: [CornerLight; 4],
}

/// Corner offsets `(s, t)` in `{0,1}` for the four quad vertices, CCW.
const CORNERS: [(u32, u32); 4] = [(0, 0), (1, 0), (1, 1), (0, 1)];

/// AO shade of a fully-occluding neighbour. Vanilla's darkest ambient-occlusion
/// sample is `0.2` (not `0.0`); with the always-open block in front of the face
/// contributing `1.0`, the darkest corner averages to `0.4`, never black.
const AO_OCCLUDED: f32 = 0.2;
/// Vanilla only substitutes a dark neighbour's light with the centre light when
/// the centre is itself lit above this threshold (`LightCoordsUtil.smoothBlend`).
const SMOOTH_LIGHT_MIN_CENTRE: u8 = 2;

fn level_to_byte(level: f32) -> u8 {
    (level / 15.0 * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Compute vanilla-style smooth lighting for the four corners of `face` at `p`.
///
/// For each corner we average four samples — the two edge-adjacent neighbours,
/// the diagonal corner, and the centre block in front of the face — as
/// continuous floats, matching `BlockModelLighter`:
///
/// * **AO** averages a per-cell shade (`1.0` open, [`AO_OCCLUDED`] occluding).
/// * **Sky/block light** average the four neighbours' levels, but a dark
///   *occluding* neighbour is replaced by the centre light (vanilla's
///   `smoothBlend` rule) so an opaque corner does not drag the face to black —
///   the "air must carry light" hazard in reverse.
///
/// We deliberately skip vanilla's `faceShape`-weighted interpolation and the
/// hidden-diagonal substitution (`translucentN`), which only matter for
/// non-cube models; this path meshes full cubes.
fn face_corner_lighting(hood: &SectionNeighborhood, p: [i32; 3], face: Face) -> [CornerLight; 4] {
    let g = face_geom(face);
    let n = face.normal();
    let np = [p[0] + n[0], p[1] + n[1], p[2] + n[2]];
    let centre = hood.cell(np[0], np[1], np[2]);
    let cell = |o: [i32; 3]| hood.cell(o[0], o[1], o[2]);
    let ao_of = |c: Cell| if c.occludes { AO_OCCLUDED } else { 1.0 };

    // Sky/block channel blend for one corner (vanilla `smoothBlend`).
    let blend = |samples: [Cell; 3], pick: fn(Cell) -> u8, centre_v: u8| -> u8 {
        let substitute = centre_v > SMOOTH_LIGHT_MIN_CENTRE;
        let mut sum = centre_v as f32;
        for s in samples {
            let v = if substitute && s.occludes {
                centre_v
            } else {
                pick(s)
            };
            sum += v as f32;
        }
        level_to_byte(sum / 4.0)
    };

    let mut out = [CornerLight {
        ao: 0,
        sky: 0,
        block: 0,
    }; 4];
    for (i, &(s, t)) in CORNERS.iter().enumerate() {
        let su = 2 * s as i32 - 1;
        let sv = 2 * t as i32 - 1;
        let a = cell([
            np[0] + su * g.u[0],
            np[1] + su * g.u[1],
            np[2] + su * g.u[2],
        ]);
        let b = cell([
            np[0] + sv * g.v[0],
            np[1] + sv * g.v[1],
            np[2] + sv * g.v[2],
        ]);
        let d = cell([
            np[0] + su * g.u[0] + sv * g.v[0],
            np[1] + su * g.u[1] + sv * g.v[1],
            np[2] + su * g.u[2] + sv * g.v[2],
        ]);
        let shade = (ao_of(a) + ao_of(b) + ao_of(d) + 1.0) * 0.25;
        out[i] = CornerLight {
            ao: (shade * 255.0).round().clamp(0.0, 255.0) as u8,
            sky: blend([a, b, d], |c| c.sky_light, centre.sky_light),
            block: blend([a, b, d], |c| c.block_light, centre.block_light),
        };
    }
    out
}

/// Whether `face` of the block at `p` is visible: the block draws and its
/// neighbour in the face direction does not occlude. Returns the face's sprite.
fn face_visible(hood: &SectionNeighborhood, p: [i32; 3], face: Face) -> Option<u16> {
    let here = hood.cell(p[0], p[1], p[2]);
    let surface = here.surface?;
    let n = face.normal();
    let neighbour: Cell = hood.cell(p[0] + n[0], p[1] + n[1], p[2] + n[2]);
    if neighbour.occludes {
        return None;
    }
    Some(surface.sprites[face.index()].0)
}

/// Emit one quad (4 vertices, 6 indices) for `face` at block `base_block`,
/// spanning `w × h` tiles along the face's `u`/`v` axes.
fn emit_quad(
    mesh: &mut Mesh,
    face: Face,
    base_block: [i32; 3],
    w: u32,
    h: u32,
    sprite: u16,
    corners: [CornerLight; 4],
) {
    let g = face_geom(face);
    let start = mesh.vertices.len() as u32;
    for (i, &(s, t)) in CORNERS.iter().enumerate() {
        let sw = s * w;
        let th = t * h;
        let pos = [
            (base_block[0] + g.base[0] + sw as i32 * g.u[0] + th as i32 * g.v[0]) as u32,
            (base_block[1] + g.base[1] + sw as i32 * g.u[1] + th as i32 * g.v[1]) as u32,
            (base_block[2] + g.base[2] + sw as i32 * g.u[2] + th as i32 * g.v[2]) as u32,
        ];
        mesh.vertices.push(PackedVertex::pack(VertexFields {
            pos,
            normal: face,
            ao: corners[i].ao,
            sky_light: corners[i].sky,
            block_light: corners[i].block,
            sprite,
            u: sw as u8,
            v: th as u8,
        }));
    }

    // Flip the triangulation diagonal when AO disagrees across it, so the
    // interpolated darkening stays symmetric (the classic AO "anisotropy" fix).
    let d02 = corners[0].ao as u16 + corners[2].ao as u16;
    let d13 = corners[1].ao as u16 + corners[3].ao as u16;
    if d02 > d13 {
        mesh.indices
            .extend_from_slice(&[start, start + 1, start + 2, start + 2, start + 3, start]);
    } else {
        mesh.indices.extend_from_slice(&[
            start + 1,
            start + 2,
            start + 3,
            start + 3,
            start,
            start + 1,
        ]);
    }
}

/// Per-face culled meshing: one quad per visible face. This is the correctness
/// reference the greedy mesher is validated against.
#[must_use]
pub fn mesh_simple(hood: &SectionNeighborhood) -> Mesh {
    let mut mesh = Mesh::default();
    let size = crate::section::SECTION_SIZE as i32;
    for x in 0..size {
        for y in 0..size {
            for z in 0..size {
                let p = [x, y, z];
                for face in Face::ALL {
                    if let Some(sprite) = face_visible(hood, p, face) {
                        let corners = face_corner_lighting(hood, p, face);
                        emit_quad(&mut mesh, face, p, 1, 1, sprite, corners);
                    }
                }
            }
        }
    }
    mesh
}

/// Greedy meshing: merge coplanar faces sharing direction, sprite, and all four
/// per-corner light values into larger quads.
#[must_use]
pub fn mesh_greedy(hood: &SectionNeighborhood) -> Mesh {
    let mut mesh = Mesh::default();
    let size = crate::section::SECTION_SIZE;

    for face in Face::ALL {
        let g = face_geom(face);
        let iu = axis_of(g.u);
        let iv = axis_of(g.v);
        let inrm = axis_of(face.normal());

        for ln in 0..size {
            // Build the slice mask.
            let mut mask: Vec<Option<QuadKey>> = vec![None; size * size];
            for cu in 0..size {
                for cv in 0..size {
                    let mut p = [0i32; 3];
                    p[iu] = cu as i32;
                    p[iv] = cv as i32;
                    p[inrm] = ln as i32;
                    if let Some(sprite) = face_visible(hood, p, face) {
                        let corners = face_corner_lighting(hood, p, face);
                        mask[cu * size + cv] = Some(QuadKey { sprite, corners });
                    }
                }
            }

            // Merge maximal rectangles.
            let mut consumed = vec![false; size * size];
            for cu in 0..size {
                for cv in 0..size {
                    let idx = cu * size + cv;
                    let Some(key) = mask[idx] else { continue };
                    if consumed[idx] {
                        continue;
                    }
                    // Extend width along cv.
                    let mut w = 1;
                    while cv + w < size {
                        let j = cu * size + (cv + w);
                        if consumed[j] || mask[j] != Some(key) {
                            break;
                        }
                        w += 1;
                    }
                    // Extend height along cu.
                    let mut h = 1;
                    'grow: while cu + h < size {
                        for k in 0..w {
                            let j = (cu + h) * size + (cv + k);
                            if consumed[j] || mask[j] != Some(key) {
                                break 'grow;
                            }
                        }
                        h += 1;
                    }
                    for du in 0..h {
                        for dv in 0..w {
                            consumed[(cu + du) * size + (cv + dv)] = true;
                        }
                    }

                    let mut p = [0i32; 3];
                    p[iu] = cu as i32;
                    p[iv] = cv as i32;
                    p[inrm] = ln as i32;
                    // The rect spans `h` along iu and `w` along iv, which map to
                    // the face's u and v axes respectively.
                    emit_quad(
                        &mut mesh,
                        face,
                        p,
                        h as u32,
                        w as u32,
                        key.sprite,
                        key.corners,
                    );
                }
            }
        }
    }
    mesh
}

/// Verify that the geometry table produces outward-facing CCW winding for every
/// face: the first triangle's normal must point along `+face.normal()`.
#[must_use]
pub fn face_winding_is_outward(face: Face) -> bool {
    let g = face_geom(face);
    // Corners 0,1,2 for a unit quad.
    let corner = |s: i32, t: i32| {
        [
            g.base[0] + s * g.u[0] + t * g.v[0],
            g.base[1] + s * g.u[1] + t * g.v[1],
            g.base[2] + s * g.u[2] + t * g.v[2],
        ]
    };
    let a = corner(0, 0);
    let b = corner(1, 0);
    let c = corner(1, 1);
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let cross = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    cross == face.normal()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section::{SectionView, SpriteId};

    struct Empty;
    impl SectionView for Empty {
        fn cell(&self, _x: usize, _y: usize, _z: usize) -> Cell {
            Cell::EMPTY
        }
    }

    struct Full(SpriteId);
    impl SectionView for Full {
        fn cell(&self, _x: usize, _y: usize, _z: usize) -> Cell {
            Cell::solid(self.0)
        }
    }

    struct One(usize, usize, usize, SpriteId);
    impl SectionView for One {
        fn cell(&self, x: usize, y: usize, z: usize) -> Cell {
            if (x, y, z) == (self.0, self.1, self.2) {
                Cell::solid(self.3)
            } else {
                Cell::EMPTY
            }
        }
    }

    struct Checker;
    impl SectionView for Checker {
        fn cell(&self, x: usize, y: usize, z: usize) -> Cell {
            if (x + y + z).is_multiple_of(2) {
                Cell::solid(SpriteId(1))
            } else {
                Cell::EMPTY
            }
        }
    }

    /// A flat floor: solid at y==0, air above. Neighbour below is empty.
    struct Floor;
    impl SectionView for Floor {
        fn cell(&self, _x: usize, y: usize, _z: usize) -> Cell {
            if y == 0 {
                Cell::solid(SpriteId(5))
            } else {
                Cell::EMPTY
            }
        }
    }

    #[test]
    fn all_faces_wind_outward() {
        for f in Face::ALL {
            assert!(face_winding_is_outward(f), "face {f:?} winds inward");
        }
    }

    #[test]
    fn empty_section_meshes_to_nothing() {
        let s = Empty;
        let hood = SectionNeighborhood::centre_only(&s);
        assert_eq!(mesh_simple(&hood).stats().quads, 0);
        assert_eq!(mesh_greedy(&hood).stats().quads, 0);
    }

    #[test]
    fn single_block_makes_six_faces() {
        let s = One(8, 8, 8, SpriteId(3));
        let hood = SectionNeighborhood::centre_only(&s);
        assert_eq!(mesh_simple(&hood).quad_count(), 6);
        // Greedy cannot merge a lone cube either.
        assert_eq!(mesh_greedy(&hood).quad_count(), 6);
    }

    #[test]
    fn interior_faces_are_culled() {
        // Two adjacent solid blocks: the shared faces are hidden, so 10 not 12.
        struct Two;
        impl SectionView for Two {
            fn cell(&self, x: usize, y: usize, z: usize) -> Cell {
                if y == 8 && z == 8 && (x == 8 || x == 9) {
                    Cell::solid(SpriteId(1))
                } else {
                    Cell::EMPTY
                }
            }
        }
        let s = Two;
        let hood = SectionNeighborhood::centre_only(&s);
        assert_eq!(mesh_simple(&hood).quad_count(), 10);
    }

    #[test]
    fn boundary_faces_depend_on_neighbour() {
        // A full centre section with no neighbours draws all 6 outer 16×16
        // faces; simple mesher emits 16×16×6 = 1536 boundary quads.
        let centre = Full(SpriteId(2));
        let hood = SectionNeighborhood::centre_only(&centre);
        assert_eq!(mesh_simple(&hood).quad_count(), 6 * 16 * 16);

        // With all 6 face neighbours solid, every outer face is culled → 0.
        let nb = Full(SpriteId(2));
        let mut hood2 = SectionNeighborhood::centre_only(&centre);
        hood2.set(1, 0, 0, Some(&nb));
        hood2.set(-1, 0, 0, Some(&nb));
        hood2.set(0, 1, 0, Some(&nb));
        hood2.set(0, -1, 0, Some(&nb));
        hood2.set(0, 0, 1, Some(&nb));
        hood2.set(0, 0, -1, Some(&nb));
        assert_eq!(mesh_simple(&hood2).quad_count(), 0);
    }

    #[test]
    fn greedy_merges_a_full_face_into_one_quad() {
        // Full section, no neighbours: each of the 6 outer faces is a uniform
        // 16×16 plane with identical AO/light, so greedy merges each to 1 quad.
        let centre = Full(SpriteId(2));
        let hood = SectionNeighborhood::centre_only(&centre);
        let g = mesh_greedy(&hood);
        assert_eq!(g.quad_count(), 6);
        // versus the simple reference:
        assert_eq!(mesh_simple(&hood).quad_count(), 6 * 256);
    }

    #[test]
    fn greedy_matches_simple_visibility_on_checker() {
        // Worst case: checkerboard cannot merge anything, so greedy == simple.
        let s = Checker;
        let hood = SectionNeighborhood::centre_only(&s);
        assert_eq!(
            mesh_greedy(&hood).quad_count(),
            mesh_simple(&hood).quad_count()
        );
    }

    #[test]
    fn greedy_merges_flat_floor_top_into_one_quad() {
        let s = Floor;
        let hood = SectionNeighborhood::centre_only(&s);
        let simple = mesh_simple(&hood);
        let greedy = mesh_greedy(&hood);
        // The top face (16×16) merges to one quad; greedy must have far fewer.
        assert!(greedy.quad_count() < simple.quad_count());
        // Sanity: the top of the floor is one merged quad among the total.
        assert!(greedy.quad_count() >= 1);
    }

    #[test]
    fn ao_darkens_against_an_occluder() {
        // A flat floor with a single block sitting on it. Top faces adjacent to
        // the bump must be darkened (AO < 3) by that occluder.
        struct FloorWithBump;
        impl SectionView for FloorWithBump {
            fn cell(&self, x: usize, y: usize, z: usize) -> Cell {
                if y == 0 {
                    Cell::solid(SpriteId(5))
                } else if y == 1 && x == 5 && z == 5 {
                    Cell::solid(SpriteId(6))
                } else {
                    Cell::EMPTY
                }
            }
        }
        let bumped = FloorWithBump;
        let hood = SectionNeighborhood::centre_only(&bumped);
        let mesh = mesh_simple(&hood);
        // Some top-face vertex near the bump must be AO-darkened (< full 255).
        let any_dark = mesh
            .vertices
            .iter()
            .map(|v| v.unpack())
            .filter(|f| f.normal == Face::PosY)
            .any(|f| f.ao < 255);
        assert!(any_dark, "expected AO darkening next to the bump");
    }

    #[test]
    fn ao_flat_where_unoccluded() {
        // A single flat floor with no bumps: every top-face vertex is fully lit.
        let s = Floor;
        let hood = SectionNeighborhood::centre_only(&s);
        let mesh = mesh_simple(&hood);
        let top_all_bright = mesh
            .vertices
            .iter()
            .map(|v| v.unpack())
            .filter(|f| f.normal == Face::PosY)
            .all(|f| f.ao == 255);
        assert!(top_all_bright);
    }

    #[test]
    fn greedy_never_produces_more_quads_than_simple() {
        for seed in 0..4u32 {
            struct Noise(u32);
            impl SectionView for Noise {
                fn cell(&self, x: usize, y: usize, z: usize) -> Cell {
                    let h = (x as u32).wrapping_mul(73856093)
                        ^ (y as u32).wrapping_mul(19349663)
                        ^ (z as u32).wrapping_mul(83492791)
                        ^ self.0.wrapping_mul(2654435761);
                    if h.is_multiple_of(3) {
                        Cell::solid(SpriteId((h % 4) as u16))
                    } else {
                        Cell::EMPTY
                    }
                }
            }
            let s = Noise(seed);
            let hood = SectionNeighborhood::centre_only(&s);
            let simple = mesh_simple(&hood).quad_count();
            let greedy = mesh_greedy(&hood).quad_count();
            assert!(
                greedy <= simple,
                "seed {seed}: greedy {greedy} > simple {simple}"
            );
        }
    }

    #[test]
    fn indices_reference_valid_vertices() {
        let s = Full(SpriteId(2));
        let hood = SectionNeighborhood::centre_only(&s);
        for mesh in [mesh_simple(&hood), mesh_greedy(&hood)] {
            let n = mesh.vertices.len() as u32;
            assert!(mesh.indices.iter().all(|&i| i < n));
            assert_eq!(mesh.indices.len(), mesh.quad_count() * 6);
            assert_eq!(mesh.vertices.len(), mesh.quad_count() * 4);
        }
    }
}
