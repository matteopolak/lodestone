# Parallel dispatch: file-disjoint work batches

## What it is

This is a durable coordination guide for dividing Lodestone work among concurrent agents. A batch is
a bounded outcome with a small, explicit file set, one integration path, and a verification command
that demonstrates a visible or connected result.

The goal is not to maximize the number of active agents. It is to keep independent work independent
while protecting the shared server, renderer, registry, and documentation seams from accidental
overlap.

## How it works

Classify proposed work by its owned files before dispatching it. A batch is ready when its files do
not overlap another active batch, or when the overlap is a named one-line broker patch that the
coordinator will apply after both owners finish. Read-only investigation can run alongside any
implementation work, but it must report evidence rather than change tracker state on its own.

Use these ownership lanes:

| lane | typical ownership | dispatch rule |
|---|---|---|
| protocol family | one directory under `crates/versions/` | One writer per family; shared protocol changes are a separate batch. |
| client feature | its feature crate plus one shell integration point | The integration point has a named owner and must reach pixels or an observable client event. |
| server subsystem | a new module and a narrow registration seam | Serialize edits to tick orchestration, entity lifecycle, and shared world state. |
| generated data | generator, input, generated output, and assertion test | The producer and consumer land together; generated output is never edited by hand. |
| documentation | one subsystem document | Change the document's durable description, not a running log. Regenerate the index after edits. |

A batch brief records the outcome, exclusive file set, permitted broker seams, prerequisites, and
verification. It must identify the production consumer: packet decode to event, event to state,
state to draw, or server action to wire output. A crate-local test alone is insufficient when the
work is intended to change gameplay or rendering.

Schedule server-core work as a single lane whenever it changes shared ticking, entity lifecycle,
or world administration. Other lanes may proceed in parallel only when their integration seam is
stable and they do not edit the same source file. Prefer one module per packet, species family, or
render path so independent work has naturally disjoint ownership.

### Dispatch matrix

This matrix is a file-ownership schedule, not a historical queue. Re-check the
working tree before assigning a row; only rows whose listed files remain
disjoint can run together.

| batch | exclusive files or seam | may run with | waits for |
|---|---|---|---|
| protocol client | `crates/versions/26.2/src/` client adapter, excluding server protocol code | legacy families, UI, render, audio | server-protocol owner when a shared packet changes |
| legacy family | exactly one of `crates/versions/1.8/`, `1.9/`, or `1.14/` | every other distinct family | external byte fixtures and canonical mapping |
| entity AI family | one `lodestone-entity/src/ai/roster/` or `brain/roster/` module | other family modules | the brokered roster registration line |
| menu and client UX | the named `menu/` screen set and its widget module | protocol, audio, entity AI | any shared option/settings owner |
| render path | one render subsystem plus its asset inputs | protocol, UI, audio | the renderer integration owner |
| audio or particles | its crate and shell feed | UI, protocol, AI | an event producer when none exists |
| tools and benchmarks | `xtask/`, bench harnesses, CI scripts | all implementation batches | no source writer; use committed inputs |
| documentation | one subsystem document | all disjoint source batches | regenerated index after edits |
| server service | a new listener/service module and its narrow registration seam | non-server-core batches | owner of `integrated.rs` or connection setup |
| server core | `server.rs`, `tick.rs`, `mobs.rs`, shared world state | no other server-core batch | the preceding server-core unit |

### Parallelism and dependencies

Start with the mutually disjoint protocol-client, one-per-family legacy,
entity-roster-module, menu, render, audio/particle, tooling, and documentation
batches. Reserve the server core for one owner. As those batches finish, route
server work through this dependency order:

```text
shared server state and narrow registration seams
    -> tick and world-state changes
    -> persistence or chunk-residency work
    -> population and species drivers
    -> higher-level AI, economy, bosses, and plugin surfaces
```

Within a subsystem, release a shared file before starting the next dependent
unit. An entity roster module can be prepared in parallel with another family,
but the `goals_for` registration and production driver are single-owner broker
patches. The same rule applies to packet decode: a family adapter may progress
independently, but a shared server dispatcher is one serialized patch. A stalled
independent batch can be replaced only by a row with no intersection with active
file sets.

## How to change it

Before dispatching, inspect `git status`, search the proposed file set, and ask the current owner of
an overlapping file whether the work can be split by module. Do not reserve a broad directory when a
smaller file-level boundary exists.

When a dependency is removed or a seam changes, update the batch brief rather than preserving the
old sequence. Delete a completed or obsolete row from a live dispatch board; this guide deliberately
does not retain a historical queue.

For a shared integration point, designate one owner. Contributors should prepare self-contained
modules and tests, then send the owner the exact API and behavior required for the narrow patch.
This avoids concurrent rewrites of high-contention files such as tick orchestration and registry
tables.

## Configuration

There is no runtime configuration. Coordination uses the repository's shared-checkout rules:
explicit file ownership, no broad staging or formatting, and foreground verification. Use `just
check-seam` whenever a batch changes version-family dependencies, and use the subsystem-specific
checks in addition to `just check` or `just test` as appropriate.

## Dependencies

This guide relies on the workspace layout, `just` recipes, `xtask` connectedness and island checks,
and the subsystem plans in `docs/plans/`. The detailed version seam is documented in
[`multi-protocol-seam.md`](../multi-protocol-seam.md); entity behavior and protocol-family design are
documented by the sibling plans in this directory.
