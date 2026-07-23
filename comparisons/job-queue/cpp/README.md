# jobqueue — persistent leased job queue (C++)

A single-process implementation of the persistent leased job queue spec.
Built for the three-way Edda / Rust / C++ comparison; the goal is idiomatic
modern C++ that reads cleanly against the same spec the other two
implementations are written from.

Implements v1.0, the v1.1 extension (priority levels, scheduled enqueue,
per-queue metrics), and the v2.0 extension (dependency DAG, worker registry
with capability matching, cancellation cascade, audit log, multi-tenant
namespaces, priority promotion). All earlier scenarios pass unchanged —
99 cases / 1634 assertions in the current suite.

## Build & run

Requirements:

- CMake ≥ 3.20
- A C++20 compiler (tested on MSVC 19.43 from VS 2022)

```sh
cmake -S . -B build
cmake --build build --config Release
ctest --test-dir build -C Release --output-on-failure
```

Single-config generators (Ninja, Unix Makefiles) work the same; drop the
`--config Release` arguments. Tests can also be run directly:

```sh
./build/Release/jobqueue_tests   # or .\build\Release\jobqueue_tests.exe on Windows
```

Catch2 v3 is fetched via `FetchContent` on first configure.

## Design

### Language level

C++20. The implementation uses `std::jthread` (for tests),
`std::string_view`, `std::optional`, `std::variant`, designated initializers,
and `std::filesystem`. Nothing more exotic is needed.

### Concurrency

A single `std::mutex` protects all queue state. Every public operation takes
that one lock, mutates, persists, and returns. There is no blocking inside
the lock (no condition variables, no I/O on the critical path apart from the
synchronous snapshot write), so every operation terminates in bounded time
and deadlocks are impossible by construction.

The spec is explicit that `acquire` does not block — there is no notion of
"wait for work". A single mutex is the obvious fit for that shape:
finer-grained schemes would buy contention reduction we don't need and add
ordering hazards we'd then have to reason about.

### Persistence

Each mutating operation rewrites the entire queue state to disk via an
atomic-rename (`write tempfile → flush → close → rename`). The format is a
small custom little-endian binary layout defined in `src/storage.cpp` — six
fixed-width fields per job plus length-prefixed payload bytes.

We chose a custom binary format over JSON to keep the library
dependency-free and the persistence code direct. The alternative
(`nlohmann/json` via `FetchContent`) hit Windows MAX_PATH limits during
clone — the repo contains test-report filenames over 200 characters — and a
hand-rolled format was simpler than working around that.

The persistence model is "snapshot the whole world on every mutation". For
queues with thousands of in-flight jobs this would be the obvious thing to
replace first (with a write-ahead log or per-record updates), but for the
correctness focus of this comparison the cost is acceptable and the code is
easier to follow.

Durability claim: the spec requires SIGKILL safety. Atomic rename gives us
that — after the rename returns, the new file is the canonical state. We do
*not* call `fsync` on POSIX or `FlushFileBuffers` on Windows, so
power-loss safety is best-effort. Adding the platform-specific sync calls is
straightforward; it was left out to keep the code portable and small.

### Clock

All time reads go through a `jobqueue::Clock` interface, with two impls:

- `SystemClock` — wraps `std::chrono::system_clock` + a `std::mt19937_64`
  for jitter.
- `ManualClock` — used in every test in this suite. `advance(d)` moves time
  forward by a fixed amount; `set_uniform01(v)` pins the next random draw,
  so jitter is fully deterministic.

`TimePoint` is pinned to nanosecond resolution
(`time_point<system_clock, nanoseconds>`) so that `now + nanoseconds` returns
the same type — necessary on MSVC, where `system_clock::duration` is 100ns
and the unpinned alias triggers narrowing-conversion errors.

### Attempt counting

The spec phrases the dead-letter trigger as "if attempt count reaches
max_attempts" and the backoff formula as `2^(attempt-1)` using the
pre-increment value. Read literally these are inconsistent — `reaches max`
+ pre-increment-formula gives `max_attempts - 1` total attempts, but the
spec's scenario 15 ("after fail `max_attempts` times the job moves to
dead-letter") clearly wants `max_attempts` total tries.

We honor the test scenario. Concretely: `attempt` starts at 1 on enqueue;
each `acquire` returns the current attempt unchanged; `fail` computes
backoff with the pre-increment attempt (so the first retry uses `2^0`),
increments, and then dead-letters when `attempt >= max_attempts`. With
`max_attempts = 3`, this gives three `acquire → fail` cycles before the
job moves to dead-letter, matching the test scenario.

### Lease semantics & sweep

We keep a single `std::vector<Job>` in enqueue order. State (Pending /
Leased / RetryPending / DeadLettered) is a field on the job, not a separate
container — so a lease expiring and "returning to its original position" is
just a state transition, not a list move.

A lazy "sweep" runs at the start of every mutating call (`enqueue`,
`acquire`, `complete`, `fail`). It demotes expired leases back to Pending
and promotes ready retries to Pending. `heartbeat` deliberately does *not*
sweep, so it can distinguish "your lease just expired" (`LeaseExpired`)
from "you never held this job" (`NotLeaseHolder`).

To support scenario 24 (heartbeat racing with a sweep), the `lease_holder`
field is kept on the job after a demotion. A subsequent heartbeat from the
previous holder thus sees "I was the holder, but the lease is gone" and
returns `LeaseExpired`. If a new worker acquires in between, `lease_holder`
is overwritten and the old worker correctly gets `NotLeaseHolder`.

### v1.1 — priority, scheduled enqueue, metrics

**Priority** is a `std::uint32_t` in 1..=10 (default 5). Validation runs at
the API boundary and returns `EnqueueErr::InvalidPriority`. `acquire` picks
the maximum-priority acquirable job, with FIFO as the tiebreaker. The
ordering is a linear scan — O(n) per acquire is fine at this scale, and
keeps the data structure flat (one `std::vector<Job>` ordered by enqueue
sequence). A heap or per-priority deque would help at high throughput but
would distort the implementation for a comparison demo.

**Scheduled enqueue** adds an `std::optional<TimePoint> scheduled_at` to
`EnqueueOptions` and `Job`. A scheduled job is in `Pending` state from the
moment of enqueue (so it counts against `active_capacity`); a single
`is_acquirable(j, now)` predicate gates whether `acquire` will return it.
That keeps the sweep logic simple — no new state in `JobState`, no extra
promotion step — and the spec's "max(retry_ready_at, scheduled_at)" rule
falls out automatically: RetryPending→Pending promotion still keys off
`retry_ready_at`, then the `is_acquirable` filter applies the
`scheduled_at` floor.

**Both** options come in through a single `EnqueueOptions` struct with
designated-initializer-friendly defaults, so the v1 call shape
(`queue.enqueue("payload")`) still compiles unchanged.

**Metrics** live in `include/jobqueue/metrics.hpp` (new file — new feature
gets its own header). The `Metrics` struct holds seven monotonic counters
plus three transient gauges. Counters are stored on `JobQueue` and
incremented at the relevant transition points (enqueue success, acquire
success, complete, fail, sweep-time lease demotion, dead-letter transition,
retry-pending transition). They flow through the existing persistence path
so they survive restart. Gauges are computed at `metrics()` snapshot time
from the live `jobs_` / `dead_letter_` containers. The whole snapshot is
taken under the queue mutex, so observers never see internally-inconsistent
states (e.g. `completed_total > acquired_total`).

The persistence magic bumped from `JQV1` to `JQV2` when the v1.1 fields
were added. A pre-v1.1 file fails to load with a bad-magic error rather
than silently corrupting state. There is no migration helper — the
durability tests use either `MemoryStorage` (cleared per process) or a
fresh temp file per run.

### v2.0 — workflow engine surface

The v2.0 extension turns the queue into a workflow engine. The shape of
each addition:

**Dependencies.** Jobs carry a `depends_on: vector<JobId>`. A job is
*eligible* (acquirable) only once every dependency has reached
`Completed`. To make that lookup possible, jobs reaching terminal states
(Completed, Cancelled, DeadLettered) stay in the canonical `jobs_` vector
— they're no longer evicted on complete. Dead-letter rotation prunes both
the per-namespace dead-letter ring and the underlying canonical entry;
descendants of the dropped job are already cancelled by that point.

**Cycle detection.** `enqueue` validates every dep id exists in the same
namespace, then runs a defensive DFS that rejects if any dep's transitive
closure reaches the about-to-be-allocated id. With monotonic id allocation
and immutable dependencies, a real transitive cycle cannot form through
the public API; the scan exists to catch the self-cycle case (which falls
out as "unknown id") and to harden against a corrupted load.

**Cancellation cascade.** `cancel(id, reason)` runs a BFS over forward
dependents and transitions each to `Cancelled`. Terminal descendants stop
the cascade (the spec is explicit). The originating event's audit payload
is the user-supplied reason; cascaded events carry `cascade from <id>`.
All N events in one cascade share `at_nanos`. The same mechanism fires
inside `fail` when attempts are exhausted — DL transition + cascade run
under one lock so the chain is atomic. When a leased job is cancelled, the
lease_holder is *kept* on the job (same trick as natural lease expiry) so
that a stray heartbeat from the original holder reports `LeaseExpired`,
not the more generic `NotLeaseHolder`.

**Workers + capabilities.** A new worker registry holds `id ->
{capabilities[]}`. Worker registration is opt-in via
`Config::require_worker_registration`; with the default `false`, v1.x
callers continue to acquire without registering anything. When the opt-in
is true, unregistered callers get `UnknownWorker`. Capability matching is
*superset*: a job's `required_capabilities` must all appear in the
worker's. Deregistering force-expires every lease the worker held; held
jobs return to the active set without bumping the attempt counter (same
semantics as natural expiry).

**Namespaces.** Every job belongs to a namespace (default
`"default"`). `active_capacity` and `dead_letter_capacity` are
per-namespace. The internal `namespaces_` map holds config + counters +
dead-letter ring per namespace; a namespace is auto-created from the
queue-level defaults on first use unless `register_namespace` ran first.
Dependencies may not cross namespaces (rejected as `InvalidDependency`).
The queue-wide rollup that `metrics()` returns is computed by summation
over per-namespace counters; `metrics_per_namespace()` returns the
breakdown.

**`metrics()` is additive, not changed.** The spec says "metrics() now
returns both a rollup and a per-namespace breakdown". To keep the v1.1
API exactly intact (its tests do `auto m = q.metrics(); m.enqueued_total`),
we kept `metrics()` returning the rollup `Metrics` and added a new
`metrics_per_namespace()` method. The two together satisfy the spec
without breaking the existing callers.

**Audit log.** Every state transition emits an `AuditEvent` with a
queue-lifetime-monotonic `event_id`. The log is bounded by
`Config::audit_retention` (default 10000); on overflow the oldest event
is dropped and `oldest_retained_event_id_` advances. `audit_since(after)`
returns events with `event_id > after` or `AuditEventDropped` when the
watermark has fallen off the bounded log. `audit_recent(n)` returns the
last `n` events.

**Promotion.** `promote(job_id, new_priority)` raises a Pending job's
priority. Strictly greater than current, in range 1..=10, job must be in
Pending state. Each promotion bumps `promoted_total` and emits a
`Promoted` audit event with payload `old->new`.

**File organization.** The new subsystems split into dedicated files:
`audit.hpp` and `worker.hpp` for the new types; `cascade.cpp` for the
cancellation cascade + dead-letter transition (they share BFS shape and
audit-event vocabulary); `admin.cpp` for the v2 administrative surface
(workers, namespaces, promotion, audit readers). The core lifecycle
(ctor, sweep, persist) and the five v1 job-state transitions stay in
`jobqueue.cpp`.

The persistence magic bumped again from `JQV2` to `JQV3` when the v2.0
fields were added.

## Project layout

```
include/jobqueue/
  types.hpp        Job, JobState, EnqueueOptions, errors, Config
  metrics.hpp      Metrics snapshot type
  clock.hpp        Clock interface + SystemClock + ManualClock
  storage.hpp      Storage interface + Memory/File/Null impls
  audit.hpp        v2 — AuditEvent + AuditEventKind + AuditErr
  worker.hpp       v2 — Worker + WorkerView
  jobqueue.hpp     the JobQueue class itself
src/
  clock.cpp
  storage.cpp      JQV3 binary codec (atomic-rename file write)
  jobqueue.cpp     ctor, sweep, persist, v1 ops (enqueue/acquire/...)
  cascade.cpp      v2 — cancellation cascade + dead-letter transition
  admin.cpp        v2 — workers, namespaces, promote, audit readers
tests/
  test_*.cpp       Catch2 tests, one file per scenario group
CMakeLists.txt
```

## Scenario coverage

Every numbered scenario in the spec has a dedicated `TEST_CASE`. The test
file is named after the scenario group:

| Scenarios | File                            |
|-----------|---------------------------------|
| 1–5       | `tests/test_basics.cpp`         |
| 6–12      | `tests/test_lease.cpp`          |
| 13–17     | `tests/test_retry.cpp`          |
| 18–21     | `tests/test_persistence.cpp`    |
| 22–25     | `tests/test_concurrency.cpp`    |
| 26–28     | `tests/test_capacity.cpp`       |
| 29–30     | `tests/test_time.cpp`           |
| 31–35     | `tests/test_priority.cpp`       |
| 36–40     | `tests/test_scheduled.cpp`      |
| 41–45     | `tests/test_metrics.cpp`        |
| 46–47     | `tests/test_interaction.cpp`    |
| 100–108   | `tests/test_dependencies.cpp`   |
| 116–123   | `tests/test_workers.cpp`        |
| 126–133   | `tests/test_cancellation.cpp`   |
| 136–142   | `tests/test_audit.cpp`          |
| 146–151   | `tests/test_namespaces.cpp`     |
| 156–160   | `tests/test_promotion.cpp`      |
| 161–165   | `tests/test_crosscut.cpp`       |

Persistence tests use `MemoryStorage` (a shared, in-process snapshot that
survives the `JobQueue` being dropped and reconstructed). One extra test
exercises `FileStorage` end-to-end against a real file on disk.

A handful of supplementary cases (`35b`, `45b`, `47b`) cover behaviors
implied by the spec but not enumerated explicitly: default-priority
return value, `dead_lettered_total` as a high-water mark across
dead-letter rotation, and `scheduled_at`-dominates-retry ordering.

### Notes on cycle-detection tests (102, 103)

Because dependencies are immutable after enqueue and ids are allocated
monotonically by the queue, a real transitive cycle cannot be constructed
through the public API: at the moment a job is enqueued, its prospective
id isn't in any existing `depends_on` list. The "direct A→A cycle" test
exercises this naturally — depending on the prospective self-id is
equivalent to depending on an unknown id (`InvalidDependency`). The
"indirect A→B→A" test verifies the same rejection path with a more
elaborate dep set; the cycle-detection scan itself is present and
documented but won't trigger through the public API.

### Heartbeat after cancellation

The spec leaves the choice of `NotLeaseHolder` vs `LeaseExpired` to the
implementation when heartbeating a job that has been cancelled. This
implementation preserves the `lease_holder` field on cancellation (same
trick as natural lease expiry) so heartbeat returns `LeaseExpired`,
consistent with v1 expiry semantics.

## Limitations

- Snapshot-on-every-mutation persistence is O(jobs) per operation. Fine for
  this comparison's scale; would be the first thing to replace at high
  throughput.
- No `fsync` / `FlushFileBuffers` — atomic rename survives SIGKILL but not
  power loss.
- No background lease reaper; sweeps are lazy on operation entry. Jobs whose
  leases expired sit in `Leased` state until the next operation observes them.
  This is invisible to correct callers and saves a thread.
