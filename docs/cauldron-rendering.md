# Cauldron rendering

## What it is

Cauldron block models combine an opaque body and rim with an inset liquid
surface. They remain on Lodestone's depth-writing model path so the liquid
cannot blend through the cauldron body.

## How it works

`lodestone-render::block_models` tags the `cauldron`, `water_cauldron`, and
`lava_cauldron` state models. `lodestone-shell::mesher` excludes those tagged
models from whole-model translucent routing even when their liquid sprite has
partial alpha. The ordinary cutout depth test then keeps the body in front of
the inset surface where their projected pixels overlap.

## How to change it

Add a state here only when one baked model mixes an opaque enclosure with an
internal partially-alpha surface. Do not broadly classify transparent blocks
as opaque; stained glass, ice, and ordinary translucent terrain still require
the sorted depth-write-off pass. If cauldron liquid becomes a separately
generated mesh, remove the exception and give the body and liquid independent
pipelines instead.

## Configuration

There are no runtime flags. The state classification is derived from canonical
block paths in `crates/lodestone-render/src/block_models.rs`.

## Dependencies

This relies on the baked block-model resolver, `SnapshotModelView` in the shell
mesher, and the existing opaque/cutout and translucent terrain pipelines.
