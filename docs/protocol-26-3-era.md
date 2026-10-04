# The 26.3 protocol era

## What it is

`lodestone-v26-3` is the join-only client family for Minecraft 26.3
(protocol 777): release metadata, packet IDs, game-data translation tables, and
a dialect that drives the 26.2 adapter over the 26.3 wire. Hosting stays on
protocol 776.

## How it works

The release jar's `version.json` supplies the protocol, data, and pack versions.
Its `--reports` generator supplies `packets.json`, from which
`src/generated/packet_ids.rs` is produced. The packet table covers all states
and directions, including IDs for packets that the client does not yet decode.
The 26.3 report adds configuration and play packets and shifts existing play
IDs. An ID match says nothing about a packet body's shape.
`connection_dialect()` supplies these tables to the reusable 26.2 connection
core. It opts into the shared Configuration `registry_data` decoder: the
captured 26.3 body has one registry name, an entry count, and ordered entries
with optional network NBT, matching that decoder's strict framing. The adapter
folds these into its per-connection registry store. The complete captured
`minecraft:world_clock` body in `tests/configuration_registry.rs` exercises the
production adapter path; an extra byte and a wrong entry count both fail.
The official
registry reports have 1,196 block IDs in 26.2 and 1,286 in 26.3; for example,
`minecraft:oak_log` moves from 49 to 51. Passing 26.3 tag member IDs to the
26.2 block identity table would name the wrong blocks. Selected identity maps
now translate members before installing an immutable session-owned snapshot;
no network tag override is process-global.

### Play

`connection_dialect()` admits Play, and `lodestone_v26_3::adapter()` is what
the registry's `v26-3` feature constructs. Bodies whose layout changed in 26.3
are read by release-gated branches inside the shared 26.2 adapter, keyed on
`GameDataVersion::V26_3`. Layout changes found against a live server:

- **`BitSet` is a byte array.** Through 26.2 a bit set is a VarInt count of
  big-endian 64-bit words; 26.3 sends a VarInt count of bytes, LSB-first, with
  trailing zero bytes trimmed. This affects every light mask (in
  `level_chunk_with_light` and `light_update`) and the partially-filtered chat
  `FilterMask`. `lodestone_world::BitSetWire` selects the framing, and
  `bit_set_wire` maps a release to it.
- **The `tag` slot display is an item holder set**: VarInt 0 then a tag
  identifier, or VarInt n then n-1 inline item ids. Older releases send a bare
  tag identifier. It appears in `recipe_book_add`.
- **`post_effects`** (Configuration and Play) names screen shader chains. An
  empty list is the default unfiltered screen and is accepted; a non-empty one
  is logged and dropped, since the renderer has no post-effect pipeline.
- **Teleport acknowledgements carry the pose, with no movement echo after
  them.** `accept_teleportation` is the id, three doubles and two floats
  (`complete_teleport_response`). The server also now **disconnects a client
  that sends two positioned movement packets in one tick** ("Invalid move
  player packet received"). 26.2 still owes a full movement packet after each
  acknowledgement. On 26.3 the join placement and an immediate second teleport
  put two such echoes into one tick, a kick that showed up only at high frame
  rates, so `encode_correction_echo` sends none there.

Any check on a packet ID must happen after translation or by name:
`decode_chunk_packet` runs before the general translation and matches
`minecraft:level_chunk_with_light` by name, because 26.3's `keep_alive` uses the
number 26.2 gives the chunk packet.

`tests/live_join.rs` is the acceptance gate. It is ignored by default and joins a
running offline-mode vanilla 26.3 server
(`LODESTONE_26_3_SERVER=host:port`, default `127.0.0.1:25580`). It stays in Play
for 40 s, past the first keep-alive, and requires at least 25 loaded chunks, no
disconnect, and zero `ERROR` events. The driver drops undecodable packets and
carries on, so the error count is the only signal for most layout mistakes.

```text
cargo test -p lodestone-v26-3 --test live_join -- --ignored --nocapture
```

The gate covers an idle join. Interaction (block edits, inventory, combat,
chat) is not yet exercised against a live 26.3 server.
The release server jar used for these tables has SHA-1
`33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c`; its generated
`packets.json` has SHA-1 `57d738152562d40d7ba3fc4f106431ec4858de40`.

### Live connection capture

`crates/versions/26.3/tests/capture_connection.py` is a standalone raw TCP
oracle. It checks that the cached official server jar has the SHA-1 above, then
starts that jar in a temporary Apple container with a 1 GiB Java heap, a
2 GiB container limit, a 128 MiB per-file limit, and a checked 512 MiB
temporary-directory limit. The temporary offline-mode world accepts one fixed test
profile. The probe performs protocol 777 Handshaking and Login, acknowledges
Configuration, records every decompressed clientbound packet body, and stops
after the first Play login and answered Play keep-alive. It does not use the
Lodestone adapter.

The committed `tests/fixtures/connection_26_3.json` records the observed
Configuration packet sequence, all 32 registry packet names and entry names in
wire order, the tag packet's registry and tag names in wire order, body lengths,
and SHA-256 hashes of the exact captured bytes. Small Configuration bodies are
included as complete hex. The optional `--raw-output PATH` writes complete
decompressed bodies to a local JSONL file for packet-by-packet investigation;
keep that large capture out of the repository. A fresh run compares the
fixture's registry contents and tag membership, as well as stable raw bodies.

```text
python3 crates/versions/26.3/tests/capture_connection.py --negative-control
python3 crates/versions/26.3/tests/capture_connection.py --record --raw-output /tmp/connection-26-3.jsonl
```

`--negative-control` first sends a protocol 776 Login and requires the 777
server to disconnect it. The probe caps each frame at 8 MiB, the captured
payload total at 32 MiB, and the connection at 100 seconds. The initial
capture and a fresh server run reached Play and matched the compact fixture.
Three registry bodies (`worldgen/biome`, `dimension_type`, and `timeline`) and
the tag body had varying raw hashes between fresh worlds while their parsed
content was equal; their first-run raw hashes remain provenance, while
the live comparison uses canonical content and membership hashes. Tag registry
and tag-name order is preserved as observed, but is not asserted to be stable
across fresh worlds. This proves the server accepted this narrow connection
flow; it does not prove that the 26.2 client can decode 26.3 Play packets or
that online account authentication works.

## How to change it

Fetch the official 26.3 server jar into `.cache/mc/26.3/`, run its `--reports`
generator there, and regenerate the packet table with
`cargo xtask gen-packet-ids --version 26.3 --protocol 777 --out crates/versions/26.3/src/generated/packet_ids.rs`.
Run the same command with `--check` to detect drift. Compare metadata against
the release jar and update the compact version fixture if a later release is
added. To find layout changes, decompile both release clients with
`.cache/vineflower.jar` into `.cache/mc/<version>/client-src` and diff the
network codec sources. Then confirm each finding with the live gate above.
To extend the Configuration boundary, translate tag member IDs into canonical
block IDs before installing them, then verify against captured membership.

## Configuration

`lodestone-registry`'s `v26-3` feature registers the family, and the shell's
default `live` feature turns it on. The live gate reads `LODESTONE_26_3_SERVER`.
The protocol and pack constants are fixed to the release jar.

## Dependencies

`lodestone-v26-2` supplies the reusable compatibility base. The official
release jar and its generated reports are development oracles and are not
runtime dependencies.
