# Filled map item rendering

## What it is

This covers the filled map item's own visual: the generated per-map pixel
texture and border frame when held or displayed in an item frame. The wire,
session fold, palette conversion, and these two render paths are live; marker
icons and a retained per-map GPU texture cache remain follow-up work.

> **Update: the blocker below is resolved, and the shell now reads the fold.**
> `MAP_ITEM_DATA` (id 51) decodes into `ClientEvent::MapItemData`, `route` sends it
> to **`session`** (`apply_maps` → `SessionMaps`) with `shell: false`, so there is
> deliberately **no `net.rs` `forward` arm** — see
> [`map-and-advancement-wire.md`](./map-and-advancement-wire.md), including the
> `MapPatch` field order and the fact that the colour half is a **sub-rectangle**,
> not a 128×128 frame. `MapStore::apply` blits the patch itself, so a reader calls
> `MapState::color_at(x, y)` and never handles rectangles.
>
> `Sim::maps()` is the shell's read seam. `Sim::map_source()` captures a cheap
> copy-on-write `MapStore` snapshot for held and framed-map rendering;
> `Sim::map_debug()` gathers the F3-only map count and explored fraction. The
> diagnostic is skipped entirely while F3 is closed.
>
> The survey that follows is kept because its reasoning about *where* the renderer
> belongs is unaffected; only its premise ("nothing decodes it") has changed.

## The finding as it stood: the data did not reach the client at all

**No renderer was built, because the wire data this issue depends on is not
decoded anywhere in this codebase.** Checked directly rather than assumed,
per this task's own instruction to verify before building against data that
might not arrive:

```
$ grep -rn "MAP_ITEM_DATA" crates/protocol/v770/src/
crates/protocol/v770/src/generated/packet_ids.rs:208:        pub const MAP_ITEM_DATA: i32 = 51;
crates/protocol/v770/src/generated/packet_ids.rs:351:            ("minecraft:map_item_data", MAP_ITEM_DATA),
crates/protocol/v770/src/generated/packet_ids.rs:896:            Some(play::clientbound::MAP_ITEM_DATA)
```

All three hits are in `generated/packet_ids.rs` — the auto-generated
id/name table produced from Mojang's own `packets.json` regardless of
whether anything actually decodes the packet. `crates/protocol/v770/src/packets/`
(where every packet that *is* decoded gets a hand-written struct) has
**zero** files or hits for `map`/`Map` at all:

```
$ find crates/protocol/v770/src/packets -iname "*map*"
(no output)
```

So `Map Item Data` (the clientbound packet carrying a map id, its
colour-indexed pixel updates, scale and marker list — the *only* wire
source for what a filled map actually looks like) has an id constant and
nothing else: no packet struct, no decoder, no dispatch arm. A server can
send it and this client silently drops it as an unrecognised play packet,
the same as any other undecoded id.

This is a **different** gap from the item components this session found
missing separately (`minecraft:potion_contents`, `minecraft:banner_patterns`
land as opaque-but-present `ComponentValue::Opaque` blobs — the item stack
carries the byte, just not a typed reading of it). A filled map's pixel data
is not carried by the item stack at all; it lives in its own packet, keyed
by the map's numeric id, sent (and re-sent on update) independently of
whether the item is even in an inventory the client can see. There is
nothing an item-component fix could recover here — this is a missing packet
decoder, full stop.

## Why nothing was built anyway

Per this task's own framing: "if it does not [reach us], that is the
finding, and say so rather than building against data that never arrives."
A renderer here — palette lookup, marker icons, border frame, held/GUI/
item-frame pose — would have every one of `CLAUDE.md`'s "island" properties:
individually buildable, individually testable (a renderer can always be fed
synthetic pixel data in a unit test), and reaching **zero** real pixels in
play, because nothing would ever call it with real content. That is worse
than not building it, because it would look done.

## What is NOT blocked, and was checked

The map item's **GUI icon** (an empty/unfilled map, or a filled map's icon
before any per-instance texture is available) still needs *some* fallback
appearance — checked whether the ordinary flat-sprite path already covers
it:

```
$ cat assets/minecraft/items/map.json   # via .cache/mc/26.2/client-src
{"model": {"type": "minecraft:model", "model": "minecraft:item/map"}}
$ cat assets/minecraft/items/filled_map.json
{"model": {"type": "minecraft:model", "model": "minecraft:item/filled_map", "tints": [...]}}
```

`minecraft:item/filled_map` is a plain `builtin/generated` model with a
`minecraft:map_color` tint source on a static `map_filled_markings`-style
base sprite — this is the **generic border/base icon** every filled map
shares before its own per-instance pixels are drawn on top, and it already
resolves through the ordinary [`IconPart::Sprite`] path this codebase
already renders (no special renderer, no missing geometry). So a filled map
in an inventory slot already shows *a* map icon today, just never its own
unique terrain — consistent with vanilla itself, which layers the
per-instance texture as an *addition* to this same base icon, not a
replacement for it.

`minecraft:map_color`'s tint (like `minecraft:potion`'s, see
`docs/item-variants.md`'s scoping) is a per-instance value in principle
(vanilla derives a map's average terrain colour for the icon border), but
with no packet decoding the per-instance data at all, there is nothing to
compute it from yet — it correctly falls back to the tint's `default`, the
same "untinted/default until real data exists" behaviour every other
undecoded tint source in this codebase has today.

## How it works today

`MapStore` owns an `Arc<BTreeMap<i32, MapState>>`, and each map's 128×128
palette grid is an `Arc<Vec<u8>>`. Cloning the store for a render source is
therefore pointer-sized unless a later packet mutates it. `MapStore::apply`
uses `Arc::make_mut`: an active render snapshot keeps its old values, while
the live store copies only the map tree and pixel grid that are actually
changed. The renderer carries that same pixel `Arc` to both consumers rather
than allocating and copying 16 KiB per lookup.

This is exact synchronous extraction, not speculative simulation. A frame sees
one immutable store generation; incoming map packets update the next generation
without invalidation bookkeeping or races.

## What is still missing

The remaining rendering work is:

1. **Retained per-map dynamic textures.** The current renderer converts and
   uploads a 128×128 `Rgba8UnormSrgb` texture when a map is drawn. The CPU map
   pixels are shared, but the GPU texture is not yet cached by map id and store
   generation. Add an explicit generation counter before retaining textures;
   pointer identity alone cannot distinguish in-place mutation after
   `Arc::make_mut` obtains unique ownership.
2. **Decoration and GUI draws.** The held and item-frame terrain draws are
   implemented. Decoration sprites and the GUI's per-instance terrain preview
   are still absent. Keep map geometry on `EntityPipeline`: the model shader is
   already at wgpu's four-bind-group floor and cannot accept another texture
   group.

## How to change it

Change packet folding and snapshot ownership in
`crates/lodestone-game/src/maps.rs`. Keep the copy-on-write tests when adding
fields: a snapshot must remain immutable after a packet updates the live store.
The shell source contract is in `gpu/sources.rs`, its capture in
`sim/render_sources.rs`, and texture creation/draws in `gpu/maps.rs`.

Do not expose mutable pixel slices across the render seam. For a retained GPU
cache, add a monotonic generation to `MapState`, increment it only when pixels
change, and key the texture cache by `(map_id, generation)`.

## Configuration

There are no environment variables or player settings specific to maps. The
ordinary render-distance and item/entity visibility rules determine whether an
item-frame map is considered for drawing.

## Dependencies

- `lodestone-game` owns `MapStore` and applies `ClientEvent::MapItemData`.
- `lodestone-shell` captures render sources and owns the wgpu textures/draws.
- `lodestone-render` supplies the Minecraft map-palette conversion.
- The v770 protocol adapter decodes the map item data packet.

### Two stale pointers, corrected

* **The held-item pose does not wait on the previously-cited blockers.** One
  referenced a nonexistent issue, and the other was bow-draw arm poses —
  unrelated. The real seam is `ItemVariants::resolve`'s
  `display_context` branch, which `spyglass_in_hand` already uses — 26 of 26.2's
  items name a different model in the hand than in the slot, and that fork is
  already built.
* **There is no `net.rs` `forward` arm to add.** `route` marks `MapItemData`
  `shell: false` on purpose; adding one would put a second writer on `SessionMaps`.
