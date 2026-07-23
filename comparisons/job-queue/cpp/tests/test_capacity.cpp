#include "helpers.hpp"

using namespace jqtest;

TEST_CASE("scenario 26: dead-letter beyond capacity drops the oldest entry", "[capacity]") {
    Config cfg{
        .dead_letter_capacity = 2,
        .max_attempts         = 1,
        .backoff_base         = 1ms,
        .jitter_fraction      = 0.0,
    };
    auto f = Fixture::make(cfg);
    JobId id1 = expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->acquire("w", 10s));
    expect_ok(f.queue->fail("w", id1, "r1"));

    JobId id2 = expect_ok(f.queue->enqueue("b"));
    expect_ok(f.queue->acquire("w", 10s));
    expect_ok(f.queue->fail("w", id2, "r2"));

    JobId id3 = expect_ok(f.queue->enqueue("c"));
    expect_ok(f.queue->acquire("w", 10s));
    expect_ok(f.queue->fail("w", id3, "r3"));

    auto dl = f.queue->dead_letter_snapshot();
    REQUIRE(dl.size() == 2);
    // Oldest (id1) was dropped.
    REQUIRE(dl[0].id == id2);
    REQUIRE(dl[1].id == id3);
}

TEST_CASE("scenario 27: retry-pending job counts against active_capacity", "[capacity]") {
    Config cfg{
        .active_capacity = 2,
        .max_attempts    = 5,
        .backoff_base    = 60s,        // long backoff so it stays retry-pending
        .jitter_fraction = 0.0,
    };
    auto f = Fixture::make(cfg);
    auto id1 = expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->enqueue("b"));
    expect_ok(f.queue->acquire("w", 30s));
    expect_ok(f.queue->fail("w", id1, "x")); // a is now retry-pending
    // Active is now 2 (one retry-pending + one pending). Enqueue must reject.
    REQUIRE(expect_err(f.queue->enqueue("c")) == EnqueueErr::QueueFull);
}

TEST_CASE("scenario 28: in-flight (leased) job does NOT count against active_capacity",
          "[capacity]") {
    Config cfg{.active_capacity = 2};
    auto f = Fixture::make(cfg);
    expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->enqueue("b"));
    expect_ok(f.queue->acquire("w", 30s)); // a is now leased
    // Active is back to 1; we can enqueue another.
    expect_ok(f.queue->enqueue("c"));
    REQUIRE(f.queue->active_count() == 2);
    REQUIRE(f.queue->leased_count() == 1);
}
