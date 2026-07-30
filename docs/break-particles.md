# Break particles

The debris a block throws when it breaks — vanilla's `TerrainParticle`, spawned by
`ClientLevel.addDestroyBlockEffect` (the whole-block burst) and
`addBreakingBlockEffect` (the single fleck per mining hit).

This doc is about the **colour** of that debris: which sprite it samples, which tint it
multiplies by, and the bug where a cascading block's debris came out white.

## What it is

A terrain particle is a small camera-facing billboard textured from a random **quarter**
of its block's `#particle` sprite, tinted by a per-state colour, and shaded by the light
at its cell. Three layers own the parts:

| layer | file | owns |
|---|---|---|
| simulation | `crates/lodestone-particle/src/{lib,emit}.rs` | physics, lifetimes, `uo`/`vo` quarter-window draws, the `0.6` base grey |
| resolution | `crates/lodestone-shell/src/particles.rs` | state id → atlas UV rect, state id → tint, GPU instances, the billboard pass |
| per-state data | `crates/lodestone-render/src/block_models.rs` | `StateModel::particle_uv`, `StateModel::particle_tint`, baked once from `client.jar` |

`lodestone-particle` emits `SpriteSource::BlockState(id)`; it has no atlas and no opinion
about pixels. The shell holds both the engine and the atlas, so the join happens there —
see `particles.rs`'s module docs.

## How it works

### The two emit sites, and why only one of them matters live

```
crates/lodestone-shell/src/sim.rs:2444   Sim::break_block          — DEMO WORLD ONLY
crates/lodestone-shell/src/sim.rs:3034   NetUpdate::BlockDestroyed — the live path
crates/lodestone-shell/src/interact.rs:395  Particles::breaking_block — per mining hit
```

`Sim::break_block` is the offline demo world's direct edit. On a live server the dig is
routed through the server, so the *only* destroy-burst emitter is
`NetUpdate::BlockDestroyed`, fed by `LevelEvent` **2001**
(`PARTICLES_DESTROY_BLOCK`). There is no local prediction of a destroy burst on a live
server, for the punched block or for a cascaded one.

2001's `data` field is a **block-state id** (`Block.getId(blockState)` on the server,
`Block.stateById(data)` in vanilla's `LevelEventHandler`). It is the authoritative signal
that a block broke, because by the time `BLOCK_UPDATE` arrives the cell is air and the
texture the debris needs is gone. Wire layout, big-endian and **fixed width, not
VarInt**: `i32` event, `i64` packed `BlockPos.asLong`, `i32` data, `bool` global.

### Sprite

`particle_uv` is vanilla's `BakedModel.particleIcon()` — the model's `#particle` texture
variable, resolved through the parent chain. It is **not** the texture of any face:
`grass_block` declares `"particle": "block/dirt"`, and `block/template_torch` declares
`"particle": "#torch"`. Frame 0 of an animated sprite is used, matching how a face bakes.

An unresolvable state produces no UV rect and the fragment is **dropped and counted** into
`ParticleFrame::unresolved` (surfaced on the F3 overlay as `D/A+Nunres`). It never draws
as a placeholder — which is why an unresolved sprite cannot be the cause of visible
wrong-coloured debris.

### Tint — the part that was missing

Vanilla's `TerrainParticle` constructor (26.2,
`.cache/mc/26.2/client-src/net/minecraft/client/particle/TerrainParticle.java`):

```java
this.rCol = this.gCol = this.bCol = 0.6F;
BlockTintSource tintSource = Minecraft.getInstance().getBlockColors().getTintSource(blockState, 0);
if (tintSource != null) {
   int col = tintSource.colorAsTerrainParticle(blockState, level, pos);
   this.rCol *= (col >> 16 & 0xFF) / 255.0F;   // and g, b
}
this.quadSize /= 2.0F;
```

`colorAsTerrainParticle` is a **separate virtual method** from the in-world face tint, and
two of vanilla's registrations override it to disagree with the face tint
(`BlockTintSources`):

- **`grass_block`** → `-1` (untinted). It has to be: its `#particle` is `block/dirt`, so
  applying the grass colormap would throw *green dirt*. Older clients spelled this out
  inline as `if (!state.is(Blocks.GRASS_BLOCK))`.
- **`water` / `bubble_column`** → the biome water colour, even though `color` and
  `colorInWorld` are both `-1` (the fluid *surface* is tinted by the fluid model instead).

Everything else inherits `colorAsTerrainParticle` from `colorInWorld`, so it agrees with
the layer-0 face tint. That is why
`lodestone_assets::tint::vanilla_particle_tint_kind` delegates to `vanilla_tint_kind(block,
0, props)` and only special-cases those two, rather than keeping a second copy of the
whole `BlockColors.createDefault()` table.

`BlockModels::build` resolves that kind through `DefaultTints` (the fixed **plains**
colours, sampled from the pack's real colormap PNGs) into `StateModel::particle_tint`.
`Particles::new` copies the whole table into `state_tint`, and
`Particles::state_tint_of` multiplies it into every emitted fragment. **1,592 of 32,366
states** carry a tint on a complete 26.2 pack.

### Shading and the fragment shader

`Particles::extract` maps the quarter-window's sprite-local UVs into absolute atlas UVs,
then multiplies the colour by the terrain shader's own light term, `0.2 + 0.8 *
max(sky, block)`. The fragment shader is `texel * colour`, discarding `a < 0.02`. Both
atlas and colour target are sRGB, so the multiply happens in **linear** space — unlike
block tint and face shade, which vanilla multiplies in gamma space (see `CLAUDE.md`).

## The white-debris bug (fixed), and what was measured

Reported as: *"if I break a block that causes another to break, the particles for the
other block are white."*

### The hypothesis that was wrong

The obvious reading is that 2001's `data` was decoded wrong all along and was masked on
the punched block by a correct-looking local prediction. **It was not.** Two independent
measurements:

- **Real server bytes.** `crates/protocol/v770/tests/live_destroy_block_event.rs` stands a
  torch on a block against the flat creative 26.2 oracle, pulls the support with
  `setblock … air`, and captures the frame vanilla's `Block.updateOrDestroy` →
  `Level.destroyBlock` path emits for the cascade:

  ```
  level_event event=2001 pos=(12,-58,-4) data=3370
  raw = 00 00 07 d1 | 00 00 03 3f ff ff cf c6 | 00 00 0d 2a | 00
  ```

  Hand-decoded big-endian: event `2001`, position `(12,-58,-4)`, data `3370`, global
  `false`. `3370` is `minecraft:torch`'s id in the generated
  `Block.BLOCK_STATE_REGISTRY` census — cross-checked independently against
  `generated/reports/blocks.json`, which also says 3370. The decode is correct and always
  was.
- **There is no local prediction to mask anything.** `Sim::break_block` (`sim.rs:2444`) is
  the *demo world's* direct edit; on a live server both the punched block and the cascaded
  one get their burst from the same 2001 arm. The masking mechanism did not exist.

### The actual mechanism

Both emit sites passed a hardcoded `[1.0; 3]` where vanilla multiplies by the tint source.
The tinted blocks are **exactly** the ones vanilla stores **greyscale** in the atlas —
`grass`, `fern`, the leaves, `redstone_dust_*`, `lily_pad` — because it colours them at
draw time. So the missing multiply did not desaturate their debris slightly; it rendered
the raw grey sprite at `0.6`:

| block | `#particle` texel | debris before | debris after |
|---|---|---|---|
| `redstone_wire[power=0]` | `#fefefe` | **`#bfbfbf`** | `#6f0000` |
| `melon_stem` | grey | **`#808080`** | `#008000` |
| `short_grass` | `#838183` | **`#777777`** | `#5b6748` |
| `oak_leaves` | grey | **`#757575`** | `#516133` |
| `lily_pad` | grey | **`#6a6a6a`** | `#254c2e` |
| `stone`, `dirt`, `torch` | coloured | unchanged | unchanged |

Census over every tinted `(block, tint)` pair: **49 of 52 threw grey debris** before the
fix, **0 after**.

### Why the report reads as "the *cascading* block"

Not because the two code paths differ — they do not. Because of **which blocks cascade**.
The block a player punches is nearly always untinted (stone, dirt, planks, ore), for which
`[1.0; 3]` is the right answer. The block that pops when its support is removed is nearly
always foliage or wiring — grass, fern, sugar cane, vine, lily pad, redstone wire — i.e.
the tinted set. The asymmetry is in the block *population*, not the code path, which is
exactly why "break something and look at the debris" never reproduced it.

### The silent fallback

`[1.0; 3]` was not a fallback that fired on failure — it was a **plausible constant** at
the call site, which is worse: there was no failure to log. Nothing was unresolved, no
counter moved, and `ParticleFrame` reported `64/64+0unres` while the debris was wrong.
The fix is structural rather than diagnostic: the tint is now **derived from the state id
inside `Particles`**, so a new emit site cannot reintroduce the constant by omission, and
`Particles::tinted_state_count()` / `BlockModels::particle_tinted_state_count()` exist so
a gate can prove the table is populated rather than trusting that it is.

## How to change it

- **A block's debris is the wrong colour** → `vanilla_particle_tint_kind` in
  `crates/lodestone-assets/src/tint.rs`. Check the block against
  `BlockColors.createDefault()` in the decompiled client, and check whether its tint source
  overrides `colorAsTerrainParticle`. Do **not** derive the tint from the quads'
  `tint_index`: the `#particle` sprite is a different texture from any face, and
  `grass_block` proves the two lookups genuinely disagree.
- **A block's debris is the wrong texture** → `BakedModel::particle_uv` in
  `crates/lodestone-assets/src/bake.rs` (`#particle` resolution) and the multipart
  first-model-wins rule beside it.
- **Debris draws nothing** → check `ParticleFrame::unresolved` before anything else; an
  unresolved sprite is silent in pixels but loud in that counter.
- **Debris fills the whole cell for a slab or a fence** → known, deliberate gap.
  `Particles::destroy_block` passes `emit::FULL_CUBE` because vanilla derives the fragment
  grid from the block's **outline** shape (not its collision shape — `short_grass` has an
  outline and no collision) and the shell does not carry outline geometry per state yet.
  Vanilla would throw 32 fragments for a slab and 12-ish for a torch; we throw 64.

### Gotchas

- **The tint is the fixed plains default**, not the live biome colour, matching how the
  mesher tints terrain quads. A grass tuft's debris therefore agrees with the tuft beside
  it — which is the property that matters — but not with a swamp.
- **`caller tint × state tint`.** `destroy_block`/`breaking_block` still take a `tint`
  parameter; it is an *extra* multiplier composed on top of the state's own, not a
  replacement. Both live callers pass `[1.0; 3]`.
- **A quarter of the sprite, mirrored.** `Particle::uv_local` returns `u0 > u1` on purpose:
  vanilla's `getU0` is `(uo+1)/4` and `getU1` is `uo/4`. "Fixing" it makes debris disagree
  with vanilla.
- **A mostly-transparent sprite throws mostly-invisible debris.** A torch's 16×16 is ~88%
  transparent, so ~55 of its 64 fragments discard on alpha. That is vanilla behaviour, not
  a bug — do not chase the low visible count.
- **The atlas is mip-mapped with `min_filter: Linear`, and mip levels are `solidify`d
  while level 0 is not.** `scale_alpha_to_coverage`'s `+ 0.025` bias means a fully
  transparent region has alpha `6/255 ≈ 0.024` at mip 1 — just above the shader's `0.02`
  discard threshold, with the nearest opaque colour bled across it. Measured not to matter
  at the distances a break is viewed from (LOD stays at 0 out past 40 blocks for a
  0.1-block billboard), but it is the first thing to suspect if distant debris ever grows
  a bright halo.

## Configuration

None at runtime. Everything is baked from the resource pack at startup:

- `LODESTONE_ASSETS`, or a discovered `.cache/mc/<version>/` holding both `client.jar` and
  `generated/reports/blocks.json` (`crates/lodestone-shell/src/resources.rs`).
- Without a pack, `Sim` builds `Particles::with_demo_palette`, whose table is the demo
  block ids and which is untinted — correct, because the demo palette has no colormaps and
  no tinted blocks.

## Tests

| test | what it pins | needs |
|---|---|---|
| `lodestone-assets` `tests/tint.rs::particle_tint_diverges_from_the_face_tint_exactly_where_vanilla_does` | the two `colorAsTerrainParticle` divergences, plus that every other block agrees with the face tint | nothing |
| `lodestone-shell` `particles::tests::a_states_particle_tint_reaches_the_emitted_fragments` | the wiring: `state_tint_of`'s multiply reaching a fragment's colour, and caller tint composing rather than replacing | nothing |
| `lodestone-shell` `tests/break_particle_tint.rs` | the real vanilla atlas: cascading blocks' debris is tinted, untinted blocks are untouched, and a census of every tinted `(block, tint)` pair | `client.jar` + `blocks.json` |
| `lodestone-v770` `tests/live_destroy_block_event.rs` | 2001's `data` really is the cascaded block's state id, hand-decoded from captured server bytes | flat creative oracle (`:25570`, RCON `:25571`) |
| `lodestone-shell` `tests/break_particles_pixels.rs` | that debris reaches the framebuffer at all | GPU adapter |

`break_particle_tint.rs` derives its **control** by substituting the state's tint back out
of the extracted instance colour, so subject and control come from one burst and differ
only in the multiplier under test. The control is asserted to be *grey* on every run — a
control that merely *would* fire is not evidence. Note it divides the tint out of a single
channel, not per channel: `redstone_wire[power=0]`'s tint is `[0.3, 0.0, 0.0]`, and a
per-channel division reconstructs a red control instead of the grey one the pre-fix code
actually produced.

**`break_particles_pixels.rs` could not have caught this**, and that is the interesting
part. It is an exemplary pixel gate with a tight paired control — and it renders the
**demo palette**, which has no colormaps and no tinted blocks, so it structurally cannot
exercise a tint. That is the *world* species of vacuous test from `CLAUDE.md`: the flaw is
in the input data, not in the test source, and no amount of reading the test reveals it.
`break_particle_tint.rs` exists to be pointed at the real atlas instead.

## Dependencies

- `lodestone-particle` — `ParticleEngine`, `emit::{destroy_block_effect,
  breaking_block_effect}`, `SpriteSource`, `Behaviour::Terrain`.
- `lodestone-assets` — `bake::BakedModel::particle_uv`, `tint::{vanilla_tint_kind,
  vanilla_particle_tint_kind, TintKind}`, `AtlasBuilder`, `ParticleAtlas`.
- `lodestone-render` — `BlockModels` (`particle_uv`, `particle_tint`, the stitched atlas
  the pass samples), `Camera`, `GpuAtlas`.
- `lodestone-v770` — `packets::game::LevelEvent`, `block_states` (the state-id census).

## Known open gap: sheet particles sample the wrong atlas

Not the white-debris bug, found beside it, and **not fixed here** because the fix is in a
file another agent holds.

`Particles::sheet_uv` resolves `SpriteSource::Sheet` (flame, smoke, crits, splashes)
against `ParticleAtlas` — a **separate stitch** from the block-model atlas, with its own
dimensions and packing. But `crates/lodestone-shell/src/gpu.rs` builds exactly one
`particle_atlas_bind_group`, from the *block model* atlas, and `ParticleRenderer::draw`
binds only that. So a resolved sheet particle samples block-atlas texels at
particle-atlas coordinates: `/particle minecraft:flame` draws fragments of arbitrary block
textures.

It is invisible to `tests/live_particles.rs`, which asserts `unresolved == 0` — and the
UVs *do* resolve. Fixing it needs a second bind group (`ParticleRenderer` already has the
`tex_layout` to build one) plus a per-particle choice of which to bind, i.e. splitting the
draw into a block-atlas pass and a sheet-atlas pass.
