//! Sort projection and the per-function [`PredicateEnv`] used by every
//! refinement-discharge path.
//!
//! `sort_of_ty` maps a typecheck-side [`TyId`] to its
//! [`edda_refine::Sort`] for predicate-fragment lifting; `FnRefineEnv`
//! is the [`PredicateEnv`] implementation that backs path / literal /
//! identifier resolution during a single function's discharge. Both
//! pieces are consumed by [`super::clauses`], [`super::termination`],
//! and [`super::coherence_preservation`].

use std::cell::RefCell;
use std::collections::HashMap;

use smol_str::SmolStr;

use edda_intern::{Interner, Symbol};
use edda_refine::{
    FieldRef, IntSort, IntWidth, Predicate, PredicateEnv, RecordSchema, Schema, Sort,
    lift_predicate,
};
use edda_resolve::{BindingId, BindingKind, ModuleId, Resolved, ResolvedPackage};
use edda_span::Span;
use edda_syntax::ast::visit::{Visitor, walk_expr};
use edda_syntax::ast::{self, Expr, ExprKind, Ident, Literal, Path};

use crate::cx::{TyCx, TypeDeclShape};
use crate::prim::Primitive;
use crate::ty::{TyId, TyInterner, TyKind};

//            the two functions must change together
//          FnRefineEnv::lookup_field can recover the TyCx lookup key from a
//          bare Sort::Record it's handed back by the lifter
/// Build the [`edda_refine::RecordRef`] naming a product [`TypeDeclInfo`].
/// The name packs the owning [`BindingId`] (module index + intra-module
/// index) rather than the source-level type name, so two same-named
/// records declared in different modules never collide — [`Schema`]
/// declares one Z3 datatype per distinct name. `record_ref_binding_id`
/// is the inverse.
fn record_ref_for(id: BindingId) -> edda_refine::RecordRef {
    edda_refine::RecordRef::new(format!("record#{}.{}", id.module.as_u32(), id.index))
}

/// Inverse of `record_ref_for` — recover the `BindingId` a `RecordRef`
/// was minted from. Returns `None` for a malformed name (never produced
/// by this module, so only reachable if a `RecordRef` from a different
/// origin is fed back through `lookup_field`).
fn record_ref_binding_id(record: &edda_refine::RecordRef) -> Option<BindingId> {
    let tail = record.name().strip_prefix("record#")?;
    let (module_str, index_str) = tail.split_once('.')?;
    let module = module_str.parse::<u32>().ok()?;
    let index = index_str.parse::<u32>().ok()?;
    Some(BindingId::new(ModuleId::new(module), index))
}

/// Project a `TyId` to a `Sort` for use in predicate variable bindings.
///
/// Returns `None` for types that have no predicate-fragment representation
/// (sum, capability, tuple, error). Nominal *sum* sort projection is
/// deferred to a later wave; nominal *product* (record) types project to
/// `Sort::Record` so `.field` access on a struct-typed value enters the
/// predicate fragment.
pub(crate) fn sort_of_ty(id: TyId, ty_interner: &TyInterner, ty_cx: &TyCx) -> Option<Sort> {
    match ty_interner.kind(id) {
        TyKind::Primitive(p) => prim_to_sort(*p),
        TyKind::Slice(elem_id) => {
            let elem = sort_of_ty(*elem_id, ty_interner, ty_cx)?;
            Some(Sort::slice(elem))
        }
        TyKind::Nominal(binding_id) => {
            let info = ty_cx.type_decl(*binding_id)?;
            match &info.kind {
                TypeDeclShape::Product { .. } => Some(Sort::Record(record_ref_for(*binding_id))),
                TypeDeclShape::Sum { .. } => None,
            }
        }
        _ => None,
    }
}

//            `lookup_field` correctly report `UnknownField` for it —
//            `RecordSchema`'s positional accessor index is assigned from
//            this filtered list, not the source-level field position, so
//            omission never desyncs the Z3 datatype's accessor order
//          Z3Backend::with_schema, mirroring build_ty_cx's TyCx walk
/// Build the [`Schema`] every SMT discharge in this module needs to
/// resolve `.field` access — one [`RecordSchema`] per product `type`
/// declaration reachable from `ty_cx`, keyed by [`record_ref_for`]'s
/// naming so `Sort::Record` values produced by `sort_of_ty` resolve back
/// to the right Z3 datatype. Fields whose type has no `Sort` projection
/// (e.g. `String`) are omitted from the schema; `lookup_field` already
/// returns `None` for those, matching the pre-existing `UnknownField`
/// behaviour for unsupported field types.
pub(crate) fn build_schema(ty_cx: &TyCx, ty_interner: &TyInterner, interner: &Interner) -> Schema {
    let mut schema = Schema::empty();
    for (binding_id, info) in ty_cx.iter_type_decls() {
        let TypeDeclShape::Product { .. } = &info.kind else {
            continue;
        };
        let name = record_ref_for(binding_id).name().to_string();
        let fields: Vec<(SmolStr, Sort)> = info
            .fields()
            .iter()
            .filter_map(|field| {
                let field_name = interner.try_resolve(field.name)?;
                let sort = sort_of_ty(field.ty, ty_interner, ty_cx)?;
                Some((SmolStr::new(field_name), sort))
            })
            .collect();
        schema = schema.with_record(RecordSchema::new(name, fields));
    }
    schema
}

/// Map a `Primitive` to its `Sort`. Returns `None` for float, string, unit,
/// never, and meta types, which are outside the required-decidable fragment.
/// Crate-visible for [`super::call_precondition`]'s `alloc_array`
/// element-sort projection.
pub(crate) fn prim_to_sort(p: Primitive) -> Option<Sort> {
    match p {
        Primitive::I8    => Some(Sort::Int(IntSort::sized(IntWidth::W8,    true))),
        Primitive::I16   => Some(Sort::Int(IntSort::sized(IntWidth::W16,   true))),
        Primitive::I32   => Some(Sort::Int(IntSort::sized(IntWidth::W32,   true))),
        Primitive::I64   => Some(Sort::Int(IntSort::sized(IntWidth::W64,   true))),
        Primitive::I128  => Some(Sort::Int(IntSort::sized(IntWidth::W128,  true))),
        Primitive::Isize => Some(Sort::Int(IntSort::sized(IntWidth::Isize, true))),
        Primitive::U8    => Some(Sort::Int(IntSort::sized(IntWidth::W8,    false))),
        Primitive::U16   => Some(Sort::Int(IntSort::sized(IntWidth::W16,   false))),
        Primitive::U32   => Some(Sort::Int(IntSort::sized(IntWidth::W32,   false))),
        Primitive::U64   => Some(Sort::Int(IntSort::sized(IntWidth::W64,   false))),
        Primitive::U128  => Some(Sort::Int(IntSort::sized(IntWidth::W128,  false))),
        Primitive::Usize => Some(Sort::Int(IntSort::sized(IntWidth::Usize, false))),
        Primitive::Bool  => Some(Sort::Bool),
        _                => None,
    }
}

//            the resolver's `BindingEntry::name` for `BindingKind::Param` entries
//          function-level refinement discharge in check.rs
/// `PredicateEnv` implementation backed by the function's parameter sorts,
/// the caller's `let`-bound local sorts, and a stack of quantifier-bound
/// locals pushed by the refine lifter.
///
/// Path resolution routes through the resolver's span-keyed `Resolutions`
/// map. Param bindings consult `param_sorts`; `Local` bindings consult, in
/// order, the LIFO `quantifier_bounds` stack, the transient `result_binding`
/// (the `ensures` `result` keyword), then `local_sorts`. Field projection
/// resolves against `ty_cx` (via `record_ref_binding_id` + `TyCx::type_decl`);
/// nominal *sum* sorts are not yet wired — a sum-sorted receiver has no
/// `Sort` at all (see `sort_of_ty`), so it never reaches `lookup_field`.
pub(crate) struct FnRefineEnv<'a> {
    pub(crate) package: &'a ResolvedPackage,
    pub(crate) param_sorts: &'a HashMap<Symbol, (SmolStr, Sort)>,
    pub(crate) local_sorts: &'a HashMap<Symbol, (SmolStr, Sort)>,
    pub(crate) interner: &'a Interner,
    pub(crate) ty_interner: &'a TyInterner,
    pub(crate) ty_cx: &'a TyCx,
    pub(crate) quantifier_bounds: RefCell<Vec<(Symbol, SmolStr, Sort)>>,
    pub(crate) result_binding: RefCell<Option<(Symbol, SmolStr, Sort)>>,
}

impl<'a> FnRefineEnv<'a> {
    /// Build an env over a function's parameter + local sorts. The
    /// quantifier-bound stack starts empty and `result_binding` unset;
    /// callers toggle the latter around an instantiated-`ensures` lift via
    /// [`FnRefineEnv::set_result_binding`].
    pub(crate) fn new(
        package: &'a ResolvedPackage,
        param_sorts: &'a HashMap<Symbol, (SmolStr, Sort)>,
        local_sorts: &'a HashMap<Symbol, (SmolStr, Sort)>,
        interner: &'a Interner,
        ty_interner: &'a TyInterner,
        ty_cx: &'a TyCx,
    ) -> Self {
        Self {
            package,
            param_sorts,
            local_sorts,
            interner,
            ty_interner,
            ty_cx,
            quantifier_bounds: RefCell::new(Vec::new()),
            result_binding: RefCell::new(None),
        }
    }

    /// Install (or clear) the `result` binding for the duration of an
    /// instantiated-callee-`ensures` lift. The symbol must be the interned
    /// `"result"` keyword; the name is the caller local the postcondition is
    /// instantiated onto so the lifted `Var` unifies with that local.
    pub(crate) fn set_result_binding(&self, binding: Option<(Symbol, SmolStr, Sort)>) {
        *self.result_binding.borrow_mut() = binding;
    }
}

impl PredicateEnv for FnRefineEnv<'_> {
    fn lookup_path(&self, span: Span) -> Option<(SmolStr, Sort)> {
        let resolved = self.package.resolutions().lookup_path(span)?;
        let Resolved::Binding(id) = resolved else { return None; };
        let entry = self.package.binding(id);
        match entry.kind {
            BindingKind::Param => {
                let (name, sort) = self.param_sorts.get(&entry.name)?;
                Some((name.clone(), sort.clone()))
            }
            BindingKind::Local => {
                // Walk the quantifier-bound stack top-down so the
                // innermost binder of a shadowed name wins.
                let bounds = self.quantifier_bounds.borrow();
                for (sym, name, sort) in bounds.iter().rev() {
                    if *sym == entry.name {
                        return Some((name.clone(), sort.clone()));
                    }
                }
                drop(bounds);
                // The `ensures` `result` keyword — when this env is lifting an
                // instantiated callee postcondition — stands for the caller
                // local the call's value was bound to.
                if let Some((sym, name, sort)) = self.result_binding.borrow().as_ref()
                    && *sym == entry.name
                {
                    return Some((name.clone(), sort.clone()));
                }
                // Caller `let`-bound locals (e.g. a `decreases` measure that
                // references a call-result local).
                if let Some((name, sort)) = self.local_sorts.get(&entry.name) {
                    return Some((name.clone(), sort.clone()));
                }
                None
            }
            _ => None,
        }
    }

    fn expr_sort(&self, expr: &Expr) -> Option<Sort> {
        match &expr.kind {
            ExprKind::Path(path) => self.lookup_path(path.span).map(|(_, s)| s),
            ExprKind::Literal(lit) => lit_sort(lit),
            ExprKind::Binary { lhs, .. } => self.expr_sort(lhs),
            ExprKind::Unary { expr: operand, .. } => self.expr_sort(operand),
            _ => None,
        }
    }

    /// Resolve `.field` access against `ty_cx`'s recorded product-type
    /// layout. `base_sort` must be `Sort::Record(record_ref)` — any other
    /// sort (including `Sort::Sum`, not yet wired) returns `None`, which
    /// the lifter surfaces as `LiftError::UnknownField`.
    fn lookup_field(&self, base_sort: &Sort, field: &Ident) -> Option<FieldRef> {
        let Sort::Record(record_ref) = base_sort else {
            return None;
        };
        let binding_id = record_ref_binding_id(record_ref)?;
        let info = self.ty_cx.type_decl(binding_id)?;
        let field_info = info.field(field.name)?;
        let field_sort = sort_of_ty(field_info.ty, self.ty_interner, self.ty_cx)?;
        let field_name = self.interner.try_resolve(field.name)?;
        Some(FieldRef::new(record_ref.clone(), field_name, field_sort))
    }

    fn type_sort(&self, ty: &ast::Type) -> Option<Sort> {
        ast_type_to_sort(ty, self.interner)
    }

    fn ident_name(&self, ident: &Ident) -> SmolStr {
        SmolStr::new(self.interner.resolve(ident.name))
    }

    fn push_quantifier_bound(&self, ident: &Ident, sort: Sort) {
        let name = SmolStr::new(self.interner.resolve(ident.name));
        self.quantifier_bounds
            .borrow_mut()
            .push((ident.name, name, sort));
    }

    fn pop_quantifier_bound(&self, ident: &Ident) {
        let mut bounds = self.quantifier_bounds.borrow_mut();
        // Pop the topmost entry — must match the ident we just pushed
        // (stack discipline maintained by the refine lifter).
        if let Some((sym, _, _)) = bounds.last() {
            if *sym == ident.name {
                bounds.pop();
                return;
            }
        }
        // Defensive: if the lifter's push/pop got out of sync somewhere,
        // we'd silently leak a bound here. The invariant on the lifter
        // side ensures this never happens in practice.
    }
}

/// Derive the predicate sort of an integer or boolean literal.
///
/// Integer literals default to `i64` (the LIA integer sort). Float, string,
/// and composite literals are outside the predicate fragment and return `None`.
fn lit_sort(lit: &Literal) -> Option<Sort> {
    match lit {
        Literal::Int { .. } => Some(Sort::Int(IntSort::sized(IntWidth::W64, true))),
        Literal::Bool(_)    => Some(Sort::Bool),
        _                   => None,
    }
}

/// Project a cast-target [`ast::Type`] to a refine [`Sort`]. Recognises the
/// primitive type spellings (`i64`, `usize`, …) and `[T]` slices; everything
/// else (nominal, tuple, function, refined, meta) is outside the predicate
/// fragment and yields `None` — the lifter then reports
/// `UnsupportedCastTarget`. Mirrors [`sort_of_ty`] but works off the
/// pre-typecheck AST so the termination / clause discharge can resolve
/// `x as T` measures without first lowering the annotation to a `TyId`.
fn ast_type_to_sort(ty: &ast::Type, interner: &Interner) -> Option<Sort> {
    match &ty.kind {
        ast::TypeKind::Path(path) => {
            if path.segments.len() != 1 {
                return None;
            }
            let name = interner.try_resolve(path.segments[0].name)?;
            prim_to_sort(Primitive::from_name(name)?)
        }
        ast::TypeKind::Slice(inner) => Some(Sort::slice(ast_type_to_sort(inner, interner)?)),
        _ => None,
    }
}

/// Collect the instantiated field-`where` facts implied by every
/// field-projection sub-expression of `expr` (e.g. `d.nanos` inside
/// `result == d.nanos`, or a substituted call-site clause's `Field`
/// node rooted at the actual argument).
///
/// Each fact is the field's own inline `where` predicate with every
/// sibling field name of the same record substituted by its own
/// projection off the same base — so a refinement that references a
/// sibling field (`len: usize where len <= items.len()`) instantiates
/// correctly, not just a self-reference. A multi-segment chain
/// (`a.b.c`) contributes one fact per level that carries a refinement.
/// Unliftable facts (e.g. a refinement referencing something outside
/// the predicate fragment, or a base expression this walk cannot sort)
/// are silently dropped, matching this module's existing policy for
/// every other refinement fold.
pub(crate) fn field_refinement_facts(expr: &Expr, env: &FnRefineEnv<'_>) -> Vec<Predicate> {
    let mut collector = FieldFactCollector { env, out: Vec::new() };
    collector.visit_expr(expr);
    collector.out
}

struct FieldFactCollector<'a, 'env> {
    env: &'env FnRefineEnv<'a>,
    out: Vec<Predicate>,
}

impl<'a, 'env, 'ast> Visitor<'ast> for FieldFactCollector<'a, 'env> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match &expr.kind {
            ExprKind::Path(path) => self.collect_path_facts(path),
            ExprKind::Field { receiver, name } => self.collect_field_facts(receiver, *name),
            _ => {}
        }
        walk_expr(self, expr);
    }
}

impl<'a, 'env> FieldFactCollector<'a, 'env> {
    fn collect_path_facts(&mut self, path: &Path) {
        let Some((_, sort)) = self.env.lookup_path(path.span) else {
            return;
        };
        let mut base_segments = vec![path.segments[0]];
        let mut sort = sort;
        for seg in &path.segments[1..] {
            let base = Expr {
                span: path.span,
                kind: ExprKind::Path(Path {
                    span: path.span,
                    segments: base_segments.clone(),
                }),
            };
            let Some(next_sort) = self.emit_field_fact_and_descend(&sort, &base, *seg) else {
                break;
            };
            base_segments.push(*seg);
            sort = next_sort;
        }
    }

    fn collect_field_facts(&mut self, receiver: &Expr, name: Ident) {
        let Ok(receiver_pred) = lift_predicate(receiver, self.env) else {
            return;
        };
        let sort = receiver_pred.sort();
        let _ = self.emit_field_fact_and_descend(&sort, receiver, name);
    }

    /// Look up `name` as a field of record `sort`; if it carries an
    /// inline `where`, instantiate it (substituting every sibling field
    /// name with its own projection off `base`) and lift it into
    /// `self.out`. Returns the field's own sort so a multi-segment walk
    /// can descend further, or `None` when `sort` isn't a known record
    /// / the field isn't found / its type has no predicate-fragment
    /// sort.
    fn emit_field_fact_and_descend(
        &mut self,
        sort: &Sort,
        base: &Expr,
        name: Ident,
    ) -> Option<Sort> {
        let Sort::Record(record_ref) = sort else {
            return None;
        };
        let binding_id = record_ref_binding_id(record_ref)?;
        let info = self.env.ty_cx.type_decl(binding_id)?;
        let field_info = info.field(name.name)?;

        if let Some(refinement_expr) = &field_info.refinement {
            let mut subst: HashMap<Symbol, Expr> = HashMap::new();
            for sibling in info.fields() {
                subst.insert(
                    sibling.name,
                    Expr {
                        span: base.span,
                        kind: ExprKind::Field {
                            receiver: Box::new(base.clone()),
                            name: Ident {
                                name: sibling.name,
                                span: name.span,
                            },
                        },
                    },
                );
            }
            let substituted = super::substitute_paths(refinement_expr, &subst, self.env.package);
            if let Ok(pred) = lift_predicate(&substituted, self.env) {
                self.out.push(pred);
            }
        }

        sort_of_ty(field_info.ty, self.env.ty_interner, self.env.ty_cx)
    }
}
