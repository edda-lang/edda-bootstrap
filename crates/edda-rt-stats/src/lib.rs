//! Env-gated allocation accounting for the `__edda_*` alloc-family externs.
//!
//! Off by default: every hook is a single relaxed atomic load when
//! `EDDA_RT_ALLOC_STATS` is unset, so instrumented and uninstrumented
//! builds behave identically for A/B purposes. When set to `1`, cumulative
//! counters per allocation kind plus a power-of-two size histogram are
//! maintained, and a one-line summary is printed to stderr every 256 MiB
//! of cumulative allocation (phase-alignable against `--trace` output,
//! which also goes to stderr).
//!
//! This exists to attribute the T1 self-host memory blowup:
//! process RSS ≈ cumulative
//! allocation minus freed bytes (`__edda_free_raw` / `__edda_box_unbox_raw`
//! / the old buffer of a successful `__edda_realloc_array_raw` —
//! everything else is currently leaked), so
//! the per-kind byte streams plus the freed counters ARE the attribution
//! ledger.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// `__edda_alloc_raw` — Box-shaped single-value allocations.
pub const K_BOX: usize = 0;
/// `__edda_alloc_array_raw` — fresh array/slice allocations (Vec bufs, hashmap tables).
pub const K_ARRAY: usize = 1;
/// `__edda_realloc_array_raw` — the NEW buffer of a Vec/array growth step.
pub const K_REALLOC: usize = 2;
/// `__edda_string_concat` — concatenation result buffers.
pub const K_CONCAT: usize = 3;
/// `alloc_edstr` / `alloc_edslice` — leak-copied string/byte payloads
/// (format_* results, fs reads, io lines, etc.).
pub const K_LEAK: usize = 4;

const KINDS: usize = 5;
const NAMES: [&str; KINDS] = ["box", "array", "realloc", "concat", "strleak"];

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU64 = AtomicU64::new(0);

static COUNT: [AtomicU64; KINDS] = [ZERO; KINDS];
static BYTES: [AtomicU64; KINDS] = [ZERO; KINDS];
/// Bytes handed back via `__edda_free_raw` / `__edda_box_unbox_raw` /
/// the freed old buffer of a successful `__edda_realloc_array_raw`.
static FREED_BYTES: AtomicU64 = ZERO;
static FREED_COUNT: AtomicU64 = ZERO;
/// Cumulative allocated bytes across all kinds.
static TOTAL: AtomicU64 = ZERO;
/// Power-of-two size histogram over every recorded allocation.
static HIST: [AtomicU64; 40] = [ZERO; 40];

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("EDDA_RT_ALLOC_STATS").is_ok_and(|v| v == "1"))
}

fn bucket(bytes: u64) -> usize {
    (64 - bytes.max(1).leading_zeros() as usize).min(39)
}

/// Record one allocation of `bytes` under `kind`.
pub fn record(kind: usize, bytes: u64) {
    if !enabled() {
        return;
    }
    COUNT[kind].fetch_add(1, Ordering::Relaxed);
    BYTES[kind].fetch_add(bytes, Ordering::Relaxed);
    HIST[bucket(bytes)].fetch_add(1, Ordering::Relaxed);
    let before = TOTAL.fetch_add(bytes, Ordering::Relaxed);
    const STEP: u64 = 256 << 20;
    if (before + bytes) / STEP != before / STEP {
        dump(before + bytes);
    }
}

const ES_SLOTS: usize = 256;
/// Per-element-size realloc accounting — the element size passed to
/// `__edda_realloc_array_raw` fingerprints the monomorphised `Vec(T)`
/// (spec instances are content-addressed per T, and `size_of(T)` is
/// injected at every call site), so a per-size table attributes realloc
/// traffic to concrete collection element types without backtraces.
static ES_KEY: [AtomicU64; ES_SLOTS] = [ZERO; ES_SLOTS];
static ES_COUNT: [AtomicU64; ES_SLOTS] = [ZERO; ES_SLOTS];
static ES_BYTES: [AtomicU64; ES_SLOTS] = [ZERO; ES_SLOTS];
/// Reallocs where the new element count did not exceed the old — the
/// `into_array`-style full-size shrink/copy, not a growth step.
static COPY_COUNT: AtomicU64 = ZERO;
static COPY_BYTES: AtomicU64 = ZERO;

fn sample_sizes() -> &'static Vec<u64> {
    static SIZES: OnceLock<Vec<u64>> = OnceLock::new();
    SIZES.get_or_init(|| {
        std::env::var("EDDA_RT_ALLOC_SAMPLE")
            .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect())
            .unwrap_or_default()
    })
}

fn sample_budget_init() -> u64 {
    std::env::var("EDDA_RT_ALLOC_SAMPLE_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120)
}

fn sample_min_bytes() -> u64 {
    static MIN: OnceLock<u64> = OnceLock::new();
    *MIN.get_or_init(|| {
        std::env::var("EDDA_RT_ALLOC_SAMPLE_MIN_KIB")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(|k: u64| k << 10)
            .unwrap_or(1 << 20)
    })
}

static SAMPLES_LEFT: AtomicU64 = AtomicU64::new(u64::MAX);

/// Attribute one realloc to its element size; `grow` is false for a
/// shrink/copy (new count <= old count).
pub fn record_realloc_elem(elem_size: u64, total: u64, old_elems: u64, new_elems: u64) {
    if !enabled() {
        return;
    }
    let grow = new_elems > old_elems;
    if SAMPLES_LEFT.load(Ordering::Relaxed) == u64::MAX {
        SAMPLES_LEFT.store(sample_budget_init(), Ordering::Relaxed);
    }
    if total >= sample_min_bytes()
        && sample_sizes().contains(&elem_size)
        && SAMPLES_LEFT.load(Ordering::Relaxed) > 0
    {
        SAMPLES_LEFT.fetch_sub(1, Ordering::Relaxed);
        eprintln!(
            "edda-rt-sample: realloc es{} {} -> {} elems ({}KiB)",
            elem_size,
            old_elems,
            new_elems,
            total >> 10,
        );
    }
    if !grow {
        COPY_COUNT.fetch_add(1, Ordering::Relaxed);
        COPY_BYTES.fetch_add(total, Ordering::Relaxed);
    }
    let key = elem_size.max(1);
    let mut i = (key as usize).wrapping_mul(31) % ES_SLOTS;
    for _ in 0..ES_SLOTS {
        let k = ES_KEY[i].load(Ordering::Relaxed);
        if k == key
            || (k == 0
                && ES_KEY[i]
                    .compare_exchange(0, key, Ordering::Relaxed, Ordering::Relaxed)
                    .map_or_else(|cur| cur == key, |_| true))
        {
            ES_COUNT[i].fetch_add(1, Ordering::Relaxed);
            ES_BYTES[i].fetch_add(total, Ordering::Relaxed);
            return;
        }
        i = (i + 1) % ES_SLOTS;
    }
}

/// Record an explicit free (`__edda_free_raw` / `__edda_box_unbox_raw` /
/// the freed old buffer of a successful `__edda_realloc_array_raw`).
pub fn record_free(bytes: u64) {
    if !enabled() {
        return;
    }
    FREED_COUNT.fetch_add(1, Ordering::Relaxed);
    FREED_BYTES.fetch_add(bytes, Ordering::Relaxed);
}

fn dump(total: u64) {
    let mut line = format!("edda-rt-stats: total={}MiB", total >> 20);
    for k in 0..KINDS {
        let c = COUNT[k].load(Ordering::Relaxed);
        let b = BYTES[k].load(Ordering::Relaxed);
        line.push_str(&format!(" {}={}n/{}MiB", NAMES[k], c, b >> 20));
    }
    line.push_str(&format!(
        " freed={}n/{}MiB",
        FREED_COUNT.load(Ordering::Relaxed),
        FREED_BYTES.load(Ordering::Relaxed) >> 20,
    ));
    line.push_str(" hist=");
    let mut first = true;
    for (i, h) in HIST.iter().enumerate() {
        let n = h.load(Ordering::Relaxed);
        if n > 0 {
            if !first {
                line.push(',');
            }
            line.push_str(&format!("2^{}:{}", i, n));
            first = false;
        }
    }
    line.push_str(&format!(
        " realloc_copy={}n/{}MiB",
        COPY_COUNT.load(Ordering::Relaxed),
        COPY_BYTES.load(Ordering::Relaxed) >> 20,
    ));
    let mut tops: Vec<(u64, u64, u64)> = Vec::new();
    for i in 0..ES_SLOTS {
        let k = ES_KEY[i].load(Ordering::Relaxed);
        if k != 0 {
            tops.push((
                ES_BYTES[i].load(Ordering::Relaxed),
                k,
                ES_COUNT[i].load(Ordering::Relaxed),
            ));
        }
    }
    tops.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    line.push_str(" top_elem=");
    for (rank, (bytes, key, count)) in tops.iter().take(10).enumerate() {
        if rank > 0 {
            line.push(',');
        }
        line.push_str(&format!("es{}:{}n/{}MiB", key, count, bytes >> 20));
    }
    eprintln!("{line}");
}
