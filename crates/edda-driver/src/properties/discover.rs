//! Property-target discovery and refinement-predicate rendering.
//!
//! Walks the resolved AST for refinement-bearing functions, classifies
//! each parameter's generator strategy, and renders each `ensures`
//! predicate into the marker-wrapped form the runner synthesiser
//! projects onto concrete call-site values.

use edda_intern::{Interner, Symbol};
use edda_resolve::ResolvedPackage;
use edda_syntax::ast::{
    BinOp, EffectMember, EffectRow, Expr, ExprKind, FnBody, FnDecl, ItemKind, Literal,
    RefinementClause, RefinementKind, Visibility,
};

use crate::properties::analyse::{IntPrim, analyse_param};
use crate::properties::strategy::Strategy;

/// A discovered property test target — a function with at least one
/// `requires`/`ensures` clause, or an `@property` attribute, that the
/// runner will exercise.
#[derive(Clone, Debug)]
pub struct PropertyTarget {
    /// Canonical dotted module path for the import (e.g.
    /// `my_pkg.factorial`'s module would be `my_pkg`).
    pub module_dot_path: String,
    /// Function name as written at the source.
    pub name: String,
    /// Source-rendered per-parameter strategy decisions. Empty when
    /// the analyser produced no usable input set for the function;
    /// the runner skips those targets.
    pub strategies: Vec<Strategy>,
    /// Per-parameter source-name. Used to render call arguments and
    /// the `result` substitution in `ensures` predicates.
    pub param_names: Vec<String>,
    /// Source-rendered `ensures` predicate (`result` substituted with
    /// the bound variable name at synthesis time). When no `ensures`
    /// clause exists this is `None` and the runner only checks that
    /// the call returns without panicking.
    pub ensures_predicates: Vec<String>,
}

/// Walk `package` collecting every function with at least one
/// `requires`/`ensures` clause, or an `@property` attribute, as a
/// [`PropertyTarget`] the runner can synthesise calls for.
pub fn discover_targets(package: &ResolvedPackage, interner: &Interner) -> Vec<PropertyTarget> {
    let mut out = Vec::new();
    for module in package.modules() {
        let module_entry = package.module_entry(module.id);
        let module_dot_path = module_entry
            .canonical_path
            .display(interner)
            .to_string();
        for item in &module_entry.ast.items {
            let ItemKind::Function(fn_decl) = &item.kind else {
                continue;
            };
            if fn_decl.visibility != Visibility::Public {
                continue;
            }
            if !matches!(fn_decl.body, FnBody::Block(_)) {
                continue;
            }
            if !effects_callable_from_harness(fn_decl.effects.as_ref(), interner) {
                continue;
            }
            // Qualifies as a property target when it carries at least
            // one Requires or Ensures clause, or when it is
            // `@property`-attributed — the latter covers a
            // `bool`-returning function whose own return value is the
            // assertion, with no explicit `ensures` clause at all.
            let has_refinement = fn_decl
                .refinements
                .iter()
                .any(|c| matches!(c.kind, RefinementKind::Requires | RefinementKind::Ensures));
            let has_property_attr = item
                .attributes
                .iter()
                .any(|a| interner.resolve(a.name.name) == "property");
            if !has_refinement && !has_property_attr {
                continue;
            }
            let target = build_target(fn_decl, &module_dot_path, interner, has_property_attr);
            out.push(target);
        }
    }
    out
}

fn build_target(
    fn_decl: &FnDecl,
    module_dot_path: &str,
    interner: &Interner,
    is_property_attr: bool,
) -> PropertyTarget {
    let name = interner.resolve(fn_decl.name.name).to_string();
    let mut strategies = Vec::with_capacity(fn_decl.params.len());
    let mut param_names = Vec::with_capacity(fn_decl.params.len());
    let requires_refs: Vec<&RefinementClause> = fn_decl
        .refinements
        .iter()
        .filter(|c| c.kind == RefinementKind::Requires)
        .collect();
    let mut any_unanalysable = false;
    let mut param_syms: Vec<Symbol> = Vec::with_capacity(fn_decl.params.len());
    for param in &fn_decl.params {
        let (int_prim, is_bool, inline_pred) = classify_param_ty(&param.ty, interner);
        let strategy = analyse_param(
            param.name.name,
            int_prim,
            is_bool,
            &requires_refs,
            inline_pred,
        );
        if matches!(strategy, Strategy::Unanalyzable) {
            any_unanalysable = true;
        }
        param_names.push(
            interner
                .try_resolve(param.name.name)
                .unwrap_or("<missing>")
                .to_string(),
        );
        param_syms.push(param.name.name);
        strategies.push(strategy);
    }
    let strategies = if any_unanalysable {
        Vec::new()
    } else {
        strategies
    };
    // Render ensures predicates with marker substitution for `result`
    // (postcondition return-value binder) and every parameter name.
    // Synthesis-time string replacement then projects the markers onto
    // the concrete `r_<idx>` binding and the per-tuple argument source.
    // Markers wrap a control character (`\u{1}`) that cannot occur in
    // valid Edda source so the replacement is collision-free. A clause
    // that does not render (see `render_predicate_inner`) is dropped
    // rather than synthesising an invalid call site from it.
    let mut ensures_predicates: Vec<String> = fn_decl
        .refinements
        .iter()
        .filter(|c| c.kind == RefinementKind::Ensures)
        .filter_map(|c| render_predicate_with_markers(&c.pred, interner, &param_syms))
        .collect();
    // A `@property`-attributed function with no explicit `ensures`
    // clause and a `bool` return uses its own return value as the
    // assertion — the marker-only predicate renders to `if !r_N {
    // panic ... }` once synthesis substitutes the `result` marker.
    if ensures_predicates.is_empty() && is_property_attr && returns_bool(fn_decl, interner) {
        ensures_predicates.push(result_marker());
    }
    PropertyTarget {
        module_dot_path: module_dot_path.to_string(),
        name,
        strategies,
        param_names,
        ensures_predicates,
    }
}

//   (`TypeKind::Refined`) is unwrapped one level before classifying
//   the base type; `pred` is returned alongside so the caller can
//   fold it into the same bound-extraction path top-level `requires`
//   clauses use — the locked grammar admits only one refinement layer
//   on a parameter type, so no further unwrapping is attempted
/// Recognise a parameter's type as a specific integer primitive or
/// `bool`, unwrapping one `TypeKind::Refined` layer first. Composite
/// shapes (tuples, slices, nominal, function types) return
/// `(None, false, _)` so the analyser maps them to
/// `Strategy::Unanalyzable`. The returned [`IntPrim`] carries the
/// width-plus-signedness needed to clamp generated values to the
/// declared parameter type. The third element is the inline
/// refinement predicate, when the parameter's type carried one.
fn classify_param_ty<'a>(
    ty: &'a edda_syntax::ast::Type,
    interner: &Interner,
) -> (Option<IntPrim>, bool, Option<&'a Expr>) {
    use edda_syntax::ast::TypeKind;
    let (base_ty, inline_pred) = match &ty.kind {
        TypeKind::Refined { base, pred } => (base.as_ref(), Some(pred)),
        _ => (ty, None),
    };
    let TypeKind::Path(path) = &base_ty.kind else {
        return (None, false, inline_pred);
    };
    if path.segments.len() != 1 {
        return (None, false, inline_pred);
    }
    let name = interner.resolve(path.segments[0].name);
    let (int_prim, is_bool) = match name {
        "i8" => (Some(IntPrim::I8), false),
        "i16" => (Some(IntPrim::I16), false),
        "i32" => (Some(IntPrim::I32), false),
        "i64" => (Some(IntPrim::I64), false),
        "i128" => (Some(IntPrim::I128), false),
        "isize" => (Some(IntPrim::ISize), false),
        "u8" => (Some(IntPrim::U8), false),
        "u16" => (Some(IntPrim::U16), false),
        "u32" => (Some(IntPrim::U32), false),
        "u64" => (Some(IntPrim::U64), false),
        "u128" => (Some(IntPrim::U128), false),
        "usize" => (Some(IntPrim::USize), false),
        "bool" => (None, true),
        _ => (None, false),
    };
    (int_prim, is_bool, inline_pred)
}

/// True when `fn_decl`'s return type is the single-segment `bool`
/// path. Used to decide whether an `@property`-attributed function
/// with no `ensures` clause can use its own return value as the
/// synthesised assertion.
fn returns_bool(fn_decl: &FnDecl, interner: &Interner) -> bool {
    use edda_syntax::ast::TypeKind;
    let Some(ty) = fn_decl.return_ty.as_ref() else {
        return false;
    };
    let TypeKind::Path(path) = &ty.kind else {
        return false;
    };
    path.segments.len() == 1 && interner.resolve(path.segments[0].name) == "bool"
}

//   explicit empty `with {}` both count as callable — both mean "pure"
//   per CLAUDE.md "Empty effect row may be written as `with {}` or
//   omitted entirely"
/// True when `effects` is empty or contains solely the bare `panic`
/// member — the only effect-row shape the synthesised runner's
/// fixed `with {panic}` entry point (see
/// [`crate::properties::synth::synthesize_runner_source`]) can call
/// without an unheld capability or an undeclared `err:`/`yield:`/
/// graded/`cancellation`/`divergence`/`nondet` effect.
fn effects_callable_from_harness(effects: Option<&EffectRow>, interner: &Interner) -> bool {
    let Some(row) = effects else {
        return true;
    };
    row.members.iter().all(|member| match member {
        EffectMember::Capability(ident) => interner.resolve(ident.name) == "panic",
        EffectMember::Named { .. } | EffectMember::Spread(_) | EffectMember::Graded { .. } => {
            false
        }
    })
}

//   appears in `param_syms` are emitted as the marker token
//   `\u{1}P<idx>\u{1}`; the `result` keyword path is emitted as
//   `\u{1}R\u{1}`. All other forms render identically to
//   [`render_predicate`]
//   Edda source — the lexer does not admit control chars in
//   identifiers — so the markers cannot collide with user code at
//   synthesis-time string replacement
/// Render a refinement predicate with substitution markers around the
/// `result` postcondition binder and every parameter-name path. The
/// synthesised runner replaces the markers with the call-site values
/// per tuple. Returns `None` when the predicate (or a subexpression of
/// it) is not renderable — see [`render_predicate_inner`].
fn render_predicate_with_markers(
    expr: &Expr,
    interner: &Interner,
    param_syms: &[Symbol],
) -> Option<String> {
    render_predicate_inner(expr, interner, param_syms)
}

//   Field-projection (`receiver.field`, recursing through the
//   receiver so a marker-substituted `result` still projects, e.g.
//   `result.line`); every other `ExprKind` (Call, MethodCall,
//   TupleIndex, comptime-indexed field access, control flow, ...)
//   returns `None` — there is no valid Edda rendering for it, and a
//   composite expression (Binary/Unary/Field) whose child is
//   unrenderable also returns `None` rather than splicing a partial
//   placeholder into the rendered text; `build_target` drops any
//   `ensures` clause that renders to `None` instead of synthesising a
//   call site from it
/// Shared rendering core. When `param_syms` is non-empty the renderer
/// emits a marker token for any single-segment Path whose interned
/// symbol matches a parameter, plus a marker for the `result` binder.
/// Pass `&[]` to render without substitution markers.
fn render_predicate_inner(
    expr: &Expr,
    interner: &Interner,
    param_syms: &[Symbol],
) -> Option<String> {
    match &expr.kind {
        ExprKind::Binary { op, lhs, rhs } => {
            let op_str = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Mod => "%",
                BinOp::WrapAdd => "+%",
                BinOp::WrapSub => "-%",
                BinOp::WrapMul => "*%",
                BinOp::CheckAdd => "+?",
                BinOp::CheckSub => "-?",
                BinOp::CheckMul => "*?",
                BinOp::CheckMod => "%?",
                BinOp::SatAdd => "+|",
                BinOp::SatSub => "-|",
                BinOp::SatMul => "*|",
                BinOp::Eq => "==",
                BinOp::Ne => "!=",
                BinOp::Lt => "<",
                BinOp::Le => "<=",
                BinOp::Gt => ">",
                BinOp::Ge => ">=",
                BinOp::And => "&&",
                BinOp::Or => "||",
                BinOp::BitAnd => "&",
                BinOp::BitOr => "|",
                BinOp::BitXor => "^",
                BinOp::Shl => "<<",
                BinOp::Shr => ">>",
            };
            Some(format!(
                "({} {} {})",
                render_predicate_inner(lhs, interner, param_syms)?,
                op_str,
                render_predicate_inner(rhs, interner, param_syms)?,
            ))
        }
        ExprKind::Unary {
            op: edda_syntax::ast::UnOp::Neg,
            expr: inner,
        } => Some(format!(
            "(- {})",
            render_predicate_inner(inner, interner, param_syms)?
        )),
        ExprKind::Path(path) => {
            if !param_syms.is_empty() && path.segments.len() == 1 {
                let head = path.segments[0].name;
                if let Some(idx) = param_syms.iter().position(|&p| p == head) {
                    return Some(param_marker(idx));
                }
                if interner.resolve(head) == "result" {
                    return Some(result_marker());
                }
            }
            Some(
                path.segments
                    .iter()
                    .map(|s| interner.resolve(s.name))
                    .collect::<Vec<_>>()
                    .join("."),
            )
        }
        ExprKind::Literal(Literal::Int { value, .. }) => Some(value.to_string()),
        ExprKind::Literal(Literal::Bool(b)) => Some(b.to_string()),
        ExprKind::Field { receiver, name } => Some(format!(
            "{}.{}",
            render_predicate_inner(receiver, interner, param_syms)?,
            interner.resolve(name.name),
        )),
        _ => None,
    }
}

/// Marker placeholder for the `result` postcondition binder. Wrapped
/// in `\u{1}` so synthesis-time string replacement cannot collide
/// with any token that might appear in valid Edda source.
pub(super) fn result_marker() -> String {
    "\u{1}R\u{1}".to_string()
}

/// Marker placeholder for the i-th parameter. See [`result_marker`]
/// for the collision-free invariant.
pub(super) fn param_marker(idx: usize) -> String {
    format!("\u{1}P{idx}\u{1}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_span::Span;
    use edda_syntax::ast::{Block, Path, ReturnMode};
    use edda_syntax::IntBase;

    fn ident(interner: &Interner, name: &str) -> edda_syntax::ast::Ident {
        edda_syntax::ast::Ident {
            name: interner.intern(name),
            span: Span::DUMMY,
        }
    }

    fn path_expr(interner: &Interner, name: &str) -> Expr {
        Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(Path {
                segments: vec![ident(interner, name)],
                span: Span::DUMMY,
            }),
        }
    }

    fn path_ty(interner: &Interner, name: &str) -> edda_syntax::ast::Type {
        edda_syntax::ast::Type {
            span: Span::DUMMY,
            kind: edda_syntax::ast::TypeKind::Path(Path {
                segments: vec![ident(interner, name)],
                span: Span::DUMMY,
            }),
        }
    }

    fn lit(value: u128) -> Expr {
        Expr {
            span: Span::DUMMY,
            kind: ExprKind::Literal(Literal::Int {
                value,
                base: IntBase::Dec,
            }),
        }
    }

    fn empty_body() -> FnBody {
        FnBody::Block(Block {
            span: Span::DUMMY,
            stmts: vec![],
            trailing: None,
        })
    }

    // `round_trip_preserves_header`'s exact shape:
    // `@property()`, a `bool` return, no top-level `requires`/
    // `ensures`, and one param carrying only an inline
    // `where` refinement (`kind_tag_in: u8 where kind_tag_in < 5`).
    #[test]
    fn property_attr_with_inline_refined_param_and_bool_return() {
        let interner = Interner::new();
        let param = edda_syntax::ast::Param {
            span: Span::DUMMY,
            name: ident(&interner, "kind_tag_in"),
            mode: edda_syntax::ast::ParamMode::Default,
            ty: edda_syntax::ast::Type {
                span: Span::DUMMY,
                kind: edda_syntax::ast::TypeKind::Refined {
                    base: Box::new(path_ty(&interner, "u8")),
                    pred: Expr {
                        span: Span::DUMMY,
                        kind: ExprKind::Binary {
                            op: BinOp::Lt,
                            lhs: Box::new(path_expr(&interner, "kind_tag_in")),
                            rhs: Box::new(lit(5)),
                        },
                    },
                },
            },
        };
        let fn_decl = FnDecl {
            span: Span::DUMMY,
            stability: None,
            visibility: Visibility::Public,
            refinement_stable: false,
            name: ident(&interner, "round_trip_preserves_header"),
            outbound_generics: vec![],
            generics: vec![],
            params: vec![param],
            return_ty: Some(path_ty(&interner, "bool")),
            return_mode: ReturnMode::ByValue,
            effects: None,
            refinements: vec![],
            body: empty_body(),
        };

        let target = build_target(&fn_decl, "cache.blob.header", &interner, true);

        assert_eq!(
            target.strategies,
            vec![Strategy::IntRange { lo: 0, hi: 4 }],
            "inline where-refined param must get a real strategy, not Unanalyzable"
        );
        assert_eq!(
            target.ensures_predicates,
            vec![result_marker()],
            "no-ensures @property bool-return function asserts its own return value"
        );
    }

    // The synthesised runner's entry point is fixed at
    // `with {panic}` and holds no capabilities; a target declaring a
    // capability, `err:`, or any other effect the harness cannot
    // satisfy must be excluded from discovery, not synthesised into a
    // call site the typechecker then rejects with
    // `effect_row_mismatch`.
    #[test]
    fn effects_callable_from_harness_accepts_none_and_bare_panic_only() {
        let interner = Interner::new();
        assert!(effects_callable_from_harness(None, &interner));

        let panic_only = EffectRow {
            span: Span::DUMMY,
            members: vec![EffectMember::Capability(ident(&interner, "panic"))],
        };
        assert!(effects_callable_from_harness(Some(&panic_only), &interner));

        let empty = EffectRow {
            span: Span::DUMMY,
            members: vec![],
        };
        assert!(effects_callable_from_harness(Some(&empty), &interner));
    }

    #[test]
    fn effects_callable_from_harness_rejects_capability_and_err() {
        let interner = Interner::new();

        let needs_allocator = EffectRow {
            span: Span::DUMMY,
            members: vec![EffectMember::Capability(ident(&interner, "allocator"))],
        };
        assert!(!effects_callable_from_harness(
            Some(&needs_allocator),
            &interner
        ));

        let fallible = EffectRow {
            span: Span::DUMMY,
            members: vec![EffectMember::Named {
                name: ident(&interner, "err"),
                ty: path_ty(&interner, "PageError"),
            }],
        };
        assert!(!effects_callable_from_harness(Some(&fallible), &interner));
    }

    // An `ensures` clause built from an unrenderable
    // subexpression (here a `Call`) must be dropped, not spliced into
    // the predicate text as `(* unrenderable predicate *)`.
    #[test]
    fn unrenderable_ensures_clause_is_dropped_not_synthesised() {
        let interner = Interner::new();
        let call_expr = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Call {
                callee: Box::new(path_expr(&interner, "helper")),
                args: vec![],
            },
        };
        let fn_decl = FnDecl {
            span: Span::DUMMY,
            stability: None,
            visibility: Visibility::Public,
            refinement_stable: false,
            name: ident(&interner, "f"),
            outbound_generics: vec![],
            generics: vec![],
            params: vec![],
            return_ty: Some(path_ty(&interner, "bool")),
            return_mode: ReturnMode::ByValue,
            effects: None,
            refinements: vec![RefinementClause {
                span: Span::DUMMY,
                kind: RefinementKind::Ensures,
                pred: call_expr,
            }],
            body: empty_body(),
        };

        let target = build_target(&fn_decl, "m", &interner, false);

        assert!(
            target.ensures_predicates.is_empty(),
            "an unrenderable ensures clause must be dropped, not rendered as invalid source"
        );
    }

    // A Binary predicate whose lhs/rhs is unrenderable
    // must drop the whole clause rather than embed a partial
    // placeholder inside otherwise-valid-looking parentheses.
    #[test]
    fn binary_predicate_with_unrenderable_child_is_dropped() {
        let interner = Interner::new();
        let call_expr = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Call {
                callee: Box::new(path_expr(&interner, "helper")),
                args: vec![],
            },
        };
        let pred = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Binary {
                op: BinOp::Ge,
                lhs: Box::new(path_expr(&interner, "result")),
                rhs: Box::new(call_expr),
            },
        };

        assert_eq!(render_predicate_with_markers(&pred, &interner, &[]), None);
    }

    #[test]
    fn no_property_attr_and_no_refinement_still_yields_no_ensures() {
        let interner = Interner::new();
        let fn_decl = FnDecl {
            span: Span::DUMMY,
            stability: None,
            visibility: Visibility::Public,
            refinement_stable: false,
            name: ident(&interner, "f"),
            outbound_generics: vec![],
            generics: vec![],
            params: vec![],
            return_ty: Some(path_ty(&interner, "bool")),
            return_mode: ReturnMode::ByValue,
            effects: None,
            refinements: vec![],
            body: empty_body(),
        };

        let target = build_target(&fn_decl, "m", &interner, false);

        assert!(
            target.ensures_predicates.is_empty(),
            "a bool-returning function is only auto-asserted when @property-attributed"
        );
    }
}
