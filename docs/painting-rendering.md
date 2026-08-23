# Painting rendering

## What it is

The wall-hung painting entity, end to end: the variant off the wire, the 51-entry
size table, one baked mesh per shape, one texture per variant, and the GPU pass
that draws them. A painting is neither a rig nor a billboard — it is a flat slab
of `width x height` blocks — so it reaches pixels through its own pass rather
than through the mob corpus.

## How it works

### The chain, and which link each piece is

| link | symbol |
| --- | --- |
| wire | `Painting.DATA_PAINTING_VARIANT_ID`, serializer `PAINTING_VARIANT` |
| decode | `lodestone_v770`'s `Value::PaintingVariant`, resolved through `entity_variants::painting_variant` |
| event | `lodestone_model::event::EntityMetadataUpdate::painting_variant` |
| ECS | `lodestone_ecs::entity::PaintingVariant` |
| draw record | `lodestone_shell::entities::EntityDraw::painting` |
| geometry | `lodestone_render::painting::painting_mesh` / `painting_matrix` |
| pass | `RenderState::prepare_paintings` -> `PaintingDrawBatch` |
| counter | `RenderStats::paintings_drawn` |

### The facing is the yaw, and needs nothing decoded

It is natural to reach for the spawn packet's Object Data field, because
`Painting.getAddEntityPacket` really does send `getDirection().get3DDataValue()`
there and `recreateFromPacket` really does read it. That work is unnecessary:
`HangingEntity.setDirection` also does `setYRot(direction.get2DDataValue() * 90)`,
so the facing is already in the entity's **ordinary yaw**, which `ADD_ENTITY`
carries and `EntityDraw::yaw` already holds. The four legal values (0 south, 90
west, 180 north, 270 east) survive the wire's byte-angle quantisation exactly.

`painting_matrix` is therefore the whole placement: `T(position) · Ry(180 - yaw)`,
which is `PaintingRenderer.submit`'s single `mulPose` call. No `scale(-1, -1, 1)`
and no `1.501` lift — `PaintingRenderer` extends `EntityRenderer`, not
`LivingEntityRenderer` — and the mesh is authored Y-up to match.

The position is the slab's **centre**, not a mob's feet: `Painting.calculateBoundingBox`
places the entity there.

### The variant table, and why its order is measured rather than transcribed

`lodestone_render::painting::PAINTING_VARIANTS` carries all 51 variants as
`(name, width, height)`, read out of the pinned jar's own
`data/minecraft/painting_variant/*.json`. Nine distinct `(width, height)` shapes
cover all 51.

The wire carries a `Holder<PaintingVariant>` — an index into the *server's*
registry — so a second table, `lodestone_v770`'s `entity_variants::PAINTING`,
maps id to name in **registry order**. That order is **alphabetical**, and this
is the trap: `PaintingVariants.bootstrap` registers `kebab` first, and
transcribing it would be wrong. Painting variants are a data-pack registry loaded
from JSON through the resource manager, which lists keys sorted, so id 0 is
`minecraft:alban`. That was settled by decoding the repo's own captured
`registry_data` payload
(`crates/protocol/v770/tests/fixtures/registry_data_painting_variant.hex`, from a
real vanilla 26.2 server): 51 entries, in exactly `sorted()` order, `alban` first.

**What a data pack breaks.** A pack that adds or removes a variant shifts every
id after it, and the table then names the wrong painting — silently, since every
id still resolves. The per-server answer already exists in
`ClientRegistries::entry_names`; wiring it in needs a registry handle threaded
through `read_entity_metadata`, whose signature has more than fifty call sites.
The committed table matches the pattern the appearance-variant tables beside it
already set, and carries the same hazard.

### Why a default is synthesized at spawn

`Painting.defineSynchedData` defaults the accessor to
`VariantUtils.getAny(registry)` — the registry's *first* entry, i.e. `alban`. A
painting hung with that variant is entirely at its accessors' defaults, so the
server sends **no** index-9 field for it and the variant would never arrive. The
adapter therefore synthesizes it at `ADD_ENTITY`, exactly as it already does for
a sheep's default fleece and a creeper's three flags. Without it, 50 of the 51
variants would draw and one would silently not — the most confusing possible
failure shape.

### The mesh: keyed by shape, textured by variant

`painting_mesh(width, height)` returns **two** meshes. The front face samples the
variant's own sprite; the back and the four boundary edges sample one shared
`back.png`. They are separate because this engine binds one texture per draw,
where vanilla emits a single interleaved stream out of its paintings atlas. On
the GPU they become one `GpuEntityModel` with two parts (`parts[0]` front,
`parts[1]` frame), so a batch is keyed by `(shape, face)`.

The geometry reproduces `renderPainting`'s **cell grid** rather than collapsing
to one quad. For the front that is pixel-identical — the per-cell UVs are an
exact subdivision of the same sprite — but the back and edges **tile** (each cell
samples the whole `back` sprite), so a stretched single quad would be visibly
wrong on a 4x4. Keeping the grid also leaves the door open for per-cell light.

### Light is per painting, not per cell — the one deliberate gap

Vanilla's grid exists to sample the wall **once per 1x1 cell**
(`PaintingRenderer.extractRenderState` walks the direction's own tangent and
fills `lightCoordsPerBlock`), so a large painting half in torchlight is visibly
graded. This engine carries light per *instance*, so every cell of a painting
shares its entity probe. The geometry is already per cell, so closing this is a
change to how the light lane is fed, not a re-bake.

## How to change it

* **`EntityDraw::painting` is `Option<&'static str>` and `None` must draw
  nothing.** There is no fallback shape: a painting's size in blocks *is* a
  property of its variant, so a 1x1 stand-in where a 4x4 belongs reads as a
  rendering bug rather than as an unsupported pack. The narrowing from a
  wire-supplied key to a table name happens once, in `extract_entity_draws`, so
  the draw site has no decision to make.
* **A variant in the table whose sprite is missing from the pack is skipped
  too**, for the same reason — the same asymmetry `docs/entity-rendering.md`
  records for armour and wool.
* **Adding a variant** means adding a row to `PAINTING_VARIANTS` *and* a row to
  `entity_variants::PAINTING` **in its sorted position**, and re-checking the
  fixture. The two tables are separate because one is render data and the other
  is a wire-order map; keeping them in one place would put a protocol concern in
  the render crate.
* The pass runs unconditionally beside `prepare_entities`, so a painting with no
  variant costs one `Option` test.

## Configuration

None. No feature gate, no env var. Every sprite is a jar asset, so a resource
pack replaces it through the ordinary `ResourceManager` stack.

## Verification

`crates/lodestone-shell/tests/painting_pixels.rs` drives the real
`RenderState::render` and makes two assertions, because coverage alone is not
enough:

* **Coverage** — the pixels that change between a known and an unknown variant,
  bracketed against the front face's analytic projected rect. Measured: **10,816**
  changed pixels against an analytic **10,884.9**, with the changed bounding box
  `(108, 68)..(211, 171)` against the analytic rect `(107.8, 67.8)..(212.2, 172.2)`.
* **Discrimination** — `pointer` and `pigscene` are both 4x4, so they share a
  mesh, a shape and a batch key and differ only in the bound texture. Measured
  **9,453** differing pixels. A pass that bound one shared sheet, or keyed the
  texture lookup by shape, would pass every coverage check and fail this one.

The neuter was observed rather than described: disabling the draw-loop arm took
coverage to **0** and the gate failed. Note `paintings_drawn` still read 1 under
that neuter, because it is incremented in `prepare_paintings`, one layer above
the draw.

The gate installs its own `EntityDraw`, so it verifies the draw and says nothing
about the producer — that the wire's metadata really lands in
`EntityDraw::painting` is `crates/protocol/v770`'s question.

## Dependencies

`lodestone-render` (the table, the mesh, the placement), `lodestone-model` and
`lodestone-ecs` (the event field and the component), `crates/protocol/v770` (the
serializer decode, the registry-order map and the spawn-time default), and
`lodestone-shell`'s GPU entity passes.
