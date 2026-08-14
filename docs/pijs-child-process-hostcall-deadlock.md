# PiJS `child_process` hostcall deadlock

## Reported behavior

While using the PTC `node` tool to inspect and commit repository changes, the
first implementation used Node's synchronous child-process API:

```js
import { execFileSync } from "node:child_process";

execFileSync("git", ["status", "--short", "--branch"], {
  cwd: "C:\\project\\e-agent",
  encoding: "utf8",
});
```

The tool rejected the call with:

```text
Execution denied by policy (sync child_process APIs are disabled by default)
```

The code was then changed to the asynchronous API:

```js
import { execFile } from "node:child_process";
import { promisify } from "node:util";

const exec = promisify(execFile);
const result = {};
result.status = await exec("git", ["status", "--short", "--branch"], {
  cwd: "C:\\project\\e-agent",
  encoding: "utf8",
});
result.diffStat = await exec("git", ["diff", "--stat"], {
  cwd: "C:\\project\\e-agent",
  encoding: "utf8",
});
console.log(JSON.stringify(result, null, 2));
```

This time no policy error was returned, but the entire tool call remained
pending indefinitely and produced no output. The apparent hang occurs on the
first awaited Git command, not on the final command in the program.

## Root cause

This is a hostcall dispatch deadlock, not a hung Git process.

1. `execFile()` calls the PiJS `spawn()` shim.
2. `spawn()` calls `globalThis.pi.exec(...)` and receives a Promise. This only
   enqueues an exec hostcall; it does not execute the process itself.
3. The program's top-level `await` waits for that Promise.
4. `PiJsRuntime::execute_program()` waits for the program module's `import()`
   Promise to finish.
5. The PTC executor directly awaits `execute_program()` and does not
   concurrently drain `drain_hostcall_requests()`, execute the queued command,
   and return its result through `complete_hostcall()`.

The resulting dependency cycle is:

```text
module import waits for git Promise
git Promise waits for exec hostcall completion
exec hostcall waits for the host to drain and execute it
host waits for module import to complete
```

The synchronous error is separate and intentional. `allow_unsafe_sync_exec`
defaults to `false`, preventing synchronous subprocess execution from
bypassing capability and risk mediation.

`promisify(execFile)` is also not the source of the hang. Its callback cannot
resolve until the underlying exec hostcall completes.

## Affected path

- `e-agent-node-runtime/src/runtime.rs`: the `node:child_process` `spawn()` shim
  creates the `pi.exec` hostcall.
- `e-agent-node-runtime/src/runtime.rs`: `PiJsRuntime::execute_program()` awaits
  the imported module to completion.
- `e-agent-core/src/tool/ptc/mod.rs`: `ProgrammaticToolExecutor::execute()`
  directly awaits `execute_program()` without driving exec hostcalls.

Native extension calls such as the `todo` module do not reproduce the issue
because they use the separately installed native-call bridge and are awaited
directly by Rust.

## Resolution direction

The short-term workaround is to run Git and other subprocess commands through
the host's `bash` tool instead of `node:child_process` inside PTC programs.

Full `child_process` support requires the PTC execution path to drive the
hostcall queue while the module Promise is pending: drain queued requests,
execute permitted subprocesses, stream output when requested, call
`complete_hostcall()`, and tick the PiJS scheduler until both the program and
all relevant hostcalls settle. If PTC is not intended to support exec
hostcalls, `node:child_process` should instead fail immediately with a clear
unsupported-operation error rather than leaving an unresolved Promise.
