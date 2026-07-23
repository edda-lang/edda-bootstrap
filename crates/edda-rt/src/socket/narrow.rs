//! Network capability narrowing — `Network.bind_localhost` / `.restrict_to`
//! backs the `LocalhostNetwork` / `RestrictedNetwork` capability
//! narrowings declared in `std.net.socket`.
//!
//! Mirrors the fs `ScopedFs` pattern: the
//! narrowed capability VALUE is a heap-backed handle. Unlike `Filesystem`'s
//! `AT_FDCWD` sentinel, `Network`'s entry-prologue seed is a null opaque
//! pointer (`edda-mir`'s `seed_for` falls through to `CapSeed::Null` for
//! any capability kind without a dedicated seed), so an unscoped `Network`
//! capability is simply `*const () == null`.
//!
//! Simplification: the minted handles carry policy state only — no
//! `std.net.socket` consumer function accepts `LocalhostNetwork` /
//! `RestrictedNetwork` yet (`dial` / `listen` still take bare `Network`),
//! so nothing enforces the recorded policy at runtime today. Narrowing the
//! type is the load-bearing guarantee until a consumer surface lands.

use crate::{EdSlice, EdStr};

use super::ed_str_as_str;

struct LocalhostNet {
    #[allow(dead_code)]
    port: u16,
}

struct RestrictedNet {
    #[allow(dead_code)]
    allowlist: Vec<String>,
}

/// Decode an `EdSlice` of `EdStr` elements (`[String]`) into owned strings.
unsafe fn ed_slice_of_str(hosts: &EdSlice) -> Vec<String> {
    if hosts.len == 0 || hosts.ptr.is_null() {
        return Vec::new();
    }
    // SAFETY: caller asserts `ptr` heads `len` initialised `EdStr` (16-byte
    // fat-pointer) elements — the same `[T]` wire shape `EdSlice` docs
    // describe for any element type.
    let items =
        unsafe { std::slice::from_raw_parts(hosts.ptr as *const EdStr, hosts.len as usize) };
    items
        .iter()
        .filter_map(|s| unsafe { ed_str_as_str(s) })
        .map(|s| s.to_string())
        .collect()
}

/// `bind_localhost(net: Network, port: u16) -> LocalhostNetwork` — mint a
/// loopback-bound narrowing handle carrying `port`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_net_bind_localhost(_cap_net: *const (), port: u16) -> *const () {
    Box::into_raw(Box::new(LocalhostNet { port })) as *const ()
}

/// `restrict_to(net: Network, hosts: [String]) -> RestrictedNetwork` —
/// mint an allow-listed narrowing handle carrying `hosts`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_net_restrict_to(_cap_net: *const (), hosts: EdSlice) -> *const () {
    // SAFETY: caller asserts `hosts` heads a live `[String]` slice.
    let allowlist = unsafe { ed_slice_of_str(&hosts) };
    Box::into_raw(Box::new(RestrictedNet { allowlist })) as *const ()
}
