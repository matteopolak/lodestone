//! Bounded callback-depth accounting for the Java bridge.
//!
//! A bridge host enters [`CallbackDepthGuard`] at every native callback before
//! it can call back into Java. The guard is thread-local, so independent plugin
//! threads do not consume one another's budgets, and its `Drop` implementation
//! restores the counter on every return path.

use std::cell::Cell;

/// The default maximum number of nested bridge callbacks on one thread.
pub const DEFAULT_CALLBACK_DEPTH_LIMIT: u32 = 4;

thread_local! {
    static CALLBACK_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Why a callback could not enter the configured depth budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallbackDepthError {
    limit: u32,
}

impl CallbackDepthError {
    /// The budget that was exceeded.
    #[must_use]
    pub const fn limit(self) -> u32 {
        self.limit
    }
}

impl std::fmt::Display for CallbackDepthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "reentrant callback depth limit {} exceeded", self.limit)
    }
}

impl std::error::Error for CallbackDepthError {}

/// A scoped permit for one nested Java/Rust callback.
#[derive(Debug)]
pub struct CallbackDepthGuard {
    level: u32,
}

impl CallbackDepthGuard {
    /// Enter the default callback-depth budget.
    pub fn enter() -> Result<Self, CallbackDepthError> {
        Self::enter_with_limit(DEFAULT_CALLBACK_DEPTH_LIMIT)
    }

    /// Enter a caller-selected callback-depth budget.
    pub fn enter_with_limit(limit: u32) -> Result<Self, CallbackDepthError> {
        CALLBACK_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= limit {
                return Err(CallbackDepthError { limit });
            }
            let level = current + 1;
            depth.set(level);
            Ok(Self { level })
        })
    }

    /// The one-based depth of this callback.
    #[must_use]
    pub const fn level(&self) -> u32 {
        self.level
    }
}

impl Drop for CallbackDepthGuard {
    fn drop(&mut self) {
        CALLBACK_DEPTH.with(|depth| {
            debug_assert_eq!(depth.get(), self.level);
            depth.set(self.level - 1);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_budget_rejects_overflow_and_restores_after_unwind() {
        let first = CallbackDepthGuard::enter().expect("first depth is allowed");
        let second = CallbackDepthGuard::enter().expect("second depth is allowed");
        let third = CallbackDepthGuard::enter().expect("third depth is allowed");
        let fourth = CallbackDepthGuard::enter().expect("limit depth is allowed");
        let error = CallbackDepthGuard::enter().expect_err("the next depth must be rejected");
        assert_eq!(error.limit(), DEFAULT_CALLBACK_DEPTH_LIMIT);
        assert_eq!(error.to_string(), "reentrant callback depth limit 4 exceeded");

        drop(fourth);
        drop(third);
        drop(second);
        drop(first);
        let after_unwind = CallbackDepthGuard::enter().expect("the guard restores the budget");
        assert_eq!(after_unwind.level(), 1);
    }

    #[test]
    fn zero_budget_rejects_without_poisoning_the_thread() {
        let error = CallbackDepthGuard::enter_with_limit(0).expect_err("zero is fail-closed");
        assert_eq!(error.limit(), 0);
        let allowed = CallbackDepthGuard::enter_with_limit(1).expect("a later budget still works");
        assert_eq!(allowed.level(), 1);
    }
}
