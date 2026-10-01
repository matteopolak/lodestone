# End decoration order oracle

## What it is

This fixture-backed check records two independent overlapping End decoration experiments. It proves that source order is observable: an outer island followed by a structure leaves the structure block, while the reverse leaves island material; chorus followed by the fixed platform leaves platform air, while the reverse leaves chorus plant state.

## How it works

`EndCompositeOrderOracle` runs the bundled 26.2 feature implementations in a small in-memory level, captures sorted block-state-map digests, and emits probe cells for the expected and deliberately reversed orders. `end_composite_order.rs` validates the authenticated capture and includes a `should_panic` negative control so a wrong order cannot silently become the expected contract.

The source replay storage control in `end::tests` independently reconstructs
the dense target window and compares its complete result with borrowed replay.
It covers seed 42, source `(6,0)`, targets `(6,0)` and `(7,1)`, with the fixed
platform's obsidian witness `(100,48,0)`, and seed -195764831, source and target
`(45,-115)`, with the city's purpur witness `(720,60,-1825)`. Each installs an
override at the witness, an untouched override inside the target window, and
an override outside it. Equality includes sorted net spills, gateway sidecars,
and ordered structure writes; the oracle supplies the separate feature-order
contract. These controls do not establish external parity for other lifecycle
samples.

## How to change it

Add a bounded overlapping experiment to the oracle and fixture when another ordering seam needs independent evidence. Keep the expected and reversed operation lists, full-map digests, and at least one differing probe together. Regenerate once with `bash scripts/worldgen-oracle/run.sh EndCompositeOrderOracle`; do not use this fixture to justify a production reorder without checking the affected lifecycle path.

When changing storage, keep the reference replay's original dense construction
independent of `EndGenerator::parity_decoration_grid_for_target`. The focused
grid test covers unequal source heights, restored writes, palette history, and
mutation ordinals. The isolated `end_borrowed_region_counters` test rejects the
old 589,824-cell stitch and predicts clipped platform writes for both targets.
Its `gen-counters` feature must be enabled to run the counter gate.

## Configuration

The capture uses the repository's 26.2 cache selected by `LODESTONE_MC_CACHE` and the standard oracle container runner. The Rust test has no runtime configuration.

## Dependencies

The oracle needs the bundled 26.2 server jar and its libraries. The Rust test depends only on this crate's integration-test target and the checked-in fixture.
