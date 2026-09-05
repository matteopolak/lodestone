# Anvil native entity import

## What it is

`lodestone_server::anvil_native_entity_import` moves one selected overworld `entities/` sidecar chunk into bounded native resident-entity records. It is an opt-in migration seam: the Anvil sidecar remains the complete entity source, while native storage receives only identity, type, feet position, and rotation.

## How it works

Call `preflight_entities` with the selected chunk coordinates and native vertical extent, then inspect its payload-free `EntityImportReport`. The report records every typed source value that the native record cannot retain: motion for every decoded entity, plus present health, item, age, pickup-delay, and preserved fields. It also blocks a malformed extent, duplicate UUID, non-finite pose, out-of-range coordinate, or pose outside the selected column or vertical window.

Pass the resulting `EntityImportAuthorization` to `import_entity_chunk` along with an `EntityStorage` rooted at the Anvil world and a native `WorldStorage`. The function rereads the selected sidecar chunk, reconstructs the report, requires an exact matching authorization, then sends the mapped `NativeEntityRecord` batch to `WorldStorage::write_dirty_entities`. The sidecar codec selects the overworld directory, so every emitted record uses `BuiltinDimension::Overworld`.

An authorized import writes no opaque extensions and does not modify the source region file. It is not a full entity migration: native records absent from the selected source chunk are not deleted, and unsupported source values are never restored from native storage.

## How to change it

If `NativeEntityRecord` gains a field, remove the corresponding `UnsupportedEntityData` entry only after mapping it in `native_record` and extending the independent sidecar fixture expectation. Keep source validation in `preflight_entities` synchronized with `WorldStorage::write_dirty_entities`; the report must block every malformed pose before authorization can permit a write.

Do not bypass the authorization comparison for an empty or apparently simple entity. Motion is normalized to a zero vector by `SavedEntity` when absent in the source tree, so the importer conservatively reports it for every decoded entity rather than making a missing source tag look like native support.

## Configuration

There are no environment variables or flags. The caller supplies the source chunk coordinates and a section-aligned, positive `min_y`/`height` window. The module is native-only and compiles wherever the filesystem-backed entity sidecar codec is available.

## Dependencies

The importer depends on `lodestone_server::entity_storage::EntityStorage` and `SavedEntity` for Anvil sidecar decoding, `lodestone_server::world_storage::WorldStorage` and `NativeEntityRecord` for the typed destination, and `lodestone-storage-schema` for the built-in overworld dimension value.
