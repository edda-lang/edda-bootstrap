# Persistent Leased Job Queue — Rust

Rust implementation for the three-way language comparison. Implements
**spec v2.0** (job dependencies, worker registry, cancellation, audit
log, multi-tenant namespaces, priority promotion) on top of v1.0's
lease + retry + dead-letter and v1.1's priority + scheduling + metrics.
Idiomatic, single-process, sync API; durability is paid for at every
mutation via an atomic file rename.

## Build, run, test

```sh
cargo build         # debug build
cargo test          # 98 tests pass (98 scenarios + inline + Send/Sync checks)
cargo build --release
```

The crate declares `[workspace]` in its own `Cargo.toml` and is
intentionally detached from the surrounding compiler workspace, so all
cargo commands should be run from this directory.

`leased-job-queue` is a library — no binary. Consumers open a queue with
`Queue::open(path, config, clock)` and drive it from worker threads.

## Layout

```
src/
├── lib.rs        — public re-exports
├── clock.rs      — Clock trait, Instant newtype, SystemClock, TestClock
├── config.rs     — Config + NamespaceConfig + DEFAULT_NAMESPACE
├── priority.rs   — Priority newtype (1..=10), InvalidPriority
├── enqueue.rs    — EnqueueRequest builder (v2 adds depends_on, required_capabilities, namespace)
├── error.rs      — EnqueueError, AcquireError, HeartbeatError, AckError, PromoteError, AuditError, DependencyError, CancelResult
├── metrics.rs    — Counters (persisted) + MetricsSnapshot + NamespaceMetrics
├── workers.rs    — Worker, WorkerRegistry, WorkerView
├── audit.rs      — AuditEvent, AuditEventKind, AuditLog
├── queue.rs      — Queue public API + DAG/cascade/dead-letter/backoff helpers
└── state.rs      — Job + JobState + State + persistence I/O
tests/
└── scenarios.rs  — one test per numbered spec scenario:
                    v1.0 → t1..=t30
                    v1.1 → t31..=t47 (+ t47b)
                    v2.0 → t100..=t108 (deps),  t116..=t123 (workers),
                           t126..=t133 (cancel),  t136..=t142 (audit),
                           t146..=t151 (namespaces), t156..=t160 (promote),
                           t161..=t165 (cross-cutting)
```

## v2.0 additions

### Job dependencies (DAG) — `EnqueueRequest::depends_on`

A job becomes eligible for `acquire` once every id in its `depends_on`
list has reached `Completed`. Eligibility is reified as a
`remaining_deps` counter on the job; `complete(parent)` decrements that
counter on each child in `parent.dependents`. Cancellation /
dead-letter of a parent cascades through the same `dependents`
adjacency, transitioning every transitive descendant to `Cancelled`.

`enqueue` rejects with `EnqueueError::InvalidDependency` for unknown
ids, duplicates within a single list, cross-namespace references, and
(defensively, since one-shot enqueue can't close a cycle) cycle
detection by DFS. A self-cycle (`depends_on: [self_id]`) is rejected
naturally because the new id doesn't exist until the job is admitted —
the unknown-id check catches it.

Terminal jobs (Completed / Cancelled / DeadLetter) stay in the `jobs`
map so dependency lookups remain valid for the job's lifetime. The
sole exception is dead-letter entries dropped to honor a namespace's
`dead_letter_capacity` — those are removed entirely, matching v1.x's
"dropped permanently" semantics.

### Workers + capability matching — `src/workers.rs`

`register_worker(id, caps)` adds (or overwrites) a worker. `acquire`
returns the highest-priority job whose `required_capabilities` is a
subset of the worker's registered set; jobs whose required set isn't
satisfied are filtered out of the candidate list.

Backward compatibility is preserved by a configuration switch:
`Config::require_worker_registration` (default `false`). With the
default, jobs that have no `required_capabilities` can be acquired by
any worker id — v1.x callers continue to work without registering.
Jobs *with* `required_capabilities` always require a registered
worker, returning `AcquireError::UnknownWorker` otherwise.
Setting `require_worker_registration = true` extends that strictness
to every acquire.

`deregister_worker` force-expires every lease the worker holds (job
returns to active set without incrementing `failures` — same
semantics as natural lease expiry, NOT as explicit `fail`) and
returns the count of force-expired leases. Deregistering an unknown
worker is a no-op (returns 0).

### Cancellation + cascade — `Queue::cancel`

`cancel(job_id, reason)` transitions `job_id` and every transitive
dependent (collected via BFS through the `dependents` adjacency) to
`Cancelled`. Terminal states stop the cascade — cancelling an already-
terminal job is a no-op (`CancelResult { count: 0 }`).

A `Leased` job that is being cancelled has its lease force-released
(audited as a synthetic `LeaseExpired` event with payload
`"cancelled"`). The original holder's subsequent heartbeat returns
**`NotLeaseHolder`** (chosen because the job state is now `Cancelled`
— a distinct terminal — rather than a returned-to-active pending).
This matches spec cross-cutting rule #9, which permits either
`NotLeaseHolder` or `LeaseExpired`; we picked `NotLeaseHolder` and
test 131 pins the choice.

### Audit log — `src/audit.rs`

Every mutating operation emits one or more `AuditEvent`s inside the
queue's critical section. Each event carries a monotonic
`event_id` (queue-lifetime unique, persisted), an `at: Instant`
timestamp, an `AuditEventKind`, optional job_id / worker_id, and a
free-form `payload` string.

Cascading cancellation emits N events that all share the same `at`,
making the cascade observable as a single atomic step. The first
event's payload is the originating reason; subsequent events tag
themselves `"cascaded from {root} ({reason})"`.

The log is bounded by `Config::audit_retention` (default 10_000). When
exceeded on a new event, the oldest is dropped from the front.
`audit_since(after)` returns events strictly newer than `after`;
`AuditError::AuditEventDropped { oldest_retained }` is returned if
`after` precedes the oldest still-retained event.

### Multi-tenant namespaces

Every job belongs to a namespace (string id). Default namespace is
`"default"`; v1.x callers continue to land there.

`active_capacity` and `dead_letter_capacity` are enforced per-namespace.
v1.x's top-level `Config::active_capacity` / `Config::dead_letter_capacity`
/ `Config::max_attempts` fields are retained as the *defaults* that any
namespace inherits unless overridden via `Config::namespace_configs` or
`Queue::register_namespace`. Two namespaces never compete for the same
capacity slot.

Dependencies cannot cross namespaces — `EnqueueRequest::depends_on` of
a job in `"alpha"` referencing a job in `"beta"` is rejected as
`DependencyError::CrossNamespace`. Consequently cancellation never
crosses namespaces either.

Workers are namespace-agnostic — one registered worker may acquire
from any namespace, subject only to capability matching.

`metrics()` returns both a queue-wide rollup and a per-namespace
breakdown (BTreeMap so iteration order is stable). The per-namespace
counters always sum to the rollup.

### Priority promotion — `Queue::promote`

`promote(job_id, new_priority)` raises a `Pending` (or
`Scheduled`-not-yet-due) job's priority. Rejected for non-pending
states, non-increasing values, and out-of-range values. Bumps the new
per-namespace `promoted_total` counter and emits a `Promoted` audit
event. The new priority takes effect immediately; a scheduled job
promoted before it becomes due acquires with the new priority once
the clock catches up.

## State machine (v2.0)

```
                enqueue                  enqueue (scheduled_at > now)
                   │                                   │
                   ▼                                   ▼
            ┌────────────┐                     ┌─────────────┐
            │  Pending   │ ◄── tick (due) ─── │  Scheduled  │
            │  +deps     │                     │ available_at│
            │  +caps     │                     └─────────────┘
            └─────┬──────┘
              acquire (deps satisfied, caps match)
                  ▼
            ┌────────────┐
            │  Leased    │  ── complete ──► Completed (terminal)
            │            │  ── fail (max) ──► DeadLetter (terminal)
            │            │  ── fail ──► RetryPending
            │            │  ── expire ──► Pending (last_holder)
            │            │  ── cancel ──► Cancelled (terminal)
            └─────┬──────┘
              cascade-cancel of dependents on terminal-failure
                  │
                  ▼
           Cancelled (terminal)
```

Active capacity counts Pending + RetryPending + Scheduled within a
namespace. Leased, Completed, Cancelled, and DeadLetter jobs are
excluded.

## Design choices

### Concurrency primitive — `parking_lot::Mutex` (unchanged from v1)

One mutex around the whole `State`. Every mutation runs `State::tick`,
applies the change, emits audit events, and snapshots to disk in a
single critical section. This is what makes the spec's atomicity
guarantees fall out for free — cascading cancellation, race-sensitive
heartbeat/cancel interactions, and the consistent-metrics-snapshot
invariant are all properties of "one lock, one act."

### Persistence — JSON snapshot via atomic rename (unchanged from v1)

Each mutation: serialize state to JSON → write `<path>.tmp` → fsync →
atomic rename → fsync parent dir (Unix). SIGKILL-safe per spec. JSON
keeps the on-disk format eyeballable, which still matters for a
comparison crate even at v2's state-size.

The v2.0 schema is **incompatible** with v1.x snapshots — `State`
gained `namespaces`, `workers`, `audit`, `cancelled_order`; `Job`
gained `namespace`, `depends_on`, `dependents`, `remaining_deps`,
`required_capabilities`; `JobState` gained `Completed`, `Cancelled`,
`DeadLetter` variants. The spec does not require snapshot
compatibility, and the cleaner schema (no special-cased "completed
removed, others stay" carve-out) was preferable.

### Terminal jobs stay in `jobs`

v1.x removed completed jobs from `jobs` immediately. v2.0 keeps them
(along with cancelled and dead-lettered) so dependency lookups remain
valid for the job's lifetime — a child enqueued before its parent
completes is matched against the parent in `jobs`, and a child whose
parent has *already* completed sees `state == Completed` and counts
its dependency as satisfied. The growth is unbounded but acceptable
at comparison-crate scale; spec test 165 covers full restart with
mixed terminal + active jobs.

The one exception: dead-letter entries removed to honor a namespace's
`dead_letter_capacity`. Those are gone — a new dependency on a
dropped id sees `Unknown`.

### No sorted index, derive at acquire time

`State::next_acquirable(worker_caps)` does one `min_by_key` over the
`jobs` HashMap, filtering for `Pending` + deps-satisfied +
caps-match, ordered by `(Reverse(priority), seq)`. O(N) where N is
total jobs, including terminals. For comparison scale (low thousands)
this is well within budget and avoids the two-sources-of-truth bug
surface a maintained priority queue would introduce. A separately-
indexed acquire scheme is the right next step if N ever climbs into
six figures.

### Auto-vs-strict worker registration

`Config::require_worker_registration` (default `false`) keeps v1.x
acquire-without-registering working. The strict mode is a one-line
opt-in for v2 callers who want every acquire to refuse unknown
workers. Either way, jobs with non-empty `required_capabilities`
always need a registered worker — the capability lookup demands one.

### Cycle detection is defensive

Under one-shot enqueue with an already-acyclic graph, no new edge can
close a cycle (the new node has only outbound edges). The DFS still
runs in `check_no_cycle` so the invariant is enforced under any
future mutability story.

### Heartbeat outcome after cancel

When cancel force-releases a lease, the original holder's next
heartbeat sees `JobState::Cancelled` and returns
**`NotLeaseHolder`** rather than `LeaseExpired`. Spec rule #9 leaves
the choice to the implementation; `NotLeaseHolder` is clearer
semantically (the job state is terminal, not "returned to active"),
and the audit log distinguishes the synthetic `LeaseExpired` event
emitted at cancel time from the natural ones via the `"cancelled"`
payload.

## Test coverage

`tests/scenarios.rs` covers every numbered spec scenario:

- **v1.0 (30)** — t1..=t30: basics, lease, retry, persistence,
  concurrency, capacity, time. All pass unchanged.
- **v1.1 (17)** — t31..=t47 (+ t47b): priority, scheduled enqueue,
  metrics, interactions. All pass unchanged.
- **v2.0 (39)** — t100..=t108 (deps), t116..=t123 (workers),
  t126..=t133 (cancel), t136..=t142 (audit), t146..=t151
  (namespaces), t156..=t160 (promote), t161..=t165 (cross-cutting).

Plus a Send/Sync compile-time check on every public type that's used
across threads, and 9 inline tests (5 Priority + 4 backoff).

```sh
cargo test                                          # 98 tests, all pass
RUSTFLAGS="-D warnings" cargo build --all-targets   # clean
```

Two v1 test-code adjustments compile-only, no behavior change:
1. `t22`'s `match` on `EnqueueError` gained `unreachable!()` arms for
   `InvalidPriority` (v1.1) and `InvalidDependency` (v2.0). The test
   logic is unchanged — those variants are never hit by v1 producers.
2. v1 fixture `small_config` and a handful of inline `Config { ... }`
   literals gained `..Config::default()` to absorb the new fields
   (`audit_retention`, `require_worker_registration`,
   `namespace_configs`). The behavior of every v1 scenario is
   identical.

## Non-goals (unchanged)

- **No async.** A tokio rewrite would be a different exercise.
- **No multi-process safety.** One `Queue` per file at a time.
- **No structured payload type.** Payloads are `Vec<u8>` — opaque.
- **No `unsafe`** anywhere in the crate.
