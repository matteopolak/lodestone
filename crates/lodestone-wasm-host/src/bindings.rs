//! `wasmtime`'s host-side bindings, generated from the vendored
//! [`wit/lodestone-plugin.wit`](../wit/lodestone-plugin.wit).
//!
//! The generated module tree is `lodestone::plugin::{types, logging,
//! filesystem}`; [`crate`] re-exports the handful of names callers need so that
//! nothing outside this crate spells a `bindings::lodestone::plugin::…` path.
//!
//! # Why the exports are called through `get_typed_func` rather than the
//! # generated `Plugin::instantiate`
//!
//! `bindgen!` also generates a `Plugin` struct whose `instantiate` links *every*
//! import in the world. That is the wrong shape for a capability host: the whole
//! design is that an import is present in the `Linker` only when policy grants
//! it, and a guest that does not use an ungranted import must still load. So the
//! host instantiates through the `Linker` itself and reaches the three exports with
//! `Instance::get_typed_func`, using the generated `event`/`action` types — which
//! derive `ComponentType`/`Lift`/`Lower` — as the signature. The typed lifting is
//! the part worth having; the all-or-nothing linking is not.

#![allow(clippy::needless_lifetimes, missing_debug_implementations)]

wasmtime::component::bindgen!({
    world: "plugin",
    path: "wit",
    // `PartialEq` so a test can assert an exact `Vec<Action>` rather than
    // pattern-matching arm by arm. That matters more than it sounds: an exact
    // equality over the whole returned list is what catches a lowering that
    // produces the right actions in the wrong *order*, and order is send order on
    // the wire — a guarantee this ABI makes explicitly.
    additional_derives: [PartialEq],
});
