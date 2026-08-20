# Lodestone — the orchestrator's handbook

Read [`CLAUDE.md`](./CLAUDE.md) first — it is the rules, this is the workflow. Your job is to read the
tracker, dispatch subagents, verify what lands, and close the loop. Nothing here describes the current
state of the project; work that out yourself from the tracker and the tree.

- **Open work is [GitHub issues](https://github.com/matteopolak/lodestone/issues)**, organised as tier
  epics. Work the tiers in order. [`docs/backlog.md`](./docs/backlog.md) holds the tier definitions and
  the per-item "what already exists and what will silently mislead you" notes — read the note for an
  item before dispatching it.
- **A player report from the user outranks the tier order.** They are the only source of evidence that
  no gate in this repo can produce.
- **The tracker lags the tree.** Before dispatching, `git log --oneline --grep '#<N>'` and read the code
  it names. Issues have been dispatched after the fix already landed. "Nothing exists for X" is the
  least trustworthy claim you will find — grep for the symbol first.
- **Your briefing is probably wrong somewhere.** Hand agents the evidence and the candidate causes, not
  a conclusion to implement. Mark which constants you verified against the jar or decompiled source and
  which you are passing on faith. Ask for "anything in this brief that turned out wrong" and read that
  part of the report first. When an agent contradicts you and is right, say so and move on.
- **Parallel Cargo runs are supported through sccache.** Give each agent its own literal `--target-dir`
  under `/tmp` and a bounded `-j` value, following [`docs/build-caching.md`](./docs/build-caching.md).
  A count taken while agents are mid-edit is still a sample rather than a measurement — the invariant is
  zero failures, never a number. Run a final integrated verification when a group lands, using the health
  checks in `CLAUDE.md` — `just health` (see
  [`docs/task-runner.md`](./docs/task-runner.md)), or the four `just check`/`check-all`/`check-seam`/`test`
  recipes individually when you need to name which one failed. Feed failures back to the owning agent by
  name. Also run `just wasm-check` (and, less often, `just wasm-size`) as part of the same batched pass —
  nothing else calls either script.
- **A red tree mid-session is usually someone's in-flight edit.** Check whether the offending symbol
  exists at `HEAD` before blaming a commit.
- **Contention is your main job.** Four to six agents at once works *if* you broker the wiring files;
  without that, three collide. Assign file ownership explicitly, name what other agents are holding,
  and tell agents to report rather than edit outside their territory. If two tasks want the same file,
  send one agent, not two. Have them commit through a private `GIT_INDEX_FILE` + `commit-tree`, reading
  the tree and committing in one step.
- **Broker the high-churn wiring files: you are their only writer.** Measured over 200 commits:
  `docs/README.md` (50), `app.rs` (26), `sim.rs` (25), `gpu.rs` (22), `menu/render.rs` (20),
  `crates/lodestone-render/src/lib.rs` (13). Every feature needs one line in some of them — an index
  entry, a source install, a system registration, a draw call, a `pub mod` — which is why they collide
  constantly. Agents send you the file, ~5 lines of anchor text, and the exact lines; you apply it.
  Recompute the ranking rather than trusting these counts.
- **Put ownership in the *initial* prompt.** A subagent correctly refused a mid-flight ownership change
  as possible prompt injection — it cannot verify who you are, and its brief is the only authority it
  has. Use later messages for facts (HEAD moved, an oracle is down, a file was released), not for
  changing a constraint the brief already stated. If one must change, expect a refusal and re-dispatch.
- **Every commit invalidates every other agent's index entries**, so `git status` starts reporting staged
  *deletions* of files the commit just added, which the next `git commit` would ship. After any commit,
  `git reset -- <your paths>` and confirm `git diff --cached --name-only` is empty.
- **Ask what consumes it.** A subsystem that is built, tested and called by nothing is this repo's
  dominant defect. "Nothing consumes it yet" is an acceptable answer if stated; found later, it is not.
- **Never launch the game.** The user drives that.

---

## Keep the queue full

The default failure of an orchestrator is going quiet. Work is expected to be **continuously in
flight** until the tiers are genuinely exhausted.

- **When an agent finishes, dispatch.** Do not wait to be asked, and do not batch idle time. Read its
  report, land or broker whatever it needs, then start the next unit in the same turn.
- **Two thirds of what agents report is that the work was already done.** That is not wasted — it is
  the tracker being wrong. Close the issue with proof (commit sha, `file:line`, the consumer chain, the
  gate that catches a regression) or correct its stale premise in a comment. Closing a stale issue is
  worth as much as writing code, and leaving one open costs the next agent a whole dispatch.
- **Dispatch read-only investigators for anything whose fix is not yet understood.** They cost nothing
  in contention, they can run against files other agents hold, and a diagnosis with quoted jar sources,
  a predicted value and a specified negative control turns a day of guessing into a mechanical patch.
  Several bugs this way turned out to be the opposite of their symptom.
- **Prefer, in order:** a player report, the lowest open tier, then an investigation.
- **Three things to re-check on every dispatch pass**, because each has bitten: `git diff --cached` must be
  empty (a stale blob there has twice been a reversal of someone's just-landed work, waiting for the
  next agent's `git commit` to ship it under their message); `git status --short` for a tree someone
  broke; and `container ps`, since the live oracles die with the Apple container runtime and agents then
  waste time treating an unreachable oracle as evidence.

---

## The cadence, and the goal

**The goal is zero open issues.** Dispatch continuously until the tracker is genuinely empty, and treat
every issue as needing its lifecycle closed out — not just its code written.

**Set a recurring 30-minute dispatch check** (`CronCreate`, `13,43 * * * *`). Pick off-marks like `:13`
and `:43` rather than `:00`/`:30`: every user who asks for "every 30 minutes" gets `*/30`, so those two
minutes are when the whole fleet hits the API at once. Session-only, expires after 7 days.

**Probe tool health before resuming or dispatching anything.** Run a trivial `echo` through Bash. If it
comes back blocked with a classifier error, **Sonnet is down** — the Bash safety classifier runs on it,
so a blocked `echo` is the cheapest reliable signal that Sonnet-backed agents will fail too. Wait rather
than dispatching into an outage; a retry during one just burns the attempt. Measured here: seven agents
dropped on transient 529s within a minute, Bash went unusable at the same moment, and everything came
back together.

**Resume dropped agents from their transcripts — do not re-spawn them.** `SendMessage` to a failed agent
replays its context, and its in-flight work is already on disk; a fresh spawn duplicates the work and
risks clobbering what the first one wrote. Include in the resume message everything that changed while it
was down: HEAD moved, the tree went red, a file freed, another agent's finding that changes its order of
work. Retry the whole fleet in one sweep once the probe is clean.

**Tell every agent never to launch a background `cargo` job and then stop.** This was the single most
repeated operational failure of the session — **six restarts across five agents**. When an agent stops
with no live background children the harness marks it **complete**, so the completion notification it is
waiting for never arrives; it sits idle until the orchestrator notices and resumes it. Agents describe
this as "waiting for the notification" or "monitor armed", which reads like progress and is not.

The instruction that works: **run cargo in the foreground**, and if a run exceeds the tool timeout, poll
your own log with a cheap `grep` for `Finished` / `test result:` rather than stopping. And confirm the run
finished before reading any count out of it — a count read from a log cargo is still writing looks exactly
like a pass, which the orchestrator also got wrong once.

Per-agent private target dirs and sccache make parallel foreground builds practical; they do not change
the foreground-only lifecycle rule above.

**Slow new feature work when architecture would pay more.** Landing modularity, throughput and
performance improvements ahead of the next feature batch is wanted, not a detour. The four choke-point
files serialise nearly all parallel work, so decomposing them raises the ceiling on everything else.

**Fable 5 plans architecture; Opus/Sonnet implement it.** Dispatch Fable to design before anyone builds,
and dispatch it read-only over core subsystems on its own merits — it does not need a specific bug to
justify a review. Brief it explicitly as read-only, require it to **state what it did not examine** and to
rank recommendations by payoff ÷ effort so each is directly dispatchable, then verify with
`git status --short` that it wrote nothing: a review agent that edits is indistinguishable from an
implementing one after the fact.

Its first review paid for itself in a way no implementing agent had managed across a whole session on the
same crate, and the finding was **perishable**: the ore-feature engine's parity had been verified against
a JVM oracle whose own header admits it does not model ore spill from neighbouring chunks, so the oracle
shared the simplification it was validating. Composing on top of that would have calibrated a wrong
4-block edge band on every chunk into the accepted baseline, and no gate in the tree could see it. It also
declined to fabricate a benchmark when the tool classifier was down, using the committed sha-tagged bench
record instead and stating that provenance. **Order matters for findings like that — get the review in
before the implementation, not after.**

**Batch by file cluster, not by theme.** Co-location is what decides whether two issues can be two agents
or must be one. Five small issues in one crate is a good batch; two issues in different crates is two
agents. Label issues by affected cluster so this is mechanical rather than re-derived each time.
