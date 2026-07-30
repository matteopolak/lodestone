# Time-of-day lighting: the day clock and `sky_darken`

## What it is

The one number that makes terrain and mobs darker at night: the factor the **sky**
half of the lightmap is scaled by. Both the model (terrain/fluid) and entity
shaders compute

```wgsl
light_term = 0.2 + 0.8 * max(sky * sky_darken(), block)
```

so `sky_darken` is the whole of "what time is it?" as far as rendering is
concerned. It comes from the server's day clock, and getting that clock wrong
looks exactly like a lighting bug.

Reported as two bugs, fixed as one: *"the world is fullbright"* and *"the mobs
look like they're in the daytime"*.

## How it works

```text
set_time packet
  → V770Adapter (holds a DayClock anchor)      crates/protocol/v770/src/adapter.rs
  → ClientEvent::TimeChanged { world_age, time_of_day }
  → WorldTime resource                          crates/lodestone-ecs/src/resources.rs
  → ClientHandle::world_time().1
  → sky_darken_for_time_of_day(time_of_day)     crates/lodestone-render/src/entity.rs
  → RenderState::set_sky_darken_source(..)      installed in app.rs, two connect paths
  → FogUniform.end_enabled[2]  (the spare `z` lane of the group-0 uniform)
  → model_pipeline.rs `sky_darken()` and entity_pipeline.rs `sky_darken()`
```

The factor rides the fog uniform's spare lane rather than getting a bind group of
its own because **the model shader is at wgpu's 4-bind-group floor** — camera,
atlas, palette and anim already spend all four. It is deliberately the *same* lane
for both passes so terrain and mobs cannot disagree about the hour; wiring one
without the other is worse than wiring neither (at midnight it makes mobs darker
than the blocks they stand on, which reads as a mob bug).

`0.0` in that lane is the "never wired" sentinel and reads as full daylight, since
vanilla's legitimate range is `[0.24, 1.0]`.

### 26.2's `set_time` is a clock *map*, and it is usually empty

`ClientboundSetTimePacket` is `(long gameTime, Map<Holder<WorldClock>,
ClockNetworkState>)`. Three different senders populate that map differently, and
this is the crux:

| sender | when | clock map |
|---|---|---|
| `ServerClockManager::createFullSyncPacket` | once, at join | **full** (all clocks) |
| `ServerClockManager::modifyClock` | on `/time set`, rate, pause | **one entry** |
| `MinecraftServer::forceGameTimeSynchronization` | ~once a second, forever | **`Map.of()` — empty** |

So roughly 19 packets in 20 carry no clock at all. An absent clock update means
*"nothing changed, keep what you had"* — vanilla's own client holds the clock
locally and lets the server correct it.

The adapter therefore holds a `DayClock` anchor (`total_ticks`, `rate`,
`at_game_time`) and extrapolates:

```rust
time_of_day = total_ticks + (game_time - at_game_time) * rate
```

Both the anchor and the elapsed-tick reference ride on `set_time`'s own
`gameTime`, so **no local tick loop is needed** — `time_of_day` advances in
~20-tick steps, one per sync. That granularity is invisible in the only consumer,
whose curve moves over thousands of ticks.

`rate == 0.0` means paused. `/gamerule advance_time false` (26.2's snake_case
rename of `doDaylightCycle`) reaches the client as a rate of zero, never as a
flag — `ClockInstance::packNetworkState` sends `paused || !advance_time ? 0.0 :
rate`.

### Which clock

26.2 registers two (`WorldClocks::bootstrap`): `minecraft:overworld` = `0`,
`minecraft:the_end` = `1`. The day/night cycle is the overworld one.
`SetTime::day_clock()` selects **the lowest holder id**, not the first on the
wire, because the wire type is a Java `HashMap` and the join-time full sync's two
entries can arrive in either order.

The key codec is `holderRegistry`, i.e. `registry(key, Registry::asHolderIdMap)`:
a **bare** VarInt registry id. There is no `+1` offset and no inline-direct path —
that convention belongs to the *other* codec, `ByteBufCodecs.holder(key,
directCodec)`, which `set_time` does not use.

## What was actually wrong (measured)

`SetTime::day_time()` fell back to `game_time` when `clocks` was empty. Since the
once-a-second sync *is* the empty case, that fallback ran every second and
overwrote the day time with the **monotonic world age**. Consequence:
`sky_darken_for_time_of_day` returned a **session constant** — whatever hour
`world_age % 24000` happened to name, for the entire session, unresponsive to the
server's clock.

Measured on the survival oracle, driving the server's clock over RCON and reading
the client's value back (`crates/lodestone-shell/tests/live_time_of_day.rs`):

```text
BEFORE — one reading, then the gate stops
/time set noon      -> client age=643037 time_of_day=643037 (reduced 19037) sky_darken=0.240

AFTER
/time set noon      -> client age=641917 time_of_day=894000 (reduced  6000) sky_darken=1.000
/time set midnight  -> client age=641977 time_of_day=906000 (reduced 18000) sky_darken=0.240
/time set day       -> client age=642037 time_of_day=913000 (reduced  1000) sky_darken=1.000
/time set night     -> client age=642097 time_of_day=925000 (reduced 13000) sky_darken=0.300
```

Note `time_of_day` **exceeds** `world_age` after the fix (894000 > 641917): the
day clock is its own counter and the two are unrelated on any world that has ever
had `/time set` run on it.

On that world the constant was `0.240` — permanent midnight. On a world whose
`age % 24000` lands in daylight the constant is `1.0`: **permanent noon**, which is
the reported "fullbright terrain" and "daytime mobs", both from this one value.

### Why "the light seems right when I break a block" needed no second bug

The reporter's observation was real, and their explanation of it was not. With
`sky_darken` pinned to `1.0`:

* Every **sky-lit** surface renders at `light_term = 1.0` — uniformly, wrongly
  bright. That is most of what you see outdoors.
* Every **sky-dark** surface is *untouched* by the defect, because
  `max(sky * sky_darken, block)` with `sky == 0` is `block`, algebraically
  independent of `sky_darken`.

Breaking a block exposes faces that open into a freshly-dug cell — low or zero sky
light. Those land in the second population, so they render correctly dark against
a world rendered wrongly bright. Nothing was fixed by the break; the break merely
revealed the one population the bug cannot reach.

### Beliefs that were confidently held and turned out false

Three, all of which cost time and one of which was the whole briefing:

1. **"The wire light never reaches the mesher for freshly-loaded chunks, so
   sections take the full-bright bridge; `light_update` arrives before its chunk,
   the `world.rs` seam no-ops, and the light is dropped permanently."** False, and
   the *opposite* of measured. Fingerprinting every light section of every streamed
   column at the instant its arrival event drained, and again after an 8-second
   settle: **61 of 61 columns byte-identical**. Light arrives complete inside
   `level_chunk_with_light` and is applied before `ChunkLoaded` is emitted. No
   light is dropped, and the standalone `light_update` no-op seam never fires for
   a column we hold. The CPU light path was correct end to end the whole time —
   a live terrain column meshes to sky nibbles spread across the full `0..15`
   range (14284 vertices at `0`, 10126 at `15`), not a flat field.
2. **"`ClockUpdate::holder_id` is `id + 1`, with `0` meaning an inline direct
   value."** False; the record said so and the decode never did. `holderRegistry`
   writes a bare id. The code was right and the comment was wrong — the failure
   mode CLAUDE.md warns about, found by reading the *codec definition* rather
   than the call site.
3. **"A green gate over the shader proves the feature works."** Two gates covered
   the two ends of this chain and both were green while the bug was on screen:
   `entity_night_pixels.rs` supplies `sky_darken` by hand, and
   `grass_light_response_gate.rs` supplies the light byte by hand. Both prove the
   *shader* responds; neither can see where the number comes from. This is the
   **world** species of vacuous test — the source is exemplary and the flaw is in
   what it was pointed at. See "Gates" below.

## The full-bright bridge: real, but transient

`snapshot_section_in` fills an absent neighbour's light slot with `None`, which
`mesh_snapshot`/`mesh_snapshot_models` resolve to
`UniformLight::pre_light_bridge()` — full sky `15`, no block light. This is
correct for the *edge of the loaded world* (air there should not render black),
but it means a column meshed before its horizontal neighbours arrive renders its
boundary faces full-bright.

That is measurable and it is not small. Column `(-4, -7)` on the terrain oracle,
meshed at the instant of arrival versus after its neighbours landed:

```text
sky nibble        0   ...    8   ...   15
at arrival:     6151      1378       13180
settled:        6744      1021           0
```

Over half that column's geometry was full-bright at arrival, and **zero** of it
should be sky-lit at all.

It heals, and the healing is not best-effort: `on_column_arrived` marks the eight
loaded neighbours dirty and `heal_dirty_columns` drains them on a budget.
Measured over a 60-second spiral load of 329 columns on the survival oracle:

```text
interior columns (all 8 neighbours arrived) = 257
  re-meshed after their newest neighbour    = 249
  meshed BEFORE their newest neighbour      =   0   <-- the stale-seam count
  never meshed (still in the worker queue)  =   8
mesh drops counter                          =   0
```

Zero stale seams. So the bridge produces a **sub-second full-bright frontier
during streaming**, never a persistent one, and it cannot be the reported defect.
Do not "fix" it by defaulting absent neighbour light to `0` — that reintroduces
the black-faces-at-the-frontier bug the bridge exists to prevent.

## How to change it

* **The clock anchor** lives in `DayClock` in `crates/protocol/v770/src/adapter.rs`.
  If a second clock ever needs surfacing (the End clock, id `1`), widen it to a map
  keyed by `holder_id` and select per dimension. The constraint is
  `ClientEvent::TimeChanged`'s single `time_of_day` field, not the struct.
* **Resolving the clock by name** rather than by lowest id needs the
  `minecraft:world_clock` registry from the configuration `registry_data` packet,
  which this crate does not ingest. Only necessary if a data pack reorders the
  registry.
* **The curve** is `sky_darken_for_time_of_day` in
  `crates/lodestone-render/src/entity.rs`. It is a port of 1.21's
  `Level.getSkyDarken` plus `LightTexture`'s `* 0.95 + 0.05` lift.

  **Known divergence, deliberately not chased here.** 26.2 deleted
  `getSkyDarken` and replaced it with a data-driven timeline track,
  `EnvironmentAttributes.SKY_LIGHT_FACTOR` (`Timelines::OVERWORLD_DAY`), with
  keyframes `730 → 1.0`, `11270 → 1.0`, `13140 → 0.24`, `22860 → 0.24` under a
  symmetric cubic-bezier ease, read by `LightmapRenderStateExtractor`. The two
  agree exactly on both **plateaus** — `1.0` by day and `0.24` at night, and
  `0.24` is literally `Timelines::NIGHT_SKY_LIGHT_FACTOR` — but their **ramp
  shapes differ** across dusk and dawn. Our `night` (13 000) reads `0.300` where
  the timeline is already at `0.24`. Porting the timeline is the correct
  end state and is a separate piece of work; it would also bring
  `SKY_LIGHT_COLOR` (night sky light is tinted blue, `0.48, 0.48, 1.0`), which we
  do not model at all.
* **Adding a consumer**: install it off `RenderState::sky_darken()` rather than
  re-deriving from `world_time()`, so there stays exactly one clock on screen.

## Gates

Kept deliberately separate so a pass on one cannot mask the other.

| gate | what it can see | what it structurally cannot |
|---|---|---|
| `crates/protocol/v770/tests/world_state.rs` | the hold-and-extrapolate rule: empty maps do not clobber, `rate 0` freezes, lowest-holder-id selection | anything above the adapter |
| `crates/lodestone-shell/tests/live_time_of_day.rs` | **the runtime feed**: the client's day clock follows the real server's `/time set`, and the derived `sky_darken` spans the curve | pixels |
| `crates/lodestone-render/tests/entity_night_pixels.rs` | the entity shader responds to a `sky_darken` it is *handed* | where that value comes from |
| `crates/lodestone-render/tests/grass_light_response_gate.rs` | the model shader responds to a light byte it is *handed* | where that value comes from |

The live gate carries the control this defect specifically needs. On a *fresh*
world the world age and the day clock coincide, so a broken feed and a working one
report the same number and the gate would pass vacuously. It therefore asserts
that `world_age % 24000` is **not** the value being reported — a world too young
for the two to have separated fails with a fix hint rather than passing.

Both controls were run, not described. Reverting the adapter to the old fallback
turns three hermetic tests red (`an_empty_clock_map_does_not_overwrite_the_held_day_time`,
`a_paused_clock_does_not_advance_with_the_world_age`,
`an_unsynced_clock_falls_back_to_the_world_age`) and fails the live gate on its
first reading with `time_of_day=643037 (reduced 19037) sky_darken=0.240`.

## Configuration

None. No flags, no env vars. Two things are worth knowing:

* The live gate needs the **survival** oracle (`./scripts/live-oracles/survival.sh`,
  game `:25565`, RCON `:25566`) rather than the flat creative one — not for its
  terrain, but because that world has been running long enough for the world age
  and the day clock to have diverged, which is the condition the defect needs to
  be visible.
* `/gamerule advance_time false` is 26.2's spelling. `doDaylightCycle` is rejected.

## Dependencies

* `crates/protocol/v770/src/packets/time.rs` — `SetTime` / `ClockUpdate` decode.
* `crates/protocol/v770/src/adapter.rs` — `DayClock`, the `SET_TIME` arm.
* `crates/lodestone-ecs/src/resources.rs` — the `WorldTime` resource.
* `crates/lodestone-render/src/entity.rs` — `sky_darken_for_time_of_day`.
* `crates/lodestone-shell/src/gpu.rs` — `SkyDarkenSource`, `fog_with_clock`.
* `crates/lodestone-shell/src/app.rs` — installs the source on both connect paths.
* `crates/lodestone-render/src/{model_pipeline.rs,entity_pipeline.rs}` — the two
  shaders that read the lane.
* Behavioural reference: `.cache/mc/26.2/src/net/minecraft/world/clock/*`,
  `world/timeline/Timelines.java`, `client-src/.../LightmapRenderStateExtractor.java`.
