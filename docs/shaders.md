# Shaders

## What it is

Every WGSL shader in the client lives in its own `.wgsl` file under
`crates/<crate>/src/shaders/`, pulled into the binary at compile time with
`include_str!`. There is no runtime file loading, no asset path and nothing to ship
alongside the executable — the shader text is still a `&'static str` baked into the
binary, sourced from a file instead of a Rust string literal.

Two crates own shaders: `lodestone-render` (11 — terrain, entities, sky, screen
effects) and `lodestone-shell` (11 — HUD, menus, containers, particles, debug
overlays).

## Where they are

`crates/lodestone-render/src/shaders/`

| file | const / call site | what it draws |
|---|---|---|
| `triangle.wgsl` | `frame.rs` `TRIANGLE_WGSL` | the smoke-test triangle |
| `block.wgsl` | `block.rs` `BLOCK_WGSL` | the single-block preview pipeline |
| `model.wgsl` | `model_pipeline.rs` `MODEL_WGSL` | terrain and block models — cutout discard, tint, shade |
| `fluid.wgsl` | `model_pipeline.rs` `FLUID_WGSL` | water and lava — smooth alpha, no cutout discard, water tint |
| `entity.wgsl` | `entity_pipeline.rs` `ENTITY_WGSL` | entities, held/dropped items, the first-person arm, the hurt overlay |
| `overlay.wgsl` | `screen_effects.rs` `OVERLAY_WGSL` | full-screen overlays (underwater, fire) |
| `crack.wgsl` | `crack_pipeline.rs` `CRACK_WGSL` | block-breaking crack decals |
| `sky_disc.wgsl` | `sky_pipeline.rs` | the sky disc's per-fragment horizon-to-zenith gradient |
| `sky_celestial.wgsl` | `sky_pipeline.rs` | sun/moon billboards, alpha-tested |
| `sky_cloud.wgsl` | `sky_pipeline.rs` | the cloud plane, alpha-tested and tinted |
| `sky_passthrough_color.wgsl` | `sky_pipeline.rs` `PASSTHROUGH_COLOR_WGSL` | **two** pipelines: the star field and the sunrise/sunset band. Position plus a per-frame baked vertex colour; the passes differ only in blend state |

`crates/lodestone-shell/src/shaders/`

| file | const / call site | what it draws |
|---|---|---|
| `hud.wgsl` | `hud.rs` `HUD_WGSL` | untextured HUD geometry |
| `hud_sprite.wgsl` | `hud.rs` `HUD_SPRITE_WGSL` | textured HUD sprites — also read by `hud/item_icon.rs` |
| `menu.wgsl` | `menu/render.rs` `MENU_WGSL` | menu panels and buttons |
| `menu_sprite.wgsl` | `menu/render.rs` `MENU_SPRITE_WGSL` | menu text and sprites |
| `container.wgsl` | `container.rs` `CONTAINER_WGSL` | container slots, items, drag preview |
| `container_bg.wgsl` | `container.rs` `CONTAINER_BG_WGSL` | the container background |
| `particles.wgsl` | `particles.rs` `SHADER` | billboard particles |
| `effects.wgsl` | `effects.rs` `EFFECTS_WGSL` | shell-side screen effects |
| `outline.wgsl` | `gpu/outline.rs` (inline `include_str!`) | the block-selection outline |
| `nametag.wgsl` | `gpu/nametag.rs` (inline `include_str!`) | entity and player nametags |
| `debug_lines.wgsl` | `gpu/debug_lines.rs` (inline `include_str!`) | debug line overlays |

### Duplicates, deliberately not merged

22 files, **17 distinct bodies**. Three groups are byte-identical:

- `hud.wgsl` = `menu.wgsl` = `container.wgsl` = `effects.wgsl`
- `menu_sprite.wgsl` = `container_bg.wgsl`
- `nametag.wgsl` = `debug_lines.wgsl`

They were duplicated as Rust consts before the extraction and stayed duplicated after
it, because the move was mechanical and behaviour-neutral. Merging them is a real
option but it is a **coupling decision**, not a tidy-up: one file shared by the HUD,
the menus, containers and screen effects means an edit for one of them silently
changes the other three. Whoever owns those pipelines should make that call. Until
then, one const, one file.

## How it works

Nothing clever. The const keeps its name and its type; only its value changes:

```rust
// crates/lodestone-render/src/model_pipeline.rs
const MODEL_WGSL: &str = include_str!("shaders/model.wgsl");
```

`include_str!` resolves relative to **the file containing the macro**, so a module in
a subdirectory needs one `..`:

```rust
// crates/lodestone-shell/src/gpu/outline.rs
source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/outline.wgsl").into()),
```

Because it is a compile-time include, the `.wgsl` is a build input: editing one
rebuilds the crate, and a wrong path is a compile error rather than a runtime one.

## The gotcha this deletes: a double quote

Until this change the shaders lived in Rust `r"…"` raw strings. A single `"` anywhere
inside one — **including in a comment** — terminated the Rust string early, after
which rustc parsed the remaining WGSL and the surrounding English prose as Rust. The
error messages looked nothing like the cause:

```
error: prefix `yet` is unknown
```

…pointing at a word in a shader comment. It happened **four times**, twice inside
comments that were themselves warning about the trap, and `CLAUDE.md` carried a
standing rule you had to remember every time you typed a comment.

**In a `.wgsl` file the trap is gone, not relocated.** This was measured rather than
assumed, and the measurement corrected a plausible wrong claim:

| where the `"` is | old (Rust literal) | now (`.wgsl` file) |
|---|---|---|
| inside a `//` comment | breaks the **Rust** build, error points at English | **legal and inert** — WGSL's lexer skips the comment |
| in code position | same broken Rust build | WGSL parse error: `expected expression, found "\""`, at the shader's own line |

So `wgsl_valid` does *not* flag a quote in a comment, and should not: there is no
enclosing literal left to terminate. Write shader comments normally. (Verified by
putting a `"` in `sky_disc.wgsl`'s comment and watching the suite stay green, then
putting the same `"` in code position and watching it fail.)

Editors also syntax-highlight a `.wgsl` file, which they never did inside a Rust
string.

## The `wgsl_valid` test

`crates/lodestone-render/tests/wgsl_valid.rs` and
`crates/lodestone-shell/tests/wgsl_valid.rs`. Ordinary tests — **not** `#[ignore]`d,
no GPU, no adapter — so they run in `cargo test --workspace`, in about 0.02s.

Each walks its crate's `src/shaders/` at test time and runs every `.wgsl` through
naga's WGSL front end (`wgpu::naga::front::wgsl::parse_str`) then
`naga::valid::Validator::validate`. That is the same front end wgpu itself runs inside
`Device::create_shader_module`, so this is the real check minus the adapter.

**Why it was needed.** `cargo check --workspace --all-targets` compiles the Rust that
*embeds* a shader, never the shader. The first thing that reads the WGSL is
`create_shader_module`, which only runs inside the `#[ignore]`d GPU gates. A WGSL
syntax or type error could therefore reach `main` with all three required
`cargo check`s green — the same shape as the doctest gap `CLAUDE.md` records.

**No new dependency.** `wgpu` re-exports naga as `wgpu::naga` on native targets
(`wgpu/src/lib.rs`: `#[cfg(wgpu_core)] pub use ::wgc::naga;`, and `wgpu-core` depends
on `naga` unconditionally). Nothing was added to any `Cargo.toml`. Note `naga` is
*not* a direct dependency of either crate and does not need to be.

**What it proves and what it does not.** It proves each shader parses and type-checks
*in isolation*. It cannot prove a pipeline will build: bind-group indices matching the
Rust-side layouts, `@location` numbers matching the vertex buffer layout, and entry
point names matching `VertexState::entry_point` are cross-module facts that only
pipeline creation checks. The GPU pixel gates remain the end-to-end instrument.

Four tests per crate, and three exist to stop the first being vacuous:

- `every_shader_file_parses_and_validates` — the subject. Asserts a **floor** on the
  file count (`MIN_SHADERS`), so a wrong directory path cannot pass by finding zero
  files — the *precondition* species of vacuous test.
- `the_parser_rejects_malformed_wgsl` — control for the parse stage.
- `the_validator_rejects_an_invalid_module` — control for the validation stage, using
  a module that parses cleanly but is structurally invalid (a vertex entry point with
  no `@builtin(position)` output). Needed because naga resolves types during parsing,
  so an ordinary type error never reaches the validator and would not prove it runs.
- `no_wgsl_is_inlined_in_rust_sources` — walks the crate's `src/**/*.rs` and fails if
  any contains `@vertex` or `@fragment`. This is what stops the old trap returning one
  shader at a time.

Both stages were also confirmed against a **real shipped shader**, not only the
synthetic controls: a stray quote injected into `sky_disc.wgsl`'s code produced
`sky_disc.wgsl: WGSL parse error`, and changing its `fs_main` return from `vec4` to
`vec3` produced `sky_disc.wgsl: WGSL validation error`. Both named the offending file.

## How to add a shader

1. Write `crates/<crate>/src/shaders/<name>.wgsl`, `snake_case`, named after the
   pipeline that owns it and matching the const you are about to declare.
2. `const MY_WGSL: &str = include_str!("shaders/<name>.wgsl");` next to that pipeline
   — `"../shaders/<name>.wgsl"` if the module sits in a subdirectory.
3. `cargo test -p <crate> --test wgsl_valid` — parses and validates the new file with
   no GPU, and is the fastest way to find a typo.
4. Then build the pipeline and run its GPU gate: `wgsl_valid` cannot see a bind-group
   or `@location` mismatch.

Do **not** inline a shader into Rust, including "just for a quick test" —
`no_wgsl_is_inlined_in_rust_sources` will fail. Do not raise `MIN_SHADERS` to silence a
failure; it is a floor, so adding shaders never requires touching it.

## Configuration

None. No env vars, no features, no build script.

## Dependencies

- `wgpu` (workspace, v30) — `ShaderSource::Wgsl`, and `wgpu::naga` for the test.
- Nothing else; the extraction added no dependency to any manifest.

## History and one thing left alone

Extracted from 22 inline Rust string literals across 15 files. The extraction was
mechanical and byte-exact **by construction and proved by reconstruction**: each
literal body was sliced out of `git show HEAD:<file>` and written to the `.wgsl`
unmodified, then every touched `.rs` was rebuilt by substituting the file's bytes back
into its `include_str!` call site and compared against `git show HEAD:<file>`. All 15
reconstructions were byte-identical — 43,337 bytes of WGSL round-tripped, nothing
retyped, reindented or reflowed. 21 of the 22 were `r"…"` raw strings; `CRACK_WGSL`
was a plain `"…"` literal that happened to contain no escape sequence, so the same
slice is exact. No shader was assembled by concatenation or `format!`; every one was a
single literal.

**Test-local shaders were deliberately left inline** — `scene_gpu.rs`,
`sky_pipeline_gpu.rs`, `sky_gradient_pixels.rs` and `gpu.rs` under
`crates/lodestone-render/tests/`. Several are near-copies of a production shader used
as a *control*: `sky_gradient_pixels.rs`'s `PER_VERTEX_WGSL` computes the per-vertex
gradient that `sky_disc.wgsl` deliberately replaced, and
`live_entity_light_time_of_day.rs` documents its shader as kept character-for-character
equivalent to the real one. Sharing a file between a subject and its control would
destroy exactly the independence those gates depend on. They keep the double-quote
hazard, which is the price of that independence.
