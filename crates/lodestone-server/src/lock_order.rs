//! Debug-only lock-order checks for the server's independently locked handles.
//!
//! The server has several small, cloneable mutex-backed handles rather than one
//! world lock. Keeping their acquisition order explicit catches a cross-handle
//! deadlock while it is still a useful panic, before regionised ticking would
//! multiply the number of locks involved. Release builds deliberately carry no
//! tracking state or synchronization for this diagnostic.

/// The globally ordered subset of server locks that can be held while invoking
/// a handle callback. New classes belong after the classes they may acquire.
///
/// `ScheduledQueues` precedes the block-entity and mob stores because the tick
/// loop takes it around its scheduled-and-physics pass, which may consult both.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum LockClass {
    ScheduledQueues,
    ScheduledStaged,
    BlockEntities,
    Mobs,
    Border,
    GameRules,
    WorldState,
    AccessLists,
}

/// Records one held lock in debug and test builds.
pub(crate) struct HeldLock {
    #[cfg(any(debug_assertions, test))]
    class: LockClass,
}

/// Asserts that `class` does not invert the per-thread global lock order.
///
/// The guard must live at least as long as the corresponding mutex guard. The
/// handle implementations arrange that automatically by declaring it before
/// locking and keeping it through their synchronous callback.
#[inline]
pub(crate) fn acquire(class: LockClass) -> HeldLock {
    #[cfg(any(debug_assertions, test))]
    {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            if let Some(previous) = held.last().copied() {
                assert!(
                    previous <= class,
                    "lock-order violation: attempted to acquire {class:?} \
                     while holding {previous:?}"
                );
            }
            held.push(class);
        });
        return HeldLock { class };
    }

    #[cfg(not(any(debug_assertions, test)))]
    {
        let _ = class;
        HeldLock {}
    }
}

/// Rejects dispatching owner-local work while the block-entity registry is
/// still borrowed. The current mob owner methods are invoked through their
/// existing mob handle, and scheduled physics still surrounds those calls, so
/// those two lock classes remain legal until their own hand-off migrations.
/// Block-entity workers already snapshot and commit outside this lock; keep
/// that boundary mechanically enforced here.
#[inline]
pub(crate) fn assert_owner_work_without_block_entity_lock() {
    #[cfg(any(debug_assertions, test))]
    HELD.with(|held| {
        let held = held.borrow();
        assert!(
            !held.contains(&LockClass::BlockEntities),
            "owner-local work must start without the block-entity registry lock; held {held:?}"
        );
    });
}

#[cfg(not(any(debug_assertions, test)))]
#[allow(dead_code)]
const _: fn() = assert_owner_work_without_block_entity_lock;

#[cfg(any(debug_assertions, test))]
std::thread_local! {
    static HELD: std::cell::RefCell<Vec<LockClass>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        #[cfg(any(debug_assertions, test))]
        HELD.with(|held| {
            let popped = held.borrow_mut().pop();
            debug_assert_eq!(popped, Some(self.class), "lock-order guards must drop in LIFO order");
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ordered_acquisitions_are_allowed() {
        let scheduled = crate::scheduled_tick::ScheduledTickHandle::new();
        let entities = crate::block_entities::BlockEntityHandle::new();

        scheduled.with(|_| entities.with(|_| {}));
    }

    #[test]
    #[should_panic(expected = "lock-order violation")]
    fn intentionally_inverted_production_handles_panic() {
        let rules = crate::game_rules::GameRulesHandle::new();
        let scheduled = crate::scheduled_tick::ScheduledTickHandle::new();

        rules.with(|_| scheduled.with(|_| {}));
    }

    #[test]
    #[should_panic(expected = "owner-local work must start without the block-entity registry lock")]
    fn owner_work_rejects_a_held_block_entity_lock() {
        let entities = crate::block_entities::BlockEntityHandle::new();
        entities.with(|_| {
            crate::lock_order::assert_owner_work_without_block_entity_lock();
        });
    }

}
