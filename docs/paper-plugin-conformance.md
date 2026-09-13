# Paper plugin conformance harness

## What it is

The Paper plugin conformance harness defines reproducible, operator-supplied fixtures for comparing one unmodified plugin scenario on Paper and Lodestone. Its runner validates and can execute the operator Paper driver, while refusing to claim compatibility until the Lodestone side has the required plugin lifecycle, event dispatch, and shared scenario driver.

## How it works

`runner.py` reads a versioned JSON contract. The contract names a pinned JDK, an operator-provided Paper jar and digest, one or more unmodified target plugin jars with their digests and capability domains, a separate conformance-driver plugin, a deterministic scenario file, and argv arrays for the two backends. Paths are resolved relative to the contract, hashes are checked before evidence is produced, and shell command strings are rejected. The `lodestone-server` `paper-conformance-observation` example now exercises the production proposal-backed event path and emits a Lodestone observation record.

The initial scenario is `block-break-cancel`: one fixed-seed world and one block-break action. The listener-present case expects cancellation. The required no-listener negative control expects the same break to complete. A negative control is evidence only when both backends run the same scenario; it is never reported as passing by this scaffold.

`validate` prints the normalized contract. `paper` stages verified plugin copies in an ephemeral server directory, verifies that `{java}` reports JDK 25, expands the pinned-JDK/Paper argv placeholders, launches the operator command directly, and captures exactly one structured observation after the command has run both controls. The driver must report the enabled Paper plugin names; every `target_plugins` name must be present, so a process that ignores the staged target jars cannot produce evidence. The runner rechecks both source and staged hashes after exit. A timeout, non-zero exit, missing observation, disabled target, or changed artifact is an error. `run` writes four deterministic NDJSON records: validated contract identity, blocked no-listener control, named Lodestone prerequisites, and a Paper `not_run` record. `run` exits 2 while any prerequisite is missing. It intentionally does not start Paper one-sided, because that would produce non-differential evidence.

`compare` consumes one complete observation record from each backend and emits one comparison record. It requires the two fixed controls, compares each outcome with the other backend and the scenario expectation, and reports every mismatch with its control id and reason. Blocked, incomplete, malformed, or synthetic observations are rejected. Synthetic observations are accepted only by the test-facing comparison function; the CLI never enables that mode. The Lodestone example's `external` record is accepted by the CLI because it came from production server code, but it covers the resident block proposal path; it is not evidence of a loaded Paper plugin or player packet path.

All result values pass through structural and assignment-style secret redaction (`token`, `password`, `secret`, authorization, cookies, and API/private keys). Results contain no timestamps, process IDs, or random run identifiers, so identical inputs produce identical records.

## How to change it

Extend the schema number and validation together when adding a scenario or fixture field. Keep `target_plugins` separate from the driver: the driver is repository-owned test glue, while target jars are operator-supplied maintained plugins and must remain unmodified. Keep scenario actions explicit and ordered; a new control must include a control proving that the detector can distinguish a pass from a missing run. Add a real backend only after it can execute the same scenario and return structured observations; do not turn the blocked record into a pass-through.

The future runner may interpolate only documented artifact placeholders into argv arrays. It must continue to capture output itself, keep commands shell-free, verify plugin digests, and require the plugin to remain unmodified. The Paper backend should use the operator's jar and JDK rather than storing or redistributing either artifact.

## Configuration

Use the example contract as a template:

```sh
python3 scripts/paper-conformance/runner.py validate \
  --contract scripts/paper-conformance/contract.example.json
python3 scripts/paper-conformance/runner.py run \
  --contract /operator/paper-conformance/contract.json \
  --output /operator/paper-conformance/results.ndjson
python3 scripts/paper-conformance/runner.py paper \
  --contract /operator/paper-conformance/contract.json \
  --output /operator/paper-conformance/paper.ndjson
python3 scripts/paper-conformance/runner.py compare \
  --contract /operator/paper-conformance/contract.json \
  --paper-results /operator/paper-conformance/paper.ndjson \
  --lodestone-results /operator/paper-conformance/lodestone.ndjson
cargo run -p lodestone-server --example paper-conformance-observation \
  > /operator/paper-conformance/lodestone.ndjson
```

`jdk.version`, `paper.version`, and `paper.build` are provenance fields. `paper.jar`, every plugin `jar`, and their SHA-256 values are mandatory. `target_plugins` must contain at least one declared maintained plugin and may not contain the driver. Plugin domains are `world-editing`, `permissions-economy`, or `server-api`; `unmodified` must be `true`. `commands.paper` is an argv array. `commands.lodestone` may be `null` while the core runtime seam is absent, but `run` still fails with the named Lodestone prerequisites.

`target_plugins` names the maintained plugin jars whose behavior is under test. The driver is a separate declared plugin: `driver.plugin` and `driver.entrypoint` identify exactly the operator-built driver responsible for the run and must not name a target. Its `protocol` is `paper-observation-v1`, its ordered controls are `listener-present` and `no-listener`, and `requires_real_block_break` must be true. The driver must observe (without cancelling) the target's listener through the real Paper event path, cause a real player block break, remove or disable the target listeners, cause the negative-control break, report the enabled plugin names, and print one observation record only after both results are known. Constructing an event object without a server/player break is not acceptable evidence. The repository's minimal source contract is `driver/src/io/lodestone/conformance/PaperConformancePlugin.java`; it is compiled only by the operator and is not bundled as a jar.

Build that source against the operator's already materialized Paper jar with the pinned JDK; no dependency resolver or network is involved:

```sh
JAVA_HOME=/operator/jdk-25 \
  sh scripts/paper-conformance/driver/build.sh \
  /operator/cache/paper-26.2-121.jar \
  /operator/plugins/lodestone-paper-conformance.jar
```

Record the SHA-256 printed by the recipe as the separate driver's `sha256` in the operator contract, set that plugin's `entrypoint` to `io.lodestone.conformance.PaperConformancePlugin`, and set `driver.plugin` to `LodestonePaperConformance` (the name in `driver/plugin.yml`). Record each maintained target jar separately, set `target_plugins` to those target names, and leave their bytes untouched. The driver waits for an operator-controlled real player to break the fixed target twice; the Paper command must therefore provide a client or operator procedure that performs those two breaks before the process exits.

`commands.paper` must invoke `{java}` directly as its executable and pass `{paper_jar}` to Java's `-jar` option. The runner expands those to the selected JDK's `bin/java` and the verified Paper jar. `{plugin_dir}`, `{scenario}`, and `{workdir}` are also available. Shell and executable wrappers are rejected. The process runs with an ephemeral working directory whose `plugins/` contains only the verified staged jars. `execution.timeout_seconds` defaults to 60 and bounds the complete Paper command; the command must arrange for its plugin/driver to run both controls and exit.

Backend observation files are NDJSON with exactly one `kind: "observation"` record. The Paper record must identify its `backend`, the contract scenario, a `source`, `status: "complete"`, the enabled plugin names, and two outcomes: `listener-present` must be `cancelled` and `no-listener` must be `completed`. Production observations must use `evidence_kind: "external"`; `evidence_kind: "synthetic"` is reserved for independent tests. This proves the selected target jar was enabled by Paper and drove the observed event; it does not yet prove the same unmodified jar runs inside Lodestone. Exact unsupported-member diagnostics and the JVM-disabled performance budget remain separate acceptance gates.

## Dependencies

The runner uses only the Python standard library. Paper, a supported JDK, and maintained plugin jars remain operator-supplied and outside the repository. The Paper backend uses direct `subprocess.Popen` with `shell=False` and an ephemeral working directory; it never modifies the operator artifacts. The current Lodestone production bridge provides class loading and narrow resident-world callbacks, not the plugin lifecycle or event surface required by this acceptance harness.
