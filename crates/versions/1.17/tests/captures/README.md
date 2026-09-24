# 1.17-era join captures

Clientbound bytes recorded from real vanilla servers, one file per protocol in
this era:

| file | Minecraft | protocol | oracle |
|---|---|---|---|
| `join_1_17_1.txt` | 1.17.1 | 756 | `./scripts/live-oracles/legacy.sh 1.17.1` |
| `join_1_18_2.txt` | 1.18.2 | 758 | `./scripts/live-oracles/legacy.sh 1.18.2` |

## Format

One packet per line, blank lines and `#` comments ignored:

```text
<state> <packet id, decimal> <body, lowercase hex>
```

`state` is the connection state the client was in when the packet arrived
(`login` or `play`). The id is that protocol's own number for the packet, so
the two files are not comparable line by line — fifteen clientbound play ids
differ between them. The body is post-decompression and excludes the id
VarInt.

## What they are evidence for

Both protocols' shapes were read from `minecraft-data`, which is
cross-check-grade rather than authoritative. These files are the authority:
bytes a real server sent. They are what the chunk decoder was settled against,
and they are what caught the one thing no round-trip could — 1.18's
`chunkData` buffer is *longer* than the sections inside it, by one zero byte
per section whose block palette is single-valued (see `packets/chunk.rs`'s
module docs for the three columns that measurement came from).

`tests/capture_join.rs` holds both halves: the `#[ignore]`d recorder that
wrote these, and the hermetic replay that consumes them on every `cargo test`.

## Caps, and why the files are this size

The recorder keeps at most **three** bodies of any one packet id and at most
**one** `map_chunk`. A join sends hundreds of columns and thousands of entity
moves; the hundredth adds nothing a reviewer or a replay can use, and a
multi-megabyte committed file is a burden on every later checkout. Every
*distinct* id the wire produced is still represented, which is the property
these files are evidence for. One real column is enough to exercise the
paletted decode, the biome placement, the section count and the trailing-byte
checks — and at 758 the column is most of the file, because that protocol
folds the light payload into the same packet.

Packets this crate does not translate are recorded too. Trimming a capture to
the packets already handled would make it agree with the port by construction.

## Re-recording

```text
./scripts/live-oracles/legacy.sh 1.17.1
cargo test -p lodestone-v1-17 --test capture_join -- --ignored --nocapture record_1_17_1
```

Repeat with `1.18.2` / `record_1_18_2`. Each version's oracle listens on its
own port, so the two are independent.

Two things the replay asserts that a re-record can break. The world must be
**flat and untouched**: the floor check reads uniform bedrock, dirt and grass
back out of `lodestone-world`, so a world someone has built in fails. And the
misroute census in `capture_join.rs` pins the exact split of captured ids that
name a different packet at the other protocol; a re-record with a different
packet mix changes those counts, and the right response is to re-derive them
rather than to widen the assertion.
