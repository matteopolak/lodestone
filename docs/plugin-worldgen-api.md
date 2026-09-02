# Plugin worldgen API — custom generators, custom dimensions, structure placement

## What it is

The plugin-facing seam that answers three issues Paper's own API covers — `ChunkGenerator`/
`BiomeProvider` (#132), per-world dimension creation (#134), and structure-template pasting (#136) —
scoped against what `lodestone-worldgen`/`lodestone-server` actually are today: a version-free,
oracle-verified terrain interpreter (see `docs/worldgen.md`'s own parity discipline) called imperatively from plain
functions, never installed as a bevy `System`.

Three pieces, each landed:

* [`lodestone_worldgen::generator::ChunkGenerator`] — a `dyn`-dispatched trait a plugin implements
  instead of the verified pipeline, carrying no correctness guarantee (exactly Bukkit's own
  contract).
* [`lodestone_server::plugin_dimension::DimensionRegistry`] — a plugin registers a generator plus
  server-decided dimension properties under a key and gets back a real
  [`lodestone_server::ChunkSource`].
* [`lodestone_server::structure_placement::place_structure_live`] — pastes a
  `lodestone_worldgen::structure::template::StructureTemplate` into an already-generated,
  live/persisted world; generation-time placement (a plugin generator calling
  `StructureTemplate::place` on its own working grid) needed no new API at all, since that primitive
  was already public.

`crates/plugins/lodestone-void-world` is the reference plugin exercising all three together, and its
`tests/drives_a_real_dimension_through_a_joined_client.rs` is the end-to-end proof: a real
`IntegratedServer`, a real `V770ServerProtocol`, and a real, wire-decoding `lodestone-client` observe
the plugin's terrain and both structure placements — not a test calling the plugin's own functions
directly.

## Issue #132's decision

**A plugin generator lives behind its own `dyn ChunkGenerator` trait, dispatched imperatively from
plain functions — never installed as a bevy `System`.** This was already the shape a prior pass on
this issue committed to (see the issue's own decision comment); what was missing was the trait itself
and something to dispatch it into. Both now exist.

The trait's output is [`lodestone_worldgen::dense_grid::DenseBlockGrid`] — this crate's own existing
"dense block field over a box" vocabulary, already what every real generator's composition stage
converges on internally — **not** [`lodestone_worldgen::overworld::GeneratedColumn`], which carries a
4×4×4 biome grid, generation-time block entities, a `MOTION_BLOCKING` heightmap snapshot and stage
timings: fields a demo/plugin generator has no business answering honestly. Forcing a plugin to fill
all of that would make the simplest possible generator (a flat floor, a checkerboard) carry
placeholder data for fields nothing reads meaningfully.

```rust
pub trait ChunkGenerator: Send + Sync {
    fn min_y(&self) -> i32;
    fn height(&self) -> i32;
    fn generate(&self, cx: i32, cz: i32) -> DenseBlockGrid;
    fn biome(&self) -> &str { "minecraft:plains" } // default: vanilla's own FixedBiomeSource fallback
}
```

**The native proof, not just a plugin-only trait:** `lodestone_worldgen::flat::FlatLevelSource` — a
real, jar-verified generator already serving vanilla superflat/void worlds — implements
`ChunkGenerator` too. This is one dispatch point serving both a verified native generator and an
unverified plugin one, which is what makes the "seam the vanilla interpreter also implements" half of
the original decision comment true rather than aspirational.

`OverworldGenerator`/`NetherGenerator`/`EndGenerator` do **not** implement this trait, and that is
deliberate, not a gap: their own output types (`GeneratedColumn`/`NetherColumn`/`EndColumn`) carry data
`DenseBlockGrid` cannot represent, and bridging them "lossily" into this trait would silently discard
real, verified data (structure starts, generation-time block entities) at exactly the boundary a
plugin author would reasonably expect that data to survive. If a future need arises for a plugin to
*wrap* one of the verified generators (a "vanilla terrain plus one extra rule" generator), that is a
new, wider trait — not a reason to widen this one.

## Issue #134's decision — and its honest boundary

**`crate::dimension::Dimension` stays closed.** Its own doc says so explicitly: every variant needs a
generator, a chunk store, a wire `dimension_type` holder id and a travel rule, and the holder id is
published from a **fixed, compile-time NBT table** (`DIMENSION_TYPE_REGISTRY` in the v770 protocol
family — four entries, `overworld`/`overworld_caves`/`the_end`/`the_nether`, each a literal NBT byte
array). Making `Dimension` open-ended would mean either wiring a genuinely new wire `dimension_type`
registry entry through the protocol family (a version-crate change: `crates/protocol/v770`, which sits
outside both `lodestone-worldgen`'s and `lodestone-server`'s own seam and outside this issue's file
ownership) or silently mis-describing a plugin dimension's real properties to a joining client — worse
than not offering it at all.

So [`DimensionRegistry`] is a **separate, additive** mechanism, not a fourth `Dimension` variant:

```rust
pub struct DimensionProperties {
    pub min_y: i32, pub height: i32, pub logical_height: i32,
    pub coordinate_scale: f64,
    pub natural: bool, pub bed_works: bool, pub respawn_anchor_works: bool,
    pub piglin_safe: bool, pub ultrawarm: bool,
    pub has_skylight: bool, pub has_ceiling: bool,
}

pub struct PluginDimension {
    pub key: String,              // "myplugin:void" — never "minecraft:"-prefixed
    pub properties: DimensionProperties,
    pub generator: Arc<dyn ChunkGenerator>,
}

impl DimensionRegistry {
    pub fn register(&self, dimension: PluginDimension) -> Option<Arc<PluginDimension>>;
    pub fn get(&self, key: &str) -> Option<Arc<PluginDimension>>;
    pub fn keys(&self) -> Vec<String>;
    pub fn chunk_source(&self, key: &str) -> Option<Arc<dyn ChunkSource>>; // built and cached once per key
}
```

**What this closes:** issue #132's decision comment named two blockers beyond the missing trait —
"no per-world generator selection mechanism exists" and custom dimension registration itself being an
open gap. Both are closed here with **zero changes** to `crate::integrated`:
`IntegratedServer::open_in_memory_with_entities`/`open_persistent_with_mobs` are already generic over
`S: ChunkSource + 'static`, and `DimensionRegistry::chunk_source(key)` hands back exactly that — pass
it in place of `overworld_chunk_source(seed)` to open a **primary** world backed by a plugin's
generator. `crates/plugins/lodestone-void-world`'s own integration test does exactly this.

**What this does NOT close:** a registered dimension is not (yet) reachable as a **second**,
portal-travel dimension alongside a running Overworld the way the Nether/End are. That needs the wire
`dimension_type` registry work described above — a real, scoped, future piece of work, not something
silently half-built here. Until it lands, a plugin's custom dimension is a **primary-world** generator
choice (the dominant real-world use anyway — Bukkit's `ChunkGenerator` is most commonly handed to
`WorldCreator` for exactly this), not a Nether-style secondary destination.

**Gotcha:** `DimensionProperties.min_y`/`height`/`logical_height` and the generator's own
`min_y()`/`height()` have no compile-time link — `PluginChunkSource` reads vertical bounds from the
*generator*, not from `DimensionProperties`, so a mismatch is a silent bug (a served column with the
wrong height), not a compile error. Derive one from the other at the registration call site, the way
`lodestone-void-world::register` does:

```rust
let generator = Arc::new(CheckerboardVoidGenerator::new());
registry.register(PluginDimension {
    key: DIMENSION_KEY.to_string(),
    properties: DimensionProperties {
        min_y: generator.min_y(),
        height: generator.height(),
        logical_height: generator.height(),
        ..DimensionProperties::default()
    },
    generator,
});
```

`DimensionProperties::default()` is vanilla's own `minecraft:overworld` entry — the safest default for
a plugin dimension that wants ordinary player rules and differs from vanilla only in its terrain.

## Issue #136's decision

Two separate placement moments, both now real:

**Generation-time** needed no new API. `lodestone_worldgen::structure::template::StructureTemplate::place`
was already public and already writes into a `DenseBlockGrid` — the exact type a `ChunkGenerator`
implementation already holds while building its column. A plugin generator calls it directly:

```rust
if cx == 0 && cz == 0 {
    let template = landmark_template();
    template.place(origin, &PlaceSettings::default(), &mut grid);
}
```

**Live/post-generation** was the real gap CLAUDE.md's own context named: "`StructureTemplate::place`
writes into a generation-time `DenseBlockGrid`, called only from chunk-generation code — there is
still no runtime entry point a plugin (or anything else) can call outside generation." That entry
point is [`lodestone_server::structure_placement::place_structure_live`]:

```rust
pub fn place_structure_live(
    source: &dyn ChunkSource,
    template: &StructureTemplate,
    origin: PlaceOrigin,
    settings: &PlaceSettings,
) -> usize
```

It reads the template's own bounding box, hydrates a working `DenseBlockGrid` from the **live** source
(so a processor that inspects the world — a `RuleProcessor`'s "is there water under this dirt path"
check, for one — sees real, already-placed blocks, not generation-time terrain-in-progress), calls the
exact same `StructureTemplate::place` generation uses, and writes every cell in the bounding box back
through `ChunkSource::set_block` — the same edit path a player's own block placement goes through, so
the paste persists and reports through `column()`/`block_state()` exactly like any other edit.

**Building a template without an `.nbt` file:** `StructureTemplate::from_blocks(size, palette, blocks)`
is a new, plain constructor (alongside the existing `parse`/`empty`) for a plugin building a structure
programmatically rather than shipping a file — used by both `lodestone-void-world`'s generation-time
landmark and its live-placed marker. A template that needs an attached NBT compound (a jigsaw block, a
chest's loot-table reference) still needs `StructureTemplate::parse` — `from_blocks` gives every block
`nbt: None`.

**Where templates come from otherwise:** the 1212 bundled vanilla templates are already reachable via
`lodestone_server::embedded_structure_template(id)`/`embedded_structure_template_ids()`, and a
plugin's own `.nbt` bytes go through `StructureTemplate::parse(bytes)` — both pre-existing, public, and
untouched by this work.

## What consumes this

* `crates/plugins/lodestone-void-world` — the reference plugin. `CheckerboardVoidGenerator` implements
  `ChunkGenerator` (a glass/stone checkerboard floor plus a generation-time gold-and-beacon landmark at
  chunk `(0, 0)`); `register()` registers it into a `DimensionRegistry`; `place_marker_live()` pastes a
  second, one-block template into an already-generated world.
* `crates/plugins/lodestone-void-world/tests/drives_a_real_dimension_through_a_joined_client.rs` — the
  end-to-end gate: a real `IntegratedServer` serves a `ChunkSource` obtained purely through
  `DimensionRegistry::chunk_source` (never by constructing the generator's own type directly), a real
  `lodestone-client` running the real `V770Adapter` joins over an in-memory duplex, and
  `ClientHandle::block_at` — decoded off real chunk packets — is asserted against the checkerboard, the
  generation-time landmark, and the live-pasted marker. This is what proves the seam is not a closed
  loop: the assertion (`block_at`) and the subject (the generator/registry/live-placement chain) are
  authored by different code paths, joined only by the real wire.
* `lodestone_worldgen::generator`'s own unit tests prove `FlatLevelSource`'s `ChunkGenerator` impl
  matches its own `column()`/`rows()` output exactly, and that the trait is deterministic across
  repeated calls at the same coordinates.
* `lodestone_server::plugin_worldgen`/`plugin_dimension`/`structure_placement`'s own unit tests cover
  the bridging (column↔grid, biome quarts and cells both populated, edit retention, registry caching
  and re-registration, live placement against a hand-rolled `ChunkSource`) in isolation, one layer
  below the end-to-end gate above.

## How to change it

* **Adding a field to `DimensionProperties`**: it is a plain struct with a `Default` impl reachable
  from one file (`crates/lodestone-server/src/plugin_dimension.rs`) — no wire encoding depends on it
  today (see the honest-boundary note above), so a new field is free to add and does not need touching
  anywhere else.
* **A generator that wants per-column (not per-generator) biome variety**: `ChunkGenerator::biome`
  takes `&self` with no `cx`/`cz` — this was a deliberate simplicity choice for a demo/plugin
  generator, matching vanilla's own `FixedBiomeSource` fallback. A generator needing real per-column
  biome variety should widen the trait method's signature (a breaking change to the one impl,
  `FlatLevelSource`, and to every plugin) rather than adding a second, uniform-only method — the
  existing repo lesson about a defaulted trait method plus an unforwarding wrapper being an island
  generator applies here too: **grep for every `impl ChunkGenerator for` in the workspace before
  changing the trait**, not just the one plugin you have in mind.
* **Wiring the wire `dimension_type` registry gap** (making a `DimensionRegistry` entry reachable as a
  *second*, portal-travel dimension): needs `crates/protocol/v770/src/server_protocol.rs`'s
  `DIMENSION_TYPE_REGISTRY`/`encode_registry_data`/`dimension_type_holder_id` to publish a
  dynamically-supplied entry instead of the fixed four, plus `crate::dimension::Dimension`'s travel
  machinery to accept a non-enum destination key. Both are real, scoped, future work — not attempted
  here, since both sit outside `lodestone-worldgen`/`lodestone-server`'s own seam.
* **A plugin generator that wants to reuse verified vanilla terrain plus one extra rule** (a "vanilla
  overworld but ores are diamond" generator): not served by `ChunkGenerator` today — see the note under
  issue #132 above on why `OverworldGenerator` deliberately does not implement this trait. That needs
  its own, wider seam, not a lossy bridge bolted onto this one.

## Configuration

None of its own. A `DimensionRegistry` is a plain value a plugin's own bootstrap code constructs and
populates — nothing reads an env var or a config file here.

## Dependencies

`lodestone_worldgen::generator` depends only on `lodestone_worldgen::dense_grid` and
`lodestone_worldgen::flat` (for the native `ChunkGenerator` impl) — no `lodestone-server` dependency,
keeping the trait itself version-free and server-free. `lodestone_server::plugin_worldgen`/
`plugin_dimension`/`structure_placement` depend on `lodestone-worldgen` (already a normal dependency of
`lodestone-server`) and on `crate::chunk::{ChunkSource, ChunkColumn}`. `crates/plugins/lodestone-void-world`
depends on both path-wise, plus `lodestone-client`/`lodestone-v770`/`lodestone-model`/`lodestone-data`/
`uuid`/`tokio` as dev-dependencies for its end-to-end gate only — its own library has no dependency on
any protocol family, matching the version-seam discipline every other worldgen code in this repo
follows.

## See also

- [`plugin-api.md`](plugin-api.md) — the bevy-`Plugin` client-side surface, including its
  "Registration (native tier)" section (how a plugin gets into the client's `App`). Worldgen is
  deliberately **not** part of that surface (see issue #132's decision and `docs/worldgen.md`'s parity
  discipline): a `DimensionRegistry` is populated by plain function calls, not `App::add_plugins`, so
  this document is worldgen's own, separate plugin seam.
- [`worldgen.md`](worldgen.md) — the verification/parity discipline this document's whole
  "dyn-dispatched, no oracle guarantee" framing is answering to.
- `crates/lodestone-worldgen/src/structure/template.rs`'s own module doc — the placement engine both
  generation-time and live placement are built on.
