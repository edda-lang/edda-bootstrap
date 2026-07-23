//! Unit tests for the [`super::Emitter`] surface: module tagging,
//! target-machine construction, signature declaration, and object
//! emission.

    use super::*;
    use crate::error::CompileError;
    use edda_intern::Interner;
    use edda_mir::{Body, MirPrim, MirProgram, MirType, ProgramBuilder};
    use edda_target::{AbiVariant, Arch, Os, TargetCfg, TargetTriple};

    fn linux_x86_64() -> TargetCfg {
        TargetCfg::new(TargetTriple::new(Arch::X86_64, Os::Linux, AbiVariant::Gnu))
    }

    /// Empty MirProgram for tests that don't reference any ADTs or
    /// consts. The `declare_function` path needs a program reference
    /// to resolve `MirTypeKind::Adt` in
    /// signatures; tests with primitive-only signatures pass this.
    fn empty_program() -> MirProgram {
        ProgramBuilder::new().finish()
    }

    fn windows_x86_64() -> TargetCfg {
        TargetCfg::new(TargetTriple::new(Arch::X86_64, Os::Windows, AbiVariant::Msvc))
    }

    fn macos_aarch64() -> TargetCfg {
        TargetCfg::new(TargetTriple::new(
            Arch::Aarch64,
            Os::Macos,
            AbiVariant::Darwin,
        ))
    }

    #[test]
    fn module_carries_module_id_and_triple() {
        let emitter = Emitter::new();
        let module = emitter.module("hello", &linux_x86_64());
        let ir = module.print_to_string().to_string();
        assert!(ir.contains("ModuleID = 'hello'"), "got: {ir}");
        assert!(
            ir.contains("target triple = \"x86_64-unknown-linux-gnu\""),
            "got: {ir}",
        );
    }

    #[test]
    fn windows_triple_uses_pc_vendor_and_msvc_env() {
        let emitter = Emitter::new();
        let module = emitter.module("win", &windows_x86_64());
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("target triple = \"x86_64-pc-windows-msvc\""),
            "got: {ir}",
        );
    }

    #[test]
    fn apple_target_uses_three_part_darwin_form() {
        let emitter = Emitter::new();
        let module = emitter.module("mac", &macos_aarch64());
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("target triple = \"aarch64-apple-darwin\""),
            "got: {ir}",
        );
    }

    #[test]
    fn multiple_modules_share_one_context() {
        let emitter = Emitter::new();
        let a = emitter.module("a", &linux_x86_64());
        let b = emitter.module("b", &windows_x86_64());
        assert_ne!(
            a.print_to_string().to_string(),
            b.print_to_string().to_string(),
        );
    }

    #[test]
    fn empty_module_has_no_global_definitions() {
        let emitter = Emitter::new();
        let module = emitter.module("empty", &linux_x86_64());
        assert!(module.get_first_function().is_none());
        assert!(module.get_first_global().is_none());
    }

    #[test]
    fn context_accessor_returns_same_context() {
        let emitter = Emitter::new();
        let module = emitter.module("ctx", &linux_x86_64());
        let i32_ty = emitter.context().i32_type();
        let fn_ty = i32_ty.fn_type(&[], false);
        let _func = module.add_function("noop", fn_ty, None);
        let ir = module.print_to_string().to_string();
        assert!(ir.contains("declare i32 @noop()"), "got: {ir}");
    }

    #[test]
    fn target_machine_builds_for_x86_64_linux() {
        let emitter = Emitter::new();
        let tm = emitter.target_machine(&linux_x86_64()).expect("target machine");
        let data = tm.get_target_data();
        let dl = data.get_data_layout();
        let dl_str = dl.as_str().to_string_lossy();
        // x86_64 LLVM data layouts always carry `e-m:` (little-endian, mangling
        // style) regardless of OS — that's a stable structural property.
        assert!(dl_str.starts_with("e-m:"), "got: {dl_str}");
    }

    #[test]
    fn tagged_module_has_triple_and_datalayout() {
        let emitter = Emitter::new();
        let module = emitter
            .tagged_module("tagged", &linux_x86_64())
            .expect("tagged module");
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("target triple = \"x86_64-unknown-linux-gnu\""),
            "missing triple line: {ir}",
        );
        assert!(
            ir.contains("target datalayout ="),
            "missing datalayout line: {ir}",
        );
    }

    fn add_i32_body(interner: &Interner) -> Body {
        use edda_mir::{BodyBuilder, ParamMode};
        use edda_span::Span;

        let name = interner.intern("add");
        let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
        let _ = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
        let _ = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
        let _ = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
        let block = bb.block();
        let block_id = block.id();
        block.unreachable(Span::DUMMY);
        bb.set_entry(block_id);
        bb.finish()
    }

    fn body_with_signature(
        interner: &Interner,
        name: &str,
        return_ty: MirType,
        params: &[MirType],
    ) -> Body {
        use edda_mir::{BodyBuilder, ParamMode};
        use edda_span::Span;

        let name_sym = interner.intern(name);
        let mut bb = BodyBuilder::new(name_sym, Span::DUMMY, return_ty.clone());
        let _ = bb.return_slot(return_ty, Span::DUMMY);
        for ty in params {
            let _ = bb.param(ParamMode::Let, ty.clone(), Span::DUMMY);
        }
        let block = bb.block();
        let block_id = block.id();
        block.unreachable(edda_span::Span::DUMMY);
        bb.set_entry(block_id);
        bb.finish()
    }

    #[test]
    fn declare_function_emits_primitive_signature() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let body = add_i32_body(&interner);
        let function = emitter
            .declare_function(&module, &body, &empty_program(), &interner, target.triple().arch())
            .expect("declare_function");
        assert_eq!(function.count_params(), 2);
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("declare i32 @add(i32, i32)"),
            "missing add signature in IR: {ir}",
        );
    }

    #[test]
    fn declare_function_lowers_unit_return_to_void() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let body = body_with_signature(
            &interner,
            "say",
            MirType::unit(),
            &[MirType::prim(MirPrim::I32)],
        );
        emitter
            .declare_function(&module, &body, &empty_program(), &interner, target.triple().arch())
            .expect("declare_function");
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("declare void @say(i32)"),
            "missing void signature: {ir}",
        );
    }

    #[test]
    fn declare_function_lowers_never_return_to_void() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let body = body_with_signature(&interner, "abort_now", MirType::never(), &[]);
        emitter
            .declare_function(&module, &body, &empty_program(), &interner, target.triple().arch())
            .expect("declare_function");
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("declare void @abort_now()"),
            "missing void no-arg signature: {ir}",
        );
    }

    #[test]
    fn declare_function_handles_float_and_pointer_sized_primitives() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let body = body_with_signature(
            &interner,
            "addf",
            MirType::prim(MirPrim::F64),
            &[MirType::prim(MirPrim::F64), MirType::prim(MirPrim::F64)],
        );
        emitter
            .declare_function(&module, &body, &empty_program(), &interner, target.triple().arch())
            .expect("declare_function");
        let body_usize = body_with_signature(
            &interner,
            "ptr_op",
            MirType::prim(MirPrim::Usize),
            &[MirType::prim(MirPrim::Usize)],
        );
        emitter
            .declare_function(&module, &body_usize, &empty_program(), &interner, target.triple().arch())
            .expect("declare_function");
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("declare double @addf(double, double)"),
            "missing float signature: {ir}",
        );
        // x86_64 pointer width is 64 → usize lowers to i64.
        assert!(
            ir.contains("declare i64 @ptr_op(i64)"),
            "missing pointer-sized signature: {ir}",
        );
    }

    #[test]
    fn declare_function_pointer_sized_follows_wasm32() {
        let emitter = Emitter::new();
        let target = TargetCfg::new(TargetTriple::new(
            Arch::Wasm32,
            Os::Wasi,
            AbiVariant::WasiPreview1,
        ));
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let body = body_with_signature(
            &interner,
            "p",
            MirType::prim(MirPrim::Usize),
            &[MirType::prim(MirPrim::Isize)],
        );
        emitter
            .declare_function(&module, &body, &empty_program(), &interner, target.triple().arch())
            .expect("declare_function");
        let ir = module.print_to_string().to_string();
        // wasm32 pointer width is 32 → usize/isize lower to i32.
        assert!(
            ir.contains("declare i32 @p(i32)"),
            "missing wasm32-sized signature: {ir}",
        );
    }

    #[test]
    fn declare_function_admits_str_return_as_fat_pointer() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let body = body_with_signature(&interner, "give_str", MirType::prim(MirPrim::Str), &[]);
        emitter
            .declare_function(&module, &body, &empty_program(), &interner, target.triple().arch())
            .expect("str return must lower as fat pointer { ptr, isize }");
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("{ ptr, i64 } @give_str()"),
            "expected fat-pointer return ABI: {ir}",
        );
    }

    #[test]
    fn declare_function_admits_str_param_as_fat_pointer() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let body = body_with_signature(
            &interner,
            "take_str",
            MirType::unit(),
            &[MirType::prim(MirPrim::Str)],
        );
        emitter
            .declare_function(&module, &body, &empty_program(), &interner, target.triple().arch())
            .expect("str param must lower as fat pointer { ptr, isize }");
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("void @take_str({ ptr, i64 })"),
            "expected fat-pointer param ABI: {ir}",
        );
    }

    /// Best-effort tempfile that removes itself on drop. Avoids a
    /// `tempfile` dev-dep just for object-emission smoke tests.
    struct TempPath(std::path::PathBuf);
    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    impl TempPath {
        fn new(suffix: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = std::env::temp_dir()
                .join(format!("edda-emit-{pid}-{seq}-{suffix}"));
            TempPath(path)
        }
        fn as_path(&self) -> &std::path::Path {
            &self.0
        }
    }

    #[test]
    fn write_object_produces_elf_for_linux_x86_64() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let target_machine = emitter.target_machine(&target).expect("target machine");
        let module = emitter.tagged_module("emit_obj", &target).expect("tagged module");
        let temp = TempPath::new("empty.o");
        emitter
            .write_object(&target_machine, &module, temp.as_path())
            .expect("write_object");
        let bytes = std::fs::read(temp.as_path()).expect("read");
        assert!(bytes.len() >= 4, "object too small: {}", bytes.len());
        // Cross-compiled output is always ELF for the linux-x86_64
        // triple — the host's native format does not matter.
        assert_eq!(&bytes[0..4], b"\x7fELF", "expected ELF magic, got: {:?}", &bytes[0..4]);
    }

    #[test]
    fn compile_program_to_object_emits_named_function() {
        use edda_mir::ProgramBuilder;
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let mut pb = ProgramBuilder::new();
        let _ = pb.push_body(add_i32_body(&interner));
        let program = pb.finish();
        let temp = TempPath::new("add.o");
        emitter
            .compile_program_to_object(
                "add_mod",
                &target,
                &program,
                &interner,
                temp.as_path(),
                false,
            )
            .expect("compile_program_to_object");
        let bytes = std::fs::read(temp.as_path()).expect("read");
        assert!(bytes.len() >= 4, "object too small: {}", bytes.len());
        assert_eq!(&bytes[0..4], b"\x7fELF");
        // The function name must appear verbatim in the symbol-table
        // bytes of the ELF — a structural check that the MIR body
        // actually reached object emission.
        let needle = b"add";
        assert!(
            bytes.windows(needle.len()).any(|w| w == needle),
            "symbol name {:?} not found in object bytes",
            std::str::from_utf8(needle).unwrap(),
        );
    }

    #[test]
    fn write_object_rejects_unwritable_destination() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let target_machine = emitter.target_machine(&target).expect("target machine");
        let module = emitter.tagged_module("reject", &target).expect("tagged module");
        // Directory that does not exist, so LLVM's open-for-write
        // fails before any bytes flow.
        let bogus = std::path::PathBuf::from("/this/path/does/not/exist/out.o");
        let err = emitter
            .write_object(&target_machine, &module, &bogus)
            .expect_err("write_object must reject a nonexistent dir");
        match err {
            CompileError::ObjectEmit { path, reason } => {
                assert_eq!(path, bogus);
                assert!(!reason.is_empty(), "ObjectEmit reason must not be empty");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
