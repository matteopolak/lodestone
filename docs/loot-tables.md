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
| `match_tool` / `entity_properties` / `block_state_property` | `false` (no tool/entity/state) | the respective conditions |
| `table_bonus` | `chances[0]` (fortune level 0) | `BonusLevelTableCondition.java` |
| `set_count` | rolled | `SetItemCountFunction` |
| `enchanted_count_increase` | no-op (level 0) | `…Function.java` |
| `apply_bonus` | no-op (no tool) | `ApplyBonusCount.java` |
| `explosion_decay` | no-op (no explosion) | `ApplyExplosionDecay.java` |
| `furnace_smelt` | smelted via `crate::furnace::recipe_for` | `SmeltItemFunction.java` |

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
