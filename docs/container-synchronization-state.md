# Container synchronization state

## What it is

`lodestone_model::ContainerStateId` is the version-free identity for the revision counter that ties a predicted menu click to authoritative container updates. It replaces raw integer state identifiers in the client event/action model and menu reconciliation path.

## How it works

The model keeps the counter as `u32`, while `from_wire(i32)` and `as_wire()` retain the protocol's signed VarInt bit pattern at adapters. `Menu` stores the typed value, `ClientMenu` transfers it unchanged between predicted and confirmed menus, and `ClickIntent` carries that same value into `ClientAction::ContainerClick`. Incrementing uses `ContainerStateId::next`, making the wrap operation visible at the one mutation point.

Legacy adapters that have no revision field emit `ContainerStateId::INITIAL`. Adapters with a signed wire field create the type on decode and call `as_wire()` immediately before encoding, so no local reconciliation code widens or narrows a packet integer itself.

The 26.2 click packet contains hashes of the item's component patch, not the component values.
Plain stacks have an empty patch and can be predicted locally. For a stack with a nonempty patch,
the client does not yet have a complete hash encoder: it sends the click without claimed slot or
cursor snapshots and waits for the server's slot/cursor updates. This keeps custom server items
authoritative instead of leaving a ghost cursor item after two quick clicks. The decoded patch
presence stays separate from prototype-derived item properties such as maximum stack size.

## How to change it

Keep the type in `lodestone-model`, because both packet adapters and the version-free game model need it. Add a conversion method only when a real protocol boundary requires one; internal menus, events, actions, and test fixtures should construct `ContainerStateId` directly. Preserve the round-trip and wrapping controls when changing its representation.

To enable prediction for patched stacks, implement the protocol's component hashes in the adapter
and carry enough patch data through the model and game stack. Do not infer an empty patch from an
empty subset of modeled components: a server can send components this client does not decode.

## Configuration

There is no runtime configuration. The initial value is `ContainerStateId::INITIAL`.

## Dependencies

It depends on `lodestone-model` only. Consumers are `lodestone-game`'s menu/reconciliation types and the protocol adapters that translate container packets.
