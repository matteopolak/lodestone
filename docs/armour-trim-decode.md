# Armour trim decoding, and the component-patch decode cliff

This is partial — the wire half is done; the renderer half is shell work and still
outstanding.

## What it is

`minecraft:trim` now decodes off the wire into
`lodestone_model::ItemComponents::trim`, so a smithing-table armour trim reaches
the client as a `(material, pattern)` pair. The asset layer it feeds
(`lodestone_assets::trim`, `trim_decal_pipeline`) was already complete with zero
callers; this is the missing link.

The more important half is why it had to be *modeled* rather than skipped.

## The decode cliff, and why skipping is impossible

`read_component_patch` (`crates/protocol/v770/src/adapter/inventory.rs`) has an `other =>`
arm that sets `has_unmodeled` and **stops reading the rest of the packet**. That
looks like a wart worth fixing generically — skip the unknown component's payload
and continue — and it is not fixable, verified against the jar rather than assumed:

26.2 ships **two** patch codecs, `DataComponentPatch`'s `STREAM_CODEC` and
`DELIMITED_STREAM_CODEC` fields:

| codec | payloads | used by |
|---|---|---|
| `STREAM_CODEC` | written **raw**, no length | `ItemStack.OPTIONAL_STREAM_CODEC` — **clientbound** |
| `DELIMITED_STREAM_CODEC` | `registryFriendlyLengthPrefixed` | `OPTIONAL_UNTRUSTED_STREAM_CODEC` — serverbound |

`ItemStack`'s `OPTIONAL_STREAM_CODEC` field is the join: clientbound stacks are built on the
**undelimited** one. So there is no length to skip and no self-describing framing
to walk, and the delimited variant exists precisely so a *server* can safely skip a
hostile client's junk — the asymmetry is deliberate.

**The only way to stop a given component being a decode cliff is to model it.**
That makes each unmodeled component a latent truncation bug for the whole packet,
not merely a lost field, and it is the reason `minecraft:max_stack_size` and
`minecraft:max_damage` are decoded despite no server ever sending them.

## How it works

`read_armor_trim` mirrors `ArmorTrim`'s `STREAM_CODEC` field: a
`Holder<TrimMaterial>` then a `Holder<TrimPattern>`. Each holder is a VarInt where
`0` introduces an **inline** definition and any positive value references the
registry at `value - 1`. Both forms are read, because both must be — consuming the
wrong byte count for the inline form desyncs the rest of the packet exactly as the
cliff above does.

Inline bodies, from the two `DIRECT_STREAM_CODEC`s:

* `TrimMaterial` — a `MaterialAssetGroup` (one UTF-8 asset suffix, then a VarInt
  count of `(key, suffix)` override pairs) then a description `Component` (network
  NBT).
* `TrimPattern` — an `Identifier`, a description `Component`, then a `bool` decal.

The result is two bare registry **paths** (`"netherite"`, `"silence"`), the form
`lodestone_assets::trim::{trim_material, trim_pattern}` keys its sprite tables by.

## The other components modeled for the same reason

`minecraft:trim` was the first, not the only one. Every arm in this table exists
because a component with no renderer at all was truncating the packet it rode in:

| component | payload | what it broke while unmodeled |
|---|---|---|
| `minecraft:trim` | two holders, either inline or by reference | a trimmed armour stack truncated the rest of the packet |
| `minecraft:map_id` | one `VarInt` | a filled map in any inventory did the same |
| `minecraft:pot_decorations` | a `VarInt` count then that many bare item ids | **the join** — see below |

`minecraft:pot_decorations` is the one that mattered most, because its payload is
not exotic and its carrier is not optional. Vanilla ships the advancement
`adventure/craft_decorated_pot_using_only_sherds` with a `minecraft:decorated_pot`
icon, and an advancement icon is an `ItemStackTemplate` — whose
`read_item_stack_template` turns an incomplete patch into a **fatal** decode error
rather than a partial stack, because everything after it in the packet is
unreadable. So every server that had sent an advancement tree lost its whole
`update_advancements` packet, during the initial world load.

Its wire shape is `PotDecorations.STREAM_CODEC` =
`ByteBufCodecs.registry(Registries.ITEM).apply(ByteBufCodecs.list(4))`. Two things
to get right, both re-read from the jar rather than inferred:

* **`ByteBufCodecs.registry` is `idMapper`** — `VarInt.write(id)`, with no `+1` and
  no `0` sentinel. That is *not* `ByteBufCodecs.holder`, which `minecraft:trim` two
  arms above does use. Adding an offset here consumes the right number of bytes and
  reports the wrong four sherds, which no round-trip against our own encoder can
  see.
* **`list(4)` is a maximum, not a fixed width.** A vanilla server always writes
  four (`PotDecorations::ordered` builds a four-element list unconditionally), but a
  shorter list is legal and its missing tail is `Optional.empty()` —
  `PotDecorations::getItem`'s `i >= sherds.size()` arm.

`minecraft:brick` on a face decodes to `None`, mirroring `getItem`'s
`item == Items.BRICK ? Optional.empty() : Optional.of(item)`: a brick face and a
blank face are the same state in vanilla, by construction.

Only the decode half is done. The render rig — a decorated pot's four
independently textured sprites — is separate and untouched.

## Dropping the packet is safe, and that is now pinned

The driver is fail-open on `AdapterError::Decode`: it logs *"dropping undecodable
packet and continuing session"* and keeps reading. That is only sound if the drop
leaves the reader at the next frame boundary, and it does —
`Connection::read_packet` hands the adapter a fully-buffered frame, so however many
bytes the failing decode consumes, the count cannot reach the next frame.

`crates/protocol/v770/tests/undecodable_packet_resync.rs` is the gate, and it
brackets the byte count from both sides because either would desync a decoder
reading from the *stream*:

| arm | the failing decode consumes |
|---|---|
| an advancement icon with an unmodeled component | far fewer bytes than the frame holds — `read_component_patch` returns at the unmodeled arm |
| the same frame with its advancement count inflated | the whole frame, then asks for more |

Both then require the *next* packet's exact decoded content. The frame is over
16 KiB so it is assembled from several transport reads, with a control asserting the
fixture really is that big.

It lives in `lodestone-v770` rather than `lodestone-client` deliberately.
`lodestone-client`'s own `decode_error_drops_packet_and_keeps_session` asserts the
same shape against a `FakeAdapter` that rejects the packet **without reading a byte
of the payload**, against empty payloads — exemplary source, but the consumption
profile is the whole variable here, so it cannot tell a resynchronised reader from
nothing to consume. The decoder has to be the real one.

**Corollary for diagnosis:** because the drop is sound, a transport-level codec
error arriving after one is *not* caused by it. Look upstream at the byte stream,
not at the adapter.

## How to change it, and the gotchas

* **`Registries.TRIM_MATERIAL` and `TRIM_PATTERN` are dynamic registries.** Their
  ids come from the Configuration-phase `registry_data` sync, and this client keeps
  no dynamic-registry store — so a reference-form holder has nothing to resolve
  against. `adapter/inventory.rs`'s `TRIM_MATERIAL_IDS`/`TRIM_PATTERN_IDS` are the vanilla
  **bootstrap order** (`TrimMaterials.bootstrap`, `TrimPatterns.bootstrap`), which
  is what a server without a trim datapack assigns. Exact for vanilla,
  **provisional** for a modded server — the same posture and caveat as
  `server_protocol.rs`'s `BIOME_NAMES`. An out-of-range id yields an empty string
  rather than an error: the bytes are consumed either way, which is the property
  that keeps the rest of the packet readable.
* **Do not read those tables from `lodestone_assets::trim`.** `TRIM_MATERIALS`
  there happens to be in registry order today; `TRIM_PATTERNS` beside it is
  **alphabetical**. "The asset table is in registry order" is a coincidence for one
  of the two and cannot be relied on for either.
* **The inline material carries no registry name**, only its asset suffix. That is
  what is reported, and for every vanilla material the suffix *is* the registry path
  (`MaterialAssetGroup::create(base)`); it is also the half `trim_sprite_id` needs.
* `lodestone-game`'s own `ComponentMap` has no trim representation, so
  `ItemStack -> game -> ItemStack` drops it. That is listed with the other lossy
  fields on that conversion's doc, not a silent gap.

## Configuration

None.

## Dependencies

`lodestone_model::ArmorTrim`; `lodestone_data::generated::data_component_types` for
`minecraft:trim`'s own component-type registry id (56 in 26.2, resolved by name).
Consumers: `lodestone_assets::trim` for the sprite tables, and eventually the
shell's equipment-layer renderer.
