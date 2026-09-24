# 1.19-era join captures

Clientbound bytes recorded from real vanilla servers. Three files for one
protocol, because this is a **singleton era**:

| file | Minecraft | protocol | served by this crate? | oracle |
|---|---|---|---|---|
| `join_1_19_4.txt` | 1.19.4 | 762 | yes | `./scripts/live-oracles/legacy.sh 1.19.4` |
| `join_1_18_2.txt` | 1.18.2 | 758 | **no** | `./scripts/live-oracles/legacy.sh 1.18.2` |
| `join_1_20_6.txt` | 1.20.6 | 766 | **no** | `./scripts/live-oracles/legacy.sh 1.20.6` |

A multi-protocol era gets its negative control for free: feed one member's
bytes to another member's adapter. A singleton has no sibling to misroute
against, so the control comes from outside — real bytes from the versions on
either side of the break, replayed through the only adapter this crate has.

## Format

One packet per line, blank lines and `#` comments ignored:

```text
<state> <packet id, decimal> <body, lowercase hex>
```

`state` is the connection state the client was in when the packet arrived
(`login`, `configuration` — 1.20.6 only — or `play`). The id is that
*protocol's* own number for the packet, so the three files are not comparable
line by line. The body is post-decompression and excludes the id VarInt.

## What they are evidence for

The packet shapes were read from `minecraft-data`, which is cross-check-grade
rather than authoritative. These files are the authority: bytes a real server
sent. Three things they settle that no round-trip against our own encoder
could:

* **The chunk buffer is longer than its contents**, still, at 762. The
  committed column declares 2,268 bytes across a 24-section window and leaves
  23 over — one zero byte per section whose block palette is single-valued.
  The exact-length assertion every other protocol here relies on is false.
* **The vertical window is a registry lookup.** The join packet names a
  dimension *type* and ships the registry separately, so `min_y` and `height`
  come from a walk into the blob rather than from a field.
* **Chat works in both directions.** The recorder sends one message and the
  server broadcasts it straight back as `player_chat`. The server would close
  the connection on a malformed serverbound chat packet rather than ignore it,
  so the message arriving at all is the check on this crate's signing tail;
  the decode of what comes back is the check on the clientbound side.

`tests/capture_join.rs` holds both halves: the `#[ignore]`d recorders that
wrote these, and the hermetic replay that consumes them on every `cargo test`.

## How the neighbour captures were recorded

Not through this crate's adapter, and not by choice. **The 762 adapter cannot
join either neighbour**, because of one byte at the end of `login_start`: 758
reads a bare username and treats 762's presence byte as the start of the next
packet, and 766 reads a *required* 16-byte profile UUID and rejects 762's
single `false` byte outright. So `record_neighbour` writes its own handshake
and login, which needs only the handshake's four fields and two login packet
ids that happen to be the same number in all three protocols. 1.20.6 needs
three more: it inserts a configuration phase between login and play, and holds
the connection there until the client answers its known-packs question.

That also keeps the control free of any dependency on another version crate —
which the isolation lint forbids, and which would in any case make the control
agree with a sibling port rather than with the wire.

## Caps, and why the files are this size

The recorder keeps at most **three** bodies of any one packet id and at most
**one** `map_chunk`. A join sends hundreds of columns and thousands of entity
moves; the hundredth adds nothing a reviewer or a replay can use, and a
multi-megabyte committed file is a burden on every later checkout. Every
*distinct* id the wire produced is still represented, which is the property
these files are evidence for. One real column is enough to exercise the
paletted decode, the biome placement, the section count and the trailing-byte
checks — and the column is most of the file, because this protocol folds the
light payload into the same packet.

Packets this crate does not translate are recorded too. Trimming a capture to
the packets the port already handles would make it agree with the port by
construction, which is the opposite of what an outside oracle is for.
