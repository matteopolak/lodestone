# Sound playback

## What it is

The path from a server sound packet to the machine's speakers, and the two things
that keep it quiet: a **missing sample corpus** and a **missing environment
variable**. The mixing engine was already built and correct; this doc is mostly
about everything either side of it.

## How it works

The chain, end to end, all of which exists:

| stage | where |
|---|---|
| `SOUND` (117) / `SOUND_ENTITY` (116) decode | `handle_play_chunk`, `crates/protocol/v770/src/adapter/chunk.rs` |
| `EXPLODE` (36, `minecraft:explode`) decode → `ClientEvent::Sound` | `crates/protocol/v770/src/adapter/chunk.rs`'s `decode_explode` — one packet's `explosionSound` field, no separate event type |
| → `ClientEvent::Sound` / `EntitySound` | `decode_sound`/`decode_sound_entity`, `crates/protocol/v770/src/adapter/chunk.rs` |
| → `NetUpdate::Sound` / `EntitySound` | `forward`, `crates/lodestone-shell/src/net.rs` |
| → `ShellAudio::play_sound` | `crates/lodestone-shell/src/audio.rs::ShellAudio::play_sound` |
| event → `sounds.json` → weighted pick → decode → mixer | `crates/lodestone-sound/` |
| decode / mix / spatialise | `crates/lodestone-audio/` |
| device, listener from the render camera | `ShellAudio`, `Sim::set_audio_listener` (called from `app.rs::WindowApp::redraw`) — `cpal` natively, a `web_sys::ScriptProcessorNode` pulling from the same `Mixer` in the browser (see "Configuration" below) |

`net.rs`'s `forward` is the only router involved. `ingest::handles_event` and
`session::handles_event` are correctly silent on the two sound variants: a sound is
neither per-entity ECS state nor a local-player session scalar, so it travels the
shell's own `ClientEvent` stream, the same way block events do. Do **not** add an
arm to either — that mistake has cost work twice in the other direction.

### Where the bytes come from

`sounds.json` and all 4871 `.ogg` files are **not in `client.jar`**. They live in
the launcher's content-addressed asset-object store: `asset-index-<id>.json` maps a
logical name (`minecraft/sounds/mob/zombie/hurt1.ogg`) to `{hash, size}`, and the
bytes sit at `objects/<hash[0..2]>/<hash>`. That resolution is
`crates/lodestone-shell/src/asset_objects.rs`; see
[menu-panorama](./menu-panorama.md) for why that module exists at all.

Populating the store is **two** commands, and the split matters:

```bash
cargo run -p xtask -- fetch-assets  --version 26.2   # ~3.2 MB, includes sounds.json
cargo run -p xtask -- fetch-sounds  --version 26.2   # ~80 MB, the .ogg corpus
```

`fetch-assets` alone leaves you with a registry and no samples — see the failure
mode below.

## The two failures this closes

### 1. `sounds.json` present, samples absent

Measured on a `fetch-assets`-only checkout: `minecraft/sounds.json` is present
(626,160 bytes, 1968 events) and **11 of 4871** samples are on disk. So the engine
opens a device, the registry resolves every event, no object is found, and nothing
plays — while every log line says audio is enabled. This is worse than a hard
failure because it looks like working code.

Two guards, both in `audio.rs`:

* a **startup census** (`AssetObjectStore::present_count`) logging `present`/`declared`,
  which `warn`s and names `fetch-sounds` when `present == 0`;
* a **one-shot `warn`** the first time a sound cannot be played, dropping to `debug`
  afterwards so one bad event cannot flood the log.

### 2. Two environment variables for one directory

`audio.rs` required `LODESTONE_ASSET_ROOT` and returned `None` without it, while
the rest of the shell (`resources::asset_root`) honours **`LODESTONE_ASSETS`** and
otherwise walks ancestors for a `.cache/mc/*` pack. The net effect of a plain
`cargo run --release` was: vanilla textures load, the real panorama loads, and
audio is *switched off* — with one `info` line naming a variable nothing else in
the project mentions. Setting the documented `LODESTONE_ASSETS` did not help.

`asset_objects::discover_store_root` is now the single resolver:

1. `LODESTONE_ASSET_ROOT` if set — explicit, still wins, still the way to point at
   a non-standard store;
2. `LODESTONE_ASSETS` if set;
3. ancestor walk for the highest-sorting `.cache/mc/*` that is a readable store.

An explicitly-set variable is used **verbatim** and its failure is reported against
the path the user gave, never silently replaced by the scan — otherwise a typo
hides behind a working default, which is the same bug relocated.

**The predicate is not `resources`'s predicate**, and this is not pedantry.
`resources` wants `client.jar` + `generated/reports/blocks.json` (what stitches an
atlas). A store wants exactly one `asset-index-*.json` **and** an `objects/`
directory. In this checkout `.cache/mc/1.8.9` and `.cache/mc/1.12.2` each carry an
`asset-index-*.json` with no `objects/` tree at all, so an index-only predicate
would select one of them and resolve nothing.

## What is audible, and what is not

Most sounds lodestone plays are ones the **server sent** as `SOUND` or
`SOUND_ENTITY`. Since the `SoundType` census landed
([block sound types](./block-sound-types.md)) there are also three
*client-predicted* producers, all in `sim/audio.rs`. Which sounds we get is not
guesswork; it follows from whether vanilla's server passes an *excluded player* to
`Level.playSound`, checked in 26.2's own source:

| sound | vanilla mechanism | lodestone |
|---|---|---|
| mob idle / hurt / death | `Entity.playSound` → `playSound(null, …)`, broadcast to all | **plays** |
| chest lid open / close | `ChestBlockEntity.playSound` → `playSound(null, …)` server-side | **plays** |
| item and XP pickup, ambient loops, weather | server-broadcast | **plays** |
| explosion (creeper, TNT, bed, respawn anchor) | `ClientboundExplodePacket.explosionSound`, a **dedicated packet** (id 36, `minecraft:explode`) — see below | **plays** (new) |
| *another* player's placements | excluded player is *them*, so we get it | **plays** |
| **cascading** block break (torch losing support, fire, explosion) | `Level.destroyBlock` → `levelEvent(2001, …)`, no exclusion (`Level.java`) | **plays** (new) |
| **your own** block placement | `BlockItem.place` → `playSound(player, …)` (`BlockItem.java`) — predicted | **plays** (new) |
| block break in the **offline demo world** | the same `case 2001` dispatched locally | **plays** (new) |
| **your own** mined break | predicted; `interact::drive_mining` now reads the `AudioEngine` resource directly, the same way `drive_placement` already did — see below | **plays** (new) |
| **your own** footsteps | `Player.playSound` overrides to pass `this` (`Player.java`), so nothing crosses the wire — predicted instead, `Sim::tick_footstep` (`sim/step.rs`'s physics-tick loop) | **plays** |
| *another* player's footsteps and mined breaks | never on the wire at all — see below | silent |
| UI clicks | `SimpleSoundInstance.forUI`, never on the wire — `Sim::play_ui_click_sound`, called from every activating menu click in `app::WindowApp` | **plays** (new) |

Note that the first four rows are not a theoretical list: `lodestone-sound`'s
`live_sound_gate` already proves a server-decided sound crosses the public
`ClientHandle` stream, decodes a real ogg, and mixes to a peak above 0.3.

### The one that arrived and was thrown away — and the correction

`Level.destroyBlock` fires `levelEvent(2001, pos, stateId)` with **no** excluded
entity, so the packet reaches every client in range. Vanilla's
`LevelEventHandler.levelEvent` `case 2001` does *two* things with it:
`playLocalSound(soundType.getBreakSound())`
**and** `addDestroyBlockEffect`. Lodestone's `NetUpdate::BlockDestroyed` arm did only
the second. Both halves are wired now: the arm calls
`Sim::play_block_break_sound`, which reads
`lodestone_data::sound_types::break_sound_name(state)` and plays at the block centre
with vanilla's `(volume + 1) / 2` and `pitch * 0.8`.

**But this page used to claim that fixed "every block break in the game — yours and
everyone else's", and that was wrong.** A player's own dig never produces a `2001`
packet at all: `ServerPlayerGameMode.destroyBlock`
calls `this.level.removeBlock(pos, false)` and
contains **no** `levelEvent` or `playSound` anywhere in the method. `interact.rs`
had already documented that asymmetry for the *particles* — the sound row was
written without cross-referencing it. So `2001` covers cascading breaks and nothing
else, and **another player's mined break is silent in vanilla too**.

Vanilla makes your own break audible by *predicting* it: the client's
`MultiPlayerGameMode.destroyBlock` runs `playerWillDestroy` →
`Block.spawnDestroyParticles` → `level.levelEvent(player, 2001, pos, id)`, and
`ClientLevel.levelEvent` **ignores the exclusion** and dispatches straight into the
same `case 2001` locally — sound and debris together.

### The explosion sound was not decoded at all — and it is not client-predicted

Live player report: "the creeper has a hiss but no explosion sound." The hiss
(`entity.creeper.primed`, played from `Creeper.tick`, an ordinary `Entity.playSound`)
was already audible — it is only the detonation that was silent, and the
reason is structural, not a routing gap: `v770` never decoded packet id 36
(`minecraft:explode`) at all before this change, so there was nothing for any
router to forward.

This is **not** the block-break trap repeated. Block breaking's own defect was
"vanilla predicts your own break client-side and sends nothing" — the sound
genuinely never crosses the wire for the player's own dig. An explosion is the
opposite case, verified the same way: `Creeper.explodeCreeper` calls
`level.explode(...)`, which always resolves to `ServerLevel.explode`'s
overload, which constructs a `ServerExplosion` and — after
`explosion.explode()` runs — sends exactly one `ClientboundExplodePacket` per
in-range client, carrying `center`, `radius`, `blockCount`, an optional player
knockback, `explosionParticle`, `explosionSound` (a `Holder<SoundEvent>`,
`GENERIC_EXPLODE` for a plain creeper) and a `blockParticles` list.
`ClientPacketListener.handleExplosion` does
nothing but play exactly what the server sent — at a **client-rolled** pitch,
since neither `volume` (a fixed `4.0F`) nor `pitch`
(`(1.0F + (random.nextFloat() - random.nextFloat()) * 0.2F) * 0.7F`) is on the
wire at all. `decode_explode` (`crates/protocol/v770/src/adapter/chunk.rs`) rolls the
identical die rather than inventing a fixed pitch, which is why its emitted
`ClientEvent::Sound` reaches `net.rs`'s existing `Sound` forwarding arm with no
new routing needed — this is a decode gap closed entirely inside `v770`, not
an island in any of the three routers.

`decode_explode` deliberately does not model the whole packet: `explosionParticle`
is consumed via a narrow allowlist (the two "simple", argument-less particle
registry ids `Level.explode`'s call sites ever use, `explosion_emitter`/
`explosion`) rather than the full particle-options codec, and `blockParticles`
(the flying-debris list) is not decoded at all — `explosionSound` is the
second-to-last field, so nothing downstream needs it yet. This means the
explosion shockwave/smoke particle and the block-debris particles are both
still **unimplemented**: `explosionParticle`'s registry id is recognised only
to stay byte-aligned past it, never spawned. The player's own report ("or
whatever") plausibly includes these; only the sound is fixed here.

**Correction:** neither registry id needs the shared `ParticleOptions` decoder —
both are `SimpleParticleType` with no payload at all
(`ParticleTypes.java`), which `docs/particle-catalogue.md` previously
lumped in with `dust`/`firework` (which do carry a real payload). The missing
piece is a render-side `emit::`/dispatch arm, not a decoder; see that doc's
correction for detail. The genuinely-blocked field is `blockParticles` (a
`WeightedList<ExplosionParticleInfo>`, each entry with its own particle-options
payload), which is not decoded at all.

**Verification depth:** `decode_explode` has three layers of coverage, from
weakest to strongest isolation-proof: a hand-assembled byte-accurate fixture
(`sound_particle_screen.rs`, transcribed from the stream-codec spec, not our
own encoder); a real vanilla 26.2 server capture
(`live_creeper_explosion.rs`, `#[ignore]`d, feeds the server's *actual*
`explode` bytes through the adapter); and
`net::tests::a_real_explode_packet_forwards_the_correct_explosion_sound`
(`crates/lodestone-shell/src/net.rs`, `#[cfg(feature = "live")]`, no server
needed), which is the one that proves the decoded event survives the hop
through the real, production `forward()` function into the exact
`NetUpdate::Sound` value `sim/net_apply.rs`'s arm hands to
`ShellAudio::play_sound` — the two protocol-layer tests stop at
`ClientEvent::Sound` and never call `forward` at all.

### What used to be missing here, and is not any more

**This section previously said `ShellAudio` was a private field on `Sim`, not an
ECS resource, and that footsteps had no producer at all — both were stale by the
time they were read.** The `AudioEngine` resource (`docs/sim-dissolution.md`)
predates this correction, and `drive_placement` (`interact.rs`) had already been
reading it directly for its own predicted placement sound; the live predicted
*break* was the one real remaining gap, closed the same way: `drive_mining` now
takes `mut audio: ResMut<AudioEngine>` and `clock: Res<FrameClock>` alongside its
existing params and, at the exact point `Mining::take_destroyed` fires (the same
tick the debris burst and the local block-state prediction happen), resolves
`id_value`'s `SoundType` and plays `break_sound_name` at the block centre with
`block_sound_seed(hit.block, clock.ticks)` — the identical shape
`drive_placement`'s own placement sound already used, so there is no new producer
*pattern* here, only a second call site of an existing one. `sim::actions::Sim::break_block`
(the **offline demo world**'s direct-edit path, a `Sim` method rather than a
system) already played this sound; the live path was the one still silent.

Footsteps are not missing either: `Sim::tick_footstep` (`sim/step.rs`'s physics-tick
loop, right after `w.run_schedule(GameTick)` returns each tick) has run in
production since the cave-ambience/biome-loop/rain landing — per-tick, keyed off
the block *below* the player via `sound_types::step_sound_name`, gated by
`StepAccumulator`'s distance accumulator. If a future audit finds this section
disagreeing with the tree again, re-check `git log -S 'fn tick_footstep'` before
trusting either the doc or a summary of it.

## How to change it

* **Adding a server sound source**: nothing to do. Any `SOUND`/`SOUND_ENTITY`
  packet already reaches the mixer.
* **Adding a block surface sound**: the data is
  [`lodestone_data::sound_types`](./block-sound-types.md) and the three shell
  entry points are `sim/audio.rs`'s `play_block_break_sound`,
  `play_block_place_sound` and the shared `play_block_surface_sound`. Do not
  retype `(volume + 1) / 2` or `pitch * 0.8` — they are
  `BlockSoundType::break_or_place_volume`/`_pitch`, because vanilla uses the
  identical expression at both its call sites.
* **Adding a predicted sound**: the producer is the missing half, and the seed is
  the trap. Call `ShellAudio::play_sound` from wherever the prediction happens
  with a locally-chosen seed — *not* `Instant::now`, per
  `lodestone-audio/src/select.rs` (it panics on wasm). `Sim::block_sound_seed` is
  the existing answer: a `splitmix64` over the block position and
  `FrameClock::ticks`. It deliberately does **not** draw from the particle
  engine's `JavaRandom`, even though that is already in scope at the break site,
  because shifting that sequence would break the `mining_destroy_burst` and
  `break_particle_tint` golden gates.
* **Reaching audio from an ECS system**: `ResMut<crate::sim::AudioEngine>`, exactly
  as `drive_placement`/`drive_mining` (`interact.rs`) already do — it is a real
  resource, not a private `Sim` field, and has been since `docs/sim-dissolution.md`
  landed. `Sim::play_local_sound`/`Sim::play_relative_sound` are the equivalent for
  non-system callers (`crate::app::WindowApp`, `crate::sim::Sim` methods).
* **A head-relative sound with no world position** (UI, vanilla's
  `SimpleSoundInstance.forUI`/`forMusic` shape: `Attenuation.NONE`, `RELATIVE`, no
  panning, audible identically everywhere): `Sim::play_relative_sound`, or
  `AudioEngine::play_relative_sound` from inside a system. Not the same call as a
  positioned one — passing a position and hoping it lands near the listener is an
  approximation this API does not need you to make.
* **Changing the corpus policy**: `xtask::plan_sound_corpus`. Keep it derived from
  `sounds.json`; a file list rots at the next version bump. The browser's curated
  subset (`web/scripts/stage_sounds.py`) uses the same "derive from event names,
  not files" discipline at a much smaller scale — see its module doc.
* **Gotcha — the index key has no `assets/` prefix.** `AssetObjectStore`'s
  `ResourceSource` impl strips it; call `object_bytes` directly and you must not
  pass one. Getting this wrong resolves nothing, silently.
* **Gotcha — `fixed_range` from the packet is deliberately dropped** in `forward`.
  Client attenuation uses the `sounds.json` entry distance, not the server's
  culling range.
* **Gotcha — the seed is the server's** and must be passed through unchanged;
  rolling a variant client-side desyncs every client.

## `xtask fetch-sounds`

```
cargo run -p xtask -- fetch-sounds --version 26.2 [--all] [--jobs N] [--force]
```

The corpus is **derived from `sounds.json`**, not from a list. Every event's
`sounds` array is walked, each entry's name resolved to
`<namespace>/sounds/<path>.ogg`, and `"type": "event"` entries skipped (they are
indirections to another event, which is walked in its own right). Measured on 26.2:
all 4843 distinct names resolve to a real index entry, zero misses.

A sample is excluded only when **every** event referencing it is a music event
(`music.*`, `music_disc.*`). "Every", not "any": `records/cat` is named by both
`music_disc.cat` and `jukebox.play`, and an "any" rule would silently drop it.

| set | objects | bytes |
|---|---|---|
| fetched by default | 4751 | 80.14 MB |
| excluded: 70 music tracks + 22 jukebox records | 92 | 293.23 MB |
| referenced by no event at all | 28 | — |

So the default covers **every sample a non-music event can select**: mobs, blocks,
items, entities, steps, digs, liquid, UI, notes, enchanting, fireworks, minecarts,
portals — and all six biome ambience loops. It does not cover background music or
jukebox discs; `--all` adds them (4843 objects, 373.37 MB). The 28 unreferenced
`.ogg` objects are fetched in neither mode: no event can select them.

**Why the policy is "music events" and not vanilla's `"stream": true` flag.** The
flag is the cheaper derivation and was measured: it selects 98 samples, 296 MB —
but six of those are the nether and underwater ambience loops (2.9 MB total), so it
would silence cave and nether ambience to save nothing. Measured, not assumed.

Every object goes through `xtask::ensure_object`, which verifies the SHA-1 the index
declares; there is no second fetcher. A re-run of a complete fetch downloads
nothing — it re-hashes what is on disk (80 MB of SHA-1, well under a second) and
reports every object as cached. Downloads run on 12 threads by default because each
object is one `curl` process and the corpus averages ~17 KB per file, so wall time
is connection setup, not bandwidth.

## Configuration

**Native** — `crate::audio::ShellAudio::from_env` reads the local asset-object
store:

| variable | effect |
|---|---|
| `LODESTONE_ASSET_ROOT` | asset-object root, highest priority |
| `LODESTONE_ASSETS` | pack root; the same directory in a vanilla install |
| neither set | ancestor walk for `.cache/mc/<version>` |

**Browser** — no environment variable; `web/`'s trunk build fetches everything
`ShellAudio::from_env` (the `wasm32` arm) needs and hands it in through
[`platform::assets::Bundle`](../crates/lodestone-shell/src/platform.rs)'s
`sounds_json`/`sound_objects` fields, exactly like `client.jar` and the
title-screen panorama:

| stage | where |
|---|---|
| curated event list | `web/scripts/stage_sounds.py`'s `CURATED_EVENTS` — 16 events, derived to 46 `.ogg` objects, not a hand-kept file list |
| staging (build time, conditional) | `web/Trunk.toml`'s third `post_build` hook, fail-open exactly like the panorama hook beside it |
| fetch (runtime, best-effort) | `web/src/main.rs`'s `fetch_sound_bundle` |
| gesture gate | `Sim::resume_audio_on_gesture`, called from every real mouse-press/key-press `WindowEvent` (`app/lifecycle.rs::window_event`) — a no-op on native |

Measured (26.2, this checkout's `.cache/mc/26.2`): 46 objects, 411,904 B raw /
375,502 B gzip, plus `sounds.json` staged whole at 626,160 B raw / 44,671 B gzip —
1,038,064 B raw / 420,173 B gzip total. This is fetched at runtime, the same as
`client.jar`, so it does **not** count against `just wasm-size`'s ceiling on the
compiled `.wasm` (that guard measures the linked binary only); the number is
recorded here because a real page load still pays for it over the wire.
`audio.rs` used to be `#![cfg(not(target_arch = "wasm32"))]` in its entirety —
it no longer is; both targets compile a real `ShellAudio` now, with different
backends (`cpal` vs. a `web_sys::ScriptProcessorNode`) behind the same public
API.

**Growing the curated corpus needs no Rust change and no `.wasm` rebuild.** Add
an event name to `CURATED_EVENTS`; `web/src/main.rs` fetches whatever the
staged manifest names, by name, with no hardcoded file list on the Rust side
to keep in sync.

## Dependencies

`lodestone-sound` (registry resolution, weighted selection, `cpal` device
natively, a device-free `Mixer`-driving resolver in the browser),
`lodestone-audio` (Ogg Vorbis decode, mix, spatialise — shared by both
targets), `lodestone-assets` (`SoundRegistry`, `ResourceSource`,
`MemorySource` for the browser's staged bundle), `crate::asset_objects` (the
native store), `lodestone-render::Camera` (the listener transform). `xtask`
needs `curl` on `PATH`. The browser path additionally needs `web_sys`'s
`AudioContext`/`ScriptProcessorNode` (already an ordinary `lodestone-shell`
wasm32 dependency) and, at build time, a Python 3 interpreter for
`stage_sounds.py`/`stage_panorama.py` (same requirement the panorama staging
already had).
