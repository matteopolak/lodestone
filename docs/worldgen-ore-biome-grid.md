# Ore placement biome lookup grid

## What it is

The source-ordered Overworld ore pass uses a typed, request-scoped grid to memoize the
three-dimensional nearby-corner biome selected for each candidate block position.

## How it works

The grid is indexed by source chunk, local X/Z, and generated Y. It keeps a small direct-mapped
candidate cache of the selected zoom corner rather than retaining a full block volume. A cache miss
uses the same seeded zoom corner and `BiomeCells` sources as the ordinary lookup, while later ore
entries reuse that corner and resolve the typed `BiomeRef`. Membership is then answered by the
compiled numeric feature plan; no names or JSON are involved in the candidate loop.

## How to change it

Keep the source product bounds wide enough to answer the selected neighboring quart at a source
chunk edge. Preserve the fallback for positions outside the generated Y range, and compare the
source-once output digest against the direct lookup before changing the cache layout.

## Configuration

The grid is created only by the source-ordered Overworld replay path. Its lifetime is one replay
request and it has no persistent cache or environment flag. With `gen-counters`, the ore probe
reports candidate lookup, hit, and miss counts for representative source-once runs.
The fixed-size cache hash folds the packed Y coordinate into its slot selection; low X/Z bits
alone must not determine the slot.

## Dependencies

It consumes `BiomeCells`, the seeded Overworld biome zoom helper, and the immutable
`FeatureBiomePlan` owned by the decoration catalog.
