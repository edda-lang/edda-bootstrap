// JobQueue lifecycle plus the five core job-state transitions
// (enqueue/acquire/heartbeat/complete/fail) and the v1 observers. The
// cancellation cascade lives in cascade.cpp; the v2 admin surface (workers,
// namespaces, promotion, audit readers) lives in admin.cpp.

#include "jobqueue/jobqueue.hpp"

#include <algorithm>
#include <chrono>
#include <limits>
#include <queue>
#include <unordered_set>
#include <utility>

namespace jobqueue {

namespace {

bool is_terminal(JobState s) {
    return s == JobState::DeadLettered
        || s == JobState::Completed
        || s == JobState::Cancelled;
}

bool is_acquirable_state(JobState s) {
    return s == JobState::Pending;
}

} // namespace

// ----------------------------- ctor -----------------------------

JobQueue::JobQueue(Config cfg,
                   std::shared_ptr<Clock> clock,
                   std::shared_ptr<Storage> storage)
    : cfg_(std::move(cfg)),
      clock_(std::move(clock)),
      storage_(std::move(storage)) {
    auto loaded = storage_->load();
    if (loaded) {
        jobs_                     = std::move(loaded->jobs);
        next_id_                  = loaded->next_id;
        next_seq_                 = loaded->next_seq;
        next_event_id_            = loaded->next_event_id;
        oldest_retained_event_id_ = loaded->oldest_retained_event_id;
        for (auto& [name, ns] : loaded->namespaces) {
            NamespaceState s;
            s.config      = ns.config;
            s.dead_letter = std::move(ns.dead_letter);
            s.counters    = ns.counters;
            namespaces_.emplace(name, std::move(s));
        }
        workers_   = std::move(loaded->workers);
        cancelled_ = std::move(loaded->cancelled);
        audit_     = std::move(loaded->audit);
    }
    // The "default" namespace always exists so v1.x callers find a capacity
    // slot. Per-namespace registrations override these defaults.
    if (namespaces_.find(kDefaultNamespace) == namespaces_.end()) {
        NamespaceState s;
        s.config = NamespaceConfig{cfg_.active_capacity, cfg_.dead_letter_capacity};
        namespaces_.emplace(kDefaultNamespace, std::move(s));
    }
}

// ----------------------------- helpers (locked) -----------------------------

JobQueue::NamespaceState& JobQueue::ns_locked(const std::string& name) {
    auto it = namespaces_.find(name);
    if (it != namespaces_.end()) return it->second;
    NamespaceState s;
    s.config = NamespaceConfig{cfg_.active_capacity, cfg_.dead_letter_capacity};
    auto [ins, _] = namespaces_.emplace(name, std::move(s));
    return ins->second;
}

const JobQueue::NamespaceState* JobQueue::ns_lookup_locked(const std::string& name) const {
    auto it = namespaces_.find(name);
    return (it == namespaces_.end()) ? nullptr : &it->second;
}

std::size_t JobQueue::active_count_locked(const std::string& ns) const {
    std::size_t n = 0;
    for (const auto& j : jobs_) {
        if (j.namespace_id != ns) continue;
        if (j.state == JobState::Pending || j.state == JobState::RetryPending) ++n;
    }
    return n;
}

std::size_t JobQueue::leased_count_locked() const {
    std::size_t n = 0;
    for (const auto& j : jobs_) if (j.state == JobState::Leased) ++n;
    return n;
}

Job* JobQueue::find_locked(JobId id) {
    for (auto& j : jobs_) if (j.id == id) return &j;
    return nullptr;
}

const Job* JobQueue::find_locked(JobId id) const {
    for (const auto& j : jobs_) if (j.id == id) return &j;
    return nullptr;
}

bool JobQueue::deps_satisfied_locked(const Job& j) const {
    for (auto dep : j.depends_on) {
        const Job* d = find_locked(dep);
        if (!d || d->state != JobState::Completed) return false;
    }
    return true;
}

bool JobQueue::worker_can_handle_locked(const std::vector<std::string>& caps,
                                        const std::vector<std::string>& required) const {
    for (const auto& r : required) {
        if (std::find(caps.begin(), caps.end(), r) == caps.end()) return false;
    }
    return true;
}

void JobQueue::emit_locked(AuditEventKind kind,
                           std::optional<JobId> job_id,
                           std::optional<std::string> worker_id,
                           std::string payload,
                           TimePoint now) {
    AuditEvent e;
    e.event_id  = next_event_id_++;
    e.at_nanos  = std::chrono::duration_cast<std::chrono::nanoseconds>(
                      now.time_since_epoch()).count();
    e.kind      = kind;
    e.job_id    = job_id;
    e.worker_id = std::move(worker_id);
    e.payload   = std::move(payload);
    audit_.push_back(std::move(e));
    while (audit_.size() > cfg_.audit_retention) {
        oldest_retained_event_id_ = audit_.front().event_id + 1;
        audit_.pop_front();
    }
    if (!audit_.empty() && oldest_retained_event_id_ < audit_.front().event_id) {
        oldest_retained_event_id_ = audit_.front().event_id;
    }
}

// ----------------------------- enqueue -----------------------------

Result<JobId, EnqueueErr> JobQueue::enqueue(std::string payload, EnqueueOptions opts) {
    if (opts.priority < kPriorityMin || opts.priority > kPriorityMax) {
        return EnqueueErr::InvalidPriority;
    }
    std::lock_guard<std::mutex> lock(mu_);
    const auto now = clock_->now();
    sweep_locked(now);

    // Each dependency must reference a known job in the same namespace.
    for (auto dep : opts.depends_on) {
        const Job* d = find_locked(dep);
        if (!d) return EnqueueErr::InvalidDependency;
        if (d->namespace_id != opts.namespace_id) return EnqueueErr::InvalidDependency;
    }
    // Defensive cycle scan: walk the transitive closure of dependencies; if
    // we reach the about-to-be-allocated id, reject. Cannot trigger in
    // practice (new ids aren't in any existing depends_on) but also catches
    // corrupted persistence on load.
    const JobId pending_id = next_id_;
    for (auto dep : opts.depends_on) {
        std::unordered_set<JobId> seen;
        std::queue<JobId> q;
        q.push(dep);
        while (!q.empty()) {
            JobId cur = q.front(); q.pop();
            if (cur == pending_id) return EnqueueErr::InvalidDependency;
            if (!seen.insert(cur).second) continue;
            const Job* d = find_locked(cur);
            if (!d) continue;
            for (auto sub : d->depends_on) q.push(sub);
        }
    }

    auto& ns_state = ns_locked(opts.namespace_id);
    if (active_count_locked(opts.namespace_id) >= ns_state.config.active_capacity) {
        return EnqueueErr::QueueFull;
    }

    const JobId id = next_id_++;
    Job j;
    j.id                    = id;
    j.namespace_id          = opts.namespace_id;
    j.payload               = std::move(payload);
    j.attempt               = 1;
    j.priority              = opts.priority;
    j.enqueue_seq           = next_seq_++;
    j.state                 = JobState::Pending;
    j.scheduled_at          = opts.scheduled_at;
    j.depends_on            = std::move(opts.depends_on);
    j.required_capabilities = std::move(opts.required_capabilities);
    jobs_.push_back(std::move(j));
    ns_state.counters.enqueued_total += 1;
    emit_locked(AuditEventKind::Enqueued, id, std::nullopt, "", now);

    // Retroactive cascade: if any dep is already terminal-failure, cancel
    // the newcomer (and its dependents, of which it has none at enqueue time).
    JobId terminal_parent = 0;
    bool dep_terminal = false;
    for (auto dep : jobs_.back().depends_on) {
        const Job* d = find_locked(dep);
        if (d && (d->state == JobState::DeadLettered ||
                  d->state == JobState::Cancelled)) {
            dep_terminal = true;
            terminal_parent = dep;
            break;
        }
    }
    if (dep_terminal) {
        auto cascade = compute_cascade_locked(id);
        apply_cancellation_locked(
            cascade,
            "dependency " + std::to_string(terminal_parent) + " in terminal state",
            now);
    }
    persist_locked();
    return id;
}

// ----------------------------- acquire -----------------------------

Result<AcquireOk, AcquireErr> JobQueue::acquire(std::string worker_id,
                                                Duration lease_duration) {
    std::lock_guard<std::mutex> lock(mu_);
    const auto now = clock_->now();
    sweep_locked(now);

    std::vector<std::string> caps;
    auto wit = workers_.find(worker_id);
    if (cfg_.require_worker_registration) {
        if (wit == workers_.end()) return AcquireErr::UnknownWorker;
        caps = wit->second.capabilities;
    } else if (wit != workers_.end()) {
        caps = wit->second.capabilities;
    }

    Job* best = nullptr;
    for (auto& j : jobs_) {
        if (!is_acquirable_state(j.state)) continue;
        if (j.scheduled_at && *j.scheduled_at > now) continue;
        if (!deps_satisfied_locked(j)) continue;
        if (!worker_can_handle_locked(caps, j.required_capabilities)) continue;
        if (!best ||
            j.priority > best->priority ||
            (j.priority == best->priority && j.enqueue_seq < best->enqueue_seq)) {
            best = &j;
        }
    }
    if (!best) return AcquireErr::Empty;

    best->state            = JobState::Leased;
    best->lease_holder     = worker_id;
    best->lease_expires_at = now + lease_duration;
    best->retry_ready_at.reset();
    AcquireOk ok{best->id, best->payload, best->attempt, best->priority};
    ns_locked(best->namespace_id).counters.acquired_total += 1;
    emit_locked(AuditEventKind::Acquired, best->id, worker_id,
                "attempt=" + std::to_string(best->attempt), now);
    persist_locked();
    return ok;
}

// ----------------------------- heartbeat / complete / fail -----------------------------

Result<Ok, HeartbeatErr> JobQueue::heartbeat(std::string_view worker_id,
                                             JobId job_id,
                                             Duration lease_extension) {
    std::lock_guard<std::mutex> lock(mu_);
    const auto now = clock_->now();
    Job* j = find_locked(job_id);
    if (!j) return HeartbeatErr::NotLeaseHolder;
    const bool was_holder = j->lease_holder && *j->lease_holder == worker_id;
    if (!was_holder) return HeartbeatErr::NotLeaseHolder;
    if (j->state != JobState::Leased) return HeartbeatErr::LeaseExpired;
    if (!j->lease_expires_at || *j->lease_expires_at <= now) {
        return HeartbeatErr::LeaseExpired;
    }
    j->lease_expires_at = now + lease_extension;
    emit_locked(AuditEventKind::HeartbeatExtended, job_id,
                std::string{worker_id}, "", now);
    persist_locked();
    return Ok{};
}

Result<Ok, CompleteErr> JobQueue::complete(std::string_view worker_id, JobId job_id) {
    std::lock_guard<std::mutex> lock(mu_);
    const auto now = clock_->now();
    sweep_locked(now);
    Job* j = find_locked(job_id);
    if (!j || j->state != JobState::Leased ||
        !j->lease_holder || *j->lease_holder != worker_id) {
        return CompleteErr::NotLeaseHolder;
    }
    j->state = JobState::Completed;
    j->lease_holder.reset();
    j->lease_expires_at.reset();
    ns_locked(j->namespace_id).counters.completed_total += 1;
    emit_locked(AuditEventKind::Completed, job_id, std::string{worker_id}, "", now);
    persist_locked();
    return Ok{};
}

Result<Ok, FailErr> JobQueue::fail(std::string_view worker_id,
                                   JobId job_id,
                                   std::string reason) {
    std::lock_guard<std::mutex> lock(mu_);
    const auto now = clock_->now();
    sweep_locked(now);

    Job* j = find_locked(job_id);
    if (!j || j->state != JobState::Leased ||
        !j->lease_holder || *j->lease_holder != worker_id) {
        return FailErr::NotLeaseHolder;
    }
    auto& ns_state = ns_locked(j->namespace_id);
    ns_state.counters.failed_total += 1;
    emit_locked(AuditEventKind::Failed, job_id, std::string{worker_id}, reason, now);

    if (j->attempt >= cfg_.max_attempts) {
        dead_letter_locked(*j, std::move(reason), now);
    } else {
        const Duration delay = compute_backoff_locked(j->attempt);
        j->attempt        += 1;
        j->state           = JobState::RetryPending;
        j->retry_ready_at  = now + delay;
        j->lease_holder.reset();
        j->lease_expires_at.reset();
        ns_state.counters.retry_scheduled_total += 1;
        emit_locked(AuditEventKind::RetryScheduled, job_id, std::nullopt,
                    "attempt=" + std::to_string(j->attempt), now);
    }
    persist_locked();
    return Ok{};
}

// ----------------------------- sweep + backoff -----------------------------

void JobQueue::sweep_locked(TimePoint now) {
    for (auto& j : jobs_) {
        if (j.state == JobState::Leased &&
            j.lease_expires_at && *j.lease_expires_at <= now) {
            j.state = JobState::Pending;
            j.lease_expires_at.reset();
            ns_locked(j.namespace_id).counters.lease_expired_total += 1;
            emit_locked(AuditEventKind::LeaseExpired, j.id, j.lease_holder, "", now);
        }
        if (j.state == JobState::RetryPending &&
            j.retry_ready_at && *j.retry_ready_at <= now) {
            j.state = JobState::Pending;
            j.retry_ready_at.reset();
        }
    }
}

Duration JobQueue::compute_backoff_locked(std::uint32_t attempt) {
    using ns = std::chrono::nanoseconds;
    const auto base_ns = std::chrono::duration_cast<ns>(cfg_.backoff_base).count();
    const auto cap_ns  = std::chrono::duration_cast<ns>(cfg_.backoff_cap).count();
    std::uint32_t exp = (attempt > 0) ? (attempt - 1u) : 0u;
    if (exp > 62u) exp = 62u;
    const std::uint64_t mult = 1ULL << exp;
    std::int64_t delay_ns;
    if (base_ns <= 0) {
        delay_ns = 0;
    } else {
        const auto base_u = static_cast<std::uint64_t>(base_ns);
        if (mult > std::numeric_limits<std::int64_t>::max() / base_u) {
            delay_ns = cap_ns;
        } else {
            delay_ns = static_cast<std::int64_t>(base_u * mult);
            if (delay_ns > cap_ns) delay_ns = cap_ns;
        }
    }
    if (cfg_.jitter_fraction > 0.0 && delay_ns > 0) {
        const double u = clock_->uniform01();
        const double jitter = static_cast<double>(delay_ns) * cfg_.jitter_fraction * u;
        delay_ns += static_cast<std::int64_t>(jitter);
    }
    return ns{delay_ns};
}

// ----------------------------- v1 observers + rollup -----------------------------

std::vector<DeadLetterEntry> JobQueue::dead_letter_snapshot() const {
    std::lock_guard<std::mutex> lock(mu_);
    std::vector<DeadLetterEntry> out;
    for (const auto& [_, ns] : namespaces_) {
        out.insert(out.end(), ns.dead_letter.begin(), ns.dead_letter.end());
    }
    return out;
}

std::size_t JobQueue::active_count() const {
    std::lock_guard<std::mutex> lock(mu_);
    std::size_t n = 0;
    for (const auto& j : jobs_) {
        if (j.state == JobState::Pending || j.state == JobState::RetryPending) ++n;
    }
    return n;
}

std::size_t JobQueue::leased_count() const {
    std::lock_guard<std::mutex> lock(mu_);
    return leased_count_locked();
}

Metrics JobQueue::metrics() const {
    std::lock_guard<std::mutex> lock(mu_);
    Metrics m;
    for (const auto& [_, ns] : namespaces_) {
        m.enqueued_total        += ns.counters.enqueued_total;
        m.acquired_total        += ns.counters.acquired_total;
        m.completed_total       += ns.counters.completed_total;
        m.failed_total          += ns.counters.failed_total;
        m.lease_expired_total   += ns.counters.lease_expired_total;
        m.dead_lettered_total   += ns.counters.dead_lettered_total;
        m.retry_scheduled_total += ns.counters.retry_scheduled_total;
        m.cancelled_total       += ns.counters.cancelled_total;
        m.promoted_total        += ns.counters.promoted_total;
        m.dead_letter_count     += ns.dead_letter.size();
    }
    for (const auto& j : jobs_) {
        if (j.state == JobState::Pending || j.state == JobState::RetryPending) ++m.active_count;
        else if (j.state == JobState::Leased) ++m.leased_count;
    }
    return m;
}

// ----------------------------- persist -----------------------------

void JobQueue::persist_locked() {
    PersistedState s;
    s.jobs                     = jobs_;
    s.next_id                  = next_id_;
    s.next_seq                 = next_seq_;
    s.next_event_id            = next_event_id_;
    s.oldest_retained_event_id = oldest_retained_event_id_;
    for (const auto& [name, ns] : namespaces_) {
        PersistedNamespace p;
        p.config      = ns.config;
        p.dead_letter = ns.dead_letter;
        p.counters    = ns.counters;
        s.namespaces.emplace(name, std::move(p));
    }
    s.workers   = workers_;
    s.cancelled = cancelled_;
    s.audit     = audit_;
    storage_->save(s);
}

} // namespace jobqueue
