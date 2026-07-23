#include "helpers.hpp"

using namespace jqtest;

TEST_CASE("scenario 156: promote raises priority; subsequent acquire respects new value",
          "[promotion]") {
    auto f = Fixture::make();
    auto low  = expect_ok(f.queue->enqueue("low",  EnqueueOptions{.priority = 2}));
    auto high = expect_ok(f.queue->enqueue("high", EnqueueOptions{.priority = 8}));
    // Currently `high` would win. Promote `low` to 10 — now it wins.
    expect_ok(f.queue->promote(low, 10));
    auto first = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(first.id == low);
    REQUIRE(first.priority == 10);
    auto second = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(second.id == high);
}

TEST_CASE("scenario 157: promote to lower or equal rejected", "[promotion]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("p5", EnqueueOptions{.priority = 5}));
    REQUIRE(expect_err(f.queue->promote(id, 3)) == PromoteErr::NotPromotion);
    REQUIRE(expect_err(f.queue->promote(id, 5)) == PromoteErr::NotPromotion);
    // And out-of-range rejected too.
    REQUIRE(expect_err(f.queue->promote(id, 0))  == PromoteErr::InvalidPriority);
    REQUIRE(expect_err(f.queue->promote(id, 11)) == PromoteErr::InvalidPriority);
}

TEST_CASE("scenario 158: promote a non-pending job rejected", "[promotion]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("j"));
    expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(expect_err(f.queue->promote(id, 9)) == PromoteErr::NotPending);
}

TEST_CASE("scenario 159: promote a scheduled-but-not-yet-due job works", "[promotion]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("later",
                                          EnqueueOptions{
                                              .priority     = 3,
                                              .scheduled_at = f.clock->now() + 1h,
                                          }));
    expect_ok(f.queue->promote(id, 9));
    // Still not acquirable until clock advances past scheduled_at.
    REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
    f.clock->advance(1h);
    auto ok = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(ok.id == id);
    REQUIRE(ok.priority == 9);
}

TEST_CASE("scenario 160: promote increments promoted_total and audit-logs",
          "[promotion]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("j", EnqueueOptions{.priority = 4}));
    REQUIRE(f.queue->metrics().promoted_total == 0);
    expect_ok(f.queue->promote(id, 8));
    REQUIRE(f.queue->metrics().promoted_total == 1);
    bool found = false;
    for (const auto& e : f.queue->audit_recent(10)) {
        if (e.kind == AuditEventKind::Promoted) { found = true; break; }
    }
    REQUIRE(found);
}
