# Block entity renderers

**Issue:** [#23](https://github.com/matteopolak/lodestone/issues/23) — still open; chest, skull,
standing/wall sign text, and the bell body/rim are landed and wired (see
[Skull](#skull-skeleton-wither-skeleton-zombie-creeper-player),
[Sign](#sign-standingwall-text) and [Bell](#bell)), the rest are not. **The "skull's screen half is
prepared but not yet wired" line this paragraph used to carry was itself stale** by the time this
file was next read — `a8068c5` wired it in the same session that sentence was written and nobody
came back to fix the summary at the top, exactly the staleness class `CLAUDE.md` warns is the most
common defect in this repo's own written record. Read the per-type sections below, not this
paragraph's memory of them.

**Bell is now fully wired, including its `BLOCK_EVENT` trigger, and the paragraph that used to sit
here saying otherwise was itself stale** — the live install landed before this note was next read,
and the shake trigger landed with `BellShakes` (see [Bell](#bell)). Both halves of the gap this
paragraph named are closed.

## The real vanilla scope, from the registration list — not the issue's guess-list

`BlockEntityRenderers.java`'s static block
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/blockentity/BlockEntityRenderers.java:34-61`)
is the authoritative list: 26 `register(...)` calls (chest counts once for three block types).

**"Is a block entity" and "needs a block-entity renderer" are two different sets, and the smaller one
is the one this issue is about.** `BlockEntityTypes.java` registers **49** types; only **26** of them
get a renderer. The 23 with none — furnace, jukebox, dispenser, dropper, brewing stand, daylight
detector, hopper, comparator, barrel, smoker, blast furnace, jigsaw, beehive, the four sculk types,
chiseled bookshelf, crafter, creaking heart, command block, test block, potent sulfur — are drawn
entirely by their ordinary block model. So a census of `block_entity_types` is **not** a work list
for this issue; it is nearly twice the size. Count `register(...)` calls, not registry entries.

Issue #23's own list — the one this doc used to quote as "still absent" — was hand-recalled and is
wrong in both directions:

- **`beds` are not on the registration list at all** — and, stronger than that, **`BED` is not in
  `BlockEntityTypes.java` either**, so a bed is not a block entity in 26.2 by any definition. A bed
  is a real block model (`assets/minecraft/models/block/*_bed_head.json` has genuine geometry,
  verified against the real jar), same shape as the sign correction below. There is no `BedRenderer`
  anywhere in `client-src`. Nothing to build here.
- **`item frames` and `end crystals` are not block entities.** Both are `Entity`, drawn by an
  `EntityRenderer`, not a `BlockEntityRenderer` — out of this issue's scope entirely, tracked
  wherever entity rendering is. Re-derived from the class declarations rather than from memory:
  `net/minecraft/world/entity/decoration/ItemFrame.java` is `public class ItemFrame extends
  HangingEntity`, and `net/minecraft/world/entity/boss/enderdragon/EndCrystal.java` is
  `public class EndCrystal extends Entity`. Note the *directory* is the tell — both live under
  `world/entity/`, where every real block entity lives under
  `world/level/block/entity/`.
- The list is also missing several real entries the issue never mentioned: `mob spawner`
  (`SpawnerRenderer`, a miniature spinning entity inside the cage), `piston head`
  (`PistonHeadRenderer`), `end portal`/`end gateway` (their own full-bright shader effects, not
  cuboid rigs), `beacon` (a light-shaft pipeline, not a cuboid rig), **`skull`** (see below),
  `structure block`/`test instance block` (creative/dev-only, not player-facing — out of scope),
  `campfire` (items cooking on top), `brushable block` (suspicious sand/gravel), `trial spawner`,
  `vault`, and two 26.x additions, `copper golem statue` and `shelf`.

So the honest count is: **3 of 26 registrations landed** (chest/ender chest/trapped chest all through
`ChestRenderer`), **5 more landed and wired end to end** (skull; sign text for **both** sign
registrations — `SIGN` and `HANGING_SIGN` — geometry excepted since a sign's board is a real block
model; the bell body/rim; and the shulker box — see [Shulker box](#shulker-box)), plus the banner, the
lectern and the campfire, so **11 of 26** — wall banners share the `BANNER` registration with standing
ones. The rest are still absent. Picking the next few should read this list,
not the original issue body.

**The registration list had two entries this document's "what is not built" section never mentioned
either way: `LECTERN` (`LecternRenderer`, the open book on a lectern) and `CONDUIT` (`ConduitRenderer`,
the shell/eye rig)** — a *silent* gap, which is worse than a recorded one. The lectern has since
landed (see [Lectern](#lectern)); the conduit has not. **Re-derived from
`BlockEntityRenderers.java`'s own `register` calls rather than from this paragraph**, because the
ledger has been wrong twice: `CONDUIT` really is registered, at the call between `SHULKER_BOX` and
`BELL`, so it is in scope and simply unbuilt — not a phantom.

**One claim in that same paragraph was wrong and is worth keeping as a correction**: it said porting
`BookModel` "covers two registrations", because `LecternRenderer` and `EnchantTableRenderer` bake the
same `ModelLayers.BOOK` layer. That is true of the **mesh** and false of the **work**. A lectern's
`BookModel.State` is a compile-time constant; the enchanting table's is a client-simulated animation
state machine with its own `open`/`flip`/`rot`/`time` counters, none of it on the wire. One rig, two
very different jobs — the enchanting table is a whole separate task on top.

## What it is

The cuboid rigs vanilla's `BlockEntityRenderer`s draw for blocks whose block model **does not fully
describe them**. Chest and skull are the total-absence case — their block models have zero elements,
so before this work they were a hole in the world. Bell is the partial case: its block model has real
geometry for the attachment frame, but the swinging body/rim comes from `BellRenderer` alone, same as
chest and skull in kind, just not in degree. The lectern is the far end of that spectrum: its shelf,
base and posts are *all* real block models, and only the open book on top comes from a renderer.
Today: chests (single, double left, double right; every material; the lid animation), skull/head
geometry (five of vanilla's seven types), the bell body/rim (see [Bell](#bell)), the shulker box (see
[Shulker box](#shulker-box)), standing/wall/hanging sign text, the banner with its pattern layers, and
the lectern's book (see [Lectern](#lectern)).

This is not a nice-to-have layer over an existing box. A 26.2 chest has **no block model at all** —
`assets/minecraft/blockstates/chest.json` points at `block/chest`, and that file is verbatim:

```json
{ "textures": { "particle": "minecraft:block/oak_planks" } }
```

Zero elements. Every visible triangle of a chest comes from `ChestRenderer`, so before this landed a
chest was a **hole in the world**, and no terrain metric could see it: `sections_drawn`,
`total_quads` and every pre-existing pixel gate are byte-identical with and without chests drawing.
That is why chest was first rather than sign.

**The converse is the trap, and it is easy to get backwards from memory: a 26.2 sign *is* a real
block model.** `blockstates/oak_sign.json` maps all 16 `rotation` values to `block/oak_sign_rot_N`
models with genuine geometry, and `StandingSignRenderer`
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/blockentity/StandingSignRenderer.java`)
declares **no model whatsoever** — only text transformations. So there is deliberately no sign
*geometry* here; porting one would draw a second board inside the one the terrain mesher already
produces. Sign block entities are a **text pass**, and that pass is not built (see
[What is not built](#what-is-not-built)).

**The sign NBT reaches the client, confirmed on the wire, not merely by reading the decode path.**
`crates/lodestone-world/src/block_entity.rs`'s `BlockEntity.nbt` is generic — it is populated for
*any* block entity's `BLOCK_ENTITY_DATA` payload the same way regardless of type, which chest already
proved for its own (usually-empty) NBT. To settle whether that generality actually carries sign text
rather than assuming it, a throwaway probe (not part of the tree — a standalone binary depending on
`lodestone-core`/`-model`/`-world`/`-net`/`-v770` as libraries, connected to the creative oracle,
placed a real `oak_sign` with `front_text`/`back_text` over RCON) read the resulting record straight
out of a live `World`. It arrived as:

```text
type_id = 7
nbt = Compound([
    ("back_text", Compound([("has_glowing_text", Byte(0)), ("color", String("black")),
        ("messages", List { elements: [String(""); 4] })])),
    ("is_waxed", Byte(0)),
    ("front_text", Compound([("has_glowing_text", Byte(0)), ("color", String("red")),
        ("messages", List { elements: [String("\"LODESTONE PROBE\""), String("\"\""), ...] })])),
])
```

Exactly the shape `SignText.DIRECT_CODEC` implies (per-side `has_glowing_text`/`color`/`messages`,
plus a sibling `is_waxed`). **So signs are not blocked on wire decode** — the remaining work is a
small typed parse of this already-arriving `Nbt::Compound` (there is no `SignText` struct yet, only
the raw payload) and the render pass itself. The render pass is what stayed out of scope this
session: `gpu/nametag.rs`, the substrate the issue's spec points at for the text quads, was another
agent's uncommitted work throughout (see [`docs/README.md`](./README.md) or ask the session owner for
current status before touching it). Building the codec with no consumer to call it would itself be a
zero-pixel island, so it was left as a spec rather than dead code — the confirmed NBT shape above is
what the next attempt should decode against.

## How it works

Four layers, version-free until the last:

| layer | file | what it owns |
|---|---|---|
| geometry | `crates/lodestone-assets/src/block_entity_models.rs` | the ported `EntityModelDef`s |
| renderer | `crates/lodestone-render/src/block_entity.rs` | placement, lid pose, material→sheet, batching |
| GPU | `crates/lodestone-shell/src/gpu/block_entities.rs` | pipeline, meshes, texture bind groups |
| source | `crates/lodestone-shell/src/block_entities.rs` | world → `ChestSpawn`, and the lid clock |

### The consumer chain, end to end

Two links already existed and reached **nothing**. Both are marked; if you are extending this, they
are the shape of failure to expect.

```
level_chunk_with_light ─► BlockEntity::decode_list  ─► LoadedChunk.block_entities
block_update           ─► World::sync_block_entity  ─┤       ▲
section_blocks_update  ─► World::sync_block_entity  ─┤       │
block_entity_data      ─► World::set_block_entity   ─┘       │
                                                    was DEAD: zero shell call sites
                                                             │
                       shell/block_entities.rs::chest_spawns ┘
                        (chest_candidates ─► chest_spawn)
                                    │
BLOCK_EVENT ─► v770 adapter ─► ClientEvent::BlockEvent
                                    ▲ was DEAD: fell through net.rs `forward`'s
                                    │ terminal `_ =>` arm — decoded-but-stranded
                       net.rs NetUpdate::BlockEvent
                                    │
                       sim.rs poll_net ─► Sim::chest_lids  (ticked in Sim::step)
                                    │
                       Sim::block_entity_source()  ── installed every frame ──┐
                                                                              ▼
                              app.rs ─► RenderState::set_block_entity_source
                                                                              │
                    gpu.rs::prepare_block_entities ─► plan_block_entities ────┘
                                    │
                    gpu.rs render_inner, inside the block pass ─► draw_indexed
```

**`ingest::handles_event` needed no new arm.** Checked rather than assumed, because that switch is
this repo's island factory: `SharedState::apply` forwards only ECS-handled events, and block events
travel the shell's own `ClientEvent` stream, so the ECS routing switch is not on this path at all.
The same holds for #374 below: `sync_block_entity` is a `WorldSink` call inside the adapter, not an
event, so none of the three routers is involved.

### There are **four** creation routes, not two — issue [#374](https://github.com/matteopolak/lodestone/issues/374)

The first version of that diagram listed only `level_chunk_with_light` and `block_entity_data`. Both
links were accurate, and the pair read as exhaustive. It was not, and the gap was visible in play: a
**freshly placed chest was invisible** while still opening.

In vanilla, **writing a block state is what creates the block entity** — no packet involved
(`LevelChunk.java:341`, `blockEntity = ((EntityBlock)newBlock).newBlockEntity(pos, state)`), and
`block_entity_data` is only ever *data for an entity that already exists* (its handler
`ClientPacketListener.java:1476` calls `getBlockEntity(pos, type)` and **drops** the payload when
nothing is there). Our `block_update` / `section_blocks_update` arms wrote the state and stopped, so
a placed chest had a state, no record, and `chest_candidates`' `for be in &chunk.block_entities` loop
never saw it. Interaction kept working the whole time because it resolves from the block state, which
is exactly why the bug reads as "the renderer is broken" and is not.

The fix is `World::sync_block_entity(x, y, z, Option<block_entity_type>)` in `lodestone-world`, a port
of `LevelChunk.setBlockState`'s tail, called immediately after every state write:

| new state | existing record | outcome |
|---|---|---|
| owns type `T` | none | **create** with `T` and `Nbt::End` |
| owns type `T` | type `T` | **keep**, NBT included (`isValidBlockState`) |
| owns type `T` | type `U ≠ T` | **replace**, NBT cleared ("Found mismatched block entity") |
| owns nothing | any | **remove** |

**The removal row matters as much as the creation row.** Without it, breaking a chest leaves a stale
record and this pass keeps drawing a chest in empty air — the same defect pointing the other way.

`lodestone-world` cannot resolve a state id itself (it would need `lodestone-data`, and
`lodestone-data → lodestone-model → lodestone-world` makes that a cycle), so the **caller** passes
the type and the version-specific answer comes from `lodestone_data::block_entity_types` — a census
walked out of the real jar, because neither `blocks.json` nor `registries.json` carries the
state→type pairing. See [`lodestone-data-crate.md`](lodestone-data-crate.md).

### Make that **five**, and the fifth is not a packet — issue [#381](https://github.com/matteopolak/lodestone/issues/381)

#374 wired the rule into the two packet arms, which is every route a *server* can create a block
entity by. It left the route the **client** creates one by, and there the bug survived intact: a
chest you placed yourself was still a hole, now for exactly one server round trip, because
`Sim::use_item_live` sent `use_item_on` and wrote nothing locally. Nothing in #374's diagram was
wrong; the row simply was not there to be wrong, because the prediction did not exist yet.

`crates/lodestone-shell/src/sim.rs`'s `write_predicted_block` is the fifth row, and it is the same
`set_block` + `sync_block_entity` pair in the same order — the point being that it is not a second
implementation of the rule. The **removal** half is what corrects a placement the server refuses,
which needs no new mechanism at all: vanilla's server re-sends the block state at *both* candidate
positions after every `use_item_on`, whatever it decided
(`ServerGamePacketListenerImpl.java:1397-1398`). See
[`block-placement-prediction.md`](block-placement-prediction.md) for the whole pipeline, the
`default state` census that does not exist, and why "the lowest state id for this block" is a
waterlogged chest.

`block_entity_data` still creates on a miss, deliberately unlike vanilla: vanilla can afford to drop
because it has `pendingBlockEntities` to promote from later and we do not, and the two failure modes
are not symmetric. An orphan record whose block state is not a chest resolves to no material in
`chest_spawn` and draws **nothing**, so creating is inert; dropping would lose server data we cannot
ask for again.

### Geometry, and the one difference from entity models

The bake is shared with entities verbatim — `CubeDef` / `PartDef` / `bake_entity_parts`, and
`entity::push_part_quads`' winding rule (made `pub(crate)` rather than copied: a chest whose winding
disagreed with the mobs beside it has exactly the armour-layer failure mode that function's doc
describes). **Placement is the difference, and it is total:**

| | entity | block entity |
|---|---|---|
| model space | Y-**down** | Y-**up** |
| placement | `entity_model_matrix`: `translate(feet) · rotY(180°−yaw) · scale(−s,−s,s) · translate(0,−1.501,0)` | `block_entity_placement_matrix`: `translate(pos) · rotateAround(−yaw, ½,0,½)` |
| anchor | the entity's feet | the block's corner |

`ChestRenderer.submit`'s *entire* prologue is one
`Matrix4f().rotationAround(Axis.YP.rotationDegrees(-facing.toYRot()), 0.5F, 0.0F, 0.5F)` — no flip
and no lift, because the chest's texels are already block-space: `bottom` spans y `0..10` texels and
the `lid` pivot at y `9` puts the closed lid's top at `14/16`, the real chest height. Feeding a chest
through the entity matrix buries it 1.5 blocks down, upside down.

`det(placement) == +1` for every facing (translation ∘ rotation, no handedness flip), which is *why*
the winding rule transfers unchanged. It is **measured**, not asserted from "rotations are positive"
— see `placement_preserves_orientation`.

### The lid

Vanilla applies **three** transforms, in three different classes. Collapsing any pair of them is
right at the endpoints and wrong everywhere between.

1. `ChestLidController.tickLid()` — ramps `openness` by **±0.1 per tick**, clamped `0..=1`, so a lid
   takes exactly 10 ticks. Ported in `shell/block_entities.rs::ChestLids::tick`.
2. `ChestLidController.getOpenness(a)` — `lerp(a, oOpenness, openness)`, the partial-tick
   interpolation. `ChestLids::openness`.
3. `ChestRenderer.submit` eases the progress (`open = 1-open; open = 1-open³`, a cubic ease-out),
   then `ChestModel.setupAnim` turns it into an angle (`lid.xRot = -(open·π/2)`, and
   `lock.xRot = lid.xRot`). `chest_lid_openness` and `chest_lid_x_rot` — deliberately two functions.

`lid` and `lock` are **siblings** sharing pivot `offset(0, 9, 1)`, not parent and child; nesting the
lock composes the pivot twice and puts it 9 texels too high.

### Sheets

Keyed by **texture stem**, not model name. A trapped chest shares the single-chest *mesh* and differs
only in bind group, so the batch key is `(model, texture)`; keying textures by model — as
`EntityRenderer::textures` correctly does, because a mob's sheet *is* determined by its model — draws
every trapped chest in plain oak.

22 stems: 7 materials × 3 halves, plus one half-independent ender sheet. `chest_texture_stems()`
derives that list from the *same match* the renderer resolves through, so a material cannot be added
without its sheets.

**Ender is half-independent on purpose.** `Sheets.chooseSprite` returns the single
`ENDER_CHEST_LOCATION` for every `ChestType`, and the jar ships only `entity/chest/ender.png` — no
`ender_left`/`ender_right` exist. A uniform suffix rule names a missing file and the chest falls back
to nothing, which reads as a broken renderer rather than a missing texture.

26.2 stitches these into `textures/atlas/chest.png` and submits a `SpriteId`. We bind the individual
PNGs instead: each sprite **is** the whole 64×64 sheet, so the model's own UVs (normalised against
64×64 by the bake) address a direct upload identically, and the atlas would only add a UV remap.

## Skull (skeleton, wither skeleton, zombie, creeper, player)

Like chest, `assets/minecraft/models/block/skull.json` is `{"textures":{"particle":"..."}}` — zero
elements, a hole in the world. Every visible triangle comes from `SkullBlockRenderer`/`SkullModel`.

**Geometry is shared and trivial: one 8×8×8 box.** `SkullModel.createHeadModel()` is a single `"head"`
part at `PartPose.ZERO`. What differs across vanilla's seven types is the *canvas*: skeleton, wither
skeleton and creeper skins are 64×32 (`createMobHeadLayer`), zombie and player are 64×64
(`createHumanoidHeadLayer`, base head only — the `"hat"` overlay child is not ported; see
`lodestone_assets::block_entity_models::skull_humanoid_model`'s doc for why). Baking the same box
twice, once per canvas, is the whole model corpus — `skull_mob_model()`/`skull_humanoid_model()` in
`crates/lodestone-assets/src/block_entity_models.rs`. **Two of vanilla's seven skull types are not
ported: dragon and piglin.** Both use their own multi-part rigs (`DragonHeadModel`/`PiglinHeadModel`)
unrelated to the shared box, and both are late-game/rare finds — lower value than the five common
ones for the "player survives an hour" bar this tier targets.
`lodestone_render::SkullType::from_block_path` declines them explicitly (draws nothing) rather than
drawing a wrong shape.

**Placement is the one surprise, and it inverts the chest lesson.** Chest's whole module doc leads
with "block entities are not Y-flipped, unlike entities" — true for chest, **false for skull**.
`SkullBlockRenderer.createGroundTransformation`/`createWallTransformation` both end in
`scale(-1, -1, 1)`, the same flip `entity_model_matrix` uses, because `SkullModel`'s head box is
authored in the ordinary mob Y-down convention (vanilla reuses a mob's own head geometry for the
block-entity case) and was never re-authored block-space-up the way `ChestModel` was.
`skull_ground_placement_matrix`/`skull_wall_placement_matrix` in
`crates/lodestone-render/src/block_entity.rs` port both transforms exactly, including the ground
case's `RotationSegment` (16 steps of 22.5°, segment 0 = **north** — not
[`horizontal_facing_yaw`]'s four-value south-is-0 convention, a different angle system in the same
file) and the wall case's outward `0.25`-block offset from `Direction.getStepX()/getStepZ()`.
`skull_placement_preserves_orientation` measures `det == +1` for both (same sign as the entity path,
not a mirror), and `wall_offset_moves_toward_the_named_direction` pins the offset against a
hand-verified `Direction` table rather than the function under test.

Player skulls always draw the **default Steve skin** (`entity/player/wide/steve`,
`DefaultPlayerSkin.getDefaultTexture()`'s own choice). A real profile skin needs a session-server
lookup and a network fetch — out of scope here, tracked as a gap rather than silently wrong (every
player skull looks the same rather than looking like a *specific* wrong player).

Texture stems are the **mob skins already on disk for entity rendering** — `skull_texture_stem`
just names five of them (`entity/{skeleton/skeleton, skeleton/wither_skeleton, zombie/zombie,
creeper/creeper, player/wide/steve}`), no new asset family. `block_entity_texture_stems()` (new,
`crates/lodestone-render/src/block_entity.rs`) is the union of `chest_texture_stems()` and
`skull_texture_stems()` — the shell's `resources::load_block_entity_textures` and this pass's own GPU
loader (`gpu/block_entities.rs`) both need to iterate the *combined* list, not just chest's, or a
skull draws every frame with no bind group.

### Skull's status: wired, and proven with real pixels

Everything CPU-side is in place and tested against the real 26.2 state table: `SkullType` resolution,
both placements, `BlockEntityModelSet::resolve_skull`, and the shell's `skull_spawn`/`skull_spawns`
gather (`crates/lodestone-shell/src/block_entities.rs`, reusing `chest_candidates` — already generic
over block-entity type, so a second scan was never needed). `plan_block_entities`/`BlockEntityInstance`
needed **zero changes**: `chests_and_skulls_batch_independently_in_one_frame` proves a chest and a
skull batch correctly in the same frame through the existing generic path.

**The `gpu.rs` wiring landed too** — `RenderState` now carries a `SkullSource` alongside
`BlockEntitySource`, and `prepare_block_entities` resolves and batches both families in the same
`instances` vec before `plan_block_entities`. `gpu.rs`/`gpu/*.rs` were another agent's live work for
most of this session, so the five-file patch (`gpu/sources.rs`, `gpu/block_entities.rs`, `gpu.rs`,
`sim.rs`, `app.rs`) was handed to the session orchestrator rather than applied here directly — and,
before that landed, hand-verified end to end in an isolated `git worktree add --detach` (touching
nothing in the shared checkout) with a real GPU adapter, so "this will work once wired" was a checked
claim rather than a hope by the time it was handed over.

`crates/lodestone-shell/tests/skull_block_entity_pixels.rs` is the resulting pixel gate — same shape
as `chest_block_entity_pixels.rs`: coverage measured *inside the skull's own projected screen rect*
(from the real baked vertices through the real `part_transforms`), failure output prints a bounding
box rather than a percentage, and a dedicated test locates the unconditional first-person arm and
asserts it is disjoint from the skull's rect before trusting the sibling gates' clean-control premise.
Measured green, both in the isolated worktree and against the real wiring once it reached the shared
checkout:

| gate | measurement |
|---|---|
| skull draws | rect `x136..184 y96..144` (2304 px); fill **88.1%**; changed bbox `x137..182 y97..142`, entirely inside |
| wall vs floor placement | floor rect `x136..184 y96..144`, wall rect `x134..186 y68..120` — distinct, and the frames differ by 3762 px |
| arm is elsewhere | arm bbox `x247..319 y169..239`, disjoint from the skull rect |

```bash
cargo test -p lodestone-shell --test skull_block_entity_pixels -- --ignored --nocapture
```

## Sign (standing/wall text)

Unlike chest and skull, a sign's board **is** a real block model —
`assets/minecraft/blockstates/oak_sign.json` maps all 16 `rotation` values to genuine
`block/oak_sign_rot_N` geometry, and `StandingSignRenderer` declares no model of its own, only text
transformations (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/blockentity/
StandingSignRenderer.java`). So there is nothing in `block_entity_models.rs` for a sign and nothing
in `plan_block_entities`'s batch: this type sits **beside** the chest/skull family
(`lodestone_render::sign`, not `lodestone_render::block_entity`), not inside it. The only thing this
session built is the text.

### The typed parse — `lodestone_world::sign_text`

`crates/lodestone-world/src/sign_text.rs`. Parses `front_text`/`back_text`/`is_waxed` out of the
`BlockEntity.nbt` compound `chest`/skull already proved generic (the shape this doc's earlier probe
captured, quoted above) into `SignText { front: SignSide, back: SignSide, waxed: bool }`, where
`SignSide` is `{ lines: [String; 4], glowing: bool, color: SignDyeColor }`.

**The one real surprise: `messages` elements are JSON text, not the NBT-structural component shape**
`lodestone_core::plain_text_from_nbt_component` already handles (the one every other resolved text in
this codebase — chat, player-list names, entity metadata — uses). A plain `"LODESTONE PROBE"` line
arrives as the *18-character NBT string* `"LODESTONE PROBE"` — opening and closing `"` are literal
payload bytes, not `Debug` escaping, i.e. the JSON serialization of the component stored verbatim
inside an `Nbt::String` rather than unwrapped into one. `sign_text.rs`'s `resolve_message` parses that
JSON (via `serde_json`, promoted from a dev-dependency to a real one in `lodestone-world`'s
`Cargo.toml` for this) and walks it with the same `text`/`extra` recursion
`plain_text_from_nbt_component` uses, just over a `serde_json::Value` instead of an `Nbt`. This was
re-confirmed live rather than trusted from the original probe alone: placing a fresh `oak_sign` on the
creative oracle and reading it back with `/data get block` returned
`front_text: {has_glowing_text: 1b, color: "blue", messages: ['"LODESTONE LIVE TEST"', '""', '""', '""']}`
— the same quoted-JSON-inside-a-string shape, on a second, independent capture.

`filtered_messages` (the server's profanity-filter shadow copy) is not parsed — this port has no
client-side text-filtering setting, and vanilla's own default is off, so `SignSide::lines` always
reads the unfiltered `messages`.

Where this parse lives is a compromise, not a discovery: `BlockEntity`'s own module doc says the NBT
*schema* belongs in a version crate, but `crates/protocol/v770/src/server_protocol.rs` was another
agent's in-flight file for this whole session (`CLAUDE.md`'s file-ownership notes), so the parse went
into `lodestone-world`, which this task was granted outright, with that tension recorded in the
module's own doc for whoever moves it later.

### Placement — `lodestone_render::sign`, ported term for term

`crates/lodestone-render/src/sign.rs`, not `block_entity.rs` — see that file's module doc for why a
sign shares no primitive with the chest/skull family (no `BlockEntityMesh`, no bake, no batch).
`sign_text_transform(pos, orientation, is_front)` ports `StandingSignRenderer.textTransformation`
line for line:

```text
Matrix4f result = new Matrix4f()
    .translate(0.5F, 0.5F, 0.5F)
    .rotate(Axis.YP.rotationDegrees(-angle));
if (attachmentType == WALL) result.translate(0.0F, -0.3125F, -0.4375F);
if (!isFrontText) result.rotate(Axis.YP.rotationDegrees(180.0F));
result.translate(TEXT_OFFSET);              // (0, 0.33333334, 0.046666667)
result.scale(0.010416667F, -0.010416667F, 0.010416667F);
```

Fed a local point in font-pixel space (`x` right, `y` down from the text block's own top, `z = 0`),
the result is that point's world position — the `-Y` scale is the *entire* y-flip, with no separate
step, because font-pixel space is already row-down (the same convention
`gpu/nametag.rs::layout_ink_runs` returns) and folding the flip into the matrix is what
`textTransformation` itself does.

**Front and back text are not mirror images through the same origin — they sit on the two opposite
faces of the board's thin depth**, because the 180° back rotation happens *before* `TEXT_OFFSET` is
applied, so the offset's own `z` component flips sign along with it: front origin
`z = 0.5 + 0.046666667`, back origin `z = 0.5 - 0.046666667` (ground sign, angle 0; both hand-computed
and unit-tested in `sign.rs`). The first draft of that test assumed the origins would coincide and
watched it fail — a useful reminder that "the back is the front rotated 180°" is true of the rotation
and false of where the two planes land.

`RotationSegment.convertToDegrees(segment) == segment * 22.5` is the identical formula
`skull_ground_placement_matrix` already uses for the same block-state property (segment `0` is
**north**, not `horizontal_facing_yaw`'s south-is-zero convention) — reused as the same expression,
not re-derived, so the two cannot silently drift apart.

Colour: `AbstractSignRenderer.getDarkColor`/`DyeColor.getTextColor()`/`ARGB.scaleRGB`, transcribed
into `dye_text_color_rgb` (all sixteen `DyeColor` text-colour constants, from the real jar source) and
`sign_side_color` (full colour when glowing, `ARGB.scaleRGB(dye, 0.4)` — **integer-truncated**, not a
float multiply carried through — otherwise). Per `CLAUDE.md`'s rendering constraints this multiply is
gamma-space, matching every other tint/shade in this codebase.

**Deferred, and named rather than silently missing:**

- The black-dye-glowing outline (`BLACK_TEXT_OUTLINE_COLOR = -988212`, a second offset glyph pass so
  glowing black text is not literally invisible). One narrow dye combination.
- Per-glyph world-light modulation for non-glowing text (vanilla's `state.lightCoords`). This pass
  draws unlit vertex colours unconditionally, the same simplification `gpu/nametag.rs` already
  documents making for its own text (full-bright regardless of the entity's own dimness) — sign text
  reads a little brighter than vanilla in the dark, never darker or absent.
- Line wrapping past `MAX_TEXT_LINE_WIDTH` (90 px). The four stored lines are trusted to already fit,
  which the vanilla sign-edit screen enforces at typing time; only a modded server or a hand-edited
  NBT payload could send an over-width line.
- Rich per-run formatting (colour/bold/italic/click events) inside one line. `resolve_message`
  extracts plain text only; the whole line draws in the side's own dye colour.
**No longer deferred: hanging signs — see [Hanging signs](#hanging-signs-the-same-renderer-four-numbers-apart)
below.** The bullet that used to sit here said they needed "a different model set again (chains, a
bar, its own text transform)" and cost the next reader a wrong estimate: that is **1.20's** shape.
In 26.2 there is no rig to port.

### Hanging signs: the same renderer, four numbers apart

**26.2's `HangingSignRenderer` declares no model.** It is `AbstractSignRenderer` plus one
`textTransformation` — read
`.cache/mc/26.2/client-src/net/minecraft/client/renderer/blockentity/HangingSignRenderer.java`
straight through; it is 75 lines and there is no `HangingSignModel` anywhere in `client-src`. The
board, its bar and its chains are all **block-model** geometry the terrain mesher already draws:
`assets/minecraft/blockstates/oak_hanging_sign.json` maps `attached`×`rotation` onto
`block/oak_hanging_sign[_attached]_rot_N`, which parent `block/template_hanging_sign_rot_N`, and
`oak_wall_hanging_sign.json` onto `block/template_wall_hanging_sign`. So hanging signs are the same
text-only case standing signs are, and the whole difference is `lodestone_render::SignKind`:

| | plain | hanging |
|---|---|---|
| base translate `y` | `0.5` | `0.9375` |
| pre-offset | `(0, -0.3125, -0.4375)`, **wall only** | `(0, -0.3125, 0)`, **always** |
| `TEXT_OFFSET` | `(0, 0.33333334, 0.046666667)` | `(0, -0.32, 0.073)` |
| render scale | `0.010416667` (`RENDER_SCALE 0.6666667 / 64`) | `0.0140625` (`TEXT_RENDER_SCALE 0.9 / 64`) |
| `getTextLineHeight()` | `10` | `9` |
| `getMaxTextLineWidth()` | `90` | `60` |

Three of those are worth stating out loud because each reads as a mistake:

- **The last two live on the block entity, not the renderer.** `SignBlockEntity` returns `10`/`90` and
  `HangingSignBlockEntity` *overrides* both. `TEXT_LINE_HEIGHT`'s doc in `sign.rs` used to say the
  value was "always `10` — the per-instance method never varies in the real jar", which was simply
  false; prefer `SignKind::text_line_height()`, which cannot be reached for the wrong kind.
- **A hanging sign's glyphs are bigger and its lines narrower.** Scale up (`0.9` vs `0.667`), width
  down (`60` vs `90`). Both directions are real. The live gate corroborates it independently: the same
  string draws **60 px** wide on a hanging sign against **44 px** on a plain one, and `60/44 = 1.36`
  is `0.0140625 / 0.010416667` — a magnitude the gate never asserts directly but which falls out of
  it.
- **The hanging pre-offset has no attachment branch.** `HangingSignRenderer` computes
  `state.attachmentType` (for the crumbling overlay) and then ignores it in `textTransformation` —
  wall and ceiling hanging signs differ *only* in where the angle comes from
  (`WallHangingSignBlock.FACING.toYRot()` versus `RotationSegment.convertToDegrees(ROTATION)`), which
  is already resolved into `SignOrientation` before the matrix runs. A "wall means add the wall
  offset" generalisation from the plain sign is wrong, and
  `a_wall_hanging_sign_differs_from_a_ceiling_one_only_by_its_angle` holds it down with the plain wall
  sign as its control.

Nothing else changed. `sign_candidates`, `sign_spawns`, the gather's NBT parse, the pass, the shader
and the pipeline are all shared — hanging signs needed **no** new source install in `sim/`, because
they arrive through the sign source that was already there. That is the whole reason this was cheap.

**The pixel gate is cross-arm, not a count.** `a_hanging_signs_text_draws_in_its_own_area_and_not_the_plain_ones`
projects both kinds' text planes through the real transform, asserts the two screen bands are
**disjoint in `y`** (the premise), then requires each kind's *changed* pixels to land in its own band
and not the other's. Two things made it worth building rather than trusting a vertex count:

- **A `SignKind` that reached the spawn but not the transform passes every count-based check** — both
  kinds produce ink at the same block. Neutering `gpu/sign_text.rs` to pass `SignKind::Plain` to the
  transform (leaving everything else live) was run and reddens this gate with exactly that diagnosis;
  neutering the *transform's* three branches instead reddens the premise check, which is the correct
  behaviour for a rect derived from the same expression the draw uses.
- **The first-person arm paints unconditionally, low on screen, which is where a hanging sign's text
  lands** (measured: arm bbox `x0:247 y0:169`, hanging band `y0:147 y1:189`). So the gate is
  differential against a sign-free frame rather than asserting "nothing else paints in this band" —
  an absolute version would have been measuring the arm, the premise-false control failure
  `CLAUDE.md` records.

### The GPU pass — `gpu/sign_text.rs`, beside `gpu/nametag.rs`, not inside it

`gpu/nametag.rs` was extended rather than sat beside for its *font loading and ink-run layout*
(`layout_ink_runs`/`load_font`, now `pub(super)` so `gpu/sign_text.rs` can call them directly instead
of duplicating a third jar-discovery snippet — that module's own doc already explains why *it*
duplicates `hud/vanilla_font.rs`'s, and the reasoning does not extend to a sibling file this task
owns outright). The **pass itself is a new, separate module**, not an extension of
`NameTagRenderer`, for two reasons that are both real, not stylistic:

- **Not a billboard.** A nametag's whole vertex generation is built around a per-frame
  camera-facing `right`/`up` basis; sign text has a *fixed* world orientation baked into
  `sign_text_transform` and needs no camera basis at all beyond the shared `view_proj` uniform.
  Folding it into `NameTagRenderer::prepare` would mean threading a "is this billboarded" branch
  through code that currently has no such branch anywhere.
- **Different depth pipeline.** Nametags are `LessEqual`/no-bias (normal pass) or
  `Always`/no-write (see-through pass) — see that module's doc. Sign text ports vanilla's
  `TEXT_POLYGON_OFFSET` instead: `LessEqual`, `depth_write_enabled: true`, and a polygon-offset bias
  (`constant: -10, slope_scale: -1.0`) so it wins the depth test against the coplanar board without
  z-fighting — the exact same two numeric constants (`1.0`, `10.0`) `crack_pipeline.rs` already ported
  from a *different* vanilla pipeline (`writeDepth: false` there, `true` here, which is the whole
  difference: sign text must occlude itself line over line; a decal overlay must occlude nothing).

Reuses the identical `nametag.wgsl` shader (`view_proj` uniform, flat vertex colour, no texture) —
a second `.wgsl` file would only be a byte-for-byte copy with nothing to diverge.

### The gather — `crate::block_entities::sign_spawns`

Mirrors `chest_spawns`/`skull_spawns`'s shape (`sign_candidates` → `sign_spawn` → `sign_spawns`,
sorted by position), with one structural difference: **`sign_candidates` cannot reuse
`chest_candidates`**, because chest and skull only ever need the block state at a candidate position
and `chest_candidates` deliberately discards `BlockEntity.nbt`; sign text needs the NBT, so
`sign_candidates` parses it into a typed `SignText` right there in the gather rather than threading a
raw `Nbt` value any further than it has to. `sign_kind_for_path`/`sign_kind_for_state` resolve the
block's registry path into a `SignKind` (every sign ends `_sign`, checked for `hanging` **first**
because both families share that suffix — get the order wrong and every hanging sign's text draws at
a plain sign's height and scale, which is why the test asserts the *kind* and not merely
`is_some()`); `sign_orientation` reads `rotation` (ground/ceiling) or `facing` (either wall variant)
off the block state, the identical shape `skull_orientation` already uses for the same two property
names, and it is shared unchanged by both kinds.

### The whole chain, hop by hop

```text
level_chunk_with_light / block_update / etc. ─► BlockEntity::decode_list / World::sync_block_entity
                                                  (unchanged — the chest/skull chain already proved this)
                                                                 │
                          lodestone_world::sign_text::SignText::parse(&be.nbt)
                                       (crate::block_entities::sign_candidates)
                                                                 │
                          crate::block_entities::sign_spawn / sign_spawns
                                                                 │
                    gpu.rs::render_inner ─► self.sign_source.signs(eye)
                                                                 │
                    gpu/sign_text.rs::SignTextRenderer::prepare ─► push_side_quads
                             (lodestone_render::sign_text_transform, sign_side_color)
                                                                 │
                    gpu.rs::render_inner, inside the block pass ─► SignTextRenderer::draw
```

`ingest::handles_event` needs no arm, for the same reason chest/skull needed none: sign text has no
per-tick, event-driven state (no lid, no animation) and travels the shell's own gather-per-frame path
entirely — there is nothing on the wire for a router to forward.

### Status: wired, and proven with real pixels

`crates/lodestone-shell/tests/sign_text_pixels.rs`, three `#[ignore]`d GPU gates, same shape as
`skull_block_entity_pixels.rs` but adapted for text: a rect **bounds** the whole area one text side's
local plane can occupy (`MAX_TEXT_LINE_WIDTH` wide, four lines tall, projected through the real
`sign_text_transform` and `Camera::view_projection` — never a remembered literal) rather than being
filled to a high percentage the way a solid skull box is, because glyph ink is sparse over its own
bounding plane. Measured green:

| gate | measurement |
|---|---|
| sign draws | expected rect `x121..199 y108..143`; changed bbox `x138..182 y108..113`, entirely inside; 97 non-sky px in-rect |
| front vs back | changed bbox between a front-only and a back-only frame: `x136..182 y108..113`, 135 px — distinct |
| arm is elsewhere | arm bbox `x247..319 y169..239`, disjoint from the sign's rect |

```bash
cargo test -p lodestone-shell --test sign_text_pixels -- --ignored --nocapture
```

**The negative control was watched failing**, the same discipline the chest/skull gates record:
commenting out `self.sign_text.draw(&mut pass, sign_text_count)` in `gpu.rs` and re-running produced
exactly the island shape —

```text
installing a sign source changed no pixel at all — the pass is dead
front-only and back-only text produced pixel-identical frames
```

— while `the_first_person_arm_is_somewhere_else` (which does not depend on the sign drawing at all)
kept passing, confirming the failure was specific to the neutered line and not a broken harness. The
line was restored and the suite re-run green before anything was committed.

## Bell

Bell is the **partial**-hole case, unlike chest/skull's total one. `assets/minecraft/models/block/bell.json`
has real geometry for the attachment frame (the post/mount), so before this landed a bell was not
invisible — it just had nothing hanging in it, which is easy to mistake for "looks about right" in a
quick screenshot and much easier to miss than a chest-shaped hole. Every visible triangle of the
swinging body and its flared rim comes from `BellRenderer`/`BellModel`
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/blockentity/BellRenderer.java`,
`.../client/model/object/bell/BellModel.java`).

### Geometry — one box, one nested child, 32×32

`BellModel.createBodyLayer()`:

```text
bell_body  texOffs(0,  0)  box(-3, -6, -3,  6, 7, 6)  pose offset(8, 12, 8)
  bell_base  texOffs(0, 13)  box(4, 4, 4,  8, 2, 8)  pose offset(-8, -12, -8)  (child of bell_body)
```

`bell_base` (the flared bottom rim) is nested **inside** `bell_body` in the real jar, not a sibling
under root — its local pose exactly cancels `bell_body`'s own pivot, so the rim's world pivot lands
at the block's own corner and the box itself sits directly below the tapered body, touching at the
seam. `lodestone_assets::block_entity_models::bell_model`'s own test
(`bell_base_sits_just_below_bell_body`) measures this through the real transform chain rather than
restating the texel arithmetic, and a second test
(`bell_base_is_nested_inside_bell_body_not_a_sibling`) pins the nesting itself — getting that backwards
(making them siblings) would double-apply `bell_body`'s pivot and put the rim at the wrong height
while every other check on the box's own dimensions stayed green.

Authored **block-space-up**, the same convention chest uses and unlike skull:
`BellRenderer.submit` applies no `scale(-1, -1, 1)` flip, so `CubeDef` origins and `PartPose`s add
directly with no sign flip.

### Placement needs no facing at all — the one surprise here

Unlike chest (`rotationAround(-facing.toYRot(), …)`) and skull (a full ground/wall transform pair),
`BellRenderer.submit` applies **no rotation of its own** before submitting the model. Every
`FACING`/`ATTACHMENT` combination poses the body identically; only the block's own attachment-frame
*model* (drawn by the ordinary block mesher, already real geometry) differs per attachment.
`BlockEntityModelSet::resolve_bell` therefore calls the **existing** `block_entity_placement_matrix`
with a fixed `facing_yaw_deg` of `0.0` — reusing chest's placement function unchanged rather than
adding a bell-specific one, because vanilla itself has nothing bell-specific to port here.

### The shake — a real formula, ported and predicted, but not triggered

`BellModel.setupAnim`:

```text
baseRot = sin(ticks / PI) / (4 + ticks / 3)
NORTH: xRot = -baseRot   SOUTH: xRot = +baseRot
EAST:  zRot = -baseRot   WEST:  zRot = +baseRot
```

Ported as `lodestone_render::bell_shake_angle(direction: Option<BellShakeDirection>, ticks: f32)`.
`lodestone-render`'s own unit test picks `ticks = pi^2 / 2` specifically because it makes
`sin(ticks / pi) == 1` exactly, turning the remaining unknown into one division — a magnitude
prediction from constants outside the function under test, not a sign check, per `CLAUDE.md`'s
evidence standard. `bell_block_entity_pixels.rs`'s GPU gate reuses the same `ticks` value to prove
the angle actually reaches the rendered mesh (see below), which the render-crate unit test cannot see
on its own.

**The trigger is wired.** `crate::block_entities::BellShakes` is the `ChestLids`-shaped map this
module's own "How to change it" section anticipated: a `HashMap<[i32; 3], Shake>` fed by
`NetUpdate::BlockEvent` in `sim/net_apply.rs`, advanced once per client tick in `sim/step.rs`, and
read back by `bell_spawns` through `Sim::bell_source`. `BellShakes::shake` interpolates the tick
counter against the partial tick for the same reason `ChestLids::openness` does — the angle is a
`sin` of it, so a stepped counter reads as a stutter at 60 fps — which is also why `bell_source`
captures the partial tick and **must be re-installed every frame**, unlike skull/sign.

Three details worth keeping, each of which is a way to get it subtly wrong:

- **`b0 == 1` means a different thing for a bell than for a chest** (shake vs. lid), and the packet
  cannot tell them apart. Both trackers are offered every event and the **per-type gather** is what
  reads only its own positions back out, so a rung bell never opens a chest lid. Routing on `b0`
  alone, or picking one tracker at the arm, is what would break.
- **`b1` is `Direction.from3DDataValue`, not a count** — `0` down, `1` up, `2` north, `3` south, `4`
  west, `5` east. That order is the jar's, not alphabetical and not `BellShakeDirection`'s own
  declaration order; getting it wrong swings the bell along the wrong axis, which still looks like a
  working animation. `shake_direction_from_3d` drops UP/DOWN, which `BellModel.setupAnim` has no
  rotation for.
- **A shake runs exactly 50 ticks** (`BellBlockEntity.DURATION`) and the entry is then dropped, the
  same garbage collection `ChestLids` does and safe for the same reason: an absent entry and a bell
  at rest are both `None`.

### Status: fully wired, including the live install

Same generic path chest/skull/sign already proved: `BellSpawn` → `BlockEntityModelSet::resolve_bell`
→ `plan_block_entities` → the existing `EntityPipeline` draw, with **zero changes** needed to
`gpu/block_entities.rs`'s texture loader or the draw loop itself (both are already generic over
`BLOCK_ENTITY_MODELS`/`block_entity_texture_stems()` — adding the `"bell"` entry to each was
sufficient). `gpu.rs` gained a `BellSource` field, a `set_bell_source` setter and one more
`filter_map` in `prepare_block_entities`, exactly mirroring `SkullSource`'s own three call sites.

**The live install landed too.** `Sim::bell_source()` (`sim.rs`, next to `Self::sign_source`) mirrors
`Self::skull_source` exactly — `self.net.as_ref()?.shared_handle()` plus a closure over
`crate::block_entities::bell_spawns` — and `app.rs`'s per-frame block-entity install block gained one
more arm alongside chest/skull/sign: `if let Some(f) = self.sim.bell_source() { render.set_bell_source(f); }`.
That closes the one hop that was missing: the render pass, the GPU wiring and the CPU-side gather
were already proven; this is what actually feeds a live session's data to them every frame.

**What is proven, and what still is not.**
`sim::tests::bell_source_tracks_connection_state_and_is_safe_before_login` proves the accessor tracks
connection state (`None` with no net attached, `Some` once one is, matching skull/sign) and that its
closure is safe to call before login (empty `Vec`, not a panic on the unpopulated `ClientHandle`).
What no gate in this crate proves — for chest, skull, sign or bell alike, not a bell-specific gap —
is a real client drawing one through an actual live `ClientHandle`: that needs a full login handshake
plus a chunk carrying both a `minecraft:bell` block state and a recorded block-entity entry, and no
test double here builds one. `bell_block_entity_pixels.rs`'s two GPU gates still install a hand-built
closure directly on `RenderState`, the same way `chest_block_entity_pixels.rs` does, precisely because
that harness does not exist yet.

`crates/lodestone-shell/tests/bell_block_entity_pixels.rs` is the proof this pass reaches pixels
without that install — it calls `RenderState::set_bell_source` directly, the same way
`chest_block_entity_pixels.rs` calls `set_block_entity_source` without any `sim.rs` involvement.
Measured green:

| gate | measurement |
|---|---|
| bell draws | rect `x136..184 y96..148` (2703 px); fill **74.5%**; changed bbox `x137..182 y96..147`, entirely inside |
| shaking moves real pixels | resting-vs-shaking changed bbox `x137..190 y94..150` (1123 px), inside the rect padded 10px for the rotation's own reach |
| arm is elsewhere | arm bbox `x247..319 y169..239`, disjoint from the bell rect |

```bash
cargo test -p lodestone-shell --test bell_block_entity_pixels -- --ignored --nocapture
```

**The negative control was watched failing**: commenting out the `resolve_bell` `filter_map` in
`gpu.rs`'s `prepare_block_entities` and re-running produced exactly the island shape —

```text
assertion `left == right` failed: the source is installed and the bell is in front of the camera
  left: 0
 right: 1
a resting and a shaking bell produced pixel-identical frames — the shake angle is computed but
never reaches the mesh
```

— while `the_first_person_arm_is_somewhere_else` kept passing, confirming the failure was specific to
the neutered line. The line was restored and the suite re-run green (matching the measurements above
exactly) before anything was committed.

## Shulker box

The cheapest type to add after bell, and the reason is structural rather than a coincidence: a shulker
box's whole appearance is a function of its **block state**. There is no animation state to carry, so
it slots into `plan_block_entities`' existing `(model, texture)` batch key with nothing new.

### Geometry — two sibling boxes, `createBoxLayer` and not `createBodyLayer`

`ShulkerModel.createShellMesh()`
(`.cache/mc/26.2/client-src/net/minecraft/client/model/monster/shulker/ShulkerModel.java:27-36`), on a
64x64 canvas:

```text
lid   texOffs(0,  0)  addBox(-8, -16, -8,  16, 12, 16)  pose offset(0, 24, 0)
base  texOffs(0, 28)  addBox(-8,  -8, -8,  16,  8, 16)  pose offset(0, 24, 0)
```

`createBodyLayer` shares that mesh and adds a third `head` part — for the **mob**. Baking the body
layer for a block entity draws a shulker's face floating inside every box in the world, which reads as
a texture bug rather than a wrong layer.

The two parts are **siblings on the same pivot**, not parent and child. That is what makes the lid
animation a single-part override.

### Placement is its own matrix, not `block_entity_placement_matrix` with a yaw

`ShulkerBoxRenderer.createModelTransform` (`ShulkerBoxRenderer.java:110-121`):

```text
translation(0.5, 0.5, 0.5) . scale(0.9995) . rotate(facing.getRotation())
  . scale(1, -1, -1) . translate(0, -1, 0)
```

Three differences from the chest matrix, each visible:

| | chest | shulker |
|---|---|---|
| pivot | block floor `(0.5, 0, 0.5)` | block **centre** `(0.5, 0.5, 0.5)` |
| rotation | a Y yaw (four horizontals) | `Direction.getRotation()`, **all six** faces |
| flip/lift | none | `scale(1, -1, -1)` then `translate(0, -1, 0)` |

Reusing the chest matrix draws an upside-down box a half-block low for `facing=up`, which is the common
case. The `0.9995` shrink is vanilla's own z-fighting guard against a neighbouring full block — keep it.

`ShulkerFacing::rotation` ports `Direction.getRotation()` (`Direction.java:144-153`). JOML's
`rotationXYZ(x, y, z)` is **X then Y then Z intrinsic**, so for the four horizontals the Z term is
applied *last*: `Mat4::from_rotation_z(..) * Mat4::from_rotation_x(FRAC_PI_2)`. Composing them the other
way round rotates a wall-mounted box about the wrong axis and the result still looks like a box.

### Seventeen sheets, keyed by block id

`Sheets.getShulkerBoxSprite(color)` indexes `SHULKER_TEXTURE_LOCATION` by `DyeColor.getId()`
(`Sheets.java:48,89`), so `SHULKER_COLOURS` is in **dye ordinal order** — `white, orange, magenta,
light_blue, …`, *not* the alphabetical order the texture directory listing suggests. Reading it off the
listing shifts every dyed box one sprite along, which draws a plausible wrong colour rather than
nothing.

And the colour comes off the **block id**, not a property and not NBT: vanilla has seventeen shulker
box *blocks*. A `color` property lookup finds nothing on any of them and draws every box undyed, which
reads as a texture-loading failure rather than a resolver bug.

### The lid animation is ported and not triggered

`ShulkerBoxRenderer.ShulkerBoxModel.setupAnim` (`:135-138`) is
`lid.setPos(0, 24 - progress * 0.5 * 16, 0)` and `lid.yRot = 270 deg * progress`, ported as
`shulker_lid_pose`. `progress` is `ShulkerBoxBlockEntity.getProgress(partialTicks)`, driven by the same
`BLOCK_EVENT` path a chest lid uses — and nothing in this workspace folds a shulker box's event yet, so
`ShulkerSpawn::progress` is always `0.0`. A closed box is what a shulker box looks like whenever nobody
has it open, so this is the honest state. Closing it is the `ChestLids`-shaped job: a per-position
counter fed from `net.rs`'s `BlockEvent` arm.

### Status: fully wired, including the live install

All six steps of the checklist below are done, including the one bell needed a second pass for:
`app/redraw.rs` installs `Sim::shulker_source` every frame. **That call site is not optional** — a 26.2
shulker box declares no block model of its own, so an unset source leaves a hole in the world exactly
where every box is, the chest failure mode.

## Wall banners

`BannerModel.createBodyLayer(false)` and `BannerFlagModel.createFlagLayer(false)`. The pattern
compositing, the sway, the ordered translucent mask pass and the gather were all already live for
standing banners; this is the second mesh pair plus the fork that selects it.

### It is two meshes and one placement, not the reverse

The instinct is backwards. `BannerRenderer.createWallTransformation` is
`modelTransformation(direction.toYRot())` — the **same** function
`createGroundTransformation` calls, with a different angle — so `MODEL_TRANSLATION` `(0.5, 0, 0.5)`
and the `(2/3, -2/3, -2/3)` `MODEL_SCALE` are shared, and there is **no** extra push away from the
wall. `skull_wall_placement_matrix`'s `0.25` offset has no counterpart here; adding one "because wall
placements offset" floats the banner a quarter block off the face. The offset a wall banner needs is
baked into its own mesh's `z` origins instead.

What genuinely differs is the geometry, in two places:

- **No `pole`.** `createBodyLayer` adds it only under `if (standing)`. A wall banner drawn on the
  standing rig hangs a 42-texel post in mid-air off the block face, which is why the gather declined
  wall banners outright until this mesh existed.
- **Both the bar's `y` *and* `z` origins move** (`-20.5, 9.5` against `-44, -1`), and the flag's rest
  pose moves with them (`offset(0, -20.5, 10.5)` against `(0, -44, 0)`). The flag's **cube is
  byte-identical** between the two, so the pose is the only thing separating them — a copy that reused
  the standing pose buries a wall banner two blocks into the floor while every geometry assertion
  still passes.

It is a second mesh rather than one mesh with a static pose override for the reason
`banner_flag_model`'s doc gives: the flag's `x_rot` sway is *itself* an override, and stacking a
second one on the same part is how the two start fighting over one field.

### Two angle conventions that are not interchangeable

`BannerAttachment` is an enum carrying each form's own angle, the shape `SkullOrientation` already
uses, because the two blocks have **different properties**: a standing banner has `rotation`
(`RotationSegment`, `0..16`, `22.5°` a step) and a wall banner has `facing` (four horizontals, `90°` a
step). Neither has the other's, so `banner_attachment` forks on which block it is rather than trying
both. A shared `angle: f32` field would let a caller hand a wall banner a segment and get a plausible
eighth-turn error.

### The suffix-order trap

`banner_colour` must try `_wall_banner` **before** `_banner`. `"red_wall_banner"` ends in `_banner`
too, so the other order strips it to `"red_wall"` — not a dye name — and **every wall banner in the
world silently draws nothing**. One read returns both the dye and which form it is, so the colour and
the attachment cannot disagree. The gate drives all sixteen dyes through both families (256 standing
states, 64 wall states) with the wrong-order parse asserted to fail in the same run.

## Lectern

The open book lying on a lectern — `LecternRenderer` plus `BookModel`. The cheapest type in this
module and the only one whose input is a single block-state boolean: no NBT, nothing on the wire, no
animation state, no per-frame clock.

### Geometry — seven parts, four of them paper-thin, on the module's only non-square sheet

`BookModel.createBodyLayer()`, sheet **64 × 32**
(`lodestone_assets::block_entity_models::book_model`). Three things in the transcription look like
mistakes and are not:

- **The two lids and the two flip pages are 0.005 texels thick.** They are boxes, not quads, so
  `bake_cube` emits six faces of each and two of those faces are 0.005 texels wide. A mesher that
  culled near-degenerate cubes would silently eat the covers and the turning pages and leave two page
  blocks, which still reads as a book.
- **`flip_page1` and `flip_page2` share one `CubeListBuilder`** in the jar, so identical UVs are
  deliberate. They differ only in the per-frame `yRot` `setupAnim` gives them.
- **`seam` is the only part with a rest *rotation*** — `PartPose.rotation(0, PI/2, 0)`, no offset —
  and `setupAnim` never poses it. The spine's quarter turn therefore has to survive as a rest pose;
  adding `seam` to the override list with a zero `y_rot` flattens it into the covers and still draws a
  plausible book.

`BOOK_SHEET` is named separately from `CHEST_SHEET` for the height: at 64×64 every `v` coordinate
halves and the page texture draws at the wrong scale rather than not at all.

### `openness` is a compile-time constant — do not port an animation for it

`LecternRenderer.BOOK_STATE` is `BookModel.State.forAnimation(0.0, 0.1, 0.9, 1.2)`, and `forAnimation`
computes `openness = (sin(progress * 0.02) * 0.1 + 1.25) * openness`. With `progress == 0` the `sin`
term is **exactly zero**, so the whole expression collapses to `1.25 * 1.2 == 1.5` for every lectern in
the world, on every frame. That is `LECTERN_BOOK_OPENNESS`.

The trap is that the expression *looks* like an animation. Feeding it a live tick counter would make
every lectern book breathe, which vanilla's does not — the page-flip animation belongs to
`EnchantTableRenderer`, which passes `forAnimation` a real client-simulated `progress`.
`a_lectern_books_openness_is_constant_because_its_progress_term_is_dead` recomputes the formula from
the jar's four literals and carries a non-zero-progress control, so it proves the constant is a
property of the lectern's own arguments rather than of an inert formula.

### The six poses, and the `x` term no other type here has

`book_part_poses(openness, page_flip)` returns `(part name, y_rot, x)`:

```text
left_lid.yRot    = PI + openness
right_lid.yRot   = -openness
left_pages.yRot  = openness
right_pages.yRot = -openness
flip_page1.yRot  = openness - openness * 2 * page_flip1
flip_page2.yRot  = openness - openness * 2 * page_flip2
left_pages.x = right_pages.x = flip_page1.x = flip_page2.x = sin(openness)
```

Six overrides — the widest list in this module, which is why
`BlockEntityMesh::part_transforms` takes a slice. Two things to hold on to:

- **`x` is absolute, not a delta.** `setupAnim` assigns `this.leftPages.x = Mth.sin(openness)`,
  overwriting the rest pose's `0`. Four of the six parts move their pivot as well as rotating; no
  other type in this module does.
- **The `* 2` in the flip-page terms is load-bearing.** With it, the two pages land at `+1.2` and
  `-1.2` — opposite sides of the spine, which is what makes a book look mid-turn. Drop it and they
  are `1.35` and `0.15`: both positive, same side, and a sign-only assertion still passes. The gate
  requires opposite signs.

Every posed part is a flat child of the root, so unlike the bell there is no parent/child composition
to get right.

### Placement — a `67.5°` tilt about Z, and a facing turned clockwise then negated

`LecternRenderer.submit`, in `lectern_book_placement_matrix`:

```text
translate(0.5, 1.0625, 0.5) · rotateY(-yRot) · rotateZ(67.5°) · translate(0, -0.125, 0)
```

**Not `block_entity_placement_matrix` with a yaw.** The lift happens *before* the rotation, so the
book pivots about itself rather than the block's floor corner; the `67.5°` tilt about **Z** is the
entire reason the book faces a reader instead of lying flat; and the final `-0.125` happens in the
tilted frame, so it does not commute with the first translation.

`yRot` is `FACING.getClockWise().toYRot()` — `horizontal_facing_clockwise_yaw`, **not**
`horizontal_facing_yaw` — and `submit` then rotates by its *negation*. Both steps are a quarter turn
each and easy to unwind wrongly; the plain facing yaw lays the book across the shelf at right angles
to the reader. A clockwise turn is `+90°` in `toYRot()` terms (north `180` → east `270`), so this is
one addition rather than a second four-arm match to keep in sync.

The tilt gate takes its expectation from the transform algebra rather than the implementation: `Ry`
preserves a vector's `y` component, so the angle between the book's own up axis and world up is
exactly `67.5°` at **every** facing. It computes the wrong hypothesis
(`block_entity_placement_matrix`, which gives `0°`) in the same run, and a second assertion requires
opposite facings to lean in opposite horizontal directions — which a placement missing the `Ry` term
entirely would fail while still passing the tilt check.

### `has_book` is the whole gather, and an unset source is not a hole

`crate::block_entities::lectern_spawn` declines twice, for different reasons: the state is not a
lectern (a stale record, same rule as every gather here), or it *is* a lectern with `has_book=false`
and there is genuinely nothing to draw. Only the book comes from this pass.

That makes the lectern the **mildest** degradation in the family. An unset `set_lectern_source` leaves
a complete but empty lectern rather than a hole in the world, so this one cannot be caught by looking
for a missing block — the symptom is a lectern that never holds a book. `app/redraw.rs` installs
`Sim::lectern_source` beside the other four.

### Status: fully wired, including the live install

All six steps of the checklist below are done. One model, one sheet, so every lectern in the world
coalesces into one batch regardless of facing — the facing rides the per-instance placement matrix.

## Campfire

The food cooking on a campfire — and **nothing else**. This is the type that breaks the pattern every
other section above shares, in three ways at once, so read this before assuming it is another
`resolve_*` arm.

### `CampfireRenderer` owns no mesh, no layer and no sheet

Its whole `submit` is a loop over four `ItemStackRenderState`s at four poses. There is no
`createBodyLayer`, no `bakeLayer` call, no `SpriteId` and no model field on the class. The fire, the
logs and the smoke a player sees are the **block model**, which the terrain mesher already draws — so
the intuition "campfire needs the fire texture, and vanilla is not colour-managed, so mind the gamma
space" is a plausible-sounding inference about the wrong subsystem. There is no fire texture on this
path.

Two consequences worth stating because each is a step this port does *not* have:

- no `campfire_model()` builder and no entry in `BLOCK_ENTITY_MODELS`;
- no texture stem, so `block_entity_texture_stems()` is unchanged and the expected-sheet-count
  assertion in the pixel gates stays where it was.

### It draws through the *model* pipeline, not `EntityPipeline`

A cooking item is an **item model** — the same baked quads a hotbar slot uses — so it belongs with
dropped items, thrown projectiles and items in mobs' hands in
[`dropped-items.md`](./dropped-items.md)'s pass (`gpu/world_items.rs`), not with the cuboid rigs here.
The gather
is `block_entities::campfire_spawns` (this module, beside the other five) but the consumer is
`RenderState::prepare_item_geometry`, and the placement is folded into the vertices rather than
carried as a per-instance matrix.

That is why `CampfireSource` must **not** join `prepare_block_entities`' emptiness condition the way
every source before it did: it has no `BlockEntityBatch` to contribute, and adding it there would make
the condition read as satisfied while nothing drew.

It is also why a campfire item is textured from the *block atlas* — a cooking potato and a potato lying
on the ground are the same pixels, which is the correct answer and comes for free from reusing this
pass.

### The pose, and where the display transform goes

`CampfireRenderer.submit`, term for term
(`lodestone_render::campfire_item_matrix`):

```text
T(pos) · T(0.5, 0.44921875, 0.5) · Ry(-slotYRot) · Rx(90°) · T(-0.3125, -0.3125, 0) · S(0.375)
```

then the item's own `display.fixed` on the **right**, because
`ItemStackRenderState.LayerRenderState.submit` calls `applyTransform` *after* the renderer's own
pushes. `campfire_item_mesh` is that composition; composing the display transform on the left instead
mirrors all four items into the wrong corners while still looking like four items on a campfire.

| term | value | why it is not the obvious number |
|---|---|---|
| lift | `0.44921875` (`115/256`) | **not** `0.4375` (`7/16`, the block model's own top face) — the extra `1/256` is what keeps a flat food sprite off the log it lies on |
| yaw | `-Direction.from2DDataValue((slot + facing.get2DDataValue()) % 4).toYRot()` | the slot is an **offset from the facing**, not a world corner |
| `Rx` | `90°` | what makes a sprite lie *on* the fire rather than stand up out of it |
| scale | `0.375` (`CampfireRenderer.SIZE`) | — |

`get2DDataValue()` is `toYRot()/90` — `Direction.toYRot()` is literally `(data2d & 3) * 90` — so
`horizontal_facing_yaw` covers both and there is no second table to keep in sync.

Predicted rather than measured-after-the-fact: with `facing = south` the four slot origins are
`(0.1875, 0.44921875, 0.1875)`, `(0.8125, ·, 0.1875)`, `(0.8125, ·, 0.8125)`, `(0.1875, ·, 0.8125)` —
the four corners, clockwise from above. `the_four_campfire_slots_land_in_four_distinct_corners` asserts
exactly those, because a "four items somewhere on the campfire" assertion accepts all four stacked in
one corner, which is what dropping the facing term produces. The neuter was watched: multiplying
`facing_2d` by zero reddens `the_facing_offsets_which_corner_each_slot_uses` with
`facing 90: slot 0 at Vec3(0.1875, 70.44922, 0.1875) but slot 1 of a south campfire is at
Vec3(0.8125, 70.44922, 0.1875)`.

### `Items` carries an explicit `Slot`, and the list index is not it

`ContainerHelper.saveAllItems` writes `ItemStackWithSlot.CODEC`:
`{Slot: <unsigned byte>, id: <item id>, count: <int>}`, and it **omits empty slots**. So a campfire
holding one steak in its third slot writes a *one*-element list with `Slot: 2`. Reading the list index
instead of the field agrees with the truth on a full campfire and cooks the food in the wrong corner on
a partial one — the bug hides behind the case you are most likely to build a fixture for.

`Slot` is an `Nbt::Byte`, not an int (`ExtraCodecs.UNSIGNED_BYTE`), and a missing `Slot` defaults to
`0` rather than dropping the item (`optionalAlwaysPresentFieldOf(.., "Slot", 0)`). `count` is not read:
a campfire slot holds one item and the renderer draws one copy regardless.

The client really does receive this — `CampfireBlockEntity.getUpdateTag` calls the same
`saveAllItems` — so no new packet work was needed. `soul_campfire` counts too: identical block entity,
identical registration, and the flame colour it differs by lives in the block model.

### No animation, and `CookingTimes` drives nothing

`CampfireRenderer` has no clock term at all. The flicker is the block model's animated texture, and
the NBT's `CookingTimes`/`CookingTotalTimes` are server book-keeping the client renderer never reads.
So `campfire_source` captures no partial tick — the only source in the family whose per-frame
re-install is purely `skull_source`'s disconnect-safety reason and nothing more.

### Status

Wired end to end: geometry (`campfire_item_matrix`, `campfire_item_mesh`), gather
(`campfire_spawns`), source (`CampfireSource` + `set_campfire_source`), consumer
(`merge_campfire_items` inside `prepare_item_geometry`) and the per-frame install in `app::redraw`.
`RenderStats::campfire_items_drawn` is the outside view, and it has its own field for a reason
specific to this type: a campfire contributes to **neither** `block_entities_drawn` nor
`item_drops_drawn`, so without it a broken gather is invisible in both.

Nothing is open within campfire scope.

## How to change it

### Adding a block-entity type

1. A `*_model()` builder plus a `BlockEntityModelEntry` in `BLOCK_ENTITY_MODELS`.
2. A texture-stem resolver in `render/block_entity.rs` and its entry in the preload list.
3. A `*Spawn` input struct and a `resolve_*` on `BlockEntityModelSet`.
4. A gather arm in `shell/block_entities.rs` and a prepare arm in `gpu.rs`.

You do **not** need a new pipeline. Everything draws through `EntityPipeline`, and the draw loop is
already generic over `(model, texture)`.

**First check whether the renderer has a mesh at all.** Steps 1–3 assume a cuboid rig, and
[Campfire](#campfire) is the counter-example: its renderer draws *item models*, so it has no builder,
no stem and no `resolve_*`, and its consumer is `prepare_item_geometry` rather than
`prepare_block_entities`. Read the vanilla class for a `bakeLayer` call before writing step 1 —
`PistonHeadRenderer` (which draws whole **block** models) is a second renderer of that shape.

### Gotchas, each of which has a test holding it

- **`visible_faces` is indexed by `entity::FACE_ORDER` `[Down, Up, West, North, East, South]`**, not
  by `Direction`'s discriminant. Off-by-index deletes the chest's *front* face instead of its seam,
  which still passes any "does a chest draw" gate. Held by
  `double_halves_omit_exactly_the_seam_face`, which asserts *by direction*, not by quad count.
- **Part names are the animation's only handle.** `BlockEntityMesh::index_of` resolves `"lid"` and
  `"lock"` by name; renaming either silently freezes the lid shut — the mesh still draws, so a
  coverage-only gate stays green. Held by
  `lid_and_lock_share_the_pivot_the_animation_rotates_about`.
- **South is yaw `0`** (`Direction.toYRot()`), not `Direction`'s declaration order
  (down/up/north/south/west/east), which is a quarter-turn error on every chest in the world.
  Held by `facing_rotates_the_front_of_the_chest_to_the_named_side`, which locates the *latch* after
  rotation — a `+yaw`/`−yaw` swap passes every bounds and determinant assertion.
- **`Affine` → `Mat4` is a transpose.** `Affine::m[i][j]` is row `i`, column `j`;
  `Mat4::from_cols_array_2d` takes **columns**. Feeding rows in as columns gives the inverse
  rotation, which for a lid looks like it opening *into* the chest and is easy to misread as a sign
  error in `chest_lid_x_rot`.
- **The entity lift is `+1.501` in world space, not `−1.501`.** The flip is applied *after* the
  translate, so the negative comes out positive. `placement_does_not_flip_or_lift` failed on its
  first run for exactly this; it compares against the real `entity_model_matrix` rather than
  restating its expression.
- **Do not use `entity_anim::Skeleton`.** It animates by *slot* (head, limb table) and classifies a
  chest as `AnimFamily::Static` — i.e. a permanently shut lid. Block entities take direct per-part
  pose overrides, composed through `lodestone_assets::entity::Affine::of_pose` so the `rotationZYX`
  order cannot drift from the bake's.
- **Do not nest the world read lock.** `chest_spawns` calls `loaded_chunks()` *before* taking the
  guard and drops the guard before sampling light. `std::sync::RwLock` gives no re-entrancy
  guarantee — a nested read may deadlock once a writer is queued, which on this world happens every
  time a chunk packet lands. That failure mode appears under load and never in a test.
- **No fifth bind group.** `wgpu`'s default `max_bind_groups` is 4 and the model shader spends all
  four. This pass reuses `EntityPipeline` (two groups) and adds a second bind group over the
  *existing* group-0 layout, the same trick the first-person hand pass uses. A fifth group compiles
  on an 8-group M5 and crashes at startup for everyone at the floor.
- **The source must be re-installed every frame.** It captures this frame's partial tick and a
  snapshot of the lid map. A one-shot install at connect draws every lid frozen at the fraction of a
  tick the session happened to join on.
- **Every block-state write must call `World::sync_block_entity`.** #374 was one write path that
  did not, and the whole rule is that there are no exceptions — "this one cannot need it" is the
  reasoning that produced the bug. The one live gap left is `Sim::set_block_world`, the **demo-world**
  editor, which still writes bare block states; it is harmless today only because the only values it
  is ever passed are `PLACE_BLOCK` (stone), air and water, none of which own a block entity. Closing
  it is a one-line addition and should happen the moment that world can contain a chest.

### There are two consumers of this geometry, not one

The world pass documented here is the first. The **GUI item-icon path is the second**, and it is
issue [#369](https://github.com/matteopolak/lodestone/issues/369) — **landed for chest** in
`d683a29`. It used to draw nothing, because `IconPart::Special` — where vanilla's
`ChestSpecialRenderer` goes — was an empty match arm in
`crates/lodestone-shell/src/hud/item_icon.rs`, and `block_models.rs`'s
`collect_item_model_parts` filters every non-`Model` part out at bake time so no chest ever
enters `BlockModels::items`. It now routes through `special_icon_geometry` into a third
`EntityPipeline` pass; see [`gui-item-icons.md`](gui-item-icons.md) for the consumer chain and
the per-`kind` table. The assessment below is what that work was built on and was confirmed
correct in the doing, so it is kept in the indicative rather than rewritten.

Vanilla shares one `ChestModel` between the two: `ChestSpecialRenderer.Unbaked.bake` calls
`context.entityModelSet().bakeLayer(ChestRenderer.LAYERS.select(chestType))` — literally the same
layer definition the block-entity renderer bakes — with `openness` defaulting to `0.0`. So the
geometry here **is** the right source for both, and a change to the models must be checked against
both call sites. Do not assume a chest that looks right in the world looks right in the hand.

**What is and is not reusable, assessed rather than hoped:**

- **Reusable as-is: the vertices, indices and part hierarchy.** Both passes consume
  `ModelVertex::vertex_layout()`, so there is no re-bake and no repacking.
  `BlockEntityMesh::part_transforms(placement, overrides)` already takes an **arbitrary** placement
  matrix and is public — that is the seam. `gui_item_pose(rect, display.gui)` slots in exactly where
  `block_entity_placement_matrix` goes in the world, and vanilla applies the identical
  `ItemTransform` + `-0.5` centring composition. An item chest is `openness = 0`, so there are no
  lid overrides and no animation to drive.
- **Not reusable: the texture binding.** The chest's UVs are `[0,1]` against the standalone 64×64
  `entity/chest/normal.png`; the GUI item pass binds the **stitched block atlas**, which contains
  nothing under `textures/entity/`, and it spends **all four** bind groups
  (camera+origin / atlas / palette / anim). Routing a chest through `ModelIcons` would sample
  arbitrary block texels.

So #369 is **not** a re-bake and **not** a UV remap. Stitching the 22 chest stems into the block
atlas and remapping the baked UVs would give the chest two texture paths, fight mip-4 atlas
mipmapping on a 64×64 entity sheet, and need either 22 mesh variants or a per-draw UV offset the
model shader has no slot for. The cheap route is the one the world path already proves: draw it with
`EntityPipeline`, which spends only **2** bind groups, consumes the same vertex layout, is
double-sided (so the GUI pose's negative determinant is a non-issue) and is depth-tested/writing,
matching the depth attachment `IconRenderer::draw_models` already clears — recorded inside that
existing pass. That is the route taken, and it came in at **three** files plus a gate (no
`lodestone-assets` change, no `app.rs` change, no shader change), because the placement seam and
the `draw_models` pass were both already there to be reused. The same shape covers shulker boxes
the day their model lands.

Two fidelity caveats that route carries, now observed rather than predicted: the entity shader
lights from a fixed direction with derivative-reconstructed normals rather than `gui_light`'s
per-face constants, so a GUI chest is shaded like the world chest rather than like a model item —
measured as top/bottom band means of `90.3`/`101.0` in the gate, i.e. the horizontal face is
*dimmer* than the sides here, the opposite of a `gui_light: Side` model item. And
`IconPart::Special`'s flat-sprite `base` fallback stays unused, which turned out to be true of the
**whole family and not just chest**: all ten special `base` models ship no `elements` and no
`layer0`, only a *block* `particle` texture, so there was never a flat sprite to fall back to. See
[`gui-item-icons.md`](gui-item-icons.md#known-gaps).

One thing the assessment did **not** predict, and worth carrying to the next block-entity type:
`IconRenderer::draw_models`' early return was `if count == 0` on the *model* stream's vertex
count. A slot holding only a chest makes that zero, so the new pass would have been attached, fed
and never run — the same island one layer down. When you add the second special `kind`, check the
guard before the geometry.

## Configuration

Nothing user-facing. The values that matter are all ported constants:

| constant | value | source |
|---|---|---|
| `block_entities::VIEW_DISTANCE` | `64.0` blocks | `BlockEntityRenderer.getViewDistance()`, compared against `Vec3.atCenterOf(pos)` — the block **centre**, not its corner |
| `LID_SPEED` | `0.1` / tick | `ChestLidController.tickLid()` |
| chest sheet size | 64×64 | all three `ChestModel` layers' `LayerDefinition.create(mesh, 64, 64)` |
| bell sheet size | 32×32 | `BellModel.createBodyLayer`'s `LayerDefinition.create(mesh, 32, 32)` |
| bell shake denominator | `4.0 + ticks / 3.0` | `BellModel.setupAnim` |
| expected sheet count (gates) | **derived**, not a literal | `block_entity_texture_stems().len()` — a hardcoded `22` (chest-only) already went stale once when skull landed; see `skull_block_entity_pixels.rs`'s `expected_sheets` doc |

Sheets load from `client.jar` via `resources::load_block_entity_textures`, fail-open: no pack means
chests **draw nothing** rather than a synthetic placeholder. That asymmetry with mob sheets (which do
get a placeholder) is deliberate — a flat-magenta mob reads as "this sheet is missing", but a
flat-magenta chest-shaped box reads as a renderer bug. `RenderStats::block_entity_sheets_loaded` is
what distinguishes the two from outside.

## Proof

`crates/lodestone-shell/tests/chest_block_entity_pixels.rs` — three `#[ignore]`d GPU gates:

```bash
cargo test -p lodestone-shell --test chest_block_entity_pixels -- --ignored --nocapture
```

The expected rect is projected from the **real baked vertices** of the real corpus mesh, through the
*same* `Camera::view_projection` the render call uses and the *same* `part_transforms` the draw uses
— never a remembered literal. Failure output prints a **bounding box**, not a percentage.

Measured green:

| gate | measurement |
|---|---|
| chest draws | rect `x137..183 y98..144`; fill **89.6%**; changed bbox `x138..181 y98..142`, entirely inside |
| lid animates | band above the closed silhouette: closed **0 px**, open **1504 of 1504** |
| arm is elsewhere | arm bbox `x247..319 y169..239`, disjoint from the chest rect |

**The negative control was watched failing.** The island was simulated exactly — planning left
intact so `block_entities_drawn` stayed at `1`, and the mesh upload dropped, which is the precise
shape of this repo's eleven confirmed instances:

```
the chest fills only 0.0% of its own projected rect (0 of 2209 px).
Subject's non-sky bbox: Rect { x0: 247, y0: 169, x1: 319, y1: 239 }
an open lid painted only 0 px in the 1504 px band ... Changed bbox: None
```

Note what the first failure printed: the only thing painting was the **first-person bare arm**. That
is the false control `CLAUDE.md` records, and it is caught by construction —
`the_first_person_arm_is_somewhere_else` *locates* the arm and asserts it is disjoint from the chest
rect, so the sibling gates' clean-control premise is a measurement rather than a hope.

Both other gates assert their own premise before the thing they measure: the lid gate fails loudly
if the open lid does not project above the closed chest (which would make its pixel assertion vacuous
rather than failing), and the draw gate fails if the chest projects to under 900 px.

Unit tests: 6 in `lodestone-assets`, 17 in `lodestone-render`, 10 in `lodestone-shell`. Note that all
33 are a **closed loop** with respect to the shell pass — none of them calls
`prepare_block_entities`, so every one would stay green with the draw deleted. Only the pixel gates
can see that.

### #374: the creation half

`chest_block_entity_pixels.rs` hands `RenderState` a synthetic `ChestSpawn`, so it is silent about
where spawns come from and stayed green throughout #374.
`crates/lodestone-shell/tests/placed_chest_block_entity_pixels.rs` starts one layer earlier — a real
`World` with a real loaded chunk, written through the `WorldSink` seam (`set_block` then
`sync_block_entity`, the exact pair the adapter's `BLOCK_UPDATE` arm calls), then the **real** shell
gather (`chest_candidates` + `chest_spawn`), then the real `RenderState::render`:

```bash
cargo test -p lodestone-shell --test placed_chest_block_entity_pixels -- --ignored --nocapture
```

| frame | world write | measured |
|---|---|---|
| subject | `set_block(chest)` + `sync_block_entity(Some(1))` | rect `x137..183 y98..144`, fill **89.6%** (1980/2209) |
| pre-fix control | `set_block(chest)` **only** | **0 px** in that rect; 0 spawns gathered |
| removed | then `set_block(air)` + `sync_block_entity(None)` | **0 px**; pixel-identical to the never-had-a-chest frame |

The middle row is #374 reproduced verbatim as a permanent control — a *world state* rather than a
deleted line of code, so it cannot rot. Its changed bbox is `x138..181 y98..142`, entirely inside the
rect, and the arm sits at `x247..319 y169..239`, re-measured disjoint.

**The negative control was watched failing**, twice and at two layers:

- the pixel gate with its subject switched to the pre-fix write —
  `assertion left == right failed  left: 0  right: 1` on `block_entities_drawn`, with
  `subject_spawns = []`; and
- the two `sync_block_entity` calls temporarily deleted from `adapter.rs`, which fails **three** of
  `crates/protocol/v770/tests/block_updates.rs`' world-backed gates on real `BLOCK_UPDATE` /
  `SECTION_BLOCKS_UPDATE` packet bytes:
  `a placed chest must gain a block-entity record from the state alone ... left: []`.

Those v770 gates are what join the pixel gate to the wire: they dispatch real packet bytes into a real
`World` and assert the resulting records, in both directions and for the bulk path (whose
`section << 4 | rel` reconstruction has its own negative-coordinate gate, because getting it wrong
puts the record 16 blocks away, where it still exists and still fails to draw). Each asserts its state
write landed **before** asserting anything about block entities — every seam here is a documented
no-op for an absent chunk, so a fixture that forgot to load one would read as a broken feature.

Note that `a_repeated_block_update_keeps_the_nbt_block_entity_data_delivered` passes with or without
the fix: it guards the `Kept` branch (a re-sent chest state must not wipe contents `block_entity_data`
delivered — the server re-sends `block_update` for a chest whenever a neighbour makes it a double), not
#374 itself.

### #381: the prediction half, and its refusal

Two further gates live in the same file, both driving `sim::write_predicted_block` — the production
write, not a re-spelling of its two calls, since a re-spelling would pass with the prediction deleted:

| gate | subject | control |
|---|---|---|
| `a_locally_predicted_chest_reaches_pixels_with_no_server_packet` | `write_predicted_block(chest)`, no packet decoded anywhere | **no local write at all** — #381 itself, as a world state |
| `a_refused_placement_loses_the_predicted_block_entity` | predict, then the correction (`set_block(air)` + `sync_block_entity(None)`) | a world that never had a chest, required **pixel-identical** |

The first one takes its state from `sim::predicted_placement_state` — the same resolver a click uses —
and asserts that state's properties are `facing=north, type=single, waterlogged=false` **before**
measuring any pixel. That order matters: `minecraft:chest` has 24 states and a waterlogged or
wrong-facing one fills the identical rect, so a gate that chose its own state and then looked at
pixels could not tell a correct prediction from a plausible one. The state id itself is pinned to
`blocks.json` by `sim.rs`'s hermetic `placement_states_resolve_to_the_jar_oracle` (`chest` facing
north is **3988**; note the *lowest* chest id, 3987, is waterlogged — `BooleanProperty` orders its
values `{true, false}`).

The second gate's premise is asserted first too — the prediction must be gathered as one spawn before
the removal is measured — because "no chest is drawn" is trivially satisfiable by never having drawn
one. And `block_entity_type(air) == None` is asserted explicitly, so a census change that started
handing back `Some` there fails loudly instead of silently keeping the chest.

**Neither gate has been run.** They were written in a session whose verification was batched; the
author did not execute them and did not watch either control fail. The resolver underneath them *was*
exercised standalone — copied into a scratchpad with a stand-in census and run under
`rustc --edition 2024 --test`, where the chest/slab/log/stone resolutions and all four declines pass —
but that is the pure function, not the gate.

## What is not built

Against the real 26-entry registration list (see above), not the issue's original twelve-item guess:

- **Signs — landed, both registrations** (see the [Sign](#sign-standingwall-text) section above):
  standing, wall, ceiling-hanging and wall-hanging. Still open within sign scope: the
  black-dye-glowing outline, per-glyph world-light modulation, line wrapping past the per-kind
  `max_text_line_width`, and rich per-run text formatting — all named and reasoned about in that
  section rather than silently missing. **The "hanging signs need a different model set" item that
  used to head this list was a 1.20 memory, not a 26.2 measurement** — it was four numbers.
- **Lectern — landed**, including the live per-frame install (see [Lectern](#lectern) above).
  **Conduit remains the one registration nothing here has ever named in either direction** — see the
  correction under [the registration list](#the-real-vanilla-scope-from-the-registration-list--not-the-issues-guess-list).
  Nothing is open within lectern scope: `openness` is a compile-time constant in the jar, so there is
  no animation left to wire.
- **Bell — landed**, including the `BLOCK_EVENT` shake trigger and the live per-frame install (see
  [Bell](#bell) above). The trigger needed `b0 == 1` decoded as a **different** thing from chest's own
  `b0 == 1` (direction packed into `b1` via `Direction.from3DDataValue`, not a viewer count) plus a
  50-tick clock in `BellShakes`. Nothing is open within bell scope.
- **Shulker box — landed** (see [Shulker box](#shulker-box) above), including the live per-frame
  install. Still open within shulker scope: the lid open/close animation, which needs the
  `BLOCK_EVENT` fold `ChestLids` already has a shape for.
- **Banner — landed, both attachments**, including the live per-frame install: the block-name base
  colour and NBT pattern gather (`block_entities::banner_spawns`), the pole/bar/flag rig with
  vanilla's sway, the pole-less wall rig, and the **ordered translucent mask pass** that composites
  the pattern layers through `EntityPipeline::banner_layer_pipeline`. See
  `docs/banner-shield-patterns.md` and [Wall banners](#wall-banners) below. Still open within banner
  scope: the **shield** form of the same compositing function (`shield_pattern_layers` still has no
  consumer — a shield is an item model in the hand, not a block entity, so it is a different pass).
- **Campfire — landed**, both campfire blocks, including the per-frame install (see
  [Campfire](#campfire) above). Nothing is open within campfire scope: the renderer draws only the
  cooking items and has no animation at all.
- The enchanting-table book (a full animation state machine —
  `open`/`flip`/`rot`/`time`, all client-simulated, none of it on the wire, closer in scope to the
  chest lid than to a static model; **the mesh it needs is already built and baked** — `book_model`,
  shared with the lectern — so what is left is purely the state machine and a `resolve_*` that feeds
  a live `openness`/`page_flip` into `book_part_poses`), mob spawner (draws a miniature spinning entity inside the cage —
  reuses full entity rendering, not a simple cuboid rig), piston head, brushable block,
  decorated pot (`decorated_pot` atlas; its sides need **up to four independently textured sprites
  per instance** from NBT `sherds`, which the current `(model, texture)` single-texture-per-instance
  batch key cannot express as one instance — it would need decomposing into a plain base plus up to
  four small per-side instances, not a straightforward follow-on to chest/skull), trial spawner,
  vault, copper golem statue, shelf. End portal/end gateway/beacon are their own shader effects, not
  cuboid rigs, and structure block/test instance block are creative/dev-only — none of the four
  belong in "what a survival player sees."

Also unbuilt for chests specifically: the `BrightnessCombiner` that makes a double chest's two halves
share one light sample, and the `SpecialDates.isExtendedChristmas()` clock behind
`chest_material_with_season` (the function is ported and tested; nothing calls it with `true`).

## Dependencies

- `lodestone-assets` — `entity::{CubeDef, PartDef, EntityModelDef, PartPose, Affine, bake_entity_parts}`,
  `Image::decode_png`, `ResourceManager`/`ZipSource` for the jar.
- `lodestone-render` — `entity::{push_part_quads, PartRange}`, `entity_pipeline::{EntityPipeline,
  GpuEntityModel, EntityCameraUniform, upload_instances_tinted}`, `camera::Frustum`, `models::ModelVertex`.
- `lodestone-world` — `BlockEntity`, `LoadedChunk::block_entities`, `ChunkColumn::get_block`,
  `World::sync_block_entity` / `BlockEntitySync`, and (for sign text) `sign_text::{SignText, SignSide,
  SignDyeColor}` plus a real (non-dev) `serde_json` dependency for the JSON-text message parse.
- `lodestone-data` — `block_states::{block_name, properties}` for the material and the
  `facing`/`type` properties; `block_entity_types::block_entity_type` for the state→type census the
  block-update path creates records from.
- `lodestone-render` (for sign text) — `sign::{SignOrientation, SignSpawn, TEXT_LINE_HEIGHT,
  dye_text_color_rgb, sign_side_color, sign_text_transform}`, itself depending on `lodestone-world`
  for `SignSide`/`SignDyeColor` (already a real, non-optional dependency of this crate).
- `lodestone-shell` — `net::{SharedHandle, entity_light_at}`, `resources::asset_root`, and (for sign
  text) `gpu/nametag.rs`'s `pub(super) layout_ink_runs`/`load_font`, reused rather than duplicated a
  third time by `gpu/sign_text.rs`.

## Related

- [`entity-rendering.md`](./entity-rendering.md) — the cuboid-rig machinery this reuses.
- [`gpu-module-layout.md`](./gpu-module-layout.md) — the bind-group budget and pass ordering.
- Vanilla reference: `.cache/mc/26.2/client-src/net/minecraft/client/{model/object/{chest/ChestModel,
  skull/SkullModel,bell/BellModel},renderer/blockentity/{BlockEntityRenderers,ChestRenderer,
  SkullBlockRenderer,BlockEntityRenderDispatcher,AbstractSignRenderer,StandingSignRenderer,
  BellRenderer},renderer/Sheets}.java`, plus
  `net/minecraft/world/level/block/{BellBlock,entity/BellBlockEntity}.java` for the block-state
  properties and the `BLOCK_EVENT` trigger bell does not yet decode.
  `BlockEntityRenderers.java` is the registration list itself — read it directly rather than trusting
  any summary of it, including this doc's, the next time the scope needs re-deriving.
- [`banner-shield-patterns.md`](./banner-shield-patterns.md) — the pattern compositing function and
  its now-live block-entity consumer. **This entry used to say the function was "confirmed still an
  island this session"; that is stale twice over** — `resolve_banner` has called
  `banner_pattern_layers` since #174's step D, and the shell now installs a banner source and draws
  the layers. The shield form is still without a consumer.
