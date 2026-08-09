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
| `entity_properties` / `damage_source_properties` / `location_check` | `false` (no entity/source/level) | the respective conditions |
| `block_state_property` | `false` (no state); **fully evaluated once a state is present** | `LootItemBlockStatePropertyCondition` |
| `match_tool` | `false` (no tool); **fully evaluated once a tool is present** | `MatchTool` |
| `table_bonus` | `chances[0]` (fortune level 0) | `BonusLevelTableCondition.java` |
| `set_count` | rolled | `SetItemCountFunction` |
| `enchanted_count_increase` | no-op (level 0) | `…Function.java` |
| `apply_bonus` | no-op (no tool) | `ApplyBonusCount.java` |
| `explosion_decay` | no-op (no explosion) | `ApplyExplosionDecay.java` |
| `furnace_smelt` | smelted via `crate::furnace::recipe_for` | `SmeltItemFunction.java` |

### The block state, and why 154 tables were silently wrong

`LootContext::block_state: Option<LootBlockState>` is
`LootContextParams.BLOCK_STATE`. Until it existed, `block_state_property` **parsed
into a recognised variant and evaluated as a hardcoded `false`**, so every
state-conditioned table took the wrong branch on every roll. The reported symptom
was wheat: breaking a fully-grown stalk dropped **one seed and no wheat**, at every
age. Reading `blocks/wheat.json` explains it exactly — pool 1 is an `alternatives`
whose `minecraft:wheat` child is gated on `age: "7"` and whose fallback is
`wheat_seeds`, and pool 2's bonus seeds carry the same condition at *pool* level.
Both took the false branch.

**154 of the 1,241 bundled tables carry the condition, 258 times between them.**
Four shapes, and only the first was guessed correctly before the tables were read:

| shape | tables | what the false branch produced |
|---|---|---|
| `alternatives` child (`wheat`, `beetroots`) | crops | the seed, never the crop |
| extra pool (`carrots`, `potatoes`) | crops | one carrot, never the second |
| `set_count` condition (all 68 slabs, all 17 candles, `snow`, `sea_pickle`, `glow_lichen`) | counts | always 1 — **a double slab dropped one** |
| whole-pool or whole-entry gate (`*_door`, `*_bed`, `cave_vines`, `sweet_berry_bush`) | drops | **nothing at all** |

That last row is the one worth remembering: a `part=head` gate on every bed and a
`half=lower` gate on every door meant **beds and doors dropped no item whatsoever**.

#### What resolving the state actually requires

`StatePropertiesPredicate.PropertyMatcher.match` looks the property up in the
block's `StateDefinition` and reads the *state's* value, so **every property the
block has contributes a value whether or not the caller named it**, and a matcher
over a property the block has not got is `false` rather than skipped. So the loot
context cannot take the substring between the brackets:
`block_drops::loot_block_state` resolves through
`lodestone_data::block_states::state_id` (Mojang's own 32,366-state table, with its
default-plus-overrides tiers) and reads the canonical `(name, value)` list back out
of `properties`. `"minecraft:wheat"` therefore arrives as `age=0` — a value, not an
absence — which is why it fails an `age=7` matcher for the right reason.

`StatePropertiesPredicate` also has a **ranged** matcher (`{"min": …, "max": …}`,
both strings) whose comparison is the *property's* ordering, so integers compare
numerically: a lexicographic compare puts `"9" > "10"` and would invert the answer.
No bundled table uses the ranged form — all 258 conditions are exact strings — so
that path is implemented from the record and gated synthetically against
`minecraft:composter`'s `level` with a bound of `"10"`, the input where the two
readings disagree.

#### The curation gate could not have caught this, and now something can

`bundled_tables_are_all_fully_supported_and_roll` asserts
`LootTable::unsupported_features()` is empty. That list answers **"did the parser
recognise this"**, and a variant that parses its JSON and then ignores it is
invisible to it — which is exactly how a bundle whose stated invariant is "zero
unsupported features" shipped 154 tables that were always wrong.

`LootTable::context_blind_features()` is the missing instrument: it reports
conditions that are **recognised but not evaluated**, meaning they parse data they
then discard. It is deliberately *separate* from `unsupported_features` rather than
folded into it — promoting these to unsupported would eject 30 tables from the
bundle and change nothing a player sees, while breaking the byte-identical-to-Mojang
property the corpus gate rests on. The value is in the count, which is asserted
exactly:

| condition | bundled tables |
|---|---|
| `minecraft:entity_properties` | 25 |
| `minecraft:damage_source_properties` | 4 |
| `minecraft:location_check` | 3 |

30 tables in total. Non-zero is correct — each names a context parameter this crate
does not carry — and making one of them evaluable is now a number that has to move.
`snow` (`entity_properties`), `tall_grass` and `large_fern` (`location_check`) are
the tables still wrong for this reason after the block-state landing.

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
Every table bundled under `assets/loot_table/` has zero unsupported features
**except** the four decoration-only functions listed below, so
`LootTableSet::load_bundled` rolls the right item and the right count for every
bundled table.

### The bundle is the clean subset of the whole corpus (issue #538)

`assets/loot_table/` used to be six hand-picked tables, so almost every block in
the game dropped nothing. It is now **1,241 of Mojang's 1,355** 26.2 loot tables,
copied verbatim (**885 KB**), across block tables plus chests, entities,
archaeology, shearing, brushing, dispensers, harvest, pots, spawners and gameplay.
For scale, `crates/lodestone-server/assets/` already carried 13.5 MB of worldgen
and structure data.

The subset is not a curation preference: `load_bundled` debug-asserts it, so
"clean" *is* the bundling precondition. The excluded tables are the ones using a
feature the roller does not model — mostly `copy_components`, `set_potion`,
`enchant_with_levels`, `set_damage`, plus one `minecraft:tag` entry and one
`minecraft:dynamic`. Teaching the roller one of those moves tables into the subset;
**regenerate and re-commit rather than adding files by hand** (`just
regen-loot-corpus`), and update `loot.rs`'s own
`bundled_tables_are_all_fully_supported_and_roll` count.

#### The one relaxation: decoration-only functions (issue #337)

`loot::DECORATION_ONLY_UNSUPPORTED` is a four-entry allowlist —
`enchant_randomly`, `exploration_map`, `set_name`, `set_stew_effect` — that a
bundled table *is* allowed to use. Each one only decorates an item the roll already
produced correctly, so the item id and the count are right and the enchantment /
name / map target / stew effect is absent.

**This is deliberately not a blanket relaxation, and the asymmetry is the point.**
An unsupported *condition* **fails**, so a table using one drops items it should
have produced — a silently short chest rather than a cosmetically plain one — and
an unsupported entry or number provider loses the same way. Those still keep a
table out of the bundle. It is the allowlist that let the four structure-chest
tables (`chests/shipwreck_{map,supply}`, `chests/underwater_ruin_{small,big}`) in,
without which a shipwreck's supply and map chests would have had no table to roll
at all.

Regenerate with `just regen-loot-corpus`. Its gate
(`tests/loot_corpus.rs::the_bundle_is_exactly_the_clean_subset_of_the_vanilla_corpus`)
compares three directions, and the middle one is what a bundle-only drift check
structurally cannot do:

1. every bundled table is byte-identical to Mojang's copy;
2. every **clean cache** table is bundled — so a table that newly becomes clean
   fails until it is added;
3. nothing is bundled that is not in the clean subset — so an invented table, or
   one whose features regressed, fails too.

**That gate had never run before #538.** `corpus_root()` joined `../../..` to
`CARGO_MANIFEST_DIR`, which is the repo's *parent*, so both existing tests aborted
on their own `root.is_dir()` precondition. Being `#[ignore]`d, no health check
here could see it, and `just regen-loot-corpus` now exists so there is a named way
to run it.

## How to change it

- **Add a condition/function/number-provider**: add the variant to the enum,
  its parse arm in the matching `from_value`, its empty-context semantics in
  `test`/`apply`/`int`/`float`, and a test.
- **Bundle more tables**: teach the roller the feature they use, then
  **`just regen-loot-corpus`**. Do not add files under `assets/loot_table/` by
  hand — it is generated, and the drift gate compares it against the cache. Update
  `bundled_tables_are_all_fully_supported_and_roll`'s exact count in the same
  commit.
- **Run the corpus gate** (`#[ignore]`d, needs `.cache/mc/26.2/client-src`):
  `cargo test -p lodestone-server --test loot_corpus -- --ignored --nocapture`.
  It proves every bundled table is byte-identical to Mojang's data (modulo the
  trailing newline), that all 1355 vanilla tables parse without a hard error, and
  that the bundle is exactly the clean subset in both directions.

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

Nothing to configure: the bundled tables live in `assets/loot_table/` (1,241
files, 885 KB) and are embedded by `crates/lodestone-server/build.rs` into
`$OUT_DIR/embedded_loot.rs` as 1,241 `include_str!`s (the same mechanism
`assets/worldgen/` uses for 7 MB). `cargo::rerun-if-changed` on
`assets/loot_table` rebuilds on a data change.

`block_drops::bundled_tables()` parses the whole set once per process behind a
`OnceLock`. Measured in release, `load_bundled` **plus** a roll of every one of the
1,241 tables completes inside a test reporting `0.01s`, so nothing here needs a
lazier scheme. `LootTableSet::get` is still a linear scan — 1,241 key comparisons
once per block break, which is not worth an index.

## Dependencies

- `serde_json` and `lodestone-model` (already server-crate dependencies) — no new
  crates.
- [`SpawnRng`] — the server's deterministic RNG (its `next_f32` was added for
  loot).
- [`crate::furnace::recipe_for`] — `furnace_smelt` looks the output item up here.
