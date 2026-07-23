//! `Call` terminator walker: callee resolution, capability
//! threading, argument lowering, and the success-vs-on-error split.
//!
//! Externs are declared lazily and idempotently on first sight via
//! `module.add_function`. Program-defined bodies are looked up by
//! name through `module.get_function` — the program declare pass in
//! [`super::super::program::lower_program`] guarantees the entry
//! exists before any instruction stream is lowered.
//!
//! # Submodule layout
//! - [`abi`] — Win64 aggregate/register classification, alloc-family
//!   raw-extern recognition, operand/return MIR-type recovery.
//! - [`args`] — capability-slot lookup + call-argument assembly.
//! - [`direct`] — the direct (`Body`/`Extern`) walker + callee resolution.
//! - [`indirect`] — the [`edda_mir::FuncRef::Indirect`] walker.
//! - [`extern_decl`] — lazy extern declaration.

mod abi;
mod args;
mod direct;
mod extern_decl;
mod indirect;

pub(super) use direct::lower_call;

#[cfg(test)]
mod tests {
    use crate::Emitter;
    use edda_intern::Interner;
    use edda_mir::{
        BodyBuilder, CallArg, CallMode, CapabilityKind, FuncRef, MirPrim, MirType, MirTypeKind,
        Operand, ParamMode, Place, ProgramBuilder, Terminator, TerminatorKind,
    };
    use edda_span::Span;

    use crate::lower::test_fixtures::linux_x86_64;

    /// Two-body program: `caller(y: i32) -> i32 { callee(y) }` with
    /// `callee(x: i32) -> i32 { x }`. Verifies the Call
    /// walker emits a real `call i32 @callee(i32 ...)` instruction
    /// and stores the result into the caller's return slot.
    #[test]
    fn call_to_program_defined_body_emits_call_instruction() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();

        let mut pb = ProgramBuilder::new();

        // Callee: identity over i32.
        let callee_body = {
            let name = interner.intern("callee");
            let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
            // A source-like body carries a qualified name → `linkonce_odr`
            // symbol = the qualified name.
            bb.set_qualified_name(name);
            let _ = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
            let x = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
            let block = bb.block();
            let block_id = block.id();
            block.return_(Span::DUMMY, Operand::Copy(Place::local(x)));
            bb.set_entry(block_id);
            bb.finish()
        };
        let callee_id = pb.push_body(callee_body);

        // Caller: calls callee with its own param, returns the result.
        let caller_body = {
            let name = interner.intern("caller");
            let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
            let ret_local = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
            let y = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);

            // bb0: entry placeholder — overwritten with the Call below.
            let entry_placeholder = bb.block();
            let entry_id = entry_placeholder.id();
            entry_placeholder.unreachable(Span::DUMMY);

            // bb1: success continuation — returns the call result.
            let cont_block = bb.block();
            let cont_id = cont_block.id();
            cont_block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));

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
                    on_error: None,
                },
            };
            body
        };
        let _caller_id = pb.push_body(caller_body);
        let program = pb.finish();

        let module = emitter
            .lower_program("test_mod", &target, &program, &interner)
            .expect("two-body lower must succeed");
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("call i32 @callee(i32"),
            "expected a `call i32 @callee(i32 ...)` instruction: {ir}",
        );
    }

    /// Capability threading: caller has one `Allocator` slot, calls
    /// callee which also takes one `Allocator`. Verifies the
    /// capability value is loaded from the caller's leading-param
    /// alloca and threaded as the leading LLVM argument before the
    /// user-mode args.
    #[test]
    fn call_threads_capability_as_leading_argument() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();

        let mut pb = ProgramBuilder::new();
        let cap_ty = MirType::new(MirTypeKind::Capability(CapabilityKind::Allocator));

        // Callee: takes one allocator capability + one i32 param.
        let callee_body = {
            let name = interner.intern("alloc_id");
            let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
            bb.set_qualified_name(name);
            let _ = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
            let _cap = bb.add_capability(CapabilityKind::Allocator, cap_ty.clone(), Span::DUMMY);
            let x = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
            let block = bb.block();
            let block_id = block.id();
            block.return_(Span::DUMMY, Operand::Copy(Place::local(x)));
            bb.set_entry(block_id);
            bb.finish()
        };
        let callee_id = pb.push_body(callee_body);

        // Caller: also has one allocator capability; threads it to callee.
        let caller_body = {
            let name = interner.intern("caller_with_cap");
            let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
            bb.set_qualified_name(name);
            let ret_local = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
            let caller_cap_id =
                bb.add_capability(CapabilityKind::Allocator, cap_ty.clone(), Span::DUMMY);
            let y = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);

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
                    func: FuncRef::Body(callee_id),
                    args: vec![CallArg {
                        mode: CallMode::Read,
                        operand: Operand::Copy(Place::local(y)),
                    }],
                    capabilities: vec![edda_mir::ThreadedCapability::slot(caller_cap_id)],
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
            .lower_program("cap_mod", &target, &program, &interner)
            .expect("capability-threading lower must succeed");
        let ir = module.print_to_string().to_string();

        // Callee's signature carries a leading `ptr` for the capability.
        // Source-like bodies emit under `linkonce_odr`.
        assert!(
            ir.contains("define linkonce_odr i32 @alloc_id(ptr"),
            "callee signature should lead with a `ptr` capability: {ir}",
        );
        // Caller's signature also leads with `ptr`.
        assert!(
            ir.contains("define linkonce_odr i32 @caller_with_cap(ptr"),
            "caller signature should lead with a `ptr` capability: {ir}",
        );
        // The call passes the loaded capability `ptr` before the user arg.
        assert!(
            ir.contains("call i32 @alloc_id(ptr"),
            "call should pass capability ptr as the first argument: {ir}",
        );
    }
}
