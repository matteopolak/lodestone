# F3 debug overlay

## What it is

The F3 instrument: two columns of engine and world stats drawn over the world in vanilla's own plate,
pitch and font, plus two world-space overlays (F3+B entity hitboxes, F3+G chunk borders). The
presentation — plate geometry, text metrics, column layout — is a faithful port of vanilla's
`DebugScreenOverlay`; the *content* is curated rather than faked: lines that describe the JVM (heap
stats, Java/CPU info, GPU-utilization percentage) are dropped outright instead of being filled with
fabricated numbers, and this engine's own diagnostics take the slots vanilla's JVM-only lines leave
empty.

## How it works

### Presentation

Every geometric constant — line pitch, left/right/top insets, the background plate's size relative to
its text, ink color, text scale — is transcribed from vanilla's real overlay rather than chosen. Two
details matter for a faithful port: the background plate is one pixel taller and wider than its text on
each side so consecutive lines tile with no seam, and the overlay draws at scale 1.0 in the same
GUI-scale-divided logical canvas the rest of the HUD uses — it must never pick up a HUD-wide text-scale
multiplier (see [`hud.md`](./hud.md)), which was a real, since-fixed bug here. An empty line is a group
separator: it's skipped when drawing but still advances the line index, which is what keeps later
groups from creeping up to fill the gap. Both columns' plates are drawn before either column's glyphs,
so a line's plate can never cover another line's text — slightly stronger than vanilla, which only
guarantees that within one column.

### What's ported, replaced, and dropped

Vanilla's default debug profile enables nine entries, laid out alphabetically by their registry path
(not registration order). Each surviving line falls into one of three buckets:

- **Ported verbatim** — anything with a real datum on this side: player position (world, block, chunk,
  section-relative, facing), targeted-block position, per-section client light levels. Format strings,
  precision and separators are copied character for character.
- **Replaced with an equivalent** — a vanilla line whose *shape* survives but whose *value* is
  necessarily different because this isn't a JVM: frame time in place of a framerate-limit target,
  engine version in place of Minecraft's, session status in place of server tick/tx/rx counters, real
  process RSS in place of JVM heap percentages, and adapter/driver info in place of Java/CPU/display
  info.
- **Dropped entirely** — anything with no datum to fill it: JVM-specific memory breakdowns, biome/day
  counters, sound/mood diagnostics, and every "visualize" world overlay besides hitboxes and chunk
  borders. A dropped line is absent, never faked with a placeholder value.

A handful of lines have no vanilla counterpart at all and exist because this engine needs them: a
fixed-timestep health ratio, a live-chunk/dropped-mesh counter (a chunk the server reports loaded that
silently fails to mesh used to vanish with no signal at all), an occlusion-culling summary, resident
chunk memory, the latest F3 probe round-trip time, and GPU mesh residency (see below) — plus a
handful of conditional lines (recipes, world border, maps, spawn point) that only appear once their
data has actually arrived from the server.

Two values are read fresh every frame rather than cached, deliberately: the current dimension (read off
the local player's own dimension component, so a portal trip updates it immediately rather than only at
login) and the hitbox/chunk-border toggle states (read from the same flags that gate whether those
overlays actually draw, so the on-screen hint can never disagree with reality).

### Column placement follows vanilla's semantics, not a mechanical halve

Vanilla does not split its full line list in half — it splits within three categories (a "priority"
bucket that fills whichever column is currently shorter, a "regular" bucket split down the middle, and
named groups placed as whole units) and each category ends with a separator. This port reproduces that
category-based placement, derived by running vanilla's own algorithm against vanilla's own default
profile and reusing the resulting left/right assignment — not by running the same algorithm against this
engine's different (smaller, non-JVM) entry set, which would put entries in different columns than
vanilla's screen does. Adding a new line means picking which of the three categories it belongs to and
keeping its group's separator, not choosing a column directly.

### Reading the engine-only lines

Two of the engine-only lines are easy to misread:

- **The occlusion-culling line's `active`/`off` flag is the load-bearing part, not the cull count.**
  Every failure mode of the culling graph draws *more*, never less, so a cull count of zero is
  ambiguous between "correctly nothing to cull" (e.g. looking straight down) and "the graph silently
  stopped walking." Only the flag distinguishes them.
- **The section/quad counts are per-frame *drawn* counts; the mesh-VRAM figure is *residency*, and
  conflating them is a real, previously-shipped bug.** GPU mesh memory is allocated and freed only when
  a chunk actually arrives or unloads — never by camera movement — so a residency figure that moves when
  the player merely turns on the spot is wrong by construction; it was previously computed from a
  per-frame drawn-quad count and swung with every camera rotation. The residency figure is read directly
  off real GPU buffer sizes and the mesh arena's own occupancy instead. Read the two together: the
  drawn count naturally sawtooths with camera movement, while a flat residency figure under that
  sawtooth is healthy; a *climbing* residency figure under a flat drawn count would indicate real
  fragmentation and is the only shape that would justify tuning anything here.

### The chords, and the world overlays

F3 itself no longer directly toggles the overlay on press — it arms a modifier state, and the overlay
toggles on release only if no chord (F3+B, F3+G) fired during the hold. Toggling on press instead would
make a single F3+B keystroke both open the overlay and flip hitboxes in one motion, which is not
vanilla's behavior.

The two world overlays ride the engine's existing world-space debug-line renderer rather than a
dedicated pass: hitboxes draw one wireframe box per rendered entity (sized from the same entity-dimension
data the nametag-anchor code uses, so a hitbox and a nametag can never disagree about an entity's height)
plus a short look-direction ray; chunk borders draw the player's current chunk plus a ring at every
section boundary, using the actual dimension's height range rather than a hardcoded one (so a custom or
non-overworld height range still draws the right box).

## How to change it

- **Route a new line through the shared column-fitting layout — never measure and position a line
  directly.** A second, ad-hoc draw path that bypasses the fitting logic is exactly the bug class this
  overlay used to have, where a long line could escape the canvas at high GUI scale.
- **Adding a line means choosing one of vanilla's three categories (priority, regular, named group), not
  a column.** The category is what decides placement; picking a column directly abandons the semantics
  documented above.
- **Match the existing typography convention**: `Key: value`, sentence case for keys, lowercase for enum
  values, a space after the colon, `, ` between fields on one line. A line in a different style becomes
  the visibly odd one out.
- **Floor a coordinate to get its block/chunk value — never truncate-toward-zero (`as i64`).** The two
  disagree on any negative fractional coordinate, and the bug is invisible right at the origin, so a
  regression here needs a fixture placed off-origin with a negative coordinate to catch it.
- **Anything that reads world-resident state or makes a syscall belongs behind the existing throttle**
  that already gates the handful of O(resident-world) stats to a periodic refresh, not the
  every-frame-cheap fields.
- **F3+B and F3+G are hardcoded, not rebindable** — making them real key bindings is a deliberate,
  known gap rather than an oversight, and would need new bindable actions with their own labels and
  glyphs.
- **The chunk-border height range is captured at install time from the active dimension, not assumed to
  be the overworld's.** A dimension change (e.g. a portal trip) needs the debug-line source reinstalled
  if the visible range should follow it.

## Configuration

`gui_scale` (`options.json`) scales the whole overlay through the shared logical-canvas divisor, exactly
like the rest of the HUD. There is nothing else: vanilla's per-entry enable/disable profile system is
not implemented, so the entry set here is fixed rather than user-configurable.

## Dependencies

- `crates/lodestone-shell/src/hud.rs` and `hud/vanilla_font.rs` — the shared plate/text draw and font
  stack (see [`hud.md`](./hud.md)).
- `crate::gpu::debug_lines` — the world-space line renderer both toggleable overlays ride.
- `crate::entities` — the live entity draw list hitboxes are built from.
- `lodestone_data::entity_dimensions` — hitbox sizing, shared with the nametag-anchor code.
- `crate::net` — dimension height range and per-section light lookups.
- The 26.2 jar under `.cache/mc/26.2/client-src` — behavioral reference only, never transliterated.
