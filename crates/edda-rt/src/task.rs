//! Task/Executor runtime primitives — the native (non-wasm) backing for
//! `scope(exec)` / `group.spawn { ... }` / `.await`.
//!
//! Backing model: a bounded thread-pool executor,
//! replacing the earlier one-OS-thread-per-spawned-task model — the fix
//! shape allowed either for V1.0, but an unbounded thread count exhausts OS
//! threads under load. [`ThreadPool::global`] lazily starts a fixed set of
//! worker threads sized by [`pool_size`]; a spawn beyond that count queues on
//! the shared job channel instead of starting a new OS thread. Each task is a
//! heap-allocated [`TaskShared`] handed to Edda-generated code as an opaque
//! [`TaskHandle`]; a [`TaskGroup`] additionally tracks every task spawned
//! under it so `__edda_task_group_join` can join any stragglers that were
//! never individually awaited, giving "the scope cannot exit while children
//! run" (CLAUDE.md) a runtime backstop on top of the linear-typing guarantee.
//!
//! Coordinated with the MIR terminator sketch: `body_fn`
//! receives the packed argument buffer as a raw `(ptr, len)` pair (copied
//! into an owned buffer before the job is submitted to the pool, so the
//! caller may reuse or free the original buffer immediately after `spawn`
//! returns) and returns an owning pointer to its heap-allocated result —
//! leaked, same convention as [`crate::alloc_edstr`] / [`crate::abi::alloc_edslice`]:
//! there is no dealloc ABI for task results yet.
//!
//! Panics/errors: any panic that occurs while `body_fn` runs — whether from
//! an Edda-level `panic(...)` (already lowered to `__edda_panic`, which calls
//! `std::process::abort()` directly — see `io.rs`) or a genuine Rust-level
//! bug — aborts the whole process before `__edda_task_await` ever observes
//! it. This falls out of `body_fn`'s `extern "C"` ABI: Rust converts an
//! unwind attempt crossing a non-`C-unwind` FFI boundary into an immediate
//! `panic_nounwind` abort, so it happens deep inside the pool worker thread
//! and never reaches [`worker_loop`]'s own job dispatch (there is no
//! `catch_unwind` there). This matches `__edda_panic`'s existing
//! whole-process-abort model exactly (same failure mode regardless of which
//! thread panics) and trivially satisfies "do not swallow": an abort is the
//! least swallowable outcome there is. A process abort inside one job also
//! takes down every other pool worker and every in-flight task with it —
//! identical to the old one-thread-per-task model, where any task's panic
//! already aborted the whole process. Ordinary Edda `err: T` failures are not
//! a runtime concern here: per CLAUDE.md a spawn body's row carries no
//! `err: T` — a fallible body already `handle`s its own errors and surfaces
//! failure through `Outcome(T, E)` in its return value, which travels through
//! the result buffer like any other value.
//!
//! `detach` / `cancel` / `cancel_and_await` round out the
//! `std.task` four-operation consumer set alongside `await`. `cancel` sets a
//! cooperative cancellation flag on the shared task state; a worker thread
//! checks the flag immediately before running a still-queued job's `body_fn`
//! and skips the call (producing a null result) if it is already set —
//! satisfying "a cancel arriving before a queued task starts running should
//! skip it rather than run it". Once a job has actually started
//! running, the flag remains inert exactly as before: `body_fn`'s ABI has no
//! path for a running task to observe its own handle mid-run, so a
//! genuinely in-flight task still runs to completion (or process-aborts on
//! panic) regardless of `cancel`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// C-ABI shape of a lifted spawn body: takes the packed argument buffer
/// (`args_ptr`, `args_len`) and returns an owning pointer to its
/// heap-allocated result buffer.
pub type TaskBodyFn = unsafe extern "C" fn(*const u8, usize) -> *mut u8;

//   idiom for moving a raw pointer across a thread boundary once the
//   pointee's ownership has genuinely transferred to the new thread
/// Send-safe carrier for a task body's raw result pointer.
struct SendPtr(*mut u8);
unsafe impl Send for SendPtr {}

/// One unit of pool work: run a spawned task's body (unless already
/// cancelled) and record its completion.
type Job = Box<dyn FnOnce() + Send + 'static>;

//   core count) fall back to a single worker rather than propagating an
//   error — a size-1 pool still satisfies "bounded thread count, excess
//   spawns queue", just with more queuing than an accurately-sized pool
/// Worker-thread count for the global task pool.
fn pool_size() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// One worker's dispatch loop: pull jobs off the shared queue until the
/// sending half is dropped (never happens in practice — see
/// [`ThreadPool::global`]).
fn worker_loop(receiver: &Mutex<mpsc::Receiver<Job>>) {
    loop {
        let job = {
            let rx = receiver.lock().expect("task pool queue poisoned");
            rx.recv()
        };
        match job {
            Ok(job) => job(),
            Err(_) => return,
        }
    }
}

//   once, lazily, on the first call to `global`; they run for the process's
//   remaining lifetime — there is no shutdown path, matching the runtime's
//   other host-lifetime resources
//   a shared `mpsc` queue, so excess concurrent spawns queue instead of
//   growing the live OS-thread count without bound
/// The process-wide bounded thread pool backing every `__edda_task_spawn`.
struct ThreadPool {
    sender: mpsc::Sender<Job>,
}

impl ThreadPool {
    fn global() -> &'static ThreadPool {
        static POOL: OnceLock<ThreadPool> = OnceLock::new();
        POOL.get_or_init(ThreadPool::start)
    }

    fn start() -> ThreadPool {
        let (sender, receiver) = mpsc::channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));
        for _ in 0..pool_size() {
            let receiver = Arc::clone(&receiver);
            std::thread::spawn(move || worker_loop(&receiver));
        }
        ThreadPool { sender }
    }

    fn submit(&self, job: Job) {
        self.sender
            .send(job)
            .expect("task pool workers are never torn down for the process lifetime");
    }
}

//   `Completed` exactly once, from inside its job closure, then notifies
//   `TaskShared.cond`; `Taken` marks the result as already delivered (by
//   `await` / `cancel_and_await` / `group_join`) or discarded (by `detach`) —
//   no further transition happens once a task reaches `Taken`
enum TaskState {
    Pending,
    Completed(SendPtr),
    Taken,
}

//   signals completion by locking `state`, storing `Completed`, then calling
//   `cond.notify_all()`; a consumer blocks on `cond` while `state` is
//   `Pending` (see `wait_and_take`)
//   `__edda_task_cancel_and_await` and never cleared; a worker checks it
//   immediately before running a still-`Pending` job's `body_fn` and skips
//   the call if it is already set (see module docs) — once `body_fn` is
//   actually running, nothing else observes this flag
//   from the `TaskHandle` returned to the caller and from its owning
//   `TaskGroup`'s straggler list at the same time
struct TaskShared {
    state: Mutex<TaskState>,
    cond: Condvar,
    cancelled: AtomicBool,
}

//   observe `Completed` takes the pointer and leaves `Taken` behind, so a
//   second caller on the same `Arc` (e.g. an individually-awaited task later
//   drained by `__edda_task_group_join`) returns null rather than
//   double-delivering the result
fn wait_and_take(shared: &TaskShared) -> *mut u8 {
    let mut state = shared.state.lock().expect("task registry poisoned");
    loop {
        match &*state {
            TaskState::Pending => {
                state = shared.cond.wait(state).expect("task registry poisoned");
            }
            TaskState::Completed(_) => break,
            TaskState::Taken => return std::ptr::null_mut(),
        }
    }
    match std::mem::replace(&mut *state, TaskState::Taken) {
        TaskState::Completed(SendPtr(ptr)) => ptr,
        _ => unreachable!("state cannot change while the lock is held"),
    }
}

//   of completion), `result_ptr` is dropped here instead of stored — a
//   detached task's result is never observed by any consumer, consistent
//   with the module's "leaked, no dealloc ABI" convention for task results
fn complete(shared: &TaskShared, result_ptr: *mut u8) {
    let mut state = shared.state.lock().expect("task registry poisoned");
    if matches!(*state, TaskState::Taken) {
        return;
    }
    *state = TaskState::Completed(SendPtr(result_ptr));
    drop(state);
    shared.cond.notify_all();
}

//   `__edda_task_spawn` holds one strong reference (returned as this handle)
//   and, when a group is supplied, the group holds a second
/// Opaque handle to a spawned task, returned by `__edda_task_spawn` and
/// consumed by `__edda_task_await`.
pub type TaskHandle = *mut TaskShared;

//   this group that has not yet been drained by `__edda_task_group_join`;
//   an individually-awaited task is NOT removed from this list — its
//   `TaskShared.state` is simply already `Taken` by the time the group
//   drains it, so the drain is a no-op for that entry (no double join)
/// Tracks every task spawned under one `scope(exec)` group.
pub struct TaskGroup {
    children: Mutex<Vec<Arc<TaskShared>>>,
}

//   the caller may free or reuse that buffer immediately after this call
//   with it so `__edda_task_group_join` can join it if it is never
//   individually awaited; a null `group` disables that backstop for this task
//   a freshly-spawned OS thread — once every worker is busy, this task's job
//   queues on the pool's shared channel until a worker frees up
/// Spawn `body_fn(args_ptr, args_len)` on the bounded task pool and return a
/// handle joined via `__edda_task_await`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_task_spawn(
    body_fn: TaskBodyFn,
    args_ptr: *const u8,
    args_len: usize,
    group: *mut TaskGroup,
) -> TaskHandle {
    let args_owned: Vec<u8> = if args_len == 0 || args_ptr.is_null() {
        Vec::new()
    } else {
        // SAFETY: caller asserts `args_ptr` heads `args_len` initialised bytes.
        unsafe { std::slice::from_raw_parts(args_ptr, args_len) }.to_vec()
    };
    let shared = Arc::new(TaskShared {
        state: Mutex::new(TaskState::Pending),
        cond: Condvar::new(),
        cancelled: AtomicBool::new(false),
    });
    // SAFETY: caller asserts `group`, if non-null, points at a live `TaskGroup`
    // obtained from `__edda_task_group_open` and not yet joined.
    if let Some(group) = unsafe { group.as_ref() } {
        group
            .children
            .lock()
            .expect("task group poisoned")
            .push(Arc::clone(&shared));
    }
    let job_shared = Arc::clone(&shared);
    ThreadPool::global().submit(Box::new(move || {
        let result_ptr = if job_shared.cancelled.load(Ordering::SeqCst) {
            std::ptr::null_mut()
        } else {
            let ptr = args_owned.as_ptr();
            let len = args_owned.len();
            // SAFETY: `body_fn` is the lifted spawn body; `ptr`/`len` describe
            // the owned copy made above, valid for the duration of this call.
            unsafe { body_fn(ptr, len) }
        };
        complete(&job_shared, result_ptr);
    }));
    Arc::into_raw(shared) as TaskHandle
}

//   at compile time; a second `await` on the same handle is caller UB (the
//   handle no longer names a live `Arc` reference on the Edda side)
//   result pointer; a null `handle` returns a null pointer
//   this function can return — see module docs for why (extern "C" ABI
//   boundary); this is never silently swallowed
/// Block until the task named by `handle` completes and return its result
/// buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_task_await(handle: TaskHandle) -> *mut u8 {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: caller asserts `handle` is a live `TaskHandle` from
    // `__edda_task_spawn`, consumed here exactly once (linear discipline).
    let shared = unsafe { Arc::from_raw(handle as *const TaskShared) };
    wait_and_take(&shared)
}

//   queuing) on the pool independently of this call; marking the shared
//   state `Taken` up front means its eventual result is dropped rather
//   than delivered to any consumer
/// Consume `handle` without joining; the task continues running (or queuing)
/// on the pool independently.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_task_detach(handle: TaskHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: caller asserts `handle` is a live `TaskHandle` from
    // `__edda_task_spawn`, consumed here exactly once (linear discipline).
    let shared = unsafe { Arc::from_raw(handle as *const TaskShared) };
    let mut state = shared.state.lock().expect("task registry poisoned");
    *state = TaskState::Taken;
}

//   ("cancel is mutable-mode and signal-only"); borrows through the raw
//   pointer instead of reconstructing the `Arc`, so the caller keeps
//   ownership and may still `await` / `detach` / `cancel_and_await` afterward
//   observes this write if the task is still queued (see module docs) —
//   once the task's job has started running, nothing reads the flag
/// Signal cooperative cancellation for the task named by `handle`. Does not
/// block and does not consume the handle. A still-queued task skips running
/// its body; an already-running task is unaffected.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_task_cancel(handle: TaskHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: caller asserts `handle` is a live `TaskHandle` from
    // `__edda_task_spawn`, not yet consumed; `mutable`-mode means ownership
    // stays with the caller, so this borrows rather than taking the `Arc`.
    let shared = unsafe { &*handle };
    shared.cancelled.store(true, Ordering::SeqCst);
}

//   identical to `await` plus the flag write — a still-queued task then
//   skips running its body and completes immediately with a null result
/// Signal cancellation, then block until the task named by `handle`
/// completes; returns its result buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_task_cancel_and_await(handle: TaskHandle) -> *mut u8 {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: caller asserts `handle` is a live `TaskHandle` from
    // `__edda_task_spawn`, consumed here exactly once (linear discipline).
    let shared = unsafe { Arc::from_raw(handle as *const TaskShared) };
    shared.cancelled.store(true, Ordering::SeqCst);
    wait_and_take(&shared)
}

//   transfers to the caller, released by `__edda_task_group_join`
/// Open a new task group for a `scope(exec)` region.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_task_group_open() -> *mut TaskGroup {
    Box::into_raw(Box::new(TaskGroup {
        children: Mutex::new(Vec::new()),
    }))
}

//   individually awaited has completed, giving "the scope cannot exit while
//   children run" a runtime guarantee independent of the type checker
//   individually-`await`-ed task's result was already delivered to its caller
/// Join every straggler task in `group`, then free the group.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_task_group_join(group: *mut TaskGroup) {
    if group.is_null() {
        return;
    }
    // SAFETY: caller asserts `group` is a live pointer from
    // `__edda_task_group_open`, consumed here exactly once.
    let group_box = unsafe { Box::from_raw(group) };
    let children = std::mem::take(&mut *group_box.children.lock().expect("task group poisoned"));
    for shared in children {
        let _ = wait_and_take(&shared);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    unsafe extern "C" fn increment_body(ptr: *const u8, len: usize) -> *mut u8 {
        assert_eq!(len, 8, "expected an 8-byte u64 argument buffer");
        // SAFETY: test passes an 8-byte little/native-endian u64 buffer.
        let value = unsafe { std::ptr::read_unaligned(ptr as *const u64) };
        Box::into_raw(Box::new(value + 1)) as *mut u8
    }

    unsafe extern "C" fn bump_counter_body(ptr: *const u8, len: usize) -> *mut u8 {
        assert_eq!(len, 8, "expected an 8-byte pointer-address argument buffer");
        // SAFETY: test passes the address of a live `AtomicUsize` as an 8-byte
        // native-endian integer; the pointee outlives every spawned task.
        let addr = unsafe { std::ptr::read_unaligned(ptr as *const u64) } as usize;
        let counter = addr as *const AtomicUsize;
        unsafe { (*counter).fetch_add(1, Ordering::SeqCst) };
        std::ptr::null_mut()
    }

    unsafe extern "C" fn sleep_then_bump_body(ptr: *const u8, len: usize) -> *mut u8 {
        assert_eq!(len, 8, "expected an 8-byte pointer-address argument buffer");
        // SAFETY: test passes the address of a live `AtomicUsize` as an 8-byte
        // native-endian integer; the pointee outlives every spawned task.
        let addr = unsafe { std::ptr::read_unaligned(ptr as *const u64) } as usize;
        let counter = addr as *const AtomicUsize;
        std::thread::sleep(std::time::Duration::from_millis(150));
        unsafe { (*counter).fetch_add(1, Ordering::SeqCst) };
        std::ptr::null_mut()
    }

    unsafe extern "C" fn count_calls_body(ptr: *const u8, len: usize) -> *mut u8 {
        assert_eq!(len, 8, "expected an 8-byte pointer-address argument buffer");
        // SAFETY: test passes the address of a live `AtomicUsize` as an 8-byte
        // native-endian integer; the pointee outlives every spawned task.
        let addr = unsafe { std::ptr::read_unaligned(ptr as *const u64) } as usize;
        let counter = addr as *const AtomicUsize;
        unsafe { (*counter).fetch_add(1, Ordering::SeqCst) };
        std::ptr::null_mut()
    }

    unsafe extern "C" fn blocker_body(ptr: *const u8, len: usize) -> *mut u8 {
        assert_eq!(len, 8, "expected an 8-byte pointer-address argument buffer");
        // SAFETY: test passes the address of a live `AtomicBool` as an 8-byte
        // native-endian integer; the pointee outlives every spawned task.
        let addr = unsafe { std::ptr::read_unaligned(ptr as *const u64) } as usize;
        let release = addr as *const AtomicBool;
        while !unsafe { (*release).load(Ordering::SeqCst) } {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        std::ptr::null_mut()
    }

    unsafe extern "C" fn started_gate_increment_body(ptr: *const u8, len: usize) -> *mut u8 {
        assert_eq!(len, 24, "expected value + started-addr + proceed-addr arguments");
        // SAFETY: test passes a u64 value followed by the addresses of two
        // live `AtomicBool` values, all as 8-byte native-endian integers;
        // both pointees outlive every spawned task.
        let value = unsafe { std::ptr::read_unaligned(ptr as *const u64) };
        let started_addr = unsafe { std::ptr::read_unaligned((ptr as *const u64).add(1)) } as usize;
        let proceed_addr = unsafe { std::ptr::read_unaligned((ptr as *const u64).add(2)) } as usize;
        let started = started_addr as *const AtomicBool;
        let proceed = proceed_addr as *const AtomicBool;
        unsafe { (*started).store(true, Ordering::SeqCst) };
        while !unsafe { (*proceed).load(Ordering::SeqCst) } {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Box::into_raw(Box::new(value + 1)) as *mut u8
    }

    fn u64_args(value: u64) -> [u8; 8] {
        value.to_ne_bytes()
    }

    fn two_u64_args(a: u64, b: u64) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[..8].copy_from_slice(&a.to_ne_bytes());
        buf[8..].copy_from_slice(&b.to_ne_bytes());
        buf
    }

    fn three_u64_args(a: u64, b: u64, c: u64) -> [u8; 24] {
        let mut buf = [0u8; 24];
        buf[..8].copy_from_slice(&a.to_ne_bytes());
        buf[8..16].copy_from_slice(&b.to_ne_bytes());
        buf[16..].copy_from_slice(&c.to_ne_bytes());
        buf
    }

    #[test]
    fn spawn_then_await_returns_computed_value() {
        let args = u64_args(41);
        let handle = unsafe {
            __edda_task_spawn(increment_body, args.as_ptr(), args.len(), std::ptr::null_mut())
        };
        let result_ptr = unsafe { __edda_task_await(handle) };
        assert!(!result_ptr.is_null());
        // SAFETY: `increment_body` returns a leaked `Box<u64>`.
        let value = unsafe { *(result_ptr as *mut u64) };
        assert_eq!(value, 42);
        unsafe { drop(Box::from_raw(result_ptr as *mut u64)) };
    }

    #[test]
    fn await_on_null_handle_returns_null() {
        let result_ptr = unsafe { __edda_task_await(std::ptr::null_mut()) };
        assert!(result_ptr.is_null());
    }

    #[test]
    fn spawn_without_group_is_still_awaitable() {
        let args = u64_args(0);
        let handle = unsafe {
            __edda_task_spawn(increment_body, args.as_ptr(), args.len(), std::ptr::null_mut())
        };
        let result_ptr = unsafe { __edda_task_await(handle) };
        assert!(!result_ptr.is_null());
        unsafe { drop(Box::from_raw(result_ptr as *mut u64)) };
    }

    #[test]
    fn group_join_waits_for_every_unawaited_child() {
        let counter = Box::leak(Box::new(AtomicUsize::new(0))) as *const AtomicUsize;
        let args = u64_args(counter as u64);
        let group = __edda_task_group_open();
        for _ in 0..3 {
            unsafe { __edda_task_spawn(bump_counter_body, args.as_ptr(), args.len(), group) };
        }
        unsafe { __edda_task_group_join(group) };
        let final_value = unsafe { (*counter).load(Ordering::SeqCst) };
        assert_eq!(final_value, 3, "group_join returned before every child completed");
    }

    #[test]
    fn group_join_after_individual_await_does_not_double_join() {
        let group = __edda_task_group_open();
        let args = u64_args(9);
        let handle = unsafe {
            __edda_task_spawn(increment_body, args.as_ptr(), args.len(), group)
        };
        let result_ptr = unsafe { __edda_task_await(handle) };
        assert!(!result_ptr.is_null());
        unsafe { drop(Box::from_raw(result_ptr as *mut u64)) };
        // The awaited task's slot is already drained; group_join must treat
        // it as a no-op rather than joining (or panicking on) it again.
        unsafe { __edda_task_group_join(group) };
    }

    #[test]
    fn detach_returns_without_waiting_for_task_completion() {
        let counter = Box::leak(Box::new(AtomicUsize::new(0))) as *const AtomicUsize;
        let args = u64_args(counter as u64);
        let handle = unsafe {
            __edda_task_spawn(sleep_then_bump_body, args.as_ptr(), args.len(), std::ptr::null_mut())
        };
        let started = std::time::Instant::now();
        unsafe { __edda_task_detach(handle) };
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "detach blocked on the task thread instead of returning immediately"
        );
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert_eq!(
            unsafe { (*counter).load(Ordering::SeqCst) },
            1,
            "detached task never ran to completion"
        );
    }

    #[test]
    fn detach_on_null_handle_is_a_no_op() {
        unsafe { __edda_task_detach(std::ptr::null_mut()) };
    }

    #[test]
    fn group_join_after_detach_does_not_double_join() {
        let group = __edda_task_group_open();
        let args = u64_args(9);
        let handle = unsafe { __edda_task_spawn(increment_body, args.as_ptr(), args.len(), group) };
        unsafe { __edda_task_detach(handle) };
        // The detached task's slot is already drained; group_join must treat
        // it as a no-op rather than joining (or panicking on) it again.
        unsafe { __edda_task_group_join(group) };
    }

    #[test]
    fn cancel_sets_flag_and_does_not_consume_handle() {
        // A `started`/`proceed` gate pins this task past the pool's
        // before-run cancellation check (see `cancel_before_start_skips_running_the_body`)
        // before `cancel` is signalled, so the outcome is deterministic:
        // cancelling an already-running task must not swallow its result.
        let started = Box::leak(Box::new(AtomicBool::new(false))) as *const AtomicBool;
        let proceed = Box::leak(Box::new(AtomicBool::new(false))) as *const AtomicBool;
        let args = three_u64_args(41, started as u64, proceed as u64);
        let handle = unsafe {
            __edda_task_spawn(
                started_gate_increment_body,
                args.as_ptr(),
                args.len(),
                std::ptr::null_mut(),
            )
        };
        while !unsafe { (*started).load(Ordering::SeqCst) } {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        unsafe { __edda_task_cancel(handle) };
        // SAFETY: `cancel` does not consume `handle` — it is still a live
        // `TaskShared` pointer, so a direct field read is sound here.
        assert!(
            unsafe { (*handle).cancelled.load(Ordering::SeqCst) },
            "cancel did not set the cancellation flag"
        );
        unsafe { (*proceed).store(true, Ordering::SeqCst) };
        let result_ptr = unsafe { __edda_task_await(handle) };
        assert!(
            !result_ptr.is_null(),
            "cancel must not consume the handle, and cancelling an already-running \
             task must not prevent its result from being delivered"
        );
        unsafe { drop(Box::from_raw(result_ptr as *mut u64)) };
    }

    #[test]
    fn cancel_on_null_handle_is_a_no_op() {
        unsafe { __edda_task_cancel(std::ptr::null_mut()) };
    }

    #[test]
    fn cancel_and_await_sets_flag_then_joins() {
        // Same determinism rationale as `cancel_sets_flag_and_does_not_consume_handle`:
        // the gate guarantees the body is already running before `cancel_and_await`
        // signals cancellation, so the task must still run to completion.
        let started = Box::leak(Box::new(AtomicBool::new(false))) as *const AtomicBool;
        let proceed = Box::leak(Box::new(AtomicBool::new(false))) as *const AtomicBool;
        let args = three_u64_args(9, started as u64, proceed as u64);
        let handle = unsafe {
            __edda_task_spawn(
                started_gate_increment_body,
                args.as_ptr(),
                args.len(),
                std::ptr::null_mut(),
            )
        };
        while !unsafe { (*started).load(Ordering::SeqCst) } {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        unsafe { (*proceed).store(true, Ordering::SeqCst) };
        let result_ptr = unsafe { __edda_task_cancel_and_await(handle) };
        assert!(!result_ptr.is_null());
        let value = unsafe { *(result_ptr as *mut u64) };
        assert_eq!(value, 10);
        unsafe { drop(Box::from_raw(result_ptr as *mut u64)) };
    }

    #[test]
    fn cancel_and_await_on_null_handle_returns_null() {
        let result_ptr = unsafe { __edda_task_cancel_and_await(std::ptr::null_mut()) };
        assert!(result_ptr.is_null());
    }

    #[test]
    fn cancel_before_start_skips_running_the_body() {
        let bound = pool_size();
        let release = Box::leak(Box::new(AtomicBool::new(false))) as *const AtomicBool;
        let release_args = u64_args(release as u64);
        let mut blockers = Vec::with_capacity(bound);
        for _ in 0..bound {
            let handle = unsafe {
                __edda_task_spawn(
                    blocker_body,
                    release_args.as_ptr(),
                    release_args.len(),
                    std::ptr::null_mut(),
                )
            };
            blockers.push(handle);
        }
        // FIFO submission order over the pool's shared job queue guarantees
        // this target cannot be dequeued (started) before every one of the
        // `bound` blockers above it in the queue has been dequeued — and
        // since each blocker occupies its worker until `release`, dequeuing
        // all `bound` of them means every worker is now stuck on a blocker.
        // So `cancel` below is guaranteed to land before the target starts,
        // regardless of scheduling — no sleep/timing dependence needed.
        let counter = Box::leak(Box::new(AtomicUsize::new(0))) as *const AtomicUsize;
        let target_args = u64_args(counter as u64);
        let target = unsafe {
            __edda_task_spawn(
                count_calls_body,
                target_args.as_ptr(),
                target_args.len(),
                std::ptr::null_mut(),
            )
        };
        unsafe { __edda_task_cancel(target) };
        unsafe { (*release).store(true, Ordering::SeqCst) };
        let result_ptr = unsafe { __edda_task_await(target) };
        assert!(
            result_ptr.is_null(),
            "a cancelled-before-start task should complete with a null result"
        );
        assert_eq!(
            unsafe { (*counter).load(Ordering::SeqCst) },
            0,
            "a cancelled-before-start task must not run its body"
        );
        for handle in blockers {
            unsafe { __edda_task_await(handle) };
        }
    }

    #[test]
    fn pool_is_bounded_and_queues_excess_spawns() {
        let live = Box::leak(Box::new(AtomicUsize::new(0))) as *const AtomicUsize;
        let peak = Box::leak(Box::new(AtomicUsize::new(0))) as *const AtomicUsize;
        let bound = pool_size();
        let task_count = bound * 4 + 4;
        let args = two_u64_args(live as u64, peak as u64);
        let mut handles = Vec::with_capacity(task_count);
        for _ in 0..task_count {
            let handle = unsafe {
                __edda_task_spawn(
                    concurrency_probe_body,
                    args.as_ptr(),
                    args.len(),
                    std::ptr::null_mut(),
                )
            };
            handles.push(handle);
        }
        for handle in handles {
            let result_ptr = unsafe { __edda_task_await(handle) };
            assert!(result_ptr.is_null());
        }
        assert_eq!(
            unsafe { (*live).load(Ordering::SeqCst) },
            0,
            "every spawned task must finish (live count returns to zero)"
        );
        let observed_peak = unsafe { (*peak).load(Ordering::SeqCst) };
        assert!(
            observed_peak <= bound,
            "observed concurrency ({observed_peak}) exceeded the pool bound ({bound})"
        );
    }

    unsafe extern "C" fn concurrency_probe_body(ptr: *const u8, len: usize) -> *mut u8 {
        assert_eq!(len, 16, "expected two 8-byte pointer-address arguments");
        // SAFETY: test passes the addresses of two live `AtomicUsize` values
        // as 8-byte native-endian integers; both pointees outlive every
        // spawned task.
        let live_addr = unsafe { std::ptr::read_unaligned(ptr as *const u64) } as usize;
        let peak_addr = unsafe { std::ptr::read_unaligned((ptr as *const u64).add(1)) } as usize;
        let live = live_addr as *const AtomicUsize;
        let peak = peak_addr as *const AtomicUsize;
        let now = unsafe { (*live).fetch_add(1, Ordering::SeqCst) } + 1;
        let mut observed = unsafe { (*peak).load(Ordering::SeqCst) };
        while now > observed {
            match unsafe {
                (*peak).compare_exchange(observed, now, Ordering::SeqCst, Ordering::SeqCst)
            } {
                Ok(_) => break,
                Err(current) => observed = current,
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        unsafe { (*live).fetch_sub(1, Ordering::SeqCst) };
        std::ptr::null_mut()
    }
}
