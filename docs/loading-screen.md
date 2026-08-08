# Loading screen

## What it is

The screen shown between a menu click and a playable world: named connect
phases while the session is established, then `Loading terrain...` with a real
progress bar while the initial view streams in. Issue #449 — its value is
diagnostic before it is cosmetic, because before it there was no way to tell
"still loading" from "broken".

## How it works

Two screens, and they are different mechanisms on purpose.

**Before login — a full frame.** `UiState::begin` moves to `Screen::Connecting`,
which `owns_frame` lists, so `menu/render/dispatch.rs` takes the whole frame.
No chunk packets arrive before login, so nothing needs to keep meshing behind
it. The label is `ui.connect_phase().label()`.

**After login — an overlay.** `app/redraw.rs` draws `loading_frame` /
`loading_frame_with_progress` over the still-rendering world while
`Sim::terrain_loading()` is true. It must stay an overlay: chunks have to keep
meshing and uploading behind the text, which a full-frame screen would stop.
The predicate is vanilla's own `DownloadingTerrainScreen` rule — the column
under the player's feet is not in the client world yet — so the screen clears
when the ground the player is standing on arrives, never because a bar filled.

### The phases, and why there are only three

`crate::menu::loading::ConnectPhase`. Every label is a real key transcribed from
`.cache/mc/26.2/client-src/assets/minecraft/lang/en_us.json`:

| phase | key | string | emitted from |
|---|---|---|---|
| `Connecting` | `connect.connecting` | Connecting to the server... | `NetUpdate::Connecting`, at the top of `net::run_session` |
| `Joining` | `connect.joining` | Joining world... | `NetUpdate::ConnectPhase`, right after `connect`/`connect_with` returns a handle |
| `LoadingTerrain` | `multiplayer.downloadingTerrain` | Loading terrain... | `NetUpdate::LoggedIn` |

Route: `net.rs` → `sim/net_apply.rs` → `Sim::connect_phase` →
`WindowApp::drive_ui_from_session` → `UiState::set_connect_phase` →
`frame_for`.

**`connect.authorizing`, `connect.encrypting` and `connect.negotiating` are
deliberately missing.** Vanilla sets them from inside
`ClientHandshakePacketListenerImpl`, i.e. from the handshake state machine
itself. Ours is behind one `ClientBuilder::connect().await`, so the shell cannot
observe those boundaries — showing them would mean changing the label on a
timer, which is the fake-progress failure this feature exists to avoid.

**Singleplayer shares the connect phases**, because vanilla's integrated server
is likewise reached over a real connection. 26.2 has no `menu.loadingLevel` or
`menu.generatingTerrain` string to use for the world-open step, so it lives
inside `Connecting`.

### The progress bar

`crate::menu::loading::TerrainProgress` carries the raw numerator and
denominator, never a percentage:

- **numerator** — `NetClient::loaded_chunks().len()`, the client's own applied
  columns.
- **denominator** — `(2 * view_radius + 1)^2`, the view square the server
  streams (`join_view_rings` partitions exactly that square).

`MenuFrame::progress` is the frame primitive; `menu/render/draw.rs` draws
vanilla's `LevelLoadingScreen` geometry — 200×2, centred, black track
(`0xFF000000`), green fill (`0xFF00FF00`) at `round(fraction * 200)`.

## How to change it, and the gotchas

- **A phase with no emit site in `net.rs` is an island.** It will compile, test
  green, and name a step the game never reaches. Add the `NetUpdate` send first.
- **`ConnectPhase` is not `SessionPhase`.** `SessionPhase` (in
  `lodestone-ecs`) drives the menu state machine and every `match` on it; this
  is display-only, and has boundaries that one collapses. Do not merge them.
- **`TerrainProgress::fraction` is clamped to `MAX_FRACTION = 0.99`, and that is
  load-bearing.** The screen is dismissed by `terrain_loading()`, so a bar that
  could read as full while the screen was still up would convert an honest
  freeze into a false reassurance. Leave the sliver.
- **Only singleplayer declares a view radius** (`Sim::set_view_radius`, from
  `begin_singleplayer`), so only singleplayer gets a bar. A multiplayer server
  clamps our requested view distance to its own, so the same number would be an
  upper bound there and the bar would stall at, say, 70% and read as a hang.
  `terrain_progress()` returns `None` and the screen falls back to the bare
  phase label. **Wiring multiplayer's bar needs the server's actual view
  distance**, not our request.
- **The per-chunk `ChunkStatus` grid is still absent**, and that is the issue's
  headline bullet. Vanilla's `LevelLoadingScreen` colours a 2×2-px cell per
  chunk from twelve `ChunkStatus` colours; nothing here exposes a per-column
  status (only loaded/not-loaded), so the grid is blocked on issue #289's
  ticket/loading pipeline. It must not be faked from the loaded set — that
  would be a twelve-colour grid with two colours in it.
- **`LEVEL_CHUNKS_LOAD_START` (game event 13) is still not decoded.**
  `crates/protocol/v770/src/adapter.rs`'s game-event match drops it in its
  terminal arm, so there is no "the server is about to stream" edge and
  `terrain_loading()` has to infer the state from a chunk lookup. Wiring it
  would let the terrain phase begin at the packet rather than at login.

## Configuration

`render_distance` (`options.json`) sets `view_radius = render_distance + 1`,
which is the bar's denominator. Nothing else.

## Dependencies

`crate::net` (the `NetUpdate` stream), `crate::sim` (the loaded-column count and
`terrain_loading`), `crate::menu::render` (the frame builders and the bar
primitive). No new external dependency.
