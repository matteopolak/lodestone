# Fishing

## What it is

A server-side port of vanilla's fishing rod: casting a bobber, its cast/bob/bite state
machine, the fish/junk/treasure loot roll (Luck of the Sea shifting weight toward
treasure), and reeling in a real item entity plus experience. It lives in
`lodestone_server::mobs` (`MobSim::cast_fishing_bobber`/`retrieve_fishing_bobber`/
`tick_fishing_bobbers`, in `mobs/fishing.rs`).

---

## 1. What reaches the screen, and what does not yet

**Reaches the screen today, purely as a `lodestone-server` change:** a bobber cast into
water floats and dips realistically (real physics, ticked every server tick and streamed
through the ordinary entity snapshot path — `minecraft:fishing_bobber` is already a
registered entity type), bites after a real lure/hook timer, and reeling it in through
`MobSim::retrieve_fishing_bobber` spawns a real item entity (via `MobSim::spawn_item`,
flying toward the reeling player exactly as vanilla's `retrieve()` throws it) and a real
experience orb (via `MobSim::award_experience`). Both of those are this sim's own
existing item/orb producers — this closes the loop end to end, not just to a loot roll.

**Not reachable without an edit outside this crate's owned files** (see §5): the actual
right-click-with-a-rod trigger. `MobSim::cast_fishing_bobber`/`retrieve_fishing_bobber`
are real, tested, and **have zero production callers** — the same shape
`spawn_potion_projectile_from`'s own doc discloses for splash potions. `server.rs`'s
`apply_use_item` is where the hook belongs (next to its existing `launch_intent`
dispatch for bows/throwables), and it is off limits to this change.

---

## 2. The state machine, ported from `FishingHook.java`

`FishingHook.tick`'s own three states (`Flying` → `Bobbing` → nibble → bite), the
`catchingFish` timers (`timeUntilLured` `100..600` minus Lure ticks, `timeUntilHooked`
`20..80`, `nibble` `20..40` once bitten), and the real downward-velocity dip the bite
applies are all transcribed with their real constants — see `mobs/fishing.rs`'s own doc
comments, which cite the exact vanilla method each block ports. `calculateOpenWater`'s
5×5×4-layer scan (a clean above-water-then-underwater stack, no mixed layer) is ported
using `ChunkWorld::block_state`/`crate::fluid::fluid_state_of`, including the real
`is_source()` requirement (a flowing-water cell does not count).

**Not ported, disclosed rather than silently dropped:**

* **`shouldStopFishing`'s distance/held-item check.** This sim tracks player
  *positions*, never per-connection state like "is this player still holding a rod" —
  the same limit `mobs/projectiles.rs`'s own module doc names for melee. A bobber here
  only ever despawns via the real `life >= 1200`-tick ground timeout; production code
  must call `retrieve_fishing_bobber` itself when a player switches away from the rod.
* **Hooking a floating item entity or a mob mid-flight** (`FishingHook.hookedIn` via
  `onHitEntity`). No entity-collision search runs against the bobber's own flight path.
  A bobber here only ever reaches `Bobbing`, never `HookedInEntity`.
* **The rain/sky-visibility `fishingSpeed` modifier** and **the particle bursts**
  (`FISHING`/`BUBBLE`/`SPLASH`). No weather state or world-effect channel crosses into
  the per-bobber tick loop; `fishing_speed` is always vanilla's own unmodified `1`.
* **`set_damage`/`enchant_with_levels` loot functions** (leather boots, fishing rod,
  bow, book all reel in undamaged and unenchanted) — no item-durability-roll or
  enchantment model exists anywhere in this crate yet.
* **The `bamboo` entry's biome condition is real**, unlike the rest of this list —
  `ChunkWorld::biome_at` is queried and the jungle-family check is exact.

---

## 3. The loot table, transcribed exactly

`data/minecraft/loot_table/gameplay/fishing{,/fish,/junk,/treasure}.json`, not
approximated: the three-way pool split (junk weight 10 quality -2, treasure weight 5
quality 2 — gated on `in_open_water` — fish weight 85 quality -1) uses vanilla's real
`LootPoolEntryContainer.getWeight(luck) = max(0, floor(weight + quality*luck))` integer
formula, and every item id/weight inside each sub-table is copied from its own JSON file.
`mobs/fishing.rs`'s unit tests predict the derived split at `luck = 0` (10/5/85%) and at
`luck = 15` (treasure share `35/105 ≈ 33.3%`, junk's effective weight floored to exactly
`0`) — the discriminating input an enchanted rod needs, not merely "more treasure than
before."

`luck` is `rod_luck + owner_luck`, matching `this.luck + owner.getLuck()`; every
production call site today supplies `0` for both (no Luck of the Sea/player-luck-effect
model exists), so the *mechanism* is real and tested while the *inputs* wait on an
enchantment system.

---

## 4. The metadata index trap, checked against the dump

`FishingHook.DATA_HOOKED_ENTITY` is index 8 — one of the five `INT` claimants
`entity_data_index_jvm.txt` already lists there alongside the experience orb's value,
`PrimedTnt.DATA_FUSE_ID`, `VehicleEntity.DATA_ID_HURT` and a display entity's
interpolation delay. **Neither `entity_census::is_living` nor `::is_mob` separates
these** (a fishing hook is neither), and this port does not add a sixth claimant to
that collision: it sends **no metadata for a bobber at all**. Adding
`MetadataField::FishingHookedEntity`/`::FishingHookBiting` and an encoder arm belongs in
`crates/protocol/v770`, off limits to this change — see §5.

**An entity whose renderer is a sprite/line rather than a cuboid rig stays absent from
the model corpus** — this port introduces no `model_for_type` entry for
`minecraft:fishing_bobber`, so that invariant is untouched.

---

## 5. Known gaps needing an edit outside this crate's owned files

* **The cast/reel trigger.** `server.rs`'s `apply_use_item` needs a `path ==
  "fishing_rod"` arm before its `launch_intent` check: call
  `MobSim::player_active_bobber(player_entity_id)` to decide cast vs. reel, then
  `cast_fishing_bobber`/`retrieve_fishing_bobber`. `player_entity_id` is already
  threaded through this connection's broader scope (used by
  `encode_hurt_animation`/`mount_vehicle` nearby) but is not currently a parameter of
  `apply_use_item` itself. `retrieve_fishing_bobber`'s returned `rod_damage` needs
  applying to the held stack through whatever durability path bow-shooting already
  uses.
* **`DATA_HOOKED_ENTITY`/`DATA_BITING` metadata** — new `MetadataField` variants plus an
  encoder arm in `crates/protocol/v770/src/server_protocol.rs`. Until then the bite/dip
  motion still reaches the wire through position/velocity alone (real physics, not an
  animation flag), just with no line-to-hook-line render cue.
* **Sound/particles** (`FISHING_BOBBER_THROW`/`_RETRIEVE`/`_SPLASH`, the `FISHING`/
  `BUBBLE`/`SPLASH` particle bursts) — would go through `crate::effects::WorldEffect` +
  `tick.rs`'s `publish_effect`, the same channel `crate::effects::block_destroyed`
  already uses; not wired here to keep this change inside owned files.

---

## 6. How to change it

* **Wiring the cast/reel trigger** is the one piece described in §5 above.
* **Adding rod-enchantment support** (Lure/Luck of the Sea) needs an enchantment model
  this crate does not have; once one exists, thread real levels into
  `cast_fishing_bobber`'s `luck`/`lure_speed` parameters instead of `0`/`0`.
* **Adding the hooked-entity/floating-item snag** needs the same
  entity-collision-along-a-segment search `mobs/projectiles.rs`'s
  `resolve_projectile_impacts` already performs for arrows; reusing that machinery
  rather than duplicating it is the natural next step.
* **Adding metadata** means a new `MetadataField` variant pair and an encoder arm in
  `crates/protocol/v770`, then filling `mobs/fishing.rs`'s
  `fishing_bobber_snapshots`' currently-empty `metadata: Vec::new()`.

---

## 7. Configuration

No game rule or constant gates fishing on/off today (vanilla has none either — a rod
always works). `mobs/fishing.rs`'s `FISHING_ROLL_SEED` is the RNG stream's fixed seed,
on its own stream for the same reason every other `SpawnRng` field on `MobSim` is.

---

## 8. Dependencies

* `crate::mob_spawn::SpawnRng` — the cast-noise/lure/hook/loot RNG stream.
* `crate::fluid::fluid_state_of` — water/source detection for the bobbing and
  open-water checks.
* `MobSim::spawn_item`/`MobSim::award_experience` — the two existing producers a catch
  drives (`mobs/items.rs`, `mobs/orbs.rs`).
* `ChunkWorld::biome_at`/`::surface_y`/`::block_state` — the terrain seam.
* `.cache/mc/26.2/src/net/minecraft/world/entity/projectile/FishingHook.java`,
  `.../world/item/FishingRodItem.java`,
  `data/minecraft/loot_table/gameplay/fishing{,/fish,/junk,/treasure}.json`.
