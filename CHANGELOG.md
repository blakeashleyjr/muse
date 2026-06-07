# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
