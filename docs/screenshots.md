# Screenshots

## What it is

The harness that produces the README's in-game images under `docs/images/`. Every PNG is this client rendering a real, live session against the flat creative 26.3 oracle with the vanilla assets in `LODESTONE_ASSETS` (e.g. `.cache/benchmarks/vanilla-26.3-assets`): no mock-ups, compositing or editing. `just screenshots` regenerates the set so images track the renderer. It is separate from the in-game `key.screenshot` keybind in `crates/lodestone-shell/src/screenshot.rs`; they share only the PNG encoder (the keybind reads the swapchain, this reads a headless target).

## How it works

`crates/lodestone-shell/tests/gpu/capture_screenshots.rs` is a live gate that ends at a file: it joins the oracle through `Sim` (the type `WindowApp` drives), installs every render source production installs (`app/session.rs`, `app/redraw.rs`), then per scene runs the scene's RCON commands, drains the network until the world stops arriving, advances the sim clock a fixed number of ticks, renders one frame (plus the HUD if asked), reads texels back and writes a PNG.

**Scenes are data.** One `scripts/screenshot-scenes/<name>.txt` per image (stem names the PNG; sorted order). `@` lines are directives (`@size`, `@camera`, `@look`/`@yawpitch`, `@fov`, `@wait` a wall-clock floor on the network drain, `@ticks` sim ticks to advance afterwards, `@hud`, `@hand`, `@debug`); anything else is a verbatim RCON command. `LODESTONE_SCENES=02-signs,05-hud` restricts a run.

**Determinism.** A capture with no code change reproduces the committed PNG's exact bytes (verify by capturing twice into separate `LODESTONE_CAPTURE_OUT` directories and comparing hashes). That needed two settle mechanisms: `@wait` pumps the simulation with `dt = 0` (RCON edits travel and mesh without advancing a game tick) until 40 consecutive frames upload no section, remove none and see no change in loaded-column count; `@ticks` then runs with no sleeping against a fixed absolute tick count carried across scenes, so animation phases (sea lantern sprite, beacon beam, banner sway) are captured at a deterministic moment. The join cannot be made tick-free, so its variable cost is absorbed by winding the clock to a fixed base tick before the first scene. Four more sources of run-to-run difference were removed:

- Particle placement: the engine seeds from the clock, so each scene calls `Sim::seed_particles` with an FNV-1a hash of its stem.
- Repeating command blocks left by benchmark scenes: the harness sets `gamerule command_blocks_work false`.
- Companion clients: an undrained update channel stops answering keep-alives and gets dropped at thirty seconds (removing its tab-list row and posting "left the game"), so a background thread drains them; they join one at a time in fixed order and any same-named leftover is kicked first.
- Tab-list ping bars are the wall-clock keep-alive round trip, so every row is pinned to the full-strength icon.

**The control.** A silent failure (black frame, camera inside a block) must never reach `docs/images/`, and draw counters cannot rule it out. Every frame is checked on the count of distinct colours (quantised to 5 bits per channel) and the fraction of pixels off the modal colour (so a mostly-one-thing frame like a sky gradient still clears); both thresholds sit well clear of real scenes and inside a degenerate one.

## How to change it

Add a file under `scripts/screenshot-scenes/` and run `just screenshots`; the harness needs no change. The README table is edited by hand. Gotchas:

- Build the stage high, not on the oracle's superflat surface, so scenes read against a stone plate with no horizon.
- 26.2 renamed game rules to snake_case (`advance_time`, `mob_drops`); camelCase silently fails to parse (check `help gamerule`).
- A render source has no uninstall and one `RenderState` serves the run: install both arms of any per-scene switch (e.g. a first-person-hand suppressor).
- The harness is a silent second implementation of production's render-source wiring: when `install_session_render_sources` grows a source, mirror it here in the same commit (entity ground shadows once shipped missing here unnoticed).
- `HudRenderer::new` takes the raw (non-sRGB) view format and every `attach_*` the corrected one; one format for all is a validation error at the first `set_pipeline`. The output target is `Rgba8UnormSrgb` (stored bytes are what a player sees), unlike the pixel gates' `Rgba8Unorm`.
- Do not post-process committed PNGs: any lossless re-encode shows a spurious diff on every run.
- A block-entity NBT field you omit is zero, not default (an incompletely specified campfire cooked and ejected real item entities onto the stage).
- A scene's `fill ... air` cannot clean up water (a source outside the box flows back in); the harness purges water once per run over a box wider than any stage. Every scene shares one world, so each clears and rebuilds its own plot.
- A subject that looks broken may be photographed wrong (waterlogged-by-default block flooding the stage, a skull or chest seen from its back, a connected pair on the wrong axis). Check the block's real state before blaming the draw code, and re-read a scene's header comment when its build changes.

## Configuration

| | |
|---|---|
| `LODESTONE_SCENES` | comma-separated stems; unset captures all |
| `LODESTONE_CAPTURE_OUT` | output directory instead of `docs/images` (compare runs without touching committed files) |
| oracle | `127.0.0.1:25570` game, `:25571` RCON, password `lodestone` (`scripts/live-oracles/creative.sh`) |
| output | `docs/images/<scene stem>.png` |

The harness pins world spawn, force-loads a box around it, stops day/night and weather cycles, fixes the time and suppresses command feedback. It clears the process's in-memory selected-pack order before constructing `Sim` so committed images always use the built-in resources, never a developer's local pack, and never writes that selection back.

## Dependencies

The flat creative oracle (`just oracle-creative`); a `wgpu` adapter (the harness fails rather than skips); the vanilla assets under `.cache/mc/<version>` or `LODESTONE_ASSETS` (otherwise `Sim` falls back to the procedural demo palette); `--features live`; `lodestone_testsupport::RconClient` and `lodestone::screenshot::encode_png`.
