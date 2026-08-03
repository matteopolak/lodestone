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
- **Contention is your main job.** Three or four agents at once, not more — past that they collide.
  Assign file ownership explicitly, name what other agents are holding, and tell agents to report
  rather than edit outside their territory. If two tasks want the same file, send one agent, not two.
  Have them commit through a private `GIT_INDEX_FILE` + `commit-tree`, reading the tree and committing
  in one step.
- **Every commit invalidates every other agent's index entries**, so `git status` starts reporting staged
  *deletions* of files the commit just added, which the next `git commit` would ship. After any commit,
  `git reset -- <your paths>` and confirm `git diff --cached --name-only` is empty.
- **Ask what consumes it.** A subsystem that is built, tested and called by nothing is this repo's
  dominant defect. "Nothing consumes it yet" is an acceptable answer if stated; found later, it is not.
- **Never launch the game.** The user drives that.
