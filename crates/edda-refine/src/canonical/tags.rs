//! Locked per-variant kind tags for refine's canonical [`Predicate`] encoding
//! and the tag-mapper helpers that project IR operator / sort enums onto their
//! wire bytes. Reassigning, reordering, or renaming any constant here is a
//! wire-breaking change that bumps
//! [`CERTIFICATE_FORMAT_VERSION`](crate::CERTIFICATE_FORMAT_VERSION).
//!
//! Locked by `docs/codegen/distribution/03-certificate.md §4`.

use crate::predicate::{ArithOp, BoolBinOp, CmpOp};
use crate::sort::IntWidth;

// ----- locked kind tags (wire encoding) -----

//            reordering any value invalidates every captured certificate's
//            cache key
pub(super) const TAG_VAR: u8 = 0x01;
pub(super) const TAG_INT_LIT: u8 = 0x02;
pub(super) const TAG_BOOL_LIT: u8 = 0x03;
pub(super) const TAG_ARITH: u8 = 0x04;
pub(super) const TAG_NEG: u8 = 0x05;
pub(super) const TAG_MUL_LIT: u8 = 0x06;
pub(super) const TAG_DIV_LIT: u8 = 0x07;
pub(super) const TAG_CMP: u8 = 0x08;
pub(super) const TAG_BOOL_BINOP: u8 = 0x09;
pub(super) const TAG_NOT: u8 = 0x0A;
pub(super) const TAG_IF: u8 = 0x0B;
pub(super) const TAG_FIELD_PROJ: u8 = 0x0C;
pub(super) const TAG_SLICE_LEN: u8 = 0x0D;
pub(super) const TAG_SLICE_INDEX: u8 = 0x0E;
pub(super) const TAG_SLICE_STORE: u8 = 0x0F;
pub(super) const TAG_CAST: u8 = 0x10;
pub(super) const TAG_TAG_EQ: u8 = 0x11;
pub(super) const TAG_FORALL: u8 = 0x12;
pub(super) const TAG_EXISTS: u8 = 0x13;
pub(super) const TAG_MOD_LIT: u8 = 0x14;

// Sub-tags for ArithOp / CmpOp / BoolBinOp.
pub(super) const ARITH_ADD: u8 = 0x00;
pub(super) const ARITH_SUB: u8 = 0x01;

pub(super) const CMP_EQ: u8 = 0x00;
pub(super) const CMP_NE: u8 = 0x01;
pub(super) const CMP_LT: u8 = 0x02;
pub(super) const CMP_LE: u8 = 0x03;
pub(super) const CMP_GT: u8 = 0x04;
pub(super) const CMP_GE: u8 = 0x05;

pub(super) const BOOL_AND: u8 = 0x00;
pub(super) const BOOL_OR: u8 = 0x01;

// Sort sub-tags.
pub(super) const SORT_INT: u8 = 0x00;
pub(super) const SORT_BOOL: u8 = 0x01;
pub(super) const SORT_SLICE: u8 = 0x02;
pub(super) const SORT_TUPLE: u8 = 0x03;
pub(super) const SORT_RECORD: u8 = 0x04;
pub(super) const SORT_SUM: u8 = 0x05;

pub(super) const INT_WIDTH_8: u8 = 0x00;
pub(super) const INT_WIDTH_16: u8 = 0x01;
pub(super) const INT_WIDTH_32: u8 = 0x02;
pub(super) const INT_WIDTH_64: u8 = 0x03;
pub(super) const INT_WIDTH_128: u8 = 0x04;
pub(super) const INT_WIDTH_USIZE: u8 = 0x05;
pub(super) const INT_WIDTH_ISIZE: u8 = 0x06;

pub(super) const INT_LIT_SIGNED: u8 = 0x00;
pub(super) const INT_LIT_UNSIGNED: u8 = 0x01;

// ----- tag mappers -----

pub(super) fn arith_op_tag(op: ArithOp) -> u8 {
    match op {
        ArithOp::Add => ARITH_ADD,
        ArithOp::Sub => ARITH_SUB,
    }
}

pub(super) fn cmp_op_tag(op: CmpOp) -> u8 {
    match op {
        CmpOp::Eq => CMP_EQ,
        CmpOp::Ne => CMP_NE,
        CmpOp::Lt => CMP_LT,
        CmpOp::Le => CMP_LE,
        CmpOp::Gt => CMP_GT,
        CmpOp::Ge => CMP_GE,
    }
}

pub(super) fn bool_binop_tag(op: BoolBinOp) -> u8 {
    match op {
        BoolBinOp::And => BOOL_AND,
        BoolBinOp::Or => BOOL_OR,
    }
}

pub(super) fn int_width_tag(width: IntWidth) -> u8 {
    match width {
        IntWidth::W8 => INT_WIDTH_8,
        IntWidth::W16 => INT_WIDTH_16,
        IntWidth::W32 => INT_WIDTH_32,
        IntWidth::W64 => INT_WIDTH_64,
        IntWidth::W128 => INT_WIDTH_128,
        IntWidth::Usize => INT_WIDTH_USIZE,
        IntWidth::Isize => INT_WIDTH_ISIZE,
    }
}
