# muse

**Black-box end-to-end + visual-regression testing for terminal programs.**

muse spawns a program under a PTY, drives it (keys / mouse / resize / paste),
queries a faithful screen model with web-first retrying assertions, and
snapshots it at three fidelity tiers (text / styled / pixel) across a matrix of
emulation profiles and terminal sizes.

```text
$ muse run examples/login_flow.yaml --profile xterm,vt220
[PASS] login_flow [xterm 80x24]
[PASS] login_flow [vt220 80x24]

2 passed, 0 failed, 2 total
```

## Why

Testing TUIs is usually ad-hoc: scrape stdout, sleep, hope. muse gives you a
queryable grid (the terminal "DOM"), deterministic "render finished" detection,
and byte-stable snapshots — the Playwright model, for terminals.

- **Faithful screen model.** A real VT/ANSI state machine (built on `vte`)
  maintains a `rows × cols` grid of styled cells, wide-glyph spacers, scrollback,
  alt-screen, cursor and negotiated modes.
- **Quiescence, not sleeps.** A synchronizer declares a frame *stable* on a quiet
  window, honours DEC-2026 synchronized output, and supports a cooperative
  `OSC 5379 ; muse:ready` marker for instant readiness.
- **Web-first assertions.** `expect(locator).to_be_visible()` retries against
  fresh frames until a deadline — no flaky sleeps.
- **Three snapshot tiers.** Trimmed golden text; a diff-friendly styled format;
  and deterministic pixel PNGs from a baked bitmap font (byte-identical across
  Linux & macOS — no system fonts, no floating point).
- **Multi-profile matrix.** `xterm`, `vt220`, `kitty`, `screen`, `dumb`, each
  with its own capabilities, query responses and colour reduction.

## Install

```sh
cargo build --release
# the binary is target/release/muse
```

Requires Rust 1.90+. Unix PTYs are first-class; Windows ConPTY is a P2 target.

## Commands

| command | description |
|---|---|
| `muse run <spec.yaml>…` | Run matrix-expanded test specs. `--profile`, `--size`, `--reporter pretty\|junit\|json`, `--retries`, `--shard i/n`, `--grep`, `--update-snapshots`. |
| `muse exec -- <argv>` | Spawn a program, settle, dump a snapshot (`--kind text\|styled\|pixel`, `--out file.png`). |
| `muse trace <dir>` | Inspect a recorded trace; `--frame N` renders one frame. |
| `muse doctor` | Diagnostics + self-test (font fingerprint, shells, profiles, spawn-and-assert). |
| `muse profiles` | List built-in emulation profiles and capabilities. |
| `muse conformance <dir>` | Run emulator + protocol conformance corpora. |
| `muse codegen` | How the language SDK bases are generated. |

## Test spec format

```yaml
name: login_flow
matrix:
  profiles: [xterm, vt220]
  sizes: ["80x24"]
spawn: ["sh", "-c", "printf 'username: '; read u; printf 'welcome, %s\\n' \"$u\""]
steps:
  - expect_visible: {text: "username:"}
  - write: "ada\n"
  - expect_visible: {text: "welcome, ada"}
  - snapshot: {name: after_login, kind: text}
```

Steps: `write`, `paste`, `key {key, mods}`, `resize "WxH"`, `sleep_ms`,
`expect_visible`, `expect_text {…, equals}`, `expect_contains {…, contains}`,
`snapshot {name, kind, masks, normalize, scale}`.

Locators (used by `expect_*` and snapshots): `text`, `regex`, `line`, `cell`,
`region`, `cursor`, with `ignore_case` / `whole_line` / `multiline` flags.

Snapshots are stored at
`snapshots/{spec}__{name}/{profile}__{cols}x{rows}__{os}.{txt|png}`; a missing
baseline is created and passes on first run; `--update-snapshots` overwrites.

## Architecture (crate DAG)

Strict bottom-up layering — a crate depends only on crates above it:

```text
muse-core       pure domain: grid, cell, style, color, screen, modes, cursor,
                locator, input encoding, snapshot, error, config (no I/O, no async)
muse-emulator   Emulator trait + vte backend + profiles + colour reduction
muse-render     text / styled / pixel renderers + baked bitmap font + SVG
muse-diff       mask / normalize / perceptual pixel diff / stabilize / baselines
muse-trace      trace dir format + asciinema v2 casts + recorder/reader
muse-pty        async portable-pty wrapper
muse-engine     Terminal actor, Session/Context manager, synchronizer, assertions
muse-runner     matrix expansion, scheduling, retries/flake, reporters, conformance
muse-cli        the `muse` binary
```

The wire protocol (`proto/muse/v1/muse.proto`) is the single source of truth for
the generated language SDKs; the reference engine also ships an embedded
(in-process) implementation, which is what `muse run`/`exec` use.

## Determinism

Pixel snapshots use a static bitmap font (a baked subset of GNU Unifont) and
fixed cell metrics, integer rasterisation, and a fixed palette — so identical
`Screen` ⇒ byte-identical PNG on every OS. The font is pinned by a fingerprint
test so it cannot silently drift.

## Development

```sh
cargo test --workspace                       # all unit + integration tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo llvm-cov --workspace --summary-only    # coverage (>95%)
```

The embedded font table is regenerated from a BDF font with
`tools/gen_font.py` (run once; the output is checked in).

## Status

Implemented and tested end-to-end: the full domain model, the VT emulator with
five profiles, all three render tiers, the diff/baseline subsystem, the trace
recorder, the async PTY layer, the Terminal actor + synchronizer + web-first
assertions, the matrix runner with reporters, the conformance harness, and the
`muse` CLI.

Documented but not built in this distribution (require external toolchains or
are P2 in the spec): the live gRPC/Connect server and generated multi-language
SDKs (`proto/` + `buf.gen.yaml` are provided), the C-ABI FFI for non-Rust
embedding, remote/SSH PTY transport, and the differential mode against real
`xterm`/`kitty` under Xvfb.

## License

Dual-licensed under MIT or Apache-2.0. The embedded font derives from GNU
Unifont (SIL OFL 1.1).
