//! The world-scale meshing lifecycle: turning `Arc<ChunkSection>` snapshots
//! pulled from the client's world into uploaded, cullable [`DrawRegion`]s.
//!
//! `scene.rs` owns the *frame* (cull the loaded set, emit draws). This module
//! owns everything *upstream* of a loaded scene: which sections a chunk-load
//! dirties, how a 3×3×3 neighbourhood snapshot becomes a mesh, and how those
//! meshes are built off the render thread and suballocated onto the GPU.
//!
//! ## Reading through the `Arc` seam (§12.49)
//!
//! [`ClientHandle::section_at`](../../lodestone_client) hands out
//! `Option<Arc<ChunkSection>>` that pins no lock and carries no borrow, so a
//! mesher clones the 27 `Arc`s for a neighbourhood, drops the world lock, and
//! meshes off a stable snapshot while chunk streaming and block edits continue.
//! A later edit forks exactly one section copy-on-write, leaving the snapshot
//! this module holds valid and unchanged — which is what makes off-thread
//! meshing sound.
//!
//! ## Light (world seam — accessors landed, per-section wiring pending)
//!
//! The version-free light source is now published by `lodestone-world`:
//! `World::section_light(pos, light_section_index) -> Option<SectionLight>`, and
//! `SectionLight::sky_at(x,y,z) -> u8` / `block_at(x,y,z) -> u8` return resolved
//! levels with the nibble unpacking kept on the storage side (single unpack
//! path — this module's cross-seam corner averaging cannot drift a nibble
//! against storage). Those accessors match this crate's [`SectionLight`] trait
//! one-for-one, so the adapter is a rename-forward.
//!
//! Until the path below is wired, this module still meshes with the declared
//! pre-light bridge ([`UniformLight::pre_light_bridge`], full sky / no block) so
//! no exposed face renders black in the interim (§7). **Two things gate the live
//! swap, and neither is a one-line call-site change:**
//!
//! 1. **A per-section refactor here.** [`SectionSnapshot::build_mesh`] today
//!    takes *one* shared `light: &L` and applies it to all 27 neighbourhood
//!    sections — correct for a uniform bridge, wrong for real light, since each
//!    section carries its own [`SectionLight`] and the smooth-lighting corner
//!    blend reads *neighbours'* light across seams. Real light therefore requires
//!    the snapshot to carry per-section light and `build_mesh` to sample the
//!    owning section's light per cell, not one global source.
//! 2. **A lock-free `handle` accessor** (pending in `lodestone-client`): the
//!    mesher runs off-thread on `Arc` snapshots, so it needs
//!    `handle.section_light(pos, i)` mirroring `section_at`, not `World` access
//!    under a lock. (`lodestone-world`'s real sky/block propagation is also still
//!    landing; the accessors already work, the values are just uniform until it
//!    does — no interface change when they become real.)
//!
//! The seam contract (agreed with the light-engine owner, since this module is
//! the consumer that samples across section seams):
//!
//! * **Resolved u8 per cell, not raw `LightData`.** The smooth-lighting corner
//!   blend (`face_corner_lighting` in [`crate::mesh`]) averages four individual
//!   cell levels per corner, so the mesher needs `sky/block(x,y,z) -> u8`; the
//!   nibble unpacking stays in the crate that owns the packing, so the two can
//!   never disagree.
//! * **Light-section indexing (0 = the boundary section below the world; light
//!   section `i` covers block section `i-1`).** The mesher builds
//!   `section_count + 2` sections and must light the top/bottom faces of the
//!   build range by sampling into the section beyond it — block-section index
//!   cannot name section `-1`, light-section index can. The call site does the
//!   `+1` translation from block- to light-section index.
//! * **Never default absent sky light to 15 blindly.** `sky_at` resolves a
//!   `Missing` section to `0`; the vanilla *above-the-world* sky default of `15`
//!   is dimension- and heightmap-dependent (there is no sky light in the
//!   nether/end), so it is applied by whoever knows the dimension via an explicit
//!   policy, never coerced in the mesher — coercing absent sky to 15 is the
//!   too-bright-nether bug. The [`UniformLight`] `sky_light: 15` bridge is a
//!   stand-in for *absent* data, not a claim about any dimension, and is dropped
//!   once real light samples.
//! * **`None` means unloaded, not dark.** A `None` from `section_light` is an
//!   unloaded chunk / out-of-range section (defer the seam, re-mesh on load),
//!   distinct from a present-but-dark section.
//!
//! ## Absent vs. empty neighbours
//!
//! A `None` snapshot slot — an unloaded chunk *or* an elided all-air section —
//! reads as [`Cell::EMPTY`](crate::section::Cell) at the boundary: the face is
//! still emitted (air does not occlude) but greedy runs do not merge across it.
//! When the neighbour later loads, the dirty-propagation rule
//! ([`neighbour_columns`]) re-meshes this section so the seam heals. This is the
//! chunk-pop-in behaviour vanilla also shows.

use std::sync::Arc;

use lodestone_world::ChunkSection;

use crate::mesh::{Mesh, mesh_greedy, mesh_simple};
use crate::section::{SectionNeighborhood, SectionView};
use crate::visibility::{SectionCoord, SectionVisibility, compute_visibility};
use crate::world::{BlockClassifier, ChunkSectionView, SectionLight, UniformLight};

/// A lock-free source of section snapshots, keyed by absolute
/// [`SectionCoord`] `(chunk_x, section_y, chunk_z)`.
///
/// This is the seam that decouples the renderer from the client: an application
/// adapts its `ClientHandle` (converting `section_y` to the column's storage
/// index) so `lodestone-render` never depends on `lodestone-client`. A `None`
/// result means the section is unloaded or all-air; either way it meshes as a
/// boundary of empty cells.
pub trait SectionSource {
    /// The section snapshot at `coord`, or `None` if unloaded/all-air.
    fn section(&self, coord: SectionCoord) -> Option<Arc<ChunkSection>>;
}

/// The 27 absolute section coordinates of the neighbourhood centred on `coord`,
/// in `[dx+1][dy+1][dz+1]` order (matching [`SectionSnapshot`]'s grid).
#[must_use]
pub fn neighbourhood_coords(coord: SectionCoord) -> [SectionCoord; 27] {
    let (cx, cy, cz) = coord;
    let mut out = [(0, 0, 0); 27];
    let mut i = 0;
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                out[i] = (cx + dx, cy + dy, cz + dz);
                i += 1;
            }
        }
    }
    out
}

/// The nine columns a chunk-load at `(cx, cz)` dirties: the loaded column and
/// its eight horizontal neighbours.
///
/// Loading column P supplies new geometry for P's own sections *and* invalidates
/// the boundary meshes of the eight surrounding columns, because their edge and
/// corner ambient occlusion now samples into P. Re-meshing only those columns'
/// boundary sections is enough, but callers typically re-mesh whatever loaded
/// sections fall in these columns — the set is small and bounded.
#[must_use]
pub fn neighbour_columns(cx: i32, cz: i32) -> [(i32, i32); 9] {
    let mut out = [(0, 0); 9];
    let mut i = 0;
    for dx in -1..=1 {
        for dz in -1..=1 {
            out[i] = (cx + dx, cz + dz);
            i += 1;
        }
    }
    out
}

/// The `(chunk_x, chunk_z)` column a section belongs to.
#[must_use]
pub fn column_of(coord: SectionCoord) -> (i32, i32) {
    (coord.0, coord.2)
}

/// The mesh jobs a column load at `(cx, cz)` dirties, over the vertical section
/// range `section_ys`.
///
/// Loading a column changes geometry for every *loaded* section in the nine
/// columns [`neighbour_columns`] returns: the loaded column's own sections, and
/// the surrounding columns' sections whose edge/corner ambient occlusion now
/// samples into the new column. Each present section is gathered into a 3×3×3
/// snapshot ready for [`build_batch`]. Absent (unloaded/air) sections are
/// skipped — there is nothing to draw and the boundary self-heals when they
/// later load. This is a pure function of `source`, so it is tested without a
/// GPU.
#[must_use]
pub fn dirty_jobs(
    source: &dyn SectionSource,
    cx: i32,
    cz: i32,
    section_ys: core::ops::Range<i32>,
) -> Vec<MeshJob> {
    let mut jobs = Vec::new();
    for (col_x, col_z) in neighbour_columns(cx, cz) {
        for y in section_ys.clone() {
            let coord = (col_x, y, col_z);
            if source.section(coord).is_some() {
                jobs.push(MeshJob {
                    coord,
                    snapshot: SectionSnapshot::gather(source, coord),
                });
            }
        }
    }
    jobs
}

/// A 3×3×3 grid of owned section snapshots centred on the section being meshed.
///
/// Holding [`Arc`]s (not borrows) is the whole point: the grid is a stable,
/// `Send` snapshot that outlives the world lock, so it can be meshed on a worker
/// thread. Slot `[1][1][1]` is the centre; `[dx+1][dy+1][dz+1]` is the neighbour
/// at section-offset `(dx, dy, dz)`.
#[derive(Debug, Clone, Default)]
pub struct SectionSnapshot {
    sections: [[[Option<Arc<ChunkSection>>; 3]; 3]; 3],
}

/// A 3×3×3 grid of section views borrowing into a [`SectionSnapshot`]'s `Arc`s,
/// in `[dx+1][dy+1][dz+1]` order. Aliased to keep [`SectionSnapshot::build_mesh`]
/// readable (and clippy quiet about the nested array type).
type ViewGrid<'a, C, L> = [[[Option<ChunkSectionView<'a, C, L>>; 3]; 3]; 3];

/// Per-section light for a 3×3×3 neighbourhood, indexed `[dx+1][dy+1][dz+1]` to
/// match [`SectionSnapshot`]'s section grid.
///
/// Each present section is lit by *its own* source — real light is per-section
/// and the smooth-lighting corner blend reads *neighbours'* light across seams,
/// so one shared source cannot express it. A `None` slot means that section
/// contributes no light and is dropped from meshing (it then reads as
/// [`Cell::EMPTY`](crate::section::Cell) at seams); callers must therefore supply
/// a light entry for every section they want meshed. For the pre-light bridge
/// every slot points at one [`UniformLight`]; for real light every present
/// section carries its own [`WorldSectionLight`](crate::world::WorldSectionLight).
pub type LightGrid<'a, L> = [[[Option<&'a L>; 3]; 3]; 3];

impl SectionSnapshot {
    /// Build a snapshot directly from a filled `[dx+1][dy+1][dz+1]` grid.
    #[must_use]
    pub fn from_grid(sections: [[[Option<Arc<ChunkSection>>; 3]; 3]; 3]) -> Self {
        Self { sections }
    }

    /// Pull the 27-section neighbourhood for `centre` from `source` in one pass.
    #[must_use]
    pub fn gather(source: &dyn SectionSource, centre: SectionCoord) -> Self {
        let (cx, cy, cz) = centre;
        let sections = core::array::from_fn(|ix| {
            core::array::from_fn(|iy| {
                core::array::from_fn(|iz| {
                    let coord = (cx + ix as i32 - 1, cy + iy as i32 - 1, cz + iz as i32 - 1);
                    source.section(coord)
                })
            })
        });
        Self { sections }
    }

    /// The centre section, if loaded.
    #[must_use]
    pub fn centre(&self) -> Option<&Arc<ChunkSection>> {
        self.sections[1][1][1].as_ref()
    }

    /// Whether the centre holds any geometry worth meshing (loaded and not
    /// entirely air). A non-drawable snapshot still *routes* the occlusion walk,
    /// but produces an empty mesh.
    #[must_use]
    pub fn is_drawable(&self) -> bool {
        self.centre().is_some_and(|s| s.non_air_count() > 0)
    }

    /// Mesh the centre section against its neighbours.
    ///
    /// `lights` supplies light **per section**, indexed the same way as the
    /// snapshot's grid: slot `[dx+1][dy+1][dz+1]` lights the section at offset
    /// `(dx,dy,dz)` with its own source, so the smooth-lighting corner blend
    /// reads each neighbour's real light across seams. Today the live caller
    /// fills every slot with one [`UniformLight::pre_light_bridge`] (see the
    /// module docs); the signature takes any [`SectionLight`] per slot, so real
    /// per-section light (each present section a
    /// [`WorldSectionLight`](crate::world::WorldSectionLight)) drops in without
    /// touching the mesher. A present section whose light slot is `None` is
    /// dropped from meshing — callers must light every section they mesh.
    /// `greedy` selects the merging mesher over the reference per-face one.
    #[must_use]
    pub fn build_mesh<C: BlockClassifier, L: SectionLight>(
        &self,
        classifier: &C,
        lights: &LightGrid<'_, L>,
        greedy: bool,
    ) -> Mesh {
        // Views borrow into the held `Arc`s and the per-section light; all live
        // for this call. A section is meshed only when it has *both* geometry and
        // a light source for its slot.
        let views: ViewGrid<'_, C, L> = core::array::from_fn(|ix| {
            core::array::from_fn(|iy| {
                core::array::from_fn(|iz| {
                    match (self.sections[ix][iy][iz].as_ref(), lights[ix][iy][iz]) {
                        (Some(arc), Some(light)) => {
                            Some(ChunkSectionView::new(arc.as_ref(), classifier, light))
                        }
                        _ => None,
                    }
                })
            })
        });
        let mut hood = SectionNeighborhood::default();
        for (ix, plane) in views.iter().enumerate() {
            for (iy, row) in plane.iter().enumerate() {
                for (iz, slot) in row.iter().enumerate() {
                    if let Some(view) = slot.as_ref() {
                        hood.set(
                            ix as i32 - 1,
                            iy as i32 - 1,
                            iz as i32 - 1,
                            Some(view as &dyn SectionView),
                        );
                    }
                }
            }
        }
        if greedy {
            mesh_greedy(&hood)
        } else {
            mesh_simple(&hood)
        }
    }

    /// The centre section's connectivity, for the scene's occlusion walk.
    ///
    /// [`compute_visibility`] reads only cell occlusion (from `classifier`), not
    /// light, so the [`UniformLight`] fallback is irrelevant here. An unloaded
    /// centre connects nothing ([`SectionVisibility::NONE`]); an air/sparse
    /// centre is fully open; a solid one is sealed.
    #[must_use]
    pub fn centre_visibility<C: BlockClassifier, L: SectionLight>(
        &self,
        classifier: &C,
        light: &L,
    ) -> SectionVisibility {
        match self.centre() {
            Some(arc) => {
                let view = ChunkSectionView::new(arc.as_ref(), classifier, light);
                compute_visibility(&view)
            }
            None => SectionVisibility::NONE,
        }
    }
}

/// A unit of meshing work: the section to build and its snapshot. `Send`, so a
/// batch can be meshed across a worker pool.
#[derive(Debug, Clone)]
pub struct MeshJob {
    /// The section being meshed.
    pub coord: SectionCoord,
    /// Its 3×3×3 neighbourhood snapshot.
    pub snapshot: SectionSnapshot,
}

/// A finished mesh tagged with the section it belongs to and its connectivity.
#[derive(Debug)]
pub struct BuiltSection {
    /// The section that was meshed.
    pub coord: SectionCoord,
    /// The resulting geometry (possibly empty).
    pub mesh: Mesh,
    /// The centre section's connectivity, to register with the scene graph so
    /// the occlusion walk routes correctly (an air section still routes even
    /// though its mesh is empty).
    pub visibility: SectionVisibility,
}

/// Build a batch of section meshes.
///
/// On native targets this fans the jobs out across a rayon pool — the payoff of
/// the `Arc` snapshot: each job owns its geometry, shares nothing mutable, and
/// meshes with no lock held. On `wasm32` (which has no thread pool) it runs
/// serially with identical results. The split is on `target_arch`, not a Cargo
/// feature, so unification can never drag rayon into the browser build.
/// `classifier` must be `Sync` for the parallel path; light is the
/// [`UniformLight::pre_light_bridge`] stand-in until real per-section light is
/// wired (see the module-level light-seam contract).
#[must_use]
pub fn build_batch<C: BlockClassifier + Sync>(
    jobs: Vec<MeshJob>,
    classifier: &C,
    greedy: bool,
) -> Vec<BuiltSection> {
    // PRE-LIGHT BRIDGE — not real light. Named (rather than `default()`) so this
    // masquerade is unmistakable on a read of the meshing path. Every
    // neighbourhood slot is lit by this one full-bright source until real light
    // is wired; when it lands, each present section instead carries its own
    // `WorldSectionLight` (from the driver's `section_light` snapshot) in the
    // per-section grid, and the canary test
    // `pre_light_bridge_is_the_declared_full_bright_source` guards the swap.
    let bridge = UniformLight::pre_light_bridge();
    let lights: LightGrid<'_, UniformLight> =
        core::array::from_fn(|_| core::array::from_fn(|_| core::array::from_fn(|_| Some(&bridge))));
    let build = |job: MeshJob| BuiltSection {
        coord: job.coord,
        mesh: job.snapshot.build_mesh(classifier, &lights, greedy),
        visibility: job.snapshot.centre_visibility(classifier, &bridge),
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        jobs.into_par_iter().map(build).collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        jobs.into_iter().map(build).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section::{Cell, SpriteId};
    use crate::world::SectionLight;
    use lodestone_world::PaletteKind;

    const AIR: u32 = 0;
    const STONE: u32 = 1;

    /// Canary for the pre-light bridge. `build_batch` meshes with
    /// [`UniformLight::pre_light_bridge`] until real per-section light is wired;
    /// this test pins that bridge's identity (full sky, no block light) so the
    /// bridge cannot be quietly mistaken for real light, and so replacing it
    /// with real sampling is a *deliberate* edit that must update this test.
    ///
    /// When the real seam lands, thread per-section light through the
    /// [`MeshJob`] snapshot in `build_batch` and delete this canary — do not just
    /// widen it, because a bridge that renders plausibly is the most dangerous
    /// kind of dead path.
    #[test]
    fn pre_light_bridge_is_the_declared_full_bright_source() {
        let bridge = UniformLight::pre_light_bridge();
        assert_eq!(bridge.sky_light(0, 0, 0), 15, "bridge is full sky light");
        assert_eq!(bridge.block_light(0, 0, 0), 0, "bridge has no block light");
        // The bridge must NOT be read as a per-dimension truth: it stands in for
        // absent data, so real light (e.g. nether sky 0) must override it, never
        // the reverse. Default is the same value but the live path names it.
        let d = UniformLight::default();
        assert_eq!(
            (d.sky_light(0, 0, 0), d.block_light(0, 0, 0)),
            (bridge.sky_light(0, 0, 0), bridge.block_light(0, 0, 0)),
            "default() delegates to the named bridge (one source of truth)"
        );
    }

    /// Anti-vacuity: proves per-section light actually reaches the vertices, via
    /// real `lodestone-world` snapshots through [`WorldSectionLight`]. The centre
    /// section is stored *dim* (sky 4) while every neighbour is full-bright, so
    /// the centre's top face — which samples the air directly above it, in the
    /// centre section — must carry sky 4 where the uniform bridge carries 15.
    ///
    /// The old single-shared-light signature structurally could not express a
    /// centre that differs from its neighbours; this is the test that would have
    /// stayed green against the bridge and so is the one that guards the swap.
    #[test]
    fn build_mesh_lights_each_section_from_its_own_source() {
        use crate::world::{SkyDefault, WorldSectionLight};
        use lodestone_world::{LightData, SectionLight as WorldLight};

        let centre = floor_section();
        let air = air_section();
        let grid: [[[Option<Arc<ChunkSection>>; 3]; 3]; 3] = core::array::from_fn(|ix| {
            core::array::from_fn(|iy| {
                core::array::from_fn(|iz| {
                    if (ix, iy, iz) == (1, 1, 1) {
                        Some(centre.clone())
                    } else {
                        Some(air.clone())
                    }
                })
            })
        });
        let snap = SectionSnapshot::from_grid(grid);

        // Centre stored dim; neighbours full-bright — a per-section distinction a
        // single shared light source cannot represent.
        let dim = WorldLight {
            sky: LightData::Uniform(4),
            block: LightData::Uniform(0),
        };
        let bright = WorldLight {
            sky: LightData::Uniform(15),
            block: LightData::Uniform(0),
        };
        let dim_light = WorldSectionLight::new(&dim, SkyDefault::None);
        let bright_light = WorldSectionLight::new(&bright, SkyDefault::None);
        let per_section: LightGrid<'_, WorldSectionLight> = core::array::from_fn(|ix| {
            core::array::from_fn(|iy| {
                core::array::from_fn(|iz| {
                    Some(if (ix, iy, iz) == (1, 1, 1) {
                        &dim_light
                    } else {
                        &bright_light
                    })
                })
            })
        });

        // The live default: one full-bright bridge for every slot.
        let bridge = UniformLight::pre_light_bridge();
        let uniform: LightGrid<'_, UniformLight> = core::array::from_fn(|_| {
            core::array::from_fn(|_| core::array::from_fn(|_| Some(&bridge)))
        });

        let per = snap.build_mesh(&SimpleClassifier, &per_section, true);
        let flat = snap.build_mesh(&SimpleClassifier, &uniform, true);

        assert_eq!(
            per.quad_count(),
            flat.quad_count(),
            "identical geometry — only the lighting differs"
        );
        assert_ne!(
            per.vertices, flat.vertices,
            "per-section light must change the vertex lighting the bridge cannot"
        );
    }

    #[derive(Debug)]
    struct SimpleClassifier;
    impl BlockClassifier for SimpleClassifier {
        fn classify(&self, state_id: u32, block_light: u8, sky_light: u8) -> Cell {
            if state_id == AIR {
                Cell {
                    occludes: false,
                    surface: None,
                    block_light,
                    sky_light,
                }
            } else {
                let mut c = Cell::solid(SpriteId(state_id as u16));
                c.block_light = block_light;
                c.sky_light = sky_light;
                c
            }
        }
    }

    fn air_section() -> Arc<ChunkSection> {
        Arc::new(ChunkSection::new(
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            AIR,
            0,
        ))
    }

    fn floor_section() -> Arc<ChunkSection> {
        let mut s = ChunkSection::new(PaletteKind::block_states(), PaletteKind::biomes(), AIR, 0);
        for x in 0..16 {
            for z in 0..16 {
                s.set_block(x, 0, z, STONE);
            }
        }
        Arc::new(s)
    }

    /// A source backed by a map; missing keys are `None` (unloaded/air).
    struct MapSource(std::collections::HashMap<SectionCoord, Arc<ChunkSection>>);
    impl SectionSource for MapSource {
        fn section(&self, coord: SectionCoord) -> Option<Arc<ChunkSection>> {
            self.0.get(&coord).cloned()
        }
    }

    #[test]
    fn neighbourhood_coords_are_the_27_offsets_centre_at_index_13() {
        let coords = neighbourhood_coords((5, 2, -3));
        assert_eq!(coords.len(), 27);
        assert_eq!(coords[13], (5, 2, -3), "centre is the 14th entry");
        assert!(coords.contains(&(4, 1, -4)));
        assert!(coords.contains(&(6, 3, -2)));
    }

    #[test]
    fn neighbour_columns_are_the_nine_around_a_load() {
        let cols = neighbour_columns(10, -7);
        assert_eq!(cols.len(), 9);
        assert!(cols.contains(&(10, -7)), "the loaded column itself");
        assert!(cols.contains(&(9, -8)));
        assert!(cols.contains(&(11, -6)));
        // Never a diagonal-of-diagonal.
        assert!(!cols.contains(&(12, -7)));
    }

    #[test]
    fn gather_pulls_the_centre_and_present_neighbours() {
        let mut map = std::collections::HashMap::new();
        map.insert((0, 0, 0), floor_section());
        map.insert((0, 1, 0), air_section());
        let src = MapSource(map);

        let snap = SectionSnapshot::gather(&src, (0, 0, 0));
        assert!(snap.is_drawable(), "centre has a stone floor");
        assert!(snap.centre().is_some());

        // A snapshot centred on empty space is not drawable.
        let empty = SectionSnapshot::gather(&src, (50, 50, 50));
        assert!(!empty.is_drawable());
    }

    #[test]
    fn absent_air_section_is_not_drawable() {
        let mut map = std::collections::HashMap::new();
        map.insert((0, 0, 0), air_section()); // present but all air
        let snap = SectionSnapshot::gather(&MapSource(map), (0, 0, 0));
        assert!(
            !snap.is_drawable(),
            "an all-air centre has nothing to draw even when loaded"
        );
    }

    #[test]
    fn build_batch_meshes_the_floor_and_air_together() {
        // Centre floor, with a lit-air section above so the top face is a clean
        // merged plane rather than fragmenting at the boundary.
        let mut grid: [[[Option<Arc<ChunkSection>>; 3]; 3]; 3] = Default::default();
        grid[1][1][1] = Some(floor_section());
        grid[1][2][1] = Some(air_section());
        let floor_job = MeshJob {
            coord: (0, 0, 0),
            snapshot: SectionSnapshot::from_grid(grid),
        };

        let air_job = MeshJob {
            coord: (0, 1, 0),
            snapshot: {
                let mut g: [[[Option<Arc<ChunkSection>>; 3]; 3]; 3] = Default::default();
                g[1][1][1] = Some(air_section());
                SectionSnapshot::from_grid(g)
            },
        };

        let built = build_batch(vec![floor_job, air_job], &SimpleClassifier, true);
        assert_eq!(built.len(), 2);
        let floor = built.iter().find(|b| b.coord == (0, 0, 0)).unwrap();
        let air = built.iter().find(|b| b.coord == (0, 1, 0)).unwrap();
        assert!(floor.mesh.quad_count() > 0, "the floor produced geometry");
        assert_eq!(
            air.mesh.quad_count(),
            0,
            "an all-air section meshes to nothing"
        );
        // Both are loaded, so both route the walk: air is fully open, and the
        // sparse floor (one layer of stone) is still open, not sealed.
        assert_ne!(
            air.visibility,
            SectionVisibility::NONE,
            "a loaded air section connects the walk"
        );
        assert_ne!(
            floor.visibility,
            SectionVisibility::NONE,
            "a one-layer floor does not seal the section"
        );
    }

    #[test]
    fn dirty_jobs_covers_loaded_sections_in_the_nine_columns_only() {
        let mut map = std::collections::HashMap::new();
        // Loaded column P=(0,0), two sections; one neighbour section; and a far
        // section outside the nine columns that must be ignored.
        map.insert((0, 0, 0), floor_section());
        map.insert((0, 1, 0), air_section());
        map.insert((1, 0, 0), floor_section()); // east neighbour column
        map.insert((5, 0, 5), floor_section()); // far away — not dirtied
        let src = MapSource(map);

        let jobs = dirty_jobs(&src, 0, 0, 0..2);
        let coords: Vec<SectionCoord> = jobs.iter().map(|j| j.coord).collect();

        assert!(coords.contains(&(0, 0, 0)), "P's own floor");
        assert!(coords.contains(&(0, 1, 0)), "P's own air (still routes)");
        assert!(coords.contains(&(1, 0, 0)), "east neighbour boundary");
        assert!(
            !coords.contains(&(5, 0, 5)),
            "a section outside the nine columns is untouched"
        );
        // No job for an absent slot, e.g. the west neighbour that was never loaded.
        assert!(!coords.contains(&(-1, 0, 0)));
        assert_eq!(jobs.len(), 3);
    }

    #[test]
    fn batch_build_is_deterministic_and_order_free() {
        // Identical jobs must produce identical geometry regardless of how the
        // pool schedules them — the property the `Arc` snapshot guarantees and
        // the one parallelism could break. (Exact quad count depends on the
        // full neighbour surround, which is not the point here.)
        let jobs: Vec<MeshJob> = (0..8)
            .map(|i| {
                let mut g: [[[Option<Arc<ChunkSection>>; 3]; 3]; 3] = Default::default();
                g[1][1][1] = Some(floor_section());
                g[1][2][1] = Some(air_section());
                MeshJob {
                    coord: (i, 0, 0),
                    snapshot: SectionSnapshot::from_grid(g),
                }
            })
            .collect();
        let built = build_batch(jobs, &SimpleClassifier, true);
        assert_eq!(built.len(), 8);
        let first = built[0].mesh.quad_count();
        assert!(first > 0, "the floor produced geometry");
        for b in &built {
            assert_eq!(
                b.mesh.quad_count(),
                first,
                "every identical job must mesh identically ({:?})",
                b.coord
            );
        }
    }
}
