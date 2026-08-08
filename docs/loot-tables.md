# Server loot tables: loading and rolling

Issue [#337](https://github.com/matteopolak/lodestone/issues/337), part of the
server-plumbing epic [#339](https://github.com/matteopolak/lodestone/issues/339).

## What it is

The version-free server-side loot system in `crates/lodestone-server/src/loot.rs`:
it parses Mojang's datapack loot-table JSON (the same format
`net.minecraft.world.level.storage.loot` reads) and **rolls** a table with the
server's deterministic RNG to produce `Vec<ItemStack>` — the data that becomes a
mob drop, a block drop, or a chest fill. This is the server half of the client's
`lodestone-game` recipe loader: both consume vanilla datapack JSON behind a
version seam, neither names a protocol.

The entry point is [`roll_loot`] — `roll_loot(set, table_id, rng) -> Vec<ItemStack>`
— the "empty loot context" starting point #337 asks for: no entity, no level, no
tool, no block state, no explosion, `luck = 0`. It is what a future
mob-death handler or chest filler calls after mapping its entity/block to a
table id.

## How it works

A table is `pools` (each a weighted `entries` list, a `rolls` count, optional
`conditions`/`functions`); an entry is a weighted leaf (`item`, `empty`,
`loot_table`) or a composite (`alternatives`, `group`, `sequence`). Rolling walks
the structure exactly as `LootPool.addRandomItems` does:

1. A pool whose conditions all pass emits `rolls + floor(bonus_rolls · luck)`
   rolls. A roll expands the entry tree into weighted leaves (an `alternatives`
   stops at the first child that expands; a `group`/`sequence` expands every
   child), sums the luck-adjusted weights (`max(floor(weight + quality·luck), 0)`),
   draws `nextInt(totalWeight)`, and emits the leaf it lands on.
2. A selected leaf applies entry functions, then the pool's functions, then the
   table's functions — the same order vanilla composes `LootItemFunction`s in.
3. Nested `minecraft:loot_table` entries resolve through the [`LootTableResolver`]
   supplied to [`LootTable::roll_with`], with a visited-set recursion guard
   against cycles.

### The empty loot context

Every condition/function this module understands has a **defined** empty-context
value, so a table with zero unsupported features rolls correctly:

| feature | empty-context value | vanilla source |
|---|---|---|
| `random_chance` | `nextFloat() < chance` | `LootItemRandomChanceCondition` |
| `random_chance_with_enchanted_bonus` | uses `unenchanted_chance` (level 0) | `…Condition.java` |
| `killed_by_player` | `false` (no killer param) | `…Condition.java` |
| `survives_explosion` | `true` (no explosion radius) | `ExplosionCondition.java` |
| `entity_properties` / `block_state_property` | `false` (no entity/state) | the respective conditions |
| `match_tool` | `false` (no tool) | `MatchTool.java` |
| `table_bonus` | `chances[0]` (fortune level 0) | `BonusLevelTableCondition.java` |
| `set_count` | rolled | `SetItemCountFunction` |
| `enchanted_count_increase` | no-op (level 0) | `…Function.java` |
| `apply_bonus` | no-op (no tool) | `ApplyBonusCount.java` |
| `explosion_decay` | no-op (no explosion) | `ApplyExplosionDecay.java` |
| `furnace_smelt` | smelted via `crate::furnace::recipe_for` | `SmeltItemFunction.java` |

### The tool (issue #539)

`LootContext::tool: Option<LootTool>` is `LootContextParams.TOOL`. `None`
reproduces the empty context exactly; `Some` is what makes Silk Touch, Fortune and
`match_tool` evaluate at all. `LootTool` carries the item key, the stack count, and
enchantment levels **by key**.

**A present tool changes the RNG stream even at enchantment level 0.** This is the
single easiest thing here to get wrong, and it is invisible to any distributional
test. `ApplyBonusCount.run` guards on `tool != null`, *not* on `level > 0`:

| formula | level 0, no tool | level 0, unenchanted tool | level `L` |
|---|---|---|---|
| `ore_drops` | no draw | **no draw** (`if (level > 0)` inside) | 1 draw; `count × max(nextInt(L+2), 1)` |
| `uniform_bonus_count` | no draw | **1 draw** (`nextInt(M·0 + 1)`) | 1 draw; `count + nextInt(M·L + 1)` |
| `binomial_with_bonus_count` | no draw | **`extra` draws** | `L + extra` draws |

The commonly-quoted restatement of `ore_drops` as `count * max(1, nextInt(level +
2))` — including in issue #539's own body — is arithmetically right and
draw-count **wrong**: it draws at level 0. Gated by
`an_unenchanted_tool_costs_ore_drops_no_rng_draw_but_fortune_does`, which is the
only assertion in the suite that catches it; the control observed
`left: Vec3 { x: 0.382…} , right: Vec3 { x: 0.450… }` while every distribution
assertion still passed.

`table_bonus` always draws, tool or not — only *which* chance it compares changes
(`chances[min(level, len - 1)]`, so a level above the list clamps rather than
panicking).

#### Why enchantments are keyed by name, and what is not yet live

`lodestone_model::ItemEnchantment` carries a **network registry id**, because
`minecraft:enchantment` is a datapack registry whose ids are assigned per session
at configuration time. It is **not in Mojang's `registries.json`**, so there is no
static name↔id table to generate, and this crate is version-free and could not
consult one anyway. So `LootTool` takes keys and the *producer* resolves.

Consequences, stated rather than hidden:

- **Live today**: the correct-tool gate, and every `match_tool` predicate over
  `items` (47 of the corpus's 203 `match_tool` conditions) — both need only the
  held item's key.
- **Not yet reachable from a real client**: enchantment *levels*.
  `LootTool::from_held_item` builds an unenchanted tool because no enchanted stack
  can reach this server — `V770ServerProtocol`'s `read_hashed_stack` rejects any
  stack whose `DataComponentPatch` is non-empty, and the server sends no
  enchantment registry. Both are separate work. The logic is complete and gated;
  what is missing is a producer, so treat this as a known island half and not as
  "it works".

A feature this module does not recognise is **parsed but marked unsupported**
([`LootTable::unsupported_features`]) rather than aborting the load — the same
tolerance `recipe_json` shows — and contributes nothing to a roll: an
unsupported condition fails, an unsupported function/entry/provider is a no-op.
The six tables bundled under `assets/loot_table/` are curated to have **zero**
unsupported features, so `LootTableSet::load_bundled` rolls exactly the vanilla
loot those JSON files define.

## How to change it

- **Add a condition/function/number-provider**: add the variant to the enum,
  its parse arm in the matching `from_value`, its empty-context semantics in
  `test`/`apply`/`int`/`float`, and a test.
- **Bundle another table**: drop the verbatim JSON under
  `assets/loot_table/` (ids are `minecraft:` + path-minus-extension); `build.rs`
  re-embeds it. Keep the "zero unsupported" invariant — `load_bundled` asserts it
  in debug builds.
- **Run the corpus gate** (`#[ignore]`d, needs `.cache/mc/26.2/client-src`):
  `cargo test -p lodestone-server --test loot_corpus -- --ignored --nocapture`.
  It proves every bundled table is byte-identical to Mojang's data (modulo the
  trailing newline) and that all 1355 vanilla tables parse without a hard error.

### Gotchas

- **The RNG is `SpawnRng`, not JVM-compatible.** Rolling with a fixed seed is
  deterministic and each draw's *distribution* matches Java (`nextFloat`,
  `nextDouble`, uniform `nextInt` ranges), but the exact stream differs
  (SplitMix64 vs Xoroshiro). Byte-exact JVM stream parity — #337's verification
  section — is a follow-up built on this seam, not part of it.
- **`set_count` can produce a count-0 stack**, and the roller keeps it, exactly
  as vanilla's `createStackSplitter` passes such a stack through (zombie
  rotten-flesh is `uniform 0..2`). A container `fill` drops zero stacks; a
  `getRandomItems`-style consumer sees them. `roll_loot` filters nothing.
- **`minecraft:tag` entries are unsupported**: expanding an item tag needs an
  item-tag census, which `lodestone-data` does not bundle. Only one table in the
  26.2 corpus uses one.
- **An unmodelled `match_tool` predicate must fail *closed*.** Reporting it as
  unsupported (so `load_bundled` refuses the table) and evaluating it as `false` is
  the safe pair; making it match-everything would silently turn every silk-touch
  branch on. The three shapes that fail closed today are `items: "#tag"`, a
  `components` exact matcher, and any `predicates` key other than
  `minecraft:enchantments` — none of which occur in the corpus except the one
  `#tag`.
- **`match_tool` with no `predicate` field at all is `true` for any tool**, not
  `false`. It is the one place where the absent-optional default is permissive, and
  the opposite of what the fail-closed rule above would give.

## Configuration

Nothing to configure: the bundled tables live in `assets/loot_table/` and are
embedded by `crates/lodestone-server/build.rs` into `$OUT_DIR/embedded_loot.rs`
(the same mechanism `assets/worldgen/` uses). `cargo::rerun-if-changed` on
`assets/loot_table` rebuilds on a data change.

## Dependencies

- `serde_json` and `lodestone-model` (already server-crate dependencies) — no new
  crates.
- [`SpawnRng`] — the server's deterministic RNG (its `next_f32` was added for
  loot).
- [`crate::furnace::recipe_for`] — `furnace_smelt` looks the output item up here.
