# Canonical State Census

## What it is

The generator-side census that defines stable numeric identities across the official 26.2 and 26.3 block, block-state and item reports. It supplies the append-only runtime union; the census command itself installs no tables and enables no protocol support.

## How it works

- `crates/lodestone-data/tools/canonical_census.py` keeps every 26.2 identity at its existing ID and appends 26.3's new entries in that release's wire order. Block-state identity is resource name plus sorted property pairs; block and item identity is the name. The index in each domain's `keys` column is the canonical ID.
- Each version has numeric `wire_to_canonical` and `canonical_to_wire` arrays; an unsupported outgoing identity is JSON `null`, never an alias of air or another entry. The generator exhaustively checks the old prefix, appended order, source coverage, both mappings and contiguous per-block spans, and fails on duplicate JSON keys, duplicate IDs, sparse registries, missing old identities and discontiguous spans.
- The manifest carries SHA-256s of the exact input report bytes and no timestamps or absolute paths, so identical inputs give identical bytes. Hashes say which reports were used, not that a download is authentic.
- Text joins happen only at generation; runtime uses numeric columns and typed identities, so no strings enter rendering, physics or worldgen loops. The adopted union holds 1,286 blocks, 35,723 states and 1,658 items, retaining the 1,196/32,366/1,537 prefixes.

## How to change it

- Keep canonical IDs independent of a release's registration order; extend the version policy and tests together. Never replace the base with the newest report (native storage and adapters rely on the old prefix). A release adding states to an existing block may need multiple spans; the generator refuses that rather than break `Properties::state_for_block`.
- The [behavior union emitter](./data-behavior-codegen.md) fills every total identity-indexed table before installing the larger census. Collision, outline, lighting, survival, sound, tool and item-prototype data need their own authoritative inputs, and shared identity does not prove shared behaviour. `registry_union_names` extends report-derived fixed registries under the same prefix policy and refuses removed identities (sound events use it).
- Biomes are outside this manifest: canonical IDs come from a separate domain and wire ordinals from dynamic registries; their append-only policy must preserve stored IDs with a separate sorted name lookup. Native storage versioning, Anvil compatibility, 16-bit direct palettes, assets and adapter ingress/egress are separate work.

## Configuration

Defaults: `blocks.json` and `registries.json` under `.cache/mc/26.2/generated/reports` and `.cache/mc/26.3/generated/reports`; `--base-reports`, `--latest-reports` change locations, not policy.

```sh
python3 crates/lodestone-data/tools/canonical_census.py
python3 crates/lodestone-data/tools/canonical_census.py --output /tmp/lodestone-canonical-census.json
python3 crates/lodestone-data/tools/canonical_census.py --check /tmp/lodestone-canonical-census.json
python3 crates/lodestone-data/tools/test_canonical_census.py [--official-reports]
```

The default run validates and prints counts plus a manifest digest; `--output` refuses to overwrite; `--check` needs byte-for-byte agreement; missing reports fail. The hermetic suite checks malformed input and mapping corruption; `--official-reports` exhausts the cached releases against independently recorded unchanged, shifted and new-only witnesses, with negative controls (identity mapping for shifted water, an invalid 26.2 egress for a new-only item) that must be rejected. Round trips alone are not the oracle.

## Dependencies

Official generated reports and the Python standard library; no Rust build, server or storage. Runtime integration depends on the behaviour union emitter, version adapters, asset baking and the native storage census revision.
