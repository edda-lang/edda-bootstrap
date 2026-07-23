# edda-bootstrap

Edda is a systems programming language designed for LLM-as-primary-author. Effect rows, parameter-mode linearity, refinement types over a decidable SMT fragment, and content-addressed spec instantiation in place of generics. This repository hosts `edda-bootstrap`, the v0.1 reference compiler — written in Rust 2024, targeting LLVM 18 via `inkwell`, discharging refinements through Z3 via `z3.rs`.

The language design — the charter, the roadmap, and the seven canonical language docs — lives in the Edda codex, maintained and published separately from this repository.

## Language at a glance

- **Effect rows.** Closed rows track three pure-effect kinds — `err`, `panic`, `yield` — alongside capability parameters. `?` propagates an `err: E` entry into the enclosing function's row at compile time. Rows name parameters held, not bindings derived from them.
- **Parameter modes.** Every parameter binds in one of `let` / `mutable` / `take` / `init`. A per-binding `Uninit` / `Valid` / `PartialInit(F)` / `Consumed` lattice enforces linearity statically, including per-field state on aggregates.
- **Refinement types.** `where`, `requires`, and `ensures` clauses lower to predicates over EUF + LIA + boolean + extensional arrays — a decidable fragment. Each discharged obligation produces a byte-format proof certificate; `@unverified` and `@trust` are the two escape hatches.
- **No comments.** Edda source admits no free-text comments — the lexer rejects them. Claims about code live in effect rows, refinements, and attributes; item descriptions are derived from checked facts into a structure index (`index.toon`), never authored by hand.
- **Comptime and specs.** `comptime`-keyword expressions evaluate against the typed HIR. `spec` declarations parameterize over `Type` (the meta-type) and primitives; each invocation is monomorphized to a content-addressed artifact keyed by `BLAKE3(qualified name ⊕ argument tuple ⊕ canonical body ⊕ nested-invocation set)`.
- **Cascade build with reachability-driven codegen.** The pipeline is parse → import-resolve → typecheck → codegen → compile → link. Only artifacts reachable from the active command's root set materialize. The artifact store is hash-sharded under `.edda/cache/`; a per-machine `~/.edda/global-cache/` shares specializations across projects.
- **Compiler as a service.** A long-lived daemon owns the persistent structural index under `.edda/cache/index/`. `edda-lsp` and `edda-mcp` layer Language Server Protocol and Model Context Protocol surfaces over the same daemon; the locked CLI verbs map 1:1 to MCP operations.
- **Six v0.1 targets.** `x86_64` and `aarch64` for Linux (gnu / musl), macOS, and Windows (msvc); plus `wasm32-wasi`. Linking goes through `mold`, `ld.lld`, `ld64.lld`, `lld-link`, or `wasm-ld` depending on host/target pair.

A short illustrative excerpt:

```edda
public function greet(out: Stdout, name: String) -> () with {out} {
    let banner: String = f"Hello, {name}!"
    out.print_line(banner)
}
```

## Status

The pipeline is wired end-to-end: parse → import-resolve → typecheck → MIR lower → LLVM IR → object → link. Programs produce native binaries today; coverage of the language surface grows wave by wave at the MIR-lowering and runtime edges.

In-tree Edda code exercising the toolchain:

- [`prototypes/`](prototypes/) — example programs, from a console calculator up to [`prototypes/lox-vm/`](prototypes/lox-vm/) (~3,250 lines), the largest in-tree test.
- [`comparisons/job-queue/`](comparisons/job-queue/) — one job-queue implementation written three times (Edda, Rust, C++) for surface-to-surface comparison.
- [`security/`](security/) — models of real published CVEs, each rewritten in Edda to show which language rule rejects the vulnerable shape at compile time.

## Workspace layout

The workspace is 28 library crates plus `edda-cli` (the `edda` binary).

| Crate | Role |
|---|---|
| `edda-span` | source map, span, file id, parking_lot-backed concurrency |
| `edda-intern` | string interner, 32-bit `Symbol` handles, `Send + Sync` |
| `edda-target` | locked triple grammar, per-arch feature catalogue, `target_has` |
| `edda-diag` | locked diagnostic-class catalogue, `LintConfig` severity escalation |
| `edda-syntax` | lexer, parser, AST, round-trip pretty-printer |
| `edda-manifest` | `package.toml` schema + validation, lockfile with tamper trailer |
| `edda-resolve` | path resolution → source graph → top-level items → intra-function scopes |
| `edda-types` | bidirectional inference, mode lattice, effect-row checking, comptime-purity verification |
| `edda-comptime` | HIR-walking comptime evaluator + built-in catalogue |
| `edda-codegen` | spec instantiation: canonical hashing, AST substitution, cascade walker |
| `edda-refine` | Z3 backend, AST → predicate lifter, proof-certificate byte format |
| `edda-mir` | typed-HIR → MIR lowering, structural validator |
| `edda-compile` | MIR → LLVM IR via `inkwell`; control flow, ADTs, slices |
| `edda-cache` | content-addressed store, per-tier manifest, GC schedule, atomic stage-rename commit |
| `edda-link` | linker/archiver selection, link plan, process invoke pipeline |
| `edda-rt` | runtime ABI: allocator family, crypto externs, capability runtime |
| `edda-rt-alloc` | `EdBox` / `EdSlice` wire ABI and the type-erased alloc externs |
| `edda-rt-stats` | runtime allocation statistics |
| `edda-structmap` | the `index.toon` structure-index emitter (`edda build` output) |
| `edda-mimir-archive` | reproducible `.xlib` tar.zst pack/unpack |
| `edda-mimir-canonical` | deterministic canonical encoder feeding every package hash |
| `edda-mimir-crypto` | ed25519 signing + BLAKE3 fingerprints for publisher identity |
| `edda-mimir-hash` | `xlib_hash` / `surface_hash` / `effect_hash` computation |
| `edda-mimir-registry` | registry sources + NDJSON sparse index |
| `edda-driver` | manifest → source-graph → resolution → typecheck → codegen → compile → link orchestration |
| `edda-cli` | binary: parses every locked verb, dispatches through `DriverDispatcher` |
| `edda-daemon` | persistent index + file-watcher + transport |
| `edda-lsp` | Language Server Protocol shim over the daemon |
| `edda-mcp` | Model Context Protocol shim over the daemon |

## Installing a prebuilt release

Once a release is published on this repo's [Releases page](https://github.com/edda-lang/edda-bootstrap/releases), installing needs no Rust toolchain:

```sh
curl -fsSL https://raw.githubusercontent.com/edda-lang/edda-bootstrap/main/install.sh | bash   # Linux, macOS
```
```powershell
irm https://raw.githubusercontent.com/edda-lang/edda-bootstrap/main/install.ps1 | iex           # Windows
```

Either script downloads the release archive for your platform, unpacks it to `~/.edda-bootstrap`, and adds `~/.edda-bootstrap/bin` to `PATH`. The archive bundles the `edda` binary next to its vendored `std/` and `runes/` trees, so `edda check`/`build`/`run` resolve the standard library out of the box — no `EDDA_STDLIB_ROOT` to set. A system linker is the one remaining runtime dependency (`lld`/`mold` on Linux, MSVC's `link.exe`/`lld-link` on Windows, `ld64` via Xcode Command Line Tools on macOS); Z3 ships statically linked. See the archive's bundled `README.md` for the exact `path+` line to reference a vendored rune from your own `package.toml`.

Windows (`x86-64-windows-msvc`) is the first-class, verified platform; Linux (`x86-64-linux-gnu`, `aarch64-linux-gnu`) ships labeled experimental — please help verify it. macOS (`aarch64-macos-darwin`) ships when a builder is available.

## Building from source

```sh
git clone <repo>
cd edda-bootstrap
cargo build --workspace
```

Z3 builds from vendored source via CMake. The build requires CMake (the workspace pins `CMAKE_POLICY_VERSION_MINIMUM=3.5` for CMake 4 compatibility), Python, and a C/C++ toolchain. On Windows, invoke from a Developer Command Prompt or source `vcvars64.bat` first so `INCLUDE` is set for bindgen.

The LLVM backend is feature-gated: `--features llvm` requires an LLVM 18 install and `LLVM_SYS_180_PREFIX` pointing at its root (the directory must contain `bin/llvm-config` and the `LLVM-C` library).

Compiling Edda programs that import `std.*` requires a stdlib checkout; point the compiler at one with `EDDA_STDLIB_ROOT=<path/to/stdlib>`.

## What runs today

The `edda` driver and the `xtask` runner together expose the working surface:

```sh
cargo xtask build                              # release-build the whole workspace (edda + runtime staticlib)
cargo xtask package                            # stage + archive a plug-and-play install for this platform
cargo xtask parse <path.ea>                    # lex + parse, render diagnostics
cargo xtask parse-roundtrip <path.ea>          # parse → print → parse fixed-point check
edda build                                     # in a package dir: parse → typecheck → MIR → LLVM → object → link
```

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for the pull-request workflow, the build/test steps, and the licensing and commit-authorship terms.

## License

Edda is licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option (`SPDX-License-Identifier: MIT OR Apache-2.0`). Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work shall be dual-licensed under the same terms, with no additional terms or conditions.
