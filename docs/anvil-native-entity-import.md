# Anvil native entity import

## What it is

`lodestone_server::anvil_native_entity_import` moves explicitly selected or deterministically discovered overworld `entities/` sidecar chunks into bounded native resident-entity records. It is an opt-in migration seam: the Anvil sidecar remains the complete entity source, while native storage receives only identity, type, feet position, and rotation.

## How it works

Call `preflight_entities` with the selected chunk coordinates and native vertical extent, then inspect its payload-free `EntityImportReport`. The report records every typed source value that the native record cannot retain: motion for every decoded entity, plus present health, item, age, pickup-delay, and preserved fields. It also blocks a malformed extent, duplicate UUID, non-finite pose, out-of-range coordinate, or pose outside the selected column or vertical window.

Pass the resulting `EntityImportAuthorization` to `import_entity_chunk` along with an `EntityStorage` rooted at the Anvil world and a native `WorldStorage`. The function rereads the selected sidecar chunk, reconstructs the report, requires an exact matching authorization, then sends the mapped `NativeEntityRecord` batch to `WorldStorage::write_dirty_entities`. The sidecar codec selects the overworld directory, so every emitted record uses `BuiltinDimension::Overworld`.

For filesystem-scale work, `discover_entity_chunks` accepts `EntityChunkSelection::All` or a coordinate list. `All` reads only canonical populated sidecar slots in `(x, z)` order and refuses a malformed `.mca` name rather than silently skipping it. `preflight_entity_batch` decodes every selected chunk before native storage opens, reports each chunk's loss categories without retaining unsupported NBT, and detects UUIDs duplicated across selected chunks. `import_entity_batch` accepts only the exact aggregate authorization and calls `WorldStorage::write_dirty_entity_chunks`, which validates every pose, UUID, and compact key before committing all typed poses in one transaction.

`lodestone-server anvil-convert import-entities` is the operator-facing consumer. It requires `--source`, `--destination`, an identical `--native-path`, `--min-y`, `--height`, and exactly one selection mode: repeated `--entity-chunk x,z` or `--all-entities`. Preview does not create the native destination. A lossy batch requires `--apply` and the exact payload-free `--acknowledge` review token; blockers cannot be acknowledged. A successful command reopens native storage and checks every selected typed pose. It never rewrites the Anvil sidecar.

An authorized import writes no opaque extensions and does not modify the source region file. It is not a full entity migration: native records absent from the selected source chunk are not deleted, and unsupported source values are never restored from native storage.

## How to change it

If `NativeEntityRecord` gains a field, remove the corresponding `UnsupportedEntityData` entry only after mapping it in `native_record` and extending the independent sidecar fixture expectation. Keep source validation in `preflight_entities` synchronized with `WorldStorage::write_dirty_entities` and `WorldStorage::write_dirty_entity_chunks`; the report must block every malformed pose before authorization can permit a write. Keep batch UUID validation in `preflight_entity_batch` synchronized with storage so a later source chunk cannot make an earlier chunk partially durable.

Do not bypass the authorization comparison for an empty or apparently simple entity. Motion is normalized to a zero vector by `SavedEntity` when absent in the source tree, so the importer conservatively reports it for every decoded entity rather than making a missing source tag look like native support.

## Configuration

There are no environment variables. Library callers supply source coordinates and a section-aligned, positive `min_y`/`height` window. The command uses `import-entities --source <anvil-world> --destination <native-store> --native-path <native-store> --min-y <blocks> --height <blocks> (--entity-chunk <x,z> ... | --all-entities)`, optionally followed by `--apply --acknowledge <review-token>`. The module is native-only and compiles wherever the filesystem-backed entity sidecar codec is available.

## Dependencies

The importer depends on `lodestone_server::entity_storage::EntityStorage` and `SavedEntity` for Anvil sidecar discovery and decoding, `lodestone_server::world_storage::WorldStorage`, `NativeEntityRecord`, and `NativeDirtyEntityChunk` for the typed destination, and `lodestone-storage-schema` for the built-in overworld dimension value.
