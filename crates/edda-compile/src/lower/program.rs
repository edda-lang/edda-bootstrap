//! Program-level orchestration: walk every [`Body`] in a [`MirProgram`]
//! into a single inkwell [`Module`].
//!
//! Walks the program in two passes:
//! 1. Declare every body's signature into the module via
//!    [`super::fn_sig::lower_fn_sig`], then apply the body's symbol
//!    linkage ([`apply_linkonce_comdat`] for `linkonce_odr` source
//!    functions). After this pass every callee a later body might
//!    reference is reachable through `module.get_function(name)`.
//! 2. Lower every body's instruction stream via
//!    [`super::body::lower_body`]. The `Call` walker relies
//!    on the first pass having registered every forward-callable body
//!    name, otherwise body N calling body M with M > N would miss.
//!
//! Product and sum ADTs (`AdtKind::Product` and
//! `AdtKind::Sum`) plus slices are admitted in the `adts` arena and the type
//! system — sum-typed bodies flow through `SwitchTag`, `MakeVariant`,
//! `ExtractTag`, the variant-bearing `ExtractField`, and the
//! `VariantDowncast` place projection; capability-bearing bodies
//! lower through Call; `Raise` lowers through the `?`-propagation
//! terminator walker. Every
//! body is gated through the shape
//! filters in [`super::body`] / [`super::statement`] /
//! [`super::rvalue`] / [`super::terminator`].
//!
//! Symbol model: every member re-lowers
//! its full source closure, so the same logical function is emitted into
//! several member objects. To make those objects safely combinable,
//! source functions are emitted with their deterministic
//! `<module.path>.<leaf>` symbol under `linkonce_odr` + COMDAT so the
//! linker folds identical copies to one; the binary entry (`main`) and
//! `@export` bodies keep their fixed external symbol; compiler-synthesised
//! bodies (fn-value shims, closures) with no source module path get a
//! member-unique external symbol so two *distinct* synthesised bodies
//! never collide or fold across members.

use std::collections::HashSet;

use edda_intern::Interner;
use edda_mir::{Body, BodyId, Idx, MirPrim, MirProgram, MirTypeKind};
use edda_target::{Arch, Os};
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::comdat::ComdatSelectionKind;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::AnyType;
use inkwell::values::FunctionValue;

use crate::error::CompileError;

use super::body::{derive_os, lower_body, win64_indirect_aggregate};
use super::fn_sig::lower_fn_sig;
use super::ty::inkwell_basic_type;

/// Internal symbol the user's fallible-`main` body is emitted under when a
/// CRT-callable `i32 @main()` wrapper is synthesised around it.
const FALLIBLE_ENTRY_SYMBOL: &str = "__edda_user_main";

/// Lower every [`Body`] in `program` into `module`. The caller owns
/// the module's lifetime. `module_name` is the per-member package name
/// used to make synthesised-body symbols member-unique.
///
/// When `freestanding` is set and this member produces a binary entry
/// (`main`), a tiny OS entry stub (`_edda_start`) is emitted alongside
/// `main` so the binary can be linked with no C runtime and no Rust
/// `edda-rt` on the link line — the runtime-in-Edda closeout.
/// The stub is per-target; today only
/// Windows (`_edda_start` → `main` → `ExitProcess`) is emitted.
pub(crate) fn lower_program<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    program: &MirProgram,
    interner: &Interner,
    arch: Arch,
    module_name: &str,
    freestanding: bool,
) -> Result<(), CompileError> {
    // A fallible `main` (one whose effect row carries an `err:` axis) has
    // its return projected to the wire-level `__Result<T, E...>` sum.
    // The CRT's `mainCRTStartup` cannot consume that shape: a >8-byte
    // sum arrives via a hidden `sret` pointer the CRT never passes
    // (stack corruption), and a small sum
    // (`__Result<i32, E>` is 8 bytes) returns directly in RAX, whose
    // low 32 bits land on the tag + padding — so the process exits 0
    // regardless of the returned value.
    // Either way the entry is emitted under an internal symbol and a
    // CRT-callable `i32 @main()` wrapper is synthesised below.
    //
    // A unit-returning entry needs the wrapper too: `Unit` lowers to LLVM
    // `void`, and a CRT-called `void @main()` leaves the exit code to
    // whatever is left in EAX — a garbage (observed non-zero) process
    // exit for a successful run. The
    // wrapper calls it and returns constant `0`.
    let os = derive_os(module);
    let wrapped_entry: Option<BodyId> = program.entry.filter(|&e| {
        program.bodies.get(e).is_some_and(|body| {
            entry_returns_result_sum(body, program)
                || matches!(body.return_ty.kind, MirTypeKind::Unit)
        })
    });

    let plan = compute_symbol_names(program, interner, module_name, wrapped_entry);
    // Pass 1: declare every body's signature so forward references
    // can resolve via `module.get_function(name)` during pass 2, and
    // pin each source function's `linkonce_odr` + COMDAT linkage.
    let mut functions: Vec<FunctionValue<'ctx>> = Vec::with_capacity(program.bodies.len());
    for (body_id, body) in program.bodies.iter_enumerated() {
        let idx = body_id.index();
        let symbol = plan.names[idx].as_str();
        let func = lower_fn_sig(context, module, body, program, interner, arch, symbol)?;
        if plan.comdat[idx] {
            apply_linkonce_comdat(module, func, symbol);
        }
        functions.push(func);
    }

    // Pass 2: emit each body's instruction stream into its declared
    // function. Iterating `program.bodies` in arena order pairs with
    // the `functions` vector by index.
    for ((_, body), function) in program.bodies.iter_enumerated().zip(functions.into_iter()) {
        lower_body(
            context,
            module,
            function,
            body,
            program,
            interner,
            arch,
            &plan.names,
        )?;
    }

    // Synthesise the CRT-callable `i32 @main()` wrapper around a fallible
    // or unit-returning entry (see the `wrapped_entry` comment above).
    if let Some(entry) = wrapped_entry {
        let entry_body = program
            .bodies
            .get(entry)
            .expect("wrapped_entry id was resolved from program.bodies");
        synthesize_entry_wrapper(context, module, entry_body, program, os, arch)?;
    }

    // Freestanding closeout: when the
    // member produces a binary, emit the per-target OS entry stub so the
    // link line can drop the C runtime + Rust `edda-rt`. The stub takes
    // the place of the CRT's `mainCRTStartup`: it calls `main` and exits
    // with its return value through the OS. `main` is always present by
    // this point — either the bare `i32 @main()` entry body or the
    // synthesised `i32 @main()` wrapper above.
    if freestanding && program.entry.is_some() && os == Os::Windows {
        emit_windows_entry_stub(context, module);
    }
    Ok(())
}

/// Emit the Windows freestanding OS entry stub `_edda_start`:
/// `void @_edda_start()` that calls `i32 @main()` and passes the result
/// to `ExitProcess`, then `unreachable`. This replaces the CRT's
/// `mainCRTStartup` so the binary links with `/NODEFAULTLIB` and no
/// `edda-rt`.
fn emit_windows_entry_stub<'ctx>(context: &'ctx Context, module: &Module<'ctx>) {
    let Some(main) = module.get_function("main") else {
        return;
    };
    let void_ty = context.void_type();
    let i32_ty = context.i32_type();

    // `ExitProcess(UINT uExitCode)` — external, resolved via kernel32.lib.
    let exit_fn = module.get_function("ExitProcess").unwrap_or_else(|| {
        let exit_ty = void_ty.fn_type(&[i32_ty.into()], false);
        module.add_function("ExitProcess", exit_ty, Some(Linkage::External))
    });

    let start = module.add_function("_edda_start", void_ty.fn_type(&[], false), None);
    let builder = context.create_builder();
    builder.position_at_end(context.append_basic_block(start, "entry"));

    let mut args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = Vec::new();
    for i in 0..main.count_params() {
        let param_ty = main
            .get_nth_param(i)
            .expect("param index below count_params")
            .get_type();
        args.push(undef_of(param_ty).into());
    }
    let code = builder
        .build_call(main, &args, "exit.code")
        .expect("build_call to main in the entry stub must succeed")
        .try_as_basic_value()
        .left()
        .expect("i32 @main() produces a basic return value");
    builder
        .build_call(exit_fn, &[code.into()], "")
        .expect("build_call to ExitProcess in the entry stub must succeed");
    builder
        .build_unreachable()
        .expect("build_unreachable after the noreturn ExitProcess call");
}

/// Whether `body`'s return is the wire-level `__Result<T, E...>` sum a
/// raising entry is projected to — the shape the CRT cannot consume as
/// an exit code (whether returned via sret or directly in a register).
fn entry_returns_result_sum(body: &Body, program: &MirProgram) -> bool {
    match &body.return_ty.kind {
        MirTypeKind::Adt(id) => program
            .adts
            .get(*id)
            .is_some_and(|adt| adt.kind == edda_mir::AdtKind::Sum),
        _ => false,
    }
}

/// Whether `body`'s return is passed via a hidden win64 `sret` pointer
/// (an aggregate return that is not 1/2/4/8 bytes). True for a fallible
/// `main` whose `__Result` sum exceeds 8 bytes; false for small sums
/// (`__Result<i32, E>` is 8 bytes) that return directly in a register.
fn entry_returns_via_sret(body: &Body, program: &MirProgram, os: Os, arch: Arch) -> bool {
    let ret_is_aggregate = matches!(
        &body.return_ty.kind,
        MirTypeKind::Prim(MirPrim::Str)
            | MirTypeKind::Tuple(_)
            | MirTypeKind::Adt(_)
            | MirTypeKind::Slice(_)
            | MirTypeKind::FnPtr(_)
    );
    ret_is_aggregate && win64_indirect_aggregate(&body.return_ty, os, program, arch)
}

/// Emit a CRT-callable `define i32 @main()` that calls the user's entry
/// body. For a `__Result`-returning (fallible) entry the call goes
/// through the hidden sret pointer for >8-byte sums (or stores the
/// direct register return for small ones) and the `__Result` maps to
/// the process exit code (`Ok(code)` ⇒ `code`, payload-less `Ok` ⇒ 0,
/// `Err` ⇒ non-zero discriminant). For a unit-returning entry the
/// wrapper simply calls it and returns `0`.
fn synthesize_entry_wrapper<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    entry_body: &Body,
    program: &MirProgram,
    os: Os,
    arch: Arch,
) -> Result<(), CompileError> {
    let inner = module
        .get_function(FALLIBLE_ENTRY_SYMBOL)
        .expect("wrapped entry body was declared under FALLIBLE_ENTRY_SYMBOL in pass 1");

    let i32_ty = context.i32_type();
    let main_fn = module.add_function("main", i32_ty.fn_type(&[], false), None);
    let builder = context.create_builder();
    builder.position_at_end(context.append_basic_block(main_fn, "entry"));

    // Unit-returning entry: call it (undef capability args) and exit 0.
    if matches!(entry_body.return_ty.kind, MirTypeKind::Unit) {
        let mut args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = Vec::new();
        for i in 0..inner.count_params() {
            let param_ty = inner
                .get_nth_param(i)
                .expect("param index below count_params")
                .get_type();
            args.push(undef_of(param_ty).into());
        }
        builder
            .build_call(inner, &args, "call.main")
            .expect("build_call to the unit entry body must succeed");
        builder
            .build_return(Some(&i32_ty.const_zero()))
            .expect("build_return of the constant-0 exit code");
        return Ok(());
    }

    let ret_ty = inkwell_basic_type(context, &entry_body.return_ty, program, arch).ok_or_else(
        || CompileError::UnsupportedMirShape {
            shape: "non-lowerable-entry-return",
            detail: format!(
                "fallible entry returns {:?}, which is not lowerable",
                entry_body.return_ty.kind
            ),
        },
    )?;

    // Allocate the Result slot and call the entry with the call shape
    // its declared signature demands (mirrors `lower_fn_sig`): a hidden
    // sret pointer for >8-byte sums, or a direct call whose returned
    // aggregate is stored into the slot for small sums.
    let slot = builder
        .build_alloca(ret_ty, "result.slot")
        .expect("build_alloca in a positioned entry block must succeed");
    let uses_sret = entry_returns_via_sret(entry_body, program, os, arch);
    // Fill every remaining declared parameter (capability slots — the
    // entry's effect row lowers them as leading opaque `ptr` params)
    // with undef: the CRT's bare-`main` call passes register garbage in
    // those positions today, and the runtime externs never dereference
    // the capability token, so undef matches the existing semantics.
    let mut args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = Vec::new();
    if uses_sret {
        args.push(slot.into());
    }
    for i in args.len() as u32..inner.count_params() {
        let param_ty = inner
            .get_nth_param(i)
            .expect("param index below count_params")
            .get_type();
        args.push(undef_of(param_ty).into());
    }
    let call = builder
        .build_call(inner, &args, "call.main")
        .expect("build_call to the entry body must succeed");
    if uses_sret {
        let sret_kind = Attribute::get_named_enum_kind_id("sret");
        let sret_attr = context.create_type_attribute(sret_kind, ret_ty.as_any_type_enum());
        call.add_attribute(AttributeLoc::Param(0), sret_attr);
    } else {
        let value = call
            .try_as_basic_value()
            .left()
            .expect("direct-return entry produces a basic Result value");
        builder
            .build_store(slot, value)
            .expect("build_store of the direct Result return into the slot");
    }

    // The Result tag is the leading byte (Ok ⇒ 0). Branch: Ok returns
    // the success payload (the user's declared exit code), Err returns
    // the non-zero error discriminant.
    let tag = builder
        .build_load(context.i8_type(), slot, "exit.tag")
        .expect("build_load of the Result tag byte from the sret slot")
        .into_int_value();
    let is_ok = builder
        .build_int_compare(
            inkwell::IntPredicate::EQ,
            tag,
            context.i8_type().const_zero(),
            "is.ok",
        )
        .expect("build_int_compare of the Result tag against 0");
    let ok_bb = context.append_basic_block(main_fn, "exit.ok");
    let err_bb = context.append_basic_block(main_fn, "exit.err");
    builder
        .build_conditional_branch(is_ok, ok_bb, err_bb)
        .expect("build_conditional_branch on the Result tag");

    builder.position_at_end(ok_bb);
    let ok_code = ok_payload_exit_code(context, &builder, slot, entry_body, program, arch);
    builder
        .build_return(Some(&ok_code))
        .expect("build_return of the Ok-payload exit code");

    builder.position_at_end(err_bb);
    let err_code = builder
        .build_int_z_extend(tag, i32_ty, "exit.code")
        .expect("zero-extend i8 tag to i32 exit code");
    builder
        .build_return(Some(&err_code))
        .expect("build_return of the i32 error exit code");
    Ok(())
}

/// Undef value of an arbitrary basic type — the placeholder the entry
/// wrapper passes for capability parameters it cannot materialise.
fn undef_of(ty: inkwell::types::BasicTypeEnum<'_>) -> inkwell::values::BasicValueEnum<'_> {
    use inkwell::types::BasicTypeEnum as T;
    match ty {
        T::ArrayType(t) => t.get_undef().into(),
        T::FloatType(t) => t.get_undef().into(),
        T::IntType(t) => t.get_undef().into(),
        T::PointerType(t) => t.get_undef().into(),
        T::StructType(t) => t.get_undef().into(),
        T::VectorType(t) => t.get_undef().into(),
    }
}

/// Load the Ok variant's int payload from the Result `slot` and cast it
/// to the `i32` exit code. Returns constant `0` for payload-less or
/// non-int Ok variants.
fn ok_payload_exit_code<'ctx>(
    context: &'ctx Context,
    builder: &inkwell::builder::Builder<'ctx>,
    slot: inkwell::values::PointerValue<'ctx>,
    entry_body: &Body,
    program: &MirProgram,
    arch: Arch,
) -> inkwell::values::IntValue<'ctx> {
    let i32_ty = context.i32_type();
    let zero = i32_ty.const_zero();
    let MirTypeKind::Adt(adt_id) = &entry_body.return_ty.kind else {
        return zero;
    };
    let Some(adt) = program.adts.get(*adt_id) else {
        return zero;
    };
    if adt.kind != edda_mir::AdtKind::Sum {
        return zero;
    }
    let Some(ok_variant) = adt.variants.first() else {
        return zero;
    };
    if ok_variant.fields.len() != 1 {
        return zero;
    }
    let Some(payload_ty) = super::ty::sum_variant_payload_type(context, adt, 0, program, arch)
    else {
        return zero;
    };
    let Some(sum_ty) = inkwell_basic_type(context, &entry_body.return_ty, program, arch) else {
        return zero;
    };
    let payload_ptr = builder
        .build_struct_gep(sum_ty.into_struct_type(), slot, 1, "ok.pay.ptr")
        .expect("build_struct_gep into the Result payload slot (field 1)");
    let variant_struct = builder
        .build_load(payload_ty, payload_ptr, "ok.pay")
        .expect("build_load of the Ok variant payload")
        .into_struct_value();
    let field = builder
        .build_extract_value(variant_struct, 0, "ok.code")
        .expect("build_extract_value of the Ok payload's single field");
    match field {
        inkwell::values::BasicValueEnum::IntValue(iv) => builder
            .build_int_cast(iv, i32_ty, "exit.ok")
            .expect("int-cast the Ok payload to the i32 exit code"),
        _ => zero,
    }
}

/// Pin `func` to `linkonce_odr` linkage backed by a COMDAT named after
/// its `symbol`, so identical copies emitted into other member objects
/// fold to one definition at link time.
fn apply_linkonce_comdat<'ctx>(module: &Module<'ctx>, func: FunctionValue<'ctx>, symbol: &str) {
    func.set_linkage(Linkage::LinkOnceODR);
    let comdat = module.get_or_insert_comdat(symbol);
    comdat.set_selection_kind(ComdatSelectionKind::Any);
    func.as_global_value().set_comdat(comdat);
}

/// Per-body symbol assignment: the linker-visible name plus whether it
/// is emitted under `linkonce_odr` + COMDAT (source functions) or with
/// default external linkage (entry / export / synthesised bodies).
struct SymbolPlan {
    /// Indexed by `BodyId::index()`; the symbol passed to `add_function`.
    names: Vec<String>,
    /// Parallel to `names`; `true` ⇒ apply [`apply_linkonce_comdat`].
    comdat: Vec<bool>,
}

/// Compute the per-body [`SymbolPlan`]. See the invariants for the
/// entry / export / source-function / synthesised-body rules.
/// `wrapped_entry` is the entry body id when it needs the wrapper, so it
/// is emitted under [`FALLIBLE_ENTRY_SYMBOL`] for the `main` wrapper.
fn compute_symbol_names(
    program: &MirProgram,
    interner: &Interner,
    module_name: &str,
    wrapped_entry: Option<BodyId>,
) -> SymbolPlan {
    let mut seen: HashSet<String> = HashSet::with_capacity(program.bodies.len());
    let mut names: Vec<String> = Vec::with_capacity(program.bodies.len());
    let mut comdat: Vec<bool> = Vec::with_capacity(program.bodies.len());
    for (body_id, body) in program.bodies.iter_enumerated() {
        let (mut name, is_comdat) = if program.entry == Some(body_id) {
            // The entry. A fallible entry hands the `main` symbol to the
            // synthesised wrapper and takes the internal symbol itself;
            // otherwise `pick_entry` guarantees this body is named `main`,
            // the symbol the CRT calls. External either way.
            if wrapped_entry == Some(body_id) {
                (FALLIBLE_ENTRY_SYMBOL.to_string(), false)
            } else {
                (interner.resolve(body.name).to_string(), false)
            }
        } else if let Some(export) = body.export_symbol {
            (interner.resolve(export).to_string(), false)
        } else if let Some(qn) = body.qualified_name {
            (interner.resolve(qn).to_string(), true)
        } else {
            // Synthesised body (fn-value shim / closure): member-unique
            // external symbol so distinct synthesised bodies never collide
            // or fold across member objects.
            (
                format!(
                    "{module_name}.{}.{}",
                    interner.resolve(body.name),
                    body_id.index()
                ),
                false,
            )
        };
        // Defensive collision guard: the rules above already yield unique
        // symbols (qualified names are unique per logical function; the
        // body index disambiguates synthesised bodies), but if two ever
        // coincide, suffix `.bN` so `module.add_function` lands a distinct
        // LLVM symbol. Keeps the COMDAT name in sync because the suffixed
        // `name` is what flows to `apply_linkonce_comdat`.
        if seen.contains(&name) {
            name = format!("{name}.b{}", body_id.index());
        }
        seen.insert(name.clone());
        names.push(name);
        comdat.push(is_comdat);
    }
    SymbolPlan { names, comdat }
}
