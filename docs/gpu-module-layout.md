# `gpu/` module layout and shader conventions

## What it is

How `crates/lodestone-shell`'s render coordinator (`RenderState`) is split across `gpu.rs` and a `gpu/` folder of submodules, plus the convention every WGSL shader follows: one `.wgsl` file per pipeline, pulled in with `include_str!`, never inlined as a Rust string.

## How it works

**Root.** `crates/lodestone-shell/src/gpu.rs` holds only what must be crate-visible: module doc, `mod`/`pub use` declarations, three consts (`SKY_COLOR`, `FOG_START_FRACTION`, `DEFAULT_RENDER_DISTANCE_CHUNKS`), the `RenderState` **struct**, and items several submodules share (armour/wool/flame batch accumulators, `humanoid_armour_slot`, `transparent_placeholder_atlas`, a `#[cfg(test)]` sky-colour helper). Private items in the root are visible to every descendant, whereas moving a shared struct into a submodule forces `pub(super)` on it and every field a sibling reads.

**`impl RenderState` by seam.** Multiple inherent impls span files. A method only used in its file stays private; one a sibling needs is `pub(super)` (visible to the whole `gpu` subtree).

| file | owns |
|---|---|
| `gpu/state.rs` | `RenderState::new` and the install/setter seam: fog, clear colour, optional passes (sky, screen effects, weather, particle atlas), per-frame "source" setters. Several sources must be **re-installed every frame** (partial-tick interpolated); each setter's doc says which |
| `gpu/sections.rs` | section residency for both terrain paths (upload/remove/resize/animate) and read-only borrows the HUD's 3-D item pass shares |
| `gpu/frame.rs` | the frame graph: public `render*` entry points into `render_inner`. **Submission order is load-bearing** (opaque before translucent) |
| `gpu/world_items.rs` | dropped items, projectile billboards, mob-hand items, through the model pipeline |
| `gpu/entity_passes.rs` | every per-entity layer (entities, armour, wool, flame, block entities) off one resolver and pose input, so a layer never draws off a pose the body pass didn't |
| `gpu/tests.rs`, `gpu/pixel_gates.rs` | hermetic gates and `#[ignore]`d GPU pixel gates |

Per-pass resource modules: `outline.rs` (mining wireframe), `debug_lines.rs`, `stats.rs` (`RenderStats`, F3 counters), `terrain.rs` (packed/demo storage and the shared-camera-uniform arena; [terrain rendering](terrain-rendering.md)), `entities.rs` (mob pipeline, armour, wool, textures), `sources.rs` (polled per-frame sources: entity light, sky darken, time of day, third-person body pose, hand swing, main hand), `first_person.rs`, `nametag.rs` (billboarded nametags, two-pipeline shader), `block_entities.rs` (chest/skull/bell rigs reusing the entity pipeline rather than a fifth bind group), `sign_text.rs` (sign ink only, sharing nametag glyph layout and shader; the board is terrain), `screen_effects.rs` (underwater/fire/pumpkin/spyglass/freeze/portal/confusion overlay input, a per-call argument not a polled source).

**Shaders.** Every pipeline-owning crate keeps shaders under `src/shaders/*.wgsl`, included next to the owning pipeline (`const MODEL_WGSL: &str = include_str!("shaders/model.wgsl");`; a subdirectory module needs `../shaders/...`). `lodestone-render` and `lodestone-shell` each own about a dozen; several are byte-identical across HUD/menu/container/effects sites and deliberately kept as separate consts, since sharing couples currently independent pipelines (a decision for the pass owner, not a cleanup).

Per-crate `wgsl_valid.rs` (no GPU, about 0.02 s) runs every `.wgsl` through naga's WGSL front end and validator, because `cargo check` never compiles a shader and the first reader of the text is `create_shader_module` inside an `#[ignore]`d GPU gate. It also fails on any `@vertex`/`@fragment` under `src/**/*.rs`, stopping inlining. It cannot catch a bind-group or `@location` mismatch with the pipeline; only a real GPU pixel gate can.

## How to change it, and the gotchas

- A new method goes where its caller is. A whole new pass wants its own `gpu/<pass>.rs` plus an arm in `gpu/frame.rs`'s `render_inner` (read that file's submission-order doc first).
- Adding a `RenderState` field touches two files: the struct in `gpu.rs` and the initialiser in `gpu/state.rs::new`.
- Public API is unchanged by the split (`crate::gpu::Foo` via `pub use`); check usages before removing a re-export.
- `ModelRenderer`/`SectionGpu`/`ModelSectionGpu` are built by struct literal in `RenderState::new` in the root, so every field needs `pub(super)`; `EntityRenderer` has its own `::new`, so only fields `RenderState`'s other methods reach need it.
- No file in `gpu/` carries a path-sensitive macro (`include_str!`, `include_bytes!`, `file!`, `#[path]`) except each pipeline's own shader include; keep it so code can move between directory levels.
- Write a new shader as its own `.wgsl`, never inline (`no_wgsl_is_inlined_in_rust_sources` fails on it). Run `cargo test -p <crate> --test wgsl_valid` after adding a file.
- Cross-cutting renderer constraints (the model shader's 4-bind-group floor, reversed-Z, GUI winding sign, gamma-space tint/shade, sRGB swapchain view, `LineList` on HiDPI, borrowed-resource re-attachment, `ALPHA_BLENDING` unpredictability) live once in [architecture](architecture.md)'s "Hard renderer constraints".

## Configuration

None.

## Dependencies

`wgpu`, `lodestone-render`, `lodestone-assets`, `lodestone-model`, `glam`, this crate's `entities`, `mesher`, `particles`, `resources` modules; `wgpu::naga` (re-exported on native, not a direct dependency) for the validity tests.
