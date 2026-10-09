# Anvil import preflight

## What it is

`lodestone_anvil::import_preflight` inventories an Anvil world before native conversion, separating values with a typed destination, values a lossy conversion would discard, and malformed or incompatible values that block it. `lodestone_server::anvil_import` consumes that decision for one `WorldProperties` record, one chunk record, or every present chunk of one explicitly chosen terrain region file; it is not a whole-world walker or an opaque-data preservation layer.

## How it works

- A walker builds a `PreflightReport::builder()` and calls `inspect_level_dat`, `inspect_world_gen_settings`, `inspect_chunk`, `inspect_player` and `inspect_unregistered_auxiliary_file` per source. The builder borrows NBT only while classifying; the report stores source identifiers, NBT paths, target-field classifications and reasons, never unsupported values or file bytes.
- Typed destinations today: data version, default game mode, seed, spawn block position and dimension. Other `level.dat` and world-generation fields are loss. `PreflightBuilder::inspect_native_chunk` marks block-state sections, 3D biomes, the `MOTION_BLOCKING` heightmap and light arrays supported; block entities, scheduled ticks, structures, status and auxiliary chunk fields are loss; a decoded player is a whole-record loss (no mapping yet). Payloads are not kept as extensions to make export reproduce them.
- Blockers: malformed roots, missing required values, unsupported data versions, non-built-in spawn dimensions. `PreflightReport::decide` needs `LossDecision::Abort` or `ProceedAndDiscardUnsupported`; an acknowledgement never overrides a blocker. `anvil_import::import_world_properties` reruns the report and accepts only an `ImportAuthorization` matching the accepted-loss count exactly; missing, aborted, blocked and stale authorizations fail before the native backend is called.
- `import_world_properties` maps supported values to one `WorldProperties` and leaves unsupported ones absent (including the Anvil total age, since the native day clock is not implemented). `import_chunk` maps supported chunk fields into a complete `NativeDirtyChunkRecord`, with the report listing each dropped field. `preflight_region_file` walks all 1,024 region-table entries, resolves oversized-chunk sidecars from the file's parent directory and reports every present chunk. `import_region_file` rebuilds that report, requires one aggregate authorization, prepares every native record before opening the transaction and commits them as one batch, returning the report with seen and committed counts. Player, entity, POI and auxiliary records, multi-directory discovery and export remain on the Anvil path.
- `chunk_nbt::column_from_nbt` accepts only `Status = minecraft:full`, so a region source never promotes an incomplete column to playable terrain. The import boundary uses an import-only decoder variant after preflight, so chunks like `minecraft:initialize_light` convert without weakening that fallback.

## How to change it

- Add a field to the supported set only when the native format has a typed destination and the converter consumes it, with a fixture control for the accepted NBT shape and the rejected alternative. If an extension becomes registered, add a typed extension-aware inspection; never make `inspect_unregistered_auxiliary_file` retain bytes.
- In `crates/lodestone-server/src/anvil_import.rs` keep `WORLD_PROPERTIES_KEY`, the typed chunk mapping, the field-level report and the exact-authorization check together. `preflight_world_properties` (the payload-free seam the operator command uses) must match the fresh preflight inside `import_world_properties`. `import_chunk_bytes` takes one named-NBT payload. Region functions take coordinates separately from the path; never infer them from a filename. Keep prepare-all-before-write, because looping the single-chunk importer could leave earlier chunks committed after a later failure. When the native schema grows to cover entities or ticks, update both the report and the record input rather than quietly removing a loss entry, and keep fixture coverage intentional.

## Configuration

No flags or variables. The accepted source version is `lodestone_anvil::level_dat::DATA_VERSION_26_2`; a different or malformed one blocks rather than loses.

`lodestone-server anvil-convert import-metadata` is the CLI caller for world properties. It needs `--source` (Anvil world), `--destination` and an identical `--native-path`, and reads only `level.dat` and `data/minecraft/world_gen_settings.dat`. Without `--apply` it prints a payload-free preflight and review token and never opens the destination; a lossy report needs both `--apply` and the exact `--acknowledge` token. It writes one typed record. A fresh `WorldStorage::load_world_properties` reopen rejects extension payloads (version 1 has no consumer).

## Dependencies

`lodestone-anvil` metadata wrappers and `lodestone_core::Nbt` (payload-free for native chunk inspection); the native consumers add `lodestone-storage` and `lodestone-storage-schema` via `lodestone_server::world_storage`, `chunk_nbt`, and `lodestone-world` for packed heightmap and light values. No extension values are emitted and no unsupported NBT is retained.
