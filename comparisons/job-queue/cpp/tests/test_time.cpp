#include "helpers.hpp"

using namespace jqtest;

TEST_CASE("scenario 29: ManualClock drives lease expiry and retry timing deterministically",
          "[time]") {
    Config cfg{
        .max_attempts    = 5,
        .backoff_base    = 250ms,
        .jitter_fraction = 0.0,
    };
    auto f = Fixture::make(cfg);
    auto id = expect_ok(f.queue->enqueue("a"));

    // Acquire and let the lease expire — exactly on the boundary.
    auto first = expect_ok(f.queue->acquire("w1", 1s));
    REQUIRE(first.id == id);
    f.clock->advance(999ms);
    REQUIRE(expect_err(f.queue->acquire("w2", 1s)) == AcquireErr::Empty);
    f.clock->advance(1ms); // total 1000ms: lease_expires_at <= now
    auto reclaimed = expect_ok(f.queue->acquire("w2", 1s));
    REQUIRE(reclaimed.id == id);

    // Fail and verify exact backoff boundary too (250ms).
    expect_ok(f.queue->fail("w2", id, "x"));
    f.clock->advance(249ms);
    REQUIRE(expect_err(f.queue->acquire("w3", 1s)) == AcquireErr::Empty);
    f.clock->advance(1ms); // total 250ms post-fail
    auto third = expect_ok(f.queue->acquire("w3", 1s));
    REQUIRE(third.id == id);
}

TEST_CASE("scenario 30: lease duration is measured from acquire_time; backoff from fail_time",
          "[time]") {
    Config cfg{
        .max_attempts    = 5,
        .backoff_base    = 1s,
        .jitter_fraction = 0.0,
    };
    auto f = Fixture::make(cfg);
    auto id = expect_ok(f.queue->enqueue("a"));

    // Acquire at t=0 with lease=2s. Lease should expire at t=2s.
    expect_ok(f.queue->acquire("w", 2s));
    f.clock->advance(1500ms); // t=1.5s, still leased
    // Fail at t=1.5s. Backoff = 1s, so available at t=2.5s — not at t=2s.
    expect_ok(f.queue->fail("w", id, "x"));
    f.clock->advance(500ms); // t=2s (when lease would have expired)
    REQUIRE(expect_err(f.queue->acquire("w", 1s)) == AcquireErr::Empty);
    f.clock->advance(499ms); // t=2.499s
    REQUIRE(expect_err(f.queue->acquire("w", 1s)) == AcquireErr::Empty);
    f.clock->advance(2ms);   // t=2.501s, backoff window passed
    auto retry = expect_ok(f.queue->acquire("w", 1s));
    REQUIRE(retry.id == id);
    REQUIRE(retry.attempt == 2);
}
