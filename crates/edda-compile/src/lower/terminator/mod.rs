//! MIR [`Terminator`] -> inkwell branch / return / unreachable.
//!
//! Covers the scalar terminator set (`Return`, `Goto`,
//! `SwitchBool`, `Unreachable`, `Panic`), `SwitchTag`,
//! the N-way branch on a sum's discriminant — extract the tag from
//! `{ tag, payload }` and emit `Builder::build_switch` with case
//! constants taken from `adt.variants[..].discriminant` — the `Call`
//! walker (callee resolution, capability threading,
//! argument lowering, on-error split), and `Raise`.
//! `Panic` lowers to a bare `unreachable` for now; upcoming
//! string-pool work wires a real panic-handler runtime call.
//!
//! # Submodule layout
//!
//! - [`call`] — the `Call` walker (callee resolution, capability
//!   threading, extern declaration, success / on-error split).
//! - [`raise`] — the `Raise` walker (packages an error payload into
//!   the body's return-Result sum).
//! - [`spawn`] — the `Spawn` / `Await` walkers for `scope(exec)` /
//!   `group.spawn { ... }` / `.await`:
//!   packs spawn args into a byte buffer, synthesises a
//!   `TaskBodyFn`-shaped shim per spawned body, and calls into the
//!   `edda-rt` `__edda_task_spawn` / `__edda_task_await` extern surface.
//!
//! The simple terminator arms stay inline in the dispatcher below.

pub(super) mod call;
pub(super) mod raise;
mod simple;
mod spawn;
mod switch_tag;

use edda_mir::{Terminator, TerminatorKind};

use crate::error::CompileError;

use super::LowerCtx;

/// Lower a terminator. Covers `SwitchTag` on top of the
/// scalar set (`Return`, `Goto`, `SwitchBool`, `Unreachable`,
/// `Panic`), the `Call` walker (scalar-result plus the
/// `?`-propagation success-vs-error split), and `Raise`.
pub(crate) fn lower_terminator<'ctx>(
    terminator: &Terminator,
    llvm_blocks: &[inkwell::basic_block::BasicBlock<'ctx>],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    match &terminator.kind {
        TerminatorKind::Return(operand) => simple::lower_return(operand, cx),
        TerminatorKind::Goto(target) => simple::lower_goto(*target, llvm_blocks, cx),
        TerminatorKind::SwitchBool {
            cond,
            true_bb,
            false_bb,
        } => simple::lower_switch_bool(cond, *true_bb, *false_bb, llvm_blocks, cx),
        TerminatorKind::Unreachable => simple::lower_unreachable(cx),
        TerminatorKind::Panic { msg } => simple::lower_panic(msg, cx),
        TerminatorKind::Call {
            func,
            args,
            capabilities,
            destination,
            target,
            on_error,
        } => call::lower_call(
            func,
            args,
            capabilities,
            destination,
            *target,
            *on_error,
            llvm_blocks,
            cx,
        ),
        TerminatorKind::Raise { err_adt, value } => raise::lower_raise(*err_adt, value, cx),
        TerminatorKind::SwitchTag {
            subject,
            adt,
            arms,
            otherwise,
        } => switch_tag::lower_switch_tag(subject, *adt, arms, *otherwise, llvm_blocks, cx),
        TerminatorKind::Spawn {
            child,
            args,
            group_local,
            dest,
            target,
        } => spawn::lower_spawn(*child, args, *group_local, *dest, *target, llvm_blocks, cx),
        TerminatorKind::Await { task, dest, target } => {
            spawn::lower_await(task, *dest, *target, llvm_blocks, cx)
        }
    }
}

#[cfg(test)]
mod switch_tag_tests {
    use crate::Emitter;
    use edda_intern::Interner;
    use edda_mir::{
        AdtDef, AdtKind, BodyBuilder, ConstValue, FieldDef, Idx, LayoutInfo, MirPrim, MirType,
        MirTypeKind, Operand, ParamMode, Place, ProgramBuilder, VariantDef, VariantIdx,
    };
    use edda_span::Span;

    use super::super::test_fixtures::linux_x86_64;

    /// Build a body `fn classify(o: Option) { switch o.tag { 0 => bb1, 1 => bb2, _ => bb_unreach } }`
    /// — drives the SwitchTag terminator lowering end-to-end.
    #[test]
    fn switch_tag_emits_llvm_switch_with_arm_constants() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();

        // Sum: Option<i32> with tag_width U8.
        let mut pb = ProgramBuilder::new();
        let opt_name = interner.intern("Option");
        let none_name = interner.intern("None");
        let some_name = interner.intern("Some");
        let payload_name = interner.intern("0");
        let adt_id = pb.push_adt(AdtDef {
            name: opt_name,
            span: Span::DUMMY,
            kind: AdtKind::Sum,
            variants: vec![
                VariantDef {
                    name: none_name,
                    span: Span::DUMMY,
                    fields: vec![],
                    discriminant: Some(0),
                },
                VariantDef {
                    name: some_name,
                    span: Span::DUMMY,
                    fields: vec![FieldDef {
                        name: payload_name,
                        span: Span::DUMMY,
                        ty: MirType::prim(MirPrim::I32),
                    }],
                    discriminant: Some(1),
                },
            ],
            layout: LayoutInfo::natural(),
            tag_width: Some(MirPrim::U8),
        });
        let option_ty = MirType::new(MirTypeKind::Adt(adt_id));
        let const_hundred = pb.push_const(edda_mir::Const {
            ty: MirType::prim(MirPrim::I32),
            value: ConstValue::Int(100),
        });
        let const_two_hundred = pb.push_const(edda_mir::Const {
            ty: MirType::prim(MirPrim::I32),
            value: ConstValue::Int(200),
        });

        let name = interner.intern("classify");
        let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
        let _ret_local = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
        let o = bb.param(ParamMode::Let, option_ty, Span::DUMMY);

        // `BlockBuilder` holds `&mut bb` exclusively, so seal each
        // arm before opening the next. Build the entry block last
        // and feed it the already-captured arm IDs.
        let none_block = bb.block();
        let none_id = none_block.id();
        none_block.return_(Span::DUMMY, Operand::Const(const_hundred));

        let some_block = bb.block();
        let some_id = some_block.id();
        some_block.return_(Span::DUMMY, Operand::Const(const_two_hundred));

        let unreach_block = bb.block();
        let unreach_id = unreach_block.id();
        unreach_block.unreachable(Span::DUMMY);

        let entry_block = bb.block();
        let entry_id = entry_block.id();
        entry_block.switch_tag(
            Span::DUMMY,
            Operand::Copy(Place::local(o)),
            adt_id,
            vec![
                (VariantIdx::new(0), none_id),
                (VariantIdx::new(1), some_id),
            ],
            unreach_id,
        );

        bb.set_entry(entry_id);
        let body = bb.finish();
        let program = pb.finish();

        let function = emitter
            .declare_function(&module, &body, &program, &interner, target.triple().arch())
            .unwrap();
        emitter
            .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
            .expect("SwitchTag must lower");
        let ir = module.print_to_string().to_string();

        // The switch instruction over an i8 tag with both arm constants.
        assert!(
            ir.contains("switch i8"),
            "expected `switch i8` instruction: {ir}"
        );
        // LLVM prints case labels as `i8 0, label %bbN` / `i8 1, label %bbN`.
        assert!(ir.contains("i8 0,"), "expected case for tag=0: {ir}");
        assert!(ir.contains("i8 1,"), "expected case for tag=1: {ir}");
    }
}

#[cfg(test)]
mod call_integration_tests {
    use crate::{CompileError, Emitter};
    use edda_intern::Interner;
    use edda_mir::{
        BodyBuilder, CallArg, CallMode, FnSig, FuncRef, MirPrim, MirType, Operand, ParamMode,
        Place, ProgramBuilder, Terminator, TerminatorKind,
    };
    use edda_span::Span;

    use super::super::test_fixtures::linux_x86_64;

    /// Extern call: `FuncRef::Extern { name: "malloc", sig: ... }`
    /// — verifies the extern is declared on first use (no body) and
    /// the call instruction targets the declared symbol.
    #[test]
    fn call_to_extern_declares_and_invokes_symbol() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();

        let mut pb = ProgramBuilder::new();

        // Extern signature: `extern fn malloc(size: usize) -> usize`.
        // We use usize as a pointer stand-in (MirPrim currently has no
        // raw-pointer type; wire-level both are 8 bytes on x86_64).
        let malloc_sig = FnSig {
            params: vec![(ParamMode::Let, MirType::prim(MirPrim::Usize))],
            ret: MirType::prim(MirPrim::Usize),
            capabilities: Vec::new(),
            may_raise: Vec::new(),
            may_panic: false,
        };
        let malloc_name = interner.intern("malloc");

        // Caller: calls malloc(0) and returns its result.
        let caller_body = {
            let name = interner.intern("alloc_zero");
            let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::Usize));
            let ret_local = bb.return_slot(MirType::prim(MirPrim::Usize), Span::DUMMY);
            let size = bb.param(ParamMode::Let, MirType::prim(MirPrim::Usize), Span::DUMMY);

            let entry_placeholder = bb.block();
            let entry_id = entry_placeholder.id();
            entry_placeholder.unreachable(Span::DUMMY);

            let cont_block = bb.block();
            let cont_id = cont_block.id();
            cont_block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));

            bb.set_entry(entry_id);
            let mut body = bb.finish();
            body.blocks[entry_id].terminator = Terminator {
                span: Span::DUMMY,
                kind: TerminatorKind::Call {
                    func: FuncRef::Extern {
                        name: malloc_name,
                        sig: Box::new(malloc_sig.clone()),
                    },
                    args: vec![CallArg {
                        mode: CallMode::Read,
                        operand: Operand::Copy(Place::local(size)),
                    }],
                    capabilities: Vec::new(),
                    destination: Place::local(ret_local),
                    target: cont_id,
                    on_error: None,
                },
            };
            body
        };
        let _caller_id = pb.push_body(caller_body);
        let program = pb.finish();

        let module = emitter
            .lower_program("extern_mod", &target, &program, &interner)
            .expect("extern call must lower");
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("declare i64 @malloc(i64)"),
            "expected `declare i64 @malloc(i64)` (usize lowers to i64 on x86_64): {ir}",
        );
        assert!(
            ir.contains("call i64 @malloc(i64"),
            "expected `call i64 @malloc(i64 ...)` instruction: {ir}",
        );
    }

    /// `Call` with `on_error: Some(_)` on a non-sum-returning callee
    /// is rejected with a clear shape — the contract is that
    /// `?`-propagation requires a Result-shaped (sum) return.
    #[test]
    fn call_with_on_error_on_non_sum_return_is_rejected() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();

        let mut pb = ProgramBuilder::new();

        let callee_body = {
            let name = interner.intern("trivial");
            let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
            let _ = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
            let x = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
            let block = bb.block();
            let block_id = block.id();
            block.return_(Span::DUMMY, Operand::Copy(Place::local(x)));
            bb.set_entry(block_id);
            bb.finish()
        };
        let callee_id = pb.push_body(callee_body);

        let caller_body = {
            let name = interner.intern("propagating_caller");
            let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
            let ret_local = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
            let y = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);

            // bb0: entry placeholder.
            let entry_placeholder = bb.block();
            let entry_id = entry_placeholder.id();
            entry_placeholder.unreachable(Span::DUMMY);

            // bb1: success continuation.
            let cont_block = bb.block();
            let cont_id = cont_block.id();
            cont_block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));

            // bb2: error continuation — placeholder unreachable.
            let err_block = bb.block();
            let err_id = err_block.id();
            err_block.unreachable(Span::DUMMY);

            bb.set_entry(entry_id);
            let mut body = bb.finish();
            body.blocks[entry_id].terminator = Terminator {
                span: Span::DUMMY,
                kind: TerminatorKind::Call {
                    func: FuncRef::Body(callee_id),
                    args: vec![CallArg {
                        mode: CallMode::Read,
                        operand: Operand::Copy(Place::local(y)),
                    }],
                    capabilities: Vec::new(),
                    destination: Place::local(ret_local),
                    target: cont_id,
                    on_error: Some(err_id),
                },
            };
            body
        };
        let _caller_id = pb.push_body(caller_body);
        let program = pb.finish();

        let err = emitter
            .lower_program("err_prop_mod", &target, &program, &interner)
            .expect_err("Call with on_error on i32 callee must be rejected");
        assert!(
            matches!(
                err,
                CompileError::UnsupportedMirShape { shape, .. } if shape == "call-on-error-non-sum-return"
            ),
            "wrong shape: {err:?}",
        );
    }
}

#[cfg(test)]
mod spawn_integration_tests {
    use crate::Emitter;
    use edda_intern::Interner;
    use edda_mir::{
        BodyBuilder, MirPrim, MirType, Operand, ParamMode, Place, ProgramBuilder, Terminator,
        TerminatorKind,
    };
    use edda_span::Span;

    use super::super::test_fixtures::linux_x86_64;

    /// Builds a lifted spawn body `fn <spawn>(n: i32) -> i32 { return n }`
    /// (mirroring `crates/edda-mir/src/lower/spawn/body.rs`'s real
    /// shape — same `"<spawn>"` name, ordinary by-value param) plus a
    /// parent body whose entry hand-carries a `Spawn` terminator into
    /// it, an `Await` on the returned handle, then returns the awaited
    /// result — driving both new terminator arms end-to-end through
    /// `Emitter::lower_program`.
    #[test]
    fn spawn_then_await_emits_shim_and_task_externs() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();

        let mut pb = ProgramBuilder::new();

        let spawn_body_name = interner.intern("<spawn>");
        let spawn_body = {
            let mut bb = BodyBuilder::new(spawn_body_name, Span::DUMMY, MirType::prim(MirPrim::I32));
            let _ret = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
            let n = bb.param(ParamMode::Take, MirType::prim(MirPrim::I32), Span::DUMMY);
            let block = bb.block();
            let block_id = block.id();
            block.return_(Span::DUMMY, Operand::Copy(Place::local(n)));
            bb.set_entry(block_id);
            bb.finish()
        };
        let child_id = pb.push_body(spawn_body);

        let parent_name = interner.intern("spawn_and_await");
        let parent_body = {
            let mut bb = BodyBuilder::new(parent_name, Span::DUMMY, MirType::prim(MirPrim::I32));
            let _ret = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
            let group = bb.param(ParamMode::Take, MirType::prim(MirPrim::HeapPtr), Span::DUMMY);
            let n = bb.param(ParamMode::Take, MirType::prim(MirPrim::I32), Span::DUMMY);
            let handle = bb.temp(MirType::prim(MirPrim::HeapPtr), Span::DUMMY);
            let result = bb.temp(MirType::prim(MirPrim::I32), Span::DUMMY);

            let entry_placeholder = bb.block();
            let entry_id = entry_placeholder.id();
            entry_placeholder.unreachable(Span::DUMMY);

            let await_block = bb.block();
            let await_id = await_block.id();
            await_block.unreachable(Span::DUMMY);

            let ret_block = bb.block();
            let ret_id = ret_block.id();
            ret_block.return_(Span::DUMMY, Operand::Copy(Place::local(result)));

            bb.set_entry(entry_id);
            let mut body = bb.finish();
            body.blocks[entry_id].terminator = Terminator {
                span: Span::DUMMY,
                kind: TerminatorKind::Spawn {
                    child: child_id,
                    args: vec![Operand::Copy(Place::local(n))],
                    group_local: group,
                    dest: handle,
                    target: await_id,
                },
            };
            body.blocks[await_id].terminator = Terminator {
                span: Span::DUMMY,
                kind: TerminatorKind::Await {
                    task: Operand::Copy(Place::local(handle)),
                    dest: result,
                    target: ret_id,
                },
            };
            body
        };
        let _parent_id = pb.push_body(parent_body);
        let program = pb.finish();

        let module = emitter
            .lower_program("spawn_mod", &target, &program, &interner)
            .expect("Spawn/Await must lower");
        let ir = module.print_to_string().to_string();

        assert!(
            ir.contains("call ptr @__edda_task_spawn("),
            "expected a call into __edda_task_spawn: {ir}"
        );
        assert!(
            ir.contains("call ptr @__edda_task_await("),
            "expected a call into __edda_task_await: {ir}"
        );
        assert!(
            ir.contains("__task_shim"),
            "expected a synthesised shim function: {ir}"
        );
        assert!(
            ir.contains("call i32 @"),
            "expected the shim to call the lifted spawn body: {ir}"
        );
        assert!(
            ir.contains("call ptr @malloc("),
            "expected the shim to heap-allocate its boxed result via malloc: {ir}"
        );
    }
}
