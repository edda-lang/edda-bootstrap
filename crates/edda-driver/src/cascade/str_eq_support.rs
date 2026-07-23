//! Conditional standalone compile of `std.text.string`'s `equals`
//! function, so `__edda_str_eq` always resolves to the pure-Edda body
//! when Str equality is emitted — even when this member's own source
//! never imports `std.text.string`.
//!
//! `==` / `!=` on `String` is a built-in operator, not a call through
//! `std.text.string`, so nothing naturally pulls that module into a
//! member's own closure the way an explicit `import` would. The
//! backend (`edda-compile`) unconditionally lowers the operator to a
//! call against the `__edda_str_eq` symbol name regardless; this module
//! guarantees a strong, pure-Edda definition of that symbol is always
//! part of the executable link when the operator is used and this
//! member's own object does not already provide it.

use std::collections::HashMap;
use std::path::PathBuf;

use edda_compile::Emitter;
use edda_diag::{Diagnostics, LintConfig};
use edda_mir::{
    AllocFmtBindings, BinOp, FmtBindings, FunctionInput, LoweringInput, MirPrim, MirProgram,
    Rvalue, RvalueKind, StatementKind, lower as lower_mir,
};
use edda_resolve::{FsLoader, ModulePath, ResolveCx, build_resolved_package, build_source_graph};
use edda_span::Span;

use crate::context::Driver;

/// The runtime-extern symbol name `Str`-class `Eq`/`Ne` lowers to
/// (`edda-compile/src/lower/rvalue/binop/str.rs::STR_EQ_SYMBOL`,
/// duplicated here since the two crates have no shared constants module).
pub(super) const STR_EQ_SYMBOL: &str = "__edda_str_eq";

const STR_EQ_SUPPORT_ARTIFACT: &str = "std_text_string_str_eq";

/// Whether any body in `program` lowers a `Str`-class `Eq`/`Ne` binop —
/// the signal that this member needs `std.text.string`'s `equals` body
/// linked in to provide [`STR_EQ_SYMBOL`].
pub(super) fn uses_str_eq(program: &MirProgram) -> bool {
    program.bodies.iter().any(|body| {
        body.blocks.iter().any(|block| {
            block.stmts.iter().any(|stmt| is_str_eq_assign(stmt))
        })
    })
}

fn is_str_eq_assign(stmt: &edda_mir::Statement) -> bool {
    let StatementKind::Assign { rvalue, .. } = &stmt.kind else {
        return false;
    };
    let Rvalue {
        kind: RvalueKind::BinOp { op, prim, .. },
        ..
    } = rvalue
    else {
        return false;
    };
    matches!(op, BinOp::Eq | BinOp::Ne) && matches!(prim, MirPrim::Str)
}

/// Compile `std.text.string`'s `equals` function alone into a standalone
/// object at `target/edda/<triple>/<STR_EQ_SUPPORT_ARTIFACT>.o`. Returns
/// `None` when the stdlib root is unavailable or the scratch compile
/// fails for any reason.
pub(super) fn compile_str_eq_support(driver: &mut Driver) -> Option<PathBuf> {
    let module_path = ModulePath::new(Box::from([
        driver.interner.intern("std"),
        driver.interner.intern("text"),
        driver.interner.intern("string"),
    ]));
    let file = driver.stdlib.get(&module_path)?.clone();

    let cx = ResolveCx {
        layout: &driver.layout,
        deps: &driver.deps,
        stdlib: &driver.stdlib,
        interner: &driver.interner,
    };
    let mut scratch_diags = Diagnostics::new();
    let scratch_lint_cfg = LintConfig::new();
    let graph = build_source_graph(
        std::slice::from_ref(&file),
        &cx,
        &FsLoader,
        &driver.source_map,
        &mut scratch_diags,
        &scratch_lint_cfg,
    );
    let resolved = build_resolved_package(graph, &cx, &mut scratch_diags, &scratch_lint_cfg);
    let typed = edda_types::check_package(
        &resolved,
        &driver.interner,
        &driver.ty_interner,
        &scratch_lint_cfg,
        &driver.target,
        &mut scratch_diags,
    );

    let fn_inputs: Vec<FunctionInput<'_>> = typed
        .fn_bodies
        .iter()
        .filter_map(|tf| {
            let attrs = typed.attributes.get(&tf.binding).copied()?;
            let sym = attrs.abi_symbol?;
            if driver.interner.resolve(sym) != STR_EQ_SYMBOL {
                return None;
            }
            let sig = typed.ty_cx.sig(tf.binding)?;
            Some(FunctionInput {
                binding: tf.binding,
                name: tf.name,
                span: tf.span,
                sig,
                body: &tf.body,
                export_symbol: Some(sym),
                abi: None,
                qualified_name: None,
            })
        })
        .collect();
    if fn_inputs.is_empty() {
        return None;
    }

    let resolutions = resolved.resolutions();
    let method_resolutions: HashMap<Span, edda_resolve::BindingId> =
        typed.method_resolutions.iter().map(|(s, b)| (*s, *b)).collect();
    let intrinsic_calls: HashMap<Span, edda_types::IntrinsicKind> =
        typed.intrinsic_calls.iter().map(|(s, k)| (*s, *k)).collect();
    let pointer_width_bytes = edda_compile::pointer_width(driver.target.triple().arch()) / 8;
    let eq_comparators = HashMap::new();
    let debug_formatters = HashMap::new();

    let input = LoweringInput {
        interner: &driver.interner,
        ty_interner: &driver.ty_interner,
        resolutions,
        method_resolutions: &method_resolutions,
        intrinsic_calls: &intrinsic_calls,
        type_decls: &[],
        functions: &fn_inputs,
        externs: &[],
        consts: &[],
        fmt_bindings: FmtBindings::default(),
        alloc_fmt_bindings: AllocFmtBindings::default(),
        alloc_error: None,
        pointer_width_bytes,
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
    let program = lower_mir(input).ok()?;

    let out_dir = driver
        .package_root
        .join("target")
        .join("edda")
        .join(driver.target.triple().to_string());
    std::fs::create_dir_all(&out_dir).ok()?;
    let obj_path = out_dir.join(format!("{STR_EQ_SUPPORT_ARTIFACT}.o"));
    let emitter = Emitter::new();
    emitter
        .compile_program_to_object(
            STR_EQ_SUPPORT_ARTIFACT,
            &driver.target,
            &program,
            &driver.interner,
            &obj_path,
            false,
        )
        .ok()?;
    Some(obj_path)
}
