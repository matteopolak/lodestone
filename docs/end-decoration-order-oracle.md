# End decoration order oracle

## What it is

This fixture-backed check records two independent overlapping End decoration experiments. It proves that source order is observable: an outer island followed by a structure leaves the structure block, while the reverse leaves island material; chorus followed by the fixed platform leaves platform air, while the reverse leaves chorus plant state.

## How it works

`EndCompositeOrderOracle` runs the bundled 26.2 feature implementations in a small in-memory level, captures sorted block-state-map digests, and emits probe cells for the expected and deliberately reversed orders. `end_composite_order.rs` validates the authenticated capture and includes a `should_panic` negative control so a wrong order cannot silently become the expected contract.

## How to change it

Add a bounded overlapping experiment to the oracle and fixture when another ordering seam needs independent evidence. Keep the expected and reversed operation lists, full-map digests, and at least one differing probe together. Regenerate once with `bash scripts/worldgen-oracle/run.sh EndCompositeOrderOracle`; do not use this fixture to justify a production reorder without checking the affected lifecycle path.

## Configuration

The capture uses the repository's 26.2 cache selected by `LODESTONE_MC_CACHE` and the standard oracle container runner. The Rust test has no runtime configuration.

## Dependencies

The oracle needs the bundled 26.2 server jar and its libraries. The Rust test depends only on this crate's integration-test target and the checked-in fixture.
