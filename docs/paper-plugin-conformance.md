# Paper plugin conformance harness

## What it is

Reproducible, operator-supplied fixtures for comparing one unmodified plugin scenario on Paper and Lodestone. The runner validates and can execute the operator's Paper driver, but refuses to claim compatibility until the Lodestone side has the required plugin lifecycle, event dispatch and shared scenario driver.

## How it works

`scripts/paper-conformance/runner.py` reads a versioned JSON contract naming a pinned JDK, an operator-provided Paper jar and digest, one or more unmodified target plugin jars with digests and capability domains, a separate conformance-driver plugin, a deterministic scenario file, and argv arrays for the two backends. Paths resolve relative to the contract, hashes are checked before any evidence is produced, and shell command strings are rejected. The `lodestone-server` example `paper-conformance-observation` exercises the production proposal-backed event path and emits a Lodestone observation.

The initial scenario `block-break-cancel` uses one fixed-seed world and one block break: the listener-present case expects cancellation and the required no-listener negative control expects completion. A control counts as evidence only when both backends run the same scenario; the scaffold never reports it as passing alone.

- `validate` prints the normalised contract.
- `paper` stages verified plugin copies in an ephemeral server directory, verifies `{java}` reports JDK 25, expands placeholders, launches the operator command directly and captures exactly one structured observation after both controls. The driver must report enabled plugin names and every `target_plugins` name must be present (so ignoring the staged jars cannot produce evidence). Source and staged hashes are rechecked after exit; a timeout, non-zero exit, missing observation, disabled target or changed artifact is an error.
- `run` writes four deterministic NDJSON records (validated contract identity, blocked no-listener control, named Lodestone prerequisites, Paper `not_run`) and exits 2 while any prerequisite is missing; it never starts Paper one-sided (non-differential evidence).
- `compare` takes one complete observation per backend and reports every mismatch with control id and reason against the other backend and the scenario expectation. Blocked, incomplete, malformed or synthetic observations are rejected (synthetic only through the test-facing function; the CLI never enables it). The Lodestone example's `external` record is accepted because it came from production server code, but it covers the resident block proposal path, not a loaded Paper plugin or the player packet path.

Results pass through structural and assignment-style secret redaction (`token`, `password`, `secret`, authorization, cookies, API/private keys) and contain no timestamps, process IDs or random identifiers, so identical inputs give identical records.

## How to change it

Extend the schema number and validation together when adding a scenario or fixture field. Keep `target_plugins` (operator-supplied maintained plugins, unmodified) separate from the repository-owned driver. Keep scenario actions explicit and ordered; every new control needs a control proving the detector distinguishes a pass from a missing run. Add a real backend only once it executes the same scenario and returns structured observations; never turn the blocked record into a pass-through. The runner interpolates only documented placeholders into argv, captures output itself, stays shell-free and uses the operator's jar and JDK (neither stored nor redistributed).

## Configuration

```sh
python3 scripts/paper-conformance/runner.py validate --contract scripts/paper-conformance/contract.example.json
python3 scripts/paper-conformance/runner.py run --contract /operator/paper-conformance/contract.json --output /operator/paper-conformance/results.ndjson
python3 scripts/paper-conformance/runner.py paper --contract ... --output /operator/paper-conformance/paper.ndjson
python3 scripts/paper-conformance/runner.py compare --contract ... --paper-results .../paper.ndjson --lodestone-results .../lodestone.ndjson
cargo run -p lodestone-server --example paper-conformance-observation > /operator/paper-conformance/lodestone.ndjson
```

- `jdk.version`, `paper.version`, `paper.build` are provenance; `paper.jar`, every plugin `jar` and their SHA-256 are mandatory. `target_plugins` needs at least one maintained plugin, never the driver; domains are `world-editing`, `permissions-economy` or `server-api`; `unmodified` must be `true`. `commands.lodestone` may be `null` while the runtime seam is absent (`run` still fails naming the prerequisites).
- The driver is a separate declared plugin (`driver.plugin`, `driver.entrypoint`, `protocol` `paper-observation-v1`, ordered controls `listener-present` and `no-listener`, `requires_real_block_break: true`). It must observe (without cancelling) the target's listener through the real Paper event path, cause a real player block break, remove or disable the target listeners, cause the negative-control break, report enabled plugin names and print one observation only after both results are known; constructing an event object without a real break is not evidence. The source contract is `driver/src/io/lodestone/conformance/PaperConformancePlugin.java`, compiled only by the operator against their materialised Paper jar with the pinned JDK (no resolver or network): `JAVA_HOME=/operator/jdk-25 sh scripts/paper-conformance/driver/build.sh /operator/cache/paper-26.2-121.jar /operator/plugins/lodestone-paper-conformance.jar`. Record the printed SHA-256 as the driver's `sha256`, set `entrypoint` to `io.lodestone.conformance.PaperConformancePlugin` and `driver.plugin` to `LodestonePaperConformance` (from `driver/plugin.yml`). The driver waits for an operator-controlled real player to break the fixed target twice, so the Paper command must provide a client or procedure that does so before the process exits.
- `commands.paper` invokes `{java}` as its executable with `{paper_jar}` after `-jar` (expanded to the JDK's `bin/java` and the verified jar); `{plugin_dir}`, `{scenario}` and `{workdir}` are also available; wrappers are rejected. The process runs in an ephemeral directory whose `plugins/` holds only verified staged jars; `execution.timeout_seconds` (default 60) bounds the whole command.
- Observation files are NDJSON with exactly one `kind: "observation"` record: `backend`, scenario, `source`, `status: "complete"`, enabled plugin names, and outcomes (`listener-present` = `cancelled`, `no-listener` = `completed`). Production uses `evidence_kind: "external"`; `"synthetic"` is for independent tests. This proves Paper enabled the selected target jar and drove the event, not that the same jar runs inside Lodestone; exact unsupported-member diagnostics and the JVM-disabled performance budget remain separate gates.

## Dependencies

Python standard library only; Paper, a supported JDK and plugin jars are operator-supplied. The Paper backend uses `subprocess.Popen` with `shell=False` in an ephemeral directory and never modifies operator artifacts. Lodestone's production bridge today provides class loading and narrow resident-world callbacks, not the plugin lifecycle or event surface this harness needs.
