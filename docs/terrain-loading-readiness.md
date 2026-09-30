# Terrain loading readiness

## What it is

The labelled `Loading terrain...` overlay is reserved for the first creation of a survival singleplayer world. It releases when the initial spawn view, capped at radius six, is resident and renderer-settled; the rest of the selected render distance streams after play begins. A non-empty section must be CPU-meshed and handed to `RenderState`; an all-air section must be explicitly classified as empty. Existing saves, creative/hardcore creations, and multiplayer joins do not get a fake chunk counter or grid.

Dimension travel has a separate opaque transition cover. It hides the old dimension until the player's destination column is renderer-settled, without reusing the first-world progress bar or chunk grid. A remote server need not fill the advertised view square before the player can enter it.

## How it works

`TerrainMesh` keeps a per-section settlement ledger keyed by `SectionKey`. A ready mesh enters the renderer ledger only after `app/redraw.rs` calls `RenderState::upload_section`; a `SnapshotOutcome::Empty` enters the empty ledger without an upload. Deferred or queued sections remain absent from both ledgers. `TerrainMesh::column_mesh_settled` checks the decoded local column and every section in the active `ChunkWorld` extent, so scheduler-wide pending counts, visible-section counts, and unrelated uploaded columns cannot release the gate.

Each renderer, presented, and empty membership ledger uses `ColumnSectionSet`,
which groups packed section bits by column coordinates and vertical origin.
The first 64 section indices fit in an inline word; taller custom dimensions
grow numeric words rather than allocating a hash table of section keys. The
usual single-origin column needs no inner allocation. A column reset or unload
removes only that column's membership rather than scanning every resident
section. Distinct `min_y` values remain separate groups, and empty groups and
columns are removed. This is the ledger's storage representation, not a second
cache or index, and unloads need no lookup of an already removed world column.

Ongoing streaming also tracks presented coverage separately from latest-revision
settlement. Rebuilding a section invalidates the strict ledger but does not
erase geometry already handed to the GPU. A first arrival, differing or unclassified
column replacement, or unload clears both ledgers. A driver-classified replacement
with exactly equal column and light storage leaves both ledgers and pending work
intact; the complete world payload is still installed. The browser's `full-view-presented` event checks presented
coverage across the selected render distance after a frame is submitted;
background replacement meshes do not hold that event indefinitely. The
initial-world and dimension-transition covers retain the strict predicate.

After the frame drains removals and uploads, `Sim::refresh_terrain_readiness` evaluates every column in the declared first-world view. A dimension transition instead checks the player's destination column, including all sections in its current vertical extent. The result is a one-way producer latch for the current dimension. `Sim::terrain_wait` still requires the player's decoded column while `TerrainProgressTracker` remains telemetry for the loading bar and never substitutes for section settlement. A session with no declared view retains the own-column fallback.
Once neither loading latch is active, frame redraw skips the view scan; ongoing chunk streaming does not need to recompute a completed loading gate.

The server holds the new world's initial ticks until the client acknowledges a presented world frame. The client defers that first acknowledgement until the declared spawn view and assets are renderer-ready. Later respawns acknowledge automatically. The server's streaming radius includes padding beyond the client's initial view; waiting for that padding after the loading cover disappears would leave gameplay visible while the tick-owned action queue remains paused. Chunk streaming continues during the pause and after the acknowledgement.

Both native and browser connections release the initial world-tick hold when `PlayerLoaded` arrives. Player vitals are gated on the same acknowledgement; the browser timer still publishes chunk and world-change feeds while the player waits. Keep these two releases paired when changing a connection loop.

`Sim` carries two presentation latches next to the producer readiness state: `new_world_loading` is armed only by the newly-created survival launch path, while `dimension_transition_pending` is armed by a cross-dimension respawn. The renderer checks the transition latch before the ordinary world wait and draws its opaque cover independently. After the first playable frame, `app/redraw.rs` sends the deferred acknowledgement and clears the initial latch. The producer readiness state itself remains with the mesh ledger rather than duplicating section state in the simulation struct.

Both the full-frame connection screen and the in-world loading cover use the current menu canvas settings. Stamp the GUI scale on overlays too; otherwise the same label changes size at the handoff from connection to terrain loading.

Focus loss does not turn either loading cover into the pause overlay. The window lifecycle checks initial-world ownership, `Sim::world_wait`, and `Sim::dimension_transition_pending` before applying pause-on-lost-focus, so only a presentable `Screen::Playing` state can become `Screen::Paused`; the full-frame `Screen::Connecting` state is guarded by the same state-machine transition.

The same readiness predicate gates gameplay keys, pointer motion, clicks, and hotbar scrolling. The simulation and network continue processing while the cover is visible; the player cannot interact with an unseen world behind it.

The initial-world grid covers the playable spawn square and is centred on the canvas. The server continues streaming the selected render distance and an extra neighbour ring for meshing after the gate releases. Cells use a typed status enum and the complete twelve-colour palette; network paths may currently expose only empty/full observations, but no status is collapsed through string comparisons or a uniform colour.

Dimension changes use the existing `Sim::apply_respawn` edge detector. Before old-world removals can be presented, `reset_for_dimension_change` clears the producer latch and calls `TerrainMesh::end_session`, which discards in-flight work and the old section ledger. Destination packets build a new ledger. The cover remains active through the transition and has no timeout escape. Same-dimension death respawns do not reset terrain.

The transition boundary depends on the wire event order: a changed-dimension respawn must identify the destination before old-column forget notifications are applied. Ordinary view unloads and same-dimension respawns remain immediate and do not reset the destination ledger.

## How to change it

Change the section ledger and its column predicate in `crates/lodestone-shell/src/mesher.rs`; change grouped membership storage in `crates/lodestone-shell/src/mesher/readiness.rs`. Keep full section identity and remove empty groups when changing that storage. Change the `Sim` facade and post-drain readiness check in `crates/lodestone-shell/src/sim/meshing.rs`, and keep the renderer acknowledgement immediately after `RenderState::upload_section` in `crates/lodestone-shell/src/app/redraw.rs`. Session observations belong in `crates/lodestone-shell/src/sim/session.rs`; launch scope belongs in `crates/lodestone-shell/src/app/session.rs`; the remote phase clamp belongs in `crates/lodestone-shell/src/app/menus.rs`; transition reset behavior belongs in `crates/lodestone-shell/src/sim/dimension.rs`. Grid geometry and palette live in `crates/lodestone-shell/src/menu/render/`. If the event transport changes, preserve the transition boundary before old-column removal and keep ordinary unloads and same-dimension respawns as controls.

Do not derive readiness from the progress numerator, the scheduler's global pending count, or the set of visible sections. A changed or unclassified column decode must clear that column's old ledger before it is remeshed; only authoritative ingress comparison can preserve it for equal terrain. If the renderer hand-off boundary changes, update the deferred `PlayerLoaded` send and the negative controls together. Keep the send retryable when the outbound control relay is full.

## Configuration

There are no new flags or environment variables. `config.render_distance` chooses the background stream; `INITIAL_TERRAIN_RADIUS` caps the first playable square at six columns around the player. Readiness has no timeout; incomplete terrain or asset work keeps the cover visible.

## Dependencies

The predicate relies on `ChunkWorld` residency and build extent, `TerrainMesh` snapshot routing, `RenderState::upload_section`, and the loading state assembled by `Sim::terrain_wait` and `Sim::world_wait`. The launch scope relies on the typed `SingleplayerLaunch` and `WorldGameMode` values, not a connection heuristic. The transition boundary relies on packet event ordering, while the progress bar consumes `TerrainProgressTracker` independently as telemetry and the grid consumes typed per-column statuses.
