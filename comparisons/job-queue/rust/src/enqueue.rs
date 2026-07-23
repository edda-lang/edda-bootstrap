//! Builder for [`crate::Queue::enqueue`].
//!
//! Rust has no keyword arguments, so optional enqueue parameters live
//! on a dedicated request struct. The v1 surface `enqueue(payload)`
//! continues to work via `impl From<Payload> for EnqueueRequest` — the
//! conversion is free at the call site and back-compat is total.

use crate::clock::Instant;
use crate::queue::{JobId, Payload};

/// Optional parameters for `Queue::enqueue`.
///
/// Construct with [`EnqueueRequest::new`], chain any of the builder
/// methods, then pass to `enqueue`. Validation runs inside the queue
/// under the lock, so the builder itself is infallible.
#[derive(Debug, Clone)]
pub struct EnqueueRequest {
    pub payload: Payload,
    pub priority: Option<u8>,
    pub scheduled_at: Option<Instant>,
    // v2.0 additions
    pub depends_on: Vec<JobId>,
    pub required_capabilities: Vec<String>,
    pub namespace: Option<String>,
}

impl EnqueueRequest {
    /// Start a request with `payload` and all-defaults.
    pub fn new(payload: Payload) -> Self {
        Self {
            payload,
            priority: None,
            scheduled_at: None,
            depends_on: Vec::new(),
            required_capabilities: Vec::new(),
            namespace: None,
        }
    }

    /// Set the priority. Validation runs inside `Queue::enqueue` and
    /// may produce `EnqueueError::InvalidPriority`.
    pub fn priority(mut self, value: u8) -> Self {
        self.priority = Some(value);
        self
    }

    /// Set the absolute time at which the job becomes acquirable. The
    /// job is enqueued immediately and occupies an `active_capacity`
    /// slot, but `acquire` will skip it until the queue's clock reads
    /// at or after `when`.
    pub fn scheduled_at(mut self, when: Instant) -> Self {
        self.scheduled_at = Some(when);
        self
    }

    /// Set the dependency list. The job becomes acquirable only after
    /// every id listed has reached `Completed`. References to unknown
    /// ids, duplicates, or ids in a different namespace cause
    /// `EnqueueError::InvalidDependency`.
    pub fn depends_on(mut self, ids: Vec<JobId>) -> Self {
        self.depends_on = ids;
        self
    }

    /// Set the capabilities a worker must hold to acquire this job.
    /// A worker's registered capability set must be a superset of
    /// this list.
    pub fn required_capabilities(mut self, caps: Vec<String>) -> Self {
        self.required_capabilities = caps;
        self
    }

    /// Set the target namespace. If omitted, the job lands in
    /// `"default"` (the v1.x landing zone).
    pub fn namespace(mut self, name: impl Into<String>) -> Self {
        self.namespace = Some(name.into());
        self
    }
}

impl From<Payload> for EnqueueRequest {
    fn from(payload: Payload) -> Self {
        Self::new(payload)
    }
}
