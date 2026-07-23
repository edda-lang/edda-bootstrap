//! TyEnv — lexical type-and-mode environment.
//!
//! Stack of per-block frames. Each frame maps a binding's [`Symbol`]
//! to its [`TyId`] *and* its [`BindingState`] (the §4 mode lattice).
//! Inference helpers consult and mutate this env as they walk the
//! HIR; branch joins reconcile per-binding states by GLB
//! (`inference-rules.md §4` *Branching merges states by GLB*).

use std::collections::HashMap;

use edda_intern::Symbol;

use super::mode::BindingState;
use crate::ty::TyId;

/// Per-binding payload stored inside [`TyEnv`]: the binding's [`TyId`],
/// its current [`BindingState`], and whether it may be mutably borrowed.
/// The state is mutated in place as the inference walk encounters
/// initialisers, reassignments, and `take` / `init` / `mutable`
/// call-arg modes; at branch joins the per-binding [`BindingState`]s are
/// merged by GLB. `mutable` is immutable metadata set once at bind time.
#[derive(Clone, Debug)]
struct BindingInfo {
    ty: TyId,
    state: BindingState,
    mutable: bool,
}

/// Lexical type-and-mode environment — the `Γ` plus `Σ` of
/// `inference-rules.md §1a`.
///
/// Maps a binding's [`Symbol`] to its [`TyId`] *and* its
/// [`BindingState`] (uninit / valid / consumed). Scopes are pushed at
/// block entry and popped at block exit. Lookups walk the frame stack
/// inside-out so an inner binding shadows an outer one of the same
/// name.
#[derive(Clone)]
pub(crate) struct TyEnv {
    frames: Vec<HashMap<Symbol, BindingInfo>>,
}

/// Opaque snapshot returned by [`TyEnv::restrict_mutability`] and
/// consumed by [`TyEnv::restore_mutability`].
pub(crate) struct MutabilitySnapshot(Vec<Vec<(Symbol, bool)>>);

impl TyEnv {
    /// Construct an empty environment with one open frame.
    pub fn new() -> Self {
        Self {
            frames: vec![HashMap::new()],
        }
    }

    /// Push a fresh inner frame. Called at the start of every block.
    pub fn enter_scope(&mut self) {
        self.frames.push(HashMap::new());
    }

    /// Pop the innermost frame.
    ///
    /// # Panics
    ///
    /// Debug-panics if popping would leave the env without a frame.
    pub fn exit_scope(&mut self) {
        debug_assert!(
            self.frames.len() > 1,
            "TyEnv::exit_scope underflow — every exit must be paired with an enter"
        );
        self.frames.pop();
    }

    /// Bind `name` to `ty` in the innermost frame at
    /// [`BindingState::Valid`]. Shadows any outer binding of the same
    /// name. Use [`TyEnv::bind_with_state`] for the `var x: T`
    /// (uninit) form.
    pub fn bind(&mut self, name: Symbol, ty: TyId) {
        self.bind_with_state(name, ty, BindingState::Valid);
    }

    /// Bind `name` to `ty` in the innermost frame with the supplied
    /// initial state. Used by the `var x: T` form
    /// ([`BindingState::Uninit`]) and by tests that need to drive
    /// the mode tracker from non-default states. Defaults `mutable` to
    /// `true` (permissive) — callers that know the binding is immutable
    /// (`let` locals, `Default`-mode params) use [`TyEnv::bind_with_state_mut`].
    pub fn bind_with_state(&mut self, name: Symbol, ty: TyId, state: BindingState) {
        self.bind_with_state_mut(name, ty, state, true);
    }

    /// Bind `name` with an explicit [`BindingState`] *and* mutability.
    /// `mutable == false` marks the binding immutable — a `let` local or
    /// a `Default`-mode parameter — so the mode checker rejects a
    /// `mutable` / `init` borrow of it (or of one of its fields), which
    /// would otherwise be lowered as a byval copy and silently lose the
    /// write.
    pub fn bind_with_state_mut(
        &mut self,
        name: Symbol,
        ty: TyId,
        state: BindingState,
        mutable: bool,
    ) {
        self.frames
            .last_mut()
            .expect("TyEnv has at least one frame")
            .insert(name, BindingInfo { ty, state, mutable });
    }

    /// Look `name`'s [`TyId`] up in the frame stack inside-out.
    /// Returns the innermost binding for `name`, or [`None`] if
    /// undefined.
    pub fn lookup(&self, name: Symbol) -> Option<TyId> {
        self.lookup_info(name).map(|info| info.ty)
    }

    /// Look up the state of a specific field on a binding. Routes
    /// through [`BindingState::field_state`] — `Valid` binding ⇒
    /// every field valid; `Uninit`/`Consumed` ⇒ every field shares
    /// that state; `PartialInit(F)` ⇒ field `f` is `Valid` iff
    /// `f ∈ F`. Returns `None` if `name` is not in any open frame.
    pub fn lookup_field_state(&self, name: Symbol, field: Symbol) -> Option<BindingState> {
        self.lookup_info(name).map(|info| info.state.field_state(field))
    }

    /// Look `name`'s current [`BindingState`] up in the frame stack
    /// inside-out. Returns [`None`] if the binding is undefined.
    pub fn lookup_state(&self, name: Symbol) -> Option<BindingState> {
        self.lookup_info(name).map(|info| info.state.clone())
    }

    /// Whether `name` admits a `mutable` / `init` borrow. `Some(false)`
    /// for a `let` local or `Default`-mode parameter; `Some(true)` for
    /// `var`/`uninit` locals and `mutable`/`init`/`take` params (and any
    /// permissively-bound name). [`None`] if `name` is undefined.
    pub fn lookup_mutable(&self, name: Symbol) -> Option<bool> {
        self.lookup_info(name).map(|info| info.mutable)
    }

    fn lookup_info(&self, name: Symbol) -> Option<&BindingInfo> {
        for frame in self.frames.iter().rev() {
            if let Some(info) = frame.get(&name) {
                return Some(info);
            }
        }
        None
    }

    /// Transition the innermost binding of `name` to `new_state`.
    /// Walks frames inside-out and mutates the first match. Returns
    /// `false` if `name` is not defined in any open frame.
    pub fn transition(&mut self, name: Symbol, new_state: BindingState) -> bool {
        for frame in self.frames.iter_mut().rev() {
            if let Some(info) = frame.get_mut(&name) {
                info.state = new_state;
                return true;
            }
        }
        false
    }

    /// Merge `other`'s per-binding states into `self` by GLB
    /// (`inference-rules.md §4`, *Branching merges states by GLB*).
    /// Both envs must have the same frame structure — the typical
    /// caller clones the env before each branch and merges the
    /// post-branch states here.
    pub fn merge_glb(&mut self, other: &TyEnv) {
        debug_assert_eq!(
            self.frames.len(),
            other.frames.len(),
            "TyEnv::merge_glb requires matching frame depth",
        );
        for (mine, theirs) in self.frames.iter_mut().zip(other.frames.iter()) {
            for (name, their_info) in theirs.iter() {
                if let Some(my_info) = mine.get_mut(name) {
                    my_info.state = my_info.state.glb(&their_info.state);
                }
                // Binding only present in `other` — should not happen
                // when both branches start from a shared snapshot, but
                // if it does the binding is in a partial state visible
                // to neither branch consistently. Drop it.
            }
        }
    }

    /// Number of currently-open frames (always ≥ 1).
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// Force every binding in every currently-open frame immutable,
    /// snapshotting each one's prior `mutable` flag so
    /// [`TyEnv::restore_mutability`] can undo it later.
    ///
    /// Call this *before* pushing a spawn body's own child scope: the
    /// restriction must cover only the frames that existed in the
    /// *parent* scope, so a binding added afterward (e.g. a spawn's
    /// `take`-arg locals, bound into the freshly [`TyEnv::enter_scope`]d
    /// frame) is unaffected and keeps its normal mutability.
    /// [`super::mode::transitions::reject_immutable_borrow`] is what
    /// turns a forced-`false` flag into a `mode_violation` diagnostic
    /// when the body borrows an outer binding `mutable` / `init`;
    /// `take` is exempt (it is a move, not a write-through).
    pub fn restrict_mutability(&mut self) -> MutabilitySnapshot {
        MutabilitySnapshot(
            self.frames
                .iter_mut()
                .map(|frame| {
                    frame
                        .iter_mut()
                        .map(|(sym, info)| {
                            let prior = info.mutable;
                            info.mutable = false;
                            (*sym, prior)
                        })
                        .collect()
                })
                .collect(),
        )
    }

    /// Undo a [`TyEnv::restrict_mutability`] snapshot, restoring every
    /// affected binding's prior `mutable` flag.
    ///
    /// # Panics
    ///
    /// Debug-panics if `snapshot`'s frame count no longer matches
    /// `self`'s — the caller must restore at the same frame depth it
    /// restricted at.
    pub fn restore_mutability(&mut self, snapshot: MutabilitySnapshot) {
        debug_assert_eq!(
            self.frames.len(),
            snapshot.0.len(),
            "TyEnv::restore_mutability requires the same frame depth restrict_mutability saved",
        );
        for (frame, saved) in self.frames.iter_mut().zip(snapshot.0.into_iter()) {
            for (sym, mutable) in saved {
                if let Some(info) = frame.get_mut(&sym) {
                    info.mutable = mutable;
                }
            }
        }
    }

    /// Iterate every visible binding (symbol + state) in any open
    /// frame, innermost first. Used by the loop re-entry check to
    /// compare pre-loop and post-loop states.
    pub(super) fn iter_states(&self) -> impl Iterator<Item = (Symbol, BindingState)> + '_ {
        self.frames
            .iter()
            .rev()
            .flat_map(|f| f.iter().map(|(s, info)| (*s, info.state.clone())))
    }

    /// Iterate the innermost frame's bindings as `(symbol, state, ty)`.
    /// Used by the `linear`-unconsumed scope-exit sweep, which must see
    /// exactly the bindings declared in the about-to-be-popped block —
    /// the innermost frame holds precisely those (every `bind` writes the
    /// innermost frame). Outer-frame bindings (including function
    /// parameters) are deliberately excluded: their scope has not ended.
    pub(super) fn iter_top_frame(
        &self,
    ) -> impl Iterator<Item = (Symbol, BindingState, TyId)> + '_ {
        self.frames
            .last()
            .into_iter()
            .flat_map(|f| f.iter().map(|(s, info)| (*s, info.state.clone(), info.ty)))
    }
}

impl Default for TyEnv {
    fn default() -> Self {
        Self::new()
    }
}
