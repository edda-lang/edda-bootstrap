// Administrative surface added in v2: worker registry, namespace registry,
// promotion, and the audit-log readers. Each method holds the queue mutex
// for the same reason the core ops do — operations on these registries
// interleave with job-state transitions and must be linearizable with them.

#include "jobqueue/jobqueue.hpp"

#include <algorithm>
#include <string>
#include <utility>

namespace jobqueue {

// ----------------------------- workers -----------------------------

void JobQueue::register_worker(std::string id, std::vector<std::string> capabilities) {
    std::lock_guard<std::mutex> lock(mu_);
    const auto now = clock_->now();
    auto it = workers_.find(id);
    if (it != workers_.end() && it->second.capabilities == capabilities) return;
    workers_[id] = Worker{id, std::move(capabilities)};
    emit_locked(AuditEventKind::WorkerRegistered, std::nullopt, id, "", now);
    persist_locked();
}

std::size_t JobQueue::deregister_worker(std::string_view id) {
    std::lock_guard<std::mutex> lock(mu_);
    const auto now = clock_->now();
    auto it = workers_.find(std::string{id});
    if (it == workers_.end()) return 0;
    // Force-expire held leases. Same semantics as natural expiry: attempt
    // count is NOT incremented and the job returns to the active set.
    std::size_t released = 0;
    for (auto& j : jobs_) {
        if (j.state == JobState::Leased && j.lease_holder && *j.lease_holder == id) {
            j.state = JobState::Pending;
            j.lease_expires_at.reset();
            ns_locked(j.namespace_id).counters.lease_expired_total += 1;
            emit_locked(AuditEventKind::LeaseExpired, j.id, std::string{id},
                        "worker deregistered", now);
            ++released;
        }
    }
    workers_.erase(it);
    emit_locked(AuditEventKind::WorkerDeregistered, std::nullopt, std::string{id},
                "released=" + std::to_string(released), now);
    persist_locked();
    return released;
}

std::vector<WorkerView> JobQueue::list_workers() const {
    std::lock_guard<std::mutex> lock(mu_);
    std::vector<WorkerView> out;
    out.reserve(workers_.size());
    for (const auto& [_, w] : workers_) {
        out.push_back(WorkerView{w.id, w.capabilities});
    }
    return out;
}

// ----------------------------- namespaces -----------------------------

void JobQueue::register_namespace(std::string name, NamespaceConfig config) {
    std::lock_guard<std::mutex> lock(mu_);
    auto& s = ns_locked(name);
    s.config = config;
    persist_locked();
}

std::vector<std::string> JobQueue::list_namespaces() const {
    std::lock_guard<std::mutex> lock(mu_);
    std::vector<std::string> out;
    out.reserve(namespaces_.size());
    for (const auto& [name, _] : namespaces_) out.push_back(name);
    std::sort(out.begin(), out.end());
    return out;
}

std::unordered_map<std::string, Metrics> JobQueue::metrics_per_namespace() const {
    std::lock_guard<std::mutex> lock(mu_);
    std::unordered_map<std::string, Metrics> out;
    out.reserve(namespaces_.size());
    for (const auto& [name, ns] : namespaces_) {
        Metrics m = ns.counters;
        m.dead_letter_count = ns.dead_letter.size();
        for (const auto& j : jobs_) {
            if (j.namespace_id != name) continue;
            if (j.state == JobState::Pending || j.state == JobState::RetryPending) {
                ++m.active_count;
            } else if (j.state == JobState::Leased) {
                ++m.leased_count;
            }
        }
        out.emplace(name, m);
    }
    return out;
}

// ----------------------------- promotion -----------------------------

Result<Ok, PromoteErr> JobQueue::promote(JobId job_id, std::uint32_t new_priority) {
    if (new_priority < kPriorityMin || new_priority > kPriorityMax) {
        return PromoteErr::InvalidPriority;
    }
    std::lock_guard<std::mutex> lock(mu_);
    const auto now = clock_->now();
    sweep_locked(now);
    Job* j = find_locked(job_id);
    if (!j) return PromoteErr::UnknownJob;
    if (j->state != JobState::Pending) return PromoteErr::NotPending;
    if (new_priority <= j->priority) return PromoteErr::NotPromotion;
    const std::uint32_t old_priority = j->priority;
    j->priority = new_priority;
    ns_locked(j->namespace_id).counters.promoted_total += 1;
    emit_locked(AuditEventKind::Promoted, job_id, std::nullopt,
                std::to_string(old_priority) + "->" + std::to_string(new_priority),
                now);
    persist_locked();
    return Ok{};
}

// ----------------------------- audit readers -----------------------------

std::vector<AuditEvent> JobQueue::audit_recent(std::size_t limit) const {
    std::lock_guard<std::mutex> lock(mu_);
    std::vector<AuditEvent> out;
    if (limit == 0 || audit_.empty()) return out;
    const std::size_t take = std::min(limit, audit_.size());
    out.reserve(take);
    for (auto it = audit_.end() - static_cast<std::ptrdiff_t>(take);
         it != audit_.end(); ++it) {
        out.push_back(*it);
    }
    return out;
}

Result<std::vector<AuditEvent>, AuditErr>
JobQueue::audit_since(std::uint64_t after) const {
    std::lock_guard<std::mutex> lock(mu_);
    // The caller's watermark is `after`; they want everything strictly
    // greater. If `after + 1` is older than the oldest event we still hold,
    // they've fallen off the bounded log.
    if (after + 1 < oldest_retained_event_id_) {
        return AuditErr::AuditEventDropped;
    }
    std::vector<AuditEvent> out;
    for (const auto& e : audit_) if (e.event_id > after) out.push_back(e);
    return out;
}

} // namespace jobqueue
