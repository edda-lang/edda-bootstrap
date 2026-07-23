//! Locked method-name catalogue.
//!
//! Every operation the MCP wire admits has a `&'static str` here. The
//! catalogue is grouped by namespace per `mcp-protocol.md` §4 and the
//! string values are the wire form clients send in `request.method`.
//!
//! # Stability
//!
//! Every constant in this module is wire-frozen. Renaming any string —
//! whether the namespace prefix, the leaf name, or the dot separator —
//! is a wire break. Adding a new method requires a spec update first;
//! once the spec admits it, the constant lands here and the
//! dispatcher gains a route.

/// `client.*` operations — handshake, cancellation, project lifecycle.
pub mod client {
    /// First request on every session. Negotiates protocol version
    /// and features per `mcp-protocol.md` §3.
    pub const HANDSHAKE: &str = "client.handshake";
    /// Notification cancelling an in-flight request per
    /// `mcp-protocol.md` §12.
    pub const CANCEL: &str = "client.cancel";
    /// Open a project against this session's daemon. Daemon-side
    /// equivalent of `edda_daemon::Daemon::open_project`.
    pub const OPEN_PROJECT: &str = "client.open_project";
    /// Close the open project per `edda_daemon::Daemon::close_project`.
    pub const CLOSE_PROJECT: &str = "client.close_project";
    /// Open a document overlay per `edda_daemon::Daemon::open_document`.
    pub const OPEN_DOCUMENT: &str = "client.open_document";
    /// Apply an overlay change per `edda_daemon::Daemon::apply_change`.
    pub const APPLY_CHANGE: &str = "client.apply_change";
    /// Close the document overlay per `edda_daemon::Daemon::close_document`.
    pub const CLOSE_DOCUMENT: &str = "client.close_document";
    /// Report static information about the daemon (server name,
    /// version, namespaces). Cheap, no project required.
    pub const SERVER_INFO: &str = "client.server_info";
}

/// `build.*` operations — the §5 verb namespace.
pub mod build {
    /// `edda build` equivalent.
    pub const COMPILE: &str = "build.compile";
    /// `edda check` equivalent — stops after typecheck.
    pub const TYPECHECK: &str = "build.typecheck";
    /// `edda run` equivalent.
    pub const RUN: &str = "build.run";
    /// `edda test` equivalent.
    pub const TEST: &str = "build.test";
    /// `edda bench` equivalent.
    pub const BENCH: &str = "build.bench";
    /// `edda fmt` equivalent. The spec writes this as `build.format`.
    pub const FORMAT: &str = "build.format";
    /// `edda lint` equivalent.
    pub const LINT: &str = "build.lint";
    /// `edda clean` equivalent.
    pub const CLEAN: &str = "build.clean";
}

/// `codegen.*` operations — promote/demote/regenerate/gc plus the
/// short-name → full-hash query per `mcp-protocol.md` §6.
pub mod codegen {
    /// Move artifact from cache tier to repo tier.
    pub const PROMOTE: &str = "codegen.promote";
    /// Move artifact from repo tier to cache tier.
    pub const DEMOTE: &str = "codegen.demote";
    /// Force regenerate an artifact (single name or wildcard).
    pub const REGENERATE: &str = "codegen.regenerate";
    /// Garbage-collect by tier.
    pub const GC: &str = "codegen.gc";
    /// Resolve a short artifact name to the full 64-character hash.
    pub const FULL_HASH: &str = "codegen.full_hash";
}

/// `inspect.*` operations — read-side queries per
/// `mcp-protocol.md` §7. Only `parsed_ast` and `diagnostics` route
/// end-to-end through the daemon currently; the rest return
/// `method_not_implemented` until the daemon's underlying query
/// surface grows.
pub mod inspect {
    /// Daemon query: parsed AST for a file (overlay-aware).
    pub const PARSED_AST: &str = "inspect.parsed_ast";
    /// Daemon query: diagnostics whose primary span points
    /// at a file.
    pub const DIAGNOSTICS: &str = "inspect.diagnostics";
    /// `inspectability.md` §2.
    pub const ARTIFACT_OF_INVOCATION: &str = "inspect.artifact_of_invocation";
    /// `inspectability.md` §2.
    pub const ARTIFACT_OF_NAME: &str = "inspect.artifact_of_name";
    /// `inspectability.md` §2.
    pub const ARTIFACT_OF_SPEC_BODY_ITEM: &str = "inspect.artifact_of_spec_body_item";
    /// `inspectability.md` §3.
    pub const SOURCE_OF_ARTIFACT: &str = "inspect.source_of_artifact";
    /// `inspectability.md` §3.
    pub const SOURCE_OF_ARTIFACT_ITEM: &str = "inspect.source_of_artifact_item";
    /// `inspectability.md` §3.
    pub const INVOCATION_SITES_OF_ARTIFACT: &str = "inspect.invocation_sites_of_artifact";
    /// `inspectability.md` §4.
    pub const NESTED_DEPS: &str = "inspect.nested_deps";
    /// `inspectability.md` §4.
    pub const TRANSITIVE_DEPS: &str = "inspect.transitive_deps";
    /// `inspectability.md` §4.
    pub const DIRECT_CONSUMERS: &str = "inspect.direct_consumers";
    /// `inspectability.md` §4.
    pub const TRANSITIVE_CONSUMERS: &str = "inspect.transitive_consumers";
    /// `inspectability.md` §4. Streamable.
    pub const LIVE_ARTIFACTS: &str = "inspect.live_artifacts";
    /// `inspectability.md` §4. Streamable.
    pub const STALE_ARTIFACTS: &str = "inspect.stale_artifacts";
    /// `inspectability.md` §4.
    pub const GC_ELIGIBLE_ARTIFACTS: &str = "inspect.gc_eligible_artifacts";
    /// `inspectability.md` §5.
    pub const BODY_DIFF: &str = "inspect.body_diff";
    /// `inspectability.md` §5.
    pub const CASCADE_FROM_EDIT: &str = "inspect.cascade_from_edit";
}

/// `edit.*` operations — structural edits per
/// `structural-edits.md` §§3-8. Only the catalogue ships so far;
/// every concrete operation is `method_not_implemented` until
/// `edda-daemon` grows the structural-edit surface.
pub mod edit {
    /// All-or-nothing multi-edit transaction.
    pub const TRANSACTION: &str = "edit.transaction";
    /// `declaration.rename` per `structural-edits.md` §3.
    pub const DECLARATION_RENAME: &str = "edit.declaration.rename";
    /// `signature.parameter.add` per `structural-edits.md` §3.
    pub const SIGNATURE_PARAMETER_ADD: &str = "edit.signature.parameter.add";
    /// `signature.parameter.remove` per `structural-edits.md` §3.
    pub const SIGNATURE_PARAMETER_REMOVE: &str = "edit.signature.parameter.remove";
    /// `signature.return_type.set` per `structural-edits.md` §3.
    pub const SIGNATURE_RETURN_TYPE_SET: &str = "edit.signature.return_type.set";
    /// `effect_row.add` per `structural-edits.md` §3.
    pub const EFFECT_ROW_ADD: &str = "edit.effect_row.add";
    /// `effect_row.remove` per `structural-edits.md` §3.
    pub const EFFECT_ROW_REMOVE: &str = "edit.effect_row.remove";
    /// `refactor.rename_with_cascade` per `structural-edits.md` §8.
    pub const REFACTOR_RENAME_WITH_CASCADE: &str = "edit.refactor.rename_with_cascade";
    /// `refactor.extract_function` per `structural-edits.md` §8.
    pub const REFACTOR_EXTRACT_FUNCTION: &str = "edit.refactor.extract_function";
    /// `refactor.inline_function` per `structural-edits.md` §8.
    pub const REFACTOR_INLINE_FUNCTION: &str = "edit.refactor.inline_function";
}

/// `typecheck.*` operations — inference + refinement query
/// surface per `mcp-protocol.md` §9. Every leaf returns
/// `method_not_implemented` currently — the daemon's
/// `query` surface does not yet expose typed information by position.
pub mod typecheck {
    /// Inferred / declared type at a source position.
    pub const TYPE_AT: &str = "typecheck.type_at";
    /// Inferred parameter mode at a position.
    pub const MODE_AT: &str = "typecheck.mode_at";
    /// Inferred / declared effect row of the enclosing function.
    pub const EFFECT_ROW_AT: &str = "typecheck.effect_row_at";
    /// Refinement obligations at a position.
    pub const REFINEMENT_OBLIGATIONS_AT: &str = "typecheck.refinement_obligations_at";
    /// `@unverified` / `@trust` annotations in scope at a position.
    pub const TRUST_POINTS_IN_SCOPE: &str = "typecheck.trust_points_in_scope";
    /// Whether the function enclosing a position is comptime-pure.
    pub const COMPTIME_PURE_STATUS: &str = "typecheck.comptime_pure_status";
    /// Discharged refinement obligations for a file.
    pub const DISCHARGED_REFINEMENTS: &str = "typecheck.discharged_refinements";
}

/// `layout.*` operations per `mcp-protocol.md` §10. Every leaf returns
/// `method_not_implemented` currently — `edda-comptime`'s
/// `Layout::of_ty` surface is reachable through `edda-types` but not
/// yet through `edda-daemon`'s query layer.
pub mod layout {
    /// `size_of(T)`.
    pub const SIZE_OF: &str = "layout.size_of";
    /// `align_of(T)`.
    pub const ALIGN_OF: &str = "layout.align_of";
    /// `offset_of(T, field)`.
    pub const OFFSET_OF: &str = "layout.offset_of";
    /// Attribute set on a declaration.
    pub const ATTRIBUTES_OF: &str = "layout.attributes_of";
    /// `@repr` kind for a type.
    pub const REPR_OF: &str = "layout.repr_of";
    /// Per-field layout.
    pub const FIELD_LAYOUT: &str = "layout.field_layout";
    /// `@abi` convention for a function.
    pub const ABI_OF: &str = "layout.abi_of";
}

/// Streaming-related method names.
pub mod stream {
    /// Server → client chunk notification.
    pub const CHUNK: &str = "stream.chunk";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments_ok(name: &str) -> bool {
        let mut segments = name.split('.');
        let namespace = segments.next().unwrap_or("");
        let leaf = segments.next().unwrap_or("");
        // namespace.leaf[.leaf]*; every segment is lowercase_snake_case
        if namespace.is_empty() || leaf.is_empty() {
            return false;
        }
        name.split('.')
            .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
    }

    #[test]
    fn every_locked_method_has_valid_segments() {
        let names = [
            client::HANDSHAKE,
            client::CANCEL,
            client::OPEN_PROJECT,
            client::CLOSE_PROJECT,
            client::OPEN_DOCUMENT,
            client::APPLY_CHANGE,
            client::CLOSE_DOCUMENT,
            client::SERVER_INFO,
            build::COMPILE,
            build::TYPECHECK,
            build::RUN,
            build::TEST,
            build::BENCH,
            build::FORMAT,
            build::LINT,
            build::CLEAN,
            codegen::PROMOTE,
            codegen::DEMOTE,
            codegen::REGENERATE,
            codegen::GC,
            codegen::FULL_HASH,
            inspect::PARSED_AST,
            inspect::DIAGNOSTICS,
            edit::TRANSACTION,
            edit::DECLARATION_RENAME,
            typecheck::TYPE_AT,
            layout::SIZE_OF,
            stream::CHUNK,
        ];
        for name in names {
            assert!(segments_ok(name), "method name {:?} malformed", name);
        }
    }
}
