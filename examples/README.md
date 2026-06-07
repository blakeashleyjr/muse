# Examples

Runnable muse test specs that demonstrate the spec format end-to-end.

## Running

```sh
muse run examples/login_flow.yaml
# or constrain the matrix:
muse run examples/login_flow.yaml --profile xterm --size 80x24
```

Each spec is expanded across its `matrix` (profile × size); every case runs in a
fresh, isolated context with web-first retrying assertions.

## What's here

| File | Demonstrates |
|---|---|
| `login_flow.yaml` | Spawning a shell, waiting for a prompt with `expect_visible`, sending input with `write`, and capturing both `text` and `styled` snapshots. |

## Spec format (quick reference)

```yaml
name: login_flow                 # spec name (used in snapshot paths)
matrix:
  profiles: [xterm, vt220]       # emulation profiles to run against
  sizes: ["80x24"]               # terminal sizes, "COLSxROWS"
spawn: ["sh", "-c", "…"]         # argv of the program under test
steps:
  - expect_visible: {text: "username:"}
  - write: "ada\n"
  - expect_visible: {text: "welcome, ada"}
  - snapshot: {name: after_login, kind: text}
```

Steps: `write`, `paste`, `key {key, mods}`, `resize "WxH"`, `sleep_ms`,
`expect_visible`, `expect_text {…, equals}`, `expect_contains {…, contains}`,
`snapshot {name, kind, masks, normalize, scale}`. See the
[top-level README](../README.md#test-spec-format) for the full locator and step
catalog.

## Adding an example

1. Add a `*.yaml` spec here that shows off a distinct capability.
2. Keep it self-contained — prefer `sh -c` snippets over external programs so it
   runs anywhere.
3. Verify it passes: `muse run examples/your_spec.yaml`.
4. Add a row to the table above.
