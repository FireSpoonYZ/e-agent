# E-Agent Event-Driven Implementation Plan

Status: Complete

## Development Progress

Last updated: 2026-08-16

| Phase | Status | Current evidence / remaining work |
|---|---|---|
| 0. Baseline and fixtures | Complete | Workspace baseline and Provider failure fixtures pass. Full Pi modifying/notification fixtures remain. |
| 1. Native events and attachment | Implemented | `AgentEvent`, large Tokio broadcast `EventBus`, actor-backed `SessionHandle`, shared steer/follow-up queues, receiver-first `attach()`, `SessionStatus`, `SessionView`, wakeable abort, and restored-history/no-duplicate attachment coverage exist. |
| 2. Provider async stream | Complete | Provider stream contract, OpenAI text/reasoning/tool item deltas, core assembly, usage, done/error/aborted terminal forms, external cancellation, and partial-output Provider error persistence are covered. Broader adapter fixtures remain. |
| 3. Store-first orchestration | Implemented | Normal streaming, exact two-Turn tool-loop ordering, user/assistant/tool-result Store-fatal contract tests, post-fatal input rejection, store-first terminal events, and idempotent fatal shutdown pass. |
| 4. AgentHooks | Complete | Input, before-agent/context, tool-call, tool-result, and finalizing paths have success/error coverage; Pi modifying handlers chain in registration order, invalid tool mutations and hook errors fail closed, working-copy rollback is covered, and final role/Provider terminal invariants are enforced. Remaining notification/persistence integration cases remain. |
| 5. Extract `e-agent-pi-compat` | Complete | Standalone crate, CLI composition boundary, typed forwarding, and unmodified TypeScript hook/notification fixtures now cover registration-order chaining for before-agent/context/tool-call/tool-result/final-message paths. PiJS runtime internals still reside in `e-agent-tool-ptc` and the capability matrix remains incomplete. |
| 6. Diagnostic JSONL | Complete | CLI subscribes before Session start, writes native events, flushes records, and terminates with an `observer_lagged` record. Dedicated sanitization/lag/sink-independence tests remain. |
| 7. Ratatui TUI | Complete | CLI interactive mode uses a native `SessionAttachment`; reducer, streaming Markdown, multiline editor, scrolling, abort/fatal state, terminal guard, and narrow/normal/wide CJK tests exist. Full T-01 through T-10 and real-terminal smoke remain. |
| 8. Legacy removal and composition | Complete | `LifecycleEvent`, `LifecycleEffect`, `set_message_handler`, and `emit_message` production paths are removed. Pi ownership and TUI composition remain. |

Verified commands:

```text
cargo test --workspace
cargo fmt --all -- --check
git diff --check
```

These commands currently pass. Core contracts C-01 through C-16 are covered. The core migration gate is complete; the next slice is Pi P-01 notification detail and P-07 persisted-notification ordering, followed by P-08 through P-12 compatibility coverage. Final acceptance is recorded against the release build, workspace gates, deterministic lifecycle contracts, and real `.env` print/tool/restore scenarios.

Depends on [`e-agent-event-driven-architecture.md`](./e-agent-event-driven-architecture.md).
Validation details live in [`e-agent-event-driven-test-plan.md`](./e-agent-event-driven-test-plan.md).

## 1. Delivery Strategy

Use a native-first sequence without a legacy `LifecycleHook/on_message` bridge:

1. Replace core lifecycle, streaming, and persistence contracts.
2. Extract Pi compatibility into `e-agent-pi-compat`.
3. Build TUI and JSONL as native adapters.
4. Remove old CLI output and compatibility ownership from `e-agent-tool-ptc`.

Rust API compatibility is not a release constraint. Each phase must compile and pass its
own contract gate before the next phase starts.

## 2. Dependency Order

```text
Phase 0: lock current behavior and fixtures
   |
Phase 1: native event and attachment interfaces
   |
Phase 2: Provider streaming and message assembly
   |
Phase 3: store-first Session orchestration and fatal path
   |
Phase 4: strongly typed AgentHooks
   |
Phase 5: extract e-agent-pi-compat
   |
Phase 6: native JSONL adapter
   |
Phase 7: Ratatui TUI and Markdown
   |
Phase 8: remove legacy paths and run real acceptance
```

Phases 6 and 7 can proceed in parallel only after Phases 1-5 are stable.

## 3. Phase 0: Baseline and Fixtures

### Purpose

Create a small safety baseline before replacing the lifecycle API. Do not expand the old
design.

### Work

- Record current workspace test commands and known failures.
- Add or retain minimal fixtures for:
  - one complete assistant response
  - one tool call and tool result
  - Provider failure
  - Pi input transform
  - Pi tool block/result transform
  - Pi notification handler
- Identify a published/unmodified Pi extension used by the existing acceptance path.
- Preserve existing PTC regression fixtures from
  [`pi-extension-ptc-compatibility.md`](./pi-extension-ptc-compatibility.md).

### Files likely touched

- existing core/node-runtime tests
- a new test fixture directory under the future compat crate or `tests/fixtures/pi`

### Gate

- `cargo test --workspace` baseline is known.
- Fixtures load without reading or printing `.env`.
- No production behavior changes in this phase.

## 4. Phase 1: Native Events and Session Attachment

### Purpose

Establish the UI-independent output and command boundary before changing Provider output.

### Work

- Replace `lifecycle.rs` with native observation event definitions and EventBus ownership.
- Add a large fixed-capacity Tokio broadcast sender to Session/runtime ownership.
- Add `subscribe()` or attachment API returning `EventReceiver`.
- Add `SessionHandle` command messages for prompt, steer, follow-up, abort, and close.
- Add read-only `SessionView` for metadata, persisted messages, and status.
- Define `SessionStatus` including idle, running, fatal, and closed.
- Define session/agent/turn/message/tool event payload identities.
- Add the attach sequence: receiver first, then View, then reducer drain.
- Add JSON/Serde derives only where currently required by the diagnostic adapter; do not
  claim wire stability.

### Suggested module shape

```text
e-agent-core/src/
  event.rs
  hooks.rs                 # introduced in Phase 4; placeholder module not required now
  session/
    mod.rs
    handle.rs
    store.rs
    queue.rs
    view.rs
```

Do not create modules until they contain real behavior. Exact filenames may follow existing
code style.

### Gate

- Multiple receivers observe the same native event order.
- Dropping all receivers does not fail Session execution.
- SessionView reads an existing session without exposing mutable Store internals.
- `Lagged` is surfaced to the adapter; no resync implementation is added.

## 5. Phase 2: Provider Async Stream

### Purpose

Stop discarding OpenAI stream deltas and make streaming a Provider contract.

### Work

- Replace `Provider::send() -> AssistantMessage` with an async event stream contract.
- Define native Provider stream events for:
  - response start
  - text delta
  - thinking delta/summary when available
  - tool-call input delta/item completion
  - usage update
  - terminal done/error/aborted
- Require Provider request/runtime errors to terminate the stream with an assistant error
  message rather than escaping as an unstructured panic path.
- Adapt `e-agent-provider-openai` Responses stream events into native stream events.
- Build the cumulative assistant message in core, not in the TUI.
- Publish native `message_update` deltas while retaining the cumulative working message for
  finalization.
- Preserve cancellation from Session through Provider stream consumption.

### Files likely touched

- `e-agent-core/src/provider/mod.rs`
- `e-agent-core/src/message.rs`
- `e-agent-provider-openai/src/openai/mod.rs`
- provider examples/tests

### Gate

- A fake Provider can emit deterministic text/tool deltas.
- OpenAI adapter no longer waits for terminal output items before exposing text.
- Provider error and abort produce final assistant messages with retained partial content.
- No TUI code is involved in assembling authoritative assistant messages.

## 6. Phase 3: Store-First Session Orchestration

### Purpose

Make event ordering, message authority, and fatal Store behavior exact.

### Work

- Rebuild the Session loop around the approved native ordering.
- Apply the input Hook before `agent_start`.
- Emit user lifecycle inside the first Turn:

```text
message_start(user) -> store -> message_end(user)
```

- For assistant and tool-result messages:

```text
message_start/update* -> finalization -> store -> message_end
```

- Advance in-memory authoritative messages/leaf only after Store success.
- Add `persistence_error`, `session_fatal`, and non-persistent `session_shutdown` handling.
- After Store failure:
  - reject future prompt/steer/follow-up operations
  - abort current work
  - do not create a second assistant error that needs persistence
  - do not emit normal message/turn/agent end events
  - release extension/tool/session resources idempotently
- Keep Provider error/aborted on the normal persisted terminal path.
- Keep one Agent scope across all tool-loop Turns.

### Files likely touched

- `e-agent-core/src/session/mod.rs`
- `e-agent-core/src/session/store.rs`
- `e-agent-core/src/session/queue.rs`
- native event tests

### Gate

Exact event arrays pass for:

- accepted prompt without tools
- one and multiple tool Turns
- Provider error after partial text
- user abort after partial text
- Store failure on user message
- Store failure on assistant finalization
- Store failure on tool-result message

No event is published after `session_shutdown`.

## 7. Phase 4: AgentHooks

### Purpose

Replace the wide `LifecycleEffect` dispatcher with real mutating interception points.

### Work

- Add one `AgentHooks` trait with default no-op methods for:
  - input/user-message transformation and handled outcome
  - before-agent-start system prompt and message injection
  - Provider context transformation
  - tool-call argument transformation/blocking
  - tool-result transformation
  - final-message transformation before Store
- Keep passive notifications out of the trait.
- Add endpoint-specific outcome types.
- Execute handlers serially through the composition's `AgentHooks` implementation.
- Invoke ordinary transform handlers against working copies; commit only successful,
  validated output.
- Revalidate tool input after every committed mutation or at the chain boundary before
  execution.
- Enforce final message role and terminal invariants.
- Emit `hook_error` for ordinary handler failures and continue.
- Fail closed for security-sensitive tool-call handler failures.
- Propagate cancellation; do not add timeout configuration.
- Delete `LifecycleEffect` once all core call sites use typed methods.

### Files likely touched

- new `e-agent-core/src/hooks.rs` or replacement for `lifecycle.rs`
- `e-agent-core/src/session/mod.rs`
- `e-agent-core/src/tool/extension.rs`
- hook contract tests

### Gate

- Transform chaining uses previous output.
- Handled/block short-circuits at the documented point.
- Ordinary handler error does not retain partial mutation.
- Security handler error blocks the tool.
- Invalid tool input never reaches ToolExecutor.
- Invalid final message replacement never reaches SessionStore.

## 8. Phase 5: Extract e-agent-pi-compat

### Purpose

Move Pi compatibility out of the PTC tool runtime and map it onto native contracts.

### Workspace changes

Add a workspace member:

```text
e-agent-pi-compat/
  Cargo.toml
  src/
    lib.rs
    host.rs
    hooks.rs
    events.rs
    context.rs
    diagnostics.rs
    capabilities.rs
```

Only create modules that remove real complexity. A smaller initial layout is preferred.

### Dependency rules

- `e-agent-pi-compat -> e-agent-core`
- `e-agent-pi-compat -> e-agent-node-runtime`
- optional use of common extension types where required
- no reverse dependency from core or tool-ptc

### Work

- Move `load_pi_extension()` ownership from `ProgrammaticToolExecutor`.
- Keep the persistent PiJS runtime/actor behavior required by extension closures.
- Store Pi handler registration order.
- Implement native `AgentHooks` by dispatching Pi modifying events with Pi combination rules.
- Subscribe to native EventBus and dispatch Pi notification events.
- Build Pi `ctx` from native SessionHandle/View/status and capability adapters.
- Move Pi commands, host actions, custom event bus, and extension diagnostics into compat.
- Keep Pi-registered tool execution interoperable with PTC through an explicit integration
  boundary rather than making tool-ptc own the extension host.
- Implement the architecture compatibility matrix as data/tests or at minimum as one
  authoritative table paired with fixtures.
- Warn once for unsupported registrations and return explicit unavailable errors on calls.
- Never leave unsupported UI/hostcall Promises pending.

### Compatibility priorities

1. Existing supported tools and commands.
2. Input/before-agent/context/tool/final-message modifying hooks.
3. Agent/Turn/Message/Tool/Session notification mapping.
4. Queue host actions and custom Session entries.
5. Explicit diagnostics for unsupported project/session/provider/model/custom-UI features.

### Gate

- Unmodified supported Pi fixtures load.
- Handler order and merge semantics match the matrix.
- Unsupported registration does not reject the entire extension.
- Unsupported invocation fails explicitly.
- `e-agent-tool-ptc` tests still pass without owning Pi lifecycle compatibility.
- `e-agent-core` can compile/test without node-runtime or compat dependencies.

## 9. Phase 6: Diagnostic JSONL Adapter

### Purpose

Replace CLI-owned lifecycle logging with a native EventBus observer.

### Work

- Subscribe before the first Session event.
- Serialize current native events as one JSON object per line.
- Sanitize errors and omit credentials/provider headers.
- Flush each diagnostic record if current behavior requires immediate observation.
- On `Lagged`, write one gap/error record if possible and stop the sink.
- Keep SessionStore JSONL and diagnostic event JSONL as separate files/concepts.

### Gate

- JSONL contains normal and failure event order from the same native stream as TUI.
- Disabling JSONL does not affect Session durability.
- Sink failure does not become Session Store failure.

## 10. Phase 7: Ratatui TUI

### Purpose

Replace line-oriented CLI interaction with a real native event consumer.

### Work

- Add Ratatui and Crossterm dependencies to the UI-owning crate.
- Select and verify a maintained Ratatui Markdown component.
- Implement AppState and a pure reducer for:
  - persisted transcript items
  - active assistant partial
  - active/completed tools
  - queues and running/idle/fatal status
  - editor text/cursor
  - scroll/follow mode
- Initialize AppState from SessionView after obtaining EventReceiver.
- Run three inputs in one async select/event loop:
  - Crossterm terminal events
  - core EventReceiver
  - periodic render/tick only when required
- Implement minimal multiline Markdown source editing.
- Submit through SessionHandle; do not mutate Session directly.
- Render submitted user and assistant messages as Markdown.
- Update the assistant Markdown component for each partial and fully refresh at terminal.
- Preserve partial text and mark it unpersisted on `session_fatal`; disable submission.
- Restore terminal state on normal close, fatal close, Ctrl+C, and panic guard paths.

### First-release controls

At minimum:

- insert/delete/cursor movement
- multiline insertion
- submit
- scroll transcript
- abort active run
- exit/close

Do not implement command palettes, model selectors, custom extension widgets, or preview mode
without a separate requirement.

### Gate

- Reducer and TestBackend tests pass.
- Existing session history appears before new events.
- Long streaming Markdown does not blank or corrupt the terminal.
- Resize and CJK text wrapping are usable.
- Abort and Store fatal clear working indicators.

## 11. Phase 8: Legacy Removal and Composition Cleanup

### Work

- Delete `set_message_handler`, `emit_message`, and direct message printing.
- Delete legacy lifecycle callback/effect paths after all native call sites migrate.
- Remove Pi lifecycle loading from `ProgrammaticToolExecutor`.
- Compose Provider, Store, tool runtime, optional compat, EventBus adapters, and TUI in the CLI.
- Keep print/headless behavior as an adapter, not a special core path.
- Update workspace members and dependency graph.
- Mark the old lifecycle plan superseded; do not delete historical docs unless separately
  requested.

### Gate

- Searches find no active `on_message` or `LifecycleEffect` production path.
- Tool-ptc has no Pi lifecycle host ownership.
- TUI and JSONL receive only native events.
- Workspace checks and real acceptance pass.

## 12. Final Acceptance

Run the companion test plan. Final acceptance is not complete until real `.env` scenarios
pass with the release build:

- normal streaming conversation
- tool loop
- Pi modifying and notification hooks
- abort after partial output
- restore existing session through SessionView
- Markdown user/assistant transcript
- fatal/normal shutdown behavior where safely injectable

Never print or copy `.env`, API keys, tokens, provider headers, or complete sensitive tool
inputs into test output.

## 13. Change Discipline

- Do not refactor `e-agent-node-runtime/src/runtime.rs` broadly as part of this work.
- Do not add a generic middleware framework beyond `AgentHooks`.
- Do not add a public wire-version subsystem for diagnostic JSONL.
- Do not add lag recovery until a real adapter demonstrates the need.
- Do not add Hook timeouts while extensions remain trusted in-process code.
- Do not preserve the old Rust API solely to avoid internal call-site edits.
- Add one focused contract test for each non-trivial lifecycle branch.
