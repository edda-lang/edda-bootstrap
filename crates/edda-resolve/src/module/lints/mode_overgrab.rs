//! The `mode_overgrab` lint: `mutable`/`take` params never mutated/consumed.

use std::collections::{HashMap, HashSet};

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Symbol;
use edda_syntax::ast::visit::{Visitor, walk_expr, walk_stmt};
use edda_syntax::ast::{
    CallMode, Expr, ExprKind, FnBody, ItemKind, Param, ParamMode, Stmt, StmtKind, Type,
    TypeDeclKind, TypeKind,
};

use crate::{BindingKind, Resolved, ResolvedPackage};
use crate::resolve::ResolveCx;
use super::expr_root_path_segment;

/// Emit `mode_overgrab` for every parameter on a non-`public` function
/// declared with `mutable` or `take` mode whose body shows no evidence
/// of mutation (for `mutable`) or consumption (for `take`).
///
/// Public functions are excluded — their parameter modes are part of
/// the API contract and may legitimately reserve a stronger mode for
/// future use.
///
/// Mutation evidence (`mutable`):
///   - `param = ...`, `param.field = ...`, `param[i] = ...`, etc.
///   - `mutable param` passed at a call site
///
/// Consumption evidence (`take`):
///   - explicit `take param` at a call site
///   - `return param` (whole value moved out)
///   - `match param { ... }` (destructuring consumption)
///   - struct-literal field whose value is `param` (implicit take)
///   - spawn-argument `take` binding initialised from `param`
///   - every field of `param`'s declared record type is projected out
///     via `param.field` somewhere in the body
pub fn emit_mode_overgrab_lints(
    package: &ResolvedPackage,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    let severity = lint_cfg.effective(DiagnosticClass::ModeOvergrab);
    for module in &package.modules {
        let entry = package.graph.module(module.id);
        for item in &entry.ast.items {
            let ItemKind::Function(fn_decl) = &item.kind else {
                continue;
            };
            if matches!(fn_decl.visibility, edda_syntax::ast::Visibility::Public) {
                continue;
            }
            let FnBody::Block(body_block) = &fn_decl.body else {
                continue;
            };
            let candidate_params: Vec<&edda_syntax::ast::Param> = fn_decl
                .params
                .iter()
                .filter(|p| matches!(p.mode, ParamMode::Mutable | ParamMode::Take))
                .collect();
            if candidate_params.is_empty() {
                continue;
            }
            let mut state = ModeOvergrabState {
                mutated: HashSet::new(),
                consumed: HashSet::new(),
                field_reads: HashMap::new(),
            };
            state.visit_block(body_block);
            for param in candidate_params {
                let used_correctly = match param.mode {
                    ParamMode::Mutable => state.mutated.contains(&param.name.name),
                    ParamMode::Take => {
                        state.consumed.contains(&param.name.name)
                            || all_fields_drained(package, param, &state)
                    }
                    _ => true,
                };
                if used_correctly {
                    continue;
                }
                // Parser-recovery DUMMY name → skip the lint rather than render
                // a misleading diagnostic against an unnamed parameter.
                let Some(name_text) = cx.interner.try_resolve(param.name.name) else {
                    continue;
                };
                let mode_word = match param.mode {
                    ParamMode::Mutable => "mutable",
                    ParamMode::Take => "take",
                    _ => unreachable!(),
                };
                let suggest = match param.mode {
                    ParamMode::Mutable => "drop the `mutable` prefix",
                    ParamMode::Take => "drop the `take` prefix",
                    _ => unreachable!(),
                };
                let diag = Diagnostic::new(
                    DiagnosticClass::ModeOvergrab,
                    severity,
                    param.span,
                    format!(
                        "parameter `{name_text}: {mode_word} ...` is never {} in the body",
                        match param.mode {
                            ParamMode::Mutable => "mutated",
                            ParamMode::Take => "consumed",
                            _ => unreachable!(),
                        },
                    ),
                )
                .with_note(format!("{suggest} — the default `let` mode is sufficient"));
                diags.push(diag);
            }
        }
    }
}

/// Whether every field of `param`'s declared record type was read via
/// a `param.field` path expression somewhere in the body — the
/// AST-level approximation of the type checker's per-field
/// consumption rule for `take`-mode parameters.
fn all_fields_drained(package: &ResolvedPackage, param: &Param, state: &ModeOvergrabState) -> bool {
    let Some(field_names) = record_field_names(package, &param.ty) else {
        return false;
    };
    if field_names.is_empty() {
        return false;
    }
    let Some(accessed) = state.field_reads.get(&param.name.name) else {
        return false;
    };
    field_names.iter().all(|f| accessed.contains(f))
}

/// Resolve `ty` to its declared field names when it names an
/// in-workspace record (`type ... { field: T ... }`) `TypeDecl`.
/// `ty`'s Path was resolved by the intra-function pass (`walk_type`) to
/// the `TypeDecl` binding, whose owning module's AST carries the field list.
fn record_field_names(package: &ResolvedPackage, ty: &Type) -> Option<Vec<Symbol>> {
    let TypeKind::Path(path) = &ty.kind else {
        return None;
    };
    let Resolved::Binding(binding_id) = package.resolutions.lookup_path(path.span)? else {
        return None;
    };
    let entry = package.binding(binding_id);
    if entry.kind != BindingKind::TypeDecl {
        return None;
    }
    let module_entry = package.graph.module(entry.module);
    for item in &module_entry.ast.items {
        let ItemKind::TypeDecl(decl) = &item.kind else {
            continue;
        };
        if decl.name.name != entry.name {
            continue;
        }
        let TypeDeclKind::Product { fields } = &decl.kind else {
            return None;
        };
        return Some(fields.iter().map(|f| f.name.name).collect());
    }
    None
}

/// Per-function-body visitor that collects names mutated through an
/// assignment LHS or `mutable`-mode call argument, names consumed via
/// `take`, `return`, `match`, struct-literal field, or spawn arg, and
/// the per-root set of field names read through a `root.field` path
/// expression (`declarations.md` §312 — the expression parser folds a
/// bare `.field` chain into one multi-segment `Path`, not a `Field`
/// node; see [`expr_root_path_segment`]'s invariants).
struct ModeOvergrabState {
    mutated: HashSet<Symbol>,
    consumed: HashSet<Symbol>,
    field_reads: HashMap<Symbol, HashSet<Symbol>>,
}

impl<'ast> Visitor<'ast> for ModeOvergrabState {
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        if let StmtKind::Assign { target, rhs, .. } = &stmt.kind {
            if let Some(root) = expr_root_path_segment(target) {
                self.mutated.insert(root);
            }
            self.visit_expr(target);
            self.visit_expr(rhs);
            return;
        }
        walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        match &expr.kind {
            ExprKind::Path(p) if p.segments.len() >= 2 => {
                self.field_reads
                    .entry(p.segments[0].name)
                    .or_default()
                    .insert(p.segments[1].name);
            }
            ExprKind::Call { args, .. } | ExprKind::MethodCall { args, .. } => {
                for arg in args {
                    match arg.mode {
                        Some(CallMode::Mutable) => {
                            // `mutable buf[lo..hi]` / `mutable buf.field` pass-through
                            // mutates the root: slice/field mode inherits from parent.
                            if let Some(name) = expr_root_path_segment(&arg.expr) {
                                self.mutated.insert(name);
                            }
                        }
                        Some(CallMode::Take) => {
                            if let Some(name) = single_segment_path_name(&arg.expr) {
                                self.consumed.insert(name);
                            }
                        }
                        _ => {}
                    }
                }
            }
            ExprKind::Match { scrutinee, .. } => {
                if let Some(name) = single_segment_path_name(scrutinee) {
                    self.consumed.insert(name);
                }
            }
            ExprKind::Return(Some(inner)) => {
                if let Some(name) = single_segment_path_name(inner) {
                    self.consumed.insert(name);
                }
            }
            ExprKind::StructLit { fields, .. } => {
                for field in fields {
                    if let Some(name) = single_segment_path_name(&field.value) {
                        self.consumed.insert(name);
                    }
                }
            }
            ExprKind::Spawn(spawn) => {
                for arg in &spawn.args {
                    if let Some(name) = single_segment_path_name(&arg.init) {
                        self.consumed.insert(name);
                    }
                }
            }
            _ => {}
        }
        walk_expr(self, expr);
    }
}

fn single_segment_path_name(expr: &Expr) -> Option<Symbol> {
    if let ExprKind::Path(p) = &expr.kind {
        if p.segments.len() == 1 {
            return Some(p.segments[0].name);
        }
    }
    None
}
