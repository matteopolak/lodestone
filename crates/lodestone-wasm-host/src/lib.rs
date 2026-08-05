//! Runtime plugin loading for Lodestone: a `wasmtime` host that loads a
//! WebAssembly component from a file on disk and drives it through a
//! capability-gated ABI.
//!
//! # What it is
//!
//! The second plugin tier. The first — the native one, `crates/plugins/` — is
//! ordinary `bevy` plugins with `&mut World`, registered at compile time through
//! [`lodestone_app::client_app`]. This tier trades that power for three things
//! the native tier structurally cannot offer: **runtime loading** (drop a file in
//! a directory, no rebuild — the owner's original ask), **platform independence**
//! (one `.wasm` artifact runs on every host we build for), and **failure
//! isolation** (a guest that traps, panics or spins forever is unloaded and
//! reported; a native plugin that does any of those takes the process with it).
//!
//! It is **additive**. Nothing about the native tier changes, and
//! `crates/plugins/lodestone-{autopilot,nav,event-logger}` are untouched by this
//! crate's existence — `docs/plugin-api.md`'s cost analysis is still right that
//! Baritone-class work belongs in the native tier, and always will.
//!
//! # How it works
//!
//! ```text
//!   plugin.toml  ──parse──▶ Manifest ──requested capabilities──┐
//!   plugin.wasm  ──sniff──▶ component ────────────────────┐    │
//!                                                         ▼    ▼
//!                                              PluginHost::load_file
//!                                                         │
//!                                     Linker gets only the granted imports
//!                                                         │
//!                                                         ▼
//!   Messages<GameEvent> ──lift──▶ list<event> ──▶ guest.on-tick ──▶ list<action>
//!                                                                        │
//!                                                                       lower
//!                                                                        ▼
//!                                                                   ActionQueue
//! ```
//!
//! The four pieces, and the issue each closes:
//!
//! | module | issue | what it owns |
//! |---|---|---|
//! | [`host`] | [#172](https://github.com/matteopolak/lodestone/issues/172) | the embedding: engine, per-guest `Store`, the gated `Linker`, fuel preemption |
//! | [`capability`] | #172/#175 | the capability vocabulary and the two enforcement mechanisms |
//! | `wit/lodestone-plugin.wit` | [#173](https://github.com/matteopolak/lodestone/issues/173) | the ABI surface — the WIT world, vendored as the single source of truth |
//! | `manifest` | [#175](https://github.com/matteopolak/lodestone/issues/175) | `plugin.toml`: name, version, ABI world, priority, declared capabilities |
//!
//! The last two rows land in follow-up commits on this same crate; this commit is
//! the scaffold and the world it loads.
//!
//! # Why the ABI is the intent doctrine, not a new vocabulary
//!
//! The load-bearing observation, and it is not this crate's: **`docs/plugin-api.md`'s
//! intent doctrine is accidentally an ABI spec.** Every way a native plugin
//! observes or acts is already call-shaped or copy-shaped —
//! `GameEvent(ClientEvent)` is a `Clone` value, an intent is a small POD struct
//! inserted and removed, an outcome is a small POD struct polled, an action is a
//! value pushed onto a `Vec`. None of them hands out a borrow into the `World`,
//! and a surface that never hands out a machine is exactly a surface that
//! serialises.
//!
//! So the WIT `event` and `action` variants are not a parallel dialect. They are a
//! **curated subset of the same vocabulary**, and a plugin author graduates from
//! this tier to the native one by gaining APIs rather than by rewriting against
//! different ones. Where the mapping is genuinely lossy — and it is, in three
//! specific places — `docs/wasm-plugin-host.md` says so instead of papering over
//! it.
//!
//! # How to change it
//!
//! Adding an event or action means editing **three** places, and the compiler only
//! catches the first two:
//!
//! 1. `wit/lodestone-plugin.wit` — the variant.
//! 2. `src/abi.rs` — the lift from `ClientEvent` or the lower to `ClientAction`,
//!    plus the capability that gates it.
//! 3. `src/capability.rs` — a new [`Capability`] if the new arm is not covered by
//!    an existing one, and [`CapabilitySet::default_policy`] if it should be granted
//!    by default. **Do not grant an import-column capability by default.**
//!
//! The gotcha that will bite: a guest built against an older `.wit` still loads
//! (the component model resolves imports by name), but a guest built against a
//! *newer* one does not, and the error is an unresolved import rather than
//! anything mentioning versions. [`host::ABI_WORLD`] plus the manifest's `abi`
//! field is what turns that into a legible message, so a new arm that changes the
//! world's meaning must bump the world version in the `.wit` **and** in
//! `ABI_WORLD`.
//!
//! # Configuration
//!
//! [`host::PluginHost::new`] takes the policy; [`capability::CapabilitySet::default_policy`]
//! is the "denied unless granted" default. `with_fuel`, `with_memory_limit` and
//! `with_filesystem_root` are the three knobs.
//!
//! # Dependencies
//!
//! `wasmtime` (pinned minor, `default-features = false`, **no `wasmtime-wasi`**),
//! `wit-component` for the core-module-to-component encode, `toml`/`serde` for the
//! manifest, and the version-free vocabulary crates `lodestone-model` and
//! `lodestone-ecs`. Nothing render-shaped and no protocol family, on purpose.
//!
//! See [`docs/wasm-plugin-host.md`](https://github.com/matteopolak/lodestone/blob/main/docs/wasm-plugin-host.md)
//! for the measured numbers, the capability probe, and the pending wires.

mod bindings;
pub mod capability;
pub mod host;

pub use capability::{Capability, CapabilitySet};
pub use host::{
    ABI_WORLD, Action, BlockOffset, ChatKind, ChatMessage, DEFAULT_FUEL_PER_TICK,
    DEFAULT_MEMORY_LIMIT, Event, Hand, Health, HostError, LoadedPlugin, LogLevel, PluginHost,
    PluginInfo, SectionBlocksChanged, SectionPos,
};

/// The WIT world this host speaks, as text — the same bytes `include_str!` pulled
/// out of `wit/lodestone-plugin.wit` at compile time.
///
/// Exposed so a plugin author can get the ABI out of the binary they are building
/// against rather than out of a checkout that may have moved on from it, which is
/// the difference between a legible version mismatch and an unresolved-import
/// error. A `lodestone --dump-plugin-wit` flag is the intended eventual consumer.
pub const WIT_WORLD_SOURCE: &str = include_str!("../wit/lodestone-plugin.wit");
