//! Generator strategy enum and concrete value production.
//!
//! A [`Strategy`] is the per-parameter generator decision the analyser
//! produces; [`generate_values`] turns it into the concrete [`Value`]
//! sample the runner consumes.

use crate::properties::value::Value;

// converts open bounds (`x < hi`) to inclusive (`x <= hi - 1`) at
// analysis time so generation logic stays in one shape
/// Per-parameter generator decision the analyser produces.
///
/// Strategy values are pure data — they carry no allocation and no
/// solver state. The runner consumes them via [`generate_values`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// `requires x == c` → emit just `c`.
    Constant(i128),
    /// Integer range `[lo, hi]` inclusive. Bounds default to the
    /// `i128` minimum/maximum when not refined.
    IntRange { lo: i128, hi: i128 },
    /// Boolean parameter — always emits `[false, true]`.
    BoolValue,
    /// The analyser could not produce a generator (composite type,
    /// uninterpreted predicate, unsupported refinement form). The
    /// runner skips the property test for this function and notes
    /// the gap.
    Unanalyzable,
}

// caller's `requires` clauses are satisfied by construction
// runner reads `is_empty()` to skip)
/// Generate up to `n` concrete values from `strategy`.
///
/// For `Constant`, the value repeats; for `IntRange`, the analyser
/// emits a deterministic sample (boundary values plus an evenly-
/// stepped interior); for `BoolValue`, the full `[false, true]` set;
/// for `Unanalyzable`, an empty vector.
pub fn generate_values(strategy: &Strategy, n: usize) -> Vec<Value> {
    match strategy {
        Strategy::Constant(c) => vec![Value::Int(*c); n.max(1)],
        Strategy::IntRange { lo, hi } => sample_int_range(*lo, *hi, n),
        Strategy::BoolValue => vec![Value::Bool(false), Value::Bool(true)],
        Strategy::Unanalyzable => Vec::new(),
    }
}

// returns an empty vector — the analyser is expected to never produce
// an inverted range, but defensive logic stays in one place
// `03-verification.md` §6 (Shrinkage): integer counterexamples
// shrink toward 0, then toward the boundary, then sweep interior.
// The synthesised runner panics on the first failing test, so
// putting the most-shrunk values first surfaces the minimal
// counterexample to the user directly — true adaptive shrinkage
// (bisection on runtime feedback) is the follow-up that needs the
// run-then-report harness C9's commit notes call out
fn sample_int_range(lo: i128, hi: i128, n: usize) -> Vec<Value> {
    if lo > hi {
        return Vec::new();
    }
    let n = n.max(1);
    if lo == hi {
        return vec![Value::Int(lo)];
    }
    // Emission order, in priority:
    //   1. The value closest to 0 inside [lo, hi] — the codex's
    //      canonical shrink target. If 0 is in range, 0 itself
    //      goes first.
    //   2. The lower bound `lo` (boundary).
    //   3. The upper bound `hi` (boundary).
    //   4. Interior samples — evenly-spaced steps between lo and hi.
    let mut out: Vec<Value> = Vec::with_capacity(n);
    let mut pushed: Vec<i128> = Vec::new();
    let mut push_unique = |v: i128, out: &mut Vec<Value>, pushed: &mut Vec<i128>| {
        if !pushed.contains(&v) {
            out.push(Value::Int(v));
            pushed.push(v);
        }
    };
    // Shrink-toward-zero canonical value, clamped to [lo, hi].
    let zero_anchor = if lo <= 0 && hi >= 0 {
        0
    } else if hi < 0 {
        hi
    } else {
        lo
    };
    push_unique(zero_anchor, &mut out, &mut pushed);
    if out.len() < n {
        push_unique(lo, &mut out, &mut pushed);
    }
    if out.len() < n {
        push_unique(hi, &mut out, &mut pushed);
    }
    if out.len() < n {
        // Step between lo and hi for the interior values. Use i128
        // arithmetic to avoid overflow on extreme ranges.
        let span = hi.saturating_sub(lo);
        let remaining = n.saturating_sub(out.len());
        let interior = remaining as i128;
        for i in 1..=interior {
            if out.len() >= n {
                break;
            }
            let offset = span.saturating_mul(i) / (interior + 1);
            push_unique(lo.saturating_add(offset), &mut out, &mut pushed);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_range_sample_includes_boundaries() {
        let s = Strategy::IntRange { lo: 0, hi: 10 };
        let values = generate_values(&s, 5);
        assert!(values.contains(&Value::Int(0)));
        assert!(values.contains(&Value::Int(10)));
    }

    #[test]
    fn singleton_range_emits_one_value() {
        let s = Strategy::IntRange { lo: 7, hi: 7 };
        let values = generate_values(&s, 100);
        assert_eq!(values, vec![Value::Int(7)]);
    }

    #[test]
    fn shrink_friendly_order_puts_zero_first_when_in_range() {
        // Per `corpus/edda-codex/language/03-verification.md` §6
        // (Shrinkage), integer counterexamples shrink toward 0. The
        // synthesised runner panics on the first failing test, so
        // emitting 0 first gives the user the minimal counterexample
        // directly when 0 is in the analysed range.
        let s = Strategy::IntRange { lo: -10, hi: 10 };
        let values = generate_values(&s, 5);
        assert_eq!(values[0], Value::Int(0));
    }

    #[test]
    fn shrink_friendly_order_picks_closest_to_zero_when_zero_excluded() {
        // For an all-positive range, the smallest absolute value is
        // `lo` — the bound closest to zero.
        let s = Strategy::IntRange { lo: 5, hi: 100 };
        let values = generate_values(&s, 5);
        assert_eq!(values[0], Value::Int(5));

        // For an all-negative range, the closest-to-zero is `hi`.
        let s = Strategy::IntRange { lo: -100, hi: -5 };
        let values = generate_values(&s, 5);
        assert_eq!(values[0], Value::Int(-5));
    }
}
