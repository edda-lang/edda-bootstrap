#pragma once

#include "jobqueue/jobqueue.hpp"

#include <catch2/catch_test_macros.hpp>

#include <chrono>
#include <memory>
#include <string>
#include <variant>

namespace jqtest {

using namespace std::chrono_literals;
using jobqueue::AcquireErr;
using jobqueue::AcquireOk;
using jobqueue::AuditErr;
using jobqueue::AuditEvent;
using jobqueue::AuditEventKind;
using jobqueue::CancelledEntry;
using jobqueue::CancelResult;
using jobqueue::CompleteErr;
using jobqueue::Config;
using jobqueue::DeadLetterEntry;
using jobqueue::Duration;
using jobqueue::EnqueueErr;
using jobqueue::EnqueueOptions;
using jobqueue::FailErr;
using jobqueue::HeartbeatErr;
using jobqueue::JobId;
using jobqueue::JobQueue;
using jobqueue::ManualClock;
using jobqueue::MemoryStorage;
using jobqueue::Metrics;
using jobqueue::NamespaceConfig;
using jobqueue::NullStorage;
using jobqueue::Ok;
using jobqueue::PromoteErr;
using jobqueue::Result;
using jobqueue::TimePoint;
using jobqueue::WorkerView;

struct Fixture {
    std::shared_ptr<ManualClock> clock;
    std::shared_ptr<MemoryStorage> storage;
    std::unique_ptr<JobQueue> queue;

    static Fixture make(Config cfg = {}) {
        Fixture f;
        f.clock   = std::make_shared<ManualClock>();
        f.storage = std::make_shared<MemoryStorage>();
        f.queue   = std::make_unique<JobQueue>(std::move(cfg), f.clock, f.storage);
        return f;
    }
};

template <typename T, typename E>
T expect_ok(Result<T, E> r) {
    REQUIRE(std::holds_alternative<T>(r));
    return std::get<T>(std::move(r));
}

template <typename T, typename E>
E expect_err(Result<T, E> r) {
    REQUIRE(std::holds_alternative<E>(r));
    return std::get<E>(r);
}

} // namespace jqtest
