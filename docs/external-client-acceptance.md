# External-client acceptance

## What it is

`scripts/live-oracles/external-client-acceptance.py` is an opt-in, bounded acceptance gate for
the hosted modern protocols 766 (1.20.6) and 774 (1.21.11). It starts one dedicated Lodestone
server per selected row and accepts a witness only from an installed, unmodified release client.

## How it works

The runner keeps the registry's complete hostable-row matrix for `--list`, but its external gate
is deliberately limited to 766 and 774. Rows run serially. Each gets a fresh temporary world,
ephemeral localhost port, deadline, server log, and isolated Cargo target directory; a timeout or
nonzero client-driver exit stops the server and records a failed row.

The release client must complete this ordered session before the driver writes its evidence:

1. accept the configuration stream and finish configuration;
2. receive an initial chunk batch and send its batch acknowledgement;
3. enter the world (join);
4. send at least one deliberate movement update;
5. perform exactly one `start_destroy_block` action and observe its result; and
6. close the client session cleanly, with the client initiating the disconnect and observing the
   server-side EOF.

The evidence contract is schema 2. Its `stages` array must contain those six entries in that
order. The chunk stage records a positive `batch_count`, the movement stage records a positive
`movement_count`, the action stage records `action_count: 1` plus `result_observed: true`, and the
disconnect stage records `clean: true` plus `initiated_by: "client"`. Every stage also carries a
short non-empty observation, so the report says what was witnessed rather than only repeating a
boolean. Provenance names the client binary and build, describes the capture method, and points at
non-empty client-log and capture artifacts. The final `report.json` preserves the normalized stage
records and hashes those artifacts per protocol. A server process being terminated by the runner
does not satisfy the clean-disconnect stage.

In `launch` mode the runner invokes `LODESTONE_EXTERNAL_CLIENT_DRIVER` (or `--driver`) with the
release, protocol, server address, action, required stage names, evidence schema, deadline, and
evidence-file path. In `attach` mode it starts the same server and waits for a user-operated client
or UI automation to create that evidence file. The runner contains no packet client and cannot
produce the witness itself.

List the host matrix without launching a server, client, or container:

```bash
just external-client-acceptance --list
```

Run the bounded gate for both modern hosted protocols using a release-client automation driver:

```bash
LODESTONE_EXTERNAL_CLIENT_DRIVER=/absolute/path/to/driver \
  just external-client-acceptance --protocol 766 --protocol 774 \
  --output /private/tmp/lodestone-modern-external
```

The driver receives `--release`, `--protocol`, `--host`, `--port`, `--action`,
`--required-stages` (a comma-separated ordered list), `--evidence-schema`, `--evidence`, and
`--deadline-seconds`. It must write evidence only after all six stages have completed and the client
has disconnected cleanly. A screenshot plus client log is the smallest useful artifact pair; a
packet capture is stronger when the automation can collect one.

An accepted evidence file has this shape (artifact paths may be relative to the evidence file):

```json
{
  "schema": 2,
  "protocol": 766,
  "release": "1.20.6",
  "stages": [
    {"name": "configuration", "observed": true, "observation": "finish accepted"},
    {"name": "chunk_batch_acknowledgement", "observed": true,
     "observation": "initial batch acknowledged", "batch_count": 1},
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

## How to change it

When a hosted row changes, update `ROWS` and keep its release version, registry feature, and
protocol number aligned with `lodestone_registry::hosted_protocols`. Add a protocol to
`GATE_PROTOCOLS` only when its host implements the complete six-stage contract. Do not add
join-only revisions or claim a legacy row is covered by this modern gate. Add or update a Python
contract test whenever the evidence schema changes, and keep the required action externally
observable; a successful process launch, login-only screenshot, or runner-forced disconnect is not
enough.

The dedicated-server binary accepts `--protocol <number>` so the runner can select a row from a
multi-protocol family. Its Cargo features relay registry features rather than adding direct
version-crate dependencies. Keep the recipe opt-in: this live gate is not part of `health`, normal
CI, or ordinary tests.

## Configuration

- `--protocol <number>` is repeatable and must be 766 or 774 for an acceptance run. Omit it to run
  both gate rows.
- `--mode launch` is the default and requires `--driver` or
  `LODESTONE_EXTERNAL_CLIENT_DRIVER`; `--mode attach` waits for a separately created evidence file.
- `--output` must name a new directory. It receives each row's server directory, logs, evidence,
  and aggregate `report.json`.
- `--deadline-seconds` defaults to 90; `--jobs` defaults to 2 for the dedicated-server build.
  `--target-dir` defaults to `/private/tmp/lodestone-external-client-target` so family builds do
  not contend with the shared workspace target.

## Dependencies

The runner requires Python 3, Cargo, and `lodestone-dedicated-server`. A real acceptance run also
needs locally installed, unmodified 1.20.6 and 1.21.11 release clients plus an automation driver
or human/UI-assisted evidence recorder. Release-account credentials, client-container images, and
a reliable launcher/UI automation surface are intentionally outside the repository. The gate has
not been run as part of this change; the remaining hosted protocols (5, 47, 110, 210, 316, 340,
404, 498, 578, 754, 756, 758, 762, and 776) remain external-client gaps.
