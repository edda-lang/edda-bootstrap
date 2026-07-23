//! Slice length / index / store translation.
//!
//! [`Sort::Slice(elem)`](crate::Sort::Slice) maps to a Z3 `Array(Int, <elem>)`;
//! `len` is a per-element-sort uninterpreted function declared lazily through
//! [`Translator::len_uf_for`](super::Translator::len_uf_for) (see
//! `datatype.rs`).

use z3::ast::{Ast, Dynamic, Int};

use crate::error::TranslationError;
use crate::predicate::Predicate;

use super::datatype::slice_element_sort;
use super::Translator;

impl<'ctx, 'schema> Translator<'ctx, 'schema> {
    pub(super) fn translate_slice_len(
        &mut self,
        slice: &Predicate,
    ) -> Result<Dynamic<'ctx>, TranslationError> {
        let elem_sort = slice_element_sort(slice)?;
        let arr = self.translate_as_array(slice)?;
        let app = {
            let uf = self.len_uf_for(&elem_sort)?;
            uf.apply(&[&arr as &dyn Ast])
        };
        // `len: [T] -> usize` is non-negative by type, but the UF's Int
        // codomain doesn't know that — `0 <= xs.len()` was previously
        // falsifiable. Recover it as a per-application
        // sort axiom.
        if let Some(len_int) = app.as_int() {
            let zero = Int::from_i64(self.ctx, 0);
            self.sort_axioms.push(len_int.ge(&zero));
        }
        Ok(app)
    }

    pub(super) fn translate_slice_index(
        &mut self,
        slice: &Predicate,
        index: &Predicate,
    ) -> Result<Dynamic<'ctx>, TranslationError> {
        let arr = self.translate_as_array(slice)?;
        let i = self.translate_as_int(index)?;
        Ok(arr.select(&i))
    }

    pub(super) fn translate_slice_store(
        &mut self,
        slice: &Predicate,
        index: &Predicate,
        value: &Predicate,
    ) -> Result<Dynamic<'ctx>, TranslationError> {
        let arr = self.translate_as_array(slice)?;
        let i = self.translate_as_int(index)?;
        let v = self.translate(value)?;
        let new_arr = arr.store(&i, &v);
        Ok(Dynamic::from_ast(&new_arr))
    }
}
