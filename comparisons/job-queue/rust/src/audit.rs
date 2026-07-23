//! Append-only audit log of state transitions.
//!
//! Every public mutating operation pushes one or more events through
//! [`AuditLog::push`] inside the queue's critical section, so the
//! event ordering and atomicity match the underlying state changes.
//! The log is bounded by `Config::audit_retention`; once exceeded the
//! oldest event is dropped. Watermark-based readers (`audit_since`)
//! receive [`AuditError::AuditEventDropped`] when their watermark has
//! fallen off the front of the retention window.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::clock::Instant;
use crate::error::AuditError;
use crate::queue::{JobId, WorkerId};

/// One audit-log entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Queue-lifetime-unique, monotonically-increasing identifier.
    pub event_id: u64,
    /// Clock reading at the moment the event was emitted.
    pub at: Instant,
    pub kind: AuditEventKind,
    pub job_id: Option<JobId>,
    pub worker_id: Option<WorkerId>,
    /// Short, free-form context (reason strings, attempt numbers,
    /// cascade origin id, etc.). The exact format is event-kind
    /// dependent and intentionally loose — this is an audit log, not
    /// a wire protocol.
    pub payload: String,
}

/// Kind discriminator for [`AuditEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditEventKind {
    Enqueued,
    Acquired,
    HeartbeatExtended,
    Completed,
    Failed,
    LeaseExpired,
    RetryScheduled,
    DeadLettered,
    Cancelled,
    WorkerRegistered,
    WorkerDeregistered,
    Promoted,
}

/// Bounded append-only event log.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct AuditLog {
    /// Next id to assign to a pushed event. Persisted so event_ids are
    /// queue-lifetime monotonic across restarts.
    pub next_event_id: u64,
    pub events: VecDeque<AuditEvent>,
}

impl AuditLog {
    /// Append `event` and prune to `retention`. `event_id` is assigned
    /// automatically from `next_event_id`; the field on `event` is
    /// overwritten. Returns the assigned id.
    pub fn push_with(
        &mut self,
        retention: usize,
        at: Instant,
        kind: AuditEventKind,
        job_id: Option<JobId>,
        worker_id: Option<WorkerId>,
        payload: String,
    ) -> u64 {
        let event_id = self.next_event_id.checked_add(1).expect("event_id overflow");
        self.next_event_id = event_id;
        self.events.push_back(AuditEvent {
            event_id,
            at,
            kind,
            job_id,
            worker_id,
            payload,
        });
        while self.events.len() > retention {
            self.events.pop_front();
        }
        event_id
    }

    /// Every event with `event_id > after`, in event_id order. Returns
    /// `AuditEventDropped` if `after` precedes the oldest retained
    /// event_id (the gap means the caller would miss events).
    pub fn since(&self, after: u64) -> Result<Vec<AuditEvent>, AuditError> {
        if let Some(oldest) = self.events.front() {
            // The caller hasn't missed anything iff `after + 1 >=
            // oldest.event_id`, i.e. the next event they expect is
            // either the oldest retained or strictly newer.
            if after + 1 < oldest.event_id {
                return Err(AuditError::AuditEventDropped {
                    oldest_retained: oldest.event_id,
                });
            }
        }
        Ok(self
            .events
            .iter()
            .filter(|e| e.event_id > after)
            .cloned()
            .collect())
    }

    /// Most recent `limit` events, in event_id order. An empty log
    /// returns an empty vec; `limit == 0` likewise.
    pub fn recent(&self, limit: usize) -> Vec<AuditEvent> {
        let start = self.events.len().saturating_sub(limit);
        self.events.iter().skip(start).cloned().collect()
    }
}
