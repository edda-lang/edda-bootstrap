#pragma once

#include <cstddef>
#include <cstdint>

namespace jobqueue {

// Snapshot of queue metrics returned by JobQueue::metrics(). Counters are
// monotonically increasing for the lifetime of the queue and survive
// restart; gauges are computed at snapshot time. The snapshot itself is
// taken atomically under the queue lock.
struct Metrics {
    // Counters — never decrease. Persisted across restart.
    std::uint64_t enqueued_total        = 0;
    std::uint64_t acquired_total        = 0;
    std::uint64_t completed_total       = 0;
    std::uint64_t failed_total          = 0;
    std::uint64_t lease_expired_total   = 0;
    std::uint64_t dead_lettered_total   = 0;
    std::uint64_t retry_scheduled_total = 0;
    // v2 — cancellation and promotion transitions.
    std::uint64_t cancelled_total       = 0;
    std::uint64_t promoted_total        = 0;

    // Gauges — instantaneous values, not persisted.
    std::size_t active_count      = 0;
    std::size_t leased_count      = 0;
    std::size_t dead_letter_count = 0;
};

} // namespace jobqueue
