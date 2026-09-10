# Server entity network IDs

## What it is

The server vehicle simulation exposes typed `EntityNetworkId` adapters for
spawning, mounting, observation, movement, paddle state, and dismounting. The
adapters make the server-owned versus plugin-owned id domains explicit while
the existing packet-facing simulation maps continue to use raw integers.

## How it works

`EntityNetworkId::Server` values are converted to the signed integer keys used
by the vehicle store only after a checked range conversion. `EntityNetworkId::Plugin`
values and server variants outside the signed wire range are rejected, so a
plugin-local id cannot address a vehicle accidentally. IDs returned by the
vehicle store are classified before they leave the adapter.

`boat::apply_boat_item` uses the typed spawn adapter and converts back to a raw
integer only when constructing its protocol-facing placement result. The
vehicle tests exercise the typed spawn, mount, rider lookup, and dismount path,
including a negative control for a plugin-owned id.

## How to change it

Use the `*_typed` methods for native/plugin-facing vehicle operations. Keep raw
methods at the simulation and packet boundaries until their callers have an
explicit `EntityNetworkId::from_wire` conversion. When adding a vehicle query or
mutation, provide both directions of the checked adapter and test a plugin id
as well as a normal server id.

## Configuration

None. The adapters use the existing `EntityNetworkId` representation and do
not change protocol feature selection or vehicle physics.

## Dependencies

The adapters depend on `lodestone_model::EntityNetworkId` and the existing
`lodestone-server` vehicle store. Packet encoders and the `MobSim` maps remain
the explicit raw-id boundaries.
