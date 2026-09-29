# The 26.3 protocol era

## What it is

`lodestone-v26-3` records release metadata and packet IDs for Minecraft 26.3
(protocol 777). It declares `lodestone-v26-2` as a compatibility base, while
client joining and hosting remain unavailable until wire and registry changes
are independently verified.

## How it works

The release jar's `version.json` supplies the protocol, data, and pack versions.
Its `--reports` generator supplies `packets.json`, from which
`src/generated/packet_ids.rs` is produced. The packet table covers all states
and directions, including IDs for packets that the client does not yet decode.
The 26.3 report adds configuration and play packets and shifts existing play
IDs. An ID match says nothing about a packet body's shape.
`connection_dialect()` supplies these tables to the reusable 26.2 connection
core. That dialect rejects Play and configuration registry/tag payloads until
their 26.3 bodies and registry mappings are verified.
The release server jar used for these tables has SHA-1
`33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c`; its generated
`packets.json` has SHA-1 `57d738152562d40d7ba3fc4f106431ec4858de40`.

## How to change it

Fetch the official 26.3 server jar into `.cache/mc/26.3/`, run its `--reports`
generator there, and regenerate the packet table with
`cargo xtask gen-packet-ids --version 26.3 --protocol 777 --out crates/versions/26.3/src/generated/packet_ids.rs`.
Run the same command with `--check` to detect drift. Compare metadata against
the release jar and update the compact version fixture if a later release is
added. Before registering an adapter, verify changed packet bodies and registry
payloads against a real server capture; the 26.2 decoder cannot safely resolve
26.3 IDs by changing the handshake number alone.

## Configuration

The crate has no runtime flags or environment variables. Its protocol and pack
constants are fixed to the release jar. The optional registry feature that
would expose an adapter is intentionally absent at this stage.

## Dependencies

`lodestone-v26-2` supplies the reusable compatibility base. The official
release jar and its generated reports are development oracles and are not
runtime dependencies.
