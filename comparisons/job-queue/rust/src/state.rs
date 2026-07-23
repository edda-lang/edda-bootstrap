//! Internal state machine, transitions, and persistence.
//!
//! The on-disk format is the same struct as the in-memory state — no
//! separate "persisted projection". Every public mutating operation
//! takes the queue's write lock, runs [`State::tick`] (promotes due
//! retries / scheduled jobs and expires stale leases), applies the
//! change, then snapshots the whole state to disk via [`save`].
//!
//! v2.0 keeps terminal jobs (`Completed`, `Cancelled`, `DeadLetter`)
//! in the `jobs` map indefinitely so dependency lookups remain valid
//! across the job lifecycle. Dead-letter entries dropped by the
//! per-namespace capacity bound are removed from `jobs` to match the
//! v1.x semantics of "dropped permanently."

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::audit::AuditLog;
use crate::clock::Instant;
use crate::config::{NamespaceConfig, DEFAULT_NAMESPACE};
use crate::metrics::Counters;
use crate::priority::Priority;
use crate::queue::{CancelledEntry, DeadLetterEntry, JobId, Payload, WorkerId};
use crate::workers::{caps_satisfy, WorkerRegistry};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Job {
    pub id: JobId,
    pub payload: Payload,
    pub priority: Priority,
    /// Number of completed `fail()` calls. The 1-based attempt number
    /// reported by `acquire` is `failures + 1`.
    pub failures: u32,
    /// Monotonic sequence assigned at enqueue. Tie-breaker for equal
    /// priority.
    pub seq: u64,
    /// Absolute time at/after which the job becomes acquirable. Set at
    /// enqueue, never mutated.
    pub scheduled_at: Option<Instant>,
    /// Namespace this job lives in. Defaults to `"default"`.
    pub namespace: String,
    /// IDs this job depends on. Set at enqueue, never mutated.
    pub depends_on: Vec<JobId>,
    /// IDs of jobs that depend on this one. Maintained incrementally
    /// as children are enqueued.
    pub dependents: Vec<JobId>,
    /// Count of `depends_on` parents that haven't yet `Completed`.
    /// Zero ⇒ dependency gate cleared.
    pub remaining_deps: u32,
    /// Capabilities a worker must hold to acquire this job. Empty list
    /// means "any worker."
    pub required_capabilities: Vec<String>,
    pub state: JobState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum JobState {
    Pending {
        /// `Some(worker)` iff the most recent transition into Pending
        /// was an expired lease. Lets `heartbeat` distinguish
        /// `LeaseExpired` from `NotLeaseHolder` for the original
        /// holder.
        last_holder: Option<WorkerId>,
    },
    Leased {
        worker: WorkerId,
        expires_at: Instant,
    },
    RetryPending {
        ready_at: Instant,
    },
    Scheduled {
        available_at: Instant,
    },
    Completed {
        completed_at: Instant,
    },
    Cancelled {
        reason: String,
        cancelled_at: Instant,
    },
    DeadLetter {
        final_reason: String,
        dead_at: Instant,
    },
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobState::Completed { .. } | JobState::Cancelled { .. } | JobState::DeadLetter { .. }
        )
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct NamespaceState {
    pub config: NamespaceConfig,
    pub counters: Counters,
    /// Dead-letter ring for this namespace; oldest at the front.
    pub dead_letter: VecDeque<JobId>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct State {
    pub next_id: u64,
    pub next_seq: u64,
    pub jobs: HashMap<JobId, Job>,
    pub namespaces: BTreeMap<String, NamespaceState>,
    pub workers: WorkerRegistry,
    pub audit: AuditLog,
    /// Cancellation order across all namespaces.
    pub cancelled_order: VecDeque<JobId>,
}

impl State {
    pub fn empty() -> Self {
        State {
            next_id: 1,
            next_seq: 0,
            jobs: HashMap::new(),
            namespaces: BTreeMap::new(),
            workers: WorkerRegistry::default(),
            audit: AuditLog::default(),
            cancelled_order: VecDeque::new(),
        }
    }

    /// Default namespace name, used when callers omit `namespace(..)`
    /// on `EnqueueRequest`.
    pub fn default_namespace() -> &'static str {
        DEFAULT_NAMESPACE
    }

    /// Get-or-create the [`NamespaceState`] for `name`, seeded with
    /// `default_cfg`. Idempotent.
    pub fn namespace_mut(
        &mut self,
        name: &str,
        default_cfg: NamespaceConfig,
    ) -> &mut NamespaceState {
        self.namespaces
            .entry(name.to_string())
            .or_insert_with(|| NamespaceState {
                config: default_cfg,
                counters: Counters::default(),
                dead_letter: VecDeque::new(),
            })
    }

    /// Promote due retries / scheduled jobs, expire stale leases.
    /// Counter increments for the expiry path are applied per-namespace.
    pub fn tick(&mut self, now: Instant) {
        self.promote_scheduled(now);
        self.promote_retries(now);
        self.expire_leases(now);
    }

    fn promote_scheduled(&mut self, now: Instant) {
        let due: Vec<JobId> = self
            .jobs
            .values()
            .filter_map(|j| match j.state {
                JobState::Scheduled { available_at } if available_at <= now => Some(j.id),
                _ => None,
            })
            .collect();
        for id in due {
            if let Some(job) = self.jobs.get_mut(&id) {
                job.state = JobState::Pending { last_holder: None };
            }
        }
    }

    fn promote_retries(&mut self, now: Instant) {
        let due: Vec<JobId> = self
            .jobs
            .values()
            .filter_map(|j| match j.state {
                JobState::RetryPending { ready_at } if ready_at <= now => Some(j.id),
                _ => None,
            })
            .collect();
        for id in due {
            if let Some(job) = self.jobs.get_mut(&id) {
                job.state = JobState::Pending { last_holder: None };
            }
        }
    }

    fn expire_leases(&mut self, now: Instant) {
        let expired: Vec<(JobId, WorkerId, String)> = self
            .jobs
            .values()
            .filter_map(|j| match &j.state {
                JobState::Leased { worker, expires_at } if *expires_at <= now => {
                    Some((j.id, worker.clone(), j.namespace.clone()))
                }
                _ => None,
            })
            .collect();
        for (id, worker, namespace) in expired {
            if let Some(job) = self.jobs.get_mut(&id) {
                job.state = JobState::Pending {
                    last_holder: Some(worker),
                };
            }
            if let Some(ns) = self.namespaces.get_mut(&namespace) {
                ns.counters.lease_expired_total += 1;
            }
        }
    }

    /// Eligible-for-acquire predicate: `Pending`, deps satisfied,
    /// worker holds all required capabilities.
    pub fn is_acquirable(&self, job: &Job, worker_caps: &BTreeSet<String>) -> bool {
        if !matches!(job.state, JobState::Pending { .. }) {
            return false;
        }
        if job.remaining_deps != 0 {
            return false;
        }
        caps_satisfy(worker_caps, &job.required_capabilities)
    }

    /// Highest-priority oldest-seq acquirable job for the given
    /// capability set. Returns `None` if nothing qualifies.
    pub fn next_acquirable(&self, worker_caps: &BTreeSet<String>) -> Option<JobId> {
        self.jobs
            .values()
            .filter(|j| self.is_acquirable(j, worker_caps))
            .min_by_key(|j| (std::cmp::Reverse(j.priority.value()), j.seq))
            .map(|j| j.id)
    }

    // ---------- gauge counts ----------

    pub fn active_count_ns(&self, namespace: &str) -> usize {
        self.jobs
            .values()
            .filter(|j| {
                j.namespace == namespace
                    && matches!(
                        j.state,
                        JobState::Pending { .. }
                            | JobState::RetryPending { .. }
                            | JobState::Scheduled { .. }
                    )
            })
            .count()
    }

    pub fn leased_count_ns(&self, namespace: &str) -> usize {
        self.jobs
            .values()
            .filter(|j| j.namespace == namespace && matches!(j.state, JobState::Leased { .. }))
            .count()
    }

    pub fn dead_letter_count_ns(&self, namespace: &str) -> usize {
        self.namespaces
            .get(namespace)
            .map(|ns| ns.dead_letter.len())
            .unwrap_or(0)
    }

    pub fn active_count(&self) -> usize {
        self.jobs
            .values()
            .filter(|j| {
                matches!(
                    j.state,
                    JobState::Pending { .. }
                        | JobState::RetryPending { .. }
                        | JobState::Scheduled { .. }
                )
            })
            .count()
    }

    pub fn leased_count(&self) -> usize {
        self.jobs
            .values()
            .filter(|j| matches!(j.state, JobState::Leased { .. }))
            .count()
    }

    pub fn dead_letter_count(&self) -> usize {
        self.namespaces.values().map(|ns| ns.dead_letter.len()).sum()
    }

    // ---------- iterators / snapshots ----------

    pub fn dead_letter_entries(&self) -> Vec<DeadLetterEntry> {
        let mut out = Vec::new();
        for ns in self.namespaces.values() {
            for id in &ns.dead_letter {
                let Some(job) = self.jobs.get(id) else {
                    continue;
                };
                if let JobState::DeadLetter { final_reason, .. } = &job.state {
                    out.push(DeadLetterEntry {
                        id: job.id,
                        payload: job.payload.clone(),
                        final_reason: final_reason.clone(),
                        namespace: job.namespace.clone(),
                    });
                }
            }
        }
        out
    }

    pub fn cancelled_entries(&self) -> Vec<CancelledEntry> {
        self.cancelled_order
            .iter()
            .filter_map(|id| self.jobs.get(id))
            .filter_map(|job| match &job.state {
                JobState::Cancelled {
                    reason,
                    cancelled_at,
                } => Some(CancelledEntry {
                    id: job.id,
                    payload: job.payload.clone(),
                    reason: reason.clone(),
                    namespace: job.namespace.clone(),
                    cancelled_at: *cancelled_at,
                }),
                _ => None,
            })
            .collect()
    }

    // ---------- dependency / cascade helpers ----------

    /// `root` plus every transitive dependent still in an active
    /// (non-terminal) state, in BFS order from `root`. Terminal states
    /// stop the cascade.
    pub fn cascade_collect(&self, root: JobId) -> Vec<JobId> {
        let mut visited: HashSet<JobId> = HashSet::new();
        let mut order: Vec<JobId> = Vec::new();
        let mut queue: VecDeque<JobId> = VecDeque::new();
        queue.push_back(root);
        while let Some(id) = queue.pop_front() {
            if !visited.insert(id) {
                continue;
            }
            let Some(job) = self.jobs.get(&id) else {
                continue;
            };
            if job.state.is_terminal() {
                continue;
            }
            order.push(id);
            for dep_id in &job.dependents {
                queue.push_back(*dep_id);
            }
        }
        order
    }
}

/// Load persisted state from `path`, or return an empty state if the
/// file does not exist.
pub(crate) fn load(path: &Path) -> io::Result<State> {
    if !path.exists() {
        return Ok(State::empty());
    }
    let mut f = File::open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    if buf.is_empty() {
        return Ok(State::empty());
    }
    let state: State = serde_json::from_slice(&buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(state)
}

/// Persist `state` to `path` via write-tmp + atomic rename + parent
/// fsync (Unix).
pub(crate) fn save(path: &Path, state: &State) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let bytes = serde_json::to_vec(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tmp = tmp_path(path);
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    {
        let parent_path = path.parent().unwrap_or_else(|| Path::new("."));
        let parent = if parent_path.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent_path
        };
        let dir = File::open(parent)?;
        dir.sync_all()?;
    }
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    match path.extension() {
        Some(ext) => {
            let mut new_ext = ext.to_os_string();
            new_ext.push(".tmp");
            path.with_extension(new_ext)
        }
        None => path.with_extension("tmp"),
    }
}
