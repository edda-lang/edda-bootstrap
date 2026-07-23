//! Translator state: variable interning and recursive sort mapping.
//!
//! Lives next to the [`Translator`](super::Translator) struct definition in
//! [`mod`](super) — the methods here are what populate the `vars` cache and
//! drive the [`Sort`] → Z3 sort translation that the datatype / slice / record
//! paths all funnel through.

use smol_str::SmolStr;

use z3::ast::{Array, Bool, Datatype, Dynamic, Int};
use z3::Sort as Z3Sort;

use crate::error::TranslationError;
use crate::sort::Sort;

use super::Translator;

impl<'ctx, 'schema> Translator<'ctx, 'schema> {
    // Look up or freshly declare a Z3 const for `name` at sort `sort`. Returns
    // a `Dynamic` so the caller can use it where any AST node fits.
    pub(super) fn intern_var(
        &mut self,
        name: &SmolStr,
        sort: &Sort,
    ) -> Result<Dynamic<'ctx>, TranslationError> {
        if let Some(existing) = self.vars.get(name) {
            return Ok(existing.clone());
        }
        let constant: Dynamic<'ctx> = match sort {
            Sort::Int(int_sort) => {
                let c = Int::new_const(self.ctx, name.as_str());
                // The SMT layer models every integer sort as Z3's
                // mathematical Int, which loses the type-level fact that
                // an unsigned value is non-negative — `0 <= n` on a
                // `usize` was previously falsifiable with `n = -1`.
                // Recover it as a sort axiom.
                if !int_sort.signed {
                    let zero = Int::from_i64(self.ctx, 0);
                    self.sort_axioms.push(c.ge(&zero));
                }
                Dynamic::from_ast(&c)
            }
            Sort::Bool => Dynamic::from_ast(&Bool::new_const(self.ctx, name.as_str())),
            Sort::Slice(elem) => {
                let elem_z3 = self.sort_to_z3(elem)?;
                Dynamic::from_ast(&Array::new_const(
                    self.ctx,
                    name.as_str(),
                    &Z3Sort::int(self.ctx),
                    &elem_z3,
                ))
            }
            Sort::Record(record) => {
                let z3_sort = self.record_dt_for(record.name())?.sort.clone();
                Dynamic::from_ast(&Datatype::new_const(self.ctx, name.as_str(), &z3_sort))
            }
            Sort::Sum(sum) => {
                let z3_sort = self.sum_dt_for(sum.name())?.sort.clone();
                Dynamic::from_ast(&Datatype::new_const(self.ctx, name.as_str(), &z3_sort))
            }
            Sort::Tuple(_) => {
                return Err(TranslationError::Unsupported {
                    what: "variable of tuple sort".to_string(),
                });
            }
        };
        self.vars.insert(name.clone(), constant.clone());
        Ok(constant)
    }

    pub(super) fn sort_to_z3(&mut self, sort: &Sort) -> Result<Z3Sort<'ctx>, TranslationError> {
        match sort {
            Sort::Int(_) => Ok(Z3Sort::int(self.ctx)),
            Sort::Bool => Ok(Z3Sort::bool(self.ctx)),
            Sort::Slice(elem) => {
                let elem_sort = self.sort_to_z3(elem)?;
                Ok(Z3Sort::array(self.ctx, &Z3Sort::int(self.ctx), &elem_sort))
            }
            Sort::Record(record) => Ok(self.record_dt_for(record.name())?.sort.clone()),
            Sort::Sum(sum) => Ok(self.sum_dt_for(sum.name())?.sort.clone()),
            Sort::Tuple(_) => Err(TranslationError::Unsupported {
                what: "tuple sort".to_string(),
            }),
        }
    }
}

