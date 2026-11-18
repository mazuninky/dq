//! Store-wide resource limiter for plugin invocations.
//!
//! [`PluginLimiter`] implements [`wasmtime::ResourceLimiter`] with budgets
//! that apply to the whole `Store`, not to each linear memory or table on
//! its own. A component can carry several core modules, each with its own
//! memories and tables, so the caps below are sums over everything the
//! plugin allocates during one invocation.
//!
//! # Accounting
//!
//! Wasmtime reports every allocation through the limiter: creating a
//! memory at instantiation is `memory_growing(0, minimum, maximum)`, a
//! `memory.grow` is `memory_growing(current, desired, maximum)`, and tables
//! follow the same shape through `table_growing`. An approved request adds
//! `desired - current` to the running total before wasmtime performs the
//! growth.
//!
//! The accounting is fail-closed: nothing is ever returned to the budget.
//! When wasmtime fails a growth it had approved (for example because the
//! host could not allocate), `memory_grow_failed` / `table_grow_failed`
//! leave the reservation counted. The total can then only be higher than
//! what the plugin really holds, never lower, whatever order wasmtime
//! reports events in and whichever wasm features are enabled. WebAssembly
//! memories and tables do not shrink, and each invocation gets a fresh
//! store, so there is nothing else to return either.
//!
//! The limiter refuses any request above the memory's or table's own
//! declared maximum without reserving anything, the same check
//! [`wasmtime::StoreLimits`] makes. Wasmtime would fail such a growth
//! anyway; refusing it up front keeps the budget untouched.

// Reference point for the budgets below: the plugins built from
// `examples/plugin-rust/` with wit-bindgen 0.57 and 0.63 instantiate one
// core module with one linear memory (1.1 MiB at its peak during a call)
// and one table of 19 elements. Components that need extra core modules
// (adapter or shim modules from `wit-component`, or several guest modules
// composed together) add one core instance per module and usually one
// table, and at most one memory each.

/// Total bytes of linear memory a plugin may hold across all of its
/// memories in one invocation (64 MiB).
pub(crate) const MEMORY_LIMIT: usize = 64 * 1024 * 1024;

/// Total table elements a plugin may hold across all of its tables in one
/// invocation.
///
/// Wasmtime stores a table element in about one pointer, so the cap keeps
/// table storage under 1 MiB of host memory per invocation. A compiled
/// Rust guest has one table of indirect call targets; even large guests
/// stay within a few thousand entries. For scale, wasmtime's pooling
/// allocator defaults to 20 000 elements per table.
pub(crate) const TABLE_ELEMENTS_LIMIT: usize = 100_000;

/// Maximum core module instances per store. A wit-bindgen plugin needs
/// one; 32 leaves room for adapter modules and composed components.
pub(crate) const INSTANCE_LIMIT: usize = 32;

/// Maximum linear memories per store. A wit-bindgen plugin defines one;
/// a composed component defines at most one per guest module. The byte
/// budget is shared, so this count only bounds per-memory bookkeeping.
pub(crate) const MEMORIES_LIMIT: usize = 16;

/// Maximum tables per store: one per core instance, matching
/// [`INSTANCE_LIMIT`].
pub(crate) const TABLES_LIMIT: usize = 32;

/// Store-wide [`wasmtime::ResourceLimiter`] for one plugin invocation.
#[derive(Debug, Clone)]
pub(crate) struct PluginLimiter {
    memory_limit: usize,
    table_elements_limit: usize,
    /// Bytes of linear memory reserved across every memory of the store.
    memory_used: usize,
    /// Table elements reserved across every table of the store.
    table_elements_used: usize,
    /// Set once a memory request is refused because of the store budget.
    memory_limit_hit: bool,
}

impl PluginLimiter {
    /// Limiter with a store-wide budget of `memory_limit` bytes of linear
    /// memory and `table_elements_limit` table elements. The runtime passes
    /// [`MEMORY_LIMIT`] and [`TABLE_ELEMENTS_LIMIT`]. The instance, memory
    /// and table counts are fixed at [`INSTANCE_LIMIT`], [`MEMORIES_LIMIT`]
    /// and [`TABLES_LIMIT`].
    pub(crate) fn new(memory_limit: usize, table_elements_limit: usize) -> Self {
        Self {
            memory_limit,
            table_elements_limit,
            memory_used: 0,
            table_elements_used: 0,
            memory_limit_hit: false,
        }
    }

    /// Whether a memory request was refused because the store-wide byte
    /// budget would have been exceeded. The runtime uses this to classify
    /// the plugin's failure as [`crate::PluginError::Memory`].
    pub(crate) fn memory_limit_hit(&self) -> bool {
        self.memory_limit_hit
    }
}

/// Outcome of one budget check.
enum Grant {
    /// Approved; carries the amount reserved.
    Reserved(usize),
    /// Refused because the request exceeds the item's own declared maximum.
    OverItemMaximum,
    /// Refused because the store-wide budget would be exceeded.
    OverBudget,
}

/// Reserve `desired - current` from a budget of `limit` with `used`
/// already taken. Never overflows: the sum is checked.
fn reserve(
    used: usize,
    limit: usize,
    current: usize,
    desired: usize,
    maximum: Option<usize>,
) -> Grant {
    if maximum.is_some_and(|max| desired > max) {
        return Grant::OverItemMaximum;
    }
    let delta = desired.saturating_sub(current);
    match used.checked_add(delta) {
        Some(total) if total <= limit => Grant::Reserved(delta),
        _ => Grant::OverBudget,
    }
}

impl wasmtime::ResourceLimiter for PluginLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        match reserve(
            self.memory_used,
            self.memory_limit,
            current,
            desired,
            maximum,
        ) {
            Grant::Reserved(delta) => {
                self.memory_used += delta;
                Ok(true)
            }
            Grant::OverItemMaximum => Ok(false),
            Grant::OverBudget => {
                self.memory_limit_hit = true;
                Ok(false)
            }
        }
    }

    fn memory_grow_failed(&mut self, _error: wasmtime::Error) -> wasmtime::Result<()> {
        // Fail-closed: the reservation stays counted (see the module doc).
        Ok(())
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        match reserve(
            self.table_elements_used,
            self.table_elements_limit,
            current,
            desired,
            maximum,
        ) {
            Grant::Reserved(delta) => {
                self.table_elements_used += delta;
                Ok(true)
            }
            Grant::OverItemMaximum | Grant::OverBudget => Ok(false),
        }
    }

    fn table_grow_failed(&mut self, _error: wasmtime::Error) -> wasmtime::Result<()> {
        // Fail-closed: the reservation stays counted (see the module doc).
        Ok(())
    }

    fn instances(&self) -> usize {
        INSTANCE_LIMIT
    }

    fn tables(&self) -> usize {
        TABLES_LIMIT
    }

    fn memories(&self) -> usize {
        MEMORIES_LIMIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::ResourceLimiter;

    const MIB: usize = 1024 * 1024;

    fn failure() -> wasmtime::Error {
        wasmtime::Error::msg("allocation failed")
    }

    #[test]
    fn memories_share_one_budget() {
        let mut limiter = PluginLimiter::new(64 * MIB, 10);
        // Two memories created at instantiation: `current` is 0 for each.
        assert!(limiter.memory_growing(0, 32 * MIB, None).unwrap());
        assert!(limiter.memory_growing(0, 16 * MIB, None).unwrap());
        // The first memory grows to 48 MiB: 48 + 16 = 64, exactly the cap.
        assert!(limiter.memory_growing(32 * MIB, 48 * MIB, None).unwrap());
        assert_eq!(limiter.memory_used, 64 * MIB);
        assert!(!limiter.memory_limit_hit());
        // Any further page, in any memory, is refused.
        assert!(
            !limiter
                .memory_growing(16 * MIB, 16 * MIB + 65_536, None)
                .unwrap()
        );
        assert!(limiter.memory_limit_hit());
        assert_eq!(limiter.memory_used, 64 * MIB);
    }

    #[test]
    fn memory_grow_failed_keeps_the_reservation() {
        let mut limiter = PluginLimiter::new(64 * MIB, 10);
        assert!(limiter.memory_growing(0, 32 * MIB, None).unwrap());
        assert!(limiter.memory_growing(32 * MIB, 48 * MIB, None).unwrap());
        // Wasmtime reports that the approved growth failed: the bytes stay
        // counted, however many times the failure is reported.
        limiter.memory_grow_failed(failure()).unwrap();
        limiter.memory_grow_failed(failure()).unwrap();
        assert_eq!(limiter.memory_used, 48 * MIB);
        assert!(!limiter.memory_growing(0, 32 * MIB, None).unwrap());
        assert!(limiter.memory_limit_hit());
    }

    #[test]
    fn interleaved_failures_never_lower_the_memory_total() {
        let mut limiter = PluginLimiter::new(64 * MIB, 10);
        let mut current = 0;
        for _ in 0..64 {
            // A successful one-MiB growth, then a failure report that is
            // not preceded by a request of its own.
            if limiter
                .memory_growing(current, current + MIB, None)
                .unwrap()
            {
                current += MIB;
            }
            limiter.memory_grow_failed(failure()).unwrap();
        }
        assert_eq!(limiter.memory_used, 64 * MIB);
        assert!(
            !limiter
                .memory_growing(current, current + MIB, None)
                .unwrap()
        );
    }

    #[test]
    fn growth_past_the_declared_maximum_is_refused_without_reserving() {
        let mut limiter = PluginLimiter::new(64 * MIB, 10);
        assert!(!limiter.memory_growing(0, 2 * MIB, Some(MIB)).unwrap());
        assert_eq!(limiter.memory_used, 0);
        // The memory's own maximum is not the store budget.
        assert!(!limiter.memory_limit_hit());
    }

    #[test]
    fn huge_requests_do_not_overflow_the_total() {
        let mut limiter = PluginLimiter::new(usize::MAX, 10);
        assert!(limiter.memory_growing(0, usize::MAX - 1, None).unwrap());
        assert!(!limiter.memory_growing(0, 2, None).unwrap());
        assert!(limiter.memory_limit_hit());
        assert_eq!(limiter.memory_used, usize::MAX - 1);
    }

    #[test]
    fn tables_share_one_element_budget() {
        let mut limiter = PluginLimiter::new(64 * MIB, 100);
        assert!(limiter.table_growing(0, 60, None).unwrap());
        assert!(limiter.table_growing(0, 40, None).unwrap());
        assert!(!limiter.table_growing(40, 41, None).unwrap());
        assert_eq!(limiter.table_elements_used, 100);
        // Table refusals are not memory refusals.
        assert!(!limiter.memory_limit_hit());
    }

    #[test]
    fn interleaved_table_failures_never_lower_the_element_total() {
        let mut limiter = PluginLimiter::new(64 * MIB, 100);
        let mut current = 0;
        for _ in 0..200 {
            if limiter.table_growing(current, current + 1, None).unwrap() {
                current += 1;
            }
            limiter.table_grow_failed(failure()).unwrap();
        }
        assert_eq!(limiter.table_elements_used, 100);
        assert!(!limiter.table_growing(current, current + 1, None).unwrap());
        // A request past the table's own maximum reserves nothing.
        let mut limiter = PluginLimiter::new(64 * MIB, 100);
        assert!(!limiter.table_growing(0, 20, Some(15)).unwrap());
        assert_eq!(limiter.table_elements_used, 0);
    }
}
