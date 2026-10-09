# Versioned worldgen inputs

## What it is

`crates/lodestone-worldgen/tools/worldgen_asset_inventory.py` builds a compact comparison of the cached official 26.2 and 26.3 server datapacks from each version's server jar and generated registry report: resource kinds, schema discriminators, cross-document references, deltas and source digests, without copying asset JSON. The committed result `crates/lodestone-worldgen/tools/worldgen-26.2-26.3-inventory.json` is provenance and migration input, not a runtime bundle or a parity claim.

## How it works

The script reads JSON under `data/minecraft/worldgen/` plus dimension and dimension-type definitions, and summarises document counts, identifiers, discriminator histograms and resource-ID occurrences. It also compares the `minecraft:worldgen/*` registries in each report. Jar SHA-256s and sorted per-document manifest digests bind the summary to its inputs. A resource ID present in several registries lists each matching kind as a candidate reference, not a resolved schema field.

The inventory covers 967 documents in 26.2 and 1,148 in 26.3. Main shifts:
- 26.2's 226 configured-feature and four configured-carver documents become 26.3's 240 feature and four carver documents; `configured_*` kinds disappear.
- 26.3 adds eight block-state-provider, eight material-condition and 42 material-rule documents; its noise settings reference a material-rule resource where 26.2 embeds the surface rule.
- 26.3 adds a biome, 18 structures, 57 template pools, 11 placed features and one structure set (dappled forest, abandoned camps).
- The registry report renames the carver, feature, material-condition and material-rule type registries and adds entries to block-state-provider, density-function, placement-modifier, structure-placement, tree-decorator and trunk-placer registries (exact counts and examples are in the inventory).

Worldgen block states are symbolic, not portable numeric IDs. 26.2 uses `Name` plus optional `Properties` objects; 26.3 providers may use namespaced IDs and `{id, properties}`, with features referencing provider resources. Resolve these against the selected version's semantic catalog when baking; never treat a version report's ID as a cross-version `StateId`, since the canonical union can insert states. The inventory counts symbolic forms and checks numeric ordinal fields (neither version has any). It checks a literal JSON witness in each jar; missing input, malformed JSON or excessive nesting fails, a missing jar or report exits 2, and runtime decoding must likewise fail on an unsupported discriminator or unresolved reference rather than default or skip.

Asset deltas describe data and schema only. The 26.3 release notes' faster chunk generation and structure location are algorithmic and need dedicated oracle follow-ups before claiming parity.

## How to change it

```sh
python3 crates/lodestone-worldgen/tools/worldgen_asset_inventory.py \
  --out crates/lodestone-worldgen/tools/worldgen-26.2-26.3-inventory.json
```

Alternate inputs: `--jar-262`, `--jar-263`, `--report-262`, `--report-263`. Output caps examples and never emits document bodies. Update witness paths and values only after independently checking the source documents; changed digests do not mean behavioural equivalence.

The first safe typed-runtime slice is decoding and baking 26.3 block-state-provider documents into the version-neutral provider behaviour, resolving names and properties once into the canonical catalog, and carrying typed feature and provider IDs plus resolved `StateId`s into generation so the hot path joins no strings. Unknown provider variants and unresolved states are load errors. It excludes noise algorithms, structure placement and the performance changes.

## Configuration

Defaults: `.cache/mc/26.2/versions/26.2/server-26.2.jar`, `.cache/mc/26.3/versions/26.3/server-26.3.jar` and the matching `generated/reports/registries.json`. Example cap and document roots are script constants.

## Dependencies

Python 3 standard library only (jars read as ZIP); no Rust build, package or network.
