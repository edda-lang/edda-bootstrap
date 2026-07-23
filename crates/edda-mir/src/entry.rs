//! Entry-point capability materialisation.
//!
//! The bootstrap binary entry `function main() -> i32` lowers to LLVM
//! `@main`, which the platform CRT calls with **no** capability arguments
//! (`mainCRTStartup` / `_start` invoke `main` with at most `argc`/`argv`).
//! But an effect row such as `with allocator` makes `lower_effect_row`
//! thread one leading `ptr` capability parameter per
//! [`crate::effect::CapabilitySlot`] — so a `main` that allocates would
//! declare `@main(ptr)` and the CRT's argument-less call would not supply
//! the allocator handle. The entry's capability handles must instead be
//! *materialised inside the body* rather than received as parameters.
//!
//! [`materialize_entry_capabilities`] performs that rewrite on the picked
//! entry body only: it strips the leading capability [`crate::ParamInfo`]s
//! (so the LLVM signature is capability-free), demotes their backing
//! locals to compiler temporaries, and prepends one prologue assignment
//! per slot that writes a seed value into the slot's local. For most
//! capability kinds the runtime's externs ignore the capability pointer's
//! value entirely — see `edda-rt`'s `_allocator: *const ()` parameter,
//! documented as "the runtime ignores its value but the slot must be
//! present" — so a null handle (or a well-known fd for `Stdout`/`Stderr`/
//! `Fs`) is a sound, never-dereferenced-for-identity source. The
//! `Allocator` slot is the one exception:
//! when this member's own compiled objects shadow `edda-rt.lib`'s alloc
//! externs with `std.mem.rt`'s self-hosted `alloc_raw`/`heap_fork` family
//! (signalled by `abi_rt_shadows` containing `__edda_heap_create`), that
//! family *does* validate the heap handle it receives, so a null seed
//! fails every allocation immediately. In that case the `Allocator` slot
//! is instead materialised by calling `std.mem.rt.heap_create()` in a
//! prepended block. The body's [`crate::EffectRow::capabilities`] list is
//! left intact, so call-site capability threading still resolves each slot
//! to its (now temp-backed) local and reloads the materialised handle.

use std::collections::BTreeSet;

use edda_intern::Interner;
use edda_span::Span;
use edda_types::CapabilityType;

use crate::block::BasicBlockData;
use crate::body::LocalSource;
use crate::constant::{Const, ConstValue};
use crate::effect::CapabilityKind;
use crate::ids::{BodyId, ConstId, LocalId};
use crate::operand::Operand;
use crate::place::Place;
use crate::program::MirProgram;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::statement::{Statement, StatementKind};
use crate::terminator::{FuncRef, Terminator, TerminatorKind};
use crate::ty::{FnSig, MirPrim, MirType, MirTypeKind};

/// Linux `AT_FDCWD` (`-100` as a pointer-width unsigned value): the dirfd
/// every path-relative `*at` syscall and the filesystem runtime use as the
/// working-directory base. Seeded into `main`'s `Filesystem` capability so a
/// pure-Edda runtime reads it out of the capability value.
const AT_FDCWD: u128 = 0xFFFF_FFFF_FFFF_FF9C;

/// Linker-visible symbol `std.mem.rt.heap_create` exports via
/// `@abi("__edda_heap_create")`. Presence in `Driver::abi_rt_shadows` means
/// this member's own compiled objects define it, shadowing `edda-rt.lib`'s
/// Rust definitions of the whole alloc-family (object-file symbols win over
/// archive members) — see the module doc.
const HEAP_CREATE_SYMBOL: &str = "__edda_heap_create";

/// How a capability slot's local is materialised in the entry prologue.
enum CapSeed {
    /// Null opaque pointer — sound for any capability whose runtime ignores
    /// the handle's value.
    Null,
    /// A well-known fd/dirfd, `inttoptr`'d into the slot.
    Fd(u128),
    /// Call `std.mem.rt.heap_create()` and use its result — required only
    /// when that self-hosted family shadows `edda-rt.lib`'s alloc externs.
    MintAllocator,
}

/// The backing seed for a capability slot of the given kind.
///
/// Standard streams carry their well-known fd (`0`/`1`/`2`); a `Filesystem`
/// slot carries `AT_FDCWD`. On Windows these are logical descriptors that the
/// Edda I/O / subprocess code resolves to Win32 `HANDLE`s via `GetStdHandle`
/// — the same convention `std.io.stdio` already uses. `Stdin` (`0`) needs no
/// explicit seed: fd `0` is bit-identical to the null handle. `Allocator`
/// gets `MintAllocator` iff `mint_allocator` is set (`abi_rt_shadows` proves
/// `std.mem.rt.heap_create` is in this member's own compiled closure);
/// otherwise it falls through to `Null`, matching the prior behavior
/// that stays sound as long as only `edda-rt.lib`'s Rust alloc family is on
/// the link line.
fn seed_for(kind: &CapabilityKind, mint_allocator: bool) -> CapSeed {
    match kind {
        CapabilityKind::Allocator if mint_allocator => CapSeed::MintAllocator,
        CapabilityKind::Fs => CapSeed::Fd(AT_FDCWD),
        CapabilityKind::Typed(CapabilityType::Stdout) => CapSeed::Fd(1),
        CapabilityKind::Typed(CapabilityType::Stderr) => CapSeed::Fd(2),
        _ => CapSeed::Null,
    }
}

/// Rewrite the binary entry body so its effect-row capabilities are
/// materialised in-body rather than received as leading parameters.
///
/// No-op when `entry` is out of range, the entry body opens no effect-row
/// capabilities, or the entry has no entry block. For a `main` whose row is
/// pure this leaves the body untouched. `abi_rt_shadows` is
/// `Driver::abi_rt_shadows` — the set of `@abi("__edda_*")` export symbols
/// this member's own compiled objects define — consulted only to decide
/// whether the `Allocator` slot needs a minted handle instead of a null one.
pub fn materialize_entry_capabilities(
    program: &mut MirProgram,
    entry: BodyId,
    interner: &Interner,
    abi_rt_shadows: &BTreeSet<String>,
) {
    if program.bodies.get(entry).is_none() {
        return;
    }
    let mint_allocator = abi_rt_shadows.contains(HEAP_CREATE_SYMBOL);
    // Collect each capability slot's local and the seed to materialise it
    // with (immutable borrow of the body) so the consts can be pushed onto
    // `program.consts` before the body is re-borrowed mutably below.
    let cap_seeds: Vec<(LocalId, CapSeed)> = {
        let body = program
            .bodies
            .get(entry)
            .expect("entry body presence checked above");
        body.effect_row
            .capabilities
            .iter()
            .map(|slot| (slot.param_local, seed_for(&slot.ty, mint_allocator)))
            .collect()
    };
    if cap_seeds.is_empty() {
        return;
    }

    // Each non-minted slot is seeded with a `HeapPtr`-typed constant: a null
    // handle for value-agnostic capabilities (executor, …) and an
    // `inttoptr(fd)` for fd-backed ones (`Stdout`/`Stderr`/`Filesystem`).
    // `HeapPtr` lowers to the same opaque `ptr` wire shape as a capability
    // handle. The null handle is shared; fd handles get one const each. The
    // minted `Allocator` slot (if any) is tracked separately — it gets no
    // const, only its local, since its value comes from a `Call` instead.
    let null_handle = program.consts.push(Const {
        ty: MirType::prim(MirPrim::HeapPtr),
        value: ConstValue::Zero,
    });
    let mut seed_consts: Vec<(LocalId, ConstId)> = Vec::with_capacity(cap_seeds.len());
    let mut mint_local: Option<LocalId> = None;
    for (local, seed) in &cap_seeds {
        match seed {
            CapSeed::Null => seed_consts.push((*local, null_handle)),
            CapSeed::Fd(fd) => {
                let const_id = program.consts.push(Const {
                    ty: MirType::prim(MirPrim::HeapPtr),
                    value: ConstValue::Uint(*fd),
                });
                seed_consts.push((*local, const_id));
            }
            CapSeed::MintAllocator => mint_local = Some(*local),
        }
    }
    let cap_locals: Vec<LocalId> = cap_seeds.iter().map(|&(local, _)| local).collect();

    let body = program
        .bodies
        .get_mut(entry)
        .expect("entry body presence checked above");

    strip_capability_params(body, &cap_locals);
    seed_capability_locals(body, &seed_consts);
    if let Some(allocator_local) = mint_local {
        prepend_allocator_mint(body, allocator_local, interner);
    }
}

/// Remove the capability `ParamInfo`s from `body.params`, demote their
/// backing locals to compiler temporaries, and renumber any remaining
/// (user) parameters so their `LocalSource::Param(i)` index matches their
/// new position.
fn strip_capability_params(body: &mut crate::body::Body, cap_locals: &[LocalId]) {
    // Demote each capability local: it is no longer an incoming parameter,
    // so its provenance becomes `Temp`. The prologue assignment seeded by
    // `seed_capability_locals` is its sole writer.
    for &local in cap_locals {
        if let Some(decl) = body.locals.get_mut(local) {
            decl.source = LocalSource::Temp;
        }
    }

    // Drop the capability params (the leading entries) and renumber the
    // survivors' backing locals so `Param(i)` stays in lockstep with the
    // param's position.
    body.params.retain(|param| !cap_locals.contains(&param.local));
    for (new_index, param) in body.params.iter().enumerate() {
        if let Some(decl) = body.locals.get_mut(param.local) {
            decl.source = LocalSource::Param(new_index as u32);
        }
    }
}

/// Prepend one `Assign { cap_local = Use(Const(seed)) }` per capability slot
/// to the front of the entry block's statement list, where each `seed` is the
/// slot's descriptor const (a null handle, or an `inttoptr(fd)` for fd-backed
/// capabilities).
fn seed_capability_locals(body: &mut crate::body::Body, seed_consts: &[(LocalId, ConstId)]) {
    let Some(entry_block) = body.blocks.get_mut(body.entry) else {
        return;
    };
    let mut prologue: Vec<Statement> = Vec::with_capacity(seed_consts.len());
    for &(local, const_id) in seed_consts {
        prologue.push(Statement {
            span: Span::DUMMY,
            kind: StatementKind::Assign {
                place: Place::local(local),
                rvalue: Rvalue {
                    span: Span::DUMMY,
                    kind: RvalueKind::Use(Operand::Const(const_id)),
                    ty: MirType::prim(MirPrim::HeapPtr),
                },
            },
        });
    }
    prologue.append(&mut entry_block.stmts);
    entry_block.stmts = prologue;
}

/// Prepend a new entry block that calls `std.mem.rt.heap_create()` and
/// writes the real heap handle into `allocator_local`, then falls through to
/// the body's previous entry block (which still carries the null/fd seed
/// prologue `seed_capability_locals` built for the other capability slots).
/// Replaces the null-handle seed for the `Allocator` slot specifically:
/// `std.mem.rt`'s self-hosted `alloc_raw`/`heap_fork` family validates the
/// heap handle it receives (unlike `edda-rt`'s Rust family, which ignores
/// it), so a null handle stops being sound once that family shadows the
/// Rust one at link time.
fn prepend_allocator_mint(body: &mut crate::body::Body, allocator_local: LocalId, interner: &Interner) {
    let old_entry = body.entry;
    if body.blocks.get(old_entry).is_none() {
        return;
    }
    let ret_ty = MirType::new(MirTypeKind::Capability(CapabilityKind::Allocator));
    let sig = Box::new(FnSig {
        params: Vec::new(),
        ret: ret_ty,
        capabilities: Vec::new(),
        may_raise: Vec::new(),
        may_panic: false,
    });
    let func = FuncRef::Extern {
        name: interner.intern(HEAP_CREATE_SYMBOL),
        sig,
    };
    let mint_bb = body.blocks.push(BasicBlockData {
        stmts: Vec::new(),
        terminator: Terminator {
            span: Span::DUMMY,
            kind: TerminatorKind::Call {
                func,
                args: Vec::new(),
                capabilities: Vec::new(),
                destination: Place::local(allocator_local),
                target: old_entry,
                on_error: None,
            },
        },
    });
    body.entry = mint_bb;
}
