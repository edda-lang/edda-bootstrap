#include "helpers.hpp"

using namespace jqtest;

TEST_CASE("scenario 136: enqueue emits Enqueued event", "[audit]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("first"));
    auto events = f.queue->audit_recent(10);
    REQUIRE(events.size() == 1);
    REQUIRE(events[0].kind == AuditEventKind::Enqueued);
    REQUIRE(events[0].job_id.has_value());
    REQUIRE(*events[0].job_id == id);
    REQUIRE(events[0].event_id == 1);
}

TEST_CASE("scenario 137: full acquire->complete cycle emits the right event sequence",
          "[audit]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("j"));
    expect_ok(f.queue->acquire("w", 10s));
    expect_ok(f.queue->complete("w", id));
    auto events = f.queue->audit_recent(10);
    REQUIRE(events.size() == 3);
    REQUIRE(events[0].kind == AuditEventKind::Enqueued);
    REQUIRE(events[1].kind == AuditEventKind::Acquired);
    REQUIRE(events[2].kind == AuditEventKind::Completed);
}

TEST_CASE("scenario 138: retention bound enforced; oldest dropped", "[audit]") {
    Config cfg{.audit_retention = 5};
    auto f = Fixture::make(cfg);
    for (int i = 0; i < 12; ++i) expect_ok(f.queue->enqueue("j"));
    auto events = f.queue->audit_recent(100);
    REQUIRE(events.size() == 5);
    // The most recent five enqueues should have event_ids 8..12 (1-indexed).
    REQUIRE(events.front().event_id == 8);
    REQUIRE(events.back().event_id  == 12);
}

TEST_CASE("scenario 139: audit_since returns events strictly after the watermark",
          "[audit]") {
    auto f = Fixture::make();
    auto a = expect_ok(f.queue->enqueue("a"));
    auto b = expect_ok(f.queue->enqueue("b"));
    auto c = expect_ok(f.queue->enqueue("c"));
    (void)a; (void)b; (void)c;
    auto since_one = std::get<std::vector<AuditEvent>>(f.queue->audit_since(1));
    REQUIRE(since_one.size() == 2);
    REQUIRE(since_one[0].event_id == 2);
    REQUIRE(since_one[1].event_id == 3);
    auto since_three = std::get<std::vector<AuditEvent>>(f.queue->audit_since(3));
    REQUIRE(since_three.empty());
    auto since_zero = std::get<std::vector<AuditEvent>>(f.queue->audit_since(0));
    REQUIRE(since_zero.size() == 3);
}

TEST_CASE("scenario 140: audit_since against dropped watermark -> AuditEventDropped",
          "[audit]") {
    Config cfg{.audit_retention = 3};
    auto f = Fixture::make(cfg);
    for (int i = 0; i < 10; ++i) expect_ok(f.queue->enqueue("j"));
    // Oldest retained is event_id=8 (10 events emitted, capacity 3 → ids 8,9,10).
    // A watermark of 5 is older than oldest retained (8). Asking for ">5"
    // would skip 6,7 which we no longer have.
    auto r = f.queue->audit_since(5);
    REQUIRE(std::holds_alternative<AuditErr>(r));
    REQUIRE(std::get<AuditErr>(r) == AuditErr::AuditEventDropped);
    // Watermark of 7 is OK: ">7" yields 8,9,10 — all retained.
    auto ok = std::get<std::vector<AuditEvent>>(f.queue->audit_since(7));
    REQUIRE(ok.size() == 3);
}

TEST_CASE("scenario 141: audit log survives restart; event_id continues monotonically",
          "[audit]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    std::uint64_t last_id;
    {
        JobQueue q(Config{}, clock, storage);
        expect_ok(q.enqueue("a"));
        expect_ok(q.enqueue("b"));
        last_id = q.audit_recent(10).back().event_id;
    }
    JobQueue q2(Config{}, clock, storage);
    // Pre-restart history is intact.
    auto pre = q2.audit_recent(10);
    REQUIRE(pre.size() == 2);
    REQUIRE(pre.back().event_id == last_id);
    // New events continue from the next id.
    expect_ok(q2.enqueue("c"));
    auto post = q2.audit_recent(10);
    REQUIRE(post.size() == 3);
    REQUIRE(post.back().event_id == last_id + 1);
}

TEST_CASE("scenario 142: cascading cancellation emits N events with same at_nanos",
          "[audit]") {
    auto f = Fixture::make();
    auto a = expect_ok(f.queue->enqueue("a"));
    auto b = expect_ok(f.queue->enqueue("b",
                                         EnqueueOptions{.depends_on = {a}}));
    auto c = expect_ok(f.queue->enqueue("c",
                                         EnqueueOptions{.depends_on = {b}}));
    auto d = expect_ok(f.queue->enqueue("d",
                                         EnqueueOptions{.depends_on = {c}}));
    (void)d;
    f.clock->advance(1s);
    auto res = f.queue->cancel(a, "abort");
    REQUIRE(res.count == 4);

    std::int64_t shared_at = -1;
    std::size_t count = 0;
    for (const auto& e : f.queue->audit_recent(100)) {
        if (e.kind != AuditEventKind::Cancelled) continue;
        if (shared_at < 0) shared_at = e.at_nanos;
        else REQUIRE(e.at_nanos == shared_at);
        ++count;
    }
    REQUIRE(count == 4);
}
