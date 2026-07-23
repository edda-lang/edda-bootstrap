//! Z3-backed [`Solver`] implementation.
//!
//! Per `docs/bootstrap/v0.1-scope.md` §3.5, Z3 (via `z3.rs`) is the v0.1
//! default solver. This crate wires it through the [`Solver`] trait.
//!
//! # Threading model
//!
//! [`Z3Backend`] is a zero-sized type and therefore trivially `Send + Sync`.
//! Each [`discharge`] call constructs a fresh [`z3::Context`] and
//! [`z3::Solver`], translates the [`Obligation`] in isolation, and tears
//! everything down at the end. This matches the spec's per-obligation
//! granularity and sidesteps the `!Sync` nature of long-lived `z3::Context`
//! handles.
//!
//! # Outcome projection
//!
//! Z3's `SatResult::Unknown` is overloaded — it covers both timeouts and
//! genuine "I cannot decide" outcomes. We distinguish them via
//! [`z3::Solver::get_reason_unknown`]: reason strings containing `"timeout"`
//! or `"canceled"` route to [`DischargeOutcome::Timeout`], everything else to
//! [`DischargeOutcome::Unknown`]. Per refinement-decidability.md §8, the two
//! tails differ in user-facing remediation.
//!
//! [`discharge`]: crate::Solver::discharge

use std::sync::Arc;
use std::time::{Duration, Instant};

use z3::ast::{Dynamic, Int};
use z3::{Config, Context, Model, Params, SatResult};

use crate::annotation::DischargeRoute;
use crate::certificate_mint::{
    mint_placeholder_certificate, mint_smt_certificate, mint_trust_certificate,
    mint_unverified_certificate,
};
use crate::obligation::Obligation;
use crate::outcome::{ConcreteValue, Counterexample, DischargeOutcome};
use crate::schema::Schema;
use crate::solver::Solver;
use crate::translate::Translator;

//            return, so the backend is trivially thread-safe regardless of
//            z3::Context's Send/Sync status
//            mutating the schema after construction requires building a new
//            backend (records that were already translated against the old
//            schema would otherwise drift out of sync with the typechecker)
//          worker-thread model
/// Z3-backed [`Solver`].
///
/// Construct with [`Z3Backend::new`] for an empty schema (sufficient for the
/// LIA + Bool + Array fragment) or [`Z3Backend::with_schema`] to
/// supply a [`Schema`] that describes the records and sums the typechecker
/// hands in. Each `discharge` call creates a fresh `z3::Context` and
/// `z3::Solver`, runs the translation + check, and returns the projected
/// [`DischargeOutcome`].
#[derive(Clone, Debug)]
pub struct Z3Backend {
    schema: Arc<Schema>,
}

impl Default for Z3Backend {
    fn default() -> Self {
        Z3Backend::new()
    }
}

impl Z3Backend {
    /// Construct a backend with an empty schema. Sufficient for LIA + Bool +
    /// Array obligations; record / sum / tag-equality obligations require
    /// [`Z3Backend::with_schema`].
    pub fn new() -> Z3Backend {
        Z3Backend {
            schema: Arc::new(Schema::empty()),
        }
    }

    /// Construct a backend with the given [`Schema`]. The schema is shared
    /// (via [`Arc`]) so the typechecker can hand the same schema to multiple
    /// solvers without cloning the underlying maps.
    pub fn with_schema(schema: Arc<Schema>) -> Z3Backend {
        Z3Backend { schema }
    }
}

impl Solver for Z3Backend {
    fn discharge(&self, obligation: &Obligation, timeout: Duration) -> DischargeOutcome {
        // Annotation-routed obligations skip the solver entirely and emit the
        // corresponding certificate per refinement-decidability.md §9. The SMT
        // path is the default route from Obligation::new.
        match &obligation.route {
            DischargeRoute::Unverified(ann) => {
                return DischargeOutcome::Unsat {
                    certificate: mint_unverified_certificate(
                        obligation,
                        ann.reason.clone(),
                        ann.function_site,
                    ),
                };
            }
            DischargeRoute::Trust(ann) => {
                return DischargeOutcome::Unsat {
                    certificate: mint_trust_certificate(
                        obligation,
                        ann.reason.clone(),
                        ann.obligation_site,
                    ),
                };
            }
            DischargeRoute::Comptime | DischargeRoute::Implicit => {
                // edda-comptime and edda-types mint these certs. If they
                // surface in refine, the caller bypassed the right layer; we
                // surface Unknown so the bug shows up in diagnostics.
                return DischargeOutcome::Unknown {
                    reason: Some(
                        "comptime / implicit routes are owned by edda-comptime and edda-types, \
                         not edda-refine — route the obligation through the right layer"
                            .to_string(),
                    ),
                };
            }
            DischargeRoute::Smt => {}
        }

        // SMT path. Enable Z3 proof generation so a successful unsat lets us
        // capture an SMT certificate witness per
        // distribution/03-certificate.md §5.
        let mut cfg = Config::new();
        cfg.set_proof_generation(true);
        let ctx = Context::new(&cfg);
        let solver = z3::Solver::new(&ctx);

        if let Some(outcome) = configure_timeout(&ctx, &solver, timeout) {
            return outcome;
        }

        let mut translator = Translator::new(&ctx, &self.schema);
        if let Err(outcome) = assert_context(&solver, &mut translator, obligation) {
            return outcome;
        }
        if let Err(outcome) = assert_negated_goal(&solver, &mut translator, obligation) {
            return outcome;
        }
        // Sort axioms accumulated while translating the context and goal —
        // type-level facts the mathematical-Int encoding loses (unsigned
        // variables and slice lengths are non-negative).
        // Unconditionally sound to assert: each is a truth of the Edda type
        // system, so it can only rule out models no Edda execution reaches.
        for axiom in translator.sort_axioms() {
            solver.assert(axiom);
        }

        let start = Instant::now();
        let result = solver.check();
        let elapsed = start.elapsed();

        project_result(&solver, &translator, obligation, result, timeout, elapsed)
    }
}

// Apply the per-obligation timeout via Z3 solver params. Returns Some(outcome)
// on configuration failure so the caller can short-circuit with Unknown.
fn configure_timeout<'ctx>(
    ctx: &'ctx Context,
    solver: &z3::Solver<'ctx>,
    timeout: Duration,
) -> Option<DischargeOutcome> {
    let ms = timeout.as_millis();
    let ms_u32 = if ms > u32::MAX as u128 {
        u32::MAX
    } else {
        ms as u32
    };
    let mut params = Params::new(ctx);
    params.set_u32("timeout", ms_u32);
    solver.set_params(&params);
    None
}

// Assert every predicate in the obligation's context. Translation failures
// produce DischargeOutcome::Unknown so the caller never gets a misleading
// Unsat from a partially-translated context.
fn assert_context<'ctx>(
    solver: &z3::Solver<'ctx>,
    translator: &mut Translator<'ctx, '_>,
    obligation: &Obligation,
) -> Result<(), DischargeOutcome> {
    for (i, ctx_pred) in obligation.context.iter().enumerate() {
        match translator.translate_bool(ctx_pred) {
            Ok(b) => solver.assert(&b),
            Err(err) => {
                return Err(translation_failure(format!(
                    "context predicate {i}: {err}"
                )));
            }
        }
    }
    Ok(())
}

// Translate the goal and assert its negation. To prove G from C, we ask Z3 to
// satisfy C ∧ ¬G; an unsat result means G holds in every model of C.
fn assert_negated_goal<'ctx>(
    solver: &z3::Solver<'ctx>,
    translator: &mut Translator<'ctx, '_>,
    obligation: &Obligation,
) -> Result<(), DischargeOutcome> {
    match translator.translate_bool(&obligation.goal) {
        Ok(goal) => {
            solver.assert(&goal.not());
            Ok(())
        }
        Err(err) => Err(translation_failure(format!("goal: {err}"))),
    }
}

// Project the four Z3 SatResult / reason-unknown cases into a DischargeOutcome.
fn project_result<'ctx>(
    solver: &z3::Solver<'ctx>,
    translator: &Translator<'ctx, '_>,
    obligation: &Obligation,
    result: SatResult,
    timeout: Duration,
    elapsed: Duration,
) -> DischargeOutcome {
    match result {
        SatResult::Unsat => DischargeOutcome::Unsat {
            certificate: capture_smt_certificate(solver, obligation),
        },
        SatResult::Sat => {
            let counterexample = match solver.get_model() {
                Some(model) => build_counterexample(&model, translator),
                None => Counterexample::empty(),
            };
            DischargeOutcome::Sat { counterexample }
        }
        SatResult::Unknown => {
            let reason = solver.get_reason_unknown();
            if is_timeout_reason(reason.as_deref()) {
                DischargeOutcome::Timeout {
                    configured: timeout,
                    elapsed,
                }
            } else {
                DischargeOutcome::Unknown { reason }
            }
        }
    }
}

// Build the SMT certificate from Z3's proof. Z3 returns `Some(proof)` here
// because `Config::set_proof_generation(true)` was set; if it's `None`
// (older binding behaviour, or Z3 falling back to a non-proof tactic), we
// fall back to a placeholder certificate so the discharge still succeeds —
// v0.1 is capture-only and the verifier never reads this blob.
fn capture_smt_certificate<'ctx>(
    solver: &z3::Solver<'ctx>,
    obligation: &Obligation,
) -> crate::certificate::ProofCertificate {
    // `Solver::get_proof()` returns `Option<impl Ast<'ctx>>` — an opaque
    // type that implements `fmt::Debug` (via the `Ast` trait's super-trait)
    // but not `fmt::Display` directly. Z3 emits its proof as an S-expression
    // through the same Debug renderer used for any other AST node, so we
    // format via `{:?}` to obtain the proof S-expression bytes.
    let proof_sexpr = match solver.get_proof() {
        Some(proof) => format!("{:?}", proof),
        None => return mint_placeholder_certificate(obligation),
    };
    mint_smt_certificate(obligation, z3_binding_version(), &proof_sexpr)
}

// Best-effort solver-version string. The z3 crate at 0.12 does not surface
// `Z3_get_version` through a Rust accessor; the static-link build pins a
// known upstream Z3 source revision, so we record the binding version as the
// audit anchor. Per `03-certificate.md` §3.1, this field is for audit only
// and is not used in verifier dispatch.
fn z3_binding_version() -> &'static str {
    "z3.rs@0.12.1+static-link"
}

fn is_timeout_reason(reason: Option<&str>) -> bool {
    match reason {
        Some(s) => {
            let lower = s.to_ascii_lowercase();
            lower.contains("timeout") || lower.contains("canceled") || lower.contains("cancelled")
        }
        None => false,
    }
}

fn translation_failure(detail: String) -> DischargeOutcome {
    DischargeOutcome::Unknown {
        reason: Some(format!("translation failure: {detail}")),
    }
}

// Walk the translator's variable cache, evaluate each in the model, and
// collect a [`Counterexample`]. Variables whose sort can't be projected to a
// ConcreteValue (records, sums, slices) are stringified via Z3's Display.
fn build_counterexample<'ctx>(
    model: &Model<'ctx>,
    translator: &Translator<'ctx, '_>,
) -> Counterexample {
    let mut cx = Counterexample::empty();
    for (name, value) in translator.var_bindings() {
        if let Some(concrete) = evaluate_binding(model, value) {
            cx.push(name.clone(), concrete);
        }
    }
    cx
}

fn evaluate_binding<'ctx>(
    model: &Model<'ctx>,
    value: &Dynamic<'ctx>,
) -> Option<ConcreteValue> {
    // Try each known sort projection in turn. The translator only constructs
    // Int / Bool / Array variables today; we fall back to Display for any
    // future sort we haven't taught the model walker about yet.
    if let Some(b) = value.as_bool() {
        let evaluated = model.eval(&b, true)?;
        let v = evaluated.as_bool()?;
        return Some(ConcreteValue::Bool(v));
    }
    if let Some(i) = value.as_int() {
        let evaluated = model.eval(&i, true)?;
        return Some(int_to_concrete(&evaluated));
    }
    if let Some(_a) = value.as_array() {
        // Slices stringify — there's no compact concrete form for an array
        // value beyond Z3's lambda printout.
        let rendered = format!("{value}");
        return Some(ConcreteValue::String(rendered));
    }
    let rendered = format!("{value}");
    Some(ConcreteValue::String(rendered))
}

fn int_to_concrete<'ctx>(evaluated: &Int<'ctx>) -> ConcreteValue {
    if let Some(i) = evaluated.as_i64() {
        ConcreteValue::Signed(i as i128)
    } else if let Some(u) = evaluated.as_u64() {
        ConcreteValue::Unsigned(u as u128)
    } else {
        ConcreteValue::String(format!("{evaluated}"))
    }
}
