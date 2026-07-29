# Tool mining speeds

## What it is

How fast a *held item* mines a given block state, and whether it is the
correct tool for that block's drops — vanilla `ItemStack.getDestroySpeed` and
`Player.hasCorrectToolForDrops`, for protocol 776 (Minecraft 26.2). This is
the item half of break-time math; the block half (`destroySpeed`,
`requiresCorrectToolForDrops`) is the pre-existing hardness census documented
in [`block-break-timing.md`](./block-break-timing.md). Landed in `875f452`.

Before this, `crates/lodestone-shell/src/sim.rs` fed every dig `BreakInputs::default()`
for tool fields, so a pickaxe made nothing faster: obsidian was ~4m10s of
unbroken holding regardless of what was in hand.

**This doc covers the data/evaluation seam** —
`crates/protocol/v770/src/tool.rs`, `lodestone_model::VersionAdapter::tool_mining`,
and the generated tables. As of `875f452`, `crates/lodestone-shell/src/sim.rs`
does **not** call this seam yet — `bare_hand_break_inputs` still hard-codes
`BreakInputs::default()` for `tool_speed`/`mining_efficiency`/etc. and never
reads the player's held item. Grepping the whole tree for `tool_mining(`
outside its own trait/impl definitions turns up nothing. The wiring described
here is real, tested, and reachable through `VersionAdapter`, but as of this
commit it is not yet *consumed* by the running client — see CLAUDE.md's note
on "islands." Wiring it into `sim.rs` is the natural next step and should
replace `bare_hand_break_inputs`'s doc comment (which still says "Tool speed
… left at `BreakInputs::default`") once done.

## How it works

### Why a version-owned census is needed at all

The obvious approach — decode `minecraft:tool` off the wire and evaluate it —
only covers the wire, and the wire is not where most tools live:

1. **A vanilla pickaxe sends no `minecraft:tool` at all.** A clientbound item
   stack carries a `DataComponentPatch`, the *delta* from the item's built-in
   prototype component map. 26.2 registers a pickaxe's `minecraft:tool` in
   that prototype (`ToolMaterial.applyToolProperties`), so `/give … diamond_pickaxe`
   arrives as an **empty patch**. The client is expected to already know the
   component — that prototype table is version data
   (`crate::generated_tools::ITEM_TOOLS` in `crates/protocol/v770/src/generated/tools.rs`).
2. **A rule names blocks by tag** (`Tool.Rule.blocks`, typically
   `#minecraft:mineable/pickaxe` or `#minecraft:incorrect_for_<material>_tool`).
   Tag membership is version data (`generated::BLOCK_TAGS`).
3. **When a rule names blocks directly it uses registry ids**, and matching
   those against a *block-state* id needs the state→block map, which is
   renumbered every version (`crate::generated_block_registry::STATE_BLOCK`).

A wire-supplied `minecraft:tool` (`/give …[minecraft:tool={…}]`, datapack
items) still overrides the prototype (`ToolPatch::Set`) and is evaluated by
the exact same code path, so the two sources cannot drift apart.

### The evaluation entry point

`crates/protocol/v770/src/tool.rs`, `pub fn mining(held: Option<&ItemStack>, state_id: u32) -> Option<ToolMining>`:

1. Looks up `BlockHardness::requires_correct_tool` for `state_id` (reuses the
   existing hardness census — see `block-break-timing.md`). Returns `None` if
   the state is unknown.
2. Resolves the effective patch exactly as `ItemStack.get(DataComponents.TOOL)`
   does: `held.map_or(&ToolPatch::Inherited, |s| &s.components.tool)`.
3. Branches on that patch:
   - `ToolPatch::Set(tool)` — evaluate the wire-decoded `ItemTool` (from
     `lodestone_model`) against the block.
   - `ToolPatch::Removed` — bare hand, regardless of what item this is.
   - `ToolPatch::Inherited` with no held item — bare hand.
   - `ToolPatch::Inherited` with a held item — look up `default_tool(item_name)`
     in the generated prototype table; bare hand if that item has none.
4. Both the wire-decoded and generated-prototype branches funnel into one
   shared `fn evaluate(...)`, so the two data sources cannot diverge in how a
   rule list is walked.

`evaluate` replays vanilla `Tool.getMiningSpeed` + `Tool.isCorrectForDrops`:
walk rules in order, first match wins *independently* for speed and for
correct-for-drops (a rule that only denies drops does not stop the speed
search — this is how `#incorrect_for_<material>_tool`, no speed, sits ahead of
`#mineable/<class>`, which has one, without shadowing it). Falls back to
`Tool.defaultMiningSpeed` for speed, and to `false` folded with the block's
own requirement for correctness:

```rust
correct_tool: !requires_correct_tool || correct.unwrap_or(false),
```

The bare-hand case (`fn bare_handed`) is the same formula with no rules at
all: `speed: 1.0`, `correct_tool: !requires_correct_tool`. This is where
Gotcha 1 below lives.

The result is `ToolMining { speed, correct_tool, damage_per_block }`
(`lodestone_model::adapter::ToolMining`) — the two `BreakInputs` fields the
`lodestone-game` mining formula needs (`tool_speed`, `correct_tool`) plus
`damage_per_block` for durability. `correct_tool` here is **already**
`Player.hasCorrectToolForDrops` — the block's own requirement is folded in, so
a caller has nothing left to invert (see Gotcha 1).

### The break-time formula

The tick-accumulation formula itself lives in `lodestone-game`'s `mining`
module (`BreakInputs`, `Mining::continue_`) and is unchanged by this commit —
this commit supplies inputs to it, not the loop. `crates/protocol/v770/tests/tools.rs`
restates the shape (since `lodestone-v770` cannot depend on `lodestone-game`)
as `fn ticks_to_break`:

```rust
let divider = if correct_tool { 30.0 } else { 100.0 };
let per_tick = speed / hardness / divider;
// accumulate per_tick each tick until progress >= 1.0; count ticks
```

Reference values pinned by that test file, all read from committed,
server-derived tables:

| case | `speed` | `correct_tool` | ticks |
| --- | --- | --- | --- |
| diamond pickaxe on stone | 8.0 | true | **6** |
| bare hand on stone | 1.0 | false | **151** |
| diamond pickaxe on obsidian | 8.0 | true | 188 |
| wooden pickaxe on obsidian (speed applies, tier denies drops) | 2.0 | false | 2500 |
| bare hand on obsidian | 1.0 | false | 5001 |

151, not 150, for the same reason `block-break-timing.md` documents for the
bare-hand-only path: this is f32 accumulation across many additions, not a
division, and it is server-confirmed.

### Data source

Same headless-server-dump pattern as the pre-existing hardness census, in a
new file: `crates/protocol/v770/oracle-java/ToolOracle.java`, boots the real
26.2 server, binds the vanilla datapack's tags, runs the item component
initializers, and dumps three record kinds to stdout:

- `B <id> <name>` — every `minecraft:block` registry entry in **registration**
  order (`air` = 0).
- `T <tag> <member> <member> ...` — every bound block tag's membership.
- `I`/`R` lines — every item's built-in `minecraft:tool` prototype and its
  rules.

None of the three is on the wire in the normal case (see "why a version-owned
census" above), so none of them can be derived from a live packet capture —
only from booting the jar. The dump is committed at
`crates/protocol/v770/tests/support/tool_jvm.txt` as the external anchor.

`crates/protocol/v770/tests/tools.rs` parses that dump and, in an
`#[ignore]`d `committed_tables_match_dump` (actually named
`committed_tables_cover_the_committed_dump` for the hermetic half — see below),
regenerates `src/generated/tools.rs` and `src/generated/block_registry.rs`
and diffs against what is committed. To regenerate after a version bump:

```text
# 1. Re-dump from the server
CACHE="$(cd .cache/mc/26.2 && pwd)"
HERE="$(cd crates/protocol/v770/oracle-java && pwd)"
docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
  CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
  cp /oracle/ToolOracle.java /work/ && javac -cp "$CP" -d /work /work/ToolOracle.java
  java -cp "/work:$CP" ToolOracle 2>/dev/null'
# then copy stdout over tests/support/tool_jvm.txt, keeping the `#` header

# 2. Regenerate the committed tables
LODESTONE_REGEN=1 cargo test -p lodestone-v770 --test tools \
    committed_tables_match_dump -- --ignored --nocapture
```

The item half of the dump is independently cross-checked (not just trusted)
against Mojang's own `generated/reports/minecraft/components/item/*.json` —
a second artifact, so the two must agree rather than one merely restating the
other. The block-registry-order table and the tag membership were similarly
cross-checked against `registries.json` and the extracted vanilla datapack
tags. Two of those cross-checks are committed as tests, not just claims.

The **hardness** half (`requires_correct_tool` per state) is *not* new here —
it is the table `block-break-timing.md` already documents, dumped by
`HardnessOracle.java` and read by `crates/protocol/v770/src/hardness.rs`. This
commit only adds the tool/registry/tag data on top of it.

### The live wire-decode gate

`crates/protocol/v770/tests/live_tool_component.rs` joins a real server and
diffs the decoded `minecraft:tool` patch against the JVM oracle's own
component report, live. It is gated behind **both** the `live-tool` feature
*and* `#[ignore]`:

```text
cargo test -p lodestone-v770 --features live-tool \
    --test live_tool_component -- --ignored --nocapture
```

Before this commit the two hermetic `.hex` fixtures
(`tests/fixtures/tool_component_*.hex`) were read by nothing the default test
suite runs — the "hermetic replay" only existed behind that same
feature-gated, ignored live test. They are now exercised by
`crates/protocol/v770/tests/item_components.rs` directly, which is what makes
the live gate a genuine independent check rather than the only place the
fixtures are read at all.

## Gotchas

### 1. `correct_tool` and `requires_correct_tool` are inverses, bare-handed

Confirmed against the code (`crates/protocol/v770/src/tool.rs::bare_handed`,
`crates/protocol/v770/tests/tools.rs::bare_hand_on_stone_is_151_ticks_not_45`,
and the identical warning already on `BlockHardness` in
`crates/lodestone-model/src/adapter.rs`): this is real and matches the
description exactly.

`BlockHardness::requires_correct_tool` is `BlockState.requiresCorrectToolForDrops`
— a property of the **block**. `ToolMining::correct_tool` (and
`BreakInputs::correct_tool` downstream) is `Player.hasCorrectToolForDrops` — a
property of the **held item vs. the block**, and it picks vanilla's `30`
(correct) vs `100` (wrong) speed divider. Bare-handed the two are exact
opposites — an empty hand is "correct" for exactly the blocks that demand no
tool at all:

```rust
correct_tool: !requires_correct_tool
```

Feeding `requires_correct_tool` straight into `correct_tool` compiles, looks
like faithful data wiring, and makes bare-hand stone break in **45 ticks**
instead of the correct **151** (3.4× too fast). `tests/tools.rs` pins both
numbers in the same test so a "cleanup" that re-conflates the two fields fails
immediately: `ticks_to_break(hardness, mining.speed, true)` is asserted to
equal 45 — the wrong answer — right next to the assertion that the real
`correct_tool` gives 151.

`ToolMining::correct_tool` (unlike `BlockHardness::requires_correct_tool`) is
**already folded** — `!requires_correct_tool || correct.unwrap_or(false)` — so
a caller of `tool::mining`/`VersionAdapter::tool_mining` has nothing left to
invert. Re-deriving it from `requires_correct_tool` a second time on top of
`ToolMining::correct_tool` reintroduces the same bug from the other side.

### 2. `LIVE_DIG_HARDNESS` — already gone before this commit

The user's framing describes this as if it might still exist; **it does
not**, and did not by the time `875f452` landed. `LIVE_DIG_HARDNESS` (a fake
constant `0.05` fed to every block, regardless of what it actually was) was
retired earlier, in `15d08e2` ("fix(shell): feed real per-block hardness to
mining, retire `LIVE_DIG_HARDNESS`") — a prior commit that wired the
*hardness* half of break-time (documented in `block-break-timing.md`), before
this commit added the *tool* half. Grepping the tree today
(`crates/`, excluding `target/`) for `LIVE_DIG_HARDNESS` finds only two
historical mentions, both in prose: a doc comment in
`crates/lodestone-shell/src/sim.rs` ("the retired `LIVE_DIG_HARDNESS` (`0.05`
for every block)") and `block-break-timing.md`'s own history section. No
constant by that name is defined anywhere in the current tree.

Its actual historical role, per `block-break-timing.md`: it was a single
fixed dig-progress-per-tick fed for *every* block regardless of hardness, used
before the per-block census existed. It was never a tunable "crack-overlay
cadence" knob in the sense of something meant to be adjusted — it was the bug
being fixed. The general point the user is making — *do not tune a global
constant to fix per-block break timing; wire the real per-block/per-item data
through instead* — is exactly what both `15d08e2` and `875f452` did, and
remains the right lesson even though the specific named constant is gone from
the tree.

## `block_type_name` was naming every block wrong

Confirmed from the `875f452` diff. `crates/protocol/v770/src/block_states.rs::block_type_name`
resolves a `minecraft:block` **registry id** (registration order — the id
space `block_event`, and a tool rule's explicit block set, actually carry) to
a name. Before this commit it indexed `generated_block_states::BLOCK_NAMES`
directly — but that table is built from `blocks.json`, a name-keyed JSON
object, so it is **alphabetically sorted**, not registration-ordered. Registry
id 0 (`minecraft:air`) is alphabetical index 19; registry id 1
(`minecraft:stone`) is alphabetical index 975. Every call silently resolved
to an unrelated block.

**Live blast radius:** `block_event` decoding
(`crates/protocol/v770/src/adapter.rs`, around the `block_type_name(block_id)`
call) — every note block, chest, piston, and end gateway event named the
wrong block. A real note block (registry id 109) decoded as
`minecraft:blue_glazed_terracotta`.

**`block_name(state_id)` (the *state*→name lookup used by chunk palettes and
the mesher) was not affected.** Its index and `BLOCK_NAMES` are built from the
same alphabetical ordering by the same generator, so that path stayed
self-consistent throughout. This was verified, not assumed, in the commit.

**The fix**: `block_type_name` now indexes a new generated,
registration-ordered table, `generated_block_registry::BLOCK_REGISTRY_NAMES`
(`crates/protocol/v770/src/generated/block_registry.rs`, 3,248 new lines),
built from the same `ToolOracle.java` dump's `B` records — the same dump this
whole doc is about, because the tool census needed a correct registry-id→name
map anyway (rules that name blocks explicitly use registry ids).

**Why the pre-existing test didn't catch it**: the old
`block_event_emits_pos_params_and_block_name` test in
`crates/protocol/v770/tests/world_events.rs` encoded **640** — note_block's
*alphabetical* index — and asserted the decoder returned `note_block`. Test
and decoder shared the same wrong assumption (registry id ≈ alphabetical
index), so they round-tripped and cancelled out, while every real server on
the wire sends **109**. The fix changed the test to encode 109 (note_block's
real registry id, fixed externally by `registries.json`) and added a negative
control, `block_event_does_not_read_the_alphabetical_block_index`, which
encodes 640 and asserts it now resolves to `minecraft:acacia_fence` (registry
id 640) — so a regression back to alphabetical indexing fails immediately
instead of silently passing again.

This is the CLAUDE.md-documented pattern of a hand-written test and its
decoder sharing an author and a wrong assumption, becoming invisible until an
external source (here, the registry-order dump) disagrees.

## How to change it

- **Item prototypes and block tags** are version-owned generated data:
  `crates/protocol/v770/src/generated/tools.rs` (`ITEM_TOOLS`, `BLOCK_TAGS`)
  and `crates/protocol/v770/src/generated/block_registry.rs`
  (`STATE_BLOCK`, `BLOCK_REGISTRY_NAMES`). Regenerate via the `LODESTONE_REGEN=1`
  flow above; never hand-edit (`// @generated` headers say so, same convention
  as `hardness.rs`).
- **Evaluation logic** (rule walking, patch resolution, the bare-hand
  inversion) is `crates/protocol/v770/src/tool.rs`, entered through
  `pub fn mining` and exposed version-free via
  `VersionAdapter::tool_mining` (`crates/lodestone-model/src/adapter.rs`,
  implemented in `crates/protocol/v770/src/adapter.rs`).
- **Known gap — datapack-retagged blocks**: block tags are synced by the
  `update_tags` packet, which this build does not decode. `block_tag_members`
  (`crates/protocol/v770/src/tool.rs`) therefore always answers from the
  vanilla census; a datapack that moves a block between `mineable/*` tags
  mines at the vanilla rate on this client. When `update_tags` is decoded,
  override at `block_tag_members` — it is the single lookup every rule match
  goes through.
- **Not yet wired into the shell.** `crates/lodestone-shell/src/sim.rs`'s
  `bare_hand_break_inputs` does not call `tool_mining` and does not read the
  player's selected hotbar slot / held item for mining purposes at all. Wiring
  this is the natural next step: resolve the held `ItemStack` for
  `selected_slot()`, call `adapter.tool_mining(held, state_id)`, and feed
  `speed`/`correct_tool`/`damage_per_block` into `BreakInputs` instead of the
  bare-hand constants. Until that lands, a diamond pickaxe in the running
  client still mines at bare-hand speed — the data and evaluation exist and
  are tested, but nothing in the shell calls them yet.

## Configuration

| knob | where | effect |
| ---- | ----- | ------ |
| `--protocol <n>` | `Config::protocol` | which version family's tool/tag/registry census is resolved |
| `live` feature | `lodestone-shell/Cargo.toml` | compiles a version family into the registry at all |
| `live-tool` feature | `crates/protocol/v770/Cargo.toml` | enables the live `minecraft:tool` wire-decode gate (`live_tool_component`), joins a real server |
| `LODESTONE_REGEN=1` | env var, `cargo test -p lodestone-v770 --test tools ... -- --ignored` | regenerates the committed generated tables from `tests/support/tool_jvm.txt` instead of asserting against them |

## Dependencies

- `lodestone_model::{ItemStack, ItemComponents, ToolPatch, ItemTool, ToolRule, ToolBlocks, ToolMining}` —
  the version-free item/tool model (`crates/lodestone-model/src/item.rs`,
  `crates/lodestone-model/src/adapter.rs`).
- `lodestone_model::VersionAdapter::{block_hardness, tool_mining}` — the only
  route a version-free consumer has to either census; a direct dependency on
  `lodestone-v770` from outside the protocol layer would mint a second,
  divergent data seam.
- `crates/protocol/v770/src/hardness.rs` — the pre-existing per-state
  hardness census this module reuses for `requires_correct_tool`.
- `crates/protocol/v770/src/block_states.rs` — `block_name`/`block_type_name`
  and the state↔block-name relationship the registry table cross-checks.
- `lodestone-game::mining` (`BreakInputs`, `Mining`) — the eventual consumer
  of `ToolMining`'s fields, once `sim.rs` is wired (see "Not yet wired into
  the shell" above). Not a dependency of `lodestone-v770` itself — the break
  formula is restated in `tests/tools.rs` rather than imported, since
  `lodestone-v770` cannot depend on `lodestone-game`.

## Tests

Hermetic, `cargo check --workspace --all-targets` / `cargo test -p lodestone-v770 --no-fail-fast`:
`crates/protocol/v770/tests/tools.rs` (rule evaluation, the 151/45 pin, the
five reference-value ticks table above, block-registry-order cross-checks)
and the updated `crates/protocol/v770/tests/world_events.rs`
(`block_event_emits_pos_params_and_block_name` plus the new negative control
`block_event_does_not_read_the_alphabetical_block_index`).

Drift guard (`#[ignore]`d, no external artifact needed beyond the committed
dump): `committed_tables_match_dump` in `tests/tools.rs`, same pattern as
`hardness.rs` and `collision_shapes.rs`.

Live (`--features live-tool`, `#[ignore]`d, needs a real server):
`crates/protocol/v770/tests/live_tool_component.rs`.
