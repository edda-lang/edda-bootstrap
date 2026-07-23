#include "helpers.hpp"

using namespace jqtest;

TEST_CASE("scenario 31: high-priority job enqueued AFTER low-priority is acquired first",
          "[priority]") {
    auto f = Fixture::make();
    auto low_id  = expect_ok(f.queue->enqueue("low",  EnqueueOptions{.priority = 2}));
    auto high_id = expect_ok(f.queue->enqueue("high", EnqueueOptions{.priority = 9}));
    auto first  = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(first.id == high_id);
    REQUIRE(first.priority == 9);
    auto second = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(second.id == low_id);
    REQUIRE(second.priority == 2);
}

TEST_CASE("scenario 32: equal-priority jobs preserve FIFO order", "[priority]") {
    auto f = Fixture::make();
    auto a = expect_ok(f.queue->enqueue("a", EnqueueOptions{.priority = 5}));
    auto b = expect_ok(f.queue->enqueue("b", EnqueueOptions{.priority = 5}));
    auto c = expect_ok(f.queue->enqueue("c", EnqueueOptions{.priority = 5}));
    REQUIRE(expect_ok(f.queue->acquire("w", 10s)).id == a);
    REQUIRE(expect_ok(f.queue->acquire("w", 10s)).id == b);
    REQUIRE(expect_ok(f.queue->acquire("w", 10s)).id == c);
}

TEST_CASE("scenario 33: out-of-range priority on enqueue is rejected", "[priority]") {
    auto f = Fixture::make();
    REQUIRE(expect_err(f.queue->enqueue("x", EnqueueOptions{.priority = 0}))  == EnqueueErr::InvalidPriority);
    REQUIRE(expect_err(f.queue->enqueue("x", EnqueueOptions{.priority = 11})) == EnqueueErr::InvalidPriority);
    REQUIRE(expect_err(f.queue->enqueue("x", EnqueueOptions{.priority = 999})) == EnqueueErr::InvalidPriority);
    // Boundary values 1 and 10 are accepted.
    expect_ok(f.queue->enqueue("ok-min", EnqueueOptions{.priority = 1}));
    expect_ok(f.queue->enqueue("ok-max", EnqueueOptions{.priority = 10}));
}

TEST_CASE("scenario 34: priority survives retry", "[priority]") {
    auto f = Fixture::make(Config{
        .max_attempts    = 5,
        .backoff_base    = 1s,
        .jitter_fraction = 0.0,
    });
    auto id = expect_ok(f.queue->enqueue("p7", EnqueueOptions{.priority = 7}));
    auto first = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(first.priority == 7);
    expect_ok(f.queue->fail("w", id, "boom"));
    f.clock->advance(2s);
    auto retry = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(retry.id == id);
    REQUIRE(retry.priority == 7);
    REQUIRE(retry.attempt == 2);
}

TEST_CASE("scenario 35: priority survives restart", "[priority]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    JobId id1, id2;
    {
        JobQueue q(Config{}, clock, storage);
        id1 = expect_ok(q.enqueue("low-prio",  EnqueueOptions{.priority = 1}));
        id2 = expect_ok(q.enqueue("high-prio", EnqueueOptions{.priority = 10}));
    }
    JobQueue q2(Config{}, clock, storage);
    auto first = expect_ok(q2.acquire("w", 10s));
    REQUIRE(first.id == id2);
    REQUIRE(first.priority == 10);
    auto second = expect_ok(q2.acquire("w", 10s));
    REQUIRE(second.id == id1);
    REQUIRE(second.priority == 1);
}

TEST_CASE("scenario 35b: default priority is 5", "[priority]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("default"));
    auto ok = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(ok.id == id);
    REQUIRE(ok.priority == 5);
}
