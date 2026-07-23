//! Error types for the queue operations.
//!
//! Each operation has a tight, op-specific error enum that mirrors the
//! spec. IO failures from the persistence layer are flattened into each
//! enum as a transparent `Io` variant; callers that want to be
//! exhaustive can match the logical variants and fall through to IO.

use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EnqueueError {
    /// Active capacity for the target namespace is exhausted.
    #[error("active queue is at capacity")]
    QueueFull,

    /// Priority value supplied to `enqueue` is outside `[1, 10]`.
    #[error("priority {value} is out of range [1, 10]")]
    InvalidPriority { value: u8 },

    /// `depends_on` references an unknown id, contains a cycle, or
    /// crosses namespace boundaries.
    #[error("invalid dependency: {reason}")]
    InvalidDependency { reason: DependencyError },

    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum HeartbeatError {
    /// Worker is not the current lease holder for this job, OR the
    /// job no longer exists, OR the job is in a terminal state.
    #[error("worker does not hold this lease")]
    NotLeaseHolder,

    /// The lease expired before this heartbeat arrived. Distinct from
    /// `NotLeaseHolder` only when the original holder is calling.
    #[error("lease has already expired")]
    LeaseExpired,

    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum AckError {
    /// Worker is not the current lease holder, or the job is in a
    /// terminal state. `complete`/`fail` both use this error.
    #[error("worker does not hold this lease")]
    NotLeaseHolder,

    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum AcquireError {
    /// `Config::require_worker_registration` is true and the worker
    /// is not in the registry, OR the candidate job has non-empty
    /// `required_capabilities` and the worker is not registered.
    #[error("worker {worker_id:?} is not registered")]
    UnknownWorker { worker_id: String },

    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum PromoteError {
    /// The job is not in the `Pending` state — only pending jobs may
    /// be promoted.
    #[error("job {job_id} is not pending")]
    NotPending { job_id: u64 },

    /// The supplied priority is not strictly greater than the job's
    /// current priority.
    #[error("priority {new} is not greater than current priority {current}")]
    PriorityNotIncreased { current: u8, new: u8 },

    /// Supplied priority is outside `[1, 10]`.
    #[error("priority {value} is out of range [1, 10]")]
    InvalidPriority { value: u8 },

    /// `job_id` does not match any job in the queue.
    #[error("unknown job {job_id}")]
    UnknownJob { job_id: u64 },

    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum AuditError {
    /// The watermark passed to `audit_since` precedes the oldest
    /// retained event. Includes the oldest still-retained event_id
    /// so the caller can resume from there.
    #[error("audit event watermark dropped; oldest retained event_id = {oldest_retained}")]
    AuditEventDropped { oldest_retained: u64 },
}

/// Sub-classification carried by [`EnqueueError::InvalidDependency`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DependencyError {
    /// `depends_on` includes an id the queue has never issued or that
    /// was reaped (oldest dead-letter dropped due to capacity bound).
    #[error("dependency on unknown job {job_id}")]
    Unknown { job_id: u64 },

    /// `depends_on` would form a cycle once this job is admitted.
    #[error("dependency forms a cycle through job {job_id}")]
    Cycle { job_id: u64 },

    /// `depends_on` crosses namespace boundaries.
    #[error("dependency on job {job_id} (namespace {to:?}) crosses namespace boundary (this job in {from:?})")]
    CrossNamespace {
        job_id: u64,
        from: String,
        to: String,
    },

    /// `depends_on` lists the same id twice.
    #[error("dependency list contains duplicate id {job_id}")]
    Duplicate { job_id: u64 },
}

/// Returned by `cancel`: number of jobs transitioned to the cancelled
/// state by this single call (root + all transitive dependents).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelResult {
    pub count: usize,
}
