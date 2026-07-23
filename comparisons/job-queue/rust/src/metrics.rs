//! Per-queue and per-namespace counters and the snapshot types returned
//! by `Queue::metrics`.
//!
//! Counters live on the persisted state, so they survive process
//! restart and SIGKILL exactly like the job records they describe.
//! They are strictly monotonic for the lifetime of the queue —
//! dropping a dead-letter entry due to capacity does NOT roll back
//! `dead_lettered_total`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Monotonic event counters, maintained per namespace.
///
/// Every field is bumped at the transition it names, inside the same
/// critical section that effects the transition — so the counter and
/// the underlying state move atomically.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    pub enqueued_total: u64,
    pub acquired_total: u64,
    pub completed_total: u64,
    pub failed_total: u64,
    pub lease_expired_total: u64,
    pub dead_lettered_total: u64,
    pub retry_scheduled_total: u64,
    // v2.0 additions
    pub cancelled_total: u64,
    pub promoted_total: u64,
}

impl Counters {
    /// Field-wise add — used to roll up per-namespace counters into a
    /// queue-wide total for `MetricsSnapshot`.
    pub(crate) fn add(&mut self, other: &Counters) {
        self.enqueued_total += other.enqueued_total;
        self.acquired_total += other.acquired_total;
        self.completed_total += other.completed_total;
        self.failed_total += other.failed_total;
        self.lease_expired_total += other.lease_expired_total;
        self.dead_lettered_total += other.dead_lettered_total;
        self.retry_scheduled_total += other.retry_scheduled_total;
        self.cancelled_total += other.cancelled_total;
        self.promoted_total += other.promoted_total;
    }
}

/// Per-namespace counters + gauges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceMetrics {
    // Counters
    pub enqueued_total: u64,
    pub acquired_total: u64,
    pub completed_total: u64,
    pub failed_total: u64,
    pub lease_expired_total: u64,
    pub dead_lettered_total: u64,
    pub retry_scheduled_total: u64,
    pub cancelled_total: u64,
    pub promoted_total: u64,

    // Gauges
    pub active_count: usize,
    pub leased_count: usize,
    pub dead_letter_count: usize,
}

/// Queue-wide rollup plus a per-namespace breakdown.
///
/// All values are read from the same locked state, so the snapshot is
/// internally consistent (no torn reads, and the per-namespace fields
/// always sum to the queue-wide rollup).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsSnapshot {
    // Counters — sum across namespaces
    pub enqueued_total: u64,
    pub acquired_total: u64,
    pub completed_total: u64,
    pub failed_total: u64,
    pub lease_expired_total: u64,
    pub dead_lettered_total: u64,
    pub retry_scheduled_total: u64,
    pub cancelled_total: u64,
    pub promoted_total: u64,

    // Gauges — sum across namespaces
    pub active_count: usize,
    pub leased_count: usize,
    pub dead_letter_count: usize,

    /// Per-namespace breakdown. BTreeMap so iteration order is stable.
    pub by_namespace: BTreeMap<String, NamespaceMetrics>,
}
