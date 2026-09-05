# Anvil import preflight

## What it is

`lodestone_anvil::import_preflight` inventories an Anvil world before a future native-world conversion. It separates source values with a typed destination, values that a lossy conversion would discard, and malformed or incompatible values that block conversion. It is an import-safety boundary, not a converter or an opaque-data preservation layer.

## How it works

An import walker creates a `PreflightReport::builder()` and calls `inspect_level_dat`, `inspect_world_gen_settings`, `inspect_chunk`, `inspect_player`, and `inspect_unregistered_auxiliary_file` as it decodes each source. The builder borrows NBT only during classification. Its completed `PreflightReport` stores source identifiers, NBT paths, target-field classifications, and reasons; it never stores an unsupported NBT value or file bytes.

The current typed-destination inventory covers the current data version, default game mode, seed, and spawn block position/dimension. It reports other `level.dat` and world-generation settings fields as loss because there is no native destination for them yet. A decoded chunk and player are also reported as loss: their typed palette/registry and player mappings have not been specified. The report does not treat those payloads as extensions merely to make a later export reproduce them.

Malformed roots, missing required values, unsupported data versions, and non-built-in spawn dimensions are blockers. `PreflightReport::decide` requires the caller to send `LossDecision::Abort` or `LossDecision::ProceedAndDiscardUnsupported`; an acknowledgement cannot override a blocker. A future conversion boundary must accept only an `ImportAuthorization` whose `permits_conversion` result is true.

`SupportedData` means a source value has a declared typed native destination. It deliberately does not claim that the full schema mapping or server save/load conversion exists. That work is a separate integration step.

## How to change it

Add a source field to the supported set only when the native format has a typed destination and the converter will consume it. Add a fixture-backed control for both the accepted NBT shape and the rejected alternative. If an extension becomes registered, introduce a typed extension-aware inspection path; do not change `inspect_unregistered_auxiliary_file` to retain bytes.

When chunk, player, entity, or additional world-property conversion is implemented, replace the corresponding whole-record loss with field-level destination checks and keep the explicit `LossDecision` boundary at the point conversion begins. Update the tests that use checked-in 26.2 `level.dat` and world-generation settings fixtures so their expected coverage remains intentional.

## Configuration

There are no flags or environment variables. The accepted source data version is `lodestone_anvil::level_dat::DATA_VERSION_26_2`; a different or malformed version is a blocking condition rather than a lossy one.

## Dependencies

The preflight module depends only on `lodestone-anvil`'s `level_dat` and `world_gen_settings` wrappers plus `lodestone_core::Nbt`. It does not depend on the native storage schema, storage engine, server, registry data, or an extension runtime.
