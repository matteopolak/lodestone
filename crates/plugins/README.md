# `crates/plugins/`

Crates that **consume** Lodestone's public plugin API. Not engine crates.

A plugin here is an ordinary `impl bevy_app::Plugin` added with `App::add_plugins`,
plus whatever plain libraries it needs. It orders against the public `SystemSet`
labels (`lodestone_ecs::{IngestSet, TickSet, FrameSet, ExtractSet}`), reads and
writes components and resources, and reaches the wire by pushing a
`lodestone_model::ClientAction` onto the `ActionQueue` resource. See
[`docs/plugin-api.md`](../../docs/plugin-api.md) for the surface and
[`docs/bevy-migration.md`](../../docs/bevy-migration.md) §6 for the trust model.

## What belongs here

- A crate whose reason to exist is *behaviour built on top of the client*, not a
  part of the client. If deleting it would leave a working client, it belongs here.
- Its supporting libraries, when they exist only for it. `lodestone-nav` is a plain
  library with no bevy dependency and it still lives here, because
  `lodestone-autopilot` is its only consumer and it is not something the engine
  needs.

## What does not

- Anything the shipped client depends on. The dependency edge runs **plugins →
  engine**, never back. `cargo xtask check-isolation` and `check-connected` are the
  enforcement.
- Anything that names a protocol version crate. A plugin *may* legally depend on
  one (it is a leaf crate) but doing so version-locks it; reach version data through
  `lodestone_model::VersionAdapter` instead. Every gap in that seam is a defect in
  the seam, not a reason to route around it.
- GPU access. A plugin that wants to draw gets an `Extract`-time channel to append
  to, never a `wgpu::Device` — the 4-bind-group floor and the winding-sign
  invariant are constraints a plugin author cannot be expected to satisfy.

## Licensing

**Each plugin carries its own `LICENSE`**, in its own directory, and may differ
from the engine's `MIT OR Apache-2.0`. Rust's static linking means "linked into" is
"the whole binary", so the copyleft boundary is worth being explicit about; keeping
the file next to the crate is what makes it explicit per plugin rather than a
repo-wide claim.

| crate | licence | why |
|---|---|---|
| `lodestone-nav` | LGPL-3.0-or-later | owner's choice, not a constraint the design imposes — it is clean-room (`docs/baritone-port.md` §1.2) |
| `lodestone-autopilot` | LGPL-3.0-or-later | same |

## Current contents

| crate | what |
|---|---|
| `lodestone-nav` | version-free autonomous-navigation search core: world view, movement graph, simulation-derived cost model, goals, resumable A\*. No bevy, no ECS, no threads. |
| `lodestone-autopilot` | the bevy plugin: search driver, per-tick closed-loop executor. **Not a dependency of `lodestone-shell`** — a pre-implemented *external* plugin, for headless bots built on the library. It has no chat commands: `#goto` lived in the shell and was removed with the dependency, and a plugin cannot register its own commands until [#118](https://github.com/matteopolak/lodestone/issues/118). |
| `lodestone-event-logger` | a toy `EventPriority::Monitor` reader plugin (the plugin event bus and cross-plugin priority ordering work): observes `lodestone_ecs::GameEvent`, the plugin event bus, and reports through a plain `Arc<Mutex<_>>` outside the ECS rather than a resource. `MIT OR Apache-2.0`, no licensing table row needed. |
| `lodestone-plugin-support` | shared, non-engine conveniences: a per-plugin data directory and typed config helper (`paths`/`config`), and an in-memory namespaced key-value store attachable to an entity or a chunk (`persistent_data`). `MIT OR Apache-2.0`, no licensing table row needed. |
| `lodestone-worldedit` | a WorldEdit-class bulk-edit plugin: region fill/replace with a per-session undo/redo stack, built on `lodestone_world::World::fill_region_capturing` and `lodestone_ecs::ChunkWorldWrite`. `MIT OR Apache-2.0`, no licensing table row needed. |

`lodestone-nav`/`lodestone-autopilot` are documented in
[`docs/autonomous-navigation.md`](../../docs/autonomous-navigation.md), against the
design in [`docs/baritone-port.md`](../../docs/baritone-port.md).
`lodestone-event-logger` is documented in
[`docs/plugin-api.md`](../../docs/plugin-api.md)'s "The plugin event bus and cross-plugin
priority ordering" section.
`lodestone-plugin-support` is documented in
[`docs/plugin-data-and-config.md`](../../docs/plugin-data-and-config.md).
`lodestone-worldedit` is documented in
[`docs/worldedit-plugin.md`](../../docs/worldedit-plugin.md).
