#include "helpers.hpp"

using namespace jqtest;

TEST_CASE("scenario 161: high-priority parent blocked on incomplete dep does not preempt "
          "lower-priority eligible job", "[crosscut]") {
    auto f = Fixture::make();
    auto root   = expect_ok(f.queue->enqueue("root", EnqueueOptions{.priority = 1}));
    auto high   = expect_ok(f.queue->enqueue("high-blocked",
                                              EnqueueOptions{
                                                  .priority   = 10,
                                                  .depends_on = {root},
                                              }));
    auto plain  = expect_ok(f.queue->enqueue("plain", EnqueueOptions{.priority = 3}));
    (void)high;
    // root@1 vs high(blocked) vs plain@3 → root@1 has the highest priority
    // *among acquirable* once high is filtered out; but wait — plain@3 > root@1.
    // Acquirable set = {root, plain}; max priority among them is plain@3.
    auto first = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(first.id == plain);
    // root@1 next, high still blocked.
    auto second = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(second.id == root);
    REQUIRE(expect_err(f.queue->acquire("w", 10s)) == AcquireErr::Empty);
}

TEST_CASE("scenario 162: namespace + retry - retry consumes namespace capacity",
          "[crosscut]") {
    auto f = Fixture::make(Config{
        .max_attempts    = 3,
        .backoff_base    = 60s, // long enough to keep it retry-pending
        .jitter_fraction = 0.0,
    });
    f.queue->register_namespace("a", NamespaceConfig{1, 100});
    f.queue->register_namespace("b", NamespaceConfig{1, 100});
    auto id = expect_ok(f.queue->enqueue("j", EnqueueOptions{.namespace_id = "a"}));
    expect_ok(f.queue->acquire("w", 10s));
    expect_ok(f.queue->fail("w", id, "oops")); // -> retry-pending in ns "a"
    // ns "a" is now full again (retry-pending still occupies the slot).
    REQUIRE(expect_err(f.queue->enqueue("more", EnqueueOptions{.namespace_id = "a"}))
            == EnqueueErr::QueueFull);
    // ns "b" still has its independent slot.
    expect_ok(f.queue->enqueue("j", EnqueueOptions{.namespace_id = "b"}));
}

TEST_CASE("scenario 163: workers cross namespaces (capability match alone gates)",
          "[crosscut]") {
    auto f = Fixture::make(Config{.require_worker_registration = true});
    f.queue->register_worker("w", {"gpu"});
    expect_ok(f.queue->enqueue("a",
                                EnqueueOptions{
                                    .required_capabilities = {"gpu"},
                                    .namespace_id          = "ns-a",
                                }));
    expect_ok(f.queue->enqueue("b",
                                EnqueueOptions{
                                    .required_capabilities = {"gpu"},
                                    .namespace_id          = "ns-b",
                                }));
    auto first  = expect_ok(f.queue->acquire("w", 10s));
    auto second = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(first.id  != second.id);
}

TEST_CASE("scenario 164: cancellation does not cross namespaces", "[crosscut]") {
    auto f = Fixture::make();
    auto a_root = expect_ok(f.queue->enqueue("root",
                                              EnqueueOptions{.namespace_id = "ns-a"}));
    expect_ok(f.queue->enqueue("a-child",
                                EnqueueOptions{
                                    .depends_on   = {a_root},
                                    .namespace_id = "ns-a",
                                }));
    // A different-namespace job with no deps must be untouched by the cancel.
    auto b_id = expect_ok(f.queue->enqueue("ns-b job",
                                            EnqueueOptions{.namespace_id = "ns-b"}));
    auto res = f.queue->cancel(a_root, "halt");
    REQUIRE(res.count == 2); // ns-a parent + ns-a child, NOT ns-b
    auto ok = expect_ok(f.queue->acquire("w", 10s));
    REQUIRE(ok.id == b_id);
}

TEST_CASE("scenario 165: full restart of a multi-namespace queue with workers, in-flight "
          "leases, retry-pending jobs, and the full audit history", "[crosscut]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    Config cfg{
        .max_attempts                = 5,
        .backoff_base                = 1s,
        .jitter_fraction             = 0.0,
        .require_worker_registration = true,
    };
    std::uint64_t last_event_id_pre = 0;
    JobId leased_id = 0;
    JobId retry_id  = 0;
    {
        JobQueue q(cfg, clock, storage);
        q.register_worker("w-a", {});
        q.register_worker("w-b", {});
        q.register_namespace("alpha", NamespaceConfig{10, 10});
        q.register_namespace("beta",  NamespaceConfig{10, 10});

        leased_id = expect_ok(q.enqueue("alpha-1", EnqueueOptions{.namespace_id = "alpha"}));
        retry_id  = expect_ok(q.enqueue("alpha-2", EnqueueOptions{.namespace_id = "alpha"}));
        expect_ok(q.enqueue("beta-1",  EnqueueOptions{.namespace_id = "beta"}));

        auto leased  = expect_ok(q.acquire("w-a", 60s));
        (void)leased; // alpha-1 leased to w-a
        auto retrying = expect_ok(q.acquire("w-b", 60s));
        expect_ok(q.fail("w-b", retrying.id, "oops"));
        last_event_id_pre = q.audit_recent(100).back().event_id;
    }
    JobQueue q2(cfg, clock, storage);
    // Workers + namespaces survived.
    REQUIRE(q2.list_workers().size() == 2);
    auto namespaces = q2.list_namespaces();
    REQUIRE(namespaces.size() >= 3); // default + alpha + beta
    // The lease is still held by w-a (clock hasn't advanced past expiry).
    auto hb = q2.heartbeat("w-a", leased_id, 60s);
    REQUIRE(std::holds_alternative<Ok>(hb));
    // Audit log continues monotonically.
    auto after = q2.audit_recent(100);
    REQUIRE(after.back().event_id > last_event_id_pre);
    // Retry-pending job becomes acquirable after backoff.
    clock->advance(2s);
    auto retried = q2.acquire("w-b", 10s);
    REQUIRE(std::holds_alternative<AcquireOk>(retried));
    REQUIRE(std::get<AcquireOk>(retried).id == retry_id);
}
