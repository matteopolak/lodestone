//! Progressive mining-crack overlay geometry.
//!
//! When a block is being broken the mining state machine computes a *destroy
//! stage* `0..=9` ([`Mining::destroy_stage`] in `lodestone-game`); vanilla draws
//! the matching `destroy_stage_N` sprite over the block as a second, depth-biased
//! pass. The producer half of that — stitching those ten sprites into the block
//! atlas — lives in [`BlockModels`](crate::BlockModels); this module is the
//! geometry half.
//!
//! # Follow the model, not a cube
//!
//! The crack must trace the block's **actual** shape. Now that geometry is
//! model-driven, cracking a slab or stair with a synthetic full cube would float
//! a cracked box in the air above the step. So the crack pass re-uses the
//! block's own baked quads ([`BakedQuad`]) and only rewrites their UVs: the
//! position of every crack vertex is exactly a model vertex, so the overlay is
//! the block's silhouette by construction.
//!
//! # UV projection
//!
//! `destroy_stage_N` is a single square sprite that vanilla projects in
//! block space, so the crack lines up seamlessly where two faces meet. We
//! reproduce that by taking each vertex's two in-face block-local coordinates
//! (the axes orthogonal to the face normal, each already in `0.0..=1.0` for a
//! unit block) and mapping that unit square onto the sprite's atlas rect. The
//! result tiles the crack across the whole block regardless of how the model
//! subdivides its faces.

use lodestone_assets::BakedQuad;

/// A single crack-overlay vertex: a position coincident with a block-model
/// vertex and a UV into a `destroy_stage_N` atlas rect.
///
/// Laid out for direct upload to a vertex buffer (`repr(C)`, `Pod`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CrackVertex {
    /// World-space position (block-local position plus the draw origin).
    pub position: [f32; 3],
    /// Normalised atlas UV inside the destroy-stage sprite rect.
    pub uv: [f32; 2],
}

/// Crack-overlay geometry for one block: a vertex list and a `u32` index list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CrackMesh {
    /// The crack vertices.
    pub vertices: Vec<CrackVertex>,
    /// Triangle indices into [`vertices`](Self::vertices).
    pub indices: Vec<u32>,
}

impl CrackMesh {
    /// Whether the mesh has no geometry (nothing to draw).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// Build crack-overlay geometry for a block whose model is `quads`, textured
/// with the destroy-stage sprite occupying atlas rect `rect` (`[u0, v0, u1, v1]`)
/// and translated to world position `origin`.
///
/// Every input quad contributes one crack quad at the identical position, so the
/// overlay follows the model exactly (see the module docs). Returns an empty
/// mesh for an empty model.
#[must_use]
pub fn build_crack_mesh(quads: &[BakedQuad], rect: [f32; 4], origin: [f32; 3]) -> CrackMesh {
    let mut mesh = CrackMesh::default();
    for quad in quads {
        let base = mesh.vertices.len() as u32;
        let (u_axis, v_axis) = face_plane_axes(&quad.positions);
        for p in &quad.positions {
            let a = p[u_axis];
            let b = p[v_axis];
            let u = rect[0] + (rect[2] - rect[0]) * a;
            let v = rect[1] + (rect[3] - rect[1]) * b;
            mesh.vertices.push(CrackVertex {
                position: [p[0] + origin[0], p[1] + origin[1], p[2] + origin[2]],
                uv: [u, v],
            });
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    mesh
}

/// The two block-local axes that span a quad's plane, chosen as the axes
/// orthogonal to the quad's dominant normal component. Returns `(u_axis,
/// v_axis)` as indices into a `[x, y, z]` position.
fn face_plane_axes(positions: &[[f32; 3]; 4]) -> (usize, usize) {
    let e1 = sub(positions[1], positions[0]);
    let e2 = sub(positions[2], positions[0]);
    let n = cross(e1, e2);
    let (nx, ny, nz) = (n[0].abs(), n[1].abs(), n[2].abs());
    if nx >= ny && nx >= nz {
        // Facing X: the face spans the Z and Y axes.
        (2, 1)
    } else if ny >= nx && ny >= nz {
        // Facing Y: the face spans the X and Z axes.
        (0, 2)
    } else {
        // Facing Z: the face spans the X and Y axes.
        (0, 1)
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_assets::Direction;

    fn quad(positions: [[f32; 3]; 4]) -> BakedQuad {
        BakedQuad {
            positions,
            uvs: [[0.0; 2]; 4],
            direction: Direction::Up,
            cullface: None,
            tint_index: None,
            shade: true,
            layer: 0,
            anim: 0,
        }
    }

    // A full-cube top face at y = 1.
    fn cube_top() -> BakedQuad {
        quad([
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, 0.0],
        ])
    }

    // A bottom-slab top face: identical footprint, but the surface sits at
    // y = 0.5 rather than y = 1.
    fn slab_top() -> BakedQuad {
        quad([
            [0.0, 0.5, 0.0],
            [0.0, 0.5, 1.0],
            [1.0, 0.5, 1.0],
            [1.0, 0.5, 0.0],
        ])
    }

    #[test]
    fn empty_model_yields_empty_mesh() {
        let mesh = build_crack_mesh(&[], [0.0, 0.0, 1.0, 1.0], [0.0; 3]);
        assert!(mesh.is_empty());
        assert!(mesh.vertices.is_empty());
    }

    #[test]
    fn positions_follow_model_geometry_not_a_full_cube() {
        // The crack over a slab must sit on the slab surface (y = 0.5), never
        // synthesise a full-cube face at y = 1. Negative control: a full-cube
        // overlay would emit y = 1 here, which we assert is absent.
        let mesh = build_crack_mesh(&[slab_top()], [0.0, 0.0, 1.0, 1.0], [0.0; 3]);
        assert_eq!(mesh.vertices.len(), 4);
        assert!(
            mesh.vertices.iter().all(|v| v.position[1] == 0.5),
            "crack should sit on the slab surface y=0.5, got {:?}",
            mesh.vertices.iter().map(|v| v.position[1]).collect::<Vec<_>>()
        );
        assert!(
            mesh.vertices.iter().all(|v| v.position[1] != 1.0),
            "crack must not float at the full-cube height y=1"
        );
    }

    #[test]
    fn uv_maps_face_local_square_onto_stage_rect() {
        let rect = [0.2, 0.4, 0.3, 0.5];
        let mesh = build_crack_mesh(&[cube_top()], rect, [0.0; 3]);
        // Corner (x=0, z=0) -> rect min; corner (x=1, z=1) -> rect max.
        let at = |x: f32, z: f32| {
            mesh.vertices
                .iter()
                .find(|v| v.position[0] == x && v.position[2] == z)
                .map(|v| v.uv)
                .expect("corner present")
        };
        assert_eq!(at(0.0, 0.0), [rect[0], rect[1]]);
        assert_eq!(at(1.0, 1.0), [rect[2], rect[3]]);
    }

    #[test]
    fn distinct_stage_rects_give_distinct_uvs() {
        let stage0 = build_crack_mesh(&[cube_top()], [0.0, 0.0, 0.1, 0.1], [0.0; 3]);
        let stage9 = build_crack_mesh(&[cube_top()], [0.5, 0.5, 0.6, 0.6], [0.0; 3]);
        assert_ne!(stage0.vertices[0].uv, stage9.vertices[0].uv);
    }

    #[test]
    fn origin_translates_every_position() {
        let origin = [5.0, 64.0, -3.0];
        let mesh = build_crack_mesh(&[cube_top()], [0.0, 0.0, 1.0, 1.0], origin);
        assert!(mesh.vertices.iter().all(|v| {
            v.position[1] == 64.0 + 1.0 && v.position[0] >= 5.0 && v.position[2] >= -3.0
        }));
    }

    #[test]
    fn two_triangles_per_quad() {
        let mesh = build_crack_mesh(&[cube_top()], [0.0, 0.0, 1.0, 1.0], [0.0; 3]);
        assert_eq!(mesh.indices, vec![0, 1, 2, 0, 2, 3]);
    }
}
