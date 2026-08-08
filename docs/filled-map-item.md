# Filled map item rendering (issue #184) — the wire and the fold are landed, the renderer is not

## What it is

Issue #184 asks for the filled map item's own visual: the generated
per-map pixel texture, player/marker icons, and the border frame, whether
held, in an item frame, or shown as a GUI icon.

> **Update: the blocker below is resolved, and the shell now reads the fold.**
> `MAP_ITEM_DATA` (id 51) decodes into `ClientEvent::MapItemData`, `route` sends it
> to **`session`** (`apply_maps` → `SessionMaps`) with `shell: false`, so there is
> deliberately **no `net.rs` `forward` arm** — see
> [`map-and-advancement-wire.md`](./map-and-advancement-wire.md), including the
> `MapPatch` field order and the fact that the colour half is a **sub-rectangle**,
> not a 128×128 frame. `MapStore::apply` blits the patch itself, so a reader calls
> `MapState::color_at(x, y)` and never handles rectangles.
>
> `Sim::maps()` is the shell's read seam, and `Sim::map_debug()` is its only reader
> today: an F3 line reporting the map count and the lowest-numbered map's explored
> fraction. That is a **diagnostic**, the same shape `border_debug`/`spawn_debug`
> have, and it exists so a live fold cannot be mistaken for one that never runs. It
> is not the map's picture. What the picture needs is in "What is still missing"
> below.
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
missing for #171/#174 (`minecraft:potion_contents`, `minecraft:banner_patterns`
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
`docs/item-variants.md`/#171's scoping) is a per-instance value in principle
(vanilla derives a map's average terrain colour for the icon border), but
with no packet decoding the per-instance data at all, there is nothing to
compute it from yet — it correctly falls back to the tint's `default`, the
same "untinted/default until real data exists" behaviour every other
undecoded tint source in this codebase has today.

## What is still missing

Step 1 (the decode) is done. What remains is one coherent unit, and it is a
**texture** job rather than a wiring one — which is why it was not bolted onto a
session that had no budget to do it properly:

1. **A colour palette lookup.** Vanilla's map colours are *indexed*: a
   `MapColor` base id times four shade variants, packed as `index * 4 + shade`,
   which is exactly the byte `MapState::colors` stores. The table is
   `net/minecraft/world/level/material/MapColor.java` — an outside source, hand
   expandable, and the right home is `lodestone-render` beside the drawing code
   (`lodestone-game`'s `maps` module deliberately does **not** resolve to RGB, and
   says so).
2. **A per-map dynamic texture.** 128x128 `Rgba8UnormSrgb`, one per known map id,
   uploaded when the store changes and cached by id. This is the part that makes
   it a unit of its own: it needs a texture, a bind group over an existing layout,
   and an invalidation signal — and `queue.write_buffer`/`write_texture` is ordered
   against the *submit*, not the encoder, so two maps sharing one staging buffer
   would both show the last one written.
3. **The draws.** Held (first person, `ItemInHandRenderer.renderMap`), in an item
   frame, and the `map_background` border quad plus the decoration sprites over
   both. **Do not add a fifth bind group**: the model shader is already at wgpu's
   4-group floor, so this goes through `EntityPipeline` (two groups) the way the
   chest GUI icon does — see `docs/block-entity-renderers.md`'s "two consumers"
   section for that precedent.

### Two stale pointers, corrected

* **The held-item pose does not wait on "#54/#57".** There is no issue #54, and #57
  is bow-draw arm poses. The real seam is `ItemVariants::resolve`'s
  `display_context` branch, which `spyglass_in_hand` already uses — 26 of 26.2's
  items name a different model in the hand than in the slot, and that fork is
  already built.
* **There is no `net.rs` `forward` arm to add.** `route` marks `MapItemData`
  `shell: false` on purpose; adding one would put a second writer on `SessionMaps`.
