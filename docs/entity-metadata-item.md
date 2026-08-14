# Entity metadata: the item field

## What it is

The seam that lets a dropped item (`minecraft:item`) tell the client *what it
is*. A drop carries no item id in its spawn packet; its entire visible identity
rides one entity-metadata field, index 8, under the `ITEM_STACK` serializer.
This doc covers decoding that field in the 26.2 adapter and carrying it into the
version-free [`EntityMetadataUpdate`].

Until this landed, `ITEM_STACK` was rejected outright — the comment read
"complex, self-describing payloads mobs never emit", which is true of mobs and
false of item entities. A rejected metadata decode raises no event at all, so a
live drop reached the renderer as `drop.item = None`.

## How it works

```
set_entity_data  →  packets::metadata::read_entity_metadata
                      → SER_ITEM_STACK arm
                      → adapter::read_item_stack   (the single item codec)
                      → EntityMetadataUpdate.item
                    adapter::handle_set_entity_data
                      → ClientEvent::EntityMetadataUpdated
```

Three things are worth knowing.

**The item codec is not duplicated.** `crate::adapter::read_item_stack` /
`read_component_patch` already model 26.2's `DataComponentPatch` for container
packets. The metadata path calls the same function (it is `pub(crate)` for
exactly this reason). Two independent readings of the component-patch wire is
how the two ends drift apart.

**The field is raised by serializer, not by index.** Like the registry-holder
appearance variants, `ITEM_STACK` identifies itself, so no `MetadataClass`
disambiguation is needed. Index 8 happens to be where a dropped item, an item
frame, and thrown projectiles all put it, but nothing in the decoder depends on
that.

**One place gives up alignment, deliberately.** A clientbound component patch
length-prefixes neither the patch nor its individual components, so a component
this build does not model cannot be skipped. The item codec therefore stops
there, keeps the item key, count, and any components it already read, sets
`ItemComponents::has_unmodeled`, and reports `complete == false` with the reader
parked mid-payload.

Metadata is a *stream of indexed fields terminated by a `0xFF` sentinel*, so a
parked reader cannot resume: every following byte would decode as a
plausible-but-wrong `(index, serializer, value)` triple — garbage that never
fails loudly. Scanning ahead for the sentinel is no better, because `0xFF`
occurs freely inside payload bytes. So `read_entity_metadata` **abandons the
rest of the list**, returns what it has, and flags
`DecodedMetadata::complete == false`. `handle_set_entity_data` then skips its
trailing-bytes assertion (there are trailing bytes by construction) and emits
the partial update anyway.

That is safe because metadata is applied incrementally — an update carrying a
subset of fields is the *normal* case, and every field it does carry was
consumed byte-accurately before the abandonment point. In practice nothing is
lost at all: a dropped item emits index 8 and nothing else.

## Fail open, never fail closed

This class of bug has already been a session-killer here. `read_item_stack`
once fail-*closed* on component patches, and because the driver treats a decode
error as fatal, equipping any tool ended the session — in 26.2 essentially every
real item carries components.

The rule this seam preserves: **an undecodable item must produce a partial or
absent stack, never an error that propagates.** The item key and count are read
*before* any component is, so an unrecognised component costs detail and never
the answer to "which item is this".

Two things to keep true if you extend this:

- Never turn the `complete == false` path into an `Err`.
- Never let the caller run `ensure_empty()` when `complete == false`. That is
  the subtle fail-closed regression: the packet would be dropped, throwing away
  an item identity that was already decoded exactly.

A decode error *inside* the stack (unknown item registry id, truncation, an
inline enchantment holder) still errors, and `handle_set_entity_data` swallows
it into "emit nothing for this packet". The connection survives either way.

## How to change it

- Adding a modeled data component: extend `read_component_patch` in
  `crates/protocol/v770/src/adapter/inventory.rs`. Both the container path and
  this one pick it up for free.
- Changing what the metadata event carries: `EntityMetadataUpdate` in
  `crates/lodestone-model/src/event.rs`. Remember `is_empty()` — a field missing
  from it makes an otherwise-real update get swallowed as "empty".
- The `item` field is nested `Option<Option<ItemStack>>`, matching `custom_name`:
  outer is "did this packet include the field", inner is "is a stack set".
  `Some(None)` is the empty stack, which vanilla draws as nothing.

## Tests

| Test | What it proves |
| --- | --- |
| `crates/protocol/v770/tests/item_entity_metadata.rs` | Hermetic replay of checked-in **server-authored** bytes: identity decodes, the unmodeled path stays fail-open, the reader parks exactly where expected, following fields are abandoned rather than misread. |
| `crates/protocol/v770/tests/live_item_entity_metadata.rs` (`--features live-item-entity`, `--ignored`) | Joins the real 26.2 survival oracle, `/summon`s a drop, captures the raw payload, diffs it against the fixture, and asserts the session survives. |
| `crates/protocol/v770/tests/fixtures/*.hex` | The captured bytes themselves, annotated byte-by-byte. |

The fixtures exist because an expected value must originate **outside** the code
under test. Validating a decoder against bytes our own encoder produced closes
perfectly over a shared misunderstanding — the hermetic chunk fixtures passed
that way and a live gate then produced 49 "unexpected end of input". Re-capture
with `LODESTONE_CAPTURE_FIXTURES=1` and re-review the diff; never hand-edit a
fixture to make a test pass.

## Configuration

- `live-item-entity` — Cargo feature gating the live capture gate. Off by
  default, and the tests are additionally `#[ignore]`d, so `cargo test -p
  lodestone-v770` stays hermetic.
- `LODESTONE_CAPTURE_FIXTURES=1` — rewrites the checked-in fixtures from a live
  run instead of asserting against them.
- The oracle: vanilla 26.2 survival on `127.0.0.1:25565`, RCON `:25566`,
  password `lodestone`. Start it with `./scripts/live-oracles/survival.sh`.

## Dependencies

- `lodestone-core` — `Reader`, VarInt/NBT primitives.
- `lodestone-model` — `EntityMetadataUpdate`, `ItemStack`, `ItemComponents`.
- `crate::adapter` — the shared clientbound item-stack codec.
- `lodestone-testsupport` — `RconClient` (whole RCON frame in one `write_all`;
  vanilla does exactly one `read()` per request) and `unique_username`.

## What consumes it

The whole chain is connected:

```
EntityMetadataUpdate.item          (this doc)
  → EntityView::item               lodestone-client/src/state.rs (apply_metadata)
  → EntitySnapshot::item           lodestone-shell/src/net.rs    (entity_snapshot)
  → EntityInterpolator::set_item_stack
  → EntityDraw::item               → RenderState::prepare_item_drops
```

The nesting survives as far as `EntitySnapshot` and is flattened only at the
interpolator, where the two `None`s finally mean the same thing (draw nothing).
Every fold on the way is "overwrite only when the update carried the field" —
a live drop announces its stack once and is silent afterwards, so any layer that
treats silence as an empty stack blanks the item one tick after it arrives. See
[Dropped items](./dropped-items.md) for the render half, including what the
conversion to `ResourceLocation` drops.
