# Worldgen root-placement hot path

## What it is

Mangrove root placement simulates four directional paths before committing any
root blocks. The implementation keeps the simulation's candidate and aggregate
position order stable while avoiding per-direction heap storage and transient
state-name strings.

## How it works

Each direction appends its accepted candidates directly to the tree's reusable
root-position scratch vector. A length mark brackets the direction, so a failed
simulation truncates only that direction's candidates before the whole tree is
rejected. Candidate generation returns two optional stack slots; the first and
second slots preserve the original candidate order and all random draws.

When choosing between ordinary and muddy roots, the current grid state is read
as a `StateId`, reduced to its typed base id, and compared with interned ids for
the configured names. This preserves base-state matching without allocating a
temporary `String` for every candidate.

## How to change it

Keep the direction order, candidate slot order, and rollback mark intact: all
three affect the generated write stream and subsequent random draws. If a
simulation can gain a third candidate, enlarge the fixed result before changing
back to a heap collection. Any new state comparison should use the grid's typed
id accessors rather than converting a state to text in the placement loop.

## Configuration

Root behavior comes from the configured tree's root-placer fields: trunk offset,
maximum width and length, skew chance, root providers, and muddy-root inputs.
The reusable scratch storage is thread-local and has no environment settings.

## Dependencies

The path uses `VegGrid`, `VegTags`, `RootPlacerCfg`, and the shared
`StateInterner`; candidate randomness is supplied by the world-generation
`RandomSource` abstraction.
