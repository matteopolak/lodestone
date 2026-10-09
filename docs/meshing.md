# Section meshing

## What it is

The shell terrain mesher turns immutable section neighbourhoods into packed face or baked block-model geometry. Snapshot capture stays on the owning world thread; pure geometry work runs in the native worker pool or the bounded browser drain.

## How it works

### Modules and arrival policy

- `snapshot.rs` captures the 3x3x3 section neighbourhood, distinguishing an unloaded streaming column from a complete world's true air edge. `face.rs` emits packed full cubes, `model.rs` adapts baked models, tint and visibility, `fluid.rs` emits water and lava. The parent `mesher` module owns the scheduler, result generations, dirty-column policy and ECS integration.
- Newly arriving columns enter `pending_arrivals`, apart from the ready `dirty_columns`. The frame drain admits arrivals within one column of the player first; with an incomplete halo it builds a provisional mesh against air (rebuilt when the neighbour arrives). Farther arrivals wait for the halo.
- Column admission reads maintained section occupancy under the world read lock: known-air sections settle immediately (cancellation and GPU removal), only non-empty ones get late-capture intents.

### Light-driven remeshing

Order in `add_presentation_systems`: `relight_changed_blocks` computes and queues light changes, `heal_dirty_columns` captures columns, `remesh_light_dirty_sections` captures uncovered sections. A full-column capture consumes pending light intents for the sections it reads, so one state never creates two jobs; a change arriving after capture still creates new work.

- `World::merge_light_changes` compares old and incoming nibbles once. Each `LightSectionChange` carries the union of both layers' `LightBoundaryMask`: 27 target-offset bits plus three pairs of 4-bit changed-cell bounds in eight bytes, including neighbours for local coordinates 0/1 and 14/15. Face changes union without inventing diagonals. Anything involving `LightData::Missing` stays conservative; identical stored values emit no change even if representation differs.
- The modern adapter forwards `ClientEvent::ChunkLightChangedPrecise` to `TerrainMesh::queue_light_changes`. The legacy `merge_light_changed`, `ChunkLightChanged` and `queue_light_update` stay index-based; sinks without nibble readback use whole-section masks.
- A section is queued only if non-air blocks lie within two cells of the changed bounds (two cells cover custom quads whose cull and geometric axes differ). Bounds are expanded by two, translated per destination, intersected with the 16^3 domain; rejection needs a classifier proof that the air state has no quads or fluid. Pending arrival, heal or forced-column work absorbs light changes first. This filters light work only, never geometry requests, residency or upload acknowledgements.
- Light section `i` is block-section `i - 1`, including the below/above-world sentinel layers; fan-out adds the offset before clipping and never clamps a sentinel onto a real section.
- Local relight keeps changed-cell bounds in `Relit::light_changes` (by source column and packet light-section index), unioned during writeback and merged in the drain; the aggregate is transient. Already-written local values must not go through incoming patch merge (it would erase the diff).
- `MeshWorkCounters::local_light_admission` separates candidates, queued, spatial and sampled-input rejections, absorbed, coalesced and block reads; the browser probe exposes `sessionLocal*`. Already-queued destinations coalesce before occupancy scanning. Compare counter deltas with mesh-pass totals before claiming savings.

**Sampled-light admission (opt-in).** Retains the light inputs each successfully handed-off model or fluid mesh sampled; a later patch reads only retained samples inside its changed bounds and skips the job if resolved values are unchanged. Domain `[-2,17]^3`; under 250 samples use 4-byte records, larger uniform sets a 1,000-byte bitmap plus one value, mixed sets at most 512 records. Each capture has a request revision; new captures, intents, empty settlement, reset and option rebuilds retire prior samples, and only the current successful handoff publishes replacements (so a pending 3-to-11-to-3 update is not suppressed using the original mesh). Enable with `LODESTONE_MESH_LIGHT_INPUTS=1` (native startup or Wasm compile time); off by default pending live measurement. Counters: `MeshWorkCounters::light_input_*`; debug browser reports charge `light_inputs` time. Code: `mesher/light_reads.rs`, `TerrainMesh`, `LightBoundaryMask::changed_cells`.

### Scheduling

- Browser admission enqueues section intents (key, section count, forced flag, priority; no block or light handles). Column, edit and light invalidations replace one indexed payload per section; an edit promotes the slot into the edit band, keeping its first submission age, and forced admission survives replacement. `TerrainMesh::drain_meshes_with_world` takes the world read lock for one neighbourhood, releases it, applies empty/deferred policy and meshes immediately, under a shared 4 ms deadline (one expensive section can exceed it but cannot block progress). Unload, re-arrival and reset discard obsolete work. A successful current `Ready` capture consumes a matching light intent; older snapshots, empty outcomes and deferred captures keep it.
- Native and browser share `MeshPriority` and `FairMeshOrder`: edits and their boundary neighbours precede background arrivals, lighting and upload retries, yielding to background after four edit selections. Native workers have independent order state; completion handoff has its own and obeys count, examination and byte limits. A newer snapshot inherits outstanding edit priority only until the authoritative result settles.
- Each native submission has an atomic cancellation token, cancelled on replacement/forget on the owning thread and checked before geometry compute. A skipped job still sends a completion acknowledgement (counted against the examination limit); computing jobs finish, and every built result passes the generation check. Generations describe submissions, not world mutations.
- `TerrainMesh::work_counters` exposes `MeshWorkCounters::native_scheduler` (`submitted`, `started`, `skipped_before_mesh`, `stale_results_discarded`); they are independent atomics, not a transaction. Preserve one completion per admitted job and the post-compute generation check.

### Upload and fingerprints

The worker fingerprints each section's vertex buffers, index buffers and visibility (fast non-cryptographic 128-bit, `xxhash-rust`, kept for the loaded lifetime; direct renderer uploads hash at the call site, the production path never on the frame thread). An identical result settles without rewriting GPU buffers; unloading or replacing the atlas clears it. Empty sections request GPU removal only if a prior mesh exists.

Model uploads reuse a resident origin slot and release superseded spans. A fresh section without an origin releases all three new layers and returns `SectionUploadOutcome::Failed`, with no fingerprint cached. `TerrainMesh::retry_mesh_upload` re-enters ordinary invalidation; its `had_resident` must come from real renderer residency, not presentation-readiness markers (re-arrival clears those while old geometry remains).

### Shortcuts that must stay conservative

- `mesh_snapshot_fluids_at` returns empty fluid layers without filling the padded grid when every centre-palette state is valid and dry; unused wet entries, direct storage and invalid ids fall through.
- Model and fluid passes borrow one fixed 27-slot light view per snapshot, each with its own tint cursor. No model scan runs when every validated palette state has no baked quads; fluids run independently.
- A palette proof (every state valid, fully occluding, culled direction on every quad) limits model traversal to the 1,352 boundary cells of a 16^3 section. Unculled custom geometry, partial shapes, cutouts, unknown ids and direct storage keep full traversal. Keep the unculled-model negative control.
- Visibility uses maintained occupancy only when the air state is non-occluding or a palette proves uniform occlusion. Mixed opacity uses an allocation-free boundary flood in `lodestone_render::visibility::compute_visibility_from` (512-byte bitset, 8 KiB queue, cells marked on enqueue so at most 4,096 entries).
- Resource reload uses the current model table, never cached proofs.

### Measurement diagnostics

Debug-level `frame_profile` logging (browser `?log=debug`) enables two reports, with the disabled path free of clocks and allocations:

- `mesh arrival` (`mesher/arrival_measurement.rs`): shell observation to first admission eligibility, then to a capture attempt. It excludes server generation, worker/GPU completion and presentation. At most 512 lifetimes are kept; replacements restart one, unloads cancel, overflow is counted, not fabricated. Exposed as `meshArrival`; cumulative totals must not be summed across reports.
- `mesh native` (`mesher/native_timing.rs`): per lane (background, edit) queue wait, compute, completion residence and upload CPU time, with `Applied`/`Unchanged`/`Failed` counted separately. Cancelled work records wait only; stale geometry records compute but no upload; overflow is measured once at final settlement. These are CPU observations, not GPU or compositor time.

## How to change it

- Worker inputs are `SectionSnapshot` only; `lodestone_shell::mesher` re-exports are compatibility seams. A new snapshot field must be copied through every constructor and be `Send`; a new `SectionGeometry` variant or pass needs its consumers updated. Any output field that changes pixels or occlusion must enter `SectionGeometry::fingerprint` or real updates are discarded.
- Light admission must keep remeshing loaded non-air neighbours including diagonal and vertical ones. The precise mask assumes reads stay in section-relative `[-2,17]`; widening a sampler's radius means widening both packet and local invalidation contracts.
- Browser frames call `drain_meshes_with_world`; headless unbudgeted work uses `drain_all_meshes_with_world`. Call `drain_removals` after the drain as well as before (a drain can discover an empty replacement) before testing readiness.
- A deferred capture still follows arrival recovery: unseen geometry waits for `mark_neighbours_dirty`, uploaded geometry rebuilds immediately.
- Controls to keep when touching these paths: `column_capture_absorbs_light_intent_and_later_patch_still_rebuilds` (sky 3 stores as byte 51, later 11 as 187), `coalesced_intents_capture_latest_world_once_instead_of_retaining_old_sections` (18 quads from one queue pop), `late_capture_preserves_deferred_admission_and_cancels_unloaded_or_rearriving_work`, `deferred_column_capture_keeps_the_arrival_retry_and_uploaded_rebuild`, a held-worker control (cancellation off: 3 computations and 2 stale discards; on: 1 computation and 2 skips; both hand off one 18-quad mesh), and the ignored GPU test `model_origin_exhaustion_releases_all_new_mesh_spans` (912 arena bytes retained; without rollback 1,824). Dry-centre controls include an isolated water cell with exactly 11 quads and a waterlogged fixture that must keep fluid; `dry_center_fluid_timing_control` is an ignored native diagnostic, not browser frame-time evidence.
- The mask and fan-out controls derive expected offsets independently from padded-box inequalities (faces, edges, corners, sentinels, whole-section).

## Configuration

- `ModelSectionView::interior_quads_are_culled` defaults false; snapshot adapters derive it from palette and model table.
- `MeshScheduler` takes worker count and classifier; `MeshPolicy` controls dirty-column admission; native jobs stamp `cutout_leaves` and `blend_radius` at submission, browser requests read current options. `ColumnSource` controls deferral; `PROVISIONAL_FIRST_MESH_RADIUS` bounds early admission to one column; `SkyDefault` sets absent sky fallback; `MESH_SNAPSHOT_SECTION_BUDGET` bounds frame section visits.
- Native handoff targets 2 ms of upload work (EWMA per-section cost, 50 us initial), at most 96 results and 16 MiB of geometry per frame; overflow stays queued and the first result is always allowed. Browser uses `BROWSER_MESH_BUDGET` (4 ms). Readiness is `Sim::mark_mesh_uploaded`, not CPU completion.
- `LODESTONE_MESH_LIGHT_INPUTS=1` enables sampled-light admission.
- Benchmarks: `cargo bench -p lodestone-render --bench model_shell` and `--bench visibility` (retired instructions and cycles on macOS; `-- --full-only` for the fallback alone); not a browser frame-time measurement.

## Dependencies

`lodestone-world` snapshots and light data, `lodestone-render` mesh APIs, shell classifiers and network state, Bevy ECS, `xxhash-rust`; native uses `crossbeam-channel` workers, `wasm32` the in-frame budgeted scheduler.
