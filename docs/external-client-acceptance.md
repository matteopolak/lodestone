# External-client acceptance

## What it is

`scripts/live-oracles/external-client-acceptance.py` is an opt-in, bounded acceptance gate for
the four highest-value hosted protocols: 762 (1.19.4), 766 (1.20.6), 774 (1.21.11), and 776
(26.2). It starts one dedicated Lodestone server per selected row and accepts a witness only from
an installed, unmodified release client.

## How it works

The runner keeps the registry's complete hostable-row matrix for `--list`, but its external gate
is deliberately limited to four rows. Rows run serially. Each gets a fresh temporary world,
ephemeral localhost port, deadline, server log, and isolated Cargo target directory; a timeout or
nonzero client-driver exit stops the server and records a failed row.

The six stages have one contract across all four rows, while the first two stages record the wire
flow that actually exists in each era:

| protocol | release | configuration stage `mode` | chunk stage `mode` and `batch_count` |
|---:|---|---|---|
| 762 | 1.19.4 | `login_to_play` (no Configuration phase) | `unbatched`, `0` (no batch acknowledgement packet) |
| 766 | 1.20.6 | `configuration` | `acknowledged`, positive |
| 774 | 1.21.11 | `configuration` | `acknowledged`, positive |
| 776 | 26.2 | `configuration` | `acknowledged`, positive |

Protocol 776's Configuration flow includes the synchronized registry and tag stream before the
finish signal; the gate records that the phase completed but does not replace the client's own
wire witness with a packet-level claim. Protocol 762's join carries its dimension registry inline
and goes straight into Play, so its configuration stage is an explicit direct-login checkpoint and
its chunk stage records the unbatched delivery rather than inventing an acknowledgement.

The release client must complete this ordered session before the driver writes its evidence:

1. establish the era's login flow, completing Configuration where that phase exists;
2. receive the initial chunk delivery and acknowledge its batch where the protocol provides batch
   pacing (762 records an unbatched delivery with `batch_count: 0`);
3. enter the world (join);
4. send at least one deliberate movement update;
5. perform exactly one `start_destroy_block` action and observe its result; and
6. close the client session cleanly, with the client initiating the disconnect and observing the
   server-side EOF.

The evidence contract is schema 2. Its `stages` array must contain those six entries in that
order. The configuration stage records its row's `mode` (`login_to_play` or `configuration`), and
the chunk stage records its row's `mode` (`unbatched` or `acknowledged`) plus a zero or positive
`batch_count` as described above. The movement stage records a positive `movement_count`, the
action stage records `action_count: 1` plus `result_observed: true`, and the disconnect stage
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

Run the bounded gate for the four selected hosted protocols using a release-client automation
driver:

```bash
LODESTONE_EXTERNAL_CLIENT_DRIVER=/absolute/path/to/driver \
  just external-client-acceptance --protocol 762 --protocol 766 \
  --protocol 774 --protocol 776 --output /private/tmp/lodestone-external
```

The driver receives `--release`, `--protocol`, `--host`, `--port`, `--action`,
`--configuration-mode`, `--chunk-batch-mode`, `--required-stages` (a comma-separated ordered
list), `--evidence-schema`, `--evidence`, and `--deadline-seconds`. The two mode arguments make the
era-specific flow explicit to the driver. It must write evidence only after all six stages have
completed and the client has disconnected cleanly. A screenshot plus client log is the smallest
useful artifact pair; a packet capture is stronger when the automation can collect one.

An accepted evidence file has this shape (artifact paths may be relative to the evidence file):

```json
{
  "schema": 2,
  "protocol": 766,
  "release": "1.20.6",
  "stages": [
    {"name": "configuration", "observed": true, "observation": "finish accepted",
     "mode": "configuration"},
    {"name": "chunk_batch_acknowledgement", "observed": true,
     "observation": "initial batch acknowledged", "mode": "acknowledged", "batch_count": 1},
    {"name": "join", "observed": true, "observation": "world entered"},
    {"name": "movement", "observed": true, "observation": "position update sent",
     "movement_count": 1},
    {"name": "play_action", "observed": true, "observation": "block result captured",
     "kind": "start_destroy_block", "action_count": 1, "result_observed": true},
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

For protocol 762, the same six entries use `"mode": "login_to_play"` on the configuration
entry and `"mode": "unbatched", "batch_count": 0` on the chunk entry. The observation should
say that login entered Play directly and that the initial columns arrived without batch framing;
the validator checks those row-specific modes rather than accepting a fabricated acknowledgement.

## How to change it

When a hosted row changes, update `ROWS` and keep its release version, registry feature, and
protocol number aligned with `lodestone_registry::hosted_protocols`. Add a protocol to
`GATE_PROTOCOLS` only when its host implements the complete six-stage contract. Keep the row's
configuration and chunk-batch modes aligned with the host's actual join path: 762 is direct
login-to-Play and unbatched, while 766, 774, and 776 use Configuration and batch acknowledgement.
Do not add join-only revisions or imply that a protocol has a packet it does not define. Add or
update a Python contract test whenever the evidence schema changes, and keep the required action
externally observable; a successful process launch, login-only screenshot, or runner-forced
disconnect is not enough.

The dedicated-server binary accepts `--protocol <number>` so the runner can select a row from a
multi-protocol family. Its Cargo features relay registry features rather than adding direct
version-crate dependencies. Keep the recipe opt-in: this live gate is not part of `health`, normal
CI, or ordinary tests.

## Configuration

- `--protocol <number>` is repeatable and must be 762, 766, 774, or 776 for an acceptance run.
  Omit it to run all four gate rows.
- `--mode launch` is the default and requires `--driver` or
  `LODESTONE_EXTERNAL_CLIENT_DRIVER`; `--mode attach` waits for a separately created evidence file.
- `--output` must name a new directory. It receives each row's server directory, logs, evidence,
  and aggregate `report.json`.
- `--deadline-seconds` defaults to 90; `--jobs` defaults to 2 for the dedicated-server build.
  `--target-dir` defaults to `/private/tmp/lodestone-external-client-target` so family builds do
  not contend with the shared workspace target.

## Dependencies

The runner requires Python 3, Cargo, and `lodestone-dedicated-server`. A real acceptance run also
needs locally installed, unmodified 1.19.4, 1.20.6, 1.21.11, and 26.2 release clients plus an
automation driver or human/UI-assisted evidence recorder. Release-account credentials, client
container images, and a reliable launcher/UI automation surface are intentionally outside the
repository. The gate has not been run as part of this change; the remaining hosted protocols (5,
47, 110, 210, 316, 340, 404, 498, 578, 754, 756, and 758) remain external-client gaps.
