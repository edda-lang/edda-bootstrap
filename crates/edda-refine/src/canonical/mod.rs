//! Deterministic canonical encoding for refine's [`Predicate`] IR.
//!
//! Per `docs/codegen/distribution/03-certificate.md` §3.3 / §4: every proof
//! certificate is keyed by `BLAKE3((predicate_canonical || context_canonical))`
//! where:
//!
//! 1. Each predicate is serialised into a deterministic byte sequence with
//!    *commutative operators sorted on their operands*. Two predicates that
//!    differ only by ordering of `==` / `!=` / `&&` / `||` / `+` operands
//!    produce the same canonical bytes and therefore the same cache key.
//! 2. A context conjunction is canonicalised by canonicalising each
//!    predicate, sorting the resulting byte sequences lexicographically,
//!    and concatenating with the `0xFF` separator byte (`§4` "no valid
//!    varint starts with 0xFF").
//!
//! # Encoding shape — locked
//!
//! Refine's canonical form is *not* bit-equal to the Group A AST blob
//! encoding from `distribution/01-blob-format.md §3`. Reasons:
//!
//! - refine operates on its own [`Predicate`] IR after the AST-to-predicate
//!   lift, not on the parsed AST; the lifted form has already collapsed
//!   multi-segment paths and resolved literal sorts;
//! - several refine-IR nodes (`MulLit`, `DivLit`, `TagEq`) don't correspond
//!   to AST kinds 1:1.
//!
//! The byte format is therefore refine-internal. The verifier (per
//! `03-certificate.md §6`, v1.0) will read these bytes via the same encoder
//! invoked here — the canonicaliser is the canonical-form authority for
//! refine certificates. Aligning with the Group A taxonomy is a v1.0
//! concern that pairs with the typechecker integration emitting typecheck
//! blobs.
//!
//! Per-variant kind tags below are locked; reordering or
//! renaming any tag is a wire-breaking change that bumps
//! [`CERTIFICATE_FORMAT_VERSION`](crate::CERTIFICATE_FORMAT_VERSION).
//!
//! # Workspace BLAKE3 binding
//!
//! Per the `edda-codegen` crate's convention ("routed through
//! `edda_cache::hash_bytes` so the workspace has a single BLAKE3 binding"),
//! this module computes hashes through [`edda_cache::hash_bytes`] rather
//! than depending on `blake3`
//! directly. Refine's exposed [`ObligationHash`] / [`ContextHash`] aliases
//! are `[u8; 32]` to keep the public API decoupled from
//! [`edda_cache::ArtifactHash`] — callers can re-wrap as needed.

use edda_cache::{hash_bytes, ArtifactHash};

use crate::predicate::{ArithOp, CmpOp, IntLit, IntLitValue, Predicate};
use crate::sort::{FieldRef, IntSort, Sort};
use crate::wire::encode_varint;

mod tags;

use tags::{
    arith_op_tag, bool_binop_tag, cmp_op_tag, int_width_tag, INT_LIT_SIGNED, INT_LIT_UNSIGNED,
    SORT_BOOL, SORT_INT, SORT_RECORD, SORT_SLICE, SORT_SUM, SORT_TUPLE, TAG_ARITH, TAG_BOOL_BINOP,
    TAG_BOOL_LIT, TAG_CAST, TAG_CMP, TAG_DIV_LIT, TAG_EXISTS, TAG_FIELD_PROJ, TAG_FORALL, TAG_IF,
    TAG_INT_LIT, TAG_MOD_LIT, TAG_MUL_LIT, TAG_NEG, TAG_NOT, TAG_SLICE_INDEX, TAG_SLICE_LEN,
    TAG_SLICE_STORE, TAG_TAG_EQ, TAG_VAR,
};

/// Context-separator byte. Per `distribution/03-certificate.md §4`: chosen
/// because no valid varint encoding starts with `0xFF`, so using it between
/// canonical predicates prevents collision-by-substring.
const CONTEXT_SEPARATOR: u8 = 0xFF;

/// 32-byte BLAKE3 of an obligation's canonical predicate.
pub type ObligationHash = [u8; 32];

/// 32-byte BLAKE3 of a context conjunction's canonical (sorted) form.
pub type ContextHash = [u8; 32];

//            Predicate value; equality under `Predicate: PartialEq` implies
//            byte-equality (modulo the commutative-operand reordering rule)
//            (Eq, Ne, Add, And, Or sort operands lexicographically); changing
//            this invalidates every cached proof certificate
/// Serialise a [`Predicate`] into its canonical byte form. Commutative
/// operators (`Eq`, `Ne`, `Add`, `And`, `Or`) sort their operands
/// lexicographically by canonical-form bytes before emitting.
pub fn canonical_predicate(predicate: &Predicate) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    encode_predicate(predicate, &mut out);
    out
}

//            distribution/03-certificate.md §4; conflicts with varint encoding
//            (see wire.rs) are why no valid canonical predicate begins with
//            0xFF
//            two equivalent context sets produce identical bytes
/// Serialise a context conjunction. Each predicate is canonicalised
/// individually, the resulting byte sequences are sorted lexicographically,
/// and the sorted sequences are concatenated with `0xFF` separator bytes
/// between them (no leading / trailing separators).
pub fn canonical_context(context: &[Predicate]) -> Vec<u8> {
    let mut canonicalised: Vec<Vec<u8>> = context
        .iter()
        .map(canonical_predicate)
        .collect();
    canonicalised.sort();
    let mut out = Vec::with_capacity(canonicalised.iter().map(|p| p.len() + 1).sum());
    for (i, bytes) in canonicalised.iter().enumerate() {
        if i > 0 {
            out.push(CONTEXT_SEPARATOR);
        }
        out.extend_from_slice(bytes);
    }
    out
}

//            distribution/03-certificate.md §3.3 / §4; changing the encoder
//            or hash function invalidates every cached proof certificate
/// BLAKE3 of `canonical_predicate(predicate)`. The
/// [`ProofCertificate`](crate::ProofCertificate)'s `obligation_hash` field.
pub fn obligation_hash(predicate: &Predicate) -> ObligationHash {
    bytes_to_hash(&canonical_predicate(predicate))
}

//            distribution/03-certificate.md §3.3 / §4; same invalidation
//            reach as obligation_hash
/// BLAKE3 of `canonical_context(context)`. The
/// [`ProofCertificate`](crate::ProofCertificate)'s `context_hash` field.
/// Empty contexts hash to the BLAKE3 of an empty byte sequence (the canonical
/// "no predicates" key).
pub fn context_hash(context: &[Predicate]) -> ContextHash {
    bytes_to_hash(&canonical_context(context))
}

fn bytes_to_hash(bytes: &[u8]) -> [u8; 32] {
    let artifact: ArtifactHash = hash_bytes(bytes);
    *artifact.as_bytes()
}

// ----- encoder internals -----

fn encode_predicate(predicate: &Predicate, out: &mut Vec<u8>) {
    match predicate {
        Predicate::Var(v) => {
            out.push(TAG_VAR);
            encode_str(v.name.as_str(), out);
            encode_sort(&v.sort, out);
        }
        Predicate::IntLit(lit) => {
            out.push(TAG_INT_LIT);
            encode_int_lit(*lit, out);
        }
        Predicate::BoolLit(b) => {
            out.push(TAG_BOOL_LIT);
            out.push(if *b { 1 } else { 0 });
        }
        Predicate::Arith { op, lhs, rhs } => {
            out.push(TAG_ARITH);
            out.push(arith_op_tag(*op));
            encode_binary_operands(*op == ArithOp::Add, lhs, rhs, out);
        }
        Predicate::Neg(operand) => {
            out.push(TAG_NEG);
            encode_predicate(operand, out);
        }
        Predicate::MulLit { c, expr } => {
            out.push(TAG_MUL_LIT);
            encode_int_lit(*c, out);
            encode_predicate(expr, out);
        }
        Predicate::DivLit { expr, c } => {
            out.push(TAG_DIV_LIT);
            encode_predicate(expr, out);
            encode_int_lit(*c, out);
        }
        Predicate::ModLit { expr, c } => {
            out.push(TAG_MOD_LIT);
            encode_predicate(expr, out);
            encode_int_lit(*c, out);
        }
        Predicate::Cmp { op, lhs, rhs } => {
            out.push(TAG_CMP);
            out.push(cmp_op_tag(*op));
            let commutative = matches!(op, CmpOp::Eq | CmpOp::Ne);
            encode_binary_operands(commutative, lhs, rhs, out);
        }
        Predicate::BoolBinOp { op, lhs, rhs } => {
            out.push(TAG_BOOL_BINOP);
            out.push(bool_binop_tag(*op));
            encode_binary_operands(true, lhs, rhs, out);
        }
        Predicate::Not(operand) => {
            out.push(TAG_NOT);
            encode_predicate(operand, out);
        }
        Predicate::If {
            cond,
            then_br,
            else_br,
        } => {
            out.push(TAG_IF);
            encode_predicate(cond, out);
            encode_predicate(then_br, out);
            encode_predicate(else_br, out);
        }
        Predicate::FieldProj { base, field } => {
            out.push(TAG_FIELD_PROJ);
            encode_predicate(base, out);
            encode_field_ref(field, out);
        }
        Predicate::SliceLen { slice } => {
            out.push(TAG_SLICE_LEN);
            encode_predicate(slice, out);
        }
        Predicate::SliceIndex { slice, index } => {
            out.push(TAG_SLICE_INDEX);
            encode_predicate(slice, out);
            encode_predicate(index, out);
        }
        Predicate::SliceStore {
            slice,
            index,
            value,
        } => {
            out.push(TAG_SLICE_STORE);
            encode_predicate(slice, out);
            encode_predicate(index, out);
            encode_predicate(value, out);
        }
        Predicate::Cast { value, to } => {
            out.push(TAG_CAST);
            encode_predicate(value, out);
            encode_int_sort(*to, out);
        }
        Predicate::TagEq { value, variant } => {
            out.push(TAG_TAG_EQ);
            encode_predicate(value, out);
            encode_str(variant.sum.name(), out);
            encode_str(variant.variant.as_str(), out);
        }
        Predicate::Forall {
            bound,
            lower,
            upper,
            body,
        } => {
            out.push(TAG_FORALL);
            encode_str(bound.name.as_str(), out);
            encode_sort(&bound.sort, out);
            encode_predicate(lower, out);
            encode_predicate(upper, out);
            encode_predicate(body, out);
        }
        Predicate::Exists {
            bound,
            lower,
            upper,
            body,
        } => {
            out.push(TAG_EXISTS);
            encode_str(bound.name.as_str(), out);
            encode_sort(&bound.sort, out);
            encode_predicate(lower, out);
            encode_predicate(upper, out);
            encode_predicate(body, out);
        }
    }
}

// Emit two operands, sorted lexicographically when `commutative` is true.
fn encode_binary_operands(
    commutative: bool,
    lhs: &Predicate,
    rhs: &Predicate,
    out: &mut Vec<u8>,
) {
    if commutative {
        let l = canonical_predicate(lhs);
        let r = canonical_predicate(rhs);
        let (first, second) = if l <= r { (l, r) } else { (r, l) };
        out.extend_from_slice(&first);
        out.extend_from_slice(&second);
    } else {
        encode_predicate(lhs, out);
        encode_predicate(rhs, out);
    }
}

fn encode_int_lit(lit: IntLit, out: &mut Vec<u8>) {
    encode_int_sort(lit.sort, out);
    match lit.value {
        IntLitValue::Signed(v) => {
            out.push(INT_LIT_SIGNED);
            out.extend_from_slice(&v.to_le_bytes());
        }
        IntLitValue::Unsigned(v) => {
            out.push(INT_LIT_UNSIGNED);
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
}

fn encode_int_sort(sort: IntSort, out: &mut Vec<u8>) {
    out.push(int_width_tag(sort.width));
    out.push(if sort.signed { 1 } else { 0 });
}

fn encode_sort(sort: &Sort, out: &mut Vec<u8>) {
    match sort {
        Sort::Int(s) => {
            out.push(SORT_INT);
            encode_int_sort(*s, out);
        }
        Sort::Bool => out.push(SORT_BOOL),
        Sort::Slice(elem) => {
            out.push(SORT_SLICE);
            encode_sort(elem, out);
        }
        Sort::Tuple(elements) => {
            out.push(SORT_TUPLE);
            encode_varint(elements.len() as u64, out);
            for elem in elements {
                encode_sort(elem, out);
            }
        }
        Sort::Record(record) => {
            out.push(SORT_RECORD);
            encode_str(record.name(), out);
        }
        Sort::Sum(sum) => {
            out.push(SORT_SUM);
            encode_str(sum.name(), out);
        }
    }
}

fn encode_field_ref(field: &FieldRef, out: &mut Vec<u8>) {
    encode_str(field.record.name(), out);
    encode_str(field.field.as_str(), out);
    encode_sort(&field.sort, out);
}

fn encode_str(s: &str, out: &mut Vec<u8>) {
    encode_varint(s.len() as u64, out);
    out.extend_from_slice(s.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::predicate::{IntLit, Variable};
    use crate::sort::{IntSort, IntWidth, Sort};

    fn i32_sort() -> IntSort {
        IntSort::sized(IntWidth::W32, true)
    }

    fn x_var() -> Predicate {
        Predicate::Var(Variable::new("x", Sort::Int(i32_sort())))
    }

    fn y_var() -> Predicate {
        Predicate::Var(Variable::new("y", Sort::Int(i32_sort())))
    }

    fn lit(v: i32) -> Predicate {
        Predicate::IntLit(IntLit::signed(v as i128, i32_sort()))
    }

    #[test]
    fn canonical_form_is_deterministic_for_repeated_calls() {
        let p = Predicate::add(x_var(), lit(7));
        assert_eq!(canonical_predicate(&p), canonical_predicate(&p));
    }

    #[test]
    fn commutative_add_normalises_operand_order() {
        let lhs_then_rhs = Predicate::add(x_var(), y_var());
        let rhs_then_lhs = Predicate::add(y_var(), x_var());
        assert_eq!(
            canonical_predicate(&lhs_then_rhs),
            canonical_predicate(&rhs_then_lhs)
        );
    }

    #[test]
    fn non_commutative_sub_preserves_operand_order() {
        let x_minus_y = Predicate::sub(x_var(), y_var());
        let y_minus_x = Predicate::sub(y_var(), x_var());
        assert_ne!(
            canonical_predicate(&x_minus_y),
            canonical_predicate(&y_minus_x)
        );
    }

    #[test]
    fn equality_is_commutative() {
        let lhs = Predicate::cmp(CmpOp::Eq, x_var(), lit(0));
        let rhs = Predicate::cmp(CmpOp::Eq, lit(0), x_var());
        assert_eq!(canonical_predicate(&lhs), canonical_predicate(&rhs));
    }

    #[test]
    fn less_than_is_not_commutative() {
        let lhs = Predicate::cmp(CmpOp::Lt, x_var(), lit(0));
        let rhs = Predicate::cmp(CmpOp::Lt, lit(0), x_var());
        assert_ne!(canonical_predicate(&lhs), canonical_predicate(&rhs));
    }

    #[test]
    fn distinct_predicates_produce_distinct_hashes() {
        let h1 = obligation_hash(&Predicate::add(x_var(), lit(1)));
        let h2 = obligation_hash(&Predicate::add(x_var(), lit(2)));
        assert_ne!(h1, h2);
    }

    #[test]
    fn context_canonical_form_is_order_independent() {
        let p1 = Predicate::cmp(CmpOp::Gt, x_var(), lit(0));
        let p2 = Predicate::cmp(CmpOp::Lt, x_var(), lit(100));
        let order_a = vec![p1.clone(), p2.clone()];
        let order_b = vec![p2, p1];
        assert_eq!(canonical_context(&order_a), canonical_context(&order_b));
    }

    #[test]
    fn empty_context_canonicalises_to_empty_bytes() {
        assert!(canonical_context(&[]).is_empty());
        // Empty-context hash is BLAKE3 of zero bytes — a well-known fixed value.
        let h = context_hash(&[]);
        // Sanity: it's deterministic.
        assert_eq!(h, context_hash(&[]));
    }

    #[test]
    fn context_hash_differs_between_distinct_contexts() {
        let h1 = context_hash(&[Predicate::cmp(CmpOp::Gt, x_var(), lit(0))]);
        let h2 = context_hash(&[Predicate::cmp(CmpOp::Lt, x_var(), lit(0))]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn canonical_predicate_never_starts_with_separator_byte() {
        // Per `distribution/03-certificate.md` §4, the 0xFF context separator
        // works because no canonical *predicate* starts with 0xFF — the first
        // byte is always a kind_tag, all of which are well below 0x80. (The
        // spec's prose about "no valid varint starts with 0xFF" is loose;
        // varints can start with 0xFF when the value has 0x7F in the low 7
        // bits and a non-zero continuation. The invariant that actually
        // protects against substring-collision is the kind_tag bound.)
        for predicate in [
            x_var(),
            lit(0),
            Predicate::BoolLit(false),
            Predicate::add(x_var(), lit(7)),
            Predicate::cmp(CmpOp::Eq, x_var(), lit(0)),
            Predicate::not(Predicate::BoolLit(true)),
        ] {
            let bytes = canonical_predicate(&predicate);
            assert!(!bytes.is_empty(), "predicate canonicalised to no bytes");
            assert!(
                bytes[0] < 0x80,
                "predicate leading byte 0x{:02x} would collide with context separator",
                bytes[0]
            );
        }
    }

    #[test]
    fn context_separator_appears_only_between_predicates() {
        let p1 = Predicate::cmp(CmpOp::Gt, x_var(), lit(0));
        let p2 = Predicate::cmp(CmpOp::Lt, x_var(), lit(100));
        let bytes = canonical_context(&[p1, p2]);
        let separator_count = bytes.iter().filter(|&&b| b == 0xFF).count();
        // Exactly one 0xFF between the two predicates. (The varints inside
        // each predicate's bytes can't produce 0xFF as a leading byte; they
        // may produce it as a continuation byte but the test predicates use
        // short strings whose lengths fit in single varint bytes.)
        assert_eq!(separator_count, 1, "bytes: {bytes:?}");
    }
}
