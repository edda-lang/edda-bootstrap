//! Clock and blocking-delay runtime externs backing `std.os.time`.
//!
//! ABI conventions (mirror the `@abi(...)` signatures in `std.os.time`):
//!   - capability params (`Clock` / `MonotonicClock`) keep their
//!     source-declaration position as an opaque `*const ()` slot (ignored).
//!   - single-field carrier records (`Instant`, `MonotonicInstant`,
//!     `Duration`) lower to their one `i64` field — returned in `rax`,
//!     passed as a scalar.
//!   - `-> ()` lowers to void.

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Process-lifetime reference point for the monotonic clock.
static MONOTONIC_BASE: OnceLock<std::time::Instant> = OnceLock::new();

/// Wall-clock now — backs `std.os.time.now(clock) -> Instant`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_clock_now(_clock: *const ()) -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Narrow a `Clock` capability handle to `MonotonicClock` — backs
/// `std.os.time.monotonic(clock) -> MonotonicClock`. Capability handles
/// are opaque pointers sharing one runtime representation, so the
/// narrowing is a no-op pass-through; the compile-time type check is the
/// load-bearing guarantee.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_clock_monotonic(clock: *const ()) -> *const () {
    clock
}

/// Monotonic now — backs `std.os.time.monotonic_now(clock) -> MonotonicInstant`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_monotonic_clock_now(_clock: *const ()) -> i64 {
    MONOTONIC_BASE
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_nanos() as i64
}

/// Blocking delay — backs `std.os.time.sleep(clock, d)`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_sleep_nanos(_clock: *const (), nanos: i64) {
    if nanos > 0 {
        std::thread::sleep(std::time::Duration::from_nanos(nanos as u64));
    }
}
