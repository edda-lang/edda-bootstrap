//! Tests for the binary/unary operator semantics.

use super::*;

#[test]
fn add_signed_i32() {
    let l = Value::Int(IntValue::new_signed(Primitive::I32, 1));
    let r = Value::Int(IntValue::new_signed(Primitive::I32, 2));
    let out = apply_binary(BinOp::Add, &l, &r).unwrap();
    assert!(matches!(out, Value::Int(v) if v.as_i128() == Some(3)));
}

#[test]
fn add_overflow_i8() {
    let l = Value::Int(IntValue::new_signed(Primitive::I8, 127));
    let r = Value::Int(IntValue::new_signed(Primitive::I8, 1));
    let err = apply_binary(BinOp::Add, &l, &r).unwrap_err();
    assert!(matches!(err, OpError::Overflow { op: "+", .. }));
}

#[test]
fn div_by_zero_signed() {
    let l = Value::Int(IntValue::new_signed(Primitive::I32, 10));
    let r = Value::Int(IntValue::new_signed(Primitive::I32, 0));
    let err = apply_binary(BinOp::Div, &l, &r).unwrap_err();
    assert!(matches!(err, OpError::DivByZero { op: "/" }));
}

#[test]
fn cmp_signed() {
    let l = Value::Int(IntValue::new_signed(Primitive::I32, -3));
    let r = Value::Int(IntValue::new_signed(Primitive::I32, 5));
    assert!(matches!(
        apply_binary(BinOp::Lt, &l, &r).unwrap(),
        Value::Bool(true)
    ));
    assert!(matches!(
        apply_binary(BinOp::Ge, &l, &r).unwrap(),
        Value::Bool(false)
    ));
}

#[test]
fn bool_logical_short_circuits_at_value_layer() {
    // Note: short-circuit happens at the HIR walker, not here.
    // This layer evaluates both operands fully.
    let t = Value::Bool(true);
    let f = Value::Bool(false);
    assert!(matches!(
        apply_binary(BinOp::And, &t, &f).unwrap(),
        Value::Bool(false)
    ));
    assert!(matches!(
        apply_binary(BinOp::Or, &f, &t).unwrap(),
        Value::Bool(true)
    ));
}

#[test]
fn neg_signed_int() {
    let v = Value::Int(IntValue::new_signed(Primitive::I32, 5));
    let out = apply_unary(UnOp::Neg, &v).unwrap();
    assert!(matches!(out, Value::Int(i) if i.as_i128() == Some(-5)));
}

#[test]
fn neg_unsigned_int_rejects() {
    let v = Value::Int(IntValue::new_unsigned(Primitive::U32, 5));
    let err = apply_unary(UnOp::Neg, &v).unwrap_err();
    assert!(matches!(err, OpError::KindMismatch { op: "-", .. }));
}

#[test]
fn bit_not_unsigned() {
    let v = Value::Int(IntValue::new_unsigned(Primitive::U8, 0));
    let out = apply_unary(UnOp::BitNot, &v).unwrap();
    assert!(matches!(out, Value::Int(i) if i.as_u128() == Some(0xff)));
}

#[test]
fn shift_left_unsigned() {
    let v = Value::Int(IntValue::new_unsigned(Primitive::U32, 1));
    let amt = Value::Int(IntValue::new_unsigned(Primitive::U32, 4));
    let out = apply_binary(BinOp::Shl, &v, &amt).unwrap();
    assert!(matches!(out, Value::Int(i) if i.as_u128() == Some(16)));
}

#[test]
fn float_add() {
    let l = Value::Float(FloatValue::F64(1.5));
    let r = Value::Float(FloatValue::F64(2.25));
    let out = apply_binary(BinOp::Add, &l, &r).unwrap();
    let v = match out {
        Value::Float(FloatValue::F64(v)) => v,
        other => panic!("expected F64, got {:?}", other),
    };
    assert_eq!(v, 3.75);
}

#[test]
fn mixed_kind_rejected() {
    let i = Value::Int(IntValue::new_signed(Primitive::I32, 1));
    let b = Value::Bool(true);
    let err = apply_binary(BinOp::Add, &i, &b).unwrap_err();
    assert!(matches!(err, OpError::KindMismatch { op: "+", .. }));
}
