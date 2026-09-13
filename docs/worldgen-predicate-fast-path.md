# World-generation predicate fast path

## What it is

`StatePredicate` keeps its existing text lookup for extension values while caching answers for canonical built-in states as compact local-ID bitsets. Repeated top-layer generation predicates therefore test an interned integer instead of hashing or comparing a state string.

Vegetation's `matching_block_tag` predicate follows the same boundary rule: its closed registry-name set is decoded once into a compact `Tag` enum. Placement attempts use that enum directly, while an unknown tag is represented explicitly and fails closed.

## How it works

The resolver facts are parsed once into `lodestone_data::block_states::StateId` defaults and exact overrides. The generator interner records whether each local state resolves to that generated built-in state table. `StatePredicate::bind` walks newly interned IDs once, evaluates the typed exact-state/default-state semantics, and publishes an answer bit plus a built-in-eligibility bit up to a watermark. `test_id` reads those bits for eligible IDs; extension IDs and IDs minted after the last bind call the original string implementation.

The top-layer column scan binds its five predicates once before scanning and uses `DenseBlockGrid::get_id` for motion, fluid-source, and `snowy` checks. This does not change the string-facing methods used by adapters and tests.

`BlockPredicate::parse` first decodes the closed `type` discriminator through the internally tagged Serde enum `PredicateDocument`. Its `matching_block_tag` payload is another Serde-renamed enum, so both the predicate kind and tag id cross the JSON boundary as types. `BlockPredicate::test` then performs one `tag_at` lookup and does not repeat the former string comparison chain for each candidate position. Unknown discriminator or tag values take the explicit unsupported or fail-closed fallback.

## How to change it

Keep `bind` at a generation/pass boundary rather than inside a cell loop. If a new state-producing stage is added, bind again after that stage or leave the new ID on the correctness-preserving fallback. Do not remove the built-in eligibility bit: a plugin state must not be treated as a generated-table state merely because its text resembles a built-in base name.

When supporting another `matching_block_tag`, add its `Tag` variant and membership rule in `feature::vegetation::ids`, then add the Serde-renamed boundary variant in `feature::vegetation::config::BlockTagDocument`. Do not add a string comparison in `BlockPredicate::test`; unknown ids deliberately remain `None` and match no blocks.

The cache is per predicate and per interner instance. Cloning a predicate intentionally starts with an unbound cache because local IDs are only meaningful against the interner that issued them.

## Configuration

No flags or environment variables control the path. `StatePredicate::test` remains the direct text API; `StatePredicate::test_id` is the numeric API, and `SnowSupport::bind` prepares the top-layer predicates.

## Dependencies

The path relies on `StateInterner`'s canonical binding, `DenseBlockGrid`'s local state IDs, and the resolver-backed text maps that remain the extension fallback.
