//! Control-flow inference.
//!
//! Implements:
//!
//! - **T-If-Synth** — `if c { t } else { e }` checks `c ⇐ bool`,
//!   synthesises both branches, and joins them. A missing `else`
//!   forces the trailing block's type to `()` (the if-as-statement
//!   form). Branch types must agree up to `never` absorption: any
//!   diverging arm contributes `never`, which unifies with the other
//!   branch.
//! - **T-Match-Synth** — scrutinee synth, arms unify on body type.
//!   Patterns currently supported: `Wildcard`, `Binding`,
//!   `Literal`. More complex patterns set their `ty` field via
//!   [`bind_pat`] but do not yet bind names from sub-patterns; that
//!   is follow-up work.
//! - **T-Loop-Synth** — `loop { ... }` synthesises `never`. `break <value>`
//!   is admitted to yield a value from a loop; tracking
//!   the break-value type across all `break` sites is deferred
//!   territory (the for-loop / yield-effect machinery is the natural
//!   home).
//! - **Diverging originators / control-flow exits** — `return`,
//!   `break`, `continue`, `raise`, `panic` all synthesise `never`.
//!   Their optional payloads are synthesised for side-effects (so
//!   sub-expression diagnostics fire) but the result type is fixed.
//!
//! # Module layout
//!
//! - [`branch`] — `if` / `match` synthesis + checking, the branch-type
//!   [`join_types`], and the literal-pattern scrutinee check.
//! - [`loops`] — `loop` inference + the B17 `decreases` literal-narrowing
//!   probe.
//! - [`diverge`] — `return` / `break` / `continue` / `raise` / `panic`.

mod branch;
mod diverge;
mod loops;

pub(super) use branch::{check_if, check_match, synth_if, synth_match};
pub(super) use diverge::{
    block_always_exits, expr_always_exits, synth_divergent, synth_panic, synth_raise, synth_return,
};
pub(super) use loops::synth_loop;

#[cfg(test)]
#[path = "../ctrl_tests.rs"]
mod tests;
