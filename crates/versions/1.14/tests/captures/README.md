# 1.14-era join captures

Clientbound bytes recorded from real vanilla servers, one file per protocol
this era gained beyond 1.16.5:

| file | Minecraft | protocol | oracle |
|---|---|---|---|
| `join_1_14_4.txt` | 1.14.4 | 498 | `./scripts/live-oracles/legacy.sh 1.14.4` (`:25586`) |
| `join_1_15_2.txt` | 1.15.2 | 578 | `./scripts/live-oracles/legacy.sh 1.15.2` (`:25588`) |

## Why they exist

Both protocols predate Mojang's machine-readable packet report, so their ids
and shapes were ported from `minecraft-data` — a cross-check-grade source, not
an authority. These files are the authority: bytes a real server sent.
`decode(encode(x)) == x` is satisfied by two symmetric misunderstandings; a
recorded body is not.

They earned that role immediately. `minecraft-data` models 1.14.4's
`map_chunk` with **no biome field anywhere** — its 1.14.4 `protocol.json`
lists `x`, `z`, `groundUp`, `bitMap`, `heightmaps`, `chunkData`,
`blockEntities` and nothing else. A full 1.14 column in fact ends with 256
big-endian `i32` biome ids *inside* the `chunkData` buffer, after the last
section, and a decoder that believes the schema leaves exactly 1,024 bytes
unread. That is not a shape a round-trip test can find, because both halves
would agree about a field neither knows exists; the recorded column found it
on first replay.

## Format

One clientbound packet per line, `<state> <packet id> <body as lowercase hex>`;
`#` comments. The id is as that protocol's own table numbers it, so the same
packet has different ids across the two files — `update_health` is 72 in
`join_1_14_4.txt`, 73 in `join_1_15_2.txt` and 73 again at 1.16.5, where 72 is
`experience`.

## Contents and caps

Every distinct packet id the join produced is present. Bodies are capped at
three per id (one for `map_chunk`, which is two orders of magnitude larger), so
each file is tens of KB rather than megabytes; the hundredth `rel_entity_move`
is not evidence of anything the third is not.

The oracle world is the vanilla `FLAT` preset, which is what makes the replay's
strongest assertion possible: every decoded column must have canonical bedrock
across its whole `y = 0` plane and canonical grass across `y = 3`. That single
check covers all three of this era's chunk differences at once — where the
biome array sits, whether section indices straddle a 64-bit boundary, and which
protocol's block-state table the wire ids are translated through — and the
expected ids come from `lodestone_data::block_states`, not from this crate.

## Re-recording

Start the oracle, then run the matching `#[ignore]`d recorder in
`../capture_join.rs`:

```text
./scripts/live-oracles/legacy.sh 1.14.4
cargo test -p lodestone-v1-14 --test capture_join -- --ignored --nocapture record_1_14_4
```

A re-recording is not a no-op — entity ids, UUIDs and the world seed differ per
run — so only re-record when the wire, not the port, is what changed, and say
so in the commit. Two replay tests pin counts measured off *these* files
(the 7/19 split of ids that agree with 754 versus ids that do not, and the ten
decode errors a 754 adapter produces on `join_1_14_4.txt`); a re-recording that
moves them wants the numbers re-derived, not edited to match.
