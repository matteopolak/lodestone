# Sound: playback, subtitles, ambience and music

## What it is

The client audio layer end to end: from a server sound packet to the speakers, the accessibility subtitle overlay, biome and cave ambient loops, client-predicted local sounds (footsteps, block break/place) and situational music. The mixing engine (`lodestone-audio`) and event registry (`lodestone-sound`/`lodestone-assets`) are the stable core; this doc covers what sits around them.

## How it works

### Playback chain

`SOUND`/`SOUND_ENTITY` decode to `ClientEvent::Sound`/`EntitySound`, then `net.rs`'s `forward` (the only router for these two events: a sound is neither per-entity ECS state nor a session scalar, so it must not gain an arm in `ingest::handles_event` or `session::handles_event`), then `ShellAudio::play_sound`, `lodestone-sound`'s weighted event resolution and `lodestone-audio`'s decode, mix and spatialise.

`STOP_SOUND` runs the reverse way. The mixer stops only an opaque handle while the packet has optional sound-name and category filters, so `ShellAudio` indexes handles of server-packet voices by those two fields. A stop cancels every match (a missing field is a wildcard; both missing stops every tracked server voice). Locally predicted sounds and ambience are deliberately not indexed: their producers own their lifetime.

Why it was silent despite every stage working:
1. The sample corpus is not in `client.jar`. `sounds.json` and its 4,871 `.ogg` files live in the launcher's content-addressed asset-object store (`asset-index-<id>.json` maps names to `{hash, size}`; bytes at `objects/<hash[0..2]>/<hash>`). `xtask fetch-assets` alone yields the registry with 11 of 4,871 samples, so the engine resolves events, finds no object and plays nothing while logs say audio is enabled. `xtask fetch-sounds` (~80 MB) is the separate command. A startup census warns on zero samples and a one-shot warning (debug afterwards) fires the first time a sound cannot play.
2. One resolver for the asset directory: audio, pack, atlas and fonts all use `lodestone_mc_cache::cache_root` (`LODESTONE_ASSETS`, else `.cache/mc/<current version>`). An explicitly set variable is used verbatim, never silently replaced by a scan.

### Which sounds are audible

One rule: whether the reference server passes an excluded player to its play-sound call. Broadcast sounds (mob idle/hurt/death, chest lids, pickup, other players' placements, cascading breaks via `LEVEL_EVENT` 2001, explosions via their own packet) play. Your own placement, mining and footstep sounds are predicted client-side because the server excludes the acting player and relies on that client to play them; another player's own breaks and footsteps are silent in the reference too. `LEVEL_EVENT` 2001 spawns debris particles and also a local break sound using the block's `sound_types` census ([`blocks.md`](./blocks.md)) and the `(volume+1)/2`, `pitch*0.8` scaling, never retyped since the same expression appears at both original call sites.

- Eating and drinking play each bite on the use tick (drinks included, particles or not); the integrated server sends it to others excluding the eater. The louder completion sound stays server-owned.
- The census lookup takes `lodestone_data::block_states::StateId`. Network level events keep a `BlockStateRef` tag until this boundary (canonical values validate with `StateId::new`; protocol-local values stay silent until their adapter maps them); predicted placement and footsteps validate raw values first. Every one of 32,366 states has a total `BlockSoundType` lookup, and each row's five event references are validated `SoundEventId`s. A packet holder validates its positive reference after subtracting one (unknown ids rejected), while a zero-form inline definition keeps its supplied key. Surface helpers return `Option` because `minecraft:intentionally_empty` means deliberately no sample.
- The explosion sound was missing because v26-2 never decoded packet id 36 (`minecraft:explode`). Volume and pitch are rolled client-side from the packet's particle roll, so the decoder rolls the same die. Shockwave, smoke and debris particles from the packet remain unimplemented.

### Corpus policy (`xtask fetch-sounds`)

Derived from `sounds.json` itself, never a file list: every event's sample names are walked, skipping `"type": "event"` indirections. A sample is excluded only when every event referencing it is a music event (`music.*`, `music_disc.*`; "every" because a jukebox record is referenced by both a music event and `jukebox.play`). Default: 4,751 objects, 80.14 MB, covering every non-music selectable sample including all six biome ambience loops; `--all` adds the 92 music/record objects (293.23 MB). The reference's `"stream": true` flag selects only 98 samples but would drop the nether and underwater loops, so it is not used.

The registry validates each event key at parse time through the shared resource-location rules into a typed map key; the default `minecraft:` namespace is canonicalized to the bare path and custom namespaces stay qualified. Malformed keys fail the load.

### Subtitles

A stack of right-aligned plates fading white to grey over 3 seconds, arrow-annotated for sounds from behind. `SoundEvent.subtitle` is read before weighted sample selection (selection consumes an RNG roll and subtitles belong to the event; reading after wastes a roll and desyncs the seeded pick every client shares). The hook is `ShellAudio::play_sound`, the single choke point, consuming the engine's `SoundPlayback` (listener distance, source volume, category/master/runtime gain and attenuation policy exactly as the renderer uses them). Zero gain (exact attenuation edge, or muted) produces no caption; relative UI sounds always caption (infinite range). Backwards-feeling details: the fade is brightness (RGB 255 to 75), not alpha, so an old caption goes grey on an opaque plate; every plate is the same width (max text plus both arrow glyphs); the text is centred within it while the plate is right-aligned.

### Ambient sounds and prediction

- The ambient light probe passes `lodestone-sound` a `LightSample` of two `LightLevel`s, each exactly one packed nibble (`0..=15`); `LightSample::from_packed_nibbles` unpacks world data and synthetic callers use `LightLevel::new`, `ZERO`, `MAX`. Raw signed integers once let an unrelated value change the sky divisor or cross the block-light break-even at one; keep the range check at this boundary.
- Ambience has two layers that override rather than merge: the dimension sets the cave mood default (`ambient.cave`) and a biome can replace loop, mood and additions (the Nether dimension sets nothing, so its five biomes supply everything). `biome_ambient::ambient_sounds_at(dimension, biome)` composes both (biome-only finds mood in zero biomes; dimension-only gives Nether biomes mood and no loop).
- Rain and snow: `ShellAmbience` keeps the weather one-shot at the sampled landing block centre so the mixer applies distance and panning, on `SoundCategory::Weather` (separate slider and gain). Rain is `weather.rain`; snow is `block.snow.fall`. The shell resolves the listener column's `MOTION_BLOCKING` heightmap and the biome's climate: an unloaded column or unknown climate suppresses the event; a landing above the listener uses the muffled-above gain and pitch, at or below the normal one.
- Mood triggers on darkness, not depth. Each tick one block is sampled from a 17^3 cube around the eye: any sky light drains moodiness, block light above 1 also drains, only 0 or 1 accumulates and only 0 at full rate. A lit room at Y=-40 accumulates nothing; an unlit box at Y=200 needs 6,000 dark samples and fires on tick 6,001 (accumulating a rounded-down `1/6000` undershoots 1.0 in any binary float, so no precision change makes it 6,000).
- Loop crossfade is a 40-tick linear fade and several loops can be live (crossing a border keeps both voices); a single slot gives an audible seam.
- Predicted sounds are the ones the player entity calls with itself as the argument (footsteps, muffled steps, swim sounds). Attacks and level-ups go through the reference's server-side-only path and are not predicted (predicting would double every hit sound). The reference needs no de-duplication because the server omits that client from the broadcast; `PredictionLedger` is defence in depth (`lodestone-server` sends no sound packets today).
- Footsteps are spaced by distance: distance is scaled by 0.6 and accumulated, firing at integer thresholds (first step at about 1.667 blocks), re-arming to the next integer (`(int)dist + 1`, not `dist + 1`, which drifts).

### Situational music

26.2 reads music through the camera's environment-attribute probe for `BACKGROUND_MUSIC`, and a biome contributes by setting that attribute (not a direct biome read). Order: the open screen's own music; else with a player, `END_BOSS` in the End (if the boss bar wants music) or `BackgroundMusic::select(creative, underwater)` (possibly nothing); else (title screen) `MENU`. `creative` is instabuild and may-fly, not a game-mode check (which gives spectators the creative track). Precedence is underwater, creative, default, falling back to default only when a specific slot is absent.

In the shell both inputs come from `redraw.rs` through `audio::music::world_situation`: `end_boss_active` is `Sim::music_end_boss_active` (End dimension plus a boss bar's play-music flag in `BossBarSet`), and `level_loading` is true under the world-wait or dimension-change cover, stopping the countdown so no track starts under it.

Delay randomisation has three behaviours: `music == None` uses the raw cap; `Constant` uses a flat start of 100 regardless of its cap (a literal 0 minutes would restart every tick); otherwise a draw inclusive at both ends. Two faithful oddities: a track change consumes two RNG draws in one tick (the halved delay is re-derived because the "playing" flag was not cleared first), and the countdown while a track plays parks at `max_delay`, not `i32::MAX`. `MusicDelay` validates only non-negative ticks; keep `MusicFrequency` in minutes and convert via `MusicDelay::ticks` only where the scheduler needs arithmetic.

Music is streamed, never eagerly decoded (one track is over 300 MiB resident against an 80 MB corpus; all 316 music entries declare `"stream": true`). A missing track (default corpus excludes all 70 tracks and 22 records) degrades to silence through the started-silently path: no panic or busy loop, the ordinary retry re-arms. `music.nether.warped_forest` ships with an empty sample list even in the full corpus (silent in the reference too). The biome table distinguishes "no row" (overworld default) from "present, empty row" (`pale_garden`: no music); it is generated from the bundled biome JSON and cross-checked against the reference's registration so a wrong dump cannot launder itself through regeneration.

## How to change it

- A server sound source needs no client change.
- A server sound path that must obey `STOP_SOUND` must call `ShellAudio::play_server_sound` or `play_server_entity_sound`, not the local-prediction methods (only the retained handle lets the filters reach an audible voice).
- A predicted sound needs its producer call site; seed from `Sim::block_sound_seed` (a `splitmix64` over block position and frame tick), never `Instant::now` (panics on wasm) or the particle engine's RNG (shifting it breaks golden pixel gates).
- A head-relative sound (UI clicks): `Sim::play_relative_sound`, not a positioned call with a guessed position.
- Corpus policy: `xtask::plan_sound_corpus`, derived from `sounds.json`.
- Browser sound set: add the event to `CURATED_EVENTS` in `web/scripts/stage_sounds.py`.

## Configuration

Native: `LODESTONE_ASSETS` or an ancestor walk for `.cache/mc/<current version>` via `asset_objects::discover_store_root`; `cargo run -p xtask -- fetch-sounds [--all]` populates the corpus. Browser: no env var; sounds are staged at build time (`web/Trunk.toml` `post_build`, fail-open) and fetched into a `MemorySource`, gated behind a user gesture.

## Dependencies

`lodestone-sound` (registry resolution, weighted selection, device backends: `cpal` natively, a `web_sys::ScriptProcessorNode`-driven `Mixer` in the browser), `lodestone-audio` (Ogg Vorbis decode/stream, mixing, spatialisation, `JavaRandom`), `lodestone-assets` (`SoundRegistry`, `Language`), `crate::asset_objects`, `lodestone-render::Camera`. `xtask` needs `curl`; browser staging needs Python 3.
