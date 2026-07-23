// Cancellation propagation through the dependency DAG plus the dead-letter
// transition. Kept separate from jobqueue.cpp because the BFS + audit-event
// pattern is the same shape for both code paths and benefits from sharing
// vocabulary in one place.

#include "jobqueue/jobqueue.hpp"

#include <algorithm>
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

} // namespace

std::vector<JobId> JobQueue::compute_cascade_locked(JobId seed) const {
    std::vector<JobId> result;
    std::unordered_set<JobId> visited;
    std::queue<JobId> bfs;
    bfs.push(seed);
    visited.insert(seed);
    result.push_back(seed);
    while (!bfs.empty()) {
        JobId cur = bfs.front();
        bfs.pop();
        for (const auto& j : jobs_) {
            if (is_terminal(j.state)) continue;
            if (std::find(j.depends_on.begin(), j.depends_on.end(), cur) ==
                j.depends_on.end()) continue;
            if (!visited.insert(j.id).second) continue;
            result.push_back(j.id);
            bfs.push(j.id);
        }
    }
    return result;
}

std::size_t JobQueue::apply_cancellation_locked(const std::vector<JobId>& set,
                                                const std::string& reason,
                                                TimePoint now) {
    if (set.empty()) return 0;
    const JobId origin = set.front();
    std::size_t count = 0;
    for (std::size_t i = 0; i < set.size(); ++i) {
        const JobId id = set[i];
        Job* j = find_locked(id);
        if (!j || is_terminal(j->state)) continue;
        j->state        = JobState::Cancelled;
        j->final_reason = reason;
        // Keep lease_holder as "previous holder" so a stray heartbeat from
        // the original holder reports LeaseExpired (not NotLeaseHolder).
        j->lease_expires_at.reset();
        j->retry_ready_at.reset();
        cancelled_.push_back(
            CancelledEntry{j->id, j->namespace_id, j->payload, reason});
        ns_locked(j->namespace_id).counters.cancelled_total += 1;
        const std::string payload = (i == 0)
            ? reason
            : "cascade from " + std::to_string(origin);
        emit_locked(AuditEventKind::Cancelled, id, std::nullopt, payload, now);
        ++count;
    }
    return count;
}

void JobQueue::dead_letter_locked(Job& job, std::string reason, TimePoint now) {
    auto& ns_state = ns_locked(job.namespace_id);
    const JobId job_id = job.id;
    job.state        = JobState::DeadLettered;
    job.final_reason = reason;
    job.lease_holder.reset();
    job.lease_expires_at.reset();
    ns_state.dead_letter.push_back(DeadLetterEntry{job.id, job.payload, reason});
    ns_state.counters.dead_lettered_total += 1;
    emit_locked(AuditEventKind::DeadLettered, job_id, std::nullopt, reason, now);

    // Rotate per-namespace dead-letter and prune the dropped jobs from the
    // canonical store. Their dependents have already cascade-cancelled.
    while (ns_state.dead_letter.size() > ns_state.config.dead_letter_capacity) {
        const JobId dropped_id = ns_state.dead_letter.front().id;
        ns_state.dead_letter.erase(ns_state.dead_letter.begin());
        auto it = std::find_if(jobs_.begin(), jobs_.end(),
                               [&](const Job& jj){ return jj.id == dropped_id; });
        if (it != jobs_.end() && it->state == JobState::DeadLettered) {
            jobs_.erase(it);
        }
    }

    // Cascade cancellation to dependents (the seed itself is already DL'd).
    std::vector<JobId> cascade;
    std::unordered_set<JobId> visited{job_id};
    std::queue<JobId> bfs;
    bfs.push(job_id);
    while (!bfs.empty()) {
        JobId cur = bfs.front();
        bfs.pop();
        for (const auto& j : jobs_) {
            if (is_terminal(j.state)) continue;
            if (std::find(j.depends_on.begin(), j.depends_on.end(), cur) ==
                j.depends_on.end()) continue;
            if (!visited.insert(j.id).second) continue;
            cascade.push_back(j.id);
            bfs.push(j.id);
        }
    }
    if (cascade.empty()) return;
    const std::string note = "cascade from " + std::to_string(job_id);
    for (JobId id : cascade) {
        Job* j = find_locked(id);
        if (!j || is_terminal(j->state)) continue;
        j->state        = JobState::Cancelled;
        j->final_reason = note;
        // (See apply_cancellation_locked) lease_holder is preserved so a
        // stray heartbeat from the original holder still reports LeaseExpired.
        j->lease_expires_at.reset();
        j->retry_ready_at.reset();
        cancelled_.push_back(
            CancelledEntry{j->id, j->namespace_id, j->payload, note});
        ns_locked(j->namespace_id).counters.cancelled_total += 1;
        emit_locked(AuditEventKind::Cancelled, id, std::nullopt, note, now);
    }
}

CancelResult JobQueue::cancel(JobId job_id, std::string reason) {
    std::lock_guard<std::mutex> lock(mu_);
    const auto now = clock_->now();
    sweep_locked(now);
    const Job* seed = find_locked(job_id);
    if (!seed || is_terminal(seed->state)) return CancelResult{0};
    auto cascade = compute_cascade_locked(job_id);
    std::size_t count = apply_cancellation_locked(cascade, reason, now);
    persist_locked();
    return CancelResult{count};
}

std::vector<CancelledEntry> JobQueue::cancelled_snapshot() const {
    std::lock_guard<std::mutex> lock(mu_);
    return cancelled_;
}

} // namespace jobqueue
