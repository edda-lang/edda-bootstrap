#include "helpers.hpp"

#include "jobqueue/storage.hpp"

#include <catch2/catch_test_macros.hpp>

#include <filesystem>

using namespace jqtest;
using jobqueue::FileStorage;

namespace {

std::filesystem::path unique_temp_file(const char* tag) {
    auto dir = std::filesystem::temp_directory_path() / "jobqueue_tests";
    std::filesystem::create_directories(dir);
    return dir / (std::string{tag} + "_" +
                  std::to_string(std::chrono::system_clock::now().time_since_epoch().count()) +
                  ".json");
}

} // namespace

TEST_CASE("scenario 18: enqueue + restart preserves queue contents and order", "[persistence]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    {
        JobQueue q(Config{}, clock, storage);
        expect_ok(q.enqueue("first"));
        expect_ok(q.enqueue("second"));
        expect_ok(q.enqueue("third"));
    }
    // Drop and rebuild: simulates SIGKILL + restart.
    JobQueue q2(Config{}, clock, storage);
    auto a = expect_ok(q2.acquire("w", 10s));
    auto b = expect_ok(q2.acquire("w", 10s));
    auto c = expect_ok(q2.acquire("w", 10s));
    REQUIRE(a.payload == "first");
    REQUIRE(b.payload == "second");
    REQUIRE(c.payload == "third");
}

TEST_CASE("scenario 19: acquire + restart preserves lease state for original worker_id",
          "[persistence]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    JobId id;
    {
        JobQueue q(Config{}, clock, storage);
        id = expect_ok(q.enqueue("payload"));
        auto ok = expect_ok(q.acquire("worker-A", 10s));
        REQUIRE(ok.id == id);
    }
    JobQueue q2(Config{}, clock, storage);
    // Lease still held by worker-A: no other worker can acquire.
    REQUIRE(expect_err(q2.acquire("worker-B", 10s)) == AcquireErr::Empty);
    // Original worker can still heartbeat.
    expect_ok(q2.heartbeat("worker-A", id, 10s));
    // And complete.
    expect_ok(q2.complete("worker-A", id));
}

TEST_CASE("scenario 20: dead-letter survives restart", "[persistence]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    Config cfg{
        .max_attempts    = 1,
        .backoff_base    = 1ms,
        .jitter_fraction = 0.0,
    };
    JobId id;
    {
        JobQueue q(cfg, clock, storage);
        id = expect_ok(q.enqueue("doomed"));
        expect_ok(q.acquire("w1", 10s));
        expect_ok(q.fail("w1", id, "no good"));
    }
    JobQueue q2(cfg, clock, storage);
    auto dl = q2.dead_letter_snapshot();
    REQUIRE(dl.size() == 1);
    REQUIRE(dl[0].id == id);
    REQUIRE(dl[0].payload == "doomed");
    REQUIRE(dl[0].final_reason == "no good");
}

TEST_CASE("scenario 21: FIFO order of pending jobs is preserved across restart", "[persistence]") {
    auto clock   = std::make_shared<ManualClock>();
    auto storage = std::make_shared<MemoryStorage>();
    {
        JobQueue q(Config{}, clock, storage);
        for (int i = 0; i < 10; ++i) {
            expect_ok(q.enqueue("job-" + std::to_string(i)));
        }
    }
    JobQueue q2(Config{}, clock, storage);
    for (int i = 0; i < 10; ++i) {
        auto ok = expect_ok(q2.acquire("w", 10s));
        REQUIRE(ok.payload == "job-" + std::to_string(i));
    }
}

TEST_CASE("FileStorage round-trips through a real file on disk", "[persistence][file]") {
    auto path = unique_temp_file("roundtrip");
    auto clock = std::make_shared<ManualClock>();
    JobId id1, id2;
    {
        auto storage = std::make_shared<FileStorage>(path);
        JobQueue q(Config{}, clock, storage);
        id1 = expect_ok(q.enqueue("alpha"));
        id2 = expect_ok(q.enqueue("beta"));
        expect_ok(q.acquire("w1", 30s));
    }
    REQUIRE(std::filesystem::exists(path));
    auto storage = std::make_shared<FileStorage>(path);
    JobQueue q2(Config{}, clock, storage);
    // First job is still leased, second is pending.
    auto next = expect_ok(q2.acquire("w2", 30s));
    REQUIRE(next.id == id2);
    REQUIRE(next.payload == "beta");
    // Cleanup
    std::error_code ec;
    std::filesystem::remove(path, ec);
}
