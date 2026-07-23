//! Slice / array rvalues (`MakeArray`, `ArrayIndex`, `SliceSubrange`,
//! `ArrayLen`, `StringBytes`).
//!
//! The slice family lowers on top of the tuple /
//! record / sum primitives. `MakeArray` allocates `[N x T]`
//! backing storage at the *current* builder position, fills each
//! slot, and assembles a fat-pointer `{ ptr, isize }` slice value
//! via `insertvalue`. `ArrayIndex` extracts the data pointer, GEPs
//! by the index, and loads. `SliceSubrange` borrows a `[lo, hi)`
//! half-open sub-slice via a forward-GEP of the source pointer plus
//! a `hi - lo` length recomputation (no copy). `ArrayLen` extracts
//! field 1. `StringBytes` is a zero-cost identity transform: a
//! `String`'s `{ ptr, isize }` value flows through unchanged as a
//! `[u8]` fat pointer.
//!
//! # Submodule layout
//! - [`make`] — `MakeArray`.
//! - [`index`] — `ArrayIndex` plus the shared [`array_element_ptr`]
//!   element-pointer resolver.
//! - [`slice_ops`] — `SliceSubrange`, `ArrayLen`, `StringBytes`.

mod index;
mod make;
mod slice_ops;

pub(in crate::lower) use index::array_element_ptr;
pub(super) use index::lower_array_index;
pub(super) use make::lower_make_array;
pub(super) use slice_ops::{lower_array_len, lower_string_bytes, lower_subrange};

#[cfg(test)]
mod tests {
    use crate::Emitter;
    use edda_intern::Interner;
    use edda_mir::{
        BodyBuilder, MirPrim, MirType, Operand, ParamMode, Place, ProgramBuilder, Rvalue,
        RvalueKind,
    };
    use edda_span::Span;

    use crate::lower::test_fixtures::linux_x86_64;

    /// `fn three() -> [i32] { [10, 20, 30] }` — MakeArray allocates
    /// backing storage and assembles a fat-pointer slice.
    ///
    /// Currently ignored: slice return types are rejected by
    /// `declare_function` as `non-lowerable-return` (primitive / tuple
    /// / product-ADT / sum-ADT / Unit / Never returns are handled;
    /// slice returns are not yet supported). Drop the `#[ignore]` once
    /// signature lowering admits slices.
    #[test]
    #[ignore = "slice returns not yet admitted by lower_fn_sig"]
    fn make_array_emits_alloca_stores_and_slice_insertvalue() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let slice_i32 = MirType::new(edda_mir::MirTypeKind::Slice(Box::new(MirType::prim(
            MirPrim::I32,
        ))));

        let mut pb = edda_mir::ProgramBuilder::new();
        let const10 = pb.push_const(edda_mir::Const {
            ty: MirType::prim(MirPrim::I32),
            value: edda_mir::ConstValue::Int(10),
        });
        let const20 = pb.push_const(edda_mir::Const {
            ty: MirType::prim(MirPrim::I32),
            value: edda_mir::ConstValue::Int(20),
        });
        let const30 = pb.push_const(edda_mir::Const {
            ty: MirType::prim(MirPrim::I32),
            value: edda_mir::ConstValue::Int(30),
        });

        let name = interner.intern("three");
        let mut bb = BodyBuilder::new(name, Span::DUMMY, slice_i32.clone());
        let ret_local = bb.return_slot(slice_i32.clone(), Span::DUMMY);
        let mut block = bb.block();
        let block_id = block.id();
        block.assign(
            Span::DUMMY,
            Place::local(ret_local),
            Rvalue {
                span: Span::DUMMY,
                kind: RvalueKind::MakeArray {
                    elems: vec![
                        Operand::Const(const10),
                        Operand::Const(const20),
                        Operand::Const(const30),
                    ],
                },
                ty: slice_i32,
            },
        );
        block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));
        bb.set_entry(block_id);
        let body = bb.finish();
        let program = pb.finish();
        let function = emitter
            .declare_function(&module, &body, &program, &interner, target.triple().arch())
            .unwrap();
        emitter
            .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
            .expect("MakeArray must lower");
        let ir = module.print_to_string().to_string();
        // [3 x i32] backing storage.
        assert!(
            ir.contains("alloca [3 x i32]"),
            "missing array alloca: {ir}"
        );
        // Three element stores.
        assert_eq!(
            ir.matches("store i32").count(),
            3,
            "expected three element stores: {ir}"
        );
        // Two insertvalues to assemble { ptr, isize }.
        assert!(
            ir.matches("insertvalue").count() >= 2,
            "expected at least two insertvalues (slice {{ ptr, isize }}): {ir}"
        );
    }

    /// `fn len_of(s: [i32]) -> isize { s.len() }` via ArrayLen.
    #[test]
    fn array_len_emits_extractvalue_field_1() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let slice_i32 = MirType::new(edda_mir::MirTypeKind::Slice(Box::new(MirType::prim(
            MirPrim::I32,
        ))));

        let name = interner.intern("len_of");
        let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::Isize));
        let ret_local = bb.return_slot(MirType::prim(MirPrim::Isize), Span::DUMMY);
        let s = bb.param(ParamMode::Let, slice_i32, Span::DUMMY);
        let mut block = bb.block();
        let block_id = block.id();
        block.assign(
            Span::DUMMY,
            Place::local(ret_local),
            Rvalue {
                span: Span::DUMMY,
                kind: RvalueKind::ArrayLen {
                    array: Operand::Copy(Place::local(s)),
                },
                ty: MirType::prim(MirPrim::Isize),
            },
        );
        block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));
        bb.set_entry(block_id);
        let body = bb.finish();
        let program = ProgramBuilder::new().finish();
        let function = emitter
            .declare_function(&module, &body, &program, &interner, target.triple().arch())
            .unwrap();
        emitter
            .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
            .expect("ArrayLen must lower");
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("extractvalue"),
            "missing extractvalue for ArrayLen: {ir}"
        );
    }

    /// `fn at(s: [i32], i: isize) -> i32 { s[i] }` via ArrayIndex —
    /// extracts the slice's data ptr, GEPs, loads.
    #[test]
    fn array_index_emits_gep_and_load() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let slice_i32 = MirType::new(edda_mir::MirTypeKind::Slice(Box::new(MirType::prim(
            MirPrim::I32,
        ))));

        let name = interner.intern("at");
        let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
        let ret_local = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
        let s = bb.param(ParamMode::Let, slice_i32, Span::DUMMY);
        let i = bb.param(ParamMode::Let, MirType::prim(MirPrim::Isize), Span::DUMMY);
        let mut block = bb.block();
        let block_id = block.id();
        block.assign(
            Span::DUMMY,
            Place::local(ret_local),
            Rvalue {
                span: Span::DUMMY,
                kind: RvalueKind::ArrayIndex {
                    array: Operand::Copy(Place::local(s)),
                    idx: Operand::Copy(Place::local(i)),
                },
                ty: MirType::prim(MirPrim::I32),
            },
        );
        block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));
        bb.set_entry(block_id);
        let body = bb.finish();
        let program = ProgramBuilder::new().finish();
        let function = emitter
            .declare_function(&module, &body, &program, &interner, target.triple().arch())
            .unwrap();
        emitter
            .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
            .expect("ArrayIndex must lower");
        let ir = module.print_to_string().to_string();
        // GEP on the slice's data pointer with an i32 element stride.
        assert!(
            ir.contains("getelementptr i32"),
            "missing GEP i32 for ArrayIndex: {ir}"
        );
        // Load the element value.
        assert!(
            ir.matches("load i32").count() >= 1,
            "missing load i32 for ArrayIndex element: {ir}"
        );
    }
}
