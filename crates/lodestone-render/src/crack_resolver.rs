//! Resolving a target block into ready-to-draw crack geometry.
//!
//! The crack pass needs a block's baked quads to trace its shape, but the live
//! renderer does not keep [`BlockModels`](crate::BlockModels) resident past
//! atlas construction. [`CrackResolver`] captures exactly the two things the
//! crack pass needs — each state's quads and the ten `destroy_stage_N` sprite
//! rects — so a target `(state_id, stage, position)` becomes a [`CrackMesh`]
//! without holding the whole model set (and its atlas image) alive.
//!
//! The per-state quad snapshot is now also the general **"block geometry after
//! `BlockModels` is dropped"** seam — see [`CrackResolver::state_quads`], which
//! the moving-block-model path (falling blocks, and piston heads when they land)
//! reads for exactly the reason the crack pass does. The type keeps its name
//! because the crack pass is still its only *owner*; a reader looking for block
//! geometry at draw time should look here.

use lodestone_assets::BakedQuad;
use lodestone_data::block_states::StateId;

use crate::block_models::{CRACK_STAGE_COUNT, ITEM_FRAME_SLOTS, item_frame_slot};
use crate::crack::{CrackMesh, build_crack_mesh};

/// Turns a target block state + destroy stage into crack-overlay geometry.
#[derive(Clone, Debug)]
pub struct CrackResolver {
    /// Per-state baked quads, indexed by `state_id` (empty for air / unknown).
    quads: Vec<Vec<BakedQuad>>,
    /// Normalised atlas rects of the ten `destroy_stage_N` sprites.
    stage_rects: [[f32; 4]; CRACK_STAGE_COUNT],
    /// The four item-frame body models, indexed by
    /// [`item_frame_slot`](crate::block_models::item_frame_slot). Carried here
    /// for the same reason `quads` is — the frame is a block model with no
    /// state id, and it has to survive [`BlockModels`](crate::BlockModels)
    /// being dropped. See [`Self::item_frame_quads`].
    item_frame_quads: [Vec<BakedQuad>; ITEM_FRAME_SLOTS],
}

impl CrackResolver {
    /// Build a resolver from its parts. `quads[state_id]` is that state's baked
    /// model quads; `stage_rects[s]` is the `destroy_stage_s` atlas rect.
    ///
    /// Carries **no** item-frame geometry: a caller that wants it builds
    /// through [`Self::from_models`], or adds it with
    /// [`Self::with_item_frame_quads`]. Empty is the honest default, and it is
    /// what every existing caller of this constructor (all of them tests
    /// exercising the crack or moving-block paths) already means.
    #[must_use]
    pub fn new(quads: Vec<Vec<BakedQuad>>, stage_rects: [[f32; 4]; CRACK_STAGE_COUNT]) -> Self {
        Self {
            quads,
            stage_rects,
            item_frame_quads: std::array::from_fn(|_| Vec::new()),
        }
    }

    /// Attach the four item-frame body models to a resolver built with
    /// [`Self::new`], indexed by
    /// [`item_frame_slot`](crate::block_models::item_frame_slot).
    #[must_use]
    pub fn with_item_frame_quads(
        mut self,
        item_frame_quads: [Vec<BakedQuad>; ITEM_FRAME_SLOTS],
    ) -> Self {
        self.item_frame_quads = item_frame_quads;
        self
    }

    /// Capture the crack inputs from a [`BlockModels`](crate::BlockModels): every
    /// state's quads and the ten destroy-stage rects. Runs once at renderer
    /// setup; the model set can then be dropped.
    #[must_use]
    pub fn from_models(models: &crate::BlockModels) -> Self {
        let quads = (0..models.state_count() as u32)
            .map(|id| models.quads(id).to_vec())
            .collect();
        let mut stage_rects = [[0.0; 4]; CRACK_STAGE_COUNT];
        for (stage, rect) in stage_rects.iter_mut().enumerate() {
            if let Some(uv) = models.crack_stage_uv(stage as u8) {
                *rect = uv;
            }
        }
        Self::new(quads, stage_rects).with_item_frame_quads(std::array::from_fn(|slot| {
            let glow = slot & 0b10 != 0;
            let map = slot & 0b01 != 0;
            debug_assert_eq!(item_frame_slot(glow, map), slot);
            models.item_frame_quads(glow, map).to_vec()
        }))
    }

    /// One item-frame body's baked quads, in block-local `0.0..=1.0` space.
    ///
    /// Empty when this resolver was built by [`Self::new`] without them, and
    /// empty for a pack that ships no item-frame blockstate. Callers treat that
    /// as "draw nothing", exactly as for [`Self::state_quads`].
    #[must_use]
    pub fn item_frame_quads(&self, glow: bool, map: bool) -> &[BakedQuad] {
        self.item_frame_quads[item_frame_slot(glow, map)].as_slice()
    }

    /// One block state's baked quads, in block-local `0.0..=1.0` space — the
    /// snapshot this type already holds, exposed for callers that want the block's
    /// *own* geometry rather than a crack overlay over it.
    ///
    /// The [`StateId`] argument proves this is a built-in census state before it
    /// reaches this table. Empty therefore means that state bakes no faces, not
    /// that an arbitrary wire value happened to miss the table. Protocol-local and
    /// dynamic ids stay outside this boundary until their source resolves them.
    /// Callers should treat empty as "draw nothing", never as an error: an
    /// invisible-render-shape block legitimately has no quads, and the falling-
    /// block renderer guards on exactly that (it only draws a shape that is a real
    /// model).
    ///
    /// Unlike [`mesh_for`](Self::mesh_for) this keeps the quads' **own** UVs, tint
    /// index, shade flag and animation slot — the crack path replaces the UVs with
    /// a `destroy_stage` rect, which is why it cannot be reused for drawing the
    /// block itself.
    #[must_use]
    pub fn state_quads(&self, state_id: StateId) -> &[BakedQuad] {
        self.quads
            .get(state_id.raw() as usize)
            .map_or(&[], Vec::as_slice)
    }

    /// Build crack geometry for the validated `state_id` at destroy `stage`,
    /// translated to the block's world `origin`. Returns `None` when the stage
    /// is out of range, the state has no geometry, or the stage sprite is absent
    /// (an empty rect).
    #[must_use]
    pub fn mesh_for(&self, state_id: StateId, stage: u8, origin: [f32; 3]) -> Option<CrackMesh> {
        let quads = self.quads.get(state_id.raw() as usize)?;
        if quads.is_empty() {
            return None;
        }
        let rect = *self.stage_rects.get(usize::from(stage))?;
        // An all-zero rect means the sprite never resolved; drawing it would
        // sample the atlas origin, not a crack.
        if rect == [0.0; 4] {
            return None;
        }
        let mesh = build_crack_mesh(quads, rect, origin);
        (!mesh.is_empty()).then_some(mesh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_assets::Direction;

    fn cube_top() -> BakedQuad {
        BakedQuad {
            positions: [
                [0.0, 1.0, 0.0],
                [0.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
                [1.0, 1.0, 0.0],
            ],
            uvs: [[0.0; 2]; 4],
            direction: Direction::Up,
            cullface: None,
            tint_index: None,
            shade: true,
            layer: 0,
            anim: 0,
            sprite: 0,
        }
    }

    fn rects() -> [[f32; 4]; CRACK_STAGE_COUNT] {
        let mut r = [[0.0; 4]; CRACK_STAGE_COUNT];
        for (i, rect) in r.iter_mut().enumerate() {
            let f = i as f32 / 100.0;
            *rect = [f, f, f + 0.05, f + 0.05];
        }
        r
    }

    #[test]
    fn resolves_a_target_state_to_a_crack_mesh() {
        let resolver = CrackResolver::new(vec![vec![], vec![cube_top()]], rects());
        let state_id = StateId::new(1).expect("state one is in the built-in census");
        let mesh = resolver
            .mesh_for(state_id, 3, [10.0, 64.0, -5.0])
            .expect("state 1 has geometry and stage 3 has a sprite");
        assert_eq!(mesh.vertices.len(), 4);
        // Translated to the block origin.
        assert!(mesh.vertices.iter().all(|v| v.position[1] == 64.0 + 1.0));
        // UVs land inside stage 3's rect.
        let r = rects()[3];
        assert!(
            mesh.vertices
                .iter()
                .all(|v| v.uv[0] >= r[0] && v.uv[0] <= r[2]),
            "uvs should sit in stage 3's rect {r:?}"
        );
    }

    #[test]
    fn air_state_has_no_crack() {
        let resolver = CrackResolver::new(vec![vec![], vec![cube_top()]], rects());
        let air = StateId::new(0).expect("state zero is in the built-in census");
        assert!(resolver.mesh_for(air, 3, [0.0; 3]).is_none());
    }

    #[test]
    fn out_of_range_stage_has_no_crack() {
        let resolver = CrackResolver::new(vec![vec![cube_top()]], rects());
        let state_id = StateId::new(0).expect("state zero is in the built-in census");
        assert!(resolver.mesh_for(state_id, 200, [0.0; 3]).is_none());
    }

    #[test]
    fn distinct_stages_pick_distinct_rects() {
        let resolver = CrackResolver::new(vec![vec![cube_top()]], rects());
        let state_id = StateId::new(0).expect("state zero is in the built-in census");
        let s0 = resolver.mesh_for(state_id, 0, [0.0; 3]).unwrap();
        let s9 = resolver.mesh_for(state_id, 9, [0.0; 3]).unwrap();
        assert_ne!(s0.vertices[0].uv, s9.vertices[0].uv);
    }

    #[test]
    fn absent_stage_sprite_is_skipped() {
        // A rects table where stage 5 never resolved (all-zero).
        let mut r = rects();
        r[5] = [0.0; 4];
        let resolver = CrackResolver::new(vec![vec![cube_top()]], r);
        let state_id = StateId::new(0).expect("state zero is in the built-in census");
        assert!(resolver.mesh_for(state_id, 5, [0.0; 3]).is_none());
        assert!(resolver.mesh_for(state_id, 4, [0.0; 3]).is_some());
    }

    /// Function pointers record the public typed geometry signatures at compile
    /// time. The runtime control proves a census-valid state retrieves its
    /// snapshot.
    #[test]
    fn state_geometry_requires_a_validated_census_state() {
        let _: fn(&CrackResolver, StateId) -> &[BakedQuad] = CrackResolver::state_quads;
        let _: fn(&CrackResolver, StateId, u8, [f32; 3]) -> Option<CrackMesh> =
            CrackResolver::mesh_for;
        let resolver = CrackResolver::new(vec![vec![cube_top()]], rects());
        let first_state = StateId::new(0).expect("state zero is in the built-in census");
        assert_eq!(resolver.state_quads(first_state).len(), 1);
        assert!(
            StateId::new(lodestone_data::block_states::STATE_COUNT).is_none(),
            "the census boundary must reject the first out-of-range raw id"
        );
    }
}
