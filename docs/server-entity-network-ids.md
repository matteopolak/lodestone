# Server entity network IDs

## What it is

The server vehicle simulation and connected-player registry expose typed
`EntityNetworkId` APIs for spawning, mounting, observation, movement, player
updates, and dismounting. The adapters make the server-owned versus
plugin-owned id domains explicit while packet-facing snapshots and event logs
retain raw integers only at their lowering boundaries.

## How it works

`EntityNetworkId::Server` values are converted to the signed integer keys used
by the vehicle store only after a checked range conversion. `EntityNetworkId::Plugin`
values and server variants outside the signed wire range are rejected, so a
plugin-local id cannot address a vehicle accidentally. IDs returned by the
vehicle store are classified before they leave the adapter.

`boat::apply_boat_item` uses the typed spawn adapter and converts back to a raw
integer only when constructing its protocol-facing placement result. The
vehicle tests exercise the typed spawn, mount, rider lookup, and dismount path,
including a negative control for a plugin-owned id. `PlayerRegistry` stores
typed IDs in its tracked players and tickets, routes its position, rotation,
flags, view, swing, and drop paths through typed methods, and lowers to the
signed `EntitySnapshot`, command-candidate, and swing-event fields only when
those existing compatibility surfaces require them. Its focused test checks
that a typed player update reaches the produced remote snapshot and that a
plugin-owned ID cannot mutate or enter the event log.

## How to change it

Use the `*_typed` methods for native/plugin-facing vehicle and player
operations. Keep raw methods at packet, command, and compatibility boundaries
until their callers have an explicit `EntityNetworkId::from_wire` conversion.
When adding a query or mutation, provide both directions of the checked adapter
and test a plugin id as well as a normal server id. A player ticket's
`entity_network_id` is the typed handle to pass between server subsystems;
`entity_id` remains the signed compatibility accessor for existing protocol
code.

## Configuration

None. The adapters use the existing `EntityNetworkId` representation and do
not change protocol feature selection or vehicle physics.

## Dependencies

The adapters depend on `lodestone_model::EntityNetworkId` and the existing
`lodestone-server` vehicle and player stores. Packet encoders, command
candidate fields, event logs, and the `MobSim` maps remain the explicit raw-id
boundaries.
