//! Checked-overflow and trapping-overflow CFG synthesis for the arithmetic
//! operator family: `+?` / `-?` / `*?` / `%?` (checked, raising) and the
//! default `+` / `-` / `*` (trapping, aborting).
//!
//! Split out of `arith` (the eager binary/unary/short-circuit path stays there).
//! This module file owns the shared `Overflow`-ADT lookups and `int_min_for`;
//! the checked-arithmetic lowerings live in the child `checked` module, the
//! trapping-arithmetic lowering in the child `trapping` module.
//! `arith::lower_binary` dispatches into both and `expr::lower_checked_cast`
//! reuses the ADT lookups.

use crate::adt::AdtKind;
use crate::arena::Idx;
use crate::ids::{AdtId, VariantIdx};
use crate::ty::MirPrim;

use super::ctx::LoweringContext;

mod checked;
mod trapping;

pub(super) use checked::{lower_checked_arith, lower_checked_mod};
pub(super) use trapping::lower_trapping_arith;

/// `prim::MIN` widened to `i128` for use as an `Operand::Const`. Panics
/// on unsigned or non-integer prims — callers must guard via
/// `signed` flag before calling.
pub(super) fn int_min_for(prim: MirPrim) -> i128 {
    match prim {
        MirPrim::I8 => i8::MIN as i128,
        MirPrim::I16 => i16::MIN as i128,
        MirPrim::I32 => i32::MIN as i128,
        // `Isize` is 64-bit on every v0.1 target.
        MirPrim::I64 | MirPrim::Isize => i64::MIN as i128,
        MirPrim::I128 => i128::MIN,
        _ => unreachable!("int_min_for called on non-signed-integer primitive {prim:?}"),
    }
}

/// Locate the `Overflow` ADT in the current MIR program. Returns
/// `None` when no ADT is named `Overflow` (typically because the
/// caller's file forgot `import std.overflow`).
pub(super) fn find_overflow_adt(ctx: &LoweringContext<'_>) -> Option<AdtId> {
    let target = ctx.interner.intern("Overflow");
    for (id, adt) in ctx.program.program().adts.iter_enumerated() {
        if adt.name == target {
            return Some(id);
        }
    }
    None
}

/// Locate the `overflow` variant within `Overflow`. Returns `None`
/// only when the ADT shape diverges from the locked stdlib definition.
pub(super) fn find_overflow_variant(ctx: &LoweringContext<'_>, adt_id: AdtId) -> Option<VariantIdx> {
    let target = ctx.interner.intern("overflow");
    let adt = ctx.program.program().adts.get(adt_id)?;
    if !matches!(adt.kind, AdtKind::Sum) {
        return None;
    }
    for (idx, variant) in adt.variants.iter().enumerate() {
        if variant.name == target {
            return Some(VariantIdx::new(idx));
        }
    }
    None
}
