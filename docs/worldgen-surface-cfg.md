# Compiled surface-rule evaluation

## What it is

The surface stage parses data-defined rule trees once and evaluates them through
a compact continuation graph during column scans. The graph preserves the
recursive rule's short-circuit behavior while removing per-block call-stack and
tree-walk overhead.

## How it works

Sequences are lowered from right to left, so each condition node has explicit
success and fallback edges. Conditions remain in a shared immutable arena;
their lazy X/Z and Y cache slots, biome callback, heightmap reads, and random
factory are unchanged. The recursive evaluator remains available inside the
module as a parity control, while production scans use the compiled entry.
The production column path disables only the Y-slot memo itself: a compiled
path is acyclic and visits each condition node at most once for a block, so a
Y-cache lookup cannot hit there. X/Z column memoization and all top-material
cache behavior remain enabled.

Typed biome answers from a position callback expire at every Y update, just
like other position-dependent predicates. Fixed biome answers supplied by
`top_material_typed` remain valid for that isolated lookup. A column can cross
biome boundaries vertically; retaining its first typed answer changes the
surface rule at later heights.

The region biome callback can omit the positional zoom when all eight possible
quart corners are already resident and carry the same built-in biome identity.
It uses the same shifted quart parent and vertical clamping as the zoomed
lookup. Mixed identities or any missing cell keep the original lookup, including
its stateful exterior climate search. The omitted zoom offsets depend only on
seed and coordinates; populating their request-local cache has no random-stream
or biome-search effect. This proof is checked anew at each requested Y and does
not retain a biome answer across heights.

`biome` conditions compile generated built-in names into a two-word canonical
biome bitset. Names outside that registry are retained in an ordered fallback
vector for extension registries, so built-ins avoid repeated string-set scans
without changing extension matching; each position resolves its supplied name
to the typed built-in id at most once. Packed stone spans assert the default
state invariant in debug builds once per span; release scans therefore do not
perform a second state read for every block. Both optimizations leave the
column, descending-Y, short-circuit, and random-draw order unchanged.
For a stone span, the next ceiling is its bottom row. Using the row above it
changes the depth-below condition at the span boundary.

## How to change it

Extend `RuleParser` and both evaluators together when adding a rule or
condition. Keep the fallback edge as the next rule in a sequence and add a
focused control that compares compiled and recursive results, including any
observable callback or random-draw order. Do not put mutable scan state in the
compiled graph.

Keep `RegionBiomeSidecar::uniform_resident_biome_at` conservative when changing
biome storage or zoom candidates. Every possible selected cell must be resident
before returning an identity; consulting the dynamic table during this proof
would advance its tie cursor. The `uniform_resident_surface_biome` tests compare
the production callback with the zoomed control and check zero zoom work against
mixed-corner and missing-cell controls.

## Configuration

There are no runtime flags. The graph is built by `SurfaceSystem::new` from the
dimension settings and is reused for the lifetime of that system.

## Dependencies

The graph uses `SurfaceSystem`'s existing condition cache, density/noise
objects, biome and heightmap callbacks, and interned `StateId` results. The
surface parity fixtures remain the external output check. The ignored
`surface::tests::surface_kernel_profile_shaped_fixture` test provides a fixed
seed-42 shaped/full characterization with an exact SHA-256 output digest and,
on macOS, retired-instruction and cycle counters; run it with
`cargo test -p lodestone-worldgen --release --lib surface::tests::surface_kernel_profile_shaped_fixture -- --ignored --nocapture`.
