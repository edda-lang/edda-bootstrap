#pragma once

#include "audit.hpp"
#include "metrics.hpp"
#include "types.hpp"
#include "worker.hpp"

#include <deque>
#include <filesystem>
#include <mutex>
#include <optional>
#include <unordered_map>
#include <vector>

namespace jobqueue {

// Per-namespace state persisted alongside the canonical job store. Each
// namespace owns its own capacity/dead-letter capacity, dead-letter ring, and
// metrics counters; the queue-wide rollup is computed by summation.
struct PersistedNamespace {
    NamespaceConfig config;
    std::vector<DeadLetterEntry> dead_letter;
    Metrics counters;
};

struct PersistedState {
    std::vector<Job> jobs;
    JobId next_id          = 1;
    std::uint64_t next_seq = 0;
    // v2 — namespace registry (config + per-ns dead_letter + per-ns counters).
    std::unordered_map<std::string, PersistedNamespace> namespaces;
    // v2 — registered workers, queue-wide.
    std::unordered_map<std::string, Worker> workers;
    // v2 — chronological cancelled-job log (queue-wide).
    std::vector<CancelledEntry> cancelled;
    // v2 — bounded audit log (queue-wide chronological deque).
    std::deque<AuditEvent> audit;
    std::uint64_t next_event_id            = 1;
    std::uint64_t oldest_retained_event_id = 1;
};

class Storage {
public:
    virtual ~Storage() = default;
    virtual void save(const PersistedState& state) = 0;
    virtual std::optional<PersistedState> load() = 0;
};

class NullStorage final : public Storage {
public:
    void save(const PersistedState&) override {}
    std::optional<PersistedState> load() override { return std::nullopt; }
};

class MemoryStorage final : public Storage {
public:
    void save(const PersistedState& state) override;
    std::optional<PersistedState> load() override;

private:
    std::mutex mu_;
    std::optional<PersistedState> state_;
};

class FileStorage final : public Storage {
public:
    explicit FileStorage(std::filesystem::path path);

    void save(const PersistedState& state) override;
    std::optional<PersistedState> load() override;

private:
    std::filesystem::path path_;
    std::filesystem::path tmp_path_;
    std::mutex mu_;
};

} // namespace jobqueue
