//! Persistent leased job queue.
//!
//! Implements spec v2.0 (job dependencies, worker registry, cancellation,
//! audit log, multi-tenant namespaces, priority promotion) on top of
//! v1.x's lease + retry + dead-letter + priority + scheduling + metrics.
//!
//! A single `parking_lot::Mutex` guards the in-memory state; every
//! mutating operation snapshots the full state to disk via an atomic
//! file rename before returning.

mod audit;
mod clock;
mod config;
mod enqueue;
mod error;
mod metrics;
mod priority;
mod queue;
mod state;
mod workers;

pub use audit::{AuditEvent, AuditEventKind};
pub use clock::{Clock, Instant, SystemClock, TestClock};
pub use config::{Config, NamespaceConfig, DEFAULT_NAMESPACE};
pub use enqueue::EnqueueRequest;
pub use error::{
    AckError, AcquireError, AuditError, CancelResult, DependencyError, EnqueueError,
    HeartbeatError, PromoteError,
};
pub use metrics::{Counters, MetricsSnapshot, NamespaceMetrics};
pub use priority::{InvalidPriority, Priority};
pub use queue::{
    AcquiredJob, Attempt, CancelledEntry, DeadLetterEntry, JobId, Payload, Queue, WorkerId,
};
pub use workers::WorkerView;
