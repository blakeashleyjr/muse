# Contributing to muse

Thanks for your interest in muse! This document covers how to set up a dev
environment, the checks your change must pass, and how we review contributions.

## Development setup

muse is a Rust workspace. The toolchain is pinned in
[`rust-toolchain.toml`](rust-toolchain.toml) (Rust 1.90 + `rustfmt`, `clippy`,
`llvm-tools-preview`), so `rustup` selects the right version automatically.

```sh
git clone https://github.com/blakeashleyjr/muse
cd muse
cargo build --workspace
```

Unix PTYs are first-class; Windows ConPTY is a P2 target.

### Git hooks (lefthook)

We use [lefthook](https://github.com/evilmartians/lefthook) to run the same
checks locally that CI enforces. Install it once per clone:

```sh
# pick one to get the binary:
nix-shell -p lefthook        # NixOS / nix
cargo install lefthook       # any platform with cargo
# then wire up the hooks:
lefthook install
```

This installs:

- **pre-commit** — `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-targets -- -D warnings` on staged Rust, plus
  `buf lint` on staged `proto/**` (skipped if `buf` isn't installed).
- **pre-push** — `cargo test --workspace`.

Bypass in a pinch with `git commit --no-verify` (or `LEFTHOOK=0 git commit …`),
but CI will still enforce everything.

## Checks your change must pass

These mirror [`.github/workflows/ci.yml`](.github/workflows/ci.yml):

```sh
cargo fmt --all --check                                   # formatting
cargo clippy --workspace --all-targets -- -D warnings     # lints (warnings = errors)
cargo test --workspace                                    # unit + integration tests
cargo llvm-cov --workspace --summary-only                 # coverage — CI gate is >= 95%
cargo deny check                                          # advisories + licenses + bans
```

Additional CI gates:

- **Cross-OS pixel determinism** — pixel snapshots must be byte-identical on
  Linux and macOS (the embedded bitmap font is static; a fingerprint test pins
  it). Don't change the font without regenerating it via `tools/gen_font.py` and
  updating the fingerprint.
- **Conformance** — the shipped corpora in [`conformance/`](conformance/) must
  stay green (`cargo test -p muse-cli shipped_corpus_is_green`). See
  [`conformance/README.md`](conformance/README.md) to add cases.
- **Proto** — `buf lint` and breaking-change detection on
  [`proto/`](proto/README.md).

## Coding conventions

- Respect the crate DAG: a crate may only depend on crates *above* it in the
  layering documented in the [README](README.md#architecture-crate-dag).
  `muse-core` is pure (no I/O, no async).
- Keep snapshots/rendering deterministic — no system fonts, no floating point in
  the rasterizer, fixed palette. Determinism is a hard requirement, not a
  nice-to-have.
- Add tests with your change; coverage must stay above the 95% gate.
- Public items don't yet require doc comments, but new public APIs should carry
  a `///` describing intent.

## Submitting changes

1. Branch off `main`.
2. Make your change with tests; run the full check list above.
3. Open a pull request describing **what** changed and **why**. Link any related
   issue.
4. Note user-facing changes in [`CHANGELOG.md`](CHANGELOG.md) under
   `Unreleased`.
5. A maintainer will review; CI must be green before merge.

## Reporting bugs & proposing features

Open an issue with enough to reproduce: the spec or command, the profile/size,
and observed vs expected output. For visual-regression bugs, attach the diffing
snapshots if you can.

## License

By contributing, you agree that your contributions are dual-licensed under
[MIT](LICENSE-MIT) and [Apache-2.0](LICENSE-APACHE), matching the project.
