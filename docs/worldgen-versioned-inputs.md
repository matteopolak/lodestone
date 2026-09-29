# Versioned worldgen inputs

This document describes a bounded comparison of the cached official 26.2 and 26.3 server datapacks. The inventory records resource kinds, schema discriminators, cross-document references, deltas, and source digests without copying the asset JSON.

## What it is

`crates/lodestone-worldgen/tools/worldgen_asset_inventory.py` builds a compact inventory from each version's server jar and generated registry report. The committed JSON result is `crates/lodestone-worldgen/tools/worldgen-26.2-26.3-inventory.json`; it is provenance and migration input, not a runtime asset bundle or a parity claim.

## How it works

The script reads JSON entries under `data/minecraft/worldgen/` plus dimension and dimension-type definitions from each versioned server jar, parses them, and summarizes document counts, identifiers, discriminator histograms, and resource-ID occurrences matching documents in the inventory. It also compares the `minecraft:worldgen/*` registries in each generated registry report. Jar SHA-256 values and sorted per-document manifest digests bind the summary to its inputs. If the same textual resource ID exists in multiple registries, the inventory lists each matching target kind; these are candidate references, not proof that a particular schema field resolves to each target.

The current inventory covers 967 documents in 26.2 and 1,148 in 26.3. The largest data-model shifts are:

- 26.2's 226 configured-feature documents and four configured-carver documents are represented by 26.3's 240 feature documents and four carver documents. The `configured_*` resource kinds disappear.
- 26.3 adds eight block-state-provider documents, eight material-condition documents, and 42 material-rule documents. Its noise-settings documents refer to a material-rule resource; 26.2 embeds the surface rule in each noise-settings document.
- 26.3 adds a biome, 18 structure definitions, 57 template pools, 11 placed features, and one structure set. New feature and structure data describes the dappled forest and abandoned camps.
- The registry report renames the carver, feature, material-condition, and material-rule type registries. It adds entries to block-state-provider, density-function, placement-modifier, structure-placement, tree-decorator, and trunk-placer registries. Exact counts and bounded ID examples are in the generated inventory.

Worldgen block states are symbolic inputs, not portable numeric state IDs. In 26.2, many state literals use `Name` and optional `Properties` objects. In 26.3, provider data can use namespaced block IDs and `{id, properties}` objects, and feature documents reference provider resources. Resolve those forms against the selected version's semantic block-state catalog while baking the input. Do not treat an ID from a version-specific block report as a cross-version `StateId`; the canonical union can insert states and shift existing ordinals. The inventory counts symbolic forms and checks explicit numeric ordinal fields; neither version's documents currently contains one.

The script checks a literal JSON witness in each jar. Missing input, malformed JSON, or excessive nesting fails inventory generation; a missing required jar or registry report exits 2 with a diagnostic. Runtime decoding must likewise fail explicitly on an unsupported discriminator or unresolved reference; it must not silently substitute a default or skip the document.

Asset deltas describe data and schema only. The 26.3 release notes separately report faster chunk generation and structure locating. Those are algorithmic behavior changes that cannot be inferred from this JSON inventory; keep them as dedicated oracle follow-ups before claiming a port or parity. See [Minecraft Java Edition 26.3 release notes](https://www.minecraft.net/en-us/article/minecraft-java-edition-26-3).

## How to change it

Run the script from the repository root after changing the cached jars or reports:

```sh
python3 crates/lodestone-worldgen/tools/worldgen_asset_inventory.py \
  --out crates/lodestone-worldgen/tools/worldgen-26.2-26.3-inventory.json
```

Alternate inputs can be supplied with `--jar-262`, `--jar-263`, `--report-262`, and `--report-263`. The output caps examples per changed set and never emits asset document bodies. Update the expected witness paths/values only after independently checking the corresponding source documents. Do not interpret changed digests as behavioral equivalence.

The first safe typed-runtime slice is to decode and bake 26.3 block-state-provider documents into the existing version-neutral provider behavior, resolving names and properties once into the canonical semantic state catalog. Carry typed feature/provider IDs and resolved `StateId`s into generation so the hot path performs no string joins. Unknown provider variants and unresolved block states should be explicit load errors. This slice does not cover changed noise algorithms, structure placement behavior, or the reported performance changes.

## Configuration

The defaults expect `.cache/mc/26.2/versions/26.2/server-26.2.jar`, `.cache/mc/26.3/versions/26.3/server-26.3.jar`, and the matching `generated/reports/registries.json` files. CLI flags can point at alternate cached inputs. The output example cap and document roots are script constants.

## Dependencies

The inventory uses Python 3's standard library only. It reads official server jars as ZIP files and the corresponding generated registry reports; it does not need a Rust build, third-party package, network access, or runtime integration.
