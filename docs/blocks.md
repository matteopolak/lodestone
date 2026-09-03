# Blocks: placement, breaking, outlines, sound, entities and persistence

## What it is

Everything about a block once it exists in the world: how a right-click
resolves to a placed state (server rule table, and the client's own
prediction of it), how long a block takes to break and what interrupts that,
which shapes a block presents to selection vs. collision, what sound a block
makes, how the server actually mutates and persists an edited block, the four
simulated block-entity types (composter/furnace/hopper/brewing stand), bone
meal, and how entities/players (not just terrain) survive a world restart.
Structure chests, block drops and loot tables live in
[`docs/mining-and-drops.md`](./mining-and-drops.md).

## How it works

### Placement conventions (server-side rule table)

`crates/lodestone-server/src/block_placement.rs`'s `placement(block, ctx,
block_at)` returns a state string plus any extra cells a placement owns (a
door's upper half, a bed's head, a chest's re-typed partner). There is no
single convention — vanilla stairs/doors face **with** the player,
furnaces/chests face **at** the player, an anvil takes the clockwise
direction, dispensers/pistons take the look direction's opposite, and shulker
boxes/amethyst/lightning rods just take the clicked face. Family is
classified from a cached block → `Shape` census (which properties a block's
states actually carry — a `hinge` means a door, `type` over
`top/bottom/double` a slab) built once over `lodestone_data::block_states`, so
a new 26.2 block reaches the right arm with no edit here; name lists
(`FACING_IS_LOOK`, `FACING_IS_CLICKED_FACE`) exist only where the census
cannot separate two same-shaped families (a ladder vs. a lectern). A wall
click is a **different block**, not a rotated one (`torch` → `wall_torch`,
etc.), verified against the census so an accidental suffix match yields
`None` rather than an unresolvable state. Never compute a state *id* here —
return a property-named string and let the encoder (`resolve_state_id`)
resolve it against the jar-marked default; re-deriving id arithmetic has
broken before.

Known gaps: a chest's sneak-placement branch is unmodelled (no client sneak
state reaches the server); `canSurvive` is collapsed to "use the clicked
face"; a bed/door whose second cell is occupied still places anyway; and
waterlogging is never set on placement.

### Client-side placement prediction

On a live server the shell now writes the placed block into its own world
**immediately**, instead of waiting for the server's `BLOCK_UPDATE` — writing
both the block state and its block entity in one call
(`world.set_block(..); world.sync_block_entity(..)`), the same two calls in
the same order the server's own confirmation path uses. `PlacementFacts` are
read up front (four questions over two positions) rather than answered
re-entrantly, because the decision needs the ECS write guard while a
re-entrant world read would nest `chunks → World` — the one lock order this
codebase forbids.

Nothing has to detect a server refusal: vanilla's server sends a
`BLOCK_UPDATE` for **both** the clicked cell and its neighbour after every
`use_item_on`, unconditionally, so a mispredicted cell is corrected within one
round trip and `sync_block_entity` on that same arm removes any block-entity
record the prediction wrongly created. Every classification here is therefore
allowed to err toward **not** predicting, never toward predicting wrong.

Resolving a full state (`state_for_placement`) needs values for every
property, since no census here carries a block's registered default state.
**Never substitute "the lowest state id for this block" for the default** —
`BooleanProperty`'s value order makes the lowest chest id a *waterlogged*
chest, and the lowest slab id a *top* slab; both still render as the right
block, so a pixel gate that picked its own state would pass regardless.
Values come from geometry, a handful of explicit defaults, per-block
overrides, and `NON_GEOMETRIC_DEFAULTS` — a **measured** set of 60 property
names (of 93 seen across `blocks.json`'s default states) that take exactly
one value across all 1,196 blocks; the other properties are geometric,
per-block, or excluded because vanilla computes them from context at
placement time (`persistent` on `LeavesBlock`, for instance, defaults `false`
but a player-placed leaf is `true`). Measured coverage: 721 of 1,196 blocks
resolve; 453 decline; 22 differ from the default only in `waterlogged`
(correctly, since the shell only ever predicts into air).

### Break timing

Per-block hardness (`VersionAdapter::block_hardness`) and per-item mining
speed (`VersionAdapter::tool_mining`) meet in `lodestone-game`'s `mining`
module; the shell (`Sim::drive_mining`) resolves the crosshair target's
state, the held item, and folds them into `BreakInputs`. Two traps, both
about the same field pair getting fed straight across instead of negated:

- `BlockHardness::requires_correct_tool` is a property of the **block**
  (drops nothing without a suitable tool); `BreakInputs::correct_tool` is a
  property of the **held item vs. the block**, and picks vanilla's 30
  (correct) vs. 100 (wrong) speed divider. Bare-handed they are *opposites* —
  an empty hand is "correct" for exactly the blocks that demand no tool.
  Assigning the block's flag straight to the item's field compiles, looks
  like faithful wiring, and makes bare-hand stone break in 45 ticks instead
  of the correct **151** (f32 accumulation across many additions, not a
  division — the number really is 151, not 150, and server-confirmed). When a
  tool is held, `ToolMining::correct_tool` is **already folded** the right
  way; re-inverting it a second time reintroduces the bug from the other
  side.
- `submerged` reads the raw `fluid_state.eye_in_water` flag, not
  `FluidState::under_water()` (which additionally requires the whole body to
  be submerged and is what fog selects on) — the two functions answer
  different vanilla questions and must not be harmonised.

An unknown block state or a version-free build refuses to dig rather than
guessing a hardness — guessing a number here is exactly how breaking got too
fast the first time. The v26-2 census covers all 32,366 real states, so this
never fires against a real server.

### Outline and interaction shapes

Per-state outline (`getShape`, what block **selection** uses) and interaction
(`getInteractionShape`, refines the hit *face* only, never adds a hit) shapes,
dumped and committed the same way as collision shapes — see
`docs/lodestone-data-crate.md`. The outline shape is a **third** thing,
neither collision nor fluid presence: 50.9% of all 32,366 states have an
outline differing from their collision shape, and only 3,328 states are a
true full cube. Cobweb is the cleanest proof the two censuses are not
interchangeable — it colllides with nothing but outlines to a full cube (no
override at all), while kelp and seagrass hardcode a water fluid state yet
have a real, non-empty outline and are targetable. `minecraft:light`
outlines to nothing in this census because the outline getter takes a
`CollisionContext` this table evaluates as "not holding a light item" — the
correct answer for that case, not a bug.

Both the drawn selection box and the pick **ray** must read this table, not
collision or a hardcoded unit cube — a box drawn from the real outline while
the ray still clips a per-cell unit cube is the most convincing way for a bug
to hide (leaf litter stayed both highlighted and punchable from well above
it, because the hit test never saw the census at all). `raycast` now clips
against real per-state boxes and takes its hit face from the box, not the DDA
cell boundary crossed, which also fixes face-driven placement one block off
target.

Rendering: the wireframe box used `PrimitiveTopology::LineList`, which
rasterises at exactly one physical pixel regardless of resolution — reported
as "too dim," confirmed by pixel gate to be a thickness problem, not colour
(vanilla's own alpha ≈ 0.4 was already matched; this shader was at 0.6).
Vanilla never uses a GPU line-width parameter either — it expands the line to
real screen-space quad geometry with a minimum width of `max(2.5,
window_width/1920*2.5)` logical pixels. `OutlineRenderer` now submits each
edge as 6 vertices (2 triangles), pushing each vertex along the on-screen
perpendicular by `half_width_px * side`, computed from the real render
target's pixel size so it always matches the surface actually drawn to.

### Block sound types

`lodestone_data::sound_types` is the per-state break/step/place/hit/fall
sound census — `LEVEL_EVENT` 2001 (a block break) carries only a state id and
nothing else, so a client without this table can decode the packet perfectly
and still have nothing to play. Measured: 126 distinct vanilla sound-type values
over 32,366 states (packed as a 126-entry table plus a per-state `u8` index,
~19× smaller than a per-state tuple). Hand-transcribing vanilla's own sound-type
registration
would have been wrong in three separate, measured ways: it declares 127
constants but only 126 are reachable from any real block (`TWISTING_VINES` is
dead — vanilla's twisting-vines blocks use `WEEPING_VINES` instead); `IRON`
and `METAL` are different types and the obvious name-to-block pairing is
backwards (`iron_block` is `IRON`; `METAL`, at pitch 1.5, belongs to gold/
diamond/emerald blocks, rails and hoppers); and two constants mix families
(`HARD_CROP` is wood's four sounds plus a different placement sound). Air
itself has a sound type (stone's) — a consumer must guard on `!is_air` or a
level event on an air cell plays a stone break. `minecraft:decorated_pot` is
the one block whose sound is keyed by **state**, not by block (cracked vs.
intact), which a block-keyed table structurally could not express.

### Block support, placement consumption, and item use

Three server-side joins that closed real gaps rather than adding new models:
a block whose support cell goes to air or fluid now pops off and drops
(`crate::block_support`, a **generated** 291-row table mapping each block to
the nearest vanilla ancestor whose survival check is a self-destruct on one
named support cell — hand-typing this table once lost 18 rows and invented
8), placing a block now actually consumes the item (`ItemStack::consume`, the
placement branch previously never touched the inventory), and a right-click
in mid-air now dispatches vanilla's own ordered item-use arms
(`CONSUMABLE` → eat, `EQUIPPABLE` → swap into the slot; shield-raise and
kinetic-weapon are not modelled). Eating ends on the **server's own clock**,
not a client packet — a per-tick arm lands the bite when `useItemRemaining`
counts to zero, mirroring vanilla exactly. Vanilla's own equip-slot-swap
routine's
count branch matters: with `count <= 1` the whole stack swaps and the old
piece returns to the hand; with `count > 1` only one item equips and the rest
stays in the hand while the old piece goes to inventory (or the floor).

The support-collapse cascade (`server::collapse_unsupported`) is a bounded
breadth-first walk from the broken cell's neighbours, capped at 64 (this
crate's stand-in for `maxChainedNeighborUpdates`), and this landing also gave
`destroy_block` its first neighbour fan-out at all — breaking a block beside
redstone dust previously never recomputed the dust. Gotchas: the support test
approximates vanilla's `isFaceSturdy` as "went to air or a fluid," which is
narrower and fails safe (a torch on a fence is left alone rather than
destroyed); `lily_pad`/`frogspawn` are excluded because they are *supported
by* water, and an air-or-fluid trigger would destroy them on sight; and the
creative no-drop fork is genuinely two different rules (a player's own break
skips drops in creative; a cascaded self-destruction always drops, matching
vanilla's `updateOrDestroy` not knowing who caused it).

### Editing: dig, place, and persistence

The decode → mutate → confirm path for `PLAYER_ACTION` (breaking) and
`USE_ITEM_ON` (placement). The break sequence tracks one `PendingBreak` per
connection: `StartDestroy` prices the dig and breaks immediately if progress
reaches 1.0 in one tick (the "insta-mine" branch, needed because a client
that knows a block is instant sends no `StopDestroy`); `AbortDestroy` clears
the pending dig only if it names the same position; `StopDestroy` breaks the
block if enough progress has accrued **or defers**, rather than refusing, if
it hasn't — a same-tick Start/Stop pair (what a local integrated server
always sees) can never clear the 0.7 immediate-break threshold on its own, so
refusing an early Stop made every non-instant block unbreakable.

Placement resolves the held item through a real item→block census
(`block_items::block_for_item`, ~1,537 items) rather than hardcoding
`minecraft:stone` for every placement, which was the original, now-retired
behaviour — that stone fallback outlived its own justification once an
inventory model existed, and stayed hidden because the client predicts
placement locally (so the wrong server value never desynced visibly) and the
`block_update` was genuinely sent, just carrying the wrong value — invisible
to any wire-connectivity scan. Placement also refuses to intersect the
placer: the placed state's real collision boxes are tested against the
player's own bounding box, so a state with an empty collision shape (a torch,
a rail, redstone dust) is never obstructed, matching vanilla exactly. A bare
block name (no convention resolved) now writes vanilla's real **default**
state rather than the lowest matching id — the former fallback was wrong for
661 of 797 multi-state blocks (every blade of grass rendered snowy; dust's
four connections defaulted to climbing rather than flat).

Server-retained state: `OverworldChunkSource` regenerates any *unedited*
column fresh on every request (cheap, since the generator is deterministic)
and only promotes a column into a permanently-retained edit map the moment a
`set_block` actually touches it — caching every generated column regardless
of edits was rejected because memory would scale with how much of the world a
session merely looked at, not how much it changed. `resolve_state_id` is a
linear scan over the ~32k-entry state table; safe for an occasional
confirmation packet, memoized per distinct string when a whole-column encode
needs to call it per block.

### Block entities: composter, furnace, hopper, brewing stand

Four independent, pure, tick-driven state machines
(`crates/lodestone-server/src/{composter,furnace,hopper,brewing}.rs`), each a
value type with one `tick(&mut self) -> …Tick` method reporting what changed —
no `ChunkSource`, no connection, no async. `BlockEntityRegistry` is the shared
`HashMap<BlockPos, BlockEntity>` (an enum over the four types, plus a
27-slot `Container` variant for chests/trapped chests/barrels), advanced by
the unified 20 Hz tick loop alongside the mob simulation. Placement honours
the held item for these blocks specifically (via `block_entity_for_item`);
right-clicking an already-registered furnace or hopper opens its real screen
(`OPEN_SCREEN`/`CONTAINER_SET_CONTENT`/`CONTAINER_SET_SLOT`/
`CONTAINER_SET_DATA`) and a background tick reaches an already-open window
with no client input at all (`sync_open_container` diffs slots/data against
what was last pushed, on the same 50 ms cadence as the tick loop). A
composter has no menu — matching vanilla, which never opens one — and is fed
directly by a right-click (`apply_composter_use`): a compostable item is
rolled against its chance and consumed even on a failed roll; at fill level 8
*any* click (including an empty hand) extracts bone meal instead, because
vanilla's item-offer dispatch falls through to the empty-hand handler for any
click the offer does not itself consume.

Block entities persist across a reopen: each type has a `restore` associated
function taking **every** field at once, deliberately, so adding a field
without updating the save schema is a compile error rather than a silent
data loss — a setter-based restore would compile fine and quietly drop the
new field on every load. The composter is saved under a namespaced id
(`lodestone:composter`) because vanilla has no composter block entity at all
(its fill level is a block-state property, and its ready delay is a
scheduled block tick) — a deliberate divergence, not a schema mistake.

Still open, stated plainly: a brewing stand's `Bottle` slots are not real
`ItemStack`s, so its menu cannot be opened; there is no server-authoritative
click resolution for a non-zero window (a client's own predicted diff is
applied verbatim); and nothing sends `container_close` when the block backing
an open window is broken out from under it.

### Why placement resolution is two functions

`block_entity_for_item` matches an item id down to a small `PlacedBlockEntity`
descriptor (`placed_block_entity_for_item`), and only
`PlacedBlockEntity::instantiate` ever builds a `BlockEntity`. The split is a
stack-frame constraint rather than taste, and collapsing it back into one match
breaks unrelated suites in a way that looks nothing like this code.

A debug build gives every arm of a `match` its own stack slot for that arm's
temporaries, so such a frame is the *sum* over the arms rather than the largest
of them. `size_of::<BlockEntity>()` is 16,504 bytes — its `Crafter` variant
holds an inline `[Option<ItemStack>; 9]`, and `size_of::<ItemStack>()` is 1,832
on its own — so a single forty-arm item-id match materialising one `BlockEntity`
per arm reserves 1,357,056 bytes in its prologue, more than a default thread
stack. The symptom is a stack overflow inside a single frame, with no recursion
anywhere: *calling* the function is enough to abort the process. Split in two, the same
call path reserves 33,200 + 17,392 = 50,592 bytes across the two frames that are
live at once. Boxing the payload is not an alternative — `Box::new(expr)`
evaluates `expr` into a stack temporary before the move, so each arm still
reserves its 16,504 bytes.

Read the frame, do not estimate it. It is the `sub sp, sp, …` immediate (or, for
a frame over a page, the bound of the probe loop that precedes it) in the
prologue:

```bash
ar x target/debug/liblodestone_server.rlib   # rlib members are the objects
llvm-objdump -d --disassemble-symbols=<mangled symbol> <member>.o | head -20
```

A census over every member of the rlib finds the sibling frames, all downstream
of that same 16,504-byte enum: `BlockEntityRegistry::tick_hopper` (200,080 — it
holds three `Option<BlockEntity>` across its remove/tick/reinsert),
`chunk_nbt::block_entity_from_nbt` (153,584), `structure_loot::chests_for_chunk`
(101,104), and the `HashMap::insert`/`Vec` collect instantiations over
`(BlockPos, BlockEntity)` (about 66,000 each). None is overflow-scale alone. The
one change that shrinks all of them at once is boxing `BlockEntity::Crafter`'s
slot array, which drops the enum to the `Furnace` variant's ~5.5 KB; it touches
every `Crafter { .. }` pattern in the crate and is deliberately not part of the
overflow fix.

`block_entities::tests::resolving_a_placement_fits_a_modest_stack` is the guard,
since the type system cannot state "this frame stays small" and the regression
is invisible until some unrelated suite dies. It re-execs the test binary and
resolves one item per descriptor variant on a thread holding
`PLACEMENT_STACK_BUDGET`; an over-budget frame trips that thread's guard page,
and running it in a child turns the resulting abort into a named assertion in
the parent rather than a bare `SIGABRT` that takes the whole suite with it.

### Bone meal

`apply_bone_meal(state, above_state, rng)` is a pure decide-then-apply
function mirroring `BoneMealItem::useOn` and its per-block
`isValidBonemealTarget`/`isBonemealSuccess`/`performBonemeal` triple: crops
(wheat/carrots/potatoes) always succeed and jump 2–4 growth stages;
beetroot's growth is the *same* single draw divided by 3 (not a second,
smaller draw — a common wrong guess); saplings succeed 45% of the time and
either advance one stage or are meant to grow a tree. **The item is consumed
even on a failed roll** — vanilla shrinks the stack outside the success
branch, so getting this backwards gives players free bone meal. Two named
gaps stay `NotModelled` (consumes nothing) rather than guessing a partial
effect: grass's vegetation-feature placement and a stage-1 sapling's tree
growth both need a feature placer this crate does not have, and a partial
implementation would draw a different number of RNG values than vanilla and
desync every later roll on the same connection.

### Entity and player persistence

Before this landed, restarting a world deleted every mob, every dropped item,
and the player's whole inventory, silently. The pieces: a per-player gzip
`.dat` file at `<world>/players/data/<uuid>.dat` (**not** `playerdata/` — a
reader pointed at the pre-1.21 path finds nothing and reports "new player,"
which looks exactly like correct first-join behaviour); a sibling
`entities/` region set (not a field of the terrain chunk, since 1.17), whose
per-chunk schema is a two-element `Position: IntArray[2]` with **no**
`yPos` — code reaching for a terrain chunk's `xPos` finds nothing and files
every entity under chunk `(0, 0)`; and a `DataVersion` gate that refuses
(loudly, at world open, before any byte is written) any on-disk version other
than exactly what this build writes, because there is no upgrade path and a
half-correct re-save of a mismatched schema has already, once, erased every
cave biome in a real world. An absent or newer `DataVersion` is refused for
the same reason a stale one is: reading it wrong is not a smaller mistake
than not reading it at all.

Both the entity and player readers/writers keep every field they do not
understand and write it back **verbatim** (`SavedEntity::extra`,
`PlayerData::preserved`) — a real vanilla mob or player file carries dozens of
fields this server does not simulate, and a writer that emitted only what it
understands would delete all of them on the first save. **A field may be
excluded from that catch-all only if its decode actually consumed it, never
by name** — the same NBT key can mean two different things with two
different tag types on two entity classes (`Age` is a `Short` — ticks alive —
on a dropped item, and an `Int` — breeding age, negative for a baby — on a
mob; `Health` is a constant `Short` on an item and a real `Float` on a mob). A
name-keyed exclusion list that matched `"Age"` decoded the mob's `Int` as a
`Short`, failed, and silently dropped it — every baby mob in a loaded world
would have become an adult, with a clean parse and no error anywhere.

Stale entity records are cleared **by UUID**, not by rewriting only chunks
that currently hold entities (which leaves a moved mob's old chunk still
holding a stale copy, duplicating the mob on the next load) and not by
rewriting every chunk in the file (which deletes a real vanilla world's
thousands of untouched entities the first time this sim saves over it):
every live entity's UUID goes into a set, a stored record whose UUID is in
that set is dropped as "already re-saved elsewhere," and an unknown UUID is
preserved byte-for-byte. Saving cadence: the player `.dat` writes on clean
disconnect **and** every ~30 s regardless, because the disconnect path is
reached by only one of several exit routes (a keep-alive timeout, a crash, or
a cancelled task at shutdown all skip it) — the common case (alt-F4) would
otherwise lose the whole session. Entity restore runs in the mob-seeding task
**after** `MobHandle::reseed`, because reseeding replaces the whole
simulation outright; restoring before it would delete every restored mob with
a completely green build.

Not yet done: projectiles are not persisted (the live registry holds no
owner, pickup state or damage to faithfully restore); hunger and the ender
chest are preserved but not simulated; only the overworld has entity storage
at all.

## How to change it

- Adding a placement family: extend `placement`, keyed off the block-state
  `Shape` census where possible; never a new name list unless the census
  genuinely cannot separate two families.
- Adding a support family: add the vanilla base class to
  `scripts/derive-block-support.py` and regenerate — never hand-add a block
  name or hand-transcribe the script's output.
- Adding a block-entity mechanic's numbers: each type's own module owns its
  constants, cited against the same decompiled source or generated recipe
  JSON the rest of the table came from.
- Adding a modelled player field: add it to `MODELLED_FIELDS` or the writer
  emits the NBT key twice (legal, but read back arbitrarily).
- Never derive placement's default-state fallback, break-time constants, or a
  saved schema's field list from a sibling implementation instead of the jar
  — see the specific traps above for each.

## Configuration

`--protocol <n>` / the `live` feature select which family's hardness, outline
and support censuses are resolved; without `live`, digging and placement
prediction are unconditionally refused rather than guessing. No block-entity
mechanic has a feature flag or env var of its own. Persistence has none of
its own — the world directory and autosave interval come from
`IntegratedServer::open_persistent_with_mobs`.

## Dependencies

`lodestone_data::{block_states, collision_shapes, outline_shapes,
sound_types, block_entity_types}` for every generated per-state census;
`crate::redstone`'s state-string helpers for placement's shadowed families;
`crate::loot`/`block_drops` for the support cascade's drops; `lodestone-anvil`
for the region container, gzip NBT, and the version gate; all persistence
paths are target-gated off `wasm32` (a browser world has no filesystem). See
[`docs/lodestone-data-crate.md`](./registries.md) for how each
generated table is dumped and regenerated, and
[`docs/world-save-load.md`](./world-persistence.md) for terrain/scheduled-tick
persistence, which this doc's entity/player half sits beside.
