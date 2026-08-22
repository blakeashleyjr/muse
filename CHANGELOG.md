# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Interactive sessions** — `muse serve` daemon + `muse session
  open|send|resize|snap|screen|wait|logs|trace|list|close|export-spec`,
  and `muse mcp` (the same verbs as MCP tools on stdio). A program stays
  alive between commands; `export-spec` turns a driven session into a
  runnable spec. `skills/tui-driver/SKILL.md` documents the loop.
- **Failure artifacts** — `muse run` keeps `test-results/<case>/` (final
  screen as text/PNG/JSON, per-snapshot actual/diff/baseline, a trace
  directory, `result.json`) for failing cases; `--artifacts`, `--trace`.
  Assertions are recorded into the trace.
- `--ci` (missing baseline = failure), `--allow-empty`, `--case-timeout-ms`;
  `muse.toml` / `MUSE_*` / `--config` are honoured; `MUSE_LOG`.
- Spec steps `write_line`, `mouse`, `begin_step`, `expect_not_visible`,
  `expect_count`, `expect_style`, `expect_exit`, `check_file`, `watch_log`;
  spec-level `env`, `case_tmp_env`, `sync`, `snapshot_defaults`.
- Emulator: kitty keyboard protocol negotiation (+ CSI-u encoding), xterm
  modifyOtherKeys, truthful DECRQM, DEC Special Graphics charset,
  XTVERSION; profiles gate mode negotiation.

### Fixed

- Output the program writes with no input in between now produces a
  stable frame, so retrying assertions see it (previously they resolved
  against the last post-input frame until the deadline).
- A slow-starting program is no longer snapshotted as an empty screen.
- Shutdown signals the program's whole process group and reaps it.
- A run that selects zero cases, an out-of-range `--shard`, a panicking
  case, and an empty conformance corpus are failures, not silent passes.
- `watch_log` no longer swallows non-UTF-8 or splits lines across steps;
  `mouse` rejects unknown buttons/actions; JUnit output strips control
  characters and carries `time=`.

## [0.1.0] - 2026-06-06

Initial release.

### Added

- **Domain model** (`muse-core`) — grid, cell, style, color, screen, modes,
  cursor, locators, input encoding, snapshots, config, and errors. Pure: no I/O,
  no async.
- **VT emulator** (`muse-emulator`) — a `vte`-based backend with five built-in
  profiles (`xterm`, `vt220`, `kitty`, `screen`, `dumb`), per-profile
  capabilities, query responses, and color reduction.
- **Renderers** (`muse-render`) — text, styled, and deterministic pixel (PNG)
  renderers backed by a baked subset of GNU Unifont, plus SVG export. Identical
  `Screen` ⇒ byte-identical PNG across Linux and macOS.
- **Diffing** (`muse-diff`) — masking, normalization, perceptual pixel diff,
  stabilization, and baseline management.
- **Tracing** (`muse-trace`) — native trace format, asciinema v2 cast export,
  recorder/reader.
- **PTY layer** (`muse-pty`) — async wrapper over `portable-pty`.
- **Engine** (`muse-engine`) — Terminal actor, session/context management,
  synchronizer (quiet-window + DEC-2026 synchronized output + `OSC 5379 muse:ready`),
  and web-first retrying assertions.
- **Runner** (`muse-runner`) — matrix expansion, scheduling, retries, reporters
  (pretty / JUnit / JSON), and the conformance harness.
- **CLI** (`muse-cli`) — `run`, `exec`, `trace`, `doctor`, `profiles`,
  `conformance`, `codegen`.
- Wire protocol definition (`proto/muse/v1/muse.proto`) and `buf` tooling for
  generating multi-language SDK bases.

[Unreleased]: https://github.com/blakeashleyjr/muse/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/blakeashleyjr/muse/releases/tag/v0.1.0
