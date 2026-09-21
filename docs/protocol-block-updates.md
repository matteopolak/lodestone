# Protocol Block Updates

## What it is

The server protocol seam encodes one block edit confirmation for each hosted protocol family. Runtime callers provide the canonical `lodestone_data::block_states::StateId`; the selected protocol maps that id directly to its local wire registry.

## How it works

`ServerProtocol::encode_block_update` and each version adapter's fallible helper carry `StateId` by value. Legacy adapters use their checked inverse tables, while modern adapters use the canonical id where the wire registry is identical. An unrepresentable canonical state returns an encoding error instead of being replaced with air.

Text parsing is limited to explicit import and test-fixture boundaries. No per-edit protocol encoder formats or reparses a block-state name.

## How to change it

Add a protocol-local numeric mapping beside the adapter's chunk mapping, then call it from that adapter's `try_encode_block_update`. Preserve the existing packet position and state-id bytes. Update the adapter's focused wire fixture with a `StateId` resolved once at the fixture boundary.

## Configuration

The hosted protocol feature selects the adapter. There are no block-update-specific environment variables.

## Dependencies

The seam depends on `lodestone-data` for canonical `StateId`, `lodestone-server` for `ServerProtocol` and `ServerDirective`, and each protocol crate's packet writer and canonical-to-wire inverse table.
