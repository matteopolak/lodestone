# `gpu/` module layout and shader conventions

## What it is

How `crates/lodestone-shell`'s render coordinator (`RenderState`) is split across
`gpu.rs` and a `gpu/` folder of submodules, plus the convention every WGSL shader in
the workspace follows: one `.wgsl` file per pipeline, pulled in with `include_str!`,
never inlined as a Rust string literal.

## How it works

### The root

`crates/lodestone-shell/src/gpu.rs` holds only what has to be visible crate-wide: the
module doc, `mod`/`pub use` declarations, the three top-level consts (`SKY_COLOR`,
`FOG_START_FRACTION`, `DEFAULT_RENDER_DISTANCE_CHUNKS`), the `RenderState` **struct
definition**, and a handful of items several submodules share (the armour/wool/flame
batch-accumulator structs, `humanoid_armour_slot`, `transparent_placeholder_atlas`, a
`#[cfg(test)]` sky-colour reference helper). These stay in the root because a private
item there is visible to every descendant module — moving a shared struct down into
one submodule would force `pub(super)` on it *and* on every field a sibling module
reads, which is real annotation cost for no benefit.

### `RenderState`'s own `impl` surface, one file per seam

Rust allows multiple inherent `impl RenderState` blocks in one crate, so each concern
gets its own file. A method whose only caller lives in the same file stays private;
one a sibling needs is `pub(super)` (visible to the whole `gpu` subtree, not just the
immediate parent — Rust privacy is about module-tree descendance, not file location).

| file | owns |
|---|---|
| `gpu/state.rs` | `RenderState::new` and the install/setter seam — fog and clear colour, the optional passes (sky, screen effects, weather, particle atlas), and every per-frame "source" setter. Several sources must be **re-installed every frame** because their value is partial-tick interpolated; each setter's own doc says which. |
| `gpu/sections.rs` | section residency for both terrain paths (upload/remove/resize/animate), plus the read-only borrows the HUD's 3-D item pass shares. |
| `gpu/frame.rs` | the frame graph — the public `render*` entry points funnelling into `render_inner`. **Submission order is load-bearing** here, not any individual draw; opaque-vs-translucent ordering is not a matter of taste. |
| `gpu/world_items.rs` | dropped items, projectile billboards, items in mobs' hands — item models through the model pipeline, not the entity pipeline. |
| `gpu/entity_passes.rs` | every per-entity layer (entities, armour, wool, flame, block entities), all resolving off the same resolver and pose input so a layer can never draw off a pose the body pass didn't. |
| `gpu/tests.rs` / `gpu/pixel_gates.rs` | hermetic gates (no adapter) and `#[ignore]`d GPU pixel gates respectively. |

Per-pass resources, split out earlier and unchanged in shape:

- `gpu/outline.rs` — the mining-target wireframe pass.
- `gpu/debug_lines.rs` — world-space debug lines.
- `gpu/stats.rs` — `RenderStats`, the F3 counters.
- `gpu/terrain.rs` — packed/demo section storage and the live-vanilla model path's
  shared-camera-uniform arena (see `docs/terrain-rendering.md`).
- `gpu/entities.rs` — the mob pipeline, armour layers, wool, texture loading.
- `gpu/sources.rs` — every polled per-frame source the render module can't wire
  itself (entity light, sky darken, time of day, third-person body pose, hand swing,
  main hand).
- `gpu/first_person.rs` — the first-person hand pass.
- `gpu/nametag.rs` — billboarded nametags, its own two-pipeline shader.
- `gpu/block_entities.rs` — chest/skull/bell rigs, reusing the entity pipeline rather
  than adding a fifth bind group.
- `gpu/sign_text.rs` — world-space sign text, sharing `gpu/nametag.rs`'s glyph layout
  and shader. A sign's board is ordinary terrain; this pass only paints the ink.
- `gpu/screen_effects.rs` — the underwater/fire/pumpkin/spyglass/freeze/portal/
  confusion overlay input, deliberately a plain per-call argument rather than a
  polled source (see that file's module doc for the distinction).

### Shader files

Every crate that owns a pipeline keeps its shaders under `src/shaders/*.wgsl`,
pulled in with `include_str!` next to the pipeline that owns the const:

```rust
const MODEL_WGSL: &str = include_str!("shaders/model.wgsl");
```

`include_str!` resolves relative to the file containing the macro, so a module in a
subdirectory needs a `../shaders/...` path. `lodestone-render` and `lodestone-shell`
each own about a dozen files; several are byte-identical across HUD/menu/container/
effects call sites, kept as separate consts rather than merged because sharing one
file couples pipelines that are currently independent — that is a design decision for
whoever owns those passes, not a cleanup.

Two per-crate tests (`wgsl_valid.rs`, no GPU needed, ~0.02s) run every `.wgsl` file
through naga's WGSL front end and validator — the same front end `wgpu` itself runs.
This exists because `cargo check` never compiles a shader at any feature setting; the
first thing that reads the WGSL text is `create_shader_module`, which only runs
inside an `#[ignore]`d GPU gate. The test also fails on any `@vertex`/`@fragment`
found under a crate's `src/**/*.rs`, which is what stops a shader being inlined back
into Rust. It cannot catch a bind-group or `@location` mismatch between the shader
and its pipeline — only a real GPU pixel gate can.

## How to change it, and the gotchas

- **Where does a new method go?** Follow the caller. A whole new pass wants its own
  `gpu/<pass>.rs` for its resources plus an arm in `gpu/frame.rs`'s `render_inner` for
  the draw — read that file's module doc on submission order before choosing where in
  it to place the draw.
- **Adding a field to `RenderState` touches two files**: the struct in `gpu.rs` and
  the initialiser in `gpu/state.rs::new`.
- **Public API is unchanged by the split** — every item reachable as `crate::gpu::Foo`
  still is, via `pub use` at the top of `gpu.rs`. Do not remove a re-export without
  checking `crate::gpu::` usages elsewhere first.
- **`ModelRenderer`/`SectionGpu`/`ModelSectionGpu` have no separate constructor** —
  they are built with a struct literal inside `RenderState::new` in the root, so every
  field needs `pub(super)`. `EntityRenderer` has its own `::new`, so only the fields
  `RenderState`'s other methods reach into directly need it.
- **No file in `gpu/` carries a path-sensitive macro** (`include_str!`, `include_bytes!`,
  `file!`, `#[path]`) apart from each pipeline's own shader include, which resolves
  relative to *that* file. Keep it that way — code can move between directory levels
  freely only while that holds.
- **Write a new shader as its own `.wgsl` file, never inline in Rust** — not even
  "just for a quick test". `no_wgsl_is_inlined_in_rust_sources` fails on any
  `@vertex`/`@fragment` under a crate's `src/`. A `"` inside a `.wgsl` comment is
  legal and inert (WGSL's lexer just skips the comment); that used to be a real trap
  when shaders were Rust raw strings, where one stray quote in a comment terminated
  the Rust literal and rustc parsed the remaining shader text and English prose as
  code. Run `cargo test -p <crate> --test wgsl_valid` after adding a file — fastest
  way to catch a typo before building the pipeline.
- **The model shader's 4-bind-group floor, the reversed-Z depth convention, the GUI
  winding sign, gamma-space tint/shade, the sRGB-swapchain-view requirement, the
  `LineList`-on-HiDPI trap, borrowed-GPU-resource re-attachment, and `ALPHA_BLENDING`
  unpredictability are cross-cutting renderer constraints, not specific to this
  module** — see `docs/architecture.md`'s "Hard renderer constraints" for the one
  copy of that list; do not duplicate it here.

## Configuration

None specific to either the module split or the shader convention — same
`wgpu`/`lodestone-render` dependencies as before, just distributed across files.

## Dependencies

- `wgpu`, `lodestone-render`, `lodestone-assets`, `lodestone-model`, `glam`, plus
  this crate's own `entities`, `mesher`, `particles`, `resources` modules.
- `wgpu::naga` (re-exported by `wgpu` on native targets, not a direct dependency of
  either rendering crate) for the shader-validity tests.
