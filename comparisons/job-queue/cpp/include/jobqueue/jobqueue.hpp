#pragma once

#include "audit.hpp"
#include "clock.hpp"
#include "metrics.hpp"
#include "storage.hpp"
#include "types.hpp"
#include "worker.hpp"

#include <deque>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>
#include <unordered_map>
#include <vector>

namespace jobqueue {

class JobQueue {
public:
    JobQueue(Config cfg,
             std::shared_ptr<Clock> clock,
             std::shared_ptr<Storage> storage);

    // -------- v1 / v1.1 surface ----------------------------------------

    // Enqueue a job. v1.x callers using `enqueue(payload)` continue to work
    // via the defaulted options struct. v2 adds depends_on, capabilities,
    // and namespace_id to the same struct.
    Result<JobId, EnqueueErr> enqueue(std::string payload,
                                      EnqueueOptions opts = {});

    Result<AcquireOk, AcquireErr> acquire(std::string worker_id,
                                          Duration lease_duration);

    Result<Ok, HeartbeatErr> heartbeat(std::string_view worker_id,
                                       JobId job_id,
                                       Duration lease_extension);

    Result<Ok, CompleteErr> complete(std::string_view worker_id, JobId job_id);

    Result<Ok, FailErr> fail(std::string_view worker_id,
                             JobId job_id,
                             std::string reason);

    // Backward-compatible dead-letter view across all namespaces, in
    // chronological order.
    std::vector<DeadLetterEntry> dead_letter_snapshot() const;

    std::size_t active_count() const;
    std::size_t leased_count() const;

    // v1.1 metrics rollup (queue-wide). Same shape as before — v2 callers
    // can additionally request the per-namespace breakdown via
    // metrics_per_namespace().
    Metrics metrics() const;

    // -------- v2 surface ------------------------------------------------

    // Worker registry. register_worker is idempotent; re-registering with a
    // different capability set replaces the previous one. deregister returns
    // the number of leases force-expired by the deregistration.
    void register_worker(std::string id, std::vector<std::string> capabilities);
    std::size_t deregister_worker(std::string_view id);
    std::vector<WorkerView> list_workers() const;

    // Cancellation. Cascades through dependents transitively (terminal
    // descendants stop the cascade). Returns the total count actually
    // transitioned to Cancelled (zero when the seed is unknown or already in
    // a terminal state). A leased seed force-expires its lease.
    CancelResult cancel(JobId job_id, std::string reason);
    std::vector<CancelledEntry> cancelled_snapshot() const;

    // Namespaces. Auto-created on first reference with queue-level defaults;
    // register_namespace overrides those defaults. list_namespaces returns
    // every namespace ever touched (auto-created or registered).
    void register_namespace(std::string name, NamespaceConfig config);
    std::vector<std::string> list_namespaces() const;
    std::unordered_map<std::string, Metrics> metrics_per_namespace() const;

    // Priority promotion: raise a Pending job's priority. new_priority must
    // be in the valid range and strictly greater than the job's current.
    Result<Ok, PromoteErr> promote(JobId job_id, std::uint32_t new_priority);

    // Audit log access. audit_recent returns the last `limit` events;
    // audit_since returns every event with event_id > after, or
    // AuditEventDropped if `after` is older than the oldest retained.
    std::vector<AuditEvent> audit_recent(std::size_t limit) const;
    Result<std::vector<AuditEvent>, AuditErr> audit_since(std::uint64_t after) const;

private:
    struct NamespaceState {
        NamespaceConfig config;
        std::vector<DeadLetterEntry> dead_letter;
        Metrics counters;
    };

    mutable std::mutex mu_;
    Config cfg_;
    std::shared_ptr<Clock> clock_;
    std::shared_ptr<Storage> storage_;
    std::vector<Job> jobs_;
    std::unordered_map<std::string, NamespaceState> namespaces_;
    std::unordered_map<std::string, Worker> workers_;
    std::vector<CancelledEntry> cancelled_;
    std::deque<AuditEvent> audit_;
    JobId next_id_                          = 1;
    std::uint64_t next_seq_                 = 0;
    std::uint64_t next_event_id_            = 1;
    std::uint64_t oldest_retained_event_id_ = 1;

    void sweep_locked(TimePoint now);
    Duration compute_backoff_locked(std::uint32_t attempt);
    void persist_locked();
    Job* find_locked(JobId id);
    const Job* find_locked(JobId id) const;
    NamespaceState& ns_locked(const std::string& name);
    const NamespaceState* ns_lookup_locked(const std::string& name) const;
    std::size_t active_count_locked(const std::string& ns) const;
    std::size_t leased_count_locked() const;

    bool deps_satisfied_locked(const Job& j) const;
    bool worker_can_handle_locked(const std::vector<std::string>& caps,
                                  const std::vector<std::string>& required) const;

    void emit_locked(AuditEventKind kind,
                     std::optional<JobId> job_id,
                     std::optional<std::string> worker_id,
                     std::string payload,
                     TimePoint now);

    // Returns the set of dependents (transitive) of `seed`, including seed
    // itself, restricted to jobs whose state is non-terminal.
    std::vector<JobId> compute_cascade_locked(JobId seed) const;
    // Transition every id in `set` to Cancelled with the given reason. The
    // first id in `set` is the originating cancel; others get a "cascade
    // from <parent_id>" audit payload. All events share at_nanos = now.
    std::size_t apply_cancellation_locked(const std::vector<JobId>& set,
                                          const std::string& reason,
                                          TimePoint now);
    // Move a job into its namespace's dead-letter and run the cancellation
    // cascade on its dependents. Used by fail() when attempts are exhausted.
    void dead_letter_locked(Job& job, std::string reason, TimePoint now);
};

} // namespace jobqueue
