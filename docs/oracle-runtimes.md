# Oracle runtimes: Apple `container`

## What it is

All six JVM-oracle scripts under `scripts/live-oracles/` and
`scripts/worldgen-oracle/`, plus the three Rust test files that used to shell
out to `docker` directly, now run their real vanilla server (or JVM oracle)
under Apple's `container` CLI (https://github.com/apple/container) instead of
Docker. Docker is gone from every one of these paths — there is no
`LODESTONE_ORACLE_RUNTIME` switch and no fallback; `container` is simply what
these scripts and tests invoke.

This replaced an earlier, narrower step that added a runtime-selection flag to
`creative.sh` alone and kept Docker as the default. That flag is gone too —
once the flag's own verification passed (see "How this was verified" below),
the decision was made to finish the migration in one pass rather than run a
long soak with two runtimes coexisting.

Why bother: `container` boots faster (~24s to `Done (` vs ~40s) and, more
importantly, gives back its VM reservation on `stop` instead of holding it —
measured ≈1.1–1.3 GB resident while one oracle runs versus ≈3.0 GB for Docker
Desktop, and ~50 MB residual after `stop` versus 2,539 MB retained by Docker's
VM. On a box shared with many other agents' `cargo` builds, that difference is
the whole motivation; see CLAUDE.md's *Repo hazards* Docker entry for the same
argument made about Docker itself.

## What ported, and how each one was verified

| path | what it does | verification |
|---|---|---|
| `scripts/live-oracles/creative.sh` | flat creative 26.2 oracle, the most depended-on one | full RCON-driven v770 live gate run (below) |
| `scripts/live-oracles/terrain.sh` | normal-terrain 26.2 light oracle | booted to `Done (`, op-on-join watcher confirmed live |
| `scripts/live-oracles/survival.sh` | the survival world a human actually plays against | booted to `Done (`, op-on-join watcher confirmed live |
| `scripts/live-oracles/legacy-1.12.sh` | vanilla 1.12.2 (protocol 340) oracle | **previously unverified under `container` at all** — booted to `Done (`, RCON round-trip confirmed (`list` → `There are 0/20 players online:`), and a wrong-password negative control confirmed auth actually gates |
| `scripts/worldgen-oracle/run.sh` | compiles+runs a worldgen JVM oracle against real server classes | **its `:ro` mount-suffix syntax was previously unverified under `container`** — confirmed directly (`touch` inside a `:ro` mount reports `Read-only file system`, same as Docker) and by actually running `MthOracle` end-to-end |
| `scripts/live-oracles/op-on-join.sh` | continuous op-on-join watcher, `container logs -f` | confirmed live against `creative.sh`: real `unique_username` gate accounts get opped as they join |
| `crates/protocol/v770/tests/live_respawn.rs` | RCON via `container exec … perl` against `lodestone-mc262` | straight CLI-name swap (`container exec` takes the same `<container-id> <arguments>` shape); compiles clean |
| `crates/protocol/v340/tests/live_entity.rs` | mob summon via `container exec … sh -c` | straight CLI-name swap; compiles clean |
| `crates/protocol/v340/tests/live_interaction.rs` | mob/block console commands + log read-back | CLI-name swap **plus** a rework (see below); compiles clean |

### How `creative.sh` was verified before anything else was touched

Per the migration's own ordering rule (prove the gate before deleting a
working runtime on the strength of an unproven one):

- `crates/protocol/v770/tests/live_destroy_block_event.rs` (`--features
  live-destroy-block`) — **passed under Docker, then passed under
  `container`**, with byte-identical wire mechanics (`census: minecraft:torch
  first state id = 3370; wire data = 3370` both times) and the op-on-join
  watcher confirmed opping real `unique_username` gate accounts as they
  joined.
- `crates/protocol/v770/tests/live_block_light.rs` (`--features
  live-block-light`) — **failed under both Docker and `container`**, with the
  same failure *shape* (hundreds of interior block-light cells disagreeing
  with the server) under each. This is a real, pre-existing defect in the
  block-light emission/decay engine, unrelated to which runtime hosts the
  oracle — the RCON placement, chunk streaming and diffing all completed
  successfully under both; only the physics the test is checking disagreed.
  Not a container regression; not fixed as part of this migration. Worth a
  separate issue if one doesn't already exist.
- **Negative control**: with the oracle stopped, the same gate was run again
  and failed loudly with `connection refused` rather than skipping — proving
  the earlier green result wasn't a gate that silently no-ops when nothing is
  listening (CLAUDE.md's evidence standard: an absence needs a control that
  proves the detector fires).

### The `--since` rework `live_interaction.rs` needed

Apple's `container logs` has no `--since` — only `-n <lines>`. The old
`logs_since(secs)` polled a wall-clock window; the new `logs_tail(n)` polls a
fixed line count instead. This works here because the call site
(`poll_block_is_air`) already issues one `testforblock` command and sleeps
700ms per iteration, so the response line for the most recent command lands
within a small, bounded number of lines even under concurrent world activity.
`logs_since(6)` (six seconds) became `logs_tail(40)` (forty lines) — generous
enough to outlast one round trip without needing to track a byte/line cursor.

## The three traps every ported script had to account for

All three were measured directly against these images on this machine — see
the comments in each script for the code that encodes them:

1. **Never publish with a host-IP prefix.** `-p 127.0.0.1:25571:25571` accepts
   the TCP connection and then resets on the first byte, *every time* — caught
   by a negative control against vanilla's exacting one-`read()`-per-request
   RCON framing. The bare `-p 25571:25571` form works perfectly and listens on
   all interfaces — the same exposure these scripts always had under Docker's
   bare form, so this is parity, not a new hazard. Upstream
   apple/container#2029 also reports localhost forwarding broken on the macOS
   27 beta — treat this port relay as a fragility hotspot, not a solved
   problem, and re-verify after any `container` upgrade.
2. **An explicit `container image pull` must pass `--platform linux/arm64`.**
   Without it, the default fetches the whole multi-arch manifest — measured
   5.29 GB / 64 blobs for `eclipse-temurin:25-jdk`, versus 150.6 MB / 9 blobs
   pinned. Same shape confirmed for `eclipse-temurin:8-jdk` while verifying
   `legacy-1.12.sh`: 118.6 MB / 9 blobs pinned. None of the scripts pull
   explicitly — `container run`'s on-demand pull defaults to the host's arch
   (arm64 on Apple Silicon) — so this trap doesn't fire from any script here,
   but it will fire the moment someone "helpfully" pre-pulls an image by hand.
3. **`--memory 3g` is required on every script that runs a JVM.** The per-VM
   default is 1 GiB, and every oracle here runs `-Xmx2G`, which blows straight
   through that with no override. `worldgen-oracle/run.sh` also gets it even
   though its JVM has no explicit `-Xmx`, for the same reason.

## How to change it

To add a seventh oracle script, copy the pattern in `creative.sh`: idempotent
`container system start`, `container rm -f "$NAME"`, bare `-p` publishing,
`--memory 3g`, and poll readiness with `container logs "$NAME" | grep -q
'Done ('`. If the script needs continuous opping, it can call
`op-on-join.sh` directly now — that script's own `container logs -f` works
the same way `docker logs -f` used to.

## Configuration

- No runtime-selection variable exists anymore. `container` is the only path.
- `LODESTONE_OP_NAME` — unchanged.
- Ports, world directories and RCON passwords — unchanged.

## What is still out of scope

| item | why |
|---|---|
| the orchestrator's out-of-repo `files/cleanup.sh` | its only cleanup mechanism is `docker ps -aq --filter 'name=lodestone-'`; `container list` has **no filter flag at all** (confirmed against `container list --help`), so this needs a different approach (client-side substring filter over `container list --format json`, most likely) — not something this repo's checkout can fix since the file lives outside it |
| the ad-hoc, orchestrator-managed oracles referenced by name in several `crates/**` live-gate tests (`lodestone-mc262`, `lodestone-mc189`, `lodestone-entity-oracle`, `lodestone-mc-online`, etc. — see `DESIGN.md`'s cleanup-harness registry) | these are not started by any script in this repo and were out of scope for this migration; they remain Docker-managed infrastructure |

Docker Desktop itself remains installed on this machine — this migration
removed Docker *code paths* from the repo's own oracle scripts and the three
listed test files, not the application, and not the ad-hoc oracles above that
still depend on it.

## Dependencies

- `/usr/local/bin/container` (Apple `container` CLI, 1.2.0 tested) — installed
  separately from this repo; not a build dependency.
- `container system start` must have run at least once (every script now does
  this itself, idempotently) before `container run` will succeed.
- `eclipse-temurin:25-jdk` (26.2 oracles) and `eclipse-temurin:8-jdk`
  (`legacy-1.12.sh`) — both arm64-only, cached at
  `~/Library/Application Support/com.apple.container/`.
- `scripts/live-oracles/rcon-op.py` — runtime-agnostic; it only ever speaks
  RCON over a plain socket, never shells out to any container CLI.
