# Paper plugin conformance harness

## What it is

The Paper plugin conformance harness defines reproducible, operator-supplied fixtures for comparing one unmodified plugin scenario on Paper and Lodestone. Its current runner validates the contract and emits an explicit blocked result; it does not claim compatibility while Lodestone lacks the required plugin lifecycle, event dispatch, and shared scenario driver.

## How it works

`runner.py` reads a versioned JSON contract. The contract names a pinned JDK, an operator-provided Paper jar and digest, one or more unmodified plugin jars with their digests and capability domains, a deterministic scenario file, and argv arrays for the two backends. Paths are resolved relative to the contract, hashes are checked before evidence is produced, and shell command strings are rejected. The `lodestone-server` `paper-conformance-observation` example now exercises the production proposal-backed event path and emits a Lodestone observation record.

The initial scenario is `block-break-cancel`: one fixed-seed world and one block-break action. The listener-present case expects cancellation. The required no-listener negative control expects the same break to complete. A negative control is evidence only when both backends run the same scenario; it is never reported as passing by this scaffold.

`validate` prints the normalized contract. `run` writes four deterministic NDJSON records: validated contract identity, blocked no-listener control, named Lodestone prerequisites, and a Paper `not_run` record. `run` exits 2 while any prerequisite is missing. It intentionally does not start Paper one-sided, because that would produce non-differential evidence.

`compare` consumes one complete observation record from each backend and emits one comparison record. It requires the two fixed controls, compares each outcome with the other backend and the scenario expectation, and reports every mismatch with its control id and reason. Blocked, incomplete, malformed, or synthetic observations are rejected. Synthetic observations are accepted only by the test-facing comparison function; the CLI never enables that mode. The Lodestone example's `external` record is accepted by the CLI because it came from production server code, but it covers the resident block proposal path; it is not evidence of a loaded Paper plugin or player packet path.

All result values pass through structural and assignment-style secret redaction (`token`, `password`, `secret`, authorization, cookies, and API/private keys). Results contain no timestamps, process IDs, or random run identifiers, so identical inputs produce identical records.

## How to change it

Extend the schema number and validation together when adding a scenario or fixture field. Keep scenario actions explicit and ordered; a new control must include a control proving that the detector can distinguish a pass from a missing run. Add a real backend only after it can execute the same scenario and return structured observations; do not turn the blocked record into a pass-through.

The future runner may interpolate only documented artifact placeholders into argv arrays. It must continue to capture output itself, keep commands shell-free, verify plugin digests, and require the plugin to remain unmodified. The Paper backend should use the operator's jar and JDK rather than storing or redistributing either artifact.

## Configuration

Use the example contract as a template:

```sh
python3 scripts/paper-conformance/runner.py validate \
  --contract scripts/paper-conformance/contract.example.json
python3 scripts/paper-conformance/runner.py run \
  --contract /operator/paper-conformance/contract.json \
  --output /operator/paper-conformance/results.ndjson
python3 scripts/paper-conformance/runner.py compare \
  --contract /operator/paper-conformance/contract.json \
  --paper-results /operator/paper-conformance/paper.ndjson \
  --lodestone-results /operator/paper-conformance/lodestone.ndjson
cargo run -p lodestone-server --example paper-conformance-observation \
  > /operator/paper-conformance/lodestone.ndjson
```

`jdk.version`, `paper.version`, and `paper.build` are provenance fields. `paper.jar`, every plugin `jar`, and their SHA-256 values are mandatory. Plugin domains are `world-editing`, `permissions-economy`, or `server-api`; `unmodified` must be `true`. `commands.paper` is an argv array. `commands.lodestone` may be `null` while the core runtime seam is absent, but `run` still fails with the named Lodestone prerequisites.

Backend observation files are NDJSON with exactly one `kind: "observation"` record. The record must identify its `backend`, the contract scenario, a `source`, `status: "complete"`, and two outcomes: `listener-present` must be `cancelled` and `no-listener` must be `completed`. Production observations must use `evidence_kind: "external"`; `evidence_kind: "synthetic"` is reserved for independent tests.

## Dependencies

The runner uses only the Python standard library and does not invoke Cargo. Paper, a supported JDK, and maintained plugin jars remain operator-supplied and outside the repository. The current Lodestone production bridge provides class loading and narrow resident-world callbacks, not the plugin lifecycle or event surface required by this acceptance harness.
