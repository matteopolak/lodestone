# Terrain loading readiness

## What it is

The labelled `Loading terrain...` overlay is reserved for the first creation of a survival singleplayer world. It releases when the initial spawn view (capped at radius six) is resident and renderer-settled; the rest of the render distance streams after play begins. A non-empty section must be CPU-meshed and handed to `RenderState`; an all-air section must be explicitly classified empty. Existing saves, creative/hardcore creations and multiplayer joins get no fake chunk counter or grid. Dimension travel has a separate opaque cover that hides the old dimension until the destination column is renderer-settled, without the first-world bar or grid; a remote server need not fill its advertised view square before the player can enter.

## How it works

**Settlement ledger.** `TerrainMesh` keeps a per-section ledger keyed by `SectionKey`: a ready mesh enters the renderer ledger only after `app/redraw.rs` calls `RenderState::upload_section`, and a `SnapshotOutcome::Empty` enters the empty ledger without upload; deferred or queued sections stay absent. `TerrainMesh::column_mesh_settled` checks the decoded local column and every section in the active `ChunkWorld` extent, so scheduler-wide pending counts, visible-section counts and unrelated uploaded columns cannot release the gate.

Renderer, presented and empty ledgers use `ColumnSectionSet`, which groups packed section bits by column coordinates and vertical origin: the first 64 section indices fit an inline word, taller dimensions grow numeric words, the usual single-origin column needs no inner allocation, and a column reset or unload removes only that column's membership. Distinct `min_y` values stay separate groups and empty groups are removed.

**Presented vs settled.** Streaming tracks presented coverage separately from latest-revision settlement: rebuilding a section invalidates the strict ledger but not geometry already on the GPU. A first arrival, differing or unclassified column replacement, or unload clears both; a driver-classified replacement with exactly equal column and light storage leaves both ledgers and pending work intact (the full payload is still installed). The browser's `full-view-presented` checks presented coverage across the selected distance after a frame is submitted so background replacement meshes do not hold it; the initial-world and dimension covers keep the strict predicate.

**Release.** After the frame drains removals and uploads, `Sim::refresh_terrain_readiness` evaluates every column in the declared first-world view (a dimension transition checks the player's destination column across its vertical extent). The result is a one-way producer latch per dimension; `Sim::terrain_wait` still requires the player's decoded column, `TerrainProgressTracker` stays telemetry for the bar, and a session with no declared view keeps the own-column fallback. When neither loading latch is active, redraw skips the view scan.

The server holds the new world's initial ticks until the client acknowledges a presented world frame, which the client defers until the spawn view and assets are renderer-ready; later respawns acknowledge automatically. Server streaming includes padding beyond the initial view (waiting for it after the cover disappears would show gameplay with the tick-owned action queue paused); streaming continues during the pause. Native and browser connections both release the hold on `PlayerLoaded`, and vitals are gated on the same acknowledgement (the browser timer still publishes chunk and world-change feeds meanwhile); keep the two releases paired in any connection-loop change.

`Sim` carries two presentation latches beside the readiness state: `new_world_loading` (armed only by the newly created survival launch) and `dimension_transition_pending` (armed by a cross-dimension respawn). The renderer checks the transition latch before the ordinary wait and draws its opaque cover independently; after the first playable frame `app/redraw.rs` sends the deferred acknowledgement and clears the initial latch. The connection screen and the in-world cover both use the current menu canvas settings, so stamp the GUI scale on overlays too (or the label changes size at the handoff). Focus loss does not turn either cover into the pause overlay: the window lifecycle checks initial-world ownership, `Sim::world_wait` and `Sim::dimension_transition_pending` first, so only a presentable `Screen::Playing` can become `Screen::Paused` (and `Screen::Connecting` is guarded the same way). The same predicate gates gameplay keys, pointer motion, clicks and hotbar scrolling while the simulation and network keep running.

The initial-world grid covers the playable spawn square, centred on the canvas, with a typed status enum and the full twelve-colour palette (network paths may expose only empty/full today, but no status is collapsed through string comparisons).

**Dimension changes** use `Sim::apply_respawn`'s edge detector: before old-world removals can be presented, `reset_for_dimension_change` clears the producer latch and calls `TerrainMesh::end_session` (discarding in-flight work and the old ledger); destination packets build a new one. The cover has no timeout escape; same-dimension death respawns reset nothing. This depends on wire event order: a changed-dimension respawn must identify the destination before old-column forget notifications apply, while ordinary unloads and same-dimension respawns stay immediate.

## How to change it

- Section ledger and column predicate: `crates/lodestone-shell/src/mesher.rs`; grouped membership storage: `mesher/readiness.rs` (keep full section identity and remove empty groups). `Sim` facade and post-drain check: `sim/meshing.rs`; keep the renderer acknowledgement right after `RenderState::upload_section` in `app/redraw.rs`. Session observations: `sim/session.rs`; launch scope: `app/session.rs`; remote phase clamp: `app/menus.rs`; transition reset: `sim/dimension.rs`; grid geometry and palette: `menu/render/`.
- Never derive readiness from the progress numerator, the scheduler's global pending count or visible sections. A changed or unclassified column decode must clear that column's old ledger before remeshing; only authoritative ingress comparison may preserve it for equal terrain. If the renderer hand-off boundary changes, update the deferred `PlayerLoaded` send and the negative controls together, keeping the send retryable when the outbound control relay is full. If event transport changes, preserve the transition boundary before old-column removal and keep unloads and same-dimension respawns as controls.

## Configuration

No flags. `config.render_distance` chooses the background stream; `INITIAL_TERRAIN_RADIUS` caps the first playable square at six columns. Readiness has no timeout.

## Dependencies

`ChunkWorld` residency and build extent, `TerrainMesh` snapshot routing, `RenderState::upload_section`, `Sim::terrain_wait`/`world_wait`, the typed `SingleplayerLaunch` and `WorldGameMode` for launch scope, packet event ordering for transitions, `TerrainProgressTracker` for the bar, and typed per-column statuses for the grid.
