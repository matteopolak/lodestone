# Clamped-normal placement offsets

## What it is

Clamped-normal integer providers supply the non-uniform offsets used by cave
placed features such as sulfur spikes and pointed dripstone. They are part of
the shared placement-provider model, so every feature that uses the same JSON
form receives the same sampling rule.

## How it works

`IntProvider::ClampedNormal` takes one Gaussian value from the caller's current
`RandomSource`, converts it to `f32`, applies `mean + value * deviation`, clamps
that result to the inclusive integer bounds, then truncates toward zero. A
`random_offset` samples its horizontal provider for x, its vertical provider for
y, and the horizontal provider again for z; each sample consumes the wrapper's
next Gaussian value, including its cached paired value.

The external fixed-seed control in
`feature::vegetation::tests::clamped_normal_offsets_match_compiled_server_samples`
covers seeds 0, 1, 2, 19, and 42. The sulfur-spike parser control also proves
that the offset modifier remains in the resolved placement pipeline instead of
being silently omitted.

## How to change it

Add an `IntProvider` variant and its sampling arm before teaching a new placement
record to parse a provider type. Keep `random.next_gaussian()` as the sole source
of normal values: replacing it with uniform draws or constructing a second
random source changes both positions and downstream RNG state. Update the
external-value control with independently captured samples if its numerical
rule changes.

## Configuration

The provider data is an object with `type: "minecraft:clamped_normal"`, `mean`,
`deviation`, `min_inclusive`, and `max_inclusive`. The two bundled cave records
use horizontal `0, 3, -10, 10` and vertical `0, 0.6, -2, 2` values.

## Dependencies

This relies on `lodestone_worldgen_core::rng::RandomSource` for the Gaussian
stream and `lodestone_worldgen::feature::vegetation` for placed-feature parsing
and depth-first modifier execution.
