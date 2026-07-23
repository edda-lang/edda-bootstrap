#include "helpers.hpp"

using namespace jqtest;

TEST_CASE("scenario 36: job scheduled in the future is not returned by acquire before scheduled_at",
          "[scheduled]") {
    auto f = Fixture::make();
    const auto t0 = f.clock->now();
    auto id = expect_ok(f.queue->enqueue("future",
                                          EnqueueOptions{.scheduled_at = t0 + 5s}));
    (void)id;
    // No clock advance: not acquirable.
    REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
    f.clock->advance(4999ms);
    REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
}

TEST_CASE("scenario 37: clock advanced past scheduled_at; next acquire returns the job",
          "[scheduled]") {
    auto f = Fixture::make();
    const auto t0 = f.clock->now();
    auto id = expect_ok(f.queue->enqueue("future",
                                          EnqueueOptions{.scheduled_at = t0 + 5s}));
    f.clock->advance(5s);
    auto ok = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(ok.id == id);
}

TEST_CASE("scenario 38: scheduled job counts against active_capacity from enqueue",
          "[scheduled]") {
    auto f = Fixture::make(Config{.active_capacity = 1});
    const auto t0 = f.clock->now();
    expect_ok(f.queue->enqueue("future",
                                EnqueueOptions{.scheduled_at = t0 + 1h}));
    // Capacity is now full, even though no job is acquirable.
    REQUIRE(f.queue->active_count() == 1);
    REQUIRE(expect_err(f.queue->enqueue("immediate")) == EnqueueErr::QueueFull);
    REQUIRE(expect_err(f.queue->acquire("w", 1s)) == AcquireErr::Empty);
}

TEST_CASE("scenario 39: scheduled jobs of equal scheduled_at preserve FIFO once unlocked",
          "[scheduled]") {
    auto f = Fixture::make();
    const auto due = f.clock->now() + 1s;
    auto a = expect_ok(f.queue->enqueue("a", EnqueueOptions{.scheduled_at = due}));
    auto b = expect_ok(f.queue->enqueue("b", EnqueueOptions{.scheduled_at = due}));
    auto c = expect_ok(f.queue->enqueue("c", EnqueueOptions{.scheduled_at = due}));
    f.clock->advance(1s);
    REQUIRE(expect_ok(f.queue->acquire("w", 10s)).id == a);
    REQUIRE(expect_ok(f.queue->acquire("w", 10s)).id == b);
    REQUIRE(expect_ok(f.queue->acquire("w", 10s)).id == c);
}

TEST_CASE("scenario 40: scheduled job persists across restart with scheduled_at intact",
          "[scheduled]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    const auto t0 = clock->now();
    JobId id;
    {
        JobQueue q(Config{}, clock, storage);
        id = expect_ok(q.enqueue("later",
                                  EnqueueOptions{.scheduled_at = t0 + 10s}));
    }
    JobQueue q2(Config{}, clock, storage);
    // Pre-due: still not acquirable.
    REQUIRE(expect_err(q2.acquire("w", 1s)) == AcquireErr::Empty);
    // Advance past scheduled_at and the job becomes acquirable.
    clock->advance(10s);
    auto ok = expect_ok(q2.acquire("w", 1s));
    REQUIRE(ok.id == id);
}
