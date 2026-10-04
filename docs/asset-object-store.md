# Asset object store

## What it is

`lodestone_shell::asset_objects::AssetObjectStore` reads the launcher-style
content-addressed asset index and exposes the indexed bytes as a resource
source. `AssetObjectHash` represents the validated address used for each
object's two-character fanout directory and filename.

## How it works

`parse_asset_index` is the external JSON boundary. It accepts an entry only
when its `hash` is exactly a lowercase, 40-character hexadecimal SHA-1 digest,
then stores that value as `AssetObjectHash` alongside the declared byte count.
All normal reads, presence counts, and diagnostic paths consume the typed
address, so they cannot form an `objects/<prefix>/<hash>` path from an
arbitrary string. An index name still remains a string because it is the
resource namespace key and can name any asset supplied by an index.

The store verifies the byte length at read time. A missing or truncated object
is absent; it is never forwarded to a decoder as a complete resource.

## How to change it

When adding an object-store consumer, pass an index key to
`AssetObjectStore::object_bytes` or use its `ResourceSource` implementation.
Do not reconstruct object paths from strings: add behavior to
`AssetObjectHash` if the store layout needs a new derived path. Keep external
index parsing at `parse_asset_index`; values from a different manifest format
must validate there before they become a store address.

## Configuration

`LODESTONE_ASSETS` selects a store directory directly; it must contain exactly
one `asset-index-*.json` and an `objects/` tree. Without it, discovery is
`lodestone_mc_cache::cache_root`: `.cache/mc/<current version>` (from the
`mc-version` file, or `LODESTONE_MC_VERSION`) in the nearest ancestor of the
working directory. There is no directory scan and no second variable; see
[`mc-version-bump.md`](./mc-version-bump.md).

## Dependencies

The module uses `serde_json` to parse the index and
`lodestone_assets::ResourceSource` to serve object bytes to resource consumers.
It performs no network fetches; `xtask fetch-assets` and `xtask fetch-sounds`
populate the local object store separately.
