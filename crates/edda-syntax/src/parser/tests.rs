//! End-to-end tests for the expression-level parser. Each test lexes a
//! source snippet, parses it as an expression or a block, and asserts on
//! the resulting AST. Diagnostic-free parsing is checked by inspecting
//! the [`Diagnostics`] take.

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_span::SourceMap;
use std::path::PathBuf;

use crate::ast::{
    AssignOp, BinOp, BindingMode, Expr, ExprKind, FStringPart, Literal, PatKind, RangeKind,
    StmtKind, TypeKind, UnOp,
};
use crate::lexer::lex;
use crate::parser::{parse_block, parse_expr};
use crate::token::IntBase;

fn parse_expr_str(src: &str) -> (Expr, Diagnostics) {
    let map = SourceMap::new();
    let file = map.add_file(PathBuf::from("test.ea"), src.to_string());
    let interner = Interner::new();
    let mut diags = Diagnostics::new();
    let cfg = LintConfig::new();
    let tokens = lex(src, file, &interner, &mut diags, &cfg);
    let expr = parse_expr(&tokens, &interner, &mut diags, &cfg);
    (expr, diags)
}

fn parse_expr_str_with_interner(src: &str) -> (Expr, Interner, Diagnostics) {
    let map = SourceMap::new();
    let file = map.add_file(PathBuf::from("test.ea"), src.to_string());
    let interner = Interner::new();
    let mut diags = Diagnostics::new();
    let cfg = LintConfig::new();
    let tokens = lex(src, file, &interner, &mut diags, &cfg);
    let expr = parse_expr(&tokens, &interner, &mut diags, &cfg);
    (expr, interner, diags)
}

fn parse_block_str(src: &str) -> (crate::ast::Block, Diagnostics) {
    let map = SourceMap::new();
    let file = map.add_file(PathBuf::from("test.ea"), src.to_string());
    let interner = Interner::new();
    let mut diags = Diagnostics::new();
    let cfg = LintConfig::new();
    let tokens = lex(src, file, &interner, &mut diags, &cfg);
    let block = parse_block(&tokens, &interner, &mut diags, &cfg);
    (block, diags)
}

fn unwrap_binary(e: &Expr) -> (BinOp, &Expr, &Expr) {
    match &e.kind {
        ExprKind::Binary { op, lhs, rhs } => (*op, lhs, rhs),
        other => panic!("expected Binary, got {:?}", other),
    }
}

fn unwrap_int(e: &Expr) -> u128 {
    match &e.kind {
        ExprKind::Literal(Literal::Int { value, .. }) => *value,
        other => panic!("expected Int literal, got {:?}", other),
    }
}

#[test]
fn integer_literal() {
    let (e, d) = parse_expr_str("42");
    assert!(!d.has_errors());
    assert_eq!(unwrap_int(&e), 42);
}

#[test]
fn add_left_associative() {
    // 1 + 2 + 3 = (1 + 2) + 3
    let (e, d) = parse_expr_str("1 + 2 + 3");
    assert!(!d.has_errors());
    let (op, lhs, rhs) = unwrap_binary(&e);
    assert_eq!(op, BinOp::Add);
    assert_eq!(unwrap_int(rhs), 3);
    let (op_inner, lhs_inner, rhs_inner) = unwrap_binary(lhs);
    assert_eq!(op_inner, BinOp::Add);
    assert_eq!(unwrap_int(lhs_inner), 1);
    assert_eq!(unwrap_int(rhs_inner), 2);
}

#[test]
fn mul_binds_tighter_than_add() {
    // 1 + 2 * 3 = 1 + (2 * 3)
    let (e, _) = parse_expr_str("1 + 2 * 3");
    let (op, lhs, rhs) = unwrap_binary(&e);
    assert_eq!(op, BinOp::Add);
    assert_eq!(unwrap_int(lhs), 1);
    let (op_r, _, _) = unwrap_binary(rhs);
    assert_eq!(op_r, BinOp::Mul);
}

#[test]
fn unary_neg_binds_tighter_than_mul() {
    // -2 * 3 = (-2) * 3
    let (e, _) = parse_expr_str("-2 * 3");
    let (op, lhs, _) = unwrap_binary(&e);
    assert_eq!(op, BinOp::Mul);
    let ExprKind::Unary { op: u, .. } = &lhs.kind else {
        panic!("expected Unary, got {:?}", lhs.kind);
    };
    assert_eq!(*u, UnOp::Neg);
}

#[test]
fn comparison_is_non_associative() {
    let (_, d) = parse_expr_str("a < b < c");
    assert!(d.has_errors());
}

#[test]
fn cast_binds_between_unary_and_mul() {
    // -x as i32 = (-x) as i32 ; x as i32 * 2 = (x as i32) * 2
    let (e, _) = parse_expr_str("x as i32 * 2");
    let (op, lhs, _) = unwrap_binary(&e);
    assert_eq!(op, BinOp::Mul);
    assert!(matches!(lhs.kind, ExprKind::Cast { .. }));
}

#[test]
fn parens_group() {
    let (e, _) = parse_expr_str("(1 + 2) * 3");
    let (op, _, _) = unwrap_binary(&e);
    assert_eq!(op, BinOp::Mul);
}

#[test]
fn unit_literal() {
    let (e, _) = parse_expr_str("()");
    assert!(matches!(e.kind, ExprKind::Literal(Literal::Unit)));
}

#[test]
fn tuple_two_elements() {
    let (e, _) = parse_expr_str("(1, 2)");
    let ExprKind::Tuple(elems) = &e.kind else {
        panic!("expected Tuple, got {:?}", e.kind);
    };
    assert_eq!(elems.len(), 2);
}

#[test]
fn path_dotted() {
    let (e, _) = parse_expr_str("std.fs.read");
    let ExprKind::Path(p) = &e.kind else {
        panic!("expected Path, got {:?}", e.kind);
    };
    assert_eq!(p.segments.len(), 3);
}

#[test]
fn call_simple() {
    let (e, _) = parse_expr_str("f(1, 2)");
    let ExprKind::Call { callee, args } = &e.kind else {
        panic!("expected Call, got {:?}", e.kind);
    };
    assert!(matches!(callee.kind, ExprKind::Path(_)));
    assert_eq!(args.len(), 2);
}

#[test]
fn method_call_on_complex_receiver() {
    // `(x + y).foo(z)` — the dot after `)` triggers MethodCall.
    let (e, _) = parse_expr_str("(x + y).foo(z)");
    let ExprKind::MethodCall { name, args, .. } = &e.kind else {
        panic!("expected MethodCall, got {:?}", e.kind);
    };
    assert_eq!(args.len(), 1);
    // name resolves to symbol for "foo"
    let _ = name;
}

#[test]
fn try_postfix() {
    let (e, _) = parse_expr_str("f()?");
    let ExprKind::Try(inner) = &e.kind else {
        panic!("expected Try, got {:?}", e.kind);
    };
    assert!(matches!(inner.kind, ExprKind::Call { .. }));
}

#[test]
fn await_postfix() {
    let (e, _) = parse_expr_str("task.await");
    assert!(matches!(e.kind, ExprKind::Await(_)));
}

#[test]
fn range_half_open() {
    let (e, _) = parse_expr_str("0..<n");
    let ExprKind::Range { kind, .. } = &e.kind else {
        panic!("expected Range, got {:?}", e.kind);
    };
    assert_eq!(*kind, RangeKind::HalfOpen);
}

#[test]
fn range_non_associative() {
    let (_, d) = parse_expr_str("a..<b..<c");
    assert!(d.has_errors());
}

#[test]
fn if_then_else() {
    let (e, d) = parse_expr_str("if x > 0 { 1 } else { 2 }");
    assert!(!d.has_errors());
    let ExprKind::If {
        else_branch,
        ..
    } = &e.kind
    else {
        panic!("expected If, got {:?}", e.kind);
    };
    assert!(else_branch.is_some());
}

#[test]
fn if_disables_struct_literal_in_condition() {
    // `if x { 1 }` must parse the brace as the body, not as a
    // struct-literal payload for `x`.
    let (e, d) = parse_expr_str("if x { 1 }");
    assert!(!d.has_errors());
    let ExprKind::If { cond, .. } = &e.kind else {
        panic!("expected If");
    };
    assert!(matches!(cond.kind, ExprKind::Path(_)));
}

#[test]
fn match_two_arms() {
    let src = "match v { case .case_a => 1 case .case_b => 2 }";
    let (e, d) = parse_expr_str(src);
    assert!(!d.has_errors(), "diagnostics: {:?}", d.error_count());
    let ExprKind::Match { arms, .. } = &e.kind else {
        panic!("expected Match");
    };
    assert_eq!(arms.len(), 2);
}

#[test]
fn match_arm_with_guard() {
    let src = "match v { case let x where x > 0 => 1 case _ => 0 }";
    let (e, d) = parse_expr_str(src);
    assert!(!d.has_errors());
    let ExprKind::Match { arms, .. } = &e.kind else {
        panic!("expected Match");
    };
    assert!(arms[0].guard.is_some());
    assert!(matches!(arms[1].pat.kind, PatKind::Wildcard));
}

#[test]
fn or_pattern_binder_free_admitted() {
    let src = "match v { case .case_a | .case_b => 1 case _ => 0 }";
    let (e, d) = parse_expr_str(src);
    assert!(!d.has_errors(), "diagnostics: {:?}", d.error_count());
    let ExprKind::Match { arms, .. } = &e.kind else {
        panic!("expected Match");
    };
    assert_eq!(arms.len(), 3);
}

#[test]
fn or_pattern_matching_binders_admitted() {
    let src = "match v { case .ok(let x) | .also_ok(let x) => x }";
    let (e, d) = parse_expr_str(src);
    assert!(!d.has_errors(), "diagnostics: {:?}", d.error_count());
    let ExprKind::Match { arms, .. } = &e.kind else {
        panic!("expected Match");
    };
    assert_eq!(arms.len(), 2);
    assert!(matches!(arms[0].pat.kind, PatKind::Variant { .. }));
    assert!(matches!(arms[1].pat.kind, PatKind::Variant { .. }));
}

#[test]
fn or_pattern_mismatched_binders_rejected() {
    let src = "match v { case .ok(let x) | .err(let y) => x }";
    let (_, d) = parse_expr_str(src);
    assert!(d.has_errors());
}

#[test]
fn loop_with_break_value() {
    let (e, _) = parse_expr_str("loop { break 42 }");
    let ExprKind::Loop { body, .. } = &e.kind else {
        panic!("expected Loop");
    };
    // The body's trailing expression should be a Break with value.
    let trailing = body.trailing.as_ref().expect("expected trailing in loop body");
    assert!(matches!(
        trailing.kind,
        ExprKind::Break { value: Some(_), .. }
    ));
}

#[test]
fn for_loop() {
    let (e, d) = parse_expr_str("for i in 0..<10 { i }");
    assert!(!d.has_errors());
    assert!(matches!(e.kind, ExprKind::For { .. }));
}

#[test]
fn struct_literal_with_fields() {
    let (e, d) = parse_expr_str("Point { x: 1, y: 2 }");
    assert!(!d.has_errors());
    let ExprKind::StructLit { fields, .. } = &e.kind else {
        panic!("expected StructLit, got {:?}", e.kind);
    };
    assert_eq!(fields.len(), 2);
}

#[test]
fn block_with_let_and_trailing() {
    let src = "{ let x = 1; let y = 2; x + y }";
    let (block, d) = parse_block_str(src);
    assert!(!d.has_errors());
    assert_eq!(block.stmts.len(), 2);
    assert!(block.trailing.is_some());
    let trailing = block.trailing.as_ref().unwrap();
    assert!(matches!(trailing.kind, ExprKind::Binary { .. }));
}

#[test]
fn block_with_var_binding() {
    let (block, d) = parse_block_str("{ var n = 0; n = n + 1 }");
    assert!(!d.has_errors());
    assert_eq!(block.stmts.len(), 2);
    let StmtKind::Let { mutability, .. } = &block.stmts[0].kind else {
        panic!("expected Let stmt");
    };
    assert_eq!(*mutability, BindingMode::Mutable);
    let StmtKind::Assign { op, .. } = &block.stmts[1].kind else {
        panic!("expected Assign stmt");
    };
    assert_eq!(*op, AssignOp::Plain);
}

#[test]
fn let_with_typed_pattern() {
    let (block, d) = parse_block_str("{ let x: i32 = 42 }");
    assert!(!d.has_errors());
    let StmtKind::Let { ty, init, .. } = &block.stmts.first().or(
        block.trailing.as_ref().map(|_| panic!("expected stmt not trailing"))
    ).unwrap_or_else(|| panic!("expected let stmt")).kind else {
        panic!("expected Let stmt");
    };
    assert!(ty.is_some());
    assert!(init.is_some());
}

#[test]
fn compound_assignment() {
    let (block, d) = parse_block_str("{ x += 1 }");
    assert!(!d.has_errors());
    let StmtKind::Assign { op, .. } = &block.stmts[0].kind else {
        panic!("expected Assign");
    };
    assert_eq!(*op, AssignOp::Add);
}

#[test]
fn cast_to_path_type() {
    let (e, _) = parse_expr_str("x as i32");
    let ExprKind::Cast { ty, mode, .. } = &e.kind else {
        panic!("expected Cast");
    };
    assert!(matches!(ty.kind, TypeKind::Path(_)));
    assert_eq!(*mode, crate::ast::CastMode::Trap);
}

#[test]
fn cast_with_wrapping_mode() {
    let (e, _) = parse_expr_str("x as u8 wrapping");
    let ExprKind::Cast { mode, .. } = &e.kind else {
        panic!("expected Cast");
    };
    assert_eq!(*mode, crate::ast::CastMode::Wrapping);
}

#[test]
fn cast_with_saturating_mode() {
    let (e, _) = parse_expr_str("x as u8 saturating");
    let ExprKind::Cast { mode, .. } = &e.kind else {
        panic!("expected Cast");
    };
    assert_eq!(*mode, crate::ast::CastMode::Saturating);
}

#[test]
fn cast_with_checked_mode() {
    let (e, _) = parse_expr_str("x as u8 checked");
    let ExprKind::Cast { mode, .. } = &e.kind else {
        panic!("expected Cast");
    };
    assert_eq!(*mode, crate::ast::CastMode::Checked);
}

#[test]
fn return_with_and_without_value() {
    let (e1, _) = parse_expr_str("return 42");
    let ExprKind::Return(Some(_)) = &e1.kind else {
        panic!("expected Return(Some)");
    };
    let (block, _) = parse_block_str("{ return }");
    // bare `return` in a block — should parse as Return(None) inside stmts.
    let stmt_kind = if !block.stmts.is_empty() {
        &block.stmts[0].kind
    } else {
        &StmtKind::Expr(block.trailing.as_ref().expect("expected trailing").as_ref().clone())
    };
    let StmtKind::Expr(e) = stmt_kind else {
        panic!("expected Expr stmt");
    };
    assert!(matches!(e.kind, ExprKind::Return(None)));
}

#[test]
fn negative_integer_overflow_recovers() {
    // 2^128 already errors at the lexer; the parser should still produce
    // some AST without panicking.
    let (_, d) = parse_expr_str("340282366920938463463374607431768211456 + 1");
    assert!(d.has_errors());
}

#[test]
fn string_literal_expression() {
    let (e, _) = parse_expr_str("\"hello\"");
    assert!(matches!(
        e.kind,
        ExprKind::Literal(Literal::Str(_))
    ));
}

#[test]
fn fstring_literal_expression() {
    let (e, _) = parse_expr_str("f\"hi {name}\"");
    assert!(matches!(e.kind, ExprKind::FString(_)));
}

#[test]
fn fstring_literal_segment_escapes_match_plain_string_unescape() {
    // Bootstrap parity fixture: f"a\nb" byte-equals "a\nb".
    let (fstr, interner, fdiags) = parse_expr_str_with_interner("f\"a\\nb\"");
    assert!(!fdiags.has_errors());
    let ExprKind::FString(parts) = &fstr.kind else {
        panic!("expected FString, got {:?}", fstr.kind);
    };
    assert_eq!(parts.len(), 1);
    let FStringPart::Text(sym) = parts[0] else {
        panic!("expected a single Text part, got {:?}", parts[0]);
    };
    let fstring_text = interner.resolve(sym);

    let (plain, plain_interner, pdiags) = parse_expr_str_with_interner("\"a\\nb\"");
    assert!(!pdiags.has_errors());
    let ExprKind::Literal(Literal::Str(psym)) = plain.kind else {
        panic!("expected Str literal, got {:?}", plain.kind);
    };
    let plain_text = plain_interner.resolve(psym);

    assert_eq!(fstring_text, "a\nb");
    assert_eq!(fstring_text, plain_text);
}

#[test]
fn fstring_brace_escapes_are_literal_and_not_a_slot() {
    let (e, interner, diags) = parse_expr_str_with_interner("f\"\\{not a slot\\}\"");
    assert!(!diags.has_errors());
    let ExprKind::FString(parts) = &e.kind else {
        panic!("expected FString, got {:?}", e.kind);
    };
    assert_eq!(parts.len(), 1);
    let FStringPart::Text(sym) = parts[0] else {
        panic!("expected a single Text part (no Slot), got {:?}", parts[0]);
    };
    assert_eq!(interner.resolve(sym), "{not a slot}");
}

#[test]
fn integer_with_base_preserved() {
    let (e, _) = parse_expr_str("0xFF");
    let ExprKind::Literal(Literal::Int { value, base }) = &e.kind else {
        panic!("expected Int literal");
    };
    assert_eq!(*value, 0xFF);
    assert_eq!(*base, IntBase::Hex);
}

#[test]
fn bitwise_and_equality_mix_without_parens_is_error() {
    // `expressions.md` §"Operator precedence" worked example: this is
    // the canonical C precedence trap that the lock forecloses.
    let (_, d) = parse_expr_str("a & b == c");
    assert!(d.has_errors(), "expected parse_error for `a & b == c`");
}

#[test]
fn bitwise_and_equality_mix_with_parens_parses_cleanly() {
    let (_, d) = parse_expr_str("(a & b) == c");
    assert!(!d.has_errors(), "expected clean parse for `(a & b) == c`");
    let (_, d2) = parse_expr_str("a & (b == c)");
    assert!(!d2.has_errors(), "expected clean parse for `a & (b == c)`");
}

#[test]
fn equality_then_bitwise_mix_without_parens_is_error() {
    // RHS side of the trap: `a == b & c` parses as `a == (b & c)`.
    let (_, d) = parse_expr_str("a == b & c");
    assert!(d.has_errors(), "expected parse_error for `a == b & c`");
}

#[test]
fn bitwise_or_xor_mix_with_comparison_are_errors() {
    let (_, d) = parse_expr_str("a | b < c");
    assert!(d.has_errors(), "expected parse_error for `a | b < c`");
    let (_, d2) = parse_expr_str("a ^ b != c");
    assert!(d2.has_errors(), "expected parse_error for `a ^ b != c`");
}

#[test]
fn shift_with_comparison_is_admitted() {
    // The spec's worked example only locks bitwise-and/or/xor against
    // comparison/equality. Shifts stay admitted at this scope.
    let (_, d) = parse_expr_str("a << b == c");
    assert!(!d.has_errors(), "expected clean parse for `a << b == c`");
}

#[test]
fn first_class_function_type_literal_parses_cleanly() {
    // phase-2-locks Gap 1: `function(P) -> R with {row}` is admitted as a
    // type literal in type position.
    let (_, d) = parse_block_str("{ let f: function(i32) -> i32 with {} = g }");
    assert!(
        !d.has_errors(),
        "expected clean parse for `function(...)` type literal"
    );
}

#[test]
fn comptime_in_type_position_is_rejected() {
    // `comptime.md` locks `comptime` as an expression prefix, block introducer,
    // and parameter prefix only — never as a type-position prefix. The parse
    // path must reject `comptime <Type>` in type position.
    let (_, d) = parse_block_str("{ let x: comptime i32 = 42 }");
    assert!(
        d.has_errors(),
        "expected parse_error for `comptime <Type>` in type position"
    );
}

#[test]
fn stacked_inline_where_on_one_type_is_rejected() {
    // `refinements.md` says "Multiple inline predicates on one parameter use
    // `&&`". Chaining two inline `where` clauses on one type must be rejected
    // with a targeted parse_error pointing at the second `where`.
    let (_, d) = parse_block_str("{ let x: i32 where a where b = 0 }");
    assert!(
        d.has_errors(),
        "expected parse_error for stacked inline `where` clauses"
    );
}

#[test]
fn match_range_pattern() {
    let src = "match n { case 0..<10 => 1 case 10..=255 => 2 case _ => 0 }";
    let (e, d) = parse_expr_str(src);
    assert!(!d.has_errors(), "diagnostics: {:?}", d.error_count());
    let ExprKind::Match { arms, .. } = &e.kind else {
        panic!("expected Match");
    };
    assert!(matches!(
        arms[0].pat.kind,
        PatKind::Range {
            kind: RangeKind::HalfOpen,
            ..
        }
    ));
    assert!(matches!(
        arms[1].pat.kind,
        PatKind::Range {
            kind: RangeKind::Closed,
            ..
        }
    ));
}

#[test]
fn match_at_binding_pattern() {
    let src = "match n { case whole @ 5 => whole case _ => 0 }";
    let (e, d) = parse_expr_str(src);
    assert!(!d.has_errors(), "diagnostics: {:?}", d.error_count());
    let ExprKind::Match { arms, .. } = &e.kind else {
        panic!("expected Match");
    };
    let PatKind::AtBinding { inner, .. } = &arms[0].pat.kind else {
        panic!("expected AtBinding, got {:?}", arms[0].pat.kind);
    };
    assert!(matches!(inner.kind, PatKind::Literal(Literal::Int { .. })));
}

#[test]
fn match_slice_patterns() {
    // Empty, fixed-length, rest-at-end, and rest-at-front forms. Slice
    // element binders are bare (`first`, not `let first`) even in a match
    // arm — §8 spells them that way.
    let src = "match xs { case [] => 0 case [first, ..tail] => first case [..front, last] => last }";
    let (e, d) = parse_expr_str(src);
    assert!(!d.has_errors(), "diagnostics: {:?}", d.error_count());
    let ExprKind::Match { arms, .. } = &e.kind else {
        panic!("expected Match");
    };
    assert!(matches!(
        &arms[0].pat.kind,
        PatKind::Slice { prefix, rest, suffix }
            if prefix.is_empty() && rest.is_none() && suffix.is_empty()
    ));
    let PatKind::Slice { prefix, rest, suffix } = &arms[1].pat.kind else {
        panic!("expected Slice");
    };
    assert_eq!(prefix.len(), 1);
    assert!(matches!(rest, Some(Some(_))));
    assert!(suffix.is_empty());
    let PatKind::Slice { prefix, rest, suffix } = &arms[2].pat.kind else {
        panic!("expected Slice");
    };
    assert!(prefix.is_empty());
    assert!(matches!(rest, Some(Some(_))));
    assert_eq!(suffix.len(), 1);
}

#[test]
fn malformed_slice_pattern_terminates() {
    // A reserved keyword (`init`) where a binder is expected must not spin
    // the slice loop forever (the forward-progress guard): it terminates
    // with a parse error instead of OOMing.
    let (_, d) = parse_expr_str("match xs { case [..init, last] => last case _ => 0 }");
    assert!(d.has_errors(), "expected a parse_error for the reserved-keyword binder");
}

