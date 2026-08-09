# Experience

## What it is

The XP level curve, the orb denomination ladder and a player's experience state.
`crates/lodestone-server/src/experience.rs` holds all three as pure arithmetic;
`ServerProtocol::encode_set_experience` puts the result on the wire, and
`crates/lodestone-server/src/player_data.rs` saves and restores it. The one production
*source* today is furnace smelting, paid out when the player closes the menu.

## How it works

### The level curve is three regimes, and the boundaries are the bug

`Player.getXpNeededForNextLevel`, transcribed as `level_up_cost`:

| level range | cost of the next level |
|---|---|
| `0..15` | `7 + level * 2` |
| `15..30` | `37 + (level - 15) * 5` |
| `>= 30` | `112 + (level - 30) * 9` |

Both seams are **inclusive** — level 15 is in the middle regime, level 30 in the top
one. The trap is that at level 15 the two hypotheses *coincide*
(`7 + 15*2 == 37 + 0*5 == 37`), so an exclusive-boundary implementation is only
distinguishable at level 16: inclusive gives 42, exclusive gives 39. The gate uses
16 and 31 for exactly that reason.

30 levels is **1395** points; the first level is 7.

### Orb denominations are greedy change-making, not division

`ExperienceOrb.getExperienceValue` returns the largest entry of
`[2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3, 1]` that fits, and
`awardWithDirection` loops. So 100 becomes `[73, 17, 7, 3]` — **four** orbs, and the
tail is `3` (a denomination in its own right) rather than `1+1+1`. A uniform-cap
implementation emits a different orb *count* for almost every amount, and orb count
is player-visible.

The ladder is roughly doubling but irregular (`3 → 7` is ×2.33, `7 → 17` is ×2.43),
so it is transcribed rather than generated.

### `give_points` re-expresses its carry

The overflow fraction is multiplied back by the **old** level's cost to recover
points, the level increments, and it is divided by the **new** cost. Leaving it as
`progress - 1.0` charges every subsequent level the first level's price of 7, so a
single large award over-levels badly. The downward loop is not symmetric: at level 0
it zeroes progress and total rather than borrowing, so XP cannot go negative.

`total` is genuinely a third piece of state, not derived: vanilla zeroes it on a level
underflow and never recomputes it, so a player who has enchanted has a `total` that no
longer matches their level.

### Furnace smelting, the first source that was wired

`Furnace::take_recipes_used` banks `(recipe_key, count)` per cook
and had **no caller**; `experience_for_recipes` now drains it in `server.rs`'s
`ContainerClosed` arm, which is vanilla's own
`awardUsedRecipesAndPopExperience` trigger. The recipe key is
`"<table>:<ingredient>"` and the ingredient is itself namespaced, so **split on the
first colon only** — splitting anywhere else yields a table name no lookup knows and
the function silently returns zero for every entry, which looks like "no XP yet"
rather than a failure.

### Who sends the packet, and why the join send is separate

Three producers, and for a while there was only one — which is why **the XP bar never
appeared at all**, in survival as much as creative:

| producer | when |
|---|---|
| `crate::server::join_experience` | once, at the top of both `serve_play` variants |
| the `ServerBound::ContainerClosed` arm of `dispatch_play_packet` | after a furnace pays out banked smelting XP |
| the orb absorption in both `serve_play` loops' movement arm | after `collect_nearby_orbs` banks an orb |

The furnace arm was the only one for a while. The encoder existed in both the
`ServerProtocol` trait and `V770ServerProtocol`, the client decoded `SET_EXPERIENCE`
into `ClientEvent::ExperienceChanged`, and the HUD drew the bar from it — but a
player who had never closed a furnace was sent the packet **zero times**, so the bar
had no values to draw from. Creative was a red herring in the report: vanilla does
hide the bar in creative, but it does so *client-side* via `Player.hasExperience` and
its server still sends the packet, so a server-side game-mode gate would be a
divergence.

**Vanilla does not send this from `placeNewPlayer`**, which is why "on join" needs
stating rather than being obvious. `ServerPlayer.doTick` sends whenever
`this.totalExperience != this.lastSentExp`, and `lastSentExp` is initialised to
`-99999999` — so the comparison is true on the first tick after *any* join, even at
zero experience, and the packet goes out unconditionally. Every mutator
(`setExperiencePoints`, `setExperienceLevels`, `giveExperienceLevels`,
`onEnchantmentPerformed`) additionally forces `lastSentExp = -1`, which is how a
change to **progress or level alone** — leaving `totalExperience` untouched — still
resends. The equivalent here is: send once at join, and send after every mutation.

## How to change it

* **A new XP source** (breeding, fishing, ore-breaking, mob death): call
  `PlayerExperience::give_points` and send `encode_set_experience`. **Do not add a
  second curve.** The send is not optional bookkeeping — a mutation with no send is
  the shape that made the bar invisible in the first place.
* **Spending XP** (enchanting): `PlayerExperience::take_levels`, which zeroes progress
  and total on underflow — clamping only the level leaves a full bar at level 0.
* **Persistence**: `XpLevel` / `XpP` / `XpTotal`, vanilla's own names, via
  `PlayerExperience::restored`. `PlayerData::experience` models all three, and
  `serve_play` seeds the live value from the saved file exactly as it does `vitals` and
  `inventory`.

### Persistence had to be one change, not two

The bug this replaced is worth keeping because the *file* was never wrong. `PlayerData`
did not model the three `Xp*` fields, so they rode through `PlayerData::preserved` —
read at join, written back verbatim on every save, and never looked at. XP therefore
survived the file and not the session: earn 31 levels, quit, rejoin at zero, and the
`.dat` still says 31.

The trap in fixing it is that **modelling the fields without reading them back is
strictly worse than the bug**. A writer that emits our own `XpLevel` while the live
value is `default()` overwrites the file's real XP with this session's zeros on the
first periodic save — silent, permanent loss instead of a recoverable display bug. So
`PlayerData::capture` takes the live `PlayerExperience` and `serve_play` restores from
the file in the same change; `persist_player`'s own parameter comment says so at the
call site.

The gates are `a_rejoining_player_is_sent_the_experience_they_earned` (v770's
`server_join_experience.rs`, end to end: file → session → wire) with
`control_a_saved_player_with_no_experience_still_joins_at_zero` as the arm that must
disagree, and the player-file round trip in `lodestone-server`'s
`entity_persistence_round_trip.rs`. All three use level **31** / total **1557** /
bar **50/121** — three distinct non-trivial numbers, because `XpLevel` and `XpTotal`
are adjacent `Int`s in NBT and adjacent VarInts on the wire, and a transposition of
two same-typed fields is legal everywhere except on the bar.

### Gotchas

* `SET_EXPERIENCE`'s wire order is **progress, level, total** — not declaration order
  and not alphabetical. The client-side decoder already carried that warning before
  anything encoded the packet. Read `ClientboundSetExperiencePacket`'s own `write`
  method to confirm it (`writeFloat(progress)`, `writeVarInt(level)`,
  `writeVarInt(total)`) and **not** the call site in `doTick`: the record's fields and
  its public constructor are both declared `(progress, total, level)`, so `doTick`
  passes `(experienceProgress, totalExperience, experienceLevel)` and transcribing
  *that* order transposes the two integers. They are adjacent VarInts, so the swap is
  wire-legal, survives any round trip through our own symmetric code, and shows the
  wrong number on the bar. This is the "read the record definition, not a summary of
  the call site" rule with a live example.
* `restored` clamps progress strictly **below** 1.0. Exactly 1.0 is the state the
  carry loop exists to resolve, so keeping it would level the player up on their next
  award of nothing.

## The orb entity

`MobSim` owns live orbs beside its items, projectiles and falling blocks
(`MobSim::orbs`). `award_experience` is `ExperienceOrb.awardWithDirection`: it splits an
amount over the denomination ladder, tries to merge each denomination into an existing
orb, and spawns what did not merge. `tick_orbs` is `ExperienceOrb.tick`, transcribed in
that method's own order — gravity, merge scan, player pull, move, drag, landing bounce,
age — because two of those orderings are load-bearing (the pull *before* the move, and
the bounce reading the **pre-move** fall speed).

### `value` and `count` are different numbers

`value` is `DATA_VALUE`, the points **one** absorption pays out and the only field on the
wire. `count` is how many absorptions the entity holds: a merge adds the absorbed orb's
count, `playerTouch` decrements it, and the entity goes at zero. Reading `count` as "the
points this orb is worth" pays out once and silently loses the rest of a merged pile,
with the entity still vanishing at exactly the right moment.

### The merge rule is keyed on the network entity id, and 40 is the number

`(orb.getId() - id) % 40 == 0 && orb.getValue() == value`. Only orbs whose ids are
congruent mod 40 may merge, so **consecutively spawned orbs never merge with each
other** — the first candidate for id `n` is id `n + 40`. That is why a big award is a
handful of orbs rather than one pile, and why a merge gate needs **more than 40 orbs** to
observe anything at all: `control_ten_orbs_below_the_congruence_stride_do_not_merge`
exists to pin the other side of it. The conservation assertion is
`MobSim::orb_points_outstanding`, which is `sum(value * count)` — an entity count alone
cannot see a merge that lost a count.

`scanForMerges` inflates isotropically (`inflate(0.5)`), unlike item merging's
`inflate(0.5, 0.0, 0.5)`. Two orbs a block apart vertically do merge; two items never do.

### Wired XP sources

| source | vanilla trigger | notes |
|---|---|---|
| mob death | `LivingEntity.dropExperience` | requires a player hit within 100 ticks **and** `!isBaby()` |
| ore mining | `Block.spawnAfterBreak` → `tryDropExperience` | at the cell **centre**, gated on the `blockDrops` rule |
| furnace smelting | `awardUsedRecipesAndPopExperience` | still a **direct** award to the bar, not orbs — see below |

Two traps in that table, both of which cost time to establish:

- **The player-kill guard is the whole feature.** `dropExperience` reads
  `lastHurtByPlayerMemoryTime > 0`, so a mob that starves, drowns, burns, falls or is
  killed by another mob drops **nothing**. Awarding on every death turns any mob grinder
  into an XP farm, and a gate with only the player-kill arm cannot tell the two apart —
  `only_a_player_killed_mob_drops_experience` runs both.
- **Six ores pop no XP at all.** Iron, gold and copper ore (and all three deepslate
  variants) *are* `DropExperienceBlock`s registered with `ConstantInt.of(0)`, because
  they drop raw ore. "It is a `DropExperienceBlock`, so it drops experience" is wrong for
  six blocks. Also note the tool check: vanilla's `hasCorrectToolForDrops` guards
  `dropResources`, not `spawnAfterBreak`, so breaking coal ore bare-handed yields **no
  coal and the XP anyway**.

An animal's reward is `1 + nextInt(3)` — a **roll**, from `Animal`'s own
`getBaseExperienceReward` override, not the `xpReward` field every monster uses. A table
of flat numbers is wrong for every passive mob. `mob_experience_reward` carries the whole
table with each entry's source class.

### Absorption

`collect_nearby_orbs` in `server.rs` runs on the same movement cadence the item pickup
does and takes **at most one orb per call**, gated on the player's own `takeXpDelay` of 2
ticks. Vanilla's limit is on the *player*, not the orb — an orb has no pickup delay of
its own, so it is absorbable on the tick it spawns. Draining every overlapping orb at
once would bank the same total and look completely different: the client plays one pickup
sound and one absorption animation per `TAKE_ITEM_ENTITY`.

## What is not here, and what it needs

**Client-side orb rendering.** The server spawns, ticks, merges and streams orbs with
`ExperienceOrb.DATA_VALUE` at metadata index 8, which is everything a *vanilla* client
needs. Our own client has no orb sprite: `lodestone_render::entity`'s own tests assert
`model_for_type("experience_orb").is_none()` and
`entity_texture_candidates("experience_orb").is_empty()`, so an orb entity arrives,
interpolates and can be absorbed — the absorption being visible, since the bar moves —
while drawing **zero pixels** on this client. That is the island shape CLAUDE.md's first
rule names, disclosed rather than discovered later. What it needs is a billboarded quad
pass with the `experience_orb.png` sprite sheet, frame selected by
`ExperienceOrb.getIcon`'s bucketed lookup (**not** a linear map of value to frame) plus
`ExperienceOrbRenderer`'s bob.

**Furnace XP still bypasses the orbs.** `experience_for_recipes` awards points directly
on container close where vanilla's `awardUsedRecipesAndPopExperience` pops orbs at the
player. The total is right and the orbs are missing; moving it is a one-line change to
that arm plus a rewrite of the gate that asserts the direct award.

**Breeding, fishing, trading and bottles o' enchanting** are unwired. All four are
`ExperienceOrb.award` calls in vanilla, so they now need only their own trigger —
`MobSim::award_experience` is the call.

**Orbs are not persisted.** They live in `MobSim` and are not in
`MobSim::saved_entities`, so a restart loses whatever was on the ground. Vanilla saves
them (`Health`/`Age`/`Value`/`Count`).

## Dependencies

None inside `experience.rs` — pure arithmetic, usable from any thread with no setup.
The producer path depends on `crate::furnace` and `crate::block_entities`.
