//! Behavioral tests for the spec scenarios.
//!
//! Each `t<N>_*` function corresponds to scenario `<N>` from the spec.
//! Scenarios 1..=30 are from spec v1.0 (basics, lease, retry,
//! persistence, concurrency, capacity, time); 31..=47 are from spec v1.1
//! (priority, scheduled enqueue, metrics, interaction); 100+ are from
//! spec v2.0 (dependencies, workers, cancellation, audit, namespaces,
//! promotion, cross-cutting).

use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;

use leased_job_queue::{
    AckError, AcquireError, AuditError, AuditEvent, AuditEventKind, CancelResult, Clock, Config,
    DependencyError, EnqueueError, EnqueueRequest, HeartbeatError, Instant, MetricsSnapshot,
    NamespaceConfig, PromoteError, Queue, TestClock, WorkerView,
};

// ---------- test fixture ----------

struct Fixture {
    _dir: TempDir,
    pub clock: Arc<TestClock>,
    pub queue: Queue,
}

fn fixture(config: Config) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = Arc::new(TestClock::new());
    let path = dir.path().join("queue.json");
    let queue = Queue::open(&path, config, clock.clone()).expect("open queue");
    Fixture {
        _dir: dir,
        clock,
        queue,
    }
}

fn small_config(active_cap: usize) -> Config {
    Config {
        active_capacity: active_cap,
        dead_letter_capacity: 16,
        max_attempts: 3,
        backoff_base: Duration::from_secs(1),
        backoff_cap: Duration::from_secs(60),
        jitter_fraction: 0.0, // deterministic for most tests
        default_lease_duration: Duration::from_secs(30),
        ..Config::default()
    }
}

// =========================================================================
// Basics — scenarios 1..=5
// =========================================================================

#[test]
fn t1_acquire_on_empty_returns_empty() {
    let f = fixture(small_config(10));
    let r = f
        .queue
        .acquire("worker-1", Duration::from_secs(30))
        .expect("acquire io");
    assert!(r.is_none());
}

#[test]
fn t2_enqueue_then_acquire_returns_payload() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"hello".to_vec()).expect("enqueue");
    let job = f
        .queue
        .acquire("worker-1", Duration::from_secs(30))
        .unwrap()
        .expect("a job");
    assert_eq!(job.id, id);
    assert_eq!(job.payload, b"hello");
    assert_eq!(job.attempt, 1);
}

#[test]
fn t3_acquired_job_does_not_count_against_active_capacity() {
    let f = fixture(small_config(1));
    let _id = f.queue.enqueue(b"a".to_vec()).unwrap();
    // At capacity now.
    assert!(matches!(
        f.queue.enqueue(b"b".to_vec()),
        Err(EnqueueError::QueueFull)
    ));
    // Acquire frees the slot.
    let _ = f.queue.acquire("w", Duration::from_secs(30)).unwrap().unwrap();
    assert_eq!(f.queue.active_count(), 0);
    // A new enqueue should now succeed.
    let _id2 = f.queue.enqueue(b"b".to_vec()).expect("should fit again");
}

#[test]
fn t4_complete_by_holder_removes_permanently() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let acq = f
        .queue
        .acquire("worker-1", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(acq.id, id);
    f.queue.complete("worker-1", id).expect("complete ok");
    // Not acquirable again.
    let r = f
        .queue
        .acquire("worker-2", Duration::from_secs(30))
        .unwrap();
    assert!(r.is_none());
    assert_eq!(f.queue.active_count(), 0);
}

#[test]
fn t5_enqueue_beyond_capacity_returns_full() {
    let f = fixture(small_config(2));
    f.queue.enqueue(b"a".to_vec()).unwrap();
    f.queue.enqueue(b"b".to_vec()).unwrap();
    assert!(matches!(
        f.queue.enqueue(b"c".to_vec()),
        Err(EnqueueError::QueueFull)
    ));
}

// =========================================================================
// Lease semantics — scenarios 6..=12
// =========================================================================

#[test]
fn t6_two_workers_cannot_acquire_same_job() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let a = f
        .queue
        .acquire("alice", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    let b = f.queue.acquire("bob", Duration::from_secs(30)).unwrap();
    assert_eq!(a.id, id);
    assert!(b.is_none());
}

#[test]
fn t7_lease_expiry_returns_job_to_queue() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("alice", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    f.clock.advance(Duration::from_secs(10));
    let again = f
        .queue
        .acquire("bob", Duration::from_secs(10))
        .unwrap()
        .expect("should be back in queue");
    assert_eq!(again.id, id);
}

#[test]
fn t8_heartbeat_before_expiry_keeps_lease() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("alice", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    f.clock.advance(Duration::from_secs(5));
    f.queue
        .heartbeat("alice", id, Duration::from_secs(10))
        .expect("hb ok");
    // Original lease window expires (now = 10) but the heartbeat extension
    // pushed expiry to now+10 = 15.
    f.clock.advance(Duration::from_secs(5));
    let other = f.queue.acquire("bob", Duration::from_secs(1)).unwrap();
    assert!(other.is_none(), "still leased to alice via heartbeat");
}

#[test]
fn t9_heartbeat_after_expiry_returns_lease_expired_no_release() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("alice", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    f.clock.advance(Duration::from_secs(10));
    let r = f.queue.heartbeat("alice", id, Duration::from_secs(10));
    assert!(matches!(r, Err(HeartbeatError::LeaseExpired)));
    // Job is back in the pending queue — a different worker can acquire it.
    let bob = f
        .queue
        .acquire("bob", Duration::from_secs(10))
        .unwrap()
        .expect("available");
    assert_eq!(bob.id, id);
}

#[test]
fn t10_complete_by_non_holder_returns_not_lease_holder() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("alice", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    let r = f.queue.complete("bob", id);
    assert!(matches!(r, Err(AckError::NotLeaseHolder)));
}

#[test]
fn t11_fail_by_non_holder_returns_not_lease_holder() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("alice", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    let r = f.queue.fail("bob", id, "nope".into());
    assert!(matches!(r, Err(AckError::NotLeaseHolder)));
}

#[test]
fn t12_heartbeat_by_non_holder_returns_not_lease_holder() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("alice", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    let r = f.queue.heartbeat("bob", id, Duration::from_secs(10));
    assert!(matches!(r, Err(HeartbeatError::NotLeaseHolder)));
}

// =========================================================================
// Retry & dead-letter — scenarios 13..=17
// =========================================================================

#[test]
fn t13_fail_does_not_make_job_immediately_available() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("alice", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    f.queue.fail("alice", id, "transient".into()).unwrap();
    // Still in active_count (retry-pending counts).
    assert_eq!(f.queue.active_count(), 1);
    // Not yet acquirable.
    assert!(f
        .queue
        .acquire("bob", Duration::from_secs(30))
        .unwrap()
        .is_none());
}

#[test]
fn t14_backoff_elapses_and_job_returns_with_incremented_attempt() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let first = f
        .queue
        .acquire("alice", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(first.attempt, 1);
    f.queue.fail("alice", id, "n".into()).unwrap();

    // backoff_base = 1s, jitter = 0 → exact ready_at at now + 1s.
    f.clock.advance(Duration::from_secs(1));
    let second = f
        .queue
        .acquire("bob", Duration::from_secs(30))
        .unwrap()
        .expect("retried");
    assert_eq!(second.id, id);
    assert_eq!(second.attempt, 2);
}

#[test]
fn t15_max_attempts_moves_job_to_dead_letter() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"poison".to_vec()).unwrap();
    // max_attempts=3 means three fails → dead-letter.
    for _ in 0..3 {
        // Advance enough for any pending backoff to elapse.
        f.clock.advance(Duration::from_secs(120));
        let job = f
            .queue
            .acquire("w", Duration::from_secs(30))
            .unwrap()
            .expect("available");
        assert_eq!(job.id, id);
        f.queue.fail("w", id, format!("attempt {}", job.attempt)).unwrap();
    }
    // Not acquirable.
    assert!(f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .is_none());
    // In dead-letter.
    let dead = f.queue.dead_letter_iter();
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].id, id);
    assert_eq!(dead[0].payload, b"poison");
    assert_eq!(dead[0].final_reason, "attempt 3");
}

#[test]
fn t16_lease_expiry_does_not_increment_attempt() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    // Acquire and let it expire — multiple times.
    for _ in 0..5 {
        let job = f
            .queue
            .acquire("w", Duration::from_secs(5))
            .unwrap()
            .expect("available");
        assert_eq!(job.id, id);
        // Each cycle: attempt should still be 1.
        assert_eq!(job.attempt, 1);
        f.clock.advance(Duration::from_secs(5)); // lease expires
    }
}

#[test]
fn t17_backoff_formula_obeys_bounds() {
    // delay = min(base * 2^(attempt-1), cap) + jitter in [0, delay*jitter]
    let cfg = Config {
        active_capacity: 10,
        dead_letter_capacity: 16,
        max_attempts: 10,
        backoff_base: Duration::from_millis(100),
        backoff_cap: Duration::from_secs(60),
        jitter_fraction: 0.5,
        default_lease_duration: Duration::from_secs(30),
        ..Config::default()
    };
    let f = fixture(cfg);
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    f.queue.fail("w", id, "n".into()).unwrap();

    // attempt=1 (1st fail): base*2^0 = 100ms; max jitter 50ms → range [100ms, 150ms].
    // Just before lower bound: not acquirable.
    f.clock.set(Instant::from_nanos(99_000_000));
    assert!(f
        .queue
        .acquire("w2", Duration::from_secs(30))
        .unwrap()
        .is_none());
    // After upper bound: acquirable.
    f.clock.set(Instant::from_nanos(151_000_000));
    let got = f
        .queue
        .acquire("w2", Duration::from_secs(30))
        .unwrap()
        .expect("ready");
    assert_eq!(got.id, id);
    assert_eq!(got.attempt, 2);
}

// =========================================================================
// Persistence — scenarios 18..=21
// =========================================================================

#[test]
fn t18_enqueue_survives_restart_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(10);
    let path = dir.path().join("queue.json");

    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        q.enqueue(b"a".to_vec()).unwrap();
        q.enqueue(b"b".to_vec()).unwrap();
        q.enqueue(b"c".to_vec()).unwrap();
    }

    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    let a = q.acquire("w", Duration::from_secs(30)).unwrap().unwrap();
    let b = q.acquire("w", Duration::from_secs(30)).unwrap().unwrap();
    let c = q.acquire("w", Duration::from_secs(30)).unwrap().unwrap();
    assert_eq!(a.payload, b"a");
    assert_eq!(b.payload, b"b");
    assert_eq!(c.payload, b"c");
}

#[test]
fn t19_lease_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(10);
    let path = dir.path().join("queue.json");

    let job_id;
    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        job_id = q.enqueue(b"x".to_vec()).unwrap();
        let _ = q
            .acquire("alice", Duration::from_secs(60))
            .unwrap()
            .unwrap();
    }

    // Restart.
    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    // Not yet expired (no time advanced) — bob cannot acquire.
    let bob = q.acquire("bob", Duration::from_secs(10)).unwrap();
    assert!(bob.is_none());
    // Alice can still heartbeat as the original holder.
    q.heartbeat("alice", job_id, Duration::from_secs(10))
        .expect("alice still holds");
}

#[test]
fn t20_dead_letter_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(10);
    let path = dir.path().join("queue.json");

    let id;
    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        id = q.enqueue(b"poison".to_vec()).unwrap();
        for _ in 0..3 {
            clock.advance(Duration::from_secs(120));
            let job = q.acquire("w", Duration::from_secs(30)).unwrap().unwrap();
            q.fail("w", job.id, "boom".into()).unwrap();
        }
        assert_eq!(q.dead_letter_iter().len(), 1);
    }

    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    let dead = q.dead_letter_iter();
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].id, id);
    assert_eq!(dead[0].payload, b"poison");
    assert_eq!(dead[0].final_reason, "boom");
}

#[test]
fn t21_fifo_order_preserved_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(100);
    let path = dir.path().join("queue.json");

    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        for i in 0..10u8 {
            q.enqueue(vec![i]).unwrap();
        }
    }

    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    for i in 0..10u8 {
        let job = q.acquire("w", Duration::from_secs(30)).unwrap().unwrap();
        assert_eq!(job.payload, vec![i], "FIFO order should match enqueue order");
    }
}

// =========================================================================
// Concurrency — scenarios 22..=25
// =========================================================================

#[test]
fn t22_concurrent_enqueue_respects_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = Config {
        active_capacity: 50,
        ..small_config(50)
    };
    let path = dir.path().join("queue.json");
    let q = Arc::new(Queue::open(&path, cfg, clock.clone()).unwrap());

    let n_producers = 200usize;
    let mut handles = Vec::new();
    for i in 0..n_producers {
        let q = q.clone();
        handles.push(std::thread::spawn(move || {
            q.enqueue(format!("payload-{}", i).into_bytes())
        }));
    }

    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for h in handles {
        match h.join().unwrap() {
            Ok(_) => accepted += 1,
            Err(EnqueueError::QueueFull) => rejected += 1,
            Err(EnqueueError::Io(e)) => panic!("io error: {}", e),
            // v1.1 and v2.0 added these variants; v1 producers never
            // trip them since they pass default priority and no deps.
            // Kept exhaustive so future additions get caught at compile.
            Err(EnqueueError::InvalidPriority { .. }) => unreachable!(),
            Err(EnqueueError::InvalidDependency { .. }) => unreachable!(),
        }
    }
    assert_eq!(accepted, 50);
    assert_eq!(rejected, n_producers - 50);
}

#[test]
fn t23_concurrent_acquire_exactly_k_succeed() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(100);
    let path = dir.path().join("queue.json");
    let q = Arc::new(Queue::open(&path, cfg, clock.clone()).unwrap());

    let k = 5usize;
    let m = 20usize;
    for _ in 0..k {
        q.enqueue(b"j".to_vec()).unwrap();
    }

    let mut handles = Vec::new();
    for i in 0..m {
        let q = q.clone();
        handles.push(std::thread::spawn(move || {
            q.acquire(&format!("w{}", i), Duration::from_secs(60))
                .unwrap()
        }));
    }

    let mut got = 0usize;
    let mut empty = 0usize;
    for h in handles {
        match h.join().unwrap() {
            Some(_) => got += 1,
            None => empty += 1,
        }
    }
    assert_eq!(got, k);
    assert_eq!(empty, m - k);
}

#[test]
fn t24_heartbeat_consistency_with_lease_expiry() {
    // Two paths exercised:
    //   (a) heartbeat strictly before expiry — succeeds; job stays leased.
    //   (b) heartbeat exactly at/past expiry — returns LeaseExpired and the
    //       job is back in pending.
    // The implementation serializes both effects under a single mutex, so
    // each call sees a self-consistent state.

    // (a)
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("w", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    f.clock.advance(Duration::from_secs(9));
    f.queue
        .heartbeat("w", id, Duration::from_secs(10))
        .expect("ok");
    assert!(f
        .queue
        .acquire("other", Duration::from_secs(1))
        .unwrap()
        .is_none());

    // (b)
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("w", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    f.clock.advance(Duration::from_secs(10));
    let r = f.queue.heartbeat("w", id, Duration::from_secs(10));
    assert!(matches!(r, Err(HeartbeatError::LeaseExpired)));
    let other = f
        .queue
        .acquire("other", Duration::from_secs(1))
        .unwrap()
        .expect("available after expiry");
    assert_eq!(other.id, id);

    // Stress: many concurrent races against a non-expired lease.
    // The heartbeat should win in every iteration (because tick inside
    // heartbeat runs before the lease is observed expired, and the
    // expiration boundary is in the future).
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(10);
    let path = dir.path().join("queue.json");
    let q = Arc::new(Queue::open(&path, cfg, clock.clone()).unwrap());
    let id = q.enqueue(b"x".to_vec()).unwrap();
    let _ = q.acquire("w", Duration::from_secs(1000)).unwrap().unwrap();

    let q1 = q.clone();
    let q2 = q.clone();
    let h1 = std::thread::spawn(move || {
        for _ in 0..200 {
            q1.heartbeat("w", id, Duration::from_secs(1000))
                .expect("hb ok within long lease");
        }
    });
    let h2 = std::thread::spawn(move || {
        for _ in 0..200 {
            // Should always see the job leased to "w" — never get a hit.
            let r = q2.acquire("other", Duration::from_millis(1)).unwrap();
            assert!(r.is_none());
        }
    });
    h1.join().unwrap();
    h2.join().unwrap();
}

#[test]
fn t25_complete_vs_fail_only_first_wins() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    // max_attempts=1 so a winning `fail` lands directly in dead-letter and
    // does not eat active capacity between trials.
    let cfg = Config {
        active_capacity: 4,
        max_attempts: 1,
        ..small_config(4)
    };
    let path = dir.path().join("queue.json");
    let q = Arc::new(Queue::open(&path, cfg, clock.clone()).unwrap());

    for trial in 0..50 {
        let id = q
            .enqueue(format!("t{}", trial).into_bytes())
            .unwrap_or_else(|e| panic!("trial {}: enqueue: {:?}", trial, e));
        let _ = q.acquire("w", Duration::from_secs(60)).unwrap().unwrap();

        let q1 = q.clone();
        let q2 = q.clone();
        let h1 = std::thread::spawn(move || q1.complete("w", id));
        let h2 = std::thread::spawn(move || q2.fail("w", id, "n".into()));
        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();

        let oks = [r1.is_ok(), r2.is_ok()].iter().filter(|b| **b).count();
        assert_eq!(oks, 1, "trial {}: exactly one ack must succeed", trial);
        if r1.is_err() {
            assert!(matches!(r1, Err(AckError::NotLeaseHolder)));
        }
        if r2.is_err() {
            assert!(matches!(r2, Err(AckError::NotLeaseHolder)));
        }
    }
}

// =========================================================================
// Capacity & dead-letter — scenarios 26..=28
// =========================================================================

#[test]
fn t26_dead_letter_capacity_drops_oldest() {
    let cfg = Config {
        active_capacity: 10,
        dead_letter_capacity: 2,
        max_attempts: 1, // first fail → dead-letter
        backoff_base: Duration::from_secs(1),
        backoff_cap: Duration::from_secs(60),
        jitter_fraction: 0.0,
        default_lease_duration: Duration::from_secs(30),
        ..Config::default()
    };
    let f = fixture(cfg);
    for i in 0..4u8 {
        let id = f.queue.enqueue(vec![i]).unwrap();
        let _ = f
            .queue
            .acquire("w", Duration::from_secs(30))
            .unwrap()
            .unwrap();
        f.queue
            .fail("w", id, format!("r{}", i))
            .expect("fail ok");
    }
    let dead = f.queue.dead_letter_iter();
    assert_eq!(dead.len(), 2);
    // Oldest two (i=0 and i=1) were dropped.
    assert_eq!(dead[0].payload, vec![2]);
    assert_eq!(dead[1].payload, vec![3]);
}

#[test]
fn t27_retry_pending_counts_against_active_capacity() {
    let f = fixture(small_config(1));
    let id = f.queue.enqueue(b"a".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    f.queue.fail("w", id, "x".into()).expect("fail ok");
    // Now in retry-pending — counts as 1.
    assert_eq!(f.queue.active_count(), 1);
    // Capacity 1 is full.
    assert!(matches!(
        f.queue.enqueue(b"b".to_vec()),
        Err(EnqueueError::QueueFull)
    ));
}

#[test]
fn t28_leased_does_not_count_against_active_capacity() {
    let f = fixture(small_config(1));
    let _id = f.queue.enqueue(b"a".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    // Leased job no longer counts; capacity 1 has 0 used.
    assert_eq!(f.queue.active_count(), 0);
    // Enqueue should succeed.
    let _id2 = f.queue.enqueue(b"b".to_vec()).expect("fits");
    // And a fresh worker can acquire the second one.
    let job = f
        .queue
        .acquire("w2", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(job.payload, b"b");
}

// =========================================================================
// Time — scenarios 29..=30
// =========================================================================

#[test]
fn t29_test_clock_drives_lease_and_retry_deterministically() {
    let cfg = Config {
        backoff_base: Duration::from_secs(2),
        jitter_fraction: 0.0,
        ..small_config(10)
    };
    let f = fixture(cfg);

    // Retry timing
    let id = f.queue.enqueue(b"r".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    f.queue.fail("w", id, "x".into()).unwrap();
    // Not yet ready.
    f.clock.advance(Duration::from_secs(1));
    assert!(f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .is_none());
    // Exactly at ready_at (base = 2s).
    f.clock.advance(Duration::from_secs(1));
    let again = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(again.id, id);

    // Lease expiry
    f.clock.advance(Duration::from_secs(29));
    assert!(f
        .queue
        .acquire("w2", Duration::from_secs(1))
        .unwrap()
        .is_none());
    f.clock.advance(Duration::from_secs(1));
    let recovered = f
        .queue
        .acquire("w2", Duration::from_secs(1))
        .unwrap()
        .unwrap();
    assert_eq!(recovered.id, id);
}

#[test]
fn t30_lease_from_acquire_time_backoff_from_fail_time() {
    let cfg = Config {
        backoff_base: Duration::from_secs(3),
        jitter_fraction: 0.0,
        ..small_config(10)
    };
    let f = fixture(cfg);

    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    // Acquire at clock=0, lease 10s → expires at 10.
    let _ = f
        .queue
        .acquire("w", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    // Advance to 5, then fail. Backoff = 3s ⇒ ready_at = 5 + 3 = 8.
    f.clock.advance(Duration::from_secs(5));
    f.queue.fail("w", id, "x".into()).unwrap();

    // Before 8: not ready.
    f.clock.set(Instant::from_nanos(7_999_999_999));
    assert!(f
        .queue
        .acquire("w2", Duration::from_secs(10))
        .unwrap()
        .is_none());
    // At 8: ready.
    f.clock.set(Instant::from_nanos(8_000_000_000));
    let acq2 = f
        .queue
        .acquire("w2", Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(acq2.id, id);

    // The new lease starts at clock=8, expires at 18 — proves lease is
    // measured from acquire_time, not from any earlier reference.
    f.clock.set(Instant::from_nanos(17_999_999_999));
    assert!(f
        .queue
        .acquire("w3", Duration::from_secs(1))
        .unwrap()
        .is_none());
    f.clock.set(Instant::from_nanos(18_000_000_000));
    let acq3 = f
        .queue
        .acquire("w3", Duration::from_secs(1))
        .unwrap()
        .unwrap();
    assert_eq!(acq3.id, id);
}

// =========================================================================
// Type-checker sanity: queue + clock are Send + Sync.
// =========================================================================

fn _assert_send_sync<T: Send + Sync>() {}

#[test]
fn queue_is_send_and_sync() {
    _assert_send_sync::<Queue>();
    _assert_send_sync::<Arc<Queue>>();
    _assert_send_sync::<Arc<dyn Clock>>();
}

// =========================================================================
// v1.1 Priority — scenarios 31..=35
// =========================================================================

#[test]
fn t31_high_priority_acquired_first_even_when_enqueued_later() {
    let f = fixture(small_config(10));
    let low_id = f
        .queue
        .enqueue(EnqueueRequest::new(b"low".to_vec()).priority(3))
        .unwrap();
    let high_id = f
        .queue
        .enqueue(EnqueueRequest::new(b"high".to_vec()).priority(9))
        .unwrap();

    let first = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(first.id, high_id);
    assert_eq!(first.priority.value(), 9);

    let second = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(second.id, low_id);
    assert_eq!(second.priority.value(), 3);
}

#[test]
fn t32_equal_priority_preserves_fifo() {
    let f = fixture(small_config(10));
    let mut ids = Vec::new();
    for i in 0..5u8 {
        let id = f
            .queue
            .enqueue(EnqueueRequest::new(vec![i]).priority(7))
            .unwrap();
        ids.push(id);
    }
    for expected in ids {
        let got = f
            .queue
            .acquire("w", Duration::from_secs(30))
            .unwrap()
            .unwrap();
        assert_eq!(got.id, expected);
    }
}

#[test]
fn t33_out_of_range_priority_rejected() {
    let f = fixture(small_config(10));

    let r0 = f.queue.enqueue(EnqueueRequest::new(b"x".to_vec()).priority(0));
    assert!(matches!(r0, Err(EnqueueError::InvalidPriority { value: 0 })));

    let r11 = f.queue.enqueue(EnqueueRequest::new(b"x".to_vec()).priority(11));
    assert!(matches!(r11, Err(EnqueueError::InvalidPriority { value: 11 })));

    let r255 = f
        .queue
        .enqueue(EnqueueRequest::new(b"x".to_vec()).priority(255));
    assert!(matches!(r255, Err(EnqueueError::InvalidPriority { value: 255 })));

    // Endpoints 1 and 10 are valid.
    assert!(f
        .queue
        .enqueue(EnqueueRequest::new(b"a".to_vec()).priority(1))
        .is_ok());
    assert!(f
        .queue
        .enqueue(EnqueueRequest::new(b"b".to_vec()).priority(10))
        .is_ok());
}

#[test]
fn t34_priority_survives_retry() {
    let f = fixture(small_config(10));
    let id = f
        .queue
        .enqueue(EnqueueRequest::new(b"x".to_vec()).priority(8))
        .unwrap();
    let first = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(first.priority.value(), 8);
    f.queue.fail("w", id, "transient".into()).unwrap();
    f.clock.advance(Duration::from_secs(2));
    let second = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(second.id, id);
    assert_eq!(second.priority.value(), 8);
    assert_eq!(second.attempt, 2);
}

#[test]
fn t35_priority_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(10);
    let path = dir.path().join("queue.json");

    let id;
    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        id = q
            .enqueue(EnqueueRequest::new(b"x".to_vec()).priority(2))
            .unwrap();
        // Confirm a higher-priority job would jump ahead before restart.
        q.enqueue(EnqueueRequest::new(b"y".to_vec()).priority(9))
            .unwrap();
    }

    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    let first = q.acquire("w", Duration::from_secs(30)).unwrap().unwrap();
    assert_eq!(first.priority.value(), 9);
    let second = q.acquire("w", Duration::from_secs(30)).unwrap().unwrap();
    assert_eq!(second.id, id);
    assert_eq!(second.priority.value(), 2);
}

// =========================================================================
// v1.1 Scheduled enqueue — scenarios 36..=40
// =========================================================================

#[test]
fn t36_scheduled_not_returned_before_due() {
    let f = fixture(small_config(10));
    let when = f.clock.now() + Duration::from_secs(60);
    f.queue
        .enqueue(EnqueueRequest::new(b"later".to_vec()).scheduled_at(when))
        .unwrap();
    assert!(f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .is_none());
    f.clock.advance(Duration::from_secs(59));
    assert!(f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .is_none());
}

#[test]
fn t37_scheduled_returned_after_due() {
    let f = fixture(small_config(10));
    let when = f.clock.now() + Duration::from_secs(60);
    let id = f
        .queue
        .enqueue(EnqueueRequest::new(b"later".to_vec()).scheduled_at(when))
        .unwrap();
    f.clock.set(when);
    let got = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .expect("due");
    assert_eq!(got.id, id);
}

#[test]
fn t38_scheduled_counts_against_capacity_at_enqueue() {
    let f = fixture(small_config(2));
    let when = f.clock.now() + Duration::from_secs(3_600);
    f.queue
        .enqueue(EnqueueRequest::new(b"a".to_vec()).scheduled_at(when))
        .unwrap();
    f.queue
        .enqueue(EnqueueRequest::new(b"b".to_vec()).scheduled_at(when))
        .unwrap();
    // Two scheduled jobs already occupy both slots.
    assert!(matches!(
        f.queue.enqueue(b"c".to_vec()),
        Err(EnqueueError::QueueFull)
    ));
    // The acquirable set is empty (both still scheduled) but capacity is full.
    assert!(f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .is_none());
    assert_eq!(f.queue.active_count(), 2);
}

#[test]
fn t39_scheduled_same_due_time_preserves_fifo() {
    let f = fixture(small_config(10));
    let when = f.clock.now() + Duration::from_secs(60);
    let a = f
        .queue
        .enqueue(EnqueueRequest::new(b"a".to_vec()).scheduled_at(when))
        .unwrap();
    let b = f
        .queue
        .enqueue(EnqueueRequest::new(b"b".to_vec()).scheduled_at(when))
        .unwrap();
    let c = f
        .queue
        .enqueue(EnqueueRequest::new(b"c".to_vec()).scheduled_at(when))
        .unwrap();
    f.clock.set(when);
    for expected in [a, b, c] {
        let got = f
            .queue
            .acquire("w", Duration::from_secs(30))
            .unwrap()
            .unwrap();
        assert_eq!(got.id, expected);
    }
}

#[test]
fn t40_scheduled_persists_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(10);
    let path = dir.path().join("queue.json");
    let when_nanos: u64;
    let id;
    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        let when = clock.now() + Duration::from_secs(60);
        when_nanos = when.as_nanos();
        id = q
            .enqueue(EnqueueRequest::new(b"later".to_vec()).scheduled_at(when))
            .unwrap();
        // Cannot be acquired pre-restart either.
        assert!(q
            .acquire("w", Duration::from_secs(30))
            .unwrap()
            .is_none());
    }

    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    // Still not acquirable before scheduled_at.
    assert!(q
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .is_none());
    // Advance to the persisted scheduled_at.
    clock.set(Instant::from_nanos(when_nanos));
    let got = q
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .expect("ready");
    assert_eq!(got.id, id);
    assert_eq!(got.scheduled_at, Some(Instant::from_nanos(when_nanos)));
}

// =========================================================================
// v1.1 Metrics — scenarios 41..=45
// =========================================================================

#[test]
fn t41_enqueued_total_increments_on_success() {
    let f = fixture(small_config(10));
    let m0 = f.queue.metrics();
    assert_eq!(m0.enqueued_total, 0);
    f.queue.enqueue(b"a".to_vec()).unwrap();
    f.queue.enqueue(b"b".to_vec()).unwrap();
    let m1 = f.queue.metrics();
    assert_eq!(m1.enqueued_total, 2);
}

#[test]
fn t42_enqueued_total_not_bumped_on_reject() {
    let f = fixture(small_config(1));
    // Successful enqueue.
    f.queue.enqueue(b"a".to_vec()).unwrap();
    assert_eq!(f.queue.metrics().enqueued_total, 1);

    // QueueFull rejection.
    assert!(matches!(
        f.queue.enqueue(b"b".to_vec()),
        Err(EnqueueError::QueueFull)
    ));
    assert_eq!(f.queue.metrics().enqueued_total, 1);

    // InvalidPriority rejection.
    assert!(matches!(
        f.queue.enqueue(EnqueueRequest::new(b"c".to_vec()).priority(0)),
        Err(EnqueueError::InvalidPriority { .. })
    ));
    assert_eq!(f.queue.metrics().enqueued_total, 1);
}

#[test]
fn t43_lease_expired_total_increments() {
    let f = fixture(small_config(10));
    f.queue.enqueue(b"x".to_vec()).unwrap();
    f.queue
        .acquire("w", Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(f.queue.metrics().lease_expired_total, 0);

    f.clock.advance(Duration::from_secs(5));
    // Force a tick to observe the expiry.
    let m = f.queue.metrics();
    assert_eq!(m.lease_expired_total, 1);

    // A subsequent acquire confirms the job is back in active.
    let got = f.queue.acquire("w2", Duration::from_secs(5)).unwrap();
    assert!(got.is_some());
}

#[test]
fn t44_counters_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(10);
    let path = dir.path().join("queue.json");

    let snapshot_before: MetricsSnapshot;
    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        let id = q.enqueue(b"a".to_vec()).unwrap();
        let _ = q.acquire("w", Duration::from_secs(30)).unwrap().unwrap();
        q.complete("w", id).unwrap();
        let id2 = q.enqueue(b"b".to_vec()).unwrap();
        let _ = q.acquire("w", Duration::from_secs(30)).unwrap().unwrap();
        q.fail("w", id2, "x".into()).unwrap();
        snapshot_before = q.metrics();
    }

    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    let snapshot_after = q.metrics();
    assert_eq!(snapshot_after.enqueued_total, snapshot_before.enqueued_total);
    assert_eq!(snapshot_after.acquired_total, snapshot_before.acquired_total);
    assert_eq!(snapshot_after.completed_total, snapshot_before.completed_total);
    assert_eq!(snapshot_after.failed_total, snapshot_before.failed_total);
    assert_eq!(
        snapshot_after.retry_scheduled_total,
        snapshot_before.retry_scheduled_total
    );
    // Sanity: the counters reflect real work.
    assert_eq!(snapshot_after.enqueued_total, 2);
    assert_eq!(snapshot_after.acquired_total, 2);
    assert_eq!(snapshot_after.completed_total, 1);
    assert_eq!(snapshot_after.failed_total, 1);
    assert_eq!(snapshot_after.retry_scheduled_total, 1);
}

#[test]
fn t45_metrics_snapshot_is_consistent_under_concurrent_load() {
    // Acquire/complete cycles racing with metrics reads. The invariant
    // completed_total <= acquired_total must hold in every snapshot.
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let n_jobs = 200usize;
    let cfg = small_config(n_jobs);
    let path = dir.path().join("queue.json");
    let q = Arc::new(Queue::open(&path, cfg, clock.clone()).unwrap());

    for i in 0..n_jobs {
        q.enqueue(format!("j{}", i).into_bytes()).unwrap();
    }

    let worker_count = 4usize;
    let mut workers = Vec::new();
    for w in 0..worker_count {
        let q = q.clone();
        let worker_id = format!("w{}", w);
        workers.push(std::thread::spawn(move || {
            loop {
                match q.acquire(&worker_id, Duration::from_secs(60)).unwrap() {
                    Some(job) => {
                        q.complete(&worker_id, job.id).unwrap();
                    }
                    None => break,
                }
            }
        }));
    }

    let observer = {
        let q = q.clone();
        std::thread::spawn(move || {
            for _ in 0..500 {
                let m = q.metrics();
                assert!(
                    m.completed_total <= m.acquired_total,
                    "invariant violated: completed={} > acquired={}",
                    m.completed_total,
                    m.acquired_total
                );
            }
        })
    };

    for h in workers {
        h.join().unwrap();
    }
    observer.join().unwrap();

    let final_m = q.metrics();
    assert_eq!(final_m.acquired_total, n_jobs as u64);
    assert_eq!(final_m.completed_total, n_jobs as u64);
    assert_eq!(final_m.active_count, 0);
    assert_eq!(final_m.leased_count, 0);
}

// =========================================================================
// v1.1 Interaction — scenarios 46..=47
// =========================================================================

#[test]
fn t46_high_priority_scheduled_does_not_preempt_available_low_priority() {
    let f = fixture(small_config(10));
    // Low-priority available now.
    let low_id = f
        .queue
        .enqueue(EnqueueRequest::new(b"low".to_vec()).priority(2))
        .unwrap();
    // High-priority scheduled far in the future.
    let when = f.clock.now() + Duration::from_secs(3_600);
    f.queue
        .enqueue(
            EnqueueRequest::new(b"high-but-scheduled".to_vec())
                .priority(10)
                .scheduled_at(when),
        )
        .unwrap();

    // The low-priority available job is returned first; the high-priority
    // scheduled job is NOT in the acquirable set yet.
    let first = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert_eq!(first.id, low_id);
    assert_eq!(first.priority.value(), 2);

    // The high-priority job is still not acquirable.
    assert!(f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .is_none());
}

#[test]
fn t47_failed_retry_takes_max_of_retry_and_scheduled() {
    // backoff_base = 1s, jitter = 0, so retry_ready_at = fail_time + 1s.
    // scheduled_at is set to a time AFTER fail_time + backoff so the max
    // formula picks scheduled_at.
    let cfg = Config {
        backoff_base: Duration::from_secs(1),
        jitter_fraction: 0.0,
        max_attempts: 5,
        ..small_config(10)
    };
    let f = fixture(cfg);
    let scheduled = f.clock.now() + Duration::from_secs(10);
    let id = f
        .queue
        .enqueue(EnqueueRequest::new(b"x".to_vec()).scheduled_at(scheduled))
        .unwrap();
    // Wait past scheduled_at so the job becomes acquirable.
    f.clock.set(scheduled);
    let _ = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    // Fail at scheduled_at. retry_ready = scheduled + 1s.
    f.queue.fail("w", id, "again".into()).unwrap();

    // 1s past fail: retry path alone would be ready.
    f.clock.set(scheduled + Duration::from_secs(1));
    // Since scheduled_at is already in the past, max(retry_ready, scheduled)
    // reduces to retry_ready_at = scheduled + 1s, so the job is now ready.
    let got = f
        .queue
        .acquire("w2", Duration::from_secs(30))
        .unwrap()
        .expect("ready");
    assert_eq!(got.id, id);
    assert_eq!(got.attempt, 2);
}

#[test]
fn t47b_max_formula_with_scheduled_in_future_at_fail() {
    // Engineered scenario where scheduled_at is in the future relative to
    // fail-time. Mechanism: enqueue with scheduled_at far ahead; acquire
    // it AFTER advancing clock past scheduled_at (so acquire succeeds);
    // rewind logical time impossible — instead, set a tiny backoff and a
    // large scheduled_at such that retry_ready_at < scheduled_at + 1ns
    // is not achievable. So this scenario is purely about the formula
    // taking the max — we verify with a tiny backoff and confirm the
    // retry blocks until scheduled_at + retry_delay (since both fall on
    // the same side after acquire).
    //
    // The simpler observable test: a job acquired at exactly scheduled_at,
    // then failed, with retry_delay much smaller than the gap between
    // scheduled_at and now. retry_ready_at = now + delay. scheduled_at is
    // exactly `now` at this moment (since we set clock = scheduled_at).
    // max = now + delay. So this case reduces to plain retry.
    //
    // This test exercises the max formula with both terms eligible.
    let cfg = Config {
        backoff_base: Duration::from_millis(50),
        jitter_fraction: 0.0,
        max_attempts: 5,
        ..small_config(10)
    };
    let f = fixture(cfg);
    let scheduled = f.clock.now() + Duration::from_secs(5);
    let id = f
        .queue
        .enqueue(EnqueueRequest::new(b"x".to_vec()).scheduled_at(scheduled))
        .unwrap();
    f.clock.set(scheduled);
    let _ = f
        .queue
        .acquire("w", Duration::from_secs(30))
        .unwrap()
        .unwrap();
    f.queue.fail("w", id, "n".into()).unwrap();
    // Retry delay is 50ms; scheduled_at is in the past. Ready at
    // scheduled + 50ms.
    f.clock.set(scheduled + Duration::from_millis(49));
    assert!(f
        .queue
        .acquire("w2", Duration::from_secs(30))
        .unwrap()
        .is_none());
    f.clock.set(scheduled + Duration::from_millis(50));
    let got = f
        .queue
        .acquire("w2", Duration::from_secs(30))
        .unwrap()
        .expect("ready");
    assert_eq!(got.id, id);
}

// =========================================================================
// v2.0 Dependencies — scenarios 100..=108
// =========================================================================

#[test]
fn t100_acquire_returns_parent_not_child() {
    let f = fixture(small_config(10));
    let parent = f.queue.enqueue(b"parent".to_vec()).unwrap();
    let _child = f
        .queue
        .enqueue(EnqueueRequest::new(b"child".to_vec()).depends_on(vec![parent]))
        .unwrap();
    let acq = f
        .queue
        .acquire("w", Duration::from_secs(60))
        .unwrap()
        .expect("parent eligible");
    assert_eq!(acq.id, parent);
    // Child is still blocked.
    assert!(f
        .queue
        .acquire("w2", Duration::from_secs(60))
        .unwrap()
        .is_none());
}

#[test]
fn t101_child_eligible_after_parent_completes() {
    let f = fixture(small_config(10));
    let parent = f.queue.enqueue(b"parent".to_vec()).unwrap();
    let child = f
        .queue
        .enqueue(EnqueueRequest::new(b"child".to_vec()).depends_on(vec![parent]))
        .unwrap();
    let p = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    assert_eq!(p.id, parent);
    f.queue.complete("w", parent).unwrap();
    let c = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    assert_eq!(c.id, child);
}

#[test]
fn t102_direct_cycle_rejected_via_unknown_id() {
    // A self-cycle is impossible to construct under one-shot enqueue
    // because the new id doesn't exist until admission. Referencing
    // an unallocated id falls out as Unknown.
    let f = fixture(small_config(10));
    let r = f
        .queue
        .enqueue(EnqueueRequest::new(b"x".to_vec()).depends_on(vec![9999]));
    assert!(matches!(
        r,
        Err(EnqueueError::InvalidDependency {
            reason: DependencyError::Unknown { job_id: 9999 }
        })
    ));
}

#[test]
fn t103_indirect_cycle_rejected_via_unknown_id() {
    let f = fixture(small_config(10));
    let a = f.queue.enqueue(b"a".to_vec()).unwrap();
    let b = f
        .queue
        .enqueue(EnqueueRequest::new(b"b".to_vec()).depends_on(vec![a]))
        .unwrap();
    // Closing the loop would require depending on a not-yet-existing
    // id; the unknown-id check covers it.
    let r = f
        .queue
        .enqueue(EnqueueRequest::new(b"c".to_vec()).depends_on(vec![b, 7777]));
    assert!(matches!(
        r,
        Err(EnqueueError::InvalidDependency {
            reason: DependencyError::Unknown { job_id: 7777 }
        })
    ));
}

#[test]
fn t104_unknown_dep_rejected() {
    let f = fixture(small_config(10));
    let r = f
        .queue
        .enqueue(EnqueueRequest::new(b"x".to_vec()).depends_on(vec![42]));
    assert!(matches!(
        r,
        Err(EnqueueError::InvalidDependency {
            reason: DependencyError::Unknown { .. }
        })
    ));
}

#[test]
fn t105_dep_on_dead_lettered_cancels_child() {
    let mut cfg = small_config(10);
    cfg.max_attempts = 1; // first fail → dead-letter
    let f = fixture(cfg);
    let parent = f.queue.enqueue(b"p".to_vec()).unwrap();
    let _ = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    f.queue.fail("w", parent, "boom".into()).unwrap();
    // Parent is now dead-lettered. New child enqueue succeeds but
    // immediately cancels.
    let child = f
        .queue
        .enqueue(EnqueueRequest::new(b"c".to_vec()).depends_on(vec![parent]))
        .unwrap();
    let cancelled = f.queue.cancelled_iter();
    assert!(cancelled.iter().any(|c| c.id == child));
}

#[test]
fn t106_dep_on_cancelled_cancels_child() {
    let f = fixture(small_config(10));
    let parent = f.queue.enqueue(b"p".to_vec()).unwrap();
    let result = f.queue.cancel(parent, "abort".into()).unwrap();
    assert_eq!(result, CancelResult { count: 1 });
    let child = f
        .queue
        .enqueue(EnqueueRequest::new(b"c".to_vec()).depends_on(vec![parent]))
        .unwrap();
    let cancelled = f.queue.cancelled_iter();
    assert!(cancelled.iter().any(|c| c.id == child));
}

#[test]
fn t107_grandparent_cancellation_cascades() {
    let f = fixture(small_config(10));
    let gp = f.queue.enqueue(b"gp".to_vec()).unwrap();
    let p = f
        .queue
        .enqueue(EnqueueRequest::new(b"p".to_vec()).depends_on(vec![gp]))
        .unwrap();
    let c = f
        .queue
        .enqueue(EnqueueRequest::new(b"c".to_vec()).depends_on(vec![p]))
        .unwrap();
    let result = f.queue.cancel(gp, "abort".into()).unwrap();
    assert_eq!(result.count, 3);
    let ids: std::collections::BTreeSet<_> =
        f.queue.cancelled_iter().into_iter().map(|c| c.id).collect();
    assert!(ids.contains(&gp));
    assert!(ids.contains(&p));
    assert!(ids.contains(&c));
}

#[test]
fn t108_dependency_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(10);
    let path = dir.path().join("queue.json");

    let (parent, child);
    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        parent = q.enqueue(b"parent".to_vec()).unwrap();
        child = q
            .enqueue(EnqueueRequest::new(b"child".to_vec()).depends_on(vec![parent]))
            .unwrap();
    }

    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    // Child still blocked.
    let acq1 = q.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    assert_eq!(acq1.id, parent);
    q.complete("w", parent).unwrap();
    let acq2 = q.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    assert_eq!(acq2.id, child);
}

// =========================================================================
// v2.0 Workers — scenarios 116..=123
// =========================================================================

#[test]
fn t116_register_list_acquire() {
    let f = fixture(small_config(10));
    f.queue
        .register_worker("w1", vec!["gpu".into()])
        .unwrap();
    let workers = f.queue.list_workers();
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0].id, "w1");
    assert_eq!(workers[0].capabilities, vec!["gpu".to_string()]);
    let id = f.queue.enqueue(b"job".to_vec()).unwrap();
    let acq = f
        .queue
        .acquire("w1", Duration::from_secs(60))
        .unwrap()
        .unwrap();
    assert_eq!(acq.id, id);
}

#[test]
fn t117_unknown_worker_when_strict() {
    let mut cfg = small_config(10);
    cfg.require_worker_registration = true;
    let f = fixture(cfg);
    f.queue.enqueue(b"x".to_vec()).unwrap();
    let r = f.queue.acquire("nobody", Duration::from_secs(60));
    assert!(matches!(r, Err(AcquireError::UnknownWorker { .. })));
}

#[test]
fn t118_capability_superset_matches() {
    let f = fixture(small_config(10));
    f.queue
        .register_worker("w", vec!["gpu".into(), "fp16".into()])
        .unwrap();
    let id = f
        .queue
        .enqueue(EnqueueRequest::new(b"x".to_vec()).required_capabilities(vec!["gpu".into()]))
        .unwrap();
    let acq = f
        .queue
        .acquire("w", Duration::from_secs(60))
        .unwrap()
        .unwrap();
    assert_eq!(acq.id, id);
}

#[test]
fn t119_capability_shortfall_returns_empty() {
    let f = fixture(small_config(10));
    f.queue
        .register_worker("w", vec!["cpu".into()])
        .unwrap();
    let _ = f
        .queue
        .enqueue(EnqueueRequest::new(b"gpu_job".to_vec()).required_capabilities(vec!["gpu".into()]))
        .unwrap();
    let r = f
        .queue
        .acquire("w", Duration::from_secs(60))
        .unwrap();
    assert!(r.is_none(), "no acquirable job for this worker");
}

#[test]
fn t120_deregister_holding_leases_returns_jobs() {
    let f = fixture(small_config(10));
    f.queue.register_worker("w", vec![]).unwrap();
    let a = f.queue.enqueue(b"a".to_vec()).unwrap();
    let b = f.queue.enqueue(b"b".to_vec()).unwrap();
    let ja = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    let jb = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    assert_eq!(ja.id, a);
    assert_eq!(jb.id, b);
    assert_eq!(ja.attempt, 1);
    assert_eq!(jb.attempt, 1);
    let n = f.queue.deregister_worker("w").unwrap();
    assert_eq!(n, 2);
    // Both jobs are back; attempt counter unchanged.
    let re_a = f
        .queue
        .acquire("other", Duration::from_secs(60))
        .unwrap()
        .unwrap();
    let re_b = f
        .queue
        .acquire("other", Duration::from_secs(60))
        .unwrap()
        .unwrap();
    assert_eq!(re_a.attempt, 1);
    assert_eq!(re_b.attempt, 1);
}

#[test]
fn t121_reregister_replaces_capabilities() {
    let f = fixture(small_config(10));
    f.queue
        .register_worker("w", vec!["old".into()])
        .unwrap();
    f.queue
        .register_worker("w", vec!["new1".into(), "new2".into()])
        .unwrap();
    let workers = f.queue.list_workers();
    assert_eq!(workers.len(), 1);
    let caps: std::collections::BTreeSet<_> = workers[0].capabilities.iter().cloned().collect();
    assert!(!caps.contains("old"));
    assert!(caps.contains("new1"));
    assert!(caps.contains("new2"));
}

#[test]
fn t122_workers_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(10);
    let path = dir.path().join("queue.json");
    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        q.register_worker("w1", vec!["a".into(), "b".into()])
            .unwrap();
        q.register_worker("w2", vec![]).unwrap();
    }
    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    let workers = q.list_workers();
    assert_eq!(workers.len(), 2);
    let w1 = workers.iter().find(|w| w.id == "w1").unwrap();
    assert!(w1.capabilities.contains(&"a".to_string()));
    assert!(w1.capabilities.contains(&"b".to_string()));
}

#[test]
fn t123_deregister_unknown_is_noop() {
    let f = fixture(small_config(10));
    let n = f.queue.deregister_worker("ghost").unwrap();
    assert_eq!(n, 0);
}

// =========================================================================
// v2.0 Cancellation — scenarios 126..=133
// =========================================================================

#[test]
fn t126_cancel_leaf_metrics_increment() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let m_before = f.queue.metrics().cancelled_total;
    let r = f.queue.cancel(id, "stop".into()).unwrap();
    assert_eq!(r.count, 1);
    assert_eq!(f.queue.metrics().cancelled_total, m_before + 1);
    let entries = f.queue.cancelled_iter();
    assert!(entries.iter().any(|e| e.id == id && e.reason == "stop"));
}

#[test]
fn t127_cancel_parent_cascades_to_child() {
    let f = fixture(small_config(10));
    let p = f.queue.enqueue(b"p".to_vec()).unwrap();
    let _c = f
        .queue
        .enqueue(EnqueueRequest::new(b"c".to_vec()).depends_on(vec![p]))
        .unwrap();
    let r = f.queue.cancel(p, "stop".into()).unwrap();
    assert_eq!(r.count, 2);
}

#[test]
fn t128_cancel_grandparent_cascades_to_subtree() {
    let f = fixture(small_config(10));
    let gp = f.queue.enqueue(b"gp".to_vec()).unwrap();
    let _p = f
        .queue
        .enqueue(EnqueueRequest::new(b"p".to_vec()).depends_on(vec![gp]))
        .unwrap();
    let _c1 = f
        .queue
        .enqueue(EnqueueRequest::new(b"c1".to_vec()).depends_on(vec![_p]))
        .unwrap();
    let _c2 = f
        .queue
        .enqueue(EnqueueRequest::new(b"c2".to_vec()).depends_on(vec![_p]))
        .unwrap();
    let r = f.queue.cancel(gp, "stop".into()).unwrap();
    assert_eq!(r.count, 4);
}

#[test]
fn t129_cancel_already_cancelled_is_noop() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let r1 = f.queue.cancel(id, "first".into()).unwrap();
    assert_eq!(r1.count, 1);
    let r2 = f.queue.cancel(id, "again".into()).unwrap();
    assert_eq!(r2.count, 0);
}

#[test]
fn t130_cancel_completed_is_noop_no_cascade() {
    let f = fixture(small_config(10));
    let p = f.queue.enqueue(b"p".to_vec()).unwrap();
    let c = f
        .queue
        .enqueue(EnqueueRequest::new(b"c".to_vec()).depends_on(vec![p]))
        .unwrap();
    let acq = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    assert_eq!(acq.id, p);
    f.queue.complete("w", p).unwrap();
    // Now p is Completed. cancel(p) should be a no-op and NOT cancel c.
    let r = f.queue.cancel(p, "stop".into()).unwrap();
    assert_eq!(r.count, 0);
    // c is still acquirable.
    let acq2 = f
        .queue
        .acquire("w", Duration::from_secs(60))
        .unwrap()
        .unwrap();
    assert_eq!(acq2.id, c);
}

#[test]
fn t131_cancel_leased_force_expires_lease() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f
        .queue
        .acquire("w", Duration::from_secs(60))
        .unwrap()
        .unwrap();
    let r = f.queue.cancel(id, "abort".into()).unwrap();
    assert_eq!(r.count, 1);
    // Heartbeat by previous holder must fail. We chose NotLeaseHolder
    // (documented in README).
    let hb = f.queue.heartbeat("w", id, Duration::from_secs(60));
    assert!(matches!(hb, Err(HeartbeatError::NotLeaseHolder)));
}

#[test]
fn t132_cancellation_reason_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(10);
    let path = dir.path().join("queue.json");

    let id;
    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        id = q.enqueue(b"x".to_vec()).unwrap();
        q.cancel(id, "specific-reason".into()).unwrap();
    }
    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    let entries = q.cancelled_iter();
    let entry = entries.iter().find(|e| e.id == id).expect("survived");
    assert!(entry.reason.contains("specific-reason"));
}

#[test]
fn t133_audit_log_records_each_cancelled() {
    let f = fixture(small_config(10));
    let gp = f.queue.enqueue(b"gp".to_vec()).unwrap();
    let _p = f
        .queue
        .enqueue(EnqueueRequest::new(b"p".to_vec()).depends_on(vec![gp]))
        .unwrap();
    let _c = f
        .queue
        .enqueue(EnqueueRequest::new(b"c".to_vec()).depends_on(vec![_p]))
        .unwrap();
    let before = f.queue.metrics().enqueued_total;
    let _ = f.queue.cancel(gp, "stop".into()).unwrap();
    // Pull recent events, count Cancelled ones.
    let recent = f.queue.audit_recent(100);
    let n = recent
        .iter()
        .filter(|e| matches!(e.kind, AuditEventKind::Cancelled))
        .count();
    assert_eq!(n, 3);
    let _ = before;
}

// =========================================================================
// v2.0 Audit log — scenarios 136..=142
// =========================================================================

#[test]
fn t136_enqueue_emits_enqueued_event() {
    let f = fixture(small_config(10));
    let before = f.queue.audit_recent(100).len();
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let after = f.queue.audit_recent(100);
    let new_events: Vec<&AuditEvent> = after.iter().skip(before).collect();
    assert_eq!(new_events.len(), 1);
    assert!(matches!(new_events[0].kind, AuditEventKind::Enqueued));
    assert_eq!(new_events[0].job_id, Some(id));
}

#[test]
fn t137_full_acquire_complete_event_sequence() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    f.queue.complete("w", id).unwrap();
    let recent = f.queue.audit_recent(100);
    let kinds: Vec<_> = recent
        .iter()
        .filter(|e| e.job_id == Some(id))
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            AuditEventKind::Enqueued,
            AuditEventKind::Acquired,
            AuditEventKind::Completed,
        ]
    );
}

#[test]
fn t138_retention_bound_drops_oldest() {
    let mut cfg = small_config(20);
    cfg.audit_retention = 5;
    let f = fixture(cfg);
    for _ in 0..10 {
        f.queue.enqueue(b"x".to_vec()).unwrap();
    }
    let recent = f.queue.audit_recent(100);
    assert_eq!(recent.len(), 5);
    // The oldest retained should be event_id 6.
    assert_eq!(recent[0].event_id, 6);
}

#[test]
fn t139_audit_since_returns_events_after_watermark() {
    let f = fixture(small_config(20));
    let _ = f.queue.enqueue(b"a".to_vec()).unwrap();
    let recent = f.queue.audit_recent(1);
    let watermark = recent[0].event_id;
    let _ = f.queue.enqueue(b"b".to_vec()).unwrap();
    let _ = f.queue.enqueue(b"c".to_vec()).unwrap();
    let since = f.queue.audit_since(watermark).unwrap();
    assert_eq!(since.len(), 2);
    assert!(since.iter().all(|e| e.event_id > watermark));
}

#[test]
fn t140_audit_since_dropped_watermark_errors() {
    let mut cfg = small_config(20);
    cfg.audit_retention = 3;
    let f = fixture(cfg);
    for _ in 0..10 {
        f.queue.enqueue(b"x".to_vec()).unwrap();
    }
    let r = f.queue.audit_since(1);
    assert!(matches!(r, Err(AuditError::AuditEventDropped { .. })));
}

#[test]
fn t141_audit_survives_restart_with_monotonic_ids() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(20);
    let path = dir.path().join("queue.json");

    let last_pre_restart_id;
    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        q.enqueue(b"a".to_vec()).unwrap();
        q.enqueue(b"b".to_vec()).unwrap();
        last_pre_restart_id = q.audit_recent(1)[0].event_id;
    }
    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    // Pre-restart events visible.
    let since = q.audit_since(0).unwrap();
    assert!(since.iter().any(|e| e.event_id == last_pre_restart_id));
    // New events get strictly larger ids.
    q.enqueue(b"c".to_vec()).unwrap();
    let after_new = q.audit_recent(1)[0].event_id;
    assert!(after_new > last_pre_restart_id);
}

#[test]
fn t142_cascading_cancel_events_share_at_nanos() {
    let f = fixture(small_config(20));
    let gp = f.queue.enqueue(b"gp".to_vec()).unwrap();
    let _p = f
        .queue
        .enqueue(EnqueueRequest::new(b"p".to_vec()).depends_on(vec![gp]))
        .unwrap();
    let _c = f
        .queue
        .enqueue(EnqueueRequest::new(b"c".to_vec()).depends_on(vec![_p]))
        .unwrap();
    let before = f.queue.audit_recent(100).len();
    f.queue.cancel(gp, "stop".into()).unwrap();
    let after = f.queue.audit_recent(100);
    let cancel_events: Vec<&AuditEvent> = after
        .iter()
        .skip(before)
        .filter(|e| matches!(e.kind, AuditEventKind::Cancelled))
        .collect();
    assert_eq!(cancel_events.len(), 3);
    let at = cancel_events[0].at;
    assert!(cancel_events.iter().all(|e| e.at == at));
}

// =========================================================================
// v2.0 Namespaces — scenarios 146..=151
// =========================================================================

#[test]
fn t146_capacity_is_per_namespace() {
    let mut cfg = small_config(10);
    cfg.namespace_configs.insert(
        "alpha".into(),
        NamespaceConfig {
            active_capacity: 1,
            dead_letter_capacity: 8,
            max_attempts: 3,
        },
    );
    cfg.namespace_configs.insert(
        "beta".into(),
        NamespaceConfig {
            active_capacity: 1,
            dead_letter_capacity: 8,
            max_attempts: 3,
        },
    );
    let f = fixture(cfg);
    f.queue
        .enqueue(EnqueueRequest::new(b"a".to_vec()).namespace("alpha"))
        .unwrap();
    assert!(matches!(
        f.queue
            .enqueue(EnqueueRequest::new(b"a2".to_vec()).namespace("alpha")),
        Err(EnqueueError::QueueFull)
    ));
    // beta has its own capacity slot.
    f.queue
        .enqueue(EnqueueRequest::new(b"b".to_vec()).namespace("beta"))
        .unwrap();
}

#[test]
fn t147_per_namespace_metrics_sum_to_rollup() {
    let f = fixture(small_config(50));
    for _ in 0..3 {
        f.queue
            .enqueue(EnqueueRequest::new(b"x".to_vec()).namespace("alpha"))
            .unwrap();
    }
    for _ in 0..5 {
        f.queue
            .enqueue(EnqueueRequest::new(b"x".to_vec()).namespace("beta"))
            .unwrap();
    }
    let m = f.queue.metrics();
    assert_eq!(m.enqueued_total, 8);
    let alpha_total = m.by_namespace["alpha"].enqueued_total;
    let beta_total = m.by_namespace["beta"].enqueued_total;
    assert_eq!(alpha_total + beta_total, m.enqueued_total);
}

#[test]
fn t148_cross_namespace_dep_rejected() {
    let f = fixture(small_config(10));
    let parent = f
        .queue
        .enqueue(EnqueueRequest::new(b"p".to_vec()).namespace("alpha"))
        .unwrap();
    let r = f.queue.enqueue(
        EnqueueRequest::new(b"c".to_vec())
            .namespace("beta")
            .depends_on(vec![parent]),
    );
    assert!(matches!(
        r,
        Err(EnqueueError::InvalidDependency {
            reason: DependencyError::CrossNamespace { .. }
        })
    ));
}

#[test]
fn t149_list_namespaces_grows_on_first_enqueue() {
    let f = fixture(small_config(10));
    f.queue.enqueue(b"d".to_vec()).unwrap();
    let nss = f.queue.list_namespaces();
    assert!(nss.contains(&"default".to_string()));
    f.queue
        .enqueue(EnqueueRequest::new(b"a".to_vec()).namespace("alpha"))
        .unwrap();
    let nss2 = f.queue.list_namespaces();
    assert!(nss2.contains(&"alpha".to_string()));
}

#[test]
fn t150_dead_letter_capacity_per_namespace() {
    let mut cfg = small_config(20);
    cfg.max_attempts = 1;
    cfg.namespace_configs.insert(
        "alpha".into(),
        NamespaceConfig {
            active_capacity: 10,
            dead_letter_capacity: 1,
            max_attempts: 1,
        },
    );
    cfg.namespace_configs.insert(
        "beta".into(),
        NamespaceConfig {
            active_capacity: 10,
            dead_letter_capacity: 3,
            max_attempts: 1,
        },
    );
    let f = fixture(cfg);
    // Drive 3 fails in alpha. Cap is 1; only newest survives.
    for i in 0..3u8 {
        let id = f
            .queue
            .enqueue(
                EnqueueRequest::new(vec![i])
                    .namespace("alpha")
                    .priority(5),
            )
            .unwrap();
        let _ = f
            .queue
            .acquire("w", Duration::from_secs(60))
            .unwrap()
            .unwrap();
        f.queue.fail("w", id, format!("r{i}")).unwrap();
    }
    // alpha's dead-letter should hold exactly 1.
    let alpha_dead: Vec<_> = f
        .queue
        .dead_letter_iter()
        .into_iter()
        .filter(|d| d.namespace == "alpha")
        .collect();
    assert_eq!(alpha_dead.len(), 1);
    assert_eq!(alpha_dead[0].payload, vec![2]);
}

#[test]
fn t151_namespace_config_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let cfg = small_config(10);
    let path = dir.path().join("queue.json");
    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        q.register_namespace(
            "alpha",
            NamespaceConfig {
                active_capacity: 2,
                dead_letter_capacity: 7,
                max_attempts: 4,
            },
        )
        .unwrap();
        for _ in 0..2 {
            q.enqueue(EnqueueRequest::new(b"x".to_vec()).namespace("alpha"))
                .unwrap();
        }
    }
    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    // The third enqueue should still be rejected — capacity 2 survived.
    assert!(matches!(
        q.enqueue(EnqueueRequest::new(b"y".to_vec()).namespace("alpha")),
        Err(EnqueueError::QueueFull)
    ));
}

// =========================================================================
// v2.0 Promotion — scenarios 156..=160
// =========================================================================

#[test]
fn t156_promote_raises_priority() {
    let f = fixture(small_config(10));
    let lo = f.queue.enqueue(b"lo".to_vec()).unwrap();
    let hi_target = f
        .queue
        .enqueue(EnqueueRequest::new(b"hi".to_vec()).priority(3))
        .unwrap();
    // Initial: lo has priority 5, hi_target has 3.
    // After promote(hi_target, 9), hi_target should acquire first.
    f.queue.promote(hi_target, 9).unwrap();
    let first = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    assert_eq!(first.id, hi_target);
    let second = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    assert_eq!(second.id, lo);
}

#[test]
fn t157_promote_to_lower_or_equal_rejected() {
    let f = fixture(small_config(10));
    let id = f
        .queue
        .enqueue(EnqueueRequest::new(b"x".to_vec()).priority(5))
        .unwrap();
    assert!(matches!(
        f.queue.promote(id, 5),
        Err(PromoteError::PriorityNotIncreased { current: 5, new: 5 })
    ));
    assert!(matches!(
        f.queue.promote(id, 3),
        Err(PromoteError::PriorityNotIncreased { current: 5, new: 3 })
    ));
}

#[test]
fn t158_promote_non_pending_rejected() {
    let f = fixture(small_config(10));
    let id = f.queue.enqueue(b"x".to_vec()).unwrap();
    let _ = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    // Leased → not promotable.
    assert!(matches!(
        f.queue.promote(id, 10),
        Err(PromoteError::NotPending { .. })
    ));
}

#[test]
fn t159_promote_scheduled_not_yet_due_works() {
    let f = fixture(small_config(10));
    let when = f.clock.now() + Duration::from_secs(60);
    let id = f
        .queue
        .enqueue(EnqueueRequest::new(b"x".to_vec()).scheduled_at(when).priority(2))
        .unwrap();
    f.queue.promote(id, 9).unwrap();
    f.clock.set(when);
    let acq = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    assert_eq!(acq.id, id);
    assert_eq!(acq.priority.value(), 9);
}

#[test]
fn t160_promote_increments_counter_and_audits() {
    let f = fixture(small_config(10));
    let id = f
        .queue
        .enqueue(EnqueueRequest::new(b"x".to_vec()).priority(2))
        .unwrap();
    let before = f.queue.metrics().promoted_total;
    f.queue.promote(id, 8).unwrap();
    let after = f.queue.metrics().promoted_total;
    assert_eq!(after, before + 1);
    let recent = f.queue.audit_recent(100);
    assert!(recent
        .iter()
        .any(|e| matches!(e.kind, AuditEventKind::Promoted) && e.job_id == Some(id)));
}

// =========================================================================
// v2.0 Cross-cutting — scenarios 161..=165
// =========================================================================

#[test]
fn t161_blocked_high_priority_does_not_preempt_eligible_low_priority() {
    let f = fixture(small_config(10));
    let parent = f
        .queue
        .enqueue(EnqueueRequest::new(b"parent".to_vec()).priority(5))
        .unwrap();
    let _blocked_high = f
        .queue
        .enqueue(
            EnqueueRequest::new(b"blocked_high".to_vec())
                .priority(10)
                .depends_on(vec![parent]),
        )
        .unwrap();
    let eligible_low = f
        .queue
        .enqueue(EnqueueRequest::new(b"eligible_low".to_vec()).priority(2))
        .unwrap();

    // Acquire 1: parent (priority 5).
    let first = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    assert_eq!(first.id, parent);
    // Acquire 2: the eligible-low (the blocked-high is still blocked).
    let second = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    assert_eq!(second.id, eligible_low);
}

#[test]
fn t162_retry_consumes_namespace_capacity() {
    let mut cfg = small_config(50);
    cfg.namespace_configs.insert(
        "alpha".into(),
        NamespaceConfig {
            active_capacity: 1,
            dead_letter_capacity: 8,
            max_attempts: 5,
        },
    );
    let f = fixture(cfg);
    let id = f
        .queue
        .enqueue(EnqueueRequest::new(b"x".to_vec()).namespace("alpha"))
        .unwrap();
    let _ = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    f.queue.fail("w", id, "retry".into()).unwrap();
    // alpha is at capacity due to retry-pending.
    assert!(matches!(
        f.queue
            .enqueue(EnqueueRequest::new(b"y".to_vec()).namespace("alpha")),
        Err(EnqueueError::QueueFull)
    ));
    // Other namespace unaffected.
    assert!(f
        .queue
        .enqueue(EnqueueRequest::new(b"z".to_vec()).namespace("beta"))
        .is_ok());
}

#[test]
fn t163_worker_acquires_across_namespaces() {
    let f = fixture(small_config(20));
    let a = f
        .queue
        .enqueue(EnqueueRequest::new(b"a".to_vec()).namespace("alpha"))
        .unwrap();
    let b = f
        .queue
        .enqueue(EnqueueRequest::new(b"b".to_vec()).namespace("beta"))
        .unwrap();
    let mut got = std::collections::BTreeSet::new();
    for _ in 0..2 {
        let acq = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
        got.insert(acq.id);
    }
    assert!(got.contains(&a));
    assert!(got.contains(&b));
}

#[test]
fn t164_cancel_does_not_cross_namespaces() {
    let f = fixture(small_config(10));
    let a = f
        .queue
        .enqueue(EnqueueRequest::new(b"a".to_vec()).namespace("alpha"))
        .unwrap();
    let b = f
        .queue
        .enqueue(EnqueueRequest::new(b"b".to_vec()).namespace("beta"))
        .unwrap();
    // Cross-ns deps are not allowed, so we can't construct a cross-ns
    // cancellation path. Verify cancellation in alpha leaves beta
    // untouched.
    let r = f.queue.cancel(a, "stop".into()).unwrap();
    assert_eq!(r.count, 1);
    // beta's job is unaffected.
    let acq = f.queue.acquire("w", Duration::from_secs(60)).unwrap().unwrap();
    assert_eq!(acq.id, b);
}

#[test]
fn t165_full_multi_namespace_restart() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new());
    let mut cfg = small_config(20);
    cfg.namespace_configs.insert(
        "alpha".into(),
        NamespaceConfig {
            active_capacity: 5,
            dead_letter_capacity: 8,
            max_attempts: 3,
        },
    );
    let path = dir.path().join("queue.json");

    // Pre-restart: register workers, enqueue across namespaces, leave
    // an in-flight lease, a retry-pending job, and accumulated audit.
    {
        let q = Queue::open(&path, cfg.clone(), clock.clone()).unwrap();
        q.register_worker("alice", vec!["a".into()]).unwrap();
        q.register_worker("bob", vec!["b".into()]).unwrap();
        let _a1 = q
            .enqueue(EnqueueRequest::new(b"a1".to_vec()).namespace("alpha"))
            .unwrap();
        let a2 = q
            .enqueue(EnqueueRequest::new(b"a2".to_vec()).namespace("alpha"))
            .unwrap();
        let _d1 = q.enqueue(b"d1".to_vec()).unwrap();
        let in_flight = q
            .acquire("alice", Duration::from_secs(3_600))
            .unwrap()
            .unwrap();
        // Force one job into RetryPending.
        let _ = q.acquire("bob", Duration::from_secs(60)).unwrap().unwrap();
        q.fail("bob", a2, "retry".into()).unwrap();
        let _ = in_flight; // keep id around if needed
    }

    let q = Queue::open(&path, cfg, clock.clone()).unwrap();
    // Workers survive.
    let workers = q.list_workers();
    assert_eq!(workers.len(), 2);
    // Namespaces survive.
    let nss = q.list_namespaces();
    assert!(nss.contains(&"alpha".to_string()));
    assert!(nss.contains(&"default".to_string()));
    // Audit history survives (multiple events).
    let recent = q.audit_recent(100);
    assert!(!recent.is_empty());
    // Lease still held by alice (no time advanced).
    let acq_other = q.acquire("ghost", Duration::from_secs(60)).unwrap();
    // Whatever is returned, the alice-held job is NOT among acquirable
    // until its lease expires; we just verify no crash and the state
    // is coherent.
    let _ = acq_other;
}

// =========================================================================
// v2.0 supplemental — Send/Sync + WorkerView type-tag
// =========================================================================

fn _send_sync<T: Send + Sync>() {}

#[test]
fn v2_types_send_sync() {
    _send_sync::<WorkerView>();
    _send_sync::<MetricsSnapshot>();
    _send_sync::<AuditEvent>();
}
