# Protocol 1.21.11 container sessions

## What it is

This document describes the hosted protocol-774 container bridge. It carries a
basic chest session from the server's real inventory producers to the client
read model: opening the menu, sending its contents, applying single-slot
corrections, and closing it.

## How it works

Protocol 774 uses a fixed `minecraft:menu` registry for the menu id in
`open_screen`; the generic chest menu is registry id `2`. Titles use anonymous
network NBT. Container snapshots and slot corrections carry a VarInt window id,
VarInt state id, signed-short slot indexes, and component-shaped item slots.

`lodestone-v1-21-11::V774ServerProtocol` emits these frames from the shared
`ServerProtocol` encoders. `V774Adapter` decodes each slot through the era's
item registry bridge and emits `ClientEvent::ScreenOpened`,
`ClientEvent::ContainerContent`, or `ClientEvent::ContainerSlot`. The existing
session fold then persists those events in the live menu, where prediction and
authoritative corrections meet.

The server encoder currently writes bare item stacks (zero added and removed
component counts). An item outside the protocol registry, a non-positive count,
or a count too large for the wire integer is sent as an empty slot; this keeps
the encoder from inventing a registry id when the trait cannot return an error.
The decoder preserves the fact that a received stack had opaque components via
`ItemComponents::has_unmodeled`.

## How to change it

When a protocol-774 menu entry changes, update `MENU_NAMES_774` in both the
adapter and server protocol modules and add an asymmetric literal-id test. Keep
the clientbound packet tests built from independent bytes: a codec round trip
can preserve the same mistaken field order on both sides. If item components
become encodable, extend the slot writer and add a captured component fixture;
do not silently serialize canonical component ids as protocol-774 ids.

The adapter's content count is capped at 1024 before allocation. Raise that
bound only with a measured protocol requirement and a malformed-count control.
The protocol-774 item registry inverse lives in `item_registry.rs`; update it
there rather than indexing the canonical registry directly.

## Configuration

No environment variables or feature flags are needed. The bridge is compiled
when the `v1-21-11` family is enabled and is selected by the registry for
protocol `774`.

## Dependencies

The bridge depends on `lodestone-core` for VarInt/NBT framing,
`lodestone-model` for canonical item stacks and container events,
`lodestone-data` for item registry identities, and `lodestone-server` for the
shared production container encoders and consumer path.
