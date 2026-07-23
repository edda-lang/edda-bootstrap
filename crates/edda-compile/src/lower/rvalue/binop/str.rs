//! `Str`-class [`BinOp`] lowering via the `__edda_str_eq` runtime
//! extern.
//!
//! `Str` equality (`Eq` / `Ne`) is lowered through the `__edda_str_eq`
//! runtime extern, declared and called with the SAME aggregate-by-value
//! ABI `lower_fn_sig` / `declare_extern` use for a `(String, String) ->
//! bool` function: on win64 each `{ ptr, len }` fat pointer passes
//! indirectly as a `ptr byval({ ptr, i64 })`; on the SysV / AArch64
//! targets the struct passes by value (the backend splits it into the
//! two field registers). Matching that ABI is what lets an Edda
//! `@abi("__edda_str_eq")` definition (lowered byval by `lower_fn_sig`)
//! and the `edda-rt` `extern "C" fn(EdStr, EdStr)` fallback both provide
//! the symbol without an ABI split at the call site.

use edda_mir::BinOp;
use edda_target::Os;
use inkwell::AddressSpace;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::module::Module;
use inkwell::types::{AnyType, BasicMetadataTypeEnum, StructType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

use crate::error::CompileError;

use crate::lower::LowerCtx;

/// Name of the runtime extern used to compare two Edda `String` values
/// byte-for-byte. The signature is `(a: String, b: String) -> i8` — two
/// `{ ptr, i64 }` aggregate params (win64-indirect `byval`, SysV
/// by-value) and one scalar `i8` return, matching the wire ABI
/// `lower_fn_sig` produces for a source-bodied `(String, String) -> bool`
/// function so an `@abi("__edda_str_eq")` Edda definition can provide it.
const STR_EQ_SYMBOL: &str = "__edda_str_eq";

/// Lower a `BinOp` whose operand class is `MirPrim::Str`.
///
/// Declares the runtime extern `__edda_str_eq` lazily, then emits the
/// call passing each operand's `{ ptr, isize }` fat pointer with the
/// aggregate-by-value ABI (`byval` on win64, by-value struct elsewhere).
/// The runtime returns `1`/`0` as `i8`; the helper truncates to `i1` to
/// match the `BinOp::Eq` / `BinOp::Ne` result-type contract (the `trunc`
/// reads bit 0, so an `i1`-returning Edda `@abi` provider and an
/// `i8`-returning runtime fallback are both read correctly).
pub(super) fn lower_binop_str<'ctx>(
    op: BinOp,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    if !matches!(op, BinOp::Eq | BinOp::Ne) {
        return Err(CompileError::UnsupportedMirShape {
            shape: "binop-on-str",
            detail: format!(
                "body {:?} performs {op:?} on `str` operands; only `Eq` / `Ne` \
                 are admitted on `String` operands at v0.1 — ordering and \
                 arithmetic are not specified",
                cx.body_name
            ),
        });
    }

    let lhs_struct = lhs.into_struct_value();
    let rhs_struct = rhs.into_struct_value();
    // The `{ ptr, i64 }` fat-pointer struct type, shared by both operands.
    let str_ty = lhs_struct.get_type();

    // Win64 passes a 16-byte aggregate indirectly (`byval`); the SysV /
    // AArch64 ABIs pass it by value (the field registers). This mirrors
    // `lower_fn_sig` / `declare_extern`'s `use_indirect_abi` gate so the
    // call site agrees with whichever definition provides the symbol.
    let indirect = cx.os == Os::Windows;
    let str_eq_fn = declare_str_eq(cx.context, cx.module, str_ty, indirect);

    let mut args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(2);
    let mut byval_indices: Vec<u32> = Vec::new();
    for operand in [lhs_struct, rhs_struct] {
        if indirect {
            let tmp = cx.build_entry_alloca(str_ty, "str.byval.tmp");
            cx.builder
                .build_store(tmp, operand)
                .expect("build_store of str operand into byval temp");
            byval_indices.push(args.len() as u32);
            args.push(tmp.into());
        } else {
            args.push(operand.into());
        }
    }

    let call = cx
        .builder
        .build_call(str_eq_fn, &args, "str.eq.call")
        .expect("build_call to __edda_str_eq must succeed in a positioned block");
    if indirect {
        let kind_id = Attribute::get_named_enum_kind_id("byval");
        for idx in &byval_indices {
            let attr = cx
                .context
                .create_type_attribute(kind_id, str_ty.as_any_type_enum());
            call.add_attribute(AttributeLoc::Param(*idx), attr);
        }
    }
    let ret = call
        .try_as_basic_value()
        .left()
        .expect("__edda_str_eq returns an i8 — a basic value");
    let ret_i8 = ret.into_int_value();
    // The runtime returns `1` for equal, `0` for unequal — both fit in `i1`.
    // Truncate to bool so the result matches every other `Eq`/`Ne` binop
    // (`i1` per LLVM's `icmp` lowering). `trunc` keeps bit 0, so an
    // `i1`-returning `@abi` provider (upper bits unspecified) is also read
    // correctly.
    let bool_ty = cx.context.bool_type();
    let eq_bool = cx
        .builder
        .build_int_truncate(ret_i8, bool_ty, "str.eq.bool")
        .expect("trunc i8 -> i1 in a positioned block");
    let result_bool = match op {
        BinOp::Eq => eq_bool,
        BinOp::Ne => cx
            .builder
            .build_not(eq_bool, "str.ne")
            .expect("xor i1 -> i1 in a positioned block"),
        _ => unreachable!("only Eq / Ne reach this arm; guard checked above"),
    };
    Ok(result_bool.into())
}

/// Lazily declare the `__edda_str_eq` runtime extern in `module`,
/// reusing an existing declaration if one is already registered. The
/// param ABI matches `declare_extern` for a `(String, String) -> bool`
/// signature: `ptr byval({ ptr, i64 })` on win64, the `{ ptr, i64 }`
/// struct by value otherwise; the return is `i8`.
fn declare_str_eq<'ctx>(
    context: &'ctx inkwell::context::Context,
    module: &Module<'ctx>,
    str_ty: StructType<'ctx>,
    indirect: bool,
) -> inkwell::values::FunctionValue<'ctx> {
    if let Some(existing) = module.get_function(STR_EQ_SYMBOL) {
        return existing;
    }
    let i8_ty = context.i8_type();
    let param_ty: BasicMetadataTypeEnum<'ctx> = if indirect {
        context.ptr_type(AddressSpace::default()).into()
    } else {
        str_ty.into()
    };
    let params: [BasicMetadataTypeEnum<'ctx>; 2] = [param_ty, param_ty];
    let fn_ty = i8_ty.fn_type(&params, false);
    let func = module.add_function(STR_EQ_SYMBOL, fn_ty, None);
    if indirect {
        let kind_id = Attribute::get_named_enum_kind_id("byval");
        for idx in 0..2u32 {
            let attr = context.create_type_attribute(kind_id, str_ty.as_any_type_enum());
            func.add_attribute(AttributeLoc::Param(idx), attr);
        }
    }
    func
}
