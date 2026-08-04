# Mob perception: what a goal is allowed to know

## What it is

The seam that lets a goal ask questions about the world — *am I in water, who hurt me, where is the
nearest player, is there a threat nearby* — and the server-side feed that answers them. Before issue
[#441](https://github.com/matteopolak/lodestone/issues/441) this seam existed but was **unfilled**:
`NavigatingMob`'s `impl MobController` left eight perception methods at their trait defaults, so six
goals had a `can_use` that was constant-`false` in the running game and two more read fields nothing
ever wrote. Eight of thirteen implemented goals could not act, and every one of them had a *green*
unit test. This doc records what each method reads, who feeds it, and why the whole thing was
invisible to the test suite.

---

## 1. Why this was invisible

Every affected goal had a green unit test. Those tests drive `ScriptMob`
(`crates/lodestone-entity/src/ai/goals.rs`), a test fake that **overrides all eight methods** — so
the goal logic was genuinely correct and genuinely proven, against a controller that production never
uses. `NavigatingMob` is the only production implementor, and it was the one that answered `false` /
`None` / `0` to everything.

This is CLAUDE.md's *world* species of vacuous test: the flaw is in the **input data** (which
controller the test was pointed at), so reading the test source cannot reveal it. The five goals that
*did* work — `RandomStrollGoal`, `RandomLookAroundGoal`, `MeleeAttackGoal`,
`NearestAttackableTargetGoal`, `SwellGoal` — are exactly the five `MobSim::spawn_species` installs.
The roster had been built to the subset that happened to function, which is why nothing looked wrong.

**So every gate for this subsystem must name `NavigatingMob` as its type.** A rewrite against
`ScriptMob` passes identically and proves nothing. The gates live in
`crates/lodestone-entity/src/ai/navigating_mob.rs` (`can_use` level) and
`crates/lodestone-server/tests/mob_sim.rs` (behavioural, through `MobSim::tick`).

---

## 2. The eight methods, and who answers them

| method | source | fed by | reaches the game? |
|---|---|---|---|
| `in_water` | the `PathWorld` the mob already borrows | nothing — derived | **yes** |
| `in_lava` | same | nothing — derived | **yes** |
| `last_hurt_by` | decaying record, set by `note_hurt` | `MobSim::attack`, `MobSim::tick`'s melee | **yes** |
| `is_panicking` | decaying record, set by `note_hurt` | `SimMob::apply_damage` (every damage path) | **yes** |
| `no_action_time` | the sim's own counter | `MobSim::tick` | **yes** |
| `avoid_threat` | the sim's mob census + `avoided_species` | `MobSim::tick` | **yes** |
| `nearest_player` | `MobSim::set_players` | `server.rs`'s `PlayerMoved` arm | **yes** |
| `temptation` | `MobSim::set_players` + `tempt_food` | same | **yes** |

Plus the two overridden-but-unfed fields that gated `BreedGoal` / `FollowParentGoal`:
`partner_candidate` and `parent_candidate`, both now populated by `MobSim::tick`, and `take_bred()`,
now drained and resolved into a real child spawn.

### Two records, not one

`note_hurt(attacker)` writes **both** of vanilla's damage records, because one `hurt` call does:
`lastDamageSource` (`LivingEntity.java:1268-1269`), which is what `PanicGoal.shouldPanic` reads
(`ai/goal/PanicGoal.java:61-63`), and `lastHurtByMob` (`LivingEntity.java:1358`), which is what
`HurtByTargetGoal` reads (`ai/goal/target/HurtByTargetGoal.java:34-36`).

They expire on **different** timers, and that is behaviour, not trivia:

| record | ticks | vanilla cite |
|---|---|---|
| panic (`is_panicking`) | 40 | `LivingEntity.java:1420-1421` |
| retaliation (`last_hurt_by`) | 100 | `LivingEntity.java:493` |

So a mob stops fleeing 60 ticks before it stops hunting. Collapsing them into one timer is a silent
behaviour change; `panic_expires_on_its_own_shorter_window_while_retaliation_persists` is the
assertion that fails if someone does.

`note_hurt(None)` is damage with no living attacker (fall, drowning, an explosion this seam cannot
attribute). It panics the mob and deliberately **leaves any existing `last_hurt_by` alone** — the two
records are independent, so a cow shoved into a cactus mid-fight does not forget the wolf.

---

## 3. Ordering inside `MobSim::tick`

Load-bearing, in this order:

1. `no_action_time` ages, for **every** mob.
2. `feed_perception()` — one read-only census pass, then one apply pass.
3. per-mob `m.mob.tick(&mut m.goals)` — this is what evaluates `can_use`.
4. melee resolution (now carrying the attacker's position, so the victim can retaliate).
5. `resolve_breeding()`.

Step 1 is before step 2 because that is vanilla's own order: `Mob.serverAiStep()` opens with
`this.noActionTime++` and only then ticks the selectors (`Mob.java:715-717`), so a goal sees the
already-incremented value. **This was implemented backwards first** and cost exactly one tick of idle
time — invisible to any `cargo check`, and caught only because
`no_action_time_crosses_the_seam_instead_of_staying_on_the_sim_record` asserts the sim record and the
seam reading are *equal* rather than merely both climbing. If you reorder this, that test is the one
that will tell you.

`feed_perception` is two passes for a borrow-checker reason, not a style one: deciding mob `i`'s
threat/partner/parent means reading every *other* mob, so decisions are computed under shared borrows
and applied under a mutable one.

---

## 4. How to change it

- **Adding a perception method.** Add it to `MobController` (`ai/mob.rs`) *with a default*, override it
  on `NavigatingMob`, and feed it in `MobSim::tick`. Skipping the third step is the whole defect this
  doc exists about — and it produces no warning of any kind.
- **Range gates: decide whether the range is vanilla's *mob attribute* or the *goal's* constructor
  argument, and put it in the matching place.** `TEMPT_RANGE` is an attribute
  (`ai/attributes/Attributes.java:107`, default `10.0`) so the **feed** applies it.
  `LookAtPlayerGoal`'s `lookDistance` is a constructor argument (6.0F/8.0F per species) so the feed
  passes `nearest_player` **uncut** and the goal decides. Applying a range in both places silently
  takes the minimum of the two and makes the goal's own parameter a lie.
- **Search shape.** Vanilla filters by an axis-aligned **box** (`getBoundingBox().inflate(dx, dy, dz)`)
  and only then picks the nearest by squared distance. `nearest_by` keeps both steps. Collapsing to a
  single radius is wrong in the corners — most visibly for `AVOID_RANGE_Y`, where vanilla's vertical
  extent is a flat `3.0` regardless of the horizontal one (`ai/goal/AvoidEntityGoal.java:72`).
- **`avoided_species` is perception data, not a goal set.** It answers "is that a threat to me".
  Assembling goals per species is the roster's job. Extend the table when a roster unit adds a species
  with an `AvoidEntityGoal`; an unknown species returns an empty slice so the goal stays correctly
  inert rather than fleeing everything.

### Gotchas

- **`FloatGoal`'s condition is a disjunction** (`in_water() || in_lava()`), so a test that only ever
  sets water cannot tell it from `in_water()` alone — one arm can be dead and green. Drive each arm
  separately.
- **`no_action_time`'s default `0` is wrong in the *permissive* direction.** Stroll was always
  eligible where vanilla suppresses it at `>= 100` (`ai/goal/RandomStrollGoal.java:43`). Nothing warns
  about a default that merely makes a goal too eager.
- **Identifying a breeding partner after the fact.** By the time `take_bred()` is drained, `breed()`
  has already cleared the breeder's love state, so "the other mob still in love" is not a usable key.
  `resolve_breeding` uses proximity instead — vanilla only breeds within `distanceToSqr < 9.0`
  (`ai/goal/BreedGoal.java:57`), so the nearest same-species adult inside that radius *is* the partner.
- **Both animals of a pair can call `breed()` on the same tick**, each holding the other as its
  candidate. Without the `consumed` guard in `resolve_breeding`, one mating produces two children and
  the population doubles every time.

---

## 5. The player feed, and where it comes from

`MobSim` had **no player-position feed at all** before this work: `MobSim::tick` takes no arguments
and `tick::run_tick_loop` receives no player position either — the same gap `run_mob_tick_loop`'s own
doc comment discloses for `despawn_pass`. So `nearest_player` and `temptation` had no possible source,
and they were the last two of the eight to be closed.

`MobSim::set_players` is the seam; the producer is `server::dispatch_play_packet`'s
`ServerBound::PlayerMoved` arm, which already held the new position, a `MobHandle` **and** the
player's `PlayerInventory` in one scope — so nothing had to be threaded and `tick.rs` needed no
change.

Two properties of that placement, stated rather than left to be discovered:

- **It is single-player-shaped.** `set_players` replaces the whole list, so with two connections each
  would clobber the other's entry. That is correct for `open_in_memory_with_mobs`' single player — the
  only configuration with a mob tick loop at all — and a real multiplayer server wants per-connection
  registration instead.
- **It is position-driven**, so a perfectly stationary player stops refreshing it. Harmless, because
  the value is a *position*, not a timer: a stale entry for a motionless player is still the right
  answer. The same holds for `held_item` until they move after a hotbar switch.

### `PlayerPerception` carries the held **item**, not a boolean

Because the answer is per-*species*: wheat tempts a cow and a sheep, a potato tempts only a pig,
pumpkin seeds only a chicken. A boolean computed by the producer would have to be either wrong for
some species or computed once per (player, species) pair — which is the feed's job.

`tempt_food` is the table, and **every entry is transcribed from the jar's own tag JSON** under
`.cache/mc/26.2/src/data/minecraft/tags/item/`, not from memory. That distinction is the whole point:
older versions used a single item per species, and the folklore list ("carrot for pig, seeds for
chicken") is wrong for 26.2 in two places — `pig_food` is three items and `chicken_food` is six.

**`tempt_food` is interim and should be replaced, not extended.** Roster unit B2 owns a *generated*
item-tag table following the `collision_shapes`/`hardness` generate-or-assert + `LODESTONE_REGEN=1`
pattern (the `damage_types` extraction is the closest existing precedent for pulling tags out of
datapack JSON). When it lands, `tempt_food`'s body becomes a lookup into it and nothing around it
changes — the plumbing is already in terms of a real held item. Delete
`the_tempt_table_is_per_species_and_matches_the_jar_not_folklore` and gate the generated table instead.

### The gate that would catch this going dead again

Every gate that calls `MobSim::set_players` itself would pass with **no producer anywhere** — which is
precisely the state this was in before. So the island gate is
`a_player_moved_packet_feeds_mob_perception_through_the_real_connection` in `tests/serve_play.rs`: it
never touches `set_players`, it drives a real `PLAYER_MOVED` packet through `serve_connection`, and it
asserts both the position *and* the held wheat arrived. Deleting the producer line fails that test and
nothing else. If you are changing this subsystem, that is the test to keep alive.

---

## Configuration

No env vars or flags. Every constant is a cited vanilla value: `LAST_HURT_BY_TICKS` and
`PANIC_DAMAGE_TICKS` in `ai/navigating_mob.rs`; `TEMPT_RANGE`, `AVOID_RANGE`, `AVOID_RANGE_Y`,
`BREED_RANGE`, `BREED_DISTANCE_SQR`, `FOLLOW_PARENT_RANGE`, `FOLLOW_PARENT_RANGE_Y` in
`lodestone-server/src/mobs.rs`.

## Dependencies

- `lodestone-entity` — `MobController`, `NavigatingMob`, the goals, and `PathWorld` (which supplies
  the water/lava classification, so `FloatGoal` needs no host at all).
- `lodestone-data` — `path_types`, via `ChunkWorld::base_path_type`, for real per-block-state fluid
  classification rather than a solid/air guess.
- `lodestone-server` — `MobSim` / `SimMob` own the feed; `server::apply_attack` is the production
  producer of the retaliation record.

## Related

- [`mob-ai-roster.md`](./plans/mob-ai-roster.md) — the epic plan this is unit A1/A2 of.
- Issues [#234](https://github.com/matteopolak/lodestone/issues/234) (breeding) and
  [#237](https://github.com/matteopolak/lodestone/issues/237) (aging): the half landed in `7bf2873`
  was itself an island; the resolution is here.
