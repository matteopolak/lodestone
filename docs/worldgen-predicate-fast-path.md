# World-generation predicate fast path

## What it is

`StatePredicate` stores canonical built-in defaults and exact overrides, then caches their answers as compact state-id bitsets. The structure feature placers' solidity and support checks (`VegTags::solid`, `SimpleBlockSupport`) therefore test a canonical integer instead of hashing or comparing a state string; unsupported extension states are rejected at ingress.

Vegetation's `matching_block_tag` predicate follows the same boundary rule: its closed registry-name set is decoded once into a compact `Tag` enum. Placement attempts use that enum directly, while an unknown tag is represented explicitly and fails closed.

## How it works

The resolver facts are parsed once into `lodestone_data::block_states::StateId` defaults and exact overrides. `StatePredicate::bind` walks the generated state table once, evaluates the typed exact-state/default-state semantics, and publishes an answer bit plus a built-in-eligibility bit. `test_id` reads those bits; unsupported extension IDs fail closed.

Text exists only while decoding resolver configuration and in test fixtures.

`BlockPredicate::parse` first decodes the closed `type` discriminator through the internally tagged Serde enum `PredicateDocument`. Its `matching_block_tag` payload is another Serde-renamed enum, so both the predicate kind and tag id cross the JSON boundary as types. `BlockPredicate::test` then performs one `tag_at` lookup and does not repeat the former string comparison chain for each candidate position. Unknown discriminator or tag values take the explicit unsupported or fail-closed fallback.

## How to change it

Keep `bind` at a placement boundary rather than inside a cell loop. Do not remove the built-in eligibility bit: a plugin state must not be treated as a generated-table state merely because its text resembles a built-in base name.

When supporting another `matching_block_tag`, add its `Tag` variant and membership rule in `feature::vegetation::ids`, then add the Serde-renamed boundary variant in `feature::vegetation::config::BlockTagDocument`. Do not add a string comparison in `BlockPredicate::test`; unknown ids deliberately remain `None` and match no blocks.

The cache is per predicate and indexes the canonical generated state table. Cloning a predicate intentionally starts with an unbound cache so its answers are rebuilt from the same immutable table.

## Configuration

No flags or environment variables control the path. `StatePredicate::test_id` is the runtime API; `VegTags::bind` prepares the tag bitsets once per structure placement.

## Dependencies

The path relies on `lodestone_data`'s canonical state table and the resolver's block-capability facts. `StatePredicate` lives in `lodestone_worldgen::feature::state_predicate`.
