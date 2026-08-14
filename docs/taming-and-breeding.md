# Taming and breeding

## What it is

The server-side model for making a wild animal *yours* and for making two of them produce a third:
per-species taming (a wolf, a cat, a parrot and a horse are four different mechanisms, not one with
different constants), the ownership state that survives a reconnect, sitting and following, and the
right-click that puts two adults in love. It lives in `lodestone_server::mobs` plus two new goals in
`lodestone_entity::ai::goals`.

The thing that unblocked all of it is small and structural: **a player identity at the mob
perception seam**. Before it, `SimMob::owner_id` existed with no producer anywhere in the tree — an
island in the "nothing calls this" direction — because the perception seam carried a position and a
held item and nothing else, so "tame this wolf to *me*" was not expressible.

---

## 1. The identity at the seam, and why it is two fields

`PlayerPerception` still means what it always meant: what a mob can **sense** about a player. A mob
senses a position and a held item; it does not sense a uuid. So identity went beside it rather than
inside it:

```rust
pub struct PerceivedPlayer {
    pub identity: Option<PlayerIdentity>,
    pub perception: PlayerPerception,
}

pub struct PlayerIdentity {
    pub uuid: Uuid,     // the identity
    pub entity_id: i32, // the handle
}
```

**Both, and each does a different job.**

*The uuid is what ownership is keyed on.* Vanilla stores a tamed animal's owner in
`TamableAnimal.DATA_OWNERUUID_ID`, whose serializer is
`EntityDataSerializers.OPTIONAL_LIVING_ENTITY_REFERENCE` → `EntityReference.streamCodec()` →
`UUIDUtil.STREAM_CODEC`: sixteen raw bytes. The NBT form is the same uuid through `UUIDUtil.CODEC`.
So the uuid is what both the wire and the save file demand — and it is the only identity that
*survives*, since a runtime entity id is reassigned on every reconnect and this server derives an
offline-mode uuid from the username.

*The entity id is the handle.* The rest of this sim speaks `i32` entity ids —
`SimMob::attack_target_id`, the mob-to-mob `MobOwner::Mob`, every `EntitySnapshot` — so a mob that
wants to exclude its owner from a target, or a snapshot that wants to name the owning entity, cannot
say so in uuids. Vanilla makes exactly this split: `EntityReference` stores the uuid and *caches* the
resolved live entity.

Storing only the entity id makes ownership evaporate on reconnect. Storing only the uuid makes it
unnameable to anything else in the crate. `tests/taming.rs`'s
`a_pets_owner_resolves_by_uuid_and_survives_a_new_entity_id` is the gate: its middle arm reconnects
the same account under a different entity id and requires the pet to still resolve its owner.

### Why `identity` is an `Option`

Because `From<PlayerPerception> for PerceivedPlayer` yields **no** identity, and that is the honest
state for a producer that has not been taught to supply one. `MobSim::set_players` is generic over
`Into<PerceivedPlayer>`, so a bare `Vec<PlayerPerception>` still compiles — every gate that only
cares where a mob looks stays readable without a uuid it does not use.

The trap this avoids is keying ownership on a *nil* uuid, which would make every unidentified player
the owner of every pet tamed by anyone. An unidentified player is perceived normally and owns
nothing; `an_unidentified_player_is_never_resolved_as_an_owner` asserts both halves, the second being
the control that says the feed ran at all.

### Ownership is not tameness

`SimMob` carries `owner: Option<MobOwner>` **and** a separate `tame: bool`, and they are not
redundant. A tamed pet whose owner has logged out keeps its owner (the uuid is durable) and has no
resolvable owner *position*; it is still tame. Deriving tameness from a resolved owner would un-tame
every pet the moment its owner left the player list, and `SitWhenOrderedToGoal`'s `!isTame()` arm and
`Wolf.WolfAvoidEntityGoal`'s `!wolf.isTame()` guard both read it. `MobController` therefore has
`is_tame()` as its own method next to `owner_position()`.

`MobOwner` has two variants for the same reason. A player owner is a uuid; a mob owner is a runtime
entity id, because nothing persists it and there is no uuid to resolve. Collapsing them into one
`i32` is what made ownership unable to name a player in the first place.

---

## 2. The four taming mechanisms

`tame_mechanism(species)` in `mobs/species.rs` (moved there from `mobs.rs`
by the file split; called from `MobSim::interact` in `mobs/mod.rs`). Read the
table before assuming a shared constant:

| species | trigger | roll | also sits? |
|---|---|---|---|
| wolf | **`Items.BONE`**, and only while not angry | `nextInt(3) == 0` | yes |
| cat | `#cat_food` — raw cod or salmon | `nextInt(3) == 0` | yes |
| parrot | `#parrot_food` — six seeds | `nextInt(10) == 0` | **no** |
| horse family | **being ridden**, not fed | `nextInt(getMaxTemper()) < getTemper()` | n/a |

Four things a "chance per species" table gets wrong:

* **The wolf's taming item is in none of its own food tags.** `Wolf.isFood` is `#wolf_food` (meat,
  fish, rabbit stew) and a bone is in neither that nor `#meat`. This is why `breeding_food` and
  `tame_mechanism`'s item sets are separate tables and must stay separate.
* **The parrot's odds differ by a factor of three**, and `the_parrots_odds_are_ten_and_the_wolfs_are_three`
  is built on a seed where the two bounds *disagree* — first `next_int(3)` is `0`, first
  `next_int(10)` is not — so a shared constant cannot pass it.
* **`Parrot.tryToTame` omits `setOrderedToSit(true)`.** It is the only one of the three that does. A
  parrot that sits itself down on being tamed is visibly wrong and no "it tamed" assertion sees it.
* **The horse's roll is not a chance at all**, it is a function of a persisted counter. See below.

A fifth, which fell out of writing the gates rather than reading the jar: **whether a species has to
be tamed before it can be bred depends on whether its taming item overlaps its food tag.** An
untamed wolf fed meat misses the bone arm, reaches `super.mobInteract`, and really does fall in love.
An untamed cat fed cod always attempts a *tame* instead and never reaches the love arm at all,
however the roll lands. The first draft of `breeding_items_are_per_species_and_a_parrot_has_none`
asserted otherwise and failed; the code was right.

### The horse family

Feeding raises `Temper` (`AbstractHorse.handleEating`) and never rolls. `horse_temper_gain` is a
table, not a derivation from `#horse_food`, because the two disagree in **both** directions:

* `hay_block` **is** horse food and grants **no temper** — `heal = 20.0F`, `ageUp = 180`, `temper`
  left at its `0` initialiser. Deriving temper from the tag lets a stack of bales tame a horse.
* `red_mushroom` grants **3 temper and is not in `#horse_food`**, so `isFood` is false for it while
  `handleEating` still accepts it. Deriving the accepted set from `isFood` drops it.

The roll is `MobSim::attempt_horse_tame`, and it is certain to fail at temper 0 and certain to
succeed at temper 100 — two predictions that need no seed, which is what
`the_horses_tame_roll_is_a_function_of_temper_and_failure_raises_it` asserts. Each failure adds 5
temper (`modifyTemper(5)`), which is the whole reason a horse eventually yields.

**One disclosed deviation.** Vanilla reaches that roll from `RunAroundLikeCrazyGoal`, which ticks
while a player is a *passenger*, behind its own `nextInt(adjustedTickDelay(50)) == 0` gate — so a
rider gets roughly one attempt every 25 ticks. This server has no passenger model at all, so the
attempt is made **once per mount attempt** (one empty-handed right-click) and the 1-in-50 outer gate
is not drawn. The arithmetic that makes the horse a different mechanism is unchanged: the roll,
the certainty at both ends, and the 5-temper penalty are vanilla's. Only the *pacing* differs.

### Horse breeding is also its own thing

`AbstractHorse.handleEating` calls `setInLove` in exactly two arms — `GOLDEN_CARROT` and
`GOLDEN_APPLE`/`ENCHANTED_GOLDEN_APPLE` — each gated on `isTamed() && getAge() == 0 && !isInLove()`.
Wheat, sugar, apples, carrots and hay all feed a horse and none of them breeds it, so the horse's row
in `breeding_food` is **empty** and `horse_breeding_items` is a separate predicate.

---

## 3. The dispatch order is part of the specification

`MobSim::interact` is one method with three species arms (`interact_tamable`, `interact_horse`,
`interact_animal`), and each transcribes a `mobInteract` override's `if` chain **in order**. That
order is observable, not stylistic: feeding a hurt tame wolf meat must **heal** it (`Wolf.mobInteract`'s
first arm, `isFood(stack) && getHealth() < getMaxHealth()`) and only once it is at full health does
the same item put it in love (reached through `super.mobInteract` →
`Animal.mobInteract`). A port that reordered them looks correct in any test that feeds a healthy
wolf; `feeding_a_hurt_pet_heals_it_and_feeding_a_healthy_one_breeds_it` is the one that separates them,
and it predicts the wolf's `2.0` heal rather than asserting that health went up (a cat's is `1.0`).

The sit toggle is the **last** arm — vanilla's
`if (!interactionResult.consumesAction() && isOwnedBy(player))` — so anything above it suppresses
the toggle, which is why an owner feeding a hurt pet does not also sit it down.

`InteractOutcome` is richer than a `bool` because the caller does different things with each: a tame
attempt consumes the item whether it succeeded or not, and a sit toggle consumes nothing
(`InteractionResult.SUCCESS.withoutItem()`). `consumes_item()` encodes that.

### The love gate is `age == 0`, not `!is_baby()`

`Animal.mobInteract` requires `getAge() == 0 && canFallInLove()`. The two readings differ on exactly
one input — a parent inside its post-breeding cooldown, whose age is a positive countdown: it is not
a baby and it still cannot fall in love. `!is_baby()` there lets a pair breed every 60 ticks forever.
`a_cooling_down_parent_and_a_baby_both_refuse_the_breeding_item` uses that discriminating input.

---

## 4. Sitting and following

Two new goals in `lodestone_entity::ai::goals`, upgraded from `Registration::missing` to real rows in
the wolf's roster table (`ai/roster/neutral.rs`, goal priorities 2 and 6). Both were `Missing`
specifically because no owner could be a player.

`SitWhenOrderedToGoal` has a five-clause `canUse` and its doc comment names all five with what
answers each; two are **not implemented** (`onGround()`, and the
`dist² < 144 && owner.getLastHurtByMob() != null` suppression) because no ground state or owner-hurt
state crosses the seam. Both omissions are disclosed there with their behavioural cost. The one
non-obvious clause is that the `owner == null` branch returns **`true`**, not `orderedToSit`: having
already passed `orderedToSit || isTame()`, a tame pet with no resolvable owner sits down even though
nobody told it to. That is the "pets settle when you log out" behaviour, and reading the summary
instead of the record produces `is_ordered_to_sit()` in both arms and silently drops it.

`FollowOwnerGoal` takes `(speed, startDistance, stopDistance)` as **arguments**, because vanilla's
are not uniform: wolf `(1.0, 10, 2)`, cat `(1.0, 10, 5)` — a cat stops five blocks out — parrot
`(1.0, 5, 1)`. One shared set would be wrong for two of the three. The teleport
(`tryToTeleportToOwner` past `distanceToSqr >= 144`) is **not** implemented: it lands on
`canTeleportTo`, which needs a `PathType.WALKABLE` probe plus a `noCollision` box test at an
arbitrary candidate cell, and this seam answers block questions only about the mob's own feet cell.
`unableToMoveToOwner`'s `isOrderedToSit()` conjunct *is* implemented and is the load-bearing one —
without it a sitting pet gets dragged along and the two goals fight over the MOVE flag every tick.

Note the sitting **order** and the sitting **pose** are two pieces of state, as in vanilla:
`orderedToSit` is the persisted intent an owner's click toggles and NBT round-trips as `Sitting`,
while `setInSittingPose` is the synced `0x01` flag bit `SitWhenOrderedToGoal::start`/`stop` writes.
Collapsing them means a sitting order evaporates whenever the goal is preempted by a higher-priority
flag holder. `SimMob::set_ordered_to_sit` pushes straight through to the `NavigatingMob` rather than
waiting for the next `feed_perception`, because an order given between ticks would otherwise arrive
one tick late and read as a pet ignoring the first click.

---

## 5. Breeding, the cooldown, and the orb

The breeding *machinery* already existed and works end to end: `BreedGoal`, the partner search in
`MobSim::feed_perception`, `NavigatingMob::breed`'s event, and `MobSim::resolve_breeding`'s child
spawn plus `PARENT_AGE_AFTER_BREEDING` on both parents. What was missing was the **trigger** —
nothing put a mob in love, because nothing fed one. That is now `MobSim::interact`'s love arm.

Two additions to `resolve_breeding`:

* **The experience orb.** `Animal.finalizeSpawnChildFromBreeding` ends with
  `if (gameRules.get(MOB_DROPS)) addFreshEntity(new ExperienceOrb(…, random.nextInt(7) + 1))`. It is
  **constructed, not awarded**: `ExperienceOrb.award` splits an amount into denominations and tries
  `tryMergeToExisting` first, so routing this through `award_experience` would let a second mating in
  the same spot fold silently into the first orb. `spawn_orb` is the right call. The `mob_drops` rule
  gates it, exactly as it gates a mob's death reward.
* Nothing else — the child spawn and the double cooldown were already correct.

---

## 6. Configuration

| knob | what it does |
|---|---|
| `MobSim::set_tame_rng(SpawnRng)` | replaces the tame-roll stream. The injection point a chance gate needs: seed it so the first draw is known and the outcome becomes a prediction. |
| `TAME_ROLL_SEED` / `BREED_XP_SEED` (`mobs/mod.rs`) | default seeds. Separate streams from `ORB_BEHAVIOR_SEED` and the spawn RNG, so a tame attempt cannot shift which roll a spawn or a despawn pass sees. |
| `SimMob::set_temper` | stages a horse at a chosen temper instead of feeding it 34 times. |
| `mob_drops` game rule | gates the breeding orb. |

---

## 7. What reaches the screen, and what does not

**The right-click reaches this crate now.** `ServerBound::InteractEntity` exists, `v770`'s
`INTERACT` arm decodes into it, and `dispatch_play_packet` calls `MobSim::interact` with the
player's `PlayerIdentity` — so a real client can tame a wolf from the game. Before that,
`minecraft:interact` decoded to `ServerBound::Ignored` and every mechanism here was driven
only from its own gates. Off-hand interactions are dropped rather than duplicated (a vanilla
client sends the main hand first, and running both would roll the tame chance twice), and a
consumed item goes through `consume_one` plus an `encode_container_slot` for the hotbar slot —
the slot update is not optional, because without it the client's count desyncs.

**Particles do, today — through a substitution that is now replaceable.** `MobSim::interact`
pushes a `WorldEffect::Particles` burst onto `pending_vocalisations`, which the tick loop
drains through `take_vocalisations` into real `LEVEL_PARTICLES` packets. Vanilla's mechanism is
`broadcastEntityEvent(this, (byte)6|7|18)` — the *client* expands the byte into a burst
(`TamableAnimal.spawnTamingParticles`, `Animal.handleEntityEvent`) — and this server had no
`ENTITY_EVENT` encoder at all when that was written, so the burst is published directly with
the same particle type, count and spread. A disclosed substitution rather than an
approximation of the visual: seven HEART on a success or a love, seven SMOKE on a failure,
half a block above the mob's feet.
`taming_publishes_hearts_on_success_and_smoke_on_failure` includes a third arm where a sit toggle must
add **nothing**, so a single hardcoded particle cannot pass.

**That encoder now exists**: `ServerProtocol::encode_entity_event`, with the constants named in
`lodestone_server::entity_event` (`TAMING_FAILED` 6, `TAMING_SUCCEEDED` 7, `IN_LOVE_HEARTS` 18
— verified against `EntityEvent`'s own declarations; note `IN_LOVE_HEARTS` is 18 and not
`LOVE_HEARTS`, which is 12 and is the villager's). Replacing the substitution means pushing an
entity-event cue instead of a particle burst — `MobSim`'s `pending_animations` queue and its
`MobAnimation` enum are the existing carrier, drained by the connection's timer arm. It is a
genuine improvement (the client owns the burst shape, so a version bump cannot silently drift
our count) but it is *not* a bug fix: the pixels are the same today.

**The collar does not reach the screen, and the chain is broken past this crate.** The
server-side halves are now done — `MetadataField::TamableFlags`/`HorseFlags` in `protocol.rs`,
their encode arms in `v770`, and `SimMob::snapshot`'s species switch — so the flag really is on
the wire for a tamed wolf, cat, parrot, ocelot and the five horse variants. Past that, four
hops are broken:

| hop | file | state |
|---|---|---|
| decode index 18 for a wolf | `protocol/v770/src/packets/metadata.rs` | falls into `read_entity_metadata`'s `_ => {}`; `metadata_class` does not classify wolf/cat/parrot |
| carry it | `lodestone-model/src/event.rs` | `EntityMetadataUpdate` has no tame/sit field |
| make it a component | `lodestone-ecs/src/ingest.rs` | `apply_entity_metadata` has no tame/sit component |
| pick the texture | `lodestone-render/src/entity.rs` | `entity_texture_candidates` uses `default_path()` |

The last row is the interesting one: `lodestone-assets` **already** models
`EntityVariant::Wolf { coat, state: WolfState::{Wild, Tame, Angry} }` and `wolf_coat_texture`
already appends `_tame`, and `EntityTexture::resolve` — the function that would use it — has **no
production caller** outside that crate's own tests. So a tamed wolf renders as a wild pale wolf, and
closing that is a self-contained job for whoever owns the entity-rendering cluster.

### Index 18 is a collision, and the bit differs per class

Index 18 is the **most crowded index in the game**: 37 claimants in the committed jar dump
(`protocol/v770/tests/support/entity_data_index_jvm.txt`), of which **four** are the `BYTE`
serializer — `TamableAnimal.DATA_FLAGS_ID`, `AbstractHorse.DATA_ID_FLAGS`, `Sheep.DATA_WOOL_ID`
and `Shulker.DATA_COLOR_ID`. It is also `Creeper.DATA_IS_IGNITED`'s index under `BOOLEAN`. No
`entity_census` column separates the four `BYTE` ones, unlike the index-8 and index-15
collisions this repo already documents, so the guard has to live in the **producer**:
`SimMob::snapshot` switches on the species and the encoder never guesses.

And the bit differs: `TamableAnimal.isTame()` is `& 4`, `AbstractHorse.FLAG_TAME` is `2`. The
failure mode of one shared variant is worth stating precisely, because it is subtler than "the
wrong flag": `0x04` is **not in the horse's flag set at all** (`FLAG_BRED` is `8`) and `0x02`
is not in the tamable's, so a shared variant sets an *unnamed* bit and the animal reads as
**untamed** — a perfectly-formed packet with nothing visibly wrong to chase.

Three gates hold this mechanically rather than in prose:
`lodestone-v770`'s `index_eighteen_tests::every_index_eighteen_constant_matches_the_jar_dump`
checks the constants against the dump, `index_eighteen_really_is_shared_by_several_byte_fields`
asserts the collision *itself* (so the species switch cannot become pointless ceremony
unnoticed, and a fifth claimant forces a look), and
`the_tamable_and_horse_flag_bytes_use_different_bits` asserts neither variant sets the other's
bit. `taming.rs`'s `a_tamed_mob_streams_the_flag_variant_its_own_class_uses` is the
producer-side arm, including the control that a **wild** mob streams no field at all.

---

## 8. How to change it

* **Adding a tameable species**: an arm in `tame_mechanism`, a row in `breeding_food`, and — if it
  should sit or follow — a roster entry with `SitWhenOrderedToGoal` and `FollowOwnerGoal` at that
  species' own priorities and distances. **The wolf, the cat and the parrot all have one now** —
  `lodestone_entity::ai::roster::neutral::WOLF` for the wolf,
  `lodestone_entity::ai::roster::passive::{CAT, PARROT}` for the other two — so all three sit and
  follow on a real mob. The horse family is still tameable through `MobSim::interact` but has **no**
  roster entry, because `AbstractHorse` is not a `TamableAnimal` in vanilla and registers neither
  `SitWhenOrderedToGoal` nor `FollowOwnerGoal` at all — there is nothing to add for it, not a gap.
  See `roster::passive`'s own module doc for what remains disclosed-but-missing on the cat and parrot
  tables (a cat's bed/chest goals, a parrot's shoulder-riding, neither species' wolf-style
  `OwnerHurtByTargetGoal`/`OwnerHurtTargetGoal` because vanilla registers those for the wolf only).
* **Adding a mechanism** (a llama's temper, a fox's trust, an axolotl's bucket): a `TameMechanism`
  variant, not a new constant on an existing one. The four existing mechanisms differ in trigger,
  roll shape *and* side effects, which is why the enum exists.
* **Never derive an item set from a tag without checking the code path.** Three of this doc's traps
  are a tag and a Java method disagreeing: bone vs `#wolf_food`, `hay_block` vs `temper`,
  `red_mushroom` vs `#horse_food`. Read `handleEating`/`mobInteract`, then use the tag as a
  cross-check.
* **A tame chance needs a driven RNG.** Assert the exact outcome on both sides of the threshold, and
  pick a seed where the two mechanisms you are separating actually *disagree* — a seed chosen for
  `next_int(3)` proves nothing about `next_int(10)` unless the gate says so.

---

## 9. Dependencies

* `lodestone_entity::ai::goals` — `SitWhenOrderedToGoal`, `FollowOwnerGoal`, `BreedGoal`,
  `FollowParentGoal`.
* `lodestone_entity::ai::mob::MobController` — `owner_position`, `is_tame`, `is_ordered_to_sit`,
  `set_in_sitting_pose`.
* `lodestone_entity::ai::navigating_mob::NavigatingMob` — the `tame`/`ordered_to_sit`/
  `in_sitting_pose` host injection points.
* `lodestone_entity::ai::roster::neutral` — the wolf's table.
* `crate::mob_spawn::SpawnRng` — the tame and breeding-orb streams.
* `crate::effects::WorldEffect` — the particle burst.
* `crate::tick::run_mob_tick_loop` — drains `take_vocalisations`, so the particles reach a client
  with no new wiring.
* `.cache/mc/26.2/src/net/minecraft/world/entity/` — `TamableAnimal`, `animal/Animal`,
  `animal/wolf/Wolf`, `animal/feline/Cat`, `animal/parrot/Parrot`, `animal/equine/AbstractHorse`,
  `ai/goal/{SitWhenOrderedToGoal,FollowOwnerGoal,RunAroundLikeCrazyGoal}`, and
  `.cache/mc/26.2/src/data/minecraft/tags/item/*_food.json`.
