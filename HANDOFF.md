# Lodestone — Deferred Work Handoff

**Status of this document:** everything below was **deliberately descoped**, not abandoned or
found broken. Each area is left in a *working, committed, test-covered* state. This file exists so
the work can be picked up by someone who was not present when it was built.

**Current active scope** is v770 only (protocol 776 / MC 26.2), across four workstreams:
packets, UI, entities, lighting. Everything in this document is **outside** that scope.

**Rule for anyone resuming:** every number in this file was measured on real data, not estimated.
Where something is unproven it says so explicitly. Please preserve that distinction — the single
most expensive recurring failure on this project has been a claim that outran its evidence.

**See also:** [`DESIGN.md`](./DESIGN.md) — full architecture and rationale. Its **§12 validation
log** is the highest-value part: ~20 entries recording beliefs that were confidently held and
empirically false, and how each was caught. This file is self-contained, but §12 is what stops the
same mistakes being made again.

> **Start here if you are picking this up cold:**
> [**Addendum — final play-test round**](#addendum--final-play-test-round-what-landed-what-is-left)
> at the end of this file is the current front line. It lists what landed in the last block of work
> and the **five open defects**, each with a source-level diagnosis rather than a guess:
> block breaking (crack blend + missing hardness table), mob equipment, washed-out colours, entity
> shadows, and the stranded entity events. Everything between here and there is *descoped* work,
> which is a different thing from *open* work.

---

## Where the client actually is (last verified 2026-07-30)

Run it — this was executed and screenshotted, not inferred:

```
./scripts/live-oracles/survival.sh                                        # survival, normal terrain
cargo build --release --bin lodestone --features live
./target/release/lodestone --window --live --host 127.0.0.1 --port 25565
```

**`--features live` is mandatory.** Without it the binary still starts, renders the demo world,
and only whispers `no version family compiled in for protocol 776` into the log while the HUD
shows a plausible-looking `chunks=169`. It looks like it is working. It is not.

Observed at the plains spawn `(-45, 71, -377)`: camera adopts the server spawn, `live_cols=360`,
`sections=2641`, `quads=1490562`, `entities=4`, `drops=0`, **~73 fps / 13 ms frame / 142 MB RSS**.
On screen: real terrain, grass blocks with green tops and dirt sides, tree trunks with cutout
leaf canopies, and **short_grass as see-through cross-plants**. The world is the *server's*,
meshed through per-state vanilla block models and lit by the server's own light data.

Working end to end: join → chunk stream → model-meshed vanilla terrain (including fluids) →
server lighting → biome tint → movement with live-world collision → chat → tab list → scoreboard →
containers → entity spawn/despawn with real textures and 3-tick interpolation → mining and
placement reconciled against the server → block-break particles → hotbar item icons.
Clientbound packet coverage is **108/141 decoded**, serverbound **53/69**.

> **Read the status rows below, not this paragraph, before starting work.** Staleness is the most
> common defect in this repo (there is a whole addendum on it): every wrong belief recorded here
> was *true and evidenced when written*. When a task looks blocked on "X doesn't exist yet",
> re-verify that X still doesn't exist before routing around it.

### Known-wrong things you will see immediately

These are current, reproduced-on-screen defects, not speculation:

- **~~Water and lava render as nothing.~~ CLOSED.** `29cfaea`/`544051e`/`867439c` mesh fluids into
  water/lava geometry and add the translucent pipeline; `db0e2b9` gates see-through water on
  offscreen pixels. What remains is *fog*, not geometry — see the underwater diagnosis below.
- **~~Cross-plants are not biome-tinted.~~ CLOSED.** `92a4dc6` applies biome tint in gamma space.
  The diagnostic that found it is worth keeping: averaging the whole frame gave G/R ≈ 1.13 and
  read as "global gamma"; clustering by *location* separated two spatially distinct populations
  (pale on tops/plants, saturated on grass **side-overlay strips only**), which a global transform
  cannot produce. Ask *where*, not *what*.
- **The hotbar is a placeholder bar**, while hearts and hunger beside it are pixel-correct vanilla
  sprites — so the GUI atlas and `gui.scaling` model are fine and the fault is isolated to the
  hotbar widget.
- **The HUD font is a 5×7 debug font** with a fixed advance (`3d838de` gave it mixed case, so
  server text no longer shouts, but the metrics are still wrong). Vanilla text needs `ascii.png` +
  unicode pages, **per-glyph proportional widths**, and the 1px 25%-brightness drop shadow.
  Proportional widths are most of what makes vanilla text look like vanilla text — and note that a
  font with the right characters at the wrong widths passes every `assert_eq!` on the source
  string, so this has to be gated on pixels.
- **No smooth lighting / AO on the model path** — flat per-block light plus directional shade.
  Correct, but one of the most recognisable "not Minecraft" tells now that geometry is right.
- **~~Block placement does not exist.~~ CLOSED.** `e339e06` built the game layer and `d649bfe`
  routed the shell's dig *and* place through it, so both now reconcile against the server instead
  of editing our local copy. This also retires the *"the chunk only renders properly when I break
  something"* report from a second direction: `BLOCK_UPDATE`/`SECTION_BLOCKS_UPDATE` previously
  applied without emitting a directive, so server-authoritative block changes were
  applied-but-never-drawn. Both now emit `ChunkLoaded`.

- **Mobs: three different problems that looked like one** — two now closed, one open. Reported as
  "no textures, no animations, no interpolation"; each had a different cause.
  - **~~Textures~~ CLOSED** (`b0722ea`). Real `textures/entity/**` skins are loaded from the jar;
    `gpu.rs:327 synthetic_entity_texture` survives only as the fallback for an unresolved model.
  - **~~Animation~~ CLOSED** (`58804e5`). Wired end to end and **proven at pixels**.
    `lodestone-assets::entity::bake_entity_parts` emits per-part quads with no transform folded in;
    `render/entity_anim.rs` is a pure, GPU-free animator (`Skeleton::pose`) reproducing vanilla's
    `setupAnim` per family; `render/entity.rs` uploads **one instance buffer per part** and
    `entity_pipeline.rs` draws each part's index range. Per-part instancing was chosen over CPU
    re-baking (the route this handoff previously recommended) because re-baking needs a vertex
    buffer per *entity*, whereas a mob is ~10–35 parts but hundreds of quads, so matrices move
    ~1% of the data. A per-vertex `part_id` indexing a storage buffer was rejected: it needs a
    vertex-stage storage buffer, the exact feature that killed WebGL2 (DESIGN §12.72).
    `tests/entity_anim_pixels.rs` renders a pig side-on at two half-cycle-apart inputs and asserts
    the leg band differs by ≥200 px (measured 2963) with a rest-vs-rest control at exactly 0; it
    was falsified by swapping `.pose(anim)` for `.rest_pose()`, which drives the band to 0.
    **Known divergence:** `set_*_rot` *adds* to the authored pose where vanilla *assigns*.
    Identical wherever the driven limb is authored at zero rotation, which is nearly all of them;
    `entity_anim.rs::models_that_author_a_driven_limb_rotation` pins the 14 that are not
    (spider/cave\_spider — where vanilla also adds, so not a divergence at all — plus snow\_golem,
    ender\_dragon, witch, villager, rabbit, bee, parrot, pillager, vindicator, evoker, illusioner,
    wandering\_trader). Those need per-model `setupAnim` ports. Also still unported: humanoid
    swim/crouch/ride/fall-flying/item poses, chicken wing flap, villager `isUnhappy` head shake,
    and `attack_anim` (assumes the right arm, hard-wired to 0.0 in `entities.rs::render_anim`).
  - **~~Interpolation~~ CLOSED** (`b0722ea`). The window was `TICK = 0.05` — **one** tick, where
    vanilla eases over **three** — so the ease finished in 50 ms and the mob froze until the next
    packet: move, freeze, move, freeze, which reads as "not interpolated" even though
    interpolation was running. Now `INTERP_WINDOW = TICK * INTERP_STEPS` (150 ms), with head yaw
    and pitch tracked alongside body yaw.

### Interaction feedback — full audit (what you see when you hit a block)

Audited in code on 2026-07-30, one entry per thing a player actually notices. The pattern is
consistent enough to be worth stating up front: **the logic is almost always present and correct,
and the pixels are almost always missing.**

| what the player expects | state | where |
|---|---|---|
| Block actually breaks on the server | **done** (`d649bfe`) | the shell's dig/place now route through the `lodestone-game` mining/placement layer instead of editing the local copy |
| Crack overlay on the block you're mining | **state computed, never drawn** | `mining.rs:263 destroy_stage()` returns 0–9 exactly per `getDestroyStage` |
| Crack overlay for *other* players' digs | **state computed, never drawn** | `mining.rs:420 BlockDestructionOverlays`, whose own doc says "rendering the crack is someone else's job" |
| Your arm visible in first person | **does not exist** | no first-person/held-item renderer anywhere in the tree |
| Your arm swinging when you hit | **sent, not drawn** | `sim.rs:1119` now sends `ClientAction::SwingArm` on dig, so *other* players see us swing; we still have no first-person arm to draw it on |
| Other mobs' attack swing | **timer exists, not articulated** | `entity/pose.rs` tracks the swing; renderer emits one matrix per entity |
| Broken block drops a visible item | **lifecycle done, never drawn** | `entity/item_entity.rs` (age, despawn, pickup delay, merge sentinel `32767`); no item-entity rendering |
| Item flies to you when picked up | **not handled** | no `TakeItemEntity` consumer |
| Item appears in your hotbar slot | **done for sprite items** (`c562db4`) | a flat item-sprite atlas is built and drawn into the hotbar. **Block items (`IconPart::Model`) still draw empty wells** — they need an isometric 3-D GUI render pass (`display.gui` transform), which is the largest remaining hotbar gap. Container/inventory screen slots in `container.rs` do not use `ItemAtlas` at all yet. Enchant glint is plumbed but `enchanted` is hardcoded `false`; animated item textures take frame 0 only. |
| Break particles | **done** (`9f30663`, `77c99bb`) | `lodestone-particle` runs vanilla's simulation; `shell/particles.rs` wraps it with a GPU renderer. Emitted offline from `break_block` and live from `LevelEvent` **2001** (`PARTICLES_DESTROY_BLOCK`, whose `data` is the pre-break block-state id). Gated on pixels by `shell/tests/break_particles_pixels.rs`. **Sheet-sourced particles (smoke, flame, crits, splashes) still resolve to `None`** — no stitched `particles.png` atlas exists — and are counted into `ParticleFrame::unresolved` rather than dropped silently. Translucency layer sorting is not implemented (one blended pass, `Layer` ignored). |
| Break/place sounds | **appears wired** | server sound events reach `audio.play_sound` (`sim.rs:1223`) |

Two things follow from the table. First, **the shortest path to a client that *feels* right is
renderers, not logic** — crack overlay, item entities and a first-person arm are all blocked on
drawing, not on understanding. Second, `assets/icon.rs` and `assets/item_model.rs` (the 1.21.4+
selector-tree item definitions) were the **sixth island**, built to completion before anything
could consume them; `c562db4` connected them for sprite items, which is what the island pattern
predicts is always the cheap half once someone owns the chain end to end.

### Rendering fidelity — audit (2026-07-30)

| effect | state | detail |
|---|---|---|
| Animated block textures (flowing lava/water, fire, portal, prismarine, sea lantern) | **built, no producer — seventh island** | `render/anim.rs` is complete and unit-tested: given a frame timeline and a tick it yields two atlas regions plus a blend factor, and the atlas deliberately keeps **every** physical frame resident so no re-upload is needed. Nothing constructs a `SpriteAnimation` — `lib.rs:68` re-exports it and that is the only reference in the tree. All 52 animated sprites render as a **single static frame**. |
| Waterlogged blocks | **done (rendering)** | `render/block_models.rs:128` classifies any state with `waterlogged=true` as carrying a water source, so kelp, seagrass and waterlogged stairs/slabs mesh with water. Tested (`waterlogged_blocks_carry_a_water_source`). **Placement does not set it** — `lodestone-game`'s placement parks waterlogging along with stair shape and multi-part blocks. |
| Entity shadows | **does not exist** | Vanilla draws a soft dark oval under every entity, radius scaled by the entity, projected onto the geometry below and faded by height. There is no shadow code of any kind; the only `shadow` matches in the tree are sky-light tests and the font's drop shadow. Mobs currently float visually. |
| Distance fog | **does not exist** | No fog system at all. This is the diagnosed cause of the odd underwater look — culling was *verified correct* (`B − R` flat across depth), so do **not** rewrite the mesher. |
| Particles | **done for block-break; sheet sources open** | `lodestone-particle` + `shell/particles.rs`; see the interaction table above. Remaining: a stitched `particles.png` atlas so smoke/flame/crit/splash resolve, and translucency layer sorting. |
| Underwater rendering | **wrong in a specific, measurable way** | See below. |

#### The underwater screenshot, diagnosed

Measured the blue cast down the frame (far at top, near at bottom), as `B − R` per row:

```
y=0.05  58      y=0.40  49      y=0.80  66
y=0.20  61      y=0.60  62      y=0.95  59
```

**Flat.** The tint does not vary with distance, and no pixel anywhere reaches white (channel maxima
130/173/231). Two conclusions:

1. **Face culling is working.** If interior water↔water faces were being emitted, each additional
   block of water between eye and target would blend another layer and the cast would deepen with
   distance. It doesn't — we are getting **exactly one** layer of tint no matter how much water we
   are looking through. That is the correct culling behaviour and it is worth recording as
   *verified*, because it was the obvious suspect.
2. **What's missing is fog, not geometry.** Vanilla does not create the underwater look by stacking
   translucent quads; it uses **`FogRenderer` with a short, exponential, biome-coloured water fog**
   and a heavily reduced view distance, so distant terrain fades to solid fog colour and disappears.
   We render the entire loaded world through one flat multiply, which is why the far terrain is
   still legible and the whole frame looks washed out rather than submerged.

Also missing and part of the same effect: the full-screen **`textures/misc/underwater.png` overlay**
vanilla draws when the eye is in water, and the `ambient.underwater.*` sounds (which *are* in the
generated sound table, just never triggered because nothing tracks eye-in-fluid state).

**There is no eye-in-fluid state anywhere in the client** — no `underwater`/`eye_in_water` concept.
That single piece of state gates the fog colour, the overlay, the ambient loop, and the swimming
physics, so it is worth introducing deliberately rather than four times locally.

### The island pattern — the most expensive recurring failure here

A well-tested library lands, and **nothing calls it**. Every test is green, the HUD counter looks
plausible, and the screen is wrong. Seven confirmed instances:

| island | built | actually consumed |
|---|---|---|
| GUI atlas / `gui.scaling` | `lodestone-assets::gui`, gated against the real jar | only by its own tests |
| block models, layers, translucency | `render/{models,model_pipeline,translucency}.rs` | nothing — mesher emitted full cubes |
| mining + placement | `lodestone-game`, live-gated over RCON | nothing — shell edited its local copy |
| vanilla font loader | `lodestone-assets::font`, complete since the first commit | nothing — shell drew a 5×7 debug font |
| ~~entity pose / walk animation~~ | `lodestone-entity::pose` → `render/entity_anim.rs` | **closed** — per-part instancing, gated at pixels (`58804e5`) |
| item icons / item definitions | `lodestone-assets::{icon,item_model}` | nothing — hotbar cells are empty wells |
| block texture animation | `render/anim.rs`, unit-tested | nothing — every animated sprite is frozen on frame 0 |

The common cause is that a crate's own test suite is a **closed loop**: it can be entirely green
while the crate is dead code. The counter-measure that has actually worked is to require that
**something on screen changes** before a piece is called done — a screenshot, or a measurement
taken from a running client, not from a unit test.

### Fixed since that play-test

- **Death is no longer terminal** (`44a8ec3`). My diagnosis was wrong in an instructive way: I
  claimed nothing sent `ClientAction::Respawn`. In fact `RespawnPolicy::Automatic` was already
  answering `ClientEvent::Death` and the library was recovering fine — the shell was setting
  `SessionPhase::Ended` and dropping `ClientEvent::Respawned` in a catch-all, so the library played
  on while the shell had declared the game over. **The status string invented a causal story
  (`no chunks`) that sent me looking in the wrong place.** Misleading diagnostics cost more than
  absent ones.
- **Chat resolves translation keys** (`258ffec`). `lodestone-assets::Language` loads the real
  vanilla `en_us.json` (8,123 keys) from the downloaded `client.jar`; `lodestone-game::text::resolve`
  lowers `Text → Text` so every `translate` node becomes a resolved literal subtree, with `%s` /
  `%N$s` / `%%`, style inheritance down `extra`, and **missing key → the key itself, never an error
  or empty string**. Live: `…was slain by entity.minecraft.spider` → `…was slain by Spider`.
  Here too the defect was **a missing table, not missing logic** — the model already formatted
  correctly once given a real one, so the smallest correct fix was data plus a lowering shim rather
  than a new formatter. **~~Still parked: the shell doesn't consume it yet (`chat.rs:88`).~~ CLOSED
  by `71182c8`** — the shell resolves at *ingest*, not at draw: `sim.rs`'s `NetUpdate::Chat` handler
  calls `Sim::resolve_text` before the text ever reaches `chat_log`, so by the time a `Text` gets to
  `chat.rs` it contains no `translate` nodes at all. `chat.rs` is deliberately pure (no winit, no
  GPU, no client handle, per its own module doc) and correctly does not own a `Language` table.
  Still genuinely parked: `TextContent` models only `Literal`/`Translate`
  (`lodestone-model/src/text.rs:260-269`), so keybind/score are dropped before they reach the
  resolver.

  > **Read this before trusting a grep.** This entry sent an agent to "wire up" something that had
  > been wired for two commits. `grep resolve crates/lodestone-shell/src/chat.rs` returns zero hits
  > — and that is *correct and expected*, because the consumer is one layer up. **Zero hits in the
  > file a stale note names is not evidence the feature is unwired.** Grep for the *producer*
  > (`text::resolve`) across the whole tree, not for the consumer in one named file.
- **Block placement, at the game layer** (`e339e06`). `placement.rs` mirrors vanilla's
  `BlockPlaceContext`/`performUseItemOn`: replaceable target → place in-place, else adjacent;
  interactable block wins over placement unless sneaking; `Direction.fromYRot` and
  `orderedByNearest` reproduced exactly; predict-then-reconcile over the same sequence ledger as
  mining. **Honestly bounded:** facing/axis/half/pillar/stairs resolve exactly; stair *shape*,
  waterlogging, multi-part (doors/beds), rotation-16 (signs/banners) and wall-vs-floor variants
  fall back to a default.

  **The finding worth keeping:** its live gate initially failed on sneak-placement, because
  setting a sneaking flag *in our own context* only drives *our own* decision — the server derives
  sneak from `setShiftKeyDown(input.shift())`, so without a real `SetPlayerInput { shift: true }`
  on the wire the server treated the sneak-placement as an interaction and re-opened the chest.
  A design that trusts only its own sneak flag **passes hermetically and desyncs live**. This is
  the third distinct instance on this project of the same rule: *state the server derives from our
  input must be driven through the wire, not asserted locally.*

The other big gaps are under [Never started](#7-never-started): per-entity mesh geometry (the
*mechanism* is proven via pig; the other 87 meshes are not individually verified) and chat/HUD
pixel gates.

### How these were found, which matters more than the list

Every single defect above was found by **launching the client and looking at a screenshot**, after
26 green test suites and hours of HUD counters had found none of them. Two of the sharpest
diagnoses came from the user playing for a few minutes ("I'm standing on invisible blocks", "the
grass is not transparent — I think you assume every block is a full block"), and both were
correct while my own counter-driven hypotheses were wrong.

```
RUST_LOG=warn nohup ./target/release/lodestone --window --live --host 127.0.0.1 --port 25565 &
sleep 25 && screencapture -x -o /tmp/shot.png     # then crop/zoom with PIL
```

Pixel *measurements* beat impressions: the tint defect above is a claim that survives
disagreement precisely because it is two channel ratios, not "looks a bit pale".

---

## Table of contents

1. [Multi-version protocol families (v47 / v340 / v735)](#1-multi-version-protocol-families)
2. [WebAssembly / browser target](#2-webassembly--browser-target)
3. [Audio](#3-audio)
4. [Worldgen performance](#4-worldgen-performance)
5. [Online-mode authentication](#5-online-mode-authentication)
6. [Allocator selection (closed — no action needed)](#6-allocator-selection-closed)
7. [Never started](#7-never-started)
8. [Traps that are expensive to rediscover](#8-traps-that-are-expensive-to-rediscover)

---

## 1. Multi-version protocol families

### Decision

The original target was 17 protocol families spanning 1.8.9 → 26.2. That was cut first to four
(v770, v735, v340, v47) and then to **v770 only**. The other three families **remain in the tree
and must not be deleted** — they are the empirical proof that the version-isolation architecture
works, and re-deriving that proof is far more expensive than carrying the code.

### Why the reduction happened (this is the load-bearing part)

Two independent findings converged, and together they mean 17 families was the *wrong plan*
rather than merely an ambitious one:

- **Adapter dispatch cannot be generated.** ID routing is mechanical, but lowering/raising to
  `ClientEvent`/`ClientAction`, world side effects, registry lookups, teleport replies and
  chunk-shape state are semantic per-version work.
- **Wire-shape migration cannot be generated either.** `xtask new-version` cloned v340 → v735
  correctly and mechanically, and the result was a 1.12.2 client wearing 1.16 packet IDs.

So codegen covers packet IDs and registry tables — the cheap part — and covers **neither dispatch
nor shape migration**, which are the bulk *and* the risk.

### Measured cost of a family

Do **not** use the `cargo xtask codegen-ratio` "hand-written lines" figure to plan with; it counts
docs, blanks and derived struct declarations. The real measurement, taken on v735:

| bucket | lines |
|---|---|
| generated (`packet_ids` 841 + `entity_types` 123) | 964 |
| hand-written total | 3007 |
| · doc/comments | 997 |
| · blank | 181 |
| · **actual code** | **1829** |

And within that 1829:

| file | lines | nature |
|---|---|---|
| `adapter.rs` | 712 | dispatch / choreography / lower / raise — **irreducible** |
| `chunk.rs` | 191 | paletted decode, biomes prefix, light split, flattening — **irreducible** |
| `metadata.rs` | 211 | typed union + per-version type-id table — semi-reducible |
| hand codecs | ~200 | JoinGame/Respawn NBT, slot, position — macro-closable |
| derived decls | ~515 | `#[derive]` + field lists — mechanical |

**Genuine irreducible per-version knowledge is ~900 code lines.** A fifth family is roughly a day
of work, not a project. Budget accordingly if resuming.

### State of each family

| family | version | protocol | live-verified | `ClientAction` encode |
|---|---|---|---|---|
| `v770` | 26.2 | 776 | yes — active scope | 42/43 |
| `v735` | 1.16.5 | 754 | yes, chunk decode against a real 1.16.5 server | 17/43 |
| `v340` | 1.12.2 | 340 | yes | 17/43 |
| `v47` | 1.8.9 | 47 | yes — 81 columns via `map_chunk` + `map_chunk_bulk`, 0 trailing bytes | 16/43 |

Deletability is **measured**, not asserted — `cargo xtask check-deletable <family>` simulates
removal and reports the true fallout. All families are cleanly deletable (v47 5 manifest lines,
v340 4, v770 8).

### What is left if you resume

1. **Action encode breadth is the biggest gap.** v47/v340/v735 sit at 16–17 of 43 while v770 is at
   42. Concretely, **a 1.8.9 client still cannot break a block.** `BlockAction`, `UseItemOn`/
   `UseItem` and `InteractEntity` are partial/lossy; `ContainerClick` is absent on all three.
2. **Critically — some of that gap is correct by design and must not be "fixed."** The canonical
   model is shaped by the newest protocol and older adapters translate *upward*, so
   `SetPlayerInput`, `EndClientTick` and `ChatAck` genuinely have no 1.8.9 form. **Any resumed work
   must first produce a table distinguishing *absent by design* from *not done yet*,** because a
   table where those look identical is exactly how v735 shipped registered-but-unreviewed.
3. **v47 place-interaction cannot be gated in the current lab.** The 1.8.9 container is survival
   with no RCON and no console, so the player has nothing to place. Break-only is the maximum until
   that container gets an RCON channel. This is documented in-crate rather than silently absent.

### The `SHAPE_REVIEW.toml` gate — do not remove it

`xtask new-version` clones a family and prints a residue list telling you the packet structs are
still the *source* family's wire shapes. On first use, that warning went to stdout and evaporated
while the same command wired the new family into the registry as **supported**. One command emitted
a true signal and an opposite fact, and only the fact survived.

The fix is that a family is **not registerable** while `SHAPE_REVIEW.toml` has undischarged
entries. v735 was de-registered until all 62 packet entries were audited. **Residue printed is
residue lost** — if you extend the tooling, keep the failure closed.

Also: **never clone a live test.** A cloned live gate pointing at the *source* family's server is
worse than no test, because it manufactures evidence for the wrong version. v735 shipped with a
cloned `live_chunk.rs` still pointing at the 1.12.2 container on port 25568.

---

## 2. WebAssembly / browser target

### State: working spike, goal met, frozen

Verified end to end in Chrome:

```
[status] REAL terrain from real server bytes — 16 chunks, 16 sections, 250 greedy quads
         backend: BrowserWebGpu | select_strategy(): PerDraw     ~119–121 fps
[net]    relay probe OK — browser WebSocket → relay → live server
         version.name = "26.2" | {"version":{"name":"26.2","protocol":776}, …}
```

Browser → WebSocket relay → **live vanilla 26.2 server**, round-tripping real status JSON, with
real vanilla textures decoded in-browser. `trunk` 0.21.14 is installed and `trunk serve` works.

`scripts/wasm-check.sh` passes for all wasm targets and is the regression guard.

### Architecture notes worth keeping

- **The one true blocker is networking, permanently.** Browsers cannot open raw TCP; vanilla
  servers speak only raw TCP. A browser build **strictly requires** a WebSocket↔TCP relay. No
  browser API removes this (WebTransport/WebRTC don't speak to a vanilla TCP listener either).
- **The relay is ~150 lines and protocol-blind.** Because the codec is byte-transparent framing, it
  never parses a packet, so **one relay serves all versions and all servers**. The moment it parses
  a packet it becomes a per-version component and you need one per family. Keep it dumb.
- **Payload: 933 KB brotli** (raw 3.71 MiB, gzip 1.21 MiB) at last measurement. **Report brotli** —
  servers ship wasm brotli-compressed and gzip overstates real cost by ~26%. Attribution:
  wgpu + naga + glow ≈ 1.19 MiB, i.e. the graphics stack, not our code.
- **`wasm-opt -Oz` is counterproductive for download size** — it shrinks the raw module ~10% but
  makes the *brotli* artefact 4 KB larger. It trades download for parse/instantiate time. Trunk's
  `data-wasm-opt="0"` is correct if you are optimising bytes.
- **`opt-level = "z"` (1.21 MiB) beats `"s"` (1.30) and `"3"` (1.62).** `"3"` is a +28% regression
  for speed a 250-quad scene does not need.

### `webgl` feature was removed, and it is **not** a toggle

It cost 537 KB brotli — **68% of the entire download** — for a path that **panicked before frame
0**. The terrain pipeline binds a vertex-stage storage buffer (`block.rs`, `ShaderStages::VERTEX` +
`BufferBindingType::Storage`), which WebGL2 categorically lacks, so `create_bind_group_layout`
panics at construction.

**Re-adding WebGL2 costs a downlevel-compatible render path (no vertex-stage storage), not a
feature flag.** That is recorded in `web/Cargo.toml` beside the removal. Before pricing any
fallback, *run it* — a fallback that has never executed is not a fallback.

### Traps

- **COOP/COEP asymmetry.** `trunk serve` sets both headers → `crossOriginIsolated === true`; a
  plain static server sets neither. Anything depending on cross-origin isolation (threaded meshing
  via `wasm-bindgen-rayon`) **works under trunk and fails mysteriously elsewhere.** Documented in
  `web/README.md`.
- **A 2-D `getImageData` readback of the WebGPU canvas returns all-black.** That is the un-retained
  drawing buffer, *not* a blank scene. Use the **composited screenshot** to verify. This is the
  inverse of the project's usual failure mode and just as misleading.
- **`std::fs` compiles for `wasm32-unknown-unknown` and fails only at runtime.** A `cfg` gate
  removes *existing* entry points but does nothing about a newly added ungated `fs::read`. The
  enforcement is confinement to a single gated file (`lodestone-assets/src/source_native.rs`) plus
  a grep guard in `scripts/wasm-check.sh`. Keep both.
- **Cargo features are advisory, not architectural boundaries.** Features unify across the whole
  graph, so a downstream crate taking a dependency with default features on silently overrides
  `default-features = false` elsewhere. For a hard boundary use `cfg(target_arch)`.
- **Run `scripts/wasm-check.sh` whenever a dependency is added or bumped.** Dependency changes are
  the only way this breakage class enters the tree — an `rsa 0.9` addition once pulled in a third
  major of `getrandom` via a `rand_core 0.6` pin and broke the browser build, and nothing anyone
  edited mentioned `getrandom`.

### What is left

Browser **singleplayer** is unblocked (`lodestone-server`'s tokio is target-split) but not wired.
The browser is **not** the limiting factor for multiplayer — adapter dispatch breadth is. Do not
optimise the wasm layer in response to multiplayer feeling thin.

---

## 3. Audio

### State: complete and working, consumed only partially

`lodestone-audio`: 63 tests. `lewton` 0.10.2 (pure-Rust Vorbis) + `cpal` 0.18.1 (native-only,
`cfg`-gated). Sample-driven clock with **no `Instant::now()` anywhere**, enforced by a crate-wide
guard with an empty allowlist.

`SOUND`, `SOUND_ENTITY`, `LEVEL_EVENT` and `LEVEL_PARTICLES` all dispatch, so the packet seam is
connected. Playback is gated behind `LODESTONE_ASSET_ROOT`.

### Validation approach worth preserving

Decode validation deliberately avoids self-comparison: **libsndfile encodes** the fixture,
**ffmpeg decodes** the golden PCM, **lewton** is under test — `max_abs_diff 3.1e-5`, `rms 1.8e-5`.
The test has teeth: negated, channel-swapped, and one-frame-shifted goldens are each asserted to
**fail** the tolerance.

**Trap:** a genuinely-silent vanilla ogg gives a worthless all-zeros "match" — two silent buffers
agree perfectly. Guarded by a `peak > 0.3` assertion on the fixture.

### Parity facts (transcribed with call-site citations)

- **`SoundSource` has 11 buses in 26.2** (don't forget `UI`).
- Range = `max(instanceVolume, 1.0) × attenuationDistance` (default 16).
- `AL_LINEAR_DISTANCE` rolloff 1.0 ref 0.0 → `gain = max(0, 1 − dist/maxDist)`.
- MASTER is **not** squared. Pitch clamped `[0.5, 2.0]`.
- Only MONO spatialises; stereo plays flat with no downmix.

### Known limitation, honestly graded

**Panning geometry is not parity.** Vanilla delegates stereo placement to OpenAL-Soft's HRTF. Ours
is equal-power panning, documented as an approximation rather than claimed exact.

### Sound asset layout (non-obvious)

**`sounds.json` is not in `client.jar`.** It, like every `.ogg`, lives in the external asset-object
store addressed by `asset-index-<n>.json` at `objects/<sha1[0..2]>/<sha1>`.

Corpus: **1968 events, 8024 entries (7963 file, 61 event-refs), 4843 distinct files.** Entry type
is `"file" | "event"` (**not** `"sound" | "event"`). All 61 refs are depth-1 and acyclic, but
**vanilla ships no cycle guard**, so a malicious pack would stack-overflow at play time — we bound
it with a visited set and depth cap. A `type: event` entry contributes the *referenced* event's
total weight to the parent's selection sum.

---

## 4. Worldgen performance

### State: correctness complete and verified; performance unmeasured

`lodestone-worldgen` has the strongest evidence in the codebase, all bit-exact against a JVM oracle,
element-wise, naming the divergent coordinate on failure:

```
noise router      34048 / 34048   whole region
final density     98304 / 98304   whole chunk, interpolated (4×8×4 cells, trilerped)
carvers           98304 / 98304   × 2 chunks
surface + aquifer land and ocean profiles
ore features      whole-chunk exact BOTH directions, 3 fixtures / 2 seeds / 2 terrain profiles
```

It is now genuinely on screen — wiring it into the shell moved spawn Y from 46 (a sine+hash
placeholder) to **71** (real vanilla surface height), and meshed sections from 610 to 831.

### The one open question

**Debug-build generation measured ~1.1 s/chunk (169 chunks in 3m09s).** Release-mode per-chunk time
was never measured. Before optimising, measure — and note the constraint: **generation
parallelisation must not break per-chunk RNG determinism**, which the ore-feature parity depends on.
Do not trade parity for speed.

### The bug class to watch

Buried ores draw a `nextFloat` inside `shouldSkipAirCheck` **before** the 6-neighbour air test.
Short-circuiting them desynchronises the shared RNG stream and **three ore families silently
vanish.** A wrong draw *count* is invisible to any test asking "did ores appear" and instantly fatal
to whole-chunk parity.

This is why both gate styles are needed: **exact-match on one chunk catches a wrong draw order;
count bands catch a plausible-but-wrong distribution. Neither catches the other.**

### Architecture note

Worldgen is **data, not code** — vanilla 26.2 ships its noise router as **963 JSON files** under
`data/minecraft/worldgen/`. The generator is a ~700-line version-free interpreter over per-version
JSON, not ~10k lines of ported logic.

The proof that it is data is better than the claim: the Rust interpreter reads **disk JSON** while
the oracle evaluates the **running server's live `RandomState` router**. If disk JSON were an
incomplete picture the two would diverge. They agree 100%.

### Deferred architectural step

The shell currently calls the generator **directly**. Vanilla runs singleplayer as an integrated
server, so the faithful destination is generate → loopback → client-consumes, sharing the
multiplayer path. This was deliberately deferred: closing the island today by a direct call was
worth more than the correct architecture arriving later, and **the generator itself does not have
to change when the call site is replaced.** Recorded so the shortcut is a decision with a named
successor rather than drift.

---

## 5. Online-mode authentication

### State: crypto path works end to end; a full authenticated join is untested and not claimed

```
$ cargo test -p lodestone-net --test online_handshake -- --ignored --nocapture
post-encryption disconnect reason: {"translate":"multiplayer.disconnect.unverified_username"}
test result: ok. 1 passed
```

**The measurement is the failure, and it is a strong one.** That disconnect arrived **encrypted**
and decrypted cleanly. So the server accepted our RSA-wrapped shared secret, matched the verify
token we echoed, switched on its cipher, and its AES-128-CFB8 reply round-tripped against ours. The
only thing that failed is the session-server ownership lookup, which needs a Microsoft account we
do not have.

A framing or decrypt error would mean broken crypto; a clean protocol-level "unverified username"
means the crypto is right. **When you cannot reach success, choose a failure that discriminates.**

### What exists

- `lodestone-auth`: Microsoft device-code OAuth (`flow.rs`) with token caching (`cache.rs`).
- `lodestone-net`: `Cfb8Cipher`, SRV record resolution (`resolve.rs`), legacy/status ping (`ping.rs`).
- Encryption is outermost on the wire: `encode = frame(compress(body))` then encrypt; `feed =
  decrypt` then buffer. One cipher per connection, separate CFB8 feedback registers per direction,
  key == IV == the 16-byte secret. It lives in the sans-IO codec, so the browser path inherits it.

### External vectors (an authority we did not write — keep these)

Minecraft's server-ID hash is a **signed** SHA-1: `BigInteger.toString(16)` over the raw digest, so
negatives get a leading `-` and a leading-zero digest loses a character. A naive hex digest passes
the first and fails the other two:

```
Notch  4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48
jeb_   -7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1     ← the negative case
simon  88e16a1019277b15d58faf0541e11910eb756f6      ← 39 digits, leading zero
```

CFB8-AES128 is checked against NIST SP800-38A F.3.7.

**The cipher is stateful across the whole connection, not per packet.** There is a deliberate
"per-packet-reinit-is-wrong" test that proves statefulness matters rather than merely asserting it.

### What is left

A real authenticated join, which needs Microsoft credentials. `rsa`/`rand` are native-only
(`[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`) — deliberately, since the
session-server call is native-only anyway. The docs name that seam as where a browser auth story
would land.

---

## 6. Allocator selection (closed)

**No action needed. Decision: keep the system allocator.** Benchmarked in
`crates/lodestone-allocbench` — one binary per allocator, mutually-exclusive features, peak RSS via
`/usr/bin/time -l`, median of 5.

| vs. system baseline | throughput (geomean) | mean RSS |
|---|---|---|
| mimalloc 0.1.52 | 94% | **130%** |
| snmalloc-rs 0.7.4 | 79% | 104% |
| tikv-jemallocator 0.7.0 | **113%** | 111% |

No candidate is both faster *and* leaner than macOS `libmalloc`, and the top-end wins are within
measured noise. Each costs a C/C++ toolchain dependency. **Not justified.** If meshing throughput is
later *proven* by profiling to be the bottleneck, jemalloc is the only candidate with a consistent
edge — revisit then, not before.

Two findings worth keeping:

- **Cross-thread free inverts the ranking.** Local-free order is `jemalloc > system ≈ mimalloc ≫
  snmalloc`; cross-thread free at 8–10 threads is `snmalloc > jemalloc ≈ mimalloc > system`.
  Benchmarking with same-thread free — the obvious thing to write — ranks snmalloc last and produces
  the opposite conclusion.
- **Methodology trap:** `vec![0u8; n]` routes to `alloc_zeroed`, letting an allocator skip the
  memset on fresh OS-zeroed pages — it showed jemalloc at a bogus 4×. Use `with_capacity` + a real
  fill so the benchmark matches how sections and meshes are actually written.

**Rule: library crates must never set `#[global_allocator]`.** That is an application-level
decision; a library that hijacks it breaks every downstream consumer.

---

## 7. Never started

- **Scripting host.** WASM (`wasmtime`, sandboxed) vs Lua (`mlua`, ergonomic). Leaning WASM for
  untrusted plugins with a capability-based API. No code exists.
- **The other 13 protocol families.** See §1 for the real per-family cost (~900 irreducible lines).
- **~~Entity model ports.~~ LARGELY DONE — this entry was badly stale and actively misleading.**
  It used to say ~130–150 meshes have "no data path" and that only the *mechanism* was proven (via
  pig). In fact `lodestone-assets/src/entity_models.rs` is **191 KB with ~85 hand-ported meshes** —
  drowned, warden, wither, ender dragon, the horse family, illagers, fish — each sheet-size-checked
  against the real PNG by the `real_jar` coverage test. The port largely happened and this entry
  never got updated.

  The reason pig looked like "the only proven mesh" is that pig was the only mob the *renderer
  could reach*: `EntityModelSet::load()` and `gpu.rs` were baking and uploading all ~85 meshes and
  textures every run, and a single alias table in `entity.rs` was gating them. **~70 mobs were
  rendering nothing at all**, not for want of geometry.

  > This entry sent an agent looking for a missing mesh when the bug was one line of name
  > resolution. Still genuinely unported: the remaining ~45–65 meshes of the full vanilla set.
  > The version-free primitive (`CubeDef`/`PartPose`/`PartDef` → `bake_entity`) is in
  > `lodestone-assets`, and meshes are stable across versions, so it stays author-once,
  > tweak-per-version.

### 7.1 Rendering remainder, in value order

Everything here is *visible to a player* and blocked on drawing, not on understanding. Ordered by
how much each changes the screen per unit of work, which is not the order of difficulty.

1. **3-D block-item GUI pass.** Every block item in the hotbar draws an empty well. Needs an
   isometric ortho camera plus the model's `display.gui` transform. Biggest single visible hotbar
   remainder, and the same pass unblocks container slots.
2. **Container / inventory screen slot icons.** `container.rs` does not use `ItemAtlas` at all;
   reuse the existing `push_item_quad`. Watch the `MenuKind` slot-order trap in §8 — a constant
   offset draws a plausible, wrongly-transposed inventory that reads as an art bug.
3. ~~**Entity animation.**~~ **Done** (`58804e5`) — see the mob row above. The sequencing fact it
   was gating still holds and is now unblocked: **mob physics is invisible until mobs animate**, so
   entity physics can finally be validated by watching a mob rather than by unit test alone.
4. **Fog + underwater overlay.** The diagnosed cause of the odd water. `FogRenderer`-equivalent
   with a short exponential biome-coloured water fog and reduced view distance, plus the
   full-screen `textures/misc/underwater.png` overlay. **Culling is verified correct — do not
   rewrite the mesher.**
5. **Entity shadows.** Do not exist at all. Soft dark oval under every entity, radius scaled by
   the entity, projected onto the geometry below and faded by height. Mobs currently float.
6. **First-person hand + arm swing.** No first-person/held-item renderer exists. We already *send*
   `SwingArm`, so other players see it; we have nothing to draw it on.
7. **Item entities (drops) and pickup.** `entity/item_entity.rs` has the full lifecycle (age,
   despawn, pickup delay, merge sentinel `32767`) and nothing renders it; there is no
   `TakeItemEntity` consumer for the fly-to-player animation.
8. ~~**Crack overlay.**~~ **Drawn** (`gpu.rs:312`), but **wrong in two ways** — see the
   "block breaking" addendum at the end of this file. The blend function is wrong (too white)
   and there is no hardness table, so obsidian and bedrock crack like dirt.
9. **Particle sheet atlas.** Smoke, flame, crits and splashes resolve to `None` because no
   stitched `particles.png` atlas exists. They are counted into `ParticleFrame::unresolved`
   rather than dropped, so the gap is observable rather than silent.
10. **Smooth lighting / AO on the model path.** Flat per-block light plus directional shade today.
    Correct, but one of the most recognisable "not Minecraft" tells now that geometry is right.
11. **Vanilla font metrics.** Per-glyph proportional widths + the 1px 25%-brightness drop shadow.
    A font with the right characters at the wrong widths passes every `assert_eq!` on the source
    string, so **this must be gated on pixels**.
12. **Waterlogging / stair shape / multipart on placement.** Rendering handles all three; the
    placement path in `lodestone-game` parks them.

---

## 8. Traps that are expensive to rediscover

These cost real time to find. They are not specific to the deferred work, but they are the things
most likely to bite someone resuming it.

### Four species of vacuous test

A test can be green, well-written, live, and prove nothing. Two of these species **cannot be found
by reading the test** — the source is exemplary and the flaw is a property of what the test was
pointed at.

| species | flaw lives in | readable? | example |
|---|---|---|---|
| **assertion** | the assert | yes | `let _ = walk_to(...)`; position printed, never asserted |
| **precondition** | the setup | yes | missing fixture → `skip` instead of fail; gate passed in 0.00s |
| **duration** | test lifetime vs system counters | **no** | server stops sending chunks after 10 unacked batches; every gate disconnects first |
| **world** | the input data | **no** | light propagation gated on **superflat**, where sky light never spreads sideways |

Audit questions to carry forward:
- *Does any server-side counter accumulate past our gate's lifetime?*
- *Does the input actually contain the structure the code under test exists to handle?*

### An expected value must originate outside the code under test

`decode(encode(x)) == x` is satisfied by two symmetric misunderstandings. v735's hermetic chunk
fixtures were generated with **our own encoder** and passed throughout, then the live gate produced
49 × "unexpected end of input" — the decoder was missing the 1.16.2 biomes varint length-prefix.
Encoder and decoder shared one wrong mental model, so the round trip closed perfectly on bytes no
server would ever send.

Use captured server bytes, a JVM oracle, or a hand-decoded spec example. Where a live capture is
impractical, check the fixture in **as bytes** the first time it is validated against reality.

The same trap at the oracle level: a self-authored JVM oracle validates *the behaviour you chose to
model in it*, not vanilla's. Three implementations once agreed bit-for-bit across 16 scenarios and
all three were wrong, because all 16 happened to be flush contacts where two competing formulations
coincide. **Agreement across ports is weak evidence when the ports share an author.**

### Assertions of an absence need a control proving the detector works

"No corrective teleport", "no trailing bytes", "no dropped packet" are each only as good as the
evidence the mechanism *would* have fired. The live physics gate asserts zero corrective
`player_position` packets — and the server only validates movement once `hasClientLoaded()` is true,
so without sending `player_loaded` it silently ignores movement and returns a false green. The
permanent negative control (one 30-block teleport that **must** be snapped back) is what makes the
absence meaningful.

### Test-suite health

- **`cargo build --workspace` is not a health check** — it does not build test targets. Use
  **`cargo check --workspace --all-targets`**.
- **A test total gathered during concurrent edits is a sample, not a measurement.** The meaningful
  invariant is *zero failures **and** zero non-compiling targets*, never the absolute count. A run
  once reported "1406 passed, 0 failed" while exiting 1, because a crate failed to compile.
- **A live gate behind both a feature flag and `#[ignore]` compiles to zero tests without the
  feature** and reports `ok. 0 passed`, which is indistinguishable from success at a glance. Put the
  full invocation in the docs at every call site.

### Live-server hazards

- **Offline mode derives the account UUID from the *username*, ignoring the UUID the client sends.**
  Every test sharing a name shares one persisted player file. A mob killed that player once, vanilla
  persisted the dead state, and every subsequent join was held on the death screen — **which sends
  no chunks.** A dead player is a silent, total chunk blackout while join, keep-alives and entity
  movement all continue perfectly. Use `lodestone-testsupport`'s `unique_username`.
- That helper must be unique **by construction** (an `AtomicU64` plus pid), never derived from a
  clock. A `nanos % 1e9` version reads as a 10⁹ space and delivered ~10⁶ because the platform clock
  had microsecond resolution. The counter goes **first** in the string so the server's hard 16-char
  limit truncates the *timestamp*, not the discriminator.
- **A freshly summoned entity is not selector-visible until the next server tick.** Poll for it;
  never assert immediately. `Invulnerable:1b` additionally makes an entity **un-targetable** —
  vanilla's `TargetingConditions` rejects it — so use `NoAI:1b` for a stationary lure.
- **Vanilla's RCON client performs exactly one `read()` per request** and closes the socket unless
  `pktsize == read - 4`. Sending the frame as two `write_all` calls silently closes the connection
  after a few commands. **Write the entire frame in one call.**
- **`tick step N` does not advance entity physics; only `tick sprint N` does** — and a
  `tick sprint 1` used for registration silently consumes a tick, presenting as a phantom +1 offset.

### Resource hygiene — read this before running anything

**This machine is shared with an unrelated project.** Docker holds images, volumes and build cache
belonging to the user's other work (`mht-*`, postgres, valkey, seaweedfs).

**`docker system prune`, `docker volume prune` and `docker builder prune` would each destroy it.
Never run them.** Every cleanup action must name its target explicitly. Note also that Docker's
`name=` filter is a **substring** match, not a prefix match.

Containers are named `lodestone-<purpose>`; prefer `docker run --rm`. Reclaim disk by deleting
`target/debug/incremental` first (pure regenerable cache — it does **not** force a dependency
rebuild), then stale **own-crate** artefacts in `target/debug/deps` by mtime. **Never third-party
artefacts, never `deps` wholesale** — `cargo sweep --time N` is actively wrong here, because the
oldest mtimes belong to stable third-party deps that are still current, while our own crates
accumulate one dead content hash per rebuild.

---

## Appendix: authoritative data sources, in order

1. **Mojang's own generator** (`packets.json`, `registries.json`, `blocks.json`) — authoritative,
   works for every version ≥1.14 including 26.x.
2. **Decompiled source** — reference for behaviour only; never transliterated. 26.2 ships
   de-obfuscated, so class and method names are real.
3. **minecraft-data** — bootstrap and cross-check for **1.8–1.21.11 only**; it has no 26.x data.
4. **minecraft.wiki protocol pages** — human documentation.

**Prefer interrogating the real jar over any community dataset.** `blocks.json` contains no
collision geometry at all, and minecraft-data's `blockCollisionShapes.json` measured **stale and
incomplete for 26.2** — 92.29% of states reliably covered, 30 blocks missing by name. A spot check
would have said "looks fine." The replacement boots the real server headlessly and dumps
`getCollisionShape(...).toAabbs()` for all 32,366 states.

Where minecraft-data is still the practical choice, record why.

---

## Addendum — "Islands": subsystems that are correct but not plugged in

This is the **dominant defect class in this project** and the single most useful thing
to know when picking it up. In every case below the subsystem is individually built,
individually tested, and reaches **zero pixels** because nothing calls it. They do not
show up as failures — the tree is green, the tests pass, and the game runs.

A test suite cannot see an island. Only a **pixel gate** can: assert coverage inside the
subject's screen rect, plus a negative control (empty state, or an opposite-corner
reading) that must fail the same assertion. `bulk-models` established the pattern for
entities and `impl-net` reproduced it for status effects; copy those, don't invent one.

### Island 1 — Lighting (highest value, engine is finished)

The light engine is **exact against real vanilla**, verified cell-for-cell on a live
26.2 server: sky `0/24576` disagreements, block `0/12288`, `diff_column_light` block
`0/32768` + sky `0/32768`. Each has a negative control that genuinely fired (`5120`
suppressed-sky, `298` contaminated-world). This is the best-validated subsystem here.

**It reaches no pixels.** `lodestone-render`'s `build_batch` fills its light grid with
`UniformLight::pre_light_bridge()` — full-bright — at **3 sites**.

Fixing it is two-sided and **ordering matters**: the producer (`lodestone-client`
populating `MeshJob`'s light grid from `sections_and_light_at`) must land **before or with**
the consumer (retiring the 3 bridge sites onto the existing `WorldSectionLight`/`SkyDefault`
adapter). Retiring the bridge first turns the world black — worse than full-bright.

- **Trap:** full-bright and correct lighting *both* render a visible world, so "it still
  draws" proves nothing. Assert shadowed interiors are measurably darker than open sky.
- **Trap:** light section indexing is off-by-one **by design** — light section 0 is the
  boundary *below* the world, light section `i` covers block section `i-1`, 26 light
  sections for 24 block sections. `sections_and_light_at` takes an explicit `(n, n+1)`.
  "Correcting" this into alignment is a regression that looks like a fix.
- **Trap:** the Nether has no sky light, so `SkyDefault` must not be a blanket 15.

### Island 2 — Vanilla block textures (contradicts an explicit requirement)

The playable shell renders a **procedural colour atlas**, not vanilla textures.
`lodestone-shell/src/gpu.rs:393`, inside `RenderState::new`, unconditionally calls
`crate::blocks::build_atlas()` — a generated per-sprite base colour with a deterministic
dither so surfaces "read as textured". There is no vanilla path, no feature flag and no
fallback logic at that call site.

Meanwhile `lodestone-assets` carries a real `Atlas` / `AtlasBuilder` / `AtlasDefinition`,
and **`lodestone-render` already uses it** (`block_resolver.rs`, `texture.rs`, plus
`tests/model_census.rs` and `tests/live_gate.rs`). Vanilla assets are on disk:
`.cache/mc/26.2` (412 MB).

So this island is **narrow** — the loader exists, the consumer exists, and the two are
wired together on the render side. The shell simply never asks for it. Note
`cargo test -p lodestone-assets --lib` runs **0 tests**; the coverage lives in
`lodestone-render`'s tests, so don't read the empty result as "untested".

### Island 3 — UI surfaces (partially resolved)

Five surfaces are modelled and folded in `lodestone-game` — tab list, scoreboard,
container/inventory, boss bar, status effects. Only **status effects** has been proven
to pixels (`overlay_rasterises_to_pixels`: empty frame `0`, populated widget rect `2160`,
opposite corner `0`). The rest fold state correctly and draw nothing.

`impl-game` built a `Menus` aggregate routing the 7 container events through the proven
`ClientMenu::reconcile` seam — **consume it, do not rebuild it**. Two hazards it already
solved, both of which produce a plausible-looking *wrong* inventory rather than an error:

- Container packets are in **menu order**; `SET_PLAYER_INVENTORY` is **native order**.
  `ClientMenu::set_player_native` exists with a known-value guard asserting native slot 0
  lands at menu index **36**, not 0.
- Container size comes from **server truth** (`content_len - 36`), never a hand-written
  menu-type→size table.
- Slot layout differs per menu: window 0 is `0` result / `1..=4` craft / `5..=8` armour /
  `9..=35` main / `36..=44` hotbar / `45` offhand, while `Generic{n}` is `0..n` container /
  `n..n+27` main / `n+27..n+36` hotbar — **no armour, no offhand, hotbar not at 36**.

## Addendum — `PlayerLoaded` is encoded but never sent

`ClientAction::PlayerLoaded` exists (`lodestone-model/src/action.rs:290`) and v770 encodes
it (`adapter.rs:3225`). **Nothing produces it.**

Vanilla's server seeds a ~60-tick (~3 s) `clientLoadedTimeoutTimer` after join **and after
respawn**, and **silently ignores movement packets until it elapses** unless the client
zeroes it early (`ServerGamePacketListenerImpl.hasClientLoaded()`). Vanilla sends it
automatically with no game or UI dependency. We never do — so for the first ~3 s of every
session the server discards our movement, and any gate measuring movement in that window
measures nothing and returns a **false green**.

Three live gates work around this by sleeping ~5 s each, with comments asserting the
capability is absent (`live_physics_bot.rs:45` and `:242`, `live_second_observer.rs:317`,
`live_session.rs:110`). Those comments were true when written; the variant landed later.

- **Do not** strip the waits wholesale. `live_second_observer` waits on a genuinely
  different condition — the *observer* client receiving our entity — and collapsing the two
  yields a gate that passes on latency.
- The **`minecraft:brand` custom payload** is in the same state: encoded, tested, never sent,
  where vanilla sends it at join.

## Addendum — staleness is the most common defect here

**Five separate instances surfaced in a single session.** Every one was *true, evidenced
and correct when written*, then quoted or relied upon after the world changed underneath it:

1. A "~40 of 141 packets handled" metric quoted while ~50 packets landed (real: 91) — this
   steered an entire fleet of agents at the wrong bottleneck.
2. `lodestone-render` believing `handle.section_light` didn't exist. It does — this is what
   keeps the best-verified subsystem off screen (Island 1).
3. An assumption that a 158-type entity-geometry census existed. No such table exists
   anywhere; the thing named "census" is the *spawn* census for mob-cap.
4. A gate docstring asserting "no adapter emits any entity event", written before ~50
   packets landed.
5. Three test files asserting `ClientAction::PlayerLoaded` doesn't exist (above).

**Standing rule, and the cheapest safeguard available:** when work is gated on *"X doesn't
exist yet"*, **re-verify that X still doesn't exist** before routing around it. Staleness
needs its own check precisely *because* the original claim was honest and correct — nothing
about it looks wrong on inspection, which is why it survives review.

Corollary: prefer `cargo xtask connectedness` over any hand-derived coverage number. The
hand-derived version was wrong four times, in four different ways.

## Addendum — CLOSED: `ItemStack` components (and the fail-closed lesson that outlives it)

`lodestone_model::ItemStack` now carries decoded components: `custom_name` (network NBT),
`damage`/durability, `enchantments`, and `count`. Component types resolve through a generated
111-entry `data_component_type` id↔name table rather than hardcoded ids.

**This started as a benign-sounding model gap and was in fact a session-killer.** Equipping any
tool makes the server send `container_set_slot` carrying a component patch; v770's
`read_item_stack` fail-closed on it and the driver treated that decode error as *fatal*. In 26.2
essentially every real item carries components, so **picking up an item, opening a chest, or being
handed a tool ended the session.** The disconnect was reproduced as a negative control before the
fix — `failed to decode packet: item data components are not supported` — then re-verified live.

Three lessons worth more than the fix:

1. **Fail-closed on a forward-compatible, open-ended wire structure turns every future server-side
   addition into an outage.** The driver seam now logs loudly and *drops the single packet* on
   `AdapterError::Decode`; packets are transport-framed, so an unparsable payload never desyncs the
   next one. `Unsupported`/`Encode` stay fatal, because those are structural.
2. **A round-trip test where we own both sides cannot detect a shared misunderstanding of the wire
   format.** A self-round-trip of our own encoder passed happily while the real server disconnected
   us. This is the second time this exact rule has bitten; it is why the gates are live.
3. The clientbound patch is the *trusted, non-delimited* codec — an unknown component's payload is
   **not** length-prefixed and therefore genuinely cannot be skipped in place. Decode reads modeled
   components and, at the first unmodeled one, keeps what it has, flags `has_unmodeled`, and
   abandons the rest of that one packet.

**Still open here:** `set_creative_mode_slot` still *encodes* an empty patch, so components are
dropped serverbound on creative slot-set. That is the single extension point. Enchantment ids are
session-scoped network ids from the dynamic registry, not stable across versions.

## Addendum — neighbour-aware relight is singleplayer-only (divergence trap)

Caught during the lighting handover and worth preserving, because getting it wrong
produces a bug that **survives every test in the suite**.

On **multiplayer**, `merge_light` already carries the server's seam-complete,
cross-chunk-propagated light — the server has the whole region loaded, so its values are
authoritative and complete. Firing `compute_column_light_with_neighbours` on MP chunk
arrival would **overwrite server-authoritative light with our own partial recompute**.
That is a divergence bug, and it looks like a lighting *improvement* while it happens.

The trigger predicate is therefore **"chunk arrived AND we generated its light"** (i.e.
singleplayer / integrated server), **not** "chunk arrived".

When the SP relight does get wired, its trigger is **bidirectional**: on column `N`
arriving, relight `N` *and* every already-loaded orthogonal neighbour `A` — `A`'s facing
seam was baked while `N` was absent (treated as opaque), so it is now stale. Miss that
second half and you get a **permanent dark stripe on `A`'s face that never revisits**,
survives every geometry test, and only shows up visually.

A genuine loaded-edge (neighbour actually absent) staying dark is **correct**, and
self-heals when that neighbour streams in — provided the bidirectional trigger exists.

**What to watch:** `diff_column_light_full`'s split. Interior must stay `0`; an *edge*
count that **changes on neighbour arrival** is the fix working, not a regression.

**Routing, once light is visible and a seam artifact appears:** MP shadowed seams belong
to `merge_light` plumbing / mesher sampling; SP seams belong to the neighbour-aware
relight. Cost is ~12.5 ms/column, and the wire lives in the chunk-arrival/load path, not
in the mesher — the mesher only reads whatever merged light the handle exposes.

## Addendum — traps found during final wrap-up

### `UseItemOn` needs the 26.2 `world_border_hit` bool

`use_item_on` in v770 requires a trailing `world_border_hit` boolean added in 26.2.
**Without it the live server disconnects on decode.** This is a wire-format bug that only
a live server surfaces — a hermetic round-trip test passes happily, because our encoder and
decoder agree with each other while both disagreeing with vanilla. Fixed and covered by
`use_item_on_is_byte_exact`.

Generalisation worth keeping: for any packet where we control **both** sides of a
round-trip test, the test cannot detect a shared misunderstanding of the wire format. Only
a real server can. Prefer at least one live assertion per packet family.

### `V770ServerProtocol` must stay stateless (aliasing trap)

`IntegratedServer::bind` wraps the protocol in `Arc<P>` and clones **that same `Arc`** into
every accepted connection's spawned task. Any interior-mutable "last sent" state placed
inside `V770ServerProtocol` would therefore be **silently shared across independent
clients**.

This passes every test today, because singleplayer has exactly one connection — and
corrupts as soon as a second player joins. The fix already applied: keep the protocol a
zero-sized stateless unit struct and pass `(prev, current)` snapshots as parameters, with
the caller owning per-connection state:

```rust
fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective;
fn encode_entity_update(&self, prev: Option<&EntitySnapshot>, current: &EntitySnapshot)
    -> Vec<ServerDirective>;
fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective;  // batched, matches REMOVE_ENTITIES
```

`encode_remove_entity` takes a slice and returns **one** directive because `REMOVE_ENTITIES`
is genuinely batched on the wire (VarInt count + VarInt ids) — one packet per id would be
valid but wrong-shaped.

### An empty UI panel may legitimately draw

The tab list's empty-state control is **not zero** — the empty panel itself renders a
background. Its pixel gate therefore asserts a populated-vs-empty **delta**
(`552 → 1380` bright pixels), not an absolute zero.

This matters because the intuitive gate ("expect 0 pixels when empty") **fails against
correct code**, and the natural response is to "fix" the renderer to satisfy it — removing
a background that vanilla actually draws. Check what the surface is supposed to look like
when empty before choosing between a delta assertion and an absolute one.

Surfaces proven to pixels so far, with their evidence:

| surface | populated | empty control | corner control |
|---|---|---|---|
| status effects | 2160 px in rect | 0 (whole frame) | 0 |
| tab list | 1380 bright px | 552 (panel draws) | — |
| scoreboard | 7216 px | 0 | — |
| container (chest) | 23271 px in rect | 0 | 0 |

## Addendum — THE BIGGEST ISLAND: live server terrain never renders — **CLOSED 2026-07-29**

> **RESOLVED.** Commits `93a2c1e` (classifier swap) + `f5800d9` (two-stage gate). Verified
> independently by the director: the live gate reproduces at **2690 quads, sky `[0..255]`**,
> and the real release binary against the flat-creative 26.2 oracle reports
> **`loaded vanilla block atlas … sprites=929`, `live_cols=329`, `sections=334`,
> `quads=210124`** at 120 fps / 8.0 ms frame / 94 MB RSS. The server's world is what you see.
>
> The history below is kept because the **diagnosis** is the reusable part, and because the
> ordering trap it describes is live again for anyone touching this path.

**Read this before believing any screenshot.** When the client is run with `--window --live`,
the terrain on screen is the **locally generated demo world**, not the server's. The live
connection streams events (entities, chat, health, sounds) correctly and independently, but
**the server's chunks are never meshed**.

Two independent causes, and the second is the real one:

1. `Sim::mark_column_dirty` early-returns for live columns; the meshing pipeline reads
   `self.world`, which is the singleplayer worldgen world.
2. **The shell meshes with `DemoClassifier`, whose palette is block ids `0..=9`**
   (`AIR`, `STONE`, `DIRT`, `GRASS`, `SAND`, `WATER`, `LOG`, …). A live 26.2 server streams
   *vanilla* block-state ids in the tens of thousands. `block(id)` returns `None` for all of
   them, which `DemoClassifier::classify` maps to a **non-occluding, surface-less cell** —
   i.e. air. Meshing live chunks through it renders **near-nothing**.

**The trap, and it is a serious one:** wiring live meshing *without* first swapping the
classifier produces a pipeline that runs, produces no geometry, and **passes a lighting gate
vacuously** — an empty world is trivially "not full-bright". Any gate over live terrain must
therefore first assert *non-trivial geometry exists* (quad count, coverage) before asserting
anything about its lighting.

**Correct order:** vanilla `state_id → sprite` classifier first (`impl-render`'s
`blocks_json_registry` + `BlockAtlas`, both landed and ready — see Island 2), then the
`mark_column_dirty` live rewrite. Lighting then rides along for free, because the light read
(`sections_and_light_at`) and column geometry (`world_dimensions`) are already wired.

**What is genuinely done:** singleplayer terrain is meshed *and correctly lit* as of `3870ae1`,
gated by `shadowed_meshes_darker_than_open_sky_and_the_bridge_cannot_tell` — a test whose
full-bright control renders shadowed and open cells identically at 255 and therefore fails.
That is real, verified lighting; it just currently applies to the generated world only.

### Correction to an earlier belief in this document

An earlier framing held that "multiplayer renders full-bright terrain". **That was wrong** —
MP terrain does not mesh at all. The distinction matters: "lit incorrectly" suggests a
lighting fix, while "not meshed" points at the classifier. Chasing the former would have
produced a vacuous green.

## Addendum — the obvious `player_loaded` live gate is vacuous (design note)

`PlayerLoaded` is now auto-sent (`06adf98`), policy-suppressible via `PlayerLoadedPolicy`
(`a9ae6a2`), and re-armed on **`ClientEvent::Respawned`** as well as `Death` (`6613751`) so
portal / dimension-change / `/respawn` no longer silently re-enter the ignore-movement
window. Hermetic coverage: `cargo test -p lodestone-client --test driver` → 20/20, including
`player_loaded_suppressed_under_manual_policy` and
`player_loaded_rearms_on_respawn_without_death`.

**The live gate is deliberately not written, and the intuitive design for it does not work.**

The tempting gate is: "move immediately after join with `PlayerLoaded` suppressed, observe
the server rubber-band us back." **It never fires.** When `hasClientLoaded()` is false,
vanilla **silently drops** `MovePlayer` packets — it does *not* send a correcting teleport.
So a lone walker observes **no correction at all** during the window: local prediction
diverges freely while the server keeps us at spawn. A correction only appears *after* the
window, when the accumulated catch-up move looks illegal (too fast).

A gate built on the naive design therefore passes whether or not the fix works — the classic
vacuous green.

**Two designs that actually work:**

1. **Second observer client** — confirm the *entity* did not move server-side during the
   window, from a different connection. This is the stronger option, because it observes
   authoritative state rather than inferring from our own packets.
2. **Assert on the post-window catch-up correction spike** — real, but timing-sensitive;
   gate carefully so it cannot pass merely on latency.

`PlayerLoadedPolicy::Manual` is the negative-control lever. Put the gate in
`live_physics_bot.rs` behind the `live-v770` feature.

**General lesson worth carrying:** "the server will correct us if we're wrong" is an
assumption, not a mechanism. Before building a gate on an expected server *reaction*,
confirm vanilla actually reacts — several of its validation paths **drop silently** rather
than responding, and a gate waiting for a response that never comes cannot distinguish
success from failure.

## Addendum — two vacuous gates that were proposed and caught

Both were suggested by the director, sounded specific and testable, and would have produced
a green suite proving nothing. Both were caught by the implementing agent. They are recorded
because the *shape* of the mistake recurs.

### 1. "A 1.95-tall zombie fails a gap a 1.8-tall one clears"

Proposed as the behavioural gate for the entity-dimensions census. **It does not bite.** The
pathfinder quantises to cells via `cell_height = floor(h + 1)`, which is **2 for both** 1.8
and 1.95 — both fit any 2-high tunnel, so the assertion passes whether or not the census is
wired at all.

The working gate crosses an **integer cell boundary**: enderman 2.9 → 3 cells is blocked by a
2-high tunnel, while a deliberately-wrong 1.8 enderman clears it. The landed test is
`census_height_decides_whether_a_mob_fits_a_two_high_tunnel`, and it was bite-tested by
perturbing the fold to drop census height (enderman then resolves to 2 cells → FAILED).

**Lesson:** when the system under test **quantises**, a gate must straddle a quantisation
boundary. Two values that differ continuously but land in the same bucket are indistinguishable
by construction.

### 2. "Suppress `player_loaded`, move, and observe the server rubber-band us"

Covered in full above. Vanilla **silently drops** movement while `hasClientLoaded()` is false
rather than correcting it, so the expected reaction never arrives and the gate cannot fail.

**Lesson:** confirm the server actually *reacts* before building a gate on its reaction.

### The general rule

A gate is only worth writing if you can state **what would make it fail**, and then *watch it
fail*. Every gate that has caught a real bug in this project had a negative control that was
executed and observed — not merely described. Where a negative control is impractical, say so
explicitly rather than shipping an unfalsifiable assertion.

## How the live-world gate was actually made to bite

Worth copying, because the first attempt at stage 2 quietly produced a wrong answer and the
fix is not obvious.

**Stage 1 — geometry before lighting (non-negotiable ordering).** Assert a real quad count
from *live server chunks* **first**. With the demo palette this number was ~0, so the count
itself is what proves the island is connected. Asserting lighting first would have passed
vacuously: an empty world is trivially "not full-bright".

**Stage 2 — construct the shadow, don't hunt for it.** `forceload` the spawn column, then
RCON-fill a fully sealed stone room so the server relights the interior to sky `0`. Hunting
for a naturally dark spot makes the gate depend on worldgen.

**The subtlety that produced a wrong reading:** pushing the relight to an *already-connected*
client was unreliable — the incremental `LIGHT_UPDATE` path first read sky `251`, i.e. stale
open-sky light. The fix is to connect a **fresh** client after the fill, so the relit column
arrives as a seam-complete chunk-data packet. Sky then spans `[0..255]`.

**Negative control, executed:** `full_bright_control()` (the retired `UniformLight` bridge)
renders flat at `255` and **fails** the same shadow assertion — so the gate demonstrably
distinguishes real light from no light.

### Run it

```
cargo test -p lodestone-shell --features live --test live_world_mesh -- --ignored --nocapture
```

Needs the flat-creative 26.2 oracle on :25570 (RCON :25571) and a vanilla pack under
`.cache/mc/<version>` (or `LODESTONE_ASSETS`). Per §12.52 it **fails loudly** when those are
absent rather than skipping, and the failure message names the fix.

## Recreating the test oracles

Every live gate needs a real vanilla server; none of them mock the wire format, deliberately
(see "when we own both sides of a round-trip test, it cannot detect a shared misunderstanding
of the wire format"). The containers are **not** part of the repo state — recreate them:

```
./scripts/live-oracles/creative.sh   # :25570 game, :25571 RCON — flat/creative/peaceful
./scripts/live-oracles/terrain.sh    # :25580 — normal terrain, for light gates
```

Both run `--rm` and bind-mount their world from `.cache/mc/<name>`, which is gitignored and
**deliberately preserved** by `cleanup.sh` (expensive to refetch; `terrain.sh` also copies its
`server.jar` from the creative world, so removing `.cache/mc/creative` breaks both).

Verified end-to-end: `docker rm -f lodestone-creative`, re-ran `creative.sh`, then re-ran the
live-world gate — passed, and on a *different* spawn chunk (`-1,-1` rather than `0,0`), which
also shows the gate isn't coupled to a fixed location.

Why flat/creative/peaceful for the primary oracle: tests need to *cause* an exact block
arrangement over RCON without worldgen noise or mobs perturbing it. When a gate genuinely
needs hills, caves and section seams, use the terrain oracle rather than converting this one
— superflat makes some light gates vacuous (§12.82).

**Cleanup** (`files/cleanup.sh`, outside the repo): `--status` reports, no args removes
containers, `--images` also drops the pulled JDK images, `--deep` also drops repo scratch. It
names lodestone-owned resources explicitly and never prunes, because this host carries an
unrelated project.

## Addendum — found by *playing* the client (2026-07-27)

Three defects surfaced in minutes of real play that a green test suite had not caught. Recorded
together because the common cause is the same: **every gate we had read settled state or drew
near the origin.**

### 1. Columns dropped during the join burst (live terrain invisible up close)

Symptoms: standing on **invisible blocks** with correct collision hitboxes; the column renders
correctly **the moment a block in it is broken**; terrain **farther away renders fine**.

`Sim::mark_column_dirty` takes the live branch only when the vanilla atlas, the net client, and
`world_dimensions()` are all present — and `ClientHandle::world_dimensions`' own docstring says
it returns `None` **pre-login / pre-first-chunk**. When the guard fails it falls through to the
demo path, which returns immediately for any column far from origin. **The column is then
dropped permanently: nothing retries it and nothing re-dirties it.** Chunks stream
*nearest-first*, so the earliest burst at join is exactly the set at risk; breaking a block
calls `mark_column_dirty` again, which then succeeds.

A second candidate has the identical failure mode and may also be present: `NetUpdate::Chunk`
is only a dirty *signal*, so if it is emitted before the decoded chunk is applied to the
client's `World`, `snapshot_section_live` sees an all-air centre and the key goes to
`pending_removals` — **also a permanent silent drop**.

**The defect class, which matters more than either trigger: a column that fails to mesh at
event time is discarded forever.** The fix must make meshing failures retryable.

**Why `live_world_mesh.rs` passed anyway:** it reads *settled* state — connects a fresh client,
waits, then meshes an explicitly chosen column — and only ever used chunks `(0,0)` and
`(-1,-1)`. It never exercises the event-driven path during the join burst, which is where the
bug lives. Any replacement gate must drive the real join path, spawn far from origin, and be
**observed failing before the fix**.

### 2. The HUD ignores the resource pack

Hearts, hunger, XP bar and hotbar draw as procedural coloured quads. `lodestone-assets::gui` is
**complete and good** — `stretch` / `tile` / `nine_slice`, `.png.mcmeta` parsing, and
`GuiScaling::geometry` mirroring vanilla's `GuiGraphics` blit decomposition, tested against the
real jar — but `grep` shows **its only consumers are its own test files**. Classic island: a
correct producer that never reaches a pixel. What's missing is a GUI atlas over
`assets/<ns>/textures/gui/sprites/**` and a render path that uses it. Note 26.2 uses the modern
per-sprite layout, **not** the legacy `icons.png` sheet.

### 3. Mining does not exist

`PlayerAction` (the dig packet) is defined, and ids exist for `BLOCK_DESTRUCTION` and
`TAKE_ITEM_ENTITY`, but **nothing drives them**: no break-time model, no destroy progress, no
dig state machine, no pickup handling. Inventory/container state, by contrast, is substantial
(`container.rs`, `menu.rs`, `click.rs`, `item.rs`, `recipe.rs`, `reconcile.rs`) — though it
should be checked for the same island problem before more is added.

### The transferable lesson

Gates that read settled state cannot see ordering bugs, and gates near the origin cannot see
placement or distance bugs. **Playing the client for five minutes found three defects that
26 green suites did not.** Keep a manual-play pass in the loop; it is not redundant with tests.

## Addendum — the full-cube assumption (found by the user, 2026-07-27)

The user play-tested and diagnosed this himself: *"the grass is also not transparent, i think you have
an assumption right now that every block is a full block?"* Correct, and it is the largest remaining
renderer defect.

`crates/lodestone-shell/src/blocks.rs` `classify()` returns

```rust
Cell { occludes: bool, surface: Option<Surface { sprites: [SpriteId; N] }>, .. }
```

— **full-cube-only by construction.** Every non-air block becomes an occluding cube with one sprite
per face: no geometry, no alpha, no render layer, no tint, no per-face UV/rotation.

**And the machinery to do it properly already exists, unused** — the same island pattern as the GUI
atlas: `lodestone-render/src/models.rs` (baked quads, `tint_index`, `is_full_cube`),
`model_pipeline.rs` (pipeline per `RenderLayer`), `translucency.rs` (`RenderLayer`, `SortViewpoint`,
`TranslucentMesh`).

Observed consequences, all one root cause:
- **Water renders as an opaque grey cube.** The user's original spawn was deep ocean (confirmed by
  RCON: water at every probe, sea level y=62), so the sea read as a flat grey stone plain and the
  camera inside a water block showed a screen-filling grey blob.
- **Cross-model plants (short_grass, kelp, seagrass) render as solid pillars** and wrongly occlude
  their neighbours, because `occludes` is a flag rather than a property of the geometry.
- **Blocks "look rotated wrong"** — per-face sprite assignment ignores model UV and `rotation`.
- **Foliage is unnaturally dark** — `tint_index` quads never receive biome colours.

Fixing it means resolving each state id to its baked model, emitting those quads, deriving occlusion
from `is_full_cube`, splitting into opaque/cutout/translucent layers with sorting, and applying biome
tint. Collision must **not** be derived from render quads — vanilla collision uses the block's
collision shape, which is a different thing.

## Addendum — fail-closed item decode kills the session

Found by `impl-game` while gating mining live. Equipping any tool makes the server send
`container_set_slot` carrying an item with a **data-component patch**; v770's `read_item_stack`
fail-closes on component patches and the driver treats the decode error as **fatal**.

**In 26.2 essentially every real item carries components, so picking up an item, opening a chest, or
being handed a tool ends the session.** It was invisible until someone equipped a tool during live
play.

Two lessons, both instances of rules already in this file:
- *When we own both sides of a round-trip test, it cannot detect a shared misunderstanding of the
  wire format.* A self-round-trip of our own encoder passes happily while the real server
  disconnects us.
- **Fail-closed on a forward-compatible, open-ended wire structure turns every future addition into
  an outage.** Unknown components must be skippable; an undecodable item must degrade loudly, not
  tear down the connection.

## Addendum — mining landed, with one genuinely surprising number

`bd7c51c`. Break-time replayed at **f32 fidelity**, matching the server tick-for-tick. The result
that would look like an off-by-one to a reviewer: **stone bare-handed breaks at tick 151, not the
textbook 150**, because `1/1.5/100` in f32 sums to slightly under 1.0 after 150 additions. Diamond
on stone is 6 ticks. Both server-confirmed over RCON (bare-hand stone measured at 7.99 s).

Do not "correct" 151 to 150.

---

# Addendum — final play-test round (what landed, what is left)

This is the last block of work in the session. It is written to be read on its own: each item
says what was **measured**, where the code is, and what remains. Three items were fixed and
committed; five are open and each has a diagnosis rather than a guess.

## Landed this round

| commit | what | evidence |
| --- | --- | --- |
| `0cc9534` | kelp/seagrass carry water; chunk-border water seams heal | jar-derived 5-class list; falsified the old test first |
| `7725aa3` | block updates dirty at **section** granularity, not whole columns | 4 unit tests incl. the stale-face case and a 4096→27 bound |
| `dc10e49` `cbf93cb` `d26cf14` | progressive mining crack, following each block's **baked model** (slabs, stairs, cross-plants) rather than a synthetic cube | live screenshot: crack on the target block's real face, neighbours clean |
| `ffcc763` `2970a64` | distance fog — the thing that hides the render-distance edge | `fog_gate` with its **negative control observed firing**; live shot of the treeline dissolving |
| `b49f8eb` `95a0ee8` | fog sized to the configured render distance, sky colour given one home | live at `--rd 4`; 4 tests incl. fog-inside-far-plane |
| `69f66c2` | submerged water/lava fog, driven from the physics fluid state | reads the bit-exact producer, not a local bool |
| — | animated block textures (lava, fire, portals) | `animated_block_pixels` gate |

### A portability bug worth not reintroducing (from the fog work)

The first fog wiring gave the model shader a **5th bind group**. It ran on this M5 — which
reports `max_bind_groups` of 8 — and failed the hermetic GPU gate, because wgpu's *default* limit
is **4** and the model shader already spends four on camera/atlas/palette/anim. A 5-group shader
compiles and then fails validation on any 4-group adapter, so this would have been a startup
crash for other people and never for us. It was caught by trusting the gate over the machine.

The fix folded fog into the **group-0 camera uniform** (`ModelCameraUniform = CameraUniform +
FogUniform`), which also left every `camera_bind_group` caller's signature unchanged. **Adding a
bind group to the model shader is not free — check the limit, not the adapter.**

### Why the fluid fixes are worth re-reading before touching the mesher

**The mesher and the snapshot view were correct in both cases. Do not rewrite them.**

* **Kelp/seagrass** had no air pocket bug in the fluid mesher. Vanilla gives these blocks water
  through a hardcoded `getFluidState` override, **not** a `waterlogged` blockstate property, so a
  property-driven classifier is structurally unable to see it. Exactly five classes do this in
  26.2 — `KelpBlock`, `KelpPlantBlock`, `SeagrassBlock`, `TallSeagrassBlock`, `BubbleColumnBlock`
  — extracted with:

  ```python
  re.finditer(r'(?:protected|public) FluidState getFluidState\(.*?\)\s*\{(.*?)\n   \}', src, re.S)
  # keep bodies containing 'Fluids.WATER' and not containing 'WATERLOGGED'
  ```

  over `.cache/mc/26.2/src/net/minecraft/world/level/block/**/*.java`. An earlier `awk` attempt
  found nothing because the method is `protected` and its closing brace is indented three spaces.

* **The chunk-border "water wall"** was staleness, not geometry. `mesh_fluids` consults signed
  neighbour coords and `snapshot_section_live` requests all 27 slots — both correct. The defect
  was that a column meshed while its neighbour was absent baked its seam against air, and nothing
  ever re-meshed it, so spiral loading left a **one-sided** wall at every border. Two visual
  consequences of one missing fluid neighbour: the side face is emitted with the *flow* texture,
  and `neighbor_height_at` returns 0.0 so the corner heights collapse into a ledge.

### The section-granularity ruling (read before adding another dirty signal)

`ClientEvent::ChunkLoaded` had been doing two jobs — "a column arrived" and "a block changed".
They are different invalidation units:

* a column arrival dirties the column **and its 8 horizontal seams**;
* a block change dirties one section **and only the neighbours the changed cell physically
  touches**.

Conflating them costs ~216 section meshes per redstone tick. Gating the seam heal to first
arrival fixes the cost and reintroduces a real defect: a server-side break at local `x=15` never
re-meshes the neighbouring column, so **mining at a chunk border leaves a stale face**.

`ClientEvent::SectionBlocksChanged { section, blocks }` now carries block updates. It is a
*dirty-region* signal — it names where changed and carries **no block data**, because world state
must stay queryable from the client's `World` rather than reconstructible from a bounded,
backpressuring channel. The section-relative coords are what let the consumer tell an interior
edit from a boundary one. The filter is extracted as the pure `dirty_sections_for_blocks` in
`crates/lodestone-shell/src/sim.rs` so it tests without a GPU.

---

## Open 1 — Block breaking: the crack is too white and every block cracks

Two independent defects, both confirmed at source. They present as one symptom.

**What already works, so nobody rebuilds it:** the crack overlay is drawn (`dc10e49`, `cbf93cb`,
`d26cf14`) and it is *geometrically* correct — `CrackResolver` consumes the target state's **baked
model quads**, so the crack follows slabs, stairs and cross-plants rather than a synthetic cube,
and it draws on that block only. It is fed from the live client-owned world (`net.block_at`), not
the demo world. Verified on pixels against a live server.

Three things are wrong, and two of them share one root cause.

### 1a. The overlay blend function is wrong

Ours is alpha-blended (`crates/lodestone-shell/src/gpu.rs:312`). Vanilla's `destroy_stage_0..9`
are drawn on a dedicated pipeline whose blend is **a doubled multiply**, not alpha
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/RenderPipelines.java:434`):

```java
public static final RenderPipeline CRUMBLING = register(
   RenderPipeline.builder(GLOBALS_SNIPPET)
      .withLocation("pipeline/crumbling")
      .withColorTargetState(new ColorTargetState(new BlendFunction(
          BlendFactor.DST_COLOR, BlendFactor.SRC_COLOR,   // colour: src·dst + dst·src
          BlendFactor.ONE,       BlendFactor.ZERO)))      // alpha
      .withDepthStencilState(new DepthStencilState(
          CompareOp.GREATER_THAN_OR_EQUAL, false, 1.0F, 10.0F))
      .build());
```

So the crack **multiplies darkness into the block underneath**. Alpha-blending the same texture
instead *adds* its light texels, which is precisely the "too white" the play-test saw.

Three things to port, not one:

* **Blend**: `src_factor = Dst`, `dst_factor = Src` on colour; `One`/`Zero` on alpha.
* **Depth write off**, depth compare pass-equal-or-nearer. Note vanilla is reversed-Z
  (`GREATER_THAN_OR_EQUAL`); we use `[0,1]` DirectX-style depth (§7 of `DESIGN.md`), so ours is
  `LessEqual`.
* **Depth bias — and the transcription above was BACKWARDS.** The record is
  `DepthStencilState(CompareOp depthTest, boolean writeDepth, float depthBiasScaleFactor, float
  depthBiasConstant)`, so `(GREATER_THAN_OR_EQUAL, false, 1.0F, 10.0F)` is **slope 1.0, constant
  10.0**, not "constant 1.0, slope 10.0". Verified against
  `.cache/mc/26.2/client-src/com/mojang/blaze3d/pipeline/DepthStencilState.java`.
  Both also **negate** under our depth convention: vanilla is reversed-Z, where biasing toward the
  viewer is positive; we use `[0,1]` DirectX-style depth, where it is negative — the same sign flip
  that turns `GREATER_THAN_OR_EQUAL` into `LessEqual`. So ours is
  `DepthBiasState { constant: -10, slope_scale: -1.0 }`.
  This is what stops the overlay z-fighting with the block face. Omitting it produces shimmer that
  reads as a mesher bug and sends the next person hunting in the wrong crate.

  > A four-argument constructor where two adjacent floats mean different things is exactly the kind
  > of call a summary gets wrong. **Read the record definition, not the call site.**

### 1b. There is no per-block hardness, so obsidian and bedrock crack like dirt

**This one root cause produces two separate visible symptoms**: every block cracks at the same
rate including ones that will never break, *and* the crack **pulses through stages instead of
filling smoothly** (found independently by `impl-shell` while gating the overlay on pixels).
`lodestone-game::mining` is **complete and vanilla-exact** — hardness, dig speed, the 30/100
correct-tool divider, `destroy_stage()` returning 0–9, and `hardness == -1.0` meaning unbreakable.
It is wired into the shell (`sim.rs:347`). What it is fed is not:

```rust
// crates/lodestone-shell/src/sim.rs
const LIVE_DIG_HARDNESS: f32 = 0.05;   // one constant, every block
```

The **break timing is correct anyway**, and the reason is worth understanding before "fixing" it:
the shell exploits the server's delayed-destroy rule (a `START` followed by an early `STOP` makes
the server latch the dig and finish it on its own block-accurate timer). So blocks break at the
right moment without the client knowing any hardness.

The **crack animation** is what is wrong: it runs on the fake 0.05, so it advances at one rate for
everything, and it advances for blocks that will never break. Punching bedrock draws a full
crack sequence and then nothing happens.

**The fix is a data table, not logic.** `crates/protocol/v770/src/generated/` already holds
`collision_shapes.rs`, so the extraction pattern is proven: dump from the real server jar
(`SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();` then walk every one of the 32,366
block states), **not** from minecraft-data, which lags and is incomplete for 26.x (§12.10 in
`DESIGN.md` measured it at 92.29% for collision shapes).

Per state you need `destroySpeed` (`-1.0` for bedrock and other unbreakables) and the
correct-tool predicate. Then feed `BreakInputs { hardness, correct_tool, .. }` from the block
under the crosshair instead of the constant. Three things fall out for free: unbreakable blocks
show no crack at all, `destroy_stage()` advances at per-block rates, and the pulsing stops
because the stage progression finally matches the real break time.

**Sequencing warning.** `LIVE_DIG_HARDNESS` is *load-bearing for the verified break timing* — it
is what keeps the early-`STOP` delayed-destroy trick working. Do not tune it to fix the crack
cadence; the two must be replaced together in one change, or you trade a cosmetic bug for a
functional one. `impl-shell` hit exactly this and correctly declined to perturb it.

**Do not "correct" the existing 151-tick bare-hand stone result to 150.** It is f32 accumulation
and it is server-confirmed (see the mining addendum above).

---

## Open 2 — Mobs do not hold anything (island #8)

The chain is **fully plumbed and stops one hop short of the screen**:

```
SET_EQUIPMENT (v770/src/adapter.rs:1996)
  → ClientEvent::EntityEquipmentUpdated (lodestone-model/src/event.rs:833)
  → EntityView.equipment (lodestone-client/src/state.rs:161, slot-replacing fold at :733)
  → nothing
```

`grep -rn "equipment" crates/lodestone-render/src crates/lodestone-shell/src` returns **zero
hits**. This is the island pattern in its usual form: the state is live and correct, and no
consumer exists.

What is missing is the draw:

* `EntityDraw` carries only `type / feet / yaw / scale` — there is no channel for held or worn
  items. Widening it is the first step, and it is the same blocker `impl-entity` cited when it
  *correctly refused* to fold the other stranded entity events (see "Open 5" below).
* Held items need the item model's `display.thirdperson_righthand` transform composed with the
  mob model's arm part pose. Armour needs the humanoid armour layer models.
* This shares its prerequisite with §7.1 items 1, 2 and 6 — **all four are blocked on the same
  missing item-model render pass.** Doing that pass once unblocks hotbar icons, container slots,
  the first-person hand, and mob equipment. It is the highest-leverage remaining render work.

---

## Open 3 — Colours look washed out (diagnosed, not yet confirmed at pixels)

**Leading hypothesis, with source evidence on both sides of the mismatch.**

The terrain shader writes **linear** colour and its own comments say so, explicitly assuming the
swapchain performs the sRGB encode (`crates/lodestone-render/src/model_pipeline.rs:539-556`):

> *"The atlas is an `_srgb` texture, so `textureSample` returns linear-light texels … re-encoding
> on the sRGB surface …"*

The atlas is indeed `Rgba8UnormSrgb` (`texture.rs:444`), so sampling returns linear — that half is
right. But the surface is configured from wgpu's default:

```rust
// crates/lodestone-render/src/target.rs — SurfaceTarget::new
let config = surface.get_default_config(adapter, width, height)?;
```

and `get_default_config` takes `*caps.formats.first()?` (`wgpu-30.0.0/src/api/surface.rs:92`),
while wgpu-hal's Metal backend lists **`Bgra8Unorm` first**, ahead of `Bgra8UnormSrgb`
(`wgpu-hal-30.0.0/src/metal/adapter.rs:434`). So on this machine the shader's stated assumption is
false: nothing performs the encode.

**Confirm before fixing** — this is a hypothesis about the running configuration, and the
project's own rule is that a check disagreeing with an observation is the suspect:

1. Log `target.format()` at startup. One line, and it settles the question.
2. If it is non-sRGB, prefer an sRGB format:
   `caps.formats.iter().copied().find(wgpu::TextureFormat::is_srgb).unwrap_or(caps.formats[0])`,
   *or* apply `linear_to_srgb` at the end of every fragment shader. Do exactly one of the two —
   doing both double-encodes and washes the image out in the other direction.
3. **Measure by location, never by average.** Averaging previously merged two spatially distinct
   tint populations into a number describing neither (§12.99 in `DESIGN.md`). Cluster pixels by
   where they are, then compare.

Secondary suspect if the format turns out to be sRGB already: the tint palette path at
`model_pipeline.rs:616` does `srgb_to_linear(linear_to_srgb(rgb) * tint_col)`, which is correct as
written but is the only other place a transfer function is applied by hand.

**Note the two full-bright light bridges are not the cause here.** `lodestone-render`'s own mesher
still uses `UniformLight::pre_light_bridge`, but the *shell* path meshes with real server light via
`sections_and_light_at`, and its `shadowed_meshes_darker_than_open_sky` test proves the bridge
cannot pass. The shell is what the player runs.

---

## Open 4 — Entity shadows still do not exist

Unchanged from §7.1 item 5, restated because it was asked for directly. `grep -rn "shadow"` over
`lodestone-render/src` and `lodestone-shell/src` finds only a comment at `gpu.rs:1882` and the
*lighting* tests in `mesher.rs` — nothing renders a drop shadow, and nobody is assigned to it.

Vanilla's is a soft dark oval projected onto the geometry beneath the entity, radius scaled by
entity size and alpha faded by height above the ground, drawn per receiving block face (so it
follows stairs and slabs rather than floating flat). Without it mobs read as hovering, which is
what the play-test reported.

---

## Open 5 — Stranded entity events, and why they should stay stranded for now

`EntityAnimation`, `EntityDamaged` / `EntityHurtAnimation`, `MobEffectApplied` / `Removed`,
`EntityStatus`, `EntityPassengersChanged`, `EntityLeashed` and `EntitySound` all decode and emit,
and none reach a consumer.

**This is deliberate and should not be "fixed" by folding them.** `EntityDraw` has no channel for
tint, animation frame, particles or leash geometry, so folding them now produces write-only store
fields nothing reads — connectedness theatre that improves the metric and changes no pixel. Widen
`EntityDraw` first (which Open 2 needs anyway), then fold.

---

## Open 6 — Underwater — **CLOSED** (`69f66c2`), except the per-biome fog colour

Measured, not eyeballed: on the user's underwater screenshot the blue cast down the frame as
`B − R` is **58 / 61 / 49 / 62 / 66 / 59**, far to near. Flat, and no pixel anywhere reaches white
(channel maxima 130/173/231).

**That flatness clears the obvious suspect and is worth recording as verified.** If interior
water↔water faces were being emitted, every extra block of water between eye and target would
blend another layer and the cast would deepen with distance. It does not — we get exactly one
layer of tint however much water we look through. **Fluid face culling is correct.**

What is missing is fog, not geometry. Vanilla does not build the submerged look out of stacked
translucent quads; `FogRenderer` uses a short, exponential, biome-coloured water fog plus a
heavily reduced view distance, so distant terrain fades to solid fog colour and vanishes. We now
have distance fog, so what was left was the water fog itself plus vanilla's full-screen
`textures/misc/underwater.png` overlay.

**This closed the way it should have.** `Sim` now recomputes
`lodestone_physics::compute_fluid_state` once per physics tick, against *the same collision view
movement collided against*, and `Sim::fog_settings()` selects on it: short near-eye water fog when
submerged, near-opaque lava fog in lava, render-distance sky fog otherwise. `app.rs` reconciles
against the applied fog and re-uploads only on a sky↔water↔lava crossing.

The important part is what it did **not** do: it reads the bit-exact physics producer instead of
inventing a local submerged boolean. That flag also gates the `underwater.png` overlay, the
`ambient.underwater.*` sounds (in the generated sound table, still never triggered) and swimming
physics — four consumers that would each have picked their own answer for eyes-exactly-at-the-
surface, and only one would have matched vanilla. **Anything else that needs "am I submerged"
should read the same producer.**

Two deliberate gaps remain, both recorded rather than faked:

* **Per-biome water fog colour.** Uses the default ocean colour; the biome colour is not reachable
  from the shell yet.
* **The `underwater.png` full-screen overlay** is still not drawn. It can be now — the flag it was
  waiting on exists.

---

## The one process rule that found all of this

Every defect in this addendum was found by **looking at pixels**, and none by a test. The tests
were green throughout, and honestly so — a crate's own suite is a closed loop and structurally
cannot observe that nothing ships its output.

The counter-measure that works is not more tests: **nothing is done until something on screen
changes.** In practice that means assigning work end-to-end (one owner from data through to draw)
rather than by crate, and asking each owner the one question that has now found nine islands:
*what actually consumes you?* — treating "nothing" as a defect report rather than a status update.

Two build-time traps that will otherwise cost an hour each:

* **`--features live` is mandatory for multiplayer and fails silently without it.** The client
  still starts, renders the demo world, and reports a plausible `chunks=169` while whispering
  `no version family compiled in for protocol 776` into the log.
* **The binary is `lodestone`, not `lodestone-shell`** — the `[[bin]]` name differs from the crate
  name.
* **Never `git add -A` in this repo.** It is a single shared checkout with no per-agent worktrees,
  so a blanket stage sweeps whatever anyone else has mid-edit into your commit. This clobbered
  in-flight render work three times and destroyed a `lib.rs` edit once. Stage explicit paths
  (`git add <path>`) or hunks (`git add -p`), always.
* **`cargo build --workspace` is not a health check** — it skips test targets, so a crate whose
  lib compiles and whose lib-test does not reports green. Use `cargo check --workspace
  --all-targets`, and treat any count gathered while another agent is mid-edit as a sample rather
  than a measurement.

---

# Addendum — play-test round 2 (2026-07-27), and the standing backlog

Reported by the user from real play. Split into **(A) defects with a source-level cause already
confirmed**, **(B) defects diagnosed but unconfirmed**, and **(C) the standing backlog** — work
that is wanted and not yet started. Nothing here is fixed yet.

## A. Confirmed at source

### A1. Physics and rendering disagree about what counts as water

**Two symptoms, one cause.** The user reported that waterlogged blocks (a) do not let you swim and
(b) show you as *out of* the water when your eye is inside them.

The renderer knows about waterlogging. `render/block_models.rs:128` classifies any state with
`waterlogged=true` as carrying a water source, and `0cc9534` added the five classes that get water
from a hardcoded `getFluidState` override with no blockstate property at all (`KelpBlock`,
`KelpPlantBlock`, `SeagrassBlock`, `TallSeagrassBlock`, `BubbleColumnBlock`).

**Physics knows none of it.** `grep -rn "waterlogged" crates/lodestone-physics/src/` returns
**zero hits**. The classification the physics side actually uses is the shell's `CollisionView`:

```rust
// crates/lodestone-shell/src/collision.rs:79   (offline)
fn is_water(&self, x: i32, y: i32, z: i32) -> bool { self.block_at(x, y, z) == id::WATER }

// crates/lodestone-shell/src/collision.rs:182  (live)
fn is_water(&self, x: i32, y: i32, z: i32) -> bool { self.water.contains(&self.block_at(x,y,z)) }
```

An exact block-id match, and a set of "vanilla water state ids" (`collision.rs:118-120`). Neither
can see a waterlogged stair, kelp, seagrass or a bubble column.

Because `69f66c2` correctly routed the submerged flag through the **physics** producer, this one
wrong classifier now drives *everything*: swimming, the fog, and (once drawn) the overlay and the
ambient sounds.

**This is the exact failure `69f66c2`'s own commit message warned about** — two consumers inventing
their own answer to "is this water" and disagreeing. The fix is not to patch `is_water` locally but
to give the **water-source classification one home** and have both the mesher and `CollisionView`
read it. The jar-derived five-class list and the `waterlogged` property check already exist on the
render side; that logic is the thing to lift, not to duplicate.

### A2. Fog is never applied to entities or to the terrain-block path

The user reported that mobs in water are drawn on top of the water and the underwater effect does
not apply to them.

```
grep -n "fog\|Fog" crates/lodestone-render/src/entity_pipeline.rs crates/lodestone-render/src/block.rs
→ 0 matches
```

**Only `model_pipeline.rs` applies fog.** `entity_pipeline.rs`, `block.rs` (the terrain path) and
`crack_pipeline.rs` have no fog term at all. So every mob renders unfogged regardless of depth,
which is precisely "the underwater effect does not apply to them".

Note the constraint before fixing it: fog rides in the **group-0 camera uniform**
(`ModelCameraUniform = CameraUniform + FogUniform`) specifically because the model shader is at
wgpu's 4-bind-group floor. Adding fog to the entity shader must follow the same route — fold it
into that shader's existing camera uniform. **Do not add a bind group** (see the M5 portability bug
recorded in the previous addendum: a 5-group shader validates here and fails everywhere else).

Draw **order** is a second, separate question from the fog term, and the report mentions both:
entities are drawn with no ordering relative to translucent water. Vanilla draws entities before
the translucent pass. Fixing fog without fixing order leaves mobs correctly tinted but still
punched through the water surface.

## B. Diagnosed, not yet confirmed

### B1. Drowned (and probably every mob variant) renders as its base mob

Reported: drowneds look like ordinary zombies. Expected cause is variant model/texture resolution —
`drowned` has its own model and texture, not zombie's. Related open item: §7 "Never started" records
that ~130-150 base mob meshes are hand-written `LayerDefinition` classes in vanilla with **no data
path**, and that only the *mechanism* is proven (via pig), not the individual meshes. Confirm which
of the two this is — a missing mesh, or a texture/variant lookup falling back — before building
anything.

### B2. Mob walk animation is wrong in three specific ways

Reported: legs move too fast, arms are not held in front, no held items.

* **Legs too fast** — the limb-swing amount/speed feeding `Skeleton::pose`. Vanilla scales limb
  swing by distance travelled per tick, not by wall time.
* **Arms not in front** — zombies use a *raised-arm* humanoid pose. `entity_anim.rs`'s known
  divergence list already records that humanoid swim/crouch/ride/fall-flying/item poses are
  **unported**; the zombie arm pose belongs to that same gap.
* **No held items** — this is Open 2 (mob equipment), already blocked on the item-model render
  pass, which is in progress.

## C. Standing backlog (wanted, not started)

Ordered by the player-visible value per unit of work, which is not the order of difficulty.

1. **Block breaking is wrong — too fast — and needs tool/durability input.** Distinct from the
   crack-cosmetics work already diagnosed in Open 1. `BreakInputs` carries `tool_speed`,
   `mining_efficiency`, `correct_tool`, `haste_amplifier`, `mining_fatigue` and `submerged`
   (`lodestone-game/src/mining.rs:48-85`) and **the shell feeds none of them** — it passes one
   constant hardness plus `is_air`/`on_ground`. So every tool digs at bare-hand speed and the
   client-side rate is wrong for every block. The per-state hardness table now exists; the missing
   half is reading the **held item's** tool component and durability.
   **Read the `LIVE_DIG_HARDNESS` sequencing warning in Open 1 before touching this** — that
   constant is load-bearing for the currently-*correct* break timing, and it must be retired in the
   same change that supplies real inputs, never tuned separately.
2. **Air-supply bubbles.** The player has no indication of remaining air underwater. Vanilla draws
   a bubble row above the hunger bar from the `air` field. The submerged flag exists now
   (`69f66c2`); this needs the air value plumbed and the GUI sprites drawn.
3. **Vanilla inventory and tab-list UI, using the resource-pack GUI textures.** Both currently draw
   procedural quads. `lodestone-assets::gui` is complete and gated against the real jar
   (`stretch`/`tile`/`nine_slice`, `.png.mcmeta`, `GuiScaling::geometry`) and — per the island
   table — its only consumers were its own tests. 26.2 uses the **modern per-sprite layout**, not
   the legacy `icons.png` sheet. Container slot icons are already queued behind the item-model
   pass; this is the surrounding chrome.
4. **Crafting.** Not started. `lodestone-game` already has `recipe.rs`, `click.rs` and the
   `ClientMenu::reconcile` seam that `impl-game` built — **consume those, do not rebuild them** —
   and mind the documented `MenuKind` slot-order trap: window 0 is `0` result / `1..=4` craft /
   `5..=8` armour / `9..=35` main / `36..=44` hotbar / `45` offhand, while `Generic{n}` has no
   armour or offhand and its hotbar is *not* at 36. A constant offset here draws a plausible,
   wrongly-transposed inventory that reads as an art bug rather than a logic error.

## The pattern these keep re-proving

A1 and A2 are both the same shape as everything else in this file: **the knowledge exists in the
tree and one consumer doesn't read it.** A1 has a correct water classifier that physics doesn't
use; A2 has a working fog term that two of four pipelines don't apply. Neither is a missing
algorithm. Before building anything for these, ask the question that has now found nine islands:
*what actually consumes this?*
