//! Arithmetic, comparison, integer literals, and sort coercion helpers.
//!
//! The `translate_as_*` helpers are the typed view onto [`Translator::translate`]
//! the rest of the sub-files lean on — `translate_as_int` for arithmetic
//! operands, `translate_as_bool` for boolean operands and the `Not` arm,
//! `translate_as_array` for the slice path.

use z3::ast::{Array, Ast, Bool, Dynamic, Int};

use crate::error::TranslationError;
use crate::predicate::{ArithOp, CmpOp, IntLit, IntLitValue, Predicate};
use crate::sort::{IntSort, Sort};

use super::Translator;

impl<'ctx, 'schema> Translator<'ctx, 'schema> {
    pub(super) fn translate_int_lit(&self, lit: IntLit) -> Result<Int<'ctx>, TranslationError> {
        match lit.value {
            IntLitValue::Signed(v) => match i64::try_from(v) {
                Ok(small) => Ok(Int::from_i64(self.ctx, small)),
                Err(_) => Int::from_str(self.ctx, &v.to_string()).ok_or_else(|| {
                    TranslationError::IntLitOutOfRange {
                        value: v.to_string(),
                    }
                }),
            },
            IntLitValue::Unsigned(v) => match u64::try_from(v) {
                Ok(small) => Ok(Int::from_u64(self.ctx, small)),
                Err(_) => Int::from_str(self.ctx, &v.to_string()).ok_or_else(|| {
                    TranslationError::IntLitOutOfRange {
                        value: v.to_string(),
                    }
                }),
            },
        }
    }

    pub(super) fn translate_as_int(
        &mut self,
        p: &Predicate,
    ) -> Result<Int<'ctx>, TranslationError> {
        let d = self.translate(p)?;
        d.as_int().ok_or_else(|| TranslationError::SortMismatch {
            expected: Sort::Int(IntSort::USIZE),
        })
    }

    pub(super) fn translate_as_bool(
        &mut self,
        p: &Predicate,
    ) -> Result<Bool<'ctx>, TranslationError> {
        let d = self.translate(p)?;
        d.as_bool().ok_or_else(|| TranslationError::SortMismatch {
            expected: Sort::Bool,
        })
    }

    pub(super) fn translate_as_array(
        &mut self,
        p: &Predicate,
    ) -> Result<Array<'ctx>, TranslationError> {
        let d = self.translate(p)?;
        d.as_array()
            .ok_or_else(|| TranslationError::SortMismatch {
                expected: Sort::slice(Sort::Bool),
            })
    }

    pub(super) fn translate_arith(
        &mut self,
        op: ArithOp,
        lhs: &Predicate,
        rhs: &Predicate,
    ) -> Result<Dynamic<'ctx>, TranslationError> {
        let l = self.translate_as_int(lhs)?;
        let r = self.translate_as_int(rhs)?;
        let result = match op {
            ArithOp::Add => Int::add(self.ctx, &[&l, &r]),
            ArithOp::Sub => Int::sub(self.ctx, &[&l, &r]),
        };
        Ok(Dynamic::from_ast(&result))
    }

    pub(super) fn translate_cmp(
        &mut self,
        op: CmpOp,
        lhs: &Predicate,
        rhs: &Predicate,
    ) -> Result<Dynamic<'ctx>, TranslationError> {
        // Eq / Ne work on any matching-sort pair; <, <=, >, >= are integer-only.
        // The typechecker should not produce a mismatched-sort Cmp; if it does,
        // `_safe_eq` will return an Err and we surface it as SortMismatch.
        let l = self.translate(lhs)?;
        let r = self.translate(rhs)?;
        let result = match op {
            CmpOp::Eq => l._safe_eq(&r).map_err(|_| TranslationError::SortMismatch {
                expected: Sort::Bool,
            })?,
            CmpOp::Ne => l
                ._safe_eq(&r)
                .map_err(|_| TranslationError::SortMismatch {
                    expected: Sort::Bool,
                })?
                .not(),
            CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge => {
                let li = l.as_int().ok_or_else(|| TranslationError::SortMismatch {
                    expected: Sort::Int(IntSort::USIZE),
                })?;
                let ri = r.as_int().ok_or_else(|| TranslationError::SortMismatch {
                    expected: Sort::Int(IntSort::USIZE),
                })?;
                match op {
                    CmpOp::Lt => li.lt(&ri),
                    CmpOp::Le => li.le(&ri),
                    CmpOp::Gt => li.gt(&ri),
                    CmpOp::Ge => li.ge(&ri),
                    _ => unreachable!(),
                }
            }
        };
        Ok(Dynamic::from_ast(&result))
    }
}
