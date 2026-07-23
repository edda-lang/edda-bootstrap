#pragma once

#include "types.hpp"

#include <mutex>
#include <random>

namespace jobqueue {

class Clock {
public:
    virtual ~Clock() = default;
    virtual TimePoint now() = 0;
    virtual double uniform01() = 0;
};

class SystemClock final : public Clock {
public:
    SystemClock();
    TimePoint now() override;
    double uniform01() override;

private:
    std::mt19937_64 rng_;
    std::mutex rng_mu_;
};

class ManualClock final : public Clock {
public:
    explicit ManualClock(TimePoint start = TimePoint{});

    TimePoint now() override;
    double uniform01() override;

    void advance(Duration d);
    void set_time(TimePoint t);
    void set_uniform01(double v);

private:
    std::mutex mu_;
    TimePoint now_;
    double next_uniform_ = 0.0;
};

} // namespace jobqueue
