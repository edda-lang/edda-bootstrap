# Contributing to the Edda bootstrap compiler

Thanks for your interest in improving Edda's bootstrap compiler. Contributions
of every kind are welcome — bug reports, fixes, and improvements to the
compiler, runtime, tooling, and packaging.

## Licensing of contributions

This project is dual-licensed under [MIT](LICENSE-MIT) and
[Apache-2.0](LICENSE-APACHE), at the user's option
(`SPDX-License-Identifier: MIT OR Apache-2.0`). Contributions are accepted
under the same dual grant:

> Unless you explicitly state otherwise, any contribution intentionally
> submitted for inclusion in the work by you, as defined in the Apache-2.0
> license, shall be dual licensed as above, without any additional terms or
> conditions.

There is **no Contributor License Agreement (CLA)** and **no Developer
Certificate of Origin (DCO) sign-off** to complete. By opening a pull request
you license your contribution under the dual MIT OR Apache-2.0 grant; inbound
contributions are licensed identically to outbound distribution
(inbound = outbound), and you retain copyright in your work.

If a contribution incorporates or is derived from externally licensed code,
say so in the pull request so its provenance can be recorded.

## Commit authorship

Author your commits under your own identity — a real name or handle plus an
email that links to your account. A GitHub `…@users.noreply.github.com` address
works well and keeps your personal email private. Your commits are how you are
credited, so use an identity you want associated with the work; contributions
are not folded under a shared placeholder author.

Credit the account, not the model. Across the Edda projects, AI assistance is
assumed rather than disclosed — contributions are understood to be produced with
AI tooling under human direction, so which model was used carries no meaningful
information and is not recorded. Do not add `Co-Authored-By:` trailers for AI
tools or models; the responsible account is the author. The copyright
holder-of-record line in the LICENSE files ("The Edda Authors") is a collective
legal umbrella only — it is not a commit author, and individual authors retain
copyright in their own contributions.

## Submitting changes

1. Open an issue describing the bug or proposal before large changes, so the
   approach can be discussed first.
2. Keep each pull request focused on a single, self-contained change.
3. Build and test the workspace before you submit:

   ```sh
   cargo build --workspace
   cargo test --workspace
   ```

   Building the compiler requires an LLVM 18 toolchain and a C/C++ build
   environment — the refinement solver builds a vendored SMT backend from
   source. See the README for platform setup.

By submitting a pull request, you agree to license your contribution under the
terms above.
