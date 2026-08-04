# Filled map item rendering (issue #184) — blocked on decode

## What it is

Issue #184 asks for the filled map item's own visual: the generated
per-map pixel texture, player/marker icons, and the border frame, whether
held, in an item frame, or shown as a GUI icon.

## The finding: the data does not reach the client at all

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

## Next steps, in order

1. **Decode `Map Item Data`** (`crates/protocol/v770/src/packets/`, id `51`
   in play/clientbound — server-decode agent's territory per this task's
   file ownership, not this crate's). Fields, per vanilla's packet
   definition: map id (varint), scale, locked flag, optional marker icon
   list, optional colour-patch update (x/z origin + width/height + raw
   index bytes into the map colour palette).
2. **A colour palette lookup** — vanilla's map colours are *indexed*
   (`MapColor` base id × 4 shade variants), not raw RGB; this is real
   render-crate work once (1) lands, and belongs in `lodestone-render`.
3. **Marker icons and the border frame** — GUI + held + item-frame poses,
   again render-crate work once real per-map pixel data exists to draw.
4. **Held-item pose** — this issue's own text says to land it after #54/#57
   establish the held-item pose-selection mechanism, to avoid a second,
   divergent pose seam.

Nothing here was spread across multiple crates speculatively; steps 2-4 all
wait on step 1, which is outside this task's ownership.
