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
- **Do not run tests often.** One shared checkout, no worktrees, one shared `Cargo.lock` and `target/`.
  Concurrent cargo runs contend, and a count taken while agents are mid-edit is a sample rather than a
  measurement — the invariant is zero failures, never a number. Tell agents not to run `cargo` at all,
  have them report their unverified surface honestly, and run **one** batched verification when a group
  lands, using the health checks in `CLAUDE.md`. Feed failures back to the owning agent by name.
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
- **Set a recurring check for the usage window** (`CronCreate`, every 5 hours — cron cannot express
  5h05m, so have the fired prompt read real usage with the `claude-usage` skill rather than trusting the
  cadence; pass the right `--plan`). When it fires: resume agents that stopped on the usage limit,
  then top the queue back up. Note an agent stopped by the *harness* cannot be resumed at all — relaunch
  a replacement pointed at its on-disk work, and make it read that work before planning. Cron jobs are
  session-only and expire after 7 days.
- **Three things to re-check on every such pass**, because each has bitten: `git diff --cached` must be
  empty (a stale blob there has twice been a reversal of someone's just-landed work, waiting for the
  next agent's `git commit` to ship it under their message); `git status --short` for a tree someone
  broke; and `docker ps`, since the live oracles die with Docker and agents then waste time treating an
  unreachable oracle as evidence.
