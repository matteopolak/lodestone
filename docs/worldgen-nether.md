# Nether world generation

## What it is

`lodestone_worldgen::nether::NetherGenerator` produces a complete Nether
column from the bundled noise, biome, feature, tag and structure documents. It
uses the legacy world-generation random family required by the Nether settings.

## How it works

The generator caches the pure pre-decoration prefix (structure refs, fill,
surface, carvers and structure pieces) and its post-ore result by exact chunk
coordinate. It then drives the shared 3×3 feature writers: neighbouring source
chunks can place blocks into the served chunk, exactly as a feature near a
border requires.

The Nether differs from the Overworld at decoration step 7. Each bundled biome
has a mixed list containing springs, fire, glowstone and mushrooms alongside
quartz, gold and debris ores. `NetherGenerator` splits only the placement body:
ore entries use the ore engine, other entries use the configured-feature
interpreter. Both retain their original step-7 array index and use step 7 in
their feature seed. Step 9 then runs the usual vegetal interpreter. This avoids
the tempting but wrong approach of filtering the list before seeding, which
changes every later ore stream.

The reference world at
`.cache/mc/survival/world/dimensions/minecraft/the_nether/region/` supplies
the biome and bedrock-shell oracle fixture used by `nether_gen`. The feature
consumer control compares those same recorded full chunks with a resolver that
withholds feature documents; it must observe live production block changes,
while the cached bedrock masks remain exact.

## How to change it

When adding a Nether feature body, keep its entry in the mixed list even if the
body is not supported yet. Unsupported entries still own a raw feature index;
removing one shifts the seed of every entry after it. Extend the shared feature
parser/body, then let `build_nether_feature_lists` route the new type rather
than adding a Nether-specific algorithm.

Do not use the Overworld's step-6 ore resolver for Nether step 7, and do not
replace `LegacyRandomSource` with xoroshiro in either Nether feature pass. Both
errors create plausible terrain with a different world layout.

## Configuration

There are no runtime flags. `noise_settings/nether.json` selects
`legacy_random_source`; the biome documents under
`crates/lodestone-server/assets/worldgen/biome/` select placed features and the
configured/placed-feature documents provide their bodies and placement
modifiers.

## Dependencies

`lodestone-worldgen`'s `compose`, `feature`, `feature::vegetation`,
`feature::region_view`, carver, surface and structure stages; bundled assets in
`lodestone-server`; and the cached external Nether region oracle described
above.
