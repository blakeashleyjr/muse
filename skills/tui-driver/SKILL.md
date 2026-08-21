---
name: tui-driver
description: Drive a terminal program (TUI or CLI) interactively with muse to check your own work — open it in a PTY, send keys, take text or PNG screenshots, wait for text to appear, read its raw output, and turn the session into a regression spec. Use when asked to run, verify, screenshot, or write tests for a terminal app.
---

# Driving a TUI with muse (`/tui-driver`)

`muse` is Playwright for terminals. A session keeps a program alive in a
PTY across separate commands, so you can look, act, and look again.
Everything below works from Bash; the same verbs exist as MCP tools if
`muse mcp` is registered (`claude mcp add muse -- muse mcp`).

## The loop

```bash
muse session open --name app --size 120x40 -- <program> [args…]   # prints an id; --name is an alias
muse session wait app --visible "Ready"            # retries until seen (exit 1 if not — read the FAIL line)
muse session snap app                              # plain-text screen, settled
muse session send app --key ctrl+p --text "query" --key enter
muse session wait app --regex "results?: [0-9]+" --timeout-ms 8000
muse session snap app --kind pixel --out shot.png  # PNG screenshot (view it)
muse session logs app                              # everything the program wrote (raw)
muse session close app
```

- `wait` is the assertion primitive; prefer it over sleeping. Conditions:
  `--visible T`, `--regex R`, `--not-visible T`, `--line N --contains T`,
  `--line N --equals T`, `--count-min N`, `--exit`. Exit codes: 0 held,
  1 did not hold, 2 muse/daemon error.
- `send` takes any of `--text`, `--key` (repeatable chords: `ctrl+c`,
  `alt+enter`, `shift+f5`, `escape`, `x`), `--paste`, `--bytes '\e[A'`,
  `--mouse '@row,col'` / `release:left@r,c` / `wheel_down@r,c`.
- `snap --kind styled` shows attributes/colors; `screen` dumps cursor,
  title, and modes as JSON (`--json` on any verb for machine output).
- `resize app 80x24` to test breakpoints. `list` shows sessions.
- Pass environment with `--env KEY=VALUE`, a working dir with `--cwd`.
- The daemon starts itself on the first `open` and exits when idle;
  `muse serve --stop` ends it early. Isolate with `MUSE_SOCKET=<path>`.

## When something looks wrong

1. `muse session snap app` — what is actually on screen.
2. `muse session logs app` — raw bytes the program emitted (panics,
   stderr interleaved, escape sequences).
3. `muse session screen app` — is it on the alt screen? Where is the
   cursor? Which modes (mouse, bracketed paste, sync output) are on?
4. `muse session trace app --out ./trace` — asciinema casts + every
   stable frame as JSON for offline inspection.

## Turning a check into a test

```bash
muse session export-spec app --out test/muse/specs/NN-feature.yaml
muse run test/muse/specs/NN-feature.yaml
```

The exported spec replays your inputs and asserts every `wait` that held.
Edit the generated `expect_*` steps to tighten them; add `snapshot:` steps
for visual regression. `muse run` keeps failure artifacts under
`test-results/<case>/` (`final.txt`, `final.png`, diffs, `trace/`).

## Rules

- Always `close` sessions you opened (or `close --all`), and keep
  sessions short-lived: a forgotten session holds a live process.
- Don't drive a program that expects the user's real terminal state
  (e.g. your own shell); open a fresh instance instead.
- If `open` fails with a daemon error, read `daemon.log` next to the
  socket.
