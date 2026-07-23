#include "helpers.hpp"

using namespace jqtest;

TEST_CASE("scenario 13: fail does NOT make the job immediately available", "[retry]") {
    auto f = Fixture::make(Config{
        .max_attempts = 5,
        .backoff_base = 1s,
        .backoff_cap  = 60s,
        .jitter_fraction = 0.0,
    });
    auto id = expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->acquire("w1", 10s));
    expect_ok(f.queue->fail("w1", id, "oops"));
    // Immediately after fail, with no time advance: still in retry-pending.
    REQUIRE(expect_err(f.queue->acquire("w2", 10s)) == AcquireErr::Empty);
}

TEST_CASE("scenario 14: after backoff elapses, the failed job is available with attempt+1",
          "[retry]") {
    auto f = Fixture::make(Config{
        .max_attempts    = 5,
        .backoff_base    = 1s,
        .backoff_cap     = 60s,
        .jitter_fraction = 0.0,
    });
    auto id = expect_ok(f.queue->enqueue("a"));
    auto first = expect_ok(f.queue->acquire("w1", 10s));
    REQUIRE(first.attempt == 1);
    expect_ok(f.queue->fail("w1", id, "oops"));
    // First retry uses 2^0 = 1s.
    f.clock->advance(999ms);
    REQUIRE(expect_err(f.queue->acquire("w2", 10s)) == AcquireErr::Empty);
    f.clock->advance(2ms);
    auto second = expect_ok(f.queue->acquire("w2", 10s));
    REQUIRE(second.id == id);
    REQUIRE(second.attempt == 2);
}

TEST_CASE("scenario 15: after fail max_attempts times the job moves to dead-letter", "[retry]") {
    Config cfg{
        .max_attempts    = 3,
        .backoff_base    = 1ms,
        .backoff_cap     = 1s,
        .jitter_fraction = 0.0,
    };
    auto f = Fixture::make(cfg);
    auto id = expect_ok(f.queue->enqueue("payload"));

    auto fail_once = [&](std::string_view reason) {
        auto ok = expect_ok(f.queue->acquire("w1", 10s));
        REQUIRE(ok.id == id);
        expect_ok(f.queue->fail("w1", id, std::string{reason}));
        f.clock->advance(10s); // let any backoff elapse
    };

    fail_once("first");
    fail_once("second");
    fail_once("third"); // attempt was 3; >= max_attempts -> dead-letter

    auto dl = f.queue->dead_letter_snapshot();
    REQUIRE(dl.size() == 1);
    REQUIRE(dl[0].id == id);
    REQUIRE(dl[0].payload == "payload");
    REQUIRE(dl[0].final_reason == "third");
    REQUIRE(expect_err(f.queue->acquire("w1", 10s)) == AcquireErr::Empty);
}

TEST_CASE("scenario 16: lease expiry alone does not increment attempt count", "[retry]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("a"));
    auto first = expect_ok(f.queue->acquire("w1", 100ms));
    REQUIRE(first.attempt == 1);
    f.clock->advance(200ms);
    auto second = expect_ok(f.queue->acquire("w2", 100ms));
    REQUIRE(second.id == id);
    REQUIRE(second.attempt == 1); // not incremented by lease expiry
}

TEST_CASE("scenario 17: backoff respects formula and jitter range", "[retry]") {
    // Run with no jitter first: delay should equal base * 2^(attempt-1).
    {
        Config cfg{
            .max_attempts    = 10,
            .backoff_base    = 100ms,
            .backoff_cap     = 60s,
            .jitter_fraction = 0.0,
        };
        auto f = Fixture::make(cfg);
        auto id = expect_ok(f.queue->enqueue("a"));
        // Attempt 1 -> delay 100ms.
        expect_ok(f.queue->acquire("w", 10s));
        expect_ok(f.queue->fail("w", id, "x"));
        f.clock->advance(99ms);
        REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
        f.clock->advance(2ms);
        auto a2 = expect_ok(f.queue->acquire("w", 10s));
        REQUIRE(a2.attempt == 2);
        // Attempt 2 -> delay 200ms.
        expect_ok(f.queue->fail("w", id, "x"));
        f.clock->advance(199ms);
        REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
        f.clock->advance(2ms);
        auto a3 = expect_ok(f.queue->acquire("w", 10s));
        REQUIRE(a3.attempt == 3);
        // Attempt 3 -> delay 400ms.
        expect_ok(f.queue->fail("w", id, "x"));
        f.clock->advance(399ms);
        REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
        f.clock->advance(2ms);
        auto a4 = expect_ok(f.queue->acquire("w", 10s));
        REQUIRE(a4.attempt == 4);
    }
    // Cap test: base=1s, cap=2s, attempt 5 -> 16s capped to 2s.
    {
        Config cfg{
            .max_attempts    = 10,
            .backoff_base    = 1s,
            .backoff_cap     = 2s,
            .jitter_fraction = 0.0,
        };
        auto f = Fixture::make(cfg);
        auto id = expect_ok(f.queue->enqueue("a"));
        // Burn through attempts to reach attempt=5.
        for (int i = 0; i < 4; ++i) {
            expect_ok(f.queue->acquire("w", 10s));
            expect_ok(f.queue->fail("w", id, "x"));
            f.clock->advance(60s); // skip past any backoff
        }
        auto a5 = expect_ok(f.queue->acquire("w", 10s));
        REQUIRE(a5.attempt == 5);
        expect_ok(f.queue->fail("w", id, "x"));
        // Cap is 2s; before that, not ready.
        f.clock->advance(1999ms);
        REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
        f.clock->advance(2ms);
        expect_ok(f.queue->acquire("w", 10s));
    }
    // Jitter test: max jitter (u=1.0) extends by exactly delay * jitter_fraction.
    {
        Config cfg{
            .max_attempts    = 10,
            .backoff_base    = 1s,
            .backoff_cap     = 60s,
            .jitter_fraction = 0.5,
        };
        auto f = Fixture::make(cfg);
        f.clock->set_uniform01(1.0); // worst-case jitter
        auto id = expect_ok(f.queue->enqueue("a"));
        expect_ok(f.queue->acquire("w", 10s));
        expect_ok(f.queue->fail("w", id, "x"));
        // Expected delay: 1s + 1s * 0.5 * 1.0 = 1500ms.
        f.clock->advance(1499ms);
        REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
        f.clock->advance(2ms);
        expect_ok(f.queue->acquire("w", 10s));
    }
    // Jitter u=0.0 means no jitter -> exact base.
    {
        Config cfg{
            .max_attempts    = 10,
            .backoff_base    = 1s,
            .backoff_cap     = 60s,
            .jitter_fraction = 0.5,
        };
        auto f = Fixture::make(cfg);
        f.clock->set_uniform01(0.0);
        auto id = expect_ok(f.queue->enqueue("a"));
        expect_ok(f.queue->acquire("w", 10s));
        expect_ok(f.queue->fail("w", id, "x"));
        f.clock->advance(999ms);
        REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
        f.clock->advance(2ms);
        expect_ok(f.queue->acquire("w", 10s));
    }
}
