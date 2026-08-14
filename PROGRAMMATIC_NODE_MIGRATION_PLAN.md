# Programmatic Node Migration Plan

## Goal

Replace the Python-backed programmatic tool call path with the existing
`e-agent-node-runtime` QuickJS runtime. Rename `e-agent-tool` to
`e-agent-extension`, retain dynamic extension loading through
`E_AGENT_TOOL_PATHS`, and preserve the spelling and behavior of
`#[extension]`, `#[tool]`, and `#[state]`.

The model-facing custom tool is named `node`. It runs JavaScript/TypeScript in
QuickJS with the Node-compatible modules supplied by `e-agent-node-runtime`.
It is not advertised as a complete Node.js or npm runtime.

## Non-Goals

- Retaining Python scripts, PyO3 extension loading, `.pyd` staging, or Python
  fixtures.
- Keeping the Rust package/import name `e-agent-tool` as a compatibility shim.
- Passing Rust trait objects, Tokio futures, or `rquickjs` values across a
  `cdylib` boundary.
- Broadening the Node compatibility layer while doing this migration.

## Target Contract

### Model-facing `node` tool

- Input is raw JavaScript/TypeScript source.
- Source runs as an ES module and supports top-level `await`.
- `console.log` becomes captured stdout and `console.error` becomes captured
  stderr; neither writes directly to the host terminal.
- A loaded native extension is importable by extension name:

  ```js
  import * as basic_tools from "basic_tools";
  const result = await basic_tools.bash({ command: "printf node" });
  console.log(result);
  ```

- Every generated native function accepts one JSON object matching its existing
  schema and returns a Promise. This retains schema validation and avoids a
  second JS signature system.
- Existing `node:*` modules and TypeScript support remain owned by
  `e-agent-node-runtime`.

### Dynamic extension ABI v1

The host and each `cdylib` communicate only through `#[repr(C)]` data,
pointer-plus-length UTF-8 buffers, integer session IDs, and C function
pointers. The host keeps each `libloading::Library` alive until its executor is
dropped.

`e_agent_extension_v1` returns an ABI-versioned descriptor with these actions:

1. `metadata`: returns serialized `ToolExtension` JSON.
2. `start_call`: receives session ID, tool name, and input JSON; completes once
   through a host callback with either result JSON or an error string.
3. `drop_session`: drops state only for the requested session.
4. `set_cancelled`: propagates the existing turn-wide cancellation signal.
5. `free_buffer`: releases buffers allocated by the extension.

The callback is asynchronous. It must be exactly-once, own no borrowed host
memory after `start_call` returns, and never unload the library while a call is
in flight.

## Work Plan

### 1. Establish the runtime adapter

Add a small public API to `e-agent-node-runtime` for program execution rather
than having `e-agent-core` manipulate `__e_agent_result` directly. The API
receives a native-module registry and returns `{ stdout, stderr }` or a
formatted QuickJS error with stack trace.

Keep the adapter limited to: create fresh realm, register `console`, install
loaded extension modules, evaluate one ES module, await pending work, and
dispose the realm. Do not move extension policy, generic Pi extension support,
or unrelated hostcall systems into `e-agent-core`.

### 2. Rename the extension crate

Rename directory, Cargo package, and Rust crate path from `e-agent-tool` /
`e_agent_tool` to `e-agent-extension` / `e_agent_extension`. Update every
workspace dependency and tool import. Keep the three attribute macro names and
their compile-fail diagnostics unchanged except for references to the new crate
name where necessary.

Remove `pyo3`, `pyo3-async-runtimes`, `input_from_python`, and `run` from the
renamed crate. Retain `Tool`, `ToolExtension`, `ToolFunction`, `SessionId`,
`SessionStates`, cancellation, progress, Serde, and Schemars exports.

### 3. Add the ABI and macro-generated dispatcher

Implement the v1 ABI in `e-agent-extension`. Change `#[extension]` so it:

- generates the metadata JSON descriptor;
- registers every `#[tool]` by its existing generated `Definition` type;
- validates input by deserializing the JSON object into that type;
- starts the existing async `Tool::call` and completes the C callback once;
- preserves state injection and `drop_session` behavior.

Delete all generated `#[pyfunction]`, `#[pymodule]`, Python signature, and
Python session wrapper code. Keep the Rust implementation function and JSON
schema generation unchanged.

### 4. Replace the PTC executor

Replace the implementation in `e-agent-core/src/tool/ptc/mod.rs` with a Node
executor that:

- loads `cdylib` extension descriptors with `libloading`;
- validates metadata (nonempty extension description, unique tool names, object
  input schema) without Python reflection;
- exposes one Promise-returning QuickJS native module per extension;
- binds session state before a tool call and unbinds it after completion or
  failure;
- serializes program executions until current extension state and cancellation
  ownership become per-call;
- returns captured stdout/stderr using the current `PTCOutput` shape;
- changes the tool definition and provider fixtures from `python` to `node`.

The old pure-Python fixture becomes a JavaScript/TypeScript fixture. Its test
must exercise module loading rather than preserving Python package semantics.

### 5. Migrate workspace extensions and remove Python

For every crate under `tools/`, replace the dependency/import name with
`e-agent-extension`, remove its PyO3 dependency, and continue building a
`cdylib`. Update build scripts to consume the native library directly; remove
the Windows `.dll` to `.pyd` copy.

Delete Python runner configuration and all Python-specific tests and docs.
Remove PyO3 dependencies from `e-agent-core`, the extension crate, all tool
crates, and `Cargo.lock` through Cargo resolution.

## Acceptance Criteria

### Build and dependency removal

1. `cargo fmt --check` succeeds.
2. `cargo test --workspace --all-targets` succeeds.
3. `cargo tree -i pyo3` and `cargo tree -i pyo3-async-runtimes` report no
   dependency in the workspace.
4. No workspace `Cargo.toml` declares `pyo3` or `pyo3-async-runtimes`.
5. `scripts/run-with-python.ps1` and `.pyd` staging are removed.

### Macro and extension compatibility

1. Existing `#[extension]`, `#[tool]`, and `#[state]` usage in every `tools/`
   crate compiles after only the crate/import rename.
2. Existing `trybuild` failures still reject missing descriptions, invalid
   state placement/type, duplicate state structs, and unknown input fields.
3. `ToolExtension` metadata has the same name, descriptions, system prompt,
   function order, input schema, and output schema as before.
4. Loading `basic_tools`, `my_ext`, `web_access`, `todo`, and `state_probe`
   from `E_AGENT_TOOL_PATHS` succeeds without Python installed.

### Runtime behavior

1. The executor publishes exactly one custom `node` tool with `ToolInput::Text`.
2. A TypeScript program can import `node:fs`, import `basic_tools`, use
   top-level `await`, and print a deterministic result.
3. `console.log("out")` is returned as stdout and `console.error("err")` as
   stderr; neither appears on process stdout/stderr during the test.
4. An extension call receives an object that matches its generated schema;
   missing, extra, or wrong-typed fields reject the JavaScript Promise with the
   extension and tool name in the error.
5. A thrown JavaScript error and a rejected extension Promise return a useful
   message plus QuickJS stack trace, and the next program still executes.
6. `state_probe.remember({ value: "a" })` and `recall({})` share state within
   one session, isolate two sessions, and return an empty value after
   `drop_session`.
7. Reloading an extension replaces its metadata instead of duplicating it, and
   system prompts remain in initial load order.
8. Cancellation rejects an in-flight Promise, terminates `basic_tools.bash`,
   and allows a subsequent program execution.

### Regression boundary

1. `basic_tools` retains its current file edit, image read, output truncation,
   UTF-8, process-tree timeout, and fuzzy-edit behavior through `node`.
2. Session prompt construction and non-programmatic provider message handling
   remain unchanged except that recorded custom-tool calls use `node`.
3. `e-agent-node-runtime` remains the sole owner of Node compatibility and
   QuickJS loader implementation; `e-agent-core` adds only the narrow adapter
   needed to expose native extensions.

## Delivery Sequence

Land each step with its focused tests: runtime adapter, extension ABI/macro
tests, executor integration tests, then dependency cleanup. Do not remove
PyO3 until the Node executor passes equivalent stateless, stateful, reload,
failure, and cancellation tests.
