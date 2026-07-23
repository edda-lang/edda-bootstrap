//! `Raise` terminator walker — package an error payload into the
//! body's return-Result sum and `ret` it directly.
//!
//! Every `Raise` in MIR is the `?`-propagation primitive: the
//! typechecker's rewrite ensures the enclosing body's return type
//! is the `Result`-shaped sum carrying both the success payload and
//! the error variants the function can raise.

use edda_mir::{AdtId, AdtKind, Idx, LocalId, MirTypeKind, Operand, VariantIdx};
use inkwell::types::BasicType;

use crate::error::CompileError;

use super::super::local::body_uses_sret;
use super::super::rvalue::{build_variant_in_alloca, build_variant_value};
use super::super::LowerCtx;

/// See `terminator::call::AGGREGATE_COPY_ALIGN`.
const AGGREGATE_COPY_ALIGN: u32 = 8;

/// Lower the `Raise` terminator: build the body's return-type sum
/// carrying the chosen err variant, and `ret` it directly. This is
/// the `?`-propagation primitive — every Raise in MIR is a
/// "package-and-return" of an error payload.
pub(super) fn lower_raise<'ctx>(
    err_adt: AdtId,
    value: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    let return_ty = &cx.body.return_ty;
    let return_adt_id = match &return_ty.kind {
        MirTypeKind::Adt(id) => *id,
        other => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "raise-without-sum-return-type",
                detail: format!(
                    "body {:?} Raises but its return type is {other:?}; \
                     the typechecker's `?`-propagation rewrite must produce \
                     a sum-typed return for raising functions",
                    cx.body_name
                ),
            });
        }
    };
    let return_adt = cx
        .program
        .adts
        .get(return_adt_id)
        .expect("Raise return-type AdtId comes from the same program");
    if return_adt.kind != AdtKind::Sum {
        return Err(CompileError::UnsupportedMirShape {
            shape: "raise-on-product-return",
            detail: format!(
                "body {:?} Raises but its return ADT {:?} is a Product; \
                 sum-shaped return required",
                cx.body_name, return_adt.name
            ),
        });
    }

    // Find the err variant: the variant whose single field's MirType
    // is `MirTypeKind::Adt(err_adt)`. Multi-field error variants and
    // ambiguous matches are not yet supported.
    let err_variant_idx = find_err_variant(return_adt, err_adt).ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "raise-no-matching-err-variant",
            detail: format!(
                "body {:?} Raises err_adt#{} but no variant of the return ADT {:?} \
                 carries a matching single-field payload",
                cx.body_name,
                err_adt.as_u32(),
                return_adt.name
            ),
        }
    })?;

    // Build the variant aggregate and return it. The byte-preserving
    // path (used on win64 sret bodies) assembles directly in a stack
    // alloca and memcpys into the sret slot — an SSA round-trip
    // through `build_variant_value` would let LLVM's typed
    // load/store decompose the `{ tag, payload }` struct, dropping
    // cross-variant padding bytes (the same loss that breaks the
    // `Ok(double)` path through a `{ i8, { i64 } }`-shaped max
    // payload). For non-sret returns we keep the legacy SSA path so
    // existing tests against `ret { i8, ... } %...` still pass on
    // Linux targets.
    let value_clone = value.clone();
    if body_uses_sret(cx.body, cx.program, cx.arch, cx.os) {
        let sret_ptr = cx.locals[LocalId::RETURN_SLOT.index()]
            .expect("sret-returning body has its return slot bound to the sret pointer");
        let (src_ptr, outer_ty) = build_variant_in_alloca(
            return_ty,
            return_adt_id,
            err_variant_idx,
            &[value_clone],
            cx,
        )?;
        let size_val = outer_ty
            .size_of()
            .expect("aggregate Raise sum type has a sizeof");
        cx.builder
            .build_memcpy(
                sret_ptr,
                AGGREGATE_COPY_ALIGN,
                src_ptr,
                AGGREGATE_COPY_ALIGN,
                size_val,
            )
            .expect("build_memcpy of Raise variant into sret slot");
        cx.builder
            .build_return(None)
            .expect("build_return(None) for sret-returning Raise");
    } else {
        let variant_val =
            build_variant_value(return_ty, return_adt_id, err_variant_idx, &[value_clone], cx)?;
        cx.builder
            .build_return(Some(&variant_val))
            .expect("build_return of Raise's sum value in positioned block");
    }
    Ok(())
}

/// Find which variant of a Result-shaped sum carries an `Adt(err_adt)`
/// single-field payload. Used by [`lower_raise`] to identify the Err
/// variant. Returns `None` if no variant matches.
fn find_err_variant(return_adt: &edda_mir::AdtDef, err_adt: AdtId) -> Option<VariantIdx> {
    for (i, variant) in return_adt.variants.iter().enumerate() {
        if variant.fields.len() != 1 {
            continue;
        }
        if let MirTypeKind::Adt(payload_id) = &variant.fields[0].ty.kind {
            if *payload_id == err_adt {
                return Some(VariantIdx::new(i));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::Emitter;
    use edda_intern::Interner;
    use edda_mir::{
        BodyBuilder, CallArg, CallMode, FuncRef, MirPrim, MirType, MirTypeKind, Operand,
        ParamMode, Place, ProgramBuilder, Terminator, TerminatorKind,
    };
    use edda_span::Span;

    use super::super::super::test_fixtures::linux_x86_64;

    /// Helper: build a Result-shaped sum `{Ok: T, Err: E}` and the
    /// corresponding err ADT `E`. Returns
    /// `(program_builder, result_adt, err_adt, ok_payload_ty)`.
    fn build_result_adt(
        interner: &Interner,
        ok_payload: MirType,
    ) -> (edda_mir::ProgramBuilder, edda_mir::AdtId, edda_mir::AdtId, MirType) {
        let mut pb = ProgramBuilder::new();
        let err_name = interner.intern("MyErr");
        let payload_name = interner.intern("0");
        let err_adt = pb.push_adt(edda_mir::AdtDef {
            name: err_name,
            span: Span::DUMMY,
            kind: edda_mir::AdtKind::Product,
            variants: vec![edda_mir::VariantDef {
                name: err_name,
                span: Span::DUMMY,
                fields: vec![edda_mir::FieldDef {
                    name: payload_name,
                    span: Span::DUMMY,
                    ty: MirType::prim(MirPrim::I32),
                }],
                discriminant: None,
            }],
            layout: edda_mir::LayoutInfo::natural(),
            tag_width: None,
        });
        let result_name = interner.intern("Result");
        let ok_name = interner.intern("Ok");
        let err_variant_name = interner.intern("Err");
        let result_adt = pb.push_adt(edda_mir::AdtDef {
            name: result_name,
            span: Span::DUMMY,
            kind: edda_mir::AdtKind::Sum,
            variants: vec![
                edda_mir::VariantDef {
                    name: ok_name,
                    span: Span::DUMMY,
                    fields: vec![edda_mir::FieldDef {
                        name: payload_name,
                        span: Span::DUMMY,
                        ty: ok_payload.clone(),
                    }],
                    discriminant: Some(0),
                },
                edda_mir::VariantDef {
                    name: err_variant_name,
                    span: Span::DUMMY,
                    fields: vec![edda_mir::FieldDef {
                        name: payload_name,
                        span: Span::DUMMY,
                        ty: MirType::new(MirTypeKind::Adt(err_adt)),
                    }],
                    discriminant: Some(1),
                },
            ],
            layout: edda_mir::LayoutInfo::natural(),
            tag_width: Some(MirPrim::U8),
        });
        (pb, result_adt, err_adt, ok_payload)
    }

    /// `fn always_fails(e: MyErr) -> Result<i32, MyErr> { raise e }`
    /// — Raise builds the Err variant of the body's return-Result and
    /// returns the sum directly.
    #[test]
    fn raise_builds_err_variant_and_returns_sum() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (mut pb, result_adt, err_adt, _ok_payload) =
            build_result_adt(&interner, MirType::prim(MirPrim::I32));
        let result_ty = MirType::new(MirTypeKind::Adt(result_adt));
        let err_ty = MirType::new(MirTypeKind::Adt(err_adt));

        let name = interner.intern("always_fails");
        let mut bb = BodyBuilder::new(name, Span::DUMMY, result_ty.clone());
        // Source-like body → `linkonce_odr` symbol.
        bb.set_qualified_name(name);
        let _ret_local = bb.return_slot(result_ty.clone(), Span::DUMMY);
        let e = bb.param(ParamMode::Let, err_ty, Span::DUMMY);

        // Single-block body whose terminator is Raise.
        let block = bb.block();
        let block_id = block.id();
        block.terminate(Terminator {
            span: Span::DUMMY,
            kind: TerminatorKind::Raise {
                err_adt,
                value: Operand::Copy(Place::local(e)),
            },
        });
        bb.set_entry(block_id);
        let body = bb.finish();
        let _body_id = pb.push_body(body);
        let program = pb.finish();

        let module = emitter
            .lower_program("raise_mod", &target, &program, &interner)
            .expect("Raise must lower");
        let ir = module.print_to_string().to_string();

        // The function signature: returns the Result sum
        // `{ tag: i8, payload: { i32 } }` (Err payload struct is one
        // i32 wide for MyErr).
        assert!(
            ir.contains("define linkonce_odr { i8, { i32 } } @always_fails("),
            "expected Result sum return type in signature: {ir}",
        );
        // Tag store for Err is discriminant 1.
        assert!(
            ir.contains("store i8 1,"),
            "expected `store i8 1` for the Err variant tag: {ir}",
        );
        // A final `ret { i8, { i32 } } %...` instruction.
        assert!(
            ir.contains("ret { i8, { i32 } }"),
            "expected `ret` of the assembled Result sum: {ir}",
        );
    }

    /// `Call f -> R; on Ok continue, on Err raise` — verifies the
    /// conditional branch on the call result's tag.
    #[test]
    fn call_with_on_error_emits_tag_branch() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (mut pb, result_adt, err_adt, _ok_payload) =
            build_result_adt(&interner, MirType::prim(MirPrim::I32));
        let result_ty = MirType::new(MirTypeKind::Adt(result_adt));
        let err_ty = MirType::new(MirTypeKind::Adt(err_adt));

        // Callee: `fn maybe() -> Result<i32, MyErr> { raise <synthesized> }`.
        // We need a body that *returns* a Result. For test purposes
        // the simplest is to raise (which returns the Err sum).
        let callee_body = {
            let name = interner.intern("maybe");
            let mut bb = BodyBuilder::new(name, Span::DUMMY, result_ty.clone());
            let _ = bb.return_slot(result_ty.clone(), Span::DUMMY);
            let e = bb.param(ParamMode::Let, err_ty.clone(), Span::DUMMY);
            let block = bb.block();
            let block_id = block.id();
            block.terminate(Terminator {
                span: Span::DUMMY,
                kind: TerminatorKind::Raise {
                    err_adt,
                    value: Operand::Copy(Place::local(e)),
                },
            });
            bb.set_entry(block_id);
            bb.finish()
        };
        let callee_id = pb.push_body(callee_body);

        // Caller: `fn try_it(e: MyErr) -> Result<i32, MyErr> {
        //   let r = maybe(e)?; return Ok(r);
        // }`
        let caller_body = {
            let name = interner.intern("try_it");
            let mut bb = BodyBuilder::new(name, Span::DUMMY, result_ty.clone());
            let ret_local = bb.return_slot(result_ty.clone(), Span::DUMMY);
            let e = bb.param(ParamMode::Let, err_ty.clone(), Span::DUMMY);

            // bb0: entry — Call(maybe, on_error: bb_err, target: bb_ok)
            let entry_placeholder = bb.block();
            let entry_id = entry_placeholder.id();
            entry_placeholder.unreachable(Span::DUMMY);

            // bb_ok: success continuation, just return what was written
            // into ret_local (the full Result that the call wrote).
            let ok_block = bb.block();
            let ok_id = ok_block.id();
            ok_block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));

            // bb_err: error continuation — Raise the caller's err
            // through the caller's own Result.
            let err_block = bb.block();
            let err_id = err_block.id();
            err_block.terminate(Terminator {
                span: Span::DUMMY,
                kind: TerminatorKind::Raise {
                    err_adt,
                    value: Operand::Copy(Place::local(e)),
                },
            });

            bb.set_entry(entry_id);
            let mut body = bb.finish();
            body.blocks[entry_id].terminator = Terminator {
                span: Span::DUMMY,
                kind: TerminatorKind::Call {
                    func: FuncRef::Body(callee_id),
                    args: vec![CallArg {
                        mode: CallMode::Read,
                        operand: Operand::Copy(Place::local(e)),
                    }],
                    capabilities: Vec::new(),
                    destination: Place::local(ret_local),
                    target: ok_id,
                    on_error: Some(err_id),
                },
            };
            body
        };
        let _caller_id = pb.push_body(caller_body);
        let program = pb.finish();

        let module = emitter
            .lower_program("propagate_mod", &target, &program, &interner)
            .expect("`?`-propagating Call must lower");
        let ir = module.print_to_string().to_string();

        // The conditional branch on the tag — caller inspects the
        // call result's field 0 (the i8 tag) and `icmp eq` against 0.
        assert!(
            ir.contains("icmp eq i8") || ir.contains("icmp eq"),
            "expected an icmp eq instruction comparing the tag against 0: {ir}",
        );
        // br i1 ... should follow the comparison.
        assert!(
            ir.contains("br i1"),
            "expected a conditional branch on the tag comparison: {ir}",
        );
    }
}
