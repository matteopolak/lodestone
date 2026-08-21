# F3 debug overlay

## What it is

The F3 instrument: two columns of stats over the world in vanilla's own plate,
pitch and font, plus the F3+B hitbox and F3+G chunk-border world overlays. The
presentation is a port of `DebugScreenOverlay`; the *content* is curated — vanilla
lines that describe the JVM are dropped rather than faked, and this engine's own
counters take the slots they leave.

## How it works

### Presentation, and where every constant came from

All of it is `DebugScreenOverlay` in `.cache/mc/26.2/client-src`. There are only
five numbers, and `extractLines` spends all of them:

| thing | vanilla | here |
|---|---|---|
| line pitch | `int height = 9` | `DEBUG_LINE_H` |
| left inset | `left = 2` when `alignLeft` | `DEBUG_MARGIN` |
| right inset | `left = guiWidth() - 2 - font.width(line)` | `DEBUG_MARGIN`, subtracted from `w` |
| top inset | `top = 2 + height * i` | `DEBUG_MARGIN` |
| plate | `fill(left - 1, top - 1, left + width + 1, top + height - 1, -1873784752)` | `DEBUG_LINE_BG`, `0x90505050` |
| ink | `text(font, line, left, top, -2039584, false)` | `DEBUG_LINE_INK`, `0xFFE0E0E0`, **no shadow** |
| scale | `graphics` is already the GUI-scaled canvas; no pose scale | `debug_scale = 1.0` |

Three of those are worth stating in prose because they are the ones a
reimplementation gets wrong:

- **The plate is `9` tall, not `8`.** `top - 1` to `top + height - 1` spans
  exactly `height` rows, so consecutive plates tile with no seam and no overlap,
  and it is `2` wider than the text (`left - 1` to `left + width + 1`). Ours is
  `rect_px(x - 1, y - 1, tw + 2, DEBUG_LINE_H, …)` — the same rectangle.
- **The overlay draws at scale 1.0**, in the same `gui_scale`-divided logical
  canvas the rest of the HUD lays its constants into. It must **not** pick up
  any ad-hoc HUD-wide pitch; doing so is what "the text is way too big" was,
  and it is the same double-apply the XP level number's own comment records
  one screen over. That ambient pitch (`HUD_TEXT_SCALE`/`hud_line_h()`) no
  longer exists at all — chat, its last consumer, was ported to vanilla's own
  `chatScale` metrics too (`docs/hud-text-scale.md`) — so there is nothing left
  for a reimplementation to pick up by mistake.
- **Empty strings are skipped but still advance `i`.** That is what makes a `""`
  a group separator rather than an empty plate — `Strings.isNullOrEmpty` guards
  both of vanilla's passes and the index keeps counting.

Plates first, then glyphs: vanilla runs two full passes inside `extractLines` so
a later line's plate cannot cover an earlier line's text. Ours does both columns'
plates and then both columns' glyphs, which is the same guarantee one step
stronger — vanilla calls `extractLines` once per column, so on a canvas narrow
enough for the columns to overlap, a right-hand plate *can* cover left-hand text
there and cannot here.

The font is the real vanilla `ascii.png`-derived raster whenever one is attached
(`HudRenderer::new` takes `VanillaFont::shared()`); `b.text_width` is the same
measure `b.text` advances by, which is why the right column's `x` is computed from
it rather than from a restated glyph width. With no font attached — headless
gates, jar-less runs — both fall back to the fixed 5×7 debug font and the
right-alignment still holds, because both sides of the subtraction change
together.

### The text

`DebugStats` (`crates/lodestone-shell/src/hud.rs`) owns the content in
`left_lines()` and `right_lines()`, with `lines()` as their concatenation — the
property that stops a line added to one column from going missing from every
consumer of the flat list. `debug_overlay_columns_carry_vanillas_spacers_and_concatenate`
asserts it directly.

What the overlay draws, on a live session:

```
0 fps (0.00 ms)                                        Lodestone 0.1.0

Client Light: 11 (4 sky, 11 block)                     local world
Difficulty: normal                     C: 191/1042 sections, 441 columns, …
                                                                  E: 12
XYZ: -0.500 / 70.25000 / 88.750                        P: 40/40, 0 unresolved
Block: -1 70 88
Chunk: -1 4 5 [31 5 in r.-1.0.mca]                     F/T: 0.40
Facing: west (Towards negative X) (45.0 / 12.3)        Live cols: 441, drops: 0
minecraft:overworld                                    Occl: active, nodes: …
Section-relative: 15 06 08
Targeted Block: -1, 70, 87                             Mem: 512 MiB (RSS)
                                                       World: 41984 KiB
Debug overlays: [F3+B] Hitboxes hidden;                Mesh VRAM: 65536/…
                [F3+G] Chunk borders hidden
                                                       Apple M5 (Metal)
                                                       …limits…
```

The `Debug overlays:` line is one line, wrapped here only to fit the page.
Note where the dimension sits: **between `Facing:` and `Section-relative:`**, not
at the end of the block. That is vanilla's group insertion order —
`DebugEntryPosition` adds five lines and `DebugEntrySectionPosition` appends its
one to the *same* `position` group afterwards, so the section-relative triple
comes last of the six.

### Which vanilla line became what

`DebugScreenEntries` registers 44 entries. `DebugScreenProfile.DEFAULT` enables
nine of them at `IN_OVERLAY`, and `DebugScreenEntryList.rebuildCurrentList`
sorts the enabled set with `Comparator.naturalOrder()` — `Identifier.compareTo`
compares **path first**, so the display order is alphabetical by path, not
registration order. The nine are `3d_crosshair`, `fps`, `game_version`, `memory`,
`player_position`, `player_section_position`, `simple_performance_impactors`,
`system_specs`, `tps`.

Ported means the format string is vanilla's, character for character.

| vanilla entry | vanilla line | verdict | ours |
|---|---|---|---|
| `fps` | `%d fps T: %s%s` | **replaced** | `0 fps (0.00 ms)` — the `T:` token is the framerate-limit *target* and the parenthetical is the swapchain present mode; neither is an option this shell honours, so the slot carries the frame time we do measure rather than a limit we do not enforce |
| `game_version` | `Minecraft <ver> (<launched>/<brand>)` | **replaced** | `Lodestone <ver>`, from `CARGO_PKG_VERSION` |
| `tps` | `"<brand>" server, %.0f tx, %.0f rx` / `Integrated server @ %.1f/%.1f ms…` | **replaced** | the session status line (`local world`, `connecting…`). No smoothed server tick time and no packet-rate counters exist to fill vanilla's shape; skipped entirely when empty, as vanilla's entry adds nothing with no connection |
| `player_position` | `XYZ: %.3f / %.5f / %.3f` | **ported** | verbatim, including the asymmetric precision |
| `player_position` | `Block: %d %d %d` | **ported** | verbatim |
| `player_position` | `Chunk: %d %d %d [%d %d in r.%d.%d.mca]` | **ported** | verbatim |
| `player_position` | `Facing: %s (%s) (%.1f / %.1f)` | **ported** | verbatim, with `Direction.toString()`'s lowercase name and `Mth.wrapDegrees` on both angles |
| `player_position` | `<dimension> FC: <n>` | **half ported** | the identifier is real — `minecraft:the_nether`, read from the local player's `ServerDimension` component, so it follows a portal trip and not just login. The ` FC: <n>` suffix is **dropped**: `getForceLoadedChunks` is `ServerLevel`-only and printing `0` would be a number we did not measure. Absent entirely before login, as vanilla's whole position group is with no camera entity |
| `player_section_position` | `Section-relative: %02d %02d %02d` | **ported** | verbatim |
| `light_levels` | `Client Light: %d (%d sky, %d block)` | **ported** | verbatim. `-` when there is no data — before login, or for an unloaded section |
| `light_levels` | `Server Light: (%d sky, %d block)` | **dropped** | behind `SharedConstants.DEBUG_SHOW_SERVER_DEBUG_VALUES`, off in a shipped build |
| `looking_at_block_state` | `Targeted Block: <x>, <y>, <z>` | **ported** | verbatim (prefix and comma separators) |
| `looking_at_block_state` | the block state and its properties | **dropped** | not plumbed; `DebugStats::target` carries only the position |
| `local_difficulty` | `Local Difficulty: %.2f // %.2f` | **replaced** | `Difficulty: normal (locked)` — the world difficulty the server reported. Vanilla's is a *server*-side scalar folded from inhabited time and moon brightness; the prefix drops `Local` because ours is not that number. Names are vanilla's lowercase serialized keys |
| `memory` | `Mem: %2d%% %03d/%03dMiB` | **replaced** | `Mem: <n> MiB (RSS)` — real process RSS. The percentage is `used / maxMemory` and there is no `-Xmx` to divide by |
| `memory` | `Allocation rate: %03dMiB/s` | **dropped** | a JVM heap-delta-between-GCs figure |
| `memory` | `Allocated: %2d%% %03dMiB` | **dropped** | `Runtime.totalMemory` |
| `detailed_memory` | `Memory (heap)` / `Memory (non-heap)` | **dropped** | `MemoryMXBean`, both JVM-only |
| `system_specs` | `Java: %s` | **dropped** | Java-specific |
| `system_specs` | `CPU: %s` | **dropped** | `GLX._getCpuInfo()` has no portable equivalent wired up |
| `system_specs` | `Display: %dx%d (%s)` | **dropped** | window size is not on `DebugStats` |
| `system_specs` | device name, backend + driver | **ported in spirit** | the `adapter` block, resolved once from `wgpu::Adapter::get_info()`, plus the reported limits |
| `chunk_render_stats` | `C: %d/%d %sD: %d, %s` | **replaced** | `C: <drawn>/<graph nodes> sections, <n> columns, <n> quads`. The occlusion graph is the closest thing here to `ViewArea.size()`; view distance and the dispatcher queue have no counterpart |
| `entity_render_stats` | `E: %d/%d, SD: %d` | **replaced** | `E: <drawn>` — only the drawn count is tracked |
| `particle_render_stats` | `P: %d` | **replaced** | `P: <drawn>/<alive>, <n> unresolved`. The unresolved count stays: a zero draw against a non-zero alive count is the "renders nothing, reports fine" state it exists to expose |
| `simple_performance_impactors` | `%s%sB: %d`, `Filtering: %s` | **dropped** | improved-transparency, cloud status, biome-blend radius and texture filtering are options this shell does not have yet. When it gains them, this is a ported line, not a replaced one |
| `gpu_utilization` | `GPU: %d%%` | **dropped** | `Minecraft.getGpuUtilization` has no wgpu equivalent |
| `biome` | `Biome: <id>` | **dropped** | not plumbed into `DebugStats` |
| `day_count` | `Day #%d` | **dropped** | not plumbed |
| `heightmap`, `chunk_generation_stats`, `chunk_source_stats`, `entity_spawn_counts`, `sound_mood`, `sound_cache`, `post_effect`, `looking_at_*_tags`, `looking_at_entity*` | — | **dropped** | none of these has a datum on this side yet; all are additions rather than parity fixes |
| `entity_hitboxes`, `chunk_borders` | `DebugEntryNoop` — world overlays | **ported** | F3+B and F3+G, below |
| `3d_crosshair`, `chunk_section_paths`, `chunk_section_octree`, `chunk_section_visibility`, `visualize_*` (9) | `DebugEntryNoop` — world overlays | **dropped** | not built |
| the `Debug charts:` block | `formatChart` = `[mod+key] Name visible|hidden`, joined with `; `, when the overlay is visible | **replaced** | `Debug overlays: [F3+B] Hitboxes visible; [F3+G] Chunk borders hidden` — vanilla's shape carrying the two toggles that *do* exist. None of vanilla's four charts does, so naming them would be a hint that lies |
| the `To edit: press [F3+…]` line | points at the entry-enable screen | **dropped** | there is no such screen here, and a chord that does nothing is worse than no hint |

Lines of ours with **no vanilla counterpart**, and why each stays:

| ours | why |
|---|---|
| `F/T: <n>` | fixed-timestep health. Vanilla runs 20 ticks/s, so at 50 fps this settles near 0.4; a drift is a physics-loop bug that nothing else on screen shows |
| `Live cols: <n>, drops: <n>` | `drops` is the silent-mesh-drop detector. A live column the server reports loaded that fails to mesh used to vanish with no signal at all; a healthy session reads `0` |
| `Occl: active, nodes: …, cull: …, shadow: …, walks: …` | see below — the `active`/`off` token is the load-bearing one |
| `World: <n> KiB` | heap owned by loaded chunks. The single honest world-memory number: it reads the same whether the world is locally generated or client-owned |
| `Mesh VRAM: <live>/<reserved> KiB` | see below. Vanilla has no GPU-residency line at all |
| `Recipes:`, `Border:`, `Maps:`, `Spawn:` | conditional folds-reached-the-client diagnostics, each drawn only once its datum has actually arrived. They live on `HudFrame` rather than `DebugStats`, so `build_inner` appends them, each opening its own spacer |

### Where the two live wires come from

Both of these were lines that could have read a constant, which is the whole
reason they are wired rather than formatted:

- **The dimension** is `DebugStats::dimension`, filled in `sim/step.rs`'s
  `refresh_stats` from the local player's
  `lodestone_ecs::session::ServerDimension` component — **inside** the
  `refreshes_world_stats` throttle, because it takes the ECS read lock. Reading
  that component rather than caching a shell-side value at login is load-bearing:
  the fold updates on `Respawned` as well as `Login`, which is how portal travel
  is reported, and a login-time cache is exactly the stale-value shape that
  produced the too-bright Nether.
- **The two toggle states** are `hitboxes_shown` / `chunk_borders_shown`, copied
  every frame in `app/redraw.rs` from the same `Arc<AtomicBool>`s the world-line
  source closure reads (`WindowApp::debug_hitboxes` / `debug_chunk_borders`,
  flipped in `app/lifecycle.rs`, consumed by `install_debug_lines_source`). The
  write lives in `redraw.rs` rather than in `refresh_stats` because the atomics
  are owned by `WindowApp`, not by `Sim`. One source of truth: the line's whole
  job is to report the state that decides whether boxes draw, so a second mirror
  is how a hint that lies gets shipped.

The chord names in `format_toggle` are **literals**, and correctly so: unlike
vanilla's these two are not `KeyMapping`s, so there is no
`getTranslatedKeyMessage` to ask and no unbound case to handle. **If they ever
become rebindable this must read the binding** — otherwise the hint will keep
naming the old key with total confidence, which is the failure the whole line was
added to prevent.

### How the column split was decided

This supersedes an earlier note in `left_lines`' own doc comment, which recorded
a deliberate refusal to follow vanilla on the grounds that "vanilla's split is
mechanical, and a mechanical halve would reshuffle both columns every time a line
is added". The premise was half right and the conclusion did not follow.
`DebugScreenOverlay.extractRenderState` does not halve *lines*. It halves within
three **categories**, and the categories are semantic:

| category | how a line gets there | how it is placed |
|---|---|---|
| priority | `addPriorityLine` | into whichever column is currently shorter |
| regular | `addLine` | the flat list halved at `mid = (n + 1) / 2` |
| group | `addToGroup(id, …)` | whole named groups, halved by *group count*, insertion order |

Each category block is followed by a `""`. So what decides a line's column is
which category its entry used, and reproducing *that* is what makes the layout
read as vanilla's — while being stable in exactly the way the old note wanted,
because adding a line to a group cannot move any other line across columns.

What is **not** reproduced is running vanilla's halve over *our* entry set. Ours
differs (no JVM entries, several engine-only ones), and the arithmetic on it puts
`XYZ:` in the right column — further from vanilla's screen, not closer. So the
category→column assignment is still by hand, but it is now *derived from
vanilla's own default-profile output* rather than chosen freely: running vanilla's
algorithm on `DebugScreenProfile.DEFAULT` puts the fps line, the perf-impactor
lines, the memory group and the position group **left**, and the version line, the
tps line and the system group **right**. Ours match that placement, and the order
*within* each column is vanilla's.

The consequence worth knowing before you read the screen: **the fps line heads the
left column and the version line heads the right one**, which looks backwards
until you notice `addPriorityLine` fills the shorter column and both start empty.
The gate asserts that specific asymmetry.

### Reading the engine lines

The `Occl` line is folded in `app/redraw.rs` from `RenderStats` (see
[terrain culling](./terrain-culling.md)). Three things about it:

- **`active`/`off` is the load-bearing token.** Every failure mode of this cull
  draws *more*, so a `cull` of `0` cannot on its own tell an open surface from a
  graph that refused to walk. Without the flag on screen a silently-dead graph
  looks identical to a correct one on a clear day.
- **`cull: 0` is often correct.** At a near-horizontal camera the frustum has
  already removed the subsurface and the graph has nothing left to take. It shows
  up looking steeply down or underground — measured 191 → 59 sections at pitch 75.
- **`walks` is session-cumulative, and must not increment while you turn on the
  spot.** That is the invalidation cadence's whole claim (8-block cell crossings,
  frustum decoupled from reachability), and it is only readable across two frames.
  Rising while standing still is a bug, not activity.

`nodes` is deliberately larger than the drawn section count: the graph includes
sections with no geometry, and a `nodes` that tracks the drawn count instead means
the fully-solid sections are missing and the walk has no floor to see.

**The section and quad counts on `C:` are per-frame *drawn* counts; `Mesh VRAM` is
*residency*.** The distinction is the whole content of that line, and getting it
wrong was a reported bug: `Mesh VRAM` used to be
`vram_bytes(RenderStats::total_quads)`, and `total_quads` only accumulates over
sections that survived the cull, so the VRAM figure moved every time the player
turned on the spot. That reads as buffers being allocated and freed, and nothing
of the kind happens — `RenderState::upload_section` and `remove_section` are the
only two paths that touch GPU mesh storage, and both are driven by chunk
arrival/unload, never by the camera. Rotating changes *visibility*, not residency,
which is exactly why a rotation is the input that tells the two apart.

Both figures come from `RenderState::resident_mesh_bytes` and
`reserved_mesh_bytes`, measured off the real `wgpu::Buffer` sizes and the model
arena's own occupancy rather than estimated from a quad count:

- **live** — the spans currently handed out to resident sections.
- **reserved** — the arena blocks the driver is holding. `ModelMeshArena`
  allocates 32 MiB vertex + 8 MiB index blocks and **never releases one**, so this
  is a high-water mark: walking away returns spans to the free pool for the next
  region to reuse, and reserved stays put. That retention is the design, and it is
  why there is no eviction budget to tune here.

Read them as a pair. Live sawtoothing under a flat reserved figure is healthy
reuse; reserved climbing while live does not is fragmentation, and is the only
shape that would justify a byte budget. The old estimate also priced every
live-vanilla quad at the packed path's 72 B when a `ModelVertex` quad is 152 B, so
it under-reported real mesh VRAM by a further ~2.1× on top of the cull factor.
`mesh_vram_is_a_function_of_residency_not_of_the_camera` (`gpu/sections.rs`,
`#[ignore]`d, needs an adapter) pins the invariant and computes the old formula
alongside as its own control: measured 1,853,568 → 1,365,552 bytes across a pure
180° turn, against a byte-identical 5,777,856 for the residency figure.

**KiB, not vanilla's MiB, on purpose.** The live figure's sawtooth is the signal
and MiB granularity flattens it.

### The chords

`app/input.rs`. F3 no longer resolves to `ToggleDebugOverlay` — it resolves to
`KeyOutcome::DebugModifier(pressed)` on **both** edges, and `app/lifecycle.rs`
toggles the overlay on the release only when no chord fired
(`self.debug_chord_used`). That is vanilla's
`keyDebugModifier.setDown(!didDebugAction)` in `KeyboardHandler`. Toggling on the
press, as this used to, makes F3+B both open the overlay and toggle hitboxes on
one keystroke.

`KeyGate::debug_held` carries the held state into `resolve_key`, so the precedence
stays in the one pure function every other input decision lives in.

### The world overlays

Both ride the **existing** `DebugLineRenderer` channel rather than getting a pass
of their own: they are world-space coloured segments, which is exactly what it
draws, and it draws last in the world pass so they read over everything real.
`app/session.rs`'s `install_debug_lines_source` closure appends:

- `gpu::entity_hitbox_vertices` — one white wireframe box per `EntityDraw`, plus
  a cyan 2-block look ray from eye height. Dimensions come from the jar-derived
  `lodestone_data::entity_dimensions` census scaled by the draw's own `scale` —
  the *same* source `gpu/nametag.rs` uses for the tag anchor, so a hitbox and a
  nametag cannot disagree about an entity's height.
- `gpu::chunk_border_vertices` — the four yellow uprights of the player's chunk
  plus a ring at every 16-block section boundary.

Toggles are `Arc<AtomicBool>` because the source closure is
`Fn() + Send + Sync + 'static` and cannot borrow `self`.

## How to change it, and the gotchas

- **Adding a line means picking a category, not picking a column.** Decide which
  of vanilla's three it is — priority, regular, or a named group — and put it in
  the block that category already occupies, keeping its `""` separator. Do not
  reach for a fresh column assignment; the whole reconciliation above is that the
  category is what carries the meaning.
- **Match the typography or the screen mixes two conventions.** `Key: value`,
  sentence case for the key, lowercase for an enum (`Difficulty: normal`,
  `Facing: west`), a space after the colon, `, ` between fields on one line. The
  overlay used to be SHOUTED with `k=v` shorthand in places; a new line in either
  of those styles is now the odd one out.
- **Never `as i64` a coordinate.** `Entity.blockPosition()` is `Mth.floor`, and a
  cast truncates toward zero: it maps `-0.5` to block `0` in chunk `0` where the
  truth is block `-1` in chunk `-1`. Everything that divides or masks a
  coordinate — `Block:`, `Chunk:`, `Section-relative:` — inherits the error, and
  **it is invisible at the origin**, which is why the gate's fixture sits at
  `[-0.5, 70.25, 88.75]` and pairs every expectation with what the truncating
  version produces. `DebugStats::block_position` is the one place that floors.
- **The three O(resident-world) stats are throttled to one frame in 30**
  (`sim/step.rs`, `WORLD_STATS_PERIOD`, commit `f4e73530`). The light read went
  *inside* that `if` for the same reason. **Anything you add that touches the
  world or makes a syscall belongs inside it**, not next to the cheap every-frame
  fields above.
- **There is no light-level pie chart to port.** 26.2 registers
  `minecraft:light_levels` as a *text* entry (`DebugEntryLight`) and the pie was
  removed. If a pie is wanted anyway it is a new HUD primitive, not a parity item.
- **F3+B and F3+G are not rebindable.** Vanilla declares them as real
  `KeyMapping`s (`key.debug.showHitboxes` = 66, `key.debug.showChunkBorders` = 71,
  `Options`); here they are hardcoded `KeyCode::KeyB`/`KeyG` behind the modifier.
  Making them bindable means new `InputAction`s plus their labels, category and
  GLYPH keysyms in `keybinds.rs` — a real gap, deliberately not taken, and the
  *modifier* is likewise a `KeyGate` flag rather than vanilla's second
  `KeyMapping` on the same keysym.
- **The chunk-border column range is passed in, not assumed.**
  `install_debug_lines_source` resolves `min_y`/`height` from
  `NetClient::world_dimensions` and falls back to the overworld `(-64, 384)` only
  when there is no session. A hardcoded `-64..320` would silently draw the wrong
  box in the nether or a custom-height dimension. **The range is captured at
  install time**, so a dimension change needs the source reinstalled — call
  `install_session_render_sources` on a portal trip if that becomes visible.
- **`MAX_DEBUG_LINE_SEGMENTS` is 4096** and a box is 12 segments, so hitboxes cap
  out around 340 entities and truncate silently past that. A 24-section chunk
  border is ~100 segments.
- **An entity whose type path the census cannot resolve gets no box**, rather than
  a plausible default one. A wrong hitbox is worse than a missing one: the
  overlay's whole value is being believed.
- **The `Debug overlays:` line must keep reading the atomics, not a mirror.** It
  is the only on-screen report of whether F3+B and F3+G are on, so a shell-side
  copy that drifts turns the overlay's most trustworthy property — that it is
  believed — into the bug. Same for the chord literals; see above.
- **F3+B/F3+G's lines were a `PrimitiveTopology::LineList` until this fix**,
  which rasterizes at exactly one *physical* pixel regardless of resolution or
  DPI scale — the same failure `gpu/outline.rs`'s own module doc already names
  for the block-highlight box. At a real gameplay resolution that reads as
  "doesn't draw at all" rather than merely thin, even though the closure
  feeding it (`install_debug_lines_source`) was producing correct geometry the
  whole time. `DebugLineRenderer` (`gpu/debug_lines.rs`) now expands each
  segment into a screen-space-thickened triangle ribbon — the identical
  technique `OutlineRenderer` uses — with per-vertex colour threaded through
  `debug_lines.wgsl` so a hitbox's white and a chunk border's yellow/blue
  survive the expansion. `MIN_LINE_WIDTH_PX = 1.5` is deliberately thinner
  than the outline pass's `2.5`: a diagnostic wireframe should read as a
  *line*, not a highlighted edge.
- **No existing pixel gate exercised `entity_hitbox_vertices`/
  `chunk_border_vertices` specifically.** Every debug-line gate in
  `gpu/pixel_gates.rs` installed a synthetic closure
  (`structure_block_outline_vertices`, a bare billboard), so a break in either
  real producer was invisible to the rest of that corpus — the
  shared-construction-path blindness `DESIGN.md` §12 already names for a
  render frame reached through one factory, one hop from the pipeline.
  `entity_hitbox_and_chunk_border_vertices_draw_visible_pixels` closes that
  gap: it feeds the real production functions through the real pipeline and
  asserts pixels move (423 px / 828 px at 320×240 after the width fix).
  Neutered by forcing `debug_line_count` to `0` at the draw call and confirmed
  red before landing the fix above.
- **Not built**: the four debug charts, the lightmap blit, vanilla's runtime
  entry-enable screen and its `debug-profile.json`, and the nine `visualize_*`
  world overlays. All are additions; none is a gap in what is here. The
  profiler pie chart **is** now built — see `docs/frame-profiling.md`'s "Pie
  chart" section — as a deliberate beyond-vanilla addition to the
  frame-profiling instrument, not a port of `DebugScreenOverlay`'s light-level
  pie (which vanilla itself removed in 26.2; see the light-level bullet
  above).

## Gates

All three are `hud.rs` unit tests, no adapter needed:

- `debug_overlay_plate_and_ink_match_vanillas_fill_literals` — transcribes
  `extractLines`' two signed Java `int` colours (`-1873784752`, `-2039584`) and
  **unpacks them in the test** rather than restating four floats, so a channel
  swap or a dropped alpha fails. Also pins the pitch and the margin. It used to
  additionally pin `DEBUG_LINE_H < hud_line_h()` as a negative control against
  the ad-hoc HUD-wide pitch; that pitch was deleted outright once chat (its
  last consumer) moved to vanilla's own metrics, so there is no longer a
  second pitch to compare against.
- `debug_overlay_ported_lines_match_vanillas_format_strings` — all nine ported
  lines, character for character, at `[-0.5, 70.25, 88.75]`, yaw `405`, in
  `minecraft:the_nether`, with hitboxes **on** and borders **off**. Each
  expectation carries the value the superseded or unwired version produced for the
  same input, and the gate **fails if the two coincide**. Mismatches are collected
  and asserted on the collection, so a regression reports every wrong line rather
  than the first. Three controls, all run:
  - `block_position` back to `as i64` → 3 arms, 4 messages (`Block: 0 70 88`,
    `Chunk: 0 4 5 [0 5 …]`, `Section-relative: 00 06 08`).
  - the dimension line hardcoded to `minecraft:overworld` and the two toggle
    booleans transposed → 2 arms, 4 messages. **The toggles are set to different
    values precisely so the transposition is visible** — equal booleans are the
    one input a swapped adjacent same-typed pair survives.
  - with the collection assert satisfied, the hardcoded dimension push also fails
    the pre-login absence check, which is the control proving that check can fire
    rather than merely being satisfied by a broken search.
- `debug_overlay_columns_carry_vanillas_spacers_and_concatenate` — `lines()` is
  exactly `left_lines() ++ right_lines()`, both columns carry at least two group
  spacers, neither ends with one (a trailing spacer draws nothing and only pads
  the flat list), the fps line heads the left column and the version line the
  right, and the adapter block opens with its spacer.

## Configuration

`gui_scale` (`options.json`) scales the whole overlay through
`menu::render::logical_canvas`, like the rest of the HUD. Nothing else — vanilla's
`debug-profile.json` and its per-entry `ALWAYS_ON`/`IN_OVERLAY`/`NEVER` statuses
are not implemented, so the entry set here is fixed.

## Dependencies

`lodestone_data::entity_dimensions` (hitbox sizes), `crate::net`
(`entity_light_at`, `world_dimensions`), `crate::gpu::debug_lines` (the draw),
`crate::entities::extracted_entity_draws` (the entity list),
`crate::hud::vanilla_font` (the glyphs and their advances).
