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

- `crates/lodestone-shell/src/gpu.rs` — `RenderState` and its main `impl`
  block: construction, the per-frame `render`/`render_inner` pipeline, and
  every method that is genuinely about *coordinating* the passes rather than
  about one pass's own resources. Stays the largest file on purpose — see
  `CLAUDE.md`'s note on why `RenderState` was not moved.
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

## How to change it, and the gotchas

- **Module-tree privacy, not file privacy.** These are all children of `gpu`
  (declared with `mod outline;` etc. in `gpu.rs`), so a private item in
  `gpu::terrain` is *not* visible to `gpu.rs` — Rust privacy only extends to
  **descendants** of the defining module. Items `RenderState`'s own methods
  touch directly (fields, small helper fns) are marked `pub(super)`, which
  grants visibility to the whole `gpu` subtree, not just the immediate parent.
  Conversely, `RenderState`'s own fields need no annotation at all: they are
  plain private, and every submodule (`gpu::terrain`, `gpu::entities`, …) is a
  descendant of `gpu`, so they can already see them.
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
  from `render_inner`. This was a pure extract-method move — same pass
  descriptor, same draw order, same conditional `Load` on the block pass
  elsewhere in `render_inner` (untouched). If you need to change what the hand
  pass draws, this is the method to edit; if you need to change *whether* it
  runs, that is still gated in `render_inner` around the call site.
- **The 4-bind-group-floor comment lives with `ModelRenderer`/entity code that
  the constraint actually applies to** (`gpu/terrain.rs`, `gpu/entities.rs`),
  not in the root — check there before adding a fifth bind group anywhere in
  the model or entity shaders.
- **Never put a double quote inside a WGSL shader, not even in a comment** —
  this bit twice before the split and the shaders (`gpu/outline.rs`,
  `gpu/debug_lines.rs`) moved unmodified (`sed`-extracted, not retyped) for
  exactly this reason. Use backticks in shader comments.

## Configuration

None specific to the split — same `wgpu`/`lodestone-render` dependencies as
before, just distributed across files.

## Dependencies

Same as the original `gpu.rs`: `wgpu`, `lodestone-render`, `lodestone-assets`,
`lodestone-model`, `glam`, plus this crate's own `entities`, `mesher`,
`particles`, `resources` modules.
