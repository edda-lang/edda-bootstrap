//! Implicit spec-invocation requests emitted by inference.
//!
//! Per `docs/types/inference-rules.md §3`, range literals (`lo..<hi`)
//! and `none` patterns at use sites trigger an implicit invocation of
//! the corresponding `std.core.range.Range(<T>)` or
//! `std.core.option.Option(<T>)` spec when the generated module is not
//! already in file scope. Inference records the request; codegen
//! consumes it during spec instantiation.
//!
//! `<scope>.spawn { body }` (`corpus/edda-codex/language/05-
//! concurrency-coherence.md` §2.2) follows the same pattern for
//! `std.task.Task(<T>)`, where `T` is the spawned body's return type.
//!
//! `edda-driver`'s `collect_roots` consumes these requests as codegen
//! roots, so the generated `Range_<T>` / `Option_<T>` / `Task_<T>`
//! modules materialise as artifacts. At the *type* level, `Range` /
//! `Option` requests still synthesise the error sentinel (call sites
//! that reach for the generated nominal propagate the error cascade);
//! `Task` types transparently —
//! `.spawn` / `.await` synthesise the task's semantic result type `T`,
//! and the linear handle is a MIR-level notion.

use edda_span::Span;

use crate::ty::TyId;

/// Standard-library spec for which inference may emit implicit
/// invocation requests.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ImplicitSpec {
    /// `spec std.core.range.Range(<T>)` — triggered by `lo..<hi` /
    /// `lo..=hi` range literals.
    Range,
    /// `spec std.core.option.Option(<T>)` — triggered by the `none`
    /// pattern (when the type-arg is inferable from context).
    /// The request type is implemented; the consumer arm in
    /// `synth_expr` / pattern-checking is a separate follow-up.
    Option,
    /// `spec std.task.Task(<T>)` — triggered by `<scope>.spawn { body }`,
    /// where `T` is the spawned body's return type.
    Task,
}

impl ImplicitSpec {
    /// The fully-qualified spec name as it appears in a hand-written
    /// `spec` invocation. Spec-locked; changes are user-visible.
    pub fn qualified_name(self) -> &'static str {
        match self {
            ImplicitSpec::Range => "std.core.range.Range",
            ImplicitSpec::Option => "std.core.option.Option",
            ImplicitSpec::Task => "std.task.Task",
        }
    }
}

/// One implicit-spec invocation that the inference pass detected.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct ImplicitSpecRequest {
    /// The stdlib spec being instantiated.
    pub kind: ImplicitSpec,
    /// The single comptime type argument — every `ImplicitSpec`
    /// kind (`Range`, `Option`, `Task`) is unary in `Type`.
    pub type_arg: TyId,
    /// Source span of the first inference site that triggered this
    /// request. Subsequent triggers for the same instantiation reuse
    /// the existing record; the span helps codegen attribute back
    /// to source.
    pub span: Span,
}
