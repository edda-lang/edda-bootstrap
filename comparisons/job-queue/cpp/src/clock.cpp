#include "jobqueue/clock.hpp"

#include <random>

namespace jobqueue {

SystemClock::SystemClock() : rng_(std::random_device{}()) {}

TimePoint SystemClock::now() {
    return std::chrono::time_point_cast<Duration>(
        std::chrono::system_clock::now());
}

double SystemClock::uniform01() {
    std::lock_guard<std::mutex> lock(rng_mu_);
    std::uniform_real_distribution<double> dist(0.0, 1.0);
    return dist(rng_);
}

ManualClock::ManualClock(TimePoint start) : now_(start) {}

TimePoint ManualClock::now() {
    std::lock_guard<std::mutex> lock(mu_);
    return now_;
}

double ManualClock::uniform01() {
    std::lock_guard<std::mutex> lock(mu_);
    return next_uniform_;
}

void ManualClock::advance(Duration d) {
    std::lock_guard<std::mutex> lock(mu_);
    now_ += d;
}

void ManualClock::set_time(TimePoint t) {
    std::lock_guard<std::mutex> lock(mu_);
    now_ = t;
}

void ManualClock::set_uniform01(double v) {
    std::lock_guard<std::mutex> lock(mu_);
    next_uniform_ = v;
}

} // namespace jobqueue
