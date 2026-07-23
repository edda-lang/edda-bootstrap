//! `SwitchTag` terminator-arm walker: the N-way branch on a sum's
//! discriminant.
//!
//! Extracts the tag from a `{ tag, payload }` sum (or accepts a
//! pre-extracted integer tag) and emits `Builder::build_switch` with
//! each arm's case constant taken from the ADT's per-variant
//! discriminant. Delegated to by [`super::lower_terminator`].

use edda_mir::{AdtId, AdtKind, BlockId, Idx, Operand, VariantIdx};
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, IntValue};

use crate::error::CompileError;

use super::super::operand::lower_operand;
use super::super::ty::inkwell_prim_type;
use super::super::LowerCtx;

/// Lower a `SwitchTag` terminator arm.
pub(super) fn lower_switch_tag<'ctx>(
    subject: &Operand,
    adt: AdtId,
    arms: &[(VariantIdx, BlockId)],
    otherwise: BlockId,
    llvm_blocks: &[inkwell::basic_block::BasicBlock<'ctx>],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    let adt_def = cx
        .program
        .adts
        .get(adt)
        .expect("SwitchTag references an AdtId from the same program");
    if adt_def.kind != AdtKind::Sum {
        return Err(CompileError::UnsupportedMirShape {
            shape: "switch-tag-on-product",
            detail: format!(
                "body {:?} SwitchTag on product ADT {:?}; SwitchTag requires a sum",
                cx.body_name, adt_def.name
            ),
        });
    }
    let tag_prim = adt_def.tag_width.ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "switch-tag-missing-tag-width",
            detail: format!(
                "body {:?} SwitchTag on sum ADT {:?} whose tag_width is None",
                cx.body_name, adt_def.name
            ),
        }
    })?;
    let tag_int_ty = inkwell_prim_type(cx.context, tag_prim, cx.arch)
        .expect("sum tag_width is integer-typed; inkwell_prim_type is Some")
        .into_int_type();

    let subject_val = lower_operand(subject, cx)?.ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "switch-tag-unit-subject",
            detail: format!("body {:?} SwitchTag subject is Unit", cx.body_name),
        }
    })?;
    // The subject is either the full sum struct (from a direct Copy
    // of the ADT local) or a pre-extracted integer tag (from an
    // ExtractTag rvalue preceding this terminator, as pattern_adt
    // emits). Accept both.
    let tag_val = match subject_val {
        BasicValueEnum::StructValue(s) => cx
            .builder
            .build_extract_value(s, 0, "swt.tag")
            .expect("build_extract_value at index 0 of a sum struct must succeed")
            .into_int_value(),
        BasicValueEnum::IntValue(i) => i,
        other => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "switch-tag-non-int-subject",
                detail: format!(
                    "body {:?} SwitchTag subject must be a sum struct or pre-extracted integer tag, got {other:?}",
                    cx.body_name,
                ),
            });
        }
    };

    let mut cases: Vec<(IntValue<'ctx>, BasicBlock<'ctx>)> = Vec::with_capacity(arms.len());
    for (variant_idx, target) in arms {
        if variant_idx.index() >= adt_def.variants.len() {
            return Err(CompileError::UnsupportedMirShape {
                shape: "switch-tag-arm-out-of-range",
                detail: format!(
                    "body {:?} SwitchTag arm references variant {} but ADT {:?} has {} variants",
                    cx.body_name,
                    variant_idx.index(),
                    adt_def.name,
                    adt_def.variants.len()
                ),
            });
        }
        let disc = adt_def.variants[variant_idx.index()]
            .discriminant
            .ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "switch-tag-missing-discriminant",
                detail: format!(
                    "body {:?} SwitchTag arm references variant {} of ADT {:?} with no discriminant",
                    cx.body_name,
                    variant_idx.index(),
                    adt_def.name
                ),
            })?;
        let case_const = tag_int_ty.const_int(disc, false);
        let target_bb = llvm_blocks[target.index()];
        cases.push((case_const, target_bb));
    }
    let otherwise_bb = llvm_blocks[otherwise.index()];
    cx.builder
        .build_switch(tag_val, otherwise_bb, &cases)
        .expect("build_switch inside a positioned block must succeed");
    Ok(())
}
