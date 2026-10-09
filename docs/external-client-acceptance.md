# External-client acceptance

## What it is

`scripts/live-oracles/external-client-acceptance.py` is an opt-in, bounded acceptance gate for all 16 hosted protocols: 5 (1.7.10), 47 (1.8.9), 110 (1.9.4), 210 (1.10.2), 316 (1.11.2), 340 (1.12.2), 404 (1.13.2), 498 (1.14.4), 578 (1.15.2), 754 (1.16.5), 756 (1.17.1), 758 (1.18.2), 762 (1.19.4), 766 (1.20.6), 774 (1.21.11) and 776 (26.2). It starts one dedicated Lodestone server per selected row and accepts a witness only from an installed, unmodified release client. No hosted protocol has a passing external witness yet.

## How it works

The runner keeps the registry's full hostable-row matrix for `--list` and gates every hosted row, serially. Each row gets a fresh temporary world, ephemeral localhost port, deadline and server log; Cargo uses the machine-wide shared target queue. A timeout or nonzero driver exit stops the server and records a failed row. The runner contains no packet client and cannot produce the witness itself.

Modes: `launch` (default) invokes `LODESTONE_EXTERNAL_CLIENT_DRIVER` (or `--driver`) with the release, protocol, server address, action, required stages, evidence schema, deadline and evidence path; `attach` starts the same server and waits for a user or UI automation to create the evidence file.

**Wire flow per era.** Protocols 5 through 762 (every row except the last three) have no Configuration phase or chunk-batch acknowledgement: the join stage records `configuration_mode: "login_to_play"` and the chunks stage `batch_mode: "unbatched"`, `batch_count: 0`. Protocols 766, 774 and 776 record `configuration_mode: "configuration"` and `batch_mode: "acknowledged"` with a positive `batch_count`; their Configuration flows include the registry and tag stream, and the gate records completion without replacing the client's own wire witness. The validator checks these row-specific modes rather than accepting a fabricated acknowledgement.

**The eight-stage minimum Play contract** (identical for every row; packet coverage beyond it is tracked elsewhere and cannot silently expand the gate):

1. enter the world after the era's login flow;
2. receive initial chunks, acknowledging a batch where the protocol paces them;
3. send at least one deliberate movement update;
4. break one block and place one block, observing both results;
5. send one chat message and observe its result;
6. select a hotbar slot and observe it;
7. complete one keepalive exchange with the same identifier both ways;
8. close cleanly with the client initiating the disconnect and observing the server-side EOF.

**Evidence contract (schema 3).** `stages` holds those eight entries in order, each with `observed` and a short non-empty `observation`. Join records `configuration_mode`; chunks records `batch_mode` and `batch_count`; movement a positive `movement_count`; break/place exactly one observed result of each kind; chat exactly one message; inventory selection a slot in `0..=8`; keepalive exactly one identifier-matched exchange; disconnect `clean: true` and `initiated_by: "client"`. Provenance names the client binary and exact build, the capture method, and non-empty client-log and capture artifacts (paths may be relative to the evidence file); `report.json` keeps the normalised stage records and hashes the artifacts. A runner-terminated server does not satisfy the clean-disconnect stage.

```json
{
  "schema": 3, "protocol": 766, "release": "1.20.6",
  "stages": [
    {"name": "join", "observed": true, "observation": "world entered", "configuration_mode": "configuration"},
    {"name": "chunks", "observed": true, "observation": "initial batch acknowledged", "batch_mode": "acknowledged", "batch_count": 1},
    {"name": "movement", "observed": true, "observation": "position update sent", "movement_count": 1},
    {"name": "break_place", "observed": true, "observation": "both results captured", "break_count": 1, "break_result_observed": true, "place_count": 1, "place_result_observed": true},
    {"name": "chat", "observed": true, "observation": "message appeared", "message_count": 1, "result_observed": true},
    {"name": "inventory_select", "observed": true, "observation": "slot changed", "selected_slot": 4, "result_observed": true},
    {"name": "keepalive", "observed": true, "observation": "identifier matched", "exchange_count": 1, "id_matched": true},
    {"name": "disconnect", "observed": true, "observation": "client observed EOF", "clean": true, "initiated_by": "client"}
  ],
  "provenance": {"client_binary": "/Applications/Release.app", "client_build": "1.20.6", "capture_method": "UI automation plus client log", "capture": "screen.png", "client_log": "release-client.log"}
}
```

The driver receives `--release`, `--protocol`, `--host`, `--port`, `--action`, `--configuration-mode`, `--chunk-batch-mode`, `--required-stages` (comma-separated, ordered), `--evidence-schema`, `--evidence` and `--deadline-seconds`, and must write evidence only after all eight stages complete and the client has disconnected. A screenshot plus client log is the minimum artifact pair; a packet capture is stronger.

```bash
just external-client-acceptance --list     # no server, client or container
LODESTONE_EXTERNAL_CLIENT_DRIVER=/absolute/path/to/driver \
  just external-client-acceptance --protocol 766 --protocol 774 --protocol 776 \
  --output /private/tmp/lodestone-external
```

## How to change it

When a hosted row changes, update `ROWS` and keep release, registry feature and protocol number aligned with `lodestone_registry::hosted_protocols`. Add to `GATE_PROTOCOLS` only a host meant to satisfy the whole eight-stage contract, with configuration and chunk-batch modes matching the host's real join path. Do not add join-only revisions. Update the Python contract test whenever the schema changes, and keep each required action externally observable (a launch, login-only screenshot or runner-forced disconnect is not enough).

The dedicated-server binary accepts `--protocol <number>` so the runner can pick a row of a multi-protocol family; its Cargo features relay registry features instead of depending on version crates directly. The recipe is opt-in and not part of `health`, CI or ordinary tests.

## Configuration

- `--protocol <number>` is repeatable (any of the 16 above); omit to run all.
- `--mode launch|attach`; `--output` must name a new directory (server directory, logs, evidence, aggregate `report.json` per row); `--deadline-seconds` defaults to 90.
- Build parallelism and target directory come from `~/.cargo/config.toml`; the runner never overrides the shared build queue.

## Dependencies

Python 3, Cargo and `lodestone-dedicated-server`. A real run needs locally installed, unmodified release clients for every selected row plus an automation driver or human-assisted evidence recorder; release-account credentials, client images and launcher/UI automation live outside the repository. Every row stays unverified until a manual run produces a passing `report.json` with the eight-stage witness and exact client-build provenance.
