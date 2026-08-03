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

Every sound lodestone plays is one the **server sent** as `SOUND` or
`SOUND_ENTITY`. There is no client-side sound prediction anywhere in the tree —
`play_sound`/`play_entity_sound` have exactly two call sites, both the `NetUpdate`
arms above. Which sounds that costs is not guesswork; it follows from whether
vanilla's server passes an *excluded player* to `Level.playSound`, checked in
26.2's own source:

| sound | vanilla mechanism | lodestone |
|---|---|---|
| mob idle / hurt / death | `Entity.playSound` → `playSound(null, …)`, broadcast to all | **plays** |
| chest lid open / close | `ChestBlockEntity.playSound` → `playSound(null, …)` server-side | **plays** |
| item and XP pickup, explosions, ambient loops, weather | server-broadcast | **plays** |
| *another* player's footsteps and placements | excluded player is *them*, so we get it | **plays** |
| **your own** footsteps | `Player.playSound` overrides to pass `this` (`Player.java:399`) — server broadcasts to everyone but you | silent |
| **your own** block placement | `BlockItem.place` → `playSound(player, …)` (`BlockItem.java:87`) | silent |
| UI clicks | `SimpleSoundInstance.forUI`, never on the wire | silent |
| **any** block break | *arrives, and is dropped* — see below | silent |

Note that the first four rows are not a theoretical list: `lodestone-sound`'s
`live_sound_gate` already proves a server-decided sound crosses the public
`ClientHandle` stream, decodes a real ogg, and mixes to a peak above 0.3.

### The one that arrives and is thrown away

`Level.destroyBlock` fires `levelEvent(2001, pos, stateId)` with **no** excluded
entity (`Level.java:288`), so the packet reaches the breaking player too — this is
not a predicted sound. Vanilla's `LevelEventHandler` `case 2001` does *two* things
with it (`LevelEventHandler.java:283-291`): `playLocalSound(soundType.getBreakSound())`
**and** `addDestroyBlockEffect`. Lodestone's `NetUpdate::BlockDestroyed` arm
(`sim.rs:4552`) does only the second. So every block break in the game — yours and
everyone else's — is visually right and silent, from an event that is already
decoded, already routed, and already handled.

Fixing it needs one thing the tree does not have: a **per-block-state `SoundType`
table**. `grep` finds no break/step/place sound data anywhere; the only hit is
`minecraft:break_sound` as a data-component *name*. Per the data-sources rule that
table comes from booting the real server and walking `Block.BLOCK_STATE_REGISTRY`
for `state.getSoundType()`, next to `collision_shapes` and `hardness` — not from a
community dataset. With it, the same table also unlocks predicted footsteps and
placement sounds, which is the other three silent rows. `lodestone-audio` already
documents a `play_sound(SoundInstance)` entry point for prediction and `select.rs`
already takes an injected seed for it, so the engine side is ready and only the
producer and the data are missing.

## How to change it

* **Adding a server sound source**: nothing to do. Any `SOUND`/`SOUND_ENTITY`
  packet already reaches the mixer.
* **Adding the block-break sound**: generate the `SoundType` table first (above),
  then extend `sim.rs`'s `NetUpdate::BlockDestroyed` arm with the `play_sound` half
  of vanilla's `case 2001`. This is the cheapest audible win left.
* **Adding a predicted sound**: the producer is the missing half. Call
  `ShellAudio::play_sound` from wherever the prediction happens (mining, placement,
  step), with a locally-chosen seed — *not* `Instant::now`, per
  `lodestone-audio/src/select.rs`.
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
