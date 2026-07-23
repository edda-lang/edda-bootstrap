//! Locked target-triple grammar: `<arch>-<os>-<abi-variant>`.
//!
//! All three component sets are spec-locked at build-system.md §9.

use std::fmt;
use std::str::FromStr;

/// Locked set of build-target architectures (build-system.md §9).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
#[repr(u8)]
pub enum Arch {
    /// 64-bit x86. Spec spelling: `x86-64`.
    X86_64,
    /// 64-bit ARM. Spec spelling: `aarch64`.
    Aarch64,
    /// 64-bit RISC-V. Spec spelling: `riscv64`.
    Riscv64,
    /// WebAssembly with 32-bit address space. Spec spelling: `wasm32`.
    Wasm32,
    /// WebAssembly with 64-bit address space. Spec spelling: `wasm64`.
    Wasm64,
}

/// Number of locked architectures. Bumps require a spec change.
pub const ARCH_COUNT: usize = 5;

impl Arch {
    /// Every architecture in declaration order.
    pub const ALL: [Arch; ARCH_COUNT] = [
        Self::X86_64,
        Self::Aarch64,
        Self::Riscv64,
        Self::Wasm32,
        Self::Wasm64,
    ];

    /// Spec spelling used inside a target triple (build-system.md §9).
    pub const fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86-64",
            Self::Aarch64 => "aarch64",
            Self::Riscv64 => "riscv64",
            Self::Wasm32 => "wasm32",
            Self::Wasm64 => "wasm64",
        }
    }

    /// Parse an architecture name from its spec spelling.
    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|a| a.name() == s)
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Locked set of build-target operating systems (build-system.md §9).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
#[repr(u8)]
pub enum Os {
    /// Linux kernel-based OS.
    Linux,
    /// Microsoft Windows.
    Windows,
    /// Apple macOS.
    Macos,
    /// FreeBSD.
    Freebsd,
    /// Freestanding / embedded target with no OS layer.
    BareMetal,
    /// WebAssembly System Interface host.
    Wasi,
    /// WebAssembly in a browser sandbox (no ambient filesystem, network,
    /// or process model; DOM / Web-API surface reached via capabilities).
    Browser,
}

/// Number of locked operating systems.
pub const OS_COUNT: usize = 7;

impl Os {
    /// Every OS in declaration order.
    pub const ALL: [Os; OS_COUNT] = [
        Self::Linux,
        Self::Windows,
        Self::Macos,
        Self::Freebsd,
        Self::BareMetal,
        Self::Wasi,
        Self::Browser,
    ];

    /// Spec spelling used inside a target triple.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Freebsd => "freebsd",
            Self::BareMetal => "bare-metal",
            Self::Wasi => "wasi",
            Self::Browser => "browser",
        }
    }

    /// Parse an OS name from its spec spelling.
    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|o| o.name() == s)
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Locked set of ABI variants (build-system.md §9).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
#[repr(u8)]
pub enum AbiVariant {
    /// GNU/Linux glibc ABI.
    Gnu,
    /// musl libc ABI.
    Musl,
    /// Microsoft Visual C++ ABI.
    Msvc,
    /// Apple Darwin ABI.
    Darwin,
    /// No ABI — bare-metal target.
    None,
    /// WASI preview 1 ABI.
    WasiPreview1,
}

/// Number of locked ABI variants.
pub const ABI_COUNT: usize = 6;

impl AbiVariant {
    /// Every ABI variant in declaration order.
    pub const ALL: [AbiVariant; ABI_COUNT] = [
        Self::Gnu,
        Self::Musl,
        Self::Msvc,
        Self::Darwin,
        Self::None,
        Self::WasiPreview1,
    ];

    /// Spec spelling used inside a target triple.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Gnu => "gnu",
            Self::Musl => "musl",
            Self::Msvc => "msvc",
            Self::Darwin => "darwin",
            Self::None => "none",
            Self::WasiPreview1 => "wasi-preview1",
        }
    }

    /// Parse an ABI-variant name from its spec spelling.
    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|a| a.name() == s)
    }
}

impl fmt::Display for AbiVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Build-target identifier: `(arch, os, abi-variant)`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct TargetTriple {
    arch: Arch,
    os: Os,
    abi: AbiVariant,
}

impl TargetTriple {
    /// Construct a triple from its three components.
    pub const fn new(arch: Arch, os: Os, abi: AbiVariant) -> Self {
        Self { arch, os, abi }
    }

    /// Architecture component.
    pub const fn arch(self) -> Arch {
        self.arch
    }

    /// Operating-system component.
    pub const fn os(self) -> Os {
        self.os
    }

    /// ABI-variant component.
    pub const fn abi(self) -> AbiVariant {
        self.abi
    }

    /// Ground truth for "is `cap_name` supported on this target?" — the
    /// predicate `edda_target::TargetTriple::supports_capability` from the
    /// codex's Model 1 cap-availability lock.
    ///
    /// Rules:
    ///
    /// - `Subprocess` / `Debugger` — hosted OS only
    ///   (`Linux`/`Windows`/`Macos`/`Freebsd`). WASI preview 1 has no
    ///   process-spawn / ptrace syscall, bare-metal has no process model,
    ///   browser sandboxes have neither.
    /// - `Filesystem` / `Network` family — every target except `BareMetal`
    ///   and `Browser`. The browser sandbox has no ambient filesystem or
    ///   socket access (Model 1); browser programs reach storage / fetch
    ///   through Web-API capabilities, not these.
    /// - `Executor` — every target (single-threaded ticks suffice).
    /// - Every other locked capability (clocks, I/O streams, allocator,
    ///   random) — every target except `BareMetal`. WASI preview 1 and
    ///   `Browser` admit them with reduced surface; bare-metal admits none
    ///   by construction (no syscalls, no allocator, no clock, no streams).
    ///
    /// Unknown `cap_name` values return `false` — the typechecker uses the
    /// caller-supplied locked-name strings from `CapabilityType::name()`,
    /// so a `false` for an unknown name is the safe default.
    pub fn supports_capability(self, cap_name: &str) -> bool {
        match cap_name {
            "Subprocess" | "Debugger" => matches!(
                self.os,
                Os::Linux | Os::Windows | Os::Macos | Os::Freebsd
            ),
            "Filesystem"
            | "ReadOnlyFilesystem"
            | "SandboxedFilesystem"
            | "Network"
            | "LocalhostNetwork"
            | "RestrictedNetwork" => !matches!(self.os, Os::BareMetal | Os::Browser),
            "Executor" => true,
            "Clock"
            | "MonotonicClock"
            | "Stdout"
            | "Stderr"
            | "Stdin"
            | "Allocator"
            | "Random"
            | "BoundedAllocator"
            | "DeterministicRandom" => !matches!(self.os, Os::BareMetal),
            _ => false,
        }
    }

    /// Parse a triple from its `<arch>-<os>-<abi-variant>` spec spelling.
    ///
    /// Arch and OS names may contain internal `-` (`x86-64`, `bare-metal`),
    /// so the parser anchors on known prefixes rather than splitting on `-`.
    pub fn parse(s: &str) -> Result<Self, TripleParseError> {
        if s.is_empty() {
            return Err(TripleParseError::Empty);
        }
        let (arch, after_arch) = strip_known_prefix(s, Arch::ALL.iter().copied(), Arch::name)
            .ok_or_else(|| TripleParseError::UnknownArch(s.to_owned()))?;
        let (os, after_os) = strip_known_prefix(after_arch, Os::ALL.iter().copied(), Os::name)
            .ok_or_else(|| TripleParseError::UnknownOs(after_arch.to_owned()))?;
        let abi = AbiVariant::from_name(after_os)
            .ok_or_else(|| TripleParseError::UnknownAbi(after_os.to_owned()))?;
        Ok(Self::new(arch, os, abi))
    }
}

impl fmt::Display for TargetTriple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}-{}", self.arch, self.os, self.abi)
    }
}

impl FromStr for TargetTriple {
    type Err = TripleParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Match the input against a set of known component names; on success
/// return the matched component and the remainder after the trailing `-`
/// that separates the component from the next one. The remainder is empty
/// only when the matched component is the entire input (no separator).
fn strip_known_prefix<'a, T, I, F>(input: &'a str, candidates: I, name_of: F) -> Option<(T, &'a str)>
where
    T: Copy,
    I: IntoIterator<Item = T>,
    F: Fn(T) -> &'static str,
{
    let mut best: Option<(T, &'a str)> = None;
    for candidate in candidates {
        let n = name_of(candidate);
        if let Some(rest) = input.strip_prefix(n) {
            // Require the next byte to be the component separator `-` so
            // partial matches like `wasm32-…` against arch `wasm3` cannot
            // succeed (no such arch exists today, but the rule is
            // structural). The trailing-component case (no separator) is
            // not used by the triple parser — there is always an abi
            // variant after arch and os — but we tolerate it for direct
            // callers wanting longest-match against an unprefixed string.
            if rest.is_empty() {
                if best.map_or(true, |(_, r)| r.len() > rest.len()) {
                    best = Some((candidate, rest));
                }
            } else if let Some(after) = rest.strip_prefix('-') {
                if best.map_or(true, |(_, r)| r.len() > after.len()) {
                    best = Some((candidate, after));
                }
            }
        }
    }
    best
}

/// Reason a triple string failed to parse.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum TripleParseError {
    /// Input was empty.
    Empty,
    /// No known [`Arch`] is a prefix of the input.
    UnknownArch(String),
    /// Arch matched, but no known [`Os`] is a prefix of the remainder.
    UnknownOs(String),
    /// Arch and OS matched, but the remainder is not a known [`AbiVariant`].
    UnknownAbi(String),
}

impl fmt::Display for TripleParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("target triple is empty"),
            Self::UnknownArch(s) => write!(f, "unknown architecture in target triple {s:?}"),
            Self::UnknownOs(s) => write!(f, "unknown operating system after arch: {s:?}"),
            Self::UnknownAbi(s) => write!(f, "unknown ABI variant: {s:?}"),
        }
    }
}

impl std::error::Error for TripleParseError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_round_trip() {
        for arch in Arch::ALL {
            assert_eq!(Arch::from_name(arch.name()), Some(arch));
        }
        assert_eq!(Arch::ALL.len(), ARCH_COUNT);
    }

    #[test]
    fn os_round_trip() {
        for os in Os::ALL {
            assert_eq!(Os::from_name(os.name()), Some(os));
        }
        assert_eq!(Os::ALL.len(), OS_COUNT);
    }

    #[test]
    fn abi_round_trip() {
        for abi in AbiVariant::ALL {
            assert_eq!(AbiVariant::from_name(abi.name()), Some(abi));
        }
        assert_eq!(AbiVariant::ALL.len(), ABI_COUNT);
    }

    #[test]
    fn arch_spec_spellings() {
        assert_eq!(Arch::X86_64.name(), "x86-64");
        assert_eq!(Arch::Aarch64.name(), "aarch64");
        assert_eq!(Arch::Riscv64.name(), "riscv64");
        assert_eq!(Arch::Wasm32.name(), "wasm32");
        assert_eq!(Arch::Wasm64.name(), "wasm64");
    }

    #[test]
    fn os_spec_spellings() {
        assert_eq!(Os::Linux.name(), "linux");
        assert_eq!(Os::BareMetal.name(), "bare-metal");
        assert_eq!(Os::Wasi.name(), "wasi");
        assert_eq!(Os::Browser.name(), "browser");
    }

    #[test]
    fn abi_spec_spellings() {
        assert_eq!(AbiVariant::Gnu.name(), "gnu");
        assert_eq!(AbiVariant::Msvc.name(), "msvc");
        assert_eq!(AbiVariant::Darwin.name(), "darwin");
        assert_eq!(AbiVariant::None.name(), "none");
        assert_eq!(AbiVariant::WasiPreview1.name(), "wasi-preview1");
    }

    #[test]
    fn triple_round_trips_for_every_spec_example() {
        // build-system.md §9 examples
        let examples = [
            "x86-64-linux-gnu",
            "aarch64-macos-darwin",
            "wasm32-wasi-wasi-preview1",
            "riscv64-bare-metal-none",
        ];
        for ex in examples {
            let parsed = TargetTriple::parse(ex)
                .unwrap_or_else(|e| panic!("parse {ex:?}: {e}"));
            assert_eq!(parsed.to_string(), ex);
        }
    }

    #[test]
    fn triple_parses_all_arch_os_abi_internal_dashes() {
        // The three components that contain internal `-` are the parser's
        // disambiguation pressure: `x86-64` (arch), `bare-metal` (os),
        // `wasi-preview1` (abi). One example for each.
        let t = TargetTriple::parse("x86-64-windows-msvc").unwrap();
        assert_eq!(t.arch(), Arch::X86_64);
        assert_eq!(t.os(), Os::Windows);
        assert_eq!(t.abi(), AbiVariant::Msvc);

        let t = TargetTriple::parse("aarch64-bare-metal-none").unwrap();
        assert_eq!(t.arch(), Arch::Aarch64);
        assert_eq!(t.os(), Os::BareMetal);
        assert_eq!(t.abi(), AbiVariant::None);

        let t = TargetTriple::parse("wasm32-wasi-wasi-preview1").unwrap();
        assert_eq!(t.arch(), Arch::Wasm32);
        assert_eq!(t.os(), Os::Wasi);
        assert_eq!(t.abi(), AbiVariant::WasiPreview1);
    }

    #[test]
    fn triple_parses_browser_wasm_targets() {
        // The browser-OS wasm triples the
        // DOM-target epic must parse and round-trip.
        let t = TargetTriple::parse("wasm32-browser-none").unwrap();
        assert_eq!(t.arch(), Arch::Wasm32);
        assert_eq!(t.os(), Os::Browser);
        assert_eq!(t.abi(), AbiVariant::None);
        assert_eq!(t.to_string(), "wasm32-browser-none");

        let t = TargetTriple::parse("wasm64-browser-none").unwrap();
        assert_eq!(t.arch(), Arch::Wasm64);
        assert_eq!(t.os(), Os::Browser);
        assert_eq!(t.abi(), AbiVariant::None);
    }

    #[test]
    fn browser_sandbox_denies_fs_network_and_process_caps() {
        // Parity with native `browser_supports`:
        // the browser sandbox grants clocks / streams / allocator / random /
        // executor but no ambient filesystem, network, or process control.
        let t = TargetTriple::new(Arch::Wasm32, Os::Browser, AbiVariant::None);
        for denied in [
            "Filesystem",
            "ReadOnlyFilesystem",
            "SandboxedFilesystem",
            "Network",
            "LocalhostNetwork",
            "RestrictedNetwork",
            "Subprocess",
            "Debugger",
        ] {
            assert!(!t.supports_capability(denied), "browser must deny {denied}");
        }
        for granted in [
            "Clock",
            "MonotonicClock",
            "Stdout",
            "Stderr",
            "Stdin",
            "Allocator",
            "BoundedAllocator",
            "Random",
            "DeterministicRandom",
            "Executor",
        ] {
            assert!(t.supports_capability(granted), "browser must grant {granted}");
        }
    }

    #[test]
    fn triple_parse_rejects_empty() {
        assert_eq!(TargetTriple::parse(""), Err(TripleParseError::Empty));
    }

    #[test]
    fn triple_parse_rejects_unknown_arch() {
        let err = TargetTriple::parse("sparc-linux-gnu").unwrap_err();
        assert!(matches!(err, TripleParseError::UnknownArch(_)));
    }

    #[test]
    fn triple_parse_rejects_unknown_os() {
        let err = TargetTriple::parse("x86-64-plan9-gnu").unwrap_err();
        assert!(matches!(err, TripleParseError::UnknownOs(_)));
    }

    #[test]
    fn triple_parse_rejects_unknown_abi() {
        let err = TargetTriple::parse("x86-64-linux-glibc").unwrap_err();
        assert!(matches!(err, TripleParseError::UnknownAbi(_)));
    }

    #[test]
    fn triple_from_str_works() {
        let t: TargetTriple = "x86-64-linux-gnu".parse().unwrap();
        assert_eq!(t.arch(), Arch::X86_64);
        assert_eq!(t.os(), Os::Linux);
        assert_eq!(t.abi(), AbiVariant::Gnu);
    }

    #[test]
    fn display_matches_name() {
        assert_eq!(format!("{}", Arch::X86_64), "x86-64");
        assert_eq!(format!("{}", Os::BareMetal), "bare-metal");
        assert_eq!(format!("{}", AbiVariant::WasiPreview1), "wasi-preview1");
        assert_eq!(
            format!(
                "{}",
                TargetTriple::new(Arch::Aarch64, Os::Linux, AbiVariant::Gnu)
            ),
            "aarch64-linux-gnu"
        );
    }
}
