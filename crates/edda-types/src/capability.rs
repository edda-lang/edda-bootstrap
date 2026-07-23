//! Built-in capability type catalogue.
//!
//! Capability types represent external I/O resources. They exist only at
//! the type/effect layer — no runtime representation is defined yet.

use std::fmt;

/// A built-in capability type representing an external I/O resource.
///
/// Capability types exist only at the type/effect layer — they carry no
/// runtime representation. The canonical [`TyId`](crate::TyId) for any
/// capability type is obtained from
/// [`TyInterner::capability`](crate::TyInterner::capability).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
#[repr(u8)]
pub enum CapabilityType {
    /// Wall-clock and monotonic-clock source.
    Clock = 0,
    /// Monotonic clock, obtained from [`Clock`](CapabilityType::Clock) via `.monotonic()`.
    MonotonicClock = 1,
    /// Standard output stream.
    Stdout = 2,
    /// Standard error stream.
    Stderr = 3,
    /// Standard input stream.
    Stdin = 4,
    /// Heap allocator. Type-level surface for stdlib parameters spelled
    /// `allocator: Allocator` / `allocator: mutable Allocator`; the MIR
    /// effect-row counterpart is `edda_mir::CapabilityKind::Allocator`
    /// (classified from the lowercase `allocator` capability name).
    Allocator = 5,
    /// Filesystem capability — the read/write surface in `std.fs`. The
    /// MIR effect-row counterpart is `edda_mir::CapabilityKind::Fs`.
    /// Excluded from stable function rows per `03-verification.md` §7.
    Filesystem = 6,
    /// Network capability — sockets and address-family operations.
    /// The MIR effect-row counterpart is
    /// `edda_mir::CapabilityKind::Net`. Excluded from stable function
    /// rows per `03-verification.md` §7.
    Network = 7,
    /// Non-deterministic random source (`std.random.Random`). Every
    /// draw originates the `nondet` pure effect; the MIR effect-row
    /// counterpart is `edda_mir::CapabilityKind::Random`. Excluded
    /// from stable function rows per `03-verification.md` §7.
    Random = 8,
    /// Structured-concurrency executor. The capability that
    /// `scope(exec) group { ... }` admits per `docs/syntax/effects.md` —
    /// every spawn/await within the scope binds against this slot. No
    /// runtime representation yet; the lowering pass routes
    /// concurrent tasks through cooperative state-machine ticks.
    /// Excluded from stable function rows per `03-verification.md` §7
    /// (concurrency is non-deterministic for refinement purposes).
    Executor = 9,
    /// Read-only narrowing of [`Filesystem`](CapabilityType::Filesystem).
    /// Surface for stdlib parameters spelled `fs: ReadOnlyFilesystem`;
    /// admits stat/open-for-read/read but rejects every mutating op.
    /// Shares the MIR effect-row counterpart `edda_mir::CapabilityKind::Fs`
    /// (the runtime ABI is one opaque pointer per capability slot).
    /// Excluded from stable function rows per `03-verification.md` §7.
    ReadOnlyFilesystem = 10,
    /// Sandboxed narrowing of [`Filesystem`](CapabilityType::Filesystem).
    /// Surface for stdlib parameters spelled `fs: SandboxedFilesystem`;
    /// confines every path operation to a caller-supplied root. Shares
    /// the MIR effect-row counterpart `edda_mir::CapabilityKind::Fs`.
    /// Excluded from stable function rows per `03-verification.md` §7.
    SandboxedFilesystem = 11,
    /// Loopback-only narrowing of [`Network`](CapabilityType::Network).
    /// Surface for stdlib parameters spelled `net: LocalhostNetwork`;
    /// admits IPv4/IPv6 loopback addresses only. Shares the MIR
    /// effect-row counterpart `edda_mir::CapabilityKind::Net`.
    /// Excluded from stable function rows per `03-verification.md` §7.
    LocalhostNetwork = 12,
    /// Allow-listed narrowing of [`Network`](CapabilityType::Network).
    /// Surface for stdlib parameters spelled `net: RestrictedNetwork`;
    /// caller supplies the address/port allow-list at capability
    /// construction. Shares the MIR effect-row counterpart
    /// `edda_mir::CapabilityKind::Net`. Excluded from stable function
    /// rows per `03-verification.md` §7.
    RestrictedNetwork = 13,
    /// Quota-bounded narrowing of [`Allocator`](CapabilityType::Allocator).
    /// Surface for stdlib parameters spelled `allocator: BoundedAllocator`;
    /// the inner allocator refuses requests that would exceed the caller-
    /// supplied byte budget. Shares the MIR effect-row counterpart
    /// `edda_mir::CapabilityKind::Allocator`. Like [`Allocator`], admitted
    /// in stable function rows — the bound itself is a refinement-stable
    /// declaration of intent.
    BoundedAllocator = 14,
    /// Seeded, reproducible narrowing of [`Random`](CapabilityType::Random).
    /// Surface for stdlib parameters spelled `rng: DeterministicRandom`;
    /// draws are a pure function of the seed, so the `nondet` pure effect
    /// is not originated. Shares the MIR effect-row counterpart
    /// `edda_mir::CapabilityKind::Random`. Excluded from stable function
    /// rows per `03-verification.md` §7 (the v0.1 §7 blocklist is
    /// `CapabilityType`-shape-based; narrowed determinism does not yet
    /// promote the variant onto the whitelist).
    DeterministicRandom = 15,
    /// External-process spawning capability — the `std.os.process` surface.
    /// Holds the right to launch child processes via the four
    /// `__edda_subprocess_*` runtime intrinsics (spawn / wait / kill /
    /// detach). Per-target availability is gated by the cap-availability
    /// table (PR #B); the runtime exports land in PR #C. Observably
    /// non-deterministic and excluded from stable function rows per
    /// `03-verification.md` §7.
    Subprocess = 16,
    /// Hosted-OS debugger capability — the `std.os.debug` surface. Holds
    /// the right to attach to and control another process via the
    /// platform ptrace / `DebugActiveProcess` intrinsics (attach / detach /
    /// read / write / continue). Per-target availability is gated to the
    /// hosted operating systems (`Linux`/`Windows`/`Macos`/`Freebsd`) by
    /// the cap-availability table. Observably non-deterministic and
    /// excluded from stable function rows per `03-verification.md` §7.
    Debugger = 17,
}

/// Number of [`CapabilityType`] variants. Used by [`TyInterner`](crate::TyInterner) to size
/// its pre-allocated capability handle table; must stay in sync with the variant set.
pub(crate) const CAPABILITY_COUNT: usize = 18;

impl CapabilityType {
    /// All [`CapabilityType`] variants, in discriminant order.
    ///
    /// Order matches the interner's pre-allocated capability handle table — the
    /// `i`th element here has discriminant `i` and occupies slot `i` in
    /// `TyInterner::caps`.
    pub const ALL: [CapabilityType; CAPABILITY_COUNT] = [
        CapabilityType::Clock,
        CapabilityType::MonotonicClock,
        CapabilityType::Stdout,
        CapabilityType::Stderr,
        CapabilityType::Stdin,
        CapabilityType::Allocator,
        CapabilityType::Filesystem,
        CapabilityType::Network,
        CapabilityType::Random,
        CapabilityType::Executor,
        CapabilityType::ReadOnlyFilesystem,
        CapabilityType::SandboxedFilesystem,
        CapabilityType::LocalhostNetwork,
        CapabilityType::RestrictedNetwork,
        CapabilityType::BoundedAllocator,
        CapabilityType::DeterministicRandom,
        CapabilityType::Subprocess,
        CapabilityType::Debugger,
    ];

    /// The source spelling of this capability type (`"Clock"`, `"MonotonicClock"`, …).
    pub const fn name(self) -> &'static str {
        match self {
            CapabilityType::Clock => "Clock",
            CapabilityType::MonotonicClock => "MonotonicClock",
            CapabilityType::Stdout => "Stdout",
            CapabilityType::Stderr => "Stderr",
            CapabilityType::Stdin => "Stdin",
            CapabilityType::Allocator => "Allocator",
            CapabilityType::Filesystem => "Filesystem",
            CapabilityType::Network => "Network",
            CapabilityType::Random => "Random",
            CapabilityType::Executor => "Executor",
            CapabilityType::ReadOnlyFilesystem => "ReadOnlyFilesystem",
            CapabilityType::SandboxedFilesystem => "SandboxedFilesystem",
            CapabilityType::LocalhostNetwork => "LocalhostNetwork",
            CapabilityType::RestrictedNetwork => "RestrictedNetwork",
            CapabilityType::BoundedAllocator => "BoundedAllocator",
            CapabilityType::DeterministicRandom => "DeterministicRandom",
            CapabilityType::Subprocess => "Subprocess",
            CapabilityType::Debugger => "Debugger",
        }
    }

    /// Reverse of [`CapabilityType::name`]: parse from its source spelling.
    /// Returns `None` for any input outside the locked catalogue.
    pub fn from_name(s: &str) -> Option<CapabilityType> {
        CapabilityType::ALL.into_iter().find(|c| c.name() == s)
    }

    /// Discriminant as `usize`, for indexing the interner's pre-allocated
    /// `caps` table.
    #[inline]
    pub(crate) const fn as_index(self) -> usize {
        self as usize
    }
}

impl fmt::Display for CapabilityType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
