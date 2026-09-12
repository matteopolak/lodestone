/// A CPU block-entity mesh: part-local vertices plus the part hierarchy needed
/// to rebuild transforms with per-part overrides each frame.
///
/// The hierarchy is kept here rather than in a [`crate::entity_anim::Skeleton`]
/// because `Skeleton` animates by *slot* — `head`, `right_arm`, the limb table —
/// and classifies anything without those names as `AnimFamily::Static`. A chest
/// is `Static` by that rule and would pose with a permanently shut lid. What a
/// block entity needs instead is a direct per-part pose override, which is a
/// different (and much smaller) mechanism, so it lives here.
#[derive(Debug, Clone)]
pub struct BlockEntityMesh {
    /// Four vertices per quad, part-local (no pose folded in).
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad, wound from each quad's baked outward normal.
    pub indices: Vec<u32>,
    /// One index sub-range per part, in bake (pre-order) order.
    pub parts: Vec<PartRange>,
    /// Part names, parallel to `parts`.
    pub part_names: Vec<String>,
    /// Parent index per part (`None` for the root); always less than the part's
    /// own index, so one forward pass composes the chain.
    pub part_parents: Vec<Option<usize>>,
    /// The authored pose per part; an override copies and adjusts it.
    pub part_rest: Vec<PartPose>,
    /// Local AABB minimum at rest, in block units.
    pub local_min: Vec3,
    /// Local AABB maximum at rest, in block units.
    pub local_max: Vec3,
}

impl BlockEntityMesh {
    /// Bakes a model definition into a renderable block-entity mesh.
    #[must_use]
    pub fn from_model(def: &EntityModelDef) -> Self {
        let baked = bake_entity_parts(def);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::with_capacity(baked.len());
        let mut part_names = Vec::with_capacity(baked.len());
        let mut part_parents = Vec::with_capacity(baked.len());
        let mut part_rest = Vec::with_capacity(baked.len());

        for part in &baked {
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push(PartRange {
                index_start,
                index_count: indices.len() as u32 - index_start,
                vertex_start,
                vertex_count: vertices.len() as u32 - vertex_start,
            });
            part_names.push(part.name.clone());
            part_parents.push(part.parent);
            part_rest.push(part.rest);
        }

        let mut mesh = BlockEntityMesh {
            vertices,
            indices,
            parts,
            part_names,
            part_parents,
            part_rest,
            local_min: Vec3::ZERO,
            local_max: Vec3::ZERO,
        };
        // The rest AABB, measured through the same transform chain the draw uses
        // (`part_transforms` with no overrides) rather than from the texel
        // extents restated by hand.
        let rest = mesh.part_transforms(Mat4::IDENTITY, &[]);
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for (part_index, part) in baked.iter().enumerate() {
            for quad in &part.quads {
                for p in &quad.positions {
                    let posed = rest[part_index].transform_point3(Vec3::from(*p));
                    min = min.min(posed);
                    max = max.max(posed);
                }
            }
        }
        if mesh.indices.is_empty() {
            min = Vec3::ZERO;
            max = Vec3::ZERO;
        }
        mesh.local_min = min;
        mesh.local_max = max;
        mesh
    }

    /// The index of a part by name.
    #[must_use]
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.part_names.iter().position(|n| n == name)
    }

    /// Number of quads in the mesh.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }

    /// Composes one world matrix per part: `placement · chain(parent) · pose`,
    /// with `overrides` replacing the authored pose of the parts it names.
    ///
    /// `overrides` is `(part index, pose)`; a part named twice takes the last
    /// entry. The chain uses [`lodestone_assets::entity::Affine::of_pose`] rather
    /// than a local `rotationZYX`, so the rotation order can never drift from
    /// the one the bake itself used — a second implementation of `rotZYX` is
    /// exactly how a lid ends up hinging about the wrong axis with every unit
    /// test still green.
    #[must_use]
    pub fn part_transforms(&self, placement: Mat4, overrides: &[(usize, PartPose)]) -> Vec<Mat4> {
        use lodestone_assets::entity::Affine;
        let mut poses = self.part_rest.clone();
        for (index, pose) in overrides {
            if let Some(slot) = poses.get_mut(*index) {
                *slot = *pose;
            }
        }
        let mut chain: Vec<Affine> = Vec::with_capacity(poses.len());
        for (index, pose) in poses.iter().enumerate() {
            let local = Affine::of_pose(pose);
            let world = match self.part_parents[index] {
                // `parent < index` is guaranteed by `bake_entity_parts`'
                // pre-order, so the parent's composed transform already exists.
                Some(parent) => chain[parent].compose(&local),
                None => local,
            };
            chain.push(world);
        }
        chain
            .into_iter()
            .map(|a| placement * affine_to_mat4(&a))
            .collect()
    }
}

/// Widens an [`Affine`](lodestone_assets::entity::Affine) (row-major 3×3 plus a
/// translation) into a column-major [`Mat4`].
///
/// `Affine::m[i][j]` is *row* `i`, *column* `j`; `Mat4::from_cols_array_2d`
/// takes **columns**. The transpose here is the whole point — feeding the rows
/// in as columns yields the inverse rotation, which for a chest lid looks like
/// the lid opening *into* the chest and is easy to mistake for a sign error in
/// `chest_lid_x_rot`.
fn affine_to_mat4(a: &lodestone_assets::entity::Affine) -> Mat4 {
    Mat4::from_cols_array_2d(&[
        [a.m[0][0], a.m[1][0], a.m[2][0], 0.0],
        [a.m[0][1], a.m[1][1], a.m[2][1], 0.0],
        [a.m[0][2], a.m[1][2], a.m[2][2], 0.0],
        [a.t[0], a.t[1], a.t[2], 1.0],
    ])
}
use glam::{Mat4, Vec3};
use lodestone_assets::entity::{EntityModelDef, PartPose, bake_entity_parts};

use crate::entity::{PartRange, push_part_quads};
use crate::models::ModelVertex;

