//! Name-keyed binding environment for the comptime HIR evaluator.
//!
//! Mirrors the native cteval's `ComptimeEnv`
//! (`compiler/lib/cteval/src/state/env.ea`): a flat binding stack with
//! backward-scanning lookup, in-place assignment, and depth-based
//! truncation for lexical scoping. The native env keys bindings by
//! resolver `DefId`; this side keys by interned name (`Symbol`)
//! because [`edda_types::HirPath`] carries no binding ids — the
//! backward scan makes an inner scope's binding shadow an outer
//! scope's under the same name, and the no-shadowing-within-scope rule
//! is enforced upstream by the typechecker.
//!
//! Scoping protocol (same as the native `evaluate_block`): callers
//! save [`ComptimeEnv::depth`] at block entry and
//! [`ComptimeEnv::truncate_to`] that depth on every exit path, so
//! bindings never leak out of the block that declared them.

use edda_intern::Symbol;

use crate::value::Value;

/// One `name -> value` entry on the binding stack.
#[derive(Clone, Debug)]
struct Binding {
    /// Interned binding name.
    name: Symbol,
    /// Current comptime value.
    value: Value,
}

/// The comptime evaluator's binding environment.
///
/// Backing store for `let` / `var` bindings declared inside a
/// `comptime { … }` body (and, once user-function calls land, callee
/// parameter bindings). Operations mirror the native cteval env:
/// [`push_binding`](Self::push_binding), [`lookup`](Self::lookup),
/// [`assign_binding`](Self::assign_binding), [`depth`](Self::depth),
/// [`truncate_to`](Self::truncate_to).
#[derive(Default, Debug)]
pub struct ComptimeEnv {
    /// Binding stack in declaration order.
    bindings: Vec<Binding>,
}

impl ComptimeEnv {
    /// Construct an empty environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a fresh binding. Does not check for an existing binding
    /// under the same name — shadowing across scopes is admitted and
    /// resolved by the backward scan in [`lookup`](Self::lookup).
    pub fn push_binding(&mut self, name: Symbol, value: Value) {
        self.bindings.push(Binding { name, value });
    }

    /// Resolve `name` to its innermost bound value, scanning from the
    /// most recent binding backward. `None` when no binding under
    /// `name` is live.
    pub fn lookup(&self, name: Symbol) -> Option<&Value> {
        self.lookup_from(0, name)
    }

    /// [`lookup`](Self::lookup) restricted to bindings at stack index
    /// `>= frame_base`. A user-function call frame sets its base to
    /// the depth its parameters were pushed at, so the callee's body
    /// cannot accidentally resolve a caller-local binding that happens
    /// to share a name (the env is name-keyed; the native cteval's
    /// DefId keys make this collision impossible there).
    pub fn lookup_from(&self, frame_base: usize, name: Symbol) -> Option<&Value> {
        self.bindings[frame_base..]
            .iter()
            .rev()
            .find(|b| b.name == name)
            .map(|b| &b.value)
    }

    /// Overwrite the innermost binding under `name` with `value`.
    /// Returns `false` (leaving the env untouched) when no binding
    /// under `name` is live — the caller surfaces that as an
    /// assignment-to-undeclared diagnostic.
    pub fn assign_binding(&mut self, name: Symbol, value: Value) -> bool {
        self.assign_binding_from(0, name, value)
    }

    /// [`assign_binding`](Self::assign_binding) restricted to bindings
    /// at stack index `>= frame_base` — the write-side twin of
    /// [`lookup_from`](Self::lookup_from).
    pub fn assign_binding_from(&mut self, frame_base: usize, name: Symbol, value: Value) -> bool {
        match self.bindings[frame_base..]
            .iter_mut()
            .rev()
            .find(|b| b.name == name)
        {
            Some(binding) => {
                binding.value = value;
                true
            }
            None => false,
        }
    }

    /// Current binding-stack depth. Saved at block entry and passed
    /// back to [`truncate_to`](Self::truncate_to) at block exit.
    pub fn depth(&self) -> usize {
        self.bindings.len()
    }

    /// Pop bindings until the stack is `target_depth` deep. A no-op
    /// when the stack is already at (or below) `target_depth`.
    pub fn truncate_to(&mut self, target_depth: usize) {
        self.bindings.truncate(target_depth);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::IntValue;
    use edda_intern::Interner;
    use edda_types::Primitive;

    fn int(v: i128) -> Value {
        Value::Int(IntValue::new_signed(Primitive::I64, v))
    }

    fn int_payload(v: &Value) -> i128 {
        match v {
            Value::Int(i) => i.as_i128().unwrap(),
            other => panic!("expected int, got {other:?}"),
        }
    }

    #[test]
    fn lookup_finds_innermost_shadow() {
        let interner = Interner::new();
        let x = interner.intern("x");
        let mut env = ComptimeEnv::new();
        env.push_binding(x, int(1));
        env.push_binding(x, int(2));
        assert_eq!(int_payload(env.lookup(x).unwrap()), 2);
    }

    #[test]
    fn lookup_missing_is_none() {
        let interner = Interner::new();
        let mut env = ComptimeEnv::new();
        env.push_binding(interner.intern("x"), int(1));
        assert!(env.lookup(interner.intern("y")).is_none());
    }

    #[test]
    fn assign_overwrites_innermost_only() {
        let interner = Interner::new();
        let x = interner.intern("x");
        let mut env = ComptimeEnv::new();
        env.push_binding(x, int(1));
        env.push_binding(x, int(2));
        assert!(env.assign_binding(x, int(9)));
        assert_eq!(int_payload(env.lookup(x).unwrap()), 9);
        env.truncate_to(1);
        assert_eq!(int_payload(env.lookup(x).unwrap()), 1);
    }

    #[test]
    fn assign_to_unbound_returns_false() {
        let interner = Interner::new();
        let mut env = ComptimeEnv::new();
        assert!(!env.assign_binding(interner.intern("x"), int(1)));
        assert_eq!(env.depth(), 0);
    }

    #[test]
    fn truncate_restores_saved_depth() {
        let interner = Interner::new();
        let x = interner.intern("x");
        let y = interner.intern("y");
        let mut env = ComptimeEnv::new();
        env.push_binding(x, int(1));
        let saved = env.depth();
        env.push_binding(y, int(2));
        assert_eq!(env.depth(), 2);
        env.truncate_to(saved);
        assert_eq!(env.depth(), 1);
        assert!(env.lookup(y).is_none());
        assert!(env.lookup(x).is_some());
    }
}
