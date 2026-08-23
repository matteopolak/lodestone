# Lightning bolt rendering

## What it is

The lightning bolt's own geometry: four concentric hollow square tubes traced
along one seeded random walk, 128 blocks tall, untextured, blended additively.
Distinct from the **sky flash**, which `lodestone-render`'s `weather` module has
carried since long before this landed — and that split is exactly why the gap
survived, because a storm looked like it was doing something.

## How it works

### Everything is client-side, and that is the unusual part

`LightningBolt` overrides `defineSynchedData` with an **empty body**. It
declares no `EntityDataAccessor` at all, and `SynchedEntityData.getNonDefaultValues`
omits anything still at its default, so a bolt puts **nothing** on the wire
beyond its `ADD_ENTITY` position. Its `seed` is a plain `public long` rolled by
`random.nextLong()` in the constructor on each side independently: a vanilla
client's bolt never matches its own server's, and it does not need to.

Two consequences. There is **no decode work** for this feature — the whole chain
is a spawn packet reaching the draw list. And there is **no captured-bytes
oracle** and never can be, which is why the gates are structural invariants (see
Verification).

### The chain

| link | symbol |
| --- | --- |
| seed | `lodestone_render::lightning_bolt::bolt_seed_for_entity` |
| walk | `lodestone_render::lightning_bolt::lightning_bolt_vertices` |
| pass | `lodestone_shell`'s `gpu/lightning_bolt.rs`, `shaders/lightning_bolt.wgsl` |
| counter | `RenderStats::lightning_bolt_vertices` |

### The walk

An **anchor pre-pass** walks eight height levels downward from `h = 7`, stepping
`nextInt(11) - 5` in x and z, recording each level's offset and keeping the total
as `final_x`/`final_z`. Those two exist only to be subtracted — that is what
lands the bottom of the trunk on the strike point.

Then, per **shell** `r` in `0..4`: the random source is **re-created from the
same seed**, so all four shells trace the identical walk and differ only in
width. Four nested tubes around one path, not four paths. That re-seeding reads
as a transcription mistake and is not; it is also why the trunk retraces the
pre-pass exactly, consuming the same sixteen draws in the same order.

Per shell, three **branches**: the trunk (`h` 7 down to 0, step `±5`) and two
forks (`h` 6→4 and 5→3, step `±15`) that re-anchor onto the trunk at their own
height. Four **faces** per segment form the tube. Total `4 × 14 × 4 = 224` quads,
1,344 triangle-list vertices, rebuilt every frame exactly as vanilla's `submit`
does.

Only the trunk **tapers**, widest at the sky. `rr1` pairs with the *upper*
vertex and `rr2` with the lower, and the lower vertex carries the newly-walked
offset while the upper carries the previous one. Both pairings are easy to
transpose and neither transposition is visible in a screenshot; see Verification
for the gate that separates them and for why the obvious one did not.

### The blend function is the whole look

`RenderPipelines.LIGHTNING` carries `BlendFunction.LIGHTNING`, which is
`(SRC_ALPHA, ONE)` — **additive, scaled by the source alpha**. Nothing else in
this workspace uses that pair; the closest, `glint_blend()`, is `(Src, One)`
colour with `(Zero, One)` alpha and its own doc warns that reaching for a stock
`ADDITIVE` is wrong.

It matters because the bolt's own colour is `(0.45, 0.45, 0.5)` at alpha `0.3`
— a dim blue-grey. What makes a bolt read as *white* is four shells each adding
`0.3` of it on top of the last. Under ordinary alpha blending the same geometry
comes out grey and flat, which is a plausible-looking wrong answer: this is the
pass where "it draws but looks dull" means the blend state, not the colour.

### The scale, and why nothing is culled

Segment heights are `h * 16` **in blocks**, so a bolt spans 128 blocks upward
and wanders ±5 blocks a level. `lodestone_data::entity_dimensions` records
`lightning_bolt` as having no hitbox at all, so there is no AABB a frustum test
could use — which is exactly why vanilla's `affectedByCulling` returns `false`
for it. The pass does no culling of its own; the cost ceiling is the
fixed-capacity buffer (eight simultaneous bolts).

## What is deliberately not ported

**The re-roll between flashes.** `LightningBolt.tick` re-rolls `seed` between
each of its `rand(3) + 1` flashes, and that reseeding is what makes a vanilla
bolt visibly snap to a *different shape* mid-strike. Reproducing it needs
per-bolt `life`/`flashes` state on the client, which does not exist here — the
integrated server has exactly that state machine in `lodestone-server`'s
`lightning` module, but nothing carries it to the client and nothing can, since
none of it is on the wire.

So a bolt here holds **one shape** for as long as the server keeps the entity
alive. The seed is derived from the entity id rather than rolled, which is no
less faithful (nothing on the wire constrains it) and is stable across a
reconnect. The larger half of the flicker — the sky flash — already pulses
correctly through `lodestone_render::weather`.

**No fog term**, the same gap `docs/` records for the beacon beam and sign text.
A bolt is meant to read as a bright, distance-visible flash.

## How to change it

* **This cannot be an `EntityPipeline` variant.** All eight of that type's
  variants are built over a camera + **texture** bind-group pair, and seven go
  through a helper that hard-wires an instanced second vertex buffer. A bolt has
  no texture and no instances. It has its own module, its own one-group
  pipeline and its own `.wgsl`.
* **The `java.util.Random` in `lightning_bolt.rs` is the workspace's fourth
  copy** (`lodestone_assets::entity_models`' ghast tentacles, `lodestone-audio`'s
  `select`, `lodestone-particle`'s `rng`). Consolidating them spans four crates
  and was out of scope here; it is recorded in that struct's own doc rather than
  silently repeated. The rejection loop in `next_int` is load-bearing — 11 and 31
  are not powers of two, so `next(31) % bound` is not what Java produces.
* The pass draws with the **translucent** group. It is additive, so whatever it
  should brighten must already be in the framebuffer and nothing opaque may be
  drawn over it afterward.

## Configuration

None. No feature gate, no env var, and — uniquely among the entity passes — **no
jar asset**, so it works identically on a pack-less run.

## Verification

Two gates, and neither can be a captured-bytes comparison for the reason above.

`crates/lodestone-render/tests/lightning_bolt_walk.rs` asserts **structural
invariants derived from the algorithm**. The strongest is
`the_trunk_lands_on_the_entity_origin`: the anchor subtraction only cancels if
the geometry loop re-seeds from the same seed and consumes the same draws in the
same order, so the lowest ring being symmetric about the origin is evidence
about the RNG and the loop together.

`the_trunk_taper_matches_the_predicted_half_widths` is worth reading as a
worked example of this repo's *magnitude* rule. It was first written as
"the top is wider than the bottom" — and that version was **measured passing
under a deliberate swap of `rr1`/`rr2`**, because the bolt still tapers downward
under the wrong pairing, just by different amounts. Rewritten to predict both
hypotheses from outside constants (correct: `0.7 × 1.7 = 1.19` top and
`0.7 × 0.9 = 0.63` bottom; swapped: `1.12` and `0.70`) and to require the
measurement to land on one, it fails under the same swap.

`crates/lodestone-shell/tests/lightning_bolt_pixels.rs` drives the real
`RenderState::render`. Measured: **1,344** vertices (exactly one bolt — an exact
count, because the fixed-capacity buffer's failure mode is clipping a bolt
mid-walk and a `> 0` check would call that healthy), **2,068** pixels brighter
and **0** darker, and **3,506** pixels differing between two bolts with different
entity ids.

The brighter/darker split is the additive assertion, and a coverage count cannot
make it: the bolt's colour is *darker* than the sky, so alpha-blending it would
cover the same silhouette while darkening those pixels. The neuter was observed:
passing `0` to the draw took every arm to zero.

## Dependencies

`lodestone-render` (the walk and the seed), `lodestone-shell`'s GPU layer and
`shaders/lightning_bolt.wgsl`, `glam`. No assets, no protocol work, no ECS
component.
