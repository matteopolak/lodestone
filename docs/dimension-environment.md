# Dimension environment attributes

## What it is

The registry-to-rendering path for a dimension type's environment attributes. It preserves the complete server-declared attribute map while exposing the visual fog, sky, cloud, ambient-light, and sky-light-factor values consumed by the client renderer.

## How it works

The 26.2 adapter decodes the `attributes` compound in each `minecraft:dimension_type` registry entry. The raw key/value pairs are retained in `DimensionTypeInfo::environment_attributes`, while recognized visual values are validated into typed optional fields. Invalid or non-finite typed values remain available in the raw map but are not presented as render inputs.

The session folds the resolved `DimensionTypeInfo` into `PlayerSnapshot`. Each frame, the shell resolves dimension fog and cloud values and installs them on `RenderState`; the sky pass applies the day timeline and cloud alpha gate. Submerged fog samples the rendered camera position from the live world, so third-person fog can differ from the player's swimming and air state. The same sky-light-factor source feeds terrain, fluid, entity, and first-person lightmap uniforms. A negative uniform value means no source is installed; zero is a valid value and makes the End's sky contribution disappear.

## How to change it

Add a typed field beside the existing fields in `DimensionTypeInfo`, parse it in `DimensionType::from_nbt`, and copy it in the 26.2 adapter's `dimension_type_info`. Keep the original attribute in `environment_attributes` so unknown data-pack extensions are not lost. Add a captured registry fixture assertion and a malformed-value control before adding a render consumer.

The attribute-to-render-value rules live in one place, `lodestone-shell`'s `dimension_environment` module (fog and sky colour, cloud colour, base sky-light factor, ambient floor). `Sim::fog_settings`/`Sim::cloud_color` and both connect paths' sky-darken and ambient sources (`app/session.rs`, `app/lifecycle.rs`) call it, so a new rule goes there once, not into each caller. Keep camera-fluid sampling separate from player physics: the view controls fog, while player submersion controls air and movement. Keep all four lightmap shader copies (`model.wgsl`, `entity.wgsl`, `fluid.wgsl`, and `block.wgsl`) synchronized when changing the sky-light-factor lane.

## Verification

- Decode: `registry_data.rs` in the 26.2 session tests decodes captured vanilla bytes for the four real dimensions, and builds a custom dimension from raw NBT covering bare, modifier-wrapped and numeric tags, an unknown key, hostile (non-finite or malformed) values and an attribute-free control.
- Handoff: the `dimension_environment` unit tests give a custom dimension non-default values for every attribute and pair each with a control (attribute absent, or no resolved dimension) that keeps the existing Overworld behaviour. The Overworld declares no sky-light factor, so it follows the time-of-day curve; the End declares zero.
- Pixels: `sky_pipeline_gpu.rs` (`a_dimension_cloud_colour_reaches_the_cloud_pixels`), the sky-gradient gates for fog and sky colour, and the first-person light and entity light gates for the sky-light factor and ambient floor. Run GPU gates with `-- --ignored`.
- The sky-light curve itself is checked tick-by-tick against a JVM dump in `sky_light_factor_timeline.rs`; production reads it through `base_sky_light_factor` when the dimension declares no factor.
- `Sim` needs a live connection to read the registry, so the witness stops at the pure functions; the call sites are the six listed above.

## Configuration

No environment variables or player options affect these values. They arrive in the 26.2 configuration-phase registry stream. Dimensions without a resolved registry entry use the renderer's pre-session fallback; a resolved entry with an omitted attribute does not invent a custom value.

## Dependencies

- `crates/versions/26.2/src/packets/registry.rs` decodes the registry NBT.
- `lodestone-model::DimensionTypeInfo` carries typed and raw values across the version seam.
- `lodestone-ecs` and `lodestone-client` fold the active dimension into the session snapshot.
- `lodestone-shell` resolves the active values and uploads them to `lodestone-render`.
- `lodestone-render` owns fog, sky, cloud, and lightmap math and shader uniforms.
