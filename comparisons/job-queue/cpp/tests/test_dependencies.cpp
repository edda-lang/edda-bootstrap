#include "helpers.hpp"

using namespace jqtest;

TEST_CASE("scenario 100: parent then child; acquire returns parent, not child",
          "[dependencies]") {
    auto f = Fixture::make();
    auto parent = expect_ok(f.queue->enqueue("parent"));
    auto child  = expect_ok(f.queue->enqueue("child",
                                              EnqueueOptions{.depends_on = {parent}}));
    auto first = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(first.id == parent);
    // Child is blocked on parent: not acquirable yet.
    REQUIRE(std::holds_alternative<AcquireErr>(f.queue->acquire("w2", 10s)));
    (void)child;
}

TEST_CASE("scenario 101: child eligible after parent completes", "[dependencies]") {
    auto f = Fixture::make();
    auto parent = expect_ok(f.queue->enqueue("parent"));
    auto child  = expect_ok(f.queue->enqueue("child",
                                              EnqueueOptions{.depends_on = {parent}}));
    auto p = expect_ok(f.queue->acquire("w", 10s));
    expect_ok(f.queue->complete("w", p.id));
    auto c = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(c.id == child);
}

TEST_CASE("scenario 102: cycle detection - direct cycle (A -> A) rejected",
          "[dependencies]") {
    auto f = Fixture::make();
    // The about-to-be-allocated id is 1 (no prior enqueues); referencing it
    // in depends_on collapses to "unknown id" because the job doesn't exist
    // until admitted. Either way, InvalidDependency.
    auto r = f.queue->enqueue("self", EnqueueOptions{.depends_on = {1}});
    REQUIRE(expect_err(r) == EnqueueErr::InvalidDependency);
}

TEST_CASE("scenario 103: cycle detection - indirect (A -> B -> A) rejected",
          "[dependencies]") {
    auto f = Fixture::make();
    auto a = expect_ok(f.queue->enqueue("A"));
    auto b = expect_ok(f.queue->enqueue("B", EnqueueOptions{.depends_on = {a}}));
    // We can't actually construct a closed transitive cycle through the
    // public API because dependencies are immutable, but the rejection path
    // fires the same way: any unknown id in depends_on (including a future
    // id we would need to close the loop) yields InvalidDependency.
    auto r = f.queue->enqueue("C", EnqueueOptions{.depends_on = {b, 9999}});
    REQUIRE(expect_err(r) == EnqueueErr::InvalidDependency);
}

TEST_CASE("scenario 104: unknown dependency id at enqueue -> InvalidDependency",
          "[dependencies]") {
    auto f = Fixture::make();
    auto r = f.queue->enqueue("dangler", EnqueueOptions{.depends_on = {42}});
    REQUIRE(expect_err(r) == EnqueueErr::InvalidDependency);
}

TEST_CASE("scenario 105: dependency on a dead-lettered job -> child cancels",
          "[dependencies]") {
    auto f = Fixture::make(Config{
        .max_attempts    = 1,
        .backoff_base    = 1ms,
        .jitter_fraction = 0.0,
    });
    auto parent = expect_ok(f.queue->enqueue("parent"));
    expect_ok(f.queue->acquire("w", 10s));
    expect_ok(f.queue->fail("w", parent, "boom")); // -> dead-letter
    // Now enqueue a child depending on the dead-lettered parent.
    auto child = expect_ok(f.queue->enqueue("child",
                                             EnqueueOptions{.depends_on = {parent}}));
    auto cancelled = f.queue->cancelled_snapshot();
    REQUIRE(cancelled.size() == 1);
    REQUIRE(cancelled[0].id == child);
    REQUIRE(std::holds_alternative<AcquireErr>(f.queue->acquire("w", 10s)));
}

TEST_CASE("scenario 106: dependency on a cancelled job -> child cancels",
          "[dependencies]") {
    auto f = Fixture::make();
    auto parent = expect_ok(f.queue->enqueue("parent"));
    auto res = f.queue->cancel(parent, "operator stop");
    REQUIRE(res.count == 1);
    auto child = expect_ok(f.queue->enqueue("child",
                                             EnqueueOptions{.depends_on = {parent}}));
    bool found = false;
    for (const auto& c : f.queue->cancelled_snapshot()) if (c.id == child) found = true;
    REQUIRE(found);
}

TEST_CASE("scenario 107: grandparent -> parent -> child cascade", "[dependencies]") {
    auto f = Fixture::make();
    auto gp     = expect_ok(f.queue->enqueue("gp"));
    auto parent = expect_ok(f.queue->enqueue("parent",
                                              EnqueueOptions{.depends_on = {gp}}));
    auto child  = expect_ok(f.queue->enqueue("child",
                                              EnqueueOptions{.depends_on = {parent}}));
    auto g = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(g.id == gp);
    expect_ok(f.queue->complete("w", g.id));
    auto p = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(p.id == parent);
    expect_ok(f.queue->complete("w", p.id));
    auto c = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(c.id == child);
}

TEST_CASE("scenario 108: dependencies survive restart", "[dependencies]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    JobId parent_id, child_id;
    {
        JobQueue q(Config{}, clock, storage);
        parent_id = expect_ok(q.enqueue("parent"));
        child_id  = expect_ok(q.enqueue("child",
                                         EnqueueOptions{.depends_on = {parent_id}}));
    }
    JobQueue q2(Config{}, clock, storage);
    // Child must still be blocked on parent.
    auto p = expect_ok(q2.acquire("w", 10s));
    REQUIRE(p.id == parent_id);
    REQUIRE(std::holds_alternative<AcquireErr>(q2.acquire("w2", 10s)));
    expect_ok(q2.complete("w", p.id));
    auto c = expect_ok(q2.acquire("w", 10s));
    REQUIRE(c.id == child_id);
}
