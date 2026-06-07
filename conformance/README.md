# Conformance corpora

Golden test cases that pin muse's externally observable behavior. They are split
into two harnesses:

- **`emulator/`** — VT/ANSI emulator correctness. Each case feeds raw bytes into
  the emulator and asserts the resulting screen state (cursor, line text, style
  runs). No process is spawned.
- **`protocol/`** — engine/protocol integration. Each case spawns a program and
  drives it through the same step vocabulary as a normal test spec.

## Running

```sh
# Run both corpora through the CLI:
muse conformance conformance

# What CI runs (must stay green):
cargo test -p muse-cli shipped_corpus_is_green
```

## Case format

### Emulator (`emulator/*.yaml`)

```yaml
name: sgr_basic
profile: xterm                   # which built-in profile to drive
feed: "\e[1;31mHI\e[0m"          # raw bytes fed to the emulator
expect:
  cursor: {row: 0, col: 2}
  lines:
    - text: "HI"
      styles:
        - {start: 0, len: 2, fg: {indexed: 1}, attrs: [BOLD]}
```

Existing cases: `sgr_basic`, `truecolor`, `wide_glyph`, `erase_and_move`,
`dumb_drops_color`.

### Protocol (`protocol/*.yaml`)

```yaml
name: spawn_echo
spawn: ["echo", "hi"]
steps:
  - expect_visible: {text: "hi"}
  - expect_text: {line: 0, equals: "hi"}
```

Existing cases: `spawn_echo`, `cat_roundtrip`.

## Adding a case

1. Drop a new `*.yaml` into `emulator/` or `protocol/`.
2. For emulator cases, keep `feed` minimal and assert the smallest screen state
   that captures the behavior.
3. Run `cargo test -p muse-cli shipped_corpus_is_green` to confirm it's picked up
   and passes.

The conformance harness itself lives in `crates/muse-runner/src/conformance.rs`.
