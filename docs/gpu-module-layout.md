# `gpu/` module layout

## What it is

`crates/lodestone-shell/src/gpu.rs` was a single ~5,300-line file carrying eight
distinct render responsibilities (block outline, debug lines, per-frame stats,
terrain storage, the entity pass, the polled per-frame "sources", the
first-person hand pass, and `RenderState` itself, the coordinator). Issue #359
split it into `gpu.rs` (the root, unchanged in role) plus a `gpu/` folder of
submodules. This is a pure reorganisation — no rendering behaviour changed, and
no test file was edited.

## How it works

A **second** pass (2026-08-04) took the root from 4,746 lines to 442 by moving
`RenderState`'s own `impl` surface out too, for the same reason as the first:
`gpu.rs` is named in `CLAUDE.md` as one of the repo's usual clobbering victims,
and several agents per session were queueing behind it. Also a pure move — 0
code lines added, 0 lost, verified by a line-set gate over whole `use`
statements; the only non-`use`/`mod` change was nine `pub(super)` prefixes.

### The root

- `crates/lodestone-shell/src/gpu.rs` (442 lines) — the module doc, the `mod`
  declarations and `pub use` re-exports, the three consts (`SKY_COLOR`,
  `FOG_START_FRACTION`, `DEFAULT_RENDER_DISTANCE_CHUNKS`), the `RenderState`
  **struct definition**, and the small items shared across several
  submodules: the six armour/wool/flame batch-accumulator structs,
  `humanoid_armour_slot`, `transparent_placeholder_atlas` and the
  `#[cfg(test)] sky_clear_bytes` reference helper.
  **These live in the root deliberately** — a private item in `gpu` is visible
  to every descendant, so keeping them here avoided ~20 `pub(super)`
  annotations and, more importantly, keeps the batch structs' *fields*
  annotation-free.

### `RenderState`'s own `impl` surface, one file per seam

Multiple inherent `impl RenderState` blocks in one crate are fine, so each of
these opens its own. Private methods stay private where their only caller is in
the same file; the nine that a sibling calls are `pub(super)`.

- `gpu/state.rs` (932 lines) — `RenderState::new`, plus the install/setter seam:
  the fog and clear colour (`set_fog`, `set_clear_color`, `fog_with_clock`),
  the optional passes (`install_sky`, `install_screen_effects`,
  `install_weather`, `install_particle_sheet_atlas`) and every polled source
  setter. Note several sources **must be re-installed every frame** because
  their value is partial-tick interpolated — each method's own doc says which.
- `gpu/sections.rs` (263 lines) — section residency for both terrain paths
  (`upload_section`, `upload_packed_section`, `remove_section`), `resize`,
  `update_animation`, and the read-only borrows the HUD's 3-D item pass shares
  (`model_atlas_view`, `model_palette_buffer`, `depth_view`, …).
- `gpu/frame.rs` (855 lines) — the frame graph: the four public `render*` entry
  points and the single `render_inner` they funnel into. **Submission order is
  the load-bearing thing in this file**, not any individual draw; see its module
  doc for the two rules that account for most of it.
- `gpu/world_items.rs` (352 lines) — `prepare_item_geometry`,
  `merge_thrown_item`, `merge_held_items`: dropped items, projectile
  billboards and items in mobs' hands, all *item models* through the model
  pipeline rather than cuboid rigs through the entity pipeline.
- `gpu/entity_passes.rs` (654 lines) — `prepare_entities`, `prepare_armour`,
  `prepare_wool`, `prepare_flame`, `prepare_block_entities`: every per-entity
  layer, all resolving off the *same* resolver and `AnimInput` so a layer can
  never be posed off a pose the body pass did not draw.
- `gpu/tests.rs` (487 lines) — the hermetic gates (no wgpu adapter).
- `gpu/pixel_gates.rs` (965 lines) — the `#[ignore]`d GPU gates that render a
  frame and read pixels back.

### Per-pass resources (the first split, issue #359)

- `gpu/outline.rs` — `CrackTarget` and `OutlineRenderer` (the mining-target
  wireframe pass, its own pipeline and shader).
- `gpu/debug_lines.rs` — `DebugLineVertex`, `debug_line_vertices`,
  `DebugLineRenderer` and `DebugLinesSource` (the world-space debug-line pass
  plugins install lines through).
- `gpu/stats.rs` — `RenderStats`, the per-frame counters surfaced to the debug
  overlay.
- `gpu/terrain.rs` — `SectionGpu` (packed/demo path), `ModelSectionGpu`,
  `SectionOriginArena` and `ModelRenderer` (the live-vanilla model path's
  shared-camera-uniform storage — see `docs/section-camera-uniform.md`).
- `gpu/entities.rs` — `EntityRenderer`: the mob pipeline, humanoid armour
  layers, the sheep wool layer, and the texture-loading helpers
  (`load_humanoid_armour_textures`, `entity_texture_from_image`,
  `synthetic_entity_texture`, `model_tint`).
- `gpu/sources.rs` — every polled per-frame "source" this render module
  cannot wire itself: `EntityLightSource`, `SkyDarkenSource`,
  `TimeOfDaySource`, `ThirdPersonBodyState`/`ThirdPersonBodySource`,
  `OutlineShapeSource`, `HandSwingSource`, `MainHandSource`.
- `gpu/first_person.rs` — `FirstPersonHand`/`FirstPersonArm` and the
  first-person hand pass: `prepare_first_person_hand`, `hand_light`,
  `write_hand_camera`, and `draw_first_person_hand` (the render-pass
  recording, extracted out of `render_inner` as a same-behaviour method move).
- `gpu/nametag.rs` — `NameTagRenderer` (issue #100): the billboarded
  entity/player nametag pass, its own two-pipeline shader (a depth-tested
  normal draw plus a depth-testless see-through draw), and the world-space
  glyph-quad layout that reuses `lodestone_assets::font::RasterFont` directly
  rather than `hud/vanilla_font.rs` (see that file's module doc for why). See
  `docs/entity-nametags.md`.
- `gpu/block_entities.rs` — `BlockEntityRenderer`/`BlockEntityDrawBatch` (issue
  #23): the chest/skull/bell rigs no block model covers. Reuses the **entity**
  pipeline on purpose rather than adding a fifth bind group — see its module
  doc and the 4-group note below.
- `gpu/sign_text.rs` — `SignTextRenderer`: world-space sign text, reusing
  `gpu/nametag.rs`'s `layout_ink_runs`/`load_font` and the same
  `shaders/nametag.wgsl`. A sign's *board* is real terrain and draws through
  the ordinary terrain pass; this only paints the text on it.
- `gpu/screen_effects.rs` — `ScreenEffects`, the per-frame underwater/fire/
  pumpkin/spyglass/freeze/portal/confusion overlay input. Deliberately a plain
  per-call argument, **not** a source — see its module doc for the distinction.

## How to change it, and the gotchas

- **Where does a new method go?** Follow the caller. A method whose only caller
  is in the same file stays private; one a sibling calls needs `pub(super)`.
  If you are adding a whole new pass, it wants its own `gpu/<pass>.rs` for the
  resources and an arm in `gpu/frame.rs`'s `render_inner` for the draw — and
  read `render_inner`'s module doc on submission order *before* choosing where
  in it to put the draw, because opaque-vs-translucent ordering is not a matter
  of taste.
- **Adding a field to `RenderState` touches two files** — the struct in `gpu.rs`
  and the initialiser in `gpu/state.rs`'s `new`. That is the one edit the split
  did not make cheaper; it is still two small files rather than one 4,700-line
  one.
- **Module-tree privacy, not file privacy.** These are all children of `gpu`
  (declared with `mod outline;` etc. in `gpu.rs`), so a private item in
  `gpu::terrain` is *not* visible to `gpu.rs` — Rust privacy only extends to
  **descendants** of the defining module. Items `RenderState`'s own methods
  touch directly (fields, small helper fns) are marked `pub(super)`, which
  grants visibility to the whole `gpu` subtree, not just the immediate parent.
  Conversely, `RenderState`'s own fields need no annotation at all: they are
  plain private, and every submodule (`gpu::terrain`, `gpu::entities`, …) is a
  descendant of `gpu`, so they can already see them.
  **The corollary is worth stating because it is what kept the second split
  small: a shared item is cheapest in the root.** Moving the batch-accumulator
  structs down into `gpu/entity_passes.rs` would have forced `pub(super)` on
  each of them *and on every one of their fields*, because `render_inner` in
  `gpu/frame.rs` reads those fields. Left in the root they need nothing.
- **`#[cfg(test)]` helpers shared by two test modules also belong in the root.**
  `sky_clear_bytes` is the sky reference both `gpu/tests.rs` and
  `gpu/pixel_gates.rs` classify silhouette pixels against. It sits in `gpu.rs`
  under `#[cfg(test)]` so `use super::*` reaches it from both, with no
  visibility annotation and no duplicated constant — and its own doc records
  that it was hardcoded twice before and *both* copies went stale.
- **Public API surface is unchanged.** Every item that used to be reachable as
  `crate::gpu::Foo` (from `sim.rs`, `app.rs`, etc.) still is, via `pub use` at
  the top of `gpu.rs`. Do not remove those re-exports without checking
  `crate::gpu::` usages elsewhere in the shell first.
- **`ModelRenderer` and `SectionGpu`/`ModelSectionGpu` have no separate
  constructor** — they are built with a struct literal directly inside
  `RenderState::new` (in the root), so *every* field has to be `pub(super)`,
  not just the ones a getter would expose. `EntityRenderer`, by contrast, has
  its own `::new`, so only the fields `RenderState`'s other methods reach into
  directly are `pub(super)`.
- **The first-person hand pass split real code, not just declarations.** The
  render-pass-recording block that used to sit inline in `render_inner`
  (matching on `FirstPersonHand::Item`/`Arm` and issuing the draw calls) is
  now `RenderState::draw_first_person_hand` in `gpu/first_person.rs`, called
  from `render_inner` (now in `gpu/frame.rs`). This was a pure extract-method
  move — same pass descriptor, same draw order, same conditional `Load` on the
  block pass elsewhere in `render_inner` (untouched). If you need to change what
  the hand pass draws, this is the method to edit; if you need to change
  *whether* it runs, that is still gated in `gpu/frame.rs` around the call site.
- **The 4-bind-group-floor comment lives with `ModelRenderer`/entity code that
  the constraint actually applies to** (`gpu/terrain.rs`, `gpu/entities.rs`),
  not in the root — check there before adding a fifth bind group anywhere in
  the model or entity shaders.
- **The old "never put a double quote in a WGSL shader" rule no longer applies
  here, and this bullet used to say it did.** That rule existed because shaders
  were inlined in Rust raw strings, where a `"` terminated the string and rustc
  then parsed the WGSL *and the prose* as code. Shaders now live in `.wgsl`
  files under `src/shaders/`, pulled in with `include_str!`, and
  `no_wgsl_is_inlined_in_rust_sources` fails on any `@vertex`/`@fragment` under
  a crate's `src/`. A `"` in a `.wgsl` comment is legal and inert — measured,
  not assumed. Write shader comments normally; see `docs/shaders.md` and
  `CLAUDE.md`.
- **No file in this module carries a path-sensitive macro**, which is why the
  second split could move code between directory levels freely. `include_str!`
  resolves relative to the *containing file*, so a shader handle moved from
  `src/gpu.rs` into `src/gpu/frame.rs` would silently change which path it
  looks for. Verified absent before the move (`include_str!`, `include_bytes!`,
  `file!`, `#[path]`); if you add one, keep it in the file whose directory the
  path is written against.

## Configuration

None specific to the split — same `wgpu`/`lodestone-render` dependencies as
before, just distributed across files.

## Dependencies

Same as the original `gpu.rs`: `wgpu`, `lodestone-render`, `lodestone-assets`,
`lodestone-model`, `glam`, plus this crate's own `entities`, `mesher`,
`particles`, `resources` modules.
