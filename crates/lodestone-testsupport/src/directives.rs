//! Order-tolerant assertions for `Vec<Directive>` produced by
//! `VersionAdapter::handle_packet`/`ClientDriver::execute`-style call sites.
//!
//! Many directive assertions use exact length and exact order
//! (`assert_eq!(…, vec![Directive…])` or `match directives.as_slice() { … }`)
//! because no subsequence or set-based alternative is safe for every case.
//! That is correct for *some* of them: per
//! `Driver::execute`'s doc comment, order genuinely changes wire behaviour
//! when a `SetCompression` reframes later `Send`s, when `SetState` into
//! `Configuration` performs an extra socket write, when a `Disconnect`
//! stops everything after it, or when an `Emit` auto-responds (KeepAlive,
//! TeleportPlayer) before a later `Send`. Those call sites must keep using
//! `assert_eq!`/`match` and should never be converted to use this module.
//!
//! But a large fraction of the assertions are **Emit-only**: every
//! directive in the sequence is `Directive::Emit(ClientEvent)`, nothing else
//! is present, and the events fold into independent, commutative pieces of
//! state downstream (see `crates/lodestone-ecs/src/session.rs`'s per-field
//! `match event` arms). For those, exact order is not a real invariant —
//! it's an accident of the classifier/adapter's internal call order — and
//! asserting it anyway means every new `ClientEvent` variant added anywhere
//! near an existing one can be a spurious test break when the events update
//! disjoint resources and their fold is commutative. This helper captures
//! that case while retaining strict checks for directives whose order has
//! observable wire effects.
//!
//! [`assert_emits_set`] is the replacement for exactly that shape: it
//! requires every directive to be an `Emit` (a `Send`/`SetState`/
//! `SetCompression`/`BeginEncryption` mixed in panics loudly rather than
//! being silently ignored, since those are the shapes above where order
//! *is* load-bearing), then compares the emitted `ClientEvent`s against the
//! expected set as a multiset, order-independent.
use lodestone_model::{ClientEvent, Directive};

/// Asserts that `directives` consists **only** of [`Directive::Emit`]
/// values, and that the multiset of emitted [`ClientEvent`]s equals
/// `expected` — same events, same counts, **order not checked**.
///
/// # Panics
///
/// - If any element of `directives` is not `Directive::Emit` — this helper
///   does not know how to judge whether a `Send`/`SetState`/
///   `SetCompression`/`BeginEncryption` mixed into the same batch is safe to
///   reorder, and guessing wrong would make a real wire-order bug
///   invisible. Assert those sequences exactly with `assert_eq!` instead.
/// - If the emitted events and `expected` differ as multisets. The panic
///   message reports what was missing and what was unexpected separately,
///   since "not equal" alone doesn't say which direction the drift went.
///
/// # Example
///
/// ```
/// use lodestone_model::{ClientEvent, Directive};
/// use lodestone_testsupport::assert_emits_set;
///
/// let directives = vec![
///     Directive::Emit(ClientEvent::KeepAlive { id: 1 }),
///     Directive::Emit(ClientEvent::KeepAlive { id: 2 }),
/// ];
/// assert_emits_set(
///     &directives,
///     &[
///         ClientEvent::KeepAlive { id: 2 },
///         ClientEvent::KeepAlive { id: 1 },
///     ],
/// );
/// ```
pub fn assert_emits_set(directives: &[Directive], expected: &[ClientEvent]) {
    let mut actual = Vec::with_capacity(directives.len());
    for directive in directives {
        match directive {
            Directive::Emit(event) => actual.push(event.clone()),
            other => panic!(
                "assert_emits_set: every directive must be `Emit` (got {other:?} in \
                 {directives:?}). A Send/SetState/SetCompression/BeginEncryption mixed \
                 into an Emit-only sequence usually carries a real wire-order \
                 dependency — assert this sequence exactly with assert_eq! instead of \
                 using this helper."
            ),
        }
    }

    let mut remaining: Vec<ClientEvent> = expected.to_vec();
    let mut unexpected = Vec::new();
    for event in actual {
        if let Some(pos) = remaining.iter().position(|candidate| *candidate == event) {
            remaining.remove(pos);
        } else {
            unexpected.push(event);
        }
    }

    assert!(
        remaining.is_empty() && unexpected.is_empty(),
        "assert_emits_set mismatch:\n  missing (expected but not emitted): {remaining:#?}\n  \
         unexpected (emitted but not expected): {unexpected:#?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_matching_events_in_a_different_order() {
        let directives = vec![
            Directive::Emit(ClientEvent::KeepAlive { id: 1 }),
            Directive::Emit(ClientEvent::KeepAlive { id: 2 }),
        ];
        assert_emits_set(
            &directives,
            &[
                ClientEvent::KeepAlive { id: 2 },
                ClientEvent::KeepAlive { id: 1 },
            ],
        );
    }

    #[test]
    #[should_panic(expected = "mismatch")]
    fn rejects_a_missing_event() {
        let directives = vec![Directive::Emit(ClientEvent::KeepAlive { id: 1 })];
        assert_emits_set(
            &directives,
            &[
                ClientEvent::KeepAlive { id: 1 },
                ClientEvent::KeepAlive { id: 2 },
            ],
        );
    }

    #[test]
    #[should_panic(expected = "mismatch")]
    fn rejects_an_extra_event() {
        let directives = vec![
            Directive::Emit(ClientEvent::KeepAlive { id: 1 }),
            Directive::Emit(ClientEvent::KeepAlive { id: 2 }),
        ];
        assert_emits_set(&directives, &[ClientEvent::KeepAlive { id: 1 }]);
    }

    #[test]
    #[should_panic(expected = "mismatch")]
    fn treats_duplicate_counts_as_significant_not_just_set_membership() {
        // Two KeepAlive{id:1} emitted but only one expected: this is a
        // multiset comparison, not a set-membership check, so a duplicate
        // must still be caught.
        let directives = vec![
            Directive::Emit(ClientEvent::KeepAlive { id: 1 }),
            Directive::Emit(ClientEvent::KeepAlive { id: 1 }),
        ];
        assert_emits_set(&directives, &[ClientEvent::KeepAlive { id: 1 }]);
    }

    #[test]
    #[should_panic(expected = "every directive must be `Emit`")]
    fn rejects_a_non_emit_directive_rather_than_silently_ignoring_it() {
        let directives = vec![
            Directive::Emit(ClientEvent::KeepAlive { id: 1 }),
            Directive::SetCompression(256),
        ];
        assert_emits_set(&directives, &[ClientEvent::KeepAlive { id: 1 }]);
    }
}
