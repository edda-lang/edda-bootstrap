#include "helpers.hpp"

#include <set>

using namespace jqtest;

TEST_CASE("scenario 6: two workers cannot acquire the same job", "[lease]") {
    auto f = Fixture::make();
    expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->enqueue("b"));
    auto a = expect_ok(f.queue->acquire("w1", 10s));
    auto b = expect_ok(f.queue->acquire("w2", 10s));
    REQUIRE(a.id != b.id);
    // Third acquire returns Empty because both jobs are leased.
    REQUIRE(expect_err(f.queue->acquire("w3", 10s)) == AcquireErr::Empty);
}

TEST_CASE("scenario 7: after lease_duration passes with no heartbeat, the job is available again",
          "[lease]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("a"));
    auto first = expect_ok(f.queue->acquire("w1", 1s));
    REQUIRE(first.id == id);
    // Before expiry: still leased.
    f.clock->advance(500ms);
    REQUIRE(expect_err(f.queue->acquire("w2", 1s)) == AcquireErr::Empty);
    // After expiry: available again.
    f.clock->advance(600ms);
    auto reclaimed = expect_ok(f.queue->acquire("w2", 1s));
    REQUIRE(reclaimed.id == id);
    REQUIRE(reclaimed.attempt == 1); // lease expiry does not bump attempt
}

TEST_CASE("scenario 8: heartbeat before lease expiry keeps the job leased", "[lease]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->acquire("w1", 1s));
    f.clock->advance(700ms);
    expect_ok(f.queue->heartbeat("w1", id, 1s));
    f.clock->advance(700ms); // total 1.4s from acquire, but heartbeat extended
    REQUIRE(expect_err(f.queue->acquire("w2", 1s)) == AcquireErr::Empty);
}

TEST_CASE("scenario 9: heartbeat after lease expiry returns LeaseExpired and does not re-lease",
          "[lease]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->acquire("w1", 1s));
    f.clock->advance(2s);
    auto err = expect_err(f.queue->heartbeat("w1", id, 10s));
    REQUIRE(err == HeartbeatErr::LeaseExpired);
    // Job is available — heartbeat did not extend.
    auto reclaimed = expect_ok(f.queue->acquire("w2", 10s));
    REQUIRE(reclaimed.id == id);
}

TEST_CASE("scenario 10: complete by a non-holder returns NotLeaseHolder", "[lease]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->acquire("w1", 10s));
    REQUIRE(expect_err(f.queue->complete("w2", id)) == CompleteErr::NotLeaseHolder);
}

TEST_CASE("scenario 11: fail by a non-holder returns NotLeaseHolder", "[lease]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->acquire("w1", 10s));
    REQUIRE(expect_err(f.queue->fail("w2", id, "nope")) == FailErr::NotLeaseHolder);
}

TEST_CASE("scenario 12: heartbeat by a non-holder returns NotLeaseHolder", "[lease]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->acquire("w1", 10s));
    REQUIRE(expect_err(f.queue->heartbeat("w2", id, 5s)) == HeartbeatErr::NotLeaseHolder);
}
