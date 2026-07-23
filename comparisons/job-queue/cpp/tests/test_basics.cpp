#include "helpers.hpp"

using namespace jqtest;

TEST_CASE("scenario 1: acquire on empty queue returns Empty", "[basics]") {
    auto f = Fixture::make();
    auto r = f.queue->acquire("w1", 1s);
    REQUIRE(expect_err(r) == AcquireErr::Empty);
}

TEST_CASE("scenario 2: enqueue then acquire returns the enqueued payload", "[basics]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("hello"));
    auto ok = expect_ok(f.queue->acquire("w1", 1s));
    REQUIRE(ok.id == id);
    REQUIRE(ok.payload == "hello");
    REQUIRE(ok.attempt == 1);
}

TEST_CASE("scenario 3: after acquire, the job is not counted against active capacity", "[basics]") {
    auto f = Fixture::make(Config{.active_capacity = 1});
    auto id = expect_ok(f.queue->enqueue("a"));
    (void)id;
    REQUIRE(f.queue->active_count() == 1);
    auto ok = expect_ok(f.queue->acquire("w1", 1s));
    (void)ok;
    REQUIRE(f.queue->active_count() == 0);
    REQUIRE(f.queue->leased_count() == 1);
}

TEST_CASE("scenario 4: complete by lease holder removes the job permanently", "[basics]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("x"));
    auto ok = expect_ok(f.queue->acquire("w1", 1s));
    REQUIRE(ok.id == id);
    expect_ok(f.queue->complete("w1", id));
    // Job is gone. Subsequent acquire returns Empty.
    REQUIRE(expect_err(f.queue->acquire("w1", 1s)) == AcquireErr::Empty);
    // Even after a restart it should still be gone.
    auto q2 = std::make_unique<JobQueue>(Config{}, f.clock, f.storage);
    REQUIRE(expect_err(q2->acquire("w1", 1s)) == AcquireErr::Empty);
}

TEST_CASE("scenario 5: enqueue beyond active_capacity returns QueueFull", "[basics]") {
    auto f = Fixture::make(Config{.active_capacity = 2});
    expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->enqueue("b"));
    auto err = expect_err(f.queue->enqueue("c"));
    REQUIRE(err == EnqueueErr::QueueFull);
}
