//! Per-body orchestration: pre-create BBs, run the entry prologue,
//! walk every block.
//!
//! [`lower_body`] is the seam between the per-function caller
//! ([`crate::Emitter::lower_body`]) and the per-instruction walkers
//! in [`super::statement`] and [`super::terminator`]. It:
//!
//! 1. Validates `body.entry` indexes a real block.
//! 2. Pre-creates one inkwell `BasicBlock` per MIR block so the
//!    terminator walker can reference forward blocks without
//!    ordering constraints.
//! 3. Positions the builder at the entry block and runs
//!    [`super::local::allocate_locals`] to materialise the prologue.
//! 4. Walks every block in arena order, threading statements and
//!    the single terminator through the per-construct lowering.

use edda_intern::Interner;
use edda_mir::{Body, Idx, MirPrim, MirProgram, MirType, MirTypeKind};
use edda_target::{Arch, Os};
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::FunctionValue;

use crate::error::CompileError;

use super::LowerCtx;
use super::local::allocate_locals;
use super::statement::lower_statement;
use super::terminator::lower_terminator;
use super::ty::compute_type_size;

#[cfg(test)]
mod tests;

/// Win64 x64 ABI: aggregates that are not 1/2/4/8 bytes cross the
/// call boundary via hidden pointer (`byval` for params, `sret` for
/// returns). This predicate is the shared classifier used by both
/// extern and source-bodied function declarations and call sites.
pub(super) fn win64_indirect_aggregate(
    ty: &MirType,
    os: Os,
    program: &MirProgram,
    arch: Arch,
) -> bool {
    if os != Os::Windows {
        return false;
    }
    let is_aggregate = matches!(
        &ty.kind,
        MirTypeKind::Prim(MirPrim::Str)
            | MirTypeKind::Slice(_)
            | MirTypeKind::Tuple(_)
            | MirTypeKind::Adt(_)
            // Fat function value `{ code, env }` — a 16-byte aggregate,
            // so it crosses the Win64
            // boundary via hidden pointer like any other aggregate.
            | MirTypeKind::FnPtr(_)
    );
    if !is_aggregate {
        return false;
    }
    let size = compute_type_size(ty, program, arch);
    size != 0 && !matches!(size, 1 | 2 | 4 | 8)
}

/// Derive the target [`Os`] from a module's triple string. The triple
/// string is the same string `set_triple` placed on the module at
/// construction; parsing it here avoids plumbing an extra parameter
/// through every call site of [`lower_body`].
pub(super) fn derive_os(module: &Module<'_>) -> Os {
    let triple = module.get_triple();
    let triple_bytes = triple.as_str().to_bytes();
    let s = std::str::from_utf8(triple_bytes).unwrap_or("");
    if s.contains("-windows-") {
        Os::Windows
    } else if s.contains("-linux-") {
        Os::Linux
    } else if s.contains("-darwin") || s.contains("-macos") {
        Os::Macos
    } else if s.contains("-freebsd") {
        Os::Freebsd
    } else if s.contains("-wasi") {
        Os::Wasi
    } else if s.contains("-browser") {
        Os::Browser
    } else {
        Os::Linux
    }
}

/// Walk a MIR [`Body`] and emit its basic blocks into `function`.
///
/// Constructs a [`LowerCtx`] bundling the per-body builder, body,
/// program, locals, arch, and resolved body name; passes a reference
/// to it through every per-instruction walker. See [`LowerCtx`] for
/// the rationale.
pub(crate) fn lower_body<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    function: FunctionValue<'ctx>,
    body: &Body,
    program: &MirProgram,
    interner: &Interner,
    arch: Arch,
    symbol_names: &[String],
) -> Result<(), CompileError> {
    let builder = context.create_builder();
    let body_name = interner.resolve(body.name);

    if body.entry == edda_mir::BlockId::DUMMY || body.entry.index() >= body.blocks.len() {
        return Err(CompileError::UnsupportedMirShape {
            shape: "no-entry-block",
            detail: format!("body {body_name:?} has no valid entry block"),
        });
    }

    // Pre-create one LLVM block per MIR block. Indexing by
    // `block_id.index()` is sound because IndexVec hands out IDs in
    // push order, so the i-th block ID maps to the i-th slot here.
    let mut llvm_blocks: Vec<inkwell::basic_block::BasicBlock<'ctx>> =
        Vec::with_capacity(body.blocks.len());
    for i in 0..body.blocks.len() {
        let label = format!("bb{i}");
        llvm_blocks.push(context.append_basic_block(function, &label));
    }

    // LLVM requires a function's entry block to be the first in its block
    // list (and to have no predecessors). MIR blocks are appended in id
    // order, so when `body.entry` is not block 0 — e.g. a synthesised
    // forwarding shim whose return block was reserved before its entry
    // call block — move the entry block to
    // the front. The `llvm_blocks` index map is unchanged; only LLVM's
    // block ordering is adjusted, so the per-block walk below still
    // positions by `block_id.index()` correctly.
    if body.entry.index() != 0 {
        let entry_bb = llvm_blocks[body.entry.index()];
        let first_bb = llvm_blocks[0];
        entry_bb
            .move_before(first_bb)
            .expect("entry block has no predecessors; move_before to function front must succeed");
    }

    // ----- Entry-block prologue -----
    let entry_llvm_block = llvm_blocks[body.entry.index()];
    builder.position_at_end(entry_llvm_block);
    let os = derive_os(module);
    let locals = allocate_locals(context, &builder, body, program, function, arch, os);

    let cx = LowerCtx {
        context,
        builder: &builder,
        module,
        body,
        program,
        interner,
        locals: &locals,
        arch,
        os,
        body_name,
        symbol_names,
    };

    // ----- Block walk -----
    for (block_id, bb_data) in body.blocks.iter_enumerated() {
        let llvm_block = llvm_blocks[block_id.index()];
        cx.builder.position_at_end(llvm_block);

        for stmt in &bb_data.stmts {
            lower_statement(stmt, &cx)?;
        }

        lower_terminator(&bb_data.terminator, &llvm_blocks, &cx)?;
    }

    Ok(())
}
