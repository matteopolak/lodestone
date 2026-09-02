# Sound: playback, subtitles, ambience and music

## What it is

The client audio layer end to end: the path from a server sound packet to the
speakers, the accessibility subtitle overlay, biome/cave ambient loops and
client-predicted local sounds (footsteps, block break/place), and situational
music selection. The mixing engine (`lodestone-audio`) and the event registry
(`lodestone-sound`/`lodestone-assets`) were built and correct from early on;
this doc is mostly about what sits either side of them.

## How it works

### Playback: the chain, and why it was silent

`SOUND`/`SOUND_ENTITY` decode → `ClientEvent::Sound`/`EntitySound` →
`net.rs`'s `forward` (the **only** router for these two events — a sound is
neither per-entity ECS state nor a local-player session scalar, so it must
not gain an arm in `ingest::handles_event` or `session::handles_event`) →
`ShellAudio::play_sound` → `lodestone-sound`'s weighted event resolution →
`lodestone-audio`'s decode/mix/spatialise. Two separate things kept this
silent even though every stage above existed and worked:

1. **The sample corpus is not in `client.jar`.** `sounds.json` and its 4,871
   `.ogg` files live in the launcher's content-addressed asset-object store
   (`asset-index-<id>.json` maps a logical name to `{hash, size}`; bytes sit
   at `objects/<hash[0..2]>/<hash>`). `xtask fetch-assets` alone gets the
   registry with **11 of 4,871** samples on disk — the engine resolves every
   event, finds no object, and plays nothing, with every log line saying
   audio is enabled. `xtask fetch-sounds` (~80 MB) is the second, separate
   command. A startup census warns when zero samples are present, and a
   one-shot warning (dropping to debug afterwards) fires the first time a
   sound cannot be played, so one bad event cannot flood the log.
2. **Two environment variables named one directory.** Audio required
   `LODESTONE_ASSET_ROOT`; the rest of the shell honoured `LODESTONE_ASSETS`.
   `discover_store_root` is now the single resolver (asset root, then
   assets, then an ancestor walk for a real `.cache/mc/*` store), and an
   explicitly-set variable is used verbatim rather than silently replaced by
   the walk on failure — otherwise a typo hides behind a working default.

**Which sounds are audible follows one rule**: whether vanilla's server
passes an *excluded* player to its own play-sound call. Broadcast-to-all sounds
(mob idle/hurt/death, chest lids, item pickup, another player's placements,
cascading block breaks via `LEVEL_EVENT` 2001, explosions via a dedicated
packet) all play. **Your own** placement/mining/footstep sounds are
predicted client-side, because vanilla excludes the acting player from the
broadcast and relies on that same client to play them locally — so another
player's own mined break or footsteps genuinely are silent in real vanilla
too, not just here. `LEVEL_EVENT` 2001 was previously only spawning debris
particles, not vanilla's *other* half (a local break sound) — both are wired
now, using the block's `sound_types` census (see
[`docs/blocks.md`](./blocks.md)) and vanilla's own `(volume+1)/2`,
`pitch*0.8` scaling, which must never be retyped since the identical
expression appears at both of vanilla's own call sites.

The explosion sound was missing for a structural reason, not a routing gap:
v26-2 never decoded packet id 36 (`minecraft:explode`) at all, so there was
nothing to forward. The explosion's pitch is **rolled client-side** from the
packet's own particle roll (vanilla sends the sound but not a fixed
volume/pitch — both are rolled locally on receipt), so the decoder rolls the
identical die rather than inventing a fixed value. The shockwave/smoke and
block-debris particles this same packet carries remain unimplemented — only
the sound half is fixed here.

### Corpus policy (`xtask fetch-sounds`)

Derived from `sounds.json` itself, never a file list — every event's sample
names are walked and resolved to a path, with `"type": "event"` indirections
skipped (they resolve through their own target event). A sample is excluded
only when **every** event referencing it is a music event (`music.*`,
`music_disc.*`) — "every," not "any," since a jukebox record is referenced by
both a music event and an ordinary `jukebox.play` event, and an "any" rule
would drop it. Default fetch: 4,751 objects / 80.14 MB, covering every
sample any non-music event can select (mobs, blocks, items, steps, liquid,
UI, and all six biome ambience loops); `--all` adds the 92 excluded
music/record objects (293.23 MB). This is a measured choice, not vanilla's
own `"stream": true` flag — that flag selects only 98 samples but silently
includes the nether/underwater ambience loops, which the event-based
exclusion correctly keeps in the default fetch.

### Sound subtitle captions

Vanilla's accessibility overlay — a stack of right-aligned plates fading
white-to-grey over 3 seconds, arrow-annotated when the sound came from
behind. `SoundEvent.subtitle` is parsed from `sounds.json` and read **before**
weighted sample selection, deliberately: selection consumes an RNG roll and
subtitles are a property of the event, not the chosen sample, so reading
after selection would both waste a roll and desync the seeded pick every
client agrees on. The hook lives in `ShellAudio::play_sound`/
`play_entity_sound` — the single choke point every sound in the client passes
through — and records the caption **before** the engine call, so a resolve
failure (a missing `.ogg`) still surfaces a caption, matching vanilla's own
listener hook running off submission rather than decode success.

Three things read backwards from the obvious guess: vanilla fades
**brightness** (RGB 255→75), not alpha, so an old caption goes grey on an
opaque plate rather than translucent over the world; every plate is the
**same width** (max text width plus room for both arrow glyphs), so a row
without an arrow does not shrink; and the text is **centred** inside that
width even though the plate itself is right-aligned. Range is not modelled —
every caption here is treated as audible regardless of the sound's real
attenuation distance, since the sound was genuinely submitted to the mixer,
which is a stronger signal than a distance check would add.

### Ambient sounds and client prediction

Ambience comes from **two layers that override, not merge**: the dimension
sets the cave "mood" default (`ambient.cave`), and a biome can fully replace
loop/mood/additions — the Nether's dimension type sets nothing at all,
relying entirely on its five biomes to supply everything. A biome-only
lookup finds cave mood in **zero** biomes (concluding it doesn't exist); a
dimension-only lookup gives every Nether biome cave mood and none of its own
loop. `biome_ambient::ambient_sounds_at(dimension, biome)` composes both.

The mood (cave-ambience) trigger is **darkness, not depth** — the common
wrong guess is "Y below sea level." Each tick, one block is sampled from a
17³ cube around the player's eye: any sky light *drains* moodiness, and
block light above 1 also drains it (only at exactly 0 or 1 does it
accumulate, and only at 0 does it accumulate at full rate) — so a lit room
at Y=-40 accumulates nothing while an unlit box at Y=200 accumulates at full
rate, needing 6,000 consecutive dark samples (five minutes). It fires on
tick **6,001**, not 6,000, in both `f32` and `f64` — accumulating a
repeatedly-rounded-down `1/6000` step undershoots 1.0 in any binary float, so
this is not an `f32` artifact and no precision change can "fix" it back to
6,000 (vanilla lands on 6,001 for the identical reason).

Loop crossfade is a real **40-tick** linear fade, and more than one loop can
be live at once (crossing a biome border keeps both voices, fading one down
while the other comes up) — collapsing to a single loop slot produces an
audible seam at every border. Which sounds vanilla predicts client-side is a
**three-level method override**, not a list: only the sounds a `Player`
method literally calls with itself as the argument are locally predicted
(footsteps, muffled steps, swim sounds); attacks and level-ups are routed
through a method vanilla names `playServerSideSound` and are **not**
predicted — guessing attacks were predicted would double every swing's hit
sound. Vanilla needs no de-duplication logic at all, because the same
exclusion argument that makes a sound play locally is what makes the server
omit that client from the broadcast; this crate's own `PredictionLedger` is
defence in depth for a server that might one day forget the exclusion, not a
fix for anything reachable today (`lodestone-server` currently sends no sound
packets whatsoever).

Footsteps are spaced by **distance, not time**: travelled distance is scaled
by 0.6 and accumulated, firing at successive integer thresholds — so the
first step lands at `1/0.6 ≈ 1.667` blocks, steps speed up sprinting and stop
against a wall, and the threshold re-arms to the **next integer**
(`(int)dist + 1`), not `dist + 1` — with the latter, overshoot accumulates
and the spacing slowly drifts.

### Situational music selection

26.2 does **not** read music off the biome directly (older tutorials
describe the pre-restructure shape) — vanilla's own situational-music selector probes
the camera's environment-attribute system for `BACKGROUND_MUSIC`, and a biome
contributes by *setting that attribute*. Selection order: the open screen's
own music wins outright; otherwise, with a player, `END_BOSS` in the End (if
the boss bar wants music) or the probed `BackgroundMusic::select(creative,
underwater)`, which may resolve to nothing; otherwise (no player — the title
screen) `MENU`. `creative` here is **`instabuild && mayfly`**, not
`gamemode == creative` — a gamemode check wrongly gives spectators the
creative track. `BackgroundMusic::select` precedence is underwater, then
creative, then default, falling back to `default` only when a specific slot
is *absent* — inverting either half is a narrow, easy-to-miss symptom (wrong
track only while swimming in creative).

The delay-randomisation formula has three genuinely distinct behaviours:
`music == None` uses the raw cap unrandomised; `Constant` uses a flat
starting delay of 100 regardless of its own declared cap (reading "0
minutes" literally would restart music every tick); otherwise it draws
`nextInt` inclusive at both ends. Two orderings read like bugs and are
faithful: a track change consumes **two** RNG draws in one tick (vanilla
computes a halved delay, then immediately re-derives it because it forgot to
clear the "track playing" flag first), and the countdown while a track plays
parks at `max_delay`, not `i32::MAX` — the sentinel is set once and then
immediately reclamped on the very next tick.

Music must be **streamed, never eagerly decoded** — one track decoded
eagerly is over 300 MiB resident against an 80 MB compressed corpus, and all
316 real music entries in `sounds.json` declare `"stream": true`. A missing
track (the *default* corpus excludes all 70 tracks + 22 jukebox records,
293 MB, added only with `--all`) degrades to silence through vanilla's own
`STARTED_SILENTLY` path — no panic, no busy loop, the ordinary "track
finished" retry logic re-arms on the next check. One vanilla music event,
`music.nether.warped_forest`, ships with an **empty** sample list even with
the full corpus fetched — the warped forest plays no music in real vanilla
either. The biome table distinguishing "no row" (falls back to the overworld
default) from "a present, empty row" (`pale_garden` — genuinely no music,
not a fallback) is generated from the same biome JSON already bundled for
worldgen, cross-checked against vanilla's own biome-music registration in the decompiled source
so a wrong dump
cannot launder itself through a regenerated table.

## How to change it, and the gotchas

- Adding a server sound source needs no client change — any `SOUND`/
  `SOUND_ENTITY` packet already reaches the mixer.
- Adding a predicted sound: the producer (the call site that fires it) is
  the missing half; seed it from `Sim::block_sound_seed` (a `splitmix64` over
  block position and frame tick), never `Instant::now` (panics on wasm) and
  never the particle engine's own RNG stream (shifting that sequence would
  break unrelated golden pixel gates).
- A head-relative sound with no world position (UI clicks, `forUI`/`forMusic`
  in vanilla's own shape): `Sim::play_relative_sound`, not a positioned call
  with a guessed-near position.
- Changing the corpus policy: `xtask::plan_sound_corpus`, derived from
  `sounds.json`, never a hand-kept file list.
- Growing the browser's curated sound set needs no Rust change: add the event
  name to `web/scripts/stage_sounds.py`'s `CURATED_EVENTS`.

## Configuration

**Native**: `LODESTONE_ASSET_ROOT` (highest priority) / `LODESTONE_ASSETS` /
an ancestor walk for `.cache/mc/<version>`, resolved by
`asset_objects::discover_store_root`. **Browser**: no env var — sounds are
staged at build time (`web/Trunk.toml`'s `post_build` hook, fail-open) and
fetched at runtime into a `MemorySource`, gated behind a user-gesture (audio
contexts cannot start until one). `cargo run -p xtask -- fetch-sounds
[--all]` populates the native corpus.

## Dependencies

`lodestone-sound` (registry resolution, weighted selection, device backends —
`cpal` natively, a `web_sys::ScriptProcessorNode`-driven `Mixer` in the
browser); `lodestone-audio` (Ogg Vorbis decode/stream, mixing, spatialisation,
`JavaRandom` for vanilla-matching distributions); `lodestone-assets`
(`SoundRegistry`, `Language` for subtitle translation); `crate::asset_objects`
(the native asset-object store); `lodestone-render::Camera` (the listener
transform). `xtask` needs `curl`; browser staging needs a Python 3
interpreter at build time.
