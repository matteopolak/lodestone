# F3 debug overlay

## What it is

The F3 instrument: two columns of engine and world stats drawn over the world in vanilla's plate, pitch and font, plus two world-space overlays (F3+B entity hitboxes, F3+G chunk borders). Presentation (plate geometry, text metrics, column layout) is a faithful port; content is curated: lines describing the JVM (heap, Java/CPU info, GPU-utilisation percentage) are dropped rather than faked, and this engine's own diagnostics fill the gaps.

## How it works

### Presentation

Every geometric constant (line pitch, insets, plate size relative to text, ink colour, text scale) is transcribed from the reference overlay. The plate is one pixel taller and wider than its text per side so consecutive lines tile without a seam. The overlay draws at scale 1.0 in the GUI-scale-divided logical canvas and must never pick up a HUD-wide text-scale multiplier ([HUD](./hud.md)). An empty line is a group separator: skipped when drawing but still advancing the line index. Both columns' plates are drawn before either column's glyphs, so a plate never covers another line's text.

### Ported, replaced, dropped

The default profile enables nine entries, laid out alphabetically by registry path. Each surviving line is:

- **Ported verbatim** where there is a real datum: player position (world, block, chunk, section-relative, facing), targeted block, per-section client light levels, with format strings and precision copied exactly.
- **Replaced** where the shape survives but the value must differ: frame time for a framerate target, engine version for Minecraft's, session status for server tick/tx/rx counters, process RSS for JVM heap percentages, adapter/driver info for Java/CPU/display info.
- **Dropped** with no datum: JVM memory breakdowns, biome/day counters, sound/mood diagnostics and every "visualize" overlay besides hitboxes and chunk borders. Never a placeholder.

Engine-only lines: a fixed-timestep health ratio, a live-chunk/dropped-mesh counter (a loaded chunk that silently fails to mesh used to vanish without signal), an occlusion-culling summary, resident chunk memory, the latest F3 probe round trip, GPU mesh residency, plus conditional lines (recipes, world border, maps, spawn point) that appear once the server sends their data. The current dimension (read off the local player's dimension component so a portal trip updates immediately) and the hitbox/chunk-border toggle states (read from the flags that gate drawing) are read fresh every frame.

### Column placement

The reference splits its lines by category, not in half: a "priority" bucket fills whichever column is shorter, a "regular" bucket splits down the middle, and named groups place as whole units, each category ending with a separator. This port reproduces the left/right assignment derived by running that algorithm on the reference's own default profile, not by running it on this engine's smaller entry set (which would assign different columns). A new line therefore picks a category and keeps its group's separator.

### Reading the engine-only lines

- The occlusion-culling line's `active`/`off` flag is the load-bearing part: every failure of the culling graph draws more, never less, so a cull count of zero is ambiguous between "nothing to cull" (looking straight down) and "the graph stopped walking".
- Section/quad counts are per-frame **drawn** counts; the mesh-VRAM figure is **residency**. GPU mesh memory changes only when a chunk arrives or unloads, never with camera movement, so a residency figure that moves when the player turns in place is wrong (it was once computed from the per-frame drawn-quad count). It now reads real GPU buffer sizes and the arena's occupancy. A flat residency under a sawtoothing drawn count is healthy; a climbing residency under a flat drawn count means real fragmentation.

### Chords and world overlays

F3 arms a modifier state and the overlay toggles on release only if no chord (F3+B, F3+G) fired during the hold (toggling on press would let one F3+B open the overlay and flip hitboxes). Both world overlays ride the world-space debug-line renderer: hitboxes draw a wireframe box per rendered entity (sized from the same entity-dimension data as the nametag anchor, so they never disagree on height) plus a short look ray; chunk borders draw the current chunk plus a ring at every section boundary using the active dimension's actual height range.

## How to change it

- Route a new line through the shared column-fitting layout; never measure and position directly (an ad-hoc draw path let long lines escape the canvas at high GUI scale).
- Choose a category (priority, regular, named group), not a column. Match the typography: `Key: value`, sentence-case keys, lowercase enum values, `, ` between fields.
- Floor a coordinate to get its block/chunk value, never truncate toward zero (`as i64`): they differ for negative fractional coordinates and the bug is invisible at the origin, so test off-origin with a negative coordinate.
- Anything reading world-resident state or making a syscall belongs behind the existing throttle that gates the O(resident-world) stats.
- F3+B and F3+G are hardcoded, not rebindable (a known gap; binding needs new actions with labels and glyphs).
- The chunk-border height range is captured at install from the active dimension; reinstall the debug-line source after a dimension change if the range should follow.

## Configuration

`gui_scale` (`options.json`) scales the overlay through the shared logical-canvas divisor. The reference's per-entry enable/disable profile is not implemented, so the entry set is fixed.

## Dependencies

`crates/lodestone-shell/src/hud.rs` and `hud/vanilla_font.rs` (plate/text draw and font, [HUD](./hud.md)); `crate::gpu::debug_lines`; `crate::entities` (live draw list); `lodestone_data::entity_dimensions`; `crate::net` (dimension height range, per-section light); the current-version `client-src` under `.cache/mc` as behavioural reference only.
