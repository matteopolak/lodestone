# Damage types and tags

## What it is

The authoritative `minecraft:damage_type` registry for Minecraft 26.2 — 51 damage
types and their 35 tags — generated from vanilla's own datapack JSON and consumed
by `lodestone-entity`'s `DamageFlags`, so combat, fall, fire and loot code reads
one table instead of hand-deriving "does this bypass armour?" at each call site.
Closes issue [#263](https://github.com/matteopolak/lodestone/issues/263).

## How it works

Three pieces, in dependency order:

| file | role |
|---|---|
| `crates/lodestone-data/tests/support/damage_types_jar.txt` | the anchor: vanilla's datapack JSON, verbatim |
| `crates/lodestone-data/src/generated/damage_types.rs` | the generated table (parallel arrays + a `u64` tag mask per type) |
| `crates/lodestone-data/src/damage_types.rs` | `DamageType` / `DamageTypeTag` and the accessors |
| `crates/lodestone-entity/src/damage.rs` | `DamageFlags::for_damage_type` — the consumer seam |

`DamageType` is an index into the table, ordered alphabetically. Behaviour keys
off **tags**, not off the type, so the query that matters is:

```rust
use lodestone_data::damage_types::{DamageType, DamageTypeTag};

let fall = DamageType::from_name("minecraft:fall").expect("real type");
assert!(fall.is_in(DamageTypeTag::BypassesArmor));
assert_eq!(fall.message_id(), "fall");
assert_eq!(fall.exhaustion(), 0.0);
```

and in the damage pipeline:

```rust
let flags = lodestone_entity::DamageFlags::for_damage_type(fall);
let outcome = lodestone_entity::apply_reductions(raw, &defenses, flags);
```

`for_damage_type` maps five tags onto the five pipeline stages, one-for-one with
vanilla:

| `DamageFlags` field | tag | vanilla check |
|---|---|---|
| `bypasses_armor` | `bypasses_armor` | `LivingEntity.java:1903` |
| `bypasses_effects` | `bypasses_effects` | `LivingEntity.java:1912` |
| `bypasses_resistance` | `bypasses_resistance` | `LivingEntity.java:1916` |
| `bypasses_enchantments` | `bypasses_enchantments` | `LivingEntity.java:1936` |
| `bypasses_cooldown` | `bypasses_cooldown` | `LivingEntity.java:1217` |

### Provenance: data files, not a JVM oracle

This is the one table in `lodestone-data` that needs **no** headless server boot.
Hardness, collision shapes and entity dimensions have no datapack
representation, so they need `oracle-java/*.java` walking a registry. Damage
types *are* data files, so reading them is strictly more direct — there is no
program's interpretation in between. No JVM, no Docker, no `container`.

## How to change it, and the gotchas

After a version bump:

```bash
just regen-damage-types
```

That re-extracts the dump (`scripts/extract-damage-types.py`) and regenerates the
table. Five things will bite you:

1. **The outer `server.jar` is a bundler.** `.cache/mc/26.2/server.jar` contains
   *none* of these paths — `unzip -l | grep damage_type` returns **zero** hits
   against it. The real jar is `.cache/mc/26.2/versions/26.2/server-26.2.jar`.
   Searching the wrong one looks exactly like "this version dropped the data".

2. **Tag membership is a transitive closure.** Seven of the 34 tag files
   reference other tags (`"#minecraft:is_explosion"`). `bypasses_shield` lists 12
   entries and resolves to **30**, because one entry is
   `#minecraft:bypasses_armor` (19 members). A flat reader is wrong for exactly
   those seven tags and right for the other 27 — the worst failure shape, since
   most spot checks pass. The closure is resolved once at generation time, so
   `is_in` is a single bit test.

3. **`bypasses_cooldown` is a real tag with no data file.** Declared at
   `DamageTypeTags.java:12`, gates the whole i-frame window at
   `LivingEntity.java:1217`, and **nothing opts into it** in 26.2. The table
   carries it as an empty tag (hence 35 tags from 34 files), and
   `bypasses_cooldown_is_a_real_tag_with_no_members` fails if a future version
   ships the file — so the emptiness is asserted, not assumed. A caller that
   needs to skip i-frames (fall damage does) must set the flag explicitly and say
   why; it will never come from tag data.

4. **`minecraft:generic` is itself `bypasses_armor`-tagged.** It reduces nothing
   by design, so it is the wrong type for testing armour — a fully-armoured
   subject takes full damage and the armour maths looks broken. This cost a
   mid-oracle debugging session before the table existed. Use
   `minecraft:mob_attack`, which is reducible. Pinned by
   `generic_bypasses_armor_but_mob_attack_does_not`.

5. **`message_id` is not the type name.** `mob_attack` → `"mob"`,
   `bad_respawn_point` → `"badRespawnPoint"`, `campfire` → `"inFire"`,
   `ender_pearl` → `"fall"`, `spit` → `"mob"`. Death-message code must read the
   field, never the type name.

**Adding a tag** also needs a `DamageTypeTag` variant in the right alphabetical
slot: the discriminant *is* the bit index, so a variant in the wrong place would
shift every membership bit. `every_tag_membership_matches_the_closure_of_the_committed_dump`
asserts the enum's names against the generated table in order, so a mistake here
fails loudly instead of silently mis-tagging.

**These indices are not network ids.** `minecraft:damage_type` is absent from
`registries.json` because it is purely data-driven: it has no default protocol
id, and its network id is assigned per connection by registry-sync order. Never
put a `DamageType` index on the wire.

## Gates

| test | what it pins |
|---|---|
| `every_field_of_every_type_matches_the_committed_dump` | all 51 types, every field, against the jar dump |
| `every_tag_membership_matches_the_closure_of_the_committed_dump` | all 51 × 35 = 1785 membership pairs |
| `tag_closure_resolves_references_rather_than_reading_flat` | 30 resolved vs 11 flat, prediction derived from the dump |
| `a_flat_non_resolving_reader_fails_the_closure_assertion` | permanent negative control for the closure |
| `committed_table_matches_dump` (`#[ignore]`d) | source-text drift vs the generator |
| `armour_reduction_lands_on_the_real_tag_data_for_both_types` | 10.0 (`generic`) vs 3.0 (`mob_attack`) — magnitude, not sign |
| `a_broken_bypasses_armor_lookup_would_be_caught` | permanent mutation control on the flag derivation |
| `fall_flags_come_from_the_damage_type_table_and_are_load_bearing` | the `vitals.rs` consumer is wired to the table |

Observed controls (run, not described): flipping **one bit** in `generic`'s tag
mask fails the membership gate *and* the drift gate; forcing `bypasses_armor` to
`false` in `for_damage_type` fails **5** tests in `lodestone-entity` and **2** in
`lodestone-server`, with the headline assertion reporting `expected the full
10.0, got 3`.

One honest limit worth knowing: `PlayerVitals::apply_fall_damage` passes
`Defenses::default()`, i.e. zero armour, so at *that function's* output
`bypasses_armor: true` and `false` are indistinguishable — the pre-existing
`fall_damage_reaches_health_unreduced_with_no_armour_tracked` passes under the
mutation. The flag is correct and currently **inert** there; it becomes
observable when a player equipment model lands (issue #261). The gate is
therefore on the derivation plus its composition with armour, not on that
function's return value.

## Configuration

None. No env vars or features; the table is compiled in. `LODESTONE_REGEN=1`
switches the drift test from assert to regenerate, as with every other generated
table here.

## Dependencies

* `lodestone-data` → `lodestone-model` only (the table adds no new dependency).
* `lodestone-entity` gains `lodestone-data`. No cycle: `lodestone-data` does not
  depend on `lodestone-entity`. Both are version-free, so the version seam
  (`cargo check -p lodestone-shell --no-default-features`) is unaffected.
* `lodestone-server` already depended on `lodestone-data`; the `vitals.rs`
  consumer needed no manifest change.
* Regeneration needs `python3` and the extracted jar under `.cache/mc/26.2/`.
