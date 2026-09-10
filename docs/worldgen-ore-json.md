# Ore placement JSON boundary

## What it is

The ore decoration parser decodes placed-feature modifiers and ore target predicates through closed Serde schemas. Known JSON shapes become the existing `Placement` and `RuleTest` runtime enums without retaining discriminator strings.

## How it works

`PlacementJson` accepts the five modifier kinds used by the ore pass: count, rarity filter, in-square, height range, and biome. `RuleTestJson` accepts tag-match and block-match targets. Both enums use namespaced Serde renames plus compatibility aliases for unqualified names, and both reject unknown fields. Nested integer and height-provider documents continue through their established parsers after the outer discriminator has been validated.

`parse_placements` and `parse_ore_config` are the production consumers. They run while resolver data is compiled into the generator, so generation loops receive typed modifiers and target tests rather than repeatedly inspecting JSON keys. Unknown kinds, missing required fields, or extra fields fail at the boundary instead of silently selecting a nearby branch.

## How to change it

Add a placement variant to `PlacementJson`, convert it to the corresponding `Placement` arm, and add a valid fixture or focused schema test. Keep `deny_unknown_fields`: an extra property can otherwise mask a misspelled field while leaving a plausible modifier in the generated pipeline. Add a `RuleTestJson` variant only when the ore target schema and runtime `RuleTest` both support the new predicate.

The nested provider parsers are intentionally separate. Extend those only when the provider's draw semantics and malformed-data behavior have their own evidence; changing a provider is not required to add an outer placement discriminator.

## Configuration

There are no flags or environment variables. The accepted schemas are determined by the `PlacementJson` and `RuleTestJson` enums in `lodestone_worldgen::feature`.

## Dependencies

The boundary uses `serde`/`serde_json` and feeds `parse_placements`, `parse_ore_config`, `PlacedOre`, and the ore placement driver. Runtime placement uses `IntProvider`, `HeightProvider`, and the resolver-backed block/tag data after JSON parsing has completed.
