# Situational music selection

## What it is

The layer that decides *when* to play a music track and *which* one — menu music,
per-biome overworld music, the creative and underwater variants, and the End boss
and credits tracks. It is pure logic over the player's situation plus a randomised
countdown, and it lives in `crates/lodestone-sound/src/music.rs` with a generated
biome table beside it in `src/biome_music.rs`.

The sound *engine* was never the gap. `SoundRegistry`/`SoundResolver` have always
resolved any named event to a decoded `.ogg`; nothing ever asked for a music event.
This is the thing that asks.

## How it works

Three types and one state machine, all transcribed from decompiled 26.2.

| our type | vanilla | cite |
|---|---|---|
| `music::Music` | `Music` record | `Music` |
| `music::musics::*` | `Musics` constants | `Musics` |
| `music::BackgroundMusic` | `BackgroundMusic` record | `BackgroundMusic` |
| `music::MusicFrequency` | `MusicManager.MusicFrequency` | `MusicManager.MusicFrequency` |
| `music::MusicSituation` | `Minecraft.getSituationalMusic` inputs | `Minecraft.getSituationalMusic` |
| `music::MusicManager` | `MusicManager` | `MusicManager` |

### 26.2 does not read music off the biome

This is the restructure most likely to be got wrong from memory, and older
tutorials describe the old shape. There is no `biome.getBackgroundMusic()`.
`Minecraft.getSituationalMusic` asks the camera's **environment-attribute probe**
for `EnvironmentAttributes.BACKGROUND_MUSIC`, registered as
`audio/background_music`, and a biome
contributes music by *setting that attribute*. Selection order:

1. The open screen's own `getBackgroundMusic()`, if any — wins outright, and also
   forces music volume to `1.0` (`Minecraft.getMusicVolume`).
2. Otherwise, with a player: `END_BOSS` if the dimension is the End *and* the boss
   overlay wants music; else the probed `BackgroundMusic.select(creative, underwater)`,
   which may be `None`.
3. Otherwise (no player — the title screen): `MENU`.

`creative` is **`instabuild && mayfly`** (`Minecraft.getSituationalMusic`), not
`gamemode == creative`. A gamemode check gives spectators the creative track.

`BackgroundMusic::select` precedence is **underwater, then creative, then default**,
and a specific slot falls back to `default` only
when *absent*. Both halves are easy to invert and the symptom is narrow — the wrong
track while swimming in creative.

### The delay constants

These are load-bearing: wrong values are invisible in a short test and obvious in
play. Every one is asserted in `tests/music_selection.rs`.

| music | min | max | replaces | cite |
|---|---|---|---|---|
| `MENU` | 20 | 600 | yes | `Musics.MENU` |
| `CREATIVE` | 12000 | 24000 | no | `Musics.CREATIVE` |
| `CREDITS` | 0 | 0 | yes | `Musics.CREDITS` |
| `END_BOSS` | 0 | 0 | yes | `Musics.END_BOSS` |
| `END` | 6000 | 24000 | yes | `Musics.END` |
| `UNDER_WATER` | 12000 | 24000 | no | `Musics.UNDER_WATER` + `Musics.createGameMusic` |
| `GAME` | 12000 | 24000 | no | `Musics.GAME` + `Musics.createGameMusic` |

Note `END`'s min is `FIVE_MINUTES`, not the game tracks' `TEN_MINUTES`; and
`END_BOSS`'s event is `music.dragon` (`SoundEvents.MUSIC_DRAGON`), not
`music.end_boss`.

`MusicFrequency` converts minutes to ticks as `minutes * 1200`
(`MusicManager.MusicFrequency`'s constructor) → 24000 / 12000 / 0, then
`getNextSongDelay` has three distinct behaviours, two of which are
easy to lose:

- `music == None` → the raw cap, **unrandomised**.
- `Constant` → a flat `STARTING_DELAY` (100), **ignoring** its own cap of 0. Reading
  "0 minutes" literally restarts music every tick.
- otherwise → `Mth.nextInt(rng, min(min_delay, cap), min(max_delay, cap))`,
  **inclusive at both ends** (`Mth.nextInt`).

A consequence worth knowing: at `Frequent` a game track's range collapses to
`12000..=12000`, so the delay is exactly 12000 and **no random draw is consumed**.

### Two orderings inside the tick that read like bugs

Both are faithful and both are pinned by gates.

1. **A replacing selection consumes two draws and takes the smaller.** Vanilla stops
   the old track and sets `nextSongDelay = nextInt(0, min_delay/2)` (in `MusicManager.tick`),
   but does
   **not** clear `currentMusic`; the next `if`, also in `MusicManager.tick`, therefore sees an inactive
   track and `min`s the delay again with `getNextSongDelay`. Both land in one tick.
   Reordering changes the timing *and* the random stream.
2. **While a track plays, the countdown parks at `max_delay`, not `i32::MAX`.**
   `startPlaying` sets `i32::MAX`, but the clamp in `MusicManager.tick` is *outside* the
   `currentMusic != null` block, so from the very next tick it is `min(MAX, max_delay)`.
   It then holds there because the decrement, also in `MusicManager.tick`, is guarded on nothing playing.
   An `assert_eq!(delay, i32::MAX)` a tick later fails against real vanilla — that is
   how this was found.

## How to change it, and the gotchas

### A missing track is silence — and that is a shipped configuration, not a corner

`cargo xtask fetch-sounds` **excludes music by default**: 70 tracks + 22 records,
293.23 MB, only with `--all`. Measured on a normal checkout: **0 of 70** music
objects present. So every track this layer can choose is usually absent.

The degradation is vanilla's own, not a bespoke path. `MusicManager.startPlaying`
assigns `currentMusic` *before* playing and switches on the result:
`STARTED_SILENTLY` simply skips the toast. Next tick the
`!isActive` branch clears it and re-arms a normal 12000..=24000 countdown. So
`MusicStart::Silent` recovers through the ordinary "track finished" path — no panic,
no `unwrap`, no blocking wait, and **no busy loop**.

That last clause is the one a weak gate misses, so it is asserted as a *count* of
start attempts over a fixed tick budget (immune to machine load), with a negative
control that makes the absence panic and fails the gate.

**And it is not only about unfetched assets.** Exactly one of 26.2's 54 music events,
`music.nether.warped_forest`, ships with an **empty `sounds` array**, so the warped
forest plays no music even with the full `--all` corpus. Treating an unresolvable
music event as an error would break a vanilla biome. `tests/music_assets.rs` pins
that set as an equality, so if Mojang fills it in we hear about it.

### Music must be streamed, never eagerly decoded

`SoundResolver::resolve_instance` calls `decode_vorbis` and **caches the PCM
forever**. That is right for a footstep and catastrophic for a track:
`music/game/end/the_end.ogg` is 10.76 MiB compressed and **304.33 MiB resident**
decoded, and the eight largest music/record objects are 130–300 MiB each — against a
measured world-layer budget of 77.6 MiB.

Vanilla says so in the data: **all 316 music leaf entries in the real `sounds.json`
declare `"stream": true`** (checked by `tests/music_assets.rs`). `ResolvedSound.stream`
had been parsed by `lodestone-assets` all along and ignored.

Use **`SoundResolver::resolve_streaming`**, added for this, which resolves to a
`StreamingSound { stream: VorbisStream, .. }` without decoding, and returns
`Ok(None)` rather than `DriverError::MissingFile` for absent bytes.

### The biome table is generated, not hand-written

`src/biome_music/table.rs` is produced from
`crates/lodestone-server/assets/worldgen/biome/*.json` — 66 vanilla-derived files
already in this repo, of which 42 carry `minecraft:audio/background_music`. Nothing
read them before: every consumer of `EmbeddedResolver::biome_document` looks only at
`carvers` and `features`, so the whole `attributes` map was parsed by no Rust code.

```bash
LODESTONE_REGEN=1 cargo test -p lodestone-sound --test biome_music_table
```

`tests/biome_music_table.rs` fails if the committed table has drifted, and *separately*
checks the JSON against `Musics.java`, so a wrong asset dump cannot launder itself
through a regenerated table. The generator exists because the alternative precedent —
`lodestone-assets`' hand-transcribed `BIOME_EFFECTS` in `tint.rs` — has no oracle
outside itself.

**`None` and `Some(EMPTY)` mean different things.** A biome absent from the table adds
nothing and should fall back to `BackgroundMusic::overworld()` (helper:
`overworld_music_for`). `pale_garden` is present-but-empty and means *genuinely no
music* (`OverworldBiomes.darkForest`, which builds the pale garden variant too).
Collapsing them gives pale garden the overworld
track and silences 24 biomes.

### Nothing in this layer can make a sound

`music.rs` and `biome_music.rs` hold no sink, no device, no clock and no filesystem;
everything audible goes through the caller's `MusicSink`. That is enforced, not just
stated: `the_music_modules_cannot_reach_a_device_or_a_clock` scans both files for
`AudioEngine`, `cpal`, `Instant`, `std::fs` and `Command::new` in code position.

This matters because a test that opens an output device would be *audible on the
owner's machine on every `cargo test`*, and no health check in this repo can see that
— the suite passes. The precedent is real: an accounts-screen unit test here spawned
`Command::new("open")` and opened a Microsoft OAuth URL in the owner's browser on
every `cargo test -p lodestone-shell` run.

## Configuration

- `MusicFrequency` — the "Music Frequency" option. The row already exists in
  `crates/lodestone-shell/src/menu/options.rs`'s `SOUND` table but is **inert** (`live: None`),
  as are all eleven `soundSource.*` volume sliders; `config::Options` has no volume
  field at all. Wiring them is not part of this.
- `minecraft:audio/music_volume` — per-biome, default 1.0. Only `pale_garden` sets it
  (to 0.0), which *fades* music out over ~303 ticks rather than cutting it.
- `cargo run -p xtask -- fetch-sounds --all` — required for any music to be audible.

## Dependencies

- `lodestone-audio` — `JavaRandom` (vanilla's `LegacyRandomSource` semantics) and
  `VorbisStream`.
- `lodestone-assets` — `SoundRegistry` resolution, via `SoundResolver`.
- `crates/lodestone-server/assets/worldgen/biome/*.json` — the generated table's
  oracle. A *test-time* path only; the shipped crate embeds plain `const` data and
  needs no JSON parser.

## What is still open

- ~~**No caller yet.**~~ **Wired.** Both call sites now exist:
  `crates/lodestone-shell/src/audio/music.rs` owns `ShellMusic` (the `MusicManager`,
  its `JavaRandom`, and the sink's sticky flag), inserted as the `MusicState`
  resource beside `AudioEngine` in `sim/build.rs` and ticked through
  `Sim::tick_music`. `app/menus.rs::draw_menu` drives it with `menu_situation()`
  (`in_world: false`, so `situational_music` selects `musics::MENU`) and the world
  redraw path drives it with `world_situation(..)`.

  Three things worth carrying forward from that wiring:

  - **The call sites are gated structurally**, because they cannot be reached from a
    unit test (`draw_menu` needs a window and a swapchain). `both_production_call_sites_actually_call_tick_music`
    scans both files for a non-comment `tick_music(` call, with a positive control
    proving the scanner reads real code. Deleting either call fails it by name —
    the "remove the call site and observe zero" control, made standing.
  - **Ticking per frame would be wrong.** `MusicManager::tick` decrements the delay
    by one tick per call, so calling it once per rendered frame advances vanilla's
    bookkeeping ~3x too fast at 60 Hz. `ShellMusic::advance` accumulates wall time
    into whole 20 Hz ticks, capped at 10 catch-up ticks for the same reason
    `app::pacing` caps its own.
  - **`ShellMusic::tick` takes an explicit tick count**, which is what lets the
    gates assert *counts* rather than durations — a "started within N ms" test is
    the sequential-duration trap that has already flaked a gate in this repo.

- **Still no sound, and the remaining gap is in `lodestone-audio`.**
  `ShellAudio::start_music` resolves through the new
  `AudioEngine::resolve_music` (the **streaming** path — `resolve_instance` caches
  decoded PCM and `the_end.ogg` is 304 MiB decoded) and then **drops the stream**,
  because `Mixer` has no streaming-voice API: its `SoundInstance` takes a fully
  decoded `Arc<PcmBuffer>`. `VorbisStream` exists and is unwired. So selection and
  request are closed and the last mile is open — a streaming voice in `Mixer` is the
  whole remaining work, and when it lands music plays with no change here.

  Note this is *doubly* silent in an ordinary checkout, and the second reason is
  intended: `cargo xtask fetch-sounds` excludes music, so 0 of 70 music objects are
  on disk and `resolve_streaming` returns `Ok(None)`. Silence is the correct
  default; `--all` adds 92 objects / 293 MB.

- **In-world selection reaches the 42-biome table.** `Sim::background_music`
  resolves the standing biome out of the chunk section's palette against the
  `BiomeNameCell` registry snapshot (`Sim::standing_biome_name`, the same hop
  `Sim::biome_sky_color` makes — the biome is not on the wire) and looks up its
  three-slot record; `Sim::music_volume` reads the biome's `audio/music_volume`
  alongside it.

  The fallback is **dimension-specific** on purpose, which is why this does not call
  `overworld_music_for`: the Nether's biomes all set the attribute explicitly, so a
  Nether biome with no row falls back to `BackgroundMusic::EMPTY` rather than to the
  overworld's default track. Collapsing both to `overworld()` would play overworld
  music in the Nether whenever a biome row was missing.

- **Ambience is wired** — see [ambient-sounds.md](./ambient-sounds.md). Cave
  ambience, the biome/dimension loop and the rain cadence all tick from
  `Sim::tick_ambience`, and looping playback gained the three primitives it needed
  (`Mixer::set_voice_volume`, `Voice::set_instance_volume`,
  `AudioEngine::play_loop`). Unlike music these are ordinary short events, not
  streams, so nothing here waits on a streaming voice.
- **Biome attributes over the wire.** On a real vanilla server the attribute is
  `syncable()` and arrives in the biome registry NBT, which
  `crates/protocol/v770/src/packets/registry.rs`'s `biome_sky_color` already demonstrates how to
  read. That would let a datapack override music; the generated table would remain the
  singleplayer fallback, since the integrated server sends no registry data at all.
