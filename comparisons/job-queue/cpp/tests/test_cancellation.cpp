#include "helpers.hpp"

using namespace jqtest;

TEST_CASE("scenario 126: cancel leaf job; cancelled_iter shows it; metrics increment",
          "[cancellation]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("solo"));
    auto res = f.queue->cancel(id, "operator stop");
    REQUIRE(res.count == 1);
    auto entries = f.queue->cancelled_snapshot();
    REQUIRE(entries.size() == 1);
    REQUIRE(entries[0].id == id);
    REQUIRE(entries[0].reason == "operator stop");
    REQUIRE(f.queue->metrics().cancelled_total == 1);
    // Cancelled job is not acquirable.
    REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
}

TEST_CASE("scenario 127: cancel parent; child auto-cancels; CancelResult.count=2",
          "[cancellation]") {
    auto f = Fixture::make();
    auto parent = expect_ok(f.queue->enqueue("parent"));
    auto child  = expect_ok(f.queue->enqueue("child",
                                              EnqueueOptions{.depends_on = {parent}}));
    (void)child;
    auto res = f.queue->cancel(parent, "abort");
    REQUIRE(res.count == 2);
    auto entries = f.queue->cancelled_snapshot();
    REQUIRE(entries.size() == 2);
}

TEST_CASE("scenario 128: cancel grandparent; whole subtree cancels", "[cancellation]") {
    auto f = Fixture::make();
    auto gp    = expect_ok(f.queue->enqueue("gp"));
    auto p     = expect_ok(f.queue->enqueue("p",
                                             EnqueueOptions{.depends_on = {gp}}));
    auto c1    = expect_ok(f.queue->enqueue("c1",
                                             EnqueueOptions{.depends_on = {p}}));
    auto c2    = expect_ok(f.queue->enqueue("c2",
                                             EnqueueOptions{.depends_on = {p}}));
    (void)c1; (void)c2;
    auto res = f.queue->cancel(gp, "abort tree");
    REQUIRE(res.count == 4);
    REQUIRE(f.queue->cancelled_snapshot().size() == 4);
}

TEST_CASE("scenario 129: cancel already-cancelled job is no-op", "[cancellation]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("solo"));
    REQUIRE(f.queue->cancel(id, "first").count == 1);
    REQUIRE(f.queue->cancel(id, "second").count == 0);
    REQUIRE(f.queue->cancelled_snapshot().size() == 1);
    REQUIRE(f.queue->metrics().cancelled_total == 1);
}

TEST_CASE("scenario 130: cancel completed job is no-op; no cascade", "[cancellation]") {
    auto f = Fixture::make();
    auto parent = expect_ok(f.queue->enqueue("parent"));
    auto child  = expect_ok(f.queue->enqueue("child",
                                              EnqueueOptions{.depends_on = {parent}}));
    (void)child;
    auto ok = expect_ok(f.queue->acquire("w", 10s));
    expect_ok(f.queue->complete("w", ok.id));
    auto res = f.queue->cancel(parent, "too late");
    REQUIRE(res.count == 0);
    // Child is still eligible because parent completed normally.
    auto c = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(c.payload == "child");
}

TEST_CASE("scenario 131: cancel leased job force-expires lease", "[cancellation]") {
    auto f = Fixture::make();
    auto id = expect_ok(f.queue->enqueue("hot"));
    expect_ok(f.queue->acquire("w", 10s));
    auto res = f.queue->cancel(id, "halt");
    REQUIRE(res.count == 1);
    // Heartbeat now sees state != Leased and returns LeaseExpired.
    auto hb = f.queue->heartbeat("w", id, 1s);
    REQUIRE(expect_err(hb) == HeartbeatErr::LeaseExpired);
    // Job is not back in active — it's terminal-cancelled.
    REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
}

TEST_CASE("scenario 132: cancellation reason persisted across restart", "[cancellation]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    JobId id;
    {
        JobQueue q(Config{}, clock, storage);
        id = expect_ok(q.enqueue("doomed"));
        q.cancel(id, "operator stop");
    }
    JobQueue q2(Config{}, clock, storage);
    auto entries = q2.cancelled_snapshot();
    REQUIRE(entries.size() == 1);
    REQUIRE(entries[0].id == id);
    REQUIRE(entries[0].reason == "operator stop");
}

TEST_CASE("scenario 133: audit log has one Cancelled event per cancelled job",
          "[cancellation]") {
    auto f = Fixture::make();
    auto a = expect_ok(f.queue->enqueue("a"));
    auto b = expect_ok(f.queue->enqueue("b",
                                         EnqueueOptions{.depends_on = {a}}));
    auto c = expect_ok(f.queue->enqueue("c",
                                         EnqueueOptions{.depends_on = {b}}));
    (void)c;
    auto res = f.queue->cancel(a, "abort");
    REQUIRE(res.count == 3);

    std::size_t cancelled_events = 0;
    std::int64_t shared_at = 0;
    for (const auto& e : f.queue->audit_recent(100)) {
        if (e.kind == AuditEventKind::Cancelled) {
            if (cancelled_events == 0) shared_at = e.at_nanos;
            else REQUIRE(e.at_nanos == shared_at);
            ++cancelled_events;
        }
    }
    REQUIRE(cancelled_events == 3);
}
