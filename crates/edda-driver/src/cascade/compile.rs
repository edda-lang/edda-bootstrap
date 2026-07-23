//! Cascade compile stage: lower the typed HIR into `MirProgram` form,
//! dump a `.mir` artifact, drive MIR -> LLVM -> native object emission,
//! and build the `FunctionInput`/`TypeDeclInput`/`ConstInput`/
//! `ExternInput` slices MIR lowering consumes from the typed package.

use std::path::Path;

use edda_compile::Emitter;
use edda_mir::{
    AllocFmtBindings, ConstInput, ConstValue, ExternInput, FmtBindings, FunctionInput,
    LoweringInput, MirProgram, TypeDeclInput, lower as lower_mir, pretty as pretty_mir,
};
use edda_resolve::{BindingKind, ModulePath, ResolvedPackage};
use edda_types::TypedPackage;

use crate::context::Driver;

use super::{STAGE_COMPILE, push_compile_error, push_io_error, push_mir_error};

/// Outcome of the compile stage as the cascade sees it.
pub(super) struct CompileReport {
    /// Number of artifacts the stage successfully wrote to disk.
    pub(super) artifacts_generated: usize,
}

/// Lower the typed HIR into [`MirProgram`] form, dump a pretty-printed
/// `.mir` artifact for inspection, and drive the MIR → LLVM → native
/// object-file pipeline through
/// [`edda_compile::Emitter::compile_program_to_object`].
///
/// Returns `Some(report)` when MIR lowering succeeded — the caller
/// uses `report.artifacts_generated` to populate the summary line.
/// Returns `None` only when MIR lowering itself rejected the typed
/// HIR; each [`MirError`] is projected to a [`Diagnostic`] through
/// [`MirError::class`] + [`MirError::to_string`].
///
/// Both artifacts land under `target/edda/<triple>/`:
/// - `<package>.mir` — pretty-printed MIR for inspection.
/// - `<package>.o` — native object file emitted by LLVM 18 via
///   `inkwell`.
///
/// Sub-failures after MIR lowering (filesystem IO on either artifact,
/// LLVM init failure, unsupported MIR shape) push a diagnostic and
/// continue with the artifacts that did succeed; the summary's
/// generated-count reflects what actually hit disk.
pub(super) fn drive_compile(driver: &mut Driver, typed: &TypedPackage) -> Option<CompileReport> {
    // The MIR-lowering pass needs the resolver's span-keyed resolution
    // map to translate multi-segment paths (cross-module function calls,
    // variant constructors) into the binding ids registered in the
    // program-wide function / ADT maps. It is also the source of each
    // function's canonical module path for symbol mangling.
    let resolved = driver
        .resolved
        .as_ref()
        .expect("drive_compile requires a resolved package");
    let fn_inputs = build_fn_inputs(typed, resolved, &driver.interner);
    if !reject_duplicate_symbol_overrides(
        &mut driver.diagnostics,
        &driver.lint_cfg,
        &driver.interner,
        &fn_inputs,
    ) {
        return None;
    }
    let abi_rt_shadows = collect_abi_rt_shadows(&fn_inputs, &driver.interner);
    if std::env::var_os("EDDA_DBG_RTSHADOW").is_some() {
        let has_hc = abi_rt_shadows.contains("__edda_heap_create");
        let mut v: Vec<&str> = abi_rt_shadows.iter().map(|s| s.as_str()).collect();
        v.sort();
        eprintln!(
            "[RTSHADOW] pkg={} heap_create={} shadows={:?}",
            driver.manifest.package.as_ref(),
            has_hc,
            v
        );
    }
    driver.abi_exports = collect_abi_exports(&fn_inputs, &driver.interner);
    let resolutions = resolved.resolutions();
    let type_decl_inputs = build_type_decl_inputs(typed, resolved, &driver.interner);
    let extern_inputs = build_extern_inputs(typed);
    let const_inputs = build_const_inputs(typed);
    let fmt_bindings = resolve_fmt_bindings(resolved, &driver.interner);
    let alloc_fmt_bindings = resolve_alloc_fmt_bindings(resolved, &driver.interner);
    let alloc_error = resolve_alloc_error_binding(resolved, &driver.interner);

    // Rebuild the method-resolutions map as a `std::collections::HashMap`
    // so the MIR-lowering crate doesn't have to depend on `ahash` just
    // for the input type. The contents are identical; the only
    // difference is the hash table implementation.
    let method_resolutions: std::collections::HashMap<edda_span::Span, edda_resolve::BindingId> =
        typed.method_resolutions.iter().map(|(s, b)| (*s, *b)).collect();

    // Rebuild intrinsic_calls map similarly.
    let intrinsic_calls: std::collections::HashMap<edda_span::Span, edda_types::IntrinsicKind> =
        typed.intrinsic_calls.iter().map(|(s, k)| (*s, *k)).collect();

    let pointer_width_bytes =
        edda_compile::pointer_width(driver.target.triple().arch()) / 8;

    // `==` / `!=` on a `derive eq` nominal operand lowers to a `Call` into
    // the materialised structural comparator. Built from the pass-2 resolved package so every
    // `std.core.compare.eq_<T>_<hex>` module is already present, and from
    // `typed.ty_cx` so the transitive closure of nominal field / payload
    // types reachable from each `derive eq` target is mapped too.
    let eq_comparators = crate::codegen::build_eq_comparator_map(
        resolved,
        &driver.interner,
        &driver.ty_interner,
        &typed.ty_cx,
    );

    // An aggregate f-string interpolation slot (`f"{v}"` where `v` derives
    // `debug`) lowers to a `Call` into the materialised
    // `std.core.fmt.debug_<T>.format` formatter.
    // Built from the pass-2 resolved package so every `debug_<T>_<hex>`
    // module is present, and from `typed.ty_cx` for the transitive closure of
    // nominal field / payload types reachable from each `derive debug` target.
    let debug_formatters = crate::codegen::build_debug_formatter_map(
        resolved,
        &driver.interner,
        &driver.ty_interner,
        &typed.ty_cx,
    );

    let input = LoweringInput {
        interner: &driver.interner,
        ty_interner: &driver.ty_interner,
        resolutions,
        method_resolutions: &method_resolutions,
        intrinsic_calls: &intrinsic_calls,
        type_decls: &type_decl_inputs,
        functions: &fn_inputs,
        externs: &extern_inputs,
        consts: &const_inputs,
        fmt_bindings,
        alloc_fmt_bindings,
        alloc_error,
        pointer_width_bytes,
        // §C10 — wire the active target and TyCx through so MIR can
        // evaluate `comptime size_of(T)` / `align_of(T)` /
        // `target_has(...)` at lower-time.
        target_cfg: Some(&driver.target),
        ty_cx: Some(&typed.ty_cx),
        comptime_type_paths: &typed.comptime_type_paths,
        comptime_builtin_calls: &typed.comptime_builtin_calls,
        comptime_fn_calls: &typed.comptime_fn_calls,
        primitive_static_calls: &typed.primitive_static_calls,
        capability_method_calls: &typed.capability_method_calls,
        eq_comparators: &eq_comparators,
        debug_formatters: &debug_formatters,
    };

    let mut program = match lower_mir(input) {
        Ok(p) => p,
        Err(errs) => {
            for err in &errs {
                push_mir_error(&mut driver.diagnostics, &driver.lint_cfg, err);
            }
            return None;
        }
    };

    // Str equality (`==`/`!=`) needs no `import std.text.string` — it is
    // a built-in operator, so nothing naturally pulls that module into
    // this member's own closure. When this member's own object doesn't
    // already shadow `__edda_str_eq` (`abi_rt_shadows`) but its MIR uses
    // the operator anyway, compile `std.text.string`'s `equals` alone
    // into a standalone object the link stage always appends.
    if !abi_rt_shadows.contains(super::str_eq_support::STR_EQ_SYMBOL)
        && super::str_eq_support::uses_str_eq(&program)
    {
        driver.str_eq_support_object = super::str_eq_support::compile_str_eq_support(driver);
    }

    pick_entry(&mut program, &driver.interner);
    collect_dll_imports(driver, typed);
    // Record whether this member produces a binary so the link stage can
    // choose executable-vs-static-archive.
    driver.has_entry = program.entry.is_some();

    // The entry body (`@main`) is invoked by the CRT with no threaded
    // capabilities, so any `with allocator`-style effect row on `main`
    // must have its capability handles materialised in-body rather than
    // received as leading parameters.
    // Without this the entry would declare `@main(ptr)` and the CRT's
    // argument-less call could not supply the allocator the program needs
    // to allocate from `main`. `abi_rt_shadows` (read here before the move
    // into `driver` below) tells it whether `std.mem.rt.heap_create` is in
    // this member's own compiled closure, so the `Allocator` slot gets a
    // minted heap handle instead of a null one when needed.
    if let Some(entry) = program.entry {
        edda_mir::materialize_entry_capabilities(
            &mut program,
            entry,
            &driver.interner,
            &abi_rt_shadows,
        );
    }
    driver.abi_rt_shadows = abi_rt_shadows;

    let out_dir = driver
        .package_root
        .join("target")
        .join("edda")
        .join(driver.target.triple().to_string());
    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        push_io_error(&mut driver.diagnostics, &driver.lint_cfg, &out_dir, &err);
        return Some(CompileReport { artifacts_generated: 0 });
    }

    let mut artifacts_generated = 0;

    if write_mir_dump(driver, &program, &out_dir) {
        artifacts_generated += 1;
    }
    if write_object(driver, &program, &out_dir) {
        artifacts_generated += 1;
    }

    Some(CompileReport { artifacts_generated })
}

/// Pretty-print the MIR program and write it to
/// `<out_dir>/<package>.mir`.
fn write_mir_dump(driver: &mut Driver, program: &MirProgram, out_dir: &Path) -> bool {
    let mir_text = pretty_mir(program, &driver.interner);
    let mir_path = out_dir.join(format!("{}.mir", driver.manifest.package));
    if let Err(err) = std::fs::write(&mir_path, &mir_text) {
        push_io_error(&mut driver.diagnostics, &driver.lint_cfg, &mir_path, &err);
        return false;
    }
    driver
        .reporter
        .module_done(&mir_path.display().to_string(), STAGE_COMPILE);
    true
}

/// Drive [`edda_compile::Emitter::compile_program_to_object`] to emit
/// `<out_dir>/<package>.o`.
fn write_object(driver: &mut Driver, program: &MirProgram, out_dir: &Path) -> bool {
    let obj_path = out_dir.join(format!("{}.o", driver.manifest.package));
    let emitter = Emitter::new();
    let module_name = driver.manifest.package.as_ref();
    let result = emitter.compile_program_to_object(
        module_name,
        &driver.target,
        program,
        &driver.interner,
        &obj_path,
        driver.options.freestanding,
    );
    if let Err(err) = result {
        push_compile_error(&mut driver.diagnostics, &driver.lint_cfg, &err);
        return false;
    }
    driver
        .reporter
        .module_done(&obj_path.display().to_string(), STAGE_COMPILE);
    driver.compile_outputs.push(obj_path);
    true
}

/// Materialise the [`FunctionInput`] slice MIR lowering consumes from
/// the typed package.
fn build_fn_inputs<'a>(
    typed: &'a TypedPackage,
    resolved: &ResolvedPackage,
    interner: &edda_intern::Interner,
) -> Vec<FunctionInput<'a>> {
    typed
        .fn_bodies
        .iter()
        .filter_map(|tf| {
            typed.ty_cx.sig(tf.binding).map(|sig| {
                let attrs = typed.attributes.get(&tf.binding).copied();
                let export_symbol = attrs.and_then(|a| a.abi_symbol);
                let abi = attrs
                    .and_then(|a| a.abi)
                    .map(|abi| attr_abi_to_mir(abi, interner));
                let segments = resolved
                    .module_entry(tf.binding.module)
                    .canonical_path
                    .segments();
                let qualified_name =
                    Some(interner.intern(&mangle_symbol(segments, tf.name, interner)));
                FunctionInput {
                    binding: tf.binding,
                    name: tf.name,
                    span: tf.span,
                    sig,
                    body: &tf.body,
                    export_symbol,
                    abi,
                    qualified_name,
                }
            })
        })
        .collect()
}

/// Reject duplicate `@abi("sym")` symbol-name overrides within this
/// member's link set. Returns `false` (and pushes one diagnostic per
/// duplicate) when any two functions claim the same symbol.
fn reject_duplicate_symbol_overrides(
    diagnostics: &mut edda_diag::Diagnostics,
    lint_cfg: &edda_diag::LintConfig,
    interner: &edda_intern::Interner,
    fn_inputs: &[FunctionInput<'_>],
) -> bool {
    let mut first_claim: std::collections::HashMap<edda_intern::Symbol, edda_span::Span> =
        std::collections::HashMap::new();
    let mut ok = true;
    for f in fn_inputs {
        let Some(sym) = f.export_symbol else { continue };
        if first_claim.contains_key(&sym) {
            let class = edda_diag::DiagnosticClass::TypecheckError;
            let severity = lint_cfg.effective(class);
            diagnostics.push(edda_diag::Diagnostic::new(
                class,
                severity,
                f.span,
                format!(
                    "duplicate `@abi(\"{}\")` symbol-name override — another function in this package already exports this linker symbol",
                    interner.resolve(sym)
                ),
            ));
            ok = false;
        } else {
            first_claim.insert(sym, f.span);
        }
    }
    ok
}

/// Mangle a function's canonical module path + leaf into its
/// linker-visible symbol (`<seg0>.<seg1>...<segN>.<leaf>`).
fn mangle_symbol(
    segments: &[edda_intern::Symbol],
    leaf: edda_intern::Symbol,
    interner: &edda_intern::Interner,
) -> String {
    let mut out = String::new();
    for seg in segments {
        out.push_str(interner.resolve(*seg));
        out.push('.');
    }
    out.push_str(interner.resolve(leaf));
    out
}

/// Materialise the [`TypeDeclInput`] slice MIR lowering consumes from
/// the typed package.
fn build_type_decl_inputs<'a>(
    typed: &'a TypedPackage,
    resolved: &ResolvedPackage,
    interner: &edda_intern::Interner,
) -> Vec<TypeDeclInput<'a>> {
    typed
        .type_decls
        .iter()
        .filter_map(|td| {
            typed.ty_cx.type_decl(td.binding).map(|info| {
                let attrs = typed.attributes.get(&td.binding).copied();
                let align = attrs
                    .and_then(|a| a.align)
                    .and_then(edda_mir::AlignBytes::new);
                let repr = attrs.and_then(|a| a.repr).map(attr_repr_to_mir);
                let layout = attrs.and_then(|a| a.layout).map(attr_layout_to_mir);
                let synthesize_box_ptr =
                    is_box_t_materialisation(td.binding, td.name, resolved, interner);
                TypeDeclInput {
                    binding: td.binding,
                    name: td.name,
                    info,
                    align,
                    repr,
                    layout,
                    synthesize_box_ptr,
                }
            })
        })
        .collect()
}

/// Detect whether `(binding, name)` is the `type Box {}` materialised
/// inside a codegen-generated `std.mem.alloc.Box_<T>` module — the
/// source-side spec-name recognition rule for the option-C runtime ABI.
///
/// Returns `true` when:
/// - `name` resolves to the literal string `"Box"`, AND
/// - `binding.module`'s canonical path has exactly four segments
///   `std . mem . alloc . Box_<T>`, where `<T>` is any non-empty
///   argument leaf (e.g. `Box_Expr`, `Box_TreeNode`, `Box_StringMap_f64`).
///
/// Stricter than just leaf-prefix matching: a user module named
/// `Box_Foo` containing a `type Box` does not trigger the synthesis
/// because its parent path is not `std.mem.alloc`.
fn is_box_t_materialisation(
    binding: edda_resolve::BindingId,
    name: edda_intern::Symbol,
    resolved: &ResolvedPackage,
    interner: &edda_intern::Interner,
) -> bool {
    if interner.resolve(name) != "Box" {
        return false;
    }
    let module_entry = resolved.module_entry(binding.module);
    let segments = module_entry.canonical_path.segments();
    if segments.len() != 4 {
        return false;
    }
    if interner.resolve(segments[0]) != "std" {
        return false;
    }
    if interner.resolve(segments[1]) != "mem" {
        return false;
    }
    if interner.resolve(segments[2]) != "alloc" {
        return false;
    }
    let leaf = interner.resolve(segments[3]);
    leaf.len() > "Box_".len() && leaf.starts_with("Box_")
}

/// Convert a slice-2 [`edda_types::AttrAbi`] to the MIR-layer
/// [`edda_mir::AbiTag`]. The mapping is total — every `AttrAbi` variant
/// has a defined `AbiTag`. `SysV64` / `Win64` go through
/// [`edda_mir::AbiTag::Named`] because the locked
/// `resolve_named_abi` catalogue in `edda-compile` keys on lowercase
/// strings.
fn attr_abi_to_mir(abi: edda_types::AttrAbi, interner: &edda_intern::Interner) -> edda_mir::AbiTag {
    use edda_types::AttrAbi;
    match abi {
        AttrAbi::C => edda_mir::AbiTag::C,
        AttrAbi::System => edda_mir::AbiTag::System,
        AttrAbi::SysV64 => edda_mir::AbiTag::Named(interner.intern("sysv64")),
        AttrAbi::Win64 => edda_mir::AbiTag::Named(interner.intern("win64")),
    }
}

/// Convert a slice-2 [`edda_types::AttrRepr`] to the MIR-layer
/// [`edda_mir::ReprKind`].
fn attr_repr_to_mir(repr: edda_types::AttrRepr) -> edda_mir::ReprKind {
    use edda_types::AttrRepr;
    match repr {
        AttrRepr::Edda => edda_mir::ReprKind::Edda,
        AttrRepr::C => edda_mir::ReprKind::C,
        AttrRepr::Transparent => edda_mir::ReprKind::Transparent,
        AttrRepr::Simd => edda_mir::ReprKind::Simd,
        AttrRepr::Opaque => edda_mir::ReprKind::Opaque,
    }
}

/// Convert a slice-2 [`edda_types::AttrLayout`] to the MIR-layer
/// [`edda_mir::LayoutPolicy`].
fn attr_layout_to_mir(layout: edda_types::AttrLayout) -> edda_mir::LayoutPolicy {
    use edda_types::AttrLayout;
    match layout {
        AttrLayout::Natural => edda_mir::LayoutPolicy::Natural,
        AttrLayout::Declared => edda_mir::LayoutPolicy::Declared,
        AttrLayout::Sorted => edda_mir::LayoutPolicy::Sorted,
        AttrLayout::Packed => edda_mir::LayoutPolicy::Packed,
    }
}

/// Resolve `std.core.fmt`'s extern format shims into a [`FmtBindings`]. Used
/// by MIR fstring lowering to route `f"{x}"` through the stdlib's
/// extern declarations instead of inventing a `FuncRef::Extern` from
/// thin air. Each field is independent — a missing `std.core.fmt.format_u64`
/// does not block routing `format_i64`.
fn resolve_fmt_bindings(
    resolved: &ResolvedPackage,
    interner: &edda_intern::Interner,
) -> FmtBindings {
    let std_sym = interner.intern("std");
    let core_sym = interner.intern("core");
    let fmt_sym = interner.intern("fmt");
    let module_path = ModulePath::new(Box::from([std_sym, core_sym, fmt_sym]));
    let Some(module_id) = resolved.graph().lookup_by_path(&module_path) else {
        return FmtBindings::default();
    };
    let items = &resolved.module(module_id).items;
    let lookup_fn = |name: &str| -> Option<edda_resolve::BindingId> {
        let sym = interner.intern(name);
        let id = items.lookup(sym)?;
        match items.get(id).kind {
            BindingKind::Function => Some(id),
            _ => None,
        }
    };
    FmtBindings {
        format_i64: lookup_fn("format_i64"),
        format_u64: lookup_fn("format_u64"),
        format_f64: lookup_fn("format_f64"),
        format_bool: lookup_fn("format_bool"),
        format_str: lookup_fn("format_str"),
        string_concat: lookup_fn("string_concat"),
    }
}

/// Resolve `std.core.fmt`'s allocator-taking pure-Edda format bodies
/// into an [`AllocFmtBindings`]. Used by MIR fstring lowering to
/// retarget `f"{n}"` / `f"{b}"` through `i64_to_string` /
/// `u64_to_string` / `f64_to_string` / `bool_to_string` (threading the
/// enclosing function's own `Allocator`) instead of the capability-blind
/// `format_i64`/etc. externs `resolve_fmt_bindings` resolves.
fn resolve_alloc_fmt_bindings(
    resolved: &ResolvedPackage,
    interner: &edda_intern::Interner,
) -> AllocFmtBindings {
    let std_sym = interner.intern("std");
    let core_sym = interner.intern("core");
    let fmt_sym = interner.intern("fmt");
    let module_path = ModulePath::new(Box::from([std_sym, core_sym, fmt_sym]));
    let Some(module_id) = resolved.graph().lookup_by_path(&module_path) else {
        return AllocFmtBindings::default();
    };
    let items = &resolved.module(module_id).items;
    let lookup_fn = |name: &str| -> Option<edda_resolve::BindingId> {
        let sym = interner.intern(name);
        let id = items.lookup(sym)?;
        match items.get(id).kind {
            BindingKind::Function => Some(id),
            _ => None,
        }
    };
    AllocFmtBindings {
        i64_to_string: lookup_fn("i64_to_string"),
        u64_to_string: lookup_fn("u64_to_string"),
        f64_to_string: lookup_fn("f64_to_string"),
        bool_to_string: lookup_fn("bool_to_string"),
        concat: resolve_string_concat_binding(resolved, interner),
    }
}

/// Resolve `std.text.string.concat`'s `BindingId`, the fstring-fold
/// retarget for [`AllocFmtBindings::concat`].
fn resolve_string_concat_binding(
    resolved: &ResolvedPackage,
    interner: &edda_intern::Interner,
) -> Option<edda_resolve::BindingId> {
    let std_sym = interner.intern("std");
    let text_sym = interner.intern("text");
    let string_sym = interner.intern("string");
    let module_path = ModulePath::new(Box::from([std_sym, text_sym, string_sym]));
    let module_id = resolved.graph().lookup_by_path(&module_path)?;
    let items = &resolved.module(module_id).items;
    let sym = interner.intern("concat");
    let id = items.lookup(sym)?;
    match items.get(id).kind {
        BindingKind::Function => Some(id),
        _ => None,
    }
}

/// Resolve `std.mem.alloc.AllocError`'s `BindingId` — the row-admission
/// check for the alloc-retarget.
fn resolve_alloc_error_binding(
    resolved: &ResolvedPackage,
    interner: &edda_intern::Interner,
) -> Option<edda_resolve::BindingId> {
    let std_sym = interner.intern("std");
    let mem_sym = interner.intern("mem");
    let alloc_sym = interner.intern("alloc");
    let module_path = ModulePath::new(Box::from([std_sym, mem_sym, alloc_sym]));
    let module_id = resolved.graph().lookup_by_path(&module_path)?;
    let items = &resolved.module(module_id).items;
    let sym = interner.intern("AllocError");
    let id = items.lookup(sym)?;
    match items.get(id).kind {
        BindingKind::TypeDecl => Some(id),
        _ => None,
    }
}

/// Materialise the [`ConstInput`] slice MIR lowering consumes from
/// the typed package. Each entry pre-interns one module-level `let`
/// constant; reference-site lowering reads back through
/// `ctx.module_consts` to emit `Operand::Const`.
fn build_const_inputs(typed: &TypedPackage) -> Vec<ConstInput> {
    use edda_types::ConstInit;
    typed
        .ty_cx
        .iter_consts()
        .filter_map(|(binding, ty, init)| {
            let value = match init {
                ConstInit::Int(v) => ConstValue::Int(v),
                ConstInit::Float(bits) => ConstValue::Float(bits),
                ConstInit::Bool(b) => ConstValue::Bool(b),
                ConstInit::Str(sym) => ConstValue::Str(sym),
                ConstInit::Unsupported => return None,
            };
            Some(ConstInput {
                binding,
                ty,
                value,
            })
        })
        .collect()
}

/// Materialise the [`ExternInput`] slice MIR lowering consumes from
/// the typed package.
fn build_extern_inputs<'a>(typed: &'a TypedPackage) -> Vec<ExternInput<'a>> {
    typed
        .extern_decls
        .iter()
        .filter_map(|ed| {
            typed.ty_cx.sig(ed.binding).map(|sig| ExternInput {
                binding: ed.binding,
                name: ed.name,
                span: ed.span,
                symbol: ed.symbol,
                sig,
            })
        })
        .collect()
}

/// Record every `extern "sym" from "dll"` declaration's `(dll, symbol)`
/// pair on [`Driver::dll_imports`] so the link stage can synthesise the
/// PE import libraries.
fn collect_dll_imports(driver: &mut Driver, typed: &TypedPackage) {
    for ed in &typed.extern_decls {
        let Some(dll) = ed.dll else { continue };
        let dll = driver.interner.resolve(dll).to_string();
        let symbol = driver.interner.resolve(ed.symbol).to_string();
        driver.dll_imports.entry(dll).or_default().insert(symbol);
    }
}

/// Collect the set of `@abi("__edda_*")` export symbols this member's
/// functions define. A non-empty set means the compiled object shadows
/// `edda_rt.lib`'s definition of those runtime symbols, so the link
/// stage must let the first-listed object definition win over the
/// redundant rt member.
fn collect_abi_rt_shadows(
    fn_inputs: &[FunctionInput<'_>],
    interner: &edda_intern::Interner,
) -> std::collections::BTreeSet<String> {
    fn_inputs
        .iter()
        .filter_map(|f| f.export_symbol)
        .map(|sym| interner.resolve(sym).to_string())
        .filter(|name| name.starts_with("__edda_"))
        .collect()
}

/// Collect the C-ABI export surface of this member: the `@abi("sym")`
/// symbol-name overrides that do not shadow a runtime symbol. The link
/// stage turns each into an explicit DLL export when the manifest
/// requests `kind = "dynamic_library"`; ELF/Mach-O dynamic links export
/// non-hidden symbols implicitly and ignore this set.
fn collect_abi_exports(
    fn_inputs: &[FunctionInput<'_>],
    interner: &edda_intern::Interner,
) -> std::collections::BTreeSet<String> {
    fn_inputs
        .iter()
        .filter_map(|f| f.export_symbol)
        .map(|sym| interner.resolve(sym).to_string())
        .filter(|name| !name.starts_with("__edda_"))
        .collect()
}

/// Tag the program's binary entry point.
///
/// The bootstrap MVP picks the first body named `main` — full
/// `build-system.md` §10's `bin` selection logic lands when the
/// manifest grows a `[bin]` table.
fn pick_entry(program: &mut MirProgram, interner: &edda_intern::Interner) {
    let main_sym = interner.intern("main");
    program.entry = program
        .bodies
        .iter_enumerated()
        .find(|(_, body)| body.name == main_sym)
        .map(|(id, _)| id);
}
