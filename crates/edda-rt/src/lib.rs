//! Edda runtime: C-ABI bridge symbols for stdlib I/O and string operations.
//!
//! Implements every `__edda_*` symbol the bootstrap compiler links Edda programs against.
//! String values use the Edda fat-pointer ABI: `EdStr = { ptr: *mut u8, len: u64 }`.
//! Result types use the Edda sum-ADT ABI: `{ tag: u8, <max-payload> }` where tag 0 = Ok
//! and tag 1 = Err, with the error ADT bytes at the leading bytes of the payload slot.
//!
//! Memory: the alloc-family externs (`__edda_alloc_raw` / `__edda_alloc_array_raw` /
//! `__edda_free_raw` / `__edda_box_unbox_raw` / `__edda_realloc_array_raw`) carry an explicit
//! dealloc ABI — `free` / `unbox` reconstruct the original `Layout` from the call-site
//! `(size, align)` pair and hand it back to the global allocator, and `realloc_array` frees
//! the old backing buffer the same way on its success path.
//! The convenience constructors
//! `alloc_edstr` / `alloc_edslice` that back the I/O, string, format, fs, process,
//! and subprocess result payloads have no reclaim path and are intentionally leaked: those
//! payloads outlive the call and Edda has no dealloc ABI for them yet. This split is
//! sufficient for the bootstrap demo programs.
//!
//! All exported symbols use `#[unsafe(no_mangle)] extern "C"` to match lld-link's lookup.
//!
//! The runtime surface is decomposed by domain: `abi` holds the shared
//! `EdStr` / `EdSlice` fat-pointer wire types and their heap-leaking
//! constructors, re-exported at the crate root so submodules import them as
//! `crate::*`; each remaining module owns one `__edda_*` extern family.

#![allow(private_interfaces)] // ABI types are FFI-only; Rust visibility rules don't apply

mod abi;

mod alloc;
mod bits;
mod byteorder;
mod crypto;
mod encoding;
mod fs;
mod io;
mod math;
mod process;
mod random;
mod raw;
mod socket;
mod string;
mod subprocess;
mod task;
mod time;

// Shared ABI types live in `abi` but are referenced as `crate::*` across the
// domain and crypto/socket/subprocess submodules — re-export to preserve that.
pub(crate) use abi::{EdSlice, EdStr, alloc_edstr};

// The six shadowable alloc-family externs and the alloc-stats ledger live
// in `edda-rt-alloc` so they occupy their own archive member of this
// staticlib and are never force-pulled alongside the io/str/panic symbols
// when `std.mem.rt` shadows them. The
// `use` keeps the crate in the link graph; `stats` is re-exported so the
// remaining `crate::stats::record` call sites keep resolving.
pub(crate) use edda_rt_stats as stats;

//   target keeps the platform default allocator so edda-rt stays pure-Rust
//   cross-compilable (no C toolchain), per the crate's dependency policy
//   `__edda_*` alloc family, `alloc_edstr` / `alloc_edslice`, and any
//   internal Rust-side allocation) routes through mimalloc once this is
//   linked into an Edda binary — there is no second heap to keep coherent
//   because `__edda_free_raw` / `__edda_realloc_array_raw` reconstruct the
//   same `Layout` and hand it to the same global allocator
//   Edda-linked binary — the NT back-end heap's free lists degrade under the
//   native compiler's medium-block churn (observed:
//   late-sequence emit work 4-11x slower than standalone at a ~9 GB working
//   set while small LFH-bucket allocations stay fast)
/// mimalloc as the process-global allocator for Edda-linked binaries.
#[cfg(all(target_env = "msvc", target_arch = "x86_64"))]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;
