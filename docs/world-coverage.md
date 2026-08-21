# World coverage (`cargo xtask world-coverage`)

## What it is

A registry-driven census, `cargo xtask world-coverage`
(`xtask/src/world_coverage.rs`), that answers one question for every entity
type, block-entity type and particle type the game has: **does it reach any
geometry?** It exists because the two instruments that came before it are
structurally blind to this — `connectedness` asks whether a clientbound
*packet* reaches anything, and `islands` asks whether a Rust *symbol* has a
caller — while the defect that motivated it is neither. An item frame's packet
arrives, its code is called, its counter increments, and no pixel is ever
drawn.

## Why it exists: the item frame

Item frames had a ported pose matrix (`framed_item_matrix`), a hitbox entry in
`EYE_HEIGHTS`, a two-element type-path constant (`ITEM_FRAME_TYPES`), a
dedicated branch in `special_item_instances`, and their own
`RenderStats::special_item_frames_drawn` counter. Everything compiled,
everything was tested, and nothing drew: `entity_models` deliberately holds no
`item_frame` rig, and the two passes keyed on the type draw the *contents* of a
frame (a filled map, a special item), never the frame. An earlier island audit
read the code and did not find it, which is the whole argument for a tool: a
search that failed is evidence about the search.

`item_frame` is therefore the calibration case. `the_census_reports_item_frames_as_stranded`
in `xtask/src/world_coverage.rs` is the control, and it has been observed to
fail — planting `"item_frame"` into a renderer claim turns it red with
`Found Drawn("text display glyphs") instead`. **Run the neuter before trusting
a change to this tool**; a control nobody has watched fail is an argument, not
an observation.

## How it works

### Populations come from the real registry, never a list

| population | source | count |
|---|---|---|
| entity types | `lodestone_data::entity_type::EntityType::from_registry_id` over `EntityType::COUNT` | 158 |
| block-entity types | `lodestone_data::block_entity_types::block_entity_type_name` over `TYPE_COUNT` | 49 |
| particle types | `lodestone_data::particle_types::particle_type_name` over `PARTICLE_TYPE_COUNT` | 125 |

`xtask` takes `lodestone-data` and `lodestone-assets` as plain dependencies for
this. Both are leaf-ish and version-free — `lodestone-data` depends only on
`lodestone-model` — so no protocol family and no `wgpu` reach the task runner.
The alternative, re-parsing the generated tables inside `xtask`, would be a
second implementation of the registry that could drift from the one production
reads.

### Four buckets, not three

* **drawn** — something resolves geometry keyed on this subject.
* **stranded** — nothing does, *and the client's own draw surface names it
  anyway*. **This is the finding class**: half-built work, where a table entry,
  a constant or a classifier already knows the subject and no pass emits
  anything for it.
* **absent** — no geometry, and nothing in the draw surface names it. A real
  gap, but a cheap one to read: nobody started it.
* **no vanilla rig** — nothing draws it here *and nothing draws it in vanilla
  either*. Not a finding. Only ever assigned when the oracle below was actually
  read.

That fourth bucket is what makes the output actionable rather than a restatement
of the registry. Without it the block-entity population reports 24 holes; with
it, 1. A hopper, a furnace and a sculk sensor have no block-entity renderer in
the game, and a census that counts them as gaps is a census nobody acts on.

### The vanilla oracle

Two files from the pinned 26.2 decompile under `.cache/`:

* `net/minecraft/client/renderer/entity/EntityRenderers.java` — 147
  registrations. Only the **positive** signal is used: three types are
  registered against a renderer that draws nothing, and those three are
  reclassified. Absence from this file proves nothing, because about a dozen
  types (the horse family, the piglin family, squid, player) are registered
  through shared helpers rather than by name.
* `net/minecraft/client/renderer/blockentity/BlockEntityRenderers.java` — 26
  registrations, one call per type, every one of which maps 1:1 onto a registry
  key. Absence from *this* list is conclusive.

This is an outside source in the sense the evidence rules require: the
expectation comes from the game, not from our own renderer. It also produced an
independent agreement worth recording — the census's block-entity split
(25 drawn / 23 with no vanilla renderer / 1 gap) was derived from the block-state
table plus our gather predicates, and lands exactly on the 26-registration list
it never consulted for that half, with `test_instance_block` as the single
difference.

`.cache/` is not repo state. When the files are missing the run still succeeds,
but prints `vanilla renderer oracle: UNAVAILABLE` naming both paths and stating
that every count is then an over-count. "Could not look" must never share a
value with "no findings".

### How each population resolves "drawn"

**Entities** resolve through the real corpus rather than a transcription of it:
a type whose registry path *is* an `entity_models()` entry name is drawn by that
rig, which is how `canonical_model_name_for_type` works and why a newly ported
mesh makes its mob drawable with no table edit. On top of that, three rules are
read out of the AST — the alias arms in `canonical_model_name_for_type`, the
suffix ladder in `boat_model_name`, and the table inside `thrown_item_for` — and
four reviewed claims cover the passes that are not tables at all (dropped item,
experience orb, moving block, text display).

**Particles** have exactly one dispatch, `Particles::spawn_one`, and its arm
literals are extracted from the `match` **patterns** rather than from the
function's literals, so the emit calls in its bodies do not read as dispatch
keys. Its catch-all is a hard drop with no fallback sprite, which is why "has an
arm" is the whole of "draws". Two reviewed claims cover the client-predicted
emitters (block-break debris, item crumbs) that never carry a registry id.

**Block entities** have no dispatch table at all — nothing on the render path
reads a block entity's `type_id`. The chunk's block-entity list supplies
candidate *positions*, and every gather predicate keys on the **block state's own
name**. So the census asks the question that path actually answers: it inverts
the per-block-state table to get the blocks owning each type, then tests those
names against every literal *and every suffix rule* the gather layer spells. The
suffix half is load-bearing rather than a nicety — signs are claimed by
`ends_with("_sign")` and shelves by `ends_with("_shelf")`, and a scan collecting
only whole literals reports both families as unreached.

### A fourth check: render-source wiring

Cheap to run alongside, and orthogonal: the GPU state declares one `*_source`
field per per-type pass, each of which must be re-installed through the matching
`set_*_source` every frame. A field with no installer is a renderer that can
never draw. Currently 24 declared, all installed.

## The census, as of this writing

Run it rather than quoting this section — a coverage number recalled from a doc
has been wrong in this repo four times in four different ways. Findings: 53
across 332 subjects at the last run recorded here, down from 118 when this
document was written.

### Entity types — 143 drawn, 6 stranded, 6 absent, 3 no-vanilla-rig

Was 124/19/12 when this instrument first ran. The nineteen entity findings that
have closed since are written up in `docs/entity-rendering.md`, which carries the
vanilla source each rig was transcribed from and what each one still leaves out.
What follows is what is left.

**Stranded** (the finding class):

| subject | what already knows about it |
|---|---|
| `item_frame`, `glow_item_frame` | `ITEM_FRAME_TYPES`, `EYE_HEIGHTS`, `item_frame_blockstate`, `framed_item_matrix`, a draw counter. These do now draw — through a *block* model rather than a rig, which is the right mechanism for them — so the census still reads them as stranded because its entity detector's subject is the rig corpus. Read this row as a limit of the instrument, not as a hole |
| `wind_charge`, `breeze_wind_charge` | `EYE_HEIGHTS`; `thrown_item_for`'s own doc records that these need a cuboid model and must *not* be added to its table |

The `EYE_HEIGHTS` cluster that used to dominate this table was one shape
repeated: the hitbox table is populated from the registry, so it is complete,
while the rig corpus is hand-ported and was not. That is honest unported work
rather than a bug — but a mob with a hitbox row and no rig is *invisible and
solid*, which reads in play as a bug rather than as a missing feature, and that
is why the ten in it were the first thing closed.

**Absent** — `painting`, `lightning_bolt`, `fishing_bobber`, `firework_rocket`,
`dragon_fireball`, `ominous_item_spawner`. Every one has a real renderer in
vanilla, and what unites the remaining six is that **none of them is a cuboid
rig** — each needs a draw path this engine does not have, so none is a corpus
entry away. A painting is a quad textured from its variant; a lightning bolt is
procedural geometry rebuilt per frame; a bobber is a billboard plus a line back
to the caster; a dragon fireball is a single camera-facing quad assembled vertex
by vertex; a firework rocket and an ominous item spawner are both item models
taken from *entity metadata* rather than from a default, which is exactly what
keeps the rocket out of `thrown_item_for` despite being drawn the same way its
members are. The ones that *were* rigs — `evoker_fangs`, `shulker_bullet`,
`wither_skull`, `llama_spit`, `spawner_minecart`, `command_block_minecart` — have
landed.

**No vanilla rig** — `marker`, `interaction`, `area_effect_cloud`.

**Rigs no type routes to** — `boat_water_patch` (correct: the water-clip mask is
a second instance submitted for a boat that already resolved `boat`) and
`player_slim` (correct: selected per-skin by `player_model_name`, and no caller
has skin-model data yet). Both are explained; neither is a finding.

### Block-entity types — 25 drawn, 0 stranded, 1 absent, 23 no-vanilla-rig

The single gap is `test_instance_block`, a creative/dev-only renderer, which is
also the one exception `docs/block-entity-renderers.md` names. This population
is in good shape and the census says so.

### Particle types — 85 drawn, 0 stranded, 40 absent

**This section's first version read 39 drawn / 1 stranded / 85 absent**, and is
kept here because what the census found there has since been acted on: the
stranded `enchanted_hit`, and the three dead sheets below, are closed. See
[`particle-catalogue.md`](./particle-catalogue.md) for the per-family writeup.
Run the tool rather than trusting either number.

**Stranded**: none. `enchanted_hit` was the one, and it is now emitted by
`Particles::spawn_one`.

**Absent**: 40 of 125, down from 85 — still the largest single body of unreached
content in the client, and unlike the entity list it is not disguised:
`spawn_one`'s catch-all logs and drops. Worth reading as a backlog rather than
as a defect report. What is left is mostly the option-carrying types (`block`,
`block_marker`, `falling_dust`, `item_*`, the leaf families), the 26.2 additions
(`geyser*`, `noxious_gas*`, `sulfur_*`, `firefly`) and a handful of one-off
classes.

**Dead sprite sheets, found by hand rather than by the tool.** `Sheet::Effect`,
`Sheet::Enchant` and `Sheet::EnchantedHit` were declared, were in `Sheet::all()`,
and were therefore stitched into the particle atlas — with no production code
constructing any of the three; every reference outside `Sheet::all()` was in a
test. Atlas-resident and unreachable. The census could not see it: it reports the
*subject* side, and only `enchanted_hit` happened to share a name with a sheet
frame stem.

All three are now live. **The reverse query is also a gate now**, which is the
part worth keeping: `no_sheet_is_atlas_resident_and_unreachable_from_the_dispatch`
(`crates/lodestone-shell/src/particles.rs`) drives the whole 125-entry particle
registry through `spawn_particles` and requires every `Sheet::all()` entry to come
back, so a sheet added without an emitter fails by name. That is the
particle-population half of the "reverse direction" gap noted below; the
block-entity half is still open.

## Known gaps in the instrument

* **The reverse direction is only implemented for entities.** "A renderer no
  subject routes to" is computed against the `entity_models()` corpus and
  nowhere else, so a dead `BlockEntityModelEntry` goes unreported — as the three
  particle sheets above did before anyone looked. Closing this inside the tool
  means one reverse query per population, each keyed differently, which is why it
  is written down here rather than half-built. **The particle half now has a
  guard, but it lives in the subject crate rather than in this tool**
  (`no_sheet_is_atlas_resident_and_unreachable_from_the_dispatch`), because it
  can drive the real dispatch and this scanner can only read `match` patterns.
  That is a reasonable split and worth knowing about: a green
  `world-coverage` run says nothing about dead block-entity models.
* **Granularity is the registry entry.** A type that draws *something* reads as
  drawn even if a distinguishing part of it does not: a framed map drawing with
  no frame border around it would satisfy any per-type check. This is the same
  blindness `connectedness` has at the packet id, one level up.
* **A block-entity type is claimed if *any* block owning it matches a
  predicate.** A type whose blocks are partly covered reads as fully covered.

## How to change it

* **Adding a renderer** — if it is a table, prefer a mechanical `ClaimRule`
  (`ArmLiteralsInSymbol`, `LiteralsInSymbol`, `SuffixLiteralsInSymbol`,
  `ArmVariantsInSymbol`) over `Explicit`. A mechanical rule tracks the table; an
  explicit list goes stale in the direction a reader cannot see.
* **Every claim needs an anchor** — a file and a symbol that must still be
  defined. A renamed or deleted renderer fails the run rather than continuing to
  vouch for its subjects. A rule that resolves to zero subjects also fails: an
  empty claim must never read the same as a renderer that legitimately covers
  nothing.
* **Moving a module** — `DRAW_SURFACE` is a hard-failure list. If a path in it
  stops existing the command errors instead of scanning less, because a scan
  that quietly narrows is the exact failure `connectedness` shipped for a whole
  session when `adapter.rs` became `adapter/`.
* **Widening the mention surface** — resist it. The three populations have
  *separate* mention surfaces because a bare registry path is not a namespace:
  `lava`, `dolphin`, `nautilus`, `composter` and `elder_guardian` are all
  particle types and also something else, and every one of them was a false
  *stranded* against the full parse set. `hopper` was a false *drawn* for the
  same reason — a hopper block model referenced by the minecart-contents pass.

### Gotchas

* **Over-claiming is invisible in the output.** A claim that is too broad turns
  a stranded subject into a drawn one, and nothing in the report shows it. That
  is the direction to be paranoid about; under-claiming merely produces noise
  you can see.
* **Resolution is name-based, with no type checker**, the same trade
  `islands` documents. A registry path that collides with an unrelated string in
  the same surface will read as a mention.
* **A mention is not a call graph.** *Stranded* means "named in draw code and
  reaching no geometry"; it does not tell you which hop is missing. Trace the
  chain before fixing where the report points.
* **`#[cfg(test)]` modules are excluded**, including the ones that live in their
  own file and are declared with `#[cfg(test)] mod name;` from a parent —
  `gpu/tests.rs` and `gpu/pixel_gates.rs` both construct draws for mobs with no
  rig, and counting those would report unported work as half-built.

## Configuration

None. No flags, no environment variables. The only optional input is the
decompile under `.cache/mc/26.2/client-src/`, whose absence is announced in the
report rather than silently degrading it.

## Dependencies

* `lodestone-data` — all three registry populations.
* `lodestone-assets` — the `entity_models()` rig corpus.
* `syn` (with `visit`) and `proc-macro2` — the AST walk, shared with
  `xtask/src/islands.rs`. Macro bodies come back as opaque token streams, so
  those are walked as tokens; that is how a `matches!` gather predicate is seen.
* The pinned 26.2 decompile under `.cache/`, optional.

## Related

* [`island-detection.md`](./island-detection.md) — the symbol-level scanner.
* [`clientbound-packet-coverage.md`](./clientbound-packet-coverage.md) — the
  packet-level one.
* [`block-entity-renderers.md`](./block-entity-renderers.md) — the per-type
  writeup this census's block-entity half agrees with.
* [`particle-catalogue.md`](./particle-catalogue.md) — the per-type writeup for
  particles.
