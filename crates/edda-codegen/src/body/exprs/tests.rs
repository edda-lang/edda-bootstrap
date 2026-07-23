//! Tests for the expression / statement / pattern encoders.

use super::*;
use crate::body::tags;
use crate::body::test_support::{expr, ident, pat, path, ty, PassThroughResolver};
use edda_intern::Interner;
use edda_span::Span;
use edda_syntax::ast::{
    AssignOp, BinOp, BindingMode, Block, CallArg, CallMode, ExprKind, Literal, MatchArm,
    PatKind, RangeKind, Stmt, StmtKind, StructLitField, StructPatField, TypeKind, UnOp,
    VariantPatPayload,
};

fn empty_block() -> Block {
    Block {
        span: edda_span::Span::DUMMY,
        stmts: vec![],
        trailing: None,
    }
}

#[test]
fn literal_expr_writes_tag_then_literal() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_expr(&expr(ExprKind::Literal(Literal::Unit)));
    assert_eq!(
        enc.into_bytes(),
        vec![tags::expr_kind::LITERAL, tags::literal::UNIT],
    );
}

#[test]
fn path_expr_uses_resolver() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_expr(&expr(ExprKind::Path(path(&interner, &["a", "b"]))));
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::expr_kind::PATH);
    assert_eq!(&bytes[1..5], &("a.b".len() as u32).to_le_bytes());
    assert_eq!(&bytes[5..], b"a.b");
}

#[test]
fn binary_expr_writes_op_lhs_rhs() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_expr(&expr(ExprKind::Binary {
        op: BinOp::Add,
        lhs: Box::new(expr(ExprKind::Literal(Literal::Unit))),
        rhs: Box::new(expr(ExprKind::Literal(Literal::Unit))),
    }));
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::expr_kind::BINARY);
    assert_eq!(bytes[1], tags::bin_op::ADD);
    assert_eq!(bytes[2], tags::expr_kind::LITERAL);
    assert_eq!(bytes[3], tags::literal::UNIT);
    assert_eq!(bytes[4], tags::expr_kind::LITERAL);
    assert_eq!(bytes[5], tags::literal::UNIT);
}

#[test]
fn unary_expr_writes_op_then_operand() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_expr(&expr(ExprKind::Unary {
        op: UnOp::Neg,
        expr: Box::new(expr(ExprKind::Literal(Literal::Unit))),
    }));
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::expr_kind::UNARY);
    assert_eq!(bytes[1], tags::un_op::NEG);
    assert_eq!(bytes[2], tags::expr_kind::LITERAL);
    assert_eq!(bytes[3], tags::literal::UNIT);
}

#[test]
fn call_expr_writes_callee_then_args() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_expr(&expr(ExprKind::Call {
        callee: Box::new(expr(ExprKind::Path(path(&interner, &["f"])))),
        args: vec![
            CallArg::bare(expr(ExprKind::Literal(Literal::Unit))),
            CallArg::bare(expr(ExprKind::Literal(Literal::Bool(true)))),
        ],
    }));
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::expr_kind::CALL);
    // callee: PATH "f"
    assert_eq!(bytes[1], tags::expr_kind::PATH);
    assert_eq!(&bytes[2..6], &1u32.to_le_bytes());
    assert_eq!(bytes[6], b'f');
    // arg count = 2
    assert_eq!(&bytes[7..11], &2u32.to_le_bytes());
    // first arg: NONE mode tag, then LITERAL+UNIT
    assert_eq!(bytes[11], tags::call_mode::NONE);
    assert_eq!(bytes[12], tags::expr_kind::LITERAL);
    assert_eq!(bytes[13], tags::literal::UNIT);
    // second arg: NONE mode tag, then LITERAL+BOOL+0x01
    assert_eq!(bytes[14], tags::call_mode::NONE);
    assert_eq!(bytes[15], tags::expr_kind::LITERAL);
    assert_eq!(bytes[16], tags::literal::BOOL);
}

#[test]
fn call_arg_modes_round_trip_into_bytes() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_expr(&expr(ExprKind::Call {
        callee: Box::new(expr(ExprKind::Path(path(&interner, &["f"])))),
        args: vec![
            CallArg {
                span: Span::DUMMY,
                mode: Some(CallMode::Mutable),
                name: None,
                expr: expr(ExprKind::Literal(Literal::Unit)),
            },
            CallArg {
                span: Span::DUMMY,
                mode: Some(CallMode::Take),
                name: None,
                expr: expr(ExprKind::Literal(Literal::Unit)),
            },
            CallArg {
                span: Span::DUMMY,
                mode: Some(CallMode::Init),
                name: None,
                expr: expr(ExprKind::Literal(Literal::Unit)),
            },
        ],
    }));
    let bytes = enc.into_bytes();
    // Prefix breakdown:
    //   CALL(1) + PATH(1) + u32-len(4) + "f"(1) + u32-arg-count(4) = 11 bytes
    // before the first argument.
    let arg_start = 11;
    // arg 0 = INOUT + LITERAL + UNIT (3 bytes per arg)
    assert_eq!(bytes[arg_start], tags::call_mode::INOUT);
    // arg 1 = SINK + LITERAL + UNIT
    assert_eq!(bytes[arg_start + 3], tags::call_mode::SINK);
    // arg 2 = SET + LITERAL + UNIT
    assert_eq!(bytes[arg_start + 6], tags::call_mode::SET);
}

#[test]
fn method_call_writes_receiver_name_args() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_expr(&expr(ExprKind::MethodCall {
        receiver: Box::new(expr(ExprKind::Path(path(&interner, &["x"])))),
        name: ident(&interner, "push"),
        args: vec![],
    }));
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::expr_kind::METHOD_CALL);
    // bytes[1..] = expr PATH "x", then ident "push", then 0 args
    // PATH(1) + len(4) + "x"(1) = 6 bytes
    // ident: len(4) + "push"(4) = 8 bytes
    // args: count(4) = 4 bytes (no per-arg bytes since len == 0)
    assert_eq!(bytes.len(), 1 + 6 + 8 + 4);
    assert_eq!(bytes[1], tags::expr_kind::PATH);
}

#[test]
fn if_expr_else_branch_optionality_is_load_bearing() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mk = |else_branch| {
        ExprKind::If {
            cond: Box::new(expr(ExprKind::Literal(Literal::Bool(true)))),
            then_block: empty_block(),
            else_branch,
        }
    };
    let mut with_else = Encoder::new(&interner, &resolver);
    let mut without = Encoder::new(&interner, &resolver);
    with_else.write_expr(&expr(mk(Some(Box::new(expr(ExprKind::Block(
        empty_block(),
    )))))));
    without.write_expr(&expr(mk(None)));
    assert_ne!(with_else.into_bytes(), without.into_bytes());
}

#[test]
fn match_arm_writes_pat_guard_body() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_expr(&expr(ExprKind::Match {
        scrutinee: Box::new(expr(ExprKind::Literal(Literal::Unit))),
        arms: vec![MatchArm {
            span: edda_span::Span::DUMMY,
            pat: pat(PatKind::Wildcard),
            guard: None,
            body: expr(ExprKind::Literal(Literal::Unit)),
        }],
    }));
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::expr_kind::MATCH);
    assert_eq!(bytes[1], tags::expr_kind::LITERAL);
    assert_eq!(bytes[2], tags::literal::UNIT);
    assert_eq!(&bytes[3..7], &1u32.to_le_bytes()); // arms count
    assert_eq!(bytes[7], tags::pat_kind::WILDCARD);
    assert_eq!(bytes[8], tags::option_flag::NONE); // guard absent
    assert_eq!(bytes[9], tags::expr_kind::LITERAL);
    assert_eq!(bytes[10], tags::literal::UNIT);
}

#[test]
fn block_encodes_stmt_count_and_trailing_flag() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_block(&Block {
        span: edda_span::Span::DUMMY,
        stmts: vec![Stmt { attributes: Vec::new(),
            span: edda_span::Span::DUMMY,
            kind: StmtKind::Expr(expr(ExprKind::Literal(Literal::Unit))),
        }],
        trailing: Some(Box::new(expr(ExprKind::Literal(Literal::Bool(true))))),
    });
    let bytes = enc.into_bytes();
    // stmt count = 1
    assert_eq!(&bytes[0..4], &1u32.to_le_bytes());
    // StmtKind::Expr tag, expr literal unit
    assert_eq!(bytes[4], tags::stmt_kind::EXPR);
    assert_eq!(bytes[5], tags::expr_kind::LITERAL);
    assert_eq!(bytes[6], tags::literal::UNIT);
    // trailing present
    assert_eq!(bytes[7], tags::option_flag::SOME);
    assert_eq!(bytes[8], tags::expr_kind::LITERAL);
    assert_eq!(bytes[9], tags::literal::BOOL);
    assert_eq!(bytes[10], 0x01);
}

#[test]
fn let_stmt_with_type_and_init() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_stmt(&Stmt { attributes: Vec::new(),
        span: edda_span::Span::DUMMY,
        kind: StmtKind::Let {
            mutability: BindingMode::Immutable,
            pat: pat(PatKind::Binding(ident(&interner, "x"))),
            ty: Some(ty(TypeKind::Unit)),
            init: Some(expr(ExprKind::Literal(Literal::Unit))),
        },
    });
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::stmt_kind::LET);
    assert_eq!(bytes[1], tags::binding_mode::IMMUTABLE);
    assert_eq!(bytes[2], tags::pat_kind::BINDING);
    // pat ident "x": len(4) + 1
    assert_eq!(&bytes[3..7], &1u32.to_le_bytes());
    assert_eq!(bytes[7], b'x');
    // ty present + UNIT
    assert_eq!(bytes[8], tags::option_flag::SOME);
    assert_eq!(bytes[9], tags::type_kind::UNIT);
    // init present + literal unit
    assert_eq!(bytes[10], tags::option_flag::SOME);
    assert_eq!(bytes[11], tags::expr_kind::LITERAL);
    assert_eq!(bytes[12], tags::literal::UNIT);
}

#[test]
fn assign_stmt_writes_target_op_rhs() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_stmt(&Stmt { attributes: Vec::new(),
        span: edda_span::Span::DUMMY,
        kind: StmtKind::Assign {
            target: expr(ExprKind::Path(path(&interner, &["x"]))),
            op: AssignOp::Plain,
            rhs: expr(ExprKind::Literal(Literal::Unit)),
        },
    });
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::stmt_kind::ASSIGN);
    assert_eq!(bytes[1], tags::expr_kind::PATH);
    // skip target bytes
    // After "x" path: 1 (tag) + 4 (len) + 1 (x) = 6 bytes
    assert_eq!(bytes[7], tags::assign_op::PLAIN);
    assert_eq!(bytes[8], tags::expr_kind::LITERAL);
    assert_eq!(bytes[9], tags::literal::UNIT);
}

#[test]
fn pat_wildcard_is_one_byte() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_pat(&pat(PatKind::Wildcard));
    assert_eq!(enc.into_bytes(), vec![tags::pat_kind::WILDCARD]);
}

#[test]
fn pat_struct_writes_rest_flag() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut with_rest = Encoder::new(&interner, &resolver);
    let mut without = Encoder::new(&interner, &resolver);
    with_rest.write_pat(&pat(PatKind::Struct {
        path: path(&interner, &["S"]),
        fields: vec![],
        rest: true,
    }));
    without.write_pat(&pat(PatKind::Struct {
        path: path(&interner, &["S"]),
        fields: vec![],
        rest: false,
    }));
    assert_ne!(with_rest.into_bytes(), without.into_bytes());
}

#[test]
fn variant_pat_payload_tags() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut a = Encoder::new(&interner, &resolver);
    let mut b = Encoder::new(&interner, &resolver);
    let mut c = Encoder::new(&interner, &resolver);
    a.write_variant_pat_payload(&VariantPatPayload::None);
    b.write_variant_pat_payload(&VariantPatPayload::Tuple(vec![]));
    c.write_variant_pat_payload(&VariantPatPayload::Struct(vec![]));
    assert_eq!(a.into_bytes(), vec![tags::variant_pat_payload::NONE]);
    let b_bytes = b.into_bytes();
    assert_eq!(b_bytes[0], tags::variant_pat_payload::TUPLE);
    assert_eq!(&b_bytes[1..5], &0u32.to_le_bytes());
    let c_bytes = c.into_bytes();
    assert_eq!(c_bytes[0], tags::variant_pat_payload::STRUCT);
    assert_eq!(&c_bytes[1..5], &0u32.to_le_bytes());
}

#[test]
fn struct_lit_writes_path_and_fields() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_expr(&expr(ExprKind::StructLit {
        path: path(&interner, &["Point"]),
        fields: vec![StructLitField {
            span: edda_span::Span::DUMMY,
            name: ident(&interner, "x"),
            mode: None,
            value: expr(ExprKind::Literal(Literal::Unit)),
        }],
    }));
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::expr_kind::STRUCT_LIT);
    // path "Point": len(4) + 5 = 9 bytes after tag
    assert_eq!(&bytes[1..5], &("Point".len() as u32).to_le_bytes());
    // After path: field count = 1
    let after_path = 1 + 4 + 5;
    assert_eq!(&bytes[after_path..after_path + 4], &1u32.to_le_bytes());
    // After field count: ident "x" then expr literal unit
    let after_count = after_path + 4;
    assert_eq!(&bytes[after_count..after_count + 4], &1u32.to_le_bytes());
    assert_eq!(bytes[after_count + 4], b'x');
}

#[test]
fn range_expr_writes_endpoints_and_kind() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_expr(&expr(ExprKind::Range {
        lo: Some(Box::new(expr(ExprKind::Literal(Literal::Unit)))),
        hi: Some(Box::new(expr(ExprKind::Literal(Literal::Unit)))),
        kind: RangeKind::Closed,
    }));
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::expr_kind::RANGE);
    assert_eq!(bytes[1], tags::expr_kind::LITERAL);
    assert_eq!(bytes[2], tags::literal::UNIT);
    assert_eq!(bytes[3], tags::expr_kind::LITERAL);
    assert_eq!(bytes[4], tags::literal::UNIT);
    assert_eq!(bytes[5], tags::range_kind::CLOSED);
}

#[test]
fn return_value_optional() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut empty = Encoder::new(&interner, &resolver);
    let mut with_value = Encoder::new(&interner, &resolver);
    empty.write_expr(&expr(ExprKind::Return(None)));
    with_value.write_expr(&expr(ExprKind::Return(Some(Box::new(expr(
        ExprKind::Literal(Literal::Unit),
    ))))));
    assert_eq!(
        empty.into_bytes(),
        vec![tags::expr_kind::RETURN, tags::option_flag::NONE],
    );
    let with_bytes = with_value.into_bytes();
    assert_eq!(with_bytes[0], tags::expr_kind::RETURN);
    assert_eq!(with_bytes[1], tags::option_flag::SOME);
    assert_eq!(with_bytes[2], tags::expr_kind::LITERAL);
    assert_eq!(with_bytes[3], tags::literal::UNIT);
}

#[test]
fn struct_pat_field_writes_name_then_pat() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_struct_pat_field(&StructPatField {
        span: edda_span::Span::DUMMY,
        name: ident(&interner, "y"),
        pat: pat(PatKind::Wildcard),
    });
    let bytes = enc.into_bytes();
    // ident len + "y" + WILDCARD
    assert_eq!(&bytes[0..4], &1u32.to_le_bytes());
    assert_eq!(bytes[4], b'y');
    assert_eq!(bytes[5], tags::pat_kind::WILDCARD);
}

#[test]
fn break_label_value_independently_optional() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let label = Some(ident(&interner, "outer"));
    let mut e1 = Encoder::new(&interner, &resolver);
    let mut e2 = Encoder::new(&interner, &resolver);
    e1.write_expr(&expr(ExprKind::Break {
        label,
        value: None,
    }));
    e2.write_expr(&expr(ExprKind::Break {
        label: None,
        value: None,
    }));
    // Same kind, different label presence → different bytes.
    assert_ne!(e1.into_bytes(), e2.into_bytes());
}
