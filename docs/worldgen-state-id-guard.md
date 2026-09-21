# Worldgen StateId Guard

## What it is

`cargo xtask check-worldgen-state-ids` is a source-level guard that keeps generated block states in canonical `lodestone_data::block_states::StateId` form. It prevents runtime stage and materializer products from reintroducing state text allocations.

## How it works

The guard parses production Rust with `syn`, then checks state-bearing fields, function inputs and outputs, tuple overlays, block-state APIs, and text conversions through canonical formatting or parsing helpers. It reports file, line, owning symbol, and the replacement expectation. External NBT, JSON, protocol, action, display, resource, and test boundaries are explicit exclusions rather than inferred from an arbitrary string search. Reference and nested text types are traversed structurally so `&str`, `Arc<String>`, vectors, and mutation tuples do not evade the check.

Known block-predicate owners and their set/parser APIs are checked by symbol, so typed feature configuration cannot hide a `HashSet<String>` behind a generic field name. Files and items compiled only for tests are omitted from the production census.

## How to change it

Add a narrow symbol or path exclusion only for a real boundary where text is the input or output format. Keep generated stage products, palettes, spills, mutation records, server runtime state, and client block-state classification in the scan. If a new representation is necessary, give it an ID-typed API and test both a seeded violation and the legitimate boundary that must remain text-based.

## Configuration

The guard has no runtime flags. Run `cargo xtask check-worldgen-state-ids` or `just check-worldgen-state-ids` from the repository root. The current tree intentionally reports remaining migrations until all runtime state paths use `StateId`.

## Dependencies

The scanner uses `syn`, `proc-macro2`, `quote`, and the standard filesystem API. It scans production sources in worldgen, server, client, model, and shell crates; no runtime code depends on `xtask`.
