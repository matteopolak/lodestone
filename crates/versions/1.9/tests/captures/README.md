# 1.9-era join captures

Clientbound bytes recorded from real vanilla servers, one file per protocol
this era serves before 1.12.2:

| file | Minecraft | protocol | oracle |
|---|---|---|---|
| `join_1_9_4.txt` | 1.9.4 | 110 | `./scripts/live-oracles/legacy.sh 1.9.4` (`:25580`) |
| `join_1_10_2.txt` | 1.10.2 | 210 | `./scripts/live-oracles/legacy.sh 1.10.2` (`:25582`) |
| `join_1_11_2.txt` | 1.11.2 | 316 | `./scripts/live-oracles/legacy.sh 1.11.2` (`:25584`) |

## Why they exist

These three protocols predate Mojang's data generator, and this host has no
decompiler, so their packet ids and shapes were ported from `minecraft-data` —
a cross-check-grade source, not an authority. These files are the authority:
bytes a real server sent. `decode(encode(x)) == x` is satisfied by two
symmetric misunderstandings; a recorded body is not.

They have already paid for themselves once. The first 1.9.4 recording reported
`id 53 did not translate: 38 trailing bytes`, which was a `world_border` arm
that had picked up `title`'s action renumbering by accident. Nothing else in
the suite noticed.

## Format

One clientbound packet per line, `<state> <packet id> <body as lowercase hex>`;
`#` comments. The id is as that protocol's own table numbers it, so the same
packet has different ids across the three files — `update_health` is 62 here
and 65 at 1.12.2.

## Contents and caps

Every distinct packet id the join produced is present. Bodies are capped at
three per id (one for `map_chunk`, which is two orders of magnitude larger), so
each file is ~16 KB rather than ~6 MB; the hundredth `rel_entity_move` is not
evidence of anything the third is not.

Each capture also carries two `sound_effect` packets the recorder asked for
over RCON at pitch multipliers 1.5 and 0.5. Those are the measurement that
fixes the pre-1.10 byte-pitch scale: 1.9.4 put **94** and **31** on the wire,
which is `pitch * 63` truncated and is consistent with no other scale in the
plausible range (62 would give 93/31, 64 would give 96/32). 1.10.2 and 1.11.2
put `3fc00000` and `3f000000` — 1.5 and 0.5 exactly, as floats.

## Re-recording

Start the oracle, then run the matching `#[ignore]`d recorder in
`../capture_join.rs`:

```text
./scripts/live-oracles/legacy.sh 1.9.4
cargo test -p lodestone-v1-9 --test capture_join -- --ignored --nocapture record_1_9_4
```

A re-recording is not a no-op — entity ids, UUIDs and the world seed differ per
run — so only re-record when the wire, not the port, is what changed, and say
so in the commit.
