# Paper inventory bridge

## What it is

The inventory substrate for the optional Java compatibility host. It gives a
host an owned, authoritative snapshot of a connected player's native inventory
without exposing a connection task, mutable menu state, or a second inventory
simulation.

## How it works

`lodestone_server::players::PlayerRegistry` keeps one copied
`PlayerInventory` per registered player. The connection task remains the only
writer: it publishes the restored inventory at the start of Play and republishes
after each inbound packet. A host asks the registry
for a clone keyed by the player's stable UUID; disconnect removes that clone
with the player's registration. Other network clients never see it because
`PlayerView` deliberately excludes inventories.

The first mutation primitive is
`lodestone_server::PlayerInventory::set_native_count`. It changes only the
count of an existing native-slot stack and keeps its modeled component patch
attached. A stack marked `ItemComponents::has_unmodeled` is refused before the
count changes. This is the compatibility policy for data this build cannot
serialize: preserve it by retaining the original stack, or fail before a
mutation; never replace it with a partial stack. A zero count is also refused
here because clearing a slot has separate menu, synchronization, and event
semantics.

The Java bridge must use this snapshot boundary and count mutation rather than
cache inventory state on its worker. It must also preserve the explicit
refusals: a missing player, invalid slot, empty slot, zero count, or unmodeled
component is an error, not an empty item or a successful no-op.

The first JNI read is `playerHandleNativeItemKey(long, int)`. It resolves a
generation-checked player handle, sends only the copied UUID and native slot
through a bounded host port, and returns a key string or `null` for a real
empty slot. It intentionally does not return a Java item object. If the host
reports an unmodeled component, the call throws before returning a partial
projection. Count-changing JNI operations remain unexposed until their
connection-task handoff can apply the same pre-mutation refusal and container
resynchronization.

## How to change it

Add a new Java-visible inventory operation only after identifying the native
slot or menu lifecycle it consumes. Read-only queries may project a copied
stack, but writes must either retain every component on the original stack or
return a typed refusal before touching `PlayerInventory`. Do not put an
inventory borrow, a connection handle, or a menu object in `PlayerRegistry`;
the clone is what keeps foreign callbacks unable to mutate live server state.

If an operation needs to clear or replace a stack, define its authority,
container synchronization, and event order first. It cannot be smuggled into
the amount-only path.

## Configuration

There is no user configuration. The registry mirror is populated only when a
server construction has a `PlayerRegistry`; singleplayer paths without a
shared registry retain their existing connection-local inventory behavior.

## Dependencies

The substrate is implemented by `lodestone_server::server::serve_play`,
`lodestone_server::players::PlayerRegistry`, and
`lodestone_server::inventory::PlayerInventory`. The optional JVM bridge is a
consumer of the public snapshot and does not add a JVM dependency to these
server modules.
