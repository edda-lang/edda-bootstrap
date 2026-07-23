//! Shared MIR-body fixtures for the walker test suites.
//!
//! Lifts the per-construct `build_*_body` constructors and the
//! `lower_and_ir` adapter out of the individual test modules so the
//! body tests in [`super::body`], rvalue tests in [`super::rvalue`],
//! and place test in [`super::place`] can share them without
//! duplication.

#![cfg(test)]

use edda_intern::Interner;
use edda_mir::{
    BinOp, Body, BodyBuilder, ConstValue, MirPrim, MirProgram, MirType, Operand, ParamMode, Place,
    ProgramBuilder, Rvalue, RvalueKind, UnOp,
};
use edda_span::Span;
use edda_target::{AbiVariant, Arch, Os, TargetCfg, TargetTriple};

use crate::Emitter;

pub(crate) fn linux_x86_64() -> TargetCfg {
    TargetCfg::new(TargetTriple::new(Arch::X86_64, Os::Linux, AbiVariant::Gnu))
}

/// Build a single-block body whose entry returns the given constant.
pub(crate) fn build_const_return_body(
    interner: &Interner,
    name: &str,
    return_ty: MirType,
    value: ConstValue,
) -> (Body, MirProgram) {
    let mut prog_builder = ProgramBuilder::new();
    let const_id = prog_builder.push_const(edda_mir::Const {
        ty: return_ty.clone(),
        value,
    });

    let name_sym = interner.intern(name);
    let mut bb = BodyBuilder::new(name_sym, Span::DUMMY, return_ty.clone());
    let _ = bb.return_slot(return_ty, Span::DUMMY);
    let block = bb.block();
    let block_id = block.id();
    block.return_(Span::DUMMY, Operand::Const(const_id));
    bb.set_entry(block_id);
    let body = bb.finish();
    (body, prog_builder.finish())
}

/// Build a body with `n` `unreachable`-terminated blocks; entry is
/// the first.
pub(crate) fn build_n_unreachable_blocks(
    interner: &Interner,
    name: &str,
    return_ty: MirType,
    n: usize,
) -> (Body, MirProgram) {
    let name_sym = interner.intern(name);
    let mut bb = BodyBuilder::new(name_sym, Span::DUMMY, return_ty.clone());
    let _ = bb.return_slot(return_ty, Span::DUMMY);
    let mut entry: Option<edda_mir::BlockId> = None;
    for _ in 0..n {
        let block = bb.block();
        let id = block.id();
        if entry.is_none() {
            entry = Some(id);
        }
        block.unreachable(Span::DUMMY);
    }
    bb.set_entry(entry.expect("at least one block"));
    let body = bb.finish();
    (body, ProgramBuilder::new().finish())
}

/// Identity function over `i32`: `fn id(x: i32) -> i32 { return x }`.
pub(crate) fn id_i32_body(interner: &Interner) -> (Body, MirProgram) {
    let name = interner.intern("id");
    let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
    let _ = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
    let arg_local = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
    let block = bb.block();
    let block_id = block.id();
    block.return_(Span::DUMMY, Operand::Copy(Place::local(arg_local)));
    bb.set_entry(block_id);
    let body = bb.finish();
    (body, ProgramBuilder::new().finish())
}

/// Build a body that computes `ret = a <op> b` on two primitive-typed
/// parameters and returns it.
pub(crate) fn build_binop_body(
    interner: &Interner,
    name: &str,
    op: BinOp,
    prim: MirPrim,
) -> (Body, MirProgram) {
    let ty = MirType::prim(prim);
    let name_sym = interner.intern(name);
    let mut bb = BodyBuilder::new(name_sym, Span::DUMMY, ty.clone());
    let ret_local = bb.return_slot(ty.clone(), Span::DUMMY);
    let a = bb.param(ParamMode::Let, ty.clone(), Span::DUMMY);
    let b = bb.param(ParamMode::Let, ty.clone(), Span::DUMMY);
    let mut block = bb.block();
    let block_id = block.id();
    block.assign(
        Span::DUMMY,
        Place::local(ret_local),
        Rvalue {
            span: Span::DUMMY,
            kind: RvalueKind::BinOp {
                op,
                lhs: Operand::Copy(Place::local(a)),
                rhs: Operand::Copy(Place::local(b)),
                prim,
            },
            ty: ty.clone(),
        },
    );
    block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));
    bb.set_entry(block_id);
    let body = bb.finish();
    (body, ProgramBuilder::new().finish())
}

/// Build a body that computes `ret = <op> a` on one primitive-typed
/// parameter and returns it.
pub(crate) fn build_unop_body(
    interner: &Interner,
    name: &str,
    op: UnOp,
    prim: MirPrim,
) -> (Body, MirProgram) {
    let ty = MirType::prim(prim);
    let name_sym = interner.intern(name);
    let mut bb = BodyBuilder::new(name_sym, Span::DUMMY, ty.clone());
    let ret_local = bb.return_slot(ty.clone(), Span::DUMMY);
    let a = bb.param(ParamMode::Let, ty.clone(), Span::DUMMY);
    let mut block = bb.block();
    let block_id = block.id();
    block.assign(
        Span::DUMMY,
        Place::local(ret_local),
        Rvalue {
            span: Span::DUMMY,
            kind: RvalueKind::UnOp {
                op,
                arg: Operand::Copy(Place::local(a)),
                prim,
            },
            ty: ty.clone(),
        },
    );
    block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));
    bb.set_entry(block_id);
    let body = bb.finish();
    (body, ProgramBuilder::new().finish())
}

/// Lower a body through `Emitter::lower_body` and return the
/// stringified IR. Convenience for assertion-on-substring tests.
pub(crate) fn lower_and_ir(
    emitter: &Emitter,
    target: &TargetCfg,
    interner: &Interner,
    body: &Body,
    program: &MirProgram,
) -> String {
    let module = emitter.tagged_module("m", target).unwrap();
    let function = emitter
        .declare_function(&module, body, program, interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, body, program, interner, target.triple().arch())
        .unwrap();
    module.print_to_string().to_string()
}
