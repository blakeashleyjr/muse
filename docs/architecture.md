# Architecture Overview

`muse` is designed with a strict bottom-up layering strategy, ensuring that each component is isolated and depends only on the crates below it. This architecture separates pure domain logic from side-effects (like I/O and async runtime), providing deterministic behavior for terminal application testing.

## Subsystems

The workspace is divided into several focused crates, forming a clear Directed Acyclic Graph (DAG):

- **`muse-core`**: The pure domain model. Contains the implementation of the grid, cells, styles, colors, screens, modes, cursor, locators, input encoding, snapshot structures, errors, and configuration. It is entirely free of I/O and async code.
- **`muse-emulator`**: The terminal emulation layer. Defines the `Emulator` trait, integrates the `vte` backend, manages emulation profiles (e.g., `xterm`, `vt220`, `kitty`), and handles capability mapping and color reduction.
- **`muse-render`**: The rendering subsystem. Responsible for taking the screen state and producing output in various formats. Implements text, styled, and pixel renderers, and contains the baked static bitmap font and SVG generation.
- **`muse-diff`**: The snapshot diffing engine. Handles masking, normalization, perceptual pixel diffing, snapshot stabilization, and baseline management.
- **`muse-trace`**: The recording layer. Defines the trace directory format, supports `asciinema` v2 casts, and implements the recorder and reader for test execution traces.
- **`muse-pty`**: The process execution layer. A lightweight, async-aware wrapper around `portable-pty` for spawning and communicating with terminal processes.
- **`muse-engine`**: The core runtime. Implements the `Terminal` actor, Session/Context management, frame synchronization (quiescence detection and DEC-2026 synced output), and web-first retrying assertions.
- **`muse-runner`**: The test orchestration layer. Handles test spec matrix expansion, scheduling, retries, flakiness management, reporter formatting, and conformance corpus execution.
- **`muse-cli`**: The front-end command-line interface, providing the `muse` binary and exposing commands like `run`, `exec`, `trace`, and `doctor`.

## Core Runtime & Data Flow

When executing a test spec or driving a program, data flows through the system in the following way:

1. **Process Spawning**: The `muse-runner` reads a test spec and expands the test matrix. It uses `muse-pty` to spawn the target program inside a pseudo-terminal.
2. **Terminal Actuation**: The `muse-engine` drives the PTY by sending input (keys, mouse events, paste data, resizes) to the program's stdin.
3. **State Management**: As the program outputs VT/ANSI sequences, `muse-emulator` parses them via its `vte` backend and applies the mutations to the pure screen model living in `muse-core`.
4. **Synchronization**: Instead of relying on flaky sleeps, `muse-engine` monitors the output for quiescence. It declares a frame stable when the output is quiet or when specific synchronization markers (like DEC-2026 or `muse:ready`) are received.
5. **Assertions**: Once a frame is stable, `muse-engine` runs web-first assertions (e.g., `expect_visible`) against the screen DOM. If an assertion fails, it retries against fresh frames until a deadline is met.
6. **Snapshotting**: When a snapshot is requested, `muse-render` captures the current frame at the specified fidelity (text, styled, or pixel). 
7. **Verification**: `muse-diff` compares the captured snapshot against the baseline on disk. Pixel snapshots are deterministic across platforms due to fixed cell metrics, a static bitmap font, and integer rasterization.

## Verification Strategy

Quality and determinism are enforced at multiple levels:

- **Determinism Guardrails**: Pixel rendering relies exclusively on a statically baked subset of GNU Unifont. System fonts, floating-point calculations, and platform-specific text shaping are completely avoided to ensure byte-identical PNGs across Linux and macOS. A fingerprint test pins the font so it cannot silently drift.
- **Testing**: The project maintains rigorous testing with a strict requirement of >95% code coverage (`cargo llvm-cov`). This includes unit tests for domain logic and integration tests for the full end-to-end pipeline.
- **Conformance**: The `muse conformance` suite runs established emulator and protocol conformance corpora against `muse-emulator` to verify accurate VT/ANSI sequence parsing and state manipulation.
- **Static Analysis**: `cargo clippy` and `cargo fmt` are enforced across all targets, failing on warnings.
