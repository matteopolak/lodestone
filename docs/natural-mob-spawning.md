# Natural mob spawning

## What it is

How the integrated server populates a live world with mobs: the per-species `SpawnPlacements` table,
the per-column light the spawn rules test against, and the per-tick cycle that consults the biome
spawn lists and the mob caps. Issues #221 and #222 — before this, `crates/lodestone-server/src/mob_spawn.rs`
held a proven cap/despawn engine with **no production caller at all**, so a world held exactly the
mobs `seed_demo_mobs` put in it, forever, and nothing anywhere implemented
`lodestone_entity::spawn`'s `SpawnRule`/`SpawnEnvironment` seam.

Everything lives in `crates/lodestone-server/src/natural_spawn.rs` plus a ~25-line driver in
`tick.rs`.

## How it works

Four things had to join up, and each was separately complete before:

| piece | where | was |
|---|---|---|
| cap arithmetic + despawn gates | `mob_spawn.rs` | proven, driverless |
| biome spawn lists (795 entries, 66 biomes) | `lodestone_worldgen::spawners` | parsed, no reader |
| species → body/attributes/goals | `MobSim::spawn_species` | used only by the demo seeder |
| light | `lodestone_world::lighting` | client-side only |

### The cycle

`tick::run_tick_loop`, once per tick, gated on the `spawn_mobs` game rule and skipped entirely when
no player is loaded (vanilla spawns nothing without one):

1. `MobSim::census(spawnable_chunks)` rebuilds `SpawnState` from the live population — vanilla
   rebuilds `NaturalSpawner.SpawnState` every cycle for the same reason.
2. For each chunk of the tick area and each category still under its global cap,
   `NaturalSpawner::cluster` runs vanilla's `spawnCategoryForChunk`.
3. Each returned candidate becomes a real mob through `MobSim::spawn_species`, so it arrives with
   the species' own dimensions, attributes and vanilla goal set. The **category comes from the biome
   list's key**, not from `spawn_species`' hostile/friendly guess: vanilla's category is a property
   of the `EntityType` registration and the list key is exactly that.
4. `MobSim::despawn_pass` runs beside it against the nearest player — the other half of the same
   accounting, and also previously driverless.

`spawnable_chunks` is the tick area's own chunk count, so the caps scale with the area this loop
really simulates (49 columns → 11 monsters, 1 creature) rather than claiming vanilla's 289.

**This chain is verified end-to-end, not only per-part.**
`crates/lodestone-server/tests/natural_spawn_reaches_the_wire.rs` starts a real `IntegratedServer`
with **zero seeded mobs**, joins a connection through the duplex, moves the player (the only thing
that ever calls `MobSim::set_players`, so the cycle is skipped without it), and asserts an
`ADD_ENTITY` is encoded for a species the bundled plains list names — measured: one
`minecraft:sheep` in about a second. `tests/natural_spawn.rs` cannot see that, because it drives
`run_spawn_cycle` and `NaturalSpawner` directly and asserts on `MobSim::iter`; everything between
the tick loop and the packet is invisible to it. The negative control turns `spawn_mobs` off through
the same path and must observe **zero** spawn packets, so a spawn from any other producer cannot
read as a pass.

### Peaceful

Two independent gates, both keyed on the per-type `notInPeaceful` flag
(`mob_spawn::allowed_in_peaceful`, 38 registrations from the pinned decompile) and **never** on
`MobCategory == MONSTER` — vanilla keeps seven monsters on Peaceful (`piglin`, `shulker`,
`ender_dragon`, `zombie_horse`, `zombie_nautilus`, `camel_husk`, `sulfur_cube`):

| gate | where | vanilla |
|---|---|---|
| refuse the candidate | `NaturalSpawner`'s cluster loop, via `set_difficulty` | `SpawnPlacements.checkSpawnRules`' first statement |
| evict what is alive | `MobSim::remove_monsters`, from `run_tick_loop` | `Mob.checkDespawn` |

Neither is redundant. The eviction runs *before* the spawn cycle, so with only that half a monster
proposed on Peaceful lived one tick — long enough for the loop to publish its snapshot and the
connection to send `ADD_ENTITY`, then `REMOVE_ENTITIES` on the next pass. Monsters blinked in and
out on Peaceful. And the eviction used to consult `mobs.rs`'s 22-name `is_hostile_species` list,
which exists for the *category* question, so a slime, magma cube, silverfish, phantom, vex, ravager,
hoglin or warden survived Peaceful entirely — and slimes do spawn here, because this module models
slime chunks.

The refusal is placed **before** the rule's own predicate, matching vanilla: `checkSpawnRules` tests
peaceful first, and the predicate draws from the RNG (light brightness, the per-species chance), so
the order decides the stream.

### The cluster loop, and why it returns a group

`SpawnCandidateSource::cluster` returns a `Vec`, not an `Option`. **The RNG draw order and count is
the specification** — it is what makes spawn rates what they are — and vanilla's loop is:

```
getRandomPosWithin        // nextInt(16), nextInt(16), then a uniform Y over [minY, surface + 1]
for group in 0..3:
    attempts = ceil(nextFloat() * 4)
    for attempt in 0..attempts:
        x += nextInt(6) - nextInt(6);  z += nextInt(6) - nextInt(6)
        (distance gates)
        if no species yet: weighted pick, then attempts = minCount + nextInt(1 + max - min)
        (the rule's own chance gate, then its light draws)
```

Returning one mob per call would let the driver interleave a cap check into the middle of a group's
draws, which changes the stream. The cap is instead applied as the group is consumed, so it still
can never be exceeded.

### Light

Every monster rule in the game is a light test, and this server computed light nowhere.
`natural_spawn` runs `lodestone_world::compute_column_light` over the column's own **palette
indices**, with `lodestone_data::light_props` resolved once per palette entry — so no per-cell
registry lookup happens. Two bounds keep it inside the 50 ms tick budget:

* **`LIGHT_BUDGET_PER_CYCLE` (4) columns per cycle.** A column is ~1 ms in release; an unbounded
  first pass over 49 columns would blow the budget outright.
* **`LIGHT_TTL_TICKS` (200) then the cache is dropped wholesale.** There is no per-block relight
  anywhere in this tree (issue #94), so a torch placed in a dark room suppresses spawns within ten
  seconds rather than instantly. Vanilla's own lighting is asynchronous; this is a coarser version
  of the same lag, and it is why the cache is a TTL rather than a dirty set.

An unlit column returns `None`, and `None` means **do not spawn** — never "dark". Treating an
unknown light as darkness would make the budget a spawn-rate multiplier.

## How to change it, and the gotchas

**The species table is a record transcription.** Every row of `SPAWN_RULES` comes from
`SpawnPlacements.java`'s registration plus the `check*SpawnRules` body it names; the block-tag rows
come from `data/minecraft/tags/block/*_spawnable_on.json`, flattened. If you add a species, read its
predicate — the families genuinely differ, and the differences are the whole behaviour (a wolf wants
`WOLVES_SPAWNABLE_ON` and brightness > 8; a bat wants overworld base stone below, `nextBoolean()`,
and brightness ≤ `nextInt(4)`; a zombified piglin applies **no** light test at all).

**A species absent from `SPAWN_RULES` cannot spawn, deliberately.** A fallback to "no restrictions"
spawns guardians on land. `spawn_rule` returning `None` is why a species a biome list names but this
table does not know is inert rather than wrong, and
`every_bundled_biome_species_has_a_rule` fails if the bundled lists ever grow one.

**Known omissions, each named rather than approximated:**

| vanilla behaviour | why not modelled |
|---|---|
| `MORE_FREQUENT_DROWNED_SPAWNS` | a biome tag this crate has no table for. The rarer `nextInt(40)` arm is modelled, so drowned under-spawn rather than over-spawn |
| `REDUCED_WATER_AMBIENT_SPAWNS` | same |
| nether-fortress spawn list override | needs a live structure manager at the position |
| `spawn_costs` (the potential calculator) | parsed by `lodestone_worldgen::spawners` and still unread; Nether-only |
| ambient sky darkening | there is no world clock in the spawner, so `getMaxLocalRawBrightness` returns the **daytime** answer. Conservative: a brighter reading only ever suppresses a spawn, so surface night spawning is rarer than vanilla's, never commoner |

### Slime chunks, and the two seeds

`Slime.checkSlimeSpawnRules` is the one predicate in the table that is **two alternatives rather than
a conjunction**, so no `SpawnRule` field can carry it. It lives in `Special::Slime`, evaluated by
`NaturalSpawner::slime_permits`, and the alternation is the point: each arm owns its own Y band and
its own draws.

| arm | condition | draws |
|---|---|---|
| swamp surface | biome in `ALLOWS_SURFACE_SLIME_SPAWNS` (`swamp`, `mangrove_swamp`), `50 < y < 70` | `nextFloat() < surfaceSlimeSpawnChance`, then — only if that passed — `brightness <= nextInt(8)` |
| slime chunk | `seedSlimeChunk(cx, cz, worldSeed, 987234911).nextInt(10) == 0` and `y < 40` | `nextInt(10) == 0`, drawn **before** the slime-chunk test and so consumed in an ordinary chunk too |

Two traps here.

**The row used to be the swamp arm only, with `y_range: (51, 69)`** — and that band excludes every Y
the slime-chunk arm can fire at, so `lodestone_worldgen::is_slime_chunk` was a working predicate with
no reachable consumer. `slime_carries_both_arms_not_one` pins the shape back.

**`SURFACE_SLIME_SPAWN_CHANCE` is a moon-phase attribute in 26.2**, not a constant. It defaults to
`0.0` and the only thing that raises it is `Timelines.MOON`'s `FloatModifier.MAXIMUM` track, keyframed
`CONSTANT` to `MOON_BRIGHTNESS_PER_PHASE[phase] * 0.5`. So the chance is `0.5` at full moon and
**exactly `0.0` at new moon**, where the surface arm cannot fire at all. If surface swamp slimes look
broken, check the moon before the code.

**Two different seeds reach the spawner and both are load-bearing.** `NaturalSpawner::new`'s `seed`
is `tick::NATURAL_SPAWN_SEED`, a fixed literal, because the spawn stream only has to be reproducible.
`with_world_seed` is the **world generation** seed, and it is not free to choose — a wrong value gives
a *different set* of slime chunks from the ones the terrain was generated for, which is worse than
none. It arrives through `worldgen_data::active_world_seed()`, a process-global, because
`ChunkSource` has no `world_seed()` and the tick loop is handed an already-erased `Arc<W>`. That
function's own doc carries the trade and names the one-method fix that would delete it.

**Sea level is the `SEA_LEVEL` constant (63).** The tick loop holds a `ChunkSource`, not a generator,
so there is nothing to ask. Right for every overworld preset; a custom `sea_level` shifts the
water-animal bands.

**Open-to-LAN does not spawn.** `IntegratedServer::open_to_lan` builds a `MobHandle::default()` over
an empty `ChunkWorld`, so there is no terrain for the spawner to read. Fixing that means giving the
LAN constructor a real terrain snapshot, which is the same gap that already stops LAN mobs pathing.

**`is_valid_spawn_surface` approximates `BlockState.isValidSpawn`** as "a full collision cube whose
emission is under 14". A sturdy-*up-face* test needs per-face shape data the collision census does
not carry; the approximation rejects slabs and stairs vanilla would accept, so it under-spawns.

## Configuration

| knob | where | default |
|---|---|---|
| `spawn_mobs` game rule | `world_state::WorldStateHandle` | `true` |
| `LIGHT_BUDGET_PER_CYCLE` | `natural_spawn.rs` | 4 columns |
| `LIGHT_TTL_TICKS` | `natural_spawn.rs` | 200 ticks |
| `NATURAL_SPAWN_SEED` | `tick.rs` | a fixed literal — the stream must be reproducible, not world-derived |

Running the gates:

```bash
cargo test -p lodestone-server --test natural_spawn -- --test-threads=2
cargo test -p lodestone-server --lib natural_spawn -- --test-threads=2
```

`tests/natural_spawn.rs` builds its terrain by hand but takes its biome data from the real bundled
table and its light from the real engine, so "does the plains list spawn a cow" is asked of the
plains document rather than of a fixture. It asserts a lit grass plain populates with animals and no
monsters, a sealed dark room populates with monsters, and a glowstone floor takes that to **exactly
zero** — the direction is the claim, because light is the half of a spawn table that goes silently
wrong.

## Dependencies

* `lodestone_world` — the light engine. Promoted from a dev-dependency of `lodestone-server` for
  this (step 1 of [`server-chunk-light.md`](./server-chunk-light.md)'s brokered patch); its own
  dependencies are `lodestone-core`, `thiserror` and `serde_json`, all already in the graph.
* `lodestone_data` — `light_props`, `block_states`, `collision_shapes`.
* `lodestone_worldgen::spawners` — the per-biome lists, reached through
  `worldgen_data::bundled_biome_spawners()` (parsed from the embedded biome documents and cached,
  because the lists are seed-independent and building a generator to reach a constant table would
  cost the full settings parse per world).
* `crate::mob_spawn` — the cap engine, the despawn gates and `SpawnRng`.
* `crate::mobs` — `ChunkWorld` for terrain and biome, `MobSim::spawn_species` for the mob itself.
