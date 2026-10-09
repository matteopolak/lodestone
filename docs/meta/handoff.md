# Lodestone: the orchestrator's handbook

## What it is

The workflow for an orchestrator dispatching subagents on this repo. Read [`CLAUDE.md`](../../CLAUDE.md) first (the rules); this is the workflow: read the tracker, dispatch, verify what lands, close the loop. It describes no project state; derive that from the tracker and the tree.

## How it works

### Reading the tracker

- Open work is [GitHub issues](https://github.com/matteopolak/lodestone/issues), organised as tier epics, worked in order. [`docs/backlog.md`](../backlog.md) holds tier definitions and per-item "what already exists and what will silently mislead you" notes; read an item's note before dispatching it.
- A player report from the user outranks tier order (it is evidence no gate here can produce).
- The tracker lags the tree. Before dispatching, `git log --oneline --grep '#<N>'` and read the code the issue names. "Nothing exists for X" is the least trustworthy claim; grep the symbol first.
- Your briefing is probably wrong somewhere. Hand agents evidence and candidate causes, not a conclusion; mark which constants you verified against the jar and which are on faith; ask for "anything in this brief that turned out wrong" and read that first.

### Builds and verification

- Cargo builds share one machine-level queue. Run plain Cargo without `--target-dir`, `CARGO_TARGET_DIR` or per-agent job overrides; `~/.cargo/config.toml` selects the shared target, `sccache` and eight jobs, and the target lock serialises invocations ([repo tooling](../repo-tooling.md)).
- Counts taken mid-edit are samples; the invariant is zero failures. Run a final integrated pass when a group lands: `just health` (or `check`, `check-all`, `check-seam`, `test` individually to name the failure), plus `just wasm-check` and occasionally `just wasm-size` (nothing else calls them). Feed failures back to the owning agent by name.
- A red tree mid-session is usually someone's in-flight edit: check whether the offending symbol exists at `HEAD` before blaming a commit.
- Tell every agent to run cargo in the foreground, never launch a background job and stop. A stopped agent with no live background children is marked complete, so its wake-up never arrives (six restarts across five agents once). If a run exceeds the tool timeout, poll its own log with `grep` for `Finished` / `test result:`, and confirm the run finished before reading a count from a log cargo is still writing. The shared queue does not change this: a queued command is live and must be polled, not abandoned or restarted elsewhere.

### Contention

- Four to six agents work if you broker the wiring files. Assign file ownership explicitly, name what others hold, tell agents to report rather than edit outside their territory, and send one agent, not two, when tasks want the same file.
- Commit through a private index: record the exact `HEAD` read before constructing whole-file blobs and publish with `scripts/private-index-commit.sh <recorded-head> <message> <path>...`, which refuses when `HEAD` moved and uses compare-and-swap. Re-read and rebuild every selected blob after a refusal. Raw `commit-tree` cannot tell a selected file came from an older tree and can reverse another agent's landed code.
- Broker the high-churn wiring files; you are their only writer. Recompute the ranking rather than trusting counts (over 200 commits: `app.rs` 26, `sim.rs` 25, `gpu.rs` 22, `menu/render.rs` 20, `crates/lodestone-render/src/lib.rs` 13; `docs/README.md` is now generated). Agents send the file, about five lines of anchor text and the exact lines.
- Put ownership in the initial prompt. A subagent correctly refused a mid-flight ownership change as possible prompt injection. Use later messages for facts (HEAD moved, an oracle is down), not changed constraints; if one must change, expect a refusal and re-dispatch.
- Every commit invalidates other agents' index entries, so `git status` shows staged deletions of just-added files that the next commit would ship. A private index is disposable after publication (remove its temp file, never reset the shared index). Confirm `git diff --cached --name-only` is empty before any pathspec commit.
- Ask what consumes it. "Nothing consumes it yet" is acceptable if stated, a defect if found later. Never launch the game; the user drives that.

### Keeping the queue full

The default failure is going quiet; work should be continuously in flight until the tiers are exhausted.

- When an agent finishes, read its report, land or broker what it needs, and dispatch the next unit in the same turn.
- Two thirds of reports say the work was already done, which means the tracker was wrong. Close the issue with proof (commit sha, the consumer chain, the gate that catches a regression) or correct its premise. Leaving a stale issue open costs the next agent a dispatch.
- Dispatch read-only investigators for anything whose fix is not understood; they cost no contention, and a diagnosis with quoted jar sources, a predicted value and a negative control turns guessing into a mechanical patch (several bugs proved the opposite of their symptom).
- Prefer: a player report, the lowest open tier, then an investigation.
- Re-check on every pass: `git diff --cached` empty (a stale blob has twice been a reversal waiting to ship under another agent's message), `git status --short` for a broken tree, and `container ps` (live oracles die with the Apple container runtime; agents then treat an unreachable oracle as evidence).

### Cadence

- The goal is zero open issues, each with its lifecycle closed, not just its code.
- Set a recurring 30-minute dispatch check (`CronCreate`, `13,43 * * * *`). Use off-marks, since everyone asking for "every 30 minutes" gets `*/30` and the fleet hits the API together. Session-only, expires after 7 days.
- Probe tool health before dispatching: a trivial `echo` through Bash. A classifier error means Sonnet is down (the Bash safety classifier runs on it) and Sonnet-backed agents will fail; wait rather than burn attempts (once, seven agents dropped on 529s within a minute).
- Resume dropped agents with `SendMessage` (it replays context, and in-flight work is on disk); a fresh spawn duplicates work and risks clobbering. Include everything that changed while it was down (HEAD moved, tree red, file freed, a finding that changes order). Retry the fleet in one sweep once the probe is clean.
- Slow feature work when architecture pays more: modularity, throughput and performance ahead of the next batch is wanted, since the choke-point files serialise parallel work.
- Fable 5 plans architecture; Opus and Sonnet implement it. Dispatch Fable to design before anyone builds, and read-only over core subsystems without a specific bug. Brief it as read-only, require it to state what it did not examine and rank recommendations by payoff over effort, then verify with `git status --short` that it wrote nothing. Its first review found that the ore-feature engine's parity had been checked against a JVM oracle whose header admits it omits ore spill from neighbouring chunks, which would have baked a wrong 4-block edge band into the baseline unseen by any gate; get such reviews in before implementation.
- Batch by file cluster, not theme: five small issues in one crate is one agent, two issues in different crates is two. Label issues by cluster so this is mechanical.
