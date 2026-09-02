//! # The Java-plugin bridge — host-side foundation
//!
//! ## What it is
//!
//! The Rust half of running **real, unmodified Bukkit/Spigot/Paper plugin jars**
//! against this server, by backing Paper's own `net.minecraft.*` calls rather
//! than reimplementing the Bukkit API. [`docs/java-plugin-bridge.md`] carries
//! the whole design: the licensing decision, the measured census that sizes the
//! work, the ABI decision, and the threading model this crate implements.
//!
//! This crate is the **foundation tranche**, not the bridge. What is here is the
//! part that is JVM-independent, testable today, and load-bearing for
//! everything after it:
//!
//! - [`port`] — the request/response seam that makes the `EcsHandle`
//!   reentrancy deadlock **unrepresentable** from a JNI callback, rather than
//!   merely warned about.
//! - [`identity`] — generational handles, so a plugin holding a `Player`
//!   reference across ticks gets a reported failure rather than somebody else's
//!   entity.
//!
//! ## What is deliberately not here
//!
//! **No `jni` dependency and no `libjvm` linkage.** Not "not enabled yet" —
//! not present. The JNI entry points land with the spike that can execute them;
//! writing them now would put untestable code in the tree, and this repo's
//! standing rule is to wire code or delete it, never to leave a third state.
//! `tests/zero_cost_graph.rs` guards the boundary so that when they do land
//! they land behind a feature, and it explains why a `Cargo.lock` grep is the
//! wrong instrument for checking this (the lockfile already contains `jni`, via
//! Android transitives that reach no host build).
//!
//! **No speculative NMS request enum.** The census measures ~6,991 distinct
//! members that Paper's Bukkit layer reaches for; enumerating them ahead of the
//! implementation order that census dictates would be planning fiction.
//! [`port::WorldPort`] is generic over the request type for exactly that
//! reason — the mechanism is designed, the surface drops in.
//!
//! ## How it works — the one-paragraph version
//!
//! A Java event handler runs on **its own thread**, never on the tick thread,
//! and holds only a [`port::WorldPort`] — a channel endpoint with no `World`,
//! no `EcsHandle` and no guard reachable from it. The tick thread services that
//! port, taking a *short* write guard per request through
//! [`port::service_with_world`]. Because the servicer takes the guard itself
//! via `lodestone_ecs::hold_write`, a host that ever wires it inside an
//! existing guard gets a panic naming both call sites instead of the silent
//! hang that froze this client once already.
//!
//! ## How to change it
//!
//! Read [`port`]'s module doc before touching anything in it: two specific
//! edits (adding a handle field to `WorldPort`, removing the request deadline)
//! re-open the deadlock while still compiling, and `tests/reentrancy.rs` exists
//! to catch precisely those.
//!
//! ## Configuration
//!
//! [`port::DEFAULT_REQUEST_DEADLINE`]. Nothing at build time.
//!
//! ## Dependencies
//!
//! `lodestone-ecs`, and nothing else. Naming `lodestone-shell` or
//! `lodestone-client` would hand this crate a route to a real `EcsHandle` —
//! `tests/reentrancy.rs` asserts it names neither, reusing
//! `lodestone-plugin-support`'s own static check rather than writing a second
//! one.
//!
//! [`docs/java-plugin-bridge.md`]: https://github.com/matteopolak/lodestone/blob/main/docs/java-plugin-bridge.md

pub mod identity;
pub mod port;

pub use identity::{ObjectKind, ObjectRef, ObjectRegistry, ResolveError};
pub use port::{
    DEFAULT_REQUEST_DEADLINE, PortError, PortServicer, WorldPort, channel, service_with_world,
};
