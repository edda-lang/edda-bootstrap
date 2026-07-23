//! Spec-invocation name mangling and module-disambiguation hashing.
//!
//! The resolver-side reciprocal of edda_codegen::mangle — must stay byte-for-byte
//! in lock-step so pass-2 resolution finds the cascade-emitted generated modules.
//! Split out of items.rs (which keeps the item-table construction).
//!
//! Decomposed by sub-concept: short-name mangling ([`name`]), disambig-hash +
//! qname resolution ([`disambig`]), and generated-leaf composition
//! ([`generated`]). The shared `MODULE_DISAMBIG_VERSION` byte lives here.

mod disambig;
mod generated;
mod name;

pub use disambig::{module_disambig_hex_for_args, module_disambig_hex_from_ast};
pub use generated::{mangle_spec_invocation_generated_leaf, spec_invocation_module_leaf};
pub use name::mangle_spec_invocation_name;

pub(super) const MODULE_DISAMBIG_VERSION: u8 = 0x01;

#[cfg(test)]
mod tests {
    use super::*;
    use edda_intern::{Interner, Symbol};
    use edda_span::Span;
    use edda_syntax::ast::{Expr, ExprKind, Literal, SpecInvocation};
    use edda_syntax::ast::{Ident, Path as AstPath};

    fn ident(interner: &Interner, name: &str) -> Ident {
        Ident { name: interner.intern(name), span: Span::DUMMY }
    }

    fn dummy_ident() -> Ident {
        Ident { name: Symbol::DUMMY, span: Span::DUMMY }
    }

    #[test]
    fn mangle_returns_none_when_spec_path_head_is_dummy() {
        // Parser-recovery placeholder: spec path's last segment carries
        // `Symbol::DUMMY`. The mangler must bail rather than reaching
        // `interner.resolve(DUMMY)` (which panics).
        let interner = Interner::new();
        let si = SpecInvocation {
            span: Span::DUMMY,
            path: AstPath { segments: vec![dummy_ident()], span: Span::DUMMY },
            args: Vec::new(),
        };
        assert_eq!(mangle_spec_invocation_name(&si, &interner), None);
    }

    #[test]
    fn mangle_returns_none_when_arg_path_leaf_is_dummy() {
        // Spec path is well-formed but an argument carries a DUMMY leaf
        // (recovery from a malformed arg expression). Bail.
        let interner = Interner::new();
        let arg = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(AstPath {
                segments: vec![dummy_ident()],
                span: Span::DUMMY,
            }),
        };
        let si = SpecInvocation {
            span: Span::DUMMY,
            path: AstPath {
                segments: vec![ident(&interner, "Box")],
                span: Span::DUMMY,
            },
            args: vec![arg],
        };
        assert_eq!(mangle_spec_invocation_name(&si, &interner), None);
    }

    #[test]
    fn mangle_admits_integer_literal_arg() {
        // `spec std.collections.array.Array(u8, 32)` — the second arg is an
        // integer literal. Must produce `Array_u8_32` to match the codegen
        // mangler so the SpecInvocation binding name lines up with the
        // generated module's leaf.
        let interner = Interner::new();
        let u8_arg = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(AstPath {
                segments: vec![ident(&interner, "u8")],
                span: Span::DUMMY,
            }),
        };
        let n_arg = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Literal(Literal::Int {
                value: 32,
                base: edda_syntax::IntBase::Dec,
            }),
        };
        let si = SpecInvocation {
            span: Span::DUMMY,
            path: AstPath {
                segments: vec![
                    ident(&interner, "std"),
                    ident(&interner, "collections"),
                    ident(&interner, "array"),
                    ident(&interner, "Array"),
                ],
                span: Span::DUMMY,
            },
            args: vec![u8_arg, n_arg],
        };
        let mangled = mangle_spec_invocation_name(&si, &interner)
            .expect("integer literal arg must mangle");
        assert_eq!(interner.resolve(mangled), "Array_u8_32");
    }

    #[test]
    fn mangle_path_qualifies_multi_segment_args() {
        // `Vec(Vec_String.Vec)` must mangle
        // to `Vec_Vec_String_Vec` (segments joined with `_`), not the
        // leaf-only `Vec_Vec`. Two distinct nested-Vec invocations then
        // produce distinct binding names instead of colliding.
        let interner = Interner::new();
        let arg_string = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(AstPath {
                segments: vec![ident(&interner, "Vec_String"), ident(&interner, "Vec")],
                span: Span::DUMMY,
            }),
        };
        let arg_usize = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(AstPath {
                segments: vec![ident(&interner, "Vec_usize"), ident(&interner, "Vec")],
                span: Span::DUMMY,
            }),
        };
        let si_string = SpecInvocation {
            span: Span::DUMMY,
            path: AstPath {
                segments: vec![
                    ident(&interner, "std"),
                    ident(&interner, "collections"),
                    ident(&interner, "vec"),
                    ident(&interner, "Vec"),
                ],
                span: Span::DUMMY,
            },
            args: vec![arg_string],
        };
        let si_usize = SpecInvocation {
            span: Span::DUMMY,
            path: AstPath {
                segments: vec![
                    ident(&interner, "std"),
                    ident(&interner, "collections"),
                    ident(&interner, "vec"),
                    ident(&interner, "Vec"),
                ],
                span: Span::DUMMY,
            },
            args: vec![arg_usize],
        };
        let m_string = mangle_spec_invocation_name(&si_string, &interner)
            .expect("multi-segment arg must mangle");
        let m_usize = mangle_spec_invocation_name(&si_usize, &interner)
            .expect("multi-segment arg must mangle");
        assert_eq!(interner.resolve(m_string), "Vec_Vec_String_Vec");
        assert_eq!(interner.resolve(m_usize), "Vec_Vec_usize_Vec");
        assert_ne!(m_string, m_usize);
    }

    #[test]
    fn mangle_strips_lowercase_module_prefix_for_fully_qualified_args() {
        // `Option(probe78.main.MyRow)` must
        // mangle to `Option_MyRow` (matching codegen's `type_leaf_mangle`),
        // NOT `Option_probe78_main_MyRow`. Otherwise the resolver-side
        // SpecInvocation binding name disagrees with the cascade-emitted
        // artifact short name, and Vec_MyRow.ea body references to
        // `Option_MyRow` fail to resolve.
        let interner = Interner::new();
        let arg_my_row = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(AstPath {
                segments: vec![
                    ident(&interner, "probe78"),
                    ident(&interner, "main"),
                    ident(&interner, "MyRow"),
                ],
                span: Span::DUMMY,
            }),
        };
        let si = SpecInvocation {
            span: Span::DUMMY,
            path: AstPath {
                segments: vec![
                    ident(&interner, "std"),
                    ident(&interner, "core"),
                    ident(&interner, "option"),
                    ident(&interner, "Option"),
                ],
                span: Span::DUMMY,
            },
            args: vec![arg_my_row],
        };
        let mangled = mangle_spec_invocation_name(&si, &interner)
            .expect("fully-qualified path arg must mangle");
        assert_eq!(interner.resolve(mangled), "Option_MyRow");
    }

    #[test]
    fn mangle_keeps_leaf_only_for_single_segment_arg() {
        // A single-segment arg
        // (`Vec(String)`) must produce the leaf-only
        // mangle `Vec_String` — both before and after the nested-Vec
        // disambiguation fix, the single-segment path bypasses the
        // strip-lowercase loop entirely. This test locks that surface in
        // so a future refactor of the loop cannot regress single-segment
        // primitive args back to the `Vec_String` → something-else shape
        // that once flooded `Vec_u8` / `Vec_String` consumers.
        let interner = Interner::new();
        let arg_string = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(AstPath {
                segments: vec![ident(&interner, "String")],
                span: Span::DUMMY,
            }),
        };
        let si = SpecInvocation {
            span: Span::DUMMY,
            path: AstPath {
                segments: vec![
                    ident(&interner, "std"),
                    ident(&interner, "collections"),
                    ident(&interner, "vec"),
                    ident(&interner, "Vec"),
                ],
                span: Span::DUMMY,
            },
            args: vec![arg_string],
        };
        let mangled = mangle_spec_invocation_name(&si, &interner)
            .expect("single-segment arg must mangle");
        assert_eq!(interner.resolve(mangled), "Vec_String");
    }

    #[test]
    fn mangle_keeps_leaf_only_for_kind_module_qualified_arg() {
        // `Option(kind_mod.Token)`
        // must mangle to `Option_Token` — the leaf-only form — because
        // `kind_mod` is a lowercase Edda module segment with no
        // collision-disambiguation role. Before the nested-Vec disambiguation
        // fix, the leaf-only behavior here was a happy accident of
        // always-taking-last-segment; now it is structurally guaranteed by
        // the strip-lowercase rule. Two distinct module paths with the same type-suffix
        // (`Token`) refer to one nominal type after resolution, so they
        // SHOULD share a mangled form.
        let interner = Interner::new();
        let arg = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(AstPath {
                segments: vec![ident(&interner, "kind_mod"), ident(&interner, "Token")],
                span: Span::DUMMY,
            }),
        };
        let si = SpecInvocation {
            span: Span::DUMMY,
            path: AstPath {
                segments: vec![
                    ident(&interner, "std"),
                    ident(&interner, "core"),
                    ident(&interner, "option"),
                    ident(&interner, "Option"),
                ],
                span: Span::DUMMY,
            },
            args: vec![arg],
        };
        let mangled = mangle_spec_invocation_name(&si, &interner)
            .expect("module-qualified arg must mangle");
        assert_eq!(interner.resolve(mangled), "Option_Token");
    }

    #[test]
    fn mangle_nested_vec_args_with_spec_mangled_intermediate_stay_distinct() {
        // `Vec(Vec_String.Vec)` and
        // `Vec(Vec_usize.Vec)` are the collision-prone shape — the
        // strip-lowercase loop terminates at the first non-lowercase head
        // (`Vec_String` / `Vec_usize`), so the intermediate spec-mangled
        // segment is retained and the two args mangle distinctly. This
        // is the same property the nested-Vec disambiguation fix originally
        // addressed; restating it as a dedicated regression test guards
        // against any future "simplification" that re-introduces the
        // `Vec_Vec` collision.
        let interner = Interner::new();
        let arg_string = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(AstPath {
                segments: vec![ident(&interner, "Vec_String"), ident(&interner, "Vec")],
                span: Span::DUMMY,
            }),
        };
        let arg_usize = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(AstPath {
                segments: vec![ident(&interner, "Vec_usize"), ident(&interner, "Vec")],
                span: Span::DUMMY,
            }),
        };
        let mk = |arg| SpecInvocation {
            span: Span::DUMMY,
            path: AstPath {
                segments: vec![
                    ident(&interner, "std"),
                    ident(&interner, "collections"),
                    ident(&interner, "vec"),
                    ident(&interner, "Vec"),
                ],
                span: Span::DUMMY,
            },
            args: vec![arg],
        };
        let m_string = mangle_spec_invocation_name(&mk(arg_string), &interner)
            .expect("nested-Vec_String arg must mangle");
        let m_usize = mangle_spec_invocation_name(&mk(arg_usize), &interner)
            .expect("nested-Vec_usize arg must mangle");
        assert_ne!(
            m_string, m_usize,
            "collision-prone nested-Vec invocations must mangle to distinct names",
        );
        assert_eq!(interner.resolve(m_string), "Vec_Vec_String_Vec");
        assert_eq!(interner.resolve(m_usize), "Vec_Vec_usize_Vec");
    }

    #[test]
    fn mangle_admits_bool_literal_arg() {
        let interner = Interner::new();
        let arg_true = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Literal(Literal::Bool(true)),
        };
        let si = SpecInvocation {
            span: Span::DUMMY,
            path: AstPath {
                segments: vec![ident(&interner, "Flag")],
                span: Span::DUMMY,
            },
            args: vec![arg_true],
        };
        let mangled =
            mangle_spec_invocation_name(&si, &interner).expect("bool literal must mangle");
        assert_eq!(interner.resolve(mangled), "Flag_true");
    }
}
