# Worldgen resource front ends

## What it is

The 26.3 resource front end decodes block states and provider resources into the existing canonical state and typed vegetation representations before generation. The 32-bit density engine that evaluates terrain lives in [26.3 density engine](worldgen-engine-26-3.md).

## How it works

`frontend26_3::parse_state` accepts a compact block name or an object with `id` and optional `properties`. Omitted properties use the block's registered defaults. Every supplied property is validated against the canonical state census, so an unknown property or value cannot silently select a default state.

`frontend26_3::ProviderBaker` separates holder references from direct documents. A string passed to `bake_holder` identifies a `block_state_provider` resource; a string inside the resource's direct document identifies a block state. The lookup callback receives a canonical namespaced resource identifier. Named providers are resolved once and cached as typed values. Recursive references, excessive nesting, missing resources and unsupported discriminators return `FrontendError` with a resource path.

Simple, weighted, supported rule-based and weighted-source randomized-integer providers bake into `BlockStateProvider`. Randomized properties become validated canonical-state arrays before placement. Rule providers preserve their optional-result semantics, including no result when no rule or fallback matches. Consumers requiring the current world state on a missing result must supply that behavior explicitly. Nullable nested rule branches are rejected because the current executor does not continue to later rules after an empty child result.

The provider representation currently cannot preserve the random draw count of a randomized-integer provider with a simple source: its weighted-source executor draws a selection even for one entry. That source form is therefore rejected. Noise, dual-noise, noise-threshold, rotated, random-block and copy-properties providers are also rejected until their exact execution contracts are supported. Supported predicate tags are currently the two ground-replacement tags used by the bundled tree providers; other tags and predicate types fail explicitly.

The numeric density engine is a separate module, documented in [26.3 density engine](worldgen-engine-26-3.md). It consumes the bundled JSON directly; this front end does not feed it.

`frontend26_3::MaterialBaker` resolves material rule and condition holders into the existing surface control-flow representation. Its typed graph retains density-root ordinals and load-time noise/random resource bindings; `MaterialInputs` supplies the current request's samples and conditions. Optional ore placement can continue into later material rules when it returns no state. Production surface-context binding remains separate from successful ingress; see [surface control flow](worldgen-surface-cfg.md).

## How to change it

Add state or provider schema support in `frontend26_3` and keep resource resolution at this boundary. Confirm operation order, random consumption and optional-result behavior before extending it. Each new discriminator needs a positive fixture plus malformed and unsupported controls. Do not map an unknown predicate to an always-true predicate.

The integrated generator must pass its current bundle's provider lookup and retain the resulting typed objects. Adding a public module alone does not wire a natural-world generator: feature parsing, material rules, remaining density operations and all three dimension constructors still need migration. Keep one production cache authority, keyed by immutable bundle and execution policy. Once the complete current generator takes over, remove the obsolete natural-world assets and executor paths. Remote protocol compatibility is a separate concern and does not require retaining a second integrated generator.

## Configuration

The provider graph accepts at most 128 nested levels and 65,536 visited nodes per bake call, including randomized-property expansion and cached graph copies. Weighted totals must fit a signed 32-bit integer and be positive. There are no environment variables or runtime JSON lookups.

## Dependencies

The front end uses `serde_json` only at load time, `lodestone-data` for exact canonical state identity, and the existing `feature::vegetation::BlockStateProvider`, `BlockPredicate` and `IntProvider` execution types. The embedding application supplies the selected bundle's immutable resource lookup.
