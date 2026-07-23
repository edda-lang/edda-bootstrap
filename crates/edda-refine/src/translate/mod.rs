//! Predicate → Z3 AST translation.
//!
//! Covers LIA + Bool + Array, plus EUF for records,
//! sum tag-equality, and non-narrowing integer cast (one-line identity at
//! the SMT layer). Tuples remain deferred — they're representable
//! as Z3 datatypes but require a per-shape declaration that isn't worth the
//! implementation cost yet; tuple-bearing predicates surface as
//! [`TranslationError::Unsupported`].
//!
//! # Sort mapping
//!
//! - [`Sort::Int`] (any width / signedness) → Z3 `Int`. Bit-width is not
//!   represented at the SMT layer because the refinement-decidability §2
//!   contract is LIA over mathematical integers — overflow obligations are
//!   themselves predicates the typechecker discharges with `where x <= MAX`
//!   constraints, not implicit modular arithmetic.
//! - [`Sort::Bool`] → Z3 `Bool`.
//! - [`Sort::Slice(elem)`] → Z3 `Array(Int, <elem>)` with a per-element-sort
//!   uninterpreted `len_<key>: Array(Int, T) -> Int` function declared
//!   lazily.
//! - [`Sort::Record(name)`] → Z3 datatype declared from the
//!   [`Schema`](crate::Schema) registry. Field projection uses the
//!   datatype's accessor; structural equality is free.
//! - [`Sort::Sum(name)`] → Z3 datatype with one constructor per variant.
//!   Tag-equality uses constructor application; variant-distinctness is
//!   free.
//! - [`Sort::Tuple`] → deferred (`TranslationError::Unsupported`).
//!
//! # Variable caching
//!
//! Every Edda variable maps to exactly one Z3 typed constant per translation
//! session. Repeated mentions of the same variable resolve to the same Z3
//! const so the solver can reason about them as the same symbol.
//!
//! # Module layout
//!
//! - [`mod`](self) — [`Translator`] struct, constructor, master dispatch
//!   ([`Translator::translate`]) and the bool-typed entry point used by the
//!   discharge layer ([`Translator::translate_bool`]).
//! - `state` — variable interning ([`Translator::intern_var`]) and the
//!   recursive [`Sort`] → Z3 sort mapping ([`Translator::sort_to_z3`]).
//! - `datatype` — record / sum datatype lazy declaration, field projection,
//!   tag equality, slice-length uninterpreted-function caching, and the
//!   free helpers `slice_element_sort` / `sanitize_uf_suffix`.
//! - `arith` — integer literals, arithmetic, comparison, and the
//!   `translate_as_*` sort coercion helpers.
//! - `bool_ctrl` — boolean binary operators and if-then-else.
//! - `slice` — slice length / index / store.

use std::collections::HashMap;

use smol_str::SmolStr;

use z3::ast::{Bool, Dynamic, Int};
use z3::{Context, DatatypeSort, FuncDecl};

use crate::error::TranslationError;
use crate::predicate::Predicate;
use crate::schema::Schema;
use crate::sort::Sort;

mod arith;
mod bool_ctrl;
mod datatype;
mod slice;
mod state;

//            Z3 const, so two occurrences of the same Edda variable name reach
//            the solver as the same SMT symbol
//            `len()` call on slices of that element sort
//            declared at a particular Z3 sort, every subsequent reference to
//            the same record / sum name reuses the cached `DatatypeSort`
//            truth (unsigned-sorted variables and slice-length applications
//            are non-negative), so asserting all of them alongside any
//            context / negated goal is always sound
/// Translation state for one discharge attempt. Borrows a Z3
/// [`Context`] and a [`Schema`] and caches the variables, uninterpreted
/// functions, and datatypes introduced during translation.
pub(crate) struct Translator<'ctx, 'schema> {
    pub(super) ctx: &'ctx Context,
    pub(super) schema: &'schema Schema,
    pub(super) vars: HashMap<SmolStr, Dynamic<'ctx>>,
    pub(super) len_ufs: HashMap<LenKey, FuncDecl<'ctx>>,
    pub(super) record_dts: HashMap<SmolStr, DatatypeSort<'ctx>>,
    pub(super) sum_dts: HashMap<SmolStr, DatatypeSort<'ctx>>,
    pub(super) sort_axioms: Vec<Bool<'ctx>>,
}

// Cache key for the `len_<elem>` UFs. We key on the element sort's display
// shape so two `Slice(Int{i32})` references share a UF.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub(super) struct LenKey(pub(super) String);

impl<'ctx, 'schema> Translator<'ctx, 'schema> {
    /// Construct a fresh translator bound to `ctx` and the type [`Schema`].
    /// Pass [`Schema::empty`] when the obligation is known not to touch any
    /// records or sums.
    pub fn new(ctx: &'ctx Context, schema: &'schema Schema) -> Translator<'ctx, 'schema> {
        Translator {
            ctx,
            schema,
            vars: HashMap::new(),
            len_ufs: HashMap::new(),
            record_dts: HashMap::new(),
            sum_dts: HashMap::new(),
            sort_axioms: Vec::new(),
        }
    }

    /// Translate a predicate whose sort is [`Sort::Bool`] — the goal /
    /// context-conjunct entry point used by the discharge layer.
    pub fn translate_bool(&mut self, p: &Predicate) -> Result<Bool<'ctx>, TranslationError> {
        let d = self.translate(p)?;
        d.as_bool().ok_or_else(|| TranslationError::SortMismatch {
            expected: Sort::Bool,
        })
    }

    /// Iterate over (name, z3-const) pairs the translator has interned. Used
    /// by [`Z3Backend`](crate::Z3Backend) to project a Z3 model into a
    /// [`Counterexample`](crate::Counterexample).
    pub(crate) fn var_bindings(&self) -> impl Iterator<Item = (&SmolStr, &Dynamic<'ctx>)> {
        self.vars.iter()
    }

    /// Type-level facts the mathematical-Int encoding loses, accumulated
    /// during translation: `v >= 0` for every unsigned-sorted variable and
    /// `len(xs) >= 0` for every slice-length application. The discharge
    /// layer asserts each alongside the obligation's context — sound
    /// unconditionally because every entry is a truth of the Edda type
    /// system, never a caller-supplied assumption.
    pub(crate) fn sort_axioms(&self) -> impl Iterator<Item = &Bool<'ctx>> {
        self.sort_axioms.iter()
    }

    /// Translate any predicate, returning a sort-erased [`Dynamic`]. Callers
    /// that know the expected sort downcast via [`Dynamic::as_int`],
    /// [`Dynamic::as_bool`], or [`Dynamic::as_array`].
    pub fn translate(&mut self, p: &Predicate) -> Result<Dynamic<'ctx>, TranslationError> {
        match p {
            Predicate::Var(v) => Ok(self.intern_var(&v.name, &v.sort)?),
            Predicate::IntLit(lit) => Ok(Dynamic::from_ast(&self.translate_int_lit(*lit)?)),
            Predicate::BoolLit(b) => Ok(Dynamic::from_ast(&Bool::from_bool(self.ctx, *b))),
            Predicate::Arith { op, lhs, rhs } => self.translate_arith(*op, lhs, rhs),
            Predicate::Neg(operand) => {
                let i = self.translate_as_int(operand)?;
                Ok(Dynamic::from_ast(&i.unary_minus()))
            }
            Predicate::MulLit { c, expr } => {
                let lit = self.translate_int_lit(*c)?;
                let e = self.translate_as_int(expr)?;
                Ok(Dynamic::from_ast(&Int::mul(self.ctx, &[&lit, &e])))
            }
            Predicate::DivLit { expr, c } => {
                let lit = self.translate_int_lit(*c)?;
                let e = self.translate_as_int(expr)?;
                Ok(Dynamic::from_ast(&e.div(&lit)))
            }
            Predicate::ModLit { expr, c } => {
                let lit = self.translate_int_lit(*c)?;
                let e = self.translate_as_int(expr)?;
                Ok(Dynamic::from_ast(&e.modulo(&lit)))
            }
            Predicate::Cmp { op, lhs, rhs } => self.translate_cmp(*op, lhs, rhs),
            Predicate::BoolBinOp { op, lhs, rhs } => self.translate_bool_binop(*op, lhs, rhs),
            Predicate::Not(operand) => {
                let b = self.translate_as_bool(operand)?;
                Ok(Dynamic::from_ast(&b.not()))
            }
            Predicate::If {
                cond,
                then_br,
                else_br,
            } => self.translate_ite(cond, then_br, else_br),
            Predicate::SliceLen { slice } => self.translate_slice_len(slice),
            Predicate::SliceIndex { slice, index } => self.translate_slice_index(slice, index),
            Predicate::SliceStore {
                slice,
                index,
                value,
            } => self.translate_slice_store(slice, index, value),
            Predicate::FieldProj { base, field } => self.translate_field_proj(base, field),
            Predicate::Cast { value, to: _ } => {
                // Non-narrowing integer casts collapse at the Z3 layer: every
                // Edda integer sort maps to Z3's mathematical Int, so the cast
                // is identity. Narrowing-cast obligations are separate goals
                // (ObligationKind::NarrowingCast) that the typechecker raises
                // with range constraints; refine discharges them through the
                // LIA path, not through Cast translation.
                self.translate(value)
            }
            Predicate::TagEq { value, variant } => self.translate_tag_eq(value, variant),
            Predicate::Forall {
                bound,
                lower,
                upper,
                body,
            } => self.translate_quantifier(bound, lower, upper, body, Quantifier::Forall),
            Predicate::Exists {
                bound,
                lower,
                upper,
                body,
            } => self.translate_quantifier(bound, lower, upper, body, Quantifier::Exists),
        }
    }

    fn translate_quantifier(
        &mut self,
        bound: &crate::predicate::Variable,
        lower: &Predicate,
        upper: &Predicate,
        body: &Predicate,
        kind: Quantifier,
    ) -> Result<Dynamic<'ctx>, TranslationError> {
        // Fresh Z3 integer const for the bound variable. We stash any
        // previous binding of the same name (outer-scope variable or a
        // nested-quantifier shadow) so the post-quantifier scope is
        // restored exactly.
        let bound_int = z3::ast::Int::new_const(self.ctx, bound.name.as_str());
        let prev = self.vars.insert(
            bound.name.clone(),
            Dynamic::from_ast(&bound_int),
        );

        // Translate sub-predicates inside the augmented context.
        let lo_result = self.translate_as_int(lower);
        let hi_result = self.translate_as_int(upper);
        let body_result = self.translate_as_bool(body);

        // Restore previous binding.
        match prev {
            Some(prev_val) => {
                self.vars.insert(bound.name.clone(), prev_val);
            }
            None => {
                self.vars.remove(&bound.name);
            }
        }

        let lo = lo_result?;
        let hi = hi_result?;
        let body_b = body_result?;

        let lower_ok = bound_int.ge(&lo);
        let upper_ok = bound_int.lt(&hi);
        let range = Bool::and(self.ctx, &[&lower_ok, &upper_ok]);

        let constrained = match kind {
            Quantifier::Forall => range.implies(&body_b),
            Quantifier::Exists => Bool::and(self.ctx, &[&range, &body_b]),
        };

        let quantified = match kind {
            Quantifier::Forall => {
                z3::ast::forall_const(self.ctx, &[&bound_int], &[], &constrained)
            }
            Quantifier::Exists => {
                z3::ast::exists_const(self.ctx, &[&bound_int], &[], &constrained)
            }
        };
        Ok(Dynamic::from_ast(&quantified))
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum Quantifier {
    Forall,
    Exists,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::predicate::{CmpOp, IntLit, Variable};
    use crate::sort::{IntSort, IntWidth, RecordRef};
    use z3::{Config, Solver};

    fn fresh_ctx() -> Context {
        Context::new(&Config::new())
    }

    fn i32_sort() -> IntSort {
        IntSort::sized(IntWidth::W32, true)
    }

    #[test]
    fn variable_caching_yields_same_z3_symbol() {
        let ctx = fresh_ctx();
        let schema = Schema::empty();
        let mut t = Translator::new(&ctx, &schema);
        let v = Predicate::Var(Variable::new("x", Sort::Int(i32_sort())));
        let a = t.translate(&v).unwrap();
        let b = t.translate(&v).unwrap();
        // Same name + sort → cache hit, same Z3 AST node.
        assert!(a == b);
    }

    #[test]
    fn translation_dispatches_through_z3_round_trip() {
        // x > 5 ∧ x < 10 should be SAT in Z3.
        let ctx = fresh_ctx();
        let schema = Schema::empty();
        let mut t = Translator::new(&ctx, &schema);
        let x = Predicate::Var(Variable::new("x", Sort::Int(i32_sort())));
        let p = Predicate::and(
            Predicate::cmp(
                CmpOp::Gt,
                x.clone(),
                Predicate::IntLit(IntLit::signed(5, i32_sort())),
            ),
            Predicate::cmp(
                CmpOp::Lt,
                x,
                Predicate::IntLit(IntLit::signed(10, i32_sort())),
            ),
        );
        let b = t.translate_bool(&p).unwrap();
        let solver = Solver::new(&ctx);
        solver.assert(&b);
        assert_eq!(solver.check(), z3::SatResult::Sat);
    }

    #[test]
    fn field_projection_against_empty_schema_reports_unknown_type() {
        let ctx = fresh_ctx();
        let schema = Schema::empty();
        let mut t = Translator::new(&ctx, &schema);
        let base = Predicate::Var(Variable::new(
            "buf",
            Sort::Record(RecordRef::new("StringBuf")),
        ));
        let field = crate::sort::FieldRef::new(
            RecordRef::new("StringBuf"),
            "len",
            Sort::usize(),
        );
        let p = Predicate::field_proj(base, field);
        let err = t.translate(&p).unwrap_err();
        match err {
            TranslationError::UnknownTypeName { name } => {
                assert_eq!(name.as_str(), "StringBuf");
            }
            other => panic!("expected UnknownTypeName, got {other:?}"),
        }
    }

    #[test]
    fn sanitize_uf_suffix_collapses_punctuation() {
        use super::datatype::sanitize_uf_suffix;
        assert_eq!(sanitize_uf_suffix("Int(IntSort { width: W64, signed: true })"), "Int_IntSort_width_W64_signed_true");
    }

    // `0 <= n` for `n: usize` is provable only through the unsigned
    // non-negativity sort axiom — the mathematical-Int
    // encoding alone lets Z3 pick `n = -1`.
    #[test]
    fn unsigned_var_nonneg_axiom_discharges_zero_le() {
        let ctx = fresh_ctx();
        let schema = Schema::empty();
        let mut t = Translator::new(&ctx, &schema);
        let goal = Predicate::cmp(
            CmpOp::Le,
            Predicate::IntLit(IntLit::signed(0, i32_sort())),
            Predicate::Var(Variable::new("n", Sort::usize())),
        );
        let g = t.translate_bool(&goal).unwrap();
        let solver = Solver::new(&ctx);
        solver.assert(&g.not());
        for ax in t.sort_axioms() {
            solver.assert(ax);
        }
        assert_eq!(solver.check(), z3::SatResult::Unsat);
    }

    // `0 <= xs.len()` is provable only through the per-application
    // slice-length non-negativity axiom — `len` is an
    // uninterpreted `Array(Int, T) -> Int` function otherwise.
    #[test]
    fn slice_len_nonneg_axiom_discharges_zero_le_len() {
        let ctx = fresh_ctx();
        let schema = Schema::empty();
        let mut t = Translator::new(&ctx, &schema);
        let xs = Predicate::Var(Variable::new(
            "xs",
            Sort::slice(Sort::Int(IntSort::sized(IntWidth::W8, false))),
        ));
        let goal = Predicate::cmp(
            CmpOp::Le,
            Predicate::IntLit(IntLit::signed(0, i32_sort())),
            Predicate::slice_len(xs),
        );
        let g = t.translate_bool(&goal).unwrap();
        let solver = Solver::new(&ctx);
        solver.assert(&g.not());
        for ax in t.sort_axioms() {
            solver.assert(ax);
        }
        assert_eq!(solver.check(), z3::SatResult::Unsat);
    }

    // Signed sorts must NOT receive the non-negativity axiom — `0 <= x`
    // for `x: i32` is genuinely falsifiable.
    #[test]
    fn signed_var_gets_no_nonneg_axiom() {
        let ctx = fresh_ctx();
        let schema = Schema::empty();
        let mut t = Translator::new(&ctx, &schema);
        let x = Predicate::Var(Variable::new("x", Sort::Int(i32_sort())));
        let _ = t.translate(&x).unwrap();
        assert_eq!(t.sort_axioms().count(), 0);
    }
}
