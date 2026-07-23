#pragma once

#include "types.hpp"

#include <cstdint>
#include <optional>
#include <string>

namespace jobqueue {

// Kinds of state transitions captured in the audit log. The set is fixed at
// v2.0; adding variants requires a storage codec version bump.
enum class AuditEventKind : std::uint8_t {
    Enqueued           = 0,
    Acquired           = 1,
    HeartbeatExtended  = 2,
    Completed          = 3,
    Failed             = 4,
    LeaseExpired       = 5,
    RetryScheduled     = 6,
    DeadLettered       = 7,
    Cancelled          = 8,
    WorkerRegistered   = 9,
    WorkerDeregistered = 10,
    Promoted           = 11,
};

// One immutable entry in the audit log. event_id is queue-lifetime monotonic
// (never resets across restart) and serves as the cursor for incremental
// tailing via audit_since().
struct AuditEvent {
    std::uint64_t event_id = 0;
    std::int64_t  at_nanos = 0;
    AuditEventKind kind    = AuditEventKind::Enqueued;
    std::optional<JobId> job_id;
    std::optional<std::string> worker_id;
    std::string payload;
};

enum class AuditErr {
    // The watermark passed to audit_since() is older than the oldest event
    // still in the bounded log. The caller has lost continuity and must
    // resync via audit_recent().
    AuditEventDropped,
};

} // namespace jobqueue
