# Protocol family census

## What it is

The registry census is the aggregation-level guard for the supported joining families. It checks that every compiled protocol appears once, resolves through the registry, and is claimed by the adapter returned for that protocol.

## How it works

`crates/lodestone-registry/tests/family_census.rs` validates every feature configuration without requiring all families in a default build. Partial builds must expose only known protocol rows with no duplicates. An `--all-features` run additionally requires the complete ten-family and sixteen-protocol set.

Family-specific packet fixtures remain in each `crates/versions/*/tests` directory. The census deliberately stops at registry resolution, so it cannot hide a missing adapter→ECS or adapter→server consumer behind an aggregation assertion.

## How to change it

When adding or removing a family, update `EXPECTED_PROTOCOLS` and `EXPECTED_FAMILIES` together with the registry feature and family table. Run the default test and the `--all-features` test; then add the family’s independent wire and consumer fixture in its own crate.

## Configuration

The test is feature-aware. `cargo test -p lodestone-registry` checks partial/default builds, while `cargo test -p lodestone-registry --all-features` enables the exact full-census assertion.

## Dependencies

The test depends only on the public `lodestone-registry` resolution API and the version adapters enabled by Cargo features. It has no live-server or external-fixture dependency.
