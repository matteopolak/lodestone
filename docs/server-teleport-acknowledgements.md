# Server teleport acknowledgements

## What it is

26.2 player-position corrections carry a server-issued id. The connection holds its latest id and accepts movement only after the client echoes that same id with `accept_teleportation`.

## How it works

`ServerProtocol` exposes acknowledgement-aware join, respawn, dimension-change, and ordinary teleport encoders without changing legacy families. `V770ServerProtocol` writes the supplied id at the start of every player-position payload and decodes the reply into `ServerBound::TeleportationAccepted`.

`server::TeleportAcknowledgements` is connection-local. Issuing a correction replaces any earlier pending id; a wrong, stale, malformed, or duplicate reply cannot clear the newer pending correction. `dispatch_play_packet` drops player and vehicle movement while an id remains pending, but still processes the matching acknowledgement.

## How to change it

When adding a clientbound player-position producer, allocate its id with `issue_teleport_id` and call the matching `*_with_teleport_id` encoder. Do not send a fixed id or clear the gate from any packet other than `TeleportationAccepted`. A protocol family should opt in with `uses_teleport_acknowledgements` only after every one of its player-position producers writes the supplied id.

## Configuration

There is no runtime configuration. Legacy protocol families keep their previous ungated movement behavior through the trait defaults.

## Dependencies

This depends on `lodestone_server::protocol::ServerProtocol`, `ServerBound`, and the 26.2 `AcceptTeleportation`/player-position packet codecs.
