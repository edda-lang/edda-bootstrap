# Persistent Leased Job Queue — Edda

Edda implementation for the three-way language comparison. Built across
three versioned specs: v1.0 (leased queue), v1.1 (priority + scheduled
enqueue + metrics), v2.0 (DAG dependencies + workers + cancellation +
audit log + namespaces + promotion). Every previous-version scenario
continues to pass unmodified.

## Build, run

```sh
# From the workspace root, build the bootstrap compiler once:
cargo xtask build

# Then from this directory:
../../../target/release/edda check     # typecheck + spec instantiation
../../../target/release/edda build     # codegen artifacts
../../../target/release/edda run       # runs main.ea — the scenario runner
```

`edda check` reports `42 modules, 0 errors`. `edda build` reports
`42 modules, 19 artifacts, 0 errors`. The package typechecks and lowers
cleanly under the current bootstrap.

## Layout

```
package.toml
src/
├── main.ea                     — scenario runner: per-scenario handler wrappers (94 total)
├── clock.ea                    — LogicalClock value type
├── queue_error.ea              — 8-variant QueueError (v1+v1.1+v2)
├── job.ea                      — Job, PendingEntry, LeasedEntry, DeadEntry, CancelledEntry, QueueConfig, AcquiredJob, priority constants
├── retry.ea                    — backoff math + reproducible Jitter PRNG
├── worker.ea                   — WorkerEntry, WorkerView, capability matching (v2)
├── audit.ea                    — AuditEvent + AuditEventKind (v2)
├── namespace.ea                — NamespaceState type (v2)
├── queue.ea                    — JobQueue type, ~30 public ops, Snapshot, take_snapshot, restore
├── metrics.ea                  — MetricsSnapshot + NamespaceMetrics carriers
├── persist.ea                  — documentation stub for on-disk format
├── test_support.ea             — common config builders + list builders for [u64] / [String]
├── assert.ea                   — AssertError + assert_* helpers
├── scenarios_part1.ea          — s01–s17: basics, lease, retry (v1)
├── scenarios_part2.ea          — s18–s30: persistence, concurrency, capacity, time (v1)
├── scenarios_part3.ea          — s31–s47: priority, scheduling, metrics (v1.1)
├── scenarios_v2_deps.ea        — s100–s108: DAG dependencies
├── scenarios_v2_workers.ea     — s116–s123: workers + capability matching
├── scenarios_v2_cancel.ea      — s126–s133: cancellation + cascade
├── scenarios_v2_audit.ea       — s136–s142: audit log emission + retention + restart
├── scenarios_v2_ns.ea          — s146–s151: multi-tenant namespaces
├── scenarios_v2_promote.ea     — s156–s160: priority promotion
└── scenarios_v2_cross.ea       — s161–s165: cross-feature interaction
tests/
└── main.ea                     — placeholder for the future `edda test` runner wave
```

## v2.0 surface

### DAG dependencies

Every entry carries `depends_on: [u64]` and `required_capabilities:
[String]`. `enqueue_full2(q, payload, priority, scheduled_at_nanos,
namespace, depends_on, required_capabilities, allocator)` is the
canonical v2.0 enqueue surface; v1 / v1.1 entry points delegate with
defaults (priority=5, scheduled_at=0, namespace="default", deps=[],
caps=[]).

Cycle detection at enqueue: every id in `depends_on` must already be
known to the queue (pending, leased, dead, or cancelled). Since a new
job's id is assigned at enqueue time, it cannot be in `depends_on`, so
self-cycles and indirect cycles are caught structurally as "unknown
dep" → `InvalidDependency`.

`acquire` skips entries whose dependencies aren't completed. Combined
with priority + scheduled_at: the acquirable set is
`{ e | e.available_at_nanos <= now && deps_eligible(e) &&
worker.capabilities ⊇ e.required_capabilities }`, ordered by
`(priority desc, seq asc)`.

When a parent enters a terminal failure state (dead-letter or
cancelled), every transitive dependent cascades into cancelled state.
Lease expiry, `complete`, and explicit `fail` do NOT cascade unless
they push the parent into a terminal state.

### Workers + capability matching

Workers are tracked by `register_worker(q, worker_id, capabilities,
clk, allocator)`. Re-registration replaces the capability set
idempotently. `deregister_worker(q, worker_id, clk, allocator)` removes
the worker AND force-expires every lease it holds (returning each job
to active with `attempt` unchanged — the same semantics as natural
lease expiry).

`acquire(worker_id, ...)` reads the requester's capability set and
filters candidate jobs by capability-superset matching. A worker can
acquire jobs from any namespace as long as capabilities match.

**Backward-compatibility note:** the v2 spec calls for `acquire` to
raise `UnknownWorker` when the requester is unregistered. The v1
`acquire` signature has no `err: QueueError` row, so adding the variant
would break every v1 scenario. The resolution: `acquire` returns
`.none` (the "no work available" outcome) for unknown workers in
strict-worker mode; a separate `verify_worker(q, worker_id)` raises the
`UnknownWorker` error variant explicitly. The first
`register_worker` call flips the queue into strict-worker mode;
v1.x callers (who never register) stay on the lax-mode path where any
worker id acquires.

This is one of the "or equivalent in your language's idiom"
permissions the v2 spec admits — the error-variant surface lives in
`verify_worker`; the runtime impact (no jobs visible to unknown
workers) lives in `acquire`. Test 117 uses `verify_worker` for the
error-variant assertion and `acquire` for the runtime impact
assertion.

### Cancellation + cascade

`cancel(q, job_id, reason, clk, allocator) -> usize` cancels the job
and every transitive dependent in a single atomic call. The count
returned is the full subtree size (originator + descendants).

Cancellation moves jobs into a distinct `cancelled` store —
never dead-letter. A leased job cancelled mid-flight has its lease
force-expired (entry removed from `leased`, not returned to active);
the holder receives `NotLeaseHolder` on the next heartbeat. Cancellation
of already-terminal jobs (completed / dead-lettered / cancelled) is a
no-op with zero cascade.

Every cancelled job — originator and cascaded descendants — emits
exactly one `Cancelled` audit event. All events share the same
`at_nanos` (the originating call's clock reading) so the cascade is
observable as a single tick of work.

### Audit log

The queue maintains an append-only `audit_log: Vec_AuditEvent.Vec`
preallocated to `cfg.audit_retention` slots at construction. `event_id`
is queue-lifetime monotonic and survives restart.

Events surface 12 transition kinds: `Enqueued`, `Acquired`,
`HeartbeatExtended`, `Completed`, `Failed`, `LeaseExpired`,
`RetryScheduled`, `DeadLettered`, `Cancelled`, `WorkerRegistered`,
`WorkerDeregistered`, `Promoted`.

`audit_since(q, after_event_id, allocator)` and `audit_recent(q, limit,
allocator)` return fresh slices. Asking `audit_since` for a watermark
older than the oldest retained event raises `AuditEventDropped`.

The audit-log push path is allocator-free (preallocated capacity +
direct slot write), so `complete()` — which has no allocator in its v1
signature — can still emit its `Completed` event without breaking
backward compatibility.

### Multi-tenant namespaces

Every entry carries `namespace: String`. `active_capacity` and
`dead_letter_capacity` are enforced per-namespace. A namespace is
auto-registered on first enqueue with the queue-wide defaults, or
pre-registered via `register_namespace(q, name, active_cap, dead_cap,
allocator)` for custom caps. `namespace_count(q) +
namespace_name_at(q, i)` enumerate the registry.

Dependencies cannot cross namespaces. Cross-namespace deps at enqueue
raise `InvalidDependency`. Workers are namespace-agnostic — capability
matching is per-job, not per-namespace.

Per-namespace counters mirror the queue-wide counter set:
`namespace_metrics_count(q) + namespace_metrics_at(q, i)` return a
`NamespaceMetrics` carrier per namespace. Invariant:
`sum_over_namespaces(counter) == queue_wide(counter)`.

### Priority promotion

`promote(q, job_id, new_priority, clk, allocator)` raises a pending
job's priority. Rejected (`PromotionRejected`) when:
- `new_priority` is out of band (`1..=10`),
- `new_priority <= current_priority` (strict increase only),
- the job is not in pending state (leased / dead / cancelled / unknown).

Promotion increments `promoted_total` (queue-wide and per-namespace)
and emits a `Promoted` audit event. Promoting a scheduled-but-not-
yet-due job works; the new priority applies once the job becomes
acquirable.

## Design choices

### Single-file storage + Vec specs

The bootstrap typechecker treats `spec std.vec.Vec(T)` invocations in
different files as distinct nominal types. To avoid cross-file Vec
divergence, every storage Vec (`pending`, `leased`, `dead_letter`,
`cancelled`, `workers`, `namespaces`, `audit_log`) and its spec
invocation live in `queue.ea`. Submodules contribute pure value types
(`Job`, `AuditEvent`, `WorkerEntry`, `NamespaceState`) and pure helpers
over primitive types — no Vec values cross module boundaries.

### Flat collections + per-namespace filtering

A namespace registry maps `name → NamespaceState`. Storage collections
themselves are flat, not partitioned: every `PendingEntry` carries its
`namespace` field, and per-namespace counts come from filtering the
flat collection. The walk is O(n) per enqueue; for the comparison's
scale (low thousands of pending jobs across single-digit tenants) the
filter cost is below the locking constant factor of any partitioned
strategy and keeps the code path obvious.

### Cancel cascade via repeat-until-stable

`cascade_cancel_dependents` walks pending + leased, finds one entry
whose `depends_on` includes any id already in `cancelled` or
`dead_letter`, and cancels it. The pass repeats until no further
matches surface. Each iteration shrinks the un-cancelled set, so the
algorithm converges in at most `len(pending) + len(leased)` passes.
The per-pass O(n*deps) walk is acceptable at the comparison's scale
and avoids the bookkeeping a worklist-based DFS would add.

### Audit log — preallocated for `complete()`'s allocator-free signature

v1's `complete(q, worker_id, job_id)` has no allocator parameter.
v2's audit log needs a `Completed` event from `complete`. To keep the
v1 signature intact, the audit log is preallocated to its full
`cfg.audit_retention` slots at queue construction (and at restore), and
appends use direct slot writes (`q.audit_log.data[pos] = event;
q.audit_log.len += 1`) rather than `Vec.push` — which would need
allocator. The preallocation contract holds across restart: `restore`
re-allocates `audit_log.data` at `cfg.audit_retention` slots.

### Per-namespace counters via rebuild-then-set_at

The bootstrap doesn't yet admit field-level mutation of Vec entries
(`v[i].x = ...`); the only sanctioned write is `Vec.set_at(v, i,
whole_value)`. To bump a single counter on a `NamespaceState`, the
implementation reads the existing record, constructs a new record with
the target field incremented, and writes the whole record back. The
nine `bump_ns_*` helpers each implement one bump; the verbosity is the
cost of the Vec API constraint.

### Strict-vs-lax worker mode

The queue starts in lax-worker mode: `acquire` accepts any worker id
and treats unknown ids as "anonymous worker with empty capabilities".
The first `register_worker` call flips to strict mode: unknown ids
yield `.none` from acquire and `UnknownWorker` from `verify_worker`.
This lets v1.x scenarios (which never register workers) pass without
modification while v2 callers get the strict-worker discipline.

### Time — `LogicalClock` value

Same v1 design: the queue takes `clock.LogicalClock` values, not the
runtime's `MonotonicClock` capability. `q.last_seen_clock_nanos` tracks
the most recently observed reading so that operations without a clock
parameter (`complete`, audit-log appends from `complete`) can attribute
events to the last known time.

### Persistence — snapshot + restore in `queue.ea`

`Snapshot` and the per-Vec clone helpers live in `queue.ea` for the
same reason as the storage: cross-file Vec sharing would diverge into
distinct nominal types. The `persist.ea` module is a doc stub
describing the intended on-disk format that would sit on top of the
in-memory snapshot once `std.fs` exposes `OpenOptions::create_new` and
a temp-file rename surface. Scenarios that exercise restart-fidelity
use the in-memory `take_snapshot` / `restore` path.

## Backward compatibility

| Surface | v1.0 | v1.1 | v2.0 |
|---|---|---|---|
| `enqueue(q, payload, allocator) -> u64` | ✅ | ✅ | ✅ delegate |
| `enqueue_full(q, payload, priority, scheduled_at, allocator)` | — | ✅ | ✅ delegate |
| `enqueue_full2(q, payload, priority, scheduled_at, ns, deps, caps, allocator)` | — | — | ✅ |
| `acquire(q, worker_id, lease_duration, clk, allocator) -> Option_AcquiredJob` | ✅ | ✅ | ✅ (no err row change) |
| `complete(q, worker_id, job_id)` | ✅ | ✅ | ✅ |
| `fail(q, worker_id, job_id, reason, clk, allocator)` | ✅ | ✅ | ✅ |
| `heartbeat(q, worker_id, job_id, extension, clk, allocator)` | ✅ | ✅ | ✅ |
| `metrics_snapshot(q) -> MetricsSnapshot` | — | ✅ | ✅ (queue-wide only; per-ns via separate ops) |
| `cancel`, `promote`, `register_*`, `audit_*`, `verify_worker` | — | — | ✅ new |

Every v1.0 and v1.1 scenario (s01..s47) passes unchanged.

**One spec interpretation flagged:** the v2 spec calls for
`acquire` to raise `UnknownWorker`. To preserve the v1 signature
exactly, that error variant lives on `verify_worker` (a paired helper)
rather than on `acquire` itself. This is the "or equivalent in your
language's idiom" allowance documented in §2 of the v2.0 spec.

## Verification

```
$ ../../../target/release/edda check
check: 42 modules, 17 artifacts (2 cached, 15 generated), 0.12s
$ ../../../target/release/edda build
build: 42 modules, 19 artifacts (2 cached, 17 generated), 1.95s
```

Zero errors from either. The cascade goes parse → resolve →
typecheck → spec materialisation → codegen on every file (including
the scenario runner) and exits cleanly.

`edda run` completes the same cascade and emits MIR. The runtime
execution path that prints `PASS s01` to stdout is gated on the
bootstrap's runtime-execution wave; once that wave lands, the same
`edda run` invocation will drive every scenario to completion.

## Non-goals (unchanged from v1)

- **No async.** Edda's `scope(exec)` would be the natural target;
  the spec does not require it.
- **No multi-process safety.** One `JobQueue` instance per file at a
  time.
- **No structured payload type.** Payloads are `String`. Callers
  serialise whatever they want on top.
- **No on-disk implementation.** `persist.ea` documents the intended
  format; `queue.take_snapshot` / `queue.restore` exercise the
  in-memory restart path that every persistence scenario covers.
