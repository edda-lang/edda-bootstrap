//! Injectable clock abstraction.
//!
//! The spec requires all time reads to go through a `Clock` so tests can
//! advance time deterministically. `Instant` is a custom newtype over `u64`
//! nanoseconds — it can be serialized and constructed at arbitrary values,
//! unlike `std::time::Instant`.

use std::ops::{Add, Sub};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Monotonic timestamp expressed as nanoseconds since some clock-defined epoch.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug, Hash, Serialize, Deserialize)]
pub struct Instant(u64);

impl Instant {
    pub const ZERO: Instant = Instant(0);

    pub fn from_nanos(n: u64) -> Self {
        Instant(n)
    }

    pub fn as_nanos(self) -> u64 {
        self.0
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, d: Duration) -> Instant {
        let nanos = d.as_nanos().min(u64::MAX as u128) as u64;
        Instant(self.0.saturating_add(nanos))
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;
    fn sub(self, other: Instant) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(other.0))
    }
}

/// Source of monotonic time. Implementations must be cheap to call and
/// thread-safe.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Real wall clock backed by `std::time::Instant`. Epoch is set at construction.
pub struct SystemClock {
    epoch: std::time::Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self { epoch: std::time::Instant::now() }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        let nanos = self.epoch.elapsed().as_nanos();
        Instant(nanos.min(u64::MAX as u128) as u64)
    }
}

/// Manually advanced clock for tests.
pub struct TestClock {
    now: Mutex<Instant>,
}

impl TestClock {
    pub fn new() -> Self {
        Self { now: Mutex::new(Instant::ZERO) }
    }

    pub fn advance(&self, d: Duration) {
        let mut t = self.now.lock().unwrap();
        *t = *t + d;
    }

    pub fn set(&self, t: Instant) {
        *self.now.lock().unwrap() = t;
    }
}

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self.now.lock().unwrap()
    }
}
