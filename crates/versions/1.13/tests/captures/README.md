# Committed captures for the 1.13 era

Real bytes from real vanilla servers. Nothing here is generated from this
crate, and nothing here should be edited by hand.

## The files

| file | what it is | recorded by |
|---|---|---|
| `join_1_13_2.txt` | a real protocol-404 join, through this crate's own adapter | `tests/capture_join.rs::record_1_13_2` |
| `join_1_12_2.txt` | a real protocol-340 join, through a hand-written handshake and **no** adapter | `tests/capture_join.rs::record_neighbour_1_12_2` |
| `join_1_14_4.txt` | a real protocol-498 join, likewise | `tests/capture_join.rs::record_neighbour_1_14_4` |
| `entity_types_1_13_2.txt` | the type id a real 1.13.2 server put on the wire for each entity it was asked to summon | `tests/entity_types.rs::record_entity_type_ids_from_the_wire` |
| `neighbour_names.rs` | **generated**: the two neighbouring protocols' clientbound packet names, from `minecraft-data` | the procedure in its own header |

## Join capture format

One packet per line, comments start with `#`:

```text
<state> <packet id> <body as lowercase hex>
```

`state` is `login` or `play`; `packet id` is decimal, as that protocol's own
table numbers it; the body is the decompressed packet payload with the id
varint already stripped. A capture is evidence about the wire, so it records
every id the server sent, including packets this family does not translate.

Two caps keep the files reviewable: at most three bodies per packet id, and at
most **one** `map_chunk` — one real column is enough to exercise the paletted
decode, the inline light arrays, the biome tail and the trailing-byte check,
and a second would add 30 kB for nothing.

## Why the two neighbour captures exist

1.13.2 is a one-member era, so the "same crate, wrong protocol" control the
other era crates use has nothing to misroute *to*. What it has instead is two
era boundaries. Feeding a real 1.12.2 join and a real 1.14.4 join to the 404
adapter is what makes "1.13.2 belongs to neither neighbouring era" a
measurement rather than a claim about `minecraft-data`.

They are recorded without any adapter because a version crate may not depend
on a sibling version crate, and does not need to: the handshake is a protocol
VarInt, a host string, a port and a next-state VarInt in all three protocols,
and login `set_compression`/`success` are ids 3 and 2 in all three.

## Entity-type transcript format

```text
<name> living <type id>     # arrived in spawn_entity_living
<name> object <type id>     # arrived in spawn_entity
<name> refused <reason>     # vanilla's own reply, verbatim
```

The name went in through vanilla's own `/summon`, the id came back off
vanilla's own wire, and the two were correlated by the summoned entity's own
UUID — read back over RCON from the entity itself, not inferred from timing.
Which packet carried the id is recorded because at 404 the two index
**different** id spaces; see [`docs/protocol-1-13-era.md`](../../../../docs/protocol-1-13-era.md).

## Re-recording

Each needs its own oracle up first:

```text
./scripts/live-oracles/legacy.sh 1.13.2
cargo test -p lodestone-v1-13 --test capture_join -- --ignored --nocapture record_1_13_2
cargo test -p lodestone-v1-13 --test entity_types -- --ignored --nocapture record_entity_type_ids_from_the_wire

./scripts/live-oracles/legacy.sh 1.12.2
cargo test -p lodestone-v1-13 --test capture_join -- --ignored --nocapture record_neighbour_1_12_2

./scripts/live-oracles/legacy.sh 1.14.4
cargo test -p lodestone-v1-13 --test capture_join -- --ignored --nocapture record_neighbour_1_14_4
```

A re-recorded capture will not be byte-identical to the committed one — entity
ids, uuids and timings differ per run — so re-record only when something on
the wire is actually in question, and expect the asserted misroute split in
`capture_join.rs` to need re-deriving if the set of captured ids changes.
