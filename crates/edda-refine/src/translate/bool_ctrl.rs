//! Boolean binary operators and if-then-else.

use z3::ast::{Bool, Dynamic};

use crate::error::TranslationError;
use crate::predicate::{BoolBinOp, Predicate};

use super::Translator;

impl<'ctx, 'schema> Translator<'ctx, 'schema> {
    pub(super) fn translate_bool_binop(
        &mut self,
        op: BoolBinOp,
        lhs: &Predicate,
        rhs: &Predicate,
    ) -> Result<Dynamic<'ctx>, TranslationError> {
        let l = self.translate_as_bool(lhs)?;
        let r = self.translate_as_bool(rhs)?;
        let result = match op {
            BoolBinOp::And => Bool::and(self.ctx, &[&l, &r]),
            BoolBinOp::Or => Bool::or(self.ctx, &[&l, &r]),
        };
        Ok(Dynamic::from_ast(&result))
    }

    pub(super) fn translate_ite(
        &mut self,
        cond: &Predicate,
        then_br: &Predicate,
        else_br: &Predicate,
    ) -> Result<Dynamic<'ctx>, TranslationError> {
        let c = self.translate_as_bool(cond)?;
        let t = self.translate(then_br)?;
        let e = self.translate(else_br)?;
        Ok(c.ite(&t, &e))
    }
}
