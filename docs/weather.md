# Weather

Rain and snow columns, the darkening rain and thunder apply to the sky, the fog and
the lightmap, and the lightning flash.

## What it is

The server owns weather entirely. It sends four `GAME_EVENT` codes — `START_RAINING`
(1), `STOP_RAINING` (2), `RAIN_LEVEL_CHANGE` (7), `THUNDER_LEVEL_CHANGE` (8) — and
the client turns two scalars in `0.0..=1.0` into:

| effect | where it lands |
|---|---|
| angled camera-relative rain/snow quads | `lodestone-render/src/weather_pipeline.rs`, drawn inside the shell's block pass |
| sky disc + horizon + terrain fog darkened | `app.rs`'s `desired_fog` composition, via `weather_darken_linear` |
| lightmap darkened (terrain, mobs, held arm) | the existing `sky_darken` lane, via `weather_sky_light_factor` |
| lightning flash (sky tint + full-bright) | the same two, via `lightning_flash_linear` and `weather_sky_light_factor` |

Rain level also gates all of it: at `0.0` the extraction returns immediately, the fog
composition is byte-identical to no weather at all, and the pass draws nothing.

## How it works

```
GAME_EVENT (v770 adapter)
  -> ClientEvent::WeatherChanged { raining, rain_level, thunder_level }
  -> net.rs `forward`'s arm            <- the fold, into a shared cell
  -> net::WeatherCell (2x AtomicU32 + AtomicU64)
  -> NetClient::shared_weather()
  -> app.rs `WeatherTracker::state()`  <- one read per frame, shared by both consumers
       |
       +-> render.set_sky_darken_source closure -> weather_sky_light_factor -> lightmap
       +-> redraw's `desired_fog`            -> weather_darken_linear     -> sky/fog/clear
       +-> weather_columns_for_frame         -> extract_columns/column_instance
             -> RenderState::prepare_weather -> WeatherRenderer::draw
```

`ADD_ENTITY` for a `lightning_bolt` reaches the same `forward` and bumps the cell's
lightning sequence number; `WeatherTracker` turns a change in that number into a
250 ms flash (5 game ticks at 20 tps).

### Why the wire state does not travel `NetUpdate`

`ServerLevel.tickWeather` ramps the rain level by ±0.01 **per tick** and broadcasts
`RAIN_LEVEL_CHANGE` on every change (`ServerLevel.java:762-775`), so a channel would
carry ~20 messages a second whose only purpose is to be superseded. The cell is the
same "latest wins, never queue" shape as `net::SharedHandle`, and it keeps the read
out of `Sim` — nothing in the simulation acts on rain level, only the renderer and
(eventually) the audio cadence do.

The **arm still lives in `forward`**, not in the net loop that calls it. That `match`
is the one place a reader looks to answer "does anything consume event X"; three
separate islands have already been found in that single function, and an event
handled outside it is invisible to that reading.

### Why the router is `net.rs` and not either ECS router

`SharedState::apply` consults `ingest::handles_event` and `session::handles_event`.
Per `CLAUDE.md`'s rule of thumb: per-entity state is `ingest`, local-player scalars
are `session`, and **world** state is neither — it travels the shell stream, exactly
as `BlockEvent` does. Rain level is a property of the level.

## Vanilla constants, and where each was read

All paths are relative to `.cache/mc/26.2/`. The table in
`lodestone-render/src/weather.rs`'s module doc is the authoritative copy; the
load-bearing ones:

| quantity | value | source |
|---|---|---|
| perpendicular-offset table | 32 × 32, centred at 16 | `client-src/.../WeatherEffectRenderer.java:43-60` |
| rain column speed | `3.0 + rand` | `:168` |
| rain texture scroll | `-(ticks + offset + partial) / 32 * speed`, wrapped at 32 | `:169-170` |
| snow V drift | `-((ticks & 511) + partial) / 512` | `:181` |
| per-column seed | `x*x*3121 + x*45238971 ^ z*z*418711 + z*13761` | `:80` |
| rain / snow max alpha | `1.0` / `0.8` | `:133-134` |
| distance fade | `lerp(min(d²/r², 1), max_alpha, 0.5) * intensity` | `:201` |
| V from world height | `y * 0.25 + v_offset` | `:214-215` |
| rain→snow threshold | height-adjusted temperature `>= 0.15` is rain | `.../biome/Biome.java:175-176` |
| `getThunderLevel` | raw thunder **× rain** | `.../level/Level.java:918-920` |
| `isRaining` / `isThundering` | `rain > 0.2` / `thunder > 0.9` | `Level.java:947`, `:943` |
| sky rain darken | `×(1 − r·0.5, 1 − r·0.5, 1 − r·0.4)` | `.../fog/environment/AtmosphericFogEnvironment.java:51-54` |
| sky thunder darken | `×(1 − t·0.5)`, all three channels | `:57-59` |
| `SKY_LIGHT_FACTOR` floor | `0.24`; alpha `0.3125` rain, `0.52734375` thunder | `.../attribute/WeatherAttributes.java:19`, `:30` |
| weather layer split | `thunder = t`, `rain = r − t` | `WeatherAttributes.java:49-50` |
| lightning flash | lerp `0.22` toward `(204, 204, 255)`; `SKY_LIGHT_FACTOR` forced to `1.0` | `.../multiplayer/ClientLevel.java:264-268` |
| flash lifetime | `skyFlashTime = 2` refreshed while `life >= 0`, `life` starts at 2 | `.../entity/LightningBolt.java:45`, `:139-141` |
| rain sound cadence | `rand(3) < rainSoundTime++` | `ClientLevel.java:384` |
| rain sound volumes | `0.2 @ 1.0` near, `0.1 @ 0.5` from above | `ClientLevel.java:388-390` |
| blend / cull / depth | `TRANSLUCENT`, no cull, `GREATER_THAN_OR_EQUAL` no-write | `.../RenderPipelines.java:141-143`, `:635-640` |

## The three things that will bite you

### 1. `START_RAINING` sets the rain level to `0.0`, and `STOP_RAINING` sets it to `1.0`

That is vanilla's own inversion at `ClientPacketListener.java:1542-1545`, and
`WeatherState::apply_raining` reproduces it deliberately. It is invisible in practice
because `ServerLevel.java:783-791` sends a `RAIN_LEVEL_CHANGE` carrying the true
level immediately after every start/stop, plus one per tick while the level ramps.

Do not "fix" it to the intuitive polarity without deciding what should happen on a
server with `doWeatherCycle false`, where vanilla itself shows no rain after a bare
`/weather rain`. There is a test
(`start_raining_zeroes_the_level_and_stop_raining_fills_it`) whose whole job is to
fail loudly and send you here.

### 2. Thunder is always multiplied by rain

`WeatherState::thunder_level()` is the composed value; `raw_thunder_level()` is the
wire field. Reading the raw one into a darkening term blacks out a clear sky the
moment a server sends a stale non-zero thunder level — which it does on **every
join**, because `PlayerList.java:654-656` sends all three unconditionally.

### 3. Every colour operation here is in gamma space

Vanilla is not colour-managed: `ARGB.scaleRGB` multiplies 0–255 byte channels
(`ARGB.java:108-115`) and `ARGB.srgbLerp` interpolates them (`:155-161`).
`FogSettings`' colours are **linear**, so use `weather_darken_linear` /
`lightning_flash_linear`, which own the round-trip. Doing it in linear light pulls
both scale factors toward 1.0; measured at linear 0.2, the correct answer is 0.0466
and the linear-space answer is 0.1 — more than twice as bright, and an "it got
darker" assertion passes on either. `darkening_in_linear_light_would_be_measurably_too_weak`
pins both hypotheses.

## What reaches pixels, and what does not

Reaching pixels from the wire, end to end:

* **Rain and snow droplets, correctly chosen per column.** Angled camera-relative
  quads, 441 columns at the default radius, density scaled by the rain level,
  depth-tested against terrain, alpha-blended, no depth write. `ShellWeatherProbe::
  precipitation` (`app.rs`) resolves the standing biome per column (`ClientHandle::
  section_at` + `ChunkSection::biome_at_block`) and looks its climate up in
  `net::BiomeClimateCell`, folded from `ClientEvent::BiomeClimates` at `Login`
  exactly as `WeatherCell` folds `WeatherChanged`; vanilla's own threshold decides
  the split (`Biome.java:176`, height-adjusted temperature `>= 0.15` is rain, below
  is snow). See "Snow: closed" below for the exact hop and its live gate.
* **Sky, horizon, terrain fog and the below-horizon clear colour**, darkened by both
  rain and thunder — one composition, so the four can never disagree.
* **The lightmap**, via the existing `sky_darken` lane, so terrain, mobs *and* the
  first-person arm all darken together under a storm.
* **The lightning flash**: the sky tints blue-white and the lightmap goes full-bright
  for 5 ticks per bolt.

Not reaching pixels, each for a named reason:

| gap | blocker |
|---|---|
| **Rain ambience** | `Sim`'s `ShellAudio` is private with no public play method. |
| **Lightning bolt geometry** | The bolt's own model; deferred (see "Deferred"). |
| **Rain splash particles** | `ClientLevel.tickWeatherEffects`' `ParticleTypes.RAIN`; needs the per-column heightmap below. |
| **Per-column terrain height and `canSeeSky`** | No `column_height` accessor on `ClientHandle`. |

### Snow: closed

**Both halves are closed.** Biome **holder ids**
reach the client two ways — the initial `level_chunk_with_light`'s per-section biome
container, and, since that session, a live update via `chunks_biomes`
(`World::merge_biomes`, `crates/lodestone-world/src/world.rs`) — and
`lodestone_world::ChunkSection::biome_at_block` reads either. The biome's *climate*
now reaches the client too:
[`ClientRegistries::biome_climates`](../crates/protocol/v770/src/packets/registry.rs)
decodes `has_precipitation`/`temperature`/`downfall` off the same `registry_data` entry
`biome_sky_colors` already reads (top-level fields, siblings of `attributes` — see
`Biome.ClimateSettings.CODEC`, `Biome.java:358-368`, **not** nested under `attributes`
the way `sky_color` is), and `ClientEvent::BiomeClimates` carries it out of the adapter
at the same `Login`-time point `BiomeVisuals` does.

That table is **not** a new field on `BiomeVisuals`, which the original plan below (kept
for the record) called for — `crates/lodestone-ecs/src/session.rs` destructures
`ClientEvent::BiomeVisuals { sky_colors }` with no `..`, so adding fields there is a
breaking change to a file outside this session's file ownership. `BiomeClimates` is a
sibling variant instead, routed `SHELL` (`crates/lodestone-model/src/event.rs`) exactly
like `WeatherChanged` — it needs no `ingest`/`session` arm, only the same
`net.rs`-`forward`-into-a-cell shape `WeatherCell` already uses (see the `Login`-time
proof in `crates/protocol/v770/tests/join_flow.rs`, and the live cross-check against
Mojang's own `worldgen/biome/*.json` files in
`crates/protocol/v770/tests/live_registry_data.rs::biome_climates_from_a_real_server_match_mojangs_own_biome_files`,
measured 66/66 biomes resolved and matching on the creative oracle).

**The probe: closed.** `ShellWeatherProbe::precipitation` (`crates/lodestone-shell/src/app.rs`)
no longer hard-codes `Rain`. `net::BiomeClimateCell` mirrors `WeatherCell` — a `Mutex<Vec<..>>`,
not lock-free atomics, since the whole table replaces once per `Login` rather than changing
per-tick — and is folded by `net::forward`'s `BiomeClimates` arm exactly where `WeatherChanged`
folds into `WeatherCell`. `ShellWeatherProbe::biome_precipitation` reads the block's biome via
`ClientHandle::section_at(pos, section_index)` + `ChunkSection::biome_at_block`, looks that
biome's climate up in the cell, and calls `height_adjusted_temperature` then
`precipitation_for_temperature` — both already implemented and unit-tested in
`lodestone-render`'s `weather.rs`, unchanged by this patch. Every unresolved hop (world not
loaded, section elided, climate table still empty, or the biome's own fields unresolved) falls
back to `Rain`, matching `sky_visible`'s own "absent data reads as open sky" rule.

Gated three ways: `net::tests::forward_folds_biome_climates_into_the_cell_without_using_the_channel`
(the fold, hermetic), `net::tests::a_real_frozen_biome_crosses_vanillas_own_rain_snow_threshold_and_a_dry_one_does_not`
(vanilla's own `0.15` threshold — `Biome.java:176` — against real frozen_peaks/desert values
copied from `.cache/mc/26.2/src/data/minecraft/worldgen/biome/`, hermetic), and
`app::tests::live_precipitation_matches_vanillas_own_threshold_for_real_biomes` (`#[ignore]`d,
against the survival oracle: connects through `ClientBuilder` directly, captures the real
`ClientEvent::BiomeClimates` off the raw stream, and cross-checks `ShellWeatherProbe::
precipitation` at real loaded columns against an independently-derived expectation — not a
call to `lodestone_render::weather`, to avoid the `decode(encode(x)) == x` trap. Measured live:
spawn biome 40 (temperature 0.8) answers `Rain`; biome 52 (temperature 0.2, height-adjusted to
~0.12) answers `Snow` — both branches genuinely reached in one run).

<details><summary>Original plan (superseded by the field-vs-variant finding above)</summary>

Both fields are already on the wire and already in the hermetic fixture —
`crates/protocol/v770/tests/registry_data.rs:227-228` asserts
`has_precipitation: Byte(1)` and `temperature: Float(0.8)` in a real entry. Closing
it is a `biome_climates()` table alongside `biome_sky_colors()`, one new
`ClientEvent::BiomeVisuals` field, and a `ShellWeatherProbe::precipitation` that
consults it. **Not** a new packet and **not** a proxy.

</details>

## How to change it

* **Constants and geometry**: `crates/lodestone-render/src/weather.rs`. Pure, no GPU,
  no world access; 20 unit tests.
* **The pass**: `crates/lodestone-render/src/weather_pipeline.rs` and
  `src/shaders/weather.wgsl`.
* **Wire → state**: `net.rs`'s `forward`, and `WeatherCell` above it.
* **Per-frame composition**: `app.rs`'s `WeatherTracker`, `ShellWeatherProbe` and
  `weather_columns_for_frame`, plus the `desired_fog` block in `redraw`.
* **Install**: `resources::load_weather_textures` →
  `RenderState::install_weather`, at both connect paths in `app.rs`.

Gotchas beyond the three above:

* **The pass draws inside the existing block pass**, after the particles and before
  the outline. It needs a depth buffer already holding every opaque surface (or rain
  shows through walls) and must not write depth (or overlapping columns punch holes
  in each other). Vanilla uses a dedicated `WEATHER_TARGET` because it feeds a
  transparency-sorting chain we do not have.
* **Depth compare is `LessEqual`, flipped from vanilla's `GREATER_THAN_OR_EQUAL`** —
  we use DirectX-style `[0, 1]` depth, not reversed-Z.
* **Two bind groups only** (camera, texture). That is why the per-column light term is
  resolved on the CPU instead of binding a lightmap as vanilla does: the model shader
  is already at wgpu's 4-group floor and headroom is worth keeping.
* **Rain and snow are two draws over one buffer**, which only works because
  `extract_columns` sorts rain-first. A `rain_count` taken from a differently ordered
  list textures snow as rain with no error anywhere.
* **`AddressMode::Repeat` is load-bearing**, not a default: the whole animation is a
  U/V offset running far outside `0..1`.
* **The animation phase is driven by the tick clock**, never frame time. A frame
  counter makes the fall speed frame-rate dependent — the exact defect
  `entities.rs`'s `limb_swing_tracks_per_tick_travel_not_the_interpolation_gap`
  records for the walk cycle.
* **A ramping rain level re-uploads the fog uniform every tick**, not only on a
  water crossing. Intended: the ramp is ±0.01/tick over ~100 ticks, and a
  change-detected upload that ignored it would render a storm at clear-sky colours.

## Deferred

* **Lightning bolt geometry.** The flash is done; the visible bolt is not. It needs
  `LightningBoltRenderer`'s own procedural geometry (a seeded branching quad strip,
  regenerated per flash from `LightningBolt.seed`), which is more work than everything
  else in this doc combined and shares no code with any existing pass. Scoped out
  deliberately rather than half-built.
* **Repeat flashes per bolt.** Vanilla re-flashes `rand(3) + 1` times by resetting the
  entity's `life` (`LightningBolt.java:47`, `:131-134`). We see only the spawn, so one
  flash per bolt. `LIGHTNING_FLASH_TICKS` documents this.
* **Rain ambience.** The cadence is written and tested
  (`lodestone_render::RainAmbience`, vanilla's `rainSoundTime` including the
  above/below split); it has no producer. One `pub fn play_local_sound` on
  `crate::sim::Sim`, forwarding to `self.audio` exactly as the `NetUpdate::Sound` arm
  at `sim.rs:4722` already does, is the whole remaining wiring. `weather.rain` resolves
  to 8 real samples (`ambient/weather/rain{1..8}.ogg`, confirmed present in the 26.2
  asset index), so vanilla's repeated-one-shot cadence needs no looping API at all —
  a true loop would be *less* faithful, because it would lose the sample variation.

## Configuration

* `lodestone_render::DEFAULT_WEATHER_RADIUS` (10 columns each way = 441 columns).
  Vanilla's `weatherRadius` option, already present in this shell's options menu as
  "Weather Effect Radius" (`menu/options.rs:496`) but not yet read by this pass. Must
  stay below `HALF_RAIN_TABLE_SIZE` or a column indexes outside the offset table;
  `extract_columns` clamps rather than panicking.
* `textures/environment/rain.png` and `snow.png` from `client.jar`. Absent — a
  jar-less run — means no droplets, but the darkening still works, because that half is
  composed from scalars and needs no textures.

## Dependencies

* `lodestone-render`: `crate::fog` (the gamma round-trip helpers `multiply_gamma`,
  `linear_to_srgb_f32`, `srgb_to_linear_f32`), `crate::light` (the lightmap term),
  `crate::Camera`, `lodestone-assets` (`Image`, `ResourceManager`), `wgpu`, `glam`,
  `bytemuck`.
* `lodestone-shell`: `lodestone-render`, `crate::net` (the cell and
  `entity_light_at`), `crate::resources` (jar IO).
* Protocol: `GAME_EVENT` and `ADD_ENTITY` on v770, already decoded — see
  `crates/protocol/v770/tests/world_events.rs`.
