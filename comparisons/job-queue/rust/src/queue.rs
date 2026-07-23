//! Public queue API.
//!
//! Every mutating operation takes the queue's single
//! `parking_lot::Mutex`, runs `State::tick` (promotes due retries /
//! scheduled jobs and expires stale leases), applies the change, emits
//! the relevant audit events, then snapshots state to disk before
//! returning. The snapshot-per-mutation policy makes the queue
//! SIGKILL-safe at the cost of one fsync per op.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::audit::{AuditEvent, AuditEventKind};
use crate::clock::{Clock, Instant};
use crate::config::Config;
use crate::enqueue::EnqueueRequest;
use crate::error::{
    AckError, AcquireError, AuditError, CancelResult, DependencyError, EnqueueError,
    HeartbeatError, PromoteError,
};
use crate::metrics::{Counters, MetricsSnapshot, NamespaceMetrics};
use crate::priority::Priority;
use crate::state::{self, Job, JobState, State};
use crate::workers::WorkerView;

pub type JobId = u64;
pub type WorkerId = String;
pub type Payload = Vec<u8>;
pub type Attempt = u32;

/// Result of a successful `acquire`.
#[derive(Debug, Clone)]
pub struct AcquiredJob {
    pub id: JobId,
    pub payload: Payload,
    /// 1-based attempt number being initiated.
    pub attempt: Attempt,
    pub priority: Priority,
    pub scheduled_at: Option<Instant>,
    /// Namespace this job belongs to. v1.x callers see `"default"`.
    pub namespace: String,
    /// Capabilities required to acquire this job. Empty = any worker.
    pub required_capabilities: Vec<String>,
}

/// One entry from the dead-letter store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadLetterEntry {
    pub id: JobId,
    pub payload: Payload,
    pub final_reason: String,
    pub namespace: String,
}

/// One entry from the cancelled store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelledEntry {
    pub id: JobId,
    pub payload: Payload,
    pub reason: String,
    pub namespace: String,
    pub cancelled_at: Instant,
}

/// Persistent leased job queue.
pub struct Queue {
    inner: Mutex<Inner>,
    config: Config,
    clock: Arc<dyn Clock>,
}

struct Inner {
    state: State,
    path: PathBuf,
}

impl Queue {
    /// Open (or create) a queue backed by `path`. If the file exists,
    /// the contents are loaded and the queue resumes where the previous
    /// process left off.
    pub fn open<P: AsRef<Path>>(
        path: P,
        config: Config,
        clock: Arc<dyn Clock>,
    ) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let state = state::load(&path)?;
        Ok(Self {
            inner: Mutex::new(Inner { state, path }),
            config,
            clock,
        })
    }

    // ================================================================
    //                      Lifecycle operations
    // ================================================================

    /// Enqueue a job. Accepts a `Vec<u8>` directly (defaults) or an
    /// [`EnqueueRequest`] for priority, scheduling, dependencies,
    /// capabilities, and namespace.
    pub fn enqueue<R: Into<EnqueueRequest>>(&self, request: R) -> Result<JobId, EnqueueError> {
        let request = request.into();
        let priority = match request.priority {
            Some(v) => Priority::new(v)
                .map_err(|_| EnqueueError::InvalidPriority { value: v })?,
            None => Priority::DEFAULT,
        };
        let namespace = request
            .namespace
            .clone()
            .unwrap_or_else(|| State::default_namespace().to_string());
        let ns_cfg = self.config.namespace_config(&namespace);

        let mut inner = self.inner.lock();
        let now = self.clock.now();
        inner.state.tick(now);
        inner.state.namespace_mut(&namespace, ns_cfg.clone());

        let cap = inner
            .state
            .namespaces
            .get(&namespace)
            .expect("namespace just ensured")
            .config
            .active_capacity;
        if inner.state.active_count_ns(&namespace) >= cap {
            return Err(EnqueueError::QueueFull);
        }

        validate_dependencies(&request.depends_on, &namespace, &inner.state)
            .map_err(|reason| EnqueueError::InvalidDependency { reason })?;

        let id = inner.state.next_id;
        inner.state.next_id = id.checked_add(1).expect("job id overflow");
        let seq = inner.state.next_seq;
        inner.state.next_seq = seq.checked_add(1).expect("seq overflow");

        let mut remaining_deps = 0u32;
        let mut cascade_reason: Option<String> = None;
        for &dep_id in &request.depends_on {
            let dep_state = inner
                .state
                .jobs
                .get(&dep_id)
                .map(|j| j.state.clone())
                .expect("validated above");
            match dep_state {
                JobState::Completed { .. } => {}
                JobState::Cancelled { .. } => {
                    cascade_reason = Some(format!("dependency {dep_id} was cancelled"));
                }
                JobState::DeadLetter { .. } => {
                    cascade_reason = Some(format!("dependency {dep_id} was dead-lettered"));
                }
                _ => {
                    remaining_deps = remaining_deps.checked_add(1).unwrap();
                    inner
                        .state
                        .jobs
                        .get_mut(&dep_id)
                        .expect("validated above")
                        .dependents
                        .push(id);
                }
            }
        }

        let initial_state = match request.scheduled_at {
            Some(when) if when > now => JobState::Scheduled { available_at: when },
            _ => JobState::Pending { last_holder: None },
        };

        let job = Job {
            id,
            payload: request.payload,
            priority,
            failures: 0,
            seq,
            scheduled_at: request.scheduled_at,
            namespace: namespace.clone(),
            depends_on: request.depends_on.clone(),
            dependents: Vec::new(),
            remaining_deps,
            required_capabilities: request.required_capabilities.clone(),
            state: initial_state,
        };
        inner.state.jobs.insert(id, job);

        inner
            .state
            .namespaces
            .get_mut(&namespace)
            .expect("namespace exists")
            .counters
            .enqueued_total += 1;
        let payload_desc = format!("ns={namespace} priority={}", priority.value());
        inner.state.audit.push_with(
            self.config.audit_retention,
            now,
            AuditEventKind::Enqueued,
            Some(id),
            None,
            payload_desc,
        );

        if let Some(reason) = cascade_reason {
            cascade_cancel(&mut inner.state, id, reason, now, self.config.audit_retention);
        }

        state::save(&inner.path, &inner.state)?;
        Ok(id)
    }

    /// Atomically claim the highest-priority available job.
    pub fn acquire(
        &self,
        worker_id: &str,
        lease_duration: Duration,
    ) -> Result<Option<AcquiredJob>, AcquireError> {
        let mut inner = self.inner.lock();
        let now = self.clock.now();
        inner.state.tick(now);

        if self.config.require_worker_registration
            && !inner.state.workers.contains(worker_id)
        {
            return Err(AcquireError::UnknownWorker {
                worker_id: worker_id.to_string(),
            });
        }

        let worker_caps = resolve_worker_caps(&inner.state, worker_id);
        let candidate_id = inner.state.next_acquirable(&worker_caps);

        if candidate_id.is_none()
            && !inner.state.workers.contains(worker_id)
            && any_acquirable_requires_caps(&inner.state)
        {
            return Err(AcquireError::UnknownWorker {
                worker_id: worker_id.to_string(),
            });
        }

        let Some(id) = candidate_id else {
            return Ok(None);
        };

        let expires_at = now + lease_duration;
        let job = inner
            .state
            .jobs
            .get_mut(&id)
            .expect("next_acquirable invariant");
        job.state = JobState::Leased {
            worker: worker_id.to_string(),
            expires_at,
        };
        let acquired = AcquiredJob {
            id,
            payload: job.payload.clone(),
            attempt: job.failures + 1,
            priority: job.priority,
            scheduled_at: job.scheduled_at,
            namespace: job.namespace.clone(),
            required_capabilities: job.required_capabilities.clone(),
        };
        let namespace = job.namespace.clone();

        inner
            .state
            .namespaces
            .get_mut(&namespace)
            .expect("namespace exists")
            .counters
            .acquired_total += 1;
        inner.state.audit.push_with(
            self.config.audit_retention,
            now,
            AuditEventKind::Acquired,
            Some(id),
            Some(worker_id.to_string()),
            format!("attempt={}", acquired.attempt),
        );

        state::save(&inner.path, &inner.state)?;
        Ok(Some(acquired))
    }

    /// Extend the lease for `job_id` by `lease_extension` from now.
    pub fn heartbeat(
        &self,
        worker_id: &str,
        job_id: JobId,
        lease_extension: Duration,
    ) -> Result<(), HeartbeatError> {
        let mut inner = self.inner.lock();
        let now = self.clock.now();
        inner.state.tick(now);

        let job = match inner.state.jobs.get_mut(&job_id) {
            Some(j) => j,
            None => return Err(HeartbeatError::NotLeaseHolder),
        };

        match &job.state {
            JobState::Leased { worker, .. } if worker == worker_id => {
                let new_expiry = now + lease_extension;
                job.state = JobState::Leased {
                    worker: worker_id.to_string(),
                    expires_at: new_expiry,
                };
            }
            JobState::Leased { .. } => return Err(HeartbeatError::NotLeaseHolder),
            JobState::Pending {
                last_holder: Some(holder),
            } if holder == worker_id => return Err(HeartbeatError::LeaseExpired),
            _ => return Err(HeartbeatError::NotLeaseHolder),
        }

        inner.state.audit.push_with(
            self.config.audit_retention,
            now,
            AuditEventKind::HeartbeatExtended,
            Some(job_id),
            Some(worker_id.to_string()),
            String::new(),
        );
        state::save(&inner.path, &inner.state)?;
        Ok(())
    }

    /// Mark the job complete. Decrements dependents' remaining_deps.
    pub fn complete(&self, worker_id: &str, job_id: JobId) -> Result<(), AckError> {
        let mut inner = self.inner.lock();
        let now = self.clock.now();
        inner.state.tick(now);

        if !is_lease_holder(&inner.state, worker_id, job_id) {
            return Err(AckError::NotLeaseHolder);
        }

        let (namespace, dependents) = {
            let job = inner.state.jobs.get_mut(&job_id).expect("checked above");
            job.state = JobState::Completed { completed_at: now };
            (job.namespace.clone(), job.dependents.clone())
        };
        for child_id in dependents {
            if let Some(child) = inner.state.jobs.get_mut(&child_id) {
                child.remaining_deps = child.remaining_deps.saturating_sub(1);
            }
        }

        inner
            .state
            .namespaces
            .get_mut(&namespace)
            .expect("namespace exists")
            .counters
            .completed_total += 1;
        inner.state.audit.push_with(
            self.config.audit_retention,
            now,
            AuditEventKind::Completed,
            Some(job_id),
            Some(worker_id.to_string()),
            String::new(),
        );
        state::save(&inner.path, &inner.state)?;
        Ok(())
    }

    /// Mark an explicit failure. Schedules a retry with backoff or
    /// moves the job to dead-letter (and cascade-cancels its
    /// dependents) once the namespace's max_attempts is reached.
    pub fn fail(
        &self,
        worker_id: &str,
        job_id: JobId,
        reason: String,
    ) -> Result<(), AckError> {
        let mut inner = self.inner.lock();
        let now = self.clock.now();
        inner.state.tick(now);

        if !is_lease_holder(&inner.state, worker_id, job_id) {
            return Err(AckError::NotLeaseHolder);
        }

        let namespace = inner.state.jobs[&job_id].namespace.clone();
        let ns_max_attempts = inner
            .state
            .namespaces
            .get(&namespace)
            .expect("namespace exists")
            .config
            .max_attempts;
        let attempt_before = inner.state.jobs[&job_id].failures + 1;
        let delay = compute_backoff(
            self.config.backoff_base,
            self.config.backoff_cap,
            self.config.jitter_fraction,
            attempt_before,
        );

        let job = inner.state.jobs.get_mut(&job_id).expect("checked above");
        job.failures += 1;
        let failures = job.failures;
        let scheduled_at = job.scheduled_at;

        inner
            .state
            .namespaces
            .get_mut(&namespace)
            .expect("namespace exists")
            .counters
            .failed_total += 1;
        inner.state.audit.push_with(
            self.config.audit_retention,
            now,
            AuditEventKind::Failed,
            Some(job_id),
            Some(worker_id.to_string()),
            format!("attempt={attempt_before} reason={reason}"),
        );

        if failures >= ns_max_attempts {
            terminal_dead_letter(
                &mut inner.state,
                job_id,
                reason,
                now,
                self.config.audit_retention,
            );
            let dependents = inner
                .state
                .jobs
                .get(&job_id)
                .map(|j| j.dependents.clone())
                .unwrap_or_default();
            for child in dependents {
                cascade_cancel(
                    &mut inner.state,
                    child,
                    format!("parent {job_id} dead-lettered"),
                    now,
                    self.config.audit_retention,
                );
            }
        } else {
            let retry_ready_at = now + delay;
            let ready_at = match scheduled_at {
                Some(t) => retry_ready_at.max(t),
                None => retry_ready_at,
            };
            inner
                .state
                .jobs
                .get_mut(&job_id)
                .expect("still present")
                .state = JobState::RetryPending { ready_at };
            inner
                .state
                .namespaces
                .get_mut(&namespace)
                .expect("namespace exists")
                .counters
                .retry_scheduled_total += 1;
            inner.state.audit.push_with(
                self.config.audit_retention,
                now,
                AuditEventKind::RetryScheduled,
                Some(job_id),
                None,
                format!("ready_at_nanos={}", ready_at.as_nanos()),
            );
        }

        state::save(&inner.path, &inner.state)?;
        Ok(())
    }

    /// Cancel `job_id` and every transitive dependent. Idempotent —
    /// cancelling a job already in a terminal state returns
    /// `CancelResult { count: 0 }` and does NOT cascade.
    pub fn cancel(&self, job_id: JobId, reason: String) -> std::io::Result<CancelResult> {
        let mut inner = self.inner.lock();
        let now = self.clock.now();
        inner.state.tick(now);
        let count = cascade_cancel(
            &mut inner.state,
            job_id,
            reason,
            now,
            self.config.audit_retention,
        );
        state::save(&inner.path, &inner.state)?;
        Ok(CancelResult { count })
    }

    /// Raise a pending (or scheduled-not-yet-due) job's priority.
    pub fn promote(&self, job_id: JobId, new_priority: u8) -> Result<(), PromoteError> {
        let new_prio = Priority::new(new_priority)
            .map_err(|_| PromoteError::InvalidPriority { value: new_priority })?;

        let mut inner = self.inner.lock();
        let now = self.clock.now();
        inner.state.tick(now);

        let job = inner
            .state
            .jobs
            .get_mut(&job_id)
            .ok_or(PromoteError::UnknownJob { job_id })?;

        if !matches!(
            job.state,
            JobState::Pending { .. } | JobState::Scheduled { .. }
        ) {
            return Err(PromoteError::NotPending { job_id });
        }

        let current = job.priority;
        if new_prio <= current {
            return Err(PromoteError::PriorityNotIncreased {
                current: current.value(),
                new: new_prio.value(),
            });
        }

        job.priority = new_prio;
        let namespace = job.namespace.clone();

        inner
            .state
            .namespaces
            .get_mut(&namespace)
            .expect("namespace exists")
            .counters
            .promoted_total += 1;
        inner.state.audit.push_with(
            self.config.audit_retention,
            now,
            AuditEventKind::Promoted,
            Some(job_id),
            None,
            format!("{} -> {}", current.value(), new_prio.value()),
        );

        state::save(&inner.path, &inner.state)?;
        Ok(())
    }

    // ================================================================
    //                      Admin operations
    // ================================================================

    /// Register (or re-register) a worker with the given capability
    /// set. Idempotent — repeated calls overwrite the capability set.
    pub fn register_worker(
        &self,
        worker_id: &str,
        capabilities: Vec<String>,
    ) -> std::io::Result<()> {
        let mut inner = self.inner.lock();
        let now = self.clock.now();
        inner.state.tick(now);
        inner.state.workers.register(worker_id, capabilities.clone());
        inner.state.audit.push_with(
            self.config.audit_retention,
            now,
            AuditEventKind::WorkerRegistered,
            None,
            Some(worker_id.to_string()),
            format!("caps={}", capabilities.join(",")),
        );
        state::save(&inner.path, &inner.state)?;
        Ok(())
    }

    /// Deregister a worker, force-expiring every lease it holds (no
    /// attempt-counter increment). Returns the count of leases
    /// force-expired. Deregistering an unknown worker is a no-op.
    pub fn deregister_worker(&self, worker_id: &str) -> std::io::Result<usize> {
        let mut inner = self.inner.lock();
        let now = self.clock.now();
        inner.state.tick(now);

        let held: Vec<(JobId, String)> = inner
            .state
            .jobs
            .values()
            .filter_map(|j| match &j.state {
                JobState::Leased { worker, .. } if worker == worker_id => {
                    Some((j.id, j.namespace.clone()))
                }
                _ => None,
            })
            .collect();

        for (id, namespace) in &held {
            if let Some(job) = inner.state.jobs.get_mut(id) {
                job.state = JobState::Pending {
                    last_holder: Some(worker_id.to_string()),
                };
            }
            if let Some(ns) = inner.state.namespaces.get_mut(namespace) {
                ns.counters.lease_expired_total += 1;
            }
            inner.state.audit.push_with(
                self.config.audit_retention,
                now,
                AuditEventKind::LeaseExpired,
                Some(*id),
                Some(worker_id.to_string()),
                String::from("worker deregistered"),
            );
        }

        let removed = inner.state.workers.deregister(worker_id).is_some();
        if removed {
            inner.state.audit.push_with(
                self.config.audit_retention,
                now,
                AuditEventKind::WorkerDeregistered,
                None,
                Some(worker_id.to_string()),
                String::new(),
            );
        }

        state::save(&inner.path, &inner.state)?;
        Ok(held.len())
    }

    pub fn list_workers(&self) -> Vec<WorkerView> {
        let inner = self.inner.lock();
        inner.state.workers.list()
    }

    /// Create (or overwrite) the configuration for a namespace.
    pub fn register_namespace(
        &self,
        name: &str,
        config: crate::config::NamespaceConfig,
    ) -> std::io::Result<()> {
        let mut inner = self.inner.lock();
        let ns = inner.state.namespace_mut(name, config.clone());
        ns.config = config;
        state::save(&inner.path, &inner.state)?;
        Ok(())
    }

    pub fn list_namespaces(&self) -> Vec<String> {
        let inner = self.inner.lock();
        inner.state.namespaces.keys().cloned().collect()
    }

    // ================================================================
    //                      Read-only / observability
    // ================================================================

    pub fn dead_letter_iter(&self) -> Vec<DeadLetterEntry> {
        let inner = self.inner.lock();
        inner.state.dead_letter_entries()
    }

    pub fn cancelled_iter(&self) -> Vec<CancelledEntry> {
        let inner = self.inner.lock();
        inner.state.cancelled_entries()
    }

    pub fn audit_since(&self, after: u64) -> Result<Vec<AuditEvent>, AuditError> {
        let inner = self.inner.lock();
        inner.state.audit.since(after)
    }

    pub fn audit_recent(&self, limit: usize) -> Vec<AuditEvent> {
        let inner = self.inner.lock();
        inner.state.audit.recent(limit)
    }

    pub fn metrics(&self) -> MetricsSnapshot {
        let mut inner = self.inner.lock();
        let now = self.clock.now();
        inner.state.tick(now);

        let mut rollup = Counters::default();
        let mut by_namespace = std::collections::BTreeMap::new();
        let mut total_active = 0usize;
        let mut total_leased = 0usize;
        let mut total_dead = 0usize;

        for (name, ns) in &inner.state.namespaces {
            rollup.add(&ns.counters);
            let active = inner.state.active_count_ns(name);
            let leased = inner.state.leased_count_ns(name);
            let dead = inner.state.dead_letter_count_ns(name);
            total_active += active;
            total_leased += leased;
            total_dead += dead;
            by_namespace.insert(
                name.clone(),
                NamespaceMetrics {
                    enqueued_total: ns.counters.enqueued_total,
                    acquired_total: ns.counters.acquired_total,
                    completed_total: ns.counters.completed_total,
                    failed_total: ns.counters.failed_total,
                    lease_expired_total: ns.counters.lease_expired_total,
                    dead_lettered_total: ns.counters.dead_lettered_total,
                    retry_scheduled_total: ns.counters.retry_scheduled_total,
                    cancelled_total: ns.counters.cancelled_total,
                    promoted_total: ns.counters.promoted_total,
                    active_count: active,
                    leased_count: leased,
                    dead_letter_count: dead,
                },
            );
        }

        MetricsSnapshot {
            enqueued_total: rollup.enqueued_total,
            acquired_total: rollup.acquired_total,
            completed_total: rollup.completed_total,
            failed_total: rollup.failed_total,
            lease_expired_total: rollup.lease_expired_total,
            dead_lettered_total: rollup.dead_lettered_total,
            retry_scheduled_total: rollup.retry_scheduled_total,
            cancelled_total: rollup.cancelled_total,
            promoted_total: rollup.promoted_total,
            active_count: total_active,
            leased_count: total_leased,
            dead_letter_count: total_dead,
            by_namespace,
        }
    }

    pub fn active_count(&self) -> usize {
        let mut inner = self.inner.lock();
        let now = self.clock.now();
        inner.state.tick(now);
        inner.state.active_count()
    }

    pub fn leased_count(&self) -> usize {
        let mut inner = self.inner.lock();
        let now = self.clock.now();
        inner.state.tick(now);
        inner.state.leased_count()
    }

    pub fn dead_letter_count(&self) -> usize {
        let inner = self.inner.lock();
        inner.state.dead_letter_count()
    }
}

// ================================================================
//                      Module-private helpers
// ================================================================

fn is_lease_holder(state: &State, worker_id: &str, job_id: JobId) -> bool {
    match state.jobs.get(&job_id) {
        Some(j) => matches!(&j.state, JobState::Leased { worker, .. } if worker == worker_id),
        None => false,
    }
}

fn resolve_worker_caps(state: &State, worker_id: &str) -> BTreeSet<String> {
    match state.workers.get(worker_id) {
        Some(w) => w.capabilities.clone(),
        None => BTreeSet::new(),
    }
}

fn any_acquirable_requires_caps(state: &State) -> bool {
    state.jobs.values().any(|j| {
        matches!(j.state, JobState::Pending { .. })
            && j.remaining_deps == 0
            && !j.required_capabilities.is_empty()
    })
}

/// Validate `depends_on` for unknown ids, duplicates, cross-namespace
/// references, and (defensively, since one-shot enqueue cannot close a
/// cycle) cycles.
fn validate_dependencies(
    deps: &[JobId],
    namespace: &str,
    state: &State,
) -> Result<(), DependencyError> {
    let mut seen: BTreeSet<JobId> = BTreeSet::new();
    for &dep_id in deps {
        if !seen.insert(dep_id) {
            return Err(DependencyError::Duplicate { job_id: dep_id });
        }
        let job = state
            .jobs
            .get(&dep_id)
            .ok_or(DependencyError::Unknown { job_id: dep_id })?;
        if job.namespace != namespace {
            return Err(DependencyError::CrossNamespace {
                job_id: dep_id,
                from: namespace.to_string(),
                to: job.namespace.clone(),
            });
        }
    }
    check_no_cycle(deps, state)?;
    Ok(())
}

fn check_no_cycle(deps: &[JobId], state: &State) -> Result<(), DependencyError> {
    fn visit(
        id: JobId,
        state: &State,
        on_stack: &mut BTreeSet<JobId>,
        visited: &mut BTreeSet<JobId>,
    ) -> Result<(), DependencyError> {
        if on_stack.contains(&id) {
            return Err(DependencyError::Cycle { job_id: id });
        }
        if visited.contains(&id) {
            return Ok(());
        }
        on_stack.insert(id);
        if let Some(job) = state.jobs.get(&id) {
            for &parent in &job.depends_on {
                visit(parent, state, on_stack, visited)?;
            }
        }
        on_stack.remove(&id);
        visited.insert(id);
        Ok(())
    }
    let mut visited = BTreeSet::new();
    let mut on_stack = BTreeSet::new();
    for &dep in deps {
        visit(dep, state, &mut on_stack, &mut visited)?;
    }
    Ok(())
}

/// Transition `job_id` to DeadLetter, push to its namespace's
/// dead-letter ring (dropping the oldest if over capacity), emit the
/// audit event.
fn terminal_dead_letter(
    state: &mut State,
    job_id: JobId,
    reason: String,
    now: Instant,
    audit_retention: usize,
) {
    let namespace = state.jobs[&job_id].namespace.clone();
    if let Some(job) = state.jobs.get_mut(&job_id) {
        job.state = JobState::DeadLetter {
            final_reason: reason.clone(),
            dead_at: now,
        };
    }
    let cap = state
        .namespaces
        .get(&namespace)
        .expect("namespace exists")
        .config
        .dead_letter_capacity;
    let ns = state
        .namespaces
        .get_mut(&namespace)
        .expect("namespace exists");
    ns.dead_letter.push_back(job_id);
    ns.counters.dead_lettered_total += 1;
    while ns.dead_letter.len() > cap {
        if let Some(dropped) = ns.dead_letter.pop_front() {
            state.jobs.remove(&dropped);
        }
    }
    state.audit.push_with(
        audit_retention,
        now,
        AuditEventKind::DeadLettered,
        Some(job_id),
        None,
        reason,
    );
}

/// Cascade-cancel `root` and every transitive dependent. Returns
/// the number transitioned. All emitted `Cancelled` audit events
/// share the same `at` timestamp so the cascade is one atomic step.
fn cascade_cancel(
    state: &mut State,
    root: JobId,
    reason: String,
    now: Instant,
    audit_retention: usize,
) -> usize {
    let order = state.cascade_collect(root);
    if order.is_empty() {
        return 0;
    }
    let mut count = 0usize;
    for (idx, id) in order.iter().enumerate() {
        let namespace = state.jobs[id].namespace.clone();
        if let JobState::Leased { worker, .. } = state.jobs[id].state.clone() {
            if let Some(ns) = state.namespaces.get_mut(&namespace) {
                ns.counters.lease_expired_total += 1;
            }
            state.audit.push_with(
                audit_retention,
                now,
                AuditEventKind::LeaseExpired,
                Some(*id),
                Some(worker.clone()),
                String::from("cancelled"),
            );
        }
        let event_payload = if idx == 0 {
            reason.clone()
        } else {
            format!("cascaded from {root} ({reason})")
        };
        if let Some(job) = state.jobs.get_mut(id) {
            job.state = JobState::Cancelled {
                reason: event_payload.clone(),
                cancelled_at: now,
            };
        }
        state.cancelled_order.push_back(*id);
        if let Some(ns) = state.namespaces.get_mut(&namespace) {
            ns.counters.cancelled_total += 1;
        }
        state.audit.push_with(
            audit_retention,
            now,
            AuditEventKind::Cancelled,
            Some(*id),
            None,
            event_payload,
        );
        count += 1;
    }
    count
}

/// Compute backoff per spec: `delay = min(base * 2^(attempt-1), cap)` +
/// `uniform_random(0, delay * jitter_fraction)`.
fn compute_backoff(
    base: Duration,
    cap: Duration,
    jitter_fraction: f64,
    attempt: u32,
) -> Duration {
    let exp = attempt.saturating_sub(1);
    let shift = 1u64.checked_shl(exp).unwrap_or(u64::MAX);
    let base_nanos = base.as_nanos().min(u64::MAX as u128) as u64;
    let cap_nanos = cap.as_nanos().min(u64::MAX as u128) as u64;
    let unbounded = base_nanos.saturating_mul(shift);
    let bounded = unbounded.min(cap_nanos);
    let mut delay = Duration::from_nanos(bounded);
    if jitter_fraction > 0.0 && bounded > 0 {
        let max_jitter = ((bounded as f64) * jitter_fraction.clamp(0.0, 1.0)) as u64;
        if max_jitter > 0 {
            let jitter = rand::thread_rng().gen_range(0..=max_jitter);
            delay += Duration::from_nanos(jitter);
        }
    }
    delay
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[test]
    fn first_retry_uses_two_to_the_zero() {
        let d = compute_backoff(Duration::from_millis(100), Duration::from_secs(30), 0.0, 1);
        assert_eq!(d, Duration::from_millis(100));
    }

    #[test]
    fn doubles_each_attempt() {
        let base = Duration::from_millis(100);
        let cap = Duration::from_secs(30);
        assert_eq!(compute_backoff(base, cap, 0.0, 1), Duration::from_millis(100));
        assert_eq!(compute_backoff(base, cap, 0.0, 2), Duration::from_millis(200));
        assert_eq!(compute_backoff(base, cap, 0.0, 3), Duration::from_millis(400));
        assert_eq!(compute_backoff(base, cap, 0.0, 4), Duration::from_millis(800));
    }

    #[test]
    fn caps_at_backoff_cap() {
        let d = compute_backoff(Duration::from_secs(1), Duration::from_secs(5), 0.0, 10);
        assert_eq!(d, Duration::from_secs(5));
    }

    #[test]
    fn jitter_in_bounds() {
        let base = Duration::from_millis(100);
        let cap = Duration::from_secs(30);
        for _ in 0..100 {
            let d = compute_backoff(base, cap, 0.5, 1);
            assert!(d >= Duration::from_millis(100));
            assert!(d <= Duration::from_millis(150));
        }
    }
}
