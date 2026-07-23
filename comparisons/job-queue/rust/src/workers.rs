//! Worker registry — tracks workers and their capabilities.
//!
//! In v1.x workers were anonymous strings. v2.0 elevates them to
//! first-class entities so the queue can enforce capability-based
//! routing (each job's `required_capabilities` must be a subset of the
//! acquiring worker's registered set).
//!
//! Backward compatibility: jobs with empty `required_capabilities`
//! never need a registered worker; the v1.x flow of acquiring with an
//! ad-hoc worker id continues to work. Only `Config::require_worker_
//! registration = true` makes registration mandatory for every acquire.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::queue::WorkerId;

/// A registered worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Worker {
    pub id: WorkerId,
    pub capabilities: BTreeSet<String>,
}

/// Read-only view returned by `Queue::list_workers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerView {
    pub id: WorkerId,
    /// Capabilities in deterministic (sorted) order.
    pub capabilities: Vec<String>,
}

/// In-process worker registry. Backed by a `BTreeMap` so list order is
/// deterministic.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct WorkerRegistry {
    pub workers: BTreeMap<WorkerId, Worker>,
}

impl WorkerRegistry {
    pub fn register(&mut self, id: &str, capabilities: Vec<String>) {
        let caps: BTreeSet<String> = capabilities.into_iter().collect();
        self.workers.insert(
            id.to_string(),
            Worker {
                id: id.to_string(),
                capabilities: caps,
            },
        );
    }

    pub fn deregister(&mut self, id: &str) -> Option<Worker> {
        self.workers.remove(id)
    }

    pub fn get(&self, id: &str) -> Option<&Worker> {
        self.workers.get(id)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.workers.contains_key(id)
    }

    /// Snapshot of the registry as `WorkerView`s in sorted order by id.
    pub fn list(&self) -> Vec<WorkerView> {
        self.workers
            .values()
            .map(|w| WorkerView {
                id: w.id.clone(),
                capabilities: w.capabilities.iter().cloned().collect(),
            })
            .collect()
    }
}

/// Returns true iff the worker's capability set is a superset of `required`.
pub(crate) fn caps_satisfy(worker_caps: &BTreeSet<String>, required: &[String]) -> bool {
    required.iter().all(|c| worker_caps.contains(c))
}
