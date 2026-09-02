//! The section occlusion graph's consumer side (U3): the camera walk, its
//! cross-frame cache, and the invalidation cadence.
//!
//! # What this removes that frustum culling cannot
//!
//! Standing on the surface, the frustum contains the entire column of sections
//! *below* you and the distance circle keeps all of them. Only connectivity
//! reachability — "can light get from the section I am in to that one through
//! open space" — removes the underground, and inside a cave it removes almost
//! everything else too. The per-section half is
//! [`lodestone_render::compute_visibility`], computed in the mesh worker
//! (`mesher::snapshot_visibility`); this is the BFS from the camera over the
//! result.
//!
//! # The cadence, and why the frustum is not in it
//!
//! Vanilla re-walks when the camera crosses an **8-block cell** on any axis or
//! when the graph changes (its own invalidate-if-needed check), and
//! applies the frustum *per frame* over the cached reachable set rather than
//! inside the walk. [`OcclusionCache`] is that: keyed on
//! `(camera 8-block cell, graph generation)`, so turning on the spot re-walks
//! nothing, and the frustum stays in `TerrainCull::classify` where it runs every
//! frame regardless.
//!
//! # The failure mode to watch, and how it is visible
//!
//! Every way this can go wrong except one draws *more*: an absent graph entry
//! reads as open, an unwalkable graph degrades to `with_reachable(None)`, and a
//! stale cache is a superset from a neighbouring cell. That is deliberate, and it
//! is also why the degradation is silent — a cull that quietly stopped culling
//! looks exactly like a cull that found nothing to cull. `RenderStats`'
//! `occlusion_active` / `occlusion_graph_sections` / `occlusion_walks` exist to
//! separate those two: zero `sections_culled_occlusion` with `occlusion_active`
//! true and a plausible `occlusion_graph_sections` is a real frame; zero with
//! `occlusion_active` **false** is the graph refusing to walk.
//!
//! The one direction that loses pixels is the walk itself over-culling, which is
//! angle-dependent — hence [`TerrainOcclusion::Shadow`] and the angle sweep in
//! `crates/lodestone-render/tests/occlusion_angle_sweep.rs`.

use std::collections::HashSet;
use std::sync::Arc;

use lodestone_render::{Camera, SectionCoord};

use super::RenderState;

/// What the occlusion graph is allowed to do this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerrainOcclusion {
    /// Do not walk at all — the pre-U3 cull (distance ∩ frustum).
    Off,
    /// Walk and report [`RenderStats::sections_occlusion_shadow`], but draw
    /// everything the frustum and distance tests keep.
    ///
    /// The soak arm. Nothing can disappear while this is selected, so it is what
    /// you play with when you want to know whether the reachable set agrees with
    /// what you can actually see, and it is the arm to switch to first if terrain
    /// ever does vanish.
    Shadow,
    /// Walk and cull. The default.
    #[default]
    On,
}

/// The cached reachable set plus the key it is valid for.
///
/// `walks` is a session-cumulative counter, not a per-frame one: the whole claim
/// of the cadence is that it does *not* increment on rotation, and a per-frame
/// value cannot express that. Read it across two frames.
#[derive(Debug, Default)]
pub(super) struct OcclusionCache {
    /// `(camera 8-block cell, graph generation)` the set was walked for.
    key: Option<((i32, i32, i32), u64)>,
    /// The set, shared with every [`lodestone_render::TerrainCull`] built from
    /// it — `Arc` so a frame costs a refcount bump rather than a rehash of
    /// several thousand coords.
    reachable: Option<Arc<HashSet<SectionCoord>>>,
    /// How many walks this session.
    pub(super) walks: u64,
}

/// The 8-block invalidation cell vanilla keys `invalidateIfNeeded` on
/// (`SectionOcclusionGraph`'s `lastCameraSectionX/Y/Z` are section-grid, but its
/// `Frustum.offsetToFullyIncludeCameraCube(8)` and the "camera moved" test are
/// both on the half-section grid — the same 8 as
/// [`lodestone_render::CAMERA_CUBE_BLOCKS`]).
const INVALIDATION_CELL_BLOCKS: f32 = lodestone_render::CAMERA_CUBE_BLOCKS;

impl RenderState {
    /// Choose whether the occlusion graph culls, only counts, or is not walked.
    ///
    /// Orthogonal to [`set_terrain_culling`](Self::set_terrain_culling), which is
    /// the bigger hammer (it turns distance and frustum off too). For diagnosing
    /// *missing terrain* reach for this one first: [`TerrainOcclusion::Shadow`]
    /// keeps the rest of the frame byte-identical and still tells you what the
    /// walk wanted to remove.
    pub fn set_terrain_occlusion(&mut self, mode: TerrainOcclusion) {
        self.occlusion_mode = mode;
        // A mode change must not read a set walked under the old one.
        self.occlusion.borrow_mut().key = None;
    }

    /// The current occlusion mode.
    #[must_use]
    pub fn terrain_occlusion(&self) -> TerrainOcclusion {
        self.occlusion_mode
    }

    /// How many sections the occlusion graph holds.
    ///
    /// The number to assert against the resident set: the graph must hold **every
    /// meshed section**, geometry or not, or the walk is working from a world with
    /// holes in it. `section_count()` is a *subset* of this (it counts only
    /// sections with geometry), so `occlusion_graph_sections >= section_count()`
    /// is the live invariant.
    #[must_use]
    pub fn occlusion_graph_sections(&self) -> usize {
        self.vis_graph.len()
    }

    /// This frame's reachable set, walked or reused from
    /// [`OcclusionCache`].
    ///
    /// `None` — which leaves the cull at distance ∩ frustum — for any of:
    ///
    /// * [`TerrainOcclusion::Off`];
    /// * `render_distance_chunks == 0`, which already disables the distance test
    ///   and is what an uninitialised `RenderState` holds. The walk's bounds
    ///   *are* the view circle, so there is no honest finite cylinder to walk
    ///   without one, and a cull that blanks the world on a default-constructed
    ///   state is indistinguishable from a broken renderer;
    /// * an empty graph (nothing meshed yet, or the packed demo path, which has
    ///   no visibility producer);
    /// * `terrain_culling == false`, so the `smartCull` off arm really is the
    ///   pre-cull frame and not "the pre-cull frame plus a walk nobody reads".
    pub(super) fn frame_reachable(&self, camera: &Camera) -> Option<Arc<HashSet<SectionCoord>>> {
        if self.occlusion_mode == TerrainOcclusion::Off
            || !self.terrain_culling
            || self.render_distance_chunks == 0
        {
            return None;
        }
        let cell = |v: f32| (v / INVALIDATION_CELL_BLOCKS).floor() as i32;
        let key = (
            (
                cell(camera.position.x),
                cell(camera.position.y),
                cell(camera.position.z),
            ),
            self.vis_graph.generation(),
        );
        let mut cache = self.occlusion.borrow_mut();
        if cache.key == Some(key) {
            return cache.reachable.clone();
        }

        // The walk itself, bounds included, is `lodestone_render`'s — deliberately
        // not re-derived here, so the angle-sweep gate over there exercises the
        // same function this frame does.
        let reachable = lodestone_render::reachable_from_camera(
            &self.vis_graph,
            camera.position,
            self.render_distance_chunks,
        )?;

        cache.walks += 1;
        cache.key = Some(key);
        cache.reachable = Some(Arc::new(reachable));
        cache.reachable.clone()
    }

    /// Insert or replace a section's connectivity. Called from `upload_section`
    /// for every meshed section, **including ones whose geometry is empty**.
    pub(super) fn record_section_visibility(
        &mut self,
        coord: SectionCoord,
        visibility: lodestone_render::SectionVisibility,
    ) {
        self.vis_graph.insert(coord, visibility);
    }

    /// Drop a section's connectivity (chunk unload, or a section that became
    /// all air). Absent coords read as open, so this only ever makes the walk
    /// more permissive.
    pub(super) fn forget_section_visibility(&mut self, coord: SectionCoord) {
        self.vis_graph.remove(coord);
    }

    /// Session-cumulative walk count — see [`OcclusionCache::walks`].
    #[must_use]
    pub fn occlusion_walks(&self) -> u64 {
        self.occlusion.borrow().walks
    }
}
