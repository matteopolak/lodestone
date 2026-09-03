# Protocol 5 wire captures

Bytes recorded from a real Minecraft 1.7.10 server, one file per question they
answer:

| file | what it records | oracle |
|---|---|---|
| `join_1_7_10.txt` | every clientbound packet id a join produces, with bodies | `./scripts/live-oracles/legacy.sh 1.7.10` (`:25602`) |
| `entity_types_1_7_10.txt` | the numeric entity type each name spawns as | same oracle, driven over RCON on `:25603` |

## Why they exist

Protocol 5 predates Mojang's machine-readable packet report by six years, so
there is no first-party dump to port from and no decompiled 26.2 source that
describes it. Every id and shape in this crate came from `minecraft-data`,
which this repo treats as cross-check grade and never as an authority. These
files are the authority.

They are also the only thing that can catch the failure mode this era is full
of: a field order inside a run of same-typed fields. `decode(encode(x)) == x`
is satisfied by two symmetric misunderstandings, and so is a length check —
both orders produce a body of exactly the right size. The bulk chunk packet's
metadata sits *after* its payload here and *before* it at protocol 47; a
single-column packet parses under either reading without erroring, and only a
recorded two-column packet tells them apart.

## Format

One packet per line, `<state> <packet id> <body as lowercase hex>`, with `#`
comments. Ids are as protocol 5's own table numbers them, which is not how any
later protocol numbers the same packet.

`entity_types_1_7_10.txt` uses its own line format, documented in its header:
each row pairs a name with the type id the wire carried for it, and a name the
server's command interface refused is recorded verbatim as a refusal rather
than dropped, so a later reader can tell "this era has no such entity" from
"nobody asked".

## Contents and caps

`join_1_7_10.txt` keeps at most three bodies per packet id, and two for a chunk
packet, in wire order. Every distinct id the join produced is still present —
that is the property the file is evidence for, and the hundredth relative-move
adds nothing the third does not.

The oracle world is the vanilla `FLAT` preset, which is what lets the replay
assert canonical bedrock at `y` 0, dirt at 1, grass at 3 and not-grass at 4.
Those expected ids come from `lodestone_data::block_states`, the jar-derived
26.2 registry, so the assertion is anchored outside this crate at both ends:
real bytes in, a first-party registry out.

## What is deliberately not here

**Single-column `map_chunk` bodies.** A vanilla server of this era streams
terrain entirely through `map_chunk_bulk`; walking a client 320 blocks
produced 420 single-column `map_chunk` packets and every one was a chunk
unload. The unload's exact bytes are pinned in `../chunk.rs` rather than here,
because twelve bytes inline next to the assertion they support are easier to
check than a capture file; the data-bearing single-column path has no vanilla
example to record at all and is built by hand there.

**Movement.** `../live_movement.rs` is a live gate, not a capture, because the
thing it measures is the server's *response* over a 320-block walk — how often
it corrects the client's position, and whether it ever unloads a column. A
recording cannot express that, since a wrong client produces a shorter
transcript rather than a different one.

## Re-recording

Start the oracle, then run the matching `#[ignore]`d recorder:

```text
./scripts/live-oracles/legacy.sh 1.7.10
cargo test -p lodestone-v1-7 --test capture_join   -- --ignored --nocapture record_1_7_10
LODESTONE_REGEN=1 cargo test -p lodestone-v1-7 --test entity_types -- --nocapture regenerate
```

A re-recording is not a no-op: entity ids, UUIDs, the world seed and the spawn
point all differ per run. Re-record when the wire is what changed, not the
port, and say so in the commit.

The entity-type recorder writes `src/generated/entity_types.rs` from this
directory's transcript, and it reproduces the committed file byte for byte —
including the provenance notes on each table. If a regeneration drops those
notes, the generator is what is wrong, not the notes.
