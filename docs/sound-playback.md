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
| `SOUND` (117) / `SOUND_ENTITY` (116) decode | `crates/protocol/v770/src/adapter.rs:3405` |
| → `ClientEvent::Sound` / `EntitySound` | `adapter.rs:1340`, `:1362` |
| → `NetUpdate::Sound` / `EntitySound` | `crates/lodestone-shell/src/net.rs:1212`, `:1228` |
| → `ShellAudio::play_sound` | `crates/lodestone-shell/src/sim.rs:4631`, `:4644` |
| event → `sounds.json` → weighted pick → decode → mixer | `crates/lodestone-sound/` |
| decode / mix / spatialise | `crates/lodestone-audio/` |
| `cpal` device, listener from the render camera | `ShellAudio`, `Sim::set_audio_listener` (called from `app.rs:1735`) |

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
*client-predicted* producers, all in `sim.rs`. Which sounds we get is not
guesswork; it follows from whether vanilla's server passes an *excluded player* to
`Level.playSound`, checked in 26.2's own source:

| sound | vanilla mechanism | lodestone |
|---|---|---|
| mob idle / hurt / death | `Entity.playSound` → `playSound(null, …)`, broadcast to all | **plays** |
| chest lid open / close | `ChestBlockEntity.playSound` → `playSound(null, …)` server-side | **plays** |
| item and XP pickup, explosions, ambient loops, weather | server-broadcast | **plays** |
| *another* player's placements | excluded player is *them*, so we get it | **plays** |
| **cascading** block break (torch losing support, fire, explosion) | `Level.destroyBlock` → `levelEvent(2001, …)`, no exclusion (`Level.java:280-289`) | **plays** (new) |
| **your own** block placement | `BlockItem.place` → `playSound(player, …)` (`BlockItem.java:87`) — predicted | **plays** (new) |
| block break in the **offline demo world** | the same `case 2001` dispatched locally | **plays** (new) |
| **your own** mined break | predicted; the emit lives in an ECS system with no audio handle — see below | silent |
| **your own** footsteps | `Player.playSound` overrides to pass `this` (`Player.java:399`) — server broadcasts to everyone but you | silent |
| *another* player's footsteps and mined breaks | never on the wire at all — see below | silent |
| UI clicks | `SimpleSoundInstance.forUI`, never on the wire | silent |

Note that the first four rows are not a theoretical list: `lodestone-sound`'s
`live_sound_gate` already proves a server-decided sound crosses the public
`ClientHandle` stream, decodes a real ogg, and mixes to a peak above 0.3.

### The one that arrived and was thrown away — and the correction

`Level.destroyBlock` fires `levelEvent(2001, pos, stateId)` with **no** excluded
entity (`Level.java:280-289`), so the packet reaches every client in range. Vanilla's
`LevelEventHandler` `case 2001` does *two* things with it
(`LevelEventHandler.java:283-291`): `playLocalSound(soundType.getBreakSound())`
**and** `addDestroyBlockEffect`. Lodestone's `NetUpdate::BlockDestroyed` arm did only
the second. Both halves are wired now: the arm calls
`Sim::play_block_break_sound`, which reads
`lodestone_data::sound_types::break_sound_name(state)` and plays at the block centre
with vanilla's `(volume + 1) / 2` and `pitch * 0.8`.

**But this page used to claim that fixed "every block break in the game — yours and
everyone else's", and that was wrong.** A player's own dig never produces a `2001`
packet at all: `ServerPlayerGameMode.destroyBlock`
(`ServerPlayerGameMode.java:262-298`) calls `this.level.removeBlock(pos, false)` and
contains **no** `levelEvent` or `playSound` anywhere in the method. `interact.rs`
had already documented that asymmetry for the *particles* — the sound row was
written without cross-referencing it. So `2001` covers cascading breaks and nothing
else, and **another player's mined break is silent in vanilla too**.

Vanilla makes your own break audible by *predicting* it: the client's
`MultiPlayerGameMode.destroyBlock` runs `playerWillDestroy` →
`Block.spawnDestroyParticles` → `level.levelEvent(player, 2001, pos, id)`, and
`ClientLevel.levelEvent` **ignores the exclusion** and dispatches straight into the
same `case 2001` locally (`ClientLevel.java:877-882`) — sound and debris together.

### What is still missing, and the exact seam

The live predicted break. Its debris sibling is emitted in
`interact.rs`'s `drive_mining` at `mining.0.take_destroyed()`, which is a Bevy system
running *inside* the one `World` — and `ShellAudio` is a private field on `Sim`, not an
ECS resource, so the system cannot reach it. (Making it a resource is not a
one-liner: `AudioEngine` owns a live `cpal` stream, and `interact.rs` already records
a deadlock from a system taking a read guard on the `World` it runs in.)

The seam that closes it: a small `PredictedBlockSounds(Vec<([i32; 3], u32)>)` ECS
resource that `drive_mining` pushes to next to the `destroy_block` call, drained by
`Sim` immediately after it runs the `GameTick` schedule and fed to
`Sim::play_block_break_sound`. That is a *new producer path*, deliberately not
half-built here.

Footsteps are the other missing producer and a bigger one — per-tick, per-surface,
distance-gated, and keyed off the block *below* the player. The data is now free
(`sound_types::step_sound_name(id)`), so what remains is the step-distance
accumulator and the surface pick, not a census.

## How to change it

* **Adding a server sound source**: nothing to do. Any `SOUND`/`SOUND_ENTITY`
  packet already reaches the mixer.
* **Adding a block surface sound**: the data is
  [`lodestone_data::sound_types`](./block-sound-types.md) and the three shell
  entry points are `sim.rs`'s `play_block_break_sound`,
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
* **Reaching audio from an ECS system**: you cannot, today. See "What is still
  missing" above for the seam.
* **Changing the corpus policy**: `xtask::plan_sound_corpus`. Keep it derived from
  `sounds.json`; a file list rots at the next version bump.
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

| variable | effect |
|---|---|
| `LODESTONE_ASSET_ROOT` | asset-object root, highest priority |
| `LODESTONE_ASSETS` | pack root; the same directory in a vanilla install |
| neither set | ancestor walk for `.cache/mc/<version>` |

No feature flag: `audio.rs` is `#![cfg(not(target_arch = "wasm32"))]` and otherwise
always compiled.

## Dependencies

`lodestone-sound` (registry resolution, weighted selection, `cpal` device),
`lodestone-audio` (Ogg Vorbis decode, mix, spatialise), `lodestone-assets`
(`SoundRegistry`, `ResourceSource`), `crate::asset_objects` (the store),
`lodestone-render::Camera` (the listener transform). `xtask` needs `curl` on
`PATH`.
