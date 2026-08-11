# Contributing

TorQ is a small, dense codebase. The bar is kernel-style: simple, correct,
reviewable code — no ceremony, no speculative abstraction.

## Commits

- Conventional Commits: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`,
  `chore:` (+ optional scope).
- Subject ≤ 72 chars; body explains *why*, not what.
- One logical change per commit.

## Code

- Rust edition 2024, MSRV 1.85 (`rust-version` in the workspace manifest).
- `cargo fmt` and `cargo clippy --workspace --all-targets -- -D warnings`
  must pass before any merge.
- Prefer fewer, denser lines over ceremony: no filler doc comments, no
  needless generics, no wrappers that don't earn their lines.
- Reuse what exists: librqbit's `Api` facade, shared runners for
  declarative sources — do not hand-roll a second convention.
- No new dependencies without a reason; prefer audited crates over
  hand-rolled crypto/encoding (e.g. `base64` over a local encoder).

## Tests

- Every change ships tests. New behavior defends an observable contract and
  fails on a plausible bug; E2E exercises the real binary against the real
  API (see existing daemon E2E flows).
- No test may depend on the network; live-source checks are manual/spike
  only.

## Review

- No PR merges without a review. Reviewers check for: silent failure paths,
  lock discipline (never hold our locks across engine calls), error
  propagation (no swallowed `Result`), and memory/CPU behavior at scale.
