# Server-side light

## What it is

How the integrated server computes the sky and block light it puts on the wire for a served chunk,
and how it keeps that light current after a block is placed or broken, rather than only computing
it once at the moment a chunk is first sent.

## How it works

### The engine and the census

Sky light and block light share one flood-fill algorithm, descending one level per step from a
source cell, differing only in what seeds it: sky light seeds every cell open to the sky at the
maximum level, block light seeds every cell whose block actually emits light. The engine takes its
per-block-state light behavior (how much a block dampens light passing through it, and how much it
emits on its own) through an injected lookup table built from a real per-block-state census for this
game version, so the lighting engine itself stays free of any registry or version dependency. Two
entry points exist: computing a column in isolation, and computing it against a real 3×3
neighborhood — the isolated compute is exact for everything except a thin band near the column's own
edge, since light decays fast enough that nothing more than one chunk away can ever reach into it.

**An absent light section on the wire means full daylight, not darkness.** A section present in
neither the sky nor the block light data is resolved by a real client to that dimension's own
default (maximum, for the overworld) rather than treated as zero — which is exactly why sending no
computed light at all (the state before this subsystem existed) produced a uniformly *bright* world:
lit caves, lit sealed rooms, no real night, rather than the reverse.

### Keeping light current after an edit

A block edit that changes what a cell emits (placing or breaking a torch, a lit furnace, glowstone)
or what it dampens (breaking a solid block open to reveal a shaft, or sealing one) triggers a real
recompute-and-resend of the affected column's light, over the same dedicated light-update wire
message a real client already merges into its own view. The predicate deciding whether an edit is
worth this cost compares the two blocks' **resolved** emission and dampening values, not their raw
state strings — a decorative-only change (a block's rotation, an unrelated property) must never
trigger a resend, while a change that alters neither the string's obvious "look" nor its emission but
does change its dampening (a block swapped for one of equal opacity but different type) still must,
since sky light crossing a boundary depends on dampening, not on emission alone. Getting this
predicate narrowed to emission alone was a real, owner-visible bug: breaking a tree trunk (which
emits nothing, same as the air replacing it) darkened nothing on the wire even though a real shaft of
daylight had just opened up, because the check never looked at what the edit had done to *occlusion*.

### The cross-chunk seam, and why it's still open

The encoder that produces a column's light has no access to the chunk source its neighboring columns
live in, so it always runs the **isolated** compute — accurate everywhere except a thin band near a
column's own border, where the true answer depends on terrain in the neighboring column. Measured
against an exact 3×3 compute, the residual is small (most served columns are unaffected; the worst
observed case affected a small fraction of one column's cells) and its direction is a hard invariant:
the isolated compute is never brighter than the correct one at a border, only occasionally darker —
which matters because it means the visible defect is a barely-perceptible dark seam, never a light
leaking somewhere it shouldn't. Closing this properly needs the light to be computed where a real
neighborhood is already available (inside the chunk source, alongside generation or loading) and
carried on the loaded column itself, rather than recomputed in isolation at encode time. The one trap
in that plan is real and worth stating plainly: **if a chunk column ever caches its own precomputed
light, every code path that writes a block into an already-cached column must invalidate that cached
light.** A block write that lands in a retained column without touching anything derived from it
would otherwise serve visibly correct-looking wire bytes computed from stale light — a client that
re-meshes and shows no visible change at all, which is a far harder defect to notice than an error.

Today, a relight resend is likewise the isolated compute and reaches only the connection that caused
the edit — a second player sharing the same world does not see the corrected light on their own
screen until they leave and re-enter that column.

### Validated against a real, already-lit vanilla world

The engine's correctness is checked against a real vanilla server's own generated-and-lit world data
— the world's stored sky and block light arrays, computed entirely independently of this project's
own terrain reader, so neither the input blocks nor the expected light output came from this code.
Sky light agrees with vanilla's own values completely on real, laterally-varied terrain (chunks whose
light is not merely straight-down attenuation through open sky or water, which would trivially agree
under almost any implementation); the small residual disagreement in block light is a documented gap
in the per-block-state emission census for a couple of specific block types, not a defect in the
propagation algorithm itself — and the same governing invariant applies here as at the chunk border:
this project's light is never *brighter* than a real vanilla server's own answer, only occasionally
too dark where a census entry is still missing, which is the safe direction for a gap of this kind to
fail in.

## How to change it

- **Adding or correcting a block's light behavior**: update its emission/dampening entry in the
  per-block-state census, from the real per-version data source, not from a value inferred by
  analogy with a similar-looking block.
- **Closing the cross-chunk seam**: move the compute into the chunk source (generation and load),
  where a real neighborhood already exists, cache the result on the loaded column, and make every
  block-write path on that column invalidate the cached light — do this together, not as two
  separate changes, or a stale cache will silently start shipping.
- **Do not widen the relight predicate to fire on every placement "to be safe."** A resend recomputes
  and re-sends a whole column's worth of data; firing it on every ordinary, non-light-relevant edit
  (a block swapped for another of identical light behavior) turns routine building into a stream of
  unnecessary full-column resends.
- **Never compute light on the client's own live (multiplayer) path.** The client's contract is that
  a live server connection supplies light and a local/offline world computes its own — this
  subsystem exists precisely so the server side of that contract is actually held up.

## Configuration

None. Light is computed unconditionally for every served column and on every light-relevant edit;
there is no feature flag or setting that disables it. The per-block-state census is fixed data for a
given game version, regenerated from its real source only when that version changes.

## Dependencies

- The shared world-storage crate's lighting engine and light-data wire format.
- The generated per-block-state light census (dampening and emission), and the block-state
  name/property census it is keyed through.
- The chunk source/column types the light is computed over — see `docs/chunk-storage.md` and
  `docs/chunk-lifecycle.md`.
- A real vanilla server (for the oracle comparison only) and the pinned game-version data sources
  behind the census; neither is required for the engine to run in production.
