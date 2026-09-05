# External-client acceptance

## What it is

`scripts/live-oracles/external-client-acceptance.py` is an opt-in, bounded acceptance gate for all
16 hosted protocols: 5 (1.7.10), 47 (1.8.9), 110 (1.9.4), 210 (1.10.2), 316 (1.11.2), 340
(1.12.2), 404 (1.13.2), 498 (1.14.4), 578 (1.15.2), 754 (1.16.5), 756 (1.17.1), 758 (1.18.2),
762 (1.19.4), 766 (1.20.6), 774 (1.21.11), and 776 (26.2). It starts one dedicated Lodestone
server per selected row and accepts a witness only from an installed, unmodified release client.

## How it works

The runner keeps the registry's complete hostable-row matrix for `--list` and gates every hosted
row. Rows run serially. Each gets a fresh temporary world,
ephemeral localhost port, deadline, server log, and isolated Cargo target directory; a timeout or
nonzero client-driver exit stops the server and records a failed row.

The eight-stage minimum Play contract is identical for every row. Join and chunk evidence also
records the wire flow that actually exists in each era:

| protocol | release | join `configuration_mode` | chunks `batch_mode` and `batch_count` |
|---:|---|---|---|
| 5 | 1.7.10 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 47 | 1.8.9 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 110 | 1.9.4 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 210 | 1.10.2 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 316 | 1.11.2 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 340 | 1.12.2 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 404 | 1.13.2 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 498 | 1.14.4 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 578 | 1.15.2 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 754 | 1.16.5 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 756 | 1.17.1 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 758 | 1.18.2 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 762 | 1.19.4 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 766 | 1.20.6 | `configuration` | `acknowledged`, positive |
| 774 | 1.21.11 | `configuration` | `acknowledged`, positive |
| 776 | 26.2 | `configuration` | `acknowledged`, positive |

Protocols 5, 47, 110, 210, 316, 340, 404, 498, 578, 754, 756, 758, and 762 have no Configuration
phase or chunk-batch acknowledgement: their join stage records the direct-login path,
and their chunks stage records the unbatched delivery rather than inventing an
acknowledgement. Their join packets carry the dimension information needed by each era's host;
the 1.13 and 1.14-era rows retain their own chunk framing while sharing the same gate modes.
Protocols 766, 774, and 776 use Configuration flows that include the synchronized
registry and tag stream before the finish signal; the gate records that the phase completed but
does not replace the client's own wire witness with a packet-level claim.

The release client must complete this finite ordered session before the driver writes its evidence:

1. enter the world after completing the era's login flow;
2. receive initial chunks and acknowledge a batch where the protocol provides batch pacing;
3. send at least one deliberate movement update;
4. break one block and place one block, observing both world results;
5. send one chat message and observe its result;
6. select a hotbar slot and observe the selection;
7. complete one keepalive exchange with the same identifier in both directions; and
8. close the client session cleanly, with the client initiating the disconnect and observing the
   server-side EOF.

These eight rows are the closure boundary for minimum hosted Play support; packet coverage beyond
them is tracked separately and cannot silently expand this gate. No hosted protocol has a passing
external witness yet, so the currently passing matrix is empty. All 16 rows are configured and can
be attempted; a row becomes accepted only when an exact release client supplies all eight observations.

The evidence contract is schema 3. Its `stages` array must contain those eight entries in that
order. The join stage records its row's `configuration_mode` (`login_to_play` or `configuration`), and
the chunks stage records its row's `batch_mode` (`unbatched` or `acknowledged`) plus a zero or positive
`batch_count` as described above. The movement stage records a positive `movement_count`, the
break/place stage records exactly one observed result of each kind, chat records exactly one observed
message, inventory selection records a slot in `0..=8`, keepalive records exactly one identifier-matched
exchange, and the disconnect stage
records `clean: true` plus `initiated_by: "client"`. Every stage also carries a short non-empty
observation, so the report says what was witnessed rather than only repeating a boolean. Provenance
names the client binary and exact build, describes the capture method, and points at non-empty
client-log and capture artifacts. The final `report.json` preserves the normalized stage records
and hashes those artifacts per protocol. A server process being terminated by the runner does not
satisfy the clean-disconnect stage.

In `launch` mode the runner invokes `LODESTONE_EXTERNAL_CLIENT_DRIVER` (or `--driver`) with the
release, protocol, server address, action, required stage names, evidence schema, deadline, and
evidence-file path. In `attach` mode it starts the same server and waits for a user-operated client
or UI automation to create that evidence file. The runner contains no packet client and cannot
produce the witness itself.

List the host matrix without launching a server, client, or container:

```bash
just external-client-acceptance --list
```

Run the bounded gate for selected hosted protocols using a release-client automation
driver:

```bash
LODESTONE_EXTERNAL_CLIENT_DRIVER=/absolute/path/to/driver \
  just external-client-acceptance --protocol 5 --protocol 47 \
  --protocol 110 --protocol 210 --protocol 316 --protocol 340 \
  --protocol 404 --protocol 498 --protocol 578 --protocol 754 \
  --protocol 756 --protocol 758 --protocol 762 --protocol 766 \
  --protocol 774 --protocol 776 --output /private/tmp/lodestone-external
```

The driver receives `--release`, `--protocol`, `--host`, `--port`, `--action`,
`--configuration-mode`, `--chunk-batch-mode`, `--required-stages` (a comma-separated ordered
list), `--evidence-schema`, `--evidence`, and `--deadline-seconds`. The two mode arguments make the
era-specific flow explicit to the driver. It must write evidence only after all eight stages have
completed and the client has disconnected cleanly. A screenshot plus client log is the smallest
useful artifact pair; a packet capture is stronger when the automation can collect one.

An accepted evidence file has this shape (artifact paths may be relative to the evidence file):

```json
{
  "schema": 3,
  "protocol": 766,
  "release": "1.20.6",
  "stages": [
    {"name": "join", "observed": true, "observation": "world entered",
     "configuration_mode": "configuration"},
    {"name": "chunks", "observed": true, "observation": "initial batch acknowledged",
     "batch_mode": "acknowledged", "batch_count": 1},
    {"name": "movement", "observed": true, "observation": "position update sent",
     "movement_count": 1},
    {"name": "break_place", "observed": true, "observation": "both results captured",
     "break_count": 1, "break_result_observed": true,
     "place_count": 1, "place_result_observed": true},
    {"name": "chat", "observed": true, "observation": "message appeared",
     "message_count": 1, "result_observed": true},
    {"name": "inventory_select", "observed": true, "observation": "slot changed",
     "selected_slot": 4, "result_observed": true},
    {"name": "keepalive", "observed": true, "observation": "identifier matched",
     "exchange_count": 1, "id_matched": true},
    {"name": "disconnect", "observed": true, "observation": "client observed EOF",
     "clean": true, "initiated_by": "client"}
  ],
  "provenance": {
    "client_binary": "/Applications/Release.app",
    "client_build": "1.20.6",
    "capture_method": "UI automation plus client log",
    "capture": "screen.png",
    "client_log": "release-client.log"
  }
}
```

For protocols 5, 47, 110, 210, 316, 340, 404, 498, 578, 754, 756, 758, and 762, the same eight
entries use `"configuration_mode": "login_to_play"` on the join entry and `"batch_mode": "unbatched",
"batch_count": 0` on the chunk entry. The observations should say that login entered Play
directly and that the initial columns arrived without batch framing; the validator checks those
row-specific modes rather than accepting a fabricated acknowledgement.

## How to change it

When a hosted row changes, update `ROWS` and keep its release version, registry feature, and
protocol number aligned with `lodestone_registry::hosted_protocols`. Add a protocol to
`GATE_PROTOCOLS` only when its host is intended to satisfy the complete eight-stage contract. Keep the row's
configuration and chunk-batch modes aligned with the host's actual join path: protocols 5 through
762 listed above are direct login-to-Play and unbatched, while 766, 774, and 776 use Configuration
and batch acknowledgement. Do not add join-only revisions or imply that a protocol has a packet it
does not define. Add or update a Python contract test whenever the evidence schema changes, and
keep the required action externally observable; a successful process launch, login-only screenshot,
or runner-forced disconnect is not enough.

The dedicated-server binary accepts `--protocol <number>` so the runner can select a row from a
multi-protocol family. Its Cargo features relay registry features rather than adding direct
version-crate dependencies. Keep the recipe opt-in: this live gate is not part of `health`, normal
CI, or ordinary tests.

## Configuration

- `--protocol <number>` is repeatable and must be 5, 47, 110, 210, 316, 340, 404, 498, 578, 754,
  756, 758, 762, 766, 774, or 776 for an acceptance run. Omit it to run all 16 gate rows.
- `--mode launch` is the default and requires `--driver` or
  `LODESTONE_EXTERNAL_CLIENT_DRIVER`; `--mode attach` waits for a separately created evidence file.
- `--output` must name a new directory. It receives each row's server directory, logs, evidence,
  and aggregate `report.json`.
- `--deadline-seconds` defaults to 90; `--jobs` defaults to 2 for the dedicated-server build.
  `--target-dir` defaults to `/private/tmp/lodestone-external-client-target` so family builds do
  not contend with the shared workspace target.

## Dependencies

The runner requires Python 3, Cargo, and `lodestone-dedicated-server`. A real acceptance run also
needs locally installed, unmodified 1.7.10, 1.8.9, 1.9.4, 1.10.2, 1.11.2, 1.12.2, 1.13.2,
1.14.4, 1.15.2, 1.16.5, 1.17.1, 1.18.2, 1.19.4, 1.20.6, 1.21.11, and 26.2 release clients
plus an automation driver or human/UI-assisted evidence recorder. Release-account credentials,
client container images, and a reliable launcher/UI automation surface are intentionally outside the
repository. All 16 hosted protocols are now represented by gate rows, but no client was launched
while this documentation was updated: every row remains unverified until its manual execution
produces a passing `report.json` with the eight-stage witness and exact client-build provenance.
