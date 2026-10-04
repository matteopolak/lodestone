# Canonical State Census

## What it is

The generator-side canonical census defines stable numeric identities across the official 26.2 and 26.3 block, block-state, and item reports. It supplies the append-only runtime union; the census command itself does not install tables or enable protocol support.

## How it works

`crates/lodestone-data/tools/canonical_census.py` keeps all 26.2 identities at their existing numeric IDs and appends entries introduced in 26.3 in that release's wire order. Block-state identity consists of the resource name and sorted property pairs; block and item identity consists of the resource name. The array index in each domain's `keys` column is its canonical ID.

Each version has numeric `wire_to_canonical` and `canonical_to_wire` arrays. An unsupported outgoing identity is JSON `null`, never an alias to air or another entry. The generator exhaustively checks the old prefix, appended order, source coverage, both mappings, and contiguous per-block state spans. Duplicate JSON keys, duplicate IDs, sparse registries, missing old identities, and discontiguous canonical block spans fail explicitly.

The manifest includes SHA-256 hashes of the exact input report bytes. It has no timestamp or absolute input paths, so identical inputs produce identical bytes. Hashes establish which local reports were used; they do not authenticate a downloaded release by themselves.

Text joins happen only during generation. Runtime consumers use generated numeric columns and existing typed identities. This workflow does not add strings to rendering, physics, or world-generation loops. The adopted union contains 1,286 blocks, 35,723 states, and 1,658 items, retaining the original 1,196/32,366/1,537 prefixes.

## How to change it

Keep canonical IDs independent of new releases' registration order. Extend the generator's version policy and tests together when adding another release. Do not replace the base with the newest report: native storage and existing adapters rely on the old canonical prefix. A release adding states to an existing block may require multiple state spans; the current generator refuses that case instead of breaking `Properties::state_for_block`.

The [behavior union emitter](./data-behavior-codegen.md) populates every total identity-indexed table before installing the larger runtime census. Collision, outline, lighting, survival, sound, tool, and item-prototype data require their own authoritative inputs; these two JSON reports do not establish those behaviors. Shared identities also do not prove identical behavior between versions. `registry_union_names` extends report-derived fixed registries with the same prefix policy and refuses removed identities; sound events use it without weakening that check.

Biomes are deliberately outside this manifest: their canonical IDs come from a separate generated domain, and wire ordinals arrive through dynamic registries. Their future append-only policy must preserve stored IDs while maintaining a separate sorted name lookup. Native storage version handling, Anvil compatibility, 16-bit direct block palettes, rendering assets, and adapter ingress/egress remain separate integration work.

## Configuration

Python 3's standard library is sufficient. Default inputs are `blocks.json` and `registries.json` under `.cache/mc/26.2/generated/reports` and `.cache/mc/26.3/generated/reports`. Override directories with `--base-reports` and `--latest-reports`; these flags change input locations, not the fixed version policy.

From the repository root:

```sh
python3 crates/lodestone-data/tools/canonical_census.py
python3 crates/lodestone-data/tools/canonical_census.py --output /tmp/lodestone-canonical-census.json
python3 crates/lodestone-data/tools/canonical_census.py --check /tmp/lodestone-canonical-census.json
python3 crates/lodestone-data/tools/test_canonical_census.py
python3 crates/lodestone-data/tools/test_canonical_census.py --official-reports
```

The default command validates and prints counts plus a manifest digest. `--output` creates a new file and refuses to overwrite one; `--check` requires byte-for-byte agreement with an existing artifact. Missing required reports fail rather than skipping validation.

The small hermetic suite checks malformed input and deliberate mapping corruption. `--official-reports` additionally exhausts the cached releases and checks separately recorded unchanged, shifted, and new-only witnesses. Its negative controls substitute an identity mapping for shifted water and give a new-only item an invalid 26.2 egress mapping; each must be rejected. Round trips alone are not the oracle.

## Dependencies

This workflow depends on the official generated reports and Python's standard library. It does not build Rust, start a server, install runtime tables, or write storage records. Runtime integration depends on the behavior union emitter, version adapters, asset baking, and the native storage census revision.
