# Nether scattered ore

## What it is

The Nether scattered-ore feature places independent ore candidates around each
resolved origin instead of growing one connected blob. It is the feature body
used by the Nether's step-7 ancient-debris entry and shares the configured
targets, placement modifiers, and exposure rule with standard ore.

## How it works

`feature::PlacedScatteredOre` carries the parsed body and its placed-feature
modifiers. `nether::build_nether_feature_lists` recognizes the scattered body
without removing it from the mixed step-7 list, so every entry keeps its raw
array index. The 3×3 source pass derives each source's decoration seed and
then applies the explicit step-7 feature seed for that index. Standard and
scattered entries therefore retain the same stream position while dispatching
to different bodies.

The body first draws a bounded attempt count from `size + 1`. Each attempt
draws two floats for each axis, rounds the difference with half-up semantics,
and limits the attempt distance to seven blocks. A candidate is written only
when the shared target rule and discard-on-air-exposure check accept it. The
focused fixture captures the final ancient-debris cells at local `(2, 21, 13)`,
`(2, 21, 14)`, and `(2, 86, 3)` for seed `42` and source chunk `(-8, -8)`.
Its independent packet comes from the full Nether oracle export, so it
includes the same structure-bearing pre-decoration prefix and mixed
source-completion lifecycle as `NetherGenerator::column`.

## How to change it

Keep the configured body dispatch and the placed-feature seed/index handling in
sync when adding another scattered entry. Preserve the order of the two float
draws per axis and the half-up rounding rule; changing either changes candidate
coordinates without necessarily changing the total count. Extend the shared
target/exposure path rather than duplicating its predicates in the Nether
module.

The full-column test and its standard-ore negative control are the focused
controls. Keep the `scope full-column` fixture marker synchronized with the
production entry point: a post-ore feature trace is not comparable to a
packet-ready column.

## Configuration

There are no runtime flags. The configured and placed feature documents under
`crates/lodestone-server/assets/worldgen/` select the body, target tag,
attempt size, exposure chance, and placement modifiers. The focused full-column
capture uses seed `42`, Nether step `7`, and source chunk `(-8, -8)`.

## Dependencies

The implementation depends on `lodestone_worldgen::feature` for ore parsing,
placement walking, target rules, and region writes; `lodestone_worldgen::nether`
for the mixed 3×3 source pass; and the bundled world-generation assets. The
focused test consumes an independently captured sealed packet exported by
`scripts/worldgen-oracle/large-parity.sh`; the same packet path is used by the
external end-to-end parity gate.
