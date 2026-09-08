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

The noise carrier is 128 rows tall, but Nether vegetal decoration evaluates the
dimension's 256-row resident window. A border source can therefore place a
mushroom in the served chunk at or above y=128. `NetherColumn::decoration_spills`
keeps those upper-window writes separate from the compact terrain carrier, and
`ChunkColumn::from_nether` applies them after padding; otherwise the generated
heightmap would report air one block below the external result.

The prefix values are immutable for a fixed seed and coordinate. Normal
generation keeps a 32-entry demand-ordered memo to bound resident memory, while
large packet replay calls `NetherGenerator::prepare_packet_replay` with its
target coordinates. That helper derives the full target-plus-neighbour closure
from the 5×5 prefix read radius, so a bounded scan does not evict a prefix that
the next packet needs. The `pre_decoration_computations` and
`pre_decoration_evictions` counters make the closure and any thrashing visible
without changing generation decisions.

Biome carvers are normalized by `compose::build_biome_carvers` before that
prefix runs. A biome document may declare one carver id directly or an ordered
array; both forms become the same ordered carver list. Treating the direct form
as an empty array removes the entire cave pass, which leaves solid netherrack
where the generated terrain has cave air and lava.

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

The shared `feature::apply_ore_step_3x3_per_source` wrapper is intentionally
fixed to step 6 for the Overworld. Nether code must call its explicit-step
variant with 7, because the step participates in each feature's seed. Do not
replace `LegacyRandomSource` with xoroshiro in either Nether feature pass;
either error creates plausible terrain with a different world layout.

When changing the biome-document parser, preserve a direct carver id as a
single-element list and preserve array order exactly. The source chunk and list
index seed each carver, so dropping or reordering an entry changes the whole
17×17 carve neighbourhood.

When adding another replay consumer, pass its complete target coordinate list
to the preparation helper rather than choosing a fixed cache size. If the
consumer reads beyond the packet's 3×3 neighbours, expose a separate
coordinate-derived closure helper and add a byte-identity control against the
unprepared source before enabling the larger memo.

## Configuration

There are no runtime flags. `noise_settings/nether.json` selects
`legacy_random_source`; the biome documents under
`crates/lodestone-server/assets/worldgen/biome/` select placed features and the
configured/placed-feature documents provide their bodies and placement
modifiers.

The read-only one-target profile is an opt-in developer probe. Run
`cargo run -p lodestone-v26-2 --example nether_packet_profile` to see the
prefix, full-column, packet and cache-counter timings without starting the
game.

## Dependencies

`lodestone-worldgen`'s `compose`, `feature`, `feature::vegetation`,
`feature::region_view`, carver, surface and structure stages; bundled assets in
`lodestone-server`; and the cached external Nether region oracle described
above.
