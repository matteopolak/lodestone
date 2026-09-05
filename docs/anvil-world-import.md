# Anvil world terrain import

## What it is

`lodestone_server::anvil_world_import` composes the bounded Anvil terrain-region converter into one deterministic world-directory import. It imports only canonical terrain files under `region/`; player, entity, POI, metadata, and auxiliary data remain separate migration choices.

## How it works

`preflight_world_directory` enumerates only `region/r.<x>.<z>.mca` files and sorts them by their signed coordinates. It combines each region's payload-free `PreflightReport` in that stable order, so one `ImportAuthorization` covers every supported and discarded terrain field in the selected world.

`import_world_directory` repeats discovery, decodes every region, and converts every present chunk into an internal prepared native record before it opens `WorldStorage::write_dirty_chunks`. It then compares the supplied authorization with the fresh aggregate report and commits all records in one native transaction. A malformed later region, incompatible source chunk, declined authorization, or stale loss count therefore cannot leave an earlier terrain region written.

The path is deliberately a terrain-only coordinator. It does not infer dimensions from a directory name, discover other dimension directories, follow player files, or treat entity/POI sidecars as safely imported. Callers select the built-in dimension and the native vertical extent explicitly.

## How to change it

Keep discovery strict: a `.mca` file with a noncanonical name is an error because using a guessed coordinate would relocate every local chunk slot. Preserve the `BTreeMap` coordinate order when adding another source member so reports and preparation remain reproducible.

Do not write a region from inside the walk. Add any new source conversion to the preparation phase, include its entries in the one aggregate report, and pass all prepared native records through the sole final `write_dirty_chunks` call. If a source family needs different authorization semantics, give it a distinct coordinator rather than weakening the terrain report.

## Configuration

There are no flags or environment variables. The caller supplies the Anvil world root, native `WorldStorage`, selected dimension identifier, aligned `min_y`, positive height, and matching `ImportAuthorization`.

## Dependencies

The coordinator depends on `lodestone_anvil::import_preflight` for aggregate loss accounting, `anvil_import` for region decoding and typed terrain preparation, and `world_storage::WorldStorage` for the one native transaction.
