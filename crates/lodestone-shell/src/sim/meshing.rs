//! `Sim`'s meshing cluster: `dirty_sections_for_blocks` (the section-invalidation
//! filter), block read/write against the chunk store (`block_at_world`,
//! `set_block_world`), re-mesh scheduling (`remesh_around`, `remesh_section`,
//! `remesh_changed_blocks`, `on_column_arrived`, `mark_column_dirty`), and
//! placement-prediction reconciliation (`reconcile_predictions`) — seam 7,
//! the last of the sim.rs decomposition sequence (seam 1 was the test
//! module, `sim/tests.rs`; seam 2 was placement prediction,
//! `sim/placement.rs`; seam 3 was the interaction/combat cluster,
//! `sim/actions.rs`; seam 4 was the net-apply cluster, `sim/net_apply.rs`;
//! seam 5 was the audio cluster, `sim/audio.rs`; seam 6 was the camera
//! cluster, `sim/camera.rs`).
//!
//! `use super::*;` for the same reason every other seam file uses it:
//! `sim::meshing` is a descendant of `sim` and already has the same
//! visibility into `Sim`'s private fields and `sim.rs`'s other private
//! helpers that the earlier seams have.
//!
//! `dirty_sections_for_blocks` is `pub(crate)` here (it was a plain private
//! `fn` in `sim.rs`) and re-exported the same way `placement::is_air_state`
//! and `camera::fog_for_render_distance` are: `sim/tests.rs`'s
//! `dirty_sections_for_blocks(...)` calls cross the new sibling boundary, and
//! `sim.rs`'s own private `use meshing::dirty_sections_for_blocks;` re-enters
//! its `use super::*;` glob — no `pub use` needed since nothing outside this
//! crate names it.
//!
//! Five methods widen from private to `pub(crate)`: `block_at_world` and
//! `set_block_world` are called from `sim.rs`'s own `crack_target` (this
//! module's *parent* now — privacy only cascades downward, the rule every
//! earlier seam's doc repeats) and from `sim/actions.rs`/`sim/tests.rs`
//! (siblings); `remesh_around` from `sim/actions.rs`; `remesh_changed_blocks`
//! and `reconcile_predictions` from `sim/net_apply.rs`'s `poll_net`;
//! `on_column_arrived` and `mark_column_dirty` from both `sim/net_apply.rs`
//! and `sim/tests.rs`. `remesh_section` stays private: its only callers
//! (`remesh_around`, `remesh_changed_blocks`) moved here with it.
//!
//! **`remesh_around`'s own body later moved again**, out of `sim` entirely
//! and into [`TerrainMesh::remesh_around`](crate::mesher::TerrainMesh::remesh_around)
//! — it had reduced to pure `ChunkWorld`/`TerrainMesh` math with nothing else
//! of `Sim`'s in it, once its World-guard plumbing is factored through
//! [`Sim::terrain_and_world`]. What is left here is the one-line delegation;
//! `remesh_section` is unaffected and still backs `remesh_changed_blocks`.

use super::*;

/// Which section meshes a set of changed cells invalidates.
///
/// A section's geometry is a function of its whole 3×3×3 neighbourhood (face
/// culling reads the 6 face-adjacent sections; AO samples the 3 cells around
/// every vertex corner, which reach across section *edges and corners* too), so
/// a changed cell dirties its own section **plus** every neighbour section it
/// physically touches — and no others. A cell at local x=15 touches the +x
/// neighbour; an interior cell touches nothing else. Skipping the neighbour is
/// the defect that leaves a stale face at a chunk border while mining on a live
/// server; dirtying all 27 unconditionally pays a 27× re-mesh for every redstone
/// tick. Hence the per-axis filter rather than either extreme.
///
/// Coordinates are **section-relative** (`0..=15`), matching the wire form of
/// `SECTION_BLOCKS_UPDATE`, and the result is in absolute section coordinates.
pub(crate) fn dirty_sections_for_blocks(
    sx: i32,
    sy: i32,
    sz: i32,
    blocks: &[[u8; 3]],
) -> BTreeSet<(i32, i32, i32)> {
    let mut dirty: BTreeSet<(i32, i32, i32)> = BTreeSet::new();
    for &[bx, by, bz] in blocks {
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if (dx == -1 && bx != 0) || (dx == 1 && bx != 15) {
                        continue;
                    }
                    if (dy == -1 && by != 0) || (dy == 1 && by != 15) {
                        continue;
                    }
                    if (dz == -1 && bz != 0) || (dz == 1 && bz != 15) {
                        continue;
                    }
                    dirty.insert((sx + dx, sy + dy, sz + dz));
                }
            }
        }
        // Every further cell can only add sections already reachable from a full
        // 3×3×3, so once all 27 are queued there is nothing left to find. This
        // is what bounds a 4096-cell `SECTION_BLOCKS_UPDATE` to 27 re-meshes.
        if dirty.len() == 27 {
            break;
        }
    }
    dirty
}

impl Sim {
    /// The block state id at a world position, or air when the column is not
    /// loaded or the y is outside the build range.
    pub(crate) fn block_at_world(&self, block: [i32; 3]) -> u32 {
        let pos = ChunkPos {
            x: block[0].div_euclid(16),
            z: block[2].div_euclid(16),
        };
        let store = self.chunk_world();
        let world = store.read();
        let Some(chunk) = world.get(pos) else {
            return id::AIR;
        };
        let col = &chunk.column;
        if block[1] < col.min_y() || block[1] >= col.max_y() {
            return id::AIR;
        }
        lodestone_world::BlockVolume::block(
            col,
            block[0].rem_euclid(16) as usize,
            block[1],
            block[2].rem_euclid(16) as usize,
        )
    }

    /// Write a block into the chunk store. Offline-world editing only: on a live
    /// session the server is authoritative and the edit arrives as a block-update
    /// packet.
    ///
    /// There is nothing to invalidate afterwards. Before Stage 4 this was the one
    /// write path to `Sim.world` and therefore the one place the cached offline
    /// collision clone had to be cleared by hand — a missed clear reading as "I
    /// mined the block but still cannot walk through it". The collision source now
    /// reads the store itself, so the rule is gone rather than merely obeyed.
    ///
    /// # Why this does *not* call `sync_block_entity`, unlike every other writer
    ///
    /// `value` is a [`crate::blocks::id`] constant — the shell's **own** ten-entry
    /// demo palette, deliberately unrelated to any protocol's ids (see that
    /// module's docs). Running it through `lodestone_data`'s 26.2
    /// `state_id → block_entity_type` census would be a category error: `id::WATER`
    /// is `5`, and real state `5` is some unrelated 26.2 block that may well own a
    /// block entity. So the demo world has no block entities, correctly — the
    /// palette contains nothing that could have one. The live prediction's writer
    /// is [`write_predicted_block`], which is fed real census state ids.
    pub(crate) fn set_block_world(&mut self, block: [i32; 3], value: u32) -> bool {
        let pos = ChunkPos {
            x: block[0].div_euclid(16),
            z: block[2].div_euclid(16),
        };
        let store = self.chunk_world_write();
        let mut world = store.write();
        let Some(chunk) = world.get_mut(pos) else {
            return false;
        };
        let col = &mut chunk.column;
        if block[1] < col.min_y() || block[1] >= col.max_y() {
            return false;
        }
        col.set_block(
            block[0].rem_euclid(16) as usize,
            block[1],
            block[2].rem_euclid(16) as usize,
            value,
        );
        true
    }

    /// Re-snapshot and re-schedule the section holding `block`, plus any
    /// neighbour section that shares the boundary the block sits on (a face on a
    /// section edge changes the neighbour's mesh via culling/AO). Sections that
    /// became all-air are queued for GPU removal instead.
    ///
    /// A one-line delegation through [`Sim::terrain_and_world`] since the
    /// re-mesh seam moved: the 3×3×3 boundary filter and extent math that used
    /// to live here touched only [`ChunkWorld`] and [`TerrainMesh`] and no
    /// other `Sim` state, so it moved to [`TerrainMesh::remesh_around`]
    /// (`mesher.rs`) — see that method's doc for why this is where it stops
    /// (never a plugin-callable primitive).
    pub(crate) fn remesh_around(&mut self, block: [i32; 3]) {
        self.terrain_and_world(|store, terrain| terrain.remesh_around(store, block));
    }

    /// Push vanilla's **See-Through Leaves** option down from the menu layer
    /// — `options.cutoutLeaves`, `false` (FAST) means leaves render solid.
    ///
    /// Called once per presented frame like [`Self::set_view_bobbing`], but
    /// unlike every one of that family's siblings this one can be genuinely
    /// expensive: [`TerrainMesh::set_cutout_leaves`]'s own equality guard is
    /// what keeps an unconditional per-frame call from re-meshing the whole
    /// loaded world every frame — see that method's doc.
    pub fn set_cutout_leaves(&mut self, cutout_leaves: bool) {
        self.terrain_and_world(|store, terrain| terrain.set_cutout_leaves(cutout_leaves, store));
    }

    /// Push vanilla's `options.biomeBlendRadius` down to the mesh layer, the
    /// same shape as [`Self::set_cutout_leaves`] just above and with the same
    /// per-frame-poll contract: `TerrainMesh::set_blend_radius`'s own equality
    /// guard is what keeps this affordable, because a real change re-meshes
    /// every loaded column.
    pub fn set_blend_radius(&mut self, radius: i32) {
        self.terrain_and_world(|store, terrain| terrain.set_blend_radius(radius, store));
    }

    /// Reloads the block/model atlas and the classifier the mesh workers use
    /// from whatever resource packs are currently selected
    /// (`crate::resources::selected_packs`), respawning the worker pool and
    /// force-remeshing every loaded column against the new atlas —
    /// the sim-side half of a live resource-pack reload.
    /// `crate::menu::packs::commit`'s own doc used to name this as the
    /// missing piece ("this client has no live reload"); this is that piece.
    ///
    /// Called once per presented frame, like [`Self::set_cutout_leaves`], but
    /// **the equality guard here is [`crate::resources::pack_generation`]
    /// rather than a value comparison** — `set_selected_packs` bumps it, so
    /// this is a no-op on every frame except the one after a real selection
    /// change (or, once a pack-folder watch exists, a real file change).
    ///
    /// Also a no-op, loud once, on:
    /// - the demo world (no `net`) — there is no server world to re-texture,
    ///   and the demo palette never depends on a resource pack;
    /// - a session with no vanilla atlas to begin with (a jar-less run,
    ///   already on the demo-palette fallback) — swapping an atlas that does
    ///   not exist needs a full re-classification, not a reload;
    /// - a reload whose own `BlockResources::load(true)` falls back to the
    ///   demo palette (an unreadable or corrupt pack) — the *previous*,
    ///   working atlas is kept rather than silently downgrading a live
    ///   session to the id space `Sim::refresh_mesh_policy`'s
    ///   `id_spaces_agree` says a live session must never use.
    ///
    /// Returns the freshly loaded atlas on a real reload, so the caller
    /// (`WindowApp::redraw`) knows to also swap the GPU-side atlas bind
    /// groups and reattach the GUI atlas — this method has already forced
    /// the world's own remesh, which is the one piece a GPU-only swap could
    /// never reach (a rebuilt atlas moves every sprite's UVs, and baked
    /// terrain geometry does not re-bake itself).
    #[must_use]
    pub fn reload_resource_pack_atlas(&mut self) -> Option<Arc<BlockAtlas>> {
        let generation = crate::resources::pack_generation();
        if generation == self.last_pack_generation {
            return None;
        }
        self.last_pack_generation = generation;
        if self.net.is_none() || self.vanilla_atlas.is_none() {
            return None;
        }
        let resources = BlockResources::load(true);
        let BlockResources {
            classifier,
            vanilla_atlas,
            language,
            banner: _,
            // Deliberately not reloaded here — see the module-level note on
            // why this reload's scope stops short of particles: `Particles`'
            // own `(Sheet, frame) -> UV` table would need rebuilding in step
            // with any new particle atlas, and drifting the two apart is
            // exactly issue #45.
            particle_atlas: _,
        } = resources;
        let Some(atlas) = vanilla_atlas else {
            tracing::warn!(
                target: "assets",
                "resource pack reload fell back to the demo palette; keeping \
                 the previous atlas rather than mid-session downgrading a \
                 live server to the demo id space"
            );
            return None;
        };
        let worker_count = std::thread::available_parallelism()
            .map(|n| n.get().max(1))
            .unwrap_or(2);
        self.terrain_and_world(|store, terrain| {
            terrain.reload_classifier(store, worker_count, classifier);
        });
        self.language = language;
        self.vanilla_atlas = Some(Arc::clone(&atlas));
        Some(atlas)
    }

    /// Re-snapshot and re-schedule one section. A section that snapshots to
    /// nothing is queued for GPU removal rather than left showing stale geometry.
    ///
    /// One path, not two: before Stage 4 this branched on `vanilla_atlas &&
    /// net && world_dimensions` to pick which of the two `World`s to read.
    fn remesh_section(&mut self, cx: i32, cz: i32, si: usize, min_y: i32, section_count: usize) {
        let key = SectionKey { cx, cz, si, min_y };
        self.terrain_and_world(|store, terrain| terrain.mesh_section(store, key, section_count));
    }

    /// Re-mesh after a server-authoritative edit inside section
    /// `(sx, sy, sz)`, where `blocks` are the section-relative coordinates of
    /// every changed cell.
    ///
    /// Section granularity, not column: this is the signal every redstone tick
    /// carries, and a whole-column re-mesh is ~24 sections each snapshotting a
    /// 27-section neighbourhood. A cell on a section face also dirties the
    /// section across that face — culling, AO and fluid corner heights all read
    /// across the boundary — so an edit at local x=15 fixes the neighbouring
    /// column's seam too, which a column-scoped signal cannot express. Keys are
    /// deduplicated first, so a 4096-cell update still submits at most 27
    /// snapshots.
    pub(crate) fn remesh_changed_blocks(&mut self, sx: i32, sy: i32, sz: i32, blocks: &[[u8; 3]]) {
        let Some(extent) = self.chunk_world().extent() else {
            return;
        };
        let base_si = extent.min_y.div_euclid(16);
        for (nsx, nsy, nsz) in dirty_sections_for_blocks(sx, sy, sz, blocks) {
            let si = nsy - base_si;
            if si < 0 || si as usize >= extent.section_count {
                continue;
            }
            self.remesh_section(nsx, nsz, si as usize, extent.min_y, extent.section_count);
        }
    }

    /// Handle a chunk-arrival signal for `(cx, cz)`: mesh that column now, and
    /// queue its **loaded horizontal neighbours** for a boundary re-mesh.
    ///
    /// A section's geometry is a function of its whole 3×3×3 neighbourhood, so a
    /// column that was meshed while `(cx, cz)` was still absent baked its seam
    /// against air. Left alone that is permanent, and it is exactly what a
    /// play-test sees: **water grows a falling "wall" at every chunk border**
    /// (the neighbour cell reads as no-fluid, so the side face is emitted and the
    /// corner heights collapse), plus wrong cross-chunk AO and stray culled
    /// faces. The tell that it is a staleness bug and not a mesher bug is that
    /// breaking any block in the column fixes it — [`Sim::remesh_around`] already
    /// re-meshes neighbours.
    ///
    /// The centre column meshes immediately (load responsiveness); the eight
    /// neighbours are coalesced into `TerrainMesh::dirty_columns` and drained on a
    /// budget by the `heal_dirty_columns` system, so a spiral load re-meshes each
    /// column a small constant number of times instead of nine.
    pub(crate) fn on_column_arrived(&mut self, cx: i32, cz: i32) {
        self.mark_column_dirty(cx, cz);
        self.terrain_and_world(|store, terrain| terrain.mark_neighbours_dirty(store, cx, cz));
    }

    /// Handle a `ChunkLoaded` / [`NetUpdate::Chunk`] dirty-region signal: the
    /// column at `(cx, cz)` changed, so re-mesh every section it holds.
    ///
    /// **One path since Stage 4.** This used to be two, chosen by
    /// `vanilla_atlas.is_some() && net.is_some() && world_dimensions().is_some()`:
    /// one reading the client-owned world through `NetClient`, one reading `Sim`'s
    /// own. With a single [`ChunkWorld`] store there is one world to read, and the
    /// only thing the old guard genuinely encoded — *is the mesh classifier's
    /// block-id space the store's?* — survives as `MeshPolicy::id_spaces_agree`.
    /// Light stays server-authoritative on the live path: nothing here recomputes
    /// it (that would overwrite the server's seam-complete cross-chunk light with a
    /// partial result — a divergence bug). Multiplayer *consumes* light;
    /// singleplayer computes it.
    pub(crate) fn mark_column_dirty(&mut self, cx: i32, cz: i32) {
        self.terrain_and_world(|store, terrain| terrain.mesh_column(store, cx, cz));
    }

    /// Handle a [`NetUpdate::ChunkUnloaded`] signal: drop every GPU section the
    /// column at `(cx, cz)` still owns.
    ///
    /// Deliberately *not* a `terrain_and_world` call, unlike every other method
    /// in this cluster: [`TerrainMesh::forget_column`] takes no store, because
    /// the column has already left it. Threading a `&ChunkWorld` in here would
    /// invite the natural-looking implementation that enumerates the column's
    /// sections from `store.extent()` — which enumerates nothing, silently, and
    /// would reproduce the bug with a fix-shaped commit in front of it.
    pub(crate) fn on_column_unloaded(&mut self, cx: i32, cz: i32) {
        self.terrain_mut(|terrain| terrain.forget_column(cx, cz));
        // The mirror of `on_column_arrived`'s second line, and #479's second
        // half. An arrival re-drives the neighbours that baked their seam against
        // air; a departure has to re-drive the neighbours that are still *waiting*
        // on this column, because it is never coming back and a plain dirty signal
        // would defer them again and drop the result. See
        // `TerrainMesh::forced_columns`.
        self.terrain_and_world(|store, terrain| {
            terrain.force_neighbours_of_departed(store, cx, cz)
        });
    }

    /// Settle any placement prediction the server has just overwritten.
    ///
    /// [`NetUpdate::SectionBlocks`] is the shell's view of `BLOCK_UPDATE` /
    /// `SECTION_BLOCKS_UPDATE`: the authoritative state has **already** been
    /// applied to the one store by the adapter, which (since #374) already created
    /// or removed the block entity with it. So this does not correct the world —
    /// the world is corrected by construction, including a refused placement whose
    /// bogus chest record is dropped by that arm's `sync_block_entity` — it only
    /// clears the prediction from [`Placement`]'s ledger and asks whether the
    /// server agreed.
    ///
    /// Both halves matter. Without the clear the ledger grows without bound for the
    /// whole session, one entry per right-click, because nothing else drains it (the
    /// `block_changed_ack` sequence is decoded by the adapter but has no shell
    /// consumer). Without the answer a refusal is invisible.
    pub(crate) fn reconcile_predictions(&mut self, sx: i32, sy: i32, sz: i32, blocks: &[[u8; 3]]) {
        let pending: Vec<BlockPos> = self.read(|w| {
            w.resource::<PlacementPredictor>()
                .0
                .pending()
                .iter()
                .map(|prediction| prediction.pos)
                .collect()
        });
        // The common case by far — one `O(1)` read, and a `/fill` of 4096 cells
        // does no per-cell work at all.
        if pending.is_empty() {
            return;
        }
        for &[rel_x, rel_y, rel_z] in blocks {
            let pos = BlockPos::new(
                (sx << 4) | i32::from(rel_x),
                (sy << 4) | i32::from(rel_y),
                (sz << 4) | i32::from(rel_z),
            );
            if !pending.contains(&pos) {
                continue;
            }
            let server_block = self
                .net
                .as_ref()
                .and_then(|net| net.block_at(pos))
                .and_then(lodestone_data::block_states::block_name)
                .and_then(|name| name.parse::<lodestone_model::Identifier>().ok());
            let outcome = self.write(|w| {
                w.resource_mut::<PlacementPredictor>()
                    .0
                    .reconcile(pos, server_block.as_ref())
            });
            if outcome.corrected {
                tracing::debug!(
                    target: "placement",
                    "server overrode the predicted block at {:?} with {:?}",
                    pos,
                    server_block
                );
            }
        }
    }
}
