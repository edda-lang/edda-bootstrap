#include "helpers.hpp"

#include <atomic>
#include <thread>
#include <vector>

using namespace jqtest;

TEST_CASE("scenario 41: enqueued_total increments on every successful enqueue",
          "[metrics]") {
    auto f = Fixture::make();
    REQUIRE(f.queue->metrics().enqueued_total == 0);
    expect_ok(f.queue->enqueue("a"));
    REQUIRE(f.queue->metrics().enqueued_total == 1);
    expect_ok(f.queue->enqueue("b"));
    expect_ok(f.queue->enqueue("c"));
    REQUIRE(f.queue->metrics().enqueued_total == 3);
}

TEST_CASE("scenario 42: enqueued_total does NOT increment on rejected enqueue",
          "[metrics]") {
    auto f = Fixture::make(Config{.active_capacity = 1});
    expect_ok(f.queue->enqueue("a"));
    REQUIRE(f.queue->metrics().enqueued_total == 1);
    // QueueFull rejection
    REQUIRE(expect_err(f.queue->enqueue("b")) == EnqueueErr::QueueFull);
    REQUIRE(f.queue->metrics().enqueued_total == 1);
    // InvalidPriority rejection
    REQUIRE(expect_err(f.queue->enqueue("c", EnqueueOptions{.priority = 99}))
            == EnqueueErr::InvalidPriority);
    REQUIRE(f.queue->metrics().enqueued_total == 1);
}

TEST_CASE("scenario 43: lease_expired_total increments on lease expiry",
          "[metrics]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("a"));
    (void)id;
    expect_ok(f.queue->acquire("w", 1s));
    REQUIRE(f.queue->metrics().lease_expired_total == 0);
    f.clock->advance(2s);
    // Trigger the sweep via another acquire on the same job.
    expect_ok(f.queue->acquire("w2", 1s));
    REQUIRE(f.queue->metrics().lease_expired_total == 1);
}

TEST_CASE("scenario 44: counters survive a restart with exact prior values",
          "[metrics]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    Config cfg{
        .max_attempts    = 3,
        .backoff_base    = 1ms,
        .jitter_fraction = 0.0,
    };
    Metrics before;
    {
        JobQueue q(cfg, clock, storage);
        auto id1 = expect_ok(q.enqueue("a"));
        auto id2 = expect_ok(q.enqueue("b"));
        expect_ok(q.acquire("w", 10s));
        expect_ok(q.complete("w", id1));
        expect_ok(q.acquire("w", 10s));
        expect_ok(q.fail("w", id2, "x")); // retry-pending
        clock->advance(10s);
        expect_ok(q.acquire("w", 10s));
        expect_ok(q.fail("w", id2, "x")); // retry-pending again (attempt 2)
        clock->advance(10s);
        expect_ok(q.acquire("w", 10s));
        expect_ok(q.fail("w", id2, "final")); // dead-letter (attempt 3)
        before = q.metrics();
    }
    JobQueue q2(cfg, clock, storage);
    auto after = q2.metrics();
    REQUIRE(after.enqueued_total        == before.enqueued_total);
    REQUIRE(after.acquired_total        == before.acquired_total);
    REQUIRE(after.completed_total       == before.completed_total);
    REQUIRE(after.failed_total          == before.failed_total);
    REQUIRE(after.lease_expired_total   == before.lease_expired_total);
    REQUIRE(after.dead_lettered_total   == before.dead_lettered_total);
    REQUIRE(after.retry_scheduled_total == before.retry_scheduled_total);
    // Sanity-check that the run actually moved counters.
    REQUIRE(after.enqueued_total      == 2);
    REQUIRE(after.completed_total     == 1);
    REQUIRE(after.failed_total        == 3);
    REQUIRE(after.dead_lettered_total == 1);
    REQUIRE(after.retry_scheduled_total == 2);
}

TEST_CASE("scenario 45: metrics snapshot is consistent under interleaved acquire/complete",
          "[metrics]") {
    auto f = Fixture::make(Config{.active_capacity = 1024});
    // Producer: enqueue many jobs.
    for (int i = 0; i < 100; ++i) expect_ok(f.queue->enqueue("j"));

    std::atomic<bool> stop{false};
    std::atomic<int> violations{0};

    // Observer reads metrics in a tight loop and asserts completed <= acquired.
    std::jthread observer([&] {
        while (!stop.load()) {
            auto m = f.queue->metrics();
            if (m.completed_total > m.acquired_total) violations.fetch_add(1);
        }
    });

    // Worker: acquire + complete pairs.
    std::vector<std::jthread> workers;
    for (int i = 0; i < 4; ++i) {
        workers.emplace_back([&] {
            for (int k = 0; k < 25; ++k) {
                auto r = f.queue->acquire("w", 10s);
                if (std::holds_alternative<AcquireOk>(r)) {
                    auto ok = std::get<AcquireOk>(r);
                    (void)f.queue->complete("w", ok.id);
                }
            }
        });
    }
    workers.clear(); // join workers
    stop.store(true);
    observer.join();

    REQUIRE(violations.load() == 0);

    auto m = f.queue->metrics();
    REQUIRE(m.completed_total <= m.acquired_total);
    REQUIRE(m.completed_total == 100);
    REQUIRE(m.acquired_total  == 100);
}

TEST_CASE("scenario 45b: dead_lettered_total stays at high-water even after rotation",
          "[metrics]") {
    Config cfg{
        .dead_letter_capacity = 1, // tiny so rotation kicks in immediately
        .max_attempts         = 1,
        .backoff_base         = 1ms,
        .jitter_fraction      = 0.0,
    };
    auto f = Fixture::make(cfg);
    for (int i = 0; i < 3; ++i) {
        auto id = expect_ok(f.queue->enqueue("j"));
        expect_ok(f.queue->acquire("w", 10s));
        expect_ok(f.queue->fail("w", id, "x"));
    }
    auto m = f.queue->metrics();
    REQUIRE(m.dead_lettered_total == 3);   // total transitions
    REQUIRE(m.dead_letter_count   == 1);   // current size after rotation
}
