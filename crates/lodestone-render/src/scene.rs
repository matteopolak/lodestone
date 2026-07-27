//! The world-scale render plan: the layer that turns *a loaded world of
//! sections* into *a culled list of draws for one frame*.
//!
//! Everything this needs already existed as tested primitives — per-section
//! meshes ([`DrawRegion`]), the occlusion graph and camera walk
//! ([`walk_visible`]), frustum culling ([`Camera::frustum`]), and the draw
//! submission strategies ([`crate::strategy`]). What was missing was the *seam*
//! that binds them: a renderer that draws one section proves the pipeline; a
//! [`WorldScene`] that meshes, culls, and batches a view distance of them proves
//! the architecture.
//!
//! # What this owns
//!
//! * **Mesh lifecycle.** [`WorldScene::insert_section`] registers (or replaces)
//!   a section's uploaded mesh region and its connectivity;
//!   [`WorldScene::remove_section`] evicts it on chunk unload. The region is
//!   built *elsewhere* — off-thread, from a snapshot of the 27-section
//!   neighbourhood the world hands out as section `Arc`s (§12.49) — and handed
//!   here already uploaded. Nothing in this type touches the GPU or blocks on a
//!   mesh build, which is exactly what lets meshing move off the render thread.
//! * **Per-frame culling.** [`WorldScene::plan_frame`] composes frustum culling
//!   with the connected-space occlusion walk and returns the visible
//!   [`DrawRegion`]s plus [`CullStats`]. Handing those regions to a
//!   [`DrawStrategy`](crate::strategy::DrawStrategy) is all that remains to draw
//!   the frame.
//!
//! # Anti-vacuity
//!
//! A renderer that is "fast" because it culled *everything* is the pixel-domain
//! version of a gate that passes while asserting nothing. [`CullStats`] is
//! built so that failure mode is measurable: `drawable == drawn +
//! culled_frustum + culled_occlusion` always holds, and
//! [`CullStats::is_meaningful`] is true only when a frame both drew something
//! *and* culled something. Benchmarks assert it.

use std::collections::{HashMap, HashSet};

use glam::Vec3;

use crate::camera::Camera;
use crate::section::SECTION_SIZE;
use crate::strategy::DrawRegion;
use crate::visibility::{SectionCoord, SectionVisibility, VisibilityGraph, walk_visible};

/// Indices per quad (two triangles), used to report quad counts from index
/// counts.
const INDICES_PER_QUAD: u32 = 6;

/// The section grid cell a world-space point falls in (floor division by the
/// section size, so it is correct for negative coordinates too).
#[must_use]
pub fn section_of(point: Vec3) -> SectionCoord {
    let s = SECTION_SIZE as f32;
    (
        (point.x / s).floor() as i32,
        (point.y / s).floor() as i32,
        (point.z / s).floor() as i32,
    )
}

/// Per-frame culling accounting. The invariant
/// `drawable == drawn + culled_frustum + culled_occlusion` holds by
/// construction, which is what makes these numbers trustworthy rather than
/// decorative.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CullStats {
    /// Sections loaded in the scene (including empty/air ones, which carry no
    /// geometry but still route the occlusion walk).
    pub loaded: usize,
    /// Sections that actually have geometry to draw (`index_count > 0`).
    pub drawable: usize,
    /// Drawable sections that survived both culls and are recorded this frame.
    pub drawn: usize,
    /// Drawable sections rejected by the view frustum.
    pub culled_frustum: usize,
    /// Drawable sections inside the frustum but unreachable through connected
    /// open space (hidden behind opaque geometry).
    pub culled_occlusion: usize,
    /// Total quads across the sections drawn this frame.
    pub drawn_quads: u64,
}

impl CullStats {
    /// Total drawable sections culled this frame.
    #[must_use]
    pub fn culled(&self) -> usize {
        self.culled_frustum + self.culled_occlusion
    }

    /// The anti-vacuity guard: a frame is *meaningful* only if it both drew
    /// something and culled something. A frame that drew nothing (empty gate) or
    /// culled nothing (no working culling) at a normal camera over a populated
    /// world is a bug, and a benchmark that reports a fast frame time without
    /// this being true is measuring the wrong thing.
    #[must_use]
    pub fn is_meaningful(&self) -> bool {
        self.drawn > 0 && self.culled() > 0
    }
}

/// The visible draws for one frame, plus the accounting that produced them.
#[derive(Debug, Clone)]
pub struct FramePlan {
    /// Every drawable section's region, with `visible` set for this frame.
    /// Culled regions are retained with `visible = false` so an indirect
    /// strategy can zero their instance count without resizing the list.
    pub regions: Vec<DrawRegion>,
    /// Culling accounting for this frame.
    pub stats: CullStats,
}

impl FramePlan {
    /// The regions that survived culling, in draw order.
    pub fn visible_regions(&self) -> impl Iterator<Item = &DrawRegion> {
        self.regions.iter().filter(|r| r.visible)
    }
}

/// A loaded world of meshed sections, ready to be culled and drawn per frame.
#[derive(Debug, Default)]
pub struct WorldScene {
    /// Uploaded mesh region per loaded section (air sections carry an empty
    /// region so they still count as loaded and route the walk).
    sections: HashMap<SectionCoord, DrawRegion>,
    /// Connectivity graph for the camera occlusion walk.
    graph: VisibilityGraph,
}

impl WorldScene {
    /// An empty scene.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a section's uploaded mesh and connectivity. This is
    /// the mesh-lifecycle entry point: the `mesh` region is produced off-thread
    /// from a section-neighbourhood snapshot and handed here already uploaded,
    /// so this call neither meshes nor touches the GPU. An air section is
    /// registered with an empty region (`index_count == 0`) and its all-open
    /// [`SectionVisibility`]; it draws nothing but still routes the walk.
    pub fn insert_section(
        &mut self,
        coord: SectionCoord,
        mesh: DrawRegion,
        visibility: SectionVisibility,
    ) {
        self.sections.insert(coord, mesh);
        self.graph.insert(coord, visibility);
    }

    /// Evict a section on chunk unload. Returns `true` if it was loaded. Both
    /// the mesh region and the connectivity entry are dropped, so a subsequent
    /// frame neither draws nor walks through it.
    pub fn remove_section(&mut self, coord: SectionCoord) -> bool {
        let had_mesh = self.sections.remove(&coord).is_some();
        let had_vis = self.graph.remove(coord).is_some();
        had_mesh || had_vis
    }

    /// Number of loaded sections (including air).
    #[must_use]
    pub fn loaded_len(&self) -> usize {
        self.sections.len()
    }

    /// Whether any section is loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// Whether a section is loaded.
    #[must_use]
    pub fn contains(&self, coord: SectionCoord) -> bool {
        self.sections.contains_key(&coord)
    }

    /// Cull the world for `camera` and return the frame's visible draws.
    ///
    /// Culling composes two stages:
    /// 1. **Frustum.** Sections outside the view frustum are dropped.
    /// 2. **Occlusion.** From the camera's section, [`walk_visible`] flood-fills
    ///    through connected open space (frustum-gated), so sections sealed off
    ///    behind opaque geometry never draw. If the camera is outside the loaded
    ///    set (e.g. flying above the world) there is no walk origin, so this
    ///    falls back to pure frustum culling — still culling, never "draw all".
    ///
    /// Cost: `O(loaded)` frustum tests plus one BFS bounded by the visible set.
    /// No allocation per section beyond the returned plan.
    #[must_use]
    pub fn plan_frame(&self, camera: &Camera) -> FramePlan {
        let frustum = camera.frustum();
        let camera_section = section_of(camera.position);

        let reachable: HashSet<SectionCoord> = if self.graph.contains(camera_section) {
            walk_visible(&self.graph, camera_section, |c| frustum.section_visible(c))
                .into_iter()
                .collect()
        } else {
            self.sections
                .keys()
                .copied()
                .filter(|&c| frustum.section_visible(c))
                .collect()
        };

        let mut regions = Vec::new();
        let mut stats = CullStats {
            loaded: self.sections.len(),
            ..CullStats::default()
        };

        for (&coord, mesh) in &self.sections {
            if mesh.index_count == 0 {
                continue; // air / empty: routes the walk, but nothing to draw.
            }
            stats.drawable += 1;

            let in_frustum = frustum.section_visible(coord);
            let reached = reachable.contains(&coord);
            let visible = in_frustum && reached;

            if visible {
                stats.drawn += 1;
                stats.drawn_quads += u64::from(mesh.index_count / INDICES_PER_QUAD);
            } else if !in_frustum {
                stats.culled_frustum += 1;
            } else {
                stats.culled_occlusion += 1;
            }

            let mut region = *mesh;
            region.visible = visible;
            regions.push(region);
        }

        FramePlan { regions, stats }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drawable region with `count` quads' worth of indices.
    fn mesh(instance: u32, quads: u32) -> DrawRegion {
        DrawRegion {
            first_index: 0,
            index_count: quads * INDICES_PER_QUAD,
            base_vertex: 0,
            instance,
            visible: true,
        }
    }

    /// An empty (air) region.
    fn air() -> DrawRegion {
        DrawRegion {
            first_index: 0,
            index_count: 0,
            base_vertex: 0,
            instance: 0,
            visible: true,
        }
    }

    /// Camera at the centre of section `(0,0,0)` looking down +Z (default yaw).
    fn camera_at_origin_section() -> Camera {
        Camera {
            position: Vec3::new(8.0, 8.0, 8.0),
            ..Camera::default()
        }
    }

    #[test]
    fn section_of_floors_toward_negative() {
        assert_eq!(section_of(Vec3::new(0.0, 0.0, 0.0)), (0, 0, 0));
        assert_eq!(section_of(Vec3::new(15.9, 15.9, 15.9)), (0, 0, 0));
        assert_eq!(section_of(Vec3::new(16.0, 0.0, 0.0)), (1, 0, 0));
        assert_eq!(section_of(Vec3::new(-0.1, 0.0, 0.0)), (-1, 0, 0));
        assert_eq!(section_of(Vec3::new(-16.0, 0.0, 0.0)), (-1, 0, 0));
    }

    #[test]
    fn evicting_a_section_removes_it_from_the_plan() {
        let mut scene = WorldScene::new();
        scene.insert_section((0, 0, 0), air(), SectionVisibility::all());
        scene.insert_section((0, 0, 1), mesh(0, 4), SectionVisibility::all());
        scene.insert_section((0, 0, 2), mesh(1, 4), SectionVisibility::all());

        let plan = scene.plan_frame(&camera_at_origin_section());
        assert_eq!(plan.stats.drawn, 2, "both front sections should draw");

        assert!(scene.remove_section((0, 0, 2)), "section was loaded");
        assert!(!scene.contains((0, 0, 2)));

        let plan = scene.plan_frame(&camera_at_origin_section());
        assert_eq!(plan.stats.drawn, 1, "evicted section must not draw");
        assert!(
            plan.visible_regions().all(|r| r.instance != 1),
            "the evicted section's instance slot must be gone from the draw list",
        );
        assert!(
            !scene.remove_section((0, 0, 2)),
            "removing an absent section is a no-op",
        );
    }

    #[test]
    fn frustum_culls_sections_behind_the_camera() {
        let mut scene = WorldScene::new();
        scene.insert_section((0, 0, 0), air(), SectionVisibility::all());
        // In front (+Z) and behind (-Z) the camera, all with geometry.
        scene.insert_section((0, 0, 1), mesh(1, 4), SectionVisibility::all());
        scene.insert_section((0, 0, 2), mesh(2, 4), SectionVisibility::all());
        scene.insert_section((0, 0, -1), mesh(3, 4), SectionVisibility::all());
        scene.insert_section((0, 0, -2), mesh(4, 4), SectionVisibility::all());

        let plan = scene.plan_frame(&camera_at_origin_section());

        assert!(
            plan.stats.culled_frustum >= 2,
            "the two sections behind the camera must be frustum-culled, got {}",
            plan.stats.culled_frustum,
        );
        assert!(plan.stats.drawn >= 1, "front sections must still draw");
        // No behind-camera instance survives.
        assert!(
            plan.visible_regions()
                .all(|r| r.instance != 3 && r.instance != 4),
            "sections behind the camera must not be in the visible draw list",
        );
    }

    #[test]
    fn occlusion_walk_stops_at_a_solid_section() {
        let mut scene = WorldScene::new();
        // A +Z corridor: air camera section, air, SOLID wall, then a section
        // sealed behind the wall.
        scene.insert_section((0, 0, 0), air(), SectionVisibility::all());
        scene.insert_section((0, 0, 1), mesh(1, 4), SectionVisibility::all());
        scene.insert_section((0, 0, 2), mesh(2, 4), SectionVisibility::solid());
        scene.insert_section((0, 0, 3), mesh(3, 4), SectionVisibility::all());

        let plan = scene.plan_frame(&camera_at_origin_section());

        assert!(
            plan.stats.culled_occlusion >= 1,
            "the section behind the wall must be occlusion-culled, got {}",
            plan.stats.culled_occlusion,
        );
        assert!(
            plan.visible_regions().all(|r| r.instance != 3),
            "the sealed-off section must not draw",
        );
        // The wall itself (its near face) is visible.
        assert!(
            plan.visible_regions().any(|r| r.instance == 2),
            "the wall you are looking at must draw",
        );
    }

    #[test]
    fn air_sections_are_not_counted_as_drawable() {
        let mut scene = WorldScene::new();
        scene.insert_section((0, 0, 0), air(), SectionVisibility::all());
        scene.insert_section((0, 0, 1), air(), SectionVisibility::all());

        let plan = scene.plan_frame(&camera_at_origin_section());
        assert_eq!(plan.stats.loaded, 2);
        assert_eq!(plan.stats.drawable, 0);
        assert_eq!(plan.stats.drawn, 0);
        assert!(plan.regions.is_empty());
    }

    #[test]
    fn cull_stats_invariant_always_holds() {
        let mut scene = WorldScene::new();
        scene.insert_section((0, 0, 0), air(), SectionVisibility::all());
        for z in -3..=3 {
            for x in -3..=3 {
                scene.insert_section((x, 0, z), mesh((x + z) as u32, 4), SectionVisibility::all());
            }
        }
        let plan = scene.plan_frame(&camera_at_origin_section());
        let s = plan.stats;
        assert_eq!(
            s.drawable,
            s.drawn + s.culled_frustum + s.culled_occlusion,
            "every drawable section is exactly one of drawn / frustum-culled / occlusion-culled",
        );
        assert_eq!(
            plan.regions.iter().filter(|r| r.visible).count(),
            s.drawn,
            "visible region count must match the drawn stat",
        );
    }

    #[test]
    fn camera_outside_loaded_world_falls_back_to_frustum_only() {
        let mut scene = WorldScene::new();
        // Sections at the camera's height, split in front (+Z) and behind (-Z),
        // but the camera's OWN section (0,25,0) is left unloaded so there is no
        // occlusion-walk origin and the frustum-only fallback is exercised.
        for z in [-3, -2, -1, 1, 2, 3] {
            scene.insert_section(
                (0, 25, z),
                mesh((z + 8) as u32, 4),
                SectionVisibility::all(),
            );
        }
        let camera = Camera {
            position: Vec3::new(8.0, 408.0, 8.0), // centre of section (0,25,0)
            ..Camera::default()                   // yaw 0 → looks +Z
        };
        assert_eq!(section_of(camera.position), (0, 25, 0));
        assert!(
            !scene.contains(section_of(camera.position)),
            "camera section must be unloaded for this test",
        );
        let plan = scene.plan_frame(&camera);
        assert_eq!(
            plan.stats.culled_occlusion, 0,
            "no occlusion walk without a loaded camera section",
        );
        assert!(
            plan.stats.drawn > 0 && plan.stats.culled_frustum > 0,
            "fallback must still cull by frustum, not draw-all or draw-none: {:?}",
            plan.stats,
        );
    }
}
