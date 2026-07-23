#include "helpers.hpp"

#include <atomic>
#include <thread>
#include <vector>

using namespace jqtest;

TEST_CASE("scenario 22: N producers concurrently - total accepted = min(N, capacity)",
          "[concurrency]") {
    constexpr int N = 64;
    constexpr std::size_t cap = 25;
    auto f = Fixture::make(Config{.active_capacity = cap});

    std::atomic<int> accepted{0};
    std::vector<std::jthread> threads;
    threads.reserve(N);
    for (int i = 0; i < N; ++i) {
        threads.emplace_back([&, i] {
            auto r = f.queue->enqueue("p" + std::to_string(i));
            if (std::holds_alternative<JobId>(r)) {
                accepted.fetch_add(1);
            }
        });
    }
    threads.clear(); // join all
    REQUIRE(static_cast<std::size_t>(accepted.load()) == cap);
    REQUIRE(f.queue->active_count() == cap);
}

TEST_CASE("scenario 23: M workers concurrently acquire on K available jobs - exactly K succeed",
          "[concurrency]") {
    constexpr int M = 32;
    constexpr int K = 10;
    auto f = Fixture::make(Config{.active_capacity = K});
    for (int i = 0; i < K; ++i) expect_ok(f.queue->enqueue("j" + std::to_string(i)));

    std::atomic<int> succeeded{0};
    std::atomic<int> empties{0};
    std::vector<std::jthread> threads;
    threads.reserve(M);
    for (int i = 0; i < M; ++i) {
        threads.emplace_back([&, i] {
            auto r = f.queue->acquire("w" + std::to_string(i), 30s);
            if (std::holds_alternative<AcquireOk>(r)) succeeded.fetch_add(1);
            else                                       empties.fetch_add(1);
        });
    }
    threads.clear();
    REQUIRE(succeeded.load() == K);
    REQUIRE(empties.load() == M - K);
}

TEST_CASE("scenario 24: heartbeat racing with lease-expiry check has consistent outcome",
          "[concurrency]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("j"));
    expect_ok(f.queue->acquire("w1", 100ms));
    // Move clock right to the expiry boundary.
    f.clock->advance(100ms);

    std::atomic<bool> hb_ok{false};
    std::atomic<bool> hb_expired{false};
    std::atomic<bool> sweep_ran{false};

    // Thread A: heartbeat.
    // Thread B: enqueue (triggers sweep via the active-capacity check).
    std::jthread a([&] {
        auto r = f.queue->heartbeat("w1", id, 1s);
        if (std::holds_alternative<Ok>(r)) hb_ok.store(true);
        else if (std::get<HeartbeatErr>(r) == HeartbeatErr::LeaseExpired) hb_expired.store(true);
    });
    std::jthread b([&] {
        (void)f.queue->enqueue("trigger-sweep");
        sweep_ran.store(true);
    });
    a.join();
    b.join();

    REQUIRE(sweep_ran.load());
    // Exactly one of {ok, LeaseExpired} must hold; never both, never neither.
    REQUIRE((hb_ok.load() ^ hb_expired.load()));

    if (hb_ok.load()) {
        // Heartbeat won: job is still leased to w1, not available to acquire.
        REQUIRE(expect_err(f.queue->acquire("w2", 1s)) == AcquireErr::Empty);
    } else {
        // Sweep won + heartbeat saw expiry: job is back in the queue.
        auto ok = expect_ok(f.queue->acquire("w2", 1s));
        REQUIRE(ok.id == id);
    }
}

TEST_CASE("scenario 25: complete racing with fail on the same job - only the first succeeds",
          "[concurrency]") {
    // Repeat many times to stress the race.
    for (int iter = 0; iter < 200; ++iter) {
        auto f = Fixture::make(Config{
            .max_attempts    = 5,
            .backoff_base    = 1s,
            .jitter_fraction = 0.0,
        });
        auto id = expect_ok(f.queue->enqueue("racey"));
        expect_ok(f.queue->acquire("w1", 30s));

        std::atomic<int> complete_ok{0};
        std::atomic<int> complete_err{0};
        std::atomic<int> fail_ok{0};
        std::atomic<int> fail_err{0};

        {
            std::jthread t1([&] {
                auto r = f.queue->complete("w1", id);
                if (std::holds_alternative<Ok>(r)) complete_ok.fetch_add(1);
                else                                complete_err.fetch_add(1);
            });
            std::jthread t2([&] {
                auto r = f.queue->fail("w1", id, "racey");
                if (std::holds_alternative<Ok>(r)) fail_ok.fetch_add(1);
                else                                fail_err.fetch_add(1);
            });
        }

        // Exactly one call succeeded.
        REQUIRE((complete_ok.load() + fail_ok.load()) == 1);
        REQUIRE((complete_err.load() + fail_err.load()) == 1);
    }
}
