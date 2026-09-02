# Screenshots

## What it is

The harness that produces the README's in-game images under `docs/images/`. Every
PNG is this client rendering a real, live session against the flat creative 26.2
oracle — no mock-ups, no compositing, no editing. `just screenshots` regenerates
the whole set, so the images can be refreshed whenever the renderer changes instead
of drifting into a record of how the client looked one afternoon.

This is separate from `crates/lodestone-shell/src/screenshot.rs`, the in-game
`key.screenshot` keybind. The two share only the PNG encoder: the keybind reads the
window's swapchain, this harness reads a headless render target.

## How it works

`crates/lodestone-shell/tests/capture_screenshots.rs` is a live gate that ends at a
file rather than an assertion: it joins the oracle through `Sim` (the same type
`WindowApp` drives), installs every render source `app/session.rs`/`app/redraw.rs`
install in production, then per scene runs the scene's RCON commands, drains the
network until the world stops arriving, advances the sim clock a fixed number of
ticks, renders one frame (plus the HUD if the scene asks for it), reads the texels
back and writes a PNG.

**Scenes are data, not code.** One `scripts/screenshot-scenes/<name>.txt` per image
— the stem names the PNG, and files run in sorted order. A `@`-prefixed line is a
directive (`@size`, `@camera`, `@look`/`@yawpitch`, `@fov`, `@wait` — a wall-clock
floor on the network drain — `@ticks` — sim ticks to advance after that, before the
shot — `@hud`, `@hand`, `@debug`); everything else is a verbatim RCON command.
`LODESTONE_SCENES=02-signs,05-hud` restricts a run to those stems for fast
iteration.

### Settling the world before the shot

A capture with no code change reproduces the committed PNG's exact bytes. Getting
there needed two independent settle mechanisms, not one: **`@wait`** pumps the
simulation with `dt = 0` (so RCON edits travel over the socket and get meshed
without advancing any game tick) until 40 consecutive frames upload no section,
remove none, and see no change in loaded-column count — a floor on wall-clock time,
not the whole wait, so a slow machine costs seconds rather than a wrong capture.
**`@ticks`** then runs afterward with no sleeping at all, against a fixed absolute
tick count carried across scenes, so every animation phase (a sea lantern's
sprite, a beacon beam, a banner's sway) is captured at a deterministic moment rather
than "whatever the machine managed in a wall-clock window". The join itself is the
one phase that cannot be made tick-free (a client that never ticks never sends a
position), so its variable cost is absorbed by winding the clock up to a fixed base
tick before the first scene runs.

### The control

A capture's worst failure is a silent one — a black frame, a camera stuck inside a
block, committed straight to `docs/images/`. Draw counters cannot rule this out (a
harness here has measured itself submitting real geometry and reading back nothing),
so every frame is checked on two pixel statistics before being written: the count of
distinct colours (quantised to 5 bits/channel — a frame that never lit is one flat
colour) and the fraction of pixels off the modal colour (so a frame that is *mostly*
one thing, like a legible sky gradient, still clears the floor). Both thresholds sit
well clear of every measured real scene and well inside a degenerate one.

## How to change it

Add a file under `scripts/screenshot-scenes/`, run `just screenshots`; nothing in
the harness needs to know about a new scene. Edit `README.md`'s own table to change
what it shows — the harness only writes files.

Gotchas, each of which cost a real run:

- **Build the stage high, not on the oracle's own superflat surface**, so the
  scene reads well against a stone plate with no horizon behind it.
- **26.2 renamed every game rule to snake_case** (`advance_time`, `mob_drops`, …) —
  the camelCase spellings silently fail to parse; check with `help gamerule` rather
  than a wiki page.
- **A render source has no uninstall, and one `RenderState` serves the whole run**
  — install both arms of any per-scene switch (e.g. a first-person-hand
  suppressor), never just one, or a later scene silently inherits the wrong state.
- **The harness is a second, silent implementation of production's render-source
  wiring** — when `install_session_render_sources` grows a new source, mirror it
  here in the same commit, or every capture quietly loses that source (entity
  ground shadows shipped with no source installed here for a while, and nothing
  anywhere objected).
- **`HudRenderer::new` takes the raw (non-sRGB) view format and every `attach_*`
  takes the corrected one** — building the whole renderer against one format is a
  validation error at the first `set_pipeline`.
- **The output target is `Rgba8UnormSrgb`**, unlike the pixel gates' plain
  `Rgba8Unorm` — that is the format whose stored bytes are what a player actually
  sees and therefore belongs in a PNG.
- **Do not post-process the committed PNGs.** Any lossless re-encode makes every
  subsequent run show a spurious diff against the harness's own output; keep the
  committed bytes exactly what the harness wrote.
- **A block-entity NBT field you leave out is a zero, not a default** — an
  incompletely-specified campfire cooked and ejected real item entities onto the
  stage, which is real run-to-run pixel noise the settle logic cannot see (it isn't
  a streaming race).
- **Water cannot be cleaned up by a scene's own `fill … air`** — a source block
  left outside the cleared box flows back in and floods the next scene; the harness
  purges water once per run over a box wider than any stage.
- **Every scene shares one world** — each file must clear and rebuild its own plot
  rather than assuming it starts empty.
- **A subject that looks broken may just be photographed wrong.** Several
  historical "renderer bug" reports here turned out to be the scene's own block
  state: a waterlogged-by-default block flooding the stage, a skull or chest
  photographed from its unfinished/back side, a connected-block pair placed on the
  wrong facing axis. Removing the subject and watching the artefact go proves the
  subject is *involved*, not that its renderer is at fault — check the block's
  actual state before believing the draw code dropped something.
- **A scene's own header comment can stop matching what it builds** without the
  image changing at all — re-read a scene's rationale when its build changes, the
  same way you'd re-check a stale doc comment.

## Configuration

| | |
|---|---|
| `LODESTONE_SCENES` | comma-separated scene stems to capture; unset captures all |
| oracle | `127.0.0.1:25570` game, `:25571` RCON, password `lodestone` (`scripts/live-oracles/creative.sh`) |
| output | `docs/images/<scene stem>.png` |

The harness pins world spawn, force-loads a box around it, stops the day/night and
weather cycles, fixes the time, and suppresses command feedback so a re-run is
reproducible rather than "whatever the world was doing". It also clears the capture
process's in-memory selected-pack order before constructing `Sim`, so committed
images always use vanilla's built-in 26.2 resources rather than a developer's local
resource pack selection — and never writes that selection back to disk.

## Dependencies

- The flat creative 26.2 oracle (`just oracle-creative`).
- A `wgpu` adapter — the harness fails rather than skips without one.
- The vanilla assets under `.cache/mc/26.2` (or `LODESTONE_ASSETS`) — without them
  `Sim` falls back to the demo path and would capture the procedural palette
  instead of the real game.
- `--features live` (compiles `v770` into the registry).
- `lodestone_testsupport::RconClient`, `lodestone::screenshot::encode_png`.
