# Worldgen Biome Types

## What it is

Worldgen biome cells carry generated `BuiltinBiome` identities and compact
`BiomeRef` handles instead of one owned string per sampled cell. The name view
exists only at display, packet, and region serialization boundaries.

## How it works

`lodestone-data` generates the built-in biome enum from the checked-in worldgen
asset registry. The generated enum's alphabetical discriminants are canonical
internal identities, not protocol registry ordinals. `BiomeRef` packs one of
those identities, or an explicitly assigned extension index, into a `u32`.

The worldgen consumer is being migrated separately. This data slice deliberately
does not assign extension indices per column: the host registry that admits a
dynamic biome must own the mapping and resolve its name only at a serialization
or display boundary.

## How to change it

Regenerate `crates/lodestone-data/src/generated/biome_enum.rs` with
`LODESTONE_REGEN=1 cargo test -p lodestone-data --test biome_enum --
committed_enum_matches_authoritative_assets -- --ignored`. Do not hand-edit
the generated file. When a consumer needs a built-in name, use the generated
enum's static name accessor; do not treat its canonical identity as a wire
registry id. Dynamic biome support must provide an explicit host-owned
`ExtensionId` registry before it crosses into a packet or save encoder.

## Configuration

No runtime flags are required. Strict resource loading is the default for
worldgen tables; extension-aware constructors are opt-in for plugin/data-pack
boundaries and tests.

## Dependencies

The enum is generated from the repository's authoritative worldgen asset
registry and consumed by `lodestone-worldgen`. Packet and region encoders may
use the borrowed palette name view without changing the compact worldgen
storage.
