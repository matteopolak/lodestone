# External-client acceptance

## What it is

`scripts/live-oracles/external-client-acceptance.py` is an opt-in, bounded acceptance runner for the host direction. It starts one dedicated Lodestone server for each selected hosted protocol and accepts a witness only from an installed release client that joins and performs a block-breaking Play action.

## How it works

The matrix lists the registry's hostable protocol rows, their matching release-client versions, and the dedicated-server feature that compiles exactly that family. Rows run serially. Each gets a fresh temporary world, ephemeral localhost port, 90-second deadline, server log, and an isolated Cargo target directory; a timeout or nonzero client-driver exit stops the server and records a failed row.

In `launch` mode the runner invokes `LODESTONE_EXTERNAL_CLIENT_DRIVER` (or `--driver`) with the release, protocol, server address, action, deadline, and evidence-file path. In `attach` mode it starts the same server and waits for a user-operated client or UI automation to create that evidence file. The runner itself contains no packet client and cannot produce the witness.

The evidence JSON must identify the exact release/protocol and mark both join and `start_destroy_block` as observed. Its provenance must name the release binary and build, describe the capture method, and point at nonempty client-log and capture artifacts. The final `report.json` hashes those artifacts per protocol, preserving what actually supplied the assertion. This establishes an external session, not a round-trip or an in-memory adapter test.

List the exact rows without launching anything:

```bash
just external-client-acceptance --list
```

Run one row using a release-client automation driver:

```bash
LODESTONE_EXTERNAL_CLIENT_DRIVER=/absolute/path/to/driver \
  just external-client-acceptance --protocol 47 --output /private/tmp/lodestone-v47
```

The driver receives `--release`, `--protocol`, `--host`, `--port`, `--action`, `--evidence`, and `--deadline-seconds`. It must write the evidence only after the release client has joined and the action's result has been captured. A screenshot plus client log is the smallest useful artifact pair, but a packet capture is stronger when the automation can collect one.

## How to change it

When a hosted row changes, update the runner's `ROWS` table and keep its release version, registry feature, and protocol number aligned with `lodestone_registry::hosted_protocols`. Do not add join-only revisions. Add a contract test if the evidence schema gains a new required field, and keep the required action externally observable; a successful process launch or a login-only screenshot is not enough.

The dedicated-server binary accepts `--protocol <number>` so the runner can select a row from a multi-protocol family. Its Cargo features relay registry features rather than adding direct version-crate dependencies. Keep that seam intact: only `lodestone-registry` names version crates.

## Configuration

- `--protocol <number>` is repeatable; omit it for every hosted row.
- `--mode launch` is the default and requires `--driver` or `LODESTONE_EXTERNAL_CLIENT_DRIVER`; `--mode attach` waits for a separately created evidence file.
- `--output` must name a new directory. It receives each row's server directory, logs, evidence, and aggregate `report.json`.
- `--deadline-seconds` defaults to 90; `--jobs` defaults to 2. `--target-dir` defaults to `/private/tmp/lodestone-external-client-target` so the expensive family builds do not contend with the shared workspace target.

## Dependencies

The runner requires Python 3, Cargo, and `lodestone-dedicated-server`. A real acceptance run also needs a locally installed, unmodified release client for every selected row and an automation driver or human/UI-assisted evidence recorder. Release-account credentials and a reliable launcher/UI automation surface are intentionally outside the repository; without them, the runner reports no passing external-client acceptance.
