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
The predicate is `menu::loading::is_level_ready`, vanilla's
`LevelLoadTracker.WaitingForPlayerChunk.isReady` — so the screen clears when the
ground the player is standing on arrives, never because a bar filled.

### The backdrop: the panorama under the wash, not a flat fill

Both frames take `MenuBackdrop::Panorama` (the default), and that replaced a
`overlay: true` in which **one flag did two jobs**: it selected the translucent
backdrop colour in `menu::render::draw::build`, *and* it was the only thing
suppressing the panorama in `MenuRenderer::draw`. Asking for a wash therefore
turned the sky off, and the screen rendered as a flat clear with a translucent
quad on it. Reported as looking wrong; it was.

No vanilla path produces a flat fill. Read off the 26.2 decompile rather than
inferred:

- `ConnectScreen` overrides no background at all, so it takes the base
  `Screen.extractBackground`: panorama (its `minecraft.level == null` gate holds
  while connecting) → blur → `menu_background.png`.
- `LevelLoadingScreen.extractBackground`'s `OTHER` arm — the ordinary loading
  reason — calls `extractPanorama` with **no** `level == null` gate, so the
  panorama covers even a live level. Its other two arms are the nether-portal
  sprite and the end-portal shader, which this client does not have; they are
  their own piece of work, not this frame's.

Three consequences worth knowing before changing this:

- **The wash is not the backdrop quad.** Under `Panorama` the full-screen colour
  quad is skipped entirely (`MenuGeometry::backdrop_floats`) and the 25 %-black
  wash arrives as the panorama shader's own `dim` uniform, from
  `panorama::dim_for_screen` keyed on `MenuFrame::logo`. Reinstating a quad would
  double the darkening. See `menu-panorama.md`.
- **The post-login `render_overlay` call's `Load` op is now merely harmless.** The
  panorama covers every pixel of the world it draws over — exactly as vanilla's
  does. The world still meshes and uploads behind it, which is the property the
  paragraph above cares about; only its *visibility* changed.
- **`MenuBackdrop::Opaque` is the fallback, not a screen's choice.** With no
  panorama textures loaded (a jar-less or headless run) `Panorama` degrades to the
  same opaque quad, which is why `Panorama::is_translucent()` is `false`.

A unit test can only see the *declaration* and the quad's colour
(`the_loading_screen_asks_for_the_panorama_and_the_in_world_screens_do_not`):
`build` is pure and emits the quad unconditionally, so whether the cubemap reaches
a pixel is decided in `MenuRenderer::draw` and needs a GPU. That half is
`menu_panorama_pixels::the_loading_screen_draws_the_panorama_under_the_menu_background_wash`,
which predicts the washed byte three ways — 224 for the correct linear-space
multiply, 255 for no wash, 191 for a gamma-space one — and shoots the **title**
frame through the same band as a cross arm, because one washed frame and one
unwashed frame measured together is what distinguishes "the wash reaches the right
screens" from "everything went dark".

### The dismissal condition, in full

Four observations, gathered by `Sim::terrain_loading` and decided by
`is_level_ready`. Note `ReceivingLevelScreen` is **gone** in 26.2; the screen
carrying `multiplayer.downloadingTerrain` is `LevelLoadingScreen`, and
`Minecraft.doWorldLoad` builds one unconditionally for singleplayer next to
`ConnectScreen`/`ClientPacketListener` for multiplayer — so it really does appear
on every join, not only on world creation.

| observation | effect | vanilla |
|---|---|---|
| player's own column loaded | dismisses | `playerSectionReady` (we use the column, a strictly earlier condition) |
| 30 s elapsed | dismisses **anyway** | `CLIENT_WAIT_TIMEOUT_MS` |
| player dead | dismisses | `player.isAlive()` |
| player outside build height, or no dimensions yet | dismisses | `level.isOutsideBuildHeight` |

**The last three are bail-outs, not requirements** — vanilla's ternary reads
"only wait if waiting could work", and transcribing them as `&&`ed preconditions
for dismissal inverts it into a screen that hangs in exactly the cases they were
written for. The dead case has teeth here: a server holding a dead player on the
death screen sends no chunks at all, so a column-only wait would never finish.

**It is not the view square.** `TerrainProgress`'s `(2r+1)^2` is the *bar's*
denominator and nothing else; waiting on it would hold the screen for the whole
initial stream.

**Why the timeout is load-bearing rather than defensive.** Without it the
dismissal is a liveness assumption about the server, and that assumption has
already failed once: `lodestone_server::server`'s join loop used
`join_view_rings`' ring *offsets* as absolute chunk coordinates, so the streamed
square was centred on chunk `(0, 0)` rather than on the joining player. For a
player restored away from the origin the awaited column was never sent — and the
server's `ViewTracker` had recorded it as sent, so it never would be. The screen
had no way out. That defect is fixed; the timeout is what makes the next one
present as a 30 s delay instead of a game that never starts.

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
