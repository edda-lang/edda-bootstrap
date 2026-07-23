#pragma once

#include <chrono>
#include <cstdint>
#include <optional>
#include <string>
#include <variant>
#include <vector>

namespace jobqueue {

using JobId = std::uint64_t;
using Duration = std::chrono::nanoseconds;
// We pin TimePoint to nanosecond resolution so chrono arithmetic with our
// Duration type doesn't widen the time_point into an incompatible alias
// (MSVC's system_clock::duration is 100ns; libstdc++ uses nanoseconds —
// this avoids implicit-conversion errors across platforms).
using TimePoint = std::chrono::time_point<std::chrono::system_clock, Duration>;

// Reserved namespace used by v1.x callers that don't specify one.
inline constexpr const char* kDefaultNamespace = "default";

enum class JobState : std::uint8_t {
    Pending,
    Leased,
    RetryPending,
    DeadLettered,
    // v2: jobs reaching terminal states are kept in the canonical store so
    // dependents can resolve eligibility / propagation correctly.
    Completed,
    Cancelled,
};

// Priority bounds: 1..=10 inclusive, higher means more urgent.
// Default for unspecified enqueues is 5 (middle of range).
inline constexpr std::uint32_t kPriorityMin     = 1;
inline constexpr std::uint32_t kPriorityMax     = 10;
inline constexpr std::uint32_t kPriorityDefault = 5;

struct EnqueueOptions {
    std::uint32_t priority = kPriorityDefault;
    // If set, the job is not acquirable until clock reaches this point. The
    // job still counts against active_capacity from the moment of enqueue.
    std::optional<TimePoint> scheduled_at;
    // v2: job-level dependencies. Every entry must reference a known job and
    // must not create a cycle, or enqueue is rejected with InvalidDependency.
    std::vector<JobId> depends_on;
    // v2: workers acquiring this job must have a capability superset.
    std::vector<std::string> required_capabilities;
    // v2: per-job namespace. Capacity, dead-letter, and metrics are scoped
    // per namespace; default keeps v1.x behavior.
    std::string namespace_id = kDefaultNamespace;
};

struct Job {
    JobId id = 0;
    std::string namespace_id = kDefaultNamespace;
    std::string payload;
    std::uint32_t attempt = 1;
    std::uint32_t priority = kPriorityDefault;
    std::uint64_t enqueue_seq = 0;
    JobState state = JobState::Pending;
    std::optional<std::string> lease_holder;
    std::optional<TimePoint> lease_expires_at;
    std::optional<TimePoint> retry_ready_at;
    std::optional<TimePoint> scheduled_at;
    // Final reason — set on DeadLettered or Cancelled; carries the user's
    // explanation across the terminal transition and across restart.
    std::optional<std::string> final_reason;
    std::vector<JobId> depends_on;
    std::vector<std::string> required_capabilities;
};

struct DeadLetterEntry {
    JobId id = 0;
    std::string payload;
    std::string final_reason;
};

struct CancelledEntry {
    JobId id = 0;
    std::string namespace_id;
    std::string payload;
    std::string reason;
};

struct AcquireOk {
    JobId id;
    std::string payload;
    std::uint32_t attempt;
    std::uint32_t priority;
};

struct CancelResult {
    // Total jobs transitioned to Cancelled by this call (including the seed).
    // Zero when the seed is unknown, in a terminal state, or already cancelled.
    std::size_t count = 0;
};

// Per-namespace configuration. Created on first reference (queue-level
// defaults from Config) unless register_namespace() supplied one earlier.
struct NamespaceConfig {
    std::size_t active_capacity      = 1024;
    std::size_t dead_letter_capacity = 256;
};

enum class EnqueueErr {
    QueueFull,
    InvalidPriority,
    // v2: dependency list references a job we don't know about, would form
    // a cycle, or crosses namespaces.
    InvalidDependency,
};

enum class AcquireErr {
    Empty,
    // v2: worker_id is not registered (only enforced when the queue's
    // Config::require_worker_registration is true).
    UnknownWorker,
};

enum class HeartbeatErr { NotLeaseHolder, LeaseExpired };
enum class CompleteErr  { NotLeaseHolder };
enum class FailErr      { NotLeaseHolder };

enum class PromoteErr {
    UnknownJob,
    NotPending,
    InvalidPriority,
    NotPromotion, // new_priority <= current
};

struct Ok {};

template <typename T, typename E>
using Result = std::variant<T, E>;

struct Config {
    // Queue-level defaults used to seed any namespace that hasn't been
    // explicitly registered. Per-namespace registrations override these.
    std::size_t active_capacity      = 1024;
    std::size_t dead_letter_capacity = 256;
    std::uint32_t max_attempts       = 3;
    Duration backoff_base            = std::chrono::milliseconds{100};
    Duration backoff_cap             = std::chrono::seconds{30};
    double jitter_fraction           = 0.0;
    Duration default_lease_duration  = std::chrono::seconds{30};
    // v2: audit log retention bound. Older events are dropped (FIFO).
    std::size_t audit_retention      = 10000;
    // v2: opt-in worker registration. When false (default), acquire accepts
    // any worker_id — keeps v1.x callers running unchanged.
    bool require_worker_registration = false;
};

} // namespace jobqueue
