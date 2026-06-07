# Wire protocol (`muse.v1`)

`muse/v1/muse.proto` is the **single source of truth** for muse's wire protocol —
the contract between a muse client and a muse engine, and the basis for the
generated language SDKs. The reference engine also ships an embedded
(in-process) implementation, which is what `muse run` / `muse exec` use; the
proto is what an out-of-process gRPC/Connect server and non-Rust SDKs are built
against.

## Layout

| File | Role |
|---|---|
| `muse/v1/muse.proto` | The `Muse` service and its messages (Handshake, NewContext, Spawn, Write, Key, Mouse, Resize, Paste, ResolveLocator, Snapshot, Assert, SetProfile, BeginStep/EndStep, StartTrace/ExportTrace, Subscribe). |
| `buf.yaml` | [buf](https://buf.build) module config — `STANDARD` lint rules and `FILE`-level breaking-change detection. |
| `buf.gen.yaml` | Code-generation plugins and output paths for each target SDK. |

## Linting & breaking-change checks

```sh
buf lint proto
buf breaking proto --against 'https://github.com/blakeashleyjr/muse.git#branch=main,subdir=proto'
```

CI enforces both in the `proto` job (`.github/workflows/ci.yml`). The lefthook
pre-commit hook also runs `buf lint` on staged `proto/**` when `buf` is
installed.

## Generating SDKs

```sh
buf generate proto
```

Per `buf.gen.yaml`, this writes generated code under `../sdks/<lang>/gen` for:

- **Go** — `protocolbuffers/go` + `connectrpc/go`
- **TypeScript** — `connectrpc/es`
- **Python** — `protocolbuffers/python` + `grpc/python`
- **C++** — `protoc` built-in `cpp`

The `sdks/` tree holds hand-written ergonomic wrappers layered on the generated
base; the generated `gen/` subdirectories are git-ignored (see
[`.gitignore`](../.gitignore)). The live server and SDKs are a P2 deliverable —
see the [Status section](../README.md#status) of the top-level README.
