//! Queue configuration set at construction time.
//!
//! v2.0 layers namespace-scoped configuration on top of v1.x. The
//! top-level [`Config`] fields keep their v1.x meaning — `active_capacity`,
//! `dead_letter_capacity`, and `max_attempts` are now the *defaults*
//! applied to any namespace that doesn't register its own
//! [`NamespaceConfig`]. v1.x callers that never register a namespace get
//! a single `"default"` namespace whose effective config is built from
//! the top-level fields exactly as before.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Per-namespace tunables. Two namespaces never share an
/// `active_capacity` or `dead_letter_capacity` slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespaceConfig {
    /// Maximum jobs in `pending + retry-pending + scheduled` state at
    /// once for this namespace. Leased and dead-lettered jobs do NOT
    /// count against this.
    pub active_capacity: usize,

    /// Maximum dead-lettered entries retained in this namespace.
    /// When exceeded, the oldest dead-lettered entry in this namespace
    /// is dropped.
    pub dead_letter_capacity: usize,

    /// Number of explicit `fail()` calls before a job is dead-lettered.
    pub max_attempts: u32,
}

impl Default for NamespaceConfig {
    fn default() -> Self {
        Self {
            active_capacity: 1024,
            dead_letter_capacity: 256,
            max_attempts: 3,
        }
    }
}

/// Top-level queue configuration. v1.x fields are preserved exactly;
/// v2.0 adds `audit_retention`, `require_worker_registration`, and an
/// optional per-namespace config override map.
#[derive(Debug, Clone)]
pub struct Config {
    /// v1.x: default per-namespace active capacity. Acts as the
    /// fallback for any namespace that hasn't registered its own
    /// [`NamespaceConfig`].
    pub active_capacity: usize,

    /// v1.x: default per-namespace dead-letter capacity.
    pub dead_letter_capacity: usize,

    /// v1.x: default per-namespace max_attempts.
    pub max_attempts: u32,

    /// v1.x: base unit of the exponential backoff (queue-wide).
    pub backoff_base: Duration,

    /// v1.x: upper bound on the deterministic part of the backoff.
    pub backoff_cap: Duration,

    /// v1.x: fraction of backoff that may be added as uniform random
    /// jitter. `0.0` disables jitter.
    pub jitter_fraction: f64,

    /// v1.x: default lease duration. Informational — `acquire` always
    /// takes an explicit lease duration.
    pub default_lease_duration: Duration,

    // ---------- v2.0 additions ----------
    /// Maximum events retained by the audit log. When exceeded on a
    /// new event, the oldest event is dropped.
    pub audit_retention: usize,

    /// If `true`, `acquire` returns `UnknownWorker` whenever called
    /// with a worker_id that hasn't been registered. If `false`
    /// (default — preserves v1.x behavior), `acquire` only enforces
    /// registration for jobs whose `required_capabilities` is
    /// non-empty.
    pub require_worker_registration: bool,

    /// Per-namespace overrides. Namespaces not present here inherit
    /// the queue-wide defaults (the v1.x top-level fields).
    pub namespace_configs: HashMap<String, NamespaceConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            active_capacity: 1024,
            dead_letter_capacity: 256,
            max_attempts: 3,
            backoff_base: Duration::from_millis(100),
            backoff_cap: Duration::from_secs(30),
            jitter_fraction: 0.1,
            default_lease_duration: Duration::from_secs(30),
            audit_retention: 10_000,
            require_worker_registration: false,
            namespace_configs: HashMap::new(),
        }
    }
}

impl Config {
    /// Effective per-namespace config: the override in
    /// `namespace_configs[name]` if registered, otherwise the
    /// queue-wide defaults built from the top-level v1.x fields.
    pub fn namespace_config(&self, name: &str) -> NamespaceConfig {
        if let Some(cfg) = self.namespace_configs.get(name) {
            return cfg.clone();
        }
        NamespaceConfig {
            active_capacity: self.active_capacity,
            dead_letter_capacity: self.dead_letter_capacity,
            max_attempts: self.max_attempts,
        }
    }
}

/// Built-in name of the namespace that v1.x jobs land in if no
/// namespace is supplied.
pub const DEFAULT_NAMESPACE: &str = "default";
