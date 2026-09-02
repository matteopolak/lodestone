//! Data-driven packet dispatch tables with construction-time checks.
//!
//! A family's `handle_play` today is an `if packet_id == X { .. } else if ..
//! else { /* silently drop it */ }` chain. The terminal `_ =>` arm is an
//! island factory: a packet the wire carries and nobody wired a translation
//! for is silently and permanently unhandled, with nothing red anywhere.
//!
//! [`Table`] replaces that chain with data: a slice of named [`Handler`]s
//! plus a slice of deliberately-unhandled entries ([`IGNORED`], name and
//! *reason*), built once per negotiated protocol by [`Table::build`].
//! Construction fails loudly, naming the offending packet, in exactly three
//! cases — see [`DispatchError`]:
//!
//! - a packet id the protocol's own table declares, with no [`Handler`] bound
//!   to its name and no matching [`IGNORED`] entry either (the `_ =>` case,
//!   reborn as a build-time error instead of a silent drop);
//! - a [`Handler`] whose declared [`ProtocolRange`] excludes the protocol
//!   being built for (a range widened without checking it against every
//!   protocol it now claims to cover);
//! - a [`Handler`] bound to a name absent from the protocol's own id table
//!   (a handler nobody's wire will ever call).
//!
//! Both `Handler` and `Table` are generic over the payload a family actually
//! runs (`T`) — this module knows nothing about `ClientEvent`, `Directive`,
//! or any session type, and never will; that keeps it a leaf of the
//! dependency graph everything else builds on.

use crate::ProtocolRange;
use std::collections::BTreeMap;

/// One dispatch entry: the protocol range its definition covers, and the
/// payload a family runs for it (a `fn` pointer, a closure, an enum — the
/// family decides). Paired with its packet name in a `&[(&str, Handler<T>)]`
/// table, mirroring the shape of the generated `packet_ids.rs` `ENTRIES`
/// tables this module is designed to be built from.
#[derive(Debug, Clone, Copy)]
pub struct Handler<T> {
    /// Protocol range this handler is valid for.
    pub protocols: ProtocolRange,
    /// The payload a family runs for this packet.
    pub run: T,
}

impl<T> Handler<T> {
    /// Builds a handler bound to `protocols`.
    #[must_use]
    pub const fn new(protocols: ProtocolRange, run: T) -> Self {
        Self { protocols, run }
    }
}

/// One entry on a family's declared-ignore list: a packet name paired with
/// *why* no handler translates it (e.g. `"server-only debug packet"`, or
/// `"v770 has this; backport"`). The all-caps name mirrors the per-family
/// `static IGNORED: &[dispatch::IGNORED]` table this type is meant to
/// populate.
#[derive(Debug, Clone, Copy)]
#[allow(non_camel_case_types)]
pub struct IGNORED {
    /// Canonical packet name, matching the id table's own name column.
    pub name: &'static str,
    /// Why this packet has no handler.
    pub reason: &'static str,
}

impl IGNORED {
    /// Builds an ignore-list entry.
    #[must_use]
    pub const fn new(name: &'static str, reason: &'static str) -> Self {
        Self { name, reason }
    }
}

/// Every way [`Table::build`] can refuse to construct a table. Each variant
/// names the offending packet so the failure is actionable without a
/// debugger.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DispatchError {
    /// A packet id the protocol's table declares has neither a bound
    /// [`Handler`] nor a matching [`IGNORED`] entry — the `_ =>` island,
    /// caught at construction instead of dropped silently forever.
    #[error("{name:?} (id {id}) has no handler and is not on the IGNORED list")]
    UnlistedId {
        /// Canonical packet name.
        name: &'static str,
        /// Numeric packet id in the protocol's own table.
        id: i32,
    },
    /// A bound [`Handler`]'s declared range excludes the protocol being
    /// built for.
    #[error("handler for {name:?} (id {id}) declares protocols {declared} which excludes protocol {protocol}")]
    OutOfRange {
        /// Canonical packet name.
        name: &'static str,
        /// Numeric packet id in the protocol's own table.
        id: i32,
        /// Protocol version the table is being built for.
        protocol: i32,
        /// The handler's own declared range.
        declared: ProtocolRange,
    },
    /// A [`Handler`] is bound to a name the protocol's id table does not
    /// contain, so its wire can never reach it.
    #[error("handler for {name:?} has no matching packet id in this protocol's table")]
    UnboundHandler {
        /// Canonical packet name the handler was bound to.
        name: &'static str,
    },
    /// Two handlers were bound to the same packet name.
    #[error("duplicate handler declared for {name:?}")]
    DuplicateHandler {
        /// Canonical packet name bound twice.
        name: &'static str,
    },
    /// An [`IGNORED`] entry names a packet the protocol's id table does not
    /// contain — dead backlog, not a live exemption.
    #[error("{name:?} is on the IGNORED list but has no matching packet id in this protocol's table")]
    StaleIgnored {
        /// Canonical packet name the stale entry names.
        name: &'static str,
    },
}

/// A protocol's `packet id -> handler` map, built once by [`Table::build`]
/// and then queried by [`Table::get`] on every incoming packet.
#[derive(Debug)]
pub struct Table<'a, T> {
    entries: BTreeMap<i32, &'a T>,
}

impl<'a, T> Table<'a, T> {
    /// Builds a dispatch table for `protocol` from the protocol's own
    /// `(name, id)` packet table (the shape `gen-packet-ids` emits as
    /// `ENTRIES`), a family's bound handlers, and its declared ignore list.
    ///
    /// # Errors
    ///
    /// Returns [`DispatchError`] on the first of: an id with no handler and
    /// no ignore entry, a handler whose range excludes `protocol`, a handler
    /// bound to a name absent from `ids`, a duplicate handler name, or a
    /// stale ignore entry naming an absent packet. See the module docs for
    /// why each of these is a real defect rather than a formality.
    pub fn build(
        protocol: i32,
        ids: &[(&'static str, i32)],
        handlers: &'a [(&'static str, Handler<T>)],
        ignored: &[IGNORED],
    ) -> Result<Self, DispatchError> {
        let mut handler_by_name: BTreeMap<&'static str, &'a Handler<T>> = BTreeMap::new();
        for (name, handler) in handlers {
            if handler_by_name.insert(name, handler).is_some() {
                return Err(DispatchError::DuplicateHandler { name });
            }
        }

        let mut entries = BTreeMap::new();
        let mut matched_handlers: BTreeMap<&'static str, ()> = BTreeMap::new();
        let mut matched_ignored: BTreeMap<&'static str, ()> = BTreeMap::new();

        for &(name, id) in ids {
            if let Some(&handler) = handler_by_name.get(name) {
                if !handler.protocols.contains(protocol) {
                    return Err(DispatchError::OutOfRange {
                        name,
                        id,
                        protocol,
                        declared: handler.protocols,
                    });
                }
                matched_handlers.insert(name, ());
                entries.insert(id, &handler.run);
                continue;
            }
            if let Some(entry) = ignored.iter().find(|entry| entry.name == name) {
                matched_ignored.insert(entry.name, ());
                continue;
            }
            return Err(DispatchError::UnlistedId { name, id });
        }

        for (name, _) in handlers {
            if !matched_handlers.contains_key(name) {
                return Err(DispatchError::UnboundHandler { name });
            }
        }
        for entry in ignored {
            if !matched_ignored.contains_key(entry.name) {
                return Err(DispatchError::StaleIgnored { name: entry.name });
            }
        }

        Ok(Self { entries })
    }

    /// Looks up the handler payload bound to `id`, if any.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<&'a T> {
        self.entries.get(&id).copied()
    }

    /// Number of dispatchable entries in this table.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether this table has no dispatchable entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDS: &[(&str, i32)] = &[
        ("minecraft:set_health", 0),
        ("minecraft:add_entity", 1),
        ("minecraft:debug_sample", 2),
    ];

    fn handle_set_health(_ctx: i32) -> i32 {
        11
    }

    fn handle_add_entity(_ctx: i32) -> i32 {
        4
    }

    #[test]
    fn build_succeeds_and_dispatches_every_handled_id() {
        let handlers: &[(&str, Handler<fn(i32) -> i32>)] = &[
            (
                "minecraft:set_health",
                Handler::new(ProtocolRange::ALL, handle_set_health),
            ),
            (
                "minecraft:add_entity",
                Handler::new(ProtocolRange::new(1, 776), handle_add_entity),
            ),
        ];
        let ignored = &[IGNORED::new(
            "minecraft:debug_sample",
            "server-only debug packet",
        )];

        let table = Table::build(776, IDS, handlers, ignored).expect("table should build");

        assert_eq!(table.len(), 2);
        assert_eq!(table.get(0).map(|run| run(0)), Some(11));
        assert_eq!(table.get(1).map(|run| run(0)), Some(4));
        assert_eq!(table.get(2), None, "ignored ids dispatch to nothing");
    }

    #[test]
    fn negative_control_unlisted_id_fails_construction() {
        // Neither a handler nor an ignore entry names `minecraft:debug_sample`
        // (id 2): this is the `_ =>` island, reborn as a build error. Watched
        // failing first (this is the assertion for that failure), then the
        // handled/ignored variant above proves the same table shape succeeds
        // once every id is accounted for.
        let handlers: &[(&str, Handler<fn(i32) -> i32>)] = &[
            (
                "minecraft:set_health",
                Handler::new(ProtocolRange::ALL, handle_set_health),
            ),
            (
                "minecraft:add_entity",
                Handler::new(ProtocolRange::ALL, handle_add_entity),
            ),
        ];

        let err = Table::build(776, IDS, handlers, &[]).expect_err("unlisted id must fail");
        assert_eq!(
            err,
            DispatchError::UnlistedId {
                name: "minecraft:debug_sample",
                id: 2,
            }
        );
    }

    #[test]
    fn negative_control_out_of_range_handler_fails_construction() {
        // `add_entity`'s handler is bound only up to protocol 340; building
        // for 776 must be rejected rather than silently dispatching a
        // handler that was never reviewed against this protocol.
        let handlers: &[(&str, Handler<fn(i32) -> i32>)] = &[
            (
                "minecraft:set_health",
                Handler::new(ProtocolRange::ALL, handle_set_health),
            ),
            (
                "minecraft:add_entity",
                Handler::new(ProtocolRange::new(1, 340), handle_add_entity),
            ),
        ];
        let ignored = &[IGNORED::new(
            "minecraft:debug_sample",
            "server-only debug packet",
        )];

        let err = Table::build(776, IDS, handlers, ignored).expect_err("out-of-range must fail");
        assert_eq!(
            err,
            DispatchError::OutOfRange {
                name: "minecraft:add_entity",
                id: 1,
                protocol: 776,
                declared: ProtocolRange::new(1, 340),
            }
        );
    }

    #[test]
    fn negative_control_unbound_handler_fails_construction() {
        // A handler bound to a name this protocol's own id table does not
        // carry can never be reached by any real packet -- a dead handler
        // declaration, and construction must say so by name.
        let handlers: &[(&str, Handler<fn(i32) -> i32>)] = &[
            (
                "minecraft:set_health",
                Handler::new(ProtocolRange::ALL, handle_set_health),
            ),
            (
                "minecraft:add_entity",
                Handler::new(ProtocolRange::ALL, handle_add_entity),
            ),
            (
                "minecraft:does_not_exist",
                Handler::new(ProtocolRange::ALL, handle_add_entity),
            ),
        ];
        let ignored = &[IGNORED::new(
            "minecraft:debug_sample",
            "server-only debug packet",
        )];

        let err = Table::build(776, IDS, handlers, ignored).expect_err("unbound must fail");
        assert_eq!(
            err,
            DispatchError::UnboundHandler {
                name: "minecraft:does_not_exist",
            }
        );
    }

    #[test]
    fn stale_ignored_entry_fails_construction() {
        let handlers: &[(&str, Handler<fn(i32) -> i32>)] = &[
            (
                "minecraft:set_health",
                Handler::new(ProtocolRange::ALL, handle_set_health),
            ),
            (
                "minecraft:add_entity",
                Handler::new(ProtocolRange::ALL, handle_add_entity),
            ),
        ];
        let ignored = &[
            IGNORED::new("minecraft:debug_sample", "server-only debug packet"),
            IGNORED::new("minecraft:long_gone", "no longer emitted by any protocol"),
        ];

        let err = Table::build(776, IDS, handlers, ignored).expect_err("stale ignore must fail");
        assert_eq!(
            err,
            DispatchError::StaleIgnored {
                name: "minecraft:long_gone",
            }
        );
    }

    #[test]
    fn duplicate_handler_fails_construction() {
        let handlers: &[(&str, Handler<fn(i32) -> i32>)] = &[
            (
                "minecraft:set_health",
                Handler::new(ProtocolRange::ALL, handle_set_health),
            ),
            (
                "minecraft:set_health",
                Handler::new(ProtocolRange::ALL, handle_set_health),
            ),
        ];

        let err = Table::build(776, IDS, handlers, &[]).expect_err("duplicate must fail");
        assert_eq!(
            err,
            DispatchError::DuplicateHandler {
                name: "minecraft:set_health",
            }
        );
    }
}
