# Dimension environment attributes

## What it is

The registry-to-rendering path for a dimension type's environment attributes. It preserves the complete server-declared attribute map while exposing the visual fog, sky, cloud, ambient-light, and sky-light-factor values consumed by the client renderer.

## How it works

The 26.2 adapter decodes the `attributes` compound in each `minecraft:dimension_type` registry entry. The raw key/value pairs are retained in `DimensionTypeInfo::environment_attributes`, while recognized visual values are validated into typed optional fields. Invalid or non-finite typed values remain available in the raw map but are not presented as render inputs.

The session folds the resolved `DimensionTypeInfo` into `PlayerSnapshot`. Each frame, the shell resolves dimension fog and cloud values and installs them on `RenderState`; the sky pass applies the day timeline and cloud alpha gate. The same sky-light-factor source feeds terrain, fluid, entity, and first-person lightmap uniforms. A negative uniform value means no source is installed; zero is a valid value and makes the End's sky contribution disappear.

## How to change it

Add a typed field beside the existing fields in `DimensionTypeInfo`, parse it in `DimensionType::from_nbt`, and copy it in the 26.2 adapter's `dimension_type_info`. Keep the original attribute in `environment_attributes` so unknown data-pack extensions are not lost. Add a captured registry fixture assertion and a malformed-value control before adding a render consumer.

Visual consumers belong in `Sim::fog_settings`/`Sim::cloud_color` and the per-frame source installation in `app/redraw.rs`. Keep all four lightmap shader copies (`model.wgsl`, `entity.wgsl`, `fluid.wgsl`, and `block.wgsl`) synchronized when changing the sky-light-factor lane.

## Configuration

No environment variables or player options affect these values. They arrive in the 26.2 configuration-phase registry stream. Dimensions without a resolved registry entry use the renderer's pre-session fallback; a resolved entry with an omitted attribute does not invent a custom value.

## Dependencies

- `crates/versions/26.2/src/packets/registry.rs` decodes the registry NBT.
- `lodestone-model::DimensionTypeInfo` carries typed and raw values across the version seam.
- `lodestone-ecs` and `lodestone-client` fold the active dimension into the session snapshot.
- `lodestone-shell` resolves the active values and uploads them to `lodestone-render`.
- `lodestone-render` owns fog, sky, cloud, and lightmap math and shader uniforms.
