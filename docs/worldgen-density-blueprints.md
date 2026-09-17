# Density blueprints

## What it is

The density blueprint cache separates immutable resolver data from seed-specific
noise and evaluator state. It shortens repeated construction of a generator
without sharing mutable memo tables or changing density output.

## How it works

`Builder::build` uses a resolver fingerprint to look up a validated blueprint
keyed by the exact root JSON. Density references are expanded in the original
depth-first order while the blueprint is created. Instantiation then recreates
the density tree for each seed, allocating interpolation slots and point memo
ids in the same order as the uncached builder and seeding every noise from that
builder. A resolver without `asset_fingerprint` always uses the uncached path.

The compiled block-field graph omits `cache_2d` and marker wrappers. Both are
transparent during field evaluation; retaining them only added a dispatch for
every sampled block. The point evaluator still owns the original density tree,
so its marker and point-cache behavior is unchanged. `NoiseChunkSampler` also
offers `final_density_column` for callers that walk a contiguous vertical run:
it keeps one field context and reuses the fixed X/Z interpolation coordinates.
`NoiseChunkRegionSampler` extends that boundary across a request-scoped block
rectangle. A single bounded scratch then shares interpolation corners between
adjacent columns while callers retain independent aquifer status and fluid
caches. The region is dropped after its products are published rather than
retained as a generator-wide cache. For the production 4×8×4 final-density
root, `Program` also records the exact `min`/`squeeze`/interpolated/noodle
operator shape and its dynamically assigned interpolation slots. The bounded
sampler can then fill one 128-value cell with X-inner/Y/Z lerps: it evaluates
the control interpolation first, masks the out-of-range blocks, and skips the
three conditional noodle interpolators when that mask is empty. The Overworld
 fill loop opts into this cell path only for aligned 4×8 programs; other roots
 and dimensions retain the column path.

## How to change it

Extend `BlueprintNode::parse` and `BlueprintNode::instantiate` together when a
density node is added. Match the existing builder's child traversal order,
especially for interval selectors, because that order controls slot allocation
and seeded noise creation. Add a digest comparison and a dynamic-resolver
negative control before changing the cache key or sharing an instantiated
product. Use `NoiseChunkRegionSampler::from_program` only when the caller can
state a complete inclusive query rectangle; it rejects out-of-bounds vertical
runs and does not provide a substitute for unbounded point samplers. If the
 final-density root changes, update the exact graph matcher and its cell-vs-
 column equivalence controls together. Keep the per-cell mask and fixed
 four-channel corner carrier local to the evaluator so the region sampler does
 not retain a tile; the active-lane output loop must retain the control,
 ridge-a, ridge-b, then thickness cache-write order.

## Configuration

No runtime flag selects the cache. `Resolver::asset_fingerprint` opts an
immutable asset bundle in; returning `None` keeps dynamic datapack resolvers
uncached. The cache is process-local and keyed by the resolver fingerprint and
root JSON.

On the release fixture probe, a repeated 6x6 generator construction fell from
86.4 ms on the uncached path to 70.6 ms with a warm blueprint cache (18.3%
less wall time). Populating the cache on the first construction measured 107.5
ms versus 83.7 ms for the uncached control, so the cache is a repeated-world
setup optimisation rather than a first-construction shortcut. Both paths
produced the same density digest, noise signature, and slot sequence.

## Dependencies

The cache is implemented in `lodestone-worldgen-core::density` and is consumed
by the worldgen builders. Region sampling depends on the compiled `Program`,
the bounded `Scratch`, and the caller's stage-local aquifer. It depends only on
`serde_json` and the existing density, noise, RNG, and memo implementations.
