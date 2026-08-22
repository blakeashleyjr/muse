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
| `muse run <spec.yaml>…` | Run matrix-expanded test specs. `--profile`, `--size`, `--reporter pretty\|junit\|json`, `--retries`, `--shard i/n`, `--grep`, `--update-snapshots`, `--ci` (a missing baseline fails), `--artifacts dir`, `--trace on\|retain-on-failure\|off`, `--case-timeout-ms`. |
| `muse session …` | Drive a program interactively across commands: `open`, `send`, `resize`, `snap`, `screen`, `wait`, `logs`, `trace`, `list`, `close`, `export-spec`. See below. |
| `muse serve` | The session daemon (started on demand by `session open`; `--stop` ends it). |
| `muse mcp` | The session verbs as MCP tools on stdio, for agent hosts (`claude mcp add muse -- muse mcp`). |
| `muse exec -- <argv>` | Spawn a program, settle, dump a snapshot (`--kind text\|styled\|pixel`, `--out file.png`). |
| `muse trace <dir>` | Inspect a recorded trace; `--frame N` renders one frame. |
| `muse doctor` | Diagnostics + self-test (font fingerprint, shells, profiles, spawn-and-assert). |
| `muse profiles` | List built-in emulation profiles and capabilities. |
| `muse conformance <dir>` | Run emulator + protocol conformance corpora (recursively). |
| `muse codegen` | How the language SDK bases would be generated (no SDKs ship yet). |

`--config file` / `$MUSE_CONFIG` / `./muse.toml` supply defaults (see
`muse.toml`); `MUSE_*` env overrides the file; flags override both.
`MUSE_LOG=debug` turns on tracing to stderr.

## Interactive sessions (the agent loop)

A session keeps a program alive in a PTY between commands — look, act,
look again — which is how an agent checks its own work on a TUI:

```sh
muse session open --name app --size 120x40 -- ./my-tui     # prints an id
muse session wait app --visible "Ready"                     # retries until seen; exit 1 if not
muse session send app --key ctrl+p --text "query" --key enter
muse session snap app                                       # settled screen as text
muse session snap app --kind pixel --out shot.png           # or a PNG
muse session logs app                                       # everything the program wrote
muse session export-spec app --out test/app.yaml            # inputs + waits that held → a spec
muse session close app
```

`wait` conditions: `--visible`, `--regex`, `--not-visible`, `--line N
--contains/--equals`, `--count-min`, `--exit`. `send` takes `--text`,
repeatable `--key` chords (`ctrl+c`, `alt+enter`, `f5`), `--paste`,
`--bytes '\e[A'`, `--mouse '@row,col'`. Every verb accepts `--json`.
Exit codes: 0 held, 1 did not hold, 2 muse/daemon error. The daemon
exits when idle; `MUSE_SOCKET` isolates one.

When a `muse run` case fails it keeps `test-results/<case>/` with
`final.txt`, `final.png`, `final.json` (cursor/modes), per-snapshot
`*.actual`/`*.diff`/`*.baseline` files, `result.json`, and a `trace/`
directory (asciinema casts, every stable frame, steps with assertions).

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

Steps: `write`, `write_line`, `paste`, `key {key, mods}`,
`mouse {row, col, button, action, mods}`, `resize "WxH"`, `sleep_ms`,
`begin_step "name"`, `expect_visible`, `expect_not_visible`,
`expect_text {…, equals}`, `expect_contains {…, contains}`,
`expect_count {…, eq|min|max}`, `expect_style {…, bold|fg|bg|…}`,
`expect_exit {code, timeout_ms}`, `snapshot {name, kind, masks, normalize, scale}`,
`check_file {path, reject_re}`, `watch_log {path, reject_re}`.

Spec-level keys: `matrix {profiles, sizes}`, `env`, `case_tmp_env` (a fresh
per-case directory exported as that variable and available as `{case_tmp}`),
`snapshot_defaults`, `sync {quiet_window_ms, max_settle_ms}`.

Locators (used by `expect_*` and snapshots): `text`, `regex`, `line`, `cell`,
`region`, `cursor`, with `ignore_case` / `whole_line` / `multiline` flags and a
per-step `timeout_ms`.

Snapshots are stored at
`snapshots/{spec}__{name}/{profile}__{cols}x{rows}__{os}.{txt|png}`; a missing
baseline is created and passes on first run (fails under `--ci`);
`--update-snapshots` overwrites.

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

Also built: the session daemon + `muse session` CLI + `muse mcp` server
(NDJSON over a unix socket; the `proto/` gRPC definition is not used by it).

Documented but not built in this distribution (require external toolchains or
are P2 in the spec): the gRPC/Connect server and generated multi-language
SDKs (`proto/` + `buf.gen.yaml` are provided), the C-ABI FFI for non-Rust
embedding, remote/SSH PTY transport, the differential mode against real
`xterm`/`kitty` under Xvfb, and DCS passthrough (sixel / kitty graphics).

## License

Dual-licensed under MIT or Apache-2.0. The embedded font derives from GNU
Unifont (SIL OFL 1.1).
