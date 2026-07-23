#include "helpers.hpp"

using namespace jqtest;

TEST_CASE("scenario 46: high-priority scheduled-future job does NOT preempt a low-priority "
          "available job", "[interaction]") {
    auto f = Fixture::make();
    auto low_id = expect_ok(f.queue->enqueue("low", EnqueueOptions{.priority = 1}));
    expect_ok(f.queue->enqueue("high-future",
                                EnqueueOptions{
                                    .priority     = 10,
                                    .scheduled_at = f.clock->now() + 1h,
                                }));
    auto first = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(first.id == low_id); // priority comparison is over the acquirable set only
    REQUIRE(first.priority == 1);
}

TEST_CASE("scenario 47: a failed retry-scheduled job is acquirable at max(retry_ready_at, "
          "scheduled_at)", "[interaction]") {
    auto f = Fixture::make(Config{
        .max_attempts    = 5,
        .backoff_base    = 1s,
        .jitter_fraction = 0.0,
    });
    const auto t0 = f.clock->now();
    // Schedule 3s out; backoff after first fail is 1s.
    auto id = expect_ok(f.queue->enqueue("future",
                                          EnqueueOptions{.scheduled_at = t0 + 3s}));
    // First acquire happens at t=3s.
    f.clock->advance(3s);
    auto first = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(first.id == id);
    REQUIRE(first.attempt == 1);
    // Fail at t=3s. Backoff = 1s, so the retry alone would unlock at t=4s.
    expect_ok(f.queue->fail("w", id, "boom"));
    // But scheduled_at is t=3s (already past), so it is no longer the floor —
    // ready_time should be retry_ready_at = t=4s.
    f.clock->advance(999ms);
    REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
    f.clock->advance(2ms); // t=4.001s
    auto retry = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(retry.id == id);
    REQUIRE(retry.attempt == 2);
}

TEST_CASE("scenario 47b: scheduled_at dominates when retry_ready_at is earlier",
          "[interaction]") {
    auto f = Fixture::make(Config{
        .max_attempts    = 5,
        .backoff_base    = 1s,
        .jitter_fraction = 0.0,
    });
    const auto t0 = f.clock->now();
    // Schedule far out: scheduled_at = t=100s.
    auto id = expect_ok(f.queue->enqueue("far",
                                          EnqueueOptions{.scheduled_at = t0 + 100s}));
    f.clock->advance(100s);
    auto first = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(first.id == id);
    // Fail: backoff = 1s, so retry_ready_at = t=101s.
    expect_ok(f.queue->fail("w", id, "boom"));
    // scheduled_at is t=100s (past) — retry_ready_at wins at t=101s.
    f.clock->advance(999ms);
    REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
    f.clock->advance(2ms);
    expect_ok(f.queue->acquire("w", 10s));
}
