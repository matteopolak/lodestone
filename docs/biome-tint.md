# Real per-position biome tint (grass, foliage, water)

Issue-adjacent to #171/#174 (biome tint). See [`worldgen-biomes.md`](./worldgen-biomes.md) for the
data half (biome assignment, per-quart climate) this builds on.

## What it is

Grass, foliage, dry-foliage and water quads used to render one fixed **plains-default** colour
everywhere — real per-biome variety existed in the world data (climate table, per-biome
`water_color`/`grass_color`/`foliage_color`/`grass_color_modifier`), but nothing consumed it at
render time. `lodestone_assets::tint::BiomeTint` had zero implementors outside a test mock. This
closes that gap: the live mesher now resolves each tinted quad's **real, position-blended** colour,
matching vanilla's own `ClientLevel.calculateBlockTint` box-blend.

## How it works

**The data** (`crates/lodestone-assets/src/tint.rs`):

- `BiomeEffects` bundles everything one biome needs: `temperature`, `downfall`, `water_color`, the
  three optional colormap overrides (`grass_color`/`foliage_color`/`dry_foliage_color`), and
  `grass_modifier`. `BIOME_EFFECTS` is a 66-entry static table, one row per vanilla biome, values
  transcribed directly from `.cache/mc/26.2/src/data/minecraft/worldgen/biome/*.json` — the same
  jar files `docs/worldgen-biomes.md`'s "66/66" gate already checks. `biome_effects(id)` looks a
  biome up by name (bare path or `minecraft:`-prefixed, both accepted).
- `blend_box(x, z, radius, sample)` is vanilla's box-average kernel, ported line-for-line from
  `ClientLevel.calculateBlockTint` (`.cache/mc/26.2/client-src/net/minecraft/client/multiplayer/
  ClientLevel.java:1012-1034`): a `(2*radius+1)²` average of the *resolved* colour (not of
  temperature/downfall — the average happens **after** colormap sampling and the grass modifier,
  exactly like vanilla's split between `Biome.getGrassColor` (one point) and `calculateBlockTint`
  (the box around it)), with vanilla's own per-channel **integer** (floor) division. Default radius
  is `DEFAULT_BLEND_RADIUS = 2` — vanilla's `Options.java:472` default `biomeBlendRadius`, a 5×5 = 25
  sample average. This client has no biome-blend-radius setting, so `2` is not a guess, it is the
  only value reachable.

**The glue** (`crates/lodestone-render/src/biome_tint.rs`):

- `NamedBiomeTint<F>` implements `lodestone_assets::tint::BiomeTint` over any `F: Fn(BlockPos) ->
  Option<&'static str>` (a biome-name lookup), falling back to a hardcoded plains `BiomeEffects` for
  an unresolved position — matching the pre-existing default look exactly rather than an arbitrary
  colour.
- `resolve_blended_tint(kind, colormaps, biome, radius, x, y, z)` wraps `Colormaps::resolve` in
  `blend_box`, returning `None` for `TintKind::None`/`Constant`/`RedstonePower` (not
  position-dependent, nothing to blend) and `Some(rgb)` for `Grass`/`Foliage`/`DryFoliage`/`Water`.

**The mesher plumbing** (`crates/lodestone-render/src/{models.rs,block_models.rs}` +
`shaders/{model,fluid}.wgsl`):

- `ModelVertex` grew a fifth field, `tint_rgb_override: [u8; 4]` (rgb + an override flag in `.w`) —
  **additive**, not a replacement for the existing `tint` palette-index byte. The frame-shared
  palette (`model_pipeline.rs`'s group 2) can only hold *one* colour per slot at a time, so it
  structurally cannot hold "grass in a desert" and "grass in a swamp" simultaneously; the real
  colour has to travel on the vertex instead, computed once per quad at mesh time.
- `block_models.rs` reserves four fixed palette slots (`GRASS_TINT_SLOT`/`FOLIAGE_TINT_SLOT`/
  `DRY_FOLIAGE_TINT_SLOT`/`WATER_TINT_SLOT`, just below the `UNTINTED` sentinel) for the four
  biome-dependent `TintKind`s, pre-filled with the plains default exactly as before. A quad's
  `tint_index` still names one of these; `biome_tint_kind_for_slot` is the reverse lookup a live
  view needs to know *which* kind to resolve.
- `ModelSectionView::biome_tint_at(x, y, z, slot)` / `FluidSectionView::water_tint_at(x, y, z)` are
  new **default-`None`** trait methods `mesh_models`/`mesh_fluids` call per quad. Default `None`
  means every existing view (GUI items, headless tests, a view with no biome grid) renders exactly
  as before — this is fully additive, not a breaking change to either trait.
- `model.wgsl`/`fluid.wgsl` read the new attribute: `tint_rgb_override.a != 0` uses `.rgb` directly
  (already gamma-space bytes, matching the existing palette-multiply convention — see "Gamma space"
  below); otherwise both fall back to the palette lookup / hardcoded water constant exactly as
  before.
- **Entity pipeline collision, found and fixed.** `ModelVertex::vertex_layout()` is also what
  `entity_pipeline.rs` builds its own instance-buffer attributes on top of, starting at location 4
  (documented in that file: "Instance attributes start at location 4, past `ModelVertex`'s 0..=3").
  Growing the *existing* `vertex_layout()` to expose the new field at location 4 collided with that
  — measured directly as a `wgpu` validation panic ("Two or more vertex attributes were assigned to
  the same location in the shader: 4") on every entity pixel gate. Fixed by keeping
  `vertex_layout()` unchanged (four attributes, locations 0..=3, entity pipeline's contract intact)
  and adding a **separate** `vertex_layout_with_biome_tint()` (five attributes) that only the
  model/fluid pipelines use.

**The live wiring** (`crates/lodestone-shell/src/mesher.rs`):

- `SnapshotModelView`/`SnapshotFluidView` (the real views `MeshScheduler` meshes through) implement
  the two new trait methods: resolve the biome name at a snapshot-relative position via
  `ChunkSection::biome_at_block` + `FALLBACK_BIOME_NAMES`, wrap it in a `NamedBiomeTint`, and call
  `resolve_blended_tint`.
- `FALLBACK_BIOME_NAMES` is a **known, provisional** id→name table (see "Gotchas" below) — the
  correct source is per-connection registry data, which nothing yet threads from `NetClient` into
  the mesher's worker threads.

## How to change it, and gotchas

- **The id→name mapping is provisional.** `FALLBACK_BIOME_NAMES` mirrors
  `crates/protocol/v770/src/server_protocol.rs`'s `BIOME_NAMES` (alphabetical over the 55 biomes
  the embedded overworld generator can select — nether/end aren't servable yet, see
  `worldgen-biomes.md`). This is **exactly right against this codebase's own server** (the only
  server v770 can host, and the default `cargo run --release` path), because both sides derive the
  same alphabetical order from the same fixed set — but it would very likely be wrong against a
  third-party vanilla server, whose real registry-sync order this client already decodes correctly
  (`crates/protocol/v770/src/packets/registry.rs`'s `ClientRegistries::entry_names(BIOME)`) but does
  not yet thread anywhere past `net.rs`. Swapping `FALLBACK_BIOME_NAMES` for a live
  `ClientRegistries`-backed lookup is real, separately-scoped follow-up work (needs a `NetClient`
  accessor plus threading an `Arc` through `MeshScheduler`'s worker-thread jobs, which currently
  touch no live client state by design).
- **The swamp/mangrove-swamp noise term is unported.** `GrassColorModifier::Swamp` picks between two
  constants based on `Biome.BIOME_INFO_NOISE` (a Perlin sampler); `BiomeTint::grass_modifier_noise`
  stays at its trait default `0.0` (always the `>= -0.1` branch), so those two biomes render a
  uniform colour rather than vanilla's mottled one. 64 of 66 biomes are unaffected — see
  `crates/lodestone-render/src/biome_tint.rs`'s module docs.
- **Only `mesh_models`/`mesh_fluids` are wired, not `mesh_simple`.** Grass and water were never
  packed-cube candidates in the first place (tinted, so the D1 split in `models.rs` already routes
  them through the wide path), so this doesn't need to touch the packed/greedy mesher at all.
- **The palette's four reserved slots must never be `intern()`-ed over.** `TintPalette::intern`
  caps its auto-incrementing index at `RESERVED_SLOTS_START - 1` for exactly this reason — see
  `block_models.rs`'s `TintPalette` doc if that constant ever needs to move.
- **Gamma space**: tint multiplies happen in **gamma** (sRGB byte) space in both shaders, matching
  vanilla — verified by `tint_gamma_gate.rs` (G/R ≈ 1.30 for a grass-tinted quad, not the ~1.13 a
  linear-space regression would produce). The new vertex-carried colour follows the same convention
  (straight sRGB bytes, no linearisation) so it drops into the existing gamma round-trip unchanged.

## Configuration

No env vars or flags. `DEFAULT_BLEND_RADIUS` (`lodestone_assets::tint`) is the only tunable, and
nothing currently overrides it.

## Dependencies

- `crates/lodestone-assets` — `tint.rs`'s `BiomeEffects`/`BIOME_EFFECTS`/`biome_effects`/`blend_box`.
- `crates/lodestone-render` — `biome_tint.rs` (the `BiomeTint` glue), `models.rs` (vertex + trait
  plumbing), `block_models.rs` (reserved palette slots), `model_pipeline.rs` (the two vertex-layout
  methods), `shaders/{model,fluid}.wgsl`.
- `crates/lodestone-shell` — `mesher.rs`'s `SnapshotModelView`/`SnapshotFluidView` (the live wiring),
  `ChunkSection::biome_at_block` (`lodestone-world`, pre-existing).

## Verification

- `crates/lodestone-assets/tests/tint.rs` — `blend_box`/`biome_effects` unit tests, including an
  exact hand-computed box-average and every value checked against the jar.
- `crates/lodestone-render/src/biome_tint.rs`'s own `#[cfg(test)]` module — `NamedBiomeTint`
  correctness and a real cross-boundary blend (plains/swamp) that lands strictly between the two
  pure colours.
- `crates/lodestone-render/tests/biome_tint_gate.rs` — hermetic proof that `mesh_models`/
  `mesh_fluids` consume the new trait methods, with a location-keyed assertion (not a frame
  average) and a fired negative control.
- `crates/lodestone-shell/tests/biome_tint_live_mesh.rs` — `#[ignore]`d, needs `client.jar`: drives
  the **real** `mesh_snapshot_models` over a real `BlockModels` and a world with two real biomes,
  and gets exactly `[0x6A, 0x70, 0x39]` for the swamp side (predicted from the jar source
  independently of this code) and a distinct, real colour for the desert side.
