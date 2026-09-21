# Terrain loading readiness

## What it is

The labelled `Loading terrain...` overlay is reserved for the first creation of a survival singleplayer world. It releases only when every column in the declared initial view is resident and renderer-settled. A non-empty section must be CPU-meshed and handed to `RenderState`; an all-air section must be explicitly classified as empty. Existing saves, creative/hardcore creations, and multiplayer joins do not get a fake chunk counter or grid.

Dimension travel has a separate opaque transition cover. It hides the old dimension while the destination is installed, but deliberately has no initial-world label, progress bar, or chunk grid. This keeps a portal transition from looking like first-world generation and prevents a remote join from getting stuck behind a local progress denominator.

## How it works

`TerrainMesh` keeps a per-section settlement ledger keyed by `SectionKey`. A ready mesh enters the renderer ledger only after `app/redraw.rs` calls `RenderState::upload_section`; a `SnapshotOutcome::Empty` enters the empty ledger without an upload. Deferred or queued sections remain absent from both ledgers. `TerrainMesh::column_mesh_settled` checks the decoded local column and every section in the active `ChunkWorld` extent, so scheduler-wide pending counts, visible-section counts, and unrelated uploaded columns cannot release the gate.

After the frame drains removals and uploads, `Sim::refresh_terrain_readiness` evaluates every column in the declared view. The result is a one-way producer latch for the current dimension. `Sim::terrain_wait` still requires the player's decoded column while `TerrainProgressTracker` remains telemetry for the loading bar and never substitutes for section settlement. A session with no declared view retains the own-column fallback.

`Sim` carries two presentation latches next to the producer readiness state: `new_world_loading` is armed only by the newly-created survival launch path, while `dimension_transition_pending` is armed by a cross-dimension respawn. The renderer checks the transition latch before the ordinary world wait and draws its opaque cover independently. The producer readiness state itself remains with the mesh ledger rather than duplicating section state in the simulation struct.

Focus loss does not turn either loading cover into the pause overlay. The window lifecycle checks initial-world ownership, `Sim::world_wait`, and `Sim::dimension_transition_pending` before applying pause-on-lost-focus, so only a presentable `Screen::Playing` state can become `Screen::Paused`; the full-frame `Screen::Connecting` state is guarded by the same state-machine transition.

The initial-world grid uses the selected render distance, capped at `MAX_GRID_RADIUS`, and is centred on the canvas. The session streams one neighbour ring for horizontal meshing, but that implementation ring is not shown as an extra player-facing distance. Cells use a typed status enum and the complete twelve-colour palette; network paths may currently expose only empty/full observations, but no status is collapsed through string comparisons or a uniform colour.

Dimension changes use the existing `Sim::apply_respawn` edge detector. Before old-world removals can be presented, `reset_for_dimension_change` clears the producer latch and calls `TerrainMesh::end_session`, which discards in-flight work and the old section ledger. Destination packets build a new ledger. The cover remains active through the transition and has no timeout escape. Same-dimension death respawns do not reset terrain.

The transition boundary depends on the wire event order: a changed-dimension respawn must identify the destination before old-column forget notifications are applied. Ordinary view unloads and same-dimension respawns remain immediate and do not reset the destination ledger.

## How to change it

Change the section ledger and its column predicate in `crates/lodestone-shell/src/mesher.rs`. Change the `Sim` facade and post-drain readiness check in `crates/lodestone-shell/src/sim/meshing.rs`, and keep the renderer acknowledgement immediately after `RenderState::upload_section` in `crates/lodestone-shell/src/app/redraw.rs`. Session observations belong in `crates/lodestone-shell/src/sim/session.rs`; launch scope belongs in `crates/lodestone-shell/src/app/session.rs`; the remote phase clamp belongs in `crates/lodestone-shell/src/app/menus.rs`; transition reset behavior belongs in `crates/lodestone-shell/src/sim/dimension.rs`. Grid geometry and palette live in `crates/lodestone-shell/src/menu/render/`. If the event transport changes, preserve the transition boundary before old-column removal and keep ordinary unloads and same-dimension respawns as controls.

Do not derive readiness from the progress numerator, the scheduler's global pending count, or the set of visible sections. A fresh decode of a column must clear that column's old ledger before it is remeshed. If the renderer hand-off boundary changes, update the acknowledgement and the negative controls together.

## Configuration

There are no new flags or environment variables. `config.render_distance` chooses the initial-world square (currently bounded by `MAX_GRID_RADIUS`). Readiness has no timeout; incomplete terrain or asset work keeps the cover visible.

## Dependencies

The predicate relies on `ChunkWorld` residency and build extent, `TerrainMesh` snapshot routing, `RenderState::upload_section`, and the loading state assembled by `Sim::terrain_wait` and `Sim::world_wait`. The launch scope relies on the typed `SingleplayerLaunch` and `WorldGameMode` values, not a connection heuristic. The transition boundary relies on packet event ordering, while the progress bar consumes `TerrainProgressTracker` independently as telemetry and the grid consumes typed per-column statuses.
