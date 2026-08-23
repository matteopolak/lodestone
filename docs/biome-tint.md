# Real per-position biome tint (grass, foliage, water)

See [`worldgen-biomes.md`](./worldgen-biomes.md) for the
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
  biome up by name (bare path or `minecraft:`-prefixed, both accepted) via `FIRST_BYTE_INDEX`, a
  compile-time `const fn` bucketing of the table by first byte — **3.79 string compares on an
  average hit instead of 33.5, and usually one or none on a miss.** The table's alphabetical order
  is therefore load-bearing now (it makes equal first bytes contiguous) and
  `biome_effects_table_is_strictly_ascending` enforces it. A `binary_search_by` was tried instead
  and measured **worse** — see "How to change it".
- `BlendRowCursor` is `blend_box` evaluated incrementally along a row of constant `z`: adjacent
  cells' radius-2 boxes share 20 of their 25 columns, so a sliding per-channel sum of *column*
  sums gives the **bit-identical** colour for 5 new samples per step instead of 25. Bit-identical
  and not merely close because `u32` addition is associative, the largest possible channel total
  (`15² × 255 = 57,375`) is nowhere near overflow, and vanilla's floor division still happens
  exactly once at the end. A `z` change or an `x` jump of the window width or more rebuilds from
  scratch, i.e. costs exactly `blend_box`, so it is never the slower choice.
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
  **It is still here and still the reference implementation** — `BlendedTintCursor` below is proved
  byte-identical against it at run time, so deleting it would remove the only outside expectation
  that gate has.
- `BlendedTintCursor` is what the meshers actually call. It is `resolve_blended_tint` over a
  `BlendRowCursor`, plus the part the row cursor cannot know: a blend also depends on the
  `TintKind` and on the `y` every sample is taken at, so the cursor keys its window on `(kind, y)`
  as well and invalidates on a change. A mismatch costs one full rebuild — exactly
  `resolve_blended_tint` — so a caller with a hostile access pattern pays nothing but the key
  comparison. **It caches sampled colours, so it is only correct while the world it samples is
  unchanging**; that holds inside one `mesh_fluids`/`mesh_models` call over an immutable
  `SectionSnapshot`, and it is why one lives in a `RefCell` on the view rather than anywhere
  longer-lived. Holding a `RefCell` makes those views `!Sync`, which is sound because the mesh
  worker pool parallelises over *sections*, one view each.
- `NamedBiomeTint`'s four-entry name→effects memo is still there and still measures as a
  win, but it is no longer load-bearing for the table scan itself. Note it deliberately does **not**
  memoise an unresolvable name, so a section whose biome id is past the registry reaches the table
  on every sample rather than once — the arm the first-byte index helped most (3.2× on the isolated
  blend).

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
  `ChunkSection::biome_at_block` + `biome_name_at`, wrap it in a `NamedBiomeTint`, and call
  `resolve_blended_tint`.
- `biome_name_at` prefers a **live** id→name table when one is known, falling back to
  `FALLBACK_BIOME_NAMES` only when it is empty (no connection yet, an offline/demo world, or a
  version/server that sends no biome registry) — see the next bullet for how the live table gets
  there, and "Gotchas" below for what is still provisional.

## How to change it, and gotchas

- **This is a hot path, and the two things that made it cheap are both counter-intuitive. Read
  `DESIGN.md` §12.128 before touching either.**
  - **A `binary_search_by` over the sorted table is slower than the linear scan it replaces**, and
    the reason generalises to any string table: `find`'s `*name == path` is `str::eq`, which
    compares **lengths first** and only reaches `memcmp` when they match — 8.6 instructions per
    entry here, because most entries differ from the probe in length. An `Ordering` comparator has
    no such shortcut, so each of its ~7 probes is a real `memcmp` call. Measured: 58 → **309**
    instructions per call for the table's first entry, and `mesh_fluids` regressed 6,629 → 6,815
    instructions per fluid cell. **Seven expensive compares beat thirty-three cheap ones only if
    you never price them.** The fix that worked was doing fewer of the *cheap* compares.
  - **The sliding blend box must stay bit-exact, and "close" is a defect.** Vanilla is not
    colour-managed: tint and shade multiply in gamma space, so a blend that reassociates its sum in
    floating point, or divides per sample instead of once at the end, shifts colours by a byte or
    two — invisible in a screenshot and wrong. `BlendRowCursor`'s exactness rests on integer
    associativity and on the division staying where vanilla puts it. If you change the accumulator
    type or the division point, the identity gates will tell you, and they are the only thing that
    will.
- **A stale or absent biome registry cannot render terrain *untinted* — it can only ever render
  it as plains, and telling those two apart is what the instrument below is for.** The ordering
  that invites the mistake is real: `Sim::refresh_mesh_policy` publishes `TerrainMesh::biome_names`
  at the *top* of `poll_net`, before that same poll drains its own updates, so a section meshed in
  the poll carrying the server's `registry_data` is tinted against the registry as it stood a
  moment earlier. But every unresolvable path — an empty table, a table too short for the id, a
  name absent from `BIOME_EFFECTS` — lands on `biome_tint.rs`'s `PLAINS_FALLBACK`, whose climate
  is plains' own, which resolves to the exact colour `BlockModels::build` interned at
  `GRASS_TINT_SLOT`. Measured both ways: hermetically by
  `an_unresolvable_biome_id_renders_the_plains_default_and_keeps_the_grass_palette_slot`, and end
  to end against the live creative oracle, where forcing `SnapshotModelView::biome_tint_at` to
  return `None` for **every** quad produced a **byte-identical** screenshot capture (0 pixels
  changed) while forcing it to return white changed exactly the 24,821 ground pixels to neutral
  grey. **So neutral-grey ground is never a registry story** — it needs the tint to be skipped
  (`tint_rgb_override` absent *and* the vertex's palette slot at 255), which is a different
  mechanism.
- **The two failures look identical on a plains world, so a screenshot cannot separate them.**
  `mesher.rs`'s `TintProbe` buckets every `biome_tint_at` call — resolved, unresolved, colormaps
  absent, not a blended kind, untinted quad — and `mesher::biome_tint_counts()` exposes the
  running `(resolved, skipped)` totals for the process. The first skip logs a `tracing::warn!` on
  target `mesh`; setting `LODESTONE_TINT_PROBE` additionally prints one line per meshed section to
  stderr (`path=`, `names=` and the whole histogram), which is how you read this in a harness that
  installs no `tracing` subscriber. Use it before theorising about a tint that looks wrong: it
  answers *"did this section tint at all, and against how many biome names"*, which is the
  question, and it is not derivable from the pixels.
- **Adding a biome means keeping `BIOME_EFFECTS` sorted.** `FIRST_BYTE_INDEX` assumes entries with
  the same first byte are contiguous; an entry in the wrong place resolves to `None` and renders the
  plains fallback, with no compile error. Sort with `LC_ALL=C sort` — `_` is `0x5F`, *below* every
  lowercase letter, so "alphabetical ignoring underscores" is the wrong order.
- **The id→name mapping now threads a live registry, closing the gap this section used to
  describe as open.** `crates/protocol/v770/src/packets/registry.rs`'s
  `ClientRegistries::entry_names(BIOME)` already decoded a real server's registry-sync order
  correctly; the missing piece was carrying it past that crate. The path, end to end:
  `V770Adapter::handle_play_chunk`'s `LOGIN` handling (`crates/protocol/v770/src/adapter/chunk.rs`) now emits a third biome event,
  `ClientEvent::BiomeRegistryNames { names }`, alongside `BiomeVisuals`/`BiomeClimates` →
  `crates/lodestone-shell/src/net.rs`'s `forward` folds it into a new `BiomeNameCell` (same
  "whole table replaces at once, never queued" shape as `BiomeClimateCell`), exposed as
  `NetClient::shared_biome_names()` → `Sim::refresh_mesh_policy` reads a snapshot every tick into
  `TerrainMesh::biome_names` (an `Arc<[&'static str]>`, mirroring how `MeshPolicy::sky_default`
  already crosses this exact boundary) → `TerrainMesh::mesh_column`/`mesh_section` attach it to
  the `SectionSnapshot` via `SnapshotOutcome::with_biome_names`/`SectionSnapshot::with_biome_names`
  → `biome_name_at` reads it off the snapshot it was handed, inside the mesh worker thread.
  **Baked into the snapshot itself, not threaded into `MeshScheduler`'s worker closures** — a
  worker thread only ever sees the jobs on its channel, never a live `Sim`/`NetClient`, so the
  per-connection value has to be captured at snapshot time, exactly like `sky_default` already is.
- **The `&'static str` bound forced a deliberate, bounded leak.** `NamedBiomeTint<F>` requires
  `F: Fn(BlockPos) -> Option<&'static str>` (`crates/lodestone-render/src/biome_tint.rs`, out of
  scope to relax for this change), but names arrive as owned `String`s off the wire. `BiomeNameCell`
  leak-interns each one once (`Box::leak`) on the rare `Login`-time fold; a session that reconnects
  many times leaks at most a few KB total, which is the trade documented on `BiomeNameCell` itself.
- **`FALLBACK_BIOME_NAMES` is now a true fallback, not the only path — and it is still provisional
  on its own terms.** It mirrors `crates/protocol/v770/src/server_protocol.rs`'s `BIOME_NAMES`
  (alphabetical over the 55 biomes the embedded overworld generator can select — nether/end aren't
  servable yet, see `worldgen-biomes.md`). That table is **unchanged by this work** — this fix is
  entirely client-side (id→name *resolution*), not server-side (id→name *assignment*), and touching
  `server_protocol.rs` was out of this batch's scope. The fallback stays exactly right against this
  codebase's own server (the only server v770 can host) and is now only reached when no live
  registry has arrived — never against a connection where one has.
- **The gate that actually proves this: `tests/biome_tint_live_mesh.rs`'s
  `live_mesh_snapshot_models_resolves_biome_names_from_the_live_registry_not_the_fallback_table`.**
  A fixture built from `FALLBACK_BIOME_NAMES`'s own order cannot distinguish "the live table is
  consulted" from "the fallback silently won and happened to agree" — it would be vacuous by
  construction. This gate's fixture registry order **deliberately disagrees** with
  `FALLBACK_BIOME_NAMES` at both tested biome ids (each names the *other* one), so a regression
  back to fallback-only resolution fails it, with a fired negative control proving the same world
  gives the opposite answer through the empty/fallback path.
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
  exact hand-computed box-average and every value checked against the jar. Plus `BlendRowCursor`
  against `blend_box` at every radius vanilla exposes, walked forward, backward, revisited in place
  and jumped by exactly `width - 1`/`width`/`width + 1`; an exact predicted sample count (a 16-cell
  row costs **100** samples against `blend_box`'s 400); and a fired control proving the hashed
  fixture field distinguishes a one-column shift in 38 of 40 positions, so the identity assertions
  are not satisfiable by any window arithmetic.
- `crates/lodestone-assets/src/tint.rs`'s own `#[cfg(test)]` module — the private invariants the
  public surface cannot see: `BIOME_EFFECTS` strictly ascending (with a control proving the
  ascending-check fires on both a swap and a duplicate), `FIRST_BYTE_INDEX` covering all 66 entries
  and only matching ones, and the compare-count arithmetic behind the 8.8× narrowing.
- `crates/lodestone-render/tests/biome_tint_row_identity_gate.rs` — `BlendedTintCursor` byte-identical
  to `resolve_blended_tint` over ~3,800 positions of a **four-way biome junction** and a 256×256
  gradient colormap, with the kind rotating per cell so the invalidation path is exercised; then the
  same comparison through the real `mesh_fluids`/`mesh_models` loops as FNV-1a digests of the
  `bytemuck` byte images. Three fired controls: a mis-keyed cursor (built from the same public
  `BlendRowCursor`, keyed on `(x, z)` only) must be rejected by the sweep; shifting the junction one
  column must move both mesh digests; and the junction blend must differ from all four pure sides.
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
  `live_mesh_snapshot_models_resolves_biome_names_from_the_live_registry_not_the_fallback_table`
  (same file) is the live-registry-threading gate: a deliberately permuted fixture registry order
  proves the two ids resolve to the *opposite* biome from what `FALLBACK_BIOME_NAMES` would give,
  with a fired control confirming the fallback path alone still gives the old (wrong-if-real-server)
  answer on the identical snapshot.
- `crates/lodestone-shell/tests/biome_tint_live_mesh.rs`'s
  `an_unresolvable_biome_id_renders_the_plains_default_and_keeps_the_grass_palette_slot` — the
  gate for the gotcha above: an id past the end of a real (non-empty) registry must render exactly
  the palette's plains default *and* keep `GRASS_TINT_SLOT` on every top-face vertex, never 255.
  Its control names the same id `minecraft:swamp` and requires swamp's colormap-independent
  constant instead, so the first arm cannot pass for a mesher that resolves no biome at all. Fired
  control: making the unresolved arm answer `minecraft:badlands` fails it with `[144, 129, 77]`
  against the plains default `[145, 189, 89]`.
- `crates/lodestone-shell/src/net.rs`'s own `#[cfg(test)]` module —
  `forward_folds_biome_registry_names_into_the_cell_without_using_the_channel` proves the real
  `forward` function folds `ClientEvent::BiomeRegistryNames` into `BiomeNameCell` and that it never
  crosses the `NetUpdate` channel, matching `BiomeClimateCell`'s own test.
