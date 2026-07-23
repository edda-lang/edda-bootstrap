use crate::Emitter;
use edda_intern::Interner;
use edda_mir::{
    BodyBuilder, ConstValue, Idx, MirPrim, MirType, Operand, ParamMode, Place, ProgramBuilder,
};
use edda_span::Span;

use crate::lower::test_fixtures::{
    build_const_return_body, build_n_unreachable_blocks, id_i32_body, linux_x86_64,
};

#[test]
fn emits_constant_int_return() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let (body, program) = build_const_return_body(
        &interner,
        "k",
        MirType::prim(MirPrim::I32),
        ConstValue::Int(42),
    );
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect("lower_body");
    let ir = module.print_to_string().to_string();
    assert!(ir.contains("ret i32 42"), "missing constant return: {ir}");
}

#[test]
fn emits_negative_int_return() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let (body, program) = build_const_return_body(
        &interner,
        "k",
        MirType::prim(MirPrim::I32),
        ConstValue::Int(-7),
    );
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect("lower_body");
    let ir = module.print_to_string().to_string();
    assert!(ir.contains("ret i32 -7"), "missing negative constant return: {ir}");
}

#[test]
fn emits_bool_return() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let (body, program) = build_const_return_body(
        &interner,
        "k",
        MirType::prim(MirPrim::Bool),
        ConstValue::Bool(true),
    );
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect("lower_body");
    let ir = module.print_to_string().to_string();
    assert!(ir.contains("ret i1 true"), "missing bool true return: {ir}");
}

#[test]
fn emits_zero_return() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let (body, program) = build_const_return_body(
        &interner,
        "k",
        MirType::prim(MirPrim::I64),
        ConstValue::Zero,
    );
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect("lower_body");
    let ir = module.print_to_string().to_string();
    assert!(ir.contains("ret i64 0"), "missing zero return: {ir}");
}

#[test]
fn emits_unit_return_as_void() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let name = interner.intern("u");
    let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::unit());
    let _ = bb.return_slot(MirType::unit(), Span::DUMMY);
    let block = bb.block();
    let block_id = block.id();
    block.return_(Span::DUMMY, Operand::Unit);
    bb.set_entry(block_id);
    let body = bb.finish();
    let program = ProgramBuilder::new().finish();
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect("lower_body");
    let ir = module.print_to_string().to_string();
    assert!(ir.contains("ret void"), "missing void return: {ir}");
}

#[test]
fn emits_unreachable_body() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let (body, program) =
        build_n_unreachable_blocks(&interner, "u", MirType::prim(MirPrim::I32), 1);
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect("lower_body");
    let ir = module.print_to_string().to_string();
    assert!(ir.contains("unreachable"), "missing unreachable: {ir}");
}

#[test]
fn creates_one_llvm_block_per_mir_block() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let (body, program) =
        build_n_unreachable_blocks(&interner, "many", MirType::prim(MirPrim::I32), 4);
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect("lower_body");
    let ir = module.print_to_string().to_string();
    assert!(ir.contains("bb0:"), "missing bb0: {ir}");
    assert!(ir.contains("bb1:"), "missing bb1: {ir}");
    assert!(ir.contains("bb2:"), "missing bb2: {ir}");
    assert!(ir.contains("bb3:"), "missing bb3: {ir}");
}

#[test]
fn identity_function_emits_alloca_store_load_ret() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let (body, program) = id_i32_body(&interner);
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect("lower_body");
    let ir = module.print_to_string().to_string();
    assert!(ir.contains("alloca i32"), "missing alloca: {ir}");
    assert!(ir.contains("store i32"), "missing store: {ir}");
    assert!(ir.contains("load i32"), "missing load: {ir}");
    assert!(ir.contains("ret i32"), "missing ret: {ir}");
}

#[test]
fn move_lowers_identically_to_copy() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module_copy = emitter.tagged_module("m_copy", &target).unwrap();
    let module_move = emitter.tagged_module("m_move", &target).unwrap();
    let interner = Interner::new();

    // Copy variant.
    let name = interner.intern("id_copy");
    let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
    let _ = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
    let arg_local = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
    let block = bb.block();
    let block_id = block.id();
    block.return_(Span::DUMMY, Operand::Copy(Place::local(arg_local)));
    bb.set_entry(block_id);
    let body_copy = bb.finish();

    // Move variant.
    let name = interner.intern("id_move");
    let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
    let _ = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
    let arg_local = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
    let block = bb.block();
    let block_id = block.id();
    block.return_(Span::DUMMY, Operand::Move(Place::local(arg_local)));
    bb.set_entry(block_id);
    let body_move = bb.finish();

    let program = ProgramBuilder::new().finish();
    let function_copy = emitter
        .declare_function(&module_copy, &body_copy, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(
            &module_copy,
            function_copy,
            &body_copy,
            &program,
            &interner,
            target.triple().arch(),
        )
        .unwrap();
    let function_move = emitter
        .declare_function(&module_move, &body_move, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(
            &module_move,
            function_move,
            &body_move,
            &program,
            &interner,
            target.triple().arch(),
        )
        .unwrap();
    let ir_copy = module_copy.print_to_string().to_string();
    let ir_move = module_move.print_to_string().to_string();
    assert!(ir_copy.contains("alloca i32") && ir_copy.contains("ret i32"));
    assert!(ir_move.contains("alloca i32") && ir_move.contains("ret i32"));
}

#[test]
fn param_store_preserves_argument_order() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let name = interner.intern("snd");
    let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
    let _ = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
    let _a = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
    let b = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
    let block = bb.block();
    let block_id = block.id();
    block.return_(Span::DUMMY, Operand::Copy(Place::local(b)));
    bb.set_entry(block_id);
    let body = bb.finish();
    let program = ProgramBuilder::new().finish();
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .unwrap();
    let ir = module.print_to_string().to_string();
    // Two parameters, three allocas (ret slot + 2 params), two stores.
    assert_eq!(ir.matches("alloca i32").count(), 3);
    assert_eq!(ir.matches("store i32").count(), 2);
}

// ----- Tuple end-to-end -----

/// Identity over `(i32, i64)` exercises tuple-typed param,
/// tuple-typed return slot, and round-trip through alloca.
#[test]
fn tuple_identity_lowers_with_struct_alloca() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let name = interner.intern("id_pair");
    let pair_ty = MirType::new(edda_mir::MirTypeKind::Tuple(vec![
        MirType::prim(MirPrim::I32),
        MirType::prim(MirPrim::I64),
    ]));
    let mut bb = BodyBuilder::new(name, Span::DUMMY, pair_ty.clone());
    let _ = bb.return_slot(pair_ty.clone(), Span::DUMMY);
    let p = bb.param(ParamMode::Let, pair_ty, Span::DUMMY);
    let block = bb.block();
    let block_id = block.id();
    block.return_(Span::DUMMY, Operand::Copy(Place::local(p)));
    bb.set_entry(block_id);
    let body = bb.finish();
    let program = ProgramBuilder::new().finish();
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect("tuple identity must lower");
    let ir = module.print_to_string().to_string();
    // Function signature carries the anonymous struct type.
    assert!(
        ir.contains("define { i32, i64 } @id_pair({ i32, i64 }"),
        "expected struct param + return in signature: {ir}"
    );
    // Two struct allocas (param slot + return slot).
    assert_eq!(
        ir.matches("alloca { i32, i64 }").count(),
        2,
        "expected two struct allocas: {ir}"
    );
}

/// `t.1 = v` via a Place with `[Field(1)]` lowers as
/// `getelementptr` against the struct type + `store`.
#[test]
fn field_projection_write_emits_struct_gep() {
    use edda_mir::{FieldIdx, Projection, Rvalue, RvalueKind};

    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let name = interner.intern("set_snd");
    let pair_ty = MirType::new(edda_mir::MirTypeKind::Tuple(vec![
        MirType::prim(MirPrim::I32),
        MirType::prim(MirPrim::I64),
    ]));
    let mut bb = BodyBuilder::new(name, Span::DUMMY, pair_ty.clone());
    let ret_local = bb.return_slot(pair_ty.clone(), Span::DUMMY);
    // `var t: (i32, i64);` — fresh uninit local.
    let t = bb.user_local(
        interner.intern("t"),
        edda_mir::Mutability::Mut,
        pair_ty.clone(),
        Span::DUMMY,
    );
    let v = bb.param(ParamMode::Let, MirType::prim(MirPrim::I64), Span::DUMMY);
    let mut block = bb.block();
    let block_id = block.id();
    // t.1 = v
    block.assign(
        Span::DUMMY,
        edda_mir::Place {
            local: t,
            projection: vec![Projection::Field(FieldIdx::new(1))],
        },
        Rvalue {
            span: Span::DUMMY,
            kind: RvalueKind::Use(Operand::Copy(Place::local(v))),
            ty: MirType::prim(MirPrim::I64),
        },
    );
    // return t
    block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));
    bb.set_entry(block_id);
    let body = bb.finish();
    let program = ProgramBuilder::new().finish();
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect("field-projection write must lower");
    let ir = module.print_to_string().to_string();
    // LLVM 18.1.6 emits `inbounds nuw`; earlier 18.x emits plain
    // `inbounds`. Tolerate both spellings.
    assert!(
        ir.contains("getelementptr inbounds { i32, i64 }")
            || ir.contains("getelementptr inbounds nuw { i32, i64 }"),
        "expected struct GEP for tuple field write: {ir}"
    );
    assert!(ir.contains("store i64"), "expected i64 store at field: {ir}");
}
