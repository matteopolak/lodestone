# Plugin worldgen API: custom generators, custom dimensions, structure placement

## What it is

The plugin-facing seam in `lodestone-worldgen` and `lodestone-server` for custom chunk generators and biomes, primary-world dimension properties and structure-template placement. It is a version-free, plain-function surface (never a bevy `System`) beside the oracle-verified terrain interpreter (see [worldgen](worldgen.md) for the parity discipline). Three pieces:

- `lodestone_worldgen::generator::ChunkGenerator`: a `dyn` trait a plugin implements instead of the verified pipeline, with no parity guarantee.
- `lodestone_server::plugin_dimension::DimensionRegistry`: registers a generator plus server-decided properties under a key and returns a real `ChunkSource`.
- `lodestone_server::structure_placement::place_structure_live`: pastes a `StructureTemplate` into a live or persisted world (generation-time placement calls `StructureTemplate::place` on the generator's working grid).

`crates/plugins/lodestone-void-world` is the reference plugin, and `tests/drives_a_real_dimension_through_a_joined_client.rs` is the end-to-end gate: a real `IntegratedServer`, `V770ServerProtocol` and wire-decoding `lodestone-client` observe its terrain and both placements.

## How it works

### Generator dispatch

```rust
pub trait ChunkGenerator: Send + Sync {
    fn min_y(&self) -> i32;
    fn height(&self) -> i32;
    fn generate(&self, cx: i32, cz: i32) -> DenseBlockGrid;
    fn biome(&self) -> &str { "minecraft:plains" }
}
```

Output is `lodestone_worldgen::dense_grid::DenseBlockGrid`, the vocabulary every real generator's composition stage uses, not the 26.3 terrain source's column (a 4x4x4 biome grid, generation-time block entities, heightmaps, structure starts), which a simple plugin generator should not have to fabricate. The server consumes canonical `StateId` cells directly; generation, retention and live placement never format or parse state strings (text syntax stays at a plugin's config or template input).

`FlatLevelSource` (the jar-verified superflat/void generator) also implements the trait, so one dispatch point serves verified and unverified generators. `Terrain263` deliberately does not: its output (structure starts, generation-time block entities) cannot be represented, and a lossy bridge would drop real data where an author expects it to survive. A plugin wrapping a verified generator (terrain plus one extra rule) needs a new, wider trait, not a wider one here.

### Dimension registration

`crate::dimension::Dimension` stays closed: each variant needs a generator, chunk store, wire `dimension_type` holder id and travel rule, and holder ids come from a fixed compile-time NBT table (`DIMENSION_TYPE_REGISTRY` in the v26-2 family: `overworld`, `overworld_caves`, `the_end`, `the_nether`). Opening it would need a new wire registry entry in the protocol family or would misdescribe a plugin dimension to joining clients. So `DimensionRegistry` is separate and additive:

```rust
pub struct DimensionProperties {
    pub min_y: i32, pub height: i32, pub logical_height: i32,
    pub coordinate_scale: f64,
    pub natural: bool, pub bed_works: bool,
    pub has_skylight: bool, pub has_ceiling: bool,
}
pub struct PluginDimension { pub key: ResourceKey, pub properties: DimensionProperties, pub generator: Arc<dyn ChunkGenerator> }

impl DimensionRegistry {
    pub fn register(&self, dimension: PluginDimension) -> Option<Arc<PluginDimension>>;
    pub fn get(&self, key: &ResourceKey) -> Option<Arc<PluginDimension>>;
    pub fn keys(&self) -> Vec<ResourceKey>;
    pub fn chunk_source(&self, key: &ResourceKey) -> Option<Arc<dyn ChunkSource>>; // built and cached once per key
}
```

`ResourceKey` validates namespace and path (dots and hyphens in a namespace are fine) before a dimension enters the registry; other boundaries lower it with `key.to_string()`. `IntegratedServer::open_in_memory_with_entities` / `open_persistent_with_mobs` are generic over `S: ChunkSource + 'static`, so `DimensionRegistry::chunk_source(key)` in place of `overworld_chunk_source(seed)` opens a primary world on a plugin generator with no change to `crate::integrated`.

Not covered: a registered dimension is not reachable as a second, portal-travel dimension beside a running Overworld; that needs the wire `dimension_type` work above. It is a primary-world choice for now.

Gotcha: `DimensionProperties` bounds and the generator's `min_y()`/`height()` have no compile-time link, and `PluginChunkSource` reads bounds from the generator, so a mismatch serves a wrong-height column silently. Derive one from the other at registration, as `lodestone-void-world::register` does (`min_y: generator.min_y(), height: generator.height(), logical_height: generator.height(), ..DimensionProperties::default()`). The default describes `minecraft:overworld` rules, the safest base for a plugin dimension differing only in terrain.

### Structure placement

Generation-time: `StructureTemplate::place(origin, &PlaceSettings::default(), &mut grid)` writes into the `DenseBlockGrid` the generator already holds.

Live: `place_structure_live(source: &dyn ChunkSource, template, origin: PlaceOrigin, settings: &PlaceSettings) -> usize` reads the template's bounding box, hydrates a working grid from the live source (so a world-inspecting processor, such as a `RuleProcessor`'s "water under this dirt path", sees real placed blocks), calls the same `StructureTemplate::place`, and writes every cell back through `ChunkSource::set_block`, the player-edit path, so the paste persists and reads through `column()`/`block_state()`.

`StructureTemplate::from_blocks(size, palette, blocks)` builds a template programmatically (every block gets `nbt: None`; jigsaw blocks and chest loot references need `parse`). The 1212 bundled templates come from `lodestone_server::embedded_structure_template(id)` / `embedded_structure_template_ids()`, and a plugin's own `.nbt` through `StructureTemplate::parse(bytes)`.

### Consumers and tests

`lodestone-void-world` has `CheckerboardVoidGenerator` (glass and stone checkerboard plus a generation-time gold-and-beacon landmark at chunk `(0, 0)`), `register()` and `place_marker_live()`. Its end-to-end gate obtains the `ChunkSource` only through `DimensionRegistry::chunk_source` and asserts `ClientHandle::block_at` (decoded from real chunk packets) against the checkerboard, landmark and marker, so subject and assertion join only at the wire. `lodestone_worldgen::generator`'s unit tests match `FlatLevelSource`'s impl against its `column()`/`rows()` and check determinism; `plugin_worldgen`, `plugin_dimension` and `structure_placement` tests cover column and grid bridging, biome quarts and cells, edit retention, registry caching and re-registration, and live placement against a hand-rolled `ChunkSource`.

## How to change it

- A `DimensionProperties` field is free to add: one file (`crates/lodestone-server/src/plugin_dimension.rs`) and no wire encoding depends on it.
- Per-column biome variety: `ChunkGenerator::biome` takes `&self` with no coordinates. Widen the signature (breaking `FlatLevelSource` and every plugin) rather than adding a second uniform-only method, and grep every `impl ChunkGenerator for` in the workspace first (a defaulted trait method plus an unforwarding wrapper is an island).
- The wire `dimension_type` gap: `crates/versions/26.2/src/server_protocol.rs`'s `DIMENSION_TYPE_REGISTRY`/`encode_registry_data`/`dimension_type_holder_id` must publish a dynamic entry instead of the fixed four, and `crate::dimension::Dimension`'s travel must accept a non-enum destination key. Outside this seam and unattempted.
- Reusing verified terrain plus an extra rule needs its own wider seam, not a lossy bridge.

## Configuration

None. A `DimensionRegistry` is a plain value built by the plugin's bootstrap; no env var or file.

## Dependencies

`lodestone_worldgen::generator` depends only on `dense_grid` and `flat` (no server dependency, keeping the trait version-free). `plugin_worldgen`, `plugin_dimension` and `structure_placement` use `lodestone-worldgen` and `crate::chunk::{ChunkSource, ChunkColumn}`. `lodestone-void-world` uses `lodestone-client`, `lodestone-v26-2`, `lodestone-model`, `lodestone-data`, `uuid` and `tokio` only as dev-dependencies for its gate; its library depends on no protocol family. See [plugin API](plugin-api.md) (worldgen is not part of that surface: registries are populated by plain calls, not `App::add_plugins`), [worldgen](worldgen.md), and the module doc of `crates/lodestone-worldgen/src/structure/template.rs`.
