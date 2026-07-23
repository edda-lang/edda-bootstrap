#include "helpers.hpp"

#include <algorithm>

using namespace jqtest;

TEST_CASE("scenario 146: two namespaces; capacity is per-namespace", "[namespaces]") {
    auto f = Fixture::make(Config{.active_capacity = 2});
    // Fill the default namespace.
    expect_ok(f.queue->enqueue("d1"));
    expect_ok(f.queue->enqueue("d2"));
    REQUIRE(expect_err(f.queue->enqueue("d3")) == EnqueueErr::QueueFull);
    // A different namespace still has its own slots (auto-created with the
    // queue-level defaults: active_capacity=2).
    expect_ok(f.queue->enqueue("other-1", EnqueueOptions{.namespace_id = "other"}));
    expect_ok(f.queue->enqueue("other-2", EnqueueOptions{.namespace_id = "other"}));
    REQUIRE(expect_err(f.queue->enqueue("other-3", EnqueueOptions{.namespace_id = "other"}))
            == EnqueueErr::QueueFull);
}

TEST_CASE("scenario 147: per-namespace metrics; rollup matches sum", "[namespaces]") {
    auto f = Fixture::make();
    expect_ok(f.queue->enqueue("a"));
    expect_ok(f.queue->enqueue("b", EnqueueOptions{.namespace_id = "alt"}));
    expect_ok(f.queue->enqueue("c", EnqueueOptions{.namespace_id = "alt"}));

    auto per_ns = f.queue->metrics_per_namespace();
    REQUIRE(per_ns.size() == 2);
    REQUIRE(per_ns.at("default").enqueued_total == 1);
    REQUIRE(per_ns.at("alt").enqueued_total == 2);

    auto rollup = f.queue->metrics();
    REQUIRE(rollup.enqueued_total ==
            per_ns.at("default").enqueued_total +
            per_ns.at("alt").enqueued_total);
}

TEST_CASE("scenario 148: cross-namespace dependency rejected", "[namespaces]") {
    auto f = Fixture::make();
    auto here = expect_ok(f.queue->enqueue("here"));
    auto r = f.queue->enqueue("over-there",
                               EnqueueOptions{
                                   .depends_on   = {here},
                                   .namespace_id = "other",
                               });
    REQUIRE(expect_err(r) == EnqueueErr::InvalidDependency);
}

TEST_CASE("scenario 149: list_namespaces grows on first enqueue", "[namespaces]") {
    auto f = Fixture::make();
    // The default namespace always exists.
    auto initial = f.queue->list_namespaces();
    REQUIRE(std::find(initial.begin(), initial.end(), "default") != initial.end());

    expect_ok(f.queue->enqueue("x", EnqueueOptions{.namespace_id = "alpha"}));
    expect_ok(f.queue->enqueue("y", EnqueueOptions{.namespace_id = "beta"}));
    auto listed = f.queue->list_namespaces();
    REQUIRE(std::find(listed.begin(), listed.end(), "alpha") != listed.end());
    REQUIRE(std::find(listed.begin(), listed.end(), "beta")  != listed.end());
}

TEST_CASE("scenario 150: dead-letter capacity per-namespace", "[namespaces]") {
    auto f = Fixture::make(Config{
        .max_attempts    = 1,
        .backoff_base    = 1ms,
        .jitter_fraction = 0.0,
    });
    f.queue->register_namespace("tight", NamespaceConfig{1024, 1});
    f.queue->register_namespace("loose", NamespaceConfig{1024, 100});

    auto fail_one = [&](const std::string& ns){
        auto id = expect_ok(f.queue->enqueue("j",
                                              EnqueueOptions{.namespace_id = ns}));
        expect_ok(f.queue->acquire("w", 10s));
        expect_ok(f.queue->fail("w", id, "x"));
    };
    fail_one("tight");
    fail_one("tight");
    fail_one("tight");
    auto per_ns = f.queue->metrics_per_namespace();
    REQUIRE(per_ns.at("tight").dead_letter_count == 1); // capped
    REQUIRE(per_ns.at("tight").dead_lettered_total == 3); // monotonic

    fail_one("loose");
    fail_one("loose");
    auto per_ns2 = f.queue->metrics_per_namespace();
    REQUIRE(per_ns2.at("loose").dead_letter_count == 2);
}

TEST_CASE("scenario 151: namespace config survives restart", "[namespaces]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    {
        JobQueue q(Config{}, clock, storage);
        q.register_namespace("custom", NamespaceConfig{3, 7});
        expect_ok(q.enqueue("x", EnqueueOptions{.namespace_id = "custom"}));
        expect_ok(q.enqueue("y", EnqueueOptions{.namespace_id = "custom"}));
        expect_ok(q.enqueue("z", EnqueueOptions{.namespace_id = "custom"}));
    }
    JobQueue q2(Config{}, clock, storage);
    // The persisted active_capacity=3 should still reject the fourth enqueue.
    auto r = q2.enqueue("w", EnqueueOptions{.namespace_id = "custom"});
    REQUIRE(expect_err(r) == EnqueueErr::QueueFull);
}
