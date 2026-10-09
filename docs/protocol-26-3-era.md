# The 26.3 protocol era

## What it is

`lodestone-v26-3` is the client and host family for Minecraft 26.3 (protocol 777): release metadata, packet IDs, game-data translation tables, and a dialect that drives the 26.2 adapter over the 26.3 wire. Hosting is covered in [server-release-hosting.md](./server-release-hosting.md).

## How it works

- The release jar's `version.json` supplies protocol, data and pack versions; its `--reports` generator supplies `packets.json`, from which `src/generated/packet_ids.rs` is produced. The table covers all states and directions, including packets the client does not yet decode. An ID match says nothing about a body's layout.
- `connection_dialect()` feeds these tables to the reusable 26.2 connection core and opts into the shared Configuration `registry_data` decoder (registry name, entry count, ordered entries with optional network NBT). `lodestone_v26_3::adapter()` is what the registry's `v26-3` feature constructs. Layout changes are release-gated branches inside the shared 26.2 adapter, keyed on `GameDataVersion::V26_3`.
- Block IDs shift between releases (1,196 in 26.2, 1,286 in 26.3; `minecraft:oak_log` 49 to 51), so tag member IDs are translated before installing an immutable session-owned snapshot. No tag override is process-global.
- Check packet IDs only after translation or by name: `decode_chunk_packet` matches `minecraft:level_chunk_with_light` by name because 26.3's `keep_alive` uses the number 26.2 gives the chunk packet.

Layout changes found against a live server:

- **`BitSet` is a byte array.** Previously a VarInt count of big-endian 64-bit words; now a VarInt count of bytes, LSB-first, trailing zeros trimmed. It affects every light mask (`level_chunk_with_light`, `light_update`) and the chat `FilterMask`. `lodestone_world::BitSetWire` and `bit_set_wire` select the framing.
- **The `tag` slot display is an item holder set**: VarInt 0 plus a tag identifier, or VarInt n plus n-1 inline item ids (appears in `recipe_book_add`).
- **`post_effects`** (Configuration and Play) names screen shader chains. An empty list is accepted; a non-empty one is logged and dropped, since there is no post-effect pipeline.
- **Teleport acknowledgements carry the pose and are not followed by a movement echo**: `accept_teleportation` is the id, three doubles, two floats. The server disconnects a client that sends two positioned movement packets in one tick, which join placement plus an immediate second teleport triggered at high frame rates, so `encode_correction_echo` sends none on 26.3 (26.2 still owes one).

## Live gates

- `tests/live_join.rs` (ignored) joins a running offline-mode 26.3 server (`LODESTONE_26_3_SERVER=host:port`, default `127.0.0.1:25580`), stays in Play for 40 s past the first keep-alive and requires at least 25 loaded chunks, no disconnect and zero `ERROR` events. The driver drops undecodable packets, so the error count is the main signal. It covers an idle join only; interaction is not yet exercised.

  ```text
  cargo test -p lodestone-v26-3 --test live_join -- --ignored --nocapture
  ```
- `crates/versions/26.3/tests/capture_connection.py` is a raw TCP oracle that checks the cached server jar's SHA-1, starts it in a temporary Apple container (1 GiB heap, 2 GiB container, bounded temp dir), performs Handshake, Login and Configuration with one fixed test profile, records every clientbound body, and stops after the first answered Play keep-alive. It bypasses the Lodestone adapter. Frames are capped at 8 MiB, total payload at 32 MiB, the run at 100 s.
- The committed `tests/fixtures/connection_26_3.json` holds the Configuration sequence, the 32 registry packet and entry names in wire order, tag registry and tag names, body lengths and SHA-256 of the captured bytes. Three registry bodies and the tag body vary in raw hash between fresh worlds while parsed content is equal, so live comparison uses canonical content and membership hashes. `--raw-output PATH` writes full bodies to a local JSONL (keep it out of the repository).

  ```text
  python3 crates/versions/26.3/tests/capture_connection.py --negative-control
  python3 crates/versions/26.3/tests/capture_connection.py --record --raw-output /tmp/connection-26-3.jsonl
  ```

  `--negative-control` first requires the 777 server to refuse a protocol 776 Login. The capture proves the server accepts this narrow flow, not that Play decoding or online authentication works.
- Oracle jar SHA-1 `33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c`; its `packets.json` `57d738152562d40d7ba3fc4f106431ec4858de40`. The `tests/configuration_registry.rs` test feeds the full captured `minecraft:world_clock` body through the production path (an extra byte or wrong entry count fails).

## How to change it

- Fetch the 26.3 server jar into `.cache/mc/26.3/`, run its `--reports` generator, then regenerate: `cargo xtask gen-packet-ids --version 26.3 --protocol 777 --out crates/versions/26.3/src/generated/packet_ids.rs` (`--check` detects drift). Update the version fixture when a later release is added.
- To find layout changes, decompile both release clients with `.cache/vineflower.jar` into `.cache/mc/<version>/client-src`, diff the network codec sources and confirm each finding with the live gate.
- When extending the Configuration boundary, translate tag member IDs to canonical block IDs first and verify against captured membership.

## Configuration

The `v26-3` feature of `lodestone-registry` registers the family; the shell's default `live` feature enables it. The live gate reads `LODESTONE_26_3_SERVER`. Protocol and pack constants are fixed to the release jar.

## Dependencies

`lodestone-v26-2` is the compatibility base. The release jar and reports are development oracles, not runtime dependencies.
