# Hosting protocol 777 (26.3)

## What it is

The integrated server hosts protocol 777 (26.3) and no other modern protocol: 776 is joined, never hosted. The implementation is the `V770ServerProtocol` code in the 26.2 crate, parameterised by release. A `ServerRelease` (the 26.3 dialect plus Configuration payloads captured from a vanilla 26.3 server) is threaded through every encoder as a `Wire`, so a 26.3 client is never handed a 26.2 packet id, registry id, block-state id or body layout.

## How it works

`lodestone_v26_3::server_protocol()` builds `V770ServerProtocol::for_release(server_release())`; the registry's `v26-3` server family returns it for 777. The unit value `V770ServerProtocol` is the base-layout implementation (a same-named const with `Wire::BASE`); the registry no longer offers it for 776, and it survives as the in-process fixture the 26.2 crate's server tests drive. Retiring it entirely means making the 26.3 release the base wire, which rewrites every encoder.

Three translation points, all by name or by registry rather than by number:

- **Outgoing packet ids.** Encoders keep writing the 26.2 ids. `ServerProtocol::outbound_id_map` hands the connection a `PacketIdMap`, applied once at the write point (`lodestone_net::Connection::write_packet_in`). Packets 26.2 does not have (`swing_animation`) use a placeholder id from `RELEASE_ONLY_BASE` that the map resolves by name.
- **Incoming packets.** `decode_release` maps the wire id to a name; the bodies that changed (`punch`, `accept_teleportation`, `sign_update`) use the layouts in `packets::release_layout`, everything else is translated to its 26.2 id and decoded as before.
- **Ids inside bodies.** `server_protocol::wire::Wire` converts block states, items, biomes, block-entity, entity, menu, particle, sound, data-component, attribute, command-parser and custom-stat ids. Chunks keep light computed over canonical states; `encode_column_body` rebuilds the column in the release's block-state (16-bit direct palette) and biome (7-bit) ids and writes the byte-array light `BitSet`.

Body layouts that differ and are written by the encoders under `Wire::is_latest`: `explode` (trailing sound), `level_particles` (particle first, three speeds, distribution kind), `animate` (renumbered; arm swings are the separate `swing_animation` packet) and the recipe `tag` slot display (a holder set).

Configuration replays the release's captured burst (`ServerRelease::config`): 32 `registry_data` packets in the vanilla server's own order, then `update_tags`. Biome holder ids come from the captured biome registry.

## How to change it

- A new id-bearing field: take a `Wire` in the encoder and convert through `Wire::fixed`, `Wire::state` or `Wire::item_by_name`. The base (`Wire::BASE`) path is the layout the 26.2 crate's tests assert; keep it byte-identical or move those tests with it.
- A release-only packet: add its name to `RELEASE_ONLY_PACKETS` and send it with the matching placeholder id.
- Recapture the Configuration fixtures: `python3 crates/versions/26.3/tests/capture_connection.py --raw-output /tmp/raw.jsonl`, then `python3 crates/versions/26.3/tools/gen_server_config_fixtures.py /tmp/raw.jsonl`. This rewrites `crates/versions/26.3/fixtures/server-config/` and `src/generated/server_config.rs`.
- Serverbound `player_action`: 26.3 inserts a destroy-direction action after start-destroy, so `V770Adapter::player_action_ordinal` shifts the client's ordinals and `decode_release` shifts them back (dropping the new action).
- Gotcha (known divergence): the worldgen bundle is still the 26.2 one (`WorldgenScope::V26_2`); the bundled loot tables, recipes, item tags and stored predicates follow `mc-version` (`scripts/regen-server-data.py`, `just regen-loot-corpus`). The staged port is in `docs/backlog.md`.

## Configuration

No flags. The shell's default protocol is 777 (`Config::protocol`); `--protocol 776` still joins a 26.2 server but singleplayer is unavailable on it.

## Dependencies

`lodestone-v26-2` (shared encoders and dialect), `lodestone-data` (`GameDataVersion` id tables), `lodestone-net` (`PacketIdMap`), `lodestone-registry` (the `v26-3` `ServerFamily`).

Tests: `crates/versions/26.3/tests/hosted_join.rs` joins the real 26.3 client decoder to the hosted server; `hosted_join_control.rs` is its control, proving a 26.2-framed server fails the same client.
