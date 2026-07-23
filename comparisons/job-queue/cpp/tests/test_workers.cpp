#include "helpers.hpp"

#include <algorithm>

using namespace jqtest;

TEST_CASE("scenario 116: register, list, acquire", "[workers]") {
    auto f = Fixture::make(Config{.require_worker_registration = true});
    f.queue->register_worker("w1", {"x", "y"});
    auto listed = f.queue->list_workers();
    REQUIRE(listed.size() == 1);
    REQUIRE(listed[0].id == "w1");
    REQUIRE(listed[0].capabilities == std::vector<std::string>{"x", "y"});

    expect_ok(f.queue->enqueue("any"));
    auto ok = expect_ok(f.queue->acquire("w1", 10s));
    REQUIRE(ok.payload == "any");
}

TEST_CASE("scenario 117: acquire with unknown worker -> UnknownWorker", "[workers]") {
    auto f = Fixture::make(Config{.require_worker_registration = true});
    expect_ok(f.queue->enqueue("job"));
    auto err = expect_err(f.queue->acquire("ghost", 10s));
    REQUIRE(err == AcquireErr::UnknownWorker);
}

TEST_CASE("scenario 118: capability superset matching succeeds", "[workers]") {
    auto f = Fixture::make(Config{.require_worker_registration = true});
    f.queue->register_worker("w1", {"gpu", "tpu", "fpu"});
    expect_ok(f.queue->enqueue("crunch",
                                EnqueueOptions{.required_capabilities = {"gpu", "tpu"}}));
    auto ok = expect_ok(f.queue->acquire("w1", 10s));
    REQUIRE(ok.payload == "crunch");
}

TEST_CASE("scenario 119: capability shortfall -> Empty (no acquirable job for this worker)",
          "[workers]") {
    auto f = Fixture::make(Config{.require_worker_registration = true});
    f.queue->register_worker("w_general", {});
    f.queue->register_worker("w_gpu", {"gpu"});
    expect_ok(f.queue->enqueue("crunch",
                                EnqueueOptions{.required_capabilities = {"gpu"}}));
    // No-cap worker cannot acquire the gpu job.
    REQUIRE(expect_err(f.queue->acquire("w_general", 10s)) == AcquireErr::Empty);
    // GPU-capable worker can.
    expect_ok(f.queue->acquire("w_gpu", 10s));
}

TEST_CASE("scenario 120: deregister while holding 2 leases -> 2 jobs returned, attempt unchanged",
          "[workers]") {
    auto f = Fixture::make(Config{.require_worker_registration = true});
    f.queue->register_worker("w1", {});
    f.queue->register_worker("w2", {});
    expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->enqueue("b"));
    auto first  = expect_ok(f.queue->acquire("w1", 10s));
    auto second = expect_ok(f.queue->acquire("w1", 10s));
    REQUIRE(first.attempt == 1);
    REQUIRE(second.attempt == 1);

    auto released = f.queue->deregister_worker("w1");
    REQUIRE(released == 2);
    // Both jobs are back in the active set; attempt counts are still 1.
    auto a = expect_ok(f.queue->acquire("w2", 10s));
    auto b = expect_ok(f.queue->acquire("w2", 10s));
    REQUIRE(a.attempt == 1);
    REQUIRE(b.attempt == 1);
}

TEST_CASE("scenario 121: re-register with new capabilities replaces old set", "[workers]") {
    auto f = Fixture::make(Config{.require_worker_registration = true});
    f.queue->register_worker("w", {"old"});
    f.queue->register_worker("w", {"new", "shiny"});
    auto listed = f.queue->list_workers();
    REQUIRE(listed.size() == 1);
    REQUIRE(listed[0].capabilities == std::vector<std::string>{"new", "shiny"});
}

TEST_CASE("scenario 122: workers survive restart with capabilities intact", "[workers]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    Config cfg{.require_worker_registration = true};
    {
        JobQueue q(cfg, clock, storage);
        q.register_worker("w1", {"a", "b"});
        q.register_worker("w2", {});
    }
    JobQueue q2(cfg, clock, storage);
    auto listed = q2.list_workers();
    REQUIRE(listed.size() == 2);
    std::sort(listed.begin(), listed.end(),
              [](const WorkerView& x, const WorkerView& y){ return x.id < y.id; });
    REQUIRE(listed[0].id == "w1");
    REQUIRE(listed[0].capabilities == std::vector<std::string>{"a", "b"});
    REQUIRE(listed[1].id == "w2");
    REQUIRE(listed[1].capabilities.empty());
}

TEST_CASE("scenario 123: deregistering an unknown worker is a no-op", "[workers]") {
    auto f = Fixture::make();
    auto released = f.queue->deregister_worker("nobody");
    REQUIRE(released == 0);
    REQUIRE(f.queue->list_workers().empty());
}
