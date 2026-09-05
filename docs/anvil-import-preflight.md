# Anvil import preflight

## What it is

`lodestone_anvil::import_preflight` inventories an Anvil world before native conversion. It separates source values with a typed destination, values that a lossy conversion would discard, and malformed or incompatible values that block conversion. `lodestone_server::anvil_import` consumes that decision for one bounded `WorldProperties` record; it is not a whole-world walker or an opaque-data preservation layer.

## How it works

An import walker creates a `PreflightReport::builder()` and calls `inspect_level_dat`, `inspect_world_gen_settings`, `inspect_chunk`, `inspect_player`, and `inspect_unregistered_auxiliary_file` as it decodes each source. The builder borrows NBT only during classification. Its completed `PreflightReport` stores source identifiers, NBT paths, target-field classifications, and reasons; it never stores an unsupported NBT value or file bytes.

The current typed-destination inventory covers the current data version, default game mode, seed, and spawn block position/dimension. It reports other `level.dat` and world-generation settings fields as loss because there is no native destination for them yet. A decoded chunk and player are also reported as loss: their typed palette/registry and player mappings have not been specified. The report does not treat those payloads as extensions merely to make a later export reproduce them.

Malformed roots, missing required values, unsupported data versions, and non-built-in spawn dimensions are blockers. `PreflightReport::decide` requires the caller to send `LossDecision::Abort` or `LossDecision::ProceedAndDiscardUnsupported`; an acknowledgement cannot override a blocker. `anvil_import::import_world_properties` reruns this report and accepts only an `ImportAuthorization` that exactly matches the source's accepted-loss count. Missing, aborted, blocked, and stale authorizations all fail before the native backend is called.

`SupportedData` means a source value has a declared typed native destination. The current consumer maps those values to one native `WorldProperties` record and leaves unsupported fields absent, including the Anvil total-age field because the native day clock is not implemented. Chunk, player, entity, and auxiliary-file records remain on the Anvil path until their mappings are specified.

## How to change it

Add a source field to the supported set only when the native format has a typed destination and the converter will consume it. Add a fixture-backed control for both the accepted NBT shape and the rejected alternative. If an extension becomes registered, introduce a typed extension-aware inspection path; do not change `inspect_unregistered_auxiliary_file` to retain bytes.

The bounded consumer is in `crates/lodestone-server/src/anvil_import.rs`. Keep its fixed `WORLD_PROPERTIES_KEY`, typed field mapping, and exact authorization check together. When chunk, player, entity, or additional world-property conversion is implemented, add a separate typed consumer and keep the explicit `LossDecision` boundary at the point conversion begins. Update the tests that use checked-in 26.2 `level.dat` and world-generation settings fixtures so their expected coverage remains intentional.

## Configuration

There are no flags or environment variables. The accepted source data version is `lodestone_anvil::level_dat::DATA_VERSION_26_2`; a different or malformed version is a blocking condition rather than a lossy one.

## Dependencies

The preflight module depends only on `lodestone-anvil`'s `level_dat` and `world_gen_settings` wrappers plus `lodestone_core::Nbt`. The native consumer additionally depends on `lodestone-storage`/`lodestone-storage-schema` through `lodestone_server::world_storage`; it emits no extension values and retains no source NBT.
