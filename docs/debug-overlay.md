# F3 debug overlay

## What it is

The F3 instrument: two columns of stats over the world, plus the F3+B hitbox and
F3+G chunk-border world overlays. Issue #197 turned the existing single dense
column into vanilla's two-column layout, gave every line vanilla's translucent
plate, and added the two chords and the light readout.

## How it works

### The text

`DebugStats` (`crates/lodestone-shell/src/hud.rs`) owns the content in two
methods:

- `left_lines()` — player and world: `LODESTONE`, `XYZ`, `CHUNK`, `FACING`,
  `TARGET`, `LIGHT`, `DIFFICULTY`, the status line, and the conditional `spawn`
  diagnostic.
- `right_lines()` — engine internals: `FPS`, `F/T`, chunk/section/quad counts,
  live columns and mesh drops, particles, VRAM/world/RSS, and the conditional
  `recipes=`/`border ` diagnostics.

`lines()` is the concatenation of the two, so `one_line()` and anything else
wanting "every line" needs no knowledge of the split, and a line added to either
column cannot go missing from the flat list.

The draw is in `hud.rs`'s `build_inner`. Left column at `x = HUD_MARGIN`; right
column right-aligned at `w - HUD_MARGIN - b.text_width(line, scale)`, which is
vanilla's `guiWidth() - 2 - font.width(line)`. Two passes: every plate first
(`DEBUG_LINE_BG`, vanilla's `0x90505050`), then every glyph
(`DEBUG_LINE_INK`, `0xFFE0E0E0`, no shadow) — vanilla's `extractLines` does the
same, so a later line's plate cannot cover an earlier line's text.

**Vanilla's own split is mechanical in 26.2** (`DebugScreenOverlay` balances
`regularLines` at `mid = (n + 1) / 2` and keeps named groups contiguous). Ours is
by hand, on purpose: a mechanical halve reshuffles *both* columns every time a
line is added.

### The chords

`app/input.rs`. F3 no longer resolves to `ToggleDebugOverlay` — it resolves to
`KeyOutcome::DebugModifier(pressed)` on **both** edges, and `app/lifecycle.rs`
toggles the overlay on the release only when no chord fired
(`self.debug_chord_used`). That is vanilla's
`keyDebugModifier.setDown(!didDebugAction)` (`KeyboardHandler.java:554-555`).
Toggling on the press, as this used to, makes F3+B both open the overlay and
toggle hitboxes on one keystroke.

`KeyGate::debug_held` carries the held state into `resolve_key`, so the
precedence stays in the one pure function every other input decision lives in.

### The world overlays

Both ride the **existing** `DebugLineRenderer` channel rather than getting a pass
of their own: they are world-space coloured segments, which is exactly what it
draws, and it draws last in the world pass so they read over everything real.
`app/session.rs`'s `install_debug_lines_source` closure now appends:

- `gpu::entity_hitbox_vertices` — one white wireframe box per `EntityDraw`, plus
  a cyan 2-block look ray from eye height. Dimensions come from the jar-derived
  `lodestone_data::entity_dimensions` census scaled by the draw's own `scale` —
  the *same* source `gpu/nametag.rs` uses for the tag anchor, so a hitbox and a
  nametag cannot disagree about an entity's height.
- `gpu::chunk_border_vertices` — the four yellow uprights of the player's chunk
  plus a ring at every 16-block section boundary.

Toggles are `Arc<AtomicBool>` because the source closure is
`Fn() + Send + Sync + 'static` and cannot borrow `self`.

### The light readout

`DebugStats::light` is `Option<(sky, block)>`, filled in `sim/step.rs`'s
`refresh_stats` from `net::entity_light_at` with the dimension's sky policy from
`shared_sky_default`. **`None` is drawn as `LIGHT -`**, which is the honest state
before login or for an unloaded section.

## How to change it, and the gotchas

- **The three O(resident-world) stats are throttled to one frame in 30**
  (`sim/step.rs`, `WORLD_STATS_PERIOD`, commit `f4e73530`). The light read went
  *inside* that `if` for the same reason. **Anything you add that touches the
  world or makes a syscall belongs inside it**, not next to the cheap
  every-frame fields above.
- **Issue #197 asked for a "light-level pie chart"; 26.2 does not have one.**
  `DebugScreenEntries` registers `minecraft:light` as a *text* entry
  (`DebugEntryLight`) printing `Client Light: <raw> (<sky> sky, <block> block)`,
  and the pie was removed. `LIGHT` reproduces the entry that exists rather than
  a chart that does not. If a pie is wanted anyway it is a new HUD primitive,
  not a parity item.
- **F3+B and F3+G are not rebindable.** Vanilla declares them as real
  `KeyMapping`s (`key.debug.showHitboxes` = 66, `key.debug.showChunkBorders` =
  71, `Options.java`); here they are hardcoded `KeyCode::KeyB`/`KeyG` behind the
  modifier. Making them bindable means new `InputAction`s plus their labels,
  category and GLYPH keysyms in `keybinds.rs` — a real gap, deliberately not
  taken, and the *modifier* is likewise a `KeyGate` flag rather than vanilla's
  second `KeyMapping` on the same keysym.
- **The chunk-border column range is passed in, not assumed.**
  `install_debug_lines_source` resolves `min_y`/`height` from
  `NetClient::world_dimensions` and falls back to the overworld `(-64, 384)` only
  when there is no session. A hardcoded `-64..320` would silently draw the wrong
  box in the nether or a custom-height dimension. **The range is captured at
  install time**, so a dimension change needs the source reinstalled — call
  `install_session_render_sources` on a portal trip if that becomes visible.
- **`MAX_DEBUG_LINE_SEGMENTS` is 4096** and a box is 12 segments, so hitboxes
  cap out around 340 entities and truncate silently past that. A 24-section
  chunk border is ~100 segments.
- **An entity whose type path the census cannot resolve gets no box**, rather
  than a plausible default one. A wrong hitbox is worse than a missing one: the
  overlay's whole value is being believed.
- **Not built**: the remaining `DebugScreenEntries` visual toggles (F3+1/2
  charts, `3d_crosshair`, `chunk_section_paths`, the `visualize_*` family), and
  vanilla's runtime entry-enable profile.

## Configuration

`gui_scale` (`options.json`) scales the whole overlay through
`menu::render::logical_canvas`, like the rest of the HUD. Nothing else.

## Dependencies

`lodestone_data::entity_dimensions` (hitbox sizes), `crate::net`
(`entity_light_at`, `world_dimensions`), `crate::gpu::debug_lines` (the draw),
`crate::entities::extracted_entity_draws` (the entity list).
